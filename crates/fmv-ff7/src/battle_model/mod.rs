//! FF7 battle models from `battle/battle.lgp`: enemies, battle locations (the 3D backgrounds) and
//! the playable characters, with their animations, as the [`FieldModel`] and [`FieldAnimation`]
//! types the field characters use, so a renderer that draws those draws these unchanged.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 481 models of the
//! archive (11 119 entries): the ignored real-data tests of this module and the offscreen shots of
//! `fmv-client` (`avatar/shots/battle_models.rs`) assert and draw everything quoted here. Other
//! editions are unverified.
//!
//! # The archive
//!
//! Every model is a group of entries named by a two-letter prefix `ab` and a two-letter suffix:
//!
//! | entry | content |
//! |---|---|
//! | `abaa` | the skeleton ([`skeleton`]): header and bones; its first word says enemy, location or character |
//! | `abab` | extra data this module does not read (enemies' and characters' own run-time tables) |
//! | `abac`, `abad`, .. | the model's textures (`.tex`, [`crate::tex`]), as many as the skeleton says |
//! | `abam`, `aban`, .. | the meshes (`.p`, [`crate::pmodel`]), one per bone that has one, or per piece of a location |
//! | `abck`, `abcl`, .. | the weapon meshes of a playable character |
//! | `abda` | the animations ([`animation`]) |
//!
//! All 11 119 entries of the 481 prefixes are accounted for by these names, with no entry left over.
//! **Mesh files are the field loader's `.p`**: the same parser and the same winding (97.7% of the
//! enemy and 96.6% of the character parts are clockwise seen from outside by the sign of their
//! signed volume, as the field ones are, so the same corner swap makes them counter-clockwise, see
//! [`FieldModel`]'s module). Three things differ from the field models:
//!
//! * **No normals**: not one of the 8 979 meshes (parts and weapons) has any. [`MeshPart::normals`]
//!   is zero.
//! * **Every textured group is marked "blend on"** (source alpha, constant alpha 128) in its render
//!   state, all 1 198 location groups included. Read as the field loader reads it that would draw
//!   every arena half transparent, so a textured group is [`PartBlend::Opaque`] here and the
//!   texture's own colour key (alpha 0) cuts it out; only untextured groups with the blend bit
//!   (208 of them, 201 in the parts of the models) are [`PartBlend::Alpha`] with alpha 128/255.
//!   **UNVERIFIED**: that the game itself draws it so (a transparent texel's colour is the only
//!   place a texture's alpha comes from in the files).
//! * The group's texture number is the render state's word 4 as well (all 1 516 textured groups
//!   agree), and picks the texture file `abac` + number.
//! * **Textures repeat.** 890 of the 1 198 textured groups of the locations have texture
//!   coordinates outside `0..1` (down to -16): the game draws them with repeat addressing, the
//!   client's avatar sampler clamps. The loader moves every coordinate into the texture (cutting the
//!   few triangles that cross a repeat, see `tiling`) so the unchanged drawing path shows the repeat:
//!   with the clamp alone every arena is streaks of its textures' edge rows (drawn and looked at).
//!
//! # The three kinds and their ids
//!
//! The 481 model ids are the two letters in base 26 (`aa` is 0, `aq` is 16, `rt` is 461, see
//! [`skeleton::prefix_of_id`]); the skeleton's kind word sorts them into three runs:
//!
//! * **Enemies** are ids 0 to 369 (370 models). **The enemy id of a formation entry in `scene.bin` is
//!   the model id**: all 352 distinct enemy ids of the 256 scenes (625 scene enemy slots) name a
//!   model whose kind is enemy; the models are the ones the scene's enemy names say (id 16 `aq` is
//!   the MP, 17 `ar` the Guard Hound, 18 the Mono Drive, 21 the Sweeper, 25 the Whole Eater, 40
//!   Eligor, 49 the Pyramid, 53 the Hammer Blaster, 66 the Hundred Gunner, 81 the Midgar Zolom, 96
//!   Grangalan, 127 Reno, 170 the Bizarre Bug, 200 the Boundfat, 250 the Captain, 300 the Jamar
//!   Armor and 340 the Chocobo were drawn and compared with the names), see
//!   [`BattleModels::load_enemy`].
//! * **Battle locations** are ids 370 to 459 (90 models). The location code of a formation's battle
//!   setup (`BattleSetup::location`) counts from the first of them: code 0 is id 370 (`og`), code 9
//!   id 379 (`op`), code 89 id 459 (`rr`). The 89 codes the 256 scenes use are 0 to 89 except 82
//!   (28 setups have none: `0xFFFF`). **Evidence**, which can fail: the fields' random-encounter
//!   formations lead to the setups' codes, and the three fields of the first mission's Mako reactor
//!   (`md1stin`, `md1_1`, `md1_2`) fight in code 9 and nothing else, the two ice cave fields
//!   (`icedun_1`, `icedun_2`) only in 50 and the crater (`crater_1`) only in 62; the models drawn are
//!   a reactor hall (the Shinra emblem on the floor, hazard stripes, the emergency sign), a blue-black
//!   ice cave, grey rock and, for code 2 (the most used: 79 setups), grassland. See
//!   [`BattleModels::load_location`].
//! * **Characters** are ids 460 to 480 (21 models): the playable characters, some alternate forms
//!   of them and four creatures, see [`BattleCharacter`].
//!
//! # Conventions
//!
//! * **Native model space** is the field models': right-handed, `x` is the character's *left*, `y`
//!   points *down*, a character faces `-z` (see [`crate::anim`]). **One model unit is one battle
//!   unit**: the unit of the formation positions and camera records of `scene.bin` (a standing
//!   Cloud is about 860 units tall, the MP about 790; the formations of the first scenes put enemies
//!   at `z` from -1 500 to -3 000), so the field's model scale does not apply. The ground is at
//!   `y = 0`: with the clips' root translation applied the lowest vertex of every frame of the idle
//!   clip is within 16 units of it for Cloud, Tifa, Barret, Aerith, the MP and the Guard Hound (0.6
//!   to 15.6). The root's `z` is forward: the MP's slot 2 carries it 1 140 units along `-z`, slots 3
//!   and 4 hold it there and slot 5 brings it back, the last frame of each ending where the next
//!   begins (-1 139 and -1 145).
//! * In `scene.bin`'s battle space the camera looks from `+x` (positions like `(6173, -1776, 4517)`,
//!   `y` up is negative) at a point near the origin, so `+z` is to the viewer's right: the party
//!   stands at positive `z` facing `-z` (a model's natural heading) and the enemies at negative `z`,
//!   to be turned half a turn about `y` to face them.
//! * **Locations** have no skeleton: one identity bone, parts at their absolute positions (floor
//!   near `y = 0`, a sky dome of a few tens of thousands of units above and around), and a
//!   one-frame identity animation so the same drawing path takes them.
//! * **Animation slots.** `animations[i]` is slot `i` of the model's pack: the same number the
//!   enemy records of `scene.bin` use (`EnemyRecord::animation_indices`). A slot that is not an
//!   animation (a 4-byte "special" entry, 707 of the 7 837 slots) or does not decode holds a copy of
//!   frame 0 of the first slot that does, and [`BattleModel::slot_ok`] is false for it. Of the 2 502
//!   animation references the enemy records make, 2 148 land on real slots, 45 on special entries
//!   and 309 beyond the model's slots (all of them for attacks the record uses; 268 of the 309 belong
//!   to the placeholder enemies 0 to 9): the consumer must be ready for both.
//! * **Weapons** are one-bone [`FieldModel`]s in [`BattleModel::weapons`]: 16 for each of the 16
//!   character models (the party's weapons: Cloud's swords, Barret's gun arm) and one for each of 19
//!   enemies (the MP's baton: 17 vertices, hanging at his belt in the idle clip and raised in his
//!   melee slot). Where each is held is a separate per-animation track
//!   ([`BattleModel::weapon_tracks`], slot-aligned with `animations`): draw the weapon as a second
//!   model with the same placement and the same frame index as the body, **for enemies as well**.
//!   Evidence that track `i` belongs to slot `i`: all 776 real (track, slot) pairs of the 35 models
//!   with weapons have exactly the same frame count (Cloud: 40 of 40), and the Buster Sword is in
//!   Cloud's hands in the idle clip and in eight other slots drawn mid-clip.
//!
//! # Using it
//!
//! ```no_run
//! # use fmv_ff7::battle_model::{BattleCharacter, BattleModels};
//! # use fmv_ff7::model::{mat4_mul, mat4_translate, transform_point};
//! # fn demo(path: &std::path::Path) -> Result<(), fmv_ff7::battle_model::BattleModelError> {
//! let mut models = BattleModels::open(path)?;                  // battle.lgp, read-only
//! let mp = models.load_enemy(16)?;                             // enemy id of scene.bin
//! let cloud = models.load_character(BattleCharacter::Cloud)?;
//! let arena = models.load_location(9)?;
//! // pose slot 0 (idle) at frame 3, standing at battle position (x, y, z)
//! if let Some(pose) = mp.model.pose(&mp.animations[0], 3) {
//!     let place = mat4_translate([-500.0, 0.0, -1500.0]);
//!     for part in &mp.model.parts {
//!         let to_battle = mat4_mul(&place, &pose[part.bone_index]);
//!         let _first = part.positions.first().map(|p| transform_point(&to_battle, *p));
//!     }
//! }
//! # let _ = (cloud, arena);
//! # Ok(()) }
//! ```
//!
//! # Safety against untrusted input
//!
//! Every file is parsed by a module that bounds its allocations by the file's own size, a model may
//! read at most [`MAX_BATTLE_LOAD_BYTES`] of archive entries, and a clip that does not decode
//! costs its slot, not the model.

