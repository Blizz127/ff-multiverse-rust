//! Original passenger-train entry through the checked entry constructor and strict replay.
//! Reads owned field assets in place. No test creates a saved visit or authorizes gameplay.
//! `cargo test -p fmv-ff7 --test train_arrival_real -- --ignored --nocapture`

mod script_common;

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    replay_scene, replay_train_arrival, train_arrival_ready, train_scene, CameraOp, SceneBoundary,
    SceneError, SceneInput, SceneJump, ScenePlay, SceneResources, Stage, Then, TrainArrival, WindowAck,
};
use fmv_ff7::script::scene::train_arrival::codec::{encode_train_arrival, replay_train_arrival_record};
use fmv_ff7::script::{FieldScratch, FieldScript};
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

const RESOURCES: SceneResources = SceneResources { gil: Some(240) };

fn fields() -> (FieldFile, FieldFile) {
    let path = std::env::var_os("FMV_FF7_FLEVEL").map(std::path::PathBuf::from)
        .unwrap_or_else(|| flevel_path(&data_dir().expect("FMV_FF7_DATA or FMV_FF7_FLEVEL must name the owned field archive")));
    let mut archive = LgpArchive::open_path(path).unwrap();
    (load_field(&mut archive, "tin_1").unwrap(), load_field(&mut archive, "cargoin").unwrap())
}

fn landing() -> SceneJump {
    SceneJump { field: 139, x: -2, y: 608, triangle: 29, direction: 252 }
}

fn story(partial: bool) -> StoryState {
    // A sparse extended snapshot does not pretend any train flag or affection is known.
    let mut story = StoryState::decode(&[3, 48, 0, 0, 0]).unwrap();
    if partial {
        story.write_masked(13, 101, 0x80, 0x80).unwrap();
        story.write_masked(3, 223, 0x10, 0x10).unwrap();
    }
    assert_eq!(story.known_bit(3, 223, 0), None);
    assert_eq!(story.known_bit(3, 223, 2), None);
    story
}

fn assert_source(field: &FieldFile, cargo: &FieldFile) {
    let cargo = FieldScript::from_field(cargo).unwrap();
    let exit = cargo.disassemble(10, 2).unwrap();
    let exit = exit.iter().find(|instruction| instruction.offset == 0x0682).unwrap();
    assert_eq!((exit.mnemonic, ["field", "x", "y", "triangle", "direction"].map(|name| exit.literal(name))),
        ("MAPJUMP", [Some(139), Some(-2), Some(608), Some(29), Some(252)]));

    let script = FieldScript::from_field(field).unwrap();
    for (entity, offset, address, value) in [(2, 0x0C50, 0, 0), (3, 0x0C64, 0, 0),
        (4, 0x0C74, 0, 0), (8, 0x0D4C, 6, 10), (8, 0x0D50, 4, 0)] {
        let code = script.disassemble(entity, 0).unwrap();
        let instruction = code.iter().find(|instruction| instruction.offset == offset).unwrap();
        assert_eq!((instruction.mnemonic, instruction.operand("var").map(|var| (var.bank, var.value)), instruction.literal("value")),
            ("SETBYTE", Some((5, address)), Some(value)));
    }
    let cloud = script.disassemble(30, 3).unwrap();
    let pose = cloud.iter().find(|instruction| instruction.offset == 0x1512).unwrap();
    assert_eq!((pose.mnemonic, ["x", "y", "z", "triangle"].map(|name| pose.literal(name))),
        ("XYZI", [Some(2), Some(537), Some(-53), Some(28)]));
    let moment = cloud.iter().find(|instruction| instruction.offset == 0x1537).unwrap();
    assert_eq!((moment.mnemonic, moment.operand("var").map(|var| (var.bank, var.value)), moment.literal("value")),
        ("SETWORD", Some((2, 0)), Some(51)));
}

/// Keep the stage's actual times, including the final work after Cloud first appears.
fn finish(mut play: ScenePlay) -> (ScenePlay, Vec<(u32, Stage)>, u32) {
    let mut events = play.initial_stage_events().iter().cloned().map(|event| (0, event)).collect::<Vec<_>>();
    let mut origin = 0;
    let mut ack = None;
    let mut windows = Vec::new();
    loop {
        let beat = play.next_strict(ack.take()).unwrap();
        events.extend(beat.stage.into_iter().map(|event| (origin + event.at, event.what)));
        match beat.then {
            Then::Window(window) => {
                let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("a window checkpoint") };
                windows.push((checkpoint.entity, checkpoint.instruction_offset, checkpoint.dialog, checkpoint.window, checkpoint.choices));
                assert!(windows.len() <= 2);
                assert!(train_arrival_ready(&play).is_err(), "an unacknowledged window is not a ready field entry");
                origin += window.at;
                ack = Some(WindowAck { checkpoint, answer: None });
            }
            Then::End { at, how } => {
                assert_eq!(how, End::Finished);
                assert_eq!(windows, [(17, Some(0x1066), Some(32), 0, None), (37, Some(0x2874), Some(39), 0, None)]);
                return (play, events, origin + at);
            }
        }
    }
}

