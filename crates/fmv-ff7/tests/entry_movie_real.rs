//! Original md1stin entry, evaluated read-only with synthetic host movie observations.
//!
//! This checks the VM's response to source instructions. It does not decode/play a movie,
//! prove wall-clock cadence, mix audio, draw a frame, admit an account, or publish a save.
//! Presented/Completed below are explicit test inputs, never evidence of actual display/EOF.
//! The first test preserves the missing native startup-party knowledge and checks that it
//! cannot produce an authoritative afterimage. A separate test accepts FMV_ENTRY_STORY: a
//! reviewed original moment-0 v4 StoryState binary blob. A third supplies the independently
//! evidenced PC reset-boundary party input, without claiming later startup-helper continuity.

mod script_common;

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{
    reactor_opening_scene, CameraOp, ControlAck, ControlBoundary, ControlError, ControlOp, ControlPolicy,
    EntryAudio, EntryBeat, EntryError, EntryScenePlay, EntryYield, FadeOp, Limits, MovieAck, MovieAsset,
    MovieCounterObservation, MovieCounterProfile, MovieRequest, MovieRequestKind,
    SceneBoundary, SceneError, SceneResources, Stage, Then, WindowAck,
};
use fmv_ff7::script::{FieldScratch, FieldScratchError, FieldScript, Instruction};
use fmv_ff7::story::{evaluate_native_party, NativePartyOp, NativePartyOpError, NativePartyState, StoryState};
use script_common::{data_dir, flevel_path, load_field};

const POLICY: ControlPolicy = ControlPolicy { user_release: true, menu_release: true, actor_selection: true };
const HORIZON: u32 = 4096;

fn field() -> FieldFile {
    let dir = data_dir().expect("FMV_FF7_DATA is required for this explicitly selected owned-data test");
    let mut archive = LgpArchive::open_path(flevel_path(&dir)).expect("read-only original flevel.lgp");
    load_field(&mut archive, "md1stin").expect("original md1stin")
}

fn instruction(script: &FieldScript<'_>, entity: usize, slot: usize, offset: usize) -> Instruction {
    script.disassemble(entity, slot).expect("original script").into_iter()
        .find(|ins| ins.offset == offset).expect("reviewed original instruction site")
}

fn check_source(field: &FieldFile) {
    let script = FieldScript::from_field(field).unwrap();
    assert_eq!(script.entity_count(), 12, "run every original initializer, including the movie helper's RET");
    let at = |entity, slot, offset| instruction(&script, entity, slot, offset);
    assert_eq!((at(0, 0, 0x03D4).mnemonic, at(0, 0, 0x03D4).literal("movie")), ("PMVIE", Some(53)));
    assert_eq!(at(0, 0, 0x03E8).mnemonic, "MOVIE");
    let request = at(0, 0, 0x03E5);
    assert_eq!(request.mnemonic, "REQ", "the movie helper is independently queued, never REQEW or a synthetic root");
    assert_eq!(request.literal("entity"), Some(3));
    for (offset, threshold) in [(0x0556, 20), (0x056E, 664)] {
        let comparison = at(3, 3, offset);
        assert_eq!((comparison.mnemonic, comparison.literal("right"), comparison.literal("op")),
            ("IFSW", Some(threshold), Some(2)), "strictly greater source threshold");
        assert_eq!(comparison.operand("left").map(|operand| (operand.bank, operand.value)), Some((6, 6)));
    }
    for (offset, target) in [(0x0569, 0x0553), (0x058B, 0x056B)] {
        let back = at(3, 3, offset);
        assert_eq!((back.mnemonic, back.jump_target), ("JMPB", Some(target)),
            "each helper poll retains the backward jump's one-field-frame turn");
    }
    for (entity, slot, offset, mnemonic) in [
        (0, 0, 0x0396, "FADE"), (0, 0, 0x03FD, "PRTYE"),
        (3, 3, 0x0553, "MVIEF"), (3, 3, 0x056B, "MVIEF"),
        (3, 3, 0x055E, "NFADE"), (3, 3, 0x0576, "MUSIC"),
        (3, 3, 0x0578, "MULCK"), (3, 3, 0x057A, "AKAO2"),
        (0, 0, 0x048F, "MESSAGE"), (0, 0, 0x049D, "UC"),
        (0, 0, 0x049F, "MENU2"), (0, 0, 0x04A1, "CC"),
    ] { assert_eq!(at(entity, slot, offset).mnemonic, mnemonic); }
    assert_eq!(["character1", "character2", "character3"].map(|name| at(0, 0, 0x03FD).literal(name)),
        [Some(0), Some(254), Some(254)]);
}

