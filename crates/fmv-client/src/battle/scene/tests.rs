//! Tests of the scene as a whole: placement maths, building from a battle, what a frame holds,
//! how events play, the overlay, and when the window gets a scene at all. No GPU and no game
//! files: the stand-in characters are stick figures ([`crate::avatar::library::tests`]) and the
//! rules are the synthetic ones or a hand-made engine.

use super::*;
use crate::avatar::library::tests::{character, Counting};
use crate::battle::scene::model::{BattleActorModel, BattleModelSource, StandInModels};
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId};
use fmv_ff7::model::transform_point;
use fmv_ff7_battle::rules::{BattleConfig, Command, StartMode, Target};
use fmv_protocol::battle::{CameraPose, EnemyPlacement};
use fmv_protocol::{BattleRules, BattleSetup, ServerMsg, SyntheticRules};
use std::cell::RefCell;
use std::rc::Rc;

const W: u32 = 1100;
const H: u32 = 680;

fn kit() -> ModelKit {
    ModelKit::new(Box::new(StandInModels), Some(Box::new(Counting::new(Rc::new(RefCell::new(Vec::new())), vec![]))))
}

fn member(i: u64, character: u8) -> BattleMember {
    BattleMember { entity: EntityId(1 + i), account: AccountId(1 + i), character, level: 5, weapon: None, armor: None, limit: 0 }
}

fn setup_for(formation: u16, party: usize) -> BattleSetup {
    BattleSetup {
        formation_id: formation,
        seed: 7,
        party: (0..party as u64).map(|i| member(i, i as u8)).collect(),
        preemptive: false,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
    }
}

/// The engine of `formation` (1: two dummies) for `party` members.
fn engine(formation: u16, party: usize) -> BattleState {
    SyntheticRules::default().build(&setup_for(formation, party)).unwrap()
}

/// A formation of two enemies seen from a camera on the positive-x side (made-up numbers).
fn formation() -> FormationScene {
    FormationScene {
        location: 0,
        layout: fmv_ff7_battle::scene::BattleLayout::Normal,
        camera: CameraPose { position: [9000, -2400, 1800], target: [0, -300, 100] },
        enemies: vec![
            EnemyPlacement { enemy_id: 3, position: [-600, 0, -1400], row: 1 },
            EnemyPlacement { enemy_id: 5, position: [550, 0, -1800], row: 1 },
        ],
    }
}

fn start(party: usize, formation: Option<FormationScene>) -> SceneStart {
    SceneStart { id: BattleId(1), party: (0..party as u64).map(|i| member(i, i as u8)).collect(), my_actor: Some(ActorId(0)), formation }
}

fn scene(party: usize, with_formation: bool) -> (BattleScene, BattleState) {
    let engine = engine(1, party);
    let scene = BattleScene::new(&start(party, with_formation.then(formation)), Some(&engine), &[], &mut kit()).expect("a scene");
    (scene, engine)
}

fn view(scene: &BattleScene) -> View {
    scene.view(W, H)
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

// ----- placement ------------------------------------------------------------------------------

/// The stick figure's nose is a small triangle at head height pointing forward (native `-z`):
/// after placement facing a target, the nose is nearer the target than the body.
#[test]
fn a_model_placed_facing_a_target_has_its_nose_pointing_at_it() {
    let cloud = character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3);
    let pose = cloud.pose(AnimState::Idle, 0).unwrap();
    let nose_native = transform_point(&pose[0], [0.0, -22.0, -2.5]);
    let body_native = transform_point(&pose[0], [0.0, -22.0, 0.0]);
    for target in [[0.0f32, 4000.0], [4000.0, 0.0], [0.0, -4000.0], [-3000.0, 1000.0], [2500.0, -2500.0]] {
        let from = [300.0f32, 200.0];
        let yaw = layout::yaw_towards(from, target).unwrap();
        let place = placement([from[0], 0.0, from[1]], yaw, [2.0, 2.0, 2.0], 0.0);
        let (nose, body) = (transform_point(&place, nose_native), transform_point(&place, body_native));
        let toward = [target[0] - from[0], target[1] - from[1]];
        let len = toward[0].hypot(toward[1]);
        let ahead = ((nose[0] - body[0]) * toward[0] + (nose[2] - body[2]) * toward[1]) / len;
        let across = ((nose[0] - body[0]) * toward[1] - (nose[2] - body[2]) * toward[0]) / len;
        assert!(near(ahead, 5.0, 1e-3), "the nose is 2.5 model units (5 battle units) ahead of the body towards {target:?}: {ahead}");
        assert!(across.abs() < 1e-3, "and not to either side: {across}");
        // It stands where it was put, upright (head above feet; up is negative y), scaled.
        let feet = transform_point(&place, transform_point(&pose[0], [0.0, 5.0, 0.0]));
        assert!(near(feet[0], from[0], 1e-3) && near(feet[1], 0.0, 1e-3) && near(feet[2], from[1], 1e-3), "{feet:?}");
        let head = transform_point(&place, transform_point(&pose[0], [0.0, -25.0, 0.0]));
        assert!(near(head[1], -60.0, 1e-3), "30 model units tall, scaled by 2: {head:?}");
    }
}

#[test]
fn a_model_that_falls_lies_on_its_back_behind_where_it_stood() {
    let facing = layout::yaw_towards([0.0, 0.0], [0.0, -1000.0]).unwrap();
    for fall in [0.0f32, 0.5, 1.0] {
        let place = placement([100.0, 0.0, 50.0], facing, [1.0, 1.0, 1.0], fall);
        // The top of a model one unit tall (native y = -1, above its feet).
        let head = transform_point(&place, [0.0, -1.0, 0.0]);
        let angle = fall * std::f32::consts::FRAC_PI_2;
        // It faces -z, so behind it is +z: the head goes back and down to the ground.
        assert!(near(head[1], -angle.cos(), 1e-4), "fall {fall}: {head:?}");
        assert!(near(head[2] - 50.0, angle.sin(), 1e-4), "fall {fall}: {head:?}");
        assert!(near(head[0], 100.0, 1e-4));
    }
    // Falling is clamped.
    let way_over = placement([0.0; 3], 0.0, [1.0; 3], 7.0);
    let lying = placement([0.0; 3], 0.0, [1.0; 3], 1.0);
    for (a, b) in way_over.iter().zip(&lying) {
        assert!(near(*a, *b, 1e-6));
    }
}

