//! The MDI pane: MAIN's program card while the panel sits in MDI.
//!
//! There is no MDI page. The panel position opens MAIN and this pane takes the
//! slot the `PROGRAM` card uses, keyed to the mode rather than to page entry.
//! It holds the blocks entered this session and issues them through the same
//! explicit `IssueMdi` path the probe and tool-change actions use.
//!
//! The block in the input is also drawn on MAIN's toolpath, in the pane's own
//! colour, so the operator sees the move before issuing it.

use std::path::{Path, PathBuf};

use egui::{Align, FontId, Rect, RichText, Stroke, Ui, Vec2};
use linc_core::command::{MachineCommand, MdiOrigin};
use linc_core::gcode::{parse_preview, Bounds3, PathSegment, SegmentKind};
use linc_core::mdi_file::{self, SaveError};
use linc_core::model::PanelMode;
use linc_view::{arc_points, Projection, ViewMode};

use crate::screens::common;
use crate::shell::LincApp;
use crate::spacing;
use crate::state::BlockOverlay;
use crate::theme;

/// `theme::card`'s inner margin and border: the pane's own layout works
/// inside both. A frame's rect is its content plus the margin plus the stroke
/// on each side, so the stroke has to be counted or the card lands 2 px past
/// the slot it was given.
const CARD_MARGIN: f32 = 12.0;
const CARD_STROKE: f32 = 1.0;
const CARD_INSET: f32 = CARD_MARGIN + CARD_STROKE;
/// One input line. The pane reserves at least this much, and the slot grows by
/// a line for every line the block wraps to.
const INPUT_HEIGHT: f32 = 30.0;
/// The input's inner margin, set on the widget so the slot the pane reserves is
/// measured with the width the text really gets.
const INPUT_MARGIN_X: f32 = 4.0;
const INPUT_MARGIN_Y: f32 = 2.0;
const STRIP_HEIGHT: f32 = 16.0;
/// Smallest usable listing. The listing is what gives way when the MODAL band
/// is tall, never the band.
const LISTING_MIN: f32 = 46.0;

/// Draw the pane into the slot the program card would have used: the block
/// listing, the highlighted input, the one-line result/error strip, and the
/// MODAL band under it.
pub fn pane(app: &mut LincApp, ui: &mut Ui, height: f32) {
    // The card's rect is taken once, before the frame: the MODAL band is
    // anchored to its bottom and the listing is sized from what is left, all in
    // this frame. AGENTS.md forbids sizing children from `available_height` (a
    // frame does not bound what it gives its content) and forbids feeding a
    // measured height into the next frame (the card then oscillates).
    let card = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), height));
    let codes = modal_codes(app);
    theme::card(ui, |ui| {
        let body = (height - 2.0 * CARD_INSET).max(0.0);
        ui.set_min_height(body);
        ui.set_max_height(body);
        ui.set_min_width(ui.available_width());
        ui.set_max_width(ui.available_width());
        ui.spacing_mut().interact_size.y = 18.0;
        ui.spacing_mut().item_spacing.y = 2.0;
        let top = ui.cursor().min.y;
        ui.horizontal(|ui| {
            theme::section_header(ui, "MDI");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} BLOCKS", app.mdi.len()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("PANEL MDI")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::ACCENT),
                );
            });
        });
        theme::separator(ui);
        let header = ui.cursor().min.y - top;
        let row_height = ui.spacing().interact_size.y.max(16.0);
        // Measured before anything is placed: the input's bottom edge is pinned
        // above the strip, so a block that wraps grows upward and takes the room
        // from the listing instead of reaching the MODAL band.
        let band = band_layout(card, header, codes, input_height(ui, &app.mdi.input));
        listing_rows(app, ui, band.listing, row_height);
        // The block is classified against the envelope here, once: the overlay
        // is what paints the red part, and the input's border and Enter's gate
        // read the same segments.
        overlay(app);
        let beyond = app
            .machine_bounds()
            .and_then(|limits| app.mdi.overlay.as_ref().and_then(|cache| excess(&cache.segments, limits)));
        let response = highlighted_input(app, ui, band.input, beyond);
        if app.profile.config.mdi.auto_focus && app.mdi.focus && !app.overlay_open() {
            // One-shot: the pane asks for the keyboard when it appears or when
            // the panel switches into MDI, and gives it up the moment the
            // operator clicks the toolpath or the graph. A dialog owns the
            // keyboard while it is open, so the request waits for it.
            response.request_focus();
            app.mdi.focus = false;
        }
        strip(app, ui, band.strip);
        // The band is the card AUTO shows: same widget, same column count, so
        // the codes read the same on both pages. It is anchored to the card's
        // rect rather than to the cursor, so a listing that grows or shrinks
        // never moves it.
        let mut modal_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("mdi-modal-band")
                .max_rect(band.modal)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        common::modal_pane_in(app, &mut modal_ui, 8, "mdi-modal", band.modal);
    });
}

