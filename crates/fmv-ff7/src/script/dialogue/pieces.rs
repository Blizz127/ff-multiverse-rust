//! A dialog string as a window shows it: runs of characters with the layout codes between them.
//!
//! [`parse_message`] reads the raw bytes of one dialog (the byte values and control codes are
//! those of [`crate::script::decode_dialog`], from FFRTT "FF7/FF Text" and "FF7/Field/DialogWindow")
//! and keeps what a window needs: the characters, where a line ends, where a page ends, where a
//! choice line starts and which colour a run is in. Everything else (pauses, waits, the Japanese
//! page selectors) is dropped: a window that does not animate has nothing to do with them.
//!
//! **Canon exactly as written.** The game's name codes (`EA..=F2`, one per playable character)
//! become that character's own default name ([`CHARACTER_NAMES`]) and nothing else: whoever plays
//! is never written into a line that is about Cloud or the rest of the cast. The party codes
//! (`F3..=F5`) become the name of the character the story hook ([`StoryVars::party_member`]) puts in
//! that slot, again by its canon name. Quotation marks and the ellipsis stay the typographic
//! characters the Steam data uses (`“ ” …`); a renderer with a smaller font folds them itself.
//!
//! Measured on the owned Steam copy (counts of bytes in all 56 048 dialogs of the 702 fields):
//! the only bytes of the `0x60..=0xD3` range that occur are `B2` (`“`, 53 823), `B3` (`”`,
//! 53 619), `A9` (`…`, 23 695) and `B4` (`‘`, once); the byte `5F` and the code `E5` never occur.

use super::StoryVars;
use crate::script::glyph;

/// The default names of the nine playable characters, in the order of the name codes `EA..=F2`
/// (character id 0 is Cloud). Constants: the real defaults live in the game's kernel data, which
/// this module does not read (a gap, see the module docs of [`super`]).
pub const CHARACTER_NAMES: [&str; 9] = ["Cloud", "Barret", "Tifa", "Aerith", "Red XIII", "Yuffie", "Cait Sith", "Vincent", "Cid"];

/// The text colours of the escapes `FE D2..=DB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextColour {
    Gray,
    Blue,
    Red,
    Purple,
    Green,
    Cyan,
    Yellow,
    /// The colour every window starts in.
    #[default]
    White,
    Flash,
    Rainbow,
}

impl TextColour {
    /// The colour of the escape `FE <sub>` (`D2..=DB`).
    fn of_escape(sub: u8) -> Option<Self> {
        Some(match sub {
            0xD2 => Self::Gray,
            0xD3 => Self::Blue,
            0xD4 => Self::Red,
            0xD5 => Self::Purple,
            0xD6 => Self::Green,
            0xD7 => Self::Cyan,
            0xD8 => Self::Yellow,
            0xD9 => Self::White,
            0xDA => Self::Flash,
            0xDB => Self::Rainbow,
            _ => return None,
        })
    }
}

/// One element of a message, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Characters (never empty, never a control character).
    Text(String),
    /// `E7`: the next character starts a new line.
    NewLine,
    /// `E8` / `E9`: wait for a button, then show the next page. A page break also restarts the
    /// count of lines a question's choices are numbered by (see [`super::Choice`]).
    NewPage,
    /// `E0`: the cursor column of a choice line (the game draws its hand there).
    Choice,
    /// `FE D2..=DB`: the text that follows is in this colour (until the next colour).
    Colour(TextColour),
}

fn flush(out: &mut Vec<Piece>, text: &mut String) {
    if !text.is_empty() {
        out.push(Piece::Text(std::mem::take(text)));
    }
}

/// Arguments that follow `FE <sub>` (the same table as [`crate::script::decode_dialog`]).
fn escape_args(sub: u8) -> usize {
    match sub {
        0xDD => 2,
        0xE2 => 4,
        _ => 0,
    }
}

