//! Process-level tests of the real `fmv-server` binary: the READY/stdout contract, SIGTERM
//! handling and startup failures.

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use fmv_core::AreaId;
use fmv_fixture::FIXTURE_AREA_KEY;
use fmv_protocol::{read_msg, write_msg, ClientMsg, ServerMsg, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_fmv-server");
const FF7_ERROR: &str = "FF7 adapter not wired yet; requires owned FF7 content imported via fmv-import";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prints_one_ready_line_serves_and_exits_zero_on_sigterm_and_sigint() {
    for signal in ["TERM", "INT"] {
        ready_serve_and_stop_on(signal).await;
    }
}

async fn ready_serve_and_stop_on(signal: &str) {
    let mut child = Command::new(BIN)
        .args(["--bind", "127.0.0.1:0"])
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

    // Exactly one stdout line, in the documented format, once the listener is bound.
    let ready = timeout(Duration::from_secs(30), stdout.next_line())
        .await
        .expect("READY within 30 s")
        .expect("read stdout")
        .expect("stdout closed before READY");
    let fields: Vec<&str> = ready.split(' ').collect();
    let [word, protocol, build, addr, area, fixture] = fields[..] else {
        panic!("READY line has the wrong shape: {ready:?}");
    };
    assert_eq!(word, "READY");
    assert_eq!(protocol, format!("protocol={PROTOCOL_VERSION}"));
    assert_eq!(build, format!("build={}", env!("CARGO_PKG_VERSION")));
    let addr: SocketAddr = addr.strip_prefix("addr=").expect("addr= field").parse().expect("socket address");
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0, "the READY line reports the actual bound port");
    assert_eq!(area, "area=Fixture/plaza");
    assert_eq!(fixture, "fixture=true");

    // The process really serves protocol v1.
    let mut client = TcpStream::connect(addr).await.expect("connect");
    write_msg(
        &mut client,
        &ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "binary-test".into(),
            display_name: "Probe".into(),
            tint: [0; 3],
            appearance_ref: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(read_msg::<_, ServerMsg>(&mut client).await.unwrap(), ServerMsg::Welcome { .. }));
    let (area, fingerprint) = match read_msg::<_, ServerMsg>(&mut client).await.unwrap() {
        ServerMsg::AreaManifest { area, fingerprint, fixture, .. } => {
            assert!(fixture);
            (area, fingerprint)
        }
        other => panic!("expected AreaManifest, got {other:?}"),
    };
    assert_eq!(area, AreaId { game: fmv_core::GameId::Fixture, key: FIXTURE_AREA_KEY.into() });
    write_msg(&mut client, &ClientMsg::JoinArea { area, fingerprint }).await.unwrap();
    assert!(matches!(read_msg::<_, ServerMsg>(&mut client).await.unwrap(), ServerMsg::AreaSnapshot { .. }));

    // SIGTERM / SIGINT: joined clients are told, the process exits 0, and stdout stays at one line.
    let pid = child.id().expect("child pid");
    let status = std::process::Command::new("kill").args([format!("-{signal}"), pid.to_string()]).status().expect("run kill");
    assert!(status.success());
    loop {
        match timeout(Duration::from_secs(10), read_msg::<_, ServerMsg>(&mut client)).await.expect("shutdown notice in time") {
            Ok(ServerMsg::Reject { reason }) => {
                assert_eq!(reason, "server shutting down", "SIG{signal}");
                break;
            }
            Ok(_) => continue,
            Err(e) => panic!("SIG{signal}: connection ended without the shutdown notice: {e}"),
        }
    }
    let exit = timeout(Duration::from_secs(15), child.wait()).await.expect("server exits in time").expect("wait");
    let log = log.await.unwrap_or_default();
    assert_eq!(exit.code(), Some(0), "SIG{signal}: graceful shutdown must exit 0; stderr:\n{log}");
    assert_eq!(stdout.next_line().await.expect("read stdout"), None, "SIG{signal}: nothing but the READY line on stdout");
    assert!(log.contains("FIXTURE"), "the fixture area must be labeled in the log:\n{log}");
}

#[tokio::test]
async fn startup_failures_are_clear_and_nonzero() {
    // FF7 is not wired: --game ff7 and/or --ff7-library fail before anything is bound or printed.
    for args in [
        vec!["--game", "ff7"],
        vec!["--game", "ff7", "--ff7-library", "/nonexistent/owned-library"],
        vec!["--ff7-library", "/nonexistent/owned-library"],
    ] {
        let out = Command::new(BIN).args(&args).output().await.expect("run fmv-server");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?} must exit nonzero; stderr: {stderr}");
        assert!(stderr.contains(FF7_ERROR), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}: no READY line may be printed, got {:?}", out.stdout);
    }

    // Unknown area: the error names the areas that do exist.
    let out = Command::new(BIN).args(["--bind", "127.0.0.1:0", "--area", "nope"]).output().await.expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(FIXTURE_AREA_KEY), "{stderr}");
    assert!(out.stdout.is_empty());

    // Out-of-range flags are refused by the argument parser (exit code 2), not by a panic later.
    let out = Command::new(BIN).args(["--tick-hz", "0"]).output().await.expect("run");
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}
