//! Shared helpers of the server's integration tests: real TCP sockets, raw protocol clients
//! (`fmv_protocol` `read_msg`/`write_msg`), an in-process server hosting the FIXTURE area.
//!
//! Replies are always awaited with a loop that skips unrelated messages (joins, updates, pongs)
//! and a timeout, so tests never depend on the exact interleaving of server broadcasts.
//!
//! Every in-process server gets its own private (ephemeral) account store unless the test names
//! a `state_dir`, so tests never touch a real state directory and never collide on its lock.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AccountId, EntityId, EntitySnapshot, Pos};
use fmv_fixture::FixtureAdapter;
use fmv_protocol::{
    read_msg, write_msg, Auth, ClientMsg, Placement, ProtoError, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION,
};
use fmv_server::{Server, ServerConfig, ServerError};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

pub const STEP: Duration = Duration::from_secs(5);

static NAME_SEQ: AtomicU32 = AtomicU32::new(1);

/// A display name that no other test client in this process registers (names are unique per
/// server, and registering is how a test client gets an account). At most 24 characters.
pub fn uniq(prefix: &str) -> String {
    format!("{prefix}{}", NAME_SEQ.fetch_add(1, Ordering::Relaxed))
}

pub struct TestServer {
    pub addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), ServerError>>,
}

impl TestServer {
    pub async fn start(config: ServerConfig) -> Self {
        Self::try_start(config).await.expect("bind")
    }

    pub async fn try_start(config: ServerConfig) -> Result<Self, ServerError> {
        let config = ServerConfig { bind: "127.0.0.1:0".parse().unwrap(), ..config };
        let server = Server::bind(config, Box::new(FixtureAdapter::new())).await?;
        let addr = server.local_addr();
        assert!(server.is_fixture());
        assert_eq!(
            server.ready_line(),
            format!("READY protocol={PROTOCOL_VERSION} build={} addr={addr} area=Fixture/plaza fixture=true", env!("CARGO_PKG_VERSION"))
        );
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async move {
            let _ = stopped.await;
        }));
        Ok(Self { addr, stop, task })
    }

    /// Stop the server and wait until everything it held (connections, the account store and its
    /// lock) is gone.
    pub async fn shutdown(self) {
        let _ = self.stop.send(());
        timeout(STEP * 2, self.task).await.expect("server stops in time").expect("server task did not panic").expect("serve returns Ok");
    }
}

pub fn fixture_info() -> AreaInfo {
    FixtureAdapter::new().list_areas().remove(0)
}

/// The `JoinArea` a client that holds the FIXTURE area locally sends.
pub fn join_area() -> ClientMsg {
    let info = fixture_info();
    ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() }
}

pub struct Client {
    pub stream: TcpStream,
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Self {
        let stream = timeout(STEP, TcpStream::connect(addr)).await.expect("connect timed out").expect("connect");
        Self { stream }
    }

    /// Connect from another loopback address (all of 127.0.0.0/8 is loopback on Linux), so that the
    /// server sees a different peer: the per-address limits are per peer.
    pub async fn connect_from(addr: SocketAddr, source: &str) -> Self {
        let socket = TcpSocket::new_v4().expect("socket");
        socket.bind(format!("{source}:0").parse().expect("source address")).expect("bind the source address");
        let stream = timeout(STEP, socket.connect(addr)).await.expect("connect timed out").expect("connect");
        Self { stream }
    }

    /// A client whose kernel receive buffer is tiny, so that it fills up (and the server's writes
    /// to it block) after a few KiB once the client stops reading.
    pub async fn connect_with_tiny_receive_buffer(addr: SocketAddr) -> Self {
        let socket = TcpSocket::new_v4().expect("socket");
        socket.set_recv_buffer_size(4096).expect("SO_RCVBUF");
        let stream = timeout(STEP, socket.connect(addr)).await.expect("connect timed out").expect("connect");
        Self { stream }
    }

    pub async fn send(&mut self, msg: &ClientMsg) {
        write_msg(&mut self.stream, msg).await.expect("send");
    }

    /// Send a hand-written JSON payload inside a valid length-prefixed frame.
    pub async fn send_json(&mut self, json: &str) {
        let mut frame = u32::try_from(json.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        self.stream.write_all(&frame).await.expect("send raw frame");
    }

    pub async fn next(&mut self) -> ServerMsg {
        timeout(STEP, read_msg::<_, ServerMsg>(&mut self.stream))
            .await
            .expect("timed out waiting for a server message")
            .expect("read server message")
    }

    /// Read until `pick` matches, skipping everything else.
    pub async fn until<T>(&mut self, what: &str, mut pick: impl FnMut(&ServerMsg) -> Option<T>) -> T {
        loop {
            let msg = timeout(STEP, read_msg::<_, ServerMsg>(&mut self.stream))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap_or_else(|e| panic!("connection ended while waiting for {what}: {e}"));
            if let Some(found) = pick(&msg) {
                return found;
            }
        }
    }

    /// Everything that arrives within `quiet` of silence (used to prove that nothing is sent).
    pub async fn drain_for(&mut self, quiet: Duration) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(Ok(msg)) = timeout(quiet, read_msg::<_, ServerMsg>(&mut self.stream)).await {
            out.push(msg);
        }
        out
    }

    /// The server must close the connection (anything still queued is skipped). Our own end is closed
    /// too: after a farewell the server keeps reading (and discarding) for a moment, so that its
    /// reset cannot destroy the farewell, and it counts the connection against the address's share
    /// until we hang up. A well-behaved client hangs up as soon as it has seen the end.
    pub async fn expect_closed(&mut self) {
        loop {
            match timeout(STEP, read_msg::<_, ServerMsg>(&mut self.stream)).await.expect("timed out waiting for close") {
                Err(ProtoError::Closed | ProtoError::Io(_)) => break,
                Err(e) => panic!("unexpected error while waiting for close: {e}"),
                Ok(_) => continue,
            }
        }
        let _ = self.stream.shutdown().await;
    }

