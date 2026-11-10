//! Synthetic continuation lifecycle through the real store, with saver acknowledgements held
//! explicitly. No game files, sockets, sleep, or live account data are involved.

mod door_coop;
mod retire_landing;
mod size_bounds;

use std::sync::Arc;
use std::time::Duration;

use fmv_adapter_api::GameAdapter;
use fmv_core::{AvatarAppearance, GameId, InventoryEntry};
use fmv_fixture::rooms::{RoomsAdapter, HALL, YARD};
use fmv_party::PartyConfig;
use fmv_persist::{Progression, Store};
use fmv_protocol::{ClientMsg, SyntheticRules};
use fmv_protocol::battle::{ActorId, Command, Target};
use tokio::sync::{mpsc, watch};

use super::*;
use crate::config::BattleSettings;
use crate::progress::{self, Progress, ProgressSeed};
use crate::saver::{persist_scene_batch, Msg, SaverHandle};
use crate::world::{BattleReply, NewPlayer, Outbound};
use crate::world::scene::{SceneHost, SceneInfo, SceneSettings, SceneTransaction, TriggerInfo};

const SCENE: &str = "fixture/durable-bridge";
const SOURCE: [u8; 32] = [17; 32];
const CARGO: &str = "fixture/automatic-cargo";
const CARGO_SOURCE: [u8; 32] = [18; 32];
const ORIGINAL: &[u8] = &[39];
const FINAL: &[u8] = &[40];

/// Two windows with a single authoritative combat boundary between them. The adapter rejects
/// anything other than the exact accepted prefix, including a fabricated victory/choice.
struct Host { formation: u16, automatic: bool }

impl SceneHost for Host {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![
            SceneInfo { id: SCENE.into(), field: HALL.into(), arrival: Pos::default(), arrival_facing: 0.0, arrival_hint: None },
            SceneInfo { id: CARGO.into(), field: YARD.into(), arrival: Pos::new(150.0, -100.0, 0.0), arrival_facing: 0.0, arrival_hint: None },
        ]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { (scene == SCENE && progress == ORIGINAL) || (scene == CARGO && progress == FINAL) }
    fn automatic(&self, scene: &str) -> bool { (scene == SCENE && self.automatic) || scene == CARGO }
    fn continuation(&self, scene: &str) -> bool { scene == SCENE || scene == CARGO }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { panic!("continuations never use the legacy completion path") }
    fn evaluate_continuation(&self, scene: &str, original: &[u8], _: u32, gil: u32, replay: Option<&SceneReplay>) -> Option<SceneBoundary> {
        if scene == CARGO {
            if original != FINAL || replay.is_some_and(|r| r.source != CARGO_SOURCE || r.validate().is_err() || !r.events.is_empty()) { return None; }
            return Some(SceneBoundary { source: CARGO_SOURCE, progress: FINAL.to_vec(), gil,
                at: SceneBoundaryKind::Window { step: 0, identity: vec![30], choices: None } });
        }
        if scene != SCENE || original != ORIGINAL || replay.is_some_and(|r| r.source != SOURCE || r.validate().is_err()) { return None; }
        let events = replay.map_or(&[][..], |r| r.events.as_slice());
        let window = |step, choices| SceneBoundaryKind::Window { step, identity: vec![10 + step as u8], choices };
        let valid_first = |event: &SceneReplayEvent| matches!(event,
            SceneReplayEvent::Window { step: 0, identity, answer: Some(2) } if identity == &[10]);
        let valid_battle = |event: &SceneReplayEvent| matches!(event,
            SceneReplayEvent::Battle { ordinal: 1, identity, formation, progress, .. }
                if identity == &[20] && *formation == self.formation && progress == ORIGINAL);
        let current_gil = events.iter().rev().find_map(|event| match event { SceneReplayEvent::Battle { gil, .. } => Some(*gil), _ => None }).unwrap_or(gil);
        let at = match events {
            [] => window(0, Some([2, 3])),
            [first] if valid_first(first) => SceneBoundaryKind::Battle { ordinal: 1, identity: vec![20], formation: self.formation, flags: Some(8) },
            [first, battle] if valid_first(first) && valid_battle(battle) => window(1, None),
            [first, battle, SceneReplayEvent::Window { step: 1, identity, answer: None }]
                if valid_first(first) && valid_battle(battle) && identity == &[11] => SceneBoundaryKind::Complete(SceneResult {
                    progress: FINAL.to_vec(), changed: true, summary: "synthetic bridge completed".into(),
                    travel: Some(Exit { line: (Pos::default(), Pos::default()), target: YARD.into(),
                        arrival: Pos::new(150.0, -100.0, 0.0), arrival_facing: Some(0.0), arrival_hint: None, kind: ExitKind::Gateway }),
                    transaction: Some(SceneTransaction { expected_gil: gil, next_gil: current_gil }),
                }),
            _ => return None,
        };
        Some(SceneBoundary { source: SOURCE, progress: ORIGINAL.to_vec(), gil: current_gil, at })
    }
}

const DOOR: &str = "fixture/zero-window-door";
const ARRIVAL: &str = "fixture/linked-arrival";
const DOOR_STORY: &[u8] = &[48];
const ARRIVED_STORY: &[u8] = &[51];
const DOOR_SOURCE: [u8; 32] = [31; 32];
const ARRIVAL_SOURCE: [u8; 32] = [32; 32];

/// A door with no dialogue or story write, followed by a two-window arrival in the destination.
/// Its final control position deliberately differs from the map-jump landing in that same field.
struct DoorHost { final_field: &'static str }

impl DoorHost {
    fn landing() -> Exit {
        Exit { line: (Pos::default(), Pos::default()), target: YARD.into(),
            arrival: Pos::new(150.0, -100.0, 0.0), arrival_facing: Some(1.5), arrival_hint: Some(29), kind: ExitKind::Gateway }
    }
    fn control(&self) -> Exit {
        Exit { line: (Pos::default(), Pos::default()), target: self.final_field.into(),
            arrival: Pos::new(80.0, 120.0, 0.0), arrival_facing: Some(0.25), arrival_hint: Some(28), kind: ExitKind::Gateway }
    }
}

impl SceneHost for DoorHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![
            SceneInfo { id: DOOR.into(), field: HALL.into(), arrival: Pos::default(), arrival_facing: 0.0, arrival_hint: None },
            SceneInfo { id: ARRIVAL.into(), field: YARD.into(), arrival: Self::landing().arrival, arrival_facing: 1.5, arrival_hint: Some(29) },
        ]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { (scene == DOOR || scene == ARRIVAL) && progress == DOOR_STORY }
    fn automatic(&self, scene: &str) -> bool { scene == ARRIVAL }
    fn requires_landing(&self, scene: &str) -> bool { scene == ARRIVAL }
    fn continuation(&self, scene: &str) -> bool { scene == DOOR || scene == ARRIVAL }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn entry(&self, progress: &[u8]) -> Option<Exit> { (progress == ARRIVED_STORY).then(|| self.control()) }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { panic!("the door and arrival require durable completion") }
    fn evaluate_continuation(&self, scene: &str, original: &[u8], _: u32, gil: u32, replay: Option<&SceneReplay>) -> Option<SceneBoundary> {
        let source = match scene { DOOR => DOOR_SOURCE, ARRIVAL => ARRIVAL_SOURCE, _ => return None };
        if original != DOOR_STORY || replay.is_some_and(|r| r.source != source || r.validate().is_err()) { return None; }
        let events = replay.map_or(&[][..], |r| r.events.as_slice());
        let complete = |progress: &[u8], travel| SceneBoundaryKind::Complete(SceneResult {
            progress: progress.to_vec(), changed: progress != original, summary: "synthetic door/arrival".into(), travel,
            transaction: Some(SceneTransaction { expected_gil: gil, next_gil: gil }),
        });
        let window = |step| SceneBoundaryKind::Window { step, identity: vec![40 + step as u8], choices: None };
        let accepted = |event: &SceneReplayEvent, expected| matches!(event,
            SceneReplayEvent::Window { step, identity, answer: None } if *step == expected && identity == &[40 + expected as u8]);
        let (progress, at) = match (scene, events) {
            (DOOR, []) => (DOOR_STORY, complete(DOOR_STORY, Some(Self::landing()))),
            (ARRIVAL, []) => (DOOR_STORY, window(0)),
            (ARRIVAL, [first]) if accepted(first, 0) => (DOOR_STORY, window(1)),
            (ARRIVAL, [first, second]) if accepted(first, 0) && accepted(second, 1) => (ARRIVED_STORY, complete(ARRIVED_STORY, None)),
            _ => return None,
        };
        Some(SceneBoundary { source, progress: progress.to_vec(), gil, at })
    }
}

