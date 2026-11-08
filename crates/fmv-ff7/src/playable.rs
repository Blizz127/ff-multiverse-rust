//! The playable field character models, keyed by stable logical appearance ids.
//!
//! Other crates (the protocol's `appearance_ref`, the avatar store, the client) refer to a
//! character by an id such as `"ff7/cloud"`; this module says which `char.lgp` skeleton and which
//! idle, walk and run animations that id means ([`resolve_appearance`], [`playable_field_models`]).
//! The ids are **stable**: they are persisted in avatar profiles, so an id is never renamed or
//! reused for another character.
//!
//! **Derived from the owned FF7 Steam Edition (en) on 2026-11-08 and verified there by the
//! ignored test `playable_models_match_the_field_loaders` in `tests/real_data.rs`:** the
//! model-loader section ([`crate::field::ModelLoader`]) of every one of the 702 fields lists the
//! models the field uses, each with its skeleton and animations. For each character below the
//! table's skeleton is the one the loader records of the main party model use, and its idle, walk
//! and run animations are the first three animations of those records:
//!
//! | id | name | skeleton | idle | walk | run | records with this skeleton, of them opening idle, walk, run | frames idle / walk / run |
//! |---|---|---|---|---|---|---|---|
//! | `ff7/cloud` | Cloud Strife | `aaaa` (`n_cloud_sk`) | `acfe` | `aaff` | `aaga` | 728, 610 (115 of the other 118 open with `aafe`, the generic standing clip, instead of `acfe`) | 1 / 30 / 15 |
//! | `ff7/tifa` | Tifa Lockhart | `aagb` (`n_tifa_sk`) | `abcd` | `abce` | `abcf` | 529, 529 | 1 / 23 / 15 |
//! | `ff7/barret` | Barret Wallace | `acgd` (`sd_ballet_sk`) | `adcb` | `adcc` | `adcd` | 315, 310 | 1 / 30 / 15 |
//! | `ff7/aerith` | Aerith Gainsborough | `auff` (`n_earith_sk`) | `avbf` | `avca` | `avcb` | 220, 219 | 2 / 20 / 15 |
//! | `ff7/red_xiii` | Red XIII | `adda` | `aeae` | `aeaf` | `aeba` | 251, 235 | 1 / 20 / 15 |
//! | `ff7/cid` | Cid Highwind | `abda` | `abie` | `abif` | `abja` | 472, 472 | 1 / 30 / 15 |
//! | `ff7/yuffie` | Yuffie Kisaragi | `abjb` | `acfb` | `acfc` | `acfd` | 242, 242 | 2 / 25 / 14 |
//! | `ff7/vincent` | Vincent Valentine | `aehd` | `afdf` | `afea` | `afeb` | 197, 194 | 2 / 30 / 15 |
//!
//! (Aerith's loader records are named `earith`, Barret's `ballet`, as in the original data.) The
//! counts in the table are asserted by that test, which also checks that all three animations of
//! every row fit the skeleton (equal bone counts), that every skeleton and animation named by any
//! of the 5 454 loader records exists in `char.lgp`, and that the idle clip has at most 4 frames
//! and the walk and run clips at least 8. Skeleton names, loader counts and the
//! first-three-animations rule were measured, not assumed. What stays **unverified** is the
//! *meaning* of the animation slots: that slot 0 is a standing pose, 1 the walk and 2 the run is
//! the convention of every loader record. It was confirmed by looking at the posed models
//! (`examples/model_dump.rs`): the idle pose of all eight characters from the front, side and back
//! (each reads as the character: Cloud, Tifa, Barret, Aerith, Red XIII, Cid, Yuffie, Vincent) and
//! eight frames of the walk of Cloud and Aerith; and for Cloud `cloud_poses_are_physically_plausible`
//! asserts that the walk keeps the planted foot on the ground through its 30 frames while the hip
//! bobs by about 0.8 units and that the 15-frame run leaves it. The walk and run slots of the
//! other characters rest on the loader order and on all three clips fitting the skeleton. Cait Sith
//! has no standard party model in the owned loaders (his records are named after the field) and is
//! not listed.
//!
//! Alternative skeletons exist for the same characters (Cloud in other clothes, Tifa as a
//! child, ...); they are not playable appearances here.

