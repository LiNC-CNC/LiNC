//! The sidebar SHORTCUTS pane.

use super::*;
use crate::state::ShortcutCommand;

/// One line of the SHORTCUTS pane: the key legend on the left, the action name
/// on the right, and what a click on the row runs.
#[derive(Clone, Debug)]
pub(super) struct Shortcut {
    pub(super) key: String,
    pub(super) label: &'static str,
    /// `None` on a legend that names a gesture the mouse cannot perform (PAN,
    /// ZOOM, the arrow keys, a multi-key filter) or a command a page owns.
    pub(super) command: Option<ShortcutCommand>,
}

impl Shortcut {
    /// A row that only prints its key: no mouse target.
    pub(super) fn hint(key: impl Into<String>, label: &'static str) -> Self {
        Self {
            key: key.into(),
            label,
            command: None,
        }
    }

    /// A row whose click runs the same command its key runs.
    pub(super) fn run(
        key: impl Into<String>,
        label: &'static str,
        command: ShortcutCommand,
    ) -> Self {
        Self {
            key: key.into(),
            label,
            command: Some(command),
        }
    }
}

/// One titled group of page-local keyboard commands shown in the sidebar.
/// An empty title means the group has no header (most pages).
pub(super) struct ShortcutGroup {
    pub(super) title: &'static str,
    pub(super) items: Vec<Shortcut>,
}

impl LincApp {
    /// Take the SHORTCUTS row the operator clicked, for the page that owns the
    /// command.
    ///
    /// The sidebar draws before the page, so a click parked here is read by the
    /// page's own key handler in the same frame. A click no page handler took
    /// is dropped at the start of the next `handle_keyboard`, so a row cannot
    /// fire late on a page it was not drawn for.
    pub(crate) fn take_shortcut_click(&mut self) -> Option<ShortcutCommand> {
        self.nav.shortcut_click.take()
    }

    /// `[ui] keyboard = "pc"` asks for legends a normal keyboard can follow:
    /// `F5`..`F8` on the navigation and `F10` for the view toggles, instead of
    /// the CNC panel's printed keycaps (`MAIN`, `FILE`, `TOOL`, `PROBE`, `▢`).
    /// The bindings are the same either way; only the labels change.
    pub(crate) fn pc_keyboard(&self) -> bool {
        self.profile.config.ui.keyboard == "pc"
    }

