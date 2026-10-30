//! LinuxCNC G-code lexer and bounded preview parser.
//!
//! The preview parser is intentionally small: it understands the words needed
//! to draw a toolpath and mark obvious syntax errors. It never imports remaps
//! and never talks to the machine interpreter. It does follow the common o-word
//! subset (sub/call, if/elseif/else/endif, while, repeat, goto, parameter
//! assignment and expressions) under a hard step and call-depth limit, and it
//! says so on the result when it had to stop short.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::model::AxisMap;
use crate::model::SyntaxIssue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    GCode,
    MCode,
    Axis,
    Number,
    FeedSpeed,
    Tool,
    Parameter,
    Variable,
    Comment,
    ProgramNumber,
    LineNumber,
    OWord,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    /// Source text, without transformation.
    pub text: String,
}

impl Token {
    /// G/M/axis words are displayed uppercase; comments are not transformed.
    pub fn display(&self) -> String {
        match self.kind {
            TokenKind::Comment => self.text.clone(),
            TokenKind::GCode
            | TokenKind::MCode
            | TokenKind::Axis
            | TokenKind::FeedSpeed
            | TokenKind::Tool
            | TokenKind::Parameter
            | TokenKind::OWord
            | TokenKind::ProgramNumber
            | TokenKind::LineNumber => self.text.to_ascii_uppercase(),
            _ => self.text.clone(),
        }
    }
}

/// Skip the blanks between a word letter and its value. LinuxCNC accepts
/// `G0 X 16.339` and `G1 X	0` as one word.
fn skip_spaces(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && (bytes[index] as char).is_whitespace() {
        index += 1;
    }
    index
}

