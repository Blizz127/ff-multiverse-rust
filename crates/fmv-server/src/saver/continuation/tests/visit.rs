//! Linked, noncombat visits through the real scene FIFO. These synthetic checkpoints
//! exercise storage ordering only; they do not activate field-visit admission or replay.

use fmv_persist::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1,
    MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES};

use super::*;

fn entered(store: &Store, account: AccountId) -> SceneState {
    let before = store.get_scene_state(account, GAME).unwrap();
    let operation = FieldVisitOperation { id: [11; 16], kind: FieldVisitOperationKind::Enter };
    let mut next = before.clone();
    let mut known = [0; 256];
    known[31] = 1;
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1,
        visit_id: [31; 16],
        revision: 0,
        field: "fixture/train".into(),
        source_identity: [41; 32],
        entry: FieldVisitEntryV1 { operation: operation.id, arrival: vec![1, 4, 2] },
        anchor: vec![2, 1, 8],
        scratch_values: [0; 256],
        scratch_known_masks: known,
        active_continuation: None,
    });
    store.commit_field_visit(&operation, &before, &next).unwrap().current
}

fn attach_job(before: &SceneState) -> SceneBatchJob {
    let mut visit = before.field_visit.clone().unwrap();
    visit.revision += 1;
    visit.active_continuation = Some(INSTANCE);
    SceneBatchJob {
        operation: operation(SceneOperationKind::AttachVisit),
        members: vec![SceneBatchMember {
            expected: before.clone(),
            next_story: None,
            next_gil: None,
            next_visit: Some(visit),
            checkpoint: Some(SceneCheckpoint {
                instance: INSTANCE,
                scene_key: "fixture/train-talk".into(),
                source_hash: [19; 32],
                replay_version: 1,
                original_story: before.pair.story.blob.clone(),
                original_gil: character(before).gil,
                seed: 123,
                replay: vec![0, 255, 0, 7],
                phase: ScenePhase::Resume { segment: 0 },
                consumed_battles: 0,
            }),
            reservation: None,
            battle: None,
        }],
    }
}

fn complete_job(before: &SceneState) -> SceneBatchJob {
    let mut visit = before.field_visit.clone().unwrap();
    visit.revision += 1;
    visit.scratch_values[31] = 1;
    visit.active_continuation = None;
    SceneBatchJob {
        operation: operation(SceneOperationKind::CompleteVisit),
        members: vec![SceneBatchMember {
            expected: before.clone(),
            next_story: Some(vec![7, 0, 255, 0]),
            next_gil: Some(character(before).gil - 1),
            next_visit: Some(visit),
            checkpoint: None,
            reservation: None,
            battle: None,
        }],
    }
}

fn advance_job(before: &SceneState) -> SceneBatchJob {
    let mut visit = before.field_visit.clone().unwrap();
    visit.revision += 1;
    let mut checkpoint = before.checkpoint.clone().unwrap();
    checkpoint.replay.extend_from_slice(&[1, 0, 255]);
    SceneBatchJob {
        operation: SceneOperation { ordinal: 1, ..operation(SceneOperationKind::AdvanceVisit) },
        members: vec![SceneBatchMember {
            expected: before.clone(),
            next_story: None,
            next_gil: None,
            next_visit: Some(visit),
            checkpoint: Some(checkpoint),
            reservation: None,
            battle: None,
        }],
    }
}

fn assert_visit_identity(before: &SceneState, after: &SceneState) {
    let before = before.field_visit.as_ref().unwrap();
    let after = after.field_visit.as_ref().unwrap();
    assert_eq!(after.visit_id, before.visit_id);
    assert_eq!(after.field, before.field);
    assert_eq!(after.source_identity, before.source_identity);
    assert_eq!(after.entry, before.entry);
    assert_eq!(after.anchor, before.anchor, "the saver does not invent a reconstructed runtime anchor");
}

