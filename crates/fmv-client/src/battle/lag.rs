//! What the panels show of HP, MP and who is on their feet: the state AFTER an action, shown when the action LANDS.
//!
//! The replayed engine says what happened to everybody the moment a batch arrives, so a [`BattleView`] built from it
//! has the HP, the MP and the dead of the WHOLE batch at once, while the show is played one performance after the
//! other (`scene::cue`: a blow lands, its follow-through plays, and only then does the counter that answers it run up and land,
//! up to a second and a half later). A bar that dropped before the blow that dropped it was seen made the numbers and the
//! panel disagree for that long. This module is the other half of what `popups` does for the numbers: it holds every value
//! back until the performance that changes it lands.
//!
//! # How
//!
//! When a batch arrives the client hands over the numbers of every fighter before the engine applied it and after
//! ([`Lag::on_batch`]). Walking the batch with the same timeline as the numbers and the banner ([`cue::Cue::delay_of`]), each
//! event that changes a value becomes a [`Change`] due at its instant: a blow takes HP when it lands, a heal gives it, a
//! revival stands the fighter up, an Ether gives MP, a spell takes its MP when its performance BEGINS (the caster pays as it
//! casts), a turn of Poison or Regen at once, and a death flips the fighter when its collapse begins (a beat after the
//! blow that did it, like the enemy list). What is shown is **the state of the engine minus the changes that have not
//! landed yet** ([`Lag::apply`]), so several batches in flight compose (a counter still to land while the next batch
//! arrives) and nothing is ever remembered as an absolute number that could be wrong.
//!
//! The amounts are walked with the engine's own rules for what an event means (a blow takes at most what the target has, a
//! heal is what was restored, a revival brings the HP it names), and the part of the real change those rules do not
//! explain is added to the last change of the fighter, so the shown value always ends at the engine's: **the lag can only
//! be late, never wrong**, and it is gone at the instant of the last landing of the batch ([`MAX_BATCH_SPAN`] after the
//! batch arrived at the latest).
//!
//! # Statuses: the tags and the bodies
//!
//! A status is held back the same way. A status an action inflicts or cures is an event of its performance
//! (`StatusInflicted`, `StatusEnded`), so it becomes a [`Change`] due at the landing of that performance, which is the instant its
//! word floats up and its sign breaks over the fighter; a status that ran out on the clock lands with the 0.3 s beat of an
//! event that has no announcement; a knockout, which removes every status without saying so, takes them with the collapse.
//! What the panels show is the engine's set minus the flips that have not landed: [`Lag::apply`] undoes the ones still ahead,
//! the latest first, so that a status inflicted by one performance of a batch and cured by a later one is shown from the
//! first landing to the second. The 3D scene does not keep a second
//! lag: it takes the statuses of the bodies from the very bars the panels are drawn from
//! ([`super::scene::BattleScene::show_statuses`]), so a body's colour, droop and held pose change at the instant its tag does.
//!
//! What is NOT held back: the gauges (the ATB bar fills continuously, and the menu opens when the engine says it is our
//! turn), the limit gauge, whether a fighter is ready (a sleeper loses its gold ring at once) and the menu, which
//! reads the engine's own numbers (the MP of the next spell, the targets that are alive) before this is applied. The
//! enemy list's held-back deaths ([`super::hud::BattleHud::decorate`]) are the same idea for the same moment.
//!
//! [`MAX_BATCH_SPAN`]: super::scene::cue::MAX_BATCH_SPAN

use super::scene::cue;
use super::view::BattleView;
use fmv_ff7_battle::rules::{ActorId, BattleEvent, BattleState};
use fmv_ff7_battle::status::{Status, StatusSet};

/// Changes kept at most; beyond this the oldest are dropped (a window that stopped drawing must not grow the list).
const MAX_PENDING: usize = 256;

/// One fighter's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vitals {
    pub hp: u32,
    pub max_hp: u32,
    pub mp: u32,
    pub max_mp: u32,
    pub alive: bool,
    /// The statuses it has (the ones the rules play).
    pub status: StatusSet,
}

/// The numbers of everybody in `state`, party first and then the enemies.
pub fn vitals_of(state: &BattleState) -> Vec<(ActorId, Vitals)> {
    state
        .party_ids()
        .into_iter()
        .chain(state.enemy_ids())
        .filter_map(|id| state.actor(id).map(|c| (id, Vitals { hp: c.hp, max_hp: c.max_hp, mp: c.mp, max_mp: c.max_mp, alive: c.is_alive(), status: c.status.set() })))
        .collect()
}

/// A change of one fighter's numbers that has not landed yet: HP and MP by what they change by, the standing by what it
/// becomes, the statuses by the ones it gains and the ones it loses.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Change {
    /// When it lands (seconds on the client's clock).
    at: f64,
    actor: ActorId,
    hp: i64,
    mp: i64,
    /// `Some(true)`: the fighter stands up (a revival), `Some(false)`: it falls.
    stands: Option<bool>,
    /// The statuses it gains and the ones it loses at that instant.
    on: StatusSet,
    off: StatusSet,
}

impl Change {
    fn is_nothing(&self) -> bool {
        self.hp == 0 && self.mp == 0 && self.stands.is_none() && self.on.is_empty() && self.off.is_empty()
    }
}

