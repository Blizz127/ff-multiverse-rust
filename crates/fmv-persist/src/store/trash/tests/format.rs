//! Archive version exclusion and recovery must preserve the durable story format fence.

use super::*;
use crate::NewProgression;

fn seed_story(store: &Store, name: &str) -> Registered {
    store.register_with_character_and_initial_state(name, [1, 2, 3], "default", None,
        None, None, Some(NewProgression { game: GameId::Ff7, blob: vec![3, 17, 0, 255] }), NOW).unwrap().0
}

fn legacy_archive(store: &Store) -> (Deleted, crate::ProgressPair) {
    let account = make(store, "Archived", 1).account_id;
    let pair = store.get_progress_pair(account, GameId::Ff7).unwrap();
    assert_eq!(store.index().format_version(), 2);
    let deleted = store.delete_account(account, NOW + 1).unwrap();
    assert_eq!(read_manifest(&store.root.join(TRASH_DIR).join(&deleted.entry)).unwrap().schema_version, 1);
    (deleted, pair)
}

fn without_manifest(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = files(dir);
    files.remove(MANIFEST_FILE);
    files
}

fn rekey(mut pair: crate::ProgressPair, account: AccountId) -> crate::ProgressPair {
    pair.story.account_id = account;
    if let Some(character) = &mut pair.character { character.account_id = account; }
    pair
}

fn downgrade_header(root: &Path, version: u32) {
    let path = root.join(crate::index::INDEX_LOG_FILE);
    let text = fs::read_to_string(&path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    let (_, payload) = first.split_once(' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_str(payload).unwrap();
    header["schema_version"] = serde_json::json!(version);
    let payload = serde_json::to_string(&header).unwrap();
    let digest = blake3::hash(payload.as_bytes()).to_hex();
    fs::write(path, format!("{} {payload}\n{rest}", &digest[..8])).unwrap();
}

#[test]
fn old_manifest_readers_accept_v1_and_refuse_v2_before_decoding_records() {
    for upgraded in [false, true] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        if upgraded { seed_story(&store, "Fence"); }
        let (account, _) = store.create_account("No story", NOW).unwrap();
        let deleted = store.delete_account(account, NOW + 1).unwrap();
        let entry = root.join(TRASH_DIR).join(&deleted.entry);
        let path = entry.join(MANIFEST_FILE);
        let bytes = fs::read(&path).unwrap();
        let version = if upgraded { 2 } else { 1 };
        assert_eq!(read_manifest(&entry).unwrap().schema_version, version);
        if upgraded {
            assert!(matches!(fsio::check_schema_version_upto(&path, &bytes, 1),
                Err(PersistError::FutureSchema { found: 2, supported: 1, .. })));
            assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        } else {
            assert_eq!(fsio::check_schema_version_upto(&path, &bytes, 1).unwrap(), 1);
            assert_eq!(store.index().format_version(), 2);
        }
        assert_eq!(fs::read(path).unwrap(), bytes, "version checks never rewrite archives");
    }
}

#[test]
fn a_legacy_story_restore_upgrades_its_manifest_before_the_recovery_intent_is_exposed() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let (deleted, _) = legacy_archive(&store);
    let original = root.join(TRASH_DIR).join(&deleted.entry);
    let source = without_manifest(&original);
    let next = store.writer().unwrap().next_account_id;
    FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
    assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
    let restoring = root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry));
    let manifest = read_manifest(&restoring).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.restore.unwrap().account_id, next);
    assert_eq!(store.index().format_version(), 4);
    assert_eq!(without_manifest(&restoring), source);
}

