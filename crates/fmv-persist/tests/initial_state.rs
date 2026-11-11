//! Registration commits optional character and story seeds with one account index entry.
//! The story bytes are synthetic, opaque to persistence, and every store is temporary.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    CharacterProgress, GameCharacter, NewCharacterProgress, NewPlayerCharacter, NewProgression,
    PersistError, Progression, Registered, SceneCommit, Store, StoreOptions, MAX_BLOB_BYTES,
};
use serde_json::json;

const GAME: GameId = GameId::Fixture;

fn story_bytes() -> Vec<u8> {
    (0..5_117usize).map(|index| ((index * 37 + index / 256) % 256) as u8).collect()
}

fn new_story() -> NewProgression {
    NewProgression { game: GAME, blob: story_bytes() }
}

fn new_progress() -> NewCharacterProgress {
    NewCharacterProgress {
        exp: 1_234,
        ap: 9,
        gil: 240,
        limit: 7,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(4, 1)]),
        ..NewCharacterProgress::new(GAME, 2, 6)
    }
}

fn expected_progress(account: AccountId) -> CharacterProgress {
    CharacterProgress {
        exp: 1_234,
        ap: 9,
        gil: 240,
        limit: 7,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(4, 1)]),
        ..CharacterProgress::new(account, GAME, 2, 6, NOW)
    }
}

fn character() -> NewPlayerCharacter {
    NewPlayerCharacter::for_game("generic", GAME, GameCharacter {
        binding: None,
        slot: Some(2),
        progress: GameCharacter::pointers(GAME, Some(2)),
    })
}

fn register(store: &Store, name: &str, with_progress: bool, with_story: bool) -> (Registered, Option<CharacterProgress>, Option<Progression>) {
    store.register_with_character_and_initial_state(
        name, [1, 2, 3], "default", None, Some(character()),
        with_progress.then(new_progress), with_story.then(new_story), NOW,
    ).unwrap()
}

fn character_progress_file(root: &Path, account: AccountId) -> PathBuf {
    root.join("progression").join(account.0.to_string()).join("Fixture.character.json")
}

fn story_file(root: &Path, account: AccountId) -> PathBuf {
    progression_file(root, account, "Fixture")
}

#[test]
fn all_four_seed_combinations_commit_one_index_entry_and_return_the_exact_records_after_reopen() {
    for (with_progress, with_story) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let before = store.account_count();
        let (registered, progress, story) = register(&store, "Fresh", with_progress, with_story);
        let account = registered.account_id;
        let lines = log_lines(&root);
        let commits = lines.iter().filter(|line| line["op"] == "account").collect::<Vec<_>>();
        // An explicit story seed may first compact the index to the new format, adding
        // a header and allocation high-water mark. Only the account entry commits seeds.
        assert_eq!(commits.len(), 1, "one account entry commits both seeds");
        assert_eq!(commits[0]["id"], account.0);
        assert_eq!(commits[0]["avatar"], registered.avatar.avatar_id.0);
        assert_eq!(lines.last(), Some(commits[0]), "the account becomes visible after format selection");
        assert_eq!(store.account_count(), before + 1);
        assert_eq!(lines[0]["schema_version"], if with_story { 4 } else { 2 }, "an explicit story seed activates the index format fence");
        assert_eq!(read_json(&root.join("index.json"))["schema_version"], 2, "the legacy marker remains unchanged");
        assert_eq!(progress, with_progress.then(|| expected_progress(account)));
        assert_eq!(story, with_story.then(|| Progression::new(account, GAME, story_bytes(), NOW)));
        assert_eq!(character_progress_file(&root, account).exists(), with_progress);
        assert_eq!(story_file(&root, account).exists(), with_story);
        let metadata = read_json(&account_file(&root, account));
        for (field, present, path) in [
            ("registration_progress", with_progress, character_progress_file(&root, account)),
            ("registration_story", with_story, story_file(&root, account)),
        ] {
            if present {
                let bytes = fs::read(path).unwrap();
                assert_eq!(metadata[field]["game"], json!(GAME));
                assert_eq!(metadata[field]["digest"], json!(*blake3::hash(&bytes).as_bytes()), "the fingerprint covers exactly the written bytes");
            } else { assert!(metadata.get(field).is_none(), "absent seeds add no account metadata"); }
        }
        let files = file_map(&root);
        drop(store);

        let store = open(&root);
        assert_eq!(file_map(&root), files, "opening never replaces seeded records with defaults");
        assert_eq!(store.get_character_progress(account, GAME).unwrap(), progress);
        assert_eq!(store.get_progression(account, GAME).unwrap(), story.unwrap_or_else(|| Progression::empty(account, GAME)));
        assert_eq!(store.get_player_character(account).unwrap(), registered.character);
        assert_eq!(store.authenticate(&registered.token).unwrap(), account);
    }
}