pub mod animation;
pub mod skeleton;
mod tiling;

use crate::anim::FieldAnimation;
use crate::lgp::{LgpArchive, LgpError};
use crate::model::{FieldModel, MeshPart, ModelBone, ModelError, PartBlend};
use crate::pmodel::{PModel, PRenderState};
use crate::tex::{parse_tex, RgbaImage};
use animation::{clip_to_animation, decode_clip, rest_animation, split_pack, PackEntry};
use skeleton::{
    animation_pack_name, id_of_prefix, part_name, prefix_of_id, skeleton_name, texture_name, weapon_name, BattleModelKind,
    BattleSkeleton,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tiling::tile_group;

/// Cumulative cap on the archive entry bytes one model load reads (64 MiB: the largest model of
/// the owned archive reads well under 4 MiB).
pub const MAX_BATTLE_LOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Errors from opening `battle.lgp` or loading a model from it.
#[derive(Debug, Error)]
pub enum BattleModelError {
    #[error("battle.lgp: {0}")]
    Archive(#[from] LgpError),
    #[error("battle.lgp has no {kind} entry named {name:?}")]
    Missing { kind: &'static str, name: String },
    /// A file of the model is malformed (or the model's own parsers refuse it).
    #[error("battle model data: {0}")]
    Format(#[from] ModelError),
    #[error("the archive has no battle model skeletons (not a battle.lgp)")]
    NoModels,
    #[error("no {what}")]
    NoSuchModel { what: String },
    #[error("battle model {prefix:?} is a {found}, not a {wanted}")]
    WrongKind { prefix: String, found: &'static str, wanted: &'static str },
    #[error("the model needs more than {limit} bytes of archive entries")]
    Budget { limit: u64 },
}

/// The playable characters (and their stable table of model prefixes). **Identified on the owned
/// archive by drawing every one of the 21 character models** (idle clip, weapon 0): the standard
/// model of each is the first of its group in id order. The other models of the kind are alternate
/// forms: Cloud again (`si`), three more Barrets (`sc`, `sd`, `se`), two more Vincents (`sg`, `sh`),
/// the frog (`rs`) and four creatures (`sj` to `sm`); they load through
/// [`BattleModels::load_prefix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BattleCharacter {
    Cloud,
    Barret,
    Tifa,
    Aerith,
    RedXiii,
    Yuffie,
    CaitSith,
    Vincent,
    Cid,
    Sephiroth,
}

impl BattleCharacter {
    /// Every character, in the game's party order (Cloud, Barret, Tifa, Aerith, Red XIII, Yuffie,
    /// Cait Sith, Vincent, Cid) and then Sephiroth.
    pub const ALL: [BattleCharacter; 10] = [
        Self::Cloud,
        Self::Barret,
        Self::Tifa,
        Self::Aerith,
        Self::RedXiii,
        Self::Yuffie,
        Self::CaitSith,
        Self::Vincent,
        Self::Cid,
        Self::Sephiroth,
    ];

    /// The two-letter model prefix (`rt` for Cloud).
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Cloud => "rt",
            Self::Tifa => "ru",
            Self::Aerith => "rv",
            Self::RedXiii => "rw",
            Self::Yuffie => "rx",
            Self::CaitSith => "ry",
            Self::Cid => "rz",
            Self::Sephiroth => "sa",
            Self::Barret => "sb",
            Self::Vincent => "sf",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Cloud => "Cloud Strife",
            Self::Barret => "Barret Wallace",
            Self::Tifa => "Tifa Lockhart",
            Self::Aerith => "Aerith Gainsborough",
            Self::RedXiii => "Red XIII",
            Self::Yuffie => "Yuffie Kisaragi",
            Self::CaitSith => "Cait Sith",
            Self::Vincent => "Vincent Valentine",
            Self::Cid => "Cid Highwind",
            Self::Sephiroth => "Sephiroth",
        }
    }

    /// The character an appearance id means (`ff7/cloud`, see [`crate::playable`]; ASCII
    /// case-insensitive, surrounding whitespace ignored), `None` for an id this table does not know.
    pub fn from_appearance_ref(appearance_ref: &str) -> Option<Self> {
        let wanted = appearance_ref.trim().to_ascii_lowercase();
        let name = wanted.strip_prefix("ff7/")?;
        Some(match name {
            "cloud" => Self::Cloud,
            "barret" => Self::Barret,
            "tifa" => Self::Tifa,
            "aerith" => Self::Aerith,
            "red_xiii" => Self::RedXiii,
            "yuffie" => Self::Yuffie,
            "cait_sith" => Self::CaitSith,
            "vincent" => Self::Vincent,
            "cid" => Self::Cid,
            "sephiroth" => Self::Sephiroth,
            _ => return None,
        })
    }
}

/// A loaded battle model.
#[derive(Debug, Clone)]
pub struct BattleModel {
    pub kind: BattleModelKind,
    /// The two-letter prefix of the model's entries (`aq`).
    pub prefix: String,
    /// Skeleton and meshes. For a location: one identity bone and the pieces at their absolute
    /// positions.
    pub model: FieldModel,
    /// One animation per slot of the pack, index-aligned with the game's slot numbers (see the
    /// module docs); never empty (a location has one identity animation).
    pub animations: Vec<FieldAnimation>,
    /// Whether `animations[i]` is the slot's own clip (`false`: a stand-in copy of frame 0 of the
    /// first good slot).
    pub slot_ok: Vec<bool>,
    /// The weapon meshes (16 for a character model, 1 for 19 enemies, none for the rest), each a
    /// one-bone model.
    pub weapons: Vec<FieldModel>,
    /// Where the weapon is held, per animation slot: a one-bone animation whose root translation and
    /// rotation are the weapon's placement in the character's native space (the same frame index
    /// as the body's clip). Slot-aligned with `animations` but possibly shorter (Cloud has 42
    /// tracks for 52 slots): a slot past the end has no track. Empty for models without weapons.
    pub weapon_tracks: Vec<FieldAnimation>,
    /// Whether `weapon_tracks[i]` is the track's own clip.
    pub weapon_track_ok: Vec<bool>,
}

impl BattleModel {
    /// Per-bone matrices of slot `slot` at `frame` (see [`FieldAnimation::pose`]; frames loop).
    pub fn pose(&self, slot: usize, frame: usize) -> Option<Vec<[f32; 16]>> {
        self.model.pose(self.animations.get(slot)?, frame)
    }

    /// The matrix that places weapon `weapon` for slot `slot` at `frame`, in the character's native
    /// space (the weapon's own vertices are in its own frame), `None` if there is no such weapon or
    /// track.
    pub fn weapon_pose(&self, weapon: usize, slot: usize, frame: usize) -> Option<[f32; 16]> {
        let model = self.weapons.get(weapon)?;
        let pose = model.pose(self.weapon_tracks.get(slot)?, frame)?;
        pose.first().copied()
    }
}

/// The index of an opened `battle.lgp`.
struct Index {
    /// Model id -> kind, for every prefix with a readable skeleton.
    kinds: BTreeMap<usize, BattleModelKind>,
    /// The id of the first battle location (code 0).
    first_location: Option<usize>,
}

/// An open `battle.lgp` (read-only) and the means to load its models.
pub struct BattleModels<R = std::fs::File> {
    lgp: LgpArchive<R>,
    index: Index,
}

impl<R> std::fmt::Debug for BattleModels<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BattleModels").field("models", &self.index.kinds.len()).finish_non_exhaustive()
    }
}

