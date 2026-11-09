//! `..ab`: the animation scripts of a battle model, and the decoder that says which clips of the
//! model's pack each of them plays.
//!
//! A model's pack of clips ([`super::animation`]) is only the raw material. WHAT the game plays for an
//! action (an attack, a spell, an item, a hurt reaction) is a small **script** of the model's own, in its `ab`
//! entry, that strings clips together with movements ("hop forward, throw, hop back"); the engine runs the
//! script whose index the action names. The scripts of every playable character answer to the same
//! indices (20 is a blow with a short-range weapon, 32 an item used on an ally...) but play the clips of the
//! character's own pack, which differ: what plays for an action is read from the character's own script, not
//! from one table of slots.
//!
//! # Layout
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-09** against the `ab` entries of the nine playable
//! characters and a few enemies (the ignored real-data tests of [`super`] and of `fmv-client`'s battle sheets):
//!
//! * A header of [`HEADER_LEN`] bytes (26 little-endian words, the model's settings: not read here), then a
//!   table of little-endian `u32` **absolute offsets** of the scripts, one per script index, running up to the
//!   lowest of them. A character has 74 scripts (the first starts at byte 400), an enemy 32 (at byte 232): the
//!   count is not stored, the table ends where the lowest script starts. A script that is not used points at a
//!   short one that only plays the idle clip (several indices share one offset: 400 for most of the unused ones),
//!   or, in Sephiroth's table and in the first entry of one enemy's (`hb`), at nothing (a zero: the script is
//!   absent). All 391 models that are not battle locations have a table that parses (13 309 scripts, 62 absent).
//! * A script is a string of bytes: a code **below `0x8E` is a clip index** (play that slot of the pack and
//!   wait for it), a code from `0x8E` up is an opcode with a fixed number of argument bytes ([`args_of`]).
//!   The opcode table is the public one of the FF7 modding wiki (the page "Animation Script" of the
//!   Qhimm wiki / FFRTT mirror: Cloud's script 32 is `hop forward (3), throw (9), hop back (4)` there and
//!   here). Every script the nine characters use (0 to 45) decodes with this table and ends in `FE C0` (end of
//!   script), `EE` or `FF` (run the idle script) or `C1` (jump back to the label `C9`, the idle scripts); the last
//!   scripts of a table (a limit break of Tifa's: they enter the same string at different places) run to the end
//!   of the file instead, which is not an end this decoder calls one ([`Script::terminated`]).
//! * A **played clip** is the number of a slot of the pack (`animations[i]` of [`super::BattleModel`]).
//! * An **enemy's** scripts (32 of them) follow one convention, **verified on the owned files on 2026-11-09** over the 329 enemies
//!   that have a name: script 0 is the idle loop (`A9 C9 00 C1`), script 1 the hurt reaction (the slot it plays, then `F6`, the
//!   engine's death effect), script 2 the death (a clip in 45 of them, `HOME IDLE` in the rest), and the attacks follow:
//!   `scene.bin`'s `animation_indices` are indices of these scripts, NOT slots of the pack. An attack marks where it lands with
//!   `F7` (damage, after `b` ticks), `C2` (the number) or `EC` (the effect: a spell or a special), moves the actor with `D1`/`D0`
//!   and sends it back with `FA` (`fmv-client`'s `battle::scene::clips` reads them).
//!
//! # Sources
//!
//! * The FF7 modding wiki's "Animation Script" page (opcode list and argument sizes: `https://ff7-mods.github.io/ff7-flat-wiki/FF7/Battle/Battle_Animation/Animation_Script.html`)
//!   and the Qhimm forum thread "(PSX) FF7 - Battle animation index for playable characters?" (the pointer
//!   table of `rtab`, the item script). Only layout and opcode facts were taken; every one was re-checked on
//!   the owned files.
//!
//! # Safety against untrusted input
//!
//! Every offset is checked against the file's length, a script is at most [`MAX_SCRIPT_OPS`] long, and the
//! pointer table at most [`MAX_SCRIPTS`] entries; nothing is allocated from a number the file says.

use crate::model::ModelError;
use crate::util::{slice_at, u32_at};

/// Bytes of the header that precede the table of script offsets.
pub const HEADER_LEN: usize = 104;
/// Most scripts accepted (the owned models have 74 and 32).
pub const MAX_SCRIPTS: usize = 256;
/// Most operations decoded for one script (the owned ones have under 40).
pub const MAX_SCRIPT_OPS: usize = 512;
/// The first code that is an opcode and not a clip index.
pub const FIRST_OPCODE: u8 = 0x8E;

