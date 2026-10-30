//! Jog increments, jog speed, and the handwheel feed.

use super::*;


impl LincApp {
    /// The finite steps the INI declares.
    pub fn increments(&self) -> &[(String, f64)] {
        &self.jog_increments
    }

    pub fn jog_increment_index(&self) -> usize {
        self.jog_increment_index
            .min(self.increments().len().saturating_sub(1))
    }

    /// Selected increment in machine units; 0.0 means continuous jog.
    pub fn jog_increment_value(&self) -> f64 {
        crate::state::jog_increment(self.increments(), self.jog_increment_index()).1
    }

    pub fn jog_increment_label(&self) -> Option<String> {
        crate::state::jog_increment(self.increments(), self.jog_increment_index())
            .0
            .into()
    }

    /// Publish the selection to `linc.jog-increment` when it changes.
    pub fn set_jog_increment_index(&mut self, index: usize) {
        let index = index.min(self.increments().len().saturating_sub(1));
        if index == self.jog_increment_index {
            return;
        }
        self.jog_increment_index = index;
        self.preferences.jog_increment = Some(index);
        self.preferences_dirty = true;
        let value = self.jog_increment_value();
        self.send(linc_core::command::MachineCommand::SetHalOutput {
            output: linc_core::command::LincOutput::JogIncrement,
            value: linc_core::command::HalValue::Float(value),
        });
    }

    /// Jog speed in machine units per second.
    ///
    /// The ceiling is the profile's `[jog] max_speed` in units per minute,
    /// switched to `rapid_speed` while the panel's RAPID key is held, and
    /// scaled by the operator's percentage. The machine still clamps to its own
    /// MAX_VELOCITY.
    /// The value for `halui.axis.jog-speed`.
    ///
    /// This build reads the pin as machine units per **minute**: 50 written to
    /// it jogs at 0.83 mm/s, which is what the operator saw as "1 mm/s". The
    /// page keeps thinking in units per second (`jog_speed_value`), and the
    /// conversion happens here so there is one place to change.
    pub fn jog_speed_output(&self) -> f64 {
        self.jog_speed_value() * 60.0
    }

    pub fn jog_speed_value(&self) -> f64 {
        let ceiling = if self.snapshot.panel.rapid {
            self.profile.config.jog.rapid_speed
        } else {
            self.profile.config.jog.max_speed
        };
        // The feed override scales the jog, as probe_basic does with its
        // `mult2.jog-speed` (feed override * 100 -> jog speed percentage).
        let percent = self.jog_speed_percent * self.snapshot.panel.feed_override;
        crate::state::jog_speed(ceiling / 60.0, percent)
    }

    /// `egui-winit` turns the platform clipboard shortcut (Ctrl+X) into
    /// `Event::Cut`, not a key event, so the Probe page's X data reset never
    /// sees it. Hand the shortcut back to the Probe page as the key event it
    /// expects; leave the clipboard event alone while a text field can use it,
    /// and never synthesize the chord for any other page. Refresh (Ctrl+R) and
    /// redo (Ctrl+Y) never need this because they are not clipboard shortcuts.
    pub(super) fn hand_cut_to_page(&mut self, ui: &egui::Ui) {
        // Only the Probe page binds Ctrl+X. Every other page keeps the
        // clipboard event, so Ctrl+X stays a text-editing chord there and the
        // Files page's delete is Backspace.
        if self.page != Page::Probe {
            return;
        }
        if ui.ctx().text_edit_focused() || self.overlay_open() {
            return;
        }
        ui.input_mut(|input| {
            if !input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Cut))
            {
                return;
            }
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Cut));
            input.events.push(egui::Event::Key {
                key: CUT_SHORTCUT.key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: CUT_SHORTCUT.modifiers,
            });
        });
    }
}

