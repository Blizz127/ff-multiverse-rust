//! The operator commands `delete`, `trash` and `restore` of `fmv-characters`, run as a real process against temp state
//! directories. Every "nothing else changed" is a whole-tree snapshot (every file's bytes and every directory by
//! relative path; only the `LOCK` file is left out). Hermetic: every spawned tool gets `FMV_STATE_DIR` pointing at an
//! empty temp directory that must stay empty, and nothing here can touch a real state directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{CharacterProgress, GameCharacter, NewPlayerCharacter, PersistError, Progression, Registered, Store};

const TOOL: &str = env!("CARGO_BIN_EXE_fmv-characters");
const NOW: u64 = 1_700_000_000;

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

/// The paths that are in only one of the trees or differ between them.
fn diff(before: &BTreeMap<String, Node>, after: &BTreeMap<String, Node>) -> BTreeSet<String> {
    before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).cloned().collect()
}

/// A state directory of a v8 server with `names` registered (accounts 1, 2, ...), each a played character: a sturdy
/// body, progress with items and a limit gauge, and a story blob.
fn directory(names: &[&str]) -> (tempfile::TempDir, PathBuf, Vec<Registered>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let store = Store::open(&root).unwrap();
    let mut registered = Vec::new();
    for (n, name) in names.iter().enumerate() {
        let slot = u8::try_from(n % 4).unwrap();
        let entry = GameCharacter { binding: Some("ff7/body/sturdy".into()), slot: Some(slot), progress: GameCharacter::pointers(GameId::Ff7, Some(slot)) };
        let new = NewPlayerCharacter::for_game("sturdy", GameId::Ff7, entry);
        let reg = store.register_with_character(name, [10, 20, 30], "default", Some((GameId::Ff7, "ff7/body/sturdy")), Some(new), NOW).unwrap();
        store.authenticate(&reg.token).unwrap();
        store
            .put_character_progress(&CharacterProgress {
                exp: 1_000 + n as u32,
                gil: 55,
                inventory: Some(vec![InventoryEntry::new(3, 2)]),
                limit: 99,
                ..CharacterProgress::new(reg.account_id, GameId::Ff7, slot, 7, NOW + 1)
            })
            .unwrap();
        store.put_progression(&Progression::new(reg.account_id, GameId::Ff7, vec![1, 6, 0], NOW + 2)).unwrap();
        registered.push(reg);
    }
    drop(store);
    (dir, root, registered)
}

fn files_of(tree: &BTreeMap<String, Node>, account: u64) -> BTreeSet<String> {
    tree.keys()
        .filter(|rel| {
            rel.as_str() == format!("accounts/{account}.json")
                || rel.as_str() == format!("avatars/{account}.json")
                || rel.as_str() == format!("characters/{account}.json")
                || rel.starts_with(&format!("progression/{account}"))
        })
        .cloned()
        .collect()
}

