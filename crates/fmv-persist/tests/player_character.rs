//! The player-character record: created with the account, added to accounts that predate it, never
//! allowed to move or re-key what an older build wrote.
//!
//! The migration tests load records in the EXACT shape the v6 and the v7 build wrote (`V6_*` and `V7_*`
//! below are the bytes of the files: each is also checked against what this build's serializer writes for the
//! same values, so the fixture cannot drift from the format) into a real store directory, run the
//! migration, and require every file that was there to be byte for byte what it was and the numbers that the
//! pointer finds to be the numbers that were stored.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Barrier};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    CharacterOrigin, CharacterProgress, GameCharacter, LegacyView, NewPlayerCharacter, PersistError, PlayerCharacter, ProgressRef, Store,
    MAX_PLAYER_CHARACTER_BYTES,
};
use serde_json::json;

fn character_file(root: &std::path::Path, id: AccountId) -> std::path::PathBuf {
    root.join("characters").join(format!("{}.json", id.0))
}

fn progress_file(root: &std::path::Path, id: AccountId) -> std::path::PathBuf {
    root.join("progression").join(id.0.to_string()).join("Ff7.character.json")
}

// ---- the files of a v6 and a v7 server, byte for byte ---------------------------------------------------------

/// `avatars/1.json` as a v6 or v7 server wrote it for Blizz (who chose the Tifa look). The avatar format did not
/// change between v5 and v8.
const AVATAR_TIFA: &str = r#"{
  "schema_version": 1,
  "avatar_id": 1,
  "account_id": 1,
  "display_name": "Blizz",
  "tint": [
    90,
    125,
    245
  ],
  "base_archetype": "default",
  "per_game_overrides": {
    "Ff7": "ff7/tifa"
  },
  "created_at": 1700000000,
  "updated_at": 1700000000
}
"#;

/// `progression/1/Ff7.character.json` as the **v6** server last saved it: level 8, four stacks of items (nine
/// items), and no `limit` key (v6 had no gauge).
const V6_PROGRESS: &str = r#"{
  "schema_version": 1,
  "account_id": 1,
  "game": "Ff7",
  "character": 2,
  "level": 8,
  "exp": 1044,
  "ap": 61,
  "gil": 260,
  "updated_at": 1793500000,
  "inventory": [
    {
      "item": 0,
      "count": 2
    },
    {
      "item": 3,
      "count": 1
    },
    {
      "item": 7,
      "count": 1
    },
    {
      "item": 11,
      "count": 5
    }
  ]
}
"#;

/// The same account's record as the **v7** server writes it once a battle left a limit gauge: the v6 file plus
/// `"limit": 137` after the inventory.
const V7_PROGRESS: &str = r#"{
  "schema_version": 1,
  "account_id": 1,
  "game": "Ff7",
  "character": 2,
  "level": 20,
  "exp": 17820,
  "ap": 330,
  "gil": 9120,
  "updated_at": 1793600000,
  "inventory": [
    {
      "item": 0,
      "count": 9
    },
    {
      "item": 3,
      "count": 4
    }
  ],
  "limit": 137
}
"#;

fn items(stacks: &[(u16, u16)]) -> Vec<InventoryEntry> {
    stacks.iter().map(|&(item, count)| InventoryEntry::new(item, count)).collect()
}

/// What the fixtures above are, as values.
fn v6_record() -> CharacterProgress {
    CharacterProgress {
        exp: 1044,
        ap: 61,
        gil: 260,
        inventory: Some(items(&[(0, 2), (3, 1), (7, 1), (11, 5)])),
        ..CharacterProgress::new(AccountId(1), GameId::Ff7, 2, 8, 1_793_500_000)
    }
}

fn v7_record() -> CharacterProgress {
    CharacterProgress {
        exp: 17_820,
        ap: 330,
        gil: 9_120,
        inventory: Some(items(&[(0, 9), (3, 4)])),
        limit: 137,
        ..CharacterProgress::new(AccountId(1), GameId::Ff7, 2, 20, 1_793_600_000)
    }
}

fn pretty(record: &CharacterProgress) -> String {
    format!("{}\n", serde_json::to_string_pretty(record).unwrap())
}

