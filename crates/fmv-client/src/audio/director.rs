//! What should be playing, decided from what the game is doing. Pure: no engine, no files, no
//! threads, so every rule below is a plain unit test.
//!
//! The rules are the game's own:
//!
//! * **The field's music is sticky.** A field script that says `MUSIC` starts that track; one that
//!   does not leaves whatever plays playing (rooms of one place share a track), and asking for the
//!   track that already plays does not restart it. The first field of a session without a `MUSIC`
//!   is silent.
//! * **A battle has its own music**: the field's `BMUSC` track when the script sets one, otherwise
//!   the standard battle theme.
//! * **A victory plays the fanfare once**, and when it is over the field's music comes back (the
//!   field's `FMUSC` track when it sets one, otherwise the track that played before the fight).
//! * **Running away and losing go straight back** to the field's music.
//!
//! What a *track* is (a name from `music.idx`), how it reaches the speakers (decoding, crossfades) and
//! what happens when a file is missing are not decided here: [`Director::want`] only says which track
//! should be playing, and the caller reports back what it could not load
//! ([`Director::unavailable`]).

use std::collections::HashSet;
use std::time::Duration;

/// The battle theme of a field that sets none (`Fight On!`, music.idx's `bat`). Of the 313 distinct
/// (field, `BMUSC` operand) pairs in the owner's data, 250 name it themselves.
pub const STANDARD_BATTLE_TRACK: &str = "bat";
/// The one-shot played after a won battle: 5.2 s, no loop tags (the track `fan2` loops for 54 s, so
/// it is not a jingle).
pub const VICTORY_TRACK: &str = "fanfare";

/// What a field's script says about music, as track names (lower case, as `music.idx` spells them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AreaMusic {
    /// The track that starts on arrival (`MUSIC` in an entity's own script); `None`: the music goes on.
    pub arrival: Option<String>,
    /// The music of the next fight (`BMUSC`); `None`: the standard battle theme.
    pub battle: Option<String>,
    /// The music after the next fight (`FMUSC`); `None`: the field's music again.
    pub after_battle: Option<String>,
}

/// The battle counters of the client's mirror ([`crate::battle::BattleClient`]), as the director
/// sees them: it reacts to changes, not to the counters themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BattleCounts {
    pub in_battle: bool,
    pub entered: u64,
    pub ended: u64,
    pub victories: u64,
    pub defeats: u64,
    pub escapes: u64,
}

/// Why a track is wanted (for the log, and for choosing how it fades in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The field's own music.
    Field,
    /// A fight is on.
    Battle,
    /// A fight was won: the fanfare.
    Victory,
    /// The field's music is back after a fight.
    AfterFight,
}

impl Why {
    pub fn text(self) -> &'static str {
        match self {
            Why::Field => "field",
            Why::Battle => "battle",
            Why::Victory => "victory",
            Why::AfterFight => "back in the field after the fight",
        }
    }
}

/// How the music changes hands: a new field's track crossfades slowly, a fight cuts in fast, the
/// fanfare is nearly a cut, and the field's music comes back over a moment.
pub fn fade_into(why: Why) -> Duration {
    match why {
        Why::Field => Duration::from_millis(1_200),
        Why::Battle => Duration::from_millis(200),
        Why::Victory => Duration::from_millis(120),
        Why::AfterFight => Duration::from_millis(700),
    }
}

/// The track that should be playing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub track: String,
    /// Loop it (everything but the fanfare).
    pub repeat: bool,
    pub why: Why,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Field,
    Battle,
    /// Won: the fanfare plays, and until it is over nothing else is wanted.
    Fanfare { over: bool },
}

/// See the module docs.
#[derive(Debug)]
pub struct Director {
    standard_battle: String,
    victory: String,
    /// The area the client is in, when it said (`None`: no area yet).
    area: Option<String>,
    /// What the area's script says, once the analysis of it arrived.
    music: AreaMusic,
    /// The field's current track (sticky across areas that name none).
    field: Option<String>,
    phase: Phase,
    seen: BattleCounts,
    /// Tracks that could not be loaded (no file, damaged): never wanted again.
    unavailable: HashSet<String>,
}

impl Default for Director {
    fn default() -> Self {
        Self::new()
    }
}

impl Director {
    pub fn new() -> Self {
        Self {
            standard_battle: STANDARD_BATTLE_TRACK.to_string(),
            victory: VICTORY_TRACK.to_string(),
            area: None,
            music: AreaMusic::default(),
            field: None,
            phase: Phase::Field,
            seen: BattleCounts::default(),
            unavailable: HashSet::new(),
        }
    }

