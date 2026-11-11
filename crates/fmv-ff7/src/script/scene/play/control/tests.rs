//! Invented scripts exercising control cuts through the public wrapper, never live admission.

use super::*;
use crate::script::scene::{BattleResolution, ScriptRef, Step, Trigger};
use crate::script::synth::{dialog, SynthScript};
use crate::testutil::FieldSpec;

const ALL: ControlPolicy = ControlPolicy { user_release: true, menu_release: true, actor_selection: true };
const NONE: ControlPolicy = ControlPolicy { user_release: false, menu_release: false, actor_selection: false };

fn uc(disabled: u8) -> Vec<u8> { vec![0x33, disabled] }
fn menu(disabled: u8) -> Vec<u8> { vec![0x4A, disabled] }
fn cc(entity: u8) -> Vec<u8> { vec![0xBF, entity] }
fn wait(frames: u16) -> Vec<u8> { [vec![0x24], frames.to_le_bytes().to_vec()].concat() }
fn set(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn req(op: u8, entity: u8, slot: u8) -> Vec<u8> { vec![op, entity, (2 << 5) | slot] }
fn code(parts: Vec<Vec<u8>>) -> Vec<u8> { [parts.concat(), vec![0]].concat() }
fn words(values: &[i16]) -> Vec<u8> { values.iter().flat_map(|n| n.to_le_bytes()).collect() }
fn place(x: i16, y: i16) -> Vec<u8> { [vec![0xA5, 0, 0], words(&[x, y, 0, 0])].concat() }
fn walk(x: i16, y: i16) -> Vec<u8> { [vec![0xA8, 0], words(&[x, y])].concat() }

fn field(script: SynthScript) -> FieldFile {
    FieldFile::parse(FieldSpec { overrides: vec![(0, script.build())], ..Default::default() }.build()).unwrap()
}

fn source(setup: Vec<Vec<u8>>, body: Vec<Vec<u8>>) -> FieldFile {
    field(SynthScript::default().entity("actor", vec![(0, code(setup)), (1, code(body))]))
}

fn definition() -> SceneDef {
    SceneDef {
        id: "test/control-boundary".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 80, moments: 0..=u16::MAX },
        setup: vec![], given: vec![], root: vec![Step::Await(ScriptRef::new(0, 1))], cast: vec![],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}

fn story() -> StoryState {
    let mut story = StoryState::at(4).into_extended().unwrap();
    for address in 4..=6 { story.write_byte(1, address, 0).unwrap(); }
    story
}
fn resources() -> SceneResources { SceneResources { gil: Some(100) } }
fn play(field: &FieldFile, def: &SceneDef) -> ControlScenePlay {
    ControlScenePlay::with_scratch(field, def, &story(), 77, resources(), &FieldScratch::cleared_for_entry(), ALL, Limits::default()).unwrap()
}
fn cut(beat: ControlBeat) -> ControlCheckpoint {
    let ControlBeat::Control { checkpoint, .. } = beat else { panic!("expected a control cut: {beat:?}") };
    checkpoint
}
fn end(beat: ControlBeat) -> Beat {
    let ControlBeat::Scene(beat) = beat else { panic!("expected an ordinary terminal beat: {beat:?}") };
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }), "{beat:?}");
    beat
}
fn ack(play: &mut ControlScenePlay, checkpoint: ControlCheckpoint) -> ControlBeat {
    play.next_strict(Some(ControlAck::Control(checkpoint))).unwrap()
}

