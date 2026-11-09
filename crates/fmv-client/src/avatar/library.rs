//! The characters this window can draw: each appearance is loaded once, merged into a drawable
//! mesh and kept.
//!
//! An avatar names how the game draws it by an `appearance_ref` (its binding: `ff7/body/sturdy` for a
//! new character's generic placeholder body, `ff7/tifa` for the canon look of an account that predates
//! protocol v8) that comes from the network, so it is untrusted text: it is only ever *looked up* in the
//! table of playable models, the canon characters AND the placeholder bodies
//! ([`fmv_ff7::playable::resolve_model`]); what is cached is keyed by the table's own id, never by the raw
//! string, so no stream of odd refs can grow the cache beyond the two tables' size. An avatar without an
//! appearance, or with one the tables do not know, is drawn as the default character
//! ([`fmv_ff7::DEFAULT_APPEARANCE`]) and flagged as a stand-in, which the renderer marks with the
//! player's tint so such players can still be told apart. A placeholder body is told from a canon model by
//! [`is_placeholder_body`]: the renderer tints a body with its player's colour and leaves a canon model its own.
//!
//! A character that fails to load (a damaged archive) is remembered as failed and reported once, so
//! the frame loop never retries it: those players are drawn as markers.

use super::mesh::MergedMesh;
use super::source::CharacterLoader;
use fmv_ff7::model::mat4_identity;
use fmv_ff7::playable::{resolve_model, Character, PlayableModel, PLACEHOLDER_BODIES, PLAYABLE_FIELD_MODELS};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_ASSET_ID: AtomicU64 = AtomicU64::new(1);

pub use crate::state::MAX_APPEARANCE_REF;

/// A loaded character: its decoded data and the merged mesh that is drawn.
pub struct AvatarAsset {
    /// Distinguishes loaded characters, so the GPU copy is made once per character.
    pub id: u64,
    pub appearance: PlayableModel,
    pub character: Character,
    pub mesh: MergedMesh,
    /// How far the top of the head is above the ground in the idle pose, in model units: where a
    /// name label goes.
    pub height_units: f32,
}

impl std::fmt::Debug for AvatarAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarAsset")
            .field("id", &self.id)
            .field("appearance", &self.appearance.appearance_ref)
            .field("mesh", &self.mesh)
            .field("height_units", &self.height_units)
            .finish()
    }
}

impl AvatarAsset {
    /// Merge a loaded character into a drawable asset.
    pub fn new(character: Character) -> Result<Self, String> {
        let mesh = MergedMesh::build(&character.model).map_err(|e| e.to_string())?;
        // The idle pose's highest point (native y points down, so the head top is the minimum y).
        let height_units = character
            .pose(fmv_core::AnimState::Idle, 0)
            .and_then(|pose| character.model.posed_bounds(&pose, &mat4_identity()))
            .map_or(0.0, |(lo, _)| (-lo[1]).max(0.0));
        let height_units = if height_units.is_finite() && height_units > 0.5 { height_units } else { DEFAULT_HEIGHT_UNITS };
        Ok(Self {
            id: NEXT_ASSET_ID.fetch_add(1, Ordering::Relaxed),
            appearance: character.appearance,
            character,
            mesh,
            height_units,
        })
    }
}

/// Head height assumed when a model's own cannot be measured (Cloud, the tallest, is 33.2).
pub const DEFAULT_HEIGHT_UNITS: f32 = 30.0;

/// What [`AvatarLibrary::get`] found for an avatar.
#[derive(Debug, Clone)]
pub struct Appearance {
    /// The table id the avatar resolved to (the default's when it named nothing usable).
    #[cfg_attr(not(test), allow(dead_code))]
    pub id: &'static str,
    /// The avatar named no known appearance, so the default character stands in for it.
    pub stand_in: bool,
    /// The drawable character; `None` when it could not be loaded (draw a marker instead).
    pub asset: Option<Arc<AvatarAsset>>,
}

enum Slot {
    Loaded(Arc<AvatarAsset>),
    /// Reported once; never retried.
    Failed,
}

/// What identifies a loaded non-player model: its skeleton stem and standing clip, lower case.
type NpcKey = (String, String);

/// Most placeholder bodies the players can wear at once (the table of bodies holds up to this many: a test
/// fails when it grows past it, so the cache arithmetic below is never silently wrong).
pub const MAX_BODY_MODELS: usize = 4;

