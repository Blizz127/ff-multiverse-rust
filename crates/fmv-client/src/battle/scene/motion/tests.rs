//! Tests of the [`Director`]: what a performance does over time, with real clips and without, how a
//! blow lands on schedule whatever the clips, and that nothing a server sends can grow the schedule
//! without bound or break it.

use super::super::clips::testing::{cloud_like_pack, cloud_like_scripts, enemy_like_pack, enemy_like_scripts};
use super::super::clips::{party, ClipSet};
use super::super::cue::{self, DEATH_BEAT, MAX_IMPACT_DELAY};
use super::*;
use fmv_ff7::battle_model::script::Script;

const PLAIN: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look::PLAIN;

/// Two party members at z = +2000 and two enemies at z = -1500, all 150 wide.
fn procedural() -> Director {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    Director::new(sides, anchors())
}

fn anchors() -> Vec<Anchor> {
    let at = |x: f32, z: f32| Anchor { ground: [x, z], radius: 150.0 };
    vec![at(-500.0, 2000.0), at(500.0, 2000.0), at(-500.0, -1500.0), at(500.0, -1500.0)]
}

/// A pack of `n` slots of `frames` frames each, all real, none travelling, at 15 frames per second.
fn pack(n: usize, frames: u16) -> ClipSet {
    ClipSet { frames: vec![frames; n], ok: vec![true; n], travel: vec![0.0; n], lead: vec![0; n], fps: 15.0 }
}

/// The invented enemy (an MP's shapes, see `clips::testing`): attack 272 plays the gun-shaped script (a run-up that
/// carries it 900 units, poses, a long clip that holds the aim and runs home), 273 the swing-shaped one (a leap, the move
/// to the target, a swing, the recovery), 274 the spell-shaped one; its hurt clip is slot 1, and with `death_clip` its death
/// plays slot 10 (without, like most of the real ones, its death script plays nothing: it fades).
fn enemy_kit(death_clip: bool) -> Kit {
    let mut scripts = enemy_like_scripts();
    if !death_clip {
        scripts[2] = Script::default();
    }
    Kit::enemy(enemy_like_pack(), &scripts, &[(272, 3), (273, 4), (274, 5)])
}

/// The same four with real clips: the party's shared slots and blows of Cloud's shape (52 slots, `cloud_like_pack`)
/// and enemies of the shapes of `enemy_kit` (no death clip).
fn with_clips() -> Director {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let party = Kit::party(cloud_like_pack(), &cloud_like_scripts());
    let enemy = enemy_kit(false);
    Director::with_kits(sides, anchors(), vec![party.clone(), party, enemy.clone(), enemy])
}

fn damage(actor: u8, target: u8, critical: bool) -> BattleEvent {
    BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount: 42, critical }
}

fn action(actor: u8, what: ActionRef) -> BattleEvent {
    BattleEvent::Action { actor: ActorId(actor), action: what }
}

/// A plain blow: the announcement and what it did.
fn blow(attacker: u8, target: u8) -> Vec<BattleEvent> {
    vec![action(attacker, ActionRef::Attack), damage(attacker, target, false)]
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

/// Whether an effect is what lands on a target (a blow, a burst, a sign), not what its performer does (a glow it gathers, a
/// bolt or an item in the air, a guard, a fizzle, the flash of a limit break).
fn lands_on_target(kind: Kind) -> bool {
    !matches!(kind, Kind::Charge | Kind::Bolt { .. } | Kind::Toss | Kind::Guard | Kind::Fizzle | Kind::Flash)
}

/// The instants at which blows land on someone: the effects that land on targets.
fn impacts(d: &Director) -> Vec<f64> {
    d.fx().iter().filter(|f| lands_on_target(f.kind)).map(|f| f.land).collect()
}

/// The effects of a kind.
fn of_kind(d: &Director, want: fn(Kind) -> bool) -> Vec<Fx> {
    d.fx().iter().filter(|f| want(f.kind)).copied().collect()
}

const SLASH: Kind = Kind::Strike { how: effects::Strike::Slash, critical: false };
const SPELL: Style = Style::Spell;

// ----- the events and the schedule ------------------------------------------------------------

#[test]
fn a_blow_lands_the_instant_its_numbers_are_born_and_what_lands_follows_the_table() {
    for (what, delay) in [(ActionRef::Attack, 0.50), (ActionRef::EnemyAttack(272), 0.55), (ActionRef::Magic(27), 0.60), (ActionRef::Item(3), 0.45)] {
        let mut d = with_clips();
        let attacker = if matches!(what, ActionRef::EnemyAttack(_)) { 2 } else { 0 };
        d.on_batch(10.0, &[action(attacker, what), damage(attacker, 3 - attacker, false)], PLAIN);
        assert_eq!(impacts(&d).len(), 1, "{what:?}");
        assert!((impacts(&d)[0] - (10.0 + delay)).abs() < 1e-9, "{what:?}: {:?}", impacts(&d));
    }
    // A batch without an announcement lands after the short beat.
    let mut d = procedural();
    d.on_batch(10.0, &[damage(0, 2, false)], PLAIN);
    assert!((impacts(&d)[0] - (10.0 + cue::impact_delay(None))).abs() < 1e-9);
}

#[test]
fn the_overlays_numbers_and_the_scenes_impacts_are_the_same_instants_for_the_same_batch() {
    use crate::battle::hud::BattleHud;
    use crate::battle::popups::Popups;
    let cure = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure, style: SPELL };
    // Every kind of action that has something land, with the effects it can have.
    type LookFn<'a> = &'a dyn Fn(ActorId, ActionRef) -> Look;
    let batches: [(Vec<BattleEvent>, LookFn); 7] = [
        (blow(0, 2), PLAIN),
        (vec![action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), damage(2, 1, true)], PLAIN),
        (vec![action(0, ActionRef::Magic(27)), BattleEvent::MpSpent { actor: ActorId(0), amount: 4 }, damage(0, 2, false), damage(0, 3, false)], PLAIN),
        (vec![action(1, ActionRef::Magic(0)), BattleEvent::Heal { actor: ActorId(1), target: ActorId(0), amount: 80 }], &cure),
        (vec![action(0, ActionRef::Item(3)), BattleEvent::MpRestored { actor: ActorId(0), target: ActorId(1), amount: 30 }], PLAIN),
        // No announcement at all: the short beat of an older server.
        (vec![damage(0, 2, false)], PLAIN),
        // A blow and the enemy's counter to it, in one batch: one after the other, the numbers and the impacts alike.
        (vec![action(0, ActionRef::Attack), damage(0, 2, false), action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), damage(2, 1, true)], PLAIN),
    ];
    for now in [3.0, 77.123456, 1.0e6] {
        for (events, look) in &batches {
            let mut popups = Popups::default();
            popups.on_events(now, events);
            let mut scene = with_clips();
            scene.on_batch(now, events, *look);
            // The instants the numbers are born at and the instants the blows land at are the same ones.
            let mut born: Vec<f64> = popups.births().iter().map(|(_, at)| *at).collect();
            let mut landed = impacts(&scene);
            born.sort_by(f64::total_cmp);
            landed.sort_by(f64::total_cmp);
            assert!(!born.is_empty() && born == landed, "{events:?} at {now}: numbers {born:?}, impacts {landed:?}");
            // The first performance of a batch lands within the cap; a counter or any later one within the span of a batch.
            assert!(born[0] - now <= MAX_IMPACT_DELAY + 1e-6, "{events:?}: {born:?}");
            assert!(born.iter().all(|&at| at - now <= cue::MAX_BATCH_SPAN + 1e-6));
        }
    }
    // A death too: an enemy leaves the list at the instant the director begins its collapse.
    let events = [action(0, ActionRef::Attack), damage(0, 2, false), BattleEvent::Death { actor: ActorId(2) }];
    let mut hud = BattleHud::new(None);
    hud.begin(1, Some(ActorId(0)));
    hud.on_events(10.0, &events, false);
    let mut scene = with_clips();
    scene.on_batch(10.0, &events, PLAIN);
    let begins = scene.deaths[2].expect("a death is scheduled");
    let listed = |t: f64| {
        let mut view = crate::battle::BattleView {
            enemies: vec![crate::battle::Bar { actor: ActorId(2), alive: false, ..crate::battle::Bar::default() }],
            ..crate::battle::BattleView::default()
        };
        hud.decorate(&mut view, t);
        view.enemies[0].alive
    };
    assert!(listed(begins - 0.001) && !listed(begins + 0.001), "the list drops the enemy at the instant its collapse begins ({begins})");
}

