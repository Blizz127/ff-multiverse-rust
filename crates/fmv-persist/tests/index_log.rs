//! The index log: migration of a version 1 `index.json`, torn writes, compaction, and what a
//! snapshot of each shape does. Version 1 directories are built by hand from the documented
//! format (see `common::write_v1_dir`), because the code that wrote them is gone.

mod common;

use std::fs;

use common::*;
use fmv_core::{AccountId, GameId};
use fmv_persist::{
    restore_snapshot, AvatarId, PersistError, Progression, Store, INDEX_LOG_VERSION, SCHEMA_VERSION,
};
use serde_json::json;

fn accounts_1_2_3() -> Vec<V1Account> {
    vec![
        V1Account::new(1, "Cloud", Some(1)),
        V1Account::new(2, "Tifa", None),
        V1Account::new(3, "\u{00C4}rger", Some(2)),
    ]
}

// ---- migration ---------------------------------------------------------------------------------

#[test]
fn a_version_1_directory_is_migrated_transparently_on_open() {
    let (_guard, root) = fresh_root();
    let accounts = accounts_1_2_3();
    let original = write_v1_dir(&root, &accounts, 4, 3);

    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("migrated") && w.contains("3 accounts")),
        "{:?}",
        store.open_warnings()
    );

    // The old index is kept byte for byte (it was renamed, not rewritten).
    assert_eq!(fs::read(root.join("index.json.v1.bak")).unwrap(), original);
    // `index.json` is now the format marker: it says version 2, which a build that only knows
    // version 1 refuses (FutureSchema) instead of using a stale index and reusing ids.
    let marker = read_json(&root.join("index.json"));
    assert_eq!(marker["schema_version"], INDEX_LOG_VERSION);
    assert!(marker["schema_version"].as_u64().unwrap() > u64::from(SCHEMA_VERSION));
    assert_eq!(marker["index"], "index.log");
    assert!(index_log(&root).is_file());

    // Everything the old index knew works, including lookups that need the re-derived keys.
    for a in &accounts {
        assert_eq!(store.authenticate(&a.token).unwrap(), AccountId(a.id));
    }
    assert_eq!(store.find_account_by_name("TIFA"), Some(AccountId(2)));
    assert_eq!(
        store.find_account_by_name("A\u{0308}RGER"),
        Some(AccountId(3)),
        "keys are derived again from the display names"
    );
    assert_eq!(
        store.find_account_by_name("\u{FF34}\u{FF49}\u{FF46}\u{FF41}"),
        Some(AccountId(2))
    );
    assert_eq!(
        store
            .account_info(AccountId(1))
            .unwrap()
            .unwrap()
            .created_at,
        NOW + 1
    );
    assert_eq!(
        store
            .avatar_for_account(AccountId(1))
            .unwrap()
            .unwrap()
            .avatar_id,
        AvatarId(1)
    );
    assert_eq!(
        store
            .avatar_for_account(AccountId(3))
            .unwrap()
            .unwrap()
            .avatar_id,
        AvatarId(2)
    );
    assert!(store.avatar_for_account(AccountId(2)).unwrap().is_none());
    assert_eq!(store.account_count(), 3);
    // There is no telling whether they logged in, so none of them is ever offered for release.
    assert!(store.unclaimed_accounts().is_empty());
    assert!(matches!(
        store.release_unclaimed(AccountId(1)),
        Err(PersistError::NotReleasable { .. })
    ));

    // Ids continue where the old index stopped: nothing is reused.
    let reg = store
        .register("Barret", [0; 3], "default", None, NOW)
        .unwrap();
    assert_eq!(
        (reg.account_id, reg.avatar.avatar_id),
        (AccountId(4), AvatarId(3))
    );
    let late = store
        .create_avatar(AccountId(2), [1, 1, 1], "default", NOW)
        .unwrap();
    assert_eq!(late.avatar_id, AvatarId(4));
    // Avatars written by the old build update normally.
    let mut avatar = store.avatar_for_account(AccountId(1)).unwrap().unwrap();
    avatar.tint = [8, 8, 8];
    store.put_avatar(&avatar).unwrap();
    store
        .put_progression(&Progression::new(AccountId(1), GameId::Ff7, vec![1], NOW))
        .unwrap();

    // A second open is quiet and sees the same world.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 4);
    assert_eq!(store.authenticate(&reg.token).unwrap(), AccountId(4));
    assert_eq!(
        store.authenticate(&accounts[0].token).unwrap(),
        AccountId(1)
    );
    assert_eq!(
        fs::read(root.join("index.json.v1.bak")).unwrap(),
        original,
        "the backup is never touched again"
    );
}

