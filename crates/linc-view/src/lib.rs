//! 2D/3D toolpath and sparkline rendering for LiNC.
//!
//! Projection is CPU-side into egui's painter. No GPU buffers, no animation
//! loop and no machine access.

const FONT_SCALE: f32 = 1.25;

pub(crate) mod hershey;
pub mod travel;

use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use linc_core::gcode::{ArcSpec, Bounds3, PathSegment, Plane, Preview, SegmentKind};
use linc_core::model::{MachineVariant, TrailPoint};

pub mod theme {
    use egui::Color32;

    pub const SURFACE: Color32 = Color32::from_rgb(0x14, 0x16, 0x19);
    pub const FG: Color32 = Color32::from_rgb(0xe8, 0xea, 0xed);
    pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xa6);
    pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x35, 0x3a);
    pub const ACCENT: Color32 = Color32::from_rgb(0x4d, 0xa3, 0xff);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x3d, 0xdc, 0x84);
    pub const WARNING: Color32 = Color32::from_rgb(0xff, 0xb0, 0x20);
    pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x5c, 0x5c);
    pub const CYAN: Color32 = Color32::from_rgb(0x67, 0xe8, 0xf9);
    /// Stock envelope: a neutral grey block that stays behind the toolpath.
    /// Live toolhead marker.
    pub const TOOL: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
    /// The axis triad at the work origin: the same yellow as the DRO cards use
    /// for their X/Y/Z labels (`theme::C3` in the UI), so the datum and the
    /// readout read as one system.
    pub const AXIS: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
    /// The segment of the line highlighted in the G-code pane. Yellow, so it is
    /// distinct from the cut path's blue and the live trail's green.
    pub const CURRENT: Color32 = Color32::from_rgb(0xf7, 0xd7, 0x4a);
    pub fn dim(color: Color32, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    }

    pub const STOCK_FILL: Color32 = Color32::from_rgba_premultiplied(0x4a, 0x50, 0x57, 0x14);
    pub const STOCK_EDGE: Color32 = Color32::from_rgb(0x5a, 0x62, 0x6b);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Iso3d,
    /// Looking along X at the YZ plane.
    SideYz,
    /// Looking along Y at the XZ plane.
    SideXz,
    /// Looking down Z at the XY plane.
    TopXy,
    LatheXz,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Iso3d => "3D / ISO",
            Self::SideYz => "YZ / SIDE",
            Self::SideXz => "XZ / FRONT",
            Self::TopXy => "XY / TOP",
            Self::LatheXz => "XZ / TURN",
        }
    }
}

/// The next view in the `Ctrl+F10` cycle: ISO, X, Y, Z and back.
///
/// The lathe has one flat view, so it toggles between it and ISO.
pub fn cycle_view(mode: ViewMode, variant: MachineVariant) -> ViewMode {
    if variant == MachineVariant::Lathe {
        return match mode {
            ViewMode::Iso3d => ViewMode::LatheXz,
            _ => ViewMode::Iso3d,
        };
    }
    match mode {
        ViewMode::Iso3d => ViewMode::SideYz,
        ViewMode::SideYz => ViewMode::SideXz,
        ViewMode::SideXz => ViewMode::TopXy,
        _ => ViewMode::Iso3d,
    }
}

/// Blender's default-ish orbit: 45° around, ~35° above the XY plane, the classic
/// isometric reading of a mill table.
pub const DEFAULT_YAW: f32 = std::f32::consts::FRAC_PI_4;
pub const DEFAULT_PITCH: f32 = 0.615;

#[derive(Clone, Debug)]
pub struct TrajectoryView<'a> {
    pub preview: &'a Preview,
    /// The live trail with the kind of move that reached each point, so a
    /// rapid is dotted here exactly as it is in the preview.
    pub live_trail: &'a [TrailPoint],
    pub tool_tip: Option<[f64; 3]>,
    pub machine_bounds: Option<Bounds3>,
    /// Where the work-zero cross is drawn, in the frame the view uses. The
    /// machine origin stays at zero; touching off moves this marker instead of
    /// moving the part and the tool.
    pub origin: [f64; 3],
    /// A second origin marker, drawn in the current-line colour: the OFFSETS
    /// page shows the row being edited here while the active system keeps its
    /// own cross.
    pub highlight_origin: Option<[f64; 3]>,
    pub highlight_label: Option<String>,
    /// Offset the preview was drawn with, for the corner work coordinates.
    pub work_offset: Option<[f64; 3]>,
    pub selected_line: Option<usize>,
    pub current_line: Option<usize>,
    pub variant: MachineVariant,
    pub view: ViewMode,
    pub zoom: f32,
    pub pan: Vec2,
    /// Orbit of the 3D view: rotation about the vertical axis and elevation.
    /// Ignored by the flat views, which have a fixed direction.
    pub yaw: f32,
    pub pitch: f32,
    pub show_grid: bool,
    pub show_live_trail: bool,
    /// Draw the technical drawing on the program bounds: dimension lines and
    /// their values.
    pub show_program_bounds: bool,
    /// Draw the program box outline itself.
    pub show_program_box: bool,
    pub show_machine_bounds: bool,
    /// Draw the stock envelope. SPEC §6.1 lists stock alongside the program
    /// path, live trail and bounds in the mill toolpath.
    pub show_stock: bool,
    /// Stock fill opacity, 0-255. Zero draws no stock at all.
    pub stock_alpha: u8,
    pub show_selected_line: bool,
    pub show_legend: bool,
    /// Frame everything that is drawn, not just the program: the live trail,
    /// the tool tip and the work-zero markers. The Offsets preview uses this so
    /// an offset far from the part is still on screen; the Machine page keeps
    /// the program-only fit.
    pub fit_all: bool,
    /// Draw the machine-origin triad in a muted colour and a line from it to the
    /// active work origin. The Offsets page is where the distance between the
    /// two matters; the Machine page shows only the work origin.
    pub origin_tether: bool,
    /// Show the machine envelope regardless of the Ctrl+B bounds cycle. The
    /// Offsets preview wants the travel as context around the offset.
    pub machine_bounds_always: bool,
    /// Lathe only: mirror X about the Z centreline of the turning view. It
    /// moves the projection, not the data: the preview, the live trail and the
    /// tool marker stay the same coordinates.
    pub mirror_x: bool,
    pub selected_line_color: Color32,
    pub height: f32,
    pub fit: bool,
    /// Keep the view framed on the program until the operator pans or zooms. A
    /// one-shot fit is computed for one rect; when the card's rect is not final
    /// yet (first frame, resize) that pan puts the part outside the real card.
    pub auto_fit: bool,
    pub axis_labels: Vec<String>,
}

impl<'a> TrajectoryView<'a> {
    pub fn new(
        preview: &'a Preview,
        live_trail: &'a [TrailPoint],
        variant: MachineVariant,
    ) -> Self {
        Self {
            origin: [0.0; 3],
            highlight_origin: None,
            highlight_label: None,
            work_offset: None,
            preview,
            live_trail,
            tool_tip: None,
            machine_bounds: None,
            selected_line: None,
            current_line: None,
            variant,
            view: if variant == MachineVariant::Lathe {
                ViewMode::LatheXz
            } else {
                ViewMode::Iso3d
            },
            zoom: 1.0,
            pan: Vec2::ZERO,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            show_grid: true,
            show_live_trail: true,
            show_program_bounds: true,
            show_program_box: false,
            show_machine_bounds: false,
            show_stock: true,
            stock_alpha: 0,
            show_selected_line: true,
            show_legend: true,
            fit_all: false,
            origin_tether: false,
            machine_bounds_always: false,
            mirror_x: false,
            selected_line_color: theme::CURRENT,
            height: 400.0,
            fit: false,
            auto_fit: false,
            axis_labels: vec!["X".into(), "Y".into(), "Z".into()],
        }
    }
}

/// The card's orthographic camera: the fit, the orbit and the pan in one
/// place.
///
/// Public because the MDI overlay draws the block being edited on the same
/// picture: a second copy of this fitting is what lets the overlay drift away
/// from the program after a change to the orbit or the scale.
pub struct Projection {
    min: [f64; 3],
    max: [f64; 3],
    rect: Rect,
    scale: f32,
    zoom: f32,
    pan: Vec2,
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
    /// Lathe only: draw X on the other side of the Z centreline.
    mirror_x: bool,
}

/// The screen-plane coordinates of a point in the view's frame.
///
/// The 3D view is an orthographic camera: `yaw` turns it about the vertical
/// axis (0 looks from the front, -Y, towards +Y), `pitch` raises it above the
/// XY plane. Screen y grows downwards, so the vertical component is negated —
/// the opposite sign drew +Z downwards.
fn view_xy(local: [f32; 3], mode: ViewMode, yaw: f32, pitch: f32, mirror_x: bool) -> (f32, f32) {
    match mode {
        ViewMode::Iso3d => {
            let (sin_yaw, cos_yaw) = yaw.sin_cos();
            let (sin_pitch, cos_pitch) = pitch.sin_cos();
            // The camera stands on the operator's side of the machine: yaw 0
            // looks from -Y, so +Y runs away from the viewer into the back and
            // a larger yaw orbits towards +X. The horizontal axis of the screen
            // is therefore `+X + Y` and the direction towards the camera is
            // `+X - Y`; getting the second sign wrong (the old `+X + Y`) put
            // the camera behind the machine, with +Y coming towards the viewer.
            let right = local[0] * cos_yaw + local[1] * sin_yaw;
            let depth = local[0] * sin_yaw - local[1] * cos_yaw;
            let up = local[2] * cos_pitch - depth * sin_pitch;
            (right, -up)
        }
        ViewMode::SideYz => (local[1], -local[2]),
        ViewMode::SideXz => (local[0], -local[2]),
        ViewMode::TopXy => (local[0], -local[1]),
        // The turning view looks down +Y at the XZ plane: Z runs across the
        // screen and X up it. The mirror keeps Z and flips the sign of X, so the
        // part reads on the other side of the Z centreline with the same
        // numbers under it. Only this view has a centreline to mirror about.
        ViewMode::LatheXz => {
            if mirror_x {
                (local[2], local[0])
            } else {
                (local[2], -local[0])
            }
        }
    }
}

impl Projection {
    pub fn new(
        bounds: Bounds3,
        rect: Rect,
        mode: ViewMode,
        yaw: f32,
        pitch: f32,
        zoom: f32,
        pan: Vec2,
    ) -> Self {
        // No fixed clearance here: one machine unit is nothing on a
        // millimetre envelope and most of a small part, so Ctrl+0 on the
        // inch-scale lathe framed the padding and left a third of the card
        // empty on each side. The 0.82 keeps the margin below.
        let expanded = bounds;
        let center = [
            (expanded.min[0] + expanded.max[0]) * 0.5,
            (expanded.min[1] + expanded.max[1]) * 0.5,
            (expanded.min[2] + expanded.max[2]) * 0.5,
        ];
        // Fit the box that is actually on screen: project its eight corners with
        // the current orbit, so a rotated view still frames the whole machine.
        let mut min = [f32::INFINITY, f32::INFINITY];
        let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
        for corner in 0..8 {
            let point = [
                if corner & 1 == 0 {
                    expanded.min[0]
                } else {
                    expanded.max[0]
                },
                if corner & 2 == 0 {
                    expanded.min[1]
                } else {
                    expanded.max[1]
                },
                if corner & 4 == 0 {
                    expanded.min[2]
                } else {
                    expanded.max[2]
                },
            ];
            let local = [
                (point[0] - center[0]) as f32,
                (point[1] - center[1]) as f32,
                (point[2] - center[2]) as f32,
            ];
            // The mirror only negates one screen coordinate about the centre,
            // so the fitted extent — and therefore the scale the fit produces —
            // is the same either way.
            let (x, y) = view_xy(local, mode, yaw, pitch, false);
            min = [min[0].min(x), min[1].min(y)];
            max = [max[0].max(x), max[1].max(y)];
        }
        let world_w = (max[0] - min[0]).max(1.0);
        let world_h = (max[1] - min[1]).max(1.0);
        let scale = ((rect.width() / world_w).min(rect.height() / world_h) * 0.82).max(0.01);
        Self {
            min: expanded.min,
            max: expanded.max,
            rect,
            scale,
            zoom: zoom.clamp(0.05, 40.0),
            pan,
            mode,
            yaw,
            pitch,
            mirror_x: false,
        }
    }

