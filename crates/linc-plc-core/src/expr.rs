//! Free-text expressions for compare cells and compute coils.
//!
//! ClassicLadder's compare and operate blocks carry a string evaluated every
//! scan (`EvalCompare`, `MakeCalc`). The grammar is that one, with operands
//! respelled to LiNC names by the importer: comparisons use `=` and never `==`,
//! `<>` is not-equal, and `,`, `&` and `|` combine conditions.
//!
//! The parser is a Pratt parser over a token vector, so every refusal carries the
//! byte offset of the token that caused it, and it builds a small typed tree
//! before lowering, so a condition and a number cannot be confused. Lowering
//! produces the VM's stack ops: a word expression leaves one word on the word
//! stack, a condition leaves one bit on the bit stack.

use crate::compile::{Op, WordOperand};
use crate::profile::resolve::resolve;

/// Nesting limit for one expression. The parser must bound its own recursion
/// before it walks the tokens, or a deep expression would overflow the stack
/// before any diagnostic could be produced.
pub const MAX_EXPR_DEPTH: usize = 32;
/// Operations one expression may lower to. The scan's operand stacks are
/// `STACK_DEPTH` deep, and this keeps an expression far inside that.
pub const MAX_EXPR_OPS: usize = 512;

/// Declared word names, lowercased, mapped to their bank slot. The resolver owns
/// the spelling; this table only answers a declared name.
pub use crate::profile::SymbolLookup;

/// Why an expression was refused. The variant picks the validator's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprErrorKind {
    Syntax,
    Operand,
    Function,
    Depth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub kind: ExprErrorKind,
    /// Byte offset of the offending token in the source text.
    pub offset: usize,
    pub message: String,
}

impl ExprError {
    fn new(kind: ExprErrorKind, offset: usize, message: impl Into<String>) -> Self {
        Self {
            kind,
            offset,
            message: message.into(),
        }
    }

    /// The stable diagnostic code this refusal maps to.
    pub fn code(&self) -> &'static str {
        match self.kind {
            ExprErrorKind::Syntax => "expr_syntax",
            ExprErrorKind::Operand => "expr_operand",
            ExprErrorKind::Function => "expr_function",
            ExprErrorKind::Depth => "expr_depth",
        }
    }
}

/// Lower a condition. The ops leave one bit on the bit stack.
pub fn lower_bit(text: &str, lookup: &SymbolLookup) -> Result<Vec<Op>, ExprError> {
    let node = parse_expression(text, lookup)?;
    if node.kind() != Kind::Bit {
        return Err(ExprError::new(
            ExprErrorKind::Operand,
            0,
            "this expression is a number, not a condition; compare it with `=` or `<`",
        ));
    }
    let mut ops = Vec::new();
    emit(&node, &mut ops);
    check_ops(&ops)?;
    Ok(ops)
}

/// Lower a compute coil's assignment, `target = value`. Returns the target and
/// the ops that leave the value on the word stack.
pub fn lower_assignment(
    text: &str,
    lookup: &SymbolLookup,
) -> Result<(WordOperand, Vec<Op>), ExprError> {
    let tokens = lex(text)?;
    let mut depth = 0usize;
    let split = tokens.iter().position(|token| {
        match token.token {
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            Token::Eq if depth == 0 => return true,
            _ => {}
        }
        false
    });
    let Some(split) = split else {
        return Err(ExprError::new(
            ExprErrorKind::Syntax,
            0,
            "a compute coil needs `target = value`",
        ));
    };
    let target = parse_target(&tokens[..split], lookup)?;
    let value = &tokens[split + 1..];
    if value.is_empty() {
        return Err(ExprError::new(
            ExprErrorKind::Syntax,
            tokens[split].offset,
            "the assignment has no value after `=`",
        ));
    }
    let mut parser = Parser::new(value, lookup);
    let node = parser.parse(0)?;
    parser.finish()?;
    if node.kind() != Kind::Word {
        return Err(ExprError::new(
            ExprErrorKind::Operand,
            tokens[split].offset,
            "an assignment writes a number; a condition is written into a bit coil",
        ));
    }
    let mut ops = Vec::new();
    emit(&node, &mut ops);
    check_ops(&ops)?;
    Ok((target, ops))
}

