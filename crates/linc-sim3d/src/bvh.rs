//! A bounding-volume hierarchy over one [`TriMesh`].
//!
//! [`MeshIndex`] answers nearest-point queries for a [`Sphere`] (probe tip) and
//! a [`Capsule`] (cutter or probe stem) with the signed-distance convention of
//! [`crate::types`]: the reported distance is `d - radius`.
//!
//! The tree is built once, split at the median triangle centroid along the
//! longest axis of the node, with leaves of up to [`LEAF_SIZE`] triangles. Split
//! order is total (centroid axis value, then triangle index) so the same mesh
//! always yields the same tree and the same answers.

use std::cmp::Ordering;

use crate::mesh::{
    aabb_distance_sq, point_aabb_distance_sq, point_triangle_closest, segment_triangle_closest,
    triangle_centroid,
};
use crate::types::{Bounds, Capsule, Contact, Sphere, TriMesh, P3};

/// Triangles per leaf.
pub const LEAF_SIZE: usize = 8;
/// Depth cap; protects against pathological meshes of coincident centroids.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy)]
struct Node {
    bounds: Bounds,
    /// Offset of this leaf's triangles into [`MeshIndex::order`]; `count == 0`
    /// means an inner node.
    start: u32,
    count: u32,
    children: [u32; 2],
}

/// A [`TriMesh`] plus a BVH, ready for nearest-point queries.
#[derive(Debug, Clone)]
pub struct MeshIndex {
    mesh: TriMesh,
    /// Triangle indices grouped by leaf.
    order: Vec<u32>,
    nodes: Vec<Node>,
    /// Per-triangle centroid, parallel to `mesh.indices`.
    centroids: Vec<P3>,
}

impl MeshIndex {
    /// Build the hierarchy for `mesh`, taking a copy of it.
    pub fn build(mesh: &TriMesh) -> Self {
        let count = mesh.triangle_count();
        let mut centroids = Vec::with_capacity(count);
        for i in 0..count {
            let triangle = mesh.triangle(i).unwrap_or([[0.0; 3]; 3]);
            centroids.push(triangle_centroid(triangle));
        }
        let mut order: Vec<u32> = (0..count as u32).collect();
        let mut nodes = Vec::new();
        if count > 0 {
            Self::build_node(&mut nodes, &mut order, 0, &centroids, mesh, 0);
        }
        Self {
            mesh: mesh.clone(),
            order,
            nodes,
            centroids,
        }
    }

    fn build_node(
        nodes: &mut Vec<Node>,
        order: &mut [u32],
        base: usize,
        centroids: &[P3],
        mesh: &TriMesh,
        depth: usize,
    ) -> u32 {
        let mut bounds = Bounds::default();
        for &triangle_index in order.iter() {
            if let Some(triangle) = mesh.triangle(triangle_index as usize) {
                for point in triangle {
                    bounds.expand(point);
                }
            }
        }
        let index = nodes.len() as u32;
        if order.len() <= LEAF_SIZE || depth >= MAX_DEPTH {
            nodes.push(Node {
                bounds,
                start: base as u32,
                count: order.len() as u32,
                children: [u32::MAX; 2],
            });
            return index;
        }

        let mut centroid_bounds = Bounds::default();
        for &triangle_index in order.iter() {
            centroid_bounds.expand(centroids[triangle_index as usize]);
        }
        let extent = centroid_bounds.size();
        let axis = if extent[0] >= extent[1] && extent[0] >= extent[2] {
            0
        } else if extent[1] >= extent[2] {
            1
        } else {
            2
        };
        let mid = order.len() / 2;
        order.sort_by(|&left, &right| {
            centroids[left as usize][axis]
                .partial_cmp(&centroids[right as usize][axis])
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.cmp(&right))
        });

        nodes.push(Node {
            bounds,
            start: base as u32,
            count: 0,
            children: [u32::MAX; 2],
        });
        let left = Self::build_node(nodes, &mut order[..mid], base, centroids, mesh, depth + 1);
        let right = Self::build_node(
            nodes,
            &mut order[mid..],
            base + mid,
            centroids,
            mesh,
            depth + 1,
        );
        nodes[index as usize].children = [left, right];
        index
    }

    /// Nearest triangle to `sphere.center`, reported as `d - sphere.radius`.
    ///
    /// `None` for an empty mesh.
    pub fn nearest_sphere(&self, sphere: &Sphere) -> Option<Contact> {
        let center = sphere.center;
        self.nearest(
            |triangle| {
                let closest = point_triangle_closest(center, triangle);
                (closest.distance_sq, closest.point)
            },
            |bounds| point_aabb_distance_sq(center, bounds.min, bounds.max),
            sphere.radius,
        )
    }

    /// Nearest triangle to capsule segment `a`-`b`, reported as
    /// `d - capsule.radius`.
    ///
    /// `None` for an empty mesh.
    pub fn nearest_capsule(&self, capsule: &Capsule) -> Option<Contact> {
        let (a, b) = (capsule.a, capsule.b);
        let mut segment_min = [f64::INFINITY; 3];
        let mut segment_max = [f64::NEG_INFINITY; 3];
        for i in 0..3 {
            segment_min[i] = a[i].min(b[i]);
            segment_max[i] = a[i].max(b[i]);
        }
        self.nearest(
            |triangle| {
                let closest = segment_triangle_closest(a, b, triangle);
                (closest.distance_sq, closest.point)
            },
            |bounds| aabb_distance_sq(segment_min, segment_max, bounds.min, bounds.max),
            capsule.radius,
        )
    }

    /// Descend the tree, evaluating triangles only where the node's lower bound
    /// can still beat the best distance found so far.
    fn nearest<Eval, Lower>(&self, eval: Eval, lower_bound: Lower, radius: f64) -> Option<Contact>
    where
        Eval: Fn([P3; 3]) -> (f64, P3),
        Lower: Fn(&Bounds) -> f64,
    {
        let root = *self.nodes.first()?;
        let mut best_sq = f64::INFINITY;
        let mut best_point = [0.0; 3];
        let mut best_triangle = 0usize;
        let mut found = false;
        let mut stack: Vec<u32> = vec![0];
        while let Some(node_index) = stack.pop() {
            let node = if node_index == 0 {
                root
            } else {
                match self.nodes.get(node_index as usize) {
                    Some(node) => *node,
                    None => continue,
                }
            };
            if found && lower_bound(&node.bounds) >= best_sq {
                continue;
            }
            if node.count > 0 {
                let start = node.start as usize;
                let end = start + node.count as usize;
                for &triangle_index in &self.order[start..end] {
                    let triangle = match self.mesh.triangle(triangle_index as usize) {
                        Some(triangle) => triangle,
                        None => continue,
                    };
                    let (distance_sq, point) = eval(triangle);
                    if distance_sq < best_sq {
                        best_sq = distance_sq;
                        best_point = point;
                        best_triangle = triangle_index as usize;
                        found = true;
                    }
                }
            } else {
                stack.push(node.children[0]);
                stack.push(node.children[1]);
            }
        }
        if !found {
            return None;
        }
        Some(Contact {
            distance: best_sq.sqrt() - radius,
            point: best_point,
            triangle: best_triangle,
        })
    }

    /// Bounds of the indexed mesh.
    pub fn bounds(&self) -> Bounds {
        self.mesh.bounds
    }

    /// Number of indexed triangles.
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count()
    }

    /// The indexed mesh.
    pub fn mesh(&self) -> &TriMesh {
        &self.mesh
    }

    /// Per-triangle centroid, parallel to the mesh's triangle list.
    pub fn centroids(&self) -> &[P3] {
        &self.centroids
    }
}
