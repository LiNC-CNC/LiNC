//! The 3D view: a perspective camera, the model baked into triangles, and the
//! egui Mesh those triangles are drawn with.
//!
//! egui only draws 2D triangles, so this module does the projection itself: the
//! camera turns machine millimetres into screen points and the result is pushed
//! as an egui mesh. No third renderer, no GPU state, no shader.
//!
//! The work is split in two halves:
//!
//! - Scene::build walks the model's meshes once, orients their normals, bakes
//!   the flat lighting into vertex colours and collects the grid, the
//!   limit-plane outlines and the axis labels. It runs when the model changes.
//! - Scene::screen_mesh projects the scene for the current camera. It runs when
//!   the camera, the viewport or the scene changes, and the tool - a few
//!   hundred triangles that follow the live tip - is a per-frame mesh of its
//!   own.
//!
//! Machine coordinates, the same convention the model uses: millimetres, X to
//! the right, Y to the back, Z up, origin at machine X0Y0Z0.

use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use crate::model::SimModel;
use crate::tools::{ActiveTool, CutterShape};
use crate::types::{Bounds, TriMesh, P3};

/// The camera never looks exactly along its own up vector.
const MIN_PITCH_DEG: f64 = -88.0;
const MAX_PITCH_DEG: f64 = 88.0;
/// Near plane, in mm: anything closer is dropped, not clipped.
const NEAR_MM: f64 = 1.0;
const MIN_DISTANCE_MM: f64 = 5.0;
const MAX_DISTANCE_MM: f64 = 100_000.0;
/// The fixed light: mostly from above, a little from the front left.
const LIGHT: [f64; 3] = [0.3505, -0.4506, 0.8210];
/// Shading floor, so a face turned away from the light still reads as a face.
const AMBIENT: f64 = 0.42;

const BACKGROUND: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
const WORKPIECE: Color32 = Color32::from_rgb(0x9c, 0xcf, 0xf2);
const TOOL_STEEL: Color32 = Color32::from_rgb(0xbc, 0xc0, 0xc6);
const TOOL_COLLIDING: Color32 = Color32::from_rgb(0xe2, 0x4c, 0x3c);
const MARKER: Color32 = Color32::from_rgb(0xff, 0x3b, 0x30);
const GRID_MINOR: Color32 = Color32::from_rgb(0x3b, 0x40, 0x48);
const GRID_MAJOR: Color32 = Color32::from_rgb(0x59, 0x60, 0x6a);
const LIMIT_PLANE: Color32 = Color32::from_rgba_unmultiplied_const(0x5f, 0x8f, 0xc8, 0x8c);
const AXIS_X: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);
const AXIS_Y: Color32 = Color32::from_rgb(0x6c, 0xd0, 0x7c);
const AXIS_Z: Color32 = Color32::from_rgb(0x6c, 0xa8, 0xf0);

// ------------------------------------------------------------------ camera

/// An orbiting perspective camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// The point the camera orbits and looks at, in machine mm.
    pub target: P3,
    /// Rotation about +Z, in degrees; 0 puts the eye on the +X side.
    pub yaw_deg: f64,
    /// Elevation above the XY plane, in degrees; positive looks down.
    pub pitch_deg: f64,
    /// Distance from the eye to the target, in mm.
    pub distance_mm: f64,
    /// Vertical field of view, in degrees.
    pub fov_deg: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self::iso()
    }
}

impl Camera {
    /// The three-quarter view the window opens with.
    pub fn iso() -> Self {
        Self {
            target: [0.0; 3],
            yaw_deg: -55.0,
            pitch_deg: 26.0,
            distance_mm: 900.0,
            fov_deg: 45.0,
        }
    }

    /// Straight down, with +X to the right and +Y up the screen.
    pub fn top() -> Self {
        Self {
            yaw_deg: -90.0,
            pitch_deg: MAX_PITCH_DEG,
            ..Self::iso()
        }
    }

    /// Looking along -Y, the operator's side of the machine.
    pub fn front() -> Self {
        Self {
            yaw_deg: 90.0,
            pitch_deg: 0.0,
            ..Self::iso()
        }
    }

