//! Arrival completion installs the terminal pair, first visit and both receipts as one
//! recoverable batch. No intermediate checkpoint-free state is ever acknowledged.

use super::*;
use crate::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const RESULT: &[u8] = b"checked arrival completed with terminal scratch and placement";
const ENTRY_ID: [u8; 16] = [61; 16];

fn operation() -> SceneOperation { op(SceneOperationKind::CompleteArrivalVisit) }

fn fixture() -> (tempfile::TempDir, Store, AccountId, SceneState) {
    let (dir, store, accounts) = rig();
    let account = accounts[0];
    // Retain both pre-existing receipt families, rather than checking empty maps only.
    let pair = store.get_progress_pair(account, GAME).unwrap();
    let mut changed = pair.clone();
    changed.story.blob.push(11);
    store.commit_scene_progress("fixture/earlier-arrival", &pair, &changed).unwrap();
    let idle = store.get_scene_state(account, GAME).unwrap();
    let earlier = SceneOperation { instance: [62; 32], ..op(SceneOperationKind::BattleOutcome) };
    store.commit_scene_batch(&earlier, b"earlier completed fight", std::slice::from_ref(&idle), std::slice::from_ref(&idle)).unwrap();
    // A synthetic, validated imported arrival checkpoint: no visit or frozen battle roster.
    let path = store.character_path(account.0, GAME).unwrap();
    let mut record = CharacterRecord::read(&path).unwrap().unwrap();
    record.checkpoint = Some(SceneCheckpoint {
        instance: INSTANCE, scene_key: "fixture/train-arrival".into(), source_hash: [29; 32],
        replay_version: 1, original_story: changed.story.blob, original_gil: record.progress.gil,
        seed: 5, replay: vec![1, 3, 8], phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    record.stamp();
    record.validate().unwrap();
    fsio::write_json(&path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    let before = store.get_scene_state(account, GAME).unwrap();
    assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    assert!(before.field_visit.is_none() && record.last_visit_commit.is_none());
    (dir, store, account, before)
}

fn after(before: &SceneState, large: bool) -> SceneState {
    let mut next = before.clone();
    next.checkpoint = None;
    next.pair.story.blob.push(51);
    next.pair.story.updated_at = 8;
    let character = next.pair.character.as_mut().unwrap();
    character.gil -= 3;
    character.updated_at = 8;
    let mut values = [0; 256];
    let mut masks = [0; 256];
    values[31] = 1;
    masks[31] = 3;
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [63; 16], revision: 0, field: "fixture/train".into(),
        source_identity: [30; 32],
        entry: FieldVisitEntryV1 { operation: ENTRY_ID, arrival: vec![255; if large { 8192 } else { 7 }] },
        anchor: vec![254; if large { 8192 } else { 9 }], scratch_values: values, scratch_known_masks: masks,
        active_continuation: None,
    });
    next
}

fn stored(mut state: SceneState, large: bool) -> SceneState {
    state.pair.character.as_mut().unwrap().schema_version = if large { 5 } else { 4 };
    state
}

fn record(store: &Store, account: AccountId) -> CharacterRecord {
    CharacterRecord::read(&store.character_path(account.0, GAME).unwrap()).unwrap().unwrap()
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { collect(root, &entry.path(), out); }
            else if entry.file_name() != "LOCK" {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn check_receipts(previous: &CharacterRecord, current: &CharacterRecord, before: &SceneState, after: &SceneState) {
    assert_eq!(current.scene_receipts, previous.scene_receipts);
    assert_eq!(current.batch_receipts.len(), previous.batch_receipts.len() + 1);
    for (key, receipt) in &previous.batch_receipts { assert_eq!(current.batch_receipts.get(key), Some(receipt)); }
    let batch = current.batch_receipts.get(&operation_key(&operation()).unwrap()).unwrap();
    assert_eq!((batch.members, batch.slot), (1, 0));
    assert_eq!(batch.result, selected_key(RESULT).unwrap());
    let receipt = serde_json::to_value(current.last_visit_commit.as_ref().unwrap()).unwrap();
    assert_eq!(receipt["operation"], serde_json::to_value(ENTRY_ID).unwrap());
    let expected = digest(&("fmv-scene-arrival-visit-v1", operation(), selected_key(RESULT).unwrap(), before, after)).unwrap();
    assert_eq!(receipt["request"], expected, "the entry receipt binds the exact stored public images after schema stamping");
}

#[test]
fn complete_arrival_atomically_mints_the_first_visit_and_preserves_all_older_receipts() {
    for large in [false, true] {
        let (dir, store, account, before) = fixture();
        let previous = record(&store, account);
        let next = after(&before, large);
        let saved = store.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
        let expected = stored(next, large);
        assert_eq!(saved.commit, SceneCommit::Applied);
        assert_eq!(saved.current, vec![expected.clone()]);
        assert_eq!(store.index().format_version(), if large { 6 } else { 5 });
        let current = record(&store, account);
        assert!(current.checkpoint.is_none() && current.reservation.is_none() && current.scene_roster.is_none());
        check_receipts(&previous, &current, &before, &expected);
        assert!(!dir.path().join(INTENT_FILE).exists());
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), expected);
        assert_eq!(record(&reopened, account), current);
    }
}

#[test]
fn complete_arrival_retry_returns_later_rewards_and_visits_and_does_not_alias_standalone_entry() {
    let (_dir, store, account, before) = fixture();
    let next = after(&before, false);
    let initial = store.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap().current.remove(0);
    // A standalone Enter has a different receipt domain, even when given the same entry id.
    let mut standalone_before = before.clone();
    standalone_before.checkpoint = None;
    let duplicate_entry = FieldVisitOperation { id: ENTRY_ID, kind: FieldVisitOperationKind::Enter };
    assert!(matches!(store.commit_field_visit(&duplicate_entry, &standalone_before, &next), Err(PersistError::TransactionConflict(_))));
    store.update_character_progress(account, GAME, |character| {
        let mut character = character.unwrap();
        character.gil += 17;
        character.exp += 13;
        character.updated_at += 1;
        character
    }).unwrap();
    let rewarded = store.get_scene_state(account, GAME).unwrap();
    let mut advanced = rewarded.clone();
    let visit = advanced.field_visit.as_mut().unwrap();
    visit.revision = 1;
    visit.scratch_values[31] = 3;
    let advance = FieldVisitOperation { id: [64; 16], kind: FieldVisitOperationKind::Advance };
    let later = store.commit_field_visit(&advance, &rewarded, &advanced).unwrap().current;
    assert_ne!(later, initial);
    let previous = record(&store, account);
    let duplicate = store.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, vec![later.clone()]);
    assert!(store.scene_batch_applied(&operation(), RESULT, &[account], GAME).unwrap());
    assert_eq!(record(&store, account), previous, "a later standalone advance keeps its replacement visit receipt");
    assert!(matches!(store.commit_scene_batch(&operation(), b"another selected result", &[before], &[next]), Err(PersistError::TransactionConflict(_))));
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), later);
}

