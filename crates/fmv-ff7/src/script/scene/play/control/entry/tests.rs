//! Invented entry scripts; no owned files, renderer, movie decoder or live admission.

use super::*;
use crate::script::scene::{CameraOp, CastMember, FadeOp, Look, Spot, Step, Trigger};
use crate::script::synth::SynthScript;
use crate::testutil::FieldSpec;

const ALL: ControlPolicy = ControlPolicy { user_release: true, menu_release: true, actor_selection: true };
const RUN: [u8; 16] = [1; 16];

fn uc(disabled: u8) -> Vec<u8> { vec![0x33, disabled] }
fn menu(disabled: u8) -> Vec<u8> { vec![0x4A, disabled] }
fn wait(frames: u16) -> Vec<u8> { [vec![0x24], frames.to_le_bytes().to_vec()].concat() }
fn set(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn copy_byte(bank: u8, destination: u8, source: u8) -> Vec<u8> { vec![0x80, (bank << 4) | bank, destination, source] }
fn code(parts: Vec<Vec<u8>>) -> Vec<u8> { [parts.concat(), vec![0]].concat() }
fn slot_zero(initializer: Vec<Vec<u8>>, main: Vec<Vec<u8>>) -> Vec<u8> { [code(initializer), code(main)].concat() }
fn words(values: &[i16]) -> Vec<u8> { values.iter().flat_map(|n| n.to_le_bytes()).collect() }
fn place(x: i16, y: i16) -> Vec<u8> { [vec![0xA5, 0, 0], words(&[x, y, 0, 0])].concat() }
fn pmvie(movie: u8) -> Vec<u8> { vec![0xF8, movie] }
fn movie() -> Vec<u8> { vec![0xF9] }
fn mvief(address: u8) -> Vec<u8> { vec![0xFA, 6, address] }
fn fade() -> Vec<u8> { vec![0x6B, 0, 0, 12, 23, 34, 2, 1, 255] }
fn camera() -> Vec<u8> { [vec![0x64, 0], words(&[60, -20])].concat() }

fn field(script: SynthScript) -> FieldFile {
    FieldFile::parse(FieldSpec { overrides: vec![(0, script.build())], ..Default::default() }.build()).unwrap()
}

fn definition() -> SceneDef {
    SceneDef {
        id: "test/entry-movie-boundary".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "director".into(), slot: 0, talk_radius: 80, moments: 0..=u16::MAX },
        setup: vec![], given: vec![], root: vec![Step::StartMain(0)],
        cast: vec![CastMember { entity: 0, name: "director".into(), look: Look::Appearance("test/director".into()), start: None }],
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
fn play(field: &FieldFile, def: &SceneDef, mains: &[u8], run: [u8; 16]) -> EntryScenePlay {
    EntryScenePlay::with_scratch(field, def, &story(), 77, resources(), &FieldScratch::cleared_for_entry(),
        mains, run, MovieCounterObservation { value: 0 }, ALL, Limits::default()).unwrap()
}
fn request(beat: EntryBeat, kind: MovieRequestKind) -> MovieRequest {
    let EntryYield::Movie(request) = beat.then else { panic!("expected movie request: {beat:?}") };
    assert_eq!(request.kind, kind);
    request
}
fn control(beat: EntryBeat) -> ControlCheckpoint {
    let EntryYield::Control(checkpoint) = beat.then else { panic!("expected control cut: {beat:?}") };
    checkpoint
}
fn asset(frames: u32) -> MovieAsset {
    MovieAsset { frames, timeline_fingerprint: [7; 32],
        counter_profile: MovieCounterProfile { fingerprint: [8; 32], maximum: 1000 } }
}
fn scratch_word(play: &EntryScenePlay, address: usize) -> u16 {
    play.scene().scratch_outcome().unwrap().unwrap().after().read_u16(6, address).unwrap()
}
fn movie_source(tail: Vec<Vec<u8>>) -> FieldFile {
    field(SynthScript::default().entity("director", vec![(0, slot_zero(vec![],
        [vec![pmvie(53), movie()], tail].concat()))]))
}

// Besides visible effects, rejected input must preserve the retained scheduler's clock,
// cumulative audit and accepted transcript. A matching later acknowledgement still works.
macro_rules! held {
    ($play:expr) => { (
        $play.field_frame(), $play.movie_counter().value, $play.cancelled(), $play.control_state(),
        $play.scene().snapshot(), $play.scene().stats().clone(), $play.scene().writes(),
        $play.scene().resources().clone(), $play.scene().scratch_outcome(), $play.transcript().to_vec(),
        format!("{:?}", $play.inner.play.vm)
    ) };
}

#[test]
fn all_original_initializers_retain_effects_and_finish_before_any_selected_main() {
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(
            vec![uc(1), menu(1), set(1, 4, 7), set(5, 9, 9), place(10, 20), fade(), camera(), vec![0xF0, 3], vec![0xFC, 4], vec![0xF1, 0, 6, 0, 64]],
            vec![copy_byte(1, 6, 4), uc(0), vec![0x12, 0]]))])
        .entity("helper", vec![(0, slot_zero(vec![set(1, 4, 11)], vec![set(1, 6, 200)]))])
        .entity("last", vec![(0, slot_zero(vec![set(1, 4, 13)], vec![set(1, 6, 201)]))]));
    let def = definition(); // No hand-maintained setup list: every real initializer matters.
    let mut play = play(&field, &def, &[0], RUN);
    let beat = play.advance_to(0).unwrap();
    let checkpoint = match beat.then { EntryYield::Control(c) => c, _ => panic!("the main's UC0 is the first cut: {beat:?}") };
    assert_eq!((checkpoint.entity, checkpoint.frame, checkpoint.operation), (0, 0, ControlOp::User { disabled: 0 }));
    assert_eq!(checkpoint.after, ControlState { user_disabled: Some(0), menu_disabled: Some(1), actor: None });
    let after = play.scene().current_story().unwrap();
    assert_eq!((after.var(1, 4), after.var(1, 6)), (13, 13), "the selected main reads the last initializer, never an earlier partial setup");
    assert_eq!(play.scene().scratch_outcome().unwrap().unwrap().after().values()[9], 9);
    assert!(beat.stage.iter().any(|event| matches!(event.what, Stage::Place { entity: 0, x: 10, y: 20, .. })), "initializer placement is an emitted effect");
    assert!(beat.stage.iter().any(|event| matches!(event.what, Stage::Sound { id: 6, .. })), "initializer sound is not discarded by quiet setup");
    let presentation: Vec<_> = beat.stage.iter().filter(|event| matches!(event.what, Stage::Fade(_) | Stage::Camera(_)))
        .map(|event| (event.at, event.what.clone())).collect();
    assert_eq!(presentation, [
        (0, Stage::Fade(FadeOp::Fade { kind: 1, rgb: [12, 23, 34], speed: 2, adjust: 255 })),
        (0, Stage::Camera(CameraOp::Scroll { x: 60, y: -20, frames: 0 })),
    ], "initializer fade and camera retain their original values, time and order");
    let audio: Vec<_> = beat.audio.iter().map(|cue| (cue.frame, cue.entity, cue.instruction.mnemonic, cue.instruction.literal("id"))).collect();
    assert_eq!(audio, [(0, 0, "MUSIC", Some(3)), (0, 0, "FMUSC", Some(4))],
        "literal music cues keep their order; SOUND is emitted only through its existing stage event");
    assert_eq!(play.scene().ended(), None);
    play.acknowledge(ControlAck::Control(checkpoint)).unwrap();
    assert!(matches!(play.advance_to(1).unwrap().then, EntryYield::Idle));
    assert_eq!(play.scene().current_story().unwrap().var(1, 6), 13, "unselected mains did not run");
    assert_eq!(play.scene().ended(), None, "the main's parked poll is field idleness, not a finished authored scene");
}