#[test]
fn setup_and_locks_are_observed_but_same_frame_releases_are_separate_exact_cuts() {
    let field = source(vec![uc(1), menu(1), cc(2)], vec![uc(0), menu(0), cc(1), menu(7), uc(0)]);
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    let mut play = play(&field, &def);
    assert_eq!(play.boundary(), None, "quiet setup cannot hand control to a caller");
    assert_eq!(play.control_state(), ControlState { user_disabled: Some(1), menu_disabled: Some(1), actor: Some(2) });
    let first = cut(play.next_strict(None).unwrap());
    assert_eq!((first.ordinal, first.frame, first.entity, first.operation), (1, 0, 0, ControlOp::User { disabled: 0 }));
    assert_eq!(first.after, ControlState { user_disabled: Some(0), menu_disabled: Some(1), actor: Some(2) });
    assert_eq!(play.scene().ended(), None, "UC0 is evidence, not scene completion or a grant");
    let second = cut(ack(&mut play, first));
    let third = cut(ack(&mut play, second));
    let fourth = cut(ack(&mut play, third));
    assert_eq!((second.ordinal, second.frame, second.operation), (2, 0, ControlOp::Menu { disabled: 0 }));
    assert_eq!((third.ordinal, third.frame, third.operation), (3, 0, ControlOp::Actor { entity: 1 }));
    assert_eq!((fourth.ordinal, fourth.frame, fourth.operation), (5, 0, ControlOp::User { disabled: 0 }));
    assert_eq!(fourth.after, ControlState { user_disabled: Some(0), menu_disabled: Some(7), actor: Some(1) });
    assert_eq!(second.instruction_offset, first.instruction_offset + 2);
    assert_eq!(third.instruction_offset, second.instruction_offset + 2);
    assert_eq!(fourth.instruction_offset, third.instruction_offset + 4);
    assert_eq!(first.identity, play.replay_identity());
    assert!(matches!(end(ack(&mut play, fourth)).then, Then::End { at: 0, .. }));

    let mut selected = ControlScenePlay::with_resources(&field, &def, &story(), 77, resources(),
        ControlPolicy { actor_selection: true, ..NONE }, Limits::default()).unwrap();
    let actor = cut(selected.next_strict(None).unwrap());
    assert_eq!((actor.ordinal, actor.operation, actor.after.user_disabled, actor.after.menu_disabled),
        (3, ControlOp::Actor { entity: 1 }, Some(0), Some(0)));
    end(ack(&mut selected, actor));
    assert_eq!(selected.control_state().menu_disabled, Some(7));
}

#[test]
fn ambient_controls_contribute_after_state_without_becoming_foreground_release_boundaries() {
    let field = field(SynthScript::default()
        .entity("actor", vec![(1, code(vec![wait(1), uc(0)]))])
        .entity("ambient", vec![(0, vec![0, 0x33, 0, 0x4A, 0, 0xBF, 2, 0x12, 0])]));
    let mut def = definition();
    def.root = vec![Step::StartAmbientMain(1), Step::Await(ScriptRef::new(0, 1))];
    let mut play = play(&field, &def);
    let checkpoint = cut(play.next_strict(None).unwrap());
    assert_eq!((checkpoint.entity, checkpoint.ordinal, checkpoint.frame), (0, 1, 1));
    assert_eq!(checkpoint.after, ControlState { user_disabled: Some(0), menu_disabled: Some(0), actor: Some(2) });
    end(ack(&mut play, checkpoint));
}

#[test]
fn rejected_acknowledgements_leave_the_boundary_effects_and_later_execution_unchanged() {
    let field = source(vec![], vec![uc(0), menu(0), vec![0x99, 5, 20], set(1, 4, 9)]);
    let def = definition();
    let mut changed = play(&field, &def);
    let first = cut(changed.next_strict(None).unwrap());
    let fake_window = WindowAck { checkpoint: WindowCheckpoint { ordinal: 1, entity: 0,
        instruction_offset: Some(first.instruction_offset), window: 0, dialog: None, choices: None }, answer: None };
    let invalid = [
        None,
        Some(ControlAck::Window(fake_window)),
        Some(ControlAck::Control(ControlCheckpoint { ordinal: first.ordinal + 1, ..first })),
        Some(ControlAck::Control(ControlCheckpoint { instruction_offset: first.instruction_offset + 1, ..first })),
        Some(ControlAck::Control(ControlCheckpoint { frame: first.frame + 1, ..first })),
        Some(ControlAck::Control(ControlCheckpoint { operation: ControlOp::Menu { disabled: 0 }, ..first })),
        Some(ControlAck::Control(ControlCheckpoint { after: ControlState::default(), ..first })),
        Some(ControlAck::Control(ControlCheckpoint { identity: ReplayIdentity { seed: 78, ..first.identity }, ..first })),
    ];
    for input in invalid {
        let held = (changed.boundary(), changed.control_state(), changed.transcript().to_vec(),
            changed.scene().stats().clone(), changed.scene().writes(), changed.scene().resources().clone(), changed.scene().scratch_outcome());
        assert_eq!(changed.next_strict(input), Err(ControlError::Acknowledgement));
        assert_eq!((changed.boundary(), changed.control_state(), changed.transcript().to_vec(),
            changed.scene().stats().clone(), changed.scene().writes(), changed.scene().resources().clone(), changed.scene().scratch_outcome()), held);
    }
    let second = cut(ack(&mut changed, first));
    assert_eq!(changed.next_strict(Some(ControlAck::Control(first))), Err(ControlError::Acknowledgement), "a repeated old ACK cannot consume MENU2");
    let actual = end(ack(&mut changed, second));
    let mut clean = play(&field, &def);
    let clean_first = cut(clean.next_strict(None).unwrap());
    let clean_second = cut(ack(&mut clean, clean_first));
    assert_eq!(end(ack(&mut clean, clean_second)), actual);
    assert_eq!(changed.scene().writes(), clean.scene().writes());
    assert_eq!(changed.scene().scratch_outcome(), clean.scene().scratch_outcome(), "rejected ACKs did not consume RNG");
    assert_eq!(changed.transcript(), clean.transcript());
    assert_eq!(changed.scene().current_story().unwrap().var(1, 4), 9);
    assert_eq!(changed.next_strict(Some(ControlAck::Control(second))), Err(ControlError::Acknowledgement), "completion cannot be acknowledged twice");
}

