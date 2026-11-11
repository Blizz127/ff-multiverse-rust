use super::*;
use crate::avatar::frame::{tests::view, AvatarFrame};
use crate::scene::player::Run;
use crate::scene::tests::{def_in, Scripted};
use crate::ui::{draw::DrawList, hud::{self, HudInput}};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{Beat, CastMember, MoveHow, Stage, Then, Timed};
use fmv_protocol::{SceneRole, SceneRunId};
use std::collections::HashMap;

fn player() -> ScenePlayer {
    let mut def = def_in("test-labels");
    def.cast = vec![
        CastMember { entity: 18, name: "av_b".into(), look: Look::Model { hrc: "BWFD".into(), idle: "acfe".into() }, start: None },
        CastMember { entity: 20, name: "earith".into(), look: Look::Appearance("ff7/aerith".into()), start: None },
        CastMember { entity: 21, name: "av_w".into(), look: Look::Model { hrc: "bxbe".into(), idle: "acfe".into() }, start: None },
        CastMember { entity: 22, name: "unplaced".into(), look: Look::Appearance("ff7/cloud".into()), start: None },
        CastMember { entity: 23, name: "cloud".into(), look: Look::Appearance("ff7/cloud".into()), start: None },
    ];
    def.speakers = vec![("Flower girl".into(), 20), ("Aerith".into(), 20)];
    let source = Scripted {
        initial: vec![
            Stage::Place { entity: 18, x: 10, y: 20, z: Some(30), triangle: Some(1) },
            Stage::Offset { entity: 18, x: 5, y: -4, z: 3 },
            Stage::Place { entity: 20, x: 40, y: 50, z: Some(60), triangle: Some(1) },
            Stage::Place { entity: 21, x: 70, y: 80, z: Some(90), triangle: Some(1) },
            Stage::Show { entity: 21, visible: false },
            Stage::Place { entity: 23, x: 100, y: 110, z: Some(120), triangle: Some(1) },
        ],
        beats: vec![Beat {
            stage: vec![
                Timed { at: 0, what: Stage::Move { entity: 18, from: None, x: 70, y: 20, z: Some(30), triangle: None,
                    frames: 30, how: MoveHow::Walk, animation: None } },
                Timed { at: 15, what: Stage::Show { entity: 20, visible: false } },
            ],
            then: Then::End { at: 300, how: End::Finished },
        }].into(),
        asked: Default::default(),
    };
    ScenePlayer::new(Run { id: SceneRunId(7), participants: vec![], role: SceneRole::Owner }, def, Box::new(source), false)
}

fn built(anchors: HashMap<Who, Pos>) -> Built {
    Built {
        frame: AvatarFrame { models: vec![], tints: vec![], identities: vec![], zoom: 400.0, field_depth: None,
            overlay_bias: 0.0, ground: vec![], scenery: None },
        markers: vec![], anchors,
    }
}

#[test]
fn cast_names_follow_visible_heads_or_moving_offset_markers_and_preserve_contextual_names() {
    let mut scene = player();
    let view = view();
    let names = |labels: &[Label]| labels.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
    let labels = cast_labels(&scene, None, &view);
    assert_eq!(names(&labels), ["Biggs", "Flower girl", "Cloud Strife"], "hidden and unplaced actors have no labels, and scene names take precedence");
    let biggs = labels.iter().find(|l| l.text == "Biggs").unwrap();
    let rendered = view.to_view(Pos::new(15.0, 16.0, 33.0)).unwrap();
    assert_eq!((biggs.x, biggs.y, biggs.kind), (rendered.x, rendered.y, LabelKind::Npc));

    let head = Pos::new(-40.0, 55.0, 0.0);
    let frame = built(HashMap::from([(cast_who(18), head)]));
    let anchored = cast_labels(&scene, Some(&frame), &view);
    assert_eq!((anchored[0].x, anchored[0].y), (head.x, head.y), "the actual drawn head takes precedence over the marker");
    assert_eq!(anchored[1], labels[1], "a model that did not load retains its marker label");

    scene.advance(0.5);
    let moved = cast_labels(&scene, None, &view);
    assert_eq!(names(&moved), ["Biggs", "Cloud Strife"], "an authored hide removes the label with its body");
    let rendered = view.to_view(Pos::new(45.0, 16.0, 33.0)).unwrap();
    assert_eq!((moved[0].x, moved[0].y), (rendered.x, rendered.y), "the fallback follows both MOVE and the model offset");
    assert_ne!((moved[0].x, moved[0].y), (labels[0].x, labels[0].y));
}

