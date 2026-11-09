//! PC `kernel.bin` / `kernel2.bin` parser: commands, battle and growth data, starting data.
//!
//! # `kernel.bin`
//!
//! Sources:
//! * Overview: <https://wiki.ffrtt.ru/index.php?title=FF7%2FKernel%2FKernel.bin>
//! * Section header format ("BIN-GZIP"):
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Kernel/Low_level_libraries.html>
//! * Section 0: <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Command_data.html>
//! * Section 1: <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Attack_data.html>
//! * Section 2: <https://wiki.ffrtt.ru/index.php/FF7/Battle_and_growth_data>
//! * Section 3 (same record format as the savemap character record):
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Character_starting_stats.html> and
//!   <https://wiki.ffrtt.ru/index.php/FF7/Savemap>
//! * Sections 5 and 6: <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Weapon_data.html> and
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Armor_data.html>
//! * Section 4 (items, 28-byte records; the flat wiki counts sections from 1, so it calls it
//!   section 5): <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Item_data.html>
//! * Section 8 (materia, 20-byte records; "section 9" on the flat wiki):
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Materia_data.html> and the type byte at
//!   <https://wiki.ffrtt.ru/index.php/FF7/Materia_Types>
//! * Targeting bits: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data>
//! * Special attack flags (active low): <https://wiki.ffrtt.ru/index.php/FF7/Battle/Special_Attack_Flags>
//!
//! The file is a run of gzip sections, each preceded by a 6-byte little-endian header:
//! `u16` compressed size, `u16` decompressed size, `u16` file type (0..=8 for the data
//! sections, 9 for the text sections). The vanilla file has 27 sections followed by a couple
//! of trailing padding bytes, which are tolerated.
//!
//! | section | content | handled |
//! |---|---|---|
//! | 0 | 32 command records of 8 bytes | parsed ([`CommandRecord`]) |
//! | 1 | 128 attack records of 28 bytes | on demand ([`Kernel::attack`]) |
//! | 2 | battle and growth data | parsed ([`GrowthData`]) |
//! | 3 | initialization data (9 savemap character records, then the party, then the item stock) | parsed ([`InitCharacter`], [`Kernel::starting_inventory`]) |
//! | 4 | 128 item records of 28 bytes | on demand ([`Kernel::item`]) |
//! | 5, 6 | weapons (44 bytes), armor (36 bytes) | on demand ([`WeaponRecord`], [`WeaponSounds`], [`ArmorRecord`]) |
//! | 8 | 96 materia records of 20 bytes | on demand ([`Kernel::materia`]) |
//! | 7 | accessories | raw bytes only |
//! | 9..27 | FF text tables | raw bytes; name lookup via [`Kernel::text_entry`] |
//!
//! Raw sections are available through [`Kernel::section`].
//!
//! ## What was checked against the owner's Steam copy
//!
//! The ignored tests in `tests/real_data.rs` check, on the owner's own files, that the attack index
//! of the spells is the index of the "Names - magic" text table (Cure 0, Cure2 1, Cure3 2, Life 7,
//! Fire 27, Ice 30, Bolt 33, ...), that item 0 is Potion (a flat heal of 20 x Power = 100 HP), 3 Ether
//! (the same, on MP), 7 Phoenix Down, that the materia of the starting records are Lightning and Ice
//! (Cloud) and Fire (Red XIII) and that the starting stock is Potion x3, Ether x1 and Phoenix Down x1.
//!
//! ## Stat derivation (APPROXIMATION)
//!
//! [`GrowthData::stat_block`] returns a *deterministic expected* stat block for a character
//! at a level. The real game rolls random level-up bonuses per character, so two real
//! characters of equal level differ slightly; this closed form is the baseline curve the
//! level-up routine tracks (real stats lag a little below it). It is identical on every
//! machine, which matters because server and clients each derive party stats from their own
//! copy of the owner's files. It evaluates the 16-byte stat curve of the band containing
//! the level:
//!
//! * primary stat: `base + gradient * level / 100`
//! * HP: `40 * base + level * gradient` (base is signed)
//! * MP: `2 * base + level * gradient / 10` (base is signed)
//!
//! The wiki writes the HP/MP baselines with `(level - 1)` and gives the MP base multiplier
//! as 40 in one place and 2 in another. The forms above (evaluated at `level`, MP base x2)
//! are the ones consistent with the shipped data: the bands of a curve join approximately
//! continuously, at level 1 they reproduce the HP and MP stored in the populated starting
//! records of the characters that join later, and Cloud's own starting record agrees for
//! Strength, Vitality, Magic, Spirit and HP (his Dexterity, Luck and MP differ slightly,
//! consistent with hand-tuned data). Random per-level rolls are not modelled, and equipment stat
//! bonuses are not applied either.
//!
//! ## Experience curves
//!
//! [`GrowthData::exp_for_level`], [`GrowthData::exp_to_next_level`] and
//! [`GrowthData::level_for_exp`] turn a character's EXP curve (one of curves 55 to 63) into the
//! experience thresholds of levels 2 to 99: `modifier * level^2 / 10` to go from `level` to
//! `level + 1`, with the modifier of the band of the level being attained. This one is the
//! documented rule and is checked against the owner's file (Cloud: 6, 33, 94, 202, 372, 616 for
//! levels 2 to 7), not an invention; see [`GrowthData::exp_for_level`] for the source and the
//! one remaining APPROXIMATION. [`MAX_LEVEL`], [`MAX_EXP`], [`MAX_GIL`] and [`MAX_AP`] are the
//! game's own caps for what the battle rewards add up to.
//!
//! # `kernel2.bin`
//!
//! The PC-only `kernel2.bin` holds the text sections, LZSS-compressed behind a 4-byte
//! length header. [`decode_kernel2`] decodes it to one blob. The section boundaries are not
//! recorded in the file and the Steam edition's text differs from the text sections still
//! present in `kernel.bin`, so the blob is not split into sections here.
//!
//! # Safety on untrusted input
//!
//! Section count, per-section decompressed size (a `u16`, enforced through a capped gzip
//! read) and the `kernel2.bin` output size are all bounded; field reads are total.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bytes::{array_at, gunzip_capped, i8_at, u16_at, u32_at, u8_at};
use crate::scene::{AttackRecord, ATTACK_RECORD_SIZE};
use crate::text;

/// Bytes in a section header.
pub const SECTION_HEADER_SIZE: usize = 6;
/// Maximum number of sections accepted (vanilla has 27).
pub const MAX_SECTIONS: usize = 64;
/// Largest `kernel.bin` [`Kernel::open`] reads.
pub const MAX_KERNEL_BYTES: u64 = 8 * 1024 * 1024;
/// Largest `kernel2.bin` text blob [`decode_kernel2`] will produce.
pub const MAX_KERNEL2_DECODED: usize = 1 << 20;

pub const SECTION_COMMANDS: usize = 0;
pub const SECTION_ATTACKS: usize = 1;
pub const SECTION_BATTLE_AND_GROWTH: usize = 2;
pub const SECTION_INITIALIZATION: usize = 3;
pub const SECTION_ITEMS: usize = 4;
pub const SECTION_WEAPONS: usize = 5;
pub const SECTION_ARMOR: usize = 6;
pub const SECTION_ACCESSORIES: usize = 7;
pub const SECTION_MATERIA: usize = 8;
/// "Names - commands" text table.
pub const SECTION_TEXT_COMMAND_NAMES: usize = 17;
/// "Names - magic" text table: string `i` names attack `i` of section 1 for the spells.
pub const SECTION_TEXT_MAGIC_NAMES: usize = 18;
/// "Names - items" text table: string `i` names item `i` of section 4.
pub const SECTION_TEXT_ITEM_NAMES: usize = 19;
/// "Names - weapons" text table.
pub const SECTION_TEXT_WEAPON_NAMES: usize = 20;
/// "Names - armor" text table.
pub const SECTION_TEXT_ARMOR_NAMES: usize = 21;
/// "Names - materia" text table: string `i` names materia `i` of section 8.
pub const SECTION_TEXT_MATERIA_NAMES: usize = 23;

/// Command index of the Attack command.
pub const COMMAND_ATTACK: usize = 1;

pub(crate) const COMMAND_RECORD_SIZE: usize = 8;
pub(crate) const COMMAND_COUNT: usize = 32;
pub(crate) const WEAPON_RECORD_SIZE: usize = 44;
pub(crate) const ARMOR_RECORD_SIZE: usize = 36;
pub(crate) const INIT_CHARACTER_SIZE: usize = 0x84;
pub(crate) const CHARACTER_COUNT: usize = 9;
pub(crate) const ITEM_RECORD_SIZE: usize = 28;
pub(crate) const MATERIA_RECORD_SIZE: usize = 20;
/// Items in the kernel's item table (section 4). The savemap's 9-bit item index also reaches the
/// weapons, armor and accessories (128 and up), which the inventory here does not hold.
pub const ITEM_COUNT: usize = 128;
/// Materia in the kernel's materia table (section 8).
pub const MATERIA_COUNT: usize = 96;
/// The game's own cap on one stack of an item (the savemap stores a count in 7 bits; the game stops at 99).
pub const MAX_ITEM_STACK: u16 = 99;
/// Slots of the item stock in the initialization data (the savemap's 320 item slots).
pub const INIT_ITEM_SLOTS: usize = 320;
/// Materia slots of a character record: eight on the weapon, eight on the armor, one after the other.
pub(crate) const INIT_MATERIA_SLOTS: usize = 16;
pub(crate) const OFF_INIT_MATERIA: usize = 0x40;
/// The item stock follows the nine character records and four party bytes (three slots and a pad).
pub(crate) const OFF_INIT_ITEMS: usize = CHARACTER_COUNT * INIT_CHARACTER_SIZE + 4;
/// Attack index of the last spell Master Magic unlocks (Shield; the menu hides the two after it).
const LAST_MASTER_MAGIC_SPELL: u16 = 0x35;
pub(crate) const GROWTH_RECORD_SIZE: usize = 56;
const GAIN_TABLE_LEN: usize = 12;
pub(crate) const OFF_STAT_GAIN: usize = 0x1F8;
pub(crate) const OFF_HP_GAIN: usize = 0x204;
pub(crate) const OFF_MP_GAIN: usize = 0x210;
pub(crate) const OFF_CURVES: usize = 0x21C;
pub(crate) const CURVE_SIZE: usize = 16;
pub(crate) const CURVE_COUNT: usize = 64;
pub(crate) const OFF_SCENE_LOOKUP: usize = 0xF1C;
const SCENE_LOOKUP_LEN: usize = 64;
const GROWTH_MIN_SIZE: usize = OFF_SCENE_LOOKUP + SCENE_LOOKUP_LEN;

const _: () = {
    assert!(OFF_CURVES + CURVE_COUNT * CURVE_SIZE == 0x61C);
    assert!(CHARACTER_COUNT * GROWTH_RECORD_SIZE == OFF_STAT_GAIN);
};

/// Errors from parsing kernel files.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("kernel file is empty")]
    Empty,
    #[error("kernel file is {0} bytes, which exceeds the supported maximum")]
    TooLarge(u64),
    #[error("section {section}: header or data runs past the end of the file (offset {offset})")]
    Truncated { section: usize, offset: usize },
    #[error("section {section}: gzip error: {source}")]
    Gzip {
        section: usize,
        #[source]
        source: std::io::Error,
    },
    #[error("section {section}: decompressed to {actual} bytes, header says {expected}")]
    SizeMismatch {
        section: usize,
        expected: usize,
        actual: usize,
    },
    #[error("kernel file has more than 64 sections")]
    TooManySections,
    #[error("section {0} is missing")]
    MissingSection(usize),
    #[error("section {section} is {actual} bytes, need at least {expected}")]
    BadSectionSize {
        section: usize,
        expected: usize,
        actual: usize,
    },
    #[error("kernel2.bin: {0}")]
    Kernel2(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The nine playable characters, in the order of the kernel's character records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CharacterId {
    Cloud,
    Barret,
    Tifa,
    Aeris,
    RedXiii,
    Yuffie,
    CaitSith,
    Vincent,
    Cid,
}

