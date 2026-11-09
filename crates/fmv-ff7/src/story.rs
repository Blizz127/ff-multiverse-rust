//! FF7 story: per-account progression, the phases it selects, and the points that advance it.
//!
//! # The model
//!
//! FF7 keeps its main story position in one field variable, a 16-bit word at **bank 2, address 0**
//! (`Var[2][0]`; the community calls it the "game moment" or main progress counter). The field
//! scripts compare it everywhere (391 of the 702 owned fields read it, see
//! `examples/story_survey.rs`) and 213 write it. This module turns that into the things the
//! multiplayer server needs, all per ACCOUNT and independent of which character a player looks like
//! or fights as:
//!
//! * **The progression blob** ([`StoryState`]): `[1, moment_lo, moment_hi]`, three bytes, and, once a story
//!   step has set one of the few other variables the model carries ([`TRACKED_VARS`]), a second version
//!   with those variables after the moment (below). The **empty blob is a new account** and means
//!   [`NEW_GAME_MOMENT`] (1): the value the opening scene of `md1stin` leaves behind (`SETWORD Var[2][0] =
//!   1`, then control is handed to the player), so a player starts where the game first lets them walk.
//!   Every account that exists today has no progression record, or a version 1 one, and is therefore
//!   exactly as it was. Anything else that is not exactly version 1 or 2 is unreadable and is never
//!   interpreted, advanced or overwritten.
//! * **The phase of an area** ([`AreaStory::phase_key`]): each field is partitioned by the moments
//!   its own scripts compare `Var[2][0]` against (`==`, `!=`, `<`, `>`, `<=`, `>=` with a literal,
//!   in any script of the field). Two moments in one interval make every one of those comparisons
//!   answer the same, so the field looks the same to both players; the key of an interval is its
//!   first moment. **The interval that holds [`NEW_GAME_MOMENT`] is the empty key in every field**,
//!   which is what makes all existing accounts one phase and keeps their placements byte for byte
//!   what they were. (The variables beyond the moment do not take part in phases: the one the model
//!   carries only ever changes together with a moment the field distinguishes.)
//! * **Story points** ([`AreaStory::points`]): where a field's own scripts advance the story on
//!   something the server can see. See below.
//!
//! # Variables beyond the moment (blob version 2)
//!
//! The exits of a field are decided by more than the moment: the elevator of the first reactor
//! (`elevtr1`) leads down or up by a bit of `Var[1][225]`. The model carries exactly the variables
//! (and bits of them) in [`TRACKED_VARS`], every other variable is unknown: a guard that reads one cannot be
//! evaluated (an exit with such a guard stays closed, see [`crate::area`]) and a script write to one is not
//! carried out. A tracked variable is 0 for a new account, and only story points set it.
//!
//! **Blob version 2** is `[2, moment_lo, moment_hi, n, (bank, address, value) * n]`: the same moment, then
//! `n` (1 to [`MAX_STORY_VARS`]) variables with a non-zero value, in strictly ascending `(bank, address)`
//! order, so that equal states have exactly one encoding. [`StoryState::encode`] writes version 1 whenever
//! no variable is set (those blobs are byte for byte what version 1 built wrote, and
//! [`StoryState::decode`] reads both), and merging two progressions ([`merge_progression`]) is a join: the
//! further moment, the union of the set bits. **Rollback:** a build that predates version 2 reads a version 2
//! blob as unreadable (it plays in a phase of its own and never overwrites the record), as it does any blob
//! it does not know.
//!
//! # Story points
//!
//! A story point is a place or a moment where a field's own scripts advance the story, found by reading
//! them ([`AreaStory::scan`]; the server never invents one and carries out no write it could not read).
//! There are three triggers, all of them things the server can see:
//!
//! * **A walkmesh triangle** ([`StoryTrigger::Region`]). FF7 fields advance the story from small loops in an
//!   entity's script 0 (its "main" part, after the first `RET`). The first fields of the first mission do it
//!   like this (measured on the owned Steam copy: the director entity of the first field, and the same shape
//!   with another comparison in the next field; `T`, `N`, `M` and `E` are literals and an entity of the field,
//!   whose values are pinned by the ignored real-data tests in `tests/story_real.rs` and not repeated here):
//!
//!   ```text
//!   init:   SETWORD Var[b][a] = T                ; the walkmesh triangle that matters
//!           RET
//!   main:   GETAI entity=E Var[b2][a2]           ; which triangle is the (controlled) player on
//!           IFSW Var[b2][a2] == Var[b][a]        ; ...is it triangle T?
//!           IFSW Var[2][0] <cmp> N               ; ...and is the story where this step applies
//!           SETWORD Var[2][0] = M                ; story advances (then a cutscene, often a battle)
//!           JMPB main
//!   ```
//!
//!   The test of the triangle may also compare the variable that `GETAI` fills (anywhere in the field: the
//!   elevator's panel reads the one the director's loop keeps) with a literal. E is **bound to a playable
//!   character** (has a `PC`): any of them, never one in particular, so the rule is "the player's character".
//! * **A line** ([`StoryTrigger::Line`]): the trigger script of a `LINE` entity that the player walks onto
//!   (slots 2 to 6, the ones [`crate::script::ExitTrigger::is_walk_on`] counts; `[OK]` needs the button) and
//!   that starts enabled. The first mission's way into the reactor is one (`nmkin_1`).
//! * **Entering the area** ([`StoryTrigger::Enter`]): a write in script 0's init or main part that only the
//!   story guards, so the field's own scripts do it by themselves from the first frame on (a character walks
//!   to a spot and the story moves on: `md1_2`, `nrthmk`, `nmkin_2`). The server does it when the player
//!   comes to stand in the area. A script that loops or asks the player before the write is not one (it waits
//!   for something the server cannot see).
//!
//! What a point does is the list of writes the script makes there ([`Effect`]): the moment, or a bit of a
//! tracked variable, each under the conditions the script puts around it (comparisons of the moment, tests
//! of a tracked bit). The conditions of **all of a point's writes are judged on the story as it was when
//! the point was reached** (the elevator's moment write sits under its bit test, so it must not see the
//! write of the bit), and the point applies only if it moves the story forward: the moment goes up or a bit
//! goes on (a write that only clears a bit is no point). **Everything a point cannot do is named, not
//! guessed:** a guard on anything the model does not carry (a variable, a party, a key) makes the write no
//! part of a point; a write in an `else` branch, after an early `RET`, `MAPJUMP` or `GAMEOVER`, or in the
//! middle of a `REQ`'d script is not read at all. The script's `BATTLE` after a write is not run (the server
//! has no scripted battles yet); it is named in the point's label so a log says so, and nothing else of a
//! cutscene happens either: the story simply moves. What the server cannot see: **a talk** (nmkin_3's
//! woman, nmkin_5's chest: the protocol has no interaction message), and **a key press** (below).
//!
//! **The one approximation: the OK button.** The elevator's panel is "stand on triangle 8 and press OK"
//! (`IFKEYON` with OK in its mask under the triangle test). Protocol version 9 carries moves only, so the
//! server cannot see the press: **a point whose triangle test also wants OK is reached by standing on the
//! triangle** (the point's label says `OK`). It is the only place a key guard is tolerated; everywhere else
//! (a line, entering, an exit) a key guard keeps the write or the exit out. And only the elevator going
//! DOWN is carried out: the trip back up clears the bit (`BITOFF`), which is not forward, and a region fires
//! on every move inside it, so a toggle would flap.
//!
//! Advancing is monotone: a point applies only when its conditions hold for the player's current story **and**
//! the story moves forward by it.
//!
//! # The canon party
//!
//! The moment also says who the STORY's party is ([`CANON_PARTIES`], [`canon_party`]): Cloud and Barret
//! in the first reactor mission. A battle starts with that party (server-controlled companions) and the players'
//! own characters join it as extra members; nobody is replaced and the canon characters are never field NPCs. The
//! table has one row today and says, in its own docs, how a later row is established.
//!
//! # What this does not do
//!
//! * No cast per phase for FF7 yet: [`AreaRuntime::npcs_in_phase`] keeps the server's existing
//!   behaviour (the NPCs the data places unconditionally), so every account sees exactly the NPCs it
//!   always saw. The phase key is what a later lane keys the field's cast on.
//! * Only the moment and the variables of [`TRACKED_VARS`] are modelled. The other story variables (the
//!   bits NPCs and items hang off, the countdown timer of the reactor) are not part of the progression yet.
//!
//! # Untrusted data
//!
//! Like the rest of the crate: no indexing, no unchecked arithmetic, no panics on script bytes.

