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
//!
//! # Placeholder bodies (generic, non-canon)
//!
//! A player's own character is not a cast member, so FF7 draws it with a generic human extra from
//! the same `char.lgp`: one of [`PLACEHOLDER_BODIES`], picked by the game-agnostic archetype id that
//! travels in the player's identity ([`resolve_body`], [`body_or_default`]). Their appearance refs
//! are `ff7/body/<archetype>`; [`resolve_model`] reads them next to the canon ids, while the canon
//! table above stays exactly the eight cast members and [`resolve_appearance`] knows nothing of the
//! bodies. At most four bodies are offered, because the client keeps at most 32 models in its GPU
//! cache and 24 NPC models.
//!
//! **Derived from the same owned copy on 2026-11-09 and verified by the ignored test
//! `placeholder_bodies_match_the_field_loaders` in `tests/real_data.rs`.** The survey went over all
//! 5 454 loader records (385 distinct skeletons). Candidates were the skeletons of human extras
//! (not the cast above, not named story characters, not children, animals or props) that many
//! fields use; each was drawn from the idle pose (front, side and back) and with four frames of its
//! walk and three of its run, using a throwaway copy of the rasterizer of `examples/model_dump.rs`
//! (the example itself only takes cast ids), and the three below were kept for reading as plain
//! adults with clearly different outlines. The look column is a description by eye.
//!
//! | archetype | `appearance_ref` | skeleton | idle | walk | run | records with this skeleton (fields), of them opening idle, walk, run | second clip is `aaff` | scale = field scale | frames idle / walk / run | looks like |
//! |---|---|---|---|---|---|---|---|---|---|---|
//! | `lean` | `ff7/body/lean` | `awcb` | `acfe` | `aaff` | `aaga` | 29 (15), 21 | 27 | 29 | 1 / 30 / 15 | a man in a dark grey two-piece suit with white shirt and red tie, short brown hair, white shoe tips; a narrow, upright outline |
//! | `sturdy` | `ff7/body/sturdy` | `bwab` | `acfe` | `aaff` | `aaga` | 130 (63), 87 | 109 | 127 | 1 / 30 / 15 | a soldier in a blue hooded helmet with a dark visor and grey shoulder plates, a long blue coat with a dark belt and black boots; the face is covered; the widest outline, the coat flares |
//! | `slight` | `ff7/body/slight` | `cwed` | `acfe` | `aaff` | `aaga` | 10 (10), 2 | 2 | 10 | 1 / 30 / 15 | a young woman with a brown bob under a white head covering, a mint blouse with puffed sleeves, a navy skirt, bare legs and white shoes; the narrowest outline |
//!
//! The "records" column counts loader records that use the skeleton (and the fields they are in)
//! and, of those, the ones whose first three animations are exactly the row's idle, walk and run,
//! as in the canon table; "second clip is `aaff`" counts the records whose second animation is the
//! row's walk; "scale = field scale" counts the records whose own scale equals the field's (the
//! canon test checks the same for the main four). All three rows share one clip set, the one
//! Cloud's skeleton plays too: the generic clips of the 21-bone human rig. `acfe`, `aaff`, `aaga`
//! in this order open 1 186 loader records in 594 fields on 110 skeletons, 108 of them with 21 bones
//! like the three bodies (175 more records open with `aafe`, a second standing clip, instead of
//! `acfe`). The bodies' own records agree less than the canon ones do, because scripted extras
//! often bring a clip of their own in the second or third slot, `slight` most of all (2 of its 10
//! records open with the generic clips). So the table gives the rig-wide counts as well, and what
//! vouches for the clips on every body is that all three fit its skeleton and play plausibly in
//! the renders. The run is a distinct 15-frame clip for all three bodies, so
//! [`PlaceholderBody::run_is_walk`] is false for every one of them.
//!
//! **Left out on purpose:** the three extras of the first mission's reactor fields (`bwfd`, `axdc`
//! and `bxbe`, in 20, 23 and 16 fields; they stand in `md1stin`, `md1_1` and `md1_2`) are the
//! named story characters Biggs, Jessie and Wedge; the cast's own skeletons and those of the other
//! named story characters; every child, animal and prop model; and the armed guard `azhe` (a sword
//! at the hip, 57 records in 35 fields). Of the women, `slight` was kept over `bsfc`, who is in
//! about as many fields (11) and plays the generic walk second in 9 of her 11 records, because its
//! outline differs more from the two men's (judged by eye and by a one-off comparison of rendered
//! front and side outlines that is not part of the tests) and `bsfc`'s long rigid skirt flares
//! out in the generic run. What stays **unverified** is how the bodies look in the field under a
//! player tint, and the slot order of these records (the same loader-order convention as above).

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

