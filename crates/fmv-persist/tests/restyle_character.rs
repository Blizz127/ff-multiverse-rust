//! `Store::restyle_player_character`: the look of an existing account's character changes, and nothing else on the
//! disk does.
//!
//! The directories these tests restyle are built in the EXACT shape of a v7 server's state directory (an account
//! with an avatar profile that names a canon look, a character-progress record with every number a battle can
//! leave, a story blob, and NO `characters/` directory: `V7_*` below are the bytes of the files, each checked
//! against what this build's serializer writes for the same values) and of a v8 server's (the same, with the
//! record the character creator made). Every check is made on a **whole-tree snapshot** (every file's bytes and
//! every directory, by relative path; only the `LOCK` file, which is not data, is left out), so "nothing else
//! changed" means the index log, the format marker, the accounts, the avatars and the progression, all of them.
//! Everything works on temp directories.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    CharacterOrigin, CharacterProgress, GameCharacter, LegacyView, NewPlayerCharacter, PersistError, PlayerCharacter, ProgressRef, Progression,
    Restyle, RestyleAdded, RestyleError, Store, MAX_PLAYER_CHARACTER_BYTES,
};

// ---- the files of a v7 server, byte for byte ------------------------------------------------------------------

/// `avatars/1.json` as a v6 to v8 server wrote it for Blizz (who chose the Tifa look).
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

/// `progression/1/Ff7.character.json` as the v7 server saves it once a battle left a limit gauge.
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

/// `progression/1/Ff7.json`: the story blob, three bytes, stored as hex.
const V7_STORY: &str = r#"{
  "schema_version": 1,
  "account_id": 1,
  "game": "Ff7",
  "blob": "0102ff",
  "updated_at": 1700000100
}
"#;

const STORY_BLOB: [u8; 3] = [0x01, 0x02, 0xff];

fn v7_progress_record() -> CharacterProgress {
    CharacterProgress {
        exp: 17_820,
        ap: 330,
        gil: 9_120,
        inventory: Some(vec![InventoryEntry::new(0, 9), InventoryEntry::new(3, 4)]),
        limit: 137,
        ..CharacterProgress::new(AccountId(1), GameId::Ff7, 2, 20, 1_793_600_000)
    }
}

fn pretty<T: serde::Serialize>(value: &T) -> String {
    format!("{}\n", serde_json::to_string_pretty(value).unwrap())
}

fn character_file(root: &Path, id: u64) -> PathBuf {
    root.join("characters").join(format!("{id}.json"))
}

fn progress_file(root: &Path, id: u64) -> PathBuf {
    root.join("progression").join(id.to_string()).join("Ff7.character.json")
}

/// A state directory as a v7 server left it: Blizz is account 1 with the avatar of [`AVATAR_TIFA`] (Tifa's look, so
/// the migration gives it slot 2), the progress record of [`V7_PROGRESS`], the story of [`V7_STORY`], a claimed
/// token, and no `characters/` directory at all.
fn v7_directory() -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = fresh_root();
    {
        let store = open(&root);
        let reg = store.register("Blizz", [90, 125, 245], "default", Some((GameId::Ff7, "ff7/tifa")), NOW).unwrap();
        assert_eq!(reg.account_id, AccountId(1));
        assert!(reg.character.is_none(), "a build before v8 made no character record");
        store.authenticate(&reg.token).unwrap();
        store.put_progression(&Progression::new(AccountId(1), GameId::Ff7, STORY_BLOB.to_vec(), NOW + 100)).unwrap();
    }
    fs::write(progress_file(&root, 1), V7_PROGRESS).unwrap();
    // This build's `open` makes the (empty) directory of the records it adds; a v7 server never did.
    fs::remove_dir(root.join("characters")).unwrap();
    assert!(!tree(&root).contains_key("characters"), "a v7 tree has no characters/ directory");
    (dir, root)
}

