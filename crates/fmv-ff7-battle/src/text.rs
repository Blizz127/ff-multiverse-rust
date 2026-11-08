//! Basic FF7 text encoding.
//!
//! Source: <https://wiki.ffrtt.ru/index.php/FF7/FF_Text> (Qhimm / Final Fantasy Inside wiki).
//!
//! The English character set stores printable text as `ASCII - 0x20`, so byte `0x00` is a
//! space, `0x01` is `!`, `0x21` is `A` and `0x5E` is `~`. `0xFF` terminates a string.
//! Only that basic range is mapped here; everything else is decoded conservatively:
//!
//! * `0xE7` (`{EOL}` line break) becomes `'\n'`.
//! * `0xFE` (`{FUNC}`, an escape followed by an argument, mostly colour codes) is dropped
//!   together with its single argument byte. APPROXIMATION: some `FUNC` codes carry more than
//!   one argument byte; they do not occur in the battle-name fields this crate reads.
//! * Any other byte (accented Latin range `0x60..=0xDF`, controls, Japanese-encoding data in
//!   test entries) becomes U+FFFD, so callers can detect non-plain-ASCII text with
//!   [`is_plain_ascii`] instead of mistaking it for real names.

/// String terminator byte.
pub const TERMINATOR: u8 = 0xFF;
/// `{FUNC}` escape byte (followed by an argument byte).
pub const FUNC: u8 = 0xFE;
/// `{EOL}` line break byte.
pub const EOL: u8 = 0xE7;

/// Decode FF7 text up to the first terminator (or the end of `bytes`).
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut iter = bytes.iter().copied();
    while let Some(b) = iter.next() {
        match b {
            TERMINATOR => break,
            FUNC => {
                // Escape + one argument byte.
                let _ = iter.next();
            }
            EOL => out.push('\n'),
            0x00..=0x5E => out.push(char::from(b + 0x20)),
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

/// [`decode`], with trailing spaces removed. Many enemy names in `scene.bin` end with an
/// encoded space (`0x00`) before the `0xFF` padding.
pub fn decode_trimmed(bytes: &[u8]) -> String {
    let mut s = decode(bytes);
    let trimmed_len = s.trim_end_matches(' ').len();
    s.truncate(trimmed_len);
    s
}

/// Encode plain ASCII (`' '..='~'`) as FF7 text without a terminator.
/// Returns `None` if `s` contains anything outside that range.
pub fn encode(s: &str) -> Option<Vec<u8>> {
    s.chars()
        .map(|c| match u8::try_from(u32::from(c)) {
            Ok(a @ 0x20..=0x7E) => Some(a - 0x20),
            _ => None,
        })
        .collect()
}

/// True if `s` consists only of printable ASCII (no U+FFFD substitutions, no newlines).
pub fn is_plain_ascii(s: &str) -> bool {
    s.chars().all(|c| matches!(c, ' '..='~'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_mapping_and_terminator() {
        // "Cloud!" with a trailing space and padding.
        let bytes = [0x23, 0x4C, 0x4F, 0x55, 0x44, 0x01, 0x00, 0xFF, 0xFF, 0x23];
        assert_eq!(decode(&bytes), "Cloud! ");
        assert_eq!(decode_trimmed(&bytes), "Cloud!");
        assert_eq!(decode(&[0x00, 0x5E, 0xFF]), " ~");
    }

    #[test]
    fn encode_roundtrip() {
        let enc = encode("Guard Hound 7?").unwrap();
        assert_eq!(decode(&enc), "Guard Hound 7?");
        assert_eq!(encode("caf\u{e9}"), None);
        assert_eq!(encode("tab\t"), None);
    }

    #[test]
    fn specials_are_handled_without_panicking() {
        assert_eq!(decode(&[0x21, 0xE7, 0x22, 0xFF]), "A\nB");
        // FUNC drops its argument byte; a trailing FUNC with no argument is fine too.
        assert_eq!(decode(&[0x21, FUNC, 0xD2, 0x22, 0xFF]), "AB");
        assert_eq!(decode(&[0x21, FUNC]), "A");
        // Unmapped bytes are visibly substituted.
        let s = decode(&[0x70, 0x5F, 0x80, 0xFF]);
        assert_eq!(s, "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert!(!is_plain_ascii(&s));
        assert!(is_plain_ascii("Plain Name 12"));
        assert_eq!(decode(&[]), "");
    }
}
