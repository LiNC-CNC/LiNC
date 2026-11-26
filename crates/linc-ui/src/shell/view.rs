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

/// Which document a preview rebuild is for: the one the editor shows, or the
/// controller's. The two are the same document until a file is prepared.
#[derive(Clone, Copy)]
enum PreviewTarget {
    Loaded,
    Front,
}

/// Is this snapshot's trail still the one from before `Ctrl+L`?
///
/// The backend applies the clear on its next conversion. Until then its
/// snapshot carries the old points, and repainting them made the trail flash
/// away and come straight back. The applied clear leaves a single point; a moved
/// tool tip means the points are new and should be kept.
pub(super) fn cleared_trail_pending(
    snapshot: &MachineSnapshot,
    cleared_tip: Option<[f64; 3]>,
) -> bool {
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
    /// Rebuild the preview of the document in front, and the issue list that
    /// goes with it.
    pub fn refresh_preview(&mut self) {
        self.refresh_preview_in(PreviewTarget::Front);
    }

    /// Rebuild the loaded document's preview, whichever document is in front.
    ///
    /// The interlock, the travel check and the run's estimate are questions
    /// about the program the machine holds, so an external change to it rebuilds
    /// this document and never the buffer being prepared.
    pub(crate) fn refresh_loaded_preview(&mut self) {
        self.refresh_preview_in(PreviewTarget::Loaded);
    }

    fn refresh_preview_in(&mut self, target: PreviewTarget) {
        let text = self.preview_document(target).editor.text();
        // The frame is the controller's offset, program or not. A jog screen that
        // has never loaded a program still has to place the tool at its machine
        // position: with preview_offset left at None, live_trail_shift collapses
        // to work - machine, and the marker is drawn on the tool's work
        // coordinate - machine home once the operator goes to WCS zero, and off
        // by the whole offset before that.
        let offset = self.preview_offset(&text);
        let offsets_revision = self.snapshot.offsets.revision;
        let offsets_rows = offsets_signature(&self.snapshot.offsets.offsets);
        {
            let doc = self.preview_document_mut(target);
            doc.preview_offset = offset.clone();
            doc.preview_offsets_seen = offsets_revision;
            doc.preview_offsets_rows = offsets_rows;
        }
        // The controller's verdict and the travel violation are facts about the
        // program the machine holds, so only that document's list carries them.
        let loaded =
            matches!(target, PreviewTarget::Loaded) || self.program.front == DocumentFront::Loaded;
        if text.is_empty() {
            {
                let doc = self.preview_document_mut(target);
                doc.preview = Preview::empty();
                doc.last_valid_preview = Preview::empty();
                doc.issues.clear();
                doc.issue_index = 0;
            }
            // An empty buffer is not a program to run: the interlock closes here
            // too, not only on the next frame that rebuilds a preview.
            self.sync_program_ok();
            return;
        }
        let axes = self.axes();
        // The plot is drawn in machine space with the work offset applied, so the
        // part sits where the work zero is on the machine. Which system: the one
        // the program selects (a `G54`/`G55` in its preamble is where it will
        // cut), falling back to the controller's active system for a program that
        // selects none.
        // Cutter compensation draws the tool centre, so it needs the radii the
        // tool table carries; without them a G41/G42 program is drawn on its
        // programmed contour.
        let mut preview = parse_preview_full(
            &text,
            &axes,
            offset.as_ref(),
            self.tool_tip(),
            &self.profile.subroutines(),
            &Tools::from_rows(&self.snapshot.tool_table.rows),
            // The machine's EXT shift is in effect before the program starts.
            Some(&self.snapshot.offsets.g92),
        );
        // The first rapid is the approach from the live tool position to the
        // program start. Keep the program geometry; do not draw that connector.
        preview.trim_approach(self.tool_tip());
        let valid = preview.error.is_none();
        {
            let doc = self.preview_document_mut(target);
            if valid {
                doc.last_valid_preview = preview.clone();
            }
            doc.preview = preview;
            doc.preview_generation = doc.preview_generation.wrapping_add(1);
        }
        // The prepared card reports the file's whole-program time. Building that
        // plan walks every segment, so it is done once per preview rebuild and
        // cached with the generation it belongs to, not once per frame.
        if matches!(target, PreviewTarget::Front) && self.program.front == DocumentFront::Prepared {
            let plan = linc_core::cycletime::Plan::from_preview(
                &self.program.doc().last_valid_preview,
                &self.profile.machine_ini,
            );
            let total = (!plan.is_empty()).then(|| plan.parts_total());
            let doc = self.program.doc_mut();
            doc.cycle_plan = total.map(|total| (doc.preview_generation, total));
        }
        self.sync_issues_in(loaded);
        self.sync_program_ok();
    }

    fn preview_document(&self, target: PreviewTarget) -> &Document {
        match target {
            PreviewTarget::Loaded => self.program.loaded(),
            PreviewTarget::Front => self.program.doc(),
        }
    }

    fn preview_document_mut(&mut self, target: PreviewTarget) -> &mut Document {
        match target {
            PreviewTarget::Loaded => self.program.loaded_mut(),
            PreviewTarget::Front => self.program.doc_mut(),
        }
    }

    /// Take the fault list the preview just built, keeping the operator's place
    /// in it when that place still exists: an edit above the fault shifts its
    /// line but must not lose the marker the operator was working on.
    pub(super) fn sync_issues(&mut self) {
        let loaded = self.program.front == DocumentFront::Loaded;
        self.sync_issues_in(loaded);
    }

    /// `loaded` names the document the list belongs to. The controller's verdict
    /// and the travel violation are injected only into the loaded document's
    /// list; a prepared document's issues are its own preview's alone.
    fn sync_issues_in(&mut self, loaded: bool) {
        let previous = {
            let doc = self.issues_document(loaded);
            doc.issues.get(doc.issue_index).cloned()
        };
        let preview_issues = self.issues_document(loaded).preview.issues.clone();
        // A program the machine cannot reach is not a syntax fault, but it is
        // exactly what the interlock is for: the 3D view paints those moves red
        // and cycle start must refuse them. One issue, on the first offending
        // line, so the error keys walk it like any other fault.
        let travel_violation = if loaded {
            self.program.travel_violation = self.outside_travel();
            self.program.travel_violation.clone()
        } else {
            None
        };
        // The controller's own verdict is one more fault to walk when it names a
        // line: the operator must be able to reach the block the machine refused,
        // and it may be a block the block-level checks accept.
        let controller_refusal = if loaded {
            self.program.machine_check.as_ref().and_then(|check| {
                if check.outcome != linc_core::verify::CheckOutcome::Refused {
                    return None;
                }
                let line = check.marked_line().or_else(|| {
                    check
                        .message
                        .as_deref()
                        .and_then(linc_core::verify::line_from_message)
                })?;
                Some((line, check.message.clone()?))
            })
        } else {
            None
        };
        let doc = self.issues_document_mut(loaded);
        doc.issues = preview_issues;
        if let Some((line, label)) = travel_violation {
            doc.issues.push(linc_core::lint::LintIssue {
                line,
                span: None,
                message: format!("outside the machine travel envelope ({label})"),
            });
            doc.issues.sort_by_key(|issue| issue.line);
        }
        if let Some((line, message)) = controller_refusal {
            if !doc.issues.iter().any(|issue| issue.line == line) {
                doc.issues.push(linc_core::lint::LintIssue {
                    line,
                    span: None,
                    message: format!("controller: {message}"),
                });
                doc.issues.sort_by_key(|issue| issue.line);
            }
        }
        if doc.issues.is_empty() {
            doc.issue_index = 0;
            return;
        }
        // Same fault, same line: an edit elsewhere below it changes nothing.
        // Same fault, moved: an edit above it shifted the line, and the operator
        // is still looking at the fault he chose, not at whatever now occupies
        // its old line number. Otherwise the index only has to stay in range.
        let kept = previous.as_ref().and_then(|previous| {
            doc.issues
                .iter()
                .position(|issue| issue.line == previous.line && issue.message == previous.message)
                .or_else(|| {
                    doc.issues
                        .iter()
                        .position(|issue| issue.message == previous.message)
                })
        });
        let in_range = doc.issue_index.min(doc.issues.len() - 1);
        doc.issue_index = kept.unwrap_or(in_range);
    }

    fn issues_document(&self, loaded: bool) -> &Document {
        if loaded {
            self.program.loaded()
        } else {
            self.program.doc()
        }
    }

    fn issues_document_mut(&mut self, loaded: bool) -> &mut Document {
        if loaded {
            self.program.loaded_mut()
        } else {
            self.program.doc_mut()
        }
    }

    /// The first move in the preview that leaves the machine's travel box, with
    /// its line and the limit it crosses.
    ///
    /// Sampled the way the toolpath paints it — arcs by the same chords
    /// [`linc_view::arc_points`] gives the card — so the block that blocks cycle
    /// start is the block that turns red, not a neighbour of it.
    fn outside_travel(&self) -> Option<(usize, String)> {
        let limits = self.machine_bounds()?;
        let preview = &self.program.loaded().preview;
        for segment in &preview.segments {
            // Straight moves are one segment; arcs are the chords the card draws.
            let chords: Vec<([f64; 3], [f64; 3])> = match &segment.arc {
                Some(arc) => linc_view::arc_points(segment, arc)
                    .windows(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect(),
                None => vec![(segment.start, segment.end)],
            };
            let offending = chords.into_iter().find(|(start, end)| {
                !matches!(
                    linc_view::travel::classify_segment(*start, *end, &limits),
                    linc_view::travel::TravelClass::Inside
                )
            });
            if let Some((start, end)) = offending {
                let point = if point_inside_travel(start, &limits) {
                    end
                } else {
                    start
                };
                return Some((segment.line, outside_label(point, &limits)));
            }
        }
        None
    }

    /// Whether the interpreter holds exactly the program on screen, with no fault
    /// in it: the condition the machine HAL needs to allow cycle start.
    pub(super) fn program_ready(&self) -> bool {
        let loaded_doc = self.program.loaded();
        let loaded = loaded_doc
            .path
            .as_deref()
            .zip(self.snapshot.program.path.as_deref())
            .is_some_and(|(shown, loaded)| shown == loaded);
        // The controller's verdict is a fact about the text, not about the
        // editor: a refusal holds the gate even when the message names no line,
        // which is every fault the interpreter raises inside a subroutine. The
        // issue list cannot carry those, so the verdict is read directly.
        let controller_ok = !self.program.machine_check.as_ref().is_some_and(|check| {
            matches!(
                check.outcome,
                linc_core::verify::CheckOutcome::Refused | linc_core::verify::CheckOutcome::Crashed
            )
        });
        // A check in flight is not an answer: the text under the gate has no
        // verdict yet, and the previous one was about different text.
        let checked = self.program.verify.is_none();
        loaded && !loaded_doc.editor.dirty && loaded_doc.issues.is_empty() && controller_ok && checked
    }

    /// Why the interlock is shut, in one sentence the operator can act on.
    ///
    /// `program_ready` answers yes or no; a press that goes nowhere needs more
    /// than that, and so does the top bar's indicator. The order is the order the
    /// checks are made: the first thing that is wrong is the one to fix first.
    pub(super) fn program_block_reason(&self) -> Option<String> {
        let loaded_doc = self.program.loaded();
        let loaded = loaded_doc
            .path
            .as_deref()
            .zip(self.snapshot.program.path.as_deref());
        match loaded {
            Some((shown, controller)) if shown == controller => {}
            _ if loaded_doc.path.is_none() && self.snapshot.program.path.is_none() => {
                return Some("no program is loaded — pick one in FILES".into());
            }
            _ => {
                return Some("the controller holds a different file than the one on screen".into());
            }
        }
        if loaded_doc.editor.dirty {
            return Some("the buffer has unsaved changes — save it (Ctrl+S)".into());
        }
        if self.program.verify.is_some() {
            return Some("checking the program — the interpreter is reading it".into());
        }
        if let Some(check) = self.program.machine_check.as_ref().filter(|check| {
            matches!(
                check.outcome,
                linc_core::verify::CheckOutcome::Refused | linc_core::verify::CheckOutcome::Crashed
            )
        }) {
            return Some(format!(
                "the interpreter refused the program: {}",
                check.message.as_deref().unwrap_or("no reason given")
            ));
        }
        if let Some(issue) = loaded_doc.issues.first() {
            return Some(format!("line {}: {}", issue.line, issue.message));
        }
        None
    }

    /// Publish the program interlock when it changes. Called after anything that
    /// can move it: a preview rebuild, a load, a save, an unload or an edit.
    pub(super) fn sync_program_ok(&mut self) {
        let ready = self.program_ready();
        if self.program_ok_sent == Some(ready) {
            return;
        }
        self.program_ok_sent = Some(ready);
        self.send(linc_core::command::MachineCommand::SetHalOutput {
            output: linc_core::command::LincOutput::ProgramOk,
            value: linc_core::command::HalValue::Bit(ready),
        });
    }

    /// Move to the next or previous fault and put the editor on it. Unlike a
    /// plain jump, this moves the caret as well: the text cursor feeds
    /// `selected_line` back every frame while the editor has focus.
    pub fn cycle_issue(&mut self, step: isize) {
        let doc = self.program.doc_mut();
        let count = doc.issues.len();
        if count == 0 {
            return;
        }
        let current = doc.issue_index.min(count - 1) as isize;
        let next = (current + step).rem_euclid(count as isize) as usize;
        doc.issue_index = next;
        let line = doc.issues[next].line.max(1);
        doc.selected_line = line;
        doc.cursor_line = Some(line);
        doc.code_scroll_to_line = Some(line);
        // The operator has taken the cursor; do not drag it back to the running
        // line until the next run starts.
        self.program.follow_current = false;
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

    /// Follow the cut through the program's loops. `motion.program-line` is the
    /// line being cut; `current_line` is the interpreter's read-ahead and sits
    /// at the end of the program while the tool is still in the first pass.
    pub fn tick_loops(&mut self) {
        let motion = self.snapshot.program.motion_line;
        let tip = self.tool_tip();
        let crate::state::ProgramUi {
            loaded,
            loop_progress,
            ..
        } = &mut self.program;
        let generation = loaded.preview_generation;
        loop_progress.follow(&loaded.preview, motion, tip, generation);
    }

    pub fn tick_preview(&mut self) {
        if let Some(due) = self.program.doc().preview_due {
            if Instant::now() >= due {
                self.program.doc_mut().preview_due = None;
                self.view.preview_ticks += 1;
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
        self.prefs.preferences.bounds_mode = Some(self.graph_style.bounds_mode);
        self.prefs.dirty = true;
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
        self.prefs.preferences.lathe_mirror = Some(on);
        self.prefs.dirty = true;
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
        self.prefs.preferences.remaining = Some(on);
        self.prefs.dirty = true;
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
    /// command still carries that number. This is the operator's own choice and
    /// it outranks the program's `G7`/`G8` from then on.
    pub fn cycle_lathe_diameter(&mut self) {
        let diameter = !self.reads_x_as_diameter();
        self.prefs.preferences.lathe_diameter = Some(diameter);
        self.prefs.dirty = true;
        self.toast(
            if diameter {
                "lathe X: diameter (2X)"
            } else {
                "lathe X: radius (X)"
            },
            theme::MUTED,
        );
    }

    /// Whether the X readouts show a diameter.
    ///
    /// A mill's X is a position, never a diameter, so the flag only lives on a
    /// lathe. The program decides: `G7` puts a lathe interpreter in diameter
    /// mode and `G8` back in radius, which is the frame the program is written
    /// in and the one the operator reads. The `Ctrl+E` override wins once it has
    /// been used.
    pub fn reads_x_as_diameter(&self) -> bool {
        x_reads_as_diameter(
            self.profile.variant == MachineVariant::Lathe,
            self.prefs.preferences.lathe_diameter,
            self.snapshot.positions.lathe_diameter_mode,
        )
    }

    pub fn cycle_toolpath_view(&mut self) {
        let next = linc_view::cycle_view(self.program.graph_view, self.profile.variant);
        self.program.graph_view = next;
        self.program.fit_requested = true;
        // The manual and homing pages keep their own view state; keep it in step
        // so the key does the same thing wherever a toolpath is on screen.
        self.manual.view = next;
        self.manual.fit = true;
        // The OFFSETS preview reads the program's view mode but frames itself,
        // so it needs its own fit request to turn with the key.
        self.offsets.preview_fit = true;
    }

    /// Fit the program toolpath: reset zoom/pan and ask the view to reframe.
    pub fn fit_graph(&mut self) {
        // The manual modes draw their own view state.
        if matches!(self.nav.page, Page::Manual | Page::Homing) {
            self.manual.zoom = 1.0;
            self.manual.pan = Vec2::ZERO;
            self.manual.fit = true;
        } else if self.nav.page == Page::Offsets {
            // The preview beside the offset table has its own zoom, pan and
            // fit, so Ctrl+0 frames the view the operator is looking at.
            self.offsets.preview_zoom = 1.0;
            self.offsets.preview_pan = Vec2::ZERO;
            self.offsets.preview_fit = true;
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
    /// The chords are page-scoped, so the page decides which view state they
    /// move. A view the operator has moved stops auto-fitting: while it is on,
    /// the fit is recomputed every frame and would undo the press.
    pub(super) fn pan_view(&mut self, pan: Vec2, zoom: f32) {
        let (zoom_out, pan_out, auto_fit) = match self.nav.page {
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
        // shallower limit than the wheel. The zoom/pan rule itself lives in
        // `linc_view::zoom_to` so a pinch cannot drift from these keys.
        linc_view::zoom_to(
            zoom_out,
            pan_out,
            (*zoom_out).max(linc_view::MIN_ZOOM) * zoom,
        );
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
        self.view.trail_cleared = true;
        self.view.trail_cleared_tip = Some(tip);
        self.send(MachineCommand::ClearTrail);
        self.toast("live trail cleared", theme::MUTED);
    }
}

/// Toolpath view and trail state that the card reads each frame.
pub(crate) struct ViewState {
    /// The work-zero cross, in machine coordinates. Held rather than recomputed
    /// each frame: while a program loads the snapshot can report no active
    /// system for a poll, and the cross would snap to the machine origin and
    /// back.
    pub origin: [f64; 3],
    /// Set by `clear_trail`: the backend applies the clear on its next
    /// conversion, and until it does its snapshot still carries the old points.
    /// While set, those are replaced by the cleared tip so the trail cannot flash
    /// back for a frame.
    pub trail_cleared: bool,
    /// Tool tip at the moment of the clear, to tell a stale trail from new
    /// motion.
    pub trail_cleared_tip: Option<[f64; 3]>,
    /// How far through its move each axis is, from the controller's distance to
    /// go: the bar sweeps once per block, empty when the block starts and full
    /// when the axis arrives.
    pub move_progress: linc_core::model::AxisMap<super::MoveProgress>,
    pub preview_ticks: u64,
}

/// Whether the X readouts show a diameter.
///
/// The program is the default: `G7` puts a lathe interpreter in diameter mode
/// and `G8` back in radius, and a program written in diameters should read that
/// way. The operator's `Ctrl+E` choice outranks it once it exists, and a mill's
/// X is a position, never a diameter.
pub fn x_reads_as_diameter(lathe: bool, preference: Option<bool>, program: bool) -> bool {
    lathe && preference.unwrap_or(program)
}

/// Whether a machine-space point lies inside the travel box. Bounds are
/// inclusive, as they are for the classifier: a point on `MIN_LIMIT` is still
/// reachable travel.
fn point_inside_travel(point: [f64; 3], limits: &linc_core::gcode::Bounds3) -> bool {
    (0..3).all(|axis| {
        let (min, max) = (limits.min[axis], limits.max[axis]);
        !min.is_finite() || !max.is_finite() || (point[axis] >= min && point[axis] <= max)
    })
}

/// The limit a point crosses, spelled like the limit banner: `X+` past
/// `MAX_LIMIT`, `X-` past `MIN_LIMIT`.
fn outside_label(point: [f64; 3], limits: &linc_core::gcode::Bounds3) -> String {
    for (axis, name) in ["X", "Y", "Z"].iter().enumerate() {
        let (min, max) = (limits.min[axis], limits.max[axis]);
        if point[axis] < min {
            return format!("{name}-");
        }
        if point[axis] > max {
            return format!("{name}+");
        }
    }
    "LIMITS".to_string()
}

#[cfg(test)]
mod tests {
    use super::x_reads_as_diameter;

    #[test]
    fn the_diameter_readout_follows_the_program_until_the_operator_chooses() {
        // G7 in the program, no preference: diameters.
        assert!(x_reads_as_diameter(true, None, true));
        // G8 in the program, no preference: radius.
        assert!(!x_reads_as_diameter(true, None, false));
        // The operator's choice wins either way.
        assert!(!x_reads_as_diameter(true, Some(false), true));
        assert!(x_reads_as_diameter(true, Some(true), false));
        // A mill has no diameter X, whatever the program or the preference say.
        assert!(!x_reads_as_diameter(false, None, true));
        assert!(!x_reads_as_diameter(false, Some(true), true));
    }
}