#[test]
fn untouched_seed_combinations_can_be_released_after_restart_and_the_name_reused() {
    for (with_progress, with_story) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let (registered, _, _) = register(&store, "Lost token", with_progress, with_story);
        drop(store);
        let store = open(&root);
        assert!(store.release_if_unclaimed(registered.account_id).unwrap());
        assert!(!character_progress_file(&root, registered.account_id).exists());
        assert!(!story_file(&root, registered.account_id).exists());
        assert!(store.account_info(registered.account_id).unwrap().is_none());
        assert!(matches!(store.authenticate(&registered.token), Err(PersistError::InvalidToken)));
        let (replacement, progress, story) = register(&store, "Lost token", with_progress, with_story);
        assert_ne!(replacement.account_id, registered.account_id);
        assert_eq!(progress, with_progress.then(|| expected_progress(replacement.account_id)));
        assert_eq!(story, with_story.then(|| Progression::new(replacement.account_id, GAME, story_bytes(), NOW)));
    }
}

#[test]
fn a_missing_declared_seed_is_not_an_untouched_registration_even_when_the_directory_is_empty() {
    for (with_progress, with_story, remove_progress, remove_story) in [
        (true, true, true, false), (true, true, false, true), (true, true, true, true),
        (true, false, true, false), (false, true, false, true),
    ] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let (registered, _, _) = register(&store, "Missing seed", with_progress, with_story);
        if remove_progress { fs::remove_file(character_progress_file(&root, registered.account_id)).unwrap(); }
        if remove_story { fs::remove_file(story_file(&root, registered.account_id)).unwrap(); }
        let before = file_map(&root);
        assert!(matches!(store.release_unclaimed(registered.account_id), Err(PersistError::NotReleasable { .. })), "missing declared files cannot be treated as an optional absent seed");
        assert!(!store.release_if_unclaimed(registered.account_id).unwrap());
        assert_eq!(file_map(&root), before);
        assert_eq!(store.find_account_by_name("Missing seed"), Some(registered.account_id));
    }
}

#[test]
fn malformed_or_missing_registration_metadata_never_releases_the_account() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    for malformed in 0..6 {
        let (registered, _, _) = register(&store, &format!("Metadata {malformed}"), true, true);
        let path = account_file(&root, registered.account_id);
        let mut metadata = read_json(&path);
        match malformed {
            0 => metadata["registration_story"]["digest"] = json!([1, 2, 3]),
            1 => metadata["registration_progress"] = json!("invalid"),
            2 => metadata["account_id"] = json!(registered.account_id.0 + 100),
            3 => { metadata.as_object_mut().unwrap().remove("registration_story"); }
            4 => metadata["registration_story"]["game"] = json!(GameId::Ff8),
            5 => {},
            _ => unreachable!(),
        }
        if malformed == 5 { fs::remove_file(&path).unwrap(); }
        else { write_json(&path, &metadata); }
        let before = file_map(&root);
        assert!(store.release_unclaimed(registered.account_id).is_err(), "malformed registration metadata case {malformed}");
        assert_eq!(file_map(&root), before);
        assert_eq!(store.find_account_by_name(&format!("Metadata {malformed}")), Some(registered.account_id));
    }
}