    /// Draw the lathe's X on the other side of the Z centreline.
    ///
    /// A camera property rather than a flag on the data: the program, the live
    /// trail, the tool marker and anything else drawing through this projection
    /// flip together, and no coordinate in the model changes. The views without
    /// a centreline ignore it.
    pub fn mirrored(mut self, mirror_x: bool) -> Self {
        self.mirror_x = mirror_x;
        self
    }

    pub fn project(&self, point: [f64; 3]) -> Pos2 {
        let center = [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ];
        let local = [
            (point[0] - center[0]) as f32,
            (point[1] - center[1]) as f32,
            (point[2] - center[2]) as f32,
        ];
        let (x, y) = view_xy(local, self.mode, self.yaw, self.pitch, self.mirror_x);
        let center_screen = self.rect.center() + self.pan;
        Pos2::new(
            center_screen.x + x * self.scale * self.zoom,
            center_screen.y + y * self.scale * self.zoom,
        )
    }

    fn draw_line(painter: &Painter, start: Pos2, end: Pos2, stroke: Stroke, dotted: bool) {
        if dotted {
            draw_dotted(painter, start, end, stroke);
        } else {
            painter.line_segment([start, end], stroke);
        }
    }
}

/// Rapids are dotted: short dots read as a rapid at any zoom, where a dash
/// pattern can look like a solid line on a long move.
fn draw_dotted(painter: &Painter, start: Pos2, end: Pos2, stroke: Stroke) {
    draw_dashed(painter, start, end, stroke, 1.6, 3.4);
}

/// One dotted rhythm across a whole polyline, so the gap phase carries over
/// from each sampled hop to the next.
fn draw_dotted_path(painter: &Painter, points: &[Pos2], stroke: Stroke) {
    draw_dashed_path(painter, points, stroke, 1.6, 3.4);
}

fn draw_dashed_path(painter: &Painter, points: &[Pos2], stroke: Stroke, dash: f32, gap: f32) {
    // Dash/gap state carried across hops. A modulo phase is tempting but f32
    // rounding turns a boundary dash into a ~1e-7 px step that cannot advance,
    // which stalls the walk and drops every dot after it.
    let mut drawing = true;
    let mut remaining = dash;
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let delta = end - start;
        let length = delta.length();
        if length < 0.5 {
            continue;
        }
        let direction = delta / length;
        let mut walked = 0.0f32;
        while walked < length {
            let run = remaining.min(length - walked);
            if drawing && run > 0.0 {
                painter.line_segment(
                    [
                        start + direction * walked,
                        start + direction * (walked + run),
                    ],
                    stroke,
                );
            }
            walked += run;
            remaining -= run;
            if remaining <= 0.001 {
                drawing = !drawing;
                remaining = if drawing { dash } else { gap };
            }
        }
    }
}

fn draw_dashed(painter: &Painter, start: Pos2, end: Pos2, stroke: Stroke, dash: f32, gap: f32) {
    let delta = end - start;
    let length = delta.length();
    if length < 0.5 {
        return;
    }
    let direction = delta / length;
    let mut position = 0.0;
    while position < length {
        let segment_end = (position + dash).min(length);
        painter.line_segment(
            [
                start + direction * position,
                start + direction * segment_end,
            ],
            stroke,
        );
        let next = position + dash + gap;
        if next <= position {
            break;
        }
        position = next;
    }
}

/// Draw one straight move, red where it lies outside the machine travel box.
///
/// The classifier works in machine coordinates, so the split point is exact in
/// the model: the inside uses the caller's stroke (cut, rapid or selected) and
/// only the part past a limit turns `theme::ERROR`.
pub fn draw_travel_line(
    painter: &Painter,
    projection: &Projection,
    start: [f64; 3],
    end: [f64; 3],
    limits: Option<Bounds3>,
    stroke: Stroke,
    dotted: bool,
) {
    let Some(limits) = limits else {
        Projection::draw_line(
            painter,
            projection.project(start),
            projection.project(end),
            stroke,
            dotted,
        );
        return;
    };
    let outside = Stroke::new(stroke.width, theme::ERROR);
    let draw = |a: [f64; 3], b: [f64; 3], stroke: Stroke, dotted: bool| {
        Projection::draw_line(
            painter,
            projection.project(a),
            projection.project(b),
            stroke,
            dotted,
        );
    };
    match travel::classify_segment(start, end, &limits) {
        travel::TravelClass::Inside => draw(start, end, stroke, dotted),
        // A red rapid is solid, not dotted: the dash pattern is what makes a
        // rapid recede, and an out-of-travel move must be the thing that stands
        // out. The outside colour carries the meaning; the width still follows
        // the caller, so a selected line stays as thick as its highlight.
        travel::TravelClass::Outside => draw(start, end, outside, false),
        travel::TravelClass::Straddling { enter, exit } => {
            let entry = travel::point_at(start, end, enter);
            let leave = travel::point_at(start, end, exit);
            if enter > 0.0 {
                draw(start, entry, outside, false);
            }
            if exit > enter {
                draw(entry, leave, stroke, dotted);
            }
            if exit < 1.0 {
                draw(leave, end, outside, false);
            }
        }
    }
}

/// The chords an arc is painted with.
///
/// Shared with the MDI overlay for the same reason as `Projection`: a block's
/// arc has to be the same curve as the program's, sampled the same way.
pub fn arc_points(segment: &PathSegment, arc: &ArcSpec) -> Vec<[f64; 3]> {
    let mut points = Vec::new();
    let (u, v) = match arc.plane {
        Plane::Xy => (0, 1),
        Plane::Xz => (0, 2),
        Plane::Yz => (1, 2),
        Plane::Uv => (0, 1),
        Plane::Uw => (0, 2),
        Plane::Vw => (1, 2),
    };
    let start_angle = arc.start_angle;
    let mut end_angle = arc.end_angle;
    if arc.clockwise {
        while end_angle >= start_angle {
            end_angle -= std::f64::consts::TAU;
        }
    } else {
        while end_angle <= start_angle {
            end_angle += std::f64::consts::TAU;
        }
    }
    let span = (end_angle - start_angle).abs();
    let steps = ((span / std::f64::consts::TAU) * 64.0)
        .ceil()
        .clamp(8.0, 128.0) as usize;
    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        let angle = start_angle + (end_angle - start_angle) * t;
        let mut point = segment.start;
        point[u] = arc.center[u] + arc.radius * angle.cos();
        point[v] = arc.center[v] + arc.radius * angle.sin();
        // Helical motion interpolates the normal axis.
        if arc.plane == Plane::Xy {
            point[2] = segment.start[2] + (segment.end[2] - segment.start[2]) * t;
        } else if arc.plane == Plane::Xz {
            point[1] = segment.start[1] + (segment.end[1] - segment.start[1]) * t;
        }
        points.push(point);
    }
    points
}

/// Zoom and pan that frame `target` while the projection stays anchored on
/// `reference`.
///
/// `Projection` derives its scale and centre from the box it is given, so a
/// stable view must hand it the same box every frame. A fit is therefore a
/// zoom/pan adjustment on top of that box, not a different box: changing the box
/// is what made loading a program rescale the view and move the markers.
fn fit_zoom_pan(
    reference: Bounds3,
    target: Bounds3,
    rect: Rect,
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
    mirror_x: bool,
) -> (f32, Vec2) {
    // The mirror is part of the camera here as everywhere: a fit that ignored
    // it would centre the part on the unmirrored picture and push it off the
    // card the moment the operator flipped X.
    let target_projection =
        Projection::new(target, rect, mode, yaw, pitch, 1.0, Vec2::ZERO).mirrored(mirror_x);
    let base =
        Projection::new(reference, rect, mode, yaw, pitch, 1.0, Vec2::ZERO).mirrored(mirror_x);
    let zoom = (target_projection.scale / base.scale).clamp(0.05, 40.0);
    let centre = base.project(target.center());
    (zoom, (rect.center() - centre) * zoom)
}

/// The box a fit frames.
///
/// By default it is the program and only the program: the tool and the live
/// trail can be far from the part, and including them shrank the part to make
/// room for a marker. With nothing loaded the machine envelope is used instead
/// of zooming into the millimetres around the tool. `fit_all` asks for exactly
/// the opposite: everything drawn, so an offset far from the part still shows.
fn fit_target(view: &TrajectoryView<'_>, reference: Bounds3) -> Bounds3 {
    let mut target = view.preview.bounds.unwrap_or(reference);
    if !view.fit_all {
        return target;
    }
    for point in view.live_trail {
        target.include(point.position);
    }
    if let Some(tip) = view.tool_tip {
        target.include(tip);
    }
    target.include(view.origin);
    if view.origin_tether {
        target.include([0.0; 3]);
    }
    if let Some(highlight) = view.highlight_origin {
        target.include(highlight);
    }
    target
}

/// A dashed line with a filled head, used for the offset arrows.
///
/// The head stops just short of the target so it points at the marker instead
/// of covering it, and the line is dashed because it relates two points rather
/// than describing a move the machine makes.
fn draw_origin_arrow(painter: &egui::Painter, from: Pos2, to: Pos2, colour: Color32) {
    let delta = to - from;
    let length = delta.length();
    if length <= 8.0 {
        return;
    }
    let direction = delta / length;
    let stroke = Stroke::new(1.5, colour);
    let tip = to - direction * 5.0;
    let barb = 12.0;
    let side = Vec2::new(-direction.y, direction.x);
    let back = tip - direction * barb;
    painter.extend(egui::Shape::dashed_line(&[from, back], stroke, 6.0, 4.0));
    // A filled head reads as a direction at this size; two barbs alone
    // disappeared into the dashes.
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            back + side * (barb * 0.45),
            back - side * (barb * 0.45),
        ],
        colour,
        Stroke::NONE,
    ));
}

/// How far the pan must move to put `value` back between `low` and `high`.
fn edge_shift(value: f32, low: f32, high: f32) -> f32 {
    if value < low {
        low - value
    } else if value > high {
        high - value
    } else {
        0.0
    }
}

/// Whether the tool moved since this card last drew it.
///
/// The previous position is kept per card, in egui's own memory: the pane's
/// zoom and pan are persisted by the caller, and a card that only follows a
/// moving head must not follow the operator's own panning.
fn tool_moved(ui: &Ui, tip: [f64; 3]) -> bool {
    let id = ui.scope_id().with("toolpath-follow-tool");
    let previous: Option<[f64; 3]> = ui.ctx().data(|data| data.get_temp(id));
    ui.ctx().data_mut(|data| data.insert_temp(id, tip));
    previous.map_or(true, |previous| {
        previous
            .iter()
            .zip(tip.iter())
            .any(|(a, b)| (a - b).abs() > 1e-6)
    })
}

/// Clearance kept between the tool marker and the card's edge when the view
/// follows a moving tool.
const TOOL_MARGIN: f32 = 30.0;

#[cfg(test)]
mod follow_tests {
    use super::edge_shift;

    #[test]
    fn the_pan_moves_only_far_enough_to_bring_the_marker_back() {
        // Inside: nothing moves, so a view the operator aimed is not disturbed.
        assert_eq!(edge_shift(100.0, 10.0, 200.0), 0.0);
        // Past the low edge, past the high edge: the marker lands on the line,
        // not in the middle.
        assert_eq!(edge_shift(-20.0, 10.0, 200.0), 30.0);
        assert_eq!(edge_shift(260.0, 10.0, 200.0), -60.0);
        assert_eq!(edge_shift(10.0, 10.0, 200.0), 0.0);
        assert_eq!(edge_shift(200.0, 10.0, 200.0), 0.0);
    }
}

/// Where the toolpath records whether the pointer was over it.
///
/// The shell's pan and zoom chords are hover-scoped for the same reason the
/// wheel is: Ctrl+Left/Right already moves region focus and Ctrl+Up/Down
/// already moves the selected line in the G-code editor and the MDI buffer,
/// so a keyboard pan may only take those chords where the view is the thing
/// under the pointer.
pub const TOOLPATH_HOVER_ID: &str = "linc-toolpath-hovered";

