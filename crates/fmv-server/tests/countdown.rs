//! End-to-end countdown: real TCP sockets, raw protocol clients, an in-process server over the FIXTURE rooms with their
//! countdown story (`RoomsAdapter::with_a_countdown_of`: the hall's story region starts a countdown with its step, the
//! hall's clear region moves the story on past it), on a real state directory, running the server's OWN tick loop.
//!
//! What it pins, through the whole stack (connection, world, tick loop, saver, store, login):
//! * the step that starts a countdown is told to the player (`ServerMsg::Countdown { Running }`, the whole length), and
//!   to the members of its party;
//! * a countdown that runs out on the server's clock is told (`Over { TimeUp }`) and **does nothing else**: nobody is
//!   moved, the story the step saved is exactly what the store holds (the server restarts on the same directory and the
//!   account logs in at that story, with no countdown: a countdown is not stored);
//! * the story moving past a countdown ends it for the whole party (`Over { Cleared }`), and it never says it ran out.

mod common;

use std::time::Duration;

use common::*;
use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AccountId, AnimState, EntityId, Pos};
use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, STORY_CLEARED};
use fmv_protocol::{ClientMsg, CountdownEnd, CountdownState, PartyEvent, ServerMsg};
use fmv_server::ServerConfig;
use tokio::time::{sleep, Instant};

fn info_of(adapter: &RoomsAdapter, key: &str) -> AreaInfo {
    adapter.list_areas().into_iter().find(|i| i.id.key == key).expect("a listed area")
}

struct Player {
    client: Client,
    entity: EntityId,
    account: AccountId,
    token: String,
}

