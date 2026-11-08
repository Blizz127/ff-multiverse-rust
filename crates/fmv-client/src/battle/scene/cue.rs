//! What a batch of battle events is as a show, and WHEN each part of it lands.
//!
//! The engine says what happened instantly and in order: `Action { actor, action }` announces what
//! an actor begins, and the effects that follow (`Damage`, `Miss`, `Heal`, `Revive`, `MpRestored`,
//! `Death`) are what it did, up to the next `Action`. The screen plays that as a PERFORMANCE: the
//! actor runs up or raises its arms, the blow lands, the target reacts and a number floats up. This
//! module is the one place that says when, because two parts of the client that do not talk to each
//! other must agree:
//!
//! * the **overlay's numbers** ([`super::super::popups`], [`super::super::hud`]) are born when the
//!   events ARRIVE (the window builds its [`BattleView`](super::super::BattleView) from the hud
//!   before the scene ever sees the events), so they cannot ask the scene when the blow lands;
//! * the **scene** ([`super::motion`]) plays real clips of different lengths and must make the blow's
//!   impact frame land at the instant the numbers are born.
//!
//! Both call [`cues`] on the batch and [`impact_delay`] for the performance, and add it to the time
//! the batch arrived. The delay depends on the KIND of action only (never on a model's clips: those
//! are time-warped to fit, see [`super::plan`]), is fixed in the table below and never exceeds
//! [`MAX_IMPACT_DELAY`]: **nothing a player sees waits on an animation**. The only skew left is that
//! the scene is brought up to date at the next drawn frame while the hud hears of the batch the
//! moment its message arrives (at most a frame, unless the window stopped drawing: then the scene
//! catches up by skipping, see [`super::plan::fit`], and the numbers keep their own clock).
//!
//! # Protocol v7: limit breaks and escape
//!
//! The matches on actions and events are exhaustive on purpose, which is how the v7 arms were found.
//! What each is to the show:
//!
//! * `ActionRef::Limit(id)`: [`impact_delay`] is [`MAX_IMPACT_DELAY`] (a long show); the performer gathers
//!   its power (the cast clips and a glow) and a strong effect breaks over the target; the id indexes the
//!   kernel's "Names - magic" table (`NameBook::spell(id)` names it).
//! * `ActionRef::Escape`: no impact (delay 0). A successful escape is `Action { Escape }` followed directly
//!   by `BattleEnd { outcome: Escaped }` and no other event: the party turns and runs off the screen (a
//!   director effect started by the action itself, see [`super::motion::Director::on_escape`]).
//! * `BattleEvent::EscapeFailed { actor }`: [`landing`] says nothing at a target; `actor` stumbles a little
//!   way back (a short procedural effect of the director; the overlay's words are the hud's).
//! * `BattleEvent::LimitGauge { actor, value }`: shows nothing in the scene ([`landing`] is `None`); it
//!   arrives right after `Action { Limit }` and after a `Death` that empties a gauge.
//! * `Outcome::Escaped`: no victory pose ([`super::motion::Director::on_victory`] is for a `Victory`
//!   event only).

use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent};

/// The most time between a batch's arrival and the moment its results are shown: a number floats
/// up, the target reacts, an enemy leaves the list. A hard cap that every entry of [`impact_delay`]
/// honours (and a unit test holds them to).
pub const MAX_IMPACT_DELAY: f64 = 0.6;

/// How long after its killing blow lands a death begins (the hurt flash first), in seconds.
pub const DEATH_BEAT: f64 = 0.25;

/// A death is never later than this after the batch arrived: the enemy list drops a beaten enemy
/// then, as its collapse begins.
pub const MAX_DEATH_DELAY: f64 = MAX_IMPACT_DELAY + DEATH_BEAT;

/// Seconds from a batch's arrival to the impact of the action it announced (`None`: effects no
/// action announced, as an older server sends them). Fixed per kind so that the overlay and the
/// scene need not talk; every entry is at most [`MAX_IMPACT_DELAY`].
pub fn impact_delay(action: Option<ActionRef>) -> f64 {
    match action {
        // No announcement: the short beat the lunge always had.
        None => 0.30,
        Some(ActionRef::Attack) => 0.50,
        Some(ActionRef::EnemyAttack(_)) => 0.55,
        // The caster's pose, then the spell breaks over the target.
        Some(ActionRef::Magic(_)) => MAX_IMPACT_DELAY,
        Some(ActionRef::Item(_)) => 0.45,
        // Nothing lands (a guard has no results).
        Some(ActionRef::Defend) => 0.0,
        // A limit break is the longest show there is: the performer gathers, then it breaks.
        Some(ActionRef::Limit(_)) => MAX_IMPACT_DELAY,
        // Running has no results to wait for: a failed attempt only stumbles, a successful one runs off.
        Some(ActionRef::Escape) => 0.0,
    }
}

