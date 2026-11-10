//! A genuinely registered extended-save account through the ordinary opening save paths.
//! Covers battle 300 (1 -> 6), team talk's durable landing (6 -> 7), run-ahead's ordinary
//! save (7 -> 8), ordered holds and reconnect. This is not a full reactor or Disc 1 traversal.
//! All account state is temporary; the owned library is read only. No debug story is seeded.

mod common;
#[path = "common/walk.rs"]
mod walk;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use common::{Client, TestServer};
use fmv_adapter_api::{AreaRuntime, MoveResult, StoryPoint};
use fmv_core::{AccountId, AnimState, EntityId, GameId, InventoryEntry, Pos};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{scene, SceneBoundary, ScenePlay, SceneResources, Then, WindowAck};
use fmv_ff7::story::StoryState;
use fmv_ff7_battle::rules::{Command, Outcome, Target};
use fmv_persist::{CharacterProgress, Progression, SceneCheckpoint, ScenePhase, SceneState, Store};
use fmv_protocol::battle::battle_party_snapshot;
use fmv_protocol::{ActorId, Auth, BattleId, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup,
    ClientMsg, NewCharacter, SceneOutcome, SceneRole, Secret, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ff7::{open_battle_data, open_ff7, prewarm, Ff7Source, SharedFf7};
use fmv_server::scene_host::Ff7SceneHost;
use fmv_server::{BattleSettings, SceneSettings, ServerConfig};

struct Peer {
    client: Client,
    entity: EntityId,
    account: AccountId,
    token: String,
    pos: Pos,
    facing: f32,
    progress: Vec<u8>,
    gil: u32,
    inventory: Vec<InventoryEntry>,
    sequence: u32,
}

async fn login(addr: std::net::SocketAddr, token: Option<&str>) -> Peer {
    login_at(addr, token, "md1stin").await
}

async fn login_at(addr: std::net::SocketAddr, token: Option<&str>, target: &str) -> Peer {
    let mut client = Client::connect(addr).await;
    let auth = match token {
        Some(token) => Auth::Token(Secret::new(token.to_string())),
        None => Auth::Register { display_name: "Fresh opening traveller".into() },
    };
    client.send(&ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, client_build: "fresh-opening-test".into(),
        auth, character: NewCharacter::with_tint([9, 9, 9]) }).await;
    let (mut identity, mut position, mut progress, mut gil, mut inventory) = (None, None, None, None, None);
    loop {
        match client.next().await {
            ServerMsg::Welcome { entity, account, issued_token, .. } => {
                let credential = match (token, issued_token) {
                    (None, Some(value)) => value.expose().to_string(),
                    (Some(value), None) => value.to_string(),
                    _ => panic!("only a true registration issues a new credential"),
                };
                identity = Some((entity, account, credential));
            }
            ServerMsg::AreaManifest { area, fingerprint, provenance, .. } => {
                assert_eq!(area.key, "md1stin");
                client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
            }
            ServerMsg::AreaChange { area, fingerprint, provenance, .. } => {
                assert_eq!(area.key, target, "only the saved landing redirects this early story");
                client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
            }
            ServerMsg::AreaSnapshot { area, entities, .. } => {
                if area.key == target {
                    let entity = identity.as_ref().expect("Welcome before snapshot").0;
                    let own = entities.iter().find(|p| p.id == entity).expect("own snapshot");
                    position = Some((own.pos, own.facing));
                }
            }
            ServerMsg::StoryUpdate { progress: value, advance } => { assert!(advance.is_none()); progress = Some(value); }
            ServerMsg::CharacterProgress { gil: value, .. } => gil = Some(value),
            ServerMsg::Inventory { items } => inventory = Some(items),
            ServerMsg::SceneStart { .. } | ServerMsg::EncounterStart { .. } | ServerMsg::StoryPending { .. }
                => panic!("registration/relogin cannot replay a consumed opening step"),
            ServerMsg::Reject { code, reason } => panic!("login refused: {code:?}: {reason}"),
            _ => {}
        }
        if let (Some((entity, account, credential)), Some((pos, facing)), Some(progress), Some(gil), Some(inventory)) =
            (identity.as_ref(), position, progress.as_ref(), gil, inventory.as_ref()) {
            return Peer { client, entity: *entity, account: *account, token: credential.clone(), pos, facing,
                progress: progress.clone(), gil, inventory: inventory.clone(), sequence: 0 };
        }
    }
}

