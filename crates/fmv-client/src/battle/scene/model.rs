//! Where an actor's model comes from, and the one thing the scene draws: an [`ActorRig`].
//!
//! # The seam for real battle models
//!
//! A battle actor's model is a [`BattleActorModel`]:
//!
//! * [`BattleActorModel::Field`]: a playable character's FIELD model from this machine's own
//!   `char.lgp`, through the avatar library. The stand-in for a party member until battle models are
//!   loaded.
//! * [`BattleActorModel::Battle`]: a real battle model, the skeleton and meshes of a
//!   `fmv_ff7::model::FieldModel` and its clips (`Vec<FieldAnimation>`, clip 0 the idle loop): the
//!   very types the avatar path poses and draws. This is what a battle-archive loader returns
//!   (`fmv_ff7::battle_model::BattleModel`'s `model` and `animations`), in battle units (scale 1,
//!   native `y` down, facing `-z`, so no field scale applies). A character's weapon, a second
//!   one-bone model whose clips are index-aligned with the character's, rides along as an
//!   [`Attachment`] and is drawn with the same placement at the same frame.
//! * [`BattleActorModel::Box`]: a coloured box. The stand-in for an enemy, and for anyone whose
//!   model cannot be had.
//!
//! WHICH model an actor gets is a [`BattleModelSource`]'s decision ([`StandInModels`] says Field for
//! the party and Box for the enemies, and has no location; [`ArchiveModels`] says the real battle
//! models of this machine's own `battle.lgp` and falls back to the stand-ins for whoever's it cannot
//! have). The source is handed to [`ModelKit::new`]; the window's kit is made by
//! [`ModelKit::from_local_content`] (called by [`super::scene_for`]), which uses the archive when the
//! client found one ([`remember_battle_lgp`], called where `char.lgp` is found too).
//!
//! # The battle archive
//!
//! [`ArchiveModels`] is a source over `fmv_ff7::battle_model::BattleModels`. It answers
//! [`BattleModelSource::enemy`] with `load_enemy(who.enemy_id?)` as a [`BattleActorModel::Battle`] (the
//! loader's `model` and `animations`, slot 0 being the idle loop; scale 1, native `y` down, facing
//! `-z`, as the archive's models are), [`BattleModelSource::party_member`] with `load_character(..)`
//! the same way (the character whose combat slot the member fights in, else Cloud: see
//! [`battle_character`]), both with weapon 0 as an [`Attachment`] whose clips are the
//! weapon's tracks (track `i` goes with clip `i` of its owner; enemies carry weapons too: the MP's
//! baton), and [`BattleModelSource::location`] with `load_location(..)`. Each distinct model is read
//! once, however many actors share it.
//!
//! [`Rigs`] turns a model into an [`ActorRig`] (its [`AvatarAsset`] for the GPU, its idle clip, its
//! size) once and shares it between actors that look alike, so each distinct model is uploaded
//! once however many stand in the battle, and never per frame.
//!
//! # Whose model a party member gets
//!
//! **Battle placeholder: the slot's own battle model.** A player's character has one combat SLOT (protocol
//! v8: `BattleMember::character`, a canon party member's id) and the engine fights it with that character's
//! combat template. The model drawn for it is that character's battle model, as a placeholder for the slot: a
//! player who holds Barret's slot is drawn as Barret in battle (with the player's own name over its head, the
//! roster's `BattleMember::name`, see [`crate::battle::BattleClient`]). How the player looks on the field (its
//! binding: a generic placeholder body, or the canon look of an account that predates v8) plays no part here:
//! the field look and the slot are two different things, and in battle the model is the slot's, never the
//! field look's ([`party_appearance`], [`battle_character`]).
//!
//! # Which clip plays what
//!
//! A real battle model comes with its [`ModelRole`] (a playable character's, an enemy's), the
//! slots its pack really has ([`BattleModelData::slot_ok`]) and its own animation scripts
//! ([`BattleModelData::scripts`]: what the game plays for an action); an enemy also with the attacks
//! `scene.bin` names for it ([`EnemyBook`], read from the `scene.bin` that [`remember_scene_bin`] was told
//! of, keyed by the enemy's id: each names the SCRIPT it plays). [`Rigs`] makes a [`Kit`] of them
//! ([`super::clips`] says which clip is which, verified on contact sheets) that the scene's choreography
//! reads; a stand-in or a box has none and moves procedurally. A clip is posed IN PLACE ([`ActorRig::clip_pose`]): the root's travel along the
//! ground is taken out (the weapon's track too, which is in the character's space) and the scene moves
//! the actor itself.

use super::clips::{ClipSet, Kit, Slot};
use super::effects::EffectKit;
use crate::avatar::library::{AvatarAsset, AvatarLibrary};
use crate::avatar::source::{CharacterLoader, LgpLoader, NON_PLAYABLE};
use fmv_ff7::battle_model::script::Script;
use fmv_ff7::battle_model::{BattleCharacter, BattleModel, BattleModelError, BattleModels};
use fmv_ff7::model::{mat4_mul, mat4_translate, ModelBone};
use fmv_ff7::playable::Character;
use fmv_ff7::{FieldAnimation, FieldModel, MeshPart, PartBlend};
use fmv_ff7_battle::rules::ActorId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Battle units per field-model unit for a field model standing in for a party member: Cloud's field
/// model is 33.2 units tall, so this makes him about 1 200 battle units, a bit more than half as
/// tall as a typical enemy box (see [`BoxSpec::for_enemy`]) is wide-spaced. A stand-in size chosen to
/// look right, not a measurement.
pub const FIELD_TO_BATTLE_SCALE: f32 = 36.0;
/// The playback rate assumed for battle clips (an ASSUMPTION about the original: the field's clips
/// are 30 per second, a battle model's may be half that); [`BattleModelData::fps`] overrides it.
pub const BATTLE_CLIP_FPS: f32 = 15.0;

/// A second model that moves with a battle model: a character's weapon. One bone; its clips are
/// index-aligned with the owner's (clip `i` of the weapon plays with clip `i` of the owner), and it
/// is drawn with the owner's placement at the owner's frame.
#[derive(Clone)]
pub struct Attachment {
    /// The same for the same model: it is uploaded to the GPU once per key.
    pub key: String,
    pub model: Arc<FieldModel>,
    pub clips: Arc<Vec<FieldAnimation>>,
}

impl Attachment {
    pub fn new(key: impl Into<String>, model: FieldModel, clips: Vec<FieldAnimation>) -> Self {
        Self { key: key.into(), model: Arc::new(model), clips: Arc::new(clips) }
    }
}

/// What a battle model is to the choreography: which table of slots its clips follow (see [`super::clips`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelRole {
    /// Nothing is known of its slots: it plays its idle loop and moves procedurally.
    #[default]
    Plain,
    /// A playable character's model: the shared slots of the nine characters and the blows of its own scripts.
    Character,
    /// An enemy's model: its hurt and death clips and what it plays for each attack are read from its own animation scripts
    /// ([`BattleModelData::scripts`]; the attacks are the ones [`BattleModelData::attacks`] names).
    Enemy,
}

