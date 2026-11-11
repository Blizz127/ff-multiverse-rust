//! An admitted arrival reserves every receipt it will need before changing its owner’s
//! checkpoint. Later acknowledgments cannot consume the final completion slot.

use super::*;
use crate::{FieldVisitEntryV1, FieldVisitV1, StoreCompatibility};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const OLD_INSTANCE: [u8; 32] = [81; 32];
const ARRIVAL_INSTANCE: [u8; 32] = [82; 32];
const PRIOR_BATTLES: u32 = 2;

fn operation(kind: SceneOperationKind, ordinal: u32) -> SceneOperation {
    SceneOperation { instance: if matches!(kind, SceneOperationKind::BeginArrival | SceneOperationKind::ContinueLanding) {
        OLD_INSTANCE
    } else { ARRIVAL_INSTANCE }, ordinal, attempt: 0, kind }
}

fn selected(operation: &SceneOperation) -> Vec<u8> {
    format!("arrival capacity {:?}/{}", operation.kind, operation.ordinal).into_bytes()
}

fn record(store: &Store, account: AccountId) -> CharacterRecord {
    CharacterRecord::read(&store.character_path(account.0, GAME).unwrap()).unwrap().unwrap()
}

fn receipt_count(record: &CharacterRecord) -> usize { record.scene_receipts.len() + record.batch_receipts.len() }

fn fixture(free: usize, landing: bool) -> (tempfile::TempDir, Store, AccountId, SceneState) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Receipt budget", 1).unwrap();
    let mut progress = CharacterProgress::new(account, GAME, 0, 7, 2);
    progress.gil = 240;
    progress.exp = 313;
    progress.ap = 11;
    progress.limit = 137;
    progress.inventory = Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 2)]);
    store.put_character_progress(&progress).unwrap();
    store.put_progression(&Progression::new(account, GAME, vec![1, 48, 0], 2)).unwrap();
    store.require_compatibility(StoreCompatibility::DurableStory).unwrap();
    let path = store.character_path(account.0, GAME).unwrap();
    let mut stored = record(&store, account);
    let count = MAX_SCENE_RECEIPTS - free;
    // Both receipt families count against the same budget; no family is evicted or ignored.
    stored.scene_receipts = (0..count / 2).map(|n| (format!("{n:064x}"), "b".repeat(64))).collect();
    stored.batch_receipts = (count / 2..count).map(|n| (format!("{n:064x}"), BatchReceipt {
        result: "c".repeat(64), members: 1, slot: 0,
    })).collect();
    stored.checkpoint = Some(SceneCheckpoint {
        instance: if landing { OLD_INSTANCE } else { ARRIVAL_INSTANCE },
        scene_key: if landing { "fixture/previous-scene" } else { "fixture/arrival" }.into(),
        source_hash: [83; 32], replay_version: 1, original_story: vec![1, 48, 0], original_gil: 240,
        seed: 7, replay: vec![1, 0],
        phase: if landing { ScenePhase::Landing { destination: vec![3, 4, 5] } } else { ScenePhase::Resume { segment: 0 } },
        consumed_battles: if landing { PRIOR_BATTLES } else { 0 },
    });
    stored.stamp();
    stored.validate().unwrap();
    fsio::write_json(&path, &stored, MAX_CHARACTER_RECORD_BYTES).unwrap();
    assert_eq!(receipt_count(&record(&store, account)), MAX_SCENE_RECEIPTS - free);
    let before = store.get_scene_state(account, GAME).unwrap();
    (dir, store, account, before)
}