#[test]
fn a_control_cut_preserves_motion_slot_waiters_and_request_completion_without_draining_them() {
    let field = field(SynthScript::default()
        .entity("actor", vec![(1, code(vec![req(0x02, 1, 1), req(0x02, 2, 1), wait(1), uc(0)]))])
        .entity("walker", vec![
            (1, code(vec![place(10, 10), walk(110, 10), wait(7), set(1, 4, 1), vec![0xA4, 0]])),
            (2, code(vec![set(1, 5, 2), vec![0xA4, 1]])),
        ])
        .entity("waiter", vec![(1, code(vec![req(0x03, 1, 2), set(1, 6, 3)]))]));
    let mut def = definition();
    def.cast = (0..3).map(|entity| CastMember { entity, name: format!("actor{entity}"),
        look: Look::Appearance(format!("test/{entity}")), start: None }).collect();
    let mut play = play(&field, &def);
    let ControlBeat::Control { stage: mut actual, at, checkpoint } = play.next_strict(None).unwrap() else { panic!("control cut") };
    assert_eq!((at, checkpoint.frame), (1, 1));
    assert_eq!(play.scene().stats().queued, 1, "the second walker script is still waiting for its priority slot");
    assert!(actual.iter().any(|event| matches!(event.what, Stage::Move { entity: 1, frames, .. } if frames > at)));
    assert!(!actual.iter().any(|event| matches!(event.what, Stage::Show { .. })));
    assert_eq!(play.scene().current_story(), Some(story()), "neither tail nor its waiter has published the later writes");
    let tail = end(ack(&mut play, checkpoint));
    let Then::End { at: after, .. } = tail.then else { unreachable!() };
    assert!(after > 7, "pending movement and WAIT retain their time");
    actual.extend(tail.stage.into_iter().map(|mut event| { event.at += checkpoint.frame; event }));
    let shown: Vec<_> = actual.iter().filter_map(|event| match event.what { Stage::Show { entity, visible } => Some((entity, visible)), _ => None }).collect();
    assert_eq!(shown, [(1, false), (1, true)], "the old job ends once, then the queued job starts once");
    let finished = play.scene().current_story().unwrap();
    assert_eq!([finished.var(1, 4), finished.var(1, 5), finished.var(1, 6)], [1, 2, 3]);

    let mut legacy = ScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &FieldScratch::cleared_for_entry()).unwrap();
    let legacy_identity = legacy.replay_identity();
    let expected = legacy.next_strict(None).unwrap();
    assert_eq!(expected.stage, actual, "cutting and resuming preserves event ordering, timing and motion");
    assert_eq!(expected.then, Then::End { at: checkpoint.frame + after, how: End::Finished });
    assert_eq!(legacy.current_story(), Some(finished));
    assert_eq!(legacy.stats().ignored.get("UC"), Some(&1), "legacy statistics retain their original control treatment");
    assert_eq!(legacy.replay_identity(), legacy_identity);
    assert_ne!(play.replay_identity(), legacy_identity, "only the opt-in replay uses a new identity domain");
}

