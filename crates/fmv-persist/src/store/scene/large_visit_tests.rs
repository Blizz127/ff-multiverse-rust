//! Every transaction family that preserves enlarged visits must retain its reader fence,
//! including receipt-only acknowledgments and interrupted before/after replacement.

use super::*;
use crate::{CharacterProgress, FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind,
    FieldVisitV1, Progression};
use crate::index::INDEX_LOG_FILE;
use std::fs;
use std::path::PathBuf;

const GAME: GameId = GameId::Fixture;
const INSTANCE: [u8; 32] = [23; 32];
const LARGE_SCHEMA: u32 = 5;
const LARGE_INDEX: u32 = 6;
const OLD_INDEX: u32 = 5;

fn fixture() -> (tempfile::TempDir, Store, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Large scene owner", 1).unwrap();
    let mut character = CharacterProgress::new(account, GAME, 0, 6, 2);
    character.gil = 240;
    store.put_character_progress(&character).unwrap();
    store.put_progression(&Progression::new(account, GAME, vec![1, 39, 0], 2)).unwrap();
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut next = before.clone();
    let operation = FieldVisitOperation { id: [19; 16], kind: FieldVisitOperationKind::Enter };
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [20; 16], revision: 0, field: "fixture/large".into(),
        source_identity: [21; 32], entry: FieldVisitEntryV1 { operation: operation.id, arrival: vec![255; 1025] },
        anchor: vec![254; 1025], scratch_values: [0; 256], scratch_known_masks: [255; 256],
        active_continuation: None,
    });
    let saved = store.commit_field_visit(&operation, &before, &next).unwrap();
    assert_eq!(saved.current.pair.character.as_ref().unwrap().schema_version, LARGE_SCHEMA);
    assert_eq!(store.index().format_version(), LARGE_INDEX);
    (dir, store, account)
}

// Deliberately forge only the checked header for recovery/acknowledgment tests.
// Production compaction correctly refuses to lower a loaded index's format.
fn set_header(root: &Path, version: u32) {
    let path = root.join(INDEX_LOG_FILE);
    let bytes = fs::read(&path).unwrap();
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let line = bytes.get(..end).unwrap();
    let space = line.iter().position(|byte| *byte == b' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(line.get(space + 1..).unwrap()).unwrap();
    assert_eq!(header["kind"], "fmv-index-log");
    header["schema_version"] = version.into();
    let payload = serde_json::to_vec(&header).unwrap();
    let hash = blake3::hash(&payload).to_hex().to_string();
    let mut replaced = format!("{} ", hash.get(..space).unwrap()).into_bytes();
    replaced.extend_from_slice(&payload);
    replaced.extend_from_slice(bytes.get(end..).unwrap());
    fs::write(path, replaced).unwrap();
}

fn records(store: &Store, account: AccountId) -> [PathBuf; 2] {
    [store.progression_path(account.0, GAME).unwrap(), store.character_path(account.0, GAME).unwrap()]
}

fn bytes(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().map(|path| fs::read(path).unwrap()).collect()
}

fn interrupted(stage: u32) -> PersistError {
    PersistError::InvalidField { field: "injected_large_scene", reason: stage.to_string() }
}

fn reward(store: &Store, account: AccountId) -> SceneState {
    store.update_character_progress(account, GAME, |character| {
        let mut character = character.unwrap();
        character.gil += 17;
        character.exp += 13;
        character.updated_at += 1;
        character
    }).unwrap();
    store.get_scene_state(account, GAME).unwrap()
}

fn ordinary_next(before: &ProgressPair) -> ProgressPair {
    let mut next = before.clone();
    next.story.blob.push(47);
    next.story.updated_at += 1;
    let character = next.character.as_mut().unwrap();
    character.gil -= 3;
    character.updated_at += 1;
    next
}

#[test]
fn ordinary_scene_new_writes_and_both_receipt_paths_fence_large_visits_before_success() {
    for acknowledgement in ["new", "duplicate", "lookup"] {
        let (dir, store, account) = fixture();
        let before = store.get_progress_pair(account, GAME).unwrap();
        let next = ordinary_next(&before);
        let selected = b"large visit ordinary choice";
        let expected = if acknowledgement == "new" { None } else {
            assert_eq!(store.commit_scene_progress_with_result("fixture/choice", selected, &before, &next).unwrap(), SceneCommit::Applied);
            Some(reward(&store, account))
        };
        let paths = records(&store, account);
        let original = bytes(&paths);
        drop(store);
        // Model a copied record under an older recognized header. Opening alone stays
        // read-only; a new write or successful receipt acknowledgment must repair the fence.
        set_header(dir.path(), OLD_INDEX);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), OLD_INDEX);
        match acknowledgement {
            "new" => assert_eq!(reopened.commit_scene_progress_with_result("fixture/choice", selected, &before, &next).unwrap(), SceneCommit::Applied),
            "duplicate" => assert_eq!(reopened.commit_scene_progress_with_result("fixture/choice", selected, &before, &next).unwrap(), SceneCommit::AlreadyApplied),
            "lookup" => assert!(reopened.scene_progress_applied("fixture/choice", &before.story, selected).unwrap()),
            _ => unreachable!(),
        }
        assert_eq!(reopened.index().format_version(), LARGE_INDEX);
        let current = reopened.get_scene_state(account, GAME).unwrap();
        assert_eq!(current.pair.character.as_ref().unwrap().schema_version, LARGE_SCHEMA);
        assert!(current.field_visit.as_ref().unwrap().entry.arrival.len() > 1024);
        if let Some(expected) = expected {
            assert_eq!(current, expected, "the acknowledgment keeps later rewards");
            assert_eq!(bytes(&paths), original, "a duplicate only upgrades the index fence");
        } else {
            assert_eq!(current.pair, next);
        }
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

fn operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: u32::from(kind == SceneOperationKind::AdvanceVisit), attempt: 0, kind }
}

