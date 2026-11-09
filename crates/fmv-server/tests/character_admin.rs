//! The operator tool `fmv-characters` (`src/bin/fmv-characters.rs`), run as a real process against temp state
//! directories: it converts an existing account's look to a generic placeholder body, creating the player-character
//! record from the legacy data first when the account has none, and writes nothing else.
//!
//! The directories are built in the shape of a v7 server's state directory (the owner's live account is one: a canon
//! look, numbers a battle left, a story, and no `characters/` directory) and of a v8 server's. Every "nothing else
//! changed" is a **whole-tree snapshot** (every file's bytes and every directory by relative path; only the `LOCK`
//! file, which is the pid of whoever holds the store, is left out). Hermetic: every spawned tool gets
//! `FMV_STATE_DIR` pointing at an empty temp directory (which must stay empty: the tool never reads it, it only
//! touches the directory it is given), and nothing here can touch a real state directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{CharacterOrigin, CharacterProgress, GameCharacter, NewPlayerCharacter, PlayerCharacter, ProgressRef, Progression, Store};

/// The tool, built by cargo for this test target.
const TOOL: &str = env!("CARGO_BIN_EXE_fmv-characters");

const NOW: u64 = 1_700_000_000;
const STORY_BLOB: [u8; 3] = [0x01, 0x02, 0xff];

fn run(args: &[&str]) -> Output {
    let env_state = tempfile::tempdir().expect("tempdir");
    let output = Command::new(TOOL).args(args).env("FMV_STATE_DIR", env_state.path()).output().expect("run fmv-characters");
    assert_eq!(fs::read_dir(env_state.path()).unwrap().count(), 0, "the tool never touches $FMV_STATE_DIR");
    output
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn dir_arg(root: &Path) -> &str {
    root.to_str().expect("utf-8 temp path")
}

// ---- state directories ----------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir,
    File(Vec<u8>),
}

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
    before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).cloned().collect::<BTreeSet<_>>().into_iter().collect()
}

fn state_path(root: &Path) -> PathBuf {
    root.join("state")
}

/// The owner's kind of account, as a v7 server left it: Blizz is account 1 with a canon look (Cloud's, so the
/// migration gives it slot 0), what a battle left (level, experience, AP, gil, two stacks of items, a limit gauge), a
/// story and a claimed token, and NO `characters/` directory.
fn v7_directory() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = state_path(dir.path());
    {
        let store = Store::open(&root).unwrap();
        let reg = store.register("Blizz", [0x5a, 0x7d, 0xf5], "default", Some((GameId::Ff7, "ff7/cloud")), NOW).unwrap();
        assert_eq!(reg.account_id, AccountId(1));
        assert!(reg.character.is_none(), "a build before v8 made no character record");
        store.authenticate(&reg.token).unwrap();
        store.put_progression(&Progression::new(AccountId(1), GameId::Ff7, STORY_BLOB.to_vec(), NOW + 100)).unwrap();
        store
            .put_character_progress(&CharacterProgress {
                exp: 17_820,
                ap: 330,
                gil: 9_120,
                inventory: Some(vec![InventoryEntry::new(0, 9), InventoryEntry::new(3, 4)]),
                limit: 137,
                ..CharacterProgress::new(AccountId(1), GameId::Ff7, 0, 20, 1_793_600_000)
            })
            .unwrap();
    }
    // A v7 server never made the directory of the records this build adds.
    fs::remove_dir(root.join("characters")).unwrap();
    let files = tree(&root);
    assert!(!files.contains_key("characters"));
    for rel in ["accounts/1.json", "avatars/1.json", "index.json", "index.log", "progression/1/Ff7.character.json", "progression/1/Ff7.json"] {
        assert!(matches!(files.get(rel), Some(Node::File(_))), "{rel}");
    }
    (dir, root)
}