/// The fixtures are what this build's serializer writes for those values (so they are exactly the shape the
/// builds before wrote: a gauge of 0 is no key at all, and a gauge is the last key), and they read back.
#[test]
fn the_v6_and_v7_fixtures_are_the_exact_files_those_builds_wrote() {
    assert_eq!(pretty(&v6_record()), V6_PROGRESS, "v6: no limit key");
    assert_eq!(pretty(&v7_record()), V7_PROGRESS, "v7: the gauge is the last key");
    assert!(!V6_PROGRESS.contains("limit"));
    assert_eq!(serde_json::from_str::<CharacterProgress>(V6_PROGRESS).unwrap(), v6_record());
    assert_eq!(serde_json::from_str::<CharacterProgress>(V7_PROGRESS).unwrap(), v7_record());
    assert_eq!(serde_json::from_str::<CharacterProgress>(V6_PROGRESS).unwrap().limit, 0, "a v6 record reads with an empty gauge");
}

/// A state directory as a server of the build before protocol v8 left it: Blizz is account 1 with the avatar of
/// [`AVATAR_TIFA`] and `progress_json` as its character-progress record.
fn legacy_directory(progress_json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, root) = fresh_root();
    {
        let store = open(&root);
        let reg = store.register("Blizz", [90, 125, 245], "default", Some((GameId::Ff7, "ff7/tifa")), NOW).unwrap();
        assert_eq!(reg.account_id, AccountId(1));
        assert!(reg.character.is_none(), "a build before v8 made no character record");
        store.authenticate(&reg.token).unwrap();
    }
    // The avatar this build wrote is the v7 file (the format did not change), and the progress record is the
    // one the older server saved.
    assert_eq!(NOW, 1_700_000_000);
    assert_eq!(fs::read_to_string(avatar_file(&root, fmv_persist::AvatarId(1))).unwrap(), AVATAR_TIFA);
    fs::create_dir_all(progress_file(&root, AccountId(1)).parent().unwrap()).unwrap();
    fs::write(progress_file(&root, AccountId(1)), progress_json).unwrap();
    assert!(!character_file(&root, AccountId(1)).exists(), "nothing of the new record yet");
    (dir, root)
}

/// The legacy rule the SERVER applies, written out for these tests: the slot is the character the avatar's look
/// names (Tifa is 2, no look is Cloud, 0), the binding is that look, and the pointers follow the slot.
fn legacy_rule(view: &LegacyView<'_>) -> GameCharacter {
    let look = view.avatar.and_then(|a| a.override_for(GameId::Ff7)).map(str::to_owned);
    let slot = match look.as_deref() {
        Some("ff7/tifa") => 2,
        _ => 0,
    };
    GameCharacter { binding: look, slot: Some(slot), progress: GameCharacter::pointers(GameId::Ff7, Some(slot)) }
}

/// Migrate account 1 and require that nothing but the new file appeared.
fn migrate_and_check(root: &std::path::Path, expect: &CharacterProgress) -> PlayerCharacter {
    let before = file_map(root);
    let store = open(root);
    let (record, added) = store.ensure_player_character(AccountId(1), GameId::Ff7, NOW + 10, legacy_rule).unwrap();
    assert!(added);

    // 1. Every file that was there is byte for byte what it was (the avatar, the progress record, the account,
    //    the index): a build that rolls back finds exactly what it wrote.
    let after = file_map(root);
    for (name, bytes) in &before {
        assert_eq!(after.get(name), Some(bytes), "{name} changed");
    }
    let added_files: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert_eq!(added_files, ["characters/1.json"], "the migration only adds the new record");

    // 2. The pointer finds the stored record, and its numbers are the numbers that were stored.
    let linked = store.linked_character_progress(AccountId(1), GameId::Ff7).unwrap().expect("the pointer finds the record");
    assert_eq!(&linked.record, expect, "every number is what the older build stored");
    assert!(linked.in_sync(), "the record's character is the slot's");
    assert_eq!(store.get_character_progress(AccountId(1), GameId::Ff7).unwrap().as_ref(), Some(expect), "and the old API reads the same");

    // 3. Doing it again changes nothing and runs no migration.
    let (again, added_again) = store
        .ensure_player_character(AccountId(1), GameId::Ff7, NOW + 20, |_| panic!("the record already has an entry for Ff7"))
        .unwrap();
    assert!(!added_again);
    assert_eq!(again, record);
    assert_eq!(file_map(root), after);
    record
}