/// A state directory as a v8 server leaves it: Wedge is account 1, made by the character creator (a sturdy body in
/// the Aerith slot, 3, with the canon look of its slot mirrored into the avatar profile for a rolled-back v7), with
/// progress and a story.
fn v8_directory() -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = fresh_root();
    {
        let store = open(&root);
        let new = NewPlayerCharacter::for_game(
            "sturdy",
            GameId::Ff7,
            GameCharacter { binding: Some("ff7/body/sturdy".into()), slot: Some(3), progress: GameCharacter::pointers(GameId::Ff7, Some(3)) },
        );
        let reg = store.register_with_character("Wedge", [200, 10, 10], "default", Some((GameId::Ff7, "ff7/aerith")), Some(new), NOW).unwrap();
        assert_eq!(reg.account_id, AccountId(1));
        assert!(reg.character.is_some());
        store.authenticate(&reg.token).unwrap();
        store.put_progression(&Progression::new(AccountId(1), GameId::Ff7, STORY_BLOB.to_vec(), NOW + 100)).unwrap();
        store
            .put_character_progress(&CharacterProgress { exp: 3_000, ap: 40, gil: 700, limit: 55, ..CharacterProgress::new(AccountId(1), GameId::Ff7, 3, 12, NOW + 200) })
            .unwrap();
    }
    (dir, root)
}

/// The migration rule the SERVER applies, written out for these tests (the real one is
/// `fmv_server::admin::legacy_entry`; this crate cannot know it): the slot is the character the avatar's look names
/// (Tifa is 2, no look is Cloud, 0), the binding is that look, and the pointers follow the slot.
fn legacy_rule(view: &LegacyView<'_>) -> GameCharacter {
    let look = view.avatar.and_then(|a| a.override_for(GameId::Ff7)).map(str::to_owned);
    let slot = match look.as_deref() {
        Some("ff7/tifa") => 2,
        _ => 0,
    };
    GameCharacter { binding: look, slot: Some(slot), progress: GameCharacter::pointers(GameId::Ff7, Some(slot)) }
}

fn never_called(_: &LegacyView<'_>) -> GameCharacter {
    panic!("the migration closure must not run here")
}

fn sturdy() -> Restyle {
    Restyle { body: "sturdy".into(), binding: "ff7/body/sturdy".into(), tint: Some([255, 0, 0]) }
}

fn lean_same_tint() -> Restyle {
    Restyle { body: "lean".into(), binding: "ff7/body/lean".into(), tint: None }
}

// ---- whole-tree snapshots ----------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir,
    File(Vec<u8>),
}

/// Every file (its bytes) and directory under `root` by relative path. `LOCK` (the pid of whoever holds the store,
/// which is not data) is the only thing left out.
fn tree(root: &Path) -> BTreeMap<String, Node> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Node>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if rel == "LOCK" {
                continue;
            }
            if path.is_dir() {
                out.insert(rel, Node::Dir);
                walk(base, &path, out);
            } else {
                out.insert(rel, Node::File(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The paths that are in only one of the trees or differ between them, sorted.
fn diff(before: &BTreeMap<String, Node>, after: &BTreeMap<String, Node>) -> Vec<String> {
    before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect()
}

fn file<'a>(tree: &'a BTreeMap<String, Node>, rel: &str) -> &'a [u8] {
    match tree.get(rel) {
        Some(Node::File(bytes)) => bytes,
        other => panic!("{rel} is not a file in the tree: {other:?}"),
    }
}

// ---- the fixtures are what they claim to be ------------------------------------------------------------------------

#[test]
fn the_v7_fixtures_are_the_exact_files_a_v7_server_wrote() {
    assert_eq!(NOW, 1_700_000_000);
    assert_eq!(pretty(&v7_progress_record()), V7_PROGRESS, "the gauge is the last key");
    assert_eq!(pretty(&Progression::new(AccountId(1), GameId::Ff7, STORY_BLOB.to_vec(), NOW + 100)), V7_STORY);
    assert_eq!(serde_json::from_str::<CharacterProgress>(V7_PROGRESS).unwrap(), v7_progress_record());

    let (_dir, root) = v7_directory();
    let files = tree(&root);
    assert_eq!(file(&files, "avatars/1.json"), AVATAR_TIFA.as_bytes(), "the avatar this build writes is the v7 file");
    assert_eq!(file(&files, "progression/1/Ff7.json"), V7_STORY.as_bytes());
    assert_eq!(file(&files, "progression/1/Ff7.character.json"), V7_PROGRESS.as_bytes());
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "accounts",
            "accounts/1.json",
            "avatars",
            "avatars/1.json",
            "index.json",
            "index.log",
            "progression",
            "progression/1",
            "progression/1/Ff7.character.json",
            "progression/1/Ff7.json"
        ],
        "a v7 state directory: no characters/"
    );
}

