//! Maximum-size opaque visit payloads retain their stronger character schema through
//! the real FIFO, both visit transaction families, later ordinary rewards, and restart.

use fmv_ff7::script::{FieldScratch, FieldScratchRun};
use fmv_persist::{SceneOperation, SceneOperationKind};

use super::*;
use crate::saver::{persist_scene_batch, SceneBatchJob, SceneBatchMember};

const INSTANCE: [u8; 32] = [91; 32];
const PAYLOAD_BYTES: usize = 8 * 1024;

fn scene_operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: 0, attempt: 0, kind }
}

fn attach(before: &SceneState) -> SceneBatchJob {
    let mut visit = before.field_visit.clone().unwrap();
    visit.revision += 1;
    visit.active_continuation = Some(INSTANCE);
    SceneBatchJob {
        operation: scene_operation(SceneOperationKind::AttachVisit),
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: None, next_gil: None,
            checkpoint: Some(SceneCheckpoint {
                instance: INSTANCE, scene_key: "fixture/large-visit-talk".into(), source_hash: [73; 32],
                replay_version: 1, original_story: before.pair.story.blob.clone(),
                original_gil: character(before).gil, seed: 7, replay: vec![1, 0],
                phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
            }),
            reservation: None, battle: None, next_visit: Some(visit),
        }],
    }
}

fn advance_scene(before: &SceneState) -> SceneBatchJob {
    let mut visit = before.field_visit.clone().unwrap();
    visit.revision += 1;
    let mut checkpoint = before.checkpoint.clone().unwrap();
    checkpoint.replay.extend_from_slice(&[1, 255]);
    SceneBatchJob {
        operation: SceneOperation { ordinal: 1, ..scene_operation(SceneOperationKind::AdvanceVisit) },
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: None, next_gil: None,
            checkpoint: Some(checkpoint), reservation: None, battle: None, next_visit: Some(visit),
        }],
    }
}

fn complete(before: &SceneState) -> SceneBatchJob {
    let mut story = StoryState::decode(&before.pair.story.blob).unwrap();
    story.add_byte_checked(1, 4, -3, 0, 100).unwrap();
    let mut visit = before.field_visit.clone().unwrap();
    let scratch = FieldScratch::from_parts(visit.scratch_values, visit.scratch_known_masks).unwrap();
    let mut run = FieldScratchRun::new(&scratch);
    run.write_masked_u8(5, 31, 1, 1).unwrap();
    let outcome = run.finish().unwrap();
    let mut after_scratch = scratch.clone();
    outcome.apply_to(&mut after_scratch, &scratch).unwrap();
    visit.scratch_values = *after_scratch.values();
    visit.scratch_known_masks = *after_scratch.known_masks();
    visit.revision += 1;
    visit.active_continuation = None;
    SceneBatchJob {
        operation: scene_operation(SceneOperationKind::CompleteVisit),
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: Some(story.encode()), next_gil: Some(character(before).gil - 1),
            checkpoint: None, reservation: None, battle: None, next_visit: Some(visit),
        }],
    }
}

fn assert_payloads(state: &SceneState, arrival: &[u8], anchor: &[u8]) {
    assert_eq!(character(state).schema_version, 5, "a large visit never downgrades its character envelope");
    let visit = state.field_visit.as_ref().unwrap();
    assert_eq!(visit.entry.arrival, arrival);
    assert_eq!(visit.anchor, anchor);
    assert_eq!(visit.entry.arrival.len(), PAYLOAD_BYTES);
    assert_eq!(visit.anchor.len(), PAYLOAD_BYTES);
}