impl BattleModels<std::fs::File> {
    /// Open `battle.lgp` at `path` read-only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BattleModelError> {
        Self::from_archive(LgpArchive::open_path(path)?)
    }
}

/// Read one archive entry, charging it against the cumulative budget.
fn read_entry<R: Read + Seek>(
    lgp: &mut LgpArchive<R>,
    kind: &'static str,
    name: &str,
    budget: &mut u64,
) -> Result<Vec<u8>, BattleModelError> {
    let bytes = match lgp.read(name) {
        Ok(bytes) => bytes,
        Err(LgpError::NotFound(_)) => return Err(BattleModelError::Missing { kind, name: name.to_string() }),
        Err(other) => return Err(other.into()),
    };
    let len = bytes.len() as u64;
    if len > *budget {
        return Err(BattleModelError::Budget { limit: MAX_BATTLE_LOAD_BYTES });
    }
    *budget -= len;
    Ok(bytes)
}

impl<R: Read + Seek> BattleModels<R> {
    /// Index an opened archive: reads the skeleton header of every model (about 480 small
    /// entries). An error if it has none.
    pub fn from_archive(mut lgp: LgpArchive<R>) -> Result<Self, BattleModelError> {
        let mut prefixes: Vec<String> = lgp
            .list()
            .iter()
            .filter_map(|e| {
                let name = e.name.to_ascii_lowercase();
                let stem = name.strip_suffix("aa")?;
                (stem.len() == 2 && id_of_prefix(stem).is_some()).then(|| stem.to_string())
            })
            .collect();
        prefixes.sort();
        prefixes.dedup();
        let mut kinds = BTreeMap::new();
        let mut budget = MAX_BATTLE_LOAD_BYTES;
        for prefix in &prefixes {
            let Some(id) = id_of_prefix(prefix) else { continue };
            // A skeleton that does not read is not a model (it simply cannot be loaded later).
            let Ok(bytes) = read_entry(&mut lgp, "skeleton", &skeleton_name(prefix), &mut budget) else { continue };
            if let Ok(skeleton) = BattleSkeleton::parse(&bytes) {
                kinds.insert(id, skeleton.kind);
            }
        }
        if kinds.is_empty() {
            return Err(BattleModelError::NoModels);
        }
        let first_location = kinds.iter().find(|(_, kind)| **kind == BattleModelKind::Location).map(|(id, _)| *id);
        Ok(Self { lgp, index: Index { kinds, first_location } })
    }

