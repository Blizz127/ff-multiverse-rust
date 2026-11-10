//! Initial character progress is committed with a new registration. Existing accounts can
//! acquire an absent record once without replacing any progress already on disk.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_persist::{
    CharacterProgress, GameCharacter, NewCharacterProgress, NewPlayerCharacter, PersistError,
    ProgressRef, Progression, Registered, SceneCommit, Store,
};

fn progress_file(root: &Path, account: AccountId) -> PathBuf {
    root.join("progression").join(account.0.to_string()).join("Ff7.character.json")
}

fn character() -> NewPlayerCharacter {
    NewPlayerCharacter::for_game("generic", GameId::Ff7, GameCharacter {
        binding: Some("ff7/cloud".into()),
        slot: Some(0),
        progress: vec![ProgressRef::Character { game: GameId::Ff7, character: 0 }, ProgressRef::Story { game: GameId::Ff7 }],
    })
}

fn initial() -> NewCharacterProgress {
    NewCharacterProgress {
        exp: 861,
        gil: 240,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1)]),
        ..NewCharacterProgress::new(GameId::Ff7, 0, 6)
    }
}

fn seeded(store: &Store, name: &str) -> (Registered, CharacterProgress) {
    let (registered, progress) = store.register_with_character_and_progress(
        name, [1, 2, 3], "default", None, Some(character()), Some(initial()), NOW,
    ).unwrap();
    (registered, progress.expect("the committed initial progress is returned without a second read"))
}

fn legacy_initial(account: AccountId) -> CharacterProgress {
    CharacterProgress {
        exp: 861,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1)]),
        ..CharacterProgress::new(account, GameId::Ff7, 0, 6, NOW + 10)
    }
}

#[test]
fn registration_commits_the_exact_initial_progress_with_its_account_and_character() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let before = log_lines(&root).len();
    let (registered, progress) = seeded(&store, "Fresh");
    assert_eq!(log_lines(&root).len(), before + 1, "one account index entry commits every initial record");
    assert_eq!(progress.account_id, registered.account_id);
    assert_eq!(progress.game, GameId::Ff7);
    assert_eq!((progress.character, progress.level, progress.exp, progress.ap, progress.gil, progress.limit), (0, 6, 861, 0, 240, 0));
    assert_eq!(progress.updated_at, NOW);
    assert_eq!(progress.inventory, initial().inventory);
    assert_eq!(store.get_character_progress(registered.account_id, GameId::Ff7).unwrap(), Some(progress.clone()));
    assert!(store.get_character_progress(registered.account_id, GameId::Ff8).unwrap().is_none());
    assert!(store.get_progression(registered.account_id, GameId::Ff7).unwrap().blob.is_empty());
    let on_disk = fs::read(progress_file(&root, registered.account_id)).unwrap();
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_character_progress(registered.account_id, GameId::Ff7).unwrap(), Some(progress));
    assert_eq!(fs::read(progress_file(&root, registered.account_id)).unwrap(), on_disk);
    assert_eq!(store.authenticate(&registered.token).unwrap(), registered.account_id);
    assert_eq!(store.get_player_character(registered.account_id).unwrap(), registered.character);
}

#[test]
fn invalid_initial_progress_never_claims_the_name_or_writes_registration_records() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let before = file_map(&root);
    let mut bad_level = initial();
    bad_level.level = 0;
    let mut empty_stack = initial();
    empty_stack.inventory = Some(vec![InventoryEntry::new(0, 0)]);
    let mut duplicate = initial();
    duplicate.inventory = Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(0, 1)]);
    let mut oversized = initial();
    oversized.inventory = Some((0..=MAX_INVENTORY_STACKS).map(|i| InventoryEntry::new(i as u16, 1)).collect());
    for progress in [bad_level, empty_stack, duplicate, oversized] {
        let result = store.register_with_character_and_progress(
            "Fresh", [1, 2, 3], "default", None, Some(character()), Some(progress), NOW,
        );
        assert!(matches!(result, Err(PersistError::InvalidField { .. })));
        assert_eq!(file_map(&root), before, "validation precedes all registration disk work");
    }
    let (registered, _) = seeded(&store, "Fresh");
    assert_eq!(registered.account_id, AccountId(1), "refused requests consume neither name nor id");
}

#[test]
fn an_untouched_initial_record_does_not_strand_a_registration_whose_token_was_not_delivered() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (registered, _) = seeded(&store, "Lost token");
    drop(store);
    let store = open(&root);
    assert!(store.release_if_unclaimed(registered.account_id).unwrap());
    assert!(store.account_info(registered.account_id).unwrap().is_none());
    assert!(!progress_file(&root, registered.account_id).exists());
    assert!(matches!(store.authenticate(&registered.token), Err(PersistError::InvalidToken)));
    let (replacement, progress) = seeded(&store, "Lost token");
    assert_ne!(replacement.account_id, registered.account_id, "a committed id is retired even when its token was lost");
    assert_eq!(progress.gil, 240);
}

