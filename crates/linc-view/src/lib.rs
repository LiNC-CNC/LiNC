//! 2D/3D toolpath and sparkline rendering for LiNC.
//!
//! Projection is CPU-side into egui's painter. No GPU buffers, no animation
//! loop and no machine access.

const FONT_SCALE: f32 = 1.25;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Iso3d,
    TopXy,
    SideXz,
    LatheXz,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Iso3d => "3D / ISO",
            Self::TopXy => "XY / TOP",
            Self::SideXz => "XZ / SIDE",
            Self::LatheXz => "XZ / TURN",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrajectoryView<'a> {
    pub preview: &'a Preview,
    pub live_trail: &'a [[f64; 3]],
    pub tool_tip: Option<[f64; 3]>,
    pub machine_bounds: Option<Bounds3>,
    pub selected_line: Option<usize>,
    pub current_line: Option<usize>,
    pub variant: MachineVariant,
    pub view: ViewMode,
    pub zoom: f32,
    pub pan: Vec2,
    pub show_grid: bool,
    pub show_live_trail: bool,
    pub show_program_bounds: bool,
    pub show_machine_bounds: bool,
    pub show_selected_line: bool,
    pub selected_line_color: Color32,
    pub height: f32,
    pub fit: bool,
    pub axis_labels: Vec<String>,
}