    /// The model ids of every model of `kind`, in order.
    pub fn ids_of_kind(&self, kind: BattleModelKind) -> Vec<usize> {
        self.index.kinds.iter().filter(|(_, k)| **k == kind).map(|(id, _)| *id).collect()
    }

    /// The kind of the model with prefix `prefix`, `None` if the archive has no such model.
    pub fn kind_of(&self, prefix: &str) -> Option<BattleModelKind> {
        self.index.kinds.get(&id_of_prefix(prefix)?).copied()
    }

    /// The prefix of the model of enemy `enemy_id` (the id `scene.bin` gives): the id in base 26,
    /// `None` unless the archive has an enemy model with that id.
    pub fn enemy_prefix(&self, enemy_id: u16) -> Option<String> {
        let id = usize::from(enemy_id);
        (self.index.kinds.get(&id) == Some(&BattleModelKind::Enemy)).then(|| prefix_of_id(id)).flatten()
    }

    /// The prefix of the battle location with code `code` (`BattleSetup::location` of `scene.bin`):
    /// the `code`-th location model, counting from the first, `None` unless it exists.
    pub fn location_prefix(&self, code: u16) -> Option<String> {
        let id = self.index.first_location?.checked_add(usize::from(code))?;
        (self.index.kinds.get(&id) == Some(&BattleModelKind::Location)).then(|| prefix_of_id(id)).flatten()
    }

