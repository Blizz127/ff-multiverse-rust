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
//! * Targeting bits: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data>
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
//! | 3 | initialization data (9 savemap character records) | parsed ([`InitCharacter`]) |
//! | 5, 6 | weapons (44 bytes), armor (36 bytes) | on demand ([`WeaponRecord`], [`ArmorRecord`]) |
//! | 4, 7, 8 | items, accessories, materia | raw bytes only |
//! | 9..27 | FF text tables | raw bytes; name lookup via [`Kernel::text_entry`] |
//!
//! Raw sections are available through [`Kernel::section`].
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
/// "Names - weapons" text table.
pub const SECTION_TEXT_WEAPON_NAMES: usize = 20;
/// "Names - armor" text table.
pub const SECTION_TEXT_ARMOR_NAMES: usize = 21;

/// Command index of the Attack command.
pub const COMMAND_ATTACK: usize = 1;

pub(crate) const COMMAND_RECORD_SIZE: usize = 8;
pub(crate) const COMMAND_COUNT: usize = 32;
pub(crate) const WEAPON_RECORD_SIZE: usize = 44;
pub(crate) const ARMOR_RECORD_SIZE: usize = 36;
pub(crate) const INIT_CHARACTER_SIZE: usize = 0x84;
pub(crate) const CHARACTER_COUNT: usize = 9;
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
    /// The remaining limit-break data of the record is left raw.
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
            raw: array_at(b, 0),
        }
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
}

impl InitCharacter {
    fn parse(b: &[u8]) -> Self {
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
        }
    }
}

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
        assert_eq!(k.section(4).unwrap(), &[1, 2, 3]);
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
            let _ = k.armor(id);
            let _ = k.weapon_name(id);
            let _ = k.init_character_by_id(id);
        }
        for i in 0..300 {
            let _ = k.attack(i);
            let _ = k.command(i);
            let _ = k.text_entry(i, i);
        }
        for level in 0..=255u8 {
            for who in CharacterId::ALL {
                let _ = k.growth.stat_block(who, level);
            }
        }
    }
}
