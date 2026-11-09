//! FF7 music: the `music.idx` track list, and what a field script says about music.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-09; every other edition is unverified.**
//! Everything below is counts, ids and names, never game bytes. `tests/music_real.rs` (run with
//! `--ignored --nocapture`) and `examples/music_survey.rs` recompute every number from the
//! owner's `flevel.lgp` (702 fields) and `music/music.idx`.
//!
//! # From a script to a track
//!
//! 1. `MUSIC` (`F0`), `BMUSC` (`F6`, the music of the fight the next `BATTLE` starts) and `FMUSC`
//!    (`FC`, the music after the next fight) take one `u8` operand, always a literal in the owned
//!    data: an index into **that field's own** list of AKAO blocks
//!    ([`FieldScript::akao_blocks`](crate::script::FieldScript::akao_blocks)). Opcode pages:
//!    FFRTT "FF7/Field/Script/Opcodes" <https://wiki.ffrtt.ru/index.php/FF7/Field/Script/Opcodes>;
//!    the blocks sit after the dialogs, FFRTT "FF7/Field/Script"
//!    <https://wiki.ffrtt.ru/index.php/FF7/Field/Script>.
//! 2. An AKAO frame is `"AKAO"` and a little-endian `u16` id at +4. The id names a piece of music:
//!    the same id always comes with the same frame size (88 of the 89 ids have one size in every
//!    block that carries them; `bat`'s has two, 4 736 bytes in 251 blocks and 4 740 in two), so a
//!    frame is one fixed piece of sequence data and the id is its number, whichever field holds it.
//! 3. `music.idx` (98 lines, one track name each; `music_ogg/<name>.ogg`, or `music/<name>.wav`
//!    for `heart`, `sato`, `sensui` and `wind`) is the track list: **the 1-based line number is
//!    the frame id minus 1** ([`FRAME_ID_OFFSET`]), so id 2 is `oa`, id 8 is `bat`, id 99 is
//!    `roll`. Id 1 has no line, and no frame id of the owned data lies past the list (they
//!    run 1..=96; the list covers 2..=99).
//!
//! The community numbering of the tracks (Qhimm forum, topic 18421
//! <https://forums.qhimm.com/index.php?topic=18421.0>: `oa` = 1, `ob` = 2, `fanfare` = 5, `bat` = 7,
//! `chu` = 10, `fan2` = 46, `wind` = 65, `sensui` = 88) counts the lines of `music.idx` from 1 and
//! says nothing about frame ids. The rule above is **not** taken from it: it is the offset the
//! data single out.
//!
//! # Evidence for "line = id - 1"
//!
//! Candidate rules "line = id - k" for k = -1, 0, +1, +2 (and, for D and E, every offset
//! -96..=96 or -6..=8), all scored the same way: a field without the music, or an id with no
//! line under that k, is a miss for every k alike.
//!
//! **A and B: name regions.** 13 groups of fields whose names say what music they should have,
//! each with the track whose own name says the same, were written down before any id was
//! scored (`REGIONS` in `tests/music_common.rs`; no id and no line number enters it). The table
//! counts member fields whose arrival id (first `MUSIC` in slot 0; A) or any `MUSIC` id (B) names
//! an allowed track:
//!
//! ```text
//! region [tracks]                                  fields | A: k=-1  0 +1 +2 | B: k=-1  0 +1 +2
//! Shinra HQ blin*, sinbil_* [sinra, sinraslo] (*)      41 |       0  2 26 26 |       0  6 33 30
//! Junon streets [junon]                                14 |       0  0  0  0 |       0  0  0  0
//! Gold Saucer plaza gld* [gold1]                        4 |       0  1  0  0 |       0  2  2  0
//! Corel, North Corel [corel]                           11 |       0  0  2  0 |       0  0  2  0
//! Rocket Town [rocket]                                  8 |       0  0  1  0 |       0  0  7  0
//! Fort Condor [condor]                                  2 |       0  0  0  0 |       0  0  0  0
//! Icicle Inn, snow [snow]                               8 |       0  0  7  0 |       0  0  7  0
//! Don Corneo's mansion colne_* [corneo]                 8 |       0  0  5  0 |       0  0  5  0
//! Cosmo Canyon [canyon]                                 5 |       0  0  3  0 |       0  0  4  0
//! Costa del Sol del* [costa]                            9 |       0  0  3  0 |       0  0  4  0
//! Wutai [utai]                                          7 |       0  0  6  0 |       0  0  6  0
//! Chocobo farm [bokujo]                                 3 |       0  0  3  0 |       0  0  3  0
//! Inn rooms *inn*, any MUSIC use [yado]                16 |       0  0  2  0 |       0  0  2  0
//! all 13 regions                                      136 |       0  3 58 26 |       0  8 75 30
//! the 12 regions with one track                        95 |       0  1 32  0 |       0  2 42  0
//! ```
//!
//! (*) `sinra` and `sinraslo` are adjacent lines, so that row cannot tell +1 from +2. Its 41
//! fields have the arrival ids 31 (24 fields), 25 (4), 11 (3), 30 (2), 32 (2), 1, 8 and 83 (one
//! each) and none (3). Under +1 the 24 at id 31 (`sinraslo`) and the 2 at id 30 (`sinra`) hit, 26;
//! under +2 the same 24 (31 is now `sinra`) and the 2 at id 32 (now `sinraslo`) hit, 26: a tie by
//! coincidence. The one-track rows do separate the offsets: no field of any of them lands on an
//! allowed track under -1 or +2, while under 0 only the Gold Saucer fields do (1 by A, 2 by B).
//! Junon scores zero under every k because its fields carry
//! other music (`parade`, `gun`, `mati`, `kita`, `over2`) or none at all (7 of its 14 fields have
//! no AKAO block); Fort Condor because `condor` (id 81) is carried by no field at all (one of its
//! fields has no block, the other only a `siera` frame).
//!
//! **C: the standard battle theme.** The 313 distinct (field, `BMUSC` operand) uses pick 250 times
//! the frame id 8 and 26 times id 11 (then 9 × id 55, 7 × id 3, 5 × id 25, 3 × id 53 and 84, ...).
//! The track called `bat` is named by 250 of the 313 under k = +1 and by none under k = -1, 0
//! and +2 (id 8 is then `kurai`, `fiddle` or `makoro`); id 11 is `chu`, the next fight track.
//!
//! **D: every offset.** Counting, for each offset `d` in -96..=96, the member fields (of the
//! one-track regions) that have an id equal to `line(track) + d`: the arrival id gives `d = +1`
//! for 30 of 79 fields, any `MUSIC` id for 42 of 95 and any frame id for 50 of 95; the best other
//! offset (+28, a coincidence between Rocket Town and North Corel, whose tracks and ids are both
//! six apart) gets 11, the next best 3, 5 and 5.
//!
//! **E: frame size against audio length (no field name used).** The size of an id's AKAO frame in
//! bytes against the length in seconds of the track line `id - k` names, 88 pairs per k (the
//! 28-byte frames of id 1 hold no sequence and are left out for every k): Pearson r is **0.58**
//! for k = +1 and between -0.24 and +0.22 for every other k from -6 to +8 (+2: 0.21, 0: 0.14,
//! -1: -0.07). The four smallest frames, 148, 244, 476 and 500 bytes, name `cintro` (12.5 s),
//! `fanfare` (5.2 s), `si` (21.6 s) and `yado` (8.8 s), four of the six shortest tracks the
//! fields reference.
//!
//! **Reading it backwards** (`music_survey --tracks`), with k = +1: `makoro` is the music of the
//! 20 Mako-reactor fields (`nmkin`, `smkin`, `semkin`, `nvmkin`, `southmk`), `canyon` of Cosmo
//! Canyon, `snow` of the Great Glacier and Icicle Inn (33 fields), `utai` of Wutai and Da-chao
//! (24), `costa` of Costa del Sol, `bokujo` of the farm, `sadbar` of North Corel, `sadsid` and
//! `sid2` of Cid's house and the Highwind, `dun2` of the dungeons (Ancient Forest, Sleeping
//! Forest, Northern Crater, ...), `oa` of `md1stin` alone. The only unnamed id is 1.
//!
//! # What `arrival` and the other accessors mean
//!
//! * [`FieldMusic::arrival`] is the first `MUSIC` in slot 0 (an entity's own init and main
//!   script) in script order. Of the 702 fields, 587 have one: **402 name one distinct id, 185
//!   several** (`ancnt1[10, 38]`, `astage_a[55, 56]`, `bigwheel[55, 42, 50, 56]`, `blin1[31, 3]`,
//!   `blin2[31, 3]`, ...). The alternatives are story branches: in 165 of the 185 the first use
//!   sits behind a forward jump ([`MusicUse::conditional`]) and in 161 every use does (a chain of
//!   `IF`s on story progress that this module does not evaluate). 356 of the 587 fields have
//!   exactly one unconditional `MUSIC` in the init part (230 have none, one has two): in 355 of
//!   them `arrival` is that use (the exception, `subin_3`, has a branch-guarded `MUSIC` ahead of
//!   it, which wins when its branch is taken), and in 18 a `MUSIC` of another id in the main part
//!   can replace it later. For the fields with several ids `arrival` is one of several story
//!   alternatives; [`FieldMusic::distinct_music`] and the `conditional` and `phase` fields show
//!   which. `None` for 115 fields: 89 have no AKAO block at all (the previous field's music goes
//!   on), 25 only play music from other slots, one has blocks and no `MUSIC`.
//! * The same `MUSIC` in any slot: 612 fields use it, 375 with one distinct id, 237 with several.
//! * [`FieldMusic::battle_theme`] is the first `BMUSC` in any slot: 276 fields have one (241 with
//!   one id, 35 with several; 303 of the 347 instructions are in slot 0, the field's own setting,
//!   44 in other slots). 271 of the 276 set one in slot 0 and in none of those does a `BMUSC` of
//!   a later slot come first, so the first one is the field's own setting; five fields
//!   (`blackbg7`, `convil_2`, `junele1`, `lastmap`, `trackin`) have `BMUSC` only in slots >= 1
//!   (scripted fights), and 20 fields have several distinct slot-0 ids. `None` (426 fields)
//!   leaves the game's standard theme
//!   (`bat`, frame id 8); whether the standard theme is `bat` for a field that sets none is not
//!   proven, but 80 % of all explicit uses pick it.
//! * [`FieldMusic::after_battle`] (`FMUSC`): 23 fields, 22 with one id; for the 21 that also have
//!   an arrival the id is the same in 8.
//! * 41 fields have id 1 as their arrival (no track: keep or stop the music). 121 frames carry
//!   id 1, all 28 bytes, and 202 instructions pick them (181 `MUSIC`, 11 `FMUSC`, 10 `MULCK`);
//!   117 fields use it in some `MUSIC`. It is not a track; it most likely means "no music".
//! * Slots, sharing and reachability: unused slots share one entry point, so a script can be
//!   referenced by many slots (331 of the 1 725 music-related instructions are); the first
//!   `(entity, slot)` that points at a script owns its instructions. No script with music is
//!   referenced by two entities, nor by slot 0 and another slot, so this loses nothing for
//!   `arrival`. Two music-related instructions lie in unreachable code (`jtemplc`, slots 3 and
//!   4, neither slot 0).
//!
//! The one-shot jingles: `fanfare` (id 6, 244 bytes, 5.2 s; the forum calls it the Junon parade
//! fanfare) is started in five fields: by `MUSIC` in `delmin1` (a talk script), `subin_1a` and
//! `subin_1b` (init part, behind a branch) and by `MUSVT` in `jundoc1a` and `junin1a` (the Junon
//! dock and ceremony). `fan2` (id 47, the 54 s looping fanfare) only in `frcyo`, by six `CMUSC`
//! and one `MUSIC`, all in talk scripts and behind a branch. Neither is ever a `BMUSC` or
//! `FMUSC`, so the fanfare after a fight is not chosen by a field script.
//!
//! The opening, maplist ids 116..=121 (`md1stin`, `md1_1`, `md1_2`, `nrthmk`, `nmkin_1`,
//! `elevtr1`: the two names bound a contiguous run of six fields, `nmkin_2..5` follow at
//! 122..=125): arrival `ob`, `ob`, `ob`, `ob`, `makoro`, none. `md1stin` also has `oa` (slot 3 of
//! another entity) and `FMUSC` `ob`; the reactor's fights use `ob` and `bat`.
//!
//! # Known gaps
//!
//! * Conditions are never evaluated (see above). `MUSVT` (3 instructions, all `fanfare`), `CMUSC`
//!   (32; the `music` operand picks a block like `MUSIC`'s, `op` is a command), `MUSVM` (never
//!   used), `MULCK` (62) and the generic `AKAO`/`AKAO2` instructions (351 fields) are not read;
//!   only two fields without a slot-0 `MUSIC` have a `CMUSC` as well.
//! * The meaning of id 1 is inferred (28-byte frame, never a track), not decoded.

