//! `..aa`: the skeleton file of a battle model (the header and one 12-byte record per bone).
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 481 `aa` entries
//! of `battle/battle.lgp` (see the ignored real-data tests of [`super`], which assert the totals
//! quoted here). Measured facts (counts only):
//!
//! * Every file is exactly `52 + 12 * bones` bytes: a 52-byte header (13 little-endian `i32`) and
//!   one `{ i32 parent, f32 length, i32 has_model }` record per bone. A battle location has no
//!   bone records at all (its `bones` field is 0 and its file is 52 bytes).
//! * Header fields (the layout is that of the public notes of Kimera, see *Sources*; the names of
//!   the fields that this crate does not use are the notes' own guesses):
//!
//!   | offset | field | measured |
//!   |---|---|---|
//!   | `0x00` | kind | 0 for 370 files (enemies), 1 for 90 (battle locations), 2 for 21 (the playable characters and their alternates) |
//!   | `0x04`, `0x08` | unknown | `(1, 1)` for enemies and characters, `(1, 0)` for locations |
//!   | `0x0C` | bones | 1 to 48 (0 for a location) |
//!   | `0x10` | unknown | 0 |
//!   | `0x14` | pieces | the number of meshes of a location (4 to 29), 0 otherwise |
//!   | `0x18` | textures | 0 to 4 (enemies, characters) and 1 to 8 (locations) |
//!   | `0x1C` | animations | the skeleton's own animation slots, 2 to 56 |
//!   | `0x20` | unknown | animations + 2 for enemies and characters, 0 for locations |
//!   | `0x24` | weapons | 0, or 1 for 19 enemies and 16 for 16 characters |
//!   | `0x28` | weapon animations | 0, 16 for those 19 enemies, 12 to 42 for the characters |
//!   | `0x2C` | unknown | 0 |
//!   | `0x30` | unknown | large and different in every file (a run-time pointer) |
//!
//! * Every bone's parent is `-1` (a root-attached chain: legs, tails and the like hang off the
//!   root) or an *earlier* bone; lengths are finite and negative (or zero for 18 bones): a child's
//!   origin is `length` along the parent's own `z` axis, so the mesh of a bone lies in `z` from
//!   `length` to about `0` (for Cloud's forearm bone the mesh's `z` runs from -153 to 56 with a
//!   length of -133). [`BattleBone::length`] keeps the stored (negative) value; the
//!   [`ModelBone`](crate::model::ModelBone) the loader builds from it has the positive length the
//!   field conventions use.
//! * `has_model` is 0 or 1, and the part file of bone `i` exists exactly when it is 1 (12 220 part
//!   names over the 481 skeletons, 0 mismatches), see [`part_name`].
//!
//! # Sources
//!
//! * KimeraCS `FF7BattleSkeleton.cs` <https://github.com/LaZar00/KimeraCS> (the 52-byte header of
//!   13 `Int32`, the bone record, the file-name rules for parts `AM..`, textures `AC..` and
//!   weapons `CK..`). Only layout facts were taken; every one was re-checked against the owned
//!   files, which agree with it.
//!
//! # Safety against untrusted input
//!
//! Every count is checked against the file's own length (checked `u64` arithmetic) before anything
//! is allocated, and against a cap that is far above any real file's value.

use crate::model::ModelError;
use crate::util::{slice_at, u32_at};

/// Bytes of the fixed header (13 `i32`).
pub const SKELETON_HEADER_LEN: usize = 52;
/// Bytes of one bone record.
pub const BONE_RECORD_LEN: usize = 12;
/// Most bones accepted (the owned skeletons have at most 48).
pub const MAX_BONES: usize = 128;
/// Most pieces of a battle location accepted (the owned ones have at most 29).
pub const MAX_PIECES: usize = 128;
/// Most textures accepted: the file names run `ac`..`az` (24 letters).
pub const MAX_TEXTURES: usize = 24;
/// Most weapons accepted: the file names run `ck`..`cz` (16 letters).
pub const MAX_WEAPONS: usize = 16;
/// Most animation slots accepted per list (the owned maxima are 56 and 42).
pub const MAX_ANIMATION_SLOTS: usize = 512;

/// What a battle model is, from the first header word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BattleModelKind {
    /// An enemy (header word 0).
    Enemy,
    /// A battle location: the 3D background, a set of meshes with no skeleton (word 1).
    Location,
    /// A playable character, or one of its alternate forms (word 2).
    Character,
}

