//! Checks of the scene interpreter against the owner's REAL FF7 data (FF7 Steam Edition, English; verified 2026-11-09).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal `cargo test` never touches
//! game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test scene_real -- --ignored --nocapture
//! ```
//!
//! The files are only ever opened read-only. The assertions record ids, counts, entities, moments and the order of
//! windows (their dialog indexes and speakers); no game text is stored in this repository and none is printed.
//!
//! One test per acceptance point of the proof scene (Cloud and Tifa at the bar of 7th Heaven, field `mds7pb_1`):
//!
//! 1. the trigger is found: [`the_trigger_is_what_the_field_does`], [`the_trigger_npc_is_tifa_where_her_stage_script_puts_her`];
//! 2. the lines come out in order: [`the_scene_plays_its_real_lines_in_order_with_their_speakers`];
//! 3. the witness stands on the floor, clear of everyone: [`the_witness_stands_on_the_floor_clear_of_everyone_the_scene_ever_places`];
//! 4. the interpreter finishes within budget and every op it meets is handled:
//!    [`the_scene_finishes_within_budget_and_every_op_it_meets_is_handled_or_known_to_change_nothing`];
//! 5. what the story carries: [`the_proof_scene_writes_only_its_mark_once_and_a_scene_that_moves_the_moment_reports_it`].

mod script_common;

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{
    proof, proof_scene, run_to_end, trigger_npc, CastMember, Ground, Look, MoveHow, SceneDef, ScenePlay, ScriptRef, Stage, Stats, Step, Then,
    Trigger, PROOF_FIELD,
};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

fn field(name: &str) -> Option<FieldFile> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    Some(load_field(&mut lgp, name).unwrap())
}

/// One window of a played scene: who opened it, who speaks, which dialog, whether it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    opener: u8,
    speaker: Option<u8>,
    dialog: usize,
    asks: bool,
}

const fn line(opener: u8, speaker: u8, dialog: usize, asks: bool) -> Line {
    Line { opener, speaker: Some(speaker), dialog, asks }
}

struct Played {
    lines: Vec<Line>,
    end: End,
    stats: Stats,
    /// Every stage event, the initial placements first.
    stage: Vec<Stage>,
}

/// Play the proof scene at `moment`, answering each question from `answers`.
fn play(f: &FieldFile, moment: u16, answers: &[usize]) -> Played {
    let def = proof_scene();
    let mut p = ScenePlay::new(f, &def, &StoryState::at(moment), 1).unwrap();
    let mut answers = answers.iter().copied();
    let mut given = None;
    let (mut lines, mut stage) = (Vec::new(), Vec::new());
    stage.extend(p.initial_stage_events().iter().cloned());
    let end = loop {
        let beat = p.next(given.take());
        stage.extend(beat.stage.iter().map(|t| t.what.clone()));
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                let MessageOrigin::Canon { dialog, .. } = w.message.origin else { panic!("an added line in a canon scene") };
                lines.push(Line { opener: w.opener, speaker: w.speaker, dialog, asks: w.message.choice.is_some() });
                if w.message.choice.is_some() {
                    given = answers.next();
                }
            }
        }
        assert!(lines.len() < 64, "a scene that never ends");
    };
    Played { lines, end, stats: p.stats().clone(), stage }
}

const CLOUD: u8 = proof::CLOUD;
const TIFA: u8 = proof::TIFA;

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn post_escape_director_reaches_its_scripted_destination() {
    let f = field("md8_4").expect("FMV_FF7_DATA and md8_4");
    let def = fmv_ff7::script::scene::post_escape_scene();
    let mesh = f.walkmesh().unwrap();
    let witness = def.witness[0];
    assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(witness.triangle)], i32::from(witness.x), i32::from(witness.y)));
    let outcome = run_to_end(&f, &def, &StoryState::at(33), 1, &[]).unwrap();
    eprintln!("post-escape outcome: {:?}, windows={}, stats={:?}", outcome.end, outcome.windows, outcome.stats);
    assert_eq!(outcome.end, End::MapJump { field: 133 });
    assert_eq!(outcome.jump, Some(fmv_ff7::script::scene::SceneJump { field: 133, x: 1629, y: -5357, triangle: 15, direction: 192 }));
    assert!(outcome.windows >= 3);
    assert!(outcome.stats.unknown.is_empty());
    assert_eq!(outcome.stats.dropped_requests, 0);
    assert_eq!(outcome.stats.unresolved, 0);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn scene_jump_field_id_resolves_to_an_existing_field() {
    use fmv_core::{EditionProvenance, GameId, Platform};
    let dir = data_dir().expect("FMV_FF7_DATA");
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(),
        region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap();
    assert_eq!(adapter.field_key_for_id(133).as_deref(), Some("md8_1"));
    assert_eq!(adapter.field_key_for_id(u16::MAX), None);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn station_director_advances_the_post_escape_story() {
    let f = field("md8_1").expect("FMV_FF7_DATA and md8_1");
    let def = fmv_ff7::script::scene::station_scene();
    let outcome = run_to_end(&f, &def, &StoryState::at(33), 1, &[]).unwrap();
    eprintln!("station director: end={:?}, windows={}, writes={:?}, stats={:?}", outcome.end, outcome.windows, outcome.writes, outcome.stats);
    assert_eq!(outcome.end, End::Finished);
    assert_eq!(outcome.writes.moment, Some(36));
    assert!(outcome.windows >= 4);
    assert!(outcome.stats.unknown.is_empty());
    assert_eq!(outcome.stats.unresolved, 0);
}

/// The scene of `md8_1` is where the team splits up: its director takes Barret out of the party (`PRTYM 1`) and writes moment
/// 36, and the story's own party table has exactly that party from 36 on, which is how the server carries the change. The
/// full restore it runs (`HMPMAX3`) is skipped, and nothing else that touches the player's state is.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_station_scene_takes_barret_out_of_the_party_and_the_storys_party_table_agrees() {
    use fmv_ff7::story::{canon_party, PartyOp, BARRET, CLOUD};
    let f = field("md8_1").expect("FMV_FF7_DATA and md8_1");
    let def = fmv_ff7::script::scene::station_scene();
    let before = StoryState::at(33);
    let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
    assert_eq!(outcome.party, vec![PartyOp::Remove(BARRET)], "the one thing the scene does to the party");
    let mut after = before.clone();
    assert!(outcome.writes.apply_to(&mut after));
    assert_eq!(after.moment, 36);
    assert_eq!((canon_party(&before), canon_party(&after)), (vec![CLOUD, BARRET], vec![CLOUD]));
    assert!(outcome.party_is_the_storys(&before, &after), "the table's row of moment 36 is what the script leaves");
    assert!(!outcome.party_is_the_storys(&before, &before), "the same scene on a story that stays put would lose Barret's leaving");
    for stats in [&outcome.setup_stats, &outcome.stats] {
        assert!(stats.unknown.is_empty(), "{:?}", stats.unknown);
        assert!(!stats.ignored.contains_key("PRTYM"), "a party change is recorded, not skipped");
    }
    assert_eq!(outcome.stats.ignored.get("HMPMAX3"), Some(&1), "the full restore: nothing here keeps HP or MP between battles");
    assert!(outcome.unknown_persistent_reads.is_empty(), "{:?}", outcome.unknown_persistent_reads);
    assert!(outcome.writes.tracked.is_empty() && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank)), "{:?}", outcome.writes);
}

/// The first scene after the escape touches no party and nothing saved: it ends in the field's own jump to `md8_1`.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_post_escape_scene_changes_nothing_that_is_saved() {
    use fmv_ff7::script::scene::AnimHow;
    let f = field("md8_4").expect("FMV_FF7_DATA and md8_4");
    let def = fmv_ff7::script::scene::post_escape_scene();
    let outcome = run_to_end(&f, &def, &StoryState::at(33), 1, &[]).unwrap();
    assert!(outcome.party.is_empty() && outcome.writes.is_story_neutral(), "{:?} {:?}", outcome.party, outcome.writes);
    assert!(outcome.unknown_persistent_reads.is_empty() && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank)));
    let script = FieldScript::from_field(&f).unwrap();
    let barret = script.entity_inits(None).into_iter().find(|init| init.entity == 2).unwrap();
    assert_eq!((barret.name.as_str(), barret.model_index), ("balette", Some(1)));
    let loader = f.model_loader().unwrap();
    assert_eq!(loader.models[1].hrc_stem(), "acgd");
    assert_eq!(loader.models[1].animations[3].stem(), "apfc");
    let mut play = ScenePlay::new(&f, &def, &StoryState::at(33), 1).unwrap();
    assert_eq!(play.animation_clip(2, 3), Some("apfc"));
    assert!(play.next(None).stage.iter().any(|event| matches!(event.what, Stage::Anim {
        entity: 2, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(0), speed: 16, divisor: 1,
    })), "Barret's main script requests a single held frame before the first window");
    let mut characters = LgpArchive::open_path(data_dir().unwrap().join("field/char.lgp")).unwrap();
    let clip = fmv_ff7::load_field_animation(&mut characters, "apfc").unwrap();
    let model = fmv_ff7::load_field_model(&mut characters, "acgd").unwrap();
    assert_eq!((clip.frame_count(), clip.bone_count()), (10, 21));
    assert!(model.fits(&clip), "the owned Hold clip fits Barret's retained skeleton");
    assert!(clip.frame(0).is_some() && model.pose(&clip, 0).is_some(), "the authored frame is in bounds and can be posed");
    // at any other moment the director does nothing at all: no window, no jump
    for moment in [1u16, 27, 32, 34, 36] {
        let idle = run_to_end(&f, &def, &StoryState::at(moment), 1, &[]).unwrap();
        assert_eq!((idle.windows, idle.jump), (0, None), "moment {moment}");
    }
}

/// The team's talk at the reactor's gate is the cutscene of `md1_1`'s story point to 7: played against the story before
/// the step, the field's own director runs the twelve windows, writes exactly the moment the story point writes, and ends
/// in its jump on (which a cutscene does not make). Its cast are the field's own models, Cloud seeded on the triangle the
/// director watches (without him there, nothing happens), and it touches no party and nothing else that is saved.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_team_talk_is_the_cutscene_of_md1_1s_story_point_and_writes_its_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{scene, team_talk_scene, TEAM_TALK_SCENE_ID};
    let f = field("md1_1").expect("FMV_FF7_DATA and md1_1");
    let def = team_talk_scene();
    assert_eq!(scene(TEAM_TALK_SCENE_ID).as_ref(), Some(&def), "registered");
    assert!(!def.starts_on_arrival() && !def.offered_to(&StoryState::at(6)), "a cutscene is never offered by itself");
    // The story point of the field, as the server serves it.
    let dir = data_dir().unwrap();
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap();
    let area = adapter.load_field("md1_1").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 1, "{points:?}");
    for moment in [1u16, 6] {
        let before = StoryState::at(moment);
        let step = area.story_advance(&before.encode(), points[0].id).expect("the point applies before 7");
        let after = StoryState::decode(&step.progress).unwrap();
        assert_eq!(after.moment, 7);
        assert!(def.presents_step("md1_1", &before, &after), "moment {moment}");
        assert!(!def.presents_step("md1_2", &before, &after) && !def.presents_step("md1_1", &after, &after));
        let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::MapJump { field: 118 }, 12, 0), "moment {moment}");
        assert_eq!(outcome.writes.moment, Some(after.moment), "the scene's own write is the point's step");
        assert!(outcome.writes.tracked.is_empty() && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank)), "{:?}", outcome.writes);
        assert!(outcome.party.is_empty());
        assert!(outcome.is_complete(), "setup {:?} / scene {:?}", outcome.setup_stats.unknown, outcome.stats.unknown);
    }
    assert!(def.presents_step("md1_1", &StoryState::at(1), &StoryState::at(7)));
    assert!(!def.presents_step("md1_1", &StoryState::at(7), &StoryState::at(8)), "a step that is not this point's");
    // Without Cloud on the triangle the director waits: the seed is what starts it.
    let mut unseeded = def.clone();
    for member in &mut unseeded.cast {
        member.start = None;
    }
    assert_eq!(run_to_end(&f, &unseeded, &StoryState::at(6), 1, &[]).unwrap().windows, 0);
    // The cast are the field's own models.
    let script = FieldScript::from_field(&f).unwrap();
    let models = fmv_ff7::script::parse_model_list(f.section(fmv_ff7::field::section::MODEL_LOADER).unwrap()).unwrap();
    let loader = f.model_loader().unwrap();
    for member in &def.cast {
        let init = script.entity_inits(Some(&models)).into_iter().find(|i| i.entity == usize::from(member.entity)).unwrap();
        let model = &loader.models[init.model_index.expect("a model")];
        let Look::Model { hrc, idle } = &member.look else { panic!("a field model") };
        assert_eq!((model.hrc_stem(), model.animations.first().map(|a| a.stem())), (hrc.clone(), Some(idle.clone())), "{}", member.name);
        assert_eq!(init.name, member.name);
    }
    // Who speaks: the windows' openers are the team (the survey: av_m, av_l, cl, ba).
    let mut play = ScenePlay::new(&f, &def, &StoryState::at(6), 1).unwrap();
    let mut openers = Vec::new();
    loop {
        match play.next(None).then {
            Then::End { .. } => break,
            Then::Window(w) => openers.push(w.opener),
        }
    }
    assert_eq!(openers, [3, 4, 4, 3, 3, 1, 3, 1, 2, 2, 2, 2]);
    assert_eq!(play.off_cast(), 0, "every stage event is about the cast");
}