/// Most player models a window holds: every canon character ([`PLAYABLE_FIELD_MODELS`]) and every placeholder
/// body ([`MAX_BODY_MODELS`]). The player table is bounded by this whatever the network says.
pub const MAX_PLAYER_MODELS: usize = PLAYABLE_FIELD_MODELS.len() + MAX_BODY_MODELS;

/// Most distinct non-player models the library holds at once, loaded or failed. The references
/// come from the server, which can name a new model in every area, so the cache is bounded; the
/// GPU keeps at most 32 characters ([`super::gpu::MAX_GPU_MODELS`]), which is why this is what is left of
/// them once the players have theirs: [`MAX_PLAYER_MODELS`] (the 8 canon characters and up to 4 placeholder
/// bodies), so 32 - 12 = 20. Beyond it a model is not loaded (its NPCs are markers) until
/// [`AvatarLibrary::keep_npcs`] has dropped those of the area that was left.
pub const MAX_NPC_MODELS: usize = super::gpu::MAX_GPU_MODELS - MAX_PLAYER_MODELS;

/// A model or clip reference worth looking up: a stem of the archive's entry names (letters,
/// digits and underscores) within the length the protocol allows. Anything else cannot name an
/// entry and is not looked up at all.
fn plausible_stem(text: &str) -> bool {
    !text.is_empty() && text.len() <= fmv_protocol::NPC_REF_CHARS && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The cache key of a non-player model, or `None` when its references cannot be looked up.
fn npc_key(model: &str, idle: Option<&str>) -> Option<NpcKey> {
    let idle = idle?;
    (plausible_stem(model) && plausible_stem(idle)).then(|| (model.to_ascii_lowercase(), idle.to_ascii_lowercase()))
}

/// The cache of loaded characters.
pub struct AvatarLibrary {
    loader: Box<dyn CharacterLoader>,
    slots: HashMap<&'static str, Slot>,
    /// The non-player models, by what the area's data names. Bounded by [`MAX_NPC_MODELS`].
    npc_slots: HashMap<NpcKey, Slot>,
    /// The cap was said once already (so a crowded field does not repeat it every frame).
    npc_cap_reported: bool,
}

impl AvatarLibrary {
    pub fn new(loader: Box<dyn CharacterLoader>) -> Self {
        Self { loader, slots: HashMap::new(), npc_slots: HashMap::new(), npc_cap_reported: false }
    }

    /// The table entry `appearance_ref` means (a canon character's model or a placeholder body's), and
    /// whether the default had to stand in.
    pub fn resolve(appearance_ref: Option<&str>) -> (PlayableModel, bool) {
        match appearance_ref.filter(|r| r.len() <= MAX_APPEARANCE_REF).and_then(resolve_model) {
            Some(model) => (model, false),
            None => (fmv_ff7::playable::resolve_appearance_or_default(None), true),
        }
    }

    /// The character for `appearance_ref`: loaded on first use, then served from the cache (a
    /// failure is cached too).
    pub fn get(&mut self, appearance_ref: Option<&str>) -> Appearance {
        let (model, stand_in) = Self::resolve(appearance_ref);
        let id = model.appearance_ref;
        if !self.slots.contains_key(id) {
            let slot = match self.loader.load(&model).and_then(AvatarAsset::new) {
                Ok(asset) => Slot::Loaded(Arc::new(asset)),
                Err(why) => {
                    eprintln!("avatar: cannot load {id} ({why}); its players are drawn as markers");
                    Slot::Failed
                }
            };
            self.slots.insert(id, slot);
        }
        let asset = match self.slots.get(id) {
            Some(Slot::Loaded(asset)) => Some(Arc::clone(asset)),
            _ => None,
        };
        Appearance { id, stand_in, asset }
    }

    /// The model of a non-player character: `model` is its skeleton stem, `idle` the stem of the
    /// clip that shows it standing (both untrusted, see [`plausible_stem`]). Loaded on first use,
    /// then served from the cache (a failure is cached too). `None` is the answer for every
    /// case in which the NPC is drawn as a marker instead: references that cannot name archive
    /// entries, no standing clip named, a model or clip that does not load or does not fit, or the
    /// area already uses [`MAX_NPC_MODELS`] other models. Never a stand-in character: a field full
    /// of Clouds is worse than markers.
    pub fn get_npc(&mut self, model: &str, idle: Option<&str>) -> Option<Arc<AvatarAsset>> {
        let key = npc_key(model, idle)?;
        if !self.npc_slots.contains_key(&key) {
            if self.npc_slots.len() >= MAX_NPC_MODELS {
                if !self.npc_cap_reported {
                    self.npc_cap_reported = true;
                    eprintln!("avatar: this area names more than {MAX_NPC_MODELS} different non-player models; the others are drawn as markers");
                }
                return None;
            }
            let slot = match self.loader.load_npc(&key.0, &key.1).and_then(AvatarAsset::new) {
                Ok(asset) => Slot::Loaded(Arc::new(asset)),
                Err(why) => {
                    // The stems are plain ASCII (checked above), so they are safe to print.
                    eprintln!("avatar: cannot load the non-player model {} with clip {} ({why}); its characters are drawn as markers", key.0, key.1);
                    Slot::Failed
                }
            };
            self.npc_slots.insert(key.clone(), slot);
        }
        match self.npc_slots.get(&key) {
            Some(Slot::Loaded(asset)) => Some(Arc::clone(asset)),
            _ => None,
        }
    }

    /// The area changed (or its characters did): `models` are the non-player models its characters
    /// name, as (skeleton, standing clip). What no longer appears there is dropped (so the cap
    /// counts only the area being shown) and what does is loaded now, so that the first frame that
    /// shows them does not stall.
    pub fn keep_npcs<'a>(&mut self, models: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) {
        let wanted: Vec<NpcKey> = models.into_iter().filter_map(|(model, idle)| npc_key(model, idle)).collect();
        self.npc_slots.retain(|key, _| wanted.contains(key));
        self.npc_cap_reported = false;
        for (model, idle) in &wanted {
            self.get_npc(model, Some(idle));
        }
    }

    /// Characters currently held (loaded or failed): never more than the table has.
    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.slots.len()
    }

    /// Non-player models currently held (loaded or failed): never more than [`MAX_NPC_MODELS`].
    #[cfg(test)]
    pub fn cached_npcs(&self) -> usize {
        self.npc_slots.len()
    }
}