fn check_ops(ops: &[Op]) -> Result<(), ExprError> {
    if ops.len() > MAX_EXPR_OPS {
        return Err(ExprError::new(
            ExprErrorKind::Depth,
            0,
            format!(
                "the expression lowers to {} operations; the limit is {MAX_EXPR_OPS}",
                ops.len()
            ),
        ));
    }
    Ok(())
}

// --- lexer -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Number(i32),
    Name(String),
    LParen,
    RParen,
    Comma,
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
}

#[derive(Debug, Clone)]
struct TokenAt {
    token: Token,
    offset: usize,
}

impl Token {
    fn symbol(&self) -> &'static str {
        match self {
            Token::Number(_) => "number",
            Token::Name(_) => "name",
            Token::LParen => "(",
            Token::RParen => ")",
            Token::Comma => ",",
            Token::Or => "|",
            Token::And => "&",
            Token::Eq => "=",
            Token::Ne => "<>",
            Token::Lt => "<",
            Token::Le => "<=",
            Token::Gt => ">",
            Token::Ge => ">=",
            Token::Plus => "+",
            Token::Minus => "-",
            Token::Star => "*",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Caret => "^",
        }
    }
}

fn lex(text: &str) -> Result<Vec<TokenAt>, ExprError> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let offset = index;
        // Numbers. A sign belongs to the unary operator, never to the literal.
        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            let text = &text[start..index];
            let value = text.parse::<i32>().map_err(|_| {
                ExprError::new(
                    ExprErrorKind::Syntax,
                    offset,
                    format!("`{text}` does not fit in 32 bits"),
                )
            })?;
            tokens.push(TokenAt {
                token: Token::Number(value),
                offset,
            });
            continue;
        }
        // Names, including a block member's `.member` suffix.
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            if index < bytes.len() && bytes[index] == b'.' {
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
            }
            tokens.push(TokenAt {
                token: Token::Name(text[start..index].to_string()),
                offset,
            });
            continue;
        }
        let two = bytes.get(index + 1).copied();
        let (token, width) = match (byte, two) {
            (b'<', Some(b'>')) => (Token::Ne, 2),
            (b'<', Some(b'=')) => (Token::Le, 2),
            (b'>', Some(b'=')) => (Token::Ge, 2),
            (b'=', _) => (Token::Eq, 1),
            (b'<', _) => (Token::Lt, 1),
            (b'>', _) => (Token::Gt, 1),
            (b'|', _) => (Token::Or, 1),
            (b'&', _) => (Token::And, 1),
            (b',', _) => (Token::Comma, 1),
            (b'(', _) => (Token::LParen, 1),
            (b')', _) => (Token::RParen, 1),
            (b'+', _) => (Token::Plus, 1),
            (b'-', _) => (Token::Minus, 1),
            (b'*', _) => (Token::Star, 1),
            (b'/', _) => (Token::Slash, 1),
            (b'%', _) => (Token::Percent, 1),
            (b'^', _) => (Token::Caret, 1),
            _ => {
                let character = text[index..].chars().next().unwrap_or('?');
                return Err(ExprError::new(
                    ExprErrorKind::Syntax,
                    offset,
                    format!(
                        "`{character}` is not part of an expression; operands are LiNC names, not `%` addresses"
                    ),
                ));
            }
        };
        tokens.push(TokenAt { token, offset });
        index += width;
    }
    Ok(tokens)
}

// --- parser ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Bit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Func {
    Abs,
    Mini,
    Maxi,
    Moy,
}

