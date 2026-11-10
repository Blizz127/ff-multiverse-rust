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
//!   with those variables after the moment (below). For legacy compatibility, the **empty blob** means
//!   [`NEW_GAME_MOMENT`] (1): the value the opening scene of `md1stin` leaves behind (`SETWORD Var[2][0] =
//!   1`, then control is handed to the player), so a player starts where the game first lets them walk.
//!   A legacy account keeps its recorded state and format until explicitly upgraded. Anything that is not
//!   exactly a supported canonical version is unreadable and is never
//!   interpreted, advanced or overwritten. Version 3 adds explicit known-bit persistent banks (below).
//! * **The phase of an area** ([`AreaStory::phase_key`]): each field is partitioned by the moments
//!   its own scripts compare `Var[2][0]` against (`==`, `!=`, `<`, `>`, `<=`, `>=` with a literal,
//!   in any script of the field). Two moments in one interval make every one of those comparisons
//!   answer the same, so the field looks the same to both players; the key of an interval is its
//!   first moment. **The interval that holds [`NEW_GAME_MOMENT`] is the empty key in every field**,
//!   which is what makes all existing accounts one phase and keeps their placements byte for byte
//!   what they were. The field runtime additionally distinguishes Jessie's upper, lower, trapped and rescued
//!   placements in `nmkin_3`, and hidden materia in `nmkin_5`, so their changes are sent immediately. These phase
//!   markers use the existing counter/rescue bit and are never saved.
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
//! # Persistent banks (blob version 3)
//!
//! Version 3 carries all five persistent field banks with a known-bit mask for every recorded byte.
//! Aliased byte/word bank selectors address the same underlying bytes; the moment remains in the header.
//! Missing values stay unknown. A known zero is stored explicitly. [`StoryState::write_masked`],
//! [`StoryState::write_byte`] and [`StoryState::write_word`] apply authored writes, and
//! [`StoryState::add_byte_checked`] provides ordered numeric changes with caller-supplied bounds.
//! Existing version 1/2 states keep their byte-for-byte representation until explicitly promoted;
//! promotion retains their tracked knowledge and does not infer new-game defaults or past choices.
//! The largest representation is [`MAX_BANK_BLOB_BYTES`] bytes. Readers must allow that bound before
//! enabling this format. The legacy union merge refuses version 3: a transactional store must use
//! [`compare_exchange_progression`] against the exact previous blob, under the same lock as its
//! scene receipt, so stale saves cannot undo clears or numeric updates. This module performs no I/O.
//! Version 3 phases include every carried value and knowledge mask; this conservative partition can
//! later be narrowed with field-specific dependency evidence.
//!
//! Explicit new-account registration can use [`StoryState::fresh_first_control`], the port's
//! source-backed reset/opening profile. [`StoryState::fresh_reset`] separately describes the earlier
//! engine reset. Neither constructor is used by decoding or legacy promotion: missing state in an
//! existing account stays unknown. The first-control profile documents the unsupported engine-owned
//! fields it withholds and the unresolved original startup routine; it is not a captured PC save.
//!
//! # Story points
//!
//! A story point is a place or a moment where a field's own scripts advance the story, found by reading
//! them ([`AreaStory::scan`], and for talks [`AreaStory::add_talk_points`]; the server never invents one and carries
//! out no write it could not read). There are five triggers, all of them things the server can see (where a player
//! walks, that it came to stand in the area, and, since `ClientMsg::Interact`, that it pressed OK):
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
//! * **A press on a triangle** ([`StoryTrigger::Press`]): the same loop with the OK button in the test (`IFKEYON`
//!   with OK in its mask under the triangle test): the elevator's panel, "stand on triangle 8 and press OK". The
//!   server takes the step when the player PRESSES OK (`ClientMsg::Interact`) while standing in the triangle, never
//!   when it walks across it.
//! * **A line** ([`StoryTrigger::Line`]): the trigger script of a `LINE` entity that the player walks onto
//!   (slots 2 to 6, the ones [`crate::script::ExitTrigger::is_walk_on`] counts; `[OK]` needs the button) and
//!   that starts enabled. The first mission's way into the reactor is one (`nmkin_1`).
//! * **Entering the area** ([`StoryTrigger::Enter`]): a write in script 0's init or main part that only the
//!   story guards, so the field's own scripts do it by themselves from the first frame on (a character walks
//!   to a spot and the story moves on: `md1_2`, `nrthmk`, `nmkin_2`). The server does it when the player
//!   comes to stand in the area. A script that loops or asks the player before the write is not one (it waits
//!   for something the server cannot see).
//! * **A talk** ([`StoryTrigger::Talk`]): a character the field places (an NPC) whose talk script (slot 1), or a
//!   script it requests, writes the moment or a tracked bit: `nmkin_3`'s woman (14) and `nmkin_5`'s materia (15). The
//!   server takes the step when the player presses OK next to the character (`ClientMsg::Interact` naming it) within its
//!   talk radius. What the talk does is not read off the text: the character's talk script is RUN (the dialogue walker,
//!   [`dialogue::run_talk`]) for the story the player has when it presses OK, after the character's own set-up has
//!   decided whether it can be talked to at all (a character that switches itself off when the story has moved is
//!   gone), and the step is what that run wrote that moves the story forward.
//!
//! What a point does is the list of writes the script makes there ([`Effect`]): the moment, or a bit of a
//! tracked variable, each under the conditions the script puts around it (comparisons of the moment, tests
//! of a tracked bit). The conditions of **all of a point's writes are judged on the story as it was when
//! the point was reached** (the elevator's moment write sits under its bit test, so it must not see the
//! write of the bit), and the point applies only if it moves the story forward: the moment goes up or a bit
//! goes on (a write that only clears a bit is no point). **Everything a point cannot do is named, not
//! guessed:** a guard on anything the model does not carry (a variable, a party, a key) makes the write no
//! part of a point; a write in an `else` branch, after an early `RET`, `MAPJUMP` or `GAMEOVER`, or in the
//! middle of a `REQ`'d script is not read at all (a talk is the exception: its scripts are run). A `BATTLE` the script
//! starts right AFTER a write is not run: it is named in the point's label so a log says so, and nothing else of a
//! cutscene happens either: the story simply moves. A `BATTLE` the block starts BEFORE the write (inline, or in a
//! script the block requests, to four requests deep: the guard scorpion, which a director's loop asks a character's
//! script to fight before it sets the moment) is a **scripted battle** ([`AreaStory::battle_of`]): the server starts it
//! for the party and pays the step when it is WON. Only what runs on every path to the write counts.
//!
//! **The OK button.** Since `ClientMsg::Interact` the server sees the press: a point whose triangle test also
//! wants OK (the elevator's panel) is a [`StoryTrigger::Press`], reached by pressing OK while standing in the
//! triangle (the point's label says `OK`). It is the only place a key guard is tolerated; everywhere else (a line,
//! entering, an exit) a key guard keeps the write or the exit out. The elevator going DOWN is the point the text shows
//! (the bit on, and the story to 12 the first time). The trip back UP clears the bit (`BITOFF`, in a script the panel
//! requests), which is not a step forward and which the text does not show, so each such panel has a sibling point
//! ([`Trigger::Panel`], `AreaStory::points` ends with them) that RUNS the panel's own script for the player's story and
//! takes the tracked bits it turned off. The two exclude each other (the lift is up or down), a press is one ride, and
//! nothing flaps: the way back is taken by pressing OK, never by walking. The model only carries a story forward
//! ([`merge_progression`] is a join), so the SAVED story keeps the lift down while the story the player holds in the
//! world, which decides the lift's exits, has it up until the player rides down again.
//!
//! Advancing is monotone: a point applies only when its conditions hold for the player's current story **and**
//! the story moves forward by it.
//!
//! **Only in the reviewed fields.** The scan reads every field, but the adapter offers a field's points (and its
//! script exits and links) only if the field is one of [`crate::REVIEWED_FIELDS`], the ten of the first mission, each
//! read case by case: a point raises a story for good, so an unreviewed script must not do it for a live account. In
//! any other field [`AreaStory::withhold_points`] takes the points out (the phases stay) and
//! [`fmv_adapter_api::AreaRuntime::story_withheld`] names them; the survey (`Ff7Adapter::survey_field`) still sees them
//! all, which is how a field is reviewed before it is added to the list.
//!
//! # The canon party
//!
//! The moment also says who the STORY's party is ([`CANON_PARTIES`], [`canon_party`]): Cloud starts alone,
//! Barret joins at the reactor door (moment 11), and leaves at the post-escape station (moment 36). A battle starts with that party (server-controlled companions) and the players'
//! own characters join it as extra members; nobody is replaced and the canon characters are never field NPCs. The
//! table names the source assignments that establish each transition.
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