/// The data of a battle model: what a battle-archive loader returns.
#[derive(Clone)]
pub struct BattleModelData {
    /// The same for the same model (its file stem, say): the model is uploaded to the GPU once per key.
    pub key: String,
    pub model: Arc<FieldModel>,
    /// The clips; `clips[0]` is the idle loop. Every clip must fit the skeleton.
    pub clips: Arc<Vec<FieldAnimation>>,
    /// Battle units per model unit.
    pub scale: f32,
    /// Clip playback rate, frames per second.
    pub fps: f32,
    /// Models that move with this one (a weapon).
    pub attachments: Vec<Attachment>,
    /// Which table of slots the clips follow.
    pub role: ModelRole,
    /// Whether each slot holds the slot's own clip (`false`: a stand-in the loader made); empty says
    /// nothing is known (every slot is taken as real).
    pub slot_ok: Arc<Vec<bool>>,
    /// An enemy's attacks: `scene.bin` attack id and the index of the animation script it plays ([`EnemyBook`]).
    pub attacks: Arc<Vec<(u16, u8)>>,
    /// The model's animation scripts (`BattleModel::scripts`): what a character's blows are made of
    /// ([`super::clips::Chain`]); none: they are procedural.
    pub scripts: Arc<Vec<Script>>,
}

impl BattleModelData {
    /// A model at battle scale 1 whose clips play at [`BATTLE_CLIP_FPS`], with no role: plain.
    pub fn new(key: impl Into<String>, model: FieldModel, clips: Vec<FieldAnimation>) -> Self {
        Self {
            key: key.into(),
            model: Arc::new(model),
            clips: Arc::new(clips),
            scale: 1.0,
            fps: BATTLE_CLIP_FPS,
            attachments: Vec::new(),
            role: ModelRole::Plain,
            slot_ok: Arc::new(Vec::new()),
            attacks: Arc::new(Vec::new()),
            scripts: Arc::new(Vec::new()),
        }
    }

    /// This model with that role.
    pub fn with_role(mut self, role: ModelRole) -> Self {
        self.role = role;
        self
    }

    /// This model with `attachment` riding along.
    pub fn with_attachment(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }
}

/// A coloured box standing on the ground, front towards `-z` of its own frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxSpec {
    pub width: f32,
    pub depth: f32,
    pub height: f32,
    pub rgb: [u8; 3],
}

/// The colours enemy boxes are told apart by.
const ENEMY_PALETTE: [[u8; 3]; 8] = [
    [176, 74, 74],
    [150, 96, 184],
    [76, 148, 96],
    [200, 148, 62],
    [72, 124, 180],
    [178, 92, 134],
    [118, 158, 62],
    [168, 116, 82],
];
/// The colours party boxes are told apart by (only when a character's model is not to be had).
const PARTY_PALETTE: [[u8; 3]; 4] = [[92, 132, 220], [96, 190, 214], [220, 190, 88], [150, 210, 120]];

impl BoxSpec {
    /// The box of an enemy: taller with its hit points (a small monster is the height of a person,
    /// a boss about twice that), coloured by what kind of enemy it is.
    pub fn for_enemy(kind: u64, max_hp: u32) -> Self {
        let height = (700.0 + 260.0 * (1.0 + max_hp as f32 / 60.0).log2()).clamp(800.0, 2600.0);
        let rgb = ENEMY_PALETTE[(kind % ENEMY_PALETTE.len() as u64) as usize];
        Self { width: height * 0.72, depth: height * 0.62, height, rgb }
    }

    /// The box standing in for party member `index` when the character's model is not to be had.
    pub fn for_party(index: usize) -> Self {
        let height = 1200.0;
        Self { width: 420.0, depth: 340.0, height, rgb: PARTY_PALETTE[index % PARTY_PALETTE.len()] }
    }

    fn key(&self) -> String {
        format!("box:{:.0}x{:.0}x{:.0}:{:02x}{:02x}{:02x}", self.width, self.depth, self.height, self.rgb[0], self.rgb[1], self.rgb[2])
    }
}

/// Where an actor's model comes from (see the module docs).
#[derive(Clone)]
pub enum BattleActorModel {
    /// The field model of the playable character the appearance ref names (`ff7/tifa`), looked up
    /// in the table of playable characters; anything it does not know is the default character.
    Field(Option<String>),
    /// A real battle model.
    Battle(BattleModelData),
    /// A coloured box.
    Box(BoxSpec),
}

impl BattleActorModel {
    /// A real battle model from its skeleton, meshes and clips (`clips[0]` the idle loop), at
    /// battle scale 1: for a model without weapons (the archive's source builds the
    /// [`BattleModelData`] itself, to attach the weapon).
    #[allow(dead_code)] // used by the tests
    pub fn battle(key: impl Into<String>, model: FieldModel, clips: Vec<FieldAnimation>) -> Self {
        Self::Battle(BattleModelData::new(key, model, clips))
    }
}

/// Who a party member is, for choosing its model. Only its combat SLOT counts (see the module docs, "Whose
/// model a party member gets"): how its player looks on the field is not asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyLook {
    pub actor: ActorId,
    /// The combat slot: the FF7 character id the engine fights as (`BattleMember::character`).
    pub character: u8,
    /// Our own character.
    pub mine: bool,
}

/// What an enemy is, for choosing its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnemyLook {
    pub actor: ActorId,
    /// The scene's enemy id of its formation entry, when the formation data names one.
    pub enemy_id: Option<u16>,
    pub name: String,
    pub max_hp: u32,
    pub level: u8,
}

/// Decides which model each actor of a battle gets.
pub trait BattleModelSource {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel;
    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel;

    /// The model of battle location `location` (the formation data names one), drawn behind
    /// everyone: a model whose vertices are already in battle coordinates (floor at `y = 0`, a sky far
    /// out at large negative `y`), placed as it is. `None`: the scene draws its own stand-in floor.
    fn location(&mut self, location: u16) -> Option<BattleActorModel> {
        let _ = location;
        None
    }
}

/// The appearance ref of the playable character FF7 character id `character` is (`None` for Cait
/// Sith, who has no field model in the owned archive).
fn character_appearance(character: u8) -> Option<&'static str> {
    Some(match character {
        0 => "ff7/cloud",
        1 => "ff7/barret",
        2 => "ff7/tifa",
        3 => "ff7/aerith",
        4 => "ff7/red_xiii",
        5 => "ff7/yuffie",
        7 => "ff7/vincent",
        8 => "ff7/cid",
        _ => return None,
    })
}

/// The appearance a party member's stand-in model has: the field model of the character in whose combat slot
/// it fights, else the default (Cloud) for a slot with no field model (Cait Sith's, or an id that is nobody's).
/// A battle placeholder: the slot's own model, never the player's field look.
pub fn party_appearance(character: u8) -> &'static str {
    character_appearance(character).unwrap_or(fmv_ff7::playable::DEFAULT_APPEARANCE)
}

/// A number that is the same for the same enemy: its id, else a hash of its name (so enemies the
/// data names differently look different even without an id).
fn kind_of(enemy: &EnemyLook) -> u64 {
    match enemy.enemy_id {
        Some(id) => u64::from(id),
        None => enemy.name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)),
    }
}

/// The stand-ins: the party's field models, boxes for the enemies.
#[derive(Debug, Default, Clone, Copy)]
pub struct StandInModels;

impl BattleModelSource for StandInModels {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        BattleActorModel::Field(Some(party_appearance(who.character).to_string()))
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        BattleActorModel::Box(BoxSpec::for_enemy(kind_of(who), who.max_hp))
    }
}

// ---------------------------------------------------------------------------------------------
// The battle archive
// ---------------------------------------------------------------------------------------------

static REMEMBERED_BATTLE_LGP: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remember where this machine's `battle.lgp` is (`None`: it has none, so battles are drawn with
/// stand-ins). [`open_ff7`](crate::ff7::open_ff7) calls it where it finds `char.lgp` (from the same
/// library set); a later call replaces the earlier one. Only [`ModelKit::from_local_content`] reads
/// it: the source and the tests take the location (or a stand-in) explicitly.
pub fn remember_battle_lgp(path: Option<PathBuf>) {
    if let Ok(mut slot) = REMEMBERED_BATTLE_LGP.lock() {
        *slot = path;
    }
}