/// Is `model` one of the generic placeholder bodies a new character is drawn with (a model of
/// [`PLACEHOLDER_BODIES`]), as opposed to a canon character's? Only a body is tinted with its player's colour.
pub fn is_placeholder_body(model: &PlayableModel) -> bool {
    PLACEHOLDER_BODIES.iter().any(|body| body.model.appearance_ref == model.appearance_ref)
}

/// The default appearance's id.
#[cfg(test)]
pub fn default_id() -> &'static str {
    fmv_ff7::playable::DEFAULT_APPEARANCE
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::avatar::mesh::tests::{model, part};
    use fmv_core::AnimState;
    use fmv_ff7::FieldAnimation;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A `.a` file with `frames` frames for `bones` bones whose angles are all zero and whose root
    /// is `height` above the ground.
    pub(crate) fn clip(frames: usize, bones: usize, height: f32) -> FieldAnimation {
        let mut out = Vec::new();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(frames as u32).to_le_bytes());
        out.extend_from_slice(&(bones as u32).to_le_bytes());
        out.extend_from_slice(&[1, 0, 2, 0]);
        out.extend(std::iter::repeat_n(0u8, 20));
        for _ in 0..frames {
            for v in [0.0f32, 0.0, 0.0, 0.0, height, 0.0] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for _ in 0..bones {
                for v in [0.0f32; 3] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        FieldAnimation::parse(&out).expect("a valid synthetic clip")
    }

    /// A stick figure with the given clip lengths: one root bone whose clips are all zero angles
    /// with the root 5 units above the ground (native y points down, so a vertex at local y = 5
    /// stands on the ground). Its body is a tall triangle 30 units high (feet at local y = 5, head
    /// top at -25) and a small "nose" triangle at head height that points forward (native -z).
    pub(crate) fn character(appearance: PlayableModel, idle: usize, walk: usize, run: usize) -> Character {
        let mut body = part(0, &[[0, 1, 2]], 3);
        body.positions = vec![[-3.0, 5.0, 0.0], [3.0, 5.0, 0.0], [0.0, -25.0, 0.0]];
        let mut nose = part(0, &[[0, 1, 2]], 3);
        nose.positions = vec![[0.0, -22.0, -1.0], [1.0, -22.0, -4.0], [-1.0, -22.0, -4.0]];
        let mut stick = model(vec![body, nose]);
        stick.bones.truncate(1);
        Character { appearance, model: stick, idle: clip(idle, 1, 5.0), walk: clip(walk, 1, 5.0), run: clip(run, 1, 5.0) }
    }

    /// A stand-in loader that counts what it is asked for and can be told to fail.
    pub(crate) struct Counting {
        pub asked: Rc<RefCell<Vec<&'static str>>>,
        pub fail: Vec<&'static str>,
        /// The non-player models asked for, as `model/clip`.
        pub asked_npc: Rc<RefCell<Vec<String>>>,
        /// Non-player skeleton stems whose load fails.
        pub fail_npc: Vec<&'static str>,
    }

    impl Counting {
        pub(crate) fn new(asked: Rc<RefCell<Vec<&'static str>>>, fail: Vec<&'static str>) -> Self {
            Self { asked, fail, asked_npc: Rc::default(), fail_npc: Vec::new() }
        }
    }

    impl CharacterLoader for Counting {
        fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
            self.asked.borrow_mut().push(appearance.appearance_ref);
            if self.fail.contains(&appearance.appearance_ref) {
                return Err("synthetic failure".into());
            }
            Ok(character(*appearance, 1, 4, 3))
        }

        fn load_npc(&mut self, model: &str, idle: &str) -> Result<Character, String> {
            self.asked_npc.borrow_mut().push(format!("{model}/{idle}"));
            if self.fail_npc.contains(&model) {
                return Err("synthetic failure".into());
            }
            Ok(character(crate::avatar::source::NON_PLAYABLE, 1, 1, 1))
        }
    }

    fn library(fail: Vec<&'static str>) -> (AvatarLibrary, Rc<RefCell<Vec<&'static str>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        (AvatarLibrary::new(Box::new(Counting::new(Rc::clone(&asked), fail))), asked)
    }

    /// A library over a [`Counting`] loader that fails for the NPC skeletons `fail_npc`, and the
    /// list of the NPC models it was asked for.
    fn npc_library(fail_npc: Vec<&'static str>) -> (AvatarLibrary, Rc<RefCell<Vec<String>>>) {
        let mut loader = Counting::new(Rc::default(), vec![]);
        loader.fail_npc = fail_npc;
        let asked = Rc::clone(&loader.asked_npc);
        (AvatarLibrary::new(Box::new(loader)), asked)
    }

    #[test]
    fn each_appearance_is_loaded_once_and_served_from_the_cache() {
        let (mut lib, asked) = library(vec![]);
        let tifa = lib.get(Some("ff7/tifa"));
        assert_eq!((tifa.id, tifa.stand_in), ("ff7/tifa", false));
        let again = lib.get(Some("ff7/tifa"));
        assert!(Arc::ptr_eq(tifa.asset.as_ref().unwrap(), again.asset.as_ref().unwrap()), "the same asset every time");
        // Spelling does not matter to the table, so it does not create a second entry.
        let loud = lib.get(Some("  FF7/Tifa "));
        assert!(Arc::ptr_eq(tifa.asset.as_ref().unwrap(), loud.asset.as_ref().unwrap()));
        lib.get(Some("ff7/barret"));
        lib.get(Some("ff7/barret"));
        assert_eq!(*asked.borrow(), ["ff7/tifa", "ff7/barret"]);
        assert_eq!(lib.cached(), 2);
        // Every asset has its own identity for the GPU cache.
        let barret = lib.get(Some("ff7/barret"));
        assert_ne!(tifa.asset.unwrap().id, barret.asset.unwrap().id);
    }

    #[test]
    fn an_unknown_or_missing_appearance_is_the_default_character_flagged_as_a_stand_in() {
        let (mut lib, asked) = library(vec![]);
        for refs in [None, Some(""), Some("ff7/nobody"), Some("ff8/squall"), Some("<script>")] {
            let a = lib.get(refs);
            assert_eq!((a.id, a.stand_in), (default_id(), true), "{refs:?}");
            assert!(a.asset.is_some());
        }
        // A known one is not a stand-in, including the default itself.
        let cloud = lib.get(Some("ff7/cloud"));
        assert_eq!((cloud.id, cloud.stand_in), ("ff7/cloud", false));
        // All of those are one cached character.
        assert_eq!((*asked.borrow()).len(), 1);
        assert_eq!(lib.cached(), 1);
        assert_eq!(default_id(), "ff7/cloud");
    }

    #[test]
    fn hostile_refs_cannot_grow_the_cache_past_the_table() {
        let (mut lib, asked) = library(vec![]);
        for i in 0..500 {
            lib.get(Some(&format!("ff7/{i}")));
            lib.get(Some(&"x".repeat(MAX_APPEARANCE_REF + 1 + i)));
        }
        assert_eq!(lib.cached(), 1, "everything unknown shares the default's single entry");
        assert_eq!(asked.borrow().len(), 1);
        for model in fmv_ff7::playable_field_models() {
            lib.get(Some(model.appearance_ref));
        }
        assert_eq!(lib.cached(), fmv_ff7::playable_field_models().len());
        // The placeholder bodies are in the table too, and only they: an id that merely looks like one is unknown.
        for body in PLACEHOLDER_BODIES {
            lib.get(Some(body.model.appearance_ref));
        }
        assert_eq!(lib.cached(), PLAYABLE_FIELD_MODELS.len() + PLACEHOLDER_BODIES.len());
        for i in 0..200 {
            lib.get(Some(&format!("ff7/body/{i}")));
            lib.get(Some(&format!("ff7/body/{}{i}", PLACEHOLDER_BODIES[0].archetype)));
        }
        assert_eq!(lib.cached(), PLAYABLE_FIELD_MODELS.len() + PLACEHOLDER_BODIES.len(), "unknown bodies share the default's entry");
        // An over-long ref is unknown even if it starts like a real one.
        let long = format!("ff7/tifa{}", " ".repeat(MAX_APPEARANCE_REF));
        assert!(lib.get(Some(&long)).stand_in);
    }

    /// A placeholder body is an appearance like a canon character: found by the table's own id (any spelling),
    /// loaded once, never a stand-in. Its archetype id is the identity's body choice, not its binding: the
    /// binding is what the entity carries.
    #[test]
    fn a_placeholder_body_is_an_appearance_and_not_a_stand_in() {
        let (mut lib, asked) = library(vec![]);
        let body = PLACEHOLDER_BODIES[0];
        let got = lib.get(Some(body.model.appearance_ref));
        assert_eq!((got.id, got.stand_in), (body.model.appearance_ref, false));
        assert!(got.asset.is_some());
        let loud = lib.get(Some(&format!("  {}  ", body.model.appearance_ref.to_uppercase())));
        assert!(Arc::ptr_eq(got.asset.as_ref().unwrap(), loud.asset.as_ref().unwrap()), "one asset whatever the spelling");
        assert_eq!(*asked.borrow(), [body.model.appearance_ref], "loaded once");
        assert_eq!(lib.get(Some(body.archetype)).id, default_id(), "the archetype is not a binding");
        assert!(lib.get(Some(body.archetype)).stand_in);
        // Every body of the table resolves to itself, and to a model that is not a canon character's.
        for body in PLACEHOLDER_BODIES {
            let (model, stand_in) = AvatarLibrary::resolve(Some(body.model.appearance_ref));
            assert_eq!((model, stand_in), (body.model, false), "{}", body.archetype);
            assert!(fmv_ff7::playable::resolve_appearance(body.model.appearance_ref).is_none(), "{} is not a canon id", body.archetype);
        }
    }

    #[test]
    fn only_the_placeholder_bodies_are_bodies() {
        for body in PLACEHOLDER_BODIES {
            assert!(is_placeholder_body(&body.model), "{}", body.archetype);
        }
        for canon in PLAYABLE_FIELD_MODELS {
            assert!(!is_placeholder_body(canon), "{}", canon.appearance_ref);
        }
        // Neither a non-player model nor the default character that stands in for an unknown look.
        assert!(!is_placeholder_body(&crate::avatar::source::NON_PLAYABLE));
        assert!(!is_placeholder_body(&AvatarLibrary::resolve(None).0));
        assert!(!is_placeholder_body(&AvatarLibrary::resolve(Some("ff7/nobody")).0));
    }

    /// The cache arithmetic: the players hold the 8 canon characters and up to 4 placeholder bodies, the area's
    /// non-player characters the rest of the GPU's 32 (20). A table of bodies that outgrows its room fails here,
    /// instead of making the GPU's cache start over in the middle of a frame.
    #[test]
    fn the_players_and_the_area_fit_the_gpu_cache() {
        assert!(PLACEHOLDER_BODIES.len() <= MAX_BODY_MODELS, "the table of bodies has {} rows: raise MAX_BODY_MODELS and lower MAX_NPC_MODELS", PLACEHOLDER_BODIES.len());
        assert_eq!((PLAYABLE_FIELD_MODELS.len(), MAX_BODY_MODELS, MAX_PLAYER_MODELS, MAX_NPC_MODELS), (8, 4, 12, 20));
        assert_eq!(MAX_PLAYER_MODELS + MAX_NPC_MODELS, crate::avatar::gpu::MAX_GPU_MODELS, "the players and the area share the GPU's models exactly");
        assert_eq!(crate::avatar::gpu::MAX_GPU_MODELS, 32);
        // Everything the players can wear, at once, is within the players' share.
        let (mut lib, _) = library(vec![]);
        for model in PLAYABLE_FIELD_MODELS {
            lib.get(Some(model.appearance_ref));
        }
        for body in PLACEHOLDER_BODIES {
            lib.get(Some(body.model.appearance_ref));
        }
        assert!(lib.cached() <= MAX_PLAYER_MODELS, "{} players' models", lib.cached());
    }

    #[test]
    fn a_failed_load_is_remembered_and_never_retried() {
        let (mut lib, asked) = library(vec!["ff7/tifa"]);
        for _ in 0..5 {
            let a = lib.get(Some("ff7/tifa"));
            assert_eq!((a.id, a.stand_in, a.asset.is_none()), ("ff7/tifa", false, true));
        }
        assert_eq!(*asked.borrow(), ["ff7/tifa"], "asked once");
        // The others are unaffected.
        assert!(lib.get(Some("ff7/cloud")).asset.is_some());
    }

    #[test]
    fn a_character_the_mesh_builder_refuses_is_a_failure_too() {
        struct Bare;
        impl CharacterLoader for Bare {
            fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
                let mut c = character(*appearance, 1, 4, 3);
                c.model.parts.clear();
                Ok(c)
            }
        }
        let mut lib = AvatarLibrary::new(Box::new(Bare));
        assert!(lib.get(Some("ff7/cloud")).asset.is_none());
        assert_eq!(lib.cached(), 1);
    }

    #[test]
    fn a_non_player_model_is_loaded_once_by_its_stems_whatever_the_spelling() {
        let (mut lib, asked) = npc_library(vec![]);
        let guard = lib.get_npc("agyb", Some("acfe")).expect("loads");
        let again = lib.get_npc("agyb", Some("acfe")).expect("cached");
        assert!(Arc::ptr_eq(&guard, &again));
        assert!(Arc::ptr_eq(&guard, &lib.get_npc("AGYB", Some("ACFE")).unwrap()), "the archive does not care about case, so neither does the cache");
        // Another skeleton, or the same one with another standing clip, is another model.
        let other = lib.get_npc("aagb", Some("acfe")).unwrap();
        let other_clip = lib.get_npc("agyb", Some("aafe")).unwrap();
        assert!(!Arc::ptr_eq(&guard, &other) && !Arc::ptr_eq(&guard, &other_clip));
        assert_ne!(guard.id, other.id, "every model has its own identity for the GPU cache");
        assert_eq!(*asked.borrow(), ["agyb/acfe", "aagb/acfe", "agyb/aafe"]);
        assert_eq!(lib.cached_npcs(), 3);
        // It is a character like any other for the renderer, standing: all three clips are the one.
        assert_eq!(guard.appearance.appearance_ref, "npc");
        assert_eq!(guard.character.pose(AnimState::Idle, 0).map(|p| p.len()), Some(1));
        assert_eq!(guard.character.animation(AnimState::Run).frame_count(), 1);
        // The players' table is not touched by any of this.
        assert_eq!(lib.cached(), 0);
    }

    #[test]
    fn references_that_cannot_name_an_archive_entry_are_never_looked_up() {
        let (mut lib, asked) = npc_library(vec![]);
        let long = "a".repeat(fmv_protocol::NPC_REF_CHARS + 1);
        for (model, idle) in [
            ("", Some("acfe")),
            ("agyb", None),
            ("agyb", Some("")),
            ("../../etc/passwd", Some("acfe")),
            ("agyb\0", Some("acfe")),
            ("ag yb", Some("acfe")),
            ("agyb.hrc", Some("acfe")),
            ("agyb", Some("a/b")),
            ("\u{10FFFF}", Some("acfe")),
            (long.as_str(), Some("acfe")),
            ("agyb", Some(long.as_str())),
        ] {
            assert!(lib.get_npc(model, idle).is_none(), "{model:?} {idle:?}");
        }
        assert!(asked.borrow().is_empty(), "nothing odd reached the archive: {:?}", asked.borrow());
        assert_eq!(lib.cached_npcs(), 0, "and nothing odd is kept");
        // The longest reference the protocol allows is fine.
        assert!(lib.get_npc(&"a".repeat(fmv_protocol::NPC_REF_CHARS), Some("acfe")).is_some());
    }

    #[test]
    fn a_non_player_model_that_fails_is_remembered_and_never_retried_and_is_never_a_stand_in() {
        let (mut lib, asked) = npc_library(vec!["aagb"]);
        for _ in 0..5 {
            assert!(lib.get_npc("aagb", Some("acfe")).is_none(), "a marker, not another character");
        }
        assert_eq!(*asked.borrow(), ["aagb/acfe"], "asked once");
        assert!(lib.get_npc("agyb", Some("acfe")).is_some(), "the others are unaffected");
        assert_eq!(lib.cached(), 0, "no playable character was loaded to stand in for it");
    }

    #[test]
    fn a_hostile_stream_of_models_cannot_grow_the_cache_past_its_cap() {
        let (mut lib, asked) = npc_library(vec![]);
        for i in 0..MAX_NPC_MODELS + 40 {
            let name = format!("m{i}");
            let loaded = lib.get_npc(&name, Some("acfe")).is_some();
            assert_eq!(loaded, i < MAX_NPC_MODELS, "model {i}");
        }
        assert_eq!(lib.cached_npcs(), MAX_NPC_MODELS);
        assert_eq!(asked.borrow().len(), MAX_NPC_MODELS, "the ones over the cap were not even asked for");
        // What is held keeps working.
        assert!(lib.get_npc("m0", Some("acfe")).is_some());
        // Leaving the area drops what the next one does not name, which makes room.
        lib.keep_npcs([("agyb", Some("acfe"))]);
        assert_eq!(lib.cached_npcs(), 1);
        assert!(lib.get_npc("m99", Some("acfe")).is_some());
    }

    #[test]
    fn keeping_an_areas_models_loads_them_now_and_forgets_the_rest() {
        let (mut lib, asked) = npc_library(vec!["aagb"]);
        lib.keep_npcs([("agyb", Some("acfe")), ("agyb", Some("ACFE")), ("aagb", Some("acfe")), ("bad name", Some("acfe")), ("acgd", None)]);
        // Loaded before any frame asks, once each (the spelling does not make a second one, a
        // failure is remembered, what cannot be looked up is left out).
        assert_eq!(*asked.borrow(), ["agyb/acfe", "aagb/acfe"]);
        assert_eq!(lib.cached_npcs(), 2);
        asked.borrow_mut().clear();
        // The next area keeps one of them and brings another: the first is not loaded again, the
        // one that is gone is dropped.
        lib.keep_npcs([("agyb", Some("acfe")), ("acgd", Some("adcb"))]);
        assert_eq!(*asked.borrow(), ["acgd/adcb"]);
        assert_eq!(lib.cached_npcs(), 2);
        // An area without any characters drops them all.
        lib.keep_npcs([]);
        assert_eq!(lib.cached_npcs(), 0);
    }

    #[test]
    fn the_head_height_is_measured_from_the_idle_pose() {
        // The stick figure's head top is at local y = -25 with the root 5 above the ground: 30 up.
        let asset = AvatarAsset::new(character(resolve_model("ff7/cloud").unwrap(), 1, 4, 3)).unwrap();
        assert!((asset.height_units - 30.0).abs() < 1e-3, "{}", asset.height_units);
        assert_eq!(asset.character.pose(AnimState::Idle, 0).map(|p| p.len()), Some(1));
        assert!(format!("{asset:?}").contains("ff7/cloud"));
    }
}
