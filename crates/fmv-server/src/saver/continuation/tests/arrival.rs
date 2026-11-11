//! A required arrival records accepted windows before creating its first idle field visit.
//! These synthetic decisions exercise the durable queue, not source or transcript evaluation.

use fmv_persist::{FieldVisitEntryV1, FieldVisitV1, MAX_FIELD_VISIT_ANCHOR_BYTES,
    MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES, MAX_ARRIVAL_METADATA_STEPS};

use super::*;

const ARRIVAL: [u8; 32] = [83; 32];

fn pending_arrival(store: &Store, account: AccountId) -> (SceneState, SceneBatchJob) {
    let prepared = prepare(store, &[account]);
    let resolved = persist_scene_batch(store, GAME, Some(&SyntheticRules::default()),
        &resolve_job(&prepared), NOW + 2).unwrap().current;
    let landed = persist_scene_batch(store, GAME, None, &finish_job(&resolved), NOW + 3).unwrap().current.remove(0);
    let checkpoint = SceneCheckpoint {
        instance: ARRIVAL,
        scene_key: "fixture/train-arrival".into(),
        source_hash: [73; 32],
        replay_version: 1,
        original_story: landed.pair.story.blob.clone(),
        original_gil: character(&landed).gil,
        seed: 71,
        replay: vec![0, 48],
        phase: ScenePhase::Resume { segment: 0 },
        consumed_battles: 0,
    };
    let job = SceneBatchJob {
        operation: SceneOperation { ordinal: 1, ..operation(SceneOperationKind::BeginArrival) },
        members: vec![SceneBatchMember {
            expected: landed.clone(), next_story: None, next_gil: None, checkpoint: Some(checkpoint),
            reservation: None, battle: None, next_visit: None,
        }],
    };
    (landed, job)
}

fn resumed_arrival(store: &Store, account: AccountId) -> (SceneState, SceneBatchJob) {
    let (landed, job) = pending_arrival(store, account);
    let resumed = persist_scene_batch(store, GAME, None, &job, NOW + 4).unwrap().current.remove(0);
    assert_eq!(resumed.pair, landed.pair);
    assert!(resumed.field_visit.is_none() && resumed.reservation.is_none());
    (resumed, job)
}

fn arrival_operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: ARRIVAL, ordinal: 0, attempt: 0, kind }
}

fn advance_arrival(before: &SceneState, ordinal: u32) -> SceneBatchJob {
    let mut checkpoint = before.checkpoint.clone().unwrap();
    checkpoint.replay.extend_from_slice(&[ordinal as u8, 255]);
    SceneBatchJob {
        operation: SceneOperation { ordinal, ..arrival_operation(SceneOperationKind::AdvanceArrival) },
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: None, next_gil: None, checkpoint: Some(checkpoint),
            reservation: None, battle: None, next_visit: None,
        }],
    }
}

fn complete_arrival(before: &SceneState) -> SceneBatchJob {
    let mut scratch_values = [0; 256];
    scratch_values[6] = 10;
    let visit = FieldVisitV1 {
        schema_version: 1, visit_id: [61; 16], revision: 0, field: "fixture/train".into(),
        source_identity: [79; 32],
        entry: FieldVisitEntryV1 { operation: [71; 16], arrival: vec![1, 48, 255] },
        anchor: vec![51, 0], scratch_values, scratch_known_masks: [255; 256], active_continuation: None,
    };
    SceneBatchJob {
        operation: arrival_operation(SceneOperationKind::CompleteArrivalVisit),
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: Some(vec![51, 0, 255, 0]),
            next_gil: Some(character(before).gil - 1), checkpoint: None,
            reservation: None, battle: None, next_visit: Some(visit),
        }],
    }
}