    /// Keyboard commands available on the current page, grouped and titled for
    /// the sidebar. A row that names a single command is also a button: see
    /// [`Shortcut::command`].
    pub(super) fn key_hints(&self) -> Vec<ShortcutGroup> {
        let k = &self.prefs.keymap;
        // The view toggles sit at the top of the pane so the display commands
        // are the first things in reach: one key flips SPLIT/CODE and its Ctrl
        // chord cycles the toolpath view. A panel prints both as the square
        // glyph; a normal keyboard legend names the F-keys instead.
        let pc = self.pc_keyboard();
        let square = if pc {
            k.program_cycle_view.label()
        } else {
            "\u{25a2}".to_string()
        };
        let view_cycle = if pc {
            k.view_cycle.label()
        } else {
            format!("Ctrl+{square}")
        };
        let file_key = if pc {
            k.program.label()
        } else {
            "FILE".to_string()
        };
        let offset_key = if pc {
            k.offsets.label()
        } else {
            "OFFSET".to_string()
        };
        // The lathe display toggles: the label names what the key does next, so
        // the pane reads as an action rather than as the current state.
        let lathe_hints: Vec<Shortcut> = if self.profile.variant == MachineVariant::Lathe {
            vec![
                Shortcut::run(
                    k.toggle_lathe_mirror.label(),
                    if self.graph_style.lathe_mirror {
                        "UNMIRROR"
                    } else {
                        "MIRROR"
                    },
                    ShortcutCommand::CycleLatheMirror,
                ),
                Shortcut::run(
                    k.toggle_lathe_diameter.label(),
                    if self.reads_x_as_diameter() {
                        "RADIUS"
                    } else {
                        "DIAMETER"
                    },
                    ShortcutCommand::CycleLatheDiameter,
                ),
            ]
        } else {
            Vec::new()
        };
        let mut groups: Vec<ShortcutGroup> = Vec::new();
        if self.program_active() {
            groups.push(ShortcutGroup {
                title: "PROGRAM CONTROL",
                items: vec![Shortcut::run(
                    k.back.label(),
                    "RESET",
                    ShortcutCommand::ResetProgram,
                )],
            });
        }
        let mut hints: Vec<Shortcut> = Vec::new();
        match self.nav.page {
            Page::Program => {
                let mut view_items = vec![
                    Shortcut::run(
                        square.clone(),
                        if self.program.view == ProgramView::Split {
                            "CODE"
                        } else {
                            "SPLIT"
                        },
                        ShortcutCommand::CycleProgramView,
                    ),
                    Shortcut::run(view_cycle.clone(), "VIEW", ShortcutCommand::CycleView),
                    Shortcut::run(k.graph_fit.label(), "FIT", ShortcutCommand::FitGraph),
                    Shortcut::run(
                        k.graph_clear.label(),
                        "CLEAR TRAIL",
                        ShortcutCommand::ClearTrail,
                    ),
                    Shortcut::run(
                        k.toggle_bounds.label(),
                        "BOUNDS",
                        ShortcutCommand::CycleBounds,
                    ),
                    Shortcut::run(
                        k.remaining.label(),
                        "REMAINING",
                        ShortcutCommand::CycleRemaining,
                    ),
                ];
                // An editable editor owns Ctrl+Up/Down for its line move and
                // Ctrl+Left/Right for word moves, like the MDI input, so the
                // toolpath's pan hint would be false while it holds the
                // keyboard.
                if !self.editing_allowed() {
                    view_items.push(Shortcut::hint(k.view_pan_hint(), "PAN"));
                }
                view_items.push(Shortcut::hint(k.view_zoom_hint(), "ZOOM"));
                view_items.extend(lathe_hints.clone());
                // The manual page has the same two faces as MAIN, and the same
                // press walks them: the hint names what the next one brings.
                if self.nav.page == Page::Manual && self.main_alternate_available() {
                    view_items.push(Shortcut::hint(
                        k.machine.label(),
                        if self.main_view == MainView::Coordinates {
                            "DASHBOARD"
                        } else {
                            "COORDS"
                        },
                    ));
                }
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: view_items,
                });
                let mut program_items: Vec<Shortcut> = Vec::new();
                if self.editing_allowed() {
                    program_items.push(Shortcut::hint("Ctrl+↑/↓", "MOVE LINE"));
                    program_items.push(Shortcut::hint("Ctrl+←/→", "WORD"));
                }
                program_items.extend([
                    Shortcut::run(file_key.clone(), "BROWSER", ShortcutCommand::OpenBrowser),
                    Shortcut::run(
                        k.program_run_from_line.label(),
                        "RUN LINE",
                        ShortcutCommand::RunFromLine,
                    ),
                    Shortcut::run(k.goto_line.label(), "GOTO", ShortcutCommand::GoToLine),
                    // No MESSAGES key here: while the editor has the
                    // keyboard a plain letter is text, so the shortcut
                    // advertised a key that types an M into the program.
                    Shortcut::run(k.save.label(), "SAVE", ShortcutCommand::SaveProgram),
                    Shortcut::run(k.undo.label(), "UNDO", ShortcutCommand::UndoEdit),
                    Shortcut::run(k.redo.label(), "REDO", ShortcutCommand::RedoEdit),
                    // Unload closes the controller's program, which is also what
                    // releases the external-link interlock.
                    Shortcut::run("Ctrl+U", "UNLOAD", ShortcutCommand::UnloadProgram),
                    // The fault walk has no buttons in the pane footer: the counter
                    // and the line are there, and these two chords are the only way
                    // to move between the marks. One entry, because the pair is a
                    // single idea.
                    Shortcut::hint(
                        format!("{}/{}", k.fault_prev.label(), k.fault_next.label()),
                        "FAULT",
                    ),
                ]);
                groups.push(ShortcutGroup {
                    title: "PROGRAM",
                    items: program_items,
                });
            }
            Page::Machine => {
                // MAIN shows the same toolpath, so it carries the same view
                // commands, plus the two program actions that do not need the
                // editor in front (run from line and go to line).
                let mut view_items = vec![
                    Shortcut::run(view_cycle.clone(), "VIEW", ShortcutCommand::CycleView),
                    Shortcut::run(k.graph_fit.label(), "FIT", ShortcutCommand::FitGraph),
                    Shortcut::run(
                        k.graph_clear.label(),
                        "CLEAR TRAIL",
                        ShortcutCommand::ClearTrail,
                    ),
                    Shortcut::run(
                        k.toggle_bounds.label(),
                        "BOUNDS",
                        ShortcutCommand::CycleBounds,
                    ),
                    Shortcut::run(
                        k.remaining.label(),
                        "REMAINING",
                        ShortcutCommand::CycleRemaining,
                    ),
                    // The square swaps the feed/servo pair below the graph for
                    // the following-error traces, and names what the next press
                    // brings back.
                    Shortcut::run(
                        square.clone(),
                        if self.following.active {
                            "FEED"
                        } else {
                            "F.ERROR"
                        },
                        ShortcutCommand::ToggleFollowingError,
                    ),
                ];
                // A second press of the key that opened MAIN swaps the whole
                // dashboard in AUTO and MDI. The hint names what the next press
                // brings, like the square's does, and is absent where the key
                // has one meaning — including the manual modes, whose MAIN page
                // is not the mode's screen, so their key opens that instead.
                if self.main_alternate_available() && self.mode_screen() == Page::Machine {
                    view_items.push(Shortcut::hint(
                        k.machine.label(),
                        if self.main_view == MainView::Coordinates {
                            "DASHBOARD"
                        } else {
                            "COORDS"
                        },
                    ));
                }
                // The MDI input owns Ctrl+Up/Down for its buffer-line move and
                // Ctrl+Left/Right for word moves, so the toolpath's pan hint
                // would be false while the pane is up.
                if !self.mdi_pane_visible() {
                    view_items.push(Shortcut::hint(k.view_pan_hint(), "PAN"));
                }
                view_items.push(Shortcut::hint(k.view_zoom_hint(), "ZOOM"));
                view_items.extend(lathe_hints.clone());
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: view_items,
                });
                if self.mdi_pane_visible() {
                    // While the pane is up its keys replace the MAIN ones:
                    // Enter and the arrows act on the buffer, Ctrl+R runs the
                    // selected block instead of a program line.
                    groups.push(ShortcutGroup {
                        title: "MDI",
                        items: vec![
                            Shortcut::hint(k.mdi_issue.label(), "ISSUE"),
                            Shortcut::hint("↑/↓", "BUFFER"),
                            Shortcut::hint("Ctrl+↑/↓", "MOVE"),
                            Shortcut::hint("Ctrl+←/→", "WORD"),
                            Shortcut::run(k.goto_line.label(), "GOTO", ShortcutCommand::GoToLine),
                            Shortcut::run(
                                k.program_run_from_line.label(),
                                "RUN LINE",
                                ShortcutCommand::RunFromLine,
                            ),
                            Shortcut::hint(k.mdi_buffer_delete.label(), "DELETE"),
                            Shortcut::run(k.save.label(), "SAVE", ShortcutCommand::SaveMdi),
                            Shortcut::run(k.back.label(), "CLEAR", ShortcutCommand::ClearMdiInput),
                        ],
                    });
                } else {
                    groups.push(ShortcutGroup {
                        title: "PROGRAM",
                        items: vec![
                            Shortcut::run(
                                file_key.clone(),
                                "BROWSER",
                                ShortcutCommand::OpenBrowser,
                            ),
                            Shortcut::run(
                                k.program_run_from_line.label(),
                                "RUN LINE",
                                ShortcutCommand::RunFromLine,
                            ),
                            Shortcut::run(k.goto_line.label(), "GOTO", ShortcutCommand::GoToLine),
                            Shortcut::run(
                                k.messages.label(),
                                "MESSAGES",
                                ShortcutCommand::OpenMessages,
                            ),
                        ],
                    });
                }
            }
            Page::Files => {
                // The square flips the right pane between the program directory
                // and the programs opened most recently; its label names what
                // the key gives you, like CODE/SPLIT on the Program page.
                let right_pane = if self.files.recent_shown {
                    "NC_FILES"
                } else {
                    "RECENT"
                };
                // Enter is contextual: a folder opens, a USB file copies into
                // NC_FILES, an NC or recent file loads.
                let (entries, selected, on_parent) = if self.files.usb_focused {
                    (
                        &self.files.usb_entries,
                        self.files.usb_selected,
                        self.files.usb_parent_selected,
                    )
                } else {
                    (
                        &self.files.nc_entries,
                        self.files.nc_selected,
                        self.files.nc_parent_selected,
                    )
                };
                let recent_focus = !self.files.usb_focused && self.files.recent_shown;
                let enter = if recent_focus {
                    // A recent row is a file path: there is no folder to open
                    // and nothing to copy into.
                    "LOAD"
                } else if on_parent || entries.get(selected).is_some_and(|entry| entry.is_dir) {
                    "OPEN"
                } else if self.files.usb_focused {
                    "COPY"
                } else {
                    "LOAD"
                };
                groups.push(ShortcutGroup {
                    title: "BROWSE",
                    items: vec![
                        Shortcut::hint("\u{2190}/\u{2192}", "PANE"),
                        Shortcut::run(
                            square.clone(),
                            right_pane,
                            ShortcutCommand::FilesToggleRecent,
                        ),
                        Shortcut::run("Enter", enter, ShortcutCommand::FilesActivate),
                    ],
                });
                // Copy, duplicate, rename and delete act on the focused pane's
                // directory row; the recent list is not one, so only NEW is
                // offered there.
                let file_items: Vec<Shortcut> = if recent_focus {
                    vec![Shortcut::run("N", "NEW", ShortcutCommand::FilesNew)]
                } else {
                    vec![
                        Shortcut::run("C", "COPY", ShortcutCommand::FilesCopy),
                        Shortcut::run("D", "DUPLICATE", ShortcutCommand::FilesDuplicate),
                        Shortcut::run("R", "RENAME", ShortcutCommand::FilesRename),
                        Shortcut::run("N", "NEW", ShortcutCommand::FilesNew),
                        Shortcut::run(
                            format!("Bksp/{}", k.files_delete.label()),
                            "DELETE",
                            ShortcutCommand::FilesDelete,
                        ),
                    ]
                };
                groups.push(ShortcutGroup {
                    title: "FILE",
                    items: file_items,
                });
                groups.push(ShortcutGroup {
                    title: "MEDIA",
                    items: vec![
                        Shortcut::run(k.refresh.label(), "REFRESH", ShortcutCommand::RefreshFiles),
                        Shortcut::run("E", "EJECT", ShortcutCommand::FilesEject),
                    ],
                });
            }
            Page::Help => {
                hints.push(Shortcut::run(
                    format!("{} / {}", k.help_search.label(), k.find.label()),
                    "SEARCH",
                    ShortcutCommand::HelpSearch,
                ));
                hints.push(Shortcut::run("Enter", "READ", ShortcutCommand::HelpRead));
                hints.push(Shortcut::hint("Bksp", "BACK"));
                hints.push(Shortcut::run(
                    "PgUp/PgDn",
                    "PAGE",
                    ShortcutCommand::HelpPage,
                ));
            }
            Page::Diagnostics => match self.diagnostics.tab {
                DiagnosticsTab::Overview => {
                    hints.push(Shortcut::hint("PgUp/PgDn", "TABS"));
                    groups.push(ShortcutGroup {
                        title: "LOG VIEW",
                        items: vec![Shortcut::run("C", "CLEAR", ShortcutCommand::ClearLogView)],
                    });
                }
                DiagnosticsTab::HalNml => {
                    // The page keys scroll only a pane that overflows; on one
                    // that fits they fall through to the tabs.
                    let scrollable = match self.diagnostics.hal_pane {
                        crate::state::HalPane::Pins => self.diagnostics.hal_pins_scrollable,
                        crate::state::HalPane::Signals => self.diagnostics.hal_signals_scrollable,
                    };
                    hints.push(Shortcut::hint(
                        "PgUp/PgDn",
                        if self.diagnostics.list_focused && scrollable {
                            "SCROLL"
                        } else {
                            "TABS"
                        },
                    ));
                    groups.push(ShortcutGroup {
                        title: "HAL BROWSER",
                        items: vec![
                            Shortcut::run("/", "SEARCH", ShortcutCommand::DiagSearch),
                            Shortcut::run("Enter", "LIST", ShortcutCommand::DiagList),
                            Shortcut::run("\u{2190}/\u{2192}", "PANE", ShortcutCommand::DiagPane),
                            Shortcut::run("F", "FASTER", ShortcutCommand::DiagFaster),
                            Shortcut::run("S", "SLOWER", ShortcutCommand::DiagSlower),
                            Shortcut::hint(k.back.label(), "SEARCH"),
                        ],
                    });
                }
                DiagnosticsTab::Logs => {
                    hints.push(Shortcut::hint(
                        "PgUp/PgDn",
                        if self.diagnostics.list_focused && self.diagnostics.logs_scrollable {
                            "SCROLL"
                        } else {
                            "TABS"
                        },
                    ));
                    groups.push(ShortcutGroup {
                        title: "LOG LIST",
                        items: vec![
                            Shortcut::run("/", "SEARCH", ShortcutCommand::DiagSearch),
                            Shortcut::run("Enter", "LIST", ShortcutCommand::DiagList),
                            Shortcut::hint(k.back.label(), "SEARCH"),
                        ],
                    });
                    // One letter per toggle, read only while the list holds the
                    // keyboard: in the search line they are text.
                    groups.push(ShortcutGroup {
                        title: "LOG FILTER",
                        items: vec![
                            Shortcut::hint("I W E C", "SEVERITY"),
                            Shortcut::hint("P G B T H", "PANEL..HAL"),
                            Shortcut::hint("N M O U Y", "NML..SYSTEM"),
                            Shortcut::run("A", "CLEAR ALL", ShortcutCommand::DiagClearFilters),
                        ],
                    });
                }
                _ => hints.push(Shortcut::hint("PgUp/PgDn", "TABS")),
            },
            Page::Manual | Page::Homing => {
                // Three groups: the toolpath, the spindle, and the keys the page
                // itself owns. The spindle group is manual modes only: HOME and
                // PROG.HOME share this page family without a spindle control.
                let mut view_items = vec![
                    Shortcut::run(k.graph_fit.label(), "FIT", ShortcutCommand::FitGraph),
                    Shortcut::run(view_cycle.clone(), "VIEW", ShortcutCommand::CycleView),
                    Shortcut::run(
                        k.toggle_bounds.label(),
                        "BOUNDS",
                        ShortcutCommand::CycleBounds,
                    ),
                    Shortcut::run(
                        k.graph_clear.label(),
                        "CLEAR TRAIL",
                        ShortcutCommand::ClearTrail,
                    ),
                    Shortcut::run(
                        k.remaining.label(),
                        "REMAINING",
                        ShortcutCommand::CycleRemaining,
                    ),
                    Shortcut::hint(k.view_pan_hint(), "PAN"),
                    Shortcut::hint(k.view_zoom_hint(), "ZOOM"),
                ];
                view_items.extend(lathe_hints.clone());
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: view_items,
                });

                if self.nav.page == Page::Manual
                    && matches!(
                        self.snapshot.panel.mode.mode(),
                        Some(PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg)
                    )
                {
                    // The group title names the subject, so the rows name only
                    // the action: FASTER, SLOWER, SET RPM.
                    groups.push(ShortcutGroup {
                        title: "SPINDLE",
                        items: vec![
                            Shortcut::hint(k.spindle_speed_up.label(), "FASTER"),
                            Shortcut::hint(k.spindle_speed_down.label(), "SLOWER"),
                            Shortcut::hint(k.spindle_speed.label(), "SET RPM"),
                        ],
                    });
                }

                let mut key_items = vec![Shortcut::hint("Arrows", "SELECT")];
                // Only INC takes the digits: a continuous JOG has no increment
                // to pick, MPG selects its step on the handwheel, and
                // HOME/PROG.HOME share this page family without a step row.
                if self.nav.page == Page::Manual
                    && self.snapshot.panel.mode.mode() == Some(PanelMode::Inc)
                    && !self.increments().is_empty()
                {
                    key_items.push(Shortcut::hint(
                        format!("1-{}", self.increments().len()),
                        "INCREMENT",
                    ));
                }
                key_items.push(Shortcut::hint("X/Y/Z", "ZERO AXIS"));
                key_items.push(Shortcut::run(
                    "A",
                    "ZERO ALL",
                    ShortcutCommand::ManualZeroAll,
                ));
                key_items.push(Shortcut::run(
                    "O",
                    "LIMIT RECOVERY",
                    ShortcutCommand::LimitRecovery,
                ));
                groups.push(ShortcutGroup {
                    title: if self.nav.page == Page::Homing {
                        "HOMING"
                    } else {
                        "MANUAL"
                    },
                    items: key_items,
                });
            }
            Page::Offsets => {
                hints.push(Shortcut::hint("Arrows", "SELECT"));
                hints.push(Shortcut::run(
                    "A",
                    "ACTIVATE",
                    ShortcutCommand::OffsetsActivate,
                ));
                hints.push(Shortcut::hint("X/Y/Z", "EDIT DRO"));
                hints.push(Shortcut::run(
                    offset_key.clone(),
                    "PROBE",
                    ShortcutCommand::CycleOffsets,
                ));
                // The preview beside the table is the same toolpath view as
                // MAIN's, so the view keys work here too.
                hints.push(Shortcut::run(
                    view_cycle.clone(),
                    "VIEW",
                    ShortcutCommand::CycleView,
                ));
                hints.push(Shortcut::run(
                    k.graph_fit.label(),
                    "FIT",
                    ShortcutCommand::FitGraph,
                ));
                hints.push(Shortcut::run(
                    k.toggle_bounds.label(),
                    "BOUNDS",
                    ShortcutCommand::CycleBounds,
                ));
                hints.push(Shortcut::run(
                    k.remaining.label(),
                    "REMAINING",
                    ShortcutCommand::CycleRemaining,
                ));
                hints.push(Shortcut::hint(k.view_pan_hint(), "PAN"));
                hints.push(Shortcut::hint(k.view_zoom_hint(), "ZOOM"));
                hints.extend(lathe_hints.clone());
            }
            Page::Probe => {
                use linc_core::probe::ProbeCategory;

                match self.probe.category {
                    ProbeCategory::ProbeHelp => groups.push(ShortcutGroup {
                        title: "PROBE HELP",
                        items: vec![
                            Shortcut::hint("Up/Down", "TOPIC"),
                            Shortcut::hint("Left/Right", "PAGE"),
                            Shortcut::hint("PgUp/PgDn", "TABS"),
                            Shortcut::hint(offset_key.clone(), "NEXT TAB"),
                            Shortcut::hint(k.back.label(), "OFFSETS"),
                        ],
                    }),
                    ProbeCategory::Calibrate => groups.push(ShortcutGroup {
                        title: "CALIBRATION KEYS",
                        items: vec![
                            Shortcut::hint("1-4", "HIGHLIGHT"),
                            Shortcut::run("Enter", "RUN CALIBRATION", ShortcutCommand::ProbeRun),
                            Shortcut::hint("V", "AVG XY / X / Y"),
                            Shortcut::run("Ctrl+P", "DIMENSIONS", ShortcutCommand::ProbeDimensions),
                            Shortcut::run(
                                "Ctrl+R",
                                "RESET CAL",
                                ShortcutCommand::ProbeResetCalibration,
                            ),
                            Shortcut::hint("PgUp/PgDn", "TABS"),
                            Shortcut::hint(offset_key.clone(), "NEXT TAB"),
                            Shortcut::hint(k.back.label(), "OFFSETS"),
                        ],
                    }),
                    category => {
                        let mut items = vec![
                            Shortcut::hint("1-9", "HIGHLIGHT"),
                            Shortcut::run("Enter", "RUN PROBE", ShortcutCommand::ProbeRun),
                            Shortcut::run(
                                "W",
                                "POSITION / DIRECT",
                                ShortcutCommand::ProbePositionMode,
                            ),
                            Shortcut::run("A", "APPLY RESULT", ShortcutCommand::ProbeApply),
                            Shortcut::run("Ctrl+P", "PARAMETERS", ShortcutCommand::ProbeParameters),
                        ];
                        if category == ProbeCategory::EdgeAngle {
                            items.push(Shortcut::run(
                                "R",
                                "APPLY ROTATION",
                                ShortcutCommand::ProbeApplyRotation,
                            ));
                        }
                        items.extend([
                            Shortcut::hint("X/Y/Z", "EDIT VISIBLE DRO"),
                            Shortcut::hint("PgUp/PgDn", "TABS"),
                            Shortcut::hint(offset_key.clone(), "NEXT TAB"),
                            Shortcut::hint(k.back.label(), "OFFSETS"),
                        ]);
                        groups.push(ShortcutGroup {
                            title: "PROBE KEYS",
                            items,
                        });
                        groups.push(ShortcutGroup {
                            title: "RESULT KEYS",
                            items: vec![
                                Shortcut::run(
                                    "U",
                                    "UPDATE PARAMS",
                                    ShortcutCommand::ProbeUpdateParams,
                                ),
                                Shortcut::run(
                                    "Ctrl+R",
                                    "RESET ALL DATA",
                                    ShortcutCommand::ProbeResetAll,
                                ),
                                Shortcut::hint("Ctrl+X/Y", "RESET X/Y DATA"),
                            ],
                        });
                    }
                }
            }
            Page::Tools => {
                hints.push(Shortcut::run(
                    k.help_search.label(),
                    "SEARCH",
                    ShortcutCommand::ToolsSearch,
                ));
                hints.push(Shortcut::hint("Arrows", "SELECT"));
                hints.push(Shortcut::run("Enter", "EDIT", ShortcutCommand::ToolsEdit));
                hints.push(Shortcut::run("T", "LOAD + G43", ShortcutCommand::ToolsLoad));
                hints.push(Shortcut::run(
                    k.tool_number.label(),
                    "TYPE NUMBER",
                    ShortcutCommand::ToolsNumber,
                ));
                hints.push(Shortcut::run(
                    "Z",
                    "TOOL POSITION",
                    ShortcutCommand::ToolsPosition,
                ));
                hints.push(Shortcut::run(
                    "Ctrl+N",
                    "NEW TOOL",
                    ShortcutCommand::ToolsNew,
                ));
                hints.push(Shortcut::run(
                    "D",
                    "DUPLICATE",
                    ShortcutCommand::ToolsDuplicate,
                ));
                if self.snapshot.loobric.is_some() {
                    hints.push(Shortcut::run(
                        "S",
                        "SYNC LOOBRIC",
                        ShortcutCommand::ToolsSync,
                    ));
                }
                hints.push(Shortcut::run(
                    "Backspace",
                    "DELETE TOOL",
                    ShortcutCommand::ToolsDelete,
                ));
            }
        }
        if !hints.is_empty() {
            groups.push(ShortcutGroup {
                title: "",
                items: hints,
            });
        }
        groups
    }
}
