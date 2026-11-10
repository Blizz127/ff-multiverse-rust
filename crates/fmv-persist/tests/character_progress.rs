//! `CharacterProgress`: the per-account, per-game record of the character an account plays, what
//! it earned (level, experience, AP, gil) and what it carries (the optional inventory), through
//! the store's public API and its on-disk layout. It is a separate record from `Progression`, the
//! story blob that is also a phase key; these tests include that the two never touch each other.
//!
//! Crash consistency is argued like the rest of the store: one atomic file replace per write
//! (temp file, fsync, rename, fsync the directory), checked here with leftovers of an interrupted
//! write, concurrent readers that must never see a torn file, and a writer process that is
//! SIGKILLed in the middle of its updates.
//!
//! The inventory was added to records that already existed on a running server, so its tests
//! (the last section) are about compatibility as much as about the field: `None` (never stored)
//! against `Some(vec![])` (stored, empty), a record without the key read and written back, and a
//! hand-edited or hostile list.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_persist::{
    restore_snapshot, CharacterProgress, PersistError, Progression, Store,
    MAX_CHARACTER_RECORD_BYTES, SCHEMA_VERSION,
};
use serde_json::{json, Value};

fn character_file(root: &Path, id: AccountId, game: &str) -> PathBuf {
    root.join("progression")
        .join(id.0.to_string())
        .join(format!("{game}.character.json"))
}

/// A record whose fields are all derived from `exp`, so that a torn mixture of two writes cannot
/// look like a valid one.
fn derived(account: AccountId, game: GameId, exp: u32) -> CharacterProgress {
    CharacterProgress {
        schema_version: SCHEMA_VERSION,
        account_id: account,
        game,
        character: (exp % 9) as u8,
        level: 1 + (exp % 90) as u8,
        exp,
        ap: exp * 2,
        gil: exp * 3,
        updated_at: NOW + u64::from(exp),
        inventory: None,
        limit: (exp % 256) as u16,
    }
}

fn is_consistent(p: &CharacterProgress) -> bool {
    *p == derived(p.account_id, p.game, p.exp)
}

fn record(
    account: AccountId,
    game: GameId,
    character: u8,
    level: u8,
    exp: u32,
) -> CharacterProgress {
    CharacterProgress {
        exp,
        ap: 5,
        gil: 250,
        ..CharacterProgress::new(account, game, character, level, NOW)
    }
}

/// Inventory stacks from `(item, count)` pairs.
fn stacks(items: impl IntoIterator<Item = (u16, u16)>) -> Vec<InventoryEntry> {
    items
        .into_iter()
        .map(|(item, count)| InventoryEntry::new(item, count))
        .collect()
}

/// `record` carrying `items` as its (stored, possibly empty) inventory.
fn carrying(record: CharacterProgress, items: Vec<InventoryEntry>) -> CharacterProgress {
    CharacterProgress {
        inventory: Some(items),
        ..record
    }
}

// ---- the record --------------------------------------------------------------------------

#[test]
fn absent_means_none_and_an_unknown_account_is_an_error() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    for game in [GameId::Ff7, GameId::Ff8, GameId::Fixture] {
        assert_eq!(
            store.get_character_progress(a, game).unwrap(),
            None,
            "{game:?}"
        );
    }
    // Looking never writes.
    assert!(!root.join("progression").join(a.0.to_string()).exists());

    let ghost = AccountId(42);
    assert!(matches!(
        store.get_character_progress(ghost, GameId::Ff7),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(matches!(
        store.put_character_progress(&record(ghost, GameId::Ff7, 0, 5, 0)),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(matches!(
        store.update_character_progress(ghost, GameId::Ff7, |_| record(
            ghost,
            GameId::Ff7,
            0,
            5,
            0
        )),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(
        !root.join("progression").join("42").exists(),
        "a refused write leaves no trace"
    );
}

#[test]
fn records_are_isolated_per_account_and_game_and_land_in_their_own_file() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();

    let a_ff7 = record(a, GameId::Ff7, 0, 7, 1234);
    let a_ff8 = record(a, GameId::Ff8, 3, 12, 99);
    let b_ff7 = record(b, GameId::Ff7, 2, 6, 372);
    for r in [&a_ff7, &a_ff8, &b_ff7] {
        store.put_character_progress(r).unwrap();
    }
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(a_ff7.clone())
    );
    assert_eq!(
        store.get_character_progress(a, GameId::Ff8).unwrap(),
        Some(a_ff8)
    );
    assert_eq!(
        store.get_character_progress(b, GameId::Ff7).unwrap(),
        Some(b_ff7)
    );
    assert_eq!(store.get_character_progress(b, GameId::Ff8).unwrap(), None);
    assert_eq!(
        store.get_character_progress(a, GameId::Fixture).unwrap(),
        None
    );

    // One file per (account, game), readable JSON that says what it is.
    assert!(character_file(&root, a, "Ff7").is_file());
    assert!(character_file(&root, a, "Ff8").is_file());
    assert!(!character_file(&root, b, "Ff8").exists());
    let on_disk = read_json(&character_file(&root, a, "Ff7"));
    assert_eq!(on_disk["schema_version"], SCHEMA_VERSION);
    assert_eq!(on_disk["game"], "Ff7");
    assert_eq!(on_disk["character"], 0);
    assert_eq!(
        (on_disk["level"].as_u64(), on_disk["exp"].as_u64()),
        (Some(7), Some(1234))
    );
    assert_eq!(
        (on_disk["ap"].as_u64(), on_disk["gil"].as_u64()),
        (Some(5), Some(250))
    );
    // Without an inventory a record is a couple of hundred bytes (what a full one adds, and the
    // cap that leaves room for it, are tested with the inventory below).
    assert!(fs::metadata(character_file(&root, a, "Ff7")).unwrap().len() < 1024);

    // Overwriting one record leaves every other file byte for byte alone.
    let before = file_map(&root);
    let mut next = a_ff7.clone();
    next.exp += 10;
    next.updated_at += 1;
    store.put_character_progress(&next).unwrap();
    let after = file_map(&root);
    let changed: Vec<_> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(changed, [format!("progression/{}/Ff7.character.json", a.0)]);

    // A caller's stale schema_version is re-stamped.
    let mut stale = next.clone();
    stale.schema_version = 77;
    store.put_character_progress(&stale).unwrap();
    assert_eq!(
        store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap()
            .schema_version,
        SCHEMA_VERSION
    );

    // Restart: everything is still there and nothing needed repairing.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(next)
    );
}

/// The story blob is also the phase key: character progress must neither touch it nor be
/// reachable through it.
#[test]
fn character_progress_and_the_story_blob_never_touch_each_other() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();

    // Only a character record: the story blob still reads as empty.
    store
        .put_character_progress(&record(a, GameId::Ff7, 0, 9, 800))
        .unwrap();
    assert_eq!(
        store.get_progression(a, GameId::Ff7).unwrap(),
        Progression::empty(a, GameId::Ff7)
    );

    // Only the story blob changes: the character record is not affected.
    let story = Progression::new(a, GameId::Ff7, vec![1, 2, 3], NOW + 1);
    store.put_progression(&story).unwrap();
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(record(a, GameId::Ff7, 0, 9, 800))
    );

    // Updating the character record changes exactly its own file; the blob's bytes stay.
    let before = file_map(&root);
    store
        .update_character_progress(a, GameId::Ff7, |cur| {
            let mut p = cur.unwrap();
            p.exp += 50;
            p.gil += 1;
            p
        })
        .unwrap();
    let after = file_map(&root);
    let changed: Vec<_> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(changed, [format!("progression/{}/Ff7.character.json", a.0)]);
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), story);
    assert_ne!(
        progression_file(&root, a, "Ff7"),
        character_file(&root, a, "Ff7")
    );
}