impl CharacterId {
    pub const ALL: [CharacterId; CHARACTER_COUNT] = [
        CharacterId::Cloud,
        CharacterId::Barret,
        CharacterId::Tifa,
        CharacterId::Aeris,
        CharacterId::RedXiii,
        CharacterId::Yuffie,
        CharacterId::CaitSith,
        CharacterId::Vincent,
        CharacterId::Cid,
    ];

    /// Index into the growth-data character records (also the savemap character id).
    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Cloud => "Cloud",
            Self::Barret => "Barret",
            Self::Tifa => "Tifa",
            Self::Aeris => "Aerith",
            Self::RedXiii => "Red XIII",
            Self::Yuffie => "Yuffie",
            Self::CaitSith => "Cait Sith",
            Self::Vincent => "Vincent",
            Self::Cid => "Cid",
        }
    }
}

/// One decoded kernel section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSection {
    /// File type from the section header (0..=8 data, 9 text).
    pub file_type: u16,
    /// Decompressed bytes.
    pub data: Vec<u8>,
}

/// Command record (section 0, 8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRecord {
    /// What choosing the command does (0 = run now, 7 = pick a target, others open submenus).
    pub cursor_action: u8,
    /// Targeting flags (`0xFF` = use the attack's or weapon's flags).
    pub target_flags: u8,
    pub camera_single: u16,
    pub camera_multi: u16,
}

impl CommandRecord {
    fn parse(b: &[u8]) -> Self {
        Self {
            cursor_action: u8_at(b, 0),
            target_flags: u8_at(b, 1),
            camera_single: u16_at(b, 4),
            camera_multi: u16_at(b, 6),
        }
    }
}

/// One 16-byte stat curve: eight (gradient, base) pairs, one per level band.
///
/// Bands (level reached): 2-11, 12-21, 22-31, 32-41, 42-51, 52-61, 62-81, 82-99. Level 1 uses
/// the first band. For HP and MP curves the base is signed; EXP curves store the experience
/// modifier in the gradient byte and leave the base at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatCurve {
    pub gradients: [u8; 8],
    pub bases: [i8; 8],
}

impl StatCurve {
    fn parse(b: &[u8]) -> Self {
        let mut gradients = [0u8; 8];
        let mut bases = [0i8; 8];
        for band in 0..8 {
            gradients[band] = u8_at(b, band * 2);
            bases[band] = i8_at(b, band * 2 + 1);
        }
        Self { gradients, bases }
    }

    /// `(gradient, base)` of the band containing `level`.
    pub fn band(&self, level: u8) -> (u8, i8) {
        let band = curve_band(level);
        (
            self.gradients.get(band).copied().unwrap_or(0),
            self.bases.get(band).copied().unwrap_or(0),
        )
    }
}

/// Index (0..8) of the curve band containing `level`.
pub const fn curve_band(level: u8) -> usize {
    match level {
        0..=11 => 0,
        12..=21 => 1,
        22..=31 => 2,
        32..=41 => 3,
        42..=51 => 4,
        52..=61 => 5,
        62..=81 => 6,
        _ => 7,
    }
}

/// Per-character record of the growth data (56 bytes): which curve each stat follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterGrowth {
    pub strength_curve: u8,
    pub vitality_curve: u8,
    pub magic_curve: u8,
    pub spirit_curve: u8,
    pub dexterity_curve: u8,
    pub luck_curve: u8,
    pub hp_curve: u8,
    pub mp_curve: u8,
    pub exp_curve: u8,
    /// Signed level modifier (byte `0x0A`). The wiki labels this "initial level"; the
    /// shipped values are small signed numbers (negative and positive), consistent with a
    /// level offset applied when the character joins the party.
    pub level_modifier: i8,
    /// The limit-break fields of the record (bytes `0x0C..0x38`).
    pub limits: CharacterLimits,
    /// The whole record, raw.
    pub raw: [u8; GROWTH_RECORD_SIZE],
}

impl CharacterGrowth {
    fn parse(b: &[u8]) -> Self {
        Self {
            strength_curve: u8_at(b, 0),
            vitality_curve: u8_at(b, 1),
            magic_curve: u8_at(b, 2),
            spirit_curve: u8_at(b, 3),
            dexterity_curve: u8_at(b, 4),
            luck_curve: u8_at(b, 5),
            hp_curve: u8_at(b, 6),
            mp_curve: u8_at(b, 7),
            exp_curve: u8_at(b, 8),
            level_modifier: i8_at(b, 0x0A),
            limits: CharacterLimits::parse(b),
            raw: array_at(b, 0),
        }
    }
}

/// The limit-break fields of a character's 56-byte growth record.
///
/// Layout (<https://ff7-mods.github.io/ff7-flat-wiki/FF7/Battle_and_growth_data.html>; the page names
/// the fields and gives no meaning for their values, which are read here from the owner's Steam copy
/// and checked by `tests/real_data.rs`):
///
/// | offset | size | field |
/// |---|---|---|
/// | `0x0C..0x18` | 4 x 3 bytes | the limit commands of limit levels 1 to 4, three slots each: the id of the limit in the "Names - magic" table (`0xFF`: no limit in that slot; the third slot of levels 1 to 3 and the second and third of level 4 are never used) |
/// | `0x18`, `0x1A` | 2 x u16 | the total kills needed to reach limit level 2 and 3 |
/// | `0x1C`, `0x20`, `0x24` | 3 x u16 | the uses of a level's first limit needed to learn its second (1-2, 2-2, 3-2) |
/// | `0x28..0x38` | 4 x u32 | the "HP divisor" of limit levels 1 to 4: how slowly the limit gauge fills at that level |
///
/// Checked against the Limit Break Mechanics guide by GideonD (a copy of its requirements table:
/// Cloud 8 uses for 1-2, 120 kills for level 2, 7 uses for 2-2, 320 kills in all for level 3, 6 uses for
/// 3-2) which this record reproduces for the characters that guide lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CharacterLimits {
    /// The limit ids by `[limit level - 1][slot]` (`None`: the slot holds no limit).
    pub commands: [[Option<u8>; 3]; 4],
    /// Total kills needed to reach limit level 2 and 3.
    pub kills: [u16; 2],
    /// Uses of the first limit of level 1, 2 and 3 needed to learn the level's second.
    pub uses: [u16; 3],
    /// The HP divisor of limit levels 1 to 4 (see [`crate::limit`]).
    pub divisors: [u32; 4],
}

impl CharacterLimits {
    fn parse(b: &[u8]) -> Self {
        // A slot past the end of a short record holds nothing (reads are total, and 0 is a real id).
        let slot = |level: usize, slot: usize| b.get(0x0C + level * 3 + slot).copied().filter(|id| *id != 0xFF);
        Self {
            commands: std::array::from_fn(|level| std::array::from_fn(|s| slot(level, s))),
            kills: [u16_at(b, 0x18), u16_at(b, 0x1A)],
            uses: [u16_at(b, 0x1C), u16_at(b, 0x20), u16_at(b, 0x24)],
            divisors: [u32_at(b, 0x28), u32_at(b, 0x2C), u32_at(b, 0x30), u32_at(b, 0x34)],
        }
    }

    /// The first limit of limit level 1: the one every character starts with (Braver, Big Shot, ...).
    pub fn first(&self) -> Option<u8> {
        self.commands[0][0]
    }

    /// The HP divisor of limit level 1, if the character has a limit at that level.
    pub fn divisor_at_level_one(&self) -> Option<u32> {
        self.first().map(|_| self.divisors[0]).filter(|d| *d > 0)
    }
}

/// A level-appropriate character stat block. See the module docs for how it is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatBlock {
    pub level: u8,
    pub strength: u16,
    pub vitality: u16,
    pub magic: u16,
    pub spirit: u16,
    pub dexterity: u16,
    pub luck: u16,
    pub max_hp: u16,
    pub max_mp: u16,
}

/// Maximum HP of a character.
pub const MAX_CHARACTER_HP: i32 = 9999;
/// Maximum MP of a character.
pub const MAX_CHARACTER_MP: i32 = 999;

/// Highest character level (the stat curves end at 99).
pub const MAX_LEVEL: u8 = 99;
/// Most experience a character can hold. The savemap field is a `u32`; the game itself stops at
/// 99,999,999. Level 99 needs only about 2.4 million, so the cap is never what limits a level.
pub const MAX_EXP: u32 = 99_999_999;
/// Most gil the party can hold (the game's own cap).
pub const MAX_GIL: u32 = 9_999_999;
/// Most AP one counter can hold: the savemap stores a materia's AP in three bytes.
pub const MAX_AP: u32 = 0x00FF_FFFF;

/// Battle and growth data (section 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrowthData {
    pub characters: Vec<CharacterGrowth>,
    /// Primary-stat gain per level-up, indexed by the capped stat difference (unused by the
    /// closed-form [`GrowthData::stat_block`], kept for completeness).
    pub stat_gain_table: [u8; GAIN_TABLE_LEN],
    pub hp_gain_percent: [u8; GAIN_TABLE_LEN],
    pub mp_gain_percent: [u8; GAIN_TABLE_LEN],
    /// All 64 curves: 0..=36 primary stats, 37..=45 HP, 46..=54 MP, 55..=63 EXP.
    pub curves: Vec<StatCurve>,
    /// scene.bin block index -> index of the first scene in that block (`0xFF` = unused).
    pub scene_lookup: [u8; SCENE_LOOKUP_LEN],
}

impl GrowthData {
    /// Parse section 2.
    pub fn parse(section: &[u8]) -> Result<Self, KernelError> {
        if section.len() < GROWTH_MIN_SIZE {
            return Err(KernelError::BadSectionSize {
                section: SECTION_BATTLE_AND_GROWTH,
                expected: GROWTH_MIN_SIZE,
                actual: section.len(),
            });
        }
        let characters = (0..CHARACTER_COUNT)
            .map(|i| CharacterGrowth::parse(section.get(i * GROWTH_RECORD_SIZE..).unwrap_or(&[])))
            .collect();
        let curves = (0..CURVE_COUNT)
            .map(|i| StatCurve::parse(section.get(OFF_CURVES + i * CURVE_SIZE..).unwrap_or(&[])))
            .collect();
        Ok(Self {
            characters,
            stat_gain_table: array_at(section, OFF_STAT_GAIN),
            hp_gain_percent: array_at(section, OFF_HP_GAIN),
            mp_gain_percent: array_at(section, OFF_MP_GAIN),
            curves,
            scene_lookup: array_at(section, OFF_SCENE_LOOKUP),
        })
    }

    pub fn curve(&self, index: u8) -> Option<&StatCurve> {
        self.curves.get(usize::from(index))
    }

    /// First-scene index of every used `scene.bin` block, in block order.
    pub fn scene_block_starts(&self) -> Vec<u8> {
        self.scene_lookup
            .iter()
            .copied()
            .take_while(|b| *b != 0xFF)
            .collect()
    }