/// Where the parts of the pane sit inside its card.
///
/// Built from the card's rect and the header measured this frame, from the
/// bottom up: the MODAL band takes the card's own height off the bottom, the
/// strip sits on top of it, the input's bottom edge is pinned above the strip,
/// and the listing gets what is left. The listing gives way when the band is
/// tall or the block in the input wraps; nothing else moves.
struct Band {
    /// The block listing, above the input.
    listing: Rect,
    /// The input's slot: its bottom is fixed, so a wrapped block grows upward.
    input: Rect,
    /// The one-line result or error, above the band.
    strip: Rect,
    /// The MODAL card's rect, inside the pane's card.
    modal: Rect,
}

fn band_layout(card: Rect, header: f32, codes: usize, input_height: f32) -> Band {
    let left = card.left() + CARD_INSET;
    let width = (card.width() - 2.0 * CARD_INSET).max(60.0);
    let modal_height = common::modal_height(codes, 8);
    let modal = Rect::from_min_size(
        egui::pos2(left, card.bottom() - CARD_INSET - modal_height),
        Vec2::new(width, modal_height),
    );
    // One gap above the band, then the strip, then a gap under the input.
    let strip = Rect::from_min_size(
        egui::pos2(left, modal.top() - 8.0 - STRIP_HEIGHT),
        Vec2::new(width, STRIP_HEIGHT),
    );
    let listing_top = card.top() + CARD_INSET + header;
    let room = (strip.top() - 2.0 - listing_top).max(0.0);
    // The listing gives way down to LISTING_MIN; past that the block is clipped
    // in its own slot rather than pushed into the strip and the band.
    let input_height = input_height.min((room - LISTING_MIN).max(INPUT_HEIGHT));
    let input = Rect::from_min_size(
        egui::pos2(left, strip.top() - 2.0 - input_height),
        Vec2::new(width, input_height),
    );
    let listing = Rect::from_min_max(
        egui::pos2(left, listing_top),
        egui::pos2(left + width, input.top().max(listing_top + 2.0) - 2.0),
    );
    Band {
        listing,
        input,
        strip,
        modal,
    }
}

/// The G groups the MODAL band will hold, for its height: the same count the
/// Program page takes.
fn modal_codes(app: &LincApp) -> usize {
    app.snapshot
        .program
        .modal
        .iter()
        .filter(|(key, _)| key.starts_with('G'))
        .count()
}

/// The committed blocks with line numbers, oldest at the top.
///
/// The list scrolls when the blocks outrun the pane. With nothing selected it
/// stays pinned to the newest block; a selection that moves is brought into
/// view, and the wheel is left alone in between.
fn listing_rows(app: &LincApp, ui: &mut Ui, rect: Rect, row_height: f32) {
    let count = app.mdi.len();
    let follow_id = egui::Id::unique("mdi-listing-follow");
    let last: Option<Option<usize>> = ui.memory(|memory| memory.data.get_temp(follow_id));
    let follow = last != Some(app.mdi.selection);
    if follow {
        ui.memory_mut(|memory| memory.data.insert_temp(follow_id, app.mdi.selection));
    }
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("mdi-listing-slot")
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    slot.set_clip_rect(rect.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt("mdi-listing")
        .auto_shrink([false, false])
        .stick_to_bottom(app.mdi.selection.is_none())
        .show(&mut slot, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for index in 0..count {
                let (row, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Sense::hover(),
                );
                if row.height() <= 0.0 {
                    continue;
                }
                let selected = app.mdi.selection == Some(index);
                if selected {
                    // The band goes down before the text, so the tokens read
                    // over it instead of being tinted by it.
                    common::selection_band(ui, row);
                }
                let mut row_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("mdi_row", index))
                        .max_rect(row)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                row_ui.set_clip_rect(row.intersect(ui.clip_rect()));
                row_ui.spacing_mut().item_spacing.x = 4.0;
                // Lane for the band's rim bar, so the numbers start at the same
                // x on every row.
                let _ = row_ui.allocate_exact_size(
                    egui::vec2(theme::SELECTION_BAR, row_height),
                    egui::Sense::hover(),
                );
                row_ui.label(
                    RichText::new(format!("{:>3}", index + 1))
                        .monospace()
                        .size(theme::font_size(7.5))
                        .color(if selected {
                            theme::ACCENT
                        } else {
                            theme::MUTED
                        }),
                );
                if let Some((block, _)) = app.mdi.blocks.get(index) {
                    let galley = row_ui.fonts_mut(|fonts| fonts.layout_job(highlight(block, 9.5)));
                    let pos = row_ui.cursor().min
                        + egui::vec2(0.0, ((row_height - galley.size().y) * 0.5).max(0.0));
                    row_ui.painter().galley(pos, galley, theme::FG);
                }
                if selected && follow {
                    ui.scroll_to_rect(row, Some(Align::Center));
                }
            }
        });
}

