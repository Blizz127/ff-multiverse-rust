//! Owned-data TCP proof of the cargo door, durable train landing and moment-51 control handoff.
//! Owned assets are read only; every account, save and server restart uses a temporary store.

mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, TestServer};
use fmv_adapter_api::{AreaRuntime, MoveResult, StoryPoint, StoryTrigger};
use fmv_core::{AccountId, AnimState, EntityId, GameId, InventoryEntry, Pos};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    cargo_door_scene, scene, MoveHow, SceneBoundary, ScenePlay, SceneResources, Stage, WindowAck,
    CARGO_DOOR_SCENE_ID, TRAIN_SCENE_ID,
};
use fmv_ff7::story::StoryState;
use fmv_persist::{CharacterProgress, Progression, ScenePhase, SceneState, Store};
use fmv_protocol::{Auth, ClientMsg, NewCharacter, SceneOutcome, SceneRole, SceneRunId, Secret, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ff7::{open_battle_data, open_ff7, prewarm, Ff7Source, SharedFf7};
use fmv_server::scene_host::Ff7SceneHost;
use fmv_server::{BattleSettings, SceneSettings, ServerConfig};

struct Peer {
    client: Client,
    pending: VecDeque<ServerMsg>,
    entity: EntityId,
    pos: Pos,
    facing: f32,
    progress: Vec<u8>,
    gil: u32,
}

impl Peer {
    async fn next(&mut self) -> ServerMsg {
        match self.pending.pop_front() { Some(message) => message, None => self.client.next().await }
    }

    async fn scene(&mut self, expected: &str) -> ServerMsg {
        loop {
            match self.next().await {
                message @ ServerMsg::SceneStart { .. } => {
                    let ServerMsg::SceneStart { scene, owner, you, .. } = &message else { unreachable!() };
                    assert_eq!(scene, expected);
                    assert_eq!((*owner, *you), (self.entity, SceneRole::Owner));
                    return message;
                }
                ServerMsg::MoveRejected { seq, corrected } => panic!("cargo approach step {seq} refused: {corrected:?}"),
                ServerMsg::AreaChange { .. } => panic!("the cargo door must play before travel"),
                ServerMsg::SceneRefused { reason, .. } => panic!("scene refused: {reason:?}"),
                ServerMsg::Reject { code, reason } => panic!("scene refused: {code:?}: {reason}"),
                _ => {}
            }
        }
    }
}

async fn login(addr: std::net::SocketAddr, token: &str, target: &str) -> Peer {
    let mut client = Client::connect(addr).await;
    client.send(&ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, client_build: "train-route-test".into(),
        auth: Auth::Token(Secret::new(token.to_string())), character: NewCharacter::with_tint([9, 9, 9]) }).await;
    let (mut entity, mut gil, mut progress, mut position) = (None, None, None, None);
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
                let own = entity.expect("Welcome precedes the area snapshot");
                let own = entities.iter().find(|p| p.id == own).expect("own entity");
                position = Some((own.pos, own.facing));
            }
            message @ ServerMsg::SceneStart { .. } => pending.push_back(message),
            ServerMsg::EncounterStart { .. } => panic!("the cargo door and train arrival contain no battle"),
            ServerMsg::Reject { code, reason } => panic!("login refused: {code:?}: {reason}"),
            _ => {}
        }
        if let (Some(entity), Some(gil), Some(progress), Some((pos, facing))) = (entity, gil, progress.as_ref(), position) {
            return Peer { client, pending, entity, pos, facing, progress: progress.clone(), gil };
        }
    }
}

async fn boot(state: &Path, adapter: &Arc<fmv_ff7::Ff7Adapter>, rules: &Arc<fmv_protocol::Ff7BattleData>) -> TestServer {
    let config = ServerConfig {
        area_key: "md1stin".into(), state_dir: Some(state.to_path_buf()),
        battle: Some(BattleSettings { random_encounters: false, ..BattleSettings::new(rules.clone()) }),
        scenes: Some(SceneSettings { host: Arc::new(Ff7SceneHost::with_extended_save(adapter.clone(), true)),
            debug_preview: false, debug_new_story: None }),
        ..ServerConfig::default()
    };
    TestServer::start_over(config, Box::new(SharedFf7(adapter.clone()))).await
}