impl BattleModelKind {
    /// The kind a header word names.
    pub fn from_word(word: u32) -> Option<Self> {
        match word {
            0 => Some(Self::Enemy),
            1 => Some(Self::Location),
            2 => Some(Self::Character),
            _ => None,
        }
    }

    /// A short lower-case word for names and messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Enemy => "enemy",
            Self::Location => "location",
            Self::Character => "character",
        }
    }
}

/// One bone record, as stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BattleBone {
    /// The parent bone (always an earlier one), or `None` for a bone attached to the root.
    pub parent: Option<usize>,
    /// The stored length: finite and negative (a child's origin is this far along the bone's `z`).
    pub length: f32,
    /// Whether the bone has a mesh (a part file, see [`part_name`]).
    pub has_model: bool,
}

/// A decoded skeleton file.
#[derive(Debug, Clone, PartialEq)]
pub struct BattleSkeleton {
    pub kind: BattleModelKind,
    pub bones: Vec<BattleBone>,
    /// Meshes of a location (0 for the other kinds).
    pub pieces: usize,
    /// Texture files (`..ac`, `..ad`, ...).
    pub textures: usize,
    /// The skeleton's own animation slots (the first entries of the `da` pack).
    pub animations: usize,
    /// Weapon models (`..ck`, `..cl`, ...).
    pub weapons: usize,
    /// Weapon animation tracks (the entries of the `da` pack after the skeleton's own).
    pub weapon_animations: usize,
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "battle skeleton", why: why.into() }
}

impl BattleSkeleton {
    /// Parse an `aa` file.
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() < SKELETON_HEADER_LEN {
            return Err(ModelError::Truncated {
                what: "battle skeleton header",
                at: 0,
                needed: SKELETON_HEADER_LEN - bytes.len(),
            });
        }
        let word = |i: usize| {
            u32_at(bytes, i * 4).ok_or(ModelError::Truncated { what: "battle skeleton header", at: i * 4, needed: 4 })
        };
        let kind_word = word(0)?;
        let kind = BattleModelKind::from_word(kind_word)
            .ok_or_else(|| invalid(format!("kind {kind_word} (expected 0, 1 or 2)")))?;
        // A count word is a non-negative i32; anything with the top bit set is damage.
        let count = |i: usize, what: &'static str, limit: usize| -> Result<usize, ModelError> {
            let value = word(i)?;
            let n = usize::try_from(value).map_err(|_| invalid(format!("{what} {value} does not fit")))?;
            if value > i32::MAX as u32 || n > limit {
                return Err(ModelError::TooMany { what, count: u64::from(value), limit: limit as u64 });
            }
            Ok(n)
        };
        let n_bones = count(3, "battle bones", MAX_BONES)?;
        let pieces = count(5, "battle location pieces", MAX_PIECES)?;
        let textures = count(6, "battle textures", MAX_TEXTURES)?;
        let animations = count(7, "battle animations", MAX_ANIMATION_SLOTS)?;
        let weapons = count(9, "battle weapons", MAX_WEAPONS)?;
        let weapon_animations = count(10, "battle weapon animations", MAX_ANIMATION_SLOTS)?;

        match kind {
            BattleModelKind::Location => {
                if n_bones != 0 {
                    return Err(invalid(format!("a battle location with {n_bones} bones")));
                }
                if pieces == 0 {
                    return Err(invalid("a battle location with no pieces"));
                }
            }
            BattleModelKind::Enemy | BattleModelKind::Character => {
                if n_bones == 0 {
                    return Err(invalid(format!("an {} with no bones", kind.label())));
                }
            }
        }

        // Checked arithmetic: n_bones <= 128, so the product cannot overflow, but the end must lie
        // inside the file.
        let records_len = n_bones * BONE_RECORD_LEN;
        let end = SKELETON_HEADER_LEN + records_len;
        if end > bytes.len() {
            return Err(ModelError::Truncated {
                what: "battle skeleton bones",
                at: bytes.len(),
                needed: end - bytes.len(),
            });
        }
        let mut bones = Vec::with_capacity(n_bones);
        for i in 0..n_bones {
            let at = SKELETON_HEADER_LEN + i * BONE_RECORD_LEN;
            let record = slice_at(bytes, at, BONE_RECORD_LEN).ok_or(ModelError::Truncated {
                what: "battle bone",
                at,
                needed: BONE_RECORD_LEN,
            })?;
            let parent_word = u32_at(record, 0).ok_or(ModelError::Truncated { what: "battle bone", at, needed: 4 })?;
            let parent = match parent_word as i32 {
                -1 => None,
                p => {
                    let p = usize::try_from(p).map_err(|_| invalid(format!("bone {i}: parent {p}")))?;
                    if p >= i {
                        return Err(invalid(format!("bone {i}: parent {p} is not an earlier bone")));
                    }
                    Some(p)
                }
            };
            let length = f32::from_bits(
                u32_at(record, 4).ok_or(ModelError::Truncated { what: "battle bone", at: at + 4, needed: 4 })?,
            );
            if !length.is_finite() {
                return Err(invalid(format!("bone {i}: length is not finite")));
            }
            let has_model =
                u32_at(record, 8).ok_or(ModelError::Truncated { what: "battle bone", at: at + 8, needed: 4 })? != 0;
            bones.push(BattleBone { parent, length, has_model });
        }
        Ok(Self { kind, bones, pieces, textures, animations, weapons, weapon_animations })
    }

    /// How many part files the model has names for: one per bone, or one per piece of a location
    /// (files that are not there are skipped by the loader exactly when the bone has no model).
    pub fn part_slots(&self) -> usize {
        match self.kind {
            BattleModelKind::Location => self.pieces,
            _ => self.bones.len(),
        }
    }

    /// Whether part slot `index` has a mesh: the bone's `has_model` flag, or every piece of a
    /// location.
    pub fn part_present(&self, index: usize) -> bool {
        match self.kind {
            BattleModelKind::Location => index < self.pieces,
            _ => self.bones.get(index).is_some_and(|b| b.has_model),
        }
    }
}

