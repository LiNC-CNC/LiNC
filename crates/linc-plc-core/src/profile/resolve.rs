//! Operand resolution: the one place text becomes a slot.
//!
//! Before M2 the same spellings were parsed in three places — `compile`'s
//! `word_operand_value`, `il`'s marker-word parser and `expr`'s symbol lookup —
//! so a dialect's operand grammar lived in three modules that had to agree.
//! [`resolve`] is now the only function that turns text into an [`Operand`];
//! everything else calls it or delegates to it.
//!
//! The spellings it owns are the dialect's: a marker word, a time preset, a
//! decimal literal, a block member, and a declared word or float point.

use std::collections::HashMap;
use std::fmt;

use crate::profile::layout::WORD_COUNT;
use crate::profile::spec::{self, Family, FbName};
use crate::profile::{SymbolLookup, FB_PER_TYPE};

/// A marker word's spelling prefix.
pub const MARKER_PREFIX: &str = "MW";

/// The marker word at `index`, e.g. `MW3`. The dialect owns the spelling, so
/// the snapshot and the importer ask for it instead of formatting their own.
pub fn marker_word(index: u16) -> String {
    format!("{MARKER_PREFIX}{index}")
}

/// How many marker words the bank begins with.
///
/// The editor and the importer need the bound; only the profile needs the
/// constant.
pub fn marker_word_count() -> u16 {
    WORD_COUNT
}

/// Every marker word in the bank, in slot order. The editor lists these, and the
/// spelling stays here rather than in the window that draws them.
pub fn marker_words() -> impl Iterator<Item = (u16, String)> {
    (0..WORD_COUNT).map(|index| (index, marker_word(index)))
}

/// The marker-word range as diagnostics print it, e.g. `MW0-15`.
pub fn marker_range() -> String {
    format!("{MARKER_PREFIX}0-{}", WORD_COUNT.saturating_sub(1))
}

/// Which field of a block a word operand names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member {
    /// The preset a program writes: `.PT` on a timer, `.PV` on a counter.
    Preset,
    /// The reading a program reads: `.ET` on a timer, `.CV` on a counter.
    Value,
}

impl Member {
    /// The suffix this member spells on a block of `kind`.
    pub fn suffix(self, kind: FbName) -> &'static str {
        match (self, kind) {
            (Self::Preset, FbName::Ton | FbName::Tof | FbName::Tp) => ".PT",
            (Self::Preset, _) => ".PV",
            (Self::Value, FbName::Ton | FbName::Tof | FbName::Tp) => ".ET",
            (Self::Value, _) => ".CV",
        }
    }
}

/// A block instance's spelling, e.g. `TON2`.
pub fn block_instance(kind: FbName, number: i32) -> String {
    format!("{}{number}", kind.text())
}

/// A block member's spelling, e.g. `TON2.PT`. The importer writes this so what it
/// writes parses back through [`resolve`].
pub fn block_member(kind: FbName, number: i32, member: Member) -> String {
    format!("{}{}", block_instance(kind, number), member.suffix(kind))
}

/// A time preset's spelling, e.g. `T#250ms`. The importer writes this instead of
/// formatting its own, so what it writes parses back through [`parse_preset`].
pub fn time_preset(milliseconds: i32) -> String {
    format!("{}{}ms", TIME_PREFIX, milliseconds)
}

/// A time preset's prefix.
pub const TIME_PREFIX: &str = "T#";

/// Parse a marker word (`MWn`, optionally with a leading `%`) to its slot.
///
/// This is the narrow form the daemon's word force and the editor's marker-word
/// check need: a marker word and nothing else. Use [`resolve`] for a general
/// operand.
pub fn marker_word_slot(text: &str) -> Option<u16> {
    let text = text.trim();
    let bare = text.strip_prefix('%').unwrap_or(text);
    if !bare.is_ascii() || bare.len() <= MARKER_PREFIX.len() {
        return None;
    }
    let (prefix, digits) = bare.split_at(MARKER_PREFIX.len());
    if prefix != MARKER_PREFIX && prefix != "mw" {
        return None;
    }
    let index = digits.parse::<u16>().ok()?;
    (index < WORD_COUNT).then_some(index)
}

/// Does this text start with the marker-word prefix (optionally after `%`)?
///
/// The editor uses it to tell "outside the bank" from "not an operand at all".
/// It says nothing about whether the word exists; [`marker_word_slot`] does.
pub fn looks_like_marker_word(text: &str) -> bool {
    let text = text.trim();
    let bare = text.strip_prefix('%').unwrap_or(text);
    bare.is_ascii()
        && bare.len() >= MARKER_PREFIX.len()
        && bare[..MARKER_PREFIX.len()].eq_ignore_ascii_case(MARKER_PREFIX)
}

/// A resolved operand: something the compiler can address, or a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// A marker word, by bank slot.
    Word(u16),
    /// A declared word or float point, by bank slot.
    Slot(u16),
    /// A literal, including a time preset in milliseconds.
    Const(i32),
    /// A block's `ET` / `CV`.
    Block(u16),
    /// A block's `PT` / `PV`.
    BlockPreset(u16),
}

