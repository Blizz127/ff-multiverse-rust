//! OFST changes the model's position independently of its logical movement and camera target.

use super::*;
use crate::scene::stage::Cast;
use fmv_ff7::script::scene::{SceneBoundary, WindowCheckpoint};
use fmv_protocol::{SceneReplay, SceneReplayEvent};

#[test]
fn model_offsets_replace_in_world_units_and_survive_placement_turning_and_movement() {
    let ground = |_: i16, _: i16, _: Option<u16>| Some(-56.0);
    let mut cast = Cast::new([18]);
    cast.apply(&Stage::Offset { entity: 18, x: 590, y: -38, z: -1316 }, 0.0, &ground);
    assert_eq!(cast.actor(18).unwrap().render_pos(), None, "an offset does not place an unplaced model");
    let place = Stage::Place { entity: 18, x: -18, y: -459, z: Some(-56), triangle: Some(53) };
    cast.apply(&place, 0.0, &ground);
    assert_eq!(cast.actor(18).unwrap().pos(), Some([-18.0, -459.0, -56.0]));
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([572.0, -497.0, -1372.0]));
    cast.apply(&Stage::Face { entity: 18, heading: 1.25, frames: 0 }, 0.0, &ground);
    cast.apply(&Stage::Show { entity: 18, visible: false }, 0.0, &ground);
    cast.apply(&Stage::Show { entity: 18, visible: true }, 0.0, &ground);
    cast.apply(&Stage::Offset { entity: 18, x: 636, y: 308, z: -1204 }, 80.0, &ground);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([618.0, -151.0, -1260.0]), "replacement is immediate, unrotated and not cumulative");
    assert_eq!(cast.actor(18).unwrap().triangle(), Some(53));
    cast.apply(&Stage::Move { entity: 18, from: None, x: 62, y: -459, z: Some(-56), triangle: None,
        frames: 8, how: MoveHow::Walk, animation: None }, 0.0, &ground);
    cast.advance(3.0);
    assert_eq!(cast.actor(18).unwrap().pos(), Some([12.0, -459.0, -56.0]));
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([648.0, -151.0, -1260.0]));
    cast.apply(&Stage::Offset { entity: 18, x: 0, y: 0, z: 0 }, 0.0, &ground);
    cast.advance(5.0);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([62.0, -459.0, -56.0]), "clearing an offset does not interrupt the walk");
    cast.apply(&Stage::Offset { entity: 18, x: -5, y: 7, z: 9 }, 0.0, &ground);
    cast.apply(&place, 0.0, &ground);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([-23.0, -452.0, -47.0]), "XYZI changes only the base position");
    cast.clear();
    assert!(cast.actor(18).is_none(), "scene teardown drops its local model state");
}

#[test]
fn linear_offsets_draw_integer_steps_through_hidden_placement_and_late_frames() {
    use fmv_ff7::script::scene::OffsetMotion;
    let mut cast = Cast::new([18, 20]);
    let ground = |_: i16, _: i16, _: Option<u16>| Some(0.0);
    for (entity, target) in [(18, -202), (20, 184)] {
        cast.apply(&Stage::Place { entity, x: 10, y: 20, z: Some(30), triangle: Some(4) }, 0.0, &ground);
        cast.apply(&Stage::OffsetMove { entity, motion: OffsetMotion::new([0; 3], [target, 0, 0], 50).unwrap() }, 0.0, &ground);
    }
    cast.apply(&Stage::Show { entity: 20, visible: false }, 0.0, &ground);
    cast.advance(0.5);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([10.0, 20.0, 30.0]), "no invented sub-frame offset interpolation");
    cast.advance(24.5);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([-91.0, 20.0, 30.0]));
    assert_eq!(cast.actor(20).unwrap().render_pos(), Some([102.0, 20.0, 30.0]), "hidden models still advance");
    cast.apply(&Stage::Place { entity: 18, x: 50, y: 60, z: Some(70), triangle: Some(9) }, 0.0, &ground);
    cast.advance(25.0);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([-152.0, 60.0, 70.0]), "placement leaves offset timing intact");
    assert_eq!(cast.actor(18).unwrap().pos(), Some([50.0, 60.0, 70.0]));
    assert_eq!(cast.actor(20).unwrap().render_pos(), Some([194.0, 20.0, 30.0]));
    let replacement = OffsetMotion::new([-202, 0, 0], [100, 0, 0], 20).unwrap();
    cast.apply(&Stage::OffsetMove { entity: 18, motion: replacement }, 10.0, &ground);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([-1.0, 60.0, 70.0]), "late events use the exact captured start and elapsed step");
    cast.apply(&Stage::Offset { entity: 18, x: 2, y: 3, z: 4 }, 0.0, &ground);
    cast.advance(100.0);
    assert_eq!(cast.actor(18).unwrap().render_pos(), Some([52.0, 63.0, 74.0]), "instant replacement cancels the tween");
}