#[tokio::test]
async fn fifo_attach_advance_complete_and_lost_ack_preserve_later_rewards_on_every_receipt_retry() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let initial = entered(&store, account);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(fmv_fixture::FixtureAdapter::new()));
    let handle = saver.handle();
    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 20, 3))));
    let before = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap().pop().unwrap();
    assert_eq!(before.field_visit, initial.field_visit);
    assert_eq!((character(&before).exp, character(&before).gil, character(&before).ap), (52, 260, 10));

    let attach = attach_job(&before);
    let attached = handle.save_scene_batch(attach.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(attached.commit, SceneCommit::Applied);
    let active = &attached.current[0];
    assert_eq!(active.pair, before.pair, "attachment cannot restamp story or resources");
    assert_eq!(active.field_visit.as_ref().unwrap().revision, 1);
    assert_eq!(active.field_visit.as_ref().unwrap().active_continuation, Some(INSTANCE));
    assert_eq!(active.field_visit.as_ref().unwrap().scratch_values, before.field_visit.as_ref().unwrap().scratch_values);
    assert_eq!(active.field_visit.as_ref().unwrap().scratch_known_masks, before.field_visit.as_ref().unwrap().scratch_known_masks);
    assert!(active.reservation.is_none());
    let checkpoint = active.checkpoint.as_ref().unwrap();
    assert_eq!((checkpoint.phase.clone(), checkpoint.consumed_battles), (ScenePhase::Resume { segment: 0 }, 0));
    assert_eq!(checkpoint.original_gil, 260);
    assert_eq!(checkpoint.original_story, before.pair.story.blob);
    assert_visit_identity(&before, active);

    let advance = advance_job(active);
    let advanced = handle.save_scene_batch(advance.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(advanced.commit, SceneCommit::Applied);
    assert_eq!(advanced.current[0].pair, active.pair, "a window acknowledgement cannot publish partial story or resources");
    let mut expected_visit = active.field_visit.clone().unwrap();
    expected_visit.revision += 1;
    assert_eq!(advanced.current[0].field_visit, Some(expected_visit), "advancing retains scratch and the active link exactly");
    assert_eq!(advanced.current[0].checkpoint, advance.members[0].checkpoint);
    let mut original_checkpoint = advanced.current[0].checkpoint.clone().unwrap();
    original_checkpoint.replay = active.checkpoint.as_ref().unwrap().replay.clone();
    assert_eq!(Some(original_checkpoint), active.checkpoint, "only the accepted transcript advances");
    let active = &advanced.current[0];

    let complete = complete_job(active);
    drop(handle.save_scene_batch(complete.clone()).unwrap());
    let completed = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap().pop().unwrap();
    assert!(completed.checkpoint.is_none() && completed.reservation.is_none());
    assert_eq!(completed.pair.story.blob, [7, 0, 255, 0]);
    let mut expected_character = character(active).clone();
    expected_character.gil -= 1;
    expected_character.updated_at = character(&completed).updated_at;
    assert_eq!(character(&completed), &expected_character, "complete changes only its selected gil field");
    let visit = completed.field_visit.as_ref().unwrap();
    assert_eq!((visit.revision, visit.scratch_values[31], visit.scratch_known_masks[31]), (3, 1, 1));
    assert!(visit.active_continuation.is_none());
    assert_visit_identity(&before, &completed);

    let mut reward = BattleReward::new(account, 4, 50, 2);
    reward.items = vec![item(7, 1)];
    assert!(handle.save(SaveJob::reward(account, 0, reward)));
    let earned = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!((character(&earned[0]).exp, character(&earned[0]).gil, character(&earned[0]).ap), (56, 309, 12));
    assert_eq!(character(&earned[0]).inventory.as_ref().unwrap().iter().find(|entry| entry.item == 7).unwrap().count, 4);
    assert_eq!(earned[0].field_visit, completed.field_visit);
    let durable = files(&dir.path().join("state"));
    for request in [&attach, &advance, &complete] {
        let retry = handle.save_scene_batch(request.clone()).unwrap().await.unwrap().unwrap();
        assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
        assert_eq!(retry.current, earned, "an older attachment, acknowledgement or completion cannot replace a later reward");
        assert_eq!(files(&dir.path().join("state")), durable);
    }
    saver.shutdown(Duration::from_secs(20)).await;
    assert!(matches!(handle.save_scene_batch(complete.clone()), Err(SceneSaveError::Stopped)));
    drop(handle);
    drop(saver);
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), earned);
    for request in [&attach, &advance, &complete] {
        let retry = persist_scene_batch(&reopened, GAME, None, request, NOW + 99).unwrap();
        assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
        assert_eq!(retry.current, earned);
    }
}

