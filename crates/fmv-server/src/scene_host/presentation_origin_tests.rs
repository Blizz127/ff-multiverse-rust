//! The station presentation's accepted origin must select its actual authored branch.
use super::*;
use fmv_protocol::SceneOrigin;

#[test]
#[ignore = "needs owner's FMV_FF7_DATA; reads original field data without writing saves"]
fn owned_station_origin_requires_the_reviewed_field_triangle_story_and_server_flag() {
    use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
    let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
    let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
    let mut host = Ff7SceneHost::with_extended_save(opened.adapter, true);
    let scene = "ff7/md1stin/station-guards";
    let area = host.field("md1stin").unwrap();
    let mesh = area.field_file().walkmesh().unwrap();
    let at = |triangle: usize| {
        let vertices = &mesh.triangles[triangle];
        SceneOrigin {
            pos: Pos::new(vertices.iter().map(|v| f32::from(v.x)).sum::<f32>() / 3.0,
                vertices.iter().map(|v| f32::from(v.y)).sum::<f32>() / 3.0,
                vertices.iter().map(|v| f32::from(v.z)).sum::<f32>() / 3.0),
            facing: 0.75, triangle: Some(triangle as u32),
        }
    };
    let origin = at(12);
    let story = StoryState::fresh_first_control();
    let before = story.encode();
    let mut after = story.clone();
    after.moment = 6;
    assert_eq!(host.point_scene("md1stin", &before, &after.encode()).as_deref(), Some(scene));
    assert!(host.requires_presentation_origin(scene));
    assert!(host.presentation_origin_valid(scene, "md1stin", &before, &origin));
    assert!(host.presentation_origin_valid(scene, "md1stin", &before, &SceneOrigin { triangle: None, ..origin }));
    for (id, field, story, origin) in [
        (scene, "md1_1", before.clone(), origin),
        ("ff7/md1stin/unknown", "md1stin", before.clone(), origin),
        (scene, "md1stin", StoryState::at(1).encode(), origin),
        (scene, "md1stin", StoryState::fresh_native_first_control().encode(), origin),
        (scene, "md1stin", StoryState::at(6).into_extended().unwrap().encode(), origin),
        (scene, "md1stin", before.clone(), at(25)),
        (scene, "md1stin", before.clone(), SceneOrigin { triangle: Some(u32::MAX), ..origin }),
        (scene, "md1stin", before.clone(), SceneOrigin { facing: f32::NAN, ..origin }),
        (scene, "md1stin", before.clone(), SceneOrigin { pos: Pos::new(0.0, 0.0, 0.0), ..origin }),
    ] {
        assert!(!host.presentation_origin_valid(id, field, &story, &origin), "{id} {field} {origin:?}");
    }
    host.extended_save = false;
    assert!(host.point_scene("md1stin", &before, &after.encode()).is_none());
    assert!(!host.presentation_origin_valid(scene, "md1stin", &before, &origin));
}