use crate::field::FieldFile;
use crate::script::{Disassembly, FieldScript, Phase, ScriptError, INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use crate::util::u16_at;
use std::collections::BTreeSet;

/// Most bytes of `music.idx` that [`MusicList::parse`] reads (the real file is 647 bytes). A longer
/// input is cut at its last complete line inside the bound.
pub const MAX_LIST_BYTES: usize = 64 * 1024;
/// Most lines of `music.idx` that [`MusicList::parse`] keeps (the real file has 98).
pub const MAX_TRACKS: usize = 1024;
/// Longest accepted track name in bytes; a longer line is a slot without a track.
pub const MAX_TRACK_NAME_LEN: usize = 64;

/// How a field script's AKAO frame id relates to a line of `music.idx`: the 1-based line number is
/// `frame id - FRAME_ID_OFFSET`, so id 2 is the first line and id 1 (and 0) name no track.
pub const FRAME_ID_OFFSET: u16 = 1;

/// `MUSIC` opcode (FFRTT "FF7/Field/Script/Opcodes", `F0 MUSIC`): one `u8` operand, an index into
/// the field's own list of AKAO blocks. The opcode table in `crate::script` names the three
/// opcodes below the same way (a unit test pins that).
const OP_MUSIC: u8 = 0xF0;
/// `BMUSC` (`F6`): the same operand, the music of the battle the next `BATTLE` starts.
const OP_BMUSC: u8 = 0xF6;
/// `FMUSC` (`FC`): the same operand, the music that plays after the next battle.
const OP_FMUSC: u8 = 0xFC;
/// Name of the operand of all three opcodes in the opcode table.
const OPERAND: &str = "id";
/// An AKAO frame is `"AKAO"`, a little-endian `u16` id, then its length: the id is complete when
/// the block holds at least this many bytes.
const FRAME_HEADER_LEN: usize = 6;
/// Offset of the id inside an AKAO frame.
const FRAME_ID_AT: usize = 4;
/// Opcode byte of `RET`, which ends the init part of script 0 (see [`crate::script::Phase`]).
const OP_RET: u8 = 0x00;

/// The track list of `music.idx`: one track name per line, in file order.
///
/// A slot is a track name (ASCII lower case, `[a-z0-9_-]`, at most [`MAX_TRACK_NAME_LEN`] bytes) or
/// empty, which means "no track" (a blank line, or a line that is not a valid name). Empty slots
/// keep their position so that the lines after them keep their numbers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MusicList {
    names: Vec<String>,
}