use std::collections::{BTreeMap, BTreeSet};

use fmv_adapter_api::{StoryPoint, StoryStep, StoryTrigger};
use fmv_core::{PhaseKey, Pos};

use crate::field::Walkmesh;
use crate::script::dialogue::StoryVars;
use crate::script::{FieldScript, Instruction, Operand, SCRIPTS_PER_ENTITY};

mod scan;

/// The bank of the main progress counter (`Var[2][0]`).
pub const MOMENT_BANK: u8 = 2;
/// The address of the main progress counter within its bank.
pub const MOMENT_ADDRESS: u8 = 0;
/// What a new account's (empty) progression stands for: the value `md1stin`'s opening scene leaves
/// the counter at before it gives the player control.
pub const NEW_GAME_MOMENT: u16 = 1;
/// The format version byte of a progression blob that holds the moment only.
pub const BLOB_VERSION: u8 = 1;
/// The format version byte of a progression blob that also holds variables beyond the moment.
pub const BLOB_VERSION_VARS: u8 = 2;
/// Most variables a version 2 blob holds beyond the moment (the whole blob stays under 52 bytes, far under
/// what the server keeps of a progression).
pub const MAX_STORY_VARS: usize = 16;
/// First byte of every non-default [`PhaseKey`] this module makes.
const PHASE_TAG: u8 = 0x4D;
/// Most story points one field may offer (a real field has a handful).
pub const MAX_STORY_POINTS: usize = 32;

