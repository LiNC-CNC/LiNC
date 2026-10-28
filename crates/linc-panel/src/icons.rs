//! Scalable vector pictograms used by the simulated SZGH panel.
//!
//! Coordinates are authored on a 64 × 32 canvas and fitted into the supplied
//! rectangle. Keeping the drawings here independent of the panel layout makes
//! them usable by other egui surfaces without textures or image assets.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

use crate::buttons;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    X,
    Y,
    Z,
    A,
    B,
    C,
}

impl Axis {
    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JogDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelIcon {
    Jog { axis: Axis, direction: JogDirection },
    SafeDoor,
    Rapid,
    MpgSim,
    SingleBlock,
    BlockDelete,
    MachineLock,
    OptionalStop,
    DryRun,
    Worklamp,
    AirBlow,
    ToolLength,
    Coolant,
    SpindleCts,
    Flush,
    Lubrication,
    Corridor,
    SpindleOrientation,
    MagazineClockwise,
    MagazineCounterClockwise,
    SpindleClockwise,
    SpindleStop,
    SpindleCounterClockwise,
    ChipClockwise,
    ChipStop,
    ChipCounterClockwise,
    Chuck,
    ProbeDown,
    ArmManual,
}

/// Select the pictogram printed on a physical panel key. Jog direction comes
/// from the key's arrow because the lathe face mounts its B-axis pair sideways.
pub fn for_key(mask: u64, arrow: Option<&str>) -> Option<PanelIcon> {
    let axis = match mask {
        buttons::JOG_XN | buttons::JOG_XP => Some(Axis::X),
        buttons::JOG_YN | buttons::JOG_YP => Some(Axis::Y),
        buttons::JOG_ZN | buttons::JOG_ZP => Some(Axis::Z),
        buttons::JOG_AN | buttons::JOG_AP => Some(Axis::A),
        buttons::JOG_BN | buttons::JOG_BP => Some(Axis::B),
        buttons::JOG_CN | buttons::JOG_CP => Some(Axis::C),
        _ => None,
    };
    if let Some(axis) = axis {
        let direction = match arrow {
            Some("<") => JogDirection::Left,
            Some(">") => JogDirection::Right,
            Some("^") => JogDirection::Up,
            Some("v") => JogDirection::Down,
            _ => return None,
        };
        return Some(PanelIcon::Jog { axis, direction });
    }

    match mask {
        buttons::SAFE_DOOR => Some(PanelIcon::SafeDoor),
        buttons::RAPID => Some(PanelIcon::Rapid),
        buttons::MPG_SIM => Some(PanelIcon::MpgSim),
        buttons::SBK => Some(PanelIcon::SingleBlock),
        buttons::BDT => Some(PanelIcon::BlockDelete),
        buttons::MLK => Some(PanelIcon::MachineLock),
        buttons::OPT => Some(PanelIcon::OptionalStop),
        buttons::DRN => Some(PanelIcon::DryRun),
        buttons::WORKLAMP => Some(PanelIcon::Worklamp),
        buttons::AIR_BLOW => Some(PanelIcon::AirBlow),
        buttons::PT_LA => Some(PanelIcon::ToolLength),
        buttons::COOLANT => Some(PanelIcon::Coolant),
        buttons::SPD_CTS => Some(PanelIcon::SpindleCts),
        buttons::FLUSH => Some(PanelIcon::Flush),
        buttons::LUB => Some(PanelIcon::Lubrication),
        buttons::CORRIDOR => Some(PanelIcon::Corridor),
        buttons::SPD_ORI => Some(PanelIcon::SpindleOrientation),
        buttons::MAG_CW => Some(PanelIcon::MagazineClockwise),
        buttons::MAG_CCW => Some(PanelIcon::MagazineCounterClockwise),
        buttons::SPD_CW | buttons::SPD2_CW => Some(PanelIcon::SpindleClockwise),
        buttons::SPD_SP | buttons::SPD2_SP => Some(PanelIcon::SpindleStop),
        buttons::SPD_CCW | buttons::SPD2_CCW => Some(PanelIcon::SpindleCounterClockwise),
        buttons::CHIP_CW => Some(PanelIcon::ChipClockwise),
        buttons::CHIP_SP => Some(PanelIcon::ChipStop),
        buttons::CHIP_CCW => Some(PanelIcon::ChipCounterClockwise),
        buttons::CHUCK => Some(PanelIcon::Chuck),
        buttons::POT_DN => Some(PanelIcon::ProbeDown),
        buttons::ARM_MAN => Some(PanelIcon::ArmManual),
        _ => None,
    }
}

pub fn paint(painter: &Painter, icon: PanelIcon, rect: Rect, color: Color32) {
    let p = IconPainter::new(painter, rect, color);
    match icon {
        PanelIcon::Jog { axis, direction } => p.jog(axis, direction),
        PanelIcon::SafeDoor => p.safe_door(),
        PanelIcon::Rapid => p.rapid(),
        PanelIcon::MpgSim => p.mpg_sim(),
        PanelIcon::SingleBlock => p.single_block(),
        PanelIcon::BlockDelete => p.block_delete(),
        PanelIcon::MachineLock => p.machine_lock(),
        PanelIcon::OptionalStop => p.optional_stop(),
        PanelIcon::DryRun => p.dry_run(),
        PanelIcon::Worklamp => p.worklamp(),
        PanelIcon::AirBlow => p.air_blow(),
        PanelIcon::ToolLength => p.tool_length(),
        PanelIcon::Coolant => p.coolant(),
        PanelIcon::SpindleCts => p.spindle_cts(),
        PanelIcon::Flush => p.flush(),
        PanelIcon::Lubrication => p.lubrication(),
        PanelIcon::Corridor => p.corridor(),
        PanelIcon::SpindleOrientation => p.spindle_orientation(),
        PanelIcon::MagazineClockwise => p.magazine(true),
        PanelIcon::MagazineCounterClockwise => p.magazine(false),
        PanelIcon::SpindleClockwise => p.spindle(SpindleIcon::Clockwise),
        PanelIcon::SpindleStop => p.spindle(SpindleIcon::Stop),
        PanelIcon::SpindleCounterClockwise => p.spindle(SpindleIcon::CounterClockwise),
        PanelIcon::ChipClockwise => p.chip(ChipIcon::Clockwise),
        PanelIcon::ChipStop => p.chip(ChipIcon::Stop),
        PanelIcon::ChipCounterClockwise => p.chip(ChipIcon::CounterClockwise),
        PanelIcon::Chuck => p.chuck(),
        PanelIcon::ProbeDown => p.probe_down(),
        PanelIcon::ArmManual => p.arm_manual(),
    }
}

struct IconPainter<'a> {
    painter: &'a Painter,
    origin: Pos2,
    scale: f32,
    color: Color32,
}