/// Lex one line. Offsets are byte offsets within the line.
pub fn lex_line(line: &str) -> Vec<Token> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        // Line number at the start: N123.
        if (i == 0 || line[..i].trim().is_empty())
            && (ch == 'N' || ch == 'n')
            && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < bytes.len() && ((bytes[i] as char).is_ascii_digit()) {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::LineNumber,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        if ch == '(' {
            let start = i;
            let mut depth = 1;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'(' {
                    depth += 1;
                } else if bytes[i] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        if ch == ';' {
            tokens.push(Token {
                kind: TokenKind::Comment,
                start: i,
                end: bytes.len(),
                text: line[i..].to_string(),
            });
            break;
        }
        if ch == '%' {
            tokens.push(Token {
                kind: TokenKind::ProgramNumber,
                start: i,
                end: i + 1,
                text: line[i..i + 1].to_string(),
            });
            i += 1;
            continue;
        }
        if ch == '#' {
            let start = i;
            i += 1;
            if bytes.get(i) == Some(&b'<') {
                while i < bytes.len() && bytes[i] != b'>' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                while i < bytes.len() && ((bytes[i] as char).is_ascii_digit()) {
                    i += 1;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Variable,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        if ch == '[' || ch == ']' {
            tokens.push(Token {
                kind: TokenKind::Variable,
                start: i,
                end: i + 1,
                text: line[i..i + 1].to_string(),
            });
            i += 1;
            continue;
        }
        if ch == '_' {
            // Named parameter used in expressions, e.g. #<_value>.
            let start = i;
            i += 1;
            while i < bytes.len()
                && (((bytes[i] as char).is_ascii_alphanumeric()) || bytes[i] == b'_')
            {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Variable,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        if ch.is_ascii_alphabetic() {
            let letter = ch.to_ascii_lowercase();
            let start = i;
            i += 1;
            match letter {
                'o' if i < bytes.len() && (bytes[i] as char).is_ascii_digit() => {
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::OWord,
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
                'g' | 'm' => {
                    while i < bytes.len()
                        && ((bytes[i] as char).is_ascii_digit() || bytes[i] == b'.')
                    {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: if letter == 'g' {
                            TokenKind::GCode
                        } else {
                            TokenKind::MCode
                        },
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
                'x' | 'y' | 'z' | 'a' | 'b' | 'c' | 'u' | 'v' | 'w' | 'i' | 'j' | 'k' | 'r'
                | 'p' | 'q' | 'l' | 'd' | 'h' | 'e' => {
                    let value = skip_spaces(bytes, i);
                    if let Some(first) = bytes.get(value) {
                        if first.is_ascii_digit() || matches!(*first, b'.' | b'-' | b'+') {
                            i = value;
                        }
                    }
                    while i < bytes.len()
                        && ((bytes[i] as char).is_ascii_digit()
                            || bytes[i] == b'.'
                            || bytes[i] == b'-'
                            || bytes[i] == b'+')
                    {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: if matches!(
                            letter,
                            'i' | 'j' | 'k' | 'r' | 'p' | 'q' | 'l' | 'd' | 'h' | 'e'
                        ) {
                            TokenKind::Parameter
                        } else {
                            TokenKind::Axis
                        },
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
                'f' | 's' => {
                    let value = skip_spaces(bytes, i);
                    if let Some(first) = bytes.get(value) {
                        if first.is_ascii_digit() || matches!(*first, b'.' | b'-' | b'+') {
                            i = value;
                        }
                    }
                    while i < bytes.len()
                        && ((bytes[i] as char).is_ascii_digit()
                            || bytes[i] == b'.'
                            || bytes[i] == b'-'
                            || bytes[i] == b'+')
                    {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::FeedSpeed,
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
                't' if i < bytes.len() && ((bytes[i] as char).is_ascii_digit()) => {
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::Tool,
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
                _ => {
                    while i < bytes.len() && ((bytes[i] as char).is_ascii_alphanumeric()) {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::Other,
                        start,
                        end: i,
                        text: line[start..i].to_string(),
                    });
                }
            }
            continue;
        }
        if ch.is_ascii_digit() || ch == '-' || ch == '+' || ch == '.' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'-'
                    || bytes[i] == b'+'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E')
            {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        tokens.push(Token {
            kind: TokenKind::Other,
            start: i,
            end: i + 1,
            text: line[i..i + 1].to_string(),
        });
        i += 1;
    }
    tokens
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    Xy,
    Xz,
    Yz,
    Uv,
    Uw,
    Vw,
}

impl Plane {
    pub fn label(self) -> &'static str {
        match self {
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::Yz => "YZ",
            Self::Uv => "UV",
            Self::Uw => "UW",
            Self::Vw => "VW",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Rapid,
    Feed,
    Arc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArcSpec {
    pub center: [f64; 3],
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
    pub clockwise: bool,
    pub plane: Plane,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathSegment {
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub kind: SegmentKind,
    pub line: usize,
    pub arc: Option<ArcSpec>,
    pub tool: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds3 {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds3 {
    pub fn from_points(points: impl IntoIterator<Item = [f64; 3]>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut bounds = Self {
            min: first,
            max: first,
        };
        for point in iter {
            for axis in 0..3 {
                bounds.min[axis] = bounds.min[axis].min(point[axis]);
                bounds.max[axis] = bounds.max[axis].max(point[axis]);
            }
        }
        Some(bounds)
    }

    pub fn include(&mut self, point: [f64; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(point[axis]);
            self.max[axis] = self.max[axis].max(point[axis]);
        }
    }

    pub fn center(&self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    pub fn size(&self) -> [f64; 3] {
        [
            (self.max[0] - self.min[0]).max(1e-6),
            (self.max[1] - self.min[1]).max(1e-6),
            (self.max[2] - self.min[2]).max(1e-6),
        ]
    }

    pub fn expanded(&self, amount: f64) -> Self {
        Self {
            min: [
                self.min[0] - amount,
                self.min[1] - amount,
                self.min[2] - amount,
            ],
            max: [
                self.max[0] + amount,
                self.max[1] + amount,
                self.max[2] + amount,
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ModalSummary {
    pub gcodes: Vec<String>,
    pub mcodes: Vec<String>,
    pub feed: Option<f64>,
    pub speed: Option<f64>,
    pub tool: Option<u16>,
    pub wcs: String,
    pub units: String,
    pub plane: String,
    pub distance_mode: String,
}

impl ModalSummary {
    pub fn lines(&self) -> Vec<(String, String)> {
        let mut lines = Vec::new();
        for gcode in &self.gcodes {
            lines.push((gcode.to_ascii_uppercase(), String::new()));
        }
        for mcode in &self.mcodes {
            lines.push((mcode.to_ascii_uppercase(), String::new()));
        }
        lines
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub segments: Vec<PathSegment>,
    pub bounds: Option<Bounds3>,
    pub line_count: usize,
    pub line_bounds: BTreeMap<usize, (usize, usize)>,
    pub error: Option<SyntaxIssue>,
    pub modal: ModalSummary,
    pub tools: Vec<u16>,
    pub program_number: Option<String>,
    pub uses_live_tool: bool,
    pub arcs: usize,
    pub rapids: usize,
    /// The drawn path is not the whole program: a construct was skipped, a
    /// limit stopped the run, or a block could not be evaluated. The UI shows a
    /// partial path only when it knows this is set.
    pub incomplete: bool,
    /// Why `incomplete` is set, for the operator and for tests.
    pub incomplete_reason: Option<String>,
    /// Subroutines the preview followed into another file, so the UI can name
    /// the routine the controller reports a line in.
    pub subroutines: Vec<SubroutineBody>,
    /// Every loop the program runs, with the parameters its test reads and the
    /// value each had when an iteration was entered.
    pub loops: Vec<LoopTrace>,
    /// For each segment, the loop pass that produced it: the loop's index into
    /// [Preview::loops] + 1 in the high half, the pass in the low half, and 0
    /// for a block outside every loop. The controller reports only lines, and a
    /// loop body repeats them, so this is what tells a running line's pass
    /// apart from the same line in another pass.
    pub segment_loop: Vec<u32>,
}

/// A loop the program runs, as the preview saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopKind {
    /// `o10 while [...]`: the test runs before each pass.
    While,
    /// `o10 do ... o10 while [...]`: the test runs after each pass.
    DoWhile,
    /// `o10 repeat [n]`: a pass counter, with no parameter of its own.
    Repeat,
}

/// A loop, the parameters its test reads, and the value each had at the moment
/// an iteration was entered.
///
/// The controller publishes no parameter values while a program runs (the
/// interpreter writes them to the var file only on reset), so these come from
/// the preview's own run of the same statements. That is exact while the test
/// reads program parameters, and a model - flagged by [LoopTrace::live] - when
/// it reads live machine state.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopTrace {
    pub kind: LoopKind,
    /// The line that opens the loop: the `while`, `repeat` or `do` line.
    pub line: usize,
    /// The line whose evaluation ends an iteration: the `while` line, the
    /// `repeat` line, or a `do`'s closing `while`.
    pub test_line: usize,
    /// The last line of the body, so a caller can tell a running line is inside
    /// this loop.
    pub end_line: usize,
    /// The parameters the test reads, as the interpreter keys them: `i` for
    /// `#<i>`, `1` for `#1`.
    pub variables: Vec<String>,
    /// One entry per iteration the preview ran, in run order. Capped at
    /// [LOOP_TRACE_LIMIT] entries: a loop that runs longer is a program the
    /// preview is not going to describe anyway.
    pub iterations: Vec<Vec<f64>>,
    /// The test reads live machine state, so the values are the preview's model.
    pub live: bool,
}

impl LoopTrace {
    /// The loop a running line is inside, innermost first.
    pub fn containing(loops: &[Self], line: usize) -> Option<usize> {
        loops
            .iter()
            .enumerate()
            .filter(|(_, trace)| line >= trace.line && line <= trace.end_line.max(trace.line))
            .min_by_key(|(_, trace)| trace.end_line.saturating_sub(trace.line))
            .map(|(index, _)| index)
    }

    /// The iteration a count of tests corresponds to. A `while` tests once
    /// before each pass, so its first pass is the first test; a `do` tests only
    /// at the bottom, so the pass in progress is one past the tests seen.
    pub fn iteration_index(&self, tests: usize) -> usize {
        match self.kind {
            LoopKind::While | LoopKind::Repeat => tests.saturating_sub(1),
            LoopKind::DoWhile => tests,
        }
    }

    /// The recorded values for an iteration, when the trace has them.
    pub fn values(&self, iteration: usize) -> Option<&[f64]> {
        self.iterations
            .get(iteration.min(self.iterations.len().saturating_sub(1)))
            .map(Vec::as_slice)
    }

    /// How the interpreter names a parameter: `#<i>`, `#1`.
    pub fn variable_label(key: &str) -> String {
        if key.chars().all(|ch| ch.is_ascii_digit()) {
            format!("#{key}")
        } else {
            format!("#<{key}>")
        }
    }
}

/// A loop body that runs longer than this is described by its test and its
/// first values, not by a full trace; the preview's own step limit is 200 000,
/// and one `Vec` per iteration would spend the UI's memory on nothing.
pub const LOOP_TRACE_LIMIT: usize = 4096;

/// The parameter keys a test expression reads. A computed index (`#[#1+1]`) is
/// left out: its key is not knowable before the test runs.
fn parameter_keys(test: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for token in lex_line(test) {
        if token.kind != TokenKind::Variable {
            continue;
        }
        let raw = &test[token.start..token.end];
        let key = if let Some(rest) = raw.strip_prefix("#<") {
            rest.trim_end_matches('>').trim().to_string()
        } else if let Some(rest) = raw.strip_prefix('#') {
            if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            rest.to_string()
        } else {
            continue;
        };
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// A parameter that comes from the machine rather than from the program: the
/// read-only status block and the named system parameters.
fn live_state_key(key: &str) -> bool {
    key.starts_with('_')
        || key
            .parse::<u32>()
            .is_ok_and(|index| (5000..6000).contains(&index))
}

fn record_iteration(traces: &mut [LoopTrace], index: Option<usize>, params: &Params) {
    let Some(index) = index else { return };
    let Some(trace) = traces.get_mut(index) else {
        return;
    };
    if trace.iterations.len() >= LOOP_TRACE_LIMIT {
        return;
    }
    let values = trace
        .variables
        .iter()
        .map(|key| params.get(key).copied().unwrap_or(0.0))
        .collect();
    trace.iterations.push(values);
}

/// A subroutine body the preview took from a file on `SUBROUTINE_PATH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubroutineBody {
    pub name: String,
    pub path: PathBuf,
    /// The lines the body covers in its own file, inclusive.
    pub first_line: usize,
    pub last_line: usize,
}

impl SubroutineBody {
    /// The body a controller line number falls in, when exactly one body has
    /// that line: two subroutines can number the same line, and the controller
    /// does not say which file it is in.
    pub fn named_at(subroutines: &[Self], line: usize) -> Option<&Self> {
        let mut found = subroutines
            .iter()
            .filter(|body| (body.first_line..=body.last_line).contains(&line));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

impl Preview {
    pub fn empty() -> Self {
        Self {
            segments: Vec::new(),
            bounds: None,
            line_count: 0,
            line_bounds: BTreeMap::new(),
            error: None,
            modal: ModalSummary {
                gcodes: vec!["G0".into()],
                wcs: "G54".into(),
                units: "G21".into(),
                plane: "G17".into(),
                distance_mode: "G90".into(),
                ..Default::default()
            },
            tools: Vec::new(),
            program_number: None,
            uses_live_tool: false,
            arcs: 0,
            rapids: 0,
            incomplete: false,
            incomplete_reason: None,
            subroutines: Vec::new(),
            loops: Vec::new(),
            segment_loop: Vec::new(),
        }
    }

    /// Drop a leading rapid that only approaches the program from the tool's
    /// current position. The first program move starts the drawn path; the
    /// approach from wherever the machine happened to be is not part of it.
    /// Returns whether a segment was removed.
    pub fn trim_approach(&mut self, start: [f64; 3]) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= 1e-9;
        // Keep a lone move: it is the whole program, not an approach to it.
        if self.segments.len() < 2 {
            return false;
        }
        let Some(first) = self.segments.first() else {
            return false;
        };
        if first.kind != SegmentKind::Rapid
            || !near(first.start[0], start[0])
            || !near(first.start[1], start[1])
            || !near(first.start[2], start[2])
            // A zero-length rapid draws nothing, so it needs no trimming.
            || (near(first.start[0], first.end[0])
                && near(first.start[1], first.end[1])
                && near(first.start[2], first.end[2]))
        {
            return false;
        }
        self.segments.remove(0);
        if !self.segment_loop.is_empty() {
            self.segment_loop.remove(0);
        }
        self.rapids = self.rapids.saturating_sub(1);
        let old = std::mem::take(&mut self.line_bounds);
        for (line, (from, to)) in old {
            let from = from.saturating_sub(1).min(self.segments.len());
            let to = to.saturating_sub(1).min(self.segments.len());
            self.line_bounds.insert(line, (from, to));
        }
        self.bounds = Bounds3::from_points(self.segments.iter().map(|segment| segment.end));
        true
    }

    pub fn segments_for_line(&self, line: usize) -> impl Iterator<Item = &PathSegment> {
        self.segments
            .iter()
            .filter(move |segment| segment.line == line)
    }

    pub fn selected_line(&self, line: usize) -> Bounds3 {
        let mut bounds = Bounds3::from_points(
            self.segments_for_line(line)
                .flat_map(|segment| [segment.start, segment.end]),
        )
        .unwrap_or(Bounds3 {
            min: [0.0, 0.0, 0.0],
            max: [0.0, 0.0, 0.0],
        });
        // Include arc sweeps conservatively.
        for segment in self.segments_for_line(line) {
            if let Some(arc) = &segment.arc {
                let r = arc.radius.abs();
                for axis in 0..2 {
                    if arc.plane == Plane::Xy {
                        bounds.include([arc.center[0] + r, arc.center[1], arc.center[2]]);
                        bounds.include([arc.center[0] - r, arc.center[1], arc.center[2]]);
                        bounds.include([arc.center[0], arc.center[1] + r, arc.center[2]]);
                        bounds.include([arc.center[0], arc.center[1] - r, arc.center[2]]);
                    } else {
                        let _ = axis;
                    }
                }
            }
        }
        bounds
    }
}

fn parse_number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok()
}

fn word_value(tokens: &[Token], letter: char) -> Option<f64> {
    for token in tokens {
        if token.kind == TokenKind::Comment || token.kind == TokenKind::Variable {
            continue;
        }
        let mut chars = token.text.chars();
        if let Some(first) = chars.next() {
            if first.eq_ignore_ascii_case(&letter) {
                let rest: String = chars.collect();
                return parse_number(&rest);
            }
        }
        if token.kind == TokenKind::Number {
            // Numbers after words are consumed above; bare numbers are
            // expressions and are not interpreted by this bounded parser.
            continue;
        }
    }
    None
}

fn word_int(tokens: &[Token], letter: char) -> Option<u32> {
    word_value(tokens, letter).and_then(|value| {
        if value.is_finite() && value >= 0.0 {
            Some(value as u32)
        } else {
            None
        }
    })
}

/// Whether this word ends the program. `M2` and `M30` do; the interpreter reads
/// no further line, so a file may end with notes that are not G-code.
fn is_program_end(token: &Token) -> bool {
    token.kind == TokenKind::MCode
        && matches!(
            parse_number(&token.text[1..]),
            Some(value) if value == 2.0 || value == 30.0
        )
}

/// The work system a program selects, as an index 1..=9 (`G54`..`G59.3`), or
/// `None` when it never selects one.
///
/// The *first* selection counts: it is the system the program starts cutting in,
/// and the preview draws the whole program with a single offset. Comments are
/// lexed as comments, so a `(G54)` note does not count.
pub fn program_wcs(text: &str) -> Option<u8> {
    for line in text.lines() {
        for token in lex_line(line) {
            if token.kind != TokenKind::GCode {
                continue;
            }
            let Some(value) = parse_number(&token.text[1..]) else {
                continue;
            };
            let index = if (54.0..=59.0).contains(&value) {
                Some((value - 53.0) as u8)
            } else if (59.1..=59.3).contains(&value) {
                Some(((value - 59.0) * 10.0).round() as u8 + 6)
            } else {
                None
            };
            if index.is_some() {
                return index;
            }
        }
    }
    None
}

fn format_g(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("G{}", value as i64)
    } else {
        format!("G{value}")
    }
}

fn format_m(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("M{}", value as i64)
    } else {
        format!("M{value}")
    }
}

fn apply_arc(
    start: [f64; 3],
    end: [f64; 3],
    plane: Plane,
    clockwise: bool,
    i: Option<f64>,
    j: Option<f64>,
    k: Option<f64>,
    r: Option<f64>,
) -> Option<ArcSpec> {
    let (u, v) = match plane {
        Plane::Xy => (0, 1),
        Plane::Xz => (0, 2),
        Plane::Yz => (1, 2),
        Plane::Uv => (0, 1),
        Plane::Uw => (0, 2),
        Plane::Vw => (1, 2),
    };
    let sep = |a: [f64; 3], b: [f64; 3]| {
        let dx = a[u] - b[u];
        let dy = a[v] - b[v];
        (dx * dx + dy * dy).sqrt()
    };
    let (center_u, center_v, radius) = if let Some(radius) = r {
        let chord = sep(start, end);
        if chord <= 1e-12 || radius.abs() < chord * 0.5 - 1e-9 {
            return None;
        }
        let midpoint = [(start[u] + end[u]) * 0.5, (start[v] + end[v]) * 0.5];
        let h = (radius * radius - chord * chord * 0.25).max(0.0).sqrt();
        let dx = (end[u] - start[u]) / chord;
        let dy = (end[v] - start[v]) / chord;
        let sign = if clockwise { -1.0 } else { 1.0 };
        let offset = if radius >= 0.0 { sign } else { -sign };
        (
            midpoint[0] + offset * -dy * h,
            midpoint[1] + offset * dx * h,
            radius.abs(),
        )
    } else {
        // The two center words belong to the plane: I/J in XY, I/K in XZ,
        // J/K in YZ. A lathe program writes `G3 X.. Z.. I.. K..`, and reading
        // only I/J would give that arc no center at all.
        let (offset_u, offset_v) = match plane {
            Plane::Xy | Plane::Uv => (i, j),
            Plane::Xz | Plane::Uw => (i, k),
            Plane::Yz | Plane::Vw => (j, k),
        };
        let cu = start[u] + offset_u.unwrap_or(0.0);
        let cv = start[v] + offset_v.unwrap_or(0.0);
        let radius = ((start[u] - cu).powi(2) + (start[v] - cv).powi(2)).sqrt();
        (cu, cv, radius)
    };
    let mut center = start;
    center[u] = center_u;
    center[v] = center_v;
    if radius < 1e-9 {
        return None;
    }
    let start_angle = (start[v] - center[v]).atan2(start[u] - center[u]);
    let mut end_angle = (end[v] - center[v]).atan2(end[u] - center[u]);
    if end == start {
        end_angle = start_angle
            + if clockwise {
                -std::f64::consts::TAU
            } else {
                std::f64::consts::TAU
            };
    }
    Some(ArcSpec {
        center,
        radius,
        start_angle,
        end_angle,
        clockwise,
        plane,
    })
}

/// Modal state of a drilling canned cycle (G81/G82/G83/G73).
///
/// The preview models the geometry only: X/Y moves at the current Z, a rapid
/// to the R plane, a feed from R to the Z depth, and a rapid to clear Z (the
/// initial Z for G98 when it is above R, otherwise R; always R for G99). G83's
/// pecks and G82's dwell are not drawn as separate moves; the plunge still
/// cuts.
struct DrillCycle {
    kind: u16,
    depth: f64,
    /// Retract plane (R word); it is a Z height, not a radius.
    r: f64,
    /// Z at the start of the cycle series, used by G98.
    initial_z: f64,
    /// G99 retracts to R instead of the initial Z.
    retract_to_r: bool,
}

fn push_cycle_segment(
    preview: &mut Preview,
    start: [f64; 3],
    end: [f64; 3],
    kind: SegmentKind,
    line: usize,
    tool: Option<u16>,
    pass: u32,
) {
    if kind == SegmentKind::Rapid {
        preview.rapids += 1;
    }
    preview.segments.push(PathSegment {
        start,
        end,
        kind,
        line,
        arc: None,
        tool,
    });
    preview.segment_loop.push(pass);
}

/// Hard limits on the preview interpreter. A runaway program -- a "while [1]"
/// or a subroutine that calls itself without a base case -- must stop the
/// preview, not hang the HMI. Hitting a limit marks the preview incomplete
/// instead of drawing a partial path as if it were the whole program.
pub const PREVIEW_STEP_LIMIT: usize = 200_000;

/// Nested subroutine calls the preview follows before it gives up.
pub const PREVIEW_DEPTH_LIMIT: usize = 64;

/// Interpreter parameters, keyed as written: "#<level>" is "level", "#1" is
/// "1". An unset parameter reads as 0, as in the interpreter.
type Params = BTreeMap<String, f64>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

/// Recursive-descent evaluator for the RS274 expression subset the preview
/// needs: numbers, parameters, brackets, arithmetic, the comparison operators,
/// AND/OR/XOR and the common single-argument functions.
///
/// Precedence follows the LinuxCNC operator table: power, then multiply and
/// divide, then add and subtract, then the comparisons, then the booleans. The
/// trigonometric functions take and return degrees, as the interpreter does.
struct Expression<'t, 'p> {
    raw: &'t [u8],
    pos: usize,
    params: &'p Params,
}

impl<'t, 'p> Expression<'t, 'p> {
    fn evaluate(text: &'t str, params: &'p Params) -> Result<f64, String> {
        let mut parser = Self {
            raw: text.as_bytes(),
            pos: 0,
            params,
        };
        let value = parser.expression()?;
        parser.skip_whitespace();
        if parser.pos < parser.raw.len() {
            return Err(format!(
                "unexpected text '{}'",
                String::from_utf8_lossy(&parser.raw[parser.pos..]).trim()
            ));
        }
        Ok(value)
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.raw.len() && (self.raw[self.pos] as char).is_whitespace() {
            self.pos += 1;
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.pos <= self.raw.len() && self.raw[self.pos..].starts_with(text.as_bytes())
    }

    /// A symbolic operator, longest match first.
    fn take_symbol(&mut self, symbols: &[&str]) -> Option<String> {
        self.skip_whitespace();
        for symbol in symbols {
            if self.starts_with(symbol) {
                self.pos += symbol.len();
                return Some((*symbol).to_string());
            }
        }
        None
    }

    /// A word operator, only when it stands alone. The interpreter matches
    /// these without regard to case: `lt` is `LT`.
    fn take_keyword(&mut self, words: &[&str]) -> Option<String> {
        self.skip_whitespace();
        for word in words {
            let end = self.pos + word.len();
            let Some(candidate) = self.raw.get(self.pos..end) else {
                continue;
            };
            if !candidate.eq_ignore_ascii_case(word.as_bytes()) {
                continue;
            }
            let after = self.raw.get(end).copied();
            if after.is_some_and(|byte| (byte as char).is_ascii_alphanumeric() || byte == b'_') {
                continue;
            }
            self.pos = end;
            return Some((*word).to_string());
        }
        None
    }

    fn expression(&mut self) -> Result<f64, String> {
        self.or_level()
    }

    fn or_level(&mut self) -> Result<f64, String> {
        let mut value = self.and_level()?;
        while let Some(op) = self.take_keyword(&["OR", "XOR"]) {
            let rhs = self.and_level()?;
            let left = value != 0.0;
            let right = rhs != 0.0;
            value = if op == "OR" {
                if left || right {
                    1.0
                } else {
                    0.0
                }
            } else if left != right {
                1.0
            } else {
                0.0
            };
        }
        Ok(value)
    }

    fn and_level(&mut self) -> Result<f64, String> {
        let mut value = self.compare_level()?;
        while self.take_keyword(&["AND"]).is_some() {
            let rhs = self.compare_level()?;
            value = if value != 0.0 && rhs != 0.0 { 1.0 } else { 0.0 };
        }
        Ok(value)
    }

    fn compare_level(&mut self) -> Result<f64, String> {
        let mut value = self.add_level()?;
        loop {
            let named = self.take_keyword(&["EQ", "NE", "GT", "GE", "LT", "LE"]);
            let op = match named.as_deref() {
                Some("EQ") => Some(CompareOp::Eq),
                Some("NE") => Some(CompareOp::Ne),
                Some("GT") => Some(CompareOp::Gt),
                Some("GE") => Some(CompareOp::Ge),
                Some("LT") => Some(CompareOp::Lt),
                Some("LE") => Some(CompareOp::Le),
                _ => match self
                    .take_symbol(&["==", "!=", "<=", ">=", "<", ">"])
                    .as_deref()
                {
                    Some("==") => Some(CompareOp::Eq),
                    Some("!=") => Some(CompareOp::Ne),
                    Some("<=") => Some(CompareOp::Le),
                    Some(">=") => Some(CompareOp::Ge),
                    Some("<") => Some(CompareOp::Lt),
                    Some(">") => Some(CompareOp::Gt),
                    _ => None,
                },
            };
            let Some(op) = op else { break };
            let rhs = self.add_level()?;
            let holds = match op {
                CompareOp::Eq => (value - rhs).abs() <= 1e-9,
                CompareOp::Ne => (value - rhs).abs() > 1e-9,
                CompareOp::Gt => value > rhs,
                CompareOp::Ge => value >= rhs,
                CompareOp::Lt => value < rhs,
                CompareOp::Le => value <= rhs,
            };
            value = if holds { 1.0 } else { 0.0 };
        }
        Ok(value)
    }

    fn add_level(&mut self) -> Result<f64, String> {
        let mut value = self.multiply_level()?;
        while let Some(op) = self.take_symbol(&["+", "-"]) {
            let rhs = self.multiply_level()?;
            value = if op == "+" { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    fn multiply_level(&mut self) -> Result<f64, String> {
        let mut value = self.unary()?;
        loop {
            if let Some(op) = self.take_symbol(&["*", "/"]) {
                let rhs = self.unary()?;
                if op == "*" {
                    value *= rhs;
                } else if rhs == 0.0 {
                    return Err("division by zero".to_string());
                } else {
                    value /= rhs;
                }
                continue;
            }
            if self.take_keyword(&["MOD"]).is_some() {
                let rhs = self.unary()?;
                if rhs == 0.0 {
                    return Err("division by zero".to_string());
                }
                value %= rhs;
                continue;
            }
            break;
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<f64, String> {
        if self.take_symbol(&["-"]).is_some() {
            return Ok(-self.unary()?);
        }
        if self.take_symbol(&["+"]).is_some() {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.primary()?;
        if self.take_symbol(&["**"]).is_some() {
            let exponent = self.unary()?;
            return Ok(base.powf(exponent));
        }
        Ok(base)
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        while self.pos < self.raw.len()
            && ((self.raw[self.pos] as char).is_ascii_digit() || self.raw[self.pos] == b'.')
        {
            self.pos += 1;
        }
        if matches!(self.raw.get(self.pos).copied(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            while self.pos < self.raw.len()
                && ((self.raw[self.pos] as char).is_ascii_digit()
                    || self.raw[self.pos] == b'+'
                    || self.raw[self.pos] == b'-')
            {
                self.pos += 1;
            }
        }
        let text = String::from_utf8_lossy(&self.raw[start..self.pos]).into_owned();
        text.parse::<f64>()
            .map_err(|_| format!("'{}' is not a number", text))
    }

    fn parameter(&mut self) -> Result<f64, String> {
        self.pos += 1;
        match self.raw.get(self.pos).copied() {
            Some(b'<') => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.raw.len() && self.raw[self.pos] != b'>' {
                    self.pos += 1;
                }
                let name = String::from_utf8_lossy(&self.raw[start..self.pos])
                    .trim()
                    .to_string();
                if self.raw.get(self.pos).copied() == Some(b'>') {
                    self.pos += 1;
                }
                Ok(self.params.get(&name).copied().unwrap_or(0.0))
            }
            Some(b'[') => {
                self.pos += 1;
                let index = self.expression()?;
                self.skip_whitespace();
                if self.raw.get(self.pos).copied() == Some(b']') {
                    self.pos += 1;
                }
                Ok(self.params.get(&label_key(index)).copied().unwrap_or(0.0))
            }
            Some(byte) if (byte as char).is_ascii_digit() => {
                let start = self.pos;
                while self.pos < self.raw.len() && (self.raw[self.pos] as char).is_ascii_digit() {
                    self.pos += 1;
                }
                let name = String::from_utf8_lossy(&self.raw[start..self.pos]).into_owned();
                Ok(self.params.get(&name).copied().unwrap_or(0.0))
            }
            _ => Err("# without a parameter".to_string()),
        }
    }

    fn function(&mut self, name: &str) -> Result<f64, String> {
        let argument = self.primary()?;
        let value = match name {
            "SQRT" => argument.sqrt(),
            "ABS" => argument.abs(),
            "SIN" => argument.to_radians().sin(),
            "COS" => argument.to_radians().cos(),
            "TAN" => argument.to_radians().tan(),
            "ASIN" => argument.asin().to_degrees(),
            "ACOS" => argument.acos().to_degrees(),
            "ATAN" => argument.atan().to_degrees(),
            "FIX" => argument.trunc(),
            "FUP" => argument.ceil(),
            "ROUND" => argument.round(),
            "LN" => argument.ln(),
            "EXP" => argument.exp(),
            other => return Err(format!("unsupported function {}", other)),
        };
        Ok(value)
    }

    fn primary(&mut self) -> Result<f64, String> {
        self.skip_whitespace();
        let Some(byte) = self.raw.get(self.pos).copied() else {
            return Err("the expression is empty".to_string());
        };
        if byte == b'[' || byte == b'(' {
            let close = if byte == b'[' { b']' } else { b')' };
            self.pos += 1;
            let value = self.expression()?;
            self.skip_whitespace();
            if self.raw.get(self.pos).copied() != Some(close) {
                return Err("unbalanced brackets".to_string());
            }
            self.pos += 1;
            return Ok(value);
        }
        if byte == b'#' {
            return self.parameter();
        }
        if (byte as char).is_ascii_digit() || byte == b'.' {
            return self.number();
        }
        if (byte as char).is_ascii_alphabetic() {
            let start = self.pos;
            while self.pos < self.raw.len()
                && ((self.raw[self.pos] as char).is_ascii_alphabetic()
                    || self.raw[self.pos] == b'_')
            {
                self.pos += 1;
            }
            let name = String::from_utf8_lossy(&self.raw[start..self.pos]).to_ascii_uppercase();
            return self.function(&name);
        }
        Err(format!("unexpected character '{}'", byte as char))
    }
}

/// Key a computed parameter number the way "#101" and "#[100+1]" name it.
fn label_key(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        format!("{}", value)
    }
}

/// A word whose value is not a literal number.
struct WordFailure {
    message: String,
    /// A syntax error stops the preview; an evaluation failure only marks it
    /// incomplete and drops the block.
    syntax: bool,
}

/// Replace every parameterised word value with its number, so the drawing code
/// below reads blocks exactly as it reads literal G-code.
fn resolve_words(tokens: &[Token], raw: &str, params: &Params) -> Result<Vec<Token>, WordFailure> {
    let mut words = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;
        if token.kind == TokenKind::Comment {
            words.push(token.clone());
            continue;
        }
        if !matches!(
            token.kind,
            TokenKind::Axis | TokenKind::Parameter | TokenKind::FeedSpeed | TokenKind::Tool
        ) {
            words.push(token.clone());
            continue;
        }
        let letter = token.text.chars().next().unwrap_or(' ');
        let rest: String = token.text.chars().skip(1).collect();
        if parse_number(&rest).is_some() {
            words.push(token.clone());
            continue;
        }
        let Some((end, expression)) = expression_at(raw, token.end) else {
            return Err(WordFailure {
                message: format!("missing value after {}", letter.to_ascii_uppercase()),
                syntax: true,
            });
        };
        let value = Expression::evaluate(&expression, params).map_err(|message| WordFailure {
            message: format!(
                "could not evaluate the value of {}: {}",
                letter.to_ascii_uppercase(),
                message
            ),
            syntax: false,
        })?;
        words.push(Token {
            kind: token.kind,
            start: token.start,
            end,
            text: format!("{}{}", letter.to_ascii_uppercase(), value),
        });
        while index < tokens.len() && tokens[index].start < end {
            index += 1;
        }
    }
    Ok(words)
}

/// The text of the value that follows a word letter when the value is not a
/// literal number: a parameter reference or a bracketed expression.
fn expression_at(raw: &str, from: usize) -> Option<(usize, String)> {
    let bytes = raw.as_bytes();
    let mut index = from;
    while index < bytes.len() && (bytes[index] as char).is_whitespace() {
        index += 1;
    }
    let start = index;
    if !matches!(bytes.get(index).copied(), Some(b'#') | Some(b'[')) {
        return None;
    }
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'[' => {
                depth += 1;
                index += 1;
            }
            b']' if depth > 0 => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    break;
                }
            }
            b'<' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'>' {
                    index += 1;
                }
                if index < bytes.len() {
                    index += 1;
                }
            }
            b'#' => {
                // A numeric parameter is digits only: in `Z#7K[...]` the K
                // starts the next word, not part of the parameter name.
                index += 1;
                while index < bytes.len() && (bytes[index] as char).is_ascii_digit() {
                    index += 1;
                }
            }
            _ if depth > 0 => index += 1,
            _ => break,
        }
    }
    let end = index.min(bytes.len());
    Some((end, raw[start..end].trim().to_string()))
}

/// Whether the `(...)` comments on a line are balanced.
///
/// A `;` comment runs to the end of the line, so a parenthesis inside it is
/// text: `;  1) z axis not altered (remains ...` must not fail the line. The
/// `;` inside `(...)` is text too.
fn comments_balanced(line: &str) -> bool {
    let mut depth = 0usize;
    for byte in line.bytes() {
        match byte {
            b';' if depth == 0 => break,
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    depth == 0
}

/// Strip comments from a line while keeping offsets: each comment character
/// becomes a space, so an expression found by offset still lines up.
fn strip_comments(line: &str) -> String {
    let mut out = Vec::with_capacity(line.len());
    let mut depth = 0usize;
    for &byte in line.as_bytes() {
        match byte {
            b';' if depth == 0 => break,
            b'(' => {
                depth += 1;
                out.push(b' ');
            }
            b')' => {
                depth = depth.saturating_sub(1);
                out.push(b' ');
            }
            _ if depth > 0 => out.push(b' '),
            _ => out.push(byte),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One statement of the preview interpreter.
#[derive(Clone)]
enum Stmt {
    Block {
        line: usize,
        raw: String,
        tokens: Vec<Token>,
    },
    Assign {
        line: usize,
        name: String,
        expr: String,
    },
    SubStart,
    SubEnd {
        line: usize,
    },
    Call {
        line: usize,
        label: String,
        args: Vec<String>,
    },
    If {
        line: usize,
        condition: String,
    },
    ElseIf {
        line: usize,
        condition: String,
    },
    Else,
    EndIf,
    While {
        line: usize,
        label: String,
        condition: String,
    },
    /// `o<label> do`: the top of a do-while loop. Its body runs at least once;
    /// the `o<label> while [condition]` at the bottom decides whether to repeat.
    Do {
        label: String,
    },
    /// The `o<label> while [condition]` that closes a `do`.
    DoWhile {
        line: usize,
        condition: String,
    },
    EndWhile,
    Repeat {
        line: usize,
        label: String,
        count: String,
    },
    EndRepeat,
    /// `o<label> break`: leave the loop this label opened.
    Break {
        line: usize,
        label: String,
    },
    /// `o<label> continue`: run the loop's test again.
    Continue {
        line: usize,
        label: String,
    },
    Goto {
        line: usize,
        target: String,
    },
    Return {
        line: usize,
    },
    /// An o-word with no keyword: a program label such as the "O1001" after a
    /// percent line. It names a place, it does nothing.
    Label,
    Unsupported {
        line: usize,
        what: String,
    },
}

/// One block the interpreter decided to run, in execution order.
struct PlannedBlock {
    line: usize,
    tokens: Vec<Token>,
    /// The loop pass this block runs in, packed as [Preview::segment_loop].
    loop_at: u32,
}

/// Pack a loop index and pass into [Preview::segment_loop]'s word.
fn pack_pass(trace: usize, pass: usize) -> u32 {
    (((trace as u32) + 1) << 16) | (pass as u32 & 0xffff)
}

/// The loop index and pass packed into [`Preview::segment_loop`].
pub fn unpack_pass(packed: u32) -> Option<(usize, usize)> {
    if packed == 0 {
        None
    } else {
        Some((((packed >> 16) - 1) as usize, (packed & 0xffff) as usize))
    }
}

struct PlannedPreview {
    blocks: Vec<PlannedBlock>,
    error: Option<SyntaxIssue>,
    incomplete: Option<String>,
    subroutines: Vec<SubroutineBody>,
    loops: Vec<LoopTrace>,
}

/// Split an o-word line into its label and the rest, as the interpreter reads
/// it: "o1000 sub", "o<probe_z_minus_wco> call [...]".
fn o_word(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('o').or_else(|| line.strip_prefix('O'))?;
    // The label may follow a space: `o <sub> call`.
    let rest = rest.trim_start();
    if let Some(rest) = rest.strip_prefix('<') {
        let end = rest.find('>')?;
        Some((
            rest[..end].trim().to_string(),
            rest[end + 1..].trim().to_string(),
        ))
    } else {
        let end = rest
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        Some((rest[..end].to_string(), rest[end..].trim().to_string()))
    }
}

/// The keyword of an o-word block, lowercased, and the rest of the line.
fn keyword_and_payload(rest: &str) -> (String, String) {
    let rest = rest.trim();
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (
        rest[..end].to_ascii_lowercase(),
        rest[end..].trim().to_string(),
    )
}

/// The bracketed arguments of a call, one expression each.
fn bracket_arguments(payload: &str) -> Vec<String> {
    let bytes = payload.as_bytes();
    let mut args = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if (bytes[index] as char).is_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index] == b'[' {
            let start = index + 1;
            let mut depth = 0usize;
            while index < bytes.len() {
                match bytes[index] {
                    b'[' => {
                        depth += 1;
                        index += 1;
                    }
                    b']' => {
                        depth = depth.saturating_sub(1);
                        index += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => index += 1,
                }
            }
            let end = index.saturating_sub(1).max(start);
            args.push(payload[start.min(end)..end].trim().to_string());
            continue;
        }
        let start = index;
        while index < bytes.len() && !(bytes[index] as char).is_whitespace() {
            index += 1;
        }
        args.push(payload[start..index].trim().to_string());
    }
    args
}

/// The parameter a "left hand side" names: "#<level>" or "#1".
fn parameter_key(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix('#')?;
    if let Some(rest) = rest.strip_prefix('<') {
        let end = rest.find('>')?;
        return Some(rest[..end].trim().to_string());
    }
    let end = rest
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// The assignment equals sign: the one after the parameter the line assigns.
///
/// Not inside brackets, and not part of a comparison. The `>` that closes
/// `#<name>` is a name delimiter, not the `>=` operator, so `#<scale>=1.5`
/// assigns exactly like `#<scale> = 1.5` does — LinuxCNC's own example programs
/// write it without the space.
fn top_level_equals(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_name = false;
    let mut name_end = None;
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'<' if depth == 0 && index > 0 && bytes[index - 1] == b'#' => in_name = true,
            b'>' if depth == 0 && in_name => {
                in_name = false;
                name_end = Some(index);
            }
            b'=' if depth == 0 => {
                let before = if index > 0 { bytes[index - 1] } else { b' ' };
                let after = bytes.get(index + 1).copied().unwrap_or(b' ');
                let closes_name = before == b'>' && name_end == Some(index - 1);
                if (closes_name || !matches!(before, b'<' | b'>' | b'!' | b'=')) && after != b'=' {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn o_word_statement(line: usize, label: &str, rest: &str) -> Stmt {
    let (keyword, payload) = keyword_and_payload(rest);
    match keyword.as_str() {
        "sub" => Stmt::SubStart,
        "endsub" => Stmt::SubEnd { line },
        "call" => Stmt::Call {
            line,
            label: label.to_string(),
            args: bracket_arguments(&payload),
        },
        "if" => Stmt::If {
            line,
            condition: payload,
        },
        "elseif" => Stmt::ElseIf {
            line,
            condition: payload,
        },
        "else" => Stmt::Else,
        "endif" => Stmt::EndIf,
        "while" => Stmt::While {
            line,
            label: label.to_string(),
            condition: payload,
        },
        "do" => Stmt::Do {
            label: label.to_string(),
        },
        "endwhile" => Stmt::EndWhile,
        "repeat" => Stmt::Repeat {
            line,
            label: label.to_string(),
            count: payload,
        },
        "endrepeat" => Stmt::EndRepeat,
        "break" => Stmt::Break {
            line,
            label: label.to_string(),
        },
        "continue" => Stmt::Continue {
            line,
            label: label.to_string(),
        },
        "goto" => Stmt::Goto {
            line,
            target: payload,
        },
        "return" => Stmt::Return { line },
        "" => Stmt::Label,
        other => Stmt::Unsupported {
            line,
            what: other.to_string(),
        },
    }
}

/// Every assignment on a line. The interpreter allows several in one block:
/// `#9=0 #10=0` sets both.
fn assignment_statements(line: usize, trimmed: &str) -> Vec<Stmt> {
    let mut out = Vec::new();
    let mut rest = trimmed;
    loop {
        let unsupported = |text: &str| Stmt::Unsupported {
            line,
            what: text.trim().to_string(),
        };
        let Some(equals) = top_level_equals(rest) else {
            out.push(unsupported(rest));
            return out;
        };
        let Some(name) = parameter_key(rest[..equals].trim()) else {
            out.push(unsupported(rest));
            return out;
        };
        let after = &rest[equals + 1..];
        let value_end = next_assignment(after);
        out.push(Stmt::Assign {
            line,
            name,
            expr: after[..value_end].trim().to_string(),
        });
        rest = after[value_end..].trim_start();
        if rest.is_empty() {
            return out;
        }
        if !rest.starts_with('#') {
            out.push(unsupported(rest));
            return out;
        }
    }
}

/// Where the next assignment of a block starts: a `#name=` at bracket depth
/// zero. `#9=#10+1` is one assignment whose value reads `#10`; `#9=0 #10=0` is
/// two.
fn next_assignment(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'#' if depth == 0 => {
                let mut end = index + 1;
                if bytes.get(end) == Some(&b'<') {
                    while end < bytes.len() && bytes[end] != b'>' {
                        end += 1;
                    }
                    end = (end + 1).min(bytes.len());
                } else {
                    while end < bytes.len() && (bytes[end] as char).is_ascii_digit() {
                        end += 1;
                    }
                }
                let mut equals = end;
                while equals < bytes.len() && (bytes[equals] as char).is_whitespace() {
                    equals += 1;
                }
                if equals > index + 1
                    && bytes.get(equals) == Some(&b'=')
                    && bytes.get(equals + 1) != Some(&b'=')
                {
                    return index;
                }
            }
            _ => {}
        }
        index += 1;
    }
    text.len()
}

/// Pair every `o<label> do` with the `o<label> while` that closes it, so the
/// planner and the run see the bottom test instead of a second loop.
fn pair_do_loops(statements: &mut [Stmt]) {
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut bottoms: Vec<usize> = Vec::new();
    for index in 0..statements.len() {
        match &statements[index] {
            Stmt::Do { label } => open.push((label.clone(), index)),
            Stmt::While { label, .. } if open.last().is_some_and(|(open, _)| open == label) => {
                open.pop();
                bottoms.push(index);
            }
            _ => {}
        }
    }
    for index in bottoms {
        let replacement = match &statements[index] {
            Stmt::While {
                line,
                label: _,
                condition,
            } => Stmt::DoWhile {
                line: *line,
                condition: condition.clone(),
            },
            _ => continue,
        };
        statements[index] = replacement;
    }
}

fn goto_label(payload: &str) -> String {
    let text = payload.trim();
    let text = text
        .strip_prefix('o')
        .or_else(|| text.strip_prefix('O'))
        .unwrap_or(text);
    if let Some(rest) = text.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            return rest[..end].trim().to_string();
        }
    }
    text.trim().to_string()
}

/// The directories `[RS274NGC] SUBROUTINE_PATH` names: where the interpreter
/// looks for the file behind an `o<name> call` the program does not define.
#[derive(Clone, Debug, Default)]
pub struct Subroutines {
    dirs: Vec<PathBuf>,
}

impl Subroutines {
    pub fn new(dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            dirs: dirs.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// The text of `<name>.ngc` along the search path. A numeric o-word names a
    /// label in the program, never a file.
    fn read(&self, name: &str) -> Option<(PathBuf, String)> {
        if name.is_empty()
            || name.chars().all(|ch| ch.is_ascii_digit())
            || name.contains(['/', '\\'])
            || name.contains("..")
        {
            return None;
        }
        let file = format!("{name}.ngc");
        self.dirs.iter().find_map(|dir| {
            let path = dir.join(&file);
            std::fs::read_to_string(&path).ok().map(|text| (path, text))
        })
    }
}

impl Stmt {
    /// Rename the labels a subroutine body defines, so its `o10` cannot meet the
    /// program's `o10` once the body joins the program's statement list. A call
    /// to another file keeps its plain name and is imported on its own.
    fn rename(&mut self, prefix: &str, local: &BTreeSet<String>) {
        match self {
            Stmt::Call { label, .. } => {
                if local.contains(label) {
                    *label = format!("{prefix}{label}");
                }
            }
            Stmt::While { label, .. }
            | Stmt::Do { label }
            | Stmt::Repeat { label, .. }
            | Stmt::Break { label, .. }
            | Stmt::Continue { label, .. } => *label = format!("{prefix}{label}"),
            Stmt::Goto { target, .. } => *target = format!("{prefix}{target}"),
            _ => {}
        }
    }
}

/// The body of the subroutine `name` in a parsed file: the statements between
/// its `sub` and the matching `endsub`.
fn local_sub(statements: &[Stmt], labels: &[Option<String>], name: &str) -> Option<(usize, usize)> {
    let start = labels
        .iter()
        .position(|label| label.as_deref() == Some(name))?;
    if !matches!(statements.get(start), Some(Stmt::SubStart)) {
        return None;
    }
    let mut depth = 0usize;
    for (index, statement) in statements.iter().enumerate().skip(start) {
        match statement {
            Stmt::SubStart => depth += 1,
            Stmt::SubEnd { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start + 1, index));
                }
            }
            _ => {}
        }
    }
    None
}

/// Append the bodies of subroutines the program calls but does not define.
///
/// The interpreter resolves such a call along `SUBROUTINE_PATH`. Without this the
/// preview stops at the call and reports the path partial, which is what every
/// program using the mill's `subroutines/` (probe routines, `go_to_home`, ...)
/// did.
fn import_subroutines(
    statements: &mut Vec<Stmt>,
    labels: &mut Vec<Option<String>>,
    subs: &Subroutines,
) -> Vec<SubroutineBody> {
    let mut done: Vec<SubroutineBody> = Vec::new();
    if subs.is_empty() {
        return done;
    }
    // Only `sub` statements define a routine: a call's own label sits in the
    // label list too, and must not make the call look locally satisfied.
    let defined: BTreeSet<String> = statements
        .iter()
        .zip(labels.iter())
        .filter_map(|(statement, label)| match (statement, label) {
            (Stmt::SubStart, Some(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    let mut pending: Vec<String> = statements
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Call { label, .. } if !defined.contains(label) => Some(label.clone()),
            _ => None,
        })
        .collect();
    let mut loaded: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        let Some((path, text)) = subs.read(&name) else {
            continue;
        };
        let (foreign, foreign_labels, _) = parse_statements(&text);
        let Some((from, to)) = local_sub(&foreign, &foreign_labels, &name) else {
            continue;
        };
        let local: BTreeSet<String> = foreign
            .iter()
            .zip(&foreign_labels)
            .filter_map(|(statement, label)| match (statement, label) {
                (Stmt::SubStart, Some(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        // U+0001 cannot occur in a label the lexer produces, so the prefix is
        // unique to this file.
        let prefix = format!("\u{1}{name}\u{1}");
        let start = statements.len();
        // The body joins the program as a subroutine of its own, so a call
        // enters it at `start + 1` and its `endsub` returns to the caller.
        statements.push(Stmt::SubStart);
        labels.push(Some(name.clone()));
        for index in from..to {
            let mut statement = foreign[index].clone();
            statement.rename(&prefix, &local);
            statements.push(statement);
            labels.push(
                foreign_labels[index]
                    .clone()
                    .map(|label| format!("{prefix}{label}")),
            );
        }
        statements.push(foreign[to].clone());
        labels.push(
            foreign_labels[to]
                .clone()
                .map(|label| format!("{prefix}{label}")),
        );
        let mut first_line = usize::MAX;
        let mut last_line = 0usize;
        for statement in &foreign[from..to] {
            if let Some(line) = statement_line(statement) {
                first_line = first_line.min(line);
                last_line = last_line.max(line);
            }
        }
        done.push(SubroutineBody {
            name: name.clone(),
            path,
            first_line: if first_line == usize::MAX {
                0
            } else {
                first_line
            },
            last_line,
        });
        // A call the body makes to yet another file, too.
        for statement in &statements[start..] {
            if let Stmt::Call { label, .. } = statement {
                if !label.contains('\u{1}') && !defined.contains(label) {
                    pending.push(label.clone());
                }
            }
        }
    }
    done
}

/// The file line a statement came from, when it carries one.
fn statement_line(statement: &Stmt) -> Option<usize> {
    match statement {
        Stmt::Block { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::SubEnd { line }
        | Stmt::Call { line, .. }
        | Stmt::If { line, .. }
        | Stmt::ElseIf { line, .. }
        | Stmt::While { line, .. }
        | Stmt::DoWhile { line, .. }
        | Stmt::Repeat { line, .. }
        | Stmt::Break { line, .. }
        | Stmt::Continue { line, .. }
        | Stmt::Goto { line, .. }
        | Stmt::Return { line }
        | Stmt::Unsupported { line, .. } => Some(*line),
        _ => None,
    }
}

/// Read the file into the statements the interpreter runs. The file is read
/// once; a block is only turned into numbers when the interpreter reaches it,
/// because a parameter can change between two visits to the same line.
fn parse_statements(text: &str) -> (Vec<Stmt>, Vec<Option<String>>, Option<SyntaxIssue>) {
    let mut statements: Vec<Stmt> = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    let mut error: Option<SyntaxIssue> = None;

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if !comments_balanced(raw) {
            error = Some(SyntaxIssue {
                line,
                message: "unmatched comment parenthesis".into(),
            });
            break;
        }
        let code = strip_comments(raw);
        let trimmed = code.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((label, rest)) = o_word(trimmed) {
            statements.push(o_word_statement(line, &label, &rest));
            labels.push(Some(label));
            continue;
        }
        if trimmed.starts_with('#') {
            let parsed = assignment_statements(line, trimmed);
            labels.resize(labels.len() + parsed.len(), None);
            statements.extend(parsed);
            continue;
        }
        if trimmed.starts_with('o') || trimmed.starts_with('O') {
            // An o-word this preview cannot model, such as a computed label
            // (`o[#1*10] call`). The interpreter may still run it; mark the
            // path partial instead of calling the program broken.
            statements.push(Stmt::Unsupported {
                line,
                what: trimmed.to_string(),
            });
            labels.push(None);
            continue;
        }
        statements.push(Stmt::Block {
            line,
            raw: raw.to_string(),
            tokens: lex_line(raw),
        });
        labels.push(None);
    }

    (statements, labels, error)
}

/// Plan the preview: the program's own statements plus the subroutine bodies its
/// calls resolve to, then run them.
fn plan_preview(text: &str, subs: &Subroutines) -> PlannedPreview {
    let (mut statements, mut labels, error) = parse_statements(text);
    let bodies = import_subroutines(&mut statements, &mut labels, subs);
    plan_statements(statements, labels, bodies, error)
}

fn plan_statements(
    mut statements: Vec<Stmt>,
    labels: Vec<Option<String>>,
    bodies: Vec<SubroutineBody>,
    mut error: Option<SyntaxIssue>,
) -> PlannedPreview {
    // A `while` that closes a `do` is that loop's bottom test, not a new loop.
    pair_do_loops(&mut statements);

    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    let mut subroutines: BTreeMap<String, usize> = BTreeMap::new();
    for (index, label) in labels.iter().enumerate() {
        let Some(label) = label else { continue };
        targets.entry(label.clone()).or_insert(index);
        // A call names the same label as its sub: "o1000 call" runs the body
        // that "o1000 sub" opens, wherever that body sits in the file.
        if matches!(statements[index], Stmt::SubStart) {
            subroutines.entry(label.clone()).or_insert(index);
        }
    }

    // Branch and loop exits, resolved once with a stack per construct: after
    // skips the rest of an if chain, enter is where a false condition lands
    // (the next branch to test, or the body of an else).
    let mut after: Vec<usize> = vec![statements.len(); statements.len()];
    let mut enter: Vec<usize> = vec![statements.len(); statements.len()];
    let mut back: Vec<usize> = vec![usize::MAX; statements.len()];
    // Where `continue` sends the run: the test of a `while`, the bottom test of
    // a `do`, the decrementing `endrepeat` of a repeat.
    let mut again: Vec<usize> = vec![usize::MAX; statements.len()];
    let mut sub_stack: Vec<usize> = Vec::new();
    let mut if_stack: Vec<usize> = Vec::new();
    let mut loop_stack: Vec<usize> = Vec::new();
    for (index, statement) in statements.iter().enumerate() {
        match statement {
            Stmt::SubStart => sub_stack.push(index),
            Stmt::SubEnd { .. } => {
                if let Some(start) = sub_stack.pop() {
                    after[start] = index + 1;
                }
            }
            Stmt::If { .. } => if_stack.push(index),
            Stmt::ElseIf { .. } | Stmt::Else => {
                if let Some(previous) = if_stack.last_mut() {
                    after[*previous] = index;
                    enter[*previous] = if matches!(statement, Stmt::Else) {
                        index + 1
                    } else {
                        index
                    };
                    *previous = index;
                }
            }
            Stmt::EndIf => {
                if let Some(previous) = if_stack.pop() {
                    after[previous] = index + 1;
                    enter[previous] = index + 1;
                }
            }
            Stmt::While { .. } | Stmt::Repeat { .. } | Stmt::Do { .. } => loop_stack.push(index),
            Stmt::DoWhile { .. } => {
                if let Some(start) = loop_stack.pop() {
                    after[start] = index + 1;
                    back[index] = start + 1;
                    again[start] = index;
                }
            }
            Stmt::EndWhile | Stmt::EndRepeat => {
                if let Some(start) = loop_stack.pop() {
                    after[start] = index + 1;
                    back[index] = start;
                    // A `while` re-tests at its own statement; a repeat lands on
                    // `endrepeat`, which decrements the count.
                    again[start] = if matches!(statements[start], Stmt::Repeat { .. }) {
                        index
                    } else {
                        start
                    };
                }
            }
            _ => {}
        }
    }

    // `break` and `continue` name the loop they act on. Resolve each to its
    // jump once the loop extents are known; a statement whose label does not
    // name the enclosing loop stays unresolved and reports itself at run time.
    let mut break_at: Vec<usize> = vec![usize::MAX; statements.len()];
    let mut continue_at: Vec<usize> = vec![usize::MAX; statements.len()];
    let mut open_loops: Vec<(usize, String)> = Vec::new();
    for index in 0..statements.len() {
        match &statements[index] {
            Stmt::While { label, .. } | Stmt::Repeat { label, .. } | Stmt::Do { label } => {
                open_loops.push((index, label.clone()));
            }
            Stmt::DoWhile { .. } | Stmt::EndWhile | Stmt::EndRepeat => {
                open_loops.pop();
            }
            Stmt::Break { label, .. } | Stmt::Continue { label, .. } => {
                let Some((start, open)) = open_loops.last() else {
                    continue;
                };
                if open != label {
                    continue;
                }
                break_at[index] = after[*start];
                continue_at[index] = again[*start];
            }
            _ => {}
        }
    }

    // One trace per loop statement, in file order, so the UI can name the loop
    // a running line is in even before the preview has run it. A `do` and its
    // closing `while` are one loop: the door is the `do`, the test is the
    // `while`, and both statements map to the same trace.
    let mut traces: Vec<LoopTrace> = Vec::new();
    let mut trace_of: Vec<Option<usize>> = vec![None; statements.len()];
    for index in 0..statements.len() {
        let (kind, line, test_line, test, door) = match &statements[index] {
            Stmt::While {
                line, condition, ..
            } => (LoopKind::While, *line, *line, condition.clone(), index),
            Stmt::Do { .. } => {
                let at = after[index].saturating_sub(1);
                let Stmt::DoWhile {
                    line: test_line,
                    condition,
                } = statements.get(at).cloned().unwrap_or(Stmt::EndWhile)
                else {
                    continue;
                };
                // A `do` carries no line of its own; the body does.
                let line = (index + 1..at)
                    .find_map(|statement| statement_line(&statements[statement]))
                    .unwrap_or(test_line);
                (LoopKind::DoWhile, line, test_line, condition, at)
            }
            // A closing `while` belongs to its `do`, which was handled above.
            Stmt::DoWhile { .. } => continue,
            Stmt::Repeat { line, count, .. } => {
                (LoopKind::Repeat, *line, *line, count.clone(), index)
            }
            _ => continue,
        };
        // The body's last line: an `endwhile` carries no line of its own, so
        // walk back to the last statement that does.
        let mut end_line = test_line;
        let mut at = after[index].saturating_sub(1);
        while at > index {
            if let Some(body_line) = statement_line(&statements[at]) {
                end_line = end_line.max(body_line);
                break;
            }
            at -= 1;
        }
        let variables = parameter_keys(&test);
        let live = variables.iter().any(|key| live_state_key(key));
        let trace = traces.len();
        trace_of[index] = Some(trace);
        trace_of[door] = Some(trace);
        traces.push(LoopTrace {
            kind,
            line,
            test_line,
            end_line,
            variables,
            iterations: Vec::new(),
            live,
        });
    }

    let mut params = Params::new();
    let mut blocks: Vec<PlannedBlock> = Vec::new();
    // The loops whose body the planner is inside, innermost last. A block's pass
    // is the innermost one's: the outer pass shows when the inner loop is not
    // running, which is where a nested program spends its time between passes.
    let mut open: Vec<usize> = Vec::new();
    macro_rules! current_pass {
        () => {
            open.last().map_or(0, |&trace| {
                pack_pass(trace, traces[trace].iterations.len().saturating_sub(1))
            })
        };
    }
    macro_rules! leave_loop {
        ($trace:expr) => {
            if let Some(trace) = $trace {
                if let Some(at) = open.iter().position(|&open| open == trace) {
                    open.truncate(at);
                }
            }
        };
    }
    let mut incomplete: Option<String> = None;
    let mut calls: Vec<(usize, Params)> = Vec::new();
    let mut repeats: Vec<(usize, i64)> = Vec::new();
    let mut pc = 0usize;
    let mut steps = 0usize;

    while pc < statements.len() {
        steps += 1;
        if steps > PREVIEW_STEP_LIMIT {
            incomplete = Some(format!(
                "stopped at the {} executed block limit",
                PREVIEW_STEP_LIMIT
            ));
            break;
        }
        match &statements[pc] {
            Stmt::Block { line, raw, tokens } => {
                let mut program_end = false;
                match resolve_words(tokens, raw, &params) {
                    Ok(words) => {
                        // `M2` and `M30` end the program: the interpreter reads
                        // no further line, so prose or a comment block after the
                        // end mark is not G-code.
                        program_end = words.iter().any(is_program_end);
                        blocks.push(PlannedBlock {
                            line: *line,
                            tokens: words,
                            loop_at: current_pass!(),
                        });
                    }
                    Err(failure) if failure.syntax => {
                        error = Some(SyntaxIssue {
                            line: *line,
                            message: failure.message,
                        });
                        break;
                    }
                    Err(failure) => {
                        if incomplete.is_none() {
                            incomplete = Some(format!("line {}: {}", line, failure.message));
                        }
                    }
                }
                if program_end {
                    break;
                }
                pc += 1;
            }
            Stmt::Assign { line, name, expr } => {
                match Expression::evaluate(expr, &params) {
                    Ok(value) => {
                        params.insert(name.clone(), value);
                    }
                    Err(_) => {
                        if incomplete.is_none() {
                            incomplete = Some(format!(
                                "line {}: could not evaluate the assignment to #<{}>",
                                line, name
                            ));
                        }
                    }
                }
                pc += 1;
            }
            Stmt::SubStart => pc = after[pc],
            Stmt::SubEnd { line } | Stmt::Return { line } => match calls.pop() {
                Some((return_to, saved)) => {
                    params = saved;
                    pc = return_to + 1;
                }
                None => {
                    if incomplete.is_none() {
                        incomplete = Some(format!(
                            "line {}: end of a subroutine that was not called",
                            line
                        ));
                    }
                    break;
                }
            },
            Stmt::Call { line, label, args } => {
                let Some(start) = subroutines.get(label).copied() else {
                    if incomplete.is_none() {
                        incomplete = Some(format!("line {}: no subroutine named {}", line, label));
                    }
                    pc += 1;
                    continue;
                };
                if calls.len() >= PREVIEW_DEPTH_LIMIT {
                    incomplete = Some(format!(
                        "subroutine call depth limit {} reached at line {}",
                        PREVIEW_DEPTH_LIMIT, line
                    ));
                    break;
                }
                let mut values = Vec::with_capacity(args.len());
                let mut failed = false;
                for argument in args {
                    match Expression::evaluate(argument, &params) {
                        Ok(value) => values.push(value),
                        Err(_) => failed = true,
                    }
                }
                if failed {
                    if incomplete.is_none() {
                        incomplete = Some(format!(
                            "line {}: could not evaluate an argument of the call to {}",
                            line, label
                        ));
                    }
                    pc += 1;
                    continue;
                }
                let mut frame = params.clone();
                for (index, value) in values.iter().enumerate() {
                    frame.insert((index + 1).to_string(), *value);
                }
                calls.push((pc, std::mem::replace(&mut params, frame)));
                pc = start + 1;
            }
            Stmt::If { line, condition } => match Expression::evaluate(condition, &params) {
                Ok(value) if value != 0.0 => pc += 1,
                Ok(_) => pc = enter[pc],
                Err(_) => {
                    // An unreadable condition cannot pick a branch: skip the
                    // whole if and say the preview is incomplete.
                    if incomplete.is_none() {
                        incomplete =
                            Some(format!("line {}: could not evaluate {}", line, condition));
                    }
                    pc = after[pc];
                }
            },
            Stmt::ElseIf { line, condition } => match Expression::evaluate(condition, &params) {
                Ok(value) if value != 0.0 => pc += 1,
                Ok(_) => pc = enter[pc],
                Err(_) => {
                    if incomplete.is_none() {
                        incomplete =
                            Some(format!("line {}: could not evaluate {}", line, condition));
                    }
                    pc = after[pc];
                }
            },
            Stmt::Else => pc = after[pc],
            Stmt::EndIf => pc += 1,
            Stmt::Label => pc += 1,
            Stmt::While {
                line, condition, ..
            } => match Expression::evaluate(condition, &params) {
                Ok(value) if value != 0.0 => {
                    record_iteration(&mut traces, trace_of[pc], &params);
                    if let Some(trace) = trace_of[pc] {
                        open.push(trace);
                    }
                    pc += 1;
                }
                Ok(_) => {
                    leave_loop!(trace_of[pc]);
                    pc = after[pc];
                }
                Err(_) => {
                    if incomplete.is_none() {
                        incomplete =
                            Some(format!("line {}: could not evaluate {}", line, condition));
                    }
                    pc = after[pc];
                }
            },
            Stmt::Do { .. } => {
                record_iteration(&mut traces, trace_of[pc], &params);
                if let Some(trace) = trace_of[pc] {
                    open.push(trace);
                }
                pc += 1;
            }
            Stmt::DoWhile {
                line, condition, ..
            } => match Expression::evaluate(condition, &params) {
                // The bottom test: true repeats the body, false leaves the loop.
                Ok(value) if value != 0.0 => {
                    record_iteration(&mut traces, trace_of[pc], &params);
                    pc = back[pc];
                }
                Ok(_) => {
                    leave_loop!(trace_of[pc]);
                    pc += 1;
                }
                Err(_) => {
                    if incomplete.is_none() {
                        incomplete =
                            Some(format!("line {}: could not evaluate {}", line, condition));
                    }
                    pc += 1;
                }
            },
            Stmt::EndWhile => {
                let start = back[pc];
                pc = if start == usize::MAX { pc + 1 } else { start };
            }
            Stmt::Repeat { line, count, .. } => match Expression::evaluate(count, &params) {
                Ok(value) if value >= 1.0 => {
                    record_iteration(&mut traces, trace_of[pc], &params);
                    repeats.push((pc, value.trunc() as i64));
                    if let Some(trace) = trace_of[pc] {
                        open.push(trace);
                    }
                    pc += 1;
                }
                Ok(_) => {
                    leave_loop!(trace_of[pc]);
                    pc = after[pc];
                }
                Err(_) => {
                    if incomplete.is_none() {
                        incomplete = Some(format!("line {}: could not evaluate {}", line, count));
                    }
                    leave_loop!(trace_of[pc]);
                    pc = after[pc];
                }
            },
            Stmt::EndRepeat => {
                let start = back[pc];
                if start == usize::MAX {
                    pc += 1;
                } else {
                    let mut remaining = 0;
                    if let Some(entry) = repeats.iter_mut().rev().find(|(at, _)| *at == start) {
                        entry.1 -= 1;
                        remaining = entry.1;
                    }
                    if remaining > 0 {
                        record_iteration(&mut traces, trace_of[start], &params);
                        pc = start + 1;
                    } else {
                        leave_loop!(trace_of[start]);
                        pc += 1;
                    }
                }
            }
            Stmt::Break { line, .. } => match break_at[pc] {
                target if target != usize::MAX => pc = target,
                _ => {
                    if incomplete.is_none() {
                        incomplete = Some(format!(
                            "line {}: break does not name the enclosing loop",
                            line
                        ));
                    }
                    pc += 1;
                }
            },
            Stmt::Continue { line, .. } => match continue_at[pc] {
                target if target != usize::MAX => pc = target,
                _ => {
                    if incomplete.is_none() {
                        incomplete = Some(format!(
                            "line {}: continue does not name the enclosing loop",
                            line
                        ));
                    }
                    pc += 1;
                }
            },
            Stmt::Goto { line, target } => {
                let name = goto_label(target);
                let destination = targets.get(&name).copied().or_else(|| {
                    Expression::evaluate(target, &params)
                        .ok()
                        .and_then(|value| targets.get(&label_key(value)).copied())
                });
                match destination {
                    Some(destination) => pc = destination,
                    None => {
                        if incomplete.is_none() {
                            incomplete = Some(format!("line {}: no label {}", line, target));
                        }
                        pc += 1;
                    }
                }
            }
            Stmt::Unsupported { line, what } => {
                if incomplete.is_none() {
                    incomplete = Some(format!(
                        "line {}: unsupported o-word statement {}",
                        line, what
                    ));
                }
                pc += 1;
            }
        }
    }

    PlannedPreview {
        blocks,
        error,
        incomplete,
        subroutines: bodies,
        loops: traces,
    }
}

/// Bounded preview parser. `axis_names` selects which coordinate words matter;
/// `offset` is the active WCS offset in machine coordinates.
pub fn parse_preview(
    text: &str,
    axis_names: &[String],
    offset: Option<&AxisMap<f64>>,
    current_position: [f64; 3],
) -> Preview {
    parse_preview_with(
        text,
        axis_names,
        offset,
        current_position,
        &Subroutines::default(),
    )
}

/// The same preview, but following `o<name> call` into the machine's
/// subroutine files the way the interpreter does.
pub fn parse_preview_with(
    text: &str,
    axis_names: &[String],
    offset: Option<&AxisMap<f64>>,
    current_position: [f64; 3],
    subs: &Subroutines,
) -> Preview {
    let mut preview = Preview::empty();
    let axis_index = |name: &str| -> Option<usize> {
        match name.to_ascii_uppercase().as_str() {
            "X" => Some(0),
            "Y" => Some(1),
            "Z" => Some(2),
            _ => None,
        }
    };
    let offset_values =
        |name: &str| -> f64 { offset.and_then(|map| map.get(name).copied()).unwrap_or(0.0) };

    let mut position = current_position;
    let mut absolute = true;
    let mut plane = Plane::Xy;
    let mut motion = 0.0f64;
    let mut feed: Option<f64> = None;
    let mut speed: Option<f64> = None;
    let mut tool: Option<u16> = None;
    let mut wcs = "G54".to_string();
    let mut units = "G21".to_string();
    // G7 lathe diameter mode: the interpreter halves every X word before it
    // becomes a machine position, so a preview that draws the word as written
    // puts the whole path at twice the X the machine goes to. G8 (radius, the
    // default) leaves the words alone.
    let mut lathe_diameter = false;
    let mut saw_motion = false;
    // Drilling canned cycle: modal, one hole per block with X/Y.
    let mut drill_cycle: Option<DrillCycle> = None;
    let mut cycle_request: Option<u16> = None;
    // G98 (initial Z) is the default retract mode; G99 retracts to R.
    let mut retract_to_r = false;

    // Execution order, not file order: an o-word program calls, loops and
    // branches, so the drawn path is what the blocks do when they run.
    let PlannedPreview {
        blocks,
        error,
        incomplete,
        subroutines,
        loops,
    } = plan_preview(text, subs);
    preview.subroutines = subroutines;
    preview.loops = loops;
    let total_lines = text.lines().count();

    for block in blocks {
        let line_no = block.line;
        // Which loop pass this block runs in, for the readout that follows a
        // running line back to its pass.
        let pass = block.loop_at;
        let tokens = block.tokens;
        if tokens.is_empty() {
            continue;
        }
        let line_start_segment = preview.segments.len();

        // Modal words.
        for token in &tokens {
            if token.kind == TokenKind::GCode {
                if let Some(value) = parse_number(&token.text[1..]) {
                    if value == 0.0 || value == 1.0 || value == 2.0 || value == 3.0 {
                        motion = value;
                        // Any ordinary motion mode cancels a canned cycle.
                        drill_cycle = None;
                    } else if value == 80.0 {
                        drill_cycle = None;
                    } else if value == 98.0 {
                        retract_to_r = false;
                    } else if value == 99.0 {
                        retract_to_r = true;
                    } else if value == 81.0 || value == 82.0 || value == 83.0 || value == 73.0 {
                        cycle_request = Some(value as u16);
                    } else if value == 20.0 {
                        units = "G20".into();
                    } else if value == 21.0 {
                        units = "G21".into();
                    } else if value == 17.0 {
                        plane = Plane::Xy;
                    } else if value == 18.0 {
                        plane = Plane::Xz;
                    } else if value == 19.0 {
                        plane = Plane::Yz;
                    } else if value == 17.1 {
                        plane = Plane::Uv;
                    } else if value == 18.1 {
                        plane = Plane::Uw;
                    } else if value == 19.1 {
                        plane = Plane::Vw;
                    } else if value == 90.0 {
                        absolute = true;
                    } else if value == 91.0 {
                        absolute = false;
                    } else if value == 7.0 {
                        lathe_diameter = true;
                    } else if value == 8.0 {
                        lathe_diameter = false;
                    } else if (54.0..=59.3).contains(&value) {
                        wcs = format_g(value);
                    }
                }
            } else if token.kind == TokenKind::MCode {
                if let Some(value) = parse_number(&token.text[1..]) {
                    if !preview.modal.mcodes.iter().any(|m| m == &format_m(value)) {
                        preview.modal.mcodes.push(format_m(value));
                    }
                }
            }
        }

        // G20 words are inches; every other length in this function is a
        // machine-unit (mm) position, so each word is converted where it is
        // read. A G20 block applies from itself, as the interpreter does.
        let unit_scale = if units == "G20" { 25.4 } else { 1.0 };
        let mm = move |value: f64| value * unit_scale;

        // One X word, in machine units. Only X is a diameter; the work offset
        // an absolute word is drawn against is already in machine units.
        let dia = |value: f64| if lathe_diameter { value * 0.5 } else { value };

        if let Some(value) = word_value(&tokens, 'f') {
            feed = Some(value);
        }
        if let Some(value) = word_value(&tokens, 's') {
            speed = Some(value);
        }
        if let Some(value) = word_int(&tokens, 't') {
            if value > 0 {
                tool = Some(value as u16);
                if !preview.tools.contains(&(value as u16)) {
                    preview.tools.push(value as u16);
                }
            }
        }
        if let Some(program) = tokens
            .iter()
            .find(|token| token.kind == TokenKind::ProgramNumber)
        {
            preview.program_number = Some(program.text.clone());
        }

        let has_axis_word = tokens.iter().any(|token| {
            token.kind == TokenKind::Axis
                && token
                    .text
                    .chars()
                    .next()
                    .is_some_and(|ch| "XYZUVW".contains(ch.to_ascii_uppercase()))
        });

        // Drilling cycle: the block's X/Y is the next hole. Preliminary motion
        // moves X/Y at the current Z, then rapids to the R plane; the feed is
        // from R to the Z depth; the retract clears to the initial Z (G98,
        // when above R) or R (G99). See G81 in the LinuxCNC G-code reference.
        let requested_cycle = cycle_request.take();
        if let Some(kind) = requested_cycle {
            match drill_cycle.as_mut() {
                Some(cycle) => cycle.kind = kind,
                None => {
                    drill_cycle = Some(DrillCycle {
                        kind,
                        depth: position[2],
                        r: position[2],
                        initial_z: position[2],
                        retract_to_r,
                    });
                }
            }
        }
        if let Some(cycle) = drill_cycle.as_mut() {
            cycle.retract_to_r = retract_to_r;
            // R first: in incremental mode the Z depth is measured from R.
            if let Some(value) = word_value(&tokens, 'r') {
                cycle.r = if absolute {
                    mm(value) + offset_values("Z")
                } else {
                    position[2] + mm(value)
                };
            }
            if let Some(value) = word_value(&tokens, 'z') {
                cycle.depth = if absolute {
                    mm(value) + offset_values("Z")
                } else {
                    cycle.r + mm(value)
                };
            }
            let mut hole = position;
            let mut has_hole_axis = false;
            for (letter, axis) in [('x', 0usize), ('y', 1)] {
                if let Some(value) = word_value(&tokens, letter) {
                    has_hole_axis = true;
                    let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
                    hole[axis] = if absolute {
                        value + offset_values(&letter.to_ascii_uppercase().to_string())
                    } else {
                        position[axis] + value
                    };
                }
            }
            // The first block of a cycle drills even without X/Y (Z only); a
            // modal repeat needs an X or Y to name a new hole.
            let drill = (requested_cycle.is_some() && has_axis_word) || has_hole_axis;
            if drill {
                let depth = cycle.depth;
                let r = cycle.r;
                let initial = cycle.initial_z;
                // 1. If the tool starts below R, move Z to R before X/Y.
                if position[2] < r - 1e-9 {
                    push_cycle_segment(
                        &mut preview,
                        position,
                        [position[0], position[1], r],
                        SegmentKind::Rapid,
                        line_no,
                        tool,
                        pass,
                    );
                    position[2] = r;
                }
                // 2. Rapid to the hole at the current Z.
                if (hole[0] - position[0]).abs() > 1e-9 || (hole[1] - position[1]).abs() > 1e-9 {
                    push_cycle_segment(
                        &mut preview,
                        position,
                        [hole[0], hole[1], position[2]],
                        SegmentKind::Rapid,
                        line_no,
                        tool,
                        pass,
                    );
                    position[0] = hole[0];
                    position[1] = hole[1];
                }
                // 3. Rapid down to the R plane.
                if (position[2] - r).abs() > 1e-9 {
                    push_cycle_segment(
                        &mut preview,
                        position,
                        [hole[0], hole[1], r],
                        SegmentKind::Rapid,
                        line_no,
                        tool,
                        pass,
                    );
                    position[2] = r;
                }
                // 4. Feed from R to the depth: this is the cutting move.
                if (position[2] - depth).abs() > 1e-9 {
                    push_cycle_segment(
                        &mut preview,
                        position,
                        [hole[0], hole[1], depth],
                        SegmentKind::Feed,
                        line_no,
                        tool,
                        pass,
                    );
                    position[2] = depth;
                }
                // 5. Rapid to clear Z: G98 keeps the initial Z when it is
                // above R, G99 (and a lower initial Z) clear to R.
                let clear = if !cycle.retract_to_r && initial > r + 1e-9 {
                    initial
                } else {
                    r
                };
                if (position[2] - clear).abs() > 1e-9 {
                    push_cycle_segment(
                        &mut preview,
                        position,
                        [hole[0], hole[1], clear],
                        SegmentKind::Rapid,
                        line_no,
                        tool,
                        pass,
                    );
                }
                position = [hole[0], hole[1], clear];
                saw_motion = true;
                preview
                    .line_bounds
                    .insert(line_no, (line_start_segment, preview.segments.len()));
                continue;
            }
            // A G81/G82/G83/G73 line with no axis words only selects the mode.
            continue;
        }

        let is_motion =
            (motion == 0.0 || motion == 1.0 || motion == 2.0 || motion == 3.0) && has_axis_word;
        if !is_motion || tokens.iter().any(|t| t.kind == TokenKind::OWord) {
            continue;
        }

        let mut target = position;
        for (axis_index_value, name) in axis_names.iter().enumerate() {
            let Some(axis) = axis_index(name) else {
                continue;
            };
            let Some(value) = word_value(&tokens, name.chars().next().unwrap_or('X')) else {
                continue;
            };
            let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
            let base = if absolute {
                value + offset_values(name)
            } else {
                position[axis] + value
            };
            target[axis] = base;
            let _ = axis_index_value;
        }
        // Handle XYZ even when axis_names is unexpected.
        for (letter, axis) in [('x', 0usize), ('y', 1), ('z', 2)] {
            if let Some(value) = word_value(&tokens, letter) {
                let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
                target[axis] = if absolute {
                    value + offset_values(&letter.to_ascii_uppercase().to_string())
                } else {
                    position[axis] + value
                };
            }
        }

        saw_motion = true;

        let start = position;
        let mut segment = PathSegment {
            start,
            end: target,
            kind: if motion == 0.0 {
                SegmentKind::Rapid
            } else if motion == 2.0 || motion == 3.0 {
                SegmentKind::Arc
            } else {
                SegmentKind::Feed
            },
            line: line_no,
            arc: None,
            tool,
        };
        if motion == 2.0 || motion == 3.0 {
            let arc = apply_arc(
                start,
                target,
                plane,
                motion == 2.0,
                word_value(&tokens, 'i').map(mm),
                word_value(&tokens, 'j').map(mm),
                word_value(&tokens, 'k').map(mm),
                word_value(&tokens, 'r').map(mm),
            );
            if arc.is_none() {
                preview.error = Some(SyntaxIssue {
                    line: line_no,
                    message: "arc missing valid I/J/K or R".into(),
                });
                break;
            }
            preview.arcs += 1;
            segment.arc = arc;
        } else if segment.kind == SegmentKind::Rapid {
            preview.rapids += 1;
        }
        preview.segments.push(segment);
        preview.segment_loop.push(pass);
        preview
            .line_bounds
            .insert(line_no, (line_start_segment, preview.segments.len()));
        position = target;
    }

    preview.line_count = total_lines;
    if preview.error.is_none() {
        preview.error = error;
    }
    // A flag the UI can show instead of drawing a partial path as if it were
    // the whole program: a skipped construct, a limit, or a block that could
    // not be evaluated.
    preview.incomplete = incomplete.is_some() || preview.error.is_some();
    preview.incomplete_reason = incomplete.or_else(|| {
        preview
            .error
            .as_ref()
            .map(|issue| format!("line {}: {}", issue.line, issue.message))
    });
    // Destination points only: the first segment starts at the tool's position
    // when the program was parsed, and including it made the program bounds (and
    // the stock) stretch out to wherever the tool happened to be, then shrink as
    // it moved in. The approach line is still drawn; it just does not define the
    // part's extents.
    preview.bounds = if saw_motion {
        Bounds3::from_points(preview.segments.iter().map(|segment| segment.end))
    } else {
        None
    };
    preview.modal = ModalSummary {
        gcodes: {
            let mut codes = vec![format_g(
                drill_cycle
                    .as_ref()
                    .map(|cycle| cycle.kind as f64)
                    .unwrap_or(motion),
            )];
            if absolute {
                codes.push("G90".into());
            } else {
                codes.push("G91".into());
            }
            codes.push(units.clone());
            codes.push(match plane {
                Plane::Xy => "G17".to_string(),
                Plane::Xz => "G18".to_string(),
                Plane::Yz => "G19".to_string(),
                Plane::Uv => "G17.1".to_string(),
                Plane::Uw => "G18.1".to_string(),
                Plane::Vw => "G19.1".to_string(),
            });
            codes.push(wcs.clone());
            codes
        },
        mcodes: preview.modal.mcodes.clone(),
        feed,
        speed,
        tool,
        wcs,
        units,
        plane: match plane {
            Plane::Xy => "G17".into(),
            Plane::Xz => "G18".into(),
            Plane::Yz => "G19".into(),
            Plane::Uv => "G17.1".into(),
            Plane::Uw => "G18.1".into(),
            Plane::Vw => "G19.1".into(),
        },
        distance_mode: if absolute { "G90".into() } else { "G91".into() },
    };
    // Normalize program number to a percent program header if present.
    preview
}

pub fn normalize_search(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != ',')
        .collect()
}

/// Find the line of the first match; returns 1-based line and byte range.
pub fn find_in_text(text: &str, needle: &str) -> Option<(usize, usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let byte = lower.find(&needle)?;
    let line = text[..byte].bytes().filter(|byte| *byte == b'\n').count() + 1;
    Some((line, byte, byte + needle.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_classifies_words_and_keeps_comments() {
        let tokens = lex_line("N10 G1 X10.5 Y-2 F300 T12 (Rough Pass) ; note");
        let kinds: Vec<TokenKind> = tokens.iter().map(|token| token.kind).collect();
        assert!(kinds.contains(&TokenKind::LineNumber));
        assert!(kinds.contains(&TokenKind::GCode));
        assert!(kinds.contains(&TokenKind::Axis));
        assert!(kinds.contains(&TokenKind::FeedSpeed));
        assert!(kinds.contains(&TokenKind::Tool));
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == TokenKind::Comment)
                .count(),
            2
        );
        let comment = tokens
            .iter()
            .find(|token| token.kind == TokenKind::Comment)
            .unwrap();
        assert_eq!(comment.display(), "(Rough Pass)");
    }

    #[test]
    fn the_demo_program_has_sane_bounds() {
        let text = include_str!("../assets/demo_mill.ngc");
        let preview = parse_preview(
            text,
            &["X".into(), "Y".into(), "Z".into()],
            None,
            [0.0, 0.0, 0.0],
        );
        println!("bounds = {:?}", preview.bounds);
        println!("segments = {}", preview.segments.len());
        let bounds = preview.bounds.expect("bounds");
        assert!(bounds.size()[0] > 100.0, "{bounds:?}");
    }

    #[test]
    fn the_program_bounds_ignore_where_the_tool_was() {
        // The tool stands far away; the program only reaches X20 Y10 Z-2. The
        // bounds must describe the part, not the approach from the tool.
        let preview = parse_preview(
            "G21 G90\nG0 X10 Y5 Z0\nG1 X20 Y10 Z-2 F100\n",
            &["X".into(), "Y".into(), "Z".into()],
            None,
            [500.0, -400.0, 300.0],
        );
        let bounds = preview.bounds.expect("bounds");
        assert!(bounds.max[0] <= 20.0 + 1e-6, "{bounds:?}");
        assert!(bounds.max[1] <= 10.0 + 1e-6, "{bounds:?}");
        assert!(bounds.min[2] >= -2.0 - 1e-6, "{bounds:?}");
        assert!(bounds.min[0] >= 10.0 - 1e-6, "{bounds:?}");
    }

    #[test]
    fn parser_builds_path_and_bounds() {
        let text = "G21 G90 G54\nG0 X0 Y0 Z5\nG1 Z-2 F300\nG1 X80 Y0 F850\nG2 X80 Y60 R30\n";
        let preview = parse_preview(text, &["X".into(), "Y".into(), "Z".into()], None, [0.0; 3]);
        assert!(preview.error.is_none());
        assert_eq!(preview.segments.len(), 4);
        assert_eq!(preview.arcs, 1);
        assert_eq!(preview.rapids, 1);
        let bounds = preview.bounds.unwrap();
        assert!(bounds.min[2] <= -2.0);
        assert!(bounds.max[0] >= 80.0);
    }

    #[test]
    fn parser_rejects_malformed_arc_and_keeps_error_line() {
        let text = "G1 X1 Y1\nG2 X2 Y2\n";
        let preview = parse_preview(text, &["X".into(), "Y".into()], None, [0.0; 3]);
        assert!(preview.error.is_some());
        assert_eq!(preview.error.unwrap().line, 2);
    }

    #[test]
    fn parser_applies_active_wcs_offset() {
        let text = "G1 X10 Y0\n";
        let mut offset = AxisMap::new();
        offset.insert("X", 100.0);
        offset.insert("Y", 50.0);
        let preview = parse_preview(text, &["X".into(), "Y".into()], Some(&offset), [0.0; 3]);
        assert_eq!(preview.segments[0].end[0], 110.0);
        assert_eq!(preview.segments[0].end[1], 50.0);
    }

    #[test]
    fn search_normalizes_spaces_and_commas() {
        assert_eq!(normalize_search("G2, G3"), "g2g3");
        assert_eq!(find_in_text("a\nG2 G3\n", "g2").unwrap().0, 2);
    }

    #[test]
    fn arc_with_r_produces_arc_spec() {
        let text = "G0 X0 Y0\nG2 X10 Y0 R5\n";
        let preview = parse_preview(text, &["X".into(), "Y".into()], None, [0.0; 3]);
        let arc = preview.segments[1].arc.as_ref().unwrap();
        assert!((arc.radius - 5.0).abs() < 1e-6);
    }

    #[test]
    fn program_wcs_reads_the_first_selection() {
        assert_eq!(program_wcs("G21 G90 G54 G17\nG0 X0\n"), Some(1));
        assert_eq!(
            program_wcs("(G54 is only a note here)\nG21\nG59.1 G0 X0\n"),
            Some(7)
        );
        assert_eq!(
            program_wcs("G55\nG54\n"),
            Some(2),
            "the first selection wins"
        );
        assert_eq!(program_wcs("G21 G90\nG0 X0\n"), None);
        assert_eq!(program_wcs(""), None);
    }

    fn preview_axes() -> Vec<String> {
        vec!["X".to_string(), "Y".to_string(), "Z".to_string()]
    }

    fn lathe_axes() -> Vec<String> {
        vec!["X".to_string(), "Z".to_string()]
    }

    /// G7 lathe diameter mode: the interpreter halves an X word before it
    /// becomes a machine position (checked against LinuxCNC: "G7 G53 G0 X1"
    /// lands on machine X 0.5), so the preview must halve it too or the whole
    /// path is drawn at twice the X the machine goes to.
    #[test]
    fn straight_and_arc_moves_keep_their_own_line_numbers() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../configs/gcode/linc-demo-mill.ngc"
        ))
        .unwrap();
        let axes = vec!["X".to_string(), "Y".to_string(), "Z".to_string()];
        let preview = parse_preview(&text, &axes, None, [0.0; 3]);
        for segment in preview
            .segments
            .iter()
            .filter(|s| (10..=16).contains(&s.line))
        {
            eprintln!(
                "line {} {:?} {:?} -> {:?}",
                segment.line, segment.kind, segment.start, segment.end
            );
        }
        let line11: Vec<_> = preview.segments_for_line(11).collect();
        assert_eq!(line11.len(), 1, "line 11 is one straight move");
        assert_eq!(line11[0].kind, SegmentKind::Feed);
        assert_eq!(line11[0].end, [-100.0, -210.0, -2.0]);
        let line12: Vec<_> = preview.segments_for_line(12).collect();
        assert_eq!(line12.len(), 1, "line 12 is the arc");
        assert_eq!(line12[0].kind, SegmentKind::Arc);
    }

    #[test]
    fn g7_diameter_mode_halves_the_x_words_the_interpreter_halves() {
        // Radius mode, the default, leaves the words alone.
        let preview = parse_preview("G8 G18 G90\nG0 X1.0 Z-1\n", &lathe_axes(), None, [0.0; 3]);
        let end = preview.segments.last().expect("a move").end;
        assert!((end[0] - 1.0).abs() < 1e-9, "G8 X {}", end[0]);

        // Diameter mode turns the same block into half the X. Z is not a
        // diameter and keeps its word.
        let preview = parse_preview("G7 G18 G90\nG0 X1.0 Z-1\n", &lathe_axes(), None, [0.0; 3]);
        let end = preview.segments.last().expect("a move").end;
        assert!((end[0] - 0.5).abs() < 1e-9, "G7 X {}", end[0]);
        assert!((end[2] + 1.0).abs() < 1e-9, "G7 Z {}", end[2]);

        // The work offset is a machine position already: only the word halves.
        let mut offset = AxisMap::new();
        offset.insert("X", -2.0);
        let preview = parse_preview(
            "G7 G18 G90\nG0 X1.0\n",
            &lathe_axes(),
            Some(&offset),
            [0.0; 3],
        );
        let end = preview.segments.last().expect("a move").end;
        assert!(
            (end[0] + 1.5).abs() < 1e-9,
            "offset + diameter X {}",
            end[0]
        );

        // Incremental moves carry a diameter increment, not a diameter position.
        let preview = parse_preview(
            "G8 G18 G90\nG0 X2.0\nG7 G91\nG1 X1.0\n",
            &lathe_axes(),
            None,
            [0.0; 3],
        );
        let end = preview.segments.last().expect("a move").end;
        assert!(
            (end[0] - 2.5).abs() < 1e-9,
            "G91 diameter step X {}",
            end[0]
        );

        // A later G8 restores the literal word.
        let preview = parse_preview(
            "G7 G18 G90\nG0 X1.0\nG8\nG0 X1.0\n",
            &lathe_axes(),
            None,
            [0.0; 3],
        );
        let end = preview.segments.last().expect("a move").end;
        assert!((end[0] - 1.0).abs() < 1e-9, "G8 again X {}", end[0]);
    }

    #[test]
    fn g81_moves_xy_then_feeds_from_the_r_plane_and_clears_g98() {
        let text = "G21 G90\nG0 Z5\nG0 X0 Y0\nG81 X10 Y0 Z-5 R2 F100\nX20\nG80\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let line4: Vec<_> = preview
            .segments
            .iter()
            .filter(|segment| segment.line == 4)
            .map(|segment| (segment.start, segment.end, segment.kind))
            .collect();
        assert_eq!(line4.len(), 4, "{line4:?}");
        assert_eq!(
            line4[0],
            ([0.0, 0.0, 5.0], [10.0, 0.0, 5.0], SegmentKind::Rapid)
        );
        assert_eq!(
            line4[1],
            ([10.0, 0.0, 5.0], [10.0, 0.0, 2.0], SegmentKind::Rapid)
        );
        assert_eq!(
            line4[2],
            ([10.0, 0.0, 2.0], [10.0, 0.0, -5.0], SegmentKind::Feed)
        );
        assert_eq!(
            line4[3],
            ([10.0, 0.0, -5.0], [10.0, 0.0, 5.0], SegmentKind::Rapid)
        );

        let line5: Vec<_> = preview
            .segments
            .iter()
            .filter(|segment| segment.line == 5)
            .map(|segment| (segment.start, segment.end, segment.kind))
            .collect();
        assert_eq!(line5.len(), 4, "{line5:?}");
        assert_eq!(
            line5[0],
            ([10.0, 0.0, 5.0], [20.0, 0.0, 5.0], SegmentKind::Rapid)
        );
        assert_eq!(
            line5[1],
            ([20.0, 0.0, 5.0], [20.0, 0.0, 2.0], SegmentKind::Rapid)
        );
        assert_eq!(
            line5[2],
            ([20.0, 0.0, 2.0], [20.0, 0.0, -5.0], SegmentKind::Feed)
        );
        assert_eq!(
            line5[3],
            ([20.0, 0.0, -5.0], [20.0, 0.0, 5.0], SegmentKind::Rapid)
        );
    }

    #[test]
    fn g81_g99_retracts_and_moves_at_the_r_plane() {
        let text = "G21 G90\nG0 Z5\nG99\nG81 X10 Y0 Z-5 R2\nX20\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let line4: Vec<_> = preview
            .segments
            .iter()
            .filter(|segment| segment.line == 4)
            .map(|segment| (segment.start, segment.end))
            .collect();
        assert_eq!(line4.len(), 4, "{line4:?}");
        assert_eq!(line4[3].1, [10.0, 0.0, 2.0], "G99 clears to R");
        let line5: Vec<_> = preview
            .segments
            .iter()
            .filter(|segment| segment.line == 5)
            .map(|segment| (segment.start, segment.end))
            .collect();
        assert_eq!(line5[0].0, [10.0, 0.0, 2.0], "the next hole moves at R");
        assert_eq!(line5[0].1, [20.0, 0.0, 2.0]);
        assert_eq!(line5[1].1, [20.0, 0.0, -5.0]);
        assert_eq!(line5[2].1, [20.0, 0.0, 2.0]);
    }

    #[test]
    fn g81_below_r_moves_to_r_first_and_clears_to_r() {
        let text = "G21 G90\nG0 Z1\nG81 X10 Y0 Z-2 R3\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let line3: Vec<_> = preview
            .segments
            .iter()
            .filter(|segment| segment.line == 3)
            .map(|segment| (segment.start, segment.end, segment.kind))
            .collect();
        assert_eq!(line3.len(), 4, "{line3:?}");
        assert_eq!(
            line3[0],
            ([0.0, 0.0, 1.0], [0.0, 0.0, 3.0], SegmentKind::Rapid)
        );
        assert_eq!(line3[0].1, [0.0, 0.0, 3.0], "below R: move Z before X/Y");
        assert_eq!(line3[1].0, [0.0, 0.0, 3.0]);
        assert_eq!(line3[1].1, [10.0, 0.0, 3.0]);
        assert_eq!(
            line3[2],
            ([10.0, 0.0, 3.0], [10.0, 0.0, -2.0], SegmentKind::Feed)
        );
        assert_eq!(
            line3[3].1,
            [10.0, 0.0, 3.0],
            "initial Z below R clears to R"
        );
    }

    #[test]
    fn g81_r_word_is_the_retract_plane() {
        let plain = "G21 G90\nG0 Z5\nG81 X10 Y0 Z-5 R2\n";
        let higher = "G21 G90\nG0 Z5\nG81 X10 Y0 Z-5 R4\n";
        let feed_start = |text: &str| {
            let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
            preview
                .segments
                .iter()
                .find(|segment| segment.kind == SegmentKind::Feed)
                .map(|segment| segment.start[2])
        };
        assert_eq!(feed_start(plain), Some(2.0));
        assert_eq!(feed_start(higher), Some(4.0));
    }
    #[test]
    fn trim_approach_drops_the_rapid_from_the_tool_to_the_first_move() {
        let text = "G21 G90\nG0 X10 Y0 Z5\nG1 X20 Y0 Z-1 F100\n";
        let mut preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.trim_approach([0.0, 0.0, 0.0]));
        assert_eq!(preview.segments.len(), 1, "{:?}", preview.segments);
        assert_eq!(preview.segments[0].kind, SegmentKind::Feed);
        assert_eq!(preview.segments[0].line, 3);
        assert_eq!(preview.line_bounds.get(&2), Some(&(0, 0)));
        assert_eq!(preview.line_bounds.get(&3), Some(&(0, 1)));
        assert!(preview.bounds.is_some());
    }

    #[test]
    fn trim_approach_keeps_a_feed_or_an_unrelated_rapid() {
        let feed = "G21 G90\nG1 X10 F100\n";
        let mut preview = parse_preview(feed, &preview_axes(), None, [0.0; 3]);
        assert!(!preview.trim_approach([0.0, 0.0, 0.0]));

        let rapid = "G21 G90\nG0 X10 Y0 Z5\n";
        let mut preview = parse_preview(rapid, &preview_axes(), None, [0.0; 3]);
        assert!(!preview.trim_approach([99.0, 0.0, 0.0]));
        assert_eq!(preview.segments.len(), 1);
    }

    // -- operator-reported preview defects ---------------------------------

    /// arcspiral.ngc: a G20 inch program whose continuation blocks carry only
    /// R and X/Y. 1008 lines, 999 arcs. Copy of /home/jack/CNC/tmp/gcode/.
    const ARCSPIRAL: &str = include_str!("../assets/arcspiral.ngc");
    /// flowsnake.ngc: a parameterised G21 program, 12 o-words, recursive subs.
    const FLOWSNAKE: &str = include_str!("../assets/flowsnake.ngc");

    fn expression(text: &str) -> Result<f64, String> {
        let params = Params::new();
        Expression::evaluate(text, &params)
    }

    #[test]
    fn expressions_follow_the_interpreter_operator_table() {
        assert_eq!(expression("[1 + 2 * 3]").unwrap(), 7.0);
        assert_eq!(expression("[2 ** 3 ** 2]").unwrap(), 512.0);
        assert_eq!(expression("[7 MOD 4]").unwrap(), 3.0);
        assert_eq!(expression("[3 GT 2]").unwrap(), 1.0);
        assert_eq!(expression("[3 LT 2]").unwrap(), 0.0);
        assert_eq!(expression("[1 AND 0 OR 1]").unwrap(), 1.0);
        assert_eq!(expression("[1 XOR 1]").unwrap(), 0.0);
        assert_eq!(expression("[SQRT[16] + ABS[-2]]").unwrap(), 6.0);
        assert_eq!(expression("[2 > 1]").unwrap(), 1.0);
        assert_eq!(expression("[10 / 4]").unwrap(), 2.5);
        assert_eq!(expression("[-[1 + 1]]").unwrap(), -2.0);
        assert!(expression("[1 / 0]").is_err(), "division by zero");
        assert!(expression("[NOPE[1]]").is_err(), "unknown function");
        assert!(expression("[1 +]").is_err(), "truncated expression");
    }

    /// A block without a G word continues the last G0/G1/G2/G3. The current
    /// parser already did this; the test pins it so a rewrite cannot lose it.
    #[test]
    fn a_block_without_a_g_word_continues_the_last_motion() {
        let text = "G21 G90\nG0 X0 Y0\nG2 X10 Y0 R6\nX10 Y10 R6\nX0 Y10 R6\nG1 X20 Y20\nY30\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.arcs, 3, "each continuation block is an arc");
        assert_eq!(preview.rapids, 1);
        for line in 3..=5 {
            let segments: Vec<_> = preview.segments_for_line(line).collect();
            assert_eq!(segments.len(), 1, "line {line}");
            assert_eq!(segments[0].kind, SegmentKind::Arc, "line {line}");
            assert!(segments[0].arc.is_some(), "line {line}");
        }
        assert_eq!(
            preview.segments_for_line(6).next().unwrap().kind,
            SegmentKind::Feed,
            "G1 cancels the arc mode"
        );
        let last = preview.segments.last().unwrap();
        assert_eq!(last.kind, SegmentKind::Feed);
        assert!((last.end[1] - 30.0).abs() < 1e-9, "{last:?}");
        assert!(!preview.incomplete);
    }

    /// G20 words are inches and the preview is in machine units: 1.0 inch is
    /// 25.4 mm. Before the fix the parser stored the words as written, so the
    /// whole drawing came out 25.4 times too small.
    #[test]
    fn g20_words_reach_the_preview_in_machine_units() {
        let preview = parse_preview(
            "G20 G90\nG0 X0 Y0\nG1 X1 Y1\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = preview.segments.last().unwrap().end;
        assert!((end[0] - 25.4).abs() < 1e-9, "X {}", end[0]);
        assert!((end[1] - 25.4).abs() < 1e-9, "Y {}", end[1]);

        // Z too, and an incremental word is an increment in inches.
        let preview = parse_preview(
            "G20 G90\nG0 Z1\nG91 G1 Z-0.5\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!((preview.segments[0].end[2] - 25.4).abs() < 1e-9);
        assert!((preview.segments[1].end[2] - 12.7).abs() < 1e-9);

        // An R arc keeps its radius and its endpoint in machine units.
        let preview = parse_preview(
            "G20 G90\nG0 X0 Y0\nG2 X1 Y0 R0.5\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        let last = preview.segments.last().unwrap();
        assert!((last.end[0] - 25.4).abs() < 1e-9);
        let arc = last.arc.as_ref().expect("arc");
        assert!((arc.radius - 12.7).abs() < 1e-9, "R {}", arc.radius);

        // G21 leaves the words alone, and a G20 block applies from itself.
        let preview = parse_preview(
            "G21 G90\nG1 X1\nG20\nG1 X2\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!((preview.segments[0].end[0] - 1.0).abs() < 1e-9);
        assert!((preview.segments[1].end[0] - 50.8).abs() < 1e-9);
    }

    /// The whole spiral, not its first arc. Extreme words: X-1.94789 and
    /// X1.883429 inches, Y-1.97843 and Y1.915669; times 25.4 that is the extent
    /// asserted below. The extent was read out of the file by a script that
    /// shares no code with this parser.
    #[test]
    fn arcspiral_preview_spans_the_whole_spiral_in_machine_units() {
        let preview = parse_preview(ARCSPIRAL, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.arcs, 999);
        assert_eq!(preview.rapids, 4);
        assert_eq!(preview.segments.len(), 1005);
        let bounds = preview.bounds.expect("bounds");
        assert!((bounds.min[0] - -49.476406).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.max[0] - 47.8390966).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.min[1] - -50.252122).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.max[1] - 48.6579926).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.min[2] - -2.54).abs() < 1e-9, "{bounds:?}");
        assert!((bounds.max[2] - 25.4).abs() < 1e-9, "{bounds:?}");
        assert!(bounds.size()[0] > 90.0, "{bounds:?}");
        assert!(bounds.size()[1] > 90.0, "{bounds:?}");
        // Only the last arc of the spiral comes back to the middle.
        let last = preview.segments.last().expect("segments");
        assert!(
            last.end[0].abs() < 0.1 && last.end[1].abs() < 0.1,
            "{last:?}"
        );
    }

    /// No error, a complete preview, and the path reaches the program's final
    /// point. Reference: the same recursion evaluated independently in Python.
    /// Level 5 expands to 4^5 = 1024 leaf moves, the three top-level calls make
    /// 3072, plus two G0, the G1 Z and the closing G0 = 3076 segments. The last
    /// leaf of the last call is (0.25, 1.0); the far corner is (3.75, 3.95).
    #[test]
    fn a_parameterised_program_parses_completely_and_reaches_its_final_point() {
        let preview = parse_preview(FLOWSNAKE, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.arcs, 0);
        assert_eq!(preview.segments.len(), 3076);

        let last = preview.segments.last().unwrap();
        assert!((last.end[0] - 0.25).abs() < 1e-12, "{last:?}");
        assert!((last.end[1] - 1.0).abs() < 1e-12, "{last:?}");

        let bounds = preview.bounds.expect("bounds");
        assert!((bounds.max[0] - 3.75).abs() < 1e-9, "{bounds:?}");
        assert!((bounds.max[1] - 3.95).abs() < 1e-9, "{bounds:?}");
        assert!(
            (bounds.min[1] - -0.01036297108184514).abs() < 1e-9,
            "{bounds:?}"
        );

        // Segments 0..2 are the preamble. The first leaf is five nested first
        // thirds from (.25, 1.0): each level calls its first sub with the same
        // start and the one-third point as its own end.
        assert_eq!(preview.segments[0].kind, SegmentKind::Rapid);
        assert_eq!(preview.segments[1].kind, SegmentKind::Rapid);
        assert_eq!(preview.segments[2].kind, SegmentKind::Feed);
        assert_eq!(preview.segments[3].kind, SegmentKind::Feed);
        let mut expected = 3.75;
        for _ in 0..5 {
            expected = (0.25 * 2.0 + expected) / 3.0;
        }
        let first_leaf = preview.segments[3].end;
        assert!(
            (first_leaf[0] - expected).abs() < 1e-12,
            "{first_leaf:?} {expected}"
        );
        assert!((first_leaf[1] - 1.0).abs() < 1e-12, "{first_leaf:?}");
    }

    #[test]
    fn the_common_o_word_subset_runs_in_the_preview() {
        let text = "G21 G90\n#<count> = 0\no10 while [#<count> LT 3]\n  o20 call [#<count>]\n  #<count> = [#<count> + 1]\no10 endwhile\no30 repeat [2]\n  G1 X10 F100\no30 endrepeat\no10 if [#<count> EQ 3]\n  G1 X20\no10 else\n  G1 X99\no10 endif\no40 if [#<count> EQ 0]\n  G1 X88\no40 elseif [#<count> NE 3]\n  G1 X77\no40 else\n  G1 X[60 + 6 MOD 4]\no40 endif\nM2\no20 sub\nG1 X[[#1 + 1] * 10]\no20 endsub\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![10.0, 20.0, 30.0, 10.0, 10.0, 20.0, 62.0]);
    }

    #[test]
    fn a_named_parameter_assigns_without_a_space_before_the_equals() {
        // LinuxCNC's example programs write `#<scale>=1.5`. The `>` that closes
        // the name is not the `>=` operator: read as a comparison, the
        // assignment is dropped, the parameter keeps the undefined value 0, and
        // every bracketed word collapses to zero. The shipped logo program then
        // reports an arc with no radius.
        let text = "#<scale>=1.5\nG21 G90\nG0 X0 Y0\nG2 X[1*#<scale>] Y[0] I[0.5*#<scale>] J[0.5*#<scale>]\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.arcs, 1);
        let arc = preview
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Arc)
            .expect("arc segment");
        assert!((arc.end[0] - 1.5).abs() < 1e-9, "{:?}", arc.end);
    }

    #[test]
    fn a_word_value_may_follow_a_space() {
        // LinuxCNC accepts the gap: `G0 X 16.339` and `G1 X\t0` are one word,
        // and the shipped sim configs are written that way.
        let text = "G21 G90\nG0 X 10 Y 0\nG1 X -5 Y\t2 F100\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let end = preview.segments.last().expect("a move").end;
        assert_eq!((end[0], end[1]), (-5.0, 2.0));
    }

    #[test]
    fn an_arc_reads_the_center_words_of_its_plane() {
        // G18 takes I and K, G19 takes J and K. Reading only I and J leaves
        // every lathe arc without a center.
        for (plane, words) in [("G18", "I1 K0"), ("G19", "J1 K0")] {
            let axis = if plane == "G18" { "X" } else { "Y" };
            let text = format!("G21 G90 {plane}\nG0 {axis}0 Z0\nG3 {axis}2 Z0 {words} F100\n");
            let preview = parse_preview(&text, &preview_axes(), None, [0.0; 3]);
            assert!(preview.error.is_none(), "{plane}: {:?}", preview.error);
            assert_eq!(preview.arcs, 1, "{plane}");
        }
    }

    #[test]
    fn a_semicolon_comment_may_contain_a_parenthesis() {
        let text = "G21 G90\n;notes:\n;  1) z axis not altered (remains at z)\nG0 X1\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.segments.len(), 1);
        // A nested comment is still one comment.
        let text = "G21 G90\n(a (b) c)\nG0 X1\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
    }

    #[test]
    fn the_preview_stops_at_program_end() {
        // The interpreter reads no line after `M2`, so the prose the shipped
        // `on_abort.ngc` handlers keep below it is not G-code.
        for end in ["M2", "M30"] {
            let text = format!("G21 G90\nG0 X1\n{end}\nThese are abort numbers:\n");
            let preview = parse_preview(&text, &preview_axes(), None, [0.0; 3]);
            assert!(preview.error.is_none(), "{end}: {:?}", preview.error);
            assert_eq!(preview.segments.len(), 1, "{end}");
        }
    }

    #[test]
    fn an_o_word_label_may_follow_a_space_and_a_computed_one_is_partial() {
        // `o <name> call` is legal; a computed label cannot be resolved before
        // the program runs, so the path is partial, not broken.
        let text = "G21 G90\no <thing> sub\nG1 X5 F100\no <thing> endsub\no <thing> call\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.segments.last().expect("a move").end[0], 5.0);
        let text = "G21 G90\n#1 = 2\nO[#1 * 10] call\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview.incomplete, "{:?}", preview.incomplete_reason);
    }

    #[test]
    fn comparison_words_are_not_case_sensitive() {
        // The interpreter accepts `lt`, `Le`, `NE`; the shipped programs use
        // lowercase.
        assert_eq!(expression("[3 lt 2]").unwrap(), 0.0);
        assert_eq!(expression("[3 Le 3]").unwrap(), 1.0);
        assert_eq!(expression("[3 nE 2]").unwrap(), 1.0);
        assert_eq!(expression("[1 aNd 0 oR 1]").unwrap(), 1.0);
        let text = "G21 G90\n#1 = 5\no100 if [#1 lt 7]\nG1 X2 F100\no100 endif\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.segments.last().expect("a move").end[0], 2.0);
    }

    #[test]
    fn a_do_while_loop_runs_its_body_at_least_once() {
        // `o10 do ... o10 while [cond]` tests at the bottom; the body runs once
        // even when the condition is false from the start.
        let text = "G21 G90\n#1 = 10\no10 do\nG1 X[#1] F100\n#1 = [#1 - 1]\no10 while [#1 lt 3]\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![10.0]);
        // Counting down from 3 loops until the test fails: 3, 2, 1.
        let text = "G21 G90\n#1 = 3\no10 do\nG1 X[#1] F100\n#1 = [#1 - 1]\no10 while [#1 gt 0]\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![3.0, 2.0, 1.0]);
    }

    #[test]
    fn a_block_may_assign_several_parameters() {
        let text = "G21 G90\n#9=0 #10=7\nG0 X[#9 + #10]\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.segments.last().expect("a move").end[0], 7.0);
    }

    #[test]
    fn a_numeric_parameter_stops_at_the_next_word() {
        // `Z#7K[..]`: the K is a word, not part of the parameter name.
        let text = "G21 G90\n#7 = 1\nG33 Z#7 K[sqrt[4]*[#7]] F100\nG0 X1\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
    }

    #[test]
    fn break_leaves_the_loop_and_continue_runs_its_test_again() {
        // `break` names the enclosing loop: it jumps past that loop's end.
        let text = "G21 G90\n#1 = 0\no10 while [#1 lt 10]\n#1 = [#1 + 1]\nG1 X[#1] F100\no20 if [#1 eq 2]\no10 break\no20 endif\no10 endwhile\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![1.0, 2.0]);

        // `continue` skips the rest of the body and re-tests.
        let text = "G21 G90\n#1 = 0\no10 while [#1 lt 3]\n#1 = [#1 + 1]\no20 if [#1 eq 2]\no10 continue\no20 endif\nG1 X[#1] F100\no10 endwhile\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![1.0, 3.0]);
    }

    #[test]
    fn break_ends_a_do_while_that_would_never_test_false() {
        // The shipped `do-while-break` test programs use `while [1]` and leave
        // through `break`; without it the preview runs to the step limit.
        let text = "G21 G90\n#1 = 0\no30 do\n#1 = [#1 + 1]\nG1 X[#1] F100\no40 if [#1 eq 2]\no30 break\no40 endif\no30 while [1]\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![1.0, 2.0]);
    }

    #[test]
    fn a_break_that_names_the_wrong_loop_is_reported() {
        let text = "G21 G90\n#1 = 0\no10 while [#1 lt 2]\n#1 = [#1 + 1]\no20 break\no10 endwhile\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview.incomplete, "a mismatched label is not a jump");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("break"), "{reason}");
    }

    fn plan(text: &str) -> Preview {
        let axes: Vec<String> = ["X", "Y", "Z"].iter().map(|s| s.to_string()).collect();
        parse_preview(text, &axes, None, [0.0; 3])
    }

    #[test]
    fn a_while_loop_records_its_test_parameter_per_iteration() {
        let preview = plan("o100 while [#<i> lt 3]\nG0 X#<i>\n#<i> = [#<i> + 1]\no100 endwhile\n");
        assert_eq!(preview.loops.len(), 1);
        let trace = &preview.loops[0];
        assert_eq!(trace.kind, LoopKind::While);
        assert_eq!(trace.line, 1);
        assert_eq!(trace.test_line, 1);
        assert_eq!(trace.end_line, 3);
        assert_eq!(trace.variables, vec!["i".to_string()]);
        // `#<i>` starts at 0 in an uninitialised parameter.
        assert_eq!(trace.iterations, vec![vec![0.0], vec![1.0], vec![2.0]]);
        assert!(!trace.live);
        // The running line is inside the loop body, not past it.
        assert_eq!(LoopTrace::containing(&preview.loops, 3), Some(0));
        assert_eq!(LoopTrace::containing(&preview.loops, 1), Some(0));
        assert_eq!(LoopTrace::containing(&preview.loops, 4), None);
        // The third pass is the third test.
        assert_eq!(trace.iteration_index(1), 0);
        assert_eq!(trace.iteration_index(3), 2);
        assert_eq!(trace.values(2), Some([2.0].as_slice()));
    }

    #[test]
    fn a_repeat_loop_records_one_value_per_pass() {
        let preview = plan("o200 repeat [4]\nG0 X1\no200 endrepeat\n");
        let trace = &preview.loops[0];
        assert_eq!(trace.kind, LoopKind::Repeat);
        assert_eq!(trace.line, 1);
        assert!(trace.variables.is_empty());
        // Four passes, and the body's own line as the extent.
        assert_eq!(trace.iterations.len(), 4);
        assert_eq!(trace.end_line, 2);
    }

    #[test]
    fn a_do_while_loop_records_the_first_pass_before_its_test() {
        let preview = plan("o300 do\n#<n> = [#<n> + 1]\no300 while [#<n> lt 2]\n");
        let trace = &preview.loops[0];
        assert_eq!(trace.kind, LoopKind::DoWhile);
        // The door is the `do`; a `do` carries no line, so the body's is used.
        assert_eq!(trace.line, 2);
        assert_eq!(trace.test_line, 3);
        assert_eq!(trace.end_line, 3);
        assert_eq!(trace.variables, vec!["n".to_string()]);
        // The pass that runs before the first test is recorded too.
        assert_eq!(trace.iterations, vec![vec![0.0], vec![1.0]]);
        assert_eq!(trace.iteration_index(0), 0);
        assert_eq!(trace.iteration_index(1), 1);
        assert_eq!(LoopTrace::containing(&preview.loops, 2), Some(0));
    }

    #[test]
    fn a_loop_test_that_reads_machine_state_is_marked() {
        let preview = plan("o400 while [#5070 lt 100]\nG0 X1\no400 endwhile\n");
        let trace = &preview.loops[0];
        assert_eq!(trace.variables, vec!["5070".to_string()]);
        assert!(trace.live);
        assert_eq!(LoopTrace::variable_label("5070"), "#5070");
        assert_eq!(LoopTrace::variable_label("i"), "#<i>");
    }

    #[test]
    fn a_negative_number_index_is_not_a_parameter_key() {
        // `#-1` is not a parameter the tracer can name; it must not become one.
        assert!(parameter_keys("[#-1 lt 2]").is_empty());
    }

    #[test]
    fn a_call_follows_the_subroutine_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("slot.ngc"),
            "o<slot> sub\nG1 X[#1] F100\nG1 Y5\no<slot> endsub\n",
        )
        .expect("write");
        let subs = Subroutines::new([dir.path().to_path_buf()]);
        let text = "G21 G90\nG0 X0 Y0\no<slot> call [3]\nG0 X0\n";
        let preview = parse_preview_with(text, &preview_axes(), None, [0.0; 3], &subs);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.subroutines.len(), 1);
        assert_eq!(preview.subroutines[0].name, "slot");
        assert!(preview.subroutines[0].path.ends_with("slot.ngc"));
        assert_eq!(preview.subroutines[0].first_line, 2);
        assert_eq!(preview.subroutines[0].last_line, 3);
        assert!(SubroutineBody::named_at(&preview.subroutines, 3).is_some());
        assert!(SubroutineBody::named_at(&preview.subroutines, 9).is_none());
        // Two bodies can number the same line; the controller does not say
        // which file it is in, so neither does the preview.
        let ambiguous = [
            SubroutineBody {
                name: "a".into(),
                path: PathBuf::from("a.ngc"),
                first_line: 1,
                last_line: 5,
            },
            SubroutineBody {
                name: "b".into(),
                path: PathBuf::from("b.ngc"),
                first_line: 4,
                last_line: 8,
            },
        ];
        assert!(SubroutineBody::named_at(&ambiguous, 4).is_none());
        assert!(SubroutineBody::named_at(&ambiguous, 2).is_some());
        let ends: Vec<[f64; 3]> = preview.segments.iter().map(|segment| segment.end).collect();
        assert_eq!(
            ends,
            vec![
                // The preamble moves nowhere, then the body runs, then the
                // program's closing rapid.
                [0.0, 0.0, 0.0],
                [3.0, 0.0, 0.0],
                [3.0, 5.0, 0.0],
                [0.0, 5.0, 0.0],
            ]
        );
    }

    #[test]
    fn an_imported_body_keeps_its_labels_apart_from_the_programs() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("twice.ngc"),
            "o<twice> sub\no100 while [#1 lt 5]\nG1 X[#1] F100\n#1 = [#1 + 1]\no100 endwhile\no<twice> endsub\n",
        )
        .expect("write");
        let subs = Subroutines::new([dir.path().to_path_buf()]);
        // The program uses o100 for its own sub, the imported body uses it for
        // a loop: the two must not meet.
        let text = "G21 G90\no100 sub\nG1 X1 F100\no100 endsub\no100 call\no<twice> call [3]\n";
        let preview = parse_preview_with(text, &preview_axes(), None, [0.0; 3], &subs);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![1.0, 3.0, 4.0]);
    }

    #[test]
    fn a_subroutine_the_path_does_not_provide_stays_partial() {
        let dir = tempfile::tempdir().expect("tempdir");
        let subs = Subroutines::new([dir.path().to_path_buf()]);
        let text = "G21 G90\no<nothing> call\nG0 X1\n";
        let preview = parse_preview_with(text, &preview_axes(), None, [0.0; 3], &subs);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview.incomplete, "a missing body is partial");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("nothing"), "{reason}");
        // A numeric label names a program label, not a file: no lookup.
        let text = "G21 G90\no10 sub\nG1 X1 F100\no10 endsub\no10 call\n";
        let preview = parse_preview_with(text, &preview_axes(), None, [0.0; 3], &subs);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
    }

    #[test]
    fn goto_jumps_back_to_its_label() {
        let text = "G21 G90\n#<n> = 0\no100 if [#<n> LT 2]\n  #<n> = [#<n> + 1]\n  G1 X[#<n> * 5] F100\n  o100 goto 100\no100 endif\nG1 X99\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let ends: Vec<f64> = preview
            .segments
            .iter()
            .map(|segment| segment.end[0])
            .collect();
        assert_eq!(ends, vec![5.0, 10.0, 99.0]);
    }

    #[test]
    fn a_runaway_while_stops_at_the_step_limit() {
        let text = "G21 G90\no100 while [1]\n  #<n> = [#<n> + 1]\no100 endwhile\nG1 X1\n";
        let started = std::time::Instant::now();
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let elapsed = started.elapsed();
        assert!(preview.incomplete, "a limit must be reported");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("limit"), "{reason}");
        // The exact budget, so the loop cannot run shorter or longer than it.
        assert!(reason.contains(&PREVIEW_STEP_LIMIT.to_string()), "{reason}");
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the step limit must bound the work, took {elapsed:?}"
        );
        // The block after the loop is never reached and nothing is drawn.
        assert!(preview.segments.is_empty());
    }

    #[test]
    fn a_sub_that_calls_itself_stops_at_the_depth_limit() {
        let text = "G21 G90\no200 sub\n  G1 X[#<depth>] F100\n  #<depth> = [#<depth> + 1]\n  o200 call\no200 endsub\no200 call\nG1 X99\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.incomplete, "a limit must be reported");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("depth"), "{reason}");
        assert!(
            reason.contains(&PREVIEW_DEPTH_LIMIT.to_string()),
            "{reason}"
        );
        assert!(
            preview.segments.len() <= PREVIEW_DEPTH_LIMIT + 1,
            "the depth limit bounds the work: {}",
            preview.segments.len()
        );
        assert!(!preview
            .segments
            .iter()
            .any(|segment| (segment.end[0] - 99.0).abs() < 1e-9));
    }

    /// An unsupported construct, an unreadable expression or a call with no
    /// subroutine must say the preview is incomplete rather than draw a path
    /// that looks whole.
    #[test]
    fn a_skipped_construct_is_reported_instead_of_drawn_silently() {
        for text in [
            // do/loop is not implemented.
            "G21\no1 do\nG1 X1\no1 loop\n",
            // A condition the evaluator cannot read.
            "G21\no1 while [FOO[1]]\nG1 X1\no1 endwhile\n",
            // A word value the evaluator cannot read.
            "G21\nG1 X[FOO[1]]\n",
            // A call to a subroutine the file does not define.
            "G21\no5 call\nG1 X1\n",
        ] {
            let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
            assert!(preview.incomplete, "{text:?}");
            assert!(preview.incomplete_reason.is_some(), "{text:?}");
        }
    }

    #[test]
    fn an_ordinary_program_is_not_marked_incomplete() {
        let preview = parse_preview(
            "G21 G90\nG0 X0 Y0 Z5\nG1 Z-1 F300\nG1 X10\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(!preview.incomplete);
        assert!(preview.incomplete_reason.is_none());

        let preview = parse_preview(
            include_str!("../assets/demo_mill.ngc"),
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
    }
}