/// Barret's talk at the reactor's door is the cutscene of `nmkin_1`'s story line to 11: played against the story before the
/// step, the line's own script runs six windows, writes exactly the moment the line point writes, and sets the party the story
/// already has. Cloud starts on the line (the initial stage puts him there), Barret where the field's staging puts him.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_reactor_door_talk_is_the_cutscene_of_nmkin_1s_story_line_and_writes_its_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{reactor_door_scene, scene, REACTOR_DOOR_SCENE_ID};
    use fmv_ff7::story::{canon_party, party_after, CLOUD, BARRET};
    let f = field("nmkin_1").expect("FMV_FF7_DATA and nmkin_1");
    let def = reactor_door_scene();
    assert_eq!(scene(REACTOR_DOOR_SCENE_ID).as_ref(), Some(&def), "registered");
    let dir = data_dir().unwrap();
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap();
    let area = adapter.load_field("nmkin_1").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 1, "{points:?}");
    let before = StoryState::at(10);
    let step = area.story_advance(&before.encode(), points[0].id).expect("the line applies at 10");
    let after = StoryState::decode(&step.progress).unwrap();
    assert_eq!(after.moment, 11);
    assert!(def.presents_step("nmkin_1", &before, &after));
    let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 6, 0));
    assert_eq!(outcome.writes.moment, Some(after.moment), "the scene's own write is the line's step");
    assert!(outcome.writes.tracked.is_empty() && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank)), "{:?}", outcome.writes);
    assert_eq!(party_after(&canon_party(&before), &outcome.party), vec![CLOUD, BARRET], "the party it sets is the one the story has");
    assert!(outcome.is_complete(), "setup {:?} / scene {:?}", outcome.setup_stats.unknown, outcome.stats.unknown);
    let script = FieldScript::from_field(&f).unwrap();
    let models = fmv_ff7::script::parse_model_list(f.section(fmv_ff7::field::section::MODEL_LOADER).unwrap()).unwrap();
    let loader = f.model_loader().unwrap();
    for member in &def.cast {
        let init = script.entity_inits(Some(&models)).into_iter().find(|i| i.entity == usize::from(member.entity)).unwrap();
        let model = &loader.models[init.model_index.expect("a model")];
        let Look::Model { hrc, idle } = &member.look else { panic!("a field model") };
        assert_eq!((model.hrc_stem(), model.animations.first().map(|a| a.stem())), (hrc.clone(), Some(idle.clone())), "{}", member.name);
        assert_eq!(init.name, member.name);
    }
    let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
    // Both stand somewhere when it begins: Cloud on the line, Barret at the door (the field's own staging).
    let placed: Vec<u8> = play.initial_stage_events().iter().filter_map(|e| match e { Stage::Place { entity, .. } => Some(*entity), _ => None }).collect();
    assert!(placed.contains(&3) && placed.contains(&4), "{placed:?}");
    let mut openers = Vec::new();
    loop {
        match play.next(None).then {
            Then::End { .. } => break,
            Then::Window(w) => openers.push(w.opener),
        }
    }
    assert_eq!(openers, [4, 3, 4, 4, 3, 12], "Barret, Cloud, Barret, Barret, Cloud, and the door's window");
    assert_eq!(play.off_cast(), 0, "every stage event is about the cast");
}

/// The talk before the Guard Scorpion is the cutscene of `nmkin_5`'s story point to 27, the point that starts the boss battle
/// first: played against the story before the step, Barret and Cloud have eleven windows and the scene ends at the battle (the
/// step is paid by the server when the battle is won, so the scene writes no moment of its own). Cloud is seeded on the
/// triangle the director watches; without him there nothing happens.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_guard_scorpion_talk_is_the_cutscene_of_the_point_that_starts_the_boss_battle() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{guard_scorpion_scene, scene, GUARD_SCORPION_SCENE_ID};
    let f = field("nmkin_5").expect("FMV_FF7_DATA and nmkin_5");
    let def = guard_scorpion_scene();
    assert_eq!(scene(GUARD_SCORPION_SCENE_ID).as_ref(), Some(&def), "registered");
    let dir = data_dir().unwrap();
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap();
    let area = adapter.load_field("nmkin_5").unwrap();
    let before = StoryState::at(26);
    let (point, step) = area.story_points().iter().find_map(|p| area.story_advance(&before.encode(), p.id).map(|s| (p.id, s))).expect("a point applies at 26");
    assert_eq!(area.story_battle(point), Some(324), "the point starts the Guard Scorpion first");
    let after = StoryState::decode(&step.progress).unwrap();
    assert_eq!(after.moment, 27);
    assert!(def.presents_step("nmkin_5", &before, &after));
    let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Battle, 11, 0));
    assert_eq!(outcome.writes.moment, None, "the step comes with the win, not in the scene");
    assert!(outcome.party.is_empty() && outcome.is_complete(), "{:?} {:?}", outcome.party, outcome.stats.unknown);
    let mut unseeded = def.clone();
    for member in &mut unseeded.cast {
        member.start = None;
    }
    assert_eq!(run_to_end(&f, &unseeded, &before, 1, &[]).unwrap().windows, 0);
    let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
    let mut openers = Vec::new();
    let mut sounds = Vec::new();
    loop {
        let beat = play.next(None);
        sounds.extend(beat.stage.into_iter().filter_map(|event| match event.what {
            Stage::Sound { id, pan } => Some((event.at, id, pan)),
            _ => None,
        }));
        match beat.then {
            Then::End { .. } => break,
            Then::Window(w) => openers.push(w.opener),
        }
    }
    assert_eq!(openers, [4, 4, 3, 4, 3, 3, 3, 3, 4, 3, 3]);
    assert_eq!(sounds.iter().map(|(_, id, pan)| (*id, *pan)).collect::<Vec<_>>(),
        [(236, 64), (0, 64), (61, 64), (59, 64), (60, 64), (0, 64)]);
    assert_eq!(sounds[3].0 - sounds[2].0, 60, "the bomb's two sounds are separated by WAIT 60");
    assert_eq!(sounds[4].0 - sounds[3].0, 30, "WAIT 10 and WAIT 20 precede the alarm");
    assert_eq!(play.off_cast(), 0);
}

/// A scene that names no witness spots (the reactor's cutscenes) gets them from its own data: on the floor the arrival is on,
/// clear of every place the cast takes by two body rings and a margin and of the floor's edge by one and a half, apart from
/// each other, and the same every time (every client of a run must agree).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_cutscenes_get_witness_spots_clear_of_their_cast() {
    use fmv_ff7::script::scene::{auto_witnesses, cast_places, guard_scorpion_scene, reactor_door_scene, team_talk_scene};
    for (def, moment) in [(team_talk_scene(), 6u16), (reactor_door_scene(), 10), (guard_scorpion_scene(), 26)] {
        assert!(def.witness.is_empty(), "{}: the data names none", def.id);
        let f = field(&def.field).unwrap();
        let story = StoryState::at(moment);
        let spots = auto_witnesses(&f, &def, &story, 1, 3);
        assert_eq!(spots, auto_witnesses(&f, &def, &story, 1, 3), "{}: the same every time", def.id);
        assert!(!spots.is_empty(), "{}: at least the owner's", def.id);
        let mesh = f.walkmesh().unwrap();
        let parts = mesh_parts(&mesh);
        let script = FieldScript::from_field(&f).unwrap();
        let ring = 5.5 * fmv_ff7::default_model_scale(script.header().scale);
        let cast = cast_places(&f, &def, &story, 1).unwrap();
        assert!(!cast.is_empty());
        for (i, s) in spots.iter().enumerate() {
            let tri = &mesh.triangles[usize::from(s.triangle)];
            assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(s.x), i32::from(s.y)), "{} spot {i} on its triangle", def.id);
            assert_eq!(parts[usize::from(s.triangle)], parts[usize::from(def.arrival.triangle)], "{} spot {i} on the arrival's floor", def.id);
            let nearest = cast.iter().map(|&(x, y)| (x - f32::from(s.x)).hypot(y - f32::from(s.y))).fold(f32::INFINITY, f32::min);
            assert!(nearest >= 2.0 * ring + 16.0, "{} spot {i} is {nearest:.0} from the cast", def.id);
            assert!(scenery_gap(&mesh, f32::from(s.x), f32::from(s.y)) >= 1.5 * ring, "{} spot {i} too near an edge", def.id);
            for o in spots.iter().skip(i + 1) {
                assert!((f32::from(o.x) - f32::from(s.x)).hypot(f32::from(o.y) - f32::from(s.y)) >= 2.0 * ring + 16.0, "{}: spots apart", def.id);
            }
            eprintln!("{} spot {i}: ({}, {}) triangle {} direction {} - {nearest:.0} from the cast, {} from the arrival", def.id, s.x, s.y, s.triangle, s.direction,
                (f32::from(s.x) - f32::from(def.arrival.x)).hypot(f32::from(s.y) - f32::from(def.arrival.y)) as i32);
        }
    }
    // the facing a spot gets is a whole DIR byte that turns back into the heading toward the cast's middle
    for d in [0u8, 1, 64, 127, 128, 192, 255] {
        assert_eq!(fmv_ff7::facing_to_field_direction(fmv_ff7::field_direction_to_facing(d)), d);
    }
}

/// The parts of a walkmesh that a walk can reach from each other (triangles joined by an edge that can be crossed, either way).
fn mesh_parts(mesh: &fmv_ff7::field::Walkmesh) -> Vec<usize> {
    let n = mesh.triangles.len();
    let mut part = vec![usize::MAX; n];
    let mut next = 0;
    for start in 0..n {
        if part[start] != usize::MAX {
            continue;
        }
        let mut stack = vec![start];
        part[start] = next;
        while let Some(t) = stack.pop() {
            for (other, slot) in part.iter_mut().enumerate() {
                let joined = (0..3).any(|e| mesh.neighbor(t, e) == Some(other) || mesh.neighbor(other, e) == Some(t));
                if joined && *slot == usize::MAX {
                    *slot = next;
                    stack.push(other);
                }
            }
        }
        next += 1;
    }
    part
}