impl MusicList {
    /// Parse the bytes of `music.idx`. Never fails and never reads more than
    /// [`MAX_LIST_BYTES`] bytes or keeps more than [`MAX_TRACKS`] lines: CRLF and LF both end a
    /// line, names are trimmed and lower-cased, a leading UTF-8 byte-order mark is ignored, and
    /// the slots without a track at the end (blank lines, in practice the file's final line end)
    /// are dropped.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut window = bytes.get(..MAX_LIST_BYTES).unwrap_or(bytes);
        if bytes.len() > MAX_LIST_BYTES {
            // A line cut in half by the bound would read as a different, shorter name: drop it.
            window = match window.iter().rposition(|&b| b == b'\n') {
                Some(end) => window.get(..=end).unwrap_or(&[]),
                None => &[],
            };
        }
        let window = window.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(window);
        let mut names: Vec<String> = window.split(|&b| b == b'\n').take(MAX_TRACKS).map(track_name).collect();
        while names.last().is_some_and(String::is_empty) {
            names.pop();
        }
        Self { names }
    }

    /// Number of slots (lines up to the last non-blank one).
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// All slots in file order; an empty string is a slot without a track.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Track name of slot `index` (0-based, so line `index + 1`); `None` past the end or for a
    /// slot without a track.
    pub fn name_at(&self, index: usize) -> Option<&str> {
        self.names.get(index).map(String::as_str).filter(|n| !n.is_empty())
    }

    /// Slot (0-based) of the first track called `name` (ASCII case-insensitive).
    pub fn index_of(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return None;
        }
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// The track an AKAO frame id names: line `id - FRAME_ID_OFFSET` of the file, counted from 1.
    /// An id that has no line (0, 1, anything past the list) and an id that lands on a blank slot
    /// give `None`; nothing is ever guessed.
    pub fn track_for_frame_id(&self, id: u16) -> Option<&str> {
        let index = usize::from(id).checked_sub(usize::from(FRAME_ID_OFFSET) + 1)?;
        self.name_at(index)
    }

    /// The frame id that names track `name`: the inverse of [`MusicList::track_for_frame_id`].
    pub fn frame_id_of(&self, name: &str) -> Option<u16> {
        let index = self.index_of(name)?;
        u16::try_from(index.checked_add(1)?.checked_add(usize::from(FRAME_ID_OFFSET))?).ok()
    }
}