    /// Play `track` as the victory fanfare instead of [`VICTORY_TRACK`] (a name from `music.idx`): an escape
    /// hatch for an owner whose ears disagree with the choice made from the files' shapes.
    pub fn with_victory_track(mut self, track: &str) -> Self {
        self.victory = track.to_string();
        self
    }

    /// The client is in `key` now (call it with the area every time; only a change counts). The old
    /// area's script no longer applies; the new one's arrives as [`Director::area_music`].
    pub fn area(&mut self, key: &str) {
        if self.area.as_deref() != Some(key) {
            self.area = Some(key.to_string());
            self.music = AreaMusic::default();
            // The fight is behind us once its fanfare is over and we are somewhere else: what plays now is
            // simply the field's music.
            if self.phase == (Phase::Fanfare { over: true }) {
                self.phase = Phase::Field;
            }
        }
    }

    /// The analysis of `key`'s script arrived. An answer for an area that was left meanwhile is dropped.
    /// Returns whether it was taken.
    pub fn area_music(&mut self, key: &str, music: AreaMusic) -> bool {
        if self.area.as_deref() != Some(key) {
            return false;
        }
        if let Some(arrival) = &music.arrival {
            self.field = Some(arrival.clone());
        }
        self.music = music;
        true
    }

    /// The battle counters as they are now (call it with them every time; only changes count).
    pub fn battle(&mut self, now: BattleCounts) {
        let before = std::mem::replace(&mut self.seen, now);
        if now.in_battle {
            self.phase = Phase::Battle;
            return;
        }
        if now.ended > before.ended {
            // The music the field wants after a fight replaces the one that played before it.
            if let Some(after) = &self.music.after_battle {
                self.field = Some(after.clone());
            }
            self.phase = if now.victories > before.victories { Phase::Fanfare { over: false } } else { Phase::Field };
        } else if self.phase == Phase::Battle {
            // A battle that is gone without an end counted (dropped with its area): back to the field.
            self.phase = Phase::Field;
        }
    }

    /// A one-shot track ran to its end (the engine counts those): the fanfare is over.
    pub fn track_ended(&mut self) {
        if let Phase::Fanfare { over: false } = self.phase {
            self.phase = Phase::Fanfare { over: true };
        }
    }

    /// `track` could not be loaded. The fanfare then counts as over at once, a battle theme that is
    /// missing leaves the field's music playing, and a field track that is missing is silence.
    pub fn unavailable(&mut self, track: &str) {
        self.unavailable.insert(track.to_string());
        if track == self.victory {
            self.track_ended();
        }
    }

    fn available(&self, track: &str) -> bool {
        !self.unavailable.contains(track)
    }

    /// The track of the next fight in this area.
    pub fn battle_track(&self) -> &str {
        self.music.battle.as_deref().unwrap_or(&self.standard_battle)
    }

    /// The victory fanfare (tests).
    #[cfg(test)]
    pub fn victory_track(&self) -> &str {
        &self.victory
    }

    /// The tracks worth decoding ahead of need: the next fight's music and the fanfare (decoding a
    /// 166 s theme takes seconds on a slow machine, and a fight cannot wait for it).
    pub fn prefetch(&self) -> Vec<String> {
        let mut tracks: Vec<String> = Vec::new();
        for track in [Some(self.battle_track()), Some(self.victory.as_str()), self.music.after_battle.as_deref()].into_iter().flatten() {
            if self.available(track) && !tracks.iter().any(|t| t == track) {
                tracks.push(track.to_string());
            }
        }
        tracks
    }

    fn field_want(&self, why: Why) -> Option<Want> {
        let track = self.field.as_ref().filter(|t| self.available(t))?;
        Some(Want { track: track.clone(), repeat: true, why })
    }

