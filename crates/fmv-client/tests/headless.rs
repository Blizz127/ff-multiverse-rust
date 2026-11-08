//! Integration tests: the real `fmv-client` binary against a scripted fake server that lives in
//! this file (protocol v2: accounts, avatars, parties). They do not depend on `fmv-server`. All
//! content is the synthetic FIXTURE area.
//!
//! Hermetic by construction: every client process is started through [`Sandbox::run`], which
//! always passes `--state-dir <tempdir>` and sets `FMV_STATE_DIR`, `XDG_DATA_HOME` and `HOME` to
//! places inside that tempdir, so no test can read or write the real state directory.
//!
//! The fake server behaves like the real one where it matters to a client: a registration creates
//! an account and issues its token exactly once, a token logs in (an unknown one is refused with
//! "invalid or unknown token"), the player's entity carries the name the SERVER stored, and party
//! requests are answered reactively with numbered `PartyEvent`s (never on a timer).

use fmv_adapter_api::{GameAdapter, MoveResult};
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, PhaseKey, Pos, SessionId};
use fmv_fixture::FixtureAdapter;
use fmv_protocol::{
    read_msg, write_msg, Auth, ClientMsg, InstanceKey, InviteId, Party, PartyEvent, PartyFailure, PartyId, PartyName, Placement,
    Secret, ServerMsg, PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWrite;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Barrier};

// ---------------------------------------------------------------------------------------------
// The fake server
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
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

/// What the fake does about parties, reactively (every answer follows a request or the join).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PartyPlay {
    /// Nothing: no party traffic at all.
    Nothing,
    /// A `PartyInvite` for entity 8 is announced (`InviteSent`) and accepted at once
    /// (`PartyFormed`); half a second later the peer leaves again (`MemberLeft`), so the final
    /// party size is 1 while the largest was 2.
    Inviter,
    /// Like `Inviter`, but the FIRST `PartyInvite` is refused (`PartyRejected`).
    InviterRefusedOnce,
    /// Right after the join the peer invites the client (`InviteSent`); the client's
    /// `PartyAccept` is answered with `PartyFormed`.
    Invitee,
    /// Right after the join one party event arrives with a gap in `seq` (event 1 "was lost");
    /// the client's `PartySync` is answered with a `PartyState`.
    GapThenSync,
}

#[derive(Clone)]
struct Script {
    kind: Kind,
    party: PartyPlay,
    /// If set, the Welcome waits until every holder of this barrier reached it (so two
    /// connections answer at the same instant and the clients really race).
    rendezvous: Option<Arc<Barrier>>,
}

impl Script {
    fn new(kind: Kind) -> Self {
        Script { kind, party: PartyPlay::Nothing, rendezvous: None }
    }
    fn happy() -> Self {
        Script::new(Kind::Happy)
    }
    fn party(mut self, party: PartyPlay) -> Self {
        self.party = party;
        self
    }
    fn rendezvous(mut self, barrier: &Arc<Barrier>) -> Self {
        self.rendezvous = Some(Arc::clone(barrier));
        self
    }
}

#[derive(Default, Debug)]
struct Report {
    received: Vec<ClientMsg>,
    /// When each message of `received` arrived, in the same order.
    received_at: Vec<Instant>,
}

impl Report {
    fn push(&mut self, at: Instant, msg: ClientMsg) {
        self.received.push(msg);
        self.received_at.push(at);
    }

    fn count(&self, pred: impl Fn(&ClientMsg) -> bool) -> usize {
        self.received.iter().filter(|m| pred(m)).count()
    }
}

/// The accounts of the fake server, shared by all its connections.
#[derive(Default)]
struct Accounts {
    by_token: HashMap<String, (AccountId, String)>,
    next: u64,
}

/// The token the fake issues to the `n`-th registered account, so tests know what to look for.
fn issued_token(n: u64, name: &str) -> String {
    let letters: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
    format!("fmvt_fake_{n}_{letters}")
}

/// What the server makes of a `Hello`: the account, the name it stores, the token it issues.
fn authenticate(db: &Mutex<Accounts>, auth: &Auth) -> Result<(AccountId, String, Option<Secret>), &'static str> {
    let mut db = db.lock().expect("accounts lock");
    match auth {
        Auth::Register { display_name } => {
            if db.by_token.values().any(|(_, n)| n.eq_ignore_ascii_case(display_name)) {
                return Err("display name already taken");
            }
            db.next += 1;
            let (id, token) = (AccountId(db.next), issued_token(db.next, display_name));
            db.by_token.insert(token.clone(), (id, display_name.clone()));
            Ok((id, display_name.clone(), Some(Secret::new(token))))
        }
        Auth::Token(secret) => match db.by_token.get(secret.expose()) {
            Some((id, name)) => Ok((*id, name.clone(), None)),
            None => Err("invalid or unknown token"),
        },
    }
}

fn avatar(name: &str) -> AvatarAppearance {
    AvatarAppearance { display_name: name.into(), tint: [200, 100, 50], appearance_ref: None }
}

fn entity(id: u64, name: &str, x: f32, y: f32) -> EntitySnapshot {
    EntitySnapshot { id: EntityId(id), pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk, avatar: avatar(name) }
}

/// The peer account of the party scripts (entity 8). Distinct from every issued account id.
const PEER: AccountId = AccountId(900);

/// Send one numbered party event (`seq` counts the events delivered to this client).
async fn party_event<W: AsyncWrite + Unpin>(wr: &mut W, seq: &mut u64, event: PartyEvent, names: &[PartyName]) {
    *seq += 1;
    let msg = ServerMsg::PartyEvent { seq: *seq, event, names: names.to_vec() };
    let _ = write_msg(wr, &msg).await;
}

