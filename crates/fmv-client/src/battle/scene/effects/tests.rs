//! Tests of the effects: which effect each event makes (the table of [`show`]), the timeline of an effect (nothing before it
//! starts, a lead, the peak at the landing, a fade, nothing after), that every piece is sound and the count of them bounded, that
//! the same effect at the same instant is the same pieces, that the elements and the statuses do not look alike, and the meshes
//! and the matrices that place them. No window and no GPU.

use super::fx::Show;
use super::*;
use fmv_ff7_battle::rules::{ActorId, BattleEvent};
use std::collections::BTreeSet;
use std::f32::consts::PI;

const A: ActorId = ActorId(0);
const T: ActorId = ActorId(2);

fn damage(critical: bool) -> BattleEvent {
    BattleEvent::Damage { actor: A, target: T, amount: 20, critical }
}

fn look(style: Style, element: Element) -> Look {
    Look { melee: false, element, style }
}

fn shown(event: &BattleEvent, look: &Look) -> Show {
    show(event, look, false).expect("an effect")
}

// ----- which effect an event makes ---------------------------------------------------------------------------------------

#[test]
fn a_blow_strikes_as_its_record_says_a_critical_hit_is_marked_and_a_shot_has_its_own_impact() {
    for how in [Strike::Slash, Strike::Thump, Strike::Punch] {
        for critical in [false, true] {
            let s = shown(&damage(critical), &look(Style::Blow(how), Element::Plain));
            assert_eq!((s.kind, s.scale), (Kind::Strike { how, critical }, 1.0), "{how:?} {critical}");
        }
    }
    assert_eq!(shown(&damage(true), &look(Style::Shot, Element::Plain)).kind, Kind::Impact { critical: true });
    assert_eq!(shown(&damage(false), &look(Style::Shot, Element::Plain)).kind, Kind::Impact { critical: false });
    // The element of a blow is the colour of its sparks.
    assert_eq!(shown(&damage(false), &look(Style::Blow(Strike::Slash), Element::Fire)).element, Element::Fire);
    // The strike of an attack record's element bits: punch, then hit or shout, else cut and what says nothing (the Attack command).
    assert_eq!(Strike::of_mask(element::PUNCH | element::HIT), Strike::Punch);
    assert_eq!(Strike::of_mask(element::HIT), Strike::Thump);
    assert_eq!(Strike::of_mask(element::SHOUT), Strike::Thump);
    assert_eq!(Strike::of_mask(element::CUT), Strike::Slash);
    assert_eq!(Strike::of_mask(element::CUT | element::HIT), Strike::Slash, "a blade that also hits still cuts");
    assert_eq!(Strike::of_mask(0), Strike::Slash);
    assert_eq!(Strike::of_mask(element::SHOOT), Strike::Slash, "a shot is staged as one before this is asked");
}

#[test]
fn a_miss_is_a_whiff_and_a_spell_item_or_limit_breaks_over_its_target_by_its_element() {
    assert_eq!(shown(&BattleEvent::Miss { actor: A, target: T }, &look(Style::Blow(Strike::Slash), Element::Plain)).kind, Kind::Whiff);
    assert_eq!(shown(&BattleEvent::Miss { actor: A, target: T }, &look(Style::Shot, Element::Plain)).kind, Kind::Whiff);
    for element in [Element::Fire, Element::Ice, Element::Bolt, Element::Earth, Element::Wind, Element::Water, Element::Poison, Element::Gravity, Element::Holy, Element::Plain] {
        let s = shown(&damage(false), &look(Style::Spell, element));
        assert_eq!((s.kind, s.element, s.scale), (Kind::Blast, element, 1.0), "{element:?}");
    }
    // An item is a lesser blast, a magical limit a greater one, a physical limit a critical blow as big as a limit is.
    let item = shown(&damage(false), &look(Style::Item, Element::Plain));
    let limit = shown(&damage(false), &look(Style::Limit { physical: false }, Element::Fire));
    let braver = shown(&damage(false), &look(Style::Limit { physical: true }, Element::Plain));
    assert_eq!(item.kind, Kind::Blast);
    assert_eq!(limit.kind, Kind::Blast);
    assert!(item.scale < 1.0 && limit.scale > 1.0);
    assert_eq!(braver.kind, Kind::Strike { how: Strike::Slash, critical: true });
    assert!(braver.scale > 1.0, "a limit break is a bigger strike than any blow");
}

