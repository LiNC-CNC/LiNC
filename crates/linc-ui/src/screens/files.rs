//! Files page: removable USB on the left, NC_FILES on the right.

use std::path::{Path, PathBuf};

use egui::{RichText, Ui, Vec2};
use linc_core::model::InterpState;

use crate::files::{self, format_modified, format_size};
use crate::shell::LincApp;
use crate::state::{CopyJob, CopyProgress, FileEntry, MountState};
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.files.poll_mount();
    poll_copy(app);
    // Page-local commands; the bottom hint row lists them.
    ui.input_mut(|input| {
        if input.consume_key(
            app.keymap.files_copy_to_usb.modifiers,
            app.keymap.files_copy_to_usb.key,
        ) && copy_to_usb_allowed(app)
        {
            copy_nc_to_usb(app);
        }
        if input.consume_key(
            app.keymap.files_copy_to_nc.modifiers,
            app.keymap.files_copy_to_nc.key,
        ) && copy_to_nc_allowed(app)
        {
            copy_usb_to_nc(app);
        }
    });
    // Left/Right move between the panes, mirroring the two columns.
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            app.files.usb_focused = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            app.files.usb_focused = false;
        }
    });
    let available = ui.available_size();
    // Leave one line for the copy/error status.
    let content_height = (available.y - 22.0).max(200.0);
    let pane_width = ((available.x - 8.0) * 0.5).max(300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(pane_width, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| usb_pane(app, ui, content_height - 26.0),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(pane_width, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| nc_pane(app, ui, content_height - 26.0),
        );
    });
    copy_status(app, ui);
}

fn copy_to_usb_allowed(app: &LincApp) -> bool {
    matches!(app.files.mount_state, MountState::Mounted) && app.files.copy.is_none()
}

fn copy_to_nc_allowed(app: &LincApp) -> bool {
    let running = app.snapshot.task.interp != InterpState::Idle || app.snapshot.task.running;
    copy_to_usb_allowed(app) && !running && !app.program_protected()
}

/// One line of feedback where the buttons used to be: progress, error, or the
/// note about how a copy is written.
fn copy_status(app: &LincApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if let Some(error) = &app.files.error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
        } else if let Some(job) = &app.files.copy {
            let label = match app.files.copy_progress {
                Some((copied, total)) => format!(
                    "COPYING {}  {} / {}",
                    job.label,
                    format_size(copied),
                    format_size(total)
                ),
                None => format!("COPYING {}  STARTING", job.label),
            };
            ui.label(
                RichText::new(label)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::ACCENT),
            );
        } else {
            ui.label(
                RichText::new("COPY USES TEMP FILE + ATOMIC RENAME")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        }
    });
}

fn usb_pane(app: &mut LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height(height);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("USB MEDIA")
                    .size(theme::font_size(16.0))
                    .strong()
                    .color(theme::FG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let eject_enabled = matches!(app.files.mount_state, MountState::Mounted)
                    && app.files.copy.is_none();
                if theme::action_button(ui, "EJECT", eject_enabled).clicked() {
                    eject(app);
                }
                if theme::action_button(ui, "REFRESH", true).clicked() {
                    app.files.refresh();
                }
            });
        });
        match &app.files.mount_state {
            MountState::NoUsb => {
                ui.label(
                    RichText::new("NO USB")
                        .size(theme::font_size(18.0))
                        .strong()
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(
                        "Auto-mount on insert via udisks2; mounts are nosuid,nodev,noexec.",
                    )
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
                );
            }
            MountState::Mounting => {
                ui.label(
                    RichText::new("MOUNTING…")
                        .size(theme::font_size(18.0))
                        .strong()
                        .color(theme::WARNING),
                );
            }
            MountState::Error(error) => {
                ui.label(
                    RichText::new("USB ERROR")
                        .size(theme::font_size(18.0))
                        .strong()
                        .color(theme::ERROR),
                );
                ui.label(
                    RichText::new(error)
                        .size(theme::font_size(10.0))
                        .color(theme::ERROR),
                );
            }
            MountState::Mounted => {
                ui.label(
                    RichText::new(format!(
                        "{} / {} FREE",
                        app.files.usb_label,
                        app.files
                            .usb_free
                            .map(format_size)
                            .unwrap_or_else(|| "—".into())
                    ))
                    .monospace()
                    .size(theme::font_size(11.0))
                    .color(theme::FG),
                );
                ui.label(
                    RichText::new("MOUNTED BY UDISKS2 / USER SESSION")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                file_list(app, ui, Pane::Usb);
            }
        }
        // The banner belongs to the *unmounted* state: it is shown after a
        // successful eject, not while the device is still mounted.
        if app.files.safe_to_remove {
            theme::banner(ui, theme::SUCCESS, "SAFE TO REMOVE");
        }
    });
}

