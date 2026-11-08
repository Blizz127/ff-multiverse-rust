//! The local player's own character progress (protocol v5): the level, experience, AP and gil the
//! server says its character has, mirrored from [`ServerMsg::CharacterProgress`].
//!
//! The server keeps and stores it and decides every number; this only remembers the last message,
//! says what changed as one-line notes (`progress:` on stderr in a headless run) and offers the HUD its
//! line and the SMOKE summary its `level=` and `exp=`. Nothing here is a game datum: the numbers are
//! the player's own, and the name shown for a character id comes from this client's own table
//! (`fmv_ff7_battle::kernel::CharacterId`), never from the server's text. A client that never hears a
//! `CharacterProgress` (a server without battle data, such as the FIXTURE one, sends none) shows no
//! progress and reports `level=0 exp=0`; 0 is not a level.

use std::collections::VecDeque;

use fmv_ff7_battle::kernel::{CharacterId, MAX_LEVEL};
use fmv_protocol::ServerMsg;

/// How long the HUD keeps saying LEVEL UP after the message that reported a level gained (seconds).
pub const LEVEL_UP_SHOWN_S: f64 = 8.0;
/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 32;

/// What the server last said about our character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressView {
    /// The game's character id (FF7: `CharacterId::index`).
    pub character: u8,
    pub level: u8,
    /// Total experience.
    pub exp: u32,
    /// Experience still missing for the next level (0 at the highest level, or when unknown).
    pub exp_to_next: u32,
    pub gil: u32,
    pub ap: u32,
}

impl ProgressView {
    /// The character's name from this client's own table, or a neutral word for an id it has no name
    /// for (never text from the network).
    pub fn character_name(&self) -> &'static str {
        CharacterId::from_index(usize::from(self.character)).map_or("Character", CharacterId::name)
    }

    /// The HUD line: `Cloud Lv 7  EXP 626/949  Gil 20`. The experience shows as `have/next level`;
    /// at the highest level (or when the server gave no next threshold) as the plain total.
    pub fn hud_text(&self) -> String {
        let exp = if self.exp_to_next > 0 {
            format!("EXP {}/{}", self.exp, self.exp.saturating_add(self.exp_to_next))
        } else if self.level >= MAX_LEVEL {
            format!("EXP {} MAX", self.exp)
        } else {
            format!("EXP {}", self.exp)
        };
        format!("{} Lv {}  {exp}  Gil {}", self.character_name(), self.level, self.gil)
    }
}

/// The mirror: the last `CharacterProgress` and what to say about it.
#[derive(Debug, Default)]
pub struct ProgressMirror {
    current: Option<ProgressView>,
    /// `CharacterProgress` messages received.
    updates: u64,
    /// Of those, the ones that reported a level gained.
    level_ups: u64,
    /// Local clock (seconds) at the last message that reported a level gained.
    leveled_up_at: Option<f64>,
    notes: VecDeque<String>,
}