    /// Number of battle location models.
    pub fn location_count(&self) -> usize {
        self.index.kinds.values().filter(|k| **k == BattleModelKind::Location).count()
    }

    /// Load the model of enemy `enemy_id` (the enemy id of a `scene.bin` formation entry).
    pub fn load_enemy(&mut self, enemy_id: u16) -> Result<BattleModel, BattleModelError> {
        let prefix = self
            .enemy_prefix(enemy_id)
            .ok_or_else(|| BattleModelError::NoSuchModel { what: format!("enemy model for enemy id {enemy_id}") })?;
        self.load_model(&prefix)
    }

    /// Load the battle location with code `code` (`BattleSetup::location`): its pieces as one
    /// model with one identity bone.
    pub fn load_location(&mut self, code: u16) -> Result<BattleModel, BattleModelError> {
        let prefix = self
            .location_prefix(code)
            .ok_or_else(|| BattleModelError::NoSuchModel { what: format!("battle location with code {code}") })?;
        self.load_model(&prefix)
    }

    /// Load a playable character's standard model.
    pub fn load_character(&mut self, which: BattleCharacter) -> Result<BattleModel, BattleModelError> {
        self.load_prefix(which.prefix(), Some(BattleModelKind::Character))
    }

    /// Load any model by its two-letter prefix (`rt`), checking its kind when `expect` is given.
    pub fn load_prefix(&mut self, prefix: &str, expect: Option<BattleModelKind>) -> Result<BattleModel, BattleModelError> {
        let prefix = prefix.trim().to_ascii_lowercase();
        let found = self
            .kind_of(&prefix)
            .ok_or_else(|| BattleModelError::NoSuchModel { what: format!("battle model with prefix {prefix:?}") })?;
        if let Some(wanted) = expect {
            if wanted != found {
                return Err(BattleModelError::WrongKind { prefix, found: found.label(), wanted: wanted.label() });
            }
        }
        self.load_model(&prefix)
    }