/// Explicit test-host contract. 1,792 is the retained owned Steam opening.avi decode count;
/// these fingerprints deliberately identify a simulated host, not the movie bytes or a
/// verified native counter mapping. In this test the counter and presented ordinal are
/// supplied independently. The tick numbers below are not inferred from the 15 fps asset.
fn admitted_test_asset() -> MovieAsset {
    MovieAsset {
        frames: 1792,
        timeline_fingerprint: *blake3::hash(b"entry_movie_real/simulated-host/owned-count1792/no-playback-proof/v1").as_bytes(),
        counter_profile: MovieCounterProfile {
            fingerprint: *blake3::hash(b"entry_movie_real/explicit-monotone-counter/no-native-ordinal-mapping/v1").as_bytes(),
            maximum: 1791,
        },
    }
}

#[derive(Default)]
struct Evidence {
    stage: Vec<(u32, Stage)>,
    audio: Vec<EntryAudio>,
}

impl Evidence {
    fn observe(&mut self, beat: EntryBeat) -> EntryYield {
        let origin = beat.frame.checked_sub(beat.at).expect("relative beat clock");
        self.stage.extend(beat.stage.into_iter().map(|event| (origin + event.at, event.what)));
        self.audio.extend(beat.audio);
        assert!(!matches!(beat.then, EntryYield::Scene(Then::End { how: End::Finished, .. })),
            "the opening director/Cloud polling and pending actor work are not a finished scene");
        beat.then
    }

    fn fade_clear_count(&self) -> usize {
        self.stage.iter().filter(|(_, event)| matches!(event,
            Stage::Fade(FadeOp::Nfade { kind: 0, rgb: [0, 0, 0], frames: 0 }))).count()
    }

    fn helper_audio(&self) -> Vec<(u32, usize, &'static str)> {
        self.audio.iter().filter(|cue| cue.entity == 3)
            .map(|cue| (cue.frame, cue.instruction.offset, cue.instruction.mnemonic)).collect()
    }
}

fn movie(then: EntryYield, expected: MovieRequestKind, offset: u32) -> MovieRequest {
    let EntryYield::Movie(request) = then else { panic!("expected original movie request") };
    assert_eq!((request.kind, request.movie, request.entity, request.instruction_offset), (expected, 53, 0, offset));
    request
}

fn idle_movie(play: &mut EntryScenePlay, evidence: &mut Evidence, tick: u32) {
    let then = evidence.observe(play.advance_to(tick).expect("pending movie retains other jobs"));
    assert!(matches!(then, EntryYield::AwaitExternal | EntryYield::Horizon), "movie wait must remain nonterminal");
    assert_eq!(play.scene().ended(), None);
    assert!(play.pending_movie_request().is_some());
    assert_eq!(play.scene().stats().windows, 0, "MOVIE has not released the director into dialogue");
}

fn helper_counter(play: &EntryScenePlay) -> u16 {
    play.scene().scratch_outcome().unwrap().unwrap().after().read_u16(6, 6).unwrap()
}

/// The observation wakes the retained job without running it. Its saved PC may be the
/// backward JMPB, in which case the next MVIEF is due one field frame later. Check that
/// exact bounded schedule rather than equating the observation tick with the read tick.
fn counter(play: &mut EntryScenePlay, evidence: &mut Evidence, request: MovieRequest, tick: u32, value: u16) -> u32 {
    let then = evidence.observe(play.advance_before(tick).expect("advance strictly before the observation tick"));
    assert!(matches!(then, EntryYield::AwaitExternal | EntryYield::Horizon));
    let previous = helper_counter(play);
    let cues_before = (evidence.fade_clear_count(), evidence.helper_audio().len());
    play.acknowledge_movie(MovieAck::Counter { token: request.token, observation: MovieCounterObservation { value } }).unwrap();
    assert_eq!(helper_counter(play), previous, "a host observation does not execute MVIEF");
    idle_movie(play, evidence, tick);
    assert_eq!(play.movie_counter().value, value);
    let read_tick = if helper_counter(play) == value { tick } else {
        assert_eq!(helper_counter(play), previous, "only the actual MVIEF can replace its prior sample");
        assert_eq!((evidence.fade_clear_count(), evidence.helper_audio().len()), cues_before,
            "a new observation cannot cause a cue before the helper reads it");
        idle_movie(play, evidence, tick + 1);
        tick + 1
    };
    assert_eq!(helper_counter(play), value,
        "the helper's next due MVIEF must read the exact observed counter after at most its one-frame backward jump");
    read_tick
}

