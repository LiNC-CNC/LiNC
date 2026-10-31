//! The geometry a G71/G72 lathe roughing cycle cuts, ported from LinuxCNC's
//! `interp_g7x.cc`.
//!
//! The preview used to approximate the layers: it stepped a profile offset
//! along one axis and clipped, which left the cuts on the finished surface
//! instead of `D` short of it and drew a finish pass that was not the one the
//! controller makes. The controller's own algorithm is a small, self-contained
//! piece of geometry: normalize the profile, offset it by `D` with arcs in the
//! inside corners, then walk layers against that offset. Porting it keeps the
//! drawing honest without linking the controller.
//!
//! Points are `(Z, X)` in machine units, mirroring the reference's complex
//! plane where the real part is Z and the imaginary part is X.
//!
//! Ported from LinuxCNC 2.9 `src/emc/rs274ngc/interp_g7x.cc` (GPL-2.0-or-later,
//! like LinuxCNC itself).

use std::ops::{Add, Div, Mul, Neg, Sub};

/// The reference's `tolerance`.
const TOL: f64 = 1e-6;

/// A point in the lathe plane: real part Z, imaginary part X.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Pt {
    pub z: f64,
    pub x: f64,
}

impl Pt {
    pub(crate) const fn new(z: f64, x: f64) -> Self {
        Self { z, x }
    }

    fn abs(self) -> f64 {
        self.z.hypot(self.x)
    }

    fn norm(self) -> f64 {
        self.z * self.z + self.x * self.x
    }

    fn conj(self) -> Self {
        Self::new(self.z, -self.x)
    }

    /// `i * self`, with `i` the imaginary unit of the reference's plane.
    fn mul_i(self) -> Self {
        Self::new(-self.x, self.z)
    }
}

impl Add for Pt {
    type Output = Pt;
    fn add(self, rhs: Pt) -> Pt {
        Pt::new(self.z + rhs.z, self.x + rhs.x)
    }
}

impl Sub for Pt {
    type Output = Pt;
    fn sub(self, rhs: Pt) -> Pt {
        Pt::new(self.z - rhs.z, self.x - rhs.x)
    }
}

impl Neg for Pt {
    type Output = Pt;
    fn neg(self) -> Pt {
        Pt::new(-self.z, -self.x)
    }
}

impl Mul for Pt {
    type Output = Pt;
    fn mul(self, rhs: Pt) -> Pt {
        Pt::new(
            self.z * rhs.z - self.x * rhs.x,
            self.z * rhs.x + self.x * rhs.z,
        )
    }
}

impl Div for Pt {
    type Output = Pt;
    fn div(self, rhs: Pt) -> Pt {
        let d = rhs.norm();
        Pt::new(
            (self.z * rhs.z + self.x * rhs.x) / d,
            (self.x * rhs.z - self.z * rhs.x) / d,
        )
    }
}

/// What a drawn move does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveKind {
    Rapid,
    Feed,
}

/// One move of the cycle's tool path, in the frame the caller gave the profile
/// in: `(Z, X)`. An arc carries its centre and direction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Move {
    pub kind: MoveKind,
    pub start: Pt,
    pub end: Pt,
    pub arc: Option<(Pt, bool)>,
}

/// A profile segment as the caller reads it from the program body.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProfileMove {
    pub start: Pt,
    pub end: Pt,
    /// Centre and counter-clockwise flag for an arc.
    pub arc: Option<(Pt, bool)>,
}

/// The words of one roughing block, in machine units (radius for X).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cycle {
    /// G72 faces in X; G71 turns in Z.
    pub facing: bool,
    /// `X` and `Z` where the cycle starts cutting.
    pub start_x: f64,
    pub start_z: f64,
    /// `D`: distance left on the profile.
    pub allow: f64,
    /// `I`: depth of each layer.
    pub increment: f64,
    /// `R`: clearance between layers.
    pub retract: f64,
}

impl Cycle {
    fn start(self) -> Pt {
        Pt::new(self.start_z, self.start_x)
    }
}

#[derive(Clone, Copy, Debug)]
enum SegKind {
    Line,
    Arc { center: Pt, ccw: bool },
}

#[derive(Clone, Debug)]
struct Segment {
    start: Pt,
    end: Pt,
    kind: SegKind,
}

impl Segment {
    fn line(start: Pt, end: Pt) -> Self {
        Self {
            start,
            end,
            kind: SegKind::Line,
        }
    }

    fn arc(ccw: bool, start: Pt, end: Pt, center: Pt) -> Self {
        Self {
            start,
            end,
            kind: SegKind::Arc { center, ccw },
        }
    }