use crate::anim::FieldAnimation;
use crate::lgp::LgpArchive;
use crate::model::{load_field_animation, load_field_model, FieldModel, ModelError};
use fmv_core::AnimState;
use std::io::{Read, Seek};

/// One playable character appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayableModel {
    /// Stable logical id, `ff7/<character>`, lower case. Persisted in avatar profiles.
    pub appearance_ref: &'static str,
    /// Display name for UIs.
    pub display_name: &'static str,
    /// `char.lgp` skeleton entry stem: load `<hrc>.hrc` ([`crate::model::load_field_model`]).
    pub hrc: &'static str,
    /// Standing animation stem (`<idle_anim>.a`); a 1-frame pose.
    pub idle_anim: &'static str,
    /// Walking animation stem; a loop (30 frames for Cloud).
    pub walk_anim: &'static str,
    /// Running animation stem; a loop (15 frames for Cloud).
    pub run_anim: &'static str,
}

impl PlayableModel {
    /// The animation stem (`<stem>.a` in `char.lgp`) that shows `state`: the idle, walk or run clip.
    pub fn animation_for(&self, state: AnimState) -> &'static str {
        match state {
            AnimState::Idle => self.idle_anim,
            AnimState::Walk => self.walk_anim,
            AnimState::Run => self.run_anim,
        }
    }
}

/// The appearance used when an avatar has none, or one this table does not know.
pub const DEFAULT_APPEARANCE: &str = "ff7/cloud";

/// A playable character ready to draw: its model and its three animations, loaded from `char.lgp`
/// (see [`load_playable`]).
#[derive(Debug, Clone)]
pub struct Character {
    pub appearance: PlayableModel,
    pub model: FieldModel,
    pub idle: FieldAnimation,
    pub walk: FieldAnimation,
    pub run: FieldAnimation,
}

impl Character {
    /// The clip that shows `state`.
    pub fn animation(&self, state: AnimState) -> &FieldAnimation {
        match state {
            AnimState::Idle => &self.idle,
            AnimState::Walk => &self.walk,
            AnimState::Run => &self.run,
        }
    }

    /// Per-bone matrices for frame `frame` of the clip for `state` (see
    /// [`crate::anim::FieldAnimation::pose`]; frames loop). `None` only if the model and clip do
    /// not fit, which [`load_playable`] has ruled out.
    pub fn pose(&self, state: AnimState, frame: usize) -> Option<Vec<[f32; 16]>> {
        self.model.pose(self.animation(state), frame)
    }
}

/// Load a playable character: its skeleton with meshes and textures, and its idle, walk and run
/// clips, from `char.lgp`. An error if any piece is missing or malformed, or a clip does not fit
/// the skeleton (it has a different bone count).
pub fn load_playable<R: Read + Seek>(lgp: &mut LgpArchive<R>, appearance: &PlayableModel) -> Result<Character, ModelError> {
    let model = load_field_model(lgp, appearance.hrc)?;
    let mut clip = |stem: &str| -> Result<FieldAnimation, ModelError> {
        let animation = load_field_animation(lgp, stem)?;
        if model.fits(&animation) {
            Ok(animation)
        } else {
            Err(ModelError::Invalid {
                what: "playable model",
                why: format!(
                    "animation {stem} drives {} bones but skeleton {} has {}",
                    animation.bone_count(),
                    appearance.hrc,
                    model.bones.len()
                ),
            })
        }
    };
    let (idle, walk, run) = (clip(appearance.idle_anim)?, clip(appearance.walk_anim)?, clip(appearance.run_anim)?);
    Ok(Character { appearance: *appearance, model, idle, walk, run })
}