/// One line of `music.idx` as a track name; empty when the line is blank or not a valid name.
fn track_name(line: &[u8]) -> String {
    let line = trim_ascii(line);
    let valid = !line.is_empty()
        && line.len() <= MAX_TRACK_NAME_LEN
        && line.iter().all(|b| matches!(b.to_ascii_lowercase(), b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
    if valid {
        line.iter().map(|&b| char::from(b.to_ascii_lowercase())).collect()
    } else {
        String::new()
    }
}

/// `<[u8]>::trim_ascii` (stable only since Rust 1.80, newer than this workspace promises).
fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if !first.is_ascii_whitespace() {
            break;
        }
        s = rest;
    }
    while let [rest @ .., last] = s {
        if !last.is_ascii_whitespace() {
            break;
        }
        s = rest;
    }
    s
}

/// One `MUSIC`, `BMUSC` or `FMUSC` instruction whose operand picks a real AKAO frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MusicUse {
    /// The id stored in the AKAO frame the operand picks: what
    /// [`MusicList::track_for_frame_id`] takes.
    pub frame_id: u16,
    /// The instruction's operand: an index into the field's own list of AKAO blocks.
    pub block: u8,
    /// The entity whose script holds the instruction.
    pub entity: usize,
    /// The first script slot (0..32) that points at the script holding the instruction (several
    /// slots can share one script, see [`field_music`]).
    pub slot: usize,
    /// For slot 0 (the entity's own init and main script) the part the instruction lies in;
    /// `None` for every other slot.
    pub phase: Option<Phase>,
    /// Section-relative offset of the instruction.
    pub offset: usize,
    /// The instruction lies between a forward jump and its target, so it only runs on some
    /// paths (in practice a story-progress `IF`): the field's music then depends on the story,
    /// and the other uses of the same field are its alternatives. The same meaning as
    /// [`crate::script::Tracked::conditional`], but taken from every forward jump of the script,
    /// reachable or not: a jump in dead code flags the live code behind it too (two of the 1 625
    /// reachable music instructions of the owned data, both in `southmk2` slot 3; the
    /// reachability-aware rule agrees on the other 1 623, and on every instruction of slot 0).
    pub conditional: bool,
}

/// Every `MUSIC`, `BMUSC` and `FMUSC` of one field script, resolved to AKAO frame ids, each list
/// in script order: entity ascending, slot ascending, instruction order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldMusic {
    /// `MUSIC` instructions.
    pub music: Vec<MusicUse>,
    /// `BMUSC` instructions: the music of the fight the next `BATTLE` starts.
    pub battle: Vec<MusicUse>,
    /// `FMUSC` instructions: the music that plays after the next fight.
    pub after_battle: Vec<MusicUse>,
    /// Instructions left out of the three lists: the operand is past the field's AKAO block list,
    /// or the block it picks is not a real `"AKAO"` frame (tutorial blocks) or too short to hold
    /// an id.
    pub skipped: usize,
}

