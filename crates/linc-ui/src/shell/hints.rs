//! The sidebar SHORTCUTS pane.

use super::*;

/// One titled group of page-local keyboard commands shown in the sidebar.
/// An empty title means the group has no header (most pages).
pub(super) struct ShortcutGroup {
    pub(super) title: &'static str,
    pub(super) items: Vec<(String, &'static str)>,
}

impl LincApp {
    /// Keyboard commands available on the current page, grouped and titled for
    /// the sidebar. Rendered as hints only — they are not buttons.
    pub(super) fn key_hints(&self) -> Vec<ShortcutGroup> {
        let k = &self.prefs.keymap;
        // The square is the key glyph for the view toggles: F10 alone flips
        // SPLIT/CODE, Ctrl+F10 cycles the toolpath view. Both sit at the top of
        // the pane so the display commands are the first things in reach.
        let square = "\u{25a2}";
        // The lathe display toggles: the label names what the key does next, so
        // the pane reads as an action rather than as the current state.
        let lathe_hints: Vec<(String, &'static str)> =
            if self.profile.variant == MachineVariant::Lathe {
                vec![
                    (
                        k.toggle_lathe_mirror.label(),
                        if self.graph_style.lathe_mirror {
                            "UNMIRROR"
                        } else {
                            "MIRROR"
                        },
                    ),
                    (
                        k.toggle_lathe_diameter.label(),
                        if self.chrome.lathe_diameter {
                            "RADIUS"
                        } else {
                            "DIAMETER"
                        },
                    ),
                ]
            } else {
                Vec::new()
            };
        let mut groups: Vec<ShortcutGroup> = Vec::new();
        if self.program_active() {
            groups.push(ShortcutGroup {
                title: "PROGRAM CONTROL",
                items: vec![(k.back.label(), "RESET")],
            });
        }
        let mut hints: Vec<(String, &'static str)> = Vec::new();
        match self.nav.page {
            Page::Program => {
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: [
                        vec![
                            (
                                square.to_string(),
                                if self.program.view == ProgramView::Split {
                                    "CODE"
                                } else {
                                    "SPLIT"
                                },
                            ),
                            (format!("Ctrl+{square}"), "VIEW"),
                            (k.graph_fit.label(), "FIT"),
                            (k.graph_clear.label(), "CLEAR TRAIL"),
                            (k.toggle_bounds.label(), "BOUNDS"),
                            (k.remaining.label(), "REMAINING"),
                            (k.view_pan_hint(), "PAN"),
                            (k.view_zoom_hint(), "ZOOM"),
                        ],
                        lathe_hints.clone(),
                    ]
                    .concat(),
                });
                groups.push(ShortcutGroup {
                    title: "PROGRAM",
                    items: vec![
                        ("FILE".into(), "BROWSER"),
                        (k.program_run_from_line.label(), "RUN LINE"),
                        (k.goto_line.label(), "GOTO"),
                        // No MESSAGES key here: while the editor has the
                        // keyboard a plain letter is text, so the shortcut
                        // advertised a key that types an M into the program.
                        (k.save.label(), "SAVE"),
                        (k.undo.label(), "UNDO"),
                        (k.redo.label(), "REDO"),
                    ],
                });
            }
            Page::Machine => {
                // MAIN shows the same toolpath, so it carries the same view
                // commands, plus the two program actions that do not need the
                // editor in front (run from line and go to line).
                groups.push(ShortcutGroup {
                    title: "VIEW",
                    items: [
                        vec![
                            (format!("Ctrl+{square}"), "VIEW"),
                            (k.graph_fit.label(), "FIT"),
                            (k.graph_clear.label(), "CLEAR TRAIL"),
                            (k.toggle_bounds.label(), "BOUNDS"),
                            (k.remaining.label(), "REMAINING"),
                            // The square swaps the feed/servo pair below the
                            // graph for the following-error traces, and names
                            // what the next press brings back.
                            (
                                square.to_string(),
                                if self.following.active {
                                    "FEED"
                                } else {
                                    "F.ERROR"
                                },
                            ),
                            (k.view_pan_hint(), "PAN"),
                            (k.view_zoom_hint(), "ZOOM"),
                        ],
                        lathe_hints.clone(),
                    ]
                    .concat(),
                });
                if self.mdi_pane_visible() {
                    // While the pane is up its keys replace the MAIN ones:
                    // Enter and the arrows act on the buffer, Ctrl+R runs the
                    // selected block instead of a program line.
                    groups.push(ShortcutGroup {
                        title: "MDI",
                        items: vec![
                            (k.mdi_issue.label(), "ISSUE"),
                            ("Up/Down".into(), "BUFFER"),
                            (k.goto_line.label(), "GOTO"),
                            (k.program_run_from_line.label(), "RUN LINE"),
                            (k.mdi_buffer_delete.label(), "DELETE"),
                            (k.save.label(), "SAVE"),
                            (k.back.label(), "CLEAR"),
                        ],
                    });
                } else {
                    groups.push(ShortcutGroup {
                        title: "PROGRAM",
                        items: vec![
                            ("FILE".into(), "BROWSER"),
                            (k.program_run_from_line.label(), "RUN LINE"),
                            (k.goto_line.label(), "GOTO"),
                            (k.messages.label(), "MESSAGES"),
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
                        ("\u{2190}/\u{2192}".into(), "PANE"),
                        (square.to_string(), right_pane),
                        ("Enter".into(), enter),
                    ],
                });
                // Copy, rename and delete act on the focused pane's directory
                // row; the recent list is not one, so only NEW is offered there.
                let file_items: Vec<(String, &'static str)> = if recent_focus {
                    vec![("N".into(), "NEW")]
                } else {
                    vec![
                        ("C".into(), "COPY"),
                        ("R".into(), "RENAME"),
                        ("N".into(), "NEW"),
                        (format!("Bksp/{}", k.files_delete.label()), "DELETE"),
                    ]
                };
                groups.push(ShortcutGroup {
                    title: "FILE",
                    items: file_items,
                });
                groups.push(ShortcutGroup {
                    title: "MEDIA",
                    items: vec![(k.refresh.label(), "REFRESH"), ("E".into(), "EJECT")],
                });
            }
            Page::Help => {
                hints.push((
                    format!("{} / {}", k.help_search.label(), k.find.label()),
                    "SEARCH",
                ));
                hints.push(("Enter".into(), "READ"));
                hints.push(("Bksp".into(), "BACK"));
                hints.push(("PgUp/PgDn".into(), "PAGE"));
            }
            Page::Diagnostics => hints.push(("PgUp/PgDn".into(), "TABS")),
            Page::Manual | Page::Homing => {
                hints.push((k.graph_fit.label(), "FIT"));
                hints.push((format!("Ctrl+{square}"), "VIEW"));
                hints.push((k.toggle_bounds.label(), "BOUNDS"));
                hints.push((k.graph_clear.label(), "CLEAR TRAIL"));
                hints.push((k.remaining.label(), "REMAINING"));
                hints.push((k.view_pan_hint(), "PAN"));
                hints.push((k.view_zoom_hint(), "ZOOM"));
                hints.extend(lathe_hints.clone());
                hints.push(("Arrows".into(), "SELECT"));
                // Only INC takes the digits: a continuous JOG has no increment
                // to pick, MPG selects its step on the handwheel, and
                // HOME/PROG.HOME share this page family without a step row.
                if self.nav.page == Page::Manual
                    && self.snapshot.panel.mode.mode() == Some(PanelMode::Inc)
                    && !self.increments().is_empty()
                {
                    hints.push((format!("1-{}", self.increments().len()), "INCREMENT"));
                }
                hints.push(("X/Y/Z".into(), "ZERO AXIS"));
                hints.push(("A".into(), "ZERO ALL"));
            }
            Page::Offsets => {
                hints.push(("Arrows".into(), "SELECT"));
                hints.push(("A".into(), "ACTIVATE"));
                hints.push(("X/Y/Z".into(), "EDIT DRO"));
                hints.push(("OFFSET".into(), "PROBE"));
                hints.push((k.toggle_bounds.label(), "BOUNDS"));
                hints.push((k.remaining.label(), "REMAINING"));
                hints.push((k.view_pan_hint(), "PAN"));
                hints.push((k.view_zoom_hint(), "ZOOM"));
                hints.extend(lathe_hints.clone());
            }
            Page::Probe => {
                use linc_core::probe::ProbeCategory;

                match self.probe.category {
                    ProbeCategory::ProbeHelp => groups.push(ShortcutGroup {
                        title: "PROBE HELP",
                        items: vec![
                            ("Up/Down".into(), "TOPIC"),
                            ("Left/Right".into(), "PAGE"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ],
                    }),
                    ProbeCategory::Calibrate => groups.push(ShortcutGroup {
                        title: "CALIBRATION KEYS",
                        items: vec![
                            ("1-4".into(), "HIGHLIGHT"),
                            ("Enter".into(), "RUN CALIBRATION"),
                            ("V".into(), "AVG XY / X / Y"),
                            ("Ctrl+P".into(), "DIMENSIONS"),
                            ("Ctrl+R".into(), "RESET CAL"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ],
                    }),
                    category => {
                        let mut items = vec![
                            ("1-9".into(), "HIGHLIGHT"),
                            ("Enter".into(), "RUN PROBE"),
                            ("W".into(), "POSITION / DIRECT"),
                            ("A".into(), "APPLY RESULT"),
                            ("Ctrl+P".into(), "PARAMETERS"),
                        ];
                        if category == ProbeCategory::EdgeAngle {
                            items.push(("R".into(), "APPLY ROTATION"));
                        }
                        items.extend([
                            ("X/Y/Z".into(), "EDIT VISIBLE DRO"),
                            ("PgUp/PgDn".into(), "TABS"),
                            ("OFFSET".into(), "NEXT TAB"),
                            (k.back.label(), "OFFSETS"),
                        ]);
                        groups.push(ShortcutGroup {
                            title: "PROBE KEYS",
                            items,
                        });
                        groups.push(ShortcutGroup {
                            title: "RESULT KEYS",
                            items: vec![
                                ("U".into(), "UPDATE PARAMS"),
                                ("Ctrl+R".into(), "RESET ALL DATA"),
                                ("Ctrl+X/Y".into(), "RESET X/Y DATA"),
                            ],
                        });
                    }
                }
            }
            Page::Tools => {
                hints.push(("Arrows".into(), "SELECT"));
                hints.push(("Enter".into(), "EDIT"));
                hints.push(("T".into(), "LOAD + G43"));
                hints.push((k.tool_number.label(), "TYPE NUMBER"));
                hints.push(("Z".into(), "TOOL POSITION"));
                hints.push(("Ctrl+N".into(), "NEW TOOL"));
                hints.push(("Backspace".into(), "DELETE TOOL"));
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