/// A variable beyond the moment that the story model carries (see the module docs, "Variables beyond the
/// moment"): which bits of the byte at `bank`/`address` are part of the progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedVar {
    pub bank: u8,
    pub address: u8,
    /// The bits of the byte the model carries (`0xFF`: the whole byte). A guard that tests another bit of it
    /// cannot be evaluated.
    pub bits: u8,
}

/// **The reactor's state byte, bit 0: the lift of `elevtr1` is at the bottom.** The `ele` entity of that field
/// sets it when the party rides down (its panel, then `BITON Var[1][225] bit 0`) and the four exits of the
/// lift's line are decided by it (`Var[1][225] lacks bit 0`: up to `nmkin_1`; set: down to `nmkin_2`). The
/// byte has seven more bits that the first mission's scripts set (the timer, the bomb) and that nothing here
/// writes, so only this one is carried.
pub const REACTOR_LIFT: TrackedVar = TrackedVar { bank: 1, address: 225, bits: 0x01 };

/// Every variable beyond the moment the model carries. Adding one is adding a row here, and a story point
/// that writes it.
pub const TRACKED_VARS: &[TrackedVar] = &[REACTOR_LIFT];

/// The tracked variable at `bank`/`address`, if there is one.
pub fn tracked_var(bank: u8, address: u8) -> Option<TrackedVar> {
    TRACKED_VARS.iter().copied().find(|t| t.bank == bank && t.address == address)
}

/// An account's FF7 story state: where its main story stands, and the few other variables the model carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryState {
    /// The main progress counter (`Var[2][0]`).
    pub moment: u16,
    /// The tracked variables with a non-zero value, as bytes.
    vars: BTreeMap<(u8, u8), u8>,
}

impl StoryState {
    /// Where a new account starts ([`NEW_GAME_MOMENT`], every other variable 0).
    pub const fn new_game() -> Self {
        Self::at(NEW_GAME_MOMENT)
    }

    /// The story at `moment` with every other variable 0.
    pub const fn at(moment: u16) -> Self {
        Self { moment, vars: BTreeMap::new() }
    }

    /// Read a progression blob: empty is a new account, `[1, lo, hi]` a stored moment, a canonical version 2
    /// blob a moment and variables; anything else is unreadable (`None`).
    // (`as_chunks` would satisfy the lint but needs Rust 1.88; the workspace keeps `chunks_exact`, see `lgp`.)
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn decode(blob: &[u8]) -> Option<Self> {
        match blob {
            [] => Some(Self::new_game()),
            [BLOB_VERSION, lo, hi] => Some(Self::at(u16::from_le_bytes([*lo, *hi]))),
            [BLOB_VERSION_VARS, lo, hi, count, rest @ ..] => {
                let count = usize::from(*count);
                if count == 0 || count > MAX_STORY_VARS || rest.len() != count.checked_mul(3)? {
                    return None;
                }
                let mut state = Self::at(u16::from_le_bytes([*lo, *hi]));
                let mut previous: Option<(u8, u8)> = None;
                for entry in rest.chunks_exact(3) {
                    let [bank, address, value] = entry else { return None };
                    let key = (*bank, *address);
                    let ascending = previous.is_none_or(|p| p < key);
                    let is_a_variable = (1..=15).contains(bank) && !(*bank == MOMENT_BANK && *address <= MOMENT_ADDRESS.wrapping_add(1));
                    if !(ascending && is_a_variable && *value != 0) {
                        return None; // not the one way to write this state
                    }
                    state.vars.insert(key, *value);
                    previous = Some(key);
                }
                Some(state)
            }
            _ => None,
        }
    }

