//! Explicit large-visit compatibility changes the index fence without rewriting accounts.

use super::*;
use fmv_core::InventoryEntry;

fn fixture(version: u32) -> (tempfile::TempDir, Store, Registered) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let registered = store.register_with_character("Large Visit Fence", [1, 2, 3], "original", None,
        Some(NewPlayerCharacter { body: "original".into(), ..NewPlayerCharacter::default() }), 1).unwrap();
    let mut character = CharacterProgress::new(registered.account_id, GameId::Ff7, 0, 6, 2);
    character.gil = 240;
    character.inventory = Some(vec![InventoryEntry::new(1, 3)]);
    store.put_character_progress(&character).unwrap();
    store.put_progression(&Progression::new(registered.account_id, GameId::Ff7, vec![1, 39, 0], 3)).unwrap();
    drop(store);
    let path = dir.path().join(INDEX_LOG_FILE);
    let previous = index::load_log(&path, false).unwrap().unwrap();
    index::write_compacted_version(&path, &previous.index, previous.next_account_id,
        previous.next_avatar_id, version).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.index().format_version(), version);
    (dir, store, registered)
}

fn records(store: &Store, registered: &Registered) -> Vec<(PathBuf, Vec<u8>)> {
    let account = registered.account_id.0;
    [store.account_path(account), store.avatar_path(registered.avatar.avatar_id.0),
        store.character_record_path(account), store.progression_path(account, GameId::Ff7).unwrap(),
        store.character_path(account, GameId::Ff7).unwrap()]
        .into_iter().map(|path| { let bytes = std::fs::read(&path).unwrap(); (path, bytes) }).collect()
}