// ----- building a scene -----------------------------------------------------------------------

#[test]
fn a_scene_has_the_party_first_then_the_enemies_with_models_to_match() {
    let (scene, engine) = scene(3, true);
    assert_eq!(scene.actor_count(), 5);
    assert_eq!((engine.party_ids().len(), engine.enemy_ids().len()), (3, 2));
    let sides: Vec<Side> = scene.actors.iter().map(|a| a.side).collect();
    assert_eq!(sides, [Side::Party, Side::Party, Side::Party, Side::Enemy, Side::Enemy]);
    // The party are the characters' field models (scaled up); the enemies are boxes (battle units).
    for a in &scene.actors[..3] {
        assert_eq!(a.rig.scale, model::FIELD_TO_BATTLE_SCALE);
    }
    for a in &scene.actors[3..] {
        assert_eq!(a.rig.scale, 1.0);
        assert!(a.rig.height >= 800.0);
    }
    // The enemies stand where the formation says, the party opposite.
    assert_eq!(scene.actors[3].home.pos, [-600.0, 0.0, -1400.0]);
    assert_eq!(scene.actors[4].home.pos, [550.0, 0.0, -1800.0]);
    assert!(scene.actors[..3].iter().all(|a| a.home.pos[2] > 1000.0));
    assert_eq!(scene.id(), BattleId(1));
}

#[test]
fn the_look_of_a_player_decides_its_model_and_without_one_the_character_does() {
    // Looks aligned with the party: the first member's player chose Barret, the third has none.
    let engine = engine(1, 3);
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut kit = ModelKit::new(Box::new(StandInModels), Some(Box::new(Counting::new(Rc::clone(&asked), vec![]))));
    let looks = [Some("ff7/barret".to_string()), None, None];
    BattleScene::new(&start(3, None), Some(&engine), &looks, &mut kit).unwrap();
    // Member 0: the look (Barret). Member 1: character id 1 is Barret too (already loaded). Member 2: character 2, Tifa.
    assert_eq!(*asked.borrow(), ["ff7/barret", "ff7/tifa"]);
}

#[test]
fn without_an_engine_there_is_no_scene() {
    assert!(BattleScene::new(&start(2, None), None, &[], &mut kit()).is_none());
}

#[test]
fn a_scene_without_placement_data_uses_the_default_layout_and_view() {
    let (scene, _) = scene(2, false);
    let view = view(&scene);
    // Enemies on the negative-z side in a row, the party opposite; everyone is in the window.
    let anchors = scene.screen_anchors(&view);
    assert_eq!(anchors.len(), 4);
    for a in &anchors {
        let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
        assert!(feet[0] > 0.0 && feet[0] < W as f32 && head[1] > 0.0 && feet[1] < H as f32, "{a:?}");
        assert!(head[1] < feet[1], "heads are above feet: {a:?}");
    }
}

/// The composition of a normal battle through the formation's own camera: enemies on the left of
/// the window, the party on the right, everyone inside the margins, the bottom left free.
#[test]
fn enemies_land_on_the_left_the_party_on_the_right_and_everyone_inside_the_margins() {
    let (scene, _) = scene(3, true);
    for (w, h) in [(1100u32, 680u32), (640, 480), (1920, 1080)] {
        let view = scene.view(w, h);
        let anchors = scene.screen_anchors(&view);
        let x_of = |side: Side| -> Vec<f32> { anchors.iter().filter(|a| a.side == side).map(|a| a.feet.unwrap()[0]).collect() };
        let (enemies, party) = (x_of(Side::Enemy), x_of(Side::Party));
        assert!(enemies.iter().all(|e| party.iter().all(|p| e < p)), "{w}x{h}: enemies left of the party: {enemies:?} {party:?}");
        for a in &anchors {
            let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
            assert!(feet[0] >= w as f32 * 0.05 && feet[0] <= w as f32 * 0.95, "{w}x{h}: {a:?}");
            assert!(head[1] >= 0.0 && feet[1] <= h as f32 * 0.80, "{w}x{h}: the bottom is left free: {a:?}");
        }
    }
}

#[test]
fn a_formation_camera_that_does_not_suit_the_cast_falls_back_to_the_default_view() {
    let mut f = formation();
    // Looking away from everyone.
    f.camera = CameraPose { position: [9000, -2400, 1800], target: [20_000, -300, 1800] };
    let engine = engine(1, 3);
    let scene = BattleScene::new(&start(3, Some(f)), Some(&engine), &[], &mut kit()).unwrap();
    assert_eq!(scene.cameras.len(), 3, "the formation's, the default and the one of last resort");
    let view = scene.view(W, H);
    let default = BattleCamera::look_at(DEFAULT_POSE.0, DEFAULT_POSE.1).unwrap();
    assert_eq!(view.camera, default, "the default camera shows the cast when the formation's does not");
    for a in scene.screen_anchors(&view) {
        assert!(a.feet.is_some() && a.head.is_some(), "{a:?}");
    }
    // A camera that is not a camera at all (no direction) is the default as well.
    let mut g = formation();
    g.camera = CameraPose { position: [100, 100, 100], target: [100, 100, 100] };
    let scene = BattleScene::new(&start(3, Some(g)), Some(&engine), &[], &mut kit()).unwrap();
    assert_eq!(scene.cameras.len(), 2);
    assert_eq!(scene.view(W, H).camera, default);
}

// ----- frames ---------------------------------------------------------------------------------