/// A v8 server's state directory: Wedge is account 1, made by the character creator (a sturdy body in the Aerith
/// slot, 3), with progress and a story.
fn v8_directory() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = state_path(dir.path());
    {
        let store = Store::open(&root).unwrap();
        let new = NewPlayerCharacter::for_game(
            "sturdy",
            GameId::Ff7,
            GameCharacter { binding: Some("ff7/body/sturdy".into()), slot: Some(3), progress: GameCharacter::pointers(GameId::Ff7, Some(3)) },
        );
        let reg = store.register_with_character("Wedge", [200, 10, 10], "default", Some((GameId::Ff7, "ff7/aerith")), Some(new), NOW).unwrap();
        store.authenticate(&reg.token).unwrap();
        store.put_progression(&Progression::new(AccountId(1), GameId::Ff7, STORY_BLOB.to_vec(), NOW + 100)).unwrap();
        store
            .put_character_progress(&CharacterProgress { exp: 3_000, ap: 40, gil: 700, limit: 55, ..CharacterProgress::new(AccountId(1), GameId::Ff7, 3, 12, NOW + 200) })
            .unwrap();
    }
    (dir, root)
}

/// The record of account 1 as a store reads it (the tool is not running, so the lock is free).
fn stored_record(root: &Path) -> Option<PlayerCharacter> {
    Store::open(root).unwrap().get_player_character(AccountId(1)).unwrap()
}

/// `sha256sum`'s digest of a file, if the machine has the command (the tool's own hash is hand-written, so this is the
/// independent check that its numbers are the ones everybody's tool prints).
fn system_sha256(path: &Path) -> Option<String> {
    let out = Command::new("sha256sum").arg(path).output().ok()?;
    out.status.success().then(|| text(&out.stdout).split_whitespace().next().map(str::to_owned)).flatten()
}

/// The lines of the plan: everything before the verification block, without the dry-run banner.
fn plan_lines(stdout: &str) -> Vec<&str> {
    stdout.lines().take_while(|l| !l.starts_with("verification")).filter(|l| *l != "dry run: nothing was written").collect()
}

fn assert_line(stdout: &str, line: &str) {
    assert!(stdout.lines().any(|l| l == line), "no line {line:?} in\n{stdout}");
}

// ---- the lock ------------------------------------------------------------------------------------------------------------