#[test]
fn restoring_makes_green_blue_and_golden_effects_and_an_item_restores_less_than_a_spell_and_a_limit_more() {
    let heal = BattleEvent::Heal { actor: A, target: T, amount: 30 };
    let revive = BattleEvent::Revive { actor: A, target: T, hp: 50 };
    let ether = BattleEvent::MpRestored { actor: A, target: T, amount: 10 };
    let spell = look(Style::Spell, Element::Cure);
    assert_eq!(shown(&heal, &spell), Show { kind: Kind::Mend, element: Element::Cure, scale: 1.0 });
    assert_eq!(shown(&revive, &spell).kind, Kind::Raise);
    assert_eq!(shown(&ether, &spell).kind, Kind::Mana);
    let item = shown(&heal, &look(Style::Item, Element::Cure)).scale;
    let limit = shown(&heal, &look(Style::Limit { physical: false }, Element::Cure)).scale;
    assert!(item < 1.0 && limit > 1.0, "{item} {limit}");
    // Whatever the performance's own element says, a heal is drawn as one.
    assert_eq!(shown(&heal, &look(Style::Spell, Element::Fire)).element, Element::Cure);
}

#[test]
fn a_status_is_a_sign_of_its_own_a_buff_for_what_helps_a_curse_for_what_hurts_and_a_wipe_when_it_ends() {
    for status in Status::ALL.into_iter().filter(|s| s.is_modelled()) {
        let inflicted = BattleEvent::StatusInflicted { target: T, status };
        let kind = shown(&inflicted, &look(Style::Spell, Element::Plain)).kind;
        let expect = if status.is_beneficial() { Kind::Boost(status) } else { Kind::Afflict(status) };
        assert_eq!(kind, expect, "{status}");
        assert_eq!(shown(&BattleEvent::StatusEnded { target: T, status }, &look(Style::Spell, Element::Plain)).kind, Kind::Cleanse(status));
    }
    for buff in [Status::Haste, Status::Regen, Status::Barrier, Status::MBarrier] {
        assert!(matches!(shown(&BattleEvent::StatusInflicted { target: T, status: buff }, &Look::PLAIN).kind, Kind::Boost(_)), "{buff}");
    }
    for curse in [Status::Sleep, Status::Poison, Status::Silence, Status::Stop, Status::Paralysis, Status::Slow, Status::Darkness, Status::Sadness, Status::Fury] {
        assert!(matches!(shown(&BattleEvent::StatusInflicted { target: T, status: curse }, &Look::PLAIN).kind, Kind::Afflict(_)), "{curse}");
    }
    // A status that ran out on the clock (an event with no action) ends more quietly than one that was cured.
    let ran_out = show(&BattleEvent::StatusEnded { target: T, status: Status::Barrier }, &Look::PLAIN, true).unwrap();
    let cured = show(&BattleEvent::StatusEnded { target: T, status: Status::Poison }, &look(Style::Spell, Element::Plain), false).unwrap();
    assert!(ran_out.scale < cured.scale);
}

#[test]
fn what_lands_on_nobody_makes_no_effect() {
    for event in [
        BattleEvent::Death { actor: T },
        BattleEvent::GaugeReady { actor: A },
        BattleEvent::Victory,
        BattleEvent::Defeat,
        BattleEvent::Action { actor: A, action: fmv_ff7_battle::rules::ActionRef::Attack },
        BattleEvent::LimitGauge { actor: A, value: 10 },
        BattleEvent::EscapeFailed { actor: A },
        BattleEvent::MpSpent { actor: A, amount: 3 },
        BattleEvent::StatusTick { target: T, status: Status::Poison, amount: 4 },
    ] {
        assert_eq!(show(&event, &Look::PLAIN, false), None, "{event:?}");
    }
}

// ----- the timeline -------------------------------------------------------------------------------------------------------

fn fx(kind: Kind, element: Element, start: f64, land: f64) -> Fx {
    Fx { kind, element, target: 2, from: Some(0), start, land, scale: 1.0, seed: 7 }
}

fn geo() -> Geo {
    Geo { height: 850.0, from: Some([1500.0, -120.0, 0.0]) }
}

fn pieces(fx: &Fx, now: f64) -> Vec<Layer> {
    let mut out = Vec::new();
    layers(fx, now, &geo(), &mut out);
    out
}

fn peak(fx: &Fx, now: f64) -> f32 {
    pieces(fx, now).iter().map(|l| l.alpha).fold(0.0, f32::max)
}