/// Every place a story comes back into the world at ([`STORY_ENTRIES`]) is a place of the owner's data: the field exists, the
/// spot is on the triangle it names, and that triangle is in the part of the floor every way out of the field is in (a player
/// put anywhere else would be stranded). The rows ascend, and each names the arrival the game's own data gives.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_story_entry_stands_on_the_floor_the_fields_ways_out_are_on() {
    use fmv_ff7::script::scene::{story_entry, STORY_ENTRIES};
    assert!(STORY_ENTRIES.windows(2).all(|w| w[0].from_moment < w[1].from_moment), "rows ascend by moment");
    for row in STORY_ENTRIES {
        let f = field(row.field).unwrap_or_else(|| panic!("FMV_FF7_DATA and {}", row.field));
        let mesh = f.walkmesh().unwrap();
        let tri = mesh.triangles.get(usize::from(row.spot.triangle)).unwrap_or_else(|| panic!("{}: triangle {}", row.field, row.spot.triangle));
        assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(row.spot.x), i32::from(row.spot.y)), "{}: the spot is on its triangle", row.field);
        let parts = mesh_parts(&mesh);
        let here = parts[usize::from(row.spot.triangle)];
        let gateways: Vec<_> = f.triggers().unwrap().gateways.into_iter().filter(fmv_ff7::field::Gateway::is_used).collect();
        assert!(!gateways.is_empty(), "{}: a field somebody is put in has a way out", row.field);
        for g in &gateways {
            let (mx, my) = ((i32::from(g.exit_a[0]) + i32::from(g.exit_b[0])) / 2, (i32::from(g.exit_a[1]) + i32::from(g.exit_b[1])) / 2);
            let on: Vec<usize> = (0..mesh.triangles.len()).filter(|&t| fmv_ff7::script::triangle_contains_xy(&mesh.triangles[t], mx, my)).collect();
            assert!(on.iter().any(|&t| parts[t] == here), "{}: the way out to field {} is on part(s) {:?}, the entry on part {here}", row.field, g.target_field_id,
                on.iter().map(|&t| parts[t]).collect::<Vec<_>>());
        }
    }
    // the rows are the arrivals the data names: the gateway out of the reactor, and the jump the first scene ends in
    assert_eq!(story_entry(32), None, "a story still in the reactor joins where everyone joins");
    let outside = story_entry(33).unwrap();
    let nrthmk = field("nrthmk").unwrap();
    let to_md8_4 = nrthmk.triggers().unwrap().gateways.into_iter().filter(fmv_ff7::field::Gateway::is_used).find(|g| g.destination[0] == outside.spot.x && g.destination[1] == outside.spot.y);
    assert!(to_md8_4.is_some(), "the entry of moment 33 is where nrthmk's own gateway arrives");
    let jump = run_to_end(&field("md8_4").unwrap(), &fmv_ff7::script::scene::post_escape_scene(), &StoryState::at(33), 1, &[]).unwrap().jump.unwrap();
    let street = story_entry(36).unwrap();
    assert_eq!((street.field, street.spot.x, street.spot.y, i16::try_from(street.spot.triangle).unwrap(), street.spot.direction), ("md8_1", jump.x, jump.y, jump.triangle, jump.direction));
    assert_eq!(story_entry(u16::MAX).map(|e| e.field), Some("md8_1"), "until a later row says otherwise");

    // Each scene's witness stands on its own triangle, on the floor the entry is on (the player walks away from it after).
    for def in [fmv_ff7::script::scene::post_escape_scene(), fmv_ff7::script::scene::station_scene()] {
        let f = field(&def.field).unwrap();
        let mesh = f.walkmesh().unwrap();
        let parts = mesh_parts(&mesh);
        let entry = STORY_ENTRIES.iter().find(|e| e.field == def.field).expect("an entry in the scene's field");
        let w = def.witness.first().copied().unwrap_or_else(|| panic!("{}: a witness spot", def.id));
        let tri = &mesh.triangles[usize::from(w.triangle)];
        assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(w.x), i32::from(w.y)), "{}: the witness is on triangle {}", def.id, w.triangle);
        assert_eq!(parts[usize::from(w.triangle)], parts[usize::from(entry.spot.triangle)], "{}: the witness and the entry share the floor", def.id);
    }
}

/// The scenes the game offers: ids are unique, a scene a talk starts has a reach and one that starts on arrival has none, and
/// an arrival scene is offered at moments no other arrival scene of its field is (two could not both start).
#[test]
fn the_offered_scenes_are_told_apart_and_start_one_way_each() {
    use fmv_ff7::script::scene::scenes;
    let all = scenes();
    for (i, a) in all.iter().enumerate() {
        assert!(all.iter().skip(i + 1).all(|b| b.id != a.id), "{}", a.id);
        assert_eq!(a.starts_on_arrival(), a.trigger.talk_radius == 0 && !a.is_point_cutscene());
        if let Some(moment) = a.presents {
            // A cutscene: no reach, offered at moments before the one its point moves to, and in a field whose story points
            // are served (a reviewed field), or it would never play.
            assert_eq!(a.trigger.talk_radius, 0, "{}", a.id);
            assert!(*a.trigger.moments.end() < moment, "{}", a.id);
            assert!(fmv_ff7::is_reviewed_field(&a.field), "{}: a cutscene's point must be served", a.id);
        }
        if let Some((bank, address, bit)) = a.presents_bit {
            assert!(a.presents.is_none() && a.resume_after_battle.is_none(), "{}", a.id);
            assert_eq!(a.trigger.talk_radius, 0, "{}", a.id);
            assert!(StoryState::at(*a.trigger.moments.start()).known_bit(bank, address, bit).is_some());
            assert!(fmv_ff7::is_reviewed_field(&a.field), "{}: a cutscene's point must be served", a.id);
        }
        if a.starts_on_arrival() {
            for b in all.iter().skip(i + 1).filter(|b| b.starts_on_arrival() && b.field.eq_ignore_ascii_case(&a.field)) {
                assert!(a.trigger.moments.end() < b.trigger.moments.start() || b.trigger.moments.end() < a.trigger.moments.start(), "{} and {}", a.id, b.id);
            }
        }
    }
    assert_eq!(all.first().map(|s| s.id.as_str()), Some(fmv_ff7::script::scene::PROOF_SCENE_ID), "a scene keeps its number: new ones are added at the end");
    // An arrival scene's field must not be a reviewed one: in a reviewed field the field's own story points are offered, and
    // the one that fires on entering it (`md8_1`'s director writes 33 -> 36 as an entry point) would move the story BEFORE the
    // scene is judged (the world runs the entry points first), so the scene would never be offered. Such a field reaches the
    // story through its scene alone.
    for s in all.iter().filter(|s| s.starts_on_arrival()) {
        assert!(!fmv_ff7::is_reviewed_field(&s.field), "{}: an arrival scene's field is not a reviewed field", s.id);
    }
    assert!(fmv_ff7::script::scene::is_scene_field("MD8_1") && !fmv_ff7::script::scene::is_scene_field("md1stin"));
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); the train scene is not yet enabled"]
fn train_director_finishes_after_its_authored_actor_work() {
    let f = field("tin_1").expect("FMV_FF7_DATA and tin_1");
    let def = fmv_ff7::script::scene::train_scene();
    let outcome = run_to_end(&f, &def, &StoryState::at(48), 1, &[]).unwrap();
    assert!(!outcome.setup_stats.setup_incomplete, "train setup must not stop on a budget or scene end");
    assert_eq!(outcome.end, End::Finished);
    assert_eq!(outcome.windows, 2);
    assert_eq!(outcome.writes.moment, Some(51));
    assert_eq!(outcome.stats.quiescent_tails, 1);
    assert_eq!(outcome.stats.unresolved, 0);
    assert_eq!(outcome.stats.dropped_requests, 0);
    assert!(outcome.stats.unknown.is_empty());
    let mut play = ScenePlay::new(&f, &def, &StoryState::at(48), 1).unwrap();
    loop { if let Then::End { how, .. } = play.next(None).then { assert_eq!(how, End::Finished); break; } }
    assert_eq!(play.off_cast(), 0, "the authored stage events have a cast actor");
    let models = f.model_loader().unwrap();
    for (cast, model_index) in def.cast.iter().zip([0usize, 1, 3, 4, 5, 6, 7, 10, 11]) {
        let Look::Model { hrc, idle } = &cast.look else { panic!("expected field model") };
        let model = &models.models[model_index];
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); early bar is not enabled"]
fn early_bar_survey_tracks_both_choices_and_stages_its_cast() {
    let f = field("mds7pb_1").expect("FMV_FF7_DATA and mds7pb_1");
    let def = fmv_ff7::script::scene::early_bar_scene();
    let loader = f.model_loader().unwrap();
    for (member, index) in def.cast.iter().skip(2).zip([4usize, 5, 6, 7, 9, 10]) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        let model = &loader.models[index];
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
    for answer in [1usize, 2] {
        let outcome = run_to_end(&f, &def, &StoryState::at(63), 1, &[answer]).unwrap();
        assert!(outcome.unknown_persistent_reads.contains(&(3, 211)));
        let mut play = ScenePlay::new(&f, &def, &StoryState::at(63), 1).unwrap();
        assert!(!play.setup_stats().setup_incomplete);
        let mut next = None;
        let mut windows = 0;
        loop {
            match play.next(next.take()).then {
                Then::Window(w) => { windows += 1; if w.message.choice.is_some() { next = Some(answer); } }
                Then::End { how, .. } => { assert_eq!(how, End::Finished); break; }
            }
        }
        assert_eq!(windows, 6);
        assert_eq!(play.writes().moment, Some(69));
        assert_eq!(play.writes().untracked.get(&(3, 211)), Some(&if answer == 1 { 5 } else { 4 }));
        assert_eq!((play.stats().unresolved, play.stats().dropped_requests, play.off_cast()), (0, 0, 0));
        assert!(play.stats().unknown.is_empty());
        assert!(play.stats().ignored.contains_key("MMBud") && play.stats().ignored.contains_key("MENU"));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); station arrival is not enabled"]
fn sector7_station_survey_runs_the_authored_arrival_chain() {
    let f = field("mds7st3").expect("FMV_FF7_DATA and mds7st3");
    let def = fmv_ff7::script::scene::sector7_station_scene();
    let story = StoryState::at(51);
    let outcome = run_to_end(&f, &def, &story, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.writes.moment), (End::Finished, 1, Some(63)));
    assert!(outcome.unknown_persistent_reads.is_empty());
    assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
    assert!(outcome.stats.unknown.is_empty());
    assert!(outcome.stats.handled.contains_key("SOUND"), "literal sound events are reported; this survey does not render audio");
    let mut play = ScenePlay::new(&f, &def, &story, 1).unwrap();
    loop { if let Then::End { how, .. } = play.next(None).then { assert_eq!(how, End::Finished); break; } }
    assert_eq!(play.off_cast(), 0);
    let loader = f.model_loader().unwrap();
    for (member, index) in def.cast.iter().zip([0usize, 1, 3, 4, 5, 6, 7, 8, 9, 11]) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        let model = &loader.models[index];
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); Sector 7 intro is not enabled"]
fn sector7_intro_survey_reports_its_assumed_flags_and_cast() {
    let f = field("mds7").expect("FMV_FF7_DATA and mds7");
    let def = fmv_ff7::script::scene::sector7_intro_scene();
    let story = StoryState::at(63);
    let outcome = run_to_end(&f, &def, &story, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.writes.moment), (End::Finished, 0, None));
    assert_eq!(outcome.writes.untracked.get(&(3, 209)), Some(&173));
    assert!(outcome.unknown_persistent_reads.contains(&(3, 209)));
    assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
    assert!(outcome.stats.unknown.is_empty());
    assert!(outcome.stats.handled.contains_key("SOUND"));
    let mut play = ScenePlay::new(&f, &def, &story, 1).unwrap();
    loop { if let Then::End { how, .. } = play.next(None).then { assert_eq!(how, End::Finished); break; } }
    assert_eq!(play.off_cast(), 0);
    let loader = f.model_loader().unwrap();
    for (member, index) in def.cast.iter().zip(1usize..=10) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        let model = &loader.models[index];
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); the gil scene is not enabled"]
fn flower_station_survey_distinguishes_purchase_and_nonpurchase_paths() {
    let f = field("md8_2").expect("FMV_FF7_DATA and md8_2");
    let def = fmv_ff7::script::scene::flower_station_scene();
    let story = StoryState::at(36);
    let loader = f.model_loader().unwrap();
    for (member, model) in def.cast.iter().zip(&loader.models) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
    for (answers, debit, windows) in [(&[2usize][..], false, 3), (&[3, 2, 0][..], true, 7), (&[3, 3, 0][..], true, 8), (&[3, 2, 1][..], false, 6), (&[3, 3, 1][..], false, 7)] {
        let outcome = run_to_end(&f, &def, &story, 1, answers).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.writes.moment), (End::Finished, windows, Some(39)));
        assert_eq!(outcome.stats.ignored.contains_key("GOLDd"), debit);
        assert_eq!(outcome.writes.untracked.get(&(3, 66)), Some(&1));
        assert_eq!(outcome.writes.untracked.contains_key(&(3, 111)), debit);
        assert!(outcome.unknown_persistent_reads.contains(&(3, 66)));
        assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
        assert!(outcome.stats.unknown.is_empty());
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); pillar entry is not enabled"]
fn sector7_pillar_survey_reports_untracked_entry_flag_and_cast() {
    let f = field("mds7plr1").expect("FMV_FF7_DATA and mds7plr1");
    let def = fmv_ff7::script::scene::sector7_pillar_scene();
    let story = StoryState::at(63);
    let outcome = run_to_end(&f, &def, &story, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.writes.moment), (End::Finished, 0, None));
    assert_eq!(outcome.writes.untracked.get(&(3, 208)), Some(&1));
    assert!(outcome.unknown_persistent_reads.contains(&(3, 208)));
    assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
    assert!(outcome.stats.unknown.is_empty());
    let mut play = ScenePlay::new(&f, &def, &story, 1).unwrap();
    loop { if let Then::End { how, .. } = play.next(None).then { assert_eq!(how, End::Finished); break; } }
    assert_eq!(play.off_cast(), 0);
    let loader = f.model_loader().unwrap();
    for (member, index) in def.cast.iter().zip([2usize, 4, 5, 6, 10]) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        let model = &loader.models[index];
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); cargo train is not enabled"]
fn cargo_train_survey_runs_director_and_field_staging() {
    let f = field("cargoin").expect("FMV_FF7_DATA and cargoin");
    let def = fmv_ff7::script::scene::cargo_train_scene();
    let story = StoryState::at(39);
    let outcome = run_to_end(&f, &def, &story, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.writes.moment), (End::Finished, 26, Some(48)));
    assert!(outcome.unknown_persistent_reads.is_empty());
    assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
    assert!(outcome.stats.unknown.is_empty());
    // The field's own staging declares each actor's model and binding (CHAR, PC): known declarations, skipped.
    assert!(outcome.setup_stats.ignored.contains_key("CHAR") && outcome.setup_stats.ignored.contains_key("PC"));
    assert!(outcome.setup_stats.unknown.is_empty(), "{:?}", outcome.setup_stats.unknown);
    let mut play = ScenePlay::new(&f, &def, &story, 1).unwrap();
    loop { if let Then::End { how, .. } = play.next(None).then { assert_eq!(how, End::Finished); break; } }
    assert_eq!(play.off_cast(), 0);
    let loader = f.model_loader().unwrap();
    for (member, model) in def.cast.iter().zip(&loader.models) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); opening is not enabled"]
