//! Checks of `fmv_ff7::field_sound` against the owner's REAL FF7 data (FF7 Steam Edition, English; verified
//! 2026-11-09): which sound opcodes the field scripts use, what the fields play by themselves, and what the first
//! reactor mission's ladders, jumps and lift play.
//!
//! Every test is `#[ignore]` and also returns early when its environment variables are unset, so a normal `cargo test`
//! never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_FF7_AUDIO_DATA=<.../audio data> \
//!   cargo test -p fmv-ff7 --release --test field_sound_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one with `field/flevel.lgp`) and `FMV_FF7_AUDIO_DATA` the audio
//! folder that holds `sound/audio.fmt` and `sound/audio.dat`. Both are only ever opened read-only; the assertions and the
//! printed evidence are counts, ids, lengths and field names: no game bytes and no paths are stored or printed.

mod field_sound_common;

use field_sound_common::*;
use fmv_ff7::field_sound::Source;
use fmv_ff7::sound::SfxBank;
use fmv_ff7::REVIEWED_FIELDS;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

struct Data {
    archive: Archive,
    bank: SfxBank,
    census: Census,
}

/// All 702 fields, the sound bank and the census, read once for every test of this file.
fn data() -> Option<&'static Data> {
    static DATA: OnceLock<Option<Data>> = OnceLock::new();
    DATA.get_or_init(|| {
        let (Some(dir), Some(audio)) = (data_dir(), audio_dir()) else {
            eprintln!("FMV_FF7_DATA and FMV_FF7_AUDIO_DATA are not both set; skipping");
            return None;
        };
        let archive = load_archive(&dir).expect("flevel.lgp");
        let bank = open_bank(&audio).expect("audio.fmt and audio.dat");
        let census = census(&archive.fields);
        Some(Data { archive, bank, census })
    })
    .as_ref()
}

