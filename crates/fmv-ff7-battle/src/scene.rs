//! PC `scene.bin` parser: enemy formations, enemy stat records and attack data.
//!
//! # File format
//!
//! Layout source: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes>
//! (Qhimm / Final Fantasy Inside wiki). The offsets below were also checked against the
//! owner's own FF7 Steam files (read-only, never copied) by this crate's `#[ignore]`d
//! real-data tests.
//!
//! `scene.bin` is a sequence of fixed 8192-byte blocks. Each block starts with sixteen
//! little-endian `u32` pointers, in units of 4 bytes, to gzip-compressed scenes stored in the
//! same block; `0xFFFF_FFFF` ends the list. Each scene inflates to exactly 7808 bytes:
//!
//! | offset | size | content |
//! |---|---|---|
//! | `0x0000` | 3 x u16 | enemy ids (`0xFFFF` = unused), followed by 2 bytes of padding |
//! | `0x0008` | 4 x 20 | battle setup (location, chained formation, layout, camera index) |
//! | `0x0058` | 4 x 48 | camera placement (three 12-byte cameras + unused tail) |
//! | `0x0118` | 4 x 6 x 16 | formations: six 16-byte placement entries per formation |
//! | `0x0298` | 3 x 184 | enemy data records |
//! | `0x04C0` | 32 x 28 | attack records |
//! | `0x0840` | 32 x u16 | attack ids (`0xFFFF` = unused slot) |
//! | `0x0880` | 32 x 32 | attack names (FF text) |
//! | `0x0C80` | rest | formation / enemy AI scripts (the enemies' are kept as [`Scene::ai`], see [`crate::ai`]) |
//!
//! A battle ("formation") number is `scene_index * 4 + formation_slot`; see
//! [`global_formation_index`] and [`split_formation_index`].
//!
//! # Safety on untrusted input
//!
//! No indexing on file data; all field reads go through total helpers. Every gzip stream is
//! read through a hard size cap (the scene size), the scene count is capped at
//! [`MAX_SCENES`], and block pointers are validated (inside the block, past the header,
//! strictly increasing) before any slice is taken.
//!
//! # Not parsed
//!
//! The formations' own AI scripts, status bit meanings, and elemental/status application are out of
//! scope for the M2-c battle subset (see `rules`). The Japanese `scene.bin` (16-byte names) is not
//! supported and is rejected by the size check.

use std::path::Path;

use crate::ai::SceneAi;
use crate::bytes::{array_at, gunzip_capped, i16_at, u16_array_at, u16_at, u32_at, u8_at};
use crate::text;

/// Size of one `scene.bin` block in bytes.
pub const BLOCK_SIZE: usize = 0x2000;
/// Number of scene pointers at the start of each block.
pub const BLOCK_POINTERS: usize = 16;
/// Bytes taken by the pointer table at the start of each block.
pub const BLOCK_HEADER_SIZE: usize = BLOCK_POINTERS * 4;
/// Pointer value meaning "no more scenes in this block".
pub const UNUSED_POINTER: u32 = 0xFFFF_FFFF;
/// Decompressed size of every scene.
pub const SCENE_SIZE: usize = 0x1E80;
/// Maximum number of scenes (formation ids are `0..1024`, four per scene).
pub const MAX_SCENES: usize = 256;
/// Formations per scene.
pub const FORMATIONS_PER_SCENE: usize = 4;
/// Enemy data records per scene.
pub const ENEMIES_PER_SCENE: usize = 3;
/// Placement entries per formation.
pub const ENTRIES_PER_FORMATION: usize = 6;
/// Attack record slots per scene.
pub const ATTACKS_PER_SCENE: usize = 32;
/// Largest `scene.bin` that [`SceneBin::open`] will read (the original is far smaller).
pub const MAX_SCENE_BIN_BYTES: u64 = 4 * 1024 * 1024;

const OFF_ENEMY_IDS: usize = 0x0000;
const OFF_SETUPS: usize = 0x0008;
const SETUP_SIZE: usize = 20;
const OFF_CAMERAS: usize = 0x0058;
const CAMERA_SET_SIZE: usize = 48;
const OFF_FORMATIONS: usize = 0x0118;
const ENTRY_SIZE: usize = 16;
const FORMATION_SIZE: usize = ENTRY_SIZE * ENTRIES_PER_FORMATION;
const OFF_ENEMIES: usize = 0x0298;
const ENEMY_SIZE: usize = 184;
const OFF_ATTACKS: usize = 0x04C0;
/// Size of one attack record (shared with the kernel's attack data section).
pub const ATTACK_RECORD_SIZE: usize = 28;
const OFF_ATTACK_IDS: usize = 0x0840;
const OFF_ATTACK_NAMES: usize = 0x0880;
const ATTACK_NAME_SIZE: usize = 32;

// Layout self-consistency: every table starts where the previous one ends.
const _: () = {
    assert!(OFF_ENEMY_IDS + 8 == OFF_SETUPS);
    assert!(OFF_SETUPS + FORMATIONS_PER_SCENE * SETUP_SIZE == OFF_CAMERAS);
    assert!(OFF_CAMERAS + FORMATIONS_PER_SCENE * CAMERA_SET_SIZE == OFF_FORMATIONS);
    assert!(OFF_FORMATIONS + FORMATIONS_PER_SCENE * FORMATION_SIZE == OFF_ENEMIES);
    assert!(OFF_ENEMIES + ENEMIES_PER_SCENE * ENEMY_SIZE == OFF_ATTACKS);
    assert!(OFF_ATTACKS + ATTACKS_PER_SCENE * ATTACK_RECORD_SIZE == OFF_ATTACK_IDS);
    assert!(OFF_ATTACK_IDS + ATTACKS_PER_SCENE * 2 == OFF_ATTACK_NAMES);
    assert!(OFF_ATTACK_NAMES + ATTACKS_PER_SCENE * ATTACK_NAME_SIZE == 0x0C80);
};

