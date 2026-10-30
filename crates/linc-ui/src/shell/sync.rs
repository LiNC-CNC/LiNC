//! Ingesting a machine snapshot: program, offsets, tools, probe, and the
//! following-error sampling the square key draws.

use super::*;

/// The numbers the preview's frame depends on, flattened in a stable order.
///
/// An offset row is a handful of floats; comparing them per poll is cheaper than
/// re-parsing the program to find out whether it needs re-parsing.
pub(super) fn offsets_signature(rows: &[linc_core::model::WorkOffset]) -> Vec<f64> {
    let mut out = Vec::with_capacity(rows.len() * 3);
    for row in rows {
        for axis in ["X", "Y", "Z"] {
            out.push(row.values.get(axis).copied().unwrap_or(0.0));
        }
    }
    out
}

impl LincApp {
    pub fn sync(&mut self) {
        self.poll_backend();
        let mode = self.snapshot.panel.mode.clone();
        if self.nav.last_mode.as_ref() != Some(&mode) {
            self.on_mode_change(mode.clone());
            self.nav.last_mode = Some(mode);
        }

        if self.snapshot.panel.connected != self.chrome.last_connection.unwrap_or(false) {
            self.chrome.last_connection = Some(self.snapshot.panel.connected);
        }
        let protect = self.snapshot.panel.protect_effective();
        if self.chrome.last_protect != Some(protect) {
            self.chrome.last_protect = Some(protect);
        }

        self.sync_program();
        self.sync_offsets_from_snapshot(false);
        self.sync_tools_from_snapshot(false);
        self.sync_probe();
        if self.snapshot.probe.last_result.is_some() && self.probe.last_run.is_none() {
            self.probe.last_run = Some(Instant::now());
            self.toast("probe result received", theme::SUCCESS);
        }
        self.tick_preview();
        self.overlay
            .toasts
            .retain(|toast| toast.until > Instant::now());
    }

    pub(super) fn sync_program(&mut self) {
        let revision = self.snapshot.program.revision;
        if revision != self.program.program_revision_seen {
            let new_text = self.snapshot.program.content.clone();
            let new_path = self.snapshot.program.path.clone();
            let text_changed = new_text
                .as_ref()
                .map(|text| text.as_str() != self.program.editor.text())
                .unwrap_or(false);
            if self.program.editor.dirty && text_changed && new_path == self.program.loaded_path {
                self.program.external_change = true;
            } else if let Some(text) = new_text {
                let same_as_editor = text.as_str() == self.program.editor.text();
                if same_as_editor {
                    self.program.editor.mark_saved();
                } else {
                    self.program
                        .editor
                        .set_loaded(new_path.clone().unwrap_or_default(), text.to_string());
                }
                self.program.loaded_path = new_path;
                self.program.program_revision_seen = revision;
                self.program.external_change = false;
                self.refresh_preview();
                self.program.selected_line = self.snapshot.program.current_line.max(1);
            } else {
                self.program.program_revision_seen = revision;
            }
        }
        // Only while a program is actually running: otherwise a stale
        // follow flag dragged the view back to line 1 every frame and fought
        // the operator's own cursor movement.
        // Follow the line the tool is on. `current_line` is the interpreter's
        // read-ahead and would drag the pane to the end of the program while the
        // machine is still cutting the first move.
        let executed = self.snapshot.program.executing_line();
        if self.program.follow_current
            && self.snapshot.task.running
            && executed > 0
            && executed != self.program.selected_line
        {
            self.program.selected_line = executed;
            self.program.code_scroll_to_line = Some(self.program.selected_line);
        }
        // Re-arm the follow only when a run starts. Doing it on every poll
        // overrode a line the operator had just picked with Ctrl+G or the
        // arrows, so the selection snapped back to the running line.
        if self.snapshot.task.running && !self.program.program_was_running {
            self.program.follow_current = true;
        }
        self.program.program_was_running = self.snapshot.task.running;
    }

    pub(crate) fn sync_offsets_from_snapshot(&mut self, force: bool) {
        if !force && self.snapshot.offsets.revision == self.offsets.revision_seen {
            return;
        }
        self.offsets.revision_seen = self.snapshot.offsets.revision;
        self.offsets.rows.clear();
        for offset in &self.snapshot.offsets.offsets {
            let mut draft = crate::state::OffsetDraft {
                values: std::collections::HashMap::new(),
                rotation: format!("{:.4}", offset.rotation.unwrap_or(0.0)),
            };
            for (axis, value) in offset.values.iter() {
                draft.values.insert(axis.to_string(), format!("{value:.4}"));
            }
            self.offsets.rows.insert(offset.index, draft);
        }
        self.offsets.g92 = self
            .snapshot
            .offsets
            .g92
            .iter()
            .map(|(_, value)| format!("{value:.4}"))
            .collect();
    }

