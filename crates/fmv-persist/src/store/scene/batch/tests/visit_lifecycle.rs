//! Private recovery and metadata checks for a singleton, noncombat scene within one visit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1};

const ATTACH_RESULT: &[u8] = b"checked same-field scene attached";
const ADVANCE_RESULT: &[u8] = b"accepted checked scene window prefix";
const COMPLETE_RESULT: &[u8] = b"checked story, resources and scratch completed";

fn visit_op(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { ordinal: u32::from(kind == SceneOperationKind::AdvanceVisit), ..op(kind) }
}

fn entry_operation() -> FieldVisitOperation {
    FieldVisitOperation { id: [41; 16], kind: FieldVisitOperationKind::Enter }
}

fn entry(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    let mut masks = [0; 256];
    masks[31] = 1;
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [42; 16], revision: 0, field: "fixture/bridge".into(),
        source_identity: [9; 32], entry: FieldVisitEntryV1 { operation: entry_operation().id, arrival: vec![1, 4, 8] },
        anchor: vec![2, 7, 3], scratch_values: [0; 256], scratch_known_masks: masks,
        active_continuation: None,
    });
    next
}

fn idle(store: &Store, account: AccountId) -> Vec<SceneState> {
    let before = store.get_scene_state(account, GAME).unwrap();
    vec![store.commit_field_visit(&entry_operation(), &before, &entry(&before)).unwrap().current]
}