use fmv_adapter_api::{NpcSpawn, StoryPoint, StoryStep, StoryTrigger};
use fmv_core::{PhaseKey, Pos};

use crate::field::Walkmesh;
use crate::script::dialogue::{self, StoryVars};
use crate::script::{FieldScript, Instruction, Operand, INIT_SCRIPT, SCRIPTS_PER_ENTITY};

pub(crate) mod scan;
mod banks;
mod point_exact;
pub use point_exact::StoryPointError;
mod party_visibility;
pub use party_visibility::{party_visibility_matches, reviewed_party_visibility};
mod fresh;
pub use banks::{canonical_persistent_bank, compare_exchange_progression, StoryWriteError, BLOB_VERSION_BANKS, MAX_BANK_BLOB_BYTES};

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
    /// The carried bits that nothing in this model ever sets (a subset of `bits`): they read clear for every account.
    /// A story point that needs one of them SET can never apply and is not offered, and a step that would turn one on
    /// is refused ([`StoryState::set_bit`]).
    pub never_set: u8,
}

/// Bit 0 of the reactor's state byte: the lift of `elevtr1` is at the bottom (see [`REACTOR_STATE`]).
pub const LIFT_BIT: u8 = 0;
/// Bit 5 of the reactor's state byte: the woman who is stuck on the way out of `nmkin_3` has been freed (see
/// [`REACTOR_STATE`]).
pub const FREED_BIT: u8 = 5;
/// Bit 7 of the reactor's state byte: the countdown ran out (see [`REACTOR_STATE`]).
pub const TIMEOUT_BIT: u8 = 7;