/// Executes original entry and source movie cues; all external completions are test-host
/// inputs. Stops before executing the director's post-MOVIE party operation.
fn through_movie_cues(field: &FieldFile, story: &StoryState) -> (EntryScenePlay, Evidence) {
    check_source(field);
    assert_eq!(story.moment, 0, "never rewind a first-control or existing account snapshot");
    assert!(story.supports_native_party(), "this test carries the native state explicitly");
    let before = story.encode();
    let def = reactor_opening_scene();
    assert!(def.cast.iter().all(|actor| actor.start.is_none()), "no invented cast poses");
    let mut play = EntryScenePlay::with_scratch(field, &def, story, 41, SceneResources { gil: None },
        &FieldScratch::cleared_for_entry(), &[0, 1], [0x53; 16], MovieCounterObservation { value: 0 },
        POLICY, Limits::default()).expect("original entry profile with explicit inherited counter zero");
    let mut evidence = Evidence::default();
    let preparing = movie(evidence.observe(play.advance_to(0).unwrap()), MovieRequestKind::Prepare, 0x03D4);
    assert!(evidence.stage.iter().any(|(_, event)| matches!(event,
        Stage::Fade(FadeOp::Fade { kind: 4, rgb: [0, 0, 0], speed: 1, adjust: 0 }))), "visible director initializer fade");
    assert!(evidence.stage.iter().any(|(_, event)| matches!(event,
        Stage::Camera(CameraOp::Scroll { x: 0, y: 40, frames: 0 }))), "visible initial camera instruction");
    for entity in [2, 4, 5, 6, 7, 8, 9, 10, 11] {
        assert!(evidence.stage.iter().any(|(_, event)| matches!(event,
            Stage::Place { entity: placed, .. } if *placed == entity)), "original initializer places entity {entity}");
    }
    // Preparing suspends only the director too: Cloud's selected main can run before the
    // host admits the asset. This proves cooperative retention, not native dispatch timing.
    idle_movie(&mut play, &mut evidence, 0);
    assert!(evidence.stage.iter().any(|(_, event)| matches!(event,
        Stage::Place { entity: 1, x: 3655, y: 27432, triangle: Some(25), .. })), "Cloud's original main runs while Prepare is pending");
    assert_eq!((play.control_state().user_disabled, play.control_state().menu_disabled), (Some(1), Some(1)));
    play.acknowledge_movie(MovieAck::Prepared { token: preparing.token, asset: admitted_test_asset() }).unwrap();
    let playing = movie(evidence.observe(play.advance_to(0).unwrap()), MovieRequestKind::Play, 0x03E8);
    assert_ne!(preparing.token, playing.token);
    idle_movie(&mut play, &mut evidence, 1);
    assert!(evidence.stage.iter().any(|(_, event)| matches!(event,
        Stage::Place { entity: 1, x: 3655, y: 27432, triangle: Some(25), .. })), "Cloud's separate original main supplies his pose");
    assert!(evidence.stage.iter().any(|(_, event)| matches!(event, Stage::Show { entity: 1, visible: false })));
    assert_eq!((play.control_state().user_disabled, play.control_state().menu_disabled), (Some(1), Some(1)));
    assert_eq!(play.scene().stats().handled.get("REQ"), Some(&1), "only the director queued helper 3/3 before MOVIE");
    // Presented ordinal is independent: it must not increment the source MVIEF counter.
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 17 }).unwrap();
    idle_movie(&mut play, &mut evidence, 5);
    assert_eq!(play.movie_counter().value, 0);
    assert_eq!((evidence.fade_clear_count(), evidence.helper_audio().len()), (0, 0));
    let threshold_read = counter(&mut play, &mut evidence, playing, 10, 20);
    assert_eq!(threshold_read, 11, "the already parked helper wakes at JMPB before its next MVIEF");
    assert_eq!((evidence.fade_clear_count(), evidence.helper_audio().len()), (0, 0), "20 does not satisfy >20");
    let fade_read = counter(&mut play, &mut evidence, playing, 12, 21);
    assert_eq!((evidence.fade_clear_count(), evidence.helper_audio().len()), (1, 0));
    assert!(evidence.stage.iter().any(|(frame, event)| *frame == fade_read && matches!(event,
        Stage::Fade(FadeOp::Nfade { kind: 0, .. }))), "independently queued helper executes while director awaits MOVIE");
    counter(&mut play, &mut evidence, playing, 14, 664);
    assert!(evidence.helper_audio().is_empty(), "664 does not satisfy >664");
    let music_read = counter(&mut play, &mut evidence, playing, 16, 665);
    assert_eq!(evidence.helper_audio(), [(music_read, 0x0576, "MUSIC"), (music_read, 0x0578, "MULCK"), (music_read, 0x057A, "AKAO2")]);
    for (offset, operand, value) in [(0x0576, "id", 1), (0x0578, "disabled", 1),
        (0x057A, "op", 201), (0x057A, "p1", 60), (0x057A, "p2", 32767)] {
        assert_eq!(evidence.audio.iter().find(|cue| cue.entity == 3 && cue.instruction.offset == offset)
            .unwrap().instruction.literal(operand), Some(value), "retain raw audio cue, do not infer its mixer semantics");
    }
    idle_movie(&mut play, &mut evidence, 20);
    assert_eq!((evidence.fade_clear_count(), evidence.helper_audio().len()), (1, 3), "helper cues execute once");
    play.acknowledge_movie(MovieAck::Presented { token: playing.token, ordinal: 1791 }).unwrap();
    idle_movie(&mut play, &mut evidence, 21);
    assert_eq!(play.movie_counter().value, 665, "even the final presented ordinal does not change MVIEF or release MOVIE");
    // Synthetic attestation only: no decoder, audio clock, renderer or physical EOF ran.
    play.acknowledge_movie(MovieAck::Completed { token: playing.token }).unwrap();
    assert_eq!(story.encode(), before, "input fixture is never mutated or saved");
    (play, evidence)
}

