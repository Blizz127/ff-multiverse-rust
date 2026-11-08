//! End-to-end protocol tests: real TCP sockets, raw protocol clients (`fmv_protocol`
//! `read_msg`/`write_msg`), in-process server hosting the FIXTURE area.
//!
//! Replies are always awaited with a loop that skips unrelated messages (joins, updates, pongs)
//! and a timeout, so tests never depend on the exact interleaving of server broadcasts.

use std::net::SocketAddr;
use std::time::Duration;

use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AnimState, EntityId, EntitySnapshot, GameId, Pos};
use fmv_fixture::FixtureAdapter;
use fmv_protocol::{encode, read_msg, write_msg, ClientMsg, ProtoError, ServerMsg, MAX_FRAME, PROTOCOL_VERSION};
use fmv_server::{Server, ServerConfig, ServerError};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Instant};

const STEP: Duration = Duration::from_secs(5);

struct TestServer {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), ServerError>>,
}

impl TestServer {
    async fn start(config: ServerConfig) -> Self {
        let config = ServerConfig { bind: "127.0.0.1:0".parse().unwrap(), ..config };
        let server = Server::bind(config, Box::new(FixtureAdapter::new())).await.expect("bind");
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
        Self { addr, stop, task }
    }

    async fn shutdown(self) {
        let _ = self.stop.send(());
        timeout(STEP * 2, self.task).await.expect("server stops in time").expect("server task did not panic").expect("serve returns Ok");
    }
}

fn fixture_info() -> AreaInfo {
    FixtureAdapter::new().list_areas().remove(0)
}

struct Client {
    stream: TcpStream,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = timeout(STEP, TcpStream::connect(addr)).await.expect("connect timed out").expect("connect");
        Self { stream }
    }

    /// A client whose kernel receive buffer is tiny, so that it fills up (and the server's writes
    /// to it block) after a few KiB once the client stops reading.
    async fn connect_with_tiny_receive_buffer(addr: SocketAddr) -> Self {
        let socket = TcpSocket::new_v4().expect("socket");
        socket.set_recv_buffer_size(4096).expect("SO_RCVBUF");
        let stream = timeout(STEP, socket.connect(addr)).await.expect("connect timed out").expect("connect");
        Self { stream }
    }

    async fn send(&mut self, msg: &ClientMsg) {
        write_msg(&mut self.stream, msg).await.expect("send");
    }

    /// Send a hand-written JSON payload inside a valid length-prefixed frame.
    async fn send_json(&mut self, json: &str) {
        let mut frame = u32::try_from(json.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        self.stream.write_all(&frame).await.expect("send raw frame");
    }

    async fn next(&mut self) -> ServerMsg {
        timeout(STEP, read_msg::<_, ServerMsg>(&mut self.stream))
            .await
            .expect("timed out waiting for a server message")
            .expect("read server message")
    }

    /// Read until `pick` matches, skipping everything else.
    async fn until<T>(&mut self, what: &str, mut pick: impl FnMut(&ServerMsg) -> Option<T>) -> T {
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

    /// The server must close the connection (anything still queued is skipped).
    async fn expect_closed(&mut self) {
        loop {
            match timeout(STEP, read_msg::<_, ServerMsg>(&mut self.stream)).await.expect("timed out waiting for close") {
                Err(ProtoError::Closed | ProtoError::Io(_)) => return,
                Err(e) => panic!("unexpected error while waiting for close: {e}"),
                Ok(_) => continue,
            }
        }
    }

    /// Expect a `Reject` whose reason contains `needle`, then the connection closing.
    async fn expect_reject(&mut self, needle: &str) {
        let reason = self
            .until("Reject", |m| match m {
                ServerMsg::Reject { reason } => Some(reason.clone()),
                _ => None,
            })
            .await;
        assert!(reason.contains(needle), "reject reason {reason:?} should mention {needle:?}");
        self.expect_closed().await;
    }

    async fn hello(&mut self, name: &str) {
        self.send(&ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "integration-test".into(),
            display_name: name.into(),
            tint: [10, 20, 30],
            appearance_ref: Some("test-model".into()),
        })
        .await;
    }
}

