//! The real `fmv-client` binary and the character menu: what a run does when it would open the
//! menu but cannot (no display), and that `--name` and `--headless` never go near it.
//!
//! The menu itself is a window, which these tests cannot open (this machine has no display), so its
//! logic is covered by unit tests in `src/ui/` and its pixels by the ignored offscreen tests
//! (`cargo test -p fmv-client -- --ignored offscreen`). What is checked here is the part only the
//! process can show: the exit code and the messages.
//!
//! Hermetic: every run gets `--state-dir <tempdir>` and `FMV_STATE_DIR`, `XDG_DATA_HOME` and `HOME`
//! inside it, so the real state directory is never read or written.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Run the client with `state` as its state directory, with a watchdog.
fn run(state: &Path, args: &[&str], remove_env: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmv-client"));
    cmd.args(args)
        .arg("--state-dir")
        .arg(state)
        .env("FMV_STATE_DIR", state)
        .env("XDG_DATA_HOME", state.join("xdg-data"))
        .env("HOME", state.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for e in remove_env {
        cmd.env_remove(e);
    }
    let child = cmd.spawn().expect("spawn fmv-client");
    let pid = child.id();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if done_rx.recv_timeout(Duration::from_secs(60)).is_err() {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
    });
    let out = child.wait_with_output().expect("wait");
    let _ = done_tx.send(());
    let _ = watchdog.join();
    out
}

const NO_DISPLAY: [&str; 3] = ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"];

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_menu_without_a_display_says_so_cleanly_before_any_traffic() {
    let dir = tempfile::tempdir().unwrap();
    // No --name and no --headless: this run would open the character menu. A closed port on the
    // server address proves nothing is sent: the error is about the window, not the connection.
    let out = run(dir.path(), &["--server", "127.0.0.1:9"], &NO_DISPLAY);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "no display\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("cannot open a window"), "{stderr}");
    assert!(stderr.contains("--headless") && stderr.contains("--name"), "both ways out are suggested: {stderr}");
    assert!(!stderr.contains("connecting"), "nothing was sent: {stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    // It created nothing in the state directory: opening a menu is not a login.
    assert!(!dir.path().join("accounts").exists(), "no token directory was created");
}

#[test]
fn the_menu_opens_the_content_first_like_every_other_mode() {
    // A named FF7 source that does not exist is the same content error with or without a menu,
    // and it is reported before any window is needed.
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["--ff7-flevel", "/nonexistent/owned/flevel.lgp"], &NO_DISPLAY);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "content error\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("flevel.lgp") && !stderr.contains("/nonexistent"), "no local path leaks: {stderr}");
    assert!(!stderr.contains("cannot open a window"), "the content is checked first: {stderr}");
}

#[test]
fn name_and_headless_never_show_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    // --name keeps the command-line behaviour: it still goes for the window (and fails on this
    // machine) with the message the menu-less path always had, which does not suggest --name.
    let out = run(dir.path(), &["--server", "127.0.0.1:9", "--name", "x"], &NO_DISPLAY);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cannot open a window") && stderr.contains("--headless"), "{stderr}");
    assert!(!stderr.contains("or --name"), "the old message is unchanged: {stderr}");
    // --headless without a saved account is still the old error, not a menu.
    let out = run(dir.path(), &["--server", "127.0.0.1:9", "--headless"], &NO_DISPLAY);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no saved account for 127.0.0.1:9") && stderr.contains("--name"), "{stderr}");
    assert!(!stderr.contains("cannot open a window"), "{stderr}");
}

#[test]
fn the_help_describes_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["--help"], &[]);
    let help = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0));
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "CHARACTER MENU:",
        "N creates a new one",
        "Del asks to delete the highlighted character",
        "Y (or Enter on Delete) deletes it ON THE SERVER",
        "F forgets the saved login on THIS computer only",
        "the menu is skipped",
    ] {
        assert!(flat.contains(needle), "{needle:?} is missing from --help:\n{help}");
    }
}
