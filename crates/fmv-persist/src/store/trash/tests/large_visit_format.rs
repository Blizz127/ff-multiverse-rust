//! Large visit profiles keep their stronger compatibility fence through detached archives.

use super::*;
use crate::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1, SceneState};

fn require_format(store: &Store, minimum: u32) {
    let counters = store.writer().unwrap();
    let _progress = store.progress_write();
    store.ensure_index_format(&counters, minimum).unwrap();
}

fn copy_files(source: &BTreeMap<String, Vec<u8>>, target: &Path) {
    for (relative, bytes) in source {
        let path = target.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

fn records(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut records = files(dir);
    records.remove(MANIFEST_FILE);
    records
}

fn rekey(mut state: SceneState, account: AccountId) -> SceneState {
    state.pair.story.account_id = account;
    state.pair.character.as_mut().unwrap().account_id = account;
    state
}

fn enter(store: &Store, account: AccountId, id: u8, arrival: usize, anchor: usize) -> SceneState {
    let before = store.get_scene_state(account, GameId::Ff7).unwrap();
    let operation = FieldVisitOperation { id: [id; 16], kind: FieldVisitOperationKind::Enter };
    let mut next = before.clone();
    let mut values = [0; 256];
    let mut masks = [0; 256];
    values[31] = 0b1000_0010;
    masks[31] = 0b1000_1011;
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1,
        visit_id: [id + 10; 16],
        revision: 0,
        field: "fixture/large-profile".into(),
        source_identity: [9; 32],
        entry: FieldVisitEntryV1 { operation: operation.id, arrival: vec![253; arrival] },
        anchor: vec![254; anchor],
        scratch_values: values,
        scratch_known_masks: masks,
        active_continuation: None,
    });
    store.commit_field_visit(&operation, &before, &next).unwrap().current
}

fn set_header_version(root: &Path, version: u32) {
    let path = root.join(crate::index::INDEX_LOG_FILE);
    let text = fs::read_to_string(&path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    let (_, payload) = first.split_once(' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_str(payload).unwrap();
    header["schema_version"] = serde_json::json!(version);
    let payload = serde_json::to_string(&header).unwrap();
    let checksum = blake3::hash(payload.as_bytes()).to_hex();
    fs::write(path, format!("{} {payload}\n{rest}", &checksum[..8])).unwrap();
}

fn active_entry(store: &Store, root: &Path, stage: &str) -> PathBuf {
    require_format(store, crate::index::LARGE_FIELD_VISIT_INDEX_VERSION);
    // No large record can independently reject this fixture: the manifest itself
    // must protect even an identity-only intent from legacy recovery and cleanup.
    let (account, _) = store.create_account("Active archive", NOW).unwrap();
    if stage == "delete" {
        store.prepare_delete(account, NOW + 1).unwrap().dir
    } else {
        let deleted = store.delete_account(account, NOW + 1).unwrap();
        let next = store.writer().unwrap().next_account_id;
        FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
        assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
        root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry))
    }
}