/// The changes that have not landed yet. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct Lag {
    pending: Vec<Change>,
}

impl Lag {
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// A batch of `events` arrived at `now`: `before` and `after` are everybody's numbers just before and just after the engine
    /// applied it ([`vitals_of`]). The changes that already landed by `now` are forgotten; those still ahead (an earlier batch's
    /// counter) are kept, and this batch's are added.
    pub fn on_batch(&mut self, now: f64, events: &[BattleEvent], before: &[(ActorId, Vitals)], after: &[(ActorId, Vitals)]) {
        self.pending.retain(|c| c.at > now);
        let mut walked: Vec<(ActorId, Vitals)> = before.to_vec();
        let mut touched: Vec<(ActorId, f64)> = Vec::new();
        let mut changes: Vec<Change> = Vec::new();
        for cue in cue::cues(events) {
            for event in cue.events {
                let at = now + cue.delay_of(event);
                let Some((actor, edit, amount)) = effect(event) else { continue };
                let Some((_, v)) = walked.iter_mut().find(|(a, _)| *a == actor) else { continue };
                let was = *v;
                edit(v, amount);
                let change = Change {
                    at,
                    actor,
                    hp: i64::from(v.hp) - i64::from(was.hp),
                    mp: i64::from(v.mp) - i64::from(was.mp),
                    stands: (v.alive != was.alive).then_some(v.alive),
                    on: v.status.difference(was.status),
                    off: was.status.difference(v.status),
                };
                match touched.iter_mut().find(|(a, _)| *a == actor) {
                    Some((_, last)) => *last = last.max(at),
                    None => touched.push((actor, at)),
                }
                if !change.is_nothing() {
                    changes.push(change);
                }
            }
        }
        // What the walk did not explain (a rule it does not know) goes with the last landing of that fighter, so that
        // the shown numbers end at the engine's.
        for (actor, last) in touched {
            let (Some((_, got)), Some((_, end))) = (walked.iter().find(|(a, _)| *a == actor), after.iter().find(|(a, _)| *a == actor)) else { continue };
            let rest = Change {
                at: last,
                actor,
                hp: i64::from(end.hp) - i64::from(got.hp),
                mp: i64::from(end.mp) - i64::from(got.mp),
                stands: (end.alive != got.alive).then_some(end.alive),
                on: end.status.difference(got.status),
                off: got.status.difference(end.status),
            };
            if !rest.is_nothing() {
                changes.push(rest);
            }
        }
        self.pending.extend(changes);
        let over = self.pending.len().saturating_sub(MAX_PENDING);
        self.pending.drain(..over);
    }

    /// Hold back from `view` (the engine's numbers of the moment, with the menu already made from them) what has not
    /// landed at `now`: the HP and MP of every fighter and whether it stands.
    pub fn apply(&self, view: &mut BattleView, now: f64) {
        if self.pending.iter().all(|c| c.at <= now) {
            return;
        }
        for bar in view.party.iter_mut().chain(view.enemies.iter_mut()) {
            let ahead = || self.pending.iter().filter(|c| c.actor == bar.actor && c.at > now);
            let (hp, mp): (i64, i64) = ahead().fold((0, 0), |sum, c| (sum.0 + c.hp, sum.1 + c.mp));
            bar.hp = (i64::from(bar.hp) - hp).clamp(0, i64::from(bar.max_hp)) as u32;
            bar.mp = (i64::from(bar.mp) - mp).clamp(0, i64::from(bar.max_mp)) as u32;
            // The first standing change that has not landed says how it stood until then: the opposite of what it becomes.
            if let Some(next) = ahead().filter(|c| c.stands.is_some()).min_by(|a, b| a.at.total_cmp(&b.at)) {
                bar.alive = next.stands != Some(true);
            }
            // The statuses: the engine's set with the flips that have not landed undone, the latest first (a tie keeps the order
            // of the events), so that what one performance inflicted and a later one cured is seen between the two landings.
            let mut flips: Vec<&Change> = ahead().filter(|c| !c.on.is_empty() || !c.off.is_empty()).collect();
            flips.sort_by(|a, b| a.at.total_cmp(&b.at));
            for flip in flips.iter().rev() {
                bar.status = bar.status.difference(flip.on).union(flip.off);
            }
        }
    }

    /// The changes of numbers and standing still ahead of `now`, by time (tests; the statuses are [`Lag::flips_ahead`]).
    #[cfg(test)]
    pub fn ahead(&self, now: f64) -> Vec<(f64, ActorId, i64, i64, Option<bool>)> {
        let mut out: Vec<_> =
            self.pending.iter().filter(|c| c.at > now && (c.hp != 0 || c.mp != 0 || c.stands.is_some())).map(|c| (c.at, c.actor, c.hp, c.mp, c.stands)).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }

    /// The status flips still ahead of `now`, by time: when, whose, which statuses come and which go (tests).
    #[cfg(test)]
    pub fn flips_ahead(&self, now: f64) -> Vec<(f64, ActorId, StatusSet, StatusSet)> {
        let mut out: Vec<_> = self.pending.iter().filter(|c| c.at > now && (!c.on.is_empty() || !c.off.is_empty())).map(|c| (c.at, c.actor, c.on, c.off)).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }
}