    /// Place the eye so the whole box fills the view.
    ///
    /// An empty box leaves the camera untouched.
    pub fn fit(&mut self, bounds: &Bounds) {
        if bounds.is_empty() {
            return;
        }
        self.target = bounds.center();
        let size = bounds.size();
        let radius = 0.5 * (size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt();
        let half_fov = (self.fov_deg.to_radians() * 0.5).max(0.05);
        self.distance_mm = (radius / half_fov.tan()).clamp(MIN_DISTANCE_MM, MAX_DISTANCE_MM) * 1.25;
    }

    /// Turn the camera by an angle in degrees; pitch is clamped.
    pub fn orbit(&mut self, yaw_deg: f64, pitch_deg: f64) {
        self.yaw_deg = wrap_deg(self.yaw_deg + yaw_deg);
        self.pitch_deg = (self.pitch_deg + pitch_deg).clamp(MIN_PITCH_DEG, MAX_PITCH_DEG);
    }

    /// Zoom by a scroll amount: positive zooms in.
    pub fn zoom(&mut self, scroll: f64) {
        let factor = (-scroll / 240.0).exp();
        self.distance_mm = (self.distance_mm * factor).clamp(MIN_DISTANCE_MM, MAX_DISTANCE_MM);
    }

    /// Slide the target across the view plane by a screen-space delta.
    pub fn pan(&mut self, delta: Vec2, projector: &Projector) {
        let scale = projector.mm_per_pixel_at(projector.reference_depth());
        let (right, up) = (projector.right(), projector.up());
        for axis in 0..3 {
            self.target[axis] -= right[axis] * f64::from(delta.x) * scale;
            self.target[axis] += up[axis] * f64::from(delta.y) * scale;
        }
    }

    /// The eye position in machine mm.
    pub fn eye(&self) -> P3 {
        let (sin_yaw, cos_yaw) = self.yaw_deg.to_radians().sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch_deg.to_radians().sin_cos();
        let distance = self.distance_mm.max(MIN_DISTANCE_MM);
        [
            self.target[0] + distance * cos_pitch * cos_yaw,
            self.target[1] + distance * cos_pitch * sin_yaw,
            self.target[2] + distance * sin_pitch,
        ]
    }

    /// Right, up and forward in world space; forward points at the target.
    pub fn basis(&self) -> (P3, P3, P3) {
        let forward = normalize(sub(self.target, self.eye()));
        let right = normalize(cross(forward, [0.0, 0.0, 1.0]));
        let up = cross(right, forward);
        (right, up, forward)
    }
}

fn wrap_deg(deg: f64) -> f64 {
    let wrapped = deg % 360.0;
    if wrapped < -180.0 {
        wrapped + 360.0
    } else if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// A camera and a viewport, ready to project points.
///
/// Points behind the near plane answer None, so callers drop triangles that
/// straddle it rather than clipping them.
#[derive(Debug, Clone, Copy)]
pub struct Projector {
    eye: P3,
    right: P3,
    up: P3,
    forward: P3,
    /// The camera target, which panning and marker sizes scale against.
    depth_reference: f64,
    /// Pixels per millimetre at one millimetre of depth.
    scale: f64,
    center: Pos2,
}

impl Projector {
    /// Project from a camera into a viewport.
    pub fn new(camera: &Camera, viewport: Rect) -> Self {
        let (right, up, forward) = camera.basis();
        let half_fov = (camera.fov_deg.to_radians() * 0.5).max(0.05);
        let scale = (f64::from(viewport.height()) * 0.5 / half_fov.tan()).max(1.0);
        let eye = camera.eye();
        Self {
            eye,
            right,
            up,
            forward,
            depth_reference: length(sub(camera.target, eye)),
            scale,
            center: viewport.center(),
        }
    }

    /// The eye position in machine mm.
    pub fn eye(&self) -> P3 {
        self.eye
    }

    /// The camera's right vector in world space.
    pub fn right(&self) -> P3 {
        self.right
    }

    /// The camera's up vector in world space.
    pub fn up(&self) -> P3 {
        self.up
    }

    /// Depth along the view direction, in mm; negative is behind the eye.
    pub fn view_depth(&self, point: P3) -> f64 {
        dot(sub(point, self.eye), self.forward)
    }

    /// Millimetres per pixel at a depth.
    pub fn mm_per_pixel_at(&self, depth: f64) -> f64 {
        depth.max(NEAR_MM) / self.scale
    }

    /// The depth panning is scaled against: the camera target's plane.
    pub fn reference_depth(&self) -> f64 {
        self.depth_reference.max(NEAR_MM)
    }

    /// Project a point to a screen position and its depth.
    ///
    /// None when the point is behind the near plane.
    pub fn project(&self, point: P3) -> Option<(Pos2, f64)> {
        let view = sub(point, self.eye);
        let depth = dot(view, self.forward);
        if depth <= NEAR_MM {
            return None;
        }
        let x = dot(view, self.right);
        let y = dot(view, self.up);
        let scale = self.scale / depth;
        Some((
            Pos2::new(
                self.center.x + (x * scale) as f32,
                self.center.y - (y * scale) as f32,
            ),
            depth,
        ))
    }
}

// ------------------------------------------------------------------- scene

/// One lit triangle of the model, in machine mm.
#[derive(Debug, Clone, Copy)]
pub struct Tri {
    pub a: P3,
    pub b: P3,
    pub c: P3,
    /// Outward unit normal, already corrected for the mesh's winding.
    pub normal: P3,
    /// The baked flat shade.
    pub color: Color32,
}

impl Tri {
    /// The centroid, which the depth sort and the facing test use.
    pub fn centroid(&self) -> P3 {
        centroid_of(self.a, self.b, self.c)
    }
}

/// One line of the scene: grid, travel outline or limit plane.
#[derive(Debug, Clone, Copy)]
pub struct Line {
    pub a: P3,
    pub b: P3,
    pub color: Color32,
}

/// A text label anchored at a machine point.
#[derive(Debug, Clone)]
pub struct Label {
    pub text: String,
    pub at: P3,
    pub color: Color32,
}

/// The model, baked once per model change.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub tris: Vec<Tri>,
    /// Drawn over the triangles: the table grid, which sits on the table's own
    /// top face and is hidden by it otherwise.
    pub grid: Vec<Line>,
    /// Drawn under the triangles: the X/Y/Z triad, so a part in front of it
    /// occludes it.
    pub axes: Vec<Line>,
    /// Drawn over the triangles: travel box and limit planes.
    pub outlines: Vec<Line>,
    pub labels: Vec<Label>,
    /// Everything the scene covers, for the fit button.
    pub bounds: Bounds,
}

impl Scene {
    /// Bake a model into a scene.
    pub fn build(model: &SimModel) -> Self {
        let mut scene = Self {
            bounds: model.machine_bounds(),
            ..Self::default()
        };
        for part in &model.machine().parts {
            scene.tris.extend(mesh_tris(part, part_color(&part.name)));
        }
        for workpiece in model.workpieces() {
            if !workpiece.visible || workpiece.mesh.is_empty() {
                continue;
            }
            scene.tris.extend(mesh_tris(&workpiece.mesh, WORKPIECE));
            scene.bounds = scene.bounds.union(&workpiece.mesh.bounds);
        }
        grid_lines(&mut scene.grid, &model.machine().travel, table_top(model));
        for (_, plane) in &model.machine().limit_planes {
            push_box_outline(&mut scene.outlines, &plane.bounds, LIMIT_PLANE);
        }
        push_box_outline(&mut scene.outlines, &model.machine().travel, GRID_MAJOR);
        axis_triad(&mut scene.axes, &mut scene.labels, &model.machine().travel);
        scene
    }

