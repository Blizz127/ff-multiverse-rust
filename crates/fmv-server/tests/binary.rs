//! Process-level tests of the real `fmv-server` binary: the READY/stdout contract, SIGTERM
//! handling, startup failures, and accounts surviving real process restarts.
//!
//! Hermetic: every server gets `--ephemeral` or an explicit `--state-dir` inside a temporary
//! directory, and `$FMV_STATE_DIR` points at a temporary directory too, so nothing here can touch
//! (or lock) the real state directory of the machine running the tests.

use std::net::SocketAddr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use fmv_core::AreaId;
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_adapter_api::GameAdapter;
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_fmv-server");
const NEEDS_IMPORT: &str = "needs owned FF7 content imported via fmv-import";

/// A running server process with its stdout lines and its collected stderr.
struct Running {
    child: Child,
    stdout: Lines<BufReader<ChildStdout>>,
    log: JoinHandle<String>,
    addr: SocketAddr,
    ready: String,
}

/// Start the server with `args` (plus `--bind 127.0.0.1:0`) and wait for its READY line.
async fn start(args: &[&str], env_state: &Path) -> Running {
    let mut child = Command::new(BIN)
        .args(["--bind", "127.0.0.1:0"])
        .args(args)
        .env("FMV_STATE_DIR", env_state)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fmv-server");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let log = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped")).lines();
    let ready = timeout(Duration::from_secs(30), stdout.next_line())
        .await
        .expect("READY within 30 s")
        .expect("read stdout")
        .expect("stdout closed before READY");
    let addr = ready
        .split(' ')
        .find_map(|f| f.strip_prefix("addr="))
        .expect("addr= field")
        .parse()
        .expect("socket address");
    Running { child, stdout, log, addr, ready }
}

impl Running {
    /// SIGTERM (or another signal), wait for the exit and return the status and the whole stderr.
    async fn stop(mut self, signal: &str) -> (ExitStatus, String) {
        let pid = self.child.id().expect("child pid");
        let status = std::process::Command::new("kill").args([format!("-{signal}"), pid.to_string()]).status().expect("run kill");
        assert!(status.success());
        let exit = timeout(Duration::from_secs(15), self.child.wait()).await.expect("server exits in time").expect("wait");
        let log = self.log.await.unwrap_or_default();
        assert_eq!(self.stdout.next_line().await.expect("read stdout"), None, "nothing but the READY line on stdout");
        (exit, log)
    }
}

/// Handshake as far as the account: returns `Welcome`'s account, issued token and the manifest's
/// area. `auth` is a registration or a token login.
async fn hello(addr: SocketAddr, auth: Auth) -> (TcpStream, ServerMsg) {
    let mut client = TcpStream::connect(addr).await.expect("connect");
    write_msg(
        &mut client,
        &ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "binary-test".into(),
            auth,
            character: fmv_protocol::NewCharacter::with_tint([1, 2, 3]),
        },
    )
    .await
    .unwrap();
    let welcome = read_msg::<_, ServerMsg>(&mut client).await.unwrap();
    (client, welcome)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prints_one_ready_line_serves_and_exits_zero_on_sigterm_and_sigint() {
    for signal in ["TERM", "INT"] {
        ready_serve_and_stop_on(signal).await;
    }
}

