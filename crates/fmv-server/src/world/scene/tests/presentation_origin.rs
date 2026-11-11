//! The accepted ordinary presentation pose is an explicit wire input, not a durable replay.
use super::*;
use fmv_protocol::SceneOrigin;

#[test]
fn an_ordinary_origin_is_shared_without_moving_either_participant() {
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    let mut owner = seat(&mut w, "Owner", &[], now);
    let mut watcher = seat(&mut w, "Watcher", &[], now);
    party(&mut w, &owner, &watcher, now);
    assert_eq!(go(&mut w, &watcher, 100.0, 60.0, &mut now), MoveOutcome::Applied);
    assert_eq!(go(&mut w, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    watcher.drain();
    let watcher_before = w.players.get(&watcher.entity).unwrap().snapshot.clone();
    let at = Pos::new(201.0, 7.0, 0.0);
    let facing = 1.25;
    now += Duration::from_millis(700);
    assert_eq!(w.apply_move(owner.entity, at, facing, AnimState::Walk, now), MoveOutcome::Applied);
    let expected = SceneOrigin { pos: at, facing, triangle: None };
    let starts_with_origin = |messages: Vec<ServerMsg>| messages.into_iter().filter_map(|message| match message {
        ServerMsg::SceneStart { presentation_origin, replay, you, .. } => Some((presentation_origin, replay, you)),
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(starts_with_origin(owner.drain()), [(Some(expected), None, SceneRole::Owner)]);
    assert_eq!(starts_with_origin(watcher.drain()), [(Some(expected), None, SceneRole::Watcher)]);
    let validations = host.presentation_origins.lock().unwrap();
    assert!(!validations.is_empty());
    assert!(validations.iter().all(|(scene, field, progress, origin)|
        scene == POINT_SCENE && field == HALL && progress.is_empty() && *origin == expected));
    drop(validations);
    let owner_now = &w.players.get(&owner.entity).unwrap().snapshot;
    assert_eq!((owner_now.pos, owner_now.facing), (at, facing));
    let watcher_now = &w.players.get(&watcher.entity).unwrap().snapshot;
    assert_eq!((watcher_now.pos, watcher_now.facing), (watcher_before.pos, watcher_before.facing));
    assert!(w.in_scene(owner.entity) && w.in_scene(watcher.entity));
    assert!(saved(&mut saved_rx).is_empty(), "presenting an origin must not write a story");
}

#[test]
fn an_invalid_ordinary_origin_is_refused_before_a_scene_is_published() {
    let now = Instant::now();
    for (at, facing, before, refuse) in [
        (Pos::new(f32::NAN, 0.0, 0.0), 0.0, vec![], false),
        (Pos::new(200.0, 0.0, 0.0), f32::INFINITY, vec![], false),
        (Pos::new(100.0, 0.0, 0.0), 0.0, vec![], false),
        (Pos::new(200.0, 0.0, 0.0), 0.0, vec![99], false),
        (Pos::new(200.0, 0.0, 0.0), 0.0, vec![], true),
    ] {
        let (mut w, mut saved_rx, host) = cutscene_world();
        *host.presentation_origin.lock().unwrap() = true;
        *host.presentation_origin_refuse.lock().unwrap() = refuse;
        let mut owner = seat(&mut w, "Owner", &[], now);
        owner.drain();
        let player = w.players.get_mut(&owner.entity).unwrap();
        player.snapshot.pos = at;
        player.snapshot.facing = facing;
        assert!(!w.start_cutscene(owner.entity, POINT_SCENE.into(), before, now, None));
        assert!(!w.in_scene(owner.entity));
        assert!(owner.drain().iter().all(|message| !matches!(message, ServerMsg::SceneStart { .. })));
        assert!(saved(&mut saved_rx).is_empty());
    }
}

#[test]
fn a_presentation_for_another_field_is_refused_before_source_or_pose_validation() {
    let now = Instant::now();
    let (mut w, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    let mut owner = seat(&mut w, "Owner", &[], now);
    owner.drain();
    assert!(!w.start_cutscene(owner.entity, SCENE.into(), vec![], now, None), "the yard source cannot run in the hall");
    assert!(!w.in_scene(owner.entity));
    assert!(owner.drain().iter().all(|message| !matches!(message, ServerMsg::SceneStart { .. })));
    assert!(host.presentation_origins.lock().unwrap().is_empty());
    assert!(saved(&mut saved_rx).is_empty());
}

#[test]
fn an_ordinary_scene_without_an_origin_keeps_its_explicit_null_contract() {
    let mut now = Instant::now();
    let (mut w, _, host) = cutscene_world();
    let mut owner = seat(&mut w, "Owner", &[], now);
    owner.drain();
    into_the_region(&mut w, &owner, &mut now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message,
        ServerMsg::SceneStart { presentation_origin: None, replay: None, .. })));
    assert!(host.presentation_origins.lock().unwrap().is_empty());
}

#[test]
fn an_origin_dependent_victory_tail_stays_gated_without_a_retained_battle_origin() {
    let now = Instant::now();
    let (mut w, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    *host.victory.lock().unwrap() = true;
    let mut owner = seat(&mut w, "Owner", &[], now);
    owner.drain();
    let step = w.areas.get(HALL).unwrap().runtime().story_advance(&[], 0).unwrap();
    assert!(!w.start_victory_scene(owner.entity, vec![], &step, fmv_fixture::rooms::BATTLE_FORMATION, now));
    assert!(!w.in_scene(owner.entity));
    assert!(owner.drain().iter().all(|message| !matches!(message, ServerMsg::SceneStart { .. })));
    assert!(host.presentation_origins.lock().unwrap().is_empty());
    assert!(saved(&mut saved_rx).is_empty());
}


/// Make the next intent's movement budget explicit without introducing a wall-clock wait.
fn full_move_budget(world: &mut World, owner: &Seat, now: Instant) -> f64 {
    let cap = world.areas.get(HALL).unwrap().rules().cap;
    let budget = &mut world.players.get_mut(&owner.entity).unwrap().budget;
    budget.tokens = cap;
    budget.last = now;
    cap
}

#[test]
fn a_move_across_the_whole_origin_region_stops_on_its_real_chord_before_scene_start() {
    let mut now = Instant::now();
    let (mut world, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    let from = world.players.get(&owner.entity).unwrap().snapshot.pos;
    let budget = full_move_budget(&mut world, &owner, now);
    let proposed = Pos::new(300.0, 0.0, 0.0);
    assert_eq!(world.apply_move(owner.entity, proposed, 0.75, AnimState::Run, now), MoveOutcome::Applied);
    let player = world.players.get(&owner.entity).unwrap();
    let accepted = player.snapshot.clone();
    assert!((150.0..=250.0).contains(&accepted.pos.x), "the accepted point is inside the touched region: {:?}", accepted.pos);
    assert_eq!((accepted.pos.y, accepted.pos.z), (0.0, 0.0), "clipping stays on the actual movement chord");
    assert_ne!(accepted.pos, proposed);
    assert_eq!(accepted.anim, AnimState::Idle);
    assert_eq!(accepted.facing, 0.75);
    let distance = f64::from(accepted.pos.x - from.x);
    assert!((player.budget.tokens - (budget - distance)).abs() < 0.0001, "only the accepted distance is spent");
    assert!(player.progression.is_empty());
    let messages = owner.drain();
    let correction = messages.iter().position(|message| matches!(message,
        ServerMsg::EntityUpdate { entities, .. } if entities.iter().any(|entity| entity.id == owner.entity && entity.pos == accepted.pos && entity.anim == AnimState::Idle)))
        .expect("the owner receives its accepted pose before the scene");
    let scene = messages.iter().position(|message| matches!(message,
        ServerMsg::SceneStart { presentation_origin: Some(origin), replay: None, .. }
            if origin.pos == accepted.pos && origin.facing == accepted.facing))
        .expect("the scene receives that same accepted pose");
    assert!(correction < scene);
    assert!(world.in_scene(owner.entity) && !world.in_battle(owner.entity));
    assert!(saved(&mut saved_rx).is_empty(), "the presentation has not yet paid its story point");
}

#[test]
fn an_origin_source_that_rejects_every_point_refuses_the_move_without_spending_or_publishing() {
    let mut now = Instant::now();
    let (mut world, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    let before = world.players.get(&owner.entity).unwrap().snapshot.clone();
    let budget = full_move_budget(&mut world, &owner, now);
    *host.presentation_origin_refuse.lock().unwrap() = true;
    assert_eq!(world.apply_move(owner.entity, Pos::new(300.0, 0.0, 0.0), 1.5, AnimState::Run, now),
        MoveOutcome::Rejected { corrected: before.pos, reason: crate::world::Rejection::Blocked });
    let player = world.players.get(&owner.entity).unwrap();
    assert_eq!((player.snapshot.pos, player.snapshot.facing, player.snapshot.anim), (before.pos, before.facing, before.anim));
    assert_eq!(player.budget.tokens, budget);
    assert!(player.progression.is_empty());
    assert!(!world.in_scene(owner.entity) && !world.in_battle(owner.entity));
    assert!(owner.drain().iter().all(|message| !matches!(message,
        ServerMsg::SceneStart { .. } | ServerMsg::EncounterStart { .. } | ServerMsg::StoryUpdate { .. } | ServerMsg::EntityUpdate { .. })));
    assert!(saved(&mut saved_rx).is_empty());
}

#[test]
fn an_originless_presentation_keeps_the_full_accepted_overshoot() {
    let mut now = Instant::now();
    let (mut world, mut saved_rx, host) = cutscene_world();
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    let budget = full_move_budget(&mut world, &owner, now);
    // Pass the story region's far edge (250), but not the hall gateway (300):
    // that gateway correctly takes priority over an ordinary story sweep.
    let proposed = Pos::new(280.0, 0.0, 0.0);
    assert_eq!(world.apply_move(owner.entity, proposed, 0.5, AnimState::Run, now), MoveOutcome::Applied);
    let player = world.players.get(&owner.entity).unwrap();
    assert_eq!((player.snapshot.pos, player.snapshot.anim), (proposed, AnimState::Run));
    assert_eq!(player.budget.tokens, budget - 180.0);
    assert!(player.transit.is_none() && world.in_scene(owner.entity));
    assert!(owner.drain().iter().any(|message| matches!(message,
        ServerMsg::SceneStart { presentation_origin: None, replay: None, .. })));
    assert!(host.presentation_origins.lock().unwrap().is_empty());
    assert!(saved(&mut saved_rx).is_empty());
}

#[test]
fn a_player_who_cannot_advance_the_phase_is_not_clipped_to_its_leaders_story_point() {
    let mut now = Instant::now();
    let (mut world, mut saved_rx, host) = cutscene_world();
    *host.presentation_origin.lock().unwrap() = true;
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    full_move_budget(&mut world, &owner, now);
    world.players.get_mut(&owner.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
    let proposed = Pos::new(300.0, 0.0, 0.0);
    assert_eq!(world.apply_move(owner.entity, proposed, 0.0, AnimState::Run, now), MoveOutcome::Applied);
    assert_eq!(world.players.get(&owner.entity).unwrap().snapshot.pos, proposed);
    assert!(!world.in_scene(owner.entity) && !world.in_battle(owner.entity));
    assert!(host.presentation_origins.lock().unwrap().is_empty());
    assert!(owner.drain().iter().all(|message| !matches!(message, ServerMsg::SceneStart { .. })));
    assert!(saved(&mut saved_rx).is_empty());
}

struct OriginBattleArea(Box<dyn fmv_adapter_api::AreaRuntime>);

impl fmv_adapter_api::AreaRuntime for OriginBattleArea {
    fn info(&self) -> &fmv_adapter_api::AreaInfo { self.0.info() }
    fn spawn_points(&self) -> Vec<Pos> { self.0.spawn_points() }
    fn validate_move(&self, from: Pos, to: Pos) -> fmv_adapter_api::MoveResult { self.0.validate_move(from, to) }
    fn bounds(&self) -> fmv_adapter_api::Bounds { self.0.bounds() }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.0.walk_triangles() }
    fn max_speed(&self) -> f32 { self.0.max_speed() }
    fn phase_key(&self, progress: &[u8]) -> Option<fmv_core::PhaseKey> { self.0.phase_key(progress) }
    fn story_points(&self) -> Vec<fmv_adapter_api::StoryPoint> { self.0.story_points() }
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<fmv_adapter_api::StoryStep> { self.0.story_advance(progress, point) }
    fn story_battle(&self, point: u32) -> Option<u16> { (point == 0).then_some(fmv_fixture::rooms::BATTLE_FORMATION) }
}

#[test]
fn a_clipped_guard_presentation_still_starts_its_battle_only_after_the_scene_ends() {
    let mut now = Instant::now();
    let area = OriginBattleArea(RoomsAdapter::with_story().load_area(HALL).unwrap());
    let mut world = World::with_party_config(Box::new(area), PartyConfig::default()).unwrap();
    let (handle, mut saved_rx) = SaverHandle::channel();
    world.set_progress_saver(handle);
    let host = Arc::new(FakeHost::default());
    *host.cutscene.lock().unwrap() = true;
    *host.presentation_origin.lock().unwrap() = true;
    world.configure_scenes(Some(SceneSettings { host, debug_preview: false, debug_new_story: None }));
    world.configure_battle(Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000, fixed_seed: Some(7),
        ..BattleSettings::new(Arc::new(SyntheticRules::default())) }), false);
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    full_move_budget(&mut world, &owner, now);
    assert_eq!(world.apply_move(owner.entity, Pos::new(300.0, 0.0, 0.0), 0.0, AnimState::Run, now), MoveOutcome::Applied);
    let messages = owner.drain();
    let run = starts(&messages).first().map(|start| start.0).expect("the guard presentation starts first");
    assert!(world.in_scene(owner.entity) && !world.in_battle(owner.entity));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::EncounterStart { .. })));
    assert!(saved(&mut saved_rx).is_empty());
    world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(!world.in_scene(owner.entity) && world.in_battle(owner.entity));
    assert!(owner.drain().iter().any(|message| matches!(message,
        ServerMsg::EncounterStart { formation_id, .. } if *formation_id == fmv_fixture::rooms::BATTLE_FORMATION)));
    assert!(world.players.get(&owner.entity).unwrap().progression.is_empty(), "the step still waits for victory");
    assert!(saved(&mut saved_rx).is_empty());
}

/// A source with integer field coordinates: the polygon edge is geometrically valid,
/// but a rounded x below 151 belongs to the neighboring source triangle.
struct RoundedOriginHost {
    inner: Arc<FakeHost>,
    seen: Mutex<Vec<SceneOrigin>>,
}

impl SceneHost for RoundedOriginHost {
    fn scenes(&self) -> Vec<SceneInfo> { self.inner.scenes() }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { self.inner.offered(scene, progress) }
    fn point_scene(&self, field: &str, before: &[u8], after: &[u8]) -> Option<String> {
        self.inner.point_scene(field, before, after)
    }
    fn requires_presentation_origin(&self, scene: &str) -> bool { self.inner.requires_presentation_origin(scene) }
    fn presentation_origin_valid(&self, scene: &str, field: &str, progress: &[u8], origin: &SceneOrigin) -> bool {
        self.seen.lock().unwrap().push(*origin);
        self.inner.presentation_origin_valid(scene, field, progress, origin) && origin.pos.x.round() >= 151.0
    }
    fn trigger(&self, scene: &str, progress: &[u8]) -> Option<TriggerInfo> { self.inner.trigger(scene, progress) }
    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>> { self.inner.preview_progress(scene) }
    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.inner.finish(scene, progress, seed, answers)
    }
}