#[test]
fn movie_waits_hold_only_the_issuer_and_mvief_reads_its_acknowledged_counter_not_field_time_or_presented_ordinal() {
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), set(1, 4, 1), movie(), set(1, 4, 2), uc(0)]))])
        .entity("helper", vec![(0, slot_zero(vec![], vec![
            wait(1), set(1, 5, 1), wait(1), set(1, 5, 2), mvief(30),
            wait(1), set(1, 5, 3), mvief(32), wait(1), set(1, 5, 4), mvief(34),
        ]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0, 1], RUN);
    let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert_eq!((prepare.movie, prepare.entity), (53, 0));
    let waiting = play.advance_to(1).unwrap();
    assert!(matches!(waiting.then, EntryYield::AwaitExternal));
    let at_prepare = play.scene().current_story().unwrap();
    assert_eq!((at_prepare.var(1, 4), at_prepare.var(1, 5)), (0, 1), "the helper runs while PMVIE's caller still waits for preparation");
    play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(5) }).unwrap();
    let playing = request(play.advance_to(1).unwrap(), MovieRequestKind::Play);
    assert_eq!((playing.movie, playing.entity), (53, 0));
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 0 }).unwrap();
    play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 0 } }).unwrap();
    assert!(matches!(play.advance_to(2).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(scratch_word(&play, 30), 0);
    assert_eq!(play.scene().current_story().unwrap().var(1, 4), 1, "MOVIE still holds its caller");
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 3 }).unwrap();
    assert_eq!(play.movie_counter().value, 0, "decoder ordinal alone never advances the native counter");
    play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 19 } }).unwrap();
    assert_eq!(play.movie_counter().value, 19);
    play.advance_to(3).unwrap();
    assert_eq!(scratch_word(&play, 32), 19, "MVIEF sees the host's separately profiled counter, not decoded ordinal3 or thirty-Hz scene ticks");
    play.advance_to(4).unwrap();
    assert_eq!((play.field_frame(), play.movie_counter().value, scratch_word(&play, 34)), (4, 19, 19), "without another counter observation MVIEF does not advance");
    let held = held!(play);
    assert!(matches!(play.acknowledge_movie(MovieAck::Completed { token: playing.token }), Err(EntryError::Input)));
    assert_eq!(held!(play), held, "a premature movie end cannot resume the source");
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 4 }).unwrap();
    assert!(matches!(play.advance_to(4).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(play.scene().current_story().unwrap().var(1, 4), 1, "showing the last frame alone cannot release MOVIE or run its tail");
    play.acknowledge_movie(MovieAck::Completed { token: playing.token }).unwrap();
    let cut = control(play.advance_to(4).unwrap());
    assert_eq!(cut.operation, ControlOp::User { disabled: 0 });
    assert_eq!((play.scene().current_story().unwrap().var(1, 4), play.scene().current_story().unwrap().var(1, 5)), (2, 4));
    assert_eq!(play.scene().ended(), None);
}

#[test]
fn horizons_external_waits_and_idle_are_nonterminal_and_a_backward_horizon_does_not_mutate() {
    let field = field(SynthScript::default().entity("director", vec![(0, slot_zero(vec![], vec![wait(10), uc(0), vec![0x12, 0]]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0], RUN);
    assert!(matches!(play.advance_to(0).unwrap().then, EntryYield::Horizon));
    assert!(matches!(play.advance_to(5).unwrap().then, EntryYield::Horizon));
    assert_eq!((play.field_frame(), play.scene().ended()), (5, None));
    let held = held!(play);
    assert!(matches!(play.advance_to(4), Err(EntryError::Horizon)));
    assert_eq!(held!(play), held);
    let checkpoint = control(play.advance_to(10).unwrap());
    play.acknowledge(ControlAck::Control(checkpoint)).unwrap();
    assert!(matches!(play.advance_to(10).unwrap().then, EntryYield::Horizon),
        "the retained scheduler charges one field frame for JMPB before its self-loop can park");
    assert!(matches!(play.advance_to(11).unwrap().then, EntryYield::Idle));
    assert!(matches!(play.advance_to(30).unwrap().then, EntryYield::Idle));
    assert_eq!(play.scene().ended(), None);

    let field = movie_source(vec![uc(0)]);
    let mut waiting = self::play(&field, &def, &[0], RUN);
    request(waiting.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert!(matches!(waiting.advance_to(30).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(waiting.scene().ended(), None, "an external wait is neither Finished nor Budget");
}

#[test]
fn the_hard_frame_ceiling_budgets_future_work_but_never_completes_or_budgets_a_pending_movie() {
    let def = definition();
    let scratch = FieldScratch::cleared_for_entry();
    let limited = |field: &FieldFile, mains: &[u8]| EntryScenePlay::with_scratch(field, &def, &story(), 77,
        resources(), &scratch, mains, RUN, MovieCounterObservation { value: 0 }, ALL, Limits { frames: 5, ..Limits::default() }).unwrap();
    let field = field(SynthScript::default().entity("director", vec![(0, slot_zero(vec![], vec![wait(6), uc(0)]))]));
    let mut scheduled = limited(&field, &[0]);
    assert!(matches!(scheduled.advance_to(4).unwrap().then, EntryYield::Horizon));
    assert!(matches!(scheduled.advance_to(5),
        Err(EntryError::Control(ControlError::Scene(SceneError::Scratch(FieldScratchError::InvalidOutcome))))),
        "the checked execution API refuses an afterimage of a budgeted scene");
    assert_eq!(scheduled.scene().ended(), Some(End::Budget));
    assert!(matches!(scheduled.scene().scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome))));
    assert!(scheduled.scene().current_story().is_none());

    let field = self::field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), movie(), uc(0)]))])
        .entity("helper", vec![(0, slot_zero(vec![], vec![wait(6), set(1, 5, 99)]))]));
    let mut waiting = limited(&field, &[0, 1]);
    let prepare = request(waiting.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert!(matches!(waiting.advance_to(5).unwrap().then, EntryYield::AwaitExternal),
        "the outstanding movie takes precedence even with scheduled helper work beyond the ceiling");
    assert_eq!((waiting.field_frame(), waiting.scene().ended()), (5, None));
    waiting.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(1) }).unwrap();
    let playing = request(waiting.advance_to(5).unwrap(), MovieRequestKind::Play);
    waiting.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 0 }).unwrap();
    assert!(matches!(waiting.advance_to(5).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(waiting.scene().ended(), None, "a final frame at the ceiling still needs a separate completion attestation");
}

#[test]
fn exclusive_tick_zero_keeps_initializers_pending_until_inclusive_dispatch() {
    let field = field(SynthScript::default().entity("director", vec![(0,
        slot_zero(vec![set(1, 4, 7), place(10, 20), vec![0xF0, 3]], vec![uc(0), vec![0x12, 0]]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0], RUN);
    let before_steps = play.scene().stats().steps;
    let before = play.advance_before(0).unwrap();
    assert!(matches!(before.then, EntryYield::Horizon));
    assert_eq!((before.frame, before.at, play.field_frame()), (0, 0, 0));
    assert!(before.stage.is_empty() && before.audio.is_empty());
    assert_eq!(play.scene().stats().steps, before_steps, "even the initializer root is a tick-zero job");
    assert_eq!(play.scene().current_story().unwrap().var(1, 4), 0);
    assert_eq!(play.control_state(), ControlState::default());
    assert_eq!(play.transcript(), [EntryInput::AdvanceBefore(0)]);
    let inclusive = play.advance_to(0).unwrap();
    assert!(matches!(inclusive.then, EntryYield::Control(_)));
    assert_eq!(play.scene().current_story().unwrap().var(1, 4), 7);
    assert!(inclusive.stage.iter().any(|event| matches!(event.what, Stage::Place { entity: 0, x: 10, y: 20, .. })));
    assert_eq!(inclusive.audio.len(), 1, "deferred initializer audio is emitted exactly when dispatched");
    let replayed = self::play(&field, &def, &[0], RUN).replay(play.replay_identity(), play.transcript()).unwrap();
    assert_eq!(held!(replayed), held!(play), "exclusive advances are replay inputs with their own dispatch semantics");
}

#[test]
fn a_counter_observation_at_an_exclusive_tick_precedes_due_jobs_and_wakes_parked_polling() {
    let poll = [mvief(32), vec![0x12, 3]].concat();
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), movie(), uc(0)]))])
        .entity("due", vec![(0, slot_zero(vec![], vec![wait(2), mvief(30), set(1, 5, 1), vec![0x12, 0]]))])
        .entity("poller", vec![(0, slot_zero(vec![], vec![poll]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0, 1, 2], RUN);
    let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(2) }).unwrap();
    let playing = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
    play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 7 } }).unwrap();
    assert!(matches!(play.advance_to(0).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!((scratch_word(&play, 30), scratch_word(&play, 32)), (0, 7));
    let before = play.advance_before(2).unwrap();
    assert!(matches!(before.then, EntryYield::AwaitExternal));
    assert_eq!(play.field_frame(), 2);
    assert_eq!((scratch_word(&play, 30), scratch_word(&play, 32), play.scene().current_story().unwrap().var(1, 5)), (0, 7, 0),
        "WAIT's due tick has been reached but not dispatched, and a parked poll has no new input yet");
    play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 9 } }).unwrap();
    assert_eq!((scratch_word(&play, 30), scratch_word(&play, 32)), (0, 7), "an observation wakes jobs without executing them");
    assert!(matches!(play.advance_to(2).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!((scratch_word(&play, 30), scratch_word(&play, 32), play.scene().current_story().unwrap().var(1, 5)), (9, 7, 1),
        "the due WAIT job sees this tick's observation; the parked poll wakes at its JMPB, whose retained one-frame delay remains");
    assert!(matches!(play.advance_before(3).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(scratch_word(&play, 32), 7, "the poll's MVIEF is due at tick 3, which is not dispatched yet");
    assert!(matches!(play.advance_to(3).unwrap().then, EntryYield::AwaitExternal));
    assert_eq!(scratch_word(&play, 32), 9, "the awakened poll reads the observation when its next MVIEF actually executes");
    let replayed = self::play(&field, &def, &[0, 1, 2], RUN).replay(play.replay_identity(), play.transcript()).unwrap();
    assert_eq!(held!(replayed), held!(play));
}

#[test]
fn an_exclusive_advance_returns_an_earlier_movie_boundary_at_its_actual_frame() {
    let field = field(SynthScript::default().entity("director", vec![(0,
        slot_zero(vec![], vec![wait(2), pmvie(53), movie(), uc(0)]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0], RUN);
    let beat = play.advance_before(10).unwrap();
    assert_eq!((beat.frame, beat.at, play.field_frame()), (2, 2, 2));
    let prepare = request(beat, MovieRequestKind::Prepare);
    assert_eq!(prepare.frame, 2, "the requested horizon does not overwrite the actual source boundary time");
    assert_eq!(play.transcript(), [EntryInput::AdvanceBefore(10)]);
    play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(2) }).unwrap();
    let playing = request(play.advance_before(10).unwrap(), MovieRequestKind::Play);
    assert_eq!((playing.frame, play.field_frame()), (2, 2));
}

#[test]
fn an_exclusive_advance_at_the_hard_ceiling_leaves_due_jobs_for_inclusive_dispatch() {
    let field = field(SynthScript::default().entity("director", vec![(0,
        slot_zero(vec![], vec![wait(5), set(1, 4, 7), uc(0)]))]));
    let def = definition();
    let mut play = EntryScenePlay::with_scratch(&field, &def, &story(), 77, resources(),
        &FieldScratch::cleared_for_entry(), &[0], RUN, MovieCounterObservation { value: 0 }, ALL, Limits { frames: 5, ..Limits::default() }).unwrap();
    let before = play.advance_before(5).unwrap();
    assert!(matches!(before.then, EntryYield::Horizon));
    assert_eq!((play.field_frame(), play.scene().ended(), play.scene().current_story().unwrap().var(1, 4)), (5, None, 0));
    let checkpoint = control(play.advance_to(5).unwrap());
    assert_eq!((checkpoint.frame, checkpoint.operation), (5, ControlOp::User { disabled: 0 }));
    assert_eq!((play.scene().ended(), play.scene().current_story().unwrap().var(1, 4)), (None, 7),
        "the due job at the ceiling is admissible when the host explicitly dispatches that tick");
}

#[test]
fn an_explicit_initial_movie_counter_is_identity_bound_and_survives_preparation() {
    let field = field(SynthScript::default().entity("director", vec![(0,
        slot_zero(vec![], vec![mvief(30), pmvie(53), mvief(32), movie(), uc(0)]))]));
    let def = definition();
    let zero = play(&field, &def, &[0], RUN);
    let mut seeded = EntryScenePlay::with_scratch(&field, &def, &story(), 77, resources(),
        &FieldScratch::cleared_for_entry(), &[0], RUN, MovieCounterObservation { value: 27 }, ALL, Limits::default()).unwrap();
    assert_ne!(seeded.replay_identity(), zero.replay_identity(), "a different initial MVIEF value changes execution semantics");
    assert_eq!(seeded.movie_counter().value, 27);
    let prepare = request(seeded.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert_eq!((scratch_word(&seeded, 30), seeded.movie_counter().value), (27, 27));
    seeded.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(2) }).unwrap();
    request(seeded.advance_to(0).unwrap(), MovieRequestKind::Play);
    assert_eq!((scratch_word(&seeded, 32), seeded.movie_counter().value), (27, 27),
        "PMVIE preparation neither resets the explicit counter nor invents a counter observation");
}

#[test]
fn wrong_cross_run_stale_and_premature_movie_acknowledgements_preserve_every_observable_state() {
    let field = movie_source(vec![pmvie(54), movie(), uc(0)]);
    let def = definition();
    let mut play = play(&field, &def, &[0], RUN);
    let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    let mut other = self::play(&field, &def, &[0], [2; 16]);
    let foreign = request(other.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert_ne!(prepare.token, foreign.token, "a token cannot cross runs even with identical source and inputs");
    for ack in [
        MovieAck::Prepared { token: foreign.token, asset: asset(4) },
        MovieAck::Presented { token: prepare.token, ordinal: 0 },
        MovieAck::Counter { token: prepare.token, observation: MovieCounterObservation { value: 0 } },
        MovieAck::Completed { token: prepare.token },
        MovieAck::Cancelled { token: foreign.token },
        MovieAck::Refused { token: foreign.token },
        MovieAck::Prepared { token: prepare.token, asset: asset(0) },
    ] {
        let held = held!(play);
        assert!(matches!(play.acknowledge_movie(ack), Err(EntryError::Input)));
        assert_eq!(held!(play), held);
    }
    play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(4) }).unwrap();
    let playing = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 2 }).unwrap();
    play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 19 } }).unwrap();
    for ack in [
        MovieAck::Prepared { token: prepare.token, asset: asset(4) },
        MovieAck::Presented { token: playing.token, ordinal: 2 },
        MovieAck::Presented { token: playing.token, ordinal: 1 },
        MovieAck::Presented { token: playing.token, ordinal: 4 },
        MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 19 } },
        MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 18 } },
        MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 1001 } },
        MovieAck::Counter { token: foreign.token, observation: MovieCounterObservation { value: 20 } },
        MovieAck::Completed { token: playing.token },
        MovieAck::Cancelled { token: foreign.token },
    ] {
        let held = held!(play);
        assert!(matches!(play.acknowledge_movie(ack), Err(EntryError::Input)));
        assert_eq!(held!(play), held);
    }
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 3 }).unwrap();
    play.acknowledge_movie(MovieAck::Completed { token: playing.token }).unwrap();
    let next = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert_eq!(next.movie, 54);
    assert_ne!(next.token, playing.token);
    for ack in [
        MovieAck::Prepared { token: prepare.token, asset: asset(4) },
        MovieAck::Presented { token: playing.token, ordinal: 3 },
        MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 20 } },
        MovieAck::Completed { token: playing.token },
        MovieAck::Cancelled { token: playing.token },
        MovieAck::Refused { token: playing.token },
    ] {
        let held = held!(play);
        assert!(matches!(play.acknowledge_movie(ack), Err(EntryError::Input)));
        assert_eq!(held!(play), held, "an old movie cannot consume or cancel the second request");
    }
    play.acknowledge_movie(MovieAck::Prepared { token: next.token, asset: asset(1) }).unwrap();
    let last = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
    play.acknowledge_movie(MovieAck::Presented { token: last.token, ordinal: 0 }).unwrap();
    play.acknowledge_movie(MovieAck::Completed { token: last.token }).unwrap();
    assert_eq!(control(play.advance_to(0).unwrap()).operation, ControlOp::User { disabled: 0 });
}