#[test]
fn repeated_execution_of_the_same_instruction_has_a_distinct_ordinal_even_at_one_frame() {
    let field = source(vec![], vec![uc(0)]);
    let mut def = definition();
    def.root.push(Step::Await(ScriptRef::new(0, 1)));
    let mut play = play(&field, &def);
    let first = cut(play.next_strict(None).unwrap());
    let second = cut(ack(&mut play, first));
    assert_eq!((first.instruction_offset, first.frame), (second.instruction_offset, second.frame));
    assert_eq!((first.ordinal, second.ordinal), (1, 2));
    assert_eq!(play.next_strict(Some(ControlAck::Control(first))), Err(ControlError::Acknowledgement));
    end(ack(&mut play, second));
}

#[test]
fn replay_binds_original_inputs_source_policy_limits_and_the_exact_endpoint() {
    let field = source(vec![], vec![uc(0), menu(0), cc(1)]);
    let def = definition();
    let mut original = play(&field, &def);
    let identity = original.replay_identity();
    let first = cut(original.next_strict(None).unwrap());
    let second = cut(ack(&mut original, first));
    let target = ControlBoundary::Control(second);
    let transcript = original.transcript().to_vec();
    let replayed = play(&field, &def).replay_to(identity, &transcript, target).unwrap();
    assert_eq!(replayed.boundary(), Some(target));
    assert_eq!(replayed.scene().stats(), original.scene().stats());
    assert!(matches!(play(&field, &def).replay_to(identity, &[], target), Err(ControlError::Boundary)), "a valid earlier cut cannot satisfy a later saved endpoint");
    let mut extra = transcript.clone();
    extra.push(ControlInput::Ack(ControlAck::Control(first)));
    assert!(matches!(play(&field, &def).replay_to(identity, &extra, target), Err(ControlError::Acknowledgement)));

    let mut other_story = story();
    other_story.write_byte(1, 80, 0).unwrap(); // Different knowledge, even though execution never reads it.
    let mut other_scratch = FieldScratch::cleared_for_entry();
    other_scratch.write_u8(5, 44, 1).unwrap();
    let fresh = FieldScratch::cleared_for_entry();
    let mut other_def = def.clone();
    other_def.id.push_str("-other");
    let other_field = source(vec![], vec![uc(0), menu(0), cc(2)]);
    let mut changed = vec![
        ControlScenePlay::with_scratch(&field, &def, &story(), 78, resources(), &fresh, ALL, Limits::default()).unwrap(),
        ControlScenePlay::with_scratch(&field, &def, &other_story, 77, resources(), &fresh, ALL, Limits::default()).unwrap(),
        ControlScenePlay::with_scratch(&field, &def, &story(), 77, SceneResources { gil: Some(101) }, &fresh, ALL, Limits::default()).unwrap(),
        ControlScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &other_scratch, ALL, Limits::default()).unwrap(),
        ControlScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &fresh, ControlPolicy { menu_release: false, ..ALL }, Limits::default()).unwrap(),
        play(&field, &other_def), play(&other_field, &def),
    ];
    for limits in [Limits { steps: 100_000, ..Limits::default() }, Limits { windows: 63, ..Limits::default() },
        Limits { threads: 63, ..Limits::default() }, Limits { frames: 9_999, ..Limits::default() }] {
        changed.push(ControlScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &fresh, ALL, limits).unwrap());
    }
    for changed in changed {
        assert!(matches!(changed.replay_to(identity, &transcript, target), Err(ControlError::Identity)));
    }
    let mut started = play(&field, &def);
    started.next_strict(None).unwrap();
    assert!(matches!(started.replay_to(identity, &transcript, target), Err(ControlError::Scene(SceneError::UncheckedReplay))));
}