    /// The next `Reject`: its code and its text (everything before it is skipped).
    pub async fn next_reject(&mut self) -> (RejectCode, String) {
        self.until("Reject", |m| match m {
            ServerMsg::Reject { reason, code } => Some((*code, reason.clone())),
            _ => None,
        })
        .await
    }

    /// Expect a `Reject` whose reason contains `needle`, then the connection closing.
    pub async fn expect_reject(&mut self, needle: &str) {
        let (_code, reason) = self.next_reject().await;
        assert!(reason.contains(needle), "reject reason {reason:?} should mention {needle:?}");
        self.expect_closed().await;
    }

    /// Expect a `Reject` with this CODE (what a client acts on) whose reason contains `needle` (what
    /// the player reads), then the connection closing.
    pub async fn expect_reject_as(&mut self, expected: RejectCode, needle: &str) {
        let (code, reason) = self.next_reject().await;
        assert_eq!(code, expected, "reject code (reason {reason:?})");
        assert!(reason.contains(needle), "reject reason {reason:?} should mention {needle:?}");
        self.expect_closed().await;
    }

    /// Hello that REGISTERS `name` (a new account), with a fixed look.
    pub async fn hello(&mut self, name: &str) {
        self.hello_with(Auth::Register { display_name: name.into() }, [10, 20, 30], Some("test-model".into())).await;
    }

    /// Hello that logs in with a saved token. The look sent with it must be ignored.
    pub async fn hello_token(&mut self, token: &str) {
        self.hello_with(Auth::Token(Secret::new(token)), [99, 98, 97], Some("ignored-model".into())).await;
    }

    pub async fn hello_with(&mut self, auth: Auth, tint: [u8; 3], appearance_ref: Option<String>) {
        self.send(&ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "integration-test".into(),
            auth,
            tint,
            appearance_ref,
        })
        .await;
    }
}

pub struct Joined {
    pub client: Client,
    pub entity: EntityId,
    pub account: AccountId,
    /// The token the registration issued.
    pub token: String,
    pub snapshot: Vec<EntitySnapshot>,
    pub placement: Placement,
}

/// Full happy-path handshake with a fresh registration. The order Welcome, AreaManifest,
/// AreaSnapshot, InstancePlacement is part of the protocol and is asserted strictly.
pub async fn join(addr: SocketAddr, name: &str) -> Joined {
    join_on(Client::connect(addr).await, name).await
}

pub async fn join_on(mut client: Client, name: &str) -> Joined {
    client.hello(name).await;
    let (entity, account, token) = match client.next().await {
        ServerMsg::Welcome { protocol_version, server_build, entity, account, issued_token, .. } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(server_build, env!("CARGO_PKG_VERSION"));
            let token = issued_token.expect("a registration is answered with a token").expose().to_string();
            (entity, account, token)
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    let (snapshot, placement) = finish_join(&mut client).await;
    Joined { client, entity, account, token, snapshot, placement }
}

/// Log in with `token` on a new connection and complete the handshake. Returns what `join_on`
/// does, except that no token is issued (the Welcome must say so).
pub async fn join_with_token(addr: SocketAddr, token: &str) -> Joined {
    let mut client = Client::connect(addr).await;
    client.hello_token(token).await;
    let (entity, account) = match client.next().await {
        ServerMsg::Welcome { entity, account, issued_token, .. } => {
            assert!(issued_token.is_none(), "logging in must never issue a token");
            (entity, account)
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    let (snapshot, placement) = finish_join(&mut client).await;
    Joined { client, entity, account, token: token.to_string(), snapshot, placement }
}

/// The rest of the handshake after `Welcome`: check the manifest, answer `JoinArea`, return the
/// `AreaSnapshot`'s entities and the `InstancePlacement` that follows it directly.
pub async fn finish_join(client: &mut Client) -> (Vec<EntitySnapshot>, Placement) {
    let info = fixture_info();
    match client.next().await {
        ServerMsg::AreaManifest { area, display_name, fingerprint, provenance, fixture } => {
            assert_eq!(area, info.id);
            assert_eq!(fingerprint, info.fingerprint);
            assert!(fixture, "the fixture area must be flagged as fixture");
            assert!(display_name.contains("FIXTURE"), "display name must carry the FIXTURE label: {display_name}");
            assert_eq!(provenance, info.provenance.summary());
            assert_eq!(provenance.game, fmv_core::GameId::Fixture);
        }
        other => panic!("expected AreaManifest, got {other:?}"),
    }
    client.send(&join_area()).await;
    let entities = match client.next().await {
        ServerMsg::AreaSnapshot { area, entities, .. } => {
            assert_eq!(area, info.id);
            entities
        }
        other => panic!("expected AreaSnapshot, got {other:?}"),
    };
    let placement = match client.next().await {
        ServerMsg::InstancePlacement { placement } => placement,
        other => panic!("the placement follows the snapshot directly, got {other:?}"),
    };
    (entities, placement)
}

pub fn update_for(msg: &ServerMsg, id: EntityId) -> Option<EntitySnapshot> {
    match msg {
        ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.id == id).cloned(),
        _ => None,
    }
}

pub fn rejected(msg: &ServerMsg) -> Option<(u32, Pos)> {
    match msg {
        ServerMsg::MoveRejected { seq, corrected } => Some((*seq, *corrected)),
        _ => None,
    }
}

pub fn deadline(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}