    /// Deterministic expected stat block for `who` at `level` (clamped to 1..=99).
    /// Returns `None` if the character's record or curves are missing.
    pub fn stat_block(&self, who: CharacterId, level: u8) -> Option<StatBlock> {
        let rec = self.characters.get(who.index())?;
        let level = level.clamp(1, 99);
        let lvl = i32::from(level);
        let primary = |curve_index: u8| -> Option<u16> {
            let (gradient, base) = self.curve(curve_index)?.band(level);
            let v = i32::from(base) + i32::from(gradient) * lvl / 100;
            Some(v.clamp(0, 255) as u16)
        };
        let (hp_gradient, hp_base) = self.curve(rec.hp_curve)?.band(level);
        let (mp_gradient, mp_base) = self.curve(rec.mp_curve)?.band(level);
        let hp = 40 * i32::from(hp_base) + lvl * i32::from(hp_gradient);
        let mp = 2 * i32::from(mp_base) + lvl * i32::from(mp_gradient) / 10;
        Some(StatBlock {
            level,
            strength: primary(rec.strength_curve)?,
            vitality: primary(rec.vitality_curve)?,
            magic: primary(rec.magic_curve)?,
            spirit: primary(rec.spirit_curve)?,
            dexterity: primary(rec.dexterity_curve)?,
            luck: primary(rec.luck_curve)?,
            max_hp: hp.clamp(1, MAX_CHARACTER_HP) as u16,
            max_mp: mp.clamp(0, MAX_CHARACTER_MP) as u16,
        })
    }

    /// The experience modifier of `who` for attaining `level_reached`: the gradient byte of the
    /// character's EXP curve in the band that contains that level (the base byte of an EXP curve
    /// is unused). `None` if the character or its EXP curve is missing.
    pub fn exp_modifier(&self, who: CharacterId, level_reached: u8) -> Option<u8> {
        let rec = self.characters.get(who.index())?;
        let (gradient, _unused_base) = self.curve(rec.exp_curve)?.band(level_reached);
        Some(gradient)
    }

    /// Experience needed to go from `level` (1..=98) to `level + 1`: `modifier * level^2 / 10`
    /// in integer arithmetic, where the modifier is that of the level being attained.
    fn exp_step(&self, who: CharacterId, level: u8) -> Option<u32> {
        let modifier = u32::from(self.exp_modifier(who, level.saturating_add(1))?);
        let level = u32::from(level);
        Some(modifier * level * level / 10)
    }

    /// Experience needed to go from `level` to `level + 1` (`Some(0)` at [`MAX_LEVEL`], where
    /// there is no next level; `level` is clamped to `1..=`[`MAX_LEVEL`]). `None` if the kernel
    /// has no EXP curve for the character.
    pub fn exp_to_next_level(&self, who: CharacterId, level: u8) -> Option<u32> {
        let level = level.clamp(1, MAX_LEVEL);
        if level >= MAX_LEVEL {
            // Still `None` for a missing character, like every other level.
            self.exp_modifier(who, level)?;
            return Some(0);
        }
        self.exp_step(who, level)
    }

    /// Total experience at which `who` reaches `level` (clamped to `1..=`[`MAX_LEVEL`]; level 1
    /// is 0): the sum of [`GrowthData::exp_to_next_level`] over the levels below it. `None` if
    /// the kernel has no EXP curve for the character.
    ///
    /// # Source and checks
    ///
    /// The EXP curves are the nine curves 55 to 63 of section 2 (byte `0x08` of a character's
    /// 56-byte record selects one), laid out like the other curves with the experience modifier in
    /// the gradient byte of each of the eight level bands and the base byte unused (zero in the
    /// owner's file). The rule is the one the community documents
    /// (<https://wiki.ffrtt.ru/index.php/FF7/Battle_and_growth_data>):
    /// `needed = modifier * level^2 / 10` to go from `level` to `level + 1`, with the modifier of
    /// the band of the level being attained. Where the page's worked example truncates `level^2 /
    /// 10` before multiplying, this does the multiplication first: that is what gives Cloud's
    /// well-known thresholds 6, 33, 94 and 202 EXP for levels 2 to 5 (his band 0 modifier is 68),
    /// and 372 and 616 for 6 and 7. Checked on the owner's Steam copy by the ignored test
    /// `real_exp_curves_reproduce_the_known_thresholds` in `tests/real_data.rs` (Cloud's own
    /// starting record, level 6 with 610 EXP, sits just below the level 7 threshold, as it should).
    ///
    /// APPROXIMATION: the savemap also stores a per-character "EXP to next level" counter that the
    /// game recomputes only when it levels up; Cloud's initial record holds 35 there where the
    /// curve says 6, a hand-set start value that is not modelled. Everything after the first
    /// level-up follows the curve.
    pub fn exp_for_level(&self, who: CharacterId, level: u8) -> Option<u32> {
        let level = level.clamp(1, MAX_LEVEL);
        let mut total = 0u32;
        for k in 1..level {
            total = total.saturating_add(self.exp_step(who, k)?);
        }
        // Reject a character without a curve even at level 1 (the loop above did not look).
        self.exp_modifier(who, level)?;
        Some(total)
    }

    /// The level `who` has reached with `exp` total experience: the highest level whose
    /// threshold ([`GrowthData::exp_for_level`]) is at most `exp`, from 1 up to [`MAX_LEVEL`].
    /// `None` if the kernel has no EXP curve for the character.
    pub fn level_for_exp(&self, who: CharacterId, exp: u32) -> Option<u8> {
        self.exp_modifier(who, 1)?;
        let (mut level, mut total) = (1u8, 0u32);
        while level < MAX_LEVEL {
            let next = total.saturating_add(self.exp_step(who, level)?);
            if next > exp {
                break;
            }
            total = next;
            level += 1;
        }
        Some(level)
    }
}

/// One materia in a character's equipment: which materia and its AP (savemap format: an id byte, then
/// three bytes of AP; an empty slot is `0xFF` and `0xFFFFFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EquippedMateria {
    /// Index into the kernel's materia table ([`Kernel::materia`]).
    pub id: u8,
    pub ap: u32,
}

/// One record of the initialization data (section 3): a savemap-format character record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitCharacter {
    /// Savemap character id. Do not assume it equals the record's position: the record
    /// slots for Cait Sith and Vincent hold other characters.
    pub id: u8,
    pub level: u8,
    /// Total experience at the start (savemap offset `0x3C`). Cloud's record is the only one
    /// that matches its level (see [`GrowthData::exp_for_level`]); the others hold small
    /// placeholders while their level is 1, because they join the story later.
    pub exp: u32,
    /// Strength, vitality, magic, spirit, dexterity, luck.
    pub stats: [u8; 6],
    pub name: String,
    pub weapon: u8,
    pub armor: u8,
    pub accessory: u8,
    pub back_row: bool,
    pub base_hp: u16,
    pub base_mp: u16,
    /// The materia equipped at the start (savemap offsets `0x40..0x80`: eight weapon slots, then
    /// eight armor slots; empty slots are left out). On the owner's Steam copy only Cloud (Lightning
    /// and Ice) and Red XIII (Fire among others) start with magic materia.
    pub equipped_materia: Vec<EquippedMateria>,
}

impl InitCharacter {
    fn parse(b: &[u8]) -> Self {
        let equipped_materia = (0..INIT_MATERIA_SLOTS)
            .filter_map(|slot| {
                let at = OFF_INIT_MATERIA + slot * 4;
                let id = u8_at(b, at);
                (id != 0xFF).then(|| EquippedMateria {
                    id,
                    ap: u32::from(u16_at(b, at + 1)) | (u32::from(u8_at(b, at + 3)) << 16),
                })
            })
            .collect();
        Self {
            id: u8_at(b, 0x00),
            level: u8_at(b, 0x01),
            exp: u32_at(b, 0x3C),
            stats: array_at(b, 0x02),
            name: text::decode_trimmed(b.get(0x10..0x1C).unwrap_or(&[])),
            weapon: u8_at(b, 0x1C),
            armor: u8_at(b, 0x1D),
            accessory: u8_at(b, 0x1E),
            back_row: u8_at(b, 0x20) == 0xFE,
            base_hp: u16_at(b, 0x2E),
            base_mp: u16_at(b, 0x32),
            equipped_materia,
        }
    }
}

/// One stack of the starting item stock: `count` of item `item` (an index into the item table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemStack {
    pub item: u16,
    pub count: u16,
}

/// Item record (section 4, 28 bytes): the fields the battle rules use. Layout: the first eight bytes
/// are unused (`0xFF`), then the camera ids and the fields below; the damage-calculation, power,
/// element and special-flag fields mean what they mean in an attack record
/// ([`crate::scene::AttackRecord`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemRecord {
    /// Restriction bits (offset `0x0A`), active low: selling is allowed when bit `0x01` is clear,
    /// use in battle when `0x02` is clear and use from the field menu when `0x04` is clear.
    pub restrictions: u16,
    pub target_flags: u8,
    /// The animation id of the item's effect (offset `0x0D`).
    pub effect_id: u8,
    pub damage_calc: u8,
    pub power: u8,
    pub status_chance_mode: u8,
    /// Extra effect (offset `0x12`, `0xFF` = none) and its parameter.
    pub extra_effect: u8,
    pub extra_effect_param: u8,
    pub status_mask: u32,
    pub element_mask: u16,
    /// Special attack flags, **active low** (a cleared bit turns the effect on): `0x0001` hits MP
    /// instead of HP, `0x0100` hits only the dead.
    pub special_flags: u16,
}

impl ItemRecord {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < ITEM_RECORD_SIZE {
            return None;
        }
        Some(Self {
            restrictions: u16_at(b, 0x0A),
            target_flags: u8_at(b, 0x0C),
            effect_id: u8_at(b, 0x0D),
            damage_calc: u8_at(b, 0x0E),
            power: u8_at(b, 0x0F),
            status_chance_mode: u8_at(b, 0x11),
            extra_effect: u8_at(b, 0x12),
            extra_effect_param: u8_at(b, 0x13),
            status_mask: u32_at(b, 0x14),
            element_mask: u16_at(b, 0x18),
            special_flags: u16_at(b, 0x1A),
        })
    }

    /// Whether the record allows use in battle (restriction bit `0x02` clear).
    pub fn usable_in_battle(&self) -> bool {
        self.restrictions & 0x0002 == 0
    }
}

/// Materia record (section 8, 20 bytes): the fields the battle rules use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MateriaRecord {
    /// Cumulative AP needed for levels 2 to 5, in units of 100 AP (`0xFFFF`: that level never comes).
    pub ap_levels: [u16; 4],
    /// The stat bonus index (offset `0x08`).
    pub equip_effect: u8,
    /// The element (offset `0x0C`).
    pub element: u8,
    /// The type byte (offset `0x0D`): the low nybble is the base type (`9` Magic, `0xA` Master Magic,
    /// `0xB`/`0xC` Summon, ...), the high nybble the sub-type.
    pub kind: u8,
    /// The six attribute bytes (offset `0x0E`). For a Magic materia, the attack index of the spell it
    /// teaches at level 1 to 5 (`0xFF`: none; the sixth byte is not a level).
    pub attributes: [u8; 6],
}

