//! The trash: names and stamps, and every state a crash can leave a delete or a restore in. The states are built
//! by running the real steps one at a time (`prepare_delete`, `commit_delete`, `move_into_entry`) and then dropping
//! the store, as a crash would, and opening the directory again. Temp directories only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use fmv_core::{GameId, InventoryEntry};

use super::*;
use crate::CharacterProgress;
use crate::character::{GameCharacter, NewPlayerCharacter};
use crate::model::Registered;

mod format;

const NOW: u64 = 1_700_000_000;

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    (dir, root)
}

/// An account with everything a played character has: avatar, player-character record, character progress with
/// an inventory, and a story blob.
fn make(store: &Store, name: &str, slot: u8) -> Registered {
    let entry = GameCharacter {
        binding: Some("ff7/body/sturdy".into()),
        slot: Some(slot),
        progress: GameCharacter::pointers(GameId::Ff7, Some(slot)),
    };
    let new = NewPlayerCharacter::for_game("sturdy", GameId::Ff7, entry);
    let reg = store.register_with_character(name, [10, 20, 30], "default", Some((GameId::Ff7, "ff7/body/sturdy")), Some(new), 100).unwrap();
    let mut progress = CharacterProgress::new(reg.account_id, GameId::Ff7, slot, 7, 200);
    progress.exp = 1234;
    progress.gil = 55;
    progress.inventory = Some(vec![InventoryEntry::new(3, 2)]);
    store.put_character_progress(&progress).unwrap();
    store.put_progression(&Progression::new(reg.account_id, GameId::Ff7, vec![1, 6, 0], 300)).unwrap();
    reg
}

/// Every file under `root` (relative path, `/` separators) with its bytes. `LOCK` is left out.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel != "LOCK" {
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn under(map: &BTreeMap<String, Vec<u8>>, prefix: &str) -> Vec<String> {
    map.keys().filter(|k| k.starts_with(prefix)).cloned().collect()
}

// ---- stamps and names ----------------------------------------------------------------------------------------------

#[test]
fn stamps_are_utc_and_sort_as_time_does() {
    // The values `date -u -d @N +%Y%m%dT%H%M%SZ` prints: the epoch, a leap day at the end of a century year that is
    // a leap year (2000), a leap day at noon, the turn of a century that is not a leap year, and an ordinary time.
    for (secs, want) in [
        (0, "19700101T000000Z"),
        (86_399, "19700101T235959Z"),
        (951_782_400, "20000229T000000Z"),
        (1_709_208_000, "20240229T120000Z"),
        (1_700_000_000, "20231114T221320Z"),
        (1_794_243_545, "20261109T165905Z"),
        (4_102_444_800, "21000101T000000Z"),
    ] {
        assert_eq!(utc_stamp(secs), want, "{secs}");
        assert!(is_stamp(want));
    }
    assert_eq!(utc_text(1_794_243_545), "2026-11-09T16:59:05Z");
    let mut last = utc_stamp(0);
    for secs in (0..4_000_000_000u64).step_by(86_399 * 37) {
        let stamp = utc_stamp(secs);
        assert!(stamp > last || secs == 0, "{secs}: {stamp} after {last}");
        last = stamp;
    }
    // The largest time cannot be given a year of four digits, but it does not panic.
    assert!(!utc_stamp(u64::MAX).is_empty());
}