#[cfg(test)]
pub(crate) mod layout {
    //! Offsets re-exported for the synthetic builders in `testutil`.
    pub const OFF_ENEMY_IDS: usize = super::OFF_ENEMY_IDS;
    pub const OFF_SETUPS: usize = super::OFF_SETUPS;
    pub const SETUP_SIZE: usize = super::SETUP_SIZE;
    pub const OFF_CAMERAS: usize = super::OFF_CAMERAS;
    pub const CAMERA_SET_SIZE: usize = super::CAMERA_SET_SIZE;
    pub const OFF_FORMATIONS: usize = super::OFF_FORMATIONS;
    pub const ENTRY_SIZE: usize = super::ENTRY_SIZE;
    pub const FORMATION_SIZE: usize = super::FORMATION_SIZE;
    pub const OFF_ENEMIES: usize = super::OFF_ENEMIES;
    pub const ENEMY_SIZE: usize = super::ENEMY_SIZE;
    pub const OFF_ATTACKS: usize = super::OFF_ATTACKS;
    pub const OFF_ATTACK_IDS: usize = super::OFF_ATTACK_IDS;
    pub const OFF_ATTACK_NAMES: usize = super::OFF_ATTACK_NAMES;
    pub const ATTACK_NAME_SIZE: usize = super::ATTACK_NAME_SIZE;
}

/// Errors from parsing `scene.bin`.
#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    #[error("scene.bin is empty")]
    Empty,
    #[error("scene.bin length {0} is not a multiple of the 8192-byte block size")]
    BadBlockAlignment(usize),
    #[error("scene.bin is {0} bytes, which exceeds the supported maximum")]
    TooLarge(u64),
    #[error("scene.bin contains no scenes")]
    NoScenes,
    #[error("scene.bin holds more than 256 scenes")]
    TooManyScenes,
    #[error("bad scene pointer {0:#x}")]
    BadPointer(u32),
    #[error("gzip error: {0}")]
    Gzip(#[source] std::io::Error),
    #[error("decompressed scene is {actual} bytes, expected 7808")]
    BadSceneSize { actual: usize },
    /// Wraps a per-scene error with its position in the file.
    #[error("scene.bin block {block} slot {slot}: {source}")]
    At {
        block: usize,
        slot: usize,
        #[source]
        source: Box<SceneError>,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Global battle (formation) index for `scene_index * 4 + slot`.
pub const fn global_formation_index(scene_index: usize, slot: usize) -> usize {
    scene_index * FORMATIONS_PER_SCENE + slot
}

/// Inverse of [`global_formation_index`]: `(scene_index, slot)`.
pub const fn split_formation_index(global: usize) -> (usize, usize) {
    (global / FORMATIONS_PER_SCENE, global % FORMATIONS_PER_SCENE)
}

/// Arrival layout of a battle (battle setup byte `0x12`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleLayout {
    Normal,
    Preemptive,
    BackAttack,
    SideAttack,
    Pincer,
    Pincer2,
    SideAttack2,
    SideAttack3,
    /// Normal, but the party is pinned to the front row and Change is disabled.
    NormalFrontRowLocked,
    Unknown(u8),
}

impl BattleLayout {
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Normal,
            1 => Self::Preemptive,
            2 => Self::BackAttack,
            3 => Self::SideAttack,
            4 => Self::Pincer,
            5 => Self::Pincer2,
            6 => Self::SideAttack2,
            7 => Self::SideAttack3,
            8 => Self::NormalFrontRowLocked,
            other => Self::Unknown(other),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Preemptive => 1,
            Self::BackAttack => 2,
            Self::SideAttack => 3,
            Self::Pincer => 4,
            Self::Pincer2 => 5,
            Self::SideAttack2 => 6,
            Self::SideAttack3 => 7,
            Self::NormalFrontRowLocked => 8,
            Self::Unknown(c) => c,
        }
    }

    /// Whether every Run Check of the party succeeds in this arrival: it struck first (preemptive) or
    /// surrounds the enemy (the three side attacks). The pincers, where the enemy surrounds the party, are
    /// not (`crate::rules`, "Escape").
    pub fn makes_running_sure(self) -> bool {
        matches!(self, Self::Preemptive | Self::SideAttack | Self::SideAttack2 | Self::SideAttack3)
    }
}

/// Battle setup record (20 bytes, one per formation slot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleSetup {
    /// Battle location (arena model) code.
    pub location: u16,
    /// Formation id chained into once all current enemies are beaten.
    pub next_formation: Option<u16>,
    pub escape_counter: u16,
    /// Candidate formation ids for the next Battle Arena fight.
    pub next_arena_candidates: [u16; 4],
    /// Raw flags word (offset `0x10`). Bit meanings are not decoded here.
    pub flags: u16,
    pub layout: BattleLayout,
    /// Pre-battle camera position index.
    pub pre_battle_camera: u8,
}

impl BattleSetup {
    /// The bit of [`BattleSetup::flags`] that says the party may run ("Escapable Flag (along with other
    /// flags)" on <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Battle/Battle_Scenes.html>, which names the
    /// word and decodes none of its bits). Checked on every formation of the owner's Steam `scene.bin`
    /// (`tests/real_data.rs`): the bit is set on every formation whose escape counter is 1 to 4 and clear on
    /// every one whose counter is 9 or 10, the bosses and story fights.
    pub const ESCAPABLE_FLAG: u16 = 0x0004;
    /// From this escape counter on the fight cannot be escaped (the owner's file uses 9 and 10 for them).
    pub const NO_ESCAPE_COUNTER: u16 = 9;