/// Whether the pointer was over a toolpath card on the last frame.
pub fn toolpath_hovered(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::unique(TOOLPATH_HOVER_ID)))
        .unwrap_or(false)
}

/// Draw the toolpath graph. `TrajectoryView::fit` requests a one-shot fit;
/// zoom/pan are mutated in place so the caller can persist them.
pub fn draw_trajectory(ui: &mut Ui, view: &mut TrajectoryView<'_>) -> Response {
    // The first frame of a page can report an unbounded card, and a fit computed
    // for that rect leaves a pan that puts the part outside the real one. Clamp
    // to the viewport so a fit is always computed for a size the screen can hold.
    let screen = ui.ctx().input(|input| input.viewport_rect());
    let desired = Vec2::new(
        ui.available_width().min(screen.width().max(320.0)),
        view.height.max(120.0).min(screen.height().max(120.0)),
    );
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
    // Before the keys are read: the shell decides the pan/zoom chords from
    // this value at the top of the same frame.
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::unique(TOOLPATH_HOVER_ID), response.hovered());
    });
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), theme::SURFACE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );

    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            view.zoom = (view.zoom * (1.0 + scroll * 0.002)).clamp(0.05, 40.0);
            view.auto_fit = false;
        }
    }
    // Blender-style navigation: the middle button orbits, Shift+middle pans, and
    // the wheel zooms. The primary button still pans, which is what a touch
    // screen and a left-handed operator have.
    if response.dragged_by(egui::PointerButton::Middle) {
        let delta = response.drag_delta();
        let shift = ui.input(|input| input.modifiers.shift);
        view.auto_fit = false;
        if shift || view.view != ViewMode::Iso3d {
            view.pan += delta;
        } else {
            view.yaw += delta.x * 0.01;
            // Drag up to look from above, drag down to look from below.
            view.pitch = (view.pitch - delta.y * 0.01).clamp(-1.48, 1.48);
        }
    }
    if response.double_clicked() {
        // Back to the default isometric view.
        view.yaw = DEFAULT_YAW;
        view.pitch = DEFAULT_PITCH;
        view.zoom = 1.0;
        view.pan = Vec2::ZERO;
        view.fit = true;
        view.auto_fit = true;
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        view.pan += response.drag_delta();
        view.auto_fit = false;
    }

    let mut bounds = view.preview.bounds.unwrap_or(Bounds3 {
        min: [-10.0, -10.0, -10.0],
        max: [10.0, 10.0, 10.0],
    });
    let mut has_data = view.preview.bounds.is_some();
    for point in view.live_trail {
        bounds.include(point.position);
        has_data = true;
    }
    if let Some(tip) = view.tool_tip {
        bounds.include(tip);
        has_data = true;
    }
    // The markers are actors too: they are what the operator is looking at on
    // the Offsets page.
    bounds.include(view.origin);
    if let Some(highlight) = view.highlight_origin {
        bounds.include(highlight);
    }
    if !has_data {
        bounds = Bounds3 {
            min: [-10.0, -10.0, -10.0],
            max: [10.0, 10.0, 10.0],
        };
    }
    // The reference box is the machine envelope, not the union of everything
    // drawn: the projection's scale and centre come from it, so using the union
    // rescaled the whole view the moment a program was loaded, and the tool and
    // work-zero markers appeared to jump. An explicit fit now adjusts zoom and
    // pan instead of moving the reference, so loading a program changes what is
    // drawn, not where the view is looking.
    let reference = view.machine_bounds.unwrap_or(bounds);
    // The first frame of a layout can report an unbounded card; a fit computed
    // for that rect leaves a pan that puts the part outside the real one. Wait
    // for a rect the screen can actually hold, then fit once.
    let plausible = rect.width() <= screen.width() + 1.0 && rect.height() <= screen.height() + 1.0;
    if (view.fit || view.auto_fit) && plausible {
        view.fit = false;
        view.auto_fit = true;
        let target_bounds = fit_target(view, reference);
        let (zoom, pan) = fit_zoom_pan(
            reference,
            target_bounds,
            rect.shrink(12.0),
            view.view,
            view.yaw,
            view.pitch,
            view.mirror_x,
        );
        view.zoom = zoom;
        view.pan = pan;
    }
    // Keep the tool on the card while it moves. At a zoomed or panned view a jog
    // otherwise walks the head off the picture with nothing to show where it
    // went. The pan shifts by the least amount that brings the marker back, and
    // only on a frame where the tool actually moved: panning away to look at
    // another part of the program is left alone.
    if let Some(tip) = view.tool_tip {
        if tool_moved(ui, tip) {
            let inner = rect.shrink(TOOL_MARGIN);
            let at = Projection::new(
                reference,
                rect.shrink(12.0),
                view.view,
                view.yaw,
                view.pitch,
                view.zoom,
                view.pan,
            )
            .mirrored(view.mirror_x)
            .project(tip);
            let shift = Vec2::new(
                edge_shift(at.x, inner.left(), inner.right()),
                edge_shift(at.y, inner.top(), inner.bottom()),
            );
            view.pan += shift;
        }
    }
    let projection = Projection::new(
        reference,
        rect.shrink(12.0),
        view.view,
        view.yaw,
        view.pitch,
        view.zoom,
        view.pan,
    )
    .mirrored(view.mirror_x);

    // Grid and axis labels.
    if view.show_grid {
        draw_grid(&painter, &projection, reference);
    }
    // The machine origin is context for the offset: a muted triad with a green
    // line to the active work origin, so the distance the table is editing is
    // visible in the picture rather than only in the numbers.
    if view.origin_tether {
        draw_axes(
            &painter,
            &projection,
            view.variant,
            &view.axis_labels,
            [0.0; 3],
            theme::MUTED,
        );
        draw_origin_arrow(
            &painter,
            projection.project([0.0; 3]),
            projection.project(view.origin),
            theme::SUCCESS,
        );
    }
    draw_axes(
        &painter,
        &projection,
        view.variant,
        &view.axis_labels,
        view.origin,
        theme::AXIS,
    );
    if let Some(highlight) = view.highlight_origin {
        let same_as_active = (highlight[0] - view.origin[0]).abs() < 1e-6
            && (highlight[1] - view.origin[1]).abs() < 1e-6
            && (highlight[2] - view.origin[2]).abs() < 1e-6;
        // The selected row is a temporary preview of where that system's origin
        // is: translucent blue while it is selected, until another row is
        // picked. The active system already has its own triad and green arrow,
        // so only its name is added when it is the selected row.
        let colour = if same_as_active {
            theme::AXIS
        } else {
            theme::dim(theme::ACCENT, 150)
        };
        if !same_as_active {
            draw_axes(
                &painter,
                &projection,
                view.variant,
                &view.axis_labels,
                highlight,
                colour,
            );
            if view.origin_tether {
                draw_origin_arrow(
                    &painter,
                    projection.project([0.0; 3]),
                    projection.project(highlight),
                    colour,
                );
            }
        }
        if let Some(label) = &view.highlight_label {
            painter.text(
                projection.project(highlight) + Vec2::new(8.0, 26.0),
                Align2::LEFT_TOP,
                label,
                FontId::monospace(9.0 * FONT_SCALE),
                colour,
            );
        }
    }

    // Machine bounds.
    if view.show_machine_bounds || view.machine_bounds_always {
        if let Some(machine) = view.machine_bounds {
            // The machine envelope is context, not content: keep it faint enough
            // that the program, the part and the origin marks stay dominant.
            let faint = theme::dim(theme::MUTED, 60);
            draw_bounds(&painter, &projection, machine, faint, 0.8);
        }
    }

    // Stock envelope: the block the program cuts out of. Derived from the
    // program extents so it tracks the loaded program, and drawn translucent
    // so it never hides the toolpath.
    let mut stock_drawn = false;
    if view.show_stock && view.stock_alpha > 0 && has_data {
        if let Some(bounds) = view.preview.bounds.as_ref().and_then(stock_bounds) {
            stock_drawn = true;
            // The fill is configurable and defaults to fully transparent: the
            // block used to wash out the part it was supposed to help place.
            let fill = Color32::from_rgba_premultiplied(
                theme::STOCK_FILL.r(),
                theme::STOCK_FILL.g(),
                theme::STOCK_FILL.b(),
                view.stock_alpha,
            );
            draw_bounds_filled(&painter, &projection, bounds, fill);
            draw_bounds(&painter, &projection, bounds, theme::STOCK_EDGE, 1.0);
        }
    }

    // Program cut and rapid paths.
    if has_data {
        // One tag per line: a canned cycle can contribute several segments to
        // the same line, and a tag on each of them repeated `LN n`.
        let mut tagged_lines: Vec<usize> = Vec::new();
        // The machine envelope is the INI travel box (the same one the machine
        // bounds draw). It is available whether or not the box is visible: the
        // red out-of-travel marking is state, not an optional overlay.
        let travel_limits = view.machine_bounds;
        for segment in &view.preview.segments {
            let is_selected = view.show_selected_line && view.selected_line == Some(segment.line);
            let stroke = if is_selected {
                Stroke::new(2.6, view.selected_line_color)
            } else if segment.kind == SegmentKind::Rapid {
                Stroke::new(1.0, theme::MUTED)
            } else {
                Stroke::new(1.8, theme::ACCENT)
            };
            let dotted = segment.kind == SegmentKind::Rapid;
            if let Some(arc) = &segment.arc {
                // Arcs are already drawn as short chords; classifying each chord
                // puts the red at the drawn crossing. The arc samples are much
                // finer than a pixel at display scale.
                let points = arc_points(segment, arc);
                for pair in points.windows(2) {
                    draw_travel_line(
                        &painter,
                        &projection,
                        pair[0],
                        pair[1],
                        travel_limits,
                        stroke,
                        dotted,
                    );
                }
            } else {
                draw_travel_line(
                    &painter,
                    &projection,
                    segment.start,
                    segment.end,
                    travel_limits,
                    stroke,
                    dotted,
                );
            }
            if is_selected && !tagged_lines.contains(&segment.line) {
                tagged_lines.push(segment.line);
                // Mark the middle of the move, not its end: the end of one
                // block is the start of the next, and a tag sitting on that
                // joint looked like it labelled the following move.
                let tag = if let Some(arc) = &segment.arc {
                    let points = arc_points(segment, arc);
                    projection.project(points.get(points.len() / 2).copied().unwrap_or(segment.end))
                } else {
                    projection.project([
                        (segment.start[0] + segment.end[0]) / 2.0,
                        (segment.start[1] + segment.end[1]) / 2.0,
                        (segment.start[2] + segment.end[2]) / 2.0,
                    ])
                };
                painter.circle_filled(tag, 3.0, view.selected_line_color);
                painter.text(
                    tag + Vec2::new(6.0, -6.0),
                    Align2::LEFT_BOTTOM,
                    format!("LN {}", segment.line),
                    FontId::monospace(10.0 * FONT_SCALE),
                    view.selected_line_color,
                );
            }
        }
    }

    // Program bounds.
    if view.show_program_bounds {
        if let Some(program) = view.preview.bounds {
            if view.show_program_box {
                // Solid and faint: a dashed box reads as a rapid move, and the
                // G0 path is dotted.
                draw_bounds(
                    &painter,
                    &projection,
                    program,
                    theme::dim(theme::CYAN, 80),
                    1.0,
                );
            }
            draw_bounds_dimensions(
                &painter,
                &projection,
                program,
                view.work_offset,
                view.variant,
            );
        }
    }

    // Live trail, always distinct from the loaded program. Hops are drawn one at
    // a time: the ones a rapid reached are dotted exactly like the preview's
    // rapid segments, the cuts stay solid, so the two layers read the same way.
    if view.show_live_trail && view.live_trail.len() >= 2 {
        let cut = Stroke::new(2.0, theme::SUCCESS);
        let rapid = Stroke::new(1.4, theme::SUCCESS);
        // A sampled trail can move only a couple of pixels per point. Dotted
        // per hop would restart the pattern on every point, and a hop shorter
        // than the dash paints solid, so a whole rapid run is one continuous
        // dotted path instead.
        let mut index = 1;
        while index < view.live_trail.len() {
            let from = projection.project(view.live_trail[index - 1].position);
            if view.live_trail[index].rapid {
                let mut points = vec![from];
                while index < view.live_trail.len() && view.live_trail[index].rapid {
                    points.push(projection.project(view.live_trail[index].position));
                    index += 1;
                }
                draw_dotted_path(&painter, &points, rapid);
            } else {
                let to = projection.project(view.live_trail[index].position);
                painter.line_segment([from, to], cut);
                index += 1;
            }
        }
    }
    // Live tool position. Amber, matching the accepted mockup, so the toolhead
    // is distinguishable from the green live trail it sits on.
    if let Some(tip) = view
        .tool_tip
        .or_else(|| view.live_trail.last().map(|point| point.position))
    {
        let pos = projection.project(tip);
        // An inverted triangle whose point sits exactly on the tool position:
        // the tip is the datum, so the shape has to end there rather than be
        // centred on it. A circle plus crosshair left the tip ambiguous at this
        // size.
        const HALF_WIDTH: f32 = 7.0;
        const HEIGHT: f32 = 12.0;
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos + Vec2::new(-HALF_WIDTH, -HEIGHT),
                pos + Vec2::new(HALF_WIDTH, -HEIGHT),
                pos,
            ],
            theme::TOOL,
            Stroke::new(1.0, theme::TOOL),
        ));
    }

    // Execution marker at the current line end.
    if let Some(line) = view.current_line {
        if let Some(segment) = view.preview.segments.iter().rev().find(|s| s.line == line) {
            let pos = projection.project(segment.end);
            painter.circle_filled(pos, 3.0, theme::WARNING);
        }
    }

    if view.show_legend {
        // Legend.
        let legend_rect = Rect::from_min_size(
            rect.left_top() + Vec2::new(12.0, 12.0),
            Vec2::new(214.0, 90.0),
        );
        painter.rect_filled(
            legend_rect,
            CornerRadius::same(4),
            Color32::from_rgb(0x1b, 0x1e, 0x22),
        );
        painter.rect_stroke(
            legend_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );
        painter.line_segment(
            [
                legend_rect.left_top() + Vec2::new(10.0, 17.0),
                legend_rect.left_top() + Vec2::new(32.0, 17.0),
            ],
            Stroke::new(2.0, theme::ACCENT),
        );
        painter.text(
            legend_rect.left_top() + Vec2::new(40.0, 11.0),
            Align2::LEFT_TOP,
            "PROGRAM CUT",
            FontId::monospace(10.0 * FONT_SCALE),
            theme::FG,
        );
        painter.line_segment(
            [
                legend_rect.left_top() + Vec2::new(10.0, 31.0),
                legend_rect.left_top() + Vec2::new(32.0, 31.0),
            ],
            Stroke::new(2.0, theme::SUCCESS),
        );
        painter.text(
            legend_rect.left_top() + Vec2::new(40.0, 25.0),
            Align2::LEFT_TOP,
            "LIVE TOOLPATH",
            FontId::monospace(10.0 * FONT_SCALE),
            theme::FG,
        );
        // Stock key: only meaningful when the envelope is drawn.
        if view.show_stock && stock_drawn {
            painter.rect_filled(
                Rect::from_min_size(
                    legend_rect.left_top() + Vec2::new(10.0, 41.0),
                    Vec2::new(22.0, 9.0),
                ),
                CornerRadius::same(1),
                theme::STOCK_FILL,
            );
            painter.rect_stroke(
                Rect::from_min_size(
                    legend_rect.left_top() + Vec2::new(10.0, 41.0),
                    Vec2::new(22.0, 9.0),
                ),
                CornerRadius::same(1),
                Stroke::new(1.0, theme::STOCK_EDGE),
                StrokeKind::Inside,
            );
            painter.text(
                legend_rect.left_top() + Vec2::new(40.0, 38.0),
                Align2::LEFT_TOP,
                "STOCK",
                FontId::monospace(10.0 * FONT_SCALE),
                theme::FG,
            );
        }
        if view.tool_tip.is_some() {
            // Same inverted triangle as the marker, so the key is recognisable.
            let key_tip = legend_rect.left_top() + Vec2::new(21.0, 66.0);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    key_tip + Vec2::new(-4.0, -8.0),
                    key_tip + Vec2::new(4.0, -8.0),
                    key_tip,
                ],
                theme::TOOL,
                Stroke::new(1.0, theme::TOOL),
            ));
            painter.text(
                legend_rect.left_top() + Vec2::new(40.0, 55.0),
                Align2::LEFT_TOP,
                "TOOL",
                FontId::monospace(10.0 * FONT_SCALE),
                theme::FG,
            );
        }
        painter.text(
            legend_rect.left_top() + Vec2::new(10.0, 74.0),
            Align2::LEFT_TOP,
            if view.view == ViewMode::Iso3d {
                format!(
                    "{}  •  ZOOM {:.1}x  •  MMB ORBIT",
                    view.view.label(),
                    view.zoom
                )
            } else {
                format!("{}  •  ZOOM {:.1}x", view.view.label(), view.zoom)
            },
            FontId::monospace(9.0 * FONT_SCALE),
            theme::MUTED,
        );
    }

    if !has_data {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO PROGRAM LOADED",
            FontId::monospace(14.0 * FONT_SCALE),
            theme::MUTED,
        );
    }

    draw_axis_triad(
        &painter,
        rect,
        view.view,
        view.yaw,
        view.pitch,
        view.mirror_x,
        &view.axis_labels,
    );

    response
}