/// Every kind there is, each status's signs included.
fn all_kinds() -> Vec<Kind> {
    let mut kinds = vec![Kind::Whiff, Kind::Blast, Kind::Mend, Kind::Raise, Kind::Mana, Kind::Charge, Kind::Bolt { hit: true }, Kind::Bolt { hit: false }, Kind::Toss, Kind::Guard, Kind::Fizzle, Kind::Flash];
    for critical in [false, true] {
        kinds.push(Kind::Impact { critical });
        for how in [Strike::Slash, Strike::Thump, Strike::Punch] {
            kinds.push(Kind::Strike { how, critical });
        }
    }
    for status in Status::ALL {
        kinds.extend([Kind::Afflict(status), Kind::Boost(status), Kind::Cleanse(status)]);
    }
    kinds
}

#[test]
fn an_effect_has_a_lead_then_an_age_and_is_nothing_before_it_starts_or_after_it_ends() {
    let f = fx(Kind::Blast, Element::Fire, 10.0, 10.6);
    assert_eq!(f.phase(9.99), None, "nothing before it starts");
    assert_eq!(f.phase(10.0), Some(Phase::Lead(0.0)));
    assert!(matches!(f.phase(10.3), Some(Phase::Lead(p)) if (p - 0.5).abs() < 1e-5));
    assert_eq!(f.phase(10.6), Some(Phase::Age(0.0)), "at the landing the age begins");
    assert!(matches!(f.phase(10.6 + f.life() * 0.5), Some(Phase::Age(a)) if (a - 0.5).abs() < 1e-5));
    assert_eq!(f.phase(f.end()), None, "and it is over at the end of its life");
    assert_eq!(f.phase(f64::NAN), None);
    assert_eq!(f.phase(f64::INFINITY), None);
    // An effect with no lead (start is the landing) is on from the landing.
    let blow = fx(Kind::Strike { how: Strike::Slash, critical: false }, Element::Plain, 5.0, 5.0);
    assert_eq!(blow.phase(4.999), None);
    assert_eq!(blow.phase(5.0), Some(Phase::Age(0.0)));
    // Every kind lives a while after it lands and none for ever.
    for kind in all_kinds() {
        let life = fx(kind, Element::Plain, 0.0, 1.0).life();
        assert!((0.05..=1.5).contains(&life), "{kind:?}: {life}");
    }
}

#[test]
fn nothing_is_drawn_before_an_effects_start_so_a_counters_effects_wait_for_the_counter() {
    for kind in all_kinds() {
        for element in [Element::Plain, Element::Fire, Element::Holy] {
            let f = fx(kind, element, 12.0, 12.6);
            for now in [0.0, 11.0, 11.999] {
                assert!(pieces(&f, now).is_empty(), "{kind:?} {element:?} at {now}");
            }
            assert!(pieces(&f, f.end()).is_empty() && pieces(&f, f.end() + 5.0).is_empty(), "{kind:?}: over after its life");
        }
    }
}

#[test]
fn every_piece_is_sound_the_count_is_bounded_and_the_same_effect_at_the_same_instant_is_the_same_pieces() {
    for kind in all_kinds() {
        for element in [Element::Plain, Element::Fire, Element::Ice, Element::Bolt, Element::Cure, Element::Earth, Element::Wind, Element::Water, Element::Holy, Element::Poison, Element::Gravity] {
            for scale in [0.3, 1.0, 2.0] {
                let f = Fx { scale, seed: 1234, ..fx(kind, element, 0.0, 0.6) };
                let mut now = 0.0;
                while now < f.end() {
                    let out = pieces(&f, now);
                    assert!(out.len() <= MAX_LAYERS_PER_FX, "{kind:?} {element:?} x{scale} at {now}: {} pieces", out.len());
                    for l in &out {
                        assert!(l.is_sound(), "{kind:?} {element:?} x{scale} at {now}: {l:?}");
                    }
                    assert_eq!(out, pieces(&f, now), "{kind:?} at {now}: not the same twice");
                    now += 0.013;
                }
            }
        }
    }
    // Without the actor it flies from, a bolt and a toss draw nothing and break nothing.
    let bolt = fx(Kind::Bolt { hit: true }, Element::Plain, 0.0, 0.3);
    let mut out = Vec::new();
    layers(&bolt, 0.1, &Geo { height: 850.0, from: None }, &mut out);
    assert!(out.is_empty());
    // Pieces are appended to what the caller has, not over it.
    let mut out = vec![Layer::screen([1.0; 3], 0.1)];
    layers(&fx(Kind::Blast, Element::Fire, 0.0, 0.6), 0.7, &geo(), &mut out);
    assert!(out.len() > 1 && out[0] == Layer::screen([1.0; 3], 0.1));
    // A wild height does not break the effect (a model with no height at all, a giant).
    for height in [0.0, f32::MAX, f32::NAN, -5.0] {
        let mut out = Vec::new();
        layers(&fx(Kind::Blast, Element::Ice, 0.0, 0.6), 0.7, &Geo { height, from: None }, &mut out);
        assert!(out.iter().all(Layer::is_sound), "height {height}");
    }
}