#[test]
fn a_frame_has_a_stage_every_model_a_shadow_each_and_a_ring_for_the_ready_ones() {
    let (mut scene, _) = scene(2, true);
    let view = view(&scene);
    let frame = scene.frame(&view, false);
    assert!(!frame.stage.is_empty());
    assert_eq!(frame.models.models.len(), 4, "everyone is drawn");
    // Skinned positions are in camera space, in front of the camera, one per mesh vertex.
    for m in &frame.models.models {
        assert_eq!(m.positions.len(), m.asset.mesh.positions.len());
        assert!(m.positions.iter().all(|p| p.iter().all(|c| c.is_finite()) && p[2] > 1000.0), "in front of the camera");
    }
    assert_eq!(frame.models.zoom, view.zoom as f32);
    assert_eq!(frame.clear_rgb, stage::HORIZON);
    // Shadows: one disc per actor, plus our own white outline (nobody is ready: no gold rings yet).
    let shadows = frame.models.ground.iter().filter(|s| s.inner.is_empty()).count();
    let bands = frame.models.ground.iter().filter(|s| !s.inner.is_empty()).count();
    assert_eq!((shadows, bands), (4, 1), "the one band is the white ring of our own character");
    // Make our character ready (a preemptive engine) and tell the scene: a gold ring appears.
    let party = vec![member(0, 0), member(1, 1)];
    let setup = BattleSetup { preemptive: true, party, ..setup_for(1, 2) };
    let engine = SyntheticRules::default().build(&setup).unwrap();
    scene.update(1.0, &[], Some(&engine));
    let ready = scene.frame(&view, false);
    let gold = ready.models.ground.iter().filter(|s| s.rgb == [255, 214, 64]).count();
    assert_eq!(gold, 2, "both party members are ready");
    // After the battle (no engine) nobody is.
    scene.update(2.0, &[], None);
    assert!(scene.frame(&view, false).models.ground.iter().all(|s| s.rgb != [255, 214, 64]));
    // Marks are ordered far to near, so that a near one is drawn over a far one.
    let depths: Vec<f32> = ready.models.ground.iter().map(|s| s.depth).collect();
    assert!(depths.windows(2).all(|d| d[0] >= d[1]), "{depths:?}");
}

#[test]
fn the_ground_marks_sit_under_the_feet_and_the_shadow_is_round() {
    let (scene, _) = scene(1, true);
    let view = view(&scene);
    let frame = scene.frame(&view, false);
    let screen = scene.screen_anchors(&view);
    let shadows: Vec<&GroundShape> = frame.models.ground.iter().filter(|s| s.inner.is_empty()).collect();
    assert_eq!(shadows.len(), screen.len());
    for shape in shadows {
        // The shadow's outline is centred (in the pinhole space the GPU camera places) on someone's feet.
        let n = shape.outer.len() as f32;
        let (cx, cy) = shape.outer.iter().fold((0.0, 0.0), |a, p| (a.0 + p[0], a.1 + p[1]));
        let gpu = frame.camera.project(cx / n, cy / n);
        let pixel = [W as f32 * 0.5 + gpu[0], H as f32 * 0.5 - gpu[1]];
        let nearest = screen.iter().map(|a| a.feet.unwrap()).map(|f| (pixel[0] - f[0]).hypot(pixel[1] - f[1])).fold(f32::MAX, f32::min);
        assert!(nearest < 12.0, "{pixel:?} is {nearest} px from the nearest feet");
        // It is wider than tall on the screen (a circle on the ground seen from above at a slant).
        let (xs, ys) = (shape.outer.iter().map(|p| p[0]), shape.outer.iter().map(|p| p[1]));
        let (w, h) = (xs.clone().fold(f32::MIN, f32::max) - xs.fold(f32::MAX, f32::min), ys.clone().fold(f32::MIN, f32::max) - ys.fold(f32::MAX, f32::min));
        assert!(w > h * 1.5, "{w} x {h}");
    }
}

#[test]
fn an_srgb_target_changes_only_the_colours() {
    let (scene, _) = scene(2, true);
    let view = view(&scene);
    let (plain, srgb) = (scene.frame(&view, false), scene.frame(&view, true));
    assert_eq!(plain.stage.len(), srgb.stage.len());
    assert_eq!(plain.models.models.len(), srgb.models.models.len());
    for (a, b) in plain.stage.iter().zip(&srgb.stage) {
        assert_eq!(a.pos, b.pos);
    }
}

// ----- events -----------------------------------------------------------------------------------

/// A real fight between a hand-made party and enemy with the engine's own events: the enemy has one
/// hit point, so a hit kills it. Returns the engine after the blow and the events it produced.
fn killing_blow() -> (BattleState, Vec<BattleEvent>, ActorId, ActorId) {
    for seed in 0..64 {
        let mut hero = fmv_ff7_battle::rules::Combatant::new("Hero", 10, 500);
        hero.attack = 80;
        hero.dex = 60;
        let mut foe = fmv_ff7_battle::rules::Combatant::new("Foe", 3, 1);
        foe.dex = 10;
        let config = BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() };
        let mut state = BattleState::with_config(seed, vec![hero.clone(), hero], vec![foe.clone(), foe], config);
        let (attacker, target) = (ActorId(0), state.enemy_ids()[0]);
        let mut events = state.step_ticks(1);
        if let Ok(more) = state.submit_command(attacker, Command::Attack { target }) {
            events.extend(more);
        }
        if events.iter().any(|e| matches!(e, BattleEvent::Death { actor } if *actor == target)) {
            return (state, events, attacker, target);
        }
    }
    panic!("no seed in 64 made the hero hit");
}

#[test]
fn a_blow_makes_the_attacker_lunge_and_the_target_blink_and_collapse() {
    let (state, events, attacker, target) = killing_blow();
    let engine_before = SyntheticRules::default().build(&setup_for(1, 2)).unwrap();
    // A scene for the engine's actors (2 party, 2 enemies), started before the blow.
    let mut scene = BattleScene::new(&start(2, Some(formation())), Some(&engine_before), &[], &mut kit()).unwrap();
    let view = view(&scene);
    let home = scene.screen_anchors(&view)[usize::from(attacker.0)].feet.unwrap();
    scene.update(10.0, &events, Some(&state));

    let (mut lunged, mut hidden) = (0.0f32, 0);
    let mut t = 10.0;
    while t < 13.0 {
        scene.update(t, &[], Some(&state));
        let anchors = scene.screen_anchors(&view);
        let feet = anchors[usize::from(attacker.0)].feet.unwrap();
        lunged = lunged.max((feet[0] - home[0]).hypot(feet[1] - home[1]));
        hidden += usize::from(!anchors[usize::from(target.0)].drawn);
        t += 0.01;
    }
    assert!(lunged > 60.0, "the attacker ran at its target: {lunged} px");
    assert!(hidden > 10, "the target blinked ({hidden} dark frames)");
    // Long after, the dead enemy is not drawn; the attacker is home again.
    scene.update(20.0, &[], Some(&state));
    let anchors = scene.screen_anchors(&view);
    assert!(!anchors[usize::from(target.0)].drawn, "the enemy that died is gone");
    let back = anchors[usize::from(attacker.0)].feet.unwrap();
    // (Up to the idle sway, which lifts and lowers it a little.)
    assert!((back[0] - home[0]).abs() < 1.0 && (back[1] - home[1]).abs() < 4.0, "home again: {back:?} vs {home:?}");
    // The frame leaves it out: three of the four are drawn, and it has no shadow either.
    let frame = scene.frame(&view, false);
    assert_eq!(frame.models.models.len(), 3);
    assert_eq!(frame.models.ground.iter().filter(|s| s.inner.is_empty()).count(), 3);
}