    fn load_model(&mut self, prefix: &str) -> Result<BattleModel, BattleModelError> {
        let mut budget = MAX_BATTLE_LOAD_BYTES;
        let skeleton_bytes = read_entry(&mut self.lgp, "skeleton", &skeleton_name(prefix), &mut budget)?;
        let skeleton = BattleSkeleton::parse(&skeleton_bytes)?;
        let is_location = skeleton.kind == BattleModelKind::Location;

        // Skeleton: positive lengths (the stored ones are negative, see `skeleton`); a location
        // is one identity bone that every piece hangs off.
        let bones: Vec<ModelBone> = if is_location {
            vec![ModelBone { name: "location".to_string(), parent: None, length: 0.0 }]
        } else {
            skeleton
                .bones
                .iter()
                .enumerate()
                .map(|(i, b)| ModelBone {
                    name: format!("bone{i}"),
                    parent: b.parent,
                    length: if b.length == 0.0 { 0.0 } else { -b.length },
                })
                .collect()
        };

        let mut textures = Textures::new(prefix, skeleton.textures);
        let mut parts: Vec<MeshPart> = Vec::new();
        let mut dropped = 0usize;
        for slot in 0..skeleton.part_slots() {
            if !skeleton.part_present(slot) {
                continue;
            }
            let name = part_name(prefix, slot)
                .ok_or_else(|| BattleModelError::NoSuchModel { what: format!("part file name for slot {slot}") })?;
            let bytes = read_entry(&mut self.lgp, "mesh", &name, &mut budget)?;
            let pmodel = PModel::parse(&bytes)?;
            let bone_index = if is_location { 0 } else { slot };
            build_parts(bone_index, &pmodel, &mut self.lgp, &mut textures, &mut budget, &mut parts, &mut dropped)?;
        }
        let model = FieldModel {
            hrc_name: prefix.to_string(),
            skeleton_name: format!("battle {}", skeleton.kind.label()),
            bones,
            parts,
            dropped_triangles: dropped,
            missing_textures: Vec::new(),
        };

        // Weapons: one-bone models of their own (they share the model's textures).
        let mut weapons = Vec::with_capacity(skeleton.weapons);
        for number in 0..skeleton.weapons {
            let name = weapon_name(prefix, number)
                .ok_or_else(|| BattleModelError::NoSuchModel { what: format!("weapon file name {number}") })?;
            let bytes = read_entry(&mut self.lgp, "weapon", &name, &mut budget)?;
            let pmodel = PModel::parse(&bytes)?;
            let mut parts = Vec::new();
            let mut weapon_dropped = 0usize;
            build_parts(0, &pmodel, &mut self.lgp, &mut textures, &mut budget, &mut parts, &mut weapon_dropped)?;
            weapons.push(FieldModel {
                hrc_name: name.clone(),
                skeleton_name: "battle weapon".to_string(),
                bones: vec![ModelBone { name: "weapon".to_string(), parent: None, length: 0.0 }],
                parts,
                dropped_triangles: weapon_dropped,
                missing_textures: Vec::new(),
            });
        }

        // Animations.
        let (animations, slot_ok, weapon_tracks, weapon_track_ok) = if is_location {
            (vec![rest_animation(None, 1)?], vec![true], Vec::new(), Vec::new())
        } else {
            let pack_bytes = read_entry(&mut self.lgp, "animation pack", &animation_pack_name(prefix), &mut budget)?;
            let entries = split_pack(&pack_bytes)?;
            let bone_count = model.bones.len();
            let (animations, slot_ok) = decode_slots(&entries, 0, skeleton.animations, bone_count, bone_count)?;
            let (tracks, track_ok) =
                decode_slots(&entries, skeleton.animations, skeleton.weapon_animations, 1, 1)?;
            (animations, slot_ok, tracks, track_ok)
        };

        let mut model = model;
        model.missing_textures = textures.missing;
        Ok(BattleModel {
            kind: skeleton.kind,
            prefix: prefix.to_string(),
            model,
            animations,
            slot_ok,
            weapons,
            weapon_tracks,
            weapon_track_ok,
        })
    }
}