    /// The blob that stores this state: version 1 (`[1, lo, hi]`, exactly what version 1 builds wrote) when no
    /// variable is set, else the canonical version 2.
    pub fn encode(&self) -> Vec<u8> {
        let [lo, hi] = self.moment.to_le_bytes();
        if self.vars.is_empty() {
            return vec![BLOB_VERSION, lo, hi];
        }
        let mut blob = vec![BLOB_VERSION_VARS, lo, hi, u8::try_from(self.vars.len()).unwrap_or(u8::MAX)];
        for (&(bank, address), &value) in &self.vars {
            blob.extend([bank, address, value]);
        }
        blob
    }

    /// The value of a variable beyond the moment (0 unless a story step set bits in it).
    pub fn var(&self, bank: u8, address: u8) -> u8 {
        self.vars.get(&(bank, address)).copied().unwrap_or(0)
    }

    /// The variables beyond the moment that are not 0, as `((bank, address), value)`.
    pub fn vars(&self) -> impl Iterator<Item = ((u8, u8), u8)> + '_ {
        self.vars.iter().map(|(&key, &value)| (key, value))
    }

    /// Turn bit `bit` of the tracked variable `bank`/`address` on or off. `true` when that changed the state
    /// (`false`: the bit was already so, the variable or the bit is not one the model carries, or the blob
    /// would hold more than [`MAX_STORY_VARS`] variables).
    pub fn set_bit(&mut self, bank: u8, address: u8, bit: u8, on: bool) -> bool {
        let Some(tracked) = tracked_var(bank, address) else { return false };
        let Some(mask) = 1u8.checked_shl(u32::from(bit)).filter(|m| tracked.bits & m != 0) else { return false };
        let old = self.var(bank, address);
        let new = if on { old | mask } else { old & !mask };
        if new == old {
            return false;
        }
        if new == 0 {
            self.vars.remove(&(bank, address));
        } else if self.vars.contains_key(&(bank, address)) || self.vars.len() < MAX_STORY_VARS {
            self.vars.insert((bank, address), new);
        } else {
            return false;
        }
        true
    }

    /// The byte at `bank`/`address` when the model knows all of it: the moment's two bytes, or a tracked
    /// variable whose every bit is carried. `None` for anything else (what a guard that reads it cannot decide).
    pub fn known_byte(&self, bank: u8, address: u8) -> Option<u8> {
        if bank == MOMENT_BANK {
            let [lo, hi] = self.moment.to_le_bytes();
            return match address {
                MOMENT_ADDRESS => Some(lo),
                a if a == MOMENT_ADDRESS.wrapping_add(1) => Some(hi),
                _ => None,
            };
        }
        (tracked_var(bank, address)?.bits == 0xFF).then(|| self.var(bank, address))
    }

    /// The little-endian word at `bank`/`address` when the model knows both its bytes.
    pub fn known_word(&self, bank: u8, address: u8) -> Option<u16> {
        Some(u16::from_le_bytes([self.known_byte(bank, address)?, self.known_byte(bank, address.checked_add(1)?)?]))
    }

    /// Whether bit `bit` (0 to 7) of the byte at `bank`/`address` is set, when the model carries that bit
    /// (the moment's bytes, or a bit of a tracked variable); `None` otherwise.
    pub fn known_bit(&self, bank: u8, address: u8, bit: u8) -> Option<bool> {
        let mask = 1u8.checked_shl(u32::from(bit))?;
        if bank == MOMENT_BANK {
            return self.known_byte(bank, address).map(|b| b & mask != 0);
        }
        (tracked_var(bank, address)?.bits & mask != 0).then(|| self.var(bank, address) & mask != 0)
    }

    /// The least story that has everything either of the two has: the further moment, the union of the bits.
    /// What [`merge_progression`] stores. (A tracked bit only ever goes on: the one write that clears one is
    /// not a story point.)
    fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        joined.moment = self.moment.max(other.moment);
        for (&key, &value) in &other.vars {
            let merged = joined.vars.get(&key).copied().unwrap_or(0) | value;
            if joined.vars.contains_key(&key) || joined.vars.len() < MAX_STORY_VARS {
                joined.vars.insert(key, merged);
            }
        }
        joined
    }
}