/// The location [`remember_battle_lgp`] was given, if any.
fn remembered_battle_lgp() -> Option<PathBuf> {
    REMEMBERED_BATTLE_LGP.lock().ok().and_then(|slot| slot.clone())
}

static REMEMBERED_SCENE_BIN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remember where this machine's `scene.bin` is (`None`: it has none): the enemies' attacks play the
/// clips it names ([`EnemyBook`]). [`open_ff7`](crate::ff7::open_ff7) calls it next to
/// [`remember_battle_lgp`]; without it the enemies' plain attacks play no clip of their own.
pub fn remember_scene_bin(path: Option<PathBuf>) {
    if let Ok(mut slot) = REMEMBERED_SCENE_BIN.lock() {
        *slot = path;
    }
}

fn remembered_scene_bin() -> Option<PathBuf> {
    REMEMBERED_SCENE_BIN.lock().ok().and_then(|slot| slot.clone())
}

/// What `scene.bin` says about the animation of each enemy: which script each attack plays. Attack `i` of an
/// enemy record has the id `attack_ids[i]` and plays the SCRIPT `animation_indices[i]` of the enemy model's own `ab` entry
/// (script 3 is the MP's Machine Gun, which plays slots 2 to 5 of its pack in turn: the number is not a slot).
///
/// Keyed by the enemy's id, which is also its model's id, because the scene's start does not say which
/// formation it is: the ignored real-data test `book_is_the_same_in_every_scene` holds that every scene
/// that has an enemy gives it the same attacks and slots.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnemyBook {
    by_enemy: HashMap<u16, Vec<(u16, u8)>>,
}

impl EnemyBook {
    /// Every enemy of every scene of `scenes` (the first scene that has an enemy decides).
    pub fn from_scenes(scenes: &fmv_ff7_battle::scene::SceneBin) -> Self {
        let mut by_enemy: HashMap<u16, Vec<(u16, u8)>> = HashMap::new();
        for scene in scenes.scenes() {
            for (id, record) in scene.enemy_ids.iter().zip(scene.enemies.iter()) {
                let (Some(id), Some(record)) = (id, record) else { continue };
                by_enemy.entry(*id).or_insert_with(|| Self::attacks_of_record(record));
            }
        }
        Self { by_enemy }
    }

    /// The attacks of one record: the entries that have an attack id and an animation (255 is none).
    pub fn attacks_of_record(record: &fmv_ff7_battle::scene::EnemyRecord) -> Vec<(u16, u8)> {
        record.attack_ids.iter().zip(record.animation_indices.iter()).filter(|&(&id, &slot)| id != 0xFFFF && slot != 0xFF).map(|(&id, &slot)| (id, slot)).collect()
    }

    /// The attacks of enemy `enemy_id`: attack id and script index (none for an enemy the book does not have).
    pub fn attacks_of(&self, enemy_id: u16) -> Vec<(u16, u8)> {
        self.by_enemy.get(&enemy_id).cloned().unwrap_or_default()
    }

}

/// The playable character whose battle model a party member gets: the one in whose combat slot it fights
/// (FF7's ids: Cloud 0, Barret 1, Tifa 2, Aerith 3, Red XIII 4, Yuffie 5, Cait Sith 6, Vincent 7, Cid 8),
/// else Cloud for an id that is nobody's. A battle placeholder: the slot's own battle model, whatever the
/// player looks like on the field. Cait Sith, who has no field model, has a battle model.
pub fn battle_character(character: u8) -> BattleCharacter {
    match character {
        0 => BattleCharacter::Cloud,
        1 => BattleCharacter::Barret,
        2 => BattleCharacter::Tifa,
        3 => BattleCharacter::Aerith,
        4 => BattleCharacter::RedXiii,
        5 => BattleCharacter::Yuffie,
        6 => BattleCharacter::CaitSith,
        7 => BattleCharacter::Vincent,
        8 => BattleCharacter::Cid,
        _ => BattleCharacter::Cloud,
    }
}

/// The real battle models of this machine's own `battle.lgp` (see the module docs). A model that
/// cannot be had (the archive has no such enemy, the file is damaged) is the stand-in's answer for
/// that actor, reported once on stderr and never asked for again.
pub struct ArchiveModels {
    models: BattleModels,
    /// The models made so far by key (the model's prefix): `None` for one that could not be had.
    made: HashMap<String, Option<BattleModelData>>,
    /// What `scene.bin` says about the enemies' attacks (none: their plain attacks play no clip).
    book: EnemyBook,
}

impl ArchiveModels {
    /// Open `battle.lgp` (read-only).
    pub fn open(path: &Path) -> Result<Self, String> {
        Ok(Self { models: BattleModels::open(path).map_err(|e| e.to_string())?, made: HashMap::new(), book: EnemyBook::default() })
    }

    /// This source with `book`: the enemies' attacks play the clips it names.
    pub fn with_enemy_book(mut self, book: EnemyBook) -> Self {
        self.book = book;
        self
    }

    /// A loaded model as the scene's data: its clips, and weapon 0 with the weapon's tracks as an
    /// attachment (track `i` goes with clip `i`). Keyed by the model's prefix.
    fn data_of(loaded: BattleModel, role: ModelRole) -> BattleModelData {
        let key = loaded.prefix.clone();
        let mut data = BattleModelData::new(key.clone(), loaded.model, loaded.animations).with_role(role);
        data.slot_ok = Arc::new(loaded.slot_ok);
        data.scripts = Arc::new(loaded.scripts);
        if let Some(weapon) = loaded.weapons.into_iter().next() {
            if !loaded.weapon_tracks.is_empty() {
                data = data.with_attachment(Attachment::new(format!("{key}ck"), weapon, loaded.weapon_tracks));
            }
        }
        data
    }

    /// The model under `key`, made by `load` on first use (a failure is reported, with `what`
    /// naming it, and remembered).
    fn made(
        &mut self,
        key: String,
        role: ModelRole,
        what: impl FnOnce() -> String,
        load: impl FnOnce(&mut BattleModels) -> Result<BattleModel, BattleModelError>,
    ) -> Option<BattleModelData> {
        self.made
            .entry(key)
            .or_insert_with(|| match load(&mut self.models) {
                Ok(model) => Some(Self::data_of(model, role)),
                Err(why) => {
                    eprintln!("battle: cannot load the model of {} ({why}); a stand-in is drawn", what());
                    None
                }
            })
            .clone()
    }
}

impl BattleModelSource for ArchiveModels {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        let which = battle_character(who.character);
        match self.made(which.prefix().to_string(), ModelRole::Character, || which.display_name().to_string(), |m| m.load_character(which)) {
            Some(data) => BattleActorModel::Battle(data),
            None => StandInModels.party_member(who),
        }
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        let Some(id) = who.enemy_id else { return StandInModels.enemy(who) };
        // An enemy id that names no enemy model of this archive is a box, quietly.
        let Some(prefix) = self.models.enemy_prefix(id) else { return StandInModels.enemy(who) };
        match self.made(prefix, ModelRole::Enemy, || format!("enemy {id}"), |m| m.load_enemy(id)) {
            Some(mut data) => {
                // The attacks are the enemy's own (the model is shared, the clone is ours to dress).
                data.attacks = Arc::new(self.book.attacks_of(id));
                BattleActorModel::Battle(data)
            }
            None => StandInModels.enemy(who),
        }
    }

    fn location(&mut self, location: u16) -> Option<BattleActorModel> {
        let prefix = self.models.location_prefix(location)?;
        self.made(prefix, ModelRole::Plain, || format!("battle location {location}"), |m| m.load_location(location)).map(BattleActorModel::Battle)
    }
}