    /// True when there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
            && self.grid.is_empty()
            && self.axes.is_empty()
            && self.outlines.is_empty()
    }

    /// Number of baked triangles.
    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Project the scene into an egui mesh.
    ///
    /// Triangles facing away from the eye and triangles crossing the near
    /// plane are dropped; the rest are sorted far to near, so egui's painter's
    /// algorithm draws them in the right order.
    pub fn screen_mesh(&self, projector: &Projector) -> egui::Mesh {
        screen_mesh_of(&self.tris, projector)
    }
}

/// The machine table's top face, or the bottom of travel when the model has no
/// table part.
fn table_top(model: &SimModel) -> f64 {
    model
        .machine()
        .part("table")
        .map_or(model.machine().travel.min[2], |table| table.bounds.max[2])
}

/// Bake one mesh into lit, outward-facing triangles.
///
/// An imported part may be wound inward, in which case every normal points at
/// the part's own centre. That is detected once and the whole mesh is flipped,
/// so the facing test culls the far side and not the near one. The winding
/// itself does not matter: culling and shading both use the normal.
pub fn mesh_tris(mesh: &TriMesh, color: Color32) -> Vec<Tri> {
    let mut tris = Vec::with_capacity(mesh.triangle_count());
    if mesh.is_empty() {
        return tris;
    }
    let center = mesh.bounds.center();
    let mut total = 0usize;
    let mut inward = 0usize;
    for index in 0..mesh.triangle_count() {
        let Some([a, b, c]) = mesh.triangle(index) else {
            continue;
        };
        let Some(normal) = unit_normal(a, b, c) else {
            continue;
        };
        total += 1;
        if dot(normal, sub(centroid_of(a, b, c), center)) < 0.0 {
            inward += 1;
        }
    }
    let flip = total > 0 && inward * 2 > total;
    for index in 0..mesh.triangle_count() {
        let Some([a, b, c]) = mesh.triangle(index) else {
            continue;
        };
        push_tri(&mut tris, a, b, c, color, flip);
    }
    tris
}