/// **The reactor's state byte (`Var[1][225]`): three of its bits are carried.**
///
/// * **Bit 0, the lift of `elevtr1` is at the bottom.** The `ele` entity of that field sets it when the party rides
///   down (its panel, then `BITON Var[1][225] bit 0`), clears it on the ride back up, and the four exits of the lift's
///   line are decided by it (`Var[1][225] lacks bit 0`: up to `nmkin_1`; set: down to `nmkin_2`).
/// * **Bit 5, the woman of `nmkin_3` is freed.** On the way out, at moment 27 or later, her talk script (slot 1 of `av_j`) turns
///   it on, with the bit clear, and she walks off. It is a talk that moves the story (the model's own definition: a tracked
///   bit turned on), so it is a talk point like the ones of the way down. The bit only decides where she and her
///   namesake of `nmkin_1` stand and whether the second door of `nmkin_1` opens. The field runtime now hides her in
///   `nmkin_3` once rescued; the other placements and door locks remain unmodeled. No exit or point waits for it:
///   the party takes the step, and the saved story keeps it.
/// * **Bit 7, the countdown ran out.** Each reactor field's `timeo` entity sets it when the timer that the Guard
///   Scorpion step starts reaches zero (then a `MAPJUMP` to `nrthmk`), and `nrthmk`'s finale reads it: with it clear the
///   party has escaped (moment 33), with it set the finale ends in `GAMEOVER`. It is carried ONLY so that the finale's
///   own guard can be judged, and **this server never sets it**: a countdown that runs out does nothing to the story
///   (see `fmv-server`'s `world::countdown`), so the finale applies to a party that was late as much as to one that was
///   not, and nobody is ever held at moment 27.
///
/// The byte has five more bits that the first mission's scripts set (the doors, the bomb, the characters on the way out)
/// and that nothing here writes: a guard that tests one of them cannot be judged.
pub const REACTOR_STATE: TrackedVar = TrackedVar {
    bank: 1,
    address: 225,
    bits: (1 << LIFT_BIT) | (1 << FREED_BIT) | (1 << TIMEOUT_BIT),
    never_set: 1 << TIMEOUT_BIT,
};

/// Bit 2 of `Var[3][212]`, the mark that the conversation at the bar of 7th Heaven has begun (see [`BAR_CONVERSATION`]).
pub const BAR_CONVERSATION_BIT: u8 = 2;

/// **The conversation at the bar of 7th Heaven has begun (`Var[3][212]`, bit 2).** The field's own director (`mds7pb_1`, the
/// `ID` entity) turns it on the moment it starts the conversation between Cloud and Tifa at the counter, and waits for it
/// to be clear before it starts that conversation, so the game itself never plays it twice. It is carried so that the
/// story scene built on that conversation (`crate::script::scene`, the proof scene) is offered once per story: the scene's
/// own writes reach the saved story (`fmv-server`'s `world::scene`) and [`crate::script::scene::SceneDef::begun_in`] reads
/// the mark back. No exit and no story point waits for it. The byte's other bits are flags of that field's own progress that
/// nothing here carries.
pub const BAR_CONVERSATION: TrackedVar = TrackedVar { bank: 3, address: 212, bits: 1 << BAR_CONVERSATION_BIT, never_set: 0 };

/// Every variable beyond the moment the model carries. Adding one is adding a row here, and a story point (or a story
/// scene, [`crate::script::scene`]) that writes it.
pub const TRACKED_VARS: &[TrackedVar] = &[REACTOR_STATE, BAR_CONVERSATION];

/// The tracked variable at `bank`/`address`, if there is one.
pub fn tracked_var(bank: u8, address: u8) -> Option<TrackedVar> {
    TRACKED_VARS.iter().copied().find(|t| t.bank == bank && t.address == address)
}

/// An account's FF7 story state: its moment, legacy variables, or version 3 persistent bank knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryState {
    /// The main progress counter (`Var[2][0]`).
    pub moment: u16,
    /// The tracked variables with a non-zero value, as bytes.
    vars: BTreeMap<(u8, u8), u8>,
    /// Canonical persistent banks with known-bit masks; absent in the legacy formats.
    banks: Option<BTreeMap<(u8, u8), banks::KnownByte>>,
}

impl StoryState {
    /// The legacy initial state at [`NEW_GAME_MOMENT`]: tracked bits zero, other variables unknown.
    /// Explicit extended-state registration uses [`Self::fresh_first_control`] instead.
    pub const fn new_game() -> Self {
        Self::at(NEW_GAME_MOMENT)
    }

    /// A legacy state at `moment`, with tracked bits zero and other variables unknown.
    pub const fn at(moment: u16) -> Self {
        Self { moment, vars: BTreeMap::new(), banks: None }
    }