#[test]
fn an_integer_source_rejects_the_boundary_then_accepts_an_inset_on_the_same_chord() {
    let mut now = Instant::now();
    let (mut world, mut saved_rx, inner) = cutscene_world();
    *inner.presentation_origin.lock().unwrap() = true;
    let host = Arc::new(RoundedOriginHost { inner, seen: Mutex::new(Vec::new()) });
    world.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: false, debug_new_story: None }));
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    full_move_budget(&mut world, &owner, now);
    assert_eq!(world.apply_move(owner.entity, Pos::new(300.0, 0.0, 0.0), 0.25, AnimState::Run, now), MoveOutcome::Applied);
    let accepted = world.players.get(&owner.entity).unwrap().snapshot.pos;
    assert_eq!(accepted, Pos::new(151.0, 0.0, 0.0), "one unit along the actual crossed interval, not the region centroid");
    let seen = host.seen.lock().unwrap();
    assert!(seen.len() >= 2);
    assert_eq!(seen[0].pos, Pos::new(150.0, 0.0, 0.0), "the original geometric boundary was tried first");
    assert_eq!(seen[1].pos, accepted, "source validation then accepted the one-unit inset");
    assert!(seen.iter().all(|origin| origin.triangle.is_none()), "the world cannot manufacture a triangle hint");
    assert!(owner.drain().iter().any(|message| matches!(message,
        ServerMsg::SceneStart { presentation_origin: Some(origin), replay: None, .. }
            if origin.pos == accepted && origin.facing == 0.25 && origin.triangle.is_none())));
    assert!(saved(&mut saved_rx).is_empty());
}

