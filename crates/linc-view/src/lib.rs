//! 2D/3D toolpath and sparkline rendering for LiNC.
//!
//! Projection is CPU-side into egui's painter. No GPU buffers, no animation
//! loop and no machine access.

const FONT_SCALE: f32 = 1.25;

pub(crate) mod hershey;

use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use linc_core::gcode::{ArcSpec, Bounds3, PathSegment, Plane, Preview, SegmentKind};
use linc_core::model::MachineVariant;

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
    pub live_trail: &'a [[f64; 3]],
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
    pub fn new(preview: &'a Preview, live_trail: &'a [[f64; 3]], variant: MachineVariant) -> Self {
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
            selected_line_color: theme::CURRENT,
            height: 400.0,
            fit: false,
            auto_fit: false,
            axis_labels: vec!["X".into(), "Y".into(), "Z".into()],
        }
    }
}

struct Projection {
    min: [f64; 3],
    max: [f64; 3],
    rect: Rect,
    scale: f32,
    zoom: f32,
    pan: Vec2,
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
}

/// The screen-plane coordinates of a point in the view's frame.
///
/// The 3D view is an orthographic camera: `yaw` turns it about the vertical
/// axis (0 looks from the front, -Y, towards +Y), `pitch` raises it above the
/// XY plane. Screen y grows downwards, so the vertical component is negated —
/// the opposite sign drew +Z downwards.
fn view_xy(local: [f32; 3], mode: ViewMode, yaw: f32, pitch: f32) -> (f32, f32) {
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
        ViewMode::LatheXz => (local[2], -local[0]),
    }
}

impl Projection {
    fn new(
        bounds: Bounds3,
        rect: Rect,
        mode: ViewMode,
        yaw: f32,
        pitch: f32,
        zoom: f32,
        pan: Vec2,
    ) -> Self {
        let expanded = bounds.expanded(1.0);
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
                if corner & 1 == 0 { expanded.min[0] } else { expanded.max[0] },
                if corner & 2 == 0 { expanded.min[1] } else { expanded.max[1] },
                if corner & 4 == 0 { expanded.min[2] } else { expanded.max[2] },
            ];
            let local = [
                (point[0] - center[0]) as f32,
                (point[1] - center[1]) as f32,
                (point[2] - center[2]) as f32,
            ];
            let (x, y) = view_xy(local, mode, yaw, pitch);
            min = [min[0].min(x), min[1].min(y)];
            max = [max[0].max(x), max[1].max(y)];
        }
        let world_w = (max[0] - min[0]).max(1.0);
        let world_h = (max[1] - min[1]).max(1.0);
        let scale =
            ((rect.width() / world_w).min(rect.height() / world_h) * 0.82).max(0.01);
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
        }
    }

    fn project(&self, point: [f64; 3]) -> Pos2 {
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
        let (x, y) = view_xy(local, self.mode, self.yaw, self.pitch);
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
        position += dash + gap;
    }
}