fn reactor_opening_survey_exposes_unsafe_director_tail() {
    let f = field("md1stin").expect("FMV_FF7_DATA and md1stin");
    let def = fmv_ff7::script::scene::reactor_opening_scene();
    let script = FieldScript::from_field(&f).unwrap();
    assert!(script.disassemble_partial(0, 0).unwrap().instructions.iter().any(|i|
        i.mnemonic == "SETWORD" && i.operand("var").is_some_and(|v| (v.bank, v.value) == (2, 0)) && i.literal("value") == Some(1)
    ));
    let result = run_to_end(&f, &def, &StoryState::at(0), 1, &[]).unwrap();
    assert_eq!((result.end, result.windows), (End::Battle, 1));
    assert!(result.stats.handled.contains_key("GETAI"));
    assert!(result.stats.unresolved > 0);
    assert!(result.stats.handled.contains_key("SOUND"));
    for op in ["MMBLK", "MOVIE", "PMVIE"] {
        assert!(result.stats.ignored.contains_key(op), "{op}");
    }
    // The opening sets a cutscene's party (`PRTYE 0, 254, 254`: Cloud and two empty slots): recorded, not skipped.
    assert!(!result.stats.ignored.contains_key("PRTYE"));
    assert_eq!(result.party, vec![fmv_ff7::story::PartyOp::Set([0, 254, 254])]);
    assert_eq!(result.writes.untracked.get(&(1, 4)), Some(&30));
    assert!(result.unknown_persistent_reads.contains(&(1, 4)));
    let loader = f.model_loader().unwrap();
    for (member, model) in def.cast.iter().zip(&loader.models) {
        let Look::Model { hrc, idle } = &member.look else { panic!("expected a field model") };
        assert_eq!(model.hrc_stem(), *hrc);
        assert_eq!(model.animations.first().map(|a| a.stem()).as_deref(), Some(idle.as_str()));
    }
}

// ---- 1. the trigger is found -----------------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_trigger_is_what_the_field_does() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let script = FieldScript::from_field(&f).unwrap();
    let def = proof_scene();

    // The entities the data names are the entities the field has.
    let names: Vec<_> = [proof::ID, CLOUD, TIFA].iter().map(|e| script.entity_name(usize::from(*e)).unwrap().to_string()).collect();
    assert_eq!(names, ["ID", "cloud", "tifa"]);
    assert_eq!(def.trigger.name, "tifa");
    assert_eq!(def.trigger.entity, TIFA);
    // Cloud and Tifa are party members' entities (`PC`), which is why the server never announces them as NPCs.
    let players = script.player_entities();
    assert!(players.contains(&(usize::from(CLOUD), 0)) && players.contains(&(usize::from(TIFA), 2)), "{players:?}");

    // The `ID` entity's script 0 starts the scene: the three steps, one after the other, are the scene's root.
    let listing = script.disassemble(usize::from(proof::ID), 0).unwrap();
    let steps: Vec<&str> = listing.iter().map(|i| i.mnemonic).collect();
    let at = (0..listing.len().saturating_sub(2))
        .find(|&i| {
            let (a, b, c) = (&listing[i], &listing[i + 1], &listing[i + 2]);
            a.mnemonic == "BITON"
                && a.operand("var").is_some_and(|v| (v.bank, v.value) == (3, 212))
                && a.literal("bit") == Some(2)
                && b.mnemonic == "REQEW"
                && (b.literal("entity"), b.literal("script")) == (Some(i32::from(CLOUD)), Some(i32::from(proof::CLOUD_SITS)))
                && c.mnemonic == "REQSW"
                && (c.literal("entity"), c.literal("script")) == (Some(i32::from(TIFA)), Some(i32::from(proof::COUNTER_TALK)))
        })
        .unwrap_or_else(|| panic!("the field's own trigger is not what the scene says it is: {steps:?}"));
    assert!(at > 0);
    assert_eq!(
        def.root,
        [
            Step::SetBit { bank: 3, address: 212, bit: 2 },
            Step::Await(ScriptRef::new(CLOUD, proof::CLOUD_SITS)),
            Step::Start(ScriptRef::new(TIFA, proof::COUNTER_TALK)),
        ]
    );
    // It watches Cloud's triangle: 62, the stool, which is where the cast puts Cloud.
    assert!(
        listing.iter().any(|i| i.mnemonic == "SETWORD" && i.operand("var").is_some_and(|v| (v.bank, v.value) == (6, 29)) && i.literal("value") == Some(62)),
        "the loop's mark is triangle 62"
    );
    assert_eq!(def.member(CLOUD).and_then(|m| m.start).map(|s| s.triangle), Some(62));
    let cloud_sits = script.disassemble(usize::from(CLOUD), usize::from(proof::CLOUD_SITS + 1)).unwrap();
    assert!(
        cloud_sits.iter().any(|i| i.mnemonic == "XYZI" && i.literal("triangle") == Some(62) && i.literal("x") == Some(-95) && i.literal("y") == Some(-144)),
        "Cloud's script 10 sits him at the stool the cast starts him at"
    );

    // Tifa's talk script is gated on the moment the scene is offered at, and her stage script sets the talk radius.
    let talk = script.disassemble(usize::from(TIFA), usize::from(proof::TALK)).unwrap();
    assert!(
        talk.iter().any(|i| i.mnemonic == "IFSW" && i.operand("left").is_some_and(|l| (l.bank, l.value) == (2, 0)) && i.literal("right") == Some(72) && i.literal("op") == Some(0)),
        "Tifa's talk script tests Var[2][0] == 72"
    );
    assert!(def.applies_at(72));
    let stage = script.disassemble(usize::from(TIFA), usize::from(proof::TIFA_STAGE)).unwrap();
    assert!(
        stage.iter().any(|i| i.mnemonic == "TALKR" && i.literal("radius") == Some(i32::from(def.trigger.talk_radius))),
        "her stage script sets the radius the data records"
    );
    assert!(script.span(usize::from(TIFA), usize::from(def.trigger.slot)).is_some());
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_trigger_npc_is_tifa_where_her_stage_script_puts_her() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    let npc = trigger_npc(&f, &def, &StoryState::at(72)).expect("the trigger entity is found in the field");
    assert_eq!((npc.entity, npc.name.as_str()), (TIFA, "tifa"));
    // Tifa's model and standing clip are the playable table's own (`ff7/tifa`): the NPC and the cast actor match.
    let tifa = fmv_ff7::resolve_appearance("ff7/tifa").unwrap();
    assert_eq!((npc.model.as_str(), npc.idle.as_deref()), (tifa.hrc, Some(tifa.idle_anim)));
    let cloud = fmv_ff7::resolve_appearance("ff7/cloud").unwrap();
    assert_eq!(cloud.hrc, "aaaa");
    // Behind the counter, in her lane, on triangle 7, facing +x; she can be talked to from 150 units.
    assert_eq!((npc.x, npc.y), (-268, -105));
    let ground = Ground::new(f.walkmesh().unwrap());
    assert_eq!(ground.triangle_at(npc.x, npc.y), Some(7));
    assert!(npc.z.abs() < 1.0, "{npc:?}");
    assert!((npc.heading - 0.0).abs() < 1e-5);
    assert!((npc.talk_radius - 150.0).abs() < 1e-3);
    // Before the scene's moment her stage script puts her somewhere else or nowhere the scene knows: the scene is only
    // offered at 72.
    assert!(!def.applies_at(69) && !def.applies_at(78));
}

// ---- 2. the canon lines play in order ----------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_scene_plays_its_real_lines_in_order_with_their_speakers() {
    let Some(f) = field(PROOF_FIELD) else { return };
    // Answering the drink "something hard" (line 2): eleven windows, Tifa's and Cloud's, the question among them.
    let hard = play(&f, 72, &[2]);
    assert_eq!(
        hard.lines,
        [
            line(TIFA, TIFA, 28, false),
            line(TIFA, TIFA, 29, true),
            line(TIFA, TIFA, 31, false),
            line(TIFA, TIFA, 32, false),
            line(TIFA, CLOUD, 33, false),
            line(TIFA, TIFA, 34, false),
            line(TIFA, TIFA, 35, false),
            line(TIFA, CLOUD, 36, false),
            line(TIFA, TIFA, 37, false),
            line(CLOUD, CLOUD, 38, false),
            line(TIFA, TIFA, 39, false),
        ]
    );
    assert_eq!(hard.end, End::Finished);
    // Cloud's lines are Cloud's: the windows his name is on, whichever script opened them.
    let by = |lines: &[Line], who: u8| lines.iter().filter(|l| l.speaker == Some(who)).count();
    assert_eq!((by(&hard.lines, TIFA), by(&hard.lines, CLOUD)), (8, 3));
    assert_eq!(hard.lines.iter().filter(|l| l.asks).count(), 1);
    // Every window has a speaker: nothing is narration here.
    assert!(hard.lines.iter().all(|l| l.speaker.is_some()));

    // Answering "I don't feel like it" (line 1), or nothing at all (which is the cursor's line, 1): six windows.
    let short = [line(TIFA, TIFA, 28, false), line(TIFA, TIFA, 29, true), line(TIFA, TIFA, 30, false), line(TIFA, TIFA, 37, false), line(CLOUD, CLOUD, 38, false), line(TIFA, TIFA, 39, false)];
    assert_eq!(play(&f, 72, &[1]).lines, short);
    assert_eq!(play(&f, 72, &[]).lines, short);

    // The same story and seed play the same scene (every client and the server agree).
    assert_eq!(play(&f, 72, &[2]).lines, hard.lines);
    // An answer that is no choice is taken as the nearest one (the engine's own clamp), never as a new branch.
    assert_eq!(play(&f, 72, &[99]).lines, hard.lines);
}

