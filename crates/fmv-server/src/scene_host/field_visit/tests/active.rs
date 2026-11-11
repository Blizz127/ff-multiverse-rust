//! Private active-visit replay decisions, without storage or runtime admission. Applying a
//! job below merely materializes its proposed images; it never simulates a durable ACK.

use super::*;
use super::super::ActiveVisitBoundary;
use crate::saver::SceneBatchJob;
use fmv_core::Pos;
use fmv_persist::SceneOperationKind;
use fmv_protocol::{SceneOrigin, SceneReplayEvent, MAX_SCENE_REPLAY_EVENTS};
use serde::{Deserialize, Serialize};

const INSTANCE: [u8; 32] = [61; 32];
const SEED: u32 = 37;

/// Mirror only the private encoding for controlled corruption tests. Check a byte-exact
/// round trip first: Value's map-key order would otherwise exercise only canonicalization.
#[derive(Serialize, Deserialize)]
struct ReplayInput {
    version: u32,
    account: AccountId,
    visit: FieldVisitV1,
    origin: Option<SceneOrigin>,
    events: Vec<SceneReplayEvent>,
}

fn payload(current: &SceneState) -> ReplayInput {
    let bytes = &current.checkpoint.as_ref().unwrap().replay;
    let input = serde_json::from_slice::<ReplayInput>(bytes).unwrap();
    assert_eq!(serde_json::to_vec(&input).unwrap(), *bytes, "the test mirror preserves the canonical input encoding");
    input
}

fn replace_payload(current: &mut SceneState, input: &ReplayInput) {
    current.checkpoint.as_mut().unwrap().replay = serde_json::to_vec(input).unwrap();
    // Keep the public revision consistent when testing replay validation itself, rather
    // than obtaining a rejection merely because an appended event has no matching revision.
    current.field_visit.as_mut().unwrap().revision = input.visit.revision + 1 + u64::try_from(input.events.len()).unwrap();
}

fn apply(job: &SceneBatchJob, expected: &SceneState) -> SceneState {
    assert_eq!(job.members.len(), 1);
    let member = job.members.first().unwrap();
    assert_eq!(&member.expected, expected, "the job retains the entire exact predecessor");
    assert!(member.battle.is_none() && member.reservation.is_none());
    let mut after = expected.clone();
    if let Some(story) = &member.next_story { after.pair.story.blob.clone_from(story); }
    if let Some(gil) = member.next_gil { after.pair.character.as_mut().unwrap().gil = gil; }
    after.checkpoint.clone_from(&member.checkpoint);
    after.reservation.clone_from(&member.reservation);
    after.field_visit = Some(member.next_visit.clone().expect("every active transition carries the visit"));
    after
}

fn attach(source: &IdleVisitSource<'_>, def: &SceneDef, idle: &SceneState) -> SceneState {
    let job = source.attach_scene_job(def, idle, INSTANCE, SEED, None).unwrap();
    assert_eq!(job.operation.kind, SceneOperationKind::AttachVisit);
    assert_eq!((job.operation.instance, job.operation.ordinal, job.operation.attempt), (INSTANCE, 0, 0));
    assert!(job.members.first().unwrap().next_story.is_none() && job.members.first().unwrap().next_gil.is_none());
    apply(&job, idle)
}

fn window(source: &IdleVisitSource<'_>, def: &SceneDef, current: &SceneState) -> (u32, Vec<u8>, Option<[u8; 2]>) {
    match source.active_scene_boundary(def, current).unwrap() {
        ActiveVisitBoundary::Window { step, identity, choices } => (step, identity, choices),
        ActiveVisitBoundary::Complete => panic!("expected an unacknowledged window"),
    }
}

fn acknowledgment(source: &IdleVisitSource<'_>, def: &SceneDef, current: &SceneState, answer: Option<u8>) -> SceneReplayEvent {
    let (step, identity, _) = window(source, def, current);
    SceneReplayEvent::Window { step, identity, answer }
}