struct Joined {
    client: Client,
    entity: EntityId,
    snapshot: Vec<EntitySnapshot>,
}

/// Full happy-path handshake. The order Welcome, AreaManifest, AreaSnapshot is part of the protocol
/// and is asserted strictly.
async fn join(addr: SocketAddr, name: &str) -> Joined {
    join_on(Client::connect(addr).await, name).await
}

async fn join_on(mut client: Client, name: &str) -> Joined {
    client.hello(name).await;
    let entity = match client.next().await {
        ServerMsg::Welcome { protocol_version, server_build, entity, .. } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(server_build, env!("CARGO_PKG_VERSION"));
            entity
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    let snapshot = finish_join(&mut client).await;
    Joined { client, entity, snapshot }
}

/// The rest of the handshake after `Welcome`: check the manifest, answer `JoinArea`, return the
/// `AreaSnapshot`'s entities.
async fn finish_join(client: &mut Client) -> Vec<EntitySnapshot> {
    let info = fixture_info();
    match client.next().await {
        ServerMsg::AreaManifest { area, display_name, fingerprint, provenance, fixture } => {
            assert_eq!(area, info.id);
            assert_eq!(fingerprint, info.fingerprint);
            assert!(fixture, "the fixture area must be flagged as fixture");
            assert!(display_name.contains("FIXTURE"), "display name must carry the FIXTURE label: {display_name}");
            assert_eq!(provenance, info.provenance.summary());
            assert_eq!(provenance.game, GameId::Fixture);
        }
        other => panic!("expected AreaManifest, got {other:?}"),
    }
    client.send(&ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint }).await;
    match client.next().await {
        ServerMsg::AreaSnapshot { area, entities, .. } => {
            assert_eq!(area, info.id);
            entities
        }
        other => panic!("expected AreaSnapshot, got {other:?}"),
    }
}

fn update_for(msg: &ServerMsg, id: EntityId) -> Option<EntitySnapshot> {
    match msg {
        ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.id == id).cloned(),
        _ => None,
    }
}