#[test]
#[ignore = "needs original FF7 data; checked entry acceptance remains separate from runtime admission"]
fn owned_train_entry_preserves_initialization_terminal_scratch_and_the_complete_actor_tail() {
    let (field, cargo) = fields();
    assert_source(&field, &cargo);
    for (seed, partial) in [(1, false), (17, true)] {
        let original = story(partial);
        let before = original.encode();
        let play = TrainArrival::new(&field, &original, seed, RESOURCES, landing()).unwrap().into_play();
        let entry = play.train_entry().expect("the constructor retains its actual entry evidence");
        assert_eq!(entry.original_story().encode(), before);
        assert_eq!(entry.resources().gil, RESOURCES.gil);
        assert_eq!((entry.landing().field, entry.landing().x, entry.landing().y, entry.landing().triangle, entry.landing().direction),
            (139, -2, 608, 29, 252));
        assert_eq!(entry.initial_scratch(), &FieldScratch::cleared_for_entry());
        let mut initialized = FieldScratch::cleared_for_entry();
        initialized.write_u8(5, 6, 10).unwrap();
        assert_eq!(entry.initialized_scratch(), &initialized, "all actual initializers ran; only byte6 changes its value");
        let entry_source = entry.source_identity();
        assert!(train_arrival_ready(&play).is_err());
        assert_eq!(play.ground().height(-2, 608, Some(29), None), Some(-53.0));
        assert_eq!(play.ground().height(2, 537, Some(28), None), Some(-53.0));

        let (play, events, ended_at) = finish(play);
        let snapshot = play.snapshot().unwrap();
        for stats in [&snapshot.setup_stats, &snapshot.stats] {
            assert!(!stats.setup_incomplete && stats.unknown.is_empty() && stats.unresolved == 0
                && stats.dropped_requests == 0, "{stats:?}");
        }
        assert!(snapshot.unknown_persistent_reads.is_empty() && snapshot.party.is_empty() && snapshot.jump.is_none());
        assert!(snapshot.resources.gil_ops.is_empty());
        assert_eq!((snapshot.resources.gil_before, snapshot.resources.gil_after), (Some(240), Some(240)));
        assert!(snapshot.writes.untracked.keys().all(|(bank, _)| !matches!(bank, 5 | 6)),
            "entry scratch is never a legacy or persistent write summary");
        let ready = train_arrival_ready(&play).unwrap();
        let mut expected = original.clone();
        expected.moment = 51;
        assert_eq!(ready.story().encode(), expected.encode(), "only moment changes; all unknown and partial bytes survive");
        assert_eq!(original.encode(), before, "entry construction never mutates its caller's save");
        assert_eq!(ready.story().known_bit(3, 223, 0), None);
        assert_eq!(ready.story().known_bit(3, 223, 2), None);
        let effect = ready.scratch();
        assert_eq!(effect.before(), &FieldScratch::cleared_for_entry(), "initialization stays in the cumulative preimage");
        assert_eq!(effect.assigned_masks().iter().enumerate().filter(|(_, mask)| **mask != 0)
            .map(|(address, &mask)| (address, mask)).collect::<Vec<_>>(),
            [(0, 255), (1, 255), (3, 255), (4, 255), (6, 255), (18, 255), (25, 255)]);
        assert_eq!(effect.after().known_masks(), &[255; 256]);
        for (address, value) in [(0, 0), (1, 1), (4, 0), (6, 10), (30, 0), (31, 0)] {
            assert_eq!(effect.after().read_u8(5, address), Ok(value));
        }
        assert_eq!((effect.assigned_masks()[30], effect.assigned_masks()[31]), (0, 0),
            "later lesson/once-per-visit bytes are known only because this was an accepted entry clear");
        assert!(effect.after().read_u8(5, 3).unwrap() < 5);
        assert!([10, 15, 20].contains(&effect.after().read_u8(5, 25).unwrap()));
        let mut scratch = FieldScratch::cleared_for_entry();
        assert_eq!(effect.apply_to(&mut scratch, &FieldScratch::cleared_for_entry()), Ok(true));
        assert_eq!(&scratch, effect.after());
        assert_eq!((ready.pose().x, ready.pose().y, ready.pose().z, ready.pose().triangle, ready.pose().direction),
            (2, 537, -53, 28, 0));
        assert_eq!(ready.transcript().len(), 2);
        assert_eq!(ready.identity(), play.replay_identity());
        assert_eq!(play.train_entry().unwrap().source_identity(), entry_source);

        let cloud_at = events.iter().find_map(|(at, event)| matches!(event,
            Stage::Place { entity: 30, x: 2, y: 537, z: Some(-53), triangle: Some(28) }).then_some(*at)).unwrap();
        for (entity, x, y) in [(34, 110, -337), (33, -95, -292), (35, 85, -292)] {
            let (at, frames) = events.iter().find_map(|(at, event)| match event {
                Stage::Move { entity: actor, x: to_x, y: to_y, frames, .. }
                    if *actor == entity && *to_x == x && *to_y == y => Some((*at, *frames)),
                _ => None,
            }).expect("each actor finishes the authored tail after Cloud appears");
            assert!(at > cloud_at && ended_at >= at + frames);
        }
        assert!(events.iter().any(|(at, event)| *at > cloud_at && matches!(event,
            Stage::Anim { entity: 35, animation: 4, how: fmv_ff7::script::scene::AnimHow::Once, .. })),
            "Wedge reaches the authored standing-loop tail before the port marks the arrival complete");
        assert_eq!(events.iter().rev().find_map(|(_, event)| match event { Stage::Camera(camera) => Some(*camera), _ => None }),
            Some(CameraOp::Scroll { x: 18, y: 2, frames: 0 }));
        let effects = events.iter().filter(|(_, event)| matches!(event, Stage::Shake { .. } | Stage::Fade(_))).collect::<Vec<_>>();
        eprintln!("checked train seed={seed}: terminal frame={ended_at}, final effects={:?}",
            &effects[effects.len().saturating_sub(6)..]);

        let rebuilt = replay_train_arrival(&field, &original, RESOURCES, landing(), ready.identity(), ready.transcript()).unwrap();
        assert_eq!(rebuilt.snapshot(), play.snapshot(), "replay retains exact terminal writes, scratch, resources and audits");
        let rebuilt_ready = train_arrival_ready(&rebuilt).unwrap();
        assert_eq!(rebuilt_ready.story(), ready.story());
        assert_eq!(rebuilt_ready.scratch(), ready.scratch());
        assert_eq!(rebuilt_ready.transcript(), ready.transcript());

        let record = encode_train_arrival(&play).unwrap();
        let retained = replay_train_arrival_record(&field, &record).unwrap();
        assert_eq!(retained.snapshot(), play.snapshot());
        assert_eq!(retained.train_entry().unwrap().original_story().encode(), before,
            "the retained record binds the original story, not the ready moment51 afterimage");
        assert_eq!(train_arrival_ready(&retained).unwrap(), ready);
        assert_eq!(encode_train_arrival(&retained).unwrap(), record);
        // A checksum is not source/semantic authorization. Re-sign altered metadata so
        // these refusals exercise the interpreter's actual source, landing and ACK checks.
        for (label, offset) in [("source", 10), ("fingerprint", 42),
            ("landing field", 88 + before.len()), ("first ACK ordinal", 98 + before.len())] {
            let mut changed = record.clone();
            changed[offset] ^= 1;
            let body = changed.len() - 32;
            let mut hash = blake3::Hasher::new();
            hash.update(b"fmv-ff7/train-arrival-record\0v1\0");
            hash.update(&changed[..body]);
            changed[body..].copy_from_slice(hash.finalize().as_bytes());
            assert!(replay_train_arrival_record(&field, &changed).is_err(),
                "valid checksum must not authorize changed {label}");
        }
        let mut corrupt = record.clone();
        let middle = corrupt.len() / 2;
        corrupt[middle] ^= 1;
        assert!(replay_train_arrival_record(&field, &corrupt).is_err());
        assert!(replay_train_arrival_record(&field, &record[..record.len() - 1]).is_err());
        assert!(replay_train_arrival_record(&cargo, &record).is_err(), "the source field must match");
    }
}

