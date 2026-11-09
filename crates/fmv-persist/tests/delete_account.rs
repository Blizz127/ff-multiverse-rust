//! Deleting a character: its records go to the trash whole, its name is free, nothing of any other account is
//! touched, and the trash brings it back. Every test runs on a temp state directory.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    CharacterProgress, GameCharacter, NewPlayerCharacter, PersistError, Progression, Registered, Store, StoreOptions, TrashState,
};

/// An account with everything a played character has: avatar, player-character record, character progress with
/// items and a limit gauge, and a story blob.
fn played(store: &Store, name: &str, slot: u8) -> Registered {
    let entry = GameCharacter {
        binding: Some("ff7/body/sturdy".into()),
        slot: Some(slot),
        progress: GameCharacter::pointers(GameId::Ff7, Some(slot)),
    };
    let new = NewPlayerCharacter::for_game("sturdy", GameId::Ff7, entry);
    let reg = store
        .register_with_character(name, [10, 20, 30], "default", Some((GameId::Ff7, "ff7/body/sturdy")), Some(new), NOW)
        .unwrap();
    let mut progress = CharacterProgress::new(reg.account_id, GameId::Ff7, slot, 7, NOW + 1);
    progress.exp = 1234;
    progress.gil = 55;
    progress.limit = 99;
    progress.inventory = Some(vec![InventoryEntry::new(3, 2), InventoryEntry::new(9, 1)]);
    store.put_character_progress(&progress).unwrap();
    store.put_progression(&Progression::new(reg.account_id, GameId::Ff7, vec![1, 6, 0], NOW + 2)).unwrap();
    store.authenticate(&reg.token).unwrap();
    reg
}

/// The files of one account, by relative path, with their bytes (account, avatar, character, progression).
fn files_of(root: &Path, account: u64, avatar: u64) -> BTreeMap<String, Vec<u8>> {
    file_map(root)
        .into_iter()
        .filter(|(rel, _)| {
            rel == &format!("accounts/{account}.json")
                || rel == &format!("avatars/{avatar}.json")
                || rel == &format!("characters/{account}.json")
                || rel.starts_with(&format!("progression/{account}/"))
        })
        .collect()
}

#[test]
fn a_deleted_character_frees_its_name_and_voids_its_token_and_nobody_else_changes() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (aria, brom) = (played(&store, "Aria", 1), played(&store, "Brom", 2));
    let brom_before = files_of(&root, brom.account_id.0, brom.avatar.avatar_id.0);
    assert_eq!(brom_before.len(), 5, "{:?}", brom_before.keys());
    let total_before = file_map(&root);

    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    assert_eq!((deleted.display_name.as_str(), deleted.unfinished.len()), ("Aria", 0));

    assert_eq!(store.find_account_by_name("aria"), None, "the name is free");
    assert!(matches!(store.authenticate(&aria.token), Err(PersistError::InvalidToken)), "the token is void");
    assert_eq!(store.account_info(aria.account_id).unwrap(), None);
    assert_eq!(store.get_avatar(aria.avatar.avatar_id).unwrap(), None);
    assert_eq!(store.avatar_for_account(aria.account_id).unwrap(), None);
    assert!(matches!(store.get_progression(aria.account_id, GameId::Ff7), Err(PersistError::UnknownAccount(_))));
    assert!(matches!(store.get_character_progress(aria.account_id, GameId::Ff7), Err(PersistError::UnknownAccount(_))));
    assert!(matches!(store.get_player_character(aria.account_id), Err(PersistError::UnknownAccount(_))));
    assert_eq!(store.account_count(), 1);
    assert!(store.unclaimed_accounts().is_empty());

    // Another account's files are byte for byte what they were, and it plays on.
    assert_eq!(files_of(&root, brom.account_id.0, brom.avatar.avatar_id.0), brom_before);
    assert_eq!(store.authenticate(&brom.token).unwrap(), brom.account_id);
    let progress = store.get_character_progress(brom.account_id, GameId::Ff7).unwrap().unwrap();
    assert_eq!((progress.exp, progress.gil, progress.limit), (1234, 55, 99));
    // Of the files that were there, only Aria's moved and only the index grew.
    let after = file_map(&root);
    for (rel, bytes) in &total_before {
        let hers = rel.contains("/1.json") || rel.starts_with("progression/1/");
        if hers {
            assert!(!after.contains_key(rel), "{rel}");
        } else if rel != "index.log" {
            assert_eq!(after.get(rel), Some(bytes), "{rel}");
        }
    }

    // The name can be registered again by anyone, at once, as a fresh character.
    let again = store.register("Aria", [200, 0, 0], "default", None, NOW + 11).unwrap();
    assert!(again.account_id.0 > brom.account_id.0, "ids are never reused");
    assert!(again.avatar.avatar_id.0 > brom.avatar.avatar_id.0);
    assert_ne!(again.token, aria.token);
    assert!(matches!(store.authenticate(&aria.token), Err(PersistError::InvalidToken)), "the old token stays void");
    assert_eq!(store.authenticate(&again.token).unwrap(), again.account_id);
    assert!(store.get_character_progress(again.account_id, GameId::Ff7).unwrap().is_none(), "nothing of the old Aria came with the name");
    assert!(store.get_progression(again.account_id, GameId::Ff7).unwrap().blob.is_empty());
}

