//! Retained values across a daemon restart.
//!
//! A retain store is a small binary blob: the program hash and the layout hash
//! gate the restore, a CRC catches a torn write, and the magic plus format
//! version catch a file from a different build. The format is deliberately
//! boring so a refused file can say exactly why it was refused.
//!
//! Nothing here touches forces: a restored value is a value, so the operator's
//! diagnostics are never silently turned into a forced plant.

/// `LPRT`: LiNC PLC Retain.
pub const MAGIC: [u8; 4] = *b"LPRT";
/// Bumped when the byte layout changes. A reader refuses what it does not know.
pub const FORMAT: u16 = 1;
/// A store larger than this is refused before it is parsed; 64 KiB holds every
/// slot this VM has with room to spare.
pub const MAX_BLOB: usize = 64 * 1024;

/// The values to keep: retained bit slots by index, and the whole word bank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retained {
    pub bits: Vec<(u16, bool)>,
    pub words: Vec<(u16, i16)>,
}

impl Retained {
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty() && self.words.is_empty()
    }
}

/// Why a store was refused. Every variant has a sentence an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    TooLarge(usize),
    BadMagic,
    UnsupportedFormat(u16),
    Truncated,
    CrcMismatch,
    LayoutMismatch,
    WrongProgram,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::TooLarge(size) => format!(
                "retain: the store is {size} bytes; the limit is {MAX_BLOB}, so it is not loaded"
            ),
            Self::BadMagic => "retain: the file is not a retain store (bad magic)".to_string(),
            Self::UnsupportedFormat(version) => {
                format!("retain: format {version} is not supported (this build writes {FORMAT})")
            }
            Self::Truncated => "retain: the store ends before its CRC".to_string(),
            Self::CrcMismatch => {
                "retain: the CRC does not match; the file was torn or edited".to_string()
            }
            Self::LayoutMismatch => {
                "retain: the program layout changed, so the saved slots do not line up".to_string()
            }
            Self::WrongProgram => "retain: the store belongs to a different program".to_string(),
        }
    }
}

/// CRC-32/ISO-HDLC, the usual zlib polynomial, without a dependency.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// The layout hash: the retained slots by name and index, plus the word count.
/// Reordering a project keeps the program hash only if the shape is identical,
/// and this catches the case where the shape moved under the same text.
pub fn layout_hash(retained_names: &[(u16, &str)], words: u16) -> u64 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&words.to_le_bytes());
    for (index, name) in retained_names {
        bytes.extend_from_slice(&index.to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
    }
    fnv1a(&bytes)
}

/// Serialise a store. The CRC covers everything before it.
pub fn pack(program_hash: &str, layout: u64, retained: &Retained) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&FORMAT.to_le_bytes());
    bytes.extend_from_slice(&layout.to_le_bytes());
    let hash = program_hash.as_bytes();
    bytes.extend_from_slice(&(hash.len() as u8).to_le_bytes());
    bytes.extend_from_slice(hash);
    bytes.extend_from_slice(&(retained.bits.len() as u16).to_le_bytes());
    for (slot, value) in &retained.bits {
        bytes.extend_from_slice(&slot.to_le_bytes());
        bytes.push(u8::from(*value));
    }
    bytes.extend_from_slice(&(retained.words.len() as u16).to_le_bytes());
    for (slot, value) in &retained.words {
        bytes.extend_from_slice(&slot.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let crc = crc32(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

/// Parse a store, refusing anything that does not belong to this program.
pub fn unpack(bytes: &[u8], program_hash: &str, layout: u64) -> Result<Retained, Refusal> {
    if bytes.len() > MAX_BLOB {
        return Err(Refusal::TooLarge(bytes.len()));
    }
    if bytes.len() < 4 + 2 + 8 + 1 + 2 + 2 + 4 {
        return Err(Refusal::Truncated);
    }
    if bytes[0..4] != MAGIC {
        return Err(Refusal::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != FORMAT {
        return Err(Refusal::UnsupportedFormat(version));
    }
    let body = &bytes[..bytes.len() - 4];
    let stored_crc = u32::from_le_bytes([
        bytes[bytes.len() - 4],
        bytes[bytes.len() - 3],
        bytes[bytes.len() - 2],
        bytes[bytes.len() - 1],
    ]);
    if crc32(body) != stored_crc {
        return Err(Refusal::CrcMismatch);
    }
    let stored_layout = u64::from_le_bytes(body[6..14].try_into().map_err(|_| Refusal::Truncated)?);
    if stored_layout != layout {
        return Err(Refusal::LayoutMismatch);
    }
    let mut cursor = 14usize;
    let hash_len = *body.get(cursor).ok_or(Refusal::Truncated)? as usize;
    cursor += 1;
    let stored_hash = body
        .get(cursor..cursor + hash_len)
        .ok_or(Refusal::Truncated)?;
    cursor += hash_len;
    if stored_hash != program_hash.as_bytes() {
        return Err(Refusal::WrongProgram);
    }
    let bit_count = read_u16(body, &mut cursor)?;
    let mut bits = Vec::with_capacity(bit_count as usize);
    for _ in 0..bit_count {
        let slot = read_u16(body, &mut cursor)?;
        let value = *body.get(cursor).ok_or(Refusal::Truncated)? != 0;
        cursor += 1;
        bits.push((slot, value));
    }
    let word_count = read_u16(body, &mut cursor)?;
    let mut words = Vec::with_capacity(word_count as usize);
    for _ in 0..word_count {
        let slot = read_u16(body, &mut cursor)?;
        let low = *body.get(cursor).ok_or(Refusal::Truncated)?;
        let high = *body.get(cursor + 1).ok_or(Refusal::Truncated)?;
        cursor += 2;
        words.push((slot, i16::from_le_bytes([low, high])));
    }
    Ok(Retained { bits, words })
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, Refusal> {
    let low = *bytes.get(*cursor).ok_or(Refusal::Truncated)?;
    let high = *bytes.get(*cursor + 1).ok_or(Refusal::Truncated)?;
    *cursor += 2;
    Ok(u16::from_le_bytes([low, high]))
}