fn fields_of(ops: &[&Op]) -> usize {
    ops.iter().map(|o| o.field.as_str()).collect::<BTreeSet<_>>().len()
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_the_two_sound_opcodes_in_numbers() {
    let Some(d) = data() else { return };
    println!("{}", census_report(&d.census, Some(&d.bank)));
    let sounds: Vec<&Op> = d.census.ops.iter().filter(|o| o.call.source == Source::Sound).collect();
    let akao: Vec<&Op> = d.census.ops.iter().filter(|o| o.call.source == Source::Akao).collect();
    let akao2: Vec<&Op> = d.census.ops.iter().filter(|o| o.call.source == Source::Akao2).collect();
    let both: Vec<&Op> = d.census.ops.iter().filter(|o| o.call.source != Source::Sound).collect();
    let either: BTreeSet<&str> = d.census.ops.iter().map(|o| o.field.as_str()).collect();
    // SOUND: what `fmv_ff7::sound` measured
    assert_eq!((d.census.fields, sounds.len(), fields_of(&sounds)), (702, 4387, 446));
    // the AKAO plays (operation 40 to 43: effect channels 1 to 4) and the AKAO2 ones
    assert_eq!((akao.len(), akao2.len(), fields_of(&both)), (2575, 4, 260));
    assert_eq!(either.len(), 566, "between them the two opcodes play in 566 fields");
    // no banked operand, no tail operand: the id is p2 and the pan p1 in every one of them
    assert_eq!((d.census.banked, d.census.akao_tail_nonzero), (0, 0));
    // the operations around the plays are the ones the forum lists (volumes and pan sweeps 160 to 175 and so on)
    for op in 40..=43 {
        assert!(d.census.akao_operations.contains_key(&op), "operation {op}");
    }
    // where the plays lie
    let at = |ops: &[&Op], w: Where| ops.iter().filter(|o| o.at == w).count();
    assert_eq!((at(&sounds, Where::Init), at(&sounds, Where::Main), at(&sounds, Where::Other)), (25, 245, 4117));
    assert_eq!((at(&both, Where::Init), at(&both, Where::Main), at(&both, Where::Other)), (183, 404, 1992));
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_akao_sound_ids_are_one_based_like_the_sound_opcodes_and_p2_zero_is_a_stop() {
    let Some(d) = data() else { return };
    let plays: Vec<&Op> = d.census.ops.iter().filter(|o| o.call.source != Source::Sound).collect();
    let nonzero: Vec<&&Op> = plays.iter().filter(|o| o.call.id != 0).collect();
    let max = nonzero.iter().map(|o| o.call.id).max().unwrap_or(0);
    let empty_one_based = |id: u16| d.bank.info_for_sound_id(id).is_none();
    // read the other way (entry = id), which ids would land on an empty slot?
    let empty_zero_based = |id: u16| d.bank.info(usize::from(id)).is_none();
    let hits_one: usize = nonzero.iter().filter(|o| empty_one_based(o.call.id)).count();
    let hits_zero: usize = nonzero.iter().filter(|o| empty_zero_based(o.call.id)).count();
    let zero_ids: BTreeSet<u16> = nonzero.iter().map(|o| o.call.id).collect();
    println!(
        "AKAO plays: {} with an id (1..={max}, {} distinct), {} stops; read 1-based {hits_one} land on an empty slot, read 0-based {hits_zero}",
        nonzero.len(),
        zero_ids.len(),
        plays.len() - nonzero.len()
    );
    assert_eq!((nonzero.len(), plays.len() - nonzero.len()), (2054, 525));
    assert!(max <= 750, "no id is past the bank: {max}");
    assert_eq!(hits_one, 0, "read 1-based (entry id - 1), no AKAO id lands on an empty slot");
    assert!(hits_zero > 0, "read 0-based some would: this check tells the readings apart");
    // the SOUND ids still hold, by the same test, after the census's own reading
    let sound_hits = d.census.ops.iter().filter(|o| o.call.source == Source::Sound && o.call.id != 0 && empty_one_based(o.call.id)).count();
    assert_eq!(sound_hits, 0);
    // a play with p2 = 0 carries a pan-like p1 (an id would be anything): 64 or 63 or 127 and a few others
    let pans = &d.census.stop_pans;
    assert_eq!(pans.get(&64), Some(&432));
    assert_eq!(pans.get(&63), Some(&65));
    assert_eq!(pans.values().sum::<usize>(), 525);
    assert!(pans.keys().all(|p| p % 8 == 0 || [63, 95, 127].contains(p)), "{pans:?}");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_an_init_part_only_starts_loops_or_stops_channels() {
    let Some(d) = data() else { return };
    let init: Vec<&Op> = d.census.ops.iter().filter(|o| o.at == Where::Init).collect();
    let stops = init.iter().filter(|o| o.call.id == 0).count();
    let loops_started = init.iter().filter(|o| o.call.id != 0 && loops(&d.bank, o.call.id)).count();
    println!("init-part plays {}: {stops} stops, {loops_started} loop clips", init.len());
    assert_eq!((init.len(), stops, loops_started), (208, 136, 72), "every play of an init part is a stop or a loop clip");
    // the clips' loop flag is what makes them loops: 90 of the 724
    let flagged = (0..d.bank.len()).filter(|&e| d.bank.info(e).is_some_and(|i| i.loop_region.is_some())).count();
    assert_eq!(flagged, 90);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_what_the_fields_play_by_themselves() {
    let Some(d) = data() else { return };
    let mut by_moment = BTreeMap::new();
    for moment in [1u16, 15, 27] {
        let s = ambient_survey(&d.archive.fields, &d.bank, moment);
        let loops: usize = s.per_field.values().map(Vec::len).sum();
        println!(
            "moment {moment}: {} fields, {loops} loops, {} unjudged conditions in {} fields, {} left open, stops {:?}",
            s.per_field.len(),
            s.unjudged,
            s.unjudged_fields,
            s.left_open,
            s.stops
        );
        assert_eq!(s.banked, 0);
        by_moment.insert(moment, s);
    }
    let fields = |m: u16| by_moment[&m].per_field.keys().cloned().collect::<Vec<_>>();
    // the reactor interior: the hum before the boss, nothing in the boss room after it
    for name in ["nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"] {
        assert!(fields(15).iter().any(|f| f == name), "{name} hums at moment 15");
    }
    // a story that moves the moment changes what the field plays: the hum before 27, the alarm at it
    let at = |m: u16, f: &str| by_moment[&m].per_field.get(f).map(|l| l.iter().map(|a| (a.channel, a.id)).collect::<Vec<_>>());
    for f in ["nmkin_2", "nmkin_3", "nmkin_4"] {
        assert_eq!(at(15, f), Some(vec![(3, 38)]), "{f}");
        assert_eq!(at(27, f), Some(vec![(2, 60)]), "{f}");
    }
    assert_eq!(at(15, "nmkin_5"), Some(vec![(3, 38)]));
    assert_eq!(at(27, "nmkin_5"), None);
    for f in ["nrthmk", "nmkin_1", "elevtr1"] {
        assert_eq!(at(15, f), None, "{f}");
        assert_eq!(at(27, f), Some(vec![(2, 60)]), "{f}");
    }
    for f in ["md1stin", "md1_1", "md1_2"] {
        for m in [1, 15, 27] {
            assert_eq!(at(m, f), None, "{f} at {m}");
        }
    }
    // the ids are clips that loop (the rule that makes them ambient) and that exist
    for s in by_moment.values() {
        for list in s.per_field.values() {
            for a in list {
                assert!(loops(&d.bank, a.id) && d.bank.decode_sound_id(a.id).is_ok_and(|c| !c.samples.is_empty()), "ch{} id {} in a start-up", a.channel, a.id);
            }
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_the_reviewed_fields_play_what_their_scripts_say_and_every_clip_exists() {
    let Some(d) = data() else { return };
    let mut needs = BTreeSet::new();
    for name in REVIEWED_FIELDS {
        // the story before the boss (15) and the one after it (27), whose alarm is in the ids too
        let r = reviewed_field(&d.archive, &d.bank, name, &[15, 27]).unwrap_or_else(|| panic!("{name}"));
        let ambient = &r.ambient[0].1;
        println!(
            "{name}: ambient [{}], {} links, {} lift flips, {} exits; ids {:?}",
            show_ambient(ambient),
            r.links.len(),
            r.flips.len(),
            r.exits.len(),
            r.ids
        );
        needs.extend(r.ids.iter().copied());
        // every id a reviewed field needs resolves to a non-empty clip
        for id in &r.ids {
            let clip = d.bank.decode_sound_id(*id).unwrap_or_else(|e| panic!("{name}: sound id {id}: {e}"));
            assert!(!clip.samples.is_empty(), "{name}: sound id {id} is empty");
        }
        match name {
            "md1stin" | "md1_1" | "md1_2" => assert!(r.ids.is_empty(), "{name} plays nothing"),
            "nrthmk" | "nmkin_1" => assert_eq!(r.ids, BTreeSet::from([60]), "{name}: the alarm after the boss and nothing before it"),
            "elevtr1" => {
                let [off, on] = r.flips.as_slice() else { panic!("{:?}", r.flips) };
                // (the scan order: the ride back, then the ride down, or the other way round)
                for (_, events) in [off, on] {
                    assert_eq!(events.iter().map(|e| (e.at, e.call.channel, e.call.id)).collect::<Vec<_>>(), [(0, 1, 42)]);
                }
                assert_eq!(r.flips.iter().filter(|(on, _)| *on).count(), 1);
            }
            "nmkin_3" => {
                let jumps = r.links.iter().filter(|(ok, e)| !ok && e.iter().any(|e| e.call.id == 62 && e.at == 15)).count();
                let ladders = r.links.iter().filter(|(ok, e)| *ok && e.iter().any(|e| e.call.id == 21 && e.at == 0)).count();
                assert_eq!((jumps, ladders), (2, 3), "{:?}", r.links);
            }
            "nmkin_4" => assert!(r.links.iter().any(|(ok, e)| !ok && e.iter().any(|e| e.call.id == 62)) && r.links.iter().any(|(ok, e)| *ok && e.iter().any(|e| e.call.id == 21))),
            _ => {}
        }
    }
    // the clips' lengths and loops, as the docs say
    let info = |id: u16| d.bank.info_for_sound_id(id).unwrap_or_else(|| panic!("{id}"));
    let secs = |id: u16| info(id).frames as f64 / f64::from(info(id).rate);
    assert_eq!(needs, BTreeSet::from([21, 38, 42, 60, 62, 260]));
    for (id, seconds, looped, channels) in [(21u16, 0.14, false, 1u16), (38, 3.92, true, 1), (42, 3.92, true, 1), (60, 1.02, true, 2), (62, 0.22, false, 1), (260, 4.88, false, 1)] {
        assert!((secs(id) - seconds).abs() < 0.01, "id {id}: {} s", secs(id));
        assert_eq!((info(id).loop_region.is_some(), info(id).channels), (looped, channels), "id {id}");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_the_doors_that_play_before_their_jump_in_numbers() {
    let Some(d) = data() else { return };
    let exits = exit_census(&d.archive);
    println!("walk-on script exits {}; with a sound before the jump {} in {} fields", exits.walk_on, exits.with_a_sound, exits.fields.len());
    assert_eq!(exits.walk_on, 1114);
    assert_eq!((exits.with_a_sound, exits.fields.len()), (EXITS_WITH_A_SOUND, EXIT_FIELDS));
    // the reviewed fields have no door with a sound: the server's script exits are theirs only
    for name in REVIEWED_FIELDS {
        assert!(!exits.fields.contains(name), "{name}");
    }
}

/// What the static count of walk-on exits with a sound before their jump came to (see the test above).
const EXITS_WITH_A_SOUND: usize = 150;
const EXIT_FIELDS: usize = 63;

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and sound files (FMV_FF7_AUDIO_DATA)"]
fn real_every_id_any_plan_of_any_field_plays_is_a_clip() {
    let Some(d) = data() else { return };
    for moment in [1u16, 15, 27] {
        let plans = all_plans(&d.archive, &d.bank, moment);
        println!(
            "moment {moment}: links {} in {} fields, flips {} in {} fields, exits with a sound {} in {} fields, {} distinct ids",
            plans.links,
            plans.fields_with_links.len(),
            plans.flips,
            plans.fields_with_flips.len(),
            plans.exits,
            plans.fields_with_exits.len(),
            plans.ids.len()
        );
        let empty: Vec<u16> = plans.ids.iter().copied().filter(|id| d.bank.decode_sound_id(*id).map_or(true, |c| c.samples.is_empty())).collect();
        assert!(empty.is_empty(), "plans play ids that name no clip: {empty:?}");
        assert_eq!((plans.links, plans.fields_with_links.len()), (123, 26));
        assert_eq!((plans.flips, plans.fields_with_flips.len()), (2, 1));
        assert_eq!((plans.exits, plans.fields_with_exits.len()), (6, 6));
    }
}