#[test]
fn entry_names_say_their_state_and_nothing_else_parses() {
    let parsed = |name: &str| parse_name(name).map(|p| (p.base.to_owned(), p.account, p.state));
    let base = "20261109T165905Z-7";
    assert_eq!(parsed(base), Some((base.into(), 7, TrashState::Deleted)));
    assert_eq!(parsed("20261109T165905Z-7.pending"), Some((base.into(), 7, TrashState::Pending)));
    assert_eq!(parsed("20261109T165905Z-7.restoring-12"), Some((base.into(), 7, TrashState::Restoring { as_account: AccountId(12) })));
    assert_eq!(parsed("20261109T165905Z-7.restored-12"), Some((base.into(), 7, TrashState::Restored { as_account: AccountId(12) })));
    for bad in [
        "",
        "7",
        "-7",
        "20261109T165905Z",
        "20261109T165905Z-",
        "20261109T165905Z-+7",
        "20261109T165905Z- 7",
        "20261109T165905Z-7.pending.pending",
        "20261109T165905Z-7.restoring-",
        "20261109T165905Z-7.restoring-x",
        "20261109T165905Z-7.restored-+3",
        "20261109T165905Z-7.restore",
        "2026110T165905Z-7",
        "20261109t165905z-7",
        "20261109T165905Z-99999999999999999999999",
        "../20261109T165905Z-7",
        "20261109T165905Z-7/",
    ] {
        assert!(parse_name(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn a_manifest_may_only_name_records_of_its_own_account() {
    for rel in ["accounts/7.json", "characters/7.json", "avatars/3.json", "progression/7/Ff7.json", "progression/7/Ff7.character.json"] {
        assert!(owns(7, Some(3), rel), "{rel}");
    }
    for rel in [
        "accounts/8.json",
        "characters/8.json",
        "avatars/4.json",
        "progression/8/Ff7.json",
        "progression/7/Ff7.txt",
        "progression/7/../8/Ff7.json",
        "progression/7/sub/Ff7.json",
        "progression/7/.Ff7.json",
        "progression/7/Ff.7.json",
        "../accounts/7.json",
        "accounts/../accounts/7.json",
        "/etc/passwd",
        "index.log",
        "LOCK",
        "accounts/7.json/",
        "",
    ] {
        assert!(!owns(7, Some(3), rel), "{rel}");
    }
    // Without an avatar link no avatar file is the account's.
    assert!(!owns(7, None, "avatars/3.json"));
}

// ---- delete ------------------------------------------------------------------------------------------------------

#[test]
fn a_delete_moves_the_files_into_one_entry_and_nothing_else() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let b = make(&store, "Brom", 2);
    drop(store);
    let before = files(&root);
    let store = Store::open(&root).unwrap();
    let deleted = store.delete_account(a.account_id, NOW).unwrap();
    assert_eq!(deleted.display_name, "Aria");
    assert_eq!(deleted.entry, format!("{}-{}", utc_stamp(NOW), a.account_id.0));
    assert!(deleted.unfinished.is_empty());
    drop(store);
    let after = files(&root);

    // Aria's five files are in the entry, byte for byte, in the layout of the state directory.
    let mine = ["accounts/1.json", "avatars/1.json", "characters/1.json", "progression/1/Ff7.character.json", "progression/1/Ff7.json"];
    assert_eq!(deleted.files, ["accounts/1.json", "avatars/1.json", "characters/1.json", "progression/1/Ff7.character.json", "progression/1/Ff7.json"]);
    for rel in mine {
        assert!(!after.contains_key(rel), "{rel} is still in the state directory");
        assert_eq!(after.get(&format!("trash/{}/{rel}", deleted.entry)), before.get(rel), "{rel} in the trash");
    }
    assert!(!root.join("progression/1").exists(), "the emptied progression directory is gone");
    // Brom's files and every other file are byte for byte what they were; only the index grew, and the trash is new.
    for (rel, bytes) in &before {
        if mine.contains(&rel.as_str()) || rel == "index.log" {
            continue;
        }
        assert_eq!(after.get(rel), Some(bytes), "{rel} changed");
    }
    assert!(under(&before, "accounts/2").len() == 1 && under(&after, "progression/2").len() == 2);
    assert_eq!(under(&after, "trash/").len(), 6, "the manifest and the five files: {:?}", under(&after, "trash/"));
    assert!(after["index.log"].len() > before["index.log"].len() && after["index.log"].starts_with(&before["index.log"]), "the index only grew");
    let manifest = read_manifest(&root.join("trash").join(&deleted.entry)).unwrap();
    assert_eq!((manifest.account_id, manifest.display_name.as_str(), manifest.avatar_id, manifest.deleted_at), (1, "Aria", Some(1), NOW));
    assert_eq!(manifest.deleted_at_utc, "2023-11-14T22:13:20Z");
    let _ = b;
}

#[test]
fn a_crash_after_the_plan_leaves_the_account_intact() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let before = files(&root);
    {
        let _writer = store.writer().unwrap();
        let prepared = store.prepare_delete(a.account_id, NOW).unwrap();
        assert!(prepared.dir.join(MANIFEST_FILE).is_file());
        // ...and the process dies here.
    }
    drop(store);
    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("abandoned an interrupted delete of account 1")), "{:?}", store.open_warnings());
    assert_eq!(files(&root), before, "the plan is gone and nothing else changed");
    let token = a.token.clone();
    assert_eq!(store.authenticate(&token).unwrap(), a.account_id);
    assert_eq!(store.find_account_by_name("aria"), Some(a.account_id));
    assert!(store.trash_entries().unwrap().is_empty());
}