    fn radius(&self) -> f64 {
        match self.kind {
            SegKind::Line => (self.start - self.end).abs(),
            SegKind::Arc { center, .. } => (self.start - self.end)
                .abs()
                .min((self.start - center).abs()),
        }
    }

    fn offset(&mut self, distance: f64) {
        match self.kind {
            SegKind::Line => {
                let d = self.start - self.end;
                let d = d.mul_i() * Pt::new(distance / d.abs(), 0.0);
                self.start = self.start + d;
                self.end = self.end + d;
            }
            SegKind::Arc { center, ccw } => {
                let radius = (self.start - center).abs();
                let factor = (radius + if ccw { 1.0 } else { -1.0 } * distance) / radius;
                self.start = (self.start - center) * Pt::new(factor, 0.0) + center;
                self.end = (self.end - center) * Pt::new(factor, 0.0) + center;
            }
        }
    }

    fn flip_imag(&mut self) {
        match &mut self.kind {
            SegKind::Line => {
                self.start = self.start.conj();
                self.end = self.end.conj();
            }
            SegKind::Arc { center, ccw } => {
                *ccw = !*ccw;
                self.start = self.start.conj();
                self.end = self.end.conj();
                *center = center.conj();
            }
        }
    }

    fn flip_real(&mut self) {
        match &mut self.kind {
            SegKind::Line => {
                self.start = -self.start.conj();
                self.end = -self.end.conj();
            }
            SegKind::Arc { center, ccw } => {
                *ccw = !*ccw;
                self.start = -self.start.conj();
                self.end = -self.end.conj();
                *center = -center.conj();
            }
        }
    }

    fn rotate(&mut self) {
        match &mut self.kind {
            SegKind::Line => {
                self.start = -self.start.mul_i();
                self.end = -self.end.mul_i();
            }
            SegKind::Arc { center, .. } => {
                self.start = -self.start.mul_i();
                self.end = -self.end.mul_i();
                *center = -center.mul_i();
            }
        }
    }

    fn move_by(&mut self, d: Pt) {
        self.start = self.start + d;
        self.end = self.end + d;
        if let SegKind::Arc { center, .. } = &mut self.kind {
            *center = *center + d;
        }
    }

    fn monotonic(&self) -> bool {
        match self.kind {
            SegKind::Line => (self.end.z - self.start.z) <= 1e-3,
            SegKind::Arc { center, ccw } => {
                let entry = self.start.x - center.x;
                let exit = self.end.x - center.x;
                let dz = self.end.z - self.start.z;
                if ccw {
                    entry >= -1e-3 && exit >= -1e-3 && dz <= -1e-3
                } else {
                    entry <= 1e-3 && exit <= 1e-3 && dz <= -1e-3
                }
            }
        }
    }

    /// The Z values at which the segment crosses the X plane `x`.
    fn intersection_z(&self, x: f64) -> Vec<f64> {
        let mut is = Vec::new();
        match self.kind {
            SegKind::Line => {
                if self.start.x.min(self.end.x) > x + TOL || self.start.x.max(self.end.x) < x - TOL
                {
                    return is;
                }
                if (self.start.x - self.end.x).abs() < TOL {
                    is.push(self.start.z);
                    is.push(self.end.z);
                } else {
                    is.push(
                        (x - self.start.x) / (self.end.x - self.start.x)
                            * (self.end.z - self.start.z)
                            + self.start.z,
                    );
                }
            }
            SegKind::Arc { center, ccw } => {
                let r = self.start - center;
                let mut s = -(x - r.abs() - center.x) * (x + r.abs() - center.x);
                if s < -TOL {
                    return is;
                }
                if s < 0.0 {
                    s = 0.0;
                }
                s = s.sqrt();
                for candidate in [center.z + s, center.z - s] {
                    let p = Pt::new(candidate, x);
                    if self.on_segment(p, ccw) {
                        is.push(candidate);
                    }
                }
            }
        }
        is
    }

    fn on_segment(&self, p: Pt, ccw: bool) -> bool {
        let SegKind::Arc { center, .. } = self.kind else {
            return true;
        };
        let start = (self.start - center).conj() * (p - center);
        let end = (self.end - center).conj() * (p - center);
        if ccw {
            start.x >= -TOL && end.x <= TOL
        } else {
            start.x <= TOL && end.x >= -TOL
        }
    }