fn advance(source: &IdleVisitSource<'_>, def: &SceneDef, current: &SceneState, answer: Option<u8>) -> SceneState {
    let ack = acknowledgment(source, def, current, answer);
    let job = source.advance_scene_job(def, current, ack).unwrap();
    assert_eq!(job.operation.kind, SceneOperationKind::AdvanceVisit);
    assert!(job.members.first().unwrap().next_story.is_none() && job.members.first().unwrap().next_gil.is_none());
    apply(&job, current)
}

fn assert_uncommitted(idle: &SceneState, current: &SceneState, advances: u64) {
    assert_eq!(current.pair, idle.pair, "accepted input is saved before authored effects are committed");
    let mut visit = idle.field_visit.clone().unwrap();
    visit.revision += 1 + advances;
    visit.active_continuation = Some(INSTANCE);
    assert_eq!(current.field_visit.as_ref(), Some(&visit), "scratch, entry, anchor and visit identity remain exact");
    let checkpoint = current.checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.instance, INSTANCE);
    assert_eq!(checkpoint.original_story, idle.pair.story.blob);
    assert_eq!(checkpoint.original_gil, idle.pair.character.as_ref().unwrap().gil);
    assert_eq!(checkpoint.seed, SEED);
    assert_eq!(checkpoint.phase, ScenePhase::Resume { segment: 0 });
    assert_eq!(checkpoint.consumed_battles, 0);
    assert!(!checkpoint.replay.is_empty());
    assert!(current.reservation.is_none());
}

fn initial_scratch() -> FieldScratch {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 19, 0).unwrap(); // ASK reads the existing cursor before opening.
    scratch.write_masked_u8(5, 31, 0x80, 0x80).unwrap();
    scratch
}

fn assert_replay_refused(source: &IdleVisitSource<'_>, def: &SceneDef, current: &SceneState) {
    assert!(source.active_scene_boundary(def, current).is_err());
    assert!(source.complete_scene_job(def, current).is_err());
}

#[test]
fn attach_and_each_ack_preserve_all_effects_until_a_complete_stored_prefix() {
    let body = [byte(1, 4, 47), gold(0x3A, 7), vec![0x82, 0x50, 31, 0],
        vec![0x40, 0, 0], vec![0x48, 5, 0, 0, 0, 1, 19], copy(13, 100, 5, 19), word(2, 0, 37)].concat();
    let field = field(&[], &body);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let scratch = initial_scratch();
    let idle = state(&source, &story(), &scratch);
    let attached = attach(&source, &def, &idle);
    assert_uncommitted(&idle, &attached, 0);
    assert_eq!(window(&source, &def, &attached).0, 0);
    assert_eq!(window(&source, &def, &attached).2, None);
    assert!(source.complete_scene_job(&def, &attached).is_err());

    let first_ack = acknowledgment(&source, &def, &attached, None);
    let first_job = source.advance_scene_job(&def, &attached, first_ack.clone()).unwrap();
    assert_eq!(first_job.operation.ordinal, 1);
    let first = apply(&first_job, &attached);
    assert_uncommitted(&idle, &first, 1);
    assert_eq!(window(&source, &def, &first).0, 1);
    assert_eq!(window(&source, &def, &first).2, Some([0, 1]));
    assert!(source.complete_scene_job(&def, &first).is_err());
    assert!(source.advance_scene_job(&def, &first, first_ack).is_err(), "an old window cannot acknowledge the new one");

    let final_ack = acknowledgment(&source, &def, &first, Some(1));
    let final_job = source.advance_scene_job(&def, &first, final_ack.clone()).unwrap();
    assert_eq!(final_job.operation.ordinal, 2);
    let finished = apply(&final_job, &first);
    assert_uncommitted(&idle, &finished, 2);
    assert!(matches!(source.active_scene_boundary(&def, &finished), Ok(ActiveVisitBoundary::Complete)));
    assert!(source.advance_scene_job(&def, &finished, final_ack).is_err(), "no trailing ACK after the terminal prefix");

    let completion = source.complete_scene_job(&def, &finished).unwrap();
    assert_eq!(completion.operation.kind, SceneOperationKind::CompleteVisit);
    assert_eq!((completion.operation.instance, completion.operation.ordinal, completion.operation.attempt), (INSTANCE, 0, 0));
    let completed = apply(&completion, &finished);
    let mut expected = idle.clone();
    let mut after_story = story();
    after_story.write_byte(1, 4, 47).unwrap();
    after_story.write_byte(13, 100, 1).unwrap();
    after_story.moment = 37;
    expected.pair.story.blob = after_story.encode();
    expected.pair.character.as_mut().unwrap().gil = 233;
    let visit = expected.field_visit.as_mut().unwrap();
    visit.revision += 4;
    visit.scratch_values[19] = 1;
    visit.scratch_values[31] = 0x81;
    visit.scratch_known_masks[31] = 0x81;
    assert_eq!(completed, expected, "completion changes only the jointly interpreted afterimages and metadata");
    assert_eq!(source.complete_scene_job(&def, &finished).unwrap(), completion, "same prefix produces the same immutable retry decision");
    assert!(source.complete_scene_job(&def, &completed).is_err(), "a cleared continuation cannot be completed again");
}