fn spawn_fake_server(script: Script) -> (u16, std::thread::JoinHandle<Report>) {
    let (port, handle) = spawn_fake_server_n(vec![script]);
    (port, std::thread::spawn(move || handle.join().expect("server thread").remove(0)))
}

/// A fake server that serves one connection per script, in the order they arrive, all handled
/// concurrently, sharing one set of accounts. The reports come back in the same order.
fn spawn_fake_server_n(scripts: Vec<Script>) -> (u16, std::thread::JoinHandle<Vec<Report>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(serve(listener, scripts))
    });
    (port, handle)
}

async fn serve(listener: std::net::TcpListener, scripts: Vec<Script>) -> Vec<Report> {
    let listener = TcpListener::from_std(listener).unwrap();
    let db = Arc::new(Mutex::new(Accounts::default()));
    let mut handlers = Vec::new();
    for script in scripts {
        let Ok(Ok((stream, _))) = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await else {
            break;
        };
        handlers.push(tokio::spawn(handle(stream, script, Arc::clone(&db))));
    }
    let mut reports = Vec::new();
    for h in handlers {
        reports.push(h.await.expect("a connection handler panicked"));
    }
    reports
}

async fn handle(stream: TcpStream, script: Script, db: Arc<Mutex<Accounts>>) -> Report {
    let mut report = Report::default();
    let (mut rd, mut wr) = stream.into_split();

    // Read client messages in their own task so the serving loop below can `select!` safely.
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, ClientMsg)>();
    tokio::spawn(async move {
        while let Ok(m) = read_msg::<_, ClientMsg>(&mut rd).await {
            if tx.send((Instant::now(), m)).is_err() {
                break;
            }
        }
    });

    let info = FixtureAdapter::new().list_areas().remove(0);
    let Some((at, hello)) = rx.recv().await else { return report };
    let auth = match &hello {
        ClientMsg::Hello { auth, .. } => auth.clone(),
        other => panic!("the first message must be Hello, got {other:?}"),
    };
    report.push(at, hello);

    if script.kind == Kind::Reject {
        let _ = write_msg(&mut wr, &ServerMsg::Reject { reason: "server full".into() }).await;
        return report;
    }
    let (account, name, issued) = match authenticate(&db, &auth) {
        Ok(ok) => ok,
        Err(reason) => {
            let _ = write_msg(&mut wr, &ServerMsg::Reject { reason: reason.into() }).await;
            return report;
        }
    };
    if let Some(barrier) = &script.rendezvous {
        barrier.wait().await;
    }
    write_msg(
        &mut wr,
        &ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "fake-server/0".into(),
            session: SessionId(1),
            entity: EntityId(7),
            account,
            issued_token: issued,
        },
    )
    .await
    .unwrap();

    let (area, fingerprint, fixture) = match script.kind {
        Kind::TamperedFingerprint => {
            let mut f = info.fingerprint;
            f.0[0] ^= 0xff;
            (info.id.clone(), f, true)
        }
        Kind::OfferFf7 => (AreaId { game: GameId::Ff7, key: "md1stin".into() }, info.fingerprint, false),
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

    let Some((at, reply)) = rx.recv().await else { return report };
    let joined = matches!(reply, ClientMsg::JoinArea { .. });
    report.push(at, reply);
    if !joined {
        return report;
    }

    // The player's entity carries the name the SERVER stored for the account.
    write_msg(
        &mut wr,
        &ServerMsg::AreaSnapshot { area: area.clone(), tick: 1, entities: vec![entity(7, &name, -100.0, 0.0), entity(8, "peer", 100.0, 0.0)], npcs: vec![] },
    )
    .await
    .unwrap();
    let placement = Placement {
        account,
        key: InstanceKey::new(area, PhaseKey(vec![])),
        party: None,
        phase_owner: account,
        story_advance_allowed: true,
    };
    write_msg(&mut wr, &ServerMsg::InstancePlacement { placement }).await.unwrap();

    if script.kind == Kind::DropAfterJoin {
        tokio::time::sleep(Duration::from_millis(400)).await;
        return report; // dropping the stream closes the connection
    }

    let names = vec![
        PartyName { account, name: name.clone(), entity: Some(EntityId(7)) },
        PartyName { account: PEER, name: "peer".into(), entity: Some(EntityId(8)) },
    ];
    let mut seq = 0u64; // party events delivered to this client
    match script.party {
        PartyPlay::Invitee => {
            let sent = PartyEvent::InviteSent { invite: InviteId(5), from: PEER, to: account, expires_at: 60_000 };
            party_event(&mut wr, &mut seq, sent, &names).await;
        }
        PartyPlay::GapThenSync => {
            seq = 1; // event 1 was "lost": the client will see event 2 first
            let sent = PartyEvent::InviteSent { invite: InviteId(6), from: PEER, to: account, expires_at: 60_000 };
            party_event(&mut wr, &mut seq, sent, &names).await;
        }
        _ => {}
    }

    let mut tick = 1u64;
    let mut peer_x = 100.0f32;
    let mut intents = 0;
    let mut rejected = false;
    let mut invites_received = 0;
    let mut peer_leaves_at: Option<Instant> = None;
    let mut timer = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some((at, msg)) = msg else { break };
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
                    ClientMsg::PartyInvite { target } if matches!(script.party, PartyPlay::Inviter | PartyPlay::InviterRefusedOnce) => {
                        invites_received += 1;
                        let refuse = (script.party == PartyPlay::InviterRefusedOnce && invites_received == 1) || *target != EntityId(8);
                        if refuse {
                            let _ = write_msg(&mut wr, &ServerMsg::PartyRejected { error: PartyFailure::UnknownPlayer }).await;
                        } else {
                            let sent = PartyEvent::InviteSent { invite: InviteId(1), from: account, to: PEER, expires_at: 60_000 };
                            party_event(&mut wr, &mut seq, sent, &names).await;
                            let formed = PartyEvent::PartyFormed {
                                party: PartyId(1),
                                invite: InviteId(1),
                                leader: account,
                                members: vec![account, PEER],
                            };
                            party_event(&mut wr, &mut seq, formed, &names).await;
                            peer_leaves_at = Some(Instant::now() + Duration::from_millis(500));
                        }
                    }
                    ClientMsg::PartyAccept { invite } if script.party == PartyPlay::Invitee && *invite == InviteId(5) => {
                        let formed = PartyEvent::PartyFormed {
                            party: PartyId(1),
                            invite: InviteId(5),
                            leader: PEER,
                            members: vec![PEER, account],
                        };
                        party_event(&mut wr, &mut seq, formed, &names).await;
                    }
                    ClientMsg::PartySync if script.party == PartyPlay::GapThenSync => {
                        let state = ServerMsg::PartyState {
                            seq: 3,
                            party: Some(Party { id: PartyId(1), leader: PEER, members: vec![PEER, account] }),
                            invites: vec![],
                            names: names.clone(),
                        };
                        let _ = write_msg(&mut wr, &state).await;
                    }
                    _ => {}
                }
                let leaving = matches!(msg, ClientMsg::Leave);
                report.push(at, msg);
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
                if peer_leaves_at.is_some_and(|at| Instant::now() >= at) {
                    peer_leaves_at = None;
                    let left = PartyEvent::MemberLeft {
                        party: PartyId(1),
                        account: PEER,
                        reason: fmv_protocol::LeaveReason::Left,
                        members: vec![account],
                    };
                    party_event(&mut wr, &mut seq, left, &names).await;
                }
            }
        }
    }
    report
}