#[test]
fn a_delete_survives_a_restart_a_compaction_and_another_restart() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let zed = played(&store, "Zed", 1);
    let brom = played(&store, "Brom", 2);
    assert!(fs::read_to_string(index_log(&root)).unwrap().contains("Zed"));
    store.delete_account(zed.account_id, NOW + 10).unwrap();
    drop(store);

    let store = open(&root);
    assert!(store.open_warnings().is_empty(), "{:?}", store.open_warnings());
    assert_eq!(store.find_account_by_name("Zed"), None, "nothing comes back after a restart");
    assert!(matches!(store.authenticate(&zed.token), Err(PersistError::InvalidToken)));
    assert_eq!(store.account_count(), 1);
    store.compact_index().unwrap();
    assert!(!fs::read_to_string(index_log(&root)).unwrap().contains("Zed"), "the compacted index has no trace of the account");
    drop(store);

    let store = open(&root);
    assert!(store.open_warnings().is_empty(), "{:?}", store.open_warnings());
    assert_eq!(store.find_account_by_name("Zed"), None);
    assert_eq!(store.authenticate(&brom.token).unwrap(), brom.account_id);
    let next = store.register("Cid", [1, 1, 1], "default", None, NOW + 20).unwrap();
    assert!(next.account_id.0 > brom.account_id.0, "the id of the deleted account is retired, not handed out again");
    assert!(next.avatar.avatar_id.0 > brom.avatar.avatar_id.0);
    assert_eq!(store.trash_entries().unwrap().len(), 1, "the trash is still there");
}

#[test]
fn the_trash_holds_every_file_exactly_as_it_was() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let aria = played(&store, "Aria", 1);
    let before = files_of(&root, aria.account_id.0, aria.avatar.avatar_id.0);
    let deleted = store.delete_account(aria.account_id, 1_794_243_545).unwrap();
    assert_eq!(deleted.entry, "20261109T165905Z-1");
    let entry = root.join("trash").join(&deleted.entry);
    for (rel, bytes) in &before {
        assert_eq!(&fs::read(entry.join(rel)).unwrap(), bytes, "{rel}");
    }
    let manifest = read_json(&entry.join("manifest.json"));
    assert_eq!(manifest["account_id"], 1);
    assert_eq!(manifest["display_name"], "Aria");
    assert_eq!(manifest["deleted_at"], 1_794_243_545u64);
    assert_eq!(manifest["deleted_at_utc"], "2026-11-09T16:59:05Z");
    assert_eq!(manifest["kind"], "fmv-trash-entry");
    assert_eq!(manifest["files"].as_array().unwrap().len(), before.len());
    assert!(has_temp_files(&root).is_empty(), "{:?}", has_temp_files(&root));
    // The directories are owner-only like the rest of the state directory (they hold token hashes).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [root.join("trash"), entry.clone(), entry.join("accounts"), entry.join("progression")] {
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o077, 0, "{}", dir.display());
        }
        assert_eq!(fs::metadata(entry.join("accounts/1.json")).unwrap().permissions().mode() & 0o077, 0);
    }
}

#[test]
fn ids_that_share_a_prefix_never_get_mixed_up() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    // Accounts 1 to 11: the files of 1 are a prefix of the names of 10 and 11 (`1`, `10`, `11`).
    let all: Vec<Registered> = (1..=11u8).map(|n| played(&store, &format!("Hero{n}"), n % 4)).collect();
    let others: Vec<(u64, BTreeMap<String, Vec<u8>>)> =
        all.iter().skip(1).map(|r| (r.account_id.0, files_of(&root, r.account_id.0, r.avatar.avatar_id.0))).collect();
    store.delete_account(all[0].account_id, NOW + 10).unwrap();
    for (id, before) in &others {
        let avatar = all.iter().find(|r| r.account_id.0 == *id).unwrap().avatar.avatar_id.0;
        assert_eq!(&files_of(&root, *id, avatar), before, "account {id}");
    }
    // And deleting 11 leaves 1's neighbours alone too.
    store.delete_account(all[10].account_id, NOW + 11).unwrap();
    assert_eq!(store.account_count(), 9);
    for r in all.iter().skip(1).take(9) {
        assert_eq!(store.authenticate(&r.token).unwrap(), r.account_id);
        assert!(store.get_character_progress(r.account_id, GameId::Ff7).unwrap().is_some());
    }
}