#[test]
fn every_window_identity_and_choice_must_match_the_unacknowledged_boundary() {
    let field = field(&[], &[vec![0x40, 0, 0], vec![0x48, 5, 0, 0, 0, 1, 19]].concat());
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    let attached = attach(&source, &def, &idle);
    let (step, identity, _) = window(&source, &def, &attached);
    let mut wrong_identity = identity.clone(); wrong_identity.push(0);
    for ack in [
        SceneReplayEvent::Window { step: step + 1, identity: identity.clone(), answer: None },
        SceneReplayEvent::Window { step, identity: wrong_identity, answer: None },
        SceneReplayEvent::Window { step, identity: identity.clone(), answer: Some(0) },
        SceneReplayEvent::Battle { ordinal: 1, identity, formation: 300, progress: idle.pair.story.blob.clone(), gil: 240 },
    ] { assert!(source.advance_scene_job(&def, &attached, ack).is_err()); }
    let question = advance(&source, &def, &attached, None);
    for answer in [None, Some(2), Some(255)] {
        let ack = acknowledgment(&source, &def, &question, answer);
        assert!(source.advance_scene_job(&def, &question, ack).is_err(), "ASK never invents or clamps {answer:?}");
    }
    let answered = advance(&source, &def, &question, Some(0));
    assert!(matches!(source.active_scene_boundary(&def, &answered), Ok(ActiveVisitBoundary::Complete)));
    assert!(source.complete_scene_job(&def, &answered).is_ok());
    assert_uncommitted(&idle, &question, 1);
}

#[test]
fn current_state_and_checkpoint_tampering_never_reinterpret_an_attached_visit() {
    let field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    let attached = attach(&source, &def, &idle);
    for kind in ["account", "story", "gil", "visit", "revision", "source", "entry", "anchor", "scratch", "knowledge",
        "seed", "original-story", "original-gil", "instance", "checkpoint-source", "scene", "phase", "consumed", "payload"] {
        let mut altered = attached.clone();
        match kind {
            "account" => { altered.pair.story.account_id = AccountId(8); altered.pair.character.as_mut().unwrap().account_id = AccountId(8); },
            "story" => altered.pair.story.blob = StoryState::at(36).into_extended().unwrap().encode(),
            "gil" => altered.pair.character.as_mut().unwrap().gil += 1,
            "visit" => altered.field_visit.as_mut().unwrap().visit_id = [8; 16],
            "revision" => altered.field_visit.as_mut().unwrap().revision += 1,
            "source" => altered.field_visit.as_mut().unwrap().source_identity = [88; 32],
            "entry" => altered.field_visit.as_mut().unwrap().entry.arrival.push(1),
            "anchor" => altered.field_visit.as_mut().unwrap().anchor.push(1),
            "scratch" => altered.field_visit.as_mut().unwrap().scratch_values[19] = 1,
            "knowledge" => altered.field_visit.as_mut().unwrap().scratch_known_masks[20] = 1,
            "seed" => altered.checkpoint.as_mut().unwrap().seed += 1,
            "original-story" => altered.checkpoint.as_mut().unwrap().original_story = StoryState::at(36).into_extended().unwrap().encode(),
            "original-gil" => altered.checkpoint.as_mut().unwrap().original_gil += 1,
            "instance" => {
                altered.checkpoint.as_mut().unwrap().instance = [9; 32];
                altered.field_visit.as_mut().unwrap().active_continuation = Some([9; 32]);
            },
            "checkpoint-source" => altered.checkpoint.as_mut().unwrap().source_hash = [99; 32],
            "scene" => altered.checkpoint.as_mut().unwrap().scene_key.push_str("-changed"),
            "phase" => altered.checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 1 },
            "consumed" => altered.checkpoint.as_mut().unwrap().consumed_battles = 1,
            "payload" => altered.checkpoint.as_mut().unwrap().replay = vec![0, 255, 4],
            _ => unreachable!(),
        }
        assert!(source.active_scene_boundary(&def, &altered).is_err(), "altered {kind}");
        assert!(source.complete_scene_job(&def, &altered).is_err(), "altered {kind}");
    }
    assert_uncommitted(&idle, &attached, 0);
}