/// How an event shows on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Landing {
    /// A blow (or a heal, or a revival, or MP coming back) lands on `target` at the impact.
    Impact { target: ActorId },
    /// `actor` begins to die: after the blow that did it, [`DEATH_BEAT`] later.
    Death { actor: ActorId },
}

/// What `event` does on screen when it lands, `None` for those that show nothing at the actor
/// (gauges, the cost of an action, the end of the battle, an announcement).
pub fn landing(event: &BattleEvent) -> Option<Landing> {
    match *event {
        BattleEvent::Damage { target, .. }
        | BattleEvent::Miss { target, .. }
        | BattleEvent::Heal { target, .. }
        | BattleEvent::Revive { target, .. }
        | BattleEvent::MpRestored { target, .. } => Some(Landing::Impact { target }),
        BattleEvent::Death { actor } => Some(Landing::Death { actor }),
        BattleEvent::GaugeReady { .. }
        | BattleEvent::Victory
        | BattleEvent::Defeat
        | BattleEvent::Action { .. }
        // The gauge's bar is the overlay's; the stumble of a failed escape is the director's own.
        | BattleEvent::LimitGauge { .. }
        | BattleEvent::EscapeFailed { .. }
        | BattleEvent::MpSpent { .. } => None,
    }
}

/// Seconds from a batch's arrival to the moment `event` lands, inside a performance of `action`.
pub fn delay_of(action: Option<ActionRef>, event: &BattleEvent) -> f64 {
    match landing(event) {
        // After its blow, a beat later, and never later than the cap says (whatever the table holds).
        Some(Landing::Death { .. }) => (impact_delay(action) + DEATH_BEAT).min(MAX_DEATH_DELAY),
        Some(Landing::Impact { .. }) => impact_delay(action),
        None => 0.0,
    }
}

/// One performance: what an actor began and what came of it.
#[derive(Debug, Clone, Copy)]
pub struct Cue<'a> {
    /// The announcement (`None`: effects before any announcement of the batch).
    pub action: Option<(ActorId, ActionRef)>,
    /// The events after the announcement (not including it) up to the next one.
    pub events: &'a [BattleEvent],
}

