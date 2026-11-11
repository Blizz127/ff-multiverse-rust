//! Authoritative evaluation is deliberately separate from the permissive dialogue preview.
//! No result escapes after a guessed input, skipped effect, or incomplete run.

use super::*;
use crate::script::{field_wide_bytes, FieldScratch, FieldScratchError, FieldScratchOutcome, FieldScratchRun};
use crate::story::{canonical_persistent_bank, NativePartyOp, NativePartyOpError, NativeStoryBuilder, NativeStoryError, NativeStoryEvent, NativeStoryJournal, PartyOp, StoryState};
use std::cell::{Cell, RefCell};

/// Final authored assignment to the selected bits of one physical persistent byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactWrite {
    pub bank: u8,
    pub address: u8,
    pub value: u8,
    pub mask: u8,
}

/// Effective field battle mode at the one caller-authorized BATTLE instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactBattle {
    pub formation: u16,
    pub flags: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactRun {
    pub writes: Vec<ExactWrite>,
    pub battle: Option<ExactBattle>,
    /// Resolved PRTYE assignments. Every empty slot is254; original255 fill semantics have
    /// already been applied against the current party, so callers may use story::party_after.
    pub party: Vec<PartyOp>,
    /// Ordered complete persistent afterimage for native-party execution; `writes` is only
    /// an inspection summary in this format and must not be independently applied.
    pub native: Option<NativeStoryJournal>,
    /// Exact persistent state at BATTLE, before its victory suffix executes.
    pub battle_story: Option<StoryState>,
    /// Field-visit memory is separate from persistent story assignments. Present only
    /// for an explicit checked-scratch entry point; old APIs retain their old behavior.
    pub scratch: Option<FieldScratchOutcome>,
    /// Scratch exactly at the acknowledged BATTLE, before its victory suffix.
    pub battle_scratch: Option<FieldScratch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExactError {
    #[error("exact dialogue requires an extended story")]
    LegacyStory,
    #[error(transparent)]
    NativeStory(#[from] NativeStoryError),
    #[error(transparent)]
    NativeParty(#[from] NativePartyOpError),
    #[error(transparent)]
    Scratch(#[from] FieldScratchError),
    #[error("existing-visit setup would change scratch values or knowledge")]
    SetupScratch,
    #[error("checked scratch does not accept trigger seeds")]
    ScratchSeed,
    #[error("checked scratch requires unambiguous source selectors for triangle read-back")]
    ScratchTrigger,
    #[error("availability of character {0} is not known")]
    UnknownAvailability(u8),
    #[error("the field has no such entity or script")]
    MissingScript,
    #[error("the character is hidden or not talkable")]
    NotTalkable,
    #[error("unknown input at {offset}: bank {bank}, address {address}, mask {mask:#04x}")]
    UnknownRead { offset: usize, bank: u8, address: u8, mask: u8 },
    #[error("unsupported opcode {opcode:#04x} at {offset}")]
    Unsupported { offset: usize, opcode: u8 },
    #[error("invalid or unsupported operand at {offset}")]
    InvalidOperand { offset: usize },
    #[error("setup would change persistent bank {bank}, address {address}")]
    SetupWrite { bank: u8, address: u8 },
    #[error("setup would change the current party")]
    SetupParty,
    #[error("direct write to party mirror byte {address} would bypass the current party overlay")]
    PartyMirror { address: u8 },
    #[error("a question requires an explicit answer")]
    MissingAnswer,
    #[error("answer {answer} is outside {first}..={last}")]
    InvalidAnswer { answer: usize, first: usize, last: usize },
    #[error("answers remain after the script ended")]
    ExtraAnswer,
    #[error("script request is absent, asynchronous, or exceeds the depth budget at {offset}")]
    Request { offset: usize },
    #[error("script did not finish safely: {0:?}")]
    End(End),
    #[error("a required battle was absent or did not match")]
    Battle,
    #[error("a countdown was absent, repeated, or did not match the verified duration")]
    Countdown,
    #[error("a movement read-back was unresolved")]
    Unresolved,
}

/// Inputs established by the authoritative trigger. Seeds are known execution inputs (for
/// example the GETAI destinations holding the standing triangle), not persistent assignments.
/// `after_battle` evaluates the tail only for one matching, caller-acknowledged victory; it
/// neither runs nor rewards that battle. The caller must gate application on that victory.
#[derive(Debug, Clone, Default)]
pub struct ExactScript {
    pub entity: usize,
    pub slot: usize,
    pub through_init: bool,
    /// Audit this entity's init only, through its first RET, before entering the selected root.
    pub setup: bool,
    pub start_offset: Option<usize>,
    pub keys: u32,
    pub seeds: Vec<((u8, u8), u8)>,
    /// The observed actor for each seeded triangle (bank, address, entity). Checked scratch
    /// APIs require the original GETAI selector here, not its canonical physical bank.
    pub triangle_sources: Vec<(u8, u8, usize)>,
    pub after_battle: Option<u16>,
    pub countdown: Option<u32>,
}

#[derive(Debug, Clone)]
pub(super) struct ExactState {
    story: StoryState,
    native: Option<NativeStoryBuilder>,
    scratch: Option<FieldScratchRun>,
    persistent_change: Option<(u8, u8)>,
    pub(super) masks: BTreeMap<(u8, u8), u8>,
    seeds: BTreeMap<(u8, u8), u8>,
    fault: RefCell<Option<ExactError>>,
    offset: Cell<usize>,
    after_battle: Option<u16>,
    battle: Option<ExactBattle>,
    battle_story: Option<StoryState>,
    battle_scratch: Option<FieldScratch>,
    battle_flags: u16,
    triangle_sources: BTreeMap<(u8, u8), usize>,
    triangle_selectors: BTreeMap<(u8, u8), u8>,
    moved_actors: BTreeSet<usize>,
    all_positions_changed: bool,
    root_triangle_reads: BTreeSet<usize>,
    party: Vec<u8>,
    party_ops: Vec<PartyOp>,
    party_changed: bool,
    countdown: Option<u32>,
    countdown_seen: bool,
}

impl ExactState {
    /// Until the first change, every earlier assignment was already known and identical
    /// to the predecessor. Comparing against that predecessor therefore detects the first
    /// value or knowledge change without needing another full persistent overlay. Once set,
    /// the marker survives restoration and cannot be inferred from the final write map.
    fn record_persistent_assignment(&mut self, bank: u8, address: u8, value: u8, mask: u8) {
        let Some(bank) = canonical_persistent_bank(bank) else { return };
        if self.persistent_change.is_none()
            && (self.story.known_mask(bank, address) & mask != mask
                || (self.story.var(bank, address) ^ value) & mask != 0) {
            self.persistent_change = Some((bank, address));
        }
    }
}

fn key(bank: u8, address: u8) -> (u8, u8) {
    (canonical_persistent_bank(bank).unwrap_or(if bank == 6 { 5 } else { bank }), address)
}

fn presentation(opcode: u8) -> bool {
    matches!(opcode,
        0x08 | 0x09 | 0x24..=0x27 | 0x29 | 0x2A | 0x2C | 0x2D | 0x2E | 0x2F | 0x33..=0x37
        | 0x41..=0x43 | 0x4A | 0x50..=0x55 | 0x57 | 0x5E | 0x5F
        | 0x61..=0x68 | 0x6A..=0x6C | 0x6F | 0xA0..=0xA3 | 0xA5..=0xA8 | 0xAB..=0xB6
        | 0xBA..=0xBD | 0xC0 | 0xC2..=0xC7 | 0xD0..=0xD3 | 0xD6..=0xD9
        | 0xDA..=0xDE | 0xE0..=0xEF | 0xF0..=0xF6 | 0xFC | 0xFD)
}

impl Walk {
    fn exact(mut self, story: &StoryState, seeds: &[((u8, u8), u8)], battle: Option<u16>) -> Result<Self, ExactError> {
        if !story.supports_extended() { return Err(ExactError::LegacyStory); }
        let native = story.supports_native_party().then(|| NativeStoryBuilder::new(story)).transpose()?;
        let mut known = BTreeMap::new();
        for &((bank, address), value) in seeds {
            if canonical_persistent_bank(bank).is_none() && !matches!(bank, 5 | 6) {
                return Err(ExactError::InvalidOperand { offset: 0 });
            }
            // Trigger observations are not durable raw-party initialization.
            if native.is_some() && key(bank, address).0 == 3 && (9..=11).contains(&address) {
                return Err(ExactError::InvalidOperand { offset: 0 });
            }
            if known.insert(key(bank, address), value).is_some_and(|old| old != value) {
                return Err(ExactError::InvalidOperand { offset: 0 });
            }
        }
        self.exact = Some(ExactState {
            story: story.clone(), native, scratch: None, persistent_change: None, masks: BTreeMap::new(), seeds: known,
            fault: RefCell::new(None), offset: Cell::new(0), after_battle: battle, battle: None, battle_story: None, battle_scratch: None,
            // Original FieldInitDefaultValues starts the field battle mode at zero.
            battle_flags: 0, triangle_sources: BTreeMap::new(), triangle_selectors: BTreeMap::new(), moved_actors: BTreeSet::new(), all_positions_changed: false, root_triangle_reads: BTreeSet::new(),
            party: if story.supports_native_party() { Vec::new() } else { (0..3).filter_map(|slot| story.party_member(slot)).collect() }, party_ops: Vec::new(), party_changed: false,
            countdown: None, countdown_seen: false,
        });
        Ok(self)
    }

    fn with_scratch(mut self, scratch: Option<&FieldScratch>) -> Self {
        if let Some(exact) = &mut self.exact { exact.scratch = scratch.map(FieldScratchRun::new); }
        self
    }

    fn checked_scratch(&self, bank: u8) -> Option<&FieldScratchRun> {
        if !matches!(bank, 5 | 6) { return None; }
        self.exact.as_ref()?.scratch.as_ref()
    }

    pub(super) fn has_checked_scratch(&self) -> bool {
        self.exact.as_ref().is_some_and(|exact| exact.scratch.is_some())
    }

    pub(super) fn checked_scratch_word(&self, bank: u8, address: u8) -> Option<u16> {
        let scratch = self.checked_scratch(bank)?;
        Some(match scratch.read_u16(bank, usize::from(address)) {
            Ok(value) => value,
            Err(error) => { self.fail_exact(error.into()); 0 },
        })
    }

    pub(super) fn checked_field_bit(&self, env: &dyn StoryVars, bank: u8, address: u8, wide: bool, bit: u8) -> Option<bool> {
        if !self.has_checked_scratch() { return None; }
        let mask = 1u16 << bit;
        if let Some(scratch) = self.checked_scratch(bank) {
            let value = if wide {
                scratch.read_masked_u16(bank, usize::from(address), mask)
            } else {
                scratch.read_masked_u8(bank, usize::from(address), mask as u8).map(u16::from)
            };
            return Some(match value {
                Ok(value) => value & mask != 0,
                Err(error) => { self.fail_exact(error.into()); false },
            });
        }
        let Some(bytes) = field_wide_bytes(bank) else { self.invalid_exact(); return Some(false); };
        if wide && bytes == 2 && address == u8::MAX { self.invalid_exact(); return Some(false); }
        if wide && bytes == 1 && bit >= 8 { return Some(false); }
        let Some(at) = address.checked_add(bit / 8) else { self.invalid_exact(); return Some(false); };
        let mask = 1 << (bit % 8);
        Some(self.read_masked(env, bank, at, mask) & mask != 0)
    }

    pub(super) fn write_checked_scratch_word(&mut self, bank: u8, address: u8, value: u16) -> bool {
        if !matches!(bank, 5 | 6) { return false; }
        let Some(exact) = &mut self.exact else { return false };
        let Some(scratch) = &mut exact.scratch else { return false };
        let changed = match scratch.write_u16(bank, usize::from(address), value) {
            Ok(changed) => changed,
            Err(error) => { self.fail_exact(error.into()); return true; },
        };
        for offset in 0..if bank == 6 { 2 } else { 1 } {
            let Some(at) = address.checked_add(offset) else { self.invalid_exact(); return true; };
            let Some(&byte) = scratch.current().values().get(usize::from(at)) else { self.invalid_exact(); return true; };
            self.writes.insert((5, at), byte);
            exact.masks.insert((5, at), 255);
        }
        if changed { self.idle.clear(); }
        true
    }

    pub(super) fn exact_key(&self, bank: u8, address: u8) -> (u8, u8) {
        if self.exact.is_some() { key(bank, address) } else { (bank, address) }
    }

    pub(super) fn exact_fault(&self) -> Option<ExactError> {
        self.exact.as_ref().and_then(|e| e.fault.borrow().clone())
    }

    pub(super) fn fail_exact(&self, error: ExactError) {
        if let Some(exact) = &self.exact {
            let mut fault = exact.fault.borrow_mut();
            if fault.is_none() { *fault = Some(error); }
        }
    }

    pub(super) fn invalid_exact(&self) {
        if let Some(exact) = &self.exact {
            self.fail_exact(ExactError::InvalidOperand { offset: exact.offset.get() });
        }
    }

    pub(super) fn peek_byte(&self, env: &dyn StoryVars, bank: u8, address: u8) -> u8 {
        if let Some(scratch) = self.checked_scratch(bank) {
            // Export bytes only for masked assignment merging; authoritative reads below
            // always validate the selected knowledge before this value can drive execution.
            return scratch.current().values().get(usize::from(address)).copied().unwrap_or_else(|| {
                self.invalid_exact();
                0
            });
        }
        let k = self.exact_key(bank, address);
        if let Some(exact) = &self.exact {
            if let Some(native) = &exact.native {
                if canonical_persistent_bank(bank).is_some() {
                    let value = native.state().var(k.0, k.1);
                    let assigned = exact.masks.get(&k).copied().unwrap_or(0);
                    return exact.seeds.get(&k).map_or(value, |seed| (value & assigned) | (seed & !assigned));
                }
            }
        }
        self.writes.get(&k).copied().unwrap_or_else(|| match &self.exact {
            Some(exact) => exact.seeds.get(&k).copied().unwrap_or_else(|| exact.story.var(k.0, k.1)),
            None => env.byte(bank, address),
        })
    }

    pub(super) fn read_masked(&self, env: &dyn StoryVars, bank: u8, address: u8, mask: u8) -> u8 {
        if self.has_checked_scratch() && field_wide_bytes(bank).is_none() { self.invalid_exact(); return 0; }
        if let Some(scratch) = self.checked_scratch(bank) {
            return match scratch.read_masked_u8(bank, usize::from(address), mask) {
                Ok(value) => value,
                Err(error) => { self.fail_exact(error.into()); 0 },
            };
        }
        if let Some(exact) = &self.exact {
            let k = key(bank, address);
            let current = exact.native.as_ref().map_or(&exact.story, NativeStoryBuilder::state);
            let baseline = if exact.seeds.contains_key(&k) || k.0 == 5 { 255 } else { current.known_mask(k.0, k.1) };
            let known = baseline | exact.masks.get(&k).copied().unwrap_or(0);
            if known & mask != mask {
                self.fail_exact(ExactError::UnknownRead { offset: exact.offset.get(), bank: k.0, address, mask: mask & !known });
            }
        }
        self.peek_byte(env, bank, address)
    }

    pub(super) fn set_masked(&mut self, env: &dyn StoryVars, bank: u8, address: u8, value: u8, mask: u8) {
        let before = self.peek_byte(env, bank, address);
        self.set_byte_with_mask(bank, address, (before & !mask) | (value & mask), mask);
    }

    pub(super) fn set_byte_with_mask(&mut self, bank: u8, address: u8, value: u8, mask: u8) {
        let k = self.exact_key(bank, address);
        if self.exact.is_some() && !self.native_exact() && k.0 == 3 && (9..=11).contains(&address) {
            self.fail_exact(ExactError::PartyMirror { address });
            return;
        }
        if let Some(exact) = &mut self.exact {
            if canonical_persistent_bank(bank).is_none() && !matches!(bank, 5 | 6) {
                self.invalid_exact();
                return;
            }
            exact.record_persistent_assignment(bank, address, value, mask);
            if let Some(scratch) = &mut exact.scratch {
                if matches!(bank, 5 | 6) {
                    match scratch.write_masked_u8(bank, usize::from(address), value, mask) {
                        Ok(changed) => if changed { self.idle.clear(); },
                        Err(error) => { self.fail_exact(error.into()); return; },
                    }
                }
            }
            if let Some(native) = &mut exact.native {
                if canonical_persistent_bank(bank).is_some() {
                    if let Err(error) = native.write_masked(bank, address, value, mask) {
                        self.fail_exact(error.into());
                        return;
                    }
                }
            }
            let old_mask = exact.masks.get(&k).copied().unwrap_or(0);
            let old = self.writes.insert(k, value);
            exact.masks.insert(k, old_mask | mask);
            if old != Some(value) || old_mask & mask != mask { self.idle.clear(); }
        } else {
            self.writes.insert(k, value);
            self.idle.clear();
        }
    }

    pub(super) fn exact_source_byte(&self, env: &dyn StoryVars, ins: &Instruction, high: bool) -> Option<u8> {
        let Some(source) = ins.operand("value") else { self.invalid_exact(); return None };
        if source.bank == 0 { return Some(((source.value >> if high { 8 } else { 0 }) & 255) as u8); }
        let address = (source.value & 255) as u8;
        if high && self.has_checked_scratch() {
            match field_wide_bytes(source.bank) {
                // The discarded low byte cannot affect HBYTE. Byte-wide selectors
                // therefore produce zero without requiring knowledge of that byte.
                Some(1) => return Some(0),
                Some(2) => if let Some(scratch) = self.checked_scratch(source.bank) {
                    return match scratch.read_masked_u16(source.bank, usize::from(address), 0xFF00) {
                        Ok(value) => Some((value >> 8) as u8),
                        Err(error) => { self.fail_exact(error.into()); None },
                    };
                },
                _ => { self.invalid_exact(); return None; },
            }
        }
        let at = if high { address.checked_add(1) } else { Some(address) };
        let Some(at) = at else { self.invalid_exact(); return None };
        Some(self.var_byte(env, source.bank, at))
    }

    /// Resolve original PartyReplace:254 forces an empty slot;255 admits an unused member
    /// of the preceding party. Slots remain ordered. Holes cannot be persisted by the current
    /// canonical-party table, so a nontrailing empty slot refuses instead of being compacted.
    pub(super) fn exact_party(&mut self, ins: &Instruction) {
        let mut requested = Vec::new();
        let mut seen = BTreeSet::new();
        for name in ["character1", "character2", "character3"] {
            let member = match ins.literal(name) {
                Some(id @ 0..=8) => {
                    let id = id as u8;
                    if !seen.insert(id) { self.invalid_exact(); return; }
                    id
                }
                Some(id @ (254 | 255)) => id as u8,
                _ => { self.invalid_exact(); return; }
            };
            requested.push(member);
        }
        let Some(exact) = &self.exact else { return };
        let mut main = [255; 3];
        for (slot, &member) in main.iter_mut().zip(&exact.party) { *slot = member; }
        let mut bank = main;
        if requested.contains(&255) {
            // Original255 retains raw bank slots, which need not have the main party's order.
            // No canonical-party inference can establish missing persistent byte knowledge.
            for (slot, address) in bank.iter_mut().zip(9..=11) {
                *slot = self.var_byte(&exact.story, 3, address);
            }
            if self.exact_fault().is_some() { return; }
        }
        // Without255 PartyReplace overwrites all three slots, independently of old raw values.
        // The placeholder above is never journaled as initialization.
        let Ok(requested) = <[u8; 3]>::try_from(requested) else { self.invalid_exact(); return };
        let Ok(change) = crate::script::party::transition(bank, main, PartyOp::Set(requested)) else { self.invalid_exact(); return };
        if change.main != change.bank { self.invalid_exact(); return; }
        let mut party = Vec::new();
        let mut empty = false;
        for member in change.main {
            if member == 255 { empty = true; }
            else if empty { self.invalid_exact(); return; }
            else { party.push(member); }
        }
        let mut resolved = [254; 3];
        for (slot, &member) in resolved.iter_mut().zip(&party) { *slot = member; }
        let Some(exact) = &mut self.exact else { return };
        if exact.party != party {
            self.idle.clear();
            exact.party_changed = true;
        }
        exact.party = party;
        exact.party_ops.push(PartyOp::Set(resolved));
        // Original ADD_PARTY_MEMBER in PartyReplace writes physical memory_bank_2[9..11].
        // These are actual PRTYE assignments, not values inferred during initialization.
        // GETPC uses the same resolved tuple, and direct bank3/4 reads use this exact journal.
        for (address, member) in (9..=11).zip(resolved) {
            let value = if member == 254 { 255 } else { member };
            exact.record_persistent_assignment(3, address, value, 255);
            let old = self.writes.insert((3, address), value);
            let old_mask = exact.masks.insert((3, address), 255);
            if old != Some(value) || old_mask != Some(255) { self.idle.clear(); }
        }
    }

    pub(super) fn party_member(&self, env: &dyn StoryVars, slot: u8) -> Option<u8> {
        if let Some(native) = self.exact.as_ref().and_then(|exact| exact.native.as_ref()) {
            if slot >= 3 { return None; }
            return match native.state().native_party().and_then(|party| party.known_main_slot(slot).ok().flatten()) {
                Some(255) => None,
                Some(member @ 0..=8) => Some(member),
                Some(member) => { self.fail_exact(NativePartyOpError::UnsupportedSelector(member).into()); None },
                None => { self.fail_exact(NativePartyOpError::UnknownMainSlot(slot).into()); None },
            };
        }
        match &self.exact {
            Some(exact) => exact.party.get(usize::from(slot)).copied(),
            None => env.party_member(slot),
        }
    }

    pub(super) fn in_party(&self, env: &dyn StoryVars, character: u8) -> bool {
        if self.native_exact() {
            if character > 8 { self.fail_exact(NativePartyOpError::UnsupportedSelector(character).into()); return false; }
            // IFPRTYQ walks the raw field tuple in order and returns on its first match.
            for address in 9..=11 {
                if self.var_byte(env, 3, address) == character { return true; }
            }
            return false;
        }
        match &self.exact { Some(exact) => exact.party.contains(&character), None => env.in_party(character) }
    }

    pub(super) fn native_exact(&self) -> bool {
        self.exact.as_ref().is_some_and(|exact| exact.native.is_some())
    }

    pub(super) fn native_word(&mut self, bank: u8, address: u8, value: u16) -> bool {
        if canonical_persistent_bank(bank).is_none() { return false; }
        let Some(exact) = &mut self.exact else { return false };
        let Some(native) = &mut exact.native else { return false };
        if let Err(error) = native.write_word(bank, address, value) {
            self.fail_exact(error.into());
            return true;
        }
        for (at, byte) in [address, address + 1].into_iter().zip(value.to_le_bytes()) {
            let k = key(bank, at);
            exact.record_persistent_assignment(bank, at, byte, 255);
            let old = self.writes.insert(k, byte);
            let old_mask = exact.masks.insert(k, 255);
            if old != Some(byte) || old_mask != Some(255) { self.idle.clear(); }
        }
        true
    }

    pub(super) fn native_member(&self, character: u8) -> bool {
        if character > 8 { self.fail_exact(NativePartyOpError::UnsupportedSelector(character).into()); return false; }
        let available = self.exact.as_ref().and_then(|exact| exact.native.as_ref())
            .and_then(|native| native.state().native_party())
            .and_then(|party| party.known_available(character).ok().flatten());
        match available {
            Some(value) => value,
            None => { self.fail_exact(ExactError::UnknownAvailability(character)); false },
        }
    }

    pub(super) fn exact_message(&self, raw: &[u8], env: &dyn StoryVars) -> Vec<Piece> {
        struct Current<'a> { walk: &'a Walk, env: &'a dyn StoryVars }
        impl StoryVars for Current<'_> {
            fn byte(&self, bank: u8, address: u8) -> u8 { self.walk.var_byte(self.env, bank, address) }
            fn party_member(&self, slot: u8) -> Option<u8> { self.walk.party_member(self.env, slot) }
        }
        parse_message(raw, &Current { walk: self, env })
    }

    pub(super) fn native_party_instruction(&mut self, env: &dyn StoryVars, ins: &Instruction) {
        if ins.opcode == 0x0B {
            // Native GTPYE reads each raw source immediately before its destination write.
            // Stage the complete instruction so a late unknown source or invalid destination
            // cannot leave the first assignments in the live execution overlay.
            let mut staged = self.clone();
            for (address, name) in (9..=11).zip(["var1", "var2", "var3"]) {
                let Some((bank, destination)) = Self::dest(ins, name) else { self.invalid_exact(); return };
                let value = staged.var_byte(env, 3, address);
                if staged.exact_fault().is_some() { break; }
                staged.set_byte(bank, destination, value);
                if staged.exact_fault().is_some() { break; }
            }
            if let Some(error) = staged.exact_fault() { self.fail_exact(error); }
            else { *self = staged; }
            return;
        }
        let operation = match ins.opcode {
            0x0A | 0xCA => {
                let mut slots = [0; 3];
                // SPTYE snapshots all three operand values before PartyReplace starts.
                for (out, name) in slots.iter_mut().zip(["character1", "character2", "character3"]) {
                    if ins.operand(name).is_none() { self.invalid_exact(); return; }
                    let value = self.num(env, ins, name, Width::U8);
                    let Ok(value) = u8::try_from(value) else { self.invalid_exact(); return };
                    *out = value;
                }
                NativePartyOp::Set(slots)
            }
            0xC8 | 0xC9 | 0xCD..=0xCF => {
                let Some(character) = ins.literal("character").and_then(|value| u8::try_from(value).ok()) else { self.invalid_exact(); return };
                match ins.opcode {
                    0xC8 => NativePartyOp::Add(character),
                    0xC9 => NativePartyOp::Remove(character),
                    0xCD => {
                        let Some(exists) = ins.literal("exists") else { self.invalid_exact(); return };
                        NativePartyOp::Available { character, available: exists != 0 }
                    }
                    0xCE => NativePartyOp::Lock { character, locked: true },
                    _ => NativePartyOp::Lock { character, locked: false },
                }
            }
            _ => { self.invalid_exact(); return; },
        };
        if self.exact_fault().is_some() { return; }
        let Some(exact) = &mut self.exact else { return };
        let Some(native) = &mut exact.native else { return };
        let effect = match native.party(operation) {
            Ok(effect) => effect,
            Err(error) => { self.fail_exact(error.into()); return; },
        };
        if effect.changed() { self.idle.clear(); }
        // Mirror only for inspection/read masks; the effect already owns these assignments.
        for (slot, (value, _)) in effect.after().raw_slots().into_iter().enumerate() {
            if effect.assignments().raw_slots & (1 << slot) != 0 {
                let k = (3, 9 + slot as u8);
                exact.record_persistent_assignment(k.0, k.1, value, 255);
                self.writes.insert(k, value);
                exact.masks.insert(k, 255);
            }
        }
    }

    /// CC may reassert control of the uniquely observed trigger actor, never transfer it.
    pub(super) fn exact_control(&self, ins: &Instruction) {
        let actor = ins.literal("entity").and_then(|value| usize::try_from(value).ok());
        let observed = self.exact.as_ref().map(|exact| exact.triangle_sources.values().copied().collect::<BTreeSet<_>>());
        if !observed.is_some_and(|actors| actors.len() == 1 && actor.is_some_and(|actor| actors.contains(&actor))) {
            self.invalid_exact();
        }
    }

    pub(super) fn exact_battle_mode(&mut self, ins: &Instruction) {
        let Some(flags) = ins.literal("flags").and_then(|flags| u16::try_from(flags).ok()) else { self.invalid_exact(); return };
        if let Some(exact) = &mut self.exact { exact.battle_flags = flags; }
    }

    pub(super) fn exact_instruction(&mut self, ins: &Instruction, entity: usize) {
        let Some(exact) = &mut self.exact else { return };
        exact.offset.set(ins.offset);
        // No endpoint is simulated here. Even an awaited movement invalidates an earlier
        // authoritative triangle, while party-wide movement invalidates every actor's position.
        let actor_changed = matches!(ins.opcode, 0xA1 | 0xA5..=0xAA | 0xAD | 0xC0 | 0xC2 | 0xC3) && exact.moved_actors.insert(entity);
        let all_changed = matches!(ins.opcode, 0x08 | 0x09 | 0x2A) && !exact.all_positions_changed;
        if all_changed { exact.all_positions_changed = true; }
        // A preceding JMP/IF cannot be mistaken for an unchanged idle loop before the now-stale
        // GETAI is reached and either proved idle or refused.
        if actor_changed || all_changed { self.idle.clear(); }
        // Only state-neutral presentation commands may disappear. All other unimplemented
        // instructions, notably resources, party membership and variable read-backs, refuse.
        if ins.stray_banks { self.invalid_exact(); return; }
        let interpreted = matches!(ins.opcode, 0x00..=0x03 | 0x07 | 0x10..=0x19 | 0x30..=0x32 | 0x38 | 0x40 | 0x48
            | 0x60 | 0x70 | 0x72 | 0x74 | 0x7E | 0x82..=0x84 | 0x99..=0x9B | 0xA4 | 0xB9 | 0xBF | 0xCA | 0xCB | 0xFF)
            || (exact.native.is_some() && matches!(ins.opcode, 0x0A | 0x0B | 0xC8 | 0xC9 | 0xCC..=0xCF))
            || arith_of(ins.opcode).is_some();
        if !interpreted && !presentation(ins.opcode) {
            self.fail_exact(ExactError::Unsupported { offset: ins.offset, opcode: ins.opcode });
        }
    }

    /// A narrowly proven asynchronous request can be treated as awaited: its only state effect
    /// is a final scratch completion byte, and the caller immediately polls exactly that byte
    /// through an otherwise camera-only loop. This is the authored lift's request/join shape.
    /// Other asynchronous scheduling remains unsupported.
    pub(super) fn exact_joined_request(&self, script: &FieldScript<'_>, env: &dyn StoryVars, ins: &Instruction) -> bool {
        fn proof(walk: &Walk, script: &FieldScript<'_>, env: &dyn StoryVars, ins: &Instruction) -> Option<()> {
            let guard = decode_at(script.bytes(), ins.end(), 0).ok()?;
            if !matches!(guard.opcode, 0x14 | 0x15) || guard.literal("op") != Some(0) { return None; }
            let (bank, address) = Walk::dest(&guard, "left")?;
            if !matches!(bank, 5 | 6) { return None; }
            let signal = u8::try_from(guard.literal("right")?).ok()?;
            if walk.var_byte(env, bank, address) == signal { return None; }
            let exit_jump = decode_at(script.bytes(), guard.end(), 0).ok()?;
            if !matches!(exit_jump.opcode, 0x10 | 0x11) { return None; }
            let exit = exit_jump.jump_target?;
            let mut pc = guard.jump_target?;
            if pc != exit_jump.end() || exit <= pc { return None; }
            let mut joined = false;
            for _ in 0..64 {
                let op = decode_at(script.bytes(), pc, 0).ok()?;
                if matches!(op.opcode, 0x12 | 0x13) {
                    joined = op.jump_target == Some(guard.offset) && op.end() == exit;
                    break;
                }
                if !matches!(op.opcode, 0x24 | 0x5F | 0x61..=0x68 | 0x6A..=0x6C | 0x6F) || op.end() >= exit { return None; }
                pc = op.end();
            }
            if !joined { return None; }
            let entity = usize::try_from(ins.literal("entity")?).ok()?;
            let slot = usize::try_from(ins.literal("script")?).ok()?;
            completion_script(script, entity, slot, Some((key(bank, address), signal)), true, 0).then_some(())
        }
        proof(self, script, env, ins).is_some()
    }

    pub(super) fn exact_presentation_request(&self, script: &FieldScript<'_>, ins: &Instruction) -> bool {
        let target = ins.literal("entity").zip(ins.literal("script"))
            .and_then(|(e, s)| Some((usize::try_from(e).ok()?, usize::try_from(s).ok()?)));
        target.is_some_and(|(e, s)| completion_script(script, e, s, None, false, 0))
    }

    pub(super) fn exact_countdown(&mut self, ins: &Instruction) {
        let duration = ins.literal("hours").zip(ins.literal("minutes")).zip(ins.literal("seconds"))
            .and_then(|((h, m), s)| u32::try_from(h.checked_mul(3600)?.checked_add(m.checked_mul(60)?)?.checked_add(s)?).ok());
        let Some(exact) = &mut self.exact else { return };
        if duration.is_some() && duration == exact.countdown && !exact.countdown_seen {
            exact.countdown_seen = true;
        } else {
            self.fail_exact(ExactError::Countdown);
        }
    }

    /// A trigger may supply GETAI's exact authoritative destination. No other actor read-back
    /// is inferred from a placeholder position or a default bank value.
    pub(super) fn exact_getai(&mut self, ins: &Instruction) {
        let Some((bank, address)) = Self::dest(ins, "var_triangle") else { self.invalid_exact(); return };
        if self.has_checked_scratch() && (matches!(bank, 5 | 6)
            || self.exact.as_ref().and_then(|exact| exact.triangle_selectors.get(&key(bank, address))) != Some(&bank)) {
            self.fail_exact(ExactError::ScratchTrigger);
            return;
        }
        let entity = ins.literal("entity").and_then(|value| usize::try_from(value).ok());
        let value = self.exact.as_ref().and_then(|exact| {
            let entity = entity?;
            if exact.all_positions_changed || exact.moved_actors.contains(&entity)
                || exact.triangle_sources.get(&key(bank, address)) != Some(&entity) { return None; }
            let low = *exact.seeds.get(&key(bank, address))?;
            if exact.scratch.is_some() && field_wide_bytes(bank) == Some(1) { return Some(u16::from(low)); }
            let high = address.checked_add(1)?;
            Some(u16::from_le_bytes([low, *exact.seeds.get(&key(bank, high))?]))
        });
        match value {
            Some(value) => {
                if self.frames.len() == 1 {
                    if let Some(exact) = &mut self.exact { exact.root_triangle_reads.insert(ins.offset); }
                }
                self.set_word(bank, address, value);
            }
            None => self.invalid_exact(),
        }
    }

    pub(super) fn exact_tainted_getai(&self, ins: &Instruction) -> bool {
        if ins.opcode != 0xB9 { return false; }
        self.exact.as_ref().is_some_and(|exact| exact.all_positions_changed
            || ins.literal("entity").and_then(|entity| usize::try_from(entity).ok())
                .is_some_and(|entity| exact.moved_actors.contains(&entity)))
    }

    /// A completed action may return to a polling loop after moving its actor. Do not invent
    /// the new triangle: finish only if every possible fresh observation leads back to this
    /// same read through comparisons and jumps, with no further observable instruction.
    pub(super) fn exact_completed_idle(&self, script: &FieldScript<'_>, env: &dyn StoryVars, ins: &Instruction) -> bool {
        let Some(exact) = &self.exact else { return false };
        if ins.opcode != 0xB9 || ins.stray_banks || self.frames.len() != 1 || exact.battle.is_none()
            || !exact.root_triangle_reads.contains(&ins.offset) { return false; }
        let Some((bank, address)) = Self::dest(ins, "var_triangle") else { return false };
        // GETAI to persistent storage would itself be a new durable assignment, even if every
        // branch below it is idle. Only a scratch read may disappear at this boundary.
        if !matches!(bank, 5 | 6) { return false; }
        let Some(high) = address.checked_add(1) else { return false };
        let Some(actor) = ins.literal("entity").and_then(|actor| usize::try_from(actor).ok()) else { return false };
        if exact.triangle_sources.get(&key(bank, address)) != Some(&actor)
            || !(exact.all_positions_changed || exact.moved_actors.contains(&actor)) { return false; }
        let Some(code) = script.bytes().get(..script.layout().code_end) else { return false };
        let probe = self.clone(); // Dependency audit in this speculative proof cannot alter the live walk.
        let observed = [key(bank, address), key(bank, high)];
        let mut proof = IdleProof { walk: &probe, env, code, anchor: ins.offset, observed,
            remaining: 128, visiting: BTreeSet::new(), proven: BTreeSet::new() };
        proof.returns(ins.end())
    }

    pub(super) fn exact_battle(&mut self, formation: u16) -> bool {
        let Some(exact) = &mut self.exact else { return false };
        if exact.after_battle == Some(formation) && exact.battle.is_none() {
            exact.battle = Some(ExactBattle { formation, flags: exact.battle_flags });
            exact.battle_story = exact.native.as_ref().map(|native| native.state().clone());
            exact.battle_scratch = exact.scratch.as_ref().map(|scratch| scratch.current().clone());
            true
        } else {
            self.fail_exact(ExactError::Battle);
            false
        }
    }

    fn exact_result(&self, end: End) -> Result<ExactRun, ExactError> {
        if let Some(error) = self.exact_fault() { return Err(error); }
        if self.unresolved { return Err(ExactError::Unresolved); }
        if end != End::Finished { return Err(ExactError::End(end)); }
        let exact = self.exact.as_ref().ok_or(ExactError::LegacyStory)?;
        if exact.after_battle.is_some() && exact.battle.is_none() { return Err(ExactError::Battle); }
        if exact.countdown.is_some() && !exact.countdown_seen { return Err(ExactError::Countdown); }
        let mut writes = Vec::new();
        for (&(bank, address), &mask) in &exact.masks {
            if canonical_persistent_bank(bank).is_none() { continue; }
            let value = self.writes.get(&(bank, address)).ok_or(ExactError::InvalidOperand { offset: exact.offset.get() })?;
            writes.push(ExactWrite { bank, address, value: *value & mask, mask });
        }
        Ok(ExactRun { writes, battle: exact.battle, party: exact.party_ops.clone(),
            native: exact.native.as_ref().map(NativeStoryBuilder::snapshot), battle_story: exact.battle_story.clone(),
            scratch: exact.scratch.as_ref().map(FieldScratchRun::snapshot).transpose()?, battle_scratch: exact.battle_scratch.clone() })
    }
}

/// A deliberately small abstract interpreter for a completed action's idle tail. The next
/// triangle is symbolic; every branch it can affect is explored. All other inputs must already
/// be exact. Only returning to the same read is a successful leaf; another cycle is not proof.
struct IdleProof<'a> {
    walk: &'a Walk,
    env: &'a dyn StoryVars,
    code: &'a [u8],
    anchor: usize,
    observed: [(u8, u8); 2],
    remaining: usize,
    visiting: BTreeSet<usize>,
    proven: BTreeSet<usize>,
}

impl IdleProof<'_> {
    /// None inside Some is the symbolic fresh triangle; outer None is an invalid dependency.
    fn value(&self, ins: &Instruction, name: &str, width: Width) -> Option<Option<i32>> {
        let operand = ins.operand(name)?;
        if operand.bank == 0 { return Some(Some(operand.value)); }
        let address = (operand.value & 255) as u8;
        let mut symbolic = self.observed.contains(&key(operand.bank, address));
        if !matches!(width, Width::U8) {
            symbolic |= self.observed.contains(&key(operand.bank, address.checked_add(1)?));
        }
        if symbolic { return Some(None); }
        let value = self.walk.num(self.env, ins, name, width);
        self.walk.exact_fault().is_none().then_some(Some(value))
    }

    fn returns(&mut self, pc: usize) -> bool {
        if pc == self.anchor || self.proven.contains(&pc) { return true; }
        if self.remaining == 0 || !self.visiting.insert(pc) { return false; }
        self.remaining -= 1;
        let Ok(ins) = decode_at(self.code, pc, 0) else { return false };
        if ins.stray_banks { return false; }
        let result = match ins.opcode {
            0x10..=0x13 => ins.jump_target.is_some_and(|target| self.returns(target)),
            0x14..=0x19 => {
                let (width, bits) = match ins.opcode { 0x14 | 0x15 => (Width::U8, 8), 0x16 | 0x17 => (Width::S16, 16), _ => (Width::U16, 16) };
                let Some(op @ 0..=10) = ins.literal("op") else { return false };
                let Some(target) = ins.jump_target else { return false };
                let values = (self.value(&ins, "left", width), self.value(&ins, "right", width));
                if matches!(op, 9 | 10) && !matches!(values.1, Some(Some(bit)) if bit >= 0 && bit < bits as i32) { return false; }
                match values {
                    (Some(Some(left)), Some(Some(right))) => self.returns(if compare(op, left, right, bits) { ins.end() } else { target }),
                    (Some(_), Some(_)) => self.returns(ins.end()) && self.returns(target),
                    _ => false,
                }
            }
            _ => false,
        };
        self.visiting.remove(&pc);
        if result { self.proven.insert(pc); }
        result
    }
}

/// Linear presentation request tree, with one optional final completion assignment. Refusing
/// branches, asynchronous descendants and other writes makes the join proof independent of
/// values that another script could change while the request is in flight.
fn completion_script(script: &FieldScript<'_>, entity: usize, slot: usize, signal: Option<((u8, u8), u8)>, allow_movement: bool, depth: usize) -> bool {
    if depth > Limits::default().depth { return false; }
    let Some(span) = script.span(entity, slot) else { return false };
    let mut pc = span.start;
    for _ in 0..512 {
        let Ok(ins) = decode_at(script.bytes(), pc, 0) else { return false };
        if ins.stray_banks { return false; }
        match ins.opcode {
            0x00 => return signal.is_none(),
            0x80 => {
                let Some((expected, value)) = signal else { return false };
                let Some((bank, address)) = Walk::dest(&ins, "var") else { return false };
                return key(bank, address) == expected && ins.literal("value") == Some(i32::from(value))
                    && decode_at(script.bytes(), ins.end(), 0).ok().is_some_and(|next| next.opcode == 0x00);
            }
            0x03 => {
                let target = ins.literal("entity").zip(ins.literal("script"))
                    .and_then(|(e, s)| Some((usize::try_from(e).ok()?, usize::try_from(s).ok()?)));
                if !target.is_some_and(|(e, s)| completion_script(script, e, s, None, allow_movement, depth + 1)) { return false; }
            }
            0x40 | 0x7E | 0xA4 => {},
            // The station's finite actor script trees may alter model bindings/positions. Their
            // positions are tainted during execution; no later read-back may use the old snapshot.
            0xA1 | 0xA5..=0xA8 | 0xC7 => {},
            opcode if presentation(opcode) && (allow_movement || !matches!(opcode,
                0x08 | 0x09 | 0x2A | 0x33 | 0x4A | 0xA0 | 0xA1 | 0xA5..=0xA8 | 0xAD | 0xC0 | 0xC2..=0xC7 | 0xD0..=0xD3 | 0xD6..=0xD9)) => {},
            _ => return false,
        }
        // Original scripts may share a tail or RET by falling through; only the code-area bound applies.
        pc = ins.end();
        if pc >= script.layout().code_end { return false; }
    }
    false
}

fn run(walk: &mut Walk, script: &FieldScript<'_>, state: &StoryState, answers: &[usize]) -> Result<ExactRun, ExactError> {
    let mut answers = answers.iter().copied();
    let mut answer = None;
    loop {
        let beat = walk.resume(script, state, answer.take());
        if let Some(error) = walk.exact_fault() { return Err(error); }
        match beat {
            Beat::Message(message) => if let Some(choice) = message.choice {
                let chosen = answers.next().ok_or(ExactError::MissingAnswer)?;
                if !(choice.first..=choice.last).contains(&chosen) {
                    return Err(ExactError::InvalidAnswer { answer: chosen, first: choice.first, last: choice.last });
                }
                answer = Some(chosen);
            },
            Beat::End(end) => {
                if answers.next().is_some() { return Err(ExactError::ExtraAnswer); }
                return walk.exact_result(end);
            }
        }
    }
}

pub fn run_script_exact(script: &FieldScript<'_>, state: &StoryState, seed: u32, entry: &ExactScript, answers: &[usize]) -> Result<ExactRun, ExactError> {
    run_script_exact_mode(script, state, None, seed, entry, answers)
}

/// Execute against an explicit field-visit snapshot. Unknown scratch is never cleared or
/// inferred from story progress. `through_init` is an explicit authored root-entry choice;
/// `setup` instead audits existing-visit initialization and cannot change scratch knowledge.
/// Scratch trigger seeds/read-back remain unsupported: this API has no separate authoritative
/// triangle-value input. Persistent GETAI observations must retain the original instruction
/// selector and supply exactly the bytes it reads back. Canonicalized panel metadata does not
/// establish that provenance, so no checked panel entry is provided.
pub fn run_script_exact_with_scratch(script: &FieldScript<'_>, state: &StoryState, scratch: &FieldScratch, seed: u32, entry: &ExactScript, answers: &[usize]) -> Result<ExactRun, ExactError> {
    run_script_exact_mode(script, state, Some(scratch), seed, entry, answers)
}

fn run_script_exact_mode(script: &FieldScript<'_>, state: &StoryState, scratch: Option<&FieldScratch>, seed: u32, entry: &ExactScript, answers: &[usize]) -> Result<ExactRun, ExactError> {
    if scratch.is_some() {
        if entry.seeds.iter().any(|&((bank, _), _)| matches!(bank, 5 | 6)) { return Err(ExactError::ScratchSeed); }
        if entry.triangle_sources.iter().any(|&(bank, _, _)| matches!(bank, 5 | 6)) { return Err(ExactError::ScratchTrigger); }
    }
    if script.span(entry.entity, entry.slot).is_none() { return Err(ExactError::MissingScript); }
    let mut walk = Walk::script(entry.entity, entry.slot, seed, Limits::default()).pressing(entry.keys)
        .exact(state, &entry.seeds, entry.after_battle)?.with_scratch(scratch);
    if let Some(exact) = &mut walk.exact {
        exact.countdown = entry.countdown;
        for &(bank, address, entity) in &entry.triangle_sources {
            if entity >= script.entity_count() { return Err(ExactError::InvalidOperand { offset: 0 }); }
            let bytes = if scratch.is_some() { field_wide_bytes(bank).ok_or(ExactError::ScratchTrigger)? } else { 2 };
            let high_known = if bytes == 2 {
                let high = address.checked_add(1).ok_or(ExactError::InvalidOperand { offset: 0 })?;
                exact.seeds.contains_key(&key(bank, high))
            } else { true };
            if !exact.seeds.contains_key(&key(bank, address)) || !high_known
                || exact.triangle_sources.insert(key(bank, address), entity).is_some_and(|old| old != entity) {
                return Err(ExactError::InvalidOperand { offset: 0 });
            }
            if scratch.is_some() && exact.triangle_selectors.insert(key(bank, address), bank).is_some_and(|old| old != bank) {
                return Err(ExactError::ScratchTrigger);
            }
        }
    }
    if entry.setup {
        let setup = audited_setup(script, entry.entity, state, false, scratch)?;
        if let (Some(exact), Some(setup_exact)) = (&mut walk.exact, &setup.exact) { exact.battle_flags = setup_exact.battle_flags; }
        // Trigger seeds were observed after field setup. Setup placements precede those fresh
        // observations, unlike movements executed by the selected root below.
        walk.writes = setup.writes;
        for &(k, _) in &entry.seeds { walk.writes.remove(&key(k.0, k.1)); }
    }
    walk.through_init = entry.through_init;
    if let Some(offset) = entry.start_offset {
        if offset >= script.layout().code_end || decode_at(script.bytes(), offset, 0).is_err() { return Err(ExactError::MissingScript); }
        walk.started = true;
        walk.frames.push(Frame::new(offset, entry.entity));
    }
    run(&mut walk, script, state, answers)
}

/// Audit setup for availability without treating talk-time setup as a new field entry. A setup
/// assignment must already be fully known with that value; changing values or knowledge refuses.
pub fn run_talk_exact(script: &FieldScript<'_>, entity: usize, state: &StoryState, seed: u32, answers: &[usize]) -> Result<ExactRun, ExactError> {
    run_talk_exact_mode(script, entity, state, None, seed, answers)
}

/// Talk in an existing field visit, retaining its explicit scratch values and knowledge.
/// Setup is a dry-run audit: even a scratch change followed by restoration refuses.
pub fn run_talk_exact_with_scratch(script: &FieldScript<'_>, entity: usize, state: &StoryState, scratch: &FieldScratch, seed: u32, answers: &[usize]) -> Result<ExactRun, ExactError> {
    run_talk_exact_mode(script, entity, state, Some(scratch), seed, answers)
}

fn run_talk_exact_mode(script: &FieldScript<'_>, entity: usize, state: &StoryState, scratch: Option<&FieldScratch>, seed: u32, answers: &[usize]) -> Result<ExactRun, ExactError> {
    if script.span(entity, INIT_SCRIPT).is_none() || script.span(entity, TALK_SCRIPT).is_none() { return Err(ExactError::MissingScript); }
    let setup = audited_setup(script, entity, state, true, scratch)?;
    if setup.talk_off || !setup.visible { return Err(ExactError::NotTalkable); }
    let mut walk = Walk::talk(entity, seed, Limits::default()).exact(state, &[], None)?.with_scratch(scratch);
    // Setup scratch values remain available. Persistent assignments above were verified against
    // the exact baseline, so they need neither a second write nor invented known bits.
    if let (Some(exact), Some(setup_exact)) = (&mut walk.exact, &setup.exact) { exact.battle_flags = setup_exact.battle_flags; }
    walk.writes = setup.writes;
    run(&mut walk, script, state, answers)
}

fn audited_setup(script: &FieldScript<'_>, entity: usize, state: &StoryState, main: bool, scratch: Option<&FieldScratch>) -> Result<Walk, ExactError> {
    if script.span(entity, INIT_SCRIPT).is_none() { return Err(ExactError::MissingScript); }
    let mut setup = Walk::script(entity, INIT_SCRIPT, 1, Limits::default()).exact(state, &[], None)?.with_scratch(scratch);
    setup.quiet = main;
    let established = run(&mut setup, script, state, &[])?;
    if established.scratch.as_ref().is_some_and(FieldScratchOutcome::any_change) { return Err(ExactError::SetupScratch); }
    if let Some(native) = &established.native {
        if let Some(event) = native.events().iter().find(|event| event.changed()) {
            return Err(match event {
                NativeStoryEvent::Party(_) => ExactError::SetupParty,
                NativeStoryEvent::Assignment(write) => {
                    let (bank, address) = write.key();
                    ExactError::SetupWrite { bank, address }
                }
            });
        }
    }
    if setup.exact.as_ref().is_some_and(|exact| exact.party_changed) {
        return Err(ExactError::SetupParty);
    }
    if let Some((bank, address)) = setup.exact.as_ref().and_then(|exact| exact.persistent_change) {
        return Err(ExactError::SetupWrite { bank, address });
    }
    for write in established.writes {
        if state.known_mask(write.bank, write.address) & write.mask != write.mask
            || (state.var(write.bank, write.address) ^ write.value) & write.mask != 0 {
            return Err(ExactError::SetupWrite { bank: write.bank, address: write.address });
        }
    }
    Ok(setup)
}

pub fn run_panel_exact(script: &FieldScript<'_>, entity: usize, state: &StoryState, seed: u32, triangle: u16, vars: &[(u8, u8)]) -> Result<ExactRun, ExactError> {
    let main = script.disassemble_partial(entity, INIT_SCRIPT).ok()
        .and_then(|listing| listing.instructions.into_iter().find(|ins| ins.opcode == 0x00).map(|ins| ins.end()))
        .ok_or(ExactError::MissingScript)?;
    let mut entry = ExactScript { entity, slot: INIT_SCRIPT, setup: true, start_offset: Some(main), keys: KEY_OK, ..ExactScript::default() };
    let [lo, hi] = triangle.to_le_bytes();
    for &(bank, address) in vars {
        let high = address.checked_add(1).ok_or(ExactError::InvalidOperand { offset: 0 })?;
        entry.seeds.extend([((bank, address), lo), ((bank, high), hi)]);
        let mut sources = BTreeSet::new();
        let mut starts = BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..crate::script::SCRIPTS_PER_ENTITY {
                let Some(span) = script.span(entity, slot) else { continue };
                if !starts.insert(span.start) { continue; }
                let listing = script.disassemble_partial(entity, slot).map_err(|_| ExactError::MissingScript)?;
                for ins in listing.instructions.iter().filter(|ins| ins.opcode == 0xB9) {
                    let Some((b, a)) = Walk::dest(ins, "var_triangle") else { continue };
                    if key(b, a) != key(bank, address) { continue; }
                    sources.insert(ins.literal("entity").and_then(|e| usize::try_from(e).ok())
                        .ok_or(ExactError::InvalidOperand { offset: ins.offset })?);
                }
            }
        }
        if sources.len() != 1 { return Err(ExactError::InvalidOperand { offset: 0 }); }
        let source = sources.into_iter().next().ok_or(ExactError::InvalidOperand { offset: 0 })?;
        if source >= script.entity_count() { return Err(ExactError::InvalidOperand { offset: 0 }); }
        entry.triangle_sources.push((bank, address, source));
    }
    run_script_exact(script, state, seed, &entry, &[])
}

#[cfg(test)]
mod native_message_tests {
    use super::*;
    use crate::script::synth::SynthScript;
    use crate::story::NativePartyState;

    #[test]
    fn native_window_names_use_the_current_main_slots_after_party_operations() {
        let mut before = StoryState::at(36).into_native_party().unwrap();
        *before.native_party_mut().unwrap() = NativePartyState::from_parts(
            [0, 255, 255], 7, (1, 1), (0, 0),
        ).unwrap();
        let bytes = SynthScript { dialogs: vec![vec![0xF3, 0xFF]], ..Default::default() }
            .entity("npc", vec![(1, vec![0xCA, 2, 254, 254, 0x40, 0, 0, 0])]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let mut walk = Walk::script(0, 1, 7, Limits::default()).exact(&before, &[], None).unwrap();
        let Beat::Message(message) = walk.resume(&script, &before, None) else { panic!("native message") };
        assert_eq!(message.pieces, vec![Piece::Text("Tifa".into())]);
        assert_eq!(walk.exact_fault(), None);
        assert_eq!(before.native_party().unwrap().known_main(), Some([0, 255, 255]));
    }
}