#[test]
fn cancellation_and_refusal_stop_the_current_request_without_finishing_or_running_its_tail() {
    let field = movie_source(vec![set(1, 4, 99), uc(0)]);
    let def = definition();
    for preparing in [true, false] {
        for refused in [false, true] {
            let mut play = play(&field, &def, &[0], RUN);
            let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
            let token = if preparing { prepare.token } else {
                play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(4) }).unwrap();
                request(play.advance_to(0).unwrap(), MovieRequestKind::Play).token
            };
            let effects = (play.field_frame(), play.movie_counter().value, play.control_state(), play.scene().stats().clone(),
                play.scene().writes(), play.scene().resources().clone(), play.scene().scratch_outcome());
            let ack = if refused { MovieAck::Refused { token } } else { MovieAck::Cancelled { token } };
            play.acknowledge_movie(ack).unwrap();
            if !refused { assert!(play.cancelled()); }
            for horizon in [0, 30] {
                let result = play.advance_to(horizon);
                assert!(if refused { matches!(result, Err(EntryError::Refused)) } else { matches!(result, Err(EntryError::Cancelled)) });
            }
            assert_eq!((play.field_frame(), play.movie_counter().value, play.control_state(), play.scene().stats().clone(),
                play.scene().writes(), play.scene().resources().clone(), play.scene().scratch_outcome()), effects);
            assert_eq!(play.scene().current_story().unwrap().var(1, 4), 0);
            assert_eq!(play.scene().ended(), None, "refused or cancelled media is never successful authored completion");
            let stopped = held!(play);
            assert!(play.acknowledge_movie(MovieAck::Completed { token }).is_err());
            assert_eq!(held!(play), stopped, "a late successful EOF cannot revive the cancelled source");
        }
    }
}