/// An enemy's counter plays after the blow that provoked it, not over it: its performance begins when the blow's is over, it lands
/// after the blow has landed and its follow-through has played, and the battle's victory pose waits for it.
#[test]
fn a_counter_in_the_batch_of_the_blow_plays_after_it_and_not_at_the_same_time() {
    let melee: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look::PLAIN;
    let batch = [action(0, ActionRef::Attack), damage(0, 2, false), action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), damage(2, 1, false)];
    let counter_begins = 10.0 + cue::length_of(Some(ActionRef::Attack));
    // With clips: the blow lands at 10.5, the counter's blows (one announcement, a burst on each of the two) land together
    // after the blow's performance is over; the performer of the counter has nothing of the counter before it begins.
    let mut d = with_clips();
    d.on_batch(10.0, &batch, melee);
    let landed = impacts(&d);
    assert_eq!(landed.len(), 3, "{landed:?}");
    assert!((landed[0] - (10.0 + cue::impact_delay(Some(ActionRef::Attack)))).abs() < 1e-9, "{landed:?}");
    let counter_lands = counter_begins + cue::impact_delay(Some(ActionRef::EnemyAttack(272)));
    assert!(landed[1..].iter().all(|&at| (at - counter_lands).abs() < 1e-9), "{landed:?} against {counter_lands}");
    assert!(counter_lands >= landed[0] + cue::follow_through(Some(ActionRef::Attack)), "after the follow-through, not at once");
    assert!((d.last_impact() - counter_lands).abs() < 1e-9);
    // The enemy flinches from the blow (its hurt clip, from the impact) and only then begins its own performance.
    let started: Vec<f64> = d.segments_of(2).iter().map(|s| s.start).collect();
    assert!(started.iter().filter(|&&at| at < counter_begins - 1e-9).count() == 1, "only the hurt clip before the counter begins: {started:?}");
    assert!(started.iter().any(|&at| at >= counter_begins - 1e-9), "its own performance begins with the counter: {started:?}");
    // Without clips (a lunge): the enemy stands where it is until the counter begins, then runs up.
    let mut d = procedural();
    d.on_batch(10.0, &batch, melee);
    assert_eq!(d.motion(2, counter_begins - 0.05).offset[0], 0.0);
    assert_eq!(d.motion(2, counter_begins - 0.05).offset[2], 0.0, "it stands where it is while the blow plays");
    let moved = (1..40).map(|k| d.motion(2, counter_begins + k as f64 * 0.01).offset).any(|o| o.iter().any(|&c| c.abs() > 1.0));
    assert!(moved, "it runs up once its turn comes");
    // A victory in the same batch rejoices after the LAST blow has landed (the counter's), not the first.
    let mut won = with_clips();
    let mut events = batch.to_vec();
    events.push(BattleEvent::Victory);
    won.on_batch(10.0, &events, melee);
    let first_pose = (0..2).filter_map(|i| won.victories[i]).fold(f64::INFINITY, f64::min);
    assert!(first_pose >= counter_lands, "the party rejoices after the counter: {first_pose} against {counter_lands}");
}

#[test]
fn no_blow_lands_later_than_the_cap_whatever_its_clips_its_reach_or_what_its_actor_was_doing() {
    // Very long clips (200 frames: 13 seconds), every kind of action, at several distances, with the
    // actor busy with an earlier performance that is still running.
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let party = Kit::party(pack(52, 200), &cloud_like_scripts());
    let mut long = enemy_like_pack();
    long.frames = vec![200; long.frames.len()];
    let enemy = Kit::enemy(long, &enemy_like_scripts(), &[(272, 3), (273, 4), (274, 5)]);
    for far in [0.0f32, 500.0, 3000.0, 20_000.0] {
        let anchors = vec![Anchor { ground: [0.0, 0.0], radius: 150.0 }, Anchor { ground: [10.0, 0.0], radius: 150.0 }, Anchor { ground: [0.0, -far], radius: 150.0 }, Anchor { ground: [0.0, -far], radius: 150.0 }];
        let mut d = Director::with_kits(sides.clone(), anchors, vec![party.clone(), party.clone(), enemy.clone(), enemy.clone()]);
        let mut now = 100.0;
        for (i, what) in [ActionRef::Attack, ActionRef::Magic(27), ActionRef::Item(3), ActionRef::Defend, ActionRef::Attack].into_iter().enumerate() {
            let before = d.fx().len();
            // The same actor again before the last one is over: its performance takes over.
            d.on_batch(now, &[action(0, what), damage(0, 2, i % 2 == 0)], PLAIN);
            // Every shape of the enemy's scripts (the gun, the swing, the spell) in turn.
            let attack = [272, 273, 274][i % 3];
            d.on_batch(now + 0.01, &[action(2, ActionRef::EnemyAttack(attack)), damage(2, 1, false)], PLAIN);
            for fx in &d.fx()[before..] {
                assert!(fx.land <= now + 0.01 + MAX_IMPACT_DELAY + 1e-9, "{what:?} at {far}: the effect lands at {} after arrival {now}", fx.land);
                assert!(fx.land >= now && fx.start >= now, "never before the batch arrived");
                assert!(fx.start <= fx.land, "it begins before it lands");
            }
            now += 0.2;
        }
        // And what each actor plays at those instants is a clip (the new performance's), never nothing odd.
        for t in (0..40).map(|k| 100.0 + k as f64 * 0.05) {
            for i in 0..4 {
                let m = d.motion(i, t);
                assert!(m.offset.iter().all(|c| c.is_finite()) && m.alpha.is_finite());
                if let Some(c) = m.clip {
                    assert!(c.frame.is_finite() && c.frame >= 0.0 && c.frame <= 199.0, "{c:?}");
                }
            }
        }
    }
}

#[test]
fn a_flood_of_batches_does_not_make_the_schedule_grow_and_none_of_them_lands_late() {
    let mut d = with_clips();
    for k in 0..500 {
        let t = 10.0 + k as f64 * 0.001;
        d.on_batch(t, &blow(0, 2), PLAIN);
        d.on_batch(t, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), BattleEvent::Death { actor: ActorId(3) }], PLAIN);
    }
    assert!(d.segments.iter().all(|s| s.len() <= MAX_SEGMENTS), "{:?}", d.segments.iter().map(Vec::len).collect::<Vec<_>>());
    assert!(d.pending() <= MAX_EFFECTS && d.fx().len() <= MAX_FX, "{} effects, {} pictures", d.pending(), d.fx().len());
    let last = 10.0 + 0.499;
    assert!(impacts(&d).iter().all(|&at| at <= last + MAX_IMPACT_DELAY + 1e-9), "{:?}", impacts(&d));
    // A death still counts, whatever was dropped.
    assert!(d.deaths[3].is_some());
    // Everything is over a few seconds later.
    d.prune(20.0);
    assert!(d.pending() == 0 && d.fx().is_empty() && d.segments.iter().all(Vec::is_empty));
}

#[test]
fn a_new_performance_of_an_actor_takes_over_from_its_start() {
    let mut d = with_clips();
    d.on_batch(10.0, &blow(0, 2), PLAIN);
    // Mid-swing of the first, the same actor casts: from the cast's own start its clips are the cast's.
    d.on_batch(10.2, &[action(0, ActionRef::Magic(27)), damage(0, 2, false)], PLAIN);
    let cast_begin = d.segments[0].iter().find(|s| s.slot == 12).expect("the cast begins").start;
    assert!(cast_begin >= 10.2 - 1e-9);
    let at = |t: f64| d.motion(0, t).clip.map(|c| c.slot);
    assert!(matches!(at(10.15), Some(26 | 27 | 30)), "still the first blow's clips: {:?}", at(10.15));
    assert_eq!(at(cast_begin + 0.05), Some(12), "the cast has taken over");
}