/// Only an explicitly supported original party input may continue into dialogue/control.
fn through_first_control(field: &FieldFile, story: &StoryState) -> EntryScenePlay {
    let before = story.encode();
    let (mut play, mut evidence) = through_movie_cues(field, story);
    let then = evidence.observe(play.advance_to(HORIZON).expect("source after explicit synthetic completion"));
    let EntryYield::Scene(Then::Window(window)) = then else { panic!("the original director reaches its first dialogue") };
    assert!(window.message.choice.is_none());
    assert!(matches!(window.message.origin, MessageOrigin::Canon { dialog: 30, .. }));
    let Some(ControlBoundary::Scene(SceneBoundary::Window(checkpoint))) = play.boundary() else { panic!("exact window boundary") };
    assert_eq!((checkpoint.entity, checkpoint.instruction_offset, checkpoint.dialog), (0, Some(0x048F), Some(30)));
    let dialogue_frame = play.field_frame();
    play.acknowledge(ControlAck::Window(WindowAck { checkpoint, answer: None })).unwrap();
    let mut control_frames = Vec::new();
    for (offset, operation) in [
        (0x049D, ControlOp::User { disabled: 0 }),
        (0x049F, ControlOp::Menu { disabled: 0 }),
        (0x04A1, ControlOp::Actor { entity: 1 }),
    ] {
        let then = evidence.observe(play.advance_to(HORIZON).expect("retain exact first-control boundary"));
        let EntryYield::Control(checkpoint) = then else { panic!("each executed control instruction yields separately") };
        assert_eq!((checkpoint.entity, checkpoint.instruction_offset, checkpoint.operation), (0, offset, operation));
        control_frames.push(checkpoint.frame);
        assert_eq!(play.scene().ended(), None);
        play.acknowledge(ControlAck::Control(checkpoint)).unwrap();
    }
    assert_eq!(control_frames, [dialogue_frame + 20; 3], "source WAIT20, not elapsed dialogue/movie wall time");
    let first_control = play.field_frame();
    assert!(evidence.stage.iter().any(|(at, event)| matches!(event,
        Stage::Move { entity: 2, x: 3836, y: 29295, frames, .. }
            if *at <= first_control && at.saturating_add(*frames) > first_control)),
        "Barret's asynchronously requested first walk remains in flight at the control cut");
    assert!(!evidence.stage.iter().any(|(at, event)| *at <= first_control && matches!(event,
        Stage::Move { entity: 2, x: 3578, y: 29360, .. })), "the actor's second walk was not drained before first control");
    let then = evidence.observe(play.advance_to(first_control + 1).unwrap());
    assert!(matches!(then, EntryYield::Horizon | EntryYield::Idle), "control handoff retains pending actor work and director polling");
    assert_eq!(play.scene().ended(), None);
    assert_eq!(play.scene().stats().dropped_requests, 0);
    assert!(!play.scene().stats().ignored.contains_key("PMVIE") && !play.scene().stats().ignored.contains_key("MOVIE")
        && !play.scene().stats().ignored.contains_key("MVIEF"), "entry movie opcodes were executed");
    assert_eq!(story.encode(), before, "input fixture is never mutated or saved");
    eprintln!("original entry: movie53 requests, helper counters20/21/664/665, dialogue30 and first controls observed; synthetic host inputs only");
    play
}