#[test]
fn an_effect_begins_with_its_performance_peaks_at_the_landing_and_fades_out() {
    for kind in all_kinds() {
        let f = fx(kind, Element::Fire, 20.0, 20.6);
        let life = f.life();
        // The strongest piece in the first part of its age is at least as strong as in the second half, and it is nearly gone
        // at the end: it peaks as it lands and fades.
        let early = (0..=15).map(|k| peak(&f, f.land + life * k as f64 * 0.02)).fold(0.0, f32::max);
        let late = (0..50).map(|k| peak(&f, f.land + life * (0.5 + k as f64 * 0.01))).fold(0.0, f32::max);
        let last = peak(&f, f.end() - 1e-3);
        if matches!(kind, Kind::Toss | Kind::Bolt { .. }) {
            // These fly: they have a lead and nothing to say after it (a bolt leaves a line that fades).
            continue;
        }
        assert!(early > 0.0, "{kind:?} shows nothing at its landing");
        assert!(early >= late - 1e-4, "{kind:?}: {early} early, {late} late");
        assert!(last <= 0.3 * early.max(0.01) + 0.02, "{kind:?} is still {last} at its end (peak {early})");
    }
    // The ones that begin with their performance have something to show before they land, and it grows towards the landing.
    for kind in [Kind::Charge, Kind::Blast, Kind::Mend, Kind::Mana, Kind::Raise, Kind::Afflict(Status::Poison), Kind::Boost(Status::Haste)] {
        let f = fx(kind, Element::Ice, 30.0, 30.6);
        let (early, late) = (peak(&f, 30.0 + 0.6 * 0.05), peak(&f, 30.0 + 0.6 * 0.95));
        assert!(early > 0.0, "{kind:?} begins with its performance");
        assert!(late >= early, "{kind:?}: {early} at the start, {late} at the landing");
    }
    // Effects that only happen at the target (a blow, a miss, a guard, a fizzle) have no lead at all.
    for kind in [Kind::Strike { how: Strike::Thump, critical: true }, Kind::Whiff, Kind::Impact { critical: false }, Kind::Cleanse(Status::Sleep), Kind::Flash] {
        let f = fx(kind, Element::Plain, 40.0, 40.0);
        assert_eq!(f.phase(39.99), None, "{kind:?}");
        assert!(peak(&f, 40.0) > 0.0, "{kind:?}");
    }
}

// ----- shapes -------------------------------------------------------------------------------------------------------------

fn prims_at(kind: Kind, element: Element, age: f64) -> Vec<Prim> {
    let f = fx(kind, element, 0.0, 0.0);
    pieces(&f, f.life() * age).iter().map(|l| l.prim).collect()
}

fn colours_at(kind: Kind, element: Element, age: f64) -> Vec<[f32; 3]> {
    let f = fx(kind, element, 0.0, 0.0);
    pieces(&f, f.life() * age).iter().map(|l| l.rgb).collect()
}

