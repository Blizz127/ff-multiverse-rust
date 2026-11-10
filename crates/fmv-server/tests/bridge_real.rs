//! Owned-data TCP proof of the bridge's authored trigger, durable battles, and cargo arrival.
//! Every account, checkpoint and restart belongs to a temporary store; owned assets are read only.

mod common;
#[path = "common/walk.rs"]
mod walk;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, TestServer};
use fmv_adapter_api::{AreaRuntime, MoveResult, StoryPoint, StoryTrigger};
use fmv_core::{AccountId, AnimState, EntityId, GameId, InventoryEntry, Pos};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    scene, BattleResolution, BattleResume, SceneBoundary, ScenePlay, SceneResources, WindowAck,
    BRIDGE_SCENE_ID, CARGO_TRAIN_SCENE_ID,
};
use fmv_ff7::story::StoryState;
use fmv_ff7_battle::rules::{Command, Outcome, Target};
use fmv_persist::{CharacterProgress, Progression, ScenePhase, SceneState, Store};
use fmv_protocol::battle::battle_party_snapshot;
use fmv_protocol::{
    ActorId, Auth, BattleId, BattleInitialState, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup,
    ClientMsg, EscapePolicy, NewCharacter, SceneOrigin, SceneOutcome, SceneReplayEvent, SceneRole, SceneRunId,
    Secret, ServerMsg, PROTOCOL_VERSION,
};
use fmv_server::ff7::{open_battle_data, open_ff7, prewarm, Ff7Source, SharedFf7};
use fmv_server::scene_host::Ff7SceneHost;
use fmv_server::{BattleSettings, SceneSettings, ServerConfig};

struct Peer {
    client: Client,
    pending: VecDeque<ServerMsg>,
    entity: EntityId,
    pos: Pos,
    progress: Vec<u8>,
    gil: u32,
}

impl Peer {
    async fn next(&mut self) -> ServerMsg {
        match self.pending.pop_front() { Some(message) => message, None => self.client.next().await }
    }

    async fn scene_start(&mut self, expected: &str) -> ServerMsg {
        loop {
            match self.next().await {
                message @ ServerMsg::SceneStart { .. } => {
                    let ServerMsg::SceneStart { scene, owner, you, .. } = &message else { unreachable!() };
                    assert_eq!(scene, expected);
                    assert_eq!((*owner, *you), (self.entity, SceneRole::Owner));
                    return message;
                }
                ServerMsg::MoveRejected { seq, corrected } => panic!("authored route step {seq} refused: {corrected:?}"),
                ServerMsg::Reject { code, reason } => panic!("scene refused: {code:?}: {reason}"),
                _ => {}
            }
        }
    }
}

async fn login(addr: std::net::SocketAddr, token: &str, target: &str) -> Peer {
    let mut client = Client::connect(addr).await;
    client.send(&ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, client_build: "bridge-route-test".into(),
        auth: Auth::Token(Secret::new(token.to_string())), character: NewCharacter::with_tint([9, 9, 9]) }).await;
    let (mut entity, mut gil, mut progress, mut pos) = (None, None, None, None);
    let mut pending = VecDeque::new();
    loop {
        match client.next().await {
            ServerMsg::Welcome { entity: value, .. } => entity = Some(value),
            ServerMsg::CharacterProgress { gil: value, .. } => gil = Some(value),
            ServerMsg::StoryUpdate { progress: value, .. } => progress = Some(value),
            ServerMsg::AreaManifest { area, fingerprint, provenance, .. }
            | ServerMsg::AreaChange { area, fingerprint, provenance, .. } => {
                client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
            }
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == target => {
                let own = entity.expect("Welcome before the area snapshot");
                pos = Some(entities.iter().find(|p| p.id == own).expect("own entity").pos);
            }
            ServerMsg::Reject { code, reason } => panic!("login refused: {code:?}: {reason}"),
            message @ (ServerMsg::SceneStart { .. } | ServerMsg::SceneBattleStarted { .. }
                | ServerMsg::EncounterStart { .. } | ServerMsg::BattleEvent { .. }
                | ServerMsg::BattleEnd { .. } | ServerMsg::SceneBattleResumed { .. }) => pending.push_back(message),
            _ => {}
        }
        if let (Some(entity), Some(gil), Some(progress), Some(pos)) = (entity, gil, progress.as_ref(), pos) {
            return Peer { client, pending, entity, pos, progress: progress.clone(), gil };
        }
    }
}