#[test]
fn a_movie_can_be_cancelled_or_refused_while_a_helper_control_cut_remains_unconsumed() {
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), movie(), set(1, 4, 99)]))])
        .entity("helper", vec![(0, slot_zero(vec![], vec![wait(1), uc(0), set(1, 5, 99)]))]));
    let def = definition();
    for refused in [false, true] {
        let mut play = play(&field, &def, &[0, 1], RUN);
        let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
        play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(1) }).unwrap();
        let playing = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
        play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 0 }).unwrap();
        let cut = control(play.advance_to(1).unwrap());
        assert_eq!((cut.entity, cut.frame), (1, 1));
        for ack in [
            MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 1 } },
            MovieAck::Completed { token: playing.token },
        ] {
            let held = held!(play);
            assert!(matches!(play.acknowledge_movie(ack), Err(EntryError::Input)));
            assert_eq!(held!(play), held, "ordinary movie progress must respect the helper's global cut");
        }
        let boundary = play.boundary();
        let vm = format!("{:?}", play.inner.play.vm);
        let ack = if refused { MovieAck::Refused { token: playing.token } } else { MovieAck::Cancelled { token: playing.token } };
        play.acknowledge_movie(ack).unwrap();
        assert_eq!(play.boundary(), boundary, "stopping the driver does not consume or replace the helper's control checkpoint");
        assert_eq!(format!("{:?}", play.inner.play.vm), vm, "neither blocked issuer nor helper continues on cancellation");
        let after = play.scene().current_story().unwrap();
        assert_eq!((after.var(1, 4), after.var(1, 5), play.scene().ended()), (0, 0, None));
        let result = play.acknowledge(ControlAck::Control(cut));
        assert!(if refused { matches!(result, Err(EntryError::Refused)) } else { matches!(result, Err(EntryError::Cancelled)) });
    }
}

