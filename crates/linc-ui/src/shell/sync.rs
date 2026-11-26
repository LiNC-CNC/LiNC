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

/// The EXT (`G92`) register as the preview's frame depends on it, in the same
/// stable axis order as [`offsets_signature`].
///
/// The register is not a row: it moves every system at once, and neither the
/// offsets revision nor the rows say so.
pub(super) fn register_signature(register: &linc_core::model::AxisMap<f64>) -> Vec<f64> {
    ["X", "Y", "Z"]
        .into_iter()
        .map(|axis| register.get(axis).copied().unwrap_or(0.0))
        .collect()
}

impl LincApp {
    pub fn sync(&mut self) {
        self.poll_backend();
        // A controller check is a child process on a worker thread: its verdict
        // is picked up here, never waited for in the frame that asked.
        self.poll_verify();
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
        self.service_cycle_start();
        self.sync_offsets_from_snapshot(false);
        self.sync_tools_from_snapshot(false);
        self.sync_probe();
        if self.snapshot.probe.last_result.is_some() && self.probe.last_run.is_none() {
            self.probe.last_run = Some(Instant::now());
            self.toast("probe result received", theme::SUCCESS);
        }
        self.tick_preview();
        self.tick_loops();
        self.tick_cycle_time();
        self.tick_limit_recovery();
        self.overlay
            .toasts
            .retain(|toast| toast.until > Instant::now());
    }

    /// Time the run and estimate how much of it is left. The bottom bar's CYCLE
    /// and LEFT fields read this, and so does nothing else: a wrong estimate
    /// costs the operator a glance, never a move.
    pub(super) fn tick_cycle_time(&mut self) {
        let crate::state::ProgramUi {
            loaded,
            cycle,
            loop_progress,
            ..
        } = &mut self.program;
        let input = CycleInput {
            program: &self.snapshot.program,
            preview: &loaded.preview,
            // The toolpath has already followed the cut through the preview's
            // unrolled order, so the estimate reads the same cursor the loop
            // readout does rather than looking for the cut a second time.
            cursor: loop_progress.cursor,
            tip: self.snapshot.tool_tip,
            rates: &self.profile.machine_ini,
            // Read fresh every frame: the feed and rapid override the planner is
            // applying are part of how long the moves left will take, and the
            // operator can turn them at any moment.
            overrides: linc_core::cycletime::Overrides::from_feed_state(&self.snapshot.feed),
            generation: loaded.preview_generation,
            // An edited buffer is a preview of text the controller is not
            // running, and its timing is not the program's.
            preview_matches: !loaded.editor.dirty,
        };
        cycle.tick(&input, &mut self.cycle_store);
    }

    pub(super) fn sync_program(&mut self) {
        let revision = self.snapshot.program.revision;
        if revision != self.program.loaded().program_revision_seen {
            let new_text = self.snapshot.program.content.clone();
            let new_path = self.snapshot.program.path.clone();
            let text_changed = new_text
                .as_ref()
                .map(|text| text.as_str() != self.program.loaded().editor.text())
                .unwrap_or(false);
            if self.program.loaded().editor.dirty
                && text_changed
                && new_path == self.program.loaded().path
            {
                self.program.external_change = true;
            } else if let Some(text) = new_text {
                let same_as_editor = text.as_str() == self.program.loaded().editor.text();
                let loaded = self.program.loaded_mut();
                if same_as_editor {
                    loaded.editor.mark_saved();
                } else {
                    loaded
                        .editor
                        .set_loaded(new_path.clone().unwrap_or_default(), text.to_string());
                }
                loaded.path = new_path;
                loaded.program_revision_seen = revision;
                self.program.external_change = false;
                self.refresh_loaded_preview();
                self.program.loaded_mut().selected_line = self.snapshot.program.current_line.max(1);
            } else {
                // The controller has no program any more: it was unloaded (or
                // closed from another HMI). Clear LiNC's own copy, or the editor
                // and the 3D view keep showing a program the machine cannot run -
                // text that is still editable and saveable over nothing. The
                // unsaved-buffer case is left alone: with no `path` there is no
                // file to have been unloaded.
                if self.program.loaded().path.is_some() {
                    let loaded = self.program.loaded_mut();
                    loaded.editor = Default::default();
                    loaded.path = None;
                    loaded.selected_line = 1;
                    self.program.external_change = false;
                    self.program.follow_current = false;
                    self.refresh_loaded_preview();
                }
                self.program.loaded_mut().program_revision_seen = revision;
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
            && executed != self.program.loaded().selected_line
        {
            let loaded = self.program.loaded_mut();
            loaded.selected_line = executed;
            loaded.code_scroll_to_line = Some(executed);
        }
        // Re-arm the follow only when a run starts. Doing it on every poll
        // overrode a line the operator had just picked with Ctrl+G or the
        // arrows, so the selection snapped back to the running line.
        let running = self.snapshot.task.running;
        if running && !self.program.program_was_running {
            self.program.follow_current = true;
            // A new run clears the mark the last one left in the listing.
            self.program.completed_at = None;
        } else if !running && self.program.program_was_running {
            self.on_run_finished();
        }
        self.program.program_was_running = running;
    }

    /// The run just stopped. A program that reached its last command leaves the
    /// editor at the top, because that is where the next cycle starts, and keeps
    /// the finished line marked in the listing. A run that was stopped part way,
    /// or tripped an E-stop or a fault, keeps its selection: that line answers
    /// "where did it stop", which is the question the operator has just then.
    ///
    /// Only the selection moves. `snapshot.program.current_line` is left where
    /// the controller put it, so the progress readout keeps its 100% and the
    /// REMAINING view keeps the path behind the tool eaten.
    fn on_run_finished(&mut self) {
        let program = &self.snapshot.program;
        let last = program.last_command_line();
        // The controller stops reporting at the last block that carried motion,
        // so "it reached the end" means either that line was the last command or
        // everything after it can no longer move an axis.
        let reached_end = last > 0
            && program.current_line > 0
            && (program.current_line >= last || program.tail_is_motionless(program.current_line));
        let stopped = self.snapshot.task.paused
            || self.snapshot.task.interp != linc_core::model::InterpState::Idle
            || self.snapshot.machine.estop
            || self.snapshot.machine.fault;
        if !reached_end || stopped {
            return;
        }
        self.program.completed_at = Some(program.current_line);
        let loaded = self.program.loaded_mut();
        loaded.selected_line = 1;
        loaded.code_scroll_to_line = Some(1);
        self.program.follow_current = true;
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
