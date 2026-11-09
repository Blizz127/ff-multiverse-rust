//! Tests of the [`Director`]: what a performance does over time, with real clips and without, how a
//! blow lands on schedule whatever the clips, and that nothing a server sends can grow the schedule
//! without bound or break it.

use super::super::clips::testing::{cloud_like_pack, cloud_like_scripts};
use super::super::clips::{party, ClipSet};
use super::super::cue::{self, DEATH_BEAT, MAX_IMPACT_DELAY};
use super::*;

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
    ClipSet { frames: vec![frames; n], ok: vec![true; n], travel: vec![0.0; n], fps: 15.0 }
}

/// The same four with real clips: the party's shared slots and blows of Cloud's shape (52 slots, `cloud_like_pack`)
/// and enemies that attack with attack 272 (slot 3, 15 frames) and run up with slot 2 (1 000 units forward) and back
/// with slot 5.
fn with_clips() -> Director {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let party = Kit::party(cloud_like_pack(), &cloud_like_scripts());
    let mut set = pack(8, 15);
    set.travel[2] = -1000.0;
    set.travel[5] = 1000.0;
    let enemy = Kit::enemy(set, &[(272, 3)]);
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

/// The instants at which blows land on someone: the bursts of the physical hits.
fn impacts(d: &Director) -> Vec<f64> {
    d.bursts().iter().map(|b| b.at).collect()
}

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
    let cure = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure };
    // Every kind of action that has something land, with the effects it can have.
    type LookFn<'a> = &'a dyn Fn(ActorId, ActionRef) -> Look;
    let batches: [(Vec<BattleEvent>, LookFn); 6] = [
        (blow(0, 2), PLAIN),
        (vec![action(2, ActionRef::EnemyAttack(272)), damage(2, 0, false), damage(2, 1, true)], PLAIN),
        (vec![action(0, ActionRef::Magic(27)), BattleEvent::MpSpent { actor: ActorId(0), amount: 4 }, damage(0, 2, false), damage(0, 3, false)], PLAIN),
        (vec![action(1, ActionRef::Magic(0)), BattleEvent::Heal { actor: ActorId(1), target: ActorId(0), amount: 80 }], &cure),
        (vec![action(0, ActionRef::Item(3)), BattleEvent::MpRestored { actor: ActorId(0), target: ActorId(1), amount: 30 }], PLAIN),
        // No announcement at all: the short beat of an older server.
        (vec![damage(0, 2, false)], PLAIN),
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
            assert!(born.iter().all(|&at| at - now <= MAX_IMPACT_DELAY + 1e-6));
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

#[test]
fn no_blow_lands_later_than_the_cap_whatever_its_clips_its_reach_or_what_its_actor_was_doing() {
    // Very long clips (200 frames: 13 seconds), every kind of action, at several distances, with the
    // actor busy with an earlier performance that is still running.
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let party = Kit::party(pack(52, 200), &cloud_like_scripts());
    let enemy = Kit::enemy(pack(8, 200), &[(272, 3)]);
    for far in [0.0f32, 500.0, 3000.0, 20_000.0] {
        let anchors = vec![Anchor { ground: [0.0, 0.0], radius: 150.0 }, Anchor { ground: [10.0, 0.0], radius: 150.0 }, Anchor { ground: [0.0, -far], radius: 150.0 }, Anchor { ground: [0.0, -far], radius: 150.0 }];
        let mut d = Director::with_kits(sides.clone(), anchors, vec![party.clone(), party.clone(), enemy.clone(), enemy.clone()]);
        let mut now = 100.0;
        for (i, what) in [ActionRef::Attack, ActionRef::Magic(27), ActionRef::Item(3), ActionRef::Defend, ActionRef::Attack].into_iter().enumerate() {
            let before = d.bursts().len();
            // The same actor again before the last one is over: its performance takes over.
            d.on_batch(now, &[action(0, what), damage(0, 2, i % 2 == 0)], PLAIN);
            d.on_batch(now + 0.01, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false)], PLAIN);
            for burst in &d.bursts()[before..] {
                assert!(burst.at <= now + 0.01 + MAX_IMPACT_DELAY + 1e-9, "{what:?} at {far}: the blow lands at {} after arrival {now}", burst.at);
                assert!(burst.at >= now, "never before the batch arrived");
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
    assert!(d.pending() <= MAX_EFFECTS && d.bursts().len() <= MAX_BURSTS, "{} effects, {} bursts", d.pending(), d.bursts().len());
    let last = 10.0 + 0.499;
    assert!(impacts(&d).iter().all(|&at| at <= last + MAX_IMPACT_DELAY + 1e-9), "{:?}", impacts(&d));
    // A death still counts, whatever was dropped.
    assert!(d.deaths[3].is_some());
    // Everything is over a few seconds later.
    d.prune(20.0);
    assert!(d.pending() == 0 && d.bursts().is_empty() && d.segments.iter().all(Vec::is_empty));
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
    // The burst (sparks) is at the target from the impact.
    assert_eq!(d.bursts().len(), 1);
    assert_eq!((d.bursts()[0].target, d.bursts()[0].kind), (2, BurstKind::Hit { critical: false }));
    // It runs up to its target and is back home when it is over.
    assert!(d.motion(0, impact).offset[2] < -1000.0, "it ran at its target (to -z): {:?}", d.motion(0, impact));
    d.prune(20.0);
    assert_eq!(d.motion(0, 20.0), Motion::default());
}

#[test]
fn a_long_range_blow_hops_forward_and_no_further_and_an_item_at_an_enemy_is_the_throw_forward() {
    let mut d = with_clips();
    let shoot = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Plain };
    d.on_batch(10.0, &blow(0, 2), &shoot);
    let impact = 10.5;
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
fn an_enemys_attack_plays_the_clip_scene_bin_names_and_runs_up_with_its_run_clip() {
    let mut d = with_clips();
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false)], PLAIN);
    // The run-up is the clip with the big forward travel (slot 2), the attack slot 3, the way back slot 5.
    assert_eq!(d.motion(2, 5.01).clip.map(|c| c.slot), Some(2));
    assert_eq!(d.motion(2, 5.55 - 0.001).clip.map(|c| c.slot), Some(3));
    let slots: Vec<Slot> = d.segments[2].iter().map(|s| s.slot).collect();
    assert_eq!(slots, [2, 3, 3, 5]);
    // A hit on a party member plays its hurt clip (slot 15).
    assert_eq!(d.motion(1, 5.6).clip.map(|c| c.slot), Some(15));
    // An attack the table does not know plays the enemy's first attack's clip.
    let mut d = with_clips();
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(9999)), damage(2, 1, false)], PLAIN);
    assert!(d.segments[2].iter().any(|s| s.slot == 3));
}