/// FF7's character ids as the savemap and a field script's `PC` operand number them (the ids that
/// `fmv_ff7_battle::kernel::CharacterId::index` and a battle's `BattleMember::character` use too). Only the
/// ones the canon party table below names.
pub const CLOUD: u8 = 0;
/// Barret's character id (see [`CLOUD`]).
pub const BARRET: u8 = 1;

/// One row of [`CANON_PARTIES`]: who stands in the story's own party from `from_moment` on, until the next row's
/// moment, in party order (slot 0 first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonParty {
    /// The first main-progress moment (`Var[2][0]`) this party holds for.
    pub from_moment: u16,
    /// The characters, at most three (FF7's party has three slots), no repeats.
    pub members: &'static [u8],
}

/// **The canon party by story moment** (a small table, extended row by row): the characters of the game's own story
/// that fight BESIDE the players as server-controlled companions. A player's original character is added to this
/// party, never put in a member's place, so these stay in the story whoever plays.
///
/// What is established, and from where: the first reactor mission (the Bombing Mission: the moments of `md1stin`
/// and `md1_1`, and every moment after them until a row below says otherwise) is fought by **Cloud and Barret**.
/// That is the game's story (the opening mission's party), what
/// [`StartOfGame`](crate::script::dialogue::StartOfGame) has always answered to a dialogue script's `IFPRTYQ`, and
/// what the first mission's own fields are built for: `md1stin` and `md1_1` each bind one party entity to character
/// 0 and one to character 1 (a `PC` operand; the second wears Barret's skeleton: measured by
/// `tests/area_real.rs`, `barret_is_a_party_member_in_the_data_...`). The table does NOT read `kernel.bin`'s initial
/// party bytes (they read Barret, Tifa, Cloud on the owned copy: a savemap default the game's first script
/// replaces) nor the party-setting instructions of those fields (the only one, in `md1stin`'s opening scene, is
/// `PRTYE 0, 254, 254`, whose last two operands are not character ids: a cutscene's party, measured with
/// `examples/script_dump`).
///
/// **A later row is added when someone has established, from the field scripts' `PRTYP`, `PRTYM` and `PRTYE`
/// instructions and the moment they run at, who joins or leaves and when** (Aerith and Tifa join after the
/// reactor; none of that is in a field this slice offers yet). Until then a story that has moved past the first
/// mission keeps the Bombing Mission's party rather than an invented one. Rows ascend by moment and the first is
/// moment 0, so every moment has a party (a test holds the table to that).
pub const CANON_PARTIES: &[CanonParty] = &[CanonParty { from_moment: 0, members: &[CLOUD, BARRET] }];

/// The canon party of a story state ([`CANON_PARTIES`]): the row with the greatest `from_moment` that is not past
/// the moment. Cloud and Barret in every row there is today.
pub fn canon_party(state: &StoryState) -> Vec<u8> {
    CANON_PARTIES
        .iter()
        .rev()
        .find(|row| row.from_moment <= state.moment)
        .map_or_else(Vec::new, |row| row.members.to_vec())
}

/// The canon party for an account whose progression blob is `progress` (what `AreaRuntime::story_party` answers):
/// the party of the state it records, and, for a blob that cannot be read (never interpreted or advanced), the
/// new game's own party.
pub fn canon_party_of(progress: &[u8]) -> Vec<u8> {
    canon_party(&StoryState::decode(progress).unwrap_or_else(StoryState::new_game))
}

/// The dialogue lane's source of story variables (`StoryVars`) for an account: `Var[2][0]` is its moment
/// (the word at bank 2, address 0: low byte first), the tracked variables ([`TRACKED_VARS`]) are what the story
/// has set in them, every other variable is 0 as in a new game, and the party is the story's canon party
/// ([`canon_party`]) whatever the player's own character is. So a conversation reads the player's OWN story,
/// whichever character the player looks like: `talk.set_story(Box::new(move || Box::new(state.clone())))` with
/// `state` decoded from the progression the server told this client (`fmv-client`'s `StoryMirror::progress`).
impl StoryVars for StoryState {
    fn byte(&self, bank: u8, address: u8) -> u8 {
        let [lo, hi] = self.moment.to_le_bytes();
        match (bank, address) {
            (MOMENT_BANK, MOMENT_ADDRESS) => lo,
            (MOMENT_BANK, a) if a == MOMENT_ADDRESS.wrapping_add(1) => hi,
            _ => self.var(bank, address),
        }
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        canon_party(self).get(usize::from(slot)).copied()
    }
}