// ---------------------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------------------

/// The two lower-case letters of model id `id` in base 26 (`0` is `aa`, `16` is `aq`, `461` is
/// `rt`), or `None` beyond `zz` (675).
pub fn prefix_of_id(id: usize) -> Option<String> {
    if id >= 26 * 26 {
        return None;
    }
    let letter = |n: usize| char::from(b'a'.wrapping_add(u8::try_from(n).unwrap_or(0)));
    Some([letter(id / 26), letter(id % 26)].iter().collect())
}

/// The model id of a two-letter prefix (`aq` is 16), `None` if it is not two letters `a`..`z`
/// (case-insensitive).
pub fn id_of_prefix(prefix: &str) -> Option<usize> {
    let mut chars = prefix.chars();
    let (a, b) = (chars.next()?, chars.next()?);
    if chars.next().is_some() {
        return None;
    }
    let digit = |c: char| {
        let c = c.to_ascii_lowercase();
        c.is_ascii_lowercase().then(|| usize::from(c as u8 - b'a'))
    };
    Some(digit(a)? * 26 + digit(b)?)
}

/// `<prefix>aa`: the skeleton entry (archive names have no extension).
pub fn skeleton_name(prefix: &str) -> String {
    format!("{prefix}aa")
}

/// `<prefix>da`: the animation pack.
pub fn animation_pack_name(prefix: &str) -> String {
    format!("{prefix}da")
}

/// The part file of bone (or piece) `index`: letters run `am`, `an`, .. `az`, `ba`, .. (the third
/// and fourth letters are the number `12 + index` in base 26), so bone 0 is `<prefix>am` and bone
/// 49 is `<prefix>cj`. `None` past `zz`.
pub fn part_name(prefix: &str, index: usize) -> Option<String> {
    Some(format!("{prefix}{}", prefix_of_id(index.checked_add(12)?)?))
}

/// The texture file with number `number`: `<prefix>ac`, `<prefix>ad`, ..
pub fn texture_name(prefix: &str, number: usize) -> Option<String> {
    let letter = u8::try_from(number).ok().filter(|&n| usize::from(n) < MAX_TEXTURES)?;
    Some(format!("{prefix}a{}", char::from(b'c' + letter)))
}