#[test]
fn the_checks_every_game_shares_refuse_a_bad_record_and_keep_the_old_one() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let good = record(a, GameId::Ff7, 0, 5, 100);
    store.put_character_progress(&good).unwrap();

    // Level 0 never existed.
    let zero = CharacterProgress {
        level: 0,
        ..good.clone()
    };
    assert!(matches!(
        store.put_character_progress(&zero),
        Err(PersistError::InvalidField { field: "level", .. })
    ));
    assert!(matches!(
        store.update_character_progress(a, GameId::Ff7, |_| zero.clone()),
        Err(PersistError::InvalidField { field: "level", .. })
    ));
    // An update may not answer with another account's or another game's record.
    let b = AccountId(2);
    assert!(matches!(
        store.update_character_progress(a, GameId::Ff7, |_| record(b, GameId::Ff7, 0, 5, 1)),
        Err(PersistError::InvalidField { .. })
    ));
    assert!(matches!(
        store.update_character_progress(a, GameId::Ff7, |_| record(a, GameId::Ff8, 0, 5, 1)),
        Err(PersistError::InvalidField { .. })
    ));
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(good)
    );
    assert_eq!(store.get_character_progress(a, GameId::Ff8).unwrap(), None);
    assert_eq!(has_temp_files(&root), Vec::<String>::new());

    // The store has no opinion on a game's own caps: those belong to its adapter.
    let big = CharacterProgress {
        exp: u32::MAX,
        ap: u32::MAX,
        gil: u32::MAX,
        level: u8::MAX,
        ..record(a, GameId::Ff8, 200, 1, 0)
    };
    store.put_character_progress(&big).unwrap();
    assert_eq!(
        store.get_character_progress(a, GameId::Ff8).unwrap(),
        Some(big)
    );
}

// ---- the atomic read-modify-write ----------------------------------------------------------

#[test]
fn update_starts_from_none_and_hands_back_what_it_stored() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();

    let mut seen = Vec::new();
    for step in 0..3u32 {
        let stored = store
            .update_character_progress(a, GameId::Ff7, |cur| {
                seen.push(cur.clone());
                let mut p = cur.unwrap_or_else(|| record(a, GameId::Ff7, 0, 6, 0));
                p.exp += 16;
                p.gil += 10;
                p.schema_version = 99; // ignored: the store stamps its own
                p
            })
            .unwrap();
        assert_eq!(stored.schema_version, SCHEMA_VERSION);
        assert_eq!(stored.exp, 16 * (step + 1));
        assert_eq!(
            store.get_character_progress(a, GameId::Ff7).unwrap(),
            Some(stored)
        );
    }
    assert_eq!(seen[0], None, "the first update sees no record");
    assert_eq!(seen[1].as_ref().unwrap().exp, 16);
    assert_eq!(seen[2].as_ref().unwrap().exp, 32);
}

#[test]
fn concurrent_updates_never_lose_an_increment() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    const THREADS: u32 = 6;
    const EACH: u32 = 25;

    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for _ in 0..EACH {
                    store
                        .update_character_progress(a, GameId::Ff7, |cur| {
                            let mut p = cur.unwrap_or_else(|| record(a, GameId::Ff7, 0, 1, 0));
                            p.exp += 1;
                            p.gil += 2;
                            p.ap += 3;
                            p
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let p = store
        .get_character_progress(a, GameId::Ff7)
        .unwrap()
        .unwrap();
    let n = THREADS * EACH;
    // The record started with ap 5 / gil 250 (see `record`) only if it existed first; here the
    // first update created it from `record(.., 0)`, which carries ap 5 and gil 250 once.
    assert_eq!((p.exp, p.gil, p.ap), (n, 250 + 2 * n, 5 + 3 * n));
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

// ---- crash consistency ---------------------------------------------------------------------

/// A reader never sees half a write: while a writer replaces the record over and over, every read
/// is a whole record that some write produced (all of its fields derive from one `exp`).
#[test]
fn readers_never_see_a_torn_record() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    store
        .put_character_progress(&derived(a, GameId::Ff7, 1))
        .unwrap();

    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            for exp in 2..=200u32 {
                store
                    .put_character_progress(&derived(a, GameId::Ff7, exp))
                    .unwrap();
            }
        })
    };
    let mut last = 0;
    let mut reads = 0u32;
    while !writer.is_finished() || reads < 200 {
        let p = store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .expect("always present");
        assert!(is_consistent(&p), "a torn or mixed record: {p:?}");
        assert!(p.exp >= last, "time went backwards: {} after {last}", p.exp);
        last = p.exp;
        reads += 1;
    }
    writer.join().unwrap();
    assert_eq!(
        store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap()
            .exp,
        200
    );
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

#[test]
fn the_leftovers_of_an_interrupted_write_are_swept_and_the_old_record_survives() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let old = record(a, GameId::Ff7, 0, 7, 1000);
    store.put_character_progress(&old).unwrap();
    drop(store);

    // A crash after the temp file was written (even completely) but before the rename: the temp
    // file holds a newer record, the real one is untouched.
    let dir = root.join("progression").join(a.0.to_string());
    let newer = serde_json::to_vec_pretty(&CharacterProgress {
        exp: 5000,
        ..old.clone()
    })
    .unwrap();
    fs::write(dir.join(".Ff7.character.json.tmp-123-1"), &newer).unwrap();
    fs::write(dir.join(".Ff8.character.json.tmp-123-2"), b"{\"schema_ver").unwrap();

    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("2 temp file")),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
    // Not the newer, unfinished value: a write counts only once its rename happened.
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(old)
    );
    assert_eq!(store.get_character_progress(a, GameId::Ff8).unwrap(), None);
}

