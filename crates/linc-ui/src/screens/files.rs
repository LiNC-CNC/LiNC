//! Files page: removable USB on the left, NC_FILES on the right.

use std::path::{Path, PathBuf};

use egui::{RichText, Ui, Vec2};
use egui_extras::{Column as ExtraColumn, TableBuilder};
use linc_core::model::InterpState;

use crate::files::{self, format_modified, format_size};
use crate::screens::common;
use crate::shell::LincApp;
use crate::state::{CopyJob, CopyProgress, FileEntry, MountState};
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.files.poll_mount();
    poll_copy(app);
    // Take keyboard focus off whatever had it: the Program editor is a text
    // widget and consumes Enter and the arrows, so the file lists never saw
    // the keys.
    common::focus_anchor(ui, "files-page-focus");
    // Left/Right move between the panes, mirroring the two columns. A rename
    // field owns the arrows while it is open.
    if app.files.rename.is_none() && !app.overlay_open() {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                app.files.usb_focused = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                app.files.usb_focused = false;
            }
        });
    }
    handle_keys(app, ui);
    let available = ui.available_size();
    // Leave one line for the copy/error status.
    let content_height = (available.y - 22.0).max(200.0);
    let pane_width = ((available.x - 8.0) * 0.5).max(300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(pane_width, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_width(pane_width);
                usb_pane(app, ui, content_height - 26.0);
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(pane_width, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_width(pane_width);
                nc_pane(app, ui, content_height - 26.0);
            },
        );
    });
    copy_status(app, ui);
}

/// Page keys, consumed before the lists draw.
///
/// Handling them inside the list let a focused row widget take Enter and the
/// arrows for itself, so opening a program from NC_FILES did nothing.
fn handle_keys(app: &mut LincApp, ui: &mut Ui) {
    // The rename field owns Enter/Escape; typing is left to the TextEdit.
    if app.files.rename.is_some() {
        let mut commit = false;
        let mut cancel = false;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                commit = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                cancel = true;
            }
        });
        if commit {
            commit_rename(app);
        } else if cancel {
            app.files.rename = None;
        }
        return;
    }
    if app.overlay_open() {
        return;
    }
    let usb = app.files.usb_focused;
    let entries: Vec<FileEntry> = if usb {
        app.files.usb_entries.clone()
    } else {
        app.files.nc_entries.clone()
    };
    let current = if usb {
        app.files.usb_current.clone()
    } else {
        Some(app.files.nc_current.clone())
    };
    let root = if usb {
        app.files.usb_root.clone()
    } else {
        Some(app.files.nc_root.clone())
    };
    let mut selected = if usb {
        app.files.usb_selected
    } else {
        app.files.nc_selected
    };
    // `..` is reachable when inside the tree, and in NC_FILES at its root when
    // `show_parent_above_root` is enabled.
    let parent_available = current
        .as_deref()
        .zip(root.as_deref())
        .map(|(current, root)| {
            current != root
                || (!usb
                    && app.files.allow_parent_above_root
                    && current.parent().is_some())
        })
        .unwrap_or(false);
    let mut on_parent = parent_available
        && if usb {
            app.files.usb_parent_selected
        } else {
            app.files.nc_parent_selected
        };
    let mut open_dir: Option<PathBuf> = None;
    let mut load: Option<PathBuf> = None;
    let mut copy_enter = false;
    let mut copy_side = false;
    let mut rename = false;
    let mut new_file = false;
    let mut eject_media = false;
    let mut delete: Option<PathBuf> = None;
    let mut scroll_selection = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::C) {
            copy_side = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::R) {
            rename = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::N) {
            new_file = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::E) {
            eject_media = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            if on_parent {
                if !entries.is_empty() {
                    on_parent = false;
                    selected = 0;
                }
            } else {
                selected = (selected + 1).min(entries.len().saturating_sub(1));
            }
            scroll_selection = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            if !on_parent {
                if selected == 0 && parent_available {
                    on_parent = true;
                } else {
                    selected = selected.saturating_sub(1);
                }
            }
            scroll_selection = true;
        }
        let page = app.files.page_rows.max(1);
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
            if on_parent {
                if !entries.is_empty() {
                    on_parent = false;
                    selected = page.saturating_sub(1).min(entries.len() - 1);
                }
            } else if !entries.is_empty() {
                selected = (selected + page).min(entries.len() - 1);
            }
            scroll_selection = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
            if !on_parent {
                if selected >= page {
                    selected -= page;
                } else {
                    selected = 0;
                    if parent_available {
                        on_parent = true;
                    }
                }
            }
            scroll_selection = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            if on_parent {
                if let Some(parent) = current.as_deref().and_then(Path::parent) {
                    open_dir = Some(parent.to_path_buf());
                }
            } else if let Some(entry) = entries.get(selected) {
                if entry.is_dir {
                    open_dir = Some(entry.path.clone());
                } else if usb {
                    // Enter on a USB file copies it into NC_FILES.
                    copy_enter = true;
                } else {
                    load = Some(entry.path.clone());
                }
            }
        }
        let delete_pressed = input.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
            || input.consume_key(egui::Modifiers::CTRL, egui::Key::X);
        if delete_pressed && !on_parent {
            if let Some(entry) = entries.get(selected) {
                if !entry.is_dir {
                    delete = Some(entry.path.clone());
                }
            }
        }
    });
    if usb {
        app.files.usb_selected = selected;
        app.files.usb_parent_selected = on_parent;
    } else {
        app.files.nc_selected = selected;
        app.files.nc_parent_selected = on_parent;
    }
    if scroll_selection {
        app.files.scroll_to_selection = true;
    }
    if let Some(dir) = open_dir {
        if usb {
            app.files.usb_current = Some(dir.clone());
            app.files.usb_entries = crate::state::read_dir_entries(&dir);
            app.files.usb_selected = 0;
            app.files.usb_parent_selected = false;
        } else {
            app.files.nc_current = dir.clone();
            app.files.nc_entries = crate::state::read_dir_entries(&dir);
            app.files.nc_selected = 0;
            app.files.nc_parent_selected = false;
        }
    }
    if let Some(path) = load {
        load_selected(app, &path);
    }
    if copy_enter && copy_to_nc_allowed(app) {
        copy_usb_to_nc(app);
    }
    if copy_side && !on_parent {
        copy_selected(app);
    }
    if rename && !on_parent {
        begin_rename(app);
    }
    if new_file {
        new_program(app);
    }
    if eject_media {
        eject(app);
    }
    if let Some(path) = delete {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        app.modal = Some(crate::state::Modal::ConfirmDelete { path, label, usb });
    }
}

