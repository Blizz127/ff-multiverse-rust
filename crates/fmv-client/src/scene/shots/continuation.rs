//! Real-data presentation checks for the post-flower continuation. These exercise field
//! staging separately from the server's durable battle and destination handoff tests.

use super::*;
use crate::camera::FieldCamera;
use crate::window::camera_input;

/// Exercise every authored answer path through the real client continuation API. Battles are
/// acknowledged with synthetic victorious resource snapshots here; combat and durable writes
/// have separate server tests. These pictures check the field cast and camera on both sides.
/// Canon Cloud begins at a server-accepted crossing, rebound through the same shared helper
/// as live playback. The midpoint in the unbound survey definition is not used.
#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; all bridge presentation paths"]
fn offscreen_real_ff7_bridge_choices_and_battle_resumes() {
    use fmv_ff7::script::scene::{BattleResolution, BattleResume, SceneBoundary, SceneResources, WindowAck};
    use fmv_protocol::{BattleId, SceneOrigin, SceneReplay};
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("md8brdg").expect("bridge field");
    for mask in 0u8..8 {
        let origin = SceneOrigin { pos: Pos::new(-426.0, 830.0, 518.0), facing: 0.0, triangle: None };
        let mut def = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &fmv_ff7::script::scene::bridge_scene(),
            origin.pos, origin.facing, origin.triangle).expect("accepted crossing binds Cloud's entrance");
        let mut story = StoryState::at(39).into_extended().expect("exact story");
        def.witness = fmv_ff7::script::scene::auto_witnesses_with_resources(area.field_file(), &def, &story, SEED, SceneResources { gil: Some(600) }, 3);
        let mut rig = make_rig_with_snapshot(&real, def.clone(), true, story.clone(), 600);
        let mut shadow = ScenePlay::with_resources(area.field_file(), &def, &story, SEED, SceneResources { gil: Some(600) }).unwrap();
        let replay = SceneReplay { source: shadow.replay_identity().fingerprint, origin: Some(origin), events: Vec::new() };
        shadow.next_strict(None).unwrap();
        let run = SceneRunId(1);
        rig.state.apply(0.0, ServerMsg::SceneStart {
            run, scene: def.id.clone(), seed: SEED, progress: story.encode(), gil: 600, replay: Some(replay),
            owner: EntityId(1), participants: (0..3).map(|spot| SceneParticipant { entity: EntityId(u64::from(spot) + 1), spot }).collect(),
            you: SceneRole::Owner,
        });
        assert!(rig.state.scene.is_active(), "strict bridge scene built");
        let answers = [if mask & 1 == 0 { 3 } else { 2 }, if mask & 2 == 0 { 1 } else { 0 }, if mask & 4 == 0 { 1 } else { 0 }];
        let mut camera = FieldCamera::default();
        let (mut last_window, mut windows, mut questions, mut skipped, mut ended) = (None, 0, 0, 0, false);
        let mut battles = Vec::new();
        let mut camera_checked = 0;
        let mut camera_misses = Vec::new();
        for _ in 0..2400 {
            rig.draw();
            let draw = rig.draw();
            rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 0.25));
            let player = rig.state.scene.player().expect("held through the entire bridge scene");
            skipped = skipped.max(player.counts().anims_skipped);
            let window = player.panel().map(|p| (player.epoch(), p.window().clone(), p.on_choices(SIZE), p.position().1));
            if let Some((epoch, window, choices, cursor)) = window {
                if last_window != Some(epoch) {
                    last_window = Some(epoch);
                    windows += 1;
                    if windows <= 2 {
                        let cloud = player.cast_draws().into_iter().find(|c| c.entity == 1).expect("Cloud stands at the crossing");
                        assert_eq!(cloud.pos, origin.pos, "the first two windows retain the accepted crossing position");
                    }
                    if let (Some(cloud), Some(range)) = (player.cast_draws().into_iter().find(|c| c.entity == 1), rig.view.range) {
                        let at = rig.view.to_view(cloud.pos).expect("Cloud projects into the field");
                        // The authored camera stops at the field's range even when an actor walks
                        // beyond it. Measure only places that its screen can actually show.
                        if (range.min.x..=range.max.x).contains(&at.x) && (range.min.y..=range.max.y).contains(&at.y) {
                            camera_checked += 1;
                            let px = view_to_window(&rig.view.bounds, SIZE.0, SIZE.1, at.x, at.y);
                            if !(0.0..=SIZE.0 as f32).contains(&px[0]) || !(0.0..=SIZE.1 as f32).contains(&px[1]) {
                                camera_misses.push(windows);
                            }
                        }
                    }
                    if mask == 0 || mask == 7 {
                        rig.picture_clipped(&mut gpu, &format!("bridge-path-{mask}-window-{windows:02}"), SIZE, true, camera.clip_range(rig.view.range));
                    }
                }
                if choices {
                    let first = window.message.choice.expect("choice range").first;
                    for _ in 0..cursor { rig.state.scene.key(KeyCode::ArrowUp, true, false); }
                    for _ in first..answers[questions] { rig.state.scene.key(KeyCode::ArrowDown, true, false); }
                    questions += 1;
                }
                rig.state.scene.key(KeyCode::Enter, true, false);
            } else {
                rig.state.scene.advance(0.25);
            }
            for cmd in rig.state.scene.take_commands() {
                match cmd {
                    super::super::SceneCmd::Step { step, answer, .. } => {
                        let Some(SceneBoundary::Window(checkpoint)) = shadow.boundary() else { panic!("unexpected client window {step}") };
                        assert_eq!(checkpoint.ordinal, step + 1, "VM window ordinals are one-based; wire steps count passed windows");
                        shadow.next_strict(Some(WindowAck { checkpoint, answer })).unwrap();
                    }
                    super::super::SceneCmd::BattleReady { ordinal, step, .. } => {
                        let Some(SceneBoundary::Battle(checkpoint)) = shadow.boundary() else { panic!("unexpected client battle {ordinal}") };
                        assert_eq!(checkpoint.ordinal, ordinal);
                        assert_eq!(step, windows);
                        battles.push(checkpoint.formation);
                        let battle_id = BattleId(u64::from(ordinal) + 1);
                        rig.state.apply(0.0, ServerMsg::SceneBattleStarted { run, ordinal, battle_id });
                        let before = rig.state.scene.player().unwrap().cast_draws().iter().map(|c| (c.entity, c.pos, c.frame)).collect::<Vec<_>>();
                        rig.state.scene.advance_visible(2.0, false);
                        let after = rig.state.scene.player().unwrap().cast_draws().iter().map(|c| (c.entity, c.pos, c.frame)).collect::<Vec<_>>();
                        assert_eq!(before, after, "the battle freezes the field cast");
                        let prior = story.clone();
                        let snapshot = shadow.snapshot().unwrap();
                        snapshot.writes.apply_extended_to(&mut story, &prior).unwrap();
                        let gil = snapshot.resources.gil_after.unwrap() + 100;
                        shadow.resume_battle_checked(BattleResume { checkpoint, resolution: BattleResolution::Victory, story: story.clone(), resources: SceneResources { gil: Some(gil) } }).unwrap();
                        shadow.next_strict(None).unwrap();
                        rig.state.apply(0.0, ServerMsg::SceneBattleResumed { run, ordinal, battle_id, progress: story.encode(), gil });
                        rig.state.scene.advance_visible(2.0, false);
                        assert_eq!(rig.state.scene.player().unwrap().cast_draws().iter().map(|c| (c.entity, c.pos, c.frame)).collect::<Vec<_>>(), before,
                            "durable acknowledgement still waits for the battle result panel");
                        rig.state.scene.advance_visible(0.0, true);
                    }
                    super::super::SceneCmd::End { skipped: false, .. } => { ended = true; }
                    other => panic!("unexpected bridge command {other:?}"),
                }
            }
            if ended { break; }
        }
        let expected: Vec<u16> = (0..3).filter(|bit| mask & (1 << bit) != 0).map(|bit| 328 + bit).collect();
        eprintln!("bridge path {mask}: {windows} windows, {questions} questions, battles {battles:?}, skipped animations {skipped}, camera checked {camera_checked}, misses {camera_misses:?}");
        assert!(ended, "bridge path {mask} completed");
        assert_eq!(questions, 3);
        assert_eq!(battles, expected);
        assert_eq!(windows, [8, 9, 10, 11, 10, 11, 12, 13][usize::from(mask)]);
        assert_eq!(shadow.boundary(), Some(SceneBoundary::End(fmv_ff7::script::dialogue::End::MapJump { field: 138 })));
        assert_eq!(skipped, 0, "bridge path {mask}: every cast gesture is supported");
        assert!(camera_checked > 0 && camera_misses.is_empty(), "bridge path {mask}: Cloud stays in view at its windows");
    }
}