#[tokio::test]
async fn maximum_entry_and_anchor_survive_every_fifo_visit_transition_reward_and_restart_retry() {
    let (dir, store, account) = fixture();
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())), Arc::new(adapter()));
    let handle = saver.handle();
    let before = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap().pop().unwrap();
    let small = entry(&before);
    let mut next = small.next().clone();
    let arrival = vec![255; PAYLOAD_BYTES];
    let anchor = vec![254; PAYLOAD_BYTES];
    let visit = next.field_visit.as_mut().unwrap();
    visit.entry.arrival = arrival.clone();
    visit.anchor = anchor.clone();
    let enter = FieldVisitSaveJob::new(small.operation(), before, next, NOW + 1).unwrap();
    let entered = handle.save_field_visit(enter).unwrap().await.unwrap().unwrap();
    assert_eq!(entered.commit, SceneCommit::Applied);
    assert_payloads(&entered.current, &arrival, &anchor);

    let mut next = entered.current.clone();
    let mut story = StoryState::decode(&next.pair.story.blob).unwrap();
    story.add_byte_checked(1, 4, -1, 0, 100).unwrap();
    next.pair.story.blob = story.encode();
    next.pair.character.as_mut().unwrap().gil -= 2;
    let visit = next.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.scratch_values[30] = 2;
    visit.scratch_known_masks[30] = 2;
    let idle_advance = FieldVisitSaveJob::new(operation(12, FieldVisitOperationKind::Advance),
        entered.current, next, NOW + 2).unwrap();
    let advanced = handle.save_field_visit(idle_advance.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(advanced.commit, SceneCommit::Applied);
    assert_eq!(advanced.current, *idle_advance.next());
    assert_payloads(&advanced.current, &arrival, &anchor);
    assert_eq!(StoryState::decode(&advanced.current.pair.story.blob).unwrap().known_byte(1, 4), Some(49));
    assert_eq!(character(&advanced.current).gil, 238);

    let attach = attach(&advanced.current);
    let attached = handle.save_scene_batch(attach.clone()).unwrap().await.unwrap().unwrap().current.remove(0);
    assert_payloads(&attached, &arrival, &anchor);
    assert_eq!(attached.pair, advanced.current.pair, "attachment preserves both records exactly");
    assert_eq!(attached.field_visit.as_ref().unwrap().revision, 2);
    assert_eq!(attached.field_visit.as_ref().unwrap().scratch_values, advanced.current.field_visit.as_ref().unwrap().scratch_values);
    assert_eq!(attached.field_visit.as_ref().unwrap().scratch_known_masks, advanced.current.field_visit.as_ref().unwrap().scratch_known_masks);

    let acknowledge = advance_scene(&attached);
    let acknowledged = handle.save_scene_batch(acknowledge.clone()).unwrap().await.unwrap().unwrap().current.remove(0);
    assert_payloads(&acknowledged, &arrival, &anchor);
    assert_eq!(acknowledged.pair, attached.pair, "a window prefix cannot publish partial gameplay changes");
    let mut expected_visit = attached.field_visit.clone().unwrap();
    expected_visit.revision += 1;
    assert_eq!(acknowledged.field_visit, Some(expected_visit));
    assert_eq!(acknowledged.checkpoint, acknowledge.members[0].checkpoint);

    let complete = complete(&acknowledged);
    let completed = handle.save_scene_batch(complete.clone()).unwrap().await.unwrap().unwrap().current.remove(0);
    assert_payloads(&completed, &arrival, &anchor);
    assert!(completed.checkpoint.is_none() && completed.reservation.is_none());
    let visit = completed.field_visit.as_ref().unwrap();
    assert_eq!((visit.revision, visit.scratch_values[30], visit.scratch_known_masks[30],
        visit.scratch_values[31], visit.scratch_known_masks[31]), (4, 2, 2, 1, 1));
    assert!(visit.active_continuation.is_none());
    let story = StoryState::decode(&completed.pair.story.blob).unwrap();
    assert_eq!(story.known_byte(1, 4), Some(46));
    assert_eq!(story.known_bit(3, 66, 0), Some(true));
    let mut expected_character = character(&acknowledged).clone();
    expected_character.gil -= 1;
    expected_character.updated_at = character(&completed).updated_at;
    assert_eq!(character(&completed), &expected_character);

    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 20, 3))));
    let earned = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap().pop().unwrap();
    assert_payloads(&earned, &arrival, &anchor);
    assert_eq!((character(&earned).exp, character(&earned).gil, character(&earned).ap), (52, 257, 10));
    assert_eq!(earned.field_visit, completed.field_visit);
    assert_eq!(earned.pair.story, completed.pair.story);
    let durable = files(dir.path());
    let duplicate = handle.save_field_visit(idle_advance.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, earned, "the retained idle receipt cannot replace the completed visit or later rewards");
    for job in [&attach, &acknowledge, &complete] {
        let duplicate = handle.save_scene_batch(job.clone()).unwrap().await.unwrap().unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, vec![earned.clone()]);
    }
    assert_eq!(files(dir.path()), durable, "receipt retries do not rewrite either large payload or its records");
    saver.shutdown(Duration::from_secs(20)).await;
    drop(handle);
    drop(saver);
    drop(store);

    let reopened = Store::open(dir.path().join("state")).unwrap();
    assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), earned);
    let duplicate = persist_field_visit(&reopened, GAME, &adapter(), &idle_advance).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, earned);
    for job in [&attach, &acknowledge, &complete] {
        let duplicate = persist_scene_batch(&reopened, GAME, None, job, NOW + 99).unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, vec![earned.clone()]);
    }
    assert_eq!(files(dir.path()), durable);
}