#[test]
fn actual_large_entry_or_anchor_survives_import_rekey_and_an_older_manifest_wrapper() {
    for (arrival, anchor) in [(8_192, 3), (3, 8_192)] {
        let (_source_guard, source_root) = state_dir();
        let source = Store::open(&source_root).unwrap();
        let account = make(&source, "Large archived visit", 1).account_id;
        let original = enter(&source, account, 1, arrival, anchor);
        assert_eq!(original.pair.character.as_ref().unwrap().schema_version, 5);
        assert_eq!(source.index().format_version(), 6);
        let original_record = CharacterRecord::read(&source.character_path(account.0, GameId::Ff7).unwrap()).unwrap().unwrap();
        let receipt = serde_json::to_vec(&original_record.last_visit_commit).unwrap();
        let deleted = source.delete_account(account, NOW + 1).unwrap();
        let source_entry = source_root.join(TRASH_DIR).join(&deleted.entry);
        assert_eq!(read_manifest(&source_entry).unwrap().schema_version, 4);
        let manifest_path = source_entry.join(MANIFEST_FILE);
        assert!(matches!(fsio::check_schema_version_upto(&manifest_path, &fs::read(&manifest_path).unwrap(), 3),
            Err(PersistError::FutureSchema { found: 4, supported: 3, .. })));
        let source_archive = files(&source_entry);

        for old_index in 2..=5 {
            let mut archive = source_archive.clone();
            let mut entry = deleted.entry.clone();
            let mut expected = original.clone();
            let mut previous_account = account;
            for generation in 1..=2 {
                let (_guard, root) = state_dir();
                let target = Store::open(&root).unwrap();
                require_format(&target, old_index);
                for n in 0..generation { target.create_account(&format!("Unrelated {n}"), NOW).unwrap(); }
                let imported = root.join(TRASH_DIR).join(&entry);
                copy_files(&archive, &imported);
                if generation == 1 {
                    let mut manifest = read_manifest(&imported).unwrap();
                    manifest.schema_version = old_index - 1; // Cover wrappers 1, 2, 3 and 4.
                    write_manifest(&imported, &manifest).unwrap();
                }
                let archived_records = records(&imported);
                drop(target);
                let before = files(&root);
                let target = Store::open(&root).unwrap();
                assert_eq!(target.index().format_version(), old_index);
                let entries = target.trash_entries().unwrap();
                assert_eq!(entries.len(), 1);
                assert!(entries[0].problem.is_none());
                target.restore_check(&entry).unwrap();
                assert_eq!(files(&root), before, "a complete detached archive does not opt in its host store until restore");
                let restored = target.restore_account(&entry, NOW + 10 + generation).unwrap();
                assert!(restored.unfinished.is_empty());
                assert_ne!(restored.account_id, previous_account);
                previous_account = restored.account_id;
                expected = rekey(expected, restored.account_id);
                assert_eq!(target.index().format_version(), 6, "a schema-5 record cannot be hidden by an older manifest");
                assert_eq!(target.get_scene_state(restored.account_id, GameId::Ff7).unwrap(), expected);
                let sealed = root.join(TRASH_DIR).join(&restored.entry);
                assert_eq!(read_manifest(&sealed).unwrap().schema_version, 4);
                assert_eq!(records(&sealed), archived_records, "rekey never rewrites the original archived records");
                drop(target);
                let target = Store::open(&root).unwrap();
                assert_eq!(target.index().format_version(), 6);
                assert_eq!(target.get_scene_state(restored.account_id, GameId::Ff7).unwrap(), expected);
                let record = CharacterRecord::read(&target.character_path(restored.account_id.0, GameId::Ff7).unwrap()).unwrap().unwrap();
                assert_eq!(serde_json::to_vec(&record.last_visit_commit).unwrap(), receipt);
                let deleted = target.delete_account(restored.account_id, NOW + 20 + generation).unwrap();
                entry = deleted.entry;
                let exported = root.join(TRASH_DIR).join(&entry);
                assert_eq!(read_manifest(&exported).unwrap().schema_version, 4);
                archive = files(&exported);
            }
        }
        assert_eq!(files(&source_entry), source_archive);
    }
}