#[test]
fn a_party_members_blow_plays_its_attack_the_target_flinches_and_a_burst_flashes_at_the_impact() {
    let mut d = with_clips();
    d.on_batch(10.0, &blow(0, 2), PLAIN);
    let impact = 10.5;
    // The attacker plays its own script's clips (Cloud's shape: crouch 26, run 27, chop 30, recover 28, back 29); at the
    // impact it is two ticks into the chop, which is the tick its script marks the damage at. Slot 10, the item thrown at
    // an enemy that the first version took for the attack, is never played.
    assert_eq!(d.motion(0, 10.01).clip.map(|c| c.slot), Some(26));
    let swing = d.motion(0, impact - 0.001).clip.expect("swinging");
    assert_eq!(swing.slot, 30);
    assert!(near(swing.frame, 2.0, 0.05), "the blow lands at the impact: {swing:?}");
    assert!(d.segments_of(0).iter().all(|s| s.slot != 10));
    assert_eq!(d.motion(0, impact + 0.1).clip.map(|c| c.slot), Some(30), "the chop goes on");
    assert_eq!(d.motion(0, impact + 0.45).clip.map(|c| c.slot), Some(28), "then the recovery");
    // The target starts its hurt clip (slot 1 of an enemy) at the impact, not before.
    assert_eq!(d.motion(2, impact - 0.01).clip, None);
    assert_eq!(d.motion(2, impact + 0.05).clip.map(|c| c.slot), Some(1));
    assert!(d.motion(2, impact + 0.05).visible, "a hurt clip is the flinch: no blinking");
    // The slashes and sparks are at the target from the impact, and nothing else is shown (a blow has no lead).
    assert_eq!(d.fx().len(), 1);
    assert_eq!((d.fx()[0].target, d.fx()[0].kind), (2, SLASH));
    assert_eq!((d.fx()[0].start, d.fx()[0].land), (impact, impact));
    // It runs up to its target and is back home when it is over.
    assert!(d.motion(0, impact).offset[2] < -1000.0, "it ran at its target (to -z): {:?}", d.motion(0, impact));
    d.prune(20.0);
    assert_eq!(d.motion(0, 20.0), Motion::default());
}

#[test]
fn a_long_range_blow_hops_forward_and_no_further_and_an_item_at_an_enemy_is_the_throw_forward() {
    let mut d = with_clips();
    let shoot = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Plain, style: Style::Shot };
    d.on_batch(10.0, &blow(0, 2), &shoot);
    let impact = 10.5;
    // A shot: a bolt flies from the shooter to the target and lands with the blow, which is a shot's small impact.
    let bolts = of_kind(&d, |k| matches!(k, Kind::Bolt { hit: true }));
    assert_eq!(bolts.len(), 1);
    assert_eq!((bolts[0].from, bolts[0].target, bolts[0].land), (Some(0), 2, impact));
    assert!(bolts[0].start >= 10.0 && bolts[0].start < impact, "it leaves the muzzle before it lands: {bolts:?}");
    assert_eq!(of_kind(&d, |k| matches!(k, Kind::Impact { critical: false })).len(), 1);
    // The clips of its other script: the hop forward (3), the shot (31), the hop back (4); it never runs.
    let slots: Vec<Slot> = d.segments_of(0).iter().map(|s| s.slot).collect();
    assert_eq!(slots, [3, 31, 31, 4]);
    let o = d.motion(0, impact).offset;
    let went = o[0].hypot(o[2]);
    assert!(went > 100.0 && went <= party::HOP + 1.0, "it hopped towards the target and did not run to it ({went}, the target is 3 000 units away)");
    // The impact is where it always is, and the target flinches then.
    assert_eq!(impacts(&d).len(), 1);
    assert!((impacts(&d)[0] - impact).abs() < 1e-9);
    assert_eq!(d.motion(2, impact + 0.05).clip.map(|c| c.slot), Some(1));
    // It is home when it is over.
    d.prune(20.0);
    assert_eq!(d.motion(0, 20.0), Motion::default());
    // An item thrown at an enemy is the throw forward (slot 10); used on an ally, the gesture with the arm raised (9).
    let mut d = with_clips();
    d.on_batch(1.0, &[action(0, ActionRef::Item(3)), damage(0, 2, false)], PLAIN);
    assert_eq!(d.motion(0, 1.2).clip.map(|c| c.slot), Some(10));
    let mut d = with_clips();
    d.on_batch(1.0, &[action(0, ActionRef::Item(3)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 50 }], PLAIN);
    assert_eq!(d.motion(0, 1.2).clip.map(|c| c.slot), Some(9));
}

#[test]
fn an_enemys_attack_plays_the_clips_of_the_script_scene_bin_names_and_goes_as_far_as_the_script_says() {
    let mut d = with_clips();
    // Attack 272 plays the gun-shaped script: the run-up (2), two poses (3, 4), the long clip (5) that holds the aim and
    // then runs home. The shot lands 4 ticks into the long clip, at the impact.
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false)], PLAIN);
    let slots: Vec<Slot> = d.segments[2].iter().map(|s| s.slot).collect();
    assert_eq!(slots, [2, 3, 4, 5, 5, 5]);
    assert_eq!(d.motion(2, 5.01).clip.map(|c| c.slot), Some(2));
    let firing = d.motion(2, 5.55 - 0.001).clip.expect("firing");
    assert_eq!(firing.slot, 5);
    assert!(near(firing.frame, 4.0, 0.1), "the shot lands 4 ticks into the long clip: {firing:?}");
    // Nothing sends it to its target: it stops where the run-up took it (900 units) though the target is 3 600 away.
    let o = d.motion(2, 5.55).offset;
    assert!(near(o[0].hypot(o[2]), 900.0, 1.0), "{o:?}");
    // A hit on a party member plays its hurt clip (slot 15).
    assert_eq!(d.motion(1, 5.6).clip.map(|c| c.slot), Some(15));
    // Attack 273 plays the swing-shaped script (a leap, a wind-up, the swing, the recovery) and it does go to its target.
    let mut d = with_clips();
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(273)), damage(2, 1, false)], PLAIN);
    let slots: Vec<Slot> = d.segments[2].iter().map(|s| s.slot).collect();
    assert_eq!(slots, [6, 7, 8, 8, 9]);
    let o = d.motion(2, 5.55).offset;
    assert!(o[0].hypot(o[2]) > 3000.0, "the move to the target took it there: {o:?}");
    // An attack the table does not know plays the enemy's first attack's script.
    let mut d = with_clips();
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(9999)), damage(2, 1, false)], PLAIN);
    assert_eq!(d.segments[2].iter().map(|s| s.slot).collect::<Vec<_>>(), [2, 3, 4, 5, 5, 5]);
    // So does a plain attack (an announcement of `Attack` by an enemy).
    let mut d = with_clips();
    d.on_batch(5.0, &blow(2, 1), PLAIN);
    assert!(d.segments[2].iter().any(|s| s.slot == 5));
}

#[test]
fn an_enemys_spell_is_cast_where_it_stands_and_lands_at_the_effect() {
    let mut d = with_clips();
    let fire = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Fire, style: SPELL };
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(274)), damage(2, 1, false)], &fire);
    // The gathering (11, 12, 13) up to the effect at the impact, then the end (14).
    let slots: Vec<Slot> = d.segments[2].iter().map(|s| s.slot).collect();
    assert_eq!(slots, [11, 12, 13, 14]);
    assert_eq!(d.motion(2, 5.55 - 0.001).clip.map(|c| c.slot), Some(13));
    assert_eq!(d.motion(2, 5.55 + 0.05).clip.map(|c| c.slot), Some(14));
    assert_eq!(d.pending(), 0, "it does not run anywhere");
    // The burst is at the target at the impact.
    assert!(impacts(&d).iter().all(|&at| (at - 5.55).abs() < 1e-9));
}

#[test]
fn an_attack_whose_script_cannot_be_played_moves_procedurally_and_does_not_borrow_anothers() {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    // Attack 272 plays script 3; 275 names script 6 (it strikes with a stand-in clip), 276 script 7 (it marks nothing) and
    // 277 a script that is not there.
    let enemy = Kit::enemy(enemy_like_pack(), &enemy_like_scripts(), &[(272, 3), (275, 6), (276, 7), (277, 40)]);
    let mut d = Director::with_kits(sides, anchors(), vec![Kit::none(), Kit::none(), enemy.clone(), enemy]);
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false)], PLAIN);
    assert!(d.segments_of(2).iter().any(|s| s.slot == 5), "the playable attack plays its script's clips");
    for bad in [275u16, 276, 277] {
        let mut d = d.clone();
        d.on_batch(9.0, &[action(3, ActionRef::EnemyAttack(bad)), damage(3, 1, false)], PLAIN);
        assert!(d.segments_of(3).is_empty(), "attack {bad} plays none of attack 272's clips: {:?}", d.segments_of(3));
        let run = d.effects.iter().filter(|e| matches!(e, Effect::Lunge { actor: 3, .. })).count();
        assert_eq!(run, 1, "it runs up to its target like an actor without clips");
        // It lands on schedule all the same.
        assert!(d.fx().iter().any(|f| (f.land - (9.0 + cue::impact_delay(Some(ActionRef::EnemyAttack(bad))))).abs() < 1e-9));
    }
    // An attack the scene does not name for it at all (or a plain attack) plays the first attack's script.
    let mut named_nowhere = d.clone();
    named_nowhere.on_batch(20.0, &[action(3, ActionRef::EnemyAttack(999)), damage(3, 1, false)], PLAIN);
    assert!(named_nowhere.segments_of(3).iter().any(|s| s.slot == 5));
}