impl MateriaRecord {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < MATERIA_RECORD_SIZE {
            return None;
        }
        Some(Self {
            ap_levels: [u16_at(b, 0), u16_at(b, 2), u16_at(b, 4), u16_at(b, 6)],
            equip_effect: u8_at(b, 0x08),
            element: u8_at(b, 0x0C),
            kind: u8_at(b, 0x0D),
            attributes: array_at(b, 0x0E),
        })
    }

    /// A Magic (`9`) or Master Magic (`0xA`) materia: it grants the Magic command.
    pub fn is_magic(&self) -> bool {
        matches!(self.kind & 0x0F, 0x9 | 0xA)
    }

    /// The materia level (1 to 5) that `ap` raw AP reaches: each threshold is 100 times its stored
    /// value, and a threshold of `0xFFFF` is never reached.
    pub fn level_for_ap(&self, ap: u32) -> u8 {
        let mut level = 1u8;
        for threshold in self.ap_levels {
            if threshold == 0xFFFF || ap < u32::from(threshold) * 100 {
                break;
            }
            level += 1;
        }
        level
    }

    /// The spells (attack indexes, ascending) a Magic materia teaches at `level` or below: the
    /// attribute bytes of the first `level` slots that are not `0xFF`.
    pub fn spells_at_level(&self, level: u8) -> Vec<u16> {
        let mut spells: Vec<u16> = self
            .attributes
            .iter()
            .take(usize::from(level.clamp(1, 5)))
            .filter(|a| **a != 0xFF)
            .map(|a| u16::from(*a))
            .collect();
        spells.sort_unstable();
        spells.dedup();
        spells
    }

    /// The spells this materia gives at `ap`: nothing unless it is a Magic materia; Master Magic gives
    /// every spell up to Shield, the others the spells of their level ([`MateriaRecord::spells_at_level`]).
    pub fn unlocked_spells(&self, ap: u32) -> Vec<u16> {
        match self.kind & 0x0F {
            0xA => (0..=LAST_MASTER_MAGIC_SPELL).collect(),
            0x9 => self.spells_at_level(self.level_for_ap(ap)),
            _ => Vec::new(),
        }
    }

    /// For the All materia ([`crate::rules::STARTER_ALL_MATERIA`]): how many times per battle it lets the
    /// magic it is paired with hit a whole side at `level` (1 to 5). The record's attribute bytes 1 to 5
    /// are those numbers (1, 2, 3, 4 and 5 on the Steam copy, matching the community's "once per battle at
    /// level 1 ... five times at level 5"); `0xFF` is "none" like in a spell slot, and nothing is more
    /// than [`MAX_ALL_USES`]. 0 for a level outside 1 to 5.
    pub fn all_uses(&self, level: u8) -> u8 {
        match self.attributes.get(usize::from(level)) {
            Some(&uses) if (1..=5).contains(&level) && uses != 0xFF => uses.min(MAX_ALL_USES),
            _ => 0,
        }
    }
}

/// The most times per battle any All materia lets a spell hit a whole side (its level 5).
pub const MAX_ALL_USES: u8 = 5;

/// Weapon record (section 5, 44 bytes): only the fields the battle rules use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeaponRecord {
    pub target_flags: u8,
    pub damage_calc: u8,
    /// Attack strength added to the wielder's Strength.
    pub attack: u8,
    /// Critical hit rate bonus (percent).
    pub crit_rate: u8,
    /// Hit rate (percent).
    pub accuracy: u8,
    pub equip_mask: u16,
}

impl WeaponRecord {
    /// Targeting bit 0x20: attacks are halved when attacker or target is in the back row.
    pub fn is_short_range(&self) -> bool {
        self.target_flags & 0x20 != 0
    }
}

/// Offset of the weapon record's "mask for access high sound id (0x100+)" byte.
const WEAPON_OFF_SOUND_MASK: usize = 0x0B;
/// Offsets of the weapon record's three sound bytes: "Sound Effect Id for normal hit", "for critical hit" and "for
/// missed attack".
const WEAPON_OFF_HIT_SOUND: usize = 0x24;
const WEAPON_OFF_CRITICAL_SOUND: usize = 0x25;
const WEAPON_OFF_MISS_SOUND: usize = 0x26;

/// The sounds of a weapon's blows, as the game's sound ids (the same id space as the field scripts' `SOUND`: 1-based,
/// entry `id - 1` of `audio.fmt`; `fmv_ff7::sound` has the evidence). Read from the weapon record (section 5):
///
/// * three bytes, at `0x24` (a blow that hits), `0x25` (a critical hit) and `0x26` (a miss), source
///   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Weapon_data.html> (and the same page of the FFRTT wiki);
/// * a mask byte at `0x0B` ("mask for access high sound id (0x100+)"): a sound byte only reaches 255, and the sound
///   bank has 750 entries, so the bits of the mask add `0x100` to an id. **Bit 0 is for the hit sound, bit 1 for
///   the critical one, bit 2 for the miss** (the order of the three bytes). Checked on the owner's Steam copy
///   (`tests/real_data.rs`, `real_weapon_sounds_...`): the mask is `0xF8` (no bit) for Cloud, Barret's guns, Red XIII
///   and Aerith, `0xFB` (bits 0 and 1) for Tifa, Cid, Yuffie, Cait Sith, Sephiroth and Barret's other weapons, and
///   `0xF9` (bit 0) for Vincent, so every id is at most 485 and **bit 2 is never set** (no miss sound is a high id).
///   The proof that bit 0 is the hit and bit 1 the critical hit is in the clips the ids then name: Tifa's hit
///   (36 + 0x100) and critical (94 + 0x100), Yuffie's hit (89 + 0x100) and Vincent's hit (18 + 0x100) land on the
///   clips a hand-made list of the sound bank calls "Tifa Hit", "Tifa Critical Hit", "Yuffie Shuriken Slicing" and a gun
///   (see `fmv_ff7::sound`, "Battle sound ids"), and Vincent's hit id is the very id an enemy's `Shotgun` attack
///   names as its impact sound (274) in `scene.bin`.
///
/// An id of 0 would be "no sound" (`fmv_ff7::sound::entry_for_sound_id`); no real weapon has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeaponSounds {
    /// Played when a blow hits.
    pub hit: u16,
    /// Played when a blow is a critical hit.
    pub critical: u16,
    /// Played when a blow misses.
    pub miss: u16,
}

/// Armor record (section 6, 36 bytes): only the fields the battle rules use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmorRecord {
    pub defense: u8,
    pub magic_defense: u8,
    /// Physical evade percentage ("Defense %").
    pub evade_percent: u8,
    pub magic_evade_percent: u8,
    pub equip_mask: u16,
}

/// A decoded `kernel.bin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    sections: Vec<KernelSection>,
    pub commands: Vec<CommandRecord>,
    pub growth: GrowthData,
    pub init_characters: Vec<InitCharacter>,
    trailing_bytes: usize,
}

impl Kernel {
    /// Parse the bytes of a PC `kernel.bin`.
    pub fn parse(bytes: &[u8]) -> Result<Self, KernelError> {
        if bytes.is_empty() {
            return Err(KernelError::Empty);
        }
        let mut sections: Vec<KernelSection> = Vec::new();
        let mut pos = 0usize;
        while pos + SECTION_HEADER_SIZE <= bytes.len() {
            let compressed = usize::from(u16_at(bytes, pos));
            let expected = usize::from(u16_at(bytes, pos + 2));
            let file_type = u16_at(bytes, pos + 4);
            if compressed == 0 {
                // Zero padding after the last section.
                break;
            }
            let section = sections.len();
            if section >= MAX_SECTIONS {
                return Err(KernelError::TooManySections);
            }
            let body_start = pos + SECTION_HEADER_SIZE;
            let body =
                bytes
                    .get(body_start..body_start + compressed)
                    .ok_or(KernelError::Truncated {
                        section,
                        offset: pos,
                    })?;
            let data = gunzip_capped(body, expected)
                .map_err(|source| KernelError::Gzip { section, source })?;
            if data.len() != expected {
                return Err(KernelError::SizeMismatch {
                    section,
                    expected,
                    actual: data.len(),
                });
            }
            sections.push(KernelSection { file_type, data });
            pos = body_start + compressed;
        }
        let trailing_bytes = bytes.len().saturating_sub(pos);

        let need = |index: usize| -> Result<&[u8], KernelError> {
            sections
                .get(index)
                .map(|s| s.data.as_slice())
                .ok_or(KernelError::MissingSection(index))
        };
        let command_section = need(SECTION_COMMANDS)?;
        if command_section.len() < COMMAND_COUNT * COMMAND_RECORD_SIZE {
            return Err(KernelError::BadSectionSize {
                section: SECTION_COMMANDS,
                expected: COMMAND_COUNT * COMMAND_RECORD_SIZE,
                actual: command_section.len(),
            });
        }
        let commands = (0..COMMAND_COUNT)
            .map(|i| {
                CommandRecord::parse(
                    command_section
                        .get(i * COMMAND_RECORD_SIZE..)
                        .unwrap_or(&[]),
                )
            })
            .collect();
        let growth = GrowthData::parse(need(SECTION_BATTLE_AND_GROWTH)?)?;
        let init_section = need(SECTION_INITIALIZATION)?;
        if init_section.len() < CHARACTER_COUNT * INIT_CHARACTER_SIZE {
            return Err(KernelError::BadSectionSize {
                section: SECTION_INITIALIZATION,
                expected: CHARACTER_COUNT * INIT_CHARACTER_SIZE,
                actual: init_section.len(),
            });
        }
        let init_characters = (0..CHARACTER_COUNT)
            .map(|i| {
                InitCharacter::parse(init_section.get(i * INIT_CHARACTER_SIZE..).unwrap_or(&[]))
            })
            .collect();

        Ok(Self {
            sections,
            commands,
            growth,
            init_characters,
            trailing_bytes,
        })
    }