/// The axes present in `labels`, as a screen direction or `None` when the axis
/// points straight at or away from the viewer.
pub fn axis_triad(
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
    mirror_x: bool,
    labels: &[String],
) -> Vec<(&'static str, Option<Vec2>)> {
    let mut out = Vec::new();
    for (label, unit) in [
        ("X", [1.0f32, 0.0, 0.0]),
        ("Y", [0.0, 1.0, 0.0]),
        ("Z", [0.0, 0.0, 1.0]),
    ] {
        if !labels.iter().any(|axis| axis == label) {
            continue;
        }
        let (dx, dy) = view_xy(unit, mode, yaw, pitch, mirror_x);
        let length = (dx * dx + dy * dy).sqrt();
        let direction = if length < 0.2 {
            None
        } else {
            Some(Vec2::new(dx / length, dy / length))
        };
        out.push((label, direction));
    }
    out
}

/// A small X/Y/Z indicator in the graph's top-right corner.
///
/// The arrow direction comes from the same projection as the part, so it turns
/// with the orbit. An axis that points at the viewer has no screen direction and
/// is drawn as a circle instead; its name goes under the whole triad, where it
/// does not sit in the middle of the other two arms.
fn draw_axis_triad(
    painter: &Painter,
    rect: Rect,
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
    mirror_x: bool,
    labels: &[String],
) {
    let center = rect.right_top() + Vec2::new(-46.0, 42.0);
    let length = 18.0;
    let font = FontId::monospace(9.0 * FONT_SCALE);
    // Half the height of the 9 px face, so labels count towards the arm's reach.
    let half_text = 5.0;
    let mut bottom = center.y + 4.0;
    let mut at_viewer = Vec::new();
    for (label, direction) in axis_triad(mode, yaw, pitch, mirror_x, labels) {
        match direction {
            Some(direction) => {
                let tip = center + direction * length;
                painter.line_segment([center, tip], Stroke::new(1.4, theme::MUTED));
                // Arrowhead: two short legs back from the tip.
                let side = Vec2::new(-direction.y, direction.x) * 3.0;
                let back = direction * 5.0;
                painter.line_segment([tip, tip - back + side], Stroke::new(1.4, theme::MUTED));
                painter.line_segment([tip, tip - back - side], Stroke::new(1.4, theme::MUTED));
                let label_pos = tip + direction * 7.0;
                painter.text(
                    label_pos,
                    Align2::CENTER_CENTER,
                    label,
                    font.clone(),
                    theme::MUTED,
                );
                bottom = bottom.max(label_pos.y + half_text);
            }
            None => {
                // Pointing at the viewer: a circle with a dot. The label is
                // placed after the arms are known, below all of them.
                painter.circle_stroke(center, 4.0, Stroke::new(1.2, theme::MUTED));
                painter.circle_filled(center, 1.2, theme::MUTED);
                at_viewer.push(label);
            }
        }
    }
    for (index, label) in at_viewer.into_iter().enumerate() {
        painter.text(
            Pos2::new(center.x, bottom + 6.0 + index as f32 * 10.0),
            Align2::CENTER_TOP,
            label,
            font.clone(),
            theme::MUTED,
        );
    }
}

/// Stock envelope derived from the program extents: the program's XY footprint
/// extended down to the lowest Z it reaches, with a small margin. Returns
/// `None` when the program has no usable extents.
fn stock_bounds(program: &Bounds3) -> Option<Bounds3> {
    let (min, max) = (program.min, program.max);
    if !min.iter().chain(max.iter()).all(|value| value.is_finite()) {
        return None;
    }
    let span_x = (max[0] - min[0]).abs();
    let span_y = (max[1] - min[1]).abs();
    if span_x <= f64::EPSILON && span_y <= f64::EPSILON {
        return None;
    }
    let margin = 2.0;
    let floor = min[2].min(max[2]) - margin;
    Some(Bounds3 {
        min: [min[0] - margin, min[1] - margin, floor],
        max: [
            max[0] + margin,
            max[1] + margin,
            max[2].max(min[2]) + margin,
        ],
    })
}

