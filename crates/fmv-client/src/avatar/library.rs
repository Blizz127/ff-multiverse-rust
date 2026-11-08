//! The characters this window can draw: each appearance is loaded once, merged into a drawable
//! mesh and kept.
//!
//! An avatar names its look by an `appearance_ref` (`ff7/tifa`) that comes from the network, so it
//! is untrusted text: it is only ever *looked up* in the table of playable characters
//! ([`fmv_ff7::resolve_appearance`]); what is cached is keyed by the table's own id, never by the
//! raw string, so no stream of odd refs can grow the cache beyond the table's size. An avatar
//! without an appearance, or with one the table does not know, is drawn as the default character
//! ([`fmv_ff7::DEFAULT_APPEARANCE`]) and flagged as a stand-in, which the renderer marks with the
//! player's tint so such players can still be told apart.
//!
//! A character that fails to load (a damaged archive) is remembered as failed and reported once, so
//! the frame loop never retries it: those players are drawn as markers.

use super::mesh::MergedMesh;
use super::source::CharacterLoader;
use fmv_ff7::model::mat4_identity;
use fmv_ff7::playable::{resolve_appearance, Character, PlayableModel};
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

/// The cache of loaded characters.
pub struct AvatarLibrary {
    loader: Box<dyn CharacterLoader>,
    slots: HashMap<&'static str, Slot>,
}

impl AvatarLibrary {
    pub fn new(loader: Box<dyn CharacterLoader>) -> Self {
        Self { loader, slots: HashMap::new() }
    }

    /// The table entry `appearance_ref` means, and whether the default had to stand in.
    pub fn resolve(appearance_ref: Option<&str>) -> (PlayableModel, bool) {
        match appearance_ref.filter(|r| r.len() <= MAX_APPEARANCE_REF).and_then(resolve_appearance) {
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

    /// Characters currently held (loaded or failed): never more than the table has.
    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.slots.len()
    }
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
    }

    impl CharacterLoader for Counting {
        fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
            self.asked.borrow_mut().push(appearance.appearance_ref);
            if self.fail.contains(&appearance.appearance_ref) {
                return Err("synthetic failure".into());
            }
            Ok(character(*appearance, 1, 4, 3))
        }
    }

    fn library(fail: Vec<&'static str>) -> (AvatarLibrary, Rc<RefCell<Vec<&'static str>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        (AvatarLibrary::new(Box::new(Counting { asked: Rc::clone(&asked), fail })), asked)
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
        // An over-long ref is unknown even if it starts like a real one.
        let long = format!("ff7/tifa{}", " ".repeat(MAX_APPEARANCE_REF));
        assert!(lib.get(Some(&long)).stand_in);
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
    fn the_head_height_is_measured_from_the_idle_pose() {
        // The stick figure's head top is at local y = -25 with the root 5 above the ground: 30 up.
        let asset = AvatarAsset::new(character(resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3)).unwrap();
        assert!((asset.height_units - 30.0).abs() < 1e-3, "{}", asset.height_units);
        assert_eq!(asset.character.pose(AnimState::Idle, 0).map(|p| p.len()), Some(1));
        assert!(format!("{asset:?}").contains("ff7/cloud"));
    }
}
