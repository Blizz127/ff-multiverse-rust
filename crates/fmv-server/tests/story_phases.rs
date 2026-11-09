//! End-to-end story phasing: real TCP sockets, raw protocol clients, an in-process server over the
//! FIXTURE rooms with their synthetic story (`RoomsAdapter::with_story`: the hall has a story region,
//! both rooms are in the empty phase for a new account and in another one after the region was
//! entered, and the yard's guard is in that second phase's cast only), on a real state directory.
//!
//! What it pins, through the whole stack (connection, world, saver, store, login):
//! * a new account is told nothing about any story and sits in the empty phase, as always;
//! * walking into the region advances the story, which the player is told (`StoryUpdate`), and moves it
//!   to another instance of the SAME area (a fresh snapshot of an instance without the other player,
//!   a placement with the new phase) while the other player hears it leave;
//! * the step is saved (the server restarts on the same directory), is a plain v7-shaped
//!   `Progression` record, and both players log in again in the phases they left in, apart, the one that
//!   advanced being told where its story stands.

mod common;

use std::time::Duration;

use common::*;
use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AccountId, AnimState, EntityId, EntitySnapshot, PhaseKey, Pos};
use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, STORY_ADVANCED_PHASE};
use fmv_protocol::{ClientMsg, Placement, ServerMsg};
use fmv_server::ServerConfig;
use tokio::time::sleep;

fn info_of(adapter: &RoomsAdapter, key: &str) -> AreaInfo {
    adapter.list_areas().into_iter().find(|i| i.id.key == key).expect("a listed area")
}

struct Player {
    client: Client,
    entity: EntityId,
    account: AccountId,
    token: String,
    snapshot: Vec<EntitySnapshot>,
    placement: Placement,
}

/// Handshake into the hall: `Welcome`, `AreaManifest`, `JoinArea`, the snapshot and the placement that
/// follows it directly. `auth` is a registration (`Err(name)`) or a saved token (`Ok(token)`).
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
    match client.next().await {
        ServerMsg::AreaManifest { area, fingerprint, .. } => assert_eq!((area, fingerprint), (hall.id.clone(), hall.fingerprint)),
        other => panic!("expected AreaManifest, got {other:?}"),
    }
    client.send(&ClientMsg::JoinArea { area: hall.id.clone(), fingerprint: hall.fingerprint, provenance: hall.provenance.summary() }).await;
    let snapshot = match client.next().await {
        ServerMsg::AreaSnapshot { area, entities, .. } => {
            assert_eq!(area.key, HALL);
            entities
        }
        other => panic!("expected the AreaSnapshot, got {other:?}"),
    };
    let placement = match client.next().await {
        ServerMsg::InstancePlacement { placement } => placement,
        other => panic!("the placement follows the snapshot directly, got {other:?}"),
    };
    Player { client, entity, account, token, snapshot, placement }
}