#[test]
fn snapshots_and_archives_retain_index_six_even_after_the_only_large_visit_shrinks() {
    for shrink in [false, true] {
        let (guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        let account = make(&store, "Snapshot visit", 1).account_id;
        let mut expected = enter(&store, account, 1, 1_025, 1_025);
        if shrink { expected = enter(&store, account, 2, 3, 3); }
        assert_eq!(expected.pair.character.as_ref().unwrap().schema_version, if shrink { 4 } else { 5 });
        assert_eq!(store.index().format_version(), 6);
        let (empty, _) = store.create_account("No progress", NOW).unwrap();
        let deleted_empty = store.delete_account(empty, NOW + 1).unwrap();
        let empty_entry = root.join(TRASH_DIR).join(&deleted_empty.entry);
        assert_eq!(read_manifest(&empty_entry).unwrap().schema_version, 4, "the store fence also protects records without a visit");

        let snapshot = guard.path().join("snapshot");
        let before = files(&root);
        store.snapshot(&snapshot).unwrap();
        assert_eq!(files(&root), before);
        assert!(!snapshot.join(TRASH_DIR).exists(), "snapshots exclude detached archives");
        let restored_root = guard.path().join("snapshot-restored");
        crate::restore_snapshot(&snapshot, &restored_root).unwrap();
        let restored = Store::open(&restored_root).unwrap();
        assert_eq!(restored.index().format_version(), 6);
        assert_eq!(restored.get_scene_state(account, GameId::Ff7).unwrap(), expected);
        assert!(restored.trash_entries().unwrap().is_empty());

        let deleted = store.delete_account(account, NOW + 2).unwrap();
        let archived = root.join(TRASH_DIR).join(&deleted.entry);
        assert_eq!(read_manifest(&archived).unwrap().schema_version, 4);
        for (entry, original) in [(&deleted.entry, &archived), (&deleted_empty.entry, &empty_entry)] {
            let (_target_guard, target_root) = state_dir();
            let target = Store::open(&target_root).unwrap();
            target.create_account("Unrelated", NOW).unwrap();
            let imported = target_root.join(TRASH_DIR).join(entry);
            copy_files(&files(original), &imported);
            let source_records = records(&imported);
            let outcome = target.restore_account(entry, NOW + 3).unwrap();
            assert!(outcome.unfinished.is_empty());
            assert_eq!(target.index().format_version(), 6, "manifest4 retains the source fence even with no schema5 record");
            assert_eq!(records(&target_root.join(TRASH_DIR).join(&outcome.entry)), source_records);
            if entry == &deleted.entry {
                assert_eq!(target.get_scene_state(outcome.account_id, GameId::Ff7).unwrap(), rekey(expected.clone(), outcome.account_id));
            } else {
                assert!(target.get_scene_state(outcome.account_id, GameId::Ff7).unwrap().pair.character.is_none());
            }
        }
    }
}

#[test]
fn large_restore_faults_keep_the_format_archive_and_exactly_one_account_allocation() {
    for stage in ["format", "rename", "append", "progress"] {
        let (_source_guard, source_root) = state_dir();
        let source = Store::open(&source_root).unwrap();
        let account = make(&source, "Archived", 1).account_id;
        let expected = enter(&source, account, 1, 1_025, 3);
        let deleted = source.delete_account(account, NOW + 1).unwrap();
        let archive = files(&source_root.join(TRASH_DIR).join(&deleted.entry));
        let (_target_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        store.create_account("Unrelated", NOW).unwrap();
        let entry = root.join(TRASH_DIR).join(&deleted.entry);
        copy_files(&archive, &entry);
        let source_records = records(&entry);
        let next = store.writer().unwrap().next_account_id;
        let mut before = files(&root);
        before.remove(crate::index::INDEX_LOG_FILE);
        match stage {
            "format" => crate::store::FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true)),
            "rename" => FAIL_RESTORE_RENAME.with(|flag| flag.set(true)),
            "append" => FAIL_RESTORE_APPEND.with(|flag| flag.set(true)),
            "progress" => FAIL_RESTORE_PROGRESS.with(|flag| flag.set(true)),
            _ => unreachable!(),
        }
        let result = store.restore_account(&deleted.entry, NOW + 2);
        if stage == "progress" { assert!(!result.unwrap().unfinished.is_empty()); }
        else { assert!(result.is_err()); }
        let restoring = root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry));
        if stage == "format" {
            let mut after = files(&root);
            after.remove(crate::index::INDEX_LOG_FILE);
            assert_eq!(after, before, "the format fence is written before any archive or identity installation");
            assert_eq!(files(&entry), archive);
            assert!(!restoring.exists());
        } else {
            let manifest = read_manifest(&restoring).unwrap();
            assert_eq!(manifest.schema_version, 4);
            assert_eq!(manifest.restore.unwrap().account_id, next);
            assert_eq!(records(&restoring), source_records);
            assert_eq!(store.index().format_version(), 6);
        }
        let before = files(&root);
        assert!(store.create_account("Cannot allocate", NOW + 3).is_err());
        assert_eq!(files(&root), before);
        drop(store);
        if stage == "progress" { fs::remove_file(root.join(PROGRESSION_DIR).join(next.to_string())).unwrap(); }

        let recovered = Store::open(&root).unwrap();
        assert_eq!(recovered.index().format_version(), 6);
        assert!(!restoring.exists());
        let restored_account = if matches!(stage, "format" | "rename") {
            assert_eq!(recovered.writer().unwrap().next_account_id, next);
            assert_eq!(read_manifest(&entry).unwrap().schema_version, 4);
            assert!(read_manifest(&entry).unwrap().restore.is_none());
            assert_eq!(records(&entry), source_records);
            recovered.restore_account(&deleted.entry, NOW + 4).unwrap().account_id
        } else {
            assert_eq!(recovered.writer().unwrap().next_account_id, next + 1);
            assert_eq!(recovered.find_account_by_name("Archived"), Some(AccountId(next)));
            AccountId(next)
        };
        assert_eq!(restored_account, AccountId(next));
        assert_eq!(recovered.get_scene_state(restored_account, GameId::Ff7).unwrap(), rekey(expected, restored_account));
        let sealed = root.join(TRASH_DIR).join(format!("{}.restored-{next}", deleted.entry));
        assert_eq!(read_manifest(&sealed).unwrap().schema_version, 4);
        assert_eq!(records(&sealed), source_records);
        let (later, _) = recovered.create_account("Later", NOW + 5).unwrap();
        assert_eq!(later, AccountId(next + 1), "recovery cannot duplicate or skip account allocation");
    }
}