// ---------------------------------------------------------------------------------------------
// Running the client
// ---------------------------------------------------------------------------------------------

/// A private state directory for the clients of one test (and everything else a client could
/// resolve a directory from).
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox { dir: tempfile::tempdir().expect("tempdir") }
    }

    fn state(&self) -> &Path {
        self.dir.path()
    }

    fn accounts(&self) -> PathBuf {
        self.state().join("accounts")
    }

    /// The default token file of the fake server on `port`.
    fn token_file(&self, port: u16) -> PathBuf {
        self.accounts().join(format!("127.0.0.1_{port}.token"))
    }

    /// File names in the accounts directory, sorted.
    fn account_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.accounts())
            .map(|rd| rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Run the client with a watchdog so a hung client fails the test instead of hanging it.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[], &[])
    }

    /// [`Sandbox::run`] with environment variables removed or set for the child (applied after the
    /// sandbox's own: a test may point `FMV_STATE_DIR` at a nonexistent library, say; `--state-dir`
    /// keeps the accounts private either way).
    fn run_env(&self, args: &[&str], remove_env: &[&str], set_env: &[(&str, &str)]) -> Output {
        run_in(self.state(), args, remove_env, set_env)
    }
}

/// Run the client with `state` as its state directory (always passed as `--state-dir`, and as
/// `FMV_STATE_DIR`, `XDG_DATA_HOME` and `HOME` inside it, so nothing real can be touched), with a
/// watchdog so a hung client fails the test instead of hanging it.
fn run_in(state: &Path, args: &[&str], remove_env: &[&str], set_env: &[(&str, &str)]) -> Output {
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
    for (k, v) in set_env {
        cmd.env(k, v);
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

/// Run the client in a fresh sandbox (for tests that keep no state between runs).
fn run_client(args: &[&str], remove_env: &[&str]) -> Output {
    Sandbox::new().run_env(args, remove_env, &[])
}

/// The `key=value` fields of a SMOKE line, in order.
fn smoke_fields(line: &str) -> Vec<(String, String)> {
    let mut parts = line.split_whitespace();
    assert_eq!(parts.next(), Some("SMOKE"), "line: {line}");
    parts
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or_else(|| panic!("not key=value: {kv} in {line}"));
            (k.to_string(), v.to_string())
        })
        .collect()
}

/// The contract of `scripts/smoke.sh`: these keys, in this order, and nothing else. The first eight
/// are protocol v2's; `battles` and `victories` were appended in v3 and `area_changes` after them for
/// area travel (fields are only ever appended).
const SMOKE_KEYS: [&str; 11] =
    ["name", "area", "fixture", "peers_seen", "updates", "rejected", "rtt_ms", "party", "battles", "victories", "area_changes"];

/// stdout is exactly one SMOKE line with exactly the documented keys in the documented order.
fn assert_smoke_contract(stdout: &str) -> HashMap<String, String> {
    assert_eq!(stdout.lines().count(), 1, "stdout must be exactly one line, got: {stdout:?}");
    assert_eq!(stdout.split_whitespace().count(), 1 + SMOKE_KEYS.len(), "SMOKE plus {} fields: {stdout:?}", SMOKE_KEYS.len());
    let fields = smoke_fields(stdout.trim_end());
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, SMOKE_KEYS, "the SMOKE keys and their order are a contract: {stdout}");
    assert_eq!(keys[..8], SMOKE_KEYS[..8], "the v2 keys come first, unchanged: {stdout}");
    assert_eq!(keys[..10], SMOKE_KEYS[..10], "the v3 battle fields keep their place: {stdout}");
    assert_eq!(keys.last(), Some(&"area_changes"), "the travel field is the last appended one: {stdout}");
    fields.into_iter().collect()
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