#[tokio::test]
async fn arrival_fifo_advances_then_completes_despite_lost_ack_and_old_retries_preserve_later_rewards() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let (landed, begin) = pending_arrival(&store, account);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(fmv_fixture::FixtureAdapter::new()));
    let handle = saver.handle();
    let begun = handle.save_scene_batch(begin.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(begun.commit, SceneCommit::Applied);
    let initial = begun.current.into_iter().next().unwrap();
    assert_eq!(initial.pair, landed.pair, "begin retains the full landed pair, including timestamps");
    assert!(initial.field_visit.is_none() && initial.reservation.is_none());
    let mut current = initial.clone();
    let mut advances = Vec::new();
    assert_eq!(MAX_ARRIVAL_METADATA_STEPS, 3, "two windows plus the terminal presentation seal");
    for ordinal in 1..=MAX_ARRIVAL_METADATA_STEPS {
        let job = advance_arrival(&current, ordinal);
        let saved = handle.save_scene_batch(job.clone()).unwrap().await.unwrap().unwrap();
        assert_eq!(saved.commit, SceneCommit::Applied);
        current = saved.current.into_iter().next().unwrap();
        assert_eq!(current.pair, initial.pair, "accepted dialogue cannot restamp or publish gameplay state");
        assert!(current.field_visit.is_none() && current.reservation.is_none());
        assert_eq!(current.checkpoint, job.members[0].checkpoint);
        advances.push(job);
    }
    let complete = complete_arrival(&current);
    drop(handle.save_scene_batch(complete.clone()).unwrap());
    let completed = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap().remove(0);
    assert!(completed.checkpoint.is_none() && completed.reservation.is_none());
    assert_eq!(completed.field_visit, complete.members[0].next_visit);
    assert_eq!(completed.pair.story.blob, [51, 0, 255, 0]);
    let mut expected_character = character(&initial).clone();
    expected_character.gil -= 1;
    expected_character.updated_at = character(&completed).updated_at;
    expected_character.schema_version = character(&completed).schema_version;
    assert_eq!(character(&completed), &expected_character, "only selected gil and storage stamps change");
    let visit = completed.field_visit.as_ref().unwrap();
    assert_eq!((visit.entry.arrival.len(), visit.anchor.len()), (3, 2));
    assert_eq!(character(&completed).schema_version, 4,
        "small visits retain schema 4; schema 5 is required only when either payload exceeds 1024 bytes");
    assert!(applied(&store, &complete));

    let mut reward = BattleReward::new(account, 4, 50, 2);
    reward.items = vec![item(7, 1)];
    assert!(handle.save(SaveJob::reward(account, 0, reward)));
    let earned = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!((character(&earned[0]).exp, character(&earned[0]).gil, character(&earned[0]).ap), (44, 288, 9));
    assert_eq!(character(&earned[0]).inventory.as_ref().unwrap().iter().find(|item| item.item == 7).unwrap().count, 4);
    assert_eq!(earned[0].field_visit, completed.field_visit);
    let durable = files(&dir.path().join("state"));
    for request in std::iter::once(&begin).chain(advances.iter()).chain(std::iter::once(&complete)) {
        let retried = handle.save_scene_batch(request.clone()).unwrap().await.unwrap().unwrap();
        assert_eq!(retried.commit, SceneCommit::AlreadyApplied);
        assert_eq!(retried.current, earned, "old window or completion receipts cannot overwrite a later reward");
        assert_eq!(files(&dir.path().join("state")), durable);
    }
    saver.shutdown(Duration::from_secs(20)).await;
    drop(handle);
    drop(saver);
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), earned);
    for request in std::iter::once(&begin).chain(advances.iter()).chain(std::iter::once(&complete)) {
        let retried = persist_scene_batch(&reopened, GAME, None, request, NOW + 99).unwrap();
        assert_eq!(retried.commit, SceneCommit::AlreadyApplied);
        assert_eq!(retried.current, earned);
    }
    assert_eq!(files(&dir.path().join("state")), durable);
}

