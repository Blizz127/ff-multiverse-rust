//! Checks against the owner's REAL `battle/battle.lgp` (FF7 Steam Edition, English; verified
//! 2026-11-08).
//!
//! Every test is `#[ignore]` and also returns early when no archive is configured, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --lib battle_model::real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one with `battle/battle.lgp`);
//! `FMV_FF7_BATTLE_LGP` names the archive directly. The files are only ever opened read-only, and
//! the assertions record counts and sizes: no game bytes are stored in this repository or printed.
//! (The checks that need `scene.bin` live in `fmv-client`'s `avatar/shots/battle_models.rs`, which
//! has the scene parser.)

use super::skeleton::{animation_pack_name, part_name, skeleton_name, texture_name, weapon_name};
use super::*;
use crate::model::mat4_identity;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn battle_lgp() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FMV_FF7_BATTLE_LGP") {
        return Some(PathBuf::from(path));
    }
    match std::env::var_os("FMV_FF7_DATA") {
        Some(dir) => Some(PathBuf::from(dir).join("battle").join("battle.lgp")),
        None => {
            eprintln!("neither FMV_FF7_BATTLE_LGP nor FMV_FF7_DATA is set; skipping");
            None
        }
    }
}

fn all_prefixes(lgp: &LgpArchive<std::fs::File>) -> BTreeSet<String> {
    lgp.list().iter().map(|e| e.name[..2].to_ascii_lowercase()).collect()
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA or FMV_FF7_BATTLE_LGP)"]
fn archive_structure_and_naming_rules() {
    let Some(path) = battle_lgp() else { return };
    let mut lgp = LgpArchive::open_path(&path).unwrap();
    assert_eq!(lgp.len(), 11_119);
    assert!(lgp.list().iter().all(|e| e.name.len() == 4 && e.path_index == 0));
    let prefixes = all_prefixes(&lgp);
    assert_eq!(prefixes.len(), 481);

    let mut expected: BTreeSet<String> = BTreeSet::new();
    let mut kinds: BTreeMap<BattleModelKind, usize> = BTreeMap::new();
    let (mut bones, mut parts, mut max_bones, mut max_pieces, mut max_slots, mut max_tracks) = (0usize, 0usize, 0, 0, 0, 0);
    let mut non_negative_lengths = 0usize;
    for prefix in &prefixes {
        let bytes = lgp.read(&skeleton_name(prefix)).unwrap();
        let sk = BattleSkeleton::parse(&bytes).unwrap();
        // 52 bytes of header plus 12 per bone, nothing else
        assert_eq!(bytes.len(), 52 + 12 * sk.bones.len(), "{prefix}");
        *kinds.entry(sk.kind).or_default() += 1;
        bones += sk.bones.len();
        non_negative_lengths += sk.bones.iter().filter(|b| b.length >= 0.0).count();
        max_bones = max_bones.max(sk.bones.len());
        max_pieces = max_pieces.max(sk.pieces);
        max_slots = max_slots.max(sk.animations);
        max_tracks = max_tracks.max(sk.weapon_animations);
        expected.insert(skeleton_name(prefix));
        expected.insert(format!("{prefix}ab"));
        if sk.kind != BattleModelKind::Location {
            expected.insert(animation_pack_name(prefix));
        }
        for n in 0..sk.textures {
            expected.insert(texture_name(prefix, n).unwrap());
        }
        for slot in 0..sk.part_slots() {
            if sk.part_present(slot) {
                expected.insert(part_name(prefix, slot).unwrap());
                parts += 1;
            }
        }
        for n in 0..sk.weapons {
            expected.insert(weapon_name(prefix, n).unwrap());
        }
    }
    eprintln!("kinds {kinds:?}, bones {bones}, mesh files {parts}, max bones {max_bones}, max pieces {max_pieces}, max slots {max_slots}, max tracks {max_tracks}");
    assert_eq!(kinds[&BattleModelKind::Enemy], 370);
    assert_eq!(kinds[&BattleModelKind::Location], 90);
    assert_eq!(kinds[&BattleModelKind::Character], 21);
    assert_eq!((max_bones, max_pieces, max_slots, max_tracks), (48, 29, 56, 42));
    assert_eq!(non_negative_lengths, 18, "the stored lengths are negative, 18 are zero");
    // every entry of the archive is one of the names the rules produce, and every name exists
    let actual: BTreeSet<String> = lgp.list().iter().map(|e| e.name.to_ascii_lowercase()).collect();
    assert_eq!(actual.difference(&expected).count(), 0, "entries the rules do not explain");
    assert_eq!(expected.difference(&actual).count(), 0, "names the rules produce that do not exist");
    assert_eq!(actual.len(), 11_119);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA or FMV_FF7_BATTLE_LGP)"]
