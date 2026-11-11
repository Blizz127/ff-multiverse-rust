//! The original Jessie choice across durable ACKs and two real store restarts. Only
//! stored input prefixes resume the scene; effects remain speculative until Complete.
//! Reuses the parent's explicit fixture initialization, source, anchor and temporary stores.

use super::*;
use super::super::ActiveVisitBoundary;
use crate::saver::SceneBatchJob;
use fmv_persist::{SceneOperationKind, ScenePhase};
use fmv_protocol::{SceneOrigin, SceneReplayEvent};

const INSTANCE: [u8; 32] = [71; 32];
const SEED: u32 = 1;

fn origin() -> SceneOrigin {
    SceneOrigin { pos: Pos::new(72.0, -319.0, -53.0),
        facing: fmv_ff7::field_direction_to_facing(64), triangle: Some(47) }
}

async fn save(handle: &SaverHandle, job: SceneBatchJob) -> SceneState {
    let saved = handle.save_scene_batch(job).unwrap().await.unwrap().unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current.len(), 1);
    saved.current.into_iter().next().unwrap()
}

fn window(source: &IdleVisitSource<'_>, def: &SceneDef, state: &SceneState,
    expected_step: u32, expected_choices: Option<[u8; 2]>, answer: Option<u8>) -> SceneReplayEvent {
    let ActiveVisitBoundary::Window { step, identity, choices } = source.active_scene_boundary(def, state).unwrap()
        else { panic!("the next authored window must still await its ACK") };
    assert_eq!((step, choices), (expected_step, expected_choices));
    assert!(!identity.is_empty());
    SceneReplayEvent::Window { step, identity, answer }
}

fn assert_pending(idle: &SceneState, current: &SceneState, accepted: u64) {
    assert_eq!(current.pair, idle.pair, "ACKs cannot publish speculative affection or resource changes");
    let mut expected_visit = idle.field_visit.as_ref().unwrap().clone();
    expected_visit.revision += 1 + accepted;
    expected_visit.active_continuation = Some(INSTANCE);
    assert_eq!(current.field_visit.as_ref(), Some(&expected_visit),
        "scratch values/knowledge, source, visit id, entry and opaque anchor remain unchanged");
    assert!(current.reservation.is_none());
    let checkpoint = current.checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.instance, INSTANCE);
    assert_eq!(checkpoint.scene_key, JESSIE_TRAIN_TALK_SCENE_ID);
    assert_eq!(checkpoint.original_story, idle.pair.story.blob);
    assert_eq!(checkpoint.original_gil, 240);
    assert_eq!(checkpoint.seed, SEED);
    assert_eq!(checkpoint.phase, ScenePhase::Resume { segment: 0 });
    assert_eq!(checkpoint.consumed_battles, 0);
    // Inspect metadata only; never substitute or manufacture this private replay payload.
    let payload: serde_json::Value = serde_json::from_slice(&checkpoint.replay).unwrap();
    assert_eq!(serde_json::from_value::<SceneOrigin>(payload["origin"].clone()).unwrap(), origin());
    assert_eq!(payload["events"].as_array().unwrap().len(), accepted as usize);
    assert_eq!(serde_json::from_value::<FieldVisitV1>(payload["visit"].clone()).unwrap(),
        *idle.field_visit.as_ref().unwrap(), "the replay retains the exact original idle input");
}