#[test]
fn a_crash_after_a_torn_plan_is_the_same() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let before = files(&root);
    // The directory exists, and the write of its manifest was cut short: a temp file and no manifest.
    let dir = root.join("trash").join(format!("{}-1.pending", utc_stamp(NOW)));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".manifest.json.tmp-1-0"), b"{\"schema_version\":1,\"kind\":\"fmv-tr").unwrap();
    drop(store);
    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("abandoned an interrupted delete of account 1")), "{:?}", store.open_warnings());
    let mut after = files(&root);
    assert!(after.keys().all(|k| !k.starts_with("trash/")), "{:?}", under(&after, "trash/"));
    after.retain(|k, _| !k.starts_with("trash/"));
    assert_eq!(after, before);
    assert_eq!(store.authenticate(&a.token).unwrap(), a.account_id);
}

#[test]
fn a_crash_after_the_commit_is_finished_when_the_store_opens() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let (a, b) = (make(&store, "Aria", 1), make(&store, "Brom", 2));
    let before = files(&root);
    {
        let _writer = store.writer().unwrap();
        let prepared = store.prepare_delete(a.account_id, NOW).unwrap();
        store.commit_delete(a.account_id).unwrap();
        assert!(store.find_account_by_name("Aria").is_none(), "the commit freed the name");
        let _ = prepared;
        // ...and the process dies before one file has moved.
    }
    drop(store);
    assert!(root.join("accounts/1.json").is_file(), "nothing moved yet");

    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("finished an interrupted delete of account 1")), "{:?}", store.open_warnings());
    assert!(!store.open_warnings().iter().any(|w| w.contains("not referenced by the index")), "{:?}", store.open_warnings());
    let after = files(&root);
    let entry = format!("trash/{}-1", utc_stamp(NOW));
    for rel in ["accounts/1.json", "avatars/1.json", "characters/1.json", "progression/1/Ff7.json", "progression/1/Ff7.character.json"] {
        assert!(!after.contains_key(rel), "{rel}");
        assert_eq!(after.get(&format!("{entry}/{rel}")), before.get(rel), "{rel}");
    }
    assert!(!root.join("trash").join(format!("{}-1.pending", utc_stamp(NOW))).exists(), "the entry is sealed");
    for (rel, bytes) in &before {
        if rel.contains("/1") || rel == "index.log" || rel.ends_with("/1.json") {
            continue;
        }
        assert_eq!(after.get(rel), Some(bytes), "{rel} changed");
    }
    assert!(matches!(store.authenticate(&a.token), Err(PersistError::InvalidToken)));
    assert_eq!(store.authenticate(&b.token).unwrap(), b.account_id);
    let entries = store.trash_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].state, entries[0].display_name.as_str(), entries[0].account_id), (TrashState::Deleted, "Aria", a.account_id));
    // The name can be registered again, and the new account is not Aria's.
    let again = store.register("Aria", [1, 2, 3], "default", None, NOW + 1).unwrap();
    assert!(again.account_id.0 > b.account_id.0, "ids are never reused");
    // A second open has nothing left to settle.
    drop(store);
    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().is_empty(), "{:?}", store.open_warnings());
}