#[test]
fn events_that_arrive_after_the_battle_ended_are_still_played() {
    // The killing blow and the end of the battle arrive together: the engine is gone (None) by the
    // time the scene is updated, but the events still play.
    let (state, events, _, target) = killing_blow();
    let engine_before = SyntheticRules::default().build(&setup_for(1, 2)).unwrap();
    let mut scene = BattleScene::new(&start(2, Some(formation())), Some(&engine_before), &[], &mut kit()).unwrap();
    let view = view(&scene);
    drop(state);
    scene.update(5.0, &events, None);
    let mut dark = 0;
    let mut t = 5.0;
    while t < 8.0 {
        scene.update(t, &[], None);
        dark += usize::from(!scene.screen_anchors(&view)[usize::from(target.0)].drawn);
        t += 0.01;
    }
    assert!(dark > 10, "the target still blinked and collapsed without an engine ({dark})");
    // Death is played from the event alone: the enemy is gone at the end.
    assert!(!scene.screen_anchors(&view)[usize::from(target.0)].drawn);
}

#[test]
fn a_scene_that_starts_after_an_enemy_died_does_not_draw_it() {
    let (state, _, _, target) = killing_blow();
    // The scene is built from the engine as it stands now: one enemy is already dead.
    let scene = BattleScene::new(&start(2, None), Some(&state), &[], &mut kit()).unwrap();
    let mut scene = scene;
    scene.update(1.0, &[], Some(&state));
    let view = view(&scene);
    assert!(!scene.screen_anchors(&view)[usize::from(target.0)].drawn);
    assert_eq!(scene.frame(&view, false).models.models.len(), 3);
}

#[test]
fn a_miss_makes_the_target_step_aside_and_a_clock_that_goes_wrong_does_not_break_the_scene() {
    let (mut scene, engine) = scene(2, true);
    let view = view(&scene);
    let home = scene.screen_anchors(&view)[2].feet.unwrap();
    let miss = [BattleEvent::Miss { actor: ActorId(0), target: ActorId(2) }];
    scene.update(3.0, &miss, Some(&engine));
    let (mut stepped, mut ever_hidden) = (0.0f32, false);
    let mut t = 3.0;
    while t < 5.0 {
        scene.update(t, &[], Some(&engine));
        let target = scene.screen_anchors(&view)[2];
        let feet = target.feet.unwrap();
        stepped = stepped.max((feet[0] - home[0]).hypot(feet[1] - home[1]));
        ever_hidden |= !target.drawn;
        t += 0.02;
    }
    assert!(stepped > 20.0, "the target stepped aside: {stepped} px");
    assert!(!ever_hidden, "and was never hit (no blinking)");
    // Time that is not a time leaves the clock where it was.
    let before = scene.now;
    scene.update(f64::NAN, &[], Some(&engine));
    assert_eq!(scene.now, before);
    // Events about actors the scene does not have change nothing.
    scene.update(6.0, &[BattleEvent::Death { actor: ActorId(99) }, BattleEvent::Damage { actor: ActorId(98), target: ActorId(0), amount: 1, critical: false }], Some(&engine));
    assert_eq!(scene.frame(&view, false).models.models.len(), 4);
}

// ----- protocol v7: limit breaks and escape -------------------------------------------------------

#[test]
fn a_limit_break_never_runs_up_and_takes_the_colour_of_what_it_does_and_running_is_no_blow() {
    // Party of four: Cloud (actor 0) strikes with his limit, Aeris (actor 3) heals with hers.
    let engine = engine(1, 4);
    let plain = Look { melee: false, element: Element::Plain };
    assert_eq!(look_in(Some(&engine), ActorId(0), ActionRef::Limit(128)), plain);
    assert_eq!(look_in(Some(&engine), ActorId(3), ActionRef::Limit(142)), Look { melee: false, element: Element::Cure });
    assert_eq!(look_in(Some(&engine), ActorId(0), ActionRef::Escape), plain);
    // The id the server names is not looked up: the performer's own limit decides (a lie changes nothing).
    assert_eq!(look_in(Some(&engine), ActorId(3), ActionRef::Limit(7)), Look { melee: false, element: Element::Cure });
    // Without the engine, or for somebody who is not there, it is a plain blow like any other action.
    assert_eq!(look_in(None, ActorId(0), ActionRef::Limit(128)), Look::PLAIN);
    assert_eq!(look_in(Some(&engine), ActorId(99), ActionRef::Escape), Look::PLAIN);
}

/// A preemptive battle of two heroes against one foe, the engine before anything and the events of a
/// command of the first hero (after the announcement of the full gauges).
fn command_of_the_first_hero(limit: u16, command: impl Fn(&BattleState) -> Command) -> (BattleState, BattleState, Vec<BattleEvent>) {
    let mut setup = setup_for(3, 2);
    setup.preemptive = true;
    setup.party[0].limit = limit;
    let before = SyntheticRules::default().build(&setup).unwrap();
    let mut engine = before.clone();
    let mut events = engine.step_ticks(1);
    let command = command(&engine);
    events.extend(engine.submit_command(ActorId(0), command).expect("the first hero can act"));
    (before, engine, events)
}