// ---------------------------------------------------------------------------------------
// Placeholder bodies for original characters
// ---------------------------------------------------------------------------------------

/// One generic (non-canon) human body offered to new characters as a placeholder.
///
/// See the module docs for where each body comes from and what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderBody {
    /// Game-agnostic identity choice, stable, lower case ASCII, for example `"sturdy"`. This id is
    /// stored in the player's identity and travels across games, so it is a short neutral word and
    /// never an FF7 name; an id is never renamed or reused for another body.
    pub archetype: &'static str,
    /// UI label, for example `"Sturdy"`.
    pub label: &'static str,
    /// The FF7 model: `appearance_ref` is `ff7/body/<archetype>` (at most 64 characters), and
    /// `hrc`, `idle_anim`, `walk_anim` and `run_anim` are `char.lgp` stems like the canon rows'.
    pub model: PlayableModel,
    /// True if `model.run_anim` is the same clip as `model.walk_anim` (a body without a run clip of
    /// its own). False for every body of [`PLACEHOLDER_BODIES`] today.
    pub run_is_walk: bool,
}

/// A [`PlaceholderBody`] row: the appearance ref and the display name are derived from the
/// archetype and the label, so the three cannot drift apart.
macro_rules! body {
    ($archetype:literal, $label:literal, $hrc:literal, $idle:literal, $walk:literal, $run:literal, $run_is_walk:literal) => {
        PlaceholderBody {
            archetype: $archetype,
            label: $label,
            model: PlayableModel {
                appearance_ref: concat!("ff7/body/", $archetype),
                display_name: $label,
                hrc: $hrc,
                idle_anim: $idle,
                walk_anim: $walk,
                run_anim: $run,
            },
            run_is_walk: $run_is_walk,
        }
    };
}

/// The default body: the plain man in a suit, the one that reads as the most ordinary person (a
/// visible face, no uniform). It is [`DEFAULT_BODY`] and first in [`PLACEHOLDER_BODIES`]; the
/// fallback of [`body_or_default`] is this const itself, so it works without any table lookup.
const LEAN: PlaceholderBody = body!("lean", "Lean", "awcb", "acfe", "aaff", "aaga", false);
const STURDY: PlaceholderBody = body!("sturdy", "Sturdy", "bwab", "acfe", "aaff", "aaga", false);
const SLIGHT: PlaceholderBody = body!("slight", "Slight", "cwed", "acfe", "aaff", "aaga", false);

/// Every placeholder body, in presentation order (the default first). At most four, see the
/// module docs. The rows are described, measured and verified in the module docs.
pub const PLACEHOLDER_BODIES: &[PlaceholderBody] = &[LEAN, STURDY, SLIGHT];

/// The archetype used when a character has none, or one this table does not know: the first row
/// of [`PLACEHOLDER_BODIES`].
pub const DEFAULT_BODY: &str = "lean";

/// The body whose archetype is `archetype` (ASCII case-insensitive, surrounding whitespace
/// ignored), or `None` if there is no such body; callers fall back to [`DEFAULT_BODY`] (see
/// [`body_or_default`]).
pub fn resolve_body(archetype: &str) -> Option<PlaceholderBody> {
    let wanted = archetype.trim();
    PLACEHOLDER_BODIES.iter().find(|b| b.archetype.eq_ignore_ascii_case(wanted)).copied()
}