fn config(state: &Path, adapter: Arc<fmv_ff7::Ff7Adapter>, rules: Arc<fmv_protocol::Ff7BattleData>) -> ServerConfig {
    ServerConfig {
        area_key: "md1stin".into(), state_dir: Some(state.to_path_buf()),
        battle: Some(BattleSettings { random_encounters: false, battle_speed: 0,
            ticks_per_second: 1000, fixed_seed: Some(1), ..BattleSettings::new(rules) }),
        scenes: Some(SceneSettings { host: Arc::new(Ff7SceneHost::with_extended_save(adapter, true)),
            debug_preview: false, debug_new_story: None }),
        ..ServerConfig::default()
    }
}

async fn boot(state: &Path, adapter: &Arc<fmv_ff7::Ff7Adapter>, rules: &Arc<fmv_protocol::Ff7BattleData>) -> TestServer {
    TestServer::start_over(config(state, adapter.clone(), rules.clone()), Box::new(SharedFf7(adapter.clone()))).await
}

/// Walk both real gateways, then cross the director's line through accepted bridge walkmesh moves.
async fn walk_to_bridge(peer: &mut Peer, adapter: &fmv_ff7::Ff7Adapter) {
    let mut sequence = 0u32;
    for (field, target) in [("md8_2", "md8_3"), ("md8_3", "md8brdg")] {
        let area = adapter.load_field(field).unwrap();
        let exit = area.exits().into_iter().find(|exit| exit.target == target).expect("authored forward gateway");
        let path = walk::route_across(&area, peer.pos, &exit).expect("walkable gateway");
        for pos in path {
            tokio::time::sleep(Duration::from_millis(100)).await;
            sequence += 1;
            peer.client.send(&ClientMsg::MoveIntent { seq: sequence, pos, facing: 0.0, anim: AnimState::Walk }).await;
        }
        let mut joined = false;
        loop {
            match peer.next().await {
                ServerMsg::AreaChange { area, fingerprint, provenance, .. } => {
                    assert_eq!(area.key, target);
                    peer.client.send(&ClientMsg::JoinArea { area, fingerprint, provenance }).await;
                    joined = true;
                }
                ServerMsg::AreaSnapshot { area, entities, .. } if area.key == target => {
                    assert!(joined);
                    peer.pos = entities.iter().find(|p| p.id == peer.entity).unwrap().pos;
                    break;
                }
                ServerMsg::MoveRejected { seq, corrected } => panic!("gateway route step {seq} refused: {corrected:?}"),
                ServerMsg::SceneStart { scene, .. } => panic!("unexpected scene before the bridge line: {scene}"),
                ServerMsg::Reject { code, reason } => panic!("route refused: {code:?}: {reason}"),
                _ => {}
            }
        }
    }
    let area = adapter.load_field("md8brdg").unwrap();
    let def = fmv_ff7::script::scene::bridge_scene();
    let line = def.line_trigger().unwrap();
    let point = |p: [i16; 3]| Pos::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
    let trigger = StoryPoint { id: 0, label: "bridge director".into(), trigger: StoryTrigger::Line(point(line.a), point(line.b)) };
    for _ in 0..80 {
        let wanted = Pos::new(peer.pos.x, peer.pos.y - 20.0, peer.pos.z);
        let MoveResult::Accepted(pos) = area.validate_move(peer.pos, wanted) else { panic!("bridge's straight approach is obstructed") };
        assert!(pos.dist_xy(wanted) < 0.75);
        let crossed = trigger.is_hit_by(peer.pos, pos);
        tokio::time::sleep(Duration::from_millis(100)).await;
        sequence += 1;
        peer.client.send(&ClientMsg::MoveIntent { seq: sequence, pos, facing: 0.0, anim: AnimState::Walk }).await;
        peer.pos = pos;
        if crossed { return; }
    }
    panic!("bridge approach never crossed the authored line");
}