#[test]
fn interrupted_restore_stages_keep_v2_archives_and_recover_the_exact_progress() {
    for stage in ["rename", "append", "progress"] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        let (deleted, original_pair) = legacy_archive(&store);
        let deleted_path = root.join(TRASH_DIR).join(&deleted.entry);
        let source = without_manifest(&deleted_path);
        let next = store.writer().unwrap().next_account_id;
        match stage {
            "rename" => FAIL_RESTORE_RENAME.with(|flag| flag.set(true)),
            "append" => FAIL_RESTORE_APPEND.with(|flag| flag.set(true)),
            "progress" => FAIL_RESTORE_PROGRESS.with(|flag| flag.set(true)),
            _ => unreachable!(),
        }
        let result = store.restore_account(&deleted.entry, NOW + 2);
        if stage == "progress" { assert!(!result.unwrap().unfinished.is_empty()); }
        else { assert!(result.is_err()); }
        let restoring = root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry));
        let manifest = read_manifest(&restoring).unwrap();
        assert_eq!(manifest.schema_version, 2, "{stage}");
        assert_eq!(manifest.restore.unwrap().account_id, next);
        assert_eq!(without_manifest(&restoring), source);
        assert_eq!(store.index().format_version(), 4);
        let frozen = files(&root);
        assert!(store.create_account("Wait for recovery", NOW + 3).is_err());
        assert_eq!(files(&root), frozen);
        drop(store);
        if stage == "progress" { fs::remove_file(root.join(PROGRESSION_DIR).join(next.to_string())).unwrap(); }

        let recovered = Store::open(&root).unwrap();
        assert_eq!(recovered.index().format_version(), 4);
        assert!(!restoring.exists());
        let restored_account = if stage == "rename" {
            let manifest = read_manifest(&deleted_path).unwrap();
            assert_eq!(manifest.schema_version, 2, "rollback must not downgrade the archive format");
            assert!(manifest.restore.is_none());
            assert_eq!(without_manifest(&deleted_path), source);
            assert!(recovered.find_account_by_name("Archived").is_none());
            recovered.restore_account(&deleted.entry, NOW + 4).unwrap().account_id
        } else {
            assert_eq!(recovered.find_account_by_name("Archived"), Some(AccountId(next)));
            AccountId(next)
        };
        let sealed = root.join(TRASH_DIR).join(format!("{}.restored-{}", deleted.entry, restored_account.0));
        assert_eq!(read_manifest(&sealed).unwrap().schema_version, 2);
        assert_eq!(without_manifest(&sealed), source);
        assert_eq!(recovered.get_progress_pair(restored_account, GameId::Ff7).unwrap(), rekey(original_pair, restored_account));
    }
}

#[test]
fn a_failed_index_fence_leaves_the_legacy_archive_and_all_source_records_unchanged() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let (deleted, original_pair) = legacy_archive(&store);
    let entry = root.join(TRASH_DIR).join(&deleted.entry);
    let archive = files(&entry);
    let mut original_files = files(&root);
    original_files.remove(crate::index::INDEX_LOG_FILE);
    crate::store::FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
    assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
    assert_eq!(files(&entry), archive, "the manifest stays v1 until the global fence is durable");
    assert!(read_manifest(&entry).unwrap().restore.is_none());
    let mut current_files = files(&root);
    current_files.remove(crate::index::INDEX_LOG_FILE);
    assert_eq!(current_files, original_files, "only the uncertain header replacement may have happened");
    assert!(store.create_account("No allocation", NOW + 3).is_err());
    drop(store);

    let reopened = Store::open(&root).unwrap();
    assert_eq!(reopened.index().format_version(), 4);
    assert_eq!(files(&entry), archive);
    let restored = reopened.restore_account(&deleted.entry, NOW + 4).unwrap();
    assert_eq!(read_manifest(&root.join(TRASH_DIR).join(&restored.entry)).unwrap().schema_version, 2);
    assert_eq!(reopened.get_progress_pair(restored.account_id, GameId::Ff7).unwrap(), rekey(original_pair, restored.account_id));
}

#[test]
fn recovery_refuses_a_staged_v2_manifest_under_a_downgraded_index_without_touching_files() {
    for stage in ["delete", "restore"] {
        for version in [2, 3] {
            let (_guard, root) = state_dir();
            let store = Store::open(&root).unwrap();
            let registered = seed_story(&store, "Protected");
            if stage == "delete" {
                let prepared = store.prepare_delete(registered.account_id, NOW + 1).unwrap();
                assert_eq!(read_manifest(&prepared.dir).unwrap().schema_version, 2);
            } else {
                let deleted = store.delete_account(registered.account_id, NOW + 1).unwrap();
                FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
                assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
            }
            drop(store);
            downgrade_header(&root, version);
            let before = files(&root);
            assert!(Store::open(&root).is_err(), "{stage} manifest v2 cannot be recovered under index v{version}");
            assert_eq!(files(&root), before, "rejection precedes rollback, progress installation, or entry movement");
        }
    }
}