#[test]
fn a_miss_is_a_step_aside_and_a_small_whiff_beside_the_target() {
    let mut d = with_clips();
    d.on_batch(3.0, &[action(0, ActionRef::Attack), BattleEvent::Miss { actor: ActorId(0), target: ActorId(3) }], PLAIN);
    assert_eq!(d.fx().len(), 1);
    assert_eq!((d.fx()[0].kind, d.fx()[0].target, d.fx()[0].land), (Kind::Whiff, 3, 3.5));
    let mid = d.motion(3, 3.5 + f64::from(DODGE) * 0.5);
    assert!(near(mid.offset[0].hypot(mid.offset[2]), DODGE_DISTANCE, 1.0), "{mid:?}");
    assert_eq!(mid.clip, None, "a dodge plays no hurt clip");
}

// ----- spells, items, guards ------------------------------------------------------------------

#[test]
fn a_cast_gathers_a_glow_begins_to_the_impact_and_breaks_in_the_elements_colour() {
    let mut d = with_clips();
    let fire = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Fire, style: SPELL };
    d.on_batch(10.0, &[action(0, ActionRef::Magic(27)), BattleEvent::MpSpent { actor: ActorId(0), amount: 4 }, damage(0, 2, false)], &fire);
    let charges = of_kind(&d, |k| k == Kind::Charge);
    assert_eq!(charges.len(), 1);
    assert_eq!((charges[0].target, charges[0].element, charges[0].scale), (0, Element::Fire, 1.0));
    assert!((charges[0].start - 10.0).abs() < 1e-9 && (charges[0].land - 10.6).abs() < 1e-9, "{charges:?}");
    assert_eq!(d.motion(0, 10.1).clip.map(|c| c.slot), Some(12));
    assert_eq!(d.motion(0, 10.7).clip.map(|c| c.slot), Some(13), "holds the pose as the spell breaks");
    // The burst is a spell's, in the colour of fire, on the target, at the impact; the target flinches.
    let blasts = of_kind(&d, |k| k == Kind::Blast);
    assert_eq!(blasts.len(), 1);
    assert_eq!((blasts[0].element, blasts[0].target), (Element::Fire, 2));
    assert!((blasts[0].land - 10.6).abs() < 1e-9);
    // It gathers over the target from the start of the performance and peaks at the impact.
    assert!((blasts[0].start - 10.0).abs() < 1e-9, "{blasts:?}");
    assert_eq!(d.motion(2, 10.65).clip.map(|c| c.slot), Some(1));
}

#[test]
fn a_heal_bursts_green_and_an_items_effect_is_an_items_and_a_guard_is_the_guard_clip_once() {
    let mut d = with_clips();
    let cure = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure, style: SPELL };
    d.on_batch(1.0, &[action(0, ActionRef::Magic(0)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 80 }], &cure);
    let mends = of_kind(&d, |k| k == Kind::Mend);
    assert_eq!(mends.len(), 1);
    assert_eq!((mends[0].element, mends[0].target, mends[0].scale), (Element::Cure, 1, 1.0));
    assert_eq!(d.motion(1, 1.7).clip, None, "a heal is no flinch");
    // An item: tossed from its user to the target, a lesser restoring effect there.
    let mut d = with_clips();
    let item = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure, style: Style::Item };
    d.on_batch(1.0, &[action(0, ActionRef::Item(3)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 50 }], &item);
    let mends = of_kind(&d, |k| k == Kind::Mend);
    assert!(mends.len() == 1 && mends[0].scale < 1.0, "{mends:?}");
    let tosses = of_kind(&d, |k| k == Kind::Toss);
    assert_eq!(tosses.len(), 1);
    assert_eq!((tosses[0].from, tosses[0].target), (Some(0), 1));
    assert!(tosses[0].start > 1.0 && tosses[0].start < tosses[0].land && (tosses[0].land - (1.0 + cue::impact_delay(Some(ActionRef::Item(3))))).abs() < 1e-9);
    assert_eq!(d.motion(0, 1.2).clip.map(|c| c.slot), Some(9), "the item gesture");
    let mut d = with_clips();
    d.on_batch(1.0, &[action(1, ActionRef::Defend)], PLAIN);
    // The guard is script 8's clip (5: 10 frames), played once at its own speed (0.4 s in it is on its 6th frame), never the
    // cover script's held pose (8).
    let guarding = d.motion(1, 1.4).clip.expect("guarding");
    assert_eq!(guarding.slot, 5);
    assert!(near(guarding.frame, 6.0, 0.1), "{guarding:?}");
    assert!(d.segments_of(1).iter().all(|s| s.slot != 8 && !s.looping));
    // A guard shimmers at the guard, at once, and that is all it shows.
    assert_eq!(d.fx().len(), 1);
    assert_eq!((d.fx()[0].kind, d.fx()[0].target, d.fx()[0].start, d.fx()[0].land), (Kind::Guard, 1, 1.0, 1.0));
    assert_eq!(d.motion(1, 1.0 + 10.0 / 15.0 + 0.05).clip, None, "and it is over with the clip: back to the stance");
}