impl FieldMusic {
    /// The frame id that plays when the field loads: the first `MUSIC` in slot 0 of any entity,
    /// that is, an entity's own init or main script. Uses in other slots run when the player
    /// talks to or touches something, or crosses a line, and are not what plays on arrival.
    /// `None` means the field sets no music itself (the previous field's music goes on).
    pub fn arrival(&self) -> Option<u16> {
        self.music.iter().find(|u| u.slot == INIT_SCRIPT).map(|u| u.frame_id)
    }

    /// The first `BMUSC` of the field, in any slot. `None` when the field sets none: the caller
    /// then plays the game's standard battle theme.
    pub fn battle_theme(&self) -> Option<u16> {
        self.battle.first().map(|u| u.frame_id)
    }

    /// The first `FMUSC` of the field, in any slot.
    pub fn after_battle(&self) -> Option<u16> {
        self.after_battle.first().map(|u| u.frame_id)
    }

    /// The distinct frame ids of all `MUSIC` uses (any slot), in order of first use.
    pub fn distinct_music(&self) -> Vec<u16> {
        distinct(&self.music)
    }

    /// The distinct frame ids of all `BMUSC` uses, in order of first use.
    pub fn distinct_battle(&self) -> Vec<u16> {
        distinct(&self.battle)
    }

    /// The distinct frame ids of all `FMUSC` uses, in order of first use.
    pub fn distinct_after_battle(&self) -> Vec<u16> {
        distinct(&self.after_battle)
    }
}

fn distinct(uses: &[MusicUse]) -> Vec<u16> {
    let mut seen = BTreeSet::new();
    uses.iter().map(|u| u.frame_id).filter(|id| seen.insert(*id)).collect()
}

/// The `[from, to)` byte regions a forward jump skips on some path: from the end of each forward
/// jump instruction to its target.
fn skipped_regions(d: &Disassembly) -> Vec<(usize, usize)> {
    d.instructions
        .iter()
        .filter(|ins| ins.is_jump())
        .filter_map(|ins| ins.jump_target.filter(|&target| target > ins.offset).map(|target| (ins.end(), target)))
        .collect()
}

/// `(block index, frame id)` of every real AKAO block that is long enough to hold an id.
fn frame_ids(script: &FieldScript<'_>) -> Vec<(usize, u16)> {
    script
        .akao_blocks()
        .iter()
        .filter(|b| b.is_akao() && b.len >= FRAME_HEADER_LEN)
        .filter_map(|b| {
            let at = usize::try_from(b.offset).ok()?.checked_add(FRAME_ID_AT)?;
            Some((b.index, u16_at(script.bytes(), at)?))
        })
        .collect()
}

/// Collect the music instructions of `script`.
///
/// Each distinct script is walked once (several slots, mostly unused ones, share an entry point;
/// the first slot that points at a script owns its instructions, like
/// [`FieldScript::map_jumps`]). A script that stops decoding early still contributes the
/// instructions before the bad byte.
pub fn field_music(script: &FieldScript<'_>) -> FieldMusic {
    let mut out = FieldMusic::default();
    let frames = frame_ids(script);
    let mut walked = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !walked.insert(span.start) {
                continue;
            }
            let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
            // Script 0 is the init part up to its first RET and the main part after it.
            let first_ret = if slot == INIT_SCRIPT { d.instructions.iter().find(|i| i.opcode == OP_RET).map(|i| i.offset) } else { None };
            let skipped = skipped_regions(&d);
            for ins in &d.instructions {
                let list = match ins.opcode {
                    OP_MUSIC => &mut out.music,
                    OP_BMUSC => &mut out.battle,
                    OP_FMUSC => &mut out.after_battle,
                    _ => continue,
                };
                let block = ins.literal(OPERAND).and_then(|v| u8::try_from(v).ok());
                let frame_id = block.and_then(|b| frames.iter().find(|&&(index, _)| index == usize::from(b)).map(|&(_, id)| id));
                let (Some(block), Some(frame_id)) = (block, frame_id) else {
                    out.skipped += 1;
                    continue;
                };
                let phase = (slot == INIT_SCRIPT).then_some(match first_ret {
                    Some(ret) if ins.offset > ret => Phase::Main,
                    _ => Phase::Init,
                });
                let conditional = skipped.iter().any(|&(from, to)| from <= ins.offset && ins.offset < to);
                list.push(MusicUse { frame_id, block, entity, slot, phase, offset: ins.offset, conditional });
            }
        }
    }
    out
}