/// A model made ready to draw.
pub struct ActorRig {
    /// The mesh the GPU holds (one upload per asset, keyed by its id).
    pub asset: Arc<AvatarAsset>,
    /// The clips; `clips[0]` is the idle loop.
    clips: Arc<Vec<FieldAnimation>>,
    /// Battle units per model unit.
    pub scale: f32,
    /// How far the top of the head is above the ground, in battle units.
    pub height: f32,
    /// Half the width of the model, in battle units (its shadow and how close another can come).
    pub radius: f32,
    /// A stand-in has no motion of its own (its clip is a still pose): the scene sways it a little.
    pub procedural_idle: bool,
    /// Playback rate of the clips.
    pub fps: f32,
    /// Models that move with this one (a weapon): drawn with the same placement at the same frame.
    pub attachments: Vec<RigAttachment>,
    /// Which of the clips plays what ([`Kit::none`] for a stand-in or a box: it moves procedurally).
    pub kit: Kit,
}

/// An [`Attachment`] made ready to draw.
pub struct RigAttachment {
    pub asset: Arc<AvatarAsset>,
    clips: Arc<Vec<FieldAnimation>>,
}

/// The root of `clip` at the (fractional, wrapping) frame position `frame`: its `x` and `z`
/// translation, interpolated as [`FieldAnimation::pose_at`] interpolates.
fn root_xz(clip: &FieldAnimation, frame: f32) -> [f32; 2] {
    let n = clip.frame_count();
    let time = if frame.is_finite() { frame.rem_euclid(n as f32) } else { 0.0 };
    let lower = (time.floor() as usize).min(n - 1);
    let t = time - time.floor();
    let (Some(a), Some(b)) = (clip.frame(lower), clip.frame((lower + 1) % n)) else { return [0.0; 2] };
    let mix = |k: usize| a.root_translation[k] + (b.root_translation[k] - a.root_translation[k]) * t;
    [mix(0), mix(2)]
}

/// Take the root's travel along the ground out of a pose: every bone matrix moved by `-root`.
fn in_place(pose: &mut [[f32; 16]], root: [f32; 2]) {
    let shift = mat4_translate([-root[0], 0.0, -root[1]]);
    for bone in pose.iter_mut() {
        *bone = mat4_mul(&shift, bone);
    }
}

impl ActorRig {
    /// The pose of clip `slot` at the (fractional) frame position `frame`, standing IN PLACE: the root's
    /// translation along the ground is taken out (see [`super::clips`]: the game's clips carry travel of
    /// hundreds of units that this client composes itself), its height is kept. `None` when the rig has no
    /// such slot or the clip does not fit the skeleton; the root's `(x, z)` that was taken out comes with
    /// the pose, for the attachments.
    pub fn clip_pose(&self, slot: Slot, frame: f32) -> Option<(Vec<[f32; 16]>, [f32; 2])> {
        let clip = self.clips.get(usize::from(slot))?;
        let mut pose = clip.pose_at(&self.asset.character.model.bones, frame)?;
        let root = root_xz(clip, frame);
        in_place(&mut pose, root);
        Some((pose, root))
    }

    /// The poses of the attachments in clip `slot` at `frame` for a body whose root `(x, z)` was taken
    /// out: the weapon's track is in the character's space, so it moves by the same. An attachment with no
    /// track for `slot` is left out.
    pub fn attachment_clip_poses(&self, slot: Slot, frame: f32, root: [f32; 2]) -> Vec<(&Arc<AvatarAsset>, Vec<[f32; 16]>)> {
        self.attachments
            .iter()
            .filter_map(|a| {
                let mut pose = a.clips.get(usize::from(slot))?.pose_at(&a.asset.character.model.bones, frame)?;
                in_place(&mut pose, root);
                Some((&a.asset, pose))
            })
            .collect()
    }

    /// The frame position `seconds` into the idle loop: 0 for a still pose, else the loop's own.
    fn frame_at(&self, seconds: f32) -> Option<f32> {
        let clip = self.clips.first()?;
        Some(if self.procedural_idle || clip.frame_count() <= 1 { 0.0 } else { seconds * self.fps })
    }

    /// The pose of the idle loop `seconds` into it (a still pose for a one-frame clip): per-bone
    /// matrices into native model space. `None` only if the clip does not fit the skeleton.
    pub fn idle_pose(&self, seconds: f32) -> Option<Vec<[f32; 16]>> {
        let clip = self.clips.first()?;
        clip.pose_at(&self.asset.character.model.bones, self.frame_at(seconds)?)
    }

    /// The poses of the attachments at the same frame as [`ActorRig::idle_pose`] (clip 0 of each, as
    /// its clips are aligned with the owner's); one that cannot be posed is left out.
    pub fn attachment_poses(&self, seconds: f32) -> Vec<(&Arc<AvatarAsset>, Vec<[f32; 16]>)> {
        let Some(frame) = self.frame_at(seconds) else { return Vec::new() };
        self.attachments
            .iter()
            .filter_map(|a| Some((&a.asset, a.clips.first()?.pose_at(&a.asset.character.model.bones, frame)?)))
            .collect()
    }
}

impl std::fmt::Debug for ActorRig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorRig")
            .field("asset", &self.asset.id)
            .field("scale", &self.scale)
            .field("height", &self.height)
            .field("radius", &self.radius)
            .field("procedural_idle", &self.procedural_idle)
            .finish()
    }
}

/// A `.a` clip of one frame in which none of `bones` is rotated and the root is where it stands.
pub(super) fn still_clip(bones: usize) -> FieldAnimation {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(bones as u32).to_le_bytes());
    bytes.extend_from_slice(&[1, 0, 2, 0]);
    bytes.extend(std::iter::repeat_n(0u8, 20));
    bytes.extend(std::iter::repeat_n(0u8, 24 + 12 * bones));
    FieldAnimation::parse(&bytes).expect("a still clip in the documented layout")
}

fn shade(rgb: [u8; 3], k: f32) -> [u8; 4] {
    let c = |v: u8| (f32::from(v) * k).round().clamp(0.0, 255.0) as u8;
    [c(rgb[0]), c(rgb[1]), c(rgb[2]), 255]
}

