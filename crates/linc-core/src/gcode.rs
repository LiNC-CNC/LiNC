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
    /// A word that carries a value but is not an axis: the arc-centre words
    /// I, J, K, the radius R, the cycle and offset words P, Q, L, D, H, E.
    Word,
    Variable,
    Comment,
    ProgramNumber,
    LineNumber,
    /// The label of an o-word statement: `o100`, `o<probe_z>`.
    OWord,
    /// The word that tells the interpreter what the o-word does: `while`,
    /// `call`, `endrepeat`.
    Keyword,
    /// A comparison or function word inside an expression: `lt`, `and`, `sqrt`.
    Operator,
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
            | TokenKind::Word
            | TokenKind::OWord
            | TokenKind::Keyword
            | TokenKind::Operator
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
/// The o-word statement a line starts with: the label's span, the keyword that
/// follows it, and where the rest of the line begins.
///
/// The interpreter reads these words as a keyword and an expression, not as
/// G-code words, so `while` must not be lexed as the axis word W followed by a
/// run of parameter letters - which is what a word lexer sees in it.
fn o_statement(line: &str) -> Option<(usize, usize, Option<(usize, usize)>, usize)> {
    // The keywords the interpreter dispatches on (interp_read.cc, `read_o_word`
    // under `CMP`), and nothing else: a word it does not know is an error there,
    // so colouring one as a keyword would promise syntax that will not run.
    const KEYWORDS: [&str; 15] = [
        "sub",
        "endsub",
        "call",
        "do",
        "while",
        "repeat",
        "if",
        "elseif",
        "else",
        "endif",
        "break",
        "continue",
        "endwhile",
        "endrepeat",
        "return",
    ];
    let bytes = line.as_bytes();
    let mut start = 0;
    while start < bytes.len() && (bytes[start] as char).is_whitespace() {
        start += 1;
    }
    let mut i = start;
    if !matches!(bytes.get(i), Some(b'o') | Some(b'O')) {
        return None;
    }
    i += 1;
    // The label is a number, a name, or an expression: `o100`, `o<myfile>`,
    // `o[#101+2]`.
    match bytes.get(i) {
        Some(b'<') => {
            while i < bytes.len() && bytes[i] != b'>' {
                i += 1;
            }
            if i >= bytes.len() {
                return None;
            }
            i += 1;
        }
        Some(b'[') => {
            let mut depth = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        Some(byte) if byte.is_ascii_digit() => {
            while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                i += 1;
            }
        }
        _ => return None,
    }
    let label_end = i;
    let mut at = i;
    while at < bytes.len() && (bytes[at] as char).is_whitespace() {
        at += 1;
    }
    let keyword_start = at;
    while at < bytes.len() && (bytes[at] as char).is_ascii_alphabetic() {
        at += 1;
    }
    let word = &line[keyword_start..at];
    let keyword = KEYWORDS
        .iter()
        .any(|known| word.eq_ignore_ascii_case(known))
        .then_some((keyword_start, at));
    Some((start, label_end, keyword, at))
}