/// [`field_music`] of a decompressed field file's script section.
pub fn field_music_of(field: &FieldFile) -> Result<FieldMusic, ScriptError> {
    Ok(field_music(&FieldScript::from_field(field)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::mnemonic;
    use crate::script::synth::SynthScript;

    /// `names.len()` invented track names `t1`, `t2`, ... one per line.
    fn invented(count: usize) -> Vec<u8> {
        (1..=count).map(|n| format!("t{n}\r\n")).collect::<String>().into_bytes()
    }

    #[test]
    fn parse_accepts_crlf_and_lf_trims_and_lowercases() {
        let l = MusicList::parse(b"OA\r\n  Ob \nDun2\r\n\r\n");
        assert_eq!(l.names(), ["oa", "ob", "dun2"]);
        assert_eq!(l.len(), 3);
        assert!(!l.is_empty());
        assert!(MusicList::parse(b"").is_empty());
        assert!(MusicList::parse(b"\r\n\r\n\n").is_empty());
    }

    #[test]
    fn a_blank_line_in_the_middle_keeps_its_slot_and_trailing_blanks_go() {
        let l = MusicList::parse(b"a\r\n\r\nb\r\n\r\n\r\n");
        assert_eq!(l.names(), ["a", "", "b"]);
        assert_eq!(l.name_at(0), Some("a"));
        assert_eq!(l.name_at(1), None);
        assert_eq!(l.name_at(2), Some("b"));
        assert_eq!(l.name_at(3), None);
        // the blank slot names no track, and `b` keeps frame id 4 (line 3 + FRAME_ID_OFFSET)
        assert_eq!(l.track_for_frame_id(3), None);
        assert_eq!(l.track_for_frame_id(4), Some("b"));
        assert_eq!(l.index_of(""), None);
        assert_eq!(l.index_of("B"), Some(2));
    }

    #[test]
    fn invalid_names_are_slots_without_a_track() {
        let long = "x".repeat(MAX_TRACK_NAME_LEN);
        let too_long = "x".repeat(MAX_TRACK_NAME_LEN + 1);
        let text = format!("ok_1-a\r\nhas space\r\nfile.ogg\r\ncaf\u{e9}\r\n{long}\r\n{too_long}\r\nnul\0\r\nlast\r\n");
        let l = MusicList::parse(text.as_bytes());
        let names: Vec<&str> = l.names().iter().map(String::as_str).collect();
        assert_eq!(names, ["ok_1-a", "", "", "", long.as_str(), "", "", "last"]);
        // the invalid lines still count: `last` is line 8
        assert_eq!(l.frame_id_of("last"), Some(8 + FRAME_ID_OFFSET));
    }

    #[test]
    fn a_byte_order_mark_and_stray_carriage_returns_do_not_matter() {
        let l = MusicList::parse(b"\xEF\xBB\xBFoa\r\nob\r\r\nhi\n");
        assert_eq!(l.names(), ["oa", "ob", "hi"]);
        // a bare CR is not a line end: inside a line it makes the name invalid (a slot without a
        // track), and trailing slots without a track are dropped
        assert_eq!(MusicList::parse(b"a\rb\nc\n").names(), ["", "c"]);
        assert!(MusicList::parse(b"a\rb\n").is_empty());
    }

    #[test]
    fn the_frame_id_rule_and_its_edges() {
        let l = MusicList::parse(&invented(98));
        assert_eq!(l.len(), 98);
        // line = id - FRAME_ID_OFFSET: the first line is id 2, the last (98th) id 99
        assert_eq!(FRAME_ID_OFFSET, 1);
        assert_eq!(l.track_for_frame_id(0), None);
        assert_eq!(l.track_for_frame_id(1), None);
        assert_eq!(l.track_for_frame_id(2), Some("t1"));
        assert_eq!(l.track_for_frame_id(3), Some("t2"));
        assert_eq!(l.track_for_frame_id(99), Some("t98"));
        assert_eq!(l.track_for_frame_id(100), None);
        assert_eq!(l.track_for_frame_id(u16::MAX), None);
        // the inverse agrees for every track
        for (index, name) in l.names().iter().enumerate() {
            let id = l.frame_id_of(name).unwrap();
            assert_eq!(usize::from(id), index + 2);
            assert_eq!(l.track_for_frame_id(id), Some(name.as_str()));
        }
        assert_eq!(l.frame_id_of("nope"), None);
        assert_eq!(MusicList::default().track_for_frame_id(2), None);
    }

    #[test]
    fn input_and_entry_counts_are_bounded() {
        // far more lines than the cap: only the first MAX_TRACKS are kept
        let many = invented(MAX_TRACKS * 3);
        assert!(many.len() < MAX_LIST_BYTES);
        let l = MusicList::parse(&many);
        assert_eq!(l.len(), MAX_TRACKS);
        assert_eq!(l.name_at(MAX_TRACKS - 1), Some(format!("t{MAX_TRACKS}").as_str()));
        // longer than the byte bound: the line the bound cuts is dropped, never read short.
        // 66-byte lines (64 name bytes + CRLF): 992 whole lines fit in 65 536 bytes, the 993rd
        // is cut after its 64th byte, which would otherwise read as a complete name.
        let name = "n".repeat(MAX_TRACK_NAME_LEN);
        let big = format!("{name}\r\n").repeat(2000).into_bytes();
        assert!(big.len() > MAX_LIST_BYTES);
        let l = MusicList::parse(&big);
        assert_eq!(l.len(), MAX_LIST_BYTES / 66);
        assert!(l.names().iter().all(|n| *n == name));
        // no newline at all inside the bound: nothing to keep
        assert!(MusicList::parse(&vec![b'a'; MAX_LIST_BYTES + 1]).is_empty());
        // exactly at the bound nothing is dropped: 1008 lines of 65 bytes and one of 16 bytes
        // make 65 536
        let mut exact = format!("{name}\n").repeat(1008).into_bytes();
        exact.extend_from_slice(b"last_track_name\n");
        assert_eq!(exact.len(), MAX_LIST_BYTES);
        let l = MusicList::parse(&exact);
        assert_eq!(l.len(), 1009);
        assert_eq!(l.name_at(1008), Some("last_track_name"));
        // one more byte and the input is over the bound: the cut-off extra line end changes nothing
        exact.push(b'\n');
        assert_eq!(MusicList::parse(&exact).len(), 1009);
    }

    #[test]
    fn the_opcode_table_names_the_three_music_opcodes() {
        assert_eq!(mnemonic(OP_MUSIC), Some("MUSIC"));
        assert_eq!(mnemonic(OP_BMUSC), Some("BMUSC"));
        assert_eq!(mnemonic(OP_FMUSC), Some("FMUSC"));
        assert_eq!(mnemonic(OP_RET), Some("RET"));
    }

    /// A real-looking AKAO frame: tag, id, then padding to 12 bytes.
    fn akao(id: u16) -> Vec<u8> {
        let mut v = b"AKAO".to_vec();
        v.extend_from_slice(&id.to_le_bytes());
        v.extend_from_slice(&[0; 6]);
        v
    }

    /// A tutorial block: anything but an AKAO tag.
    fn tutorial() -> Vec<u8> {
        b"TUTO\0\0\0\0\0\0\0\0".to_vec()
    }

    /// `(entity name, [(slot, code)])` for each entity of a synthetic section.
    type Entities<'a> = Vec<(&'a str, Vec<(usize, Vec<u8>)>)>;

    fn section(entities: Entities<'_>, blocks: Vec<Vec<u8>>) -> Vec<u8> {
        let mut s = SynthScript { akao: blocks, ..SynthScript::default() };
        for (name, scripts) in entities {
            s = s.entity(name, scripts);
        }
        s.build()
    }

    fn music_of(entities: Entities<'_>, blocks: Vec<Vec<u8>>) -> FieldMusic {
        let bytes = section(entities, blocks);
        field_music(&FieldScript::parse(&bytes).unwrap())
    }

    const RET: u8 = 0x00;

    #[test]
    fn arrival_is_the_first_slot_0_music_and_later_slots_are_not_arrival() {
        // entity 0 only plays music when talked to (slot 1: block 0 = id 10); entity 1's own
        // script plays block 1 (id 20), and its talk script block 2 (id 30)
        let m = music_of(
            vec![
                ("npc", vec![(1, vec![OP_MUSIC, 0, RET])]),
                ("dir", vec![(0, vec![OP_MUSIC, 1, RET]), (1, vec![OP_MUSIC, 2, RET])]),
            ],
            vec![akao(10), akao(20), akao(30)],
        );
        assert_eq!(m.arrival(), Some(20));
        // script order: entity ascending, slot ascending
        let seen: Vec<(u16, u8, usize, usize)> = m.music.iter().map(|u| (u.frame_id, u.block, u.entity, u.slot)).collect();
        assert_eq!(seen, [(10, 0, 0, 1), (20, 1, 1, 0), (30, 2, 1, 1)]);
        assert_eq!(m.distinct_music(), [10, 20, 30]);
        assert_eq!(m.skipped, 0);
        assert_eq!((m.battle_theme(), m.after_battle()), (None, None));
        // only slots >= 1 play music: nothing on arrival
        let talk_only = music_of(vec![("npc", vec![(1, vec![OP_MUSIC, 0, RET])])], vec![akao(10)]);
        assert_eq!(talk_only.arrival(), None);
        assert_eq!(talk_only.music.len(), 1);
    }

    #[test]
    fn init_and_main_parts_of_script_0_are_told_apart() {
        // init: MUSIC 0 RET   main: MUSIC 1 (after the first RET)
        let m = music_of(vec![("dir", vec![(0, vec![OP_MUSIC, 0, RET, OP_MUSIC, 1, RET])])], vec![akao(7), akao(9)]);
        let phases: Vec<(u16, Option<Phase>)> = m.music.iter().map(|u| (u.frame_id, u.phase)).collect();
        assert_eq!(phases, [(7, Some(Phase::Init)), (9, Some(Phase::Main))]);
        // the first use in script order is the arrival
        assert_eq!(m.arrival(), Some(7));
        // other slots have no phase
        let talk = music_of(vec![("npc", vec![(1, vec![OP_MUSIC, 0, RET])])], vec![akao(7)]);
        assert_eq!(talk.music.first().map(|u| u.phase), Some(None));
        // the offsets are section-relative and increase with the instruction order
        assert!(m.music.windows(2).all(|w| w[0].offset < w[1].offset));
    }

    #[test]
    fn a_use_behind_a_forward_jump_is_conditional() {
        // JMPF 3 at +0: the distance counts from its operand (+1), so it lands at +4 and skips
        // the MUSIC 0 at +2; MUSIC 1 at +4 is outside the skipped region and always runs.
        let jmpf = 0x10;
        let m = music_of(vec![("dir", vec![(0, vec![jmpf, 3, OP_MUSIC, 0, OP_MUSIC, 1, OP_BMUSC, 1, RET])])], vec![akao(7), akao(9)]);
        let flags: Vec<(u16, bool)> = m.music.iter().map(|u| (u.frame_id, u.conditional)).collect();
        assert_eq!(flags, [(7, true), (9, false)]);
        assert_eq!(m.battle.iter().map(|u| u.conditional).collect::<Vec<_>>(), [false]);
        // a backward jump (an idle loop) does not make anything conditional
        let jmpb = 0x12;
        let looped = music_of(vec![("dir", vec![(0, vec![OP_MUSIC, 0, RET, OP_MUSIC, 1, jmpb, 3])])], vec![akao(7), akao(9)]);
        assert!(looped.music.iter().all(|u| !u.conditional), "{looped:?}");
    }

    #[test]
    fn battle_and_after_battle_music_are_collected_separately() {
        let m = music_of(
            vec![("dir", vec![(0, vec![OP_BMUSC, 1, OP_FMUSC, 0, OP_BMUSC, 2, OP_FMUSC, 0, OP_MUSIC, 2, RET])])],
            vec![akao(3), akao(8), akao(11)],
        );
        assert_eq!(m.battle_theme(), Some(8));
        assert_eq!(m.distinct_battle(), [8, 11]);
        assert_eq!(m.after_battle(), Some(3));
        assert_eq!(m.distinct_after_battle(), [3]);
        assert_eq!(m.arrival(), Some(11));
        assert_eq!(m.battle.len(), 2);
        assert_eq!(m.after_battle.len(), 2);
        assert_eq!(m.music.len(), 1);
    }

    #[test]
    fn operands_past_the_block_list_and_tutorial_blocks_are_skipped_and_counted() {
        // blocks: 0 = a real frame, 1 = a tutorial. MUSIC 1 and BMUSC 1 pick the tutorial block,
        // MUSIC 7 and FMUSC 255 are past the list; only MUSIC 0 and FMUSC 0 are usable.
        let m = music_of(
            vec![("dir", vec![(0, vec![OP_MUSIC, 1, OP_MUSIC, 7, OP_BMUSC, 1, OP_FMUSC, 255, OP_MUSIC, 0, OP_FMUSC, 0, RET])])],
            vec![akao(5), tutorial()],
        );
        assert_eq!(m.skipped, 4);
        assert_eq!(m.distinct_music(), [5]);
        assert_eq!(m.battle_theme(), None);
        assert_eq!(m.after_battle(), Some(5));
        assert_eq!(m.arrival(), Some(5));
        // a field without any block skips every use
        let none = music_of(vec![("dir", vec![(0, vec![OP_MUSIC, 0, OP_BMUSC, 0, OP_FMUSC, 0, RET])])], vec![]);
        assert_eq!(none.skipped, 3);
        assert_eq!(none.arrival(), None);
        assert!(none.music.is_empty() && none.battle.is_empty() && none.after_battle.is_empty());
    }

    #[test]
    fn a_block_too_short_to_hold_an_id_is_skipped() {
        // block 0 is 5 bytes ("AKAO" and one byte) before block 1 begins, block 1 is whole
        let m = music_of(vec![("dir", vec![(0, vec![OP_MUSIC, 0, OP_MUSIC, 1, RET])])], vec![b"AKAO\x01".to_vec(), akao(0x1234)]);
        assert_eq!(m.skipped, 1);
        assert_eq!(m.distinct_music(), [0x1234]);
    }

    #[test]
    fn other_opcodes_and_shared_returns_are_not_music() {
        // a field whose scripts hold no music opcode: only RETs, a placement-free CHAR
        let m = music_of(vec![("a", vec![(0, vec![0xA1, 0, RET])]), ("b", vec![])], vec![akao(2)]);
        assert_eq!(m, FieldMusic::default());
        // MUSVT (0xF3) takes the same one-byte operand but is not one of the three
        let vt = music_of(vec![("a", vec![(0, vec![0xF3, 0, RET])])], vec![akao(2)]);
        assert_eq!(vt, FieldMusic::default());
    }

    #[test]
    fn duplicate_frame_ids_and_distinct_order() {
        // blocks 0 and 2 carry the same id; first-use order is kept
        let m = music_of(
            vec![("dir", vec![(0, vec![OP_MUSIC, 1, OP_MUSIC, 0, OP_MUSIC, 2, OP_MUSIC, 1, RET])])],
            vec![akao(40), akao(41), akao(40)],
        );
        assert_eq!(m.distinct_music(), [41, 40]);
        assert_eq!(m.music.len(), 4);
    }

    #[test]
    fn garbage_sections_do_not_panic() {
        assert!(FieldScript::parse(&[0u8; 8]).is_err());
        // a section that parses but whose scripts are junk still yields a FieldMusic
        let mut bytes = section(vec![("dir", vec![(0, vec![OP_MUSIC, 0, 0xEE, 0xEE, 0xEE, RET])])], vec![akao(5)]);
        let script = FieldScript::parse(&bytes).unwrap();
        let _ = field_music(&script);
        bytes.truncate(bytes.len() - 3);
        if let Ok(script) = FieldScript::parse(&bytes) {
            let _ = field_music(&script);
        }
    }
}
