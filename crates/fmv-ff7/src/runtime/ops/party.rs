//! Party membership, character resources and the remaining save-data instructions: `MMBud`, `MMBLK`, `MMBUK`, `IFMEMBQ`, `CHGLD`, `CKITM`, `STITM`, `DLITM`, `SMTRA`, `DMTRA`, `CMTRA`, `HMPMAX1/2/3`, `MHMMX`, `HPu`, `HPd`, `MPu`, `MPd`, `BTMD2`, `BTRLD`, `TUTOR`, `SPECIAL`, `PMJMP`, `PMJMP2`, `LSTMP`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).
//!
//! # The save-data model
//!
//! The instructions of this family read and change a small part of the save that does not live in the 0x500-byte
//! variable banks: who is in the PHS, the item inventory, the materia list and the characters' HP/MP.
//! [`PartySaveData`] holds exactly that. The host loads it with [`FieldRuntime::set_save_data`] before the field
//! starts and exports it with [`FieldRuntime::save_data`] when it saves. Everything else in [`PartyState`] is
//! transient: it belongs to this field visit and is never persisted.

use std::collections::BTreeMap;

use super::super::{Event, FieldRuntime, Flow, CommandState, NO_CHARACTER, PARTY_SLOTS};

/// Item kinds the inventory can hold (ids 0-319: items, weapons, armour, accessories).
pub const ITEM_KINDS: u16 = 320;
/// The largest stack of one item.
pub const MAX_STACK: u8 = 99;
/// Materia the inventory can hold.
pub const MAX_MATERIA: usize = 200;
/// Playable characters with a record in the save.
pub const CHARACTERS: usize = 9;
/// The bank-1 byte (flat image index) whose bits record materia the party has ever owned.
const MATERIA_FLAGS: usize = 75;
/// The flat index of bank 13/14 address 31: `SMTRA`'s "the list was full" result.
const MATERIA_RESULT: usize = 0x300 + 31;

/// One character's current and maximum HP and MP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharacterVitals {
    pub hp: u16,
    pub max_hp: u16,
    pub mp: u16,
    pub max_mp: u16,
}

/// The persistent part of the save this family reads and writes (see the module documentation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartySaveData {
    /// PHS visibility: bit `c` set when character `c` is available (`MMBud`, `IFMEMBQ`).
    pub phs_visibility: u16,
    /// PHS locking: bit `c` set when the player may not move character `c` in or out (`MMBLK`, `MMBUK`).
    pub phs_locking: u16,
    /// The item inventory: item id → quantity (1-99). Ids are below [`ITEM_KINDS`].
    pub items: BTreeMap<u16, u8>,
    /// The materia list, each owned materia as `id | ap << 8` (24-bit AP), in the order it was obtained.
    pub materia: Vec<u32>,
    /// HP and MP of the nine characters (Cloud, Barret, Tifa, Aeris, Red XIII, Yuffie, Cait Sith, Vincent, Cid).
    pub characters: [CharacterVitals; CHARACTERS],
    /// The field message speed as the options menu stores it (`SMSPD`/`GMSPD` use `255 - speed`).
    pub message_speed: u8,
}

impl PartySaveData {
    /// How many of item `id` the party holds.
    pub fn item_count(&self, id: u16) -> u8 {
        self.items.get(&id).copied().unwrap_or(0)
    }

    /// Add `quantity` of item `id`, capped at [`MAX_STACK`]. Returns `false` for an id the inventory has no slot for.
    pub fn add_item(&mut self, id: u16, quantity: u8) -> bool {
        if id >= ITEM_KINDS {
            return false;
        }
        if quantity > 0 {
            let held = self.items.entry(id).or_default();
            *held = held.saturating_add(quantity).min(MAX_STACK);
        }
        true
    }

    /// Remove `quantity` of item `id` (the stack disappears at zero). Returns `false` for an id with no slot.
    pub fn remove_item(&mut self, id: u16, quantity: u8) -> bool {
        if id >= ITEM_KINDS {
            return false;
        }
        if let Some(held) = self.items.get_mut(&id) {
            *held = held.saturating_sub(quantity);
            if *held == 0 {
                self.items.remove(&id);
            }
        }
        true
    }

    /// Add a materia (`id | ap << 8`) if the list has room.
    pub fn add_materia(&mut self, materia: u32) -> bool {
        if self.materia.len() >= MAX_MATERIA {
            return false;
        }
        self.materia.push(materia);
        true
    }