fn begin(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    next.checkpoint = Some(SceneCheckpoint {
        instance: ARRIVAL_INSTANCE, scene_key: "fixture/arrival".into(), source_hash: [84; 32],
        replay_version: 1, original_story: before.pair.story.blob.clone(),
        original_gil: before.pair.character.as_ref().unwrap().gil, seed: 9,
        replay: vec![2, 0], phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    next
}

fn advance(before: &SceneState, ordinal: u32) -> SceneState {
    let mut next = before.clone();
    next.checkpoint.as_mut().unwrap().replay.push(u8::try_from(ordinal).unwrap());
    next
}

fn visit() -> FieldVisitV1 {
    FieldVisitV1 {
        schema_version: 1, visit_id: [85; 16], revision: 0, field: "fixture/arrival".into(),
        source_identity: [86; 32], entry: FieldVisitEntryV1 { operation: [87; 16], arrival: vec![1, 2, 3] },
        anchor: vec![4, 5, 6], scratch_values: [0; 256], scratch_known_masks: [255; 256], active_continuation: None,
    }
}

fn complete(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    next.checkpoint = None;
    next.field_visit = Some(visit());
    next.pair.story.blob.push(51);
    next.pair.story.updated_at += 1;
    next.pair.character.as_mut().unwrap().updated_at += 1;
    next
}

fn next_for(operation: &SceneOperation, before: &SceneState) -> SceneState {
    match operation.kind {
        SceneOperationKind::BeginArrival | SceneOperationKind::ContinueLanding => begin(before),
        SceneOperationKind::AdvanceArrival => advance(before, operation.ordinal),
        SceneOperationKind::CompleteArrivalVisit => complete(before),
        _ => unreachable!(),
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() { collect(root, &path, out); }
            else if entry.file_name() != "LOCK" { out.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap()); }
        }
    }
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

#[test]
fn five_free_receipts_admit_the_whole_arrival_and_old_retries_still_succeed_when_full() {
    assert_eq!(MAX_ARRIVAL_METADATA_STEPS, 3);
    let (dir, store, account, before) = fixture(5, true);
    let original = record(&store, account);
    let operations = [operation(SceneOperationKind::BeginArrival, PRIOR_BATTLES),
        operation(SceneOperationKind::AdvanceArrival, 1), operation(SceneOperationKind::AdvanceArrival, 2),
        operation(SceneOperationKind::AdvanceArrival, 3), operation(SceneOperationKind::CompleteArrivalVisit, 0)];
    let mut current = before;
    let mut requests = Vec::new();
    for (index, operation) in operations.into_iter().enumerate() {
        let next = next_for(&operation, &current);
        let result = selected(&operation);
        let saved = store.commit_scene_batch(&operation, &result, std::slice::from_ref(&current), std::slice::from_ref(&next)).unwrap();
        assert_eq!(saved.commit, SceneCommit::Applied);
        requests.push((operation, result, current, next));
        current = saved.current.into_iter().next().unwrap();
        assert_eq!(receipt_count(&record(&store, account)), MAX_SCENE_RECEIPTS - 4 + index);
        assert_eq!(store.index().format_version(), 5);
    }
    let final_record = record(&store, account);
    assert_eq!(receipt_count(&final_record), MAX_SCENE_RECEIPTS);
    assert_eq!(final_record.scene_receipts, original.scene_receipts);
    assert!(original.batch_receipts.iter().all(|(key, value)| final_record.batch_receipts.get(key) == Some(value)));
    assert!(current.checkpoint.is_none() && current.field_visit.is_some());
    store.update_character_progress(account, GAME, |progress| {
        let mut progress = progress.unwrap();
        progress.gil += 17;
        progress.exp += 13;
        progress.updated_at += 1;
        progress
    }).unwrap();
    let later = store.get_scene_state(account, GAME).unwrap();
    let durable = files(dir.path());
    for (operation, result, before, next) in &requests {
        let retry = store.commit_scene_batch(operation, result, std::slice::from_ref(before), std::slice::from_ref(next)).unwrap();
        assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
        assert_eq!(retry.current, vec![later.clone()], "receipt lookup precedes the now-exhausted capacity check");
        assert!(store.scene_batch_applied(operation, result, &[account], GAME).unwrap());
    }
    assert_eq!(files(dir.path()), durable);
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), later);
    for (operation, result, before, next) in requests {
        assert_eq!(reopened.commit_scene_batch(&operation, &result, &[before], &[next]).unwrap().current, vec![later.clone()]);
    }
}