#[test]
fn a_crash_in_the_middle_of_the_moves_is_finished_when_the_store_opens() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let before = files(&root);
    {
        let _writer = store.writer().unwrap();
        let prepared = store.prepare_delete(a.account_id, NOW).unwrap();
        store.commit_delete(a.account_id).unwrap();
        // Two of the five files move, and the process dies.
        let partial = Manifest { files: prepared.manifest.files.iter().take(2).cloned().collect(), ..prepared.manifest.clone() };
        assert!(move_into_entry(&store.root, &prepared.dir, &partial).failed.is_empty());
    }
    drop(store);
    assert!(root.join("progression/1/Ff7.json").is_file(), "a file is still where it was");
    assert!(!root.join("accounts/1.json").exists(), "and one has moved");

    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("finished an interrupted delete of account 1")), "{:?}", store.open_warnings());
    let after = files(&root);
    let entry = format!("trash/{}-1", utc_stamp(NOW));
    for rel in ["accounts/1.json", "avatars/1.json", "characters/1.json", "progression/1/Ff7.json", "progression/1/Ff7.character.json"] {
        assert!(!after.contains_key(rel), "{rel}");
        assert_eq!(after.get(&format!("{entry}/{rel}")), before.get(rel), "{rel}");
    }
    assert!(!root.join("progression/1").exists());
    assert!(store.find_account_by_name("Aria").is_none());
}

#[test]
fn a_failed_commit_leaves_the_account_and_no_plan() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let before = files(&root);
    // The log cannot be appended to: the delete fails, and the plan that was written is removed.
    store.log().poison("test".into());
    let err = store.delete_account(a.account_id, NOW).unwrap_err();
    assert!(matches!(err, PersistError::IndexPoisoned { .. }), "{err}");
    assert_eq!(files(&root), before, "the plan is gone and nothing changed");
    assert_eq!(store.authenticate(&a.token).unwrap(), a.account_id);
}

#[test]
fn deleting_what_is_not_there_is_refused() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    assert!(matches!(store.delete_account(AccountId(99), NOW), Err(PersistError::UnknownAccount(_))));
    store.delete_account(a.account_id, NOW).unwrap();
    assert!(matches!(store.delete_account(a.account_id, NOW), Err(PersistError::UnknownAccount(_))), "a second delete is refused, not repeated");
    assert_eq!(store.trash_entries().unwrap().len(), 1);
}

#[test]
fn a_manifest_that_names_another_accounts_file_is_refused_and_nothing_is_moved() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let (a, b) = (make(&store, "Aria", 1), make(&store, "Brom", 2));
    let before = files(&root);
    {
        let _writer = store.writer().unwrap();
        let prepared = store.prepare_delete(a.account_id, NOW).unwrap();
        store.commit_delete(a.account_id).unwrap();
        // A damaged or hostile manifest that also names Brom's records.
        let mut hostile = prepared.manifest.clone();
        hostile.files.push("accounts/2.json".into());
        hostile.files.push("progression/2/Ff7.json".into());
        write_manifest(&prepared.dir, &hostile).unwrap();
    }
    drop(store);
    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("could not settle the trash entry") && w.contains("not a record of account 1")), "{:?}", store.open_warnings());
    let after = files(&root);
    for (rel, bytes) in &before {
        if rel.starts_with("accounts/2") || rel.starts_with("avatars/2") || rel.starts_with("characters/2") || rel.starts_with("progression/2") {
            assert_eq!(after.get(rel), Some(bytes), "{rel}: Brom's file moved or changed");
        }
    }
    assert_eq!(store.authenticate(&b.token).unwrap(), b.account_id);
}