#[test]
fn a_limit_break_from_the_engine_gathers_at_the_performer_and_breaks_over_the_target_late_in_one_burst_pair() {
    let (before, engine, events) = command_of_the_first_hero(255, |e| Command::Limit { target: Target::One(e.enemy_ids()[0]) });
    let target = engine.enemy_ids()[0];
    assert!(events.contains(&BattleEvent::Action { actor: ActorId(0), action: ActionRef::Limit(128) }), "{events:?}");
    assert!(events.contains(&BattleEvent::LimitGauge { actor: ActorId(0), value: 0 }), "the gauge is emptied");
    let mut scene = BattleScene::new(&start(2, None), Some(&before), &[], &mut kit()).unwrap();
    scene.update(10.0, &events, Some(&engine));
    // One glow at the performer, a bloom and a star over the target, both at the cap of the delay.
    let charges = scene.director.charges().to_vec();
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0].caster, 0);
    let bursts = scene.director.bursts().to_vec();
    assert!(bursts.len() >= 2 && bursts.iter().all(|b| b.target == usize::from(target.0)), "{bursts:?}");
    assert!(bursts.iter().all(|b| b.at > 10.0 + 0.5), "a long show: nothing breaks at once ({bursts:?})");
    // Nobody left: the party is drawn all the time, and after the show the enemy took the damage as numbers.
    let view = view(&scene);
    let mut t = 10.0;
    while t < 14.0 {
        scene.update(t, &[], Some(&engine));
        assert!(scene.screen_anchors(&view)[0].drawn && scene.screen_anchors(&view)[1].drawn, "at {t}");
        t += 0.05;
    }
}

#[test]
fn a_party_that_runs_away_turns_its_back_leaves_the_stage_and_shows_no_victory_while_the_enemy_stays() {
    let (before, engine, events) = command_of_the_first_hero(0, |_| Command::Escape);
    assert_eq!(engine.outcome(), Some(fmv_ff7_battle::rules::Outcome::Escaped), "a preemptive start gets away for sure");
    let mut scene = BattleScene::new(&start(2, None), Some(&before), &[], &mut kit()).unwrap();
    let view = view(&scene);
    let home: Vec<Option<[f32; 2]>> = scene.screen_anchors(&view).iter().map(|a| a.feet).collect();
    // The battle is over by the time the scene hears of it (no engine), and the run plays all the same.
    scene.update(10.0, &events, None);
    scene.update(10.8, &[], None);
    let mid = scene.screen_anchors(&view);
    let moved = |i: usize, now: &[ActorScreen]| match (home[i], now[i].feet) {
        (Some(a), Some(b)) => (a[0] - b[0]).hypot(a[1] - b[1]) > 20.0,
        _ => true,
    };
    assert!(moved(0, &mid) && moved(1, &mid), "both heroes are on their way: {mid:?} from {home:?}");
    assert!(!moved(2, &mid), "the enemy stays where it is");
    // Long after, the heroes are not drawn at all (no shadows, no models) and nothing stands up to cheer.
    scene.update(20.0, &[], None);
    let later = scene.screen_anchors(&view);
    assert!(!later[0].drawn && !later[1].drawn && later[2].drawn, "{later:?}");
    let frame = scene.frame(&view, false);
    assert_eq!(frame.models.models.len(), 1, "only the enemy is left on the stage");
    assert_eq!(frame.models.ground.iter().filter(|s| s.inner.is_empty()).count(), 1);
}

#[test]
fn an_escape_that_fails_throws_the_runner_back_and_nobody_leaves() {
    let mut setup = setup_for(4, 2);
    setup.preemptive = false;
    let before = SyntheticRules::default().build(&setup).unwrap();
    let failed = [BattleEvent::Action { actor: ActorId(0), action: ActionRef::Escape }, BattleEvent::EscapeFailed { actor: ActorId(0) }];
    // How far (px) the first hero strays from its mark in two seconds after `events`, and where it ends.
    let sweep = |events: &[BattleEvent]| {
        let mut scene = BattleScene::new(&start(2, None), Some(&before), &[], &mut kit()).unwrap();
        let view = view(&scene);
        let home = scene.screen_anchors(&view)[0].feet.unwrap();
        scene.update(10.0, events, Some(&before));
        let mut farthest = 0.0f32;
        let mut t = 10.0;
        while t < 12.0 {
            scene.update(t, &[], Some(&before));
            let anchors = scene.screen_anchors(&view);
            assert!(anchors.iter().all(|a| a.drawn), "nobody leaves, at {t}");
            if let Some(feet) = anchors[0].feet {
                farthest = farthest.max((feet[0] - home[0]).hypot(feet[1] - home[1]));
            }
            t += 0.02;
        }
        (farthest, scene.screen_anchors(&view)[0].feet.unwrap(), home)
    };
    let (idle, _, _) = sweep(&[]);
    let (thrown, back, home) = sweep(&failed);
    assert!(thrown > idle + 8.0, "the runner is thrown back well beyond the idle sway: {thrown} px against {idle} px");
    // And it is home again afterwards (up to the idle sway).
    assert!((back[0] - home[0]).abs() < 1.0 && (back[1] - home[1]).abs() < 4.0, "home again: {back:?} vs {home:?}");
}

// ----- the overlay: the scene only says where the heads are ---------------------------------------

#[test]
fn the_heads_are_given_in_the_order_of_the_views_bars_so_the_overlay_finds_each_fighter() {
    use crate::battle::view::Anchors;
    let s = state_in_battle();
    let battle = s.battle.view(1.0).unwrap();
    let mut slot = None;
    let mut kit_slot = None;
    let scene = scene_for(&mut slot, &mut kit_slot, &s, &battle, 1.0, &[]).unwrap();
    let view = scene.view(W, H);
    let anchors = scene.head_anchors(&view);
    assert_eq!((anchors.party.len(), anchors.enemies.len()), (battle.party.len(), battle.enemies.len()));
    // Every bar's actor finds its own point, and they differ (nobody stands on a head).
    let points: Vec<(f32, f32)> = battle.party.iter().chain(&battle.enemies).map(|b| Anchors::of(&anchors, &battle, b.actor).expect("on screen")).collect();
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            assert!((a.0 - b.0).hypot(a.1 - b.1) > 5.0, "{a:?} {b:?}");
        }
        assert!(a.0 > 0.0 && a.0 < W as f32 && a.1 > 0.0 && a.1 < H as f32, "{a:?} is in the window");
    }
}

// ----- when the window gets a scene ---------------------------------------------------------------