#[test]
fn an_attack_whose_clip_cannot_be_played_moves_procedurally_and_does_not_borrow_anothers() {
    let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
    let mut set = pack(8, 15);
    set.travel[2] = -1000.0;
    set.travel[5] = 1000.0;
    // Attack 272 plays slot 3 (real); attack 273 names slot 4, which is not the slot's own clip; 274 names slot 40,
    // past the pack.
    set.ok[4] = false;
    let enemy = Kit::enemy(set, &[(272, 3), (273, 4), (274, 40)]);
    let mut d = Director::with_kits(sides, anchors(), vec![Kit::none(), Kit::none(), enemy.clone(), enemy]);
    d.on_batch(5.0, &[action(2, ActionRef::EnemyAttack(272)), damage(2, 1, false)], PLAIN);
    assert!(d.segments_of(2).iter().any(|s| s.slot == 3), "the playable attack plays its clip");
    for bad in [273u16, 274] {
        let mut d = d.clone();
        d.on_batch(9.0, &[action(3, ActionRef::EnemyAttack(bad)), damage(3, 1, false)], PLAIN);
        assert!(d.segments_of(3).iter().all(|s| s.slot != 3), "attack {bad} does not play attack 272's clip: {:?}", d.segments_of(3));
        let run = d.effects.iter().filter(|e| matches!(e, Effect::Lunge { actor: 3, .. })).count();
        assert_eq!(run, 1, "it runs up to its target like an actor without clips");
        // It lands on schedule all the same.
        assert!(d.bursts().iter().any(|b| (b.at - (9.0 + cue::impact_delay(Some(ActionRef::EnemyAttack(bad))))).abs() < 1e-9));
    }
    // An attack the scene does not name for it at all (or a plain attack) plays the first attack's clip.
    let mut named_nowhere = d.clone();
    named_nowhere.on_batch(20.0, &[action(3, ActionRef::EnemyAttack(999)), damage(3, 1, false)], PLAIN);
    assert!(named_nowhere.segments_of(3).iter().any(|s| s.slot == 3));
}

#[test]
fn a_miss_is_a_step_aside_with_no_burst_and_a_spell_that_misses_shows_nothing_either() {
    let mut d = with_clips();
    d.on_batch(3.0, &[action(0, ActionRef::Attack), BattleEvent::Miss { actor: ActorId(0), target: ActorId(3) }], PLAIN);
    assert!(d.bursts().is_empty());
    let mid = d.motion(3, 3.5 + f64::from(DODGE) * 0.5);
    assert!(near(mid.offset[0].hypot(mid.offset[2]), DODGE_DISTANCE, 1.0), "{mid:?}");
    assert_eq!(mid.clip, None, "a dodge plays no hurt clip");
}

// ----- spells, items, guards ------------------------------------------------------------------

#[test]
fn a_cast_gathers_a_glow_begins_to_the_impact_and_breaks_in_the_elements_colour() {
    let mut d = with_clips();
    let fire = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Fire };
    d.on_batch(10.0, &[action(0, ActionRef::Magic(27)), BattleEvent::MpSpent { actor: ActorId(0), amount: 4 }, damage(0, 2, false)], &fire);
    assert_eq!(d.charges().len(), 1);
    assert_eq!((d.charges()[0].caster, d.charges()[0].element), (0, Element::Fire));
    assert!((d.charges()[0].start - 10.0).abs() < 1e-9 && (d.charges()[0].end - 10.6).abs() < 1e-9);
    assert_eq!(d.motion(0, 10.1).clip.map(|c| c.slot), Some(12));
    assert_eq!(d.motion(0, 10.7).clip.map(|c| c.slot), Some(13), "holds the pose as the spell breaks");
    // The burst is a spell's, in the colour of fire, on the target, at the impact; the target flinches.
    assert_eq!((d.bursts()[0].kind, d.bursts()[0].element, d.bursts()[0].target), (BurstKind::Spell, Element::Fire, 2));
    assert!((d.bursts()[0].at - 10.6).abs() < 1e-9);
    assert_eq!(d.motion(2, 10.65).clip.map(|c| c.slot), Some(1));
}