impl<'a> IconPainter<'a> {
    fn new(painter: &'a Painter, rect: Rect, color: Color32) -> Self {
        let scale = (rect.width() / 64.0).min(rect.height() / 32.0);
        let origin = rect.center() - Vec2::new(64.0 * scale, 32.0 * scale) / 2.0;
        Self {
            painter,
            origin,
            scale,
            color,
        }
    }

    fn point(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x * self.scale, y * self.scale)
    }

    fn stroke(&self, width: f32) -> Stroke {
        Stroke::new((width * self.scale).max(0.65), self.color)
    }

    fn line(&self, points: &[(f32, f32)], width: f32) {
        let points = points.iter().map(|&(x, y)| self.point(x, y)).collect();
        self.painter.add(Shape::line(points, self.stroke(width)));
    }

    fn closed(&self, points: &[(f32, f32)], width: f32) {
        let mut points = points.to_vec();
        if let Some(&first) = points.first() {
            points.push(first);
        }
        self.line(&points, width);
    }

    fn filled(&self, points: &[(f32, f32)]) {
        let points = points.iter().map(|&(x, y)| self.point(x, y)).collect();
        self.painter
            .add(Shape::convex_polygon(points, self.color, Stroke::NONE));
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32, width: f32) {
        self.painter.rect(
            Rect::from_min_max(self.point(x, y), self.point(x + w, y + h)),
            0.0,
            Color32::TRANSPARENT,
            self.stroke(width),
            egui::StrokeKind::Inside,
        );
    }

    fn circle(&self, x: f32, y: f32, radius: f32, width: f32) {
        self.painter
            .circle_stroke(self.point(x, y), radius * self.scale, self.stroke(width));
    }

    fn dot(&self, x: f32, y: f32, radius: f32) {
        self.painter
            .circle_filled(self.point(x, y), radius * self.scale, self.color);
    }

    fn text(&self, x: f32, y: f32, size: f32, text: &str) {
        self.painter.text(
            self.point(x, y),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(size * self.scale),
            self.color,
        );
    }

    fn arc(&self, x: f32, y: f32, radius: f32, start: f32, end: f32, width: f32) {
        let steps = (((end - start).abs() * radius * 0.65).ceil() as usize).max(8);
        let points: Vec<_> = (0..=steps)
            .map(|index| {
                let angle = start + (end - start) * index as f32 / steps as f32;
                self.point(x + radius * angle.cos(), y + radius * angle.sin())
            })
            .collect();
        self.painter.add(Shape::line(points, self.stroke(width)));
    }

    fn arrow_head(&self, tip: (f32, f32), direction: (f32, f32), size: f32) {
        let length = (direction.0 * direction.0 + direction.1 * direction.1).sqrt();
        let (dx, dy) = (direction.0 / length, direction.1 / length);
        let (px, py) = (-dy, dx);
        self.filled(&[
            tip,
            (
                tip.0 - dx * size + px * size * 0.55,
                tip.1 - dy * size + py * size * 0.55,
            ),
            (
                tip.0 - dx * size - px * size * 0.55,
                tip.1 - dy * size - py * size * 0.55,
            ),
        ]);
    }

    fn wave(&self, x: f32, y: f32, w: f32, amplitude: f32, cycles: f32, width: f32) {
        let points: Vec<_> = (0..=64)
            .map(|index| {
                let t = index as f32 / 64.0;
                (
                    x + w * t,
                    y + amplitude * (t * cycles * std::f32::consts::TAU).sin(),
                )
            })
            .collect();
        self.line(&points, width);
    }

    fn right_key_outline(&self) {
        self.line(
            &[(10.0, 27.0), (47.0, 27.0), (57.0, 16.0), (47.0, 5.0)],
            1.5,
        );
    }

    fn jog(&self, axis: Axis, direction: JogDirection) {
        match direction {
            JogDirection::Right => {
                self.closed(
                    &[
                        (5.0, 9.0),
                        (37.0, 9.0),
                        (37.0, 4.0),
                        (59.0, 16.0),
                        (37.0, 28.0),
                        (37.0, 23.0),
                        (5.0, 23.0),
                    ],
                    1.8,
                );
                self.text(24.0, 16.0, 14.0, axis.label());
            }
            JogDirection::Left => {
                self.closed(
                    &[
                        (59.0, 9.0),
                        (27.0, 9.0),
                        (27.0, 4.0),
                        (5.0, 16.0),
                        (27.0, 28.0),
                        (27.0, 23.0),
                        (59.0, 23.0),
                    ],
                    1.8,
                );
                self.text(40.0, 16.0, 14.0, axis.label());
            }
            JogDirection::Up => {
                self.closed(
                    &[
                        (18.0, 30.0),
                        (18.0, 15.0),
                        (10.0, 15.0),
                        (32.0, 2.0),
                        (54.0, 15.0),
                        (46.0, 15.0),
                        (46.0, 30.0),
                    ],
                    1.8,
                );
                self.text(32.0, 22.0, 14.0, axis.label());
            }
            JogDirection::Down => {
                self.closed(
                    &[
                        (18.0, 2.0),
                        (18.0, 17.0),
                        (10.0, 17.0),
                        (32.0, 30.0),
                        (54.0, 17.0),
                        (46.0, 17.0),
                        (46.0, 2.0),
                    ],
                    1.8,
                );
                self.text(32.0, 10.0, 14.0, axis.label());
            }
        }
    }

    fn safe_door(&self) {
        self.rect(12.0, 4.0, 40.0, 24.0, 1.5);
        self.line(&[(32.0, 4.0), (32.0, 28.0)], 1.2);
        self.rect(25.0, 12.0, 4.0, 8.0, 1.4);
        self.rect(35.0, 12.0, 4.0, 8.0, 1.4);
        self.line(&[(29.0, 8.0), (29.0, 3.0)], 1.2);
        self.arrow_head((29.0, 2.0), (0.0, -1.0), 3.0);
        self.line(&[(35.0, 8.0), (35.0, 3.0)], 1.2);
        self.arrow_head((35.0, 2.0), (0.0, -1.0), 3.0);
    }

    fn rapid(&self) {
        self.wave(7.0, 17.0, 50.0, 8.0, 2.5, 2.0);
        self.line(&[(20.0, 6.0), (20.0, 2.0)], 1.2);
        self.arrow_head((20.0, 2.0), (0.0, -1.0), 2.8);
        self.line(&[(44.0, 6.0), (44.0, 2.0)], 1.2);
        self.arrow_head((44.0, 2.0), (0.0, -1.0), 2.8);
    }

    fn mpg_sim(&self) {
        self.right_key_outline();
        self.circle(26.0, 15.0, 9.0, 1.5);
        self.circle(26.0, 15.0, 1.5, 1.2);
        for angle in [
            -std::f32::consts::FRAC_PI_2,
            std::f32::consts::PI / 6.0,
            5.0 * std::f32::consts::PI / 6.0,
        ] {
            self.line(
                &[
                    (26.0, 15.0),
                    (26.0 + 7.0 * angle.cos(), 15.0 + 7.0 * angle.sin()),
                ],
                1.3,
            );
        }
    }

    fn single_block(&self) {
        self.right_key_outline();
        self.rect(15.0, 7.0, 29.0, 17.0, 1.4);
        self.line(&[(20.0, 15.5), (37.0, 15.5)], 1.6);
        self.arrow_head((39.0, 15.5), (1.0, 0.0), 5.0);
    }

    fn block_delete(&self) {
        self.right_key_outline();
        self.rect(15.0, 6.0, 28.0, 19.0, 1.4);
        self.line(&[(19.0, 22.0), (38.0, 9.0)], 1.5);
        self.line(&[(11.0, 28.0), (49.0, 28.0)], 1.2);
    }

    fn machine_lock(&self) {
        self.right_key_outline();
        self.line(&[(12.0, 16.0), (48.0, 16.0)], 1.3);
        self.closed(
            &[
                (13.0, 11.0),
                (25.0, 11.0),
                (25.0, 7.0),
                (33.0, 16.0),
                (25.0, 25.0),
                (25.0, 21.0),
                (13.0, 21.0),
            ],
            1.4,
        );
        self.closed(
            &[
                (51.0, 11.0),
                (39.0, 11.0),
                (39.0, 7.0),
                (31.0, 16.0),
                (39.0, 25.0),
                (39.0, 21.0),
                (51.0, 21.0),
            ],
            1.4,
        );
    }

    fn optional_stop(&self) {
        self.right_key_outline();
        self.circle(27.0, 16.0, 9.0, 1.5);
        self.arc(27.0, 16.0, 12.0, -2.7, -0.45, 1.4);
        self.line(&[(19.0, 3.0), (19.0, 7.0)], 1.4);
        self.line(&[(35.0, 3.0), (35.0, 7.0)], 1.4);
        self.line(&[(13.0, 28.0), (49.0, 28.0)], 1.2);
    }

    fn dry_run(&self) {
        self.right_key_outline();
        self.wave(13.0, 17.0, 33.0, 6.0, 2.0, 1.7);
        self.circle(22.0, 4.0, 1.5, 1.1);
        self.circle(32.0, 4.0, 1.5, 1.1);
        self.circle(42.0, 4.0, 1.5, 1.1);
        self.line(&[(12.0, 28.0), (49.0, 28.0)], 1.2);
    }

    fn worklamp(&self) {
        self.arc(
            32.0,
            13.0,
            9.0,
            std::f32::consts::PI,
            std::f32::consts::TAU,
            1.6,
        );
        self.closed(
            &[(23.0, 13.0), (25.0, 22.0), (39.0, 22.0), (41.0, 13.0)],
            1.5,
        );
        self.line(&[(26.0, 25.0), (38.0, 25.0)], 1.5);
        for index in 0..7 {
            let angle = 0.15 + index as f32 * (std::f32::consts::PI - 0.3) / 6.0;
            let (dx, dy) = (angle.cos(), angle.sin());
            self.line(
                &[
                    (32.0 + 12.0 * dx, 13.0 + 12.0 * dy),
                    (32.0 + 16.0 * dx, 13.0 + 16.0 * dy),
                ],
                1.3,
            );
        }
        self.line(&[(27.0, 2.0), (25.0, 0.0)], 1.2);
        self.line(&[(37.0, 2.0), (39.0, 0.0)], 1.2);
    }

    fn air_blow(&self) {
        self.rect(10.0, 9.0, 25.0, 11.0, 1.5);
        self.line(
            &[(35.0, 11.0), (42.0, 11.0), (42.0, 18.0), (35.0, 18.0)],
            1.4,
        );
        self.line(&[(15.0, 6.0), (15.0, 9.0)], 1.2);
        self.line(&[(30.0, 6.0), (30.0, 9.0)], 1.2);
        self.line(&[(43.0, 12.0), (57.0, 7.0)], 1.5);
        self.line(&[(43.0, 14.5), (59.0, 14.5)], 1.5);
        self.line(&[(43.0, 17.0), (57.0, 22.0)], 1.5);
        self.line(&[(10.0, 25.0), (43.0, 25.0)], 1.2);
    }

    fn tool_length(&self) {
        self.line(&[(9.0, 26.0), (43.0, 26.0)], 1.5);
        self.closed(
            &[
                (11.0, 12.0),
                (28.0, 12.0),
                (28.0, 20.0),
                (38.0, 20.0),
                (38.0, 23.0),
                (23.0, 23.0),
                (23.0, 17.0),
                (11.0, 17.0),
            ],
            1.4,
        );
        self.line(&[(49.0, 3.0), (49.0, 22.0)], 1.6);
        self.arrow_head((49.0, 25.0), (0.0, 1.0), 5.0);
        self.line(&[(45.0, 3.0), (53.0, 3.0)], 1.3);
    }

    fn coolant(&self) {
        self.line(
            &[
                (16.0, 6.0),
                (46.0, 6.0),
                (46.0, 18.0),
                (37.0, 18.0),
                (37.0, 12.0),
                (23.0, 12.0),
                (23.0, 18.0),
                (15.0, 18.0),
            ],
            3.0,
        );
        self.line(&[(28.0, 6.0), (28.0, 2.0), (38.0, 2.0)], 1.6);
        self.line(&[(11.0, 18.0), (20.0, 18.0)], 2.2);
        self.line(&[(13.0, 21.0), (10.0, 25.0)], 1.3);
        self.line(&[(17.0, 21.0), (17.0, 26.0)], 1.3);
        self.line(&[(21.0, 21.0), (24.0, 25.0)], 1.3);
        self.dot(11.0, 29.0, 1.0);
        self.dot(22.0, 29.0, 1.0);
    }

    fn spindle_cts(&self) {
        self.rect(21.0, 10.0, 23.0, 11.0, 1.5);
        self.line(&[(27.0, 10.0), (27.0, 3.0), (38.0, 3.0), (38.0, 10.0)], 1.5);
        self.line(&[(17.0, 13.0), (21.0, 13.0)], 1.4);
        self.line(&[(44.0, 13.0), (48.0, 13.0)], 1.4);
        for x in [24.0, 29.0, 34.0, 39.0] {
            self.line(&[(x, 22.0), (x - 1.5, 27.0)], 1.3);
        }
        self.line(&[(17.0, 30.0), (48.0, 30.0)], 1.2);
    }

    fn flush(&self) {
        self.line(&[(10.0, 27.0), (54.0, 27.0)], 2.0);
        self.line(&[(17.0, 22.0), (47.0, 22.0), (47.0, 27.0)], 1.5);
        self.line(
            &[(44.0, 22.0), (44.0, 11.0), (51.0, 11.0), (51.0, 27.0)],
            1.5,
        );
        self.line(
            &[
                (18.0, 4.0),
                (34.0, 4.0),
                (34.0, 10.0),
                (26.0, 10.0),
                (26.0, 14.0),
            ],
            2.2,
        );
        self.line(&[(27.0, 14.0), (24.0, 19.0)], 1.2);
        self.line(&[(30.0, 13.0), (30.0, 19.0)], 1.2);
        self.line(&[(33.0, 12.0), (36.0, 18.0)], 1.2);
        self.line(&[(12.0, 30.0), (52.0, 30.0)], 1.2);
    }

    fn lubrication(&self) {
        self.closed(
            &[(27.0, 8.0), (45.0, 12.0), (40.0, 25.0), (22.0, 20.0)],
            1.6,
        );
        self.arc(39.0, 9.0, 8.0, -2.6, -0.3, 1.4);
        self.line(&[(25.0, 12.0), (15.0, 20.0), (8.0, 23.0)], 1.7);
        self.line(&[(8.0, 23.0), (6.0, 27.0)], 1.3);
        self.closed(
            &[(12.0, 25.0), (9.0, 29.0), (12.0, 31.0), (15.0, 29.0)],
            1.1,
        );
        self.line(&[(29.0, 8.0), (31.0, 4.0), (36.0, 5.0)], 1.4);
    }

    fn corridor(&self) {
        self.rect(10.0, 4.0, 41.0, 24.0, 1.5);
        for x in [18.0, 26.0, 34.0, 42.0] {
            self.line(&[(x, 5.0), (x, 27.0)], 1.1);
        }
        self.line(&[(11.0, 11.0), (50.0, 11.0)], 1.1);
        self.line(&[(11.0, 22.0), (50.0, 22.0)], 1.1);
        self.line(&[(19.0, 16.5), (43.0, 16.5)], 1.5);
        self.arrow_head((47.0, 16.5), (1.0, 0.0), 5.0);
    }

    fn spindle_orientation(&self) {
        self.arc(32.0, 17.0, 11.0, 0.15, std::f32::consts::TAU - 0.15, 1.6);
        self.line(&[(23.0, 8.0), (20.0, 3.0), (24.0, 1.0)], 1.5);
        self.line(&[(41.0, 8.0), (44.0, 3.0), (40.0, 1.0)], 1.5);
        self.line(&[(32.0, 6.0), (32.0, 11.0)], 1.5);
        self.dot(32.0, 17.0, 1.5);
    }

    fn magazine(&self, clockwise: bool) {
        let mut teeth = Vec::new();
        for index in 0..48 {
            let angle = index as f32 * std::f32::consts::TAU / 48.0;
            let radius = if index % 4 == 1 || index % 4 == 2 {
                14.0
            } else {
                11.5
            };
            teeth.push((32.0 + radius * angle.cos(), 16.0 + radius * angle.sin()));
        }
        self.closed(&teeth, 1.2);
        self.arc(32.0, 15.0, 7.0, 0.0, std::f32::consts::PI, 1.6);
        self.line(&[(25.0, 9.0), (25.0, 15.0)], 1.6);
        self.line(&[(39.0, 9.0), (39.0, 15.0)], 1.6);
        let (start, end, tip, direction) = if clockwise {
            (-2.6, -0.5, (38.0, 8.0), (0.7, 0.7))
        } else {
            (-0.5, -2.6, (26.0, 8.0), (-0.7, 0.7))
        };
        self.arc(32.0, 16.0, 9.0, start, end, 1.1);
        self.arrow_head(tip, direction, 3.0);
    }

    fn spindle(&self, kind: SpindleIcon) {
        self.rect(13.0, 8.0, 32.0, 16.0, 1.5);
        self.line(&[(45.0, 6.0), (49.0, 6.0), (49.0, 26.0), (45.0, 26.0)], 1.5);
        self.line(&[(51.0, 9.0), (51.0, 23.0)], 1.3);
        match kind {
            SpindleIcon::Clockwise => {
                self.line(&[(29.0, 23.0), (29.0, 8.0)], 1.6);
                self.arrow_head((29.0, 5.0), (0.0, -1.0), 5.0);
            }
            SpindleIcon::CounterClockwise => {
                self.line(&[(29.0, 8.0), (29.0, 23.0)], 1.6);
                self.arrow_head((29.0, 26.0), (0.0, 1.0), 5.0);
            }
            SpindleIcon::Stop => {
                self.circle(57.0, 16.0, 3.0, 1.4);
            }
        }
    }

    fn chip(&self, kind: ChipIcon) {
        self.wave(9.0, 16.0, 38.0, 7.0, 4.0, 1.5);
        self.arc(51.0, 16.0, 7.0, 0.65, std::f32::consts::TAU - 0.65, 1.5);
        match kind {
            ChipIcon::Clockwise => {
                self.line(&[(12.0, 3.0), (45.0, 3.0)], 1.3);
                self.arrow_head((49.0, 3.0), (1.0, 0.0), 4.0);
            }
            ChipIcon::CounterClockwise => {
                self.line(&[(19.0, 3.0), (52.0, 3.0)], 1.3);
                self.arrow_head((15.0, 3.0), (-1.0, 0.0), 4.0);
            }
            ChipIcon::Stop => {
                self.circle(59.0, 16.0, 2.3, 1.3);
            }
        }
    }

    fn chuck(&self) {
        self.rect(14.0, 9.0, 34.0, 14.0, 1.5);
        self.rect(10.0, 12.0, 5.0, 8.0, 1.4);
        self.line(&[(20.0, 9.0), (20.0, 4.0), (42.0, 4.0), (42.0, 9.0)], 1.4);
        self.line(&[(31.0, 4.0), (31.0, 0.5)], 1.4);
        self.line(&[(35.0, 10.0), (43.0, 22.0)], 1.2);
        self.line(&[(40.0, 10.0), (47.0, 20.0)], 1.2);
        self.line(&[(29.0, 23.0), (29.0, 30.0)], 1.5);
        self.line(&[(25.0, 30.0), (33.0, 30.0)], 1.3);
    }

    fn probe_down(&self) {
        self.rect(26.0, 2.0, 12.0, 7.0, 1.5);
        self.rect(23.0, 9.0, 18.0, 10.0, 1.5);
        self.line(
            &[
                (27.0, 19.0),
                (27.0, 24.0),
                (32.0, 29.0),
                (37.0, 24.0),
                (37.0, 19.0),
            ],
            1.5,
        );
        self.line(&[(32.0, 29.0), (32.0, 31.0)], 1.5);
        self.line(&[(19.0, 4.0), (23.0, 4.0)], 1.3);
        self.line(&[(41.0, 4.0), (45.0, 4.0)], 1.3);
    }

    fn arm_manual(&self) {
        self.line(&[(9.0, 27.0), (55.0, 27.0)], 1.6);
        self.circle(17.0, 23.0, 3.0, 1.4);
        self.circle(25.0, 12.0, 3.0, 1.4);
        self.circle(41.0, 17.0, 3.0, 1.4);
        self.line(
            &[(17.0, 23.0), (25.0, 12.0), (41.0, 17.0), (49.0, 10.0)],
            3.0,
        );
        self.line(&[(48.0, 7.0), (54.0, 7.0), (54.0, 13.0)], 1.5);
        self.line(&[(49.0, 10.0), (55.0, 16.0)], 1.4);
        self.rect(11.0, 26.0, 12.0, 3.0, 1.3);
    }
}