#[test]
fn migration_derives_the_keys_again_and_the_older_account_keeps_a_colliding_name() {
    let (_guard, root) = fresh_root();
    // Version 1 compared names by lower case only, so it accepted both of these. Under the
    // current rule they are the same name.
    let accounts = vec![
        V1Account::new(1, "Tifa", Some(1)),
        V1Account::new(2, "Tifa\u{3164}", Some(2)),
        V1Account::new(3, "\u{FF2D}\u{FF4F}\u{FF4F}\u{FF4E}", Some(3)),
    ];
    write_v1_dir(&root, &accounts, 4, 4);

    let store = open(&root);
    let warnings = store.open_warnings().join("\n");
    assert!(
        warnings.contains("look-alike") && warnings.contains("2 (name held by 1)"),
        "{warnings}"
    );
    // Both accounts still log in, and each keeps its own avatar.
    assert_eq!(
        store.authenticate(&accounts[0].token).unwrap(),
        AccountId(1)
    );
    assert_eq!(
        store.authenticate(&accounts[1].token).unwrap(),
        AccountId(2)
    );
    // The lower id holds the name, for lookups and for new registrations.
    assert_eq!(store.find_account_by_name("Tifa"), Some(AccountId(1)));
    assert_eq!(
        store.find_account_by_name("Tifa\u{3164}"),
        Some(AccountId(1))
    );
    assert!(matches!(
        store.register("tifa", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));
    // The account that lost the name collision can still change its avatar: its own name is
    // what is compared, not the name table.
    let mut loser = store.avatar_for_account(AccountId(2)).unwrap().unwrap();
    loser.tint = [5, 5, 5];
    store.put_avatar(&loser).unwrap();
    assert_eq!(
        store
            .avatar_for_account(AccountId(2))
            .unwrap()
            .unwrap()
            .tint,
        [5, 5, 5]
    );
    // And a name that merely looks different in v1 terms (fullwidth) is found.
    assert_eq!(store.find_account_by_name("Moon"), Some(AccountId(3)));

    // The collision is derived afresh on every open, so it stays visible until it is resolved.
    drop(store);
    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .join("\n")
            .contains("2 (name held by 1)"),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.find_account_by_name("Tifa"), Some(AccountId(1)));
}