async fn ready_serve_and_stop_on(signal: &str) {
    let env_state = tempfile::tempdir().expect("tempdir");
    let server = start(&["--ephemeral"], env_state.path()).await;

    // Exactly one stdout line, in the documented format, once the listener is bound.
    let fields: Vec<&str> = server.ready.split(' ').collect();
    let [word, protocol, build, addr, area, fixture] = fields[..] else {
        panic!("READY line has the wrong shape: {:?}", server.ready);
    };
    assert_eq!(word, "READY");
    assert_eq!(protocol, format!("protocol={PROTOCOL_VERSION}"));
    assert_eq!(build, format!("build={}", env!("CARGO_PKG_VERSION")));
    let addr: SocketAddr = addr.strip_prefix("addr=").expect("addr= field").parse().expect("socket address");
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0, "the READY line reports the actual bound port");
    assert_eq!(area, "area=Fixture/plaza");
    assert_eq!(fixture, "fixture=true");

    // The process really serves protocol v2: register, get the manifest, join, get the snapshot.
    let (mut client, welcome) = hello(addr, Auth::Register { display_name: "Probe".into() }).await;
    match welcome {
        ServerMsg::Welcome { issued_token, .. } => assert!(issued_token.is_some(), "a registration is answered with a token"),
        other => panic!("expected Welcome, got {other:?}"),
    }
    let info = FixtureAdapter::new().list_areas().remove(0);
    let (area, fingerprint) = match read_msg::<_, ServerMsg>(&mut client).await.unwrap() {
        ServerMsg::AreaManifest { area, fingerprint, fixture, .. } => {
            assert!(fixture);
            (area, fingerprint)
        }
        other => panic!("expected AreaManifest, got {other:?}"),
    };
    assert_eq!(area, AreaId { game: fmv_core::GameId::Fixture, key: FIXTURE_AREA_KEY.into() });
    write_msg(&mut client, &ClientMsg::JoinArea { area, fingerprint, provenance: info.provenance.summary() }).await.unwrap();
    assert!(matches!(read_msg::<_, ServerMsg>(&mut client).await.unwrap(), ServerMsg::AreaSnapshot { .. }));

    // SIGTERM / SIGINT: joined clients are told, the process exits 0, and stdout stays at one line.
    let mut running = server;
    let pid = running.child.id().expect("child pid");
    let status = std::process::Command::new("kill").args([format!("-{signal}"), pid.to_string()]).status().expect("run kill");
    assert!(status.success());
    loop {
        match timeout(Duration::from_secs(10), read_msg::<_, ServerMsg>(&mut client)).await.expect("shutdown notice in time") {
            Ok(ServerMsg::Reject { reason, code }) => {
                assert_eq!(reason, "server shutting down", "SIG{signal}");
                assert_eq!(code, RejectCode::ShuttingDown, "SIG{signal}");
                break;
            }
            Ok(_) => continue,
            Err(e) => panic!("SIG{signal}: connection ended without the shutdown notice: {e}"),
        }
    }
    let exit = timeout(Duration::from_secs(15), running.child.wait()).await.expect("server exits in time").expect("wait");
    let log = running.log.await.unwrap_or_default();
    assert_eq!(exit.code(), Some(0), "SIG{signal}: graceful shutdown must exit 0; stderr:\n{log}");
    assert_eq!(running.stdout.next_line().await.expect("read stdout"), None, "SIG{signal}: nothing but the READY line on stdout");
    assert!(log.contains("FIXTURE"), "the fixture area must be labeled in the log:\n{log}");
    assert!(log.contains("ephemeral=true"), "an ephemeral store says so:\n{log}");
    assert!(!log.contains("fmvt_"), "no token may reach the log:\n{log}");
}

/// The FIXTURE server has no battle data: even with `--debug-encounters` it says so at startup, keeps
/// the READY line exactly as it was, and refuses a battle request cleanly (the session goes on).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fixture_server_with_debug_encounters_warns_and_refuses_battle_requests_cleanly() {
    let env_state = tempfile::tempdir().expect("tempdir");
    let server = start(&["--ephemeral", "--debug-encounters"], env_state.path()).await;
    // Battle support does not change the READY line: protocol, build, addr, area, fixture.
    assert_eq!(server.ready.split(' ').count(), 6, "{}", server.ready);
    assert!(server.ready.starts_with(&format!("READY protocol={PROTOCOL_VERSION} ")), "{}", server.ready);

    let (mut client, welcome) = hello(server.addr, Auth::Register { display_name: "Probe".into() }).await;
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    let info = FixtureAdapter::new().list_areas().remove(0);
    let (area, fingerprint) = match read_msg::<_, ServerMsg>(&mut client).await.unwrap() {
        ServerMsg::AreaManifest { area, fingerprint, .. } => (area, fingerprint),
        other => panic!("expected AreaManifest, got {other:?}"),
    };
    write_msg(&mut client, &ClientMsg::JoinArea { area, fingerprint, provenance: info.provenance.summary() }).await.unwrap();
    write_msg(&mut client, &ClientMsg::RequestEncounter { formation_id: None }).await.unwrap();
    let rejected = loop {
        match timeout(Duration::from_secs(10), read_msg::<_, ServerMsg>(&mut client)).await.expect("an answer in time") {
            Ok(ServerMsg::BattleRejected { battle_id, reason }) => break (battle_id, reason),
            Ok(ServerMsg::Reject { reason, .. }) => panic!("the session was ended instead of the request refused: {reason}"),
            Ok(_) => continue,
            Err(e) => panic!("connection ended: {e}"),
        }
    };
    assert_eq!(rejected, (None, fmv_protocol::BattleRejection::Unsupported));

    let (status, log) = server.stop("TERM").await;
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(log.contains("no effect"), "the operator is told that --debug-encounters does nothing here:\n{log}");
    assert!(log.contains("battles unsupported") || log.contains("no battle data"), "{log}");
}

