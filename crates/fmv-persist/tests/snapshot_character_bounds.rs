//! Snapshots preserve large validated character envelopes without raising unrelated file limits.
//! All stores and deliberately malformed copy inputs are temporary and synthetic.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_persist::{
    restore_snapshot, CharacterProgress, PersistError, Progression, SceneCheckpoint,
    SceneCommit, SceneOperation, SceneOperationKind, ScenePhase, SceneState, SnapshotInfo,
    Store, MAX_CHARACTER_RECORD_BYTES, MAX_RECORD_BYTES, MAX_SCENE_RECEIPTS,
    MAX_SCENE_REPLAY_BYTES,
};
use serde_json::json;

const GAME: GameId = GameId::Fixture;

fn large_valid_character(store: &Store, root: &Path) -> (AccountId, SceneState) {
    let (account, _) = store.create_account("Large checkpoint", NOW).unwrap();
    let story = vec![u8::MAX; 8 * 1024];
    store.put_progression(&Progression::new(account, GAME, story.clone(), u64::MAX)).unwrap();
    store.put_character_progress(&CharacterProgress {
        character: u8::MAX,
        level: u8::MAX,
        exp: u32::MAX,
        ap: u32::MAX,
        gil: u32::MAX,
        limit: u16::MAX,
        inventory: Some((0..MAX_INVENTORY_STACKS).map(|i| {
            InventoryEntry::new(u16::MAX - u16::try_from(i).unwrap(), u16::MAX)
        }).collect()),
        ..CharacterProgress::new(account, GAME, 0, 1, u64::MAX)
    }).unwrap();
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut after = before.clone();
    let operation = SceneOperation {
        instance: [u8::MAX; 32], ordinal: 0, attempt: 0, kind: SceneOperationKind::Finish,
    };
    after.checkpoint = Some(SceneCheckpoint {
        instance: operation.instance,
        scene_key: "\\".repeat(256),
        source_hash: [u8::MAX; 32],
        replay_version: u32::MAX,
        original_story: story,
        original_gil: u32::MAX,
        seed: u32::MAX,
        replay: vec![u8::MAX; MAX_SCENE_REPLAY_BYTES],
        phase: ScenePhase::Landing { destination: vec![u8::MAX; 8 * 1024] },
        consumed_battles: 0,
    });
    let committed = store.commit_scene_batch(&operation, b"large synthetic landing", &[before], &[after]).unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    let expected = committed.current.into_iter().next().unwrap();

    // Retained receipt histories need not be built through thousands of disk commits.
    // Keep the real batch receipt and add structurally valid historical cohorts, then
    // validate the entire private envelope through the public reader before copying it.
    let path = progression_file(root, account, "Fixture.character");
    let mut record = read_json(&path);
    let receipts = record["batch_receipts"].as_object_mut().unwrap();
    for n in 1..MAX_SCENE_RECEIPTS {
        let key = blake3::hash(format!("historical snapshot receipt {n}").as_bytes()).to_hex().to_string();
        assert!(receipts.insert(key, json!({
            "result": "f".repeat(64), "members": 16, "slot": 15,
        })).is_none());
    }
    assert_eq!(receipts.len(), MAX_SCENE_RECEIPTS);
    write_json(&path, &record);
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), expected);
    assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![expected.clone()]);
    let len = fs::metadata(&path).unwrap().len();
    assert!(len > 3 * MAX_RECORD_BYTES, "the valid envelope must exercise the large-file path: {len}");
    assert!(len <= MAX_CHARACTER_RECORD_BYTES, "the public reader accepted {len} bytes");
    (account, expected)
}

#[test]
fn maximum_replay_and_receipt_history_survive_snapshot_and_restore_byte_for_byte() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let (account, expected) = large_valid_character(&store, &root);
    let original = file_map(&root);
    let snapshot = dir.path().join("snapshot");
    let taken = store.snapshot(&snapshot).unwrap();
    assert_eq!(taken.files, original.len());
    assert_eq!(taken.bytes, original.values().map(|bytes| bytes.len() as u64).sum::<u64>());
    assert_eq!(file_map(&snapshot), original);

    let restored = dir.path().join("restored");
    assert_eq!(restore_snapshot(&snapshot, &restored).unwrap(), taken);
    assert_eq!(file_map(&restored), original);
    assert_eq!(file_map(&snapshot), original, "restoring must not rewrite the source snapshot");
    let copy = open(&restored);
    assert_eq!(copy.get_scene_state(account, GAME).unwrap(), expected);
    assert_eq!(copy.get_scene_group(account, GAME).unwrap(), vec![expected]);
    assert_eq!(file_map(&restored), original, "opening preserves replay, receipts and all source bytes");
}

fn oversized_json(size: u64) -> Vec<u8> {
    let mut bytes = b"{\"size_probe\":\"".to_vec();
    let suffix = b"\"}\n";
    bytes.resize(usize::try_from(size).unwrap() - suffix.len(), b'x');
    bytes.extend_from_slice(suffix);
    assert_eq!(bytes.len() as u64, size);
    bytes
}

fn write_probe(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn assert_size_refusal(result: fmv_persist::Result<SnapshotInfo>, source: &Path, limit: u64) {
    match result {
        Err(PersistError::TooLarge { path, size, max }) => {
            assert_eq!(path, source);
            assert_eq!(size, limit + 1);
            assert_eq!(max, limit);
        }
        other => panic!("expected a {limit}-byte copy bound for {}, got {other:?}", source.display()),
    }
}

fn both_copy_directions_enforce_bound(relative: &str, limit: u64) {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let snapshot = dir.path().join("source-snapshot");
    store.snapshot(&snapshot).unwrap();
    // Snapshot copying is intentionally raw and bounded: this probes path recognition,
    // not schema validation or whether an account is present in the index.
    let bytes = oversized_json(limit + 1);
    write_probe(&root, relative, &bytes);
    write_probe(&snapshot, relative, &bytes);

    let taken = dir.path().join("failed-snapshot");
    assert_size_refusal(store.snapshot(&taken), &root.join(relative), limit);
    assert!(!taken.join("index.log").exists(), "a failed copy must not claim completeness");
    let restored = dir.path().join("failed-restore");
    assert_size_refusal(restore_snapshot(&snapshot, &restored), &snapshot.join(relative), limit);
    assert!(!restored.join("index.log").exists(), "a failed restore must not claim completeness");
    assert_eq!(fs::read(root.join(relative)).unwrap(), bytes);
    assert_eq!(fs::read(snapshot.join(relative)).unwrap(), bytes);
}

#[test]
fn recognized_character_files_still_refuse_more_than_four_mib_in_both_directions() {
    both_copy_directions_enforce_bound("progression/1/Ff7.character.json", MAX_CHARACTER_RECORD_BYTES);
}

#[test]
fn only_canonical_progression_character_paths_receive_the_larger_copy_bound() {
    for relative in [
        "accounts/Ff7.character.json",
        "avatars/Ff7.character.json",
        "characters/Ff7.character.json",
        "progression/1/Ff7.json",
        "progression/1/Unknown.character.json",
        "progression/1/ff7.character.json",
        "progression/not-account/Ff7.character.json",
        "progression/01/Ff7.character.json",
        "progression/+1/Ff7.character.json",
        "progression/18446744073709551616/Ff7.character.json",
    ] {
        both_copy_directions_enforce_bound(relative, MAX_RECORD_BYTES);
    }
}