    /// Read a progression blob: empty retains the legacy initial state, `[1, lo, hi]` a stored moment, a canonical version 2
    /// blob a moment and variables, version 3 persistent values and known masks; anything else is unreadable (`None`).
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
            [BLOB_VERSION_BANKS, ..] => banks::decode(blob),
            _ => None,
        }
    }

    /// The blob that stores this state: version 1 (`[1, lo, hi]`, exactly what version 1 builds wrote) when no
    /// variable is set, else the canonical version 2; an explicitly extended state always writes version 3.
    pub fn encode(&self) -> Vec<u8> {
        if let Some(banks) = &self.banks { return banks::encode(self.moment, banks); }
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

    /// The carried value (unknown bits read zero here). Use `known_byte`/`known_bit` for authoritative guards;
    /// version 3 resolves bank aliases, including the moment's byte view.
    pub fn var(&self, bank: u8, address: u8) -> u8 {
        if self.supports_extended() { return self.bank_byte(bank, address).value; }
        self.vars.get(&(bank, address)).copied().unwrap_or(0)
    }

    /// The variables beyond the moment that are not 0, as `((bank, address), value)`.
    pub fn vars(&self) -> impl Iterator<Item = ((u8, u8), u8)> + '_ {
        self.vars.iter().map(|(&key, &value)| (key, value))
            .chain(self.banks.iter().flat_map(|b| b.iter()).filter(|(_, b)| b.value != 0).map(|(&key, b)| (key, b.value)))
    }

    /// Turn bit `bit` of the tracked variable `bank`/`address` on or off. `true` when that changed the state
    /// (`false`: the bit was already so, the variable or the bit is not one the model carries, or the blob
    /// would hold more than [`MAX_STORY_VARS`] variables).
    pub fn set_bit(&mut self, bank: u8, address: u8, bit: u8, on: bool) -> bool {
        let Some(tracked) = tracked_var(bank, address) else { return false };
        let Some(mask) = 1u8.checked_shl(u32::from(bit)).filter(|m| tracked.bits & m != 0) else { return false };
        if on && tracked.never_set & mask != 0 {
            return false;
        }
        if self.supports_extended() {
            return self.write_masked(bank, address, if on { mask } else { 0 }, mask).unwrap_or(false);
        }
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
        if self.supports_extended() {
            let byte = self.bank_byte(bank, address);
            return (byte.mask == 0xFF).then_some(byte.value);
        }
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
        if self.supports_extended() {
            let byte = self.bank_byte(bank, address);
            return (byte.mask & mask != 0).then_some(byte.value & mask != 0);
        }
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
/// **Opening through moment 10: Cloud alone.** The owned `md1stin` director executes
/// `PRTYE 0,254,254` at offset `0x03FD` before handing control back at moment 1. The
/// empty slots are not Barret: a field's `PC` model binding does not establish party membership.
/// **Moment 11: Cloud and Barret.** The reactor door's awaited `nmkin_1` entity 12 slot 3
/// executes `PRTYE 0,1,254` at `0x0C20` before its caller finishes the moment-11 step.
/// The exact point evaluator journals that change and requires its final party to match this row.
///
/// **A later row is added when someone has established, from the field scripts' `PRTYP`, `PRTYM` and `PRTYE`
/// instructions and the moment they run at, who joins or leaves and when.** Rows ascend by moment and the first is
/// moment 0, so every moment has a party (a test holds the table to that).
///
/// **Moment 36: Cloud alone.** After the escape the director of `md8_1` (the street the team jumps down to) runs
/// `PRTYM character=1` (Barret leaves the party) and then, with nothing in between that reads the party, writes
/// moment 36 (measured: `tests/scene_real.rs`, `the_station_scene_takes_barret_out_of_the_party_...`). The team
/// splits up there, so from 36 on the story's party is Cloud. A story scene that changes the party is accepted only
/// when this table agrees with what its script did ([`party_after`]): the table is how the server carries the change.
/// Tifa and Aerith join later, in fields nothing offers yet; until a row says so, a story past 36 keeps Cloud alone
/// rather than an invented party.
pub const CANON_PARTIES: &[CanonParty] = &[
    CanonParty { from_moment: 0, members: &[CLOUD] },
    CanonParty { from_moment: 11, members: &[CLOUD, BARRET] },
    CanonParty { from_moment: 36, members: &[CLOUD] },
];

/// What a field script does to the story's party (`PRTYP`, `PRTYM`, `PRTYE`), in the script's own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartyOp {
    /// `PRTYP`: join at the first free field-party slot, replacing its last slot when full.
    Add(u8),
    /// `PRTYM`: leave the matching field-party slot empty, without compacting either tuple.
    Remove(u8),
    /// `PRTYE`: requested field-party slots. 254 forces an empty slot; 255 retains a remaining
    /// old member, in field-party order. Retained main-party members keep their original slots.
    Set([u8; 3]),
}

