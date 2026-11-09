//! `ClientMsg::Interact` and the links of an area over real TCP sockets and an in-process server hosting the FIXTURE
//! rooms with their interactions (`RoomsAdapter::with_interactions`): the hall has two walk-on links that put a player
//! across it, a ladder that waits for OK and a press region that moves a new account's story. The helpers are in
//! `common`. A client that never sends `Interact` plays as it always did (the first test).
//!
//! The hall's spawns are (-100, 0) and (100, 0) and its speed is 600 units a second, so a hop of up to 400 units after
//! 700 ms is within the budget. The routes below keep clear of everything they do not mean to touch: the walk-on links
//! (x = -150 and x = 130, y from -50 to 50), the story region (x 150..250, y -50..50: walking through it moves a new
//! account's story, which would leave nothing to press) and the battle regions.

mod common;

use std::time::Duration;

use common::*;
use fmv_adapter_api::GameAdapter;
use fmv_core::{AnimState, EntityId, Pos};
use fmv_fixture::rooms::{RoomsAdapter, HALL, LINK_A_TO, LINK_C_TO, STORY_ADVANCED};
use fmv_protocol::{ClientMsg, RejectCode, ServerMsg};
use fmv_server::ServerConfig;
use tokio::time::sleep;

fn rooms_config() -> ServerConfig {
    ServerConfig { area_key: HALL.to_string(), ..ServerConfig::default() }
}

struct Player {
    client: Client,
    entity: EntityId,
    seq: u32,
}