#[test]
fn an_interrupted_migration_is_finished_by_the_next_open() {
    // (a) The log was renamed into place but the old index was not moved away yet.
    let (_guard, root) = fresh_root();
    let original = write_v1_dir(&root, &accounts_1_2_3(), 4, 3);
    drop(open(&root));
    let marker = fs::read(root.join("index.json")).unwrap();
    // Register something after the migration, so that the log is provably the newer truth.
    let store = open(&root);
    let newer = store
        .register("Newer", [0; 3], "default", None, NOW)
        .unwrap();
    drop(store);
    // Put the old index back where it was (as if the crash came before the rename).
    fs::write(root.join("index.json"), &original).unwrap();
    let store = open(&root);
    let warnings = store.open_warnings().join("\n");
    assert!(
        warnings.contains("version 1 index.json was found next to index.log"),
        "{warnings}"
    );
    assert_eq!(
        store.authenticate(&newer.token).unwrap(),
        newer.account_id,
        "the log stays authoritative"
    );
    assert_eq!(store.account_count(), 4);
    drop(store);
    // The leftover was moved aside without overwriting the first backup, and the marker is back.
    assert_eq!(fs::read(root.join("index.json.v1.bak")).unwrap(), original);
    assert_eq!(
        fs::read(root.join("index.json.v1.bak.1")).unwrap(),
        original
    );
    assert_eq!(fs::read(root.join("index.json")).unwrap(), marker);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    drop(store);

    // (b) The old index was moved away but the marker was not written yet: quiet repair.
    fs::remove_file(root.join("index.json")).unwrap();
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        read_json(&root.join("index.json"))["schema_version"],
        INDEX_LOG_VERSION
    );
    drop(store);

    // (c) A marker that cannot be read is moved aside, with a warning; the log is still the index.
    fs::write(root.join("index.json"), b"{ not json").unwrap();
    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("could not be read")),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 4);
    assert_eq!(
        read_json(&root.join("index.json"))["schema_version"],
        INDEX_LOG_VERSION
    );
    drop(store);

    // (d) A crash while the new log was being written leaves only a temp file: it is swept and
    // the migration runs again from the untouched old index.
    let (_guard, root) = fresh_root();
    write_v1_dir(&root, &accounts_1_2_3(), 4, 3);
    fs::write(root.join(".index.log.tmp-1-1"), b"half a log").unwrap();
    let store = open(&root);
    let warnings = store.open_warnings().join("\n");
    assert!(
        warnings.contains("removed 1 temp file") && warnings.contains("migrated"),
        "{warnings}"
    );
    assert_eq!(store.account_count(), 3);
}

#[test]
fn migration_copes_with_unreadable_records_and_refuses_what_it_cannot_understand() {
    // A missing record only costs the display name, which the old index still has (lower case).
    let (_guard, root) = fresh_root();
    let accounts = accounts_1_2_3();
    write_v1_dir(&root, &accounts, 4, 3);
    fs::remove_file(account_file(&root, AccountId(2))).unwrap();
    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("could not be read") && w.contains('2')),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.authenticate(&accounts[1].token).unwrap(),
        AccountId(2)
    );
    assert_eq!(store.find_account_by_name("TIFA"), Some(AccountId(2)));
    assert!(
        store.account_info(AccountId(2)).is_err(),
        "the record is still missing; that is reported where it was before"
    );
    drop(store);

    // An account whose token entry is missing from the old index takes the hash from its record.
    let (_guard, root) = fresh_root();
    let accounts = accounts_1_2_3();
    write_v1_dir(&root, &accounts, 4, 3);
    let mut index = read_json(&root.join("index.json"));
    index["tokens"]
        .as_object_mut()
        .unwrap()
        .remove(&hash_of(&accounts[0].token));
    write_json(&root.join("index.json"), &index);
    let store = open(&root);
    assert_eq!(
        store.authenticate(&accounts[0].token).unwrap(),
        AccountId(1)
    );
    drop(store);

    // No token anywhere: refuse, and release the lock.
    let (_guard, root) = fresh_root();
    write_v1_dir(&root, &accounts, 4, 3);
    let mut index = read_json(&root.join("index.json"));
    index["tokens"]
        .as_object_mut()
        .unwrap()
        .remove(&hash_of(&accounts[0].token));
    write_json(&root.join("index.json"), &index);
    fs::remove_file(account_file(&root, AccountId(1))).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));
    assert!(!root.join("LOCK").exists());
    assert!(
        root.join("index.json").exists(),
        "a refused migration leaves the old index alone"
    );
    assert!(!index_log(&root).exists());

    // An account with two tokens cannot be represented: refuse rather than drop one.
    let (_guard, root) = fresh_root();
    write_v1_dir(&root, &accounts, 4, 3);
    let mut index = read_json(&root.join("index.json"));
    index["tokens"]
        .as_object_mut()
        .unwrap()
        .insert(hash_of("second token"), json!(1));
    write_json(&root.join("index.json"), &index);
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));

    // A record from a newer build is not papered over.
    let (_guard, root) = fresh_root();
    write_v1_dir(&root, &accounts, 4, 3);
    let mut record = read_json(&account_file(&root, AccountId(1)));
    record["schema_version"] = json!(9);
    write_json(&account_file(&root, AccountId(1)), &record);
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::FutureSchema { .. })
    ));
    assert!(!root.join("LOCK").exists());

    // A damaged old index is an error, as it always was.
    let (_guard, root) = fresh_root();
    write_v1_dir(&root, &accounts, 4, 3);
    fs::write(
        root.join("index.json"),
        br#"{"schema_version":1,"next_account_id":0,"next_avatar_id":1}"#,
    )
    .unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));
}