fn trash_entries(root: &Path) -> Vec<String> {
    match fs::read_dir(root.join("trash")) {
        Ok(dir) => {
            let mut names: Vec<String> = dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

// ---- delete ----------------------------------------------------------------------------------------------------

#[test]
fn delete_moves_one_account_into_the_trash_and_changes_nothing_else() {
    let (_guard, root, accounts) = directory(&["Aria Vale", "Brom", "Cid"]);
    let before = tree(&root);
    let aria = files_of(&before, 1);
    assert_eq!(aria.len(), 6, "{aria:?}: account, avatar, character, two progress files and the progress directory");

    let out = run(&[dir_arg(&root), "delete", "Aria Vale"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("account 1 \"Aria Vale\""), "{stdout}");
    assert!(stdout.contains("delete: 5 file(s) move into the trash"), "{stdout}");
    assert!(stdout.contains("moved: accounts/1.json (sha256 "), "{stdout}");
    assert!(stdout.contains("moved: progression/1/Ff7.character.json (sha256 "), "{stdout}");
    assert!(stdout.contains("the name \"Aria Vale\" is free"), "{stdout}");
    assert!(stdout.contains("index.log: changed"), "{stdout}");
    assert!(stdout.contains("other file(s)") && stdout.contains("byte for byte unchanged"), "{stdout}");
    assert!(!stdout.contains("VERIFICATION FAILED"), "{stdout}");
    assert!(stdout.contains("result: done") && stdout.contains(" restore "), "{stdout}");
    assert!(!stdout.contains(&accounts[0].token), "the token is never printed");

    let after = tree(&root);
    let entries = trash_entries(&root);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].ends_with("-1"), "{entries:?}");
    // The only paths that differ: Aria's files, the index, and the new trash.
    let changed = diff(&before, &after);
    for rel in &changed {
        let hers = aria.contains(rel) || rel == "index.log" || rel == "trash" || rel.starts_with("trash/");
        assert!(hers, "{rel} changed but is not Aria's, the index or the trash");
    }
    for rel in aria.iter().filter(|rel| matches!(before[*rel], Node::File(_))) {
        assert!(!after.contains_key(rel), "{rel} is still in the state directory");
        assert_eq!(after.get(&format!("trash/{}/{rel}", entries[0])), before.get(rel), "{rel} in the trash is byte for byte what it was");
    }
    assert!(after.contains_key(&format!("trash/{}/manifest.json", entries[0])));
    for account in [2, 3] {
        for rel in files_of(&before, account) {
            assert_eq!(after.get(&rel), before.get(&rel), "{rel}");
        }
    }

    // The name is free in the store, the token is void, the others are fine.
    let store = Store::open(&root).unwrap();
    assert_eq!(store.find_account_by_name("aria vale"), None);
    assert!(matches!(store.authenticate(&accounts[0].token), Err(PersistError::InvalidToken)));
    assert_eq!(store.authenticate(&accounts[1].token).unwrap(), AccountId(2));
    assert_eq!(store.account_count(), 2);
    let again = store.register("Aria Vale", [1, 2, 3], "default", None, NOW + 9).unwrap();
    assert_eq!(again.account_id, AccountId(4), "the retired id is not used again");
}

#[test]
fn delete_names_the_account_by_id_or_by_name_and_refuses_what_it_does_not_know() {
    let (_guard, root, _) = directory(&["Aria", "Brom"]);
    let before = tree(&root);
    for unknown in ["99", "Nobody", "", "aria brom"] {
        let out = run(&[dir_arg(&root), "delete", unknown]);
        assert_eq!(out.status.code(), Some(1), "{unknown:?}: {}", text(&out.stderr));
        assert!(text(&out.stderr).contains("no file was written") || text(&out.stderr).contains("required"), "{}", text(&out.stderr));
        assert_eq!(tree(&root), before, "{unknown:?}");
    }
    let out = run(&[dir_arg(&root), "delete", "2"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("account 2 \"Brom\""), "{}", text(&out.stdout));
    assert_eq!(trash_entries(&root).len(), 1);
    // A usage error is exit 2 and writes nothing.
    let out = run(&[dir_arg(&root), "delete"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("usage:") && text(&out.stderr).contains("delete <account-id|name>"), "{}", text(&out.stderr));
}

#[test]
fn a_dry_run_delete_shows_the_plan_and_writes_nothing() {
    let (_guard, root, _) = directory(&["Aria", "Brom"]);
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "delete", "Aria", "--dry-run"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(stdout.contains("dry run: nothing was written") && stdout.contains("delete: 5 file(s) move into the trash"), "{stdout}");
    assert!(stdout.contains("  accounts/1.json") && stdout.contains("  characters/1.json"), "{stdout}");
    assert!(stdout.contains("result: dry run; no file was written"), "{stdout}");
    assert_eq!(tree(&root), before, "not even a trash directory was made");
}

#[test]
fn delete_is_refused_while_a_server_holds_the_state_directory_and_changes_nothing() {
    let (_guard, root, _) = directory(&["Aria"]);
    let _server = Store::open(&root).unwrap();
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "delete", "Aria"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("locked") && stderr.contains("nothing was changed"), "{stderr}");
    assert_eq!(tree(&root), before);
}

#[test]
fn delete_refuses_a_directory_that_is_not_a_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(&[dir_arg(&dir.path().join("nope")), "delete", "Aria"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("cannot read"), "{}", text(&out.stderr));
    assert!(!dir.path().join("nope").exists(), "a mistyped path is not turned into a state directory");
}

// ---- trash and restore ------------------------------------------------------------------------------------------

#[test]
fn trash_lists_what_was_deleted_and_says_when_there_is_nothing() {
    let (_guard, root, _) = directory(&["Aria", "Brom"]);
    let out = run(&[dir_arg(&root), "trash"]);
    assert!(out.status.success());
    assert_eq!(text(&out.stdout).trim(), "the trash is empty");
    let before = tree(&root);
    assert_eq!(tree(&root), before, "listing writes nothing");
    run(&[dir_arg(&root), "delete", "Brom"]);
    let out = run(&[dir_arg(&root), "trash"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("the trash: 1 entr(ies)") && stdout.contains("\"Brom\"") && stdout.contains("account 2") && stdout.contains("; deleted"), "{stdout}");
    assert!(stdout.contains("5 file(s)"), "{stdout}");
}

#[test]
fn restore_brings_a_character_back_as_a_new_account_with_a_new_token_and_changes_nothing_else() {
    let (_guard, root, accounts) = directory(&["Aria Vale", "Brom", "Cid"]);
    let aria_before = {
        let store = Store::open(&root).unwrap();
        (
            store.get_player_character(AccountId(1)).unwrap().unwrap(),
            store.get_character_progress(AccountId(1), GameId::Ff7).unwrap().unwrap(),
            store.get_progression(AccountId(1), GameId::Ff7).unwrap(),
        )
    };
    assert!(run(&[dir_arg(&root), "delete", "Aria Vale"]).status.success());
    let entry = trash_entries(&root).remove(0);
    let before = tree(&root);

    let out = run(&[dir_arg(&root), "restore", "Aria Vale"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("restore: as a NEW account") && stdout.contains("a NEW token"), "{stdout}");
    assert!(stdout.contains("written: accounts/4.json (sha256 "), "{stdout}");
    assert!(stdout.contains("the trash entry kept its 5 record file(s) byte for byte"), "{stdout}");
    assert!(stdout.contains("result: done; \"Aria Vale\" is account 4 again (the old id 1 stays retired)"), "{stdout}");
    assert!(!stdout.contains("VERIFICATION FAILED"), "{stdout}");
    let token = stdout.lines().find_map(|l| l.strip_prefix("token: ")).expect("the token is printed once").trim().to_owned();
    assert!(token.starts_with("fmvt_") && token.len() == 5 + 64, "{token}");
    assert!(stdout.contains("shown once"), "{stdout}");

    // Only the new account's files, the index and the entry's own folder changed.
    let after = tree(&root);
    for rel in diff(&before, &after) {
        let new_files = ["accounts/4.json", "avatars/4.json", "characters/4.json", "progression/4"];
        let ok = rel == "index.log" || new_files.iter().any(|f| rel.starts_with(f)) || rel.starts_with(&format!("trash/{entry}"));
        assert!(ok, "{rel} changed but is not the new account, the index or the entry");
    }
    for account in [2, 3] {
        for rel in files_of(&before, account) {
            assert_eq!(after.get(&rel), before.get(&rel), "{rel}");
        }
    }
    assert_eq!(trash_entries(&root), [format!("{entry}.restored-4")], "the entry says what became of it");

    // In the store: the same character under the new ids, the new token logs in, the old one does not.
    let store = Store::open(&root).unwrap();
    assert_eq!(store.authenticate(&token).unwrap(), AccountId(4));
    assert!(matches!(store.authenticate(&accounts[0].token), Err(PersistError::InvalidToken)));
    assert_eq!(store.find_account_by_name("aria vale"), Some(AccountId(4)));
    let mut want = aria_before.0.clone();
    want.account_id = AccountId(4);
    assert_eq!(store.get_player_character(AccountId(4)).unwrap().unwrap(), want);
    let mut want = aria_before.1.clone();
    want.account_id = AccountId(4);
    assert_eq!(store.get_character_progress(AccountId(4), GameId::Ff7).unwrap().unwrap(), want);
    assert_eq!(store.get_progression(AccountId(4), GameId::Ff7).unwrap().blob, aria_before.2.blob);
    assert!(store.unclaimed_accounts().is_empty());
    drop(store);

    // The same entry cannot be restored a second time, and the listing says what became of it.
    let again = run(&[dir_arg(&root), "restore", &entry]);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("no restorable trash entry"), "{}", text(&again.stderr));
    let listing = text(&run(&[dir_arg(&root), "trash"]).stdout);
    assert!(listing.contains("restored as account 4"), "{listing}");
}

#[test]
fn a_dry_run_restore_runs_the_same_checks_and_writes_nothing() {
    let (_guard, root, _) = directory(&["Aria", "Brom"]);
    assert!(run(&[dir_arg(&root), "delete", "Aria"]).status.success());
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "restore", "Aria", "--dry-run"]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(stdout.contains("dry run: nothing was written") && stdout.contains("result: dry run; no file was written"), "{stdout}");
    assert!(!stdout.contains("token:"), "a dry run mints no token");
    assert_eq!(tree(&root), before);
    // The checks are real: with the name taken the dry run refuses, like the restore.
    let store = Store::open(&root).unwrap();
    store.register("Aria", [1, 2, 3], "default", None, NOW + 9).unwrap();
    drop(store);
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "restore", "Aria", "--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("already taken") && text(&out.stderr).contains("no file was written"), "{}", text(&out.stderr));
    assert_eq!(tree(&root), before);
}

#[test]
fn restore_is_refused_when_the_name_is_taken_and_when_the_entry_is_damaged_and_changes_nothing() {
    let (_guard, root, _) = directory(&["Aria", "Brom"]);
    assert!(run(&[dir_arg(&root), "delete", "Aria"]).status.success());
    let entry = trash_entries(&root).remove(0);
    // Somebody took the name.
    let store = Store::open(&root).unwrap();
    store.register("ARIA", [1, 2, 3], "default", None, NOW + 9).unwrap();
    drop(store);
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "restore", &entry]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("already taken"), "{}", text(&out.stderr));
    assert_eq!(tree(&root), before);
    // A damaged record in the entry.
    let record = root.join("trash").join(&entry).join("characters/1.json");
    fs::write(&record, b"{ not json").unwrap();
    let before = tree(&root);
    let out = run(&[dir_arg(&root), "restore", &entry]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("corrupt"), "{}", text(&out.stderr));
    assert_eq!(tree(&root), before);
}
