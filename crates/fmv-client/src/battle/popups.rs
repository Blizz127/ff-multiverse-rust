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
//! [`popup_of`] is the one place that says which event makes which popup. The engine's `Heal`,
//! `MpRestored` and `Revive` events (the contract of the Magic/Item commands) get their arms there
//! when they exist: `Heal { target, amount }` is [`PopupKind::Heal`], `MpRestored { target, amount }` is
//! [`PopupKind::Mp`], `Revive { target, hp }` is [`PopupKind::Heal`] with `hp`.
//!
//! Everything that arrives here comes from the server: a popup over a fighter that does not exist
//! simply finds no place on screen, and the list is bounded.

use fmv_ff7_battle::rules::{ActorId, BattleEvent};

/// How long a popup stays, in seconds.
pub const LIFETIME_S: f64 = 1.1;
/// Popups kept at most; beyond this the oldest are dropped.
const MAX_POPUPS: usize = 48;
/// Popups that start over the same fighter within this many seconds of each other are stacked
/// instead of printed over one another.
const STACK_S: f64 = 0.4;
/// Most popups stacked over one fighter.
const MAX_STACK: u8 = 3;

/// What a popup says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Damage { amount: u32, critical: bool },
    Heal { amount: u32 },
    /// MP restored.
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
    /// How many popups were already stacked over the same fighter when it appeared (0 is the
    /// lowest).
    pub slot: u8,
}

/// The popup an event makes, if any: who it floats up from and what it says.
pub fn popup_of(event: &BattleEvent) -> Option<(ActorId, PopupKind)> {
    match *event {
        BattleEvent::Damage { target, amount, critical, .. } => Some((target, PopupKind::Damage { amount, critical })),
        BattleEvent::Miss { target, .. } => Some((target, PopupKind::Miss)),
        // Gauges filling, deaths and the end say nothing over a fighter. Events this build does not
        // know yet (the Magic/Item contract's) make no popup until they get their arm above.
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Born {
    target: ActorId,
    kind: PopupKind,
    at: f64,
    slot: u8,
}

/// The popups of one battle.
#[derive(Debug, Clone, Default)]
pub struct Popups {
    live: Vec<Born>,
}

impl Popups {
    pub fn clear(&mut self) {
        self.live.clear();
    }

    /// A popup over `target`, starting `now` (seconds on the client's clock).
    pub fn push(&mut self, now: f64, target: ActorId, kind: PopupKind) {
        self.live.retain(|p| now - p.at < LIFETIME_S);
        let stacked = self.live.iter().filter(|p| p.target == target && (now - p.at).abs() < STACK_S).count();
        let slot = u8::try_from(stacked).unwrap_or(MAX_STACK).min(MAX_STACK);
        if self.live.len() >= MAX_POPUPS {
            self.live.remove(0);
        }
        self.live.push(Born { target, kind, at: now, slot });
    }

    /// The popups the events make.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent]) {
        for event in events {
            if let Some((target, kind)) = popup_of(event) {
                self.push(now, target, kind);
            }
        }
    }

    /// The popups on screen at `now`, oldest first.
    pub fn views(&self, now: f64) -> Vec<Popup> {
        self.live
            .iter()
            .filter(|p| now - p.at < LIFETIME_S)
            .map(|p| Popup { target: p.target, kind: p.kind, age_ms: ((now - p.at).max(0.0) * 1000.0) as u32, slot: p.slot })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn damage(target: u8, amount: u32) -> BattleEvent {
        BattleEvent::Damage { actor: ActorId(0), target: ActorId(target), amount, critical: false }
    }

    #[test]
    fn damage_and_misses_make_popups_and_other_events_do_not() {
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
            ],
        );
        let kinds: Vec<(u8, PopupKind)> = p.views(1.0).iter().map(|v| (v.target.0, v.kind)).collect();
        assert_eq!(
            kinds,
            [
                (3, PopupKind::Damage { amount: 120, critical: false }),
                (4, PopupKind::Damage { amount: 999, critical: true }),
                (5, PopupKind::Miss)
            ]
        );
        assert_eq!(PopupKind::Damage { amount: 120, critical: false }.text(), "120");
        assert_eq!(PopupKind::Miss.text(), "Miss");
        assert_eq!(PopupKind::Heal { amount: 7 }.text(), "7");
        assert_eq!(PopupKind::Mp { amount: 30 }.text(), "30");
    }

    #[test]
    fn a_popup_ages_and_leaves_after_its_lifetime() {
        let mut p = Popups::default();
        p.push(10.0, ActorId(3), PopupKind::Miss);
        assert_eq!(p.views(10.0)[0].age_ms, 0);
        assert_eq!(p.views(10.5)[0].age_ms, 500);
        assert_eq!(p.views(10.0 + LIFETIME_S - 0.001).len(), 1);
        assert!(p.views(10.0 + LIFETIME_S + 0.001).is_empty(), "gone at the end of its life");
        // A clock that went backwards does not make an age out of nothing.
        assert_eq!(p.views(9.0)[0].age_ms, 0);
        // Pushing prunes what has expired.
        p.push(20.0, ActorId(4), PopupKind::Miss);
        assert_eq!(p.live.len(), 1);
    }

    #[test]
    fn hits_on_one_fighter_in_quick_succession_stack_instead_of_overprinting() {
        let mut p = Popups::default();
        for (t, target) in [(1.0, 3), (1.1, 3), (1.2, 3), (1.25, 3), (1.3, 4)] {
            p.push(t, ActorId(target), PopupKind::Damage { amount: 1, critical: false });
        }
        let slots: Vec<(u8, u8)> = p.views(1.3).iter().map(|v| (v.target.0, v.slot)).collect();
        assert_eq!(slots, [(3, 0), (3, 1), (3, 2), (3, 3), (4, 0)], "the stack is capped, other fighters start at the bottom");
        // Much later the same fighter starts at the bottom again.
        p.push(5.0, ActorId(3), PopupKind::Miss);
        assert_eq!(p.views(5.0)[0].slot, 0);
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