/// `C`: copy the focused pane's selection to the other side.
fn copy_selected(app: &mut LincApp) {
    let on_parent = if app.files.usb_focused {
        app.files.usb_parent_selected
    } else {
        app.files.nc_parent_selected
    };
    if on_parent {
        app.toast("select a file, not the parent row", theme::WARNING);
        return;
    }
    if app.files.usb_focused {
        if copy_to_nc_allowed(app) {
            copy_usb_to_nc(app);
        } else {
            app.toast("copy to NC_FILES is blocked", theme::WARNING);
        }
    } else if copy_to_usb_allowed(app) {
        copy_nc_to_usb(app);
    } else {
        app.toast("copy to USB is blocked", theme::WARNING);
    }
}

/// Open the NAME cell of `path` as a text field. The stem is selected so typing
/// replaces the name and leaves the extension.
fn start_rename(
    app: &mut LincApp,
    usb: bool,
    path: PathBuf,
    name: String,
    is_dir: bool,
) {
    let select_len = if is_dir {
        name.chars().count()
    } else {
        Path::new(&name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().chars().count())
            .unwrap_or_else(|| name.chars().count())
    };
    app.files.rename = Some(crate::state::FileRename {
        usb,
        path,
        buffer: name,
        select_len,
        focus: true,
    });
}

fn begin_rename(app: &mut LincApp) {
    let usb = app.files.usb_focused;
    let on_parent = if usb {
        app.files.usb_parent_selected
    } else {
        app.files.nc_parent_selected
    };
    if on_parent {
        app.toast("select a file, not the parent row", theme::WARNING);
        return;
    }
    let entry = if usb {
        app.files.usb_entries.get(app.files.usb_selected).cloned()
    } else {
        app.files.nc_entries.get(app.files.nc_selected).cloned()
    };
    let Some(entry) = entry else {
        app.toast("select a file to rename", theme::WARNING);
        return;
    };
    start_rename(app, usb, entry.path, entry.name, entry.is_dir);
}