/// What `Store::open` itself writes, which the tool's callers must not blame on a restyle: on a v7 tree it makes the
/// empty `characters/` directory (a v8 store keeps its records there) and nothing else (the index log, the format
/// marker and every record are byte for byte what they were); on a v8 tree it writes nothing.
#[test]
fn opening_the_store_adds_the_characters_directory_to_a_v7_tree_and_nothing_to_a_v8_one() {
    let (_dir, root) = v7_directory();
    let before = tree(&root);
    drop(open(&root));
    let after = tree(&root);
    assert_eq!(diff(&before, &after), ["characters"], "only the empty directory");
    assert_eq!(after.get("characters"), Some(&Node::Dir));
    assert_eq!(before.get("index.log"), after.get("index.log"));
    assert_eq!(before.get("index.json"), after.get("index.json"));

    let (_dir, root) = v8_directory();
    let before = tree(&root);
    drop(open(&root));
    assert_eq!(diff(&before, &tree(&root)), Vec::<String>::new(), "a v8 tree is not changed by opening it");
}

// ---- an account that predates the record ----------------------------------------------------------------------------

/// The core claim, on the shape of the owner's account: the record is created exactly as the first login of a v8
/// server would have made it, the look then changes, and the one file written is the record.
#[test]
fn a_v7_account_gets_its_record_as_the_migration_would_make_it_and_then_only_the_look_changes() {
    let (_twin_dir, twin_root) = v7_directory();
    let (_dir, root) = v7_directory();
    let at = NOW + 10;

    // The twin: the first login's migration, with the same closure and the same time.
    let migrated = {
        let store = open(&twin_root);
        let (record, added) = store.ensure_player_character(AccountId(1), GameId::Ff7, at, legacy_rule).unwrap();
        assert!(added);
        record
    };
    assert_eq!(migrated.origin, CharacterOrigin::Migrated);
    assert_eq!(migrated.slot(GameId::Ff7), Some(2));
    assert_eq!(migrated.binding(GameId::Ff7), Some("ff7/tifa"));
    assert_eq!((migrated.identity.body.as_str(), migrated.identity.tint, migrated.identity.name.as_str()), ("", [90, 125, 245], "Blizz"));

    // The restyle.
    let start = tree(&root);
    let store = open(&root);
    let opened = tree(&root);
    let calls = AtomicUsize::new(0);
    let report = store
        .restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), at, false, |view| {
            calls.fetch_add(1, Ordering::SeqCst);
            legacy_rule(view)
        })
        .unwrap();
    drop(store);
    let end = tree(&root);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the migration ran once");

    // 1. Open made the directory; the restyle wrote exactly one file. Everything else, the index log and the format
    //    marker included, is byte for byte what it was.
    assert_eq!(diff(&start, &opened), ["characters"]);
    assert_eq!(diff(&opened, &end), ["characters/1.json"]);
    assert_eq!(diff(&start, &end), ["characters", "characters/1.json"]);
    for (rel, node) in &start {
        assert_eq!(end.get(rel), Some(node), "{rel} changed");
    }
    assert_eq!(file(&end, "progression/1/Ff7.character.json"), V7_PROGRESS.as_bytes());
    assert_eq!(file(&end, "progression/1/Ff7.json"), V7_STORY.as_bytes());
    assert_eq!(file(&end, "avatars/1.json"), AVATAR_TIFA.as_bytes(), "the look a rolled-back v7 server reads is untouched");

    // 2. The record is the migration's, field by field (timestamps included), with only the look changed.
    let mut expected = migrated.clone();
    expected.identity.body = "sturdy".into();
    expected.identity.tint = [255, 0, 0];
    expected.games.get_mut("Ff7").unwrap().binding = Some("ff7/body/sturdy".into());
    let stored: PlayerCharacter = serde_json::from_slice(file(&end, "characters/1.json")).unwrap();
    assert_eq!(stored, expected);
    assert_eq!(file(&end, "characters/1.json"), pretty(&expected).as_bytes(), "the file is the record in the store's own format");
    assert_eq!(open(&root).get_player_character(AccountId(1)).unwrap(), Some(expected.clone()), "and a fresh open reads it");

    // 3. What must not move did not: slot, pointers, name, origin, creation time.
    let game = stored.game(GameId::Ff7).unwrap();
    assert_eq!(game.slot, Some(2));
    assert_eq!(game.progress, vec![ProgressRef::Character { game: GameId::Ff7, character: 2 }, ProgressRef::Story { game: GameId::Ff7 }]);
    assert_eq!((stored.identity.name.as_str(), stored.origin, stored.created_at), ("Blizz", CharacterOrigin::Migrated, at));
    let store = open(&root);
    let linked = store.linked_character_progress(AccountId(1), GameId::Ff7).unwrap().expect("the pointer still finds the record");
    assert_eq!(linked.record, v7_progress_record(), "every number is what the v7 server stored");
    assert!(linked.in_sync());

    // 4. The report says the same.
    assert_eq!(report.added, RestyleAdded::Record);
    assert_eq!(report.before, migrated, "before is what the migration makes");
    assert_eq!(report.after, expected);
    assert_eq!((report.account, report.game, report.dry_run, report.written), (AccountId(1), GameId::Ff7, false, true));
    assert_eq!(report.path, character_file(&root, 1));
    assert!(report.look_changed());
    assert_eq!(report.before.slot(GameId::Ff7), report.after.slot(GameId::Ff7));
    assert_eq!(report.before.origin, report.after.origin);
}

