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
}