/// Replay authored party operations, retaining both field and main slot arrays between operations.
/// `PRTYE`'s 255 operands preserve unrequested old members; 254 forces empty slots. Transfers to the
/// main party retain surviving members in place, then fill its holes with new members. Only the
/// returned comparison list omits empty slots; intermediate operations never compact them.
///
/// This starts from a canonical party whose field and main slots agree. Exact scene execution
/// separately validates authoritative field-bank inputs and rejects outcomes its durable party table
/// cannot represent. Invalid character IDs or duplicate tuples do not change this comparison state.
pub fn party_after(party: &[u8], ops: &[PartyOp]) -> Vec<u8> {
    let mut bank = [255; 3];
    for (slot, &member) in bank.iter_mut().zip(party) { *slot = member; }
    let mut main = bank;
    for &op in ops {
        if let Ok(change) = crate::script::party::transition(bank, main, op) {
            bank = change.bank;
            main = change.main;
        }
    }
    main.into_iter().filter(|&member| member != 255).collect()
}

/// The canon party of a story state ([`CANON_PARTIES`]): the row with the greatest `from_moment` that is not past
/// the moment. The reviewed opening starts with Cloud, adds Barret at the reactor door, and
/// returns to Cloud alone at the post-escape station.
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

/// Whether the countdown that a step started (the step left the story at `started`) still matters to an account whose
/// story is `now` (`AreaRuntime::countdown_live`): while the main story counter is where the step left it. The first
/// reactor's timer is shown, and runs out, only at that moment (every reactor field's `time` and `timeo` entities test
/// `Var[2][0] == 27`), so a step that moves the counter on (the finale's 27 to 33) makes it moot, and a lift that goes
/// up or down (a tracked bit) or a character talked to does not. A blob that cannot be read is never live.
pub fn countdown_live(started: &[u8], now: &[u8]) -> bool {
    matches!((StoryState::decode(started), StoryState::decode(now)), (Some(a), Some(b)) if a.moment == b.moment)
}

/// The progression to store when `advanced` is saved over `stored`: the join of the two (the further moment, the
/// union of the set variable bits), so that a story never goes back and a late save cannot undo a later one. The
/// stored bytes come back exactly as they were when the join adds nothing to them (a new account's empty blob
/// stays empty under an equal story). A `stored` blob that cannot be read stays as it is (it is never
/// overwritten), and so does one when `advanced` cannot be read. If either state is version 3 this returns
/// `stored` unchanged: ordered updates require [`compare_exchange_progression`].
pub fn merge_progression(stored: &[u8], advanced: &[u8]) -> Vec<u8> {
    let (Some(kept), Some(newer)) = (StoryState::decode(stored), StoryState::decode(advanced)) else { return stored.to_vec() };
    // A clear or numeric assignment cannot be merged without its exact expected predecessor.
    // The transaction path must use compare_exchange_progression for version 3.
    if kept.supports_extended() || newer.supports_extended() { return stored.to_vec(); }
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
pub(crate) fn guards_of(list: &[Instruction], k: usize) -> Vec<&Instruction> {
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

    /// Whether the condition can hold for any story this model makes: a carried bit that nothing sets
    /// ([`TrackedVar::never_set`]) is never set, so a condition that wants it set never holds.
    fn possible(self) -> bool {
        match self {
            Self::Moment(..) => true,
            Self::Bit { bank, address, bit, set } => {
                !set || tracked_var(bank, address).is_none_or(|t| 1u8.checked_shl(u32::from(bit)).is_none_or(|mask| t.never_set & mask == 0))
            }
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
    /// Whether the write can change a story this model makes: turning on a bit that nothing sets
    /// ([`TrackedVar::never_set`]) changes nothing (the step is refused), so it is no write.
    fn possible(self) -> bool {
        match self {
            Self::Moment(_) | Self::Bit { on: false, .. } => true,
            Self::Bit { bank, address, bit, on: true } => {
                tracked_var(bank, address).is_none_or(|t| 1u8.checked_shl(u32::from(bit)).is_none_or(|mask| t.never_set & mask == 0))
            }
        }
    }

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
    /// The player presses OK next to a character whose talk script moves the story. What the talk does is not read
    /// off the script here: the talk script is run (the dialogue walker) for the story the player has when it presses
    /// OK, so it sees the conditions the character's own set-up and its talk put on the story, however it spells them.
    Talk { entity: usize, center: Pos, radius: f32 },
    /// The same press as a [`Trigger::Triangle`] that wants OK (the sibling the scan adds for each), but what it does
    /// is not read off the text: the panel entity's own script is RUN (its init part and then its main loop, with the
    /// player standing on the triangle: `vars` are the variables the loops read the player's triangle from, filled with
    /// it) for the story the player has, and the step is what that run writes that is a step BACK of a tracked bit: the
    /// lift going up. The ride down is the static point's (a step forward); this one applies only when the story has the
    /// lift down, so the two exclude each other and a press is one ride.
    Panel { entity: usize, triangle: usize, corners: [Pos; 3], vars: Vec<(u8, u8)> },
}

/// One story point as the script defines it (see the module docs).
#[derive(Debug, Clone, PartialEq)]
struct PointDef {
    trigger: Trigger,
    /// The conditions every one of the writes needs (none for a talk: the talk script is run).
    requires: Vec<Cond>,
    /// The writes, in the order the script makes them (none for a talk).
    effects: Vec<Effect>,
    /// A `BATTLE` the script starts right after the write (not run by the server: only named in the label).
    battle: Option<u16>,
    /// A `BATTLE` the script starts BEFORE the write, in the block the write is in (a scripted fight whose win is what
    /// moves the story: the server starts it and pays the step when it is won).
    battle_first: Option<u16>,
    /// A timer the write's block sets (`STTIM` with literal hours, minutes and seconds, in the same block as the write):
    /// its length in seconds. The step carries it as [`StoryStep::countdown`], and the server runs the countdown.
    countdown: Option<u32>,
    /// The exact script entry behind this point. Version 3 executes this entry and retains
    /// every supported persistent write instead of using the legacy curated projection.
    source: Option<PointSource>,
}

#[derive(Debug, Clone, PartialEq)]
struct PointSource {
    entity: usize,
    slot: usize,
    start_offset: Option<usize>,
    setup: bool,
    triangle_vars: Vec<(u8, u8)>,
    triangle_sources: Vec<(u8, u8, usize)>,
}

/// The authored entry admitted by an existing story point. This identifies source only;
/// it supplies no projected after-image and does not authorize skipping script effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoryPointSource {
    pub entity: u8,
    pub slot: u8,
    /// The main program immediately after script zero's initial RET.
    pub main: bool,
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
            Trigger::Talk { entity, .. } => return format!("talk to entity {entity}: the talk script moves the story"),
            Trigger::Panel { triangle, .. } => return format!("triangle {triangle} (OK): the panel's own script runs, what it does to a tracked bit besides a step forward (the ride back)"),
        };
        let first = self.battle_first.map_or(String::new(), |b| format!(" (starts battle {b} first, the step is paid when it is won)"));
        let battle = self.battle.map_or(String::new(), |b| format!(" (the script then starts battle {b}, which the server does not run)"));
        let timer = self.countdown.map_or(String::new(), |s| format!(" (starts a countdown of {s} s)"));
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
        format!("{place}: {}{first}{battle}{timer}", effects.join("; "))
    }

    fn story_trigger(&self) -> StoryTrigger {
        match &self.trigger {
            // A triangle whose test also wants the OK button is a press (the elevator's panel), not a region.
            Trigger::Triangle { corners, wants_ok: true, .. } => StoryTrigger::Press(corners.to_vec()),
            Trigger::Triangle { corners, .. } => StoryTrigger::Region(corners.to_vec()),
            Trigger::Line { a, b } => StoryTrigger::Line(*a, *b),
            Trigger::Enter => StoryTrigger::Enter,
            Trigger::Talk { center, radius, .. } => StoryTrigger::Talk { center: *center, radius: *radius },
            Trigger::Panel { corners, .. } => StoryTrigger::Press(corners.to_vec()),
        }
    }
}

