//! The story blob (`Progression`) as the story model uses it: loading the records that protocol v7
//! builds wrote (or never wrote), the atomic read-modify-write that keeps a story from going back, and
//! what an unreadable record does (nothing: it stays).
//!
//! A v7 server never wrote a `Progression` at all (nothing in it stores story state; only tests and
//! tools did), so every real account has NO progression file, and the other records a v7 account
//! has (the character record, with or without inventory and limit) live beside it. The story model
//! reads "no file" as a new account (empty blob, moment 1 for FF7). These tests pin that, and that a
//! record in the shape v7 wrote (`schema_version`, `account_id`, `game`, `blob`, `updated_at`; no
//! other key) still loads, and that a story record this build writes has exactly that shape, so a
//! rollback to v7 reads it (the blob is opaque there).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use fmv_core::{AccountId, GameId};
use fmv_persist::{CharacterProgress, PersistError, Progression, Store, MAX_BLOB_BYTES, SCHEMA_VERSION};
use serde_json::{json, Value};

fn progression_file(root: &Path, id: AccountId, game: &str) -> PathBuf {
    root.join("progression").join(id.0.to_string()).join(format!("{game}.json"))
}

fn account(store: &Store, name: &str) -> AccountId {
    store.create_account(name, NOW).unwrap().0
}

/// A FF7-style story blob (`[1, moment_lo, moment_hi]`): the store neither knows nor checks the shape.
fn moment(m: u16) -> Vec<u8> {
    let [lo, hi] = m.to_le_bytes();
    vec![1, lo, hi]
}

fn moment_of(blob: &[u8]) -> u16 {
    match blob {
        [] => 1,
        [1, lo, hi] => u16::from_le_bytes([*lo, *hi]),
        _ => 0,
    }
}

/// What a v7 build wrote for a progression record, byte for byte in its field names.
fn v7_record(account: AccountId, game: &str, blob_hex: &str) -> Value {
    json!({ "schema_version": 1, "account_id": account.0, "game": game, "blob": blob_hex, "updated_at": 0 })
}

#[test]
fn an_account_that_v7_never_wrote_a_story_for_reads_as_the_empty_default() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let id = account(&store, "Blizz");
    // What v7 did store for such an account: a character record (no inventory key, no limit key).
    let mut character = CharacterProgress::new(id, GameId::Ff7, 0, 7, NOW);
    character.exp = 123;
    store.put_character_progress(&character).unwrap();
    assert!(!progression_file(&root, id, "Ff7").exists(), "no story file, as on every v7 account");

    let story = store.get_progression(id, GameId::Ff7).unwrap();
    assert_eq!(story, Progression::empty(id, GameId::Ff7));
    assert!(story.blob.is_empty() && story.updated_at == 0, "an empty blob is a new account to the story model");
    // Reading it neither creates it nor touches the character record that sits beside it.
    assert!(!progression_file(&root, id, "Ff7").exists());
    assert_eq!(store.get_character_progress(id, GameId::Ff7).unwrap(), Some(character));
}

#[test]
fn records_in_the_shape_v7_wrote_load_unchanged_and_an_update_sees_exactly_them() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (empty, opaque) = (account(&store, "Empty"), account(&store, "Opaque"));
    for (id, blob_hex, blob) in [(empty, "", vec![]), (opaque, "0707", vec![7u8, 7])] {
        let path = progression_file(&root, id, "Ff7");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&v7_record(id, "Ff7", blob_hex)).unwrap()).unwrap();
        let loaded = store.get_progression(id, GameId::Ff7).unwrap();
        assert_eq!(loaded, Progression { schema_version: 1, account_id: id, game: GameId::Ff7, blob, updated_at: 0 });
        // An update that changes nothing hands the closure exactly the record, and writes it back
        // in the same shape.
        let mut seen = None;
        let back = store
            .update_progression(id, GameId::Ff7, |current| {
                seen = Some(current.clone());
                current
            })
            .unwrap();
        assert_eq!(Some(loaded.clone()), seen);
        assert_eq!(back, loaded);
        let keys: Vec<String> = read_json(&path).as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["account_id", "blob", "game", "schema_version", "updated_at"], "no key a v7 build would not know");
    }
    // The opaque blob [7, 7] of a hand-written record is exactly what the story model must NOT
    // interpret or overwrite: it is still there.
    assert_eq!(store.get_progression(opaque, GameId::Ff7).unwrap().blob, vec![7, 7]);
}