#[test]
fn replay_exposes_unacknowledged_movie_requests_so_the_restored_prefix_can_continue() {
    let field = movie_source(vec![set(1, 4, 7), uc(0)]);
    let def = definition();
    let mut original = play(&field, &def, &[0], RUN);
    assert_eq!(original.pending_movie_request(), None);
    request(original.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    let mut prepared = play(&field, &def, &[0], RUN)
        .replay(original.replay_identity(), original.transcript()).unwrap();
    assert_eq!(held!(prepared), held!(original));
    let restored = prepared.pending_movie_request().expect("the replay prefix ends at an unacknowledged prepare");
    assert_eq!(Some(restored), original.pending_movie_request());
    assert_eq!((restored.kind, restored.movie, restored.entity, restored.frame), (MovieRequestKind::Prepare, 53, 0, 0));
    prepared.acknowledge_movie(MovieAck::Prepared { token: restored.token, asset: asset(2) }).unwrap();
    assert_eq!(prepared.pending_movie_request(), None);
    request(prepared.advance_to(0).unwrap(), MovieRequestKind::Play);

    let mut playing = play(&field, &def, &[0], RUN)
        .replay(prepared.replay_identity(), prepared.transcript()).unwrap();
    assert_eq!(held!(playing), held!(prepared));
    let restored = playing.pending_movie_request().expect("the replay prefix ends at an unacknowledged play");
    assert_eq!(Some(restored), prepared.pending_movie_request());
    assert_eq!((restored.kind, restored.movie, restored.entity, restored.frame), (MovieRequestKind::Play, 53, 0, 0));
    playing.acknowledge_movie(MovieAck::Counter { token: restored.token, observation: MovieCounterObservation { value: 7 } }).unwrap();
    playing.acknowledge_movie(MovieAck::Presented { token: restored.token, ordinal: 1 }).unwrap();
    playing.acknowledge_movie(MovieAck::Completed { token: restored.token }).unwrap();
    assert_eq!(playing.pending_movie_request(), None);
    let checkpoint = control(playing.advance_to(0).unwrap());
    assert_eq!(checkpoint.operation, ControlOp::User { disabled: 0 });
    assert_eq!((playing.movie_counter().value, playing.scene().current_story().unwrap().var(1, 4)), (7, 7),
        "continuation needs only the restored public request, never a token retained outside the transcript");
}

#[test]
fn a_battle_during_movie_preparation_or_play_is_refused_before_executing_its_instruction() {
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), movie(), set(1, 4, 99)]))])
        .entity("helper", vec![(0, slot_zero(vec![], vec![wait(2), vec![0x70, 0, 72, 1], set(1, 5, 99)]))]));
    let def = definition();
    let battle_offset = u32::try_from(FieldScript::from_field(&field).unwrap().span(1, 0).unwrap().start + 4).unwrap();
    for in_play in [false, true] {
        for refused in [false, true] {
            let mut play = play(&field, &def, &[0, 1], RUN);
            let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
            let pending = if in_play {
                play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(2) }).unwrap();
                request(play.advance_to(0).unwrap(), MovieRequestKind::Play)
            } else { prepare };
            assert!(matches!(play.advance_before(2).unwrap().then, EntryYield::AwaitExternal));
            let vm = format!("{:?}", play.inner.play.vm);
            let beat = play.advance_to(2).unwrap();
            assert_eq!((beat.frame, beat.at), (2, 0));
            assert_eq!(beat.then, EntryYield::MovieBattleRefused { entity: 1, instruction_offset: battle_offset });
            assert!(beat.stage.is_empty() && beat.audio.is_empty());
            assert_eq!(format!("{:?}", play.inner.play.vm), vm,
                "BATTLE must not advance its PC, count as an executed instruction, or create battle state");
            assert_eq!(play.boundary(), None, "there is no battle checkpoint to resume");
            assert_eq!(play.scene().ended(), None);
            assert_eq!(play.pending_movie_request(), Some(pending));
            let held = held!(play);
            assert!(matches!(play.advance_to(2), Err(EntryError::MovieBattle)));
            let ack = if in_play { MovieAck::Presented { token: pending.token, ordinal: 0 } }
                else { MovieAck::Prepared { token: pending.token, asset: asset(2) } };
            assert!(matches!(play.acknowledge_movie(ack), Err(EntryError::MovieBattle)));
            assert_eq!(held!(play), held, "ordinary advancement and movie acknowledgement cannot reopen the refused battle");
            let ack = if refused { MovieAck::Refused { token: pending.token } }
                else { MovieAck::Cancelled { token: pending.token } };
            play.acknowledge_movie(ack).unwrap();
            assert_eq!(format!("{:?}", play.inner.play.vm), vm, "movie cleanup cannot execute the refused BATTLE");
            assert_eq!(play.boundary(), None);
            let after = play.scene().current_story().unwrap();
            assert_eq!((after.var(1, 4), after.var(1, 5)), (0, 0));
            let result = play.advance_to(2);
            assert!(if refused { matches!(result, Err(EntryError::Refused)) } else { matches!(result, Err(EntryError::Cancelled)) });
        }
    }
}