fn nc_pane(app: &mut LincApp, ui: &mut Ui, height: f32) {
    theme::card(ui, |ui| {
        ui.set_min_height(height);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("NC_FILES")
                    .size(theme::font_size(16.0))
                    .strong()
                    .color(theme::FG),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(app.files.nc_current.display().to_string())
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        file_list(app, ui, Pane::Nc);
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Usb,
    Nc,
}

fn file_list(app: &mut LincApp, ui: &mut Ui, pane: Pane) {
    let entries: Vec<FileEntry> = match pane {
        Pane::Usb => app.files.usb_entries.clone(),
        Pane::Nc => app.files.nc_entries.clone(),
    };
    let current = match pane {
        Pane::Usb => app.files.usb_current.clone(),
        Pane::Nc => Some(app.files.nc_current.clone()),
    };
    let root = match pane {
        Pane::Usb => app.files.usb_root.clone(),
        Pane::Nc => Some(app.files.nc_root.clone()),
    };
    let mut selected = match pane {
        Pane::Usb => app.files.usb_selected,
        Pane::Nc => app.files.nc_selected,
    };
    let mut open_dir: Option<PathBuf> = None;
    let mut load_file: Option<PathBuf> = None;
    let mut copy_file: Option<PathBuf> = None;
    let mut delete_file: Option<PathBuf> = None;
    let mut up = false;
    let focused = (pane == Pane::Usb) == app.files.usb_focused;
    // Keyboard only drives the focused pane.
    if focused {
        if ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            selected = (selected + 1).min(entries.len().saturating_sub(1));
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            selected = selected.saturating_sub(1);
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(entry) = entries.get(selected) {
                if entry.is_dir {
                    open_dir = Some(entry.path.clone());
                } else if pane == Pane::Nc {
                    // Enter on a program opens it; `load_selected` returns to
                    // the Program page once the backend confirms the load.
                    load_file = Some(entry.path.clone());
                } else {
                    // Enter on a USB file copies it into NC_FILES, which is
                    // what the pane is for.
                    copy_file = Some(entry.path.clone());
                }
            }
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Backspace)) {
            up = true;
        }
        // Delete is configurable; the CNC keyboard has no Delete key, so Ctrl+X
        // is accepted as the same command.
        let delete_pressed = ui.ctx().input(|i| {
            i.key_pressed(egui::Key::Delete)
                || (i.modifiers.command && i.key_pressed(egui::Key::X))
        });
        if delete_pressed && app.files.copy.is_none() {
            if let Some(entry) = entries.get(selected) {
                if !entry.is_dir {
                    delete_file = Some(entry.path.clone());
                }
            }
        }
    }
    // Scrollable list.
    egui::ScrollArea::vertical()
        .id_salt(("file-list", pane as u8))
        .max_height(ui.available_height().max(120.0))
        .show(ui, |ui| {
            egui::Grid::new(format!("file-grid-{:?}", pane as u8))
                .num_columns(3)
                .striped(true)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("NAME")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("SIZE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("MODIFIED")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.end_row();
                    let can_up = current
                        .as_deref()
                        .zip(root.as_deref())
                        .map(|(current, root)| current != root)
                        .unwrap_or(false);
                    if can_up {
                        if ui.selectable_label(false, "..").clicked() {
                            app.files.usb_focused = pane == Pane::Usb;
                            up = true;
                        }
                        ui.label("");
                        ui.label("");
                        ui.end_row();
                    }
                    for (index, entry) in entries.iter().enumerate() {
                        let label = if entry.is_dir {
                            format!("{}/", entry.name)
                        } else {
                            entry.name.clone()
                        };
                        let response = ui.selectable_label(
                            selected == index,
                            RichText::new(label)
                                .monospace()
                                .size(theme::font_size(11.0))
                                .color(if entry.is_symlink {
                                    theme::WARNING
                                } else if entry.is_dir {
                                    theme::ACCENT
                                } else {
                                    theme::FG
                                }),
                        );
                        if response.clicked() {
                            selected = index;
                            app.files.usb_focused = pane == Pane::Usb;
                        }
                        if response.double_clicked() {
                            if entry.is_dir {
                                open_dir = Some(entry.path.clone());
                            } else if pane == Pane::Nc {
                                load_file = Some(entry.path.clone());
                            }
                        }
                        ui.label(
                            RichText::new(if entry.is_dir {
                                "DIR".to_string()
                            } else {
                                format_size(entry.size)
                            })
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                        );
                        ui.label(
                            RichText::new(format_modified(entry.modified))
                                .monospace()
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                        ui.end_row();
                    }
                });
        });
    if up {
        if let (Some(current), Some(root)) = (current.as_deref(), root.as_deref()) {
            if current != root {
                if let Some(parent) = current.parent() {
                    if parent.starts_with(root) {
                        open_dir = Some(parent.to_path_buf());
                    }
                }
            }
        }
    }
    if let Some(dir) = open_dir {
        if pane == Pane::Usb {
            app.files.usb_current = Some(dir.clone());
            app.files.usb_entries = crate::state::read_dir_entries(&dir);
            app.files.usb_selected = 0;
        } else {
            app.files.nc_current = dir.clone();
            app.files.nc_entries = crate::state::read_dir_entries(&dir);
            app.files.nc_selected = 0;
        }
    }
    match pane {
        Pane::Usb => app.files.usb_selected = selected,
        Pane::Nc => app.files.nc_selected = selected,
    }
    if let Some(path) = load_file {
        if pane == Pane::Nc {
            load_selected(app, &path);
        }
    }
    if copy_file.is_some() && pane == Pane::Usb {
        copy_usb_to_nc(app);
    }
    if let Some(path) = delete_file {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        app.modal = Some(crate::state::Modal::ConfirmDelete {
            path,
            label,
            usb: pane == Pane::Usb,
        });
    }
}