#[cfg(target_os = "linux")]
mod killed_writer {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct KillOnDrop(Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Not a test of its own: `a_writer_killed_mid_update_leaves_a_whole_record` re-runs this
    /// binary with only this test selected and `FMV_PERSIST_TEST_WRITER` set, so that it acts as
    /// a server process that keeps saving battle results until it is killed.
    #[test]
    fn zz_update_until_killed() {
        let Ok(root) = std::env::var("FMV_PERSIST_TEST_WRITER") else {
            return;
        };
        let store = Store::open(&root).expect("child could not open the store");
        let account = store
            .find_account_by_name("player")
            .expect("the account exists");
        let started = Instant::now();
        // Bounded, so an orphaned child ends by itself if its parent dies.
        while started.elapsed() < Duration::from_secs(120) {
            store
                .update_character_progress(account, GameId::Ff7, |cur| {
                    derived(account, GameId::Ff7, cur.map_or(0, |p| p.exp) + 1)
                })
                .expect("child update");
        }
    }

    /// The exp the record file holds right now, read straight from the file (the child owns the
    /// store). `None` while there is no file or it cannot be parsed yet.
    fn exp_on_disk(path: &Path) -> Option<u32> {
        let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        u32::try_from(value["exp"].as_u64()?).ok()
    }

    #[test]
    fn a_writer_killed_mid_update_leaves_a_whole_record() {
        let (_guard, root) = fresh_root();
        let account = {
            let store = open(&root);
            store.create_account("player", NOW).unwrap().0
        };
        let file = character_file(&root, account, "Ff7");
        let exe = std::env::current_exe().unwrap();
        let mut floor = 0u32;
        let mut swept_a_leftover = false;

        for round in 0..8u64 {
            let mut child = KillOnDrop(
                Command::new(&exe)
                    .args([
                        "--exact",
                        "killed_writer::zz_update_until_killed",
                        "--test-threads=1",
                    ])
                    .env("FMV_PERSIST_TEST_WRITER", &root)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn writer process"),
            );
            // Wait until the child really is saving: the record advanced past the last round.
            let deadline = Instant::now() + Duration::from_secs(120);
            while exp_on_disk(&file).is_none_or(|exp| exp < floor + 3) {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "the writer exited early"
                );
                assert!(Instant::now() < deadline, "the writer never made progress");
                std::thread::sleep(Duration::from_millis(5));
            }
            // Stagger the kill so the rounds hit different moments of the write cycle.
            std::thread::sleep(Duration::from_micros(700 * round));
            // kill -9: no destructor runs, whatever the child was doing stops right there.
            child.0.kill().unwrap();
            child.0.wait().unwrap();

            // The record is whole and is a value the child wrote (every field derives from exp)...
            let store = open(&root);
            swept_a_leftover |= store
                .open_warnings()
                .iter()
                .any(|w| w.contains("temp file"));
            let p = store
                .get_character_progress(account, GameId::Ff7)
                .unwrap()
                .unwrap();
            assert!(is_consistent(&p), "round {round}: a torn record: {p:?}");
            assert!(p.exp >= floor + 3, "round {round}: {} after {floor}", p.exp);
            // ...no half-written file is left once the store has been opened again...
            assert_eq!(has_temp_files(&root), Vec::<String>::new(), "round {round}");
            // ...and the store goes on from there.
            let next = store
                .update_character_progress(account, GameId::Ff7, |cur| {
                    derived(account, GameId::Ff7, cur.unwrap().exp + 1)
                })
                .unwrap();
            assert_eq!(next.exp, p.exp + 1);
            floor = next.exp;
        }
        eprintln!(
            "a kill landed between a temp file and its rename in some round: {swept_a_leftover}"
        );
    }
}

// ---- backup, permissions, the account lifecycle ---------------------------------------------

#[test]
fn snapshots_carry_character_progress_and_restore_it() {
    let (guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();
    store
        .put_progression(&Progression::new(a, GameId::Ff7, vec![9, 9], NOW))
        .unwrap();
    let ff7 = record(a, GameId::Ff7, 0, 8, 949);
    let ff8 = record(a, GameId::Ff8, 1, 3, 12);
    let beta = record(b, GameId::Ff7, 4, 10, 2000);
    for r in [&ff7, &ff8, &beta] {
        store.put_character_progress(r).unwrap();
    }

    let snap = guard.path().join("snap");
    let info = store.snapshot(&snap).unwrap();
    assert!(info.files >= 8, "{info:?}");
    for (id, game) in [(a, "Ff7"), (a, "Ff8"), (b, "Ff7")] {
        let rel = format!("progression/{}/{game}.character.json", id.0);
        assert_eq!(
            fs::read(snap.join(&rel)).unwrap(),
            fs::read(root.join(&rel)).unwrap(),
            "{rel}"
        );
    }
    assert_eq!(has_temp_files(&snap), Vec::<String>::new());

    // A record written after the snapshot is not in it; restoring gives the old state.
    let mut later = ff7.clone();
    later.exp = 9999;
    store.put_character_progress(&later).unwrap();
    drop(store);
    let restored = guard.path().join("restored");
    restore_snapshot(&snap, &restored).unwrap();
    let copy = open(&restored);
    assert!(
        copy.open_warnings().is_empty(),
        "{:?}",
        copy.open_warnings()
    );
    assert_eq!(
        copy.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(ff7)
    );
    assert_eq!(
        copy.get_character_progress(a, GameId::Ff8).unwrap(),
        Some(ff8)
    );
    assert_eq!(
        copy.get_character_progress(b, GameId::Ff7).unwrap(),
        Some(beta)
    );
    assert_eq!(
        copy.get_progression(a, GameId::Ff7).unwrap().blob,
        vec![9, 9]
    );
}

#[cfg(unix)]
#[test]
fn character_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    store
        .put_character_progress(&record(a, GameId::Ff7, 0, 5, 1))
        .unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&character_file(&root, a, "Ff7")), 0o600);
    assert_eq!(mode(&root.join("progression").join(a.0.to_string())), 0o700);
    // An update replaces the file with one that is just as private.
    store
        .update_character_progress(a, GameId::Ff7, |c| c.unwrap())
        .unwrap();
    assert_eq!(mode(&character_file(&root, a, "Ff7")), 0o600);
}

