//! Unit tests of the battle model loader over a synthetic `battle.lgp` (invented data, written
//! field by field through the independent builders of the parsers' own test modules).

use super::animation::tests::{build_pack, encode_clip};
use super::animation::RawFrame;
use super::skeleton::tests::SkeletonSpec;
use super::*;
use crate::model::{mat4_identity, transform_point};
use crate::pmodel::tests::PSpec;
use crate::tex::tests::TexSpec;
use crate::testutil::build_lgp;
use std::io::Cursor;

fn frame(position: [i32; 3], rotations: &[[i16; 3]]) -> RawFrame {
    RawFrame { position, rotations: rotations.to_vec() }
}

/// One untextured triangle, with no normals at all (as every battle mesh has none).
fn one_triangle_mesh() -> Vec<u8> {
    PSpec {
        normals: vec![],
        groups: vec![(0, 1, 0, 3, 0, false, 0)],
        polygons: vec![([0, 1, 2], [0, 0, 0])],
        bounding_box: false,
        ..PSpec::default()
    }
    .build()
}

/// The synthetic archive's entries as (name, bytes), so a test can drop or damage one.
fn entries() -> Vec<(String, Vec<u8>)> {
    let mut e: Vec<(String, Vec<u8>)> = Vec::new();
    let mut add = |name: &str, bytes: Vec<u8>| e.push((name.to_string(), bytes));

    // enemy 16 ("aq"): three bones (the middle one has no mesh), two textures, three animation slots
    // of which the second is a special entry
    let aq = SkeletonSpec { textures: 2, animations: 3, ..SkeletonSpec::enemy() };
    add("aqaa", aq.build());
    add("aqab", vec![0; 8]);
    add("aqam", PSpec::default().build());
    add("aqao", one_triangle_mesh());
    add("aqac", TexSpec::default().build());
    add("aqad", TexSpec { palette: vec![[0, 0, 0, 255], [5, 5, 5, 255], [6, 6, 6, 255], [7, 7, 7, 255]], key_flag: false, ..TexSpec::default() }.build());
    let slot0 = vec![
        frame([0, -100, 0], &[[0, 0, 0], [-1024, 0, 0], [0, 0, 0], [0, 0, 0]]),
        frame([4, -90, 2], &[[0, 256, 0], [-1024, 0, 0], [512, 0, 0], [0, 0, 1024]]),
    ];
    let slot2 = vec![frame([0, -50, 0], &[[0, 0, 0]; 4])];
    add(
        "aqda",
        build_pack(&[
            (4, 2, encode_clip(&slot0, 0, 0)),
            (1, 1, vec![1, 2, 3, 4]),
            (4, 1, encode_clip(&slot2, 4, 2)),
        ]),
    );

    // enemy 1 ("ab"): a single bone; its clips hold one angle triple although the header says two
    add("abaa", SkeletonSpec { kind: 0, declared_bones: 1, pieces: 0, textures: 0, animations: 1, weapons: 0, weapon_animations: 0, bones: vec![(-1, -8.0, 1)] }.build());
    add("abam", one_triangle_mesh());
    add("abda", build_pack(&[(2, 2, encode_clip(&[frame([0, -5, 0], &[[0, 0, 0]]), frame([1, -5, 0], &[[1024, 0, 0]])], 0, 0))]));

    // character 461 ("rt"): two bones, two weapons, three skeleton slots and two weapon tracks
    let rt = SkeletonSpec {
        kind: 2,
        declared_bones: 2,
        pieces: 0,
        textures: 0,
        animations: 3,
        weapons: 2,
        weapon_animations: 2,
        bones: vec![(-1, -30.0, 1), (0, -12.0, 1)],
    };
    add("rtaa", rt.build());
    add("rtam", one_triangle_mesh());
    add("rtan", one_triangle_mesh());
    add("rtck", one_triangle_mesh());
    add("rtcl", PSpec::default().build());
    let body = vec![frame([0, -300, 0], &[[0, 0, 0], [-1024, 0, 0], [0, 0, 0]])];
    add(
        "rtda",
        build_pack(&[
            (3, 1, encode_clip(&body, 0, 0)),
            (3, 1, encode_clip(&body, 0, 0)),
            (3, 1, encode_clip(&body, 0, 0)),
            (1, 2, encode_clip(&[frame([10, -300, 5], &[[0, 0, 0]]), frame([12, -300, 5], &[[1024, 0, 0]])], 0, 0)),
            (1, 1, encode_clip(&[frame([-4, -280, 1], &[[0, 512, 0]])], 4, 1)),
        ]),
    );

    // locations 26 ("ba", code 0) and 27 ("bb", code 1): pieces, no animations
    let loc = |pieces: u32| SkeletonSpec { kind: 1, declared_bones: 0, pieces, textures: 2, animations: 0, weapons: 0, weapon_animations: 0, bones: vec![] };
    add("baaa", loc(2).build());
    add("baam", PSpec::default().build());
    add("baan", one_triangle_mesh());
    add("baac", TexSpec::default().build());
    add("baad", TexSpec::default().build());
    add("bbaa", loc(1).build());
    add("bbam", one_triangle_mesh());
    add("bbac", TexSpec::default().build());
    add("bbad", TexSpec::default().build());
    e
}