    /// Follow the segment while climbing: `true` when it does not climb.
    fn climb(&self, location: &mut Pt, out: &mut dyn MotionSink) -> Result<bool, String> {
        match self.kind {
            SegKind::Line => {
                if self.end.x + TOL < self.start.x {
                    return Ok(true);
                }
                if (*location - self.start).abs() > TOL {
                    // The reference throws "How did we get here?".
                    return Err("the profile walk lost the surface".to_string());
                }
                out.straight(MoveKind::Feed, self.end);
                *location = self.end;
                Ok(false)
            }
            SegKind::Arc { center, ccw } => {
                if !ccw {
                    if location.z > center.z {
                        return Ok(true);
                    }
                    if (*location - self.end).abs() > 1e-3 {
                        out.circular(ccw, center, self.end);
                    }
                    *location = self.end;
                    Ok(false)
                } else {
                    if location.z < center.z {
                        return Ok(true);
                    }
                    let mut ep = self.end;
                    if self.end.z < center.z {
                        ep.z = center.z;
                        ep.x = center.x + (self.start - center).abs();
                    }
                    if (*location - ep).abs() > 1e-3 {
                        out.circular(ccw, center, ep);
                    }
                    *location = ep;
                    Ok(true)
                }
            }
        }
    }

    fn climb_only(&self, location: &mut Pt, out: &mut dyn MotionSink) {
        match self.kind {
            SegKind::Line => {
                if self.end.x < self.start.x {
                    return;
                }
                let is = self.intersection_z(location.x);
                if is.is_empty() {
                    return;
                }
                location.z = is[0];
                out.straight(MoveKind::Feed, *location);
                out.straight(MoveKind::Feed, self.end);
                *location = self.end;
            }
            SegKind::Arc { center, ccw } => {
                let is = self.intersection_z(location.x);
                let Some(front) = is.first().copied() else {
                    return;
                };
                let back = *is.last().unwrap_or(&front);
                if !ccw {
                    if back > center.z {
                        return;
                    }
                    location.z = back;
                    out.straight(MoveKind::Feed, *location);
                    if (*location - self.end).abs() > 1e-3 {
                        out.circular(ccw, center, self.end);
                    }
                    *location = self.end;
                } else {
                    if front < center.z {
                        return;
                    }
                    location.z = front;
                    out.straight(MoveKind::Feed, *location);
                    let mut ep = self.end;
                    if self.end.z < center.z {
                        ep.z = center.z;
                        ep.x = center.x + (self.start - center).abs();
                    }
                    if (*location - ep).abs() > 1e-3 {
                        out.circular(ccw, center, ep);
                    }
                    *location = ep;
                }
            }
        }
    }

    /// Move to the segment while diving: `true` when it is not a diving
    /// segment.
    fn dive(
        &self,
        location: &mut Pt,
        x: f64,
        out: &mut dyn MotionSink,
        fast: bool,
    ) -> Result<bool, String> {
        match self.kind {
            SegKind::Line => {
                if self.start.x <= self.end.x + TOL {
                    return Ok(true);
                }
                if x < self.end.x {
                    if fast {
                        out.straight(MoveKind::Rapid, self.end);
                    } else {
                        out.straight(MoveKind::Feed, self.end);
                    }
                    *location = self.end;
                    return Ok(false);
                }
                let is = self.intersection_z(x);
                let Some(z) = is.first().copied() else {
                    // The reference throws "x too large in straight dive".
                    return Err("a layer missed the straight surface".to_string());
                };
                let ep = Pt::new(z, x);
                if fast {
                    out.straight(MoveKind::Rapid, ep);
                } else {
                    out.straight(MoveKind::Feed, ep);
                }
                *location = ep;
                Ok(false)
            }
            SegKind::Arc { center, ccw } => {
                if (*location - self.end).abs() < TOL || location.z - self.end.z <= TOL {
                    *location = self.end;
                    return Ok(false);
                }
                let mut is = self.intersection_z(x);
                if is.is_empty() {
                    is = self.intersection_z(x - TOL);
                }
                let first = is.first().copied();
                let back = is.last().copied();
                if ccw {
                    if location.z - TOL > center.z {
                        return Ok(true);
                    }
                    if back.is_none_or(|back| back > center.z) {
                        out.circular(ccw, center, self.end);
                        *location = self.end;
                        Ok(false)
                    } else if let Some(back) = back {
                        let ep = Pt::new(back, x);
                        out.circular(ccw, center, ep);
                        *location = ep;
                        Ok(false)
                    } else {
                        Ok(false)
                    }
                } else {
                    if location.z <= center.z {
                        return Ok(true);
                    }
                    match first {
                        None => {
                            out.circular(ccw, center, self.end);
                            *location = self.end;
                            Ok(false)
                        }
                        Some(front) if front < center.z => Ok(false),
                        Some(front) => {
                            let ep = Pt::new(front, x);
                            out.circular(ccw, center, ep);
                            *location = ep;
                            Ok(false)
                        }
                    }
                }
            }
        }
    }
}