/// A record the character creator made: only the look moves, the closure never runs, and the slot, the pointers,
/// the name, the origin and the creation time are the record's own.
#[test]
fn a_v8_account_is_restyled_in_place_and_only_its_record_changes() {
    let (_dir, root) = v8_directory();
    let start = tree(&root);
    let made: PlayerCharacter = serde_json::from_slice(file(&start, "characters/1.json")).unwrap();
    assert_eq!((made.origin, made.slot(GameId::Ff7), made.identity.body.as_str()), (CharacterOrigin::Created, Some(3), "sturdy"));

    let at = NOW + 500;
    let store = open(&root);
    let opened = tree(&root);
    let report = store.restyle_player_character(AccountId(1), GameId::Ff7, &lean_same_tint(), at, false, never_called).unwrap();
    drop(store);
    let end = tree(&root);

    assert_eq!(diff(&start, &opened), Vec::<String>::new());
    assert_eq!(diff(&opened, &end), ["characters/1.json"], "the one file");
    let now_stored: PlayerCharacter = serde_json::from_slice(file(&end, "characters/1.json")).unwrap();
    let mut expected = made.clone();
    expected.identity.body = "lean".into();
    expected.games.get_mut("Ff7").unwrap().binding = Some("ff7/body/lean".into());
    expected.updated_at = at;
    assert_eq!(now_stored, expected, "only the body and the binding moved (and updated_at)");
    assert_eq!(now_stored.identity.tint, [200, 10, 10], "no colour was named, so it is kept");
    assert_eq!((now_stored.slot(GameId::Ff7), now_stored.created_at, now_stored.origin), (Some(3), made.created_at, CharacterOrigin::Created));
    assert_eq!(now_stored.game(GameId::Ff7).unwrap().progress, made.game(GameId::Ff7).unwrap().progress, "the pointers");
    assert_eq!(file(&end, "avatars/1.json"), file(&start, "avatars/1.json"), "the v7 mirror of the slot's look is untouched");
    assert_eq!(file(&end, "progression/1/Ff7.character.json"), file(&start, "progression/1/Ff7.character.json"));

    assert_eq!(report.added, RestyleAdded::Nothing);
    assert_eq!(report.before, made);
    assert_eq!(report.after, expected);
    assert!(report.written && report.look_changed());
}

