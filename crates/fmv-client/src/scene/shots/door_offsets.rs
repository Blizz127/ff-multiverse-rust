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

/// The reactor's three door openings use four model entities, including the two paired
/// leaves. Each fixture invokes one original slot and its real NPC/dialogue dependencies.
/// Given bytes describe a test-only legacy preview, not an authorized durable save or scene.
#[test]
#[ignore = "needs owned FF7 data, FMV_BACKGROUND_TEST_DIR and an offscreen GPU"]
fn offscreen_upscale_nmkin_1_authored_model_doors_keep_their_motion_and_depth() {
    use crate::background_pack::PngFieldPack;
    use fmv_ff7::script::scene::{CastMember, Given, Look, ScriptRef, Step};
    use fmv_ff7::script::FieldScript;

    let Some(real) = open() else { return };
    let Some(root) = std::env::var_os("FMV_BACKGROUND_TEST_DIR") else {
        eprintln!("FMV_BACKGROUND_TEST_DIR is needed for the private reactor door comparison; skipping");
        return;
    };
    let pack = PngFieldPack::open(&PathBuf::from(root)).expect("the explicit private replacement pack");
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("nmkin_1").expect("the owned reactor field");
    let field = area.field_file();
    let script = FieldScript::from_field(field).unwrap();
    let inits = script.entity_inits(None);
    let loader = field.model_loader().unwrap();
    let native_width = FieldView::ff7_with_pack(&real.adapter, "nmkin_1", true, None).unwrap()
        .backdrop.as_ref().unwrap().width;
    let background_ids = |view: &FieldView| (view.backdrop.as_ref().unwrap().id,
        view.stage.as_ref().unwrap().scenery.as_ref().unwrap().id);
    // Logical placements and independently measured integer offsets at steps 0,15,30.
    let doors = [
        (5u8, Pos::new(-1536.0, 4562.0, -271.0), [[-14i16, 0, -22], [68, 0, -22], [150, 0, -22]]),
        (6, Pos::new(-965.0, 2546.0, -273.0), [[0, 0, 0], [0, 92, 0], [0, 184, 0]]),
        (7, Pos::new(-1576.0, 3168.0, -273.0), [[0, 0, 0], [-92, 0, 0], [-184, 0, 0]]),
        (8, Pos::new(-1392.0, 3169.0, -273.0), [[0, 0, 0], [77, 0, 0], [154, 0, 0]]),
    ];
    for (opener, prior_doors, preceding) in [(6u8, 0u8, &[][..]), (7, 8, &[6][..]), (5, 24, &[6, 7, 8][..])] {
        let targets: Vec<_> = doors.iter().filter(|(entity, _, _)| *entity == opener || (opener == 7 && *entity == 8)).collect();
        let mut def = fmv_ff7::script::scene::reactor_door_scene();
        let cloud_start = def.cast.iter().find(|member| member.entity == 3).unwrap().start;
        def.id = format!("test/nmkin_1/owned-door-{opener}-raster-comparison");
        def.presents = None;
        def.setup = [0u8, 3, 4, 5, 6, 7, 8, 9, 10].into_iter().map(|entity| ScriptRef::new(entity, 0)).collect();
        // Door bits3/4 are not part of legacy StoryState::set_bit's tracked mask. These
        // explicit preview values also retain the door5 latch and its first-visit NPC branch.
        def.given = vec![Given { bank: 1, address: 225, value: prior_doors },
            Given { bank: 1, address: 226, value: 0 }, Given { bank: 5, address: 2, value: 0 }];
        def.root = preceding.iter().copied().map(Step::StartAmbientMain).collect();
        def.root.push(Step::Await(ScriptRef::new(opener, 3)));
        def.cast.clear();
        def.speakers = vec![("Cloud".into(), 3), ("Biggs".into(), 9), ("Jessie".into(), 10)];
        for entity in [3u8, 5, 6, 7, 8, 9, 10] {
            let init = inits.iter().find(|init| init.entity == usize::from(entity)).expect("the original participant");
            let model = &loader.models[init.model_index.expect("the participant has its original model")];
            def.cast.push(CastMember { entity, name: init.name.clone(),
                look: Look::Model { hrc: model.hrc_stem(), idle: model.animations.first().expect("the standing clip").stem() },
                start: if entity == 3 { cloud_start } else { None } });
        }
        // Each separate opening finishes all NPC requests it starts. Chaining the roots
        // immediately would overlap Biggs/Jessie's asynchronous movement from the prior door.
        let mut original_samples = BTreeMap::new();
        for (name, selected_pack) in [("native", None), ("syw4x", Some(&pack))] {
            let mut rig = make_rig_with_snapshot(&real, def.clone(), true, StoryState::at(10), 600);
            rig.view = FieldView::ff7_with_pack(&real.adapter, "nmkin_1", true, selected_pack).unwrap();
            rig.view.set_background_texture_limit(gpu.max_texture_dimension_2d()).unwrap();
            assert_eq!(rig.view.backdrop.as_ref().unwrap().width,
                native_width * if selected_pack.is_some() { 4 } else { 1 }, "the requested raster path is active");
            // The fixture's cast is the complete roster under test. Unnamed static doors
            // cannot be hidden by name matching, so do not leave duplicate closed NPCs in
            // the snapshot behind these actual scene actors. The local witness is retained.
            rig.state.apply(0.0, ServerMsg::AreaSnapshot { area: rig.state.area().clone(), tick: 2,
                entities: Vec::new(), npcs: Vec::new() });
            super::train::start_held(&mut rig, None);
            let initial_background = background_ids(&rig.view);
            let mut captured = BTreeSet::new();
            let mut offsets = BTreeMap::<u8, BTreeSet<[i16; 3]>>::new();
            let (mut ended, mut windows, mut last_window) = (false, 0, None);
            for _ in 0..2400 {
                rig.draw();
                let drawn = rig.draw();
                assert_eq!(background_ids(&rig.view), initial_background,
                    "the authored model-door motion does not replace background resources");
                let scene = rig.state.scene.player().expect("the preview holds its terminal frame");
                if scene.has_window() && last_window != Some(scene.epoch()) {
                    last_window = Some(scene.epoch());
                    windows += 1;
                }
                let casts = scene.cast_draws();
                let mut samples = Vec::new();
                for &&(entity, position, phases) in &targets {
                    let actor = casts.iter().find(|actor| actor.entity == entity).expect("the authored door is visible");
                    assert_eq!(actor.pos, position, "OFST does not change the original logical placement");
                    let offset = [(actor.render_pos.x - actor.pos.x) as i16,
                        (actor.render_pos.y - actor.pos.y) as i16, (actor.render_pos.z - actor.pos.z) as i16];
                    offsets.entry(entity).or_default().insert(offset);
                    for (phase, expected) in ["closed", "halfway", "open"].into_iter().zip(phases) {
                        if offset == expected && captured.insert((entity, phase)) {
                            assert!(drawn.built.as_ref().unwrap().anchors.contains_key(&super::super::draw::cast_who(entity)),
                                "the exact original door model is loaded through field_draw");
                            samples.push((entity, phase));
                        }
                    }
                }
                for (entity, phase) in samples {
                    let key = (entity, phase);
                    let (_, drawn) = rig.picture(&mut gpu, &format!("75-nmkin_1-{name}-door-{entity}-{phase}"), SIZE, true);
                    let built = drawn.built.as_ref().unwrap();
                    let geometry = built.frame.models.iter().map(|model| model.positions.clone()).collect::<Vec<_>>();
                    if selected_pack.is_none() { original_samples.insert(key, geometry); }
                    else { assert_eq!(original_samples.get(&key), Some(&geometry),
                        "replacement pixels preserve every model vertex and camera depth at door {entity} {phase}"); }
                }
                if ended { break; }
                if rig.state.scene.player().unwrap().has_window() { rig.state.scene.key(KeyCode::Enter, true, false); }
                else { rig.state.scene.advance(1.0 / 30.0); }
                for command in rig.state.scene.take_commands() {
                    match command {
                        super::super::SceneCmd::Step { answer: None, .. } => {},
                        super::super::SceneCmd::End { skipped: false, .. } => ended = true,
                        other => panic!("unexpected authored-door preview command {other:?}"),
                    }
                }
            }
            assert!(ended, "door{opener}'s original scripts and their NPC requests finish");
            assert_eq!(windows > 0, opener != 5, "doors6/7 advance their original dialogue; door5 has none");
            let expected: BTreeSet<_> = targets.iter().flat_map(|&&(entity, _, _)|
                [(entity, "closed"), (entity, "halfway"), (entity, "open")]).collect();
            assert_eq!(captured, expected, "the actual initial offset, step15 and step30 are all drawn");
            for &&(entity, _, _) in &targets {
                assert!(offsets[&entity].len() >= 25, "door{entity} moves through its authored integer steps");
            }
            eprintln!("{name}: authored door{opener}, {windows} original windows, samples={captured:?}");
        }
    }
}