#[tokio::test]
#[ignore = "needs original FF7 data; active visits are tested only in throwaway stores"]
async fn owned_jessie_active_visit_resumes_saved_answers_and_commits_latch_with_affection_once() {
    let adapter = owned_adapter();
    let area = adapter.load_field("tin_1").unwrap();
    let field = area.field_file();
    let def = definition(field);
    let source = IdleVisitSource::new("tin_1", field, &def.setup, PROFILE).unwrap();
    for affection in [0u8, 2, 30] {
        for answer in [2u8, 3] {
            let original = story(affection);
            let scratch = initialized(field, &def, &original, 240);
            let (dir, store, account) = fixture(&original);
            let before_entry = store.get_scene_state(account, GAME).unwrap();
            store.commit_field_visit(&FieldVisitOperation { id: [1; 16], kind: FieldVisitOperationKind::Enter },
                &before_entry, &entry(&before_entry, &source, &scratch, 1)).unwrap();
            let saver = start_saver(&store, &adapter);
            let handle = saver.handle();
            let idle = snapshot(&handle, account).await;
            let durable = files(dir.path());
            assert!(source.attach_scene_job(&def, &idle, INSTANCE, SEED, None).is_err(),
                "an opaque fixture anchor cannot replace the actual accepted talk origin");
            assert_eq!(files(dir.path()), durable);

            let attach = source.attach_scene_job(&def, &idle, INSTANCE, SEED, Some(origin())).unwrap();
            assert_eq!(attach.operation.kind, SceneOperationKind::AttachVisit);
            assert!(attach.members[0].next_story.is_none() && attach.members[0].next_gil.is_none());
            let attached = save(&handle, attach).await;
            assert_pending(&idle, &attached, 0);
            assert!(source.complete_scene_job(&def, &attached).is_err(), "the question has not been acknowledged");
            let changed_source = IdleVisitSource::new("tin_1", field, &def.setup,
                b"owned-test-only/different-reconstruction-profile/v1").unwrap();
            assert!(changed_source.active_scene_boundary(&def, &attached).is_err());

            let first_ack = window(&source, &def, &attached, 0, Some([2, 3]), Some(answer));
            let mut wrong_ack = first_ack.clone();
            if let SceneReplayEvent::Window { step, .. } = &mut wrong_ack { *step += 1; }
            let durable = files(dir.path());
            assert!(source.advance_scene_job(&def, &attached, wrong_ack).is_err());
            assert_eq!(files(dir.path()), durable);
            let advance = source.advance_scene_job(&def, &attached, first_ack.clone()).unwrap();
            assert_eq!(advance.operation.kind, SceneOperationKind::AdvanceVisit);
            assert_eq!(advance.operation.ordinal, 1);
            let answered = save(&handle, advance).await;
            assert_pending(&idle, &answered, 1);
            saver.shutdown(Duration::from_secs(20)).await;
            drop(store);

            // First restart: the stored choice selects the next window. Do not send the
            // answer again or rebuild a fresh transcript from test parameters.
            let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
            assert_eq!(store.get_scene_state(account, GAME).unwrap(), answered);
            let saver = start_saver(&store, &adapter);
            let handle = saver.handle();
            let resumed = snapshot(&handle, account).await;
            assert_pending(&idle, &resumed, 1);
            assert!(source.complete_scene_job(&def, &resumed).is_err(), "the second authored window still awaits ACK");
            assert!(source.advance_scene_job(&def, &resumed, first_ack).is_err(), "a saved answer cannot acknowledge another window");
            let final_ack = window(&source, &def, &resumed, 1, None, None);
            let advance = source.advance_scene_job(&def, &resumed, final_ack).unwrap();
            assert_eq!(advance.operation.ordinal, 2);
            let finished = save(&handle, advance).await;
            assert_pending(&idle, &finished, 2);
            assert_eq!(source.active_scene_boundary(&def, &finished).unwrap(), ActiveVisitBoundary::Complete);
            saver.shutdown(Duration::from_secs(20)).await;
            drop(store);

            // Second restart: both ACKs are durable and the replay is Finished, but the
            // stored story and scratch still contain the original values until Complete.
            let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
            assert_eq!(store.get_scene_state(account, GAME).unwrap(), finished);
            let saver = start_saver(&store, &adapter);
            let handle = saver.handle();
            let terminal = snapshot(&handle, account).await;
            assert_pending(&idle, &terminal, 2);
            assert_eq!(source.active_scene_boundary(&def, &terminal).unwrap(), ActiveVisitBoundary::Complete);
            let complete = source.complete_scene_job(&def, &terminal).unwrap();
            assert_eq!(complete.operation.kind, SceneOperationKind::CompleteVisit);
            assert_eq!(source.complete_scene_job(&def, &terminal).unwrap(), complete,
                "only the saved terminal prefix determines this immutable completion");
            let completed = save(&handle, complete.clone()).await;
            let mut expected_story = original.clone();
            expected_story.write_byte(1, 4, if answer == 3 { affection.saturating_sub(3) } else { affection }).unwrap();
            assert_eq!(completed.pair.story.blob, expected_story.encode());
            let visit = completed.field_visit.as_ref().unwrap();
            let initial_visit = idle.field_visit.as_ref().unwrap();
            assert_eq!((visit.revision, visit.scratch_values[31], visit.scratch_known_masks[31]), (4, 1, 255));
            assert_eq!((visit.visit_id, visit.source_identity, &visit.entry, &visit.anchor),
                (initial_visit.visit_id, initial_visit.source_identity, &initial_visit.entry, &initial_visit.anchor));
            assert!(visit.active_continuation.is_none() && completed.checkpoint.is_none() && completed.reservation.is_none());
            let mut expected_character = idle.pair.character.clone().unwrap();
            expected_character.updated_at = completed.pair.character.as_ref().unwrap().updated_at;
            assert_eq!(completed.pair.character, Some(expected_character), "the talk changes no unrelated resources");
            assert!(source.complete_scene_job(&def, &completed).is_err(), "a cleared continuation cannot start a second completion");

            assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 17, 3))));
            let earned = snapshot(&handle, account).await;
            let character = earned.pair.character.as_ref().unwrap();
            assert_eq!((character.exp, character.gil, character.ap, character.limit), (52, 257, 10, 137));
            assert_eq!(earned.field_visit, completed.field_visit);
            let durable = files(dir.path());
            let duplicate = handle.save_scene_batch(complete.clone()).unwrap().await.unwrap().unwrap();
            assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
            assert_eq!(duplicate.current.as_slice(), std::slice::from_ref(&earned), "a late completion ACK returns the current rewarded record");
            assert_eq!(files(dir.path()), durable, "exact completion retry cannot deduct again or restore pre-reward values");
            saver.shutdown(Duration::from_secs(20)).await;
            drop(store);
            assert_eq!(Store::open(dir.path().join("state")).unwrap().get_scene_state(account, GAME).unwrap(), earned);
        }
    }
}