#[test]
#[ignore = "needs original FF7 data; malformed entry inputs never create ready evidence"]
fn owned_train_entry_refuses_guessed_landings_wrong_moments_and_unchecked_replay_modes() {
    let (field, cargo) = fields();
    let original = story(false);
    for wrong in [SceneJump { field: 138, ..landing() }, SceneJump { x: -1, ..landing() },
        SceneJump { y: 607, ..landing() }, SceneJump { triangle: 28, ..landing() },
        SceneJump { direction: 0, ..landing() }] {
        assert!(TrainArrival::new(&field, &original, 1, RESOURCES, wrong).is_err());
    }
    for moment in [0, 47, 49, 51, u16::MAX] {
        let mut wrong = original.clone();
        wrong.moment = moment;
        assert!(TrainArrival::new(&field, &wrong, 1, RESOURCES, landing()).is_err());
    }
    assert!(TrainArrival::new(&field, &StoryState::at(48), 1, RESOURCES, landing()).is_err(),
        "legacy promotion is not evidence of an accepted checked entry");
    assert!(TrainArrival::new(&field, &original, 1, SceneResources { gil: None }, landing()).is_err());
    assert!(TrainArrival::new(&cargo, &original, 1, RESOURCES, landing()).is_err());

    let (entry, _, _) = finish(TrainArrival::new(&field, &original, 1, RESOURCES, landing()).unwrap().into_play());
    let ready = train_arrival_ready(&entry).unwrap();
    let def = train_scene();
    let legacy = ScenePlay::with_resources(&field, &def, &original, 1, RESOURCES).unwrap();
    assert_ne!(legacy.replay_identity(), ready.identity());
    assert!(legacy.train_entry().is_none());
    assert!(train_arrival_ready(&legacy).is_err());
    assert!(replay_train_arrival(&field, &original, RESOURCES, landing(), legacy.replay_identity(), ready.transcript()).is_err());
    assert!(replay_scene(&field, &def, &original, RESOURCES, ready.identity(), ready.transcript()).is_err());
    let existing = ScenePlay::with_scratch(&field, &def, &original, 1, RESOURCES,
        entry.train_entry().unwrap().initialized_scratch()).unwrap();
    assert_ne!(existing.replay_identity(), ready.identity(), "existing-visit setup and new entry are different replay contracts");
    assert!(train_arrival_ready(&existing).is_err());

    for prefix in [&ready.transcript()[..0], &ready.transcript()[..1]] {
        assert!(replay_train_arrival(&field, &original, RESOURCES, landing(), ready.identity(), prefix).is_err(),
            "the full-ready helper refuses a truncated, otherwise valid prefix");
    }
    let mut wrong_identity = ready.identity();
    wrong_identity.version += 1;
    assert!(replay_train_arrival(&field, &original, RESOURCES, landing(), wrong_identity, ready.transcript()).is_err());
    let mut wrong = ready.transcript().to_vec();
    let SceneInput::Window(first) = &mut wrong[0] else { panic!("the train's first input is a window") };
    first.answer = Some(0);
    assert!(replay_train_arrival(&field, &original, RESOURCES, landing(), ready.identity(), &wrong).is_err());
    let mut extra = ready.transcript().to_vec();
    extra.push(extra[1].clone());
    assert!(replay_train_arrival(&field, &original, RESOURCES, landing(), ready.identity(), &extra).is_err());

    let mut unchecked = entry;
    unchecked.next(None);
    assert!(train_arrival_ready(&unchecked).is_err(), "even unchecked playback after completion invalidates strict evidence");
    assert!(encode_train_arrival(&unchecked).is_err());
}