/// Translucent solid for the stock envelope: the three visible faces of the
/// box, so it reads as a block rather than a wireframe.
/// Dimension lines and labels on the program bounds, drawn the way AXIS marks
/// an envelope: a line outside each near edge, witness lines from the corners,
/// arrowheads at the ends, and the value in the plane of the drawing.
fn draw_bounds_dimensions(
    painter: &Painter,
    projection: &Projection,
    bounds: Bounds3,
    work_offset: Option<[f64; 3]>,
    variant: MachineVariant,
) {
    let min = bounds.min;
    let max = bounds.max;
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let smallest = size[0].min(size[1]).min(size[2]).max(0.5);
    // Annotation size in pixels, converted to model units, so the dimensions
    // stay readable at any zoom the way a drawing's text does.
    let pixels_per_unit = (projection.scale * projection.zoom).max(1e-6) as f64;
    let glyph = (18.0 / pixels_per_unit).min(smallest * 0.2);
    let gap = 22.0 / pixels_per_unit;
    let color = theme::dim(theme::CYAN, 190);
    let offset = work_offset.unwrap_or([0.0; 3]);
    let pad = glyph * 0.6;
    let row = glyph * 1.1;
    // In a side view the dimension measured along the view axis collapses to a
    // point. Its labels have nowhere to stand, so they are left out entirely
    // rather than piled up on the drawing.
    let hidden = view_axis(projection);

    // The drawing is laid out around the box as the operator sees it, and the
    // camera now stands in front of the machine (+Y runs into the screen). That
    // swaps which model corner appears where: the upper-right edge is the max-Y
    // one now, so the X dimension moves there and its values, offsets and glyph
    // tops are mirrored in Y with it. The Y dimension keeps the min-X edge,
    // which is still the upper-left one. Done this way every label lands in
    // exactly the same place on screen as it did when the camera was behind the
    // machine.

    // X: outside the back edge, on the ground plane. The length sits in the
    // middle; each end carries the work coordinate of that end.
    dimension_line(
        painter,
        projection,
        [min[0], max[1], min[2]],
        [max[0], max[1], min[2]],
        [0.0, gap, 0.0],
        color,
    );
    if hidden != Some('X') {
        put_label(
            painter,
            projection,
            [min[0] + size[0] * 0.5, max[1] + gap + row, min[2]],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            glyph,
            &format!("{:.2}", size[0]),
            color,
            None,
        );
        let start_text = format!("X{:.1}", min[0] - offset[0]);
        put_label(
            painter,
            projection,
            [
                min[0] - pad,
                max[1] + gap + row + pad + text_width(&start_text, glyph) * 0.5,
                min[2],
            ],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            glyph,
            &start_text,
            color,
            Some(([min[0], max[1] + gap, min[2]], [-1.0, 0.0, 0.0])),
        );
        let end_text = format!("X{:.1}", max[0] - offset[0]);
        put_label(
            painter,
            projection,
            [
                max[0] + pad,
                max[1] + gap + row + pad + text_width(&end_text, glyph) * 0.5,
                min[2],
            ],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            glyph,
            &end_text,
            color,
            Some(([max[0], max[1] + gap, min[2]], [1.0, 0.0, 0.0])),
        );
    }

    // Y: outside the left edge, reading along Y.
    dimension_line(
        painter,
        projection,
        [min[0], max[1], min[2]],
        [min[0], min[1], min[2]],
        [-gap, 0.0, 0.0],
        color,
    );
    if hidden != Some('Y') {
        put_label(
            painter,
            projection,
            [min[0] - gap - row, max[1] - size[1] * 0.5, min[2]],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            glyph,
            &format!("{:.2}", size[1]),
            color,
            None,
        );
        // Each end value stays at its own end: the ends of a dimension are not
        // interchangeable, so the near coordinate belongs at the near end even
        // though the mirrored drawing moved which end of the line appears where.
        let start_text = format!("Y{:.1}", min[1] - offset[1]);
        put_label(
            painter,
            projection,
            [
                min[0] - gap - row - text_width(&start_text, glyph) * 0.5,
                min[1] - pad,
                min[2],
            ],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            glyph,
            &start_text,
            color,
            Some(([min[0] - gap, min[1], min[2]], [0.0, -1.0, 0.0])),
        );
        let end_text = format!("Y{:.1}", max[1] - offset[1]);
        put_label(
            painter,
            projection,
            [
                min[0] - gap - row - text_width(&end_text, glyph) * 0.5,
                max[1] + pad,
                min[2],
            ],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            glyph,
            &end_text,
            color,
            Some(([min[0] - gap, max[1], min[2]], [0.0, 1.0, 0.0])),
        );
    }

    // Z: up the front-left corner (min X, min Y), which is the corner on the
    // left of the drawing. The far corner already carries the X and Y
    // dimensions and their witnesses.
    //
    // The labels stand in the XZ plane: like the X and Y dimensions, a label
    // reads along its own axis and its glyph tops point away from the box, so
    // the Z text lies flat across X instead of turning into the YZ plane. `up`
    // is -X, not +X, or the letters come out mirrored.
    dimension_line(
        painter,
        projection,
        [min[0], min[1], min[2]],
        [min[0], min[1], max[2]],
        [-gap * 1.4, -gap * 1.4, 0.0],
        color,
    );
    let start_text = format!("Z{:.1}", min[2] - offset[2]);
    let end_text = format!("Z{:.1}", max[2] - offset[2]);
    // Y stays on the line so every label is coplanar with it.
    let z_line_x = min[0] - gap * 1.4;
    let z_label_y = min[1] - gap * 1.4;
    if hidden != Some('Z') && variant == MachineVariant::Lathe {
        // The turning view draws Z across the screen with X up it, so the mill's
        // label plane would stand every one of these on its head: they read along
        // the line instead, upright, one row clear of it.
        let up = z_label_up(projection, variant);
        let row = glyph * 1.7;
        // The measuring line runs outside the box at min X; the labels sit
        // further outside it, on the -X side. Glyph-up stays up so the text
        // remains upright, but it must not double as the row offset: +X there
        // pushed every label back toward the box.
        let at = |z: f64, shift: f64| lathe_z_label_anchor(z_line_x, z_label_y, row, z, shift);
        put_label(
            painter,
            projection,
            at(min[2] + size[2] * 0.5, 0.0),
            [0.0, 0.0, 1.0],
            up,
            glyph,
            &format!("{:.2}", size[2]),
            color,
            None,
        );
        let start_width = text_width(&start_text, glyph);
        put_label(
            painter,
            projection,
            at(min[2], -pad - start_width * 0.5),
            [0.0, 0.0, 1.0],
            up,
            glyph,
            &start_text,
            color,
            None,
        );
        let end_width = text_width(&end_text, glyph);
        put_label(
            painter,
            projection,
            at(max[2], pad + end_width * 0.5),
            [0.0, 0.0, 1.0],
            up,
            glyph,
            &end_text,
            color,
            None,
        );
    } else if hidden != Some('Z') {
        put_label(
            painter,
            projection,
            [
                z_line_x - pad - glyph * 0.5,
                z_label_y,
                min[2] + size[2] * 0.5,
            ],
            [0.0, 0.0, 1.0],
            [-1.0, 0.0, 0.0],
            glyph,
            &format!("{:.2}", size[2]),
            color,
            None,
        );
        // The two end values are the length label turned a quarter turn clockwise
        // about Y: they read along +X with their caps up +Z, so they are written
        // horizontally, centred on the line and clear of its end by a pad. Below
        // and above the line is the only room for horizontal text, since sideways
        // they would run back across it.
        put_label(
            painter,
            projection,
            [z_line_x, z_label_y, min[2] - pad - glyph * 0.5],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            glyph,
            &start_text,
            color,
            Some((
                [min[0] - gap * 1.4, min[1] - gap * 1.4, min[2]],
                [0.0, 0.0, -1.0],
            )),
        );
        put_label(
            painter,
            projection,
            [z_line_x, z_label_y, max[2] + pad + glyph * 0.5],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            glyph,
            &end_text,
            color,
            Some((
                [min[0] - gap * 1.4, min[1] - gap * 1.4, max[2]],
                [0.0, 0.0, 1.0],
            )),
        );
    }
}

/// Anchor of a lathe Z boundary label: one row on the -X side of the measuring
/// line, so the value clears the line away from the box.
fn lathe_z_label_anchor(line_x: f64, line_y: f64, row: f64, z: f64, shift: f64) -> [f64; 3] {
    [line_x - row, line_y, z + shift]
}

/// Width of `text` at `glyph` height, in model units.
fn text_width(text: &str, glyph: f64) -> f64 {
    hershey::string_len(text) as f64 * glyph / 440.0
}

/// The axis that points straight at or away from the camera, when the view
/// looks along one machine axis.
fn view_axis(projection: &Projection) -> Option<char> {
    for (axis, unit) in [
        ('X', [1.0f32, 0.0, 0.0]),
        ('Y', [0.0, 1.0, 0.0]),
        ('Z', [0.0, 0.0, 1.0]),
    ] {
        let (dx, dy) = view_xy(
            unit,
            projection.mode,
            projection.yaw,
            projection.pitch,
            projection.mirror_x,
        );
        if (dx * dx + dy * dy).sqrt() < 0.2 {
            return Some(axis);
        }
    }
    None
}

/// Screen direction of a model-space vector, without the projection scale:
/// zero means the vector points at or away from the viewer.
fn screen_direction(projection: &Projection, vector: [f64; 3]) -> Vec2 {
    let (dx, dy) = view_xy(
        [vector[0] as f32, vector[1] as f32, vector[2] as f32],
        projection.mode,
        projection.yaw,
        projection.pitch,
        projection.mirror_x,
    );
    Vec2::new(dx, dy)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Glyph-up direction of the Z dimension labels.
///
/// The mill reads them up the XZ plane with -X as the glyph tops. The lathe
/// turns X vertical on screen (and the mirror turns it over), so that same
/// vector points at the floor there and the labels come out upside down: the
/// lathe reads them along the line instead, tops up the screen whichever way X
/// runs.
fn z_label_up(projection: &Projection, variant: MachineVariant) -> [f64; 3] {
    const PLUS_X: [f64; 3] = [1.0, 0.0, 0.0];
    const MINUS_X: [f64; 3] = [-1.0, 0.0, 0.0];
    if variant != MachineVariant::Lathe {
        return MINUS_X;
    }
    if screen_direction(projection, PLUS_X).y < 0.0 {
        PLUS_X
    } else {
        MINUS_X
    }
}

/// Roll a label's glyph-up vector a quarter turn about its baseline so the text
/// plane faces the camera. Only for a plane that is edge-on: the baseline stays
/// where it is, which keeps the label reading along the axis it measures.
///
/// The second value is set when the readable orientation would put the text
/// back across the drawing: the baseline then moves one glyph height the other
/// way, instead of turning the letters upside down.
fn rolled_up(
    projection: &Projection,
    center: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
) -> ([f64; 3], bool) {
    let mut normal = cross(right, up);
    let screen = screen_direction(projection, normal);
    // Readable orientation: tops up the screen, and tops to the left when the
    // baseline is vertical.
    let flip = screen.y > 0.05 || (screen.y.abs() <= 0.05 && screen.x > 0.0);
    if flip {
        normal = [-normal[0], -normal[1], -normal[2]];
    }
    // The labels sit outside the box. Keep the glyph tops on the outside of the
    // drawing, so the text does not cover the part or the dimension line.
    let away = projection.project(center) - projection.rect.center();
    let shift = screen_direction(projection, normal).dot(away) < -1e-3;
    (normal, shift)
}

/// Draw a dimension label.
///
/// `flat` is set for the value at the end of a dimension line, as
/// `(end, outward)`: the point where the line ends and the direction it points
/// away from the span. When the label's baseline lies along the view, the text
/// is drawn flat clear of the line: inside the span under a horizontal
/// dimension, or just past the end of a vertical one. Pass `None` for the
/// length label in the middle of the line.
fn put_label(
    painter: &Painter,
    projection: &Projection,
    center: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    glyph: f64,
    text: &str,
    color: Color32,
    flat: Option<([f64; 3], [f64; 3])>,
) {
    let pixels_per_unit = (projection.scale * projection.zoom) as f64;
    if screen_direction(projection, right).length() < 0.2 {
        // The baseline points into the screen, so no rotation about it can make
        // the text readable: lay it flat and clear of the line.
        let (end, outward) = flat.unwrap_or((center, up));
        let glyph_px = (glyph * pixels_per_unit) as f32;
        let width_px = hershey::string_len(text) * glyph_px / 440.0;
        let gap = (glyph * 0.6 * pixels_per_unit) as f32;
        let end_pos = projection.project(end);
        let direction = screen_direction(projection, outward).normalized();
        let half_along = 0.5 * (width_px * direction.x.abs() + glyph_px * direction.y.abs());
        let position = if direction.x.abs() > direction.y.abs() {
            // A horizontal dimension: step back inside the span along the line
            // and out across it, under the drawing.
            let away = end_pos - projection.rect.center();
            let mut across = away - direction * away.dot(direction);
            if across.length() < 1e-3 {
                across = Vec2::new(-direction.y, direction.x);
            }
            across = across.normalized();
            let half_across = 0.5 * (width_px * across.x.abs() + glyph_px * across.y.abs());
            // The value sits further inside than the pad that spaces the line
            // from the arrow, so the vertical dimension's end value at the same
            // corner does not run into it.
            end_pos - direction * (half_along + gap * 2.5) + across * (half_across + gap)
        } else {
            // A vertical dimension: the text goes just past the end, centred on
            // the line, where there is room above or below the drawing.
            end_pos + direction * (half_along + gap)
        };
        hershey::plot_string_flat(painter, position, glyph_px, text, color);
        return;
    }
    let mut center = center;
    let up = if screen_direction(projection, up).length() < 0.2 {
        let (up, shift) = rolled_up(projection, center, right, up);
        // Lay the text on the outside by moving the baseline, not by turning
        // the glyphs upside down, and leave a pad between it and the line.
        let pad = glyph * 0.6;
        let offset = if shift { -glyph - pad } else { pad };
        center = [
            center[0] + up[0] * offset,
            center[1] + up[1] * offset,
            center[2] + up[2] * offset,
        ];
        up
    } else {
        up
    };
    let width = text_width(text, glyph);
    let origin = [
        center[0] - right[0] * width * 0.5,
        center[1] - right[1] * width * 0.5,
        center[2] - right[2] * width * 0.5,
    ];
    hershey::plot_string(projection, origin, right, up, glyph, text, color, painter);
}

/// A dimension line with witness lines and arrowheads, in model space so it
/// shares the box's perspective.
fn dimension_line(
    painter: &Painter,
    projection: &Projection,
    from: [f64; 3],
    to: [f64; 3],
    offset: [f64; 3],
    color: Color32,
) {
    let a = projection.project(from);
    let b = projection.project(to);
    let oa = projection.project([
        from[0] + offset[0],
        from[1] + offset[1],
        from[2] + offset[2],
    ]);
    let ob = projection.project([to[0] + offset[0], to[1] + offset[1], to[2] + offset[2]]);
    if (ob - oa).length() < 4.0 {
        return;
    }
    let stroke = Stroke::new(0.8, color);
    painter.line_segment([oa, ob], stroke);
    // Witness lines: from the corner the dimension measures to its line.
    painter.line_segment([a, oa], Stroke::new(0.6, theme::dim(color, 120)));
    painter.line_segment([b, ob], Stroke::new(0.6, theme::dim(color, 120)));
    // Arrowheads at both ends, pointing inwards (<---> style): the legs run
    // towards the middle so the tips meet at the witness lines.
    let direction = (ob - oa).normalized();
    let normal = Vec2::new(-direction.y, direction.x) * 3.0;
    for (tip, sign) in [(oa, 1.0), (ob, -1.0)] {
        painter.line_segment([tip, tip + direction * 7.0 * sign + normal], stroke);
        painter.line_segment([tip, tip + direction * 7.0 * sign - normal], stroke);
    }
}

fn draw_bounds_filled(painter: &Painter, projection: &Projection, bounds: Bounds3, fill: Color32) {
    let min = bounds.min;
    let max = bounds.max;
    let corner = |x: f64, y: f64, z: f64| projection.project([x, y, z]);
    let top = [
        corner(min[0], min[1], max[2]),
        corner(max[0], min[1], max[2]),
        corner(max[0], max[1], max[2]),
        corner(min[0], max[1], max[2]),
    ];
    // Top face plus the two side faces nearest the viewer.
    for face in [
        top.to_vec(),
        vec![
            corner(min[0], min[1], min[2]),
            corner(max[0], min[1], min[2]),
            corner(max[0], min[1], max[2]),
            corner(min[0], min[1], max[2]),
        ],
        vec![
            corner(max[0], min[1], min[2]),
            corner(max[0], max[1], min[2]),
            corner(max[0], max[1], max[2]),
            corner(max[0], min[1], max[2]),
        ],
    ] {
        painter.add(egui::Shape::convex_polygon(face, fill, Stroke::NONE));
    }
}

fn draw_bounds(
    painter: &Painter,
    projection: &Projection,
    bounds: Bounds3,
    color: Color32,
    width: f32,
) {
    let stroke = Stroke::new(width, color);
    let min = bounds.min;
    let max = bounds.max;
    let corners = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], max[2]],
        [min[0], max[1], max[2]],
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (a, b) in edges {
        Projection::draw_line(
            painter,
            projection.project(corners[a]),
            projection.project(corners[b]),
            stroke,
            false,
        );
    }
}