/// The train starts with its crew, then shows Cloud at the authored placement before its
/// eleventh window. The actual window pipeline must preserve that staging and play every
/// supported gesture. This records the first, tenth, eleventh and final windows for inspection.
#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; cargo arrival presentation"]
fn offscreen_real_ff7_cargo_arrival_cast() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let mut def = fmv_ff7::script::scene::cargo_train_scene();
    // MAPJUMP's (0,0,triangle0) is a placeholder. This is Cloud's measured eventual
    // control handoff; the scene keeps the incoming player held during the introduction.
    def.arrival = Spot { x: -24, y: 16, triangle: 20, direction: 0 };
    let story = StoryState::at(39).into_extended().expect("exact story snapshot");
    let area = real.adapter.load_field("cargoin").expect("cargo field");
    def.witness = fmv_ff7::script::scene::auto_witnesses_with_resources(area.field_file(), &def, &story, SEED,
        fmv_ff7::script::scene::SceneResources { gil: Some(600) }, 3);
    assert_eq!(def.witness.len(), 3, "all participants get separate witness positions");
    let mut rig = make_rig_with_snapshot(&real, def, true, story, 600);
    rig.state.apply(0.0, ServerMsg::SceneStart {
        run: SceneRunId(1), scene: rig.def.id.clone(), seed: SEED, progress: rig.story.encode(), gil: rig.gil,
        replay: Some(fmv_protocol::SceneReplay { source: rig.play.replay_identity().fingerprint, origin: None, events: Vec::new() }),
        owner: EntityId(1), participants: (0..3).map(|spot| SceneParticipant { entity: EntityId(u64::from(spot) + 1), spot }).collect(),
        you: SceneRole::Owner,
    });
    assert!(rig.state.scene.is_active(), "the held arrival is built");
    // An unrelated visitor must survive the participant filter in both renderer paths.
    let visitor_tint = [211, 1, 203];
    let visitor_at = spot_pos(&rig, &rig.def.witness[0]);
    rig.state.apply(0.0, ServerMsg::PlayerJoined { snapshot: EntitySnapshot {
        id: EntityId(4), pos: visitor_at, facing: 0.0, anim: AnimState::Idle,
        avatar: AvatarAppearance::new("Visitor", visitor_tint, Some(rig.looks[0].into())),
    } });
    rig.draw();
    let visitors = rig.draw();
    assert!(visitors.built.as_ref().unwrap().anchors.contains_key(&Who(4)), "an unrelated player's model remains visible");
    let fallback = field_draw(None, &mut rig.npc_epoch, &rig.view, &rig.state, rig.own_tint, 1.0, 0.0);
    assert!(fallback.markers.iter().any(|m| m.tint == visitor_tint), "its fallback marker remains visible too");
    assert!(!fallback.markers.iter().any(|m| m.own), "the held owner's fallback marker is hidden");
    rig.state.apply(0.0, ServerMsg::PlayerLeft { entity: EntityId(4) });
    let mut camera = FieldCamera::default();
    let mut last_window = None;
    let mut windows = 0;
    let mut skipped = 0;
    let mut seen = BTreeSet::new();
    let mut visible_participants = BTreeSet::new();
    let mut intro_centre = None;
    let mut problems = Vec::new();
    let mut commands = Vec::new();
    let mut completed = false;
    for _ in 0..2400 {
        if !rig.state.scene.is_active() { break; }
        rig.draw();
        let draw = rig.draw();
        rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 0.25));
        let scene = rig.state.scene.player().expect("the scene is active");
        if scene.counts().anims_skipped > skipped {
            eprintln!("cargo skip after {windows} windows: counts {:?}, cast {:?}", scene.counts(),
                scene.cast_draws().iter().map(|c| (c.entity, c.clip)).collect::<Vec<_>>());
        }
        skipped = skipped.max(scene.counts().anims_skipped);
        for actor in scene.cast_draws() { seen.insert(actor.entity); }
        if scene.has_window() && last_window != Some(scene.epoch()) {
            last_window = Some(scene.epoch());
            windows += 1;
            let cloud = scene.cast_draws().into_iter().find(|actor| actor.entity == 11).map(|actor| actor.pos);
            if windows <= 10 && cloud.is_some() {
                problems.push(format!("window {windows}: Cloud appeared before his authored entrance"));
            }
            if windows >= 11 && cloud != Some(Pos::new(-24.0, 16.0, 0.0)) {
                problems.push(format!("window {windows}: Cloud is at {cloud:?}, expected his authored placement"));
            }
            assert!(draw.own_at.is_none(), "the hidden participant cannot steer the camera");
            let built = draw.built.as_ref().expect("real models built");
            for entity in 1..=3 {
                if built.anchors.contains_key(&Who(entity)) { visible_participants.insert(entity); }
            }
            assert!(super::super::draw::participant_label_hidden(Some(scene), EntityId(1), Subject::Own));
            assert!(super::super::draw::participant_label_hidden(Some(scene), EntityId(1), Subject::Remote(EntityId(2))));
            assert!(!super::super::draw::participant_label_hidden(Some(scene), EntityId(1), Subject::Remote(EntityId(4))), "unrelated players remain visible");
            if windows <= 10 {
                let centre = [(rig.view.bounds.min.x + rig.view.bounds.max.x) * 0.5,
                    (rig.view.bounds.min.y + rig.view.bounds.max.y) * 0.5];
                assert_eq!(*intro_centre.get_or_insert(centre), centre, "the introduction camera stays put before Cloud appears");
            }
            if [1, 10, 11, 26].contains(&windows) {
                rig.picture_clipped(&mut gpu, &format!("cargo-arrival-window-{windows:02}"), SIZE, true, camera.clip_range(rig.view.range));
            }
        }
        if rig.state.scene.player().is_some_and(|p| p.has_window()) {
            rig.state.scene.key(KeyCode::Enter, true, false);
        } else {
            rig.state.scene.advance(0.25);
        }
        let new = rig.state.scene.take_commands();
        completed |= new.iter().any(|c| matches!(c, super::super::SceneCmd::End { skipped: false, .. }));
        commands.extend(new);
        if completed { break; }
    }
    eprintln!("cargo: {windows} windows, seen cast {seen:?}, skipped animations {skipped}, visible participant bodies {visible_participants:?}");
    assert_eq!(windows, 26, "every authored arrival window was displayed");
    assert_eq!(seen, BTreeSet::from([11, 12, 13, 14, 15]), "all five cast models appear");
    assert!(commands.iter().any(|c| matches!(c, super::super::SceneCmd::End { skipped: false, .. })), "the introduction finishes normally");
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(visible_participants.is_empty(), "participants stay hidden throughout the held cargo introduction");
    assert!(rig.state.scene.player().unwrap().hides_participant(EntityId(1)), "local completion still waits for the durable server acknowledgement");
    rig.state.apply(0.0, ServerMsg::SceneEnded { run: SceneRunId(1), outcome: SceneOutcome::Finished });
    assert!(!rig.state.scene.is_active());
    rig.draw();
    assert!(rig.draw().built.as_ref().unwrap().anchors.contains_key(&Who(1)), "the durable control handoff shows the player's own body");
    assert_eq!(skipped, 0, "every arrival gesture is supported");
}