/// An account whose character earned something is a player, not an orphan: the operator tools
/// that give back unclaimed accounts must leave it alone, like one with a story blob.
#[test]
fn an_account_with_character_progress_is_not_released_as_an_orphan() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (empty, _) = store.create_account("Empty", NOW).unwrap();
    let (played, _) = store.create_account("Played", NOW).unwrap();
    store
        .put_character_progress(&record(played, GameId::Ff7, 0, 7, 700))
        .unwrap();

    assert!(!store.release_if_unclaimed(played).unwrap());
    assert!(matches!(
        store.release_unclaimed(played),
        Err(PersistError::NotReleasable {
            reason: "it has stored progression",
            ..
        })
    ));
    assert_eq!(
        store
            .get_character_progress(played, GameId::Ff7)
            .unwrap()
            .map(|p| p.exp),
        Some(700)
    );
    // The account with nothing is released as before.
    assert!(store.release_if_unclaimed(empty).unwrap());
}

// ---- hostile and damaged files ---------------------------------------------------------------

fn put_raw(root: &Path, account: AccountId, game: &str, bytes: &[u8]) -> PathBuf {
    let path = character_file(root, account, game);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

fn doc(f: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = json!({
        "schema_version": 1, "account_id": 1, "game": "Ff7", "character": 0, "level": 5,
        "exp": 100, "ap": 1, "gil": 2, "updated_at": 3
    });
    f(&mut v);
    serde_json::to_vec(&v).unwrap()
}

#[test]
fn damaged_files_are_errors_never_panics_and_never_replaced_by_defaults() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    assert_eq!(a, AccountId(1));
    let (b, _) = store.create_account("Beta", NOW).unwrap();

    // A hand-edited extra field is ignored (a reader of this version reads what it knows).
    put_raw(&root, a, "Ff7", &doc(|v| v["note"] = json!("hi")));
    assert_eq!(
        store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap()
            .exp,
        100
    );

    let corrupt: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("truncated", b"{\"schema_version\":1,\"acc".to_vec()),
        ("null", b"null".to_vec()),
        ("array", b"[1,2,3]".to_vec()),
        ("not utf8", vec![0xff, 0xfe, 0x00, 0x80]),
        (
            "no schema",
            doc(|v| drop(v.as_object_mut().unwrap().remove("schema_version"))),
        ),
        ("string schema", doc(|v| v["schema_version"] = json!("1"))),
        (
            "missing exp",
            doc(|v| drop(v.as_object_mut().unwrap().remove("exp"))),
        ),
        ("string level", doc(|v| v["level"] = json!("5"))),
        ("level over u8", doc(|v| v["level"] = json!(256))),
        ("negative exp", doc(|v| v["exp"] = json!(-1))),
        ("exp over u32", doc(|v| v["exp"] = json!(4_294_967_296u64))),
        ("float gil", doc(|v| v["gil"] = json!(1.5))),
        ("unknown game", doc(|v| v["game"] = json!("Ff99"))),
        ("level zero on disk", doc(|v| v["level"] = json!(0))),
        (
            "another account's record",
            doc(|v| v["account_id"] = json!(2)),
        ),
        ("another game's record", doc(|v| v["game"] = json!("Ff8"))),
        (
            // Under the size cap (2 * 1900 + 1 bytes), so it is the parser's depth limit that
            // refuses it. A bigger bomb is refused by the cap, before any parsing.
            "deeply nested",
            format!("{}1{}", "[".repeat(1900), "]".repeat(1900)).into_bytes(),
        ),
    ];
    for (what, bytes) in &corrupt {
        let path = put_raw(&root, a, "Ff7", bytes);
        match store.get_character_progress(a, GameId::Ff7) {
            Err(PersistError::Corrupt { .. }) => {}
            other => panic!("{what}: expected Corrupt, got {other:?}"),
        }
        // Nothing replaced the damaged file: an operator can still look at it.
        assert_eq!(&fs::read(&path).unwrap(), bytes, "{what}");
    }

    // Schema versions: 0 never existed, a newer one is from a newer build.
    put_raw(&root, a, "Ff7", &doc(|v| v["schema_version"] = json!(0)));
    assert!(matches!(
        store.get_character_progress(a, GameId::Ff7),
        Err(PersistError::InvalidSchema { found: 0, .. })
    ));
    put_raw(
        &root,
        a,
        "Ff7",
        &doc(|v| v["schema_version"] = json!(3)),
    );
    assert!(matches!(
        store.get_character_progress(a, GameId::Ff7),
        Err(PersistError::FutureSchema { found, supported, .. })
            if found == 3 && supported == 2
    ));
    // ... and an update refuses to build on such a record rather than overwrite it.
    let mut called = false;
    let err = store.update_character_progress(a, GameId::Ff7, |c| {
        called = true;
        c.unwrap()
    });
    assert!(
        matches!(err, Err(PersistError::FutureSchema { .. })),
        "{err:?}"
    );
    assert!(!called, "the closure never saw a record it could not read");

    // Size cap: a record over the cap is refused unread, a directory is not a record.
    let padding = " ".repeat(usize::try_from(MAX_CHARACTER_RECORD_BYTES).unwrap());
    let big = format!("{}{padding}", String::from_utf8(doc(|_| {})).unwrap());
    put_raw(&root, a, "Ff7", big.as_bytes());
    assert!(matches!(
        store.get_character_progress(a, GameId::Ff7),
        Err(PersistError::TooLarge { max, .. }) if max == MAX_CHARACTER_RECORD_BYTES
    ));
    let path = character_file(&root, b, "Ff7");
    fs::create_dir_all(&path).unwrap();
    assert!(matches!(
        store.get_character_progress(b, GameId::Ff7),
        Err(PersistError::Corrupt { .. })
    ));
}

// ---- the inventory -------------------------------------------------------------------------------

/// A record exactly as a build from before the inventory wrote it, and as the server that is
/// already running holds them: pretty JSON in the order of the struct's fields, a final newline,
/// no `inventory` key. Account 1, game Ff7. (Spelled with `\n` rather than as a multi-line
/// literal, so that a checkout with CRLF line endings does not change the bytes compared.)
const RECORD_BEFORE_THE_INVENTORY: &str = concat!(
    "{\n",
    "  \"schema_version\": 1,\n",
    "  \"account_id\": 1,\n",
    "  \"game\": \"Ff7\",\n",
    "  \"character\": 0,\n",
    "  \"level\": 5,\n",
    "  \"exp\": 100,\n",
    "  \"ap\": 1,\n",
    "  \"gil\": 2,\n",
    "  \"updated_at\": 3\n",
    "}\n",
);