#[test]
fn movie_assets_and_counter_profiles_bind_play_tokens_and_replay_checks_the_original_run_and_source() {
    let field = movie_source(vec![uc(0)]);
    let def = definition();
    let finish = |asset: MovieAsset| {
        let mut play = play(&field, &def, &[0], RUN);
        let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
        play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset }).unwrap();
        let playing = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
        play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 1 }).unwrap();
        play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value: 0 } }).unwrap();
        play.acknowledge_movie(MovieAck::Completed { token: playing.token }).unwrap();
        let checkpoint = control(play.advance_to(0).unwrap());
        (playing.token, checkpoint, play)
    };
    let media = asset(2);
    let (token, checkpoint, mut original) = finish(media);
    for changed in [
        MovieAsset { timeline_fingerprint: [9; 32], ..media },
        MovieAsset { counter_profile: MovieCounterProfile { fingerprint: [9; 32], ..media.counter_profile }, ..media },
        MovieAsset { counter_profile: MovieCounterProfile { maximum: 999, ..media.counter_profile }, ..media },
    ] {
        let (changed_token, changed_checkpoint, _) = finish(changed);
        assert_ne!(changed_token, token, "playback cannot reuse a token for different timing or counter semantics");
        assert_ne!(changed_checkpoint.accepted_prefix, checkpoint.accepted_prefix);
        assert_eq!(ControlCheckpoint { accepted_prefix: checkpoint.accepted_prefix, ..changed_checkpoint }, checkpoint,
            "otherwise-identical later source cuts bind the media evidence in their accepted prefix");
        let held = held!(original);
        assert!(matches!(original.acknowledge(ControlAck::Control(changed_checkpoint)), Err(EntryError::Input)));
        assert_eq!(held!(original), held, "a cut from different movie evidence cannot release this source");
    }
    let identity = original.replay_identity();
    let replayed = play(&field, &def, &[0], RUN).replay(identity, original.transcript()).unwrap();
    assert_eq!(replayed.boundary(), original.boundary());
    assert_eq!(held!(replayed), held!(original));
    assert!(matches!(play(&field, &def, &[0], [2; 16]).replay(identity, original.transcript()), Err(EntryError::Control(ControlError::Identity))),
        "source identity alone cannot replay another run's movie acknowledgement");
    let changed_source = movie_source(vec![set(1, 4, 9), uc(0)]);
    assert!(matches!(play(&changed_source, &def, &[0], RUN).replay(identity, original.transcript()),
        Err(EntryError::Control(ControlError::Identity))), "different original source bytes cannot consume this transcript");
}