#[test]
fn an_enemy_with_a_death_clip_plays_it_from_the_start_of_its_death_and_fades_when_the_fall_has_been_seen() {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let party = Kit::party(cloud_like_pack(), &cloud_like_scripts());
    let enemy = enemy_kit(true);
    let mut d = Director::with_kits(sides, anchors(), vec![party.clone(), party, enemy.clone(), enemy]);
    d.on_batch(1.0, &[action(0, ActionRef::Attack), damage(0, 2, false), BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    let begins = 1.0 + 0.5 + DEATH_BEAT;
    // Until its death begins it flinches (its hurt clip from the blow) and is whole.
    assert_eq!(d.motion(2, begins - 0.01).clip.map(|c| c.slot), Some(1));
    assert_eq!(d.motion(2, begins - 0.01).alpha, 1.0);
    // From the start of the death: the death clip (slot 10: 16 frames) at its own speed, whole while the fall is seen
    // (half the clip: 0.5 s), then the alpha falls steadily and the enemy is gone when the fade is over.
    let m = d.motion(2, begins + 0.2);
    assert_eq!(m.clip.map(|c| c.slot), Some(10));
    assert!(near(m.clip.map_or(0.0, |c| c.frame), 3.0, 0.1) && m.alpha == 1.0 && m.squash == 1.0, "{m:?}");
    let fade_from = 0.5;
    let mut last = 1.0;
    for step in 1..=11 {
        let m = d.motion(2, begins + fade_from + f64::from(FADE) * f64::from(step) / 12.0);
        assert!(m.alpha < last && m.alpha > 0.0 && !m.gone && m.visible, "fading at step {step}: {m:?}");
        last = m.alpha;
    }
    let m = d.motion(2, begins + fade_from + f64::from(FADE) + 0.01);
    assert!(m.gone && m.alpha < 0.01, "{m:?}");
    // The clip ends on its last frame (a second in) and stays there while the fade goes on.
    assert_eq!(d.motion(2, begins + 1.05).clip.map(|c| c.frame), Some(15.0));
    // The enemy that fell has no death clip of its own here (its death script plays nothing) fades as before: the hurt clip.
    let mut plain = with_clips();
    plain.on_batch(1.0, &[action(0, ActionRef::Attack), damage(0, 2, false), BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    assert_eq!(plain.motion(2, begins + 0.2).clip.map(|c| c.slot), Some(1));
    assert!(plain.motion(2, begins + 0.2).alpha < 1.0);
}

// ----- deaths, fades, victory -------------------------------------------------------------------

#[test]
fn a_party_member_who_dies_plays_its_death_clip_and_lies_in_the_dead_pose_until_revived() {
    let mut d = with_clips();
    d.on_batch(0.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false), BattleEvent::Death { actor: ActorId(1) }], PLAIN);
    let start = 0.55 + DEATH_BEAT;
    assert!((d.deaths[1].unwrap() - start).abs() < 1e-9);
    assert_ne!(d.motion(1, start - 0.01).clip.map(|c| c.slot), Some(18), "not before the blow has landed and flinched");
    let falling = d.motion(1, start + 0.2);
    assert_eq!(falling.clip.map(|c| c.slot), Some(18), "{falling:?}");
    assert!(near(falling.clip.unwrap().frame, 0.2 * 15.0, 0.1), "the clip runs at its own speed");
    assert_eq!((falling.fall, falling.squash, falling.alpha), (0.0, 1.0, 1.0), "the clip is the fall: no tipping over");
    // After the clip: the dead pose, held.
    d.prune(start + 100.0);
    let down = d.motion(1, start + 100.0);
    assert_eq!(down.clip, Some(ClipPose { slot: 6, frame: 0.0 }));
    assert!(!down.gone && down.visible);
    // The engine says alive again: standing.
    d.sync(start + 101.0, |i| Some(i != 2 && i != 3));
    assert_eq!(d.motion(1, start + 101.0).clip, None);
}

#[test]
fn an_enemy_with_clips_flinches_and_fades_away_with_a_real_alpha_and_is_gone_at_the_end() {
    let mut d = with_clips();
    d.on_batch(1.0, &[action(0, ActionRef::Attack), damage(0, 2, false), BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    let begins = 1.0 + 0.5 + DEATH_BEAT;
    assert_eq!(d.motion(2, begins - 0.01).alpha, 1.0, "whole until its collapse begins");
    assert_eq!(d.motion(2, 1.5 + 0.05).clip.map(|c| c.slot), Some(1), "its hurt clip plays from the blow");
    // Then the alpha falls steadily to nothing, with no blinking and no squashing.
    let mut last = 1.0;
    for step in 1..=11 {
        let m = d.motion(2, begins + f64::from(FADE) * f64::from(step) / 12.0);
        assert!(m.alpha < last && m.alpha > 0.0 && !m.gone, "fading at step {step}: {m:?}");
        assert!(m.visible && m.squash == 1.0);
        last = m.alpha;
    }
    let gone = d.motion(2, begins + f64::from(FADE) + 0.01);
    assert!(gone.gone && gone.alpha < 0.01);
    // The survivor is untouched by any of it.
    assert_eq!(d.motion(3, begins + 0.3), Motion::default());
}

#[test]
fn when_the_battle_is_won_the_standing_party_rejoices_after_the_last_blow_one_after_the_other() {
    let mut d = with_clips();
    let mut events = blow(0, 2);
    events.push(BattleEvent::Death { actor: ActorId(2) });
    events.push(BattleEvent::Victory);
    d.on_batch(10.0, &events, PLAIN);
    let first = 10.5 + 0.45;
    assert_ne!(d.motion(0, first - 0.05).clip.map(|c| c.slot), Some(2), "not before the last blow has landed");
    let m = d.motion(0, first + 0.3);
    assert_eq!(m.clip.map(|c| c.slot), Some(2), "{m:?}");
    // Staggered: the second member starts later, and a pose that is over is held on its last frame.
    assert_ne!(d.motion(1, first + 0.05).clip.map(|c| c.slot), Some(2));
    assert_eq!(d.motion(1, first + 5.0).clip, Some(ClipPose { slot: 2, frame: 14.0 }));
    // A fallen member does not rejoice.
    let mut d = with_clips();
    d.on_batch(10.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false), BattleEvent::Death { actor: ActorId(1) }, BattleEvent::Victory], PLAIN);
    assert_ne!(d.motion(1, 30.0).clip.map(|c| c.slot), Some(2));
    assert_eq!(d.motion(0, 30.0).clip.map(|c| c.slot), Some(2));
}

// ----- an actor without clips: the procedural motion --------------------------------------------

#[test]
fn without_clips_a_blow_makes_the_attacker_run_up_and_back_and_the_target_blink_and_shake() {
    let mut d = procedural();
    d.on_batch(10.0, &blow(0, 3), PLAIN);
    let impact = 10.5;
    assert_eq!(d.motion(0, 9.9), Motion::default());
    let Some(Effect::Lunge { lunge, reach, .. }) = d.effects.iter().copied().find(|e| matches!(e, Effect::Lunge { .. })) else { panic!("a run") };
    // Distance 3500 less both radii and the gap: the attacker ends up that much closer by the impact.
    let dist = (1000.0f32).hypot(3500.0);
    assert!(near(reach, dist - 300.0 - LUNGE_GAP, 1.0));
    let there = d.motion(0, impact);
    assert!(near(there.offset[0].hypot(there.offset[2]), reach, 1.0), "it has arrived at the impact: {there:?}");
    assert!(there.offset[2] < 0.0 && there.offset[0] > 0.0, "towards -z and the target's x: {there:?}");
    assert!(there.clip.is_none(), "a stand-in plays no clip");
    // It hops on the way (up is negative y) and is on the ground when it holds.
    let mid_run = d.motion(0, lunge.start + lunge.out * 0.5);
    assert!(mid_run.offset[1] < -100.0, "{mid_run:?}");
    assert!(there.offset[1].abs() < 1.0);
    // The target blinks, hidden for a half blink, drawn for the next, and is whole again after.
    let visible = |u: f64| d.motion(3, impact + u).visible;
    assert!(!visible(0.01) && visible(0.07) && !visible(0.13) && visible(0.19));
    assert_eq!(d.motion(3, impact - 0.01), Motion::default());
    assert_eq!(d.motion(3, impact + f64::from(FLINCH_BLINK) + 0.05), Motion::default());
    // Home again when it is over.
    d.prune(30.0);
    assert_eq!(d.motion(0, 30.0), Motion::default());
}

#[test]
fn a_critical_blow_blinks_longer_and_shakes_harder() {
    let (mut normal, mut critical) = (procedural(), procedural());
    normal.on_batch(5.0, &[action(0, ActionRef::Attack), damage(0, 2, false)], PLAIN);
    critical.on_batch(5.0, &[action(0, ActionRef::Attack), damage(0, 2, true)], PLAIN);
    let impact = 5.5;
    assert!(!critical.motion(2, impact + 0.50).visible && normal.motion(2, impact + 0.50).visible);
    let magnitude = |m: Motion| m.offset[0].hypot(m.offset[2]);
    assert!(magnitude(critical.motion(2, impact + 0.02)) > magnitude(normal.motion(2, impact + 0.02)));
    assert_eq!(critical.fx()[0].kind, Kind::Strike { how: effects::Strike::Slash, critical: true });
    assert_eq!(normal.fx()[0].kind, SLASH);
}

#[test]
fn an_enemy_without_clips_blinks_and_collapses_and_a_party_member_falls_over() {
    let mut d = procedural();
    d.on_batch(1.0, &[action(0, ActionRef::Attack), damage(0, 2, false), BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    let begins = 1.0 + 0.5 + DEATH_BEAT;
    assert!(!d.is_dying_or_dead(2, begins - 0.01) && d.is_dying_or_dead(2, begins + 0.01));
    // Flattens, monotonically, blinking, and is gone at the end.
    let mut last = 1.0;
    for step in 1..=16 {
        let m = d.motion(2, begins + f64::from(COLLAPSE) * f64::from(step) / 17.0);
        assert!(m.squash < last && !m.gone, "{m:?}");
        last = m.squash;
    }
    assert!(last < 0.2);
    assert!(d.motion(2, begins + f64::from(COLLAPSE) + 0.01).gone);
    let phases: Vec<bool> = (0..20).map(|k| d.motion(2, begins + 0.01 + f64::from(k) * 0.03).visible).collect();
    assert!(phases.contains(&true) && phases.contains(&false), "{phases:?}");
    // A party member without a death clip falls over, and stays down until revived.
    let mut d = procedural();
    d.on_batch(0.0, &[action(2, ActionRef::Attack), damage(2, 1, false), BattleEvent::Death { actor: ActorId(1) }], PLAIN);
    let start = d.deaths[1].unwrap();
    let falling = d.motion(1, start + f64::from(FALL) * 0.5);
    assert!(falling.fall > 0.2 && falling.fall < 0.8 && falling.squash == 1.0 && !falling.gone, "{falling:?}");
    d.prune(start + 100.0);
    let down = d.motion(1, start + 100.0);
    assert_eq!((down.fall, down.gone, down.visible), (1.0, false, true));
    d.sync(start + 101.0, |i| Some(i != 0 && i != 2 && i != 3));
    assert_eq!(d.motion(1, start + 101.0).fall, 0.0, "no longer down");
}

#[test]
fn a_death_nobody_saw_is_already_over_and_no_engine_changes_nothing() {
    let mut d = procedural();
    // A scene that starts late: the engine already says enemy 3 is dead.
    d.sync(50.0, |i| Some(i != 3));
    assert!(d.motion(3, 50.0).gone, "{:?}", d.motion(3, 50.0));
    assert_eq!(d.motion(2, 50.0), Motion::default());
    // With nothing to ask, nothing changes either way.
    let mut e = procedural();
    e.sync(1.0, |_| None);
    assert!(e.deaths.iter().all(Option::is_none));
    // A death the director already plays is not restarted by the engine saying dead, nor by another death event.
    let mut f = procedural();
    f.on_batch(1.0, &[BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    let started = f.deaths[2];
    f.sync(1.1, |i| Some(i != 2));
    assert_eq!(f.deaths[2], started);
    f.on_batch(9.0, &[BattleEvent::Death { actor: ActorId(2) }], PLAIN);
    assert_eq!(f.deaths[2], started);
}

#[test]
fn nonsense_never_breaks_anything() {
    for mut d in [procedural(), with_clips()] {
        // Actors the scene does not have, an attacker that is its own target, a target on top of it.
        d.on_batch(0.0, &[action(99, ActionRef::Attack), damage(99, 2, false), damage(0, 99, false)], PLAIN);
        d.on_batch(0.0, &[BattleEvent::Miss { actor: ActorId(200), target: ActorId(0) }, BattleEvent::Death { actor: ActorId(77) }], PLAIN);
        d.on_batch(1.0, &[action(2, ActionRef::Attack), damage(2, 2, false)], PLAIN);
        d.on_batch(f64::NAN, &blow(0, 2), PLAIN);
        d.on_batch(f64::INFINITY, &blow(0, 2), PLAIN);
        for t in [-5.0, 0.0, 1.0, 1.3, 2.0, 1e9] {
            for i in 0..6 {
                let m = d.motion(i, t);
                assert!(m.offset.iter().all(|c| c.is_finite()) && m.squash.is_finite() && m.fall.is_finite() && m.alpha.is_finite(), "{i} at {t}: {m:?}");
                if let Some(c) = m.clip {
                    assert!(c.frame.is_finite(), "{c:?}");
                }
            }
        }
    }
    // No actors, no panics.
    let mut none = Director::new(vec![], vec![]);
    none.on_batch(0.0, &blow(0, 1), PLAIN);
    none.sync(0.0, |_| Some(false));
    none.prune(1.0);
    assert_eq!(none.motion(0, 0.0), Motion::default());
    // Mismatched lists are cut to the shorter.
    let uneven = Director::new(vec![Side::Party; 3], vec![Anchor { ground: [0.0; 2], radius: 1.0 }; 2]);
    assert_eq!(uneven.deaths.len(), 2);
}

#[test]
fn idle_sway_is_small_distinct_per_actor_and_periodic() {
    for index in 0..4 {
        for step in 0..200 {
            let s = idle(index, step as f64 * 0.05);
            assert!((0.974..=1.026).contains(&s.breath), "{s:?}");
            assert!(s.sway.abs() <= 0.051 && (0.0..=22.1).contains(&s.bob), "{s:?}");
        }
    }
    assert_ne!(idle(0, 1.0), idle(1, 1.0), "actors do not breathe in step");
    let period = 3.4 * 2.0;
    let (a, b) = (idle(2, 0.7), idle(2, 0.7 + period));
    assert!(near(a.breath, b.breath, 1e-4) && near(a.sway, b.sway, 1e-4));
}

// ----- protocol v7: limit breaks and escape ----------------------------------------------------------

/// What `look_in` says of a limit break that strikes (it does not run up) and of one that heals.
const STRIKE_LIMIT: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look { melee: false, element: Element::Plain, style: Style::Limit { physical: true } };
const HEAL_LIMIT: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look { melee: false, element: Element::Cure, style: Style::Limit { physical: false } };

#[test]
fn a_limit_break_is_a_long_spell_like_show_the_performer_gathers_and_a_strong_burst_breaks() {
    let mut d = with_clips();
    let batch = [action(0, ActionRef::Limit(128)), BattleEvent::LimitGauge { actor: ActorId(0), value: 0 }, damage(0, 2, false)];
    d.on_batch(10.0, &batch, STRIKE_LIMIT);
    // It lands at the cap, like the longest spell, and the gauge event lands nothing of its own: a strike as hard as a
    // critical hit, bigger than any blow, and a flash over the first target at the same instant.
    assert_eq!(impacts(&d).len(), 1);
    assert!(impacts(&d).iter().all(|&at| (at - (10.0 + MAX_IMPACT_DELAY)).abs() < 1e-9), "{:?}", impacts(&d));
    let strikes = of_kind(&d, |k| matches!(k, Kind::Strike { critical: true, .. }));
    assert!(strikes.len() == 1 && strikes[0].target == 2 && strikes[0].scale > 1.0, "{strikes:?}");
    let flashes = of_kind(&d, |k| k == Kind::Flash);
    assert!(flashes.len() == 1 && flashes[0].target == 2 && (flashes[0].land - 10.6).abs() < 1e-9, "{flashes:?}");
    // The performer glows from the arrival to the impact, more brightly than a caster, and plays the cast clips (it does not run up).
    let charges = of_kind(&d, |k| k == Kind::Charge);
    assert_eq!(charges.len(), 1);
    assert_eq!((charges[0].target, charges[0].start), (0, 10.0));
    assert!((charges[0].land - 10.6).abs() < 1e-9 && charges[0].scale > 1.0);
    assert!(d.segments_of(0).iter().any(|s| s.slot == party::CAST[0]), "{:?}", d.segments_of(0));
    assert_eq!(d.pending(), 0, "no lunge: nothing runs at the target");
    // The same show for what the performer does without clips: the burst and the glow, no panic.
    let mut plain = procedural();
    plain.on_batch(10.0, &batch, STRIKE_LIMIT);
    assert_eq!(plain.fx().len(), 3, "the glow, the strike and the flash");
    // The numbers and the blow agree whatever the show: the same instant the overlay's number is born.
    use crate::battle::popups::Popups;
    let mut popups = Popups::default();
    popups.on_events(10.0, &batch);
    let born: Vec<f64> = popups.births().iter().map(|(_, at)| *at).collect();
    assert_eq!(born.len(), 1);
    assert!((born[0] - (10.0 + MAX_IMPACT_DELAY)).abs() < 1e-9);
}

#[test]
fn a_healing_limit_glows_green_over_the_ally_it_heals() {
    let mut d = with_clips();
    let batch = [action(1, ActionRef::Limit(142)), BattleEvent::LimitGauge { actor: ActorId(1), value: 0 }, BattleEvent::Heal { actor: ActorId(1), target: ActorId(0), amount: 100 }];
    d.on_batch(5.0, &batch, HEAL_LIMIT);
    let mends = of_kind(&d, |k| k == Kind::Mend);
    assert_eq!(mends.len(), 1);
    assert_eq!((mends[0].target, mends[0].element), (0, Element::Cure));
    assert!(mends[0].scale > 1.0, "a limit heals bigger than a spell");
    assert!((mends[0].land - (5.0 + MAX_IMPACT_DELAY)).abs() < 1e-9);
    let charges = of_kind(&d, |k| k == Kind::Charge);
    assert_eq!((charges.len(), charges[0].element), (1, Element::Cure));
}

#[test]
fn a_failed_escape_throws_the_performer_back_and_it_comes_back_and_nobody_runs() {
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(0) }], PLAIN);
    assert_eq!(d.pending(), 1, "one stumble");
    assert!(d.fx().is_empty(), "nothing lands and nothing is gathered");
    // Away from the enemies (they stand at z = -1500, the party at +2000): +z, to its farthest in a third of
    // the time, and home again when it is over.
    let at = |t: f64| d.motion(0, t).offset;
    assert!(at(10.0)[2].abs() < 1e-3);
    assert!(at(10.0 + 0.17)[2] > 200.0 && at(10.0 + 0.17)[2] <= 281.0, "{:?}", at(10.17));
    assert!(at(10.0 + 0.5)[2] >= 0.0 && at(10.0 + 0.5)[2] < at(10.17)[2]);
    assert!(at(10.0 + 0.6).iter().all(|c| c.abs() < 1e-3), "back where it stood");
    // The other member stays, and nobody turns or leaves.
    for t in [10.1, 11.0, 30.0] {
        for i in 0..4 {
            let m = d.motion(i, t);
            assert!(!m.gone && m.turn == 0.0, "{i} at {t}: {m:?}");
        }
    }
    // The stumble is bounded by the schedule like every effect: a flood does not grow it.
    for k in 0..400 {
        d.on_batch(20.0 + f64::from(k), &[action(0, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(0) }], PLAIN);
    }
    assert!(d.pending() <= 128);
}

#[test]
fn a_successful_escape_makes_the_living_party_turn_run_off_and_leave_with_no_victory_pose() {
    let mut d = with_clips();
    // Member 1 is down: it lies where it fell and does not run.
    d.on_batch(5.0, &[action(2, ActionRef::Attack), damage(2, 1, false), BattleEvent::Death { actor: ActorId(1) }], PLAIN);
    // The announcement alone: no failure after it, so the party is away.
    d.on_batch(10.0, &[action(0, ActionRef::Escape)], PLAIN);
    // Before it starts nothing moved; then the runner's back is turned, the run clip loops, and it covers
    // the ground away from the fight (+z) faster and faster.
    assert_eq!(d.motion(0, 9.9), Motion::default());
    let (early, later) = (d.motion(0, 10.2), d.motion(0, 11.0));
    assert!(early.turn == std::f32::consts::PI && early.offset[2] > 0.0 && later.offset[2] > early.offset[2], "{early:?} {later:?}");
    assert_eq!(later.clip.map(|c| c.slot), Some(party::RUN), "the run clip, not the victory pose");
    let (f1, f2) = (d.motion(0, 10.2).clip.expect("a clip").frame, d.motion(0, 10.2 + 0.1).clip.expect("a clip").frame);
    assert!(f1 != f2, "the run loops: the frame advances");
    // Off the screen after RUN_OFF seconds (and gone: not drawn, shadow included).
    assert!(!d.motion(0, 10.0 + 1.5).gone && d.motion(0, 10.0 + 1.7).gone && d.motion(0, 99.0).gone);
    // The fallen member does not move, and the enemies stay where they are.
    for t in [10.5, 12.0, 99.0] {
        let down = d.motion(1, t);
        assert!(!down.gone && down.turn == 0.0 && down.offset[2].abs() < 1e-3, "{down:?}");
        for foe in [2, 3] {
            let m = d.motion(foe, t);
            assert!(!m.gone && m.turn == 0.0 && m.offset.iter().all(|c| c.abs() < 1e-3), "{foe} at {t}: {m:?}");
        }
    }
    // The announcement is all that was needed: a second one changes nothing (the first start counts).
    let before = d.motion(0, 11.0);
    d.on_batch(10.5, &[action(0, ActionRef::Escape)], PLAIN);
    assert_eq!(d.motion(0, 11.0), before);
    // Without clips the party still turns and leaves (the procedural model slides off).
    let mut plain = procedural();
    plain.on_batch(1.0, &[action(0, ActionRef::Escape)], PLAIN);
    assert!(plain.motion(1, 1.8).turn == std::f32::consts::PI && plain.motion(1, 1.8).clip.is_none() && plain.motion(1, 5.0).gone);
}

#[test]
fn nothing_a_server_sends_about_limits_or_escapes_breaks_the_director() {
    let mut d = with_clips();
    // Unknown performers, a gauge for nobody, an escape by an actor that is not there, absurd times.
    d.on_batch(0.0, &[action(99, ActionRef::Limit(128)), BattleEvent::LimitGauge { actor: ActorId(99), value: u16::MAX }, damage(99, 98, true)], STRIKE_LIMIT);
    d.on_batch(0.0, &[action(77, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(77) }], PLAIN);
    d.on_batch(f64::NAN, &[action(0, ActionRef::Escape)], PLAIN);
    d.on_batch(f64::INFINITY, &[action(0, ActionRef::Limit(1))], STRIKE_LIMIT);
    for t in [-5.0, 0.0, 1.0, 1e9] {
        for i in 0..5 {
            let m = d.motion(i, t);
            assert!(m.offset.iter().all(|c| c.is_finite() || t.is_nan()) && m.turn.is_finite(), "{i} at {t}: {m:?}");
        }
    }
    // An escape with nobody in the scene is a no-op, with no enemies the party runs along the field's depth.
    let mut empty = Director::new(vec![], vec![]);
    empty.on_escape(1.0);
    let mut party_only = Director::new(vec![Side::Party], vec![Anchor { ground: [0.0, 2000.0], radius: 100.0 }]);
    party_only.on_escape(1.0);
    assert!(party_only.motion(0, 1.5).offset[2] > 0.0);
}

// ----- the picture's effects: what every kind of performance shows -----------------------------------------------------

/// What the effects of the director are, in order.
fn kinds(d: &Director) -> Vec<Kind> {
    d.fx().iter().map(|f| f.kind).collect()
}

fn heal(actor: u8, target: u8) -> BattleEvent {
    BattleEvent::Heal { actor: ActorId(actor), target: ActorId(target), amount: 40 }
}

fn inflicted(target: u8, status: fmv_ff7_battle::status::Status) -> BattleEvent {
    BattleEvent::StatusInflicted { target: ActorId(target), status }
}

#[test]
fn a_spell_on_a_whole_side_shows_on_every_target_and_the_caster_gathers_it_once() {
    let mut d = with_clips();
    let fire = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Fire, style: SPELL };
    d.on_batch(10.0, &[action(0, ActionRef::Magic(27)), BattleEvent::MpSpent { actor: ActorId(0), amount: 4 }, damage(0, 2, false), damage(0, 3, false)], &fire);
    let blasts = of_kind(&d, |k| k == Kind::Blast);
    assert_eq!(blasts.iter().map(|f| f.target).collect::<Vec<_>>(), [2, 3], "an effect on each of the two enemies");
    assert!(blasts.iter().all(|f| f.element == Element::Fire && (f.land - 10.6).abs() < 1e-9 && (f.start - 10.0).abs() < 1e-9), "{blasts:?}");
    assert_ne!(blasts[0].seed, blasts[1].seed, "each has particles of its own");
    assert_eq!(of_kind(&d, |k| k == Kind::Charge).len(), 1, "one glow at the caster, not one per target");
    // The same for a whole party healed: an effect over every ally.
    let mut d = with_clips();
    let cure = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure, style: SPELL };
    d.on_batch(10.0, &[action(0, ActionRef::Magic(0)), heal(0, 0), heal(0, 1)], &cure);
    assert_eq!(of_kind(&d, |k| k == Kind::Mend).iter().map(|f| f.target).collect::<Vec<_>>(), [0, 1]);
}

#[test]
fn a_shot_flies_a_bolt_to_each_of_its_targets_and_past_the_one_it_missed() {
    // The MP's machine gun at the whole party: one is hit, one dodges.
    let shot = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Plain, style: Style::Shot };
    let mut d = with_clips();
    d.on_batch(10.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), BattleEvent::Miss { actor: ActorId(2), target: ActorId(1) }], &shot);
    let landing = 10.0 + cue::impact_delay(Some(ActionRef::EnemyAttack(272)));
    let bolts = of_kind(&d, |k| matches!(k, Kind::Bolt { .. }));
    assert_eq!(bolts.iter().map(|f| (f.target, f.kind)).collect::<Vec<_>>(), [(0, Kind::Bolt { hit: true }), (1, Kind::Bolt { hit: false })]);
    assert!(bolts.iter().all(|f| f.from == Some(2) && (f.land - landing).abs() < 1e-9 && f.start < f.land && f.start >= 10.0), "{bolts:?}");
    // What lands is a shot's small impact and a whiff.
    assert_eq!(kinds(&d).iter().filter(|k| matches!(k, Kind::Impact { critical: false })).count(), 1);
    assert_eq!(kinds(&d).iter().filter(|k| **k == Kind::Whiff).count(), 1);
    // A blow with no shot in it flies nothing.
    let mut d = with_clips();
    d.on_batch(10.0, &blow(0, 2), PLAIN);
    assert!(!kinds(&d).iter().any(|k| matches!(k, Kind::Bolt { .. } | Kind::Toss)));
}