/// The compatibility that matters most: the records that exist today have no `inventory` key.
/// They load as "never stored" (`None`, which is not an empty inventory), and one that is written
/// back without an inventory is, byte for byte, what it was.
#[test]
fn a_record_from_before_the_inventory_loads_without_one_and_is_written_back_unchanged() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    assert_eq!(a, AccountId(1));
    let expected = CharacterProgress {
        exp: 100,
        ap: 1,
        gil: 2,
        ..CharacterProgress::new(a, GameId::Ff7, 0, 5, 3)
    };

    // The text the store wrote, and the same fields as compact JSON (the `doc` helper).
    for (what, bytes) in [
        (
            "as the store wrote it",
            RECORD_BEFORE_THE_INVENTORY.as_bytes().to_vec(),
        ),
        ("compact", doc(|_| {})),
    ] {
        put_raw(&root, a, "Ff7", &bytes);
        let loaded = store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, expected, "{what}");
        assert_eq!(
            loaded.inventory, None,
            "{what}: never stored, not an empty inventory"
        );
    }

    // Putting back what was read writes the very bytes that were there: no `inventory` key
    // appears, not even an empty one.
    let path = put_raw(&root, a, "Ff7", RECORD_BEFORE_THE_INVENTORY.as_bytes());
    let loaded = store
        .get_character_progress(a, GameId::Ff7)
        .unwrap()
        .unwrap();
    store.put_character_progress(&loaded).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        RECORD_BEFORE_THE_INVENTORY
    );

    // An update on it keeps working, and one that does not set an inventory writes none.
    let mut seen = None;
    let updated = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            seen = cur.clone();
            let mut p = cur.expect("the record is there");
            p.exp += 50;
            p.gil += 5;
            p.updated_at += 1;
            p
        })
        .unwrap();
    assert_eq!(seen, Some(expected.clone()), "the closure saw it as it was");
    assert_eq!(
        (updated.exp, updated.gil, updated.updated_at),
        (150, 7, 4),
        "{updated:?}"
    );
    assert_eq!(updated.inventory, None);
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("inventory"), "{text}");
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(updated)
    );

    // A record that a server builds from scratch (no inventory) is written without the key too.
    store.put_character_progress(&expected).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        RECORD_BEFORE_THE_INVENTORY
    );
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

/// What a v6 build wrote for an account that had items (the case of the owner's real account on the
/// preview server): the `inventory` key, and no `limit` key, which did not exist yet.
const RECORD_OF_THE_V6_BUILD: &str = concat!(
    "{\n",
    "  \"schema_version\": 1,\n",
    "  \"account_id\": 1,\n",
    "  \"game\": \"Ff7\",\n",
    "  \"character\": 0,\n",
    "  \"level\": 7,\n",
    "  \"exp\": 626,\n",
    "  \"ap\": 2,\n",
    "  \"gil\": 10,\n",
    "  \"updated_at\": 3,\n",
    "  \"inventory\": [\n",
    "    {\n",
    "      \"item\": 0,\n",
    "      \"count\": 3\n",
    "    }\n",
    "  ]\n",
    "}\n",
);

/// The limit gauge was added to records that already exist: a record of the v6 build (with its items and
/// without the key) loads with an empty gauge, and written back without one it is, byte for byte, what it
/// was. A gauge is stored beside the items and the numbers, changing it touches nothing else, and a
/// record that is emptied again loses the key.
#[test]
fn a_record_from_the_v6_build_loads_with_an_empty_gauge_and_a_gauge_is_stored_beside_the_rest() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let expected = CharacterProgress {
        exp: 626,
        ap: 2,
        gil: 10,
        inventory: Some(stacks([(0, 3)])),
        ..CharacterProgress::new(a, GameId::Ff7, 0, 7, 3)
    };
    let path = put_raw(&root, a, "Ff7", RECORD_OF_THE_V6_BUILD.as_bytes());
    let loaded = store.get_character_progress(a, GameId::Ff7).unwrap().unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.limit, 0, "an empty gauge, which is also what a new character has");

    // Written back as it was read, the record is byte for byte the v6 one: no `limit` key appears.
    store.put_character_progress(&loaded).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), RECORD_OF_THE_V6_BUILD);

    // An update that stores a gauge changes the gauge and nothing else.
    let updated = store
        .update_character_progress(a, GameId::Ff7, |cur| CharacterProgress { limit: 200, ..cur.expect("the record is there") })
        .unwrap();
    assert_eq!(updated, CharacterProgress { limit: 200, ..expected.clone() });
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"limit\": 200") && text.contains("\"inventory\""), "{text}");
    assert_eq!(store.get_character_progress(a, GameId::Ff7).unwrap(), Some(updated.clone()));

    // The same record read by a build that does not know the key (its struct ignores it) is the v6 record
    // without the gauge: that build drops the gauge the next time it writes, and nothing else.
    #[derive(serde::Deserialize, serde::Serialize)]
    struct V6Record {
        schema_version: u32,
        account_id: AccountId,
        game: GameId,
        character: u8,
        level: u8,
        exp: u32,
        ap: u32,
        gil: u32,
        updated_at: u64,
        inventory: Option<Vec<InventoryEntry>>,
    }
    let v6: V6Record = serde_json::from_str(&text).expect("a v6 build reads a v7 record");
    assert_eq!((v6.level, v6.exp, v6.inventory.clone()), (7, 626, Some(stacks([(0, 3)]))));
    let rewritten = serde_json::to_string(&v6).unwrap();
    assert!(!rewritten.contains("limit"), "{rewritten}");
    let after_rollback: CharacterProgress = serde_json::from_str(&rewritten).unwrap();
    assert_eq!(after_rollback, expected, "back to an empty gauge, nothing else lost");

    // Emptying the gauge again writes the v6 bytes again.
    store
        .update_character_progress(a, GameId::Ff7, |cur| CharacterProgress { limit: 0, ..cur.expect("the record is there") })
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), RECORD_OF_THE_V6_BUILD);

    // A gauge that is no u16 is a damaged file, never clamped and never replaced by a default.
    for bad in [json!(70_000), json!(-1), json!("full")] {
        let bytes = doc(|v| v["limit"] = bad.clone());
        put_raw(&root, a, "Ff7", &bytes);
        let err = store.get_character_progress(a, GameId::Ff7).unwrap_err();
        assert!(matches!(err, PersistError::Corrupt { .. }), "{bad}: {err:?}");
    }
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