#[test]
fn independent_counter_observations_drive_synthetic_mvief_thresholds_without_advancing_presented_frames() {
    // Two invented tests use the opening's interesting numbers, but establish no native
    // phase/rate mapping: only explicit counter observations can satisfy these conditions.
    let conditional = |threshold: u16, address: u8| {
        [vec![0x18, 0x60, 30, 0], threshold.to_le_bytes().to_vec(), vec![2, 5], set(1, address, 1)].concat()
    };
    let mut poll = [mvief(30), conditional(20, 5), conditional(664, 6)].concat();
    let back = u8::try_from(poll.len()).unwrap();
    poll.extend([0x12, back]);
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![pmvie(53), movie(), uc(0)]))])
        .entity("poller", vec![(0, slot_zero(vec![], vec![poll]))]));
    let def = definition();
    let mut play = play(&field, &def, &[0, 1], RUN);
    let prepare = request(play.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    play.acknowledge_movie(MovieAck::Prepared { token: prepare.token, asset: asset(2) }).unwrap();
    let playing = request(play.advance_to(0).unwrap(), MovieRequestKind::Play);
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 0 }).unwrap();
    for (tick, (value, expected)) in [(20, [0, 0]), (21, [1, 0]), (664, [1, 0]), (665, [1, 1])].into_iter().enumerate() {
        // JMPB consumes one field frame in the retained scheduler. Counter values remain
        // independent inputs; each is sampled before that tick's next MVIEF dispatch.
        let tick = u32::try_from(tick).unwrap();
        assert!(matches!(play.advance_before(tick).unwrap().then, EntryYield::AwaitExternal));
        play.acknowledge_movie(MovieAck::Counter { token: playing.token, observation: MovieCounterObservation { value } }).unwrap();
        assert!(matches!(play.advance_to(tick).unwrap().then, EntryYield::AwaitExternal));
        assert_eq!((play.field_frame(), play.movie_counter().value, scratch_word(&play, 30)), (tick, value, value));
        let story = play.scene().current_story().unwrap();
        assert_eq!([story.var(1, 5), story.var(1, 6)], expected);
        assert_eq!(play.scene().ended(), None);
    }
    let held = held!(play);
    assert!(matches!(play.acknowledge_movie(MovieAck::Completed { token: playing.token }), Err(EntryError::Input)),
        "a high native counter does not prove the final decoded frame was presented");
    assert_eq!(held!(play), held);
}

