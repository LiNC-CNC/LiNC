//! Page routing and the navigation column: which page the panel mode owns,
//! where a page returns to, and the geometry of a nav row.

use super::*;

pub(super) fn draw_nav_icon(painter: &egui::Painter, rect: egui::Rect, page: Page, color: Color32) {
    let s = egui::Stroke::new(1.5, color);
    let c = rect.center();
    match page {
        Page::Machine => {
            painter.rect_stroke(rect.shrink(2.0), 3.0, s, StrokeKind::Inside);
            painter.circle_stroke(c, 3.0, s);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 2.0, c.y),
                    egui::pos2(rect.right() - 2.0, c.y),
                ],
                s,
            );
        }
        Page::Program => {
            painter.rect_stroke(
                rect.shrink2(Vec2::new(2.0, 1.0)),
                2.0,
                s,
                StrokeKind::Inside,
            );
            for offset in [-3.0, 0.0, 3.0] {
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 5.0, c.y + offset),
                        egui::pos2(rect.right() - 5.0, c.y + offset),
                    ],
                    s,
                );
            }
        }
        Page::Offsets => {
            painter.circle_stroke(c, 7.0, s);
            painter.line_segment(
                [
                    egui::pos2(c.x, rect.top() + 1.0),
                    egui::pos2(c.x, rect.top() + 5.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x, rect.bottom() - 5.0),
                    egui::pos2(c.x, rect.bottom() - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 1.0, c.y),
                    egui::pos2(rect.left() + 5.0, c.y),
                ],
                s,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.right() - 5.0, c.y),
                    egui::pos2(rect.right() - 1.0, c.y),
                ],
                s,
            );
        }
        Page::Tools => {
            let points = [
                egui::pos2(rect.left() + 2.0, c.y),
                egui::pos2(rect.left() + 6.0, c.y),
                egui::pos2(rect.left() + 8.0, rect.top() + 5.0),
                egui::pos2(rect.left() + 11.0, rect.bottom() - 4.0),
                egui::pos2(rect.left() + 14.0, c.y),
                egui::pos2(rect.right() - 2.0, c.y),
            ];
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], s);
            }
        }
        Page::Diagnostics => {
            painter.circle_stroke(c, 7.0, s);
            painter.circle_filled(c, 2.0, color);
        }
        Page::Help => {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(15.0),
                color,
            );
        }
        Page::Files | Page::Probe | Page::Homing | Page::Manual => {
            painter.rect_stroke(rect.shrink(3.0), 2.0, s, StrokeKind::Inside);
        }
    }
}

/// Parse a page name as the diagnostic tools spell it. Accepts the navigation
/// label and the enum-ish spelling, so `G-CODE HELP` and `HELP` both work.
pub fn page_from_name(name: &str) -> Option<Page> {
    let wanted = name
        .trim()
        .to_ascii_uppercase()
        .replace(['-', '_', ' '], " ");
    for page in [
        Page::Machine,
        Page::Program,
        Page::Files,
        Page::Tools,
        Page::Offsets,
        Page::Diagnostics,
        Page::Help,
        Page::Probe,
        Page::Homing,
        Page::Manual,
    ] {
        let label = page.label().to_ascii_uppercase().replace(['-', '_'], " ");
        if label == wanted || format!("{page:?}").to_ascii_uppercase() == wanted {
            return Some(page);
        }
    }
    None
}

/// Navigation entries in display order, with their label and keycap hint.
///
/// Which of these actually appear is `[ui] pages` in the profile, so a machine
/// can hide the pages it does not use (a machine with no changer can hide
/// TOOLS, for example) without removing the screen from the binary.
/// Ordered to match the keyboard: MAIN (F5), FILE (F6), TOOL (F7), OFFSET
/// (F8). A page that has no function key follows the ones that do.
///
/// DIAGNOSTICS and G-CODE HELP are listed too: `configs/base.toml` names all
/// seven, so the operator can reach the diagnostics and help pages with the
/// arrows and Enter as well as with their `[keys]` chords (Ctrl+D, Ctrl+H).
/// They draw as one-line rows without an icon (see [`NAV_PRIMARY`]).
pub const NAV_ITEMS: [(Page, &str, &str); 6] = [
    (Page::Machine, "MACHINE", "MAIN"),
    (Page::Program, "PROGRAM", "FILE"),
    (Page::Tools, "TOOLS", "TOOL"),
    (Page::Offsets, "OFFSETS", "PROBE"),
    (Page::Diagnostics, "DIAG", "Ctrl+D"),
    (Page::Help, "G-CODE", "Ctrl+H"),
];