// ---- a dry run ----------------------------------------------------------------------------------------------------------

#[test]
fn a_dry_run_writes_nothing_and_reports_what_the_real_run_does() {
    type Builder = fn() -> (tempfile::TempDir, PathBuf);
    for (shape, build) in [("v7", v7_directory as Builder), ("v8", v8_directory as Builder)] {
        let at = NOW + 50;
        let (_dry_dir, dry_root) = build();
        let (_real_dir, real_root) = build();

        let dry_store = open(&dry_root);
        let opened = tree(&dry_root);
        let dry = dry_store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), at, true, legacy_rule).unwrap();
        assert_eq!(tree(&dry_root), opened, "{shape}: a dry run writes nothing at all, not even a temp file");
        drop(dry_store);
        assert!(!tree(&dry_root).keys().any(|k| k.contains(".tmp-")), "{shape}");

        let real_store = open(&real_root);
        let real = real_store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), at, false, legacy_rule).unwrap();
        drop(real_store);

        assert!(dry.dry_run && !dry.written, "{shape}");
        assert!(!real.dry_run && real.written, "{shape}");
        assert_eq!(dry.added, real.added, "{shape}: the same plan");
        assert_eq!(dry.before, real.before, "{shape}: the same plan");
        assert_eq!(dry.after, real.after, "{shape}: the same plan");
        assert_eq!((dry.path.clone(), real.path.clone()), (character_file(&dry_root, 1), character_file(&real_root, 1)), "{shape}");
        assert!(dry.look_changed(), "{shape}");
        // And the dry run's plan is exactly what the real run then wrote.
        let written: PlayerCharacter = serde_json::from_slice(file(&tree(&real_root), "characters/1.json")).unwrap();
        assert_eq!(written, dry.after, "{shape}");
    }
}

#[test]
fn a_dry_run_of_a_v7_account_does_not_create_its_record() {
    let (_dir, root) = v7_directory();
    let store = open(&root);
    let opened = tree(&root);
    let report = store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW, true, legacy_rule).unwrap();
    assert_eq!(report.added, RestyleAdded::Record);
    assert!(!character_file(&root, 1).exists());
    assert_eq!(tree(&root), opened);
    assert_eq!(store.get_player_character(AccountId(1)).unwrap(), None, "the store still sees no record");
}

// ---- the lock ---------------------------------------------------------------------------------------------------------

/// The tool opens the store like a server does, so a server (or any other process that holds the store) refuses it
/// and nothing is touched.
#[test]
fn a_store_that_is_held_refuses_a_second_open_and_the_tree_is_untouched() {
    let (_dir, root) = v7_directory();
    let before = tree(&root);
    let held = open(&root);
    let while_held = tree(&root);
    assert_eq!(diff(&before, &while_held), ["characters"], "what the holder's own open made");
    let refused = Store::open(&root).unwrap_err();
    assert!(matches!(refused, PersistError::Locked { pid, .. } if pid == std::process::id()), "{refused:?}");
    assert!(refused.to_string().contains("locked"), "{refused}");
    assert_eq!(tree(&root), while_held, "the refused open changed nothing");
    // The holder itself can restyle, and its work is not blocked by the refused attempt.
    held.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW, false, legacy_rule).unwrap();
    drop(held);
    assert!(Store::open(&root).is_ok(), "released");
}

// ---- what is refused -----------------------------------------------------------------------------------------------------

#[test]
fn an_unknown_account_is_refused_and_nothing_is_written() {
    let (_dir, root) = v7_directory();
    let store = open(&root);
    let opened = tree(&root);
    for dry_run in [false, true] {
        match store.restyle_player_character(AccountId(99), GameId::Ff7, &sturdy(), NOW, dry_run, never_called) {
            Err(RestyleError::UnknownAccount(AccountId(99))) => {}
            other => panic!("expected UnknownAccount, got {other:?}"),
        }
    }
    assert!(matches!(store.preview_player_character(AccountId(99), GameId::Ff7, NOW, never_called), Err(RestyleError::UnknownAccount(AccountId(99)))));
    assert_eq!(tree(&root), opened);
}