impl Cue<'_> {
    /// Seconds from arrival to impact.
    pub fn impact(&self) -> f64 {
        impact_delay(self.action.map(|(_, a)| a))
    }

    /// Who performs it: the announcer, else whoever dealt the first effect.
    pub fn performer(&self) -> Option<ActorId> {
        if let Some((actor, _)) = self.action {
            return Some(actor);
        }
        self.events.iter().find_map(|e| match *e {
            BattleEvent::Damage { actor, .. }
            | BattleEvent::Miss { actor, .. }
            | BattleEvent::Heal { actor, .. }
            | BattleEvent::Revive { actor, .. }
            | BattleEvent::MpRestored { actor, .. } => Some(actor),
            _ => None,
        })
    }

    /// The actors something lands on, in order, each once.
    pub fn targets(&self) -> Vec<ActorId> {
        let mut out: Vec<ActorId> = Vec::new();
        for e in self.events {
            if let Some(Landing::Impact { target }) = landing(e) {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        out
    }
}

/// `events` (one batch, in order) as performances. Every event is in exactly one cue; events before
/// the batch's first announcement form a cue with no action; an announcement with nothing after it
/// is a cue with no events.
pub fn cues(events: &[BattleEvent]) -> Vec<Cue<'_>> {
    let mut out = Vec::new();
    let mut open: Option<(ActorId, ActionRef)> = None;
    let mut from = 0;
    for (i, event) in events.iter().enumerate() {
        if let BattleEvent::Action { actor, action } = *event {
            if open.is_some() || i > from {
                out.push(Cue { action: open, events: &events[from..i] });
            }
            open = Some((actor, action));
            from = i + 1;
        }
    }
    if open.is_some() || from < events.len() {
        out.push(Cue { action: open, events: &events[from..] });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ActorId = ActorId(0);
    const T: ActorId = ActorId(2);

    fn damage(actor: u8, target: u8) -> BattleEvent {
        BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount: 5, critical: false }
    }

    fn action(actor: u8, what: ActionRef) -> BattleEvent {
        BattleEvent::Action { actor: ActorId(actor), action: what }
    }

    #[test]
    fn no_action_lands_later_than_the_cap_and_a_death_is_after_its_blow() {
        for what in [
            None,
            Some(ActionRef::Attack),
            Some(ActionRef::EnemyAttack(272)),
            Some(ActionRef::Magic(27)),
            Some(ActionRef::Item(3)),
            Some(ActionRef::Defend),
            Some(ActionRef::Limit(128)),
            Some(ActionRef::Escape),
        ] {
            let d = impact_delay(what);
            assert!((0.0..=MAX_IMPACT_DELAY).contains(&d), "{what:?}: {d}");
            let death = delay_of(what, &BattleEvent::Death { actor: T });
            assert!((death - d - DEATH_BEAT).abs() < 1e-12 && death <= MAX_DEATH_DELAY, "{what:?}: {death}");
            assert_eq!(delay_of(what, &damage(0, 2)), d);
        }
        // What shows nothing at the actor lands at once.
        for e in [
            BattleEvent::GaugeReady { actor: A },
            BattleEvent::Victory,
            BattleEvent::MpSpent { actor: A, amount: 3 },
            action(0, ActionRef::Attack),
            BattleEvent::LimitGauge { actor: A, value: 137 },
            BattleEvent::EscapeFailed { actor: A },
        ] {
            assert_eq!((landing(&e), delay_of(Some(ActionRef::Attack), &e)), (None, 0.0), "{e:?}");
        }
        // A limit break is the longest show (the cap), an escape has nothing to wait for.
        assert_eq!((impact_delay(Some(ActionRef::Limit(128))), impact_delay(Some(ActionRef::Escape))), (MAX_IMPACT_DELAY, 0.0));
    }

    #[test]
    fn a_limit_is_one_cue_with_its_gauge_and_blow_and_an_escape_is_one_cue_with_no_target() {
        let limit = [action(0, ActionRef::Limit(128)), BattleEvent::LimitGauge { actor: A, value: 0 }, damage(0, 2)];
        let of_limit = cues(&limit);
        assert_eq!(of_limit.len(), 1);
        assert_eq!((of_limit[0].action, of_limit[0].events.len(), of_limit[0].performer()), (Some((A, ActionRef::Limit(128))), 2, Some(A)));
        assert_eq!(of_limit[0].targets(), [T], "the gauge lands on nobody: the blow does");
        assert_eq!(of_limit[0].impact(), MAX_IMPACT_DELAY);
        // A failed escape: the announcement and the failure, nothing lands.
        let failed = [action(1, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(1) }];
        let of_failed = cues(&failed);
        assert_eq!((of_failed.len(), of_failed[0].events.len(), of_failed[0].targets().len(), of_failed[0].impact()), (1, 1, 0, 0.0));
        // A successful one is a bare announcement.
        let away = [action(1, ActionRef::Escape)];
        assert!(cues(&away)[0].events.is_empty());
    }

    #[test]
    fn a_batch_is_one_cue_per_announcement_with_the_effects_that_follow_it() {
        let batch = [damage(1, 0), action(0, ActionRef::Attack), damage(0, 2), BattleEvent::Death { actor: T }, action(1, ActionRef::Magic(27)), damage(1, 2), damage(1, 3), BattleEvent::Victory];
        let cues = cues(&batch);
        assert_eq!(cues.len(), 3);
        // Effects before the first announcement have no action.
        assert!(cues[0].action.is_none() && cues[0].events.len() == 1 && cues[0].performer() == Some(ActorId(1)));
        assert_eq!(cues[1].action, Some((A, ActionRef::Attack)));
        assert_eq!(cues[1].events, &batch[2..4]);
        assert_eq!(cues[2].action, Some((ActorId(1), ActionRef::Magic(27))));
        assert_eq!(cues[2].events.len(), 3, "the end of the battle belongs to the last cue");
        assert_eq!(cues[2].targets(), [T, ActorId(3)]);
        // Every event is in exactly one cue.
        assert_eq!(cues.iter().map(|c| c.events.len() + usize::from(c.action.is_some())).sum::<usize>(), batch.len());
        assert_eq!(cues[1].impact(), impact_delay(Some(ActionRef::Attack)));
    }

    #[test]
    fn empty_batches_and_bare_announcements_make_sense() {
        assert!(cues(&[]).is_empty());
        let bare = [action(0, ActionRef::Defend), action(1, ActionRef::Defend)];
        let cues = cues(&bare);
        assert_eq!(cues.len(), 2);
        assert!(cues.iter().all(|c| c.events.is_empty() && c.targets().is_empty()));
        assert_eq!(cues[1].performer(), Some(ActorId(1)));
        // Only orphans: one cue without an action.
        let orphans = [BattleEvent::Miss { actor: A, target: T }];
        assert_eq!(super::cues(&orphans).len(), 1);
        assert_eq!(super::cues(&[BattleEvent::GaugeReady { actor: A }])[0].performer(), None);
    }
}