struct Seat { account: AccountId, entity: EntityId, rx: mpsc::Receiver<Outbound> }

impl Seat {
    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut messages = vec![];
        while let Ok(message) = self.rx.try_recv() { messages.push((*message).clone()); }
        messages
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Store,
    world: World,
    saves: mpsc::UnboundedReceiver<Msg>,
    rules: SyntheticRules,
    formation: u16,
    owner: Seat,
    now: Instant,
    run: SceneRunId,
}

fn world(rules: SyntheticRules, formation: u16) -> (World, mpsc::UnboundedReceiver<Msg>) {
    world_at(rules, formation, HALL)
}

fn world_at(rules: SyntheticRules, formation: u16, home: &str) -> (World, mpsc::UnboundedReceiver<Msg>) {
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::new());
    let mut world = World::with_party_config(adapter.load_area(home).unwrap(), PartyConfig::default()).unwrap();
    world.set_area_source(adapter);
    world.configure_battle(Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000,
        fixed_seed: Some(7), ..BattleSettings::new(Arc::new(rules)) }), true);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(Host { formation, automatic: false }), debug_preview: false, debug_new_story: None }));
    let (saver, saves) = SaverHandle::channel();
    world.set_progress_saver(saver);
    (world, saves)
}

fn join(world: &mut World, state: SceneState, group: Vec<SceneState>, now: Instant) -> Seat {
    admit(world, state, group, now).expect("coherent account joins")
}

fn admit(world: &mut World, state: SceneState, group: Vec<SceneState>, now: Instant) -> Result<Seat, crate::world::JoinError> {
    let account = state.pair.story.account_id;
    let character = state.pair.character.as_ref().unwrap();
    let (session, entity) = world.allocate_ids();
    let (tx, rx) = mpsc::channel(512);
    world.join(NewPlayer { session, entity, account,
        avatar: AvatarAppearance { display_name: "Synthetic owner".into(), tint: [1, 2, 3], appearance_ref: None },
        progression: state.pair.story.blob.clone(),
        progress: ProgressSeed { character: character.character, stored: Some(Progress::from_record(character)), inventory: character.inventory.clone() },
        scene_state: Some(state), scene_group: group, tx, close: Arc::new(watch::channel(false).0),
    }, now)?;
    Ok(Seat { account, entity, rx })
}

impl Rig {
    fn new(formation: u16) -> Self {
        let rules = SyntheticRules { enemy_hp: 100, ..SyntheticRules::default() };
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let (account, _) = store.create_account("Synthetic owner", 1).unwrap();
        store.put_progression(&Progression::new(account, GameId::Fixture, ORIGINAL.to_vec(), 1)).unwrap();
        let mut character = progress::fresh(&rules, 0).to_record(account, GameId::Fixture, 1);
        character.gil = 240;
        character.inventory = Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(20, 2)]);
        store.put_character_progress(&character).unwrap();
        let (mut world, saves) = world(rules, formation);
        let now = Instant::now();
        let state = store.get_scene_state(account, GameId::Fixture).unwrap();
        let mut owner = join(&mut world, state.clone(), vec![state], now);
        owner.drain();
        let run = SceneRunId(71);
        world.start_run(run, SceneRun { scene: SCENE.into(), owner: owner.entity, watchers: vec![], seed: 19,
            progress: ORIGINAL.to_vec(), gil: 240, answers: vec![], steps: 0, started: now, last_activity: now,
            writes_story: true, preview: false, then: None }, &mut vec![]);
        assert!(owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneStart { replay: Some(_), .. })));
        Self { _dir: dir, store, world, saves, rules, formation, owner, now, run }
    }

    fn send(&mut self, message: ClientMsg) { self.world.scene_message(self.owner.entity, message, self.now); }
    fn tick(&mut self) { self.world.advance_scenes(self.now); }
    fn stored(&self) -> SceneState { self.store.get_scene_state(self.owner.account, GameId::Fixture).unwrap() }

    fn request_battle(&mut self) {
        self.send(ClientMsg::SceneStep { run: self.run, step: 0, answer: Some(2) });
        self.send(ClientMsg::SceneBattleReady { run: self.run, ordinal: 1, step: 1 });
    }

    fn snapshot(&mut self) {
        let Msg::SceneSnapshot { accounts, done } = self.saves.try_recv().expect("ordered snapshot requested") else { panic!("expected snapshot, not an ordinary save") };
        let states = self.store.get_scene_states(&accounts, GameId::Fixture).unwrap();
        done.send(Ok(states)).unwrap();
        self.tick();
    }

    fn pending_batch(&mut self) -> (SceneBatchJob, oneshot::Sender<Result<SceneBatchCommitted, SceneSaveError>>) {
        match self.saves.try_recv().expect("durable batch requested") {
            Msg::SceneBatch { job, done } => (job, done),
            _ => panic!("continuation attempted an ordinary reward/story write"),
        }
    }

    fn apply(&self, job: &SceneBatchJob) -> SceneBatchCommitted {
        persist_scene_batch(&self.store, GameId::Fixture, Some(&self.rules), job, 10).expect("actual durable batch commits")
    }

    fn ack(&mut self, done: oneshot::Sender<Result<SceneBatchCommitted, SceneSaveError>>, saved: SceneBatchCommitted) {
        done.send(Ok(saved)).unwrap();
        self.tick();
    }

    fn prepare(&mut self) -> BattleId {
        self.request_battle();
        self.snapshot();
        let (job, done) = self.pending_batch();
        assert_eq!(job.operation.kind, SceneOperationKind::PrepareBattle);
        let saved = self.apply(&job);
        self.ack(done, saved);
        self.world.players[&self.owner.entity].battle.expect("acknowledged checkpoint starts combat")
    }

    fn win(&mut self, battle: BattleId) {
        self.win_without_scene_tick(battle);
        self.tick();
    }

    fn win_without_scene_tick(&mut self, battle: BattleId) {
        for _ in 0..1000 {
            self.now += Duration::from_millis(10);
            let reply = self.world.battle_command(self.owner.entity, battle, ActorId(0),
                Command::Item { item: 20, target: Target::One(ActorId(1)) }, self.now);
            if matches!(reply, BattleReply::Done) { return; }
            self.owner.drain();
        }
        panic!("synthetic actor never became ready to use its winning item");
    }
}