#[test]
fn a_complete_v2_archive_is_listable_in_a_v2_store_until_explicit_restore_raises_the_fence() {
    let (_source_guard, source_root) = state_dir();
    let source = Store::open(&source_root).unwrap();
    seed_story(&source, "Fence");
    let (account, _) = source.create_account("Imported without story", NOW).unwrap();
    let deleted = source.delete_account(account, NOW + 1).unwrap();
    let source_entry = source_root.join(TRASH_DIR).join(&deleted.entry);
    assert_eq!(read_manifest(&source_entry).unwrap().schema_version, 2);
    assert!(!source_entry.join(progression_rel(account.0, "Ff7.json")).exists());

    let (_target_guard, target_root) = state_dir();
    let target = Store::open(&target_root).unwrap();
    let imported = target_root.join(TRASH_DIR).join(&deleted.entry);
    for (relative, bytes) in files(&source_entry) {
        let path = imported.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    assert_eq!(target.index().format_version(), 2);
    drop(target);
    let before = files(&target_root);
    let target = Store::open(&target_root).unwrap();
    assert_eq!(target.index().format_version(), 2);
    let entries = target.trash_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, TrashState::Deleted);
    assert!(entries[0].problem.is_none());
    target.restore_check(&deleted.entry).unwrap();
    assert_eq!(files(&target_root), before, "open, listing and dry run preserve a detached complete archive");
    let restored = target.restore_account(&deleted.entry, NOW + 2).unwrap();
    assert!(restored.unfinished.is_empty());
    assert_eq!(target.index().format_version(), 4);
    assert_eq!(read_manifest(&target_root.join(TRASH_DIR).join(&restored.entry)).unwrap().schema_version, 2);
}

#[test]
fn damaged_or_future_pending_manifest_headers_cannot_bypass_the_outer_recovery_fence() {
    for version in [2, 3, 4] {
        for damage in ["kind", "foreign file", "future schema", "missing schema", "string schema", "truncated"] {
            if version == 4 && matches!(damage, "kind" | "foreign file") { continue; }
            let (_guard, root) = state_dir();
            let store = Store::open(&root).unwrap();
            let registered = seed_story(&store, "Still live");
            let prepared = store.prepare_delete(registered.account_id, NOW + 1).unwrap();
            let path = prepared.dir.join(MANIFEST_FILE);
            let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(manifest["schema_version"], 2);
            assert_eq!(store.find_account_by_name("Still live"), Some(registered.account_id));
            match damage {
                "kind" => manifest["kind"] = serde_json::json!("not-a-trash-entry"),
                "foreign file" => manifest["files"] = serde_json::json!(["accounts/999999.json"]),
                "future schema" => manifest["schema_version"] = serde_json::json!(5),
                "missing schema" => { manifest.as_object_mut().unwrap().remove("schema_version"); }
                "string schema" => manifest["schema_version"] = serde_json::json!("2"),
                "truncated" => {},
                _ => unreachable!(),
            }
            if damage == "truncated" { fs::write(&path, b"{\"schema_version\":2,").unwrap(); }
            else { fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap(); }
            drop(store);
            downgrade_header(&root, version);
            let before = files(&root);
            let opened = Store::open(&root);
            if damage == "future schema" {
                assert!(matches!(opened, Err(PersistError::FutureSchema { found: 5, supported: 4, .. })),
                    "a future archive version is refused even when index v{version} is current");
            } else {
                assert!(opened.is_err(), "pending {damage} under index v{version} cannot fall through to live-account cleanup");
            }
            assert_eq!(files(&root), before, "{damage}: the real manifest and live records survive refusal");
            assert!(prepared.dir.is_dir());
        }
    }
}