/// `--server 127.0.0.1:<port> [--name <name>] --headless <extra...>`
fn headless_args(port: u16, name: Option<&str>, extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = vec!["--server".into(), format!("127.0.0.1:{port}")];
    if let Some(name) = name {
        v.extend(["--name".to_string(), name.to_string()]);
    }
    v.push("--headless".into());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

fn as_strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// The `Hello` (the first message) of a report: its authentication and tint.
fn hello_of(report: &Report) -> (Auth, [u8; 3]) {
    match &report.received[0] {
        ClientMsg::Hello { protocol_version, auth, tint, .. } => {
            assert_eq!(*protocol_version, PROTOCOL_VERSION);
            (auth.clone(), *tint)
        }
        other => panic!("first message must be Hello, got {other:?}"),
    }
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// A token file as an earlier run (or a user) would have left it.
fn write_token_file(sandbox: &Sandbox, file_name: &str, contents: &str, mode: u32) -> PathBuf {
    std::fs::create_dir_all(sandbox.accounts()).unwrap();
    let path = sandbox.accounts().join(file_name);
    std::fs::write(&path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let _ = mode;
    path
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn headless_bot_joins_walks_and_prints_exactly_one_smoke_line() {
    let (port, server) = spawn_fake_server(Script::happy());
    let args = headless_args(port, Some("botA"), &["--bot", "circle", "--duration", "1.5", "--tint", "336699"]);
    let sandbox = Sandbox::new();
    let out = sandbox.run(&as_strs(&args));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "status {:?}\nstdout: {stdout}\nstderr: {stderr}", out.status);

    // stdout: exactly one SMOKE line with the documented keys, in order (party appended last).
    let smoke = assert_smoke_contract(&stdout);
    // The name is the one the SERVER shows for our entity (here: what the account was registered as).
    assert_eq!(smoke["name"], "botA");
    assert_eq!(smoke["area"], "Fixture/plaza");
    assert_eq!(smoke["fixture"], "true");
    // Entities 8 and 9 are the other players; our own entity 7 never counts.
    assert_eq!(smoke["peers_seen"], "2", "{stdout}");
    assert!(smoke["updates"].parse::<u32>().unwrap() >= 3, "{stdout}");
    assert_eq!(smoke["rejected"], "1", "{stdout}");
    assert!(smoke["rtt_ms"].parse::<i64>().unwrap() >= 0, "a pong was answered: {stdout}");
    assert_eq!(smoke["party"], "0", "this bot never was in a party: {stdout}");

    // stderr: the prominent FIXTURE notice, which account was used, and never the token.
    assert!(stderr.contains("FIXTURE AREA (synthetic, not FF7)"), "{stderr}");
    assert!(stderr.contains("account: registered a new account `botA`; token saved to"), "{stderr}");
    assert!(!stderr.contains("fmvt_") && !stdout.contains("fmvt_"), "the token is never printed: {stderr}");

    // What the server saw.
    let report = server.join().unwrap();
    let local = FixtureAdapter::new().list_areas().remove(0);
    let (auth, tint) = hello_of(&report);
    assert_eq!(auth, Auth::Register { display_name: "botA".into() });
    assert_eq!(tint, [0x33, 0x66, 0x99]);
    match &report.received[1] {
        ClientMsg::JoinArea { area, fingerprint, provenance } => {
            assert_eq!(*area, local.id);
            assert_eq!(*fingerprint, local.fingerprint);
            assert_eq!(*provenance, local.provenance.summary(), "JoinArea carries the client's own provenance");
        }
        other => panic!("second message must be JoinArea, got {other:?}"),
    }
    assert!(
        matches!(report.received.last(), Some(ClientMsg::Leave)),
        "Leave must be the last message: {:?}",
        report.received.last()
    );
    assert!(report.received.iter().any(|m| matches!(m, ClientMsg::Ping { .. })), "a Ping is sent");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartyInvite { .. } | ClientMsg::PartyAccept { .. })), 0, "no party flag, no party requests");

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
        assert!(matches!(area.validate_move(*p, *p), MoveResult::Accepted(_)), "intent off the walkable area: {p:?}");
    }
}

/// `--travel` toward an area that no exit of the current one leads to: the bot says so on stderr
/// instead of wandering off, the run still ends well, and the SMOKE line says no area changed
/// (`area_changes=0`, its last field).
#[test]
fn a_travel_bot_with_no_exit_to_its_target_says_so_and_the_run_still_ends_well() {
    let (port, server) = spawn_fake_server(Script::happy());
    let args = headless_args(port, Some("botT"), &["--bot", "circle", "--duration", "1.5", "--travel", "elsewhere"]);
    let sandbox = Sandbox::new();
    let out = sandbox.run(&as_strs(&args));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "status {:?}\nstdout: {stdout}\nstderr: {stderr}", out.status);
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(smoke["area"], "Fixture/plaza");
    assert_eq!(smoke["area_changes"], "0", "{stdout}");
    assert!(stderr.contains("travel behaviour: walk through the exits to elsewhere"), "{stderr}");
    assert!(stderr.contains("travel: gave up: no exit of Fixture/plaza leads to elsewhere"), "{stderr}");
    let report = server.join().unwrap();
    assert!(matches!(report.received.last(), Some(ClientMsg::Leave)), "{:?}", report.received.last());
}

