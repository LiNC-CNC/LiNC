//! Planned and measured cycle time: how long the moves a program has left will
//! take.
//!
//! LinuxCNC publishes no elapsed or remaining program time, and it publishes no
//! distance along the path either. What it does publish is the line being cut,
//! and the preview in [`crate::gcode`] has already walked every block of that
//! program with the modal state in force, so the HMI can say what the moves
//! cost: a move takes its length over the feed rate that block programmed, a
//! rapid takes the slowest axis's share of it, and a dwell takes what its `P`
//! word says.
//!
//! That plan is only as good as the model. Real motion is not a constant-speed
//! straight line: a 0.2 mm line at 3000 mm/min never reaches 3000 mm/min because
//! the planner has to ramp, a tool change takes however long it takes, and the
//! operator's feed override is not in the program at all. Those all show up as
//! the same measurement - the run took longer than the plan said - so
//! [`Learned`] records that ratio per move. A program run a second time predicts
//! itself, and a loop body that repeats inside one run predicts its own next
//! pass.
//!
//! Nothing here decides what to display; [`crate::model::ProgramState`] and the
//! UI own the clock. This module is the arithmetic.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gcode::{PathSegment, Preview, SegmentKind};
use crate::ini::MachineIni;

/// What a rapid is assumed to reach when the machine INI carries no velocity at
/// all, in machine units per second like every other velocity here. It only has
/// to be plausible - 500 mm/s is 30 m/min, a fast mill - because a machine
/// without `MAX_VELOCITY` is misconfigured and the measured calibration corrects
/// the guess.
const FALLBACK_RAPID: f64 = 500.0;

/// Seconds per minute: the plan counts seconds, an INI counts per second.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Seconds a move costs at 100 % override, split by what decides each part.
///
/// The split exists because the parts answer to different knobs: the rapid
/// override scales a rapid, the feed override scales a cut, and nothing scales a
/// dwell. Keeping them apart is what lets the operator turn a knob mid-run and
/// see the estimate follow, instead of waiting for the run to learn it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveSeconds {
    /// Time the machine's axis limits impose: a whole rapid, and the floor under
    /// a feed move the axes cannot run fast enough for.
    pub rapid: f64,
    /// Time the programmed feed rate imposes.
    pub feed: f64,
    /// Time a dwell holds for.
    pub hold: f64,
}

impl MoveSeconds {
    pub fn total(self) -> f64 {
        self.rapid + self.feed + self.hold
    }

    /// These seconds at the overrides the run is actually cutting at.
    pub fn scaled(self, overrides: Overrides) -> f64 {
        self.rapid / overrides.rapid + self.feed / overrides.feed + self.hold
    }
}

impl std::ops::Add for MoveSeconds {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            rapid: self.rapid + other.rapid,
            feed: self.feed + other.feed,
            hold: self.hold + other.hold,
        }
    }
}

impl std::ops::Sub for MoveSeconds {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            rapid: self.rapid - other.rapid,
            feed: self.feed - other.feed,
            hold: self.hold - other.hold,
        }
    }
}

impl std::ops::Mul<f64> for MoveSeconds {
    type Output = Self;

    fn mul(self, factor: f64) -> Self {
        Self {
            rapid: self.rapid * factor,
            feed: self.feed * factor,
            hold: self.hold * factor,
        }
    }
}

/// The overrides a run is cutting at, as factors: 1.0 is 100 %.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overrides {
    pub feed: f64,
    pub rapid: f64,
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            feed: 1.0,
            rapid: 1.0,
        }
    }
}

impl Overrides {
    /// Slowest override the estimate will divide by. At zero the machine is
    /// holding, not crawling, and an infinite estimate reads as a fault.
    const MIN: f64 = 0.05;
    /// Fastest. LinuxCNC's ceiling is around twice the programmed rate; anything
    /// wilder is a backend reporting something that is not an override.
    const MAX: f64 = 2.0;

    /// The factors the trajectory planner is applying: the panel's override
    /// values, or 100 % when `M48`/`M49` has them switched off.
    ///
    /// A zero is a real override - the operator holding the feed - so it is
    /// clamped to `MIN` rather than read as 100 %, or the estimate would promise
    /// a cut at a feed the machine is not moving at. A value that is not a number
    /// at all is what a backend that does not report overrides produces, and
    /// that reads as 100 %.
    pub fn of(feed: f64, rapid: f64, enabled: bool) -> Self {
        let factor = |value: f64| {
            if !enabled || !value.is_finite() || value < 0.0 {
                1.0
            } else {
                value.clamp(Self::MIN, Self::MAX)
            }
        };
        Self {
            feed: factor(feed),
            rapid: factor(rapid),
        }
    }