#[test]
fn windows_and_battles_keep_their_own_acknowledgements_between_control_cuts() {
    let field = field(SynthScript { dialogs: vec![dialog("A window.")], ..Default::default() }
        .entity("actor", vec![(1, code(vec![uc(0), vec![0x40, 0, 0], vec![0x72, 8, 0],
            set(5, 9, 77), vec![0x70, 0, 72, 1], menu(0), set(1, 4, 9)]))]));
    let def = definition();
    let mut play = play(&field, &def);
    let identity = play.replay_identity();
    let first = cut(play.next_strict(None).unwrap());
    assert!(matches!(ack(&mut play, first), ControlBeat::Scene(Beat { then: Then::Window(_), .. })));
    let Some(ControlBoundary::Scene(SceneBoundary::Window(window))) = play.boundary() else { panic!("window checkpoint") };
    let held = play.scene().snapshot();
    assert_eq!(play.next_strict(Some(ControlAck::Control(first))), Err(ControlError::Acknowledgement));
    assert_eq!(play.next_strict(Some(ControlAck::Window(WindowAck { checkpoint: window, answer: Some(0) }))), Err(ControlError::Acknowledgement));
    assert_eq!(play.scene().snapshot(), held);
    let battle_beat = play.next_strict(Some(ControlAck::Window(WindowAck { checkpoint: window, answer: None }))).unwrap();
    assert!(matches!(battle_beat, ControlBeat::Scene(Beat { then: Then::End { how: End::Battle, .. }, .. })));
    let Some(ControlBoundary::Scene(SceneBoundary::Battle(battle))) = play.boundary() else { panic!("battle checkpoint") };
    assert_eq!((battle.formation, battle.flags), (328, Some(8)));
    assert_eq!(play.next_strict(None), Err(ControlError::Acknowledgement));
    assert_eq!(play.next_strict(Some(ControlAck::Control(first))), Err(ControlError::Acknowledgement));
    let resume = BattleResume { checkpoint: battle, resolution: BattleResolution::Victory,
        story: play.scene().current_story().unwrap(), resources: SceneResources { gil: Some(125) } };
    play.resume_battle_checked(resume.clone()).unwrap();
    assert_eq!(play.resume_battle_checked(resume), Err(ControlError::Scene(SceneError::BattleBoundary)));
    let second = cut(play.next_strict(None).unwrap());
    assert_eq!((second.ordinal, second.operation), (2, ControlOp::Menu { disabled: 0 }));
    assert_eq!(play.scene().resources().gil_after, Some(125));
    assert_eq!(play.scene().scratch_outcome().unwrap().unwrap().after().values()[9], 77);
    end(ack(&mut play, second));
    let replayed = self::play(&field, &def).replay_to(identity, play.transcript(), ControlBoundary::Scene(SceneBoundary::End(End::Finished))).unwrap();
    assert_eq!(replayed.scene().current_story(), play.scene().current_story());
    assert_eq!(replayed.scene().resources(), play.scene().resources());
    assert_eq!(replayed.scene().scratch_outcome(), play.scene().scratch_outcome());
}

#[test]
fn converging_choice_paths_bind_the_accepted_prefix_before_the_same_control_instruction() {
    let mut text = dialog("First");
    text.pop();
    text.push(0xE7);
    text.extend(dialog("Second"));
    let field = field(SynthScript { dialogs: vec![text], ..Default::default() }
        .entity("actor", vec![(1, code(vec![vec![0x48, 5, 0, 0, 0, 1, 20], uc(0)]))]));
    let def = definition();
    let mut first = play(&field, &def);
    let mut second = play(&field, &def);
    first.next_strict(None).unwrap();
    second.next_strict(None).unwrap();
    let Some(ControlBoundary::Scene(SceneBoundary::Window(window))) = first.boundary() else { panic!("question") };
    assert_eq!(window.choices, Some([0, 1]));
    assert_eq!(first.next_strict(Some(ControlAck::Window(WindowAck { checkpoint: window, answer: Some(2) }))), Err(ControlError::Acknowledgement));
    let a = cut(first.next_strict(Some(ControlAck::Window(WindowAck { checkpoint: window, answer: Some(0) }))).unwrap());
    let b = cut(second.next_strict(Some(ControlAck::Window(WindowAck { checkpoint: window, answer: Some(1) }))).unwrap());
    assert_eq!(ControlCheckpoint { accepted_prefix: b.accepted_prefix, ..a }, b,
        "both choices execute the identical release instruction at the same time with the same control state");
    assert_ne!(a.accepted_prefix, b.accepted_prefix, "their retained scratch/scheduler histories must not share an ACK");
    assert_ne!(a.identity_bytes(), b.identity_bytes());
    let held = (first.boundary(), first.transcript().to_vec(), first.scene().stats().clone(), first.scene().scratch_outcome());
    assert_eq!(first.next_strict(Some(ControlAck::Control(b))), Err(ControlError::Acknowledgement));
    assert_eq!((first.boundary(), first.transcript().to_vec(), first.scene().stats().clone(), first.scene().scratch_outcome()), held);
    assert!(matches!(play(&field, &def).replay_to(first.replay_identity(), second.transcript(), ControlBoundary::Control(a)), Err(ControlError::Boundary)));
    end(ack(&mut first, a));
    end(ack(&mut second, b));
    assert_eq!(first.scene().scratch_outcome().unwrap().unwrap().after().values()[20], 0);
    assert_eq!(second.scene().scratch_outcome().unwrap().unwrap().after().values()[20], 1);
}