fn load_selected(app: &mut LincApp, path: &Path) {
    let extension = path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();
    if !app
        .profile
        .config
        .files
        .load_extensions
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&extension))
    {
        app.toast(
            format!("not a configured G-code extension: {extension}"),
            theme::ERROR,
        );
        return;
    }
    if !path.starts_with(app.profile.nc_files_root()) {
        app.toast("file is outside NC_FILES", theme::ERROR);
        return;
    }
    if app.snapshot.task.interp != InterpState::Idle || app.snapshot.task.running {
        app.toast("cannot load while running", theme::ERROR);
        return;
    }
    app.request_load(path.to_path_buf());
}

fn copy_nc_to_usb(app: &mut LincApp) {
    let Some(entry) = app.files.nc_entries.get(app.files.nc_selected).cloned() else {
        app.toast("select an NC_FILES file", theme::WARNING);
        return;
    };
    if entry.is_dir {
        app.toast("folder copy is out of scope; select a file", theme::WARNING);
        return;
    }
    let Some(usb_root) = app.files.usb_root.clone() else {
        app.toast("no USB media mounted", theme::ERROR);
        return;
    };
    if entry.is_symlink {
        app.toast("symlink rejected", theme::ERROR);
        return;
    }
    let destination = usb_root.join(&entry.name);
    if destination.exists() {
        app.files.pending_overwrite = Some((entry.path, usb_root, destination.clone()));
        app.modal = Some(crate::state::Modal::ConfirmOverwrite(entry.name));
        return;
    }
    start_copy_job(app, entry.path, usb_root, destination, false);
}