#[test]
fn a_snapshot_taken_before_the_log_existed_restores_and_migrates() {
    let (guard, snapshot) = fresh_root();
    let original = write_v1_dir(&snapshot, &accounts_1_2_3(), 4, 3);
    let target = guard.path().join("restored");
    let info = restore_snapshot(&snapshot, &target).unwrap();
    // 3 account records, 2 avatars and the old index.
    assert_eq!(info.files, 3 + 2 + 1, "{info:?}");
    assert_eq!(fs::read(target.join("index.json")).unwrap(), original);
    assert!(!index_log(&target).exists());
    let store = open(&target);
    assert!(store.open_warnings().iter().any(|w| w.contains("migrated")));
    assert_eq!(store.account_count(), 3);
    // The source snapshot is untouched.
    assert_eq!(fs::read(snapshot.join("index.json")).unwrap(), original);
    assert!(!index_log(&snapshot).exists());
}

#[test]
fn snapshots_of_a_migrated_directory_carry_the_log_and_the_marker() {
    let (guard, root) = fresh_root();
    write_v1_dir(&root, &accounts_1_2_3(), 4, 3);
    let store = open(&root);
    let snap = guard.path().join("snap");
    let info = store.snapshot(&snap).unwrap();
    let files = file_map(&snap);
    assert!(files.contains_key("index.log") && files.contains_key("index.json"));
    assert!(
        !files.contains_key("index.json.v1.bak"),
        "the migration backup is not part of the state"
    );
    assert_eq!(info.files, files.len());
    assert_eq!(files["index.log"], fs::read(index_log(&root)).unwrap());
    // The snapshot opens on its own, quietly.
    let copy = open(&snap);
    assert!(
        copy.open_warnings().is_empty(),
        "{:?}",
        copy.open_warnings()
    );
    assert_eq!(copy.account_count(), 3);
    drop(copy); // a directory that holds a LOCK is a state directory, not a snapshot
    let restored = guard.path().join("restored");
    restore_snapshot(&snap, &restored).unwrap();
    assert_eq!(file_map(&restored), files);
}

// ---- torn writes -------------------------------------------------------------------------------

/// A valid entry for a registration that was never committed, as the bytes a crashed append
/// could have left behind.
fn uncommitted_entry() -> String {
    log_line(&account_entry(4, "ghost", &hash_of("ghost-token")))
}