#[test]
fn each_arrival_step_requires_its_remaining_budget_before_writing_anything() {
    let steps = [(operation(SceneOperationKind::BeginArrival, PRIOR_BATTLES), 5usize),
        (operation(SceneOperationKind::AdvanceArrival, 1), 4), (operation(SceneOperationKind::AdvanceArrival, 2), 3),
        (operation(SceneOperationKind::AdvanceArrival, 3), 2), (operation(SceneOperationKind::CompleteArrivalVisit, 0), 1)];
    for (operation, required) in steps {
        for free in [required - 1, required] {
            let (dir, store, account, before) = fixture(free, operation.kind == SceneOperationKind::BeginArrival);
            let next = next_for(&operation, &before);
            assert!(validate_transition(&operation, std::slice::from_ref(&before), std::slice::from_ref(&next)).is_ok());
            let original = files(dir.path());
            let result = store.commit_scene_batch(&operation, &selected(&operation), std::slice::from_ref(&before), std::slice::from_ref(&next));
            if free < required {
                assert!(result.is_err(), "{:?}/{} needs {required} slots", operation.kind, operation.ordinal);
                assert_eq!(files(dir.path()), original, "capacity refusal creates neither an intent nor a compatibility upgrade");
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
                assert_eq!(store.index().format_version(), 4);
            } else {
                assert_eq!(result.unwrap().commit, SceneCommit::Applied);
                assert_eq!(receipt_count(&record(&store, account)), MAX_SCENE_RECEIPTS - free + 1);
            }
        }
    }
}

#[test]
fn legacy_continue_landing_still_needs_only_its_one_receipt() {
    let (_dir, store, account, before) = fixture(1, true);
    let operation = operation(SceneOperationKind::ContinueLanding, PRIOR_BATTLES);
    let next = begin(&before);
    let saved = store.commit_scene_batch(&operation, b"legacy continuation", std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, vec![next]);
    assert_eq!(receipt_count(&record(&store, account)), MAX_SCENE_RECEIPTS);
    assert_eq!(store.index().format_version(), 4, "the new arrival admission contract does not alter legacy continuation fences");
}

#[test]
fn begin_arrival_refuses_changed_gameplay_invalid_checkpoints_and_unfrozen_membership() {
    for changed in ["no checkpoint", "not landing", "wrong instance", "ordinal", "attempt", "same instance", "zero instance",
        "zero source", "new phase", "consumed", "new original story", "new original gil", "old reserved", "new reserved",
        "old visit", "new visit", "story", "story time", "gil", "character time", "exp", "inventory", "cohort"] {
        let (dir, store, _account, before) = fixture(5, true);
        let mut old = before.clone();
        let mut next = begin(&before);
        let mut operation = operation(SceneOperationKind::BeginArrival, PRIOR_BATTLES);
        match changed {
            "no checkpoint" => old.checkpoint = None,
            "not landing" => { let cp = old.checkpoint.as_mut().unwrap(); cp.phase = ScenePhase::Resume { segment: 0 }; cp.consumed_battles = 0; operation.ordinal = 0; },
            "wrong instance" => operation.instance = [88; 32],
            "ordinal" => operation.ordinal += 1,
            "attempt" => operation.attempt = 1,
            "same instance" => next.checkpoint.as_mut().unwrap().instance = OLD_INSTANCE,
            "zero instance" => next.checkpoint.as_mut().unwrap().instance = [0; 32],
            "zero source" => next.checkpoint.as_mut().unwrap().source_hash = [0; 32],
            "new phase" => next.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: vec![1] },
            "consumed" => { let cp = next.checkpoint.as_mut().unwrap(); cp.phase = ScenePhase::Resume { segment: 1 }; cp.consumed_battles = 1; },
            "new original story" => next.checkpoint.as_mut().unwrap().original_story.push(199),
            "new original gil" => next.checkpoint.as_mut().unwrap().original_gil += 1,
            "old reserved" => old.reservation = Some(SceneReservation { instance: OLD_INSTANCE, ordinal: 0, attempt: 0, slot: 0 }),
            "new reserved" => next.reservation = Some(SceneReservation { instance: ARRIVAL_INSTANCE, ordinal: 0, attempt: 0, slot: 0 }),
            "old visit" => { let mut value = visit(); value.active_continuation = Some(OLD_INSTANCE); old.field_visit = Some(value); },
            "new visit" => { let mut value = visit(); value.active_continuation = Some(ARRIVAL_INSTANCE); next.field_visit = Some(value); },
            "story" => next.pair.story.blob.push(51),
            "story time" => next.pair.story.updated_at += 1,
            "gil" => next.pair.character.as_mut().unwrap().gil -= 1,
            "character time" => next.pair.character.as_mut().unwrap().updated_at += 1,
            "exp" => next.pair.character.as_mut().unwrap().exp += 1,
            "inventory" => next.pair.character.as_mut().unwrap().inventory = None,
            "cohort" => {},
            _ => unreachable!(),
        }
        let (old, next) = if changed == "cohort" { (vec![old.clone(), old], vec![next.clone(), next]) } else { (vec![old], vec![next]) };
        assert!(validate_transition(&operation, &old, &next).is_err(), "{changed} must fail structure, not merely a stale compare-and-swap");
        let original = files(dir.path());
        assert!(store.commit_scene_batch(&operation, &selected(&operation), &old, &next).is_err(), "{changed}");
        assert_eq!(files(dir.path()), original, "{changed}");
    }
}