/// `--debug-limit-full` is a debug flag: alone it is a usage error (exit code 2, nothing printed or bound), and
/// on a server without battle data (the FIXTURE one) it says at start-up that it does nothing there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_debug_limit_flag_needs_debug_encounters_and_a_fixture_server_says_it_has_no_effect() {
    let env_state = tempfile::tempdir().expect("tempdir");
    let state = env_state.path().to_str().expect("utf-8 temp path");
    let out = run_failing(&["--ephemeral", "--debug-limit-full"], state).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--debug-encounters"), "the usage error names the flag it needs: {stderr}");
    assert!(out.stdout.is_empty(), "no READY line");

    let server = start(&["--ephemeral", "--debug-encounters", "--debug-limit-full"], env_state.path()).await;
    let (status, log) = server.stop("TERM").await;
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(log.contains("--debug-limit-full has no effect"), "the operator is told that the flag does nothing here:\n{log}");
}

/// Accounts live in the state directory: a registration made against one process is honoured by the
/// next process on the same `--state-dir`, the directory is held locked while a server runs, and
/// a refused second server says so without printing a READY line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accounts_persist_across_processes_and_the_state_directory_is_locked() {
    let sandbox = tempfile::tempdir().expect("tempdir");
    let state = sandbox.path().join("accounts");
    let state_arg = state.to_str().expect("utf-8 path");

    // Process 1 registers an account.
    let first = start(&["--state-dir", state_arg], sandbox.path()).await;
    let (mut client, welcome) = hello(first.addr, Auth::Register { display_name: "Persisted".into() }).await;
    let (account, token) = match welcome {
        ServerMsg::Welcome { account, issued_token, .. } => (account, issued_token.expect("a token").expose().to_string()),
        other => panic!("expected Welcome, got {other:?}"),
    };
    write_msg(&mut client, &ClientMsg::Leave).await.unwrap();

    // While it runs, a second server on the same directory is refused: exit 1, no READY line.
    let out = timeout(
        Duration::from_secs(60),
        Command::new(BIN)
            .args(["--bind", "127.0.0.1:0", "--state-dir", state_arg])
            .env("FMV_STATE_DIR", sandbox.path())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the second server must fail promptly")
    .expect("run fmv-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("locked"), "the error says the directory is locked: {stderr}");
    assert!(out.stdout.is_empty(), "no READY line from a refused server");

    let (status, log) = first.stop("TERM").await;
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(!log.contains(&token), "the token must never be logged");
    assert!(log.contains("account store open"), "{log}");

    // Process 2 on the same directory: the token logs the same account in; nothing is issued.
    let second = start(&["--state-dir", state_arg], sandbox.path()).await;
    let (_client, welcome) = hello(second.addr, Auth::Token(Secret::new(&token))).await;
    match welcome {
        ServerMsg::Welcome { account: again, issued_token, .. } => {
            assert_eq!(again, account, "the same account after a restart");
            assert!(issued_token.is_none());
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    // The name is still taken.
    let (_c, refused) = hello(second.addr, Auth::Register { display_name: "persisted".into() }).await;
    assert!(
        matches!(refused, ServerMsg::Reject { ref reason, code: RejectCode::NameTaken } if reason.contains("already taken")),
        "{refused:?}"
    );
    let (status, log) = second.stop("TERM").await;
    assert_eq!(status.code(), Some(0), "{log}");

    // Account files are private to the owner (they hold token hashes).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in ["index.json", "index.log"] {
            let mode = std::fs::metadata(state.join(file)).unwrap_or_else(|e| panic!("{file}: {e}")).permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{file}: state files must not be group/other accessible (mode {mode:o})");
        }
    }
}

/// A flood of junk connections must not be a flood of log lines: each one used to write an INFO line
/// ("closing connection"), which fills a disk under a sustained flood. Past a small budget the lines
/// go to the debug level, and ONE line says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_junk_connections_cannot_fill_the_log() {
    let env_state = tempfile::tempdir().expect("tempdir");
    let server = start(&["--ephemeral"], env_state.path()).await;
    let junk = 300;
    let flood_started = std::time::Instant::now();
    for _ in 0..junk {
        let mut socket = TcpStream::connect(server.addr).await.expect("connect");
        // A frame that is not a message: refused as malformed. The client hangs up as soon as it has
        // the answer, so the server does not wait around for it.
        let garbage = b"this is not json";
        let mut frame = u32::try_from(garbage.len()).expect("small").to_le_bytes().to_vec();
        frame.extend_from_slice(garbage);
        socket.write_all(&frame).await.expect("send");
        let answer = timeout(Duration::from_secs(10), read_msg::<_, ServerMsg>(&mut socket)).await.expect("an answer in time");
        assert!(matches!(answer, Ok(ServerMsg::Reject { code: RejectCode::BadRequest, .. })), "{answer:?}");
    }
    // The budget renews every 10 s: a slow machine may cross a window boundary once or twice.
    let windows = flood_started.elapsed().as_secs() / 10 + 2;
    // The server is still fine afterwards: a real client registers.
    let (_client, welcome) = hello(server.addr, Auth::Register { display_name: "AfterTheFlood".into() }).await;
    assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");

    let (status, log) = server.stop("TERM").await;
    assert_eq!(status.code(), Some(0), "{log}");
    let written = log.lines().filter(|l| l.contains("closing connection")).count() as u64;
    // All the junk came from one address: its small per-address budget per window, then a notice.
    assert!(written >= 1, "the first few are still logged:\n{log}");
    assert!(written <= 4 * windows, "{written} 'closing connection' lines for {junk} junk connections:\n{log}");
    let notices = log.lines().filter(|l| l.contains("being logged less")).count() as u64;
    assert!((1..=windows).contains(&notices), "one line per window says that lines are held back ({notices}):\n{log}");
    assert!(log.lines().count() < 60, "the whole log of the run stays small:\n{log}");
}

