//! End-to-end area travel: real TCP sockets, raw protocol clients, an in-process server hosting the
//! FIXTURE rooms (`fmv_fixture::rooms`: `hall` and `yard`, joined by an exit line each way, an NPC
//! in the yard, a yard that is twice as fast as the hall). The helpers are in `common`.
//!
//! The clients here are honest ones: they answer an `AreaChange` from a copy of the adapter they
//! hold locally, exactly as `fmv-client` does from its own game files.

mod common;

use std::time::Duration;

use common::*;
use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AnimState, EntityId, EntitySnapshot, Fingerprint, Pos};
use fmv_fixture::rooms::{RoomsAdapter, EXIT_X, HALL, HALL_ARRIVAL, YARD, YARD_ARRIVAL, YARD_NPC_POS};
use fmv_protocol::{ClientMsg, NpcSnapshot, ServerMsg};
use fmv_server::ServerConfig;
use tokio::time::sleep;

fn rooms_config() -> ServerConfig {
    ServerConfig { area_key: HALL.to_string(), ..ServerConfig::default() }
}

fn info_of(adapter: &RoomsAdapter, key: &str) -> AreaInfo {
    adapter.list_areas().into_iter().find(|i| i.id.key == key).expect("a listed area")
}

fn join_message(info: &AreaInfo) -> ClientMsg {
    ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() }
}

struct Player {
    client: Client,
    entity: EntityId,
}