/// The terminator of a script: `FE` with the argument `C0`.
pub const END: u8 = 0xFE;
/// Launches the model's idle script: the end of an action.
pub const IDLE: u8 = 0xEE;
/// The opcodes this module names (the rest are only decoded for their length).
pub mod code {
    /// Plays the clip of the byte after the next one (the idle scripts of the enemies).
    pub const PLAY_AFTER_SKIP: u8 = 0xA9;
    /// Halts the camera scripts and plays the clip of its argument once.
    pub const PLAY_ONCE: u8 = 0xB6;
    /// A label that jumps (`C1`) go back to.
    pub const LABEL: u8 = 0xC9;
    /// Jumps back to the first label.
    pub const JUMP_BACK: u8 = 0xC1;
    /// Moves the actor to `[x, z]` relative to its target over `frames` frames: word, word, byte.
    pub const MOVE_TARGET_XZ: u8 = 0xD1;
    /// Moves the actor to `[x, 0]` relative to its target: word, byte.
    pub const MOVE_TARGET_X: u8 = 0xD0;
    /// Target-relative move to `[x, y, z]` over `frames` frames: three words, two bytes.
    pub const MOVE_TARGET_XYZ: u8 = 0xCF;
    /// Moves the actor to `[x, z]` of its own field over `frames` frames: word, word, byte.
    pub const MOVE_TO: u8 = 0xC8;
    /// Moves the actor to `[x, 0]` relative to the current action's target: two words (seen in Cover).
    pub const MOVE_TO_TARGET: u8 = 0xAB;
    /// Places the actor at `[x, z]` relative to its target: two words.
    pub const PLACE_AT_TARGET: u8 = 0xFB;
    /// Self-relative move to `[x, 0]` over `frames` frames: word, byte.
    pub const MOVE_SELF_X: u8 = 0xD4;
    /// Self-relative move: three words, two bytes.
    pub const MOVE_SELF_XYZ: u8 = 0xD5;
    /// Sends the actor back to where it was before.
    pub const RETURN: u8 = 0xFA;
    /// Restores where the actor normally stands.
    pub const HOME: u8 = 0xE5;
    /// After `b0` frames the actor glides back to its origin over `b1` frames.
    pub const GLIDE_HOME: u8 = 0xA8;
    /// Loads the wait counter with its argument (frames).
    pub const WAIT_SET: u8 = 0xF4;
    /// Ticks the wait counter down to zero, then goes on.
    pub const WAIT: u8 = 0xF3;
    /// Shows the action's name.
    pub const SHOW_NAME: u8 = 0xEA;
    /// Barret's gun flash: delay and duration in frames.
    pub const GUN_FLASH: u8 = 0x96;
    /// Plays the loaded effect (magic, summon, limit, enemy attack).
    pub const EFFECT: u8 = 0xEC;
    /// After `b` frames: sound, the target's reaction and the damage display.
    pub const DAMAGE: u8 = 0xF7;
    /// Schedules the damage display `b` frames out, silently.
    pub const DAMAGE_QUIET: u8 = 0xC2;
    /// Waits for every pending animation and damage handler.
    pub const WAIT_ALL: u8 = 0x9E;
}

/// How many argument bytes opcode `code` (from [`FIRST_OPCODE`] up) takes, `None` for one this module does
/// not know (the placeholders the engine handles as plain scripts, and codes that are not used).
pub fn args_of(code: u8) -> Option<usize> {
    Some(match code {
        // No arguments.
        0x8E | 0x8F | 0x92 | 0x93 | 0x95 | 0x9B | 0x9C | 0x9E | 0x9F | 0xA4..=0xA6 | 0xAA | 0xAE | 0xB0..=0xB4 | 0xB7 | 0xB8 => 0,
        0xC1 | 0xC3 | 0xC5 | 0xC9 | 0xCA | 0xCD | 0xDF..=0xE6 | 0xE8 | 0xEA..=0xEE | 0xF0..=0xF3 | 0xF6 | 0xF9 | 0xFA | 0xFC | 0xFF => 0,
        // One byte.
        0x91 | 0x98 | 0x9D | 0xA0 | 0xA2 | 0xA3 | 0xA7 | 0xAC | 0xAF | 0xB6 | 0xB9 | 0xBC | 0xBE | 0xC2 | 0xC6 | 0xCC | 0xCE | 0xD6 | 0xDA => 1,
        0xE7 | 0xF4 | 0xF5 | 0xF7 | 0xF8 | 0xFE => 1,
        // Two bytes: byte+byte, or one word.
        0x96 | 0x97 | 0xA1 | 0xA8 | 0xBA | 0xBF | 0xD7 | 0xDD | 0xDE | 0xA9 => 2,
        // Three: byte+word, word+byte.
        0x90 | 0xC4 | 0xC7 | 0xD0 | 0xD4 | 0xD8 | 0xDC | 0xE9 => 3,
        // Four: word+word, word+byte+byte.
        0x9A | 0xAB | 0xBD | 0xDB | 0xFB => 4,
        // Five.
        0x94 | 0xAD | 0xC8 | 0xD1 => 5,
        // Six.
        0x99 | 0xFD => 6,
        0xCB | 0xCF | 0xD5 => 8,
        0xB5 => 13,
        _ => return None,
    })
}