impl<'a> TrajectoryView<'a> {
    pub fn new(preview: &'a Preview, live_trail: &'a [[f64; 3]], variant: MachineVariant) -> Self {
        Self {
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
            show_grid: true,
            show_live_trail: true,
            show_program_bounds: true,
            show_machine_bounds: true,
            show_selected_line: true,
            selected_line_color: theme::ACCENT,
            height: 400.0,
            fit: false,
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
}

impl Projection {
    fn new(bounds: Bounds3, rect: Rect, mode: ViewMode, zoom: f32, pan: Vec2) -> Self {
        let expanded = bounds.expanded(1.0);
        let size = expanded.size();
        let (world_w, world_h) = match mode {
            ViewMode::Iso3d => {
                let w = (size[0] + size[1]) * 0.866;
                let h = (size[0] + size[1]) * 0.5 + size[2];
                (w.max(1.0), h.max(1.0))
            }
            ViewMode::TopXy => (size[0].max(1.0), size[1].max(1.0)),
            ViewMode::SideXz | ViewMode::LatheXz => (size[2].max(1.0), size[0].max(1.0)),
        };
        let scale =
            ((rect.width() / world_w as f32).min(rect.height() / world_h as f32) * 0.82).max(0.01);
        Self {
            min: expanded.min,
            max: expanded.max,
            rect,
            scale,
            zoom: zoom.clamp(0.05, 40.0),
            pan,
            mode,
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
        let (x, y) = match self.mode {
            ViewMode::Iso3d => {
                let sx = (local[0] - local[1]) * 0.866;
                let sy = (local[0] + local[1]) * 0.5 - local[2];
                (sx, -sy)
            }
            ViewMode::TopXy => (local[0], -local[1]),
            ViewMode::SideXz | ViewMode::LatheXz => (local[2], -local[0]),
        };
        let center_screen = self.rect.center() + self.pan;
        Pos2::new(
            center_screen.x + x * self.scale * self.zoom,
            center_screen.y + y * self.scale * self.zoom,
        )
    }

    fn draw_line(painter: &Painter, start: Pos2, end: Pos2, stroke: Stroke, dashed: bool) {
        if dashed {
            draw_dashed(painter, start, end, stroke, 6.0, 4.0);
        } else {
            painter.line_segment([start, end], stroke);
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

/// Draw the toolpath graph. `TrajectoryView::fit` requests a one-shot fit;
/// zoom/pan are mutated in place so the caller can persist them.
pub fn draw_trajectory(ui: &mut Ui, view: &mut TrajectoryView<'_>) -> Response {
    let desired = Vec2::new(ui.available_width(), view.height.max(120.0));
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
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        view.pan += response.drag_delta();
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
    if !has_data {
        bounds = Bounds3 {
            min: [-10.0, -10.0, -10.0],
            max: [10.0, 10.0, 10.0],
        };
    }
    if view.fit {
        view.zoom = 1.0;
        view.pan = Vec2::ZERO;
        view.fit = false;
    }
    let projection = Projection::new(bounds, rect.shrink(12.0), view.view, view.zoom, view.pan);

    // Grid and axis labels.
    if view.show_grid {
        draw_grid(&painter, &projection, bounds);
    }
    draw_axes(&painter, &projection, view.variant, &view.axis_labels);

    // Machine bounds.
    if view.show_machine_bounds {
        if let Some(machine) = view.machine_bounds {
            draw_bounds(&painter, &projection, machine, theme::MUTED, 1.0, true);
            if let Some(label) = view_label(view.view, machine) {
                painter.text(
                    projection.project(label.0),
                    Align2::LEFT_TOP,
                    "MACHINE",
                    FontId::monospace(10.0 * FONT_SCALE),
                    theme::MUTED,
                );
            }
        }
    }

    // Program cut and rapid paths.
    if has_data {
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
                for pair in points.windows(2) {
                    Projection::draw_line(
                        &painter,
                        projection.project(pair[0]),
                        projection.project(pair[1]),
                        stroke,
                        false,
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
            if is_selected {
                let tag = projection.project(segment.end);
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
            draw_bounds(&painter, &projection, program, theme::CYAN, 1.0, true);
            if let Some(label) = view_label(view.view, program) {
                painter.text(
                    projection.project(label.0),
                    Align2::LEFT_BOTTOM,
                    "PROGRAM",
                    FontId::monospace(10.0 * FONT_SCALE),
                    theme::CYAN,
                );
            }
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
    if let Some(tip) = view.tool_tip.or_else(|| view.live_trail.last().copied()) {
        let pos = projection.project(tip);
        painter.circle_stroke(pos, 6.0, Stroke::new(1.5, theme::SUCCESS));
        painter.line_segment(
            [pos - Vec2::new(9.0, 0.0), pos + Vec2::new(9.0, 0.0)],
            Stroke::new(1.0, theme::SUCCESS),
        );
        painter.line_segment(
            [pos - Vec2::new(0.0, 9.0), pos + Vec2::new(0.0, 9.0)],
            Stroke::new(1.0, theme::SUCCESS),
        );
    }

    // Execution marker at the current line end.
    if let Some(line) = view.current_line {
        if let Some(segment) = view.preview.segments.iter().rev().find(|s| s.line == line) {
            let pos = projection.project(segment.end);
            painter.circle_filled(pos, 3.0, theme::WARNING);
        }
    }

    // Legend.
    let legend_rect = Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, 12.0),
        Vec2::new(214.0, 54.0),
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
    painter.text(
        legend_rect.left_top() + Vec2::new(10.0, 40.0),
        Align2::LEFT_TOP,
        format!("{}  •  ZOOM {:.1}x", view.view.label(), view.zoom),
        FontId::monospace(9.0 * FONT_SCALE),
        theme::MUTED,
    );

    if !has_data {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO PROGRAM LOADED",
            FontId::monospace(14.0 * FONT_SCALE),
            theme::MUTED,
        );
    }

    response
}

fn view_label(mode: ViewMode, bounds: Bounds3) -> Option<([f64; 3], &'static str)> {
    match mode {
        ViewMode::Iso3d => Some(([bounds.min[0], bounds.min[1], bounds.max[2]], "MACHINE")),
        ViewMode::TopXy => Some(([bounds.min[0], bounds.min[1], 0.0], "MACHINE")),
        ViewMode::SideXz | ViewMode::LatheXz => {
            Some(([bounds.min[0], 0.0, bounds.max[2]], "MACHINE"))
        }
    }
}

fn draw_bounds(
    painter: &Painter,
    projection: &Projection,
    bounds: Bounds3,
    color: Color32,
    width: f32,
    dashed: bool,
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
            dashed,
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
                ViewMode::SideXz | ViewMode::LatheXz => {
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
) {
    let origin = [0.0, 0.0, 0.0];
    let origin_pos = projection.project(origin);
    let length = match projection.mode {
        ViewMode::Iso3d => 24.0,
        ViewMode::TopXy => 24.0,
        ViewMode::SideXz | ViewMode::LatheXz => 24.0,
    };
    let mut axes = Vec::new();
    if axis_labels.iter().any(|axis| axis == "X") {
        axes.push((
            "X",
            Vec2::new(length * 0.866, length * 0.5),
            Color32::from_rgb(0xfb, 0xbf, 0x24),
        ));
    }
    if axis_labels.iter().any(|axis| axis == "Y") || variant == MachineVariant::Mill {
        axes.push((
            "Y",
            Vec2::new(-length * 0.866, length * 0.5),
            Color32::from_rgb(0xfb, 0xbf, 0x24),
        ));
    }
    if axis_labels.iter().any(|axis| axis == "Z") || variant == MachineVariant::Mill {
        axes.push((
            "Z",
            Vec2::new(0.0, -length),
            Color32::from_rgb(0xfb, 0xbf, 0x24),
        ));
    }
    painter.circle_filled(origin_pos, 2.0, theme::MUTED);
    for (label, vector, color) in axes {
        painter.line_segment([origin_pos, origin_pos + vector], Stroke::new(1.2, color));
        painter.text(
            origin_pos + vector + Vec2::new(3.0, -3.0),
            Align2::LEFT_BOTTOM,
            label,
            FontId::monospace(10.0 * FONT_SCALE),
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
        let projection = Projection::new(bounds, rect, ViewMode::Iso3d, 1.0, Vec2::ZERO);
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
    fn nice_steps_are_round_numbers() {
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(0.7), 1.0);
        assert_eq!(nice_step(270.0), 500.0);
    }
}