#[test]
fn unreachable_release_bytes_do_not_become_checkpoints_or_observations() {
    // The known byte is zero. IFUB == 1 skips the complete two-byte UC0 instruction.
    let field = source(vec![], vec![vec![0x14, 0x10, 4, 1, 0, 3], uc(0), menu(0)]);
    let mut play = play(&field, &definition());
    let checkpoint = cut(play.next_strict(None).unwrap());
    assert_eq!((checkpoint.ordinal, checkpoint.operation), (1, ControlOp::Menu { disabled: 0 }));
    assert_eq!(checkpoint.after.user_disabled, None, "a byte scan cannot establish authored control state");
    assert!(!play.scene().stats().handled.contains_key("UC"));
    end(ack(&mut play, checkpoint));
}

/// Read-only source survey of the original station's moment-1 guard approach and victory
/// tail. Mode 32 remains refused by public battle replay. The private VM continuation below
/// is only a test seam to inspect the source's UC/MENU instructions and retained actor jobs;
/// it grants no battle authority, manufactures no accepted battle digest, and is not replayed
/// or persisted. Frame counts are this VM's estimates, never a native-engine timing claim.
#[test]
#[ignore = "needs the owner's original FF7 archive (FMV_FF7_DATA); source survey, no runtime admission"]
fn owned_station_mode32_survey_cuts_at_control_release_with_guard_work_still_pending() {
    use crate::lgp::LgpArchive;

    let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned FF7 data path"));
    let mut archive = LgpArchive::open_path(data.join("field/flevel.lgp")).expect("read-only original archive");
    let stored = archive.read("md1stin").expect("original station field");
    let field = FieldFile::parse(crate::lzss::decompress(&stored).unwrap()).unwrap();
    // This base predates the guard presentation definition. Keep the narrow source root
    // local: original director main, original initializers, no byte patch or forced join.
    // Cloud's position is an interior point of the real triggering triangle, not an
    // authored cutscene placement. In a live scene the world must supply the accepted pose.
    let origin = Spot { x: 3783, y: 28446, triangle: 12, direction: 0 };
    let mesh = field.walkmesh().unwrap();
    assert!(crate::script::triangle_contains_xy(&mesh.triangles[12], i32::from(origin.x), i32::from(origin.y)));
    let def = SceneDef {
        id: "test/md1stin/guard-control-survey".into(), game: "ff7".into(), field: "md1stin".into(),
        trigger: Trigger { entity: 0, name: "dir".into(), slot: 0, talk_radius: 0, moments: 1..=1 },
        setup: [0, 1, 4, 5].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect(),
        given: vec![], root: vec![Step::StartMain(0)],
        cast: [(1, "cl", "aaaa"), (4, "hei0", "bwab"), (5, "hei1", "bwab")].into_iter()
            .map(|(entity, name, hrc)| CastMember {
                entity, name: name.into(), look: Look::Model { hrc: hrc.into(), idle: "acfe".into() },
                start: (entity == 1).then_some(origin),
            }).collect(),
        speakers: vec![], witness: vec![], arrival: origin,
        presents: None, presents_bit: None, resume_after_battle: None,
    };
    let before = StoryState::fresh_native_first_control();
    assert_eq!(before.moment, 1);
    let resources = SceneResources { gil: Some(600) };
    let mut play = ControlScenePlay::with_resources(&field, &def, &before, 77, resources, ALL, Limits::default()).unwrap();
    assert!(play.scene().initial_stage_events().iter().any(|event| matches!(*event,
        Stage::Place { entity: 1, x, y, z: Some(_), triangle: Some(12) } if (x, y) == (origin.x, origin.y))),
        "the director's GETAI must see the supplied real trigger pose, not an initializer fallback");
    // At moment 1 the original branch skips the opening to CC1, not past it.
    // Selecting an actor is observed separately; it is not a UC/MENU release grant.
    let script = FieldScript::from_field(&field).unwrap();
    let director = script.disassemble(0, 0).unwrap();
    assert_eq!(director.iter().find(|ins| ins.offset == 972).unwrap().jump_target, Some(1185));
    let selected = cut(play.next_strict(None).unwrap());
    assert_eq!((selected.entity, selected.instruction_offset, selected.operation, selected.frame),
        (0, 1185, ControlOp::Actor { entity: 1 }, 0));
    let following = ack(&mut play, selected);
    let ControlBeat::Scene(prelude) = following else { panic!("expected the actual battle after CC1, before any UC/MENU release: {following:?}") };
    let Then::End { at: battle_frame, how: End::Battle } = prelude.then else { panic!("original station battle: {:?}", prelude.then) };
    let Some(ControlBoundary::Scene(SceneBoundary::Battle(battle))) = play.boundary() else { panic!("captured original battle") };
    assert_eq!((battle.entity, battle.instruction_offset, battle.formation, battle.flags), (0, 1222, 300, Some(32)));
    assert_eq!((play.control_state().user_disabled, play.control_state().menu_disabled), (Some(1), Some(1)));
    let guard_motion_end = prelude.stage.iter().filter_map(|event| match event.what {
        Stage::Move { entity: 4, frames, .. } => Some(event.at + frames), _ => None,
    }).max().expect("the original guard approach moved guard 4");
    assert!(guard_motion_end > battle_frame, "the VM still has an asynchronous guard move at BATTLE; these are VM estimates");
    assert!(!prelude.stage.iter().any(|event| matches!(event.what, Stage::Show { entity: 4, visible: false })));
    let after = play.scene().current_story().expect("fully known authored battle prefix");
    assert_eq!(after.moment, 6);
    let resume = BattleResume { checkpoint: battle, resolution: BattleResolution::Victory, story: after.clone(), resources };
    let held = (play.boundary(), play.transcript().to_vec(), play.scene().snapshot());
    assert_eq!(play.resume_battle_checked(resume), Err(ControlError::Scene(SceneError::BattleMode)));
    assert_eq!((play.boundary(), play.transcript().to_vec(), play.scene().snapshot()), held,
        "the public API still refuses mode 32 without consuming the battle");

    let acknowledged_approach = play.transcript().to_vec();
    assert_eq!(acknowledged_approach, [ControlInput::Ack(ControlAck::Control(selected))]);

    // Test-only source inspection. Preserve the captured scheduler and exact evaluated
    // story; do not invent ControlInput::Battle or an accepted-prefix hash for this bypass.
    play.play.vm.resume_battle(&after, resources);
    play.play.story = after.clone();
    play.play.ended = None;
    assert_eq!(play.transcript(), acknowledged_approach);
    let ControlBeat::Control { stage: uc_stage, at: uc_at, checkpoint: uc } = play.next_strict(None).unwrap() else { panic!("actual UC0 cut") };
    assert_eq!((uc.entity, uc.instruction_offset, uc.operation), (0, 1229, ControlOp::User { disabled: 0 }));
    assert_eq!((uc_at, uc.frame), (0, battle_frame));
    assert_eq!((uc.after.user_disabled, uc.after.menu_disabled), (Some(0), Some(1)));
    assert!(guard_motion_end > uc.frame);
    assert_eq!(play.scene().ended(), None, "the cut did not finish or join the scene");
    let ControlBeat::Control { stage: menu_stage, at: menu_at, checkpoint: menu } = ack(&mut play, uc) else { panic!("actual MENU2 cut") };
    assert_eq!((menu.entity, menu.instruction_offset, menu.operation), (0, 1231, ControlOp::Menu { disabled: 0 }));
    assert_eq!((menu_at, menu.frame, menu.ordinal), (0, uc.frame, uc.ordinal + 1));
    assert_eq!((menu.after.user_disabled, menu.after.menu_disabled), (Some(0), Some(0)));
    assert!(guard_motion_end > menu.frame, "MENU2 did not add a wait for the other guard");
    assert_eq!(play.scene().ended(), None);
    assert!(!uc_stage.iter().chain(&menu_stage).any(|event| matches!(event.what, Stage::Show { entity: 4, visible: false })));

    let ControlBeat::Scene(tail) = ack(&mut play, menu) else { panic!("no extra control release in the director poll") };
    assert!(matches!(tail.then, Then::End { how: End::Budget, .. }),
        "the conditional GETAI poll is not a RET or Finished: {:?}", tail.then);
    let hide_times: Vec<_> = tail.stage.iter().filter_map(|event| match event.what {
        Stage::Show { entity: 4, visible: false } => Some(menu.frame + event.at), _ => None,
    }).collect();
    assert_eq!(hide_times, [guard_motion_end], "the pending guard tail ran once after its estimated move, without being discarded");
    // The original slot-4 entry is VISI0 without a RET. A script-table span is
    // disassembly metadata, not an inserted return: this VM continues at the following
    // bytes (the next entity's initializer) until the actual RET, still as guard 5.
    // Derive that path from the source instead of guessing that one request means one hide.
    let mut pc = script.span(5, 4).unwrap().start;
    let mut hide_source = Vec::new();
    let mut fallthrough = Vec::new();
    for _ in 0..16 {
        let ins = crate::script::decode_at(script.bytes(), pc, 0).unwrap();
        fallthrough.push(ins.mnemonic);
        if ins.mnemonic == "VISI" {
            hide_source.push((ins.offset, ins.literal("visible").unwrap() != 0));
        }
        pc = ins.end();
        if ins.mnemonic == "RET" { break; }
    }
    assert_eq!(fallthrough, ["VISI", "CHAR", "XYZI", "DIR", "TLKON", "SOLID", "VISI", "RET"],
        "this survey depends on the original finite, linear slot-4 path");
    assert_eq!(hide_source, [(0x05EC, false), (0x0602, false)]);
    let guard5_shows: Vec<_> = uc_stage.iter().map(|event| (battle_frame, event))
        .chain(menu_stage.iter().map(|event| (uc.frame, event)))
        .chain(tail.stage.iter().map(|event| (menu.frame, event)))
        .filter_map(|(origin, event)| match event.what {
            Stage::Show { entity: 5, visible } => Some((origin + event.at, visible)), _ => None,
        }).collect();
    assert_eq!(guard5_shows, hide_source.iter().map(|(_, visible)| (battle_frame, *visible)).collect::<Vec<_>>(),
        "observed guard5 events {guard5_shows:?}; source hide sites {hide_source:?}");

    // An uncut run of the same source and private survey continuation must emit the
    // complete identical tail. This detects replayed/dropped requests or timing changes
    // caused by our cuts, without pretending these VM estimates are native frame counts.
    let mut uncut = ControlScenePlay::with_resources(&field, &def, &before, 77, resources, NONE, Limits::default()).unwrap();
    let ControlBeat::Scene(uncut_approach) = uncut.next_strict(None).unwrap() else { panic!("uncut approach") };
    assert_eq!(uncut_approach.then, Then::End { at: battle_frame, how: End::Battle });
    assert_eq!(uncut.boundary(), Some(ControlBoundary::Scene(SceneBoundary::Battle(battle))));
    assert_eq!(uncut.scene().current_story(), Some(after.clone()));
    uncut.play.vm.resume_battle(&after, resources);
    uncut.play.story = after;
    uncut.play.ended = None;
    let ControlBeat::Scene(uncut_tail) = uncut.next_strict(None).unwrap() else { panic!("uncut tail") };
    assert_eq!(uncut_tail.then, tail.then);
    let actual: Vec<_> = uc_stage.iter().map(|event| (battle_frame, event))
        .chain(menu_stage.iter().map(|event| (uc.frame, event)))
        .chain(tail.stage.iter().map(|event| (menu.frame, event)))
        .map(|(origin, event)| Timed { at: origin + event.at, what: event.what.clone() }).collect();
    let expected: Vec<_> = uncut_tail.stage.into_iter()
        .map(|event| Timed { at: battle_frame + event.at, what: event.what }).collect();
    assert_eq!(actual, expected, "control cuts retain exactly the uncut source survey's actor work and order");
    assert!(!play.transcript().iter().any(|event| matches!(event, ControlInput::Battle(_))),
        "this private survey never fabricates a replayable battle outcome");
}