#[test]
#[ignore = "needs original FF7 data; rejected ACKs must leave the live checked entry unchanged"]
fn owned_train_entry_requires_both_exact_plain_window_acknowledgments() {
    let (field, _) = fields();
    let mut play = TrainArrival::new(&field, &story(false), 1, RESOURCES, landing()).unwrap().into_play();
    assert!(matches!(play.next_strict(None).unwrap().then, Then::Window(_)));
    let Some(SceneBoundary::Window(first)) = play.boundary() else { panic!("the first window") };
    assert_eq!((first.entity, first.dialog, first.choices), (17, Some(32), None));
    let before = play.snapshot();
    assert!(encode_train_arrival(&play).is_err(), "an unacknowledged entry cannot mint a retained record");
    assert!(matches!(play.next_strict(Some(WindowAck { checkpoint: first, answer: Some(0) })), Err(SceneError::WindowBoundary)));
    assert_eq!(play.snapshot(), before);
    let mut wrong = first;
    wrong.ordinal += 1;
    assert!(matches!(play.next_strict(Some(WindowAck { checkpoint: wrong, answer: None })), Err(SceneError::WindowBoundary)));
    assert_eq!(play.snapshot(), before);
    assert!(matches!(play.next_strict(Some(WindowAck { checkpoint: first, answer: None })).unwrap().then, Then::Window(_)));
    let Some(SceneBoundary::Window(second)) = play.boundary() else { panic!("the second window") };
    assert_eq!((second.entity, second.dialog, second.choices), (37, Some(39), None));
    let before = play.snapshot();
    assert!(matches!(play.next_strict(Some(WindowAck { checkpoint: first, answer: None })), Err(SceneError::WindowBoundary)));
    assert_eq!(play.snapshot(), before);
    assert!(train_arrival_ready(&play).is_err());
    assert!(matches!(play.next_strict(Some(WindowAck { checkpoint: second, answer: None })).unwrap().then,
        Then::End { how: End::Finished, .. }));
    assert!(train_arrival_ready(&play).is_ok());
}