// ---- 3. the witness stands on the floor, clear of everyone ------------------------------------

/// Every place the cast takes in a played scene: the initial placements and every walk, sampled each ten units.
fn cast_path(stage: &[Stage]) -> Vec<(u8, f32, f32)> {
    let mut at: std::collections::BTreeMap<u8, (f32, f32)> = Default::default();
    let mut out = Vec::new();
    for s in stage {
        match *s {
            Stage::Place { entity, x, y, .. } => {
                at.insert(entity, (f32::from(x), f32::from(y)));
                out.push((entity, f32::from(x), f32::from(y)));
            }
            Stage::Move { entity, from, x, y, how, .. } => {
                let start = from.map(|(fx, fy)| (f32::from(fx), f32::from(fy))).or_else(|| at.get(&entity).copied());
                let end = (f32::from(x), f32::from(y));
                if let (Some(a), MoveHow::Walk | MoveHow::Jump | MoveHow::Ladder) = (start, how) {
                    let n = ((end.0 - a.0).hypot(end.1 - a.1) / 10.0).ceil().max(1.0) as u32;
                    for k in 0..=n {
                        let t = k as f32 / n as f32;
                        out.push((entity, a.0 + (end.0 - a.0) * t, a.1 + (end.1 - a.1) * t));
                    }
                }
                at.insert(entity, end);
                out.push((entity, end.0, end.1));
            }
            _ => {}
        }
    }
    out
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_witness_stands_on_the_floor_clear_of_everyone_the_scene_ever_places() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    let ground = Ground::new(f.walkmesh().unwrap());
    let script = FieldScript::from_field(&f).unwrap();
    // One model unit is `default_model_scale(512)` walkmesh units; a ring under a character is 5.5 model units across
    // its radius, so two characters clear of each other stand more than two ring radii apart.
    let scale = fmv_ff7::default_model_scale(script.header().scale);
    let clearance = 2.0 * 5.5 * scale;
    assert!(clearance >= 40.0, "{clearance}");
    let required = clearance + 16.0;

    // Every place the cast ever stands or walks, over every way the scene can go (both answers).
    let mut path: Vec<(u8, f32, f32)> = Vec::new();
    for answers in [vec![2usize], vec![1usize]] {
        path.extend(cast_path(&play(&f, 72, &answers).stage));
    }
    assert!(path.iter().any(|p| p.0 == CLOUD) && path.iter().any(|p| p.0 == TIFA));
    // The NPCs the field announces stand there too.
    let statics: Vec<(f32, f32)> = fmv_ff7::script::dialogue::characters(&script)
        .iter()
        .flat_map(|c| c.places.iter().map(|&(x, y)| (f32::from(x), f32::from(y))))
        .collect();

    // Where the debug preview puts a player: on the floor (the triangle the data names holds it) and within the trigger's reach
    // of Tifa, who stands where her stage script puts her.
    let a = def.arrival;
    assert_eq!(ground.triangle_at(a.x, a.y), Some(a.triangle), "the arrival is on triangle {:?}", ground.triangle_at(a.x, a.y));
    let npc = trigger_npc(&f, &def, &StoryState::at(72)).unwrap();
    let reach = (f32::from(a.x) - f32::from(npc.x)).hypot(f32::from(a.y) - f32::from(npc.y));
    assert!(reach <= npc.talk_radius, "the arrival is {reach:.0} from Tifa (her reach is {})", npc.talk_radius);
    // (the band of floor within Tifa's reach is narrow: at the counter's edge a body's ring, 22, just touches the stools)
    let arrival_gap = scenery_gap(ground.mesh(), f32::from(a.x), f32::from(a.y));
    assert!(arrival_gap >= 5.5 * scale - 2.0, "the arrival is {arrival_gap:.1} from the nearest wall, stool or table");

    assert_eq!(def.witness.len(), 3, "the owner and two watchers");
    for (i, spot) in def.witness.iter().enumerate() {
        // on the floor: inside the triangle the data names, at that triangle's height
        let tri = ground.mesh().triangles.get(usize::from(spot.triangle)).expect("a triangle of the mesh");
        assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(spot.x), i32::from(spot.y)), "witness {i} is on triangle {}", spot.triangle);
        let h = ground.height(spot.x, spot.y, Some(spot.triangle), None).expect("a height");
        let want = fmv_ff7::script::triangle_height_at(tri, i32::from(spot.x), i32::from(spot.y)).unwrap() as f32;
        assert!((h - want).abs() < 0.01, "witness {i}: {h} vs {want}");
        // clear of every actor, at every place of the scene
        let nearest = path.iter().map(|&(_, x, y)| (x - f32::from(spot.x)).hypot(y - f32::from(spot.y))).fold(f32::INFINITY, f32::min);
        assert!(nearest >= required, "witness {i} comes within {nearest:.0} of an actor (needs {required:.0})");
        // clear of the NPCs, and of each other
        for &(sx, sy) in &statics {
            assert!((sx - f32::from(spot.x)).hypot(sy - f32::from(spot.y)) >= required, "witness {i} stands on an NPC");
        }
        for (j, other) in def.witness.iter().enumerate().filter(|(j, _)| *j != i) {
            let d = (f32::from(other.x) - f32::from(spot.x)).hypot(f32::from(other.y) - f32::from(spot.y));
            assert!(d >= required, "witnesses {i} and {j} are {d:.0} apart");
        }
        // clear of the scenery the walkmesh knows: the stools and tables are holes in it, so the body's ring (5.5 model units) must not
        // reach a boundary edge (an edge with no neighbour)
        let edge_gap = scenery_gap(ground.mesh(), f32::from(spot.x), f32::from(spot.y));
        assert!(edge_gap >= 5.5 * scale * 1.5, "witness {i} stands {edge_gap:.0} from the nearest wall, stool or table (its ring is {:.0})", 5.5 * scale);
        eprintln!(
            "witness {i}: ({}, {}) triangle {} clear of every actor by {nearest:.0} (needs {required:.0}), {edge_gap:.0} from the nearest wall, stool or table",
            spot.x, spot.y, spot.triangle
        );
    }
}

/// The distance in x and y from `(x, y)` to the nearest boundary edge of the walkmesh (an edge with no neighbour: a wall, or the
/// edge of a hole such as a stool or a table).
fn scenery_gap(mesh: &fmv_ff7::field::Walkmesh, x: f32, y: f32) -> f32 {
    let mut best = f32::INFINITY;
    for (i, tri) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            if mesh.neighbor(i, e).is_some() {
                continue;
            }
            let (a, b) = (tri[e], tri[(e + 1) % 3]);
            let (ax, ay, bx, by) = (f32::from(a.x), f32::from(a.y), f32::from(b.x), f32::from(b.y));
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min((x - (ax + dx * t)).hypot(y - (ay + dy * t)));
        }
    }
    best
}

// ---- 4. within budget, every op handled --------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_scene_finishes_within_budget_and_every_op_it_meets_is_handled_or_known_to_change_nothing() {
    let Some(f) = field(PROOF_FIELD) else { return };
    // The ops the VM carries out, and the ones it knows change nothing a scene shows.
    let handled = [
        "ANIM!2", "ANIME1", "ANIME2", "ANIMW", "ASK", "BITON", "CANM!2", "DFANM", "DIR", "DIRA", "IFUB", "IFUBL", "JMPF", "JMPFL", "MESSAGE", "MOVE", "PLUS!",
        "REQEW", "REQSW", "RET", "SCR2D", "SCR2DC", "SCRCC", "SCRLW", "SETBYTE", "SOUND", "TURA", "WAIT", "WINDOW", "XYZI",
    ];
    let ignored = ["MENU2", "TALKR", "UC", "WREST"];
    let mut seen_handled = std::collections::BTreeSet::new();
    for answers in [vec![2usize], vec![1usize], vec![]] {
        let p = play(&f, 72, &answers);
        assert_eq!(p.end, End::Finished, "{answers:?}");
        let s = &p.stats;
        assert!(s.steps < 1_000, "{answers:?}: {} instructions (the budget is 200 000)", s.steps);
        assert!(s.unknown.is_empty(), "{answers:?}: opcodes the VM does not know: {:?}", s.unknown);
        for op in s.handled.keys() {
            assert!(handled.contains(op), "{answers:?}: {op} is handled but not listed");
            seen_handled.insert(*op);
        }
        for op in s.ignored.keys() {
            assert!(ignored.contains(op), "{answers:?}: {op} is skipped but not listed");
        }
        assert_eq!((s.dropped_requests, s.unresolved), (0, 0), "{answers:?}: every request was made and every read answered: {s:?}");
        assert_eq!(s.parked, 0, "{answers:?}: nothing needed parking");
    }
    // The longest way through uses every handled op listed above.
    let missing: Vec<_> = handled.iter().filter(|op| !seen_handled.contains(*op)).collect();
    assert!(missing.is_empty(), "listed but never met: {missing:?}");
    // The stage scripts that place the cast run quietly and meet nothing unknown either.
    let def = proof_scene();
    let p = ScenePlay::new(&f, &def, &StoryState::at(72), 1).unwrap();
    assert!(p.setup_stats().unknown.is_empty(), "{:?}", p.setup_stats().unknown);
    assert!(p.setup_stats().steps < 1_000);
    // Everything about the scene is on the cast: the stage events of entities outside it (the camera, the lights)
    // are counted, not shown.
    let p = play(&f, 72, &[2]);
    assert!(p.stage.iter().any(|s| matches!(s, Stage::Camera(_))), "the camera moves are carried");
}