/// The navigation rows that carry an icon and a two-line label.
///
/// The four entries with a function key do: they are what the operator's hand
/// finds on the panel. DIAGNOSTICS and G-CODE HELP are reached by a chord or by
/// the arrows, so they draw as one-line rows without an icon — a third of the
/// attention, and in a six-row list the vertical space.
pub(super) const NAV_PRIMARY: [Page; 4] =
    [Page::Machine, Page::Program, Page::Tools, Page::Offsets];

/// Geometry of one navigation row: the target the operator gets and the pill
/// painted inside it.
pub struct NavRowGeometry {
    /// Interaction height. Never below [`theme::MIN_TARGET`].
    pub height: f32,
    /// Painted pill. Shorter than `height`: what shrank is the padding, not
    /// the target.
    pub pill: f32,
    /// An icon beside a two-line label.
    pub icon: bool,
}

pub fn nav_row_geometry(page: Page) -> NavRowGeometry {
    if NAV_PRIMARY.contains(&page) {
        NavRowGeometry {
            height: theme::NAV_ROW_HEIGHT,
            pill: theme::NAV_PILL_PRIMARY,
            icon: true,
        }
    } else {
        NavRowGeometry {
            height: theme::NAV_ROW_HEIGHT,
            pill: theme::NAV_PILL_SECONDARY,
            icon: false,
        }
    }
}

/// Navigation entries to show, from `[ui] pages`.
///
/// An empty list means "all of them", so a profile that never mentions the key
/// still gets the full navigation.
pub(super) fn nav_pages(configured: &[String]) -> Vec<Page> {
    if configured.is_empty() {
        return NAV_ITEMS.iter().map(|(page, _, _)| *page).collect();
    }
    configured.iter().map(|key| page_from_config(key)).collect()
}

/// Stable preferences key for a page. Mode-driven pages are never persisted.
pub(super) fn page_key(page: Page) -> &'static str {
    match page {
        Page::Machine => "machine",
        Page::Program => "program",
        Page::Offsets => "offsets",
        Page::Tools => "tools",
        Page::Diagnostics => "diagnostics",
        Page::Help => "help",
        Page::Files => "files",
        Page::Probe => "probe",
        Page::Homing => "homing",
        Page::Manual => "manual",
    }
}

pub(super) fn page_from_config(value: &str) -> Page {
    match value.to_ascii_lowercase().as_str() {
        "program" => Page::Program,
        "offsets" => Page::Offsets,
        "tools" => Page::Tools,
        "diagnostics" => Page::Diagnostics,
        "help" => Page::Help,
        "files" => Page::Files,
        "probe" => Page::Probe,
        _ => Page::Machine,
    }
}

pub fn mode_page(mode: &PanelModeValidity) -> Option<Page> {
    match mode.mode()? {
        // MDI has no page of its own: it opens MAIN, where the mode swaps
        // the program card for the MDI pane.
        PanelMode::Mdi => Some(Page::Machine),
        PanelMode::Home | PanelMode::ProgHome => Some(Page::Homing),
        PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg => Some(Page::Manual),
        PanelMode::Edit => Some(Page::Program),
        PanelMode::Auto => None,
    }
}

impl LincApp {
    /// The screen the panel's current mode owns.
    ///
    /// `MAIN` is the operator asking for "the screen for this mode", not for the
    /// AUTO dashboard: in EDIT that screen is the program editor, in
    /// JOG/INC/MPG it is the manual page and in HOME/PROG.HOME it is the homing
    /// page. The shared `mode_page` mapping answers `None` for AUTO (whose
    /// screen is MAIN) and for an unknown or zero mode, which fails closed onto
    /// the dashboard as well.
    pub fn mode_screen(&self) -> Page {
        mode_page(&self.snapshot.panel.mode).unwrap_or(Page::Machine)
    }