#[test]
fn a_torn_last_entry_is_dropped_and_the_log_goes_on() {
    let entry = uncommitted_entry();
    let flipped = {
        let mut bytes = entry.clone().into_bytes();
        bytes[30] ^= 0x01; // a complete line whose checksum no longer matches
        bytes
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("half an entry", entry.as_bytes()[..40].to_vec()),
        (
            "an entry without its newline",
            entry.trim_end().as_bytes().to_vec(),
        ),
        ("a complete line with a wrong checksum", flipped),
        ("a block of zeros", vec![0u8; 4096]),
        ("a lone checksum", b"deadbeef".to_vec()),
        ("a few bytes", b"ab".to_vec()),
    ];
    for (label, torn) in cases {
        let (_guard, root) = fresh_root();
        let store = open(&root);
        let regs: Vec<_> = (1..=3)
            .map(|n| {
                store
                    .register(&format!("player{n}"), [0; 3], "default", None, NOW)
                    .unwrap()
            })
            .collect();
        drop(store);
        let committed = fs::read(index_log(&root)).unwrap();
        let mut damaged = committed.clone();
        damaged.extend_from_slice(&torn);
        fs::write(index_log(&root), &damaged).unwrap();

        let store = open(&root);
        let warnings = store.open_warnings().join("\n");
        assert!(
            warnings.contains(&format!("dropped {} byte(s)", torn.len())),
            "{label}: {warnings}"
        );
        // Everything that was committed is there; the torn registration is not.
        assert_eq!(store.account_count(), 3, "{label}");
        for reg in &regs {
            assert_eq!(
                store.authenticate(&reg.token).unwrap(),
                reg.account_id,
                "{label}"
            );
        }
        assert_eq!(store.find_account_by_name("ghost"), None, "{label}");
        // The tail was cut off before anything was appended, so the log is clean again.
        // (The logins above appended one claim entry each, right after the committed entries:
        // every line of the file verifies, so none of the torn bytes is left in the middle.)
        let after_claims = fs::read(index_log(&root)).unwrap();
        assert!(after_claims.starts_with(&committed), "{label}");
        assert_eq!(
            log_lines(&root).len(),
            committed.split(|&b| b == b'\n').count() - 1 + 3,
            "{label}"
        );
        // The id of the torn registration was never committed, so the next one gets it.
        let next = store
            .register("ghost", [0; 3], "default", None, NOW)
            .unwrap();
        assert_eq!(next.account_id, AccountId(4), "{label}");
        drop(store);
        let store = open(&root);
        assert!(
            store.open_warnings().is_empty(),
            "{label}: {:?}",
            store.open_warnings()
        );
        assert_eq!(store.account_count(), 4, "{label}");
        assert_eq!(
            store.authenticate(&next.token).unwrap(),
            AccountId(4),
            "{label}"
        );
        log_lines(&root); // every line verifies
    }
}

#[test]
fn damage_that_is_not_a_torn_tail_is_never_guessed_away() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    for n in 1..=3 {
        store
            .register(&format!("player{n}"), [0; 3], "default", None, NOW)
            .unwrap();
    }
    drop(store);
    let good = fs::read(index_log(&root)).unwrap();

    // A bad line with entries after it.
    let mut text = String::from_utf8(good.clone()).unwrap();
    let second_line = text.find('\n').unwrap() + 1;
    text.replace_range(second_line + 12..second_line + 13, "#");
    fs::write(index_log(&root), &text).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));
    assert!(!root.join("LOCK").exists());

    // A checksum-valid last line that this build does not understand (an entry kind of a newer
    // build) is not a torn write either: it was written whole, so it is never dropped silently.
    let mut text = String::from_utf8(good.clone()).unwrap();
    text.push_str(&log_line(&json!({"op": "from-the-future", "account": 1})));
    fs::write(index_log(&root), &text).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));

    // Neither is a repeated id.
    let mut text = String::from_utf8(good.clone()).unwrap();
    text.push_str(&log_line(&account_entry(3, "again", &hash_of("again"))));
    fs::write(index_log(&root), &text).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));

    // A tail of garbage far larger than any entry is corruption, not a torn write.
    let mut bytes = good.clone();
    bytes.extend(std::iter::repeat_n(b'x', 200_000));
    fs::write(index_log(&root), &bytes).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::Corrupt { .. })
    ));

    // And the untouched log is fine.
    fs::write(index_log(&root), &good).unwrap();
    let store = open(&root);
    assert!(store.open_warnings().is_empty());
    assert_eq!(store.account_count(), 3);
}

#[test]
fn restoring_a_snapshot_whose_log_is_damaged_is_refused() {
    let (guard, root) = fresh_root();
    let store = open(&root);
    store
        .register("player1", [0; 3], "default", None, NOW)
        .unwrap();
    let snap = guard.path().join("snap");
    store.snapshot(&snap).unwrap();
    // A snapshot is written whole, so even a torn final entry means it is damaged.
    let mut bytes = fs::read(index_log(&snap)).unwrap();
    bytes.extend_from_slice(b"1234");
    fs::write(index_log(&snap), bytes).unwrap();
    let target = guard.path().join("restored");
    assert!(matches!(
        restore_snapshot(&snap, &target),
        Err(PersistError::Corrupt { .. })
    ));
    assert!(!target.exists());
}