#[test]
fn every_arrival_completion_crash_boundary_recovers_both_records_and_both_receipts_once() {
    for large in [false, true] {
        for crash_at in 0..=5 {
            let (dir, store, account, before) = fixture();
            let previous = record(&store, account);
            let next = after(&before, large);
            let original = files(dir.path());
            let result = store.commit_scene_batch_with_hook(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next),
                |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) });
            assert!(result.is_err());
            if crash_at == 0 {
                assert_eq!(files(dir.path()), original);
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
            } else {
                assert_eq!(store.index().format_version(), if large { 6 } else { 5 });
                assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)),
                    Err(PersistError::TransactionRecoveryRequired)));
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            let expected = stored(next.clone(), large);
            assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), if crash_at == 0 { before.clone() } else { expected.clone() });
            let retry = reopened.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
            assert_eq!(retry.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(retry.current, vec![expected.clone()]);
            check_receipts(&previous, &record(&reopened, account), &before, &expected);
            let durable = files(dir.path());
            assert_eq!(reopened.commit_scene_batch(&operation(), RESULT, &[before], &[next]).unwrap().commit, SceneCommit::AlreadyApplied);
            assert_eq!(files(dir.path()), durable);
        }
    }
}

#[test]
fn a_checksummed_arrival_intent_cannot_forge_its_entry_receipt_or_bound_afterimage() {
    for changed in ["request", "operation", "missing receipt", "entry", "anchor", "source", "scratch", "story", "gil", "schema", "older receipt"] {
        let (dir, store, _account, before) = fixture();
        let next = after(&before, true);
        store.commit_scene_batch_with_hook(&operation(), RESULT, &[before], &[next],
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        let path = dir.path().join(INTENT_FILE);
        drop(store);
        let mut file: BatchIntentFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let row = file.intent.rows.first_mut().unwrap();
        let character = row.after.character.as_mut().unwrap();
        match changed {
            "request" | "operation" => {
                let mut receipt = serde_json::to_value(character.last_visit_commit.as_ref().unwrap()).unwrap();
                if changed == "request" { receipt["request"] = "0".repeat(64).into(); }
                else { receipt["operation"] = serde_json::to_value([65u8; 16]).unwrap(); }
                character.last_visit_commit = Some(serde_json::from_value(receipt).unwrap());
            }
            "missing receipt" => character.last_visit_commit = None,
            "entry" => character.field_visit.as_mut().unwrap().entry.arrival[0] = 1,
            "anchor" => character.field_visit.as_mut().unwrap().anchor[0] = 1,
            "source" => character.field_visit.as_mut().unwrap().source_identity = [66; 32],
            "scratch" => character.field_visit.as_mut().unwrap().scratch_values[31] = 3,
            "story" => row.after.story.as_mut().unwrap().blob.push(199),
            "gil" => character.progress.gil += 1,
            "schema" => character.progress.schema_version = 4,
            "older receipt" => character.scene_receipts.clear(),
            _ => unreachable!(),
        }
        file.checksum = digest(&file.intent).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let evidence = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{changed}");
        assert_eq!(files(dir.path()), evidence, "{changed}: recovery refuses before touching either record");
    }
}

#[test]
fn arrival_completion_refuses_invalid_boundaries_resources_links_and_stale_predecessors_without_writes() {
    for changed in ["no checkpoint", "wrong instance", "zero source", "wrong story", "wrong gil", "battle", "reserved", "existing visit",
        "ordinal", "attempt", "no new visit", "next checkpoint", "next reserved", "revision", "link", "zero entry", "zero visit", "zero visit source",
        "unknown bits", "large entry", "large anchor", "character", "level", "exp", "ap", "limit", "inventory", "different account", "cohort", "stale"] {
        let (dir, store, account, before) = fixture();
        let mut old = before.clone();
        let mut next = after(&old, false);
        let mut operation = operation();
        match changed {
            "no checkpoint" => old.checkpoint = None,
            "wrong instance" => old.checkpoint.as_mut().unwrap().instance = [67; 32],
            "zero source" => old.checkpoint.as_mut().unwrap().source_hash = [0; 32],
            "wrong story" => old.checkpoint.as_mut().unwrap().original_story.push(199),
            "wrong gil" => old.checkpoint.as_mut().unwrap().original_gil += 1,
            "battle" => old.checkpoint.as_mut().unwrap().phase = ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 303, battle_seed: 5 },
            "reserved" => old.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 }),
            "existing visit" => {
                old.field_visit = next.field_visit.clone();
                old.field_visit.as_mut().unwrap().active_continuation = Some(INSTANCE);
            }
            "ordinal" => operation.ordinal = 1,
            "attempt" => operation.attempt = 1,
            "no new visit" => next.field_visit = None,
            "next checkpoint" => next.checkpoint = old.checkpoint.clone(),
            "next reserved" => next.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 }),
            "revision" => next.field_visit.as_mut().unwrap().revision = 1,
            "link" => next.field_visit.as_mut().unwrap().active_continuation = Some(INSTANCE),
            "zero entry" => next.field_visit.as_mut().unwrap().entry.operation = [0; 16],
            "zero visit" => next.field_visit.as_mut().unwrap().visit_id = [0; 16],
            "zero visit source" => next.field_visit.as_mut().unwrap().source_identity = [0; 32],
            "unknown bits" => next.field_visit.as_mut().unwrap().scratch_values[30] = 1,
            "large entry" => next.field_visit.as_mut().unwrap().entry.arrival = vec![255; crate::MAX_FIELD_VISIT_ENTRY_BYTES + 1],
            "large anchor" => next.field_visit.as_mut().unwrap().anchor = vec![255; crate::MAX_FIELD_VISIT_ANCHOR_BYTES + 1],
            "character" => next.pair.character.as_mut().unwrap().character += 1,
            "level" => next.pair.character.as_mut().unwrap().level += 1,
            "exp" => next.pair.character.as_mut().unwrap().exp += 1,
            "ap" => next.pair.character.as_mut().unwrap().ap += 1,
            "limit" => next.pair.character.as_mut().unwrap().limit += 1,
            "inventory" => next.pair.character.as_mut().unwrap().inventory.as_mut().unwrap()[0].count += 1,
            "different account" => next.pair.story.account_id = AccountId(account.0 + 1),
            "cohort" => {},
            "stale" => {
                let path = store.character_path(account.0, GAME).unwrap();
                let mut current = record(&store, account);
                current.progress.ap += 1;
                fsio::write_json(&path, &current, MAX_CHARACTER_RECORD_BYTES).unwrap();
            }
            _ => unreachable!(),
        }
        let guest = if changed == "cohort" {
            Some(store.get_scene_state(AccountId(account.0 + 1), GAME).unwrap())
        } else { None };
        let old = if let Some(guest) = &guest { vec![old, guest.clone()] } else { vec![old] };
        let next = if let Some(guest) = guest { vec![next, guest] } else { vec![next] };
        if changed != "stale" {
            assert!(validate_operation(&operation, RESULT).and_then(|()| validate_transition(&operation, &old, &next)).is_err(),
                "{changed}: the structural rule must refuse even before consulting the stored predecessor");
        }
        let evidence = files(dir.path());
        assert!(store.commit_scene_batch(&operation, RESULT, &old, &next).is_err(), "{changed}");
        assert_eq!(files(dir.path()), evidence, "{changed}: no format selection, intent or record replacement");
        assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