#[tokio::test]
async fn a_queued_reward_invalidates_a_frozen_attach_without_partial_metadata_or_stopping_the_fifo() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let initial = entered(&store, account);
    let stale = attach_job(&initial);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(fmv_fixture::FixtureAdapter::new()));
    let handle = saver.handle();
    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 0, 20, 0))));
    let current = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap().pop().unwrap();
    let durable = files(&dir.path().join("state"));
    assert!(matches!(handle.save_scene_batch(stale).unwrap().await.unwrap(), Err(SceneSaveError::StaleState)));
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), current);
    assert_eq!(files(&dir.path().join("state")), durable);

    let attached = handle.save_scene_batch(attach_job(&current)).unwrap().await.unwrap().unwrap();
    let mut stale_completion = complete_job(&attached.current[0]);
    stale_completion.members[0].expected.pair.character.as_mut().unwrap().limit += 1;
    let durable = files(&dir.path().join("state"));
    assert!(matches!(handle.save_scene_batch(stale_completion).unwrap().await.unwrap(), Err(SceneSaveError::StaleState)));
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), attached.current[0]);
    assert_eq!(files(&dir.path().join("state")), durable);

    let completed = handle.save_scene_batch(complete_job(&attached.current[0])).unwrap().await.unwrap().unwrap();
    assert_eq!(completed.commit, SceneCommit::Applied);
    assert_eq!(character(&completed.current[0]).gil, 259);
    assert!(completed.current[0].checkpoint.is_none());
    saver.shutdown(Duration::from_secs(20)).await;
}

#[test]
fn reusing_a_visit_operation_for_another_result_refuses_without_touching_either_record() {
    let (dir, store, accounts) = fixture(1);
    let initial = entered(&store, accounts[0]);
    let attach = attach_job(&initial);
    let active = persist_scene_batch(&store, GAME, None, &attach, NOW + 1).unwrap().current.remove(0);
    let complete = complete_job(&active);
    let saved = persist_scene_batch(&store, GAME, None, &complete, NOW + 2).unwrap();
    let durable = files(&dir.path().join("state"));
    for altered in ["gil", "story", "scratch"] {
        let mut conflict = complete.clone();
        match altered {
            "gil" => conflict.members[0].next_gil = Some(character(&active).gil - 2),
            "story" => conflict.members[0].next_story = Some(vec![8, 0, 255, 0]),
            "scratch" => conflict.members[0].next_visit.as_mut().unwrap().scratch_values[31] = 0,
            _ => unreachable!(),
        }
        assert!(matches!(persist_scene_batch(&store, GAME, None, &conflict, NOW + 3),
            Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))), "{altered}");
        assert_eq!(states(&store, &accounts), saved.current, "{altered}");
        assert_eq!(files(&dir.path().join("state")), durable, "{altered}");
    }
}

