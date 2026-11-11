//! Owned-data pictures of the cargo doorway and the passenger-car arrival. Battles and
//! persistence are covered elsewhere; these tests exercise the original cast through the
//! actual client stage, camera, body filtering and renderer until the durable handoff.

use super::*;
use crate::camera::FieldCamera;
use crate::window::camera_input;
use fmv_protocol::{SceneOrigin, SceneReplay};

pub(super) fn start_held(rig: &mut Rig, origin: Option<SceneOrigin>) {
    rig.state.apply(0.0, ServerMsg::SceneStart {
        run: SceneRunId(1), scene: rig.def.id.clone(), seed: SEED,
        progress: rig.story.encode(), gil: rig.gil,
        replay: Some(SceneReplay { source: rig.play.replay_identity().fingerprint, origin, events: Vec::new() }), presentation_origin: None,
        owner: EntityId(1), participants: (0..3).map(|spot| SceneParticipant { entity: EntityId(u64::from(spot) + 1), spot }).collect(),
        you: SceneRole::Owner,
    });
    assert!(rig.state.scene.is_active(), "the exact-source arrival is built");
}

pub(super) fn frame(rig: &mut Rig, camera: &mut FieldCamera, dt: f32) -> FieldDraw {
    rig.draw();
    let drawn = rig.draw();
    rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &drawn, SIZE, dt));
    drawn
}