/// The input, syntax-highlighted like the editor and drawn in `rect`.
///
/// `rect` is the slot [`band_layout`] reserved: its bottom edge is pinned above
/// the result strip, so a block that wraps fills the slot upward.
fn highlighted_input(
    app: &mut LincApp,
    ui: &mut Ui,
    rect: Rect,
    beyond: Option<String>,
) -> egui::Response {
    let id = egui::Id::unique("linc-mdi-input");
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("mdi-input-slot")
            .max_rect(rect)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    // A block longer than the room above the strip is clipped in its own slot
    // rather than painted over the strip.
    slot.set_clip_rect(rect.intersect(ui.clip_rect()));
    caret_after_recall(app, &slot, id);
    auto_space(app, &slot, id);
    let mut text = app.mdi.input.clone();
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = highlight(buffer.as_str(), 19.0);
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let response = slot.add(
        egui::TextEdit::singleline(&mut text)
            // Stable id: the shell lists it in `text_focused`, so Home, End
            // and the page keys stay the text cursor's while it has focus.
            .id(id)
            .font(FontId::monospace(theme::font_size(19.0)))
            .margin(egui::Margin::symmetric(
                INPUT_MARGIN_X as i8,
                INPUT_MARGIN_Y as i8,
            ))
            .desired_width(rect.width())
            .layouter(&mut layouter),
    );
    if text != app.mdi.input {
        app.mdi.input = text;
        app.mdi.last_error = None;
    }
    if beyond.is_some() {
        // The block would leave the envelope: the frame carries it, so Enter is
        // refused for something the operator can see before pressing it.
        slot.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(3),
            Stroke::new(1.6, theme::ERROR),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// The height the input needs for `text`: one line, or one per row the block
/// wraps to at the width the widget gives it.
fn input_height(ui: &Ui, text: &str) -> f32 {
    let mut job = highlight(text, 19.0);
    // The widget lays the block out inside its own margin; the two pixels of
    // slack keep a block that lands on the boundary from wrapping to one row
    // more than the slot was sized for.
    job.wrap.max_width = (ui.available_width() - 2.0 * INPUT_MARGIN_X - 2.0).max(1.0);
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    (galley.size().y + 2.0 * INPUT_MARGIN_Y).max(INPUT_HEIGHT)
}

/// Put the text caret after a block the pane recalled into the input.
///
/// Recall replaces the text under the widget, and the widget's own caret stays
/// where the previous block left it — at the start of the new one, so the next
/// key typed would land in front of it. The text itself is not touched: only
/// the cursor the edit will use.
fn caret_after_recall(app: &mut LincApp, ui: &Ui, id: egui::Id) {
    if !app.mdi.caret_to_end {
        return;
    }
    app.mdi.caret_to_end = false;
    let end = app.mdi.input.chars().count();
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(end),
        )));
    state.store(ui.ctx(), id);
}

/// Insert the space a word needs in front of it, before the `TextEdit` draws.
///
/// Layer: event interception. The rule itself lives in [`crate::spacing`] so
/// the program editor shares it; this caller still reads the typed characters,
/// loads and stores the input's [`egui::text_edit::TextEditState`] and writes
/// `app.mdi.input`. The space goes into the buffer and the stored caret — and
/// the selection the keystroke is replacing — is moved here, so the edit
/// inserts the character at the shifted caret and its own cursor arithmetic
/// stays right: the caret ends after the character that was typed.
fn auto_space(app: &mut LincApp, ui: &Ui, id: egui::Id) {
    if !ui.memory(|memory| memory.has_focus(id)) {
        return;
    }
    // Typed characters only: a paste is a whole block the operator chose, and a
    // recalled block keeps the text it was issued with until a key is typed.
    let mut typed: Vec<char> = Vec::new();
    ui.input(|input| {
        for event in &input.events {
            if let egui::Event::Text(text) = event {
                typed.extend(text.chars());
            }
        }
    });
    if typed.is_empty() {
        return;
    }
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let chars: Vec<char> = app.mdi.input.chars().collect();
    let (caret, anchor) = match state.cursor.char_range() {
        Some(range) => {
            let range = range.as_sorted_char_range();
            (range.end.0, range.start.0)
        }
        // No cursor yet: the input was just focused, and egui puts the caret at
        // the end of the text.
        None => (chars.len(), chars.len()),
    };
    let Some((text, caret)) = spacing::space_typed(&chars, caret, anchor, &typed) else {
        return;
    };
    app.mdi.input = text.into_iter().collect();
    app.mdi.last_error = None;
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(caret),
        )));
    state.store(ui.ctx(), id);
}