fn attach(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    let state = &mut next[0];
    state.checkpoint = Some(SceneCheckpoint {
        instance: INSTANCE, scene_key: "fixture/bridge/talk".into(), source_hash: [19; 32],
        replay_version: 1, original_story: state.pair.story.blob.clone(),
        original_gil: state.pair.character.as_ref().unwrap().gil, seed: 5,
        replay: vec![1, 3, 8], phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    let visit = state.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.active_continuation = Some(INSTANCE);
    next
}

fn complete(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    let state = &mut next[0];
    state.checkpoint = None;
    state.pair.story.blob.push(47);
    state.pair.story.updated_at = 7;
    let character = state.pair.character.as_mut().unwrap();
    character.gil -= 3;
    character.updated_at = 7;
    let visit = state.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.active_continuation = None;
    visit.scratch_values[31] = 1;
    visit.scratch_values[30] = 2;
    visit.scratch_known_masks[30] = 2;
    next
}

fn advance(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    next[0].checkpoint.as_mut().unwrap().replay.push(2);
    next[0].field_visit.as_mut().unwrap().revision += 1;
    next
}

fn baseline(store: &Store, account: AccountId, kind: SceneOperationKind) -> Vec<SceneState> {
    let before = idle(store, account);
    if kind != SceneOperationKind::AttachVisit {
        store.commit_scene_batch(&op(SceneOperationKind::AttachVisit), ATTACH_RESULT, &before, &attach(&before)).unwrap().current
    } else { before }
}

fn change(before: &[SceneState], kind: SceneOperationKind) -> (Vec<SceneState>, &'static [u8]) {
    match kind {
        SceneOperationKind::AttachVisit => (attach(before), ATTACH_RESULT),
        SceneOperationKind::AdvanceVisit => (advance(before), ADVANCE_RESULT),
        SceneOperationKind::CompleteVisit => (complete(before), COMPLETE_RESULT),
        _ => unreachable!(),
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { read(root, &entry.path(), out); }
            else if entry.file_type().unwrap().is_file() {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    read(root, root, &mut out);
    out
}

#[test]
fn every_attach_advance_and_complete_failure_boundary_recovers_the_entire_visit_once() {
    for kind in [SceneOperationKind::AttachVisit, SceneOperationKind::AdvanceVisit, SceneOperationKind::CompleteVisit] {
        for crash_at in 0..=5 {
            let (dir, store, accounts) = rig();
            let account = accounts[0];
            let before = baseline(&store, account, kind);
            assert_eq!(store.index().format_version(), crate::index::FIELD_VISIT_INDEX_VERSION);
            let (next, selected) = change(&before, kind);
            let character_path = store.character_path(account.0, GAME).unwrap();
            let previous = CharacterRecord::read(&character_path).unwrap().unwrap();
            assert!(previous.last_visit_commit.is_some());
            let guests = store.get_scene_states(&accounts[1..], GAME).unwrap();
            let original = files(dir.path());
            let mut observed = Vec::new();
            let result = store.commit_scene_batch_with_hook(&visit_op(kind), selected, &before, &next, |stage| {
                observed.push(stage);
                if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }
            });
            assert!(matches!(result, Err(PersistError::InvalidField { field: "injected_checkpoint", .. })), "{kind:?} at {crash_at}");
            assert_eq!(observed, (0..=crash_at).collect::<Vec<_>>());
            if crash_at == 0 {
                assert_eq!(files(dir.path()), original, "before intent, no receipt or data changed");
                assert_eq!(store.get_scene_states(&[account], GAME).unwrap(), before);
            } else {
                assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.get_scene_group(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.get_progress_pair(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_progression(&before[0].pair.story), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_character_progress(before[0].pair.character.as_ref().unwrap()), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.commit_scene_batch(&visit_op(kind), selected, &before, &next), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.snapshot(dir.path().join("blocked")), Err(PersistError::TransactionRecoveryRequired)));
                assert_eq!(dir.path().join(INTENT_FILE).exists(), crash_at <= 3);
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            assert_eq!(reopened.get_scene_states(&[account], GAME).unwrap(), if crash_at == 0 { before.clone() } else { next.clone() });
            assert_eq!(reopened.get_scene_states(&accounts[1..], GAME).unwrap(), guests);
            let recovered = CharacterRecord::read(&character_path).unwrap().unwrap();
            assert_eq!(recovered.last_visit_commit, previous.last_visit_commit, "{kind:?} must retain the unrelated entry receipt");
            let saved = reopened.commit_scene_batch(&visit_op(kind), selected, &before, &next).unwrap();
            assert_eq!(saved.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(saved.current, next);
            assert_eq!(reopened.get_scene_group(account, GAME).unwrap(), next);
            let record = CharacterRecord::read(&character_path).unwrap().unwrap();
            assert_eq!(record.last_visit_commit, previous.last_visit_commit);
            assert_eq!(record.scene_receipts, previous.scene_receipts);
            assert_eq!(record.batch_receipts.len(), previous.batch_receipts.len() + 1);
            assert_eq!(record.progress.inventory, previous.progress.inventory);
            assert_eq!(record.progress.limit, previous.progress.limit);
            assert_eq!(record.scene_roster, None, "an unreserved visit scene must not acquire a combat roster");
            let durable = files(dir.path());
            let retry = reopened.commit_scene_batch(&visit_op(kind), selected, &before, &next).unwrap();
            assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
            assert_eq!(retry.current, next);
            assert_eq!(files(dir.path()), durable, "the durable duplicate does not rewrite records");
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }
}

#[test]
fn a_recomputed_checksum_cannot_authorize_other_visit_metadata_or_replace_its_prior_receipt() {
    for kind in [SceneOperationKind::AttachVisit, SceneOperationKind::AdvanceVisit, SceneOperationKind::CompleteVisit] {
        let changes: &[&str] = if kind == SceneOperationKind::AttachVisit {
            &["prior receipt", "visit identity", "source", "entry", "anchor", "revision", "story", "gil", "scratch", "original story", "original gil"]
        } else if kind == SceneOperationKind::AdvanceVisit {
            &["prior receipt", "visit identity", "source", "entry", "anchor", "revision", "story", "gil", "scratch",
                "original story", "original gil", "source hash", "scene key", "seed", "phase", "replay unchanged"]
        } else {
            &["prior receipt", "visit identity", "source", "entry", "anchor", "revision", "knownness", "link"]
        };
        for &changed in changes {
            let (dir, store, accounts) = rig();
            let before = baseline(&store, accounts[0], kind);
            let (next, selected) = change(&before, kind);
            store.commit_scene_batch_with_hook(&visit_op(kind), selected, &before, &next,
                |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
            let path = dir.path().join(INTENT_FILE);
            drop(store);
            let mut file: BatchIntentFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let row = &mut file.intent.rows[0];
            let record = row.after.character.as_mut().unwrap();
            match changed {
                "prior receipt" => {
                    // Keep the receipt structurally valid, and keep the original before-image
                    // intact: only the private preservation check can authorize this change.
                    let mut forged = serde_json::to_value(record.last_visit_commit.as_ref().unwrap()).unwrap();
                    forged["request"] = serde_json::Value::String("0".repeat(64));
                    record.last_visit_commit = Some(serde_json::from_value(forged).unwrap());
                    record.validate().unwrap();
                    assert_ne!(record.last_visit_commit, row.before.character.as_ref().unwrap().last_visit_commit);
                }
                "story" => row.after.story.as_mut().unwrap().blob.push(199),
                "gil" => record.progress.gil += 1,
                "original story" => record.checkpoint.as_mut().unwrap().original_story.push(199),
                "original gil" => record.checkpoint.as_mut().unwrap().original_gil += 1,
                "source hash" => record.checkpoint.as_mut().unwrap().source_hash = [20; 32],
                "scene key" => record.checkpoint.as_mut().unwrap().scene_key.push_str("/other"),
                "seed" => record.checkpoint.as_mut().unwrap().seed += 1,
                "phase" => {
                    let checkpoint = record.checkpoint.as_mut().unwrap();
                    checkpoint.phase = ScenePhase::Resume { segment: 1 };
                    checkpoint.consumed_battles = 1;
                }
                "replay unchanged" => record.checkpoint.as_mut().unwrap().replay = row.before.character.as_ref().unwrap().checkpoint.as_ref().unwrap().replay.clone(),
                _ => {
                    let visit = record.field_visit.as_mut().unwrap();
                    match changed {
                        "visit identity" => visit.visit_id = [43; 16],
                        "source" => visit.source_identity = [10; 32],
                        "entry" => visit.entry.arrival.push(199),
                        "anchor" => visit.anchor.push(199),
                        "revision" => visit.revision += 1,
                        "scratch" => visit.scratch_values[31] = 1,
                        "knownness" => { visit.scratch_values[31] = 0; visit.scratch_known_masks[31] = 0; }
                        "link" => visit.active_continuation = Some(INSTANCE),
                        _ => unreachable!(),
                    }
                }
            }
            file.checksum = digest(&file.intent).unwrap();
            std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
            let corrupt = files(dir.path());
            assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{kind:?}: {changed}");
            assert_eq!(files(dir.path()), corrupt, "{kind:?}: {changed} must refuse before repairing any record");
            assert!(path.exists());
        }
    }
}

/// Model a damaged or older global format fence while retaining a correctly checksummed
/// index header. Production never downgrades an index containing field visits.
fn set_header_version(root: &Path, version: u32) {
    let path = root.join(crate::index::INDEX_LOG_FILE);
    let bytes = std::fs::read(&path).unwrap();
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let space = bytes[..end].iter().position(|byte| *byte == b' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(&bytes[space + 1..end]).unwrap();
    assert_eq!(header["kind"], "fmv-index-log");
    header["schema_version"] = version.into();
    let payload = serde_json::to_vec(&header).unwrap();
    let hash = blake3::hash(&payload).to_hex().to_string();
    let mut replaced = format!("{} ", &hash[..space]).into_bytes();
    replaced.extend_from_slice(&payload);
    replaced.extend_from_slice(&bytes[end..]);
    std::fs::write(path, replaced).unwrap();
}

#[test]
fn every_visit_lifecycle_intent_requires_index_five_before_recovery_installs_either_image() {
    for kind in [SceneOperationKind::AttachVisit, SceneOperationKind::AdvanceVisit, SceneOperationKind::CompleteVisit] {
        let (dir, store, accounts) = rig();
        let before = baseline(&store, accounts[0], kind);
        let (next, selected) = change(&before, kind);
        store.commit_scene_batch_with_hook(&visit_op(kind), selected, &before, &next,
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        set_header_version(dir.path(), crate::index::DURABLE_STORY_INDEX_VERSION);
        let damaged = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{kind:?}");
        assert_eq!(files(dir.path()), damaged, "{kind:?} cannot repair records before checking the fence");
        assert!(dir.path().join(INTENT_FILE).exists());
        set_header_version(dir.path(), crate::index::FIELD_VISIT_INDEX_VERSION);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), crate::index::FIELD_VISIT_INDEX_VERSION);
        assert_eq!(reopened.get_scene_states(&accounts[..1], GAME).unwrap(), next);
        assert_eq!(reopened.commit_scene_batch(&visit_op(kind), selected, &before, &next).unwrap().commit, SceneCommit::AlreadyApplied);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

#[test]
fn the_entry_receipt_survives_the_scene_and_cannot_reset_its_completed_latch() {
    let (dir, store, accounts) = rig();
    let account = accounts[0];
    let initial = store.get_scene_state(account, GAME).unwrap();
    let entry_image = entry(&initial);
    let entered = store.commit_field_visit(&entry_operation(), &initial, &entry_image).unwrap().current;
    let path = store.character_path(account.0, GAME).unwrap();
    let receipt = CharacterRecord::read(&path).unwrap().unwrap().last_visit_commit;
    let before = vec![entered];
    let attached = store.commit_scene_batch(&op(SceneOperationKind::AttachVisit), ATTACH_RESULT, &before, &attach(&before)).unwrap().current;
    assert_ne!(attached[0].checkpoint.as_ref().unwrap().source_hash, attached[0].field_visit.as_ref().unwrap().source_identity,
        "the replay fingerprint and stable field profile are separate identities");
    assert_eq!(CharacterRecord::read(&path).unwrap().unwrap().last_visit_commit, receipt);
    let advanced = store.commit_scene_batch(&visit_op(SceneOperationKind::AdvanceVisit), ADVANCE_RESULT, &attached, &advance(&attached)).unwrap().current;
    assert_eq!(CharacterRecord::read(&path).unwrap().unwrap().last_visit_commit, receipt);
    let completed = store.commit_scene_batch(&op(SceneOperationKind::CompleteVisit), COMPLETE_RESULT, &advanced, &complete(&advanced)).unwrap().current;
    assert_eq!(CharacterRecord::read(&path).unwrap().unwrap().last_visit_commit, receipt);
    assert_eq!(completed[0].pair.character.as_ref().unwrap().gil, 237);
    assert_eq!(completed[0].field_visit.as_ref().unwrap().scratch_values[31], 1);
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    let durable = files(dir.path());
    let duplicate = reopened.commit_field_visit(&entry_operation(), &initial, &entry_image).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, completed[0], "late entry acknowledgment returns the current completed visit");
    assert_eq!(files(dir.path()), durable, "the old entry receipt cannot rerun initialization or economic effects");
    assert_eq!(CharacterRecord::read(&path).unwrap().unwrap().last_visit_commit, receipt);
}
