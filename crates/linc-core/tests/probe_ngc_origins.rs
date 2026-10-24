//! Static guard for the DIRECT start-axis origin of the four top edge-angle
//! routines (review finding PR-02).
//!
//! A DIRECT run stores raw `G10 L2` offsets, so the captured start position
//! (`#5420`/`#5421`, a work coordinate) needs the source WCS origin added on
//! the captured start axis exactly like the probed axis. Both rotation branches
//! must carry the term: the two `o130`/`o140` blocks differ only by
//! `R[#<edge_angle>]`. The G90 completion move must NOT carry it; it is issued
//! in work coordinates. These tests read the shipped subroutines, so dropping a
//! term fails here instead of silently writing the wrong offset on the machine.

use std::fs;
use std::path::{Path, PathBuf};

fn routine(name: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../subroutines/probe_basic")
        .join(format!("{name}.ngc"));
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

struct Case {
    routine: &'static str,
    /// The origin capture the start-axis term needs, on the *other* axis of the
    /// probed edge.
    origin_capture: &'static str,
    /// Start-axis term, work position plus source origin.
    start_term: &'static str,
    /// Probed-axis term, which already added its origin before this fix.
    probed_term: &'static str,
}

const CASES: [Case; 4] = [
    Case {
        routine: "probe_top_front_edge_angle",
        origin_capture: "#<workspace_x> = #[5201 + [20 * #5220]]",
        start_term: "X[#<x_start_position> + #<workspace_x>]",
        probed_term: "Y[#<y_plus_zero_edge_start> + #<workspace_y>]",
    },
    Case {
        routine: "probe_top_back_edge_angle",
        origin_capture: "#<workspace_x> = #[5201 + [20 * #5220]]",
        start_term: "X[#<x_start_position> + #<workspace_x>]",
        probed_term: "Y[#<y_minus_zero_edge_start> + #<workspace_y>]",
    },
    Case {
        routine: "probe_top_left_edge_angle",
        origin_capture: "#<workspace_y> = #[5202 + [20 * #5220]]",
        start_term: "Y[#<y_start_position> + #<workspace_y>]",
        probed_term: "X[#<x_plus_zero_edge_start> + #<workspace_x>]",
    },
    Case {
        routine: "probe_top_right_edge_angle",
        origin_capture: "#<workspace_y> = #[5202 + [20 * #5220]]",
        start_term: "Y[#<y_start_position> + #<workspace_y>]",
        probed_term: "X[#<x_minus_zero_edge_start> + #<workspace_x>]",
    },
];

fn g10_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.trim_start().starts_with("G10 L2 P#5220 "))
        .collect()
}

#[test]
fn both_direct_branches_add_the_start_axis_origin() {
    for case in &CASES {
        let text = routine(case.routine);
        let name = case.routine;
        assert!(
            text.contains(case.origin_capture),
            "{name}.ngc must capture the start-axis origin as {}",
            case.origin_capture
        );

        let lines = g10_lines(&text);
        assert_eq!(
            lines.len(),
            2,
            "{name}.ngc needs exactly the two DIRECT branches, found {lines:?}"
        );
        // One branch without rotation, one with the requested rotation.
        assert!(
            lines.iter().any(|line| !line.contains("R[#<edge_angle>]")),
            "{name}.ngc lost the no-rotation DIRECT branch"
        );
        assert!(
            lines.iter().any(|line| line.contains("R[#<edge_angle>]")),
            "{name}.ngc lost the rotated DIRECT branch"
        );
        for line in &lines {
            assert!(
                line.contains(case.start_term),
                "{name}.ngc DIRECT branch must store {} , found {line}",
                case.start_term
            );
            assert!(
                line.contains(case.probed_term),
                "{name}.ngc DIRECT branch must keep the probed-axis term {} , found {line}",
                case.probed_term
            );
            // The plain work coordinate alone is the PR-02 defect.
            let bare = case.start_term.split(" + ").next().unwrap();
            assert!(
                !line.contains(&format!("{bare}]")),
                "{name}.ngc still records the bare start position {bare}] without its origin"
            );
        }

        for line in text
            .lines()
            .filter(|line| line.trim_start().starts_with("G38.3 "))
        {
            assert!(
                !line.contains("workspace_"),
                "{name}.ngc probes/completes in work coordinates, so no G38.3 line may add a \
                 WCS origin: {line}"
            );
        }
    }
}

#[test]
fn every_top_angle_routine_captures_both_workspace_origins() {
    for case in &CASES {
        let text = routine(case.routine);
        for capture in [
            "#<workspace_x> = #[5201 + [20 * #5220]]",
            "#<workspace_y> = #[5202 + [20 * #5220]]",
        ] {
            assert_eq!(
                text.matches(capture).count(),
                1,
                "{} must capture {capture} exactly once",
                case.routine
            );
        }
    }
}