#[test]
fn changed_source_profile_or_definition_cannot_resume_the_old_prefix() {
    let field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    let attached = attach(&source, &def, &idle);
    let changed_field = super::field(&[], &[vec![0x40, 0, 0], byte(5, 31, 1)].concat());
    let other_bytes = field_with_tag(&[], &[0x40, 0, 0], 1);
    let init = [ScriptRef::new(0, 0)];
    for changed in [
        IdleVisitSource::new(KEY, &changed_field, &[], PROFILE).unwrap(),
        IdleVisitSource::new(KEY, &other_bytes, &[], PROFILE).unwrap(),
        IdleVisitSource::new(KEY, &field, &[], b"replacement-profile").unwrap(),
        IdleVisitSource::new(KEY, &field, &init, PROFILE).unwrap(),
    ] { assert_replay_refused(&changed, &def, &attached); }
    let mut root = def.clone(); root.root = vec![Step::Await(ScriptRef::new(0, 2))];
    let mut trigger = def.clone(); trigger.trigger.moments = 35..=36;
    let mut name = def.clone(); name.id.push_str("-other");
    for changed in [root, trigger, name] { assert_replay_refused(&source, &changed, &attached); }
}

#[test]
fn battles_travel_and_skipped_state_cannot_be_attached_or_crossed_by_an_ack() {
    for effect in [vec![0x70, 0, 44, 1], vec![0x60, 1, 0, 0, 0, 0, 0, 0, 0, 0], vec![0x3F], vec![0x58, 0, 1, 0, 1]] {
        let no_window = field(&[], &effect);
        let source = IdleVisitSource::new(KEY, &no_window, &[], PROFILE).unwrap();
        let idle = state(&source, &story(), &initial_scratch());
        assert!(source.attach_scene_job(&definition(), &idle, INSTANCE, SEED, None).is_err(), "immediate {effect:?}");

        let after_window = field(&[], &[vec![0x40, 0, 0], effect.clone()].concat());
        let source = IdleVisitSource::new(KEY, &after_window, &[], PROFILE).unwrap();
        let idle = state(&source, &story(), &initial_scratch());
        let attached = attach(&source, &definition(), &idle);
        let ack = acknowledgment(&source, &definition(), &attached, None);
        assert!(source.advance_scene_job(&definition(), &attached, ack).is_err(), "next boundary {effect:?}");
        assert!(source.complete_scene_job(&definition(), &attached).is_err());
        assert_uncommitted(&idle, &attached, 0);
    }
}