/// Decode `count` slots of `entries` starting at `first`: each slot's clip as an animation for a
/// skeleton of `bones` bones (`triplet_bones` decides the angle triples per frame: 1 for a single
/// bone), or a stand-in (frame 0 of the first good slot, or the identity pose) for a slot that is
/// special, missing or does not decode. Returns the animations and whether each is the slot's own.
fn decode_slots(
    entries: &[PackEntry<'_>],
    first: usize,
    count: usize,
    bones: usize,
    triplet_bones: usize,
) -> Result<(Vec<FieldAnimation>, Vec<bool>), BattleModelError> {
    let mut decoded: Vec<Option<FieldAnimation>> = Vec::with_capacity(count);
    for i in 0..count {
        let animation = first.checked_add(i).and_then(|at| entries.get(at)).filter(|e| e.is_clip()).and_then(|entry| {
            let triplets = if triplet_bones <= 1 { 1 } else { triplet_bones + 1 };
            let clip = decode_clip(entry, triplets).ok()?;
            clip_to_animation(&clip, bones).ok()
        });
        decoded.push(animation);
    }
    let stand_in = rest_animation(decoded.iter().flatten().next(), bones.max(1))?;
    let slot_ok = decoded.iter().map(Option::is_some).collect();
    let animations = decoded.into_iter().map(|a| a.unwrap_or_else(|| stand_in.clone())).collect();
    Ok((animations, slot_ok))
}

/// The textures of one model, loaded on first use.
struct Textures<'a> {
    prefix: &'a str,
    /// How many texture files the skeleton declares.
    declared: usize,
    loaded: HashMap<usize, Option<Arc<RgbaImage>>>,
    missing: Vec<String>,
}

impl<'a> Textures<'a> {
    fn new(prefix: &'a str, declared: usize) -> Self {
        Self { prefix, declared, loaded: HashMap::new(), missing: Vec::new() }
    }

    /// The entry stem and the decoded image of texture `number` (`None`: it could not be had; the
    /// part is then drawn untextured and the stem is listed in `FieldModel::missing_textures`).
    fn get<R: Read + Seek>(
        &mut self,
        lgp: &mut LgpArchive<R>,
        budget: &mut u64,
        number: usize,
    ) -> Result<(Option<String>, Option<Arc<RgbaImage>>), BattleModelError> {
        let Some(name) = texture_name(self.prefix, number) else { return Ok((None, None)) };
        if !self.loaded.contains_key(&number) {
            let image = if number >= self.declared {
                None
            } else {
                match read_entry(lgp, "texture", &name, budget) {
                    Ok(bytes) => parse_tex(&bytes).ok().map(Arc::new),
                    Err(BattleModelError::Budget { limit }) => return Err(BattleModelError::Budget { limit }),
                    Err(_) => None,
                }
            };
            if image.is_none() {
                self.missing.push(name.clone());
            }
            self.loaded.insert(number, image);
        }
        Ok((Some(name), self.loaded.get(&number).cloned().flatten()))
    }
}

