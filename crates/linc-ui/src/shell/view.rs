//! The toolpath card's view controls: framing, pan, zoom, trail, and the
//! preview the card projects.

use super::*;


/// Screen pixels the toolpath moves per arrow-key pan.
///
/// A few presses should cross the part, but one press must not jump it out of
/// the card; 40 px is about a short drag.
pub(super) const VIEW_PAN_STEP: f32 = 40.0;

/// Zoom per key press, matching one wheel notch.
pub(super) const VIEW_ZOOM_STEP: f32 = 1.25;

/// Is this snapshot's trail still the one from before `Ctrl+L`?
///
/// The backend applies the clear on its next conversion. Until then its
/// snapshot carries the old points, and repainting them made the trail flash
/// away and come straight back. The applied clear leaves a single point; a moved
/// tool tip means the points are new and should be kept.
pub(super) fn cleared_trail_pending(snapshot: &MachineSnapshot, cleared_tip: Option<[f64; 3]>) -> bool {
    let moved = match (snapshot.tool_tip, cleared_tip) {
        (Some(now), Some(before)) => {
            (now[0] - before[0]).abs() > 1e-4
                || (now[1] - before[1]).abs() > 1e-4
                || (now[2] - before[2]).abs() > 1e-4
        }
        _ => false,
    };
    !(snapshot.live_trail.len() <= 2 || moved)
}

impl LincApp {
    pub fn refresh_preview(&mut self) {
        let text = self.program.editor.text();
        // The frame is the controller's offset, program or not. A jog screen that
        // has never loaded a program still has to place the tool at its machine
        // position: with preview_offset left at None, live_trail_shift collapses
        // to work - machine, and the marker is drawn on the tool's work
        // coordinate - machine home once the operator goes to WCS zero, and off
        // by the whole offset before that.
        let offset = self.preview_offset(&text);
        self.program.preview_offset = offset.clone();
        self.program.preview_offsets_seen = self.snapshot.offsets.revision;
        self.program.preview_offsets_rows = offsets_signature(&self.snapshot.offsets.offsets);
        if text.is_empty() {
            self.program.preview = Preview::empty();
            self.program.last_valid_preview = Preview::empty();
            return;
        }
        let axes = self.axes();
        // The plot is drawn in machine space with the work offset applied, so the
        // part sits where the work zero is on the machine. Which system: the one
        // the program selects (a `G54`/`G55` in its preamble is where it will
        // cut), falling back to the controller's active system for a program that
        // selects none.
        let mut preview = parse_preview_with(
            &text,
            &axes,
            offset.as_ref(),
            self.tool_tip(),
            &self.profile.subroutines(),
        );
        // The first rapid is the approach from the live tool position to the
        // program start. Keep the program geometry; do not draw that connector.
        preview.trim_approach(self.tool_tip());
        let valid = preview.error.is_none();
        if valid {
            self.program.last_valid_preview = preview.clone();
        }
        self.program.preview = preview;
    }

    /// The work offset the preview is drawn with, in machine coordinates: the
    /// system the program selects, else the controller's active one.
    pub(super) fn preview_offset(&self, text: &str) -> Option<linc_core::model::AxisMap<f64>> {
        let index = linc_core::gcode::program_wcs(text).or(self.snapshot.offsets.active_index)?;
        self.snapshot
            .offsets
            .offsets
            .iter()
            .find(|row| row.index == index)
            .map(|row| row.values.clone())
            .or_else(|| self.active_offset().cloned())
    }

    pub fn tick_preview(&mut self) {
        if let Some(due) = self.program.preview_due {
            if Instant::now() >= due {
                self.program.preview_due = None;
                self.preview_ticks += 1;
                self.refresh_preview();
            }
        }
    }

    /// Cycle the toolpath view through ISO, X, Y and Z, refitting each time so
    /// the new projection frames the program instead of inheriting a zoom that
    /// only suited the old one.
    /// `Ctrl+B`: dimensions only -> + machine box -> + program box -> off.
    pub fn cycle_bounds(&mut self) {
        self.graph_style.bounds_mode = (self.graph_style.bounds_mode + 1) % 4;
        self.preferences.bounds_mode = Some(self.graph_style.bounds_mode);
        self.preferences_dirty = true;
        let label = match self.graph_style.bounds_mode {
            0 => "bounds: dimensions",
            1 => "bounds: + machine",
            2 => "bounds: + program box",
            _ => "bounds: off",
        };
        self.toast(label, theme::MUTED);
    }

    /// `Ctrl+M`: mirror the lathe's X about the Z centreline of the turning
    /// view. Screen only; the model keeps its coordinates.
    pub fn cycle_lathe_mirror(&mut self) {
        let on = !self.graph_style.lathe_mirror;
        self.graph_style.lathe_mirror = on;
        self.preferences.lathe_mirror = Some(on);
        self.preferences_dirty = true;
        // The pan was aimed at the picture the other way round, so re-frame:
        // a flipped part that keeps the old pan can slide off the card.
        self.program.fit_requested = true;
        self.manual.fit = true;
        self.toast(
            if on {
                "lathe X: mirrored"
            } else {
                "lathe X: not mirrored"
            },
            theme::MUTED,
        );
    }

