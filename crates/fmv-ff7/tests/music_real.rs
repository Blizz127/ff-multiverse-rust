//! Checks of `fmv_ff7::music` against the owner's REAL FF7 data (FF7 Steam Edition, English;
//! verified 2026-11-09): the `music.idx` track list, the music opcodes of all 702 fields, and the
//! evidence that fixes how an AKAO frame id names a track.
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` or `FMV_FF7_AUDIO_DATA` is
//! unset, so a normal `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_FF7_AUDIO_DATA=<.../audio data> \
//!     cargo test -p fmv-ff7 --test music_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one containing `field/flevel.lgp`) and
//! `FMV_FF7_AUDIO_DATA` the audio folder that holds `music/music.idx` and `music_ogg/`. The files
//! are only ever opened read-only; the assertions record counts, ids, field and track names: no
//! game bytes and no game text are stored in this repository. `--nocapture` prints the evidence
//! tables (the same text as `cargo run --example music_survey`).

mod music_common;

use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::music::{MusicList, FRAME_ID_OFFSET};
use music_common::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

struct Data {
    records: Vec<FieldRecord>,
    list: MusicList,
    audio: PathBuf,
}

/// All 702 fields and the track list, loaded once for every test of this file.
fn data() -> Option<&'static Data> {
    static DATA: OnceLock<Option<Data>> = OnceLock::new();
    DATA.get_or_init(|| {
        let Some(dir) = data_dir() else {
            eprintln!("FMV_FF7_DATA is not set; skipping");
            return None;
        };
        let Some(audio) = audio_dir() else {
            eprintln!("FMV_FF7_AUDIO_DATA is not set; skipping");
            return None;
        };
        let mut lgp = LgpArchive::open_path(flevel_path(&dir)).unwrap();
        let records = load_records(&mut lgp).unwrap();
        let list = load_music_list(&audio).unwrap();
        Some(Data { records, list, audio })
    })
    .as_ref()
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn the_track_list_matches_the_audio_folder() {
    let Some(d) = data() else { return };
    let l = &d.list;
    assert_eq!(l.len(), 98);
    let names: Vec<&str> = l.names().iter().map(String::as_str).collect();
    assert_eq!(&names[..7], ["oa", "ob", "dun2", "guitar2", "fanfare", "makoro", "bat"]);
    assert_eq!(&names[96..], ["heart", "roll"]);
    assert!(names.iter().all(|n| !n.is_empty()), "no blank slot");
    assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), 98, "no name twice");
    // the chosen rule: 1-based line = id - FRAME_ID_OFFSET
    assert_eq!(FRAME_ID_OFFSET, 1);
    assert_eq!(l.track_for_frame_id(8), Some("bat"));
    assert_eq!((l.track_for_frame_id(2), l.track_for_frame_id(99)), (Some("oa"), Some("roll")));
    assert_eq!((l.track_for_frame_id(0), l.track_for_frame_id(1), l.track_for_frame_id(100)), (None, None, None));
    assert_eq!((l.frame_id_of("bat"), l.frame_id_of("fanfare"), l.frame_id_of("fan2")), (Some(8), Some(6), Some(47)));
    // every track of the list has its file: 94 `music_ogg/<name>.ogg` and the four `music/<name>.wav`
    let missing: Vec<&str> = names.iter().copied().filter(|n| !track_file_exists(&d.audio, n)).collect();
    assert!(missing.is_empty(), "{missing:?}");
    let wavs: Vec<&str> = names.iter().copied().filter(|n| d.audio.join("music").join(format!("{n}.wav")).is_file()).collect();
    assert_eq!(wavs, ["sato", "wind", "sensui", "heart"]);
    let oggs = names.iter().filter(|n| d.audio.join("music_ogg").join(format!("{n}.ogg")).is_file()).count();
    assert_eq!(oggs, 94);
    // every frame id the fields carry names a track, except id 1; none lies past the list
    let ids: BTreeSet<u16> = d.records.iter().flat_map(FieldRecord::frame_ids).collect();
    let nameless: Vec<u16> = ids.iter().copied().filter(|&id| l.track_for_frame_id(id).is_none()).collect();
    assert_eq!(nameless, [1]);
    assert!(ids.iter().all(|&id| usize::from(id) <= l.len() + usize::from(FRAME_ID_OFFSET)));
    let mapped: BTreeSet<&str> = ids.iter().filter_map(|&id| l.track_for_frame_id(id)).collect();
    assert_eq!(mapped.len(), 88);
    let no_file: Vec<&&str> = mapped.iter().filter(|n| !track_file_exists(&d.audio, n)).collect();
    assert!(no_file.is_empty(), "{no_file:?}");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn every_field_maps_and_the_headline_numbers_hold() {
    let Some(d) = data() else { return };
    let r = &d.records;
    assert_eq!(r.len(), 702);
    assert!(r.iter().all(|f| f.maplist_index.is_some()));
    // AKAO blocks: 613 fields carry some, 1 342 blocks in all, 1 320 of them real "AKAO" frames
    assert_eq!(r.iter().filter(|f| !f.blocks.is_empty()).count(), 613);
    assert_eq!(r.iter().map(|f| f.blocks.len()).sum::<usize>(), 1342);
    assert_eq!(r.iter().map(FieldRecord::frames).sum::<usize>(), 1320);
    let ids: BTreeSet<u16> = r.iter().flat_map(FieldRecord::frame_ids).collect();
    assert_eq!((ids.len(), ids.first().copied(), ids.last().copied()), (89, Some(1), Some(96)));
    // an id is one fixed piece of music data: every block that carries it has the same size, except
    // `bat` (id 8): 4 736 bytes in 251 blocks, 4 740 in two
    let mut sizes: BTreeMap<u16, BTreeMap<usize, usize>> = BTreeMap::new();
    for f in r {
        for (id, len) in f.blocks.iter().zip(&f.block_lens) {
            if let Some(id) = id {
                *sizes.entry(*id).or_default().entry(*len).or_default() += 1;
            }
        }
    }
    let varying: Vec<(&u16, &BTreeMap<usize, usize>)> = sizes.iter().filter(|(_, m)| m.len() > 1).collect();
    assert_eq!(varying.len(), 1, "{varying:?}");
    assert_eq!(varying.first().map(|(id, m)| (**id, m.iter().map(|(l, n)| (*l, *n)).collect::<Vec<_>>())), Some((8, vec![(4736, 251), (4740, 2)])));
    assert_eq!(sizes.get(&1).map(|m| m.iter().map(|(l, n)| (*l, *n)).collect::<Vec<_>>()), Some(vec![(28, 121)]));

    // extraction: the music opcodes with a usable operand, the two that are not
    let uses = |f: fn(&FieldRecord) -> usize| r.iter().map(f).sum::<usize>();
    assert_eq!(uses(|f| f.music.music.len()), 1249);
    assert_eq!(uses(|f| f.music.battle.len()), 347);
    assert_eq!(uses(|f| f.music.after_battle.len()), 30);
    assert_eq!(uses(|f| f.music.skipped), 2);
    let skipped: Vec<&str> = r.iter().filter(|f| f.music.skipped > 0).map(|f| f.name.as_str()).collect();
    assert_eq!(skipped, ["junair2", "junone7"], "a MUSIC 0 that names a tutorial block (22 tutorial blocks in six fields)");
    // every operand is a literal
    assert!(r.iter().flat_map(|f| &f.ops).filter(|o| ["MUSIC", "BMUSC", "FMUSC"].contains(&o.mnemonic)).all(|o| o.operand.is_some()));

    // the other music opcodes the extraction leaves out
    let count = |m: &str| r.iter().flat_map(|f| &f.ops).filter(|o| o.mnemonic == m).count();
    assert_eq!((count("MUSVT"), count("MUSVM"), count("MULCK"), count("CMUSC")), (3, 0, 62, 32));
    // arrival is None for 115 fields: 89 without blocks, 25 whose MUSIC is only in other slots, 1 without any
    assert_eq!(r.iter().filter(|f| f.music.arrival().is_none()).count(), 115);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn the_evidence_fixes_the_frame_id_rule_at_plus_one() {
    let Some(d) = data() else { return };
    println!("{}", evidence_report(&d.records, &d.list));
    println!("{}", size_report(&d.records, &d.list, &d.audio));

    // A and B: member fields on a track their name asks for, k = -1, 0, +1, +2
    let scores: Vec<KScore> = KS.iter().map(|&k| score_k(&d.records, &d.list, k)).collect();
    let hits = |f: fn(&KScore) -> (usize, usize)| scores.iter().map(|s| f(s).0).collect::<Vec<_>>();
    let members = |f: fn(&KScore) -> (usize, usize)| scores.iter().map(|s| f(s).1).collect::<Vec<_>>();
    assert_eq!(hits(KScore::arrival_total), [0, 3, 58, 26]);
    assert_eq!(hits(KScore::arrival_decisive), [0, 1, 32, 0]);
    assert_eq!(hits(KScore::any_music_total), [0, 8, 75, 30]);
    assert_eq!(hits(KScore::any_music_decisive), [0, 2, 42, 0]);
    assert!(members(KScore::arrival_total).iter().all(|&m| m == 136));
    assert!(members(KScore::arrival_decisive).iter().all(|&m| m == 95));
    // C: the standard battle music. 313 distinct (field, BMUSC operand) uses; the track `bat` is
    // named by 250 of them under k = +1 and by none under the other three
    assert_eq!(scores.iter().map(|s| (s.battle_uses, s.battle_on_bat)).collect::<Vec<_>>(), [(313, 0), (313, 0), (313, 250), (313, 0)]);
    let uses = battle_uses(&d.records);
    let mut by_id: BTreeMap<u16, usize> = BTreeMap::new();
    for id in uses.values() {
        *by_id.entry(*id).or_default() += 1;
    }
    assert_eq!((by_id.get(&8), by_id.get(&11)), (Some(&250), Some(&26)));
    assert_eq!((d.list.track_for_frame_id(8), d.list.track_for_frame_id(11)), (Some("bat"), Some("chu")));
    // arrival ids with no line: only id 1 under k = +1 (and +2), none past the list
    assert_eq!(scores.iter().map(|s| s.arrival_ids_without_track).collect::<Vec<_>>(), [0, 0, 1, 1]);
    // D: every offset in -96..=96: +1 wins by a wide margin; the runner-up (+28) is a coincidence
    // between Rocket Town and North Corel (their two tracks are six lines apart, as are the ids)
    for (scope, members, best) in [(Scope::Arrival, 79, 30), (Scope::AnyMusic, 95, 42), (Scope::AnyFrame, 95, 50)] {
        let (votes, n) = offset_ranking(&d.records, &d.list, scope);
        let top = ranked(&votes);
        assert_eq!(n, members, "{scope:?}");
        assert_eq!(top.first(), Some(&(1, best)), "{scope:?}");
        assert_eq!(top.get(1), Some(&(28, 11)), "{scope:?}");
    }
    // Shinra HQ: the arrival ids behind the tie of k = +1 and k = +2 (26 each): 24 fields have id 31
    // (`sinraslo` under +1, `sinra` under +2), and the other two hits are different fields: two
    // with id 30 (`sinra` under +1) and, under +2, two with id 32 (`sinraslo` under +2, `dokubo`
    // under +1)
    let shinra: BTreeMap<Option<u16>, usize> = region_members(&d.records, &REGIONS[0]).iter().fold(BTreeMap::new(), |mut m, r| {
        *m.entry(r.music.arrival()).or_default() += 1;
        m
    });
    assert_eq!(shinra.values().sum::<usize>(), 41);
    assert_eq!(
        shinra.iter().map(|(id, n)| (*id, *n)).collect::<Vec<_>>(),
        [(None, 3), (Some(1), 1), (Some(8), 1), (Some(11), 3), (Some(25), 4), (Some(30), 2), (Some(31), 24), (Some(32), 2), (Some(83), 1)]
    );
    // why the weak regions are weak: half of Junon's 14 fields carry no AKAO block at all, and no
    // field carries a frame for `condor` (id 81)
    let junon = region_members(&d.records, &REGIONS[1]);
    assert_eq!((junon.len(), junon.iter().filter(|r| r.blocks.is_empty()).count()), (14, 7));
    assert_eq!(d.list.frame_id_of("condor"), Some(81));
    assert!(!d.records.iter().any(|r| r.frame_ids().contains(&81)));
    // reading the table backwards: who uses which track (k = +1)
    let usage: BTreeMap<u16, Vec<String>> = track_usage(&d.records, &d.list).into_iter().map(|t| (t.id, t.fields)).collect();
    assert_eq!(usage.get(&2).map(|f| f.iter().map(String::as_str).collect::<Vec<_>>()), Some(vec!["md1stin"])); // oa
    let families = |id: u16| usage.get(&id).map(|f| f.iter().map(|n| family(n)).collect::<BTreeSet<&str>>()).unwrap_or_default();
    assert_eq!(usage.get(&7).map(Vec::len), Some(20)); // makoro: the Mako reactors
    assert_eq!(families(7), BTreeSet::from(["gnmk", "mtcrl", "nmkin", "nvmkin", "semkin", "smkin", "southmk"]));
    assert_eq!((usage.get(&26).map(Vec::len), usage.get(&78).map(Vec::len), usage.get(&77).map(Vec::len)), (Some(17), Some(33), Some(24))); // canyon, snow, utai
    assert_eq!(families(26), BTreeSet::from(["bugin", "cos", "cosin", "cosmin", "cosmo"]));
    assert_eq!(families(78), BTreeSet::from(["hyou", "icedun", "move", "snmayor", "snmin", "sninn", "snow", "snw"]));
    assert_eq!(families(77), BTreeSet::from(["datiao", "hideway", "tower", "uta", "utapb", "utmin", "uttmpin", "uutai", "yufy"]));
    // the four smallest AKAO frames name four of the six shortest tracks the fields reference
    let mut small: Vec<(usize, u16)> = frame_sizes(&d.records).into_iter().map(|(id, s)| (s, id)).collect();
    small.sort_unstable();
    assert_eq!(small.iter().take(4).map(|&(s, _)| s).collect::<Vec<_>>(), [148, 244, 476, 500]);
    let smallest: BTreeSet<&str> = small.iter().take(4).filter_map(|&(_, id)| d.list.track_for_frame_id(id)).collect();
    assert_eq!(smallest, BTreeSet::from(["cintro", "fanfare", "si", "yado"]));
    // E: the size of an id's AKAO frame against the length of the audio of the track it names:
    // r = 0.58 under k = +1 and weak under every other offset from -6 to +8
    let e = size_vs_duration(&d.records, &d.list, &d.audio);
    let plus_one = e.iter().find(|(k, _, _)| *k == 1).copied().unwrap();
    assert_eq!(plus_one.1, 88);
    assert!(plus_one.2 > 0.5, "{plus_one:?}");
    assert!(e.iter().filter(|(k, _, _)| *k != 1).all(|(_, _, r)| *r < 0.3), "{e:?}");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn the_ambiguity_of_the_policy_is_what_the_docs_say() {
    let Some(d) = data() else { return };
    println!("{}", ambiguity_report(&d.records, &d.list));
    println!("{}", tracks_report(&d.records, &d.list));
    let a = ambiguity(&d.records, &d.list);
    assert_eq!((a.fields, a.fields_with_blocks, a.no_blocks, a.frames_without_music), (702, 613, 89, 1));
    // MUSIC in any slot: 612 fields; 375 with one distinct id, 237 with several
    assert_eq!((a.music_any.fields, a.music_any.one, a.music_any.several), (612, 375, 237));
    // the arrival id (first MUSIC in slot 0): 587 fields; 402 with one distinct id, 185 with several
    assert_eq!((a.arrival.fields, a.arrival.one, a.arrival.several), (587, 402, 185));
    assert_eq!(a.music_but_no_arrival, 25);
    // 356 of the 587 have exactly one unconditional MUSIC in the init part; 230 have none; and in
    // 165 of the 185 several-id fields the first use sits behind a branch (in 161 every use does)
    assert_eq!((a.slot0_fields, a.slot0_one_unconditional_init, a.slot0_no_unconditional_init), (587, 356, 230));
    assert_eq!((a.several_first_conditional, a.several_all_conditional), (165, 161));
    // of the 356: `arrival` is that unconditional use in all but `subin_3` (a branch-guarded MUSIC comes first
    // there), and in 18 a main-part MUSIC of another id can replace it later
    assert_eq!(a.one_unconditional_but_first_is_another, ["subin_3"]);
    assert_eq!(a.one_unconditional_with_other_ids, 18);
    // BMUSC: 271 of the 276 fields set one in slot 0 and in none of them does a BMUSC of a later
    // slot come first; five fields have BMUSC only in other slots (scripted fights)
    assert_eq!(a.battle_fields_with_slot0, 271);
    assert!(a.battle_first_in_other_slot_despite_slot0.is_empty());
    let several0 = d.records.iter().filter(|r| r.music.battle.iter().filter(|u| u.slot == 0).map(|u| u.frame_id).collect::<BTreeSet<_>>().len() > 1).count();
    assert_eq!(several0, 20, "fields with several distinct slot-0 BMUSC ids");
    let mut only_other = a.battle_only_in_other_slots.clone();
    only_other.sort();
    assert_eq!(only_other, ["blackbg7", "convil_2", "junele1", "lastmap", "trackin"]);
    // BMUSC: 276 fields, 241 with one distinct id; 303 instructions in slot 0, 44 elsewhere
    assert_eq!((a.battle.fields, a.battle.one, a.battle.several), (276, 241, 35));
    assert_eq!((a.battle_uses_slot0, a.battle_uses_other), (303, 44));
    // FMUSC: 23 fields (one with two ids); 21 also have an arrival and in 8 of them it is the same id
    assert_eq!((a.after_battle.fields, a.after_battle.one, a.after_battle.several), (23, 22, 1));
    assert_eq!((a.after_battle_with_arrival, a.after_battle_equals_arrival), (21, 8));
    // the arrival ids with no track are all id 1: 41 fields
    assert_eq!(a.arrival_without_track.len(), 41);
    assert!(a.arrival_without_track.iter().all(|n| d.records.iter().any(|r| &r.name == n && r.music.arrival() == Some(1))));

    // id 1: 121 frames of 28 bytes, picked by 202 instructions (181 MUSIC, 11 FMUSC, 10 MULCK)
    assert_eq!(uses_of_frame(&d.records, 1).len(), 202);
    assert_eq!(d.records.iter().filter(|r| r.music.distinct_music().contains(&1)).count(), 117);
    // `fanfare` (id 6, the shortest real frame but one: 244 bytes) is started in five fields: by
    // MUSIC in `delmin1` (a talk script), `subin_1a` and `subin_1b` (init part of an entity's
    // script, behind a branch) and by MUSVT in `jundoc1a` and `junin1a` (the Junon ceremony fields)
    let fanfare: BTreeSet<String> = uses_of_frame(&d.records, 6).iter().filter_map(|l| l.split(' ').next().map(str::to_string)).collect();
    assert_eq!(fanfare.iter().map(String::as_str).collect::<Vec<_>>(), ["delmin1", "jundoc1a", "junin1a", "subin_1a", "subin_1b"]);
    assert_eq!(uses_of_frame(&d.records, 6).iter().filter(|l| l.contains(" MUSIC ")).count(), 4);
    assert_eq!(uses_of_frame(&d.records, 6).iter().filter(|l| l.contains(" MUSVT ")).count(), 3);
    // `fan2` (id 47) only in `frcyo`: six CMUSC and one MUSIC, all in talk scripts (slot 1), all conditional
    let fan2 = uses_of_frame(&d.records, 47);
    assert_eq!(fan2.len(), 7);
    assert!(fan2.iter().all(|l| l.starts_with("frcyo ") && l.contains("slot 1") && l.contains("conditional")), "{fan2:?}");
    assert_eq!((fan2.iter().filter(|l| l.contains(" CMUSC ")).count(), fan2.iter().filter(|l| l.contains(" MUSIC ")).count()), (6, 1));
    // neither is ever a battle or after-battle music
    assert!(d.records.iter().flat_map(|r| r.music.battle.iter().chain(&r.music.after_battle)).all(|u| u.frame_id != 6 && u.frame_id != 47));
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn the_fields_from_md1stin_to_elevtr1_in_maplist_order() {
    let Some(d) = data() else { return };
    // The two names bound a contiguous run of the maplist: ids 116..=121, six fields, all in the
    // archive; `nmkin_2` .. `nmkin_5` follow at 122..=125.
    let mut run: Vec<&FieldRecord> = d.records.iter().filter(|r| r.maplist_index.is_some_and(|i| (116..=125).contains(&i))).collect();
    run.sort_by_key(|r| r.maplist_index);
    let names: Vec<&str> = run.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"]);
    let track = |id: u16| d.list.track_for_frame_id(id).unwrap_or("-").to_string();
    for r in &run {
        println!(
            "{:>3} {:<8} arrival {:<7} music {:?} battle {:?} after {:?}",
            r.maplist_index.unwrap_or(0),
            r.name,
            r.music.arrival().map_or("-".to_string(), track),
            r.music.distinct_music().into_iter().map(track).collect::<Vec<_>>(),
            r.music.distinct_battle().into_iter().map(track).collect::<Vec<_>>(),
            r.music.distinct_after_battle().into_iter().map(track).collect::<Vec<_>>(),
        );
    }
    let arrival: Vec<Option<&str>> = run.iter().take(6).map(|r| r.music.arrival().and_then(|id| d.list.track_for_frame_id(id))).collect();
    // the opening: the train station and the streets play `ob`, the reactor `makoro`, the elevator
    // sets no music at all
    assert_eq!(arrival, [Some("ob"), Some("ob"), Some("ob"), Some("ob"), Some("makoro"), None]);
    let first = run.first().unwrap();
    // md1stin: MUSIC 0 (ob) in the entity's own script, MUSIC 1 (oa) in another slot, FMUSC 0 (ob)
    assert_eq!(first.music.distinct_music().into_iter().map(track).collect::<Vec<_>>(), ["ob", "oa"]);
    assert_eq!(first.music.after_battle().map(track), Some("ob".to_string()));
    assert_eq!((first.music.music.first().map(|u| u.frame_id), first.music.music.get(1).map(|u| u.slot)), (Some(3), Some(3)));
    assert_eq!(first.music.battle_theme(), None);
    // the reactor's fights: `ob` and `bat`
    let reactor = run.iter().find(|r| r.name == "nmkin_1").unwrap();
    assert_eq!(reactor.music.distinct_battle().into_iter().map(track).collect::<Vec<_>>(), ["ob", "bat"]);
    // every track these fields name has its audio file
    for r in &run {
        for id in r.music.distinct_music().into_iter().chain(r.music.distinct_battle()).chain(r.music.distinct_after_battle()) {
            if let Some(t) = d.list.track_for_frame_id(id) {
                assert!(track_file_exists(&d.audio, t), "{} -> {t}", r.name);
            }
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA, FMV_FF7_AUDIO_DATA)"]
fn the_extraction_agrees_with_an_independent_scan_of_the_scripts() {
    let Some(d) = data() else { return };
    let mut unreachable = Vec::new();
    let mut checked = 0usize;
    let mut conditional_differs: Vec<String> = Vec::new();
    for r in &d.records {
        // every music instruction the scan finds, resolved through the field's own blocks, is a
        // use of the extraction at the same place with the same id, phase and branch flag
        let extracted: BTreeMap<(usize, usize, usize), &fmv_ff7::music::MusicUse> =
            r.music.music.iter().chain(&r.music.battle).chain(&r.music.after_battle).map(|u| ((u.entity, u.slot, u.offset), u)).collect();
        let mut expected = 0usize;
        for o in &r.ops {
            if !o.reachable {
                unreachable.push(format!("{} {} slot {}", r.name, o.mnemonic, o.slot));
            }
            // a script that holds music is never shared between slot 0 and another slot, nor
            // between two entities: the first slot that points at it is its owner
            assert!(!(o.slot != 0 && o.refs.iter().any(|&(_, s)| s == 0)), "{} {}", r.name, o.mnemonic);
            assert!(o.refs.iter().all(|&(e, _)| e == o.entity), "{} {}", r.name, o.mnemonic);
            if !["MUSIC", "BMUSC", "FMUSC"].contains(&o.mnemonic) {
                continue;
            }
            let frame = o.operand.and_then(|v| usize::try_from(v).ok()).and_then(|b| r.blocks.get(b).copied().flatten());
            let Some(frame) = frame else { continue };
            expected += 1;
            let u = extracted.get(&(o.entity, o.slot, o.offset)).unwrap_or_else(|| panic!("{} {} at {} not extracted", r.name, o.mnemonic, o.offset));
            assert_eq!(u.frame_id, frame, "{} {}", r.name, o.mnemonic);
            assert_eq!(u.phase, o.phase, "{} {}", r.name, o.mnemonic);
            if o.reachable {
                checked += 1;
                if u.conditional != o.conditional {
                    conditional_differs.push(format!("{} {} slot {} offset {}: extraction {}, scan {}", r.name, o.mnemonic, o.slot, o.offset, u.conditional, o.conditional));
                }
            }
        }
        assert_eq!(extracted.len(), expected, "{}", r.name);
    }
    // 331 of the 1 725 music-related instructions lie in scripts that several slots share
    let all_ops = d.records.iter().flat_map(|r| &r.ops).count();
    let shared = d.records.iter().flat_map(|r| &r.ops).filter(|o| o.refs.len() > 1).count();
    assert_eq!((shared, all_ops), (331, 1725));
    // two music instructions lie in unreachable code, both outside slot 0 (`jtemplc`)
    assert_eq!(unreachable, ["jtemplc MUSIC slot 3", "jtemplc CMUSC slot 4"]);
    // The branch flag of the extraction (every forward jump) equals the reachability-aware one of
    // the script-0 analysis except for two instructions of `southmk2` (slot 3): a `JMPF` in dead
    // code (it follows an unconditional `JMPB`) is counted as skipping the live code behind it.
    assert_eq!(
        conditional_differs,
        ["southmk2 MUSIC slot 3 offset 1221: extraction true, scan false", "southmk2 MUSIC slot 3 offset 1273: extraction true, scan false"],
        "of {checked}"
    );
    // (1 249 MUSIC + 347 BMUSC + 30 FMUSC extracted, less the unreachable MUSIC)
    assert_eq!(checked, 1249 + 347 + 30 - 1);
}