    /// Read and parse `path`, refusing files larger than [`MAX_KERNEL_BYTES`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KernelError> {
        let path = path.as_ref();
        let len = std::fs::metadata(path)?.len();
        if len > MAX_KERNEL_BYTES {
            return Err(KernelError::TooLarge(len));
        }
        Self::parse(&std::fs::read(path)?)
    }

    /// All decoded sections, in file order.
    pub fn sections(&self) -> &[KernelSection] {
        &self.sections
    }

    /// Decompressed bytes of one section.
    pub fn section(&self, index: usize) -> Option<&[u8]> {
        self.sections.get(index).map(|s| s.data.as_slice())
    }

    /// Bytes left after the last section (the vanilla file has a couple of padding bytes).
    pub fn trailing_bytes(&self) -> usize {
        self.trailing_bytes
    }

    /// Command record by command index (index 1 is Attack, see [`COMMAND_ATTACK`]).
    pub fn command(&self, index: usize) -> Option<&CommandRecord> {
        self.commands.get(index)
    }

    /// The limit-break fields of `who`'s growth record (see [`CharacterLimits`]).
    pub fn limits(&self, who: CharacterId) -> Option<&CharacterLimits> {
        self.growth.characters.get(who.index()).map(|c| &c.limits)
    }

    /// Attack record `index` of the kernel attack data (section 1).
    pub fn attack(&self, index: usize) -> Option<AttackRecord> {
        let data = self.section(SECTION_ATTACKS)?;
        data.get(index.checked_mul(ATTACK_RECORD_SIZE)?..)
            .and_then(AttackRecord::parse)
    }

    /// Weapon record by weapon id (section 5).
    pub fn weapon(&self, id: u8) -> Option<WeaponRecord> {
        let data = self.section(SECTION_WEAPONS)?;
        let rec = data.get(usize::from(id) * WEAPON_RECORD_SIZE..)?;
        if rec.len() < WEAPON_RECORD_SIZE {
            return None;
        }
        Some(WeaponRecord {
            target_flags: u8_at(rec, 0x00),
            damage_calc: u8_at(rec, 0x02),
            attack: u8_at(rec, 0x04),
            crit_rate: u8_at(rec, 0x07),
            accuracy: u8_at(rec, 0x08),
            equip_mask: u16_at(rec, 0x0E),
        })
    }

    /// The sounds of weapon `id`'s blows (see [`WeaponSounds`]); `None` past the end of the weapon table. Read-only:
    /// nothing in the rules depends on it.
    pub fn weapon_sounds(&self, id: u8) -> Option<WeaponSounds> {
        let data = self.section(SECTION_WEAPONS)?;
        let rec = data.get(usize::from(id) * WEAPON_RECORD_SIZE..)?;
        if rec.len() < WEAPON_RECORD_SIZE {
            return None;
        }
        let mask = u8_at(rec, WEAPON_OFF_SOUND_MASK);
        // A sound byte plus 0x100 when its bit of the mask is set.
        let sound = |offset: usize, bit: u8| u16::from(u8_at(rec, offset)) | if mask & bit != 0 { 0x100 } else { 0 };
        Some(WeaponSounds {
            hit: sound(WEAPON_OFF_HIT_SOUND, 0x01),
            critical: sound(WEAPON_OFF_CRITICAL_SOUND, 0x02),
            miss: sound(WEAPON_OFF_MISS_SOUND, 0x04),
        })
    }

    /// Armor record by armor id (section 6).
    pub fn armor(&self, id: u8) -> Option<ArmorRecord> {
        let data = self.section(SECTION_ARMOR)?;
        let rec = data.get(usize::from(id) * ARMOR_RECORD_SIZE..)?;
        if rec.len() < ARMOR_RECORD_SIZE {
            return None;
        }
        Some(ArmorRecord {
            defense: u8_at(rec, 0x02),
            magic_defense: u8_at(rec, 0x03),
            evade_percent: u8_at(rec, 0x04),
            magic_evade_percent: u8_at(rec, 0x05),
            equip_mask: u16_at(rec, 0x12),
        })
    }

    /// The initialization record whose savemap character id is `save_id`.
    pub fn init_character_by_id(&self, save_id: u8) -> Option<&InitCharacter> {
        self.init_characters.iter().find(|c| c.id == save_id)
    }

    /// Item record `index` of the item table (section 4; `None` past [`ITEM_COUNT`]).
    pub fn item(&self, index: u16) -> Option<ItemRecord> {
        if usize::from(index) >= ITEM_COUNT {
            return None;
        }
        let data = self.section(SECTION_ITEMS)?;
        ItemRecord::parse(data.get(usize::from(index) * ITEM_RECORD_SIZE..)?)
    }

    /// Materia record `id` of the materia table (section 8; `None` past [`MATERIA_COUNT`]).
    pub fn materia(&self, id: u8) -> Option<MateriaRecord> {
        if usize::from(id) >= MATERIA_COUNT {
            return None;
        }
        let data = self.section(SECTION_MATERIA)?;
        MateriaRecord::parse(data.get(usize::from(id) * MATERIA_RECORD_SIZE..)?)
    }

    /// The item stock a new game starts with, from the initialization data (section 3, after the
    /// character records and the party): the savemap's 320 slots of a `u16` each, the item index in
    /// the low nine bits and the count in the high seven, `0xFFFF` for an empty slot. Only consumable
    /// items count (index below [`ITEM_COUNT`]; weapons, armor and accessories are not kept), the
    /// same item in two slots is merged and a stack never passes [`MAX_ITEM_STACK`]. In file order.
    /// Empty when the section is too short to hold a stock. On the owner's Steam copy: Potion x3,
    /// Ether x1, Phoenix Down x1.
    pub fn starting_inventory(&self) -> Vec<ItemStack> {
        let Some(section) = self.section(SECTION_INITIALIZATION) else {
            return Vec::new();
        };
        let mut stacks: Vec<ItemStack> = Vec::new();
        for slot in 0..INIT_ITEM_SLOTS {
            let raw = u16_at(section, OFF_INIT_ITEMS + slot * 2);
            if raw == 0xFFFF {
                continue;
            }
            let (item, count) = (raw & 0x01FF, raw >> 9);
            // A count of zero is also what a read past the end of a short section gives.
            if count == 0 || usize::from(item) >= ITEM_COUNT {
                continue;
            }
            match stacks.iter_mut().find(|s| s.item == item) {
                Some(stack) => stack.count = (stack.count + count).min(MAX_ITEM_STACK),
                None => stacks.push(ItemStack {
                    item,
                    count: count.min(MAX_ITEM_STACK),
                }),
            }
        }
        stacks
    }

    /// The spells (attack indexes, ascending, no repeats) that the magic materia in `who`'s starting
    /// equipment teach at the AP they start with. Empty for a character without a starting record
    /// (Cait Sith and Vincent) or without magic materia.
    pub fn equipped_spells(&self, who: CharacterId) -> Vec<u16> {
        let record = u8::try_from(who.index())
            .ok()
            .and_then(|id| self.init_character_by_id(id));
        let mut spells: Vec<u16> = record
            .into_iter()
            .flat_map(|rec| rec.equipped_materia.iter())
            .filter_map(|m| self.materia(m.id).map(|rec| rec.unlocked_spells(m.ap)))
            .flatten()
            .collect();
        spells.sort_unstable();
        spells.dedup();
        spells
    }

    /// The total experience `who` starts with when it starts at `level` (clamped to
    /// `1..=`[`MAX_LEVEL`]): the kernel's own starting record when that record belongs to this
    /// level on the character's EXP curve (Cloud's does: level 6 with 610 EXP, six short of level
    /// 7), else the least that level takes ([`GrowthData::exp_for_level`]). The other characters'
    /// records hold level 1 with a few placeholder points, because they join the story later and
    /// start at the leader's level. `None` if the kernel has no EXP curve for the character.
    pub fn starting_exp(&self, who: CharacterId, level: u8) -> Option<u32> {
        let level = level.clamp(1, MAX_LEVEL);
        let floor = self.growth.exp_for_level(who, level)?;
        let own = u8::try_from(who.index())
            .ok()
            .and_then(|id| self.init_character_by_id(id))
            .filter(|rec| {
                rec.level == level && self.growth.level_for_exp(who, rec.exp) == Some(level)
            })
            .map(|rec| rec.exp);
        Some(own.unwrap_or(floor))
    }

    /// String `index` of an FF text table section (a `u16` pointer table followed by
    /// strings; the first pointer doubles as the table size), decoded with [`text::decode_trimmed`].
    ///
    /// The back-reference compression (`0xF9`) used by the battle text section is not
    /// expanded, so use this for the name tables (commands, weapons, armor, ...).
    pub fn text_entry(&self, section: usize, index: usize) -> Option<String> {
        let data = self.section(section)?;
        let count = usize::from(u16_at(data, 0)) / 2;
        if index >= count {
            return None;
        }
        let offset = usize::from(u16_at(data, index * 2));
        let tail = data.get(offset..)?;
        Some(text::decode_trimmed(tail))
    }

    pub fn command_name(&self, index: usize) -> Option<String> {
        self.text_entry(SECTION_TEXT_COMMAND_NAMES, index)
    }

    pub fn weapon_name(&self, id: u8) -> Option<String> {
        self.text_entry(SECTION_TEXT_WEAPON_NAMES, usize::from(id))
    }

    pub fn armor_name(&self, id: u8) -> Option<String> {
        self.text_entry(SECTION_TEXT_ARMOR_NAMES, usize::from(id))
    }

    /// Name of spell `index` (an attack index of section 1) from this file's "Names - magic" table.
    /// The Steam edition's `kernel2.bin` text may differ from the text sections of `kernel.bin`.
    pub fn magic_name(&self, index: u16) -> Option<String> {
        self.text_entry(SECTION_TEXT_MAGIC_NAMES, usize::from(index))
    }

    /// Name of item `index` from this file's "Names - items" table.
    pub fn item_name(&self, index: u16) -> Option<String> {
        self.text_entry(SECTION_TEXT_ITEM_NAMES, usize::from(index))
    }

    /// Name of materia `id` from this file's "Names - materia" table.
    pub fn materia_name(&self, id: u8) -> Option<String> {
        self.text_entry(SECTION_TEXT_MATERIA_NAMES, usize::from(id))
    }
}

/// Decode `kernel2.bin` (4-byte little-endian compressed length, then LZSS data) into the
/// concatenated text sections. See the module docs for what is and is not decoded.
pub fn decode_kernel2(bytes: &[u8]) -> Result<Vec<u8>, KernelError> {
    if bytes.len() < 4 {
        return Err(KernelError::Kernel2("file shorter than its 4-byte header"));
    }
    let compressed = u32_at(bytes, 0) as usize;
    let body = bytes
        .get(4..)
        .and_then(|rest| rest.get(..compressed))
        .ok_or(KernelError::Kernel2("header length exceeds the file size"))?;
    lzss_decode(body, MAX_KERNEL2_DECODED)
}

