//! Geometry types shared by every wave of `linc-sim3d`.
//!
//! # Signed-distance convention
//!
//! A collision query reduces the moving volume to a point (a [`Sphere`] is its
//! centre) or to a segment (a [`Capsule`] is the segment `a`-`b` swept by its
//! radius). For a query volume of radius `r`:
//!
//! ```text
//! d                = distance between the query point/segment and the mesh
//! Contact.distance = d - r        // negative = penetration, 0 = first contact
//! Contact.point    = the closest point ON THE MESH
//! Contact.triangle = index of the mesh triangle that point belongs to
//! ```
//!
//! `tests/geometry.rs` asserts this convention.

pub use crate::bvh::MeshIndex;

/// A point or direction in millimetres.
pub type P3 = [f64; 3];

/// An axis-aligned bounding box.
///
/// The default value is the *empty* box: `min` is `+INFINITY` and `max` is
/// `-INFINITY`, so it can be grown from nothing with [`Bounds::expand`] and
/// disappears from a [`Bounds::union`]. A box whose `min` and `max` coincide is
/// a single point and is *not* empty.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bounds {
    pub min: P3,
    pub max: P3,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }
}

impl Bounds {
    /// The empty box, the same value as `Bounds::default()`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when the box contains nothing (any axis has `min > max`).
    pub fn is_empty(&self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }

    /// Box around every point of `points`; the empty box for no points.
    pub fn from_points(points: &[P3]) -> Self {
        let mut bounds = Self::default();
        for point in points {
            bounds.expand(*point);
        }
        bounds
    }

    /// Midpoint. An empty box reports the origin.
    pub fn center(&self) -> P3 {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    /// Half the size along each axis. An empty box reports zeros.
    pub fn half_extent(&self) -> P3 {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            (self.max[0] - self.min[0]) * 0.5,
            (self.max[1] - self.min[1]) * 0.5,
            (self.max[2] - self.min[2]) * 0.5,
        ]
    }

    /// Full size along each axis. An empty box reports zeros.
    pub fn size(&self) -> P3 {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }

    /// Smallest box containing both boxes; an empty operand is ignored.
    pub fn union(&self, other: &Bounds) -> Bounds {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let mut out = Self::default();
        for i in 0..3 {
            out.min[i] = self.min[i].min(other.min[i]);
            out.max[i] = self.max[i].max(other.max[i]);
        }
        out
    }

    /// Grow this box in place so it contains `point`.
    pub fn expand(&mut self, point: P3) {
        for i in 0..3 {
            self.min[i] = self.min[i].min(point[i]);
            self.max[i] = self.max[i].max(point[i]);
        }
    }

    /// A copy of this box grown to include `point`.
    pub fn expanded(&self, point: P3) -> Bounds {
        let mut out = *self;
        out.expand(point);
        out
    }

    /// True when `point` lies inside the box, allowing `eps` on every side.
    pub fn contains(&self, point: P3, eps: f64) -> bool {
        (0..3).all(|i| point[i] >= self.min[i] - eps && point[i] <= self.max[i] + eps)
    }

    /// Overlap of two boxes, or `None` when they are disjoint or either is empty.
    ///
    /// Boxes that share only a face, edge or corner do overlap; the result is
    /// then a box of zero volume that is still not empty.
    pub fn intersect(&self, other: &Bounds) -> Option<Bounds> {
        if self.is_empty() || other.is_empty() {
            return None;
        }
        let mut out = Self {
            min: [0.0; 3],
            max: [0.0; 3],
        };
        for i in 0..3 {
            out.min[i] = self.min[i].max(other.min[i]);
            out.max[i] = self.max[i].min(other.max[i]);
            if out.min[i] > out.max[i] {
                return None;
            }
        }
        Some(out)
    }

    /// True when the two boxes overlap (touching counts as overlapping).
    pub fn intersects(&self, other: &Bounds) -> bool {
        self.intersect(other).is_some()
    }
}