// ---- 5. what the story carries ------------------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_proof_scene_writes_only_its_mark_once_and_a_scene_that_moves_the_moment_reports_it() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    // The proof scene's script writes the field's own flags (and Tifa's affection points). Of those the story model carries
    // exactly one, the bar's mark (`Var[3][212]` bit 2, `story::BAR_CONVERSATION`): the scene turns it on first, and with it
    // on the story has had the conversation, so the scene is not offered again (acceptance 5). The conversation does not
    // move the story's moment, and the rest is the field's own, said so and not carried.
    let story = StoryState::at(72);
    for answers in [&[2usize][..], &[1][..]] {
        let out = run_to_end(&f, &def, &story, 1, answers).unwrap();
        assert_eq!(out.end, End::Finished);
        assert!(!out.writes.is_story_neutral(), "{:?}", out.writes);
        assert_eq!(out.writes.moment, None, "the conversation does not move the moment");
        let [((3, 212), byte)] = out.writes.tracked[..] else { panic!("exactly the bar's byte is carried: {:?}", out.writes) };
        assert!(byte & 0b100 != 0, "the mark is on: {byte:#b}");
        // (the long way, 'something hard', gives Tifa's affection points a bump: the field's own, never carried)
        assert_eq!(out.writes.untracked.get(&(1, 4)), (answers == [2]).then_some(&5), "Tifa's affection points are the field's own");
        // The scene also changes OTHER bits of the bar's byte, the field's own, which the story does not carry: they are
        // reported as untracked (so a server that saves stories refuses the scene instead of dropping them), and applying
        // the writes keeps the mark alone.
        let other = out.writes.untracked.get(&(3, 212)).copied().expect("the byte's other bits change too");
        assert!(other & !0b100 != 0, "{other:#b}");
        // applied once: the story keeps the mark (and only the mark) and the scene is no longer offered to it
        let mut state = story.clone();
        assert!(def.offered_to(&state));
        assert!(out.writes.apply_to(&mut state));
        assert_eq!(state.var(3, 212), 0b100, "only the mark is kept");
        assert!(!out.writes.apply_to(&mut state), "applied twice it changes nothing");
        assert!(def.begun_in(&state) && !def.offered_to(&state), "a story that has had the conversation is not offered it");
        assert_eq!(StoryState::decode(&state.encode()), Some(state), "and the mark is what the stored blob says");
    }

    // A scene whose script DOES write the moment: Tifa's script 15 (Cloud walks out of the bar, Tifa stops him), which
    // sets the moment 81 -> 84 and then jumps to another field (not performed: the scene ends there).
    let leaving = SceneDef {
        id: "ff7/mds7pb_1/tifa-promise".to_string(),
        game: "ff7".to_string(),
        field: PROOF_FIELD.to_string(),
        trigger: Trigger { entity: TIFA, name: "tifa".to_string(), slot: 1, talk_radius: 70, moments: 81..=81 },
        setup: Vec::new(),
        // Flag 0 of `Var[3][213]` is what turns her script's short call from the door into the long goodbye.
        given: vec![fmv_ff7::script::scene::Given { bank: 3, address: 213, value: 0b0000_0001 }],
        root: vec![Step::Start(ScriptRef::new(TIFA, 15))],
        cast: vec![
            CastMember { entity: CLOUD, name: "cloud".into(), look: Look::Appearance("ff7/cloud".into()), start: None },
            CastMember { entity: TIFA, name: "tifa".into(), look: Look::Appearance("ff7/tifa".into()), start: None },
        ],
        speakers: vec![("Cloud".into(), CLOUD), ("Tifa".into(), TIFA)],
        witness: Vec::new(),
        arrival: fmv_ff7::script::scene::Spot { x: 0, y: 0, triangle: 0, direction: 0 }, presents: None, resume_after_battle: None, presents_bit: None,
    };
    let out = run_to_end(&f, &leaving, &StoryState::at(81), 1, &[]).unwrap();
    assert_eq!(out.end, End::MapJump { field: 294 }, "the scene ends where the script leaves the field");
    assert_eq!(out.writes.moment, Some(84));
    assert!(out.stats.unknown.is_empty() || out.stats.steps < 10_000, "{:?}", out.stats.unknown);
    // applied once: the moment goes to 84, and applying it again (or to a later story) changes nothing
    let mut state = StoryState::at(81);
    assert!(out.writes.apply_to(&mut state));
    assert_eq!(state.moment, 84);
    assert!(!out.writes.apply_to(&mut state));
    let mut later = StoryState::at(96);
    assert!(!out.writes.apply_to(&mut later) && later.moment == 96, "a story never goes back");
    eprintln!("the leaving scene: {} windows, ends {:?}, writes {:?}, unknown {:?}", out.windows, out.end, out.writes.moment, out.stats.unknown);
}

// ---- robustness: every script of the game ------------------------------------------------------

/// Run the VM on EVERY script slot of EVERY field of the game as if it were a scene's root: none may panic or run past its
/// budget, and what the VM does not know is counted by mnemonic (the survey of what a later scene would need).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_vm_ends_every_script_of_the_game_within_its_budget_and_counts_what_it_does_not_know() {
    use fmv_ff7::script::scene::{Limits, SceneVars, Vm};
    use std::collections::BTreeMap;
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    let entries: Vec<(usize, String)> =
        lgp.list().iter().enumerate().filter(|(_, e)| script_common::is_field_name(&e.name)).map(|(i, e)| (i, e.name.clone())).collect();
    let limits = Limits { steps: 4_000, windows: 16, threads: 16, frames: 1_800 };
    let (mut fields, mut runs, mut windows) = (0usize, 0usize, 0usize);
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut ignored: BTreeMap<&'static str, u32> = BTreeMap::new();
    for (index, _name) in &entries {
        let Ok(stored) = lgp.read_index(*index) else { continue };
        let Ok(bytes) = fmv_ff7::lzss::decompress(&stored) else { continue };
        let Ok(field) = FieldFile::parse(bytes) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        fields += 1;
        for entity in 0..script.entity_count() {
            for slot in 1..fmv_ff7::script::SCRIPTS_PER_ENTITY {
                if script.span(entity, slot).is_none() {
                    continue;
                }
                let (Ok(e), Ok(s)) = (u8::try_from(entity), u8::try_from(slot)) else { continue };
                let mut vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, limits);
                vm.begin(vec![Step::Start(ScriptRef::new(e, s))]);
                runs += 1;
                let mut resumes = 0;
                let end = loop {
                    let beat = vm.resume(&script, None);
                    resumes += 1;
                    assert!(resumes <= 200, "a scene that never ends: {_name} entity {entity} slot {slot}");
                    match beat.then {
                        Then::End { how, .. } => break how,
                        Then::Window(_) => windows += 1,
                    }
                };
                let key = match end {
                    End::Finished => "Finished",
                    End::Budget => "Budget",
                    End::Broken => "Broken",
                    End::MapJump { .. } => "MapJump",
                    End::Battle => "Battle",
                    End::MiniGame => "MiniGame",
                    End::GameOver => "GameOver",
                };
                *ends.entry(key.to_string()).or_default() += 1;
                for (op, n) in &vm.stats().unknown {
                    *unknown.entry(op).or_default() += n;
                }
                for (op, n) in &vm.stats().ignored {
                    *ignored.entry(op).or_default() += n;
                }
            }
        }
    }
    eprintln!("{fields} fields, {runs} scripts run as a scene root, {windows} windows handed out; ends: {ends:?}");
    eprintln!("unknown opcodes (count over all runs): {unknown:?}");
    eprintln!("skipped as known to change nothing a scene shows: {} distinct opcodes", ignored.len());
    assert!(fields >= 700 && runs > 20_000, "{fields} fields, {runs} runs");
    assert_eq!(ends.values().sum::<usize>(), runs, "every run ended");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn guard_scorpion_victory_continuation_has_only_the_two_tail_windows_and_saves_nothing() {
    use fmv_ff7::script::scene::{auto_witnesses, guard_scorpion_victory_scene};
    let Some(f) = field("nmkin_5") else { return };
    let def = guard_scorpion_victory_scene();
    let story = StoryState::at(26);
    assert!(!def.offered_to(&story));
    assert!(!def.presents_step("nmkin_5", &story, &StoryState::at(27)));
    assert!(def.follows_battle_step("nmkin_5", &story, &StoryState::at(27), 324));
    assert!(!def.follows_battle_step("nmkin_5", &story, &StoryState::at(27), 323));
    let mut p = ScenePlay::new(&f, &def, &story, 1).unwrap();
    let mut dialogs = Vec::new();
    loop {
        match p.next(None).then {
            Then::Window(w) => {
                let MessageOrigin::Canon { dialog, .. } = w.message.origin else { panic!("unexpected added line") };
                dialogs.push(dialog);
            },
            Then::End { how, .. } => { assert_eq!(how, End::Finished); break; },
        }
    }
    assert_eq!(dialogs, [89, 91]);
    let outcome = run_to_end(&f, &def, &story, 1, &[]).unwrap();
    assert!(outcome.is_complete(), "{outcome:?}");
    assert!(outcome.writes.is_story_neutral(), "{outcome:?}");
    assert_eq!(outcome.questions, 0);
    assert_eq!(auto_witnesses(&f, &def, &story, 1, 3).len(), 3);
}

