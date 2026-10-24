//! Triangle-level geometry: closest points, distances and segment/triangle
//! intersection. [`crate::bvh::MeshIndex`] is built on these primitives; the
//! tests compare its answers against a brute-force loop that uses them directly.

use glam::DVec3;

use crate::types::P3;

/// Convert a shared point type into glam's vector for the maths below.
pub fn to_vec(p: P3) -> DVec3 {
    DVec3::new(p[0], p[1], p[2])
}

/// Convert a glam vector back into a shared point.
pub fn from_vec(v: DVec3) -> P3 {
    [v.x, v.y, v.z]
}

/// A closest point and the squared distance to it.
///
/// Squared distances are used throughout so that bounding-volume pruning never
/// pays for a square root; callers that report a distance take one at the end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Closest {
    /// The closest point on the queried triangle or edge.
    pub point: P3,
    /// Squared distance from the query to [`Closest::point`].
    pub distance_sq: f64,
}

/// Closest point between two segments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentClosest {
    /// Closest point on the first segment (`p1`-`q1`).
    pub on_first: P3,
    /// Closest point on the second segment (`p2`-`q2`).
    pub on_second: P3,
    /// Squared distance between the two points.
    pub distance_sq: f64,
}

/// Closest point on `triangle` to `p`.
///
/// Regions of the triangle are tested in Ericson's order; a degenerate triangle
/// (zero area, from a bad tessellation) degrades to its nearest vertex.
pub fn point_triangle_closest(p: P3, triangle: [P3; 3]) -> Closest {
    let p = to_vec(p);
    let a = to_vec(triangle[0]);
    let b = to_vec(triangle[1]);
    let c = to_vec(triangle[2]);

    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    let mut best = a;
    if d1 <= 0.0 && d2 <= 0.0 {
        return finish(best, p);
    }

    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return finish(b, p);
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let denom = d1 - d3;
        if denom == 0.0 {
            return finish(a, p);
        }
        best = a + ab * (d1 / denom);
        return finish(best, p);
    }

    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return finish(c, p);
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let denom = d2 - d6;
        if denom == 0.0 {
            return finish(a, p);
        }
        best = a + ac * (d2 / denom);
        return finish(best, p);
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let denom = (d4 - d3) + (d5 - d6);
        if denom == 0.0 {
            return finish(b, p);
        }
        best = b + (c - b) * ((d4 - d3) / denom);
        return finish(best, p);
    }

    let denom = va + vb + vc;
    if denom == 0.0 {
        // Degenerate triangle: pick the nearest vertex.
        let mut nearest = a;
        for candidate in [b, c] {
            if (p - candidate).length_squared() < (p - nearest).length_squared() {
                nearest = candidate;
            }
        }
        return finish(nearest, p);
    }
    let v = vb / denom;
    let w = vc / denom;
    finish(a + ab * v + ac * w, p)
}

fn finish(point: DVec3, query: DVec3) -> Closest {
    let point = if point.is_finite() {
        point
    } else {
        DVec3::ZERO
    };
    Closest {
        point: from_vec(point),
        distance_sq: (point - query).length_squared(),
    }
}