macro_rules! playable {
    ($ref:literal, $name:literal, $hrc:literal, $idle:literal, $walk:literal, $run:literal) => {
        PlayableModel {
            appearance_ref: $ref,
            display_name: $name,
            hrc: $hrc,
            idle_anim: $idle,
            walk_anim: $walk,
            run_anim: $run,
        }
    };
}

/// Every playable appearance, in presentation order (the four main characters of the first
/// milestone first). See the module docs for where each row comes from.
pub const PLAYABLE_FIELD_MODELS: &[PlayableModel] = &[
    playable!("ff7/cloud", "Cloud Strife", "aaaa", "acfe", "aaff", "aaga"),
    playable!("ff7/tifa", "Tifa Lockhart", "aagb", "abcd", "abce", "abcf"),
    playable!("ff7/barret", "Barret Wallace", "acgd", "adcb", "adcc", "adcd"),
    playable!("ff7/aerith", "Aerith Gainsborough", "auff", "avbf", "avca", "avcb"),
    playable!("ff7/red_xiii", "Red XIII", "adda", "aeae", "aeaf", "aeba"),
    playable!("ff7/cid", "Cid Highwind", "abda", "abie", "abif", "abja"),
    playable!("ff7/yuffie", "Yuffie Kisaragi", "abjb", "acfb", "acfc", "acfd"),
    playable!("ff7/vincent", "Vincent Valentine", "aehd", "afdf", "afea", "afeb"),
];

/// [`PLAYABLE_FIELD_MODELS`] as an owned list.
pub fn playable_field_models() -> Vec<PlayableModel> {
    PLAYABLE_FIELD_MODELS.to_vec()
}

/// The appearance named `appearance_ref` (ASCII case-insensitive, surrounding whitespace
/// ignored), or `None` if this table has no such id; callers fall back to
/// [`DEFAULT_APPEARANCE`].
pub fn resolve_appearance(appearance_ref: &str) -> Option<PlayableModel> {
    let wanted = appearance_ref.trim();
    PLAYABLE_FIELD_MODELS.iter().find(|m| m.appearance_ref.eq_ignore_ascii_case(wanted)).copied()
}