/// One line: the last issue result, or the error that stopped it.
fn strip(app: &LincApp, ui: &mut Ui, rect: Rect) {
    let (text, color) = match (&app.mdi.last_error, &app.mdi.last_result) {
        (Some(error), _) => (error.clone(), theme::ERROR),
        (None, Some(result)) => (format!("ISSUED {result}"), theme::SUCCESS),
        // Nothing to report: the issue and recall keys are listed in the
        // shortcuts rail, so an idle strip says nothing. The line it occupies
        // stays reserved, or the listing would resize the moment a block is
        // issued.
        (None, None) => return,
    };
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("mdi-strip")
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    slot.set_clip_rect(rect.intersect(ui.clip_rect()));
    slot.add(
        egui::Label::new(
            RichText::new(text)
                .monospace()
                .size(theme::font_size(9.0))
                .color(color),
        )
        .truncate(),
    );
}

/// One G-code block as a highlighted galley.
///
/// The layout is the program view's, gaps included, so a row and the input
/// read exactly the block that will be issued — joining the tokens alone
/// dropped the spaces between them.
fn highlight(text: &str, size: f32) -> egui::text::LayoutJob {
    let mut job = crate::screens::program::line_job(text, size);
    job.wrap.max_width = f32::INFINITY;
    job
}

/// Why the issue path refuses `block`, or `None` when it may send it. The
/// preconditions are the ones the page always applied: length, panel mode,
/// machine ON / homed / idle, and a block that parses.
fn issue_blocked(app: &LincApp, block: &str) -> Option<String> {
    if block.is_empty() {
        return Some("MDI command is empty".into());
    }
    if block.len() > app.profile.config.mdi.max_command_length {
        return Some(format!(
            "command exceeds {} characters",
            app.profile.config.mdi.max_command_length
        ));
    }
    if app.snapshot.panel.mode.mode() != Some(PanelMode::Mdi) {
        return Some("panel mode is not MDI".into());
    }
    if let Err(error) = app.snapshot.issue_allowed() {
        return Some(error);
    }
    let preview = parse_preview(block, &app.axes(), None, [0.0; 3]);
    if let Some(error) = preview.error {
        return Some(format!("line {}: {}", error.line, error.message));
    }
    if let Some(reason) = travel_excess(app, block) {
        return Some(reason);
    }
    None
}

/// Why `block` may not be issued because of the machine envelope, or `None`.
///
/// The same extent the overlay paints red: the block's own moves, started from
/// where the tool is now.
fn travel_excess(app: &LincApp, block: &str) -> Option<String> {
    let limits = app.machine_bounds()?;
    let segments = block_segments(app, block, app.tool_tip());
    excess(&segments, limits)
}