/// What an event does to a fighter's numbers, given the number it carries.
type Edit = fn(&mut Vitals, u32);

/// Who an event changes and how, by the engine's own meaning of it: the fighter, the edit, and the number it carries.
/// `None` for an event that changes no number of the panel.
fn effect(event: &BattleEvent) -> Option<(ActorId, Edit, u32)> {
    /// A blow, or a turn of Poison: at most what the fighter has (the event names the damage rolled).
    fn lose(v: &mut Vitals, n: u32) {
        v.hp = v.hp.saturating_sub(n);
    }
    /// A heal, or a turn of Regen: what was restored (the event never names more than was missing).
    fn gain(v: &mut Vitals, n: u32) {
        v.hp = v.hp.saturating_add(n).min(v.max_hp);
    }
    /// A revival: back with the HP the event names.
    fn stand(v: &mut Vitals, n: u32) {
        v.hp = n.min(v.max_hp);
        v.alive = true;
    }
    fn restore(v: &mut Vitals, n: u32) {
        v.mp = v.mp.saturating_add(n).min(v.max_mp);
    }
    fn pay(v: &mut Vitals, n: u32) {
        v.mp = v.mp.saturating_sub(n);
    }
    /// A knockout: it falls, and every status goes with it (the engine says nothing of those).
    fn fall(v: &mut Vitals, _: u32) {
        v.alive = false;
        v.status = StatusSet::EMPTY;
    }
    /// A status an action inflicted (the number is its bit) or one that ended (a cure, the clock, a blow that woke it).
    fn inflict(v: &mut Vitals, n: u32) {
        if let Some(status) = u8::try_from(n).ok().and_then(Status::from_index) {
            v.status.insert(status);
        }
    }
    fn end(v: &mut Vitals, n: u32) {
        if let Some(status) = u8::try_from(n).ok().and_then(Status::from_index) {
            v.status.remove(status);
        }
    }
    Some(match *event {
        BattleEvent::Damage { target, amount, .. } | BattleEvent::StatusTick { target, status: Status::Poison, amount } => (target, lose, amount),
        BattleEvent::Heal { target, amount, .. } | BattleEvent::StatusTick { target, amount, .. } => (target, gain, amount),
        BattleEvent::Revive { target, hp, .. } => (target, stand, hp),
        BattleEvent::MpRestored { target, amount, .. } => (target, restore, amount),
        BattleEvent::MpSpent { actor, amount } => (actor, pay, amount),
        BattleEvent::Death { actor } => (actor, fall, 0),
        BattleEvent::StatusInflicted { target, status } => (target, inflict, u32::from(status.index())),
        BattleEvent::StatusEnded { target, status } => (target, end, u32::from(status.index())),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::view::{Bar, BattleView};
    use super::*;
    use fmv_ff7_battle::rules::ActionRef as A;

    fn v(hp: u32, max_hp: u32, mp: u32, max_mp: u32) -> Vitals {
        Vitals { hp, max_hp, mp, max_mp, alive: hp > 0, status: StatusSet::EMPTY }
    }

    fn dmg(actor: u8, target: u8, amount: u32) -> BattleEvent {
        BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount, critical: false }
    }

    fn act(actor: u8, action: A) -> BattleEvent {
        BattleEvent::Action { actor: ActorId(actor), action }
    }

    /// The party of 0 to 2 and the enemies 3 and 4, in the state the engine has after a batch.
    fn engine(numbers: &[(u8, Vitals)]) -> Vec<(ActorId, Vitals)> {
        numbers.iter().map(|(a, v)| (ActorId(*a), *v)).collect()
    }

    /// The panel the view would show from `state` (the engine's numbers).
    fn view_of(state: &[(ActorId, Vitals)]) -> BattleView {
        let bar = |(actor, v): &(ActorId, Vitals)| Bar { actor: *actor, name: format!("F{}", actor.0), hp: v.hp, max_hp: v.max_hp, mp: v.mp, max_mp: v.max_mp, alive: v.alive, status: v.status, ..Bar::default() };
        BattleView { party: state.iter().filter(|(a, _)| a.0 < 3).map(bar).collect(), enemies: state.iter().filter(|(a, _)| a.0 >= 3).map(bar).collect(), ..BattleView::default() }
    }

    /// What the panel shows for `actor` at `now`, from the engine's state `state` and what `lag` still holds back.
    fn shown(lag: &Lag, state: &[(ActorId, Vitals)], now: f64, actor: u8) -> (u32, u32, bool) {
        let mut view = view_of(state);
        lag.apply(&mut view, now);
        let bar = view.party.iter().chain(&view.enemies).find(|b| b.actor == ActorId(actor)).expect("a bar");
        (bar.hp, bar.mp, bar.alive)
    }

    /// The statuses the panel shows for `actor` at `now`.
    fn tags(lag: &Lag, state: &[(ActorId, Vitals)], now: f64, actor: u8) -> StatusSet {
        let mut view = view_of(state);
        lag.apply(&mut view, now);
        view.party.iter().chain(&view.enemies).find(|b| b.actor == ActorId(actor)).expect("a bar").status
    }

    const T: f64 = 50.0;

    #[test]
    fn a_blow_takes_hp_when_it_lands_not_when_it_arrives() {
        let before = engine(&[(0, v(300, 400, 20, 30)), (3, v(100, 120, 0, 0))]);
        let after = engine(&[(0, v(300, 400, 20, 30)), (3, v(70, 120, 0, 0))]);
        let events = [act(0, A::Attack), dmg(0, 3, 30)];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let lands = T + cue::impact_delay(Some(A::Attack));
        // The engine says 70 the moment the batch arrives; the panel says 100 until the blow lands.
        assert_eq!(shown(&lag, &after, T, 3), (100, 0, true));
        assert_eq!(shown(&lag, &after, lands - 0.001, 3), (100, 0, true));
        assert_eq!(shown(&lag, &after, lands, 3), (70, 0, true), "at the instant its number floats up");
        assert_eq!(shown(&lag, &after, lands + 5.0, 3), (70, 0, true));
        // Nobody else is touched.
        assert_eq!(shown(&lag, &after, T, 0), (300, 20, true));
    }

    #[test]
    fn a_counter_takes_the_partys_hp_when_it_lands_after_the_blow_that_provoked_it() {
        // Barret (1) hits the scorpion (3); its counter hits Cloud (0), Barret (1) and Tifa (2), who falls.
        let before = engine(&[(0, v(427, 427, 59, 59)), (1, v(312, 480, 22, 40)), (2, v(98, 390, 20, 50)), (3, v(55, 55, 0, 0))]);
        let after = engine(&[(0, v(307, 427, 59, 59)), (1, v(216, 480, 22, 40)), (2, v(0, 390, 20, 50)), (3, v(30, 55, 0, 0))]);
        let events = [act(1, A::Attack), dmg(1, 3, 25), act(3, A::EnemyAttack(286)), dmg(3, 0, 120), dmg(3, 1, 96), dmg(3, 2, 130), BattleEvent::Death { actor: ActorId(2) }];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let blow = T + cue::impact_delay(Some(A::Attack));
        let counter = T + cue::length_of(Some(A::Attack)) + cue::impact_delay(Some(A::EnemyAttack(286)));
        let collapse = counter + cue::DEATH_BEAT;
        assert!(blow < counter);
        // When the blow has landed: the enemy is down, the party is not touched yet (the counter has not even begun).
        assert_eq!(shown(&lag, &after, blow + 0.01, 3), (30, 0, true));
        assert_eq!(shown(&lag, &after, blow + 0.01, 0), (427, 59, true));
        assert_eq!(shown(&lag, &after, blow + 0.01, 1), (312, 22, true));
        assert_eq!(shown(&lag, &after, blow + 0.01, 2), (98, 20, true), "Tifa still has her HP and is standing");
        // When the counter has landed: the three numbers, and Tifa at 0 HP but not yet fallen (her collapse begins a beat later).
        assert_eq!(shown(&lag, &after, counter + 0.01, 0), (307, 59, true));
        assert_eq!(shown(&lag, &after, counter + 0.01, 1), (216, 22, true));
        assert_eq!(shown(&lag, &after, counter + 0.01, 2), (0, 20, true));
        assert_eq!(shown(&lag, &after, collapse + 0.01, 2), (0, 20, false), "and then the name goes dead");
        // After the whole batch the panel IS the engine's state, to the last digit.
        for who in 0..4 {
            let end = after.iter().find(|(a, _)| a.0 == who).unwrap().1;
            assert_eq!(shown(&lag, &after, collapse + 0.01, who), (end.hp, end.mp, end.alive), "actor {who}");
        }
        assert!(lag.ahead(collapse + 0.001).is_empty());
    }

    #[test]
    fn overkill_and_overheal_come_out_at_the_engines_numbers() {
        // 999 on a fighter with 40 left: the engine took 40 (the event names the damage rolled); a heal of 500 on a fighter
        // missing 30: the event names what was restored (30).
        let before = engine(&[(0, v(100, 130, 0, 0)), (3, v(40, 200, 0, 0))]);
        let after = engine(&[(0, v(130, 130, 0, 0)), (3, v(0, 200, 0, 0))]);
        let events = [
            act(0, A::Attack),
            dmg(0, 3, 999),
            BattleEvent::Death { actor: ActorId(3) },
            act(0, A::Magic(0)),
            BattleEvent::Heal { actor: ActorId(0), target: ActorId(0), amount: 30 },
        ];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let end = T + 5.0;
        assert_eq!(shown(&lag, &after, T + 0.1, 3), (40, 0, true));
        assert_eq!(shown(&lag, &after, end, 3), (0, 0, false));
        assert_eq!(shown(&lag, &after, T + 0.1, 0), (100, 0, true));
        assert_eq!(shown(&lag, &after, end, 0), (130, 0, true));
        // The first blow takes the 40 and not the 999: a bar never goes negative or up while it waits.
        let held: Vec<i64> = lag.ahead(T).iter().filter(|c| c.1 == ActorId(3) && c.2 != 0).map(|c| c.2).collect();
        assert_eq!(held, [-40]);
    }

    #[test]
    fn two_performances_on_one_fighter_step_its_bar_twice() {
        // Two enemies (3, 4) hit Cloud (0) in one batch: 100, then 60.
        let before = engine(&[(0, v(400, 400, 0, 0)), (3, v(10, 10, 0, 0)), (4, v(10, 10, 0, 0))]);
        let after = engine(&[(0, v(240, 400, 0, 0)), (3, v(10, 10, 0, 0)), (4, v(10, 10, 0, 0))]);
        let events = [act(3, A::EnemyAttack(1)), dmg(3, 0, 100), act(4, A::EnemyAttack(2)), dmg(4, 0, 60)];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let first = T + cue::impact_delay(Some(A::EnemyAttack(1)));
        let second = T + cue::length_of(Some(A::EnemyAttack(1))) + cue::impact_delay(Some(A::EnemyAttack(2)));
        assert_eq!(shown(&lag, &after, first - 0.01, 0).0, 400);
        assert_eq!(shown(&lag, &after, first + 0.01, 0).0, 300, "the first blow's 100");
        assert_eq!(shown(&lag, &after, second - 0.01, 0).0, 300);
        assert_eq!(shown(&lag, &after, second + 0.01, 0).0, 240, "and then the second's 60");
    }

    #[test]
    fn a_batch_that_arrives_while_the_last_ones_counter_is_pending_composes_with_it() {
        // Batch one: a blow and a counter on Cloud (-100). Batch two arrives before the counter has landed: Cloud drinks a potion
        // (+50). The panel must not lose either, and must end at the engine's state.
        let s0 = engine(&[(0, v(400, 500, 0, 0)), (3, v(50, 50, 0, 0))]);
        let s1 = engine(&[(0, v(300, 500, 0, 0)), (3, v(20, 50, 0, 0))]);
        let s2 = engine(&[(0, v(350, 500, 0, 0)), (3, v(20, 50, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(0, A::Attack), dmg(0, 3, 30), act(3, A::EnemyAttack(286)), dmg(3, 0, 100)], &s0, &s1);
        let counter = T + cue::length_of(Some(A::Attack)) + cue::impact_delay(Some(A::EnemyAttack(286)));
        let second_at = T + 0.2;
        assert!(second_at < counter);
        lag.on_batch(second_at, &[act(0, A::Item(0)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(0), amount: 50 }], &s1, &s2);
        let potion = second_at + cue::impact_delay(Some(A::Item(0)));
        assert!(potion < counter, "the potion lands before the counter");
        // Now: the engine says 350. Before the potion lands the panel says 400 (the counter's 100 and the potion's 50 both ahead):
        // the potion's 50 comes off the 350, the counter's 100 goes on: 350 - 50 + 100 = 400.
        assert_eq!(shown(&lag, &s2, second_at + 0.01, 0).0, 400);
        assert_eq!(shown(&lag, &s2, potion + 0.01, 0).0, 450, "the potion landed, the counter has not: 400 + 50");
        assert_eq!(shown(&lag, &s2, counter + 0.01, 0).0, 350, "and the counter: the engine's number");
        assert!(lag.ahead(counter + 0.001).is_empty());
    }

    #[test]
    fn a_bar_never_goes_over_its_maximum_while_a_heal_lands_before_the_blow_the_engine_applied_first() {
        // The engine applied the counter (-100) and then the potion (+50) on a fighter with 400 of 400; on the screen the potion
        // lands first, and 450 of 400 does not exist: the bar stays full until the counter lands, and ends at 350.
        let s0 = engine(&[(0, v(400, 400, 0, 0)), (3, v(50, 50, 0, 0))]);
        let s1 = engine(&[(0, v(300, 400, 0, 0)), (3, v(50, 50, 0, 0))]);
        let s2 = engine(&[(0, v(350, 400, 0, 0)), (3, v(50, 50, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(3, A::EnemyAttack(1)), dmg(3, 0, 100)], &s0, &s1);
        lag.on_batch(T + 0.1, &[act(0, A::Item(0)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(0), amount: 50 }], &s1, &s2);
        let potion = T + 0.1 + cue::impact_delay(Some(A::Item(0)));
        let counter = T + cue::impact_delay(Some(A::EnemyAttack(1)));
        let (first, last) = (potion.min(counter), potion.max(counter));
        for at in [T + 0.01, T + 0.2, first + 0.01, last + 0.01] {
            let hp = shown(&lag, &s2, at, 0).0;
            assert!(hp <= 400, "{at}: {hp}");
        }
        assert_eq!(shown(&lag, &s2, last + 0.01, 0).0, 350, "it ends at the engine's number");
    }

    #[test]
    fn mp_is_paid_when_the_spell_begins_and_restored_when_the_item_lands() {
        let before = engine(&[(0, v(100, 100, 30, 50)), (1, v(100, 100, 5, 50)), (3, v(90, 90, 0, 0))]);
        let after = engine(&[(0, v(100, 100, 22, 50)), (1, v(100, 100, 25, 50)), (3, v(60, 90, 0, 0))]);
        // Cloud casts (8 MP) at the enemy; Barret drinks an Ether (+20 MP) in the next performance of the batch.
        let events = [
            act(0, A::Magic(27)),
            BattleEvent::MpSpent { actor: ActorId(0), amount: 8 },
            dmg(0, 3, 30),
            act(1, A::Item(3)),
            BattleEvent::MpRestored { actor: ActorId(1), target: ActorId(1), amount: 20 },
        ];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        // The cast begins with the batch: the 8 MP are paid at once, the blow lands later.
        assert_eq!(shown(&lag, &after, T + 0.01, 0), (100, 22, true));
        assert_eq!(shown(&lag, &after, T + 0.01, 3), (90, 0, true));
        assert_eq!(shown(&lag, &after, T + cue::impact_delay(Some(A::Magic(27))) + 0.01, 3), (60, 0, true));
        // The Ether lands in its own performance, after the spell's is over.
        let ether = T + cue::length_of(Some(A::Magic(27))) + cue::impact_delay(Some(A::Item(3)));
        assert_eq!(shown(&lag, &after, ether - 0.01, 1).1, 5);
        assert_eq!(shown(&lag, &after, ether + 0.01, 1).1, 25);
    }

    #[test]
    fn a_revival_stands_a_fighter_up_when_it_lands_and_a_death_that_is_revived_in_the_same_batch_ends_standing() {
        let before = engine(&[(0, v(0, 200, 10, 10)), (3, v(40, 40, 0, 0))]);
        let after = engine(&[(0, v(60, 200, 10, 10)), (3, v(40, 40, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(1, A::Item(7)), BattleEvent::Revive { actor: ActorId(1), target: ActorId(0), hp: 60 }], &before, &after);
        let up = T + cue::impact_delay(Some(A::Item(7)));
        assert_eq!(shown(&lag, &after, up - 0.01, 0), (0, 10, false), "still down until the item lands");
        assert_eq!(shown(&lag, &after, up + 0.01, 0), (60, 10, true));
        // A rule the walk does not know: the engine took 5 HP more than the events say. It goes with the last landing.
        let mut lag = Lag::default();
        let after = engine(&[(0, v(55, 200, 10, 10)), (3, v(40, 40, 0, 0))]);
        lag.on_batch(T, &[act(1, A::Item(7)), BattleEvent::Revive { actor: ActorId(1), target: ActorId(0), hp: 60 }], &before, &after);
        assert_eq!(shown(&lag, &after, up + 0.01, 0).0, 55, "the shown number ends at the engine's, whatever the events said");
    }

    #[test]
    fn the_turns_of_poison_and_regen_change_hp_at_once_and_a_status_alone_changes_nothing() {
        use fmv_ff7_battle::status::Status;
        let before = engine(&[(0, v(100, 200, 0, 0)), (1, v(100, 200, 0, 0))]);
        let after = engine(&[(0, with(v(93, 200, 0, 0), &[Status::Sleep])), (1, v(120, 200, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(
            T,
            &[
                BattleEvent::StatusTick { target: ActorId(0), status: Status::Poison, amount: 7 },
                BattleEvent::StatusTick { target: ActorId(1), status: Status::Regen, amount: 20 },
                BattleEvent::StatusInflicted { target: ActorId(0), status: Status::Sleep },
                BattleEvent::GaugeReady { actor: ActorId(1) },
            ],
            &before,
            &after,
        );
        // Both ticks are the clock's, not an action's: nothing waits.
        assert_eq!(shown(&lag, &after, T, 0).0, 93);
        assert_eq!(shown(&lag, &after, T, 1).0, 120);
        // The status alone changes no number: nothing of HP, MP or standing is ahead, only the Sleep's tag, which comes up with
        // the short beat of an event that has no announcement.
        assert!(lag.ahead(T).is_empty());
        let beat = T + cue::impact_delay(None);
        assert_eq!(lag.flips_ahead(T).len(), 1);
        assert_eq!(tags(&lag, &after, beat - 0.01, 0), StatusSet::EMPTY);
        assert_eq!(tags(&lag, &after, beat + 0.01, 0), set(&[Status::Sleep]));
    }

    #[test]
    fn a_death_alone_waits_for_its_collapse_and_nobody_unknown_or_untouched_is_changed() {
        let before = engine(&[(0, v(0, 100, 0, 0)), (3, v(10, 10, 0, 0))]);
        let after = before.clone();
        let mut lag = Lag::default();
        // An event for an actor the engine does not list is skipped; a batch that changes nothing leaves nothing ahead.
        lag.on_batch(T, &[act(9, A::Attack), dmg(9, 8, 5), BattleEvent::Victory, BattleEvent::LimitGauge { actor: ActorId(0), value: 3 }], &before, &after);
        assert!(lag.ahead(T).is_empty());
        let mut view = view_of(&after);
        lag.apply(&mut view, T);
        assert_eq!(view, view_of(&after), "an empty lag changes nothing");
    }

    #[test]
    fn what_has_landed_is_forgotten_and_the_list_is_bounded() {
        let state = engine(&[(0, v(100, 100, 0, 0)), (3, v(50, 100, 0, 0))]);
        let before = engine(&[(0, v(100, 100, 0, 0)), (3, v(60, 100, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(0, A::Attack), dmg(0, 3, 10)], &before, &state);
        assert_eq!(lag.ahead(T).len(), 1);
        // The next batch, long after, forgets it.
        lag.on_batch(T + 10.0, &[], &state, &state);
        assert!(lag.pending.is_empty());
        for i in 0..1000 {
            lag.on_batch(T + 20.0 + f64::from(i) * 1e-6, &[act(0, A::Attack), dmg(0, 3, 1)], &before, &state);
        }
        assert!(lag.pending.len() <= MAX_PENDING);
        lag.clear();
        assert!(lag.pending.is_empty());
    }

    // ----- statuses ------------------------------------------------------------------------------------------------------------

    fn with(mut v: Vitals, list: &[Status]) -> Vitals {
        v.status = list.iter().copied().collect();
        v
    }

    fn set(list: &[Status]) -> StatusSet {
        list.iter().copied().collect()
    }

    fn inflicted(target: u8, status: Status) -> BattleEvent {
        BattleEvent::StatusInflicted { target: ActorId(target), status }
    }

    fn ended(target: u8, status: Status) -> BattleEvent {
        BattleEvent::StatusEnded { target: ActorId(target), status }
    }

    #[test]
    fn a_status_comes_up_on_the_panel_when_its_spell_lands_and_not_when_the_message_arrives() {
        // Cloud (0) puts the foe (3) to sleep: the engine says Sleep the moment the batch arrives.
        let before = engine(&[(0, v(300, 400, 20, 30)), (3, v(100, 120, 0, 0))]);
        let after = engine(&[(0, v(300, 400, 12, 30)), (3, with(v(100, 120, 0, 0), &[Status::Sleep]))]);
        let events = [act(0, A::Magic(11)), BattleEvent::MpSpent { actor: ActorId(0), amount: 8 }, inflicted(3, Status::Sleep)];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let lands = T + cue::impact_delay(Some(A::Magic(11)));
        assert_eq!(tags(&lag, &after, T, 3), StatusSet::EMPTY, "the engine has it, the panel does not yet");
        assert_eq!(tags(&lag, &after, lands - 0.001, 3), StatusSet::EMPTY);
        assert_eq!(tags(&lag, &after, lands, 3), set(&[Status::Sleep]), "at the instant its word floats up");
        assert_eq!(tags(&lag, &after, lands + 9.0, 3), set(&[Status::Sleep]));
        // Nobody else is touched, and the numbers are still the engine's (the spell's MP is paid as it begins).
        assert_eq!(tags(&lag, &after, T, 0), StatusSet::EMPTY);
        assert_eq!(shown(&lag, &after, T + 0.01, 0), (300, 12, true));
        assert_eq!(lag.flips_ahead(T).len(), 1);
    }

    #[test]
    fn a_cure_takes_a_tag_off_when_it_lands_and_the_one_it_does_not_cure_stays() {
        let poisoned = [Status::Poison, Status::Silence];
        let before = engine(&[(0, with(v(100, 200, 10, 30), &poisoned)), (3, v(10, 10, 0, 0))]);
        let after = engine(&[(0, with(v(100, 200, 7, 30), &[Status::Silence])), (3, v(10, 10, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(0, A::Magic(4)), BattleEvent::MpSpent { actor: ActorId(0), amount: 3 }, ended(0, Status::Poison)], &before, &after);
        let lands = T + cue::impact_delay(Some(A::Magic(4)));
        assert_eq!(tags(&lag, &after, lands - 0.01, 0), set(&poisoned), "still poisoned on the panel until the cure lands");
        assert_eq!(tags(&lag, &after, lands + 0.01, 0), set(&[Status::Silence]));
    }

    #[test]
    fn haste_that_cancels_slow_flips_both_tags_at_one_instant_and_a_status_inflicted_then_cured_is_seen_between_the_two() {
        let before = engine(&[(0, with(v(100, 200, 30, 30), &[Status::Slow])), (1, v(100, 200, 30, 30))]);
        let after = engine(&[(0, with(v(100, 200, 12, 30), &[Status::Haste])), (1, v(100, 200, 30, 30))]);
        let mut lag = Lag::default();
        // Slow ends and Haste begins, as the engine announces a Haste that replaces a Slow.
        lag.on_batch(T, &[act(1, A::Magic(19)), ended(0, Status::Slow), inflicted(0, Status::Haste)], &before, &after);
        let lands = T + cue::impact_delay(Some(A::Magic(19)));
        assert_eq!(tags(&lag, &after, lands - 0.01, 0), set(&[Status::Slow]));
        assert_eq!(tags(&lag, &after, lands + 0.01, 0), set(&[Status::Haste]));
        // Sleep put on by the first performance of a batch and cured by the next one: the engine ends with nothing, the panel
        // shows nothing before the first landing, the Sleep between the two, and nothing after the cure.
        let before = engine(&[(0, v(100, 200, 30, 30)), (3, v(10, 10, 0, 0))]);
        let events = [act(3, A::EnemyAttack(1)), inflicted(0, Status::Sleep), act(1, A::Item(9)), ended(0, Status::Sleep)];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &before);
        let (slept, cured) = (T + cue::impact_delay(Some(A::EnemyAttack(1))), T + cue::length_of(Some(A::EnemyAttack(1))) + cue::impact_delay(Some(A::Item(9))));
        for (at, want) in [(T, StatusSet::EMPTY), (slept - 0.01, StatusSet::EMPTY), (slept + 0.01, set(&[Status::Sleep])), (cured - 0.01, set(&[Status::Sleep])), (cured + 0.01, StatusSet::EMPTY), (T + 9.0, StatusSet::EMPTY)] {
            assert_eq!(tags(&lag, &before, at, 0), want, "at {at}");
        }
    }

    #[test]
    fn a_status_that_runs_out_on_the_clock_and_a_battle_that_starts_with_one_land_with_the_short_beat_of_an_unannounced_event() {
        let before = engine(&[(0, with(v(100, 200, 30, 30), &[Status::Barrier])), (3, v(10, 10, 0, 0))]);
        let after = engine(&[(0, v(100, 200, 30, 30)), (3, v(10, 10, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[ended(0, Status::Barrier)], &before, &after);
        let beat = T + cue::impact_delay(None);
        assert_eq!(tags(&lag, &after, beat - 0.01, 0), set(&[Status::Barrier]));
        assert_eq!(tags(&lag, &after, beat + 0.01, 0), StatusSet::EMPTY, "the same beat as the word that says it ended");
    }

    #[test]
    fn a_knockout_takes_the_tags_with_the_collapse_and_a_status_the_events_do_not_explain_goes_with_the_last_landing() {
        // Poisoned Tifa (2) is struck down by the foe (3): her tags stay until her collapse begins.
        let before = engine(&[(2, with(v(30, 390, 20, 50), &[Status::Poison, Status::Haste])), (3, v(55, 55, 0, 0))]);
        let after = engine(&[(2, v(0, 390, 20, 50)), (3, v(55, 55, 0, 0))]);
        let events = [act(3, A::EnemyAttack(286)), dmg(3, 2, 130), BattleEvent::Death { actor: ActorId(2) }];
        let mut lag = Lag::default();
        lag.on_batch(T, &events, &before, &after);
        let blow = T + cue::impact_delay(Some(A::EnemyAttack(286)));
        let collapse = blow + cue::DEATH_BEAT;
        assert_eq!(tags(&lag, &after, blow + 0.01, 2), set(&[Status::Poison, Status::Haste]), "the blow has landed, she has not fallen");
        assert_eq!(tags(&lag, &after, collapse + 0.01, 2), StatusSet::EMPTY, "and her statuses are gone with her");
        // A rule the walk does not know: the engine took a status away that no event named. It goes with the last landing.
        let before = engine(&[(0, with(v(100, 200, 30, 30), &[Status::Fury])), (3, v(10, 10, 0, 0))]);
        let after = engine(&[(0, v(100, 200, 30, 30)), (3, v(10, 10, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(3, A::EnemyAttack(1)), dmg(3, 0, 0)], &before, &after);
        let lands = T + cue::impact_delay(Some(A::EnemyAttack(1)));
        assert_eq!(tags(&lag, &after, lands - 0.01, 0), set(&[Status::Fury]));
        assert_eq!(tags(&lag, &after, lands + 0.01, 0), StatusSet::EMPTY);
    }

    #[test]
    fn two_batches_in_flight_compose_their_status_flips_and_all_end_at_the_engines_set() {
        // Batch one: a counter that poisons Cloud lands late. Batch two arrives first and puts Barrier on him.
        let s0 = engine(&[(0, v(400, 500, 30, 30)), (3, v(50, 50, 0, 0))]);
        let s1 = engine(&[(0, with(v(400, 500, 30, 30), &[Status::Poison])), (3, v(30, 50, 0, 0))]);
        let s2 = engine(&[(0, with(v(400, 500, 14, 30), &[Status::Poison, Status::Barrier])), (3, v(30, 50, 0, 0))]);
        let mut lag = Lag::default();
        lag.on_batch(T, &[act(0, A::Attack), dmg(0, 3, 20), act(3, A::EnemyAttack(286)), inflicted(0, Status::Poison)], &s0, &s1);
        let poisoned = T + cue::length_of(Some(A::Attack)) + cue::impact_delay(Some(A::EnemyAttack(286)));
        let second = T + 0.2;
        assert!(second < poisoned);
        lag.on_batch(second, &[act(0, A::Magic(15)), BattleEvent::MpSpent { actor: ActorId(0), amount: 16 }, inflicted(0, Status::Barrier)], &s1, &s2);
        let barrier = second + cue::impact_delay(Some(A::Magic(15)));
        assert!(barrier < poisoned, "the barrier lands before the counter's poison");
        assert_eq!(tags(&lag, &s2, second + 0.01, 0), StatusSet::EMPTY);
        assert_eq!(tags(&lag, &s2, barrier + 0.01, 0), set(&[Status::Barrier]), "the later flip is undone, the earlier one is not");
        assert_eq!(tags(&lag, &s2, poisoned + 0.01, 0), set(&[Status::Poison, Status::Barrier]), "the engine's set");
        assert!(lag.flips_ahead(poisoned + 0.001).is_empty());
    }
}
