//! Preference writes, batched and throttled so a keystroke does not hit disk.

use super::*;

impl LincApp {
    /// Keep the MDI buffer across restarts: the blocks are the operator's own
    /// machine commands, and recalling one is why the buffer exists.
    pub fn remember_mdi_history(&mut self) {
        self.prefs.preferences.mdi_history = self.mdi.block_texts();
        self.prefs.dirty = true;
    }

    pub(super) fn remember_program_view(&mut self, view: ProgramView) {
        let key = match view {
            ProgramView::Split => "split",
            ProgramView::Code => "code",
        };
        if self.prefs.preferences.program_view.as_deref() != Some(key) {
            self.prefs.preferences.program_view = Some(key.to_string());
            self.prefs.dirty = true;
        }
    }

    /// Flush preferences at most every two seconds while running, so a crash
    /// cannot lose more than a couple of seconds of UI state.
    /// Ask for the preferences file to be written on the next save window.
    pub fn touch_preferences(&mut self) {
        self.prefs.dirty = true;
    }

    /// Persist operator-owned probe settings after a committed UI edit. Serde
    /// skips the profile-owned tool number and controller-owned calibration.
    pub fn remember_probe_params(&mut self) {
        let params = self.probe.params.clone();
        if self.prefs.preferences.probe.as_ref() != Some(&params) {
            self.prefs.preferences.probe = Some(params);
            self.prefs.dirty = true;
        }
    }

    pub fn save_preferences(&mut self, force: bool) {
        if !self.prefs.dirty {
            return;
        }
        if !force && self.prefs.saved_at.elapsed() < Duration::from_secs(2) {
            return;
        }
        match self.prefs.preferences.save(&self.profile.preferences_path) {
            Ok(()) => {
                self.prefs.dirty = false;
                self.prefs.saved_at = std::time::Instant::now();
            }
            Err(error) => {
                log::warn!("cannot write preferences: {error}");
                self.prefs.dirty = false;
            }
        }
    }
}

/// The preference file, the keymap read from it, and the write throttle.
pub(crate) struct PrefsUi {
    /// Resolved `[keys]` bindings. The profile owns the keymap; the shell
    /// consumes it instead of hardcoding keys.
    pub keymap: crate::state::Keymap,
    /// Runtime UI preferences (CONFIGURATION.md §9). Machine identity, HAL
    /// pins, safety flags and the INI path are never stored here.
    pub preferences: linc_core::preferences::Preferences,
    pub(super) dirty: bool,
    pub(super) saved_at: std::time::Instant,
}
