//! The field script's memory: the five persistent banks of the savemap and the field's scratch bank.
//!
//! Banks are addressed by the 4-bit selector a script stores next to an operand (behaviour as documented for the
//! original engine: FFRTT "FF7/Field/Script" and the PSX decompilation's memory helpers, read for behaviour only):
//!
//! | selector (byte, word) | storage                         |
//! |-----------------------|---------------------------------|
//! | 0                     | the literal stored in the code  |
//! | 1, 2                  | persistent `0x000..0x100`       |
//! | 3, 4                  | persistent `0x100..0x200`       |
//! | 11, 12                | persistent `0x200..0x300`       |
//! | 13, 14                | persistent `0x300..0x400`       |
//! | 15, 7                 | persistent `0x400..0x500`       |
//! | 5, 6                  | the field's 256 scratch bytes   |
//!
//! Byte reads of a word selector read one byte and byte writes store the low byte, as the original does. A word
//! access reads two consecutive bytes little-endian; in a persistent bank a word at address 255 continues into the
//! next bank exactly as the original's flat array does. The scratch bank has no byte after 255: that read is 0 and
//! that write is dropped (the original would touch memory outside the array).

/// Bytes of persistent field memory (five banks of 256).
pub const PERSISTENT_BYTES: usize = 0x500;
/// Bytes of a field's scratch memory.
pub const SCRATCH_BYTES: usize = 256;

/// The persistent banks. These are the "story" of a save: every flag and counter the field scripts keep.
#[derive(Clone, PartialEq, Eq)]
pub struct Banks(pub Box<[u8; PERSISTENT_BYTES]>);

impl Default for Banks {
    fn default() -> Self {
        Banks(Box::new([0; PERSISTENT_BYTES]))
    }
}

impl std::fmt::Debug for Banks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.0.iter().filter(|b| **b != 0).count();
        write!(f, "Banks({set} non-zero bytes)")
    }
}

impl Banks {
    /// The byte at `index` of the flat 0x500-byte image.
    pub fn get(&self, index: usize) -> u8 {
        self.0.get(index).copied().unwrap_or(0)
    }

    pub fn set(&mut self, index: usize, value: u8) {
        if let Some(b) = self.0.get_mut(index) {
            *b = value;
        }
    }

    /// The byte at `address` of the bank a selector names (`None` for a selector that is not persistent).
    pub fn var(&self, selector: u8, address: u8) -> Option<u8> {
        base_of(selector).map(|base| self.get(base + usize::from(address)))
    }

    /// Lower-case hex of the whole image (for sidecar records).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The image a [`Banks::to_hex`] string names; `None` unless it is exactly 0x500 bytes of hex.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != PERSISTENT_BYTES * 2 || !hex.is_ascii() {
            return None;
        }
        let mut out = Box::new([0u8; PERSISTENT_BYTES]);
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Banks(out))
    }
}

/// Where a selector's storage lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Literal,
    Persistent(usize),
    Scratch,
    /// A selector the original does not map (8, 9, 10): reads 0, writes nothing.
    None,
}

/// Start of a persistent selector's 256 bytes in the flat image.
pub fn base_of(selector: u8) -> Option<usize> {
    match selector {
        1 | 2 => Some(0x000),
        3 | 4 => Some(0x100),
        11 | 12 => Some(0x200),
        13 | 14 => Some(0x300),
        15 | 7 => Some(0x400),
        _ => None,
    }
}

pub fn place_of(selector: u8) -> Place {
    match selector {
        0 => Place::Literal,
        5 | 6 => Place::Scratch,
        s => base_of(s).map(Place::Persistent).unwrap_or(Place::None),
    }
}

/// Whether a selector's word access reads two bytes (the even member of each pair, and 7).
pub fn is_word_selector(selector: u8) -> bool {
    matches!(selector, 2 | 4 | 6 | 7 | 12 | 14)
}

/// The field's whole memory: persistent banks and scratch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub banks: Banks,
    pub scratch: [u8; SCRATCH_BYTES],
}

impl Default for Memory {
    fn default() -> Self {
        Memory { banks: Banks::default(), scratch: [0; SCRATCH_BYTES] }
    }
}

impl Memory {
    /// Read one byte of a bank (`selector` != 0).
    pub fn read8(&self, selector: u8, address: u8) -> u8 {
        match place_of(selector) {
            Place::Persistent(base) => self.banks.get(base + usize::from(address)),
            Place::Scratch => self.scratch[usize::from(address)],
            Place::Literal | Place::None => 0,
        }
    }

    pub fn write8(&mut self, selector: u8, address: u8, value: u8) {
        match place_of(selector) {
            Place::Persistent(base) => self.banks.set(base + usize::from(address), value),
            Place::Scratch => self.scratch[usize::from(address)] = value,
            Place::Literal | Place::None => {}
        }
    }

    /// Read a 16-bit value: a word selector reads two bytes, a byte selector one (zero-extended, as the original).
    pub fn read16(&self, selector: u8, address: u8) -> i16 {
        let lo = self.read8(selector, address);
        if !is_word_selector(selector) {
            return i16::from(lo);
        }
        let hi = match place_of(selector) {
            Place::Persistent(base) => self.banks.get(base + usize::from(address) + 1),
            Place::Scratch => self.scratch.get(usize::from(address) + 1).copied().unwrap_or(0),
            _ => 0,
        };
        i16::from_le_bytes([lo, hi])
    }

    pub fn write16(&mut self, selector: u8, address: u8, value: i16) {
        let [lo, hi] = value.to_le_bytes();
        self.write8(selector, address, lo);
        if !is_word_selector(selector) {
            return;
        }
        match place_of(selector) {
            Place::Persistent(base) => self.banks.set(base + usize::from(address) + 1, hi),
            Place::Scratch => {
                if let Some(b) = self.scratch.get_mut(usize::from(address) + 1) {
                    *b = hi;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_and_word_selectors_share_storage() {
        let mut m = Memory::default();
        m.write16(2, 10, 0x1234);
        assert_eq!(m.read8(1, 10), 0x34);
        assert_eq!(m.read8(1, 11), 0x12);
        assert_eq!(m.read16(1, 10), 0x34, "a byte selector reads one byte");
        m.write8(15, 3, 9);
        assert_eq!(m.read8(7, 3), 9);
        assert_eq!(m.banks.get(0x403), 9);
    }

    #[test]
    fn a_word_at_255_continues_into_the_next_bank_but_not_past_scratch() {
        let mut m = Memory::default();
        m.write16(2, 255, 0x0201);
        assert_eq!(m.banks.get(0xFF), 1);
        assert_eq!(m.banks.get(0x100), 2);
        m.write16(6, 255, 0x0201);
        assert_eq!(m.scratch[255], 1);
        assert_eq!(m.read16(6, 255), 1);
    }

    #[test]
    fn banks_round_trip_through_hex() {
        let mut b = Banks::default();
        b.set(0x4FF, 7);
        assert_eq!(Banks::from_hex(&b.to_hex()), Some(b));
        assert_eq!(Banks::from_hex("00"), None);
    }
}