#[test]
fn a_directory_replacing_either_expected_seed_is_not_an_absent_optional_file() {
    for replace_story in [false, true] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let (registered, _, _) = register(&store, "Wrong file type", true, true);
        let path = if replace_story { story_file(&root, registered.account_id) }
            else { character_progress_file(&root, registered.account_id) };
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let before = file_map(&root);
        assert!(matches!(store.release_unclaimed(registered.account_id), Err(PersistError::NotReleasable { .. })));
        assert_eq!(file_map(&root), before);
        assert!(path.is_dir());
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_replacing_either_expected_seed_is_refused_even_if_its_target_matches() {
    for replace_story in [false, true] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let (registered, _, _) = register(&store, "Linked seed", true, true);
        let path = if replace_story { story_file(&root, registered.account_id) }
            else { character_progress_file(&root, registered.account_id) };
        let target = root.join("copied-seed.json");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let before = file_map(&root);
        assert!(matches!(store.release_unclaimed(registered.account_id), Err(PersistError::NotReleasable { .. })));
        assert_eq!(file_map(&root), before);
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_replacing_the_whole_seed_directory_cannot_release_its_untouched_files() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let (registered, _, _) = register(&store, "Linked directory", true, true);
    let path = root.join("progression").join(registered.account_id.0.to_string());
    let original = file_map(&path);
    let target = dir.path().join("untouched-seeds");
    fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let before = file_map(&root);
    assert!(matches!(store.release_unclaimed(registered.account_id), Err(PersistError::NotReleasable { .. })));
    assert_eq!(file_map(&root), before);
    assert_eq!(file_map(&target), original, "both original seed files survive the refusal");
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    assert_eq!(store.find_account_by_name("Linked directory"), Some(registered.account_id));
}

#[test]
fn edits_extra_files_receipts_and_claimed_tokens_protect_seeded_accounts() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    for (index, change) in ["story", "story time", "resources", "extra game", "extra file", "receipt", "claimed", "authenticated"].into_iter().enumerate() {
        let (registered, progress, story) = register(&store, &format!("Used {index}"), true, true);
        let account = registered.account_id;
        match change {
            "story" | "story time" => {
                let mut story = story.unwrap();
                if change == "story" { story.blob[17] ^= 0xff; }
                else { story.updated_at += 1; }
                store.put_progression(&story).unwrap();
            }
            "resources" => {
                let mut progress = progress.unwrap();
                progress.gil -= 1;
                store.put_character_progress(&progress).unwrap();
            }
            "extra game" => store.put_progression(&Progression::new(account, GameId::Ff8, vec![1], NOW)).unwrap(),
            "extra file" => fs::write(root.join("progression").join(account.0.to_string()).join("extra.bin"), b"unexpected saved data").unwrap(),
            "receipt" => {
                let pair = store.get_progress_pair(account, GAME).unwrap();
                assert_eq!(store.commit_scene_progress("fixture/unchanged", &pair, &pair).unwrap(), SceneCommit::Applied);
                assert_eq!(store.get_character_progress(account, GAME).unwrap().unwrap().gil, 240, "the receipt alone is enough to protect the account");
            }
            "claimed" => { store.mark_claimed(account).unwrap(); }
            "authenticated" => { store.authenticate(&registered.token).unwrap(); }
            _ => unreachable!(),
        }
        let before = file_map(&root);
        assert!(matches!(store.release_unclaimed(account), Err(PersistError::NotReleasable { .. })), "{change}");
        assert!(!store.release_if_unclaimed(account).unwrap());
        assert_eq!(file_map(&root), before, "refusal preserves all files: {change}");
    }
}