    /// The track that should be playing now, or `None` when nothing is wanted (the caller then leaves
    /// things as they are: it never stops music by itself).
    pub fn want(&self) -> Option<Want> {
        match self.phase {
            Phase::Battle => {
                let track = self.battle_track();
                if self.available(track) {
                    Some(Want { track: track.to_string(), repeat: true, why: Why::Battle })
                } else {
                    self.field_want(Why::Field)
                }
            }
            Phase::Fanfare { over: false } if self.available(&self.victory) => {
                Some(Want { track: self.victory.clone(), repeat: false, why: Why::Victory })
            }
            Phase::Fanfare { .. } => self.field_want(Why::AfterFight),
            Phase::Field => self.field_want(Why::Field),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music(arrival: Option<&str>, battle: Option<&str>, after: Option<&str>) -> AreaMusic {
        AreaMusic { arrival: arrival.map(str::to_string), battle: battle.map(str::to_string), after_battle: after.map(str::to_string) }
    }

    fn want(director: &Director) -> Option<(String, bool, Why)> {
        director.want().map(|w| (w.track, w.repeat, w.why))
    }

    fn field(track: &str) -> Option<(String, bool, Why)> {
        Some((track.to_string(), true, Why::Field))
    }

    fn in_area(key: &str, m: AreaMusic) -> Director {
        let mut d = Director::new();
        d.area(key);
        assert!(d.area_music(key, m));
        d
    }

    fn counts(in_battle: bool, entered: u64, ended: u64, victories: u64, defeats: u64, escapes: u64) -> BattleCounts {
        BattleCounts { in_battle, entered, ended, victories, defeats, escapes }
    }

    #[test]
    fn nothing_is_wanted_before_an_area_names_a_track() {
        let mut d = Director::new();
        assert_eq!(d.want(), None);
        d.area("md1stin");
        assert_eq!(d.want(), None, "the script's analysis has not arrived");
        assert!(d.area_music("md1stin", music(None, None, None)));
        assert_eq!(d.want(), None, "a first field without a MUSIC is silent");
    }

    #[test]
    fn an_areas_arrival_track_is_the_field_music() {
        let d = in_area("md1stin", music(Some("ob"), None, Some("ob")));
        assert_eq!(want(&d), field("ob"));
    }

    #[test]
    fn the_field_music_is_sticky_across_areas_that_name_none() {
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.area("md1_1");
        assert_eq!(want(&d), field("ob"), "the new area's script is not known yet: the music goes on");
        assert!(d.area_music("md1_1", music(None, None, None)));
        assert_eq!(want(&d), field("ob"));
        // Another area that names a track replaces it.
        d.area("mds7st1");
        assert!(d.area_music("mds7st1", music(Some("kurai"), Some("bat"), None)));
        assert_eq!(want(&d), field("kurai"));
        // Asking again with the same key changes nothing.
        d.area("mds7st1");
        assert_eq!(want(&d), field("kurai"));
    }

    #[test]
    fn an_answer_for_an_area_that_was_left_is_dropped() {
        let mut d = in_area("a", music(Some("ob"), None, None));
        d.area("b");
        assert!(!d.area_music("a", music(Some("yume"), None, None)), "the answer for a arrives after b was entered");
        assert_eq!(want(&d), field("ob"));
        assert!(d.area_music("b", music(Some("mati"), None, None)));
        assert_eq!(want(&d), field("mati"));
    }

    #[test]
    fn a_battle_wants_the_fields_battle_music_or_the_standard_theme() {
        let mut d = in_area("mds7st1", music(Some("kurai"), Some("chu"), None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        assert_eq!(want(&d), Some(("chu".to_string(), true, Why::Battle)));
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        assert_eq!(want(&d), Some(("bat".to_string(), true, Why::Battle)));
        assert_eq!(d.battle_track(), "bat");
        // A battle before any area named a track still has its music.
        let mut d = Director::new();
        d.battle(counts(true, 1, 0, 0, 0, 0));
        assert_eq!(want(&d), Some(("bat".to_string(), true, Why::Battle)));
    }

    #[test]
    fn a_victory_plays_the_fanfare_once_and_then_the_field_music_returns() {
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("fanfare".to_string(), false, Why::Victory)));
        // While it plays, nothing else is wanted, however often we ask.
        assert_eq!(want(&d), Some(("fanfare".to_string(), false, Why::Victory)));
        d.track_ended();
        assert_eq!(want(&d), Some(("ob".to_string(), true, Why::AfterFight)));
        // A second end-of-track changes nothing.
        d.track_ended();
        assert_eq!(want(&d), Some(("ob".to_string(), true, Why::AfterFight)));
        // The next fight starts over.
        d.battle(counts(true, 2, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("bat".to_string(), true, Why::Battle)));
        d.battle(counts(false, 2, 2, 2, 0, 0));
        assert_eq!(want(&d), Some(("fanfare".to_string(), false, Why::Victory)), "the fanfare plays again after the next win");
    }

    #[test]
    fn running_away_and_losing_go_straight_back_to_the_field_music() {
        for (escapes, defeats) in [(1, 0), (0, 1)] {
            let mut d = in_area("md1stin", music(Some("ob"), None, None));
            d.battle(counts(true, 1, 0, 0, 0, 0));
            d.battle(counts(false, 1, 1, 0, defeats, escapes));
            assert_eq!(want(&d), field("ob"), "escapes {escapes} defeats {defeats}");
        }
    }

    #[test]
    fn the_music_the_field_wants_after_a_fight_replaces_the_one_before_it() {
        let mut d = in_area("junpb_2", music(Some("parade"), Some("chu"), Some("condor")));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        d.track_ended();
        assert_eq!(want(&d), Some(("condor".to_string(), true, Why::AfterFight)));
        // It is the field's music from now on (it stays when the next area names none).
        d.area("next");
        assert_eq!(want(&d), field("condor"));
    }

    #[test]
    fn two_things_between_two_looks_are_seen_as_what_they_came_to() {
        // A battle that began and was won between two looks: no battle theme, the fanfare.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("fanfare".to_string(), false, Why::Victory)));
        // One won and the next already on: the fight's music.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 2, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("bat".to_string(), true, Why::Battle)));
        // Joining a session that is in a battle already.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 0, 0, 0, 0, 0));
        assert_eq!(want(&d), Some(("bat".to_string(), true, Why::Battle)));
    }

    #[test]
    fn a_battle_that_vanishes_without_an_end_returns_to_the_field() {
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 0, 0, 0, 0));
        assert_eq!(want(&d), field("ob"));
    }