// ---- compaction ----------------------------------------------------------------------------------

#[test]
fn compact_index_drops_dead_entries_and_keeps_ids_and_logins() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let regs: Vec<_> = (1..=5)
        .map(|n| {
            store
                .register(
                    &format!("player{n}"),
                    [0; 3],
                    "default",
                    Some((GameId::Ff7, "m")),
                    NOW,
                )
                .unwrap()
        })
        .collect();
    store.authenticate(&regs[0].token).unwrap(); // account 1 has logged in
    store.release_unclaimed(regs[1].account_id).unwrap();
    store.release_unclaimed(regs[4].account_id).unwrap();
    let entries_before = log_lines(&root).len();
    assert!(entries_before >= 5 + 1 + 2);

    store.compact_index().unwrap();
    // One entry per live account, one per avatar link, then the id marks:
    // header + 3 accounts + 3 avatars + next.
    let lines = log_lines(&root);
    assert_eq!(lines.len(), 1 + 3 + 3 + 1, "{lines:?}");
    assert_eq!(
        lines.last().unwrap(),
        &json!({"op": "next", "account": 6, "avatar": 6})
    );
    assert_eq!(
        lines[1]["claimed"], true,
        "the claim was folded into the account entry"
    );
    assert!(lines[2].get("claimed").is_none());
    assert!(lines[1..=3]
        .iter()
        .all(|l| l["op"] == "account" && l.get("avatar").is_none()));
    assert_eq!(
        lines[4..=6].to_vec(),
        vec![
            json!({"op": "avatar", "account": 1, "avatar": 1}),
            json!({"op": "avatar", "account": 3, "avatar": 3}),
            json!({"op": "avatar", "account": 4, "avatar": 4}),
        ]
    );

    // The store keeps working on the new file: appends land in it, logins still resolve.
    let late = store
        .register("late", [0; 3], "default", None, NOW)
        .unwrap();
    assert_eq!(late.account_id, AccountId(6), "released ids stay retired");
    assert_eq!(log_lines(&root).len(), 1 + 3 + 3 + 1 + 1);
    assert_eq!(
        store.authenticate(&regs[2].token).unwrap(),
        regs[2].account_id
    );
    drop(store);

    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 4);
    assert_eq!(store.authenticate(&late.token).unwrap(), AccountId(6));
    assert!(matches!(
        store.authenticate(&regs[1].token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(store.find_account_by_name("player2"), None);
    // Claimed stays claimed across compaction and restart; unclaimed stays unclaimed.
    assert!(matches!(
        store.release_unclaimed(regs[0].account_id),
        Err(PersistError::NotReleasable { .. })
    ));
    assert!(store
        .unclaimed_accounts()
        .iter()
        .all(|(id, _)| *id != regs[0].account_id));
    // Ids 2 and 5 are retired for good, also after the log was rewritten.
    assert_eq!(
        store
            .register("after", [0; 3], "default", None, NOW)
            .unwrap()
            .account_id,
        AccountId(7)
    );
}

/// Avatar ids only grow along the log, but they need not grow with the account ids: two
/// registrations in flight finish in either order, and the server (two calls per registration,
/// from several threads) did exactly that. A log rewritten in account order with the avatars
/// inline would then no longer replay: the server could not restart.
#[test]
fn avatar_ids_that_do_not_grow_with_account_ids_survive_migration_and_restarts() {
    let (_guard, root) = fresh_root();
    let accounts = vec![
        V1Account::new(1, "Alpha", Some(3)),
        V1Account::new(2, "Beta", Some(1)),
        V1Account::new(3, "Gamma", Some(2)),
    ];
    write_v1_dir(&root, &accounts, 4, 4);

    let check = |store: &Store| {
        for a in &accounts {
            let avatar = store.avatar_for_account(AccountId(a.id)).unwrap().unwrap();
            assert_eq!(Some(avatar.avatar_id.0), a.avatar, "account {}", a.id);
        }
    };
    let store = open(&root); // migrates: writes the log
    check(&store);
    drop(store);
    // The log that migration wrote must replay on the next open.
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    check(&store);
    let reg = store
        .register("Delta", [0; 3], "default", None, NOW)
        .unwrap();
    assert_eq!(reg.avatar.avatar_id, AvatarId(4), "no avatar id is reused");
    // Compaction writes it again, and that must replay too.
    store.compact_index().unwrap();
    check(&store);
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    check(&store);
    assert_eq!(
        store
            .avatar_for_account(reg.account_id)
            .unwrap()
            .unwrap()
            .avatar_id,
        AvatarId(4)
    );
    assert_eq!(
        store
            .register("Epsilon", [0; 3], "default", None, NOW)
            .unwrap()
            .avatar
            .avatar_id,
        AvatarId(5)
    );
}

#[test]
fn interleaved_two_step_registrations_compact_and_reopen() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    // A creates its account, B creates its account, B gets its avatar first, then A.
    let (alpha, alpha_token) = store.create_account("Alpha", NOW).unwrap();
    let (beta, _) = store.create_account("Beta", NOW).unwrap();
    let beta_avatar = store.create_avatar(beta, [2; 3], "default", NOW).unwrap();
    let alpha_avatar = store.create_avatar(alpha, [1; 3], "default", NOW).unwrap();
    assert_eq!(
        (beta_avatar.avatar_id, alpha_avatar.avatar_id),
        (AvatarId(1), AvatarId(2))
    );
    drop(store);

    // The log as it happened replays...
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    store.compact_index().unwrap();
    drop(store);
    // ...and so does the compacted one, with every link where it was.
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.avatar_for_account(alpha).unwrap().unwrap(),
        alpha_avatar
    );
    assert_eq!(
        store.avatar_for_account(beta).unwrap().unwrap(),
        beta_avatar
    );
    assert_eq!(store.authenticate(&alpha_token).unwrap(), alpha);
    assert_eq!(
        store
            .register("Gamma", [0; 3], "default", None, NOW)
            .unwrap()
            .avatar
            .avatar_id,
        AvatarId(3)
    );
}