/// The progression to store when `advanced` is saved over `stored`: the join of the two (the further moment, the
/// union of the set variable bits), so that a story never goes back and a late save cannot undo a later one. The
/// stored bytes come back exactly as they were when the join adds nothing to them (a new account's empty blob
/// stays empty under an equal story). A `stored` blob that cannot be read stays as it is (it is never
/// overwritten), and so does one when `advanced` cannot be read.
pub fn merge_progression(stored: &[u8], advanced: &[u8]) -> Vec<u8> {
    let (Some(kept), Some(newer)) = (StoryState::decode(stored), StoryState::decode(advanced)) else { return stored.to_vec() };
    let joined = kept.join(&newer);
    if joined == kept {
        stored.to_vec()
    } else {
        joined.encode()
    }
}

/// How a script compares the counter with a literal (the codes of `IFUB`/`IFSW`/`IFUW`'s `op`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

impl Cmp {
    /// The ordered comparisons: 0 `==`, 1 `!=`, 2 `>`, 3 `<`, 4 `>=`, 5 `<=`. The other codes are bit
    /// tests (not comparisons of a value) and are not understood.
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Eq,
            1 => Self::Ne,
            2 => Self::Gt,
            3 => Self::Lt,
            4 => Self::Ge,
            5 => Self::Le,
            _ => return None,
        })
    }

    /// `moment <self> literal`.
    fn holds(self, moment: u16, literal: i32) -> bool {
        let m = i32::from(moment);
        match self {
            Self::Eq => m == literal,
            Self::Ne => m != literal,
            Self::Gt => m > literal,
            Self::Lt => m < literal,
            Self::Ge => m >= literal,
            Self::Le => m <= literal,
        }
    }

    /// `literal <self> moment` read from the counter's side.
    fn mirrored(self) -> Self {
        match self {
            Self::Gt => Self::Lt,
            Self::Lt => Self::Gt,
            Self::Ge => Self::Le,
            Self::Le => Self::Ge,
            same => same,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => ">=",
            Self::Le => "<=",
        }
    }

    /// The moments at which this comparison with `literal` starts answering differently from the
    /// moment before: the edges of the intervals it cuts the axis into.
    fn edges(self, literal: i32) -> [Option<i32>; 2] {
        let next = literal.saturating_add(1);
        match self {
            Self::Eq | Self::Ne => [Some(literal), Some(next)],
            Self::Gt | Self::Le => [Some(next), None],
            Self::Lt | Self::Ge => [Some(literal), None],
        }
    }
}

/// The opcodes that compare two values and jump when the answer is false.
const COMPARES: [&str; 6] = ["IFUB", "IFUBL", "IFSW", "IFSWL", "IFUW", "IFUWL"];

fn is_moment(o: &Operand) -> bool {
    o.bank == MOMENT_BANK && (o.value & 0xFF) == i32::from(MOMENT_ADDRESS)
}

/// `(bank, address)` of a banked operand.
fn var_of(o: &Operand) -> (u8, u8) {
    (o.bank, u8::try_from(o.value & 0xFF).unwrap_or(0))
}

/// `left <op> right` of a comparison instruction, when `op` is an ordered comparison.
fn compare_parts(ins: &Instruction) -> Option<(&Operand, &Operand, Cmp)> {
    if !COMPARES.contains(&ins.mnemonic) {
        return None;
    }
    let op = Cmp::from_code(ins.operand("op")?.value)?;
    Some((ins.operand("left")?, ins.operand("right")?, op))
}

/// `moment <cmp> literal` when the instruction compares the counter with a literal (either way
/// round).
fn moment_compare(ins: &Instruction) -> Option<(Cmp, i32)> {
    let (left, right, op) = compare_parts(ins)?;
    if is_moment(left) && right.bank == 0 {
        Some((op, right.value))
    } else if is_moment(right) && left.bank == 0 {
        Some((op.mirrored(), left.value))
    } else {
        None
    }
}