#[test]
fn a_delete_does_not_follow_a_planted_link() {
    #[cfg(unix)]
    {
        let (_guard, root) = state_dir();
        let store = Store::open(&root).unwrap();
        let a = make(&store, "Aria", 1);
        let outside = _guard.path().join("outside.json");
        fs::write(&outside, b"not a record").unwrap();
        // The character record of Aria is replaced by a link to a file outside the store.
        let record = root.join("characters/1.json");
        fs::remove_file(&record).unwrap();
        std::os::unix::fs::symlink(&outside, &record).unwrap();
        let deleted = store.delete_account(a.account_id, NOW).unwrap();
        assert!(!deleted.files.iter().any(|f| f == "characters/1.json"), "a link is not a record: {:?}", deleted.files);
        assert_eq!(fs::read(&outside).unwrap(), b"not a record", "the target was not touched");
    }
}

// ---- restore ----------------------------------------------------------------------------------------------------

#[test]
fn a_crash_after_a_restore_committed_is_finished_when_the_store_opens() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let deleted = store.delete_account(a.account_id, NOW).unwrap();
    let restored = store.restore_account(&deleted.entry, NOW + 5).unwrap();
    assert!(restored.unfinished.is_empty());
    let new = restored.account_id.0;
    let want = files(&root);
    drop(store);

    // Put it back to where a crash between the commit and the seal leaves it: no progress records yet, and the
    // entry still `.restoring`.
    let entry = root.join("trash").join(&deleted.entry);
    let sealed = root.join("trash").join(format!("{}.restored-{new}", deleted.entry));
    assert!(sealed.is_dir());
    fs::rename(&sealed, root.join("trash").join(format!("{}.restoring-{new}", deleted.entry))).unwrap();
    fs::remove_dir_all(root.join(format!("progression/{new}"))).unwrap();
    assert!(!entry.exists());

    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("finished an interrupted restore")), "{:?}", store.open_warnings());
    assert_eq!(files(&root), want, "the same files as an uninterrupted restore");
    let progress = store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().unwrap();
    assert_eq!((progress.account_id, progress.exp, progress.gil), (restored.account_id, 1234, 55));
    assert_eq!(store.authenticate(&restored.token).unwrap(), restored.account_id);
}