fn lgp_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    build_lgp(&refs)
}

fn models_from(entries: &[(String, Vec<u8>)]) -> BattleModels<Cursor<Vec<u8>>> {
    BattleModels::from_archive(LgpArchive::open(Cursor::new(lgp_of(entries))).unwrap()).unwrap()
}

fn models() -> BattleModels<Cursor<Vec<u8>>> {
    models_from(&entries())
}

fn without(skip: &str) -> Vec<(String, Vec<u8>)> {
    entries().into_iter().filter(|(n, _)| !n.eq_ignore_ascii_case(skip)).collect()
}

fn replaced(name: &str, bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    entries().into_iter().map(|(n, d)| if n == name { (n, bytes.clone()) } else { (n, d) }).collect()
}

#[test]
fn indexes_the_models_by_kind_and_maps_ids_to_prefixes() {
    let m = models();
    assert_eq!(m.kind_of("aq"), Some(BattleModelKind::Enemy));
    assert_eq!(m.kind_of("RT"), Some(BattleModelKind::Character));
    assert_eq!(m.kind_of("ba"), Some(BattleModelKind::Location));
    assert_eq!(m.kind_of("zz"), None);
    assert_eq!(m.kind_of("aqa"), None);
    assert_eq!(m.ids_of_kind(BattleModelKind::Enemy), [1, 16]);
    assert_eq!(m.ids_of_kind(BattleModelKind::Location), [26, 27]);
    assert_eq!(m.ids_of_kind(BattleModelKind::Character), [461]);
    // an enemy id is the model id in base 26 and must be an enemy
    assert_eq!(m.enemy_prefix(16).as_deref(), Some("aq"));
    assert_eq!(m.enemy_prefix(1).as_deref(), Some("ab"));
    assert_eq!(m.enemy_prefix(17), None, "no such model");
    assert_eq!(m.enemy_prefix(26), None, "a location is not an enemy");
    assert_eq!(m.enemy_prefix(461), None, "a character is not an enemy");
    assert_eq!(m.enemy_prefix(u16::MAX), None);
    // a location code counts from the first location model
    assert_eq!((m.location_count(), m.location_prefix(0).as_deref(), m.location_prefix(1).as_deref()), (2, Some("ba"), Some("bb")));
    assert_eq!(m.location_prefix(2), None);
    assert_eq!(m.location_prefix(u16::MAX), None);
    assert!(format!("{m:?}").contains("models"));
}