fn rejected(msg: &ServerMsg) -> Option<(u32, Pos)> {
    match msg {
        ServerMsg::MoveRejected { seq, corrected } => Some((*seq, *corrected)),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_players_move_get_corrected_and_leave() {
    let server = TestServer::start(ServerConfig::default()).await;

    // Both clients complete the handshake and spawn on distinct spawn points.
    let mut a = join(server.addr, "Alice").await;
    assert_eq!(a.snapshot.len(), 1);
    assert_eq!(a.snapshot[0].id, a.entity);
    assert_eq!(a.snapshot[0].pos, Pos::new(-100.0, 0.0, 0.0));
    assert_eq!(a.snapshot[0].avatar.display_name, "Alice");
    assert_eq!(a.snapshot[0].avatar.tint, [10, 20, 30]);
    assert_eq!(a.snapshot[0].avatar.appearance_ref.as_deref(), Some("test-model"));

    let mut b = join(server.addr, "Bob").await;
    assert_ne!(a.entity, b.entity);
    assert_eq!(b.snapshot.len(), 2, "B's snapshot contains A and B");
    assert!(b.snapshot.iter().any(|e| e.id == a.entity));
    assert!(b.snapshot.iter().any(|e| e.id == b.entity && e.pos == Pos::new(100.0, 0.0, 0.0)));
    let a_id = a.entity;
    let joined = a
        .client
        .until("PlayerJoined", |m| match m {
            ServerMsg::PlayerJoined { snapshot } => Some(snapshot.clone()),
            _ => None,
        })
        .await;
    assert_eq!(joined.id, b.entity);
    assert_eq!(joined.avatar.display_name, "Bob");

    // A moves 80 units after 200 ms (budget 600 * 1.5 * 0.2 = 180 units, even more if the machine
    // is slow). z is sent as 7 and snapped back to 0 by the fixture adapter.
    sleep(Duration::from_millis(200)).await;
    a.client.send(&ClientMsg::MoveIntent { seq: 1, pos: Pos::new(-20.0, 0.0, 7.0), facing: 1.25, anim: AnimState::Walk }).await;
    let seen_by_b = b.client.until("EntityUpdate for A", |m| update_for(m, a_id)).await;
    assert_eq!(seen_by_b.pos, Pos::new(-20.0, 0.0, 0.0));
    assert_eq!(seen_by_b.facing, 1.25);
    assert_eq!(seen_by_b.anim, AnimState::Walk);
    assert_eq!(seen_by_b.avatar.display_name, "Alice");
    // The mover receives the same authoritative update.
    let echoed = a.client.until("A's own EntityUpdate", |m| update_for(m, a_id)).await;
    assert_eq!(echoed, seen_by_b);

    // Off-mesh: 350 units is inside the speed budget (450 after >= 0.5 s) but y = 350 is outside the
    // plaza, so this exercises validate_move and not the speed rule.
    sleep(Duration::from_millis(600)).await;
    a.client.send(&ClientMsg::MoveIntent { seq: 2, pos: Pos::new(-20.0, 350.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
    let (seq, corrected) = a.client.until("MoveRejected for off-mesh move", rejected).await;
    assert_eq!((seq, corrected), (2, Pos::new(-20.0, 0.0, 0.0)));

    // Too fast: (440, -10) is on the mesh but 460 units away, beyond the 450-unit budget cap.
    a.client.send(&ClientMsg::MoveIntent { seq: 3, pos: Pos::new(440.0, -10.0, 0.0), facing: 0.0, anim: AnimState::Run }).await;
    let (seq, corrected) = a.client.until("MoveRejected for speeding", rejected).await;
    assert_eq!((seq, corrected), (3, Pos::new(-20.0, 0.0, 0.0)));

    // Non-finite: 1e300 does not fit an f32 and becomes infinity on decode.
    a.client.send_json(r#"{"MoveIntent":{"seq":4,"pos":{"x":1e300,"y":0.0,"z":0.0},"facing":0.0,"anim":"Idle"}}"#).await;
    let (seq, corrected) = a.client.until("MoveRejected for non-finite move", rejected).await;
    assert_eq!((seq, corrected), (4, Pos::new(-20.0, 0.0, 0.0)));

    // Let several ticks pass (50 ms each) so that anything the rejected moves wrongly marked as changed
    // would be flushed on its own, instead of being coalesced into the next valid move's update.
    sleep(Duration::from_millis(300)).await;

    // A valid move afterwards works, and it is the first update B sees for A since the first move:
    // none of the three rejected moves was broadcast.
    a.client.send(&ClientMsg::MoveIntent { seq: 5, pos: Pos::new(30.0, 0.0, 0.0), facing: 2.0, anim: AnimState::Idle }).await;
    let next_for_b = b.client.until("EntityUpdate for A after rejections", |m| update_for(m, a_id)).await;
    assert_eq!(next_for_b.pos, Pos::new(30.0, 0.0, 0.0), "rejected moves must not be broadcast");

    // Intents that reach the server in the same instant (TCP coalescing, a stall on either side) are
    // judged against the travel budget earned over the time that really passed, not against the
    // ~1 unit that "time since the previous intent" would allow. Three intents in ONE write, 370
    // units in total, after a pause that filled the 450-unit budget: none may be refused.
    sleep(Duration::from_millis(600)).await;
    let step = |seq, x, y| ClientMsg::MoveIntent { seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk };
    let mut write = Vec::new();
    for m in [step(6, 165.0, 0.0), step(7, 300.0, 0.0), step(8, 300.0, 100.0)] {
        write.extend_from_slice(&encode(&m).unwrap());
    }
    a.client.stream.write_all(&write).await.unwrap();
    let at = Pos::new(300.0, 100.0, 0.0);
    a.client
        .until("A's update at the end of the bunched moves", |m| {
            if let Some((seq, _)) = rejected(m) {
                panic!("bunched in-budget intent {seq} was refused");
            }
            update_for(m, a_id).filter(|e| e.pos == at)
        })
        .await;
    let seen = b.client.until("B sees the bunched moves", |m| update_for(m, a_id).filter(|e| e.pos == at)).await;
    assert_eq!(seen.pos, at);

    // A move can start and end on walkable ground and still cross a hole: (300, 250) -> (500, -100) is
    // only 403 units, inside the budget, and both ends are on the mesh, but the straight line passes
    // through the off-mesh notch (x > 400, y > 0), for example at (420, 40). It is refused and A stays put.
    sleep(Duration::from_millis(600)).await;
    a.client.send(&step(9, 300.0, 250.0)).await;
    sleep(Duration::from_millis(600)).await;
    a.client.send(&step(10, 500.0, -100.0)).await;
    let (seq, corrected) = a.client.until("MoveRejected for the chord across the hole", rejected).await;
    assert_eq!((seq, corrected), (10, Pos::new(300.0, 250.0, 0.0)), "the hop to (300, 250) was accepted, the one across the notch was not");

    // Ping/Pong carries the server tick.
    a.client.send(&ClientMsg::Ping { nonce: 77 }).await;
    let (nonce, tick) = a
        .client
        .until("Pong", |m| match m {
            ServerMsg::Pong { nonce, tick } => Some((*nonce, *tick)),
            _ => None,
        })
        .await;
    assert_eq!(nonce, 77);
    assert!(tick >= 1, "the tick loop is running");

    // A disconnects: B is told.
    drop(a.client);
    let gone = b
        .client
        .until("PlayerLeft", |m| match m {
            ServerMsg::PlayerLeft { entity } => Some(*entity),
            _ => None,
        })
        .await;
    assert_eq!(gone, a_id);

    // An explicit Leave also ends the session cleanly.
    b.client.send(&ClientMsg::Leave).await;
    b.client.expect_closed().await;

    // A client speaking another protocol version is rejected before anything else happens.
    let mut old = Client::connect(server.addr).await;
    old.send(&ClientMsg::Hello {
        protocol_version: PROTOCOL_VERSION + 1,
        client_build: "from the future".into(),
        display_name: "Eve".into(),
        tint: [0; 3],
        appearance_ref: None,
    })
    .await;
    old.expect_reject("protocol version").await;

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_input_is_rejected_limits_hold_and_the_server_survives() {
    let config = ServerConfig { hello_timeout: Duration::from_millis(300), max_msgs_per_sec: 20, ..ServerConfig::default() };
    let server = TestServer::start(config).await;
    let info = fixture_info();

    // JoinArea with a fingerprint the server does not host.
    let mut c = Client::connect(server.addr).await;
    c.hello("Mallory").await;
    let _ = c.next().await; // Welcome
    let _ = c.next().await; // AreaManifest
    let mut wrong = info.fingerprint;
    wrong.0[0] ^= 0xff;
    c.send(&ClientMsg::JoinArea { area: info.id.clone(), fingerprint: wrong }).await;
    c.expect_reject("fingerprint mismatch").await;

    // JoinArea for another area.
    let mut c = Client::connect(server.addr).await;
    c.hello("Mallory").await;
    let _ = c.next().await;
    let _ = c.next().await;
    let mut other = info.id.clone();
    other.key = "elsewhere".into();
    c.send(&ClientMsg::JoinArea { area: other, fingerprint: info.fingerprint }).await;
    c.expect_reject("area mismatch").await;

    // CannotJoin: the server just closes.
    let mut c = Client::connect(server.addr).await;
    c.hello("Mallory").await;
    let _ = c.next().await;
    let _ = c.next().await;
    c.send(&ClientMsg::CannotJoin { area: info.id.clone(), reason: "I have no owned copy".into() }).await;
    c.expect_closed().await;

    // Empty display name, and a first message that is not Hello.
    let mut c = Client::connect(server.addr).await;
    c.hello(" \t ").await;
    c.expect_reject("display name").await;
    let mut c = Client::connect(server.addr).await;
    c.send(&ClientMsg::Ping { nonce: 1 }).await;
    c.expect_reject("expected Hello").await;

    // Silence: no Hello within the (shortened) hello timeout.
    let mut c = Client::connect(server.addr).await;
    c.expect_reject("timed out waiting for Hello").await;

    // Garbage inside a valid frame, and a frame announcing more than MAX_FRAME bytes. Neither may panic
    // the server; both get a reason.
    let mut c = Client::connect(server.addr).await;
    c.send_json("this is not json").await;
    c.expect_reject("malformed").await;
    let mut c = Client::connect(server.addr).await;
    c.stream.write_all(&(MAX_FRAME + 1).to_le_bytes()).await.unwrap();
    c.expect_reject("frame too large").await;

    // Rate limit: 20 msgs/s sustained. Hello + JoinArea use two tokens, a flood of pings does the rest.
    let mut c = Client::connect(server.addr).await;
    c.hello("Flooder").await;
    let _ = c.next().await;
    let _ = c.next().await;
    c.send(&ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint }).await;
    for nonce in 0..200 {
        // The server may already have hung up on us mid-flood; that is the expected outcome.
        if write_msg(&mut c.stream, &ClientMsg::Ping { nonce }).await.is_err() {
            break;
        }
    }
    c.expect_reject("rate limit").await;

    // After all of that a normal player can still join, and sees only itself (no ghosts).
    let ok = join(server.addr, "Survivor").await;
    assert_eq!(ok.snapshot.len(), 1, "earlier failed clients left no entities behind");

    // Silent joined clients are evicted (the default is 30 s; shortened here). A client that keeps
    // talking (here: pings) is not. The evicted client's slot and avatar are freed.
    let idle = TestServer::start(ServerConfig {
        idle_timeout: Some(Duration::from_secs(1)),
        max_connections: 2,
        ..ServerConfig::default()
    })
    .await;
    let mut quiet = join(idle.addr, "Quiet").await;
    let quiet_entity = quiet.entity;
    let mut chatty = join(idle.addr, "Chatty").await;
    let mut full = Client::connect(idle.addr).await;
    full.expect_reject("server full").await; // both slots are taken
    for nonce in 0..10 {
        sleep(Duration::from_millis(150)).await;
        chatty.client.send(&ClientMsg::Ping { nonce }).await;
    }
    // The last of ten pings over 1.5 s is still answered: the chatty client stayed connected
    // through a 1 s idle timeout (`until` panics with this name if it is not).
    chatty
        .client
        .until("Pong 9 (the chatty client must not be evicted)", |m| match m {
            ServerMsg::Pong { nonce: 9, .. } => Some(()),
            _ => None,
        })
        .await;
    quiet.client.expect_reject("idle timeout").await;
    let mut freed = None;
    for _ in 0..50 {
        let mut again = Client::connect(idle.addr).await;
        again.hello("Replacement").await;
        match again.next().await {
            ServerMsg::Welcome { .. } => {
                freed = Some(again);
                break;
            }
            ServerMsg::Reject { .. } => sleep(Duration::from_millis(100)).await,
            other => panic!("unexpected {other:?}"),
        }
    }
    let mut replacement = freed.expect("the evicted client's connection slot must be freed");
    let entities = finish_join(&mut replacement).await;
    assert!(entities.iter().all(|e| e.id != quiet_entity), "the evicted client's avatar must be gone");
    idle.shutdown().await;

    // Connection cap: a server with room for exactly one connection.
    let small = TestServer::start(ServerConfig { max_connections: 1, ..ServerConfig::default() }).await;
    let mut first = Client::connect(small.addr).await;
    first.hello("First").await;
    assert!(matches!(first.next().await, ServerMsg::Welcome { .. }), "the first connection holds the only slot");
    let mut second = Client::connect(small.addr).await;
    second.expect_reject("server full").await;
    drop(first);
    // The slot frees up shortly after the first client is gone.
    let mut accepted = false;
    for _ in 0..50 {
        let mut again = Client::connect(small.addr).await;
        again.hello("Again").await;
        match again.next().await {
            ServerMsg::Welcome { .. } => {
                accepted = true;
                break;
            }
            ServerMsg::Reject { .. } => sleep(Duration::from_millis(100)).await,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(accepted, "a slot must free up after the first client disconnects");
    small.shutdown().await;
    server.shutdown().await;
}

/// The slow-client guarantee over real sockets: a client that stops reading is dropped (its avatar
/// removed, the others told) and never stalls anybody else. World-level queue logic is unit tested;
/// this covers the writer task, the bounded queue, the kernel buffers and the teardown together.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_is_dropped_without_stalling_the_others() {
    // A tiny outbound queue and a long write timeout, so that the queue filling up (and not the
    // timeout) is what ends the slow client. The rate limit is lifted so that a flood is possible.
    let config = ServerConfig {
        outbound_queue: 8,
        max_msgs_per_sec: 5_000_000,
        write_timeout: Duration::from_secs(60),
        ..ServerConfig::default()
    };
    let server = TestServer::start(config).await;

    // The slow client has a tiny kernel receive buffer and never reads after the handshake.
    let slow = join_on(Client::connect_with_tiny_receive_buffer(server.addr).await, "Slow").await;
    let slow_entity = slow.entity;
    let mut observer = join(server.addr, "Observer").await;

    // It floods Pings, each answered with a Pong it never reads. Once the kernel buffers on both
    // sides are full the server's writer blocks, the 8-message queue overflows and the server
    // must close the connection. Bounded by time, not by message count.
    let flood = tokio::spawn(async move {
        let mut stream = slow.client.stream;
        let burst = encode(&ClientMsg::Ping { nonce: 1 }).unwrap().repeat(1000);
        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            match timeout(Duration::from_secs(10), stream.write_all(&burst)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return true, // the server hung up on us
                Err(_) => return false,    // the server stopped even reading from us
            }
        }
        false
    });

    // Meanwhile the observer keeps pinging. It must always be answered promptly, and must be told
    // when the slow client's avatar is removed.
    let mut worst = Duration::ZERO;
    let mut nonce = 0_u64;
    let mut left = false;
    timeout(Duration::from_secs(60), async {
        while !left {
            nonce += 1;
            let wanted = nonce;
            let sent = Instant::now();
            observer.client.send(&ClientMsg::Ping { nonce }).await;
            observer
                .client
                .until("the observer's Pong while another client floods the server", |m| match m {
                    ServerMsg::PlayerLeft { entity } if *entity == slow_entity => {
                        left = true;
                        None
                    }
                    ServerMsg::Pong { nonce, .. } if *nonce == wanted => Some(()),
                    _ => None,
                })
                .await;
            worst = worst.max(sent.elapsed());
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the slow client was never dropped");
    assert!(worst < Duration::from_secs(4), "a flooding client stalled another player for {worst:?}");
    assert!(flood.await.expect("flood task"), "the server must have closed the slow client's connection");

    // No ghost avatar is left behind, and the server still takes newcomers.
    let late = join(server.addr, "Late").await;
    assert!(late.snapshot.iter().all(|e| e.id != slow_entity), "the dropped client's avatar must be gone");
    assert_eq!(late.snapshot.len(), 2, "the observer and the newcomer");
    server.shutdown().await;
}
