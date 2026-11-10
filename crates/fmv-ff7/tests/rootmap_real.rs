//! ROOTMAP's authored lesson and alternate departure, read from the owner's FF7 data.
//!
//! These ignored tests record only structural facts, never dialogue text or assets. Run with
//! `FMV_FF7_DATA=<data> cargo test -p fmv-ff7 --test rootmap_real -- --ignored`.
//! They establish script/state coverage, not rendered palette, model-offset or movie parity.

mod script_common;

use fmv_adapter_api::AreaRuntime;
use fmv_core::{EditionProvenance, GameId, Platform, Pos};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{
    run_to_end_with_resources_strict, train_map_scene, Look, Outcome, Resources, SceneBoundary,
    SceneJump, ScenePlay, Stage, Then, WindowAck,
};
use fmv_ff7::script::{triangle_contains_xy, FieldScript};
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use script_common::{data_dir, flevel_path, load_field};

fn field() -> Option<FieldFile> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let mut archive = LgpArchive::open_path(flevel_path(&dir)).expect("owned field archive");
    Some(load_field(&mut archive, "rootmap").expect("owned ROOTMAP field"))
}

fn story(bit4: Option<bool>) -> StoryState {
    let mut story = StoryState::at(51).into_extended().unwrap();
    // Jessie has invited the player. This does not establish the independent producer bit.
    story.write_masked(3, 223, 1 << 2, 1 << 2).unwrap();
    if let Some(set) = bit4 {
        story.write_masked(3, 223, if set { 1 << 4 } else { 0 }, 1 << 4).unwrap();
    }
    story
}