/// A server (here a `Store`) holds the state directory: the tool is refused, says so, and touches nothing.
#[test]
fn a_held_state_directory_refuses_the_tool_and_nothing_is_touched() {
    let (_dir, root) = v7_directory();
    let held = Store::open(&root).unwrap();
    let while_held = tree(&root);
    for args in [
        vec![dir_arg(&root), "restyle", "Blizz", "--body", "sturdy"],
        vec![dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--dry-run"],
        vec![dir_arg(&root), "show", "Blizz"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(err.contains("locked"), "{args:?}: {err}");
        assert!(err.contains("nothing was changed"), "{args:?}: {err}");
        assert!(text(&out.stdout).is_empty(), "{args:?}: nothing is reported as done");
        assert_eq!(tree(&root), while_held, "{args:?}: the tree is untouched");
    }
    drop(held);
    let out = run(&[dir_arg(&root), "show", "Blizz"]);
    assert!(out.status.success(), "released: {}", text(&out.stderr));
}

// ---- the owner's case ------------------------------------------------------------------------------------------------

/// The owner's account: a v7 account with a canon look is converted to a placeholder body (and a colour). The record is
/// created exactly as the server's first login would create it, the look then changes, and the one file written is
/// that record: every other file, the progression above all, is byte for byte what it was.
#[test]
fn a_v7_account_is_restyled_to_a_placeholder_body_and_nothing_else_changes() {
    let (_twin_dir, twin_root) = v7_directory();
    let (_dir, root) = v7_directory();
    let start = tree(&root);
    let digests: Vec<(&str, Option<String>)> = [
        "accounts/1.json",
        "avatars/1.json",
        "index.json",
        "index.log",
        "progression/1/Ff7.character.json",
        "progression/1/Ff7.json",
    ]
    .into_iter()
    .map(|rel| (rel, system_sha256(&root.join(rel))))
    .collect();
    // Where the machine has `sha256sum` the independent check really runs on all six files (it is not skipped by a
    // failure to run it).
    let has_sha256sum = Command::new("sha256sum").arg("--version").output().is_ok_and(|o| o.status.success());
    assert_eq!(digests.iter().filter(|(_, system)| system.is_some()).count(), if has_sha256sum { 6 } else { 0 });

    let out = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "", "a clean run is silent on stderr");

    // The before/after summary.
    assert_line(&stdout, "account 1 \"Blizz\" (game Ff7)");
    assert_line(&stdout, "record: created from the legacy data: slot 0, binding ff7/cloud");
    assert_line(&stdout, "slot (unchanged): 0");
    assert_line(&stdout, "body: (none) -> sturdy");
    assert_line(&stdout, "binding: ff7/cloud -> ff7/body/sturdy");
    assert_line(&stdout, "tint: 5a7df5 -> ff0000");
    assert!(!stdout.contains("dry run"), "{stdout}");

    // The verification block: every file that was not written, hashed before and after, and the one that was.
    assert!(stdout.lines().any(|l| l.starts_with("verification")), "{stdout}");
    for (rel, system) in &digests {
        let line = stdout.lines().find(|l| l.trim_start().starts_with(&format!("{rel} sha256 "))).unwrap_or_else(|| panic!("no line for {rel}:\n{stdout}"));
        assert!(line.ends_with(" -> same"), "{line}");
        let shown = line.trim_start().strip_prefix(&format!("{rel} sha256 ")).unwrap().strip_suffix(" -> same").unwrap();
        assert_eq!(shown.len(), 12, "{line}");
        if let Some(full) = system {
            assert_eq!(shown, &full[..12], "{rel}: the tool's SHA-256 is the one sha256sum prints");
        }
    }
    assert!(!stdout.lines().any(|l| l.contains("progression/1/Ff7.character.json") && l.contains("written")), "{stdout}");
    let written = stdout.lines().find(|l| l.trim_start().starts_with("written: characters/1.json (new file, sha256 ")).unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(written.trim_start().len(), "written: characters/1.json (new file, sha256 )".len() + 12, "{written}");
    assert!(stdout.contains("directory characters was created when the store was opened"), "open's one effect is reported as open's: {stdout}");
    assert!(stdout.contains("read back: the record on disk is the planned one"), "{stdout}");
    assert!(!stdout.contains("CHANGED") && !stdout.contains("FAILED"), "{stdout}");
    assert_line(&stdout, "result: done; characters/1.json was written and every other file is unchanged");

    // The whole tree: the empty directory the store makes and the one record. Nothing else, the progression included.
    let end = tree(&root);
    assert_eq!(diff(&start, &end), ["characters", "characters/1.json"]);
    for (rel, node) in &start {
        assert_eq!(end.get(rel), Some(node), "{rel} changed");
    }
    assert!(end.keys().all(|k| !k.contains(".tmp-")), "no temp file is left");

    // The record on disk: the placeholder body, the colour, and everything the migration decides.
    let record = stored_record(&root).expect("the record exists now");
    assert_eq!(record.binding(GameId::Ff7), Some("ff7/body/sturdy"));
    assert_eq!((record.identity.body.as_str(), record.identity.tint, record.identity.name.as_str()), ("sturdy", [255, 0, 0], "Blizz"));
    assert_eq!(record.slot(GameId::Ff7), Some(0), "the combat slot is Cloud's, as it was");
    assert_eq!(record.origin, CharacterOrigin::Migrated);
    assert_eq!(
        record.game(GameId::Ff7).unwrap().progress,
        vec![ProgressRef::Character { game: GameId::Ff7, character: 0 }, ProgressRef::Story { game: GameId::Ff7 }]
    );
    // It is the record the server's own login migration makes, with only the look changed: the twin directory is
    // migrated by `Store::ensure_player_character` with the server's closure (`admin::legacy_entry`).
    let migrated = Store::open(&twin_root)
        .unwrap()
        .ensure_player_character(AccountId(1), GameId::Ff7, NOW, |view| fmv_server::admin::legacy_entry(GameId::Ff7, view))
        .unwrap()
        .0;
    let mut expected = migrated;
    expected.identity.body = "sturdy".into();
    expected.identity.tint = [255, 0, 0];
    expected.games.get_mut("Ff7").unwrap().binding = Some("ff7/body/sturdy".into());
    expected.created_at = record.created_at;
    expected.updated_at = record.updated_at;
    assert_eq!(record, expected);
    assert!(record.created_at >= NOW, "stamped with the time of the run");

    // The numbers the pointer finds are the ones the v7 server stored.
    let store = Store::open(&root).unwrap();
    let linked = store.linked_character_progress(AccountId(1), GameId::Ff7).unwrap().expect("the pointer finds the record");
    assert!(linked.in_sync());
    assert_eq!((linked.record.level, linked.record.exp, linked.record.ap, linked.record.gil, linked.record.limit), (20, 17_820, 330, 9_120, 137));
    assert_eq!(linked.record.inventory, Some(vec![InventoryEntry::new(0, 9), InventoryEntry::new(3, 4)]));
}

/// `--dry-run` prints the plan a real run carries out, writes nothing, and proves it.
#[test]
fn a_dry_run_prints_the_plan_and_writes_nothing() {
    let (_real_dir, real_root) = v7_directory();
    let (_dir, root) = v7_directory();
    let start = tree(&root);

    let out = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000", "--dry-run"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert_eq!(stdout.lines().next(), Some("dry run: nothing was written"), "{stdout}");
    assert_line(&stdout, "result: dry run; no file was written");
    assert_line(&stdout, "  not written: characters/1.json (absent)");
    assert!(!stdout.contains("written: characters/1.json (new file"), "{stdout}");
    assert!(!stdout.contains("CHANGED") && !stdout.contains("FAILED"), "{stdout}");

    // Nothing was written but the empty directory the store makes when it is opened (and says so).
    assert_eq!(diff(&start, &tree(&root)), ["characters"]);
    assert!(!root.join("characters").join("1.json").exists());
    assert!(stdout.contains("directory characters was created when the store was opened"), "{stdout}");
    assert_eq!(stored_record(&root), None, "the store still sees no record");

    // The plan is the real run's.
    let real = run(&[dir_arg(&real_root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000"]);
    assert!(real.status.success(), "{}", text(&real.stderr));
    assert_eq!(plan_lines(&stdout), plan_lines(&text(&real.stdout)));
    assert_eq!(plan_lines(&stdout).len(), 6, "account, record, slot, body, binding, tint: {stdout}");
}

// ---- show ------------------------------------------------------------------------------------------------------------------

#[test]
fn show_prints_the_record_or_what_the_first_login_would_create_and_writes_nothing() {
    let (_dir, root) = v7_directory();
    let start = tree(&root);

    let out = run(&[dir_arg(&root), "show", "Blizz"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert_line(&stdout, "account 1 \"Blizz\" (game Ff7)");
    assert!(stdout.contains("record: none yet; this is what the first login would create from the legacy data (nothing is written by this command)"), "{stdout}");
    assert_line(&stdout, "  binding: ff7/cloud (a canon character's look)");
    assert_line(&stdout, "  slot: 0");
    assert_line(&stdout, "  body: (none)");
    assert_line(&stdout, "  tint: 5a7df5");
    assert_line(&stdout, "  origin: migrated");
    assert_line(&stdout, "  progress pointers: character progress (character 0), story");
    assert!(stdout.contains("avatar profile look (what a server rolled back to v7 reads; this tool never changes it): ff7/cloud"), "{stdout}");
    assert_eq!(diff(&start, &tree(&root)), ["characters"], "only the empty directory the store makes");

    // After the restyle `show` reads the stored record, by name or by id.
    let restyled = run(&[dir_arg(&root), "restyle", "1", "--body", "slight"]);
    assert!(restyled.status.success(), "{}", text(&restyled.stderr));
    let after = tree(&root);
    for who in ["1", "blizz", "BLIZZ"] {
        let out = run(&[dir_arg(&root), "show", who]);
        let stdout = text(&out.stdout);
        assert!(out.status.success(), "{who}: {stdout}\n{}", text(&out.stderr));
        assert!(stdout.contains("record: stored (created "), "{who}: {stdout}");
        assert_line(&stdout, "  binding: ff7/body/slight (a placeholder body)");
        assert_line(&stdout, "  body: slight");
        assert_line(&stdout, "  slot: 0");
        assert_line(&stdout, "  tint: 5a7df5");
        assert!(stdout.contains("this tool never changes it): ff7/cloud"), "the avatar's look is still the canon one: {stdout}");
        assert_eq!(tree(&root), after, "{who}: show wrote nothing");
    }
}

// ---- what is refused -----------------------------------------------------------------------------------------------------

/// A body that is not offered is refused, listing what is; a canon character is not a body; nothing is opened, so not
/// even the empty directory is made.
#[test]
fn a_body_that_is_not_offered_is_refused_with_the_list_and_nothing_is_touched() {
    let (_dir, root) = v7_directory();
    let start = tree(&root);
    for bad in ["cloud", "Cloud", "ff7/cloud", "tifa", "ff7/tifa", "ff7/body/lean", "nobody", "", "../../etc/passwd"] {
        let out = run(&[dir_arg(&root), "restyle", "Blizz", "--body", bad]);
        assert_eq!(out.status.code(), Some(1), "{bad:?}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(err.contains("refused") && err.contains("offered: lean, sturdy, slight"), "{bad:?}: {err}");
        assert!(err.contains("a canon character's look is not one"), "{bad:?}: {err}");
        assert!(text(&out.stdout).is_empty(), "{bad:?}");
        assert_eq!(tree(&root), start, "{bad:?}: nothing was opened or written");
    }
    // A game that has no bodies has nothing to restyle to.
    let out = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "lean", "--game", "fixture"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Fixture offers no bodies"), "{}", text(&out.stderr));
    assert_eq!(tree(&root), start);
    // The offered spelling in any case is accepted (the canonical id is stored).
    let out = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "SlIgHt", "--dry-run"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_line(&text(&out.stdout), "body: (none) -> slight");
}

#[test]
fn bad_usage_exits_2_prints_the_usage_and_touches_nothing() {
    let (_dir, root) = v7_directory();
    let start = tree(&root);
    let dir = dir_arg(&root);
    for args in [
        vec![],
        vec![dir],
        vec![dir, "frobnicate"],
        vec![dir, "show"],
        vec![dir, "show", "Blizz", "extra"],
        vec![dir, "show", "Blizz", "--body", "lean"],
        vec![dir, "restyle", "Blizz"],
        vec![dir, "restyle", "--body", "sturdy"],
        vec![dir, "restyle", "Blizz", "--body"],
        vec![dir, "restyle", "Blizz", "--body", "sturdy", "--body", "lean"],
        vec![dir, "restyle", "Blizz", "--body", "sturdy", "--tint", "red"],
        vec![dir, "restyle", "Blizz", "--body", "sturdy", "--tint", "12345"],
        vec![dir, "restyle", "Blizz", "--body", "sturdy", "--game", "ff99"],
        vec![dir, "restyle", "Blizz", "--body", "sturdy", "--force"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(err.contains("usage: fmv-characters"), "{args:?}: {err}");
        assert!(err.contains("--body <lean|sturdy|slight>"), "{args:?}: {err}");
        assert!(text(&out.stdout).is_empty(), "{args:?}");
        assert_eq!(tree(&root), start, "{args:?}: nothing was opened or written");
    }
}

#[test]
fn an_unknown_account_and_a_path_that_is_not_a_state_directory_are_refused() {
    let (dir, root) = v7_directory();
    let start = tree(&root);
    for who in ["99", "Nobody", "0"] {
        let out = run(&[dir_arg(&root), "restyle", who, "--body", "sturdy"]);
        assert_eq!(out.status.code(), Some(1), "{who}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(err.contains("unknown account") && err.contains("no file was written"), "{who}: {err}");
        let shown = run(&[dir_arg(&root), "show", who]);
        assert_eq!(shown.status.code(), Some(1), "{who}");
    }
    assert_eq!(diff(&start, &tree(&root)), ["characters"], "only what opening the store does");

    // A path that is not a state directory is stopped before the store could make one out of it.
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let missing = dir.path().join("missing");
    for path in [&empty, &missing] {
        let out = run(&[dir_arg(path), "restyle", "Blizz", "--body", "sturdy"]);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains("cannot read") || text(&out.stderr).contains("no index.log"), "{}", text(&out.stderr));
    }
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0, "an empty directory is left empty");
    assert!(!missing.exists(), "a missing one is not created");
    // The mistake worth a hint: the directory that HOLDS the state directory (a server keeps its state in the `server`
    // directory inside its base one) instead of the state directory itself.
    let above = tree(dir.path());
    let out = run(&[dir_arg(dir.path()), "show", "Blizz"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("not a server state directory") && err.contains("`server` directory"), "{err}");
    assert_eq!(tree(dir.path()), above, "nothing was made in it");
}

/// A record that cannot be read is left for the operator exactly as it is.
#[test]
fn a_damaged_record_is_refused_and_left_exactly_as_it_is() {
    let (_dir, root) = v8_directory();
    fs::write(root.join("characters").join("1.json"), b"{ not json").unwrap();
    let start = tree(&root);
    for dry_run in [false, true] {
        let mut args = vec![dir_arg(&root), "restyle", "Wedge", "--body", "lean"];
        if dry_run {
            args.push("--dry-run");
        }
        let out = run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(err.contains("cannot be read") && err.contains("left exactly as it is") && err.contains("no file was written"), "{err}");
        assert_eq!(tree(&root), start, "{args:?}");
    }
}

/// An account with neither an avatar profile nor a record has nothing to build the character from: refused, and told
/// what to do.
#[test]
fn an_account_with_neither_avatar_nor_record_is_refused_with_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let root = state_path(dir.path());
    {
        let store = Store::open(&root).unwrap();
        store.create_account("Cid", NOW).unwrap();
    }
    let start = tree(&root);
    let out = run(&[dir_arg(&root), "restyle", "Cid", "--body", "lean"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("neither a player-character record nor an avatar profile") && err.contains("log in with it once"), "{err}");
    assert_eq!(tree(&root), start);
}

// ---- other shapes ----------------------------------------------------------------------------------------------------------

/// A record the character creator made: it existed, so it is restyled, not rebuilt, and only the record changes.
#[test]
fn a_v8_account_is_restyled_in_place_and_only_its_record_changes() {
    let (_dir, root) = v8_directory();
    let start = tree(&root);
    let before = stored_record(&root).unwrap();

    let out = run(&[dir_arg(&root), "restyle", "wedge", "--body", "lean"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert_line(&stdout, "account 1 \"Wedge\" (game Ff7)");
    assert_line(&stdout, "record: existed (origin created)");
    assert_line(&stdout, "slot (unchanged): 3");
    assert_line(&stdout, "body: sturdy -> lean");
    assert_line(&stdout, "binding: ff7/body/sturdy -> ff7/body/lean");
    assert_line(&stdout, "tint: unchanged (c80a0a)");
    assert!(stdout.lines().any(|l| l.trim_start().starts_with("written: characters/1.json (sha256 ") && l.contains(" -> sha256 ")), "{stdout}");
    assert!(!stdout.contains("was created when the store was opened"), "a v8 tree is not changed by opening it: {stdout}");

    assert_eq!(diff(&start, &tree(&root)), ["characters/1.json"]);
    let after = stored_record(&root).unwrap();
    let mut expected = before.clone();
    expected.identity.body = "lean".into();
    expected.games.get_mut("Ff7").unwrap().binding = Some("ff7/body/lean".into());
    expected.updated_at = after.updated_at;
    assert_eq!(after, expected, "only the body and the binding moved (and updated_at)");
    assert_eq!((after.slot(GameId::Ff7), after.origin, after.identity.tint), (Some(3), CharacterOrigin::Created, [200, 10, 10]));
}

/// Running the same restyle again writes nothing; changing only the colour is a write of the record again.
#[test]
fn restyling_twice_with_the_same_look_writes_nothing_the_second_time() {
    let (_dir, root) = v7_directory();
    let first = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000"]);
    assert!(first.status.success(), "{}", text(&first.stderr));
    let after_first = tree(&root);

    let again = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000"]);
    let stdout = text(&again.stdout);
    assert!(again.status.success(), "{stdout}\n{}", text(&again.stderr));
    assert_line(&stdout, "record: existed (origin migrated)");
    assert_line(&stdout, "body: sturdy -> sturdy");
    assert_line(&stdout, "tint: unchanged (ff0000)");
    assert_line(&stdout, "nothing to change: the character already looks like this");
    assert_line(&stdout, "result: nothing to change; no file was written");
    assert!(stdout.contains("not written: characters/1.json (sha256 "), "{stdout}");
    assert_eq!(tree(&root), after_first, "the record was not even rewritten");

    // A dry run of the same look says so too.
    let dry = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy", "--tint", "ff0000", "--dry-run"]);
    assert!(dry.status.success(), "{}", text(&dry.stderr));
    let dry_out = text(&dry.stdout);
    assert_line(&dry_out, "dry run: nothing was written");
    assert_line(&dry_out, "nothing to change: the character already looks like this");
    assert_line(&dry_out, "result: dry run; no file was written");
    assert_eq!(tree(&root), after_first);

    // No --tint keeps the stored colour, so this is the same look too.
    let keep = run(&[dir_arg(&root), "restyle", "Blizz", "--body", "sturdy"]);
    assert!(keep.status.success());
    assert_eq!(tree(&root), after_first);

    // Only the colour changes: that is a write, of that one file.
    let recolour = run(&[dir_arg(&root), "restyle", "1", "--body", "sturdy", "--tint", "00ff00"]);
    let stdout = text(&recolour.stdout);
    assert!(recolour.status.success(), "{stdout}\n{}", text(&recolour.stderr));
    assert_line(&stdout, "tint: ff0000 -> 00ff00");
    assert_eq!(diff(&after_first, &tree(&root)), ["characters/1.json"]);
    assert_eq!(stored_record(&root).unwrap().identity.tint, [0, 255, 0]);
}

/// The account is found by id or by name (the store's own rule: case does not matter), and both are the same account.
#[test]
fn an_account_is_found_by_id_or_by_name_alike() {
    let (_dir, root) = v7_directory();
    let mut plans = Vec::new();
    for who in ["1", "Blizz", "blizz", "BLIZZ"] {
        let out = run(&[dir_arg(&root), "restyle", who, "--body", "lean", "--dry-run"]);
        assert!(out.status.success(), "{who}: {}", text(&out.stderr));
        plans.push(text(&out.stdout));
    }
    let first = plan_lines(&plans[0]);
    assert!(plans.iter().all(|p| plan_lines(p) == first), "{plans:?}");
}