    pub fn from_feed_state(state: &crate::model::FeedState) -> Self {
        Self::of(
            state.feed_override,
            state.rapid_override,
            state.feed_override_enabled,
        )
    }
}

/// Planned seconds for every move of a program.
///
/// The order is [`Preview::segments`] order, which is the interpreter's order,
/// not the file's: a loop body appears once per pass, so a `repeat [100]` of one
/// line is a hundred planned moves. The seconds are at 100 % override; the
/// overrides are applied when the remaining time is read, so a knob turned
/// mid-run moves the estimate at once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Nominal seconds for each move, split by what decides them.
    parts: Vec<MoveSeconds>,
    /// `before[i]` is the planned seconds before move `i`, so the last entry is
    /// the whole program.
    before: Vec<MoveSeconds>,
}

impl Plan {
    /// The plan for a parsed program, at the machine's axis limits.
    pub fn from_preview(preview: &Preview, ini: &MachineIni) -> Self {
        let rates = AxisRates::from_ini(ini);
        let segments = &preview.segments;
        let mut parts: Vec<MoveSeconds> = Vec::with_capacity(segments.len());
        let mut index = 0;
        while index < segments.len() {
            // A block's words apply to every move it draws: a canned cycle is
            // one block and five moves, and a G93 block's time is the whole
            // line's, however it divides up.
            let line = segments[index].line;
            let timing = preview.line_timing.get(&line).copied().unwrap_or_default();
            let first = parts.len();
            while index < segments.len() && segments[index].line == line {
                parts.push(rates.move_secs(&segments[index], timing.feed));
                index += 1;
            }
            let run_total: f64 = parts[first..].iter().map(|part| part.total()).sum();
            if timing.fixed > 0.0 {
                if run_total > 1e-9 {
                    let scale = timing.fixed / run_total;
                    for part in &mut parts[first..] {
                        *part = *part * scale;
                    }
                } else if let Some(part) = parts.get_mut(first) {
                    // A G93 block that draws no length at all is still the time
                    // its F word names.
                    part.feed += timing.fixed;
                }
            }
            if timing.hold > 0.0 {
                if let Some(part) = parts.get_mut(first) {
                    part.hold += timing.hold;
                }
            }
        }

        let mut before = Vec::with_capacity(parts.len() + 1);
        let mut sum = MoveSeconds::default();
        before.push(sum);
        for part in &parts {
            sum = sum + *part;
            before.push(sum);
        }
        Self { parts, before }
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Planned seconds for move `index`, at 100 % override.
    pub fn secs(&self, index: usize) -> f64 {
        self.parts
            .get(index)
            .map(|part| part.total())
            .unwrap_or(0.0)
    }

    /// Planned seconds for the whole program, at 100 % override.
    pub fn total(&self) -> f64 {
        self.before.last().map(|part| part.total()).unwrap_or(0.0)
    }

    /// Planned seconds for the whole program, split by what decides them.
    pub fn parts_total(&self) -> MoveSeconds {
        self.before.last().copied().unwrap_or_default()
    }

    /// Planned seconds before `index`, plus `frac` of that move: where the cut
    /// has reached, in the plan's own time and at 100 % override.
    pub fn parts_at(&self, index: usize, frac: f64) -> MoveSeconds {
        let index = index.min(self.parts.len());
        let before = self
            .before
            .get(index)
            .copied()
            .unwrap_or_else(|| self.parts_total());
        before + self.parts.get(index).copied().unwrap_or_default() * frac.clamp(0.0, 1.0)
    }
}

/// The machine's per-axis speed limits, as `[AXIS_n] MAX_VELOCITY` writes them:
/// machine units per second.
struct AxisRates {
    axes: [f64; 3],
    fallback: f64,
}

impl AxisRates {
    fn from_ini(ini: &MachineIni) -> Self {
        let mut axes = [0.0; 3];
        for axis in &ini.axes {
            let slot = match axis.name.trim().to_ascii_uppercase().as_str() {
                "X" => 0,
                "Y" => 1,
                "Z" => 2,
                _ => continue,
            };
            if axis.max_velocity > 0.0 {
                axes[slot] = axis.max_velocity;
            }
        }
        let fallback = if ini.max_linear_velocity > 0.0 {
            ini.max_linear_velocity
        } else {
            FALLBACK_RAPID
        };
        Self { axes, fallback }
    }