/// [`resolve_appearance`] with the fallback to [`DEFAULT_APPEARANCE`] applied: always a model.
pub fn resolve_appearance_or_default(appearance_ref: Option<&str>) -> PlayableModel {
    appearance_ref.and_then(resolve_appearance).or_else(|| resolve_appearance(DEFAULT_APPEARANCE)).unwrap_or(
        PlayableModel {
            appearance_ref: DEFAULT_APPEARANCE,
            display_name: "Cloud Strife",
            hrc: "aaaa",
            idle_anim: "acfe",
            walk_anim: "aaff",
            run_anim: "aaga",
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_four_main_characters_come_first_with_stable_ids() {
        let table = playable_field_models();
        let ids: Vec<&str> = table.iter().take(4).map(|m| m.appearance_ref).collect();
        assert_eq!(ids, ["ff7/cloud", "ff7/tifa", "ff7/barret", "ff7/aerith"]);
        assert_eq!(table.len(), PLAYABLE_FIELD_MODELS.len());
        let cloud = resolve_appearance("ff7/cloud").unwrap();
        assert_eq!((cloud.hrc, cloud.idle_anim, cloud.walk_anim, cloud.run_anim), ("aaaa", "acfe", "aaff", "aaga"));
        assert_eq!(cloud.display_name, "Cloud Strife");
    }

    #[test]
    fn ids_and_entries_are_well_formed_and_unique() {
        let mut ids = HashSet::new();
        let mut skeletons = HashSet::new();
        for m in PLAYABLE_FIELD_MODELS {
            assert!(ids.insert(m.appearance_ref), "duplicate id {}", m.appearance_ref);
            assert!(skeletons.insert(m.hrc), "two appearances share skeleton {}", m.hrc);
            let rest = m
                .appearance_ref
                .strip_prefix("ff7/")
                .unwrap_or_else(|| panic!("{} is not under ff7/", m.appearance_ref));
            assert!(
                !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{}",
                m.appearance_ref
            );
            assert!(m.appearance_ref.len() <= 64, "must fit the server's appearance_ref limit");
            for stem in [m.hrc, m.idle_anim, m.walk_anim, m.run_anim] {
                assert_eq!(stem.len(), 4, "{stem}");
                assert!(
                    stem.bytes().all(|b| b.is_ascii_lowercase()),
                    "{stem}: archive stems here are lower case, no extension"
                );
            }
            assert!(!m.display_name.is_empty());
            // the idle, walk and run animations are three different clips
            assert!(
                m.idle_anim != m.walk_anim && m.walk_anim != m.run_anim && m.idle_anim != m.run_anim,
                "{}",
                m.appearance_ref
            );
        }
    }

    #[test]
    fn animation_states_map_to_the_three_clips() {
        let cloud = resolve_appearance("ff7/cloud").unwrap();
        assert_eq!(cloud.animation_for(AnimState::Idle), "acfe");
        assert_eq!(cloud.animation_for(AnimState::Walk), "aaff");
        assert_eq!(cloud.animation_for(AnimState::Run), "aaga");
    }

    #[test]
    fn a_playable_character_loads_with_its_three_clips_and_rejects_ill_fitting_ones() {
        use crate::model::tests::synthetic_char_lgp;
        use std::io::Cursor;
        let test_model = |idle: &'static str, run: &'static str| PlayableModel {
            appearance_ref: "test/model",
            display_name: "Test",
            hrc: "test",
            idle_anim: idle,
            walk_anim: "walk",
            run_anim: run,
        };
        let mut lgp = LgpArchive::open(Cursor::new(synthetic_char_lgp())).unwrap();
        let character = load_playable(&mut lgp, &test_model("walk", "walk")).unwrap();
        assert_eq!(character.model.bones.len(), 2);
        assert_eq!(character.appearance.hrc, "test");
        for state in [AnimState::Idle, AnimState::Walk, AnimState::Run] {
            assert_eq!(character.animation(state).bone_count(), 2);
            let pose = character.pose(state, 3).unwrap();
            assert_eq!(pose.len(), 2);
        }
        assert!(format!("{character:?}").contains("Character"));
        // a clip with the wrong bone count is refused, naming it
        let err = load_playable(&mut lgp, &test_model("walk", "odd")).unwrap_err();
        assert!(matches!(&err, ModelError::Invalid { why, .. } if why.contains("odd") && why.contains("3 bones")), "{err}");
        // missing pieces are errors, not panics
        assert!(matches!(load_playable(&mut lgp, &test_model("nope", "walk")), Err(ModelError::Missing { .. })));
        let unknown_skeleton = PlayableModel { hrc: "none", ..test_model("walk", "walk") };
        assert!(matches!(load_playable(&mut lgp, &unknown_skeleton), Err(ModelError::Missing { .. })));
    }

    #[test]
    fn resolving_is_case_insensitive_and_unknown_ids_fall_back() {
        assert_eq!(resolve_appearance("FF7/Tifa").map(|m| m.hrc), Some("aagb"));
        assert_eq!(resolve_appearance("  ff7/barret ").map(|m| m.hrc), Some("acgd"));
        for unknown in ["", "ff7", "ff7/", "ff7/nobody", "cloud", "ff8/squall", "test-model", "ff7/cloud/2"] {
            assert_eq!(resolve_appearance(unknown), None, "{unknown:?}");
        }
        assert_eq!(resolve_appearance_or_default(None).appearance_ref, DEFAULT_APPEARANCE);
        assert_eq!(resolve_appearance_or_default(Some("nope")).appearance_ref, DEFAULT_APPEARANCE);
        assert_eq!(resolve_appearance_or_default(Some("ff7/aerith")).hrc, "auff");
        assert!(resolve_appearance(DEFAULT_APPEARANCE).is_some(), "the default must exist in the table");
    }
}