/// One operation of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A clip index (below [`FIRST_OPCODE`]): play that slot of the model's pack, and wait for it.
    Play(u8),
    /// An opcode and its argument bytes.
    Code { code: u8, args: Vec<u8> },
}

impl Op {
    /// The `i`-th little-endian 16-bit argument word of an opcode (`None` for a clip or a short opcode).
    pub fn word(&self, i: usize) -> Option<i16> {
        let Op::Code { args, .. } = self else { return None };
        let word: [u8; 2] = args.get(2 * i..2 * i + 2)?.try_into().ok()?;
        Some(i16::from_le_bytes(word))
    }

    /// The opcode, `None` for a clip.
    pub fn code(&self) -> Option<u8> {
        match self {
            Op::Play(_) => None,
            Op::Code { code, .. } => Some(*code),
        }
    }
}

/// A decoded script.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    pub ops: Vec<Op>,
    /// The script ended on a terminator, an idle launch or a jump back (and not on the end of the file).
    pub terminated: bool,
}

impl Script {
    /// The slots of the pack this script plays, in order (a clip index, the clip of `A9`'s second argument
    /// and the clip of `B6`).
    pub fn clips(&self) -> Vec<u8> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Play(n) => Some(*n),
                Op::Code { code: code::PLAY_AFTER_SKIP, args } => args.get(1).copied(),
                Op::Code { code: code::PLAY_ONCE, args } => args.first().copied(),
                Op::Code { .. } => None,
            })
            .collect()
    }

    /// The opcodes of the script, in order (the clips left out).
    pub fn codes(&self) -> Vec<u8> {
        self.ops.iter().filter_map(Op::code).collect()
    }

    /// Does the script use opcode `c`?
    pub fn uses(&self, c: u8) -> bool {
        self.ops.iter().any(|op| op.code() == Some(c))
    }
}

/// The scripts of one model: the table of offsets over the `ab` entry's bytes.
#[derive(Debug, Clone)]
pub struct AnimationScripts<'a> {
    bytes: &'a [u8],
    /// Where each script starts, `None` for an entry that points outside the scripts (Sephiroth's table has zeros for
    /// the scripts he does not use): that script is absent, the others are not affected.
    offsets: Vec<Option<usize>>,
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "battle animation script", why: why.into() }
}

impl<'a> AnimationScripts<'a> {
    /// Read the table of an `ab` entry. The table ends where the lowest script starts (its length is not stored); an
    /// entry that points outside the scripts (Sephiroth's table has zeros, an enemy's first script can be absent) is an
    /// absent script, not an error. An error only if the file has no table at all or it would have more than
    /// [`MAX_SCRIPTS`] entries.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ModelError> {
        let mut entries: Vec<Option<usize>> = Vec::new();
        let mut table_end = bytes.len();
        loop {
            let at = HEADER_LEN + 4 * entries.len();
            if at + 4 > table_end {
                break;
            }
            if entries.len() >= MAX_SCRIPTS {
                return Err(ModelError::TooMany { what: "animation scripts", count: entries.len() as u64 + 1, limit: MAX_SCRIPTS as u64 });
            }
            let word = u32_at(bytes, at).ok_or(ModelError::Truncated { what: "animation script table", at, needed: 4 })?;
            // A script lies after the table, inside the file.
            let offset = usize::try_from(word).ok().filter(|&o| o >= at + 4 && o < bytes.len());
            table_end = table_end.min(offset.unwrap_or(usize::MAX));
            entries.push(offset);
        }
        if entries.is_empty() {
            return Err(ModelError::Truncated { what: "animation script table", at: HEADER_LEN, needed: 4 });
        }
        // What the table was thought to hold before its end was known and that lies inside it is no script either.
        let offsets = entries.into_iter().map(|o| o.filter(|&o| o >= table_end)).collect();
        Ok(Self { bytes, offsets })
    }

    /// How many scripts the table has.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Where script `index` starts: `None` for an index past the table and for an absent script.
    pub fn offset(&self, index: usize) -> Option<usize> {
        self.offsets.get(index).copied().flatten()
    }

    /// Decode script `index`: `None` if there is no such script (past the table, or absent), an error if it runs into
    /// an opcode this module does not know or an argument that is cut off by the end of the file.
    pub fn script(&self, index: usize) -> Option<Result<Script, ModelError>> {
        let start = self.offset(index)?;
        Some(decode(self.bytes, start))
    }
}