#[derive(Clone, Copy)]
enum SpindleIcon {
    Clockwise,
    Stop,
    CounterClockwise,
}

#[derive(Clone, Copy)]
enum ChipIcon {
    Clockwise,
    Stop,
    CounterClockwise,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pictogram_key_from_the_photo_has_a_drawing() {
        let masks = [
            buttons::JOG_XN,
            buttons::JOG_XP,
            buttons::JOG_YN,
            buttons::JOG_YP,
            buttons::JOG_ZN,
            buttons::JOG_ZP,
            buttons::JOG_AN,
            buttons::JOG_AP,
            buttons::JOG_BN,
            buttons::JOG_BP,
            buttons::SAFE_DOOR,
            buttons::RAPID,
            buttons::MPG_SIM,
            buttons::SBK,
            buttons::BDT,
            buttons::MLK,
            buttons::OPT,
            buttons::DRN,
            buttons::WORKLAMP,
            buttons::AIR_BLOW,
            buttons::PT_LA,
            buttons::COOLANT,
            buttons::SPD_CTS,
            buttons::FLUSH,
            buttons::LUB,
            buttons::CORRIDOR,
            buttons::SPD_ORI,
            buttons::MAG_CW,
            buttons::MAG_CCW,
            buttons::SPD_CW,
            buttons::SPD_SP,
            buttons::SPD_CCW,
            buttons::CHIP_CW,
            buttons::CHIP_SP,
            buttons::CHIP_CCW,
            buttons::CHUCK,
            buttons::POT_DN,
            buttons::ARM_MAN,
        ];
        for mask in masks {
            let arrow = match mask {
                buttons::JOG_XN
                | buttons::JOG_YN
                | buttons::JOG_ZN
                | buttons::JOG_AN
                | buttons::JOG_BN => Some("<"),
                buttons::JOG_XP
                | buttons::JOG_YP
                | buttons::JOG_ZP
                | buttons::JOG_AP
                | buttons::JOG_BP => Some(">"),
                _ => None,
            };
            assert!(
                for_key(mask, arrow).is_some(),
                "missing icon for mask {mask}"
            );
        }
    }

    #[test]
    fn programmable_keys_keep_text_only_legends() {
        for mask in [buttons::K1, buttons::K2, buttons::K3, buttons::K4] {
            assert_eq!(for_key(mask, None), None);
        }
    }

    #[test]
    fn jog_icon_uses_the_mounted_arrow_direction() {
        assert_eq!(
            for_key(buttons::JOG_BP, Some("<")),
            Some(PanelIcon::Jog {
                axis: Axis::B,
                direction: JogDirection::Left,
            })
        );
        assert_eq!(
            for_key(buttons::JOG_BP, Some("^")),
            Some(PanelIcon::Jog {
                axis: Axis::B,
                direction: JogDirection::Up,
            })
        );
    }
}