#[test]
fn every_element_blasts_in_colours_and_shapes_of_its_own() {
    use Element::*;
    let elements = [Fire, Ice, Bolt, Earth, Wind, Water, Poison, Gravity, Holy, Plain];
    let count = |e, p: Prim| prims_at(Kind::Blast, e, 0.2).iter().filter(|q| **q == p).count();
    // Fire: tongues and embers; ice: a star and shards; bolt: three bolts; wind: blades; poison: bubbles (rings).
    assert!(count(Fire, Prim::Streak) >= 4 && count(Fire, Prim::Disc) >= 8, "fire: {:?}", prims_at(Kind::Blast, Fire, 0.2));
    assert!(count(Ice, Prim::Star) >= 2 && count(Ice, Prim::Streak) >= 6);
    assert_eq!(count(Bolt, Prim::Bolt), 3);
    assert!(count(Wind, Prim::Crescent) >= 4);
    assert!(count(Poison, Prim::Ring) >= 7, "bubbles");
    assert!(count(Earth, Prim::Streak) >= 7, "rocks");
    assert!(count(Water, Prim::Disc) >= 9, "drops");
    assert!(count(Gravity, Prim::Ring) >= 4, "rings closing in");
    assert!(count(Holy, Prim::Streak) >= 3, "a pillar and a cross");
    // No two elements are the same set of pieces, nor the same colour of bloom.
    let shapes: BTreeSet<Vec<u8>> = elements.iter().map(|e| prims_at(Kind::Blast, *e, 0.2).iter().map(|p| p.index() as u8).collect()).collect();
    assert_eq!(shapes.len(), elements.len(), "ten elements, ten different shapes");
    let blooms: Vec<[f32; 3]> = elements.iter().map(|e| colours_at(Kind::Blast, *e, 0.2)[1]).collect();
    for i in 0..blooms.len() {
        for j in i + 1..blooms.len() {
            assert_ne!(blooms[i], blooms[j], "{:?} and {:?}", elements[i], elements[j]);
        }
    }
    // A bolt flickers: the same bolt is not the same brightness at every instant.
    let flicker: BTreeSet<u32> = (0..12).map(|k| (peak(&fx(Kind::Blast, Bolt, 0.0, 0.0), k as f64 * 0.017) * 1000.0) as u32).collect();
    assert!(flicker.len() > 3);
}

#[test]
fn every_status_has_a_sign_of_its_own_and_a_buff_is_told_from_a_curse() {
    let modelled: Vec<Status> = Status::ALL.into_iter().filter(|s| s.is_modelled()).collect();
    assert_eq!(modelled.len(), 13);
    for age in [0.15, 0.4, 0.7] {
        let signs: Vec<Vec<(Prim, [f32; 3], u32)>> = modelled
            .iter()
            .map(|&s| {
                let kind = if s.is_beneficial() { Kind::Boost(s) } else { Kind::Afflict(s) };
                let f = fx(kind, Element::Plain, 0.0, 0.0);
                pieces(&f, f.life() * age).iter().map(|l| (l.prim, l.rgb, (l.alpha * 100.0) as u32)).collect()
            })
            .collect();
        for i in 0..signs.len() {
            assert!(!signs[i].is_empty(), "{} has a sign at {age}", modelled[i]);
            for j in i + 1..signs.len() {
                assert_ne!(signs[i], signs[j], "{} and {} look alike at {age}", modelled[i], modelled[j]);
            }
        }
    }
    // The signs are in the status's own colour: Sleep is blue, Poison violet, Haste cyan, Regen green, Barrier blue and MBarrier pink.
    let main = |s: Status, kind: fn(Status) -> Kind| -> [f32; 3] { colours_at(kind(s), Element::Plain, 0.3)[0] };
    let sleep = main(Status::Sleep, Kind::Afflict);
    assert!(sleep[2] > sleep[0] && sleep[2] > sleep[1] * 0.99, "{sleep:?}");
    let regen = main(Status::Regen, Kind::Boost);
    assert!(regen[1] > regen[0] && regen[1] > regen[2]);
    let haste = main(Status::Haste, Kind::Boost);
    assert!(haste[2] > haste[0] && haste[1] > haste[0]);
    let mbarrier = status_rgb(Status::MBarrier);
    assert!(mbarrier[0] > mbarrier[1] && mbarrier[2] > mbarrier[1], "pink");
    // Sleep draws its Zs: streaks (bars and slashes), nine of them for three letters.
    let zs = prims_at(Kind::Afflict(Status::Sleep), Element::Plain, 0.5).iter().filter(|p| **p == Prim::Streak).count();
    assert_eq!(zs, 9);
    // A barrier is a shell: a ring and a faint fill, taller than it is wide.
    let f = fx(Kind::Boost(Status::Barrier), Element::Plain, 0.0, 0.0);
    let shell = pieces(&f, 0.3).into_iter().find(|l| l.prim == Prim::Ring).expect("a ring");
    let Place::Facing { half, .. } = shell.place else { panic!("a ring faces the camera") };
    assert!(half[1] > half[0] * 1.2, "{half:?}");
    // A cure shows the colour of what it cured.
    let wipe = |s: Status| colours_at(Kind::Cleanse(s), Element::Plain, 0.3);
    assert_ne!(wipe(Status::Poison), wipe(Status::Sleep));
}