#[test]
fn a_story_this_build_writes_is_a_plain_v7_shaped_record() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let id = account(&store, "Blizz");
    let written = store
        .update_progression(id, GameId::Ff7, |mut current| {
            current.blob = moment(6);
            current.updated_at = NOW + 5;
            current
        })
        .unwrap();
    assert_eq!(written.blob, moment(6));
    assert_eq!(written.schema_version, SCHEMA_VERSION, "SCHEMA_VERSION stays 1: nothing in the record changed shape");
    let on_disk = read_json(&progression_file(&root, id, "Ff7"));
    assert_eq!(
        on_disk,
        json!({ "schema_version": 1, "account_id": id.0, "game": "Ff7", "blob": "010600", "updated_at": NOW + 5 }),
        "what a v7 build would read: hex blob, nothing else"
    );
    assert_eq!(store.get_progression(id, GameId::Ff7).unwrap(), written);
    // Other games and the character record are untouched.
    assert_eq!(store.get_progression(id, GameId::Ff8).unwrap(), Progression::empty(id, GameId::Ff8));
    assert_eq!(store.get_character_progress(id, GameId::Ff7).unwrap(), None);
}

#[test]
fn an_update_that_names_another_account_or_game_or_a_huge_blob_writes_nothing() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (a, b) = (account(&store, "A"), account(&store, "B"));
    store.update_progression(a, GameId::Ff7, |mut p| {
        p.blob = moment(6);
        p
    })
    .unwrap();
    let before = fs::read(progression_file(&root, a, "Ff7")).unwrap();

    let wrong_account = store.update_progression(a, GameId::Ff7, |mut p| {
        p.account_id = b;
        p
    });
    assert!(matches!(wrong_account, Err(PersistError::InvalidField { field: "account_id", .. })), "{wrong_account:?}");
    let wrong_game = store.update_progression(a, GameId::Ff7, |mut p| {
        p.game = GameId::Ff8;
        p
    });
    assert!(matches!(wrong_game, Err(PersistError::InvalidField { .. })), "{wrong_game:?}");
    let huge = store.update_progression(a, GameId::Ff7, |mut p| {
        p.blob = vec![0; MAX_BLOB_BYTES + 1];
        p
    });
    assert!(matches!(huge, Err(PersistError::BlobTooLarge { .. })), "{huge:?}");
    assert_eq!(fs::read(progression_file(&root, a, "Ff7")).unwrap(), before, "the old record stays");

    let ghost = store.update_progression(AccountId(9999), GameId::Ff7, |p| p);
    assert!(matches!(ghost, Err(PersistError::UnknownAccount(_))), "{ghost:?}");
}

#[test]
fn a_record_that_cannot_be_read_is_an_error_and_is_never_replaced_by_a_default() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let id = account(&store, "Blizz");
    let path = progression_file(&root, id, "Ff7");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // damaged JSON, and a record of another account's
    for garbage in [b"{ not json".to_vec(), serde_json::to_vec(&v7_record(AccountId(id.0 + 77), "Ff7", "")).unwrap()] {
        fs::write(&path, &garbage).unwrap();
        let mut ran = false;
        let result = store.update_progression(id, GameId::Ff7, |p| {
            ran = true;
            p
        });
        assert!(result.is_err() && !ran, "the closure never ran over a record it could not read: {result:?}");
        assert!(store.get_progression(id, GameId::Ff7).is_err());
        assert_eq!(fs::read(&path).unwrap(), garbage, "the file is untouched");
    }
}

#[test]
fn updates_that_merge_forward_never_lose_the_furthest_story_whatever_order_they_arrive_in() {
    let (_dir, root) = fresh_root();
    let store = Arc::new(open(&root));
    let id = account(&store, "Blizz");
    let handles: Vec<_> = (0..8u16)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                // every thread saves moments in a different, interleaved order
                for step in 0..40u16 {
                    let m = 2 + (step * 7 + t * 13) % 300;
                    store
                        .update_progression(id, GameId::Ff7, |mut p| {
                            if m > moment_of(&p.blob) {
                                p.blob = moment(m);
                            }
                            p
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let expected = (0..8u16).flat_map(|t| (0..40u16).map(move |s| 2 + (s * 7 + t * 13) % 300)).max().unwrap();
    let stored = store.get_progression(id, GameId::Ff7).unwrap();
    assert_eq!(moment_of(&stored.blob), expected, "the furthest moment survived every interleaving");
    assert!(has_temp_files(&root).is_empty(), "no temp file left behind");
}