impl Operand {
    /// Is this somewhere a compute coil may write? A literal is a value, not a
    /// place; a bank slot and a block member (`PT`/`PV`, `ET`/`CV`) are fields.
    pub fn is_writable(self) -> bool {
        matches!(
            self,
            Self::Word(_) | Self::Slot(_) | Self::Block(_) | Self::BlockPreset(_)
        )
    }
}

/// Why one operand's text did not resolve. The caller owns the diagnostic code
/// and sentence, because each site refuses for its own reason; this carries the
/// text that failed so the sentence can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperandError {
    pub text: String,
}

impl fmt::Display for OperandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "`{}` is not a marker word, a number, a time, a block member or a declared word point",
            self.text
        )
    }
}

/// Resolve one operand, in this order: a block member, a marker word, a time,
/// a decimal literal, then a declared word symbol. The order is the dialect's
/// and is what makes `T#4s` a time rather than a symbol name.
pub fn resolve(text: &str, symbols: &SymbolLookup) -> Result<Operand, OperandError> {
    let text = text.trim();
    if let Some(rest) = text
        .strip_suffix(".PT")
        .or_else(|| text.strip_suffix(".PV"))
        .or_else(|| text.strip_suffix(".pt"))
        .or_else(|| text.strip_suffix(".pv"))
    {
        if let Some((_, instance, _)) = parse_block(rest, rest, "") {
            return Ok(Operand::BlockPreset(instance));
        }
        return Err(OperandError {
            text: text.to_string(),
        });
    }
    if let Some(rest) = text
        .strip_suffix(".ET")
        .or_else(|| text.strip_suffix(".CV"))
        .or_else(|| text.strip_suffix(".et"))
        .or_else(|| text.strip_suffix(".cv"))
    {
        if let Some((_, instance, _)) = parse_block(rest, rest, "") {
            return Ok(Operand::Block(instance));
        }
        return Err(OperandError {
            text: text.to_string(),
        });
    }
    if let Some(index) = marker_word_slot(text) {
        return Ok(Operand::Word(index));
    }
    if text.starts_with("T#") || text.starts_with("t#") {
        return parse_preset(text, true)
            .map(Operand::Const)
            .ok_or_else(|| OperandError {
                text: text.to_string(),
            });
    }
    if let Ok(value) = text.parse::<i32>() {
        return Ok(Operand::Const(value));
    }
    if let Some(slot) = symbols
        .get(text)
        .or_else(|| symbols.get(&text.to_ascii_lowercase()))
    {
        return Ok(Operand::Slot(*slot));
    }
    Err(OperandError {
        text: text.to_string(),
    })
}

/// `TON` + `TON2` + `T#4s` -> kind, instance index, preset in ms or counts.
///
/// The instance index is the ABI slot (`kind * FB_PER_TYPE + number`), which the
/// profile owns because it is the profile that fixes four instances per kind.
pub fn parse_block(block: &str, instance: &str, preset: &str) -> Option<(FbName, u16, i32)> {
    let instance = instance.trim();
    let name = block_name(block).or_else(|| {
        let letters: String = instance
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        block_name(&letters)
    })?;
    let digits: String = instance
        .chars()
        .skip_while(|c| c.is_ascii_alphabetic())
        .collect();
    let number = digits.parse::<u16>().unwrap_or(0);
    if number >= FB_PER_TYPE {
        return None;
    }
    let index = name.code() * FB_PER_TYPE + number;
    let timer = matches!(name, FbName::Ton | FbName::Tof | FbName::Tp);
    let preset = parse_preset(preset, timer)?;
    Some((name, index, preset))
}

fn block_name(text: &str) -> Option<FbName> {
    match spec::spec(text)?.family {
        Family::Fb(name) => Some(name),
        _ => None,
    }
}

/// Presets: `T#4s`, `T#1s500ms`, `T#2m`, or a count.
///
/// A timer preset is milliseconds and is **not** clamped: the word bank is 32
/// bits, so `T#5m` is 300,000 ms. An unrepresentable preset is refused (`None`)
/// rather than silently cut down, because a five-minute dwell that runs for
/// 32.7 seconds is a wrong value, not a limitation.
pub fn parse_preset(text: &str, timer: bool) -> Option<i32> {
    let text = text.trim();
    if timer {
        if text.is_empty() {
            return Some(1000);
        }
        let body = text
            .strip_prefix("T#")
            .or_else(|| text.strip_prefix("t#"))
            .unwrap_or(text);
        let mut total = 0i32;
        let mut digits = String::new();
        let mut rest = body;
        while !rest.is_empty() {
            let (number, tail) = match rest.find(|c: char| c.is_ascii_alphabetic()) {
                Some(index) => rest.split_at(index),
                None => (rest, ""),
            };
            digits.clear();
            digits.push_str(number);
            let value: i32 = digits.parse().ok()?;
            let (unit, tail) = if let Some(tail) = tail.strip_prefix("ms") {
                (1, tail)
            } else if let Some(tail) = tail.strip_prefix('s') {
                (1000, tail)
            } else {
                let tail = tail.strip_prefix('m')?;
                (60_000, tail)
            };
            total = total.checked_add(value.checked_mul(unit)?)?;
            rest = tail;
        }
        Some(total.max(0))
    } else {
        if text.is_empty() {
            return Some(10);
        }
        text.parse::<i32>().ok()
    }
}