/// A record that cannot be read is never replaced by a default behind the operator's back: the file stays as it is,
/// the migration closure does not run, and nothing else is written.
#[test]
fn a_damaged_record_is_refused_and_left_exactly_as_it_is() {
    let (_dir, root) = v8_directory();
    let good = String::from_utf8(fs::read(character_file(&root, 1)).unwrap()).unwrap();
    let store = open(&root);
    for (what, bytes) in [
        ("not json", b"{ not json".to_vec()),
        ("a newer schema", good.replace("\"schema_version\": 1", "\"schema_version\": 2").into_bytes()),
        ("another account's record", good.replace("\"account_id\": 1", "\"account_id\": 2").into_bytes()),
        ("a record that breaks the rules", good.replace("\"character\": 3", "\"character\": 5").into_bytes()),
        ("an oversized file", vec![b' '; MAX_PLAYER_CHARACTER_BYTES as usize + 1]),
    ] {
        fs::write(character_file(&root, 1), &bytes).unwrap();
        let damaged = tree(&root);
        for dry_run in [false, true] {
            match store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW, dry_run, never_called) {
                Err(RestyleError::UnreadableRecord(_)) => {}
                other => panic!("{what}: expected UnreadableRecord, got {other:?}"),
            }
        }
        assert!(matches!(store.preview_player_character(AccountId(1), GameId::Ff7, NOW, never_called), Err(RestyleError::UnreadableRecord(_))), "{what}");
        assert_eq!(tree(&root), damaged, "{what}: the record and everything else is left for the operator");
        assert_eq!(fs::read(character_file(&root, 1)).unwrap(), bytes, "{what}");
    }
}

/// An account with neither an avatar profile nor a record has nothing to build a character from (the server gives
/// an account its avatar at its first login, before the record). It is refused and the error says what to do.
#[test]
fn an_account_with_neither_avatar_nor_record_is_refused_with_what_to_do() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (account, _token) = store.create_account("Cid", NOW).unwrap();
    assert!(store.avatar_for_account(account).unwrap().is_none());
    let opened = tree(&root);
    for dry_run in [false, true] {
        match store.restyle_player_character(account, GameId::Ff7, &sturdy(), NOW, dry_run, never_called) {
            Err(e @ RestyleError::NothingToMigrateFrom(_)) => {
                assert!(e.to_string().contains("log in with it once"), "{e}");
            }
            other => panic!("expected NothingToMigrateFrom, got {other:?}"),
        }
    }
    assert!(matches!(store.preview_player_character(account, GameId::Ff7, NOW, never_called), Err(RestyleError::NothingToMigrateFrom(_))));
    assert_eq!(tree(&root), opened);
}

/// The avatar is only needed to BUILD a record: an account that has the record but no avatar profile is restyled.
#[test]
fn an_account_that_has_its_record_but_no_avatar_is_restyled() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (account, _token) = store.create_account("Cid", NOW).unwrap();
    store.ensure_player_character(account, GameId::Ff7, NOW + 1, legacy_rule).unwrap();
    let opened = tree(&root);
    let report = store.restyle_player_character(account, GameId::Ff7, &sturdy(), NOW + 2, false, never_called).unwrap();
    assert_eq!(report.added, RestyleAdded::Nothing);
    let end = tree(&root);
    assert_eq!(diff(&opened, &end), ["characters/1.json"]);
    assert_eq!(report.after.identity.name, "Cid");
    assert_eq!(report.after.binding(GameId::Ff7), Some("ff7/body/sturdy"));
}

