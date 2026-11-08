//! The operator tool for orphaned registrations (`src/bin/fmv-accounts.rs`), run as a real
//! process against a temp state directory.

mod common;

use std::process::{Command, Output};

use common::*;
use fmv_core::{AccountId, GameId};
use fmv_persist::{PersistError, Progression};

/// The tool, built by cargo for this test target.
const TOOL: &str = env!("CARGO_BIN_EXE_fmv-accounts");

fn run(args: &[&str]) -> Output {
    Command::new(TOOL)
        .args(args)
        .output()
        .expect("run fmv-accounts")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_admin_tool_lists_and_releases_orphaned_registrations() {
    let (_guard, root) = fresh_root();
    let dir = root.to_str().unwrap();
    let store = open(&root);
    let reg = |name: &str| store.register(name, [0; 3], "default", None, NOW).unwrap();
    let orphan = reg("Orphan");
    let player = reg("Player");
    let busy = reg("Busy");
    let ghost = reg("Ghost");
    store.authenticate(&player.token).unwrap();
    store
        .put_progression(&Progression::new(
            busy.account_id,
            GameId::Ff7,
            vec![1],
            NOW,
        ))
        .unwrap();
    // With the store open, the tool is refused: the state directory is locked.
    let refused = run(&[dir, "list"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("locked"),
        "{}",
        text(&refused.stderr)
    );
    drop(store);

    // list: the three accounts nobody has logged into (a progression record does not claim it).
    let listed = run(&[dir, "list"]);
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    let out = text(&listed.stdout);
    assert!(
        out.contains("3 of 4 accounts have never been logged into"),
        "{out}"
    );
    for name in ["Orphan", "Busy", "Ghost"] {
        assert!(out.contains(&format!("{name:?}")), "{out}");
    }
    assert!(!out.contains("Player"), "{out}");

    // release: the orphan goes; the player who logged in, the account with progression and an
    // unknown id are kept and reported, and the exit status says that something was kept.
    let released = run(&[
        dir,
        "release",
        &orphan.account_id.0.to_string(),
        &player.account_id.0.to_string(),
        &busy.account_id.0.to_string(),
        "99",
    ]);
    assert_eq!(
        released.status.code(),
        Some(1),
        "{}",
        text(&released.stderr)
    );
    let out = text(&released.stdout);
    assert!(
        out.contains(&format!("released account {}", orphan.account_id.0)),
        "{out}"
    );
    assert!(
        out.contains(&format!("kept account {}", player.account_id.0)),
        "{out}"
    );
    assert!(
        out.contains(&format!("kept account {}", busy.account_id.0)),
        "{out}"
    );
    assert!(out.contains("kept account 99"), "{out}");

    // release-older-than: what is left unclaimed and has no progression goes.
    let swept = run(&[dir, "release-older-than", "0"]);
    let out = text(&swept.stdout);
    assert!(
        out.contains("2 unclaimed account(s) older than 0 s"),
        "{out}"
    );
    assert!(
        out.contains(&format!("released account {}", ghost.account_id.0)),
        "{out}"
    );
    assert!(
        out.contains(&format!("kept account {}", busy.account_id.0)),
        "{out}"
    );

    // The result, seen by the store itself: only the orphan and the ghost were released, their
    // names can be registered again, ids are not reused, and the player is untouched.
    let store = open(&root);
    assert_eq!(store.account_count(), 2);
    assert_eq!(
        store.find_account_by_name("Player"),
        Some(player.account_id)
    );
    assert_eq!(store.find_account_by_name("Busy"), Some(busy.account_id));
    assert_eq!(store.find_account_by_name("Orphan"), None);
    assert_eq!(
        store.authenticate(&player.token).unwrap(),
        player.account_id
    );
    assert!(matches!(
        store.authenticate(&orphan.token),
        Err(PersistError::InvalidToken)
    ));
    let again = store
        .register("Orphan", [0; 3], "default", None, NOW)
        .unwrap();
    assert_eq!(again.account_id, AccountId(5));
    drop(store);

    // Bad invocations are usage errors, not crashes, and change nothing.
    for args in [
        vec![dir],
        vec![dir, "release"],
        vec![dir, "release", "not-a-number"],
        vec![dir, "release-older-than"],
        vec![dir, "release-older-than", "-5"],
        vec![dir, "frobnicate"],
    ] {
        let bad = run(&args);
        assert_eq!(
            bad.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&bad.stderr)
        );
        assert!(text(&bad.stderr).contains("usage"), "{args:?}");
    }
    assert_eq!(open(&root).account_count(), 3);
}