async fn boot(state: &Path, adapter: &Arc<fmv_ff7::Ff7Adapter>, rules: &Arc<fmv_protocol::Ff7BattleData>) -> TestServer {
    TestServer::start_over(ServerConfig {
        area_key: "md1stin".into(), state_dir: Some(state.to_path_buf()),
        battle: Some(BattleSettings { random_encounters: false, battle_speed: 0, ticks_per_second: 200,
            fixed_seed: Some(1), companions_player_commanded: true, ..BattleSettings::new(rules.clone()) }),
        scenes: Some(SceneSettings { host: Arc::new(Ff7SceneHost::with_extended_save(adapter.clone(), true)),
            debug_preview: false, debug_new_story: None }),
        ..ServerConfig::default()
    }, Box::new(SharedFf7(adapter.clone()))).await
}

/// BFS over accepted owned-walkmesh steps, stopping on the first crossing of this story point.
/// Other field exits are excluded, so a region probe cannot accidentally become a transfer.
fn route_to_point(area: &dyn AreaRuntime, from: Pos, point: &StoryPoint) -> Vec<Pos> {
    const CELL: f32 = 30.0;
    let bounds = area.bounds();
    let cell = |p: Pos| (((p.x - bounds.min.x) / CELL).round() as i64, ((p.y - bounds.min.y) / CELL).round() as i64);
    let start = cell(from);
    let mut seen = HashMap::from([(start, (None, from))]);
    let mut queue = VecDeque::from([start]);
    let exits = area.exits();
    while let Some(at) = queue.pop_front() {
        let current = seen[&at].1;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let next = (at.0 + dx, at.1 + dy);
            if seen.contains_key(&next) { continue; }
            let wanted = Pos::new(bounds.min.x + next.0 as f32 * CELL, bounds.min.y + next.1 as f32 * CELL, current.z);
            let MoveResult::Accepted(pos) = area.validate_move(current, wanted) else { continue };
            if !pos.is_finite() || pos.dist_xy(wanted) > 0.75 || exits.iter().any(|exit| exit.is_crossed_by(current, pos)) { continue; }
            seen.insert(next, (Some(at), pos));
            if point.is_hit_by(current, pos) && !point.is_hit_by(current, current) {
                let mut route = vec![pos];
                let mut previous = at;
                while let Some(parent) = seen[&previous].0 {
                    route.push(seen[&previous].1);
                    previous = parent;
                }
                route.reverse();
                return route;
            }
            queue.push_back(next);
        }
    }
    panic!("no accepted route from the real spawn to {}", point.label);
}

async fn walk(peer: &mut Peer, route: &[Pos]) {
    for pos in route {
        // Refill movement budget, never used as evidence that a save completed.
        tokio::time::sleep(Duration::from_millis(100)).await;
        peer.sequence += 1;
        peer.client.send(&ClientMsg::MoveIntent { seq: peer.sequence, pos: *pos, facing: 0.0, anim: AnimState::Walk }).await;
        peer.pos = *pos;
        peer.facing = 0.0;
    }
}