#[test]
fn a_v6_shaped_record_migrates_without_changing_any_number() {
    let (_dir, root) = legacy_directory(V6_PROGRESS);
    let record = migrate_and_check(&root, &v6_record());

    assert_eq!(record.origin, CharacterOrigin::Migrated);
    assert_eq!((record.identity.name.as_str(), record.identity.tint, record.identity.body.as_str()), ("Blizz", [90, 125, 245], ""));
    let game = record.game(GameId::Ff7).unwrap();
    assert_eq!(game.slot, Some(2), "the slot is the character the account was fighting as: Tifa");
    assert_eq!(game.binding.as_deref(), Some("ff7/tifa"), "its look is kept as its binding");
    assert_eq!(game.character_ref(), Some(&ProgressRef::Character { game: GameId::Ff7, character: 2 }));
    assert!(game.has_story_ref());
    // The file on disk is the record, and a fresh open reads it.
    assert_eq!(open(&root).get_player_character(AccountId(1)).unwrap(), Some(record));
    // The progress record is still the exact v6 file.
    assert_eq!(fs::read_to_string(progress_file(&root, AccountId(1))).unwrap(), V6_PROGRESS);
}

#[test]
fn a_v7_shaped_record_migrates_without_changing_any_number() {
    let (_dir, root) = legacy_directory(V7_PROGRESS);
    let record = migrate_and_check(&root, &v7_record());
    assert_eq!(record.slot(GameId::Ff7), Some(2));
    // The limit gauge, the inventory and the level are in the file the pointer names, untouched.
    let linked = open(&root).linked_character_progress(AccountId(1), GameId::Ff7).unwrap().unwrap();
    assert_eq!((linked.record.level, linked.record.limit), (20, 137));
    assert_eq!(linked.record.inventory, Some(items(&[(0, 9), (3, 4)])));
    assert_eq!(fs::read_to_string(progress_file(&root, AccountId(1))).unwrap(), V7_PROGRESS);
}

/// An account that never earned anything has no progress record: the migration finds none, adds none and the
/// pointer finds none (the caller starts from the game's own numbers, as it always has).
#[test]
fn an_account_without_a_progress_record_migrates_and_still_has_none() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register("Wedge", [1, 2, 3], "default", None, NOW).unwrap();
    let before = file_map(&root);
    let (record, added) = store.ensure_player_character(reg.account_id, GameId::Ff7, NOW + 1, legacy_rule).unwrap();
    assert!(added);
    // No look at all: Cloud's slot, which a migrated account may hold, and no binding to keep.
    let game = record.game(GameId::Ff7).unwrap();
    assert_eq!((game.slot, game.binding.as_deref()), (Some(0), None));
    assert_eq!(store.linked_character_progress(reg.account_id, GameId::Ff7).unwrap(), None);
    assert_eq!(store.get_character_progress(reg.account_id, GameId::Ff7).unwrap(), None);
    let after = file_map(&root);
    assert_eq!(after.keys().filter(|k| !before.contains_key(*k)).count(), 1, "one new file");
    assert!(!after.keys().any(|k| k.starts_with("progression/")), "and no progress file was made");
}

/// The progress record says one character and the look another (a rolled-back build that fights by look
/// saved it, or a hand edit). The migration's rule decides the slot; the record is left as it is, and the
/// pointer reports that the two differ so the caller can let the slot win.
#[test]
fn a_stored_character_that_differs_from_the_slot_is_reported_and_left_alone() {
    let odd = V6_PROGRESS.replace("\"character\": 2", "\"character\": 0");
    assert_ne!(odd, V6_PROGRESS);
    let (_dir, root) = legacy_directory(&odd);
    let before = file_map(&root);
    let store = open(&root);
    let (record, _) = store.ensure_player_character(AccountId(1), GameId::Ff7, NOW, legacy_rule).unwrap();
    assert_eq!(record.slot(GameId::Ff7), Some(2), "the look decided");
    let linked = store.linked_character_progress(AccountId(1), GameId::Ff7).unwrap().unwrap();
    assert_eq!((linked.record.character, linked.expected_character, linked.in_sync()), (0, 2, false));
    assert_eq!(linked.record.level, 8, "the numbers are the stored ones whoever the character is");
    let after = file_map(&root);
    assert_eq!(after.get("progression/1/Ff7.character.json"), before.get("progression/1/Ff7.character.json"));
}