    /// Remove up to `quantity` owned materia that are exactly `materia` (same id, same AP).
    pub fn remove_materia(&mut self, materia: u32, quantity: u8) {
        let mut left = quantity;
        self.materia.retain(|m| {
            if left > 0 && *m == materia {
                left -= 1;
                false
            } else {
                true
            }
        });
    }

    /// How many owned materia have `id` and exactly `ap` AP.
    pub fn count_materia(&self, id: u8, ap: u32) -> usize {
        self.materia.iter().filter(|m| (**m & 0xFF) as u8 == id && **m >> 8 == ap).count()
    }
}

/// Where `TUTOR` is: the tutorial is a screen the host shows, and the instruction waits for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tutorial {
    #[default]
    Idle,
    /// The host should show tutorial `id` and call [`FieldRuntime::complete_tutorial`].
    Pending(u8),
    /// The host finished it; the instruction completes on its next visit.
    Done(u8),
}

/// This family's state: the persistent save data and the transient values of this field visit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartyState {
    /// The persistent part (see [`PartySaveData`]).
    pub save: PartySaveData,
    /// The result of the last battle (`BTRLD` reads it); the host sets it when a battle returns.
    pub battle_result: u16,
    /// `BTMD2`'s two 16-bit battle mode words (the original calls them battle mode 2 and battle mode 1).
    pub battle_mode2: u16,
    pub battle_mode1: u16,
    /// The field `PMJMP` asked to preload.
    pub preloaded_map: Option<u16>,
    /// The field the party came from (`LSTMP` reads it); the host sets it on field entry.
    pub previous_field: u16,
    /// `SPECIAL`'s switches: the party-menu arrow is hidden, battles are locked, the movie lock.
    pub arrow_disabled: u8,
    pub battle_lock: u8,
    pub movie_lock: u8,
    /// `SPCNM`'s substitutions: character id → text id.
    pub special_names: BTreeMap<u8, u8>,
    /// The nine characters' names in the game's text encoding (set by the host; `PNAME` copies them into the banks).
    pub names: [Vec<u8>; CHARACTERS],
    /// `RSGLB` asked for the savemap to be reset. The runtime never wipes progress itself: the host decides what a
    /// reset means for the instance's save and clears this flag.
    pub globals_reset_requested: bool,
    /// `TUTOR`'s state.
    pub tutorial: Tutorial,
}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    matches!(
        op,
        0x0F | 0x21 | 0x22 | 0x23 | 0x3B | 0x3C..=0x3F | 0x45 | 0x47 | 0x4D | 0x4F | 0x58..=0x5D | 0x6E | 0xCC..=0xCF | 0xD8 | 0xD9
    )
}

/// The save record of a (battle) character id: 0-8 are themselves, the flashback's 9 and 10 use Cait Sith's and
/// Vincent's records, as the original does.
fn record_of(character: u8) -> Option<usize> {
    match character {
        0..=8 => Some(usize::from(character)),
        9 => Some(6),
        10 => Some(7),
        _ => None,
    }
}

/// The bit of a PHS mask for `character` (none for a character beyond the mask's 16 bits).
fn bit(character: u8) -> u16 {
    1u16.checked_shl(u32::from(character)).unwrap_or(0)
}

/// Offset into the flat bank image where `PNAME` writes, by the bank selector's low nibble.
fn name_offset(selector: u8) -> usize {
    match selector & 0xF {
        15 => 1024,
        13 => 768,
        11 => 512,
        3 => 256,
        _ => 0,
    }
}

impl FieldRuntime {
    // ---- host interface ------------------------------------------------------------------------------------

    /// Load the persistent party data (PHS masks, inventory, materia, HP/MP) from the save.
    pub fn set_save_data(&mut self, save: PartySaveData) {
        self.party_ext.save = save;
    }

    /// The persistent party data, as the field left it, for the host to save.
    pub fn save_data(&self) -> &PartySaveData {
        &self.party_ext.save
    }

    /// The family's whole state (the transient values too).
    pub fn party_state(&self) -> &PartyState {
        &self.party_ext
    }

    /// The field the party came from (what `LSTMP` reads).
    pub fn set_previous_field(&mut self, field: u16) {
        self.party_ext.previous_field = field;
    }

    /// The result of the last battle (what `BTRLD` reads).
    pub fn set_battle_result(&mut self, result: u16) {
        self.party_ext.battle_result = result;
    }

    /// Character `character`'s name in the game's text encoding (what `PNAME` copies into the banks).
    pub fn set_character_name(&mut self, character: u8, name: Vec<u8>) {
        if let Some(slot) = self.party_ext.names.get_mut(usize::from(character)) {
            *slot = name;
        }
    }

