//! FF7 field dialog text: byte string -> readable text.
//!
//! Sources (layout facts only; no code copied):
//!
//! * FFRTT "FF7/FF Text" <https://wiki.ffrtt.ru/index.php/FF7/FF_Text> (also mirrored at
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Text_encoding.html>): bytes `0x00..=0x5E` are
//!   ASCII minus `0x20`, `0x60..=0xD3` are accented letters and symbols, `0xE0..=0xFF` are control
//!   codes, `0xFF` ends a string.
//! * FFRTT "FF7/Field/DialogWindow" <https://wiki.ffrtt.ru/index.php/FF7/Field/DialogWindow>:
//!   how many argument bytes each control code takes (`0xFA..=0xFD` one, `FE DD` two, `FE E2`
//!   four, every other code none) and what the `FE xx` escapes do.
//!
//! The decoder is total: every byte string decodes to *some* text (unknown bytes become
//! `{XX}`), it never panics and never reads past the slice it is given. Control codes are shown
//! as `{NAME}` markers (`{NEWPAGE}`, `{CLOUD}`, `{GRAY}`, ...); a line break is a real `'\n'`.

/// What a dialog string's decoding consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText {
    /// Readable text, with control codes rendered as `{...}` markers.
    pub text: String,
    /// Bytes consumed, including the terminating `0xFF` when there is one.
    pub consumed: usize,
    /// Whether a terminating `0xFF` was found before the input ended.
    pub terminated: bool,
}

/// Byte that terminates a dialog string.
pub const DIALOG_END: u8 = 0xFF;

/// Glyphs for `0x60..=0xD3` (`None` = documented as "nothing").
const HIGH_GLYPHS: [Option<char>; 0x74] = {
    const fn c(ch: char) -> Option<char> {
        Some(ch)
    }
    [
        // 0x60
        c('Ä'), c('Á'), c('Ç'), c('É'), c('Ñ'), c('Ö'), c('Ü'), c('á'), c('à'), c('â'), c('ä'), c('ã'), c('å'), c('ç'), c('é'), c('è'),
        // 0x70
        c('ê'), c('ë'), c('í'), c('ì'), c('î'), c('ï'), c('ñ'), c('ó'), c('ò'), c('ô'), c('ö'), c('õ'), c('ú'), c('ù'), c('û'), c('ü'),
        // 0x80
        c('⌘'), c('°'), c('¢'), c('£'), c('Ù'), c('Û'), c('¶'), c('ß'), c('®'), c('©'), c('™'), c('´'), c('¨'), c('≠'), c('Æ'), c('Ø'),
        // 0x90
        c('∞'), c('±'), c('≤'), c('≥'), c('¥'), c('µ'), c('∂'), c('Σ'), c('Π'), c('π'), c('⌡'), c('ª'), c('º'), c('Ω'), c('æ'), c('ø'),
        // 0xA0
        c('¿'), c('¡'), c('¬'), c('√'), c('ƒ'), c('≈'), c('∆'), c('«'), c('»'), c('…'), None, c('À'), c('Ã'), c('Õ'), c('Œ'), c('œ'),
        // 0xB0
        c('–'), c('—'), c('“'), c('”'), c('‘'), c('’'), c('÷'), c('◊'), c('ÿ'), c('Ÿ'), c('⁄'), c('¤'), c('‹'), c('›'), c('ﬁ'), c('ﬂ'),
        // 0xC0
        c('■'), c('▪'), c('‚'), c('„'), c('‰'), c('Â'), c('Ê'), c('Á'), c('Ë'), c('È'), c('Í'), c('Î'), c('Ï'), c('Ì'), c('Ó'), c('Ô'),
        // 0xD0
        c(' '), c('Ò'), c('Ù'), c('Û'),
    ]
};

/// First byte covered by [`HIGH_GLYPHS`].
const HIGH_BASE: u8 = 0x60;

/// Names of the colour escapes `FE D2..=DB`.
const COLOURS: [&str; 10] = ["GRAY", "BLUE", "RED", "PURPLE", "GREEN", "CYAN", "YELLOW", "WHITE", "FLASH", "RAINBOW"];