/// Correct the shared point of two segments to where their lines meet.
fn intersect_end(this: &mut Segment, previous: &mut Segment) {
    let (SegKind::Line, SegKind::Line) = (this.kind, previous.kind) else {
        intersect_end_round(this, previous);
        return;
    };
    let rot = (this.start - this.end).conj() / Pt::new((this.start - this.end).abs(), 0.0);
    let ps = (previous.start - this.end) * rot;
    let pe = (previous.end - this.end) * rot;
    if (ps.x - pe.x).abs() < 1e-12 {
        // Parallel lines never meet; the reference throws.
        return;
    }
    let f = ps.x / (ps.x - pe.x);
    let is = (ps + (pe - ps) * Pt::new(f, 0.0)) / rot + this.end;
    this.start = is;
    previous.end = is;
}

fn intersect_end_round(this: &mut Segment, previous: &mut Segment) {
    let (SegKind::Line, SegKind::Line) = (this.kind, previous.kind) else {
        // One of the two is an arc. The reference has a case for every pair.
        match (this.kind, previous.kind) {
            (SegKind::Line, SegKind::Arc { .. }) => {
                if (this.start - previous.end).abs() < TOL {
                    return;
                }
                let rot =
                    (this.start - this.end).conj() / Pt::new((this.start - this.end).abs(), 0.0);
                let pe = (previous.end - this.end) * rot;
                let (center, _) = arc_parts(&previous.kind);
                let pc = (center - this.end) * rot;
                let mut b = pc.norm() - pe.norm() - pc.x * pc.x;
                b = b.max(0.0).sqrt();
                let s1 = (Pt::new(pc.z + b, 0.0)) / rot + this.end;
                let s2 = (Pt::new(pc.z - b, 0.0)) / rot + this.end;
                let is = if (this.start - s1).abs() < (this.start - s2).abs() {
                    s1
                } else {
                    s2
                };
                this.start = is;
                previous.end = is;
            }
            (SegKind::Arc { .. }, SegKind::Line) => {
                if (this.start - previous.end).abs() < TOL {
                    return;
                }
                let rot = (previous.start - previous.end).conj()
                    / Pt::new((previous.start - previous.end).abs(), 0.0);
                let pe = (this.end - previous.end) * rot;
                let (center, _) = arc_parts(&this.kind);
                let pc = (center - previous.end) * rot;
                let mut b = pc.norm() - pe.norm() - pc.x * pc.x;
                b = b.max(0.0).sqrt();
                let s1 = Pt::new(pc.z + b, 0.0) / rot + previous.end;
                let s2 = Pt::new(pc.z - b, 0.0) / rot + previous.end;
                let is = if (this.start - s1).abs() < (this.start - s2).abs() {
                    s1
                } else {
                    s2
                };
                this.start = is;
                previous.end = is;
            }
            (SegKind::Arc { center, .. }, SegKind::Arc { .. }) => {
                let a = (this.start - center).abs();
                let previous_start = previous.start;
                let previous_kind = previous.kind;
                let (previous_center, _) = arc_parts(&previous_kind);
                let b = (previous_start - previous_center).abs();
                let c = (center - previous_center).abs();
                let cos_b = ((c * c + a * a - b * b) / 2.0 / a / c).clamp(-1.0, 1.0);
                let rot = Pt::new(cos_b, (1.0 - cos_b * cos_b).max(0.0).sqrt());
                let is =
                    rot * (previous_center - center) / Pt::new(c, 0.0) * Pt::new(a, 0.0) + center;
                this.start = is;
                previous.end = is;
            }
            (SegKind::Line, SegKind::Line) => unreachable!("handled above"),
        }
        return;
    };
    // Both are lines; the mixed cases were handled above.
}

fn arc_parts(kind: &SegKind) -> (Pt, bool) {
    match *kind {
        SegKind::Arc { center, ccw } => (center, ccw),
        SegKind::Line => (Pt::default(), false),
    }
}

/// Where the cycle writes its moves.
trait MotionSink {
    fn straight(&mut self, kind: MoveKind, end: Pt);
    fn circular(&mut self, ccw: bool, center: Pt, end: Pt);
}