/// The pieces of one dialog string: everything up to its terminating `FF` (or the end of
/// `bytes`). Total: every byte string gives some answer, nothing panics and nothing is read past
/// the slice.
pub fn parse_message(bytes: &[u8], vars: &dyn StoryVars) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        i = i.saturating_add(1);
        match b {
            0xFF => break,
            0xE0 => {
                flush(&mut out, &mut text);
                out.push(Piece::Choice);
            }
            // A tab is the one-character indent that lines up the continuation of a quotation.
            0xE1 => text.push(' '),
            0xE2 => text.push_str(", "),
            0xE3 => text.push_str(".\""),
            0xE4 => text.push_str("...\""),
            0xE7 => {
                flush(&mut out, &mut text);
                out.push(Piece::NewLine);
            }
            0xE8 | 0xE9 => {
                flush(&mut out, &mut text);
                out.push(Piece::NewPage);
            }
            0xEA..=0xF2 => {
                if let Some(name) = CHARACTER_NAMES.get(usize::from(b - 0xEA)) {
                    text.push_str(name);
                }
            }
            0xF3..=0xF5 => {
                let member = vars.party_member(b - 0xF3);
                if let Some(name) = member.and_then(|c| CHARACTER_NAMES.get(usize::from(c))) {
                    text.push_str(name);
                }
            }
            // The button glyphs of the PlayStation game, in plain characters.
            0xF6 => text.push_str("(O)"),
            0xF7 => text.push_str("(^)"),
            0xF8 => text.push_str("([])"),
            0xF9 => text.push_str("(X)"),
            // Japanese page selectors: one argument byte, nothing to show.
            0xFA..=0xFD => i = i.saturating_add(1),
            0xFE => {
                let Some(&sub) = bytes.get(i) else { break };
                i = i.saturating_add(1);
                if let Some(colour) = TextColour::of_escape(sub) {
                    flush(&mut out, &mut text);
                    out.push(Piece::Colour(colour));
                } else if matches!(sub, 0xDE | 0xDF | 0xE1) {
                    // A number the script sets with `MPARA`/`WNUMB`: not tracked, so nought.
                    text.push('0');
                }
                i = i.saturating_add(escape_args(sub));
            }
            plain => {
                if let Some(c) = glyph(plain) {
                    text.push(c);
                }
            }
        }
    }
    flush(&mut out, &mut text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::dialogue::StartOfGame;

    fn enc(s: &str) -> Vec<u8> {
        s.bytes().map(|b| b - 0x20).collect()
    }

    fn parse(bytes: &[u8]) -> Vec<Piece> {
        parse_message(bytes, &StartOfGame)
    }

    #[test]
    fn plain_text_lines_and_pages() {
        let mut b = enc("Biggs");
        b.push(0xE7);
        b.extend(enc("Hello"));
        b.push(0xE8);
        b.extend(enc("Bye"));
        b.push(0xFF);
        b.extend(enc("never read"));
        assert_eq!(
            parse(&b),
            [Piece::Text("Biggs".into()), Piece::NewLine, Piece::Text("Hello".into()), Piece::NewPage, Piece::Text("Bye".into())]
        );
        assert_eq!(parse(&[]), Vec::<Piece>::new());
        assert_eq!(parse(&[0xFF]), Vec::<Piece>::new());
    }

    #[test]
    fn the_steam_quotes_the_ellipsis_and_the_tab_are_what_the_data_uses() {
        // “ ” … are B2 B3 A9; a tab (E1) is one indent character; E2 is ", ".
        let b = [0xB2, 0x21, 0xE2, 0x22, 0xB3, 0xE7, 0xE1, 0xA9, 0x23, 0xFF];
        assert_eq!(parse(&b), [Piece::Text("\u{201c}A, B\u{201d}".into()), Piece::NewLine, Piece::Text(" \u{2026}C".into())]);
    }

    #[test]
    fn colours_choices_and_dropped_codes() {
        let mut b = vec![0xE0];
        b.extend(enc("Yes"));
        b.extend([0xFE, 0xD4]);
        b.extend(enc("red"));
        // FE DD takes two argument bytes, one of which is the terminator byte: data, not the end.
        b.extend([0xFE, 0xDD, 0xFF, 0x01, 0xFE, 0xFF]);
        b.extend(enc("x"));
        b.push(0xFF);
        // The wait and the pause (FE FF) are dropped and neither ends the text: "red" and "x" run on.
        assert_eq!(
            parse(&b),
            [Piece::Choice, Piece::Text("Yes".into()), Piece::Colour(TextColour::Red), Piece::Text("redx".into())]
        );
    }

    #[test]
    fn names_are_the_canon_defaults_and_never_anyone_else() {
        // {CLOUD} {BARRET} {CID}, then the three party slots.
        let b = [0xEA, 0x00, 0xEB, 0x00, 0xF2, 0x00, 0xF3, 0x00, 0xF4, 0x00, 0xF5, 0xFF];
        let pieces = parse(&b);
        assert_eq!(pieces, [Piece::Text("Cloud Barret Cid Cloud Barret ".into())], "slot 3 is empty in the default party");
        struct Odd;
        impl StoryVars for Odd {
            fn byte(&self, _: u8, _: u8) -> u8 {
                0
            }
            fn party_member(&self, slot: u8) -> Option<u8> {
                [Some(2), Some(4), Some(200)].get(usize::from(slot)).copied().flatten()
            }
        }
        // An id with no canon name writes nothing rather than inventing one.
        assert_eq!(parse_message(&[0xF3, 0x00, 0xF4, 0x00, 0xF5, 0xFF], &Odd), [Piece::Text("Tifa Red XIII ".into())]);
    }

    #[test]
    fn every_byte_every_truncation_and_filler_never_panics() {
        for b in 0..=255u8 {
            for tail in [&[][..], &[0xFF][..], &[0xFE][..], &[0xFE, 0xDD][..], &[0xFE, 0xE2, 1, 2, 3][..], &[0xFA][..]] {
                let mut bytes = vec![b];
                bytes.extend_from_slice(tail);
                let _ = parse(&bytes);
            }
            let _ = parse(&[0xFE, b, b, b, b, b, b, 0xFF]);
        }
    }

    #[test]
    fn no_piece_is_empty_and_text_has_no_control_characters() {
        let mut state = 0x1234_5678u32;
        for _ in 0..2000 {
            let len = {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as usize
            };
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            for p in parse(&bytes) {
                if let Piece::Text(t) = p {
                    assert!(!t.is_empty());
                    assert!(t.chars().all(|c| !c.is_control()), "{t:?}");
                }
            }
        }
    }
}