#[test]
fn timed_offsets_move_the_avatar_and_speaker_feet_but_leave_the_follow_target_on_the_base() {
    use crate::avatar::frame::{tests::view, AvatarFrame, Built};
    use crate::camera::SceneAim;
    use crate::render::view_to_window;
    use crate::scene::draw::{camera_shot, speaker_spot};
    use std::collections::HashMap;
    let mut rig = rig_with(vec![Beat {
        stage: vec![timed(15, Stage::Offset { entity: 18, x: 100, y: -30, z: 20 })],
        then: say(30, 18, 18, 1).then,
    }, end(0)], false);
    begin(&mut rig, SceneRole::Owner);
    let positions = |scene: &ScenePlayer| {
        let actor = scene.cast_draws().into_iter().find(|actor| actor.entity == 18).unwrap();
        (actor.pos, actor.render_pos)
    };
    rig.client.advance(14.0 / 30.0);
    let base = Pos::new(10.0, 10.0, 11.0);
    assert_eq!(positions(rig.client.player().unwrap()), (base, base));
    rig.client.advance(1.0 / 30.0);
    let rendered = Pos::new(110.0, -20.0, 31.0);
    assert_eq!(positions(rig.client.player().unwrap()), (base, rendered));
    rig.client.advance(1.0);
    let scene = rig.client.player().unwrap();
    assert!(scene.has_window());
    assert_eq!(cast_players(scene).into_iter().find(|actor| actor.who == cast_who(18)).unwrap().pos, rendered);
    let view = view();
    let projected = view.to_view(base).unwrap();
    assert_eq!(camera_shot(scene, &view).aim, SceneAim::Lead { lead: Some([projected.x, projected.y]) });
    let head = view.to_view(Pos::new(rendered.x, rendered.y, rendered.z + 30.0)).unwrap();
    let built = Built {
        frame: AvatarFrame { models: vec![], tints: vec![], identities: vec![], zoom: 400.0, field_depth: None,
            overlay_bias: 0.0, ground: vec![], scenery: None },
        markers: vec![], anchors: HashMap::from([(cast_who(18), head)]),
    };
    let spot = speaker_spot(scene, Some(&built), &view, 1100, 680).expect("the shifted speaker is in view");
    let feet = view.to_view(rendered).unwrap();
    let feet = view_to_window(&view.bounds, 1100, 680, feet.x, feet.y);
    assert_eq!(spot.feet, (feet[0], feet[1]), "the panel uses the same shifted feet as the model");
    rig.client.advance(10.0);
    assert_eq!(positions(rig.client.player().unwrap()), (base, rendered), "dialogue does not clear or accumulate the offset");
}

struct OffsetSource { scripted: Scripted, ordinal: u32, boundary: Option<SceneBoundary> }

fn checkpoint(ordinal: u32) -> WindowCheckpoint {
    WindowCheckpoint { ordinal, entity: 18, instruction_offset: Some(100 + ordinal), window: 0, dialog: Some(ordinal as u16), choices: None }
}

impl SceneSource for OffsetSource {
    fn initial(&self) -> Vec<Stage> { self.scripted.initial() }
    fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.scripted.height_at(x, y, triangle) }
    fn fingerprint(&self) -> Option<[u8; 32]> { Some([42; 32]) }
    fn boundary(&self) -> Option<SceneBoundary> { self.boundary }
    fn next(&mut self, answer: Option<usize>) -> Beat {
        assert_eq!(answer, None);
        let beat = self.scripted.next(answer);
        self.ordinal += 1;
        self.boundary = Some(match beat.then {
            Then::Window(_) => SceneBoundary::Window(checkpoint(self.ordinal)),
            Then::End { how, .. } => SceneBoundary::End(how),
        });
        beat
    }
}