#[test]
fn unknown_inputs_remain_unknown_and_late_faults_cannot_commit_a_prefix() {
    for read in [copy(3, 80, 5, 22), copy(5, 22, 1, 99)] {
        let immediate = field(&[], &read);
        let source = IdleVisitSource::new(KEY, &immediate, &[], PROFILE).unwrap();
        let idle = state(&source, &story(), &initial_scratch());
        assert!(source.attach_scene_job(&definition(), &idle, INSTANCE, SEED, None).is_err());
        let delayed = field(&[], &[byte(1, 4, 47), vec![0x40, 0, 0], read, byte(5, 22, 0), byte(1, 99, 0)].concat());
        let source = IdleVisitSource::new(KEY, &delayed, &[], PROFILE).unwrap();
        let idle = state(&source, &story(), &initial_scratch());
        let attached = attach(&source, &definition(), &idle);
        let ack = acknowledgment(&source, &definition(), &attached, None);
        assert!(source.advance_scene_job(&definition(), &attached, ack).is_err());
        assert_uncommitted(&idle, &attached, 0);
    }
    let ask = field(&[], &[0x48, 5, 0, 0, 0, 1, 19]);
    let source = IdleVisitSource::new(KEY, &ask, &[], PROFILE).unwrap();
    let unknown_cursor = state(&source, &story(), &FieldScratch::unknown());
    assert!(source.attach_scene_job(&definition(), &unknown_cursor, INSTANCE, SEED, None).is_err());
}

#[test]
fn zero_window_completion_and_native_v4_keep_independent_knowledge_masks() {
    let body = [vec![0x82, 0x60, 31, 0], byte(13, 100, 7), gold(0x3A, 1)].concat();
    let field = field(&[], &body);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    for before_story in [story(), story().into_native_party().unwrap()] {
        let idle = state(&source, &before_story, &initial_scratch());
        let attached = attach(&source, &definition(), &idle);
        assert_uncommitted(&idle, &attached, 0);
        assert!(matches!(source.active_scene_boundary(&definition(), &attached), Ok(ActiveVisitBoundary::Complete)));
        let job = source.complete_scene_job(&definition(), &attached).unwrap();
        let completed = apply(&job, &attached);
        let after_story = StoryState::decode(&completed.pair.story.blob).unwrap();
        assert_eq!(after_story.native_party(), before_story.native_party(), "unknown native party fields are never inferred from moment");
        assert_eq!(after_story.known_byte(13, 100), Some(7));
        assert_eq!(after_story.known_mask(13, 101), 0x80);
        let after_scratch = next_scratch(&completed);
        assert_eq!(after_scratch.known_masks()[31], 0x81);
        assert_eq!(after_scratch.values()[31], 0x81);
        assert_eq!(after_scratch.known_masks()[20], 0);
        assert_eq!(completed.pair.character.as_ref().unwrap().gil, 239);
        assert!(completed.checkpoint.is_none());
        assert_eq!(completed.field_visit.as_ref().unwrap().active_continuation, None);
        assert_eq!(completed.field_visit.as_ref().unwrap().revision, 14);
    }
}

#[test]
fn missing_legacy_active_and_superseded_visit_inputs_are_refused() {
    let field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    assert!(source.attach_scene_job(&def, &idle, [0; 32], SEED, None).is_err());
    let attached = attach(&source, &def, &idle);
    assert!(source.attach_scene_job(&def, &attached, [62; 32], SEED, None).is_err());
    for kind in ["missing", "legacy", "reservation", "active", "overflow", "malformed"] {
        let mut invalid = idle.clone();
        match kind {
            "missing" => invalid.field_visit = None,
            "legacy" => invalid.pair.story.blob = vec![1, 36, 0],
            "reservation" => invalid.reservation = Some(SceneReservation { instance: [4; 32], ordinal: 0, attempt: 0, slot: 1 }),
            "active" => invalid.field_visit.as_mut().unwrap().active_continuation = Some([5; 32]),
            "overflow" => invalid.field_visit.as_mut().unwrap().revision = u64::MAX,
            "malformed" => invalid.field_visit.as_mut().unwrap().scratch_values[20] = 1,
            _ => unreachable!(),
        }
        assert!(source.attach_scene_job(&def, &invalid, INSTANCE, SEED, None).is_err(), "{kind}");
    }
    let mut superseded = attached.clone();
    let visit = superseded.field_visit.as_mut().unwrap();
    visit.visit_id = [11; 16];
    visit.revision = 0;
    visit.active_continuation = None;
    assert_replay_refused(&source, &def, &superseded);
}