#[test]
fn an_incomplete_committed_restore_blocks_play_until_progress_and_receipts_are_recovered() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let original = make(&store, "Aria", 1);
    let before = store.get_progress_pair(original.account_id, GameId::Ff7).unwrap();
    let mut after = before.clone();
    after.story.blob = vec![1, 39, 0];
    after.character.as_mut().unwrap().gil -= 1;
    store.commit_scene_progress_with_result("flower", b"buy-one", &before, &after).unwrap();
    let deleted = store.delete_account(original.account_id, NOW).unwrap();
    let next = store.writer().unwrap().next_account_id;
    // Fail only after the index commit: allocation cleanup must have finished before the
    // obstruction appears, otherwise this would test an ordinary refused allocation.
    let obstruction = root.join(format!("progression/{next}"));
    FAIL_RESTORE_PROGRESS.with(|flag| flag.set(true));
    let restored = store.restore_account(&deleted.entry, NOW + 5).unwrap();
    assert!(!restored.unfinished.is_empty(), "the admin retains its result and token");
    let account = restored.account_id;
    assert_eq!(account.0, next);
    assert!(matches!(store.authenticate(&restored.token), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.get_progress_pair(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.get_progression(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.get_character_progress(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.put_progression(&Progression::new(account, GameId::Ff7, vec![], NOW)), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.update_progression(account, GameId::Ff7, |_| panic!("no story replacement")), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.put_character_progress(&CharacterProgress::new(account, GameId::Ff7, 1, 7, NOW)), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.update_character_progress(account, GameId::Ff7, |_| panic!("no default replacement")), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.snapshot(root.with_file_name("unsafe-snapshot")), Err(PersistError::TransactionRecoveryRequired)));
    drop(store);

    // A warning-only open here would allow gameplay to fabricate the still-missing records.
    assert!(matches!(Store::open(&root), Err(PersistError::Corrupt { .. })));
    fs::remove_file(obstruction).unwrap();
    let recovered = Store::open(&root).unwrap();
    assert_eq!(recovered.authenticate(&restored.token).unwrap(), account);
    let pair = recovered.get_progress_pair(account, GameId::Ff7).unwrap();
    assert_eq!(pair.story.blob, after.story.blob);
    assert_eq!(pair.character.as_ref().unwrap().gil, 54);
    assert_eq!(pair.character.as_ref().unwrap().inventory, after.character.as_ref().unwrap().inventory);
    let mut expected_story = before.story;
    expected_story.account_id = account;
    assert!(recovered.scene_progress_applied("flower", &expected_story, b"buy-one").unwrap(), "the original receipt survives rekeying and recovery");
}

#[test]
fn an_uncertain_restore_append_preserves_its_records_and_recovery_plan() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let original = make(&store, "Aria", 1);
    let before = store.get_progress_pair(original.account_id, GameId::Ff7).unwrap();
    let mut after = before.clone();
    after.story.blob = vec![1, 39, 0];
    after.character.as_mut().unwrap().gil -= 1;
    store.commit_scene_progress_with_result("flower", b"buy-one", &before, &after).unwrap();
    let deleted = store.delete_account(original.account_id, NOW).unwrap();
    let (next, next_avatar) = {
        let counters = store.writer().unwrap();
        (counters.next_account_id, counters.next_avatar_id)
    };
    // The account entry reaches disk, but the caller cannot know that its append committed.
    // In-memory allocation counters have not advanced; cleanup or another allocation would
    // destroy the records which replay is about to make live.
    FAIL_RESTORE_APPEND.with(|flag| flag.set(true));
    assert!(store.restore_account(&deleted.entry, NOW + 5).is_err());
    let restoring = root.join("trash").join(format!("{}.restoring-{next}", deleted.entry));
    let plan = read_manifest(&restoring).unwrap().restore.expect("the uncertain commit keeps its recovery plan");
    assert_eq!((plan.account_id, plan.avatar_id), (next, Some(next_avatar)));
    assert!(!root.join("trash").join(&deleted.entry).exists());
    let account_path = root.join(format!("accounts/{next}.json"));
    let avatar_path = root.join(format!("avatars/{next_avatar}.json"));
    let character_path = root.join(format!("characters/{next}.json"));
    let preserved: Vec<_> = [&account_path, &avatar_path, &character_path].into_iter()
        .map(|path| (path.clone(), fs::read(path).expect("possibly committed records must not be removed"))).collect();
    let frozen = files(&root);
    assert!(matches!(store.create_account("Brom", NOW + 6), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 6), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.get_progress_pair(AccountId(next), GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.snapshot(root.with_file_name("uncertain-snapshot")), Err(PersistError::TransactionRecoveryRequired)));
    assert_eq!(files(&root), frozen, "refused operations must preserve all recovery evidence");
    drop(store);

    let recovered = Store::open(&root).unwrap();
    let account = AccountId(next);
    assert_eq!(recovered.find_account_by_name("Aria"), Some(account));
    assert!(recovered.unclaimed_accounts().is_empty(), "the replayed restore is claimed");
    for (path, bytes) in preserved { assert_eq!(fs::read(path).unwrap(), bytes); }
    assert!(!restoring.exists());
    assert!(root.join("trash").join(format!("{}.restored-{next}", deleted.entry)).is_dir());
    let pair = recovered.get_progress_pair(account, GameId::Ff7).unwrap();
    assert_eq!(pair.story.blob, after.story.blob);
    assert_eq!(pair.character.as_ref().unwrap().gil, 54);
    assert_eq!(pair.character.as_ref().unwrap().inventory, after.character.as_ref().unwrap().inventory);
    let mut expected_story = before.story;
    expected_story.account_id = account;
    assert!(recovered.scene_progress_applied("flower", &expected_story, b"buy-one").unwrap());
    let (other, _) = recovered.create_account("Brom", NOW + 7).unwrap();
    assert!(other.0 > next, "the replayed committed id cannot be reused");
}

#[test]
fn a_restore_rename_sync_failure_fences_allocation_until_rollback_finishes() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let original = make(&store, "Aria", 1);
    let deleted = store.delete_account(original.account_id, NOW).unwrap();
    let next = store.writer().unwrap().next_account_id;
    let deleted_path = root.join("trash").join(&deleted.entry);
    let restoring = root.join("trash").join(format!("{}.restoring-{next}", deleted.entry));
    // The directory rename happened; its durability acknowledgement failed before any
    // account record or commit. The still-reserved id must not be reused in this process.
    FAIL_RESTORE_RENAME.with(|flag| flag.set(true));
    assert!(store.restore_account(&deleted.entry, NOW + 5).is_err());
    assert_eq!(read_manifest(&restoring).unwrap().restore.unwrap().account_id, next);
    assert!(!deleted_path.exists());
    let frozen = files(&root);
    assert!(matches!(store.create_account("Brom", NOW + 6), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 6), Err(PersistError::TransactionRecoveryRequired)));
    assert!(matches!(store.snapshot(root.with_file_name("rename-snapshot")), Err(PersistError::TransactionRecoveryRequired)));
    assert_eq!(files(&root), frozen);
    drop(store);

    // Recovery must not admit another allocation while rollback still cannot return the
    // entry to its deleted name, or a later recovery could erase the new account's files.
    fs::create_dir(&deleted_path).unwrap();
    fs::write(deleted_path.join("obstruction"), b"not empty").unwrap();
    assert!(matches!(Store::open(&root), Err(PersistError::Corrupt { .. })));
    assert_eq!(read_manifest(&restoring).unwrap().restore.unwrap().account_id, next,
        "a failed rollback rename must keep its matching plan for the next recovery");
    fs::remove_dir_all(&deleted_path).unwrap();
    let recovered = Store::open(&root).unwrap();
    assert!(!restoring.exists());
    assert!(read_manifest(&deleted_path).unwrap().restore.is_none());
    assert!(recovered.find_account_by_name("Aria").is_none());
    let replacement = recovered.register("Brom", [4, 5, 6], "default", None, NOW + 7).unwrap();
    assert_eq!(replacement.account_id, AccountId(next), "the settled, uncommitted id is safe to allocate again");
    let replacement_files = files(&root);
    drop(recovered);
    let reopened = Store::open(&root).unwrap();
    assert_eq!(reopened.authenticate(&replacement.token).unwrap(), replacement.account_id);
    assert_eq!(reopened.avatar_for_account(replacement.account_id).unwrap(), Some(replacement.avatar));
    // Authentication appends only the claim; all identity and progress files survive.
    let mut after = files(&root);
    let mut before = replacement_files;
    after.remove("index.log");
    before.remove("index.log");
    assert_eq!(after, before);
}