fn door_world(rules: SyntheticRules, home: &str, final_field: &'static str) -> (World, mpsc::UnboundedReceiver<Msg>) {
    let (mut world, saves) = world_at(rules, 3, home);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(DoorHost { final_field }), debug_preview: false, debug_new_story: None }));
    (world, saves)
}

fn door_rig(final_field: &'static str) -> Rig {
    let rules = SyntheticRules::default();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    let (account, _) = store.create_account("Door owner", 1).unwrap();
    store.put_progression(&Progression::new(account, GameId::Fixture, DOOR_STORY.to_vec(), 1)).unwrap();
    let mut character = progress::fresh(&rules, 0).to_record(account, GameId::Fixture, 1);
    character.gil = 240;
    character.inventory = Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(20, 2)]);
    store.put_character_progress(&character).unwrap();
    let (mut world, saves) = door_world(rules, HALL, final_field);
    let now = Instant::now();
    let state = store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut owner = join(&mut world, state.clone(), vec![state], now);
    owner.drain();
    let run = SceneRunId(71);
    world.start_run(run, SceneRun { scene: DOOR.into(), owner: owner.entity, watchers: vec![], seed: 19,
        progress: DOOR_STORY.to_vec(), gil: 240, answers: vec![], steps: 0, started: now, last_activity: now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    assert!(owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, replay: Some(_), .. } if scene == DOOR)));
    Rig { _dir: dir, store, world, saves, rules, formation: 3, owner, now, run }
}

/// Drop both process-side runtime and the store lock, reopen the exact durable files, and admit
/// from their coherent group. The new world deliberately may have a different home field.
fn restart_door(rig: Rig, home: &str, final_field: &'static str) -> Rig {
    let Rig { _dir, store, world, saves, rules, formation, owner, now, .. } = rig;
    let account = owner.account;
    let root = store.root().to_path_buf();
    drop(world);
    drop(saves);
    drop(owner);
    drop(store);
    let store = Store::open(root).unwrap();
    let (mut world, saves) = door_world(rules, home, final_field);
    let group = store.get_scene_group(account, GameId::Fixture).unwrap();
    let state = group.iter().find(|state| state.pair.story.account_id == account).unwrap().clone();
    let owner = join(&mut world, state, group, now);
    let run = world.players[&owner.entity].scene.unwrap_or(SceneRunId(0));
    Rig { _dir, store, world, saves, rules, formation, owner, now, run }
}

fn enter_door_destination(rig: &mut Rig) -> Vec<ServerMsg> {
    let messages = rig.owner.drain();
    let offer = messages.iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).cloned().expect("the retained destination is offered");
    let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
    assert_eq!(rig.world.join_area(rig.owner.entity, &area, &fingerprint, &provenance, rig.now), crate::world::TravelReply::Done);
    messages
}

fn door_resources(state: &SceneState) -> (Progress, Option<Vec<InventoryEntry>>) {
    let record = state.pair.character.as_ref().unwrap();
    (Progress::from_record(record), record.inventory.clone())
}

fn request_arrival_complete(rig: &mut Rig) {
    rig.send(ClientMsg::SceneStep { run: rig.run, step: 0, answer: None });
    rig.send(ClientMsg::SceneStep { run: rig.run, step: 1, answer: None });
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
}