    /// The tutorial `TUTOR` is waiting on, if any.
    pub fn pending_tutorial(&self) -> Option<u8> {
        match self.party_ext.tutorial {
            Tutorial::Pending(id) => Some(id),
            _ => None,
        }
    }

    /// The host showed the pending tutorial; `TUTOR` completes on its next visit.
    pub fn complete_tutorial(&mut self) {
        if let Tutorial::Pending(id) = self.party_ext.tutorial {
            self.party_ext.tutorial = Tutorial::Done(id);
        }
    }

    // ---- instructions --------------------------------------------------------------------------------------

    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_party(&mut self, op: u8) -> Option<Flow> {
        if !implemented(op) {
            return None;
        }
        Some(match op {
            0x0F => self.special(),
            0x21 => self.tutor(),
            0x22 => {
                self.party_ext.battle_mode2 = self.p16(1);
                self.party_ext.battle_mode1 = self.p16(3);
                self.state.battle_mode = self.p16(1);
                self.advance(5);
                Flow::Continue
            }
            0x23 => {
                let result = self.party_ext.battle_result;
                self.wr16(2, 2, result as i16);
                self.advance(3);
                Flow::Continue
            }
            0x3B => {
                let gil = self.gil.unwrap_or(0);
                self.wr16(1, 2, gil as u16 as i16);
                self.wr16(2, 3, (gil >> 16) as u16 as i16);
                self.advance(4);
                Flow::Continue
            }
            0x3C | 0x3D | 0x3F => {
                for c in self.party() {
                    self.restore(c);
                }
                self.advance(1);
                Flow::Continue
            }
            0x3E => {
                for c in 0..CHARACTERS as u8 {
                    self.restore(c);
                }
                self.advance(1);
                Flow::Continue
            }
            0x45 => self.vitals(|v, n| v.mp = (i32::from(v.mp) + n).clamp(0, i32::from(v.max_mp)) as u16),
            0x47 => self.vitals(|v, n| v.mp = (i32::from(v.mp) - n).clamp(0, i32::from(v.max_mp)) as u16),
            0x4D => self.vitals(|v, n| v.hp = (i32::from(v.hp) + n).clamp(0, i32::from(v.max_hp)) as u16),
            // The wiki says nothing about HP- killing; the original code for it is not in the decompilation, so a
            // living character is kept at 1 HP at least.
            0x4F => self.vitals(|v, n| {
                if v.hp > 0 {
                    v.hp = (i32::from(v.hp) - n).clamp(1, i32::from(v.max_hp).max(1)) as u16;
                }
            }),
            0x58 | 0x59 => self.item_op(op == 0x58),
            0x5A => {
                let id = (self.rd16(1, 2) as u16) & 0x1FF;
                let held = self.party_ext.save.item_count(id);
                self.wr8(2, 4, held);
                self.advance(5);
                Flow::Continue
            }
            0x5B => {
                let materia = self.materia_word(1, 3);
                let ap = materia >> 8;
                let fitted = self.party_ext.save.add_materia(materia);
                if fitted {
                    self.note_materia(materia);
                }
                self.emit(Event::Materia { materia: materia as u8, ap });
                // 0 when the materia fitted, 1 when the list was full.
                self.mem.banks.set(MATERIA_RESULT, u8::from(!fitted));
                self.advance(7);
                Flow::Continue
            }
            0x5C => {
                let materia = self.materia_word(1, 3);
                let quantity = self.p8(7);
                self.party_ext.save.remove_materia(materia, quantity);
                self.advance(8);
                Flow::Continue
            }
            0x5D => {
                // Four AP bytes (the fourth is never part of a 24-bit AP), then the materia id, then the result.
                let ap = u32::from(self.rd8(1, 4)) | (u32::from(self.rd8(2, 5)) << 8) | (u32::from(self.rd8(3, 6)) << 16);
                let id = self.p8(8);
                let count = self.party_ext.save.count_materia(id, ap).min(255) as u8;
                self.wr8(6, 9, count);
                self.advance(10);
                Flow::Continue
            }
            0x6E => {
                let field = self.party_ext.previous_field;
                self.wr16(2, 2, field as i16);
                self.advance(3);
                Flow::Continue
            }
            0xCC => {
                let truth = self.party_ext.save.phs_visibility & bit(self.p8(1)) != 0;
                self.branch(truth, 3, usize::from(self.p8(2)) + 2)
            }
            0xCD => {
                let character = self.p8(2);
                if self.p8(1) != 0 {
                    self.party_ext.save.phs_visibility |= bit(character);
                } else {
                    self.party_ext.save.phs_visibility &= !bit(character);
                    let mut party = self.party();
                    if party.contains(&character) {
                        for slot in party.iter_mut().filter(|c| **c == character) {
                            *slot = NO_CHARACTER;
                        }
                        self.set_party(party);
                    }
                }
                self.advance(3);
                Flow::Continue
            }
            0xCE => {
                self.party_ext.save.phs_locking |= bit(self.p8(1));
                self.advance(2);
                Flow::Continue
            }
            0xCF => {
                self.party_ext.save.phs_locking &= !bit(self.p8(1));
                self.advance(2);
                Flow::Continue
            }
            0xD8 => {
                self.party_ext.preloaded_map = Some(self.p16(1));
                self.advance(3);
                Flow::Continue
            }
            // The original waits until the preloaded field has finished loading. The host loads fields whole, so
            // there is nothing to wait for.
            _ => {
                self.advance(1);
                Flow::Continue
            }
        })
    }

