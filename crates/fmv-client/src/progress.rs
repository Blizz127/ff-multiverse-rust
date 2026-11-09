//! The local player's own character progress (protocol v5): the level, experience, AP and gil the
//! server says its character has, mirrored from [`ServerMsg::CharacterProgress`].
//!
//! The server keeps and stores it and decides every number; this only remembers the last message,
//! says what changed as one-line notes (`progress:` on stderr in a headless run) and offers the HUD its
//! line and the SMOKE summary its `level=` and `exp=`. Nothing here is a game datum: the numbers are
//! the player's own. The progress is the player's own character's, which fights in a canon party member's
//! combat SLOT (protocol v8): the lines say who the player is (its own name, which the mirror is told by
//! [`ProgressMirror::set_player`] when the session is built) and, in brackets, which slot it fights in (the
//! slot's name comes from this client's own table, `fmv_ff7_battle::kernel::CharacterId`, never from the
//! server's text): `Wedge (Barret)  Lv 6  EXP 82/84  Gil 25`. A client that never hears a
//! `CharacterProgress` (a server without battle data, such as the FIXTURE one, sends none) shows no
//! progress and reports `level=0 exp=0`; 0 is not a level.

use std::collections::VecDeque;

use crate::headless::encode_name;
use fmv_ff7_battle::kernel::{CharacterId, MAX_LEVEL};
use fmv_protocol::ServerMsg;

/// How long the HUD keeps saying LEVEL UP after the message that reported a level gained (seconds).
pub const LEVEL_UP_SHOWN_S: f64 = 8.0;
/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 32;

/// The name of the stat curve `slot` (FF7: the canon character's id whose growth, spells and template the character
/// fights with) from this client's own table, or `None` for an id it has no name for (never text from the network).
/// **Internal**: it is what the logs and the smoke scripts say; a player is never shown it as who it fights as, see
/// [`FightingStyle`].
pub fn slot_name(slot: u8) -> Option<&'static str> {
    CharacterId::from_index(usize::from(slot)).map(CharacterId::name)
}

/// A FIGHTING STYLE: what a character's stat curve (`NewCharacter::slot`, `BattleMember::character`, `CharacterProgress::character`
/// in the protocol, which keeps those names) is called to the player. The curve is the growth, spells and combat template of a
/// canon character of the game, chosen once at creation; it says nothing about who the player is, and the canon
/// party fights beside the player's character as server-controlled companions. So the screens never say "who you fight as" and
/// never show the canon character's name for a player: they show the style, whose labels name no canon character. The
/// mapping (documented here, the one place it lives):
///
/// | curve | canon character the stats come from | style |
/// |---|---|---|
/// | 0 | Cloud (never offered to a new character; accounts that predate the creator keep it) | Swordsman |
/// | 1 | Barret | Gunner |
/// | 2 | Tifa | Martial artist |
/// | 3 | Aerith | Mage |
/// | 4 | Red XIII | Guardian |
/// | 5 | Yuffie | Ninja |
/// | 6 | Cait Sith | Gambler |
/// | 7 | Vincent | Marksman |
/// | 8 | Cid | Lancer |
///
/// A style's blurb is one short line of what the curve is good at (its stats, in words of ours).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FightingStyle {
    /// The game's own id of the curve (FF7: the canon character's): what the protocol carries. Internal.
    pub curve: u8,
    /// What the player is shown.
    pub label: &'static str,
    /// One line about the curve, for the creator.
    pub blurb: &'static str,
}

/// Every FF7 fighting style, by curve id (see [`FightingStyle`]).
pub const FF7_FIGHTING_STYLES: [FightingStyle; 9] = [
    FightingStyle { curve: 0, label: "Swordsman", blurb: "a balanced all-rounder" },
    FightingStyle { curve: 1, label: "Gunner", blurb: "the sturdiest and strongest, but slow" },
    FightingStyle { curve: 2, label: "Martial artist", blurb: "quick and strong with the fists" },
    FightingStyle { curve: 3, label: "Mage", blurb: "frail, but a master of magic" },
    FightingStyle { curve: 4, label: "Guardian", blurb: "tough and swift, with a strong spirit" },
    FightingStyle { curve: 5, label: "Ninja", blurb: "very fast and very lucky" },
    FightingStyle { curve: 6, label: "Gambler", blurb: "unpredictable, lives on luck" },
    FightingStyle { curve: 7, label: "Marksman", blurb: "quick, deadly at range and with magic" },
    FightingStyle { curve: 8, label: "Lancer", blurb: "strong and sturdy, a hard hitter" },
];

/// The fighting style of curve `curve`, or `None` for an id this client has no style for (never text from the network).
pub fn fighting_style(curve: u8) -> Option<&'static FightingStyle> {
    FF7_FIGHTING_STYLES.iter().find(|s| s.curve == curve)
}