#[test]
fn a_scene_without_dynamic_origin_rejects_an_unexpected_or_nonfinite_origin() {
    let field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let idle = state(&source, &story(), &initial_scratch());
    for origin in [
        SceneOrigin { pos: Pos::new(0.0, 0.0, 0.0), facing: 0.0, triangle: Some(0) },
        SceneOrigin { pos: Pos::new(f32::NAN, 0.0, 0.0), facing: 0.0, triangle: Some(0) },
    ] {
        assert!(source.attach_scene_job(&definition(), &idle, INSTANCE, SEED, Some(origin)).is_err());
    }
}

#[test]
fn stored_prefix_events_are_revalidated_even_with_coherent_lengths_and_revisions() {
    let field = field(&[], &[vec![0x40, 0, 0], vec![0x48, 5, 0, 0, 0, 1, 19], byte(1, 4, 47)].concat());
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    let first = advance(&source, &def, &attach(&source, &def, &idle), None);
    let finished = advance(&source, &def, &first, Some(1));
    assert!(source.complete_scene_job(&def, &finished).is_ok());
    for kind in ["identity", "step", "missing-answer", "out-of-range", "plain-answer", "duplicate", "trailing", "missing-first"] {
        let mut altered = finished.clone();
        let mut input = payload(&altered);
        match kind {
            "identity" => {
                let SceneReplayEvent::Window { identity, .. } = input.events.first_mut().unwrap() else { unreachable!() };
                *identity.first_mut().unwrap() ^= 1;
            },
            "step" => {
                let SceneReplayEvent::Window { step, .. } = input.events.first_mut().unwrap() else { unreachable!() };
                *step = 1;
            },
            "missing-answer" | "out-of-range" => {
                let SceneReplayEvent::Window { answer, .. } = input.events.get_mut(1).unwrap() else { unreachable!() };
                *answer = if kind == "missing-answer" { None } else { Some(2) };
            },
            "plain-answer" => {
                let SceneReplayEvent::Window { answer, .. } = input.events.first_mut().unwrap() else { unreachable!() };
                *answer = Some(0);
            },
            "duplicate" => input.events.insert(1, input.events.first().unwrap().clone()),
            "trailing" => {
                let mut extra = input.events.last().unwrap().clone();
                let SceneReplayEvent::Window { step, .. } = &mut extra else { unreachable!() };
                *step = 2;
                input.events.push(extra);
            },
            "missing-first" => { input.events.remove(0); },
            _ => unreachable!(),
        }
        replace_payload(&mut altered, &input);
        assert!(source.active_scene_boundary(&def, &altered).is_err(), "stored {kind}");
        assert!(source.complete_scene_job(&def, &altered).is_err(), "stored {kind}");
    }
    // A shorter, otherwise valid prefix is still a pending question, never completion.
    let mut incomplete = finished.clone();
    let mut input = payload(&incomplete);
    input.events.pop();
    replace_payload(&mut incomplete, &input);
    assert_eq!(window(&source, &def, &incomplete).2, Some([0, 1]));
    assert!(source.complete_scene_job(&def, &incomplete).is_err());
}

#[test]
fn private_context_changes_cannot_resume_even_with_matching_public_images() {
    let field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let def = definition();
    let idle = state(&source, &story(), &initial_scratch());
    let attached = attach(&source, &def, &idle);
    for kind in ["account", "anchor", "entry", "scratch", "revision", "origin", "version"] {
        let mut altered = attached.clone();
        let mut input = payload(&altered);
        match kind {
            "account" => {
                input.account = AccountId(8);
                altered.pair.story.account_id = input.account;
                altered.pair.character.as_mut().unwrap().account_id = input.account;
            },
            "anchor" => { input.visit.anchor.push(1); altered.field_visit.as_mut().unwrap().anchor.clone_from(&input.visit.anchor); },
            "entry" => { input.visit.entry.arrival.push(1); altered.field_visit.as_mut().unwrap().entry.clone_from(&input.visit.entry); },
            "scratch" => { input.visit.scratch_values[19] = 1; altered.field_visit.as_mut().unwrap().scratch_values[19] = 1; },
            "revision" => input.visit.revision += 1,
            "origin" => input.origin = Some(SceneOrigin { pos: Pos::new(0.0, 0.0, 0.0), facing: 0.0, triangle: Some(0) }),
            "version" => input.version += 1,
            _ => unreachable!(),
        }
        replace_payload(&mut altered, &input);
        assert!(source.active_scene_boundary(&def, &altered).is_err(), "original {kind}");
        assert!(source.complete_scene_job(&def, &altered).is_err(), "original {kind}");
    }
}