/// An empty symbol table, for callers that only need the dialect's own
/// spellings (`HashMap::new` allocates nothing until something is inserted).
pub fn no_symbols() -> HashMap<String, u16> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::{
        block_instance, block_member, looks_like_marker_word, marker_range, marker_word,
        marker_word_slot, parse_block, parse_preset, resolve, time_preset, Member, Operand,
    };
    use crate::profile::SymbolLookup;

    fn symbols(pairs: &[(&str, u16)]) -> SymbolLookup {
        pairs
            .iter()
            .map(|(name, slot)| ((*name).to_string(), *slot))
            .collect()
    }

    #[test]
    fn a_marker_word_resolves_to_its_slot_and_the_bank_bounds_it() {
        assert_eq!(resolve("MW3", &symbols(&[])), Ok(Operand::Word(3)));
        assert_eq!(resolve(" %mw15 ", &symbols(&[])), Ok(Operand::Word(15)));
        assert!(resolve("MW16", &symbols(&[])).is_err(), "outside the bank");
        assert!(resolve("MW", &symbols(&[])).is_err());
    }

    #[test]
    fn every_other_spelling_resolves_or_is_refused() {
        let table = symbols(&[("iw0", 16), ("w5", 21)]);
        assert_eq!(resolve("T#4s", &table), Ok(Operand::Const(4_000)));
        assert_eq!(
            parse_preset(&time_preset(250), true),
            Some(250),
            "round trip"
        );
        assert_eq!(resolve("-12", &table), Ok(Operand::Const(-12)));
        assert_eq!(resolve("iw0", &table), Ok(Operand::Slot(16)));
        assert_eq!(
            resolve("IW0", &table),
            Ok(Operand::Slot(16)),
            "case-insensitive"
        );
        assert_eq!(resolve("TON0.ET", &table), Ok(Operand::Block(0)));
        let counter = crate::profile::FbName::Ctu.code() * crate::profile::FB_PER_TYPE + 1;
        assert_eq!(resolve("CTU1.CV", &table), Ok(Operand::Block(counter)));
        assert_eq!(resolve("TON2.PT", &table), Ok(Operand::BlockPreset(2)));
        assert!(resolve("NOPE", &table).is_err());
        assert!(resolve("TON0.Q", &table).is_err(), "Q is a bit, not a word");
        assert!(resolve("BOGUS.PT", &table).is_err());
    }

    #[test]
    fn the_marker_spelling_is_owned_here() {
        assert_eq!(marker_word(3), format!("{}{}", super::MARKER_PREFIX, 3));
        assert_eq!(marker_range(), format!("{}0-15", super::MARKER_PREFIX));
        assert_eq!(marker_word_slot(&marker_word(7)), Some(7));
        assert_eq!(marker_word_slot("mw7"), Some(7));
        assert_eq!(
            marker_word_slot("TON0.ET"),
            None,
            "members are not marker words"
        );
        assert_eq!(marker_word_slot("MW16"), None);
        assert!(looks_like_marker_word("%mw3 "));
        assert!(
            looks_like_marker_word("MW99"),
            "the range check is separate"
        );
        assert!(
            looks_like_marker_word("MW"),
            "a prefix is enough to name the range"
        );
        assert!(!looks_like_marker_word("IW0"));
    }

    #[test]
    fn a_written_block_member_parses_back() {
        let table = SymbolLookup::new();
        for (kind, number) in [
            (crate::profile::spec::FbName::Ton, 2),
            (crate::profile::spec::FbName::Tof, 0),
            (crate::profile::spec::FbName::Ctu, 1),
            (crate::profile::spec::FbName::Ctd, 3),
        ] {
            let preset = block_member(kind, number, Member::Preset);
            let value = block_member(kind, number, Member::Value);
            assert!(resolve(&preset, &table).is_ok(), "`{preset}` must parse");
            assert!(resolve(&value, &table).is_ok(), "`{value}` must parse");
            assert_eq!(
                resolve(&preset, &table).expect("preset"),
                Operand::BlockPreset(
                    parse_block(kind.text(), &block_instance(kind, number), "")
                        .expect("instance")
                        .1,
                ),
            );
        }
    }

    #[test]
    fn a_block_instance_keeps_its_abi_index() {
        let (name, index, preset) = parse_block("CTU", "CTU0", "5").expect("counter");
        assert_eq!(index, name.code() * crate::profile::FB_PER_TYPE);
        assert_eq!(preset, 5);
        assert!(
            parse_block("CTU", "CTU4", "5").is_some(),
            "the fifth instance fits"
        );
        assert!(
            parse_block("CTU", &format!("CTU{}", crate::profile::FB_PER_TYPE), "5").is_none(),
            "one past the last instance does not"
        );
        assert!(parse_block("FROB", "FROB0", "5").is_none());
    }
}