#[test]
fn a_status_spell_shows_its_sign_where_it_lands_and_one_that_did_not_take_fizzles_at_its_caster() {
    use fmv_ff7_battle::status::Status;
    let spell = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Plain, style: SPELL };
    // Sleep on the enemy, Haste on an ally: a curse and a buff, each over its target, from the start of the cast.
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Magic(11)), BattleEvent::MpSpent { actor: ActorId(0), amount: 8 }, inflicted(2, Status::Sleep)], &spell);
    d.on_batch(20.0, &[action(0, ActionRef::Magic(19)), inflicted(1, Status::Haste)], &spell);
    let signs: Vec<(Kind, usize, f64)> = d.fx().iter().filter(|f| matches!(f.kind, Kind::Afflict(_) | Kind::Boost(_))).map(|f| (f.kind, f.target, f.land)).collect();
    assert_eq!(signs, [(Kind::Afflict(Status::Sleep), 2, 10.6), (Kind::Boost(Status::Haste), 1, 20.6)]);
    assert!(d.fx().iter().filter(|f| matches!(f.kind, Kind::Afflict(_) | Kind::Boost(_))).all(|f| f.start < f.land), "it begins with the cast");
    // A spell whose roll failed says nothing at all but its announcement and its cost: its glow breaks into a fizzle at the caster.
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Magic(11)), BattleEvent::MpSpent { actor: ActorId(0), amount: 8 }], &spell);
    assert_eq!(kinds(&d), [Kind::Charge, Kind::Fizzle]);
    let fizzle = d.fx()[1];
    assert_eq!((fizzle.target, fizzle.from), (0, None));
    assert!((fizzle.land - 10.6).abs() < 1e-9);
    // An item that found nothing to do (an Antidote on a healthy ally) is the same, with no toss for it to fly.
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Item(9))], &|_, _| Look { melee: false, element: Element::Plain, style: Style::Item });
    assert_eq!(kinds(&d), [Kind::Fizzle]);
    // Running away is no performance with effects of its own (the party turns and runs, or stumbles).
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(0) }], PLAIN);
    assert!(d.fx().is_empty());
}