#[tokio::test]
async fn stale_arrival_predecessors_and_conflicting_results_preserve_records_and_the_fifo() {
    let (dir, store, accounts) = fixture(1);
    let (initial, _) = resumed_arrival(&store, accounts[0]);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(fmv_fixture::FixtureAdapter::new()));
    let handle = saver.handle();
    for mut stale in [advance_arrival(&initial, 1), complete_arrival(&initial)] {
        stale.members[0].expected.pair.character.as_mut().unwrap().limit += 1;
        let durable = files(&dir.path().join("state"));
        assert!(matches!(handle.save_scene_batch(stale).unwrap().await.unwrap(), Err(SceneSaveError::StaleState)));
        assert_eq!(store.get_scene_state(accounts[0], GAME).unwrap(), initial);
        assert_eq!(files(&dir.path().join("state")), durable);
    }
    let advance = advance_arrival(&initial, 1);
    let advanced = handle.save_scene_batch(advance.clone()).unwrap().await.unwrap().unwrap().current.remove(0);
    let mut conflicting_advance = advance.clone();
    conflicting_advance.members[0].checkpoint.as_mut().unwrap().replay.push(9);
    let durable = files(&dir.path().join("state"));
    assert!(matches!(handle.save_scene_batch(conflicting_advance).unwrap().await.unwrap(),
        Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    assert_eq!(store.get_scene_state(accounts[0], GAME).unwrap(), advanced);
    assert_eq!(files(&dir.path().join("state")), durable);

    let complete = complete_arrival(&advanced);
    let completed = handle.save_scene_batch(complete.clone()).unwrap().await.unwrap().unwrap().current;
    let durable = files(&dir.path().join("state"));
    for changed in ["story", "gil", "scratch", "entry", "anchor"] {
        let mut conflicting = complete.clone();
        let member = &mut conflicting.members[0];
        match changed {
            "story" => member.next_story.as_mut().unwrap().push(7),
            "gil" => member.next_gil = member.next_gil.map(|gil| gil - 1),
            "scratch" => member.next_visit.as_mut().unwrap().scratch_values[31] = 1,
            "entry" => member.next_visit.as_mut().unwrap().entry.operation = [72; 16],
            "anchor" => member.next_visit.as_mut().unwrap().anchor.push(1),
            _ => unreachable!(),
        }
        assert!(matches!(handle.save_scene_batch(conflicting).unwrap().await.unwrap(),
            Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))), "{changed}");
        assert_eq!(states(&store, &accounts), completed, "{changed}");
        assert_eq!(files(&dir.path().join("state")), durable, "{changed}");
    }
    saver.shutdown(Duration::from_secs(20)).await;
}