/// How a battle group is blended (see the module docs): a textured group is opaque (its texture's
/// colour key cuts it out), an untextured one blends when its render state says so, as a field
/// group does (**UNVERIFIED**, [`crate::pmodel::PRenderState`]).
fn classify_blend(state: &PRenderState, textured: bool) -> (PartBlend, f32) {
    if textured || !state.blend {
        return (PartBlend::Opaque, 1.0);
    }
    let alpha = f32::from(state.alpha) / 255.0;
    // Direct3D blend factors: 5 = SRCALPHA, 2 = ONE
    if state.src_blend == 5 && state.dst_blend == 2 {
        (PartBlend::Additive, alpha)
    } else {
        (PartBlend::Alpha, alpha)
    }
}

/// The parts of one `.p` mesh as [`MeshPart`]s on bone `bone_index`: one per group, vertices
/// deduplicated, corners 1 and 2 of every triangle swapped (the files are clockwise seen from
/// outside, the output counter-clockwise: the field loader's rule), no normals.
fn build_parts<R: Read + Seek>(
    bone_index: usize,
    pmodel: &PModel,
    lgp: &mut LgpArchive<R>,
    textures: &mut Textures<'_>,
    budget: &mut u64,
    parts: &mut Vec<MeshPart>,
    dropped: &mut usize,
) -> Result<(), BattleModelError> {
    for (group_index, group) in pmodel.groups.iter().enumerate() {
        let resolved = pmodel.group_triangles(group);
        *dropped += resolved.dropped;
        if resolved.triangles.is_empty() {
            continue;
        }
        let (texture_name, texture) = if group.textured {
            textures.get(lgp, budget, group.texture_number as usize)?
        } else {
            (None, None)
        };

        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut colors: Vec<[u8; 4]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();

        // A textured group whose coordinates leave the texture repeats it: bake the repeat into
        // geometry (see `tiling`). `None` when it does not, or when it would grow without bound.
        let tiled = if group.textured && texture.is_some() { tile_group(pmodel, &resolved.triangles) } else { None };
        if let Some(pieces) = tiled {
            let mut seen: HashMap<[u32; 6], u32> = HashMap::new();
            for piece in &pieces {
                for corner in piece {
                    let color = corner.color.map(|c| c.round().clamp(0.0, 255.0) as u8);
                    let key = [
                        corner.position[0].to_bits(),
                        corner.position[1].to_bits(),
                        corner.position[2].to_bits(),
                        corner.uv[0].to_bits(),
                        corner.uv[1].to_bits(),
                        u32::from_le_bytes(color),
                    ];
                    let index = match seen.get(&key) {
                        Some(&i) => i,
                        None => {
                            let i = u32::try_from(positions.len()).map_err(|_| ModelError::TooMany {
                                what: "mesh part vertices",
                                count: positions.len() as u64,
                                limit: u64::from(u32::MAX),
                            })?;
                            positions.push(corner.position);
                            normals.push([0.0; 3]);
                            uvs.push(corner.uv);
                            colors.push(color);
                            seen.insert(key, i);
                            i
                        }
                    };
                    indices.push(index);
                }
            }
        } else {
            let mut seen: HashMap<(usize, Option<usize>, Option<usize>), u32> = HashMap::new();
            for triangle in &resolved.triangles {
                // The file's winding is clockwise seen from outside: swap corners 1 and 2.
                for corner in [triangle[0], triangle[2], triangle[1]] {
                    let key = (corner.vertex, corner.normal, corner.texcoord);
                    let index = match seen.get(&key) {
                        Some(&i) => i,
                        None => {
                            let i = u32::try_from(positions.len()).map_err(|_| ModelError::TooMany {
                                what: "mesh part vertices",
                                count: positions.len() as u64,
                                limit: u64::from(u32::MAX),
                            })?;
                            positions.push(pmodel.vertices.get(corner.vertex).copied().unwrap_or([0.0; 3]));
                            normals.push(corner.normal.and_then(|n| pmodel.normals.get(n)).copied().unwrap_or([0.0; 3]));
                            uvs.push(corner.texcoord.and_then(|t| pmodel.texcoords.get(t)).copied().unwrap_or([0.0; 2]));
                            colors.push(pmodel.vertex_colors.get(corner.vertex).copied().unwrap_or([255; 4]));
                            seen.insert(key, i);
                            i
                        }
                    };
                    indices.push(index);
                }
            }
        }
        let (blend, alpha) = classify_blend(&pmodel.render_state(group_index), group.textured);
        parts.push(MeshPart {
            bone_index,
            positions,
            normals,
            uvs,
            colors,
            indices,
            texture,
            texture_name,
            blend,
            alpha,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod real;
