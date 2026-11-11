//! Arrival acknowledgments persist only a replay prefix. Their receipt survives later
//! prefixes and completion; no story, resource or scratch afterimage is published early.

use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use crate::{FieldVisitEntryV1, FieldVisitV1};

const ARRIVAL_GAME: GameId = GameId::Fixture;
const ARRIVAL_INSTANCE: [u8; 32] = [33; 32];

fn fixture() -> (tempfile::TempDir, Store, AccountId, SceneState) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Arrival acknowledgments", 1).unwrap();
    let mut character = CharacterProgress::new(account, ARRIVAL_GAME, 1, 7, 2);
    character.gil = 240;
    character.exp = 317;
    character.ap = 9;
    character.limit = 133;
    character.inventory = Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 2)]);
    store.put_character_progress(&character).unwrap();
    store.put_progression(&Progression::new(account, ARRIVAL_GAME, vec![1, 48, 0], 2)).unwrap();
    let pair = store.get_progress_pair(account, ARRIVAL_GAME).unwrap();
    store.commit_scene_progress_with_result("earlier ordinary scene", b"retained receipt", &pair, &pair).unwrap();
    let state = store.get_scene_state(account, ARRIVAL_GAME).unwrap();
    store.commit_scene_batch(&SceneOperation { instance: [31; 32], ordinal: 0, attempt: 0,
        kind: SceneOperationKind::BattleOutcome }, b"earlier battle", std::slice::from_ref(&state), std::slice::from_ref(&state)).unwrap();
    // A real previous scene supplies this checkpoint in production. Seed its exact valid
    // stored envelope here, retaining both kinds of completed receipts from real commits.
    let path = store.character_path(account.0, ARRIVAL_GAME).unwrap();
    let mut record = CharacterRecord::read(&path).unwrap().unwrap();
    record.checkpoint = Some(SceneCheckpoint {
        instance: ARRIVAL_INSTANCE, scene_key: "fixture/arrival".into(), source_hash: [34; 32],
        replay_version: 1, original_story: pair.story.blob, original_gil: 240, seed: 5,
        replay: vec![1, 0], phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    record.stamp();
    record.validate().unwrap();
    fsio::write_json(&path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    let state = store.get_scene_state(account, ARRIVAL_GAME).unwrap();
    assert_eq!(store.index().format_version(), 4);
    (dir, store, account, state)
}

fn operation(ordinal: u32) -> SceneOperation {
    SceneOperation { instance: ARRIVAL_INSTANCE, ordinal, attempt: 0, kind: SceneOperationKind::AdvanceArrival }
}

fn advanced(before: &SceneState, prefix: u8) -> SceneState {
    let mut next = before.clone();
    next.checkpoint.as_mut().unwrap().replay.push(prefix);
    next
}

fn visit() -> FieldVisitV1 {
    FieldVisitV1 {
        schema_version: 1, visit_id: [35; 16], revision: 0, field: "fixture/arrival".into(),
        source_identity: [36; 32], entry: FieldVisitEntryV1 { operation: [37; 16], arrival: vec![1, 2, 3] },
        anchor: vec![4, 5, 6], scratch_values: [0; 256], scratch_known_masks: [255; 256], active_continuation: None,
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() { collect(root, &path, out); }
            else if entry.file_name() != "LOCK" {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn set_header(root: &Path, version: u32) {
    // Deliberately forge only the checked header. Supported compaction must never lower it.
    let path = root.join(crate::index::INDEX_LOG_FILE);
    let bytes = fs::read(&path).unwrap();
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let line = bytes.get(..end).unwrap();
    let space = line.iter().position(|byte| *byte == b' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(line.get(space + 1..).unwrap()).unwrap();
    header["schema_version"] = version.into();
    let payload = serde_json::to_vec(&header).unwrap();
    let hash = blake3::hash(&payload).to_hex().to_string();
    let mut replaced = format!("{} ", hash.get(..space).unwrap()).into_bytes();
    replaced.extend_from_slice(&payload);
    replaced.extend_from_slice(bytes.get(end..).unwrap());
    fs::write(path, replaced).unwrap();
}

#[test]
fn arrival_acknowledgments_keep_the_pair_and_return_current_after_later_prefixes_and_completion() {
    let (dir, store, account, before) = fixture();
    let path = store.character_path(account.0, ARRIVAL_GAME).unwrap();
    let prior = CharacterRecord::read(&path).unwrap().unwrap();
    let first = advanced(&before, 1);
    let saved = store.commit_scene_batch(&operation(1), b"first arrival window", std::slice::from_ref(&before), std::slice::from_ref(&first)).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, vec![first.clone()]);
    assert_eq!(saved.current[0].pair, before.pair, "timestamps and every gameplay value remain exact");
    assert_eq!(store.index().format_version(), 5, "the operation needs its fence even though its character remains schema3");
    let current = CharacterRecord::read(&path).unwrap().unwrap();
    assert_eq!(current.progress.schema_version, 3);
    assert_eq!(current.scene_receipts, prior.scene_receipts);
    assert!(prior.batch_receipts.iter().all(|(key, value)| current.batch_receipts.get(key) == Some(value)));
    assert_eq!(current.batch_receipts.len(), prior.batch_receipts.len() + 1);
    assert!(current.field_visit.is_none() && current.last_visit_commit.is_none());

    let second = advanced(&first, 2);
    store.commit_scene_batch(&operation(2), b"second arrival window", std::slice::from_ref(&first), std::slice::from_ref(&second)).unwrap();
    let retry = store.commit_scene_batch(&operation(1), b"first arrival window", std::slice::from_ref(&before), std::slice::from_ref(&first)).unwrap();
    assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
    assert_eq!(retry.current, vec![second.clone()]);

    let third = advanced(&second, 3);
    store.commit_scene_batch(&operation(3), b"terminal presentation sealed", std::slice::from_ref(&second), std::slice::from_ref(&third)).unwrap();
    let mut complete = third.clone();
    complete.checkpoint = None;
    complete.field_visit = Some(visit());
    complete.pair.story.blob.push(51);
    complete.pair.story.updated_at += 1;
    complete.pair.character.as_mut().unwrap().updated_at += 1;
    store.commit_scene_batch(&SceneOperation { ordinal: 0, kind: SceneOperationKind::CompleteArrivalVisit, ..operation(1) },
        b"accepted arrival complete", std::slice::from_ref(&third), &[complete]).unwrap();
    store.update_character_progress(account, ARRIVAL_GAME, |character| {
        let mut character = character.unwrap();
        character.gil += 19;
        character.exp += 7;
        character.updated_at += 1;
        character
    }).unwrap();
    let later = store.get_scene_state(account, ARRIVAL_GAME).unwrap();
    let durable = files(dir.path());
    assert_eq!(store.commit_scene_batch(&operation(1), b"first arrival window", &[before], &[first]).unwrap().current, vec![later.clone()]);
    assert!(store.scene_batch_applied(&operation(2), b"second arrival window", &[account], ARRIVAL_GAME).unwrap());
    assert_eq!(store.get_scene_state(account, ARRIVAL_GAME).unwrap(), later);
    assert_eq!(files(dir.path()), durable, "old acknowledgments neither rewind replay nor erase the ready visit/reward");
}

#[test]
fn all_six_arrival_acknowledgment_failure_boundaries_recover_without_publishing_gameplay_changes() {
    for crash_at in 0..=5 {
        let (dir, store, account, before) = fixture();
        let next = advanced(&before, 1);
        let original = files(dir.path());
        let mut stages = Vec::new();
        let failed = store.commit_scene_batch_with_hook(&operation(1), b"arrival window", std::slice::from_ref(&before), std::slice::from_ref(&next), |stage| {
            stages.push(stage);
            if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }
        });
        assert!(failed.is_err());
        assert_eq!(stages, (0..=crash_at).collect::<Vec<_>>());
        if crash_at == 0 {
            assert_eq!(files(dir.path()), original);
            assert_eq!(store.index().format_version(), 4);
            assert_eq!(store.get_scene_state(account, ARRIVAL_GAME).unwrap(), before);
        } else {
            assert_eq!(store.index().format_version(), 5);
            assert!(matches!(store.get_scene_state(account, ARRIVAL_GAME), Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.commit_scene_batch(&operation(1), b"arrival window", std::slice::from_ref(&before), std::slice::from_ref(&next)),
                Err(PersistError::TransactionRecoveryRequired)));
        }
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_state(account, ARRIVAL_GAME).unwrap(), if crash_at == 0 { before.clone() } else { next.clone() });
        let saved = reopened.commit_scene_batch(&operation(1), b"arrival window", std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
        assert_eq!(saved.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
        assert_eq!(saved.current, vec![next]);
        assert_eq!(saved.current[0].pair, before.pair);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

#[test]
fn arrival_acknowledgments_refuse_state_mutations_and_invalid_boundaries_before_any_write() {
    type Mutation = (&'static str, fn(&mut SceneState, &mut SceneState));
    let changes: &[Mutation] = &[
        ("story", |_, next| next.pair.story.blob.push(9)),
        ("story timestamp", |_, next| next.pair.story.updated_at += 1),
        ("gil", |_, next| next.pair.character.as_mut().unwrap().gil += 1),
        ("character timestamp", |_, next| next.pair.character.as_mut().unwrap().updated_at += 1),
        ("inventory", |_, next| next.pair.character.as_mut().unwrap().inventory = None),
        ("source", |_, next| next.checkpoint.as_mut().unwrap().source_hash = [8; 32]),
        ("zero source", |old, next| { old.checkpoint.as_mut().unwrap().source_hash = [0; 32]; next.checkpoint.as_mut().unwrap().source_hash = [0; 32]; }),
        ("original story", |_, next| next.checkpoint.as_mut().unwrap().original_story.push(7)),
        ("original gil", |_, next| next.checkpoint.as_mut().unwrap().original_gil += 1),
        ("seed", |_, next| next.checkpoint.as_mut().unwrap().seed += 1),
        ("replay version", |_, next| next.checkpoint.as_mut().unwrap().replay_version += 1),
        ("instance", |_, next| next.checkpoint.as_mut().unwrap().instance = [8; 32]),
        ("scene", |_, next| next.checkpoint.as_mut().unwrap().scene_key.push('x')),
        ("missing checkpoint", |_, next| next.checkpoint = None),
        ("unchanged replay", |old, next| next.checkpoint.as_mut().unwrap().replay = old.checkpoint.as_ref().unwrap().replay.clone()),
        ("landing", |_, next| next.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: vec![1] }),
        ("battle prefix", |_, next| { let cp = next.checkpoint.as_mut().unwrap(); cp.phase = ScenePhase::Resume { segment: 1 }; cp.consumed_battles = 1; }),
        ("reservation", |old, next| { let r = SceneReservation { instance: ARRIVAL_INSTANCE, ordinal: 0, attempt: 0, slot: 0 }; old.reservation = Some(r.clone()); next.reservation = Some(r); }),
        ("visit", |old, next| { let mut v = visit(); v.active_continuation = Some(ARRIVAL_INSTANCE); old.field_visit = Some(v.clone()); next.field_visit = Some(v); }),
    ];
    let (dir, store, account, before) = fixture();
    let original = files(dir.path());
    for (name, mutate) in changes {
        let mut old = before.clone();
        let mut next = advanced(&old, 1);
        mutate(&mut old, &mut next);
        assert!(validate_transition(&operation(1), std::slice::from_ref(&old), std::slice::from_ref(&next)).is_err(), "{name}");
        assert!(store.commit_scene_batch(&operation(1), b"invalid", &[old], &[next]).is_err(), "{name}");
        assert_eq!(files(dir.path()), original, "{name}");
    }
    let next = advanced(&before, 1);
    for (ordinal, attempt) in [(0, 0), (crate::MAX_ARRIVAL_METADATA_STEPS + 1, 0), (1, 1)] {
        assert!(store.commit_scene_batch(&SceneOperation { ordinal, attempt, ..operation(1) }, b"invalid boundary",
            std::slice::from_ref(&before), std::slice::from_ref(&next)).is_err());
        assert_eq!(files(dir.path()), original);
    }
    for instance in [[0; 32], [99; 32]] {
        assert!(store.commit_scene_batch(&SceneOperation { instance, ..operation(1) }, b"wrong instance",
            std::slice::from_ref(&before), std::slice::from_ref(&next)).is_err());
    }
    assert!(store.commit_scene_batch(&operation(1), b"empty", &[], &[]).is_err());
    let mut other = before.clone();
    other.pair.story.account_id = AccountId(account.0 + 1);
    other.pair.character.as_mut().unwrap().account_id = other.pair.story.account_id;
    let other_next = advanced(&other, 1);
    assert!(validate_transition(&operation(1), &[before.clone(), other.clone()], &[next.clone(), other_next.clone()]).is_err(),
        "two distinct accounts cannot share an arrival acknowledgment");
    assert!(store.commit_scene_batch(&operation(1), b"cohort", &[before.clone(), other], &[next.clone(), other_next]).is_err());
    // A valid but stale full predecessor must not bypass CAS through a replay-only update.
    let mut stale = before.clone();
    stale.pair.character.as_mut().unwrap().exp += 1;
    let stale_next = advanced(&stale, 1);
    assert!(validate_transition(&operation(1), std::slice::from_ref(&stale), std::slice::from_ref(&stale_next)).is_ok());
    assert!(matches!(store.commit_scene_batch(&operation(1), b"stale", &[stale], &[stale_next]), Err(PersistError::TransactionConflict(_))));
    assert_eq!(store.get_scene_state(account, ARRIVAL_GAME).unwrap(), before);
    assert_eq!(files(dir.path()), original);
}

#[test]
fn arrival_acknowledgment_recovery_requires_index_five_despite_schema_three_images() {
    for lower in [3, 4] {
        let (dir, store, account, before) = fixture();
        let next = advanced(&before, 1);
        store.commit_scene_batch_with_hook(&operation(1), b"arrival window", std::slice::from_ref(&before), std::slice::from_ref(&next),
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        set_header(dir.path(), lower);
        let original = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "index {lower}");
        assert_eq!(files(dir.path()), original, "an inadequate fence cannot authorize any intent installation");
        set_header(dir.path(), 5);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_state(account, ARRIVAL_GAME).unwrap(), next);
        assert_eq!(reopened.get_character_progress(account, ARRIVAL_GAME).unwrap().unwrap().schema_version, 3);
    }
}

#[test]
fn invalid_or_unknown_arrival_intents_refuse_before_installing_any_record() {
    for mutation in ["resource", "unchanged replay", "future operation"] {
        let (dir, store, _account, before) = fixture();
        let next = advanced(&before, 1);
        store.commit_scene_batch_with_hook(&operation(1), b"arrival window", &[before], &[next],
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        let path = dir.path().join(INTENT_FILE);
        let mut file: BatchIntentFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if mutation == "future operation" {
            // Use the same typed-intent field order and checksum algorithm, with only the
            // operation name replaced. Enum decoding must fail before recovery mutation.
            let intent = serde_json::to_string(&file.intent).unwrap().replace("\"AdvanceArrival\"", "\"FutureAdvanceArrival\"");
            let mut value = serde_json::to_value(&file).unwrap();
            value["intent"] = serde_json::from_str(&intent).unwrap();
            value["checksum"] = blake3::hash(intent.as_bytes()).to_hex().to_string().into();
            fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        } else {
            let row = file.intent.rows.first_mut().unwrap();
            if mutation == "resource" { row.after.character.as_mut().unwrap().progress.gil += 1; }
            else {
                row.after.character.as_mut().unwrap().checkpoint.as_mut().unwrap().replay = row.before.character.as_ref().unwrap().checkpoint.as_ref().unwrap().replay.clone();
            }
            file.checksum = digest(&file.intent).unwrap();
            fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        }
        let original = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{mutation}");
        assert_eq!(files(dir.path()), original, "{mutation}: refusal retains both records and the recoverable evidence");
    }
}