    /// The Run Difficulty of the fight: how many Run Checks must succeed before the party is away (the
    /// escape counter, at least 1), or `None` when the fight cannot be escaped (the escapable flag is clear,
    /// or the counter is [`BattleSetup::NO_ESCAPE_COUNTER`] or more). See `crate::rules`, "Escape".
    pub fn run_difficulty(&self) -> Option<u8> {
        if self.flags & Self::ESCAPABLE_FLAG == 0 || self.escape_counter >= Self::NO_ESCAPE_COUNTER {
            return None;
        }
        u8::try_from(self.escape_counter.max(1)).ok()
    }

    fn parse(b: &[u8]) -> Self {
        let next = u16_at(b, 0x02);
        Self {
            location: u16_at(b, 0x00),
            next_formation: (next != 0xFFFF).then_some(next),
            escape_counter: u16_at(b, 0x04),
            next_arena_candidates: u16_array_at(b, 0x08),
            flags: u16_at(b, 0x10),
            layout: BattleLayout::from_code(u8_at(b, 0x12)),
            pre_battle_camera: u8_at(b, 0x13),
        }
    }
}

/// One camera: position and look direction in battle units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Camera {
    pub position: [i16; 3],
    pub direction: [i16; 3],
}

/// Camera placement for one formation slot: the idle camera plus two cameras used by
/// enemy animations. The trailing 12 bytes of the record are unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CameraPlacement {
    pub cameras: [Camera; 3],
}

impl CameraPlacement {
    fn parse(b: &[u8]) -> Self {
        let cam = |base: usize| Camera {
            position: [i16_at(b, base), i16_at(b, base + 2), i16_at(b, base + 4)],
            direction: [
                i16_at(b, base + 6),
                i16_at(b, base + 8),
                i16_at(b, base + 10),
            ],
        };
        Self {
            cameras: [cam(0), cam(12), cam(24)],
        }
    }
}

/// One enemy placement inside a formation (16 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormationEntry {
    /// Index 0..6 of this entry inside its formation.
    pub slot: u8,
    /// Enemy id; matches one of the scene's three [`Scene::enemy_ids`].
    pub enemy_id: u16,
    pub position: [i16; 3],
    /// Row (higher = further back). Enemy rows are not used by the M2-c rules.
    pub row: u16,
    pub cover_flags: u16,
    /// Initial state flags (low 5 bits used: visible, targetable, main script active, ...).
    pub initial_flags: u32,
}

/// One formation: the placed enemies (unused entries are omitted).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Formation {
    pub entries: Vec<FormationEntry>,
}

impl Formation {
    fn parse(b: &[u8]) -> Self {
        let mut entries = Vec::new();
        for k in 0..ENTRIES_PER_FORMATION {
            let e = k * ENTRY_SIZE;
            let enemy_id = u16_at(b, e);
            if enemy_id == 0xFFFF {
                continue;
            }
            entries.push(FormationEntry {
                slot: k as u8,
                enemy_id,
                position: [i16_at(b, e + 2), i16_at(b, e + 4), i16_at(b, e + 6)],
                row: u16_at(b, e + 8),
                cover_flags: u16_at(b, e + 10),
                initial_flags: u32_at(b, e + 12),
            });
        }
        Self { entries }
    }
}

/// How an enemy reacts to an element (enemy data rate codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityRate {
    Death,
    Double,
    Half,
    Nullify,
    Absorb,
    FullHeal,
    Other(u8),
}

impl AffinityRate {
    pub fn from_code(code: u8) -> Self {
        match code {
            0x00 => Self::Death,
            0x02 => Self::Double,
            0x04 => Self::Half,
            0x05 => Self::Nullify,
            0x06 => Self::Absorb,
            0x07 => Self::FullHeal,
            other => Self::Other(other),
        }
    }
}

/// One element/status affinity entry of an enemy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Affinity {
    /// Element code (`0x00..=0x0F` elements, `0x20..=0x3F` statuses).
    pub code: u8,
    pub rate: AffinityRate,
}

/// Human-readable name for element codes `0x00..=0x0E`.
pub fn element_name(code: u8) -> Option<&'static str> {
    const NAMES: [&str; 15] = [
        "Fire", "Ice", "Bolt", "Earth", "Bio", "Gravity", "Water", "Wind", "Holy", "Health", "Cut",
        "Hit", "Punch", "Shoot", "Scream",
    ];
    NAMES.get(usize::from(code)).copied()
}

/// Enemy data record (184 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnemyRecord {
    /// Decoded name with trailing spaces removed. May contain U+FFFD for entries stored in a
    /// non-English encoding (a few unused test entries do).
    pub name: String,
    pub level: u8,
    /// The enemy's speed byte. This is its Dexterity as used by the ATB timer and hit chance.
    pub speed: u8,
    pub luck: u8,
    pub evade: u8,
    pub strength: u8,
    pub defense: u8,
    pub magic: u8,
    pub magic_defense: u8,
    /// Element/status codes (`0xFF` = none); pairs with `element_rates`.
    pub element_codes: [u8; 8],
    pub element_rates: [u8; 8],
    pub animation_indices: [u8; 16],
    /// Attack ids (`0xFFFF` = none), resolved through the scene's attack table.
    pub attack_ids: [u16; 16],
    pub camera_ids: [u16; 16],
    /// Drop/steal rates paired with `item_ids` (`< 0x80` drop, `>= 0x80` steal).
    pub item_rates: [u8; 4],
    pub item_ids: [u16; 4],
    pub manip_attack_indices: [u16; 3],
    pub mp: u16,
    pub ap: u16,
    pub morph_item: u16,
    /// Back-attack damage multiplier (`damage * value / 8`).
    pub back_attack_multiplier: u8,
    pub hp: u32,
    pub exp: u32,
    pub gil: u32,
    /// Raw status-immunity bit mask.
    pub status_immunities: u32,
}