#[test]
fn an_inventory_round_trips_and_an_empty_one_stays_empty() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let path = character_file(&root, a, "Ff7");
    let text = || fs::read_to_string(&path).unwrap();
    let get = |store: &Store| {
        store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap()
    };

    // A stored inventory comes back as it went in, in the same order (the store never sorts).
    let items = stacks([(10, 3), (2, 99), (300, 1), (0, 65535)]);
    let with_items = carrying(record(a, GameId::Ff7, 0, 7, 1234), items.clone());
    store.put_character_progress(&with_items).unwrap();
    assert_eq!(get(&store), with_items);
    let on_disk = read_json(&path);
    assert_eq!(
        on_disk["inventory"],
        json!([
            {"item": 10, "count": 3},
            {"item": 2, "count": 99},
            {"item": 300, "count": 1},
            {"item": 0, "count": 65535}
        ])
    );
    assert_eq!(on_disk["exp"], 1234, "the other fields are where they were");

    // `Some(vec![])` is a stored, empty inventory: it stays empty on disk and after a restart,
    // and is not turned into "never stored".
    let empty = carrying(with_items.clone(), vec![]);
    store.put_character_progress(&empty).unwrap();
    assert!(text().contains("\"inventory\": []"), "{}", text());
    assert_eq!(get(&store).inventory, Some(vec![]));
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(get(&store), empty);

    // An update that leaves the inventory alone keeps it (empty here): it neither disappears
    // nor is it filled in.
    let touched = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            let mut p = cur.expect("stored");
            p.exp += 1;
            p
        })
        .unwrap();
    assert_eq!(touched.inventory, Some(vec![]));
    assert!(text().contains("\"inventory\": []"), "{}", text());

    // One that sets it replaces it, and the closure is handed what was stored.
    let mut seen = None;
    let refilled = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            seen = cur.as_ref().map(|p| p.inventory.clone());
            let mut p = cur.expect("stored");
            p.inventory = Some(items.clone());
            p
        })
        .unwrap();
    assert_eq!(seen, Some(Some(vec![])));
    assert_eq!(refilled.inventory, Some(items.clone()));
    assert_eq!(get(&store), refilled);

    // One that sets it back to `None` makes it "never stored" again: the key is gone, so the
    // game's starting stock applies once more.
    let cleared = store
        .update_character_progress(a, GameId::Ff7, |cur| CharacterProgress {
            inventory: None,
            ..cur.expect("stored")
        })
        .unwrap();
    assert_eq!(cleared.inventory, None);
    assert!(!text().contains("inventory"), "{}", text());
    assert_eq!(get(&store).inventory, None);

    // `put` replaces the whole record, the inventory included: a record built from scratch
    // clears a stored one. (To keep the items, change the record that was read.)
    store.put_character_progress(&with_items).unwrap();
    assert_eq!(get(&store).inventory, Some(items));
    store
        .put_character_progress(&record(a, GameId::Ff7, 0, 7, 1234))
        .unwrap();
    assert_eq!(get(&store).inventory, None);
    assert!(!text().contains("inventory"), "{}", text());
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

/// Two results for one account that arrive at once add up in the inventory too, not only in the
/// numbers: the inventory is part of the atomic read-modify-write.
#[test]
fn concurrent_updates_add_up_inventory_stacks_too() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    const THREADS: u16 = 4;
    const EACH: u16 = 25;

    let workers: Vec<_> = (0..THREADS)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for _ in 0..EACH {
                    store
                        .update_character_progress(a, GameId::Ff7, |cur| {
                            let mut p = cur.unwrap_or_else(|| record(a, GameId::Ff7, 0, 1, 0));
                            // One of item 7 (shared by every thread) and one of its own item.
                            let items = p.inventory.get_or_insert_with(Vec::new);
                            for item in [7, 100 + t] {
                                match items.iter_mut().find(|e| e.item == item) {
                                    Some(stack) => stack.count += 1,
                                    None => items.push(InventoryEntry::new(item, 1)),
                                }
                            }
                            p
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let mut items = store
        .get_character_progress(a, GameId::Ff7)
        .unwrap()
        .unwrap()
        .inventory
        .expect("an inventory was stored");
    let mut want = stacks([(7, THREADS * EACH)]);
    want.extend((0..THREADS).map(|t| InventoryEntry::new(100 + t, EACH)));
    // Which update created which stack is up to the scheduler; what each stack holds is not.
    items.sort();
    want.sort();
    assert_eq!(items, want);
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
}

/// The cap had to grow for the inventory (a full one is several KiB), and it must still do its
/// job: bound what a hostile file can make the store read.
#[test]
fn a_full_inventory_fits_the_record_cap_and_a_record_over_it_is_still_refused() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let path = character_file(&root, a, "Ff7");
    let cap = usize::try_from(MAX_CHARACTER_RECORD_BYTES).unwrap();

    // The biggest record there can be: every one of the 320 stacks, five digits in each number.
    let widest =
        stacks((0..MAX_INVENTORY_STACKS).map(|i| (u16::MAX - u16::try_from(i).unwrap(), u16::MAX)));
    assert_eq!(widest.len(), MAX_INVENTORY_STACKS);
    let full = carrying(derived(a, GameId::Ff7, 1), widest.clone());
    store.put_character_progress(&full).unwrap();
    let size = fs::metadata(&path).unwrap().len();
    // More than the 4 KiB that the cap was before inventories (that is why it grew), and under
    // the cap with room to spare.
    assert!(size > 4 * 1024, "{size}");
    assert!(size * 3 / 2 < MAX_CHARACTER_RECORD_BYTES, "{size}");
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(full.clone())
    );

    // An update of the full record reads it, changes it and writes it back. It changes something
    // (the order of the stacks and the experience), so the write is a real one.
    let changed = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            let mut p = cur.expect("the full record is there");
            p.exp += 7;
            p.inventory.as_mut().expect("with its inventory").reverse();
            p
        })
        .unwrap();
    let reversed: Vec<_> = widest.iter().rev().copied().collect();
    assert_eq!(changed.exp, full.exp + 7);
    assert_eq!(changed.inventory, Some(reversed));
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(changed.clone())
    );
    assert_eq!(has_temp_files(&root), Vec::<String>::new());

    // The same after a restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(changed.clone())
    );

    // The cap is exact and a file over it is refused unread: this very record padded with spaces
    // (JSON ignores them, so only the size can matter) is read at exactly the cap and refused
    // one byte over it. Neither an update nor a read replaces it.
    let record_bytes = fs::read(&path).unwrap();
    let padded_to = |len: usize| {
        let mut bytes = record_bytes.clone();
        bytes.resize(len, b' ');
        bytes
    };
    put_raw(&root, a, "Ff7", &padded_to(cap));
    assert_eq!(
        store.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(changed)
    );
    put_raw(&root, a, "Ff7", &padded_to(cap + 1));
    assert!(matches!(
        store.get_character_progress(a, GameId::Ff7),
        Err(PersistError::TooLarge { size, max, .. })
            if size == MAX_CHARACTER_RECORD_BYTES + 1 && max == MAX_CHARACTER_RECORD_BYTES
    ));
    let mut called = false;
    let err = store.update_character_progress(a, GameId::Ff7, |c| {
        called = true;
        c.unwrap()
    });
    assert!(matches!(err, Err(PersistError::TooLarge { .. })), "{err:?}");
    assert!(!called, "the closure never saw a record it could not read");
    assert_eq!(fs::read(&path).unwrap(), padded_to(cap + 1));
}