/// The weapon file with number `number`: `<prefix>ck`, `<prefix>cl`, .. `<prefix>cz`.
pub fn weapon_name(prefix: &str, number: usize) -> Option<String> {
    let letter = u8::try_from(number).ok().filter(|&n| usize::from(n) < MAX_WEAPONS)?;
    Some(format!("{prefix}c{}", char::from(b'k' + letter)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A skeleton file in the documented layout, written field by field.
    pub(crate) struct SkeletonSpec {
        pub kind: u32,
        pub declared_bones: u32,
        pub pieces: u32,
        pub textures: u32,
        pub animations: u32,
        pub weapons: u32,
        pub weapon_animations: u32,
        /// `(parent, length, has_model)`
        pub bones: Vec<(i32, f32, u32)>,
    }

    impl SkeletonSpec {
        /// An enemy with three bones: a root with a model, a child without, a grandchild with.
        pub(crate) fn enemy() -> Self {
            Self {
                kind: 0,
                declared_bones: 3,
                pieces: 0,
                textures: 1,
                animations: 2,
                weapons: 0,
                weapon_animations: 0,
                bones: vec![(-1, -10.0, 1), (0, -20.0, 0), (1, -5.0, 1)],
            }
        }

        pub(crate) fn build(&self) -> Vec<u8> {
            let words: [u32; 13] = [
                self.kind,
                1,
                u32::from(self.kind != 1),
                self.declared_bones,
                0,
                self.pieces,
                self.textures,
                self.animations,
                self.animations + 2,
                self.weapons,
                self.weapon_animations,
                0,
                0xDEAD_BEEF,
            ];
            let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            for &(parent, length, has_model) in &self.bones {
                out.extend_from_slice(&parent.to_le_bytes());
                out.extend_from_slice(&length.to_le_bytes());
                out.extend_from_slice(&has_model.to_le_bytes());
            }
            out
        }
    }

    #[test]
    fn parses_the_documented_layout() {
        let bytes = SkeletonSpec::enemy().build();
        assert_eq!(bytes.len(), 52 + 3 * 12);
        let sk = BattleSkeleton::parse(&bytes).unwrap();
        assert_eq!(sk.kind, BattleModelKind::Enemy);
        assert_eq!((sk.textures, sk.animations, sk.weapons, sk.weapon_animations, sk.pieces), (1, 2, 0, 0, 0));
        assert_eq!(sk.bones.len(), 3);
        assert_eq!(sk.bones[0], BattleBone { parent: None, length: -10.0, has_model: true });
        assert_eq!(sk.bones[1], BattleBone { parent: Some(0), length: -20.0, has_model: false });
        assert_eq!(sk.bones[2], BattleBone { parent: Some(1), length: -5.0, has_model: true });
        assert_eq!(sk.part_slots(), 3);
        assert!(sk.part_present(0) && !sk.part_present(1) && sk.part_present(2) && !sk.part_present(3));
    }

    #[test]
    fn a_location_has_pieces_and_no_bone_records() {
        let spec = SkeletonSpec {
            kind: 1,
            declared_bones: 0,
            pieces: 5,
            textures: 3,
            animations: 0,
            bones: vec![],
            ..SkeletonSpec::enemy()
        };
        let sk = BattleSkeleton::parse(&spec.build()).unwrap();
        assert_eq!(sk.kind, BattleModelKind::Location);
        assert_eq!((sk.bones.len(), sk.pieces, sk.part_slots(), sk.textures), (0, 5, 5, 3));
        assert!(sk.part_present(4) && !sk.part_present(5));
    }

    #[test]
    fn rejects_damage_without_panicking_or_allocating_wildly() {
        let good = SkeletonSpec::enemy().build();
        assert!(matches!(BattleSkeleton::parse(&[]), Err(ModelError::Truncated { .. })));
        assert!(matches!(BattleSkeleton::parse(good.get(..51).unwrap()), Err(ModelError::Truncated { .. })));
        // the last bone record cut short
        assert!(matches!(BattleSkeleton::parse(good.get(..good.len() - 1).unwrap()), Err(ModelError::Truncated { .. })));
        let poke = |word: usize, value: u32| {
            let mut b = good.clone();
            b[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            BattleSkeleton::parse(&b)
        };
        assert!(matches!(poke(0, 3), Err(ModelError::Invalid { .. })), "unknown kind");
        assert!(matches!(poke(3, 0), Err(ModelError::Invalid { .. })), "an enemy with no bones");
        assert!(matches!(poke(3, 100), Err(ModelError::Truncated { .. })), "more bones than the file holds");
        assert!(matches!(poke(3, u32::MAX), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(3, 0x8000_0000), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(3, MAX_BONES as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(5, u32::MAX), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(6, MAX_TEXTURES as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(7, MAX_ANIMATION_SLOTS as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(9, MAX_WEAPONS as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(10, u32::MAX), Err(ModelError::TooMany { .. })));
        // a location must have pieces and no bones
        assert!(matches!(poke(0, 1), Err(ModelError::Invalid { .. })), "a location with bones");
        // bad bone records: a parent that is not an earlier bone, a non-finite length
        let bone = |i: usize, field: usize, value: u32| {
            let mut b = good.clone();
            let at = 52 + i * 12 + field * 4;
            b[at..at + 4].copy_from_slice(&value.to_le_bytes());
            BattleSkeleton::parse(&b)
        };
        assert!(matches!(bone(1, 0, 5), Err(ModelError::Invalid { .. })), "forward parent");
        assert!(matches!(bone(1, 0, 1), Err(ModelError::Invalid { .. })), "self parent");
        assert!(matches!(bone(1, 0, u32::MAX - 1), Err(ModelError::Invalid { .. })), "negative parent other than -1");
        assert!(matches!(bone(0, 1, f32::NAN.to_bits()), Err(ModelError::Invalid { .. })));
        assert!(matches!(bone(0, 1, f32::INFINITY.to_bits()), Err(ModelError::Invalid { .. })));
        // any non-zero has_model word means "has a model"
        assert!(bone(1, 2, 7).unwrap().bones[1].has_model);
    }

    #[test]
    fn damaged_files_never_panic() {
        let good = SkeletonSpec::enemy().build();
        for n in 0..good.len() {
            let _ = BattleSkeleton::parse(good.get(..n).unwrap_or(&[]));
        }
        for i in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                let _ = BattleSkeleton::parse(&b);
            }
        }
    }

    #[test]
    fn model_ids_and_prefixes_are_base_26_letters() {
        assert_eq!(prefix_of_id(0).as_deref(), Some("aa"));
        assert_eq!(prefix_of_id(16).as_deref(), Some("aq"));
        assert_eq!(prefix_of_id(17).as_deref(), Some("ar"));
        assert_eq!(prefix_of_id(25).as_deref(), Some("az"));
        assert_eq!(prefix_of_id(26).as_deref(), Some("ba"));
        assert_eq!(prefix_of_id(461).as_deref(), Some("rt"));
        assert_eq!(prefix_of_id(675).as_deref(), Some("zz"));
        assert_eq!(prefix_of_id(676), None);
        assert_eq!(prefix_of_id(usize::MAX), None);
        for id in 0..676 {
            assert_eq!(id_of_prefix(&prefix_of_id(id).unwrap()), Some(id));
        }
        assert_eq!(id_of_prefix("RT"), Some(461));
        for bad in ["", "a", "abc", "a1", "é", "-a", " a"] {
            assert_eq!(id_of_prefix(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn file_names_follow_the_documented_rules() {
        // parts: am..az, ba..bz, ca..cj for bones 0..49
        assert_eq!(part_name("rt", 0).as_deref(), Some("rtam"));
        assert_eq!(part_name("rt", 13).as_deref(), Some("rtaz"));
        assert_eq!(part_name("rt", 14).as_deref(), Some("rtba"));
        assert_eq!(part_name("rt", 39).as_deref(), Some("rtbz"));
        assert_eq!(part_name("rt", 40).as_deref(), Some("rtca"));
        assert_eq!(part_name("rt", 49).as_deref(), Some("rtcj"));
        assert_eq!(part_name("rt", 664), None);
        assert_eq!(part_name("rt", usize::MAX), None);
        assert_eq!(texture_name("rt", 0).as_deref(), Some("rtac"));
        assert_eq!(texture_name("rt", 7).as_deref(), Some("rtaj"));
        assert_eq!(texture_name("rt", 23).as_deref(), Some("rtaz"));
        assert_eq!(texture_name("rt", 24), None);
        assert_eq!(weapon_name("rt", 0).as_deref(), Some("rtck"));
        assert_eq!(weapon_name("rt", 15).as_deref(), Some("rtcz"));
        assert_eq!(weapon_name("rt", 16), None);
        assert_eq!(skeleton_name("rt"), "rtaa");
        assert_eq!(animation_pack_name("rt"), "rtda");
    }
}