    #[test]
    fn an_area_change_during_a_battle_waits_for_the_battle_to_end() {
        let mut d = in_area("a", music(Some("ob"), None, None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.area("b");
        assert!(d.area_music("b", music(Some("mati"), Some("chu"), None)));
        assert_eq!(want(&d), Some(("chu".to_string(), true, Why::Battle)), "the battle goes on with the new area's battle music");
        d.battle(counts(false, 1, 1, 0, 0, 1));
        assert_eq!(want(&d), field("mati"));
    }

    #[test]
    fn a_track_that_cannot_be_loaded_is_never_wanted() {
        // No fanfare file: a win goes straight back to the field music.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.unavailable("fanfare");
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("ob".to_string(), true, Why::AfterFight)));
        // No battle theme: the field's music plays on through the fight.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.unavailable("bat");
        d.battle(counts(true, 1, 0, 0, 0, 0));
        assert_eq!(want(&d), field("ob"));
        // No field track: nothing is wanted (what plays, plays on).
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.unavailable("ob");
        assert_eq!(d.want(), None);
        // The fanfare that goes missing while it is wanted counts as over.
        let mut d = in_area("md1stin", music(Some("ob"), None, None));
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("fanfare".to_string(), false, Why::Victory)));
        d.unavailable("fanfare");
        assert_eq!(want(&d), Some(("ob".to_string(), true, Why::AfterFight)));
    }

    #[test]
    fn the_next_fights_music_and_the_fanfare_are_worth_decoding_ahead() {
        let d = in_area("md1stin", music(Some("ob"), None, Some("ob")));
        assert_eq!(d.prefetch(), ["bat", "fanfare", "ob"]);
        let d = in_area("mds7st1", music(Some("kurai"), Some("chu"), None));
        assert_eq!(d.prefetch(), ["chu", "fanfare"]);
        let mut d = in_area("mds7st1", music(Some("kurai"), None, None));
        d.unavailable("fanfare");
        assert_eq!(d.prefetch(), ["bat"]);
    }

    #[test]
    fn the_fanfare_can_be_another_track() {
        let mut d = in_area("md1stin", music(Some("ob"), None, None)).with_victory_track("fan2");
        assert_eq!(d.victory_track(), "fan2");
        d.battle(counts(true, 1, 0, 0, 0, 0));
        d.battle(counts(false, 1, 1, 1, 0, 0));
        assert_eq!(want(&d), Some(("fan2".to_string(), false, Why::Victory)));
        assert_eq!(d.prefetch(), ["bat", "fan2"]);
    }

    #[test]
    fn the_fades_say_how_a_track_enters() {
        assert!(fade_into(Why::Victory) < fade_into(Why::Battle));
        assert!(fade_into(Why::Battle) < fade_into(Why::AfterFight));
        assert!(fade_into(Why::AfterFight) < fade_into(Why::Field));
        for why in [Why::Field, Why::Battle, Why::Victory, Why::AfterFight] {
            assert!(!why.text().is_empty());
        }
    }
}
