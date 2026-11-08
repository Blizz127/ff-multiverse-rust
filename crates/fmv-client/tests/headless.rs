//! Integration tests: the real `fmv-client` binary against a scripted fake server that lives in
//! this file. They do not depend on `fmv-server`. All content is the synthetic FIXTURE area.

use fmv_adapter_api::{GameAdapter, MoveResult};
use fmv_core::{AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos, SessionId};
use fmv_fixture::FixtureAdapter;
use fmv_protocol::{read_msg, write_msg, ClientMsg, ServerMsg, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
enum Script {
    /// Offer the real fixture area, then act like a small server (updates, a joining peer, one
    /// MoveRejected, pongs).
    Happy,
    /// Offer the fixture area with a tampered fingerprint.
    TamperedFingerprint,
    /// Offer an FF7 area this client has no content for.
    OfferFf7,
    /// Answer Hello with Reject.
    Reject,
    /// Join the client, then hang up after a moment.
    DropAfterJoin,
}

#[derive(Default, Debug)]
struct Report {
    received: Vec<ClientMsg>,
}

fn avatar(name: &str) -> AvatarAppearance {
    AvatarAppearance {
        display_name: name.into(),
        tint: [200, 100, 50],
        appearance_ref: None,
    }
}

fn entity(id: u64, name: &str, x: f32, y: f32) -> EntitySnapshot {
    EntitySnapshot {
        id: EntityId(id),
        pos: Pos::new(x, y, 0.0),
        facing: 0.0,
        anim: AnimState::Walk,
        avatar: avatar(name),
    }
}

fn spawn_fake_server(script: Script) -> (u16, std::thread::JoinHandle<Report>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(serve(listener, script))
    });
    (port, handle)
}

async fn serve(listener: std::net::TcpListener, script: Script) -> Report {
    let mut report = Report::default();
    let listener = TcpListener::from_std(listener).unwrap();
    let Ok(Ok((stream, _))) = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await else {
        return report;
    };
    let (mut rd, mut wr) = stream.into_split();

    // Read client messages in their own task so the serving loop below can `select!` safely.
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    tokio::spawn(async move {
        while let Ok(m) = read_msg::<_, ClientMsg>(&mut rd).await {
            if tx.send(m).is_err() {
                break;
            }
        }
    });

    let info = FixtureAdapter::new().list_areas().remove(0);
    let Some(hello) = rx.recv().await else { return report };
    report.received.push(hello);

    if matches!(script, Script::Reject) {
        let _ = write_msg(
            &mut wr,
            &ServerMsg::Reject {
                reason: "server full".into(),
            },
        )
        .await;
        return report;
    }
    write_msg(
        &mut wr,
        &ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "fake-server/0".into(),
            session: SessionId(1),
            entity: EntityId(7),
        },
    )
    .await
    .unwrap();

    let (area, fingerprint, fixture) = match script {
        Script::TamperedFingerprint => {
            let mut f = info.fingerprint;
            f.0[0] ^= 0xff;
            (info.id.clone(), f, true)
        }
        Script::OfferFf7 => (
            AreaId {
                game: GameId::Ff7,
                key: "md1stin".into(),
            },
            info.fingerprint,
            false,
        ),
        _ => (info.id.clone(), info.fingerprint, true),
    };
    write_msg(
        &mut wr,
        &ServerMsg::AreaManifest {
            area: area.clone(),
            display_name: info.display_name.clone(),
            fingerprint,
            provenance: info.provenance.summary(),
            fixture,
        },
    )
    .await
    .unwrap();

    let Some(reply) = rx.recv().await else { return report };
    let joined = matches!(reply, ClientMsg::JoinArea { .. });
    report.received.push(reply);
    if !joined {
        return report;
    }

    write_msg(
        &mut wr,
        &ServerMsg::AreaSnapshot {
            area,
            tick: 1,
            entities: vec![entity(7, "me", -100.0, 0.0), entity(8, "peer", 100.0, 0.0)],
        },
    )
    .await
    .unwrap();

    if matches!(script, Script::DropAfterJoin) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        return report; // dropping the stream closes the connection
    }

    let mut tick = 1u64;
    let mut peer_x = 100.0f32;
    let mut intents = 0;
    let mut rejected = false;
    let mut timer = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                match &msg {
                    ClientMsg::Ping { nonce } => {
                        let _ = write_msg(&mut wr, &ServerMsg::Pong { nonce: *nonce, tick }).await;
                    }
                    ClientMsg::MoveIntent { seq, .. } => {
                        intents += 1;
                        if intents == 3 && !rejected {
                            rejected = true;
                            let m = ServerMsg::MoveRejected { seq: *seq, corrected: Pos::new(-100.0, 0.0, 0.0) };
                            let _ = write_msg(&mut wr, &m).await;
                        }
                    }
                    _ => {}
                }
                let leaving = matches!(msg, ClientMsg::Leave);
                report.received.push(msg);
                if leaving {
                    break;
                }
            }
            _ = timer.tick() => {
                tick += 1;
                peer_x += 2.0;
                let _ = write_msg(&mut wr, &ServerMsg::EntityUpdate { tick, entities: vec![entity(8, "peer", peer_x, 0.0)] }).await;
                if tick == 5 {
                    let _ = write_msg(&mut wr, &ServerMsg::PlayerJoined { snapshot: entity(9, "late", 0.0, 100.0) }).await;
                }
            }
        }
    }
    report
}