#[derive(Debug)]
enum BinKind {
    Or,
    And,
    Cmp(CompareOp),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

#[derive(Debug)]
enum Node {
    Const(i32),
    Slot(u16),
    Block(u16),
    BlockPreset(u16),
    Bin(BinKind, Box<Node>, Box<Node>),
    Neg(Box<Node>),
    Pos(Box<Node>),
    Not(Box<Node>),
    Call(Func, Vec<Node>),
}

impl Node {
    fn kind(&self) -> Kind {
        match self {
            Node::Const(_) | Node::Slot(_) | Node::Block(_) | Node::BlockPreset(_) => Kind::Word,
            Node::Neg(_) | Node::Pos(_) => Kind::Word,
            Node::Not(_) => Kind::Bit,
            Node::Bin(kind, _, _) => match kind {
                BinKind::Or | BinKind::And | BinKind::Cmp(_) => Kind::Bit,
                _ => Kind::Word,
            },
            Node::Call(_, _) => Kind::Word,
        }
    }
}

/// Binding powers, low to high. `,` is lowest because ClassicLadder reads it as
/// the AND between comparison groups, and `^` is highest and right-associative.
const BP_COMMA: u8 = 1;
const BP_OR: u8 = 2;
const BP_AND: u8 = 3;
const BP_COMPARE: u8 = 4;
const BP_ADD: u8 = 5;
const BP_MUL: u8 = 6;
const BP_UNARY: u8 = 8;
const BP_POW: u8 = 9;

struct Parser<'a> {
    tokens: &'a [TokenAt],
    at: usize,
    lookup: &'a SymbolLookup,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [TokenAt], lookup: &'a SymbolLookup) -> Self {
        Self {
            tokens,
            at: 0,
            lookup,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<&'a TokenAt> {
        self.tokens.get(self.at)
    }

    fn offset_here(&self) -> usize {
        self.tokens
            .get(self.at)
            .map(|token| token.offset)
            .or_else(|| self.tokens.last().map(|token| token.offset))
            .unwrap_or(0)
    }

    fn finish(&self) -> Result<(), ExprError> {
        match self.peek() {
            Some(token) => Err(ExprError::new(
                ExprErrorKind::Syntax,
                token.offset,
                format!("`{}` is not expected here", token.token.symbol()),
            )),
            None => Ok(()),
        }
    }

    fn parse(&mut self, min_bp: u8) -> Result<Node, ExprError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            return Err(ExprError::new(
                ExprErrorKind::Depth,
                self.offset_here(),
                format!("the expression nests deeper than {MAX_EXPR_DEPTH} levels"),
            ));
        }
        let mut lhs = self.parse_prefix()?;
        while let Some((bin, lbp, rbp, symbol)) = self.peek_binary() {
            if lbp < min_bp {
                break;
            }
            let offset = self.offset_here();
            self.at += 1;
            let rhs = self.parse(rbp)?;
            lhs = combine(bin, lhs, rhs, offset, symbol)?;
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn peek_binary(&self) -> Option<(BinKind, u8, u8, &'static str)> {
        let token = self.peek()?;
        let (kind, lbp, rbp) = match token.token {
            Token::Comma => (BinKind::And, BP_COMMA, BP_OR),
            Token::Or => (BinKind::Or, BP_OR, BP_AND),
            Token::And => (BinKind::And, BP_AND, BP_COMPARE),
            Token::Eq => (BinKind::Cmp(CompareOp::Eq), BP_COMPARE, BP_ADD),
            Token::Ne => (BinKind::Cmp(CompareOp::Ne), BP_COMPARE, BP_ADD),
            Token::Lt => (BinKind::Cmp(CompareOp::Lt), BP_COMPARE, BP_ADD),
            Token::Le => (BinKind::Cmp(CompareOp::Le), BP_COMPARE, BP_ADD),
            Token::Gt => (BinKind::Cmp(CompareOp::Gt), BP_COMPARE, BP_ADD),
            Token::Ge => (BinKind::Cmp(CompareOp::Ge), BP_COMPARE, BP_ADD),
            Token::Plus => (BinKind::Add, BP_ADD, BP_MUL),
            Token::Minus => (BinKind::Sub, BP_ADD, BP_MUL),
            Token::Star => (BinKind::Mul, BP_MUL, BP_UNARY),
            Token::Slash => (BinKind::Div, BP_MUL, BP_UNARY),
            Token::Percent => (BinKind::Mod, BP_MUL, BP_UNARY),
            // Right-associative: the right side accepts an operator of the same
            // power, so `2^3^2` is `2^(3^2)`.
            Token::Caret => (BinKind::Pow, BP_POW, BP_POW),
            _ => return None,
        };
        Some((kind, lbp, rbp, token.token.symbol()))
    }

    fn parse_prefix(&mut self) -> Result<Node, ExprError> {
        let Some(token) = self.peek() else {
            return Err(ExprError::new(
                ExprErrorKind::Syntax,
                self.offset_here(),
                "the expression ends where a value is needed",
            ));
        };
        let offset = token.offset;
        match &token.token {
            Token::Number(value) => {
                let value = *value;
                self.at += 1;
                Ok(Node::Const(value))
            }
            Token::Minus | Token::Plus => {
                let negate = token.token == Token::Minus;
                self.at += 1;
                let operand = self.parse(BP_UNARY)?;
                if operand.kind() != Kind::Word {
                    return Err(ExprError::new(
                        ExprErrorKind::Operand,
                        offset,
                        format!(
                            "`{}` applies to a number, not to a condition",
                            if negate { "-" } else { "+" }
                        ),
                    ));
                }
                Ok(if negate {
                    Node::Neg(Box::new(operand))
                } else {
                    Node::Pos(Box::new(operand))
                })
            }
            Token::LParen => {
                self.at += 1;
                let inner = self.parse(0)?;
                match self.peek() {
                    Some(TokenAt {
                        token: Token::RParen,
                        ..
                    }) => {
                        self.at += 1;
                        Ok(inner)
                    }
                    Some(other) => Err(ExprError::new(
                        ExprErrorKind::Syntax,
                        other.offset,
                        format!("expected `)`, found `{}`", other.token.symbol()),
                    )),
                    None => Err(ExprError::new(
                        ExprErrorKind::Syntax,
                        offset,
                        "this `(` is never closed",
                    )),
                }
            }
            Token::Name(name) => {
                let name = name.clone();
                self.at += 1;
                let upper = name.to_ascii_uppercase();
                if upper == "NOT" {
                    let operand = self.parse(BP_UNARY)?;
                    if operand.kind() != Kind::Bit {
                        return Err(ExprError::new(
                            ExprErrorKind::Operand,
                            offset,
                            "`NOT` negates a condition, not a number; compare it first",
                        ));
                    }
                    return Ok(Node::Not(Box::new(operand)));
                }
                let next_is_call = matches!(
                    self.peek(),
                    Some(TokenAt {
                        token: Token::LParen,
                        ..
                    })
                );
                if next_is_call {
                    let Some(func) = function(&upper) else {
                        return Err(ExprError::new(
                            ExprErrorKind::Function,
                            offset,
                            format!("`{name}` is not a function; use ABS, MINI, MAXI, MOY or AVG"),
                        ));
                    };
                    return self.parse_call(func, offset);
                }
                resolve_operand(&name, offset, self.lookup)
            }
            other => {
                let symbol = other.symbol();
                self.at += 1;
                Err(ExprError::new(
                    ExprErrorKind::Syntax,
                    offset,
                    format!("expected a value, found `{symbol}`"),
                ))
            }
        }
    }

    fn parse_call(&mut self, func: Func, offset: usize) -> Result<Node, ExprError> {
        // Consume `(`.
        self.at += 1;
        let mut args = Vec::new();
        if matches!(
            self.peek(),
            Some(TokenAt {
                token: Token::RParen,
                ..
            })
        ) {
            self.at += 1;
        } else {
            loop {
                // Arguments stop at `,`, which is the AND operator everywhere
                // else. BP_COMMA + 1 keeps this comma for the argument list.
                args.push(self.parse(BP_COMMA + 1)?);
                match self.peek() {
                    Some(TokenAt {
                        token: Token::Comma,
                        ..
                    }) => {
                        self.at += 1;
                    }
                    Some(TokenAt {
                        token: Token::RParen,
                        ..
                    }) => {
                        self.at += 1;
                        break;
                    }
                    Some(other) => {
                        return Err(ExprError::new(
                            ExprErrorKind::Syntax,
                            other.offset,
                            format!("expected `,` or `)`, found `{}`", other.token.symbol()),
                        ))
                    }
                    None => {
                        return Err(ExprError::new(
                            ExprErrorKind::Syntax,
                            offset,
                            "this call is never closed",
                        ))
                    }
                }
            }
        }
        let wanted = if func == Func::Abs { 1 } else { 2 };
        if args.len() != wanted {
            let name = function_name(func);
            return Err(ExprError::new(
                ExprErrorKind::Function,
                offset,
                format!(
                    "`{name}` takes {wanted} argument{}, not {}",
                    if wanted == 1 { "" } else { "s" },
                    args.len()
                ),
            ));
        }
        for argument in &args {
            if argument.kind() != Kind::Word {
                return Err(ExprError::new(
                    ExprErrorKind::Operand,
                    offset,
                    format!("`{}` takes numbers, not conditions", function_name(func)),
                ));
            }
        }
        Ok(Node::Call(func, args))
    }
}