struct Driver {
    run: SceneRunId,
    play: ScenePlay,
    source: [u8; 32],
    origin: Option<SceneOrigin>,
    passed: u32,
    answered: usize,
    battles: usize,
}

impl Driver {
    fn from_start(start: ServerMsg, adapter: &fmv_ff7::Ff7Adapter) -> Self {
        let ServerMsg::SceneStart { run, scene: id, seed, progress, gil, replay, .. } = start else { panic!("scene start") };
        let replay = replay.expect("bridge and cargo use strict continuation playback");
        replay.validate().unwrap();
        let mut def = scene(&id).expect("registered continuation");
        let area = adapter.load_field(&def.field).unwrap();
        if let Some(origin) = replay.origin {
            def = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &def,
                origin.pos, origin.facing, origin.triangle).expect("authoritative line-crossing pose");
        } else {
            assert_ne!(id, BRIDGE_SCENE_ID, "the bridge must retain its actual crossing pose");
        }
        let mut play = ScenePlay::with_resources(area.field_file(), &def, &StoryState::decode(&progress).unwrap(),
            seed, SceneResources { gil: Some(gil) }).unwrap();
        assert_eq!(play.replay_identity().fingerprint, replay.source);
        play.next_strict(None).unwrap();
        let (mut passed, mut answered, mut battles) = (0, 0, 0);
        for event in replay.events {
            match (play.boundary().unwrap(), event) {
                (SceneBoundary::Window(mark), SceneReplayEvent::Window { step, identity, answer }) => {
                    assert_eq!(step, mark.ordinal - 1);
                    assert_eq!(identity, mark.identity_bytes());
                    answered += usize::from(answer.is_some());
                    passed = mark.ordinal;
                    play.next_strict(Some(WindowAck { checkpoint: mark, answer })).unwrap();
                }
                (SceneBoundary::Battle(mark), SceneReplayEvent::Battle { ordinal, identity, formation, progress, gil }) => {
                    assert_eq!((ordinal, formation, identity), (mark.ordinal, mark.formation, mark.identity_bytes()));
                    play.resume_battle_checked(BattleResume { checkpoint: mark, resolution: BattleResolution::Victory,
                        story: StoryState::decode(&progress).unwrap(), resources: SceneResources { gil: Some(gil) } }).unwrap();
                    play.next_strict(None).unwrap();
                    battles += 1;
                }
                mismatch => panic!("restored transcript differs: {mismatch:?}"),
            }
        }
        Self { run, play, source: replay.source, origin: replay.origin, passed, answered, battles }
    }

    async fn advance(&mut self, peer: &mut Peer, answers: &[u8]) -> SceneBoundary {
        loop {
            let boundary = self.play.boundary().unwrap();
            let SceneBoundary::Window(mark) = boundary else { return boundary };
            let answer = mark.choices.map(|[first, last]| {
                let answer = *answers.get(self.answered).expect("the original question has a chosen answer");
                assert!((first..=last).contains(&answer));
                self.answered += 1;
                answer
            });
            peer.client.send(&ClientMsg::SceneStep { run: self.run, step: mark.ordinal - 1, answer }).await;
            self.play.next_strict(Some(WindowAck { checkpoint: mark, answer })).unwrap();
            self.passed = mark.ordinal;
        }
    }

    async fn ready(&self, peer: &mut Peer) -> fmv_ff7::script::scene::BattleCheckpoint {
        let SceneBoundary::Battle(mark) = self.play.boundary().unwrap() else { panic!("battle boundary") };
        peer.client.send(&ClientMsg::SceneBattleReady { run: self.run, ordinal: mark.ordinal, step: self.passed }).await;
        mark
    }

    async fn finish(&self, peer: &mut Peer) {
        assert!(matches!(self.play.boundary(), Some(SceneBoundary::End(End::Finished | End::MapJump { .. }))));
        peer.client.send(&ClientMsg::SceneEnd { run: self.run, skipped: false }).await;
        peer.client.send(&ClientMsg::SceneEnd { run: self.run, skipped: false }).await;
    }
}

