//! `CharacterProgress`: the per-account, per-game record of the character an account plays and
//! what it earned (level, experience, AP, gil), through the store's public API and its on-disk
//! layout. It is a separate record from `Progression`, the story blob that is also a phase key;
//! these tests include that the two never touch each other.
//!
//! Crash consistency is argued like the rest of the store: one atomic file replace per write
//! (temp file, fsync, rename, fsync the directory), checked here with leftovers of an interrupted
//! write, concurrent readers that must never see a torn file, and a writer process that is
//! SIGKILLed in the middle of its updates.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use fmv_core::{AccountId, GameId};
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
    // The small record cap leaves plenty of room.
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
        &doc(|v| v["schema_version"] = json!(SCHEMA_VERSION + 1)),
    );
    assert!(matches!(
        store.get_character_progress(a, GameId::Ff7),
        Err(PersistError::FutureSchema { found, supported, .. })
            if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
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