    /// The shortest time the axes allow for the straight interpolation this move
    /// draws: the axis that has the furthest to go at its own limit decides.
    fn axis_secs(&self, segment: &PathSegment) -> f64 {
        let mut slowest = 0.0f64;
        for axis in 0..3 {
            let speed = if self.axes[axis] > 0.0 {
                self.axes[axis]
            } else {
                self.fallback
            };
            let time = (segment.end[axis] - segment.start[axis]).abs() / speed;
            slowest = slowest.max(time);
        }
        slowest
    }

    /// Planned seconds for one move at `feed`, in machine units per minute. A
    /// move with no programmed feed - a rapid, or one whose block never named an
    /// `F` - is timed at the axis limits, and a feed slower than the axes allow
    /// is timed at the axes' floor.
    fn move_secs(&self, segment: &PathSegment, feed: f64) -> MoveSeconds {
        let axis = self.axis_secs(segment);
        if segment.kind == SegmentKind::Rapid {
            return MoveSeconds {
                rapid: axis,
                ..MoveSeconds::default()
            };
        }
        if feed <= 1e-9 {
            return MoveSeconds {
                rapid: axis,
                ..MoveSeconds::default()
            };
        }
        MoveSeconds {
            feed: (segment.length() / feed * SECONDS_PER_MINUTE).max(axis),
            ..MoveSeconds::default()
        }
    }
}

/// One move of the drawn path, named so it can be found again.
///
/// Segment indices are not a stable name: a work offset change rebuilds the
/// path, and its first move is trimmed when the tool already stands where the
/// program starts. The line and the move's place among the moves that line
/// draws survive both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct MoveKey {
    pub line: usize,
    pub ordinal: usize,
}

/// The name of move `index` in the drawn path.
pub fn move_key(preview: &Preview, index: usize) -> Option<MoveKey> {
    let segment = preview.segments.get(index)?;
    let (from, _) = preview.line_bounds.get(&segment.line)?;
    Some(MoveKey {
        line: segment.line,
        ordinal: index.saturating_sub(*from),
    })
}

/// Where move `key` is in the drawn path, if this preview still draws it.
pub fn move_index(preview: &Preview, key: MoveKey) -> Option<usize> {
    let (from, to) = preview.line_bounds.get(&key.line)?;
    let index = from + key.ordinal;
    if index >= *to {
        return None;
    }
    let segment = preview.segments.get(index)?;
    (segment.line == key.line).then_some(index)
}

/// The first move of the block on `line`, as a position in the plan's order.
pub fn line_start(preview: &Preview, line: usize) -> Option<usize> {
    preview.line_bounds.get(&line).map(|(from, _)| *from)
}

/// The slowest ratio a measurement may record, and the fastest. A move that
/// stalls for a minute and a move that claims to have run in a millisecond are
/// both measurement noise, not calibration.
const MIN_RATIO: f64 = 0.02;
const MAX_RATIO: f64 = 100.0;

/// How much of each new measurement the stored one gives up. A re-run of the
/// same program repeats the same cornering losses, so the old figure is worth
/// keeping; a run at a different feed override must show up within a few moves.
const EMA: f64 = 0.5;

/// The most measurements held in memory, and the most written to the store.
/// Beyond this the moves are run-rate uniform enough that `recent` speaks for
/// them.
pub const MAX_MOVES: usize = 4096;

/// Measured seconds against planned seconds, per move.
///
/// A ratio of 1.0 is the plan being right; 1.4 is the move taking 40 % longer
/// than its length and feed imply, which is what a short move that never reaches
/// its feed looks like.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Learned {
    /// The measurements, keyed by move.
    #[serde(default)]
    moves: BTreeMap<MoveKey, f64>,
    /// The last ratio measured, which stands in for every move that has no
    /// measurement of its own.
    #[serde(default)]
    recent: Option<f64>,
}

impl Learned {
    /// The ratio recorded for this move, if any.
    pub fn ratio(&self, key: MoveKey) -> Option<f64> {
        self.moves.get(&key).copied()
    }

    /// The ratio to scale an unmeasured move by: the last one measured, or the
    /// plan itself before anything has been measured.
    pub fn recent(&self) -> f64 {
        self.recent.unwrap_or(1.0)
    }