#[test]
fn malformed_or_inapplicable_visit_decisions_never_enter_the_queue_or_touch_storage() {
    let (dir, store, accounts) = fixture(2);
    let idle = entered(&store, accounts[0]);
    let other = entered(&store, accounts[1]);
    let attach = attach_job(&idle);
    let active = persist_scene_batch(&store, GAME, None, &attach, NOW + 1).unwrap().current.remove(0);
    let advance = advance_job(&active);
    let complete = complete_job(&active);
    for valid in [&attach, &advance, &complete] { valid.validate().unwrap(); }
    let current = states(&store, &accounts);
    let durable = files(&dir.path().join("state"));
    let (handle, mut queue) = crate::saver::SaverHandle::channel();

    let mut cases = Vec::new();
    for bad in ["oversized anchor", "oversized entry", "oversized field", "noncanonical scratch",
        "missing next visit", "changed identity", "changed source", "changed entry", "changed anchor",
        "forgotten knowledge", "equal attach story", "equal attach gil", "reservation", "battle", "multiple owners",
        "equal advance story", "equal advance gil", "advance changes scratch", "advance changes original checkpoint",
        "advance ordinal zero", "advance ordinal 129"] {
        let mut job = if bad.contains("advance") { advance.clone() } else { attach.clone() };
        if bad == "multiple owners" {
            job.members.push(attach_job(&other).members.remove(0));
            cases.push((bad, job));
            continue;
        }
        let member = &mut job.members[0];
        match bad {
            // Keep both images otherwise equal so these exercise the independent bounds
            // and canonicality checks, rather than merely the immutable-field comparison.
            "oversized anchor" => {
                let bytes = vec![1; MAX_FIELD_VISIT_ANCHOR_BYTES + 1];
                member.expected.field_visit.as_mut().unwrap().anchor = bytes.clone();
                member.next_visit.as_mut().unwrap().anchor = bytes;
            }
            "oversized entry" => {
                let bytes = vec![1; MAX_FIELD_VISIT_ENTRY_BYTES + 1];
                member.expected.field_visit.as_mut().unwrap().entry.arrival = bytes.clone();
                member.next_visit.as_mut().unwrap().entry.arrival = bytes;
            }
            "oversized field" => {
                let field = "f".repeat(MAX_FIELD_VISIT_FIELD_BYTES + 1);
                member.expected.field_visit.as_mut().unwrap().field = field.clone();
                member.next_visit.as_mut().unwrap().field = field;
            }
            "noncanonical scratch" => {
                member.expected.field_visit.as_mut().unwrap().scratch_values[30] = 1;
                member.next_visit.as_mut().unwrap().scratch_values[30] = 1;
            }
            "missing next visit" => member.next_visit = None,
            "changed identity" => member.next_visit.as_mut().unwrap().visit_id = [32; 16],
            "changed source" => member.next_visit.as_mut().unwrap().source_identity = [42; 32],
            "changed entry" => member.next_visit.as_mut().unwrap().entry.operation = [12; 16],
            "changed anchor" => member.next_visit.as_mut().unwrap().anchor.push(1),
            "forgotten knowledge" => member.next_visit.as_mut().unwrap().scratch_known_masks[31] = 0,
            "equal attach story" | "equal advance story" => member.next_story = Some(member.expected.pair.story.blob.clone()),
            "equal attach gil" | "equal advance gil" => member.next_gil = Some(character(&member.expected).gil),
            "reservation" => member.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 }),
            "battle" => member.battle = Some(SceneBattleSettlement {
                character: character(&member.expected).character,
                consumed_items: Vec::new(),
                reward: BattleReward::new(accounts[0], 0, 0, 0),
                final_limit: None,
            }),
            "advance changes scratch" => member.next_visit.as_mut().unwrap().scratch_values[31] = 1,
            "advance changes original checkpoint" => member.checkpoint.as_mut().unwrap().original_story.push(9),
            "advance ordinal zero" => job.operation.ordinal = 0,
            "advance ordinal 129" => job.operation.ordinal = 129,
            _ => unreachable!(),
        }
        cases.push((bad, job));
    }

    // Every older operation must reject the new setter. Its otherwise accepted queue
    // shape is checked first, so ResolveBattle's required explicit settlement cannot
    // accidentally account for the refusal instead of next_visit itself.
    for kind in [SceneOperationKind::PrepareBattle, SceneOperationKind::ResolveBattle,
        SceneOperationKind::Finish, SceneOperationKind::ContinueLanding,
        SceneOperationKind::Complete, SceneOperationKind::BattleOutcome] {
        let mut legacy = attach.clone();
        legacy.operation.kind = kind;
        legacy.members[0].next_visit = None;
        if kind == SceneOperationKind::ResolveBattle {
            legacy.members[0].battle = Some(SceneBattleSettlement {
                character: character(&idle).character,
                consumed_items: Vec::new(),
                reward: BattleReward::new(accounts[0], 0, 0, 0),
                final_limit: None,
            });
        }
        legacy.validate().unwrap();
        legacy.members[0].next_visit = attach.members[0].next_visit.clone();
        cases.push(("existing operation with next visit", legacy));
    }
    for (case, invalid) in cases {
        assert!(matches!(handle.save_scene_batch(invalid), Err(SceneSaveError::InvalidRequest)), "{case}");
        assert!(matches!(queue.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)), "{case}");
        assert_eq!(states(&store, &accounts), current, "{case}");
        assert_eq!(files(&dir.path().join("state")), durable, "{case}");
    }
}