#[test]
fn a_migrated_cloud_slot_is_kept_and_a_character_can_gain_a_second_game_later() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register("Cloudy", [1, 2, 3], "default", Some((GameId::Ff7, "ff7/cloud")), NOW).unwrap();
    let (record, _) = store.ensure_player_character(reg.account_id, GameId::Ff7, NOW + 1, legacy_rule).unwrap();
    assert_eq!(record.slot(GameId::Ff7), Some(0), "slot 0 is a migrated account's; the store has no game rule about it");
    let first = file_map(&root);

    // The same character enters another game: an entry is added, the Ff7 entry and the identity are not touched.
    let (both, added) = store
        .ensure_player_character(reg.account_id, GameId::Fixture, NOW + 2, |view| {
            assert_eq!(view.name, "Cloudy");
            assert!(view.progress.is_none());
            GameCharacter { progress: GameCharacter::pointers(GameId::Fixture, None), ..GameCharacter::default() }
        })
        .unwrap();
    assert!(added);
    assert_eq!(both.games.len(), 2);
    assert_eq!(both.game(GameId::Ff7), record.game(GameId::Ff7));
    assert_eq!(both.identity, record.identity);
    assert_eq!((both.created_at, both.updated_at, both.origin), (record.created_at, NOW + 2, CharacterOrigin::Migrated));
    let second = file_map(&root);
    let changed: Vec<&String> = second.keys().filter(|k| first.get(*k) != second.get(*k)).collect();
    assert_eq!(changed, ["characters/1.json"]);
}

// ---- creation with the account ---------------------------------------------------------------------------------

fn barret_slot() -> NewPlayerCharacter {
    NewPlayerCharacter::for_game(
        "sturdy",
        GameId::Ff7,
        GameCharacter { binding: Some("ff7/body/sturdy".into()), slot: Some(1), progress: GameCharacter::pointers(GameId::Ff7, Some(1)) },
    )
}

#[test]
fn registering_with_a_character_stores_it_with_the_account_in_one_step() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register_with_character("Wedge", [200, 10, 10], "default", None, Some(barret_slot()), NOW).unwrap();
    let record = reg.character.clone().expect("the registration returns the record");
    assert_eq!(record.account_id, reg.account_id);
    assert_eq!((record.origin, record.created_at, record.updated_at), (CharacterOrigin::Created, NOW, NOW));
    assert_eq!((record.identity.name.as_str(), record.identity.tint, record.identity.body.as_str()), ("Wedge", [200, 10, 10], "sturdy"));
    assert_eq!(record.slot(GameId::Ff7), Some(1));
    assert_eq!(record.binding(GameId::Ff7), Some("ff7/body/sturdy"));
    assert_eq!(store.get_player_character(reg.account_id).unwrap(), Some(record.clone()));
    // The file is the pretty JSON of the record, with the pointers spelled out.
    assert_eq!(read_json(&character_file(&root, reg.account_id)), serde_json::to_value(&record).unwrap());
    assert_eq!(
        read_json(&character_file(&root, reg.account_id))["games"]["Ff7"]["progress"],
        json!([{"record": "character", "game": "Ff7", "character": 1}, {"record": "story", "game": "Ff7"}])
    );
    // The legacy files are there too and no progress record was written (nothing was earned).
    assert!(avatar_file(&root, reg.avatar.avatar_id).exists());
    assert!(!progress_file(&root, reg.account_id).exists());
    // It survives a restart.
    drop(store);
    assert_eq!(open(&root).get_player_character(reg.account_id).unwrap(), Some(record));
    assert!(has_temp_files(&root).is_empty());
}