#[test]
fn an_account_with_no_progress_and_no_character_record_is_deleted_too() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    // An account made by other tooling: no avatar, no character, no progress.
    let (bare, token) = store.create_account("Bare", NOW).unwrap();
    let deleted = store.delete_account(bare, NOW + 1).unwrap();
    assert_eq!(deleted.files, ["accounts/1.json"]);
    assert!(matches!(store.authenticate(&token), Err(PersistError::InvalidToken)));
    assert!(store.find_account_by_name("Bare").is_none());
    // It restores to an account with no avatar (the first login makes one, as for any account without).
    let restored = store.restore_account(&deleted.entry, NOW + 2).unwrap();
    assert_eq!(restored.avatar_id, None);
    assert_eq!(store.authenticate(&restored.token).unwrap(), restored.account_id);
    assert_eq!(store.avatar_for_account(restored.account_id).unwrap(), None);
}

#[test]
fn two_deletes_of_one_account_at_once_delete_it_once() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let aria = played(&store, "Aria", 1);
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let (store, barrier) = (Arc::clone(&store), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                store.delete_account(aria.account_id, NOW + 10 + n)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
    assert!(results.iter().filter_map(|r| r.as_ref().err()).all(|e| matches!(e, PersistError::UnknownAccount(_))), "{results:?}");
    assert_eq!(store.trash_entries().unwrap().len(), 1);
}

#[test]
fn logins_are_not_held_up_by_a_delete_and_a_delete_does_not_break_them() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let aria = played(&store, "Aria", 1);
    let brom = played(&store, "Brom", 2);
    let stop = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let (store, stop, started, token) = (Arc::clone(&store), Arc::clone(&stop), Arc::clone(&started), brom.token.clone());
            std::thread::spawn(move || {
                let mut logins = 0u32;
                loop {
                    assert_eq!(store.authenticate(&token).unwrap(), brom.account_id);
                    assert!(store.get_player_character(brom.account_id).unwrap().is_some());
                    assert!(store.get_character_progress(brom.account_id, GameId::Ff7).unwrap().is_some());
                    logins += 1;
                    if logins == 1 {
                        started.fetch_add(1, Ordering::SeqCst);
                    }
                    if stop.load(Ordering::Relaxed) {
                        return logins;
                    }
                }
            })
        })
        .collect();
    // Every reader has logged in once, and keeps going while the delete and the restore run.
    while started.load(Ordering::SeqCst) < 3 {
        std::thread::yield_now();
    }
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    store.restore_account(&deleted.entry, NOW + 11).unwrap();
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        assert!(reader.join().unwrap() >= 1);
    }
}

// ---- restore ----------------------------------------------------------------------------------------------------