/// Decode the script that starts at `start` of `bytes`: up to a terminator (`FE` with `C0`), an idle launch
/// (`EE`, `FF`), a jump back (`C1`), or the end of the bytes.
pub fn decode(bytes: &[u8], start: usize) -> Result<Script, ModelError> {
    let mut ops = Vec::new();
    let mut at = start;
    loop {
        let Some(&c) = bytes.get(at) else { return Ok(Script { ops, terminated: false }) };
        if ops.len() >= MAX_SCRIPT_OPS {
            return Err(ModelError::TooMany { what: "script operations", count: ops.len() as u64, limit: MAX_SCRIPT_OPS as u64 });
        }
        at += 1;
        if c < FIRST_OPCODE {
            ops.push(Op::Play(c));
            continue;
        }
        let n = args_of(c).ok_or_else(|| invalid(format!("opcode {c:#04x} at {} is not known", at - 1)))?;
        let args = slice_at(bytes, at, n).ok_or(ModelError::Truncated { what: "animation script operation", at, needed: n })?.to_vec();
        at += n;
        let ends = match c {
            END => args.first() == Some(&0xC0),
            IDLE | 0xFF | code::JUMP_BACK => true,
            _ => false,
        };
        ops.push(Op::Code { code: c, args });
        if ends {
            return Ok(Script { ops, terminated: true });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An `ab` entry made of `scripts` (each a byte string) behind a table of `n` offsets: the layout the owned
    /// files have, with invented scripts.
    pub(crate) fn build_ab(n: usize, scripts: &[&[u8]]) -> Vec<u8> {
        let first = HEADER_LEN + 4 * n;
        let mut out = vec![0u8; HEADER_LEN];
        let mut at = first;
        let mut offsets = Vec::new();
        for s in scripts {
            offsets.push(at);
            at += s.len();
        }
        // Indices past the scripts given point at the first one (the "unused" scripts of the real files).
        for i in 0..n {
            let offset = offsets.get(i).copied().unwrap_or(first);
            out.extend((offset as u32).to_le_bytes());
        }
        for s in scripts {
            out.extend_from_slice(s);
        }
        out
    }

    #[test]
    fn a_script_of_clips_and_opcodes_decodes_to_its_clips_in_order() {
        // An invented script: load an effect, clip 7, a sound (a byte and a word), clip 12, show the name, wait 5
        // frames, clip 8, return, launch the idle script.
        let gesture: &[u8] = &[0xE8, 0x07, 0xD8, 0x02, 0x00, 0x02, 0x0C, 0xEA, 0xF4, 0x05, 0xF3, 0x08, 0xFA, 0xEE];
        let idle: &[u8] = &[0x00, 0xFE, 0xC0];
        let file = build_ab(4, &[idle, gesture]);
        let scripts = AnimationScripts::parse(&file).expect("a table");
        assert_eq!(scripts.len(), 4);
        let script = scripts.script(1).expect("script 1").expect("decodes");
        assert!(script.terminated);
        assert_eq!(script.clips(), [7, 12, 8]);
        assert!(script.uses(code::RETURN) && script.uses(code::WAIT) && !script.uses(code::HOME) && !script.uses(code::MOVE_TO));
        assert_eq!(script.ops.len(), 14 - 3 - 1, "the sound's 3 bytes and the wait's 1 are arguments, not operations");
        // The idle script: a clip, the end.
        let idle = scripts.script(0).expect("script 0").expect("decodes");
        assert_eq!((idle.clips(), idle.terminated), (vec![0], true));
        // An index past the table is not a script.
        assert!(scripts.script(4).is_none());
    }

    #[test]
    fn an_enemy_idle_loop_plays_its_clip_through_a9_and_stops_at_the_jump_back() {
        // A9 C9 00 C1: skip a byte, play clip 0, jump back to the label.
        let file = build_ab(2, &[&[0xA9, 0xC9, 0x00, 0xC1], &[0x05, 0xFE, 0xC0]]);
        let scripts = AnimationScripts::parse(&file).expect("a table");
        let idle = scripts.script(0).expect("script").expect("decodes");
        assert_eq!((idle.clips(), idle.terminated), (vec![0], true));
        assert_eq!(scripts.script(1).expect("script").expect("decodes").clips(), [5]);
    }

    #[test]
    fn the_arguments_of_a_move_are_words_and_a_script_that_is_cut_off_is_an_error() {
        // C8 [x: -1000] [z: 300] [frames: 8], then clip 7.
        let mut run: Vec<u8> = vec![0xC8];
        run.extend((-1000i16).to_le_bytes());
        run.extend(300i16.to_le_bytes());
        run.push(8);
        run.extend([0x07, 0xFE, 0xC0]);
        let file = build_ab(1, &[&run]);
        let scripts = AnimationScripts::parse(&file).expect("a table");
        let s = scripts.script(0).expect("script").expect("decodes");
        assert_eq!(s.ops[0].word(0), Some(-1000));
        assert_eq!(s.ops[0].word(1), Some(300));
        assert_eq!(s.clips(), [7]);
        // A move whose arguments the file does not hold.
        let cut = [0xC8u8, 0x01];
        assert!(decode(&cut, 0).is_err());
        // An opcode the table does not have.
        assert!(decode(&[0xBB], 0).is_err());
        // The end of the bytes ends an unterminated script (not an error).
        let open = decode(&[0x01, 0x02], 0).expect("decodes");
        assert_eq!((open.clips(), open.terminated), (vec![1, 2], false));
    }

    #[test]
    fn a_damaged_table_costs_scripts_not_the_model_and_is_never_allocated_from_its_numbers() {
        // Too short for a header: no table at all.
        assert!(AnimationScripts::parse(&[0u8; 50]).is_err());
        assert!(AnimationScripts::parse(&[0u8; HEADER_LEN + 2]).is_err());
        // An entry before the scripts or past the end is an absent script: the others are still there.
        let mut file = build_ab(3, &[&[0x00, 0xFE, 0xC0]]);
        file[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&5u32.to_le_bytes());
        file[HEADER_LEN + 8..HEADER_LEN + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        let table = AnimationScripts::parse(&file).expect("the table parses");
        assert_eq!((table.len(), table.offset(0).is_some(), table.offset(1), table.offset(2)), (3, true, None, None));
        assert!(table.script(1).is_none() && table.script(2).is_none());
        assert_eq!(table.script(0).expect("script 0").expect("decodes").clips(), [0]);
        // The first script may be the absent one (an enemy of the owned archive has none): the table still ends where the
        // lowest script that is there starts.
        let mut file = build_ab(3, &[&[0x00, 0xFE, 0xC0], &[0x01, 0xFE, 0xC0]]);
        file[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&0u32.to_le_bytes());
        let table = AnimationScripts::parse(&file).expect("the table parses");
        assert_eq!((table.len(), table.offset(0)), (3, None));
        assert_eq!(table.script(1).expect("script 1").expect("decodes").clips(), [1]);
        // A word that points into the table itself ends the table there: nothing is allocated from it.
        let mut file = build_ab(3, &[&[0x00, 0xFE, 0xC0]]);
        file[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&(HEADER_LEN as u32 + 8).to_le_bytes());
        let table = AnimationScripts::parse(&file).expect("the table parses");
        assert_eq!(table.len(), 2, "the table ends where its lowest pointer says");
        // A file of zeros has a table of absent scripts, as long as the file allows and never longer than the cap.
        let table = AnimationScripts::parse(&[0u8; HEADER_LEN + 40]).expect("a table of absent scripts");
        assert_eq!(table.len(), 10);
        assert!((0..table.len()).all(|i| table.script(i).is_none()));
        assert!(AnimationScripts::parse(&vec![0u8; HEADER_LEN + 4 * (MAX_SCRIPTS + 2)]).is_err(), "a table that would be longer than the cap is refused");
    }

    #[test]
    fn every_opcode_the_table_knows_has_a_length_and_the_unknown_ones_have_none() {
        for c in FIRST_OPCODE..=0xFF {
            // The placeholders the wiki lists (handled as plain scripts, may crash): not decodable here.
            let placeholder = matches!(c, 0xBB | 0xC0 | 0xD2 | 0xD3 | 0xD9 | 0xEF);
            assert_eq!(args_of(c).is_none(), placeholder, "{c:#04x}");
        }
        assert_eq!(args_of(code::MOVE_TARGET_XZ), Some(5));
        assert_eq!(args_of(code::MOVE_TARGET_XYZ), Some(8));
        assert_eq!(args_of(END), Some(1));
        assert_eq!(args_of(IDLE), Some(0));
    }
}