#[test]
fn a_registration_whose_character_is_refused_takes_no_name_and_writes_nothing() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let before = file_map(&root);
    for bad in [
        // A pointer that names another character than the slot.
        NewPlayerCharacter::for_game(
            "x",
            GameId::Ff7,
            GameCharacter { slot: Some(1), progress: vec![ProgressRef::Character { game: GameId::Ff7, character: 2 }], ..GameCharacter::default() },
        ),
        // A look that is not a reference at all.
        NewPlayerCharacter::for_game("x", GameId::Ff7, GameCharacter { binding: Some(String::new()), ..GameCharacter::default() }),
        // An unknown game, and a body with a control character.
        NewPlayerCharacter { body: "x".into(), games: BTreeMap::from([("Ff99".to_owned(), GameCharacter::default())]) },
        NewPlayerCharacter { body: "b\u{7}".into(), games: BTreeMap::new() },
    ] {
        let err = store.register_with_character("Wedge", [1, 2, 3], "default", None, Some(bad), NOW).unwrap_err();
        assert!(matches!(err, PersistError::InvalidField { .. }), "{err}");
    }
    assert_eq!(file_map(&root), before, "nothing was written");
    assert_eq!(store.account_count(), 0);
    // The name is free: the refusals did not burn it.
    assert!(store.register_with_character("Wedge", [1, 2, 3], "default", None, Some(barret_slot()), NOW).is_ok());
}

#[test]
fn releasing_an_unclaimed_account_removes_its_character_record_too() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register_with_character("Ghost", [1, 2, 3], "default", None, Some(barret_slot()), NOW).unwrap();
    assert!(character_file(&root, reg.account_id).exists());
    store.release_unclaimed(reg.account_id).unwrap();
    assert!(!character_file(&root, reg.account_id).exists());
    assert!(matches!(store.get_player_character(reg.account_id), Err(PersistError::UnknownAccount(_))));
    // The name can be registered again, with a different slot.
    let again = store
        .register_with_character(
            "Ghost",
            [1, 2, 3],
            "default",
            None,
            Some(NewPlayerCharacter::for_game(
                "x",
                GameId::Ff7,
                GameCharacter { slot: Some(2), progress: GameCharacter::pointers(GameId::Ff7, Some(2)), ..GameCharacter::default() },
            )),
            NOW,
        )
        .unwrap();
    assert_eq!(again.character.unwrap().slot(GameId::Ff7), Some(2));
}

/// A crash between the files and the log entry leaves a file nobody references: it is reported on the next open
/// and overwritten by the registration that gets the id, never read as someone's character.
#[test]
fn a_character_file_left_by_a_crash_is_reported_and_overwritten() {
    let (_dir, root) = fresh_root();
    drop(open(&root));
    fs::write(character_file(&root, AccountId(1)), b"{\"stale\": true}").unwrap();
    let store = open(&root);
    assert!(
        store.open_warnings().iter().any(|w| w.contains("player-character") && w.contains("not referenced by the index")),
        "{:?}",
        store.open_warnings()
    );
    let reg = store.register_with_character("Fresh", [1, 2, 3], "default", None, Some(barret_slot()), NOW).unwrap();
    assert_eq!(reg.account_id, AccountId(1));
    assert_eq!(store.get_player_character(AccountId(1)).unwrap().unwrap().identity.name, "Fresh");
}

#[test]
fn a_damaged_or_foreign_record_is_an_error_and_never_replaced_by_a_default() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register_with_character("Wedge", [1, 2, 3], "default", None, Some(barret_slot()), NOW).unwrap();
    let other = store.register("Other", [1, 2, 3], "default", None, NOW).unwrap();
    let path = character_file(&root, reg.account_id);
    let good = fs::read(&path).unwrap();

    for (what, bytes) in [
        ("not json", b"{ not json".to_vec()),
        ("a newer schema", String::from_utf8(good.clone()).unwrap().replace("\"schema_version\": 1", "\"schema_version\": 2").into_bytes()),
        ("another account's record", String::from_utf8(good.clone()).unwrap().replace("\"account_id\": 1", "\"account_id\": 2").into_bytes()),
        ("a record that breaks the rules", String::from_utf8(good.clone()).unwrap().replace("\"character\": 1", "\"character\": 5").into_bytes()),
        ("an oversized file", vec![b' '; MAX_PLAYER_CHARACTER_BYTES as usize + 1]),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(store.get_player_character(reg.account_id).is_err(), "{what}");
        assert!(
            store.ensure_player_character(reg.account_id, GameId::Ff7, NOW, |_| panic!("no migration over a damaged record")).is_err(),
            "{what}"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes, "{what}: the file is left for the operator");
    }
    // The other account is not affected, and an account that does not exist is said so.
    assert_eq!(store.get_player_character(other.account_id).unwrap(), None);
    assert!(matches!(store.get_player_character(AccountId(99)), Err(PersistError::UnknownAccount(_))));
    assert!(matches!(
        store.ensure_player_character(AccountId(99), GameId::Ff7, NOW, |_| GameCharacter::default()),
        Err(PersistError::UnknownAccount(_))
    ));
}