/// The story after pressing OK next to character `entity` when the story is `state`: its talk script is run (the
/// dialogue walker, with `state` as the story it reads) and what it wrote that moves the story forward is applied. `None`
/// when the character cannot be talked to in this story, or the talk moves nothing forward.
fn talk_step(script: &FieldScript<'_>, entity: usize, state: &StoryState) -> Option<StoryState> {
    let run = dialogue::run_talk(script, entity, state, 1).ok()?;
    let mut next = state.clone();
    let mut forward = false;
    // the moment: both bytes of the word, or the one a SETBYTE wrote
    let (lo, hi) = (run.writes.get(&(MOMENT_BANK, MOMENT_ADDRESS)), run.writes.get(&(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1))));
    if lo.is_some() || hi.is_some() {
        let [now_lo, now_hi] = state.moment.to_le_bytes();
        let moment = u16::from_le_bytes([lo.copied().unwrap_or(now_lo), hi.copied().unwrap_or(now_hi)]);
        if moment > next.moment {
            next.moment = moment;
            forward = true;
        }
    }
    for tracked in TRACKED_VARS {
        let Some(&written) = run.writes.get(&(tracked.bank, tracked.address)) else { continue };
        for bit in 0..8u8 {
            let mask = 1u8 << bit;
            if tracked.bits & mask != 0 && written & mask != 0 {
                forward |= next.set_bit(tracked.bank, tracked.address, bit, true);
            }
        }
    }
    forward.then_some(next)
}