fn replace_header(root: &Path, version: u32) {
    // Deliberately damage only the checked header; supported compaction cannot downgrade.
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
fn arrival_recovery_and_duplicate_acknowledgments_require_the_visits_index_fence() {
    for large in [false, true] {
        let (dir, store, account, before) = fixture();
        let next = after(&before, large);
        store.commit_scene_batch_with_hook(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next),
            |stage| if stage == 2 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        let required = if large { 6 } else { 5 };
        replace_header(dir.path(), required - 1);
        let evidence = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
        assert_eq!(files(dir.path()), evidence);
        replace_header(dir.path(), required);
        let reopened = Store::open(dir.path()).unwrap();
        let expected = stored(next.clone(), large);
        assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), expected);
        drop(reopened);
        for lookup in [false, true] {
            replace_header(dir.path(), required - 1);
            let reopened = Store::open(dir.path()).unwrap();
            let previous = record(&reopened, account);
            if lookup { assert!(reopened.scene_batch_applied(&operation(), RESULT, &[account], GAME).unwrap()); }
            else {
                let retry = reopened.commit_scene_batch(&operation(), RESULT, std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
                assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
                assert_eq!(retry.current, vec![expected.clone()]);
            }
            assert_eq!(reopened.index().format_version(), required);
            assert_eq!(record(&reopened, account), previous);
            drop(reopened);
        }
    }
}