/// Run the server with arguments that must make it fail at startup, hermetically: the dev-port
/// is never used (`--bind` port 0), `$FMV_STATE_DIR` points at `state_dir` (so the owner's real
/// library, which exists on a machine that imported FF7, can never be found by accident), and a
/// server that unexpectedly starts serving is cut off by the timeout instead of hanging the test.
async fn run_failing(args: &[&str], state_dir: &str) -> std::process::Output {
    timeout(
        Duration::from_secs(60),
        Command::new(BIN)
            .args(["--bind", "127.0.0.1:0"])
            .args(args)
            .env("FMV_STATE_DIR", state_dir)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{args:?}: the server must fail promptly (it started serving instead?)"))
    .expect("run fmv-server")
}

#[tokio::test]
async fn startup_failures_are_clear_and_nonzero() {
    // FF7 without owned content fails before anything is bound or printed, and says how to fix it.
    let empty = tempfile::tempdir().expect("tempdir");
    let state = empty.path().to_str().expect("utf-8 temp path");
    for args in [
        vec!["--game", "ff7"], // the default library is resolved through $FMV_STATE_DIR (empty here)
        vec!["--game", "ff7", "--ff7-library", state],
        vec!["--ff7-library", state],
        vec!["--game", "fixture", "--ff7-library", state], // an FF7 source flag always means FF7
    ] {
        let out = run_failing(&args, state).await;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?} must exit nonzero; stderr: {stderr}");
        assert!(stderr.contains(NEEDS_IMPORT), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}: no READY line may be printed, got {:?}", out.stdout);
    }
    // A direct archive that is not there is named as the archive, not as a library problem.
    let out = run_failing(&["--ff7-flevel", "/nonexistent/owned/flevel.lgp"], state).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("flevel.lgp") && !stderr.contains("/nonexistent"), "no local path in the message: {stderr}");
    assert!(out.stdout.is_empty());
    // A set that is not in the library.
    let out = run_failing(&["--ff7-library", state, "--ff7-set", "ff7-pc-nope-00000000"], state).await;
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());

    // An account store directory that cannot be used (it is a file): a clear error, no READY line.
    let file = empty.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let out = run_failing(&["--state-dir", file.to_str().unwrap()], state).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("account store"), "{stderr}");
    assert!(out.stdout.is_empty());

    // Unknown area: the error names the areas that do exist.
    let out = run_failing(&["--ephemeral", "--area", "nope"], state).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(FIXTURE_AREA_KEY), "{stderr}");
    assert!(out.stdout.is_empty());

    // Out-of-range flags are refused by the argument parser (exit code 2), not by a panic later.
    let out = run_failing(&["--ephemeral", "--tick-hz", "0"], state).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    // --ephemeral and --state-dir contradict each other.
    let out = run_failing(&["--ephemeral", "--state-dir", state], state).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