/// The jumps before `list[k]` that skip over it when taken: the forward jumps whose target lies
/// beyond it. Instruction `k` runs only if every one of them did NOT jump.
fn guards_of(list: &[Instruction], k: usize) -> Vec<&Instruction> {
    let Some(at) = list.get(k).map(|i| i.offset) else { return Vec::new() };
    list.get(..k)
        .unwrap_or(&[])
        .iter()
        .filter(|g| g.is_jump() && g.jump_target.is_some_and(|t| t > at) && g.end() <= at)
        .collect()
}

/// A condition on the story that a script puts around a write: what the model can judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cond {
    /// `moment <cmp> literal`.
    Moment(Cmp, i32),
    /// Bit `bit` of the tracked variable is `set` (or clear).
    Bit { bank: u8, address: u8, bit: u8, set: bool },
}

impl Cond {
    fn holds(self, state: &StoryState) -> bool {
        match self {
            Self::Moment(cmp, literal) => cmp.holds(state.moment, literal),
            Self::Bit { bank, address, bit, set } => state.known_bit(bank, address, bit) == Some(set),
        }
    }

    /// The comparison without the name of what is compared (`==1`), for a point whose conditions are all moments.
    fn short(self) -> String {
        match self {
            Self::Moment(cmp, literal) => format!("{}{literal}", cmp.symbol()),
            Self::Bit { .. } => self.full(),
        }
    }

    fn full(self) -> String {
        match self {
            Self::Moment(cmp, literal) => format!("moment {}{literal}", cmp.symbol()),
            Self::Bit { bank, address, bit, set } => format!("Var[{bank}][{address}] bit {bit} {}", if set { "set" } else { "clear" }),
        }
    }
}

/// What a script writes that the model carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `Var[2][0] = n`.
    Moment(u16),
    /// `BITON` / `BITOFF` of a bit of a tracked variable.
    Bit { bank: u8, address: u8, bit: u8, on: bool },
}

impl Op {
    fn text(self) -> String {
        match self {
            Self::Moment(n) => n.to_string(),
            Self::Bit { bank, address, bit, on } => format!("Var[{bank}][{address}] bit {bit} {}", if on { "on" } else { "off" }),
        }
    }
}

/// One write of a story point, under the conditions the script puts around it besides the point's own.
#[derive(Debug, Clone, PartialEq)]
struct Effect {
    requires: Vec<Cond>,
    op: Op,
}

/// Where a story point is.
#[derive(Debug, Clone, PartialEq)]
enum Trigger {
    /// The player's character stands on this walkmesh triangle (the script may also want OK pressed there).
    Triangle { triangle: usize, corners: [Pos; 3], wants_ok: bool },
    /// The player walks onto this line.
    Line { a: Pos, b: Pos },
    /// The player comes to stand in the area.
    Enter,
}

/// One story point as the script defines it (see the module docs).
#[derive(Debug, Clone, PartialEq)]
struct PointDef {
    trigger: Trigger,
    /// The conditions every one of the writes needs.
    requires: Vec<Cond>,
    /// The writes, in the order the script makes them.
    effects: Vec<Effect>,
    /// A `BATTLE` the script starts right after (not run by the server).
    battle: Option<u16>,
}

impl PointDef {
    /// The story after reaching this point with `state`, or `None` when it does not apply: the point's conditions
    /// fail, or the writes whose own conditions hold do not move the story forward (the moment up, a bit on).
    /// Every write is judged on `state`, not on what an earlier write of the point did.
    fn apply(&self, state: &StoryState) -> Option<StoryState> {
        if !self.requires.iter().all(|c| c.holds(state)) {
            return None;
        }
        let mut next = state.clone();
        let mut forward = false;
        for effect in self.effects.iter().filter(|e| e.requires.iter().all(|c| c.holds(state))) {
            match effect.op {
                Op::Moment(n) if n > next.moment => {
                    next.moment = n;
                    forward = true;
                }
                Op::Moment(_) => {}
                Op::Bit { bank, address, bit, on } => forward |= next.set_bit(bank, address, bit, on) && on,
            }
        }
        forward.then_some(next)
    }