/// The story after pressing OK on the triangle of a panel when the story is `state`, when that is a step BACK of a tracked
/// bit (see [`Trigger::Panel`]): the panel entity's script 0 is run through its init part into its main loop with OK
/// pressed and the player's triangle in the variables the loop reads it from, and the tracked bits the run turned off
/// that the story had on are turned off. `None` when the run moves anything forward (the moment up, a tracked bit on:
/// that is the ride down, a point of its own) or clears nothing.
fn panel_step(script: &FieldScript<'_>, entity: usize, triangle: usize, vars: &[(u8, u8)], state: &StoryState) -> Option<StoryState> {
    /// The story, with the player's triangle in the variables the panel's loop reads it from.
    struct Standing<'a> {
        state: &'a StoryState,
        triangle: BTreeMap<(u8, u8), u8>,
    }
    impl StoryVars for Standing<'_> {
        fn byte(&self, bank: u8, address: u8) -> u8 {
            self.triangle.get(&(bank, address)).copied().unwrap_or_else(|| self.state.byte(bank, address))
        }
        fn party_member(&self, slot: u8) -> Option<u8> {
            self.state.party_member(slot)
        }
    }
    let [lo, hi] = u16::try_from(triangle).ok()?.to_le_bytes();
    let mut seeds = BTreeMap::new();
    for &(bank, address) in vars {
        seeds.insert((bank, address), lo);
        if let Some(next) = address.checked_add(1) {
            seeds.insert((bank, next), hi);
        }
    }
    let env = Standing { state, triangle: seeds };
    let mut walk = dialogue::Walk::script(entity, crate::script::INIT_SCRIPT, 1, dialogue::Limits::default()).through_init().pressing(dialogue::KEY_OK);
    while !matches!(walk.resume(script, &env, None), dialogue::Beat::End(_)) {}
    let (mut next, mut raised, mut cleared) = (state.clone(), false, false);
    let (lo, hi) = (walk.written(MOMENT_BANK, MOMENT_ADDRESS), walk.written(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1)));
    if lo.is_some() || hi.is_some() {
        let [now_lo, now_hi] = state.moment.to_le_bytes();
        raised |= u16::from_le_bytes([lo.unwrap_or(now_lo), hi.unwrap_or(now_hi)]) > state.moment;
    }
    for tracked in TRACKED_VARS {
        let Some(written) = walk.written(tracked.bank, tracked.address) else { continue };
        for bit in 0..8u8 {
            let mask = 1u8 << bit;
            if tracked.bits & mask == 0 {
                continue;
            }
            match (state.known_bit(tracked.bank, tracked.address, bit) == Some(true), written & mask != 0) {
                (false, true) => raised = true,
                (true, false) => cleared |= next.set_bit(tracked.bank, tracked.address, bit, false),
                _ => {}
            }
        }
    }
    (cleared && !raised).then_some(next)
}

/// What changed between two states, as `key=value` words without spaces (`moment=1->6`, `var1.225=0->1`).
fn summary(before: &StoryState, after: &StoryState) -> String {
    let mut words = Vec::new();
    if before.moment != after.moment {
        words.push(format!("moment={}->{}", before.moment, after.moment));
    }
    let keys: BTreeSet<(u8, u8)> = before.vars().chain(after.vars()).map(|(key, _)| key).collect();
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
    source: StoryScript,
}

/// Keep the scanned source available to `advance`, without dumping owned script bytes in Debug.
#[derive(Clone, Default, PartialEq)]
struct StoryScript(Vec<u8>);

impl std::fmt::Debug for StoryScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoryScript").field("bytes", &self.0.len()).finish()
    }
}