/// Everything that can be wrong with an inventory in a file that somebody edited (or made):
/// the record is `Corrupt`, no update builds on it, and it is never replaced by a default.
#[test]
fn a_damaged_inventory_makes_the_record_corrupt_and_is_never_replaced() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    assert_eq!(a, AccountId(1));
    let list = |n: usize| Value::Array((0..n).map(|i| json!({"item": i, "count": 1})).collect());

    // Not in this list: a JSON null (it reads as `None`, see the end of this test) and a
    // tuple-shaped entry such as `[3, 99]`, which serde accepts as an entry. That is lenient, but
    // all it can produce is a well-formed entry, and the rules below apply to it as to any other.
    let damaged: Vec<(&str, Value)> = vec![
        // Rules about the list.
        (
            "an item twice",
            json!([{"item": 3, "count": 1}, {"item": 3, "count": 2}]),
        ),
        (
            "an item twice, apart",
            json!([{"item": 3, "count": 1}, {"item": 4, "count": 1}, {"item": 3, "count": 1}]),
        ),
        ("a stack of zero", json!([{"item": 3, "count": 0}])),
        ("one stack too many", list(MAX_INVENTORY_STACKS + 1)),
        // Wrong JSON types.
        ("a string for the list", json!("potion")),
        ("a number for the list", json!(5)),
        ("a bool for the list", json!(true)),
        ("an object for the list", json!({"item": 1, "count": 1})),
        ("a number for an entry", json!([7])),
        ("a null entry", json!([null])),
        ("a string item", json!([{"item": "3", "count": 1}])),
        ("a float item", json!([{"item": 1.5, "count": 1}])),
        ("a string count", json!([{"item": 1, "count": "2"}])),
        ("a float count", json!([{"item": 1, "count": 2.5}])),
        ("a null count", json!([{"item": 1, "count": null}])),
        ("a missing count", json!([{"item": 1}])),
        ("a missing item", json!([{"count": 1}])),
        // Out of range for the 16-bit fields.
        ("a count of 65536", json!([{"item": 1, "count": 65536}])),
        ("an item of 65536", json!([{"item": 65536, "count": 1}])),
        ("a negative count", json!([{"item": 1, "count": -1}])),
        ("a negative item", json!([{"item": -1, "count": 1}])),
    ];
    for (what, inventory) in &damaged {
        let bytes = doc(|v| v["inventory"] = inventory.clone());
        let path = put_raw(&root, a, "Ff7", &bytes);
        match store.get_character_progress(a, GameId::Ff7) {
            Err(PersistError::Corrupt { .. }) => {}
            other => panic!("{what}: expected Corrupt, got {other:?}"),
        }
        // An update does not build on it either, and nothing replaced the damaged file: an
        // operator can still look at it.
        let mut called = false;
        let err = store.update_character_progress(a, GameId::Ff7, |c| {
            called = true;
            c.unwrap()
        });
        assert!(
            matches!(err, Err(PersistError::Corrupt { .. })),
            "{what}: {err:?}"
        );
        assert!(
            !called,
            "{what}: the closure saw a record it could not read"
        );
        assert_eq!(&fs::read(&path).unwrap(), &bytes, "{what}");
    }
    assert_eq!(has_temp_files(&root), Vec::<String>::new());

    // The operator is told what is wrong, and with which file.
    for (inventory, wants) in [
        (
            json!([{"item": 3, "count": 1}, {"item": 3, "count": 2}]),
            "item 3 appears more than once",
        ),
        (
            json!([{"item": 4, "count": 0}]),
            "item 4 has a stack of zero",
        ),
        (list(MAX_INVENTORY_STACKS + 1), "has 321 stacks"),
    ] {
        put_raw(&root, a, "Ff7", &doc(|v| v["inventory"] = inventory));
        match store.get_character_progress(a, GameId::Ff7) {
            Err(PersistError::Corrupt { path, reason }) => {
                assert!(path.ends_with("Ff7.character.json"), "{path:?}");
                assert!(
                    reason.contains("inventory") && reason.contains(wants),
                    "{reason}"
                );
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    // What is fine: nothing stored (a JSON null reads like a missing key), a stored empty list,
    // a short one, and a full one.
    for (what, inventory, expected) in [
        ("null", json!(null), None),
        ("an empty list", json!([]), Some(vec![])),
        (
            "a short list",
            json!([{"item": 3, "count": 2}, {"item": 1, "count": 65535}]),
            Some(stacks([(3, 2), (1, 65535)])),
        ),
        (
            "exactly the limit",
            list(MAX_INVENTORY_STACKS),
            Some(stacks(
                (0..MAX_INVENTORY_STACKS).map(|i| (u16::try_from(i).unwrap(), 1)),
            )),
        ),
    ] {
        put_raw(&root, a, "Ff7", &doc(|v| v["inventory"] = inventory));
        let read = store
            .get_character_progress(a, GameId::Ff7)
            .unwrap_or_else(|e| panic!("{what}: {e}"))
            .unwrap();
        assert_eq!(read.inventory, expected, "{what}");
    }
}

/// What a caller can get wrong through the API. Only a list that a `Vec<InventoryEntry>` can
/// hold gets here: an item or count outside 0..=65535 cannot even be built (they are 16-bit),
/// and a list of the wrong JSON type is not a list; those are the damaged files above.
#[test]
fn a_bad_inventory_is_refused_on_put_and_update_and_the_old_record_stays() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let path = character_file(&root, a, "Ff7");
    let account_dir = root.join("progression").join(a.0.to_string());
    let over_the_limit = u16::try_from(MAX_INVENTORY_STACKS).unwrap();

    let bad_lists: Vec<(&str, Vec<InventoryEntry>)> = vec![
        ("an item twice", stacks([(3, 1), (3, 2)])),
        ("an item twice, apart", stacks([(3, 1), (4, 1), (3, 1)])),
        ("a stack of zero", stacks([(3, 0)])),
        (
            "one stack too many",
            stacks((0..=over_the_limit).map(|item| (item, 1))),
        ),
        ("a huge list", vec![InventoryEntry::new(1, 1); 100_000]),
    ];
    // Both ways of writing refuse `bad` as an inventory problem and leave the file as it was
    // (`before`: its bytes, or `None` if there was no file and no trace of one may appear).
    let refused = |what: &str, bad: &CharacterProgress, before: Option<&[u8]>| {
        let put = store.put_character_progress(bad);
        assert!(
            matches!(
                put,
                Err(PersistError::InvalidField {
                    field: "inventory",
                    ..
                })
            ),
            "{what}: put gave {put:?}"
        );
        let mut seen = None;
        let update = store.update_character_progress(a, GameId::Ff7, |cur| {
            seen = Some(cur);
            bad.clone()
        });
        assert!(
            matches!(
                update,
                Err(PersistError::InvalidField {
                    field: "inventory",
                    ..
                })
            ),
            "{what}: update gave {update:?}"
        );
        assert!(seen.is_some(), "{what}: the update ran its closure");
        match before {
            Some(bytes) => assert_eq!(&fs::read(&path).unwrap(), bytes, "{what}"),
            None => assert!(
                !account_dir.exists(),
                "{what}: a refused first write leaves no trace"
            ),
        }
        assert_eq!(has_temp_files(&root), Vec::<String>::new(), "{what}");
    };

    // Nothing stored yet: nothing may appear.
    for (what, items) in &bad_lists {
        let bad = carrying(record(a, GameId::Ff7, 0, 5, 100), items.clone());
        refused(what, &bad, None);
    }

    // With a good record stored: it stays, byte for byte, whatever else the bad one changed.
    let good = carrying(
        record(a, GameId::Ff7, 0, 5, 100),
        stacks([(10, 3), (2, 99)]),
    );
    store.put_character_progress(&good).unwrap();
    let stored = fs::read(&path).unwrap();
    for (what, items) in &bad_lists {
        let bad = carrying(
            CharacterProgress {
                exp: 999_999,
                level: 50,
                ..good.clone()
            },
            items.clone(),
        );
        refused(what, &bad, Some(&stored));
        assert_eq!(
            store.get_character_progress(a, GameId::Ff7).unwrap(),
            Some(good.clone()),
            "{what}"
        );
    }

    // The way a server really gets here: it picks up an item that the account has already and
    // adds a second stack instead of adding to the count. The store refuses; the server merges.
    let err = store.update_character_progress(a, GameId::Ff7, |cur| {
        let mut p = cur.expect("stored");
        p.inventory
            .as_mut()
            .expect("with an inventory")
            .push(InventoryEntry::new(10, 1));
        p
    });
    assert!(
        matches!(&err, Err(PersistError::InvalidField { field: "inventory", reason })
            if reason.contains("item 10")),
        "{err:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), stored);

    // Merged, it is fine.
    let merged = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            let mut p = cur.expect("stored");
            for stack in p.inventory.as_mut().expect("with an inventory") {
                if stack.item == 10 {
                    stack.count += 1;
                }
            }
            p
        })
        .unwrap();
    assert_eq!(merged.inventory, Some(stacks([(10, 4), (2, 99)])));
}