#[test]
fn a_critical_hit_is_bigger_and_gold_a_boss_gets_a_bigger_effect_and_a_limit_the_biggest() {
    let size = |l: &Layer| match l.place {
        Place::Facing { half, .. } => half[0].max(half[1]),
        Place::Between { .. } | Place::Screen => 0.0,
    };
    let biggest = |f: &Fx, now: f64| pieces(f, now).iter().map(size).fold(0.0, f32::max);
    let blow = |critical| fx(Kind::Strike { how: Strike::Slash, critical }, Element::Plain, 0.0, 0.0);
    let (plain, crit) = (blow(false), blow(true));
    assert!(biggest(&crit, 0.12) > biggest(&plain, 0.12) * 1.2);
    let gold = |f: &Fx| pieces(f, 0.1).iter().filter(|l| l.rgb[2] < 0.5 && l.rgb[0] > 0.9).count();
    assert!(gold(&crit) > 0 && gold(&plain) == 0, "{} gold pieces in a critical hit, {} in a plain one", gold(&crit), gold(&plain));
    // The same blow on a hound and on a boss.
    let big = |height: f32| {
        let mut out = Vec::new();
        layers(&plain, 0.12, &Geo { height, from: None }, &mut out);
        out.iter().map(size).fold(0.0, f32::max)
    };
    assert!(big(2400.0) > big(800.0) * 2.0);
    // A limit break flashes over the whole picture, a spell's burst does not.
    let flash = fx(Kind::Flash, Element::Plain, 0.0, 0.0);
    assert!(pieces(&flash, 0.02).iter().any(|l| l.place == Place::Screen && l.alpha > 0.3));
    assert!(pieces(&fx(Kind::Blast, Element::Fire, 0.0, 0.0), 0.1).iter().all(|l| l.place != Place::Screen));
    // The flash is short.
    assert!(flash.life() < 0.5 && peak(&flash, flash.life() * 0.9) < 0.15);
    // Strength scales the effect.
    let strong = Fx { scale: 1.6, ..fx(Kind::Blast, Element::Fire, 0.0, 0.0) };
    assert!(biggest(&strong, 0.1) > biggest(&fx(Kind::Blast, Element::Fire, 0.0, 0.0), 0.1) * 1.3);
}

#[test]
fn a_bolt_leaves_the_shooter_reaches_the_target_at_the_landing_and_leaves_a_line_that_fades() {
    let f = fx(Kind::Bolt { hit: true }, Element::Plain, 9.86, 10.0);
    let from = geo().from.unwrap();
    let between = |now: f64| -> Vec<([f32; 3], [f32; 3])> {
        pieces(&f, now).iter().filter_map(|l| if let Place::Between { from, to, .. } = l.place { Some((from, to)) } else { None }).collect()
    };
    // Early in its flight the head is near the shooter, at the landing at the target (the middle of its chest).
    let near = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let early = between(9.86 + 0.14 * 0.1);
    assert!(!early.is_empty());
    assert!(early.iter().all(|(_, head)| near(*head, from) < near(*head, [0.0, 0.0, 0.0])), "the head is nearer the shooter than the target early on");
    let late = between(9.9999);
    assert!(late.iter().all(|(_, head)| near(*head, [0.0, 0.0, 0.0]) < 120.0), "the head is at the target at the landing: {late:?}");
    // After it, the whole line from the shooter to the target is there and fades.
    let line_early = between(10.0 + 0.01);
    assert!(line_early.iter().any(|(a, b)| near(*a, from) < 200.0 && near(*b, [0.0; 3]) < 200.0), "{line_early:?}");
    assert!(peak(&f, 10.0 + f.life() * 0.9) < peak(&f, 10.0 + 0.01) * 0.3);
    // A bolt that misses goes past the target, on the side its seed says.
    let miss = fx(Kind::Bolt { hit: false }, Element::Plain, 9.86, 10.0);
    let head = pieces(&miss, 9.9999).iter().find_map(|l| if let Place::Between { to, .. } = l.place { Some(to) } else { None }).expect("a bolt");
    assert!(head[0].abs() > 300.0, "past the target: {head:?}");
    // The muzzle flashes at the shooter as the bolt leaves.
    assert!(pieces(&f, 9.87).iter().any(|l| l.prim == Prim::Star && matches!(l.place, Place::Facing { at, .. } if near(at, from) < 1.0)));
}