#[test]
fn an_advance_only_replay_is_already_bound_to_its_original_run_before_any_movie_token_exists() {
    let field = field(SynthScript::default().entity("director", vec![(0, slot_zero(vec![], vec![wait(10), uc(0)]))]));
    let def = definition();
    let mut original = play(&field, &def, &[0], RUN);
    assert!(matches!(original.advance_to(5).unwrap().then, EntryYield::Horizon));
    assert_eq!(original.transcript(), [EntryInput::Advance(5)]);
    let other = play(&field, &def, &[0], [2; 16]);
    assert_ne!(other.replay_identity(), original.replay_identity());
    assert!(matches!(other.replay(original.replay_identity(), original.transcript()), Err(EntryError::Control(ControlError::Identity))));
}

#[test]
fn insufficient_thread_capacity_refuses_construction_or_checked_execution_never_partial_success() {
    let field = field(SynthScript::default()
        .entity("director", vec![(0, slot_zero(vec![], vec![set(1, 4, 11)]))])
        .entity("missing", vec![(0, slot_zero(vec![], vec![set(1, 5, 99)]))]));
    let def = definition();
    let scratch = FieldScratch::cleared_for_entry();
    for threads in [0, 1] {
        let limits = Limits { threads, ..Limits::default() };
        assert!(matches!(EntryScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &scratch,
            &[0, 1], RUN, MovieCounterObservation { value: 0 }, ALL, limits), Err(EntryError::Profile)));
        assert!(matches!(EntryScenePlay::with_resources(&field, &def, &story(), 77, resources(),
            &[0, 1], RUN, MovieCounterObservation { value: 0 }, ALL, limits), Err(EntryError::Profile)));
    }
    let mut play = EntryScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &scratch,
        &[0, 1], RUN, MovieCounterObservation { value: 0 }, ALL, Limits { threads: 2, ..Limits::default() }).unwrap();
    assert!(matches!(play.advance_to(0),
        Err(EntryError::Control(ControlError::Scene(SceneError::Scratch(FieldScratchError::InvalidOutcome))))));
    assert_eq!(play.scene().ended(), Some(End::Broken),
        "a dropped request invalidates checked execution immediately, before a partial scene can become a successful result");
    assert!(matches!(play.scene().scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome))));
    assert!(play.scene().stats().dropped_requests > 0);
    assert!(!play.scene().writes().persistent.contains_key(&(1, 5)), "a failed second main request cannot execute its tail");
    assert!(play.scene().current_story().is_none(), "an incomplete entry cannot publish an authoritative story");
}

#[test]
fn opting_into_entry_movies_does_not_change_legacy_playback_or_its_replay_identity() {
    let field = movie_source(vec![mvief(30), uc(0)]);
    let def = definition();
    let scratch = FieldScratch::cleared_for_entry();
    let mut legacy = ScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &scratch).unwrap();
    let identity = legacy.replay_identity();
    let before = legacy.next_strict(None).unwrap();
    assert!(matches!(before.then, Then::End { how: End::Finished, .. }));
    for instruction in ["PMVIE", "MOVIE", "MVIEF"] { assert_eq!(legacy.stats().ignored.get(instruction), Some(&1)); }
    assert_eq!(legacy.replay_identity(), identity);

    let mut entry = play(&field, &def, &[0], RUN);
    assert_ne!(entry.replay_identity(), identity, "new external-input semantics require their own identity domain");
    request(entry.advance_to(0).unwrap(), MovieRequestKind::Prepare);
    assert_eq!(entry.scene().ended(), None);
    let mut again = ScenePlay::with_scratch(&field, &def, &story(), 77, resources(), &scratch).unwrap();
    assert_eq!(again.replay_identity(), identity);
    assert_eq!(again.next_strict(None).unwrap(), before);
    assert_eq!(again.stats(), legacy.stats());
    assert_eq!(again.scratch_outcome(), legacy.scratch_outcome());
}