/// Run the client binary with a watchdog so a hung client fails the test instead of hanging it.
fn run_client(args: &[&str], remove_env: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmv-client"));
    cmd.args(args)
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

fn parse_smoke(line: &str) -> HashMap<String, String> {
    let mut parts = line.split_whitespace();
    assert_eq!(parts.next(), Some("SMOKE"), "line: {line}");
    parts
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| panic!("not key=value: {kv} in {line}"));
            (k.to_string(), v.to_string())
        })
        .collect()
}

/// Decode the percent-encoding the client applies to `name=` (every byte outside `A-Za-z0-9-._~`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).expect("two hex digits");
            out.push(u8::from_str_radix(hex, 16).expect("hex"));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("utf-8")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn headless_args(port: u16, name: &str, extra: &[&'static str]) -> Vec<String> {
    let mut v: Vec<String> = [
        "--server".to_string(),
        format!("127.0.0.1:{port}"),
        "--name".into(),
        name.into(),
        "--headless".into(),
    ]
    .to_vec();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
fn headless_bot_joins_walks_and_prints_exactly_one_smoke_line() {
    let (port, server) = spawn_fake_server(Script::Happy);
    let args = headless_args(
        port,
        "botA",
        &["--bot", "circle", "--duration", "1.5", "--tint", "336699"],
    );
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(
        out.status.success(),
        "status {:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );

    // stdout: exactly one SMOKE line, nothing else.
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must be exactly one line, got: {stdout:?}"
    );
    let smoke = parse_smoke(stdout.trim_end());
    assert_eq!(smoke["name"], "botA");
    assert_eq!(smoke["area"], "Fixture/plaza");
    assert_eq!(smoke["fixture"], "true");
    // Entities 8 and 9 are the other players; our own entity 7 never counts.
    assert_eq!(smoke["peers_seen"], "2", "{stdout}");
    assert!(smoke["updates"].parse::<u32>().unwrap() >= 3, "{stdout}");
    assert_eq!(smoke["rejected"], "1", "{stdout}");
    assert!(
        smoke["rtt_ms"].parse::<i64>().unwrap() >= 0,
        "a pong was answered: {stdout}"
    );
    assert_eq!(smoke.len(), 7, "no extra keys: {stdout}");

    // stderr: the prominent FIXTURE notice.
    assert!(stderr.contains("FIXTURE AREA (synthetic, not FF7)"), "{stderr}");

    // What the server saw.
    let report = server.join().unwrap();
    let local = FixtureAdapter::new().list_areas().remove(0);
    match &report.received[0] {
        ClientMsg::Hello {
            protocol_version,
            display_name,
            tint,
            appearance_ref,
            ..
        } => {
            assert_eq!(*protocol_version, PROTOCOL_VERSION);
            assert_eq!(display_name, "botA");
            assert_eq!(*tint, [0x33, 0x66, 0x99]);
            assert!(appearance_ref.is_none());
        }
        other => panic!("first message must be Hello, got {other:?}"),
    }
    match &report.received[1] {
        ClientMsg::JoinArea { area, fingerprint } => {
            assert_eq!(*area, local.id);
            assert_eq!(*fingerprint, local.fingerprint);
        }
        other => panic!("second message must be JoinArea, got {other:?}"),
    }
    assert!(
        matches!(report.received.last(), Some(ClientMsg::Leave)),
        "Leave must be the last message: {:?}",
        report.received.last()
    );
    assert!(
        report.received.iter().any(|m| matches!(m, ClientMsg::Ping { .. })),
        "a Ping is sent"
    );

    let area = FixtureAdapter::new().load_area("plaza").unwrap();
    let intents: Vec<(u32, Pos)> = report
        .received
        .iter()
        .filter_map(|m| match m {
            ClientMsg::MoveIntent { seq, pos, .. } => Some((*seq, *pos)),
            _ => None,
        })
        .collect();
    assert!(intents.len() >= 5, "a moving bot sends intents, got {}", intents.len());
    assert!(intents.len() <= 34, "at most ~20 Hz, got {} in 1.5 s", intents.len());
    for w in intents.windows(2) {
        assert_eq!(w[1].0, w[0].0 + 1, "seq must increase by one: {intents:?}");
    }
    assert_eq!(intents[0].0, 1);
    for (_, p) in &intents {
        assert!(
            matches!(area.validate_move(*p, *p), MoveResult::Accepted(_)),
            "intent off the walkable area: {p:?}"
        );
    }

    // A display name with spaces, `=`, `%` and a Unicode line separator must not break the
    // one-line, whitespace-separated key=value contract: the SMOKE line carries it
    // percent-encoded, while the server still receives the real name in `Hello`.
    let awkward = "Red XIII=100%\u{2028}x";
    let (port, server) = spawn_fake_server(Script::Happy);
    let args = headless_args(port, awkward, &["--bot", "idle", "--duration", "0.3"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(!stdout.contains('\u{2028}'), "no raw separator in the SMOKE line: {stdout:?}");
    assert_eq!(stdout.split_whitespace().count(), 8, "SMOKE + 7 fields: {stdout:?}");
    let smoke = parse_smoke(stdout.trim_end());
    assert_eq!(smoke.len(), 7, "{stdout}");
    assert_eq!(percent_decode(&smoke["name"]), awkward, "{stdout}");
    assert_eq!(smoke["area"], "Fixture/plaza");
    match &server.join().unwrap().received[0] {
        ClientMsg::Hello { display_name, .. } => assert_eq!(display_name, awkward),
        other => panic!("first message must be Hello, got {other:?}"),
    }
}

/// Every way a start-up can fail has its own exit code and a clear message, and none of them
/// prints a SMOKE line or panics. Cases that need a server use the scripted fake server; the
/// rest never touch the network.
#[test]
fn failures_exit_with_distinct_codes_and_clear_messages() {
    // --- Fake-server failures ---------------------------------------------------------------
    // Tampered fingerprint: the client declines with CannotJoin and exits with the content code.
    let (port, server) = spawn_fake_server(Script::TamperedFingerprint);
    let out = run_client(&as_strs(&headless_args(port, "botB", &["--duration", "1"])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(
        out.status.code(),
        Some(3),
        "fingerprint mismatch\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.is_empty(), "no SMOKE line when the handshake failed: {stdout:?}");
    assert!(stderr.contains("fingerprint mismatch"), "{stderr}");
    let report = server.join().unwrap();
    assert!(matches!(report.received[0], ClientMsg::Hello { .. }));
    match &report.received[1] {
        ClientMsg::CannotJoin { area, reason } => {
            assert_eq!(area.key, "plaza");
            assert!(reason.contains("fingerprint mismatch"), "{reason}");
        }
        other => panic!("expected CannotJoin, got {other:?}"),
    }
    assert_eq!(
        report.received.len(),
        2,
        "no JoinArea, no movement: {:?}",
        report.received
    );

    // An FF7 area without local FF7 content: refused with the import hint.
    let (port, server) = spawn_fake_server(Script::OfferFf7);
    let out = run_client(&as_strs(&headless_args(port, "botC", &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(
        out.status.code(),
        Some(3),
        "ff7 offered\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(
        stderr.contains("needs owned FF7 content imported via fmv-import"),
        "{stderr}"
    );
    let report = server.join().unwrap();
    assert!(
        matches!(&report.received[1], ClientMsg::CannotJoin { reason, .. } if reason.contains("needs owned FF7 content imported via fmv-import")),
        "{:?}",
        report.received
    );

    // Server Reject: runtime failure (1).
    let (port, server) = spawn_fake_server(Script::Reject);
    let out = run_client(&as_strs(&headless_args(port, "botD", &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "reject\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(
        stderr.contains("server rejected the connection: server full"),
        "{stderr}"
    );
    let _ = server.join();

    // Connection lost mid-run: the summary is still printed, the exit code is 1, and the bot
    // notices the hang-up instead of waiting out --duration.
    let (port, server) = spawn_fake_server(Script::DropAfterJoin);
    let started = std::time::Instant::now();
    let out = run_client(&as_strs(&headless_args(port, "botE", &["--duration", "30"])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the bot must notice the hang-up"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "drop after join\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert_eq!(parse_smoke(stdout.trim_end())["peers_seen"], "1");
    assert!(stderr.contains("connection to the server lost"), "{stderr}");
    let _ = server.join();

    // --- Failures that never reach a server --------------------------------------------------
    // `--game ff7` is refused before any connection is attempted (nothing listens on port 1).
    let out = run_client(
        &["--server", "127.0.0.1:1", "--name", "x", "--game", "ff7", "--headless"],
        &[],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "--game ff7: {stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("needs owned FF7 content imported via fmv-import"),
        "{stderr}"
    );
    assert!(!stderr.contains("connecting"), "must not touch the network: {stderr}");

    // Connection refused.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // listener dropped: nothing accepts on this port any more
    let out = run_client(&as_strs(&headless_args(port, "botF", &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "refused: {stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("cannot connect to"), "{stderr}");

    // Window mode without a display: a clean error, not a panic.
    let out = run_client(
        &["--server", "127.0.0.1:9", "--name", "x"],
        &["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(
        out.status.code(),
        Some(1),
        "no display\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stderr.contains("cannot open a window"), "{stderr}");
    assert!(
        stderr.contains("--headless"),
        "the error suggests the headless mode: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");

    // Usage errors use clap's exit code 2.
    for args in [
        &["--headless"][..],                                    // --name is required
        &["--name", "x", "--bot", "line"][..],                  // --bot needs --headless
        &["--name", "x", "--headless", "--tint", "zzzzzz"][..], // invalid tint
    ] {
        let out = run_client(args, &[]);
        assert_eq!(out.status.code(), Some(2), "usage error for {args:?}");
    }
}