pub fn lex_line(line: &str) -> Vec<Token> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    // An o-word statement owns the whole line: its label, its keyword, and an
    // expression. Lexing it as G-code words is what turned `while` into W, H, I,
    // L, E and coloured each letter as an axis or a parameter.
    let mut expression = false;
    if let Some((label_start, label_end, keyword, rest)) = o_statement(line) {
        tokens.push(Token {
            kind: TokenKind::OWord,
            start: label_start,
            end: label_end,
            text: line[label_start..label_end].to_string(),
        });
        if let Some((from, to)) = keyword {
            tokens.push(Token {
                kind: TokenKind::Keyword,
                start: from,
                end: to,
                text: line[from..to].to_string(),
            });
            expression = true;
        }
        i = rest;
    }
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
        // Polar words: `@` is a radius, `^` an angle in degrees. Neither
        // letter is alphabetic, so they would otherwise land in the catch-all
        // as `Other` and the whole block would read as having no words at all.
        if ch == '@' || ch == '^' {
            let start = i;
            i += 1;
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
                kind: TokenKind::Axis,
                start,
                end: i,
                text: line[start..i].to_string(),
            });
            continue;
        }
        if expression && ch.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Operator,
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
                            TokenKind::Word
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
        // A sign belongs to a number only when a digit or a point follows it.
        // In an expression `[#1 + 2]` the plus is an operator, not a number
        // whose text happens to be "+".
        let signed = !expression
            && matches!(ch, '-' | '+')
            && bytes
                .get(i + 1)
                .is_some_and(|next| next.is_ascii_digit() || *next == b'.');
        if ch.is_ascii_digit() || signed || ch == '.' {
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
        // Symbol operators: `+`, `-`, `*`, `/`, `=`, and `**` as one run.
        if matches!(ch, '+' | '-' | '*' | '/' | '=') {
            let start = i;
            while i < bytes.len() && matches!(bytes[i], b'+' | b'-' | b'*' | b'/' | b'=') {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Operator,
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

    /// The two coordinate indices an angle in this plane runs between: the
    /// point at angle `a` is `center + r * (cos a, sin a)` on these axes.
    pub fn axes(self) -> (usize, usize) {
        match self {
            Self::Xy | Self::Uv => (0, 1),
            Self::Xz | Self::Uw => (0, 2),
            Self::Yz | Self::Vw => (1, 2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Rapid,
    Feed,
    Arc,
    /// A `G38.n` straight probe: a feed-rate move along a straight line that
    /// ends where the probe trips. The preview has no probe input to watch, so
    /// it draws the line to the programmed point, which is where the reference
    /// says an unsuccessful probe leaves the reported position.
    Probe,
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

impl ArcSpec {
    /// The quarter turns the sweep passes through. An arc is not bounded by its
    /// two ends: a full circle starts and ends on the same point, so the ends
    /// alone say it is round in neither axis.
    pub fn extent_points(&self) -> Vec<[f64; 3]> {
        const TAU: f64 = std::f64::consts::TAU;
        let (u, v) = self.plane.axes();
        let travel = if self.clockwise {
            self.start_angle - self.end_angle
        } else {
            self.end_angle - self.start_angle
        };
        // A full turn is stored as a whole TAU, whose remainder is zero, so it
        // is read before the sweep wraps.
        let sweep = if travel >= TAU - 1e-9 {
            TAU
        } else {
            travel.rem_euclid(TAU)
        };
        let mut points = Vec::with_capacity(4);
        for turn in 0..4 {
            let angle = turn as f64 * std::f64::consts::FRAC_PI_2;
            let delta = if self.clockwise {
                (self.start_angle - angle).rem_euclid(TAU)
            } else {
                (angle - self.start_angle).rem_euclid(TAU)
            };
            if delta > sweep + 1e-9 {
                continue;
            }
            let mut point = self.center;
            point[u] = self.center[u] + self.radius * angle.cos();
            point[v] = self.center[v] + self.radius * angle.sin();
            points.push(point);
        }
        points
    }
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

    /// The bounds of a drawn path: every segment end, plus what an arc reaches
    /// between them. Reading ends alone is wrong for arcs, and reads a full
    /// circle as a single point. The first segment's start is left out on
    /// purpose: it is where the tool happened to stand, not where the part is.
    pub fn from_segments(segments: &[PathSegment]) -> Option<Self> {
        let mut iter = segments.iter();
        let first = iter.next()?;
        let mut bounds = Self::from_points([first.end])?;
        for segment in std::iter::once(first).chain(iter) {
            bounds.include(segment.end);
            if let Some(arc) = &segment.arc {
                for point in arc.extent_points() {
                    bounds.include(point);
                }
            }
        }
        Some(bounds)
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
    /// For each segment, an index into [Preview::loop_stacks]: the loops a
    /// block sits inside and the pass each is on. The controller reports only
    /// lines, and a loop body repeats them, so this is what tells a running
    /// line's pass apart from the same line in another pass.
    pub segment_loop: Vec<u32>,
    /// The distinct loop stacks, outermost loop first, index 0 being "inside no
    /// loop". A nested program re-enters the same few stacks, so the table
    /// stays small while every segment still names its whole nest.
    pub loop_stacks: Vec<Vec<(u32, u32)>>,
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
    /// The subset of [LoopTrace::variables] the body assigns: the counters.
    /// The rest are the limits the test compares them against, and a status
    /// line naming a limit tells the operator nothing the program does not.
    pub counters: Vec<String>,
    /// One entry per iteration the preview ran, in run order. Capped at
    /// [LOOP_TRACE_LIMIT] entries: a loop that runs longer is a program the
    /// preview is not going to describe anyway.
    pub iterations: Vec<Vec<f64>>,
    /// The test reads live machine state, so the values are the preview's model.
    pub live: bool,
    /// Set when every pass after the first draws the same path. The controller
    /// then reports the same body lines for all of them and the segment's pass
    /// tag cannot tell two passes apart; the UI counts returns to
    /// [LoopPath::seam] instead. `None` when the passes differ or there is only
    /// one pass.
    pub repeated_path: Option<LoopPath>,
}

/// One pass of a loop whose passes all draw the same path, reduced to what a
/// pass counter needs: where a pass ends, which way the next one starts, and
/// how big the path is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopPath {
    /// The point a pass ends and the next begins.
    pub seam: [f64; 3],
    /// Unit direction the repeated body leaves the seam.
    pub heading: [f64; 3],
    /// Drawn length of one pass, in machine units.
    pub length: f64,
    /// How far the body reaches from the seam. The UI uses it to size the gate
    /// that catches the tool returning to the seam.
    pub reach: f64,
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
            named_key(rest.trim_end_matches('>'))
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

/// Whether a parameter outlives the subroutine that writes it.
///
/// "Subroutines can change the value of parameters above #30 and those changes
/// will be visible to the calling code", and a named parameter whose name
/// starts with an underscore is global; #1-#30 and a plain `#<name>` belong to
/// the call that set them. Treating every parameter as call-local lost `#61`
/// between `daisy.ngc`'s two subroutines and stopped the preview.
fn global_key(key: &str) -> bool {
    key.starts_with('_') || key.parse::<u32>().is_ok_and(|index| index >= 31)
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
            loop_stacks: vec![Vec::new()],
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
        self.bounds = Bounds3::from_segments(&self.segments);
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
        // Include arc sweeps: the ends alone leave a round feature as narrow
        // as its chord.
        for segment in self.segments_for_line(line) {
            if let Some(arc) = &segment.arc {
                for point in arc.extent_points() {
                    bounds.include(point);
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

/// One axis of the work offset the preview draws against, in machine units.
/// G10 L2/L20 can change it mid-program, so it lives in a value the parser owns
/// rather than in the map the caller passed in.
fn offset_of(offsets: &AxisMap<f64>, name: &str) -> f64 {
    offsets.get(name).copied().unwrap_or(0.0)
}

/// The system number a G5X name selects, as G10's P word spells it.
fn wcs_index(name: &str) -> Option<u8> {
    match name {
        "G54" => Some(1),
        "G55" => Some(2),
        "G56" => Some(3),
        "G57" => Some(4),
        "G58" => Some(5),
        "G59" => Some(6),
        "G59.1" => Some(7),
        "G59.2" => Some(8),
        "G59.3" => Some(9),
        _ => None,
    }
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

/// A lathe cycle whose contour is the subroutine its `Q` word names.
struct LatheProfileCycle {
    /// The G code as written, for the message a report carries.
    code: String,
    q: Option<u16>,
    /// G71/G72 remove material in passes first; G70 only finishes.
    roughing: bool,
}

/// The `G70`/`G71`/`G72` family in a block, if one is there.
///
/// These cycles do not go where their axis words point: `X` and `Z` are the
/// limits the cycle works inside, and the shape it cuts comes from the
/// subroutine `Q` names. Their `.1` and `.2` variants are the pocket-cutting
/// forms of the same two cycles.
fn lathe_profile_cycle(tokens: &[Token]) -> Option<LatheProfileCycle> {
    for token in tokens {
        if token.kind != TokenKind::GCode {
            continue;
        }
        let Some(value) = parse_number(&token.text[1..]) else {
            continue;
        };
        let (roughing, finishing) = match value {
            70.0 | 70.1 | 70.2 => (false, true),
            71.0 | 71.1 | 71.2 | 72.0 | 72.1 | 72.2 => (true, true),
            _ => (false, false),
        };
        if !finishing {
            continue;
        }
        return Some(LatheProfileCycle {
            code: format_g(value),
            q: word_int(tokens, 'q')
                .filter(|label| *label > 0)
                .map(|l| l as u16),
            roughing,
        });
    }
    None
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
    let (u, v) = plane.axes();
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

/// Resolve a block's polar words into an XY position in program coordinates.
///
/// `@` is a radius and `^` an angle in degrees, both from the program origin.
/// In absolute mode (G90) they name the position, so a lone `@` keeps the
/// current angle and a lone `^` keeps the current radius. In incremental mode
/// (G91) they are added to the current radius and angle, radius first, exactly
/// as `interp_find.cc` resolves them. Either way the point is indeterminate at
/// the origin, where neither radius nor angle is defined.
fn polar_target(
    program: [f64; 2],
    radius: Option<f64>,
    theta: Option<f64>,
    absolute: bool,
) -> Result<[f64; 2], &'static str> {
    let [mut x, mut y] = program;
    let angle = |x: f64, y: f64| y.atan2(x);
    // The interpreter compares against zero the same way: there is no tolerance
    // to pick here, the point either has an angle or it does not.
    let at_origin = x == 0.0 && y == 0.0;
    match (radius, theta) {
        (None, None) => {}
        (Some(radius), Some(theta)) if absolute => {
            let (sin, cos) = theta.to_radians().sin_cos();
            (x, y) = (radius * cos, radius * sin);
        }
        (Some(radius), None) if absolute => {
            if at_origin {
                return Err("a polar radius needs an angle when the move starts at the origin");
            }
            let (sin, cos) = angle(x, y).sin_cos();
            (x, y) = (radius * cos, radius * sin);
        }
        (None, Some(theta)) if absolute => {
            let radius = x.hypot(y);
            let (sin, cos) = theta.to_radians().sin_cos();
            (x, y) = (radius * cos, radius * sin);
        }
        (radius, theta) => {
            if at_origin {
                return Err("a polar move in incremental mode is indeterminate at the origin");
            }
            if let Some(increment) = radius {
                let (sin, cos) = angle(x, y).sin_cos();
                let radius = x.hypot(y) + increment;
                (x, y) = (radius * cos, radius * sin);
            }
            if let Some(increment) = theta {
                let (sin, cos) = (angle(x, y) + increment.to_radians()).sin_cos();
                let radius = x.hypot(y);
                (x, y) = (radius * cos, radius * sin);
            }
        }
    }
    Ok([x, y])
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

/// Tool radii the preview needs for cutter compensation.
///
/// `G41`/`G42` name a tool with the D word and the machine reads that tool's
/// radius from the tool table; `G41.1`/`G42.1` carry the radius in D directly.
/// The preview has no table of its own, so the caller hands over the radii it
/// already has. Radii are machine units (mm), like every other length here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tools {
    radii: BTreeMap<u16, f64>,
    pockets: BTreeMap<u16, u16>,
}

impl Tools {
    pub fn new(entries: impl IntoIterator<Item = (u16, f64)>) -> Self {
        Self {
            radii: entries
                .into_iter()
                .filter(|(_, radius)| radius.is_finite())
                .collect(),
            pockets: BTreeMap::new(),
        }
    }

    /// The radius of `tool`, when the table has one.
    pub fn radius(&self, tool: u16) -> Option<f64> {
        self.radii.get(&tool).copied()
    }

    /// The pocket `tool` sits in, for `#<_current_pocket>` and
    /// `#<_selected_pocket>`.
    pub fn pocket(&self, tool: u16) -> Option<u16> {
        self.pockets.get(&tool).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.radii.is_empty()
    }

    /// The radii a machine's tool table carries, from the tool rows the
    /// snapshot publishes. A tool whose diameter is on file contributes half of
    /// it; one with no diameter is left out, so `G41 D<n>` naming it is reported
    /// instead of silently drawn as an uncompensated path.
    pub fn from_rows(rows: &[crate::model::ToolRow]) -> Self {
        let mut tools = Self::new(rows.iter().filter_map(|row| {
            let diameter = row.geometry.diameter?;
            (diameter > 0.0).then_some((row.tool, diameter * 0.5))
        }));
        for row in rows {
            if let Some(pocket) = row.pocket {
                tools.pockets.insert(row.tool, pocket);
            }
        }
        tools
    }
}

/// Cutter compensation as `G41`/`G42` select it: which side of the programmed
/// path the tool centre runs on, and how far out, in machine units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Comp {
    /// `G41`: the tool centre runs to the left of the programmed direction.
    left: bool,
    radius: f64,
}

impl Comp {
    /// The in-plane unit normal the tool centre is displaced along for a move
    /// in direction `(du, dv)`. Left of travel is the direction rotated a
    /// quarter turn counter-clockwise, as the interpreter's G41 defines it.
    fn normal(self, du: f64, dv: f64, len: f64) -> (f64, f64) {
        let sign = if self.left { 1.0 } else { -1.0 };
        (sign * -dv / len, sign * du / len)
    }
}

/// One compensated move: where the tool centre enters and leaves it.
struct Compensated {
    start: [f64; 3],
    end: [f64; 3],
    arc: Option<ArcSpec>,
    /// The exit point and travel direction, for the next move to join onto.
    junction: ([f64; 2], [f64; 2]),
}

/// The two in-plane indices cutter compensation acts on, or `None` for a plane
/// the interpreter does not allow compensation in.
fn comp_axes(plane: Plane) -> Option<(usize, usize)> {
    match plane {
        Plane::Xy => Some((0, 1)),
        Plane::Xz => Some((0, 2)),
        Plane::Yz => Some((1, 2)),
        // The UV/UW/VW planes have no rotary axes the preview draws.
        _ => None,
    }
}

/// The tool-centre move for one programmed move under cutter compensation.
///
/// The programmed line is the part contour, so the tool centre runs alongside
/// it at `comp.radius`. Consecutive moves are joined where their offset lines
/// cross, which is what the interpreter does for a corner the tool can reach;
/// a corner it cannot reach (the offset lines meet far past the corner) is
/// reported instead of guessed, because a guessed join would draw a straight
/// cut through the part.
///
/// `previous` is the exit point and direction of the last compensated move, or
/// `None` on the first move after `G41`/`G42`, where the programmed start is the
/// lead-in the interpreter requires.
fn compensate(
    comp: Comp,
    plane: Plane,
    start: [f64; 3],
    end: [f64; 3],
    arc: Option<&ArcSpec>,
    previous: Option<([f64; 2], [f64; 2])>,
) -> Result<Compensated, String> {
    let (u, v) = match comp_axes(plane) {
        Some(axes) => axes,
        None => {
            return Err(
                "cutter compensation is drawn for the G17, G18 and G19 planes only".to_string(),
            )
        }
    };
    let radius = comp.radius;

    // An arc is offset by moving its radius: the tool centre sweeps a circle
    // with the same centre. Whether that circle is the smaller or the larger
    // one depends on which side the tool is on and which way the arc turns.
    if let Some(arc) = arc {
        let inside = comp.left != arc.clockwise;
        let offset_radius = if inside {
            arc.radius - radius
        } else {
            arc.radius + radius
        };
        if offset_radius <= 1e-9 {
            return Err(format!(
                "the tool radius {radius} is wider than the {} arc it must follow",
                arc.radius
            ));
        }
        let at = |angle: f64| {
            let mut point = arc.center;
            point[u] += offset_radius * angle.cos();
            point[v] += offset_radius * angle.sin();
            point
        };
        let mut offset_arc = arc.clone();
        offset_arc.radius = offset_radius;
        let mut from = at(arc.start_angle);
        let mut to = at(arc.end_angle);
        // Table rotation and the in-plane axes stay as programmed; only the two
        // in-plane coordinates move out to the tool centre.
        from[2] = start[2];
        to[2] = end[2];
        let direction = {
            let du = to[u] - from[u];
            let dv = to[v] - from[v];
            let len = du.hypot(dv);
            if len < 1e-12 {
                // A full circle: the tangent at the start angle.
                let slant = if arc.clockwise { -1.0 } else { 1.0 };
                (
                    -slant * arc.start_angle.sin(),
                    slant * arc.start_angle.cos(),
                )
            } else {
                (du / len, dv / len)
            }
        };
        return Ok(Compensated {
            start: from,
            end: to,
            arc: Some(offset_arc),
            junction: ([to[u], to[v]], [direction.0, direction.1]),
        });
    }

    let du = end[u] - start[u];
    let dv = end[v] - start[v];
    let len = du.hypot(dv);
    // A move that changes no in-plane coordinate (a plain Z plunge) leaves the
    // tool centre where the previous move put it.
    if len < 1e-12 {
        let Some((point, direction)) = previous else {
            return Ok(Compensated {
                start,
                end,
                arc: None,
                junction: ([start[u], start[v]], [0.0, 0.0]),
            });
        };
        let mut from = start;
        from[u] = point[0];
        from[v] = point[1];
        let mut to = end;
        to[u] = point[0];
        to[v] = point[1];
        return Ok(Compensated {
            start: from,
            end: to,
            arc: None,
            junction: (point, direction),
        });
    }
    let direction = [du / len, dv / len];
    let normal = comp.normal(du, dv, len);
    let exit = [end[u] + normal.0 * radius, end[v] + normal.1 * radius];
    let entry: [f64; 2] = match previous {
        // The first move after G41/G42 is the lead-in: it starts on the
        // programmed point, which must lie at least a tool radius away.
        None => {
            if len < radius - 1e-9 {
                return Err(format!(
                    "the lead-in move is shorter than the {} tool radius",
                    radius
                ));
            }
            [start[u], start[v]]
        }
        Some((point, along)) => {
            let cross = along[0] * direction[1] - along[1] * direction[0];
            if cross.abs() < 1e-9 {
                // The two moves run the same way (or fold straight back on each
                // other); either way the offset lines are parallel.
                if radius > 1e-9 && along[0] * direction[0] + along[1] * direction[1] < 0.0 {
                    return Err(
                        "the path doubles back on itself while cutter compensation is on"
                            .to_string(),
                    );
                }
                point
            } else {
                // Where the previous offset line meets this one.
                let t = ((exit[0] - point[0]) * direction[1] - (exit[1] - point[1]) * direction[0])
                    / cross;
                let meet = [point[0] + t * along[0], point[1] + t * along[1]];
                // A corner the tool cannot wrap is one whose join lands far from
                // the corner itself, which is this move's start point. LinuxCNC
                // refuses there or inserts a loop; drawing the cross-over
                // straight would cut into the part. A zero radius is the "no
                // tool loaded, no D word" case, where the tool centre is the
                // programmed line and no corner is too sharp.
                let corner = [start[u], start[v]];
                if radius > 1e-9 && (meet[0] - corner[0]).hypot(meet[1] - corner[1]) > 4.0 * radius
                {
                    return Err(
                        "the path turns too sharply for the tool to follow under cutter \
                         compensation"
                            .to_string(),
                    );
                }
                meet
            }
        }
    };
    let mut from = start;
    from[u] = entry[0];
    from[v] = entry[1];
    let mut to = end;
    to[u] = exit[0];
    to[v] = exit[1];
    Ok(Compensated {
        start: from,
        end: to,
        arc: None,
        junction: (exit, direction),
    })
}

/// Nested subroutine calls the preview follows before it gives up.
pub const PREVIEW_DEPTH_LIMIT: usize = 64;

/// The read-only status parameters [`Live`] publishes, by the name an
/// expression writes after `#<_`.
///
/// These are the ones the bundled reference documents that a preview can know:
/// the ones about this interpreter rather than about a probe that has not run.
const LIVE_NAMES: &[&str] = &[
    "_x",
    "_y",
    "_z",
    "_a",
    "_b",
    "_c",
    "_u",
    "_v",
    "_w",
    "_abs_x",
    "_abs_y",
    "_abs_z",
    "_abs_a",
    "_abs_b",
    "_abs_c",
    "_abs_u",
    "_abs_v",
    "_abs_w",
    "_metric",
    "_imperial",
    "_metric_machine",
    "_absolute",
    "_incremental",
    "_plane",
    "_motion_mode",
    "_ccomp",
    "_coord_system",
    "_feed",
    "_rpm",
    "_current_tool",
    "_current_pocket",
    "_selected_tool",
    "_selected_pocket",
    "_tool_offset",
    "_retract_r_plane",
    "_retract_old_z",
    "_inverse_time",
    "_units_per_minute",
    "_units_per_rev",
    "_ijk_absolute_mode",
    "_lathe_diameter_mode",
    "_lathe_radius_mode",
    "_spindle_on",
    "_spindle_cw",
    "_mist",
    "_flood",
    "_speed_override",
    "_feed_override",
    "_adaptive_feed",
    "_feed_hold",
    "_value",
    "_value_returned",
    "_line",
    "_call_level",
    "_task",
    "_remap_level",
    "_vmajor",
    "_vminor",
];

/// The numbered parameters that mirror part of [`LIVE_NAMES`].
const NUMBERED_LIVE: &[(&str, &str)] = &[
    ("5021", "_abs_x"),
    ("5022", "_abs_y"),
    ("5023", "_abs_z"),
    ("5024", "_abs_a"),
    ("5025", "_abs_b"),
    ("5026", "_abs_c"),
    ("5027", "_abs_u"),
    ("5028", "_abs_v"),
    ("5029", "_abs_w"),
    ("5420", "_x"),
    ("5421", "_y"),
    ("5422", "_z"),
    ("5423", "_a"),
    ("5424", "_b"),
    ("5425", "_c"),
    ("5426", "_u"),
    ("5427", "_v"),
    ("5428", "_w"),
];

/// LinuxCNC's version as `#<_vmajor>` and `#<_vminor>` report it: the major and
/// minor numbers as one number, then the patch number as a fraction. The
/// preview reads the documentation bundle it ships, so it reports the version
/// that documentation describes.
const LINUXCNC_VERSION: (f64, f64) = (2.9, 0.0);

/// The interpreter status `#<_name>` reads, as far as a preview can know it.
///
/// The planner resolves a block's expressions before the drawing pass runs, so
/// it cannot ask the drawing pass where the tool is. It keeps this instead: the
/// resolved words of every block pass through [`Live::observe`] for the modal
/// flags and [`Live::advance`] for the position, in the order the interpreter
/// runs them, and [`Live::update`] writes the result into the parameters a
/// block's expressions read.
///
/// Positions are machine units throughout, as everywhere else in this file;
/// [`Live::read`] converts to the units the program is written in.
#[derive(Clone, Debug, PartialEq)]
struct Live {
    /// Machine position: what an absolute word lands on, which is
    /// `#<_abs_x>`. `#<_x>` is this minus the work offset.
    position: [f64; 3],
    offset: [f64; 3],
    metric_program: bool,
    absolute: bool,
    /// 17, 18, 19, or 17.1/18.1/19.1 for the UV-plane modes.
    plane: f64,
    /// G0-G3, or the canned cycle that is modal, or G80.
    motion: f64,
    /// 40 off, 41 left, 42 right.
    comp: u8,
    feed: Option<f64>,
    speed: Option<f64>,
    selected: Option<u16>,
    selected_pocket: Option<u16>,
    wcs: String,
    lathe_diameter: bool,
    retract_to_r: bool,
    tool_offset: bool,
    inverse_time: bool,
    units_per_minute: bool,
    units_per_rev: bool,
    ijk_absolute: bool,
    spindle_on: bool,
    spindle_cw: bool,
    mist: bool,
    flood: bool,
    speed_override: bool,
    feed_override: bool,
    adaptive_feed: bool,
    feed_hold: bool,
    /// The R plane of the canned cycle that is modal, in machine units.
    cycle_r: Option<f64>,
    /// A, B and C in degrees. The preview draws no rotary geometry -- the shipped
    /// profiles have no rotary axis -- but a program that computes from one gets
    /// the value it programmed rather than zero.
    rotary: [f64; 3],
    /// Where the last `G38.n` probe ended, in machine units. The reference says
    /// an unsuccessful probe reports the programmed point, which is where the
    /// preview's line ends, since it has no probe input to watch.
    probe: Option<[f64; 3]>,
    /// From the planner: the block being resolved, how deep in subroutine
    /// calls that is, and what the last `return` or `endsub` handed back.
    line: usize,
    call_level: usize,
    value: f64,
    value_returned: bool,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            offset: [0.0; 3],
            metric_program: true,
            absolute: true,
            plane: 17.0,
            motion: 0.0,
            comp: 40,
            feed: None,
            speed: None,
            selected: None,
            selected_pocket: None,
            wcs: "G54".to_string(),
            lathe_diameter: false,
            retract_to_r: false,
            tool_offset: false,
            inverse_time: false,
            units_per_minute: true,
            units_per_rev: false,
            ijk_absolute: false,
            spindle_on: false,
            spindle_cw: false,
            mist: false,
            flood: false,
            // M48 is the start-up state: both overrides are on.
            speed_override: true,
            feed_override: true,
            adaptive_feed: false,
            feed_hold: false,
            cycle_r: None,
            rotary: [0.0; 3],
            probe: None,
            line: 0,
            call_level: 0,
            value: 0.0,
            value_returned: false,
        }
    }
}

impl Live {
    fn at(position: [f64; 3], offset: AxisMap<f64>) -> Self {
        Self {
            position,
            offset: [
                offset_of(&offset, "X"),
                offset_of(&offset, "Y"),
                offset_of(&offset, "Z"),
            ],
            ..Self::default()
        }
    }

    /// What `#<_name>` reads, in the program's units. `None` for a name the
    /// preview does not publish, which then keeps whatever value it had.
    fn read(&self, name: &str) -> Option<f64> {
        let unit = if self.metric_program { 1.0 } else { 1.0 / 25.4 };
        // `#<_x>` is the relative position, which includes the work offset;
        // `#<_abs_x>` is the machine position, which includes none of it. The
        // parser tracks the machine position, as the drawing pass does.
        let relative = |axis: usize| (self.position[axis] - self.offset[axis]) * unit;
        let machine = |axis: usize| self.position[axis] * unit;
        let flag = |on: bool| if on { 1.0 } else { 0.0 };
        Some(match name {
            "_x" => relative(0),
            "_y" => relative(1),
            "_z" => relative(2),
            // A rotary word is in degrees, so the program's length units do not
            // scale it. Nothing tracks a rotary work offset, so the absolute and
            // relative values are the one programmed position.
            "_a" | "_abs_a" => self.rotary[0],
            "_b" | "_abs_b" => self.rotary[1],
            "_c" | "_abs_c" => self.rotary[2],
            "_u" | "_v" | "_w" | "_abs_u" | "_abs_v" | "_abs_w" => 0.0,
            "_abs_x" => machine(0),
            "_abs_y" => machine(1),
            "_abs_z" => machine(2),
            "_metric" => flag(self.metric_program),
            "_imperial" => flag(!self.metric_program),
            // The preview is told the program, not how the machine is set up,
            // so it answers with the program's units.
            "_metric_machine" => flag(self.metric_program),
            "_absolute" => flag(self.absolute),
            "_incremental" => flag(!self.absolute),
            "_plane" => self.plane,
            "_motion_mode" => self.motion,
            "_ccomp" => self.comp as f64,
            "_coord_system" => coordinate_system(&self.wcs),
            // F and S as written, in program units, as the reference says.
            "_feed" => self.feed.unwrap_or(0.0),
            "_rpm" => self.speed.unwrap_or(0.0),
            "_current_tool" => f64::from(self.selected.unwrap_or(0)),
            "_selected_tool" => match self.selected {
                Some(tool) => f64::from(tool),
                None => -1.0,
            },
            "_current_pocket" => f64::from(self.selected_pocket.unwrap_or(0)),
            "_selected_pocket" => match self.selected_pocket {
                Some(pocket) => f64::from(pocket),
                None => -1.0,
            },
            "_tool_offset" => flag(self.tool_offset),
            "_retract_r_plane" => flag(self.retract_to_r),
            "_retract_old_z" => flag(!self.retract_to_r),
            "_inverse_time" => flag(self.inverse_time),
            "_units_per_minute" => flag(self.units_per_minute),
            "_units_per_rev" => flag(self.units_per_rev),
            "_ijk_absolute_mode" => flag(self.ijk_absolute),
            // G7/G8 as they are modal; whether the machine is a lathe at all is
            // not something the program text says.
            "_lathe_diameter_mode" => flag(self.lathe_diameter),
            "_lathe_radius_mode" => flag(!self.lathe_diameter),
            "_spindle_on" => flag(self.spindle_on),
            "_spindle_cw" => flag(self.spindle_cw),
            "_mist" => flag(self.mist),
            "_flood" => flag(self.flood),
            "_speed_override" => flag(self.speed_override),
            "_feed_override" => flag(self.feed_override),
            "_adaptive_feed" => flag(self.adaptive_feed),
            "_feed_hold" => flag(self.feed_hold),
            "_value" => self.value,
            "_value_returned" => flag(self.value_returned),
            "_line" => self.line as f64,
            "_call_level" => self.call_level as f64,
            // The preview is the interpreter that is not part of milltask, and
            // the reference says so for exactly this reason: a program can tell
            // the two apart and skip the probe test that only the real one can
            // answer.
            "_task" => 0.0,
            "_remap_level" => 0.0,
            "_vmajor" => LINUXCNC_VERSION.0,
            "_vminor" => LINUXCNC_VERSION.1,
            _ => return None,
        })
    }

    /// Write every published parameter into `params`, where an expression reads
    /// it. The keys are seeded by [`new_params`], so this allocates nothing.
    fn update(&self, params: &mut Params) {
        for name in LIVE_NAMES {
            let Some(value) = self.read(name) else {
                continue;
            };
            if let Some(slot) = params.get_mut(*name) {
                *slot = value;
            }
        }
        for (number, name) in NUMBERED_LIVE {
            let Some(value) = self.read(name) else {
                continue;
            };
            if let Some(slot) = params.get_mut(*number) {
                *slot = value;
            }
        }
        // #5061-#5069 are where a probe reports the point it stopped at. The
        // preview's probe never trips, so it reports the programmed point, the
        // way the reference says an unsuccessful probe does, and #5070 stays 0:
        // the preview interpreter's probe always fails, which is why the probe
        // macros branch on `#<_task>` before testing it.
        if let Some(probe) = self.probe {
            let unit = if self.metric_program { 1.0 } else { 1.0 / 25.4 };
            for (index, number) in ["5061", "5062", "5063"].iter().enumerate() {
                if let Some(slot) = params.get_mut(*number) {
                    *slot = probe[index] * unit;
                }
            }
        }
    }

    /// The modal flags a block sets. Called before [`Live::advance`], because a
    /// G20 in the block applies to the block's own words.
    fn observe(&mut self, tokens: &[Token]) {
        for token in tokens {
            match token.kind {
                TokenKind::GCode => {
                    let Some(value) = parse_number(&token.text[1..]) else {
                        continue;
                    };
                    match value {
                        0.0 | 1.0 | 2.0 | 3.0 => self.motion = value,
                        73.0 | 80.0 | 81.0 | 82.0 | 83.0 => self.motion = value,
                        20.0 => self.metric_program = false,
                        21.0 => self.metric_program = true,
                        7.0 => self.lathe_diameter = true,
                        8.0 => self.lathe_diameter = false,
                        17.0 => self.plane = 17.0,
                        18.0 => self.plane = 18.0,
                        19.0 => self.plane = 19.0,
                        17.1 => self.plane = 17.1,
                        18.1 => self.plane = 18.1,
                        19.1 => self.plane = 19.1,
                        40.0 => self.comp = 40,
                        41.0 | 41.1 => self.comp = 41,
                        42.0 | 42.1 => self.comp = 42,
                        43.0 | 43.1 => self.tool_offset = true,
                        49.0 => self.tool_offset = false,
                        90.0 => self.absolute = true,
                        90.1 => self.ijk_absolute = true,
                        91.0 => self.absolute = false,
                        91.1 => self.ijk_absolute = false,
                        93.0 => {
                            self.inverse_time = true;
                            self.units_per_minute = false;
                            self.units_per_rev = false;
                        }
                        94.0 => {
                            self.inverse_time = false;
                            self.units_per_minute = true;
                            self.units_per_rev = false;
                        }
                        95.0 => {
                            self.inverse_time = false;
                            self.units_per_minute = false;
                            self.units_per_rev = true;
                        }
                        98.0 => self.retract_to_r = false,
                        99.0 => self.retract_to_r = true,
                        value if (54.0..=59.3).contains(&value) => self.wcs = format_g(value),
                        _ => {}
                    }
                }
                TokenKind::MCode => {
                    let Some(value) = parse_number(&token.text[1..]) else {
                        continue;
                    };
                    let on = word_int(tokens, 'p').unwrap_or(1) != 0;
                    match value {
                        3.0 => {
                            self.spindle_on = true;
                            self.spindle_cw = true;
                        }
                        4.0 => {
                            self.spindle_on = true;
                            self.spindle_cw = false;
                        }
                        5.0 => self.spindle_on = false,
                        7.0 => self.mist = true,
                        8.0 => self.flood = true,
                        9.0 => {
                            self.mist = false;
                            self.flood = false;
                        }
                        48.0 => {
                            self.speed_override = true;
                            self.feed_override = true;
                        }
                        49.0 => {
                            self.speed_override = false;
                            self.feed_override = false;
                        }
                        50.0 => self.feed_override = on,
                        51.0 => self.speed_override = on,
                        52.0 => self.adaptive_feed = on,
                        53.0 => self.feed_hold = on,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if let Some(tool) = word_int(tokens, 't') {
            if tool > 0 {
                self.selected = Some(tool as u16);
            }
        }
        if let Some(value) = word_value(tokens, 'f') {
            self.feed = Some(value);
        }
        if let Some(value) = word_value(tokens, 's') {
            self.speed = Some(value);
        }
    }

    /// Where the block leaves the tool. Called after [`Live::observe`], so the
    /// block's own G20/G90/G7 apply to its words the way the interpreter reads
    /// them.
    fn advance(&mut self, tokens: &[Token]) {
        // Blocks that carry axis words without moving: G10 writes a coordinate
        // system, G92 sets one, G28/G30 home, G43/G44 set and G49 clears the
        // tool length. The offsets a G10 or G92 writes are not tracked here
        // either, so a program that reads `#<_abs_*>` straight after one reads
        // the offsets the preview started with.
        let mut machine_coordinates = false;
        let mut probe = false;
        for token in tokens {
            if token.kind != TokenKind::GCode {
                continue;
            }
            match parse_number(&token.text[1..]) {
                Some(10.0 | 28.0 | 30.0 | 43.0 | 43.1 | 44.0 | 49.0 | 92.0 | 92.1) => return,
                // G53 moves in machine coordinates, so its words name the
                // machine position and no offset applies to them.
                Some(53.0) => machine_coordinates = true,
                // A straight probe is a feed-rate move that is not modal, so it
                // moves wherever the motion mode was.
                Some(38.2 | 38.3 | 38.4 | 38.5) => probe = true,
                _ => {}
            }
        }
        let has_axis = tokens.iter().any(|token| {
            token.kind == TokenKind::Axis
                && token
                    .text
                    .chars()
                    .next()
                    .is_some_and(|ch| "XYZUVW".contains(ch.to_ascii_uppercase()))
        });
        let polar = word_value(tokens, '@').or_else(|| word_value(tokens, '^'));
        // Rotary words are in degrees and their absolute or incremental rule is
        // the same one the linear axes use, so they are tracked even though the
        // drawn geometry has no rotary axis.
        for (letter, axis) in [('a', 0usize), ('b', 1), ('c', 2)] {
            if let Some(value) = word_value(tokens, letter) {
                self.rotary[axis] = if self.absolute {
                    value
                } else {
                    self.rotary[axis] + value
                };
            }
        }
        if !has_axis && polar.is_none() {
            return;
        }
        if !probe && matches!(self.motion, 73.0 | 81.0 | 82.0 | 83.0) {
            self.advance_cycle(tokens);
            return;
        }
        if !probe && !matches!(self.motion, 0.0 | 1.0 | 2.0 | 3.0) {
            return;
        }
        let unit = if self.metric_program { 1.0 } else { 25.4 };
        let mm = |value: f64| value * unit;
        let dia = |value: f64| {
            if self.lathe_diameter {
                value * 0.5
            } else {
                value
            }
        };
        let mut target = self.position;
        for (letter, axis) in [('x', 0usize), ('y', 1), ('z', 2)] {
            let Some(value) = word_value(tokens, letter) else {
                continue;
            };
            let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
            target[axis] = if self.absolute {
                if machine_coordinates {
                    value
                } else {
                    value + self.offset[axis]
                }
            } else {
                self.position[axis] + value
            };
        }
        let radius = word_value(tokens, '@');
        let theta = word_value(tokens, '^');
        if radius.is_some() || theta.is_some() {
            // Polar words are read in program coordinates, so measure from the
            // program origin and add the offset back afterwards.
            let at = [
                self.position[0] - self.offset[0],
                self.position[1] - self.offset[1],
            ];
            if let Ok(next) = polar_target(at, radius.map(mm), theta, self.absolute) {
                target[0] = next[0] + self.offset[0];
                target[1] = next[1] + self.offset[1];
            }
        }
        self.position = target;
        if probe {
            // Where an unsuccessful probe leaves the reported position, and the
            // point the preview drew the line to.
            self.probe = Some([
                self.position[0] - self.offset[0],
                self.position[1] - self.offset[1],
                self.position[2] - self.offset[2],
            ]);
        }
    }

    /// A canned cycle ends clear of the part at the hole it drilled. `L` passes
    /// and polar words name more than one hole; the preview keeps the first,
    /// which is where the modal tool position is after the block.
    fn advance_cycle(&mut self, tokens: &[Token]) {
        let unit = if self.metric_program { 1.0 } else { 25.4 };
        let mm = |value: f64| value * unit;
        let dia = |value: f64| {
            if self.lathe_diameter {
                value * 0.5
            } else {
                value
            }
        };
        let initial = self.position[2];
        if let Some(value) = word_value(tokens, 'r') {
            self.cycle_r = Some(if self.absolute {
                mm(value) + self.offset[2]
            } else {
                initial + mm(value)
            });
        }
        for (letter, axis) in [('x', 0usize), ('y', 1)] {
            let Some(value) = word_value(tokens, letter) else {
                continue;
            };
            let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
            self.position[axis] = if self.absolute {
                value + self.offset[axis]
            } else {
                self.position[axis] + value
            };
        }
        let r = self.cycle_r.unwrap_or(initial);
        // G98 keeps the initial Z when it is clear of R; G99 clears to R.
        self.position[2] = if self.retract_to_r { r } else { initial.max(r) };
    }
}

/// The value `#<_coord_system>` reads: 540 for G54, 591 for G59.1.
fn coordinate_system(wcs: &str) -> f64 {
    let (major, minor) = wcs
        .trim_start_matches('G')
        .split_once('.')
        .unwrap_or((wcs.trim_start_matches('G'), "0"));
    let major = major.parse::<f64>().unwrap_or(0.0);
    let minor = minor.parse::<f64>().unwrap_or(0.0);
    major * 10.0 + minor
}

/// Interpreter parameters, keyed by [`named_key`] or the digits of a numbered
/// parameter: "#<level>" is "level", "#1" is "1". An unset parameter reads as
/// 0, as in the interpreter.
type Params = BTreeMap<String, f64>;

/// A parameter map with the read-only status names in it, so a block that reads
/// `#<_x>` finds a value without [`Live::update`] having to allocate.
fn new_params() -> Params {
    let mut params = Params::new();
    for name in LIVE_NAMES {
        params.insert((*name).to_string(), 0.0);
    }
    for (number, _) in NUMBERED_LIVE {
        params.insert((*number).to_string(), 0.0);
    }
    // The straight-probe results (#5061-#5069) and its success flag (#5070).
    for number in 5061..=5070 {
        params.insert(number.to_string(), 0.0);
    }
    params
}

/// The map key a named parameter is stored under. LinuxCNC keeps named
/// parameters in a `std::map` compared with `strcasecmp`, so `#<RADIUS>` and
/// `#<radius>` are one parameter; a case-sensitive key here reads `CIRCLE_XY`'s
/// `I[-#<radius>]` as `I0` and puts a zero-radius arc on the error list.
fn named_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

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
                let name = named_key(&String::from_utf8_lossy(&self.raw[start..self.pos]));
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
            TokenKind::Axis | TokenKind::Word | TokenKind::FeedSpeed | TokenKind::Tool
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
        /// `endsub [expr]`: the value the caller reads as `#<_value>`.
        expr: Option<String>,
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
        /// `return [expr]`: the value the caller reads as `#<_value>`.
        expr: Option<String>,
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

impl Preview {
    /// The loops a segment sits inside, outermost first, each with the pass it
    /// is on. Empty for a block outside every loop.
    pub fn segment_stack(&self, segment: usize) -> &[(u32, u32)] {
        self.segment_loop
            .get(segment)
            .and_then(|id| self.loop_stacks.get(*id as usize))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

struct PlannedPreview {
    blocks: Vec<PlannedBlock>,
    error: Option<SyntaxIssue>,
    incomplete: Option<String>,
    subroutines: Vec<SubroutineBody>,
    loops: Vec<LoopTrace>,
    loop_stacks: Vec<Vec<(u32, u32)>>,
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
        return Some(named_key(&rest[..end]));
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

/// The expression `return`/`endsub` carries, when it carries one. The caller
/// reads the result as `#<_value>`.
fn return_value(payload: &str) -> Option<String> {
    let payload = payload.trim();
    (!payload.is_empty()).then(|| payload.to_string())
}

fn o_word_statement(line: usize, label: &str, rest: &str) -> Stmt {
    let (keyword, payload) = keyword_and_payload(rest);
    match keyword.as_str() {
        "sub" => Stmt::SubStart,
        "endsub" => Stmt::SubEnd {
            line,
            expr: return_value(&payload),
        },
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
        "return" => Stmt::Return {
            line,
            expr: return_value(&payload),
        },
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
        | Stmt::SubEnd { line, .. }
        | Stmt::Call { line, .. }
        | Stmt::If { line, .. }
        | Stmt::ElseIf { line, .. }
        | Stmt::While { line, .. }
        | Stmt::DoWhile { line, .. }
        | Stmt::Repeat { line, .. }
        | Stmt::Break { line, .. }
        | Stmt::Continue { line, .. }
        | Stmt::Goto { line, .. }
        | Stmt::Return { line, .. }
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
fn plan_preview(text: &str, subs: &Subroutines, live: &mut Live, tools: &Tools) -> PlannedPreview {
    let (mut statements, mut labels, error) = parse_statements(text);
    let bodies = import_subroutines(&mut statements, &mut labels, subs);
    plan_statements(statements, labels, bodies, error, live, tools)
}

fn plan_statements(
    mut statements: Vec<Stmt>,
    labels: Vec<Option<String>>,
    bodies: Vec<SubroutineBody>,
    mut error: Option<SyntaxIssue>,
    live: &mut Live,
    tools: &Tools,
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
        // A counter is a test parameter the body changes; a limit is one it only
        // reads. That is what the interpreter's loops look like, and it is what
        // lets the status line show the walk without repeating the bound.
        let counters: Vec<String> = variables
            .iter()
            .filter(|key| {
                statements[index..after[index].min(statements.len())]
                    .iter()
                    .any(|statement| {
                        matches!(
                            statement,
                            Stmt::Assign { name, .. } if name == *key
                        )
                    })
            })
            .cloned()
            .collect();
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
            counters,
            iterations: Vec::new(),
            live,
            repeated_path: None,
        });
    }

    let mut params = new_params();
    let mut blocks: Vec<PlannedBlock> = Vec::new();
    // The loops whose body the planner is inside, outermost first. A block's
    // tag is the whole nest, so the readout can stack every enclosing loop.
    let mut open: Vec<usize> = Vec::new();
    // Distinct nests, index 0 being "inside no loop".
    let mut stacks: Vec<Vec<(u32, u32)>> = vec![Vec::new()];
    macro_rules! current_pass {
        () => {{
            let nest: Vec<(u32, u32)> = open
                .iter()
                .map(|&trace| {
                    (
                        trace as u32,
                        traces[trace].iterations.len().saturating_sub(1) as u32,
                    )
                })
                .collect();
            match stacks.iter().position(|found| *found == nest) {
                Some(index) => index as u32,
                None => {
                    stacks.push(nest);
                    (stacks.len() - 1) as u32
                }
            }
        }};
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
        // What the block about to run reads from `#<_name>`: where the blocks
        // before it left the tool, and how deep in a call it is.
        if let Some(line) = statement_line(&statements[pc]) {
            live.line = line;
        }
        live.call_level = calls.len();
        live.update(&mut params);
        match &statements[pc] {
            Stmt::Block { line, raw, tokens } => {
                let mut program_end = false;
                match resolve_words(tokens, raw, &params) {
                    Ok(words) => {
                        // `M2` and `M30` end the program: the interpreter reads
                        // no further line, so prose or a comment block after the
                        // end mark is not G-code.
                        program_end = words.iter().any(is_program_end);
                        // The status parameters follow every block in the order
                        // the interpreter runs them, so a later block reading
                        // `#<_x>` sees where this one left the tool.
                        live.observe(&words);
                        live.advance(&words);
                        if let Some(tool) = live.selected {
                            live.selected_pocket = tools.pocket(tool);
                        }
                        // A G70/G71/G72 cycle does not move where its X and Z
                        // words say: those are the cycle's limits. The contour it
                        // cuts is the one the subroutine named by Q draws, so run
                        // that body once and draw it -- that is the surface the
                        // cycle leaves behind. The stepped roughing passes between
                        // are not drawn, and the preview says so.
                        if let Some(cycle) = lathe_profile_cycle(&words) {
                            let profile = cycle
                                .q
                                .and_then(|label| subroutines.get(&label.to_string()).copied());
                            match profile {
                                Some(start) if calls.len() < PREVIEW_DEPTH_LIMIT => {
                                    if cycle.roughing && incomplete.is_none() {
                                        incomplete = Some(format!(
                                            "line {}: the {} roughing passes are not drawn, only \
                                             its profile",
                                            line, cycle.code
                                        ));
                                    }
                                    // The profile body runs as a call with no
                                    // arguments: it sees the program's globals and
                                    // none of its locals, as a subroutine does.
                                    let frame: Params = params
                                        .iter()
                                        .filter(|(key, _)| global_key(key))
                                        .map(|(key, value)| (key.clone(), *value))
                                        .collect();
                                    calls.push((pc, std::mem::replace(&mut params, frame)));
                                    pc = start + 1;
                                    continue;
                                }
                                Some(_) => {
                                    if incomplete.is_none() {
                                        incomplete = Some(format!(
                                            "subroutine call depth limit {} reached at line {}",
                                            PREVIEW_DEPTH_LIMIT, line
                                        ));
                                    }
                                    pc += 1;
                                    continue;
                                }
                                None => {
                                    if incomplete.is_none() {
                                        incomplete = Some(format!(
                                            "line {}: the {} profile subroutine is not in this \
                                             program",
                                            line, cycle.code
                                        ));
                                    }
                                    pc += 1;
                                    continue;
                                }
                            }
                        }
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
            Stmt::SubEnd { line, expr } | Stmt::Return { line, expr } => match calls.pop() {
                Some((return_to, mut saved)) => {
                    // What the block hands back, which the caller reads as
                    // `#<_value>`; `#<_value_returned>` says whether it was
                    // written or defaulted to zero. An expression the preview
                    // cannot follow leaves the value at zero, as a bare return
                    // does, and says so.
                    match expr {
                        Some(expr) => match Expression::evaluate(expr, &params) {
                            Ok(value) => {
                                live.value = value;
                                live.value_returned = true;
                            }
                            Err(_) => {
                                live.value = 0.0;
                                live.value_returned = false;
                                if incomplete.is_none() {
                                    incomplete = Some(format!(
                                        "line {}: could not evaluate the value returned to the caller",
                                        line
                                    ));
                                }
                            }
                        },
                        None => {
                            live.value = 0.0;
                            live.value_returned = false;
                        }
                    }
                    // The callee's locals vanish with its frame; what it wrote to
                    // a global parameter is the whole point of a global one.
                    for (key, value) in params.iter() {
                        if global_key(key) {
                            saved.insert(key.clone(), *value);
                        }
                    }
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
                // `#<_value_returned>` is about the last call, so a new one
                // clears it before its callee runs.
                live.value_returned = false;
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
                // Lexical scope: the callee sees the globals and not the
                // caller's locals, and its arguments arrive as #1..#n.
                let mut frame: Params = params
                    .iter()
                    .filter(|(key, _)| global_key(key))
                    .map(|(key, value)| (key.clone(), *value))
                    .collect();
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
                    // The test runs once per pass, and the loop stays open
                    // between them; only a loop not yet open is entered.
                    if let Some(trace) = trace_of[pc].filter(|trace| !open.contains(trace)) {
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
                if let Some(trace) = trace_of[pc].filter(|trace| !open.contains(trace)) {
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
                    if let Some(trace) = trace_of[pc].filter(|trace| !open.contains(trace)) {
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
        loop_stacks: stacks,
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
    parse_preview_full(
        text,
        axis_names,
        offset,
        current_position,
        subs,
        &Tools::default(),
    )
}

/// The full preview: the same as [`parse_preview_with`], plus the tool radii
/// `G41`/`G42` need to draw the tool centre instead of the programmed contour.
pub fn parse_preview_full(
    text: &str,
    axis_names: &[String],
    offset: Option<&AxisMap<f64>>,
    current_position: [f64; 3],
    subs: &Subroutines,
    tools: &Tools,
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
    // The work offset the path is drawn against. It starts as the caller's and
    // a G10 L2/L20 block changes it for the rest of the program.
    let mut offsets: AxisMap<f64> = offset.cloned().unwrap_or_default();

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
    // Cutter compensation. `comp` is the state a move is drawn under; a
    // G41/G42 in the block itself lands in `comp_request` and applies to that
    // block's move, which is the lead-in. `comp_exit` is where the last
    // compensated move left the tool centre, so the next one can join onto it.
    let mut comp: Option<Comp> = None;
    let mut comp_request: Option<Comp> = None;
    let mut comp_exit: Option<([f64; 2], [f64; 2])> = None;
    // A G41/G42 that named a tool the table has no radius for: the move is
    // drawn as programmed and the preview says why it had to guess.
    let mut comp_radius_unknown: Option<(usize, u16)> = None;
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
        loop_stacks,
    } = plan_preview(
        text,
        subs,
        &mut Live::at(current_position, offsets.clone()),
        tools,
    );
    // The planner's own verdict on what it could not follow; the drawing pass
    // below adds to it when it meets a path it cannot compensate.
    let mut incomplete = incomplete;
    preview.subroutines = subroutines;
    preview.loops = loops;
    preview.loop_stacks = loop_stacks;
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
        // Set by a G10 in this block: the block writes an offset, it never moves.
        let mut g10 = false;
        // Set by a G38.2/G38.3/G38.4/G38.5 in this block. A straight probe is
        // not modal: the block moves at feed rate along a straight line and the
        // motion mode it was in survives it.
        let mut probe: Option<f64> = None;
        // Set by a G53 in this block: its axis words name machine positions, so
        // no work offset applies to them. Like G38.n it is a one-block word.
        let mut machine_coordinates = false;

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
                    } else if value == 10.0 {
                        g10 = true;
                    } else if value == 40.0 {
                        // G40 ends compensation; the block's own move, if any,
                        // is the lead-out and runs on the programmed path.
                        comp = None;
                        comp_request = None;
                        comp_exit = None;
                    } else if value == 41.0 || value == 42.0 || value == 41.1 || value == 42.1 {
                        let left = value == 41.0 || value == 41.1;
                        // G41.1/G42.1 carry the radius in D; G41/G42 carry a
                        // tool number there and take its radius from the table.
                        let dynamic = value == 41.1 || value == 42.1;
                        let d = word_int(&tokens, 'd');
                        let (radius, unknown) = if dynamic {
                            // The radius itself, which need not be a whole number.
                            (word_value(&tokens, 'd').unwrap_or(0.0), None)
                        } else {
                            match d {
                                Some(tool) if tool > 0 => match tools.radius(tool as u16) {
                                    Some(radius) => (radius, None),
                                    None => (0.0, Some(tool as u16)),
                                },
                                // No D word: the tool in the spindle, and 0
                                // when that tool has no radius on file.
                                _ => match tool.and_then(|tool| tools.radius(tool)) {
                                    Some(radius) => (radius, None),
                                    None => (0.0, tool),
                                },
                            }
                        };
                        if let Some(tool) = unknown {
                            if !tools.is_empty() {
                                comp_radius_unknown = Some((line_no, tool));
                            }
                        }
                        comp_request = Some(Comp { left, radius });
                    } else if value == 38.2 || value == 38.3 || value == 38.4 || value == 38.5 {
                        probe = Some(value);
                    } else if value == 53.0 {
                        machine_coordinates = true;
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

        // G10 writes a coordinate system, it does not move. Drawing its axis
        // words as a move put a rapid to the offset value on the path, so every
        // probe macro that zeroed a WCS showed a spike to the origin.
        if g10 {
            let mode = word_int(&tokens, 'l').unwrap_or(0);
            // P0 means the system that is active right now.
            let index = word_int(&tokens, 'p').map(|index| index as u8);
            let applies = match index {
                Some(index) => wcs_index(&wcs) == Some(index),
                None => true,
            };
            let mut row = offsets.clone();
            for (letter, axis, axis_index) in [('x', "X", 0usize), ('y', "Y", 1), ('z', "Z", 2)] {
                let Some(value) = word_value(&tokens, letter) else {
                    continue;
                };
                let value = if axis_index == 0 {
                    dia(mm(value))
                } else {
                    mm(value)
                };
                let value = match mode {
                    // L2: the axis words are the offset itself.
                    2 if absolute => value,
                    2 => offset_of(&row, axis) + value,
                    // L20: the axis words are what the current position should
                    // read, so the offset is the gap between the two.
                    20 => position[axis_index] - value,
                    // L1/L10/L11 write the tool table, which the preview does
                    // not read; the block still moves nothing.
                    _ => continue,
                };
                row.insert(axis, value);
            }
            if applies && (mode == 2 || mode == 20) {
                offsets = row;
            }
            continue;
        }

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

        // Polar coordinates: `@` is a radius and `^` an angle in degrees. The
        // interpreter reads them in the G17 plane in place of X and Y, so a
        // block can move without naming an axis at all.
        let polar_radius = word_value(&tokens, '@');
        let polar_theta = word_value(&tokens, '^');
        let has_polar_word = polar_radius.is_some() || polar_theta.is_some();
        let has_xy_word = tokens.iter().any(|token| {
            token.kind == TokenKind::Axis
                && token
                    .text
                    .chars()
                    .next()
                    .is_some_and(|ch| "XYxy".contains(ch))
        });

        // G76 threads in the ZX plane: the tool stands on the drive line, `I`
        // places the thread peak against it and `K` is the full depth, so the
        // root the cycle leaves is `I + K` away. `Z` is where the thread ends and
        // `E`/`L` taper the entry, the exit or both. The passes between the first
        // cut and the root are the same line cut deeper, so they are reported
        // rather than drawn as a solid block of material removed.
        if tokens.iter().any(|token| {
            token.kind == TokenKind::GCode && parse_number(&token.text[1..]) == Some(76.0)
        }) {
            if plane != Plane::Xz {
                preview.error = Some(SyntaxIssue {
                    line: line_no,
                    message: "G76 threads in the ZX plane: it needs G18".into(),
                });
                break;
            }
            let Some(end_word) = word_value(&tokens, 'z') else {
                if incomplete.is_none() {
                    incomplete = Some(format!(
                        "line {line_no}: G76 needs a Z word for where the thread ends"
                    ));
                }
                continue;
            };
            // A thread offset is a radius on the machine axis: in G7 diameter
            // mode the words are diameters, so they count half.
            let depth = if lathe_diameter { 0.5 } else { 1.0 };
            let peak = mm(word_value(&tokens, 'i').unwrap_or(0.0));
            let full = mm(word_value(&tokens, 'k').unwrap_or(0.0));
            let drive_x = position[0];
            let start_z = position[2];
            let root_x = drive_x + (peak + full) * depth;
            let end_z = if absolute {
                mm(end_word) + offset_of(&offsets, "Z")
            } else {
                start_z + mm(end_word)
            };
            let direction = if end_z >= start_z { 1.0 } else { -1.0 };
            let room = (end_z - start_z).abs();
            let taper = mm(word_value(&tokens, 'e').unwrap_or(0.0)).abs();
            let ends = word_int(&tokens, 'l').unwrap_or(0);
            let entry = if ends == 1 || ends == 3 {
                taper.min(room)
            } else {
                0.0
            };
            let exit = if ends == 2 || ends == 3 {
                taper.min(room - entry)
            } else {
                0.0
            };
            let y = position[1];
            let mut at = [drive_x, y, start_z];
            let cut = |preview: &mut Preview, from: [f64; 3], to: [f64; 3], kind| {
                if (to[2] - from[2]).abs() > 1e-12 || (to[0] - from[0]).abs() > 1e-12 {
                    push_cycle_segment(preview, from, to, kind, line_no, tool, pass);
                }
            };
            if entry > 0.0 {
                let to = [root_x, y, start_z + direction * entry];
                cut(&mut preview, at, to, SegmentKind::Feed);
                at = to;
            } else {
                // No entry taper: the tool rapids in to the cut depth, then
                // synchronises and starts cutting.
                let to = [root_x, y, start_z];
                cut(&mut preview, at, to, SegmentKind::Rapid);
                at = to;
            }
            let root_end = [root_x, y, end_z - direction * exit];
            cut(&mut preview, at, root_end, SegmentKind::Feed);
            at = root_end;
            // The tool finishes at the end of the drive line, tapering out if `L`
            // asked for it and rapiding clear if it did not.
            let out = [drive_x, y, end_z];
            cut(
                &mut preview,
                at,
                out,
                if exit > 0.0 {
                    SegmentKind::Feed
                } else {
                    SegmentKind::Rapid
                },
            );
            position = out;
            saw_motion = true;
            preview
                .line_bounds
                .insert(line_no, (line_start_segment, preview.segments.len()));
            if incomplete.is_none() {
                incomplete = Some(format!(
                    "line {line_no}: the threading passes between the first cut and the root are \
                     not drawn"
                ));
            }
            continue;
        }

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
                    mm(value) + offset_of(&offsets, "Z")
                } else {
                    position[2] + mm(value)
                };
            }
            if let Some(value) = word_value(&tokens, 'z') {
                cycle.depth = if absolute {
                    mm(value) + offset_of(&offsets, "Z")
                } else {
                    cycle.r + mm(value)
                };
            }
            if has_polar_word && has_xy_word {
                preview.error = Some(SyntaxIssue {
                    line: line_no,
                    message: "polar coordinates cannot be combined with X or Y words".into(),
                });
                break;
            }
            if has_polar_word && plane != Plane::Xy {
                preview.error = Some(SyntaxIssue {
                    line: line_no,
                    message: "polar coordinates need the G17 plane".into(),
                });
                break;
            }
            // L repeats the whole cycle. With incremental words each pass steps
            // once more, which is how `hole-circle.ngc` bolts a ring of holes in
            // one block; in absolute mode every pass names the same hole.
            let repeats = word_int(&tokens, 'l').unwrap_or(1).max(1);
            let mut holes: Vec<[f64; 3]> = Vec::new();
            // The first block of a cycle drills even without a hole word (Z
            // only); a modal repeat needs X/Y/polar to name a new hole.
            if (requested_cycle.is_some() && has_axis_word) || has_xy_word || has_polar_word {
                if has_polar_word {
                    // The interpreter resolves polar words in program
                    // coordinates, so measure from the program origin and add
                    // the work offset afterwards.
                    let mut at = [
                        position[0] - offset_of(&offsets, "X"),
                        position[1] - offset_of(&offsets, "Y"),
                    ];
                    let mut failed = false;
                    for _ in 0..repeats {
                        match polar_target(at, polar_radius.map(mm), polar_theta, absolute) {
                            Ok(next) => at = next,
                            Err(message) => {
                                preview.error = Some(SyntaxIssue {
                                    line: line_no,
                                    message: message.into(),
                                });
                                failed = true;
                                break;
                            }
                        }
                        holes.push([
                            at[0] + offset_of(&offsets, "X"),
                            at[1] + offset_of(&offsets, "Y"),
                            position[2],
                        ]);
                    }
                    if failed {
                        break;
                    }
                } else {
                    let mut hole = position;
                    for _ in 0..repeats {
                        for (letter, axis) in [('x', 0usize), ('y', 1)] {
                            let Some(value) = word_value(&tokens, letter) else {
                                continue;
                            };
                            let value = if axis == 0 { dia(mm(value)) } else { mm(value) };
                            hole[axis] = if absolute {
                                value
                                    + offset_of(&offsets, &letter.to_ascii_uppercase().to_string())
                            } else {
                                hole[axis] + value
                            };
                        }
                        holes.push(hole);
                    }
                }
            }
            if !holes.is_empty() {
                let depth = cycle.depth;
                let r = cycle.r;
                let initial = cycle.initial_z;
                for hole in holes {
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
                    if (hole[0] - position[0]).abs() > 1e-9 || (hole[1] - position[1]).abs() > 1e-9
                    {
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
                }
                saw_motion = true;
                preview
                    .line_bounds
                    .insert(line_no, (line_start_segment, preview.segments.len()));
            }
            // A G81/G82/G83/G73 line with no hole words only selects the mode.
            continue;
        }

        // A rotary axis is not drawn. No shipped profile has one, and drawing
        // it would mean rotating the path with the table rather than moving the
        // tool, which is a different job from a three-axis toolpath. The block's
        // X/Y/Z words are still drawn; only the rotary part is missing, and the
        // preview says so instead of dropping it.
        for (letter, axis) in [('a', "A"), ('b', "B"), ('c', "C")] {
            if word_value(&tokens, letter).is_none() {
                continue;
            }
            if incomplete.is_none() {
                incomplete = Some(format!(
                    "line {}: the {axis} axis move is not drawn: the preview models no \
                     rotary axis",
                    line_no
                ));
            }
            break;
        }

        // A probe block moves whether or not a motion mode was in force: the
        // interpreter allows `G38.2` after a `G80`. Its own rules are the ones
        // the preview can check, so it checks them here and reports them rather
        // than drawing a move the machine would refuse.
        if let Some(kind) = probe {
            let code = format_g(kind);
            let refusal = if !has_axis_word {
                Some(format!("{code} needs an axis word to probe towards"))
            } else if comp.is_some() {
                Some(format!("{code} cannot run while cutter compensation is on"))
            } else {
                None
            };
            if let Some(message) = refusal {
                preview.error = Some(SyntaxIssue {
                    line: line_no,
                    message,
                });
                break;
            }
        }

        let is_motion =
            (motion == 0.0 || motion == 1.0 || motion == 2.0 || motion == 3.0 || probe.is_some())
                && (has_axis_word || has_polar_word);
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
                if machine_coordinates {
                    value
                } else {
                    value + offset_of(&offsets, name)
                }
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
                target[axis] = if !absolute {
                    position[axis] + value
                } else if machine_coordinates {
                    value
                } else {
                    value + offset_of(&offsets, &letter.to_ascii_uppercase().to_string())
                };
            }
        }

        if has_polar_word && has_xy_word {
            preview.error = Some(SyntaxIssue {
                line: line_no,
                message: "polar coordinates cannot be combined with X or Y words".into(),
            });
            break;
        }
        if has_polar_word && plane != Plane::Xy {
            preview.error = Some(SyntaxIssue {
                line: line_no,
                message: "polar coordinates need the G17 plane".into(),
            });
            break;
        }
        if has_polar_word {
            // The interpreter resolves polar words in program coordinates, so
            // the radius and angle are measured from the program origin and the
            // work offset is added afterwards.
            let program = [
                position[0] - offset_of(&offsets, "X"),
                position[1] - offset_of(&offsets, "Y"),
            ];
            match polar_target(program, polar_radius.map(mm), polar_theta, absolute) {
                Ok([x, y]) => {
                    target[0] = x + offset_of(&offsets, "X");
                    target[1] = y + offset_of(&offsets, "Y");
                }
                Err(message) => {
                    preview.error = Some(SyntaxIssue {
                        line: line_no,
                        message: message.into(),
                    });
                    break;
                }
            }
        }

        saw_motion = true;

        // A G41/G42 in this block applies to its move, which is the lead-in.
        if let Some(requested) = comp_request.take() {
            comp = Some(requested);
            comp_exit = None;
        }

        let start = position;
        let mut segment = PathSegment {
            start,
            end: target,
            kind: if probe.is_some() {
                SegmentKind::Probe
            } else if motion == 0.0 {
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
        // A probe is a straight line, so a G2/G3 that is still modal cannot turn
        // it into an arc.
        if probe.is_none() && (motion == 2.0 || motion == 3.0) {
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

        // Cutter compensation: the programmed line is the contour, so the tool
        // centre runs alongside it. The parser keeps tracking the programmed
        // point; only the drawn segment moves out to the tool centre.
        if let Some(active) = comp {
            let joining = comp_exit.is_some();
            match compensate(
                active,
                plane,
                start,
                target,
                segment.arc.as_ref(),
                comp_exit,
            ) {
                Ok(compensated) => {
                    segment.start = compensated.start;
                    segment.end = compensated.end;
                    segment.arc = compensated.arc;
                    // The offset lines meet at a point that belongs to both
                    // moves, so the previous move is stretched to it: without
                    // that the drawn contour jumps at every corner.
                    if joining {
                        if let (Some(last), Some((u, v))) =
                            (preview.segments.last_mut(), comp_axes(plane))
                        {
                            let closed = (last.start[u] - last.end[u]).abs() < 1e-9
                                && (last.start[v] - last.end[v]).abs() < 1e-9;
                            if !closed {
                                last.end[u] = compensated.start[u];
                                last.end[v] = compensated.start[v];
                                if let Some(spec) = last.arc.as_mut() {
                                    spec.end_angle = (last.end[v] - spec.center[v])
                                        .atan2(last.end[u] - spec.center[u]);
                                }
                            }
                        }
                    }
                    comp_exit = Some(compensated.junction);
                }
                Err(reason) => {
                    // A move the tool cannot follow: report it and draw that one
                    // move on the programmed line, then re-engage at the next.
                    // Dropping compensation for the rest of the program would
                    // turn one reported corner into a silently uncompensated
                    // contour.
                    if incomplete.is_none() {
                        incomplete = Some(format!("line {line_no}: {reason}"));
                    }
                    comp_exit = None;
                }
            }
            if let Some((at, tool)) = comp_radius_unknown.take() {
                if incomplete.is_none() {
                    incomplete = Some(format!(
                        "line {at}: the tool table has no radius for tool {tool}, so the \
                         compensated path is drawn at the programmed line"
                    ));
                }
            }
        }

        preview.segments.push(segment);
        preview.segment_loop.push(pass);
        preview
            .line_bounds
            .insert(line_no, (line_start_segment, preview.segments.len()));
        position = target;
    }

    // A loop whose passes draw the same path needs a counter the segment tags
    // cannot give it. Walk the drawn segments once and record one pass's seam,
    // heading and size for each such loop.
    attach_loop_paths(&mut preview);

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
    // Destination points plus what the arcs reach between them: the first
    // segment starts at the tool's position when the program was parsed, and
    // including it made the program bounds (and the stock) stretch out to
    // wherever the tool happened to be, then shrink as it moved in. The
    // approach line is still drawn; it just does not define the part's
    // extents.
    preview.bounds = if saw_motion {
        Bounds3::from_segments(&preview.segments)
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
            // Cutter compensation is modal and changes what every following
            // move does, so the readout names it while it is on.
            if let Some(active) = comp {
                codes.push(if active.left { "G41" } else { "G42" }.to_string());
            }
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

/// Chain hash for one pass's segment shapes. A pass repeats when its hash and
/// segment count match the reference pass's; a 64-bit collision is not a
/// practical concern for deciding whether to count a seam.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x100_0000_01b3;

fn mix_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash ^= byte as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn mix_f64(hash: &mut u64, value: f64) {
    mix_u64(hash, value.to_bits());
}

/// One pass of a loop, accumulated in the order the preview runs it.
#[derive(Clone, Debug)]
struct PassShape {
    hash: u64,
    count: usize,
    seam: [f64; 3],
    heading: Option<[f64; 3]>,
    length: f64,
    min: [f64; 3],
    max: [f64; 3],
    reach: f64,
}

impl Default for PassShape {
    fn default() -> Self {
        Self {
            hash: FNV_OFFSET,
            count: 0,
            seam: [0.0; 3],
            heading: None,
            length: 0.0,
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
            reach: 0.0,
        }
    }
}

impl PassShape {
    fn add(&mut self, segment: &PathSegment) {
        mix_f64(&mut self.hash, segment.start[0]);
        mix_f64(&mut self.hash, segment.start[1]);
        mix_f64(&mut self.hash, segment.start[2]);
        mix_f64(&mut self.hash, segment.end[0]);
        mix_f64(&mut self.hash, segment.end[1]);
        mix_f64(&mut self.hash, segment.end[2]);
        mix_u64(&mut self.hash, segment.kind as u64);
        if let Some(arc) = &segment.arc {
            mix_f64(&mut self.hash, arc.center[0]);
            mix_f64(&mut self.hash, arc.center[1]);
            mix_f64(&mut self.hash, arc.center[2]);
            mix_f64(&mut self.hash, arc.radius);
            mix_f64(&mut self.hash, arc.start_angle);
            mix_f64(&mut self.hash, arc.end_angle);
            mix_u64(&mut self.hash, arc.clockwise as u64);
            let (u, v) = arc.plane.axes();
            mix_u64(&mut self.hash, u as u64);
            mix_u64(&mut self.hash, v as u64);
        } else {
            mix_u64(&mut self.hash, 0);
        }
        self.count += 1;
        self.seam = segment.end;
        if self.heading.is_none() {
            self.heading = segment_heading(segment);
        }
        self.length += segment_length(segment);
        let mut include = |point: [f64; 3]| {
            for (axis, coordinate) in point.iter().enumerate() {
                self.min[axis] = self.min[axis].min(*coordinate);
                self.max[axis] = self.max[axis].max(*coordinate);
            }
        };
        include(segment.start);
        include(segment.end);
        if let Some(arc) = &segment.arc {
            for point in arc.extent_points() {
                include(point);
            }
        }
    }

    /// The farthest the pass reaches from its seam, as a scale for the gate.
    /// The bounds are used rather than sampled points: an arc's ends can be the
    /// same point while its sweep fills the whole circle.
    fn finish(&mut self) {
        if self.count == 0 || !self.min[0].is_finite() || !self.max[0].is_finite() {
            return;
        }
        let seam = self.seam;
        for corner in 0..8 {
            let mut point = [0.0; 3];
            for (axis, coordinate) in point.iter_mut().enumerate() {
                *coordinate = if corner & (1 << axis) == 0 {
                    self.min[axis]
                } else {
                    self.max[axis]
                };
            }
            let distance = ((point[0] - seam[0]).powi(2)
                + (point[1] - seam[1]).powi(2)
                + (point[2] - seam[2]).powi(2))
            .sqrt();
            self.reach = self.reach.max(distance);
        }
    }

    fn same(&self, other: &Self) -> bool {
        self.hash == other.hash && self.count == other.count
    }
}

/// The shape of one loop across the passes the preview ran.
#[derive(Default)]
struct LoopShape {
    pass: u32,
    started: bool,
    current: PassShape,
    first: Option<PassShape>,
    reference: Option<PassShape>,
    repeated: bool,
}

impl LoopShape {
    fn observe(&mut self, pass: u32, segment: &PathSegment) {
        if self.started && pass != self.pass {
            self.finish();
        }
        if !self.started {
            self.started = true;
            self.pass = pass;
            self.current = PassShape::default();
        }
        self.current.add(segment);
    }

    fn finish(&mut self) {
        if !self.started || self.current.count == 0 {
            self.started = false;
            return;
        }
        self.current.finish();
        let run = std::mem::take(&mut self.current);
        self.started = false;
        if self.pass == 0 {
            self.first = Some(run);
        } else if self.reference.is_none() {
            self.reference = Some(run);
            self.repeated = true;
        } else if let Some(reference) = &self.reference {
            if !reference.same(&run) {
                self.repeated = false;
            }
        }
    }
}

/// Unit heading at a segment's start. An arc uses its tangent because a full
/// circle's start and end are the same point, so its chord is zero.
fn segment_heading(segment: &PathSegment) -> Option<[f64; 3]> {
    if let Some(arc) = &segment.arc {
        let (u, v) = arc.plane.axes();
        let sign = if arc.clockwise { -1.0 } else { 1.0 };
        let mut heading = [0.0; 3];
        heading[u] = -arc.start_angle.sin() * sign;
        heading[v] = arc.start_angle.cos() * sign;
        return Some(heading);
    }
    let delta = [
        segment.end[0] - segment.start[0],
        segment.end[1] - segment.start[1],
        segment.end[2] - segment.start[2],
    ];
    let length = (delta[0].powi(2) + delta[1].powi(2) + delta[2].powi(2)).sqrt();
    (length > 1e-9).then(|| [delta[0] / length, delta[1] / length, delta[2] / length])
}

fn segment_length(segment: &PathSegment) -> f64 {
    let Some(arc) = &segment.arc else {
        return ((segment.end[0] - segment.start[0]).powi(2)
            + (segment.end[1] - segment.start[1]).powi(2)
            + (segment.end[2] - segment.start[2]).powi(2))
        .sqrt();
    };
    const TAU: f64 = std::f64::consts::TAU;
    let travel = if arc.clockwise {
        arc.start_angle - arc.end_angle
    } else {
        arc.end_angle - arc.start_angle
    };
    // A full turn is stored as a whole TAU, whose remainder is zero, so it is
    // read before the sweep wraps; this is the same sweep `extent_points` uses.
    let sweep = if travel >= TAU - 1e-9 {
        TAU
    } else {
        travel.rem_euclid(TAU)
    };
    arc.radius * sweep
}

/// Fill in [LoopTrace::repeated_path] for the loops whose passes draw one path.
///
/// The body's line is the same on every pass, so when the paths repeat, the
/// tool's position has no way to name the pass. The seam - the point every pass
/// returns to - and the heading out of it give the UI something to count.
fn attach_loop_paths(preview: &mut Preview) {
    if preview.loops.is_empty() || preview.segments.is_empty() {
        return;
    }
    let mut shapes: Vec<LoopShape> = (0..preview.loops.len())
        .map(|_| LoopShape::default())
        .collect();
    for (index, segment) in preview.segments.iter().enumerate() {
        let Some(stack) = preview
            .segment_loop
            .get(index)
            .and_then(|id| preview.loop_stacks.get(*id as usize))
        else {
            continue;
        };
        for &(trace, pass) in stack {
            if let Some(shape) = shapes.get_mut(trace as usize) {
                shape.observe(pass, segment);
            }
        }
    }
    for shape in &mut shapes {
        shape.finish();
    }
    for (trace, shape) in preview.loops.iter_mut().zip(shapes.iter()) {
        if !shape.repeated {
            continue;
        }
        let Some(reference) = &shape.reference else {
            continue;
        };
        let Some(heading) = reference.heading else {
            continue;
        };
        if reference.length <= 1e-9 || reference.reach <= 1e-9 {
            continue;
        }
        let seam = shape
            .first
            .as_ref()
            .map_or(reference.seam, |pass| pass.seam);
        trace.repeated_path = Some(LoopPath {
            seam,
            heading,
            length: reference.length,
            reach: reference.reach,
        });
    }
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
    fn a_full_circle_bounds_both_axes() {
        // CIRCLE_XY.NGC's tuning move: every segment ends on X10 Y0, so the
        // ends alone measure the path 10 mm wide in X and nothing in Y. The
        // drawn path is a 10 mm circle in both.
        let text = "G21 G90 G17\nG0 X10 Y0\nG3 X10 Y0 I-5 J0 F3000\n";
        let preview = parse_preview(text, &["X".into(), "Y".into(), "Z".into()], None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let bounds = preview.bounds.expect("bounds");
        let [width, height, _] = bounds.size();
        assert!((width - 10.0).abs() < 1e-6, "{bounds:?}");
        assert!((height - 10.0).abs() < 1e-6, "{bounds:?}");
        assert!((bounds.min[1] + 5.0).abs() < 1e-6, "{bounds:?}");
        assert!((bounds.max[1] - 5.0).abs() < 1e-6, "{bounds:?}");
    }

    #[test]
    fn an_arc_bounds_only_the_quarter_turns_it_passes() {
        // A quarter turn in the first quadrant reaches X10 Y10 and no further.
        // The bounds must not grow to the circle the arc is part of, or every
        // rounded corner would claim its radius in both directions.
        let text = "G21 G90 G17\nG0 X10 Y0\nG3 X0 Y10 I-10 J0 F300\n";
        let preview = parse_preview(text, &["X".into(), "Y".into()], None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let bounds = preview.bounds.expect("bounds");
        for (axis, expected) in [(0, (0.0, 10.0)), (1, (0.0, 10.0))] {
            assert!((bounds.min[axis] - expected.0).abs() < 1e-6, "{bounds:?}");
            assert!((bounds.max[axis] - expected.1).abs() < 1e-6, "{bounds:?}");
        }
    }

    #[test]
    fn the_circle_tuning_move_measures_round() {
        // The whole program, loop and all: the Y extent of a circle drawn by
        // repeated full turns is the diameter, not zero.
        let text = "G21 G90 G17 G40 G49 G80\n#1 = 1\n#<iterations> = 3\n#<RADIUS> = 5\n\
                    G0 X10 Y0\nO100 WHILE [#1 LE #<iterations>]\n\
                    G3 X10 Y0 I[-#<radius>] J0 F3000\n#1 = [ #1 + 1]\n\
                    O100 ENDWHILE\nG0 X0 Y0\nM2\n";
        let preview = parse_preview(text, &["X".into(), "Y".into()], None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.arcs, 3, "the loop draws one circle per pass");
        let [width, height, _] = preview.bounds.expect("bounds").size();
        assert!((width - 10.0).abs() < 1e-6, "width {width}");
        assert!((height - 10.0).abs() < 1e-6, "height {height}");
    }

    #[test]
    fn polar_words_keep_their_letter_and_value() {
        assert_eq!(
            kinds("g91 g1 @-.0025 ^4.5"),
            vec![
                (TokenKind::GCode, "g91".into()),
                (TokenKind::GCode, "g1".into()),
                (TokenKind::Axis, "@-.0025".into()),
                (TokenKind::Axis, "^4.5".into()),
            ]
        );
    }

    #[test]
    fn polar_absolute_names_the_point_by_radius_and_angle() {
        let preview = parse_preview(
            "G21 G90 G17 G0 X0 Y0\nG1 @10 ^90\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = preview.segments.last().expect("a move").end;
        assert!(end[0].abs() < 1e-9, "{end:?}");
        assert!((end[1] - 10.0).abs() < 1e-9, "{end:?}");
    }

    #[test]
    fn a_lone_polar_word_keeps_the_other_coordinate() {
        // `@` keeps the angle the tool is already at; `^` keeps its radius.
        let preview = parse_preview(
            "G21 G90 G17 G0 X10 Y0\nG1 @5\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = preview.segments.last().expect("a move").end;
        assert!((end[0] - 5.0).abs() < 1e-9, "{end:?}");
        assert!(end[1].abs() < 1e-9, "{end:?}");

        let preview = parse_preview(
            "G21 G90 G17 G0 X10 Y0\nG1 ^90\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = preview.segments.last().expect("a move").end;
        assert!(end[0].abs() < 1e-9, "{end:?}");
        assert!((end[1] - 10.0).abs() < 1e-9, "{end:?}");
    }

    #[test]
    fn polar_incremental_adds_radius_then_angle() {
        // The spiral.ngc step in inches: 2 in out, then 0.0025 in in and 4.5
        // degrees on. The interpreter keeps the angle while it shrinks the
        // radius, then turns, which is the same point as doing both at once.
        let preview = parse_preview(
            "G20 G90 G17 G0 X2 Y0\nG91 G1 @-.0025 ^4.5\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = preview.segments.last().expect("a move").end;
        let radius = (2.0 - 0.0025) * 25.4;
        let (sin, cos) = 4.5f64.to_radians().sin_cos();
        assert!((end[0] - radius * cos).abs() < 1e-9, "{end:?}");
        assert!((end[1] - radius * sin).abs() < 1e-9, "{end:?}");
    }

    #[test]
    fn a_polar_word_names_the_next_hole_in_a_drill_cycle() {
        // polar.ngc: the first position establishes the radius, then `^` steps
        // the angle. The interpreter turns those into hole positions, so the
        // holes are drawn instead of the block being refused.
        let preview = parse_preview(
            "G21 G90 G17 G0 X63.5 Y0 Z5\nG81 R2.54 Z-12.7 F60\n^120\n^240\nM2\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let holes: Vec<[f64; 3]> = preview
            .segments
            .iter()
            .filter(|segment| segment.kind == SegmentKind::Feed)
            .map(|segment| segment.end)
            .collect();
        assert_eq!(holes.len(), 3, "{holes:?}");
        assert!((holes[0][0] - 63.5).abs() < 1e-9, "{:?}", holes[0]);
        let (sin, cos) = 120f64.to_radians().sin_cos();
        assert!((holes[1][0] - 63.5 * cos).abs() < 1e-9, "{:?}", holes[1]);
        assert!((holes[1][1] - 63.5 * sin).abs() < 1e-9, "{:?}", holes[1]);
    }

    #[test]
    fn the_l_word_repeats_a_drill_cycle() {
        // hole-circle.ngc in one block: six holes 60 degrees apart on a 3 inch
        // bolt circle, each pass one polar step further round.
        let preview = parse_preview(
            "G20 G17 G90\nG0 X1.5 Y0 Z0\nG91 G81 R.1 Z-.5 ^60 L6 F10\nM2\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let holes: Vec<[f64; 3]> = preview
            .segments
            .iter()
            .filter(|segment| segment.kind == SegmentKind::Feed)
            .map(|segment| segment.end)
            .collect();
        assert_eq!(holes.len(), 6, "{holes:?}");
        let radius = 1.5 * 25.4;
        let (sin, cos) = 60f64.to_radians().sin_cos();
        assert!((holes[0][0] - radius * cos).abs() < 1e-6, "{:?}", holes[0]);
        assert!((holes[0][1] - radius * sin).abs() < 1e-6, "{:?}", holes[0]);
        // The sixth pass is a full turn from the start.
        assert!((holes[5][0] - radius).abs() < 1e-6, "{:?}", holes[5]);
        assert!(holes[5][1].abs() < 1e-6, "{:?}", holes[5]);
    }

    #[test]
    fn a_parameter_above_thirty_survives_a_subroutine_return() {
        // daisy.ngc sets #61 in one subroutine and reads it in another.
        // "Subroutines can change the value of parameters above #30 and those
        // changes will be visible to the calling code"; #1-#30 and a plain
        // `#<name>` are local to the call.
        let text = "G21 G90\n#1 = 7\no100 sub\n#1 = 99\n#61 = 42\n#<local> = 5\n#<_kept> = 3\no100 endsub\no100 call\nG1 X[#1] Y[#61] F100\nG0 X[#<_kept>]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let feed = preview
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Feed)
            .expect("a feed move");
        assert_eq!((feed.end[0], feed.end[1]), (7.0, 42.0));
        let end = preview.segments.last().expect("a move").end;
        assert_eq!(end[0], 3.0, "the global named parameter is visible");
    }

    #[test]
    fn a_g10_block_writes_an_offset_without_moving() {
        // The probe macros zero a WCS with `G10 L2 P#5220 ...`. The axis words
        // are an offset, not a destination, so the block draws no segment and
        // the moves after it are drawn against the new row.
        let preview = parse_preview(
            "G21 G90 G54\nG0 X0 Y0 Z5\nG10 L2 P1 X10 Y20\nG0 X1 Y0\nM2\n",
            &preview_axes(),
            None,
            [0.0; 3],
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(preview.segments.len(), 2, "two G0 moves and no G10 move");
        let end = preview.segments.last().expect("a move").end;
        assert_eq!((end[0], end[1]), (11.0, 20.0));
    }

    #[test]
    fn polar_refuses_the_blocks_the_interpreter_refuses() {
        // X or Y in the same block leaves the plane ambiguous.
        let preview = parse_preview("G21 G90 G17 G1 X1 @2 ^3\n", &preview_axes(), None, [0.0; 3]);
        let issue = preview.error.expect("error");
        assert!(issue.message.contains("X or Y"), "{}", issue.message);
        assert!(preview.incomplete);

        // Polar turns are XY only.
        let preview = parse_preview("G21 G90 G18 G1 @2 ^3\n", &preview_axes(), None, [0.0; 3]);
        let issue = preview.error.expect("error");
        assert!(issue.message.contains("G17"), "{}", issue.message);

        // At the origin there is no angle for a radius to keep.
        let preview = parse_preview("G21 G90 G17 G91 G1 @2\n", &preview_axes(), None, [0.0; 3]);
        let issue = preview.error.expect("error");
        assert!(issue.message.contains("origin"), "{}", issue.message);
    }

    #[test]
    fn the_spiral_program_draws_every_pass() {
        let text = "g20 g64 g90\ns3400 m3\n\ng0 z1\ng0 x2 y0\ng1 z-.1 f24\n\n\
                    o100 repeat [800]\ng91 g1 @-.0025 ^4.5\no100 endrepeat\n\n\
                    g90 g0 z1\nm2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(
            preview.segments.len(),
            804,
            "three setup moves, 800 polar moves, one retract"
        );
        let bounds = preview.bounds.expect("bounds");
        let [width, height, _] = bounds.size();
        assert!(width > 90.0 && width < 104.0, "{bounds:?}");
        assert!(height > 90.0 && height < 104.0, "{bounds:?}");
        // Ten turns in, 0.0025 in per turn, the tool is back at the middle.
        let end = preview.segments[802].end;
        assert!(end[0].abs() < 1e-6 && end[1].abs() < 1e-6, "{end:?}");
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

    /// Parse with a tool radius on file, which is what `G41`/`G42` read.
    fn parse_with_tool(text: &str, tool: u16, diameter: f64) -> Preview {
        parse_preview_full(
            text,
            &preview_axes(),
            None,
            [0.0; 3],
            &Subroutines::default(),
            &Tools::new([(tool, diameter * 0.5)]),
        )
    }

    /// The drawn contour, rounded, so a test can name where the tool centre
    /// goes without carrying float noise.
    fn drawn(preview: &Preview) -> Vec<([f64; 2], [f64; 2])> {
        preview
            .segments
            .iter()
            .map(|segment| {
                let round = |point: [f64; 3]| {
                    [
                        (point[0] * 1e6).round() / 1e6,
                        (point[1] * 1e6).round() / 1e6,
                    ]
                };
                (round(segment.start), round(segment.end))
            })
            .collect()
    }

    /// Parse with a work offset in force, so a test can tell the work position
    /// `#<_x>` reads from the machine position `#<_abs_x>` reads.
    fn parse_with_offset(text: &str, offset: [f64; 3]) -> Preview {
        let mut values = AxisMap::default();
        for (axis, value) in [("X", offset[0]), ("Y", offset[1]), ("Z", offset[2])] {
            values.insert(axis, value);
        }
        parse_preview_full(
            text,
            &preview_axes(),
            Some(&values),
            [0.0; 3],
            &Subroutines::default(),
            &Tools::default(),
        )
    }

    /// Where a program's last move ends.
    fn ends(preview: &Preview) -> [f64; 3] {
        preview
            .segments
            .last()
            .map_or([0.0; 3], |segment| segment.end)
    }

    /// Every drawn endpoint, in three axes, so a lathe program's X and Z can be
    /// told apart from a mill program's X and Y.
    fn segment_ends(preview: &Preview) -> Vec<[f64; 3]> {
        preview.segments.iter().map(|segment| segment.end).collect()
    }

    #[test]
    fn a_g53_block_draws_the_machine_point_it_names() {
        // G53 X20 puts the machine at 20 whatever the work offset is, so the
        // drawn move ends at 20 and not at 20 + 100. G53 is a one-block word:
        // the next block is back in the work system.
        let text = "G21 G90\nG53 G0 X20\nG0 X10\nM2\n";
        let preview = parse_with_offset(text, [100.0, 0.0, 0.0]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let machine = preview.segments.first().expect("the G53 move");
        assert!((machine.end[0] - 20.0).abs() < 1e-9, "{machine:?}");
        assert!(
            (ends(&preview)[0] - 110.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_thread_cycle_draws_the_root_it_leaves() {
        // g76.ngc's thread: the tool is on the drive line at X0.2, I-0.075 puts
        // the peak 0.075 in and K0.045 the full depth beyond it, so the root the
        // cycle cuts is at 0.2 - 0.075 + 0.045 = 0.17 inches. L2 with E0.05 tapers
        // the exit.
        let text =
            "G20 G90 G18\nG0 X0.2 Z0\nG76 P0.05 Z-.5 I-.075 J0.008 K0.045 Q29.5 L2 E0.05\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let root = 0.17 * 25.4;
        let cut = preview
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Feed)
            .expect("the threading cut");
        assert!((cut.start[0] - root).abs() < 1e-6, "{cut:?}");
        assert!((cut.end[2] + 0.45 * 25.4).abs() < 1e-6, "{cut:?}");
        // The tool finishes at the end of the drive line, tapered out to X0.2.
        let end = ends(&preview);
        assert!((end[0] - 0.2 * 25.4).abs() < 1e-6, "{end:?}");
        assert!((end[2] + 0.5 * 25.4).abs() < 1e-6, "{end:?}");
        assert!(preview.incomplete, "the passes between are not drawn");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("threading passes"), "{reason}");
    }

    #[test]
    fn a_thread_cycle_outside_the_zx_plane_is_reported() {
        let text = "G21 G90\nG0 X5 Z0\nG76 P1 Z-10 I-1 J0.2 K1 Q29.5 H0\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let error = preview.error.expect("a reported error");
        assert_eq!(error.line, 3);
        assert!(error.message.contains("G18"), "{}", error.message);
    }

    #[test]
    fn a_profile_cycle_draws_the_contour_its_q_subroutine_names() {
        // `G71 Q100 X2 Z-20` names a contour, not a point: X2 Z-20 are the limits
        // the roughing passes work inside. Drawing the block's own words put a
        // line where no cut happens; the cut is the profile in `o100 sub`.
        let text = "G21 G90 G18\nG0 X10 Z0\nG71 Q100 X2 Z-20 D0.5 I1 R0.3\nM2\n\
                    o100 sub\nG0 X10 Z0\nG1 X6 Z-10 F200\nG1 X2 Z-10\no100 endsub\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let contour = segment_ends(&preview);
        assert!(
            contour
                .iter()
                .any(|end| (end[0] - 6.0).abs() < 1e-9 && (end[2] + 10.0).abs() < 1e-9),
            "the profile's first cut: {contour:?}"
        );
        assert!(
            contour.iter().all(|end| (end[2] + 20.0).abs() > 1e-9),
            "nothing goes to the cycle's Z limit: {contour:?}"
        );
        assert!(preview.incomplete, "the roughing passes are not drawn");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("G71 roughing passes"), "{reason}");
    }

    #[test]
    fn a_finishing_cycle_draws_the_profile_without_a_roughing_report() {
        // G70 only follows the profile, so nothing is left undrawn.
        let text = "G21 G90 G18\nG0 X10 Z0\nG70 Q100 X2 Z-20 D0.5 P1\nM2\n\
                    o100 sub\nG1 X6 Z-10 F200\nG1 X2 Z-10\no100 endsub\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let contour = segment_ends(&preview);
        assert!(
            contour
                .iter()
                .any(|end| (end[0] - 2.0).abs() < 1e-9 && (end[2] + 10.0).abs() < 1e-9),
            "the profile's last cut: {contour:?}"
        );
    }

    #[test]
    fn a_profile_cycle_without_its_subroutine_is_reported() {
        let text = "G21 G90 G18\nG0 X10 Z0\nG72 Q999 X2 Z-20 D0.5 I1 R0.3\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("G72 profile subroutine"), "{reason}");
        // The cycle's own X/Z words are limits, so they draw nothing either.
        let contour = segment_ends(&preview);
        assert!(
            contour.iter().all(|end| (end[2] + 20.0).abs() > 1e-9),
            "no line to the limits: {contour:?}"
        );
    }

    #[test]
    fn a_rotary_word_reads_back_in_degrees() {
        // `#<_a>` is the relative A position in degrees, so the program's own
        // length units do not scale it. In a G20 program 45 degrees divided by
        // 45 is one inch: reading it as 45/25.4 degrees instead would draw a
        // hundredth of that.
        let text = "G20 G90\nG0 A45\n#1 = [[#<_a>] / 45]\nG90 G0 X[#1]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 25.4).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_rotary_move_adds_up_in_incremental_mode() {
        let text = "G21 G91\nG0 A10\nG0 A10\n#1 = [#<_a>]\nG90 G0 X[#1]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 20.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_rotary_move_is_reported_because_it_is_not_drawn() {
        // The X move in the same block is still drawn; the A part is what the
        // preview cannot show, so it says so instead of dropping it silently.
        let text = "G21 G90\nG0 X10\nG1 X20 A90 F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview.incomplete, "the rotary move must be reported");
        let reason = preview.incomplete_reason.clone().unwrap_or_default();
        assert!(reason.contains("line 3"), "{reason}");
        assert!(reason.contains("A axis move is not drawn"), "{reason}");
        assert!(
            (ends(&preview)[0] - 20.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn the_rotary_mirror_reads_the_same_as_the_named_parameter() {
        let text = "G21 G90\nG0 A30\n#1 = [#5423]\nG0 X[#1]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 30.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_probe_move_is_drawn_at_feed_rate_not_as_the_last_motion() {
        // `G38.2` after a `G0` used to draw as a rapid, and after a `G1` the
        // same line moved nothing at all until the next block. It is its own
        // kind of move: a straight feed-rate line to the programmed point.
        let text = "G21 G90\nG0 Z5\nG38.2 Z-10 F100\n#1 = [#<_motion_mode>]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let probe = preview
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Probe)
            .expect("a probe segment");
        assert!((probe.end[2] + 10.0).abs() < 1e-9, "{probe:?}");
        // A probe is not a cut, so it is not counted as one, and it is not a
        // rapid either.
        assert_eq!(preview.rapids, 1, "only the G0 is a rapid");
    }

    #[test]
    fn a_probe_leaves_the_motion_mode_alone_because_it_is_not_modal() {
        let text = "G21 G90\nG1 X0 F100\nG38.2 Z-10\nG1 X[#<_motion_mode> + 10]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        // `#<_motion_mode>` is still G1, so the block after the probe feeds to
        // X11 rather than rapiding there.
        assert!(
            (ends(&preview)[0] - 11.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_probe_reports_the_programmed_point_it_drew_to() {
        // An unsuccessful probe reports the programmed point; the preview's
        // probe never trips, so that is the point it reports too, and #5070
        // says the probe did not succeed.
        let text =
            "G21 G90\nG0 Z5\nG38.2 Z-10 F100\n#1 = [#5063]\n#2 = [#5070]\nG0 X[#1] Y[#2]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = ends(&preview);
        assert!((end[0] + 10.0).abs() < 1e-9, "{:?}", end);
        assert_eq!(end[1], 0.0);
    }

    #[test]
    fn a_probe_that_names_no_axis_is_reported() {
        let text = "G21 G90\nG0 Z5\nG38.2 F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        let error = preview.error.expect("a reported error");
        assert_eq!(error.line, 3);
        assert!(error.message.contains("axis word"), "{}", error.message);
    }

    #[test]
    fn a_probe_under_cutter_compensation_is_reported() {
        // Cutters cannot probe: the reference lists compensation as an error.
        let text = "G21 G90\nG0 X0 Y0 Z5\nG41 D1\nG1 X10\nG38.2 Z-10 F100\nM2\n";
        let preview = parse_with_tool(text, 1, 10.0);
        let error = preview.error.expect("a reported error");
        assert_eq!(error.line, 5);
        assert!(
            error.message.contains("cutter compensation"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_probe_to_the_point_it_is_already_at_draws_a_zero_length_move() {
        // The interpreter refuses this, but the preview cannot: it puts every
        // probe's end at the programmed point, so `smartprobe.ngc`'s
        // `G38.5 z#7` retract looks zero-length on its second pass, while on the
        // machine the probe stopped short of it the first time. Reporting it
        // would fail a program that runs.
        let text = "G21 G90\nG0 X0 Y0 Z5\nG38.2 Z5 F100\nG0 X10\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview
            .segments
            .iter()
            .any(|segment| segment.kind == SegmentKind::Probe));
    }

    #[test]
    fn a_live_position_parameter_reads_where_the_tool_is() {
        // The lathe macros all open this way: save the position the tool is at
        // when the macro is called, then measure the cut from it. Read as zero,
        // every one of them drew its cut from the work origin.
        let text = "G21 G90\nG0 X10 Y5\n#1 = [#<_x>]\nG1 X[#1 + 5] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 15.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn the_absolute_position_leaves_the_work_offset_out() {
        // The tool is at machine X110 with a 100 mm work offset, so the work
        // position is 10: `#<_x>` reads 10 and `#<_abs_x>` reads 110. Each
        // incremental move below adds the value it read, so the two land 100 mm
        // apart, which is exactly the offset.
        let relative = "G21 G90\nG0 X10\n#1 = [#<_x>]\nG91 G0 X[#1]\nM2\n";
        let preview = parse_with_offset(relative, [100.0, 0.0, 0.0]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 120.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );

        let absolute = "G21 G90\nG0 X10\n#1 = [#<_abs_x>]\nG91 G0 X[#1]\nM2\n";
        let preview = parse_with_offset(absolute, [100.0, 0.0, 0.0]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 220.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_tool_length_block_does_not_move_the_live_position() {
        // G43 states the tool length; its Z word is not a move, so the live Z
        // stays where the block before it left the tool.
        let text = "G21 G90\nG0 Z5\nG43 H1 Z10\n#1 = [#<_z>]\nG0 X[#1]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 5.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_g53_move_names_the_machine_position() {
        // G53 X20 puts the machine at 20 whatever the offset is, so the machine
        // position reads 20 and the work position reads 20 - 100. The drawing
        // pass still treats G53 as a work-system move, so this asserts the live
        // value alone: asserted absolutely, X[20] lands on machine 120, while
        // reading the work position instead would land on 220.
        let text = "G21 G90\nG53 G0 X20\n#1 = [#<_abs_x>]\nG90 G0 X[#1]\nM2\n";
        let preview = parse_with_offset(text, [100.0, 0.0, 0.0]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 120.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_subroutine_hands_its_return_value_back() {
        let text = "G21 G90\no100 sub\no100 return [42]\no100 endsub\no100 call\nG1 X[#<_value>] Y[#<_value_returned>] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let end = ends(&preview);
        assert!((end[0] - 42.0).abs() < 1e-9, "{:?}", end);
        assert!((end[1] - 1.0).abs() < 1e-9, "{:?}", end);
    }

    #[test]
    fn a_recursive_subroutine_multiplies_through_its_return_values() {
        // factorial.ngc, cut down: `return [#<_value> * #1]` reads the value the
        // call it just made handed back, so 4! needs every frame's value kept.
        let text = "G21 G90\no100 sub\no110 if [#1 EQ 0]\no100 return [1]\no110 else\no100 call [#1 - 1]\no100 return [#<_value> * #1]\no110 endif\no100 endsub\no100 call [4]\nG1 X[#<_value>] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 24.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_subroutine_that_returns_nothing_reads_zero() {
        let text = "G21 G90\no100 sub\no100 return\no100 endsub\no100 call\nG1 X[#<_value>] Y[#<_value_returned>] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert_eq!(ends(&preview)[0], 0.0);
        assert_eq!(ends(&preview)[1], 0.0);
    }

    #[test]
    fn the_modal_status_parameters_follow_the_blocks_that_set_them() {
        // G20 makes the program imperial, G91 incremental, G18 the XZ plane,
        // G55 the work system, M3 the spindle clockwise. Each status parameter
        // then says so, and the move adds them up: 1 + 10 + 18.
        let text = "G20 G91 G18 G0 X1\nG55 M3\nG1 X[#<_imperial> + #<_incremental> * 10 + #<_plane>] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        // 29 inches from where the first move left the tool at 1 inch.
        assert!(
            (ends(&preview)[0] - 30.0 * 25.4).abs() < 1e-6,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn the_live_position_is_written_in_the_program_units() {
        let text = "G20 G90\nG0 X1\n#1 = [#<_x>]\nG1 X[#1 + 1] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 2.0 * 25.4).abs() < 1e-6,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn the_numbered_mirror_of_a_live_position_reads_the_same() {
        let text = "G21 G90\nG0 X10\n#1 = [#5420]\nG1 X[#1 + 5] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 15.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_canned_cycle_leaves_the_live_z_clear_of_the_part() {
        // G98: after the hole the tool is back at the Z it started the cycle at.
        let text = "G21 G90\nG0 Z5\nG81 X10 Z-3 R1 F100\n#1 = [#<_z>]\nG0 X[#1]\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 5.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn the_preview_reports_itself_as_the_preview_interpreter() {
        // #<_task> is 0 for the interpreter that is not milling, and the probe
        // macros branch on it to keep the preview away from the probe test.
        let text =
            "G21 G90\no100 if [#<_task> EQ 0]\nG1 X10 F100\no100 else\nG1 X99\no100 endif\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 10.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn a_parameter_the_preview_cannot_know_still_reads_zero() {
        // Probe results are named parameters a remap sets at run time; the
        // preview has no probe to ask, and must not invent a value.
        let text = "G21 G90\nG1 X[#<_distance> + 1] F100\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(
            (ends(&preview)[0] - 1.0).abs() < 1e-9,
            "{:?}",
            ends(&preview)
        );
    }

    #[test]
    fn g41_runs_the_tool_centre_left_of_the_contour() {
        // A 10 mm cutter (D=10) cutting the top of a block while travelling in
        // +X: the tool centre is 5 mm above the programmed line, and the first
        // move is the lead-in from the programmed start.
        let text = "G21 G90 G17\nG0 X0 Y0 Z5\nG1 Z0 F100\nG41 D1\nG1 X40\nG1 Y20\nG40\nM2\n";
        let preview = parse_with_tool(text, 1, 10.0);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let contour = drawn(&preview);
        // The Z plunge, then the lead-in from the programmed start to the
        // offset line, then the line itself, then up the joined corner.
        assert_eq!(contour[2], ([0.0, 0.0], [35.0, 5.0]));
        assert_eq!(contour[3], ([35.0, 5.0], [35.0, 20.0]));
    }

    #[test]
    fn g42_runs_the_tool_centre_right_of_the_contour() {
        let text = "G21 G90 G17\nG0 X0 Y0 Z5\nG1 Z0 F100\nG42 D1\nG1 X40\nG1 Y20\nG40\nM2\n";
        let preview = parse_with_tool(text, 1, 10.0);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let contour = drawn(&preview);
        assert_eq!(contour[2], ([0.0, 0.0], [45.0, -5.0]));
        assert_eq!(contour[3], ([45.0, -5.0], [45.0, 20.0]));
    }

    #[test]
    fn the_d_word_names_the_tool_whose_radius_is_used() {
        // comp311.ngc switches tools before it compensates and lets the D word
        // be absent, so the radius comes from the tool in the spindle.
        let text = "G21 G90 G17\nT4 M6\nG0 X0 Y0 Z5\nG1 Z0 F100\nG41\nG1 X40\nG40\nM2\n";
        let preview = parse_with_tool(text, 4, 8.0);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(drawn(&preview)[2], ([0.0, 0.0], [40.0, 4.0]));
    }

    #[test]
    fn g41_1_takes_the_radius_from_the_d_word() {
        // The dynamic form carries the radius itself, so it needs no table.
        let text = "G21 G90 G17\nG0 X0 Y0 Z5\nG1 Z0 F100\nG41.1 D2.5\nG1 X40\nG40\nM2\n";
        let preview = parse_preview(text, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(drawn(&preview)[2], ([0.0, 0.0], [40.0, 2.5]));
    }

    #[test]
    fn a_compensated_arc_keeps_its_centre_and_moves_its_radius() {
        // G41 round a clockwise arc keeps the tool outside it, so the tool
        // centre swings on a circle one radius larger than the programmed one.
        let text = "G21 G90 G17\nG0 X10 Y0 Z5\nG1 Z0 F100\nG41 D1\nG2 X0 Y-10 I-10 J0\nG40\nM2\n";
        let preview = parse_with_tool(text, 1, 6.0);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        let arc = preview
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Arc)
            .expect("the compensated arc");
        let spec = arc.arc.as_ref().expect("arc spec").clone();
        assert!((spec.radius - 13.0).abs() < 1e-9, "{spec:?}");
        assert!(spec.center[0].abs() < 1e-9, "{spec:?}");
        assert!((arc.end[1] + 13.0).abs() < 1e-9, "{:?}", arc.end);
    }

    #[test]
    fn compensation_without_a_known_radius_is_reported() {
        // The table is present but has no row for tool 7, so the path is drawn
        // on the programmed line and the preview says why.
        let tools = Tools::new([(1, 5.0)]);
        let text = "G21 G90 G17\nG0 X0 Y0 Z5\nG41 D7\nG1 X40 F100\nG40\nM2\n";
        let preview = parse_preview_full(
            text,
            &preview_axes(),
            None,
            [0.0; 3],
            &Subroutines::default(),
            &tools,
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let reason = preview.incomplete_reason.clone().expect("a reason");
        assert!(reason.contains("no radius for tool 7"), "{reason}");
        assert_eq!(drawn(&preview)[1], ([0.0, 0.0], [40.0, 0.0]));
    }

    #[test]
    fn a_lead_in_shorter_than_the_radius_is_reported() {
        let text = "G21 G90 G17\nG0 X0 Y0 Z5\nG1 Z0 F100\nG41 D1\nG1 X1\nG40\nM2\n";
        let preview = parse_with_tool(text, 1, 20.0);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let reason = preview.incomplete_reason.clone().expect("a reason");
        assert!(reason.contains("shorter than"), "{reason}");
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

    #[test]
    fn named_parameters_are_case_insensitive() {
        // CIRCLE_XY.NGC assigns `#<RADIUS>` and draws with `I[-#<radius>]`. The
        // interpreter keeps named parameters in a map compared with
        // `strcasecmp`, so this is one parameter and the arc has a radius.
        let preview = plan("G21 G90\n#<RADIUS> = 5\nG0 X10 Y0\nG3 X10 Y0 I[-#<radius>] J0 F3000\n");
        assert_eq!(preview.error, None);
        let arc = preview
            .segments
            .iter()
            .find_map(|segment| segment.arc.clone())
            .expect("one arc");
        assert!((arc.radius - 5.0).abs() < 1e-9, "radius {}", arc.radius);
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

    /// The whole spiral, not its first arc. Every arc reaches a little past its
    /// own endpoint words, so the extent is the swept path: X -49.476410 and
    /// 47.880597 mm, Y -50.274371 and 48.678496 mm.
    ///
    /// Reference: a Python script that shares no code with this parser samples
    /// the same file at 4000 points per arc, picking R-form centers by the
    /// major/minor-arc rule. It gives exactly those four numbers; the endpoint
    /// words alone would be short by up to 0.05 mm.
    #[test]
    fn arcspiral_preview_spans_the_whole_spiral_in_machine_units() {
        let preview = parse_preview(ARCSPIRAL, &preview_axes(), None, [0.0; 3]);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(!preview.incomplete, "{:?}", preview.incomplete_reason);
        assert_eq!(preview.arcs, 999);
        assert_eq!(preview.rapids, 4);
        assert_eq!(preview.segments.len(), 1005);
        let bounds = preview.bounds.expect("bounds");
        assert!((bounds.min[0] - -49.476410).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.max[0] - 47.880597).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.min[1] - -50.274371).abs() < 1e-4, "{bounds:?}");
        assert!((bounds.max[1] - 48.678496).abs() < 1e-4, "{bounds:?}");
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

    fn kinds(line: &str) -> Vec<(TokenKind, String)> {
        lex_line(line)
            .into_iter()
            .map(|token| (token.kind, token.text))
            .collect()
    }

    #[test]
    fn arc_centre_words_are_value_words_not_parameters() {
        // I, J and K carry a value on the block. A `#` reference is what a
        // parameter looks like, and they must not be painted alike.
        assert_eq!(
            kinds("G2 X10 Y0 I2 J0"),
            vec![
                (TokenKind::GCode, "G2".into()),
                (TokenKind::Axis, "X10".into()),
                (TokenKind::Axis, "Y0".into()),
                (TokenKind::Word, "I2".into()),
                (TokenKind::Word, "J0".into()),
            ]
        );
        assert_eq!(
            kinds("G18 G3 X10 Z0 I2 K0")[4..],
            [
                (TokenKind::Word, "I2".into()),
                (TokenKind::Word, "K0".into())
            ]
        );
        // A signed centre and a bracketed one are words too.
        assert_eq!(
            kinds("G2 X10 Y10 I-5 J0")[3],
            (TokenKind::Word, "I-5".into())
        );
        assert_eq!(kinds("G2 X[#1] I[#2] J0")[5], (TokenKind::Word, "I".into()));
        // A radius, and a cycle's parameter words, are the same kind.
        assert_eq!(kinds("G2 X10 Y0 R5")[3], (TokenKind::Word, "R5".into()));
        assert_eq!(
            kinds("G81 X0 Y0 Z-5 R2 P1")[5],
            (TokenKind::Word, "P1".into())
        );
    }

    #[test]
    fn an_o_statement_lexes_its_keyword_and_operators() {
        // The bug this covers: `while` lexed as the axis word W plus a run of
        // parameter letters, so the editor coloured each letter separately.
        assert_eq!(
            kinds("o100 WHILE [#<i> Lt 4]"),
            vec![
                (TokenKind::OWord, "o100".into()),
                (TokenKind::Keyword, "WHILE".into()),
                (TokenKind::Variable, "[".into()),
                (TokenKind::Variable, "#<i>".into()),
                (TokenKind::Operator, "Lt".into()),
                (TokenKind::Number, "4".into()),
                (TokenKind::Variable, "]".into()),
            ]
        );
        // A named label, mixed case, and no space after the label.
        assert_eq!(
            kinds("o<probe_z> call [1]"),
            vec![
                (TokenKind::OWord, "o<probe_z>".into()),
                (TokenKind::Keyword, "call".into()),
                (TokenKind::Variable, "[".into()),
                (TokenKind::Number, "1".into()),
                (TokenKind::Variable, "]".into()),
            ]
        );
        assert_eq!(
            kinds("o200 Endwhile")[1],
            (TokenKind::Keyword, "Endwhile".into())
        );
        assert_eq!(
            kinds("o100while[#1 lt 2]")[1],
            (TokenKind::Keyword, "while".into())
        );
    }

    #[test]
    fn every_interpreter_o_keyword_lexes_as_one() {
        // interp_read.cc `read_o_word`: the closed set the interpreter
        // dispatches on.
        for keyword in [
            "sub",
            "endsub",
            "call",
            "do",
            "while",
            "repeat",
            "if",
            "elseif",
            "else",
            "endif",
            "break",
            "continue",
            "endwhile",
            "endrepeat",
            "return",
        ] {
            let line = format!("o100 {keyword}");
            let kinds = kinds(&line);
            assert_eq!(kinds[0], (TokenKind::OWord, "o100".into()), "{line}");
            assert_eq!(
                kinds[1],
                (TokenKind::Keyword, keyword.to_string()),
                "{line} is a keyword"
            );
        }
        // `goto` is not one of them: the interpreter rejects it, so it must not
        // be painted as syntax that runs.
        let kinds = kinds("o100 goto 20");
        assert!(kinds.iter().all(|(kind, _)| *kind != TokenKind::Keyword));
    }

    #[test]
    fn every_label_form_the_interpreter_accepts_lexes_as_a_label() {
        // A number, a name, and a computed expression.
        for (line, label) in [
            ("o100 call", "o100"),
            ("o<myfile> call", "o<myfile>"),
            ("o[#101+2] call", "o[#101+2]"),
        ] {
            let kinds = kinds(line);
            assert_eq!(kinds[0], (TokenKind::OWord, label.into()), "{line}");
            assert_eq!(
                kinds[1],
                (TokenKind::Keyword, "call".into()),
                "{line} is a call"
            );
        }
        // The Fanuc-style bare definition has a label and nothing else.
        assert_eq!(kinds("o2000"), vec![(TokenKind::OWord, "o2000".into())]);
        assert_eq!(
            kinds("o100 (Example 1)")[0],
            (TokenKind::OWord, "o100".into())
        );
    }

    #[test]
    fn a_return_value_expression_lexes_after_the_keyword() {
        assert_eq!(
            kinds("o123 endsub [3 * 4]"),
            vec![
                (TokenKind::OWord, "o123".into()),
                (TokenKind::Keyword, "endsub".into()),
                (TokenKind::Variable, "[".into()),
                (TokenKind::Number, "3".into()),
                (TokenKind::Operator, "*".into()),
                (TokenKind::Number, "4".into()),
                (TokenKind::Variable, "]".into()),
            ]
        );
    }

    #[test]
    fn a_line_that_only_starts_with_o_is_not_an_o_statement() {
        // No label, so the words are ordinary G-code and keep their own kinds.
        assert_eq!(kinds("G0 X1")[0], (TokenKind::GCode, "G0".into()));
        assert!(kinds("oops G0")
            .iter()
            .all(|(kind, _)| *kind != TokenKind::Keyword));
    }

    #[test]
    fn an_o_statement_still_drives_the_planner() {
        // The lexer's kinds are the editor's business; the planner reads the
        // line's text, so a keyword token must not change the path.
        let preview = plan("o100 while [#<i> lt 2]\nG0 X#<i>\n#<i> = [#<i> + 1]\no100 endwhile\n");
        assert!(preview.error.is_none());
        assert_eq!(preview.loops.len(), 1);
        assert_eq!(preview.loops[0].iterations, vec![vec![0.0], vec![1.0]]);
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
    fn a_nested_block_carries_the_whole_loop_nest() {
        // Outer pass runs the inner loop to completion, so every segment of the
        // outer body names both loops and the pass each is on.
        let preview = plan(
            "o10 while [#<outer> lt 2]\n#<inner> = 0\no20 while [#<inner> lt 2]\n\
             G0 X[#<inner> + 1]\n#<inner> = [#<inner> + 1]\no20 endwhile\n\
             #<outer> = [#<outer> + 1]\no10 endwhile\n",
        );
        assert_eq!(preview.loops.len(), 2);
        let nests: Vec<Vec<(u32, u32)>> = (0..preview.segments.len())
            .map(|index| preview.segment_stack(index).to_vec())
            .collect();
        assert_eq!(
            nests,
            vec![
                vec![(0, 0), (1, 0)],
                vec![(0, 0), (1, 1)],
                // The inner trace is unrolled across the whole run, so its pass
                // keeps counting through the outer passes: 3 is the second pass
                // of the second time round.
                vec![(0, 1), (1, 2)],
                vec![(0, 1), (1, 3)],
            ],
            "outermost pass first, inner pass last"
        );
        assert_eq!(preview.loops[0].iterations.len(), 2);
        assert_eq!(preview.loops[1].iterations.len(), 4);
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
    fn a_repeat_of_one_path_records_its_seam_and_heading() {
        // One full circle per pass: the body line and the endpoint repeat, so
        // the pass tag cannot tell the passes apart. The seam is where a pass
        // ends, and the heading is the tangent out of it.
        let preview = plan("G0 X10 Y0\no200 repeat [3]\nG3 X10 Y0 I-5 J0 F100\no200 endrepeat\n");
        let path = preview.loops[0].repeated_path.expect("one repeated path");
        assert!((path.seam[0] - 10.0).abs() < 1e-9, "{:?}", path.seam);
        assert!(path.seam[1].abs() < 1e-9, "{:?}", path.seam);
        // A full circle of radius 5: its drawn length is the circumference.
        assert!((path.length - std::f64::consts::TAU * 5.0).abs() < 1e-6);
        // The gate scale reaches the far side of the circle.
        assert!(path.reach > 9.9, "{}", path.reach);
        // Counter-clockwise from (10,0), the tangent points along +Y.
        assert!(path.heading[1] > 0.99, "{:?}", path.heading);
    }

    #[test]
    fn a_repeat_of_the_same_two_moves_records_the_second_passes_heading() {
        let preview = plan("o200 repeat [3]\nG0 X10\nG0 X0\no200 endrepeat\n");
        let path = preview.loops[0].repeated_path.expect("one repeated path");
        // The first pass ends where the second starts, so the seam is X0; the
        // second pass leaves it towards X10.
        assert!(path.seam[0].abs() < 1e-9, "{:?}", path.seam);
        assert!(path.heading[0] > 0.99, "{:?}", path.heading);
        assert!((path.length - 20.0).abs() < 1e-9, "{}", path.length);
    }

    #[test]
    fn a_loop_that_moves_each_pass_has_no_repeated_path() {
        let preview =
            plan("#<i> = 0\no10 while [#<i> lt 3]\nG0 X#<i>\n#<i> = [#<i> + 1]\no10 endwhile\n");
        assert!(preview.loops[0].repeated_path.is_none());
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