    /// `MAIN` (F5): go to the mode's page, without the Program page's
    /// second meaning of "flip SPLIT/CODE" when it is already in front.
    pub fn request_mode_page(&mut self) {
        let page = self.mode_screen();
        if page == self.nav.page {
            return;
        }
        self.request_page(page);
    }

    pub fn set_page(&mut self, page: Page) {
        self.remember_page(page);
        if page == Page::Help && self.nav.page != Page::Help {
            // Captured before the assignment below, which would otherwise make
            // the Help page its own return destination.
            self.nav.help_return_page = if self.nav.page.is_mode_driven() {
                self.nav.last_non_mode_page
            } else {
                self.nav.page
            };
        }
        if page == Page::Diagnostics && self.nav.page != Page::Diagnostics {
            self.nav.diagnostics_return_page = if self.nav.page.is_mode_driven() {
                self.nav.last_non_mode_page
            } else {
                self.nav.page
            };
        }
        if page == Page::Files {
            self.nav.files_return_page = if self.nav.page == Page::Files {
                Page::Program
            } else {
                self.nav.page
            };
            // The program list is what the operator came for. The USB pane
            // keeps the focus only for the visit that put it there.
            self.files.usb_focused = false;
            self.files.refresh();
        }
        if page == Page::Probe {
            self.nav.homing_return_page = Page::Offsets;
            // Every entry path starts with nothing selected: Esc from Files
            // returns to the tab, not to a routine Enter would run. The tab
            // itself is preserved; only F8 selects the first one.
            screens::probe::clear_selection(self);
        }
        if !page.is_mode_driven() {
            self.nav.last_non_mode_page = page;
        }
        self.nav.page = page;
        self.nav.focus_region = FocusRegion::Page;
        self.nav.page_focus_requested = true;
        if page == Page::Tools {
            // The table takes the keyboard as soon as the page opens: nothing
            // else on the page holds focus, and the cursor starts at the first
            // editable cell of the selected row.
            self.tools.editing = false;
            self.tools.edit_buffer.clear();
            if self.tools.selected_column == 0 {
                self.tools.selected_column = 1;
            }
        } else {
            // The tool dialogs belong to the page. Left open across a page
            // change they would keep `overlay_open` true with nothing on
            // screen, and every key would stay with a dialog that is not drawn.
            self.tools.number = None;
            self.tools.position = None;
        }
        if page == Page::Help {
            self.help.ensure_loaded();
        }
        if page == Page::Program && self.program.view == ProgramView::Code && self.editing_allowed()
        {
            self.program.focus_editor = true;
        }
        if page == Page::Machine {
            // Entering MAIN while the panel sits in MDI is also the pane
            // appearing, so it takes the keyboard like a mode entry does. It
            // only asks here: a click elsewhere on the page gives it up again
            // and the flag is one-shot.
            self.mdi.focus = self.snapshot.panel.mode.mode() == Some(PanelMode::Mdi);
        }
    }