#[test]
fn a_restored_character_is_the_same_character_under_new_ids_and_a_new_token() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let aria = played(&store, "Aria", 1);
    let brom = played(&store, "Brom", 2);
    let character = store.get_player_character(aria.account_id).unwrap().unwrap();
    let progress = store.get_character_progress(aria.account_id, GameId::Ff7).unwrap().unwrap();
    let story = store.get_progression(aria.account_id, GameId::Ff7).unwrap();
    let avatar = store.get_avatar(aria.avatar.avatar_id).unwrap().unwrap();
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    let brom_before = files_of(&root, brom.account_id.0, brom.avatar.avatar_id.0);

    let restored = store.restore_account(&deleted.entry, NOW + 20).unwrap();
    assert!(restored.unfinished.is_empty());
    assert_eq!(restored.display_name, "Aria");
    assert!(restored.account_id.0 > brom.account_id.0, "a NEW id: the old one is retired");
    let new_avatar = restored.avatar_id.unwrap();
    assert!(new_avatar.0 > brom.avatar.avatar_id.0);
    assert_eq!(restored.entry, format!("{}.restored-{}", deleted.entry, restored.account_id.0));
    assert!(!format!("{restored:?}").contains(&restored.token), "Debug does not show the token");

    // The name is Aria's again, the new token works and the old one still does not.
    assert_eq!(store.find_account_by_name("aria"), Some(restored.account_id));
    assert_eq!(store.authenticate(&restored.token).unwrap(), restored.account_id);
    assert!(matches!(store.authenticate(&aria.token), Err(PersistError::InvalidToken)));
    assert!(matches!(store.register("Aria", [1, 2, 3], "default", None, NOW + 21), Err(PersistError::NameTaken(_))));
    assert!(store.unclaimed_accounts().is_empty(), "a restored account is nobody's orphan");

    // Every record is the one that was deleted, re-keyed and nothing else.
    let mut want = character.clone();
    want.account_id = restored.account_id;
    assert_eq!(store.get_player_character(restored.account_id).unwrap().unwrap(), want);
    let mut want = progress.clone();
    want.account_id = restored.account_id;
    assert_eq!(store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().unwrap(), want);
    let got = store.get_progression(restored.account_id, GameId::Ff7).unwrap();
    assert_eq!((got.account_id, got.blob, got.updated_at), (restored.account_id, story.blob, story.updated_at));
    let got = store.avatar_for_account(restored.account_id).unwrap().unwrap();
    let mut want = avatar.clone();
    (want.account_id, want.avatar_id) = (restored.account_id, new_avatar);
    assert_eq!(got, want);
    assert_eq!(store.get_avatar(new_avatar).unwrap().unwrap(), want);
    // The pointers of the character point at the same slot (the progress record is found through them).
    let linked = store.follow_character_progress(&store.get_player_character(restored.account_id).unwrap().unwrap(), GameId::Ff7).unwrap().unwrap();
    assert!(linked.in_sync());
    assert_eq!(files_of(&root, brom.account_id.0, brom.avatar.avatar_id.0), brom_before, "Brom is untouched by it");

    // The entry says what became of it, and cannot be restored again.
    let entries = store.trash_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, TrashState::Restored { as_account: restored.account_id });
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 30), Err(PersistError::NoSuchTrashEntry(_))));
    assert!(matches!(store.restore_account(&restored.entry, NOW + 30), Err(PersistError::NoSuchTrashEntry(_))));

    // All of it survives a restart, and the restored character can be deleted and restored again.
    drop(store);
    let store = open(&root);
    assert!(store.open_warnings().is_empty(), "{:?}", store.open_warnings());
    assert_eq!(store.authenticate(&restored.token).unwrap(), restored.account_id);
    let second = store.delete_account(restored.account_id, NOW + 40).unwrap();
    let third = store.restore_account(&second.entry, NOW + 41).unwrap();
    assert!(third.account_id.0 > restored.account_id.0);
    assert_eq!(store.trash_entries().unwrap().len(), 2);
}

#[test]
fn a_restore_is_refused_when_the_name_is_taken_and_changes_nothing() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let aria = played(&store, "Aria", 1);
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    let newer = store.register("ARIA", [9, 9, 9], "default", None, NOW + 11).unwrap();
    let before = file_map(&root);
    let err = store.restore_account(&deleted.entry, NOW + 12).unwrap_err();
    assert!(matches!(err, PersistError::NameTaken(_)), "{err}");
    assert_eq!(file_map(&root), before, "nothing was written, and the entry did not move");
    assert_eq!(store.authenticate(&newer.token).unwrap(), newer.account_id);
    // Once that name is free again the old character can come back.
    store.delete_account(newer.account_id, NOW + 13).unwrap();
    let restored = store.restore_account(&deleted.entry, NOW + 14).unwrap();
    assert_eq!(store.find_account_by_name("aria"), Some(restored.account_id));
    assert!(store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().is_some(), "with its progress, not the newer one's");
}

#[test]
fn a_restore_is_refused_at_the_account_limit_and_changes_nothing() {
    let (_guard, root) = fresh_root();
    let store = Store::open_with(&root, StoreOptions::default().with_max_accounts(1)).unwrap();
    let aria = played(&store, "Aria", 1);
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    store.register("Brom", [1, 2, 3], "default", None, NOW + 11).unwrap();
    let before = file_map(&root);
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 12), Err(PersistError::AccountLimitReached { max: 1 })));
    assert_eq!(file_map(&root), before);
}