#[test]
fn a_crash_before_a_restore_committed_puts_the_entry_back() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let deleted = store.delete_account(a.account_id, NOW).unwrap();
    let trash_before = files(&root);
    let next = {
        let counters = store.writer().unwrap();
        (counters.next_account_id, counters.next_avatar_id)
    };
    drop(store);

    // What a crash after the intent and some of the records, before the commit, leaves.
    let dir = root.join("trash").join(&deleted.entry);
    let mut manifest = read_manifest(&dir).unwrap();
    manifest.restore = Some(RestorePlan { account_id: next.0, avatar_id: Some(next.1), at: NOW + 5 });
    write_manifest(&dir, &manifest).unwrap();
    fs::rename(&dir, root.join("trash").join(format!("{}.restoring-{}", deleted.entry, next.0))).unwrap();
    fs::write(root.join(format!("accounts/{}.json", next.0)), b"{}").unwrap();
    fs::write(root.join(format!("avatars/{}.json", next.1)), b"{}").unwrap();

    let store = Store::open(&root).unwrap();
    assert!(store.open_warnings().iter().any(|w| w.contains("undid an interrupted restore")), "{:?}", store.open_warnings());
    assert_eq!(files(&root), trash_before, "the stray records are gone and the entry is as it was after the delete");
    let entries = store.trash_entries().unwrap();
    assert_eq!((entries.len(), entries[0].state), (1, TrashState::Deleted));
    // And it restores.
    let restored = store.restore_account(&deleted.entry, NOW + 9).unwrap();
    assert_eq!(restored.account_id.0, next.0, "the id nothing committed is the next one");
    assert!(restored.unfinished.is_empty());
}