fn parse_expression(text: &str, lookup: &SymbolLookup) -> Result<Node, ExprError> {
    let tokens = lex(text)?;
    if tokens.is_empty() {
        return Err(ExprError::new(
            ExprErrorKind::Syntax,
            0,
            "the expression is empty",
        ));
    }
    let mut parser = Parser::new(&tokens, lookup);
    let node = parser.parse(0)?;
    parser.finish()?;
    Ok(node)
}

fn parse_target(tokens: &[TokenAt], lookup: &SymbolLookup) -> Result<WordOperand, ExprError> {
    match tokens {
        [TokenAt {
            token: Token::Name(name),
            offset,
        }] => resolve_target(name, *offset, lookup),
        [TokenAt { offset, .. }, ..] => Err(ExprError::new(
            ExprErrorKind::Syntax,
            *offset,
            "an assignment writes a single target: a marker word, a word point or a block member",
        )),
        [] => Err(ExprError::new(
            ExprErrorKind::Syntax,
            0,
            "the assignment has no target before `=`",
        )),
    }
}

fn resolve_target(
    name: &str,
    offset: usize,
    lookup: &SymbolLookup,
) -> Result<WordOperand, ExprError> {
    match resolve(name, lookup) {
        Ok(operand) if operand.is_writable() => Ok(operand),
        Ok(_) => Err(ExprError::new(
            ExprErrorKind::Operand,
            offset,
            format!("`{name}` is a value, not a place to write"),
        )),
        Err(_) => Err(ExprError::new(
            ExprErrorKind::Operand,
            offset,
            format!(
                "`{name}` is not a word this project declares; an assignment writes a marker word, a word point or a block member"
            ),
        )),
    }
}

