//! The real `fmv-client` binary and `--delete NAME`: the command asks the server to delete the character with the login
//! saved for NAME, removes that saved login (and only that one) when the server says it is gone, prints ONE line on stdout
//! and says how it went with its exit code. It opens no window, reads no game files and sends nothing for a name it has no
//! login for.
//!
//! The server is a fake in this file, on a local port, for one connection: it reads the first frame (which must be the
//! delete request) and answers as told. Hermetic: every run gets `--state-dir <tempdir>` and `FMV_STATE_DIR`,
//! `XDG_DATA_HOME` and `HOME` inside it, so the real state directory is never read or written, and no display variable,
//! so a window could not open if anything tried.

use fmv_protocol::{read_msg, write_msg, ClientMsg, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// What the fake server answers to the request.
#[derive(Clone, Copy)]
enum Reply {
    Deleted(&'static str),
    Reject(RejectCode, &'static str),
}

/// A fake server for ONE connection on a local port: it reads the first frame and answers. Returns the port and a handle
/// for the first frame it read (`None` when the client sent nothing).
fn serve(reply: Reply) -> (u16, std::thread::JoinHandle<Option<ClientMsg>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        rt.block_on(async move {
            let limit = Duration::from_secs(30);
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let (stream, _) = tokio::time::timeout(limit, listener.accept()).await.expect("a client in time").expect("accept");
            let (mut rd, mut wr) = stream.into_split();
            let first = match tokio::time::timeout(limit, read_msg::<_, ClientMsg>(&mut rd)).await {
                Ok(Ok(msg)) => Some(msg),
                _ => None,
            };
            let answer = match reply {
                Reply::Deleted(name) => ServerMsg::CharacterDeleted { name: name.to_string() },
                Reply::Reject(code, reason) => ServerMsg::reject(code, reason),
            };
            let _ = write_msg(&mut wr, &answer).await;
            first
        })
    });
    (port, handle)
}

/// A port nothing listens on.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox { dir: tempfile::tempdir().expect("tempdir") }
    }

    fn accounts(&self) -> PathBuf {
        self.dir.path().join("accounts")
    }

    /// A token file as an earlier run would have left it (mode 0600).
    fn save(&self, file_name: &str, token: &str, name: &str) -> PathBuf {
        std::fs::create_dir_all(self.accounts()).expect("accounts dir");
        let path = self.accounts().join(file_name);
        std::fs::write(&path, format!("{token}\nname={name}\n")).expect("token file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        path
    }

    /// Run the client with a watchdog, so that a hung client fails the test instead of hanging it.
    fn run(&self, args: &[&str]) -> Output {
        let state: &Path = self.dir.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmv-client"));
        cmd.args(args)
            .arg("--state-dir")
            .arg(state)
            .env("FMV_STATE_DIR", state)
            .env("XDG_DATA_HOME", state.join("xdg-data"))
            .env("HOME", state.join("home"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The request a delete of the login `token` must make: the version of this protocol, a build that says what the client is
/// and the token, as the first frame of the connection.
fn assert_request(first: Option<ClientMsg>, token: &str) {
    match first {
        Some(ClientMsg::DeleteCharacter { protocol_version, client_build, token: sent }) => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert!(client_build.starts_with("fmv-client/"), "{client_build}");
            assert_eq!(sent, Secret::new(token), "the token of the character asked for");
        }
        other => panic!("the first frame must be DeleteCharacter, got {other:?}"),
    }
}

/// No secret of the run on either stream.
fn assert_no_secrets(out: &Output, secrets: &[&str]) {
    for stream in [text(&out.stdout), text(&out.stderr)] {
        for secret in secrets {
            assert!(!stream.contains(secret), "a token was printed: {stream}");
        }
    }
}

const CLOUD: &str = "fmvt_cloud_secret_0123456789";
const TIFA: &str = "fmvt_tifa_secret_0123456789";

#[test]
fn delete_asks_the_server_with_the_login_of_that_name_removes_only_that_login_and_prints_one_line() {
    let (port, server) = serve(Reply::Deleted("Tifa"));
    let sandbox = Sandbox::new();
    // Cloud is the server's default login, Tifa has a file of her own.
    let cloud = sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Cloud");
    let tifa = sandbox.save(&format!("127.0.0.1_{port}.tifa.token"), TIFA, "Tifa");
    let out = sandbox.run(&["--delete", "Tifa", "--server", &format!("127.0.0.1:{port}")]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "deleted character name=Tifa\n", "exactly one line on stdout");
    assert_request(server.join().expect("the fake server"), TIFA);
    assert!(!tifa.exists(), "Tifa's saved login is gone");
    assert_eq!(std::fs::read_to_string(&cloud).expect("Cloud's login"), format!("{CLOUD}\nname=Cloud\n"), "Cloud's login is untouched");
    assert_no_secrets(&out, &[CLOUD, TIFA]);
    assert!(!stderr.contains("cannot open a window") && !stderr.contains("FF7") && !stderr.contains("panicked"), "{stderr}");
}

#[test]
fn a_login_the_server_does_not_know_is_removed_and_the_line_says_the_character_is_already_gone() {
    let (port, server) = serve(Reply::Reject(RejectCode::InvalidToken, "invalid or unknown token"));
    let sandbox = Sandbox::new();
    let cloud = sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Cloud");
    let out = sandbox.run(&["--delete", "cloud", "--server", &format!("127.0.0.1:{port}")]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "character already gone on the server name=Cloud\n", "the saved login's own spelling of the name");
    assert_request(server.join().expect("the fake server"), CLOUD);
    assert!(!cloud.exists(), "the saved login is removed");
    assert_no_secrets(&out, &[CLOUD]);
}

#[test]
fn a_name_with_a_space_is_percent_encoded_so_the_line_splits_on_spaces() {
    let (port, server) = serve(Reply::Deleted("Red XIII"));
    let sandbox = Sandbox::new();
    sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Red XIII");
    let out = sandbox.run(&["--delete", "Red XIII", "--server", &format!("127.0.0.1:{port}")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "deleted character name=Red%20XIII\n");
    drop(server.join());
}

#[test]
fn a_refusal_exits_one_keeps_the_login_and_prints_nothing_on_stdout() {
    for (code, reason, said) in [
        (RejectCode::AlreadyOnline, "that character is in the game right now", "Cloud is in the game right now (from this or another computer)."),
        (RejectCode::Internal, "internal error; try again later", "Cloud was NOT deleted and its saved login was kept. The server refused: internal error; try again later"),
        (RejectCode::ProtocolMismatch, "protocol version mismatch", "The server refused: protocol version mismatch"),
    ] {
        let (port, server) = serve(Reply::Reject(code, reason));
        let sandbox = Sandbox::new();
        let cloud = sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Cloud");
        let out = sandbox.run(&["--delete", "Cloud", "--server", &format!("127.0.0.1:{port}")]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.code(), Some(1), "{code:?}\nstdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "", "{code:?}: nothing on stdout for a failure");
        assert!(stderr.contains(said), "{code:?}: {stderr}");
        assert_request(server.join().expect("the fake server"), CLOUD);
        assert!(cloud.exists(), "{code:?}: the saved login is kept");
        assert_no_secrets(&out, &[CLOUD]);
    }
}

#[test]
fn a_server_that_cannot_be_reached_exits_one_and_keeps_the_login() {
    let port = closed_port();
    let sandbox = Sandbox::new();
    let cloud = sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Cloud");
    let out = sandbox.run(&["--delete", "Cloud", "--server", &format!("127.0.0.1:{port}")]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "");
    assert!(stderr.contains("Cloud was NOT deleted and its saved login was kept: cannot connect to"), "{stderr}");
    assert!(cloud.exists(), "the saved login is kept");
    assert_no_secrets(&out, &[CLOUD]);
}

#[test]
fn a_name_with_no_saved_login_exits_two_and_sends_nothing() {
    // Nothing listens here: a connection would be a different error, and a different exit code.
    let port = closed_port();
    let sandbox = Sandbox::new();
    let cloud = sandbox.save(&format!("127.0.0.1_{port}.token"), CLOUD, "Cloud");
    let out = sandbox.run(&["--delete", "Nobody", "--server", &format!("127.0.0.1:{port}")]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(2), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "");
    assert!(stderr.contains("no login is saved for \"Nobody\""), "{stderr}");
    assert!(!stderr.contains("cannot connect"), "nothing was sent: {stderr}");
    assert!(cloud.exists(), "Cloud's login is untouched");
    // With nothing saved at all the state directory is not even created.
    let empty = Sandbox::new();
    let out = empty.run(&["--delete", "Nobody", "--server", &format!("127.0.0.1:{port}")]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!empty.accounts().exists(), "a delete creates nothing");
}

#[test]
fn delete_is_a_command_of_its_own_and_conflicts_are_usage_errors() {
    let sandbox = Sandbox::new();
    for extra in [&["--headless"][..], &["--name", "x"], &["--game", "ff7"]] {
        let mut args = vec!["--delete", "Cloud"];
        args.extend_from_slice(extra);
        let out = sandbox.run(&args);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {}", text(&out.stderr));
        assert!(text(&out.stderr).contains("cannot be used with"), "{extra:?}: {}", text(&out.stderr));
    }
}