/// Handshake into the hall of `adapter`'s rooms: Welcome, AreaManifest, JoinArea, AreaSnapshot and
/// the InstancePlacement directly after it.
async fn join_hall(addr: std::net::SocketAddr, name: &str, adapter: &RoomsAdapter) -> Player {
    let mut client = Client::connect(addr).await;
    client.hello(name).await;
    let entity = match client.next().await {
        ServerMsg::Welcome { entity, .. } => entity,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let hall = info_of(adapter, HALL);
    match client.next().await {
        ServerMsg::AreaManifest { area, fingerprint, .. } => assert_eq!((area, fingerprint), (hall.id.clone(), hall.fingerprint)),
        other => panic!("expected AreaManifest, got {other:?}"),
    }
    client.send(&join_message(&hall)).await;
    assert_snapshot_then_placement(&mut client, HALL).await;
    Player { client, entity }
}

/// The next two messages are an `AreaSnapshot` of `key` and, directly after, an
/// `InstancePlacement` for that area. Returns the snapshot's entities and NPCs.
async fn assert_snapshot_then_placement(client: &mut Client, key: &str) -> (Vec<EntitySnapshot>, Vec<NpcSnapshot>) {
    let (entities, npcs) = match client.next().await {
        ServerMsg::AreaSnapshot { area, entities, npcs, .. } => {
            assert_eq!(area.key, key);
            (entities, npcs)
        }
        other => panic!("expected the AreaSnapshot of {key}, got {other:?}"),
    };
    match client.next().await {
        ServerMsg::InstancePlacement { placement } => assert_eq!(placement.key.area.key, key, "placement is per area"),
        other => panic!("the placement follows the snapshot directly, got {other:?}"),
    }
    (entities, npcs)
}

/// What a client holding `adapter` answers to this `AreaChange`: `JoinArea`, from its own copy.
fn honest_answer(adapter: &RoomsAdapter, offer: &ServerMsg) -> ClientMsg {
    let ServerMsg::AreaChange { area, fingerprint, .. } = offer else { panic!("not an AreaChange: {offer:?}") };
    let local = info_of(adapter, &area.key);
    assert_eq!(*fingerprint, local.fingerprint, "the server offers the area both sides hold");
    join_message(&local)
}

async fn area_change(client: &mut Client) -> ServerMsg {
    client.until("AreaChange", |m| matches!(m, ServerMsg::AreaChange { .. }).then(|| m.clone())).await
}

/// Walk `player` to `(x, y)`: wait for the speed budget to refill, then send one intent.
async fn walk(player: &mut Player, seq: u32, x: f32, y: f32) {
    sleep(Duration::from_millis(700)).await;
    player.client.send(&ClientMsg::MoveIntent { seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
}

/// Walk `player` (from either spawn point) across the hall's exit line and wait for its `AreaChange`.
async fn cross_the_hall_exit(player: &mut Player, seq: u32) -> ServerMsg {
    walk(player, seq, EXIT_X - 50.0, 0.0).await;
    walk(player, seq + 1, EXIT_X + 50.0, 0.0).await;
    area_change(&mut player.client).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn walking_through_the_exit_takes_a_player_to_the_next_area_and_traffic_stays_in_its_area() {
    let adapter = RoomsAdapter::new();
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, "Alice", &adapter).await;
    let mut b = join_hall(server.addr, "Bob", &adapter).await;
    let (a_id, b_id) = (a.entity, b.entity);
    a.client.until("PlayerJoined(B)", |m| matches!(m, ServerMsg::PlayerJoined { snapshot } if snapshot.id == b_id).then_some(())).await;

    // A walks across the hall's exit line (x = 300). B, left behind, is told A is gone.
    walk(&mut a, 1, EXIT_X - 50.0, 0.0).await;
    a.client.until("A's own update", |m| update_for(m, a_id)).await;
    walk(&mut a, 2, EXIT_X + 50.0, 0.0).await;
    b.client.until("PlayerLeft(A)", |m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == a_id).then_some(())).await;

    // A is offered the yard: where it will stand, already settled on the yard's floor.
    let offer = area_change(&mut a.client).await;
    match &offer {
        ServerMsg::AreaChange { area, display_name, fixture, arrival, .. } => {
            assert_eq!(area.key, YARD);
            assert!(*fixture && display_name.contains("FIXTURE"), "{display_name}");
            assert_eq!((arrival.x, arrival.y), YARD_ARRIVAL);
        }
        other => panic!("{other:?}"),
    }
    // Until it answers, its moves are ignored (no MoveRejected, nothing broadcast).
    a.client.send(&ClientMsg::MoveIntent { seq: 3, pos: Pos::new(EXIT_X + 60.0, 0.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
    let quiet = a.client.drain_for(Duration::from_millis(300)).await;
    assert!(!quiet.iter().any(|m| rejected(m).is_some() || update_for(m, a_id).is_some()), "{quiet:?}");

    // It holds the yard: JoinArea, then the yard's snapshot (itself and the guard) and placement.
    a.client.send(&honest_answer(&adapter, &offer)).await;
    let (entities, npcs) = assert_snapshot_then_placement(&mut a.client, YARD).await;
    assert_eq!(entities.len(), 1, "only A is in the yard");
    assert_eq!((entities[0].id, entities[0].pos.x, entities[0].pos.y), (a_id, YARD_ARRIVAL.0, YARD_ARRIVAL.1));
    assert_eq!(npcs.len(), 1);
    assert_eq!((npcs[0].name.as_str(), npcs[0].model.as_str(), npcs[0].idle_animation.as_deref()), ("guard", "guardm", Some("idle")));
    assert_eq!((npcs[0].pos.x, npcs[0].pos.y), YARD_NPC_POS);

    // A walks in the yard; B (in the hall) hears nothing of it.
    walk(&mut a, 4, YARD_ARRIVAL.0 + 80.0, 40.0).await;
    let moved = a.client.until("A's update in the yard", |m| update_for(m, a_id)).await;
    assert_eq!((moved.pos.x, moved.pos.y), (YARD_ARRIVAL.0 + 80.0, 40.0));
    let b_heard = b.client.drain_for(Duration::from_millis(300)).await;
    assert!(!b_heard.iter().any(|m| update_for(m, a_id).is_some()), "B hears nothing of the yard: {b_heard:?}");

    // B follows: A (already there) is told, and B's snapshot has both of them.
    let offer = cross_the_hall_exit(&mut b, 1).await;
    b.client.send(&honest_answer(&adapter, &offer)).await;
    let (entities, npcs) = assert_snapshot_then_placement(&mut b.client, YARD).await;
    let mut ids: Vec<EntityId> = entities.iter().map(|e| e.id).collect();
    ids.sort();
    assert_eq!(ids, [a_id, b_id], "B sees A in the yard");
    assert_eq!(npcs.len(), 1);
    let joined = a.client.until("PlayerJoined(B) in the yard", |m| match m {
        ServerMsg::PlayerJoined { snapshot } if snapshot.id == b_id => Some(snapshot.clone()),
        _ => None,
    })
    .await;
    assert_eq!((joined.pos.x, joined.pos.y), YARD_ARRIVAL);

    // Both are in the yard now: A's moves reach B.
    walk(&mut a, 5, YARD_ARRIVAL.0 + 120.0, 80.0).await;
    let seen = b.client.until("B sees A move in the yard", |m| update_for(m, a_id).filter(|e| e.pos.y == 80.0)).await;
    assert_eq!(seen.pos.x, YARD_ARRIVAL.0 + 120.0);

    // And the way back works: A walks across the yard's own line (x = -300) into the hall.
    walk(&mut a, 6, -250.0, 0.0).await;
    a.client.until("A's update", |m| update_for(m, a_id).filter(|e| e.pos.x == -250.0)).await;
    walk(&mut a, 7, -350.0, 0.0).await;
    b.client.until("PlayerLeft(A) in the yard", |m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == a_id).then_some(())).await;
    let offer = area_change(&mut a.client).await;
    match &offer {
        ServerMsg::AreaChange { area, arrival, .. } => {
            assert_eq!(area.key, HALL);
            assert_eq!((arrival.x, arrival.y), HALL_ARRIVAL);
        }
        other => panic!("{other:?}"),
    }
    a.client.send(&honest_answer(&adapter, &offer)).await;
    let (entities, npcs) = assert_snapshot_then_placement(&mut a.client, HALL).await;
    assert_eq!(entities.iter().map(|e| e.id).collect::<Vec<_>>(), [a_id], "B stayed in the yard");
    assert!(npcs.is_empty(), "the hall has no NPCs");

    // A disconnecting in the hall is no news in the yard.
    drop(a.client);
    let b_heard = b.client.drain_for(Duration::from_millis(300)).await;
    assert!(!b_heard.iter().any(|m| matches!(m, ServerMsg::PlayerLeft { .. })), "{b_heard:?}");
    b.client.send(&ClientMsg::Leave).await;
    b.client.expect_closed().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_without_the_next_area_is_sent_back_and_one_that_lies_is_thrown_out() {
    let adapter = RoomsAdapter::new();
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, "Alice", &adapter).await;
    let a_id = a.entity;

    let offer = cross_the_hall_exit(&mut a, 1).await;
    let ServerMsg::AreaChange { area, .. } = &offer else { panic!() };
    // "I do not have it": the server sends A back where it stood before the crossing move.
    a.client.send(&ClientMsg::CannotJoin { area: area.clone(), reason: "no such field here".into() }).await;
    let back = area_change(&mut a.client).await;
    match &back {
        ServerMsg::AreaChange { area, arrival, .. } => {
            assert_eq!(area.key, HALL);
            assert_eq!((arrival.x, arrival.y), (EXIT_X - 50.0, 0.0));
        }
        other => panic!("{other:?}"),
    }
    a.client.send(&honest_answer(&adapter, &back)).await;
    let (entities, _) = assert_snapshot_then_placement(&mut a.client, HALL).await;
    assert_eq!((entities[0].id, entities[0].pos.x), (a_id, EXIT_X - 50.0));

    // A second player lies about the fingerprint of the area it was sent to: out.
    let mut b = join_hall(server.addr, "Bob", &adapter).await;
    let offer = cross_the_hall_exit(&mut b, 1).await;
    let ServerMsg::AreaChange { area, provenance, .. } = &offer else { panic!() };
    b.client
        .send(&ClientMsg::JoinArea { area: area.clone(), fingerprint: Fingerprint([7; 32]), provenance: provenance.clone() })
        .await;
    b.client.expect_reject("fingerprint mismatch").await;
    // A third answers with the wrong area.
    let mut c = join_hall(server.addr, "Carol", &adapter).await;
    let offer = cross_the_hall_exit(&mut c, 1).await;
    let ServerMsg::AreaChange { fingerprint, provenance, .. } = &offer else { panic!() };
    c.client
        .send(&ClientMsg::JoinArea { area: info_of(&adapter, HALL).id, fingerprint: *fingerprint, provenance: provenance.clone() })
        .await;
    c.client.expect_reject("area mismatch").await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answers_that_nobody_asked_for_and_slow_answers_end_the_connection() {
    let adapter = RoomsAdapter::new();
    let config = ServerConfig { join_timeout: Duration::from_millis(800), ..rooms_config() };
    let server = TestServer::start_over(config, Box::new(RoomsAdapter::new())).await;

    // A JoinArea when no transfer is under way is out of place, as it always was.
    let mut a = join_hall(server.addr, "Alice", &adapter).await;
    a.client.send(&join_message(&info_of(&adapter, HALL))).await;
    a.client.expect_reject("unexpected message").await;
    let mut a = join_hall(server.addr, "Alice2", &adapter).await;
    a.client.send(&ClientMsg::CannotJoin { area: info_of(&adapter, HALL).id, reason: "x".into() }).await;
    a.client.expect_reject("unexpected message").await;

    // A player that is offered an area and says nothing is disconnected after the join timeout,
    // and the others notice nothing but its departure.
    let mut b = join_hall(server.addr, "Bob", &adapter).await;
    let mut watcher = join_hall(server.addr, "Watcher", &adapter).await;
    let _offer = cross_the_hall_exit(&mut b, 1).await;
    b.client.expect_closed().await;
    // The watcher is fine (and was told B left the hall when B crossed).
    watcher.client.send(&ClientMsg::Ping { nonce: 5 }).await;
    watcher.client.until("Pong", |m| matches!(m, ServerMsg::Pong { nonce: 5, .. }).then_some(())).await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exit_to_an_area_that_cannot_be_loaded_leads_nowhere() {
    // (The two adapters list `hall` identically; the ghost only shows in the hall's exits.)
    let adapter = RoomsAdapter::with_unloadable_area();
    let server = TestServer::start_over(rooms_config(), Box::new(RoomsAdapter::with_unloadable_area())).await;
    let mut a = join_hall(server.addr, "Alice", &adapter).await;
    let a_id = a.entity;
    // The hall's top exit (y = 250) leads to `ghost`, which the adapter lists but cannot load.
    walk(&mut a, 1, -100.0, 200.0).await;
    a.client.until("A's update", |m| update_for(m, a_id).filter(|e| e.pos.y == 200.0)).await;
    walk(&mut a, 2, -50.0, 280.0).await;
    let moved = a.client.until("A's update beyond the line", |m| update_for(m, a_id).filter(|e| e.pos.y == 280.0)).await;
    assert_eq!((moved.pos.x, moved.pos.y), (-50.0, 280.0), "the move stood: no transfer happened");
    let quiet = a.client.drain_for(Duration::from_millis(300)).await;
    assert!(!quiet.iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })), "{quiet:?}");
    a.client.send(&ClientMsg::Leave).await;
    a.client.expect_closed().await;
    server.shutdown().await;
}