impl AreaStory {
    /// A version-3 point's source entry when its known story guards admit a forward effect.
    /// A scene host can match this to an independently reviewed scene root, then evaluate
    /// that whole scene. No legacy projection is returned or applied here.
    pub fn point_source(&self, progress: &[u8], id: u32) -> Option<StoryPointSource> {
        let state = StoryState::decode(progress).filter(StoryState::supports_extended)?;
        let point = self.points.get(usize::try_from(id).ok()?)?;
        if !point.requires.iter().all(|guard| guard.holds(&state)) { return None; }
        let advances = point.effects.iter().filter(|effect| effect.requires.iter().all(|guard| guard.holds(&state)))
            .any(|effect| match effect.op {
                Op::Moment(moment) => moment > state.moment,
                Op::Bit { bank, address, bit, on } => on && state.known_bit(bank, address, bit) != Some(true),
            });
        if !advances { return None; }
        let source = point.source.as_ref()?;
        let script = FieldScript::parse(&self.source.0).ok()?;
        script.span(source.entity, source.slot)?;
        let main = source.start_offset.is_some();
        if let Some(offset) = source.start_offset {
            if source.slot != INIT_SCRIPT { return None; }
            let first_ret = script.disassemble_partial(source.entity, INIT_SCRIPT).ok()?.instructions.into_iter()
                .find(|instruction| instruction.opcode == 0x00)?;
            if first_ret.end() != offset { return None; }
        }
        Some(StoryPointSource { entity: u8::try_from(source.entity).ok()?, slot: u8::try_from(source.slot).ok()?, main })
    }

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
        Self { edges: edges.into_iter().collect(), points, source: StoryScript(script.bytes().to_vec()) }
    }

    /// The first moment of the interval that holds `moment`.
    fn interval_start(&self, moment: u16) -> u16 {
        self.edges.iter().rev().find(|&&e| e <= moment).copied().unwrap_or(0)
    }

    /// The phase of this field for a progression blob: the empty key for the interval that holds
    /// [`NEW_GAME_MOMENT`], else a tagged key naming the interval. `None` for a blob that cannot be
    /// read.
    pub fn phase_key(&self, blob: &[u8]) -> Option<PhaseKey> {
        let state = StoryState::decode(blob)?;
        let start = self.interval_start(state.moment);
        if state.supports_extended() {
            // Conservatively partition on all known persistent inputs, including their knownness.
            // Include the exact moment too: the legacy interval scanner does not yet recognize
            // every byte-alias/variable comparison that version 3 scripts can make.
            let mut key = vec![PHASE_TAG, BLOB_VERSION_BANKS];
            key.extend(blake3::hash(&state.encode()).as_bytes());
            return Some(PhaseKey(key));
        }
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

    /// What reaching point `id` does for `blob` (see `AreaRuntime::story_advance`), for the points the script's text
    /// decides by itself. A talk point has no script here to run and is never reached by this; the adapter asks
    /// [`AreaStory::advance_with`]. Version 3 executes the retained source and returns a replacement
    /// for an exact-base transaction; publishing it through legacy monotonic merge is unsupported.
    pub fn advance(&self, blob: &[u8], id: u32) -> Option<StoryStep> {
        let state = StoryState::decode(blob)?;
        if state.supports_extended() {
            let script = FieldScript::parse(&self.source.0).ok()?;
            return self.advance_exact_with(&script, blob, id).ok().flatten();
        }
        let point = self.points.get(usize::try_from(id).ok()?)?;
        let next = point.apply(&state)?;
        Some(StoryStep { summary: summary(&state, &next), progress: next.encode(), countdown: point.countdown })
    }

    /// [`AreaStory::advance`] for every kind of point: a talk point runs its character's talk script (in `script`, the
    /// field's own) for the story `blob` records, and the step is what that talk wrote that moves the story forward
    /// (the moment up, a tracked bit on). A character that is hidden or not to be talked to in that story gives nothing.
    pub fn advance_with(&self, script: &FieldScript<'_>, blob: &[u8], id: u32) -> Option<StoryStep> {
        let state = StoryState::decode(blob)?;
        if state.supports_extended() { return self.advance_exact_with(script, blob, id).ok().flatten(); }
        let point = self.points.get(usize::try_from(id).ok()?)?;
        let (next, countdown) = match &point.trigger {
            Trigger::Talk { entity, .. } => (talk_step(script, *entity, &state)?, None),
            Trigger::Panel { entity, triangle, vars, .. } => (panel_step(script, *entity, *triangle, vars, &state)?, None),
            _ => (point.apply(&state)?, point.countdown),
        };
        Some(StoryStep { summary: summary(&state, &next), progress: next.encode(), countdown })
    }

    /// The battle that point `id` starts before its step is paid (`AreaRuntime::story_battle`): a scripted fight
    /// whose win is what moves the story.
    pub fn battle_of(&self, id: u32) -> Option<u16> {
        self.points.get(usize::try_from(id).ok()?)?.battle_first
    }

    /// Version 3 gates the complete authored action on its battle, including a battle after
    /// the first story write (the station's formation 300). Legacy saves retain their lookup.
    pub fn battle_for(&self, progress: &[u8], id: u32) -> Option<u16> {
        let state = StoryState::decode(progress)?;
        let point = self.points.get(usize::try_from(id).ok()?)?;
        if state.supports_extended() { point.battle_first.or(point.battle) } else { point.battle_first }
    }

    /// Add the talk points of a field: one for each character (of the NPCs the field places, `npcs`) whose talk script
    /// can write the story (the script, or one it requests, sets the moment or a tracked bit). Appended after the
    /// points the scan found, so the ids of those do not move. `script` is the field's own.
    pub fn add_talk_points(&mut self, script: &FieldScript<'_>, npcs: &[NpcSpawn]) {
        let characters = dialogue::characters(script);
        for npc in npcs {
            if self.points.len() >= MAX_STORY_POINTS {
                break;
            }
            let Some(character) = dialogue::find_character(&characters, &npc.entity_name, npc.pos.x, npc.pos.y) else { continue };
            let entity = character.entity;
            if !scan::talk_can_write_the_story(script, entity) {
                continue;
            }
            if self.points.iter().any(|p| matches!(p.trigger, Trigger::Talk { entity: e, center, .. } if e == entity && center == npc.pos)) {
                continue;
            }
            self.points.push(PointDef {
                trigger: Trigger::Talk { entity, center: npc.pos, radius: character.talk_radius },
                requires: Vec::new(),
                effects: Vec::new(),
                battle: None,
                battle_first: None,
                countdown: None,
                source: None,
            });
        }
    }

    /// Take the story points away, keeping the phases (a field that is not on the adapter's list of reviewed
    /// fields, `crate::REVIEWED_FIELDS`, offers no point): the labels of the points that were taken, in the order of
    /// their ids.
    pub fn withhold_points(&mut self) -> Vec<String> {
        std::mem::take(&mut self.points).iter().map(PointDef::label).collect()
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