/// Project triangles into an egui mesh, culled and sorted far to near.
pub fn screen_mesh_of(tris: &[Tri], projector: &Projector) -> egui::Mesh {
    let mut faces: Vec<(f64, [Pos2; 3], Color32)> = Vec::with_capacity(tris.len());
    for tri in tris {
        // Back-face culling by normal: the outward normal has to point at the
        // eye for the face to be visible.
        if dot(tri.normal, sub(projector.eye(), tri.centroid())) <= 0.0 {
            continue;
        }
        let (Some((a, da)), Some((b, db)), Some((c, dc))) = (
            projector.project(tri.a),
            projector.project(tri.b),
            projector.project(tri.c),
        ) else {
            continue;
        };
        faces.push(((da + db + dc) / 3.0, [a, b, c], tri.color));
    }
    faces.sort_by(|left, right| right.0.total_cmp(&left.0));

    let mut mesh = egui::Mesh::default();
    mesh.vertices.reserve(faces.len() * 3);
    for (_, [a, b, c], color) in faces {
        let first = mesh.vertices.len() as u32;
        mesh.colored_vertex(a, color);
        mesh.colored_vertex(b, color);
        mesh.colored_vertex(c, color);
        mesh.add_triangle(first, first + 1, first + 2);
    }
    mesh
}

/// The tool as triangles, from the live tip.
///
/// The probe is a ball with its stylus above it; a cutter is a cylinder, a ball
/// nose or a bull nose by diameter. A colliding tool is painted red.
pub fn tool_tris(tool: &ActiveTool, tip: P3, colliding: bool) -> Vec<Tri> {
    let color = if colliding {
        TOOL_COLLIDING
    } else {
        TOOL_STEEL
    };
    let mut tris = Vec::new();
    let length = if tool.length > 0.5 { tool.length } else { 10.0 };
    let body = tool.body_radius.max(0.4);
    let tip_radius = if tool.tip_radius > 0.05 {
        tool.tip_radius
    } else {
        0.4
    };
    match tool.shape {
        CutterShape::Probe { .. } | CutterShape::Ball { .. } => {
            sphere(&mut tris, tip, tip_radius, 8, 16, color);
            frustum(
                &mut tris,
                tip,
                tip_radius * 0.5,
                length,
                body,
                body,
                16,
                color,
            );
        }
        CutterShape::Bull {
            diameter,
            corner_radius,
        } => {
            let radius = (diameter * 0.5).max(0.4);
            let corner = corner_radius.clamp(0.0, radius);
            let inner = (radius - corner).max(0.0);
            disc_down(&mut tris, tip, inner, 24, color);
            frustum(&mut tris, tip, 0.0, corner, inner, radius, 24, color);
            frustum(&mut tris, tip, corner, length, radius, radius, 24, color);
        }
        CutterShape::Flat { .. } | CutterShape::Drill { .. } | CutterShape::Chamfer { .. } => {
            let radius = (tool.shape.diameter() * 0.5).max(0.4);
            disc_down(&mut tris, tip, radius, 24, color);
            frustum(&mut tris, tip, 0.0, length, radius, radius, 24, color);
        }
    }
    tris
}

/// A sphere, as a lat/long mesh of outward-facing triangles.
fn sphere(
    out: &mut Vec<Tri>,
    center: P3,
    radius: f64,
    rings: usize,
    segments: usize,
    color: Color32,
) {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let point = |ring: usize, segment: usize| -> P3 {
        let phi =
            -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * (ring as f64 / rings as f64);
        let theta = std::f64::consts::TAU * (segment as f64 / segments as f64);
        [
            center[0] + radius * phi.cos() * theta.cos(),
            center[1] + radius * phi.cos() * theta.sin(),
            center[2] + radius * phi.sin(),
        ]
    };
    for ring in 0..rings {
        for segment in 0..segments {
            let a = point(ring, segment);
            let b = point(ring + 1, segment);
            let c = point(ring + 1, segment + 1);
            let d = point(ring, segment + 1);
            // The triangle that touches the south pole (ring 0) and the one
            // that touches the north pole (the last ring) are degenerate.
            if ring + 1 < rings {
                push_tri_away(out, a, b, c, color, center);
            }
            if ring > 0 {
                push_tri_away(out, a, c, d, color, center);
            }
        }
    }
}