    pub fn len(&self) -> usize {
        self.moves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.recent.is_none()
    }

    /// Record that a move took `ratio` times its planned time.
    pub fn observe(&mut self, key: MoveKey, ratio: f64) {
        if !ratio.is_finite() {
            return;
        }
        let ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
        let room = self.moves.len() < MAX_MOVES;
        match self.moves.get_mut(&key) {
            Some(stored) => *stored = *stored * (1.0 - EMA) + ratio * EMA,
            None if room => {
                self.moves.insert(key, ratio);
            }
            None => {}
        }
        self.recent = Some(match self.recent {
            Some(previous) => previous * (1.0 - EMA) + ratio * EMA,
            None => ratio,
        });
    }

    /// Forget everything: a different program, or a measurement that turned out
    /// to be of something else.
    pub fn clear(&mut self) {
        self.moves.clear();
        self.recent = None;
    }

    /// What the measurements change about the moves still to run, in seconds.
    ///
    /// The estimate starts from `recent` applied to everything left, which is
    /// all it can say about a move it has never seen. A move it has measured
    /// stands at its own ratio instead, and this is the difference between the
    /// two, at the overrides the run is cutting at.
    pub fn tail_correction(
        &self,
        preview: &Preview,
        plan: &Plan,
        from: usize,
        overrides: Overrides,
    ) -> f64 {
        let recent = self.recent();
        let mut correction = 0.0;
        for (key, ratio) in &self.moves {
            let Some(index) = move_index(preview, *key) else {
                continue;
            };
            if index < from {
                continue;
            }
            correction += (ratio - recent) * part_secs(plan, index, overrides);
        }
        correction
    }
}

/// Planned seconds for move `index` at the overrides the run is cutting at.
///
/// A measured move's ratio says how far the machine's own motion beats the plan;
/// it must not absorb the override as well, or the estimate would double-count
/// a knob the operator can turn at any moment.
fn part_secs(plan: &Plan, index: usize, overrides: Overrides) -> f64 {
    plan.parts
        .get(index)
        .map(|part| part.scaled(overrides))
        .unwrap_or(0.0)
}

/// The calibration file's identity: the program text it was measured on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramKey(u64);

impl ProgramKey {
    /// Hash a program. FNV-1a, so the number a file was written with stays the
    /// same across builds and toolchains: it is a name, not a security
    /// boundary.
    pub fn of(text: &str) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(hash)
    }

    fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// The cycle-time calibration kept beside the preferences file.
///
/// One program at a time: the file carries the hash of the program it was
/// measured on, and a load only answers for that hash. A second program
/// overwrites the file rather than growing it, and an edited program measures
/// itself again - instructions that are no longer there cannot inherit the
/// times of the ones that were.
#[derive(Clone, Debug, Default)]
pub struct CycleStore {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCalibration {
    /// The program this was measured on, as hex so it survives TOML's signed
    /// integers.
    program: String,
    #[serde(default)]
    recent: Option<f64>,
    #[serde(default)]
    moves: Vec<StoredMove>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredMove {
    line: usize,
    ordinal: usize,
    ratio: f64,
}

impl CycleStore {
    pub fn load(path: PathBuf) -> Self {
        Self { path }
    }

    /// The measurements for this program, or none when the file was written for
    /// another one. An unreadable or malformed file means no calibration, not an
    /// error: the estimate is a convenience, never a machine state.
    pub fn read(&self, key: ProgramKey) -> Option<Learned> {
        let text = fs::read_to_string(&self.path).ok()?;
        let stored: StoredCalibration = toml::from_str(&text).ok()?;
        if stored.program != key.hex() {
            return None;
        }
        let mut learned = Learned {
            moves: BTreeMap::new(),
            recent: stored.recent,
        };
        for entry in stored.moves {
            if learned.moves.len() >= MAX_MOVES {
                break;
            }
            learned.moves.insert(
                MoveKey {
                    line: entry.line,
                    ordinal: entry.ordinal,
                },
                entry.ratio.clamp(MIN_RATIO, MAX_RATIO),
            );
        }
        Some(learned)
    }

    /// Write the measurements for this program. A write failure is logged, not
    /// returned: a missing calibration only makes the estimate rougher.
    pub fn write(&self, key: ProgramKey, learned: &Learned) {
        if let Err(error) = self.write_inner(key, learned) {
            log::warn!(
                "cannot write cycle-time calibration {}: {error}",
                self.path.display()
            );
        }
    }

