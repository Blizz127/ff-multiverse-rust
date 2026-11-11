//! The original train-map close-ups use OFST in world units, independently of their shared base.

use super::*;
use crate::camera::FieldCamera;

/// Deliberately incomplete presentation for a labeled A/B picture. The original VM still
/// executes OFST and records the exact boundary; only its emitted model offsets are omitted.
/// This source is never used for gameplay or as an acceptance reference for replay.
struct WithoutOffsets(Box<dyn super::super::source::SceneSource>);

fn omit_offsets(mut beat: fmv_ff7::script::scene::Beat) -> fmv_ff7::script::scene::Beat {
    beat.stage.retain(|event| !matches!(event.what, Stage::Offset { .. }));
    beat
}

impl super::super::source::SceneSource for WithoutOffsets {
    fn initial(&self) -> Vec<Stage> { self.0.initial().into_iter().filter(|event| !matches!(event, Stage::Offset { .. })).collect() }
    fn next(&mut self, answer: Option<usize>) -> fmv_ff7::script::scene::Beat { omit_offsets(self.0.next(answer)) }
    fn next_checked(&mut self, answer: Option<usize>) -> Result<fmv_ff7::script::scene::Beat, String> {
        self.0.next_checked(answer).map(omit_offsets)
    }
    fn fingerprint(&self) -> Option<[u8; 32]> { self.0.fingerprint() }
    fn boundary(&self) -> Option<fmv_ff7::script::scene::SceneBoundary> { self.0.boundary() }
    fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> { self.0.animation_clip(entity, slot) }
    fn initial_default_animation_clip(&self, entity: u8) -> Option<(u8, &str)> { self.0.initial_default_animation_clip(entity) }
    fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
}

struct BeforeOffsets(Moved);

impl SceneBuilder for BeforeOffsets {
    fn build(&mut self, scene: &str, story: &StoryState, seed: u32, resources: fmv_ff7::script::scene::SceneResources) -> Result<Built, String> {
        let mut built = self.0.build(scene, story, seed, resources)?;
        built.source = Box::new(WithoutOffsets(built.source));
        Ok(built)
    }
    fn reach(&mut self, index: usize, area: &str) -> Option<f32> { self.0.reach(index, area) }
}

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; original ROOTMAP close-up offsets"]
fn offscreen_real_ff7_rootmap_closeups_keep_their_logical_base_and_authored_offsets() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let def = fmv_ff7::script::scene::train_map_scene();
    let mut story = StoryState::at(51).into_extended().expect("extended story");
    story.write_masked(3, 223, 1 << 2, (1 << 2) | (1 << 4)).unwrap();
    let mut before = make_rig_with_snapshot(&real, def.clone(), true, story.clone(), 600);
    before.state.scene.set_builder(Box::new(BeforeOffsets(Moved {
        adapter: Arc::clone(&real.adapter), def: def.clone(), reach: Ff7Scenes::new(Some(Arc::clone(&real.adapter))),
    })));
    super::train::start_held(&mut before, None);
    let mut before_camera = FieldCamera::default();
    let mut before_pixels = None;
    for _ in 0..1500 {
        super::train::frame(&mut before, &mut before_camera, 1.0 / 30.0);
        if before.state.scene.player().unwrap().has_window() {
            before_pixels = Some(before.picture_clipped(&mut gpu, "rootmap-window-01-without-offsets", SIZE, false, before_camera.clip_range(before.view.range)).0);
            break;
        }
        before.state.scene.advance(1.0 / 30.0);
    }
    assert!(before.state.scene.player().unwrap().has_window(), "the diagnostic reaches the same first dialogue");
    let mut rig = make_rig_with_snapshot(&real, def, true, story, 600);
    super::train::start_held(&mut rig, None);
    let mut camera = FieldCamera::default();
    let mut windows = 0;
    let mut last_window = None;
    let mut ended = false;
    let mut skipped = 0;
    let mut palette_states = BTreeMap::<usize, BTreeSet<Vec<Option<u16>>>>::new();
    for tick in 0..1500 {
        let drawn = super::train::frame(&mut rig, &mut camera, 1.0 / 30.0);
        super::train::participants_hidden(&rig, &drawn);
        let palette = rig.view.background_palette().expect("ROOTMAP's displayed palette");
        for page in [3, 9, 14] {
            palette_states.entry(page).or_default().insert((0..256).map(|index| palette.raw(page, index)).collect());
        }
        let scene = rig.state.scene.player().expect("the close-up remains held through its transfer");
        skipped = skipped.max(scene.counts().anims_skipped);
        let opened = scene.has_window().then(|| scene.epoch());
        let new_window = opened.is_some() && opened != last_window;
        if new_window {
            last_window = opened;
            windows += 1;
            let casts = scene.cast_draws();
            for (entity, expected) in [(12, Pos::new(572.0, -497.0, -1372.0)), (13, Pos::new(618.0, -151.0, -1260.0))] {
                let actor = casts.iter().find(|actor| actor.entity == entity).expect("both original close-up rigs are shown");
                assert_eq!(actor.pos, Pos::new(-18.0, -459.0, -56.0), "OFST never changes the logical base");
                assert_eq!(actor.render_pos, expected, "original signed OFST, without facing or scale applied to it");
                let avatars = super::super::draw::cast_players(scene);
                assert_eq!(avatars.iter().find(|avatar| avatar.who == super::super::draw::cast_who(entity)).unwrap().pos, expected);
                assert!(drawn.built.as_ref().unwrap().anchors.contains_key(&super::super::draw::cast_who(entity)),
                    "the owned close-up model loaded and its offset position projects in front of the camera");
            }
            rig.picture_clipped(&mut gpu, &format!("rootmap-window-{windows:02}"), SIZE, true, camera.clip_range(rig.view.range));
            if windows == 1 {
                // Hide dialogue in both diagnostic pictures so changed panel placement cannot
                // masquerade as a visible model. Palette, camera, fade and HUD are unchanged.
                let pixels = rig.picture_clipped(&mut gpu, "rootmap-window-01-with-offsets", SIZE, false, camera.clip_range(rig.view.range)).0;
                assert!(changed(before_pixels.as_ref().unwrap(), &pixels) > 100,
                    "the authored offsets materially change the first close-up picture");
            }
        }
        if ended { break; }
        if rig.state.scene.player().unwrap().has_window() { rig.state.scene.key(KeyCode::Enter, true, false); }
        else { rig.state.scene.advance(1.0 / 30.0); }
        for command in rig.state.scene.take_commands() {
            match command {
                super::super::SceneCmd::Step { answer: None, .. } => {},
                super::super::SceneCmd::End { skipped: false, .. } => ended = true,
                other => panic!("unexpected ROOTMAP lesson command {other:?}"),
            }
        }
        if tick == 1499 { panic!("ROOTMAP did not reach its authored transfer"); }
    }
    eprintln!("ROOTMAP: {windows} original windows, ended={ended}, animation skips={skipped}; displayed palette states {:?}",
        palette_states.iter().map(|(page, values)| (*page, values.len())).collect::<Vec<_>>());
    assert!(ended);
    assert_eq!(windows, 8);
    assert_eq!(skipped, 0, "every authored close-up gesture must resolve");
    for page in [3, 9, 14] {
        assert!(palette_states[&page].len() > 1, "the displayed palette {page} changes during the original lesson");
    }
}