/// A rigid placement: rotate about X, then Y, then Z (extrinsic, fixed axes),
/// then translate.
///
/// `rotation_deg` holds the three angles in degrees, `translation` is in
/// millimetres.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transform {
    pub translation: P3,
    pub rotation_deg: P3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// The identity placement.
    pub const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation_deg: [0.0; 3],
    };

    /// The identity placement.
    pub fn identity() -> Self {
        Self::IDENTITY
    }

    /// Place a point: `translation + Rz * Ry * Rx * point`.
    pub fn apply(&self, p: P3) -> P3 {
        let r = self.rotate(p);
        [
            r[0] + self.translation[0],
            r[1] + self.translation[1],
            r[2] + self.translation[2],
        ]
    }

    /// Rotate a direction; the translation is not applied.
    pub fn apply_dir(&self, d: P3) -> P3 {
        self.rotate(d)
    }

    /// Undo [`Transform::apply`]: `R^T * (p - translation)`.
    pub fn inverse_apply(&self, p: P3) -> P3 {
        let v = [
            p[0] - self.translation[0],
            p[1] - self.translation[1],
            p[2] - self.translation[2],
        ];
        let m = rotation_matrix(self.rotation_deg);
        [
            m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
            m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
            m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
        ]
    }

    /// This placement with another translation.
    pub fn with_translation(mut self, translation: P3) -> Self {
        self.translation = translation;
        self
    }

    /// This placement with another rotation, in degrees.
    pub fn with_rotation(mut self, rotation_deg: P3) -> Self {
        self.rotation_deg = rotation_deg;
        self
    }

    /// True when both parts are exactly zero.
    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3] && self.rotation_deg == [0.0; 3]
    }

    /// The 3x3 rotation matrix, row-major.
    pub fn matrix(&self) -> [[f64; 3]; 3] {
        rotation_matrix(self.rotation_deg)
    }

    fn rotate(&self, v: P3) -> P3 {
        let m = rotation_matrix(self.rotation_deg);
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }
}

/// `Rz * Ry * Rx` for angles given in degrees: X first, then Y, then Z.
fn rotation_matrix(rotation_deg: P3) -> [[f64; 3]; 3] {
    let (sx, cx) = rotation_deg[0].to_radians().sin_cos();
    let (sy, cy) = rotation_deg[1].to_radians().sin_cos();
    let (sz, cz) = rotation_deg[2].to_radians().sin_cos();
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

/// A triangle soup with an indexed triangle list.
#[derive(Debug, Clone, PartialEq)]
pub struct TriMesh {
    pub name: String,
    pub positions: Vec<P3>,
    pub indices: Vec<[u32; 3]>,
    pub bounds: Bounds,
}

impl TriMesh {
    /// Build a mesh and derive its bounds from `positions`.
    ///
    /// Indices are stored as given; a triangle that refers to a missing vertex
    /// simply answers `None` from [`TriMesh::triangle`].
    pub fn new(name: impl Into<String>, positions: Vec<P3>, indices: Vec<[u32; 3]>) -> Self {
        let bounds = Bounds::from_points(&positions);
        Self {
            name: name.into(),
            positions,
            indices,
            bounds,
        }
    }

    /// A closed box with outward (counter-clockwise) triangles.
    ///
    /// An empty box yields an empty mesh.
    pub fn box_mesh(name: impl Into<String>, bounds: &Bounds) -> TriMesh {
        if bounds.is_empty() {
            return TriMesh::new(name, Vec::new(), Vec::new());
        }
        let [x0, y0, z0] = bounds.min;
        let [x1, y1, z1] = bounds.max;
        let positions = vec![
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let indices = vec![
            [0, 3, 2],
            [0, 2, 1], // z = z0
            [4, 5, 6],
            [4, 6, 7], // z = z1
            [0, 1, 5],
            [0, 5, 4], // y = y0
            [3, 7, 6],
            [3, 6, 2], // y = y1
            [0, 4, 7],
            [0, 7, 3], // x = x0
            [1, 2, 6],
            [1, 6, 5], // x = x1
        ];
        TriMesh::new(name, positions, indices)
    }

    /// A copy of this mesh moved by `transform`; bounds are recomputed.
    pub fn transformed(&self, transform: &Transform) -> TriMesh {
        let positions = self.positions.iter().map(|p| transform.apply(*p)).collect();
        TriMesh::new(self.name.clone(), positions, self.indices.clone())
    }

    /// The three vertices of triangle `i`, or `None` when it is out of range.
    pub fn triangle(&self, i: usize) -> Option<[P3; 3]> {
        let index = self.indices.get(i)?;
        let a = *self.positions.get(index[0] as usize)?;
        let b = *self.positions.get(index[1] as usize)?;
        let c = *self.positions.get(index[2] as usize)?;
        Some([a, b, c])
    }

    /// Number of vertices.
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len()
    }

    /// True when the mesh has no triangles.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// A sphere used as a collision volume, usually the probe tip.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sphere {
    pub center: P3,
    pub radius: f64,
}

/// A capsule used as a collision volume, usually a cutter shaft or probe stem.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Capsule {
    pub a: P3,
    pub b: P3,
    pub radius: f64,
}

/// The result of a nearest-point query, with the signed-distance convention of
/// this module: `distance` is measured from the volume centre/segment and then
/// reduced by the volume radius.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    /// `d - radius`; negative while the volume penetrates the mesh.
    pub distance: f64,
    /// The closest point on the mesh.
    pub point: P3,
    /// Index of the mesh triangle `point` lies on.
    pub triangle: usize,
}