#[test]
fn jessie_rescue_matches_only_the_existing_flag_turning_on() {
    use fmv_ff7::script::scene::jessie_rescue_scene;
    use fmv_ff7::story::{FREED_BIT, LIFT_BIT, REACTOR_STATE};
    let mut def = jessie_rescue_scene();
    let before = StoryState::at(27);
    let mut after = before.clone();
    assert!(after.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, FREED_BIT, true));
    assert!(def.presents_step("NMKIN_3", &before, &after));
    assert!(!def.starts_on_arrival() && !def.offered_to(&before) && !def.offered_to(&after));
    assert!(!def.presents_step("nmkin_2", &before, &after));
    assert!(!def.presents_step("nmkin_3", &after, &after), "already rescued");
    assert!(!def.presents_step("nmkin_3", &after, &before), "never match a reset");
    assert!(!def.follows_battle_step("nmkin_3", &before, &after, 324));
    let mut lift_only = before.clone();
    lift_only.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, LIFT_BIT, true);
    assert!(!def.presents_step("nmkin_3", &before, &lift_only));
    after.moment = 28;
    assert!(!def.presents_step("nmkin_3", &before, &after), "not a moment step");
    let mut early = StoryState::at(26);
    early.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, FREED_BIT, true);
    assert!(!def.presents_step("nmkin_3", &StoryState::at(26), &early));
    after.moment = 27;
    def.presents_bit = Some((1, 225, 6));
    assert!(!def.presents_step("nmkin_3", &before, &after), "unknown flags never match");
    def = jessie_rescue_scene();
    def.presents = Some(27);
    assert!(!def.presents_step("nmkin_3", &before, &after), "ambiguous metadata never matches");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn jessie_rescue_plays_three_windows_and_matches_the_existing_saved_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{auto_witnesses, jessie_rescue_scene, scene, JESSIE_RESCUE_SCENE_ID, ANNOUNCED_STATE_OPS};
    use fmv_ff7::story::{LIFT_BIT, REACTOR_STATE};
    let f = field("nmkin_3").expect("FMV_FF7_DATA and nmkin_3");
    let def = jessie_rescue_scene();
    assert_eq!(scene(JESSIE_RESCUE_SCENE_ID).as_ref(), Some(&def));
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&data_dir().unwrap()), provenance).unwrap();
    let area = adapter.load_field("nmkin_3").unwrap();
    let mut before = StoryState::at(27);
    before.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, LIFT_BIT, true);
    let step = area.story_advance(&before.encode(), 0).expect("existing rescue point");
    assert_eq!(step.summary, "var1.225=1->33");
    let after = StoryState::decode(&step.progress).unwrap();
    assert!(def.presents_step("nmkin_3", &before, &after));
    assert!(area.story_advance(&step.progress, 0).is_none(), "paid once");
    let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 3, 0));
    assert!(outcome.is_complete(), "{outcome:?}");
    // The bit-clear condition reads only carried bit 5. BITON still audits the whole byte it
    // preserves during read-modify-write; no unknown persistent byte chooses the rescue path.
    assert_eq!(outcome.unknown_persistent_reads.iter().copied().collect::<Vec<_>>(), [(1, 225)]);
    assert!(outcome.party.is_empty() && outcome.jump.is_none());
    assert_eq!(outcome.writes.moment, None);
    assert_eq!(outcome.writes.tracked, [((1, 225), 33)]);
    assert!(outcome.writes.untracked.keys().all(|(bank, _)| !(1..=4).contains(bank)), "no new saved flags");
    assert!([&outcome.setup_stats, &outcome.stats].iter().all(|stats| stats.ignored.keys().all(|op| !ANNOUNCED_STATE_OPS.contains(op))));
    let mut interpreted = before.clone();
    outcome.writes.apply_to(&mut interpreted);
    assert_eq!(interpreted, after, "only the existing point's writes");
    let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
    let mut lines = Vec::new();
    loop {
        match play.next(None).then {
            Then::Window(w) => {
                let MessageOrigin::Canon { dialog, .. } = w.message.origin else { panic!("unexpected added line") };
                lines.push((w.opener, dialog));
            },
            Then::End { how, .. } => { assert_eq!(how, End::Finished); break; },
        }
    }
    assert_eq!(lines, [(3, 66), (4, 67), (4, 68)]);
    assert_eq!(play.off_cast(), 0);
    let spots = auto_witnesses(&f, &def, &before, 1, 3);
    assert!(!spots.is_empty(), "owner has a witness spot");
    eprintln!("Jessie rescue: {} automatic witness spot(s): {spots:?}", spots.len());
    let cast = fmv_ff7::script::scene::cast_places(&f, &def, &before, 1).unwrap();
    let ring = 5.5 * fmv_ff7::default_model_scale(FieldScript::from_field(&f).unwrap().header().scale);
    let mesh = f.walkmesh().unwrap();
    let parts = mesh_parts(&mesh);
    for spot in &spots {
        assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)));
        assert_eq!(parts[usize::from(spot.triangle)], parts[usize::from(def.arrival.triangle)]);
        assert!(cast.iter().all(|&(x, y)| (x - f32::from(spot.x)).hypot(y - f32::from(spot.y)) >= 2.0 * ring + 16.0));
        assert!(scenery_gap(&mesh, f32::from(spot.x), f32::from(spot.y)) >= 1.5 * ring);
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn jessie_ladder_plays_one_window_and_matches_only_the_existing_moment_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{cast_places, jessie_ladder_scene, scene, JESSIE_LADDER_SCENE_ID, ANNOUNCED_STATE_OPS};
    use fmv_ff7::story::{LIFT_BIT, REACTOR_STATE};
    let f = field("nmkin_3").expect("FMV_FF7_DATA and nmkin_3");
    let def = jessie_ladder_scene();
    assert_eq!(scene(JESSIE_LADDER_SCENE_ID).as_ref(), Some(&def));
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&data_dir().unwrap()), provenance).unwrap();
    let area = adapter.load_field("nmkin_3").unwrap();
    let mesh = f.walkmesh().unwrap();
    let parts = mesh_parts(&mesh);
    let ring = 5.5 * fmv_ff7::default_model_scale(FieldScript::from_field(&f).unwrap().header().scale);
    for moment in [0, 1, 13] {
        let mut before = StoryState::at(moment);
        before.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, LIFT_BIT, true);
        let step = area.story_advance(&before.encode(), 0).expect("existing ladder talk point");
        let after = StoryState::decode(&step.progress).unwrap();
        assert!(def.presents_step("nmkin_3", &before, &after));
        assert!(!def.starts_on_arrival() && !def.offered_to(&before));
        assert!(!def.presents_step("nmkin_3", &after, &after));
        assert!(!def.presents_step("nmkin_2", &before, &after));
        assert!(area.story_advance(&step.progress, 0).is_none(), "paid once");
        let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 1, 0));
        assert!(outcome.is_complete(), "{outcome:?}");
        assert!(outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
        assert!(outcome.party.is_empty() && outcome.jump.is_none());
        assert_eq!(outcome.writes.moment, Some(14));
        assert!(outcome.writes.tracked.is_empty());
        assert!(outcome.writes.untracked.keys().all(|(bank, _)| !(1..=4).contains(bank)), "no new saved flags");
        assert!([&outcome.setup_stats, &outcome.stats].iter().all(|stats| stats.ignored.keys().all(|op| !ANNOUNCED_STATE_OPS.contains(op))));
        let mut interpreted = before.clone();
        outcome.writes.apply_to(&mut interpreted);
        assert_eq!(interpreted, after, "only the existing point's writes");
        let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
        let mut lines = Vec::new();
        let mut jumps = Vec::new();
        let mut origin = 0u32;
        let mut windups = Vec::new();
        let mut jump_times = Vec::new();
        loop {
            let beat = play.next(None);
            for event in &beat.stage {
                let at = origin + event.at;
                match event.what {
                    Stage::Anim { entity: 4, animation: 5, how: fmv_ff7::script::scene::AnimHow::Hold,
                        first: Some(0), last: Some(39), speed: 16, divisor: 1 } => windups.push(at),
                    Stage::Move { entity: 4, how: MoveHow::Jump, .. } => jump_times.push(at),
                    _ => {},
                }
            }
            jumps.extend(beat.stage.iter().filter_map(|event| match event.what {
                Stage::Move { entity: 4, x, y, z, triangle, how: MoveHow::Jump, .. } => Some((x, y, z, triangle)), _ => None,
            }));
            match beat.then {
                Then::Window(w) => {
                    origin += w.at;
                    let MessageOrigin::Canon { dialog, .. } = w.message.origin else { panic!("unexpected added line") };
                    lines.push((w.opener, dialog));
                },
                Then::End { how, .. } => { assert_eq!(how, End::Finished); break; },
            }
        }
        assert_eq!(lines, [(4, 72)]);
        assert_eq!((windups.len(), jump_times.len()), (1, 1));
        assert_eq!(jump_times[0].checked_sub(windups[0]), Some(15), "CANM!1 starts asynchronously; only the authored WAIT15 precedes JUMP");
        let landing_height = Ground::new(mesh.clone()).height(242, 1850, Some(37), None).unwrap().round() as i16;
        assert_eq!(jumps, [(242, 1850, Some(landing_height), Some(37))], "owned JUMP preserves its landing triangle and floor");
        assert_eq!(play.off_cast(), 0);
        let cast = cast_places(&f, &def, &before, 1).unwrap();
        for spot in def.witness.iter().chain(std::iter::once(&def.arrival)) {
            assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)));
            assert_eq!(parts[usize::from(spot.triangle)], parts[26], "Jessie's upper platform");
            assert!(cast.iter().all(|&(x, y)| (x - f32::from(spot.x)).hypot(y - f32::from(spot.y)) >= 2.0 * ring + 16.0));
            assert!(scenery_gap(&mesh, f32::from(spot.x), f32::from(spot.y)) >= 1.5 * ring);
        }
    }
    for moment in [14, 26, 27, 33] {
        assert!(!def.applies_at(moment));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn run_ahead_plays_all_three_paths_and_only_the_existing_entry_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{auto_witnesses, cast_places, run_ahead_scene, scene, RUN_AHEAD_SCENE_ID, SKIPPED_STATE_OPS};
    let f = field("md1_2").expect("FMV_FF7_DATA and md1_2");
    let def = run_ahead_scene();
    assert_eq!(scene(RUN_AHEAD_SCENE_ID), Some(def.clone()));
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&data_dir().unwrap()), provenance).unwrap();
    let area = adapter.load_field("md1_2").unwrap();
    let base = area.npcs();
    assert_eq!(base.iter().map(|n| n.entity_name.as_str()).collect::<Vec<_>>(), ["av_b", "av_c", "av_w"]);
    for moment in [0, 1, 7] {
        let before = StoryState::at(moment);
        let step = area.story_advance(&before.encode(), 0).expect("existing entry point");
        let after = StoryState::decode(&step.progress).unwrap();
        assert!(def.presents_step("md1_2", &before, &after));
        assert!(!def.starts_on_arrival() && !def.offered_to(&before));
        assert!(!def.presents_step("md1_1", &before, &after));
        assert!(area.story_advance(&step.progress, 0).is_none(), "entry step is paid once");
        let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 0, 0));
        assert!(outcome.is_complete(), "{outcome:?}");
        assert!(outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
        assert!(outcome.party.is_empty() && outcome.jump.is_none() && outcome.writes.tracked.is_empty());
        assert!(outcome.writes.untracked.keys().all(|(bank, _)| !(1..=4).contains(bank)));
        assert!([&outcome.setup_stats, &outcome.stats].iter().all(|stats| stats.ignored.keys().all(|op| !SKIPPED_STATE_OPS.contains(op))));
        let mut interpreted = before.clone();
        outcome.writes.apply_to(&mut interpreted);
        assert_eq!(interpreted, after, "exact existing step, no new saved state");
        let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
        let beat = play.next(None);
        assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
        for entity in [2, 3, 4] {
            let moves: Vec<_> = beat.stage.iter().filter_map(|event| match event.what {
                Stage::Move { entity: actor, x, y, .. } if actor == entity => Some((event.at, x, y)),
                _ => None,
            }).collect();
            assert_eq!(moves.len(), match entity { 2 => 5, 3 => 2, _ => 3 });
            assert_eq!(moves.last().map(|(_, x, y)| (*x, *y)), Some((3010, 32350)));
            let hidden = beat.stage.iter().find(|event| event.what == Stage::Show { entity, visible: false }).expect("actor leaves");
            assert!(hidden.at >= moves.last().unwrap().0);
        }
        assert_eq!(play.off_cast(), 0);
    }
    let mesh = f.walkmesh().unwrap();
    let parts = mesh_parts(&mesh);
    let story = StoryState::at(7);
    let spots = auto_witnesses(&f, &def, &story, 1, 3);
    assert!(!spots.is_empty(), "the owner has a measured floor position");
    let cast = cast_places(&f, &def, &story, 1).unwrap();
    let ring = 5.5 * fmv_ff7::default_model_scale(FieldScript::from_field(&f).unwrap().header().scale);
    for spot in &spots {
        assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)));
        assert_eq!(parts[usize::from(spot.triangle)], parts[usize::from(def.arrival.triangle)]);
        assert!(cast.iter().all(|&(x,y)| (x-f32::from(spot.x)).hypot(y-f32::from(spot.y)) >= 2.0*ring+16.0));
        assert!(scenery_gap(&mesh, f32::from(spot.x), f32::from(spot.y)) >= 1.5*ring);
    }
    eprintln!("run-ahead: {} automatic witness spots; rendered visibility untested", spots.len());
    for moment in [0, 7, 8, 27, 33, 32767, 32768, u16::MAX] {
        let story = StoryState::at(moment);
        let saved = story.encode();
        let phase = area.phase_key(&saved).unwrap();
        let init = ScenePlay::new(&f, &def, &story, 1).unwrap();
        let mut expected = base.clone();
        for event in init.initial_stage_events() {
            if let Stage::Show { entity, visible } = *event {
                if let Some(npc) = entity.checked_sub(2).and_then(|index| expected.get_mut(usize::from(index))) { npc.unconditional = visible; }
            }
        }
        assert_eq!(area.npcs_in_phase(&phase), expected, "owned init, signed boundary, stable slots at {moment}");
        assert_eq!(area.phase_key(&StoryState::decode(&saved).unwrap().encode()), Some(phase));
        if moment >= 8 { assert!(!def.applies_at(moment)); }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn jessie_descends_the_stacked_floors_and_only_advances_the_existing_entry_step() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform};
    use fmv_ff7::script::scene::{auto_witnesses, cast_places, jessie_descent_scene, scene, JESSIE_DESCENT_SCENE_ID, SKIPPED_STATE_OPS};
    use fmv_ff7::story::{FREED_BIT, LIFT_BIT, REACTOR_STATE, TIMEOUT_BIT};
    let f = field("nmkin_2").expect("FMV_FF7_DATA and nmkin_2");
    let def = jessie_descent_scene();
    assert_eq!(scene(JESSIE_DESCENT_SCENE_ID), Some(def.clone()));
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&data_dir().unwrap()), provenance).unwrap();
    let area = adapter.load_field("nmkin_2").unwrap();
    let base = area.npcs();
    assert_eq!(base.iter().map(|n| n.entity_name.as_str()).collect::<Vec<_>>(), ["av_j"]);
    let mesh = f.walkmesh().unwrap();
    let ground = Ground::new(mesh.clone());
    // These targets overlap the ground floor. Nearest world-zero height would put her on
    // triangles 36 and 5, before the authored path has descended from the upper walkways.
    assert_eq!(ground.height(786, 818, Some(36), None).unwrap().round() as i16, 0);
    assert_eq!(ground.height(794, -128, Some(5), None).unwrap().round() as i16, 1);
    assert_eq!(ground.height(786, 818, Some(173), None).unwrap().round() as i16, 1058);
    assert_eq!(ground.height(794, -128, Some(56), None).unwrap().round() as i16, 554);
    for moment in [0, 1, 12] {
        let before = StoryState::at(moment);
        let step = area.story_advance(&before.encode(), 0).expect("existing entry point");
        let after = StoryState::decode(&step.progress).unwrap();
        assert!(def.presents_step("nmkin_2", &before, &after));
        assert!(!def.starts_on_arrival() && !def.offered_to(&before));
        assert!(!def.presents_step("nmkin_3", &before, &after));
        assert!(area.story_advance(&step.progress, 0).is_none(), "entry step is paid once");
        let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 0, 0));
        assert!(outcome.is_complete(), "{outcome:?}");
        assert!(outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
        assert!(outcome.party.is_empty() && outcome.jump.is_none() && outcome.writes.tracked.is_empty());
        assert_eq!(outcome.writes.moment, Some(13));
        assert!(outcome.writes.untracked.keys().all(|(bank, _)| !(1..=4).contains(bank)));
        assert!([&outcome.setup_stats, &outcome.stats].iter().all(|stats| stats.ignored.keys().all(|op| !SKIPPED_STATE_OPS.contains(op))));
        let mut interpreted = before.clone();
        outcome.writes.apply_to(&mut interpreted);
        assert_eq!(interpreted, after, "exact existing step, no new saved state");
        let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
        assert!(play.initial_stage_events().contains(&Stage::Place { entity: 4, x: -491, y: -53, z: Some(1500), triangle: Some(72) }));
        let beat = play.next(None);
        assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
        let moves: Vec<_> = beat.stage.iter().filter_map(|event| match event.what {
            Stage::Move { entity: 4, x, y, z, .. } => Some((x, y, z)), _ => None,
        }).collect();
        assert_eq!(moves.len(), 5);
        assert_eq!(moves[..4], [(381, -33, Some(1055)), (663, 773, Some(1057)), (786, 818, Some(1058)), (794, -128, Some(554))]);
        assert_eq!(moves[4], (-816, -252, ground.height(-816, -252, None, None).map(|z| z.round() as i16)));
        assert!(moves[4].2.is_some_and(|z| z < 554), "last segment reaches the lower floor");
        assert!(beat.stage.iter().any(|event| event.what == Stage::Show { entity: 4, visible: false }));
        assert_eq!(play.off_cast(), 0);
    }
    let story = StoryState::at(12);
    let spots = auto_witnesses(&f, &def, &story, 1, 3);
    assert!(!spots.is_empty(), "the owner has a measured floor position");
    let cast = cast_places(&f, &def, &story, 1).unwrap();
    let parts = mesh_parts(&mesh);
    let ring = 5.5 * fmv_ff7::default_model_scale(FieldScript::from_field(&f).unwrap().header().scale);
    for spot in spots.iter().chain(std::iter::once(&def.arrival)) {
        assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)));
        assert_eq!(parts[usize::from(spot.triangle)], parts[usize::from(def.arrival.triangle)]);
        assert!(cast.iter().all(|&(x,y)| (x-f32::from(spot.x)).hypot(y-f32::from(spot.y)) >= 2.0*ring+16.0));
        assert!(scenery_gap(&mesh, f32::from(spot.x), f32::from(spot.y)) >= 1.5*ring);
    }
    eprintln!("Jessie descent: five resolved move heights; {} automatic witness spots; rendered movement untested", spots.len());
    for moment in [0, 1, 12, 13, 14, 27, 33, 32767, 32768, u16::MAX] {
        for bits in [false, true] {
            let mut story = StoryState::at(moment);
            for bit in [LIFT_BIT, FREED_BIT, TIMEOUT_BIT] { story.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, bit, bits); }
            let saved = story.encode();
            let phase = area.phase_key(&saved).unwrap();
            let init = ScenePlay::new(&f, &def, &story, 1).unwrap();
            let mut expected = base.clone();
            for event in init.initial_stage_events() {
                if let Stage::Show { entity: 4, visible } = *event { expected[0].unconditional = visible; }
            }
            assert_eq!(area.npcs_in_phase(&phase), expected, "owned init, signed boundary, stable slot at {moment}; bits={bits}");
            assert_eq!(area.phase_key(&StoryState::decode(&saved).unwrap().encode()), Some(phase));
            if moment >= 13 { assert!(!def.applies_at(moment)); }
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn reactor_approach_plays_the_crew_paths_and_keeps_wedge_outside() {
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{EditionProvenance, GameId, Platform, Pos};
    use fmv_ff7::script::scene::{auto_witnesses, cast_places, reactor_approach_scene, scene, AnimHow, REACTOR_APPROACH_SCENE_ID, SKIPPED_STATE_OPS};
    let f = field("nrthmk").expect("FMV_FF7_DATA and nrthmk");
    let def = reactor_approach_scene();
    assert_eq!(scene(REACTOR_APPROACH_SCENE_ID), Some(def.clone()));
    assert!(def.given.is_empty(), "no assumed persistent bytes");
    let script = FieldScript::from_field(&f).unwrap();
    assert_eq!(script.header().scale, 512);
    let wedge_script = script.disassemble(5, 0).unwrap();
    let run = wedge_script.iter().position(|ins| ins.opcode == 0xA8 && ins.literal("x") == Some(-893) && ins.literal("y") == Some(-3038)).unwrap();
    let speed = wedge_script[..run].iter().rev().find(|ins| ins.mnemonic == "MSPED").unwrap().literal("speed");
    assert_eq!(speed, Some(3840), "Wedge's authored MOVE uses the run speed at field scale 512");
    let provenance = EditionProvenance { game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(), language: "en".into(), revision: "test".into(), sources: vec![], modded: false };
    let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(flevel_path(&data_dir().unwrap()), provenance).unwrap();
    let area = adapter.load_field("nrthmk").unwrap();
    let base = area.npcs();
    assert_eq!(base.iter().map(|n| n.entity_name.as_str()).collect::<Vec<_>>(), ["av_b", "av_c", "av_w"]);
    let from = adapter.load_field("md1_2").unwrap();
    let entry = from.exits().into_iter().find(|exit| exit.target == "nrthmk").unwrap();
    assert_eq!((entry.arrival.x, entry.arrival.y, entry.arrival_hint), (f32::from(def.arrival.x), f32::from(def.arrival.y), Some(u32::from(def.arrival.triangle))));
    for moment in [0, 1, 8, 9] {
        let before = StoryState::at(moment);
        let step = area.story_advance(&before.encode(), 0).expect("existing entry point");
        let after = StoryState::decode(&step.progress).unwrap();
        assert!(def.presents_step("nrthmk", &before, &after));
        assert!(!def.starts_on_arrival() && !def.offered_to(&before));
        assert!(!def.presents_step("md1_2", &before, &after));
        assert!(area.story_advance(&step.progress, 0).is_none(), "entry step is paid once");
        let outcome = run_to_end(&f, &def, &before, 1, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 0, 0));
        assert!(outcome.is_complete(), "{outcome:?}");
        assert!(outcome.unknown_persistent_reads.is_empty(), "only known bits choose the path: {outcome:?}");
        assert!(outcome.party.is_empty() && outcome.jump.is_none() && outcome.writes.tracked.is_empty());
        assert_eq!(outcome.writes.moment, Some(10));
        assert!(outcome.writes.untracked.keys().all(|(bank, _)| !(1..=4).contains(bank)));
        assert!([&outcome.setup_stats, &outcome.stats].iter().all(|stats| stats.ignored.keys().all(|op| !SKIPPED_STATE_OPS.contains(op))));
        let mut interpreted = before.clone();
        outcome.writes.apply_to(&mut interpreted);
        assert_eq!(interpreted, after, "only the existing step is saved");
        let mut play = ScenePlay::new(&f, &def, &before, 1).unwrap();
        assert_eq!(play.animation_clip(5, 2), Some("aaga"), "Wedge's authored running slot resolves through his loader model");
        assert_eq!(play.animation_clip(5, 3), Some("bxja"), "Wedge's authored slot resolves through his loader model");
        assert_eq!(play.animation_clip(5, u8::MAX), None);
        let beat = play.next(None);
        assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
        for (entity, start, path) in [
            (3, (192, -2990), vec![(-28, -2870), (15, -811)]),
            (4, (365, -3089), vec![(-19, -2945), (15, -811)]),
            (5, (578, -2971), vec![(-893, -3038)]),
        ] {
            assert!(play.initial_stage_events().iter().chain(beat.stage.iter().map(|event| &event.what)).any(|event|
                matches!(event, Stage::Place { entity: actor, x, y, .. } if *actor == entity && (*x, *y) == start)), "authored start for {entity}");
            let moves: Vec<_> = beat.stage.iter().filter_map(|event| match event.what {
                Stage::Move { entity: actor, x, y, .. } if actor == entity => Some((x, y)), _ => None,
            }).collect();
            assert_eq!(moves, path);
            let hides = beat.stage.iter().filter(|event| event.what == Stage::Show { entity, visible: false }).count();
            assert_eq!(hides, usize::from(entity != 5), "only Biggs and Jessie leave");
        }
        let wedge_animations: Vec<_> = beat.stage.iter().filter_map(|event| match event.what {
            Stage::Move { entity: 5, animation, .. } => Some(animation), _ => None,
        }).collect();
        assert_eq!(wedge_animations, [Some(2)], "MSPED 3840 exceeds three times scale 512, so ordinary MOVE selects the running slot");
        assert!(beat.stage.iter().any(|event| matches!(event.what, Stage::Anim {
            entity: 5, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1,
        })));
        assert_eq!(play.off_cast(), 0);
    }
    let mesh = f.walkmesh().unwrap();
    let parts = mesh_parts(&mesh);
    let story = StoryState::at(8);
    let spots = auto_witnesses(&f, &def, &story, 1, 3);
    let cast = cast_places(&f, &def, &story, 1).unwrap();
    let ring = 5.5 * fmv_ff7::default_model_scale(FieldScript::from_field(&f).unwrap().header().scale);
    assert!(!spots.is_empty());
    assert!(Ground::new(mesh.clone()).height(15, -811, None, None).is_none(), "the authored exit endpoint is offmesh, never a witness position");
    for spot in &spots {
        assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)));
        assert_eq!(parts[usize::from(spot.triangle)], parts[usize::from(def.arrival.triangle)]);
        assert!(cast.iter().all(|&(x,y)| (x-f32::from(spot.x)).hypot(y-f32::from(spot.y)) >= 2.0*ring+16.0));
        assert!(scenery_gap(&mesh, f32::from(spot.x), f32::from(spot.y)) >= 1.5*ring);
    }
    let loader = f.model_loader().unwrap().models.into_iter().find(|model| model.hrc_stem() == "bxbe").unwrap();
    let idle = loader.animations[3].stem();
    assert_eq!(idle, "bxja");
    let run = loader.animations[2].stem();
    assert_eq!(run, "aaga");
    let mut characters = LgpArchive::open_path(data_dir().unwrap().join("field/char.lgp")).unwrap();
    let clip = fmv_ff7::load_field_animation(&mut characters, &idle).unwrap();
    assert_eq!((clip.frame_count(), clip.bone_count()), (80, 21));
    let model = fmv_ff7::load_field_model(&mut characters, "bxbe").unwrap();
    assert!(model.fits(&clip), "the owned loop fits Wedge's retained skeleton");
    let run_clip = fmv_ff7::load_field_animation(&mut characters, &run).unwrap();
    assert_eq!((run_clip.frame_count(), run_clip.bone_count()), (15, 21));
    assert!(model.fits(&run_clip), "the owned run clip fits Wedge's retained skeleton");
    for moment in [0, 8, 9, 10, 12, 26, 27, 33, 32767, 32768, u16::MAX] {
        let story = StoryState::at(moment);
        let saved = story.encode();
        let phase = area.phase_key(&saved).unwrap();
        let mut expected = base.clone();
        if (10..27).contains(&moment) {
            expected[0].unconditional = false;
            expected[1].unconditional = false;
            expected[2].pos = Pos::new(-893.0, -3038.0, 491.0);
            expected[2].facing = fmv_ff7::field_direction_to_facing(176);
            expected[2].appearance.idle_animation = Some(idle.clone());
            let init = ScenePlay::new(&f, &def, &story, 1).unwrap();
            for entity in [3, 4] { assert!(init.initial_stage_events().contains(&Stage::Show { entity, visible: false })); }
            assert!(init.initial_stage_events().contains(&Stage::Place { entity: 5, x: -893, y: -3038, z: Some(491), triangle: Some(2) }));
            assert_eq!(init.animation_clip(5, 3), Some("bxja"));
            assert!(init.initial_stage_events().iter().any(|event| matches!(event, Stage::Anim {
                entity: 5, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1,
            })), "quiet setup retains Wedge's authored default loop");
        }
        assert_eq!(area.npcs_in_phase(&phase), expected, "stable slots and inbound phase only at {moment}");
        assert_eq!(area.phase_key(&StoryState::decode(&saved).unwrap().encode()), Some(phase));
        if moment >= 10 { assert!(!def.applies_at(moment)); }
    }
    eprintln!("reactor approach: three actor paths, {} automatic witness spots, inbound NPC phase; rendering untested", spots.len());
}