/// A vertical frustum above the tip, with outward-facing sides.
fn frustum(
    out: &mut Vec<Tri>,
    from: P3,
    skip: f64,
    length: f64,
    radius_low: f64,
    radius_high: f64,
    segments: usize,
    color: Color32,
) {
    let segments = segments.max(3);
    let z0 = from[2] + skip;
    let z1 = from[2] + length.max(skip + 0.1);
    let inside = [from[0], from[1], (z0 + z1) * 0.5];
    let ring = |z: f64, radius: f64, segment: usize| -> P3 {
        let theta = std::f64::consts::TAU * (segment as f64 / segments as f64);
        [
            from[0] + radius * theta.cos(),
            from[1] + radius * theta.sin(),
            z,
        ]
    };
    for segment in 0..segments {
        let low_a = ring(z0, radius_low, segment);
        let low_b = ring(z0, radius_low, segment + 1);
        let high_a = ring(z1, radius_high, segment);
        let high_b = ring(z1, radius_high, segment + 1);
        push_tri_away(out, low_a, high_a, high_b, color, inside);
        push_tri_away(out, low_a, high_b, low_b, color, inside);
    }
}

/// A disc facing -Z at the tip: the flat end of a cutter.
fn disc_down(out: &mut Vec<Tri>, tip: P3, radius: f64, segments: usize, color: Color32) {
    let segments = segments.max(3);
    let above = [tip[0], tip[1], tip[2] + 1.0];
    let rim = |segment: usize| -> P3 {
        let theta = std::f64::consts::TAU * (segment as f64 / segments as f64);
        [
            tip[0] + radius * theta.cos(),
            tip[1] + radius * theta.sin(),
            tip[2],
        ]
    };
    for segment in 0..segments {
        push_tri_away(out, tip, rim(segment), rim(segment + 1), color, above);
    }
}

// ----------------------------------------------------------------- helpers

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(v: P3) -> f64 {
    dot(v, v).sqrt()
}