#[test]
fn an_item_flies_to_its_target_on_an_arc_and_one_used_on_oneself_goes_up() {
    let f = fx(Kind::Toss, Element::Plain, 5.0, 5.4);
    let at = |now: f64| -> [f32; 3] {
        let l = pieces(&f, now).into_iter().find(|l| l.prim == Prim::Star).expect("the glint");
        let Place::Facing { at, .. } = l.place else { panic!("faces the camera") };
        at
    };
    let from = geo().from.unwrap();
    let (start, mid, end) = (at(5.0), at(5.2), at(5.3999));
    assert!(((start[0] - from[0]).powi(2) + (start[1] - from[1]).powi(2)).sqrt() < 100.0, "it leaves the user's hand: {start:?}");
    assert!(end[0].abs() < 150.0, "and arrives at the target: {end:?}");
    // Half way it is over the straight line between them (negative y is up).
    let straight = (from[1] + 0.0) * 0.5;
    assert!(mid[1] < straight - 100.0, "an arc: {mid:?} against {straight}");
    let own = Geo { height: 850.0, from: Some([0.0; 3]) };
    let mut out = Vec::new();
    layers(&f, 5.2, &own, &mut out);
    let glint = out.iter().find(|l| l.prim == Prim::Star).expect("a glint");
    let Place::Facing { at, .. } = glint.place else { panic!("faces the camera") };
    assert!(at[1] < -100.0, "tossed up from where it stands: {at:?}");
}

// ----- the kit, the noise and the matrices ------------------------------------------------------------------------------

#[test]
fn an_element_is_read_from_the_attacks_mask_and_a_heal_is_cure() {
    assert_eq!(Element::from_mask(element::FIRE, false), Element::Fire);
    assert_eq!(Element::from_mask(element::ICE, false), Element::Ice);
    assert_eq!(Element::from_mask(element::BOLT, false), Element::Bolt);
    assert_eq!(Element::from_mask(element::GRAVITY, false), Element::Gravity);
    assert_eq!(Element::from_mask(element::RESTORATIVE, false), Element::Cure);
    assert_eq!(Element::from_mask(element::FIRE, true), Element::Cure, "a restoring action is cure whatever its mask");
    assert_eq!(Element::from_mask(0, false), Element::Plain);
    assert_eq!(Element::from_mask(element::CUT | element::HIT | element::SHOOT, false), Element::Plain, "a weapon's kind is not a colour");
    // The first of a mask's elements in the order fire, ice, bolt wins.
    assert_eq!(Element::from_mask(element::BOLT | element::ICE, false), Element::Ice);
    // Every element has a colour of its own, and so has every status.
    let all = [Element::Plain, Element::Fire, Element::Ice, Element::Bolt, Element::Cure, Element::Earth, Element::Wind, Element::Water, Element::Holy, Element::Poison, Element::Gravity];
    for (i, a) in all.iter().enumerate() {
        assert!(a.rgb().iter().all(|c| (0.0..=1.0).contains(c)));
        for b in &all[i + 1..] {
            assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?}");
        }
    }
    let statuses: Vec<[f32; 3]> = Status::ALL.into_iter().filter(|s| s.is_modelled()).map(status_rgb).collect();
    for (i, a) in statuses.iter().enumerate() {
        assert!(a.iter().all(|c| (0.0..=1.0).contains(c)));
        assert!(statuses[i + 1..].iter().all(|b| a != b));
    }
}

#[test]
fn the_primitives_are_additive_meshes_with_triangles_that_fall_to_black_and_the_kit_has_few_of_them() {
    for prim in PRIMS {
        let part = prim_part(prim);
        assert_eq!(part.blend, PartBlend::Additive, "{prim:?}");
        assert_eq!(part.positions.len(), part.colors.len());
        assert!(part.indices.len() >= 12 && part.indices.len().is_multiple_of(3) && part.indices.iter().all(|&i| (i as usize) < part.positions.len()), "{prim:?}");
        // Brightest at the middle, nothing at the rim (the colour of an additive part is its light).
        assert!(part.colors.iter().any(|c| c[0] == 255) && part.colors.iter().any(|c| c[0] == 0), "{prim:?}");
        let radius = part.positions.iter().map(|p| p[0].hypot(p[1])).fold(0.0, f32::max);
        assert!((0.99..=1.5).contains(&radius), "{prim:?}: {radius}");
    }
    // They merge into meshes the renderer can draw, with one additive batch each.
    let kit = EffectKit::new();
    for prim in PRIMS {
        let mesh = &kit.asset(prim).mesh;
        assert_eq!(mesh.batches.len(), 1);
        assert_eq!(mesh.batches[0].kind, crate::avatar::mesh::BatchKind::Additive);
    }
    // Distinct assets, and few: the GPU keeps 32 models in all (a large battle's actors, weapons and location among them), and a
    // frame that needs more than that loses actors. Seven meshes however many effects are drawn.
    let ids: BTreeSet<u64> = PRIMS.iter().map(|p| kit.asset(*p).id).collect();
    assert_eq!((ids.len(), kit.len(), PRIMS.len()), (7, 7, 7));
    assert!(kit.len() <= 8, "the effects' share of the GPU's 32 models");
}