#[test]
fn compaction_at_open_keeps_avatar_links_that_are_out_of_account_order() {
    let (_guard, root) = fresh_root();
    fs::create_dir_all(&root).unwrap();
    let mut entries = vec![
        account_entry(1, "Alpha", &hash_of("alpha")),
        account_entry(2, "Beta", &hash_of("beta")),
        json!({"op": "avatar", "account": 2, "avatar": 1}),
        json!({"op": "avatar", "account": 1, "avatar": 2}),
    ];
    // Enough dead entries to make opening rewrite the log.
    for id in 3..2_503u64 {
        entries.push(account_entry(
            id,
            &format!("ghost{id}"),
            &hash_of(&format!("g{id}")),
        ));
        entries.push(json!({"op": "release", "account": id}));
    }
    fs::write(index_log(&root), log_text(&entries)).unwrap();

    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert!(log_lines(&root).len() < 10, "the log was compacted");
    drop(store);
    // The rewritten log replays.
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    let reg = store
        .register("Delta", [0; 3], "default", None, NOW)
        .unwrap();
    assert_eq!(
        (reg.account_id, reg.avatar.avatar_id),
        (AccountId(2_503), AvatarId(3))
    );
    let lines = log_lines(&root);
    assert!(
        lines
            .iter()
            .any(|l| l == &json!({"op": "avatar", "account": 1, "avatar": 2})),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == &json!({"op": "avatar", "account": 2, "avatar": 1})),
        "{lines:?}"
    );
}

