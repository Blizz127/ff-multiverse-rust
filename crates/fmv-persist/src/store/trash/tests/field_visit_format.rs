//! Detached archives retain the field-visit format fence through import and recovery.

use super::*;

fn require_format(store: &Store, minimum: u32) {
    let counters = store.writer().unwrap();
    let _progress = store.progress_write();
    store.ensure_index_format(&counters, minimum).unwrap();
}

fn field_visit_format(store: &Store) {
    require_format(store, crate::index::FIELD_VISIT_INDEX_VERSION);
    assert_eq!(store.index().format_version(), 5);
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

fn rekey(mut pair: crate::ProgressPair, account: AccountId) -> crate::ProgressPair {
    pair.story.account_id = account;
    if let Some(character) = &mut pair.character { character.account_id = account; }
    pair
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

#[test]
fn legacy_archive_versions_keep_their_meaning_and_an_old_max_two_reader_refuses_three() {
    for (index_version, manifest_version) in [(2, 1), (3, 1), (4, 2), (5, 3)] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        require_format(&store, index_version);
        let (account, _) = store.create_account("No saved story", NOW).unwrap();
        let deleted = store.delete_account(account, NOW + 1).unwrap();
        let entry = root.join(TRASH_DIR).join(&deleted.entry);
        let path = entry.join(MANIFEST_FILE);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(read_manifest(&entry).unwrap().schema_version, manifest_version);
        assert_eq!(read_manifest_schema(&entry).unwrap(), Some(manifest_version));
        if manifest_version == 3 {
            assert!(matches!(fsio::check_schema_version_upto(&path, &bytes, 2),
                Err(PersistError::FutureSchema { found: 3, supported: 2, .. })));
        } else {
            assert_eq!(fsio::check_schema_version_upto(&path, &bytes, 2).unwrap(), manifest_version);
        }
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn copied_v3_archives_with_only_story_or_no_progress_require_v5_on_every_rekey() {
    for with_story in [false, true] {
        for old_index in [2, 3, 4] {
            let (_source_guard, root) = state_dir();
            let source = Store::open(&root).unwrap();
            field_visit_format(&source);
            let (account, _) = source.create_account("Archived", NOW).unwrap();
            if with_story {
                let bytes = (0..5_117usize).map(|i| ((i * 17) % 256) as u8).collect();
                source.put_progression(&Progression::new(account, GameId::Ff7, bytes, NOW)).unwrap();
            }
            let mut expected = source.get_progress_pair(account, GameId::Ff7).unwrap();
            assert!(expected.character.is_none());
            let deleted = source.delete_account(account, NOW + 1).unwrap();
            let mut entry = deleted.entry;
            let mut archive = files(&root.join(TRASH_DIR).join(&entry));
            let mut prior_account = account;

            for generation in 1..=2 {
                let (_target_guard, target_root) = state_dir();
                let target = Store::open(&target_root).unwrap();
                require_format(&target, old_index);
                for member in 0..generation { target.create_account(&format!("Unrelated {member}"), NOW).unwrap(); }
                let imported = target_root.join(TRASH_DIR).join(&entry);
                copy_files(&archive, &imported);
                assert_eq!(read_manifest(&imported).unwrap().schema_version, 3);
                drop(target);

                let before = files(&target_root);
                let target = Store::open(&target_root).unwrap();
                assert_eq!(target.index().format_version(), old_index);
                let listed = target.trash_entries().unwrap();
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].state, TrashState::Deleted);
                assert!(listed[0].problem.is_none());
                target.restore_check(&entry).unwrap();
                assert_eq!(files(&target_root), before, "opening, listing and dry run do not upgrade a complete detached archive");
                let restored = target.restore_account(&entry, NOW + 10 + generation).unwrap();
                assert!(restored.unfinished.is_empty());
                assert_ne!(restored.account_id, prior_account);
                prior_account = restored.account_id;
                expected = rekey(expected, restored.account_id);
                assert_eq!(target.index().format_version(), 5, "manifest3 alone requires v5, even without character or story records");
                assert_eq!(target.get_progress_pair(restored.account_id, GameId::Ff7).unwrap(), expected);
                let sealed = target_root.join(TRASH_DIR).join(&restored.entry);
                assert_eq!(read_manifest(&sealed).unwrap().schema_version, 3);
                let mut archived_records = archive.clone();
                archived_records.remove(MANIFEST_FILE);
                assert_eq!(records(&sealed), archived_records);
                drop(target);

                let target = Store::open(&target_root).unwrap();
                assert_eq!(target.index().format_version(), 5);
                assert_eq!(target.get_progress_pair(restored.account_id, GameId::Ff7).unwrap(), expected);
                let deleted = target.delete_account(restored.account_id, NOW + 20 + generation).unwrap();
                entry = deleted.entry;
                let next_archive = target_root.join(TRASH_DIR).join(&entry);
                assert_eq!(read_manifest(&next_archive).unwrap().schema_version, 3);
                archive = files(&next_archive);
            }
        }
    }
}

#[test]
fn active_v3_archives_refuse_lower_index_versions_before_abandoning_or_finishing_intents() {
    for stage in ["delete", "restore"] {
        for lower in [2, 3, 4] {
            let (_guard, root) = state_dir();
            let store = Store::open(&root).unwrap();
            field_visit_format(&store);
            let account = make(&store, "Still protected", 1).account_id;
            if stage == "delete" {
                let prepared = store.prepare_delete(account, NOW + 1).unwrap();
                assert_eq!(read_manifest(&prepared.dir).unwrap().schema_version, 3);
                assert_eq!(store.find_account_by_name("Still protected"), Some(account), "the live-account abandoned-delete branch must also refuse");
            } else {
                let deleted = store.delete_account(account, NOW + 1).unwrap();
                FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
                assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
            }
            drop(store);
            set_header_version(&root, lower);
            let before = files(&root);
            assert!(Store::open(&root).is_err(), "{stage} manifest3 under index{lower}");
            assert_eq!(files(&root), before);
        }
    }
}

#[test]
fn future_active_manifest_four_is_refused_even_under_index_five() {
    for stage in ["delete", "restore"] {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        field_visit_format(&store);
        let (account, _) = store.create_account("Future intent", NOW).unwrap();
        let entry = if stage == "delete" {
            store.prepare_delete(account, NOW + 1).unwrap().dir
        } else {
            let deleted = store.delete_account(account, NOW + 1).unwrap();
            let next = store.writer().unwrap().next_account_id;
            FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
            assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
            root.join(TRASH_DIR).join(format!("{}.restoring-{next}", deleted.entry))
        };
        let mut manifest = read_manifest(&entry).unwrap();
        manifest.schema_version = 4;
        write_manifest(&entry, &manifest).unwrap();
        drop(store);
        let before = files(&root);
        assert!(Store::open(&root).is_err(), "future {stage} manifest must never reach legacy cleanup");
        assert_eq!(files(&root), before);
    }
}

#[test]
fn v3_restore_failures_preserve_archive_bytes_and_recover_committed_and_uncommitted_ids_correctly() {
    for stage in ["rename", "append", "progress"] {
        let (_source_guard, source_root) = state_dir();
        let source = Store::open(&source_root).unwrap();
        field_visit_format(&source);
        let account = make(&source, "Archived", 1).account_id;
        let original_pair = source.get_progress_pair(account, GameId::Ff7).unwrap();
        let deleted = source.delete_account(account, NOW + 1).unwrap();
        let archive = files(&source_root.join(TRASH_DIR).join(&deleted.entry));
        let (_target_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        store.create_account("Unrelated", NOW).unwrap();
        let entry = root.join(TRASH_DIR).join(&deleted.entry);
        copy_files(&archive, &entry);
        let source_records = records(&entry);
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
        assert_eq!(manifest.schema_version, 3);
        assert_eq!(manifest.restore.unwrap().account_id, next);
        assert_eq!(records(&restoring), source_records);
        assert_eq!(store.index().format_version(), 5);
        let before = files(&root);
        assert!(store.create_account("Cannot allocate", NOW + 3).is_err());
        assert_eq!(files(&root), before);
        drop(store);
        if stage == "progress" { fs::remove_file(root.join(PROGRESSION_DIR).join(next.to_string())).unwrap(); }

        let recovered = Store::open(&root).unwrap();
        assert_eq!(recovered.index().format_version(), 5);
        assert!(!restoring.exists());
        let restored_account = if stage == "rename" {
            assert_eq!(recovered.writer().unwrap().next_account_id, next, "an uncommitted restore consumes no account id");
            assert_eq!(read_manifest(&entry).unwrap().schema_version, 3);
            assert!(read_manifest(&entry).unwrap().restore.is_none());
            assert_eq!(records(&entry), source_records);
            recovered.restore_account(&deleted.entry, NOW + 4).unwrap().account_id
        } else {
            assert_eq!(recovered.writer().unwrap().next_account_id, next + 1, "a committed restore must retain its id exactly once");
            assert_eq!(recovered.find_account_by_name("Archived"), Some(AccountId(next)));
            AccountId(next)
        };
        assert_eq!(restored_account, AccountId(next));
        assert_eq!(recovered.get_progress_pair(restored_account, GameId::Ff7).unwrap(), rekey(original_pair, restored_account));
        let sealed = root.join(TRASH_DIR).join(format!("{}.restored-{next}", deleted.entry));
        assert_eq!(read_manifest(&sealed).unwrap().schema_version, 3);
        assert_eq!(records(&sealed), source_records);
        let (later, _) = recovered.create_account("Later", NOW + 5).unwrap();
        assert_eq!(later, AccountId(next + 1), "failure and recovery create no duplicate or skipped allocation");
    }
}

#[test]
fn a_failed_v5_fence_mutates_no_archive_or_identity_and_consumes_no_account_id() {
    let (_source_guard, source_root) = state_dir();
    let source = Store::open(&source_root).unwrap();
    field_visit_format(&source);
    let account = make(&source, "Archived", 1).account_id;
    let expected = source.get_progress_pair(account, GameId::Ff7).unwrap();
    let deleted = source.delete_account(account, NOW + 1).unwrap();
    let archive = files(&source_root.join(TRASH_DIR).join(&deleted.entry));
    let (_target_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let entry = root.join(TRASH_DIR).join(&deleted.entry);
    copy_files(&archive, &entry);
    let next = store.writer().unwrap().next_account_id;
    let mut before = files(&root);
    before.remove(crate::index::INDEX_LOG_FILE);
    crate::store::FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
    assert!(store.restore_account(&deleted.entry, NOW + 2).is_err());
    assert_eq!(files(&entry), archive);
    assert!(read_manifest(&entry).unwrap().restore.is_none());
    let mut after = files(&root);
    after.remove(crate::index::INDEX_LOG_FILE);
    assert_eq!(after, before, "only the uncertain index replacement may have happened");
    assert!(store.create_account("Wait", NOW + 3).is_err());
    drop(store);

    let recovered = Store::open(&root).unwrap();
    assert_eq!(recovered.index().format_version(), 5);
    assert_eq!(recovered.writer().unwrap().next_account_id, next);
    assert_eq!(files(&entry), archive);
    let restored = recovered.restore_account(&deleted.entry, NOW + 4).unwrap();
    assert_eq!(restored.account_id, AccountId(next));
    assert_eq!(recovered.get_progress_pair(restored.account_id, GameId::Ff7).unwrap(), rekey(expected, restored.account_id));
    assert_eq!(read_manifest(&root.join(TRASH_DIR).join(&restored.entry)).unwrap().schema_version, 3);
}

#[test]
fn actual_visit_envelopes_and_their_receipts_survive_repeated_archive_rekeys() {
    use crate::{FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1, SceneCommit};

    let (_source_guard, source_root) = state_dir();
    let source = Store::open(&source_root).unwrap();
    let (account, _) = source.create_account("Visited account", NOW).unwrap();
    let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, NOW);
    character.gil = 240;
    character.inventory = Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(4, 1)]);
    source.put_character_progress(&character).unwrap();
    let before = source.get_scene_state(account, GameId::Ff7).unwrap();
    let operation = FieldVisitOperation { id: [9; 16], kind: FieldVisitOperationKind::Enter };
    let mut values = [0; 256];
    let mut masks = [0; 256];
    values[0] = 0b0100_0010;
    masks[0] = 0b1100_0111;
    values[21] = 0x91;
    masks[21] = 0xff;
    let visit = FieldVisitV1 {
        schema_version: 1,
        visit_id: [11; 16],
        revision: 0,
        field: "fixture/field".into(),
        source_identity: [7; 32],
        entry: FieldVisitEntryV1 { operation: operation.id, arrival: vec![1, 2, 3] },
        anchor: vec![4, 5, 6],
        scratch_values: values,
        scratch_known_masks: masks,
        active_continuation: None,
    };
    let mut next = before.clone();
    next.pair.story.blob = vec![4, 17, 0, 255];
    next.pair.story.updated_at = NOW + 1;
    next.pair.character.as_mut().unwrap().gil = 239;
    next.pair.character.as_mut().unwrap().updated_at = NOW + 1;
    next.field_visit = Some(visit.clone());
    let committed = source.commit_field_visit(&operation, &before, &next).unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    let expected = committed.current;
    assert_eq!(expected.pair.character.as_ref().unwrap().schema_version, 4);
    assert_eq!(source.index().format_version(), 5);
    let record = CharacterRecord::read(&source.character_path(account.0, GameId::Ff7).unwrap()).unwrap().unwrap();
    assert_eq!(record.field_visit, Some(visit));
    assert!(record.last_visit_commit.is_some());
    let visit_bytes = serde_json::to_vec(&record.field_visit).unwrap();
    let receipt_bytes = serde_json::to_vec(&record.last_visit_commit).unwrap();
    let deleted = source.delete_account(account, NOW + 2).unwrap();
    let source_entry = source_root.join(TRASH_DIR).join(&deleted.entry);
    assert_eq!(read_manifest(&source_entry).unwrap().schema_version, 3);
    let source_archive = files(&source_entry);

    // A legacy wrapper cannot hide the character envelope's stronger requirement. Every
    // subsequent export carries manifest 3, with no dependence on registration fingerprints.
    for first_manifest in [1, 2, 3] {
        let mut archive = source_archive.clone();
        let mut entry = deleted.entry.clone();
        let mut expected = expected.clone();
        let mut previous_account = account;
        for generation in 1..=2 {
            let (_target_guard, root) = state_dir();
            let target = Store::open(&root).unwrap();
            for other in 0..generation { target.create_account(&format!("Unrelated {other}"), NOW).unwrap(); }
            let imported = root.join(TRASH_DIR).join(&entry);
            copy_files(&archive, &imported);
            if generation == 1 {
                let mut manifest = read_manifest(&imported).unwrap();
                manifest.schema_version = first_manifest;
                write_manifest(&imported, &manifest).unwrap();
            }
            let source_records = records(&imported);
            assert_eq!(target.index().format_version(), 2);
            let restored = target.restore_account(&entry, NOW + 10 + generation).unwrap();
            assert!(restored.unfinished.is_empty());
            assert_ne!(restored.account_id, previous_account);
            previous_account = restored.account_id;
            expected.pair = rekey(expected.pair, restored.account_id);
            assert_eq!(target.index().format_version(), 5, "schema-4 character data requires index 5 even under manifest {first_manifest}");
            assert_eq!(target.get_scene_state(restored.account_id, GameId::Ff7).unwrap(), expected);
            let sealed = root.join(TRASH_DIR).join(&restored.entry);
            assert_eq!(read_manifest(&sealed).unwrap().schema_version, 3);
            assert_eq!(records(&sealed), source_records, "the original archived records are never rewritten");
            drop(target);

            let target = Store::open(&root).unwrap();
            assert_eq!(target.index().format_version(), 5);
            assert_eq!(target.get_scene_state(restored.account_id, GameId::Ff7).unwrap(), expected);
            let record = CharacterRecord::read(&target.character_path(restored.account_id.0, GameId::Ff7).unwrap()).unwrap().unwrap();
            assert_eq!(record.progress.schema_version, 4);
            assert_eq!(serde_json::to_vec(&record.field_visit).unwrap(), visit_bytes, "visit identity, partial knownness and opaque placement survive rekey");
            assert_eq!(serde_json::to_vec(&record.last_visit_commit).unwrap(), receipt_bytes, "the retained receipt is preserved exactly");
            let deleted = target.delete_account(restored.account_id, NOW + 20 + generation).unwrap();
            entry = deleted.entry;
            let exported = root.join(TRASH_DIR).join(&entry);
            assert_eq!(read_manifest(&exported).unwrap().schema_version, 3);
            archive = files(&exported);
        }
        assert_eq!(files(&source_entry), source_archive, "copying and restoring never changes the original archive");
    }
}