#[test]
fn an_enemy_loads_with_bones_parts_textures_and_slot_aligned_animations() {
    let mut m = models();
    let enemy = m.load_enemy(16).unwrap();
    assert_eq!((enemy.kind, enemy.prefix.as_str()), (BattleModelKind::Enemy, "aq"));
    assert_eq!((enemy.model.hrc_name.as_str(), enemy.model.skeleton_name.as_str()), ("aq", "battle enemy"));
    // bones: parents as stored, lengths made positive (the stored ones are negative)
    let bones = &enemy.model.bones;
    assert_eq!(bones.len(), 3);
    assert_eq!((bones[0].parent, bones[0].length), (None, 10.0));
    assert_eq!((bones[1].parent, bones[1].length), (Some(0), 20.0));
    assert_eq!((bones[2].parent, bones[2].length), (Some(1), 5.0));
    // parts: bone 0 has the default mesh (a textured group and a blended untextured one), bone 1 has
    // no mesh, bone 2 one triangle
    let parts = &enemy.model.parts;
    assert_eq!(parts.iter().map(|p| p.bone_index).collect::<Vec<_>>(), [0, 0, 2]);
    // textured group: texture number 1 -> the second texture file, opaque although the render state
    // of the group says "blend on" (the default mesh's second group is the blended one)
    let textured = &parts[0];
    assert_eq!(textured.texture_name.as_deref(), Some("aqad"));
    assert!(textured.texture.is_some());
    assert_eq!(textured.blend, PartBlend::Opaque);
    assert_eq!(textured.alpha, 1.0);
    // the untextured blended group keeps the field rule: alpha blending with 128/255
    assert_eq!((parts[1].blend, parts[1].texture_name.is_none()), (PartBlend::Alpha, true));
    assert!((parts[1].alpha - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(parts[2].blend, PartBlend::Opaque);
    // no normals in the file: zero normals (the one-triangle mesh has none); vertices deduplicated;
    // winding swapped like the field loader's (the file's first triangle (0, 1, 2) comes out as
    // (0, 2, 1))
    assert!(parts[2].normals.iter().all(|n| *n == [0.0; 3]) && parts[2].normals.len() == 3);
    assert_eq!(textured.positions.len(), 4);
    let tri = |i: usize| -> [[f32; 3]; 3] { [0, 1, 2].map(|k| textured.positions[textured.indices[3 * i + k] as usize]) };
    assert_eq!(tri(0), [[0.0, 0.0, 0.0], [10.0, 10.0, 0.0], [10.0, 0.0, 0.0]]);
    assert!(enemy.model.missing_textures.is_empty());
    assert_eq!(enemy.model.dropped_triangles, 0);
    // animations: one per slot, in slot order; the special entry (slot 1) is a stand-in
    assert_eq!(enemy.animations.len(), 3);
    assert_eq!(enemy.slot_ok, [true, false, true]);
    assert_eq!((enemy.animations[0].frame_count(), enemy.animations[0].bone_count()), (2, 3));
    assert_eq!(enemy.animations[2].frame_count(), 1);
    assert_eq!(enemy.animations[1].frame_count(), 1);
    assert_eq!(enemy.animations[1].frame(0), enemy.animations[0].frame(0), "the stand-in is frame 0 of the first good slot");
    assert!(enemy.weapons.is_empty() && enemy.weapon_tracks.is_empty());
    // every slot poses the model, and the root translation is the stored one (y is down)
    for slot in 0..3 {
        let pose = enemy.pose(slot, 0).unwrap();
        assert_eq!(pose.len(), 3);
    }
    let pose = enemy.pose(0, 1).unwrap();
    assert_eq!(transform_point(&pose[0], [0.0; 3]), [4.0, -90.0, 2.0]);
    assert!(enemy.pose(3, 0).is_none(), "no such slot");
    // the root rotation is applied: bone 0 of frame 1 carries the root's 22.5 degrees about y (256/4096 of a turn is 22.5)
    let f1 = enemy.animations[0].frame(1).unwrap();
    assert_eq!(f1.root_rotation, [0.0, 22.5, 0.0]);
    assert_eq!(f1.root_translation, [4.0, 90.0, 2.0]);
}

#[test]
fn a_single_bone_enemy_takes_one_angle_triple_whatever_the_clip_header_says() {
    let mut m = models();
    let enemy = m.load_enemy(1).unwrap();
    assert_eq!(enemy.model.bones.len(), 1);
    assert_eq!(enemy.slot_ok, [true]);
    let a = &enemy.animations[0];
    assert_eq!((a.frame_count(), a.bone_count()), (2, 1));
    // the one triple is the root rotation, and the bone follows it
    assert_eq!(a.frame(1).unwrap().root_rotation, [90.0, 0.0, 0.0]);
    assert_eq!(a.frame(1).unwrap().rotations, vec![[0.0; 3]]);
    assert!(enemy.pose(0, 1).is_some());
}

#[test]
fn a_character_has_weapons_as_one_bone_models_and_their_tracks() {
    let mut m = models();
    let cloud = m.load_character(BattleCharacter::Cloud).unwrap();
    assert_eq!((cloud.kind, cloud.prefix.as_str()), (BattleModelKind::Character, "rt"));
    assert_eq!(cloud.animations.len(), 3);
    assert_eq!(cloud.slot_ok, [true, true, true]);
    assert_eq!(cloud.weapons.len(), 2);
    for weapon in &cloud.weapons {
        assert_eq!(weapon.bones.len(), 1);
        assert_eq!(weapon.bones[0].parent, None);
        assert!(weapon.parts.iter().all(|p| p.bone_index == 0));
    }
    assert_eq!(cloud.weapons[0].hrc_name, "rtck");
    assert_eq!(cloud.weapons[1].hrc_name, "rtcl");
    assert_eq!(cloud.weapons[1].parts.len(), 2, "the second weapon is the default two-group mesh");
    // two tracks for three skeleton slots: the third has none
    assert_eq!(cloud.weapon_tracks.len(), 2);
    assert_eq!(cloud.weapon_track_ok, [true, true]);
    assert_eq!((cloud.weapon_tracks[0].frame_count(), cloud.weapon_tracks[0].bone_count()), (2, 1));
    // the track places the weapon: frame 0 at the stored translation, frame 1 turned 180 degrees about x
    let at = |slot: usize, frame: usize| cloud.weapon_pose(0, slot, frame).unwrap();
    assert_eq!(transform_point(&at(0, 0), [0.0; 3]), [10.0, -300.0, 5.0]);
    assert_eq!(transform_point(&at(0, 1), [0.0; 3]), [12.0, -300.0, 5.0]);
    let turned = transform_point(&at(0, 1), [0.0, 1.0, 0.0]);
    assert!((turned[1] - -300.0).abs() < 1e-3 && (turned[2] - 5.0).abs() < 1e-3 + 1.0, "{turned:?}");
    assert_eq!(transform_point(&at(1, 0), [0.0; 3]), [-4.0, -280.0, 1.0]);
    assert!(cloud.weapon_pose(0, 2, 0).is_none(), "no track for slot 2");
    assert!(cloud.weapon_pose(2, 0, 0).is_none(), "no third weapon");
    // the body's clips are not the weapon tracks: three slots of three triples
    assert_eq!(cloud.animations[0].bone_count(), 2);
    // the other characters of the table are not in this archive
    assert!(matches!(m.load_character(BattleCharacter::Tifa), Err(BattleModelError::NoSuchModel { .. })));
}

#[test]
fn a_location_is_one_identity_bone_with_absolute_pieces_and_an_identity_animation() {
    let mut m = models();
    let arena = m.load_location(0).unwrap();
    assert_eq!((arena.kind, arena.prefix.as_str()), (BattleModelKind::Location, "ba"));
    assert_eq!(arena.model.bones.len(), 1);
    assert_eq!(arena.model.bones[0].parent, None);
    // piece 0 is the two-group default mesh, piece 1 one triangle: all on bone 0
    assert_eq!(arena.model.parts.len(), 3);
    assert!(arena.model.parts.iter().all(|p| p.bone_index == 0));
    assert_eq!(arena.animations.len(), 1);
    assert_eq!(arena.slot_ok, [true]);
    let pose = arena.pose(0, 0).unwrap();
    assert_eq!(pose.len(), 1);
    assert_eq!(pose[0], mat4_identity());
    assert!(arena.weapons.is_empty());
    // the second location has one piece
    assert_eq!(m.load_location(1).unwrap().model.parts.len(), 1);
    assert!(matches!(m.load_location(2), Err(BattleModelError::NoSuchModel { .. })));
}

/// A 10 x 10 square (two triangles, one textured group using texture 0) whose texture repeats
/// `u` x `v` times.
fn repeating_square(u: f32, v: f32) -> Vec<u8> {
    PSpec {
        texcoords: vec![[0.0, 0.0], [u, 0.0], [u, v], [0.0, v]],
        polygons: vec![([0, 1, 2], [0, 0, 0]), ([0, 2, 3], [0, 0, 0])],
        groups: vec![(0, 2, 0, 4, 0, true, 0)],
        bounding_box: false,
        ..PSpec::default()
    }
    .build()
}

#[test]
fn a_texture_that_repeats_is_baked_into_the_geometry_so_every_coordinate_is_inside_it() {
    // location code 1 ("bb") with one piece whose texture repeats 3 x 2 times
    let mut m = models_from(&replaced("bbam", repeating_square(3.0, 2.0)));
    let arena = m.load_location(1).unwrap();
    assert_eq!(arena.model.parts.len(), 1);
    let part = &arena.model.parts[0];
    assert!(part.texture.is_some());
    assert_eq!(part.blend, PartBlend::Opaque);
    assert!(part.triangle_count() > 2 && part.triangle_count() <= 24, "{} triangles", part.triangle_count());
    assert!(part.uvs.iter().flatten().all(|c| (0.0..=1.0).contains(c)), "coordinates are inside the texture");
    // the pieces tile the square: their areas add up to its 100, and none is flipped: the file's
    // triangles are counter-clockwise in the plane of this square, and like every battle mesh the
    // loader swaps two corners (clockwise files become counter-clockwise), so all come out clockwise
    let mut total = 0.0f32;
    for tri in part.indices.chunks_exact(3) {
        let p = |k: usize| part.positions[tri[k] as usize];
        let area = ((p(1)[0] - p(0)[0]) * (p(2)[1] - p(0)[1]) - (p(2)[0] - p(0)[0]) * (p(1)[1] - p(0)[1])) / 2.0;
        assert!(area < 0.0, "the swapped winding is kept by every piece");
        total -= area;
    }
    assert!((total - 100.0).abs() < 0.01, "{total}");
    // every vertex samples where the original coordinates did, up to whole repeats: u = 3x/10, v = 2y/10
    for (p, uv) in part.positions.iter().zip(&part.uvs) {
        let (du, dv) = (0.3 * p[0] - uv[0], 0.2 * p[1] - uv[1]);
        assert!((du - du.round()).abs() < 1e-3 && (dv - dv.round()).abs() < 1e-3, "{p:?} {uv:?}");
    }
    // a texture that does not repeat is not touched: same vertices as the file's four
    let mut m = models_from(&replaced("bbam", repeating_square(1.0, 1.0)));
    assert_eq!(m.load_location(1).unwrap().model.parts[0].positions.len(), 4);
    // a texture "repeating" a billion times is not cut (bounded work) and is clamped instead
    let mut m = models_from(&replaced("bbam", repeating_square(1e9, 1e9)));
    let part = &m.load_location(1).unwrap().model.parts[0];
    assert_eq!(part.triangle_count(), 2);
    assert!(part.uvs.iter().flatten().all(|c| (0.0..=1.0).contains(c)));
}

#[test]
fn missing_and_misdirected_requests_are_errors_not_panics() {
    let mut m = models();
    assert!(matches!(m.load_enemy(17), Err(BattleModelError::NoSuchModel { .. })));
    assert!(matches!(m.load_enemy(26), Err(BattleModelError::NoSuchModel { .. })), "a location is not an enemy");
    assert!(matches!(
        m.load_prefix("rt", Some(BattleModelKind::Enemy)),
        Err(BattleModelError::WrongKind { found: "character", wanted: "enemy", .. })
    ));
    assert!(matches!(m.load_prefix("nope", None), Err(BattleModelError::NoSuchModel { .. })));
    assert!(m.load_prefix(" AQ ", Some(BattleModelKind::Enemy)).is_ok(), "prefixes ignore case and spaces");
    // a missing mesh, weapon or animation pack is an error naming the entry
    let load = |skip: &str, id: u16| models_from(&without(skip)).load_enemy(id);
    assert!(matches!(load("aqao", 16), Err(BattleModelError::Missing { kind: "mesh", name }) if name == "aqao"));
    assert!(matches!(load("aqda", 16), Err(BattleModelError::Missing { kind: "animation pack", .. })));
    assert!(matches!(
        models_from(&without("rtcl")).load_character(BattleCharacter::Cloud),
        Err(BattleModelError::Missing { kind: "weapon", .. })
    ));
    // a missing texture leaves the part untextured and is listed
    let e = models_from(&without("aqad")).load_enemy(16).unwrap();
    assert!(e.model.parts[0].texture.is_none());
    assert_eq!(e.model.parts[0].texture_name.as_deref(), Some("aqad"));
    assert_eq!(e.model.missing_textures, ["aqad"]);
    assert_eq!(e.model.parts[0].blend, PartBlend::Opaque);
}

#[test]
fn a_texture_number_past_the_declared_textures_is_missing_not_read() {
    // the skeleton declares one texture but the mesh uses texture number 1
    let aq = SkeletonSpec { textures: 1, animations: 3, ..SkeletonSpec::enemy() };
    let e = models_from(&replaced("aqaa", aq.build())).load_enemy(16).unwrap();
    assert!(e.model.parts[0].texture.is_none());
    assert_eq!(e.model.missing_textures, ["aqad"]);
}

#[test]
fn a_clip_that_does_not_decode_costs_its_slot_not_the_model() {
    // slot 2's key byte is damaged
    let pack = {
        let mut e = entries();
        let (_, bytes) = e.iter_mut().find(|(n, _)| n == "aqda").unwrap();
        // entry 2 starts after: count (4), entry 0 (12 + chunk), entry 1 (12 + 4), entry header (12)
        let first = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let chunk2 = 4 + (12 + first) + (12 + 4) + 12;
        bytes[chunk2 + 4] = 9; // key 9
        e
    };
    let e = models_from(&pack).load_enemy(16).unwrap();
    assert_eq!(e.slot_ok, [true, false, false]);
    assert_eq!(e.animations.len(), 3);
    assert!(e.pose(2, 0).is_some());
    // a pack with fewer entries than the skeleton's slots: the missing slots are stand-ins
    let short = build_pack(&[(4, 1, encode_clip(&[frame([0, -1, 0], &[[0, 0, 0]; 4])], 0, 0))]);
    let e = models_from(&replaced("aqda", short)).load_enemy(16).unwrap();
    assert_eq!((e.animations.len(), e.slot_ok.clone()), (3, vec![true, false, false]));
    // a pack with no good clip at all: identity stand-ins, still posable
    let none = build_pack(&[(1, 1, vec![0; 4]), (1, 1, vec![0; 4]), (1, 1, vec![0; 4])]);
    let e = models_from(&replaced("aqda", none)).load_enemy(16).unwrap();
    assert_eq!(e.slot_ok, [false, false, false]);
    assert!(e.pose(0, 0).is_some());
    // a damaged pack (an entry that runs past the end) is an error for the model
    let mut broken = entries().into_iter().find(|(n, _)| n == "aqda").unwrap().1;
    broken.truncate(broken.len() - 3);
    assert!(matches!(models_from(&replaced("aqda", broken)).load_enemy(16), Err(BattleModelError::Format(_))));
}

#[test]
fn a_model_whose_files_are_garbage_is_an_error_and_never_a_panic() {
    for (name, junk) in [
        ("aqam", vec![0xAB; 300]),
        ("aqao", vec![]),
        ("aqac", vec![1; 10]),
        ("aqda", vec![0xFF; 40]),
        ("rtck", vec![7; 5]),
    ] {
        let mut m = models_from(&replaced(name, junk));
        // each load either works (a texture is optional) or is an error
        let _ = m.load_enemy(16);
        let _ = m.load_character(BattleCharacter::Cloud);
    }
    // a garbage skeleton is not a model at all: the other models still index
    let mut m = models_from(&replaced("aqaa", vec![1, 2, 3]));
    assert_eq!(m.kind_of("aq"), None);
    assert!(m.load_enemy(16).is_err());
    assert!(m.load_enemy(1).is_ok());
    // an archive with no skeletons is not a battle archive
    let lgp = build_lgp(&[("hello", b"x")]);
    assert!(matches!(BattleModels::from_archive(LgpArchive::open(Cursor::new(lgp)).unwrap()), Err(BattleModelError::NoModels)));
    // nor are names that merely end in "aa"
    let lgp = build_lgp(&[("xaa", b"x"), ("abcaa", b"y"), ("1aaa", b"z")]);
    assert!(matches!(BattleModels::from_archive(LgpArchive::open(Cursor::new(lgp)).unwrap()), Err(BattleModelError::NoModels)));
}

#[test]
fn byte_damage_anywhere_in_a_model_never_panics() {
    // flip bytes of every entry in turn (every position of the small ones, a sample of the large),
    // and load every model of the archive: errors and odd pictures are fine, panics are not
    let all = entries();
    for (which, (name, bytes)) in all.iter().enumerate() {
        let stride = if bytes.len() > 400 { 5 } else { 1 };
        for at in (0..bytes.len()).step_by(stride) {
            for flip in [0x80u8, 0xFF] {
                let mut damaged = all.clone();
                damaged[which].1[at] ^= flip;
                let lgp = LgpArchive::open(Cursor::new(lgp_of(&damaged))).unwrap();
                let Ok(mut m) = BattleModels::from_archive(lgp) else { continue };
                let _ = m.load_enemy(16);
                let _ = m.load_enemy(1);
                let _ = m.load_character(BattleCharacter::Cloud);
                let _ = m.load_location(0);
                let _ = m.load_location(1);
                let _ = name;
            }
        }
    }
}

#[test]
fn the_cumulative_read_budget_is_enforced() {
    let mut lgp = LgpArchive::open(Cursor::new(lgp_of(&entries()))).unwrap();
    let mut budget = 10u64;
    assert!(matches!(read_entry(&mut lgp, "mesh", "aqam", &mut budget), Err(BattleModelError::Budget { .. })));
    let mut budget = 1_000_000u64;
    let before = budget;
    let bytes = read_entry(&mut lgp, "mesh", "aqam", &mut budget).unwrap();
    assert_eq!(before - budget, bytes.len() as u64);
    assert!(matches!(read_entry(&mut lgp, "mesh", "nope", &mut budget), Err(BattleModelError::Missing { .. })));
}

#[test]
fn characters_map_to_distinct_prefixes_and_appearance_ids() {
    let prefixes: std::collections::HashSet<&str> = BattleCharacter::ALL.iter().map(|c| c.prefix()).collect();
    assert_eq!(prefixes.len(), BattleCharacter::ALL.len());
    for c in BattleCharacter::ALL {
        assert!(!c.display_name().is_empty());
        let id = skeleton::id_of_prefix(c.prefix()).unwrap();
        assert!((460..=480).contains(&id), "{}: {id}", c.prefix());
    }
    assert_eq!(BattleCharacter::Cloud.prefix(), "rt");
    assert_eq!(BattleCharacter::from_appearance_ref("ff7/cloud"), Some(BattleCharacter::Cloud));
    assert_eq!(BattleCharacter::from_appearance_ref("  FF7/Red_XIII "), Some(BattleCharacter::RedXiii));
    for (name, want) in [("ff7/barret", BattleCharacter::Barret), ("ff7/tifa", BattleCharacter::Tifa), ("ff7/aerith", BattleCharacter::Aerith)] {
        assert_eq!(BattleCharacter::from_appearance_ref(name), Some(want));
    }
    // every appearance of the field table that has a battle model is found
    for model in crate::playable::playable_field_models() {
        assert!(BattleCharacter::from_appearance_ref(model.appearance_ref).is_some(), "{}", model.appearance_ref);
    }
    for bad in ["", "ff7", "ff7/", "cloud", "ff8/squall", "ff7/nobody", "ff7/cloud/2"] {
        assert_eq!(BattleCharacter::from_appearance_ref(bad), None, "{bad:?}");
    }
}

#[test]
fn loaded_models_can_cross_threads_and_share_textures_between_clones() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BattleModel>();
    assert_send_sync::<BattleModelError>();
    let mut m = models();
    let enemy = m.load_enemy(16).unwrap();
    let copy = enemy.clone();
    assert!(Arc::ptr_eq(enemy.model.parts[0].texture.as_ref().unwrap(), copy.model.parts[0].texture.as_ref().unwrap()));
}

#[test]
fn the_blend_rule_is_opaque_for_textured_groups_and_the_field_rule_for_the_rest() {
    let blend = |blend, src, dst, alpha| PRenderState { blend, src_blend: src, dst_blend: dst, alpha };
    // every textured battle group says "blend on, source alpha, 128": still opaque
    assert_eq!(classify_blend(&blend(true, 5, 6, 128), true), (PartBlend::Opaque, 1.0));
    assert_eq!(classify_blend(&blend(true, 5, 2, 64), true), (PartBlend::Opaque, 1.0));
    // untextured groups: opaque unless the blend bit is set
    assert_eq!(classify_blend(&PRenderState::OPAQUE, false), (PartBlend::Opaque, 1.0));
    assert_eq!(classify_blend(&blend(false, 5, 6, 128), false), (PartBlend::Opaque, 1.0));
    let (kind, alpha) = classify_blend(&blend(true, 5, 6, 128), false);
    assert_eq!(kind, PartBlend::Alpha);
    assert!((alpha - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(classify_blend(&blend(true, 5, 2, 255), false), (PartBlend::Additive, 1.0));
}