#[test]
#[ignore = "needs owned FMV_FF7_DATA; source response with simulated movie observations, not playable opening acceptance"]
fn original_entry_retains_movie_cues_then_refuses_unknown_native_startup_party() {
    let original = StoryState::fresh_reset().into_native_party().unwrap();
    assert_eq!(original.native_party().unwrap().known_main(), None);
    assert_eq!(evaluate_native_party(&original, NativePartyOp::Set([0, 254, 254])),
        Err(NativePartyOpError::UnknownMainSlot(0)), "PRTYE0x03FD reconciles native main slots; reset bytes do not establish them");
    let (mut play, _) = through_movie_cues(&field(), &original);
    let movie_end_frame = play.field_frame();
    let refusal = EntryError::Control(ControlError::Scene(SceneError::Scratch(FieldScratchError::InvalidOutcome)));
    assert_eq!(play.advance_to(HORIZON).err(), Some(refusal.clone()),
        "checked execution refuses the original PRTYE rather than continuing with unknown native main slots");
    assert_eq!(play.scene().stats().unknown.get("PRTYE"), Some(&1), "the real missing-input refusal is retained");
    assert!(play.scene().stats().unresolved > 0);
    assert_eq!(play.field_frame(), movie_end_frame, "the PRTYE site precedes the director's first post-movie WAIT");
    assert_eq!(play.scene().stats().windows, 0, "dialogue30 is never handed out after the refused party operation");
    assert_eq!((play.control_state().user_disabled, play.control_state().menu_disabled, play.control_state().actor),
        (Some(1), Some(1), None), "no first-control handoff follows the refusal");
    assert_ne!(play.scene().ended(), Some(End::Finished));
    assert_eq!(play.scene().scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    assert!(play.scene().current_story().is_none(), "refused source execution cannot authorize a native save");
    let held = (play.field_frame(), play.scene().stats().clone(), play.transcript().to_vec());
    assert_eq!(play.advance_to(HORIZON).err(), Some(refusal), "checked refusal remains sticky");
    assert_eq!((play.field_frame(), play.scene().stats().clone(), play.transcript().to_vec()), held,
        "retry does not consume further source work or publish a new accepted input");
    eprintln!("this unknown-party input cannot pass native acceptance: PRTYE at md1stin entity0 offset0x03FD requires known main slots; none fabricated");
}

fn accepted_first_control(play: &EntryScenePlay) -> StoryState {
    assert!(play.scene().stats().unknown.is_empty(), "unsupported source instruction prevents native acceptance");
    assert_eq!(play.scene().stats().unresolved, 0, "unknown original inputs prevent native acceptance");
    let after = play.scene().current_story().expect("complete evaluated afterimage, never published by this test");
    assert_eq!(after.moment, 1);
    assert_eq!([9, 10, 11].map(|address| after.known_byte(3, address)), [Some(0), Some(255), Some(255)]);
    assert_eq!(after.native_party().unwrap().known_locked(0), Ok(Some(true)));
    assert!(play.scene().resources().gil_ops.is_empty(), "no invented economic input");
    after
}

#[test]
#[ignore = "needs owned FMV_FF7_DATA and reviewed FMV_ENTRY_STORY native moment-0 blob; simulated host observations only"]
fn original_entry_reaches_first_control_with_an_explicit_native_moment_zero_fixture() {
    let path = std::env::var_os("FMV_ENTRY_STORY").expect(
        "full native first-control acceptance needs a reviewed original moment-0 v4 blob in FMV_ENTRY_STORY; no native startup fixture is inferred");
    let file = std::fs::File::open(path).expect("read-only native moment-0 fixture");
    assert!(file.metadata().unwrap().len() <= 32 * 1024, "bounded StoryState fixture");
    let mut bytes = Vec::new();
    use std::io::Read;
    file.take(32 * 1024 + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 32 * 1024);
    let story = StoryState::decode(&bytes).expect("canonical reviewed native StoryState blob");
    assert_eq!(story.moment, 0, "do not rewind any existing save");
    assert!(story.native_party().and_then(|party| party.known_main()).is_some(), "original native main slots must be explicit");
    let play = through_first_control(&field(), &story);
    accepted_first_control(&play);
}

/// Test-only preimage at the original PC normal-new-game reset boundary, not a new live
/// account constructor or proof of arbitrary startup state at field dispatch. The reviewed
/// reset at 0x60BB9B writes raw/main [0,255,255] and availability u16=1; it does not establish
/// the independent lock mask. Dispatcher state 22 calls reset before selecting field 116.
/// Later helpers between reset and field entry have not been exhaustively proven inert.
/// Primary bytes: owned resources/ff7_1.02/ff7_en, reset file offset 0x20AD9B, 351 bytes.
/// Loop 0x60BC29..0x60BC67 fills main/raw slots with 255; 0x60BC69/0x60BC79 select Cloud,
/// and 0x60BC70 stores availability 1. The clear loop excludes SaveWork+0x10A4 (locks).
/// State 22 dispatches to 0x409742, calls reset at 0x40975D, then 0x4081E1 -> 0x408074 (field 116).
///
/// Original binary SHA256:
/// 26998200234f7a49a700bdc1ae5d0a43fd001cc909871a366cf82bdffaa351c9
/// Reset slice SHA256:
/// 4e80034aec8421b6d38e2f4c26e913c0820dfedc19b4866efe19b1f4d3e56c6e
fn evidenced_pc_reset_input() -> StoryState {
    let mut story = StoryState::fresh_reset().into_native_party().unwrap();
    for (address, value) in [(9, 0), (10, 255), (11, 255)] {
        story.write_byte(3, address, value).unwrap();
    }
    *story.native_party_mut().unwrap() = NativePartyState::from_parts(
        [0, 255, 255], 0b111, (1, u16::MAX), (0, 0)).unwrap();
    story
}

#[test]
#[ignore = "needs owned FMV_FF7_DATA; evidenced PC reset-boundary input with simulated movie observations, not live admission"]
fn original_entry_derives_first_control_from_the_evidenced_pc_reset_boundary() {
    let original = evidenced_pc_reset_input();
    assert_eq!(original.moment, 0);
    assert_eq!(original.native_party().unwrap().known_main(), Some([0, 255, 255]));
    assert_eq!(original.native_party().unwrap().availability_bits(), (1, u16::MAX));
    assert_eq!(original.native_party().unwrap().locking_bits(), (0, 0), "no unsupported reset lock-mask claim");
    let play = through_first_control(&field(), &original);
    let after = accepted_first_control(&play);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 255, 255]));
    assert_eq!(after.native_party().unwrap().availability_bits(), (1, u16::MAX));
    assert_eq!(after.native_party().unwrap().locking_bits(), (1, 1), "only source MMBLK0 establishes its lock bit");
    // These values are derived by actually running the source, never copied from the
    // port's first-control constructor or used to fill an existing save's unknown bytes.
    for (address, value) in [(3, 50), (4, 30), (5, 10), (6, 0), (80, 100), (81, 100), (82, 100), (83, 100)] {
        assert_eq!(after.known_byte(1, address), Some(value), "authored opening assignment at bank1 address{address}");
    }
    eprintln!("source first-control afterimage accepted from the evidenced PC reset-boundary party input; no startup-helper, playback or live-admission claim");
}