fn resolve_operand(name: &str, offset: usize, lookup: &SymbolLookup) -> Result<Node, ExprError> {
    match resolve(name, lookup) {
        Ok(WordOperand::Word(slot) | WordOperand::Slot(slot)) => Ok(Node::Slot(slot)),
        Ok(WordOperand::Block(instance)) => Ok(Node::Block(instance)),
        Ok(WordOperand::BlockPreset(instance)) => Ok(Node::BlockPreset(instance)),
        Ok(WordOperand::Const(value)) => Ok(Node::Const(value)),
        Err(_) => Err(ExprError::new(
            ExprErrorKind::Operand,
            offset,
            format!(
                "`{name}` is not a marker word, a block member or a word this project declares"
            ),
        )),
    }
}

fn function(upper: &str) -> Option<Func> {
    match upper {
        "ABS" => Some(Func::Abs),
        "MINI" => Some(Func::Mini),
        "MAXI" => Some(Func::Maxi),
        // ClassicLadder spells the average in French; both names are accepted.
        "MOY" | "AVG" => Some(Func::Moy),
        _ => None,
    }
}

fn function_name(func: Func) -> &'static str {
    match func {
        Func::Abs => "ABS",
        Func::Mini => "MINI",
        Func::Maxi => "MAXI",
        Func::Moy => "MOY",
    }
}