fn normalize(v: P3) -> P3 {
    let len = length(v);
    if len <= f64::EPSILON {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn centroid_of(a: P3, b: P3, c: P3) -> P3 {
    [
        (a[0] + b[0] + c[0]) / 3.0,
        (a[1] + b[1] + c[1]) / 3.0,
        (a[2] + b[2] + c[2]) / 3.0,
    ]
}

fn unit_normal(a: P3, b: P3, c: P3) -> Option<P3> {
    let normal = cross(sub(b, a), sub(c, a));
    if length(normal) <= 1e-12 {
        return None;
    }
    Some(normalize(normal))
}

/// Flat shade for one normal: the fixed light dotted with it, plus ambient so
/// no face is black.
pub fn lit(base: Color32, normal: P3) -> Color32 {
    let strength = dot(normal, LIGHT).max(0.0);
    let factor = AMBIENT + (1.0 - AMBIENT) * strength;
    let shade = |channel: u8| (f64::from(channel) * factor).min(255.0) as u8;
    Color32::from_rgb(shade(base.r()), shade(base.g()), shade(base.b()))
}

fn push_tri(out: &mut Vec<Tri>, a: P3, b: P3, c: P3, color: Color32, flip: bool) {
    let Some(mut normal) = unit_normal(a, b, c) else {
        return;
    };
    if flip {
        normal = [-normal[0], -normal[1], -normal[2]];
    }
    out.push(Tri {
        a,
        b,
        c,
        normal,
        color: lit(color, normal),
    });
}

/// Push a triangle whose normal is turned away from a point inside the body.
fn push_tri_away(out: &mut Vec<Tri>, a: P3, b: P3, c: P3, color: Color32, inside: P3) {
    let Some(normal) = unit_normal(a, b, c) else {
        return;
    };
    let normal = if dot(normal, sub(centroid_of(a, b, c), inside)) < 0.0 {
        [-normal[0], -normal[1], -normal[2]]
    } else {
        normal
    };
    out.push(Tri {
        a,
        b,
        c,
        normal,
        color: lit(color, normal),
    });
}

fn part_color(name: &str) -> Color32 {
    match name {
        "base" => Color32::from_rgb(0x38, 0x3b, 0x3f),
        "table" => Color32::from_rgb(0x51, 0x54, 0x59),
        "column" => Color32::from_rgb(0x45, 0x48, 0x4c),
        "head" => Color32::from_rgb(0x5b, 0x5e, 0x63),
        "spindle" => Color32::from_rgb(0x69, 0x6c, 0x71),
        "holder" => Color32::from_rgb(0x77, 0x7a, 0x7f),
        _ => Color32::from_rgb(0x4d, 0x50, 0x55),
    }
}

/// A round grid spacing for a table this wide.
pub fn grid_spacing(extent: f64) -> f64 {
    let target = (extent / 12.0).max(1.0);
    for spacing in [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 500.0] {
        if spacing >= target {
            return spacing;
        }
    }
    1000.0
}

/// Grid lines on the table top, spanning the travel.
fn grid_lines(out: &mut Vec<Line>, travel: &Bounds, z: f64) {
    if travel.is_empty() {
        return;
    }
    let size = travel.size();
    let spacing = grid_spacing(size[0].max(size[1]));
    let x0 = (travel.min[0] / spacing).floor() * spacing;
    let x1 = (travel.max[0] / spacing).ceil() * spacing;
    let y0 = (travel.min[1] / spacing).floor() * spacing;
    let y1 = (travel.max[1] / spacing).ceil() * spacing;
    let mut index = 0;
    let mut x = x0;
    while x <= x1 + 1e-9 {
        let color = if index % 5 == 0 {
            GRID_MAJOR
        } else {
            GRID_MINOR
        };
        out.push(Line {
            a: [x, y0, z],
            b: [x, y1, z],
            color,
        });
        x += spacing;
        index += 1;
    }
    index = 0;
    let mut y = y0;
    while y <= y1 + 1e-9 {
        let color = if index % 5 == 0 {
            GRID_MAJOR
        } else {
            GRID_MINOR
        };
        out.push(Line {
            a: [x0, y, z],
            b: [x1, y, z],
            color,
        });
        y += spacing;
        index += 1;
    }
}

/// The 12 edges of a box.
fn push_box_outline(out: &mut Vec<Line>, bounds: &Bounds, color: Color32) {
    if bounds.is_empty() {
        return;
    }
    let [x0, y0, z0] = bounds.min;
    let [x1, y1, z1] = bounds.max;
    let corners = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y1, z0],
        [x0, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x1, y1, z1],
        [x0, y1, z1],
    ];
    const EDGES: [(usize, usize); 12] = [
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
    for (a, b) in EDGES {
        out.push(Line {
            a: corners[a],
            b: corners[b],
            color,
        });
    }
}

/// The X/Y/Z triad, drawn at the machine origin clamped into travel.
fn axis_triad(lines: &mut Vec<Line>, labels: &mut Vec<Label>, travel: &Bounds) {
    if travel.is_empty() {
        return;
    }
    let size = travel.size();
    let origin: P3 = [
        0.0f64.clamp(travel.min[0], travel.max[0]),
        0.0f64.clamp(travel.min[1], travel.max[1]),
        travel.min[2] + size[2] * 0.05,
    ];
    let reach = size[0].max(size[1]).max(size[2]).max(50.0) * 0.12;
    let axes = [
        (AXIS_X, [reach, 0.0, 0.0], "X"),
        (AXIS_Y, [0.0, reach, 0.0], "Y"),
        (AXIS_Z, [0.0, 0.0, reach], "Z"),
    ];
    for (color, offset, name) in axes {
        let end = [
            origin[0] + offset[0],
            origin[1] + offset[1],
            origin[2] + offset[2],
        ];
        lines.push(Line {
            a: origin,
            b: end,
            color,
        });
        labels.push(Label {
            text: format!("{name} +{reach:.0} mm"),
            at: end,
            color,
        });
    }
}

// ------------------------------------------------------------------ viewer

/// The tool and contact state of the current tick, which change per frame.
#[derive(Debug, Clone, Copy)]
pub struct Live {
    /// The probe ball centre or cutter tip, in machine mm.
    pub tip: P3,
    pub tool: ActiveTool,
    /// Paint the tool red.
    pub colliding: bool,
    /// Where to put the filled contact marker.
    pub marker: Option<P3>,
}

/// A rendered view: the camera plus the scene it draws.
///
/// The screen mesh is cached and rebuilt only when the camera, the viewport or
/// the scene changes, so a still camera costs one Arc clone per frame.
pub struct Viewer {
    pub camera: Camera,
    scene: Scene,
    generation: u64,
    cache: Option<CachedMesh>,
}

struct CachedMesh {
    key: CameraKey,
    mesh: Arc<egui::Mesh>,
}

/// A quantised camera and viewport, so float noise does not rebuild the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CameraKey {
    generation: u64,
    viewport: [i32; 4],
    target: [i64; 3],
    yaw: i64,
    pitch: i64,
    distance: i64,
}

