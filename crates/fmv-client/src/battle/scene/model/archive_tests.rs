//! [`ArchiveModels`] over a synthetic `battle.lgp` (invented data: one enemy with a weapon, one
//! character with a weapon, one location, one triangle each), written field by field here so the
//! test needs no game data and does not depend on `fmv-ff7`'s own test builders.

use super::*;
use fmv_ff7_battle::rules::ActorId;
use std::io::Write;

/// An LGP archive: header, table of contents, the 3 600-byte lookup table, an empty path table, the
/// entries (24-byte header and bytes), the footer.
fn lgp(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let toc_end = 16 + entries.len() * 27;
    let mut offset = toc_end + 3600 + 2;
    let mut out = b"\0\0SQUARESOFT".to_vec();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offsets = Vec::new();
    for (name, data) in entries {
        let mut padded = [0u8; 20];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&padded);
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        out.push(0x0E);
        out.extend_from_slice(&0u16.to_le_bytes());
        offsets.push(offset);
        offset += 24 + data.len();
    }
    out.extend(std::iter::repeat_n(0u8, 3600));
    out.extend_from_slice(&0u16.to_le_bytes());
    for (name, data) in entries {
        let mut padded = [0u8; 20];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&padded);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out.extend_from_slice(b"FINAL FANTASY7");
    out
}

/// A skeleton file: kind, bones (`(parent, length, has_model)`), pieces, textures, animations,
/// weapons, weapon animations.
fn skeleton(kind: u32, bones: &[(i32, f32, u32)], pieces: u32, animations: u32, weapons: u32, weapon_animations: u32) -> Vec<u8> {
    let words: [u32; 13] = [
        kind,
        1,
        u32::from(kind != 1),
        bones.len() as u32,
        0,
        pieces,
        0,
        animations,
        animations + 2,
        weapons,
        weapon_animations,
        0,
        0xABCD,
    ];
    let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    for &(parent, length, has_model) in bones {
        out.extend_from_slice(&parent.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&has_model.to_le_bytes());
    }
    out
}

