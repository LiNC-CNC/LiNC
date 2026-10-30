//! Preference writes, batched and throttled so a keystroke does not hit disk.

use super::*;


impl LincApp {
    /// Keep the MDI buffer across restarts: the blocks are the operator's own
    /// machine commands, and recalling one is why the buffer exists.
    pub fn remember_mdi_history(&mut self) {
        self.preferences.mdi_history = self.mdi.block_texts();
        self.preferences_dirty = true;
    }

    pub(super) fn remember_program_view(&mut self, view: ProgramView) {
        let key = match view {
            ProgramView::Split => "split",
            ProgramView::Code => "code",
        };
        if self.preferences.program_view.as_deref() != Some(key) {
            self.preferences.program_view = Some(key.to_string());
            self.preferences_dirty = true;
        }
    }

    /// Flush preferences at most every two seconds while running, so a crash
    /// cannot lose more than a couple of seconds of UI state.
    /// Ask for the preferences file to be written on the next save window.
    pub fn touch_preferences(&mut self) {
        self.preferences_dirty = true;
    }

    /// Persist operator-owned probe settings after a committed UI edit. Serde
    /// skips the profile-owned tool number and controller-owned calibration.
    pub fn remember_probe_params(&mut self) {
        let params = self.probe.params.clone();
        if self.preferences.probe.as_ref() != Some(&params) {
            self.preferences.probe = Some(params);
            self.preferences_dirty = true;
        }
    }

    pub fn save_preferences(&mut self, force: bool) {
        if !self.preferences_dirty {
            return;
        }
        if !force && self.preferences_saved_at.elapsed() < Duration::from_secs(2) {
            return;
        }
        match self.preferences.save(&self.profile.preferences_path) {
            Ok(()) => {
                self.preferences_dirty = false;
                self.preferences_saved_at = std::time::Instant::now();
            }
            Err(error) => {
                log::warn!("cannot write preferences: {error}");
                self.preferences_dirty = false;
            }
        }
    }
}