fn combine(
    kind: BinKind,
    lhs: Node,
    rhs: Node,
    offset: usize,
    symbol: &'static str,
) -> Result<Node, ExprError> {
    match kind {
        BinKind::Or | BinKind::And => {
            if lhs.kind() != Kind::Bit || rhs.kind() != Kind::Bit {
                return Err(ExprError::new(
                    ExprErrorKind::Operand,
                    offset,
                    format!("`{symbol}` combines conditions; compare each side first"),
                ));
            }
        }
        BinKind::Cmp(_) => {
            if lhs.kind() != Kind::Word || rhs.kind() != Kind::Word {
                return Err(ExprError::new(
                    ExprErrorKind::Operand,
                    offset,
                    format!("`{symbol}` compares two numbers"),
                ));
            }
        }
        _ => {
            if lhs.kind() != Kind::Word || rhs.kind() != Kind::Word {
                return Err(ExprError::new(
                    ExprErrorKind::Operand,
                    offset,
                    format!("`{symbol}` needs a number on each side"),
                ));
            }
        }
    }
    Ok(Node::Bin(kind, Box::new(lhs), Box::new(rhs)))
}

fn emit(node: &Node, ops: &mut Vec<Op>) {
    match node {
        Node::Const(value) => ops.push(Op::LoadConst(*value)),
        Node::Slot(slot) => ops.push(Op::LoadWord(*slot)),
        Node::Block(instance) => ops.push(Op::FbWord(*instance)),
        Node::BlockPreset(instance) => ops.push(Op::FbPreset(*instance)),
        Node::Pos(inner) => emit(inner, ops),
        // `0 - x`, so the word stack order matches `Op::Sub`.
        Node::Neg(inner) => {
            ops.push(Op::LoadConst(0));
            emit(inner, ops);
            ops.push(Op::Sub);
        }
        Node::Not(inner) => {
            emit(inner, ops);
            ops.push(Op::Not);
        }
        Node::Bin(kind, left, right) => {
            emit(left, ops);
            emit(right, ops);
            ops.push(match kind {
                BinKind::Or => Op::Or,
                BinKind::And => Op::And,
                BinKind::Cmp(CompareOp::Eq) => Op::CmpEq,
                BinKind::Cmp(CompareOp::Ne) => Op::CmpNe,
                BinKind::Cmp(CompareOp::Lt) => Op::CmpLt,
                BinKind::Cmp(CompareOp::Le) => Op::CmpLe,
                BinKind::Cmp(CompareOp::Gt) => Op::CmpGt,
                BinKind::Cmp(CompareOp::Ge) => Op::CmpGe,
                BinKind::Add => Op::Add,
                BinKind::Sub => Op::Sub,
                BinKind::Mul => Op::Mul,
                BinKind::Div => Op::Div,
                BinKind::Mod => Op::Mod,
                BinKind::Pow => Op::Pow,
            });
        }
        Node::Call(Func::Abs, args) => {
            emit(&args[0], ops);
            ops.push(Op::Abs);
        }
        Node::Call(Func::Mini, args) => {
            emit(&args[0], ops);
            emit(&args[1], ops);
            ops.push(Op::Min);
        }
        Node::Call(Func::Maxi, args) => {
            emit(&args[0], ops);
            emit(&args[1], ops);
            ops.push(Op::Max);
        }
        Node::Call(Func::Moy, args) => {
            emit(&args[0], ops);
            emit(&args[1], ops);
            ops.push(Op::Add);
            ops.push(Op::LoadConst(2));
            ops.push(Op::Div);
        }
    }
}