fn draw_grid(painter: &Painter, projection: &Projection, bounds: Bounds3) {
    let size = bounds.size();
    let step = nice_step(size[0].max(size[1]).max(size[2]) / 10.0);
    let stroke = Stroke::new(0.5, Color32::from_rgb(0x24, 0x28, 0x2d));
    for mode_axis in 0..2 {
        let mut value = (bounds.min[mode_axis] / step).floor() * step;
        while value <= bounds.max[mode_axis] {
            let (start, end) = match projection.mode {
                ViewMode::Iso3d => {
                    if mode_axis == 0 {
                        (
                            [value, bounds.min[1], bounds.min[2]],
                            [value, bounds.max[1], bounds.min[2]],
                        )
                    } else {
                        (
                            [bounds.min[0], value, bounds.min[2]],
                            [bounds.max[0], value, bounds.min[2]],
                        )
                    }
                }
                ViewMode::TopXy => {
                    if mode_axis == 0 {
                        ([value, bounds.min[1], 0.0], [value, bounds.max[1], 0.0])
                    } else {
                        ([bounds.min[0], value, 0.0], [bounds.max[0], value, 0.0])
                    }
                }
                ViewMode::SideYz => {
                    // Horizontal axis Y, vertical Z, drawn at the near X face.
                    if mode_axis == 0 {
                        (
                            [bounds.min[0], value, bounds.min[2]],
                            [bounds.min[0], value, bounds.max[2]],
                        )
                    } else {
                        (
                            [bounds.min[0], bounds.min[1], value],
                            [bounds.max[0], bounds.min[1], value],
                        )
                    }
                }
                ViewMode::SideXz => {
                    if mode_axis == 0 {
                        ([bounds.min[0], 0.0, value], [bounds.max[0], 0.0, value])
                    } else {
                        ([value, 0.0, bounds.min[2]], [value, 0.0, bounds.max[2]])
                    }
                }
                ViewMode::LatheXz => {
                    if mode_axis == 0 {
                        ([bounds.min[0], 0.0, value], [bounds.max[0], 0.0, value])
                    } else {
                        ([value, 0.0, bounds.min[2]], [value, 0.0, bounds.max[2]])
                    }
                }
            };
            painter.line_segment([projection.project(start), projection.project(end)], stroke);
            value += step;
        }
    }
}

fn nice_step(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let exponent = value.log10().floor();
    let base = 10f64.powf(exponent);
    let fraction = value / base;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * base
}

fn draw_axes(
    painter: &Painter,
    projection: &Projection,
    variant: MachineVariant,
    axis_labels: &[String],
    origin: [f64; 3],
    color: Color32,
) {
    let origin_pos = projection.project(origin);
    let length = 24.0;
    let mut axes = Vec::new();
    if axis_labels.iter().any(|axis| axis == "X") {
        axes.push(("X", [1.0, 0.0, 0.0]));
    }
    if axis_labels.iter().any(|axis| axis == "Y") || variant == MachineVariant::Mill {
        axes.push(("Y", [0.0, 1.0, 0.0]));
    }
    if axis_labels.iter().any(|axis| axis == "Z") || variant == MachineVariant::Mill {
        axes.push(("Z", [0.0, 0.0, 1.0]));
    }
    painter.circle_filled(origin_pos, 2.0, color);
    // The cross follows the camera through the same projection as the part. It
    // used to be three fixed screen vectors, so it kept drawing the old orbit:
    // +Y pointed at the viewer no matter where the camera stood.
    let font = FontId::monospace(10.0 * FONT_SCALE);
    for (label, unit) in axes {
        let (dx, dy) = view_xy(
            unit,
            projection.mode,
            projection.yaw,
            projection.pitch,
            projection.mirror_x,
        );
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.2 {
            // Along the view, so it has no screen direction: a dot, the way the
            // corner triad shows it.
            painter.circle_stroke(origin_pos, 2.5, Stroke::new(1.2, color));
            painter.text(
                origin_pos + Vec2::new(4.0, -4.0),
                Align2::LEFT_BOTTOM,
                label,
                font.clone(),
                color,
            );
            continue;
        }
        let vector = Vec2::new(dx / len, dy / len) * length;
        painter.line_segment([origin_pos, origin_pos + vector], Stroke::new(1.2, color));
        painter.text(
            origin_pos + vector + Vec2::new(3.0, -3.0),
            Align2::LEFT_BOTTOM,
            label,
            font.clone(),
            color,
        );
    }
}

/// Compact sparkline for diagnostics. Values are drawn without axes.
pub fn draw_sparkline(ui: &mut Ui, values: &[f64], color: Color32, height: f32) -> Response {
    let desired = Vec2::new(ui.available_width(), height);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), theme::SURFACE);
    if values.len() >= 2 {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let span = (max - min).max(1e-6);
        let points: Vec<Pos2> = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let x =
                    rect.left() + rect.width() * index as f32 / (values.len() - 1).max(1) as f32;
                let y =
                    rect.bottom() - 3.0 - ((*value - min) / span) as f32 * (rect.height() - 6.0);
                Pos2::new(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.4, color)));
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "—",
            FontId::monospace(12.0 * FONT_SCALE),
            theme::MUTED,
        );
    }
    response
}