/// A display name with spaces, `=` and `%` must not break the one-line, whitespace-separated
/// key=value contract: the SMOKE line carries it percent-encoded. (The name is cleaned first, as
/// the server cleans it: the line separator in it is dropped, by `--name` and by the server's
/// own entity name alike, so the expected value is what `sanitize_name` leaves.)
#[test]
fn an_awkward_display_name_stays_one_token_in_the_smoke_line() {
    let awkward = "Red XIII=100%\u{2028}x";
    let cleaned = AvatarAppearance::sanitize_name(awkward).expect("a visible name");
    assert_eq!(cleaned, "Red XIII=100%x");
    let (port, server) = spawn_fake_server(Script::happy());
    let args = headless_args(port, Some(awkward), &["--bot", "idle", "--duration", "0.3"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(!stdout.contains('\u{2028}'), "no raw separator in the SMOKE line: {stdout:?}");
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(percent_decode(&smoke["name"]), cleaned, "{stdout}");
    assert!(smoke["name"].contains("%20") && smoke["name"].contains("%3D") && smoke["name"].contains("%25"), "{stdout}");
    assert_eq!(smoke["area"], "Fixture/plaza");
    let report = server.join().unwrap();
    assert_eq!(hello_of(&report).0, Auth::Register { display_name: cleaned });
}

/// Registration: the token the server issues is saved (private, two lines) at once; the next run
/// needs no `--name` and logs in with that token; and the name shown is the server's.
#[test]
fn registering_saves_the_token_and_the_next_run_logs_in_with_it() {
    let sandbox = Sandbox::new();
    let (port, server) = spawn_fake_server_n(vec![Script::happy(), Script::happy()]);
    let first = sandbox.run(&as_strs(&headless_args(port, Some("botR"), &["--duration", "0.4"])));
    let (stdout, stderr) = (text(&first.stdout), text(&first.stderr));
    assert!(first.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_smoke_contract(&stdout);

    // The token file: `<server-key>.token`, two lines, private.
    let file = sandbox.token_file(port);
    let saved = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("no token file {}: {e}\n{stderr}", file.display()));
    let token = issued_token(1, "botR");
    assert_eq!(saved, format!("{token}\nname=botR\n"));
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&file), 0o600, "token files are private");
        assert_eq!(mode_of(&sandbox.accounts()), 0o700, "the accounts directory is private");
    }
    assert_eq!(sandbox.account_files(), vec![format!("127.0.0.1_{port}.token")], "nothing else was left behind");
    assert!(stderr.contains("account: registered a new account `botR`; token saved to"), "{stderr}");
    assert!(stderr.contains(&file.display().to_string()), "the line says where: {stderr}");
    assert!(!stderr.contains(&token) && !stdout.contains(&token), "the token is never printed: {stderr}");

    // Second run, no --name: logs in with the saved token.
    let second = sandbox.run(&as_strs(&headless_args(port, None, &["--duration", "0.4"])));
    let (stdout2, stderr2) = (text(&second.stdout), text(&second.stderr));
    assert!(second.status.success(), "stdout: {stdout2}\nstderr: {stderr2}");
    let smoke = assert_smoke_contract(&stdout2);
    assert_eq!(smoke["name"], "botR", "the name comes from the server's profile, not the command line");
    assert!(stderr2.contains("account: logged in with the saved token for \"botR\""), "{stderr2}");
    assert!(!stderr2.contains(&token), "the token is never printed: {stderr2}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), saved, "a login does not rewrite the file");

    let reports = server.join().unwrap();
    assert_eq!(hello_of(&reports[0]).0, Auth::Register { display_name: "botR".into() });
    assert_eq!(hello_of(&reports[1]).0, Auth::Token(Secret::new(token)));
}

/// A token the server does not know: the client explains what to do (and where the file is),
/// exits with 1, prints no SMOKE line and never prints the token. `--register` then replaces the
/// stale token with a new account's.
#[test]
fn a_stale_token_gets_a_helpful_error_and_register_replaces_it() {
    let sandbox = Sandbox::new();
    let (port, server) = spawn_fake_server_n(vec![Script::happy(), Script::happy()]);
    let stale = "fmvt_stale_secret_value";
    let file = write_token_file(&sandbox, &format!("127.0.0.1_{port}.token"), &format!("{stale}\nname=Cloud\n"), 0o600);

    // Logging in with it is refused.
    let out = sandbox.run(&as_strs(&headless_args(port, None, &["--duration", "0.4"])));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "no SMOKE line when the handshake failed: {stdout:?}");
    assert!(stderr.contains("rejected the token saved for `Cloud`"), "{stderr}");
    assert!(stderr.contains("invalid or unknown token"), "{stderr}");
    assert!(stderr.contains(&file.display().to_string()), "it names the token file: {stderr}");
    assert!(stderr.contains("delete that file") && stderr.contains("--register") && stderr.contains("--name"), "{stderr}");
    assert!(!stderr.contains(stale), "the token is never printed: {stderr}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{stale}\nname=Cloud\n"), "the file is left alone");

    // `--register --name Cloud` creates a new account and replaces the saved token for that name.
    let out = sandbox.run(&as_strs(&headless_args(port, Some("Cloud"), &["--register", "--duration", "0.4"])));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_smoke_contract(&stdout);
    let fresh = issued_token(1, "Cloud");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{fresh}\nname=Cloud\n"), "replaced, not duplicated");
    assert_eq!(sandbox.account_files(), vec![format!("127.0.0.1_{port}.token")]);
    assert!(stderr.contains("account: registered a new account `Cloud`"), "{stderr}");
    #[cfg(unix)]
    assert_eq!(mode_of(&file), 0o600);

    let reports = server.join().unwrap();
    assert_eq!(hello_of(&reports[0]).0, Auth::Token(Secret::new(stale)));
    assert_eq!(hello_of(&reports[1]).0, Auth::Register { display_name: "Cloud".into() });
}