fn commit_rename(app: &mut LincApp) {
    let Some(rename) = app.files.rename.take() else {
        return;
    };
    let name = rename.buffer.trim().to_string();
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if invalid {
        app.files.error = Some("invalid file name".into());
        app.files.rename = Some(crate::state::FileRename { focus: true, ..rename });
        return;
    }
    let Some(parent) = rename.path.parent().map(Path::to_path_buf) else {
        app.files.error = Some("cannot rename without a parent directory".into());
        return;
    };
    let destination = parent.join(&name);
    if destination != rename.path {
        if destination.exists() {
            app.files.error = Some(format!("{name} already exists"));
            app.files.rename = Some(crate::state::FileRename { focus: true, ..rename });
            return;
        }
        if let Err(error) = std::fs::rename(&rename.path, &destination) {
            app.files.error = Some(format!("rename failed: {error}"));
            app.files.rename = Some(crate::state::FileRename { focus: true, ..rename });
            return;
        }
    }
    let entries = crate::state::read_dir_entries(&parent);
    let index = entries
        .iter()
        .position(|entry| entry.path == destination)
        .unwrap_or(0);
    if rename.usb {
        app.files.usb_entries = entries;
        app.files.usb_current = Some(parent);
        app.files.usb_selected = index;
    } else {
        app.files.nc_entries = entries;
        app.files.nc_current = parent;
        app.files.nc_selected = index;
    }
    app.files.error = None;
    app.toast(format!("renamed to {name}"), theme::ACCENT);
}

/// A new G-code file: header comment, modal setup with the machine's units and
/// the active work system, blank space for the body, and a program end. It
/// deliberately contains no motion, tool call or spindle start.
fn program_skeleton(units: &str, wcs: &str) -> String {
    format!(
        "(LiNC new program)\n\
         {units} G90 {wcs} G17 G40 G49 G80\n\
         G94\n\
         \n\
         (program body)\n\
         \n\
         M5\n\
         M30\n"
    )
}

fn unique_name(directory: &Path, stem: &str, extension: &str) -> String {
    let mut index = 1;
    loop {
        let name = if index == 1 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{index}.{extension}")
        };
        if !directory.join(&name).exists() {
            return name;
        }
        index += 1;
    }
}