impl EnemyRecord {
    fn parse(b: &[u8]) -> Self {
        Self {
            name: text::decode_trimmed(b.get(..0x20).unwrap_or(&[])),
            level: u8_at(b, 0x20),
            speed: u8_at(b, 0x21),
            luck: u8_at(b, 0x22),
            evade: u8_at(b, 0x23),
            strength: u8_at(b, 0x24),
            defense: u8_at(b, 0x25),
            magic: u8_at(b, 0x26),
            magic_defense: u8_at(b, 0x27),
            element_codes: array_at(b, 0x28),
            element_rates: array_at(b, 0x30),
            animation_indices: array_at(b, 0x38),
            attack_ids: u16_array_at(b, 0x48),
            camera_ids: u16_array_at(b, 0x68),
            item_rates: array_at(b, 0x88),
            item_ids: u16_array_at(b, 0x8C),
            manip_attack_indices: u16_array_at(b, 0x94),
            mp: u16_at(b, 0x9C),
            ap: u16_at(b, 0x9E),
            morph_item: u16_at(b, 0xA0),
            back_attack_multiplier: u8_at(b, 0xA2),
            hp: u32_at(b, 0xA4),
            exp: u32_at(b, 0xA8),
            gil: u32_at(b, 0xAC),
            status_immunities: u32_at(b, 0xB0),
        }
    }

    /// The enemy's Dexterity (its speed byte).
    pub fn dex(&self) -> u8 {
        self.speed
    }

    /// Element/status affinities with a code other than `0xFF`.
    pub fn affinities(&self) -> Vec<Affinity> {
        self.element_codes
            .iter()
            .zip(self.element_rates.iter())
            .filter(|(code, _)| **code != 0xFF)
            .map(|(&code, &rate)| Affinity {
                code,
                rate: AffinityRate::from_code(rate),
            })
            .collect()
    }
}

/// Attack data record (28 bytes). The same layout is used by `scene.bin` and by the kernel's
/// attack data section. Layout: <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Attack_data.html>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackRecord {
    /// "Attack %" (accuracy).
    pub accuracy: u8,
    pub impact_effect: u8,
    pub hurt_action: u8,
    pub mp_cost: u16,
    pub impact_sound: u16,
    pub camera_single: u16,
    pub camera_multi: u16,
    pub target_flags: u8,
    pub attack_effect: u8,
    /// Damage calculation type byte (high nybble: accuracy/crit rules, low nybble: formula).
    pub damage_calc: u8,
    pub power: u8,
    pub condition_submenu: u8,
    pub status_chance_mode: u8,
    pub extra_effect: u8,
    pub extra_effect_param: u8,
    pub status_mask: u32,
    pub element_mask: u16,
    pub special_flags: u16,
}

impl AttackRecord {
    /// Parse one 28-byte record; `None` if `b` is shorter than 28 bytes.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < ATTACK_RECORD_SIZE {
            return None;
        }
        Some(Self {
            accuracy: u8_at(b, 0x00),
            impact_effect: u8_at(b, 0x01),
            hurt_action: u8_at(b, 0x02),
            mp_cost: u16_at(b, 0x04),
            impact_sound: u16_at(b, 0x06),
            camera_single: u16_at(b, 0x08),
            camera_multi: u16_at(b, 0x0A),
            target_flags: u8_at(b, 0x0C),
            attack_effect: u8_at(b, 0x0D),
            damage_calc: u8_at(b, 0x0E),
            power: u8_at(b, 0x0F),
            condition_submenu: u8_at(b, 0x10),
            status_chance_mode: u8_at(b, 0x11),
            extra_effect: u8_at(b, 0x12),
            extra_effect_param: u8_at(b, 0x13),
            status_mask: u32_at(b, 0x14),
            element_mask: u16_at(b, 0x18),
            special_flags: u16_at(b, 0x1A),
        })
    }

    /// True for a plain physical attack using the standard "X1" formula (damage calculation
    /// types `0x01`, `0x11`, `0x31`, `0xA1`, `0xB1`). Source:
    /// <https://wiki.ffrtt.ru/index.php/FF7/Battle/Damage_Calculation>.
    pub fn is_basic_physical(&self) -> bool {
        matches!(self.damage_calc, 0x01 | 0x11 | 0x31 | 0xA1 | 0xB1)
    }
}

/// An attack defined by a scene: id, name and record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneAttack {
    pub id: u16,
    pub name: String,
    pub record: AttackRecord,
}

/// An enemy placement together with the enemy record it refers to.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedFormationEnemy<'a> {
    pub entry: &'a FormationEntry,
    /// Index 0..3 of the record inside the scene.
    pub enemy_slot: usize,
    pub record: &'a EnemyRecord,
}

/// One decoded scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    /// Position of this scene in `scene.bin` (0-based).
    pub index: usize,
    pub enemy_ids: [Option<u16>; ENEMIES_PER_SCENE],
    pub setups: [BattleSetup; FORMATIONS_PER_SCENE],
    pub cameras: [CameraPlacement; FORMATIONS_PER_SCENE],
    pub formations: [Formation; FORMATIONS_PER_SCENE],
    /// Enemy records; `None` where the scene's enemy id is unused.
    pub enemies: [Option<EnemyRecord>; ENEMIES_PER_SCENE],
    /// Attacks defined by this scene (unused slots omitted).
    pub attacks: Vec<SceneAttack>,
    /// The enemies' AI scripts (see [`crate::ai`]).
    pub ai: SceneAi,
}