#[test]
fn snapshots_carry_the_inventory_and_restore_it() {
    let (guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();
    let ff7 = carrying(
        record(a, GameId::Ff7, 0, 8, 949),
        stacks([(10, 3), (2, 99), (300, 1)]),
    );
    // Stored and empty, and never stored: the copy must tell them apart, like the original.
    let ff8 = carrying(record(a, GameId::Ff8, 1, 3, 12), vec![]);
    let beta = record(b, GameId::Ff7, 4, 10, 2000);
    for r in [&ff7, &ff8, &beta] {
        store.put_character_progress(r).unwrap();
    }

    let snap = guard.path().join("snap");
    store.snapshot(&snap).unwrap();
    for (id, game) in [(a, "Ff7"), (a, "Ff8"), (b, "Ff7")] {
        let rel = format!("progression/{}/{game}.character.json", id.0);
        assert_eq!(
            fs::read(snap.join(&rel)).unwrap(),
            fs::read(root.join(&rel)).unwrap(),
            "{rel}"
        );
    }
    let text = |dir: &Path, id: AccountId, game: &str| {
        fs::read_to_string(character_file(dir, id, game)).unwrap()
    };
    assert!(text(&snap, a, "Ff7").contains("\"item\": 300"));
    assert!(text(&snap, a, "Ff8").contains("\"inventory\": []"));
    assert!(!text(&snap, b, "Ff7").contains("inventory"));
    assert_eq!(has_temp_files(&snap), Vec::<String>::new());

    // Inventories changed after the snapshot are not in it: restoring gives the old ones back.
    let set = |id: AccountId, game: GameId, inventory: Option<Vec<InventoryEntry>>| {
        store
            .update_character_progress(id, game, |cur| CharacterProgress {
                inventory,
                ..cur.expect("stored")
            })
            .unwrap();
    };
    set(a, GameId::Ff7, Some(stacks([(99, 1)])));
    set(a, GameId::Ff8, None);
    set(b, GameId::Ff7, Some(stacks([(1, 1)])));
    drop(store);

    let restored = guard.path().join("restored");
    restore_snapshot(&snap, &restored).unwrap();
    let copy = open(&restored);
    assert!(
        copy.open_warnings().is_empty(),
        "{:?}",
        copy.open_warnings()
    );
    assert_eq!(
        copy.get_character_progress(a, GameId::Ff7).unwrap(),
        Some(ff7)
    );
    assert_eq!(
        copy.get_character_progress(a, GameId::Ff8).unwrap(),
        Some(ff8),
        "stored and empty stays empty, it does not become never stored"
    );
    assert_eq!(
        copy.get_character_progress(b, GameId::Ff7).unwrap(),
        Some(beta),
        "never stored stays never stored"
    );
}

/// The story blob is also the phase key: changing what an account carries must neither touch it
/// nor be reachable through it.
#[test]
fn changing_only_the_inventory_leaves_the_story_blob_alone() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let story = Progression::new(a, GameId::Ff7, vec![1, 2, 3], NOW);
    store.put_progression(&story).unwrap();
    let before_record = record(a, GameId::Ff7, 0, 9, 800);
    store.put_character_progress(&before_record).unwrap();
    let story_bytes = fs::read(progression_file(&root, a, "Ff7")).unwrap();

    // Only the inventory changes: exactly the character file is rewritten, and the story file is
    // the same bytes.
    let items = stacks([(5, 2), (6, 1)]);
    let before = file_map(&root);
    let updated = store
        .update_character_progress(a, GameId::Ff7, |cur| {
            carrying(cur.expect("stored"), items.clone())
        })
        .unwrap();
    let after = file_map(&root);
    let changed: Vec<_> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(changed, [format!("progression/{}/Ff7.character.json", a.0)]);
    assert_eq!(
        fs::read(progression_file(&root, a, "Ff7")).unwrap(),
        story_bytes
    );
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), story);
    // Nothing else of the record moved either.
    assert_eq!(updated, carrying(before_record, items.clone()));
    // The items are not in the story's file.
    assert!(!String::from_utf8(story_bytes)
        .unwrap()
        .contains("inventory"));

    // The other way round: a new story leaves the items as they are.
    store
        .put_progression(&Progression::new(a, GameId::Ff7, vec![9, 9], NOW + 1))
        .unwrap();
    assert_eq!(
        store
            .get_character_progress(a, GameId::Ff7)
            .unwrap()
            .unwrap()
            .inventory,
        Some(items)
    );
}