    pub fn request_page(&mut self, page: Page) {
        if page == Page::Program && self.nav.page == Page::Program {
            self.cycle_program_view();
            return;
        }
        if self.program.editor.dirty && self.nav.page == Page::Program && page != Page::Program {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::Navigate(page)));
        } else {
            self.set_page(page);
        }
    }

    pub fn request_files(&mut self) {
        if self.program.editor.dirty && self.nav.page == Page::Program {
            self.overlay.modal = Some(Modal::ConfirmDiscard(PendingAction::OpenFiles));
        } else {
            self.open_files();
        }
    }

    pub fn open_files(&mut self) {
        self.nav.files_return_page = if self.nav.page == Page::Files {
            Page::Program
        } else {
            self.nav.page
        };
        self.files.refresh();
        self.set_page(Page::Files);
    }

    /// Remember the last operator-selected, non-mode-driven page.
    pub(super) fn remember_page(&mut self, page: Page) {
        if page.is_mode_driven() {
            return;
        }
        let key = page_key(page);
        if self.prefs.preferences.last_page.as_deref() != Some(key) {
            self.prefs.preferences.last_page = Some(key.to_string());
            self.prefs.dirty = true;
        }
    }

    /// Enter a page that is coming back after its mode went away. The
    /// destination policy is unchanged (`last_non_mode_page` still decides),
    /// but a restored Probe page is an entry like any other: nothing may come
    /// back selected for Enter to run.
    pub(super) fn restore_page(&mut self, page: Page) {
        if page == Page::Probe {
            screens::probe::clear_selection(self);
        }
        self.nav.page = page;
    }

    pub(super) fn on_mode_change(&mut self, mode: PanelModeValidity) {
        let Some(new_mode) = mode.mode() else {
            if self.nav.page.is_mode_driven() {
                self.restore_page(self.nav.last_non_mode_page);
            }
            return;
        };
        let prior_mode = self
            .nav
            .last_mode
            .as_ref()
            .and_then(PanelModeValidity::mode);
        // Leaving EDIT is the one moment unsaved work can become unreachable:
        // the controller refuses a save outside EDIT, so AUTO can only ever run
        // the loaded file. Say so here, before the operator presses CYCLE START
        // on a program that will not run.
        let left_edit = prior_mode == Some(PanelMode::Edit) && new_mode != PanelMode::Edit;
        if left_edit && self.program.editor.dirty && self.overlay.modal.is_none() {
            self.overlay.modal = Some(Modal::UnsavedOnLeave);
        }
        if prior_mode.map(PanelMode::is_manual).unwrap_or(false)
            && !new_mode.is_manual()
            && self.nav.page == Page::Manual
        {
            self.restore_page(self.nav.last_non_mode_page);
        }
        if matches!(
            prior_mode,
            Some(PanelMode::Home) | Some(PanelMode::ProgHome)
        ) && !matches!(new_mode, PanelMode::Home | PanelMode::ProgHome)
            && self.nav.page == Page::Homing
        {
            self.restore_page(self.nav.last_non_mode_page);
        }
        match new_mode {
            PanelMode::Mdi => {
                // MDI is not a page: the panel position opens MAIN, whose
                // program card the mode swaps for the MDI pane.
                self.nav.page = Page::Machine;
                if self.profile.config.mdi.auto_focus {
                    self.mdi.focus = true;
                }
            }
            PanelMode::Home | PanelMode::ProgHome => {
                if self.nav.page != Page::Homing {
                    self.nav.homing_return_page = if self.nav.page.is_mode_driven() {
                        self.nav.last_non_mode_page
                    } else {
                        self.nav.page
                    };
                }
                self.nav.page = Page::Homing;
            }
            PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg => {
                if self.nav.page != Page::Manual {
                    self.nav.last_non_mode_page = if self.nav.page.is_mode_driven() {
                        self.nav.last_non_mode_page
                    } else {
                        self.nav.page
                    };
                }
                self.manual.view = if self.profile.variant == MachineVariant::Lathe {
                    linc_view::ViewMode::LatheXz
                } else {
                    self.manual.view
                };
                self.nav.page = Page::Manual;
            }
            PanelMode::Edit => {
                if self.nav.page != Page::Program {
                    self.nav.homing_return_page = self.nav.page;
                }
                self.nav.page = Page::Program;
                self.program.view = ProgramView::Code;
                self.program.focus_editor = self.program_editing_allowed();
            }
            PanelMode::Auto => {
                // Selecting AUTO brings up MAIN from any page: the run is
                // started from the machine page, whose toolpath and program
                // cards carry the active line. Leaving MDI swaps the pane back
                // for the normal read-only program card. This runs on the mode
                // change only, so navigating away afterwards is left alone.
                self.nav.page = Page::Machine;
            }
        }
    }

    pub fn focus_nav(&mut self) {
        self.nav.focus_region = FocusRegion::Nav;
        self.nav.nav_focus = self
            .nav
            .pages
            .iter()
            .position(|page| *page == self.nav.page)
            .unwrap_or(0);
    }

    pub(super) fn draw_nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(
            RichText::new("NAVIGATION")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        ui.add_space(4.0);
        // Copied out: the loop body navigates, which borrows `self` mutably.
        let nav = self.nav.pages.clone();
        // The rows carry their own gap: the list spacing the sidebar inherits
        // is 8 px a row, which put a band of dead space around every entry.
        ui.spacing_mut().item_spacing.y = theme::NAV_ROW_GAP;
        for (index, page) in nav.iter().enumerate() {
            let Some((_, label, keycap)) = NAV_ITEMS.iter().find(|(entry, _, _)| entry == page)
            else {
                continue;
            };
            // TOUCH PROBE is a subpage of OFFSETS; keep the parent marked in
            // the sidebar instead of leaving no entry highlighted.
            let selected =
                *page == self.nav.page || (*page == Page::Offsets && self.nav.page == Page::Probe);
            let focused = self.nav.focus_region == FocusRegion::Nav && self.nav.nav_focus == index;
            let geometry = nav_row_geometry(*page);
            // The target every row keeps, whatever it paints inside it.
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(theme::NAV_WIDTH - 20.0, geometry.height),
                Sense::click(),
            );
            let pill =
                egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), geometry.pill));
            let painter = ui.painter();
            painter.rect_filled(
                pill,
                4,
                if selected {
                    theme::ACCENT
                } else if focused {
                    theme::PANEL_RAISED
                } else {
                    theme::SURFACE
                },
            );
            if focused {
                painter.rect_stroke(
                    pill.expand(1.0),
                    5,
                    Stroke::new(2.0, theme::ACCENT),
                    StrokeKind::Outside,
                );
            }
            let icon_color = if selected {
                Color32::from_rgb(0x0b, 0x17, 0x22)
            } else {
                theme::MUTED
            };
            if geometry.icon {
                draw_nav_icon(
                    painter,
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 8.0, rect.center().y - 9.0),
                        Vec2::splat(18.0),
                    ),
                    *page,
                    icon_color,
                );
            }
            // A row with an icon holds its label and keycap on two lines beside
            // it; a one-line row starts at the pill's own inset, with no icon
            // to clear.
            let label_x = rect.left() + if geometry.icon { 34.0 } else { 12.0 };
            // Only a row with an icon is a two-line row. The secondary rows
            // carry a [keys] hint too, and painting it under the label is what
            // kept DIAGNOSTICS and G-CODE HELP two lines tall.
            let two_line = geometry.icon && !keycap.is_empty();
            let label_y = if two_line {
                rect.center().y - 6.0
            } else {
                rect.center().y
            };
            painter.text(
                egui::pos2(label_x, label_y),
                Align2::LEFT_CENTER,
                *label,
                FontId::proportional(theme::font_size(11.0)),
                if selected {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    theme::FG
                },
            );
            if two_line {
                painter.text(
                    egui::pos2(label_x, rect.center().y + 7.0),
                    Align2::LEFT_CENTER,
                    *keycap,
                    FontId::monospace(theme::font_size(8.5)),
                    if selected {
                        Color32::from_rgb(0x0b, 0x17, 0x22)
                    } else {
                        theme::MUTED
                    },
                );
            } else if !keycap.is_empty() {
                // A one-line row carries its chord at the pill's right edge, so
                // label and hint share the row instead of stacking: the labels
                // are short enough (DIAG, G-CODE) for both to fit.
                painter.text(
                    egui::pos2(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    *keycap,
                    FontId::monospace(theme::font_size(8.5)),
                    if selected {
                        Color32::from_rgb(0x0b, 0x17, 0x22)
                    } else {
                        theme::MUTED
                    },
                );
            }
            if response.clicked() {
                ui.memory_mut(|memory| memory.stop_text_input());
                // A nav click is the operator leaving whichever text field had
                // the keyboard. Without this the G-code editor kept focus (the
                // panel keyboard has no Tab), so the toolpath's pan chords and
                // the page keys stayed dead after navigating, while the nav row
                // drew its focus border and looked ready for the arrows.
                self.nav.nav_focus = index;
                self.nav.focus_region = FocusRegion::Nav;
                self.request_page(*page);
            }
        }
        // Minimal: the list ends where the rows do, and the SHORTCUTS frame
        // takes the rest of the column.
        ui.spacing_mut().item_spacing.y = theme::NAV_ROW_GAP;
        ui.add_space(theme::NAV_BOTTOM_GAP);
        theme::separator(ui);
        ui.add_space(theme::NAV_BOTTOM_GAP);
        // Page-local keyboard commands live here: the bottom bar had one row and
        // truncated the list, and the sidebar has the height. The list takes the
        // sidebar's remainder, less the frame's own margins and header.
        //
        // The pane gives the list every pixel the sidebar has left and scrolls
        // inside when the list is longer than that; a list that fits keeps the
        // frame at its content height. `available_height` is read here, outside
        // the frame, where the column really bounds it.
        let header = ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    "SHORTCUTS".to_string(),
                    FontId::proportional(theme::font_size(9.0)),
                    theme::MUTED,
                )
                .size()
                .y
        });
        // The frame's 6 px margin and 1 px stroke on each side, the header line
        // above the list, and the air that keeps the frame off the footer.
        const FRAME_CHROME: f32 = 6.0 * 2.0 + 1.0 * 2.0;
        const FOOTER_PAD: f32 = 4.0;
        let list_height = (ui.available_height() - FRAME_CHROME - header - FOOTER_PAD).max(60.0);
        let shortcuts = self.key_hints();
        if !shortcuts.is_empty() {
            Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(4)
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    // The list can be longer than the sidebar: scroll it, and keep
                    // the safety note under it on screen.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new("SHORTCUTS")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("sidebar-shortcuts")
                        .max_height(list_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // Half a text line between entries: the rows are dense enough
                            // to read as a list, with air that does not waste the pane.
                            let hint_line = ui
                                .painter()
                                .layout_no_wrap(
                                    "Ag".to_string(),
                                    egui::FontId::monospace(theme::font_size(9.0)),
                                    theme::MUTED,
                                )
                                .size()
                                .y;
                            for group in shortcuts.iter() {
                                if !group.title.is_empty() {
                                    // Also before the first title: the pane header is not a
                                    // group, and the first one was sitting right under it.
                                    ui.add_space(hint_line * 0.5);
                                    ui.label(
                                        RichText::new(group.title)
                                            .size(theme::font_size(8.0))
                                            .strong()
                                            .color(theme::MUTED),
                                    );
                                    ui.add_space(hint_line * 0.35);
                                }
                                for (index, (key, action)) in group.items.iter().enumerate() {
                                    // One line per command, exactly; `ui.horizontal`
                                    // inherited the 44 px touch target and left large air
                                    // between two 9 pt rows.
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(ui.available_width(), hint_line),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(key.clone())
                                                    .monospace()
                                                    .size(theme::font_size(9.0))
                                                    .color(theme::C3),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(*action)
                                                            .size(theme::font_size(9.0))
                                                            .color(theme::MUTED),
                                                    );
                                                },
                                            );
                                        },
                                    );
                                    if index + 1 < group.items.len() {
                                        ui.add_space(hint_line * 0.5);
                                    }
                                }
                            }
                        });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(super) fn pages_follow_physical_mode() {
        // MDI opens MAIN, whose program card the mode swaps for the pane.
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Mdi)),
            Some(Page::Machine)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Home)),
            Some(Page::Homing)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Valid(PanelMode::Jog)),
            Some(Page::Manual)
        );
        assert_eq!(
            mode_page(&PanelModeValidity::Invalid(vec![
                PanelMode::Mdi,
                PanelMode::Auto
            ])),
            None
        );
    }
}

/// Page routing state: which page is open, where it returns to, and which
/// navigation row holds the ring.
pub(crate) struct NavUi {
    pub page: Page,
    pub last_non_mode_page: Page,
    pub files_return_page: Page,
    pub help_return_page: Page,
    /// Where Esc on the Diagnostics page goes back to: the page the operator
    /// left to look at it, not the program reset.
    pub diagnostics_return_page: Page,
    pub nav_focus: usize,
    pub focus_region: FocusRegion,
    pub last_mode: Option<PanelModeValidity>,
    pub homing_return_page: Page,
    pub page_focus_requested: bool,
    /// Navigation entries the profile shows, in display order.
    pub pages: Vec<Page>,
}