async fn encounter(peer: &mut Peer, run: SceneRunId, ordinal: u32, rules: &fmv_protocol::Ff7BattleData) -> (BattleId, BattleSetup) {
    let mut announced = None;
    loop {
        match peer.next().await {
            ServerMsg::SceneBattleStarted { run: actual, ordinal: actual_ordinal, battle_id } => {
                assert_eq!((actual, actual_ordinal), (run, ordinal));
                announced = Some(battle_id);
            }
            ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack,
                battle_speed, ticks_per_second, escape_policy, initial_party, data_fingerprint } => {
                assert_eq!(announced, Some(battle_id), "the scene handoff precedes its encounter");
                assert_eq!(data_fingerprint, rules.data_fingerprint());
                assert_eq!(escape_policy, EscapePolicy::Forbidden, "BTLMD 8 overrides escapable scene.bin data");
                return (battle_id, BattleSetup { formation_id, seed, party, preemptive, back_attack,
                    battle_speed, ticks_per_second, escape_policy, initial_party });
            }
            ServerMsg::SceneEnded { .. } => panic!("a battle handoff cannot release its scene"),
            ServerMsg::Reject { code, reason } => panic!("battle handoff refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

/// Replay the real encounter, prove that escape is refused, then attack with every actor this
/// account commands. Returning requires the separate durable SceneBattleResumed acknowledgment.
#[allow(clippy::too_many_arguments)]
async fn win(peer: &mut Peer, account: AccountId, rules: &fmv_protocol::Ff7BattleData,
    run: SceneRunId, ordinal: u32, battle_id: BattleId, setup: &BattleSetup,
    potions: &mut u16, used: &mut u16) -> (Vec<u8>, u32, BattleReward, Vec<BattleInitialState>) {
    let mut replay = BattleReplay::start(rules, setup).expect("owned encounter builds on the client");
    let mut pending: Option<(ActorId, Command)> = None;
    let (mut tried_escape, mut escape_refused, mut awaiting_terminal) = (false, false, false);
    let mut reward = None;
    loop {
        if pending.is_none() && !awaiting_terminal && replay.state().outcome().is_none() {
            let actor = replay.state().ready_actors().into_iter()
                .find(|id| setup.party[usize::from(id.0)].is_commanded_by(account));
            if let Some(actor) = actor {
                let hurt = replay.state().party_ids().into_iter().find(|id| replay.state().actor(*id)
                    .is_some_and(|c| c.is_alive() && c.max_hp.saturating_sub(c.hp) >= 100));
                let command = if !tried_escape {
                    tried_escape = true;
                    Command::Escape
                } else if *used == 0 && *potions > 0 {
                    // A legal use even when little HP is missing: exercise a real item cost
                    // before the first durable victory and the following server restart.
                    Command::Item { item: 0, target: Target::One(actor) }
                } else if let Some(target) = hurt.filter(|_| *potions > 0) {
                    Command::Item { item: 0, target: Target::One(target) }
                } else {
                    let target = replay.state().enemy_ids().into_iter()
                        .find(|id| replay.state().actor(*id).is_some_and(|c| c.is_alive())).expect("living opponent");
                    Command::Attack { target }
                };
                peer.client.send(&ClientMsg::BattleCommand { battle_id, actor, command }).await;
                pending = Some((actor, command));
            }
        }
        match peer.next().await {
            ServerMsg::BattleEvent { battle_id: actual, tick, events, command } => {
                assert_eq!(actual, battle_id);
                replay.apply(tick, &events, command.as_ref()).expect("client and server battle replay agree");
                if let Some(command) = command.filter(|c| setup.party[usize::from(c.actor.0)].is_commanded_by(account)) {
                    assert_eq!(pending.take(), Some((command.actor, command.command)), "an acknowledged command was sent exactly once");
                    if matches!(command.command, Command::Item { item: 0, .. }) {
                        *potions = potions.checked_sub(1).expect("the test cannot spend unavailable Potions");
                        *used += 1;
                    }
                }
            }
            ServerMsg::BattleRejected { battle_id: actual, reason } => {
                assert_eq!(actual, Some(battle_id));
                let (actor, command) = pending.take().expect("only an outstanding command can be refused");
                match reason {
                    BattleRejection::CannotEscape => {
                        assert_eq!(command, Command::Escape);
                        assert!(tried_escape && !escape_refused);
                        escape_refused = true;
                    }
                    // The engine advances to receive time before validating a command. Keep
                    // these races narrow, and still require the terminal replay/end below.
                    BattleRejection::BattleOver => awaiting_terminal = true,
                    BattleRejection::ActorDead => {
                        assert!(replay.state().actor(actor).is_some_and(|c| !c.is_alive()));
                        if command == Command::Escape { tried_escape = false; }
                    }
                    BattleRejection::TargetDead => {
                        let target = match command {
                            Command::Attack { target } | Command::Item { target: Target::One(target), .. } => target,
                            other => panic!("unexpected target-dead command: {other:?}"),
                        };
                        assert!(replay.state().actor(target).is_some_and(|c| !c.is_alive()));
                    }
                    other => panic!("unexpected battle command refusal: {other:?}"),
                }
            }
            ServerMsg::BattleEnd { battle_id: actual, outcome, rewards } => {
                assert_eq!((actual, outcome), (battle_id, Outcome::Victory));
                replay.check_end(outcome, &rewards).expect("authored rewards match the replay");
                assert!(escape_refused);
                reward = Some(rewards.into_iter().find(|r| r.account == account).expect("player reward"));
            }
            ServerMsg::SceneBattleResumed { run: actual, ordinal: actual_ordinal, battle_id: actual_battle, progress, gil } => {
                assert_eq!((actual, actual_ordinal, actual_battle), (run, ordinal, battle_id));
                let reward = reward.expect("BattleEnd precedes the durable resume");
                *potions += reward.items.iter().filter(|item| item.item == 0).map(|item| item.count).sum::<u16>();
                return (progress, gil, reward, battle_party_snapshot(replay.state()));
            }
            ServerMsg::SceneGameOver { .. } => panic!("the commanded bridge fight lost"),
            ServerMsg::SceneEnded { .. } => panic!("victory must resume rather than release the bridge scene"),
            ServerMsg::Reject { code, reason } => panic!("fight refused: {code:?}: {reason}"),
            _ => {}
        }
    }
}

fn saved(state: &Path, account: AccountId) -> SceneState {
    Store::open(state).unwrap().get_scene_state(account, GameId::Ff7).unwrap()
}

fn record_bytes(state: &Path, account: AccountId) -> [Vec<u8>; 2] {
    let dir = state.join("progression").join(account.0.to_string());
    [std::fs::read(dir.join("Ff7.json")).unwrap(), std::fs::read(dir.join("Ff7.character.json")).unwrap()]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs FMV_FF7_LIBRARY; owned assets are read-only and every save is temporary"]
async fn bridge_walk_battle_and_cargo_checkpoints_survive_restart_without_repeating_rewards() {
    let library = std::env::var_os("FMV_FF7_LIBRARY").expect("owned library path");
    let source = Ff7Source::Library { state_dir: Some(library.into()), set: None };
    let opened = open_ff7(&source).unwrap();
    prewarm(&opened.adapter).await.unwrap();
    let (rules, _) = open_battle_data(&source).unwrap();
    for fight in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let mut story = StoryState::at(39).into_extended().unwrap();
        story.set_bit(3, 66, 0, true);
        let before = story.encode();
        let (account, token) = {
            let store = Store::open(state.path()).unwrap();
            let (account, token) = store.create_account("Bridge traveller", 1).unwrap();
            store.put_progression(&Progression::new(account, GameId::Ff7, before.clone(), 1)).unwrap();
            let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, 1);
            character.gil = 600;
            character.exp = 42;
            character.ap = 7;
            character.limit = 33;
            character.inventory = Some(vec![InventoryEntry { item: 0, count: 3 }]);
            store.put_character_progress(&character).unwrap();
            (account, token)
        };
        let answers = if fight { [2, 0, 0] } else { [3, 1, 1] };
        let mut server = boot(state.path(), &opened.adapter, &rules).await;
        let mut peer = login(server.addr, &token, "md8_2").await;
        assert_eq!((&peer.progress, peer.gil), (&before, 600));
        walk_to_bridge(&mut peer, &opened.adapter).await;
        let mut driver = Driver::from_start(peer.scene_start(BRIDGE_SCENE_ID).await, &opened.adapter);
        let activation = (driver.source, driver.origin);
        let mut earned = Vec::new();
        let (mut potions, mut used) = (3, 0);
        let mut previous_party = None;
        if fight {
            assert!(matches!(driver.advance(&mut peer, &answers).await, SceneBoundary::Battle(_)));
            let mark = driver.ready(&mut peer).await;
            assert_eq!((mark.ordinal, mark.formation), (1, 328));
            let (_, first_setup) = encounter(&mut peer, driver.run, mark.ordinal, &rules).await;
            // No commands or rewards have been acknowledged. A restart must reconstruct the
            // saved attempt, not admit a free field character or roll a different battle seed.
            server.shutdown().await;
            let checkpoint = saved(state.path(), account);
            assert_eq!(checkpoint.pair.story.blob, before);
            assert_eq!(checkpoint.pair.character.as_ref().unwrap().gil, 600);
            let stored = checkpoint.checkpoint.as_ref().expect("durable battle checkpoint");
            assert_eq!(stored.source_hash, activation.0);
            assert_eq!(stored.consumed_battles, 0);
            assert!(matches!(stored.phase, ScenePhase::BeforeBattle { ordinal: 0, formation: 328, .. }),
                "the store records the consumed prefix; wire battle ordinals start at one");
            server = boot(state.path(), &opened.adapter, &rules).await;
            peer = login(server.addr, &token, "md8brdg").await;
            driver = Driver::from_start(peer.scene_start(BRIDGE_SCENE_ID).await, &opened.adapter);
            assert_eq!((driver.source, driver.origin), activation, "recovery cannot move the original scene activation");
            assert_eq!((driver.answered, driver.battles), (1, 0));
            let mark = driver.ready(&mut peer).await;
            let (battle_id, setup) = encounter(&mut peer, driver.run, mark.ordinal, &rules).await;
            assert_eq!((setup.formation_id, setup.seed, &setup.initial_party),
                (first_setup.formation_id, first_setup.seed, &first_setup.initial_party));
            let (progress, gil, reward, carry) = win(&mut peer, account, &rules, driver.run, mark.ordinal, battle_id, &setup,
                &mut potions, &mut used).await;
            assert_eq!(progress, before, "the fight does not invent a story moment");
            assert_eq!(gil, 600 + reward.gil);
            earned.push(reward);
            previous_party = Some(carry);
            // Stop after the durable victory but before acknowledging any resumed dialogue.
            // The next SceneStart must contain this victory, never offer the fight again.
            server.shutdown().await;
            let checkpoint = saved(state.path(), account);
            let stored = checkpoint.checkpoint.as_ref().expect("durable resumed scene");
            assert_eq!(stored.consumed_battles, 1);
            assert!(matches!(stored.phase, ScenePhase::Resume { .. }));
            assert_eq!(checkpoint.pair.character.as_ref().unwrap().gil, gil);
            server = boot(state.path(), &opened.adapter, &rules).await;
            peer = login(server.addr, &token, "md8brdg").await;
            driver = Driver::from_start(peer.scene_start(BRIDGE_SCENE_ID).await, &opened.adapter);
            assert_eq!((driver.source, driver.origin), activation);
            assert_eq!((driver.answered, driver.battles), (1, 1));
            assert!(!matches!(driver.play.boundary(), Some(SceneBoundary::Battle(_))), "acknowledged victory cannot repeat");
        }
        loop {
            match driver.advance(&mut peer, &answers).await {
                SceneBoundary::Battle(mark) => {
                    assert!(fight);
                    assert_eq!(mark.ordinal as usize, earned.len() + 1);
                    assert_eq!(mark.formation, 327 + mark.ordinal as u16);
                    driver.ready(&mut peer).await;
                    let (battle_id, setup) = encounter(&mut peer, driver.run, mark.ordinal, &rules).await;
                    assert_eq!(setup.initial_party, previous_party, "consecutive fights retain the exact prior party resources");
                    assert_eq!(setup.back_attack, mark.formation == 330, "the pincer formation retains its authored opening");
                    let (progress, gil, reward, carry) = win(&mut peer, account, &rules, driver.run, mark.ordinal, battle_id, &setup,
                        &mut potions, &mut used).await;
                    earned.push(reward);
                    assert_eq!(gil, 600 + earned.iter().map(|r| r.gil).sum::<u32>());
                    driver.play.resume_battle_checked(BattleResume { checkpoint: mark, resolution: BattleResolution::Victory,
                        story: StoryState::decode(&progress).unwrap(), resources: SceneResources { gil: Some(gil) } }).unwrap();
                    driver.play.next_strict(None).unwrap();
                    driver.battles += 1;
                    previous_party = Some(carry);
                }
                SceneBoundary::End(end) => { assert_eq!(end, End::MapJump { field: 138 }); break; }
                SceneBoundary::Window(_) => unreachable!("advance consumes every window"),
            }
        }
        assert_eq!(driver.battles, if fight { 3 } else { 0 });
        assert!(!fight || used > 0, "the battle path proves a real durable item cost");
        assert_eq!(driver.answered, 3);
        driver.finish(&mut peer).await;
        let mut released = false;
        loop {
            match peer.next().await {
                ServerMsg::SceneEnded { run, outcome } => {
                    assert_eq!((run, outcome), (driver.run, SceneOutcome::Finished));
                    released = true;
                }
                ServerMsg::AreaChange { area, arrival, .. } => {
                    assert!(released, "the committed scene ends before travel");
                    assert_eq!(area.key, "cargoin");
                    let cargo = opened.adapter.load_field("cargoin").unwrap();
                    assert_eq!(arrival, cargo.place_arrival(Pos::new(-24.0, 16.0, 0.0), Some(20)));
                    break; // Deliberately do not acknowledge entry before restarting.
                }
                ServerMsg::SceneBattleStarted { .. } | ServerMsg::EncounterStart { .. } => panic!("a consumed or declined fight replayed"),
                ServerMsg::Reject { code, reason } => panic!("bridge completion refused: {code:?}: {reason}"),
                _ => {}
            }
        }
        server.shutdown().await;
        let landing = saved(state.path(), account);
        let stored = landing.checkpoint.as_ref().expect("landing persists until its arrival scene commits");
        assert!(matches!(stored.phase, ScenePhase::Landing { .. }));
        assert_eq!(landing.pair.story.blob, before, "moment 39 alone cannot identify this landing");
        let expected_gil = 600 + earned.iter().map(|r| r.gil).sum::<u32>();
        assert_eq!(landing.pair.character.as_ref().unwrap().gil, expected_gil);

        server = boot(state.path(), &opened.adapter, &rules).await;
        peer = login(server.addr, &token, "cargoin").await;
        assert_eq!(peer.gil, expected_gil);
        let mut cargo = Driver::from_start(peer.scene_start(CARGO_TRAIN_SCENE_ID).await, &opened.adapter);
        assert_eq!(cargo.advance(&mut peer, &[]).await, SceneBoundary::End(End::Finished));
        assert_eq!((cargo.passed, cargo.answered, cargo.battles), (26, 0, 0));
        cargo.finish(&mut peer).await;
        let mut updated = false;
        loop {
            match peer.next().await {
                ServerMsg::StoryUpdate { progress, .. } if StoryState::decode(&progress).is_some_and(|story| story.moment == 48) => updated = true,
                ServerMsg::SceneEnded { run, outcome } => {
                    assert_eq!((run, outcome), (cargo.run, SceneOutcome::Finished));
                    assert!(updated, "durable moment 48 precedes control release");
                    break;
                }
                ServerMsg::SceneBattleStarted { .. } | ServerMsg::EncounterStart { .. } => panic!("cargo arrival has no battle"),
                ServerMsg::Reject { code, reason } => panic!("cargo completion refused: {code:?}: {reason}"),
                _ => {}
            }
        }
        server.shutdown().await;
        let final_state = saved(state.path(), account);
        assert!(final_state.checkpoint.is_none() && final_state.reservation.is_none());
        assert_eq!(StoryState::decode(&final_state.pair.story.blob).unwrap().moment, 48);
        let character = final_state.pair.character.as_ref().unwrap();
        assert_eq!(character.gil, expected_gil);
        assert_eq!(character.exp, 42 + earned.iter().map(|r| r.exp).sum::<u32>());
        assert_eq!(character.ap, 7 + earned.iter().map(|r| r.ap).sum::<u32>());
        assert_eq!(character.limit, 33, "a player without a limit break keeps its stored gauge");
        let mut inventory = std::collections::BTreeMap::from([(0u16, 3u16)]);
        for reward in &earned {
            for item in &reward.items { *inventory.entry(item.item).or_default() += item.count; }
        }
        *inventory.get_mut(&0).unwrap() -= used;
        assert_eq!(character.inventory, Some(inventory.into_iter().filter(|(_, count)| *count > 0).map(|(item, count)| InventoryEntry { item, count }).collect()),
            "the acknowledged battle's drops are awarded exactly once and existing items survive");
        let bytes = record_bytes(state.path(), account);
        server = boot(state.path(), &opened.adapter, &rules).await;
        peer = login(server.addr, &token, "cargoin").await;
        assert_eq!((peer.progress.clone(), peer.gil), (final_state.pair.story.blob.clone(), expected_gil));
        let cargo_area = opened.adapter.load_field("cargoin").unwrap();
        assert_eq!(peer.pos, cargo_area.place_arrival(Pos::new(-24.0, 16.0, 0.0), Some(20)));
        peer.client.send(&ClientMsg::Ping { nonce: 4848 }).await;
        loop {
            match peer.next().await {
                ServerMsg::Pong { nonce: 4848, .. } => break,
                ServerMsg::SceneStart { .. } | ServerMsg::EncounterStart { .. } => panic!("completed route replays on login"),
                _ => {}
            }
        }
        server.shutdown().await;
        assert_eq!(saved(state.path(), account), final_state, "relogin preserves exact resources and consumed state");
        assert_eq!(record_bytes(state.path(), account), bytes, "relogin does not rewrite receipts or pay again");
        eprintln!("bridge TCP route: fights={}, Potions used={used}, all checkpoints recovered, cargo moment48 durable", driver.battles);
    }
}