    /// `STITM` (`add`) and `DLITM`: the quantity is packed with the id the way the original does (`qty << 9 | id`).
    fn item_op(&mut self, add: bool) -> Flow {
        let raw = self.rd16(1, 2) as u16;
        let quantity = self.rd8(2, 4);
        let word = (u16::from(quantity) << 9) | raw;
        let (id, packed) = (word & 0x1FF, (word >> 9) as u8);
        let ok = if add { self.party_ext.save.add_item(id, packed) } else { self.party_ext.save.remove_item(id, packed) };
        if !ok {
            self.coverage.anomalies += 1;
        }
        let qty = i32::from(quantity);
        self.emit(Event::Item { item: raw, delta: if add { qty } else { -qty } });
        self.advance(5);
        Flow::Continue
    }

    /// The materia word of an `SMTRA`/`DMTRA` instruction: the id, then three AP bytes, from nibble `n` and byte `at`.
    fn materia_word(&self, n: usize, at: usize) -> u32 {
        (0..4).fold(0u32, |word, i| word | (u32::from(self.rd8(n + i, at + i)) << (8 * i)))
    }

    /// The side effect adding a materia has on the bank-1 flags byte.
    fn note_materia(&mut self, materia: u32) {
        if materia & 0xFF == 44 {
            let flags = self.mem.banks.get(MATERIA_FLAGS) | 2;
            self.mem.banks.set(MATERIA_FLAGS, flags);
        }
    }

    /// Restore character `character`'s HP and MP to their maxima.
    fn restore(&mut self, character: u8) {
        if let Some(v) = record_of(character).and_then(|r| self.party_ext.save.characters.get_mut(r)) {
            v.hp = v.max_hp;
            v.mp = v.max_mp;
        }
    }

    /// `HPu`/`HPd`/`MPu`/`MPd`: change the HP or MP of the character in party slot `p8(2)` by the signed amount.
    fn vitals(&mut self, apply: impl Fn(&mut CharacterVitals, i32)) -> Flow {
        let slot = usize::from(self.p8(2));
        let amount = i32::from(self.rd16(2, 3));
        if slot >= 3 {
            self.coverage.anomalies += 1;
        } else {
            let character = self.mem.banks.get(PARTY_SLOTS + slot);
            if let Some(v) = record_of(character).and_then(|r| self.party_ext.save.characters.get_mut(r)) {
                apply(v, amount);
            }
        }
        self.advance(5);
        Flow::Continue
    }

    /// `TUTOR`: ask the host to show a tutorial and wait for it, as a command does (it shares the original's single
    /// command slot, so it waits while another command is pending).
    fn tutor(&mut self) -> Flow {
        let id = self.p8(1);
        match self.party_ext.tutorial {
            Tutorial::Idle if matches!(self.command, CommandState::Idle) => {
                self.party_ext.tutorial = Tutorial::Pending(id);
                self.emit(Event::Presentation { entity: self.current, mnemonic: "TUTOR", bytes: vec![0x21, id] });
                Flow::Yield
            }
            Tutorial::Done(done) if done == id => {
                self.party_ext.tutorial = Tutorial::Idle;
                self.advance(2);
                Flow::Continue
            }
            _ => Flow::Yield,
        }
    }