fn arc_points(segment: &PathSegment, arc: &ArcSpec) -> Vec<[f64; 3]> {
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
) -> (f32, Vec2) {
    let target_projection = Projection::new(target, rect, mode, yaw, pitch, 1.0, Vec2::ZERO);
    let base = Projection::new(reference, rect, mode, yaw, pitch, 1.0, Vec2::ZERO);
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
        target.include(*point);
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
        vec![tip, back + side * (barb * 0.45), back - side * (barb * 0.45)],
        colour,
        Stroke::NONE,
    ));
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
        bounds.include(*point);
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
        );
        view.zoom = zoom;
        view.pan = pan;
    }
    let projection = Projection::new(
        reference,
        rect.shrink(12.0),
        view.view,
        view.yaw,
        view.pitch,
        view.zoom,
        view.pan,
    );

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
        for segment in &view.preview.segments {
            let is_selected = view.show_selected_line && view.selected_line == Some(segment.line);
            let stroke = if is_selected {
                Stroke::new(2.6, view.selected_line_color)
            } else if segment.kind == SegmentKind::Rapid {
                Stroke::new(1.0, theme::MUTED)
            } else {
                Stroke::new(1.8, theme::ACCENT)
            };
            if let Some(arc) = &segment.arc {
                let points = arc_points(segment, arc);
                let dotted = segment.kind == SegmentKind::Rapid;
                for pair in points.windows(2) {
                    Projection::draw_line(
                        &painter,
                        projection.project(pair[0]),
                        projection.project(pair[1]),
                        stroke,
                        dotted,
                    );
                }
            } else {
                Projection::draw_line(
                    &painter,
                    projection.project(segment.start),
                    projection.project(segment.end),
                    stroke,
                    segment.kind == SegmentKind::Rapid,
                );
            }
            if is_selected && !tagged_lines.contains(&segment.line) {
                tagged_lines.push(segment.line);
                // Mark the middle of the move, not its end: the end of one
                // block is the start of the next, and a tag sitting on that
                // joint looked like it labelled the following move.
                let tag = if let Some(arc) = &segment.arc {
                    let points = arc_points(segment, arc);
                    projection.project(
                        points
                            .get(points.len() / 2)
                            .copied()
                            .unwrap_or(segment.end),
                    )
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
            draw_bounds_dimensions(&painter, &projection, program, view.work_offset);
        }
    }

    // Live trail, always distinct from the loaded program.
    if view.show_live_trail && view.live_trail.len() >= 2 {
        let points: Vec<Pos2> = view
            .live_trail
            .iter()
            .map(|p| projection.project(*p))
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(2.0, theme::SUCCESS)));
    }
    // Live tool position. Amber, matching the accepted mockup, so the toolhead
    // is distinguishable from the green live trail it sits on.
    if let Some(tip) = view.tool_tip.or_else(|| view.live_trail.last().copied()) {
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
        let (dx, dy) = view_xy(unit, mode, yaw, pitch);
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
/// is drawn as a circle instead.
fn draw_axis_triad(
    painter: &Painter,
    rect: Rect,
    mode: ViewMode,
    yaw: f32,
    pitch: f32,
    labels: &[String],
) {
    let center = rect.right_top() + Vec2::new(-46.0, 42.0);
    let length = 18.0;
    let font = FontId::monospace(9.0 * FONT_SCALE);
    for (label, direction) in axis_triad(mode, yaw, pitch, labels) {
        match direction {
            Some(direction) => {
                let tip = center + direction * length;
                painter.line_segment([center, tip], Stroke::new(1.4, theme::MUTED));
                // Arrowhead: two short legs back from the tip.
                let side = Vec2::new(-direction.y, direction.x) * 3.0;
                let back = direction * 5.0;
                painter.line_segment(
                    [tip, tip - back + side],
                    Stroke::new(1.4, theme::MUTED),
                );
                painter.line_segment(
                    [tip, tip - back - side],
                    Stroke::new(1.4, theme::MUTED),
                );
                painter.text(
                    tip + direction * 7.0,
                    Align2::CENTER_CENTER,
                    label,
                    font.clone(),
                    theme::MUTED,
                );
            }
            None => {
                // Pointing at the viewer: a circle with a dot.
                painter.circle_stroke(center, 4.0, Stroke::new(1.2, theme::MUTED));
                painter.circle_filled(center, 1.2, theme::MUTED);
                painter.text(
                    center + Vec2::new(9.0, 0.0),
                    Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    theme::MUTED,
                );
            }
        }
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
        max: [max[0] + margin, max[1] + margin, max[2].max(min[2]) + margin],
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
    put_label(
        painter,
        projection,
        [min[0] + size[0] * 0.5, max[1] + gap + row, min[2]],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        glyph,
        &format!("{:.2}", size[0]),
        color,
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
    );

    // Y: outside the left edge, reading along Y.
    dimension_line(
        painter,
        projection,
        [min[0], max[1], min[2]],
        [min[0], min[1], min[2]],
        [-gap, 0.0, 0.0],
        color,
    );
    put_label(
        painter,
        projection,
        [min[0] - gap - row, max[1] - size[1] * 0.5, min[2]],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        glyph,
        &format!("{:.2}", size[1]),
        color,
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
    );

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
    );
}

/// Width of `text` at `glyph` height, in model units.
fn text_width(text: &str, glyph: f64) -> f64 {
    hershey::string_len(text) as f64 * glyph / 440.0
}

/// Width of `text` at `glyph` height, in model units.
fn put_label(
    painter: &Painter,
    projection: &Projection,
    center: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    glyph: f64,
    text: &str,
    color: Color32,
) {
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
    let oa = projection.project([from[0] + offset[0], from[1] + offset[1], from[2] + offset[2]]);
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
        painter.add(egui::Shape::convex_polygon(
            face,
            fill,
            Stroke::NONE,
        ));
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
                        ([bounds.min[0], value, bounds.min[2]], [bounds.min[0], value, bounds.max[2]])
                    } else {
                        ([bounds.min[0], bounds.min[1], value], [bounds.max[0], bounds.min[1], value])
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
        let (dx, dy) = view_xy(unit, projection.mode, projection.yaw, projection.pitch);
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
pub fn draw_mini_xy(ui: &mut Ui, preview: &Preview, live: &[[f64; 3]], height: f32) -> Response {
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
        let projection = Projection::new(bounds, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH, 1.0, Vec2::ZERO);
        for point in [
            [0.0, 0.0, 0.0],
            [100.0, 100.0, 100.0],
            [-100.0, 100.0, -100.0],
        ] {
            let pos = projection.project(point);
            assert!(rect.contains(pos), "{pos:?} outside {rect:?}");
        }
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
        let (zoom, pan) =
            fit_zoom_pan(machine, program, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        println!("zoom={zoom} pan={pan:?}");
        assert!(zoom > 1.0 && zoom < 10.0, "{zoom}");
        assert!(pan.y.abs() < rect.height(), "{pan:?}");
    }

    #[test]
    fn the_axis_triad_follows_the_orbit() {
        let labels = vec!["X".to_string(), "Y".to_string(), "Z".to_string()];
        // Front view: X across, Z up, Y at the viewer (a circle).
        let triad = axis_triad(ViewMode::SideXz, DEFAULT_YAW, DEFAULT_PITCH, &labels);
        assert_eq!(triad.len(), 3);
        let x = triad[0].1.expect("X has a direction");
        assert!(x.x > 0.9, "X points right: {x:?}");
        let z = triad[2].1.expect("Z has a direction");
        assert!(z.y < -0.9, "Z points up: {z:?}");
        assert!(triad[1].1.is_none(), "Y is at the viewer: {:?}", triad[1]);

        // Top view: X right, Y down, Z at the viewer.
        let triad = axis_triad(ViewMode::TopXy, DEFAULT_YAW, DEFAULT_PITCH, &labels);
        assert!(triad[0].1.unwrap().x > 0.9);
        assert!(triad[1].1.unwrap().y < -0.9, "+Y points up on screen");
        assert!(triad[2].1.is_none(), "Z is at the viewer: {:?}", triad[2]);

        // ISO: every axis projects, none is a circle.
        let triad = axis_triad(ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH, &labels);
        assert!(triad.iter().all(|(_, direction)| direction.is_some()));

        // An axis the machine does not have is not drawn.
        let two = vec!["X".to_string(), "Z".to_string()];
        assert_eq!(axis_triad(ViewMode::SideXz, DEFAULT_YAW, DEFAULT_PITCH, &two).len(), 2);
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
        assert_eq!(cycle_view(ViewMode::Iso3d, MachineVariant::Lathe), ViewMode::LatheXz);
        assert_eq!(cycle_view(ViewMode::LatheXz, MachineVariant::Lathe), ViewMode::Iso3d);
    }

    #[test]
    fn the_axis_views_look_along_one_axis_each() {
        // Looking along X: Y is horizontal, Z is vertical.
        let (x, y) = view_xy([1.0, 2.0, 3.0], ViewMode::SideYz, DEFAULT_YAW, DEFAULT_PITCH);
        assert!((x - 2.0).abs() < 1e-6 && (y + 3.0).abs() < 1e-6, "{x},{y}");
        // Looking along Y: X is horizontal, Z is vertical.
        let (x, y) = view_xy([1.0, 2.0, 3.0], ViewMode::SideXz, DEFAULT_YAW, DEFAULT_PITCH);
        assert!((x - 1.0).abs() < 1e-6 && (y + 3.0).abs() < 1e-6, "{x},{y}");
        // Looking down Z: X is horizontal, Y is vertical (downwards).
        let (x, y) = view_xy([1.0, 2.0, 3.0], ViewMode::TopXy, DEFAULT_YAW, DEFAULT_PITCH);
        assert!((x - 1.0).abs() < 1e-6 && (y + 2.0).abs() < 1e-6, "{x},{y}");
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
            let (zoom, pan) =
                fit_zoom_pan(machine, program, rect, mode, DEFAULT_YAW, DEFAULT_PITCH);
            let projection =
                Projection::new(machine, rect, mode, DEFAULT_YAW, DEFAULT_PITCH, zoom, pan);
            for corner in 0..8 {
                let point = [
                    if corner & 1 == 0 { program.min[0] } else { program.max[0] },
                    if corner & 2 == 0 { program.min[1] } else { program.max[1] },
                    if corner & 4 == 0 { program.min[2] } else { program.max[2] },
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
        let (x, y) = view_xy([1.0, 0.0, 0.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(x > 0.5 && y > 0.0, "+X is right and near: {x},{y}");
        let (x, y) = view_xy([0.0, 1.0, 0.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(x > 0.5 && y < 0.0, "+Y goes right and away: {x},{y}");
        let (x, y) = view_xy([0.0, 0.0, 1.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(x.abs() < 1e-6 && y < 0.0, "+Z is straight up: {x},{y}");
        // Looking from the front (+Y away) means the near corner of a box is
        // the min-Y one: it sits lowest on the screen.
        let near = view_xy([0.0, -1.0, 0.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        let far = view_xy([0.0, 1.0, 0.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(near.1 > far.1, "min Y is the near side: {near:?} {far:?}");
    }

    #[test]
    fn the_orbit_turns_the_view_about_the_vertical_axis() {
        let point = [1.0f32, 0.0, 0.0];
        let (x0, y0) = view_xy(point, ViewMode::Iso3d, 0.0, 0.0);
        assert!((x0 - 1.0).abs() < 1e-6, "{x0}");
        assert!(y0.abs() < 1e-6, "{y0}");

        // A quarter turn puts the X axis exactly into the depth, so it stops
        // contributing to the screen position.
        let (x90, _) = view_xy(point, ViewMode::Iso3d, std::f32::consts::FRAC_PI_2, 0.0);
        assert!(x90.abs() < 1e-6, "{x90}");

        // +Z is up on screen, and raising the camera flattens it.
        let (_, up) = view_xy([0.0, 0.0, 1.0], ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(up < 0.0, "z must project upwards, got {up}");
        let (_, flat) = view_xy([0.0, 0.0, 1.0], ViewMode::Iso3d, DEFAULT_YAW, 0.0);
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
        let (zoom, pan) = fit_zoom_pan(envelope, envelope, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
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
        assert_eq!(all.max[2], 100.0, "the highlighted system is inside the fit");
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
        let (zoom, pan) =
            fit_zoom_pan(envelope, program, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH);
        assert!(zoom > 1.5, "zoom {zoom}");
        let fitted = Projection::new(envelope, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH, zoom, pan);
        let centre = fitted.project(program.center());
        assert!((centre.x - rect.center().x).abs() < 0.5, "{centre:?}");
        assert!((centre.y - rect.center().y).abs() < 0.5, "{centre:?}");
        // The reference is still the envelope, so the same point projects to the
        // same place whatever is loaded: that is what keeps the markers still.
        let unchanged =
            Projection::new(envelope, rect, ViewMode::Iso3d, DEFAULT_YAW, DEFAULT_PITCH, zoom, pan);
        assert_eq!(
            fitted.project([-54.35, -49.3, -27.15]),
            unchanged.project([-54.35, -49.3, -27.15])
        );
    }
}