/// Collects moves in the frame the caller gave, undoing the reference's
/// coordinate flips on the way in.
struct Swapped<'a> {
    swap: u8,
    out: &'a mut dyn MotionSink,
}

impl Swapped<'_> {
    fn point(&self, p: Pt) -> Pt {
        match self.swap {
            0 => p,
            1 => -p.conj(),
            2 => p.conj(),
            3 => -p,
            4 => p.mul_i(),
            5 => (p.mul_i()).conj(),
            6 => -(p.mul_i()).conj(),
            7 => -(p.mul_i()),
            _ => p,
        }
    }
}

impl MotionSink for Swapped<'_> {
    fn straight(&mut self, kind: MoveKind, end: Pt) {
        let end = self.point(end);
        self.out.straight(kind, end);
    }

    fn circular(&mut self, ccw: bool, center: Pt, end: Pt) {
        let center = self.point(center);
        let end = self.point(end);
        let ccw = match self.swap {
            0 | 3 | 4 | 7 => ccw,
            1 | 2 | 5 | 6 => !ccw,
            _ => ccw,
        };
        self.out.circular(ccw, center, end);
    }
}

/// Records moves as they arrive.
struct Collector {
    moves: Vec<Move>,
    at: Option<Pt>,
}

impl MotionSink for Collector {
    fn straight(&mut self, kind: MoveKind, end: Pt) {
        let start = self.at.unwrap_or(end);
        if (end - start).abs() > 1e-12 {
            self.moves.push(Move {
                kind,
                start,
                end,
                arc: None,
            });
        }
        self.at = Some(end);
    }

    fn circular(&mut self, ccw: bool, center: Pt, end: Pt) {
        let start = self.at.unwrap_or(end);
        if (end - start).abs() > 1e-12 {
            self.moves.push(Move {
                kind: MoveKind::Feed,
                start,
                end,
                arc: Some((center, ccw)),
            });
        }
        self.at = Some(end);
    }
}

struct G7x {
    segs: Vec<Segment>,
    delta: f64,
    escape: Pt,
    flip_state: u8,
    pocket_starts: Vec<Pt>,
}

impl G7x {
    fn front(&self) -> &Segment {
        &self.segs[0]
    }

    fn front_mut(&mut self) -> &mut Segment {
        &mut self.segs[0]
    }

    fn back(&self) -> &Segment {
        &self.segs[self.segs.len() - 1]
    }

    fn rotate(&mut self) {
        for seg in &mut self.segs {
            seg.rotate();
        }
        self.flip_state ^= 4;
    }

    /// Flip the profile so Z and X decrease, as the reference normalizes it.
    /// Drop segments the offset collapsed, as `do_finish` does after pushing
    /// the corner finishes (the preview has no fillet or chamfer words).
    fn do_finish(&mut self) {
        self.segs.retain(|seg| seg.radius() >= 1e-3);
    }

    fn swap(&mut self) {
        let dir_x = self.front().end.x - self.back().end.x;
        let dir_z = self.front().end.z - self.back().end.z;
        if dir_x > 0.0 {
            for seg in &mut self.segs {
                seg.flip_imag();
            }
            self.flip_state ^= 2;
        }
        if dir_z < 0.0 {
            for seg in &mut self.segs {
                seg.flip_real();
            }
            self.flip_state ^= 1;
        }
    }