#[test]
fn opting_into_large_visits_from_every_prior_header_preserves_records_and_the_new_log_handle() {
    for prior in [crate::model::INDEX_LOG_VERSION, index::SCENE_BATCH_INDEX_VERSION,
        index::DURABLE_STORY_INDEX_VERSION, index::FIELD_VISIT_INDEX_VERSION] {
        let (dir, store, registered) = fixture(prior);
        let before = records(&store, &registered);
        store.require_compatibility(index::StoreCompatibility::LargeFieldVisit).unwrap();
        assert_eq!(store.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
        assert_eq!(records(&store, &registered), before);
        assert!(store.get_scene_state(registered.account_id, GameId::Ff7).unwrap().field_visit.is_none(),
            "selecting a reader/writer fence does not create a visit");
        let upgraded_log = std::fs::read(store.log_path()).unwrap();
        for minimum in [index::StoreCompatibility::DurableStory, index::StoreCompatibility::FieldVisit,
            index::StoreCompatibility::LargeFieldVisit] {
            store.require_compatibility(minimum).unwrap();
            assert_eq!(store.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
            assert_eq!(std::fs::read(store.log_path()).unwrap(), upgraded_log,
                "lower and repeated compatibility requests must not rewrite the index");
            assert_eq!(records(&store, &registered), before);
        }
        assert!(store.unclaimed_accounts().iter().any(|(id, _)| *id == registered.account_id));
        assert_eq!(store.authenticate(&registered.token).unwrap(), registered.account_id);
        let (added, added_token) = store.create_account("After Large Fence", 4).unwrap();
        drop(store);

        // Reopen before compaction: compaction could otherwise hide a claim appended to the
        // old inode by rewriting the in-memory claimed flag into the replacement log.
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
        assert!(!reopened.unclaimed_accounts().iter().any(|(id, _)| *id == registered.account_id),
            "the authentication claim reached the replacement inode");
        assert_eq!(reopened.account_count(), 2);
        assert_eq!(reopened.authenticate(&added_token).unwrap(), added);
        assert_eq!(records(&reopened, &registered), before);
        reopened.compact_index().unwrap();
        drop(reopened);
        let compacted = Store::open(dir.path()).unwrap();
        assert_eq!(compacted.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
        assert_eq!(compacted.account_count(), 2);
        assert!(compacted.unclaimed_accounts().is_empty());
        assert_eq!(records(&compacted, &registered), before);
    }
}

#[test]
fn an_uncertain_large_visit_upgrade_fences_reads_and_writes_until_reopen() {
    for prior in [crate::model::INDEX_LOG_VERSION, index::SCENE_BATCH_INDEX_VERSION,
        index::DURABLE_STORY_INDEX_VERSION, index::FIELD_VISIT_INDEX_VERSION] {
        let (dir, store, registered) = fixture(prior);
        let account = registered.account_id;
        let before = records(&store, &registered);
        FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
        assert!(matches!(store.require_compatibility(index::StoreCompatibility::LargeFieldVisit),
            Err(PersistError::Io { .. })));
        assert!(matches!(store.get_progression(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_character_progress(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_scene_state(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.authenticate(&registered.token), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.put_progression(&Progression::empty(account, GameId::Ff7)),
            Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.update_character_progress(account, GameId::Ff7, |_| panic!("fenced delta must not run")),
            Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.create_account("Blocked", 4), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.require_compatibility(index::StoreCompatibility::FieldVisit),
            Err(PersistError::TransactionRecoveryRequired)));
        assert_eq!(records(&store, &registered), before);
        drop(store);

        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
        assert_eq!(reopened.account_count(), 1);
        assert_eq!(records(&reopened, &registered), before);
        assert_eq!(reopened.get_progression(account, GameId::Ff7).unwrap().blob, [1, 39, 0]);
        assert_eq!(reopened.get_character_progress(account, GameId::Ff7).unwrap().unwrap().gil, 240);
        assert_eq!(reopened.authenticate(&registered.token).unwrap(), account);
        let (added, _) = reopened.create_account("Recovered", 5).unwrap();
        assert_eq!(added.0, account.0 + 1, "the fenced registration consumed no account id");
    }
}

#[test]
fn an_unknown_minimum_above_large_visits_refuses_without_poisoning_or_writing() {
    for prior in [crate::model::INDEX_LOG_VERSION, index::FIELD_VISIT_INDEX_VERSION,
        index::LARGE_FIELD_VISIT_INDEX_VERSION] {
        let (_dir, store, registered) = fixture(prior);
        let before = records(&store, &registered);
        let log = std::fs::read(store.log_path()).unwrap();
        {
            let counters = store.writer().unwrap();
            let _progress = store.progress_write();
            assert!(matches!(store.ensure_index_format(&counters, 7),
                Err(PersistError::InvalidField { field: "store_compatibility", .. })));
        }
        assert!(!store.transaction_poisoned.load(Ordering::Acquire));
        assert_eq!(store.index().format_version(), prior);
        assert_eq!(std::fs::read(store.log_path()).unwrap(), log);
        assert_eq!(records(&store, &registered), before);
        let story = store.get_progression(registered.account_id, GameId::Ff7).unwrap();
        store.put_progression(&story).unwrap();
        assert_eq!(records(&store, &registered), before);
        assert_eq!(store.authenticate(&registered.token).unwrap(), registered.account_id);
        assert!(store.create_account("Still Writable", 4).is_ok());
    }
}

/// Model an imported lower header without letting compaction inspect or change the
/// character's larger visit. Production only raises compatibility versions.
fn replace_header_version(root: &Path, version: u32) {
    let path = root.join(INDEX_LOG_FILE);
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

fn large_visit_under_old_header() -> (tempfile::TempDir, Store, Registered, scene::CharacterRecord) {
    use field_visit::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1};
    let (dir, store, registered) = fixture(crate::model::INDEX_LOG_VERSION);
    let account = registered.account_id;
    let before = store.get_scene_state(account, GameId::Ff7).unwrap();
    let mut next = before.clone();
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [61; 16], revision: 0, field: "fixture/large-visit".into(),
        source_identity: [62; 32], entry: FieldVisitEntryV1 { operation: [63; 16], arrival: vec![7; 1025] },
        anchor: vec![1], scratch_values: [0; 256], scratch_known_masks: [0; 256], active_continuation: None,
    });
    let operation = FieldVisitOperation { id: [63; 16], kind: FieldVisitOperationKind::Enter };
    let committed = store.commit_field_visit(&operation, &before, &next).unwrap();
    assert_eq!(committed.commit, scene::SceneCommit::Applied);
    assert_eq!(committed.current.pair.character.as_ref().unwrap().schema_version, 5);
    assert_eq!(store.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
    let path = store.character_path(account.0, GameId::Ff7).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let record = scene::CharacterRecord::read(&path).unwrap().unwrap();
    assert!(record.field_visit.is_some() && record.last_visit_commit.is_some());
    drop(store);
    replace_header_version(dir.path(), index::FIELD_VISIT_INDEX_VERSION);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.index().format_version(), index::FIELD_VISIT_INDEX_VERSION,
        "opening and reading preserve an imported lower header until a successful write acknowledgment");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(scene::CharacterRecord::read(&path).unwrap().unwrap(), record);
    (dir, store, registered, record)
}

#[derive(Clone, Copy, Debug)]
enum OrdinaryPath { PutStory, UpdateStory, PutCharacter, UpdateCharacter, EnsureCharacter, ExactStory, ExactStoryDuplicate }

#[test]
fn ordinary_saves_and_existing_record_acknowledgments_preserve_large_visit_schema_and_receipt() {
    for path in [OrdinaryPath::PutStory, OrdinaryPath::UpdateStory, OrdinaryPath::PutCharacter,
        OrdinaryPath::UpdateCharacter, OrdinaryPath::EnsureCharacter, OrdinaryPath::ExactStory,
        OrdinaryPath::ExactStoryDuplicate] {
        let (dir, store, registered, record) = large_visit_under_old_header();
        let account = registered.account_id;
        let before = store.get_scene_state(account, GameId::Ff7).unwrap();
        let character_path = store.character_path(account.0, GameId::Ff7).unwrap();
        let character_bytes = std::fs::read(&character_path).unwrap();
        match path {
            OrdinaryPath::PutStory => {
                let mut next = before.pair.story.clone();
                next.blob.push(4);
                next.updated_at = 5;
                store.put_progression(&next).unwrap();
                assert_eq!(store.get_progression(account, GameId::Ff7).unwrap(), next);
            }
            OrdinaryPath::UpdateStory => {
                let next = store.update_progression(account, GameId::Ff7, |mut story| {
                    story.blob.push(5);
                    story.updated_at = 5;
                    story
                }).unwrap();
                assert_eq!(store.get_progression(account, GameId::Ff7).unwrap(), next);
                assert_ne!(next.blob, before.pair.story.blob);
            }
            OrdinaryPath::PutCharacter => {
                let mut next = before.pair.character.clone().unwrap();
                next.gil += 1;
                next.updated_at = 5;
                store.put_character_progress(&next).unwrap();
                assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap(), Some(next));
            }
            OrdinaryPath::UpdateCharacter => {
                store.update_character_progress(account, GameId::Ff7, |current| {
                    let mut next = current.unwrap();
                    next.ap += 1;
                    next.updated_at = 5;
                    next
                }).unwrap();
                let current = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
                assert_eq!(current.ap, record.progress.ap + 1);
            }
            OrdinaryPath::EnsureCharacter => {
                assert_eq!(store.ensure_character_progress(account, GameId::Ff7,
                    || panic!("an existing large visit must not call the initializer")).unwrap(), record.progress);
            }
            OrdinaryPath::ExactStory => {
                let (commit, current) = store.compare_exchange_progression(account, GameId::Ff7,
                    &before.pair.story.blob, b"after large visit", 5).unwrap();
                assert_eq!(commit, scene::SceneCommit::Applied);
                assert_eq!(current.story.blob, b"after large visit");
            }
            OrdinaryPath::ExactStoryDuplicate => {
                let (commit, current) = store.compare_exchange_progression(account, GameId::Ff7,
                    b"old delivery", &before.pair.story.blob, 5).unwrap();
                assert_eq!(commit, scene::SceneCommit::AlreadyApplied);
                assert_eq!(current, before.pair);
            }
        }
        assert_eq!(store.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION, "{path:?}");
        let after = scene::CharacterRecord::read(&character_path).unwrap().unwrap();
        assert_eq!(after.progress.schema_version, 5, "{path:?} must not downgrade the envelope");
        assert_eq!(after.field_visit, record.field_visit, "{path:?} retains the complete visit");
        assert_eq!(after.last_visit_commit, record.last_visit_commit, "{path:?} retains its private receipt");
        if !matches!(path, OrdinaryPath::PutCharacter | OrdinaryPath::UpdateCharacter) {
            assert_eq!(std::fs::read(&character_path).unwrap(), character_bytes,
                "{path:?} acknowledges the existing character without rewriting it");
        }
        let state = store.get_scene_state(account, GameId::Ff7).unwrap();
        assert_eq!(state.field_visit, before.field_visit);
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
        assert_eq!(reopened.get_scene_state(account, GameId::Ff7).unwrap(), state);
        assert_eq!(scene::CharacterRecord::read(&character_path).unwrap().unwrap(), after);
    }
}

#[test]
fn an_uncertain_existing_large_character_acknowledgment_preserves_its_record_until_reopen() {
    let (dir, store, registered, record) = large_visit_under_old_header();
    let account = registered.account_id;
    let before = records(&store, &registered);
    FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
    assert!(matches!(store.ensure_character_progress(account, GameId::Ff7,
        || panic!("an existing record must not be initialized")), Err(PersistError::Io { .. })));
    assert_eq!(records(&store, &registered), before);
    assert!(matches!(store.get_scene_state(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.put_character_progress(&record.progress), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.authenticate(&registered.token), Err(PersistError::TransactionRecoveryRequired)));
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.index().format_version(), index::LARGE_FIELD_VISIT_INDEX_VERSION);
    assert_eq!(records(&reopened, &registered), before);
    assert_eq!(reopened.ensure_character_progress(account, GameId::Ff7,
        || panic!("recovery must retain the existing record")).unwrap(), record.progress);
    let path = reopened.character_path(account.0, GameId::Ff7).unwrap();
    assert_eq!(scene::CharacterRecord::read(&path).unwrap().unwrap(), record);
    assert_eq!(records(&reopened, &registered), before);
}