#[test]
fn a_zero_window_door_and_linked_arrival_publish_only_after_each_ack_and_release_at_final_control() {
    let mut rig = door_rig(YARD);
    let resources = door_resources(&rig.stored());
    let initial_position = rig.world.players[&rig.owner.entity].snapshot.pos;
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    assert_eq!(finish.operation.kind, SceneOperationKind::Finish);
    assert_eq!(finish.members[0].next_story.as_deref(), Some(DOOR_STORY), "zero-window travel keeps moment 48");
    assert!(finish.members.iter().all(|member| member.battle.is_none()));
    let landing = finish.members[0].checkpoint.as_ref().unwrap();
    let payload: Payload = serde_json::from_slice(&landing.replay).unwrap();
    assert!(payload.replay.events.is_empty(), "the door fabricated no dialogue or battle");
    assert_eq!(landing.consumed_battles, 0);
    assert_eq!(payload.landing, Some(Landing::from_exit(&DoorHost::landing())));
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. } | ServerMsg::AreaChange { .. })));
    let landed = rig.apply(&finish);
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.pos, initial_position);
    assert!(rig.world.players[&rig.owner.entity].transit.is_none());
    assert_eq!(rig.stored().pair.story.blob, DOOR_STORY);
    rig.ack(done, landed);
    let released_door = enter_door_destination(&mut rig);
    assert_eq!(released_door.iter().filter(|message| matches!(message, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })).count(), 1);
    let (arrival, done) = rig.pending_batch();
    assert_eq!(arrival.operation.kind, SceneOperationKind::ContinueLanding);
    let checkpoint = arrival.members[0].checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.scene_key, ARRIVAL);
    assert_ne!(checkpoint.instance, landing.instance);
    assert!(matches!(checkpoint.phase, ScenePhase::Resume { segment: 0 }));
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, .. } if scene == ARRIVAL)));
    let arrived = rig.apply(&arrival);
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.pos, DoorHost::landing().arrival);
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, .. } if scene == ARRIVAL)));
    rig.ack(done, arrived);
    assert!(rig.owner.drain().iter().any(|message| matches!(message,
        ServerMsg::SceneStart { scene, progress, replay: Some(replay), .. }
            if scene == ARRIVAL && progress == DOOR_STORY && replay.origin.is_none() && replay.events.is_empty())));
    request_arrival_complete(&mut rig);
    let (complete, done) = rig.pending_batch();
    assert_eq!(complete.operation.kind, SceneOperationKind::Complete);
    assert_eq!(complete.members[0].next_story.as_deref(), Some(ARRIVED_STORY));
    assert!(complete.members[0].checkpoint.is_none() && complete.members[0].reservation.is_none());
    assert_eq!(rig.stored().pair.story.blob, DOOR_STORY);
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. } | ServerMsg::AreaChange { .. })));
    let completed = rig.apply(&complete);
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.pos, DoorHost::landing().arrival);
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.facing, DoorHost::landing().arrival_facing.unwrap());
    assert_eq!(rig.world.players[&rig.owner.entity].progression, DOOR_STORY);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. })));
    rig.ack(done, completed.clone());
    let control = DoorHost { final_field: YARD }.control();
    let messages = rig.owner.drain();
    let snapshot = messages.iter().position(|message| matches!(message,
        ServerMsg::AreaSnapshot { area, entities, .. } if area.key == YARD && entities.iter().any(|entity|
            entity.id == rig.owner.entity && entity.pos == control.arrival && Some(entity.facing) == control.arrival_facing)))
        .expect("the acknowledged final pose is sent in the local snapshot");
    let released = messages.iter().position(|message| matches!(message, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })).unwrap();
    assert!(snapshot < released, "control returns only after the final position is authoritative on the client");
    assert_eq!(messages.iter().filter(|message| matches!(message, ServerMsg::SceneEnded { .. })).count(), 1);
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::AreaChange { .. } | ServerMsg::EncounterStart { .. })));
    let player = &rig.world.players[&rig.owner.entity];
    assert_eq!((player.snapshot.pos, player.snapshot.facing, player.scene), (control.arrival, control.arrival_facing.unwrap(), None));
    assert_eq!(player.progression, ARRIVED_STORY);
    assert_eq!(door_resources(&rig.stored()), resources);
    let duplicate = rig.apply(&complete);
    assert_eq!(duplicate.commit, fmv_persist::SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, completed.current);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.tick();
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. } | ServerMsg::AreaChange { .. })));
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn door_landing_arrival_resume_and_completed_control_survive_restart_at_unacknowledged_boundaries() {
    let mut rig = door_rig(YARD);
    let resources = door_resources(&rig.stored());
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, pending_finish) = rig.pending_batch();
    rig.apply(&finish);
    let landing = rig.stored();
    assert!(matches!(landing.checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    assert_eq!(landing.pair.story.blob, DOOR_STORY);
    rig = restart_door(rig, HALL, YARD);
    drop(pending_finish);
    assert_eq!(rig.stored(), landing);
    assert_eq!(rig.world.transit_target(rig.owner.entity).as_deref(), Some(YARD), "moment 48 alone cannot replace the retained destination");
    enter_door_destination(&mut rig);
    let (arrival, pending_arrival) = rig.pending_batch();
    assert_eq!(arrival.operation.kind, SceneOperationKind::ContinueLanding);
    rig.apply(&arrival);
    let resume = rig.stored();
    let checkpoint = resume.checkpoint.as_ref().unwrap();
    assert!(matches!(checkpoint.phase, ScenePhase::Resume { segment: 0 }));
    assert_eq!(checkpoint.scene_key, ARRIVAL);
    assert_ne!(checkpoint.instance, landing.checkpoint.unwrap().instance);
    rig = restart_door(rig, YARD, YARD);
    drop(pending_arrival);
    let messages = rig.owner.drain();
    assert!(messages.iter().any(|message| matches!(message,
        ServerMsg::SceneStart { scene, progress, replay: Some(replay), .. }
            if scene == ARRIVAL && progress == DOOR_STORY && replay.origin.is_none() && replay.events.is_empty())));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::AreaChange { .. } | ServerMsg::EncounterStart { .. })));
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.pos, DoorHost::landing().arrival);
    assert_eq!(rig.stored(), resume);
    assert!(rig.saves.try_recv().is_err(), "a committed linked instance is recovered without another ContinueLanding");
    request_arrival_complete(&mut rig);
    let (complete, pending_complete) = rig.pending_batch();
    rig.apply(&complete);
    let completed = rig.stored();
    assert_eq!(completed.pair.story.blob, ARRIVED_STORY);
    assert!(completed.checkpoint.is_none() && completed.reservation.is_none());
    assert_eq!(rig.world.players[&rig.owner.entity].snapshot.pos, DoorHost::landing().arrival);
    assert!(rig.world.leave(rig.owner.entity), "disconnect does not undo the completed durable image");
    rig = restart_door(rig, YARD, YARD);
    drop(pending_complete);
    let control = DoorHost { final_field: YARD }.control();
    let player = &rig.world.players[&rig.owner.entity];
    assert_eq!((player.area.as_str(), player.snapshot.pos, player.snapshot.facing, player.scene),
        (YARD, control.arrival, control.arrival_facing.unwrap(), None), "checkpoint-free login uses final control, even when home is the same field");
    let messages = rig.owner.drain();
    let first_snapshot = messages.iter().find_map(|message| match message {
        ServerMsg::AreaSnapshot { entities, .. } => Some(entities), _ => None,
    }).unwrap();
    assert!(first_snapshot.iter().any(|entity| entity.id == rig.owner.entity && entity.pos == control.arrival && Some(entity.facing) == control.arrival_facing));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneStart { .. } | ServerMsg::AreaChange { .. } | ServerMsg::EncounterStart { .. })));
    assert_eq!(rig.stored(), completed);
    assert_eq!(door_resources(&rig.stored()), resources);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn arrival_complete_cannot_change_fields_or_clear_its_retained_landing() {
    let mut rig = door_rig(HALL);
    let resources = door_resources(&rig.stored());
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    let saved = rig.apply(&finish);
    rig.ack(done, saved);
    enter_door_destination(&mut rig);
    let (arrival, done) = rig.pending_batch();
    let saved = rig.apply(&arrival);
    rig.ack(done, saved);
    rig.owner.drain();
    let retained = rig.stored();
    request_arrival_complete(&mut rig);
    assert_eq!(rig.stored(), retained, "an unsupported cross-field final entry is rejected before Complete writes");
    assert_eq!(door_resources(&rig.stored()), resources);
    assert!(rig.saves.try_recv().is_err());
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. } | ServerMsg::AreaChange { .. })));
    assert!(rig.world.players.get(&rig.owner.entity).is_none_or(|player| player.scene.is_some()), "a rejected completion cannot release free control");
}

