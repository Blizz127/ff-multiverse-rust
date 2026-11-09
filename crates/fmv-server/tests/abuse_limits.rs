//! The limits that keep one host from holding a server hostage, over real TCP sockets against an
//! in-process server hosting the FIXTURE area: the per-address connection cap, the handshake
//! deadline, the registration rate limits (and what does and does not use them up), the policy for
//! sessions that only ping, and a registration whose client vanished.
//!
//! The server sees the peer address of each socket. Everything here runs on loopback, and all of
//! 127.0.0.0/8 is loopback on Linux, so a test can be "another host" by binding its client socket to
//! 127.0.0.2 (`Client::connect_from`). Helpers are in `common`.

mod common;

use std::io::Write;
use std::time::Duration;

use common::*;
use fmv_core::{AnimState, Pos};
use fmv_protocol::{encode, Auth, ClientMsg, RejectCode, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ServerConfig;
use tokio::time::{sleep, Instant};

fn own_position(joined: &Joined) -> Pos {
    joined.snapshot.iter().find(|e| e.id == joined.entity).expect("the joiner is in its own snapshot").pos
}

/// Register `name` on a fresh connection and read the first answer: `Ok` once the server says
/// `Welcome` (the connection stays open inside the returned client), `Err` with the reject.
async fn try_register(server: &TestServer, name: &str) -> Result<Client, (RejectCode, String)> {
    let mut client = Client::connect(server.addr).await;
    client.hello(name).await;
    match client.next().await {
        ServerMsg::Welcome { .. } => Ok(client),
        ServerMsg::Reject { code, reason } => Err((code, reason)),
        other => panic!("expected Welcome or Reject, got {other:?}"),
    }
}

/// Refused registrations come back quickly when the client does not wait for the server to hang up.
async fn refused(server: &TestServer, name: &str) -> (RejectCode, String) {
    try_register(server, name).await.err().unwrap_or_else(|| panic!("{name:?} must be refused"))
}

// ---------------------------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_address_holds_at_most_its_share_of_the_connections() {
    let server = TestServer::start(ServerConfig {
        max_connections_per_ip: Some(3),
        // The silent sockets below must still be there when the test looks at them.
        hello_timeout: Duration::from_secs(60),
        ..ServerConfig::default()
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(Client::connect(server.addr).await);
    }
    // The fourth from the same address is turned away, with its own code and the number, before it
    // takes a slot of the server...
    let mut fourth = Client::connect(server.addr).await;
    fourth.expect_reject_as(RejectCode::TooManyConnections, "at most 3").await;
    // ...while another address is not affected at all: it registers and plays.
    let other = join_on(Client::connect_from(server.addr, "127.0.0.2").await, &uniq("Other")).await;
    assert_eq!(other.snapshot.len(), 1);
    // A place is free again as soon as one of the three connections ends.
    drop(held.pop());
    let mut accepted = None;
    for _ in 0..50 {
        let mut again = Client::connect(server.addr).await;
        again.hello(&uniq("Back")).await;
        match again.next().await {
            ServerMsg::Welcome { .. } => {
                accepted = Some(again);
                break;
            }
            ServerMsg::Reject { code: RejectCode::TooManyConnections, .. } => sleep(Duration::from_millis(100)).await,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(accepted.is_some(), "the cap must be released when a connection of the address ends");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_sockets_are_cut_after_the_hello_deadline_and_free_their_place() {
    // The default deadline is a few seconds (see the config tests); a short one keeps this test fast.
    let server = TestServer::start(ServerConfig {
        hello_timeout: Duration::from_millis(400),
        max_connections_per_ip: Some(2),
        ..ServerConfig::default()
    })
    .await;
    let started = Instant::now();
    let mut a = Client::connect(server.addr).await;
    let mut b = Client::connect(server.addr).await;
    // Both places of the address are held by sockets that say nothing...
    let mut third = Client::connect(server.addr).await;
    third.expect_reject_as(RejectCode::TooManyConnections, "at most 2").await;
    // ...until the deadline cuts them, with a reason.
    a.expect_reject_as(RejectCode::Timeout, "timed out waiting for Hello").await;
    b.expect_reject_as(RejectCode::Timeout, "timed out waiting for Hello").await;
    assert!(started.elapsed() < Duration::from_secs(4), "the deadline took {:?}", started.elapsed());
    // The places are free: a real client gets in.
    let mut joined = None;
    for _ in 0..50 {
        match try_register(&server, &uniq("Late")).await {
            Ok(client) => {
                joined = Some(client);
                break;
            }
            Err((RejectCode::TooManyConnections, _)) => sleep(Duration::from_millis(100)).await,
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    assert!(joined.is_some());
    server.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Registrations
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_accounts_that_are_created_use_up_the_registration_limit() {
    // Three accounts per minute for the whole server (and no per-address limit in the way).
    let server = TestServer::start(ServerConfig {
        registrations_per_minute: 3,
        registrations_per_ip: None,
        max_connections_per_ip: None,
        ..ServerConfig::default()
    })
    .await;
    // The audited flood: registrations whose name has nothing visible in it. They are refused for
    // their name and use up nothing, however many come (the old server was locked out after 120).
    for _ in 0..40 {
        let (code, _) = refused(&server, "\u{200B}").await;
        assert_eq!(code, RejectCode::InvalidName);
    }
    // Names that are taken use up nothing either.
    let first = try_register(&server, "Holder").await.expect("the first account");
    for _ in 0..20 {
        let (code, _) = refused(&server, "holder").await;
        assert_eq!(code, RejectCode::NameTaken);
    }
    // So two more accounts still fit under the limit of three...
    let second = try_register(&server, &uniq("Second")).await.expect("the second account");
    let third = try_register(&server, &uniq("Third")).await.expect("the third account");
    // ...and the fourth is the one that is refused, as a rate limit, saying so.
    let (code, reason) = refused(&server, &uniq("Fourth")).await;
    assert_eq!(code, RejectCode::RateLimited);
    assert!(reason.contains("too many registrations on this server"), "{reason}");
    drop((first, second, third));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_address_cannot_use_up_the_servers_registrations() {
    let server = TestServer::start(ServerConfig {
        registrations_per_minute: 50,
        registrations_per_ip: Some(2),
        ..ServerConfig::default()
    })
    .await;
    let _a = try_register(&server, &uniq("Flood")).await.expect("first");
    let _b = try_register(&server, &uniq("Flood")).await.expect("second");
    // The third from the same address is refused as ITS limit (not "the server is busy")...
    let (code, reason) = refused(&server, &uniq("Flood")).await;
    assert_eq!(code, RejectCode::RateLimited);
    assert!(reason.contains("from your address"), "{reason}");
    // ...and another address registers as if nothing had happened.
    for i in 0..2 {
        let mut c = Client::connect_from(server.addr, "127.0.0.2").await;
        c.hello(&uniq(&format!("Honest{i}x"))).await;
        assert!(matches!(c.next().await, ServerMsg::Welcome { .. }), "another address is not limited by the first's");
    }
    server.shutdown().await;
}

/// A client that vanishes right after its `Hello`. Every plain socket operation happens while ALL the
/// runtime's workers are stalled, so the server cannot have looked at the connection yet: it first
/// sees the `Hello` with the hang-up already behind it, which is what a crashed or hostile client
/// produces. Such a registration must create no account (the token would never be read, and the
/// name would be gone for good) and must not use a registration place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_hangs_up_right_after_its_hello_does_not_burn_its_name() {
    // Room for exactly two registrations: the ghosts must not use either place.
    let server = TestServer::start(ServerConfig { registrations_per_minute: 2, ..ServerConfig::default() }).await;
    let hello = |name: &str| {
        encode(&ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "ghost".into(),
            auth: Auth::Register { display_name: name.into() },
            character: fmv_protocol::NewCharacter::with_tint([1, 2, 3]),
        })
        .expect("a Hello encodes")
    };

    let hogs = stall_the_runtime(2, Duration::from_millis(500)).await;
    // One ghost resets the connection, the other closes it politely; both after sending the Hello.
    let mut reset = std::net::TcpStream::connect(server.addr).expect("connect");
    reset.write_all(&hello("Ghost")).expect("send the Hello");
    socket2::SockRef::from(&reset).set_linger(Some(Duration::ZERO)).expect("SO_LINGER");
    drop(reset);
    let mut closed = std::net::TcpStream::connect(server.addr).expect("connect");
    closed.write_all(&hello("Ghost2")).expect("send the Hello");
    drop(closed);
    for hog in hogs {
        hog.await.expect("the stall ends");
    }
    sleep(Duration::from_millis(500)).await; // the server has now met both ghosts

    // Neither name is burned, and neither ghost used one of the two places: both register now, and
    // only the third request is over the limit.
    let ghost = try_register(&server, "Ghost").await.unwrap_or_else(|e| panic!("the name Ghost must be free: {e:?}"));
    let ghost2 = try_register(&server, "Ghost2").await.unwrap_or_else(|e| panic!("the name Ghost2 must be free: {e:?}"));
    let (code, _) = refused(&server, &uniq("Third")).await;
    assert_eq!(code, RejectCode::RateLimited, "exactly two accounts were created");
    drop((ghost, ghost2));
    server.shutdown().await;
}

/// The other half of the ghost story: the peer is still there when the connection is checked and gone
/// when the `Welcome` is written. The runtime has ONE blocking thread, which the test keeps busy, so
/// the commit of the registration starts long after the check and the client resets in between. The
/// account is created, its `Welcome` cannot be written, and it is released again (nobody holds its
/// token): the name is free for the next try, instead of lost for good.
#[test]
fn a_registration_whose_welcome_cannot_be_written_is_released() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let server = TestServer::start(ServerConfig::default()).await;
        // The registration's store call waits for the one blocking thread, after its liveness check.
        let busy = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(800)));
        let hello = encode(&ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "ghost".into(),
            auth: Auth::Register { display_name: "Ghost".into() },
            character: fmv_protocol::NewCharacter::with_tint([1, 2, 3]),
        })
        .expect("a Hello encodes");
        let mut ghost = std::net::TcpStream::connect(server.addr).expect("connect");
        ghost.write_all(&hello).expect("send the Hello");
        sleep(Duration::from_millis(250)).await; // the server has read it, found the peer there, queued the commit
        socket2::SockRef::from(&ghost).set_linger(Some(Duration::ZERO)).expect("SO_LINGER");
        drop(ghost); // reset while the commit is still waiting
        busy.await.expect("the blocking thread is free again");

        // The commit runs, the Welcome fails to be written, the account is released: the name is free.
        let mut registered = None;
        for _ in 0..50 {
            match try_register(&server, "Ghost").await {
                Ok(client) => {
                    registered = Some(client);
                    break;
                }
                Err((RejectCode::NameTaken, _)) => sleep(Duration::from_millis(100)).await,
                Err(other) => panic!("unexpected {other:?}"),
            }
        }
        assert!(registered.is_some(), "the name of a registration whose Welcome was never written must be free again");
        server.shutdown().await;
    });
}