    fn motion<'a>(&self, out: &'a mut dyn MotionSink) -> Swapped<'a> {
        Swapped {
            swap: self.flip_state,
            out,
        }
    }

    fn ensure_monotonic(&mut self) -> Result<(), String> {
        if self.front().end.z - self.front().start.z > 0.0 {
            self.front_mut().start.z = self.front().end.z;
        }
        if self.segs.iter().any(|seg| !seg.monotonic()) {
            return Err("the profile is not monotonic".to_string());
        }
        Ok(())
    }

    /// Place the profile `distance` from the finished surface, with an arc in
    /// each corner the offset opens up. Mirrors `g7x::add_distance`.
    fn add_distance(&mut self, distance: f64) -> Result<(), String> {
        let v1 = self.front().end - self.front().start;
        let v2 = self.back().start - self.front().start;
        let angle = v1 / v2;
        let distance = if angle.x < 0.0 { -distance } else { distance };
        let mut of = self.segs.remove(0);
        let mut current_distance = 0.0;
        let mut guard = 0;
        while current_distance != distance {
            guard += 1;
            if guard > 4096 {
                return Err("the profile could not be offset".to_string());
            }
            let mut max_distance = 1e9f64;
            for seg in &self.segs {
                max_distance = max_distance.min(seg.radius() / 2.0);
            }
            max_distance = max_distance.min((distance - current_distance).abs());
            if distance < 0.0 {
                max_distance = -max_distance;
            }
            if max_distance.abs() < 1e-12 {
                return Err("the profile could not be offset".to_string());
            }
            for seg in &mut self.segs {
                seg.offset(max_distance);
            }
            self.segs.retain(|seg| seg.radius() >= 1e-3);
            if self.segs.is_empty() {
                return Err("the profile disappeared while offsetting".to_string());
            }

            // Insert connecting arcs where the offset segments pull apart.
            let mut i = 0;
            while i + 1 < self.segs.len() {
                if (self.segs[i].end - self.segs[i + 1].start).z > 1e-2 {
                    let mut s = self.segs[i].clone();
                    let mut e = self.segs[i + 1].clone();
                    s.offset(-max_distance);
                    e.offset(-max_distance);
                    let center = (s.end + e.start) / Pt::new(2.0, 0.0);
                    let arc = Segment::arc(
                        distance > 0.0,
                        self.segs[i].end,
                        self.segs[i + 1].start,
                        center,
                    );
                    self.segs.insert(i + 1, arc);
                    i += 1;
                }
                i += 1;
            }

            // Drop segments the offset collapsed, then make the neighbours'
            // shared point the one place they meet.
            let mut i = 0;
            while i + 1 < self.segs.len() {
                if !self.segs[i].monotonic() {
                    let remove = if self.segs[i].radius() < self.segs[i - 1].radius() {
                        i
                    } else {
                        i - 1
                    };
                    self.segs.remove(remove);
                    i = 0;
                    continue;
                }
                if self.segs[i].radius() < 1e-3 {
                    self.segs.remove(i);
                    i = 0;
                    continue;
                }
                {
                    let (left, right) = self.segs.split_at_mut(i + 1);
                    // The reference calls `n->intersect_end(p)`: the next
                    // segment's start and this one's end move to where the two
                    // meet.
                    intersect_end(&mut right[0], &mut left[i]);
                }
                i += 1;
            }
            current_distance += max_distance;
        }
        of.end = self.front().start;
        self.segs.insert(0, of);
        Ok(())
    }

    /// Cut the pocket the profile bounds. Mirrors `g7x::pocket`.
    fn pocket(
        &mut self,
        cycle: u8,
        mut location: Pt,
        mut p: usize,
        out: &mut dyn MotionSink,
    ) -> Result<(), String> {
        let mut x = self
            .pocket_starts
            .last()
            .copied()
            .ok_or_else(|| "no pocket start".to_string())?
            .x;

        let mut cycle = cycle;
        if cycle == 2 {
            while p < self.segs.len() {
                if self.segs[p].dive(&mut location, -1e9, out, p == 0)? {
                    break;
                }
                p += 1;
            }
            cycle = 3;
        }

        let mut guard = 0;
        while p < self.segs.len() {
            guard += 1;
            if guard > 4096 {
                return Err("the roughing walk did not converge".to_string());
            }
            while x - TOL > location.x {
                x -= self.delta;
            }
            if self.segs[p].dive(&mut location, x, out, p == 0)? {
                if cycle == 1 {
                    while p < self.segs.len() {
                        self.segs[p].climb_only(&mut location, out);
                        p += 1;
                    }
                } else {
                    while p < self.segs.len() {
                        if self.segs[p]
                            .climb(&mut location, out)
                            .map_err(|error| format!("{error}: segment {p} at {location:?}"))?
                        {
                            if cycle == 3 {
                                if let Some(pocket) = self.pocket_starts.last().copied() {
                                    if location.x > pocket.x && self.pocket_starts.len() > 1 {
                                        self.pocket_starts.pop();
                                    }
                                }
                                if self.pocket_starts.len() == 1 {
                                    self.pocket_starts.push(location);
                                }
                            }
                            return self.pocket(cycle, location, p, out);
                        }
                        p += 1;
                    }
                }
                return Ok(());
            }
            if (location.x - x).abs() > TOL || self.segs[p].end == location {
                p += 1;
                continue;
            }

            let mut moved_on = false;
            for ip in p..self.segs.len() {
                let is = self.segs[ip].intersection_z(location.x);
                if is.is_empty() {
                    continue;
                }
                let destination_z = if ip != p {
                    is[0]
                } else {
                    is.last().copied().unwrap_or(is[0])
                };
                let distance = (destination_z - location.z).abs();
                if destination_z - TOL > location.z {
                    continue;
                }
                if distance < TOL {
                    if p == ip {
                        continue;
                    } else if (location - self.segs[ip].start).abs() < TOL {
                        p = ip;
                        moved_on = true;
                        break;
                    } else {
                        x -= self.delta;
                        break;
                    }
                }

                let mut dest = location;
                dest.z = destination_z;
                out.straight(MoveKind::Feed, dest);
                let escape_scale = (distance / 2.0 / self.escape.z).min(1.0);
                let scaled = Pt::new(self.escape.z * escape_scale, self.escape.x * escape_scale);
                out.straight(MoveKind::Rapid, dest + scaled);
                out.straight(MoveKind::Rapid, location - Pt::new(scaled.z, -scaled.x));
                if p == 0 {
                    out.straight(MoveKind::Rapid, location);
                } else {
                    out.straight(MoveKind::Feed, location);
                }
                x -= self.delta;
                moved_on = true;
                break;
            }
            if !moved_on {
                return Err("the roughing walk found no profile to cut".to_string());
            }
        }
        Ok(())
    }

    fn do_g71(
        &mut self,
        out: &mut dyn MotionSink,
        cycle: u8,
        x: f64,
        z: f64,
        u: f64,
        w: f64,
        d: f64,
        i: f64,
        r: f64,
        do_rotate: bool,
    ) -> Result<(), String> {
        self.front_mut().start = Pt::new(z, x);
        if do_rotate {
            self.rotate();
        }
        self.swap();
        self.do_finish();
        self.ensure_monotonic()?;
        self.add_distance(d)?;
        let displacement = Pt::new(w, u);
        for seg in &mut self.segs {
            seg.move_by(displacement);
        }
        let mut swapped_out = self.motion(out);

        self.delta = i.max(2.0 * TOL);
        self.escape = Pt::new(r, r);

        let front_start = self.front().start;
        swapped_out.straight(MoveKind::Rapid, front_start);
        if self.back().end.x < front_start.x {
            let mut ep = self.back().end;
            ep.x = front_start.x;
            self.segs.push(Segment::line(self.back().end, ep));
        }
        self.pocket_starts.push(front_start);
        self.pocket(cycle, front_start, 0, &mut swapped_out)
    }

    fn do_g72(
        &mut self,
        out: &mut dyn MotionSink,
        cycle: u8,
        x: f64,
        z: f64,
        u: f64,
        w: f64,
        d: f64,
        i: f64,
        r: f64,
    ) -> Result<(), String> {
        self.do_g71(out, cycle, x, z, u, w, d, i, r, true)
    }
}