#[test]
fn oversized_story_or_mismatched_seed_games_write_nothing_and_do_not_claim_the_name() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let before = file_map(&root);
    let oversized = NewProgression { game: GAME, blob: vec![0xab; MAX_BLOB_BYTES + 1] };
    let result = store.register_with_character_and_initial_state(
        "Retry", [1, 2, 3], "default", None, Some(character()), Some(new_progress()), Some(oversized), NOW,
    );
    assert!(matches!(result, Err(PersistError::BlobTooLarge { .. })));
    assert_eq!(file_map(&root), before);
    let mismatch = NewProgression { game: GameId::Ff8, blob: story_bytes() };
    let result = store.register_with_character_and_initial_state(
        "Retry", [1, 2, 3], "default", None, Some(character()), Some(new_progress()), Some(mismatch), NOW,
    );
    assert!(matches!(result, Err(PersistError::InvalidField { .. })));
    assert_eq!(file_map(&root), before);
    assert_eq!(store.find_account_by_name("Retry"), None);
    let (registered, progress, story) = register(&store, "Retry", true, true);
    assert_eq!(registered.account_id, AccountId(1), "rejected seeds consume neither the name nor its account id");
    assert_eq!(progress, Some(expected_progress(registered.account_id)));
    assert_eq!(story.unwrap().blob, story_bytes());
}

#[test]
fn a_taken_name_or_full_store_refuses_story_registration_without_upgrading_the_header() {
    for full in [false, true] {
        let (_dir, root) = fresh_root();
        let store = Store::open_with(&root, StoreOptions::default().with_max_accounts(if full { 1 } else { 2 })).unwrap();
        store.create_account("Existing", NOW).unwrap();
        let before = file_map(&root);
        assert_eq!(log_lines(&root)[0]["schema_version"], 2);
        let name = if full { "No room" } else { "Existing" };
        let result = store.register_with_character_and_initial_state(
            name, [1, 2, 3], "default", None, Some(character()), Some(new_progress()), Some(new_story()), NOW,
        );
        if full { assert!(matches!(result, Err(PersistError::AccountLimitReached { .. }))); }
        else { assert!(matches!(result, Err(PersistError::NameTaken(_)))); }
        assert_eq!(file_map(&root), before);
        assert_eq!(log_lines(&root)[0]["schema_version"], 2);
    }
}

#[test]
fn the_existing_progress_registration_wrapper_does_not_create_story_or_a_story_fingerprint() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (registered, progress) = store.register_with_character_and_progress(
        "Wrapper", [1, 2, 3], "default", None, Some(character()), Some(new_progress()), NOW,
    ).unwrap();
    assert_eq!(progress, Some(expected_progress(registered.account_id)));
    assert_eq!(store.get_progression(registered.account_id, GAME).unwrap(), Progression::empty(registered.account_id, GAME));
    assert!(!story_file(&root, registered.account_id).exists());
    assert!(read_json(&account_file(&root, registered.account_id)).get("registration_story").is_none());
    assert!(store.release_if_unclaimed(registered.account_id).unwrap());
}

#[test]
fn trash_restore_rekeys_both_seeds_exactly_and_drops_the_old_registration_fingerprints() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (registered, progress, story) = register(&store, "Restored", true, true);
    let original = read_json(&account_file(&root, registered.account_id));
    assert!(original.get("registration_progress").is_some() && original.get("registration_story").is_some());
    let deleted = store.delete_account(registered.account_id, NOW + 10).unwrap();
    let restored = store.restore_account(&deleted.entry, NOW + 11).unwrap();
    assert_ne!(restored.account_id, registered.account_id);
    assert!(restored.unfinished.is_empty());
    let mut expected_progress = progress.unwrap();
    expected_progress.account_id = restored.account_id;
    let mut expected_story = story.unwrap();
    expected_story.account_id = restored.account_id;
    assert_eq!(store.get_character_progress(restored.account_id, GAME).unwrap(), Some(expected_progress.clone()));
    assert_eq!(store.get_progression(restored.account_id, GAME).unwrap(), expected_story);
    let metadata = read_json(&account_file(&root, restored.account_id));
    assert!(metadata.get("registration_progress").is_none() && metadata.get("registration_story").is_none(), "fingerprints bound to the old account id are discarded on restore");
    let bytes = file_map(&root);
    assert!(!store.release_if_unclaimed(restored.account_id).unwrap(), "restoration creates a claimed account");
    assert_eq!(file_map(&root), bytes);
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_character_progress(restored.account_id, GAME).unwrap(), Some(expected_progress));
    assert_eq!(store.get_progression(restored.account_id, GAME).unwrap(), expected_story);
    assert_eq!(store.authenticate(&restored.token).unwrap(), restored.account_id);
}

