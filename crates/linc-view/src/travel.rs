//! Which part of a move lies outside the machine's travel envelope.
//!
//! The envelope is the box from the machine INI's per-axis `MIN_LIMIT` and
//! `MAX_LIMIT` (in machine coordinates, as the preview is drawn). The
//! classification is pure so the painting code can split a move at the exact
//! crossing point and the rules are testable without a window.

use linc_core::gcode::Bounds3;

/// Tolerance for treating a coordinate delta or a point-on-boundary comparison
/// as zero. Travel is in millimetres or inches; this is far below either.
const EPS: f64 = 1e-9;

/// Where a straight move lies relative to the machine travel box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TravelClass {
    /// The whole move lies within the envelope.
    Inside,
    /// No point of the move lies within the envelope.
    Outside,
    /// The move crosses the envelope boundary. `enter` and `exit` are the
    /// positions along the move, from `start` (0.0) to `end` (1.0), where it
    /// enters and leaves the travel box; they are equal when the move only
    /// touches a corner or an edge.
    Straddling { enter: f64, exit: f64 },
}

impl TravelClass {
    /// The part of the move inside the envelope, as `(enter, exit)` fractions
    /// from the start. `None` when the move never enters it.
    pub fn inside_span(self) -> Option<(f64, f64)> {
        match self {
            Self::Inside => Some((0.0, 1.0)),
            Self::Outside => None,
            Self::Straddling { enter, exit } if exit > enter => Some((enter, exit)),
            Self::Straddling { .. } => None,
        }
    }
}

/// Interpolate a point along a move. The result is in the same machine
/// coordinates as `start` and `end`.
pub fn point_at(start: [f64; 3], end: [f64; 3], t: f64) -> [f64; 3] {
    [
        start[0] + (end[0] - start[0]) * t,
        start[1] + (end[1] - start[1]) * t,
        start[2] + (end[2] - start[2]) * t,
    ]
}

/// Classify a straight move against the machine travel box.
///
/// Boundaries are inclusive: a point exactly on `MIN_LIMIT` or `MAX_LIMIT` is
/// inside, because it is still reachable travel. A zero-length move is judged
/// by its point. A move whose endpoints are both outside but which passes
/// through the box is `Straddling`, not `Outside`. An axis whose limits are not
/// finite is treated as unconstrained.
pub fn classify_segment(start: [f64; 3], end: [f64; 3], limits: &Bounds3) -> TravelClass {
    if !start.iter().all(|value| value.is_finite()) || !end.iter().all(|value| value.is_finite()) {
        return TravelClass::Outside;
    }
    if start
        .iter()
        .zip(end.iter())
        .all(|(a, b)| (b - a).abs() <= EPS)
    {
        return if point_inside(start, limits) {
            TravelClass::Inside
        } else {
            TravelClass::Outside
        };
    }

    // The travel box is convex, so the part of a line inside it is a single
    // interval. Intersect each axis's interval, in move fractions, to find it.
    let mut enter = 0.0_f64;
    let mut exit = 1.0_f64;
    for axis in 0..3 {
        let Some((min, max)) = axis_bounds(limits, axis) else {
            continue;
        };
        let delta = end[axis] - start[axis];
        if delta.abs() <= EPS {
            // This axis does not move: it is inside only if its fixed value is.
            if start[axis] < min - EPS || start[axis] > max + EPS {
                return TravelClass::Outside;
            }
            continue;
        }
        let first = (min - start[axis]) / delta;
        let second = (max - start[axis]) / delta;
        enter = enter.max(first.min(second));
        exit = exit.min(first.max(second));
        if enter > exit {
            return TravelClass::Outside;
        }
    }

    if enter <= 0.0 && exit >= 1.0 {
        TravelClass::Inside
    } else {
        TravelClass::Straddling {
            enter: enter.clamp(0.0, 1.0),
            exit: exit.clamp(0.0, 1.0),
        }
    }
}

fn point_inside(point: [f64; 3], limits: &Bounds3) -> bool {
    for axis in 0..3 {
        let Some((min, max)) = axis_bounds(limits, axis) else {
            continue;
        };
        if point[axis] < min - EPS || point[axis] > max + EPS {
            return false;
        }
    }
    true
}