fn every_model_loads_and_the_ids_split_into_the_three_kinds() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(&path).unwrap();
    assert_eq!(models.ids_of_kind(BattleModelKind::Enemy), (0..370).collect::<Vec<_>>());
    assert_eq!(models.ids_of_kind(BattleModelKind::Location), (370..460).collect::<Vec<_>>());
    assert_eq!(models.ids_of_kind(BattleModelKind::Character), (460..481).collect::<Vec<_>>());
    assert_eq!(models.location_count(), 90);
    assert_eq!(models.location_prefix(0).as_deref(), Some("og"), "location 0 is model id 370");
    assert_eq!(models.location_prefix(9).as_deref(), Some("op"));
    assert_eq!(models.location_prefix(89).as_deref(), Some("rr"));
    assert_eq!(models.location_prefix(90), None);
    assert_eq!(models.enemy_prefix(16).as_deref(), Some("aq"));
    assert_eq!(models.enemy_prefix(369).as_deref(), Some("of"));
    assert_eq!(models.enemy_prefix(370), None);

    let started = std::time::Instant::now();
    let (mut slots, mut ok_slots, mut parts, mut triangles, mut weapons, mut tracks, mut ok_tracks) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut textured_parts, mut blended_parts, mut dropped, mut missing) = (0usize, 0usize, 0usize, 0usize);
    let mut bad_slot_models: Vec<String> = Vec::new();
    for id in 0..481usize {
        let prefix = skeleton::prefix_of_id(id).unwrap();
        let model = models.load_prefix(&prefix, None).unwrap_or_else(|e| panic!("{prefix}: {e}"));
        parts += model.model.parts.len();
        triangles += model.model.triangle_count();
        textured_parts += model.model.parts.iter().filter(|p| p.texture.is_some()).count();
        blended_parts += model.model.parts.iter().filter(|p| p.blend != PartBlend::Opaque).count();
        dropped += model.model.dropped_triangles;
        missing += model.model.missing_textures.len();
        if model.kind != BattleModelKind::Location {
            slots += model.animations.len();
            ok_slots += model.slot_ok.iter().filter(|ok| **ok).count();
            if model.slot_ok.iter().any(|ok| !ok) {
                bad_slot_models.push(prefix.clone());
            }
            // every slot poses the model
            for slot in 0..model.animations.len() {
                assert!(model.pose(slot, 0).is_some(), "{prefix} slot {slot}");
            }
        }
        weapons += model.weapons.len();
        tracks += model.weapon_tracks.len();
        ok_tracks += model.weapon_track_ok.iter().filter(|ok| **ok).count();
        for w in &model.weapons {
            dropped += w.dropped_triangles;
            triangles += w.triangle_count();
        }
        for (i, track) in model.weapon_tracks.iter().enumerate() {
            assert_eq!(track.bone_count(), 1, "{prefix} track {i}");
        }
    }
    eprintln!(
        "all 481 models in {:.1?}: {parts} parts ({textured_parts} textured, {blended_parts} blended), {triangles} triangles, {slots} skeleton slots ({ok_slots} real), {weapons} weapons, {tracks} weapon tracks ({ok_tracks} real), {dropped} dropped triangles, {missing} missing textures; models with unusable slots: {bad_slot_models:?}",
        started.elapsed()
    );
    assert_eq!((dropped, missing), (0, 0));
    // 7 837 slots in the packs: 707 special entries and one corrupt clip are stand-ins; the packs
    // hold 391 x (skeleton slots + weapon tracks)
    assert_eq!(slots + tracks, 7_837);
    assert_eq!((slots - ok_slots) + (tracks - ok_tracks), 708);
    assert_eq!(weapons, 16 * 16 + 19);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA or FMV_FF7_BATTLE_LGP)"]