impl ProgressMirror {
    /// Apply a `CharacterProgress`; anything else is ignored. `now` is seconds on the local clock.
    pub fn on_message(&mut self, now: f64, msg: ServerMsg) {
        let ServerMsg::CharacterProgress { character, level, exp, exp_to_next, gil, ap, leveled_up } = msg else { return };
        let view = ProgressView { character, level, exp, exp_to_next, gil, ap };
        let before = self.current.replace(view);
        self.updates += 1;
        if leveled_up {
            self.level_ups += 1;
            self.leveled_up_at = Some(now);
        }
        // `key=value` layout: `scripts/battle-smoke.sh --progress` reads it.
        let was = match before {
            Some(old) if old.level != level => format!(" (was level {})", old.level),
            Some(_) => String::new(),
            None => " (on joining)".to_string(),
        };
        self.note(format!(
            "{} level={} exp={} exp_to_next={} gil={} ap={} leveled_up={leveled_up}{was}",
            view.character_name(),
            view.level,
            view.exp,
            view.exp_to_next,
            view.gil,
            view.ap,
        ));
    }

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }

    /// The last thing the server said, if it said anything.
    pub fn current(&self) -> Option<&ProgressView> {
        self.current.as_ref()
    }

    /// Our level, 0 until the server has said (see the module docs).
    pub fn level(&self) -> u32 {
        self.current.map_or(0, |p| u32::from(p.level))
    }

    /// Our total experience, 0 until the server has said.
    pub fn exp(&self) -> u32 {
        self.current.map_or(0, |p| p.exp)
    }

    /// `CharacterProgress` messages received.
    #[cfg(test)]
    pub fn updates(&self) -> u64 {
        self.updates
    }

    /// Messages that reported a level gained.
    #[cfg(test)]
    pub fn level_ups(&self) -> u64 {
        self.level_ups
    }

    /// Did the last report of a level gained come less than [`LEVEL_UP_SHOWN_S`] seconds before `now`?
    pub fn level_up_shown(&self, now: f64) -> bool {
        self.leveled_up_at.is_some_and(|at| now >= at && now - at < LEVEL_UP_SHOWN_S)
    }

    /// Take the notes recorded since the last call, oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(character: u8, level: u8, exp: u32, exp_to_next: u32, gil: u32, ap: u32, leveled_up: bool) -> ServerMsg {
        ServerMsg::CharacterProgress { character, level, exp, exp_to_next, gil, ap, leveled_up }
    }

    #[test]
    fn nothing_is_known_until_the_server_says() {
        let m = ProgressMirror::default();
        assert!(m.current().is_none());
        assert_eq!((m.level(), m.exp(), m.updates(), m.level_ups()), (0, 0, 0, 0));
        assert!(!m.level_up_shown(0.0));
    }

    #[test]
    fn the_last_message_is_what_we_have_and_it_is_noted_in_a_parseable_line() {
        let mut m = ProgressMirror::default();
        m.on_message(1.0, msg(0, 6, 610, 6, 0, 0, false));
        assert_eq!((m.level(), m.exp()), (6, 610));
        m.on_message(9.0, msg(0, 7, 626, 323, 20, 4, true));
        assert_eq!(*m.current().unwrap(), ProgressView { character: 0, level: 7, exp: 626, exp_to_next: 323, gil: 20, ap: 4 });
        assert_eq!((m.updates(), m.level_ups()), (2, 1));
        assert_eq!(
            m.drain_notes(),
            [
                "Cloud level=6 exp=610 exp_to_next=6 gil=0 ap=0 leveled_up=false (on joining)",
                "Cloud level=7 exp=626 exp_to_next=323 gil=20 ap=4 leveled_up=true (was level 6)",
            ]
        );
        assert!(m.drain_notes().is_empty(), "notes are taken once");
        // Without a level gained there is no "was level".
        m.on_message(10.0, msg(0, 7, 640, 309, 30, 6, false));
        assert_eq!(m.drain_notes(), ["Cloud level=7 exp=640 exp_to_next=309 gil=30 ap=6 leveled_up=false"]);
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut m = ProgressMirror::default();
        m.on_message(0.0, ServerMsg::Pong { nonce: 1, tick: 2 });
        assert!(m.current().is_none() && m.drain_notes().is_empty());
    }

    #[test]
    fn the_hud_line_reads_like_the_games_own() {
        let view = |level, exp, exp_to_next, gil| ProgressView { character: 0, level, exp, exp_to_next, gil, ap: 3 };
        assert_eq!(view(7, 1234, 266, 250).hud_text(), "Cloud Lv 7  EXP 1234/1500  Gil 250");
        // Another character, named from our own table (Aerith, not the wire's text).
        assert_eq!(ProgressView { character: 3, ..view(2, 6, 27, 0) }.hud_text(), "Aerith Lv 2  EXP 6/33  Gil 0");
        // The top: nothing is next. A level below the top with nothing next (no curve) shows the total.
        assert_eq!(view(99, 2_408_997, 0, 9_999_999).hud_text(), "Cloud Lv 99  EXP 2408997 MAX  Gil 9999999");
        assert_eq!(view(7, 626, 0, 20).hud_text(), "Cloud Lv 7  EXP 626  Gil 20");
        // An id this client has no name for is shown neutrally, and the numbers never overflow.
        assert_eq!(ProgressView { character: 200, ..view(7, u32::MAX, u32::MAX, 1) }.hud_text(), "Character Lv 7  EXP 4294967295/4294967295  Gil 1");
    }

    #[test]
    fn level_up_is_shown_for_a_while_after_the_message_that_reported_it() {
        let mut m = ProgressMirror::default();
        m.on_message(100.0, msg(0, 7, 626, 323, 20, 4, true));
        assert!(m.level_up_shown(100.0) && m.level_up_shown(107.9));
        assert!(!m.level_up_shown(108.0), "then it goes");
        assert!(!m.level_up_shown(99.0), "not before it happened");
        // A later message without a level gained does not bring it back or end it early.
        m.on_message(101.0, msg(0, 7, 640, 309, 30, 6, false));
        assert!(m.level_up_shown(102.0));
        // The notes are bounded.
        for i in 0..100 {
            m.on_message(200.0 + f64::from(i), msg(0, 7, 640 + i as u32, 1, 0, 0, false));
        }
        assert_eq!(m.drain_notes().len(), MAX_NOTES);
    }
}