    pub(super) fn sync_tools_from_snapshot(&mut self, force: bool) {
        if !force && self.snapshot.tool_table.revision == self.tools.revision_seen {
            return;
        }
        self.tools.revision_seen = self.snapshot.tool_table.revision;
        self.tools.selected_row = self
            .tools
            .selected_row
            .min(self.snapshot.tool_table.rows.len().saturating_sub(1));
        self.tools.drafts.clear();
        for row in &self.snapshot.tool_table.rows {
            self.tools
                .drafts
                .insert(row.tool, crate::state::ToolDraft::from_row(row));
        }
        if let Some(token) = self.snapshot.tool_change.token.clone() {
            self.overlay.confirm_token = Some(token);
        }
    }

    /// Copy the controller's measured #3032 value into the next parameter block.
    /// The field is read-only in the UI, so an open editor for another parameter
    /// keeps its own buffer and cannot be clobbered by this synchronization.
    pub(crate) fn sync_probe_calibration(&mut self) {
        let Some(offset) = self.snapshot.probe.calibration_offset else {
            return;
        };
        if self.probe.calibration_offset_seen == Some(offset) {
            return;
        }
        self.probe.params.calibration_offset = offset;
        self.probe.calibration_offset_seen = Some(offset);
    }

    pub(super) fn sync_probe(&mut self) {
        // The backend's legacy selected_wcs is not an Apply target. Both the
        // display and Apply follow the controller's actual active work offset.
        self.probe.wcs = self
            .snapshot
            .offsets
            .active_index
            .filter(|index| (1..=9).contains(index))
            .map(linc_core::model::wcs_name)
            .unwrap_or_else(|| "--".into());
        self.sync_probe_calibration();
        if self.snapshot.probe.enabled {
            if let Some(result) = self.snapshot.probe.last_result.as_ref() {
                if self.probe.last_run.is_none() {
                    self.probe.last_run = Some(Instant::now());
                    self.probe.last_run_operation = Some(result.operation.clone());
                }
            }
        }
    }

    /// Keep a short trace of every axis's following error for MAIN's bottom row,
    /// and the run's worst and mean values for its header.
    ///
    /// Sampled as the snapshot arrives rather than per frame: the poll is the
    /// display's own rate, and a frame-driven trace would stretch and squeeze
    /// with the renderer's speed instead of with time. The worst and the mean
    /// start again with each program, so a fresh run is never reported against
    /// the last one; jogging and MDI between programs count towards them.
    pub(super) fn sample_following_error(&mut self) {
        const SAMPLES: usize = 240;
        let running = self.snapshot.program.running;
        if running && !self.following.run_active {
            self.following.run_max = linc_core::model::AxisMap::new();
            self.following.run_abs_sum = linc_core::model::AxisMap::new();
            self.following.run_samples = linc_core::model::AxisMap::new();
        }
        self.following.run_active = running;
        let values: Vec<(String, f64)> = self
            .snapshot
            .machine
            .following_error
            .iter()
            .map(|(axis, value)| (axis.to_string(), *value))
            .collect();
        // A rapid's following error is the machine catching up with itself, not
        // the cut's accuracy: the numbers are the run's cutting error.
        let cutting = self.snapshot.motion_kind.is_cutting();
        for (axis, value) in values {
            if self.following.history.get(&axis).is_none() {
                self.following
                    .history
                    .insert(axis.clone(), std::collections::VecDeque::new());
            }
            if let Some(trace) = self.following.history.get_mut(&axis) {
                trace.push_back(value);
                while trace.len() > SAMPLES {
                    trace.pop_front();
                }
            }
            if !cutting {
                continue;
            }
            let worst = self.following.run_max.get(&axis).copied().unwrap_or(0.0);
            self.following
                .run_max
                .insert(axis.clone(), worst.max(value.abs()));
            let sum = self
                .following
                .run_abs_sum
                .get(&axis)
                .copied()
                .unwrap_or(0.0);
            self.following
                .run_abs_sum
                .insert(axis.clone(), sum + value.abs());
            let count = self.following.run_samples.get(&axis).copied().unwrap_or(0);
            self.following.run_samples.insert(axis, count + 1);
        }
    }
}

/// Following-error sampling for the square key: one run per program start.
pub(crate) struct FollowError {
    /// MAIN's bottom row shows the following-error traces instead of the feed
    /// and servo cards. Toggled with the square key there.
    pub active: bool,
    /// Following error per axis, newest last, sampled as each snapshot arrives.
    pub history: linc_core::model::AxisMap<std::collections::VecDeque<f64>>,
    /// Largest absolute following error per axis since the last program start.
    pub run_max: linc_core::model::AxisMap<f64>,
    /// Sum of absolute samples since the last program start, for the mean.
    pub run_abs_sum: linc_core::model::AxisMap<f64>,
    /// Samples behind that sum.
    pub run_samples: linc_core::model::AxisMap<u64>,
    /// Edge state: a program that starts clears the run's numbers.
    pub run_active: bool,
}