fn state_in_battle() -> ClientState {
    let area = AreaId { game: GameId::Fixture, key: "k".into() };
    let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area.clone());
    s.battle.set_rules(Some(Arc::new(SyntheticRules::default())));
    // Our own look, from the server's snapshot of our entity, and a peer's.
    let look = |id: u64, name: &str, appearance: &str| EntitySnapshot {
        id: EntityId(id),
        pos: Pos::default(),
        facing: 0.0,
        anim: AnimState::Idle,
        avatar: AvatarAppearance::new(name, [1, 2, 3], Some(appearance.to_string())),
    };
    s.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: vec![look(1, "Me", "ff7/barret"), look(2, "You", "ff7/tifa")], npcs: vec![] });
    s.apply(
        0.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(4),
            formation_id: 1,
            seed: 5,
            party: vec![member(0, 0), member(1, 1)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    s
}

#[test]
fn the_looks_of_the_party_are_ours_from_our_snapshot_and_the_others_from_the_area() {
    let s = state_in_battle();
    let start = s.battle.scene_start().unwrap();
    assert_eq!(start.id, BattleId(4));
    assert_eq!(start.party.len(), 2);
    assert_eq!(start.my_actor, Some(ActorId(0)));
    assert!(start.formation.is_none(), "the synthetic rules have no placement data");
    assert_eq!(party_looks(&s, &start.party), [Some("ff7/barret".to_string()), Some("ff7/tifa".to_string())]);
    // A member nobody knows has no look.
    let stranger = BattleMember { entity: EntityId(99), ..member(2, 2) };
    assert_eq!(party_looks(&s, &[stranger]), [None]);
}

#[test]
fn a_scene_is_made_for_a_battle_kept_while_it_is_seen_and_rebuilt_for_the_next() {
    let mut s = state_in_battle();
    let (mut slot, mut kit_slot) = (None, None);
    let view = s.battle.view(1.0).unwrap();
    assert_eq!(view.status, Status::Verified);
    assert!(scene_for(&mut slot, &mut kit_slot, &s, &view, 1.0, &[]).is_some());
    assert_eq!(slot.as_ref().map(BattleScene::id), Some(BattleId(4)));
    assert_eq!(slot.as_ref().unwrap().actor_count(), 4, "two party members and the two dummies of formation 1");
    assert!(kit_slot.is_some(), "the models are loaded once and kept for the next battle");
    // Seen again: the same scene (the scene clock moved on), not a new one.
    scene_for(&mut slot, &mut kit_slot, &s, &view, 2.5, &[]).unwrap();
    assert_eq!(slot.as_ref().unwrap().now, 2.5);
    // The battle ended: the client is out of it (no start, no engine) but the window still gets a
    // lingering view with the end panel, and the same scene goes on with it, playing what is left.
    s.apply(3.0, ServerMsg::BattleEnd { battle_id: BattleId(4), outcome: fmv_protocol::battle::Outcome::Defeat, rewards: vec![] });
    assert!(s.battle.scene_start().is_none() && s.battle.engine().is_none() && !s.battle.in_battle());
    let lingering = s.battle.view(3.5).expect("the view lingers after the end");
    assert!(lingering.finale.is_some() && lingering.id == BattleId(4));
    let scene = scene_for(&mut slot, &mut kit_slot, &s, &lingering, 3.5, &[BattleEvent::Defeat]).expect("the lingering view keeps its scene");
    assert_eq!(scene.now, 3.5);
    // Another battle: another scene.
    s.apply(
        4.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(5),
            formation_id: 0,
            seed: 1,
            party: vec![member(0, 0)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    let next = s.battle.view(4.0).unwrap();
    let scene = scene_for(&mut slot, &mut kit_slot, &s, &next, 4.0, &[]).unwrap();
    assert_eq!((scene.id(), scene.actor_count()), (BattleId(5), 2));
}

#[test]
fn a_battle_that_cannot_be_replayed_here_is_drawn_flat() {
    let mut s = state_in_battle();
    let (mut slot, mut kit_slot) = (None, None);
    // No battle data on this machine: the view says so and nothing is known of who fights.
    s.battle.set_rules(None);
    s.apply(
        5.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(9),
            formation_id: 1,
            seed: 5,
            party: vec![member(0, 0)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    let view = s.battle.view(5.0).unwrap();
    assert_eq!(view.status, Status::Unverified);
    assert!(scene_for(&mut slot, &mut kit_slot, &s, &view, 5.0, &[]).is_none());
    assert!(slot.is_none() && kit_slot.is_none(), "nothing was built, nothing loaded");
    // A view of a battle the client does not have a start for (it is over and there was no scene)
    // is flat too.
    let mut done = state_in_battle();
    let ended = done.battle.view(1.0).unwrap();
    done.apply(2.0, ServerMsg::BattleEnd { battle_id: BattleId(4), outcome: fmv_protocol::battle::Outcome::Victory, rewards: vec![] });
    assert!(scene_for(&mut slot, &mut kit_slot, &done, &ended, 2.0, &[]).is_none());
    // And a stale scene for another battle is dropped, not reused.
    let live = state_in_battle();
    let mut other = Some(BattleScene::new(&start(1, None), Some(&engine(1, 1)), &[], &mut kit()).unwrap());
    let mut wrong = BattleId(77);
    if let Some(scene) = other.as_mut() {
        scene.id = wrong;
        wrong = scene.id();
    }
    assert_eq!(wrong, BattleId(77));
    let v = live.battle.view(1.0).unwrap();
    let scene = scene_for(&mut other, &mut kit_slot, &live, &v, 1.0, &[]).unwrap();
    assert_eq!(scene.id(), BattleId(4), "rebuilt for the battle that is on");
}

#[test]
fn a_model_source_is_asked_per_actor_in_the_engines_order_with_who_the_actor_is() {
    struct Recorder(Rc<RefCell<Vec<String>>>);
    impl BattleModelSource for Recorder {
        fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
            self.0.borrow_mut().push(format!("party {} char {} mine {}", who.actor.0, who.character, who.mine));
            BattleActorModel::Box(BoxSpec::for_party(usize::from(who.actor.0)))
        }
        fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
            self.0.borrow_mut().push(format!("enemy {} id {:?} {} hp {}", who.actor.0, who.enemy_id, who.name, who.max_hp));
            BattleActorModel::Box(BoxSpec::for_enemy(0, who.max_hp))
        }
    }
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut kit = ModelKit::new(Box::new(Recorder(Rc::clone(&asked))), None);
    let scene = BattleScene::new(&start(2, Some(formation())), Some(&engine(1, 2)), &[], &mut kit).unwrap();
    assert_eq!(
        *asked.borrow(),
        ["party 0 char 0 mine true", "party 1 char 1 mine false", "enemy 2 id Some(3) Dummy A hp 200", "enemy 3 id Some(5) Dummy B hp 200"]
    );
    // A source that returns boxes for everyone gives a scene of boxes: no archive is needed.
    assert!(scene.actors.iter().all(|a| a.rig.scale == 1.0));
}

// ----- real battle models plug in here ---------------------------------------------------------

/// A source of battle models: the stick figure for the party (with a weapon), boxes for the
/// enemies, and a location for formations that name location 9. Stands in for a loader of the
/// battle archive.
struct Armed;

fn stick() -> fmv_ff7::playable::Character {
    character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3)
}

impl BattleModelSource for Armed {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        let stick = stick();
        let owner = model::BattleModelData::new(format!("pc{}", who.character), stick.model.clone(), vec![stick.idle.clone()]);
        BattleActorModel::Battle(owner.with_attachment(model::Attachment::new("sword", stick.model, vec![stick.idle])))
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        BattleActorModel::Box(BoxSpec::for_enemy(0, who.max_hp))
    }

    fn location(&mut self, location: u16) -> Option<BattleActorModel> {
        let stick = stick();
        (location == 9).then(|| BattleActorModel::battle("location-9", stick.model, vec![stick.idle]))
    }
}

#[test]
fn a_weapon_is_drawn_with_its_owners_placement_and_frame() {
    let engine = engine(1, 2);
    let mut kit = ModelKit::new(Box::new(Armed), None);
    let scene = BattleScene::new(&start(2, Some(formation())), Some(&engine), &[], &mut kit).unwrap();
    let frame = scene.frame(&view(&scene), false);
    // Two party members with a weapon each, then the two boxes.
    assert_eq!(frame.models.models.len(), 2 * 2 + 2);
    for owner in [0usize, 2] {
        let (body, weapon) = (&frame.models.models[owner], &frame.models.models[owner + 1]);
        assert!(!Arc::ptr_eq(&body.asset, &weapon.asset), "the weapon is a model of its own");
        // The weapon here is the same stick mesh, so the same placement and pose put it exactly on the owner.
        assert_eq!(body.positions, weapon.positions);
    }
    // Battle models are in battle units: no field scale.
    assert!(scene.actors[..2].iter().all(|a| a.rig.scale == 1.0));
}

#[test]
fn a_location_model_replaces_the_stand_in_floor_and_is_placed_as_it_is() {
    let engine = engine(1, 2);
    let mut at_nine = formation();
    at_nine.location = 9;
    let mut kit = ModelKit::new(Box::new(Armed), None);
    let scene = BattleScene::new(&start(2, Some(at_nine)), Some(&engine), &[], &mut kit).unwrap();
    let view = view(&scene);
    let frame = scene.frame(&view, false);
    let location = frame.location.as_ref().expect("the formation's location has a model");
    assert_eq!(location.models.len(), 1);
    assert!(location.ground.is_empty(), "no marks of its own: the actors' pass draws theirs");
    // It is placed as it is: the stick figure's first vertex is at battle (-3, 0, 0).
    let at = view.camera.to_camera([-3.0, 0.0, 0.0]);
    let got = location.models[0].positions[0];
    for k in 0..3 {
        assert!((f64::from(got[k]) - at[k]).abs() < 0.05, "axis {k}: {got:?} vs {at:?}");
    }
    // Without the stand-in floor the stage is only the haze and the sky.
    assert!(frame.stage.len() <= 12, "{} stage vertices", frame.stage.len());
    // The actors are still there, in their own pass.
    assert_eq!(frame.models.models.len(), 2 * 2 + 2);
    // A formation whose location the source has no model for keeps the stand-in floor.
    let mut elsewhere = formation();
    elsewhere.location = 3;
    let scene = BattleScene::new(&start(2, Some(elsewhere)), Some(&engine), &[], &mut ModelKit::new(Box::new(Armed), None)).unwrap();
    let frame = scene.frame(&scene.view(W, H), false);
    assert!(frame.location.is_none() && frame.stage.len() > 500, "the stand-in floor: {} vertices", frame.stage.len());
    // A scene without placement data has no location to ask for.
    let scene = BattleScene::new(&start(2, None), Some(&engine), &[], &mut ModelKit::new(Box::new(Armed), None)).unwrap();
    assert!(scene.frame(&scene.view(W, H), false).location.is_none());
}

/// A location as big as the field around the camera: a flat slab with the camera inside its extent.
struct Slab;

impl BattleModelSource for Slab {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        StandInModels.party_member(who)
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        StandInModels.enemy(who)
    }

    fn location(&mut self, _location: u16) -> Option<BattleActorModel> {
        Some(BattleActorModel::Box(BoxSpec { width: 40_000.0, depth: 40_000.0, height: 30.0, rgb: [60, 130, 70] }))
    }
}