#[test]
fn an_arrival_without_its_required_landing_is_not_started_or_held() {
    let mut rig = door_rig(YARD);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    let original = rig.stored();
    assert!(original.checkpoint.is_none() && original.reservation.is_none());
    rig = restart_door(rig, YARD, YARD);
    let messages = rig.owner.drain();
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, .. } if scene == ARRIVAL)),
        "moment 48 in the field is not a durable arrival lineage");
    assert!(rig.world.players[&rig.owner.entity].scene.is_none());
    assert_eq!(rig.world.scene_count(), 0);
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.stored(), original);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn combat_and_resume_wait_for_their_own_durable_acknowledgements_and_landing_is_retained() {
    let mut rig = Rig::new(3);
    let original = rig.stored();
    rig.request_battle();
    assert_eq!(rig.stored(), original);
    assert!(rig.world.players[&rig.owner.entity].battle.is_none());
    rig.snapshot();
    let (prepare, done) = rig.pending_batch();
    assert_eq!((prepare.operation.ordinal, prepare.operation.attempt), (0, 0), "disk ordinals start at zero");
    let prepared = rig.apply(&prepare);
    assert!(rig.world.players[&rig.owner.entity].battle.is_none(), "a committed but unacknowledged prepare cannot launch");
    rig.ack(done, prepared);
    let battle = rig.world.players[&rig.owner.entity].battle.unwrap();
    let messages = rig.owner.drain();
    let started = messages.iter().position(|m| matches!(m, ServerMsg::SceneBattleStarted { ordinal: 1, .. })).unwrap();
    let encounter = messages.iter().position(|m| matches!(m, ServerMsg::EncounterStart { .. })).unwrap();
    assert!(started < encounter, "scene must bind the battle before its engine arrives");
    rig.win(battle);
    let (resolve, done) = rig.pending_batch();
    assert_eq!(resolve.operation.kind, SceneOperationKind::ResolveBattle);
    assert_eq!(resolve.members[0].battle.as_ref().unwrap().consumed_items, [20]);
    assert_eq!(rig.stored().pair.character.as_ref().unwrap().gil, 240);
    assert_eq!(rig.world.players[&rig.owner.entity].progress.unwrap().gil, 240);
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::BattleEnd { .. } | ServerMsg::SceneBattleResumed { .. })));
    let resolved = rig.apply(&resolve);
    assert_eq!(rig.world.players[&rig.owner.entity].battle, Some(battle), "settlement ack, not disk visibility alone, releases combat");
    rig.ack(done, resolved.clone());
    let messages = rig.owner.drain();
    let end = messages.iter().position(|m| matches!(m, ServerMsg::BattleEnd { .. })).unwrap();
    let resumed = messages.iter().position(|m| matches!(m, ServerMsg::SceneBattleResumed { ordinal: 1, .. })).unwrap();
    assert!(end < resumed);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert_eq!(rig.stored().checkpoint.as_ref().unwrap().consumed_battles, 1);
    assert!(rig.stored().reservation.is_some(), "the next scene segment still owns its roster");
    let duplicate = rig.apply(&resolve);
    assert_eq!(duplicate.commit, fmv_persist::SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, resolved.current, "retry cannot pay or consume twice");
    rig.send(ClientMsg::SceneStep { run: rig.run, step: 1, answer: None });
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    assert_eq!(finish.operation.kind, SceneOperationKind::Finish);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. } | ServerMsg::SceneEnded { .. })));
    rig.world.set_unload_grace(Duration::ZERO);
    rig.world.manage_areas(rig.now);
    assert!(rig.world.areas.contains_key(YARD), "the selected landing stays loaded while its durable acknowledgement is pending");
    let landed = rig.apply(&finish);
    rig.ack(done, landed);
    let stored = rig.stored();
    assert_eq!(stored.pair.story.blob, FINAL);
    assert!(stored.reservation.is_none());
    assert!(matches!(stored.checkpoint.unwrap().phase, ScenePhase::Landing { .. }), "disconnect before area-ready must retain its destination");
    assert!(rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn invalid_or_missing_choice_cannot_start_or_save_a_battle_and_skip_is_not_completion() {
    for answer in [None, Some(0), Some(4)] {
        let mut rig = Rig::new(3);
        let before = rig.stored();
        rig.send(ClientMsg::SceneStep { run: rig.run, step: 0, answer });
        assert_eq!(rig.stored(), before);
        assert!(rig.saves.try_recv().is_err());
        assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::EncounterStart { .. } | ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
    }
    let mut rig = Rig::new(3);
    let before = rig.stored();
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    assert_eq!(rig.stored(), before);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, None);
    assert!(rig.saves.try_recv().is_err());
    assert!(rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Aborted, .. })));
}

#[test]
fn refused_settlement_never_publishes_success_or_releases_reserved_progress() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    let prepared = rig.stored();
    rig.owner.drain();
    rig.win(battle);
    let (_, done) = rig.pending_batch();
    done.send(Err(SceneSaveError::WorkerFailed)).unwrap();
    rig.tick();
    assert_eq!(rig.stored(), prepared);
    assert!(rig.world.scenes.storage_failed);
    assert!(!rig.world.players.contains_key(&rig.owner.entity));
    assert!(!rig.owner.drain().iter().any(|m| matches!(m,
        ServerMsg::BattleEnd { .. } | ServerMsg::SceneBattleResumed { .. } | ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
}

#[test]
fn disconnected_owner_does_not_cancel_a_queued_prepare_and_relogin_recovers_the_exact_checkpoint() {
    let mut rig = Rig::new(3);
    rig.request_battle();
    rig.snapshot();
    let (prepare, done) = rig.pending_batch();
    let frozen = prepare.members[0].checkpoint.clone().unwrap();
    assert!(rig.world.leave(rig.owner.entity));
    let saved = rig.apply(&prepare);
    rig.ack(done, saved);
    assert_eq!(rig.stored().checkpoint, Some(frozen.clone()));
    let (mut replacement, saves) = world(rig.rules, rig.formation);
    let group = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    let mut returned = join(&mut replacement, group[0].clone(), group, rig.now);
    let messages = returned.drain();
    assert!(replacement.players[&returned.entity].scene.is_some(), "recovery holds before ordinary play");
    assert_eq!(rig.stored().checkpoint, Some(frozen));
    rig.world = replacement;
    rig.saves = saves;
    rig.owner = returned;
    assert!(!messages.iter().any(|m| matches!(m, ServerMsg::SceneBattleResumed { .. } | ServerMsg::BattleEnd { .. })), "a prepared fight is not a victory");
}

#[test]
fn reconnect_between_battle_removal_and_dormant_handoff_waits_without_halting_storage() {
    let mut rig = Rig::new(3);
    rig.prepare();
    let checkpoint = rig.stored();
    assert!(rig.world.leave(rig.owner.entity));
    // The battle loop has removed its engine, but the scene loop has not consumed the dormant
    // notification. Admission must not try to attach a new session to that absent engine.
    assert!(matches!(rig.world.scenes.continuations[&rig.run].phase, Phase::Fighting { .. }));
    assert!(matches!(admit(&mut rig.world, checkpoint.clone(), vec![checkpoint.clone()], rig.now),
        Err(crate::world::JoinError::ProgressChanged)));
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.world.scene_count(), 1);
    rig.tick();
    assert!(matches!(rig.world.scenes.continuations[&rig.run].phase, Phase::Dormant));
    let mut returned = join(&mut rig.world, checkpoint.clone(), vec![checkpoint.clone()], rig.now);
    assert!(rig.world.players[&returned.entity].battle.is_some(), "the completed handoff reconstructs the prepared fight");
    assert!(returned.drain().iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.stored(), checkpoint);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn reconnect_after_winning_command_before_scene_tick_waits_for_the_settlement_handoff() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    let prepared = rig.stored();
    rig.win_without_scene_tick(battle);
    assert!(matches!(rig.world.scenes.continuations[&rig.run].phase, Phase::Fighting { .. }));
    assert!(!rig.world.account_in_battle(rig.owner.account), "the winning command has already removed the engine");
    assert!(rig.saves.try_recv().is_err(), "the scene loop has not selected its settlement yet");
    assert!(rig.world.leave(rig.owner.entity));
    assert!(matches!(admit(&mut rig.world, prepared.clone(), vec![prepared.clone()], rig.now),
        Err(crate::world::JoinError::ProgressChanged)), "the finished engine's absence cannot reopen its prepared battle");
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.stored(), prepared);
    rig.tick();
    let (resolve, done) = rig.pending_batch();
    assert_eq!(resolve.operation.kind, SceneOperationKind::ResolveBattle);
    let committed = rig.apply(&resolve);
    rig.ack(done, committed);
    let resolved = rig.stored();
    let mut returned = join(&mut rig.world, resolved.clone(), vec![resolved.clone()], rig.now);
    let messages = returned.drain();
    assert!(messages.iter().any(|message| matches!(message,
        ServerMsg::SceneStart { replay: Some(replay), .. }
            if replay.events.iter().any(|event| matches!(event, SceneReplayEvent::Battle { ordinal: 1, .. })))));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    assert!(!rig.world.scenes.storage_failed);
    assert_eq!(rig.stored(), resolved);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn disconnect_before_initial_snapshot_response_preserves_its_frozen_preconditions() {
    let mut rig = Rig::new(3);
    let before = rig.stored();
    rig.request_battle();
    assert!(rig.world.leave(rig.owner.entity));
    assert!(rig.world.players.is_empty());
    assert!(rig.world.scene_save_pending(rig.owner.account), "the queued snapshot still owns its account");
    rig.snapshot();
    assert!(!rig.world.scenes.storage_failed, "disconnect does not invalidate the frozen resource mirrors");
    let (prepare, done) = rig.pending_batch();
    assert_eq!(prepare.operation.kind, SceneOperationKind::PrepareBattle);
    assert_eq!(prepare.members[0].expected, before);
    let committed = rig.apply(&prepare);
    rig.ack(done, committed);
    let after = rig.stored();
    assert!(!rig.world.scenes.storage_failed);
    assert!(after.reservation.is_some());
    assert!(matches!(after.checkpoint.as_ref().unwrap().phase, ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, .. }));
    assert!(matches!(rig.world.scenes.continuations[&rig.run].phase, Phase::Dormant),
        "a disconnected cohort retains its durable checkpoint without starting combat");
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    rig.tick();
    assert_eq!(rig.stored(), after);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn an_automatic_continuation_keeps_its_authored_initial_placement() {
    let mut rig = Rig::new(3);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    let before = rig.stored();
    rig.world.configure_scenes(Some(SceneSettings {
        host: Arc::new(Host { formation: rig.formation, automatic: true }),
        debug_preview: false, debug_new_story: None,
    }));
    rig.world.automatic_scene_entered(rig.owner.entity, rig.now);
    let messages = rig.owner.drain();
    let replay = messages.iter().find_map(|message| match message {
        ServerMsg::SceneStart { scene, replay: Some(replay), .. } if scene == SCENE => Some(replay),
        _ => None,
    }).expect("the automatic continuation starts through the normal entry path");
    assert!(replay.origin.is_none(), "an automatic scene uses its authored cast placement, not the player's login position");
    assert!(rig.world.players[&rig.owner.entity].scene.is_some());
    assert_eq!(rig.stored(), before);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn battle_and_pending_commit_ignore_skip_instead_of_releasing_the_continuation() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    let prepared = rig.stored();
    rig.owner.drain();
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    assert_eq!(rig.world.players[&rig.owner.entity].battle, Some(battle));
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert_eq!(rig.stored(), prepared);
    rig.win(battle);
    let (_, _pending) = rig.pending_batch();
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { .. })));
}