#[test]
fn attach_reserves_revision_capacity_for_remaining_acknowledgments_and_completion() {
    let waiting_field = field(&[], &[0x40, 0, 0]);
    let source = IdleVisitSource::new(KEY, &waiting_field, &[], PROFILE).unwrap();
    let mut idle = state(&source, &story(), &initial_scratch());
    let required = u64::try_from(MAX_SCENE_REPLAY_EVENTS).unwrap() + 2; // Attach, every allowed ACK, Complete.
    idle.field_visit.as_mut().unwrap().revision = u64::MAX - required + 1;
    assert!(source.attach_scene_job(&definition(), &idle, INSTANCE, SEED, None).is_err());
    idle.field_visit.as_mut().unwrap().revision = u64::MAX - required;
    let attached = attach(&source, &definition(), &idle);
    let finished = advance(&source, &definition(), &attached, None);
    assert!(source.complete_scene_job(&definition(), &finished).is_ok());

    let finished_field = field(&[], &[]);
    let source = IdleVisitSource::new(KEY, &finished_field, &[], PROFILE).unwrap();
    let mut idle = state(&source, &story(), &initial_scratch());
    idle.field_visit.as_mut().unwrap().revision = u64::MAX - 1;
    assert!(source.attach_scene_job(&definition(), &idle, INSTANCE, SEED, None).is_err(),
        "even a zero-window scene must be able to clear its attached checkpoint");
    idle.field_visit.as_mut().unwrap().revision = u64::MAX - 2;
    let attached = attach(&source, &definition(), &idle);
    assert!(matches!(source.active_scene_boundary(&definition(), &attached), Ok(ActiveVisitBoundary::Complete)));
    let completed = apply(&source.complete_scene_job(&definition(), &attached).unwrap(), &attached);
    assert_eq!(completed.field_visit.as_ref().unwrap().revision, u64::MAX);
    assert!(completed.checkpoint.is_none());
}

#[test]
fn the_last_permitted_ack_can_save_finished_but_never_another_window() {
    for windows in [MAX_SCENE_REPLAY_EVENTS, MAX_SCENE_REPLAY_EVENTS + 1] {
        let mut body = [0x40, 0, 0].repeat(windows);
        body.extend_from_slice(&byte(5, 31, 1));
        let field = field(&[], &body);
        let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
        let def = definition();
        let idle = state(&source, &story(), &initial_scratch());
        let mut current = attach(&source, &def, &idle);
        for step in 0..MAX_SCENE_REPLAY_EVENTS - 1 {
            assert_eq!(window(&source, &def, &current).0, u32::try_from(step).unwrap());
            current = advance(&source, &def, &current, None);
        }
        let ack = acknowledgment(&source, &def, &current, None);
        let last = source.advance_scene_job(&def, &current, ack);
        if windows == MAX_SCENE_REPLAY_EVENTS {
            let finished = apply(&last.unwrap(), &current);
            assert!(matches!(source.active_scene_boundary(&def, &finished), Ok(ActiveVisitBoundary::Complete)));
            let completed = apply(&source.complete_scene_job(&def, &finished).unwrap(), &finished);
            assert_eq!(next_scratch(&completed).read_u8(5, 31), Ok(1));
            assert!(completed.checkpoint.is_none());
        } else {
            assert!(last.is_err(), "ACK128 cannot strand an account at unacknowledgeable window129");
            assert!(source.complete_scene_job(&def, &current).is_err());
            assert_uncommitted(&idle, &current, u64::try_from(MAX_SCENE_REPLAY_EVENTS - 1).unwrap());
        }
    }
}
