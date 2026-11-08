//! Numbers and words that float up from a fighter when something happens to it: damage in white
//! (gold for a critical hit), healing in green, restored MP in blue, and "Miss".
//!
//! [`Popups`] is the state the client keeps: it is told about the battle's events as they arrive
//! ([`Popups::on_events`]) and says which popups are on screen at a given time
//! ([`Popups::views`], pure data in [`Popup`]). Drawing them over the right fighter is
//! `panels::draw_popups`; where a fighter stands on screen is the caller's business
//! ([`super::view::Anchors`]).
//!
//! # Events
//!
//! [`popup_of`] is the one place that says which event makes which popup:
//!
//! | event | popup |
//! |---|---|
//! | `Damage { target, amount, critical }` | [`PopupKind::Damage`] over `target` |
//! | `Miss { target }` | [`PopupKind::Miss`] |
//! | `Heal { target, amount }` | [`PopupKind::Heal`] with `amount` |
//! | `Revive { target, hp }` | [`PopupKind::Heal`] with `hp` (the HP it came back with) |
//! | `MpRestored { target, amount }` | [`PopupKind::Mp`] with `amount` |
//!
//! Everything else says nothing over a fighter: gauges filling, deaths and the end, an `Action` (its
//! name goes to the banner, see `hud`) and `MpSpent` (the caster's own MP bar shows it).
//!
//! # When a number is born
//!
//! Not when its event arrives: when the blow LANDS. [`Popups::on_events`] groups the batch into
//! performances ([`cue::cues`]) and schedules each number [`cue::impact_delay`] after the batch's
//! arrival, the very instant the scene's impact frame is played at (see [`cue`] for why the two
//! agree without talking), never more than [`cue::MAX_IMPACT_DELAY`] later. A popup that is not
//! born yet is not in [`Popups::views`].
//!
//! Popups over one fighter that are on screen together (a second hit lands while the first number is
//! still rising) are kept apart where they are drawn, newest lowest: see `panels::draw_popups`.
//!
//! Everything that arrives here comes from the server: a popup over a fighter that does not exist
//! simply finds no place on screen, and the list is bounded.

use super::scene::cue;
use fmv_ff7_battle::rules::{ActorId, BattleEvent};

/// How long a popup stays, in seconds.
pub const LIFETIME_S: f64 = 1.1;
/// Popups kept at most; beyond this the oldest are dropped.
const MAX_POPUPS: usize = 48;

/// What a popup says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Damage { amount: u32, critical: bool },
    /// HP restored (the engine's `Heal` and `Revive` events).
    Heal { amount: u32 },
    /// MP restored (the engine's `MpRestored` event).
    Mp { amount: u32 },
    Miss,
}

impl PopupKind {
    /// The words on screen.
    pub fn text(self) -> String {
        match self {
            PopupKind::Damage { amount, .. } | PopupKind::Heal { amount } | PopupKind::Mp { amount } => amount.to_string(),
            PopupKind::Miss => "Miss".to_string(),
        }
    }
}

/// A popup as it is on screen now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Popup {
    /// The fighter it floats up from.
    pub target: ActorId,
    pub kind: PopupKind,
    /// Milliseconds since it appeared.
    pub age_ms: u32,
}