/// Decode FF7's LZSS variant: 4096-byte ring buffer initialised to zero, write position
/// starting at 4078, control bytes with eight flags (LSB first, 1 = literal), and 2-byte
/// references (`offset_low`, `offset_high << 4 | (length - 3)`).
///
/// This is Haruhiko Okumura's LZSS. The parameters were validated against the real
/// `kernel2.bin`: it decodes to recognisable text and one of its text sections is
/// byte-identical to the matching gzip section of `kernel.bin`.
///
/// Fails if the output would exceed `max_out`. Truncated input simply ends the output.
pub fn lzss_decode(src: &[u8], max_out: usize) -> Result<Vec<u8>, KernelError> {
    const RING: usize = 4096;

    struct Decoder {
        ring: [u8; RING],
        write: usize,
        out: Vec<u8>,
        max_out: usize,
    }

    impl Decoder {
        /// Append one byte to the output and the ring buffer.
        fn emit(&mut self, c: u8) -> Result<(), KernelError> {
            if self.out.len() >= self.max_out {
                return Err(KernelError::Kernel2("decoded text exceeds the size limit"));
            }
            self.out.push(c);
            if let Some(slot) = self.ring.get_mut(self.write) {
                *slot = c;
            }
            self.write = (self.write + 1) & (RING - 1);
            Ok(())
        }

        fn peek(&self, index: usize) -> u8 {
            self.ring.get(index & (RING - 1)).copied().unwrap_or(0)
        }
    }

    let mut dec = Decoder {
        ring: [0u8; RING],
        write: RING - 18,
        out: Vec::new(),
        max_out,
    };
    let mut i = 0usize;
    let mut flags: u32 = 0;

    loop {
        flags >>= 1;
        if flags & 0x100 == 0 {
            let Some(&control) = src.get(i) else { break };
            i += 1;
            flags = u32::from(control) | 0xFF00;
        }
        if flags & 1 != 0 {
            let Some(&c) = src.get(i) else { break };
            i += 1;
            dec.emit(c)?;
        } else {
            let (Some(&lo), Some(&hi)) = (src.get(i), src.get(i + 1)) else {
                break;
            };
            i += 2;
            let offset = usize::from(lo) | (usize::from(hi & 0xF0) << 4);
            let len = usize::from(hi & 0x0F) + 3;
            for k in 0..len {
                // Each byte is read before the next is written, so a reference may overlap
                // the bytes it is producing (as in the reference decoder).
                let c = dec.peek(offset + k);
                dec.emit(c)?;
            }
        }
    }
    Ok(dec.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        command_section, pack_kernel, synthetic_kernel_bytes, synthetic_kernel_sections,
    };

    #[test]
    fn parses_sections_and_typed_data() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        assert_eq!(k.sections().len(), 27);
        assert_eq!(k.trailing_bytes(), 2);
        assert_eq!(k.sections()[0].file_type, 0);
        assert_eq!(k.sections()[26].file_type, 9);
        assert_eq!(k.section(0).unwrap().len(), 256);
        assert_eq!(k.section(4).unwrap().len(), ITEM_COUNT * ITEM_RECORD_SIZE);
        assert_eq!(
            k.section(8).unwrap().len(),
            MATERIA_COUNT * MATERIA_RECORD_SIZE
        );
        assert!(k.section(27).is_none());

        // Commands.
        assert_eq!(k.commands.len(), 32);
        let attack = k.command(COMMAND_ATTACK).unwrap();
        assert_eq!((attack.cursor_action, attack.target_flags), (7, 0xFF));
        let magic = k.command(2).unwrap();
        assert_eq!(
            (
                magic.cursor_action,
                magic.target_flags,
                magic.camera_single,
                magic.camera_multi
            ),
            (1, 0x23, 0x0123, 0x0124)
        );
        assert_eq!(k.command_name(1).as_deref(), Some("Attack"));
        assert_eq!(k.command_name(2).as_deref(), Some("Magic"));
        assert_eq!(k.command_name(3), None);

        // Attack, weapon, armor records and names.
        let atk = k.attack(3).unwrap();
        assert_eq!((atk.accuracy, atk.power, atk.damage_calc), (90, 24, 0x11));
        assert!(atk.is_basic_physical());
        assert!(k.attack(128).is_none());
        let w0 = k.weapon(0).unwrap();
        assert!(w0.is_short_range());
        assert_eq!(
            (w0.attack, w0.crit_rate, w0.accuracy, w0.equip_mask),
            (10, 0, 90, 1)
        );
        let w1 = k.weapon(1).unwrap();
        assert!(!w1.is_short_range());
        assert_eq!(w1.attack, 11);
        assert!(k.weapon(200).is_none());
        let a2 = k.armor(2).unwrap();
        // Armor id 2: defense 5 + 3*2, magic defense 2 + 2, evade 2*2 percent.
        assert_eq!((a2.defense, a2.magic_defense, a2.evade_percent), (11, 4, 4));
        assert!(k.armor(40).is_none());
        assert_eq!(k.weapon_name(0).as_deref(), Some("Test Blade"));
        assert_eq!(k.weapon_name(1).as_deref(), Some("Test Gun"));
        assert_eq!(k.weapon_name(2), None);
        assert_eq!(k.armor_name(0).as_deref(), Some("Test Bangle"));

        // Init records are found by id, not by slot.
        assert_eq!(k.init_characters.len(), 9);
        assert_eq!(k.init_character_by_id(0).unwrap().name, "Hero");
        assert_eq!(k.init_character_by_id(8).unwrap().weapon, 8);
        // Slot 6 carries id 20, so a slot-index lookup would be wrong.
        assert_eq!(k.init_character_by_id(20).unwrap().level, 7);
        assert_eq!(k.init_character_by_id(21).unwrap().level, 8);
        assert!(k.init_character_by_id(6).is_none());
        assert!(k.init_character_by_id(7).is_none());
        assert!(k.init_character_by_id(3).unwrap().back_row);
        assert!(!k.init_character_by_id(0).unwrap().back_row);

        // Growth: record fields, tables and lookup.
        let g = &k.growth;
        assert_eq!(g.characters.len(), 9);
        assert_eq!(g.characters[1].strength_curve, 1);
        assert_eq!(g.characters[1].hp_curve, 38);
        // The level modifier byte is signed.
        assert_eq!(g.characters[0].level_modifier, 0);
        assert_eq!(g.characters[1].level_modifier, -1);
        assert_eq!(g.characters[2].level_modifier, -2);
        assert_eq!(g.characters[7].level_modifier, 4);
        assert_eq!(g.stat_gain_table[4], 3);
        assert_eq!(g.hp_gain_percent[11], 120);
        assert_eq!(g.mp_gain_percent[0], 12);
        assert_eq!(g.curves.len(), 64);
        assert_eq!(g.scene_block_starts(), vec![0, 5, 9, 14]);
    }

    #[test]
    fn the_limit_fields_of_a_growth_record_are_read_by_offset() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let barret = k.limits(CharacterId::Barret).expect("a record");
        // Limit ids by [level][slot]: unused slots (0xFF) are None.
        assert_eq!(barret.commands[0], [Some(0x81), Some(0x91), None]);
        assert_eq!(barret.commands[1], [Some(0xA1), None, None]);
        assert_eq!(barret.commands[2], [None, None, None]);
        assert_eq!(barret.commands[3], [Some(0xB1), None, None]);
        assert_eq!(barret.first(), Some(0x81));
        assert_eq!((barret.kills, barret.uses), ([11, 31], [6, 7, 8]));
        assert_eq!(barret.divisors, [110, 210, 310, 410]);
        assert_eq!(barret.divisor_at_level_one(), Some(110));
        // The raw record is still there and the typed fields are what it holds.
        let record = &k.growth.characters[CharacterId::Barret.index()];
        assert_eq!(record.raw[0x0C], 0x81);
        assert_eq!(record.limits, *barret);
        // A character with no limit in its record has none and no divisor to fill a gauge with.
        let none = CharacterLimits::parse(&[0xFF; GROWTH_RECORD_SIZE]);
        assert_eq!((none.first(), none.divisor_at_level_one()), (None, None));
        let no_divisor = CharacterLimits { divisors: [0; 4], ..*barret };
        assert_eq!(no_divisor.divisor_at_level_one(), None, "a divisor of 0 would fill nothing");
        // A record cut short reads as empty, never as a panic.
        assert_eq!(CharacterLimits::parse(&[]).first(), None);
    }

    #[test]
    fn item_records_carry_what_the_battle_rules_use() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // Potion: usable in battle, one ally, a flat heal (calc 0x26, Power 5), restorative element.
        let potion = k.item(0).unwrap();
        assert_eq!(
            (potion.target_flags, potion.damage_calc, potion.power),
            (0x01, 0x26, 5)
        );
        assert_eq!(potion.element_mask, 0x0200);
        assert_eq!(potion.restrictions, 0xFFF8);
        assert!(potion.usable_in_battle());
        assert_eq!(potion.special_flags, 0xFFFF, "no special flag: it hits HP");
        // Ether is the same with the "hits MP" bit (active low) cleared; Phoenix Down hits the dead.
        assert_eq!(k.item(3).unwrap().special_flags & 0x0001, 0);
        let phoenix = k.item(7).unwrap();
        assert_eq!(
            (
                phoenix.damage_calc,
                phoenix.power,
                phoenix.special_flags & 0x0100
            ),
            (0x24, 8, 0)
        );
        assert_eq!(phoenix.status_mask, 1);
        // An item that restricts battle use says so; an unused record reads as calc 0xFF.
        assert!(!k.item(100).unwrap().usable_in_battle());
        assert_eq!(k.item(127).unwrap().damage_calc, 0xFF);
        assert!(
            !k.item(127).unwrap().usable_in_battle(),
            "0xFFFF restrictions: nothing allowed"
        );
        // Past the table, and a truncated section, are `None`.
        assert!(k.item(128).is_none() && k.item(u16::MAX).is_none());
        let mut sections = synthetic_kernel_sections();
        sections[4].1.truncate(ITEM_RECORD_SIZE * 3 + 10);
        let short = Kernel::parse(&pack_kernel(&sections, &[])).unwrap();
        assert!(short.item(2).is_some());
        assert!(
            short.item(3).is_none(),
            "a record cut short is not a record"
        );
        // Names come from the kernel's own text tables.
        assert_eq!(k.item_name(0).as_deref(), Some("Potion"));
        assert_eq!(k.item_name(7).as_deref(), Some("Phoenix Down"));
        assert_eq!(k.magic_name(0).as_deref(), Some("Cure"));
        assert_eq!(k.magic_name(27).as_deref(), Some("Fire"));
        assert_eq!(k.materia_name(53).as_deref(), Some("Restore"));
        assert_eq!(k.magic_name(500), None);
    }

    #[test]
    fn an_all_materia_gives_one_use_per_level_and_nothing_outside_one_to_five() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let all = k.materia(23).unwrap();
        assert_eq!(all.ap_levels, [15, 60, 180, 350], "1500, 6000, 18000 and 35000 AP");
        assert_eq!((0..=6).map(|level| all.all_uses(level)).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 0]);
        assert_eq!(all.level_for_ap(0), 1);
        assert_eq!(all.level_for_ap(35_000), 5);
        // A record that is not an All materia (every attribute byte is "none") gives none, and a corrupt one
        // never gives more than the five of its top level.
        assert_eq!(k.materia(22).unwrap().all_uses(1), 0);
        let mut odd = all;
        odd.attributes = [0x51, 9, 200, 0xFF, 5, 0];
        assert_eq!((odd.all_uses(1), odd.all_uses(2), odd.all_uses(3), odd.all_uses(4), odd.all_uses(5)), (5, 5, 0, 5, 0));
    }

    #[test]
    fn materia_levels_follow_the_ap_thresholds_in_hundreds() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let fire = k.materia(49).unwrap();
        assert_eq!(fire.ap_levels, [20, 180, 350, 0xFFFF]);
        assert_eq!(
            (fire.kind, fire.attributes[..3].to_vec()),
            (0x19, vec![27, 28, 29])
        );
        assert!(fire.is_magic());
        // Level 2 needs 2000 AP (20 * 100), level 3 18000, level 4 35000; the fourth threshold never comes.
        assert_eq!(fire.level_for_ap(0), 1);
        assert_eq!(fire.level_for_ap(1999), 1);
        assert_eq!(fire.level_for_ap(2000), 2);
        assert_eq!(fire.level_for_ap(17_999), 2);
        assert_eq!(fire.level_for_ap(18_000), 3);
        assert_eq!(fire.level_for_ap(35_000), 4);
        assert_eq!(
            fire.level_for_ap(0xFF_FFFF),
            4,
            "a threshold of 0xFFFF is never reached"
        );
        // The spells of a level are the attribute bytes of the slots up to it.
        assert_eq!(fire.spells_at_level(1), vec![27]);
        assert_eq!(fire.spells_at_level(2), vec![27, 28]);
        assert_eq!(fire.spells_at_level(3), vec![27, 28, 29]);
        assert_eq!(
            fire.spells_at_level(9),
            vec![27, 28, 29],
            "levels past the fifth are clamped"
        );
        assert_eq!(fire.spells_at_level(0), vec![27], "there is no level 0");
        assert_eq!(fire.unlocked_spells(2000), vec![27, 28]);
        // Restore teaches Cure, Cure2, Regen (attack 3) and Cure3 (attack 2), listed in index order.
        let restore = k.materia(53).unwrap();
        assert_eq!(restore.unlocked_spells(0), vec![0]);
        assert_eq!(restore.unlocked_spells(2500), vec![0, 1]);
        assert_eq!(restore.unlocked_spells(40_000), vec![0, 1, 2, 3]);
        // Master Magic teaches every spell up to Shield; a command materia teaches no spell.
        let master = k.materia(73).unwrap();
        assert!(master.is_magic());
        assert_eq!(master.unlocked_spells(0), (0..=0x35).collect::<Vec<u16>>());
        let steal = k.materia(36).unwrap();
        assert!(!steal.is_magic());
        assert!(steal.unlocked_spells(0xFF_FFFF).is_empty());
        assert!(!k.materia(0).unwrap().is_magic());
        assert!(k.materia(96).is_none() && k.materia(255).is_none());
    }

    #[test]
    fn the_starting_records_list_their_equipped_materia_and_the_spells_they_teach() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let cloud = k.init_character_by_id(0).unwrap();
        assert_eq!(
            cloud.equipped_materia,
            vec![
                EquippedMateria { id: 52, ap: 0 },
                EquippedMateria { id: 50, ap: 0 }
            ]
        );
        // Weapon slots and armor slots are one list; a three-byte AP is read in full.
        let red = k.init_character_by_id(4).unwrap();
        assert_eq!(
            red.equipped_materia,
            vec![
                EquippedMateria { id: 49, ap: 1000 },
                EquippedMateria { id: 0, ap: 77 }
            ]
        );
        assert_eq!(
            k.init_character_by_id(8).unwrap().equipped_materia,
            vec![EquippedMateria {
                id: 73,
                ap: 0xFF_FFFF
            }]
        );
        assert!(k
            .init_character_by_id(1)
            .unwrap()
            .equipped_materia
            .is_empty());
        // Cloud: Bolt (33) and Ice (30). Red XIII's Fire is still level 1 at 1000 AP, and his other
        // materia is not a magic one. The record with Restore at 2500 AP knows Cure and Cure2.
        assert_eq!(k.equipped_spells(CharacterId::Cloud), vec![30, 33]);
        assert_eq!(k.equipped_spells(CharacterId::RedXiii), vec![27]);
        assert_eq!(k.equipped_spells(CharacterId::Yuffie), vec![0, 1]);
        // Master Magic: everything up to Shield. Nobody else: nothing, and no record means no spells.
        assert_eq!(
            k.equipped_spells(CharacterId::Cid),
            (0..=0x35).collect::<Vec<u16>>()
        );
        assert!(k.equipped_spells(CharacterId::Barret).is_empty());
        assert!(k.equipped_spells(CharacterId::CaitSith).is_empty());
        assert!(k.equipped_spells(CharacterId::Vincent).is_empty());
    }

    #[test]
    fn the_starting_stock_is_read_from_the_item_slots_after_the_party() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // Potion x3 (merged with the later x2 into x5), Ether x1, Phoenix Down x1; the weapon and the
        // count of zero are not there, and the file's order is kept.
        assert_eq!(
            k.starting_inventory(),
            vec![
                ItemStack { item: 0, count: 5 },
                ItemStack { item: 3, count: 1 },
                ItemStack { item: 7, count: 1 }
            ]
        );
        // A stack never passes the game's cap of 99.
        let mut sections = synthetic_kernel_sections();
        for (slot, raw) in [
            (10usize, (90u16 << 9) | 5),
            (11, (60 << 9) | 5),
            (12, (127 << 9) | 6),
        ] {
            let at = OFF_INIT_ITEMS + slot * 2;
            sections[3].1[at..at + 2].copy_from_slice(&raw.to_le_bytes());
        }
        let capped = Kernel::parse(&pack_kernel(&sections, &[]))
            .unwrap()
            .starting_inventory();
        assert_eq!(
            capped.iter().find(|s| s.item == 5),
            Some(&ItemStack {
                item: 5,
                count: MAX_ITEM_STACK
            })
        );
        assert_eq!(
            capped.iter().find(|s| s.item == 6),
            Some(&ItemStack {
                item: 6,
                count: MAX_ITEM_STACK
            })
        );
        // A section that ends before the stock has none, whatever it holds before.
        let mut sections = synthetic_kernel_sections();
        sections[3]
            .1
            .truncate(CHARACTER_COUNT * INIT_CHARACTER_SIZE);
        assert!(Kernel::parse(&pack_kernel(&sections, &[]))
            .unwrap()
            .starting_inventory()
            .is_empty());
        // And one that ends in the middle of it keeps what is there.
        let mut sections = synthetic_kernel_sections();
        sections[3].1.truncate(OFF_INIT_ITEMS + 4);
        assert_eq!(
            Kernel::parse(&pack_kernel(&sections, &[]))
                .unwrap()
                .starting_inventory(),
            vec![
                ItemStack { item: 0, count: 3 },
                ItemStack { item: 3, count: 1 }
            ]
        );
    }

    #[test]
    fn stat_block_follows_the_closed_form() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // Character 0, level 6 (band 0): primary curve 0: gradient 100, base 10.
        // primary = 10 + 100*6/100 = 16.
        let b = k.growth.stat_block(CharacterId::Cloud, 6).unwrap();
        assert_eq!(b.level, 6);
        assert_eq!(
            (
                b.strength,
                b.vitality,
                b.magic,
                b.spirit,
                b.dexterity,
                b.luck
            ),
            (16, 16, 16, 16, 16, 16)
        );
        // HP curve 37 band 0: base 4, gradient 20 -> 40*4 + 6*20 = 280.
        assert_eq!(b.max_hp, 280);
        // MP curve 46 band 0: base 7, gradient 60 -> 2*7 + 6*60/10 = 50.
        assert_eq!(b.max_mp, 50);

        // Level 99 (band 7): primary curve 0 band 7: gradient 170, base 17 -> 17 + 168 = 185.
        // HP: base -48, gradient 90 -> 40*-48 + 99*90 = 6990. MP: base 61, gradient 130 ->
        // 2*61 + 99*130/10 = 1409 -> clamped to 999.
        let b = k.growth.stat_block(CharacterId::Cloud, 99).unwrap();
        assert_eq!(b.strength, 185);
        assert_eq!(b.max_hp, 6990);
        assert_eq!(b.max_mp, 999);

        // Level 0 is clamped to 1, and above 99 to 99.
        assert_eq!(k.growth.stat_block(CharacterId::Cloud, 0).unwrap().level, 1);
        assert_eq!(
            k.growth.stat_block(CharacterId::Cloud, 200).unwrap().level,
            99
        );

        // Band boundaries.
        assert_eq!(curve_band(1), 0);
        assert_eq!(curve_band(11), 0);
        assert_eq!(curve_band(12), 1);
        assert_eq!(curve_band(61), 5);
        assert_eq!(curve_band(62), 6);
        assert_eq!(curve_band(81), 6);
        assert_eq!(curve_band(82), 7);
        assert_eq!(curve_band(99), 7);

        // Stat blocks are monotone in level for this data and never panic for any level.
        let mut prev = 0;
        for level in 1..=99u8 {
            let b = k.growth.stat_block(CharacterId::Barret, level).unwrap();
            assert!(b.strength >= prev);
            prev = b.strength;
            assert!(b.max_hp >= 1);
        }
        assert_eq!(CharacterId::from_index(8), Some(CharacterId::Cid));
        assert_eq!(CharacterId::from_index(9), None);
        assert_eq!(CharacterId::Aeris.index(), 3);
    }

    /// The synthetic kernel gives character `i` EXP curve `55 + i` with modifier
    /// `100 + 10 * band + curve` in every band (see `testutil::growth_section`), so every number
    /// below follows from `needed = modifier * level^2 / 10` by hand.
    #[test]
    fn exp_thresholds_follow_the_documented_rule() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let g = &k.growth;
        // Cloud: curve 55, band 0 modifier 155, band 1 165, band 7 225.
        assert_eq!(g.exp_modifier(CharacterId::Cloud, 2), Some(155));
        assert_eq!(g.exp_modifier(CharacterId::Cloud, 11), Some(155));
        assert_eq!(g.exp_modifier(CharacterId::Cloud, 12), Some(165));
        assert_eq!(g.exp_modifier(CharacterId::Cloud, 99), Some(225));
        // Steps: 155 * k^2 / 10 for k = 1..=5 -> 15, 62, 139, 248, 387 (the product first, then
        // the division: the first one is 15, not 155 * (1 / 10) = 0).
        assert_eq!(
            (1..=5)
                .map(|l| g.exp_to_next_level(CharacterId::Cloud, l).unwrap())
                .collect::<Vec<_>>(),
            [15, 62, 139, 248, 387]
        );
        // Totals are the running sums, level 1 costs nothing.
        assert_eq!(
            (1..=6)
                .map(|l| g.exp_for_level(CharacterId::Cloud, l).unwrap())
                .collect::<Vec<_>>(),
            [0, 15, 77, 216, 464, 851]
        );
        // The modifier is the one of the level being ATTAINED: 10 -> 11 is still band 0, but
        // 11 -> 12 attains a level of band 1.
        assert_eq!(
            g.exp_to_next_level(CharacterId::Cloud, 10),
            Some(155 * 100 / 10)
        );
        assert_eq!(
            g.exp_to_next_level(CharacterId::Cloud, 11),
            Some(165 * 121 / 10)
        );
        // Another character has another curve (curve 56: modifier 156).
        assert_eq!(
            g.exp_to_next_level(CharacterId::Barret, 3),
            Some(156 * 9 / 10)
        );
        assert_eq!(g.exp_for_level(CharacterId::Barret, 4), Some(15 + 62 + 140));
    }

    #[test]
    fn exp_levels_invert_the_thresholds_and_never_misbehave() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let g = &k.growth;
        for who in CharacterId::ALL {
            let mut prev = 0;
            for level in 1..=MAX_LEVEL {
                let t = g.exp_for_level(who, level).unwrap();
                assert!(
                    level == 1 || t > prev,
                    "{who:?} level {level}: thresholds rise"
                );
                // Exactly the threshold reaches the level, one EXP less is still the one before.
                assert_eq!(g.level_for_exp(who, t), Some(level), "{who:?} {level}");
                if level > 1 {
                    assert_eq!(
                        g.level_for_exp(who, t - 1),
                        Some(level - 1),
                        "{who:?} {level}"
                    );
                }
                prev = t;
            }
            // Level 99 is far below the caps, and nothing goes past it.
            assert!(prev < MAX_EXP, "{who:?}: {prev}");
            assert_eq!(g.level_for_exp(who, u32::MAX), Some(MAX_LEVEL));
            assert_eq!(g.level_for_exp(who, MAX_EXP), Some(MAX_LEVEL));
        }
        // Out-of-range levels are clamped, the top has no next level.
        assert_eq!(g.exp_for_level(CharacterId::Cloud, 0), Some(0));
        assert_eq!(
            g.exp_for_level(CharacterId::Cloud, 200),
            g.exp_for_level(CharacterId::Cloud, MAX_LEVEL)
        );
        assert_eq!(g.exp_to_next_level(CharacterId::Cloud, MAX_LEVEL), Some(0));
        assert_eq!(g.exp_to_next_level(CharacterId::Cloud, 255), Some(0));
        assert_eq!(
            g.exp_to_next_level(CharacterId::Cloud, 0),
            g.exp_to_next_level(CharacterId::Cloud, 1)
        );
        assert_eq!(g.level_for_exp(CharacterId::Cloud, 0), Some(1));
        assert_eq!(g.level_for_exp(CharacterId::Cloud, 14), Some(1));
        assert_eq!(g.level_for_exp(CharacterId::Cloud, 15), Some(2));
    }

    #[test]
    fn a_missing_exp_curve_is_none_not_a_guess() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let mut g = k.growth.clone();
        g.characters[0].exp_curve = 200; // no such curve
        assert_eq!(g.exp_modifier(CharacterId::Cloud, 5), None);
        assert_eq!(g.exp_for_level(CharacterId::Cloud, 1), None);
        assert_eq!(g.exp_for_level(CharacterId::Cloud, 50), None);
        assert_eq!(g.exp_to_next_level(CharacterId::Cloud, 3), None);
        assert_eq!(g.exp_to_next_level(CharacterId::Cloud, MAX_LEVEL), None);
        assert_eq!(g.level_for_exp(CharacterId::Cloud, 1000), None);
        // The other characters are unaffected.
        assert!(g.exp_for_level(CharacterId::Barret, 50).is_some());
        // A character record that is not there at all.
        g.characters.truncate(3);
        assert_eq!(g.exp_for_level(CharacterId::Cid, 5), None);
    }

    #[test]
    fn the_init_records_carry_their_experience() {
        let k = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // The synthetic init section leaves the savemap EXP field (0x3C) at zero.
        assert_eq!(k.init_character_by_id(0).unwrap().exp, 0);
        let mut sections = synthetic_kernel_sections();
        let r = 2 * INIT_CHARACTER_SIZE; // slot 2 holds character id 2
        sections[3].1[r + 0x3C..r + 0x40].copy_from_slice(&0x0001_0203u32.to_le_bytes());
        let k = Kernel::parse(&pack_kernel(&sections, &[])).unwrap();
        assert_eq!(k.init_character_by_id(2).unwrap().exp, 0x0001_0203);
        assert_eq!(k.init_character_by_id(1).unwrap().exp, 0);
    }

    /// A starting record counts only when it fits its own level on the character's curve; the
    /// synthetic Cloud (curve 55) reaches level 6 at 851 EXP and level 7 at 1,409 (see
    /// `exp_thresholds_follow_the_documented_rule`).
    #[test]
    fn starting_exp_is_the_kernels_own_when_it_fits_the_level_and_the_least_of_the_level_otherwise()
    {
        let with_cloud = |level: u8, exp: u32| {
            let mut sections = synthetic_kernel_sections();
            sections[3].1[1] = level; // Cloud's record is slot 0: its level byte
            sections[3].1[0x3C..0x40].copy_from_slice(&exp.to_le_bytes());
            Kernel::parse(&pack_kernel(&sections, &[])).unwrap()
        };
        let cloud = CharacterId::Cloud;
        // Level 6 with 861 EXP (851 <= 861 < 1409): that record is used for level 6 only.
        let k = with_cloud(6, 861);
        assert_eq!(k.growth.exp_for_level(cloud, 6), Some(851));
        assert_eq!(k.growth.exp_for_level(cloud, 7), Some(1409));
        assert_eq!(k.starting_exp(cloud, 6), Some(861));
        assert_eq!(k.starting_exp(cloud, 7), k.growth.exp_for_level(cloud, 7));
        assert_eq!(k.starting_exp(cloud, 1), Some(0));
        // A record whose EXP is not a level-6 value (5 is level 1) is a placeholder: the least of
        // the level is used instead, like for a character whose record says level 1.
        assert_eq!(with_cloud(6, 5).starting_exp(cloud, 6), Some(851));
        assert_eq!(with_cloud(6, 5000).starting_exp(cloud, 6), Some(851));
        // Characters without such a record (and levels outside 1..=99) fall back the same way.
        assert_eq!(
            k.starting_exp(CharacterId::Barret, 6),
            k.growth.exp_for_level(CharacterId::Barret, 6)
        );
        assert_eq!(k.starting_exp(cloud, 0), k.starting_exp(cloud, 1));
        assert_eq!(
            k.starting_exp(cloud, 200),
            k.growth.exp_for_level(cloud, MAX_LEVEL)
        );
        // No EXP curve, no answer.
        let mut broken = k.clone();
        broken.growth.characters[0].exp_curve = 200;
        assert_eq!(broken.starting_exp(cloud, 6), None);
    }

    #[test]
    fn rejects_structural_errors() {
        assert!(matches!(Kernel::parse(&[]), Err(KernelError::Empty)));

        // Missing typed sections.
        let only_commands = pack_kernel(&[(0, command_section())], &[]);
        assert!(matches!(
            Kernel::parse(&only_commands),
            Err(KernelError::MissingSection(2))
        ));

        // Undersized typed section.
        let mut sections = synthetic_kernel_sections();
        sections[0].1 = vec![0u8; 100];
        assert!(matches!(
            Kernel::parse(&pack_kernel(&sections, &[])),
            Err(KernelError::BadSectionSize { section: 0, .. })
        ));
        let mut sections = synthetic_kernel_sections();
        sections[2].1 = vec![0u8; 0x100];
        assert!(matches!(
            Kernel::parse(&pack_kernel(&sections, &[])),
            Err(KernelError::BadSectionSize { section: 2, .. })
        ));
        let mut sections = synthetic_kernel_sections();
        sections[3].1 = vec![0u8; 0x100];
        assert!(matches!(
            Kernel::parse(&pack_kernel(&sections, &[])),
            Err(KernelError::BadSectionSize { section: 3, .. })
        ));

        // Truncated body.
        let good = synthetic_kernel_bytes();
        assert!(matches!(
            Kernel::parse(&good[..good.len() - 40]),
            Err(KernelError::Truncated { .. }) | Err(KernelError::Gzip { .. })
        ));

        // Header lies about the decompressed size.
        let mut bad = good.clone();
        let usz = u16::from_le_bytes([bad[2], bad[3]]);
        bad[2..4].copy_from_slice(&(usz + 1).to_le_bytes());
        assert!(matches!(
            Kernel::parse(&bad),
            Err(KernelError::SizeMismatch { section: 0, .. })
        ));
        let mut bad = good.clone();
        bad[2..4].copy_from_slice(&(usz - 1).to_le_bytes());
        assert!(matches!(
            Kernel::parse(&bad),
            Err(KernelError::SizeMismatch { section: 0, .. })
        ));

        // Corrupt gzip data.
        let mut bad = good.clone();
        for b in bad.iter_mut().skip(6 + 12).take(6) {
            *b ^= 0x5A;
        }
        assert!(Kernel::parse(&bad).is_err());

        // Too many sections.
        let many: Vec<(u16, Vec<u8>)> = (0..=MAX_SECTIONS).map(|_| (9, vec![0u8; 4])).collect();
        assert!(matches!(
            Kernel::parse(&pack_kernel(&many, &[])),
            Err(KernelError::TooManySections)
        ));
    }

    #[test]
    fn tolerates_trailing_padding_variants() {
        for trailing in [
            &[][..],
            &[0u8][..],
            &[0, 0][..],
            &[0, 0, 0, 0, 0, 0, 0, 0][..],
        ] {
            let bytes = pack_kernel(&synthetic_kernel_sections(), trailing);
            let k = Kernel::parse(&bytes).unwrap();
            assert_eq!(k.sections().len(), 27);
            assert_eq!(k.trailing_bytes(), trailing.len());
        }
    }

    #[test]
    fn open_reports_missing_file() {
        assert!(matches!(
            Kernel::open("/nonexistent/definitely/not/here/kernel.bin"),
            Err(KernelError::Io(_))
        ));
    }

    // --- kernel2 / LZSS ---

    /// Minimal LZSS encoder for tests: literals only, in groups of eight.
    fn lzss_literals(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(8) {
            out.push(0xFF);
            out.extend_from_slice(chunk);
        }
        out
    }

    #[test]
    fn lzss_decodes_literals_and_overlapping_references() {
        let text = b"hello, lzss world: 0123456789";
        assert_eq!(lzss_decode(&lzss_literals(text), 1000).unwrap(), text);

        // "abc" then a reference to ring offset 4078 (where 'a' was written), length 6:
        // the copy overlaps the bytes it produces.
        let src = [0x07, b'a', b'b', b'c', 0xEE, 0xF3];
        assert_eq!(lzss_decode(&src, 100).unwrap(), b"abcabcabc");

        // A reference into the zero-initialised part of the ring yields zeros.
        let src = [0x00, 0x00, 0x00]; // flags: reference with offset 0, length 3
        assert_eq!(lzss_decode(&src, 100).unwrap(), vec![0, 0, 0]);

        // Truncated input just ends the output; output limit is enforced.
        assert_eq!(lzss_decode(&[0xFF, b'x'], 100).unwrap(), b"x");
        assert_eq!(lzss_decode(&[0x00, 0x00], 100).unwrap(), Vec::<u8>::new());
        assert!(lzss_decode(&lzss_literals(&[1u8; 100]), 50).is_err());
        assert_eq!(lzss_decode(&[], 10).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn kernel2_header_is_validated() {
        let body = lzss_literals(b"text blob");
        let mut file = (body.len() as u32).to_le_bytes().to_vec();
        file.extend_from_slice(&body);
        assert_eq!(decode_kernel2(&file).unwrap(), b"text blob");
        // Extra bytes after the declared length are ignored.
        file.extend_from_slice(&[0xAA, 0xBB]);
        assert_eq!(decode_kernel2(&file).unwrap(), b"text blob");

        assert!(matches!(decode_kernel2(&[]), Err(KernelError::Kernel2(_))));
        assert!(matches!(
            decode_kernel2(&[1, 2, 3]),
            Err(KernelError::Kernel2(_))
        ));
        // Declared length larger than the file.
        let mut lying = (1000u32).to_le_bytes().to_vec();
        lying.extend_from_slice(&body);
        assert!(matches!(
            decode_kernel2(&lying),
            Err(KernelError::Kernel2(_))
        ));
        // A hostile header cannot force a giant allocation.
        let huge = (u32::MAX).to_le_bytes();
        assert!(decode_kernel2(&huge).is_err());
    }

    #[test]
    fn lzss_expansion_is_bounded() {
        // Every 2-byte reference expands to 18 bytes; a long run of them must hit the cap
        // instead of allocating without bound.
        let mut src = Vec::new();
        for _ in 0..4000 {
            src.push(0x00);
            src.extend_from_slice(&[0x00, 0x0F].repeat(8));
        }
        assert!(lzss_decode(&src, 10_000).is_err());
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
        let mut rng = Lcg(7);
        for i in 0..64 {
            let len = [0, 1, 5, 6, 7, 100, 4096, 30000][i % 8];
            let buf: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
            // Random bytes are never a valid kernel.bin. (kernel2 / LZSS decoding of random
            // data may legitimately produce *something*, so only absence of panics is checked.)
            assert!(
                Kernel::parse(&buf).is_err(),
                "garbage of length {len} parsed"
            );
            let _ = decode_kernel2(&buf);
            let _ = lzss_decode(&buf, 1 << 16);
        }
        let good = synthetic_kernel_bytes();
        for i in 0..400 {
            let mut buf = good.clone();
            for _ in 0..(1 + i % 5) {
                let pos = if i % 3 == 0 {
                    (rng.next_u64() as usize) % 64
                } else {
                    (rng.next_u64() as usize) % buf.len()
                };
                buf[pos] = rng.next_u64() as u8;
            }
            let _ = Kernel::parse(&buf);
        }
        // Lookups on whatever parsed fine must also be total.
        let k = Kernel::parse(&good).unwrap();
        for id in 0..=255u8 {
            let _ = k.weapon(id);
            let _ = k.weapon_sounds(id);
            let _ = k.armor(id);
            let _ = k.weapon_name(id);
            let _ = k.init_character_by_id(id);
            let _ = k.materia(id);
            let _ = k.materia_name(id);
        }
        for i in 0..300 {
            let _ = k.attack(i);
            let _ = k.command(i);
            let _ = k.text_entry(i, i);
            let _ = k.item(i as u16);
            let _ = k.item_name(i as u16);
            let _ = k.magic_name(i as u16);
        }
        let _ = k.item(u16::MAX);
        let _ = k.starting_inventory();
        for who in CharacterId::ALL {
            let _ = k.equipped_spells(who);
        }
        for level in 0..=255u8 {
            for who in CharacterId::ALL {
                let _ = k.growth.stat_block(who, level);
            }
        }
    }

    /// A kernel whose weapon table has invented sound bytes (no game data): ids as the module docs describe them.
    fn kernel_with_weapon_sounds(records: &[(u8, [u8; 3])]) -> Kernel {
        let mut sections = synthetic_kernel_sections();
        let weapons = &mut sections[SECTION_WEAPONS].1;
        for (id, (mask, [hit, critical, miss])) in records.iter().enumerate() {
            let o = id * WEAPON_RECORD_SIZE;
            weapons[o + WEAPON_OFF_SOUND_MASK] = *mask;
            weapons[o + WEAPON_OFF_HIT_SOUND] = *hit;
            weapons[o + WEAPON_OFF_CRITICAL_SOUND] = *critical;
            weapons[o + WEAPON_OFF_MISS_SOUND] = *miss;
        }
        Kernel::parse(&pack_kernel(&sections, &[0, 0])).unwrap()
    }

    #[test]
    fn a_weapons_sounds_are_its_three_bytes_with_0x100_added_where_the_mask_says() {
        let k = kernel_with_weapon_sounds(&[
            // The mask of the real file has its five high bits set and only bits 0 to 2 mean anything.
            (0xF8, [18, 27, 5]),  // no bit: the bytes are the ids
            (0xFB, [36, 94, 5]),  // bits 0 and 1: the hit and the critical hit are high ids
            (0xF9, [18, 123, 47]), // bit 0 only
            (0xFC, [1, 2, 3]),    // bit 2: the miss (the owner's file never sets it)
            (0xF8, [255, 255, 255]),
            (0xFF, [255, 0, 7]),
        ]);
        let ids = |w: u8| k.weapon_sounds(w).map(|s| (s.hit, s.critical, s.miss));
        assert_eq!(ids(0), Some((18, 27, 5)));
        assert_eq!(ids(1), Some((0x100 + 36, 0x100 + 94, 5)));
        assert_eq!(ids(2), Some((0x100 + 18, 123, 47)));
        assert_eq!(ids(3), Some((1, 2, 0x100 + 3)));
        assert_eq!(ids(4), Some((255, 255, 255)), "a sound byte alone reaches 255");
        assert_eq!(ids(5), Some((0x1FF, 0x100, 0x107)), "every bit: all three are high ids");
        // The table has 128 records; past it there is nothing, and a record that is cut short is nothing too.
        assert!(k.weapon_sounds(127).is_some());
        assert_eq!(k.weapon_sounds(128), None);
        assert_eq!(k.weapon_sounds(255), None);
        // Reading the sounds changes nothing the rules read.
        assert_eq!(k.weapon(0), Kernel::parse(&synthetic_kernel_bytes()).unwrap().weapon(0));
    }
}