/// What `legacy` returns must be valid; otherwise nothing is written.
#[test]
fn a_migration_that_builds_an_invalid_entry_writes_nothing() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let reg = store.register("Wedge", [1, 2, 3], "default", None, NOW).unwrap();
    let before = file_map(&root);
    let err = store
        .ensure_player_character(reg.account_id, GameId::Ff7, NOW, |_| GameCharacter {
            slot: Some(1),
            progress: vec![ProgressRef::Character { game: GameId::Ff7, character: 4 }],
            ..GameCharacter::default()
        })
        .unwrap_err();
    assert!(matches!(err, PersistError::InvalidField { field: "progress", .. }), "{err}");
    assert_eq!(file_map(&root), before);
}

/// Two logins of one account at once (two handshakes can authenticate the same token) add the record once.
#[test]
fn first_logins_at_the_same_moment_add_the_record_once() {
    let (_dir, root) = fresh_root();
    let store = Arc::new(open(&root));
    let reg = store.register("Racer", [1, 2, 3], "default", None, NOW).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (store, barrier, runs) = (Arc::clone(&store), Arc::clone(&barrier), Arc::clone(&runs));
            std::thread::spawn(move || {
                barrier.wait();
                let (record, added) = store
                    .ensure_player_character(reg.account_id, GameId::Ff7, NOW, |view| {
                        runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        legacy_rule(view)
                    })
                    .unwrap();
                (record, added)
            })
        })
        .collect();
    let results: Vec<(PlayerCharacter, bool)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1, "the migration ran once");
    assert_eq!(results.iter().filter(|(_, added)| *added).count(), 1, "exactly one login added the record");
    assert!(results.windows(2).all(|w| w[0].0 == w[1].0), "every login saw the same record");
    assert!(has_temp_files(&root).is_empty());
}

// ---- the directory ---------------------------------------------------------------------------------------------

#[test]
fn a_snapshot_carries_the_character_records_and_a_restore_brings_them_back() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let a = store.register_with_character("Wedge", [1, 2, 3], "default", None, Some(barret_slot()), NOW).unwrap();
    let b = store.register("Blizz", [4, 5, 6], "default", Some((GameId::Ff7, "ff7/tifa")), NOW).unwrap();
    store.ensure_player_character(b.account_id, GameId::Ff7, NOW, legacy_rule).unwrap();
    let want_a = store.get_player_character(a.account_id).unwrap();
    let want_b = store.get_player_character(b.account_id).unwrap();
    assert!(want_a.is_some() && want_b.is_some());

    let snap = dir.path().join("snap");
    store.snapshot(&snap).unwrap();
    assert!(snap.join("characters").join("1.json").is_file() && snap.join("characters").join("2.json").is_file());
    drop(store);
    let restored = dir.path().join("restored");
    fmv_persist::restore_snapshot(&snap, &restored).unwrap();
    let store = open(&restored);
    assert_eq!(store.get_player_character(a.account_id).unwrap(), want_a);
    assert_eq!(store.get_player_character(b.account_id).unwrap(), want_b);
}

/// A directory that has only character records (no index) is not an empty directory: it is refused like the
/// other record directories, so the records are never overwritten by a fresh index.
#[test]
fn character_records_without_an_index_are_refused_like_the_other_records() {
    let (_dir, root) = fresh_root();
    fs::create_dir_all(root.join("characters")).unwrap();
    fs::write(character_file(&root, AccountId(1)), b"{}").unwrap();
    assert!(matches!(Store::open(&root), Err(PersistError::IndexMissing { .. })));
}