#[test]
fn a_grazing_chord_without_an_integer_source_pose_never_falls_through_to_the_battle() {
    let mut now = Instant::now();
    let area = OriginBattleArea(RoomsAdapter::with_story().load_area(HALL).unwrap());
    let mut world = World::with_party_config(Box::new(area), PartyConfig::default()).unwrap();
    let (handle, mut saved_rx) = SaverHandle::channel();
    world.set_progress_saver(handle);
    let inner = Arc::new(FakeHost::default());
    *inner.cutscene.lock().unwrap() = true;
    *inner.presentation_origin.lock().unwrap() = true;
    let host = Arc::new(RoundedOriginHost { inner, seen: Mutex::new(Vec::new()) });
    world.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: false, debug_new_story: None }));
    world.configure_battle(Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000, fixed_seed: Some(7),
        ..BattleSettings::new(Arc::new(SyntheticRules::default())) }), false);
    let mut owner = seat(&mut world, "Owner", &[], now);
    assert_eq!(go(&mut world, &owner, 100.0, 0.0, &mut now), MoveOutcome::Applied);
    owner.drain();
    let budget = full_move_budget(&mut world, &owner, now);
    assert_eq!(world.apply_move(owner.entity, Pos::new(150.2, 0.0, 0.0), 0.0, AnimState::Run, now),
        MoveOutcome::Rejected { corrected: Pos::new(100.0, 0.0, 0.0), reason: crate::world::Rejection::Blocked });
    let player = world.players.get(&owner.entity).unwrap();
    assert_eq!(player.snapshot.pos, Pos::new(100.0, 0.0, 0.0));
    assert_eq!(player.budget.tokens, budget);
    assert!(player.progression.is_empty());
    assert!(!world.in_scene(owner.entity) && !world.in_battle(owner.entity));
    let seen = host.seen.lock().unwrap();
    assert!(!seen.is_empty() && seen.iter().all(|origin| origin.pos.x.round() == 150.0));
    assert!(owner.drain().iter().all(|message| !matches!(message,
        ServerMsg::SceneStart { .. } | ServerMsg::EncounterStart { .. } | ServerMsg::StoryUpdate { .. })));
    assert!(saved(&mut saved_rx).is_empty());
}