    fn label(&self) -> String {
        let place = match &self.trigger {
            Trigger::Triangle { triangle, wants_ok, .. } => format!("triangle {triangle}{}", if *wants_ok { " (OK)" } else { "" }),
            Trigger::Line { .. } => "line".to_string(),
            Trigger::Enter => "entry".to_string(),
        };
        let battle = self.battle.map_or(String::new(), |b| format!(" (the script then starts battle {b}, which the server does not run)"));
        let effects: Vec<String> = self
            .effects
            .iter()
            .map(|e| {
                let conds: Vec<Cond> = self.requires.iter().chain(&e.requires).copied().collect();
                let when = if conds.iter().all(|c| matches!(c, Cond::Moment(..))) {
                    format!("moment {}", conds.iter().map(|c| c.short()).collect::<Vec<_>>().join(" and "))
                } else {
                    conds.iter().map(|c| c.full()).collect::<Vec<_>>().join(" and ")
                };
                format!("{when} -> {}", e.op.text())
            })
            .collect();
        format!("{place}: {}{battle}", effects.join("; "))
    }

    fn story_trigger(&self) -> StoryTrigger {
        match &self.trigger {
            Trigger::Triangle { corners, .. } => StoryTrigger::Region(corners.to_vec()),
            Trigger::Line { a, b } => StoryTrigger::Line(*a, *b),
            Trigger::Enter => StoryTrigger::Enter,
        }
    }
}

/// What changed between two states, as `key=value` words without spaces (`moment=1->6`, `var1.225=0->1`).
fn summary(before: &StoryState, after: &StoryState) -> String {
    let mut words = Vec::new();
    if before.moment != after.moment {
        words.push(format!("moment={}->{}", before.moment, after.moment));
    }
    let keys: BTreeSet<(u8, u8)> = before.vars.keys().chain(after.vars.keys()).copied().collect();
    for (bank, address) in keys {
        let (was, is) = (before.var(bank, address), after.var(bank, address));
        if was != is {
            words.push(format!("var{bank}.{address}={was}->{is}"));
        }
    }
    words.join(" ")
}

/// What one field's scripts say about the story: the intervals of the counter that make its phases,
/// and its story points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AreaStory {
    /// First moment of every interval after the first, ascending (all in `1..=65535`).
    edges: Vec<u16>,
    points: Vec<PointDef>,
}

impl AreaStory {
    /// Read a field's story out of its script and walkmesh (see the module docs). A script that
    /// cannot be decoded in places contributes what did decode.
    pub fn scan(script: &FieldScript, mesh: &Walkmesh) -> Self {
        let mut edges = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                // Slots that share an entry point are one script.
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
                for (cmp, literal) in listing.instructions.iter().filter_map(moment_compare) {
                    edges.extend(cmp.edges(literal).into_iter().flatten().filter_map(|e| u16::try_from(e).ok()).filter(|&e| e >= 1));
                }
            }
        }
        let mut points = scan::scan_points(script, mesh);
        points.truncate(MAX_STORY_POINTS);
        Self { edges: edges.into_iter().collect(), points }
    }

    /// The first moment of the interval that holds `moment`.
    fn interval_start(&self, moment: u16) -> u16 {
        self.edges.iter().rev().find(|&&e| e <= moment).copied().unwrap_or(0)
    }

    /// The phase of this field for a progression blob: the empty key for the interval that holds
    /// [`NEW_GAME_MOMENT`], else a tagged key naming the interval. `None` for a blob that cannot be
    /// read.
    pub fn phase_key(&self, blob: &[u8]) -> Option<PhaseKey> {
        let start = self.interval_start(StoryState::decode(blob)?.moment);
        if start == self.interval_start(NEW_GAME_MOMENT) {
            return Some(PhaseKey::default());
        }
        let [lo, hi] = start.to_le_bytes();
        Some(PhaseKey(vec![PHASE_TAG, lo, hi]))
    }

    /// The story points, `id` being the index: the walkmesh triangles first, then the lines, then the
    /// entry points (each in the order of the entities and their scripts).
    pub fn points(&self) -> Vec<StoryPoint> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| StoryPoint { id: u32::try_from(i).unwrap_or(u32::MAX), trigger: p.story_trigger(), label: p.label() })
            .collect()
    }

    /// What reaching point `id` does for `blob` (see `AreaRuntime::story_advance`).
    pub fn advance(&self, blob: &[u8], id: u32) -> Option<StoryStep> {
        let state = StoryState::decode(blob)?;
        let point = self.points.get(usize::try_from(id).ok()?)?;
        let next = point.apply(&state)?;
        Some(StoryStep { summary: summary(&state, &next), progress: next.encode() })
    }

    /// The first moment of every interval after the first (for tests and surveys).
    pub fn edges(&self) -> &[u16] {
        &self.edges
    }

    /// How many story points the field has.
    pub fn point_count(&self) -> usize {
        self.points.len()
    }
}

#[cfg(test)]
mod tests;