    fn write_inner(&self, key: ProgramKey, learned: &Learned) -> std::io::Result<()> {
        let Some(parent) = self.path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cycle-time path has no parent directory",
            ));
        };
        // The moves the plan gets most wrong are the ones worth keeping: a store
        // capped at MAX_MOVES keeps every move the plan is 5 % off on rather
        // than the first few thousand lines of the program.
        let mut entries: Vec<(&MoveKey, f64)> =
            learned.moves.iter().map(|(k, v)| (k, *v)).collect();
        if entries.len() > MAX_MOVES {
            entries.sort_by(|a, b| {
                (b.1 - 1.0)
                    .abs()
                    .partial_cmp(&(a.1 - 1.0).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            entries.truncate(MAX_MOVES);
            entries.sort_by_key(|(key, _)| **key);
        }
        let mut body = String::from(
            "# LiNC cycle-time calibration: measured seconds against planned seconds,\n\
             # per move. Written by the HMI; safe to delete, and re-measured by the\n\
             # next run of the program.\n",
        );
        body.push_str(&format!("program = \"{}\"\n", key.hex()));
        if let Some(recent) = learned.recent {
            body.push_str(&format!("recent = {recent}\n"));
        }
        for (move_, ratio) in entries {
            body.push_str(&format!(
                "\n[[moves]]\nline = {}\nordinal = {}\nratio = {ratio}\n",
                move_.line, move_.ordinal
            ));
        }

        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(body.as_bytes())?;
        temp.flush()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// Where the cycle-time calibration lives: beside the preferences file, like
/// the machine state. `sim.preferences.toml` becomes `sim.cycletime.toml`.
pub fn store_path(preferences_path: &Path) -> PathBuf {
    let parent = preferences_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = preferences_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("linc");
    let stem = stem.strip_suffix(".preferences").unwrap_or(stem);
    parent.join(format!("{stem}.cycletime.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axes() -> Vec<String> {
        vec!["X".into(), "Y".into(), "Z".into()]
    }

    fn parse(text: &str) -> Preview {
        crate::gcode::parse_preview(text, &axes(), None, [0.0; 3])
    }

    /// A machine whose axes each run at 60 machine units per second, so one
    /// unit is one hundredth of a minute and the arithmetic stays readable.
    fn machine() -> MachineIni {
        let mut ini = MachineIni::default();
        ini.max_linear_velocity = 60.0;
        ini.axes = ["X", "Y", "Z"]
            .into_iter()
            .map(|name| crate::ini::AxisIni {
                name: name.into(),
                max_velocity: 60.0,
                ..Default::default()
            })
            .collect();
        ini
    }

    fn plan(text: &str) -> Plan {
        Plan::from_preview(&parse(text), &machine())
    }

    #[test]
    fn a_feed_move_takes_its_length_over_the_feed() {
        // 60 mm at 600 mm/min is a tenth of a minute: 6 seconds.
        let plan = plan("G21 G90\nG1 X60 F600\nM2\n");
        assert_eq!(plan.len(), 1);
        assert!((plan.total() - 6.0).abs() < 1e-9, "{plan:?}");
    }

    #[test]
    fn a_rapid_takes_the_axes_limit_not_a_feed() {
        // 60 mm at 60 mm/s is one second, whatever feed the program mentions.
        assert!((plan("G21 G90\nG0 X60 F1\nM2\n").total() - 1.0).abs() < 1e-9);
        // A diagonal is the slowest axis's share, not the diagonal's length.
        assert!((plan("G21 G90\nG0 X60 Y60\nM2\n").total() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn an_arc_costs_its_sweep_not_its_chord() {
        // A half circle of radius 30 is 94.25 mm long; at 600 mm/min that is
        // more than the 60 mm chord would be.
        let plan = plan("G21 G90 G17\nG0 X30 Y0\nG3 X-30 Y0 I-30 J0 F600\nM2\n");
        let arc = plan.secs(plan.len() - 1);
        let expected = std::f64::consts::PI * 30.0 / 600.0 * 60.0;
        assert!((arc - expected).abs() < 0.05, "{arc} vs {expected}");
    }

    #[test]
    fn a_dwell_is_time_with_no_move() {
        let plan = plan("G21 G90\nG4 P5\nG1 X60 F600\nM2\n");
        // The hold rides on a zero-length move, so it keeps its place in the
        // plan's order; it adds 5 s and no distance.
        assert_eq!(plan.len(), 2, "the blank dwell move and the cut");
        assert!((plan.secs(0) - 5.0).abs() < 1e-9, "{plan:?}");
        assert!((plan.total() - 11.0).abs() < 1e-9, "5 s hold plus 6 s cut");
    }

    #[test]
    fn a_dwell_inside_a_loop_is_planned_every_pass() {
        let text = "G21 G90\no100 repeat [3]\nG4 P2\nG91\nG1 X10 F600\no100 endrepeat\nM2\n";
        let plan = plan(text);
        // Three passes of 2 s hold plus 1 s cut.
        assert!((plan.total() - 9.0).abs() < 1e-6, "{plan:?}");
    }

    #[test]
    fn a_g93_block_takes_the_time_its_f_names() {
        // F2 under G93 is half a minute for the block, whatever its length.
        let plan = plan("G21 G90\nG93\nG1 X60 F2\nM2\n");
        assert!((plan.total() - 30.0).abs() < 1e-9, "{plan:?}");
    }

    #[test]
    fn a_g95_block_takes_the_feed_per_revolution_at_the_spindle_speed() {
        // F0.5 mm/rev at S600 is 300 mm/min, so 60 mm takes 12 seconds.
        let plan = plan("G21 G90\nS600\nG95\nG1 X60 F0.5\nM2\n");
        assert!((plan.total() - 12.0).abs() < 1e-6, "{plan:?}");
    }

    #[test]
    fn the_overrides_scale_the_moves_they_govern() {
        // A 60 mm rapid and a 60 mm cut at 600 mm/min: 1 s and 6 s at 100 %.
        let preview = parse("G21 G90\nG0 X60\nG1 X120 F600\nM2\n");
        let plan = Plan::from_preview(&preview, &machine());
        let total = plan.parts_total();
        assert!((total.rapid - 1.0).abs() < 1e-9, "{total:?}");
        assert!((total.feed - 6.0).abs() < 1e-9, "{total:?}");
        assert!((total.scaled(Overrides::default()) - 7.0).abs() < 1e-9);
        // Half feed and quarter rapid: 4 s of rapid, 12 s of cut.
        let overrides = Overrides {
            feed: 0.5,
            rapid: 0.25,
        };
        assert!((total.scaled(overrides) - 16.0).abs() < 1e-9);
    }

    #[test]
    fn a_dwell_is_not_scaled_by_an_override() {
        let plan = plan("G21 G90\nG4 P5\nG1 X60 F600\nM2\n");
        let overrides = Overrides {
            feed: 0.5,
            rapid: 0.5,
        };
        // The 6 s cut takes 12 s at half feed; the 5 s hold is 5 s either way.
        assert!((plan.parts_total().scaled(overrides) - 17.0).abs() < 1e-9);
    }

    #[test]
    fn a_switched_off_override_reads_as_the_programmed_rate() {
        // M49: the planner applies neither knob, whatever the dials say.
        assert_eq!(Overrides::of(0.4, 0.2, false), Overrides::default());
        assert_eq!(Overrides::of(f64::NAN, -1.0, true), Overrides::default());
        // A live override is taken as it reads, up to the machine's ceiling and
        // never down to a zero the estimate would divide by.
        assert_eq!(Overrides::of(0.5, 0.25, true).feed, 0.5);
        assert_eq!(Overrides::of(0.0, 0.0, true).feed, Overrides::MIN);
        assert_eq!(Overrides::of(9.0, 9.0, true).rapid, Overrides::MAX);
    }

    #[test]
    fn a_loop_pass_is_planned_every_time_it_runs() {
        // Incremental, so every pass cuts: four 10 mm lines at 600 mm/min.
        let text = "G21 G90\no100 repeat [4]\nG91\nG1 X10 F600\no100 endrepeat\nM2\n";
        let plan = plan(text);
        assert_eq!(plan.len(), 4, "four passes, four moves");
        assert!((plan.total() - 4.0).abs() < 1e-6, "{plan:?}");
    }

    #[test]
    fn a_move_is_named_by_its_line_and_not_by_its_index() {
        let preview = parse("G21 G90\nG1 X60 F600\nM2\n");
        let key = move_key(&preview, 0).expect("a move");
        assert_eq!(key.line, 2);
        assert_eq!(key.ordinal, 0);
        assert_eq!(move_index(&preview, key), Some(0));
    }

    #[test]
    fn a_measured_move_scales_the_estimate_it_measures() {
        let mut learned = Learned::default();
        learned.observe(
            MoveKey {
                line: 2,
                ordinal: 0,
            },
            2.0,
        );
        assert_eq!(
            learned.ratio(MoveKey {
                line: 2,
                ordinal: 0
            }),
            Some(2.0)
        );
        // The first measurement is the recent ratio too: it stands in for the
        // moves that have none.
        assert_eq!(learned.recent(), 2.0);
        // A second measurement moves both halfway.
        learned.observe(
            MoveKey {
                line: 2,
                ordinal: 0,
            },
            4.0,
        );
        assert_eq!(
            learned.ratio(MoveKey {
                line: 2,
                ordinal: 0
            }),
            Some(3.0)
        );
        assert_eq!(learned.recent(), 3.0);
    }

    #[test]
    fn a_wild_measurement_is_clamped_rather_than_trusted() {
        let mut learned = Learned::default();
        learned.observe(
            MoveKey {
                line: 1,
                ordinal: 0,
            },
            1e9,
        );
        assert_eq!(learned.recent(), MAX_RATIO);
        learned.observe(
            MoveKey {
                line: 1,
                ordinal: 1,
            },
            0.0,
        );
        assert_eq!(
            learned.ratio(MoveKey {
                line: 1,
                ordinal: 1
            }),
            Some(MIN_RATIO)
        );
        assert!((MIN_RATIO..=MAX_RATIO).contains(&learned.recent()));
    }

    #[test]
    fn the_calibration_belongs_to_the_program_it_was_measured_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CycleStore::load(dir.path().join("sim.cycletime.toml"));
        let program = ProgramKey::of("G1 X60 F600\n");
        let mut learned = Learned::default();
        learned.observe(
            MoveKey {
                line: 2,
                ordinal: 1,
            },
            1.5,
        );

        assert_eq!(store.read(program), None, "no file yet");
        store.write(program, &learned);
        assert_eq!(store.read(program), Some(learned.clone()));
        assert_eq!(
            store.read(ProgramKey::of("G1 X60 F700\n")),
            None,
            "an edited program measures itself again"
        );
    }

    #[test]
    fn an_unreadable_calibration_is_no_calibration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sim.cycletime.toml");
        fs::write(&path, "program = ").unwrap();
        let store = CycleStore::load(path);
        assert_eq!(store.read(ProgramKey::of("G1 X0\n")), None);
    }

    #[test]
    fn the_calibration_sits_beside_the_preferences_file() {
        assert_eq!(
            store_path(Path::new(
                "/home/operator/.config/linc/sim.preferences.toml"
            )),
            Path::new("/home/operator/.config/linc/sim.cycletime.toml")
        );
    }

    #[test]
    fn a_measured_move_overrides_the_recent_ratio_for_the_moves_left() {
        let preview = parse("G21 G90\nG1 X60 F600\nG1 X120 F600\nM2\n");
        let plan = Plan::from_preview(&preview, &machine());
        let mut learned = Learned::default();
        // Both moves planned at 6 s; the first actually took 12 s and the last
        // measurement (recent) is 2.0, which is what the unmeasured tail uses.
        let first = move_key(&preview, 0).expect("a move");
        learned.observe(first, 2.0);
        assert_eq!(learned.recent(), 2.0);
        // Measured and recent agree, so the measurements change nothing yet.
        assert!(
            learned
                .tail_correction(&preview, &plan, 0, Overrides::default())
                .abs()
                < 1e-9
        );
        // A second move measured at half speed makes the correction negative:
        // that move stands at 0.5 while `recent` is near 1.25.
        let second = move_key(&preview, 1).expect("a move");
        learned.observe(second, 0.5);
        let recent = learned.recent();
        let expected = (2.0 - recent) * plan.secs(0) + (0.5 - recent) * plan.secs(1);
        assert!(
            (learned.tail_correction(&preview, &plan, 0, Overrides::default()) - expected).abs()
                < 1e-9
        );
        // From the second move on, only its own measurement counts.
        let expected = (0.5 - recent) * plan.secs(1);
        assert!(
            (learned.tail_correction(&preview, &plan, 1, Overrides::default()) - expected).abs()
                < 1e-9
        );
    }
}