#[test]
fn noise_depends_on_the_seed_and_the_index_only_and_stays_in_range() {
    for seed in [0, 1, 7, u32::MAX] {
        for k in 0..200 {
            let n = noise(seed, k);
            assert!((0.0..1.0).contains(&n), "{seed} {k}: {n}");
            assert_eq!(n, noise(seed, k));
        }
    }
    assert_ne!(noise(1, 0), noise(2, 0), "two effects do not look alike");
    assert_ne!(noise(1, 0), noise(1, 1), "nor two particles of one");
    // Spread over the range: a thousand draws of one seed fall in every tenth.
    let mut tenths = [0u32; 10];
    for k in 0..1000 {
        tenths[(noise(99, k) * 10.0) as usize] += 1;
    }
    assert!(tenths.iter().all(|&n| n > 50), "{tenths:?}");
}

#[test]
fn a_billboard_faces_the_camera_and_turns_in_the_screen_plane() {
    // A camera looking along +x with right = +z, down = +y (the battle camera's own frame).
    let axes = ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]);
    let m = billboard([10.0, 20.0, 30.0], [5.0, 5.0], 0.0, axes);
    let apply = |m: &[f32; 16], v: [f32; 3]| [m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12], m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13], m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14]];
    // Local x lands to the camera's right (+z), local y down (+y), scaled by the radius, about the centre.
    assert_eq!(apply(&m, [0.0; 3]), [10.0, 20.0, 30.0]);
    assert_eq!(apply(&m, [1.0, 0.0, 0.0]), [10.0, 20.0, 35.0]);
    assert_eq!(apply(&m, [0.0, 1.0, 0.0]), [10.0, 25.0, 30.0]);
    // Across and along can be different sizes (an ellipse).
    let oval = billboard([0.0; 3], [2.0, 8.0], 0.0, axes);
    assert_eq!((apply(&oval, [1.0, 0.0, 0.0]), apply(&oval, [0.0, 1.0, 0.0])), ([0.0, 0.0, 2.0], [0.0, 8.0, 0.0]));
    // A quarter turn about the view axis takes local x to where local y was.
    let q = billboard([0.0; 3], [1.0, 1.0], PI / 2.0, axes);
    let moved = apply(&q, [1.0, 0.0, 0.0]);
    assert!((moved[1] - 1.0).abs() < 1e-5 && moved[2].abs() < 1e-5, "{moved:?}");
}

#[test]
fn a_ribbon_joins_two_points_with_its_width_across_the_view() {
    let right = [0.0, 0.0, 1.0];
    let forward = [1.0, 0.0, 0.0];
    let apply = |m: &[f32; 16], v: [f32; 3]| [m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12], m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13], m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14]];
    let (from, to) = ([0.0, 0.0, -400.0], [0.0, 0.0, 600.0]);
    let m = ribbon(from, to, 30.0, right, forward).expect("a ribbon");
    // Local y from -1 to 1 runs from one end to the other.
    assert_eq!(apply(&m, [0.0, -1.0, 0.0]), from);
    assert_eq!(apply(&m, [0.0, 1.0, 0.0]), to);
    // The width is across the segment and across the view: along the screen's other axis here (y), half a width each side.
    let side = apply(&m, [1.0, 0.0, 0.0]);
    assert!((side[1].abs() - 30.0).abs() < 1e-3 && (side[2] - 100.0).abs() < 1e-3, "{side:?}");
    // A segment pointing at the camera still has a width (across the view's right), and a point has no ribbon at all.
    let toward = ribbon([0.0; 3], [500.0, 0.0, 0.0], 30.0, right, forward).expect("a ribbon");
    assert!((apply(&toward, [1.0, 0.0, 0.0])[2] - 30.0).abs() < 1e-3);
    assert!(ribbon([1.0, 2.0, 3.0], [1.0, 2.0, 3.0], 30.0, right, forward).is_none());
    assert!(ribbon([f32::NAN, 0.0, 0.0], [1.0, 2.0, 3.0], 30.0, right, forward).is_none());
}