#[test]
fn active_manifest_four_refuses_every_lower_index_before_cleanup_or_install() {
    for stage in ["delete", "restore"] {
        for lower in 2..=5 {
            let (_guard, root) = state_dir();
            let store = Store::open(&root).unwrap();
            let entry = active_entry(&store, &root, stage);
            assert_eq!(read_manifest(&entry).unwrap().schema_version, 4);
            drop(store);
            set_header_version(&root, lower);
            let before = files(&root);
            assert!(Store::open(&root).is_err(), "{stage} manifest4 under index{lower}");
            assert_eq!(files(&root), before, "a lower reader cannot abandon an active newer intent");
        }
    }
}

#[test]
fn deleting_a_large_character_under_a_lower_index_refuses_before_staging_any_intent() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let account = make(&store, "Copied large character", 1).account_id;
    let expected = enter(&store, account, 1, 1_025, 3);
    assert_eq!(expected.pair.character.as_ref().unwrap().schema_version, 5);
    drop(store);
    set_header_version(&root, 5);
    let store = Store::open(&root).unwrap();
    let before = files(&root);
    assert!(matches!(store.delete_account(account, NOW + 1), Err(PersistError::Corrupt { .. })));
    assert_eq!(files(&root), before, "neither live records nor a weaker detached manifest may be written");
    assert!(!root.join(TRASH_DIR).exists(), "the refusal precedes even the trash directory");
    assert_eq!(store.find_account_by_name("Copied large character"), Some(account));
    assert_eq!(store.index().format_version(), 5, "deletion does not eagerly opt in a malformed downgraded store");
}

#[test]
fn an_older_active_wrapper_cannot_hide_a_large_live_or_archived_character() {
    for wrapper in [1, 3] {
        for stage in ["abandoned-delete", "unmoved-delete", "moved-delete", "uncommitted-restore", "committed-restore"] {
            let (_guard, root) = state_dir();
            let store = Store::open(&root).unwrap();
            let account = make(&store, "Large active record", 1).account_id;
            let state = enter(&store, account, 1, 3, 1_025);
            assert_eq!(state.pair.character.as_ref().unwrap().schema_version, 5);
            let entry = if stage.ends_with("delete") {
                let prepared = store.prepare_delete(account, NOW + 1).unwrap();
                if stage != "abandoned-delete" { store.commit_delete(account).unwrap(); }
                if stage == "moved-delete" {
                    let manifest = read_manifest(&prepared.dir).unwrap();
                    assert!(move_into_entry(&root, &prepared.dir, &manifest).failed.is_empty());
                }
                prepared.dir
            } else {
                let deleted = store.delete_account(account, NOW + 1).unwrap();
                let next = store.writer().unwrap().next_account_id;
                if stage == "uncommitted-restore" { FAIL_RESTORE_RENAME.with(|flag| flag.set(true)); }
                else { FAIL_RESTORE_APPEND.with(|flag| flag.set(true)); }
                assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
                root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry))
            };
            let mut manifest = read_manifest(&entry).unwrap();
            manifest.schema_version = wrapper;
            write_manifest(&entry, &manifest).unwrap();
            drop(store);
            set_header_version(&root, 5);
            let before = files(&root);
            assert!(Store::open(&root).is_err(), "{stage} manifest{wrapper} still names a schema5 character");
            assert_eq!(files(&root), before, "neither the live nor archived large record may be installed or discarded under index5");
        }
    }
}

#[test]
fn malformed_or_future_active_manifests_cannot_bypass_the_version_fence() {
    for stage in ["delete", "restore"] {
        for index_version in [5, 6] {
            for damage in ["future", "missing", "string", "truncated"] {
                let (_guard, root) = state_dir();
                let store = Store::open(&root).unwrap();
                let entry = active_entry(&store, &root, stage);
                let path = entry.join(MANIFEST_FILE);
                let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                match damage {
                    "future" => manifest["schema_version"] = serde_json::json!(5),
                    "missing" => { manifest.as_object_mut().unwrap().remove("schema_version"); },
                    "string" => manifest["schema_version"] = serde_json::json!("4"),
                    "truncated" => {},
                    _ => unreachable!(),
                }
                let bytes = if damage == "truncated" { br#"{"schema_version":4,"kind":"#.to_vec() }
                    else { serde_json::to_vec(&manifest).unwrap() };
                fs::write(&path, bytes).unwrap();
                drop(store);
                set_header_version(&root, index_version);
                let before = files(&root);
                assert!(Store::open(&root).is_err(), "{damage} {stage} under index{index_version}");
                assert_eq!(files(&root), before, "invalid active metadata must survive unchanged for inspection");
            }
        }
    }
}