    /// `Ctrl+M`: REMAINING. The live trail goes away and the part of the
    /// program path the machine has already run is eaten away, block by
    /// completed block. A view mode: no coordinate, offset or controller state
    /// changes, and the eaten blocks come back with the mode.
    pub fn cycle_remaining(&mut self) {
        let on = !self.graph_style.remaining;
        self.graph_style.remaining = on;
        self.preferences.remaining = Some(on);
        self.preferences_dirty = true;
        self.toast(
            if on {
                "remaining: path eaten behind the tool"
            } else {
                "remaining: off, full path"
            },
            theme::MUTED,
        );
    }

    /// `Ctrl+E`: read the lathe's X as a radius or a diameter. Presentation
    /// only: the X position the controller reports is a radius, and every
    /// command still carries that number.
    pub fn cycle_lathe_diameter(&mut self) {
        let diameter = !self.lathe_diameter;
        self.lathe_diameter = diameter;
        self.preferences.lathe_diameter = Some(diameter);
        self.preferences_dirty = true;
        self.toast(
            if diameter {
                "lathe X: diameter (2X)"
            } else {
                "lathe X: radius (X)"
            },
            theme::MUTED,
        );
    }

    /// Whether the X readouts show a diameter. A mill's X is a position, never
    /// a diameter, so the flag only lives on a lathe.
    pub fn reads_x_as_diameter(&self) -> bool {
        self.lathe_diameter && self.profile.variant == MachineVariant::Lathe
    }

    pub fn cycle_toolpath_view(&mut self) {
        let next = linc_view::cycle_view(self.program.graph_view, self.profile.variant);
        self.program.graph_view = next;
        self.program.fit_requested = true;
        // The manual and homing pages keep their own view state; keep it in step
        // so the key does the same thing wherever a toolpath is on screen.
        self.manual.view = next;
        self.manual.fit = true;
    }

    /// Fit the program toolpath: reset zoom/pan and ask the view to reframe.
    pub fn fit_graph(&mut self) {
        // The manual modes draw their own view state.
        if matches!(self.page, Page::Manual | Page::Homing) {
            self.manual.zoom = 1.0;
            self.manual.pan = Vec2::ZERO;
            self.manual.fit = true;
        } else {
            // No placeholder zoom or pan here: the fit recomputes both from the
            // card's rect, and writing the old defaults first only painted a
            // bogus "ZOOM 1.0x" in the card's readout for the frame before the
            // fit ran. That one-frame value was the whole visible effect of
            // Ctrl+0 on a view that was already fitted.
            self.program.fit_requested = true;
        }
    }

    /// Pan and zoom the toolpath on the page in front.
    ///
    /// The chords are hover-scoped, so the page decides which view state they
    /// move. A view the operator has moved stops auto-fitting: while it is on,
    /// the fit is recomputed every frame and would undo the press.
    pub(super) fn pan_view(&mut self, pan: Vec2, zoom: f32) {
        let (zoom_out, pan_out, auto_fit) = match self.page {
            Page::Manual | Page::Homing => (
                &mut self.manual.zoom,
                &mut self.manual.pan,
                &mut self.manual.auto_fit,
            ),
            Page::Offsets => (
                &mut self.offsets.preview_zoom,
                &mut self.offsets.preview_pan,
                &mut self.offsets.preview_auto_fit,
            ),
            _ => (
                &mut self.program.graph_zoom,
                &mut self.program.graph_pan,
                &mut self.program.graph_auto_fit,
            ),
        };
        // Zoom about the card's centre, not the point the projection happens to
        // anchor on: the projection keeps the screen point at
        // `rect.center() + pan` fixed, so carrying the pan with the factor is
        // what keeps the part where the operator sees it instead of walking it
        // off the card. The arrow pan is a screen distance and comes after.
        // The upper bound belongs to the view, which knows how wide the card is
        // and stops when the screen spans 0.01 mm (`linc_view::max_zoom`).
        // Clamping to the old fixed ceiling here would keep the keyboard at a
        // shallower limit than the wheel.
        let before = (*zoom_out).max(linc_view::MIN_ZOOM);
        let after = (before * zoom).max(linc_view::MIN_ZOOM);
        *pan_out *= after / before;
        *zoom_out = after;
        *pan_out += pan;
        *auto_fit = false;
    }

    pub fn clear_trail(&mut self) {
        // View-only action. The backend is not asked to change machine state;
        // the live trail is re-seeded at the current tool tip in the local view.
        // The backend applies the clear on its next conversion, so the next few
        // snapshots still carry the old points; `trail_cleared` masks them until
        // either that clear lands or the tool moves.
        let tip = self.tool_tip();
        self.snapshot.live_trail =
            std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(tip, false)]);
        self.snapshot.tool_tip = Some(tip);
        self.trail_cleared = true;
        self.trail_cleared_tip = Some(tip);
        self.send(MachineCommand::ClearTrail);
        self.toast("live trail cleared", theme::MUTED);
    }
}