fn selected(kind: SceneOperationKind) -> &'static [u8] {
    match kind {
        SceneOperationKind::AttachVisit => b"large checked visit attached",
        SceneOperationKind::AdvanceVisit => b"large checked window accepted",
        SceneOperationKind::CompleteVisit => b"large checked visit completed",
        _ => unreachable!(),
    }
}

fn next_visit(before: &SceneState, kind: SceneOperationKind) -> SceneState {
    let mut next = before.clone();
    next.field_visit.as_mut().unwrap().revision += 1;
    match kind {
        SceneOperationKind::AttachVisit => {
            next.field_visit.as_mut().unwrap().active_continuation = Some(INSTANCE);
            next.checkpoint = Some(SceneCheckpoint {
                instance: INSTANCE, scene_key: "fixture/large/talk".into(), source_hash: [22; 32],
                replay_version: 1, original_story: before.pair.story.blob.clone(),
                original_gil: before.pair.character.as_ref().unwrap().gil, seed: 4, replay: vec![1, 8],
                phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
            });
        }
        SceneOperationKind::AdvanceVisit => next.checkpoint.as_mut().unwrap().replay.push(9),
        SceneOperationKind::CompleteVisit => {
            next.checkpoint = None;
            next.field_visit.as_mut().unwrap().active_continuation = None;
            next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
            next.pair = ordinary_next(&before.pair);
        }
        _ => unreachable!(),
    }
    next
}

fn commit_visit(store: &Store, before: &SceneState, kind: SceneOperationKind) -> SceneState {
    let result = store.commit_scene_batch(&operation(kind), selected(kind), std::slice::from_ref(before), &[next_visit(before, kind)]).unwrap();
    assert_eq!(result.commit, SceneCommit::Applied);
    result.current.into_iter().next().unwrap()
}