// ---------------------------------------------------------------------------------------------
// Sessions that only ping
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_only_pings_is_cut_and_one_that_plays_is_not() {
    let server = TestServer::start(ServerConfig {
        idle_timeout: Some(Duration::from_secs(5)),
        inactivity_timeout: Some(Duration::from_millis(1500)),
        ..ServerConfig::default()
    })
    .await;
    let mut squatter = join(server.addr, &uniq("Squat")).await;
    let mut player = join(server.addr, &uniq("Play")).await;
    let spot = own_position(&player);
    let started = Instant::now();

    // Both talk every 200 ms for three seconds, twice the inactivity limit: one only pings, the other
    // also sends game input (here: turning on the spot, which is free).
    let mut cut = None;
    for n in 0..15u64 {
        sleep(Duration::from_millis(200)).await;
        squatter.client.send(&ClientMsg::Ping { nonce: n }).await;
        player.client.send(&ClientMsg::Ping { nonce: n }).await;
        if n % 2 == 0 {
            let facing = (n % 6) as f32 * 0.5;
            player.client.send(&ClientMsg::MoveIntent { seq: n as u32, pos: spot, facing, anim: AnimState::Idle }).await;
        }
        // Read what the squatter has been told so far without waiting for more.
        let heard = squatter.client.drain_for(Duration::from_millis(5)).await;
        if let Some((code, reason)) = heard.into_iter().find_map(|m| match m {
            ServerMsg::Reject { code, reason } => Some((code, reason)),
            _ => None,
        }) {
            cut = Some((code, reason, started.elapsed()));
            break;
        }
    }
    let (code, reason, after) = cut.expect("a session that only pings must be cut within the inactivity window");
    assert_eq!(code, RejectCode::IdleTimeout);
    assert!(reason.contains("inactivity") && reason.contains("pings do not count"), "{reason}");
    // Not at once (the limit is 1.5 s from the last game input, which was the join), and not never
    // (generous upper bound: a loaded machine stretches the 200 ms steps).
    assert!(after >= Duration::from_millis(800) && after < Duration::from_secs(8), "cut after {after:?}");
    squatter.client.expect_closed().await;

    // The player keeps playing for two more seconds, past twice the limit in all, and is still there:
    // game input is what keeps a session, whatever else it does.
    for n in 100..110u64 {
        sleep(Duration::from_millis(200)).await;
        player.client.send(&ClientMsg::Ping { nonce: n }).await;
        if n % 2 == 0 {
            let facing = (n % 6) as f32 * 0.5;
            player.client.send(&ClientMsg::MoveIntent { seq: n as u32, pos: spot, facing, anim: AnimState::Idle }).await;
        }
    }
    player.client.send(&ClientMsg::Ping { nonce: 4242 }).await;
    player
        .client
        .until("Pong 4242 (a session with game input must not be cut)", |m| match m {
            ServerMsg::Pong { nonce: 4242, .. } => Some(()),
            _ => None,
        })
        .await;
    // And the squatter's place and avatar are gone.
    let late = join(server.addr, &uniq("Late")).await;
    assert!(late.snapshot.iter().all(|e| e.id != squatter.entity), "the cut session's avatar must be gone");
    server.shutdown().await;
}

// ---------------------------------------------------------------------------------------------

/// Block `workers` worker threads of the runtime for `how_long` and return once they are all
/// blocked. While they are, the server's tasks cannot run: what the test does meanwhile with plain
/// blocking sockets is seen by the server only afterwards.
async fn stall_the_runtime(workers: usize, how_long: Duration) -> Vec<tokio::task::JoinHandle<()>> {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let hogs: Vec<_> = (0..workers)
        .map(|_| {
            let started = started_tx.clone();
            tokio::spawn(async move {
                let _ = started.send(());
                std::thread::sleep(how_long);
            })
        })
        .collect();
    for _ in 0..workers {
        started_rx.recv_timeout(Duration::from_secs(10)).expect("a worker thread took the stalling task");
    }
    hogs
}