async fn pending(peer: &mut Peer, field: &str) -> u64 {
    loop {
        match peer.client.next().await {
            ServerMsg::StoryPending { id, area, pos, facing } => {
                assert_eq!(area.key, field);
                assert_eq!((pos, facing), (peer.pos, peer.facing), "hold retains the accepted trigger crossing");
                return id;
            }
            ServerMsg::MoveRejected { seq, corrected } => panic!("opening step {seq} rejected: {corrected:?}"),
            ServerMsg::StoryCommitted { .. } | ServerMsg::EncounterStart { .. }
            | ServerMsg::SceneEnded { .. } => panic!("effects or scene release cannot precede the save hold"),
            ServerMsg::StoryUpdate { advance: Some(_), .. } => panic!("story cannot publish before its hold"),
            ServerMsg::SceneRefused { reason } => panic!("opening scene refused: {reason:?}"),
            ServerMsg::BattleRejected { reason, .. } => panic!("opening battle refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("opening refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

/// The ordered Ping observes every earlier wrong ACK; neither a fast saver nor a stale
/// transaction id may release the current hold. No elapsed-time assumption is used.
async fn wrong_ack_barrier(peer: &mut Peer, current: u64, wrong: u64, nonce: u64) {
    assert_ne!(current, wrong);
    peer.client.send(&ClientMsg::StoryPendingAck { id: wrong }).await;
    peer.client.send(&ClientMsg::Ping { nonce }).await;
    loop {
        match peer.client.next().await {
            ServerMsg::Pong { nonce: actual, .. } if actual == nonce => return,
            ServerMsg::StoryCommitted { .. } | ServerMsg::EncounterStart { .. } | ServerMsg::BattleEnd { .. }
            | ServerMsg::AreaChange { .. } | ServerMsg::SceneEnded { .. } => panic!("a wrong ACK released a held transition"),
            ServerMsg::StoryUpdate { advance: Some(_), .. } => panic!("a wrong ACK published an unacknowledged transition"),
            ServerMsg::Reject { code, reason } => panic!("wrong-ACK probe disconnected: {code:?}: {reason}"),
            _ => {}
        }
    }
}

async fn encounter(peer: &mut Peer, rules: &fmv_protocol::Ff7BattleData) -> (BattleId, BattleSetup) {
    loop {
        match peer.client.next().await {
            ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack,
                battle_speed, ticks_per_second, escape_policy, initial_party, victory_fanfare, game_over_on_defeat, data_fingerprint } => {
                assert_eq!(formation_id, 300, "the ordinary station story fight");
                assert_eq!(party.iter().filter(|member| member.is_companion()).map(|member| member.character).collect::<Vec<_>>(),
                    [0], "the source-backed opening station party contains only canon Cloud");
                assert!(!victory_fanfare, "the station script applies BTLMD 0x20 before this fight");
                assert!(game_over_on_defeat, "the authored station fight enters Game Over on defeat");
                assert_eq!(data_fingerprint, rules.data_fingerprint());
                return (battle_id, BattleSetup { formation_id, seed, party, preemptive, back_attack,
                    battle_speed, ticks_per_second, escape_policy, initial_party, victory_fanfare, game_over_on_defeat });
            }
            ServerMsg::SceneBattleStarted { .. } | ServerMsg::SceneBattleResumed { .. } => panic!("ordinary story battle is not a scene continuation"),
            ServerMsg::StoryCommitted { .. } => panic!("the initial battle hold spans the entire fight"),
            ServerMsg::BattleRejected { reason, .. } => panic!("encounter refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("encounter refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

async fn win(peer: &mut Peer, rules: &fmv_protocol::Ff7BattleData, hold: u64, battle: BattleId,
    setup: &BattleSetup, expected_story: &[u8]) -> (BattleReward, u16, Option<u16>) {
    let mut replay = BattleReplay::start(rules, setup).unwrap();
    let mut pending: Option<(ActorId, Command)> = None;
    let mut terminal_race = false;
    let mut reward = None;
    let mut story = false;
    let mut used = 0u16;
    assert!(peer.inventory.iter().any(|i| i.item == 0 && i.count > 0), "fresh registration supplies a Potion for the real cost check");
    loop {
        if pending.is_none() && !terminal_race && replay.state().outcome().is_none() {
            if let Some(actor) = replay.state().ready_actors().into_iter().find(|id| setup.party[usize::from(id.0)].is_commanded_by(peer.account)) {
                let command = if used == 0 { Command::Item { item: 0, target: Target::One(actor) } } else {
                    let target = replay.state().enemy_ids().into_iter().find(|id| replay.state().actor(*id).is_some_and(|a| a.is_alive())).unwrap();
                    Command::Attack { target }
                };
                peer.client.send(&ClientMsg::BattleCommand { battle_id: battle, actor, command }).await;
                pending = Some((actor, command));
            }
        }
        match peer.client.next().await {
            ServerMsg::BattleEvent { battle_id, tick, events, command } => {
                assert_eq!(battle_id, battle);
                replay.apply(tick, &events, command.as_ref()).expect("owned replay agrees with the server");
                if let Some(command) = command.filter(|c| setup.party[usize::from(c.actor.0)].is_commanded_by(peer.account)) {
                    assert_eq!(pending.take(), Some((command.actor, command.command)));
                    if matches!(command.command, Command::Item { item: 0, .. }) { used += 1; }
                }
            }
            ServerMsg::BattleRejected { battle_id, reason } => {
                assert_eq!(battle_id, Some(battle));
                let (actor, command) = pending.take().expect("only a pending command can be rejected");
                match reason {
                    BattleRejection::BattleOver => terminal_race = true,
                    BattleRejection::ActorDead => assert!(replay.state().actor(actor).is_some_and(|a| !a.is_alive())),
                    BattleRejection::TargetDead => {
                        let target = match command {
                            Command::Attack { target } | Command::Item { target: Target::One(target), .. } => target,
                            _ => panic!("unexpected target-dead command"),
                        };
                        assert!(replay.state().actor(target).is_some_and(|a| !a.is_alive()));
                    }
                    other => panic!("unexpected battle refusal: {other:?}"),
                }
            }
            ServerMsg::BattleEnd { battle_id, outcome, rewards } => {
                assert_eq!((battle_id, outcome), (battle, Outcome::Victory));
                replay.check_end(outcome, &rewards).unwrap();
                assert!(reward.is_none(), "one durable victory notification");
                reward = Some(rewards.into_iter().find(|r| r.account == peer.account).unwrap());
            }
            ServerMsg::StoryUpdate { progress, advance: Some(advance) } => {
                assert_eq!((advance.point, progress.as_slice()), (0, expected_story));
                peer.progress = progress;
                story = true;
            }
            ServerMsg::StoryCommitted { id } => {
                assert_eq!(id, hold);
                assert!(story && reward.is_some(), "the atomic story and battle result precede release");
                assert_eq!(used, 1, "a real item cost is part of the atomic result");
                let player = setup.party.iter().position(|m| m.account() == Some(peer.account)).unwrap();
                let actor = ActorId(u8::try_from(player).unwrap());
                let limit = replay.state().limit_for(actor).map(|_| battle_party_snapshot(replay.state())[player].limit);
                return (reward.unwrap(), used, limit);
            }
            ServerMsg::StoryPending { .. } => panic!("one token holds snapshot, battle and final settlement"),
            ServerMsg::Reject { code, reason } => panic!("battle failed: {code:?}: {reason}"),
            _ => {}
        }
    }
}

async fn enter_next(peer: &mut Peer, adapter: &fmv_ff7::Ff7Adapter) {
    let area = adapter.load_field("md1stin").unwrap();
    let exit = area.exits().into_iter().find(|e| e.target == "md1_1").unwrap();
    let route = walk::route_across(&area, peer.pos, &exit).unwrap();
    walk(peer, &route).await;
    let mut joined = false;
    loop {
        match peer.client.next().await {
            ServerMsg::AreaChange { area, fingerprint, provenance, .. } => {
                assert_eq!(area.key, "md1_1");
                peer.client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
                joined = true;
            }
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == "md1_1" => {
                assert!(joined);
                let own = entities.iter().find(|p| p.id == peer.entity).unwrap();
                peer.pos = own.pos;
                peer.facing = own.facing;
                return;
            }
            ServerMsg::MoveRejected { seq, corrected } => panic!("gateway step {seq} rejected: {corrected:?}"),
            ServerMsg::EncounterStart { .. } => panic!("the paid station battle cannot replay on the way out"),
            ServerMsg::Reject { code, reason } => panic!("gateway refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

async fn scene_start(peer: &mut Peer, expected: &str) -> ServerMsg {
    loop {
        match peer.client.next().await {
            start @ ServerMsg::SceneStart { .. } => {
                let ServerMsg::SceneStart { scene, owner, you, .. } = &start else { unreachable!() };
                assert_eq!(scene, expected);
                assert_eq!((*owner, *you), (peer.entity, SceneRole::Owner));
                return start;
            }
            ServerMsg::MoveRejected { seq, corrected } => panic!("scene approach {seq} rejected: {corrected:?}"),
            ServerMsg::StoryUpdate { progress, advance } => {
                // Retiring a saved landing may publish that identical image before ordinary entry.
                assert_eq!(progress, peer.progress, "a new point cannot advance before its presentation: {advance:?}");
            }
            ServerMsg::SceneRefused { reason } => panic!("scene refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("scene refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

/// Play every strict window and stop before acknowledging the authored destination.
async fn team_talk_landing(peer: &mut Peer, adapter: &fmv_ff7::Ff7Adapter) -> (Vec<u8>, Pos) {
    let start = scene_start(peer, "ff7/md1_1/team-talk").await;
    let ServerMsg::SceneStart { run, scene: id, seed, progress, gil, replay, .. } = start else { unreachable!() };
    let replay = replay.expect("team talk is a durable authored continuation");
    replay.validate().unwrap();
    assert!(replay.events.is_empty());
    let origin = replay.origin.expect("team talk binds Cloud to the accepted player crossing");
    assert_eq!((origin.pos, origin.facing), (peer.pos, peer.facing));
    let def = scene(&id).unwrap();
    let area = adapter.load_field(&def.field).unwrap();
    let def = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &def,
        origin.pos, origin.facing, origin.triangle).expect("accepted team-talk origin");
    let before = StoryState::decode(&progress).unwrap();
    let mut play = ScenePlay::with_resources(area.field_file(), &def, &before,
        seed, SceneResources { gil: Some(gil) }).unwrap();
    assert_eq!(play.replay_identity().fingerprint, replay.source);
    play.next_strict(None).unwrap();
    let mut count = 0;
    loop {
        match play.boundary().unwrap() {
            SceneBoundary::Window(mark) => {
                assert!(mark.choices.is_none());
                assert_eq!(mark.ordinal, count + 1);
                peer.client.send(&ClientMsg::SceneStep { run, step: count, answer: None }).await;
                play.next_strict(Some(WindowAck { checkpoint: mark, answer: None })).unwrap();
                count += 1;
            }
            SceneBoundary::End(end) => { assert_eq!(end, End::MapJump { field: 118 }); break; }
            SceneBoundary::Battle(_) => panic!("team talk has no battle"),
        }
    }
    assert!(count > 0);
    let snapshot = play.snapshot().unwrap();
    let mut after = before.clone();
    snapshot.writes.apply_extended_to(&mut after, &before).unwrap();
    assert_eq!(after.moment, 7);
    let jump = snapshot.jump.expect("team talk's authored MAPJUMP");
    assert_eq!((jump.field, jump.x, jump.y, jump.triangle, jump.direction), (118, 3549, 30574, 60, 128));
    let destination = adapter.load_field("md1_2").unwrap();
    let placed = destination.place_arrival(Pos::new(3549.0, 30574.0, 0.0), Some(60));
    for _ in 0..2 { peer.client.send(&ClientMsg::SceneEnd { run, skipped: false }).await; }
    let mut ended = false;
    loop {
        match peer.client.next().await {
            ServerMsg::SceneEnded { run: actual, outcome } => {
                assert_eq!((actual, outcome), (run, SceneOutcome::Finished));
                ended = true;
            }
            ServerMsg::StoryUpdate { progress, .. } => {
                assert!(StoryState::decode(&progress).unwrap().supports_extended());
                peer.progress = progress;
            }
            ServerMsg::AreaChange { area, arrival, arrival_facing, .. } => {
                assert!(ended, "the source scene commits before travel");
                assert_eq!(area.key, "md1_2");
                assert_eq!((arrival, arrival_facing), (placed, fmv_ff7::field_direction_to_facing(128)));
                assert_eq!(peer.progress, after.encode());
                return (after.encode(), placed); // No JoinArea: recover the persisted landing on token reconnect.
            }
            ServerMsg::SceneRefused { reason } => panic!("team talk completion refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("team talk completion refused: {code:?}: {reason}"),
            ServerMsg::StoryPending { .. } | ServerMsg::EncounterStart { .. } => panic!("strict team talk uses its scene checkpoint"),
            _ => {}
        }
    }
}

async fn finish_run_ahead(peer: &mut Peer, adapter: &fmv_ff7::Ff7Adapter) {
    let start = scene_start(peer, "ff7/md1_2/run-ahead").await;
    let ServerMsg::SceneStart { run, scene: id, seed, progress, gil, replay, .. } = start else { unreachable!() };
    assert!(replay.is_none(), "run-ahead remains the ordinary point presentation");
    let def = scene(&id).unwrap();
    let area = adapter.load_field(&def.field).unwrap();
    let mut play = ScenePlay::with_resources(area.field_file(), &def, &StoryState::decode(&progress).unwrap(),
        seed, SceneResources { gil: Some(gil) }).unwrap();
    assert!(matches!(play.next(None).then, Then::End { how: End::Finished, .. }), "run-ahead has no dialogue windows");
    for _ in 0..2 { peer.client.send(&ClientMsg::SceneEnd { run, skipped: false }).await; }
}

fn saved(state: &Path, account: AccountId) -> SceneState {
    Store::open(state).unwrap().get_scene_state(account, GameId::Ff7).unwrap()
}

fn bytes(state: &Path, account: AccountId) -> [Vec<u8>; 2] {
    let path = state.join("progression").join(account.0.to_string());
    [std::fs::read(path.join("Ff7.json")).unwrap(), std::fs::read(path.join("Ff7.character.json")).unwrap()]
}

async fn await_story_image(state: &Path, account: AccountId, expected: &[u8]) {
    let path = state.join("progression").join(account.0.to_string()).join("Ff7.json");
    tokio::time::timeout(common::STEP, async {
        loop {
            let record: Progression = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if record.blob == expected { return; }
            // Poll an explicit storage predicate; elapsed time is never the success condition.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the saver publishes its story image even while the client ACK is withheld");
}

fn expected_character(original: &CharacterProgress, reward: &BattleReward, used: u16, limit: Option<u16>,
    rules: &fmv_protocol::Ff7BattleData) -> CharacterProgress {
    let mut expected = original.clone();
    expected.gil += reward.gil;
    expected.exp += reward.exp;
    expected.ap += reward.ap;
    while expected.level < rules.progress_limits().max_level
        && rules.exp_for_level(expected.character, expected.level + 1).is_some_and(|threshold| expected.exp >= threshold) {
        expected.level += 1;
    }
    if let Some(limit) = limit { expected.limit = limit; }
    let mut inventory: BTreeMap<_, _> = original.inventory.as_ref().unwrap().iter().map(|i| (i.item, i.count)).collect();
    *inventory.get_mut(&0).unwrap() -= used;
    for item in &reward.items { *inventory.entry(item.item).or_default() += item.count; }
    expected.inventory = Some(inventory.into_iter().filter(|(_, count)| *count > 0).map(|(item, count)| InventoryEntry { item, count }).collect());
    expected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs FMV_FF7_LIBRARY; fresh registration and all saves use a temporary store"]
async fn a_fresh_v3_account_walks_ordinary_battle_and_story_saves_and_recovers_an_unacked_commit() {
    let source = Ff7Source::Library { state_dir: Some(std::env::var_os("FMV_FF7_LIBRARY").expect("owned library").into()), set: None };
    let opened = open_ff7(&source).unwrap();
    prewarm(&opened.adapter).await.unwrap();
    let (rules, _) = open_battle_data(&source).unwrap();
    let station = opened.adapter.load_field("md1stin").unwrap();
    let next = opened.adapter.load_field("md1_1").unwrap();
    let runway = opened.adapter.load_field("md1_2").unwrap();
    let battle_point = station.story_points().into_iter().find(|p| p.id == 0).unwrap();
    let team_point = next.story_points().into_iter().find(|p| p.id == 0).unwrap();
    let normal_point = runway.story_points().into_iter().find(|p| p.id == 0).unwrap();
    let fresh = StoryState::fresh_first_control().encode();
    assert_eq!(station.story_battle_for(&fresh, battle_point.id), Some(300));
    assert_eq!(runway.story_battle_for(&fresh, normal_point.id), None);
    let state = tempfile::tempdir().unwrap();
    let mut server = boot(state.path(), &opened.adapter, &rules).await;
    let mut peer = login(server.addr, None).await;
    let (account, token) = (peer.account, peer.token.clone());
    assert_eq!(fresh.first(), Some(&3));
    assert_eq!(peer.progress, fresh, "real registration installs the reviewed v3 profile");
    server.shutdown().await;
    let initial = saved(state.path(), account);
    assert_eq!(initial.pair.story.blob, fresh);
    assert!(initial.checkpoint.is_none() && initial.reservation.is_none());
    let original = initial.pair.character.unwrap();
    assert_eq!((original.gil, original.inventory.as_ref()), (peer.gil, Some(&peer.inventory)));
    let initial_bytes = bytes(state.path(), account);
    server = boot(state.path(), &opened.adapter, &rules).await;
    peer = login(server.addr, Some(&token)).await;
    assert_eq!((peer.account, &peer.progress, peer.gil), (account, &fresh, original.gil));
    assert_eq!(bytes(state.path(), account), initial_bytes, "token login does not reinitialize the profile");

    let battle_story = station.story_advance(&fresh, battle_point.id).unwrap().progress;
    assert_eq!(StoryState::decode(&battle_story).unwrap().moment, 6);
    let route = route_to_point(&station, peer.pos, &battle_point);
    walk(&mut peer, &route).await;
    let battle_hold = pending(&mut peer, "md1stin").await;
    wrong_ack_barrier(&mut peer, battle_hold, battle_hold.wrapping_add(10_000), 6100).await;
    peer.client.send(&ClientMsg::StoryPendingAck { id: battle_hold }).await;
    let (battle_id, setup) = encounter(&mut peer, &rules).await;
    let (reward, used, limit) = win(&mut peer, &rules, battle_hold, battle_id, &setup, &battle_story).await;
    let expected = expected_character(&original, &reward, used, limit, &rules);

    enter_next(&mut peer, &opened.adapter).await;
    let route = route_to_point(&next, peer.pos, &team_point);
    walk(&mut peer, &route).await;
    let (landed_story, landed_pos) = team_talk_landing(&mut peer, &opened.adapter).await;
    let landed_bytes = bytes(state.path(), account);
    let landing_story: Progression = serde_json::from_slice(&landed_bytes[0]).unwrap();
    let landing_character: serde_json::Value = serde_json::from_slice(&landed_bytes[1]).unwrap();
    let checkpoint: SceneCheckpoint = serde_json::from_value(landing_character["checkpoint"].clone()).unwrap();
    assert_eq!(landing_story.blob, landed_story, "travel was offered only after its saved story");
    assert!(matches!(checkpoint.phase, ScenePhase::Landing { .. }));
    assert!(landing_character.get("reservation").is_none());
    // Retain the World so the earlier battle ACK is genuinely stale in this same token epoch.
    // EOF is the disconnect barrier; no timer guesses whether the account was released.
    peer.client.stream.shutdown().await.unwrap();
    peer.client.expect_closed().await;
    drop(peer);
    let normal_story = runway.story_advance(&landed_story, normal_point.id).expect("ordinary run-ahead entry").progress;
    assert_eq!(StoryState::decode(&normal_story).unwrap().moment, 8);
    peer = login_at(server.addr, Some(&token), "md1_2").await;
    assert_eq!((&peer.progress, peer.pos, peer.facing),
        (&landed_story, landed_pos, fmv_ff7::field_direction_to_facing(128)), "reconnect uses the authored landing, not an invented spawn");
    finish_run_ahead(&mut peer, &opened.adapter).await;
    let normal_hold = pending(&mut peer, "md1_2").await;
    await_story_image(state.path(), account, &normal_story).await;
    wrong_ack_barrier(&mut peer, normal_hold, battle_hold, 8100).await;
    // Do not acknowledge this hold. Disconnect and graceful shutdown drain the saver, proving
    // the CAS runs independently of client ACK without guessing a disk-write delay.
    drop(peer);
    server.shutdown().await;
    let complete = saved(state.path(), account);
    assert_eq!(complete.pair.story.blob, normal_story);
    assert_eq!(complete.pair.story.blob.first(), Some(&3));
    assert!(complete.checkpoint.is_none() && complete.reservation.is_none());
    let actual = complete.pair.character.as_ref().unwrap();
    let mut expected = expected;
    expected.updated_at = actual.updated_at;
    expected.schema_version = actual.schema_version;
    assert_eq!(actual, &expected, "story and reward settle once, preserving the accepted Potion cost and final gauge");
    let completed_bytes = bytes(state.path(), account);

    server = boot(state.path(), &opened.adapter, &rules).await;
    peer = login(server.addr, Some(&token)).await;
    assert_eq!((peer.account, &peer.progress, peer.gil, &peer.inventory),
        (account, &normal_story, actual.gil, actual.inventory.as_ref().unwrap()));
    // Ordinary early progress returns to the normal station spawn. Re-cross its old battle
    // region: neither the consumed fight nor the reward may repeat, including stale ACKs.
    let route = route_to_point(&station, peer.pos, &battle_point);
    walk(&mut peer, &route).await;
    for id in [battle_hold, normal_hold] { peer.client.send(&ClientMsg::StoryPendingAck { id }).await; }
    peer.client.send(&ClientMsg::Ping { nonce: 7700 }).await;
    loop {
        match peer.client.next().await {
            ServerMsg::Pong { nonce: 7700, .. } => break,
            ServerMsg::StoryPending { .. } | ServerMsg::StoryCommitted { .. } | ServerMsg::EncounterStart { .. }
            | ServerMsg::BattleEnd { .. } | ServerMsg::SceneStart { .. } => panic!("relogin or crossing repeats committed work"),
            ServerMsg::StoryUpdate { advance: Some(_), .. } => panic!("the consumed story point advanced again"),
            ServerMsg::MoveRejected { seq, corrected } => panic!("reconnected control remains held at step {seq}: {corrected:?}"),
            ServerMsg::Reject { code, reason } => panic!("relogin failed: {code:?}: {reason}"),
            _ => {}
        }
    }
    server.shutdown().await;
    assert_eq!(saved(state.path(), account), complete);
    assert_eq!(bytes(state.path(), account), completed_bytes);
    eprintln!("fresh opening TCP: registered v3, ordinary300 victory/Potion, saved team-talk landing7 reconnect, ordinary point8 staleACK fencing and unacked-save server restart; full reactor route not covered");
}