/// Handshake into the hall: Welcome, AreaManifest, JoinArea, the snapshot and the placement after it.
async fn join_hall(addr: std::net::SocketAddr, name: &str) -> Player {
    let mut client = Client::connect(addr).await;
    client.hello(name).await;
    let entity = match client.next().await {
        ServerMsg::Welcome { entity, .. } => entity,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let hall = RoomsAdapter::with_interactions().list_areas().into_iter().find(|i| i.id.key == HALL).expect("the hall");
    match client.next().await {
        ServerMsg::AreaManifest { area, .. } => assert_eq!(area, hall.id),
        other => panic!("expected AreaManifest, got {other:?}"),
    }
    client.send(&ClientMsg::JoinArea { area: hall.id.clone(), fingerprint: hall.fingerprint, provenance: hall.provenance.summary() }).await;
    assert!(matches!(client.next().await, ServerMsg::AreaSnapshot { .. }));
    assert!(matches!(client.next().await, ServerMsg::InstancePlacement { .. }));
    Player { client, entity, seq: 0 }
}

impl Player {
    /// Wait for the speed budget to refill, then send one intent; returns its `seq`.
    async fn walk(&mut self, x: f32, y: f32) -> u32 {
        sleep(Duration::from_millis(700)).await;
        self.seq += 1;
        self.client.send(&ClientMsg::MoveIntent { seq: self.seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
        self.seq
    }

    /// The next `MoveRejected` (seq, corrected).
    async fn correction(&mut self) -> (u32, Pos) {
        self.client.until("MoveRejected", rejected).await
    }

    /// Everything that arrives in the next `quiet` of silence.
    async fn quiet(&mut self) -> Vec<ServerMsg> {
        self.client.drain_for(Duration::from_millis(300)).await
    }
}

fn spot(p: Pos) -> (f32, f32) {
    (p.x, p.y)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_walk_on_link_answers_the_walking_intent_with_the_destination_and_the_fence_holds_a_stale_one() {
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::with_interactions())).await;
    let mut a = join_hall(server.addr, "Alice").await;
    // up to link A (the line x = -150) from either spawn: no key, as every client of the protocol so far plays
    a.walk(-120.0, 0.0).await;
    let seq = a.walk(-140.0, 0.0).await;
    // the answer is the message that already puts a client where the server says it is: the intent's own seq, the
    // destination on the far side of the hall
    let (rejected_seq, corrected) = a.correction().await;
    assert_eq!((rejected_seq, spot(corrected)), (seq, LINK_A_TO));
    // an intent that was already on its way from the old place is answered with the destination too, and moves nothing
    let stale = a.walk(-145.0, 0.0).await;
    let (rejected_seq, corrected) = a.correction().await;
    assert_eq!((rejected_seq, spot(corrected)), (stale, LINK_A_TO));
    // the client's resync after it took the correction agrees with it: no more answers, and the player walks on from
    // the destination (the walk-on link there, B, is disarmed: it is 20 units away, and a player put next to a link
    // has to leave it before it fires)
    a.walk(LINK_A_TO.0, LINK_A_TO.1).await;
    a.walk(LINK_A_TO.0 - 10.0, LINK_A_TO.1).await;
    let seen = a.quiet().await;
    assert!(!seen.iter().any(|m| rejected(m).is_some()), "no more corrections: {seen:?}");
    let last = seen.iter().rev().find_map(|m| update_for(m, a.entity)).expect("the player is still broadcast");
    assert_eq!((last.pos.x, last.pos.y), (LINK_A_TO.0 - 10.0, LINK_A_TO.1), "it walked on from where the link put it");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interact_at_a_ladder_moves_the_player_and_one_in_a_press_region_moves_its_story() {
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::with_interactions())).await;
    let mut a = join_hall(server.addr, "Alice").await;
    // beside the ladder's line (y = 100; it waits for OK, so touching it does nothing)
    a.walk(-50.0, 50.0).await;
    a.walk(0.0, 80.0).await;
    let seq = a.walk(0.0, 95.0).await;
    let seen = a.quiet().await;
    assert!(!seen.iter().any(|m| rejected(m).is_some()), "touching an OK link moves nobody: {seen:?}");
    a.client.send(&ClientMsg::Interact { npc: None }).await;
    let (rejected_seq, corrected) = a.correction().await;
    assert_eq!((rejected_seq, spot(corrected)), (seq, LINK_C_TO), "answered with the newest intent the server has seen");
    // the client takes it
    a.walk(LINK_C_TO.0, LINK_C_TO.1).await;
    // OK where nothing waits for it (and for an NPC that is not there) is not answered
    a.client.send(&ClientMsg::Interact { npc: None }).await;
    a.client.send(&ClientMsg::Interact { npc: Some(9) }).await;
    let seen = a.quiet().await;
    assert!(!seen.iter().any(|m| rejected(m).is_some() || matches!(m, ServerMsg::StoryUpdate { .. })), "{seen:?}");
    // round the story region and link B, into the press region (x 150..250, y 100..160): standing in it moves
    // nothing, OK does
    for (x, y) in [(100.0, -40.0), (100.0, 80.0), (200.0, 130.0)] {
        a.walk(x, y).await;
    }
    let seen = a.quiet().await;
    assert!(!seen.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::MoveRejected { .. })), "standing there is not pressing OK: {seen:?}");
    a.client.send(&ClientMsg::Interact { npc: None }).await;
    let step = a
        .client
        .until("the story step", |m| match m {
            ServerMsg::StoryUpdate { progress, advance: Some(advance) } => Some((progress.clone(), advance.point)),
            _ => None,
        })
        .await;
    assert_eq!(step, (STORY_ADVANCED.to_vec(), 1));
    // and once it is paid, another OK pays nothing
    a.client.send(&ClientMsg::Interact { npc: None }).await;
    let seen = a.quiet().await;
    assert!(!seen.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { .. })), "{seen:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interact_before_joining_the_area_is_refused_like_any_other_in_game_message() {
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::with_interactions())).await;
    let mut c = Client::connect(server.addr).await;
    c.hello(&uniq("Early")).await;
    let _ = c.next().await; // Welcome
    let _ = c.next().await; // AreaManifest
    c.send(&ClientMsg::Interact { npc: None }).await;
    c.expect_reject_as(RejectCode::BadRequest, "unexpected message before JoinArea").await;
    server.shutdown().await;
}
