//! The new writer fence must not make already durable legacy scene intents unreadable.
//! Synthetic header rewrites below model older stores; production never downgrades one.

use super::*;

const ORDINARY_INSTANCE: [u8; 32] = [99; 32];
const RESULT: &[u8] = b"ordinary terminal result";

fn pairs(states: &[SceneState]) -> Vec<ProgressPair> { states.iter().map(|state| state.pair.clone()).collect() }

fn outcome(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    next[0].pair.story.blob.extend_from_slice(&[0, 255, 0, 42]);
    for state in &mut next {
        state.pair.character.as_mut().unwrap().gil += 10;
    }
    next
}

fn ordinary_operation() -> SceneOperation {
    SceneOperation { instance: ORDINARY_INSTANCE, ordinal: 0, attempt: 0, kind: SceneOperationKind::BattleOutcome }
}

/// `index::encode_line` is private to that module. Preserve the actual header kind and
/// checksum width, applying its same blake3(JSON payload) prefix rule to the new version.
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

fn record_paths(store: &Store, accounts: &[AccountId]) -> Vec<std::path::PathBuf> {
    accounts.iter().flat_map(|account| [store.progression_path(account.0, GAME).unwrap(),
        store.character_path(account.0, GAME).unwrap()]).collect()
}

fn record_bytes(paths: &[std::path::PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().map(|path| std::fs::read(path).unwrap()).collect()
}

#[test]
fn legacy_scene_intent_recovers_under_header_three_without_silently_upgrading_it() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let after = prepare(&before);
    store.commit_scene_batch_with_hook(&op(SceneOperationKind::PrepareBattle), b"legacy prepare", &before, &after,
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    drop(store);
    set_header_version(dir.path(), crate::index::SCENE_BATCH_INDEX_VERSION);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.index().format_version(), crate::index::SCENE_BATCH_INDEX_VERSION,
        "reading/recovering a supported old intent alone does not opt into new writes");
    assert_eq!(comparable(reopened.get_scene_states(&accounts, GAME).unwrap()), comparable(after));
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn ordinary_battle_intent_requires_header_four_before_installing_any_member() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let after = outcome(&before);
    let paths = record_paths(&store, &accounts);
    store.commit_scene_batch_with_hook(&ordinary_operation(), RESULT, &before, &after,
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    let original = record_bytes(&paths);
    drop(store);
    set_header_version(dir.path(), crate::index::SCENE_BATCH_INDEX_VERSION);
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })),
        "an ordinary outcome cannot be recovered without its stronger writer fence");
    assert_eq!(record_bytes(&paths), original, "all members remain untouched on refusal");
    assert!(dir.path().join(INTENT_FILE).exists());

    set_header_version(dir.path(), crate::index::DURABLE_STORY_INDEX_VERSION);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    assert_eq!(comparable(reopened.get_scene_states(&accounts, GAME).unwrap()), comparable(after.clone()));
    let duplicate = reopened.commit_battle_outcome(ORDINARY_INSTANCE, RESULT, &pairs(&before), &pairs(&after)).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(comparable(duplicate.current), comparable(after));
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn acknowledging_a_committed_ordinary_outcome_raises_a_legacy_header_but_other_lookups_do_not() {
    for acknowledge_with_commit in [false, true] {
        let (dir, store, accounts) = rig();
        let before = store.get_scene_states(&accounts, GAME).unwrap();
        let after = outcome(&before);
        let saved = store.commit_battle_outcome(ORDINARY_INSTANCE, RESULT, &pairs(&before), &pairs(&after)).unwrap();
        let paths = record_paths(&store, &accounts);
        let original = record_bytes(&paths);
        drop(store);
        set_header_version(dir.path(), crate::index::SCENE_BATCH_INDEX_VERSION);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), crate::index::SCENE_BATCH_INDEX_VERSION);
        let index_path = dir.path().join(crate::index::INDEX_LOG_FILE);
        let index_bytes = std::fs::read(&index_path).unwrap();

        assert!(!reopened.battle_outcome_applied([100; 32], RESULT, &accounts, GAME).unwrap());
        assert!(matches!(reopened.battle_outcome_applied(ORDINARY_INSTANCE, b"conflicting result", &accounts, GAME),
            Err(PersistError::TransactionConflict(_))));
        assert_eq!(reopened.index().format_version(), crate::index::SCENE_BATCH_INDEX_VERSION,
            "absence and conflict do not acknowledge a write or raise the format");
        assert_eq!(std::fs::read(&index_path).unwrap(), index_bytes);
        assert_eq!(record_bytes(&paths), original);

        if acknowledge_with_commit {
            let duplicate = reopened.commit_battle_outcome(ORDINARY_INSTANCE, RESULT, &pairs(&before), &pairs(&after)).unwrap();
            assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
            assert_eq!(duplicate.current, saved.current);
        } else {
            assert!(reopened.battle_outcome_applied(ORDINARY_INSTANCE, RESULT, &accounts, GAME).unwrap());
        }
        assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION,
            "a successful ordinary acknowledgement requires the current writer fence");
        assert_eq!(record_bytes(&paths), original, "format fencing does not rewrite story or resources");
        drop(reopened);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(reopened.get_scene_states(&accounts, GAME).unwrap(), saved.current);
    }
}