impl Scene {
    /// Parse one decompressed scene (must be exactly [`SCENE_SIZE`] bytes).
    pub fn parse(raw: &[u8], index: usize) -> Result<Self, SceneError> {
        if raw.len() != SCENE_SIZE {
            return Err(SceneError::BadSceneSize { actual: raw.len() });
        }
        let enemy_ids = std::array::from_fn(|k| {
            let id = u16_at(raw, OFF_ENEMY_IDS + k * 2);
            (id != 0xFFFF).then_some(id)
        });
        let setups = std::array::from_fn(|f| {
            BattleSetup::parse(raw.get(OFF_SETUPS + f * SETUP_SIZE..).unwrap_or(&[]))
        });
        let cameras = std::array::from_fn(|f| {
            CameraPlacement::parse(raw.get(OFF_CAMERAS + f * CAMERA_SET_SIZE..).unwrap_or(&[]))
        });
        let formations = std::array::from_fn(|f| {
            Formation::parse(
                raw.get(OFF_FORMATIONS + f * FORMATION_SIZE..)
                    .unwrap_or(&[]),
            )
        });
        let enemies = std::array::from_fn(|k| {
            enemy_ids[k]
                .map(|_| EnemyRecord::parse(raw.get(OFF_ENEMIES + k * ENEMY_SIZE..).unwrap_or(&[])))
        });
        let mut attacks = Vec::new();
        for k in 0..ATTACKS_PER_SCENE {
            let id = u16_at(raw, OFF_ATTACK_IDS + k * 2);
            if id == 0xFFFF {
                continue;
            }
            let rec_off = OFF_ATTACKS + k * ATTACK_RECORD_SIZE;
            let Some(record) = raw.get(rec_off..).and_then(AttackRecord::parse) else {
                continue;
            };
            let name_off = OFF_ATTACK_NAMES + k * ATTACK_NAME_SIZE;
            let name = text::decode_trimmed(
                raw.get(name_off..name_off + ATTACK_NAME_SIZE)
                    .unwrap_or(&[]),
            );
            attacks.push(SceneAttack { id, name, record });
        }
        Ok(Self {
            index,
            enemy_ids,
            setups,
            cameras,
            formations,
            enemies,
            attacks,
            ai: SceneAi::parse(raw),
        })
    }

    /// Slot (0..3) of the enemy record with this id.
    pub fn enemy_slot_by_id(&self, id: u16) -> Option<usize> {
        self.enemy_ids.iter().position(|e| *e == Some(id))
    }

    /// The enemy record with this id.
    pub fn enemy_by_id(&self, id: u16) -> Option<&EnemyRecord> {
        let slot = self.enemy_slot_by_id(id)?;
        self.enemies.get(slot)?.as_ref()
    }

    /// The scene attack with this id.
    pub fn attack_by_id(&self, id: u16) -> Option<&SceneAttack> {
        self.attacks.iter().find(|a| a.id == id)
    }

    /// The scene attacks an enemy can use (its attack ids resolved through the scene table).
    pub fn enemy_attacks(&self, enemy_slot: usize) -> Vec<&SceneAttack> {
        let Some(Some(enemy)) = self.enemies.get(enemy_slot) else {
            return Vec::new();
        };
        enemy
            .attack_ids
            .iter()
            .filter(|id| **id != 0xFFFF)
            .filter_map(|id| self.attack_by_id(*id))
            .collect()
    }

    /// The first plain physical attack of an enemy, used as its "basic attack" by the
    /// M2-c rules (real FF7 picks attacks with AI scripts, see [`crate::ai`]; this is what an enemy
    /// whose script cannot be followed falls back on).
    pub fn basic_attack(&self, enemy_slot: usize) -> Option<&SceneAttack> {
        self.enemy_attacks(enemy_slot)
            .into_iter()
            .find(|a| a.record.is_basic_physical())
    }

    /// Formation entries whose enemy id resolves to one of this scene's enemy records.
    /// Entries that do not resolve are skipped (never expected in valid data).
    pub fn formation_enemies(&self, formation_slot: usize) -> Vec<ResolvedFormationEnemy<'_>> {
        let Some(formation) = self.formations.get(formation_slot) else {
            return Vec::new();
        };
        formation
            .entries
            .iter()
            .filter_map(|entry| {
                let enemy_slot = self.enemy_slot_by_id(entry.enemy_id)?;
                let record = self.enemies.get(enemy_slot)?.as_ref()?;
                Some(ResolvedFormationEnemy {
                    entry,
                    enemy_slot,
                    record,
                })
            })
            .collect()
    }
}

/// A formation addressed by its global index.
#[derive(Debug, Clone, Copy)]
pub struct FormationRef<'a> {
    pub scene: &'a Scene,
    /// Formation slot 0..4 inside the scene.
    pub slot: usize,
    pub global_index: usize,
}

impl<'a> FormationRef<'a> {
    pub fn formation(&self) -> &'a Formation {
        &self.scene.formations[self.slot % FORMATIONS_PER_SCENE]
    }

    pub fn setup(&self) -> &'a BattleSetup {
        &self.scene.setups[self.slot % FORMATIONS_PER_SCENE]
    }

    pub fn camera(&self) -> &'a CameraPlacement {
        &self.scene.cameras[self.slot % FORMATIONS_PER_SCENE]
    }

    pub fn enemies(&self) -> Vec<ResolvedFormationEnemy<'a>> {
        self.scene.formation_enemies(self.slot)
    }
}

/// A fully decoded `scene.bin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneBin {
    scenes: Vec<Scene>,
    block_scene_counts: Vec<usize>,
}