#[test]
fn scene_labels_replace_old_npc_names_and_use_existing_hud_culling_and_player_priority() {
    let scene = player();
    let view = view();
    let npc = NpcView { id: 1, name: "av_b".into(), model: "bwfd".into(), idle_animation: None,
        pos: Pos::new(-10_000.0, 0.0, 0.0), facing: 0.0 };
    assert!(label_hidden(Some(&scene), Subject::Npc(1), &[npc]));
    assert!(!label_hidden(Some(&scene), Subject::Own, &[]), "a player's name remains their own even beside the canon cast");
    let frame = built(HashMap::from([
        (cast_who(18), Pos::new(10_000.0, 0.0, 0.0)),
        (cast_who(20), Pos::new(0.0, 0.0, 0.0)),
        (cast_who(23), Pos::new(0.0, 0.0, 0.0)),
    ]));
    let mut labels = cast_labels(&scene, Some(&frame), &view);
    labels.push(Label { x: 0.0, y: 0.0, text: "Blizz".into(), kind: LabelKind::Own });
    let input = HudInput { bounds: view.bounds, labels, area: "Test field".into(), fixture: false,
        players: 1, rtt: None, party: vec![], invite_from: None };
    let mut out = DrawList::new();
    hud::draw_hud(&input, 1100, 680, &mut out);
    let shown: Vec<&str> = out.texts().iter().map(|t| t.text.as_str()).collect();
    assert!(!shown.contains(&"Biggs"), "a cast head beyond the camera uses the same off-screen culling as players");
    for name in ["Flower girl", "Cloud Strife"] {
        assert_eq!(input.labels.iter().filter(|label| label.text == name).count(), 1);
        assert!(shown.contains(&name));
    }
    let at = |name| shown.iter().position(|n| *n == name).unwrap();
    assert!(at("Blizz") > at("Flower girl") && at("Blizz") > at("Cloud Strife"), "the local player's name is drawn above the cast labels");
}

#[test]
fn a_station_trigger_actor_is_the_participants_body_not_a_second_cloud_or_name() {
    let mut def = def_in("md1stin");
    def.id = "ff7/md1stin/station-guards".into();
    def.cast = vec![CastMember { entity: 1, name: "cloud".into(), look: Look::Appearance("ff7/cloud".into()), start: None }];
    def.speakers = vec![("Cloud".into(), 1)];
    let source = Scripted {
        initial: vec![Stage::Place { entity: 1, x: 10, y: 20, z: Some(30), triangle: Some(12) }],
        beats: vec![Beat { stage: vec![], then: Then::End { at: 300, how: End::Finished } }].into(),
        asked: Default::default(),
    };
    let run = Run { id: SceneRunId(7), participants: vec![fmv_protocol::SceneParticipant { entity: EntityId(42), spot: 0 }], role: SceneRole::Owner };
    let scene = ScenePlayer::new(run, def, Box::new(source), false);
    assert_eq!(scene.cast_draws().len(), 1, "the VM retains the actor at the actual trigger position");
    assert!(cast_players(&scene).is_empty(), "the field keeps drawing the participant's own body");
    assert!(cast_labels(&scene, None, &view()).is_empty(), "there is no duplicate canon name over the player");
    assert!(!participant_label_hidden(Some(&scene), EntityId(42), Subject::Own));
    assert_eq!(camera_shot(&scene, &view()).aim, SceneAim::Lead { lead: None }, "the field camera follows the real player instead of a hidden cast clone");
}