impl CameraKey {
    fn of(camera: &Camera, viewport: Rect, generation: u64) -> Self {
        let wave = |value: f64| (value * 100.0).round() as i64;
        Self {
            generation,
            viewport: [
                viewport.min.x.round() as i32,
                viewport.min.y.round() as i32,
                viewport.max.x.round() as i32,
                viewport.max.y.round() as i32,
            ],
            target: [
                wave(camera.target[0]),
                wave(camera.target[1]),
                wave(camera.target[2]),
            ],
            yaw: wave(camera.yaw_deg),
            pitch: wave(camera.pitch_deg),
            distance: wave(camera.distance_mm),
        }
    }
}

impl Viewer {
    /// A viewer looking at a scene from the iso view, framed on it.
    pub fn new(scene: Scene) -> Self {
        let mut camera = Camera::iso();
        camera.fit(&scene.bounds);
        Self {
            camera,
            scene,
            generation: 0,
            cache: None,
        }
    }

    /// The baked scene.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Replace the scene, because the model changed.
    pub fn set_scene(&mut self, scene: Scene) {
        self.scene = scene;
        self.generation = self.generation.wrapping_add(1);
        self.cache = None;
    }

    /// Frame the whole scene.
    pub fn fit(&mut self) {
        self.camera.fit(&self.scene.bounds);
    }

    /// Look at one machine point from the current angles, zoomed in on it.
    ///
    /// This is the "show me the probe" view: the whole machine is the right
    /// frame for reading the layout, but a 3 mm ball is invisible in it.
    pub fn look_at(&mut self, point: P3, distance_mm: f64) {
        self.camera.target = point;
        self.camera.distance_mm = distance_mm.clamp(MIN_DISTANCE_MM, MAX_DISTANCE_MM);
    }

    /// Draw the view into the remaining space of a UI.
    pub fn show(&mut self, ui: &mut egui::Ui, live: &Live) {
        let available = ui.available_size();
        if available.x < 8.0 || available.y < 8.0 {
            return;
        }
        let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }

        // Input first, then project with the camera the input left behind.
        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            self.camera
                .orbit(-f64::from(delta.x) * 0.4, f64::from(delta.y) * 0.4);
        } else if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
        {
            let projector = Projector::new(&self.camera, rect);
            self.camera.pan(response.drag_delta(), &projector);
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom(f64::from(scroll));
            }
        }

        let projector = Projector::new(&self.camera, rect);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BACKGROUND);

        // Annotations that live in the machine, so the body and the parts
        // occlude them: the triad and the limit planes.
        for line in &self.scene.axes {
            draw_line(&painter, &projector, line);
        }
        for line in &self.scene.outlines {
            draw_line(&painter, &projector, line);
        }

        let mesh = self.cached_mesh(&projector, rect);
        painter.add(egui::Shape::mesh(mesh));

        // The grid sits on the table's own top face, so drawing it before the
        // mesh would hide it behind the opaque table. egui blends in draw
        // order, which is not a depth test: a hairline over the table reads the
        // way an engraved grid does.
        for line in &self.scene.grid {
            draw_line(&painter, &projector, line);
        }
        for label in &self.scene.labels {
            if let Some((pos, _)) = projector.project(label.at) {
                painter.text(
                    pos,
                    Align2::CENTER_BOTTOM,
                    &label.text,
                    FontId::monospace(12.0),
                    label.color,
                );
            }
        }

        let tool = tool_tris(&live.tool, live.tip, live.colliding);
        if !tool.is_empty() {
            painter.add(egui::Shape::mesh(screen_mesh_of(&tool, &projector)));
        }
        if let Some(point) = live.marker {
            if let Some((pos, depth)) = projector.project(point) {
                let radius = (projector.mm_per_pixel_at(depth) * 2.5).clamp(2.5, 9.0) as f32;
                painter.circle_filled(pos, radius, MARKER);
                painter.circle_stroke(pos, radius, Stroke::new(1.0, Color32::WHITE));
            }
        }
    }

    fn cached_mesh(&mut self, projector: &Projector, rect: Rect) -> Arc<egui::Mesh> {
        let key = CameraKey::of(&self.camera, rect, self.generation);
        if let Some(cached) = &self.cache {
            if cached.key == key {
                return Arc::clone(&cached.mesh);
            }
        }
        let mesh = Arc::new(self.scene.screen_mesh(projector));
        self.cache = Some(CachedMesh {
            key,
            mesh: Arc::clone(&mesh),
        });
        mesh
    }
}