/// `Some(reason)` when any point of `segments` lies outside `limits`.
fn excess(segments: &[PathSegment], limits: Bounds3) -> Option<String> {
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for segment in segments {
        // Arc chords, because that is what the overlay classifies and paints.
        let points: Vec<[f64; 3]> = match &segment.arc {
            Some(arc) => arc_points(segment, arc),
            None => vec![segment.start, segment.end],
        };
        for point in points {
            any = true;
            for axis in 0..3 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
    }
    if !any {
        return None;
    }
    let axis = (0..3).find(|&axis| low[axis] < limits.min[axis] || high[axis] > limits.max[axis])?;
    let limit = if low[axis] < limits.min[axis] {
        limits.min[axis]
    } else {
        limits.max[axis]
    };
    Some(format!(
        "runs past the {} travel limit {limit:.3}",
        ["X", "Y", "Z"][axis]
    ))
}

fn send(app: &mut LincApp, block: &str) {
    app.send(MachineCommand::IssueMdi {
        block: block.to_string(),
        origin: MdiOrigin::Mdi,
    });
    app.mdi.last_result = Some(block.to_string());
    app.mdi.last_error = None;
}

/// `Enter`: issue the typed block and commit it. A block recalled with
/// `Up`/`Down` overwrites its buffer line in place; anything else appends.
pub fn issue(app: &mut LincApp) {
    let block = app.mdi.input.trim().to_string();
    if let Some(reason) = issue_blocked(app, &block) {
        app.mdi.last_error = Some(reason);
        return;
    }
    send(app, &block);
    app.mdi.commit(app.profile.config.mdi.history_limit, block);
    app.remember_mdi_history();
}

/// `Ctrl+R`: issue the selected buffer line directly, with no dialog.
///
/// The gates are the ones run-from-line enforces, minus the AUTO-mode one:
/// the pane runs a buffer block in MDI, not a line of a loaded program. A
/// blocked run says so in the strip instead of opening a prompt.
pub fn run_selected(app: &mut LincApp) {
    let Some(block) = app.mdi.selected().map(str::to_string) else {
        app.mdi.last_error = Some("no MDI buffer line is selected".into());
        return;
    };
    if let Some(reason) = app.mdi_run_block() {
        app.mdi.last_error = Some(reason.to_string());
        return;
    }
    if let Some(reason) = issue_blocked(app, &block) {
        app.mdi.last_error = Some(reason);
        return;
    }
    send(app, &block);
}

/// `Ctrl+S`: write the buffer under `dir`.
///
/// The name rules, the composition and the write live in
/// `linc_core::mdi_file`, so the dialog only chooses the name and whether the
/// profile's pre/postamble brackets the blocks.
pub fn save_buffer(
    app: &LincApp,
    dir: &Path,
    name: &str,
    wrap: bool,
    overwrite: bool,
) -> Result<PathBuf, SaveError> {
    let body = mdi_file::compose(
        &app.mdi.block_texts(),
        &app.profile.config.mdi.save_preamble,
        &app.profile.config.mdi.save_postamble,
        wrap,
    );
    mdi_file::write_program(dir, name, &body, overwrite)
}

/// `Ctrl+S` against the machine's `PROGRAM_PREFIX`.
pub fn save(app: &LincApp, name: &str, wrap: bool, overwrite: bool) -> Result<PathBuf, SaveError> {
    save_buffer(
        app,
        &app.profile.machine_ini.program_prefix,
        name,
        wrap,
        overwrite,
    )
}

/// Refresh the cached block preview and hand it back.
///
/// The toolpath repaints at the display rate, so neither the parse nor the
/// projection may run per frame: the block is re-parsed only when one of its
/// inputs changed — the text, the machine position it starts from, or the modal
/// state and work offset it is drawn against.
pub fn overlay(app: &mut LincApp) -> &[PathSegment] {
    let input = app.mdi.input.trim().to_string();
    let start = app.tool_tip();
    let modal = modal_fingerprint(app);
    let stale = match app.mdi.overlay.as_ref() {
        Some(cache) => {
            cache.input != input
                || cache.modal != modal
                || cache
                    .start
                    .iter()
                    .zip(start.iter())
                    .any(|(cached, live)| (cached - live).abs() > 1e-9)
        }
        None => true,
    };
    if stale {
        let generation = app
            .mdi
            .overlay
            .as_ref()
            .map_or(1, |cache| cache.generation + 1);
        let segments = block_segments(app, &input, start);
        app.mdi.overlay = Some(BlockOverlay {
            input,
            start,
            modal,
            segments,
            generation,
        });
    }
    app.mdi
        .overlay
        .as_ref()
        .map(|cache| cache.segments.as_slice())
        .unwrap_or(&[])
}

/// The moves one block would make from `start`, or none.
///
/// An empty block, an over-long one and one that does not parse all yield
/// nothing: the strip carries the error and the pane would refuse to issue it,
/// so the graph must never draw a move the machine will not make.
fn block_segments(app: &LincApp, block: &str, start: [f64; 3]) -> Vec<PathSegment> {
    if block.is_empty() || block.len() > app.profile.config.mdi.max_command_length {
        return Vec::new();
    }
    // The same work offset the program preview is drawn with, so a `G54` block
    // lands where the program's own `G54` move lands.
    let offset = app.program.preview_offset.clone();
    // The block inherits the machine's modal groups. `parse_preview` starts every
    // parse absolute, in G21, XY, so without these words a `G0 X10` typed while
    // the machine sits in G91 would be drawn as a move to X10 rather than a
    // 10 mm move.
    let text = format!("{}{block}", modal_seed(app));
    let preview = parse_preview(&text, &app.axes(), offset.as_ref(), start);
    if preview.error.is_some() {
        return Vec::new();
    }
    preview.segments
}

/// Fingerprint of what decides the parse besides the block and its start: the
/// modal groups and the work offset it is drawn against.
fn modal_fingerprint(app: &LincApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (key, value) in &app.snapshot.program.modal {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    app.snapshot.offsets.active_index.hash(&mut hasher);
    for axis in ["X", "Y", "Z"] {
        let value = app
            .program
            .preview_offset
            .as_ref()
            .and_then(|offset| offset.get(axis).copied())
            .unwrap_or(0.0);
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Draw the block's move over the program on MAIN's toolpath.
///
/// Called after the card has drawn, so the block sits on top: `rect`,
/// `view`, `zoom`, `pan`, `yaw` and `pitch` are the ones the card
/// just drew with. Nothing is drawn when the pane is down or the block has no
/// moves.
pub fn draw_overlay(
    app: &mut LincApp,
    ui: &Ui,
    rect: Rect,
    view: ViewMode,
    zoom: f32,
    pan: Vec2,
    yaw: f32,
    pitch: f32,
) {
    if !app.mdi_pane_visible() {
        return;
    }
    overlay(app);
    let Some(cache) = app.mdi.overlay.as_ref() else {
        return;
    };
    if cache.segments.is_empty() {
        return;
    }
    // The card's own mirror, so a lathe block lands where the program does.
    let camera = card_projection(reference_bounds(app), rect, view, zoom, pan, yaw, pitch)
        .mirrored(app.graph_style.lathe_mirror);
    let painter = ui.painter_at(rect);
    let stroke = Stroke::new(2.4, theme::MDI_PREVIEW);
    // The block's chords go through the program's own travel split, so the part
    // that would run past an INI limit is red here too, not just on the program.
    let limits = app.machine_bounds();
    let draw = |start: [f64; 3], end: [f64; 3], dotted: bool| {
        linc_view::draw_travel_line(&painter, &camera, start, end, limits, stroke, dotted);
    };
    for segment in &cache.segments {
        // Rapid and cut stay distinguishable the way the renderer keeps them
        // apart: the rapid is the dashed one.
        let dotted = segment.kind == SegmentKind::Rapid;
        match &segment.arc {
            Some(arc) => {
                let points = arc_points(segment, arc);
                for pair in points.windows(2) {
                    draw(pair[0], pair[1], dotted);
                }
            }
            None => draw(segment.start, segment.end, dotted),
        }
    }
}

/// The modal words a block inherits, ready to be read in front of it.
///
/// The distance mode is the one that moves the picture: drawn in the wrong one,
/// a relative block lands on the far side of the part. Units and plane come
/// along because they are the same kind of inherited state.
fn modal_seed(app: &LincApp) -> String {
    const GROUPS: [&[&str]; 3] = [&["G90", "G91"], &["G20", "G21"], &["G17", "G18", "G19"]];
    let mut seed = String::new();
    for group in GROUPS {
        let active = app
            .snapshot
            .program
            .modal
            .iter()
            .map(|(code, _)| code.trim().to_ascii_uppercase())
            .find(|code| group.contains(&code.as_str()));
        if let Some(code) = active {
            seed.push_str(&code);
            seed.push(' ');
        }
    }
    seed
}

/// The box the toolpath card's projection is anchored on.
///
/// Mirrors what `draw_trajectory` collects: the loaded program, the live trail,
/// the tool and the work origin, widened to the machine envelope when the INI
/// has one. The block is added to none of it: the overlay must not rescale the
/// view the program is framed in.
fn reference_bounds(app: &LincApp) -> Bounds3 {
    let program = app.program.last_valid_preview.bounds;
    let mut bounds = program.unwrap_or(default_bounds());
    let mut any = program.is_some();
    for point in app.snapshot.live_trail.as_slice() {
        bounds.include(point.position);
        any = true;
    }
    bounds.include(app.tool_tip());
    bounds.include(app.view.origin);
    if !any {
        return app.machine_bounds().unwrap_or(default_bounds());
    }
    app.machine_bounds().unwrap_or(bounds)
}

fn default_bounds() -> Bounds3 {
    Bounds3 {
        min: [-10.0; 3],
        max: [10.0; 3],
    }
}

/// The card's camera for the block overlay.
///
/// The same box, inset and orbit `draw_trajectory` builds from the card's
/// rect, so the block is placed on the program's picture by the program's own
/// projection rather than by a copy of it.
fn card_projection(
    bounds: Bounds3,
    rect: Rect,
    view: ViewMode,
    zoom: f32,
    pan: Vec2,
    yaw: f32,
    pitch: f32,
) -> Projection {
    Projection::new(bounds, rect.shrink(12.0), view, yaw, pitch, zoom, pan)
}

#[cfg(test)]
mod tests {
    use super::{band_layout, card_projection, highlight, CARD_INSET, INPUT_HEIGHT, LISTING_MIN};
    use egui::{pos2, vec2, Pos2, Rect, Vec2};
    use linc_core::gcode::{Bounds3, PathSegment, Preview, SegmentKind};
    use linc_core::model::MachineVariant;
    use linc_view::{draw_trajectory, theme as view_theme, Projection, TrajectoryView, ViewMode};

    /// The pane must lay out a block character for character: the tokens and
    /// the spaces between them. The tokens alone read `G1Z-2F120` for a block
    /// that is issued as `G1 Z-2 F120`.
    #[test]
    fn a_laid_out_block_reads_exactly_as_the_block() {
        for block in [
            "G1 Z-2 F120",
            "G0 X1 (rapid)",
            "  G0   X1  ",
            "M3 S1200",
            "",
        ] {
            let job = highlight(block, 9.5);
            assert_eq!(job.text, block, "{block:?}");
            // The sections must cover the text with no gaps or overlaps, or
            // the caret in the input lands on the wrong glyph.
            let covered = job
                .sections
                .last()
                .map_or(0, |section| section.byte_range.end.0);
            assert_eq!(covered, block.len(), "{block:?}");
        }
    }

    #[test]
    fn the_modal_band_sits_inside_the_card_below_the_strip() {
        let card = Rect::from_min_size(pos2(920.0, 90.0), vec2(352.0, 560.0));
        let header = 26.0;
        for codes in [0, 3, 8, 12, 20] {
            let band = band_layout(card, header, codes, INPUT_HEIGHT);
            let expected = crate::screens::common::modal_height(codes, 8);
            assert!((band.modal.height() - expected).abs() < 0.01, "{codes}");
            assert!((band.modal.bottom() - (card.bottom() - CARD_INSET)).abs() < 0.01);
            assert!(band.modal.min.x >= card.left() + CARD_INSET - 0.01);
            assert!(band.modal.max.x <= card.right() - CARD_INSET + 0.01);
            // The band never reaches into the header or what is left of the
            // listing: it only ever takes space from the listing's slack.
            assert!(
                band.modal.top() >= card.top() + CARD_INSET + header + LISTING_MIN,
                "{codes} codes push the band over the listing"
            );
            // The three slots stack between the header and the band, in order,
            // and none of them overlaps the next.
            assert!(band.strip.bottom() <= band.modal.top(), "{codes}");
            assert!(band.input.bottom() <= band.strip.top(), "{codes}");
            assert!(band.listing.bottom() <= band.input.top(), "{codes}");
            assert!(
                band.listing.height() >= LISTING_MIN - 0.01,
                "{codes} codes leave no listing: {:?}",
                band.listing
            );
        }
        // A short slot still keeps the band inside the card.
        let slot = Rect::from_min_size(pos2(920.0, 90.0), vec2(352.0, 240.0));
        let band = band_layout(slot, header, 12, INPUT_HEIGHT);
        assert!(band.modal.min.y >= slot.top());
        assert!(band.modal.max.y <= slot.bottom());
    }

    #[test]
    fn a_wrapping_block_takes_its_room_from_the_listing() {
        let card = Rect::from_min_size(pos2(920.0, 90.0), vec2(352.0, 560.0));
        let one = band_layout(card, 26.0, 3, INPUT_HEIGHT);
        let three = band_layout(card, 26.0, 3, INPUT_HEIGHT + 42.0);
        // The bottom edge stays where it was, so three lines grow upward into
        // the listing instead of reaching the strip and the band.
        assert!((one.input.bottom() - three.input.bottom()).abs() < 0.01);
        assert!((one.input.top() - three.input.top() - 42.0).abs() < 0.01);
        assert!((one.listing.height() - three.listing.height() - 42.0).abs() < 0.01);
        assert!(three.input.bottom() <= three.strip.top());

        // A block longer than the room keeps the listing's minimum and is
        // clipped in its own slot.
        let huge = band_layout(card, 26.0, 3, 5000.0);
        assert!(huge.listing.height() > LISTING_MIN - 3.0, "{:?}", huge.listing);
        assert!(huge.input.bottom() <= huge.strip.top());
    }

    #[test]
    fn the_overlay_camera_lands_where_the_toolpath_puts_the_part() {
        let bounds = Bounds3 {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 50.0, 20.0],
        };
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0));
        let camera = Projection::new(bounds, rect, ViewMode::TopXy, 0.0, 0.0, 1.0, vec2(0.0, 0.0));
        let centre = camera.project([50.0, 25.0, 0.0]);
        assert!((centre.x - rect.center().x).abs() < 0.5);
        assert!((centre.y - rect.center().y).abs() < 0.5);
        // +X runs right, +Y runs up the screen, and one unit is the same length
        // on both: the flat view is a plain linear map.
        let right = camera.project([60.0, 25.0, 0.0]);
        let up = camera.project([50.0, 35.0, 0.0]);
        assert!(right.x > centre.x);
        assert!(up.y < centre.y);
        assert!(((right.x - centre.x) - (centre.y - up.y)).abs() < 0.01);
        // Zoom and pan move the picture the way the card moves it.
        let zoomed = Projection::new(bounds, rect, ViewMode::TopXy, 0.0, 0.0, 2.0, vec2(0.0, 0.0));
        let pushed = zoomed.project([60.0, 25.0, 0.0]);
        assert!((pushed.x - centre.x - 2.0 * (right.x - centre.x)).abs() < 0.01);
        let panned = Projection::new(
            bounds,
            rect,
            ViewMode::TopXy,
            0.0,
            0.0,
            1.0,
            vec2(5.0, -3.0),
        );
        let moved = panned.project([50.0, 25.0, 0.0]);
        assert!((moved.x - centre.x - 5.0).abs() < 0.01);
        assert!((moved.y - centre.y + 3.0).abs() < 0.01);
    }

    /// One frame of the card, with the cut it painted.
    fn card_cut(
        preview: &Preview,
        bounds: Bounds3,
        mode: ViewMode,
        yaw: f32,
        pitch: f32,
    ) -> (Rect, Vec<[Pos2; 2]>) {
        let mut view = TrajectoryView::new(preview, &[], MachineVariant::Mill);
        view.view = mode;
        view.yaw = yaw;
        view.pitch = pitch;
        view.zoom = 1.0;
        view.pan = Vec2::ZERO;
        view.machine_bounds = Some(bounds);
        view.height = 400.0;
        view.show_grid = false;
        view.show_legend = false;
        view.show_program_bounds = false;
        view.show_stock = false;
        view.show_selected_line = false;
        view.show_live_trail = false;
        view.fit = false;
        view.auto_fit = false;
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        let mut rect = Rect::ZERO;
        let mut output = ctx.run_ui(input, |ui| {
            rect = draw_trajectory(ui, &mut view).rect;
        });
        // Nothing here paints the textures the fonts ask for.
        output.textures_delta.clear();
        let mut cut = Vec::new();
        for clipped in &output.shapes {
            if let egui::Shape::LineSegment { points, stroke } = &clipped.shape {
                // The program cut is the card's only accent-coloured 1.8 px
                // line: grid, axes, bounds and the legend use their own.
                if stroke.color == view_theme::ACCENT && (stroke.width - 1.8).abs() < 1e-6 {
                    cut.push(*points);
                }
            }
        }
        (rect, cut)
    }

    /// The card opens in 3D/ISO, and that is the view the old test left
    /// unpinned: it held the camera to the flat views, where the mapping is
    /// linear. Here the overlay's camera is checked against the line the card
    /// itself paints for the same move, with the view orbited off the default.
    #[test]
    fn the_overlay_lands_on_the_card_in_an_orbited_view() {
        let bounds = Bounds3 {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 50.0, 20.0],
        };
        let segment = PathSegment {
            start: [10.0, 10.0, 5.0],
            end: [90.0, 40.0, 15.0],
            kind: SegmentKind::Feed,
            line: 1,
            arc: None,
            tool: None,
        };
        let preview = Preview {
            segments: vec![segment.clone()],
            bounds: Some(bounds),
            ..Preview::empty()
        };
        let (yaw, pitch) = (0.9_f32, 0.42_f32);
        let (rect, cut) = card_cut(&preview, bounds, ViewMode::Iso3d, yaw, pitch);
        assert_eq!(cut.len(), 1, "the card draws the move once");
        let camera = card_projection(bounds, rect, ViewMode::Iso3d, 1.0, Vec2::ZERO, yaw, pitch);
        for (drawn, model) in [(cut[0][0], segment.start), (cut[0][1], segment.end)] {
            let expected = camera.project(model);
            assert!(
                (drawn - expected).length() < 0.01,
                "overlay {expected:?} off the card's {drawn:?}"
            );
        }
    }
}