#[test]
fn strict_replay_reconstructs_initial_and_timed_offsets_without_moving_the_logical_actor() {
    let source = || OffsetSource {
        scripted: Scripted {
            initial: [places(), vec![Stage::Offset { entity: 18, x: 20, y: -30, z: 40 }]].concat(),
            beats: vec![
                Beat { stage: vec![timed(15, Stage::Offset { entity: 18, x: -50, y: 60, z: -70 })], then: say(30, 18, 18, 1).then },
                Beat { stage: vec![timed(0, Stage::Place { entity: 18, x: 70, y: 80, z: Some(90), triangle: Some(4) })], then: say(0, 18, 18, 2).then },
                end(0),
            ].into(), asked: Asked::default(),
        }, ordinal: 0, boundary: None,
    };
    let run = || Run { id: RUN, participants: vec![], role: SceneRole::Owner };
    let empty = SceneReplay { source: [42; 32], origin: None, events: vec![] };
    let mut live = ScenePlayer::with_replay_seeded(run(), def(), Box::new(source()), false, &empty, [0; 2]).unwrap();
    assert_eq!(live.actor(18).unwrap().render_pos(), Some([30.0, -20.0, 51.0]), "quiet setup offset is visible before the first timed event");
    live.advance(1.0);
    assert!(live.pass_window(None));
    let replay = SceneReplay { events: vec![SceneReplayEvent::Window { step: 0, identity: checkpoint(1).identity_bytes(), answer: None }], ..empty };
    let rebuilt = ScenePlayer::with_replay_seeded(run(), def(), Box::new(source()), false, &replay, [0; 2]).unwrap();
    for player in [&live, &rebuilt] {
        assert_eq!(player.actor(18).unwrap().pos(), Some([70.0, 80.0, 90.0]));
        assert_eq!(player.actor(18).unwrap().render_pos(), Some([20.0, 140.0, 20.0]));
        assert_eq!(player.actor(18).unwrap().triangle(), Some(4));
        assert_eq!(player.panel().unwrap().window().message.origin, MessageOrigin::Canon { entity: 18, dialog: 2 });
    }
}

#[test]
fn replay_advances_a_linear_offset_to_its_completed_boundary() {
    use fmv_ff7::script::scene::OffsetMotion;
    let source = || OffsetSource {
        scripted: Scripted { initial: places(), beats: vec![
            Beat { stage: vec![timed(0, Stage::OffsetMove { entity: 18,
                motion: OffsetMotion::new([0; 3], [-202, 0, 0], 50).unwrap() })], then: say(51, 18, 18, 1).then },
            say(0, 18, 18, 2), end(0),
        ].into(), asked: Asked::default() }, ordinal: 0, boundary: None,
    };
    let run = || Run { id: RUN, participants: vec![], role: SceneRole::Owner };
    let empty = SceneReplay { source: [42; 32], origin: None, events: vec![] };
    let mut live = ScenePlayer::with_replay_seeded(run(), def(), Box::new(source()), false, &empty, [0; 2]).unwrap();
    live.advance(25.0 / 30.0);
    assert_eq!(live.actor(18).unwrap().render_pos(), Some([-91.0, 10.0, 11.0]));
    assert!(!live.has_window());
    live.advance(1.0);
    assert!(live.pass_window(None));
    let replay = SceneReplay { events: vec![SceneReplayEvent::Window { step: 0, identity: checkpoint(1).identity_bytes(), answer: None }], ..empty };
    let rebuilt = ScenePlayer::with_replay_seeded(run(), def(), Box::new(source()), false, &replay, [0; 2]).unwrap();
    for player in [&live, &rebuilt] {
        assert_eq!(player.actor(18).unwrap().pos(), Some([10.0, 10.0, 11.0]));
        assert_eq!(player.actor(18).unwrap().render_pos(), Some([-192.0, 10.0, 11.0]));
        assert!(player.has_window());
    }
}