/// `--register` without a name, and a login with nothing saved and no name, are refused before any
/// connection is attempted.
#[test]
fn a_missing_name_is_reported_before_anything_is_sent() {
    let sandbox = Sandbox::new();
    // Nothing saved, no --name: a runtime error (1) that says what to do; nothing listens on port 1.
    let out = sandbox.run(&["--server", "127.0.0.1:1", "--headless"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("no saved account for 127.0.0.1:1") && stderr.contains("--name"), "{stderr}");
    assert!(!stderr.contains("connecting"), "must not touch the network: {stderr}");
    assert!(!sandbox.accounts().exists(), "looking for an account creates nothing");
    // --register needs --name: a usage error (2), from clap.
    let out = sandbox.run(&["--server", "127.0.0.1:1", "--headless", "--register"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("--name"), "{}", text(&out.stderr));
}

/// A token file that cannot be used is an error naming the file, never a guess; one that other
/// users can read is warned about once; several accounts need a name. None of this needs a server.
#[test]
fn unusable_loose_and_ambiguous_token_files_are_reported_before_connecting() {
    // Unusable: a token without its `name=` line.
    let sandbox = Sandbox::new();
    let file = write_token_file(&sandbox, "127.0.0.1_1.token", "fmvt_only_a_token_secret\n", 0o600);
    let out = sandbox.run(&["--server", "127.0.0.1:1", "--headless"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("unusable") && stderr.contains(&file.display().to_string()), "{stderr}");
    assert!(!stderr.contains("fmvt_only_a_token_secret") && !stderr.contains("connecting"), "{stderr}");

    // Several accounts saved, none chosen.
    let sandbox = Sandbox::new();
    write_token_file(&sandbox, "127.0.0.1_1.tifa.token", "tok_tifa\nname=Tifa\n", 0o600);
    write_token_file(&sandbox, "127.0.0.1_1.aerith.token", "tok_aerith\nname=Aerith\n", 0o600);
    let out = sandbox.run(&["--server", "127.0.0.1:1", "--headless"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("several accounts") && stderr.contains("Tifa") && stderr.contains("Aerith") && stderr.contains("--name"), "{stderr}");
    assert!(!stderr.contains("tok_") && !stderr.contains("connecting"), "{stderr}");

    // Readable by others: warned about, once, before connecting (the connection is refused here).
    #[cfg(unix)]
    {
        let sandbox = Sandbox::new();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        }; // nothing accepts on this port any more
        let file = write_token_file(&sandbox, &format!("127.0.0.1_{port}.token"), "fmvt_loose_secret\nname=Cloud\n", 0o644);
        let out = sandbox.run(&as_strs(&headless_args(port, None, &[])));
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "refused: {stderr}");
        assert_eq!(stderr.matches("readable by other users").count(), 1, "warned once: {stderr}");
        assert!(stderr.contains("644") && stderr.contains("chmod 600") && stderr.contains(&file.display().to_string()), "{stderr}");
        assert!(!stderr.contains("fmvt_loose_secret"), "{stderr}");
        assert!(stderr.contains("cannot connect to"), "{stderr}");
    }
}

/// The inviting bot: invites the lowest visible entity (8), the server announces and accepts, and
/// the SMOKE line reports the PEAK party size (the peer leaves again half a second later).
#[test]
fn the_inviting_bot_forms_a_party_and_reports_its_peak_size() {
    let (port, server) = spawn_fake_server(Script::happy().party(PartyPlay::Inviter));
    let args = headless_args(port, Some("inviter"), &["--party-invite-first-peer", "--duration", "4", "--bot", "idle"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(smoke["party"], "2", "the largest party this client was in: {stdout}");
    // stderr: one `party:` line per event, never on stdout.
    let party_lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with("party: ")).collect();
    assert!(party_lines.iter().any(|l| l.contains("invited")), "the invite is logged: {stderr}");
    assert!(party_lines.iter().any(|l| l.contains("formed")), "{stderr}");
    assert!(party_lines.iter().any(|l| l.contains("left")), "the peer's departure is logged: {stderr}");
    assert!(!stdout.contains("party:"), "{stdout}");

    let report = server.join().unwrap();
    let invites: Vec<&ClientMsg> = report.received.iter().filter(|m| matches!(m, ClientMsg::PartyInvite { .. })).collect();
    assert_eq!(invites, vec![&ClientMsg::PartyInvite { target: EntityId(8) }], "exactly one invite, to the lowest entity id");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartyAccept { .. })), 0, "it only invites");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartySync)), 0, "the event numbers had no gap");
}

/// A refused invite is retried (no sooner than a second later) and the bot still ends in a party.
#[test]
fn a_refused_invite_is_retried_a_second_later() {
    let (port, server) = spawn_fake_server(Script::happy().party(PartyPlay::InviterRefusedOnce));
    let args = headless_args(port, Some("retrier"), &["--party-invite-first-peer", "--duration", "6", "--bot", "idle"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(assert_smoke_contract(&stdout)["party"], "2", "{stdout}\n{stderr}");
    assert!(stderr.lines().any(|l| l.starts_with("party: request refused")), "the refusal is logged: {stderr}");

    let report = server.join().unwrap();
    let times: Vec<Instant> = report
        .received
        .iter()
        .zip(&report.received_at)
        .filter(|(m, _)| matches!(m, ClientMsg::PartyInvite { .. }))
        .map(|(_, t)| *t)
        .collect();
    assert_eq!(times.len(), 2, "one refused invite and one retry: {:?}", report.received);
    let gap = times[1].duration_since(times[0]);
    assert!(gap >= Duration::from_millis(950), "retried after {gap:?}, sooner than once a second");
}

/// The accepting bot: answers the invite it is sent exactly once, and ends in a party of two.
#[test]
fn the_accepting_bot_accepts_once_and_reports_a_party_of_two() {
    let (port, server) = spawn_fake_server(Script::happy().party(PartyPlay::Invitee));
    let args = headless_args(port, Some("acceptor"), &["--party-accept", "--duration", "3", "--bot", "idle"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(smoke["party"], "2", "{stdout}\n{stderr}");
    assert!(stderr.lines().any(|l| l.starts_with("party: ") && l.contains("peer invited")), "{stderr}");

    let report = server.join().unwrap();
    let accepts: Vec<&ClientMsg> = report.received.iter().filter(|m| matches!(m, ClientMsg::PartyAccept { .. })).collect();
    assert_eq!(accepts, vec![&ClientMsg::PartyAccept { invite: InviteId(5) }], "exactly one accept, for that invite");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartyInvite { .. })), 0, "it only accepts");
}

/// Without `--party-accept` an invite is left alone: the bot is not in a party afterwards.
#[test]
fn a_bot_without_party_flags_ignores_invites() {
    let (port, server) = spawn_fake_server(Script::happy().party(PartyPlay::Invitee));
    let args = headless_args(port, Some("shy"), &["--duration", "1.5", "--bot", "idle"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(assert_smoke_contract(&stdout)["party"], "0", "{stdout}");
    let report = server.join().unwrap();
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartyAccept { .. } | ClientMsg::PartyInvite { .. })), 0);
}