/// Handshake into the hall (`auth`: a registration, `Err(name)`, or a saved token, `Ok(token)`).
async fn enter_hall(addr: std::net::SocketAddr, auth: Result<&str, &str>, adapter: &RoomsAdapter) -> Player {
    let mut client = Client::connect(addr).await;
    match auth {
        Ok(token) => client.hello_token(token).await,
        Err(name) => client.hello(name).await,
    }
    let (entity, account, issued) = match client.next().await {
        ServerMsg::Welcome { entity, account, issued_token, .. } => (entity, account, issued_token),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let token = match (auth, issued) {
        (Ok(token), None) => token.to_string(),
        (Err(_), Some(token)) => token.expose().to_string(),
        other => panic!("a registration is answered with a token and a login with none: {other:?}"),
    };
    let hall = info_of(adapter, HALL);
    assert!(matches!(client.next().await, ServerMsg::AreaManifest { .. }));
    client.send(&ClientMsg::JoinArea { area: hall.id.clone(), fingerprint: hall.fingerprint, provenance: hall.provenance.summary() }).await;
    assert!(matches!(client.next().await, ServerMsg::AreaSnapshot { .. }));
    assert!(matches!(client.next().await, ServerMsg::InstancePlacement { .. }));
    Player { client, entity, account, token }
}

/// Walk to `(x, y)`: wait for the speed budget to refill, then send one intent.
async fn walk(player: &mut Player, seq: u32, x: f32, y: f32) {
    sleep(Duration::from_millis(700)).await;
    player.client.send(&ClientMsg::MoveIntent { seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
}

/// The next countdown message (everything else is skipped).
async fn next_countdown(client: &mut Client, what: &str) -> CountdownState {
    client
        .until(what, |m| match m {
            ServerMsg::Countdown { state } => Some(*state),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_countdown_that_runs_out_on_the_servers_clock_is_told_and_changes_nothing_that_was_saved() {
    let adapter = RoomsAdapter::with_a_countdown_of(2);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let config = || ServerConfig { area_key: HALL.to_string(), state_dir: Some(state.clone()), ..ServerConfig::default() };

    // ---- run 1: A steps into the story region and is told a countdown of two seconds ----
    let server = TestServer::start_over(config(), Box::new(RoomsAdapter::with_a_countdown_of(2))).await;
    let mut a = enter_hall(server.addr, Err("CountA"), &adapter).await;
    walk(&mut a, 1, 100.0, 0.0).await;
    walk(&mut a, 2, 200.0, 0.0).await;
    assert_eq!(next_countdown(&mut a.client, "the countdown's start").await, CountdownState::Running { remaining_ms: 2000, total_ms: 2000 });
    let started = Instant::now();
    // it runs out on the server's own clock: told once, about two seconds later
    let end = next_countdown(&mut a.client, "the end of the countdown").await;
    assert_eq!(end, CountdownState::Over { reason: CountdownEnd::TimeUp });
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1500) && waited < Duration::from_secs(4), "ran out after {waited:?}");
    // and nothing else happens: nobody is moved, nothing more is said, the player can still walk and be answered
    walk(&mut a, 3, 210.0, 10.0).await;
    let after = a.client.drain_for(Duration::from_millis(500)).await;
    assert!(
        after.iter().all(|m| !matches!(m, ServerMsg::Countdown { .. } | ServerMsg::AreaSnapshot { .. } | ServerMsg::StoryUpdate { .. } | ServerMsg::Reject { .. })),
        "{after:?}"
    );
    assert!(after.iter().any(|m| update_for(m, a.entity).is_some()), "A is still in the world and its walk was applied: {after:?}");

    // ---- the store holds the story the step saved, untouched by the clock ----
    server.shutdown().await;
    let record = state.join("progression").join(a.account.0.to_string()).join("Fixture.json");
    let text: String = std::fs::read_to_string(&record).unwrap_or_else(|e| panic!("A's story was saved at {record:?}: {e}")).split_whitespace().collect();
    assert!(text.contains("\"blob\":\"01\""), "the step's story is what is stored, whatever the clock did: {text}");

    // ---- run 2: the server restarts, A logs in at that story and has no countdown (a countdown is not stored) ----
    let server = TestServer::start_over(config(), Box::new(RoomsAdapter::with_a_countdown_of(2))).await;
    let mut a = enter_hall(server.addr, Ok(&a.token), &adapter).await;
    let told = a
        .client
        .until("A's StoryUpdate", |m| match m {
            ServerMsg::StoryUpdate { progress, advance } => Some((progress.clone(), advance.clone())),
            _ => None,
        })
        .await;
    assert_eq!(told, (STORY_ADVANCED.to_vec(), None));
    let quiet = a.client.drain_for(Duration::from_millis(2500)).await;
    assert!(quiet.iter().all(|m| !matches!(m, ServerMsg::Countdown { .. })), "no countdown after a restart: {quiet:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_party_shares_its_leaders_countdown_and_the_story_moving_past_it_ends_it_for_everyone() {
    let adapter = RoomsAdapter::with_a_countdown();
    let server = TestServer::start_over(ServerConfig { area_key: HALL.to_string(), ..ServerConfig::default() }, Box::new(RoomsAdapter::with_a_countdown())).await;
    let mut a = enter_hall(server.addr, Err("PartyA"), &adapter).await;
    let mut b = enter_hall(server.addr, Err("PartyB"), &adapter).await;
    // A invites B, B accepts
    a.client.send(&ClientMsg::PartyInvite { target: b.entity }).await;
    let invite = a
        .client
        .until("A's InviteSent", |m| match m {
            ServerMsg::PartyEvent { event: PartyEvent::InviteSent { invite, .. }, .. } => Some(*invite),
            _ => None,
        })
        .await;
    b.client.send(&ClientMsg::PartyAccept { invite }).await;
    b.client.until("the party formed", |m| matches!(m, ServerMsg::PartyEvent { event: PartyEvent::PartyFormed { .. }, .. }).then_some(())).await;

    // A steps into the story region: both are told the whole countdown
    walk(&mut a, 1, 100.0, 0.0).await;
    walk(&mut a, 2, 200.0, 0.0).await;
    let whole = CountdownState::Running { remaining_ms: 90_000, total_ms: 90_000 };
    assert_eq!(next_countdown(&mut a.client, "A's countdown").await, whole);
    assert_eq!(next_countdown(&mut b.client, "B's countdown").await, whole, "a member follows its leader's story");

    // A walks to the clear region: its story moves past what the countdown was for, and both are told it is over
    walk(&mut a, 3, 0.0, 100.0).await;
    walk(&mut a, 4, -250.0, 150.0).await;
    let cleared = CountdownState::Over { reason: CountdownEnd::Cleared };
    let mut step_told = false;
    let end = a
        .client
        .until("A's end", |m| match m {
            ServerMsg::StoryUpdate { progress, .. } if progress == STORY_CLEARED => {
                step_told = true;
                None
            }
            ServerMsg::Countdown { state } => Some(*state),
            _ => None,
        })
        .await;
    assert_eq!(end, cleared);
    assert!(step_told, "A was told its story moved on, before the countdown was ended");
    assert_eq!(next_countdown(&mut b.client, "B's end").await, cleared);
    // it never says it ran out
    let quiet = b.client.drain_for(Duration::from_millis(500)).await;
    assert!(quiet.iter().all(|m| !matches!(m, ServerMsg::Countdown { .. })), "{quiet:?}");
    let _ = (&a.token, &b.token, a.account, b.account);
    server.shutdown().await;
}