#[test]
fn a_restore_with_a_damaged_record_is_refused_before_anything_is_written() {
    let (_guard, root) = fresh_root();
    let store = Store::open(&root).unwrap();
    let aria = played(&store, "Aria", 1);
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    let entry = root.join("trash").join(&deleted.entry);
    for damaged in ["characters/1.json", "progression/1/Ff7.character.json", "progression/1/Ff7.json", "avatars/1.json", "accounts/1.json"] {
        let path = entry.join(damaged);
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"{ not json").unwrap();
        let before = file_map(&root);
        let err = store.restore_account(&deleted.entry, NOW + 12).unwrap_err();
        assert!(matches!(err, PersistError::Corrupt { .. }), "{damaged}: {err}");
        assert_eq!(file_map(&root), before, "{damaged}: nothing was written, and the entry did not move");
        assert_eq!(store.account_count(), 0);
        fs::write(&path, original).unwrap();
    }
    // A record filed under another account is as bad as a damaged one.
    let path = entry.join("progression/1/Ff7.character.json");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, original.replace("\"account_id\": 1", "\"account_id\": 2")).unwrap();
    assert!(matches!(store.restore_account(&deleted.entry, NOW + 13), Err(PersistError::Corrupt { .. })));
    fs::write(&path, original).unwrap();
    assert!(store.restore_account(&deleted.entry, NOW + 14).is_ok(), "and the entry is whole again");
}

#[test]
fn names_that_are_not_restorable_entries_are_refused() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let aria = played(&store, "Aria", 1);
    let deleted = store.delete_account(aria.account_id, NOW + 10).unwrap();
    for name in [
        "",
        "nope",
        "..",
        "../trash",
        "../accounts",
        "accounts",
        "20261109T165905Z-1",
        &format!("{}/", deleted.entry),
        &format!("{}.pending", deleted.entry),
        &format!("{}.restoring-5", deleted.entry),
        &format!("{}.restored-5", deleted.entry),
        &format!("/{}", deleted.entry),
    ] {
        assert!(matches!(store.restore_account(name, NOW + 11), Err(PersistError::NoSuchTrashEntry(_))), "{name:?}");
    }
    assert_eq!(store.account_count(), 0);
    assert!(store.restore_account(&deleted.entry, NOW + 12).is_ok());
}

#[test]
fn the_trash_lists_oldest_first_with_who_and_when() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (aria, brom, cid) = (played(&store, "Aria", 1), played(&store, "Brom", 2), played(&store, "Cid", 3));
    assert!(store.trash_entries().unwrap().is_empty());
    store.delete_account(brom.account_id, 1_700_000_100).unwrap();
    store.delete_account(aria.account_id, 1_700_000_050).unwrap();
    store.delete_account(cid.account_id, 1_700_000_200).unwrap();
    let entries = store.trash_entries().unwrap();
    let summary: Vec<(&str, u64, TrashState)> = entries.iter().map(|e| (e.display_name.as_str(), e.deleted_at, e.state)).collect();
    assert_eq!(
        summary,
        [("Aria", 1_700_000_050, TrashState::Deleted), ("Brom", 1_700_000_100, TrashState::Deleted), ("Cid", 1_700_000_200, TrashState::Deleted)]
    );
    assert!(entries.iter().all(|e| e.files == 5 && e.problem.is_none()));
    assert_eq!(entries.iter().map(|e| e.account_id).collect::<Vec<AccountId>>(), [aria.account_id, brom.account_id, cid.account_id]);
    // Something that is not an entry is not listed, and an entry whose manifest is damaged says so.
    fs::create_dir(root.join("trash/not-an-entry")).unwrap();
    fs::write(root.join("trash").join(&entries[0].name).join("manifest.json"), b"{").unwrap();
    let listed = store.trash_entries().unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed[0].problem.is_some() && listed[1].problem.is_none(), "{:?}", listed[0]);
}

#[test]
fn a_state_directory_that_never_had_a_delete_has_no_trash() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    played(&store, "Aria", 1);
    drop(store);
    let before = file_map(&root);
    let store = open(&root);
    assert!(!root.join("trash").exists(), "the trash is made by the first delete, not by opening a store");
    assert!(store.trash_entries().unwrap().is_empty());
    drop(store);
    assert_eq!(file_map(&root), before, "opening a store changes no file");
}

#[test]
fn a_snapshot_is_a_valid_state_directory_with_or_without_a_trash() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let aria = played(&store, "Aria", 1);
    played(&store, "Brom", 2);
    store.delete_account(aria.account_id, NOW + 10).unwrap();
    let dest = _guard.path().join("snapshot");
    store.snapshot(&dest).unwrap();
    drop(store);
    let copy = open(&dest);
    assert_eq!(copy.account_count(), 1);
    assert_eq!(copy.find_account_by_name("Aria"), None, "the snapshot has the index as it was: Aria is deleted");
    assert!(copy.open_warnings().is_empty(), "{:?}", copy.open_warnings());
}
