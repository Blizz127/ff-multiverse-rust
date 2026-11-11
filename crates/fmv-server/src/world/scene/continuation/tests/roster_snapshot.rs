//! Authored party snapshots pass through real prepare/settlement/reconnect persistence.
//! The rooms, host, battles and native stories are synthetic; no owned game data is read.

use super::*;
use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, Bounds, MoveResult};
use fmv_ff7::story::{checked_canon_party_of, StoryState};

const NATIVE_SCENE: &str = "fixture/native-roster";
const NATIVE_SOURCE: [u8; 32] = [94; 32];

fn native_story(slots: Option<[u8; 3]>) -> Vec<u8> {
    let mut story = StoryState::fresh_first_control().into_native_party().unwrap();
    if let Some(slots) = slots { story.native_party_mut().unwrap().write_main_slots(slots, 7).unwrap(); }
    story.encode()
}

struct NativeRooms;
struct NativeRoom(Box<dyn AreaRuntime>);

impl GameAdapter for NativeRooms {
    fn game(&self) -> GameId { GameId::Fixture }
    fn list_areas(&self) -> Vec<AreaInfo> { RoomsAdapter::new().list_areas() }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        Ok(Box::new(NativeRoom(RoomsAdapter::new().load_area(key)?)))
    }
}

impl AreaRuntime for NativeRoom {
    fn info(&self) -> &AreaInfo { self.0.info() }
    fn spawn_points(&self) -> Vec<Pos> { self.0.spawn_points() }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult { self.0.validate_move(from, to) }
    fn bounds(&self) -> Bounds { self.0.bounds() }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.0.walk_triangles() }
    fn max_speed(&self) -> f32 { self.0.max_speed() }
    // Explicitly synthetic admission; this does not enable FF7 runtime support for v4.
    fn supports_saved_progression(&self, _: &[u8]) -> bool { true }
    fn phase_key(&self, _: &[u8]) -> Option<fmv_core::PhaseKey> { Some(fmv_core::PhaseKey::default()) }
    fn story_party(&self, _: &[u8]) -> Vec<u8> { panic!("native parties require the checked query") }
    fn try_story_party(&self, progress: &[u8]) -> Result<Vec<u8>, AdapterError> {
        checked_canon_party_of(progress).ok_or_else(|| AdapterError::Unsupported("unknown native party".into()))
    }
}

/// Each accepted window reaches a battle. After victory, reevaluation has already applied
/// the next party write before presenting the next window, while the durable Resume pair
/// and retained plan still describe the last accepted battle.
struct NativeHost { original: Vec<u8>, boundaries: Vec<Vec<u8>> }

impl NativeHost {
    fn new(boundaries: Vec<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self { original: native_story(Some([0, 255, 255])), boundaries })
    }
}

impl SceneHost for NativeHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![SceneInfo { id: NATIVE_SCENE.into(), field: HALL.into(), arrival: Pos::default(), arrival_facing: 0.0, arrival_hint: None }]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { scene == NATIVE_SCENE && progress == self.original }
    fn continuation(&self, scene: &str) -> bool { scene == NATIVE_SCENE }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { panic!("durable path only") }
    fn evaluate_continuation(&self, scene: &str, original: &[u8], _: u32, gil: u32, replay: Option<&SceneReplay>) -> Option<SceneBoundary> {
        if scene != NATIVE_SCENE || original != self.original
            || replay.is_some_and(|replay| replay.source != NATIVE_SOURCE || replay.validate().is_err()) { return None; }
        let mut stage = 0usize;
        let mut window_accepted = false;
        let mut current_gil = gil;
        for event in replay.map_or(&[][..], |replay| replay.events.as_slice()) {
            match event {
                SceneReplayEvent::Window { step, identity, answer } if !window_accepted
                    && *step == stage as u32 && identity == &[100 + stage as u8]
                    && *answer == if stage == 0 { Some(2) } else { None } => window_accepted = true,
                SceneReplayEvent::Battle { ordinal, identity, formation: 3, progress, gil } if window_accepted
                    && *ordinal == stage as u32 + 1 && identity == &[200 + stage as u8]
                    && self.boundaries.get(stage) == Some(progress) => {
                        stage += 1;
                        window_accepted = false;
                        current_gil = *gil;
                    }
                _ => return None,
            }
        }
        let progress = if stage == 0 && !window_accepted { self.original.clone() } else { self.boundaries.get(stage)?.clone() };
        let at = if window_accepted {
            SceneBoundaryKind::Battle { ordinal: stage as u32 + 1, identity: vec![200 + stage as u8], formation: 3, flags: Some(8) }
        } else {
            SceneBoundaryKind::Window { step: stage as u32, identity: vec![100 + stage as u8], choices: (stage == 0).then_some([2, 3]) }
        };
        Some(SceneBoundary { source: NATIVE_SOURCE, progress, gil: current_gil, at })
    }
}