/// The mesh of a box in native model space: `x` is the model's left, `y` points down so the top is
/// at `-height`, the front is `-z`. Each face has its own shade, and the front carries a dark
/// "visor" band so that the way the box faces can be seen.
fn box_part(spec: &BoxSpec) -> MeshPart {
    let (x0, x1) = (-spec.width * 0.5, spec.width * 0.5);
    let (y0, y1) = (-spec.height, 0.0);
    let (z0, z1) = (-spec.depth * 0.5, spec.depth * 0.5);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut quad = |corners: [[f32; 3]; 4], normal: [f32; 3], color: [u8; 4]| {
        let base = positions.len() as u32;
        positions.extend(corners);
        normals.extend([normal; 4]);
        colors.extend([color; 4]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let rgb = spec.rgb;
    // The front, in three strips: the top of the head, the visor, the rest.
    let strip = |from: f32, to: f32| (y0 + (y1 - y0) * from, y0 + (y1 - y0) * to);
    for ((from, to), color) in [((0.0, 0.22), shade(rgb, 0.95)), ((0.22, 0.40), shade([24, 24, 34], 1.0)), ((0.40, 1.0), shade(rgb, 0.95))] {
        let (ya, yb) = strip(from, to);
        quad([[x0, ya, z0], [x1, ya, z0], [x1, yb, z0], [x0, yb, z0]], [0.0, 0.0, -1.0], color);
    }
    quad([[x1, y0, z1], [x0, y0, z1], [x0, y1, z1], [x1, y1, z1]], [0.0, 0.0, 1.0], shade(rgb, 0.58));
    quad([[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]], [1.0, 0.0, 0.0], shade(rgb, 0.80));
    quad([[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]], [-1.0, 0.0, 0.0], shade(rgb, 0.66));
    quad([[x0, y0, z0], [x0, y0, z1], [x1, y0, z1], [x1, y0, z0]], [0.0, -1.0, 0.0], shade(rgb, 1.18));
    quad([[x0, y1, z1], [x0, y1, z0], [x1, y1, z0], [x1, y1, z1]], [0.0, 1.0, 0.0], shade(rgb, 0.40));
    let n = positions.len();
    MeshPart {
        bone_index: 0,
        positions,
        normals,
        uvs: vec![[0.0; 2]; n],
        colors,
        indices,
        texture: None,
        texture_name: None,
        blend: PartBlend::Opaque,
        alpha: 1.0,
    }
}

/// The box as a one-bone character with a still idle clip.
fn box_character(spec: &BoxSpec) -> Character {
    let model = FieldModel {
        hrc_name: "box".into(),
        skeleton_name: "box".into(),
        bones: vec![ModelBone { name: "root".into(), parent: None, length: spec.height }],
        parts: vec![box_part(spec)],
        dropped_triangles: 0,
        missing_textures: Vec::new(),
    };
    let clip = still_clip(1);
    Character { appearance: NON_PLAYABLE, model, idle: clip.clone(), walk: clip.clone(), run: clip }
}

/// Half the widest of the idle pose's extent along `x` and `z` (battle units after `scale`).
fn radius_of(asset: &AvatarAsset, scale: f32) -> f32 {
    let model = &asset.character.model;
    let extent = asset
        .character
        .pose(fmv_core::AnimState::Idle, 0)
        .and_then(|pose| model.posed_bounds(&pose, &fmv_ff7::model::mat4_identity()))
        .map_or(0.0, |(lo, hi)| (hi[0] - lo[0]).max(hi[2] - lo[2]));
    if extent.is_finite() && extent > 0.5 {
        extent * 0.5 * scale
    } else {
        5.5 * scale
    }
}

/// Half the width of the main mass of a real battle model's idle pose, in battle units after
/// `scale`: the 90th percentile of its vertices' horizontal distance from the model's own axis, so a
/// hound's long tail or a serpent's neck does not give it a shadow (and a stand-off distance) twice
/// its body. Never more than [`radius_of`]'s widest extent and never less than half of it.
fn body_radius_of(asset: &AvatarAsset, scale: f32) -> f32 {
    let widest = radius_of(asset, scale);
    let Some(pose) = asset.character.pose(fmv_core::AnimState::Idle, 0) else { return widest };
    let mesh = &asset.mesh;
    let mut distances: Vec<f32> = Vec::with_capacity(mesh.positions.len());
    for run in &mesh.bone_runs {
        let (Some(bone), Some(vertices)) = (pose.get(run.bone), mesh.positions.get(run.start..run.start + run.len)) else { continue };
        distances.extend(vertices.iter().map(|&p| {
            let at = fmv_ff7::model::transform_point(bone, p);
            at[0].hypot(at[2])
        }));
    }
    if !widest.is_finite() || distances.is_empty() {
        return widest;
    }
    distances.sort_by(f32::total_cmp);
    let p90 = distances.get(distances.len() * 9 / 10).copied().unwrap_or(0.0) * scale;
    if p90.is_finite() {
        p90.clamp(widest * 0.5, widest)
    } else {
        widest
    }
}

/// The pack of `clips` as the choreography sees it (`ok` empty: every slot is real).
pub(super) fn clip_set(clips: &[FieldAnimation], ok: &[bool], fps: f32) -> ClipSet {
    let frames = clips.iter().map(|c| u16::try_from(c.frame_count()).unwrap_or(u16::MAX)).collect();
    let travel = clips
        .iter()
        .map(|c| match (c.frames().first(), c.frames().last()) {
            (Some(a), Some(b)) => b.root_translation[2] - a.root_translation[2],
            _ => 0.0,
        })
        .collect();
    // How many frames go by before the root has covered a tenth of its travel (a clip that carries the actor home holds
    // its pose first: the way home begins there).
    let lead = clips
        .iter()
        .map(|c| {
            let (Some(first), Some(last)) = (c.frames().first(), c.frames().last()) else { return 0 };
            let total = last.root_translation[2] - first.root_translation[2];
            if !total.is_finite() || total.abs() < 1.0 {
                return 0;
            }
            c.frames().iter().position(|f| (f.root_translation[2] - first.root_translation[2]) / total >= 0.1).map_or(0, |at| u16::try_from(at).unwrap_or(u16::MAX))
        })
        .collect();
    ClipSet { frames, ok: (0..clips.len()).map(|i| ok.get(i).copied().unwrap_or(true)).collect(), travel, lead, fps }
}

/// The drawable asset of a model and its clips, if every clip fits its skeleton. The asset's
/// character has the model's first three clips (the avatar mesh wants three).
fn battle_asset(model: &Arc<FieldModel>, clips: &Arc<Vec<FieldAnimation>>) -> Option<Arc<AvatarAsset>> {
    let idle = clips.first()?;
    if !clips.iter().all(|c| model.fits(c)) {
        return None;
    }
    let clip = |i: usize| clips.get(i).unwrap_or(idle).clone();
    let character = Character { appearance: NON_PLAYABLE, model: (**model).clone(), idle: clip(0), walk: clip(1), run: clip(2) };
    AvatarAsset::new(character).ok().map(Arc::new)
}

/// Models made ready to draw, shared between actors that look alike.
pub struct Rigs {
    /// The playable characters' field models; `None` when this machine has no `char.lgp`.
    library: Option<AvatarLibrary>,
    cache: HashMap<String, Arc<ActorRig>>,
    /// The attachments made so far by key (`None`: one that could not be drawn).
    attachments: HashMap<String, Option<Arc<AvatarAsset>>>,
}

impl Rigs {
    pub fn new(loader: Option<Box<dyn CharacterLoader>>) -> Self {
        Self { library: loader.map(AvatarLibrary::new), cache: HashMap::new(), attachments: HashMap::new() }
    }

    /// Distinct rigs made so far.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.cache.len()
    }

    /// The rig of `model`, made on first use. A model that cannot be had (no `char.lgp`, a
    /// character that does not load, a battle model whose clips do not fit) is `fallback`'s box.
    pub fn resolve(&mut self, model: &BattleActorModel, fallback: BoxSpec) -> Arc<ActorRig> {
        self.resolve_optional(model).unwrap_or_else(|| self.boxed(&fallback))
    }

    fn boxed(&mut self, spec: &BoxSpec) -> Arc<ActorRig> {
        let key = spec.key();
        if let Some(rig) = self.cache.get(&key) {
            return Arc::clone(rig);
        }
        let asset = Arc::new(AvatarAsset::new(box_character(spec)).expect("a box has triangles"));
        let rig = Arc::new(ActorRig {
            clips: Arc::new(vec![asset.character.idle.clone()]),
            height: spec.height,
            radius: spec.width.max(spec.depth) * 0.5,
            scale: 1.0,
            procedural_idle: true,
            fps: BATTLE_CLIP_FPS,
            attachments: Vec::new(),
            kit: Kit::none(),
            asset,
        });
        self.cache.insert(key, Arc::clone(&rig));
        rig
    }

    fn field(&mut self, look: Option<&str>) -> Option<Arc<ActorRig>> {
        let library = self.library.as_mut()?;
        let appearance = library.get(look);
        let key = format!("field:{}", appearance.id);
        if let Some(rig) = self.cache.get(&key) {
            return Some(Arc::clone(rig));
        }
        let asset = appearance.asset?;
        let scale = FIELD_TO_BATTLE_SCALE;
        let rig = Arc::new(ActorRig {
            clips: Arc::new(vec![asset.character.idle.clone()]),
            height: asset.height_units * scale,
            radius: radius_of(&asset, scale),
            scale,
            procedural_idle: true,
            fps: crate::avatar::anim::FIELD_FPS,
            attachments: Vec::new(),
            kit: Kit::none(),
            asset,
        });
        self.cache.insert(key, Arc::clone(&rig));
        Some(rig)
    }

    fn battle(&mut self, battle: &BattleModelData) -> Option<Arc<ActorRig>> {
        let key = format!("battle:{}", battle.key);
        if let Some(rig) = self.cache.get(&key) {
            return Some(Arc::clone(rig));
        }
        let asset = battle_asset(&battle.model, &battle.clips)?;
        if !battle.scale.is_finite() || battle.scale <= 0.0 {
            return None;
        }
        // A weapon that cannot be drawn is not drawn; the character still is. Each is made once.
        let mut attachments = Vec::new();
        for a in &battle.attachments {
            let made = self.attachments.entry(a.key.clone()).or_insert_with(|| battle_asset(&a.model, &a.clips)).clone();
            if let Some(asset) = made {
                attachments.push(RigAttachment { asset, clips: Arc::clone(&a.clips) });
            }
        }
        let procedural_idle = battle.clips.first().is_some_and(|c| c.frame_count() <= 1);
        let fps = if battle.fps.is_finite() && battle.fps > 0.0 { battle.fps } else { BATTLE_CLIP_FPS };
        // A model whose idle is a still pose has no motion of its own to dress: it stays procedural.
        let kit = match (battle.role, procedural_idle) {
            (ModelRole::Character, false) => Kit::party(clip_set(&battle.clips, &battle.slot_ok, fps), &battle.scripts),
            (ModelRole::Enemy, false) => Kit::enemy(clip_set(&battle.clips, &battle.slot_ok, fps), &battle.scripts, &battle.attacks),
            _ => Kit::none(),
        };
        let rig = Arc::new(ActorRig {
            clips: Arc::clone(&battle.clips),
            height: asset.height_units * battle.scale,
            radius: body_radius_of(&asset, battle.scale),
            scale: battle.scale,
            procedural_idle,
            fps,
            attachments,
            kit,
            asset,
        });
        self.cache.insert(key, Arc::clone(&rig));
        Some(rig)
    }

    /// The rig of `model` if it can be had, with no stand-in otherwise: for what has no fallback
    /// box (the location).
    pub fn resolve_optional(&mut self, model: &BattleActorModel) -> Option<Arc<ActorRig>> {
        match model {
            BattleActorModel::Field(look) => self.field(look.as_deref()),
            BattleActorModel::Battle(battle) => self.battle(battle),
            BattleActorModel::Box(spec) => Some(self.boxed(spec)),
        }
    }
}

/// What the scene needs to give every actor a model: who decides and who loads.
pub struct ModelKit {
    pub(super) source: Box<dyn BattleModelSource>,
    pub(super) rigs: Rigs,
    /// The meshes of the effects, made once and shared by every scene of this kit.
    pub(super) effects: Arc<EffectKit>,
}

impl ModelKit {
    /// A kit whose models are chosen by `source` and whose field models are read by `loader`.
    pub fn new(source: Box<dyn BattleModelSource>, loader: Option<Box<dyn CharacterLoader>>) -> Self {
        Self { source, rigs: Rigs::new(loader), effects: Arc::new(EffectKit::new()) }
    }

    /// The models of this machine's own archives: the real battle models of its `battle.lgp` when
    /// the client found one ([`remember_battle_lgp`]), the stand-ins ([`StandInModels`]) otherwise;
    /// the field characters come from its `char.lgp` if it has one (see [`crate::avatar::source`]),
    /// boxes for everyone without.
    pub fn from_local_content() -> Self {
        let loader = crate::avatar::source::remembered().map(|path| Box::new(LgpLoader::new(path)) as Box<dyn CharacterLoader>);
        let source: Box<dyn BattleModelSource> = match remembered_battle_lgp() {
            Some(path) => match ArchiveModels::open(&path) {
                Ok(models) => Box::new(models.with_enemy_book(Self::local_enemy_book())),
                Err(why) => {
                    eprintln!("battle: cannot open battle.lgp ({why}); stand-ins are drawn");
                    Box::new(StandInModels)
                }
            },
            None => Box::new(StandInModels),
        };
        Self::new(source, loader)
    }

    /// What this machine's own `scene.bin` says about the enemies' attacks (empty when the client found
    /// none or it does not read: the enemies' attacks then play no clip of their own).
    fn local_enemy_book() -> EnemyBook {
        let Some(path) = remembered_scene_bin() else { return EnemyBook::default() };
        match fmv_ff7_battle::scene::SceneBin::open(&path) {
            Ok(scenes) => EnemyBook::from_scenes(&scenes),
            Err(why) => {
                eprintln!("battle: cannot read scene.bin for the enemies' attack clips ({why}); their attacks play no clip");
                EnemyBook::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar::library::tests::{character, Counting};
    use fmv_core::AnimState;
    use fmv_ff7::model::{mat4_identity, transform_point};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rigs(fail: Vec<&'static str>) -> (Rigs, Rc<RefCell<Vec<&'static str>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        (Rigs::new(Some(Box::new(Counting::new(Rc::clone(&asked), fail)))), asked)
    }

    fn enemy(id: Option<u16>, name: &str, hp: u32) -> EnemyLook {
        EnemyLook { actor: ActorId(3), enemy_id: id, name: name.into(), max_hp: hp, level: 3 }
    }

    #[test]
    fn a_party_member_looks_like_the_character_of_its_slot_then_cloud() {
        // Battle placeholder: the slot's own model, one per slot.
        assert_eq!(party_appearance(2), "ff7/tifa");
        assert_eq!(party_appearance(1), "ff7/barret");
        assert_eq!(party_appearance(7), "ff7/vincent");
        for (character, want) in [(0u8, "ff7/cloud"), (3, "ff7/aerith"), (4, "ff7/red_xiii"), (5, "ff7/yuffie"), (8, "ff7/cid")] {
            assert_eq!(party_appearance(character), want);
        }
        // Cait Sith has no field model, and an unknown slot no character: Cloud.
        assert_eq!(party_appearance(6), "ff7/cloud");
        assert_eq!(party_appearance(200), "ff7/cloud");
        // Every answer is a real canon table entry (never a placeholder body: a body is how a player looks in the field).
        for character in 0..=255u8 {
            assert!(fmv_ff7::playable::resolve_appearance(party_appearance(character)).is_some());
        }
        // The stand-in source says Field with that model for the party and a box for an enemy.
        let mut source = StandInModels;
        let who = PartyLook { actor: ActorId(0), character: 2, mine: true };
        assert!(matches!(source.party_member(&who), BattleActorModel::Field(Some(ref r)) if r == "ff7/tifa"));
        assert!(matches!(source.enemy(&enemy(Some(4), "Guard", 120)), BattleActorModel::Box(_)));
    }

    #[test]
    fn enemy_boxes_grow_with_their_hit_points_within_bounds_and_kinds_differ_in_colour() {
        let sizes: Vec<f32> = [1u32, 20, 100, 1_000, 10_000, 1_000_000].iter().map(|&hp| BoxSpec::for_enemy(0, hp).height).collect();
        assert!(sizes.windows(2).all(|w| w[0] <= w[1]), "taller with more hit points: {sizes:?}");
        assert!(sizes.iter().all(|h| (800.0..=2600.0).contains(h)), "{sizes:?}");
        assert_eq!(sizes[0], 800.0);
        assert_eq!(*sizes.last().unwrap(), 2600.0);
        assert_ne!(BoxSpec::for_enemy(1, 100).rgb, BoxSpec::for_enemy(2, 100).rgb);
        assert_eq!(BoxSpec::for_enemy(1, 100).rgb, BoxSpec::for_enemy(1 + 8, 100).rgb, "the palette repeats");
        // With no id the name tells kinds apart, the same name always the same kind.
        let a = kind_of(&enemy(None, "Guard Hound", 10));
        assert_eq!(a, kind_of(&enemy(None, "Guard Hound", 99)));
        assert_ne!(a, kind_of(&enemy(None, "Sweeper", 10)));
        assert_eq!(kind_of(&enemy(Some(7), "whatever", 10)), 7);
        // Four distinct party boxes, then the palette repeats.
        assert_ne!(BoxSpec::for_party(0).rgb, BoxSpec::for_party(1).rgb);
        assert_eq!(BoxSpec::for_party(0).rgb, BoxSpec::for_party(4).rgb);
    }

    #[test]
    fn a_box_is_a_closed_mesh_standing_on_the_ground_with_its_front_towards_minus_z() {
        let spec = BoxSpec { width: 800.0, depth: 600.0, height: 1000.0, rgb: [200, 100, 50] };
        let (mut rigs, _) = rigs(vec![]);
        let rig = rigs.resolve(&BattleActorModel::Box(spec), BoxSpec::for_party(0));
        let mesh = &rig.asset.mesh;
        // Six faces; the front is three strips: eight quads, sixteen triangles, one opaque batch.
        assert_eq!(mesh.indices.len() / 3, 16);
        assert_eq!(mesh.batches.len(), 1);
        assert_eq!(mesh.positions.len(), 32);
        // In the idle pose it spans its own size: feet at 0, head top at -height (y points down).
        let pose = rig.idle_pose(0.0).unwrap();
        let (lo, hi) = rig.asset.character.model.posed_bounds(&pose, &mat4_identity()).unwrap();
        assert_eq!([lo[0], hi[0]], [-400.0, 400.0]);
        assert_eq!([lo[1], hi[1]], [-1000.0, 0.0]);
        assert_eq!([lo[2], hi[2]], [-300.0, 300.0]);
        assert!((rig.height - 1000.0).abs() < 1e-3 && (rig.radius - 400.0).abs() < 1e-3);
        assert!(rig.procedural_idle && rig.scale == 1.0);
        // The visor is on the -z face: its dark vertices all lie at z = -depth/2.
        let dark = mesh.colors.iter().enumerate().filter(|(_, c)| c[0] < 40).map(|(i, _)| mesh.positions[i]).collect::<Vec<_>>();
        assert_eq!(dark.len(), 4);
        assert!(dark.iter().all(|p| p[2] == -300.0), "{dark:?}");
        // The faces differ in brightness (that is what makes a box look solid): the top is the lightest.
        let part = box_part(&spec);
        let of_face = |normal: [f32; 3]| {
            let i = part.normals.iter().position(|n| *n == normal).unwrap();
            u32::from(part.colors[i][0])
        };
        assert!(of_face([0.0, -1.0, 0.0]) > of_face([1.0, 0.0, 0.0]), "the top is lighter than a side");
        assert!(of_face([1.0, 0.0, 0.0]) > of_face([0.0, 0.0, 1.0]), "a side is lighter than the back");
        assert!(of_face([0.0, 0.0, 1.0]) > of_face([0.0, 1.0, 0.0]), "and the underside is the darkest");
    }

    #[test]
    fn rigs_are_made_once_and_shared_between_actors_that_look_alike() {
        let (mut rigs, asked) = rigs(vec![]);
        let cloud = BattleActorModel::Field(Some("ff7/cloud".into()));
        let a = rigs.resolve(&cloud, BoxSpec::for_party(0));
        let b = rigs.resolve(&cloud, BoxSpec::for_party(1));
        assert!(Arc::ptr_eq(&a, &b), "two Clouds are one rig, one GPU upload");
        assert!(Arc::ptr_eq(&a.asset, &b.asset));
        let tifa = rigs.resolve(&BattleActorModel::Field(Some("ff7/tifa".into())), BoxSpec::for_party(0));
        assert!(!Arc::ptr_eq(&a, &tifa) && a.asset.id != tifa.asset.id);
        assert_eq!(*asked.borrow(), ["ff7/cloud", "ff7/tifa"], "each character is read once");
        // An unknown look is Cloud's rig; nothing is read again.
        let unknown = rigs.resolve(&BattleActorModel::Field(Some("ff7/nobody".into())), BoxSpec::for_party(0));
        assert!(Arc::ptr_eq(&a, &unknown));
        let none = rigs.resolve(&BattleActorModel::Field(None), BoxSpec::for_party(0));
        assert!(Arc::ptr_eq(&a, &none));
        // Boxes of the same spec are one rig, different specs two.
        let spec = BoxSpec::for_enemy(3, 100);
        let e1 = rigs.resolve(&BattleActorModel::Box(spec), spec);
        let e2 = rigs.resolve(&BattleActorModel::Box(spec), spec);
        let e3 = rigs.resolve(&BattleActorModel::Box(BoxSpec::for_enemy(4, 100)), spec);
        assert!(Arc::ptr_eq(&e1, &e2) && !Arc::ptr_eq(&e1, &e3));
        assert_eq!(rigs.count(), 4);
    }

    #[test]
    fn a_field_model_is_scaled_to_battle_units_and_sized_from_its_idle_pose() {
        let (mut rigs, _) = rigs(vec![]);
        let rig = rigs.resolve(&BattleActorModel::Field(None), BoxSpec::for_party(0));
        assert_eq!(rig.scale, FIELD_TO_BATTLE_SCALE);
        // The stand-in character is 30 model units tall and about 6 wide (see `library::tests`).
        assert!((rig.height - 30.0 * FIELD_TO_BATTLE_SCALE).abs() < 1.0, "{}", rig.height);
        assert!((rig.radius - 3.0 * FIELD_TO_BATTLE_SCALE).abs() < 1.0, "{}", rig.radius);
        assert!(rig.procedural_idle, "the stand-in's idle clip is a still pose: the scene sways it");
        // Its pose is the clip's first frame.
        assert_eq!(rig.idle_pose(1.7).map(|p| p.len()), Some(1));
        let pose = rig.idle_pose(0.0).unwrap();
        let foot = transform_point(&pose[0], [0.0, 5.0, 0.0]);
        assert!(foot[1].abs() < 1e-3, "standing on the ground: {foot:?}");
        assert_eq!(rig.asset.character.pose(AnimState::Idle, 0).map(|p| p.len()), Some(1));
    }

    #[test]
    fn a_character_that_cannot_be_had_is_the_fallback_box_and_no_archive_means_boxes_for_all() {
        let (mut failing, asked) = rigs(vec!["ff7/tifa"]);
        let fallback = BoxSpec::for_party(1);
        let rig = failing.resolve(&BattleActorModel::Field(Some("ff7/tifa".into())), fallback);
        assert!(rig.procedural_idle && rig.scale == 1.0, "a box, not a character");
        assert!((rig.height - fallback.height).abs() < 1e-3);
        failing.resolve(&BattleActorModel::Field(Some("ff7/tifa".into())), fallback);
        assert_eq!(asked.borrow().len(), 1, "the failure is remembered by the library, never retried");
        // The other characters still load.
        let cloud = failing.resolve(&BattleActorModel::Field(Some("ff7/cloud".into())), fallback);
        assert_eq!(cloud.scale, FIELD_TO_BATTLE_SCALE);
        // A machine without char.lgp has no library at all.
        let mut none = Rigs::new(None);
        let rig = none.resolve(&BattleActorModel::Field(Some("ff7/cloud".into())), fallback);
        assert_eq!(rig.scale, 1.0);
        assert!(Arc::ptr_eq(&rig, &none.resolve(&BattleActorModel::Box(fallback), fallback)), "the fallback box is shared");
    }

    #[test]
    fn a_battle_model_is_drawn_with_its_own_clips_scale_and_rate() {
        let stick = character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3);
        let mut model = BattleModelData::new("rtaa", stick.model.clone(), vec![stick.idle.clone(), stick.walk.clone()]);
        model.scale = 2.5;
        model.fps = 20.0;
        let (mut rigs, _) = rigs(vec![]);
        let rig = rigs.resolve(&BattleActorModel::Battle(model.clone()), BoxSpec::for_party(0));
        assert_eq!((rig.scale, rig.fps), (2.5, 20.0));
        assert!((rig.height - 30.0 * 2.5).abs() < 1e-3, "{}", rig.height);
        assert!(rig.procedural_idle, "a one-frame idle is a still pose");
        // The same key is the same rig (one upload); another key another.
        assert!(Arc::ptr_eq(&rig, &rigs.resolve(&BattleActorModel::Battle(model.clone()), BoxSpec::for_party(0))));
        let mut other = model.clone();
        other.key = "rtab".into();
        assert!(!Arc::ptr_eq(&rig, &rigs.resolve(&BattleActorModel::Battle(other), BoxSpec::for_party(0))));
        // The constructor on the enum is the same thing at scale 1.
        let BattleActorModel::Battle(plain) = BattleActorModel::battle("x", stick.model.clone(), vec![stick.idle.clone()]) else { panic!() };
        assert_eq!((plain.scale, plain.fps), (1.0, BATTLE_CLIP_FPS));
        // A looping idle (more than one frame) plays: two times give two poses.
        let looping = BattleModelData::new("loop", stick.model.clone(), vec![stick.walk.clone()]);
        let rig = rigs.resolve(&BattleActorModel::Battle(looping), BoxSpec::for_party(0));
        assert!(!rig.procedural_idle);
        assert_eq!(rig.idle_pose(0.3).map(|p| p.len()), Some(1));
    }

    #[test]
    fn a_weapon_rides_with_its_owner_at_the_same_frame_and_is_made_once() {
        let stick = character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3);
        // The owner loops over 4 frames; its weapon is a one-bone model with a clip of its own.
        let weapon = Attachment::new("sword", stick.model.clone(), vec![stick.run.clone()]);
        let owner = BattleModelData::new("rtaa", stick.model.clone(), vec![stick.walk.clone()]).with_attachment(weapon.clone());
        let (mut rigs, _) = rigs(vec![]);
        let rig = rigs.resolve(&BattleActorModel::Battle(owner), BoxSpec::for_party(0));
        assert!(!rig.procedural_idle, "the owner's idle loops");
        let poses = rig.attachment_poses(0.3);
        assert_eq!(poses.len(), 1);
        assert_eq!(poses[0].1.len(), 1, "one bone");
        // A weapon with the same key on another owner is the same asset (one GPU upload).
        let other = BattleModelData::new("rtab", stick.model.clone(), vec![stick.walk.clone()]).with_attachment(weapon);
        let again = rigs.resolve(&BattleActorModel::Battle(other), BoxSpec::for_party(0));
        assert!(Arc::ptr_eq(&rig.attachments[0].asset, &again.attachments[0].asset));
        assert!(!Arc::ptr_eq(&rig.asset, &again.asset));
        // A weapon whose clip does not fit its skeleton is left out; the owner is still drawn.
        let bad = Attachment::new("broken", stick.model.clone(), vec![crate::avatar::library::tests::clip(2, 5, 1.0)]);
        let hurt = BattleModelData::new("rtac", stick.model.clone(), vec![stick.idle.clone()]).with_attachment(bad);
        let rig = rigs.resolve(&BattleActorModel::Battle(hurt), BoxSpec::for_party(0));
        assert!(rig.attachments.is_empty() && rig.attachment_poses(0.0).is_empty());
        assert_eq!(rig.scale, 1.0, "the owner is a battle model, not the fallback box");
        // A character from the field has no weapon.
        let cloud = rigs.resolve(&BattleActorModel::Field(None), BoxSpec::for_party(0));
        assert!(cloud.attachment_poses(0.0).is_empty());
    }

    #[test]
    fn a_source_has_no_location_unless_it_says_so() {
        assert!(StandInModels.location(7).is_none());
        let (mut rigs, _) = rigs(vec![]);
        let stick = character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3);
        // A location that cannot be had is nothing, never the fallback box.
        assert!(rigs.resolve_optional(&BattleActorModel::battle("loc", stick.model.clone(), vec![])).is_none());
        assert!(rigs.resolve_optional(&BattleActorModel::battle("loc", stick.model.clone(), vec![stick.idle.clone()])).is_some());
    }

    #[test]
    fn a_battle_model_that_cannot_be_drawn_is_refused_not_trusted() {
        let stick = character(fmv_ff7::playable::resolve_appearance("ff7/cloud").unwrap(), 1, 4, 3);
        let (mut rigs, _) = rigs(vec![]);
        let fallback = BoxSpec::for_party(2);
        let box_rig = rigs.resolve(&BattleActorModel::Box(fallback), fallback);
        for bad in [
            // No clips at all.
            BattleModelData::new("a", stick.model.clone(), vec![]),
            // A clip for a different skeleton.
            BattleModelData::new("b", stick.model.clone(), vec![crate::avatar::library::tests::clip(2, 5, 1.0)]),
            // A scale that is not a size.
            BattleModelData { scale: 0.0, ..BattleModelData::new("c", stick.model.clone(), vec![stick.idle.clone()]) },
            BattleModelData { scale: f32::NAN, ..BattleModelData::new("d", stick.model.clone(), vec![stick.idle.clone()]) },
        ] {
            let rig = rigs.resolve(&BattleActorModel::Battle(bad), fallback);
            assert!(Arc::ptr_eq(&rig, &box_rig), "the fallback box instead");
        }
        // A rate that is not a rate is the default one.
        let odd = BattleModelData { fps: f32::INFINITY, ..BattleModelData::new("e", stick.model.clone(), vec![stick.idle.clone()]) };
        assert_eq!(rigs.resolve(&BattleActorModel::Battle(odd), fallback).fps, BATTLE_CLIP_FPS);
    }
}

#[cfg(test)]
mod archive_tests;
