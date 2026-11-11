//! Raising compatibility changes only the index fence, never initializes a field visit.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fmv_core::{GameId, InventoryEntry};

use crate::index::{self, StoreCompatibility, DURABLE_STORY_INDEX_VERSION, FIELD_VISIT_INDEX_VERSION,
    INDEX_LOG_FILE, LARGE_FIELD_VISIT_INDEX_VERSION, SCENE_BATCH_INDEX_VERSION};
use crate::model::INDEX_LOG_VERSION;
use crate::store::{Store, FAIL_SCENE_FORMAT_AFTER_RENAME};
use crate::{CharacterProgress, NewPlayerCharacter, PersistError, Progression, Registered};

fn known_records() -> (tempfile::TempDir, Store, Registered) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let registered = store.register_with_character("Compatible", [1, 2, 3], "original", None,
        Some(NewPlayerCharacter { body: "original".into(), ..NewPlayerCharacter::default() }), 1).unwrap();
    let mut character = CharacterProgress::new(registered.account_id, GameId::Ff7, 0, 6, 2);
    character.gil = 240;
    character.inventory = Some(vec![InventoryEntry { item: 1, count: 3 }]);
    store.put_character_progress(&character).unwrap();
    store.put_progression(&Progression::new(registered.account_id, GameId::Ff7, vec![1, 39, 0], 3)).unwrap();
    assert_eq!(store.index().format_version(), INDEX_LOG_VERSION);
    (dir, store, registered)
}

fn records(store: &Store, registered: &Registered) -> Vec<(PathBuf, Vec<u8>)> {
    let account = registered.account_id.0;
    [store.account_path(account), store.avatar_path(registered.avatar.avatar_id.0),
        store.character_record_path(account), store.character_path(account, GameId::Ff7).unwrap(),
        store.progression_path(account, GameId::Ff7).unwrap()]
        .into_iter().map(|path| { let bytes = std::fs::read(&path).unwrap(); (path, bytes) }).collect()
}

fn entries(path: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = std::fs::read_dir(path).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    names.sort();
    names
}

/// Known directories suffice: an unexpected account, progress subdirectory, intent, or
/// field-visit file changes one of these lists without recursively scanning the filesystem.
fn layout(store: &Store, registered: Option<&Registered>) -> Vec<(PathBuf, Vec<OsString>)> {
    let mut paths = vec![store.root().to_path_buf()];
    paths.extend(["accounts", "avatars", "characters", "progression"].map(|name| store.root().join(name)));
    if let Some(registered) = registered { paths.push(store.progression_dir(registered.account_id.0)); }
    paths.into_iter().map(|path| { let names = entries(&path); (path, names) }).collect()
}

fn legacy_fixture(version: u32) -> (tempfile::TempDir, Store, Registered) {
    let (dir, store, registered) = known_records();
    drop(store);
    let path = dir.path().join(INDEX_LOG_FILE);
    let prior = index::load_log(&path, false).unwrap().unwrap();
    index::write_compacted_version(&path, &prior.index, prior.next_account_id, prior.next_avatar_id, version).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.index().format_version(), version);
    (dir, store, registered)
}

#[test]
fn public_compatibility_is_monotonic_and_never_initializes_or_rewrites_game_records() {
    // Even an empty store can opt into a fence without creating an account or defaults.
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let empty_layout = layout(&store, None);
    assert_eq!(store.index().format_version(), INDEX_LOG_VERSION);
    for (minimum, version) in [(StoreCompatibility::DurableStory, DURABLE_STORY_INDEX_VERSION),
        (StoreCompatibility::FieldVisit, FIELD_VISIT_INDEX_VERSION)] {
        store.require_compatibility(minimum).unwrap();
        assert_eq!(store.index().format_version(), version);
        assert_eq!(store.account_count(), 0);
        assert_eq!(layout(&store, None), empty_layout);
    }
    drop(store);

    let (_dir, store, registered) = known_records();
    let before = records(&store, &registered);
    let before_layout = layout(&store, Some(&registered));
    for (minimum, version) in [(StoreCompatibility::DurableStory, DURABLE_STORY_INDEX_VERSION),
        (StoreCompatibility::FieldVisit, FIELD_VISIT_INDEX_VERSION)] {
        store.require_compatibility(minimum).unwrap();
        assert_eq!(store.index().format_version(), version);
        assert_eq!(records(&store, &registered), before);
        assert_eq!(layout(&store, Some(&registered)), before_layout);
        assert_eq!(store.account_count(), 1);
        let index_bytes = std::fs::read(store.log_path()).unwrap();
        store.require_compatibility(minimum).unwrap();
        store.require_compatibility(StoreCompatibility::DurableStory).unwrap();
        assert_eq!(store.index().format_version(), version);
        assert_eq!(std::fs::read(store.log_path()).unwrap(), index_bytes, "lower/repeated requests do no I/O");
        assert_eq!(records(&store, &registered), before);
        assert_eq!(layout(&store, Some(&registered)), before_layout);
    }
    // Selecting FieldVisit does not change the existing no-seed registration API.
    let (account, _) = store.create_account("Unseeded", 4).unwrap();
    assert_eq!(account.0, registered.account_id.0 + 1, "the fences consumed no account ids");
    assert!(store.avatar_for_account(account).unwrap().is_none());
    assert!(store.get_character_progress(account, GameId::Ff7).unwrap().is_none());
    assert!(store.get_progression(account, GameId::Ff7).unwrap().blob.is_empty());
    assert!(!store.character_record_path(account.0).exists());
    assert!(!store.progression_dir(account.0).exists(), "reading absent defaults initializes no visit or progress");
    assert_eq!(records(&store, &registered), before);
}

