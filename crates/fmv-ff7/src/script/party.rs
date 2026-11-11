//! Original field-party assignments, separate from the main party read by GETPC.
//!
//! Pinned PSX `field_opcodes.c`: PRTYP/PRTYM at 5515/5560, PartyReplace at 5636,
//! and PartyFromBank2ToSave at 5710. Physical memory_bank_2[9..11] is script bank3/4.
//! The caller decides whether its durable party model can represent the resulting main slots.

use crate::story::PartyOp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PartyChange {
    pub bank: [u8; 3],
    pub main: [u8; 3],
    /// Actual assignments, including assignments that leave a byte unchanged.
    pub assigned: [bool; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartyError {
    InvalidSlots,
    InvalidCharacter,
}

fn valid(slots: &[u8; 3]) -> bool {
    let mut seen = 0u16;
    slots.iter().all(|&member| {
        if member == 255 { return true; }
        if member > 8 || seen & (1 << member) != 0 { return false; }
        seen |= 1 << member;
        true
    })
}

pub(crate) fn transition(bank: [u8; 3], main: [u8; 3], op: PartyOp) -> Result<PartyChange, PartyError> {
    if !valid(&main) { return Err(PartyError::InvalidSlots); }
    // With no preserve-slot operand, PartyReplace's final three assignments are independent
    // of the old field tuple. An authoritative overwrite does not need invented input bytes.
    let replaces_all = matches!(op, PartyOp::Set(slots) if !slots.contains(&255));
    if !replaces_all && !valid(&bank) { return Err(PartyError::InvalidSlots); }
    let change = native_transition(bank, main, op)?;
    // The legacy wrapper retains its unique raw-tuple acceptance rule. Native writable raw
    // bytes can temporarily disagree with it; the v4 evaluator validates its final main image.
    if !valid(&change.bank) { return Err(PartyError::InvalidSlots); }
    Ok(PartyChange { bank: change.bank, main: change.main, assigned: change.raw_assigned })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePartyChange {
    pub bank: [u8; 3],
    pub main: [u8; 3],
    pub raw_assigned: [bool; 3],
    pub main_assigned: [bool; 3],
}

/// Native tuple mechanics without normalizing the raw preimage. Callers audit knowledge
/// before entering and validate the resulting main tuple against their supported domain.
/// In particular PartyReplace removes every occurrence of a requested member, so it can
/// repair raw duplicates (pinned field_opcodes.c:5636–5673). The legacy wrapper stays strict.
pub(crate) fn native_transition(mut bank: [u8; 3], main: [u8; 3], op: PartyOp) -> Result<NativePartyChange, PartyError> {
    let replaces_all = matches!(op, PartyOp::Set(slots) if !slots.contains(&255));
    let mut assigned = [false; 3];
    match op {
        PartyOp::Add(character) => {
            if character > 8 { return Err(PartyError::InvalidCharacter); }
            if !bank.contains(&character) {
                let index = bank.iter().position(|&member| member == 255).unwrap_or(2);
                if let (Some(member), Some(written)) = (bank.get_mut(index), assigned.get_mut(index)) {
                    *member = character;
                    *written = true;
                }
            }
        }
        PartyOp::Remove(character) => {
            if character > 8 { return Err(PartyError::InvalidCharacter); }
            if let Some(index) = bank.iter().position(|&member| member == character) {
                if let (Some(member), Some(written)) = (bank.get_mut(index), assigned.get_mut(index)) {
                    *member = 255;
                    *written = true;
                }
            }
        }
        PartyOp::Set(mut requested) => {
            if requested.iter().any(|&member| member > 8 && member != 254 && member != 255) {
                return Err(PartyError::InvalidCharacter);
            }
            if !replaces_all {
                // Requested members are removed from the old tuple before preserved members
                // fill 0xFF slots. 0xFE remains occupied until the final forced-empty conversion.
                for member in &mut bank {
                    if requested.iter().any(|&wanted| wanted != 255 && wanted == *member) { *member = 255; }
                }
                for member in bank.into_iter().filter(|&member| member != 255) {
                    if let Some(slot) = requested.iter_mut().find(|slot| **slot == 255) { *slot = member; }
                }
            }
            bank = requested.map(|member| if member == 254 { 255 } else { member });
            assigned = [true; 3];
        }
    }
    // Duplicate PRTYP and absent PRTYM still call PartyFromBank2ToSave. Its padded 255
    // arrays can assign unchanged empty main slots too (field_opcodes.c:5527–5531,5579).
    let (main, main_assigned) = reconcile_slots(main, bank);
    Ok(NativePartyChange { bank, main, raw_assigned: assigned, main_assigned })
}

/// Native FIELD_Init after a menu: reconcile physical field slots with the main party while
/// preserving retained field-slot order. This does not use ADD_PARTY_MEMBER or change PHS bits.
/// PartyCompare's unused result entries are 255 too: PartyRemove/PartyAdd really assign those
/// empty markers, even when the value already is 255, so the assignment mask includes them.
pub(crate) fn sync_from_main(bank: [u8; 3], main: [u8; 3]) -> Result<PartyChange, PartyError> {
    if !valid(&bank) || !valid(&main) { return Err(PartyError::InvalidSlots); }
    let (bank, assigned) = reconcile_slots(bank, main);
    Ok(PartyChange { bank, main, assigned })
}

/// Literal PartyCompare → PartyRemove → PartyAdd (field_opcodes.c:5677–5754), shared by
/// main-from-raw and FIELD_Init's raw-from-main direction. Retained destination positions
/// survive; all padded 255 entries are executed and their assignments are reported.
pub(crate) fn reconcile_slots(mut destination: [u8; 3], source: [u8; 3]) -> ([u8; 3], [bool; 3]) {
    let mut remove = [255; 3];
    let mut add = [255; 3];
    for (out, member) in remove.iter_mut().zip(destination.into_iter().filter(|member| !source.contains(member))) { *out = member; }
    for (out, member) in add.iter_mut().zip(source.into_iter().filter(|member| !destination.contains(member))) { *out = member; }
    let mut assigned = [false; 3];
    for member in remove {
        for (slot, written) in destination.iter_mut().zip(assigned.iter_mut()) {
            if *slot == member { *slot = 255; *written = true; }
        }
    }
    for member in add {
        if let Some((slot, written)) = destination.iter_mut().zip(assigned.iter_mut()).find(|(slot, _)| **slot == 255) {
            *slot = member;
            *written = true;
        }
    }
    (destination, assigned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_preserves_remaining_members_for_255_but_forces_254_empty() {
        let old = [0, 1, 2];
        let held = transition(old, old, PartyOp::Set([255, 0, 254])).unwrap();
        assert_eq!(held, PartyChange { bank: [1, 0, 255], main: [0, 1, 255], assigned: [true; 3] });
        assert_eq!(transition(old, old, PartyOp::Set([255; 3])).unwrap().bank, old);
        assert_eq!(transition(old, old, PartyOp::Set([254; 3])).unwrap().bank, [255; 3]);
    }

    #[test]
    fn main_slots_keep_holes_and_surviving_members_in_place() {
        let removed = transition([0, 1, 2], [0, 1, 2], PartyOp::Remove(0)).unwrap();
        assert_eq!(removed, PartyChange { bank: [255, 1, 2], main: [255, 1, 2], assigned: [true, false, false] });
        let added = transition(removed.bank, removed.main, PartyOp::Add(3)).unwrap();
        assert_eq!((added.bank, added.main), ([3, 1, 2], [3, 1, 2]));
        let reordered = transition([0, 1, 2], [0, 1, 2], PartyOp::Set([2, 1, 0])).unwrap();
        assert_eq!((reordered.bank, reordered.main), ([2, 1, 0], [0, 1, 2]));
    }

    #[test]
    fn actual_assignment_masks_distinguish_no_ops_from_full_assignments() {
        let old = [0, 1, 255];
        for op in [PartyOp::Add(1), PartyOp::Remove(2)] {
            assert_eq!(transition(old, old, op).unwrap().assigned, [false; 3]);
        }
        assert_eq!(transition(old, old, PartyOp::Set([0, 1, 254])).unwrap().assigned, [true; 3]);
        assert_eq!(transition([0, 1, 2], [0, 1, 2], PartyOp::Add(3)).unwrap(),
            PartyChange { bank: [0, 1, 3], main: [0, 1, 3], assigned: [false, false, true] });
    }

    #[test]
    fn fixed_overwrite_does_not_need_old_field_bytes_but_dependent_ops_do() {
        let unknown = [0, 0, 0];
        assert_eq!(transition(unknown, [0, 255, 255], PartyOp::Set([0, 1, 254])).unwrap().bank, [0, 1, 255]);
        for op in [PartyOp::Set([0, 255, 254]), PartyOp::Add(1), PartyOp::Remove(1)] {
            assert_eq!(transition(unknown, [0, 255, 255], op), Err(PartyError::InvalidSlots));
        }
        assert_eq!(transition([0, 1, 255], [0, 1, 255], PartyOp::Set([0, 0, 254])), Err(PartyError::InvalidSlots));
    }

    #[test]
    fn menu_return_preserves_field_order_and_records_native_empty_slot_assignments() {
        for (bank, main, expected, assigned) in [
            ([1, 0, 255], [0, 1, 255], [1, 0, 255], [false, false, true]),
            ([0, 2, 255], [0, 1, 255], [0, 1, 255], [false, true, true]),
            ([255, 1, 255], [0, 1, 255], [0, 1, 255], [true, false, true]),
            ([0, 1, 2], [0, 1, 2], [0, 1, 2], [false; 3]),
        ] {
            let change = sync_from_main(bank, main).unwrap();
            assert_eq!(change, PartyChange { bank: expected, main, assigned });
            let again = sync_from_main(change.bank, main).unwrap();
            assert_eq!(again.bank, change.bank);
            assert!(again.assigned.into_iter().zip(assigned).all(|(next, first)| !next || first), "repeated native returns add no new assigned byte");
        }
        assert_eq!(sync_from_main([0, 0, 0], [0, 255, 255]), Err(PartyError::InvalidSlots));
    }

}