#[test]
fn any_progress_change_story_record_or_scene_receipt_protects_an_unclaimed_account() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    for (index, change) in ["money", "inventory", "story", "receipt"].into_iter().enumerate() {
        let (registered, mut progress) = seeded(&store, &format!("Progress {index}"));
        match change {
            "money" => {
                progress.gil -= 1;
                store.put_character_progress(&progress).unwrap();
            }
            "inventory" => {
                progress.inventory = Some(Vec::new());
                store.put_character_progress(&progress).unwrap();
            }
            "story" => {
                store.put_progression(&Progression::new(registered.account_id, GameId::Ff7, Vec::new(), NOW)).unwrap();
            }
            "receipt" => {
                let before = store.get_progress_pair(registered.account_id, GameId::Ff7).unwrap();
                assert_eq!(store.commit_scene_progress("fixture/unchanged", &before, &before).unwrap(), SceneCommit::Applied);
                // Isolate the receipt from the separately-created story file: all original
                // character values still equal the initial record, but this account has acted.
                fs::remove_file(progression_file(&root, registered.account_id, "Ff7")).unwrap();
            }
            _ => unreachable!(),
        }
        let before = file_map(&root);
        assert!(matches!(store.release_unclaimed(registered.account_id), Err(PersistError::NotReleasable { .. })), "{change}");
        assert_eq!(file_map(&root), before, "refusing release must preserve every record: {change}");
    }
}

#[test]
fn claiming_or_using_the_token_protects_even_an_untouched_initial_record() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    for (name, authenticate) in [("Claimed", false), ("Logged in", true)] {
        let (registered, _) = seeded(&store, name);
        if authenticate { store.authenticate(&registered.token).unwrap(); }
        else { store.mark_claimed(registered.account_id).unwrap(); }
        let before = file_map(&root);
        assert!(!store.release_if_unclaimed(registered.account_id).unwrap());
        assert_eq!(file_map(&root), before);
    }
}

#[test]
fn ensure_preserves_existing_bytes_inventory_and_receipts_without_calling_the_initializer() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (registered, _) = seeded(&store, "Existing");
    let before = store.get_progress_pair(registered.account_id, GameId::Ff7).unwrap();
    let mut next = before.clone();
    let progress = next.character.as_mut().unwrap();
    progress.gil = 19;
    progress.exp = 1234;
    progress.limit = 7;
    progress.inventory = Some(Vec::new());
    store.commit_scene_progress("fixture/paid", &before, &next).unwrap();
    let expected = store.get_character_progress(registered.account_id, GameId::Ff7).unwrap().unwrap();
    let bytes = file_map(&root);
    let got = store.ensure_character_progress(registered.account_id, GameId::Ff7, || panic!("an existing record has no initializer")).unwrap();
    assert_eq!(got, expected);
    assert_eq!(file_map(&root), bytes);
    assert_eq!(store.commit_scene_progress("fixture/paid", &before, &next).unwrap(), SceneCommit::AlreadyApplied, "receipts survive the no-op");
}

#[test]
fn ensure_creates_a_missing_legacy_record_once_without_crediting_a_new_character_grant() {
    let (_dir, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (account, _) = store.create_account("Legacy", NOW).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let threads: Vec<_> = (0..8).map(|_| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let calls = Arc::clone(&calls);
        std::thread::spawn(move || {
            barrier.wait();
            store.ensure_character_progress(account, GameId::Ff7, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                legacy_initial(account)
            }).unwrap()
        })
    }).collect();
    for thread in threads { assert_eq!(thread.join().unwrap(), legacy_initial(account)); }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "one missing record is initialized once under the writer lock");
    let stored = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
    assert_eq!(stored.gil, 0);
    assert_eq!(stored.inventory, legacy_initial(account).inventory);
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap(), Some(stored));
}

#[test]
fn ensure_refuses_corrupt_existing_records_instead_of_replacing_them_with_defaults() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (registered, _) = seeded(&store, "Damaged");
    let path = progress_file(&root, registered.account_id);
    let damaged = b"{not a valid saved character";
    fs::write(&path, damaged).unwrap();
    let before = file_map(&root);
    assert!(matches!(store.ensure_character_progress(registered.account_id, GameId::Ff7, || panic!("a corrupt record is not missing")), Err(PersistError::Corrupt { .. })));
    assert_eq!(file_map(&root), before);
}

#[test]
fn ensure_rejects_wrong_identity_and_invalid_defaults_without_creating_a_record() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (account, _) = store.create_account("Legacy", NOW).unwrap();
    let before = file_map(&root);
    let wrong_account = legacy_initial(AccountId(account.0 + 1));
    let mut wrong_game = legacy_initial(account);
    wrong_game.game = GameId::Ff8;
    let mut invalid_level = legacy_initial(account);
    invalid_level.level = 0;
    for record in [wrong_account, wrong_game, invalid_level] {
        assert!(matches!(store.ensure_character_progress(account, GameId::Ff7, || record), Err(PersistError::InvalidField { .. })));
        assert_eq!(file_map(&root), before);
    }
    assert!(matches!(store.ensure_character_progress(AccountId(u64::MAX), GameId::Ff7,
        || panic!("unknown accounts cannot request defaults")), Err(PersistError::UnknownAccount(_))));
    assert_eq!(file_map(&root), before);
}
