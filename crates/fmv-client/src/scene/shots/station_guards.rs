//! The original station guard approach through the production scene builder and field renderer.
//! This stops at the actual Battle300 handoff; it does not invent a combat result or replay ACK.

use super::*;
use crate::camera::FieldCamera;
use crate::scene::{draw::cast_labels, SceneCmd};
use crate::ui::hud::LabelKind;
use crate::window::camera_input;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{station_guard_scene, SceneBoundary};

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; original station guards and accepted player origin"]
fn offscreen_real_ff7_station_guards_keep_the_player_and_names_at_the_accepted_crossing() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("md1stin").expect("the owned station");
    let mesh = area.field_file().walkmesh().expect("the original station floor");
    let triangle = mesh.triangles[12];
    // A measured interior point, distinct from the definition's offline preview centroid.
    // The test supplies this as the server's accepted crossing; it does not claim to test movement validation.
    let weighted = |coordinate: fn(&fmv_ff7::field::Vertex) -> i16| {
        triangle.iter().zip([2.0, 3.0, 5.0]).map(|(v, weight)| f32::from(coordinate(v)) * weight).sum::<f32>() / 10.0
    };
    let (x, y) = (weighted(|v| v.x).round() as i16, weighted(|v| v.y).round() as i16);
    assert!(triangle_contains_xy(&triangle, i32::from(x), i32::from(y)));
    let mut def = station_guard_scene();
    assert_ne!((x, y), (def.arrival.x, def.arrival.y), "the accepted crossing is not a canned preview placement");
    def.arrival = Spot { x, y, triangle: 12, direction: 96 };
    let mut rig = make_rig_with_snapshot(&real, def, false, StoryState::fresh_first_control(), 600);
    rig.view = FieldView::ff7_with_pack(&real.adapter, "md1stin", true, None).expect("the original background");
    rig.view.set_background_texture_limit(gpu.max_texture_dimension_2d()).unwrap();
    let accepted = (rig.state.own.pos(), rig.state.own.facing());
    let own_look = rig.state.own_appearance().map(str::to_string);
    assert_eq!(rig.story.moment, 1);

    // The separately interpreted source identifies the boundary; no local test tells the
    // client to finish early or supplies a made-up battle acknowledgement.
    let beat = rig.play.next_strict(None).expect("the original director reaches its next boundary");
    let end_frame = match beat.then { Then::End { at, how: End::Battle } => at, other => panic!("expected Battle300, got {other:?}") };
    let Some(SceneBoundary::Battle(boundary)) = rig.play.boundary() else { panic!("the source has a battle checkpoint") };
    assert_eq!((boundary.entity, boundary.instruction_offset, boundary.formation), (0, 1222, 300));
    assert!(end_frame > 2, "the approach has visible authored movement");
    assert!(!beat.stage.iter().any(|event| matches!(event.what, Stage::Move { entity: 1, .. })), "the director never moves the player");

    rig.start();
    let mut camera = FieldCamera::default();
    let mut positions = BTreeMap::<u8, BTreeSet<(i32, i32, i32)>>::new();
    let mut labelled_on_screen = BTreeSet::new();
    let mut pictures = Vec::new();
    let mut handoff = None;
    for tick in 0..=end_frame + 2 {
        assert!(rig.state.scene.is_active(), "the approach remains active before its source handoff");
        rig.draw();
        let drawn = rig.draw();
        rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &drawn, SIZE, 1.0 / 30.0));
        let scene = rig.state.scene.player().expect("the original approach is still playing");
        let built = drawn.built.as_ref().expect("the owned field models are built");
        assert!(!scene.is_durable(), "this is the ordinary presentation path, not a fabricated continuation");
        assert!(!scene.has_window(), "the guard approach contains no dialogue to auto-confirm");
        assert!(scene.witnesses().is_empty(), "the accepted crossing is never replaced by a spectator spot");
        assert_eq!((rig.state.own.pos(), rig.state.own.facing()), accepted);
        assert_eq!(rig.state.own_appearance(), own_look.as_deref());
        assert_eq!(drawn.own_at, rig.view.to_view(accepted.0));
        assert!(built.anchors.contains_key(&Who(1)), "the player's own body remains drawn");
        assert!(!built.anchors.contains_key(&cast_who(1)), "the VM-only Cloud does not produce a duplicate body");
        assert_eq!(scene.counts().off_cast, 0);
        assert_eq!((scene.counts().anims_skipped, scene.counts().moves_skipped), (0, 0));

        let labels = cast_labels(scene, Some(built), &rig.view);
        let guards: Vec<_> = scene.cast_draws().into_iter().filter(|actor| [4, 5].contains(&actor.entity)).collect();
        assert_eq!(labels.len(), guards.len(), "only the visible guards get cast labels; the player keeps its own name");
        for guard in guards {
            positions.entry(guard.entity).or_default().insert((guard.render_pos.x.round() as i32, guard.render_pos.y.round() as i32, guard.render_pos.z.round() as i32));
            let head = built.anchors.get(&cast_who(guard.entity)).expect("the original guard is a model, not a fallback marker");
            let name = &scene.def().member(guard.entity).unwrap().name;
            let label = labels.iter().find(|label| &label.text == name).expect("the existing guard name follows its body");
            assert_eq!((label.x, label.y, label.kind), (head.x, head.y, LabelKind::Npc));
            let [sx, sy] = view_to_window(&rig.view.bounds, SIZE.0, SIZE.1, head.x, head.y);
            if (0.0..SIZE.0 as f32).contains(&sx) && (0.0..SIZE.1 as f32).contains(&sy) { labelled_on_screen.insert(guard.entity); }
        }
        let anchor = |subject: Subject, at: Pos| {
            if label_hidden(Some(scene), subject, rig.state.npc_views())
                || super::super::draw::participant_label_hidden(Some(scene), rig.state.self_entity(), subject) { None }
            else { label_anchor(Some(built), &rig.view, drawn.own_who, subject, at) }
        };
        let own_hud = hud::gather_anchored(&rig.state, &rig.info(), &rig.view, 1.0, &anchor);
        let own: Vec<_> = own_hud.labels.iter().filter(|label| label.kind == LabelKind::Own).collect();
        assert_eq!(own.len(), 1);
        let head = built.anchors.get(&Who(1)).unwrap();
        assert_eq!((own[0].text.as_str(), own[0].x, own[0].y), ("Wedge", head.x, head.y));

        if [0, end_frame / 2, end_frame.saturating_sub(1)].contains(&tick) {
            let (image, _) = rig.picture_clipped(&mut gpu, &format!("station-guards-frame-{tick:03}"), SIZE, true, camera.clip_range(rig.view.range));
            pictures.push(image);
        }
        rig.state.scene.advance(1.0 / 30.0);
        let commands = rig.state.scene.take_commands();
        if !commands.is_empty() {
            assert_eq!(commands, [SceneCmd::End { run: SceneRunId(1), skipped: false }], "the source battle boundary hands off once through the ordinary scene-end path");
            handoff = Some(tick + 1);
            break;
        }
    }
    assert!(handoff.is_some(), "the original guard approach reaches the server handoff");
    assert!(handoff.unwrap().abs_diff(end_frame) <= 1, "handoff follows the source's presentation clock, allowing one fractional-frame rounding step");
    for entity in [4, 5] {
        assert!(positions.get(&entity).is_some_and(|path| path.len() > 3), "guard {entity} moves visibly along its original path");
        assert!(labelled_on_screen.contains(&entity), "guard {entity}'s named model enters the player's view");
    }
    assert!(pictures.len() >= 2 && pictures.first() != pictures.last(), "the inspected GPU pictures show different approach frames");
    assert!(rig.state.scene.player().is_none(), "the ordinary prelude is relinquished at handoff");
    assert_eq!((rig.state.own.pos(), rig.state.own.facing()), accepted, "ending the presentation never moves the player");
    assert!(!rig.state.battle.in_battle(), "only a server encounter can start Battle300; the test fabricates none");
    rig.state.scene.advance(1.0);
    assert!(rig.state.scene.take_commands().is_empty(), "the handoff is not resent by another frame");
    eprintln!("station guards: source Battle300 at frame {end_frame}, handed off at {:?}, named guards {:?}, owner kept its accepted position", handoff, labelled_on_screen);
}