/// Plan the moves one roughing cycle cuts, in the frame the profile arrives in.
pub(crate) fn roughing(profile: &[ProfileMove], cycle: Cycle) -> Result<Vec<Move>, String> {
    if profile.len() < 2 {
        return Ok(Vec::new());
    }
    let mut segs = Vec::with_capacity(profile.len());
    let mut previous = cycle.start();
    for (index, move_) in profile.iter().enumerate() {
        let start = if index == 0 { cycle.start() } else { previous };
        let mut seg = match move_.arc {
            Some((center, ccw)) => Segment::arc(ccw, start, move_.end, center),
            None => Segment::line(start, move_.end),
        };
        if index == 0 {
            seg.start = cycle.start();
        }
        segs.push(seg);
        previous = move_.end;
    }
    if segs.is_empty() {
        return Ok(Vec::new());
    }
    // The reference closes the profile onto the cycle's limit before it walks
    // it: a G72 profile that ends still diving gets a segment at its last X out
    // to the block's Z, and a G71 profile one at its last Z out to the block's
    // X. Without it the finish pass stops at the profile instead of the face
    // the cycle names.
    let Some(first_seg) = segs.first() else {
        return Ok(Vec::new());
    };
    let Some(last_seg) = segs.last() else {
        return Ok(Vec::new());
    };
    let last_end = last_seg.end;
    let dback = last_seg.end - last_seg.start;
    let first = first_seg.end - first_seg.start;
    if cycle.facing {
        if dback.x.abs() > 0.0 && first.z * (cycle.start_z - last_end.z) < 0.0 {
            let end = Pt::new(cycle.start_z, last_end.x);
            segs.push(Segment::line(last_end, end));
        }
    } else if dback.z.abs() > 0.0 && first.x * (cycle.start_x - last_end.x) < 0.0 {
        let end = Pt::new(last_end.z, cycle.start_x);
        segs.push(Segment::line(last_end, end));
    }
    let mut g7x = G7x {
        segs,
        delta: 1.0,
        escape: Pt::new(0.5, 0.5),
        flip_state: 0,
        pocket_starts: Vec::new(),
    };
    let mut collector = Collector {
        moves: Vec::new(),
        at: Some(profile[0].start),
    };
    if cycle.facing {
        g7x.do_g72(
            &mut collector,
            0,
            cycle.start_x,
            cycle.start_z,
            0.0,
            0.0,
            cycle.allow,
            cycle.increment,
            cycle.retract,
        )?;
    } else {
        g7x.do_g71(
            &mut collector,
            0,
            cycle.start_x,
            cycle.start_z,
            0.0,
            0.0,
            cycle.allow,
            cycle.increment,
            cycle.retract,
            false,
        )?;
    }
    Ok(collector.moves)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The G72 half of `configs/gcode/g7x-probe.ngc`: cycle point (X1, Z1),
    /// profile (X0 Z-10), (X4 Z-10), (X4 Z-6), (X8 Z-6), (X8 Z-2).
    fn facing_profile() -> Vec<ProfileMove> {
        let points = [
            Pt::new(-10.0, 0.0),
            Pt::new(-10.0, 4.0),
            Pt::new(-6.0, 4.0),
            Pt::new(-6.0, 8.0),
            Pt::new(-2.0, 8.0),
        ];
        let mut moves = Vec::new();
        let mut start = Pt::new(2.0, 0.0);
        for point in points {
            moves.push(ProfileMove {
                start,
                end: point,
                arc: None,
            });
            start = point;
        }
        moves
    }

    fn facing_cycle() -> Cycle {
        Cycle {
            facing: true,
            start_x: 1.0,
            start_z: 1.0,
            allow: 0.5,
            increment: 1.0,
            retract: 0.3,
        }
    }

    #[test]
    fn the_facing_cycle_cuts_the_fixtures_real_path() {
        // The values are the ones `/usr/bin/rs274 -i configs/lathe-sim.ini`
        // prints for this program: the pass planes, the `D` allowance on both
        // the diameter and the face, and the arc in the shoulder corner.
        let moves = roughing(&facing_profile(), facing_cycle()).expect("the cycle runs");
        let feeds: Vec<(f64, f64, f64, f64)> = moves
            .iter()
            .filter(|move_| move_.kind == MoveKind::Feed)
            .map(|move_| (move_.start.z, move_.start.x, move_.end.z, move_.end.x))
            .collect();
        assert_eq!(
            feeds,
            vec![
                (1.0, 0.0, 1.0, 7.5),
                (0.0, 0.0, 0.0, 7.5),
                (-1.0, 0.0, -1.0, 7.5),
                (-2.0, 0.0, -2.0, 7.5),
                (-3.0, 0.0, -3.0, 7.5),
                (-4.0, 0.0, -4.0, 7.5),
                (-5.0, 0.0, -5.0, 7.5),
                (-6.0, 0.0, -6.0, 3.5),
                (-7.0, 0.0, -7.0, 3.5),
                (-8.0, 0.0, -8.0, 3.5),
                (-9.0, 0.0, -9.0, 3.5),
                (-9.5, 0.0, -9.5, 3.5),
                (-9.5, 3.5, -6.0, 3.5),
                (-6.0, 3.5, -5.5, 4.0),
                (-5.5, 4.0, -5.5, 7.5),
                (-5.5, 7.5, -2.0, 7.5),
                (-2.0, 7.5, 1.0, 7.5),
            ]
        );
        // The shoulder corner is rounded with the radius `D` the finish pass
        // leaves, centred on the profile corner.
        let arc = moves
            .iter()
            .find(|move_| move_.arc.is_some())
            .expect("a corner arc");
        let (center, ccw) = arc.arc.expect("arc data");
        assert!((center.z + 6.0).abs() < 1e-9 && (center.x - 4.0).abs() < 1e-9);
        assert!(ccw, "G3, as the controller emits it");
    }

    #[test]
    fn a_cycle_that_starts_at_the_tool_position_still_cuts_the_profile() {
        // A block without X or Z starts where the tool is; that is the point
        // the profile's first segment starts from.
        let cycle = Cycle {
            start_x: 0.0,
            start_z: 2.0,
            ..facing_cycle()
        };
        let moves = roughing(&facing_profile(), cycle).expect("the cycle runs");
        assert!(moves.iter().any(|move_| move_.kind == MoveKind::Feed));
    }
}