#[test]
fn defeat_rolls_back_staged_item_cost_and_restart_requires_an_acknowledged_explicit_retry() {
    let mut rig = Rig::new(2);
    // Formation 2 can kill this level-5 player in one hit. Seed 1 gives the player
    // its first turn before that enemy (the same setup used by the battle refusal test).
    // The ordinary fixture seed 7 lets the enemy act first, before an item can be staged.
    rig.world.configure_battle(Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000,
        fixed_seed: Some(1), ..BattleSettings::new(Arc::new(rig.rules)) }), true);
    let battle = rig.prepare();
    let prepared = rig.stored();
    rig.owner.drain();
    let mut used = false;
    for _ in 0..1000 {
        rig.now += Duration::from_millis(10);
        let reply = rig.world.battle_command(rig.owner.entity, battle, ActorId(0),
            Command::Item { item: 0, target: Target::One(ActorId(0)) }, rig.now);
        match reply {
            BattleReply::Done => { used = true; break; }
            BattleReply::Refused { reason: fmv_protocol::BattleRejection::GaugeNotFull, .. } => {},
            other => panic!("the player must get an item turn before the lethal enemy: {other:?}"),
        }
        rig.owner.drain();
    }
    assert!(used, "the player must spend an item before losing");
    assert_eq!(rig.world.players[&rig.owner.entity].inventory.as_ref().unwrap().count(0), 1);
    assert_eq!(rig.stored(), prepared, "midfight consumption is staged, not independently saved");
    let mut pending = None;
    for _ in 0..180 {
        rig.now += Duration::from_secs(1);
        rig.world.advance_battles(rig.now);
        rig.tick();
        assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::BattleEnd { .. } | ServerMsg::SceneGameOver { .. })),
            "game over also waits for the durable attempt marker");
        if let Ok(message) = rig.saves.try_recv() {
            let Msg::SceneBatch { job, done } = message else { panic!("defeat must not queue an ordinary save") };
            pending = Some((job, done));
            break;
        }
    }
    let (resolve, done) = pending.expect("the synthetic lethal formation must reach game over");
    assert_eq!(rig.stored(), prepared, "the failed attempt remains pending until its marker commits");
    assert_eq!(resolve.operation.kind, SceneOperationKind::ResolveBattle);
    assert!(resolve.members.iter().all(|member| member.battle.as_ref().is_some_and(|battle| battle.consumed_items.is_empty())));
    let failed = rig.apply(&resolve);
    assert_eq!(failed.current[0].pair, prepared.pair, "defeat neither consumes items nor pays rewards");
    let checkpoint = failed.current[0].checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.consumed_battles, 0);
    assert!(matches!(checkpoint.phase, ScenePhase::BeforeBattle { ordinal: 0, attempt: 1, .. }));
    assert!(rig.world.players[&rig.owner.entity].battle.is_some(), "disk success alone cannot end the attempt");
    rig.ack(done, failed);
    assert!(rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneGameOver { ordinal: 1, .. })));
    assert_eq!(rig.world.players[&rig.owner.entity].inventory.as_ref().unwrap().count(0), 2);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    assert!(rig.saves.try_recv().is_err());
    // A fresh server reconstructs the acknowledged game-over prompt, not an automatically
    // restarted encounter. The next attempt remains the same saved deterministic setup.
    let failed = rig.stored();
    let seed = match failed.checkpoint.as_ref().unwrap().phase {
        ScenePhase::BeforeBattle { battle_seed, .. } => battle_seed,
        _ => unreachable!(),
    };
    let (mut fresh, saves) = world(rig.rules, rig.formation);
    let mut returned = join(&mut fresh, failed.clone(), vec![failed.clone()], rig.now);
    fresh.advance_scenes(rig.now);
    let messages = returned.drain();
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::SceneGameOver { ordinal: 1, .. })));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    assert!(fresh.players[&returned.entity].battle.is_none());
    rig.run = fresh.players[&returned.entity].scene.unwrap();
    rig.world = fresh;
    rig.saves = saves;
    rig.owner = returned;
    rig.send(ClientMsg::SceneRetry { run: rig.run });
    assert!(rig.world.players[&rig.owner.entity].battle.is_none());
    rig.snapshot();
    let (retry, done) = rig.pending_batch();
    assert_eq!((retry.operation.kind, retry.operation.ordinal, retry.operation.attempt), (SceneOperationKind::PrepareBattle, 0, 1));
    assert!(matches!(retry.members[0].checkpoint.as_ref().unwrap().phase,
        ScenePhase::BeforeBattle { battle_seed, .. } if battle_seed == seed));
    let retry = rig.apply(&retry);
    assert!(rig.world.players[&rig.owner.entity].battle.is_none(), "retry also waits for its own acknowledgement");
    rig.ack(done, retry);
    assert!(rig.world.players[&rig.owner.entity].battle.is_some(), "only acknowledged explicit retry restarts combat");
    assert_eq!(rig.world.players[&rig.owner.entity].inventory.as_ref().unwrap().count(0), 2);
    assert_eq!(rig.stored().pair, prepared.pair);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn a_stopped_saver_cannot_launch_the_requested_battle() {
    let mut rig = Rig::new(3);
    let before = rig.stored();
    rig.saves.close();
    rig.request_battle();
    assert_eq!(rig.stored(), before);
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneBattleStarted { .. } | ServerMsg::EncounterStart { .. })));
    assert!(rig.world.players.get(&rig.owner.entity).is_none_or(|player| player.battle.is_none()));
}