/// Approach the actual border1 line from cargo's saved control position. Every TCP move
/// is independently checked against the owned walkmesh; the final step crosses that line.
async fn cross_door(peer: &mut Peer, area: &dyn AreaRuntime) -> Pos {
    let line = cargo_door_scene().line_trigger().expect("authored door line");
    assert_eq!((line.a, line.b), ([-15, -104, 0], [53, -104, 0]));
    let pos = |p: [i16; 3]| Pos::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
    let trigger = StoryPoint { id: 0, label: "cargo door".into(), trigger: StoryTrigger::Line(pos(line.a), pos(line.b)) };
    let goal = Pos::new(19.0, -114.0, peer.pos.z);
    for seq in 1..=20 {
        let distance = peer.pos.dist_xy(goal);
        assert!(distance > 0.1, "the goal lies past the door line");
        let step = (20.0 / distance).min(1.0);
        let wanted = Pos::new(peer.pos.x + (goal.x - peer.pos.x) * step,
            peer.pos.y + (goal.y - peer.pos.y) * step, peer.pos.z);
        let MoveResult::Accepted(next) = area.validate_move(peer.pos, wanted) else { panic!("cargo's door approach is obstructed") };
        assert!(next.dist_xy(wanted) < 0.75, "the authored approach does not slide along a wall");
        let crossed = trigger.is_hit_by(peer.pos, next);
        tokio::time::sleep(Duration::from_millis(100)).await;
        peer.client.send(&ClientMsg::MoveIntent { seq, pos: next, facing: 0.0, anim: AnimState::Walk }).await;
        peer.pos = next;
        if crossed { return next; }
    }
    panic!("cargo approach never crossed the door line");
}

struct Driver {
    run: SceneRunId,
    play: ScenePlay,
    passed: u32,
}

impl Driver {
    fn new(start: ServerMsg, adapter: &fmv_ff7::Ff7Adapter, crossing: Option<Pos>) -> Self {
        let ServerMsg::SceneStart { run, scene: id, seed, progress, gil, replay, .. } = start else { panic!("scene start") };
        let replay = replay.expect("door and train use durable strict playback");
        replay.validate().unwrap();
        assert!(replay.events.is_empty(), "both starts begin at their own first boundary");
        let mut def = scene(&id).expect("registered continuation");
        let area = adapter.load_field(&def.field).unwrap();
        match crossing {
            Some(pos) => {
                assert_eq!(id, CARGO_DOOR_SCENE_ID);
                let origin = replay.origin.expect("the door preserves the accepted crossing pose");
                assert_eq!((origin.pos, origin.facing), (pos, 0.0));
                def = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &def,
                    origin.pos, origin.facing, origin.triangle).expect("accepted crossing is playable");
            }
            None => { assert_eq!(id, TRAIN_SCENE_ID); assert!(replay.origin.is_none()); }
        }
        let mut play = ScenePlay::with_resources(area.field_file(), &def, &StoryState::decode(&progress).unwrap(),
            seed, SceneResources { gil: Some(gil) }).unwrap();
        assert_eq!(play.replay_identity().fingerprint, replay.source);
        let beat = play.next_strict(None).unwrap();
        if crossing.is_some() {
            assert!(beat.stage.iter().any(|t| matches!(t.what, Stage::Sound { channel: 2, id: 233, pan: 64 })),
                "the authored door sound plays on effect channel 2 at the centre pan");
            assert!(beat.stage.iter().any(|t| matches!(t.what, Stage::Move { entity: 11, x: 9, y: -247, how: MoveHow::Jump, .. })),
                "Cloud jumps through the door before the field transfer");
            assert!(beat.stage.iter().any(|t| matches!(t.what, Stage::Show { entity: 11, visible: false })), "Cloud hides before travel");
        }
        Self { run, play, passed: 0 }
    }

    async fn advance(&mut self, peer: &mut Peer) -> End {
        loop {
            match self.play.boundary().unwrap() {
                SceneBoundary::Window(mark) => {
                    assert!(mark.choices.is_none(), "the train introduction has no questions");
                    peer.client.send(&ClientMsg::SceneStep { run: self.run, step: mark.ordinal - 1, answer: None }).await;
                    self.play.next_strict(Some(WindowAck { checkpoint: mark, answer: None })).unwrap();
                    self.passed = mark.ordinal;
                }
                SceneBoundary::End(end) => return end,
                SceneBoundary::Battle(_) => panic!("the door and train introduction contain no fight"),
            }
        }
    }

    async fn finish(&self, peer: &mut Peer) {
        assert!(matches!(self.play.boundary(), Some(SceneBoundary::End(_))));
        for _ in 0..2 { peer.client.send(&ClientMsg::SceneEnd { run: self.run, skipped: false }).await; }
    }
}