fn copy_usb_to_nc(app: &mut LincApp) {
    let Some(entry) = app.files.usb_entries.get(app.files.usb_selected).cloned() else {
        app.toast("select a USB file", theme::WARNING);
        return;
    };
    if entry.is_dir {
        app.toast("folder copy is out of scope; select a file", theme::WARNING);
        return;
    }
    if entry.is_symlink {
        app.toast("symlink rejected", theme::ERROR);
        return;
    }
    let destination_root = app.files.nc_current.clone();
    if !destination_root.starts_with(app.profile.nc_files_root()) {
        app.toast(
            "NC_FILES destination is outside PROGRAM_PREFIX",
            theme::ERROR,
        );
        return;
    }
    let destination = destination_root.join(&entry.name);
    if destination.exists() {
        app.files.pending_overwrite = Some((entry.path, destination_root, destination.clone()));
        app.modal = Some(crate::state::Modal::ConfirmOverwrite(entry.name));
        return;
    }
    start_copy_job(app, entry.path, destination_root, destination, false);
}

fn start_copy_job(
    app: &mut LincApp,
    source: PathBuf,
    destination_root: PathBuf,
    destination: PathBuf,
    overwrite: bool,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let label = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    files::start_copy(source, destination_root, destination, overwrite, tx);
    app.files.copy = Some(CopyJob {
        rx,
        label,
        started: std::time::Instant::now(),
    });
    app.files.error = None;
}

pub fn finish_overwrite(app: &mut LincApp, overwrite: bool) {
    let Some((source, root, destination)) = app.files.pending_overwrite.take() else {
        return;
    };
    if overwrite {
        start_copy_job(app, source, root, destination, true);
    }
}

fn poll_copy(app: &mut LincApp) {
    if app.files.copy.is_none() {
        app.files.copy_progress = None;
    }
    let (messages, timed_out) = {
        let Some(job) = &app.files.copy else {
            return;
        };
        let mut messages = Vec::new();
        while let Ok(message) = job.rx.try_recv() {
            messages.push(message);
        }
        (messages, job.started.elapsed().as_secs() > 300)
    };
    for message in messages {
        match message {
            CopyProgress::Progress { copied, total } => {
                // Latch it: the channel is drained here, so the actions row can
                // no longer read the progress itself.
                app.files.copy_progress = Some((copied, total));
            }
            CopyProgress::Done => {
                app.files.copy = None;
                app.files.refresh();
                app.toast("copy complete", theme::SUCCESS);
                return;
            }
            CopyProgress::Error(error) => {
                app.files.copy = None;
                app.files.error = Some(error.clone());
                app.toast(format!("copy failed: {error}"), theme::ERROR);
                return;
            }
        }
    }
    if timed_out {
        app.files.copy = None;
        app.toast("copy timed out", theme::ERROR);
    }
}

fn eject(app: &mut LincApp) {
    if app.files.copy.is_some() {
        app.toast("cannot eject during a copy", theme::WARNING);
        return;
    }
    let Some(root) = app.files.usb_root.clone() else {
        return;
    };
    let udisks_result = crate::udisks2::eject_mount(&root.to_string_lossy());
    let libc_result = if udisks_result.is_err() {
        let Ok(c_path) = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()) else {
            app.toast("invalid mount path", theme::ERROR);
            return;
        };
        unsafe { libc::umount(c_path.as_ptr()) }
    } else {
        0
    };
    if udisks_result.is_ok() || libc_result == 0 {
        app.files.usb_root = None;
        // stays true until the next successful mount clears it
        app.files.usb_current = None;
        app.files.usb_entries.clear();
        app.files.mount_state = MountState::NoUsb;
        app.files.safe_to_remove = true;
        app.toast("unmounted — SAFE TO REMOVE", theme::SUCCESS);
    } else {
        let error = std::io::Error::last_os_error();
        let detail = udisks_result.err().unwrap_or_else(|| error.to_string());
        app.toast(
            format!("eject failed: {detail}. Close files and retry."),
            theme::ERROR,
        );
    }
}
