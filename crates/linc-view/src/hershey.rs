//! Stroke font for in-plane labels.
//!
//! Ported from AXIS's `lib/python/hershey.py` (Chris Radek, GPL-2.0-or-later):
//! each glyph is a set of polylines in a 440-unit box with y downwards, and text
//! is positioned by transforming each point into the plane it belongs to, so the
//! letters are projected by the same camera as the part instead of being a flat
//! label on top of the view.
//!
//! Only the characters the toolpath needs are included: digits, the minus sign,
//! the decimal point, and the axis letters.

use egui::{Color32, Pos2, Stroke};

use crate::Projection;

/// One glyph: a list of strokes, each a list of `(x, y)` points in a 440 box,
/// y downwards (the same convention as AXIS).
pub type Glyph = &'static [&'static [(f32, f32)]];

const BOX: f32 = 440.0;

/// Glyph data, indexed by character.
fn glyph(character: char) -> Option<Glyph> {
    Some(match character {
        '0' => &[
            &[
                (240.0, 20.0), (180.0, 40.0), (140.0, 100.0), (120.0, 200.0),
                (120.0, 260.0), (140.0, 360.0), (180.0, 420.0), (240.0, 440.0),
                (280.0, 440.0), (340.0, 420.0), (380.0, 360.0), (400.0, 260.0),
                (400.0, 200.0), (380.0, 100.0), (340.0, 40.0), (280.0, 20.0),
                (240.0, 20.0),
            ],
        ],
        '1' => &[&[(120.0, 100.0), (160.0, 80.0), (220.0, 20.0), (220.0, 440.0)]],
        '2' => &[
            &[
                (140.0, 120.0), (140.0, 100.0), (160.0, 60.0), (180.0, 40.0),
                (220.0, 20.0), (300.0, 20.0), (340.0, 40.0), (360.0, 60.0),
                (380.0, 100.0), (380.0, 140.0), (360.0, 180.0), (320.0, 240.0),
                (120.0, 440.0), (400.0, 440.0),
            ],
        ],
        '3' => &[
            &[
                (160.0, 20.0), (380.0, 20.0), (260.0, 180.0), (320.0, 180.0),
                (360.0, 200.0), (380.0, 220.0), (400.0, 280.0), (400.0, 320.0),
                (380.0, 380.0), (340.0, 420.0), (280.0, 440.0), (220.0, 440.0),
                (160.0, 420.0), (140.0, 400.0), (120.0, 360.0),
            ],
        ],
        '4' => &[
            &[(320.0, 20.0), (120.0, 300.0), (420.0, 300.0)],
            &[(320.0, 20.0), (320.0, 440.0)],
        ],
        '5' => &[
            &[
                (360.0, 20.0), (160.0, 20.0), (140.0, 200.0), (160.0, 180.0),
                (220.0, 160.0), (280.0, 160.0), (340.0, 180.0), (380.0, 220.0),
                (400.0, 280.0), (400.0, 320.0), (380.0, 380.0), (340.0, 420.0),
                (280.0, 440.0), (220.0, 440.0), (160.0, 420.0), (140.0, 400.0),
                (120.0, 360.0),
            ],
        ],
        '6' => &[
            &[
                (380.0, 80.0), (360.0, 40.0), (300.0, 20.0), (260.0, 20.0),
                (200.0, 40.0), (160.0, 100.0), (140.0, 200.0), (140.0, 300.0),
                (160.0, 380.0), (200.0, 420.0), (260.0, 440.0), (280.0, 440.0),
                (340.0, 420.0), (380.0, 380.0), (400.0, 320.0), (400.0, 300.0),
                (380.0, 240.0), (340.0, 200.0), (280.0, 180.0), (260.0, 180.0),
                (200.0, 200.0), (160.0, 240.0), (140.0, 300.0),
            ],
        ],
        '7' => &[
            &[(400.0, 20.0), (200.0, 440.0)],
            &[(120.0, 20.0), (400.0, 20.0)],
        ],
        '8' => &[
            &[
                (220.0, 20.0), (160.0, 40.0), (140.0, 80.0), (140.0, 120.0),
                (160.0, 160.0), (200.0, 180.0), (280.0, 200.0), (340.0, 220.0),
                (380.0, 260.0), (400.0, 300.0), (400.0, 360.0), (380.0, 400.0),
                (360.0, 420.0), (300.0, 440.0), (220.0, 440.0), (160.0, 420.0),
                (140.0, 400.0), (120.0, 360.0), (120.0, 300.0), (140.0, 260.0),
                (180.0, 220.0), (240.0, 200.0), (320.0, 180.0), (360.0, 160.0),
                (380.0, 120.0), (380.0, 80.0), (360.0, 40.0), (300.0, 20.0),
                (220.0, 20.0),
            ],
        ],
        '9' => &[
            &[
                (380.0, 160.0), (360.0, 220.0), (320.0, 260.0), (260.0, 280.0),
                (240.0, 280.0), (180.0, 260.0), (140.0, 220.0), (120.0, 160.0),
                (120.0, 140.0), (140.0, 80.0), (180.0, 40.0), (240.0, 20.0),
                (260.0, 20.0), (320.0, 40.0), (360.0, 80.0), (380.0, 160.0),
                (380.0, 260.0), (360.0, 360.0), (320.0, 420.0), (260.0, 440.0),
                (220.0, 440.0), (160.0, 420.0), (140.0, 380.0),
            ],
        ],
        '-' => &[&[(80.0, 260.0), (440.0, 260.0)]],
        '.' => &[&[
            (120.0, 400.0), (100.0, 420.0), (120.0, 440.0), (140.0, 420.0),
            (120.0, 400.0),
        ]],
        'X' => &[&[(60.0, 20.0), (340.0, 440.0)], &[(340.0, 20.0), (60.0, 440.0)]],
        'Y' => &[&[(40.0, 20.0), (200.0, 220.0), (200.0, 440.0)], &[(360.0, 20.0), (200.0, 220.0)]],
        'Z' => &[
            &[(340.0, 20.0), (60.0, 440.0)],
            &[(60.0, 20.0), (340.0, 20.0)],
            &[(60.0, 440.0), (340.0, 440.0)],
        ],
        _ => return None,
    })
}