/// The model a stored or wired appearance ref names: a canon row of [`PLAYABLE_FIELD_MODELS`]
/// first, then a [`PLACEHOLDER_BODIES`] row by its model's `appearance_ref` (ASCII
/// case-insensitive, surrounding whitespace ignored in both cases). `None` for anything else.
pub fn resolve_model(appearance_ref: &str) -> Option<PlayableModel> {
    let wanted = appearance_ref.trim();
    resolve_appearance(wanted)
        .or_else(|| PLACEHOLDER_BODIES.iter().map(|b| b.model).find(|m| m.appearance_ref.eq_ignore_ascii_case(wanted)))
}

/// The body to use for an archetype choice: [`resolve_body`], or the default body (always a body).
pub fn body_or_default(archetype: Option<&str>) -> PlaceholderBody {
    archetype.and_then(resolve_body).or_else(|| resolve_body(DEFAULT_BODY)).unwrap_or(LEAN)
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

    // ---- placeholder bodies

    #[test]
    fn placeholder_bodies_are_a_small_well_formed_table() {
        // three to four bodies: the client's GPU cache holds 32 models and its NPC cache 24
        assert!((3..=4).contains(&PLACEHOLDER_BODIES.len()), "{} bodies", PLACEHOLDER_BODIES.len());
        let mut archetypes = HashSet::new();
        let mut refs = HashSet::new();
        let mut skeletons = HashSet::new();
        let mut labels = HashSet::new();
        for b in PLACEHOLDER_BODIES {
            // the archetype is a short neutral word: lower case ASCII letters only
            assert!(archetypes.insert(b.archetype), "duplicate archetype {}", b.archetype);
            assert!(
                (1..=16).contains(&b.archetype.len()) && b.archetype.bytes().all(|c| c.is_ascii_lowercase()),
                "{:?}",
                b.archetype
            );
            // the appearance ref is derived from it, under the namespace of the game, and fits the wire limit
            assert_eq!(b.model.appearance_ref, format!("ff7/body/{}", b.archetype));
            assert!(b.model.appearance_ref.len() <= 64, "must fit the server's appearance_ref limit");
            assert!(refs.insert(b.model.appearance_ref), "duplicate ref {}", b.model.appearance_ref);
            // the label is for people: non-empty, starts with a capital, and is also the display name
            assert!(b.label.chars().next().is_some_and(|c| c.is_ascii_uppercase()), "{:?}", b.label);
            assert!(labels.insert(b.label), "duplicate label {}", b.label);
            assert_eq!(b.model.display_name, b.label);
            // char.lgp stems like the canon rows': four lower case letters, no extension
            assert!(skeletons.insert(b.model.hrc), "two bodies share skeleton {}", b.model.hrc);
            for stem in [b.model.hrc, b.model.idle_anim, b.model.walk_anim, b.model.run_anim] {
                assert_eq!(stem.len(), 4, "{stem}");
                assert!(stem.bytes().all(|c| c.is_ascii_lowercase()), "{stem}: archive stems here are lower case, no extension");
            }
            // the standing clip is its own clip; the run is the walk exactly when the flag says so
            assert!(b.model.idle_anim != b.model.walk_anim && b.model.idle_anim != b.model.run_anim, "{}", b.archetype);
            assert_eq!(b.run_is_walk, b.model.walk_anim == b.model.run_anim, "{}: run_is_walk must say whether the clips are the same", b.archetype);
            // the three animation states map to the three clips
            assert_eq!(b.model.animation_for(AnimState::Idle), b.model.idle_anim);
            assert_eq!(b.model.animation_for(AnimState::Walk), b.model.walk_anim);
            assert_eq!(b.model.animation_for(AnimState::Run), b.model.run_anim);
        }
    }

    #[test]
    fn placeholder_bodies_never_collide_with_the_canon_table() {
        // the id fragments of the canon rows: `cloud`, `red_xiii` and its pieces `red` and `xiii`, ...
        let mut canon_words = HashSet::new();
        for m in PLAYABLE_FIELD_MODELS {
            let fragment = m.appearance_ref.strip_prefix("ff7/").unwrap();
            canon_words.insert(fragment);
            canon_words.extend(fragment.split('_'));
        }
        assert!(canon_words.contains("cloud") && canon_words.contains("red_xiii") && canon_words.contains("xiii"));
        for b in PLACEHOLDER_BODIES {
            assert!(!canon_words.contains(b.archetype), "{} is a canon id fragment", b.archetype);
            for m in PLAYABLE_FIELD_MODELS {
                assert_ne!(b.model.hrc, m.hrc, "{} shares the skeleton of {}", b.archetype, m.appearance_ref);
                assert!(!b.model.appearance_ref.eq_ignore_ascii_case(m.appearance_ref), "{}", b.archetype);
                assert_ne!(b.label, m.display_name);
            }
            // the canon resolver does not know the bodies: the canon table stays the cast
            assert_eq!(resolve_appearance(b.model.appearance_ref), None, "{}", b.archetype);
        }
    }

    #[test]
    fn the_default_body_is_the_first_row_and_exists() {
        assert_eq!(PLACEHOLDER_BODIES.first().map(|b| b.archetype), Some(DEFAULT_BODY));
        let default = resolve_body(DEFAULT_BODY).expect("the default must exist in the table");
        assert_eq!(Some(&default), PLACEHOLDER_BODIES.first());
        assert_eq!(body_or_default(None), default);
        // The three rows measured in the module docs. The archetypes are persisted in player
        // identities: an id is never renamed or reused for another body, and the skeleton and
        // clips behind it never change. The order only decides the presentation and the default
        // (together with `DEFAULT_BODY` and the fallback of `body_or_default`).
        let rows: Vec<_> = PLACEHOLDER_BODIES
            .iter()
            .map(|b| (b.archetype, b.model.hrc, b.model.idle_anim, b.model.walk_anim, b.model.run_anim))
            .collect();
        assert_eq!(
            rows,
            [
                ("lean", "awcb", "acfe", "aaff", "aaga"),
                ("sturdy", "bwab", "acfe", "aaff", "aaga"),
                ("slight", "cwed", "acfe", "aaff", "aaga"),
            ]
        );
    }

    #[test]
    fn resolving_a_body_ignores_case_and_surrounding_whitespace_only() {
        for b in PLACEHOLDER_BODIES {
            assert_eq!(resolve_body(b.archetype), Some(*b));
            assert_eq!(resolve_body(&b.archetype.to_ascii_uppercase()), Some(*b));
            assert_eq!(resolve_body(&format!("  {}\t\n", b.archetype)), Some(*b));
            // a mixed-case spelling
            let mixed: String = b.archetype.chars().enumerate().map(|(i, c)| if i % 2 == 0 { c.to_ascii_uppercase() } else { c }).collect();
            assert_eq!(resolve_body(&mixed), Some(*b), "{mixed}");
        }
        assert_eq!(resolve_body("Sturdy").map(|b| b.model.hrc), Some("bwab"));
        // everything else is None: empty, near misses, the wire form, other namespaces, canon ids
        let long = "sturdy".repeat(20_000);
        for unknown in [
            "",
            " ",
            "nobody",
            "stur",
            "sturdy2",
            "sturdy sturdy",
            "stur dy",
            "ff7/body/sturdy",
            "body/sturdy",
            "ff7/cloud",
            "cloud",
            "ff7/",
            "sturdy\0",
            "\0sturdy",
            "sturdy\u{301}",
            "ſturdy",      // long s: no Unicode case folding, only ASCII
            "ｓｔｕｒｄｙ", // full-width letters
            "STURDY\u{1F600}",
            "../sturdy",
            "sturdy/../lean",
            "%73turdy",
            "'; DROP TABLE bodies; --",
            long.as_str(),
        ] {
            assert_eq!(resolve_body(unknown), None, "{:?}", unknown.chars().take(40).collect::<String>());
        }
    }

    #[test]
    fn resolving_a_model_reads_canon_rows_first_then_bodies() {
        // canon rows: the same answer as resolve_appearance
        for m in PLAYABLE_FIELD_MODELS {
            assert_eq!(resolve_model(m.appearance_ref), Some(*m));
            assert_eq!(resolve_model(m.appearance_ref), resolve_appearance(m.appearance_ref));
        }
        assert_eq!(resolve_model(" FF7/Tifa ").map(|m| m.hrc), Some("aagb"));
        // bodies, by appearance ref
        for b in PLACEHOLDER_BODIES {
            assert_eq!(resolve_model(b.model.appearance_ref), Some(b.model));
            assert_eq!(resolve_model(&b.model.appearance_ref.to_ascii_uppercase()), Some(b.model));
            assert_eq!(resolve_model(&format!(" \t{}\n ", b.model.appearance_ref)), Some(b.model));
        }
        assert_eq!(resolve_model("FF7/BODY/Sturdy ").map(|m| m.hrc), Some("bwab"));
        // a body's archetype alone is not an appearance ref, and nothing else resolves
        let long = format!("ff7/body/{}", "x".repeat(100_000));
        for unknown in [
            "",
            "ff7",
            "ff7/",
            "ff7/body",
            "ff7/body/",
            "ff7/body/nobody",
            "ff7/body/sturdy/",
            "ff7/body/sturdy/2",
            "ff7/body//sturdy",
            "ff7/body/ sturdy",
            "ff7//body/sturdy",
            "ff7\\body\\sturdy",
            "sturdy",
            "body/sturdy",
            "ff8/body/sturdy",
            "ff7/body/sturdy\0",
            "ff7/body/ſturdy",
            "ff7/body/sturdy\u{301}",
            "ff7/cloud\0",
            "ff7/cloud/2",
            "test/model",
            long.as_str(),
        ] {
            assert_eq!(resolve_model(unknown), None, "{:?}", unknown.chars().take(40).collect::<String>());
        }
    }

    #[test]
    fn body_or_default_always_gives_a_body() {
        let default = resolve_body(DEFAULT_BODY).unwrap();
        assert_eq!(body_or_default(None), default);
        for unknown in ["", "nobody", "ff7/body/sturdy", "ff7/cloud", "sturdy\0", "ſturdy"] {
            assert_eq!(body_or_default(Some(unknown)), default, "{unknown:?}");
        }
        for b in PLACEHOLDER_BODIES {
            assert_eq!(body_or_default(Some(b.archetype)), *b);
            assert_eq!(body_or_default(Some(&format!(" {} ", b.archetype.to_ascii_uppercase()))), *b);
        }
        // the body it returns is always a table row
        assert!(PLACEHOLDER_BODIES.contains(&body_or_default(Some("whatever"))));
    }

    #[test]
    fn a_placeholder_body_loads_like_a_canon_character_and_may_run_as_it_walks() {
        use crate::model::tests::synthetic_char_lgp;
        use std::io::Cursor;
        // the synthetic archive has one skeleton (`test`, 2 bones) and one 2-bone clip (`walk`): a
        // body without a run clip of its own, so its run is its walk
        let body = PlaceholderBody {
            archetype: "test",
            label: "Test",
            model: PlayableModel {
                appearance_ref: "ff7/body/test",
                display_name: "Test",
                hrc: "test",
                idle_anim: "walk",
                walk_anim: "walk",
                run_anim: "walk",
            },
            run_is_walk: true,
        };
        assert_eq!(body.run_is_walk, body.model.walk_anim == body.model.run_anim);
        let mut lgp = LgpArchive::open(Cursor::new(synthetic_char_lgp())).unwrap();
        let character = load_playable(&mut lgp, &body.model).unwrap();
        assert_eq!((character.model.bones.len(), character.appearance), (2, body.model));
        for state in [AnimState::Idle, AnimState::Walk, AnimState::Run] {
            assert!(character.pose(state, 0).is_some(), "{state:?}");
        }
        assert_eq!(character.run.frame_count(), character.walk.frame_count());
        // a body whose clip has the wrong bone count is refused like a canon one
        let odd = PlaceholderBody { model: PlayableModel { run_anim: "odd", ..body.model }, run_is_walk: false, ..body };
        assert!(matches!(load_playable(&mut lgp, &odd.model), Err(ModelError::Invalid { .. })));
        // a body naming a skeleton that does not exist is refused, not a panic
        let missing = PlayableModel { hrc: "zzzz", ..body.model };
        assert!(matches!(load_playable(&mut lgp, &missing), Err(ModelError::Missing { .. })));
    }
}