/// Mini graph used by the Program CODE view; draws only XY program layers.
pub fn draw_mini_xy(ui: &mut Ui, preview: &Preview, live: &[TrailPoint], height: f32) -> Response {
    let mut view = TrajectoryView::new(preview, live, MachineVariant::Mill);
    view.view = ViewMode::TopXy;
    view.height = height;
    view.machine_bounds = None;
    view.show_machine_bounds = false;
    view.axis_labels = vec!["X".into(), "Y".into()];
    draw_trajectory(ui, &mut view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_brings_points_inside_the_rect() {
        let bounds = Bounds3 {
            min: [-100.0, -100.0, -100.0],
            max: [100.0, 100.0, 100.0],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let projection = Projection::new(
            bounds,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            1.0,
            Vec2::ZERO,
        );
        for point in [
            [0.0, 0.0, 0.0],
            [100.0, 100.0, 100.0],
            [-100.0, 100.0, -100.0],
        ] {
            let pos = projection.project(point);
            assert!(rect.contains(pos), "{pos:?} outside {rect:?}");
        }
    }

    /// One frame of the card, with the program cut it painted.
    ///
    /// The shapes are the card's own output, so a test can hold the shared
    /// projection to what the card actually draws instead of to a second copy
    /// of the maths.
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
                if stroke.color == theme::ACCENT && (stroke.width - 1.8).abs() < 1e-6 {
                    cut.push(*points);
                }
            }
        }
        (rect, cut)
    }

    fn assert_card_matches_projection(mode: ViewMode, yaw: f32, pitch: f32) {
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
        let (rect, cut) = card_cut(&preview, bounds, mode, yaw, pitch);
        assert_eq!(cut.len(), 1, "{mode:?}: the card draws the move once");
        let projection =
            Projection::new(bounds, rect.shrink(12.0), mode, yaw, pitch, 1.0, Vec2::ZERO);
        for (drawn, model) in [(cut[0][0], segment.start), (cut[0][1], segment.end)] {
            let expected = projection.project(model);
            assert!(
                (drawn - expected).length() < 0.01,
                "{mode:?}: drawn {drawn:?} vs projected {expected:?}"
            );
        }
    }

    #[test]
    fn the_card_draws_through_the_shared_projection() {
        assert_card_matches_projection(ViewMode::TopXy, DEFAULT_YAW, DEFAULT_PITCH);
    }

    /// The overlay's old agreement test covered only the flat views, where the
    /// mapping is linear; the card opens in 3D/ISO, so an orbited camera is the
    /// case that was unpinned.
    #[test]
    fn the_card_draws_through_the_shared_projection_when_orbited() {
        assert_card_matches_projection(ViewMode::Iso3d, 0.9, 0.42);
    }

    #[test]
    fn the_card_samples_an_arc_with_the_shared_sampler() {
        let bounds = Bounds3 {
            min: [20.0, -5.0, -5.0],
            max: [75.0, 50.0, 10.0],
        };
        let arc = ArcSpec {
            center: [50.0, 25.0, 0.0],
            radius: 20.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::FRAC_PI_2,
            clockwise: false,
            plane: Plane::Xy,
        };
        let segment = PathSegment {
            start: [70.0, 25.0, 0.0],
            end: [50.0, 45.0, 0.0],
            kind: SegmentKind::Feed,
            line: 1,
            arc: Some(arc.clone()),
            tool: None,
        };
        let preview = Preview {
            segments: vec![segment.clone()],
            bounds: Some(bounds),
            ..Preview::empty()
        };
        let (rect, cut) = card_cut(&preview, bounds, ViewMode::Iso3d, 0.9, 0.42);
        let projection = Projection::new(
            bounds,
            rect.shrink(12.0),
            ViewMode::Iso3d,
            0.9,
            0.42,
            1.0,
            Vec2::ZERO,
        );
        let points = arc_points(&segment, &arc);
        assert_eq!(cut.len(), points.len() - 1, "one chord per sample pair");
        for pair in points.windows(2) {
            let from = projection.project(pair[0]);
            let to = projection.project(pair[1]);
            assert!(
                cut.iter()
                    .any(|drawn| (drawn[0] - from).length() < 0.01
                        && (drawn[1] - to).length() < 0.01),
                "missing chord {from:?}-{to:?}"
            );
        }
    }

    /// One frame of a card's live trail: the line segments painted in the
    /// trail's colour, whether they came from one solid hop or from dots.
    fn trail_shapes(
        trail: &[TrailPoint],
        variant: MachineVariant,
        mode: ViewMode,
    ) -> Vec<[Pos2; 2]> {
        let bounds = Bounds3 {
            min: [-50.0, -50.0, -50.0],
            max: [50.0, 50.0, 50.0],
        };
        let preview = Preview {
            bounds: Some(bounds),
            ..Preview::empty()
        };
        let mut view = TrajectoryView::new(&preview, trail, variant);
        view.view = mode;
        view.machine_bounds = Some(bounds);
        view.height = 400.0;
        view.show_grid = false;
        view.show_legend = false;
        view.show_program_bounds = false;
        view.show_stock = false;
        view.show_selected_line = false;
        view.show_live_trail = true;
        view.fit = false;
        view.auto_fit = false;
        view.tool_tip = None;
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            draw_trajectory(ui, &mut view);
        });
        output.textures_delta.clear();
        let mut shapes = Vec::new();
        for clipped in &output.shapes {
            if let egui::Shape::LineSegment { points, stroke } = &clipped.shape {
                if stroke.color == theme::SUCCESS {
                    shapes.push(*points);
                }
            }
        }
        shapes
    }

    fn hop(pos: [f64; 3], rapid: bool) -> TrailPoint {
        TrailPoint::new(pos, rapid)
    }

    #[test]
    fn a_live_rapid_is_dotted_and_a_live_cut_is_solid() {
        let start = [-40.0, -40.0, 0.0];
        let end = [40.0, 40.0, 0.0];

        // A cut: one solid hop, drawn end to end.
        let cut = trail_shapes(
            &[hop(start, false), hop(end, false)],
            MachineVariant::Mill,
            ViewMode::TopXy,
        );
        assert_eq!(cut.len(), 1, "a cut is one solid segment: {cut:?}");
        let length = (cut[0][1] - cut[0][0]).length();
        assert!(length > 100.0, "the cut spans the hop: {length}");

        // A rapid: the same hop, broken into the preview's dots.
        let rapid = trail_shapes(
            &[hop(start, true), hop(end, true)],
            MachineVariant::Mill,
            ViewMode::TopXy,
        );
        assert!(
            rapid.len() > 5,
            "a rapid is a dot pattern, not one segment: {}",
            rapid.len()
        );
        for segment in &rapid {
            let length = (segment[1] - segment[0]).length();
            assert!(length <= 1.7, "each dot is short: {length}");
        }
        // The dots cover less than half the hop: the gaps are what read as a
        // rapid at a glance.
        let covered: f32 = rapid
            .iter()
            .map(|segment| (segment[1] - segment[0]).length())
            .sum();
        assert!(
            covered < length * 0.5,
            "dots leave gaps: {covered} of {length}"
        );

        // Mixed: the cut half stays one solid line, the rapid half is dotted.
        let middle = [0.0, 0.0, 0.0];
        let mixed = trail_shapes(
            &[hop(start, false), hop(middle, false), hop(end, true)],
            MachineVariant::Mill,
            ViewMode::TopXy,
        );
        assert!(
            mixed.len() > 6,
            "the rapid half adds dots to the single cut: {}",
            mixed.len()
        );
        assert!(
            mixed
                .iter()
                .any(|segment| (segment[1] - segment[0]).length() > 40.0),
            "the cut half is still drawn as one segment"
        );
    }

    #[test]
    fn short_rapid_samples_keep_one_dotted_rhythm() {
        // 1.6 px hops: shorter than a dash. Per-hop dots would paint every hop
        // solid; the continuous rhythm leaves gaps.
        let trail: Vec<TrailPoint> = (0..40)
            .map(|i| hop([-4.0 + i as f64 * 0.2, 0.0, 0.0], true))
            .collect();
        let shapes = trail_shapes(&trail, MachineVariant::Mill, ViewMode::TopXy);
        assert!(shapes.len() > 5, "the run is dotted: {}", shapes.len());
        let covered: f32 = shapes
            .iter()
            .map(|segment| (segment[1] - segment[0]).length())
            .sum();
        let span = (shapes.last().unwrap()[1] - shapes[0][0]).length();
        assert!(
            covered < span * 0.6,
            "short hops keep gaps instead of painting solid: {covered} of {span}"
        );
    }

    #[test]
    fn the_lathe_trail_uses_the_same_rapid_pattern() {
        let trail = [hop([0.0, 0.0, -40.0], true), hop([0.0, 0.0, 40.0], true)];
        let shapes = trail_shapes(&trail, MachineVariant::Lathe, ViewMode::LatheXz);
        assert!(
            shapes.len() > 5,
            "the turning view dots its rapids too: {}",
            shapes.len()
        );
        let cut = trail_shapes(
            &[hop([0.0, 0.0, -40.0], false), hop([0.0, 0.0, 40.0], false)],
            MachineVariant::Lathe,
            ViewMode::LatheXz,
        );
        assert_eq!(cut.len(), 1, "a lathe cut trail is solid");
    }

    #[test]
    fn the_fit_for_the_demo_program_is_sane() {
        let machine = Bounds3 {
            min: [-367.0, -341.0, -248.0],
            max: [2.0, 1.0, 16.0],
        };
        let program = Bounds3 {
            min: [-250.0, -210.0, -6.0],
            max: [-70.0, -70.0, 50.0],
        };
        let rect = Rect::from_min_max(Pos2::new(205.0, 288.0), Pos2::new(924.0, 499.0));
        let (zoom, pan) = fit_zoom_pan(
            machine,
            program,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        println!("zoom={zoom} pan={pan:?}");
        assert!(zoom > 1.0 && zoom < 10.0, "{zoom}");
        assert!(pan.y.abs() < rect.height(), "{pan:?}");
    }

    #[test]
    fn the_axis_triad_follows_the_orbit() {
        let labels = vec!["X".to_string(), "Y".to_string(), "Z".to_string()];
        // Front view: X across, Z up, Y at the viewer (a circle).
        let triad = axis_triad(ViewMode::SideXz, DEFAULT_YAW, DEFAULT_PITCH, false, &labels);
        assert_eq!(triad.len(), 3);
        let x = triad[0].1.expect("X has a direction");
        assert!(x.x > 0.9, "X points right: {x:?}");
        let z = triad[2].1.expect("Z has a direction");
        assert!(z.y < -0.9, "Z points up: {z:?}");
        assert!(triad[1].1.is_none(), "Y is at the viewer: {:?}", triad[1]);

        // Top view: X right, Y down, Z at the viewer.
        let triad = axis_triad(ViewMode::TopXy, DEFAULT_YAW, DEFAULT_PITCH, false, &labels);
        assert!(triad[0].1.unwrap().x > 0.9);
        assert!(triad[1].1.unwrap().y < -0.9, "+Y points up on screen");
        assert!(triad[2].1.is_none(), "Z is at the viewer: {:?}", triad[2]);

        // ISO: every axis projects, none is a circle.
        let triad = axis_triad(ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH, false, &labels);
        assert!(triad.iter().all(|(_, direction)| direction.is_some()));

        // An axis the machine does not have is not drawn.
        let two = vec!["X".to_string(), "Z".to_string()];
        assert_eq!(
            axis_triad(ViewMode::SideXz, DEFAULT_YAW, DEFAULT_PITCH, false, &two).len(),
            2
        );
    }

    fn projection(mode: ViewMode) -> Projection {
        Projection::new(
            Bounds3 {
                min: [-100.0, -100.0, -100.0],
                max: [100.0, 100.0, 100.0],
            },
            Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0)),
            mode,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            1.0,
            Vec2::ZERO,
        )
    }

    #[test]
    fn a_side_view_has_one_axis_at_the_viewer() {
        assert_eq!(view_axis(&projection(ViewMode::SideYz)), Some('X'));
        assert_eq!(view_axis(&projection(ViewMode::SideXz)), Some('Y'));
        assert_eq!(view_axis(&projection(ViewMode::TopXy)), Some('Z'));
        assert_eq!(view_axis(&projection(ViewMode::Iso3d)), None);
    }

    #[test]
    fn an_edge_on_label_rolls_to_face_the_viewer() {
        let view = projection(ViewMode::SideYz);
        // The Y length label reads along Y with its tops pointing into the
        // screen (-X). Below the drawing it must roll to +Z and move its
        // baseline down, so upright letters sit under the line.
        let (up, shift) = rolled_up(&view, [0.0, 0.0, -100.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]);
        assert_eq!(up, [0.0, 0.0, 1.0]);
        assert!(shift, "the label is below the drawing: {shift}");

        // The Z length label reads up the left side. It rolls to -Y (left) and
        // already points away from the drawing, so it needs no extra shift.
        let (up, shift) = rolled_up(&view, [-100.0, 0.0, 0.0], [0.0, 0.0, 1.0], [-1.0, 0.0, 0.0]);
        assert_eq!(up, [0.0, -1.0, 0.0]);
        assert!(!shift, "the label is left of the drawing: {shift}");
    }

    #[test]
    fn the_lathe_z_labels_sit_on_the_minus_x_side_of_the_line() {
        let anchor = lathe_z_label_anchor(10.0, 4.0, 1.5, -2.0, 0.25);
        assert_eq!(anchor, [8.5, 4.0, -1.75]);
        assert!(anchor[0] < 10.0, "labels clear the line toward -X");
    }

    #[test]
    fn the_lathe_z_labels_read_up_the_screen() {
        // The lathe turns X vertical: a label whose glyph tops run along -X
        // (the mill's plane) is upside down there, so the lathe picks the X
        // direction that reads up the screen instead.
        let lathe = projection(ViewMode::LatheXz);
        let up = z_label_up(&lathe, MachineVariant::Lathe);
        assert!(
            screen_direction(&lathe, up).y < 0.0,
            "glyph tops point up the screen: {up:?}"
        );
        assert_eq!(up, [1.0, 0.0, 0.0], "the lathe reads Z with +X as tops");
        // The baseline runs along Z, to the right: a proper, unmirrored frame.
        let right = screen_direction(&lathe, [0.0, 0.0, 1.0]);
        let up_screen = screen_direction(&lathe, up);
        assert!(right.x > 0.9, "the label reads along +Z: {right:?}");
        assert!(
            right.x * up_screen.y - right.y * up_screen.x < 0.0,
            "the label plane is not mirrored"
        );

        // The mirror turns X over, so the same case must follow it.
        let mirrored = projection(ViewMode::LatheXz).mirrored(true);
        let up = z_label_up(&mirrored, MachineVariant::Lathe);
        assert_eq!(up, [-1.0, 0.0, 0.0]);
        assert!(
            screen_direction(&mirrored, up).y < 0.0,
            "still upright when the picture is mirrored"
        );

        // The mill keeps the XZ plane it has always used.
        let mill = projection(ViewMode::Iso3d);
        assert_eq!(z_label_up(&mill, MachineVariant::Mill), [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn the_view_cycle_runs_iso_x_y_z() {
        use MachineVariant::Mill;
        assert_eq!(cycle_view(ViewMode::Iso3d, Mill), ViewMode::SideYz);
        assert_eq!(cycle_view(ViewMode::SideYz, Mill), ViewMode::SideXz);
        assert_eq!(cycle_view(ViewMode::SideXz, Mill), ViewMode::TopXy);
        assert_eq!(cycle_view(ViewMode::TopXy, Mill), ViewMode::Iso3d);
        assert_eq!(cycle_view(ViewMode::LatheXz, Mill), ViewMode::Iso3d);

        // The lathe has one flat view.
        assert_eq!(
            cycle_view(ViewMode::Iso3d, MachineVariant::Lathe),
            ViewMode::LatheXz
        );
        assert_eq!(
            cycle_view(ViewMode::LatheXz, MachineVariant::Lathe),
            ViewMode::Iso3d
        );
    }

    #[test]
    fn the_axis_views_look_along_one_axis_each() {
        // Looking along X: Y is horizontal, Z is vertical.
        let (x, y) = view_xy(
            [1.0, 2.0, 3.0],
            ViewMode::SideYz,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!((x - 2.0).abs() < 1e-6 && (y + 3.0).abs() < 1e-6, "{x},{y}");
        // Looking along Y: X is horizontal, Z is vertical.
        let (x, y) = view_xy(
            [1.0, 2.0, 3.0],
            ViewMode::SideXz,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!((x - 1.0).abs() < 1e-6 && (y + 3.0).abs() < 1e-6, "{x},{y}");
        // Looking down Z: X is horizontal, Y is vertical (downwards).
        let (x, y) = view_xy(
            [1.0, 2.0, 3.0],
            ViewMode::TopXy,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!((x - 1.0).abs() < 1e-6 && (y + 2.0).abs() < 1e-6, "{x},{y}");
    }

    #[test]
    fn the_lathe_mirror_flips_x_about_the_z_centreline() {
        // `(Z, -X)` unmirrored, `(Z, +X)` mirrored: Z is the horizontal
        // coordinate either way, and X only changes which side it lands on.
        let point = [5.0f32, 0.0, 20.0];
        let (x, y) = view_xy(point, ViewMode::LatheXz, DEFAULT_YAW, DEFAULT_PITCH, false);
        assert!((x - 20.0).abs() < 1e-6 && (y + 5.0).abs() < 1e-6, "{x},{y}");
        let (x, y) = view_xy(point, ViewMode::LatheXz, DEFAULT_YAW, DEFAULT_PITCH, true);
        assert!((x - 20.0).abs() < 1e-6 && (y - 5.0).abs() < 1e-6, "{x},{y}");

        // The camera carries the flag, so everything drawn through it flips
        // together: the same point, one sign about the card's centre line.
        let plain = projection(ViewMode::LatheXz);
        let mirrored = projection(ViewMode::LatheXz).mirrored(true);
        let tip = [12.0, 0.0, 0.0];
        let center = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0)).center();
        assert!(plain.project(tip).y < center.y, "+X above the centreline");
        assert!(mirrored.project(tip).y > center.y, "+X below it, mirrored");
        assert!(
            (plain.project(tip).x - mirrored.project(tip).x).abs() < 1e-3,
            "the mirror leaves Z alone"
        );
        // A mill view has no centreline: the flag changes nothing there.
        assert_eq!(
            projection(ViewMode::SideXz).project(tip),
            projection(ViewMode::SideXz).mirrored(true).project(tip)
        );
    }

    #[test]
    fn a_mirrored_lathe_view_still_frames_the_program() {
        let machine = Bounds3 {
            min: [-200.0, -200.0, -400.0],
            max: [200.0, 200.0, 0.0],
        };
        let program = Bounds3 {
            min: [0.0, -10.0, -120.0],
            max: [40.0, 10.0, -20.0],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        for mirror in [false, true] {
            let (zoom, pan) = fit_zoom_pan(
                machine,
                program,
                rect,
                ViewMode::LatheXz,
                DEFAULT_YAW,
                DEFAULT_PITCH,
                mirror,
            );
            let camera = Projection::new(
                machine,
                rect,
                ViewMode::LatheXz,
                DEFAULT_YAW,
                DEFAULT_PITCH,
                zoom,
                pan,
            )
            .mirrored(mirror);
            for x in [program.min[0], program.max[0]] {
                for z in [program.min[2], program.max[2]] {
                    let pos = camera.project([x, 0.0, z]);
                    assert!(
                        rect.contains(pos),
                        "mirror {mirror}: {pos:?} outside {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_fit_frames_a_small_inch_part_not_the_padding() {
        // The lathe screw: X 0.2..0.5, Z 0..2.1 inches against an envelope of
        // a few inches. A fixed one-unit clearance per side used to be most of
        // the part, so the fit left the toolpath in the middle third of the
        // card. The program must cover most of the width it is fitted into.
        let machine = Bounds3 {
            min: [-6.0, -6.0, -6.0],
            max: [6.0, 6.0, 12.0],
        };
        let program = Bounds3 {
            min: [0.2, -0.05, 0.0],
            max: [0.5, 0.05, 2.1],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 600.0));
        let (zoom, pan) = fit_zoom_pan(
            machine,
            program,
            rect,
            ViewMode::LatheXz,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        let projection = Projection::new(
            machine,
            rect,
            ViewMode::LatheXz,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            zoom,
            pan,
        );
        let start = projection.project([0.2, 0.0, 0.0]);
        let end = projection.project([0.5, 0.0, 2.1]);
        let filled = (end.x - start.x).abs() / rect.width();
        assert!(filled > 0.65, "the part fills {filled:.2} of the width");
        // And it stays inside the card.
        for point in [start, end] {
            assert!(rect.contains(point), "{point:?} outside {rect:?}");
        }
    }

    #[test]
    fn a_fit_frames_the_program_inside_the_rect() {
        // The program box alone, projected in the current view, must land
        // inside the card: that is what puts most of the toolpath in the window.
        let machine = Bounds3 {
            min: [-500.0, -320.0, -250.0],
            max: [0.0, 0.0, 0.0],
        };
        let program = Bounds3 {
            min: [-300.0, -200.0, -40.0],
            max: [-60.0, -40.0, 10.0],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        for mode in [
            ViewMode::Iso3d,
            ViewMode::SideYz,
            ViewMode::SideXz,
            ViewMode::TopXy,
        ] {
            let (zoom, pan) = fit_zoom_pan(
                machine,
                program,
                rect,
                mode,
                DEFAULT_YAW,
                DEFAULT_PITCH,
                false,
            );
            let projection =
                Projection::new(machine, rect, mode, DEFAULT_YAW, DEFAULT_PITCH, zoom, pan);
            for corner in 0..8 {
                let point = [
                    if corner & 1 == 0 {
                        program.min[0]
                    } else {
                        program.max[0]
                    },
                    if corner & 2 == 0 {
                        program.min[1]
                    } else {
                        program.max[1]
                    },
                    if corner & 4 == 0 {
                        program.min[2]
                    } else {
                        program.max[2]
                    },
                ];
                let pos = projection.project(point);
                assert!(rect.contains(pos), "{mode:?}: {pos:?} outside {rect:?}");
            }
        }
    }

    #[test]
    fn the_iso_view_puts_y_into_the_screen() {
        // LinuxCNC puts +Y to the back of the machine. From the default orbit
        // it must therefore run up the screen, away from the operator, and to
        // the right; +X runs right and towards the viewer, +Z straight up.
        let (x, y) = view_xy(
            [1.0, 0.0, 0.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(x > 0.5 && y > 0.0, "+X is right and near: {x},{y}");
        let (x, y) = view_xy(
            [0.0, 1.0, 0.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(x > 0.5 && y < 0.0, "+Y goes right and away: {x},{y}");
        let (x, y) = view_xy(
            [0.0, 0.0, 1.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(x.abs() < 1e-6 && y < 0.0, "+Z is straight up: {x},{y}");
        // Looking from the front (+Y away) means the near corner of a box is
        // the min-Y one: it sits lowest on the screen.
        let near = view_xy(
            [0.0, -1.0, 0.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        let far = view_xy(
            [0.0, 1.0, 0.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(near.1 > far.1, "min Y is the near side: {near:?} {far:?}");
    }

    #[test]
    fn the_orbit_turns_the_view_about_the_vertical_axis() {
        let point = [1.0f32, 0.0, 0.0];
        let (x0, y0) = view_xy(point, ViewMode::Iso3d, 0.0, 0.0, false);
        assert!((x0 - 1.0).abs() < 1e-6, "{x0}");
        assert!(y0.abs() < 1e-6, "{y0}");

        // A quarter turn puts the X axis exactly into the depth, so it stops
        // contributing to the screen position.
        let (x90, _) = view_xy(
            point,
            ViewMode::Iso3d,
            std::f32::consts::FRAC_PI_2,
            0.0,
            false,
        );
        assert!(x90.abs() < 1e-6, "{x90}");

        // +Z is up on screen, and raising the camera flattens it.
        let (_, up) = view_xy(
            [0.0, 0.0, 1.0],
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(up < 0.0, "z must project upwards, got {up}");
        let (_, flat) = view_xy([0.0, 0.0, 1.0], ViewMode::Iso3d, DEFAULT_YAW, 0.0, false);
        assert!(flat.abs() > up.abs(), "{flat} vs {up}");
    }

    #[test]
    fn nice_steps_are_round_numbers() {
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(0.7), 1.0);
        assert_eq!(nice_step(270.0), 500.0);
    }

    #[test]
    fn fitting_the_reference_box_is_identity() {
        let envelope = Bounds3 {
            min: [-500.0, -320.0, -250.0],
            max: [0.0, 0.0, 0.0],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let (zoom, pan) = fit_zoom_pan(
            envelope,
            envelope,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!((zoom - 1.0).abs() < 1e-4, "zoom {zoom}");
        assert!(pan.length() < 1e-3, "pan {pan:?}");
    }

    #[test]
    fn fit_all_frames_the_tool_and_the_markers() {
        let mut preview = Preview::empty();
        preview.bounds = Some(Bounds3 {
            min: [-30.0, -20.0, -10.0],
            max: [-10.0, 0.0, 0.0],
        });
        let reference = Bounds3 {
            min: [-500.0, -320.0, -250.0],
            max: [0.0, 0.0, 0.0],
        };
        let mut view = TrajectoryView::new(&preview, &[], MachineVariant::Mill);

        let program_only = fit_target(&view, reference);
        assert_eq!(program_only.min[0], -30.0);
        assert_eq!(program_only.max[0], -10.0);

        view.fit_all = true;
        view.tool_tip = Some([200.0, 0.0, 0.0]);
        view.origin = [0.0, -200.0, 0.0];
        view.highlight_origin = Some([0.0, 0.0, 100.0]);
        let all = fit_target(&view, reference);
        assert_eq!(all.max[0], 200.0, "the tool is inside the fit");
        assert_eq!(all.min[1], -200.0, "the work zero is inside the fit");
        assert_eq!(
            all.max[2], 100.0,
            "the highlighted system is inside the fit"
        );
        assert_eq!(all.min[0], -30.0, "the program is still inside the fit");
    }

    #[test]
    fn the_tether_frames_the_machine_origin() {
        let mut preview = Preview::empty();
        preview.bounds = Some(Bounds3 {
            min: [100.0, 100.0, 0.0],
            max: [110.0, 110.0, 10.0],
        });
        let reference = Bounds3 {
            min: [-500.0, -320.0, -250.0],
            max: [0.0, 0.0, 0.0],
        };
        let mut view = TrajectoryView::new(&preview, &[], MachineVariant::Mill);
        view.fit_all = true;
        view.origin = [100.0, 100.0, 0.0];
        assert_eq!(fit_target(&view, reference).min[0], 100.0);

        // The tether is drawn from machine zero, so zero must be in frame.
        view.origin_tether = true;
        let target = fit_target(&view, reference);
        assert_eq!(target.min[0], 0.0);
        assert_eq!(target.min[1], 0.0);
    }

    #[test]
    fn fitting_a_program_centres_it_without_moving_the_reference() {
        let envelope = Bounds3 {
            min: [-500.0, -320.0, -250.0],
            max: [0.0, 0.0, 0.0],
        };
        // A small part inside the envelope: the fit must zoom in on it.
        let program = Bounds3 {
            min: [-300.0, -200.0, -40.0],
            max: [-240.0, -140.0, -20.0],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let (zoom, pan) = fit_zoom_pan(
            envelope,
            program,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            false,
        );
        assert!(zoom > 1.5, "zoom {zoom}");
        let fitted = Projection::new(
            envelope,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            zoom,
            pan,
        );
        let centre = fitted.project(program.center());
        assert!((centre.x - rect.center().x).abs() < 0.5, "{centre:?}");
        assert!((centre.y - rect.center().y).abs() < 0.5, "{centre:?}");
        // The reference is still the envelope, so the same point projects to the
        // same place whatever is loaded: that is what keeps the markers still.
        let unchanged = Projection::new(
            envelope,
            rect,
            ViewMode::Iso3d,
            DEFAULT_YAW,
            DEFAULT_PITCH,
            zoom,
            pan,
        );
        assert_eq!(
            fitted.project([-54.35, -49.3, -27.15]),
            unchanged.project([-54.35, -49.3, -27.15])
        );
    }
}