/// Event numbers with a gap make the client ask for the full state (once, not in a loop), and the
/// answering `PartyState` is applied.
#[test]
fn a_gap_in_party_events_makes_the_client_ask_for_the_state() {
    let (port, server) = spawn_fake_server(Script::happy().party(PartyPlay::GapThenSync));
    let args = headless_args(port, Some("syncer"), &["--duration", "2.5", "--bot", "idle"]);
    let out = run_client(&as_strs(&args), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(smoke["party"], "2", "the PartyState (leader peer, members peer + me) was applied: {stdout}\n{stderr}");
    assert!(stderr.lines().any(|l| l.starts_with("party: ") && l.contains("lost")), "the gap is logged: {stderr}");
    assert!(stderr.lines().any(|l| l.starts_with("party: state synced")), "{stderr}");
    let report = server.join().unwrap();
    assert_eq!(report.count(|m| matches!(m, ClientMsg::PartySync)), 1, "asked once, answered once: {:?}", report.received);
}

/// The scenario of `scripts/smoke.sh`: two bots start at the same moment against one server with
/// ONE shared state directory. Both register at the same instant (the fake server answers both
/// together), so both save a token at the same instant; they must end up with two separate token
/// files and never log in as each other.
#[test]
fn two_bots_sharing_a_state_dir_get_separate_accounts_and_never_log_in_as_each_other() {
    let sandbox = Sandbox::new();
    let barrier = Arc::new(Barrier::new(2));
    let (port, server) = spawn_fake_server_n(vec![
        Script::happy().rendezvous(&barrier),
        Script::happy().rendezvous(&barrier),
        Script::happy(),
        Script::happy(),
    ]);
    let run_bot = |name: &'static str| {
        let state = sandbox.state().to_path_buf();
        let args = headless_args(port, Some(name), &["--duration", "0.6", "--bot", "idle"]);
        std::thread::spawn(move || run_in(&state, &as_strs(&args), &[], &[]))
    };
    let (a, b) = (run_bot("botA"), run_bot("botB"));
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    for (who, out) in [("botA", &a), ("botB", &b)] {
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(out.status.success(), "{who}: stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(assert_smoke_contract(&stdout)["name"], who);
    }
    // Two separate files: whichever bot won the default name, the other has its own.
    let files = sandbox.account_files();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.contains(&format!("127.0.0.1_{port}.token")), "{files:?}");
    assert!(files.iter().all(|f| !f.starts_with('.')), "no temporary file is left behind: {files:?}");
    let mut named = 0;
    for f in &files {
        let path = sandbox.accounts().join(f);
        let content = std::fs::read_to_string(&path).unwrap();
        let (token, name_line) = content.split_once('\n').expect("two lines");
        let name = name_line.strip_prefix("name=").and_then(|n| n.strip_suffix('\n')).expect("a name= line");
        assert!(["botA", "botB"].contains(&name), "{content:?}");
        assert!(token.starts_with("fmvt_fake_") && token.ends_with(&format!("_{name}")), "each file keeps its own account's token: {content:?}");
        #[cfg(unix)]
        assert_eq!(mode_of(&path), 0o600);
        named += usize::from(f != &format!("127.0.0.1_{port}.token"));
    }
    assert_eq!(named, 1, "one default file and one name-specific file: {files:?}");

    // Logging in again, each by its own name: the server sees each bot's own token, and the name
    // it reports back is that account's.
    for who in ["botA", "botB"] {
        let out = sandbox.run(&as_strs(&headless_args(port, Some(who), &["--duration", "0.4", "--bot", "idle"])));
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(out.status.success(), "{who}: stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(assert_smoke_contract(&stdout)["name"], who, "logged in as someone else: {stderr}");
        assert!(stderr.contains(&format!("account: logged in with the saved token for \"{who}\"")), "{stderr}");
    }
    let reports = server.join().unwrap();
    let tokens: Vec<Auth> = reports.iter().map(|r| hello_of(r).0).collect();
    // The two registrations (in whichever order they arrived) and then one token login each.
    let registered: Vec<&str> = tokens
        .iter()
        .filter_map(|a| match a {
            Auth::Register { display_name } => Some(display_name.as_str()),
            Auth::Token(_) => None,
        })
        .collect();
    assert_eq!(registered.len(), 2, "{tokens:?}");
    assert!(registered.contains(&"botA") && registered.contains(&"botB"), "{tokens:?}");
    let logins: Vec<&Secret> = tokens
        .iter()
        .filter_map(|a| match a {
            Auth::Token(t) => Some(t),
            Auth::Register { .. } => None,
        })
        .collect();
    assert_eq!(logins.len(), 2, "{tokens:?}");
    assert_ne!(logins[0], logins[1], "the two logins used two different tokens");
}

/// Every way a start-up can fail has its own exit code and a clear message, and none of them
/// prints a SMOKE line or panics. Cases that need a server use the scripted fake server; the
/// rest never touch the network.
#[test]
fn failures_exit_with_distinct_codes_and_clear_messages() {
    // --- Fake-server failures ---------------------------------------------------------------
    // Tampered fingerprint: the client declines with CannotJoin and exits with the content code.
    // The registration had already succeeded when the content check failed: its token must have
    // been saved first (a registered name is unrecoverable without it).
    let sandbox = Sandbox::new();
    let (port, server) = spawn_fake_server(Script::new(Kind::TamperedFingerprint));
    let out = sandbox.run(&as_strs(&headless_args(port, Some("botB"), &["--duration", "1"])));
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "fingerprint mismatch\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "no SMOKE line when the handshake failed: {stdout:?}");
    assert!(stderr.contains("fingerprint mismatch"), "{stderr}");
    let saved = std::fs::read_to_string(sandbox.token_file(port)).expect("the token was saved before the content check failed");
    assert_eq!(saved, format!("{}\nname=botB\n", issued_token(1, "botB")));
    assert!(!stderr.contains(&issued_token(1, "botB")), "an ordinary failure never prints the token: {stderr}");
    let report = server.join().unwrap();
    assert!(matches!(report.received[0], ClientMsg::Hello { .. }));
    match &report.received[1] {
        ClientMsg::CannotJoin { area, reason } => {
            assert_eq!(area.key, "plaza");
            assert!(reason.contains("fingerprint mismatch"), "{reason}");
        }
        other => panic!("expected CannotJoin, got {other:?}"),
    }
    assert_eq!(report.received.len(), 2, "no JoinArea, no movement: {:?}", report.received);

    // An FF7 area without local FF7 content: refused with the import hint.
    let (port, server) = spawn_fake_server(Script::new(Kind::OfferFf7));
    let out = run_client(&as_strs(&headless_args(port, Some("botC"), &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "ff7 offered\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("needs owned FF7 content imported via fmv-import"), "{stderr}");
    let report = server.join().unwrap();
    assert!(
        matches!(&report.received[1], ClientMsg::CannotJoin { reason, .. } if reason.contains("needs owned FF7 content imported via fmv-import")),
        "{:?}",
        report.received
    );

    // Server Reject: runtime failure (1).
    let (port, server) = spawn_fake_server(Script::new(Kind::Reject));
    let out = run_client(&as_strs(&headless_args(port, Some("botD"), &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "reject\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("server rejected the connection: server full"), "{stderr}");
    let _ = server.join();

    // Connection lost mid-run: the summary is still printed, the exit code is 1, and the bot
    // notices the hang-up instead of waiting out --duration.
    let (port, server) = spawn_fake_server(Script::new(Kind::DropAfterJoin));
    let started = Instant::now();
    let out = run_client(&as_strs(&headless_args(port, Some("botE"), &["--duration", "30"])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(started.elapsed() < Duration::from_secs(20), "the bot must notice the hang-up");
    assert_eq!(out.status.code(), Some(1), "drop after join\nstdout: {stdout}\nstderr: {stderr}");
    let smoke = assert_smoke_contract(&stdout);
    assert_eq!(smoke["peers_seen"], "1");
    assert_eq!(smoke["party"], "0");
    assert!(stderr.contains("connection to the server lost"), "{stderr}");
    let _ = server.join();

    // --- Failures that never reach a server --------------------------------------------------
    // `--game ff7` without a usable library is refused before any connection is attempted
    // (nothing listens on port 1). Hermetic: the library is an explicit nonexistent directory,
    // or $FMV_STATE_DIR names one, so the real library of the machine running the test (which
    // exists once FF7 has been imported) can never be found by accident.
    const NO_STATE: &str = "/nonexistent/fmv-state";
    for (extra, env) in [
        (vec!["--game", "ff7", "--ff7-library", NO_STATE], vec![]),
        (vec!["--ff7-library", NO_STATE], vec![]),
        (vec!["--game", "fixture", "--ff7-library", NO_STATE], vec![]), // an FF7 source flag means FF7
        // No library flag at all: the default lookup goes through $FMV_STATE_DIR, which the sandbox
        // points at its own empty directory (so there is no library to find, and nothing real to read).
        (vec!["--game", "ff7"], vec![]),
    ] {
        let mut args = vec!["--server", "127.0.0.1:1", "--name", "x", "--headless"];
        args.extend(extra.iter().copied());
        let out = Sandbox::new().run_env(&args, &[], &env);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.code(), Some(3), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout:?}");
        assert!(stderr.contains("needs owned FF7 content imported via fmv-import"), "{args:?}: {stderr}");
        assert!(!stderr.contains("connecting"), "{args:?}: must not touch the network: {stderr}");
    }
    // A direct archive that is not there is refused the same way, without echoing its path.
    let out = run_client(
        &["--server", "127.0.0.1:1", "--name", "x", "--headless", "--ff7-flevel", "/nonexistent/owned/flevel.lgp"],
        &[],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("flevel.lgp") && !stderr.contains("/nonexistent"), "{stderr}");
    assert!(!stderr.contains("connecting"), "{stderr}");

    // Connection refused.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // listener dropped: nothing accepts on this port any more
    let out = run_client(&as_strs(&headless_args(port, Some("botF"), &[])), &[]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "refused: {stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("cannot connect to"), "{stderr}");

    // Window mode without a display: a clean error, not a panic.
    let out = run_client(&["--server", "127.0.0.1:9", "--name", "x"], &["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "no display\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("cannot open a window"), "{stderr}");
    assert!(stderr.contains("--headless"), "the error suggests the headless mode: {stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    // Usage errors use clap's exit code 2.
    for args in [
        &["--name", "x", "--bot", "line"][..],                  // --bot needs --headless
        &["--name", "x", "--headless", "--tint", "zzzzzz"][..], // invalid tint
        &["--headless", "--register"][..],                      // --register needs --name
        &["--name", "x", "--party-accept"][..],                 // the party bot flags need --headless
        &["--name", "x", "--party-invite-first-peer"][..],
    ] {
        let out = run_client(args, &[]);
        assert_eq!(out.status.code(), Some(2), "usage error for {args:?}");
    }
}