fn saved(state: &Path, account: AccountId) -> SceneState {
    Store::open(state).unwrap().get_scene_state(account, GameId::Ff7).unwrap()
}

fn record_bytes(state: &Path, account: AccountId) -> [Vec<u8>; 2] {
    let dir = state.join("progression").join(account.0.to_string());
    [std::fs::read(dir.join("Ff7.json")).unwrap(), std::fs::read(dir.join("Ff7.character.json")).unwrap()]
}

fn assert_resources_unchanged(actual: &CharacterProgress, original: &CharacterProgress) {
    // The durable scene receipt updates storage metadata even though no resource changed.
    let mut actual = actual.clone();
    actual.updated_at = original.updated_at;
    actual.schema_version = original.schema_version;
    assert_eq!(&actual, original, "the door and train award and charge nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs FMV_FF7_LIBRARY; owned assets are read-only and all saves are temporary"]
async fn cargo_door_and_train_arrival_keep_their_landing_until_control_is_durably_returned() {
    let library = std::env::var_os("FMV_FF7_LIBRARY").expect("owned library path");
    let source = Ff7Source::Library { state_dir: Some(library.into()), set: None };
    let opened = open_ff7(&source).unwrap();
    prewarm(&opened.adapter).await.unwrap();
    let (rules, _) = open_battle_data(&source).unwrap();
    let cargo = opened.adapter.load_field("cargoin").unwrap();
    let train = opened.adapter.load_field("tin_1").unwrap();
    let landing_pos = train.place_arrival(Pos::new(-2.0, 608.0, 0.0), Some(29));
    let control_pos = train.place_arrival(Pos::new(2.0, 537.0, -53.0), Some(28));
    assert_ne!(landing_pos, control_pos, "arrival and the authored control handoff are distinct");
    let state = tempfile::tempdir().unwrap();
    let mut story = StoryState::at(48).into_extended().unwrap();
    story.set_bit(3, 66, 0, true);
    let before = story.encode();
    let (account, token, original_character) = {
        let store = Store::open(state.path()).unwrap();
        let (account, token) = store.create_account("Train traveller", 1).unwrap();
        store.put_progression(&Progression::new(account, GameId::Ff7, before.clone(), 1)).unwrap();
        let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, 1);
        character.gil = 617;
        character.exp = 83;
        character.ap = 19;
        character.limit = 33;
        character.inventory = Some(vec![InventoryEntry { item: 0, count: 2 }, InventoryEntry { item: 1, count: 1 }]);
        store.put_character_progress(&character).unwrap();
        (account, token, store.get_character_progress(account, GameId::Ff7).unwrap().unwrap())
    };
    let mut server = boot(state.path(), &opened.adapter, &rules).await;
    let mut peer = login(server.addr, &token, "cargoin").await;
    assert_eq!((&peer.progress, peer.gil), (&before, original_character.gil));
    assert_eq!(peer.pos, cargo.place_arrival(Pos::new(-24.0, 16.0, 0.0), Some(20)));
    let crossing = cross_door(&mut peer, &cargo).await;
    let mut door = Driver::new(peer.scene(CARGO_DOOR_SCENE_ID).await, &opened.adapter, Some(crossing));
    assert_eq!(door.advance(&mut peer).await, End::MapJump { field: 139 });
    assert_eq!(door.passed, 0, "the door script runs without dialogue");
    door.finish(&mut peer).await;
    let mut ended = false;
    loop {
        match peer.next().await {
            ServerMsg::SceneEnded { run, outcome } => { assert_eq!((run, outcome), (door.run, SceneOutcome::Finished)); ended = true; }
            ServerMsg::AreaChange { area, arrival, arrival_facing, .. } => {
                assert!(ended, "the door commits before travel is offered");
                assert_eq!(area.key, "tin_1");
                assert_eq!(arrival, landing_pos);
                assert_eq!(arrival_facing, fmv_ff7::field_direction_to_facing(252));
                break; // Do not JoinArea: restart from the durable landing, still at moment 48.
            }
            ServerMsg::StoryUpdate { progress, .. } => assert_eq!(progress, before),
            ServerMsg::SceneRefused { reason, .. } => panic!("door completion refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("door completion refused: {code:?}: {reason}"),
            _ => {}
        }
    }
    server.shutdown().await;
    let landing = saved(state.path(), account);
    assert_eq!(landing.pair.story.blob, before);
    assert_resources_unchanged(landing.pair.character.as_ref().unwrap(), &original_character);
    assert!(matches!(landing.checkpoint.as_ref().expect("durable train landing").phase, ScenePhase::Landing { .. }));
    assert!(landing.reservation.is_none());

    server = boot(state.path(), &opened.adapter, &rules).await;
    peer = login(server.addr, &token, "tin_1").await;
    assert_eq!((&peer.progress, peer.gil), (&before, original_character.gil));
    assert_eq!(peer.pos, landing_pos, "the pending landing overrides moment-48's ordinary cargo entry");
    assert_eq!(peer.facing, fmv_ff7::field_direction_to_facing(252));
    let mut intro = Driver::new(peer.scene(TRAIN_SCENE_ID).await, &opened.adapter, None);
    assert_eq!(intro.advance(&mut peer).await, End::Finished);
    assert_eq!(intro.passed, 2);
    intro.finish(&mut peer).await;
    let mut updated = false;
    let mut placed = false;
    let mut ended = false;
    while !(updated && placed && ended) {
        match peer.next().await {
            ServerMsg::StoryUpdate { progress, .. } => {
                let mut expected = story.clone();
                expected.moment = 51;
                assert_eq!(progress, expected.encode(), "only the authored story moment changes");
                updated = true;
            }
            ServerMsg::AreaSnapshot { area, entities, .. } if area.key == "tin_1" => {
                let own = entities.iter().find(|p| p.id == peer.entity).expect("own control snapshot");
                assert_eq!((own.pos, own.facing), (control_pos, fmv_ff7::field_direction_to_facing(0)));
                assert_eq!(own.anim, AnimState::Idle);
                placed = true;
            }
            ServerMsg::SceneEnded { run, outcome } => {
                assert_eq!((run, outcome), (intro.run, SceneOutcome::Finished));
                assert!(updated && placed, "moment 51 and its final control pose arrive before control is released");
                ended = true;
            }
            ServerMsg::AreaChange { .. } => panic!("train introduction returns control in the same field"),
            ServerMsg::SceneRefused { reason, .. } => panic!("train completion refused: {reason:?}"),
            ServerMsg::Reject { code, reason } => panic!("train completion refused: {code:?}: {reason}"),
            _ => {}
        }
    }
    server.shutdown().await;
    let complete = saved(state.path(), account);
    assert!(complete.checkpoint.is_none() && complete.reservation.is_none());
    assert_eq!(StoryState::decode(&complete.pair.story.blob).unwrap().moment, 51);
    assert_resources_unchanged(complete.pair.character.as_ref().unwrap(), &original_character);
    let bytes = record_bytes(state.path(), account);

    server = boot(state.path(), &opened.adapter, &rules).await;
    peer = login(server.addr, &token, "tin_1").await;
    assert_eq!((&peer.progress, peer.gil), (&complete.pair.story.blob, original_character.gil));
    assert_eq!((peer.pos, peer.facing), (control_pos, fmv_ff7::field_direction_to_facing(0)));
    for run in [door.run, intro.run] { peer.client.send(&ClientMsg::SceneEnd { run, skipped: false }).await; }
    peer.client.send(&ClientMsg::Ping { nonce: 5151 }).await;
    loop {
        match peer.next().await {
            ServerMsg::Pong { nonce: 5151, .. } => break,
            ServerMsg::SceneStart { .. } | ServerMsg::SceneEnded { .. } | ServerMsg::AreaChange { .. }
            | ServerMsg::EncounterStart { .. } => panic!("a completed route cannot replay on login or duplicate completion"),
            ServerMsg::StoryUpdate { advance: Some(_), .. } => panic!("a duplicate completion cannot advance story"),
            ServerMsg::Reject { code, reason } => panic!("relogin refused: {code:?}: {reason}"),
            _ => {}
        }
    }
    server.shutdown().await;
    assert_eq!(saved(state.path(), account), complete);
    assert_eq!(record_bytes(state.path(), account), bytes, "relogin and stale duplicate completions do not rewrite the save");
    eprintln!("train TCP route: authored door, recovered landing, 2 windows, durable moment51 and final control pose");
}