#[test]
fn a_failed_restore_puts_the_records_back_and_retains_the_selected_format_fences() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let deleted = store.delete_account(a.account_id, NOW).unwrap();
    let mut before = files(&root);
    let (next, next_avatar) = {
        let counters = store.writer().unwrap();
        (counters.next_account_id, counters.next_avatar_id)
    };
    // The new account's record cannot be written: a directory sits where the file goes.
    fs::create_dir(root.join(format!("accounts/{next}.json"))).unwrap();
    let err = store.restore_account(&deleted.entry, NOW + 5).unwrap_err();
    assert!(matches!(err, PersistError::Io { .. }), "{err}");
    // Explicit restore selected the durable story format before attempting the first
    // account write. Rollback must keep that exclusion fence and its detached manifest
    // marker, while restoring every source record and leaving no new account visible.
    let after = files(&root);
    let mut records_after = after.clone();
    before.remove(crate::index::INDEX_LOG_FILE);
    records_after.remove(crate::index::INDEX_LOG_FILE);
    let manifest_path = format!("{TRASH_DIR}/{}/{MANIFEST_FILE}", deleted.entry);
    let mut expected_manifest: serde_json::Value = serde_json::from_slice(&before.remove(&manifest_path).unwrap()).unwrap();
    assert_eq!(expected_manifest["schema_version"], LEGACY_MANIFEST_VERSION);
    expected_manifest["schema_version"] = MANIFEST_VERSION.into();
    let actual_manifest: serde_json::Value = serde_json::from_slice(&records_after.remove(&manifest_path).unwrap()).unwrap();
    assert_eq!(actual_manifest, expected_manifest, "only the compatibility marker changes; no restore plan remains");
    assert_eq!(records_after, before, "every source record is back unchanged and no new record was left");
    assert_eq!(store.index().format_version(), DURABLE_STORY_INDEX_VERSION);
    {
        let counters = store.writer().unwrap();
        assert_eq!((counters.next_account_id, counters.next_avatar_id), (next, next_avatar));
    }
    assert!(store.find_account_by_name("Aria").is_none(), "nothing committed");
    let entries = store.trash_entries().unwrap();
    assert_eq!((entries.len(), entries[0].state), (1, TrashState::Deleted));
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 6), Err(PersistError::Io { .. })));
    assert_eq!(files(&root), after, "another proven rollback makes no further durable changes");
    fs::remove_dir(root.join(format!("accounts/{next}.json"))).unwrap();
    drop(store);
    let store = Store::open(&root).unwrap();
    assert_eq!(store.index().format_version(), DURABLE_STORY_INDEX_VERSION);
    assert!(store.find_account_by_name("Aria").is_none());
    assert_eq!(files(&root), after, "reopen preserves the rolled-back records and both format fences");
    // Once the obstacle is gone, the original allocation can safely restore the account.
    let restored = store.restore_account(&deleted.entry, NOW + 7).unwrap();
    assert_eq!((restored.account_id.0, restored.avatar_id.map(|id| id.0)), (next, Some(next_avatar)));
    assert!(restored.unfinished.is_empty());
}

#[test]
fn the_restored_account_commits_claimed_and_replays_so() {
    let (_guard, root) = state_dir();
    let store = Store::open(&root).unwrap();
    let a = make(&store, "Aria", 1);
    let deleted = store.delete_account(a.account_id, NOW).unwrap();
    let restored = store.restore_account(&deleted.entry, NOW + 5).unwrap();
    assert!(store.unclaimed_accounts().is_empty(), "a restored account is nobody's orphan");
    drop(store);
    let store = Store::open(&root).unwrap();
    assert!(store.unclaimed_accounts().is_empty(), "and still is after a restart");
    assert_eq!(store.find_account_by_name("Aria"), Some(restored.account_id));
    assert!(store.open_warnings().is_empty(), "{:?}", store.open_warnings());
}