#[test]
fn copied_archives_preserve_the_story_format_fence_through_manifest_versions_and_repeated_rekeys() {
    for mode in ["no story", "legacy story", "seed story", "no story in v4"] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let no_story = matches!(mode, "no story" | "no story in v4");
        let source_fenced = matches!(mode, "seed story" | "no story in v4");
        let restored_fenced = mode != "no story";
        let (registered, mut progress, seeded_story) = register(&store, "Archive", no_story, mode == "seed story");
        let mut story = if mode == "legacy story" {
            let story = Progression::new(registered.account_id, GAME, story_bytes(), NOW);
            store.put_progression(&story).unwrap();
            Some(story)
        } else { seeded_story };
        if mode == "no story in v4" {
            register(&store, "Unrelated story", false, true);
            assert!(!story_file(&root, registered.account_id).exists(), "the archived account itself has no story");
        }
        assert_eq!(log_lines(&root)[0]["schema_version"], if source_fenced { 4 } else { 2 });
        let deleted = store.delete_account(registered.account_id, NOW + 10).unwrap();
        let mut entry = deleted.entry;
        let initial_manifest = if source_fenced { 2 } else { 1 };
        assert_eq!(read_json(&root.join("trash").join(&entry).join("manifest.json"))["schema_version"], initial_manifest,
            "archives from a v4 store retain that fence even if the account has no story");
        let mut archive = file_map(&root.join("trash").join(&entry));
        let mut prior_account = registered.account_id;

        // Each destination starts in v2. A legacy story archive starts at manifest v1 and
        // upgrades to v2 on restore. Later imports retain the v4 fence through manifest v2,
        // even with no registration fingerprints or no story in the archived account.
        for generation in 1..=2 {
            let (_target_dir, target_root) = fresh_root();
            let target = open(&target_root);
            for index in 0..generation {
                target.create_account(&format!("Unrelated {index}"), NOW).unwrap();
            }
            assert_eq!(log_lines(&target_root)[0]["schema_version"], 2);
            let imported = target_root.join("trash").join(&entry);
            for (relative, bytes) in &archive {
                let path = imported.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
            let expected_manifest = if generation == 1 { initial_manifest } else if restored_fenced { 2 } else { 1 };
            assert_eq!(read_json(&imported.join("manifest.json"))["schema_version"], expected_manifest);
            let restored = target.restore_account(&entry, NOW + 20 + generation).unwrap();
            assert!(restored.unfinished.is_empty());
            assert_ne!(restored.account_id, prior_account, "every import really rekeys the archived account");
            prior_account = restored.account_id;
            if let Some(progress) = &mut progress { progress.account_id = restored.account_id; }
            if let Some(story) = &mut story { story.account_id = restored.account_id; }
            assert_eq!(log_lines(&target_root)[0]["schema_version"], if restored_fenced { 4 } else { 2 }, "{mode}, restore {generation}");
            assert_eq!(read_json(&target_root.join("index.json"))["schema_version"], 2);
            assert_eq!(read_json(&target_root.join("trash").join(&restored.entry).join("manifest.json"))["schema_version"],
                if restored_fenced { 2 } else { 1 }, "the sealed restore preserves its archive's reader exclusion");
            let metadata = read_json(&account_file(&target_root, restored.account_id));
            assert!(metadata.get("registration_progress").is_none() && metadata.get("registration_story").is_none());
            drop(target);

            let target = open(&target_root);
            assert_eq!(log_lines(&target_root)[0]["schema_version"], if restored_fenced { 4 } else { 2 });
            assert_eq!(target.get_character_progress(restored.account_id, GAME).unwrap(), progress);
            assert_eq!(target.get_progression(restored.account_id, GAME).unwrap(), story.clone().unwrap_or_else(|| Progression::empty(restored.account_id, GAME)));
            assert_eq!(story_file(&target_root, restored.account_id).exists(), story.is_some());
            let deleted = target.delete_account(restored.account_id, NOW + 30 + generation).unwrap();
            entry = deleted.entry;
            archive = file_map(&target_root.join("trash").join(&entry));
        }
    }
}