fn assert_neutral(outcome: &Outcome, before: &StoryState) {
    assert!(outcome.is_complete(), "{outcome:?}");
    assert!(outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
    assert!(outcome.writes.is_story_neutral() && outcome.party.is_empty(), "{outcome:?}");
    assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(600), Some(600)));
    assert!(outcome.resources.gil_ops.is_empty());
    let mut after = before.clone();
    assert!(outcome.writes.apply_extended_to(&mut after, before).is_some());
    assert_eq!(after.encode(), before.encode(), "scratch animation state does not create a durable lesson marker");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_known_clear_train_map_lesson_has_eight_windows_and_its_exact_return() {
    let Some(field) = field() else { return };
    let def = train_map_scene();
    assert_eq!((def.id.as_str(), def.field.as_str()), ("ff7/rootmap/train-map", "rootmap"));
    let before = story(Some(false));
    let resources = Resources { gil: Some(600) };
    let outcome = run_to_end_with_resources_strict(&field, &def, &before, 1, resources, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::MapJump { field: 139 }, 8, 0));
    assert_eq!(outcome.jump, Some(SceneJump { field: 139, x: 39, y: -325, triangle: 49, direction: 28 }));
    assert_neutral(&outcome, &before);

    let mut play = ScenePlay::with_resources(&field, &def, &before, 1, resources).unwrap();
    let mut events = play.initial_stage_events().to_vec();
    let mut dialogs = Vec::new();
    let mut ack = None;
    loop {
        let beat = play.next_strict(ack.take()).unwrap();
        events.extend(beat.stage.into_iter().map(|event| event.what));
        match beat.then {
            Then::Window(window) => {
                assert!(window.message.choice.is_none(), "the mandatory lesson contains no question");
                let MessageOrigin::Canon { dialog, .. } = window.message.origin else { panic!("original dialogue") };
                dialogs.push((window.opener, dialog));
                let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("window checkpoint") };
                ack = Some(WindowAck { checkpoint, answer: None });
                assert!(dialogs.len() <= 8);
            }
            Then::End { how, .. } => {
                assert_eq!(how, End::MapJump { field: 139 });
                break;
            }
        }
    }
    assert_eq!(dialogs, [2, 3, 4, 5, 6, 22, 23, 24].map(|dialog| (13, dialog)));
    assert_eq!(play.off_cast(), 0);
    for entity in [12, 13] {
        assert!(events.contains(&Stage::Place { entity, x: -18, y: -459, z: Some(-56), triangle: Some(53) }),
            "both models have an authored base placement; OFST is additional presentation state");
        assert!(events.contains(&Stage::Show { entity, visible: false }));
        assert!(events.contains(&Stage::Show { entity, visible: true }));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_known_set_train_map_producer_takes_movie_24_to_the_station_without_a_lesson() {
    let Some(field) = field() else { return };
    let def = train_map_scene();
    let before = story(Some(true));
    let outcome = run_to_end_with_resources_strict(&field, &def, &before, 1, Resources { gil: Some(600) }, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::MapJump { field: 146 }, 0, 0));
    assert_eq!(outcome.jump, Some(SceneJump { field: 146, x: -2891, y: 3392, triangle: 26, direction: 0 }));
    assert_neutral(&outcome, &before);

    // Assert what the owned path requests, independently of whether movie rendering is implemented.
    let script = FieldScript::from_field(&field).unwrap();
    let code = script.disassemble(5, 1).unwrap();
    let movie = code.iter().find(|ins| ins.offset == 2315).unwrap();
    assert_eq!((movie.mnemonic, movie.literal("movie")), ("PMVIE", Some(24)));
    assert_eq!(code.iter().find(|ins| ins.offset == 2320).unwrap().mnemonic, "MOVIE");
    assert_eq!(code.iter().find(|ins| ins.offset == 2335).unwrap().mnemonic, "MAPJUMP");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn an_unknown_train_map_selector_remains_unknown_at_the_first_durable_boundary() {
    let Some(field) = field() else { return };
    let def = train_map_scene();
    let before = story(None);
    assert_eq!(before.known_bit(3, 223, 2), Some(true));
    assert_eq!(before.known_bit(3, 223, 4), None);
    assert!(!def.offered_to(&before), "an unknown selector cannot offer either branch");
    let mut play = ScenePlay::with_resources(&field, &def, &before, 1, Resources { gil: Some(600) }).unwrap();
    play.next_strict(None).unwrap();
    let snapshot = play.snapshot().expect("first durable boundary");
    // The interpreter records an unknown input; the authoritative host must refuse this snapshot.
    // Strict window acknowledgments alone are not evidence that the branch input was known.
    assert_eq!(snapshot.unknown_persistent_reads, [(3, 223)].into_iter().collect());
    assert!(snapshot.writes.is_story_neutral());
    let prepared = def.prepare_story(&before).unwrap();
    assert_eq!(prepared.known_bit(3, 223, 4), None, "preparing the scene cannot supply a false bit");
    assert_eq!(prepared.encode(), before.encode());
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_train_map_uses_its_own_models_offsets_palette_programs_and_branch_guard() {
    let Some(field) = field() else { return };
    let def = train_map_scene();
    let loader = field.model_loader().unwrap();
    assert_eq!(loader.models.len(), 2);
    for (index, entity, hrc, idle) in [(0, 12, "clgd", "cmda"), (1, 13, "cmde", "cmhb")] {
        let Look::Model { hrc: actual_hrc, idle: actual_idle } = &def.member(entity).unwrap().look else { panic!("authored model") };
        assert_eq!((actual_hrc.as_str(), actual_idle.as_str()), (hrc, idle));
        assert_eq!(loader.models[index].hrc_stem(), hrc);
        assert_eq!(loader.models[index].animations.first().map(|animation| animation.stem()).as_deref(), Some(idle));
    }
    let script = FieldScript::from_field(&field).unwrap();
    for (entity, offset, xyz) in [(12, 2716, [590, -38, -1316]), (13, 2963, [636, 308, -1204])] {
        let code = script.disassemble(entity, 3).unwrap();
        let ins = code.iter().find(|ins| ins.offset == offset).unwrap();
        assert_eq!((ins.mnemonic, ins.literal("move_type"), ins.literal("speed")), ("OFST", Some(0), Some(0)));
        assert_eq!(["x", "y", "z"].map(|name| ins.literal(name)), xyz.map(Some));
    }
    let producer = script.disassemble(4, 0).unwrap();
    let guard = producer.iter().find(|ins| ins.offset == 2269).unwrap();
    assert_eq!((guard.mnemonic, guard.operand("left").map(|operand| (operand.bank, operand.value)),
        guard.literal("right"), guard.literal("op")), ("IFUB", Some((3, 223)), Some(4), Some(9)));
    for (entity, slot, required) in [
        (0, 3, &["STPAL", "MPPAL2", "CPPAL", "LDPAL", "BGON"][..]),
        (2, 0, &["STPAL", "MPPAL2", "CPPAL", "LDPAL"][..]),
        (3, 0, &["STPAL", "MPPAL2", "CPPAL", "LDPAL"][..]),
        (6, 3, &["FADE", "FADEW", "REQSW", "REQEW"][..]),
        (8, 0, &["SCR2D", "SHAKE"][..]),
    ] {
        let code = script.disassemble(entity, slot).unwrap();
        for name in required {
            assert!(code.iter().any(|ins| ins.mnemonic == *name), "owned entity {entity} slot {slot} requires {name}");
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_authored_train_map_base_is_on_triangle_four_despite_the_scripts_triangle_53() {
    let Some(dir) = data_dir() else { return };
    let provenance = EditionProvenance {
        game: GameId::Ff7, platform: Platform::Pc, edition: "steam".into(), region: "any".into(),
        language: "en".into(), revision: "owned-test".into(), sources: vec![], modded: false,
    };
    let adapter = Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap();
    let area = adapter.load_field("rootmap").unwrap();
    let mesh = area.walkmesh();
    assert_eq!(mesh.triangles.len(), 5, "the script's triangle 53 is not a ROOTMAP triangle");
    let base = Pos::new(-18.0, -459.0, -56.0);
    let arrival = train_map_scene().arrival;
    assert_eq!((arrival.x, arrival.y, arrival.triangle, arrival.direction), (-18, -459, 4, 64));
    let containing: Vec<_> = mesh.triangles.iter().enumerate()
        .filter(|(_, triangle)| triangle_contains_xy(triangle, -18, -459)).map(|(index, _)| index).collect();
    assert_eq!(containing, [4], "preserve the authored coordinates and derive their actual floor hint");
    for hint in [Some(4), Some(53), None] {
        assert_eq!(area.place_arrival(base, hint), base);
        assert_eq!(area.place_arrival(Pos::new(base.x, base.y, 0.0), hint), base);
    }
    assert_eq!(area.validate_move(base, base).pos(), base);
    assert_ne!(area.place_arrival(Pos::new(0.0, 0.0, 0.0), Some(0)), base,
        "the incoming script's zero placeholder must not replace its authored model base");
}