/// The record's own checks apply to the result: a binding or a body that breaks them is refused, nothing is
/// written and the stored record (or the lack of one) stays.
#[test]
fn a_look_that_breaks_the_records_own_checks_is_refused_and_nothing_is_written() {
    let bad_looks: Vec<(&str, Restyle, &str)> = vec![
        ("a binding over 64 characters", Restyle { binding: "x".repeat(65), ..sturdy() }, "binding"),
        ("a binding with a control character", Restyle { binding: "\u{1b}[31m".into(), ..sturdy() }, "binding"),
        ("an empty binding", Restyle { binding: String::new(), ..sturdy() }, "binding"),
        ("a body with a control character", Restyle { body: "b\u{7}".into(), ..sturdy() }, "identity.body"),
        ("a body over 64 bytes", Restyle { body: "b".repeat(65), ..sturdy() }, "identity.body"),
    ];
    for (shape, build) in [("v7", v7_directory as fn() -> (tempfile::TempDir, PathBuf)), ("v8", v8_directory as fn() -> (tempfile::TempDir, PathBuf))] {
        let (_dir, root) = build();
        let store = open(&root);
        let opened = tree(&root);
        for (what, change, field) in &bad_looks {
            match store.restyle_player_character(AccountId(1), GameId::Ff7, change, NOW, false, legacy_rule) {
                Err(RestyleError::Store(PersistError::InvalidField { field: got, .. })) => assert_eq!(got, *field, "{shape}: {what}"),
                other => panic!("{shape}: {what}: expected InvalidField, got {other:?}"),
            }
            assert_eq!(tree(&root), opened, "{shape}: {what}: nothing was written");
        }
    }
}

/// What the migration closure returns must pass the record's checks, as it must for the login's migration.
#[test]
fn a_migration_that_builds_an_invalid_entry_writes_nothing() {
    let (_dir, root) = v7_directory();
    let store = open(&root);
    let opened = tree(&root);
    let err = store
        .restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW, false, |_| GameCharacter {
            slot: Some(1),
            progress: vec![ProgressRef::Character { game: GameId::Ff7, character: 4 }],
            ..GameCharacter::default()
        })
        .unwrap_err();
    assert!(matches!(err, RestyleError::Store(PersistError::InvalidField { field: "progress", .. })), "{err:?}");
    assert_eq!(tree(&root), opened);
}

// ---- idempotence and the closure -----------------------------------------------------------------------------------------

#[test]
fn restyling_twice_with_the_same_look_changes_nothing_the_second_time() {
    let (_dir, root) = v7_directory();
    let store = open(&root);
    let first = store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW + 10, false, legacy_rule).unwrap();
    assert!(first.written);
    let after_first = tree(&root);

    let second = store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW + 20, false, never_called).unwrap();
    assert_eq!(second.added, RestyleAdded::Nothing);
    assert!(!second.look_changed());
    assert!(!second.written, "nothing to change, nothing written");
    assert_eq!(second.before, second.after);
    assert_eq!(second.after, first.after, "identity, binding, slot, pointers and the timestamps are all as the first run left them");
    assert_eq!(tree(&root), after_first, "the record was not even rewritten");

    // A different look afterwards is a write of that one file again; the creation time and the slot stay.
    let third = store.restyle_player_character(AccountId(1), GameId::Ff7, &lean_same_tint(), NOW + 30, false, never_called).unwrap();
    assert!(third.written && third.look_changed());
    assert_eq!(diff(&after_first, &tree(&root)), ["characters/1.json"]);
    assert_eq!((third.after.created_at, third.after.updated_at, third.after.slot(GameId::Ff7)), (NOW + 10, NOW + 30, Some(2)));
    assert_eq!(third.after.identity.tint, [255, 0, 0], "the colour the first run named is kept by a run that names none");
}

#[test]
fn only_a_missing_record_or_entry_runs_the_migration_closure_and_only_once() {
    let (_dir, root) = v7_directory();
    let store = open(&root);
    let calls = AtomicUsize::new(0);
    let counting = |view: &LegacyView<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        legacy_rule(view)
    };
    store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), NOW, false, counting).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The record has the entry now: no closure, whatever the look.
    store.restyle_player_character(AccountId(1), GameId::Ff7, &lean_same_tint(), NOW + 1, false, never_called).unwrap();
    store.restyle_player_character(AccountId(1), GameId::Ff7, &lean_same_tint(), NOW + 2, true, never_called).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A character that has played another game has a record without an entry for this one: the entry is built like