    /// `SPECIAL`: the sub-opcode is byte 1; each sub-op has its own length.
    fn special(&mut self) -> Flow {
        match self.p8(1) {
            0xF5 => {
                self.party_ext.arrow_disabled = self.p8(2);
                self.advance(3);
            }
            0xF6 => {
                let character = self.rd8(3, 3);
                let len = usize::from(self.p8(5));
                let base = name_offset(self.p8(2));
                let name = self.party_ext.names.get(usize::from(character)).cloned().unwrap_or_default();
                if usize::from(character) >= CHARACTERS {
                    self.coverage.anomalies += 1;
                }
                for i in 0..len {
                    self.mem.banks.set(base + i, name.get(i).copied().unwrap_or(0xFF));
                }
                self.mem.banks.set(base + len, 0xFF);
                self.advance(6);
            }
            0xF7 => {
                let speed = 255u8.wrapping_sub(self.party_ext.save.message_speed);
                self.wr8(4, 3, speed);
                self.advance(4);
            }
            0xF8 => {
                self.party_ext.save.message_speed = 255u8.wrapping_sub(self.rd8(4, 3));
                self.advance(4);
            }
            0xF9 => {
                for id in 0..80u32 {
                    if self.party_ext.save.add_materia(id) {
                        self.note_materia(id);
                    }
                }
                self.advance(2);
            }
            0xFA => {
                for id in 0..ITEM_KINDS {
                    self.party_ext.save.add_item(id, MAX_STACK);
                }
                self.advance(2);
            }
            0xFB => {
                self.party_ext.battle_lock = self.p8(2);
                self.advance(3);
            }
            0xFC => {
                self.party_ext.movie_lock = self.p8(2);
                self.advance(3);
            }
            0xFD => {
                let (character, text) = (self.p8(2), self.p8(3));
                self.party_ext.special_names.insert(character, text);
                self.advance(4);
            }
            0xFE => {
                self.party_ext.globals_reset_requested = true;
                self.emit(Event::Presentation { entity: self.current, mnemonic: "RSGLB", bytes: vec![0x0F, 0xFE] });
                self.advance(2);
            }
            0xFF => {
                self.party_ext.save.items.clear();
                self.advance(2);
            }
            _ => {
                self.coverage.anomalies += 1;
                self.advance(2);
            }
        }
        Flow::Continue
    }

    /// Per-frame update after every entity's turn: this family has nothing that advances on its own.
    pub(in crate::runtime) fn update_party(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Banks, Command};
    use crate::script::synth::SynthScript;

    /// A main loop that only waits a frame forever: `WAIT1; JMPB 1`.
    const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

    /// A runtime whose one entity runs `main` (followed by an idle loop) from its first frame.
    fn rt_with(main: &[u8]) -> FieldRuntime {
        let mut code = vec![0x00];
        code.extend_from_slice(main);
        code.extend_from_slice(IDLE);
        let mut s = SynthScript::default().entity("p", vec![(0, code)]);
        s.scale = 512;
        let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
        rt.init();
        rt
    }