#[test]
fn each_legacy_header_stays_exact_at_minimum_two_and_appends_to_the_upgraded_log() {
    for version in [INDEX_LOG_VERSION, SCENE_BATCH_INDEX_VERSION, DURABLE_STORY_INDEX_VERSION] {
        let (dir, store, registered) = legacy_fixture(version);
        let before = records(&store, &registered);
        let before_layout = layout(&store, Some(&registered));
        let index_bytes = std::fs::read(store.log_path()).unwrap();
        {
            let counters = store.writer().unwrap();
            let _progress = store.progress_write();
            store.ensure_index_format(&counters, INDEX_LOG_VERSION).unwrap();
        }
        assert_eq!(store.index().format_version(), version);
        assert_eq!(std::fs::read(store.log_path()).unwrap(), index_bytes);
        assert_eq!(records(&store, &registered), before);
        assert_eq!(layout(&store, Some(&registered)), before_layout);

        store.require_compatibility(StoreCompatibility::FieldVisit).unwrap();
        assert_eq!(store.index().format_version(), FIELD_VISIT_INDEX_VERSION);
        assert_eq!(records(&store, &registered), before);
        assert_eq!(layout(&store, Some(&registered)), before_layout);
        assert!(store.unclaimed_accounts().iter().any(|(id, _)| *id == registered.account_id));
        assert_eq!(store.authenticate(&registered.token).unwrap(), registered.account_id);
        let (added, token) = store.create_account("After Fence", 4).unwrap();
        drop(store);

        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), FIELD_VISIT_INDEX_VERSION);
        assert_eq!(reopened.account_count(), 2);
        assert!(!reopened.unclaimed_accounts().iter().any(|(id, _)| *id == registered.account_id),
            "the claim appended to the replacement inode survived reopen");
        assert_eq!(reopened.authenticate(&token).unwrap(), added, "new registration used the replacement inode too");
        assert_eq!(records(&reopened, &registered), before);
    }
}

#[test]
fn an_uncertain_field_visit_fence_blocks_progress_and_registration_until_reopen() {
    for version in [INDEX_LOG_VERSION, SCENE_BATCH_INDEX_VERSION, DURABLE_STORY_INDEX_VERSION] {
        let (dir, store, registered) = legacy_fixture(version);
        let account = registered.account_id;
        let before = records(&store, &registered);
        let before_layout = layout(&store, Some(&registered));
        FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
        assert!(matches!(store.require_compatibility(StoreCompatibility::FieldVisit), Err(PersistError::Io { .. })));
        assert!(matches!(store.get_progression(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_character_progress(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_progress_pair(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.authenticate(&registered.token), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.put_progression(&Progression::empty(account, GameId::Ff7)), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.update_character_progress(account, GameId::Ff7, |_| panic!("fenced delta must not run")),
            Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.create_account("Blocked", 4), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.require_compatibility(StoreCompatibility::DurableStory), Err(PersistError::TransactionRecoveryRequired)));
        assert_eq!(records(&store, &registered), before);
        assert_eq!(layout(&store, Some(&registered)), before_layout);
        drop(store);

        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), FIELD_VISIT_INDEX_VERSION);
        assert_eq!(reopened.account_count(), 1);
        assert_eq!(records(&reopened, &registered), before);
        assert_eq!(layout(&reopened, Some(&registered)), before_layout);
        assert_eq!(reopened.get_progression(account, GameId::Ff7).unwrap().blob, [1, 39, 0]);
        assert_eq!(reopened.get_character_progress(account, GameId::Ff7).unwrap().unwrap().gil, 240);
        let (created, _) = reopened.create_account("After Recovery", 5).unwrap();
        assert_eq!(created.0, account.0 + 1, "the refused registration consumed no ids or records");
    }
}

#[test]
fn unsupported_minima_refuse_before_poison_or_any_filesystem_change() {
    // Also check a store already at the maximum: invalid input must not take the
    // otherwise harmless lower-minimum fastpath.
    for version in [INDEX_LOG_VERSION, FIELD_VISIT_INDEX_VERSION, LARGE_FIELD_VISIT_INDEX_VERSION] {
        for minimum in [0, 1, LARGE_FIELD_VISIT_INDEX_VERSION + 1, u32::MAX] {
            let (_dir, store, registered) = legacy_fixture(version);
            let before = records(&store, &registered);
            let before_layout = layout(&store, Some(&registered));
            let index_bytes = std::fs::read(store.log_path()).unwrap();
            {
                let counters = store.writer().unwrap();
                let _progress = store.progress_write();
                assert!(matches!(store.ensure_index_format(&counters, minimum),
                    Err(PersistError::InvalidField { field: "store_compatibility", .. })));
            }
            assert_eq!(store.index().format_version(), version);
            assert_eq!(std::fs::read(store.log_path()).unwrap(), index_bytes);
            assert_eq!(records(&store, &registered), before);
            assert_eq!(layout(&store, Some(&registered)), before_layout);
            let story = store.get_progression(registered.account_id, GameId::Ff7).unwrap();
            store.put_progression(&story).unwrap();
            assert_eq!(records(&store, &registered), before);
            assert!(store.create_account("Still Writable", 4).is_ok());
        }
    }
}