#[test]
fn all_large_visit_batch_receipt_paths_return_current_rewards_and_restore_index_six() {
    let (dir, mut store, account) = fixture();
    let idle = store.get_scene_state(account, GAME).unwrap();
    let attached = commit_visit(&store, &idle, SceneOperationKind::AttachVisit);
    let advanced = commit_visit(&store, &attached, SceneOperationKind::AdvanceVisit);
    let completed = commit_visit(&store, &advanced, SceneOperationKind::CompleteVisit);
    let later = reward(&store, account);
    assert_eq!(later.field_visit, completed.field_visit);
    let paths = records(&store, account);
    let original = bytes(&paths);
    for (kind, before, next) in [
        (SceneOperationKind::AttachVisit, idle, attached.clone()),
        (SceneOperationKind::AdvanceVisit, attached, advanced.clone()),
        (SceneOperationKind::CompleteVisit, advanced, completed),
    ] {
        for lookup in [false, true] {
            drop(store);
            set_header(dir.path(), OLD_INDEX);
            store = Store::open(dir.path()).unwrap();
            assert_eq!(store.index().format_version(), OLD_INDEX);
            if lookup {
                assert!(store.scene_batch_applied(&operation(kind), selected(kind), &[account], GAME).unwrap());
            } else {
                let retry = store.commit_scene_batch(&operation(kind), selected(kind), std::slice::from_ref(&before), std::slice::from_ref(&next)).unwrap();
                assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
                assert_eq!(retry.current, vec![later.clone()]);
            }
            assert_eq!(store.index().format_version(), LARGE_INDEX);
            assert_eq!(store.get_scene_state(account, GAME).unwrap(), later);
            assert_eq!(bytes(&paths), original);
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }
}

#[test]
fn large_visit_batch_recovery_refuses_an_old_header_before_any_partial_install() {
    for kind in [SceneOperationKind::AttachVisit, SceneOperationKind::AdvanceVisit, SceneOperationKind::CompleteVisit] {
        for crash_at in 1..=3 {
            let (dir, store, account) = fixture();
            let idle = store.get_scene_state(account, GAME).unwrap();
            let before = if kind == SceneOperationKind::AttachVisit { idle }
                else { commit_visit(&store, &idle, SceneOperationKind::AttachVisit) };
            let next = next_visit(&before, kind);
            let paths = records(&store, account);
            store.commit_scene_batch_with_hook(&operation(kind), selected(kind), std::slice::from_ref(&before), std::slice::from_ref(&next),
                |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
            assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
            let intent_path = dir.path().join(INTENT_FILE);
            let intent = fs::read(&intent_path).unwrap();
            let original = bytes(&paths);
            drop(store);
            set_header(dir.path(), OLD_INDEX);
            assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "{kind:?} stage {crash_at}");
            assert_eq!(bytes(&paths), original, "no record is installed beneath an old fence");
            assert_eq!(fs::read(&intent_path).unwrap(), intent, "the recoverable intent remains intact");
            set_header(dir.path(), LARGE_INDEX);
            let reopened = Store::open(dir.path()).unwrap();
            assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), next);
            assert!(!intent_path.exists());
            let retry = reopened.commit_scene_batch(&operation(kind), selected(kind), &[before], std::slice::from_ref(&next)).unwrap();
            assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
            assert_eq!(retry.current, vec![next]);
        }
    }
}

#[test]
fn legacy_paired_scene_intent_with_large_visit_requires_index_six_to_recover() {
    for crash_at in 1..=3 {
        let (dir, store, account) = fixture();
        let before_state = store.get_scene_state(account, GAME).unwrap();
        let before = before_state.pair.clone();
        let next = ordinary_next(&before);
        let paths = records(&store, account);
        store.commit_scene_with_hook("fixture/large/legacy", &before, &next,
            |stage| if stage == crash_at { Err(interrupted(u32::from(stage))) } else { Ok(()) }).unwrap_err();
        let intent_path = dir.path().join(INTENT_FILE);
        let intent = fs::read(&intent_path).unwrap();
        let original = bytes(&paths);
        drop(store);
        set_header(dir.path(), OLD_INDEX);
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
        assert_eq!(bytes(&paths), original);
        assert_eq!(fs::read(&intent_path).unwrap(), intent);
        set_header(dir.path(), LARGE_INDEX);
        let reopened = Store::open(dir.path()).unwrap();
        let actual = reopened.get_scene_state(account, GAME).unwrap();
        assert_eq!(actual.pair, next);
        assert_eq!(actual.field_visit, before_state.field_visit);
        assert_eq!(reopened.commit_scene_progress("fixture/large/legacy", &before, &next).unwrap(), SceneCommit::AlreadyApplied);
        assert!(!intent_path.exists());
    }
}