/// Names of the character-name codes `EA..=F2` (savemap characters 0..=8).
const CHARACTER_NAMES: [&str; 9] = ["CLOUD", "BARRET", "TIFA", "AERITH", "REDXIII", "YUFFIE", "CAITSITH", "VINCENT", "CID"];

/// Number of argument bytes that follow a top-level control byte.
fn top_level_args(b: u8) -> usize {
    match b {
        0xFA..=0xFD => 1,
        _ => 0,
    }
}

/// Number of argument bytes that follow `FE <sub>`.
fn escape_args(sub: u8) -> usize {
    match sub {
        0xDD => 2,
        0xE2 => 4,
        _ => 0,
    }
}

/// Decode one dialog string starting at `bytes[0]`, stopping after the terminating `0xFF`.
///
/// If no terminator exists, the whole slice is decoded and `terminated` is `false`.
pub fn decode_dialog(bytes: &[u8]) -> DecodedText {
    let mut text = String::new();
    let mut i = 0usize;
    let mut terminated = false;
    while let Some(&b) = bytes.get(i) {
        i += 1;
        match b {
            DIALOG_END => {
                terminated = true;
                break;
            }
            0x00..=0x5E => text.push(char::from(b + 0x20)),
            0x5F => text.push_str("{5F}"),
            HIGH_BASE..=0xD3 => match HIGH_GLYPHS.get(usize::from(b - HIGH_BASE)).copied().flatten() {
                Some(ch) => text.push(ch),
                None => push_code(&mut text, b),
            },
            0xE0 => text.push_str("{CHOICE}"),
            0xE1 => text.push_str("{TAB}"),
            0xE2 => text.push_str(", "),
            0xE3 => text.push_str(".\""),
            0xE4 => text.push_str("...\""),
            0xE7 => text.push('\n'),
            0xE8 => text.push_str("{NEWPAGE}"),
            0xE9 => text.push_str("{NEWPAGE2}"),
            0xEA..=0xF2 => match CHARACTER_NAMES.get(usize::from(b - 0xEA)) {
                Some(name) => {
                    text.push('{');
                    text.push_str(name);
                    text.push('}');
                }
                None => push_code(&mut text, b),
            },
            0xF3..=0xF5 => {
                text.push_str("{PARTY");
                text.push(char::from(b - 0xF3 + b'1'));
                text.push('}');
            }
            0xF6 => text.push_str("{CIRCLE}"),
            0xF7 => text.push_str("{TRIANGLE}"),
            0xF8 => text.push_str("{SQUARE}"),
            0xF9 => text.push_str("{CROSS}"),
            0xFA..=0xFD => {
                push_code(&mut text, b);
                i += skip_args(bytes, i, top_level_args(b), &mut text);
            }
            0xFE => {
                let Some(&sub) = bytes.get(i) else {
                    text.push_str("{FE}");
                    break;
                };
                i += 1;
                push_escape(&mut text, sub);
                i += skip_args(bytes, i, escape_args(sub), &mut text);
            }
            _ => push_code(&mut text, b),
        }
    }
    DecodedText { text, consumed: i, terminated }
}

/// Render `n` argument bytes (if present) as `{XX}` markers after their control code and return
/// how many were actually available.
fn skip_args(bytes: &[u8], at: usize, n: usize, text: &mut String) -> usize {
    let mut taken = 0usize;
    for k in 0..n {
        match at.checked_add(k).and_then(|p| bytes.get(p)) {
            Some(&arg) => {
                push_code(text, arg);
                taken += 1;
            }
            None => break,
        }
    }
    taken
}

fn push_code(text: &mut String, b: u8) {
    let digit = |v: u8| char::from_digit(u32::from(v), 16).map_or('?', |c| c.to_ascii_uppercase());
    text.push('{');
    text.push(digit(b >> 4));
    text.push(digit(b & 0x0F));
    text.push('}');
}