/// A `.p` mesh of one untextured triangle with vertex colours and no normals.
fn triangle() -> Vec<u8> {
    let words: [u32; 16] = [1, 1, 1, 3, 0, 0, 0, 3, 0, 1, 0, 0, 1, 1, 0, 1];
    let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    out.extend(std::iter::repeat_n(0u8, 64)); // run-time data
    for v in [[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, -10.0, 0.0]] {
        v.iter().for_each(|c| out.extend_from_slice(&c.to_le_bytes()));
    }
    for c in [[10u8, 20, 30, 255], [40, 50, 60, 255], [70, 80, 90, 255]] {
        out.extend_from_slice(&c);
    }
    out.extend_from_slice(&[0u8; 4]); // polygon colour
    out.extend_from_slice(&0u16.to_le_bytes());
    for v in [0u16, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut hundred = [0u32; 25];
    hundred[2] = 0x3_8200;
    hundred[13] = 2;
    hundred[14] = 1;
    hundred[23] = 255;
    hundred.iter().for_each(|w| out.extend_from_slice(&w.to_le_bytes()));
    let group: [u32; 14] = [1, 0, 1, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    group.iter().for_each(|w| out.extend_from_slice(&w.to_le_bytes()));
    out
}

/// A one-frame all-zero clip of one angle triple (a single-bone skeleton's, or a weapon track's), as
/// a pack entry.
fn still_entry() -> (u32, u32, Vec<u8>) {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&1u16.to_le_bytes()); // frames
    chunk.extend_from_slice(&11u16.to_le_bytes()); // data size: 48 + 36 bits
    chunk.push(0); // key
    chunk.extend(std::iter::repeat_n(0u8, 11));
    (1, 1, chunk)
}

fn pack(entries: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = (entries.len() as u32).to_le_bytes().to_vec();
    for (bones, frames, chunk) in entries {
        out.extend_from_slice(&bones.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// The archive on disk: enemy 16 (`aq`) with a weapon, character Cloud (`rt`) with a weapon, and
/// battle location code 0 (`ba`), in a temporary directory that lives as long as the returned guard.
fn archive() -> (tempfile::TempDir, PathBuf) {
    let one_bone = [(-1, -10.0, 1)];
    let armed_pack = pack(&[still_entry(), still_entry()]);
    let bytes = lgp(&[
        ("aqaa", skeleton(0, &one_bone, 0, 1, 1, 1)),
        ("aqam", triangle()),
        ("aqck", triangle()),
        ("aqda", armed_pack.clone()),
        ("rtaa", skeleton(2, &one_bone, 0, 1, 1, 1)),
        ("rtam", triangle()),
        ("rtck", triangle()),
        ("rtda", armed_pack),
        ("baaa", skeleton(1, &[], 1, 0, 0, 0)),
        ("baam", triangle()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("battle.lgp");
    std::fs::File::create(&path).unwrap().write_all(&bytes).unwrap();
    (dir, path)
}

fn enemy(id: Option<u16>) -> EnemyLook {
    EnemyLook { actor: ActorId(3), enemy_id: id, name: "MP".into(), max_hp: 30, level: 2 }
}

/// A canon companion: the only kind of party member the archive gives a battle model.
fn party(character: u8) -> PartyLook {
    PartyLook::companion(ActorId(0), character, true)
}

/// Battle placeholder: the slot's own battle model. The model of a party member is the character in whose
/// combat slot it fights (FF7's ids), Cait Sith included (she has a battle model although no field one), and
/// Cloud for an id that is nobody's.
#[test]
fn a_party_member_is_the_character_of_its_slot_then_cloud() {
    for (id, want) in [
        (0u8, BattleCharacter::Cloud),
        (1, BattleCharacter::Barret),
        (2, BattleCharacter::Tifa),
        (3, BattleCharacter::Aerith),
        (4, BattleCharacter::RedXiii),
        (5, BattleCharacter::Yuffie),
        (6, BattleCharacter::CaitSith),
        (7, BattleCharacter::Vincent),
        (8, BattleCharacter::Cid),
    ] {
        assert_eq!(battle_character(id), want, "slot {id}");
    }
    // An id the engine does not have: Cloud.
    assert_eq!(battle_character(9), BattleCharacter::Cloud);
    assert_eq!(battle_character(255), BattleCharacter::Cloud);
    // It agrees with the field stand-in's choice wherever a field model exists.
    for id in [0u8, 1, 2, 3, 4, 5, 7, 8] {
        let field = party_appearance(id);
        assert_eq!(BattleCharacter::from_appearance_ref(field), Some(battle_character(id)), "{field}");
    }
}

#[test]
fn the_archive_source_gives_real_battle_models_with_their_weapons_and_the_location() {
    let (_dir, path) = archive();
    let mut source = ArchiveModels::open(&path).unwrap();

    // An enemy by its id: the model, in battle units, with its weapon riding along (clip i with track i).
    let BattleActorModel::Battle(mp) = source.enemy(&enemy(Some(16))) else { panic!("the MP is a battle model") };
    assert_eq!((mp.key.as_str(), mp.scale, mp.clips.len()), ("aq", 1.0, 1));
    assert_eq!(mp.attachments.len(), 1);
    assert_eq!((mp.attachments[0].key.as_str(), mp.attachments[0].clips.len(), mp.attachments[0].model.bones.len()), ("aqck", 1, 1));
    assert_eq!(mp.model.bones.len(), 1);
    // A character by the slot it fights in (the archive has only Cloud's model: a slot that is not his is not
    // drawn as him, whatever its player looks like in the field).
    let BattleActorModel::Battle(cloud) = source.party_member(&party(0)) else { panic!("Cloud is a battle model") };
    assert_eq!((cloud.key.as_str(), cloud.attachments.len()), ("rt", 1));
    assert!(matches!(source.party_member(&party(5)), BattleActorModel::Field(Some(ref r)) if r == "ff7/yuffie"), "slot 5 is Yuffie's, not Cloud's");
    // The location of a battle setup.
    let Some(BattleActorModel::Battle(arena)) = source.location(0) else { panic!("location 0 is a battle model") };
    assert_eq!((arena.key.as_str(), arena.model.bones.len(), arena.clips.len(), arena.attachments.len()), ("ba", 1, 1, 0));

    // What the scene makes of them: real rigs at battle scale, the weapon a drawable attachment.
    let mut rigs = Rigs::new(None);
    let fallback = BoxSpec::for_enemy(0, 30);
    let rig = rigs.resolve(&BattleActorModel::Battle(mp.clone()), fallback);
    assert_eq!(rig.scale, 1.0);
    assert!(rig.procedural_idle, "a one-frame idle clip is a still pose");
    assert_eq!(rig.attachments.len(), 1);
    assert!(rig.height > 5.0 && rig.height < 11.0, "the triangle is 10 units tall: {}", rig.height);
    assert!(rigs.resolve_optional(&BattleActorModel::Battle(arena)).is_some());
}

#[test]
fn a_model_is_read_once_however_many_actors_ask_and_shared_between_them() {
    let (_dir, path) = archive();
    let mut source = ArchiveModels::open(&path).unwrap();
    let (BattleActorModel::Battle(a), BattleActorModel::Battle(b)) = (source.enemy(&enemy(Some(16))), source.enemy(&enemy(Some(16)))) else {
        panic!("two MPs")
    };
    assert!(Arc::ptr_eq(&a.model, &b.model) && Arc::ptr_eq(&a.clips, &b.clips), "one model, shared");
    assert!(Arc::ptr_eq(&a.attachments[0].model, &b.attachments[0].model));
    // so the scene makes one rig and uploads once
    let mut rigs = Rigs::new(None);
    let (ra, rb) = (rigs.resolve(&BattleActorModel::Battle(a), BoxSpec::for_party(0)), rigs.resolve(&BattleActorModel::Battle(b), BoxSpec::for_party(0)));
    assert!(Arc::ptr_eq(&ra, &rb));
}

#[test]
fn an_actor_whose_model_is_not_in_the_archive_gets_the_stand_in() {
    let (_dir, path) = archive();
    let mut source = ArchiveModels::open(&path).unwrap();
    // An enemy the formation data does not name, one the archive has no model for (17 is not in
    // this archive; 461 is a character), and one with a model but a broken clip are all boxes.
    for id in [None, Some(17), Some(461), Some(u16::MAX)] {
        assert!(matches!(source.enemy(&enemy(id)), BattleActorModel::Box(_)), "{id:?}");
    }
    // A character the archive lacks (it has only Cloud) is the field model, as before.
    assert!(matches!(source.party_member(&party(2)), BattleActorModel::Field(Some(ref r)) if r == "ff7/tifa"));
    assert!(matches!(source.party_member(&party(6)), BattleActorModel::Field(Some(ref r)) if r == "ff7/cloud"), "Cait Sith has no field model either");
    // No location: the scene's own floor (none for the 'no location' code, nor for one that is not there).
    assert!(source.location(0xFFFF).is_none());
    assert!(source.location(1).is_none());
    assert!(source.location(0).is_some());
    // The answer for a missing model is the stand-in source's own.
    assert!(matches!(StandInModels.enemy(&enemy(Some(17))), BattleActorModel::Box(_)));
}

#[test]
fn a_damaged_model_is_a_stand_in_not_a_failure_and_is_not_asked_for_again() {
    // The MP's mesh is damaged: its load fails, once, and the actor is a box.
    let (_dir, path) = archive();
    let bytes = std::fs::read(&path).unwrap();
    let needle = triangle();
    let first = bytes.windows(needle.len()).position(|w| w == needle.as_slice()).expect("the mesh is in the file");
    let mut damaged = bytes.clone();
    damaged[first..first + 8].copy_from_slice(&[0xFF; 8]); // the header's version and off04 words
    let broken = path.with_file_name("broken.lgp");
    std::fs::write(&broken, &damaged).unwrap();
    let mut source = ArchiveModels::open(&broken).unwrap();
    assert!(matches!(source.enemy(&enemy(Some(16))), BattleActorModel::Box(_)));
    assert!(matches!(source.enemy(&enemy(Some(16))), BattleActorModel::Box(_)));
    assert_eq!(source.made.len(), 1, "the failure is remembered under its key");
}

#[test]
fn the_archive_must_be_a_battle_archive_and_errors_name_no_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("secret-place").join("battle.lgp");
    let err = ArchiveModels::open(&missing).err().expect("no such file");
    assert!(!err.contains("secret-place"), "{err}");
    let junk = dir.path().join("junk.lgp");
    std::fs::write(&junk, b"this is not an archive").unwrap();
    assert!(ArchiveModels::open(&junk).is_err());
    // an LGP that holds no battle models is not a battle archive
    let other = dir.path().join("other.lgp");
    std::fs::write(&other, lgp(&[("hello", b"x".to_vec())])).unwrap();
    assert!(ArchiveModels::open(&other).err().expect("not a battle archive").contains("not a battle.lgp"));
}

#[test]
fn a_remembered_location_is_replaced_by_the_next_call() {
    // (the process-wide slot is only read by `ModelKit::from_local_content`; this test leaves it empty)
    remember_battle_lgp(Some(PathBuf::from("/somewhere/battle.lgp")));
    assert_eq!(remembered_battle_lgp(), Some(PathBuf::from("/somewhere/battle.lgp")));
    remember_battle_lgp(None);
    assert_eq!(remembered_battle_lgp(), None);
}

#[test]
fn a_battle_models_radius_is_its_body_not_its_tail() {
    use fmv_ff7::model::ModelBone;
    // A model whose idle pose has a long thin tail: 30 vertices of the body within 60 of the axis
    // and one tail vertex 900 away. The widest extent (z from -30 to 900) says 465; the body says
    // about 60, which is less than the floor of half the widest extent that the radius keeps.
    let part = |positions: Vec<[f32; 3]>| {
        let n = positions.len();
        let triangles = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
        MeshPart {
            bone_index: 0,
            positions,
            normals: vec![[0.0; 3]; n],
            uvs: vec![[0.0; 2]; n],
            colors: vec![[200, 100, 50, 255]; n],
            indices: triangles,
            texture: None,
            texture_name: None,
            blend: PartBlend::Opaque,
            alpha: 1.0,
        }
    };
    let mut body: Vec<[f32; 3]> = (0..30).map(|i| [100.0 * (i as f32 / 29.0 - 0.5), -50.0 - i as f32, 60.0 * (i as f32 / 29.0 - 0.5)]).collect();
    body.push([0.0, -20.0, 900.0]);
    let model = FieldModel {
        hrc_name: "dog".into(),
        skeleton_name: "dog".into(),
        bones: vec![ModelBone { name: "root".into(), parent: None, length: 100.0 }],
        parts: vec![part(body)],
        dropped_triangles: 0,
        missing_textures: Vec::new(),
    };
    let clip = still_clip(1);
    let mut rigs = Rigs::new(None);
    let rig = rigs.resolve(&BattleActorModel::battle("dog", model.clone(), vec![clip.clone()]), BoxSpec::for_party(0));
    let widest = radius_of(&rig.asset, 1.0);
    assert!((widest - 465.0).abs() < 1.0, "the tail makes it 930 long: {widest}");
    assert!(rig.radius < 0.6 * widest && rig.radius >= 0.5 * widest, "the body (floored at half): {} against {widest}", rig.radius);
    // and a scale scales it
    let mut big = BattleModelData::new("dog2", model, vec![clip]);
    big.scale = 2.0;
    let rig2 = rigs.resolve(&BattleActorModel::Battle(big), BoxSpec::for_party(0));
    assert!((rig2.radius - 2.0 * rig.radius).abs() < 1.0, "{} against {}", rig2.radius, rig.radius);
}