#[test]
fn disconnect_before_any_checkpoint_does_not_leave_an_unrecoverable_account_hold() {
    let mut rig = Rig::new(3);
    let before = rig.stored();
    assert!(rig.world.leave(rig.owner.entity));
    assert!(!rig.world.scene_save_pending(rig.owner.account), "an unpersisted presentation cannot reserve the absent owner forever");
    let group = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    let returned = join(&mut rig.world, group[0].clone(), group, rig.now);
    assert!(rig.world.players[&returned.entity].scene.is_none());
    assert_eq!(rig.stored(), before);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn a_mismatched_character_in_a_prepare_ack_is_refused_before_combat_or_mirror_changes() {
    let mut rig = Rig::new(3);
    rig.request_battle();
    rig.snapshot();
    let (job, done) = rig.pending_batch();
    let mut saved = rig.apply(&job);
    saved.current[0].pair.character.as_mut().unwrap().account_id = AccountId(999_999);
    rig.owner.drain();
    rig.ack(done, saved);
    assert!(rig.world.scenes.storage_failed);
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::EncounterStart { .. } | ServerMsg::CharacterProgress { .. })));
    assert_eq!(rig.stored().pair.character.unwrap().account_id, rig.owner.account);
}

#[test]
fn recovery_refuses_old_future_or_changed_script_identity_before_admitting_gameplay() {
    let mut rig = Rig::new(3);
    rig.prepare();
    let original = rig.stored();
    for version in [None, Some(1), Some(2), Some(u32::MAX)] {
        let mut state = original.clone();
        let checkpoint = state.checkpoint.as_mut().unwrap();
        if let Some(version) = version { checkpoint.replay_version = version; }
        else { checkpoint.source_hash[0] ^= 1; }
        let (mut fresh, mut saves) = world(rig.rules, rig.formation);
        let result = admit(&mut fresh, state.clone(), vec![state], rig.now);
        assert!(matches!(result, Err(crate::world::JoinError::StorageUnavailable)), "unverified replay cannot enter the world");
        assert!(fresh.players.is_empty());
        assert!(saves.try_recv().is_err());
        assert_eq!(rig.stored(), original);
    }
}

#[test]
fn reconnect_during_a_held_settlement_retries_then_receives_the_acknowledged_victory_prefix() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    rig.win(battle);
    let (resolve, done) = rig.pending_batch();
    assert!(rig.world.leave(rig.owner.entity));
    // Before the worker writes, disk and the runtime predecessor still agree. That does not
    // permit joining while an already selected result is in flight.
    let before = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    assert!(matches!(admit(&mut rig.world, before[0].clone(), before, rig.now),
        Err(crate::world::JoinError::ProgressChanged)));
    let committed = rig.apply(&resolve);
    let after = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    assert!(matches!(admit(&mut rig.world, after[0].clone(), after.clone(), rig.now),
        Err(crate::world::JoinError::ProgressChanged)), "disk success alone does not bypass the pending acknowledgement");
    rig.ack(done, committed);
    let mut returned = join(&mut rig.world, after[0].clone(), after.clone(), rig.now);
    let messages = returned.drain();
    assert!(messages.iter().any(|message| matches!(message,
        ServerMsg::SceneStart { replay: Some(replay), .. }
            if replay.events.iter().any(|event| matches!(event, SceneReplayEvent::Battle { ordinal: 1, .. })))),
        "the reconnect rebuilds the consumed victory, never its original unconsumed battle");
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. } | ServerMsg::BattleEnd { .. })));
    assert!(rig.world.players[&returned.entity].scene.is_some());
    assert!(rig.world.players[&returned.entity].battle.is_none());
    assert_eq!(rig.store.get_scene_group(returned.account, GameId::Fixture).unwrap(), after);
    assert!(rig.saves.try_recv().is_err(), "reconstruction queues no repeated reward or item cost");
}

#[test]
fn reconnect_after_new_landing_instance_is_written_waits_for_the_existing_runs_acknowledgement() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    rig.win(battle);
    let (resolve, done) = rig.pending_batch();
    let committed = rig.apply(&resolve);
    rig.ack(done, committed);
    rig.send(ClientMsg::SceneStep { run: rig.run, step: 1, answer: None });
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (finish, done) = rig.pending_batch();
    let committed = rig.apply(&finish);
    rig.ack(done, committed);
    let old_instance = rig.stored().checkpoint.as_ref().unwrap().instance;
    let offer = rig.owner.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).unwrap();
    let enter = |world: &mut World, seat: &Seat, offer: ServerMsg, now| {
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
        assert_eq!(world.join_area(seat.entity, &area, &fingerprint, &provenance, now), crate::world::TravelReply::Done);
    };
    enter(&mut rig.world, &rig.owner, offer, rig.now);
    let (arrival, done) = rig.pending_batch();
    assert_eq!(arrival.operation.kind, SceneOperationKind::ContinueLanding);
    let next = arrival.members[0].checkpoint.as_ref().unwrap();
    assert_eq!(next.scene_key, CARGO);
    assert_ne!(next.instance, old_instance);
    let new_instance = next.instance;
    let committed = rig.apply(&arrival);
    assert_eq!(rig.world.scenes.continuations[&rig.run].instance, old_instance,
        "the runtime still owns the previous instance until acknowledgement");
    assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { scene, .. } if scene == CARGO)));
    assert!(rig.world.leave(rig.owner.entity));
    let group = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    assert_eq!(group[0].checkpoint.as_ref().unwrap().instance, new_instance);
    assert!(matches!(admit(&mut rig.world, group[0].clone(), group.clone(), rig.now),
        Err(crate::world::JoinError::ProgressChanged)), "a different durable instance cannot bypass the pending account hold");
    assert_eq!(rig.world.scene_count(), 1, "reconnect cannot create a second run for the new instance");
    assert!(!rig.world.scenes.storage_failed);
    rig.ack(done, committed);
    assert_eq!(rig.world.scenes.continuations[&rig.run].instance, new_instance);
    let mut returned = join(&mut rig.world, group[0].clone(), group.clone(), rig.now);
    let offer = returned.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).unwrap();
    enter(&mut rig.world, &returned, offer, rig.now);
    assert!(returned.drain().iter().any(|message| matches!(message,
        ServerMsg::SceneStart { scene, replay: Some(replay), .. } if scene == CARGO && replay.source == CARGO_SOURCE && replay.origin.is_none())));
    assert_eq!(rig.world.scene_count(), 1);
    assert_eq!(rig.store.get_scene_group(returned.account, GameId::Fixture).unwrap(), group);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn recovery_in_the_same_entry_field_restores_the_saved_position_and_facing() {
    let mut rig = Rig::new(3);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    let origin = Pos::new(80.0, 20.0, 0.0);
    let facing = 1.0;
    let player = rig.world.players.get_mut(&rig.owner.entity).unwrap();
    player.snapshot.pos = origin;
    player.snapshot.facing = facing;
    rig.world.start_run(rig.run, SceneRun { scene: SCENE.into(), owner: rig.owner.entity,
        watchers: vec![], seed: 19, progress: ORIGINAL.to_vec(), gil: 240, answers: vec![],
        steps: 0, started: rig.now, last_activity: rig.now, writes_story: true, preview: false,
        then: None }, &mut vec![]);
    rig.owner.drain();
    rig.prepare();
    let frozen = rig.stored();
    let (mut fresh, mut saves) = world(rig.rules, rig.formation);
    let mut returned = join(&mut fresh, frozen.clone(), vec![frozen.clone()], rig.now);
    let player = &fresh.players[&returned.entity];
    assert_eq!((player.area.as_str(), player.snapshot.pos, player.snapshot.facing), (HALL, origin, facing),
        "a matching field name must not keep the generic login spawn");
    assert!(returned.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { .. })));
    assert_eq!(rig.stored(), frozen);
    assert!(saves.try_recv().is_err());
}