fn push_escape(text: &mut String, sub: u8) {
    match sub {
        0xD2..=0xDB => {
            let name = COLOURS.get(usize::from(sub - 0xD2)).copied().unwrap_or("?");
            text.push('{');
            text.push_str(name);
            text.push('}');
        }
        0xDC | 0xFF => text.push_str("{PAUSE}"),
        0xDD => text.push_str("{WAIT}"),
        0xDE => text.push_str("{VAR}"),
        0xDF => text.push_str("{HEXVAR}"),
        0xE0 => text.push_str("{SCROLL}"),
        0xE1 => text.push_str("{PADVAR}"),
        0xE2 => text.push_str("{MEMORY}"),
        0xE9 => text.push_str("{WIDTHFIX}"),
        other => {
            text.push_str("{FE");
            push_code(text, other);
            text.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode ASCII the way the game does (test helper; synthetic text only).
    fn enc(s: &str) -> Vec<u8> {
        s.bytes().map(|b| b - 0x20).collect()
    }

    #[test]
    fn ascii_is_shifted_by_0x20() {
        let mut bytes = enc("Hello, World 42!");
        bytes.push(DIALOG_END);
        let d = decode_dialog(&bytes);
        assert_eq!(d.text, "Hello, World 42!");
        assert!(d.terminated);
        assert_eq!(d.consumed, bytes.len());
    }

    #[test]
    fn stops_at_the_first_terminator_and_reports_consumed() {
        let mut bytes = enc("ab");
        bytes.push(DIALOG_END);
        bytes.extend_from_slice(&enc("cd"));
        let d = decode_dialog(&bytes);
        assert_eq!((d.text.as_str(), d.consumed, d.terminated), ("ab", 3, true));
    }

    #[test]
    fn control_codes_become_markers() {
        let bytes = [0x21, 0xE7, 0x22, 0xE8, 0xEA, 0xF3, 0xFE, 0xD4, 0x23, 0xFF];
        let d = decode_dialog(&bytes);
        assert_eq!(d.text, "A\nB{NEWPAGE}{CLOUD}{PARTY1}{RED}C");
        assert!(d.terminated);
    }

    #[test]
    fn escape_arguments_may_contain_the_terminator_byte() {
        // FE DD takes two argument bytes; an 0xFF inside them is data, not the end.
        let bytes = [0xFE, 0xDD, 0xFF, 0x01, 0x21, 0xFF];
        let d = decode_dialog(&bytes);
        assert_eq!(d.consumed, bytes.len());
        assert!(d.terminated);
        assert!(d.text.starts_with("{WAIT}{FF}{01}A"));
        // FE FF is a pause, not an end.
        let d = decode_dialog(&[0xFE, 0xFF, 0x21, 0xFF]);
        assert_eq!(d.text, "{PAUSE}A");
        assert!(d.terminated);
    }

    #[test]
    fn unterminated_and_truncated_input_never_panics() {
        assert_eq!(decode_dialog(&[]), DecodedText { text: String::new(), consumed: 0, terminated: false });
        let d = decode_dialog(&[0x21, 0x22]);
        assert_eq!((d.text.as_str(), d.consumed, d.terminated), ("AB", 2, false));
        // escape with missing argument bytes
        let d = decode_dialog(&[0xFE]);
        assert!(!d.terminated);
        let d = decode_dialog(&[0xFE, 0xE2, 0x01]);
        assert_eq!(d.consumed, 3);
        assert!(!d.terminated);
        let d = decode_dialog(&[0xFA]);
        assert!(!d.terminated);
    }

    #[test]
    fn every_byte_value_decodes() {
        for b in 0..=255u8 {
            let d = decode_dialog(&[b, DIALOG_END, DIALOG_END, DIALOG_END, DIALOG_END, DIALOG_END, DIALOG_END]);
            assert!(d.consumed >= 1);
        }
    }

    #[test]
    fn accented_range_and_unmapped_bytes() {
        assert_eq!(decode_dialog(&[0x60, 0xFF]).text, "\u{c4}");
        assert_eq!(decode_dialog(&[0xA9, 0xFF]).text, "\u{2026}");
        assert_eq!(decode_dialog(&[0xAA, 0xFF]).text, "{AA}");
        assert_eq!(decode_dialog(&[0xD8, 0xFF]).text, "{D8}");
        assert_eq!(decode_dialog(&[0x5F, 0xFF]).text, "{5F}");
    }
}