/// Advance of one character in the 440 box, as AXIS lays it out.
fn advance(character: char) -> f32 {
    match character {
        '1' => 260.0,
        '.' => 180.0,
        _ => 400.0,
    }
}

/// Length of `text` in the 440 box.
pub(crate) fn string_len(text: &str) -> f32 {
    text.chars().map(advance).sum()
}

/// Draw `text` lying in the plane at `origin` with the given right and up axes.
///
/// `size` is the glyph height in model units. `project` maps a model point to
/// the screen, so the text is foreshortened by the camera exactly like the box
/// it labels.
pub(crate) fn plot_string(
    projection: &Projection,
    origin: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    size: f64,
    text: &str,
    color: Color32,
    painter: &egui::Painter,
) {
    let scale = size / BOX as f64;
    let mut pen = 0.0f64;
    for character in text.chars() {
        if let Some(strokes) = glyph(character) {
            for stroke in strokes {
                let points: Vec<Pos2> = stroke
                    .iter()
                    .map(|(x_box, y_box)| {
                        let x = pen + *x_box as f64 * scale;
                        // `y` runs downwards in the glyph box; the plane's up
                        // axis is positive.
                        let y = (BOX as f64 - *y_box as f64) * scale;
                        projection.project([
                            origin[0] + right[0] * x + up[0] * y,
                            origin[1] + right[1] * x + up[1] * y,
                            origin[2] + right[2] * x + up[2] * y,
                        ])
                    })
                    .collect();
                if points.len() >= 2 {
                    painter.add(egui::Shape::line(points, Stroke::new(1.2, color)));
                }
            }
        }
        pen += advance(character) as f64 * scale;
    }
}