#[test]
fn legacy_schema_two_character_only_archives_keep_index_two_through_restore_and_recovery() {
    for interrupt in [false, true] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        let (account, _) = store.create_account("Legacy receipts", NOW).unwrap();
        let progress = CharacterProgress::new(account, GameId::Ff7, 1, 7, NOW);
        store.put_character_progress(&progress).unwrap();
        // A historical schema-2 envelope predates the stronger transaction writer fence.
        // No story file exists: only these character bytes decide the record requirement.
        let path = store.character_path(account.0, GameId::Ff7).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(2);
        value["scene_receipts"] = serde_json::to_value(BTreeMap::from([("a".repeat(64), "b".repeat(64))])).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let mut expected = CharacterRecord::read(&path).unwrap().unwrap();
        let deleted = store.delete_account(account, NOW + 1).unwrap();
        let entry = root.join(TRASH_DIR).join(&deleted.entry);
        let original = without_manifest(&entry);
        assert_eq!(read_manifest(&entry).unwrap().schema_version, 1);
        assert_eq!(store.index().format_version(), 2);
        if interrupt {
            FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
            assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
        }
        drop(store);
        let store = Store::open(&root).unwrap();
        assert_eq!(store.index().format_version(), 2, "legacy recovery must not impose a newer writer fence");
        assert_eq!(without_manifest(&entry), original);
        let restored = store.restore_account(&deleted.entry, NOW + 3).unwrap();
        assert!(restored.unfinished.is_empty());
        assert_eq!(store.index().format_version(), 2);
        assert_eq!(read_manifest(&root.join(TRASH_DIR).join(&restored.entry)).unwrap().schema_version, 1);
        expected.progress.account_id = restored.account_id;
        let actual = CharacterRecord::read(&store.character_path(restored.account_id.0, GameId::Ff7).unwrap()).unwrap().unwrap();
        assert_eq!(actual, expected, "legacy receipts survive the account rekey exactly");
    }
}

#[test]
fn historical_schema_three_characters_delete_and_recover_under_index_three() {
    for stage in ["complete", "abandon", "unmoved", "moved"] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        let (account, _) = store.create_account("Historical batch", NOW).unwrap();
        store.put_character_progress(&CharacterProgress::new(account, GameId::Ff7, 1, 7, NOW)).unwrap();
        {
            let counters = store.writer().unwrap();
            store.ensure_index_format(&counters, crate::index::SCENE_BATCH_INDEX_VERSION).unwrap();
        }
        // A completed historical batch has receipts, but no active continuation.
        let path = store.character_path(account.0, GameId::Ff7).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(3);
        let receipt = serde_json::json!({ "result": "b".repeat(64), "members": 1, "slot": 0 });
        value["batch_receipts"] = serde_json::to_value(BTreeMap::from([("a".repeat(64), receipt)])).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let mut expected = CharacterRecord::read(&path).unwrap().unwrap();
        let original = fs::read(&path).unwrap();
        let entry = if stage == "complete" {
            store.delete_account(account, NOW + 1).unwrap().entry
        } else {
            let prepared = store.prepare_delete(account, NOW + 1).unwrap();
            if stage != "abandon" { store.commit_delete(account).unwrap(); }
            if stage == "moved" {
                assert!(move_into_entry(&root, &prepared.dir, &prepared.manifest).failed.is_empty());
            }
            prepared.base
        };
        assert_eq!(store.index().format_version(), 3);
        drop(store);
        let store = Store::open(&root).unwrap();
        assert_eq!(store.index().format_version(), 3, "{stage}: historical recovery does not require index4");
        let entry = if stage == "abandon" {
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(store.trash_entries().unwrap().is_empty());
            store.delete_account(account, NOW + 2).unwrap().entry
        } else { entry };
        let dir = root.join(TRASH_DIR).join(&entry);
        assert_eq!(read_manifest(&dir).unwrap().schema_version, 1);
        assert_eq!(fs::read(dir.join(progression_rel(account.0, "Ff7.character.json"))).unwrap(), original);
        let restored = store.restore_account(&entry, NOW + 3).unwrap();
        assert!(restored.unfinished.is_empty());
        assert_eq!(store.index().format_version(), 4, "an explicit new restore still selects the index4 fence");
        expected.progress.account_id = restored.account_id;
        let actual = CharacterRecord::read(&store.character_path(restored.account_id.0, GameId::Ff7).unwrap()).unwrap().unwrap();
        assert_eq!(actual, expected);
    }
}