fn native_world(rules: SyntheticRules, host: Arc<NativeHost>) -> (World, mpsc::UnboundedReceiver<Msg>) {
    let adapter: Arc<dyn GameAdapter> = Arc::new(NativeRooms);
    let mut world = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    world.set_area_source(adapter);
    world.configure_battle(Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000, companions_player_commanded: true,
        fixed_seed: Some(7), ..BattleSettings::new(Arc::new(rules)) }), true);
    world.configure_scenes(Some(SceneSettings { host, debug_preview: false, debug_new_story: None }));
    let (saver, saves) = SaverHandle::channel();
    world.set_progress_saver(saver);
    (world, saves)
}

fn native_rig(host: Arc<NativeHost>) -> Rig {
    let rules = SyntheticRules { enemy_hp: 100, ..SyntheticRules::default() };
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    store.require_compatibility(fmv_persist::StoreCompatibility::FieldVisit).unwrap();
    let (account, _) = store.create_account("Native owner", 1).unwrap();
    store.put_progression(&Progression::new(account, GameId::Fixture, host.original.clone(), 1)).unwrap();
    let mut character = progress::fresh(&rules, 4).to_record(account, GameId::Fixture, 1);
    character.gil = 240;
    character.inventory = Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(20, 8)]);
    store.put_character_progress(&character).unwrap();
    let (mut world, saves) = native_world(rules, host.clone());
    let now = Instant::now();
    let state = store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut owner = join(&mut world, state.clone(), vec![state], now);
    owner.drain();
    let run = SceneRunId(71);
    world.start_run(run, SceneRun { scene: NATIVE_SCENE.into(), owner: owner.entity, watchers: vec![], seed: 19,
        progress: host.original.clone(), gil: 240, answers: vec![], steps: 0, started: now, last_activity: now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    assert!(owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { replay: Some(_), .. })));
    Rig { _dir: dir, store, world, saves, rules, formation: 3, owner, now, run }
}

fn restart_native(rig: Rig, host: Arc<NativeHost>) -> Rig {
    let Rig { _dir, store, world, saves, rules, formation, owner, now, .. } = rig;
    let account = owner.account;
    let root = store.root().to_path_buf();
    drop(world);
    drop(saves);
    drop(owner);
    drop(store);
    let store = Store::open(root).unwrap();
    let (mut world, saves) = native_world(rules, host);
    let group = store.get_scene_group(account, GameId::Fixture).unwrap();
    let state = group.iter().find(|state| state.pair.story.account_id == account).unwrap().clone();
    let owner = join(&mut world, state, group, now);
    let run = world.players[&owner.entity].scene.expect("Resume reconstructs its scene");
    Rig { _dir, store, world, saves, rules, formation, owner, now, run }
}

fn canon(plan: &SceneBattlePlan) -> Vec<u8> {
    plan.setup.party.iter().take_while(|member| !member.is_player()).map(|member| member.character).collect()
}

fn prepare_native(rig: &mut Rig, stage: u32, expected: &[u8]) -> BattleId {
    rig.send(ClientMsg::SceneStep { run: rig.run, step: stage, answer: (stage == 0).then_some(2) });
    rig.send(ClientMsg::SceneBattleReady { run: rig.run, ordinal: stage + 1, step: stage + 1 });
    rig.snapshot();
    let (job, done) = rig.pending_batch();
    assert_eq!((job.operation.kind, job.operation.ordinal), (SceneOperationKind::PrepareBattle, stage));
    let payload: Payload = serde_json::from_slice(&job.members[0].checkpoint.as_ref().unwrap().replay).unwrap();
    assert_eq!(canon(payload.plan.as_ref().unwrap()), expected);
    assert_eq!(checked_canon_party_of(job.members[0].next_story.as_deref().unwrap()), Some(expected.to_vec()));
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    rig.world.players[&rig.owner.entity].battle.expect("acknowledged native roster starts combat")
}

fn win_native(rig: &mut Rig, battle: BattleId) {
    let plan = rig.world.scenes.continuations[&rig.run].payload.plan.as_ref().unwrap();
    let own = ActorId(plan.setup.party.iter().position(|member| member.account() == Some(rig.owner.account)).unwrap() as u8);
    let enemy = ActorId(plan.setup.party.len() as u8);
    let mut finished = false;
    for _ in 0..1000 {
        rig.now += Duration::from_millis(10);
        if matches!(rig.world.battle_command(rig.owner.entity, battle, own,
            Command::Item { item: 20, target: Target::One(enemy) }, rig.now), BattleReply::Done) {
            finished = true;
            break;
        }
        rig.owner.drain();
    }
    assert!(finished, "the owner's item wins the synthetic battle");
    rig.tick();
    let (job, done) = rig.pending_batch();
    assert_eq!(job.operation.kind, SceneOperationKind::ResolveBattle);
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    assert!(matches!(rig.stored().checkpoint.unwrap().phase, ScenePhase::Resume { .. }));
}