/// What the server last said about our character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressView {
    /// The game's id of the stat curve the character fights with (FF7: `CharacterId::index`; shown as its fighting style).
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
    /// The fighting style from this client's own table ([`FightingStyle`]: never a canon character's name), or a
    /// neutral word for a curve it has no style for (never text from the network).
    pub fn style_name(&self) -> &'static str {
        fighting_style(self.character).map_or("Fighter", |s| s.label)
    }

    /// The HUD line: `<PlayerName> (<Style>)  Lv N  EXP a/b  Gil g`, for instance
    /// `Wedge (Gunner)  Lv 6  EXP 82/84  Gil 25`: who the player is, its fighting style, then the numbers.
    /// The experience shows as `have/next level`; at the highest level (or when the server gave no next
    /// threshold) as the plain total. Without a player name (the mirror was never told one) the style stands alone.
    pub fn hud_text(&self, player: &str) -> String {
        let exp = if self.exp_to_next > 0 {
            format!("EXP {}/{}", self.exp, self.exp.saturating_add(self.exp_to_next))
        } else if self.level >= MAX_LEVEL {
            format!("EXP {} MAX", self.exp)
        } else {
            format!("EXP {}", self.exp)
        };
        let who = if player.is_empty() { self.style_name().to_string() } else { format!("{player} ({})", self.style_name()) };
        format!("{who}  Lv {}  {exp}  Gil {}", self.level, self.gil)
    }
}

/// The mirror: the last `CharacterProgress` and what to say about it.
#[derive(Debug, Default)]
pub struct ProgressMirror {
    /// The player's own name (cleaned), which the lines say; empty until [`ProgressMirror::set_player`].
    player: String,
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
    /// Tell the mirror who the player is (its own character's name): the session does it when it is built,
    /// before the messages of the handshake are applied, so that the progress sent on joining is noted
    /// with the name too. The text is cleaned like any other that comes from the network.
    pub fn set_player(&mut self, name: &str) {
        self.player = crate::net::clean(name).trim().to_string();
    }

    /// The player's own name, empty if the mirror was never told.
    #[cfg(test)]
    pub fn player(&self) -> &str {
        &self.player
    }