fn axis_bounds(limits: &Bounds3, axis: usize) -> Option<(f64, f64)> {
    let (min, max) = (limits.min[axis], limits.max[axis]);
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    Some(if min <= max { (min, max) } else { (max, min) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> Bounds3 {
        Bounds3 {
            min: [-10.0, -10.0, -10.0],
            max: [10.0, 10.0, 10.0],
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn a_move_inside_the_envelope_is_inside() {
        // On the boundary counts as reachable travel, not a violation.
        assert_eq!(
            classify_segment([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], &envelope()),
            TravelClass::Inside
        );
        assert_eq!(
            classify_segment([-10.0, -10.0, -10.0], [0.0, 0.0, 0.0], &envelope()),
            TravelClass::Inside
        );
    }

    #[test]
    fn a_move_outside_the_envelope_is_outside() {
        assert_eq!(
            classify_segment([20.0, 0.0, 0.0], [30.0, 0.0, 0.0], &envelope()),
            TravelClass::Outside
        );
        // One fixed axis outside makes the whole move outside.
        assert_eq!(
            classify_segment([0.0, 20.0, 0.0], [10.0, 20.0, 0.0], &envelope()),
            TravelClass::Outside
        );
    }

    #[test]
    fn a_straddling_move_splits_where_it_leaves_the_envelope() {
        // Inside to outside: the boundary is halfway along the move.
        assert_eq!(
            classify_segment([0.0, 0.0, 0.0], [20.0, 0.0, 0.0], &envelope()),
            TravelClass::Straddling {
                enter: 0.0,
                exit: 0.5
            }
        );
        // Outside to inside: the boundary is halfway as well, but now it is
        // the entry point.
        assert_eq!(
            classify_segment([20.0, 0.0, 0.0], [0.0, 0.0, 0.0], &envelope()),
            TravelClass::Straddling {
                enter: 0.5,
                exit: 1.0
            }
        );
    }

    #[test]
    fn a_move_that_crosses_the_envelope_is_straddling() {
        // Both endpoints outside, but the line passes through the travel box.
        // This is the case a two-endpoint "either endpoint outside" test would
        // wrongly paint entirely red.
        assert_eq!(
            classify_segment([-20.0, 0.0, 0.0], [20.0, 0.0, 0.0], &envelope()),
            TravelClass::Straddling {
                enter: 0.25,
                exit: 0.75
            }
        );
    }

    #[test]
    fn a_zero_length_move_is_judged_by_its_point() {
        assert_eq!(
            classify_segment([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], &envelope()),
            TravelClass::Inside
        );
        assert_eq!(
            classify_segment([20.0, 0.0, 0.0], [20.0, 0.0, 0.0], &envelope()),
            TravelClass::Outside
        );
        assert_eq!(
            classify_segment([10.0, 10.0, 10.0], [10.0, 10.0, 10.0], &envelope()),
            TravelClass::Inside
        );
    }

    #[test]
    fn a_diagonal_crosses_two_different_limits() {
        // X enters at its minimum (t = 0.5), Y leaves at its maximum
        // (t = 0.8): two different limits, so two different crossings.
        let limits = Bounds3 {
            min: [0.0, 0.0, 0.0],
            max: [10.0, 10.0, 10.0],
        };
        let class = classify_segment([-5.0, 2.0, 5.0], [5.0, 12.0, 5.0], &limits);
        let TravelClass::Straddling { enter, exit } = class else {
            panic!("expected a straddling move, got {class:?}");
        };
        assert_close(enter, 0.5);
        assert_close(exit, 0.8);
    }

    #[test]
    fn a_diagonal_through_a_corner_touches_both_limits_at_once() {
        // Through the corner (-10, -10): both axes cross at the same fraction.
        let class = classify_segment([-20.0, -20.0, 0.0], [0.0, 0.0, 0.0], &envelope());
        let TravelClass::Straddling { enter, exit } = class else {
            panic!("expected a straddling move, got {class:?}");
        };
        assert_close(enter, 0.5);
        assert_close(exit, 1.0);
    }

    #[test]
    fn inside_span_reports_the_painted_part() {
        assert_eq!(TravelClass::Inside.inside_span(), Some((0.0, 1.0)));
        assert_eq!(TravelClass::Outside.inside_span(), None);
        assert_eq!(
            TravelClass::Straddling {
                enter: 0.25,
                exit: 0.75
            }
            .inside_span(),
            Some((0.25, 0.75))
        );
        assert_eq!(
            TravelClass::Straddling {
                enter: 0.5,
                exit: 0.5
            }
            .inside_span(),
            None
        );
    }
}