    fn set_byte(addr: u8, value: u8) -> Vec<u8> {
        vec![0x80, 0x10, addr, value]
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn word(rt: &FieldRuntime, flat: usize) -> u16 {
        u16::from_le_bytes([rt.mem.banks.get(flat), rt.mem.banks.get(flat + 1)])
    }

    #[test]
    fn items_are_added_removed_counted_and_capped() {
        // STITM id 5 x 60, STITM id 5 x 60 (cap 99), DLITM id 5 x 9, CKITM id 5 -> var 0x20, DLITM id 7 x 3.
        let code = cat(&[
            &[0x58, 0x00, 5, 0, 60],
            &[0x58, 0x00, 5, 0, 60],
            &[0x59, 0x00, 5, 0, 9],
            &[0x5A, 0x01, 5, 0, 0x20],
            &[0x59, 0x00, 7, 0, 3],
        ]);
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert_eq!(rt.save_data().item_count(5), 90);
        assert_eq!(rt.mem.banks.get(0x20), 90);
        assert_eq!(rt.save_data().item_count(7), 0);
        let events = rt.drain_events();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0], Event::Item { item: 5, delta: 60 });
        assert_eq!(events[2], Event::Item { item: 5, delta: -9 });
    }

    #[test]
    fn deleting_the_whole_stack_empties_the_slot_and_ckitm_reads_zero() {
        let code = cat(&[&[0x58, 0x00, 9, 0, 4], &[0x59, 0x00, 9, 0, 10], &[0x5A, 0x01, 9, 0, 0x21]]);
        let mut rt = rt_with(&code);
        rt.mem.banks.set(0x21, 77);
        rt.tick(None);
        assert!(rt.save_data().items.is_empty());
        assert_eq!(rt.mem.banks.get(0x21), 0);
    }

    #[test]
    fn items_take_their_id_and_quantity_from_variables() {
        // STITM with the id from bank 2 (word) at 0x30 and the quantity from bank 1 at 0x32.
        let code = [0x58, 0x12, 0x30, 0x00, 0x32];
        let mut rt = rt_with(&code);
        rt.mem.banks.set(0x30, 0x2A);
        rt.mem.banks.set(0x32, 3);
        rt.tick(None);
        assert_eq!(rt.save_data().item_count(0x2A), 3);
    }

    #[test]
    fn an_id_without_an_inventory_slot_is_an_anomaly() {
        let code = [0x58, 0x00, 0x40, 0x01, 1]; // id 320
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert!(rt.save_data().items.is_empty());
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn materia_are_added_with_their_ap_and_the_fit_flag_is_zero() {
        // SMTRA id 44, AP 0x030201.
        let mut rt = rt_with(&[0x5B, 0x00, 0x00, 44, 1, 2, 3]);
        rt.mem.banks.set(0x300 + 31, 9);
        rt.tick(None);
        assert_eq!(rt.save_data().materia, vec![44 | (0x030201 << 8)]);
        assert_eq!(rt.mem.banks.get(0x300 + 31), 0);
        assert_eq!(rt.mem.banks.get(MATERIA_FLAGS) & 2, 2, "the master-summon-style flag");
        assert!(rt.drain_events().contains(&Event::Materia { materia: 44, ap: 0x030201 }));
    }

    #[test]
    fn a_full_materia_list_reports_one() {
        let mut rt = rt_with(&[0x5B, 0x00, 0x00, 1, 0, 0, 0]);
        rt.set_save_data(PartySaveData { materia: vec![5; MAX_MATERIA], ..Default::default() });
        rt.tick(None);
        assert_eq!(rt.save_data().materia.len(), MAX_MATERIA);
        assert_eq!(rt.mem.banks.get(0x300 + 31), 1);
    }

    #[test]
    fn materia_are_removed_and_counted_by_id_and_ap() {
        let code = cat(&[
            &[0x5B, 0, 0, 7, 10, 0, 0],
            &[0x5B, 0, 0, 7, 10, 0, 0],
            &[0x5B, 0, 0, 7, 11, 0, 0],
            // CMTRA: AP 10 (4 bytes), id 7 -> var 0x40 (bank 6 = scratch, so read it back from scratch)
            &[0x5D, 0, 0, 0x06, 10, 0, 0, 0, 7, 0x40],
            // DMTRA id 7 AP 10 x 1
            &[0x5C, 0, 0, 7, 10, 0, 0, 1],
        ]);
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert_eq!(rt.mem.scratch[0x40], 2);
        assert_eq!(rt.save_data().materia, vec![7 | (10 << 8), 7 | (11 << 8)]);
    }

    #[test]
    fn gil_is_copied_into_two_words() {
        // CHGLD: both halves into bank 2 (word) at 0x40 and 0x42 (bank 1's 16-23 are the engine's clocks).
        let mut rt = rt_with(&[0x3B, 0x22, 0x40, 0x42]);
        rt.gil = Some(0x0001_2345);
        rt.tick(None);
        assert_eq!(word(&rt, 0x40), 0x2345);
        assert_eq!(word(&rt, 0x42), 1);
    }

    #[test]
    fn phs_visibility_locking_and_the_member_query() {
        // MMBud on for 3, IFMEMBQ 3 (member: set 0x30), IFMEMBQ 4 (not: skip the set), MMBLK 3, MMBLK 5, MMBUK 5.
        let code = cat(&[
            &[0xCD, 1, 3],
            &[0xCC, 3, 5],
            &set_byte(0x30, 1),
            &[0xCC, 4, 5],
            &set_byte(0x31, 1),
            &[0xCE, 3],
            &[0xCE, 5],
            &[0xCF, 5],
        ]);
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x30), 1);
        assert_eq!(rt.mem.banks.get(0x31), 0, "4 is not a member, so its branch skipped the set");
        assert_eq!(rt.save_data().phs_visibility, 1 << 3);
        assert_eq!(rt.save_data().phs_locking, 1 << 3);
    }

    #[test]
    fn removing_a_member_takes_them_out_of_the_party() {
        let mut rt = rt_with(&[0xCD, 0, 2]);
        rt.mem.banks.set(PARTY_SLOTS, 0);
        rt.mem.banks.set(PARTY_SLOTS + 1, 2);
        rt.mem.banks.set(PARTY_SLOTS + 2, NO_CHARACTER);
        rt.set_save_data(PartySaveData { phs_visibility: 0b111, ..Default::default() });
        rt.tick(None);
        assert_eq!(rt.party(), [0, NO_CHARACTER, NO_CHARACTER]);
        assert_eq!(rt.save_data().phs_visibility, 0b011);
        assert!(rt.drain_events().contains(&Event::PartyChanged { party: [0, NO_CHARACTER, NO_CHARACTER] }));
    }

    fn vitals_runtime(main: &[u8]) -> FieldRuntime {
        let mut rt = rt_with(main);
        rt.mem.banks.set(PARTY_SLOTS, 0);
        rt.mem.banks.set(PARTY_SLOTS + 1, 2);
        rt.mem.banks.set(PARTY_SLOTS + 2, NO_CHARACTER);
        let mut save = PartySaveData::default();
        save.characters.fill(CharacterVitals { hp: 100, max_hp: 500, mp: 20, max_mp: 80 });
        rt.set_save_data(save);
        rt
    }

    #[test]
    fn hp_and_mp_change_for_the_character_in_the_slot_and_stay_in_range() {
        let code = cat(&[
            &[0x4D, 0x00, 0, 50, 0],  // HPu slot 0 +50 -> 150
            &[0x4F, 0x00, 1, 30, 0],  // HPd slot 1 -30 -> 70 (Tifa)
            &[0x45, 0x00, 0, 100, 0], // MPu slot 0 +100 -> capped 80
            &[0x47, 0x00, 1, 25, 0],  // MPd slot 1 -25 -> 0
            &[0x4D, 0x00, 2, 50, 0],  // an empty slot changes nothing
        ]);
        let mut rt = vitals_runtime(&code);
        rt.tick(None);
        let c = &rt.save_data().characters;
        assert_eq!((c[0].hp, c[0].mp), (150, 80));
        assert_eq!((c[2].hp, c[2].mp), (70, 0));
        assert_eq!((c[1].hp, c[1].mp), (100, 20), "the others are untouched");
    }

    #[test]
    fn hp_down_never_kills_a_living_character_and_the_amount_may_be_a_variable() {
        // HPd slot 0 by the word in bank 2 at 0x50 (= 9999).
        let mut rt = vitals_runtime(&[0x4F, 0x02, 0, 0x50, 0]);
        rt.mem.banks.set(0x50, 0x0F);
        rt.mem.banks.set(0x51, 0x27);
        rt.tick(None);
        assert_eq!(rt.save_data().characters[0].hp, 1);
    }

    #[test]
    fn the_full_restores_fill_the_party_or_everyone() {
        let mut rt = vitals_runtime(&[0x3C, 0x3D, 0x3F]);
        rt.tick(None);
        let c = &rt.save_data().characters;
        assert_eq!((c[0].hp, c[0].mp, c[2].hp), (500, 80, 500));
        assert_eq!(c[1].hp, 100, "not in the party");
        let mut rt = vitals_runtime(&[0x3E]);
        rt.tick(None);
        assert!(rt.save_data().characters.iter().all(|c| c.hp == 500 && c.mp == 80));
    }

    #[test]
    fn the_battle_result_the_previous_field_and_the_modes_move_through_variables() {
        // BTRLD -> bank 2 at 0x60, LSTMP -> bank 2 at 0x62, BTMD2, PMJMP 77.
        let code = cat(&[&[0x23, 0x02, 0x60], &[0x6E, 0x02, 0x62], &[0x22, 1, 0, 2, 0], &[0xD8, 77, 0], &[0xD9]]);
        let mut rt = rt_with(&code);
        rt.set_battle_result(3);
        rt.set_previous_field(0x0123);
        rt.tick(None);
        assert_eq!(word(&rt, 0x60), 3);
        assert_eq!(word(&rt, 0x62), 0x0123);
        assert_eq!((rt.party_state().battle_mode2, rt.party_state().battle_mode1), (1, 2));
        assert_eq!(rt.party_state().preloaded_map, Some(77));
    }

    #[test]
    fn a_tutorial_holds_the_script_until_the_host_completes_it() {
        let code = cat(&[&[0x21, 4], &set_byte(0x70, 1)]);
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert_eq!(rt.pending_tutorial(), Some(4));
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x70), 0);
        rt.complete_tutorial();
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x70), 1);
        assert_eq!(rt.pending_tutorial(), None);
    }

    #[test]
    fn a_tutorial_waits_while_another_command_is_pending() {
        // MENU first (the host has not completed it), then TUTOR.
        let code = cat(&[&[0x49, 0x00, 9, 0], &[0x21, 1]]);
        let mut rt = rt_with(&code);
        rt.tick(None);
        assert!(matches!(rt.command(), Some(Command::Menu { .. })));
        assert_eq!(rt.pending_tutorial(), None);
    }

    #[test]
    fn special_sub_ops_have_their_effects_and_lengths() {
        let mut rt;
        let main = cat(&[
            &[0x0F, 0xF5, 1],                   // ARROW hide
            &[0x0F, 0xFB, 1],                   // BTLCK
            &[0x0F, 0xFC, 1],                   // MVLCK
            &[0x0F, 0xFD, 4, 9],                // SPCNM character 4 -> text 9
            &[0x0F, 0xF8, 0x00, 200],           // SMSPD literal 200 -> stored 55
            &[0x0F, 0xF7, 0x01, 0x80],          // GMSPD -> bank 1 at 0x80 = 255 - 55
            &[0x0F, 0xFA],                      // FLITM
            &[0x0F, 0xF9],                      // FLMAT
            &set_byte(0x71, 1),
        ]);
        rt = rt_with(&main);
        rt.tick(None);
        rt.tick(None); // an entity runs eight instructions a frame
        assert_eq!(rt.mem.banks.get(0x71), 1, "every sub-op advanced by its own length");
        let st = rt.party_state();
        assert_eq!((st.arrow_disabled, st.battle_lock, st.movie_lock), (1, 1, 1));
        assert_eq!(st.special_names.get(&4), Some(&9));
        assert_eq!(st.save.message_speed, 55);
        assert_eq!(rt.mem.banks.get(0x80), 200);
        assert_eq!(st.save.items.len(), usize::from(ITEM_KINDS));
        assert!(st.save.items.values().all(|q| *q == MAX_STACK));
        assert_eq!(st.save.materia, (0..80).collect::<Vec<u32>>());
        // CLITM empties it again.
        let mut rt = rt_with(&cat(&[&[0x0F, 0xFA], &[0x0F, 0xFF], &[0x0F, 0xFE], &set_byte(0x72, 1)]));
        rt.tick(None);
        assert!(rt.save_data().items.is_empty());
        assert!(rt.party_state().globals_reset_requested, "RSGLB asks the host; it never wipes a save itself");
        assert_eq!(rt.mem.banks.get(0x72), 1);
    }

    #[test]
    fn pname_copies_the_character_name_into_the_chosen_bank() {
        // PNAME: selector nibbles 1/3 (high nibble: character from bank 0 = literal; low: bank 3 -> +256), char 2,
        // unused, length 3.
        let mut rt = rt_with(&[0x0F, 0xF6, 0x03, 2, 0, 3]);
        rt.set_character_name(2, vec![0x11, 0x22, 0x33, 0x44]);
        rt.tick(None);
        assert_eq!(&rt.mem.banks.0[0x100..0x104], &[0x11, 0x22, 0x33, 0xFF]);
    }

    #[test]
    fn an_unknown_special_sub_op_is_an_anomaly_that_advances_two_bytes() {
        let mut rt = rt_with(&cat(&[&[0x0F, 0x01], &set_byte(0x73, 1)]));
        rt.tick(None);
        assert_eq!(rt.coverage().anomalies, 1);
        assert_eq!(rt.mem.banks.get(0x73), 1);
    }

    #[test]
    fn each_op_advances_by_its_decoded_length() {
        // Every non-branching, non-blocking op of the family, with zero operands, moves the PC by the length the
        // decoder gives (the two SPECIAL sub-ops whose lengths the decompilation fixes are checked above).
        for op in (0u8..=255).filter(|op| implemented(*op) && !matches!(op, 0x0F | 0x21 | 0xCC)) {
            let len = crate::script::decode_at(&[op, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, 0).map(|i| i.len).expect("decodes");
            let mut rt = rt_with(&[&[op][..], &[0; 11]].concat());
            let before = rt.entity_state(0).expect("entity").1;
            assert_eq!(rt.step(0), Flow::Continue, "op {op:#04x}");
            assert_eq!(rt.entity_state(0).expect("entity").1 - before, len, "op {op:#04x}");
        }
    }

    #[test]
    fn the_member_query_branches_both_ways_by_the_decoded_length() {
        let mut rt = rt_with(&[0xCC, 3, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let before = rt.entity_state(0).expect("entity").1;
        rt.step(0);
        assert_eq!(rt.entity_state(0).expect("entity").1 - before, 11, "not a member: jump + 2");
        let mut rt = rt_with(&[0xCC, 3, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        rt.set_save_data(PartySaveData { phs_visibility: 1 << 3, ..Default::default() });
        let before = rt.entity_state(0).expect("entity").1;
        rt.step(0);
        assert_eq!(rt.entity_state(0).expect("entity").1 - before, 3, "a member: fall through");
    }
}
