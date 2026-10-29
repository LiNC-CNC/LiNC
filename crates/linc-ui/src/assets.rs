//! Embedded help figures. Kept in the binary so offline help works anywhere.

macro_rules! images {
    ($($name:literal),* $(,)?) => {
        pub fn image_bytes(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../../../assets/help/images/", $name))),)*
                _ => None,
            }
        }
    };
}

macro_rules! probe_help_images {
    ($($name:literal),* $(,)?) => {
        pub fn probe_help_image_bytes(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../../../assets/probe/help/", $name))),)*
                _ => None,
            }
        }
    };
}

macro_rules! probe_icons {
    ($($name:literal),* $(,)?) => {
        pub fn probe_icon_bytes(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../../../assets/probe/icons/", $name))),)*
                _ => None,
            }
        }
    };
}

probe_icons!(
    "back_left_inside_corner.png",
    "back_left_top_corner.png",
    "back_right_inside_corner.png",
    "back_right_top_corner.png",
    "back_top_side.png",
    "cal_round_boss.png",
    "cal_round_pocket.png",
    "cal_square_boss.png",
    "cal_square_pocket.png",
    "corner_x_minus_edge_angle.png",
    "corner_x_plus_edge_angle.png",
    "corner_y_minus_edge_angle.png",
    "corner_y_plus_edge_angle.png",
    "edge_x_minus.png",
    "edge_x_plus.png",
    "edge_y_minus.png",
    "edge_y_plus.png",
    "front_left_inside_corner.png",
    "front_left_top_corner.png",
    "front_right_inside_corner.png",
    "front_right_top_corner.png",
    "front_top_side.png",
    "left_top_side.png",
    "rect_boss.png",
    "rect_pocket.png",
    "rect_pocket_center.png",
    "ridge_x.png",
    "ridge_y.png",
    "right_top_side.png",
    "round_boss.png",
    "round_pocket.png",
    "round_pocket_center.png",
    "surface_z.png",
    "top_back_edge_angle.png",
    "top_front_edge_angle.png",
    "top_left_edge_angle.png",
    "top_right_edge_angle.png",
    "valley_x.png",
    "valley_x_center.png",
    "valley_y.png",
    "valley_y_center.png",
);

images!(
    "eight-holes_en.png",
    "g2-3_en.png",
    "g2_en.png",
    "g76-01.png",
    "g76-threads_en.png",
    "g81_en.png",
    "g81a_en.png",
    "g81ex1_en.png",
    "g81ex2_en.png",
    "g81mult_en.png",
    "nurbs01.png",
    "polar01.png",
    "polar02.png",
    "twelve-holes_en.png",
);

probe_help_images!(
    "boss-and-pocket.png",
    "calibrate.png",
    "edge-angle.png",
    "extra-probe-depth.svg",
    "outside-corners.png",
    "parameters.png",
    "probing-hints.png",
    "results.png",
    "step-off-width.svg",
    "work-offsets.png",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fourteen_figures_are_embedded() {
        for name in [
            "eight-holes_en.png",
            "g2-3_en.png",
            "g2_en.png",
            "g76-01.png",
            "g76-threads_en.png",
            "g81_en.png",
            "g81a_en.png",
            "g81ex1_en.png",
            "g81ex2_en.png",
            "g81mult_en.png",
            "nurbs01.png",
            "polar01.png",
            "polar02.png",
            "twelve-holes_en.png",
        ] {
            assert!(image_bytes(name).is_some(), "missing {name}");
        }
    }

    #[test]
    fn probe_help_figures_are_embedded() {
        for name in [
            "boss-and-pocket.png",
            "calibrate.png",
            "edge-angle.png",
            "extra-probe-depth.svg",
            "outside-corners.png",
            "parameters.png",
            "probing-hints.png",
            "results.png",
            "step-off-width.svg",
            "work-offsets.png",
        ] {
            assert!(
                probe_help_image_bytes(name).is_some(),
                "missing probe help figure {name}"
            );
        }
    }
}