#[test]
fn a_location_around_the_camera_is_cut_to_what_is_in_front_of_it_once_per_camera() {
    let engine = engine(1, 2);
    let mut kit = ModelKit::new(Box::new(Slab), None);
    let scene = BattleScene::new(&start(2, Some(formation())), Some(&engine), &[], &mut kit).unwrap();
    let view = view(&scene);
    let frame = scene.frame(&view, false);
    let location = frame.location.expect("a location");
    // The slab is 40 000 wide and the camera stands over it: half of it is behind the camera. Nothing
    // of that half reaches the GPU (it would be drawn wrongly), and the half in front does.
    let positions = &location.models[0].positions;
    assert!(positions.len() > 8, "something of the slab is left");
    assert!(positions.iter().all(|p| f64::from(p[2]) >= LOCATION_NEAR - 0.05), "nothing behind the near plane");
    // The cut is made once per camera: the same asset (one GPU upload) every frame, for any window size.
    let again = scene.frame(&scene.view(640, 480), false).location.unwrap();
    assert!(Arc::ptr_eq(&location.models[0].asset, &again.models[0].asset));
    // Another camera has its own cut (it has other things behind it).
    let other = View { camera: scene.cameras[1], ..view };
    let elsewhere = scene.frame(&other, false).location.unwrap();
    assert!(!Arc::ptr_eq(&location.models[0].asset, &elsewhere.models[0].asset));
    assert!(elsewhere.models[0].positions.iter().all(|p| f64::from(p[2]) >= LOCATION_NEAR - 0.05));
    // A view through a camera the scene does not know draws no location rather than a wrong one.
    let stranger = BattleCamera::look_at([5000.0, -900.0, 5000.0], [0.0, -300.0, 0.0]).unwrap();
    assert!(scene.frame(&View { camera: stranger, ..view }, false).location.is_none());
}