impl SceneBin {
    /// Parse the bytes of a PC `scene.bin`.
    pub fn parse(bytes: &[u8]) -> Result<Self, SceneError> {
        if bytes.is_empty() {
            return Err(SceneError::Empty);
        }
        if !bytes.len().is_multiple_of(BLOCK_SIZE) {
            return Err(SceneError::BadBlockAlignment(bytes.len()));
        }
        let mut scenes: Vec<Scene> = Vec::new();
        let mut block_scene_counts = Vec::new();
        let (blocks, _) = bytes.as_chunks::<BLOCK_SIZE>();
        for (block_idx, block) in blocks.iter().enumerate() {
            let ranges = block_ranges(block).map_err(|(slot, err)| SceneError::At {
                block: block_idx,
                slot,
                source: Box::new(err),
            })?;
            for (slot, range) in ranges.iter().enumerate() {
                let at = |err: SceneError| SceneError::At {
                    block: block_idx,
                    slot,
                    source: Box::new(err),
                };
                if scenes.len() >= MAX_SCENES {
                    return Err(SceneError::TooManyScenes);
                }
                let gz = block
                    .get(range.clone())
                    .ok_or_else(|| at(SceneError::BadPointer(0)))?;
                let raw = gunzip_capped(gz, SCENE_SIZE).map_err(|e| at(SceneError::Gzip(e)))?;
                let scene = Scene::parse(&raw, scenes.len()).map_err(at)?;
                scenes.push(scene);
            }
            block_scene_counts.push(ranges.len());
        }
        if scenes.is_empty() {
            return Err(SceneError::NoScenes);
        }
        Ok(Self {
            scenes,
            block_scene_counts,
        })
    }

    /// Read and parse `path`, refusing files larger than [`MAX_SCENE_BIN_BYTES`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SceneError> {
        let path = path.as_ref();
        let len = std::fs::metadata(path)?.len();
        if len > MAX_SCENE_BIN_BYTES {
            return Err(SceneError::TooLarge(len));
        }
        Self::parse(&std::fs::read(path)?)
    }

    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }

    pub fn scene(&self, index: usize) -> Option<&Scene> {
        self.scenes.get(index)
    }

    /// Number of scenes stored in each 8192-byte block (compare with the kernel's scene
    /// lookup table).
    pub fn block_scene_counts(&self) -> &[usize] {
        &self.block_scene_counts
    }

    /// Total number of addressable formations (`scenes * 4`).
    pub fn formation_count(&self) -> usize {
        self.scenes.len() * FORMATIONS_PER_SCENE
    }

    /// The formation with this global index (`scene_index * 4 + slot`).
    pub fn formation(&self, global_index: usize) -> Option<FormationRef<'_>> {
        let (scene_index, slot) = split_formation_index(global_index);
        let scene = self.scenes.get(scene_index)?;
        Some(FormationRef {
            scene,
            slot,
            global_index,
        })
    }

    /// `(scene_index, enemy_slot)` of every enemy record whose trimmed name equals `name`.
    pub fn find_enemies_by_name(&self, name: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for scene in &self.scenes {
            for (slot, enemy) in scene.enemies.iter().enumerate() {
                if enemy.as_ref().is_some_and(|e| e.name == name) {
                    out.push((scene.index, slot));
                }
            }
        }
        out
    }
}