/// `N`: create a skeleton program in the current NC_FILES directory and put its
/// stem in rename mode so the operator can name it immediately.
fn new_program(app: &mut LincApp) {
    if app.program_protected() {
        app.toast("PROTECT ON — new files are locked", theme::WARNING);
        return;
    }
    let directory = app.files.nc_current.clone();
    if !directory.starts_with(&app.files.nc_root) {
        app.files.error = Some("NC_FILES directory is outside the root".into());
        return;
    }
    let name = unique_name(&directory, "untitled", "ngc");
    let path = directory.join(&name);
    let units = if app
        .profile
        .machine_ini
        .linear_units
        .to_ascii_lowercase()
        .starts_with("in")
    {
        "G20"
    } else {
        "G21"
    };
    let wcs = linc_core::model::wcs_name(app.snapshot.offsets.active_index.unwrap_or(1));
    if let Err(error) = std::fs::write(&path, program_skeleton(units, &wcs)) {
        app.files.error = Some(format!("could not create {name}: {error}"));
        return;
    }
    app.files.nc_entries = crate::state::read_dir_entries(&directory);
    app.files.nc_selected = app
        .files
        .nc_entries
        .iter()
        .position(|entry| entry.path == path)
        .unwrap_or(0);
    app.files.usb_focused = false;
    app.files.nc_parent_selected = false;
    app.files.error = None;
    app.toast(format!("created {name}"), theme::ACCENT);
    start_rename(app, false, path, name, false);
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
        ui.set_min_width(ui.available_width());
        ui.set_min_height(height);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("USB MEDIA")
                    .size(theme::font_size(16.0))
                    .strong()
                    .color(if app.files.usb_focused {
                        theme::ACCENT
                    } else {
                        theme::FG
                    }),
            );
        });
        match &app.files.mount_state {
            MountState::NoUsb => {
                ui.label(
                    RichText::new("NO USB")
                        .size(theme::font_size(18.0))
                        .strong()
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
        ui.set_min_width(ui.available_width());
        ui.set_min_height(height);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("NC_FILES")
                    .size(theme::font_size(16.0))
                    .strong()
                    .color(if app.files.usb_focused {
                        theme::FG
                    } else {
                        theme::ACCENT
                    }),
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
    selected = selected.min(entries.len().saturating_sub(1));
    let can_up = current
        .as_deref()
        .zip(root.as_deref())
        .map(|(current, root)| {
            current != root
                || (pane == Pane::Nc
                    && app.files.allow_parent_above_root
                    && current.parent().is_some())
        })
        .unwrap_or(false);
    let parent_rows = usize::from(can_up);
    let parent_selected = can_up
        && if pane == Pane::Usb {
            app.files.usb_parent_selected
        } else {
            app.files.nc_parent_selected
        };
    let mut open_dir: Option<PathBuf> = None;
    let mut load_file: Option<PathBuf> = None;
    let mut select_parent = false;
    let mut open_parent = false;

    // Same shape as the tool table: a header, a striped row per entry, the
    // selected row's band and rim bar, and half a text line between rows.
    let size_width = theme::measure(ui, "0000.0 MB", 9.0) + 16.0;
    let modified_width = theme::measure(ui, "0000-00-00 00:00", 9.0) + 16.0;
    let row_font = egui::FontId::monospace(theme::font_size(11.0));
    let line_height = ui
        .painter()
        .layout_no_wrap("0".to_string(), row_font, theme::FG)
        .size()
        .y;
    let row_height = line_height + 2.0;
    // The rim bar sits in the same lane for every row; the name starts after
    // it (plus air), as in the tool table's T column.
    let name_pad = theme::SELECTION_BAR + 6.0;
    ui.spacing_mut().item_spacing.y = line_height * 0.5;
    ui.spacing_mut().item_spacing.x = 8.0;
    let selected_row = selected;
    let focused = (pane == Pane::Usb) == app.files.usb_focused;
    // The rename field lives in the NAME cell of the entry being renamed.
    let rename = app.files.rename.clone();
    let rename_path = rename
        .as_ref()
        .filter(|rename| rename.usb == (pane == Pane::Usb))
        .map(|rename| rename.path.clone());
    let rename_focus = rename
        .as_ref()
        .filter(|rename| rename.usb == (pane == Pane::Usb))
        .map(|rename| rename.focus)
        .unwrap_or(false);
    let rename_select = rename
        .as_ref()
        .filter(|rename| rename.usb == (pane == Pane::Usb))
        .map(|rename| rename.select_len)
        .unwrap_or(0);
    let mut rename_buffer = rename
        .as_ref()
        .filter(|rename| rename.usb == (pane == Pane::Usb))
        .map(|rename| rename.buffer.clone());
    let rename_focused = std::cell::Cell::new(false);
    let painter = ui.painter().clone();
    let clicked = std::cell::Cell::new(None::<usize>);
    let double = std::cell::Cell::new(None::<usize>);
    let parent_clicked = std::cell::Cell::new(false);
    let parent_double = std::cell::Cell::new(false);
    let first_rect = std::cell::Cell::new(None::<egui::Rect>);
    let last_rect = std::cell::Cell::new(None::<egui::Rect>);

    let list_height = ui.available_height().max(120.0);
    let row_pitch = row_height + line_height * 0.5;
    app.files.page_rows = (((list_height - 18.0) / row_pitch).floor() as usize).max(1);
    let scroll_row = if app.files.scroll_to_selection
        && ((pane == Pane::Usb) == app.files.usb_focused)
    {
        app.files.scroll_to_selection = false;
        let row = if parent_selected {
            0
        } else {
            parent_rows + selected.min(entries.len().saturating_sub(1))
        };
        Some(row)
    } else {
        None
    };
    let mut table = TableBuilder::new(ui)
        .id_salt(("file-table", pane as u8))
        .max_scroll_height(list_height)
        .striped(true)
        .resizable(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(ExtraColumn::remainder())
        .column(ExtraColumn::exact(size_width))
        .column(ExtraColumn::exact(modified_width));
    if let Some(row) = scroll_row {
        table = table.scroll_to_row(row, None);
    }
    let table = table.header(18.0, |mut header| {
        for heading in ["NAME", "SIZE", "MODIFIED"] {
            header.col(|ui| {
                ui.label(
                    RichText::new(heading)
                        .size(theme::font_size(9.0))
                        .strong()
                        .color(theme::MUTED),
                );
            });
        }
    });
    table.body(|body| {
        body.rows(row_height, entries.len() + parent_rows, |mut row| {
            let row_index = row.index();
            let entry_index = row_index.checked_sub(parent_rows);
            let entry = entry_index.and_then(|index| entries.get(index));
            first_rect.set(None);
            last_rect.set(None);
            for (position, _) in ["name", "size", "modified"].iter().enumerate() {
                row.col(|ui| {
                    let cell_rect = ui.max_rect();
                    if position == 0 {
                        first_rect.set(Some(cell_rect));
                    }
                    if position == 2 {
                        last_rect.set(Some(cell_rect));
                    }
                    let renaming_here = position == 0
                        && matches!(
                            (rename_path.as_ref(), entry),
                            (Some(path), Some(entry)) if path == &entry.path
                        );
                    if renaming_here {
                        if let Some(buffer) = rename_buffer.as_mut() {
                            ui.add_space(name_pad);
                            let field = ui.add_sized(
                                [
                                    (ui.max_rect().width() - name_pad - 4.0).max(40.0),
                                    (row_height - 2.0).max(12.0),
                                ],
                                egui::TextEdit::singleline(buffer)
                                    .id(egui::Id::unique("linc-file-rename"))
                                    .font(egui::TextStyle::Monospace)
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::symmetric(2, 0)),
                            );
                            if rename_focus {
                                field.request_focus();
                                // The stem is selected, so typing replaces the
                                // name and leaves the extension.
                                let id = egui::Id::unique("linc-file-rename");
                                if let Some(mut state) =
                                    egui::text_edit::TextEditState::load(ui.ctx(), id)
                                {
                                    state.cursor.set_char_range(Some(
                                        egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            egui::text::CCursor::new(rename_select),
                                        ),
                                    ));
                                    state.store(ui.ctx(), id);
                                }
                                rename_focused.set(true);
                            }
                            ui.painter().rect_stroke(
                                cell_rect.shrink2(egui::vec2(1.0, 1.0)),
                                egui::CornerRadius::same(theme::SELECTION_RADIUS as u8),
                                egui::Stroke::new(1.5, theme::ERROR),
                                egui::StrokeKind::Inside,
                            );
                        }
                        return;
                    }
                    let cell = ui.interact(
                        cell_rect,
                        egui::Id::unique(("file-cell", pane as u8, row_index, position)),
                        egui::Sense::click(),
                    );
                    let double_clicked = cell.double_clicked();
                    if cell.clicked() || double_clicked {
                        if let Some(index) = entry_index {
                            if double_clicked {
                                double.set(Some(index));
                            } else {
                                clicked.set(Some(index));
                            }
                        } else if double_clicked {
                            parent_double.set(true);
                        } else {
                            parent_clicked.set(true);
                        }
                    }
                    match (position, entry) {
                        (0, Some(entry)) => {
                            ui.add_space(name_pad);
                            ui.label(
                                RichText::new(if entry.is_dir {
                                    format!("{}/", entry.name)
                                } else {
                                    entry.name.clone()
                                })
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
                        }
                        (0, None) => {
                            ui.add_space(name_pad);
                            ui.label(
                                RichText::new("..")
                                    .monospace()
                                    .size(theme::font_size(11.0))
                                    .color(theme::ACCENT),
                            );
                        }
                        (1, Some(entry)) => {
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
                        }
                        (2, Some(entry)) => {
                            ui.label(
                                RichText::new(format_modified(entry.modified))
                                    .monospace()
                                    .size(theme::font_size(10.0))
                                    .color(theme::MUTED),
                            );
                        }
                        _ => {}
                    }
                });
            }
            // One band across the row, like the tool table: the gaps between
            // cells are filled, and the rim bar is what the eye lands on.
            let band_selected = if row_index == 0 && parent_rows == 1 {
                parent_selected
            } else {
                entry_index == Some(selected_row) && !parent_selected
            };
            if band_selected {
                if let (Some(first), Some(last)) = (first_rect.get(), last_rect.get()) {
                    let band = egui::Rect::from_min_max(
                        egui::pos2(first.left(), first.top()),
                        egui::pos2(last.right(), last.bottom()),
                    );
                    let alpha = if focused {
                        theme::SELECTION_ALPHA
                    } else {
                        theme::SELECTION_ALPHA / 2
                    };
                    painter.rect_filled(
                        band,
                        egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
                        theme::tint(theme::ACCENT, alpha),
                    );
                    if focused {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(first.left(), first.top()),
                                egui::vec2(theme::SELECTION_BAR, band.height()),
                            ),
                            egui::CornerRadius::same(1),
                            theme::ACCENT,
                        );
                    }
                }
            }
        });
    });

    if let Some(buffer) = rename_buffer {
        if let Some(rename) = app.files.rename.as_mut() {
            if rename.usb == (pane == Pane::Usb) {
                rename.buffer = buffer;
                if rename_focused.get() {
                    rename.focus = false;
                }
            }
        }
    }
    if let Some(index) = double.get() {
        if let Some(entry) = entries.get(index) {
            if entry.is_dir {
                open_dir = Some(entry.path.clone());
            } else if pane == Pane::Nc {
                load_file = Some(entry.path.clone());
            }
        }
    } else if let Some(index) = clicked.get() {
        selected = index;
        app.files.usb_focused = pane == Pane::Usb;
        if pane == Pane::Usb {
            app.files.usb_parent_selected = false;
        } else {
            app.files.nc_parent_selected = false;
        }
    }
    if parent_clicked.get() {
        select_parent = true;
        app.files.usb_focused = pane == Pane::Usb;
    }
    if parent_double.get() {
        open_parent = true;
    }
    if !can_up {
        if pane == Pane::Usb {
            app.files.usb_parent_selected = false;
        } else {
            app.files.nc_parent_selected = false;
        }
    }
    if select_parent {
        if pane == Pane::Usb {
            app.files.usb_parent_selected = true;
        } else {
            app.files.nc_parent_selected = true;
        }
    }
    if open_parent {
        if let Some(parent) = current.as_deref().and_then(Path::parent) {
            let allowed = if pane == Pane::Usb {
                root.as_deref()
                    .map(|root| parent.starts_with(root))
                    .unwrap_or(false)
            } else {
                app.files.allow_parent_above_root
                    || root
                        .as_deref()
                        .map(|root| parent.starts_with(root))
                        .unwrap_or(false)
            };
            if allowed {
                open_dir = Some(parent.to_path_buf());
            }
        }
    }
    if let Some(dir) = open_dir {
        if pane == Pane::Usb {
            app.files.usb_current = Some(dir.clone());
            app.files.usb_entries = crate::state::read_dir_entries(&dir);
            app.files.usb_selected = 0;
            app.files.usb_parent_selected = false;
        } else {
            app.files.nc_current = dir.clone();
            app.files.nc_entries = crate::state::read_dir_entries(&dir);
            app.files.nc_selected = 0;
            app.files.nc_parent_selected = false;
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
        app.toast("no USB media mounted", theme::MUTED);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_program_skeleton_has_header_setup_and_end() {
        let text = program_skeleton("G21", "G55");
        assert!(text.starts_with("(LiNC new program)\n"), "{text}");
        assert!(
            text.contains("G21 G90 G55 G17 G40 G49 G80\nG94\n"),
            "{text}"
        );
        assert!(text.contains("\n\n(program body)\n\n"), "{text}");
        assert!(text.trim_end().ends_with("M30"), "{text}");
    }

    #[test]
    fn unique_name_skips_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("untitled.ngc"), "").unwrap();
        std::fs::write(directory.path().join("untitled-2.ngc"), "").unwrap();
        assert_eq!(
            unique_name(directory.path(), "untitled", "ngc"),
            "untitled-3.ngc"
        );
    }
}