fn draw_line(painter: &egui::Painter, projector: &Projector, line: &Line) {
    let (Some((a, _)), Some((b, _))) = (projector.project(line.a), projector.project(line.b))
    else {
        return;
    };
    painter.line_segment([a, b], Stroke::new(1.0, line.color));
}

/// The colours the side panel's legend explains.
pub fn legend() -> [(&'static str, Color32); 6] {
    [
        ("machine body", Color32::from_rgb(0x51, 0x54, 0x59)),
        ("workpiece", WORKPIECE),
        ("limit planes and travel", LIMIT_PLANE),
        ("tool: probe ball or cutter", TOOL_STEEL),
        ("tool colliding", TOOL_COLLIDING),
        ("contact point", MARKER),
    ]
}

// -------------------------------------------------------------- rig window

/// What the command line said about the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFlag {
    /// Neither --window nor --no-window.
    Unset,
    /// --window: open it, and treat a failure to open one as an error.
    Force,
    /// --no-window: never open it.
    Off,
}

/// What a --hal rig should do about the window.
///
/// --hal publishes the HAL component and samples it; whether the same process
/// also shows the 3D view is a policy decision, not a code path, so it is made
/// once here and tested on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Open the window; a window that cannot open is a hard error.
    Required,
    /// Open the window when this host has a display; sample headless otherwise.
    IfAvailable,
    /// Never open a window.
    Never,
}

impl WindowMode {
    /// True when the rig should try to open a window.
    pub fn wants_window(self) -> bool {
        !matches!(self, Self::Never)
    }
}

/// The window a --hal rig shows, given the flags, LINC_SIM3D_WINDOW and whether
/// a display is set.
///
/// Precedence, highest first:
///
/// 1. --no-window, then --window (clap rejects both together);
/// 2. LINC_SIM3D_WINDOW: 0/false/no/off means never, 1/true/yes/on means open it
///    even without a display, so a failure to open is then an error;
/// 3. a DISPLAY or WAYLAND_DISPLAY means "open it if this host can";
/// 4. nothing means never.
pub fn window_mode(flag: WindowFlag, env: Option<&str>, display: bool) -> WindowMode {
    match flag {
        WindowFlag::Force => WindowMode::Required,
        WindowFlag::Off => WindowMode::Never,
        WindowFlag::Unset => match env.map(str::trim) {
            Some(value) if is_off(value) => WindowMode::Never,
            Some(value) if is_on(value) => WindowMode::Required,
            _ if display => WindowMode::IfAvailable,
            _ => WindowMode::Never,
        },
    }
}

fn is_off(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn is_on(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// True when either display variable names something.
///
/// Pure, so the rule can be tested; [display_available] feeds it the process
/// environment.
pub fn has_display(display: Option<&str>, wayland_display: Option<&str>) -> bool {
    [display, wayland_display]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
}

/// True when this process has a graphical display to open a window on.
pub fn display_available() -> bool {
    has_display(
        std::env::var("DISPLAY").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

/// Why the window should close on this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The operator pressed Esc.
    Escape,
    /// SIGINT or SIGTERM arrived: the rig is shutting down, not just the view.
    StopRequested,
}

/// The close decision for one frame.
///
/// A stop signal wins over Esc, because the two mean different things
/// afterwards: after Esc a --hal rig keeps sampling, while after SIGINT or
/// SIGTERM it has to shut down. Both close the window, which is the only thing
/// the eframe loop can do about either - so the loop has to notice the stop flag
/// itself, or the process would outlive the signal that asked it to stop.
pub fn close_request(stop_requested: bool, escape: bool) -> Option<CloseReason> {
    if stop_requested {
        Some(CloseReason::StopRequested)
    } else if escape {
        Some(CloseReason::Escape)
    } else {
        None
    }
}