#[test]
fn recovery_after_a_consumed_battle_rejects_missing_roster_or_combat_state() {
    let mut rig = Rig::new(3);
    let battle = rig.prepare();
    rig.win(battle);
    let (resolve, done) = rig.pending_batch();
    let committed = rig.apply(&resolve);
    rig.ack(done, committed);
    let original = rig.stored();
    assert_eq!(original.checkpoint.as_ref().unwrap().consumed_battles, 1);
    for missing in ["plan", "carry"] {
        let mut corrupted = original.clone();
        let checkpoint = corrupted.checkpoint.as_mut().unwrap();
        let mut payload: serde_json::Value = serde_json::from_slice(&checkpoint.replay).unwrap();
        assert!(payload[missing].is_object(), "the committed checkpoint retains {missing}");
        payload[missing] = serde_json::Value::Null;
        checkpoint.replay = serde_json::to_vec(&payload).unwrap();
        let (mut fresh, mut saves) = world(rig.rules, rig.formation);
        assert!(matches!(admit(&mut fresh, corrupted.clone(), vec![corrupted], rig.now),
            Err(crate::world::JoinError::StorageUnavailable)), "missing {missing} cannot heal or reselect the frozen roster");
        assert!(fresh.players.is_empty());
        assert!(saves.try_recv().is_err());
        assert_eq!(rig.stored(), original);
    }
}

#[test]
fn recovered_coop_waits_for_each_connected_players_area_handshake() {
    let mut rig = Rig::new(3);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    let (account, _) = rig.store.create_account("Synthetic guest", 2).unwrap();
    rig.store.put_progression(&Progression::new(account, GameId::Fixture, ORIGINAL.to_vec(), 2)).unwrap();
    let mut character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
    character.inventory = Some(vec![InventoryEntry::new(0, 2)]);
    rig.store.put_character_progress(&character).unwrap();
    let state = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut guest = join(&mut rig.world, state.clone(), vec![state], rig.now);
    assert!(matches!(rig.world.party_invite(rig.owner.entity, guest.entity, rig.now), crate::world::PartyOutcome::Done));
    let invite = rig.world.parties.invites().next().unwrap().id;
    assert!(matches!(rig.world.party_accept(guest.entity, invite, rig.now), crate::world::PartyOutcome::Done));
    rig.world.start_run(rig.run, SceneRun { scene: SCENE.into(), owner: rig.owner.entity,
        watchers: vec![guest.entity], seed: 19, progress: ORIGINAL.to_vec(), gil: 240, answers: vec![],
        steps: 0, started: rig.now, last_activity: rig.now, writes_story: true, preview: false,
        then: None }, &mut vec![]);
    rig.owner.drain();
    guest.drain();
    rig.prepare();
    let group = rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap();
    assert_eq!(group.len(), 2, "both participants are frozen before either can recover");
    let (mut fresh, mut saves) = world_at(rig.rules, rig.formation, YARD);
    let owner_state = group.iter().find(|state| state.pair.story.account_id == rig.owner.account).unwrap().clone();
    let guest_state = group.iter().find(|state| state.pair.story.account_id == guest.account).unwrap().clone();
    let mut owner = join(&mut fresh, owner_state, group.clone(), rig.now);
    let mut guest = join(&mut fresh, guest_state, group.clone(), rig.now);
    let owner_offer = owner.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).unwrap();
    let guest_offer = guest.drain().into_iter().find(|message| matches!(message, ServerMsg::AreaChange { .. })).unwrap();
    let enter = |world: &mut World, seat: &Seat, offer: ServerMsg| {
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
        assert_eq!(world.join_area(seat.entity, &area, &fingerprint, &provenance, rig.now), crate::world::TravelReply::Done);
    };
    enter(&mut fresh, &owner, owner_offer);
    fresh.advance_scenes(rig.now);
    assert!(!fresh.scenes.storage_failed, "a still-loading connected guest is not corrupt storage");
    assert!(fresh.players[&owner.entity].battle.is_none());
    assert!(fresh.players[&guest.entity].transit.is_some());
    enter(&mut fresh, &guest, guest_offer);
    fresh.advance_scenes(rig.now);
    assert!(!fresh.scenes.storage_failed);
    let battle = fresh.players[&owner.entity].battle.expect("the last handshake releases the prepared fight");
    assert_eq!(fresh.players[&guest.entity].battle, Some(battle));
    assert_eq!(rig.store.get_scene_group(rig.owner.account, GameId::Fixture).unwrap(), group);
    assert!(saves.try_recv().is_err(), "a prepared recovery requires no second prepare or reward write");
}

#[test]
fn malformed_postbattle_messages_drop_only_the_sender_and_preserve_the_checkpoint() {
    for premature_end in [false, true] {
        let mut rig = Rig::new(3);
        let battle = rig.prepare();
        rig.win(battle);
        let (resolve, done) = rig.pending_batch();
        let saved = rig.apply(&resolve);
        rig.ack(done, saved);
        let before = rig.stored();
        let (account, _) = rig.store.create_account("Unrelated player", 2).unwrap();
        rig.store.put_progression(&Progression::new(account, GameId::Fixture, ORIGINAL.to_vec(), 2)).unwrap();
        let character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
        rig.store.put_character_progress(&character).unwrap();
        let state = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
        let bystander = join(&mut rig.world, state.clone(), vec![state], rig.now);
        rig.owner.drain();
        rig.send(if premature_end { ClientMsg::SceneEnd { run: rig.run, skipped: false } }
            else { ClientMsg::SceneStep { run: rig.run, step: 1, answer: Some(2) } });
        assert!(!rig.world.players.contains_key(&rig.owner.entity));
        assert!(rig.world.players.contains_key(&bystander.entity), "malformed presentation is not uncertainty about another account's storage");
        assert!(!rig.world.scenes.storage_failed);
        assert_eq!(rig.stored(), before);
        assert!(rig.saves.try_recv().is_err());
        assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
        let mut returned = join(&mut rig.world, before.clone(), vec![before], rig.now);
        assert!(returned.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { replay: Some(_), .. })),
            "the last acknowledged prefix remains recoverable after rejecting its sender");
    }
}