#[test]
fn a_status_with_no_announcement_lands_with_the_short_beat_and_has_no_lead() {
    use fmv_ff7_battle::status::Status;
    // A status that ran out on the clock, and one the battle began with, are events without an action: the beat of the numbers.
    let mut d = with_clips();
    d.on_batch(5.0, &[BattleEvent::StatusEnded { target: ActorId(1), status: Status::Barrier }, inflicted(0, Status::Poison)], PLAIN);
    let beat = 5.0 + cue::impact_delay(None);
    let all: Vec<(Kind, usize, f64, f64)> = d.fx().iter().map(|f| (f.kind, f.target, f.start, f.land)).collect();
    assert_eq!(all, [(Kind::Cleanse(Status::Barrier), 1, beat, beat), (Kind::Afflict(Status::Poison), 0, beat, beat)]);
    assert!(d.fx()[0].scale < 1.0, "it ends quietly");
}

#[test]
fn every_kind_of_performance_that_does_something_has_an_effect_and_none_of_them_begins_before_its_performance() {
    let spell = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Ice, style: SPELL };
    let item = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure, style: Style::Item };
    let miss = BattleEvent::Miss { actor: ActorId(0), target: ActorId(2) };
    type Show = fn(Kind) -> bool;
    type LookFn<'a> = &'a dyn Fn(ActorId, ActionRef) -> Look;
    let cases: Vec<(&str, Vec<BattleEvent>, LookFn, Vec<Show>)> = vec![
        ("a blow", blow(0, 2), PLAIN, vec![|k| matches!(k, Kind::Strike { critical: false, .. })]),
        ("a critical blow", vec![action(0, ActionRef::Attack), damage(0, 2, true)], PLAIN, vec![|k| matches!(k, Kind::Strike { critical: true, .. })]),
        ("a miss", vec![action(0, ActionRef::Attack), miss], PLAIN, vec![|k| k == Kind::Whiff]),
        ("a damage spell", vec![action(0, ActionRef::Magic(30)), damage(0, 2, false)], &spell, vec![|k| k == Kind::Charge, |k| k == Kind::Blast]),
        ("a heal", vec![action(0, ActionRef::Magic(0)), heal(0, 1)], &spell, vec![|k| k == Kind::Charge, |k| k == Kind::Mend]),
        ("a revival", vec![action(0, ActionRef::Magic(7)), BattleEvent::Revive { actor: ActorId(0), target: ActorId(1), hp: 30 }], &spell, vec![|k| k == Kind::Raise]),
        ("an Ether", vec![action(0, ActionRef::Item(3)), BattleEvent::MpRestored { actor: ActorId(0), target: ActorId(1), amount: 9 }], &item, vec![|k| k == Kind::Toss, |k| k == Kind::Mana]),
        ("a Potion", vec![action(0, ActionRef::Item(0)), heal(0, 1)], &item, vec![|k| k == Kind::Toss, |k| k == Kind::Mend]),
        ("a guard", vec![action(1, ActionRef::Defend)], PLAIN, vec![|k| k == Kind::Guard]),
        ("an enemy's spell", vec![action(2, ActionRef::EnemyAttack(274)), damage(2, 1, false)], &spell, vec![|k| k == Kind::Blast]),
    ];
    for (what, events, look, wanted) in cases {
        let mut d = with_clips();
        d.on_batch(10.0, &events, look);
        for want in &wanted {
            assert!(d.fx().iter().any(|f| want(f.kind)), "{what} shows what it should: {:?}", kinds(&d));
        }
        assert!(d.fx().iter().all(|f| f.start >= 10.0 && f.start <= f.land && f.land <= 10.0 + MAX_IMPACT_DELAY + 1e-9), "{what}: {:?}", d.fx());
    }
    // A counter's effects begin when the counter does, never with the batch: the blow's own are already over by then.
    let batch = [action(0, ActionRef::Attack), damage(0, 2, false), action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), damage(2, 1, false)];
    let shot = |_: ActorId, a: ActionRef| if matches!(a, ActionRef::EnemyAttack(_)) { Look { melee: false, element: Element::Plain, style: Style::Shot } } else { Look::PLAIN };
    let mut d = with_clips();
    d.on_batch(10.0, &batch, &shot);
    let counter_begins = 10.0 + cue::length_of(Some(ActionRef::Attack));
    let (first, counter): (Vec<Fx>, Vec<Fx>) = d.fx().iter().copied().partition(|f| f.target == 2);
    assert_eq!(first.len(), 1, "the blow's strike at the enemy");
    assert!(first[0].land < counter_begins, "the blow lands before the counter begins");
    assert_eq!(counter.len(), 4, "a bolt and an impact for each of the two of the party: {counter:?}");
    assert!(counter.iter().all(|f| f.start >= counter_begins - 1e-9), "nothing of the counter before it begins: {counter:?}");
    assert!(counter.iter().all(|f| (f.land - (counter_begins + cue::impact_delay(Some(ActionRef::EnemyAttack(272))))).abs() < 1e-9));
}

#[test]
fn what_is_over_is_forgotten_and_the_list_of_effects_is_bounded() {
    let mut d = with_clips();
    let fire = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Fire, style: SPELL };
    d.on_batch(10.0, &[action(0, ActionRef::Magic(27)), damage(0, 2, false)], &fire);
    let last_end = d.fx().iter().map(Fx::end).fold(0.0, f64::max);
    assert!(last_end > 10.6 && last_end < 12.0, "an effect is over about a second after it lands: {last_end}");
    d.prune(10.3);
    assert_eq!(d.fx().len(), 2, "both are still on");
    d.prune(last_end + 0.001);
    assert!(d.fx().is_empty());
    // Whatever a flood of spells says, the director keeps only so many, the newest ones.
    for k in 0..400 {
        d.on_batch(100.0 + f64::from(k) * 1e-4, &[action(0, ActionRef::Magic(27)), damage(0, 2, false), damage(0, 3, false)], &fire);
    }
    assert!(d.fx().len() <= MAX_FX, "{} effects", d.fx().len());
    assert!(d.fx().windows(2).all(|w| w[0].seed < w[1].seed), "the oldest were dropped, in order");
}