#[test]
fn first_prepare_uses_the_authored_boundary_party_and_never_fills_empty_or_unknown_slots() {
    for (boundary, expected) in [
        (native_story(Some([2, 255, 0])), Some(vec![2, 0])),
        (native_story(Some([255; 3])), Some(vec![])),
        (native_story(None), None),
    ] {
        let host = NativeHost::new(vec![boundary]);
        let mut rig = native_rig(host.clone());
        assert_eq!(checked_canon_party_of(&rig.world.players[&rig.owner.entity].progression), Some(vec![0]));
        let before = rig.stored();
        if let Some(expected) = expected {
            prepare_native(&mut rig, 0, &expected);
            let current = &rig.world.scenes.continuations[&rig.run];
            let plan = current.payload.plan.as_ref().unwrap();
            assert_eq!(plan.setup.party.iter().filter(|member| member.is_player()).count(), 1);
            assert_eq!(plan.setup.party.len(), expected.len() + 1, "known empty keeps only the owner");
            assert_eq!(rig.stored().pair.story.blob, host.boundaries[0]);
        } else {
            rig.request_battle();
            assert!(rig.saves.try_recv().is_err(), "unknown membership cannot reach even the snapshot request");
            assert_eq!(rig.stored(), before);
            assert_eq!(rig.world.battle_count(), 0);
            assert!(!rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
        }
    }
}

#[test]
fn resumed_plan_uses_the_last_battle_snapshot_before_later_reorders_removals_or_unsupported_additions() {
    for unsupported in [[2, 0, 255], [2, 1, 255]] {
        let host = NativeHost::new(vec![
            native_story(Some([2, 0, 255])), native_story(Some([0, 2, 255])),
            native_story(Some([2, 255, 255])), native_story(Some(unsupported)),
        ]);
        let mut rig = native_rig(host.clone());
        let battle = prepare_native(&mut rig, 0, &[2, 0]);
        win_native(&mut rig, battle);
        let durable = rig.stored();
        assert_eq!(durable.pair.story.blob, host.boundaries[0]);
        rig = restart_native(rig, host.clone());
        assert_eq!(rig.stored(), durable, "reconnect validates rather than rewriting the saved party");
        let resumed = &rig.world.scenes.continuations[&rig.run];
        assert_eq!(canon(resumed.payload.plan.as_ref().unwrap()), [2, 0]);
        assert_eq!(checked_canon_party_of(&resumed.boundary.progress), Some(vec![0, 2]),
            "the evaluated window has already changed party, but it cannot invalidate the prior battle plan");
        let last_battle = resumed.payload.replay.events.iter().rev().find_map(|event| match event {
            SceneReplayEvent::Battle { progress, .. } => Some(progress), _ => None,
        }).unwrap();
        assert_eq!(last_battle, &host.boundaries[0]);
        assert!(rig.owner.drain().iter().any(|message| matches!(message, ServerMsg::SceneStart { replay: Some(_), .. })));

        let battle = prepare_native(&mut rig, 1, &[0, 2]);
        win_native(&mut rig, battle);
        let battle = prepare_native(&mut rig, 2, &[2]);
        win_native(&mut rig, battle);
        let before_refusal = rig.stored();
        rig.send(ClientMsg::SceneStep { run: rig.run, step: 3, answer: None });
        rig.send(ClientMsg::SceneBattleReady { run: rig.run, ordinal: 4, step: 4 });
        assert!(rig.saves.try_recv().is_err(), "new and removed-then-rejoining actors have no carried resource evidence");
        assert_eq!(rig.stored(), before_refusal);
        assert_eq!(rig.world.battle_count(), 0);
    }
}

#[test]
fn unrelated_games_keep_opaque_progress_even_when_it_decodes_as_native_ff7_party_state() {
    struct OtherGameRoom { info: AreaInfo, inner: Box<dyn AreaRuntime> }
    impl AreaRuntime for OtherGameRoom {
        fn info(&self) -> &AreaInfo { &self.info }
        fn spawn_points(&self) -> Vec<Pos> { self.inner.spawn_points() }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult { self.inner.validate_move(from, to) }
        fn bounds(&self) -> Bounds { self.inner.bounds() }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.inner.walk_triangles() }
        fn max_speed(&self) -> f32 { self.inner.max_speed() }
        fn try_story_party(&self, _: &[u8]) -> Result<Vec<u8>, AdapterError> {
            panic!("another game's opaque progression cannot request an FF7 snapshot override")
        }
    }
    let progressions = [native_story(Some([2, 0, 255])), native_story(None), vec![254]];
    for game in GameId::ALL.into_iter().filter(|game| !matches!(game, GameId::Ff7 | GameId::Fixture)) {
        let inner = RoomsAdapter::new().load_area(HALL).unwrap();
        let mut info = inner.info().clone();
        info.id.game = game;
        info.provenance.game = game;
        // The geometry remains an explicitly synthetic fixture; this is no support claim
        // for the unrelated real game whose progression namespace is being tested.
        assert!(info.fixture);
        let world = World::new(Box::new(OtherGameRoom { info, inner })).unwrap();
        for progression in &progressions {
            assert_eq!(world.scene_battle_canon(HALL, progression), Ok(None), "opaque {game:?} state");
        }
    }
}