/// the migration builds it, the other game's entry and the record's own origin are left as they are.
#[test]
fn a_record_without_an_entry_for_the_game_gains_it_and_keeps_the_others() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let fixture_entry = GameCharacter { progress: GameCharacter::pointers(GameId::Fixture, None), ..GameCharacter::default() };
    let new = NewPlayerCharacter::for_game("lean", GameId::Fixture, fixture_entry.clone());
    let reg = store.register_with_character("Wedge", [1, 2, 3], "default", Some((GameId::Ff7, "ff7/tifa")), Some(new), NOW).unwrap();
    let made = reg.character.unwrap();
    assert!(made.game(GameId::Ff7).is_none());
    let opened = tree(&root);

    let (preview, added) = store.preview_player_character(reg.account_id, GameId::Ff7, NOW + 5, legacy_rule).unwrap();
    assert_eq!(added, RestyleAdded::Entry);
    assert_eq!(tree(&root), opened, "a preview writes nothing");
    let report = store.restyle_player_character(reg.account_id, GameId::Ff7, &sturdy(), NOW + 5, false, legacy_rule).unwrap();
    assert_eq!(report.added, RestyleAdded::Entry);
    assert_eq!(report.before, preview);
    assert_eq!(report.after.game(GameId::Fixture), Some(&fixture_entry), "the other game's entry is not touched");
    assert_eq!(report.after.slot(GameId::Ff7), Some(2), "the entry is the migration's");
    assert_eq!(report.after.binding(GameId::Ff7), Some("ff7/body/sturdy"));
    assert_eq!((report.after.origin, report.after.created_at, report.after.updated_at), (CharacterOrigin::Created, NOW, NOW + 5));
    assert_eq!(diff(&opened, &tree(&root)), ["characters/1.json"]);
}

// ---- the preview ------------------------------------------------------------------------------------------------------------

#[test]
fn a_preview_is_the_record_the_first_login_would_make_and_writes_nothing() {
    let (_twin_dir, twin_root) = v7_directory();
    let (_dir, root) = v7_directory();
    let at = NOW + 10;
    let migrated = open(&twin_root).ensure_player_character(AccountId(1), GameId::Ff7, at, legacy_rule).unwrap().0;

    let store = open(&root);
    let opened = tree(&root);
    let (preview, added) = store.preview_player_character(AccountId(1), GameId::Ff7, at, legacy_rule).unwrap();
    assert_eq!((preview, added), (migrated, RestyleAdded::Record));
    assert_eq!(tree(&root), opened);
    assert_eq!(store.get_player_character(AccountId(1)).unwrap(), None);

    // Once the record exists the preview is the stored record.
    let report = store.restyle_player_character(AccountId(1), GameId::Ff7, &sturdy(), at, false, legacy_rule).unwrap();
    let (again, added) = store.preview_player_character(AccountId(1), GameId::Ff7, at + 1, never_called).unwrap();
    assert_eq!((again, added), (report.after, RestyleAdded::Nothing));
}

// ---- the other accounts ---------------------------------------------------------------------------------------------------

/// Restyling one account leaves another account's files, record included, as they are.
#[test]
fn another_accounts_files_are_untouched() {
    let (_dir, root) = v8_directory();
    let store = open(&root);
    let other = store.register("Other", [9, 9, 9], "default", Some((GameId::Ff7, "ff7/cid")), NOW + 1).unwrap();
    store.ensure_player_character(other.account_id, GameId::Ff7, NOW + 2, legacy_rule).unwrap();
    let before = tree(&root);
    store.restyle_player_character(AccountId(1), GameId::Ff7, &lean_same_tint(), NOW + 3, false, never_called).unwrap();
    let after = tree(&root);
    assert_eq!(diff(&before, &after), ["characters/1.json"]);
    assert_eq!(after.get("characters/2.json"), before.get("characters/2.json"));
    // and the account it was meant for is the one that changed
    assert_eq!(store.get_player_character(AccountId(1)).unwrap().unwrap().identity.body, "lean");
    assert_eq!(store.get_player_character(other.account_id).unwrap().unwrap().identity.body, "");
}