#[test]
fn a_heal_bursts_green_and_an_items_effect_is_an_items_and_a_guard_holds_its_pose() {
    let mut d = with_clips();
    let cure = |_: ActorId, _: ActionRef| Look { melee: false, element: Element::Cure };
    d.on_batch(1.0, &[action(0, ActionRef::Magic(0)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(1), amount: 80 }], &cure);
    assert_eq!((d.bursts()[0].kind, d.bursts()[0].element, d.bursts()[0].target), (BurstKind::Spell, Element::Cure, 1));
    assert_eq!(d.motion(1, 1.7).clip, None, "a heal is no flinch");
    let mut d = with_clips();
    d.on_batch(1.0, &[action(0, ActionRef::Item(3)), BattleEvent::Heal { actor: ActorId(0), target: ActorId(0), amount: 50 }], PLAIN);
    assert_eq!(d.bursts()[0].kind, BurstKind::Item);
    assert_eq!(d.motion(0, 1.2).clip.map(|c| c.slot), Some(9), "the item gesture");
    let mut d = with_clips();
    d.on_batch(1.0, &[action(1, ActionRef::Defend)], PLAIN);
    assert_eq!(d.motion(1, 1.3).clip.map(|c| c.slot), Some(8));
    assert!(d.bursts().is_empty() && d.charges().is_empty());
    assert_eq!(d.motion(1, 1.0 + plan::GUARD_HOLD + 0.2).clip, None, "and lets go of it");
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
    assert_eq!(critical.bursts()[0].kind, BurstKind::Hit { critical: true });
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
const STRIKE_LIMIT: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look { melee: false, element: Element::Plain };
const HEAL_LIMIT: &dyn Fn(ActorId, ActionRef) -> Look = &|_, _| Look { melee: false, element: Element::Cure };

#[test]
fn a_limit_break_is_a_long_spell_like_show_the_performer_gathers_and_a_strong_burst_breaks() {
    let mut d = with_clips();
    let batch = [action(0, ActionRef::Limit(128)), BattleEvent::LimitGauge { actor: ActorId(0), value: 0 }, damage(0, 2, false)];
    d.on_batch(10.0, &batch, STRIKE_LIMIT);
    // It lands at the cap, like the longest spell, and the gauge event lands nothing of its own.
    assert_eq!(impacts(&d).len(), 2, "a bloom and a star at the same instant");
    assert!(impacts(&d).iter().all(|&at| (at - (10.0 + MAX_IMPACT_DELAY)).abs() < 1e-9), "{:?}", impacts(&d));
    let kinds: Vec<BurstKind> = d.bursts().iter().map(|b| b.kind).collect();
    assert!(kinds.contains(&BurstKind::Spell) && kinds.contains(&BurstKind::Hit { critical: true }), "{kinds:?}");
    assert!(d.bursts().iter().all(|b| b.target == 2));
    // The performer glows from the arrival to the impact and plays the cast clips (it does not run up).
    assert_eq!(d.charges().len(), 1);
    assert_eq!((d.charges()[0].caster, d.charges()[0].start), (0, 10.0));
    assert!((d.charges()[0].end - 10.6).abs() < 1e-9);
    assert!(d.segments_of(0).iter().any(|s| s.slot == party::CAST[0]), "{:?}", d.segments_of(0));
    assert_eq!(d.pending(), 0, "no lunge: nothing runs at the target");
    // The same show for what the performer does without clips: the burst and the glow, no panic.
    let mut plain = procedural();
    plain.on_batch(10.0, &batch, STRIKE_LIMIT);
    assert_eq!((plain.bursts().len(), plain.charges().len()), (2, 1));
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
    assert_eq!(d.bursts().len(), 1);
    let burst = d.bursts()[0];
    assert_eq!((burst.target, burst.kind, burst.element), (0, BurstKind::Spell, Element::Cure));
    assert!((burst.at - (5.0 + MAX_IMPACT_DELAY)).abs() < 1e-9);
    assert_eq!((d.charges().len(), d.charges()[0].element), (1, Element::Cure));
}

#[test]
fn a_failed_escape_throws_the_performer_back_and_it_comes_back_and_nobody_runs() {
    let mut d = with_clips();
    d.on_batch(10.0, &[action(0, ActionRef::Escape), BattleEvent::EscapeFailed { actor: ActorId(0) }], PLAIN);
    assert_eq!(d.pending(), 1, "one stumble");
    assert!(d.bursts().is_empty() && d.charges().is_empty(), "nothing lands and nothing is gathered");
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
