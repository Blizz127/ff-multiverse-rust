//! Original field fades through the same renderer as the window, including dialogue ordering.

use super::*;
use crate::camera::FieldCamera;
use crate::window::camera_input;
use fmv_ff7::script::dialogue::MessageOrigin;
use fmv_ff7::script::scene::{FadeBlend, FadeColor};

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; Scorpion's original red tint"]
fn offscreen_real_ff7_scorpion_fade_before_dialogue() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let mut rig = make_rig_with_snapshot(&real, fmv_ff7::script::scene::guard_scorpion_scene(), true, StoryState::at(26), 600);
    rig.start();
    let mut camera = FieldCamera::default();
    let mut found = false;
    for _ in 0..1200 {
        let draw = rig.draw();
        rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 1.0 / 30.0));
        let player = rig.state.scene.player().expect("the Scorpion introduction continues");
        if player.panel().is_some_and(|p| matches!(p.window().message.origin, MessageOrigin::Canon { entity: 3, dialog: 81 })) {
            found = true;
            break;
        }
        if player.has_window() { rig.state.scene.key(KeyCode::Enter, true, false); }
        else { rig.state.scene.advance(1.0 / 30.0); }
    }
    assert!(found, "the dialogue immediately before the original fade is reached");
    // Closing dialog81 starts director1/3 in parallel: WAIT10, then FADE2 with
    // RGB255,200,200. The next window opens at64, after this16-frame tint completes.
    while rig.state.scene.player().unwrap().has_window() {
        rig.state.scene.key(KeyCode::Enter, true, false);
    }
    rig.state.scene.advance(10.0 / 30.0);
    assert_eq!(rig.state.scene.player().unwrap().fade(), Some(FadeColor { blend: FadeBlend::Subtract, rgb: [0, 0, 0] }));
    rig.picture_clipped(&mut gpu, "scorpion-fade-start", SIZE, true, camera.clip_range(rig.view.range));
    rig.state.scene.advance(8.0 / 30.0);
    assert_eq!(rig.state.scene.player().unwrap().fade(), Some(FadeColor { blend: FadeBlend::Subtract, rgb: [0, 28, 28] }));
    rig.picture_clipped(&mut gpu, "scorpion-fade-eight-frames", SIZE, true, camera.clip_range(rig.view.range));
    rig.state.scene.advance(8.0 / 30.0);
    assert_eq!(rig.state.scene.player().unwrap().fade(), Some(FadeColor { blend: FadeBlend::Subtract, rgb: [0, 55, 55] }));
    rig.picture_clipped(&mut gpu, "scorpion-fade-sixteen-frames", SIZE, true, camera.clip_range(rig.view.range));
    rig.state.scene.advance(38.0 / 30.0);
    let player = rig.state.scene.player().unwrap();
    assert!(player.panel().is_some_and(|p| matches!(p.window().message.origin, MessageOrigin::Canon { entity: 3, dialog: 82 })));
    let (plain, draw) = rig.picture_with_fade(&mut gpu, "scorpion-window-without-tint", SIZE, true, camera.clip_range(rig.view.range), None);
    let (tinted, _) = rig.picture_clipped(&mut gpu, "scorpion-window-with-tint", SIZE, true, camera.clip_range(rig.view.range));
    assert_ne!(plain, tinted, "the authored fade visibly changes the real field");
    // Find an opaque piece of the original dialogue background. All its pixels must
    // remain identical: the fade is composited before dialogue, not over the whole UI.
    let player = rig.state.scene.player().unwrap();
    let mut panel = DrawList::new();
    let spot = speaker_spot(player, draw.built.as_ref(), &rig.view, SIZE.0, SIZE.1);
    player.draw(SIZE.0, SIZE.1, 0.0, spot.as_ref(), &mut panel);
    let opaque = panel.quads().iter().filter(|q| q.color[3] == 255 && q.w > 10 && q.h > 2)
        .max_by_key(|q| q.w * q.h).expect("an opaque dialogue background");
    let mut checked = 0;
    for y in (opaque.y + 1).max(0)..(opaque.y + opaque.h - 1).min(SIZE.1 as i32) {
        for x in (opaque.x + 1).max(0)..(opaque.x + opaque.w - 1).min(SIZE.0 as i32) {
            let i = (y as usize * SIZE.0 as usize + x as usize) * 4;
            assert_eq!(&plain[i..i + 4], &tinted[i..i + 4], "dialogue pixel({x},{y}) is not tinted");
            checked += 1;
        }
    }
    assert!(checked > 100, "a substantial part of the panel was compared");
    eprintln!("Scorpion fade: exact8/16-frame colours and {checked} unchanged dialogue pixels");
}