/// Closest points between segments `p1`-`q1` and `p2`-`q2`.
pub fn segment_segment_closest(p1: P3, q1: P3, p2: P3, q2: P3) -> SegmentClosest {
    let p1 = to_vec(p1);
    let q1 = to_vec(q1);
    let p2 = to_vec(p2);
    let q2 = to_vec(q2);

    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.length_squared();
    let e = d2.length_squared();
    let f = d2.dot(r);

    let (mut s, mut t);
    if a <= f64::EPSILON && e <= f64::EPSILON {
        s = 0.0;
        t = 0.0;
    } else if a <= f64::EPSILON {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= f64::EPSILON {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            s = if denom != 0.0 {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
        }
    }
    let on_first = p1 + d1 * s;
    let on_second = p2 + d2 * t;
    SegmentClosest {
        on_first: from_vec(on_first),
        on_second: from_vec(on_second),
        distance_sq: (on_first - on_second).length_squared(),
    }
}

/// The point where segment `a`-`b` crosses `triangle`, if it does.
///
/// Coplanar and parallel segments report `None`; the caller's edge tests cover
/// those cases.
pub fn segment_triangle_hit(a: P3, b: P3, triangle: [P3; 3]) -> Option<P3> {
    const EPS: f64 = 1e-12;
    let origin = to_vec(a);
    let dir = to_vec(b) - origin;
    let v0 = to_vec(triangle[0]);
    let v1 = to_vec(triangle[1]);
    let v2 = to_vec(triangle[2]);
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let h = dir.cross(edge2);
    let det = edge1.dot(h);
    if det.abs() < EPS {
        return None;
    }
    let inv_det = 1.0 / det;
    let s = origin - v0;
    let u = s.dot(h) * inv_det;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = dir.dot(q) * inv_det;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    let t = edge2.dot(q) * inv_det;
    if !(-1e-9..=1.0 + 1e-9).contains(&t) {
        return None;
    }
    Some(from_vec(origin + dir * t.clamp(0.0, 1.0)))
}

/// Closest point on `triangle` to segment `a`-`b`, and their distance.
///
/// The minimum distance between a segment and a triangle is attained either at
/// a segment endpoint against the triangle face, or between the segment and one
/// of the triangle's edges, so those candidates are exhaustive.
pub fn segment_triangle_closest(a: P3, b: P3, triangle: [P3; 3]) -> Closest {
    if let Some(hit) = segment_triangle_hit(a, b, triangle) {
        return Closest {
            point: hit,
            distance_sq: 0.0,
        };
    }
    let mut best = point_triangle_closest(a, triangle);
    let end_b = point_triangle_closest(b, triangle);
    if end_b.distance_sq < best.distance_sq {
        best = end_b;
    }
    for k in 0..3 {
        let edge = segment_segment_closest(a, b, triangle[k], triangle[(k + 1) % 3]);
        if edge.distance_sq < best.distance_sq {
            best = Closest {
                point: edge.on_second,
                distance_sq: edge.distance_sq,
            };
        }
    }
    best
}

/// Squared distance from a point to an axis-aligned box; zero inside.
pub fn point_aabb_distance_sq(point: P3, min: P3, max: P3) -> f64 {
    let mut sum = 0.0;
    for i in 0..3 {
        let gap = if point[i] < min[i] {
            min[i] - point[i]
        } else if point[i] > max[i] {
            point[i] - max[i]
        } else {
            0.0
        };
        sum += gap * gap;
    }
    sum
}

/// Squared distance between two axis-aligned boxes; zero when they touch.
pub fn aabb_distance_sq(a_min: P3, a_max: P3, b_min: P3, b_max: P3) -> f64 {
    let mut sum = 0.0;
    for i in 0..3 {
        let gap = (b_min[i] - a_max[i]).max(a_min[i] - b_max[i]).max(0.0);
        sum += gap * gap;
    }
    sum
}

/// Axis-aligned bounds of a triangle.
pub fn triangle_bounds(triangle: [P3; 3]) -> (P3, P3) {
    let mut min = triangle[0];
    let mut max = triangle[0];
    for point in &triangle[1..] {
        for i in 0..3 {
            min[i] = min[i].min(point[i]);
            max[i] = max[i].max(point[i]);
        }
    }
    (min, max)
}

/// Centroid of a triangle.
pub fn triangle_centroid(triangle: [P3; 3]) -> P3 {
    [
        (triangle[0][0] + triangle[1][0] + triangle[2][0]) / 3.0,
        (triangle[0][1] + triangle[1][1] + triangle[2][1]) / 3.0,
        (triangle[0][2] + triangle[1][2] + triangle[2][2]) / 3.0,
    ]
}
