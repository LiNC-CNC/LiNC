//! LinuxCNC G-code lexer and bounded preview parser.
//!
//! The preview parser is intentionally small: it understands the words needed
//! to draw a toolpath and mark obvious syntax errors. It never imports
//! remaps, never runs subroutines and never talks to the machine interpreter.

use std::collections::BTreeMap;

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
        if (i == 0 || line[..i].trim().is_empty()) && (ch == 'N' || ch == 'n') {
            if bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
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
        let g = self
            .gcodes
            .iter()
            .map(|g| g.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(" ");
        if !g.is_empty() {
            lines.push(("G".into(), g));
        }
        if let Some(feed) = self.feed {
            lines.push(("F".into(), format!("{feed}")));
        }
        if let Some(speed) = self.speed {
            lines.push(("S".into(), format!("{speed}")));
        }
        if let Some(tool) = self.tool {
            lines.push(("T".into(), format!("{tool}")));
        }
        lines.push(("WCS".into(), self.wcs.clone()));
        lines.push(("UNITS".into(), self.units.clone()));
        lines.push(("PLANE".into(), self.plane.clone()));
        lines.push(("DIST".into(), self.distance_mode.clone()));
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
        }
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
        let cu = start[u] + i.unwrap_or(0.0);
        let cv = start[v] + j.unwrap_or(0.0);
        let _ = k;
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

/// Bounded preview parser. `axis_names` selects which coordinate words matter;
/// `offset` is the active WCS offset in machine coordinates.
pub fn parse_preview(
    text: &str,
    axis_names: &[String],
    offset: Option<&AxisMap<f64>>,
    current_position: [f64; 3],
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
    let mut first_point = current_position;
    let mut saw_motion = false;
    let mut line_count = 0;

    for (line_index, raw_line) in text.lines().enumerate() {
        let line_no = line_index + 1;
        line_count = line_no;
        let tokens = lex_line(raw_line);
        if tokens.is_empty() {
            continue;
        }
        let has_unmatched = raw_line.matches('(').count() != raw_line.matches(')').count();
        if has_unmatched {
            preview.error = Some(SyntaxIssue {
                line: line_no,
                message: "unmatched comment parenthesis".into(),
            });
            break;
        }
        let line_start_segment = preview.segments.len();

        // O-words are structured blocks. The preview does not execute them.
        if tokens.iter().any(|token| token.kind == TokenKind::OWord) {
            // Still scan words for modal state, but do not emit a move.
        }

        // Validate obvious malformed axis words and words without values.
        for token in &tokens {
            if token.kind == TokenKind::Axis || token.kind == TokenKind::Parameter {
                let letter = token.text.chars().next().unwrap_or(' ');
                if !matches!(
                    letter.to_ascii_uppercase(),
                    'X' | 'Y'
                        | 'Z'
                        | 'A'
                        | 'B'
                        | 'C'
                        | 'U'
                        | 'V'
                        | 'W'
                        | 'I'
                        | 'J'
                        | 'K'
                        | 'R'
                        | 'P'
                        | 'Q'
                        | 'L'
                        | 'D'
                        | 'H'
                        | 'E'
                ) {
                    continue;
                }
                let rest: String = token.text.chars().skip(1).collect();
                if rest.is_empty() || parse_number(&rest).is_none() {
                    // A bare axis letter is invalid unless an expression
                    // follows in brackets; keep the last valid graph.
                    if !raw_line.contains('[') {
                        preview.error = Some(SyntaxIssue {
                            line: line_no,
                            message: format!("missing value after {}", letter),
                        });
                    }
                }
            }
            if token.kind == TokenKind::FeedSpeed || token.kind == TokenKind::Tool {
                let rest: String = token.text.chars().skip(1).collect();
                if parse_number(&rest).is_none() {
                    preview.error = Some(SyntaxIssue {
                        line: line_no,
                        message: format!(
                            "missing value after {}",
                            token.text.chars().next().unwrap_or(' ')
                        ),
                    });
                }
            }
        }
        if preview.error.is_some() {
            break;
        }

        // Modal words.
        for token in &tokens {
            if token.kind == TokenKind::GCode {
                if let Some(value) = parse_number(&token.text[1..]) {
                    if value == 0.0 || value == 1.0 || value == 2.0 || value == 3.0 {
                        motion = value;
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
                target[axis] = if absolute {
                    value + offset_values(&letter.to_ascii_uppercase().to_string())
                } else {
                    position[axis] + value
                };
            }
        }

        if !saw_motion {
            first_point = position;
            saw_motion = true;
        }

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
                word_value(&tokens, 'i'),
                word_value(&tokens, 'j'),
                word_value(&tokens, 'k'),
                word_value(&tokens, 'r'),
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
        preview
            .line_bounds
            .insert(line_no, (line_start_segment, preview.segments.len()));
        position = target;
    }

    preview.line_count = line_count;
    preview.bounds = if saw_motion {
        Bounds3::from_points(
            preview
                .segments
                .iter()
                .flat_map(|segment| [segment.start, segment.end])
                .chain([first_point]),
        )
    } else {
        None
    };
    preview.modal = ModalSummary {
        gcodes: {
            let mut codes = vec![format_g(motion)];
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
}