    /// The HUD line for the last thing the server said ([`ProgressView::hud_text`] with the player's name).
    pub fn hud_text(&self) -> Option<String> {
        self.current.as_ref().map(|view| view.hud_text(&self.player))
    }

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
        // `key=value` layout: `scripts/battle-smoke.sh --progress` reads it by key. The two names are
        // percent-encoded like every name of a line (`-` when there is none), so a name never splits it.
        let was = match before {
            Some(old) if old.level != level => format!(" (was level {})", old.level),
            Some(_) => String::new(),
            None => " (on joining)".to_string(),
        };
        let name = if self.player.is_empty() { "-".to_string() } else { encode_name(&self.player) };
        let slot = slot_name(view.character).map_or_else(|| "-".to_string(), encode_name);
        self.note(format!(
            "name={name} slot={slot} level={} exp={} exp_to_next={} gil={} ap={} leveled_up={leveled_up}{was}",
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
    #[cfg(test)]
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
        m.set_player("Wedge");
        m.on_message(1.0, msg(1, 6, 610, 6, 0, 0, false));
        assert_eq!((m.level(), m.exp()), (6, 610));
        m.on_message(9.0, msg(1, 7, 626, 323, 20, 4, true));
        assert_eq!(*m.current().unwrap(), ProgressView { character: 1, level: 7, exp: 626, exp_to_next: 323, gil: 20, ap: 4 });
        assert_eq!((m.updates(), m.level_ups()), (2, 1));
        assert_eq!(
            m.drain_notes(),
            [
                "name=Wedge slot=Barret level=6 exp=610 exp_to_next=6 gil=0 ap=0 leveled_up=false (on joining)",
                "name=Wedge slot=Barret level=7 exp=626 exp_to_next=323 gil=20 ap=4 leveled_up=true (was level 6)",
            ]
        );
        assert!(m.drain_notes().is_empty(), "notes are taken once");
        // Without a level gained there is no "was level".
        m.on_message(10.0, msg(1, 7, 640, 309, 30, 6, false));
        assert_eq!(m.drain_notes(), ["name=Wedge slot=Barret level=7 exp=640 exp_to_next=309 gil=30 ap=6 leveled_up=false"]);
    }

    /// The two names of a note are single tokens whatever they hold (`scripts` split the line on spaces and read
    /// it by key), a mirror that was never told who the player is says `-`, and an id with no name says `-` too.
    #[test]
    fn the_names_of_a_note_are_percent_encoded_and_never_empty() {
        let mut m = ProgressMirror::default();
        m.on_message(0.0, msg(4, 3, 100, 50, 0, 0, false));
        m.on_message(0.0, msg(200, 3, 100, 50, 0, 0, false));
        m.set_player("Red XIII=Blue \u{1b}[31m%");
        m.on_message(0.0, msg(4, 3, 100, 50, 0, 0, false));
        let notes = m.drain_notes();
        assert!(notes[0].starts_with("name=- slot=Red%20XIII level=3 "), "{}", notes[0]);
        assert!(notes[1].starts_with("name=- slot=- level=3 "), "{}", notes[1]);
        assert!(notes[2].starts_with("name=Red%20XIII%3DBlue%20%5B31m%25 slot=Red%20XIII level=3 "), "{}", notes[2]);
        for note in &notes {
            let keys: Vec<&str> = note.split_whitespace().filter_map(|f| f.split_once('=')).map(|(k, _)| k).collect();
            assert_eq!(keys, ["name", "slot", "level", "exp", "exp_to_next", "gil", "ap", "leveled_up"], "{note}");
        }
        assert_eq!(m.player(), "Red XIII=Blue [31m%", "the name is kept cleaned");
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut m = ProgressMirror::default();
        m.on_message(0.0, ServerMsg::Pong { nonce: 1, tick: 2 });
        assert!(m.current().is_none() && m.drain_notes().is_empty());
    }

    #[test]
    fn the_hud_line_says_who_the_player_is_and_which_fighting_style_it_has() {
        let view = |level, exp, exp_to_next, gil| ProgressView { character: 1, level, exp, exp_to_next, gil, ap: 3 };
        assert_eq!(view(6, 82, 2, 25).hud_text("Wedge"), "Wedge (Gunner)  Lv 6  EXP 82/84  Gil 25");
        assert_eq!(view(7, 1234, 266, 250).hud_text("Blizz"), "Blizz (Gunner)  Lv 7  EXP 1234/1500  Gil 250");
        // Another curve, named from our own table of styles (never the wire's text, never a canon name).
        assert_eq!(ProgressView { character: 3, ..view(2, 6, 27, 0) }.hud_text("Wedge"), "Wedge (Mage)  Lv 2  EXP 6/33  Gil 0");
        // The top: nothing is next. A level below the top with nothing next (no curve) shows the total.
        assert_eq!(view(99, 2_408_997, 0, 9_999_999).hud_text("Wedge"), "Wedge (Gunner)  Lv 99  EXP 2408997 MAX  Gil 9999999");
        assert_eq!(view(7, 626, 0, 20).hud_text("Wedge"), "Wedge (Gunner)  Lv 7  EXP 626  Gil 20");
        // A migrated account may hold Cloud's curve: its name is the player's, the style a swordsman's.
        assert_eq!(ProgressView { character: 0, ..view(7, 626, 323, 20) }.hud_text("Blizz"), "Blizz (Swordsman)  Lv 7  EXP 626/949  Gil 20");
        // An id this client has no style for is shown neutrally, and the numbers never overflow.
        assert_eq!(
            ProgressView { character: 200, ..view(7, u32::MAX, u32::MAX, 1) }.hud_text("Wedge"),
            "Wedge (Fighter)  Lv 7  EXP 4294967295/4294967295  Gil 1"
        );
        // A mirror that was never told the name shows the style alone, and the mirror's own line carries its name.
        assert_eq!(view(7, 626, 323, 20).hud_text(""), "Gunner  Lv 7  EXP 626/949  Gil 20");
        let mut m = ProgressMirror::default();
        assert_eq!(m.hud_text(), None, "nothing said: no line");
        m.on_message(0.0, msg(2, 5, 40, 10, 3, 0, false));
        assert_eq!(m.hud_text().as_deref(), Some("Martial artist  Lv 5  EXP 40/50  Gil 3"));
        m.set_player("Wedge");
        assert_eq!(m.hud_text().as_deref(), Some("Wedge (Martial artist)  Lv 5  EXP 40/50  Gil 3"));
        // No line a player can read ever carries a canon character's name for the player.
        for curve in 0..=8u8 {
            let line = ProgressView { character: curve, ..view(5, 1, 1, 1) }.hud_text("Wedge");
            for canon in fmv_ff7_battle::kernel::CharacterId::ALL {
                assert!(!line.contains(canon.name()), "{line} names {}", canon.name());
            }
        }
        // The curve names are this client's own table (internal: the logs say them): the eight a new character can take, and Cloud's.
        let names: Vec<_> = (0..=9).map(slot_name).collect();
        assert_eq!(names, [Some("Cloud"), Some("Barret"), Some("Tifa"), Some("Aerith"), Some("Red XIII"), Some("Yuffie"), Some("Cait Sith"), Some("Vincent"), Some("Cid"), None]);
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