pub(super) fn participants_hidden(rig: &Rig, drawn: &FieldDraw) {
    let scene = rig.state.scene.player().expect("the authored scene remains held");
    let built = drawn.built.as_ref().expect("the owned models are built");
    assert!(drawn.own_at.is_none(), "an absent participant cannot steer the scene camera");
    assert!(super::super::draw::participant_label_hidden(Some(scene), EntityId(1), Subject::Own), "the owner's HUD fallback is hidden too");
    for entity in 1..=3 {
        assert!(scene.hides_participant(EntityId(entity)), "participant {entity} stays out of the cast's passage");
        assert!(!built.anchors.contains_key(&Who(entity)), "participant {entity} has no body");
        assert!(super::super::draw::participant_label_hidden(Some(scene), EntityId(1), Subject::Remote(EntityId(entity))),
            "participant {entity} has no floating label");
    }
    assert!(!scene.hides_participant(EntityId(4)), "an unrelated player is not hidden");
}

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; authored cargo door crossing"]
fn offscreen_real_ff7_cargo_door_crossing() {
    use fmv_ff7::script::scene::{cargo_door_scene, with_trigger_position};
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("cargoin").expect("cargo field");
    // A measured point just across border1's horizontal doorway line. This is an
    // accepted host crossing for the presentation fixture, not an authored spawn.
    let origin = SceneOrigin { pos: Pos::new(9.0, -105.0, 0.0), facing: field_direction_to_facing(0), triangle: None };
    let def = with_trigger_position(area.field_file(), &cargo_door_scene(), origin.pos, origin.facing, origin.triangle)
        .expect("the accepted crossing binds Cloud's starting pose");
    let story = StoryState::at(48).into_extended().expect("exact story");
    let mut rig = make_rig_with_snapshot(&real, def, true, story, 600);
    let jump_clip = rig.play.animation_clip(11, 13).expect("the authored door animation exists").to_string();
    start_held(&mut rig, Some(origin));
    let mut camera = FieldCamera::default();
    let mut positions = BTreeSet::new();
    let mut in_flight = 0;
    let mut jump_frames = Vec::new();
    let mut apex = 0.0f32;
    let mut ended = false;
    let mut hidden = false;
    let mut skipped = 0;
    for tick in 0..180 {
        let drawn = frame(&mut rig, &mut camera, 1.0 / 30.0);
        participants_hidden(&rig, &drawn);
        assert!(rig.view.background_state().expect("cargo tile state").visible(2, 8, 4),
            "box2's original BGON enables group8 bit2 before the jump");
        let player = rig.state.scene.player().unwrap();
        skipped = skipped.max(player.counts().anims_skipped);
        let cloud = player.cast_draws().into_iter().find(|actor| actor.entity == 11);
        if tick == 0 { assert_eq!(cloud.expect("Cloud begins at the crossing").pos, origin.pos); }
        if let Some(cloud) = cloud {
            apex = apex.max(cloud.pos.z);
            positions.insert((cloud.pos.x.round() as i32, cloud.pos.y.round() as i32, cloud.pos.z.round() as i32));
            if cloud.pos.y < origin.pos.y - 1.0 && cloud.pos.y > -246.0 {
                in_flight += 1;
                assert_eq!(cloud.clip.map(|(clip, _)| clip), Some(jump_clip.as_str()), "the door jump keeps its authored animation");
                let Some((_, crate::avatar::anim::ClipFrame::Loop(elapsed))) = cloud.clip else {
                    panic!("the authored DFANM remains the jump's animation");
                };
                assert!(elapsed >= 15.0, "the jump continues the clip after its opening wait");
                assert!(jump_frames.last().is_none_or(|previous| elapsed > *previous), "the clip advances during the jump");
                jump_frames.push(elapsed);
            }
        } else { hidden = true; }
        if [0, 14, 20, 21, 26, 28].contains(&tick) || hidden || ended {
            rig.picture_clipped(&mut gpu, &format!("cargo-door-frame-{tick:03}"), SIZE, true, camera.clip_range(rig.view.range));
        }
        if ended { break; }
        rig.state.scene.advance(1.0 / 30.0);
        for command in rig.state.scene.take_commands() {
            match command {
                super::super::SceneCmd::End { skipped: false, .. } => ended = true,
                other => panic!("the doorway has no dialogue or battle: {other:?}"),
            }
        }
    }
    eprintln!("cargo door: clip {jump_clip}, {} distinct places, {in_flight} jump frames, hidden={hidden}, ended={ended}, skipped={skipped}", positions.len());
    assert!(ended && hidden, "Cloud disappears only after completing his door movement");
    assert!(in_flight >= 10 && positions.len() >= 10, "the authored door movement is shown over time");
    assert!(jump_frames.last().unwrap() - jump_frames[0] >= 10.0, "the jump is animated throughout its movement");
    assert!((apex - 70.3125).abs() < 0.01, "the authored JUMP12 fixed-point arc reaches its measured apex, got {apex}");
    assert_eq!(skipped, 0);
    assert!(rig.state.scene.player().unwrap().hides_participant(EntityId(1)), "the MAPJUMP still waits for the server");
}

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; authored passenger train arrival"]
fn offscreen_real_ff7_passenger_train_arrival() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let def = fmv_ff7::script::scene::train_scene();
    let story = StoryState::at(48).into_extended().expect("exact story");
    let mut rig = make_rig_with_snapshot(&real, def, true, story, 600);
    start_held(&mut rig, None);
    let mut camera = FieldCamera::default();
    let mut windows = 0;
    let mut last_window = None;
    let mut seen = BTreeSet::new();
    let mut walked = BTreeMap::<u8, BTreeSet<(i32, i32)>>::new();
    let mut skipped = 0;
    let mut ended = false;
    let mut cloud_appeared = false;
    let mut after_cloud = 0;
    let mut camera_centre = None;
    let mut door_visible = BTreeSet::new();
    for tick in 0..1200 {
        let drawn = frame(&mut rig, &mut camera, 1.0 / 30.0);
        participants_hidden(&rig, &drawn);
        door_visible.insert(rig.view.background_state().expect("train tile state").visible(2, 3, 1));
        let player = rig.state.scene.player().unwrap();
        assert_eq!(player.camera_clock().state, super::super::stage::CameraState::Scroll { x: 18, y: 2, frames: 0 },
            "the original field fixes this shot before the director runs");
        let shake = player.shake();
        let centre = [(rig.view.bounds.min.x + rig.view.bounds.max.x) * 0.5 + shake[0],
            (rig.view.bounds.min.y + rig.view.bounds.max.y) * 0.5 - shake[1]];
        let base = *camera_centre.get_or_insert(centre);
        assert!(centre[0].abs() < 0.001 && (centre[1] - 2.0).abs() < 0.001,
            "SCR2D(18,2) is centred at (0,2) after the narrow authored camera range clamps x");
        assert!((base[0] - centre[0]).abs() < 0.001 && (base[1] - centre[1]).abs() < 0.001,
            "the train's fixed shot shakes without following later actor entrances");
        skipped = skipped.max(player.counts().anims_skipped);
        let casts = player.cast_draws();
        for actor in &casts {
            seen.insert(actor.entity);
            walked.entry(actor.entity).or_default().insert((actor.pos.x.round() as i32, actor.pos.y.round() as i32));
        }
        let cloud = casts.iter().find(|actor| actor.entity == 30);
        if let Some(cloud) = cloud {
            assert_eq!(windows, 2, "Cloud arrives only after both authored windows");
            assert_eq!(cloud.pos, Pos::new(2.0, 537.0, -53.0));
            assert!((cloud.facing - field_direction_to_facing(0)).abs() < 0.001);
            after_cloud += 1;
        }
        let appeared_now = cloud.is_some() && !cloud_appeared;
        cloud_appeared |= cloud.is_some();
        let new_window = player.has_window() && last_window != Some(player.epoch());
        if new_window {
            windows += 1;
            last_window = Some(player.epoch());
            assert!(cloud.is_none(), "Cloud is absent from both initial windows");
            if windows == 1 {
                assert!(rig.view.background_state().unwrap().visible(2, 3, 1), "the original entry shot has the doorway closed");
                for (entity, expected) in [(40, Pos::new(-46.0, -358.0, -53.0)), (41, Pos::new(-91.0, -292.0, -53.0)), (37, Pos::new(91.0, -292.0, -53.0))] {
                    assert_eq!(casts.iter().find(|actor| actor.entity == entity).map(|actor| actor.pos), Some(expected),
                        "actor {entity} starts where the original field places it");
                }
            }
        }
        if new_window || appeared_now || tick % 60 == 0 || ended {
            let name = if new_window { format!("train-window-{windows:02}") } else { format!("train-frame-{tick:03}") };
            rig.picture_clipped(&mut gpu, &name, SIZE, true, camera.clip_range(rig.view.range));
        }
        if ended { break; }
        if rig.state.scene.player().unwrap().has_window() { rig.state.scene.key(KeyCode::Enter, true, false); }
        else { rig.state.scene.advance(1.0 / 30.0); }
        for command in rig.state.scene.take_commands() {
            match command {
                super::super::SceneCmd::Step { answer: None, .. } => {},
                super::super::SceneCmd::End { skipped: false, .. } => ended = true,
                other => panic!("unexpected train arrival command {other:?}"),
            }
        }
    }
    eprintln!("train arrival: {windows} windows, cast {seen:?}, {} steps after Cloud appeared, skipped {skipped}", after_cloud);
    assert!(ended && cloud_appeared && after_cloud > 20, "the full actor tail plays before control returns");
    assert_eq!(windows, 2);
    assert_eq!(seen, BTreeSet::from([30, 31, 33, 34, 35, 37, 40, 41]));
    for entity in [31, 33, 34, 35, 37, 40, 41] {
        assert!(walked.get(&entity).is_some_and(|places| places.len() > 5), "actor {entity} walks through the scene");
    }
    assert_eq!(skipped, 0, "all authored train gestures play");
    assert_eq!(door_visible, BTreeSet::from([false, true]), "the authored doorway opens and closes during the arrival");
    assert!(rig.view.background_state().unwrap().visible(2, 3, 1), "Cloud's last doorway request closes it again");
    assert!(rig.state.scene.player().unwrap().hides_participant(EntityId(1)));
    // The server publishes its durable final pose while the scene still holds input,
    // then releases the scene. This checks the production snapshot path, including
    // facing, instead of assigning a test-only local teleport after release.
    let handoff = Pos::new(2.0, 537.0, -53.0);
    rig.state.apply(0.0, ServerMsg::EntityUpdate { tick: 1, entities: vec![EntitySnapshot {
        id: EntityId(1), pos: handoff, facing: field_direction_to_facing(0), anim: AnimState::Idle,
        avatar: AvatarAppearance::new("PlayerA", rig.own_tint, Some(rig.looks[0].into())),
    }] });
    assert_eq!(rig.state.own.pos(), handoff);
    assert_eq!(rig.state.own.facing(), field_direction_to_facing(0));
    let held = frame(&mut rig, &mut camera, 0.0);
    participants_hidden(&rig, &held);
    rig.state.apply(0.0, ServerMsg::SceneEnded { run: SceneRunId(1), outcome: SceneOutcome::Finished });
    assert!(!rig.state.scene.is_active());
    let returned = frame(&mut rig, &mut camera, 0.5);
    assert!(rig.view.background_state().unwrap().visible(2, 3, 1), "the field keeps its final door state after the scene ends");
    assert_eq!(returned.own_at, rig.view.to_view(handoff), "the acknowledged handoff restores the body at Cloud's final position");
    assert!(returned.built.unwrap().anchors.contains_key(&Who(1)), "the acknowledged handoff restores the player's body");
    rig.picture_clipped(&mut gpu, "train-control-return", SIZE, true, camera.clip_range(rig.view.range));
}