/// The popup an event makes, if any: who it floats up from and what it says.
pub fn popup_of(event: &BattleEvent) -> Option<(ActorId, PopupKind)> {
    match *event {
        BattleEvent::Damage { target, amount, critical, .. } => Some((target, PopupKind::Damage { amount, critical })),
        BattleEvent::Miss { target, .. } => Some((target, PopupKind::Miss)),
        BattleEvent::Heal { target, amount, .. } => Some((target, PopupKind::Heal { amount })),
        BattleEvent::Revive { target, hp, .. } => Some((target, PopupKind::Heal { amount: hp })),
        BattleEvent::MpRestored { target, amount, .. } => Some((target, PopupKind::Mp { amount })),
        // Gauges filling, deaths and the end say nothing over a fighter, nor does an action's start
        // (the banner has its name) or the MP it cost (the caster's bar shows it).
        BattleEvent::GaugeReady { .. }
        | BattleEvent::Death { .. }
        | BattleEvent::Victory
        | BattleEvent::Defeat
        | BattleEvent::Action { .. }
        | BattleEvent::MpSpent { .. } => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Born {
    target: ActorId,
    kind: PopupKind,
    /// When it appears (possibly still ahead: a blow that has not landed yet).
    at: f64,
}

/// The popups of one battle.
#[derive(Debug, Clone, Default)]
pub struct Popups {
    /// Oldest birth first (a number scheduled for a later impact goes behind one born earlier even
    /// if it was pushed first).
    live: Vec<Born>,
}

impl Popups {
    pub fn clear(&mut self) {
        self.live.clear();
    }

    /// A popup over `target` that appears at `at` (seconds on the client's clock).
    pub fn push(&mut self, at: f64, target: ActorId, kind: PopupKind) {
        self.live.retain(|p| at - p.at < LIFETIME_S);
        if self.live.len() >= MAX_POPUPS {
            self.live.remove(0);
        }
        let place = self.live.iter().rposition(|p| p.at <= at).map_or(0, |i| i + 1);
        self.live.insert(place, Born { target, kind, at });
    }

    /// The popups the events of a batch that arrived at `now` make: each when its blow lands
    /// ([`cue::delay_of`]), not when it arrived.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent]) {
        for cue in cue::cues(events) {
            let action = cue.action.map(|(_, a)| a);
            for event in cue.events {
                if let Some((target, kind)) = popup_of(event) {
                    self.push(now + cue::delay_of(action, event), target, kind);
                }
            }
        }
    }

    /// The popups on screen at `now`, oldest first: born (not ahead of `now`) and not yet gone.
    pub fn views(&self, now: f64) -> Vec<Popup> {
        self.live
            .iter()
            .filter(|p| p.at <= now && now - p.at < LIFETIME_S)
            .map(|p| Popup { target: p.target, kind: p.kind, age_ms: ((now - p.at).max(0.0) * 1000.0).round() as u32 })
            .collect()
    }

    /// When each popup that has not been dropped appears, oldest first (tests).
    #[cfg(test)]
    pub fn births(&self) -> Vec<(ActorId, f64)> {
        self.live.iter().map(|p| (p.target, p.at)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7_battle::rules::ActionRef;

    fn damage(target: u8, amount: u32) -> BattleEvent {
        BattleEvent::Damage { actor: ActorId(0), target: ActorId(target), amount, critical: false }
    }

    #[test]
    fn damage_misses_heals_restored_mp_and_revivals_make_popups_and_other_events_do_not() {
        let mut p = Popups::default();
        p.on_events(
            1.0,
            &[
                BattleEvent::GaugeReady { actor: ActorId(0) },
                damage(3, 120),
                BattleEvent::Damage { actor: ActorId(0), target: ActorId(4), amount: 999, critical: true },
                BattleEvent::Miss { actor: ActorId(0), target: ActorId(5) },
                BattleEvent::Death { actor: ActorId(3) },
                BattleEvent::Victory,
                BattleEvent::Defeat,
                // An action's start and what it cost make no number; what it did does.
                BattleEvent::Action { actor: ActorId(0), action: ActionRef::Magic(0) },
                BattleEvent::MpSpent { actor: ActorId(0), amount: 5 },
                BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 271 },
                BattleEvent::Action { actor: ActorId(1), action: ActionRef::Item(3) },
                BattleEvent::MpRestored { actor: ActorId(1), target: ActorId(2), amount: 100 },
                BattleEvent::Action { actor: ActorId(2), action: ActionRef::Item(7) },
                BattleEvent::Revive { actor: ActorId(2), target: ActorId(0), hp: 88 },
                BattleEvent::Action { actor: ActorId(3), action: ActionRef::Attack },
                BattleEvent::Action { actor: ActorId(3), action: ActionRef::Defend },
                BattleEvent::Action { actor: ActorId(3), action: ActionRef::EnemyAttack(272) },
            ],
        );
        // Nothing is up the moment the batch arrives: every blow still has to land.
        assert!(p.views(1.0).is_empty());
        // By the cap they are all up, in the order they land: the effects no action announced first
        // (the short beat), then the two items, then the spell (the longest cast).
        let kinds: Vec<(u8, PopupKind)> = p.views(1.0 + cue::MAX_IMPACT_DELAY).iter().map(|v| (v.target.0, v.kind)).collect();
        assert_eq!(
            kinds,
            [
                (3, PopupKind::Damage { amount: 120, critical: false }),
                (4, PopupKind::Damage { amount: 999, critical: true }),
                (5, PopupKind::Miss),
                (2, PopupKind::Mp { amount: 100 }),
                (0, PopupKind::Heal { amount: 88 }),
                (1, PopupKind::Heal { amount: 271 }),
            ],
            "a revival floats the HP the fighter came back with, like a heal"
        );
        assert_eq!(PopupKind::Damage { amount: 120, critical: false }.text(), "120");
        assert_eq!(PopupKind::Miss.text(), "Miss");
        assert_eq!(PopupKind::Heal { amount: 7 }.text(), "7");
        assert_eq!(PopupKind::Mp { amount: 30 }.text(), "30");
    }

    #[test]
    fn popup_of_says_which_event_makes_which_popup() {
        let (a, t) = (ActorId(0), ActorId(2));
        assert_eq!(popup_of(&BattleEvent::Heal { actor: a, target: t, amount: 5 }), Some((t, PopupKind::Heal { amount: 5 })));
        assert_eq!(popup_of(&BattleEvent::Revive { actor: a, target: t, hp: 9 }), Some((t, PopupKind::Heal { amount: 9 })));
        assert_eq!(popup_of(&BattleEvent::MpRestored { actor: a, target: t, amount: 4 }), Some((t, PopupKind::Mp { amount: 4 })));
        assert_eq!(popup_of(&BattleEvent::Action { actor: a, action: ActionRef::Defend }), None);
        assert_eq!(popup_of(&BattleEvent::MpSpent { actor: a, amount: 4 }), None);
    }

    #[test]
    fn a_popup_ages_and_leaves_after_its_lifetime() {
        let mut p = Popups::default();
        p.push(10.0, ActorId(3), PopupKind::Miss);
        assert_eq!(p.views(10.0)[0].age_ms, 0);
        assert_eq!(p.views(10.5)[0].age_ms, 500);
        assert_eq!(p.views(10.0 + LIFETIME_S - 0.001).len(), 1);
        assert!(p.views(10.0 + LIFETIME_S + 0.001).is_empty(), "gone at the end of its life");
        // A clock that went backwards finds a popup that is not born yet: nothing, not an age out of nothing.
        assert!(p.views(9.0).is_empty());
        // Pushing prunes what has expired.
        p.push(20.0, ActorId(4), PopupKind::Miss);
        assert_eq!(p.live.len(), 1);
    }

    #[test]
    fn popups_come_oldest_first_so_the_drawing_can_keep_a_fighters_numbers_apart() {
        let mut p = Popups::default();
        for (t, target) in [(1.0, 3), (1.1, 3), (1.2, 4), (1.25, 3)] {
            p.push(t, ActorId(target), PopupKind::Damage { amount: (t * 100.0) as u32, critical: false });
        }
        let ages: Vec<(u8, u32)> = p.views(1.3).iter().map(|v| (v.target.0, v.age_ms)).collect();
        assert_eq!(ages, [(3, 300), (3, 200), (4, 100), (3, 50)]);
    }

    #[test]
    fn a_number_is_born_when_its_blow_lands_and_never_later_than_the_cap_after_the_batch_arrived() {
        use fmv_ff7_battle::rules::ActionRef as A;
        for (what, expected) in [
            (A::Attack, 0.50),
            (A::EnemyAttack(272), 0.55),
            (A::Magic(27), 0.60),
            (A::Item(3), 0.45),
        ] {
            let mut p = Popups::default();
            p.on_events(
                7.0,
                &[BattleEvent::Action { actor: ActorId(0), action: what }, BattleEvent::Damage { actor: ActorId(0), target: ActorId(3), amount: 12, critical: false }],
            );
            assert!(p.views(7.0).is_empty() && p.views(7.0 + expected - 0.001).is_empty(), "{what:?}: not before the blow lands");
            let up = p.views(7.0 + expected);
            assert_eq!(up.len(), 1, "{what:?}");
            assert_eq!(up[0].age_ms, 0, "{what:?}: it starts rising at the impact");
            assert!(expected <= cue::MAX_IMPACT_DELAY);
        }
        // A guard has nothing to land: no number at all.
        let mut p = Popups::default();
        p.on_events(7.0, &[BattleEvent::Action { actor: ActorId(0), action: A::Defend }]);
        assert!(p.views(8.0).is_empty());
    }

    #[test]
    fn the_blows_of_one_action_land_together_and_a_later_batch_does_not_wait_for_an_earlier_one() {
        use fmv_ff7_battle::rules::ActionRef as A;
        let mut p = Popups::default();
        // A spell on two targets: both numbers at the one impact. Then, a moment later, a quick item.
        p.on_events(
            2.0,
            &[
                BattleEvent::Action { actor: ActorId(0), action: A::Magic(27) },
                damage(2, 40),
                damage(3, 41),
            ],
        );
        p.on_events(2.2, &[BattleEvent::Action { actor: ActorId(1), action: A::Item(3) }, BattleEvent::Heal { actor: ActorId(1), target: ActorId(1), amount: 9 }]);
        let births = p.births();
        // Oldest first: the item's heal (2.2 + 0.45) is born before the spell's numbers (2.0 + 0.6)? No:
        // 2.65 against 2.6: the spell lands first, and the two of it are at the very same instant.
        assert_eq!(births.iter().map(|(t, _)| t.0).collect::<Vec<_>>(), [2, 3, 1]);
        assert!((births[0].1 - 2.6).abs() < 1e-12 && births[0].1 == births[1].1);
        assert!((births[2].1 - 2.65).abs() < 1e-12);
        // Each is born within the cap of the arrival of its own batch.
        assert!(births[0].1 - 2.0 <= cue::MAX_IMPACT_DELAY + 1e-12 && births[2].1 - 2.2 <= cue::MAX_IMPACT_DELAY + 1e-12);
    }

    #[test]
    fn a_number_scheduled_for_later_goes_behind_one_born_sooner_whatever_was_pushed_first() {
        let mut p = Popups::default();
        p.push(5.0, ActorId(3), PopupKind::Miss);
        p.push(4.0, ActorId(4), PopupKind::Miss);
        p.push(4.5, ActorId(5), PopupKind::Miss);
        assert_eq!(p.births().iter().map(|(t, _)| t.0).collect::<Vec<_>>(), [4, 5, 3]);
    }

    #[test]
    fn the_list_is_bounded() {
        let mut p = Popups::default();
        for i in 0..500 {
            p.push(1.0, ActorId((i % 6) as u8), PopupKind::Miss);
        }
        assert_eq!(p.views(1.0).len(), MAX_POPUPS);
        p.clear();
        assert!(p.views(1.0).is_empty());
    }
}
