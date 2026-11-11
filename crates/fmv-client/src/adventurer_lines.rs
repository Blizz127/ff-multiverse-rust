//! The adventurer's voice (owner, 2026-11-11: "minor lines only for now"): the player's own character speaks some of
//! the minor characters' lines, so it takes part in the party's talk instead of standing by as a third wheel.
//!
//! The rule is presentation only (the story, the scripts and every other player's text are unchanged):
//!
//! * only lines whose window starts with a minor speaker's name line ([`SHARED_SPEAKERS`]: the AVALANCHE members Biggs,
//!   Wedge and Jessie) can be shared; Cloud, Barret, Tifa, Aerith and every other main character keep all of theirs;
//! * never a question (a window with choices stays with whoever asks it);
//! * about one line in three, picked by the field and the dialog's number, so a line is always the same speaker's, on
//!   every machine and every replay;
//! * `FMV_ADVENTURER_LINES=off` turns it off.
//!
//! Fallbacks the owner named if this plays badly: original lines, or a silent adventurer who acts instead of talking.

use std::sync::Mutex;

use fmv_ff7::script::dialogue::{Message, MessageOrigin, Piece};

/// Minor speakers whose lines the adventurer may take.
pub const SHARED_SPEAKERS: [&str; 3] = ["Biggs", "Wedge", "Jessie"];
/// One line in this many is the adventurer's.
const ONE_IN: u64 = 3;

struct Voice {
    name: String,
    field: String,
}

static VOICE: Mutex<Option<Voice>> = Mutex::new(None);

fn enabled() -> bool {
    !matches!(std::env::var("FMV_ADVENTURER_LINES").as_deref(), Ok("off" | "0" | "no"))
}

/// Who the adventurer is (the player's character name).
pub fn set_name(name: &str) {
    let mut v = VOICE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let field = v.as_ref().map(|v| v.field.clone()).unwrap_or_default();
    *v = Some(Voice { name: name.to_string(), field });
}

/// The field the player is in (the lines are picked per field).
pub fn set_field(field: &str) {
    let mut v = VOICE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match v.as_mut() {
        Some(voice) => voice.field = field.to_string(),
        None => *v = Some(Voice { name: String::new(), field: field.to_string() }),
    }
}

/// Give `message` to the adventurer when the rule picks it. Returns whether it did.
pub fn adopt(message: &mut Message) -> bool {
    if !enabled() {
        return false;
    }
    let v = VOICE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(voice) = v.as_ref().filter(|v| !v.name.is_empty()) else { return false };
    adopt_as(message, &voice.name, &voice.field)
}

/// [`adopt`] with an explicit name and field (the pure rule).
pub fn adopt_as(message: &mut Message, adventurer: &str, field: &str) -> bool {
    let MessageOrigin::Canon { dialog, .. } = message.origin else { return false };
    if message.choice.is_some() {
        return false;
    }
    let speaker = match message.pieces.as_slice() {
        [Piece::Text(name), Piece::NewLine, ..] => name.trim(),
        _ => return false,
    };
    if !SHARED_SPEAKERS.iter().any(|s| s.eq_ignore_ascii_case(speaker)) {
        return false;
    }
    if pick(field, dialog) % ONE_IN != 0 {
        return false;
    }
    if let Some(Piece::Text(name)) = message.pieces.first_mut() {
        *name = adventurer.to_string();
    }
    true
}

/// A stable number for a line: FNV-1a over the field's key and the dialog's number.
fn pick(field: &str, dialog: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in field.to_ascii_lowercase().bytes().chain((dialog as u64).to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(speaker: &str, dialog: usize) -> Message {
        Message::new(MessageOrigin::Canon { entity: 1, dialog }, vec![Piece::Text(speaker.into()), Piece::NewLine, Piece::Text("\u{201c}Hi.\u{201d}".into())])
    }

    #[test]
    fn about_a_third_of_the_minor_lines_and_none_of_the_main_casts_are_shared() {
        let shared = (0..300).filter(|d| adopt_as(&mut line("Biggs", *d), "Ayla", "md1_1")).count();
        assert!((60..=140).contains(&shared), "{shared}");
        assert!((0..300).all(|d| !adopt_as(&mut line("Barret", d), "Ayla", "md1_1")));
        assert!((0..300).all(|d| !adopt_as(&mut line("Cloud", d), "Ayla", "md1_1")));
    }

    #[test]
    fn a_shared_line_names_the_adventurer_and_is_always_the_same_line() {
        let d = (0..100).find(|d| adopt_as(&mut line("Wedge", *d), "Ayla", "nmkin_1")).expect("one is shared");
        let mut m = line("Wedge", d);
        assert!(adopt_as(&mut m, "Ayla", "nmkin_1"));
        assert_eq!(m.pieces.first(), Some(&Piece::Text("Ayla".into())));
        let mut ask = line("Wedge", d);
        ask.choice = Some(fmv_ff7::script::dialogue::Choice { first: 1, last: 2, cursor: 1 });
        assert!(!adopt_as(&mut ask, "Ayla", "nmkin_1"), "questions stay with whoever asks");
    }
}