/// Walk to `(x, y)`: wait for the speed budget to refill, then send one intent.
async fn walk(player: &mut Player, seq: u32, x: f32, y: f32) {
    sleep(Duration::from_millis(700)).await;
    player.client.send(&ClientMsg::MoveIntent { seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
}

fn ids(entities: &[EntitySnapshot]) -> Vec<EntityId> {
    entities.iter().map(|e| e.id).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_story_step_parts_two_players_is_saved_and_both_log_in_again_in_the_phases_they_left_in() {
    let adapter = RoomsAdapter::with_story();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("server-state");
    let config = || ServerConfig { area_key: HALL.to_string(), state_dir: Some(state.clone()), ..ServerConfig::default() };

    // ---- run 1: two new accounts, one advances ----
    let server = TestServer::start_over(config(), Box::new(RoomsAdapter::with_story())).await;
    let mut a = enter_hall(server.addr, Err("StoryA"), &adapter).await;
    let mut b = enter_hall(server.addr, Err("StoryB"), &adapter).await;
    let (a_id, b_id, a_account, b_account) = (a.entity, b.entity, a.account, b.account);
    for p in [&a, &b] {
        assert_eq!(p.placement.key.phase, PhaseKey::default(), "a new account is in the empty phase");
        assert!(p.placement.story_advance_allowed && p.placement.party.is_none());
    }
    assert_eq!(ids(&b.snapshot), [a_id, b_id], "B joined A's instance");
    a.client.until("PlayerJoined(B)", |m| matches!(m, ServerMsg::PlayerJoined { snapshot } if snapshot.id == b_id).then_some(())).await;

    // A walks into the story region (x 150..250, y -50..50), well inside the hall.
    walk(&mut a, 1, 100.0, 0.0).await;
    walk(&mut a, 2, 200.0, 0.0).await;
    // A is told its story moved...
    let (progress, advance) = a
        .client
        .until("StoryUpdate", |m| match m {
            ServerMsg::StoryUpdate { progress, advance } => Some((progress.clone(), advance.clone())),
            _ => None,
        })
        .await;
    assert_eq!(progress, STORY_ADVANCED);
    let advance = advance.expect("this update follows a step");
    assert_eq!((advance.point, advance.summary.as_str()), (0, "moment=0->1"));
    // ...and is moved to another instance of the hall: a snapshot of an instance without B, then the new phase.
    let entities = a
        .client
        .until("the new snapshot", |m| match m {
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == HALL => Some(entities.clone()),
            _ => None,
        })
        .await;
    assert_eq!(ids(&entities), [a_id], "A is alone in its new instance");
    match a.client.next().await {
        ServerMsg::InstancePlacement { placement } => assert_eq!(placement.key.phase, PhaseKey(STORY_ADVANCED_PHASE.to_vec())),
        other => panic!("the placement follows the snapshot directly, got {other:?}"),
    }
    // B, left behind, hears A leave, and from then on nothing of A's walking reaches it.
    b.client.until("PlayerLeft(A)", |m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == a_id).then_some(())).await;
    walk(&mut a, 3, 210.0, 10.0).await;
    a.client.until("A's own update", |m| update_for(m, a_id)).await;
    let quiet = b.client.drain_for(Duration::from_millis(400)).await;
    assert!(quiet.iter().all(|m| update_for(m, a_id).is_none()), "B sees nothing of A: {quiet:?}");

    // ---- the store holds exactly one plain v7-shaped story record: A's ----
    server.shutdown().await;
    let record = |account: AccountId| state.join("progression").join(account.0.to_string()).join("Fixture.json");
    // (compared as text without white space, so that the file's indentation does not matter)
    let text: String = std::fs::read_to_string(record(a_account)).expect("A's story was saved").split_whitespace().collect();
    assert_eq!(text.matches("\":").count(), 5, "five keys, the shape it always had: {text}");
    for expected in [format!("\"account_id\":{}", a_account.0), "\"blob\":\"01\"".to_string(), "\"game\":\"Fixture\"".to_string(), "\"schema_version\":1".to_string()] {
        assert!(text.contains(&expected), "{expected} is in {text}");
    }
    assert!(text.contains("\"updated_at\":"), "{text}");
    assert!(!record(b_account).exists(), "B never advanced: no record is written for it");

    // ---- run 2: the server restarts on the same directory, both log in again with their tokens ----
    let server = TestServer::start_over(config(), Box::new(RoomsAdapter::with_story())).await;
    let mut a = enter_hall(server.addr, Ok(&a.token), &adapter).await;
    let mut b = enter_hall(server.addr, Ok(&b.token), &adapter).await;
    assert_eq!((a.account, b.account), (a_account, b_account));
    assert_eq!(a.placement.key.phase, PhaseKey(STORY_ADVANCED_PHASE.to_vec()), "A kept its phase");
    assert_eq!(b.placement.key.phase, PhaseKey::default(), "B kept its phase");
    assert_eq!(ids(&a.snapshot), [a.entity], "they are apart although both are in the hall");
    assert_eq!(ids(&b.snapshot), [b.entity]);
    // A is told where its story stands (it is not a new account's); B is told nothing.
    let told = a
        .client
        .until("A's StoryUpdate", |m| match m {
            ServerMsg::StoryUpdate { progress, advance } => Some((progress.clone(), advance.clone())),
            _ => None,
        })
        .await;
    assert_eq!(told, (STORY_ADVANCED.to_vec(), None));
    let b_quiet = b.client.drain_for(Duration::from_millis(500)).await;
    assert!(
        b_quiet.iter().all(|m| !matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::PlayerJoined { .. })),
        "B hears nothing of a story or of A: {b_quiet:?}"
    );
    server.shutdown().await;
}
