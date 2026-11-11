//! Owned team-talk door models through the production stage/camera/draw path.
//! This preview verifies the visible motion; strict durable replay is tested by the host.

use super::*;
use crate::camera::FieldCamera;

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; authored team-talk sliding doors"]
fn offscreen_real_ff7_team_talk_doors_show_their_linear_offset_steps() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let def = fmv_ff7::script::scene::team_talk_scene();
    let mut rig = make_rig_with_snapshot(&real, def, true, StoryState::at(6), 600);
    super::train::start_held(&mut rig, None);
    let mut camera = FieldCamera::default();
    let mut captured = BTreeSet::new();
    let mut windows = 0;
    let mut last_window = None;
    let mut ended = false;
    for _ in 0..6000 {
        let draw = super::train::frame(&mut rig, &mut camera, 1.0 / 30.0);
        let scene = rig.state.scene.player().expect("the preview holds its terminal frame");
        if scene.has_window() && last_window != Some(scene.epoch()) {
            last_window = Some(scene.epoch());
            windows += 1;
        }
        let cast = scene.cast_draws();
        if let (Some(left), Some(right)) = (cast.iter().find(|c| c.entity == 6), cast.iter().find(|c| c.entity == 7)) {
            assert_eq!(left.pos, Pos::new(1088.0, 1845.0, 1298.0));
            assert_eq!(right.pos, Pos::new(1263.0, 1824.0, 1298.0));
            let offsets = [(left.render_pos.x - left.pos.x) as i16, (right.render_pos.x - right.pos.x) as i16];
            let sample = match offsets {
                [0, 0] if windows == 10 && !scene.has_window() => Some("closed"),
                [-101, 92] => Some("halfway"),
                [-202, 184] => Some("endpoint"),
                _ => None,
            };
            if let Some(sample) = sample.filter(|sample| captured.insert(*sample)) {
                for entity in [6, 7] {
                    assert!(draw.built.as_ref().unwrap().anchors.contains_key(&super::super::draw::cast_who(entity)),
                        "the actual door model is loaded and projects in frame");
                }
                rig.picture_clipped(&mut gpu, &format!("team-talk-doors-{sample}"), SIZE, true, camera.clip_range(rig.view.range));
            }
        }
        if ended { break; }
        if rig.state.scene.player().unwrap().has_window() { rig.state.scene.key(KeyCode::Enter, true, false); }
        else { rig.state.scene.advance(1.0 / 30.0); }
        for command in rig.state.scene.take_commands() {
            match command {
                super::super::SceneCmd::Step { answer: None, .. } => {},
                super::super::SceneCmd::End { skipped: false, .. } => ended = true,
                other => panic!("unexpected team-talk preview command {other:?}"),
            }
        }
    }
    assert!(ended, "the original door waits finish and the scene reaches its transfer");
    assert_eq!(captured, BTreeSet::from(["closed", "halfway", "endpoint"]));
    eprintln!("team talk: {windows} original windows; loaded door models captured at exact steps0/25/50");
}