// ----- anchors for the battle overlay -----------------------------------------------------------

#[test]
fn head_anchors_are_over_each_head_party_first_then_enemies_in_the_engines_order() {
    let (mut scene, _) = scene(3, true);
    let view = view(&scene);
    let Anchors { party, enemies } = scene.head_anchors(&view);
    assert_eq!((party.len(), enemies.len()), (3, 2));
    let screen = scene.screen_anchors(&view);
    for (i, anchor) in party.iter().chain(&enemies).enumerate() {
        let (x, y) = anchor.expect("everyone is on the screen");
        let (feet, head) = (screen[i].feet.unwrap(), screen[i].head.unwrap());
        assert!((x - head[0]).abs() < 30.0, "over the head, not beside it: {x} vs {}", head[0]);
        assert!(y < head[1] && y > head[1] - 90.0, "a little above the top of the model: {y} vs {}", head[1]);
        assert!(y < feet[1]);
    }
    // The enemies are on the left of the party, as on the screen.
    assert!(enemies.iter().flatten().all(|e| party.iter().flatten().all(|p| e.0 < p.0)));
    // An enemy that has collapsed away has none; the others still do. Killed by the engine's own
    // events: the scene follows what happened.
    let (state, events, _, target) = killing_blow();
    scene = BattleScene::new(&start(2, Some(formation())), Some(&engine_for_two()), &[], &mut kit()).unwrap();
    scene.update(1.0, &events, Some(&state));
    scene.update(30.0, &[], Some(&state));
    let Anchors { party, enemies } = scene.head_anchors(&scene.view(W, H));
    assert!(party.iter().all(Option::is_some));
    let dead = usize::from(target.0) - 2;
    assert!(enemies[dead].is_none() && enemies[1 - dead].is_some(), "{enemies:?}");
}

fn engine_for_two() -> BattleState {
    engine(1, 2)
}

// ----- what the client hands the scene ----------------------------------------------------------

fn battle_message(events: Vec<BattleEvent>) -> ServerMsg {
    ServerMsg::BattleEvent { battle_id: BattleId(4), tick: 0, events, command: None }
}

#[test]
fn the_events_of_every_message_queue_in_order_until_taken() {
    let mut s = state_in_battle();
    assert!(s.battle.take_events().is_empty(), "nothing has happened yet");
    let a = BattleEvent::GaugeReady { actor: ActorId(0) };
    let b = BattleEvent::Miss { actor: ActorId(0), target: ActorId(2) };
    s.apply(1.0, battle_message(vec![a, b]));
    s.apply(1.1, battle_message(vec![BattleEvent::Death { actor: ActorId(3) }]));
    assert_eq!(s.battle.take_events(), [a, b, BattleEvent::Death { actor: ActorId(3) }]);
    assert!(s.battle.take_events().is_empty(), "taken once");
}

#[test]
fn events_are_kept_without_a_replay_and_for_the_battle_that_is_on_only() {
    // No battle data here: the battle is not mirrored, but the scene's events still come.
    let mut s = state_in_battle();
    s.battle.set_rules(None);
    s.apply(
        2.0,
        ServerMsg::EncounterStart {
            battle_id: BattleId(6),
            formation_id: 1,
            seed: 5,
            party: vec![member(0, 0)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    assert!(s.battle.engine().is_none());
    let hit = BattleEvent::Damage { actor: ActorId(0), target: ActorId(1), amount: 3, critical: false };
    s.apply(2.1, ServerMsg::BattleEvent { battle_id: BattleId(6), tick: 1, events: vec![hit], command: None });
    // A message for another battle is not this battle's business.
    s.apply(2.2, ServerMsg::BattleEvent { battle_id: BattleId(5), tick: 1, events: vec![BattleEvent::Victory], command: None });
    assert_eq!(s.battle.take_events(), [hit]);
    // A new battle starts with nothing of the old one's.
    s.apply(3.0, ServerMsg::BattleEvent { battle_id: BattleId(6), tick: 2, events: vec![hit], command: None });
    s.apply(
        3.5,
        ServerMsg::EncounterStart {
            battle_id: BattleId(7),
            formation_id: 0,
            seed: 1,
            party: vec![member(0, 0)],
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        },
    );
    assert!(s.battle.take_events().is_empty());
}

#[test]
fn the_last_events_of_a_battle_can_still_be_taken_after_it_ended_and_the_queue_is_bounded() {
    let mut s = state_in_battle();
    let kill = BattleEvent::Death { actor: ActorId(2) };
    s.apply(1.0, battle_message(vec![kill, BattleEvent::Victory]));
    s.apply(1.0, ServerMsg::BattleEnd { battle_id: BattleId(4), outcome: fmv_protocol::battle::Outcome::Victory, rewards: vec![] });
    assert!(!s.battle.in_battle());
    assert_eq!(s.battle.take_events(), [kill, BattleEvent::Victory], "the killing blow survives the end of the battle");
    // A window that stops drawing cannot make the queue grow without bound: the oldest are dropped.
    let mut flood = state_in_battle();
    for i in 0..2000u32 {
        flood.apply(1.0, battle_message(vec![BattleEvent::Damage { actor: ActorId(0), target: ActorId(2), amount: i, critical: false }]));
    }
    let taken = flood.battle.take_events();
    assert_eq!(taken.len(), 256);
    assert!(matches!(taken.last(), Some(BattleEvent::Damage { amount: 1999, .. })), "the newest are kept");
}