#[test]
fn a_log_full_of_dead_entries_is_compacted_when_the_store_opens() {
    let (_guard, root) = fresh_root();
    fs::create_dir_all(&root).unwrap();
    // 3,000 accounts, each registered and released: 6,000 entries, none of them live.
    let mut entries = Vec::new();
    for id in 1..=3_000u64 {
        entries.push(account_entry(
            id,
            &format!("ghost{id}"),
            &hash_of(&format!("g{id}")),
        ));
        entries.push(json!({"op": "release", "account": id}));
    }
    entries.push(account_entry(3_001, "alive", &hash_of("alive-token")));
    fs::write(index_log(&root), log_text(&entries)).unwrap();
    let size_before = fs::metadata(index_log(&root)).unwrap().len();

    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "compaction is routine: {:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 1);
    let lines = log_lines(&root);
    assert_eq!(
        lines.len(),
        3,
        "header, the one live account, the id marks: {lines:?}"
    );
    assert_eq!(
        lines[2],
        json!({"op": "next", "account": 3_002, "avatar": 1})
    );
    assert!(fs::metadata(index_log(&root)).unwrap().len() < size_before / 100);
    // Retired ids stay retired, and the survivor still logs in.
    assert_eq!(
        store
            .register("fresh", [0; 3], "default", None, NOW)
            .unwrap()
            .account_id,
        AccountId(3_002)
    );
    assert_eq!(store.authenticate("alive-token").unwrap(), AccountId(3_001));
}

/// The server registers in two steps (account, then avatar) and every account logs in once, so
/// its log holds three entries per account. That is the ordinary shape, not garbage: opening it
/// must not rewrite it, however many accounts there are.
#[test]
fn a_log_written_by_two_step_registrations_is_not_rewritten_at_open() {
    let (_guard, root) = fresh_root();
    fs::create_dir_all(&root).unwrap();
    let accounts = 5_000u64;
    let mut entries = Vec::new();
    for id in 1..=accounts {
        entries.push(account_entry(
            id,
            &format!("player{id}"),
            &hash_of(&format!("t{id}")),
        ));
        entries.push(json!({"op": "avatar", "account": id, "avatar": id}));
        entries.push(json!({"op": "claim", "account": id}));
    }
    let text = log_text(&entries);
    fs::write(index_log(&root), &text).unwrap();

    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), accounts);
    drop(store);
    assert_eq!(
        fs::read_to_string(index_log(&root)).unwrap(),
        text,
        "opening rewrote an ordinary log"
    );
}

#[test]
fn an_ordinary_log_is_not_rewritten_by_opening_it() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    for n in 1..=50 {
        let reg = store
            .register(&format!("player{n}"), [0; 3], "default", None, NOW)
            .unwrap();
        store.authenticate(&reg.token).unwrap(); // one claim entry per account: 101 entries
    }
    drop(store);
    let before = fs::read(index_log(&root)).unwrap();
    let store = open(&root);
    assert!(store.open_warnings().is_empty());
    drop(store);
    assert_eq!(fs::read(index_log(&root)).unwrap(), before);
}

// ---- the marker ----------------------------------------------------------------------------------

#[test]
fn a_directory_without_a_marker_gets_one_quietly() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    store
        .register("player", [0; 3], "default", None, NOW)
        .unwrap();
    drop(store);
    fs::remove_file(root.join("index.json")).unwrap();
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 1);
    assert_eq!(
        read_json(&root.join("index.json"))["schema_version"],
        INDEX_LOG_VERSION
    );
}

#[test]
fn unreferenced_records_of_a_released_account_are_reported_but_never_deleted() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let reg = store
        .register("player", [0; 3], "default", None, NOW)
        .unwrap();
    store.release_unclaimed(reg.account_id).unwrap();
    drop(store);
    // A crash between the release entry and the clean-up leaves the record files behind.
    fs::write(account_file(&root, reg.account_id), b"{}").unwrap();
    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("not referenced by the index")),
        "{:?}",
        store.open_warnings()
    );
    assert!(
        account_file(&root, reg.account_id).exists(),
        "nothing is deleted at open"
    );
}