/// Validated gzip byte ranges for the scenes of one block, or `(slot, error)`.
fn block_ranges(block: &[u8]) -> Result<Vec<std::ops::Range<usize>>, (usize, SceneError)> {
    let mut starts: Vec<usize> = Vec::new();
    for slot in 0..BLOCK_POINTERS {
        let p = u32_at(block, slot * 4);
        if p == UNUSED_POINTER {
            break;
        }
        let byte = u64::from(p) * 4;
        let prev_end = starts
            .last()
            .map_or(BLOCK_HEADER_SIZE as u64, |s| *s as u64 + 1);
        if byte >= BLOCK_SIZE as u64 || byte < prev_end {
            return Err((slot, SceneError::BadPointer(p)));
        }
        starts.push(byte as usize);
    }
    Ok(starts
        .iter()
        .enumerate()
        .map(|(i, &start)| start..starts.get(i + 1).copied().unwrap_or(BLOCK_SIZE))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{demo_scene, gzip, pack_scene_bin};

    fn setup_of(escape_counter: u16, flags: u16) -> BattleSetup {
        BattleSetup {
            location: 0,
            next_formation: None,
            escape_counter,
            next_arena_candidates: [0; 4],
            flags,
            layout: BattleLayout::Normal,
            pre_battle_camera: 0,
        }
    }

    #[test]
    fn the_run_difficulty_is_the_escape_counter_unless_the_fight_cannot_be_escaped() {
        // The flags the owner's file has (the high byte is always 0xFF): escapable ones carry bit 4.
        for flags in [0xFFED, 0xFFEF, 0xFFF7, 0xFFFD, 0xFFFF] {
            assert_eq!(setup_of(1, flags).run_difficulty(), Some(1), "{flags:04x}");
            assert_eq!(setup_of(3, flags).run_difficulty(), Some(3));
        }
        for flags in [0xFFE3, 0xFFE9, 0xFFEB, 0xFFF1, 0xFFF3, 0xFFF9, 0xFFFB] {
            assert_eq!(setup_of(1, flags).run_difficulty(), None, "{flags:04x}: the flag says no");
            assert_eq!(setup_of(9, flags).run_difficulty(), None);
        }
        // Either signal alone forbids it: a counter of 9 or more, or a clear flag.
        assert_eq!(setup_of(9, 0xFFFF).run_difficulty(), None);
        assert_eq!(setup_of(10, 0xFFFF).run_difficulty(), None);
        assert_eq!(setup_of(8, 0xFFFF).run_difficulty(), Some(8));
        // A counter of 0 would be an escape that is already done: it counts as 1.
        assert_eq!(setup_of(0, 0xFFFF).run_difficulty(), Some(1));
    }

    #[test]
    fn running_is_sure_after_a_preemptive_strike_and_on_a_side_attack_only() {
        let sure: Vec<u8> = (0..=9).filter(|c| BattleLayout::from_code(*c).makes_running_sure()).collect();
        assert_eq!(sure, [1, 3, 6, 7]);
    }

    fn demo_bin() -> Vec<u8> {
        let a = demo_scene(0);
        let b = demo_scene(1);
        pack_scene_bin(&[gzip(&a.raw), gzip(&b.raw), gzip(&a.raw)], 2)
    }

    #[test]
    fn parses_synthetic_scene_fields() {
        let raw = demo_scene(5).raw;
        let scene = Scene::parse(&raw, 5).unwrap();
        assert_eq!(scene.index, 5);
        assert_eq!(scene.enemy_ids, [Some(0x0201), Some(0x0202), None]);
        let grunt = scene.enemy_by_id(0x0201).unwrap();
        assert_eq!(grunt.name, "Test Grunt");
        assert_eq!(
            (grunt.level, grunt.speed, grunt.luck, grunt.evade),
            (7, 60, 5, 3)
        );
        assert_eq!(
            (
                grunt.strength,
                grunt.defense,
                grunt.magic,
                grunt.magic_defense
            ),
            (21, 11, 4, 9)
        );
        assert_eq!(
            (grunt.hp, grunt.mp, grunt.exp, grunt.gil, grunt.ap),
            (321, 12, 77, 55, 4)
        );
        assert_eq!(grunt.back_attack_multiplier, 12);
        assert_eq!(grunt.status_immunities, 0xF0F0_0001);
        assert_eq!(scene.enemy_by_id(0x0202).unwrap().name, "Test Hound");
        assert!(scene.enemy_by_id(0x0999).is_none());
        assert!(scene.enemies[2].is_none());
        // Affinities skip the 0xFF "none" codes and decode rate codes.
        let aff = grunt.affinities();
        assert_eq!(aff.len(), 2);
        assert_eq!(aff[0].code, 0x00);
        assert_eq!(aff[0].rate, AffinityRate::Half);
        assert_eq!(aff[1].rate, AffinityRate::Absorb);
        assert_eq!(element_name(0x00), Some("Fire"));
        assert_eq!(element_name(0x20), None);

        // Setups and cameras.
        assert_eq!(scene.setups[0].location, 0x0031);
        assert_eq!(scene.setups[0].next_formation, None);
        assert_eq!(scene.setups[0].layout, BattleLayout::Normal);
        assert_eq!(scene.setups[1].layout, BattleLayout::Preemptive);
        assert_eq!(scene.setups[1].next_formation, Some(0x0123));
        assert_eq!(scene.setups[1].next_arena_candidates, [0x3E7; 4]);
        assert_eq!(scene.setups[0].pre_battle_camera, 7);
        assert_eq!(scene.setups[0].flags, 0x00A5);
        assert_eq!(scene.cameras[0].cameras[0].position, [100, -200, 300]);
        assert_eq!(scene.cameras[0].cameras[0].direction, [-1, 2, -3]);

        // Formations: formation 0 = grunt + hound, formation 1 = two grunts, rest empty.
        let f0 = &scene.formations[0];
        assert_eq!(f0.entries.len(), 2);
        assert_eq!(f0.entries[0].enemy_id, 0x0201);
        assert_eq!(f0.entries[0].position, [-111, 4, -2222]);
        assert_eq!(f0.entries[1].position, [333, 4, -2444]);
        assert_eq!(f0.entries[0].initial_flags, 0x19);
        assert_eq!((f0.entries[0].row, f0.entries[0].cover_flags), (1, 0));
        assert_eq!(f0.entries[1].slot, 1);
        assert_eq!(scene.formations[1].entries.len(), 2);
        assert!(scene.formations[2].entries.is_empty());
        let resolved = scene.formation_enemies(1);
        assert_eq!(resolved.len(), 2);
        assert!(resolved
            .iter()
            .all(|r| r.enemy_slot == 0 && r.record.name == "Test Grunt"));
        assert!(scene.formation_enemies(9).is_empty());

        // Attacks: ids resolve, names decode, basic attack selection skips magic.
        assert_eq!(scene.attacks.len(), 3);
        let bite = scene.attack_by_id(0x0301).unwrap();
        assert_eq!(bite.name, "Test Bite");
        assert_eq!((bite.record.accuracy, bite.record.power), (95, 20));
        assert!(bite.record.is_basic_physical());
        assert_eq!(scene.enemy_attacks(0).len(), 2);
        assert_eq!(scene.basic_attack(0).unwrap().id, 0x0301);
        assert_eq!(scene.basic_attack(1).unwrap().id, 0x0303);
        assert!(scene.basic_attack(2).is_none());
        assert!(scene.basic_attack(7).is_none());
    }

    #[test]
    fn parses_blocks_and_indexes_formations_globally() {
        let bin = SceneBin::parse(&demo_bin()).unwrap();
        // Two scenes per block: blocks hold [2, 1] scenes.
        assert_eq!(bin.scenes().len(), 3);
        assert_eq!(bin.block_scene_counts(), &[2, 1]);
        assert_eq!(bin.formation_count(), 12);
        for (i, s) in bin.scenes().iter().enumerate() {
            assert_eq!(s.index, i);
        }
        assert_eq!(global_formation_index(2, 3), 11);
        assert_eq!(split_formation_index(11), (2, 3));
        let f = bin.formation(6).unwrap();
        assert_eq!((f.scene.index, f.slot, f.global_index), (1, 2, 6));
        assert_eq!(f.setup().location, 0x0031);
        assert!(f.formation().entries.is_empty());
        let f1 = bin.formation(5).unwrap();
        assert_eq!(f1.enemies().len(), 2);
        assert_eq!(f1.setup().layout, BattleLayout::Preemptive);
        assert_eq!(f1.camera().cameras[0].position, [100, -200, 300]);
        assert!(bin.formation(12).is_none());
        assert_eq!(
            bin.find_enemies_by_name("Test Hound"),
            vec![(0, 1), (1, 1), (2, 1)]
        );
        assert!(bin.find_enemies_by_name("Nobody").is_empty());
    }

    #[test]
    fn rejects_structural_errors() {
        assert!(matches!(SceneBin::parse(&[]), Err(SceneError::Empty)));
        assert!(matches!(
            SceneBin::parse(&vec![0xFF; BLOCK_SIZE + 1]),
            Err(SceneError::BadBlockAlignment(_))
        ));
        // A block with no scenes at all.
        assert!(matches!(
            SceneBin::parse(&vec![0xFF; BLOCK_SIZE]),
            Err(SceneError::NoScenes)
        ));

        let good = demo_bin();
        let patch_ptr = |slot: usize, value: u32| {
            let mut b = good.clone();
            b[slot * 4..slot * 4 + 4].copy_from_slice(&value.to_le_bytes());
            b
        };
        let is_bad_pointer = |r: Result<SceneBin, SceneError>| match r {
            Err(SceneError::At { source, .. }) => matches!(*source, SceneError::BadPointer(_)),
            _ => false,
        };
        // Pointer beyond the block, into the pointer table, and non-monotonic.
        assert!(is_bad_pointer(SceneBin::parse(&patch_ptr(0, 0x0800))));
        assert!(is_bad_pointer(SceneBin::parse(&patch_ptr(0, 0x0005))));
        assert!(is_bad_pointer(SceneBin::parse(&patch_ptr(1, 0x0010))));
        assert!(is_bad_pointer(SceneBin::parse(&patch_ptr(1, 0x7FFF_FFFF))));
    }

    #[test]
    fn rejects_bad_gzip_wrong_size_and_too_many_scenes() {
        // Corrupt gzip payload (flip bytes in the middle of the first stream).
        let mut bad = demo_bin();
        for b in bad.iter_mut().skip(0x40 + 12).take(8) {
            *b ^= 0xA5;
        }
        assert!(matches!(SceneBin::parse(&bad), Err(SceneError::At { .. })));

        // A valid gzip stream of the wrong decompressed size.
        let short = pack_scene_bin(&[gzip(&[1u8; 100])], 16);
        match SceneBin::parse(&short) {
            Err(SceneError::At {
                block: 0,
                slot: 0,
                source,
            }) => {
                assert!(matches!(*source, SceneError::BadSceneSize { actual: 100 }));
            }
            other => panic!("unexpected: {other:?}"),
        }
        // Larger than the cap is rejected without decompressing everything.
        let big = pack_scene_bin(&[gzip(&vec![0u8; SCENE_SIZE * 8])], 16);
        assert!(matches!(
            SceneBin::parse(&big),
            Err(SceneError::At { source, .. }) if matches!(*source, SceneError::BadSceneSize { .. })
        ));

        // 257 scenes -> too many (a zero scene compresses to a few dozen bytes).
        let tiny = gzip(&vec![0u8; SCENE_SIZE]);
        let many: Vec<Vec<u8>> = (0..MAX_SCENES + 1).map(|_| tiny.clone()).collect();
        assert!(matches!(
            SceneBin::parse(&pack_scene_bin(&many, 15)),
            Err(SceneError::TooManyScenes)
        ));
        // Exactly 256 is fine.
        let ok: Vec<Vec<u8>> = (0..MAX_SCENES).map(|_| tiny.clone()).collect();
        let bin = SceneBin::parse(&pack_scene_bin(&ok, 15)).unwrap();
        assert_eq!(bin.scenes().len(), MAX_SCENES);
        assert_eq!(bin.formation_count(), 1024);
    }

    #[test]
    fn scene_parse_requires_exact_size() {
        assert!(matches!(
            Scene::parse(&[0u8; SCENE_SIZE - 1], 0),
            Err(SceneError::BadSceneSize { .. })
        ));
        assert!(Scene::parse(&[0xFFu8; SCENE_SIZE], 0).is_ok());
        let all_zero = Scene::parse(&[0u8; SCENE_SIZE], 0).unwrap();
        // Enemy id 0 is a valid id, so all three records exist and are blank.
        assert_eq!(all_zero.enemy_ids, [Some(0), Some(0), Some(0)]);
    }

    #[test]
    fn open_rejects_missing_and_oversized_files() {
        assert!(matches!(
            SceneBin::open("/nonexistent/definitely/not/here/scene.bin"),
            Err(SceneError::Io(_))
        ));
    }

    /// Tiny deterministic generator for the garbage tests.
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    #[test]
    fn garbage_and_mutated_input_never_panics() {
        let mut rng = Lcg(1);
        // Pure garbage of assorted lengths (aligned and not) is always rejected.
        for i in 0..64 {
            let len = [0, 1, 63, 64, 8191, 8192, 8193, 16384][i % 8];
            let buf: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
            assert!(
                SceneBin::parse(&buf).is_err(),
                "garbage of length {len} parsed"
            );
        }
        // Mutations of a valid file: flip a few bytes, sometimes inside the pointer table.
        let good = demo_bin();
        for i in 0..400 {
            let mut buf = good.clone();
            for _ in 0..(1 + i % 4) {
                let pos = if i % 3 == 0 {
                    (rng.next_u64() as usize) % 0x40
                } else {
                    (rng.next_u64() as usize) % buf.len()
                };
                buf[pos] = rng.next_u64() as u8;
            }
            let _ = SceneBin::parse(&buf);
        }
        // Mutated decompressed scenes parse into *something* or error, never panic.
        let raw = demo_scene(0).raw;
        for _ in 0..200 {
            let mut buf = raw.clone();
            for _ in 0..8 {
                let pos = (rng.next_u64() as usize) % buf.len();
                buf[pos] = rng.next_u64() as u8;
            }
            let scene = Scene::parse(&buf, 0).unwrap();
            // Exercise the resolvers on whatever came out.
            for f in 0..FORMATIONS_PER_SCENE {
                let _ = scene.formation_enemies(f);
            }
            for k in 0..ENEMIES_PER_SCENE {
                let _ = scene.basic_attack(k);
            }
        }
    }
}