#[test]
fn invalid_arrival_shapes_and_bounds_are_refused_before_enqueue() {
    let (dir, store, accounts) = fixture(2);
    let (initial, begin) = resumed_arrival(&store, accounts[0]);
    let advance = advance_arrival(&initial, 1);
    let complete = complete_arrival(&initial);
    begin.validate().unwrap();
    advance.validate().unwrap();
    complete.validate().unwrap();
    let current = states(&store, &accounts);
    let durable = files(&dir.path().join("state"));
    let (handle, mut queue) = crate::saver::SaverHandle::channel();
    for bad in ["advance ordinal zero", "advance ordinal four", "complete ordinal one", "attempt",
        "zero instance", "missing checkpoint", "wrong instance", "wrong phase", "consumed battle",
        "zero source", "zero replay version", "original story", "original gil", "old visit",
        "old reservation", "new reservation", "battle", "multiple owners", "missing visit", "retained checkpoint",
        "nonzero revision", "active visit", "zero visit id", "zero entry id", "zero visit source",
        "oversized entry", "oversized anchor", "oversized field", "noncanonical scratch",
        "advance story", "advance gil", "advance visit", "advance missing checkpoint",
        "advance unchanged replay", "advance changed source", "advance changed seed", "oversized replay",
        "begin story", "begin gil", "begin visit", "begin old visit", "begin missing checkpoint",
        "begin old phase", "begin next phase", "begin same instance", "begin wrong ordinal", "begin attempt",
        "begin original story", "begin original gil", "begin source", "begin reservation", "begin battle"] {
        let mut job = if bad.starts_with("advance") { advance.clone() }
            else if bad.starts_with("begin") { begin.clone() } else { complete.clone() };
        if bad == "multiple owners" {
            let mut other = job.members[0].clone();
            other.expected.pair.story.account_id = accounts[1];
            other.expected.pair.character.as_mut().unwrap().account_id = accounts[1];
            job.members.push(other);
        } else {
            let member = &mut job.members[0];
            match bad {
                "begin story" => member.next_story = Some(member.expected.pair.story.blob.clone()),
                "begin gil" => member.next_gil = Some(character(&member.expected).gil),
                "begin visit" => member.next_visit = complete.members[0].next_visit.clone(),
                "begin old visit" => member.expected.field_visit = complete.members[0].next_visit.clone(),
                "begin missing checkpoint" => member.checkpoint = None,
                "begin old phase" => member.expected.checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 1 },
                "begin next phase" => member.checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 1 },
                "begin same instance" => member.checkpoint.as_mut().unwrap().instance = job.operation.instance,
                "begin wrong ordinal" => job.operation.ordinal = 0,
                "begin attempt" => job.operation.attempt = 1,
                "begin original story" => member.checkpoint.as_mut().unwrap().original_story.push(1),
                "begin original gil" => member.checkpoint.as_mut().unwrap().original_gil += 1,
                "begin source" => member.checkpoint.as_mut().unwrap().source_hash = [0; 32],
                "begin reservation" => member.reservation = Some(SceneReservation { instance: ARRIVAL, ordinal: 0, attempt: 0, slot: 0 }),
                "begin battle" => member.battle = Some(SceneBattleSettlement {
                    character: 0, consumed_items: vec![], reward: BattleReward::new(accounts[0], 0, 0, 0), final_limit: None,
                }),
                "advance ordinal zero" => job.operation.ordinal = 0,
                "advance ordinal four" => job.operation.ordinal = 4,
                "complete ordinal one" => job.operation.ordinal = 1,
                "attempt" => job.operation.attempt = 1,
                "zero instance" => job.operation.instance = [0; 32],
                "missing checkpoint" => member.expected.checkpoint = None,
                "wrong instance" => member.expected.checkpoint.as_mut().unwrap().instance = [84; 32],
                "wrong phase" => member.expected.checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 1 },
                "consumed battle" => member.expected.checkpoint.as_mut().unwrap().consumed_battles = 1,
                "zero source" => member.expected.checkpoint.as_mut().unwrap().source_hash = [0; 32],
                "zero replay version" => member.expected.checkpoint.as_mut().unwrap().replay_version = 0,
                "original story" => member.expected.checkpoint.as_mut().unwrap().original_story.push(1),
                "original gil" => member.expected.checkpoint.as_mut().unwrap().original_gil += 1,
                "old visit" => member.expected.field_visit = member.next_visit.clone(),
                "old reservation" => member.expected.reservation = Some(SceneReservation { instance: ARRIVAL, ordinal: 0, attempt: 0, slot: 0 }),
                "new reservation" => member.reservation = Some(SceneReservation { instance: ARRIVAL, ordinal: 0, attempt: 0, slot: 0 }),
                "battle" => member.battle = Some(SceneBattleSettlement {
                    character: 0, consumed_items: vec![], reward: BattleReward::new(accounts[0], 0, 0, 0), final_limit: None,
                }),
                "missing visit" => member.next_visit = None,
                "retained checkpoint" => member.checkpoint = member.expected.checkpoint.clone(),
                "nonzero revision" => member.next_visit.as_mut().unwrap().revision = 1,
                "active visit" => member.next_visit.as_mut().unwrap().active_continuation = Some(ARRIVAL),
                "zero visit id" => member.next_visit.as_mut().unwrap().visit_id = [0; 16],
                "zero entry id" => member.next_visit.as_mut().unwrap().entry.operation = [0; 16],
                "zero visit source" => member.next_visit.as_mut().unwrap().source_identity = [0; 32],
                "oversized entry" => member.next_visit.as_mut().unwrap().entry.arrival = vec![1; MAX_FIELD_VISIT_ENTRY_BYTES + 1],
                "oversized anchor" => member.next_visit.as_mut().unwrap().anchor = vec![1; MAX_FIELD_VISIT_ANCHOR_BYTES + 1],
                "oversized field" => member.next_visit.as_mut().unwrap().field = "f".repeat(MAX_FIELD_VISIT_FIELD_BYTES + 1),
                "noncanonical scratch" => {
                    let visit = member.next_visit.as_mut().unwrap();
                    visit.scratch_values[30] = 1;
                    visit.scratch_known_masks[30] = 0;
                }
                "advance story" => member.next_story = Some(member.expected.pair.story.blob.clone()),
                "advance gil" => member.next_gil = Some(character(&member.expected).gil),
                "advance visit" => member.next_visit = complete.members[0].next_visit.clone(),
                "advance missing checkpoint" => member.checkpoint = None,
                "advance unchanged replay" => member.checkpoint = member.expected.checkpoint.clone(),
                "advance changed source" => member.checkpoint.as_mut().unwrap().source_hash = [74; 32],
                "advance changed seed" => member.checkpoint.as_mut().unwrap().seed += 1,
                "oversized replay" => member.expected.checkpoint.as_mut().unwrap().replay = vec![0; MAX_SCENE_REPLAY_BYTES + 1],
                _ => unreachable!(),
            }
        }
        assert!(matches!(handle.save_scene_batch(job), Err(SceneSaveError::InvalidRequest)), "{bad}");
        assert!(matches!(queue.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)), "{bad}");
        assert_eq!(states(&store, &accounts), current, "{bad}");
        assert_eq!(files(&dir.path().join("state")), durable, "{bad}");
    }
}