#[test]
fn recovery_revalidates_the_reserved_budget_even_when_each_record_and_checksum_is_valid() {
    for (operation, required) in [(operation(SceneOperationKind::BeginArrival, PRIOR_BATTLES), 5usize),
        (operation(SceneOperationKind::AdvanceArrival, 1), 4), (operation(SceneOperationKind::AdvanceArrival, 2), 3),
        (operation(SceneOperationKind::AdvanceArrival, 3), 2)] {
        let (dir, store, account, before) = fixture(required, operation.kind == SceneOperationKind::BeginArrival);
        let next = next_for(&operation, &before);
        store.commit_scene_batch_with_hook(&operation, &selected(&operation), std::slice::from_ref(&before), std::slice::from_ref(&next),
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        let path = dir.path().join(INTENT_FILE);
        let character_path = store.character_path(account.0, GAME).unwrap();
        let mut file: BatchIntentFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let row = file.intent.rows.first_mut().unwrap();
        let key = "d".repeat(64);
        for images in [&mut row.before, &mut row.after] {
            let record = images.character.as_mut().unwrap();
            assert!(record.scene_receipts.insert(key.clone(), "e".repeat(64)).is_none());
            record.validate().unwrap();
        }
        let before_record = row.before.character.as_ref().unwrap().clone();
        assert_eq!(receipt_count(&before_record), MAX_SCENE_RECEIPTS - required + 1);
        assert!(validate_transition(&file.intent.operation,
            &[row.before.state(account, GAME)], &[row.after.state(account, GAME)]).is_ok());
        assert!(store.validate_batch_intent(&file.intent).is_err(), "the private reserved-capacity precondition is independently validated");
        file.checksum = digest(&file.intent).unwrap();
        drop(store);
        // Match the forged before-image too: recovery cannot reject only because it found
        // an unrelated record, and appending this receipt still stays below the global cap.
        fsio::write_json(&character_path, &before_record, MAX_CHARACTER_RECORD_BYTES).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let evidence = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{:?}/{}", operation.kind, operation.ordinal);
        assert_eq!(files(dir.path()), evidence, "insufficient future slots cannot be repaired by silently admitting the arrival");
    }
}

#[test]
fn begin_arrival_reserves_its_capacity_at_all_six_commit_boundaries() {
    for crash_at in 0..=5 {
        let (dir, store, account, before) = fixture(5, true);
        let operation = operation(SceneOperationKind::BeginArrival, PRIOR_BATTLES);
        let next = begin(&before);
        let original = files(dir.path());
        store.commit_scene_batch_with_hook(&operation, &selected(&operation), std::slice::from_ref(&before), std::slice::from_ref(&next),
            |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        if crash_at == 0 {
            assert_eq!(files(dir.path()), original);
            assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
        } else {
            assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
        }
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), if crash_at == 0 { before.clone() } else { next.clone() });
        let retry = reopened.commit_scene_batch(&operation, &selected(&operation), &[before], std::slice::from_ref(&next)).unwrap();
        assert_eq!(retry.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
        assert_eq!(retry.current, vec![next]);
        assert_eq!(receipt_count(&record(&reopened, account)), MAX_SCENE_RECEIPTS - 4);
        assert_eq!(reopened.index().format_version(), 5);
    }
}