fn the_standing_clips_put_the_feet_on_the_ground() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(&path).unwrap();
    let mut loaded: Vec<(String, BattleModel)> = Vec::new();
    for c in [BattleCharacter::Cloud, BattleCharacter::Tifa, BattleCharacter::Barret, BattleCharacter::Aerith] {
        loaded.push((c.display_name().to_string(), models.load_character(c).unwrap()));
    }
    loaded.push(("MP".to_string(), models.load_enemy(16).unwrap()));
    loaded.push(("Guard Hound".to_string(), models.load_enemy(17).unwrap()));
    for (name, model) in &loaded {
        let clip = &model.animations[0];
        assert!(model.slot_ok[0], "{name}");
        let mut worst = 0.0f32;
        let mut height = 0.0f32;
        for frame in 0..clip.frame_count() {
            let pose = model.pose(0, frame).unwrap();
            let (lo, hi) = model.model.posed_bounds(&pose, &mat4_identity()).unwrap();
            // y points down: the soles are the largest y, the top of the head the smallest
            worst = worst.max(hi[1].abs());
            height = height.max(-lo[1]);
        }
        eprintln!("{name}: slot 0 has {} frames, the lowest point stays within {worst:.1} of the ground, the head reaches {height:.0}", clip.frame_count());
        assert!(worst <= 30.0, "{name}: the feet drift {worst:.1} from y = 0");
        assert!((600.0..1300.0).contains(&height), "{name}: {height}");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA or FMV_FF7_BATTLE_LGP)"]
fn the_playable_characters_have_weapons_with_tracks_that_fit_their_slots() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(&path).unwrap();
    for c in BattleCharacter::ALL {
        let model = models.load_character(c).unwrap();
        eprintln!(
            "{:20} {}: {} bones, {} slots ({} real), {} weapons, {} tracks ({} real)",
            c.display_name(),
            c.prefix(),
            model.model.bones.len(),
            model.animations.len(),
            model.slot_ok.iter().filter(|ok| **ok).count(),
            model.weapons.len(),
            model.weapon_tracks.len(),
            model.weapon_track_ok.iter().filter(|ok| **ok).count(),
        );
        assert_eq!(model.weapons.len(), 16, "{c:?}");
        assert!(model.weapons.iter().all(|w| w.bones.len() == 1 && !w.parts.is_empty()), "{c:?}");
        // a track that exists has the frames of the slot it belongs to (the same frame index draws both)
        let mut same = 0usize;
        let mut differ = Vec::new();
        for (slot, track) in model.weapon_tracks.iter().enumerate() {
            if !model.weapon_track_ok[slot] || !model.slot_ok[slot] {
                continue;
            }
            if track.frame_count() == model.animations[slot].frame_count() {
                same += 1;
            } else {
                differ.push((slot, track.frame_count(), model.animations[slot].frame_count()));
            }
        }
        eprintln!("   tracks with the slot's frame count: {same}, others (slot, track, body): {differ:?}");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA or FMV_FF7_BATTLE_LGP)"]
fn every_location_loads_with_a_floor_near_the_ground() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(&path).unwrap();
    let mut floors = 0usize;
    let mut most_vertices = 0usize;
    for code in 0..90u16 {
        let arena = models.load_location(code).unwrap();
        assert_eq!(arena.model.bones.len(), 1);
        assert!(!arena.model.parts.is_empty(), "location {code}");
        // every texture coordinate is inside its texture (the repeats are baked into the geometry),
        // and the merged mesh the client builds stays well under its 200 000 vertex limit
        for part in &arena.model.parts {
            assert!(part.uvs.iter().flatten().all(|c| (0.0..=1.0).contains(c)), "location {code}");
        }
        let vertices: usize = arena.model.parts.iter().map(|p| p.positions.len()).sum();
        most_vertices = most_vertices.max(vertices);
        assert!(vertices < 100_000, "location {code}: {vertices} vertices");
        let pose = arena.pose(0, 0).unwrap();
        let (lo, hi) = arena.model.posed_bounds(&pose, &mat4_identity()).unwrap();
        // the ground (y = 0) is inside the model's vertical extent for all of them
        if lo[1] <= 0.0 && hi[1] >= -100.0 {
            floors += 1;
        }
        eprintln!("location {code:2} {}: {} parts, {} triangles, x {:.0}..{:.0}, y {:.0}..{:.0}, z {:.0}..{:.0}", arena.prefix, arena.model.parts.len(), arena.model.triangle_count(), lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]);
    }
    eprintln!("the largest location has {most_vertices} vertices");
    assert_eq!(floors, 90);
}
