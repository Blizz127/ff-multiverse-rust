//! Reviewed unlocked-member state for the opening route, separate from the active party.
//!
//! Original `SysSavemapReset` starts `phs_visibility_mask` at 1. Every nonempty
//! `ADD_PARTY_MEMBER` assignment ORs its member into that mask; leaving the party never
//! removes a bit. The reactor door therefore changes 1 to 3, and the station's removal
//! of Barret leaves 3. Source: pinned PSX decomp d4db3de3c3aaf61269e048cf3f6a458996290de0,
//! `main/cd.c` 39–58 and `field/field_opcodes.c` 17–28, 5515–5748.
//!
//! This is an explicit projection for the audited route through moment 63. It is not an
//! imported-save PHS reader or a general persistent PHS resource. Later chapters and any
//! other visibility mutations need that resource before authoritative execution can accept
//! them. These checks neither initialize unknown party mirror bytes nor write field banks.

use super::{canon_party, PartyOp, StoryState};

/// The unlocked members proven for the reviewed opening route, even when one has left
/// the active party. Beyond the reviewed route there is no inferred value.
pub fn reviewed_party_visibility(state: &StoryState) -> Option<u16> {
    match state.moment {
        0..=10 => Some(1),
        11..=63 => Some(3),
        _ => None,
    }
}

/// Whether the actual authored assignments preserve the reviewed unlocked-member state.
/// Exact interpreters separately validate field/main tuple inputs and reject unsupported
/// divergence before supplying this journal. A member added and removed within one step
/// still remains unlocked, so comparing the final active party alone is insufficient.
pub fn party_visibility_matches(before: &StoryState, after: &StoryState, ops: &[PartyOp]) -> bool {
    let Some(mut visible) = reviewed_party_visibility(before) else { return false };
    let Some(expected) = reviewed_party_visibility(after) else { return false };
    let mut bank = [255; 3];
    for (slot, member) in bank.iter_mut().zip(canon_party(before)) { *slot = member; }
    let mut main = bank;
    for &op in ops {
        let Ok(change) = crate::script::party::transition(bank, main, op) else { return false };
        for (member, assigned) in change.bank.into_iter().zip(change.assigned) {
            if assigned && member <= 8 { visible |= 1u16 << member; }
        }
        bank = change.bank;
        main = change.main;
    }
    visible == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_door_unlocks_barret_and_leaving_the_party_does_not_lock_him() {
        let before = StoryState::at(10).into_extended().unwrap();
        let unchanged = before.encode();
        assert_eq!(reviewed_party_visibility(&before), Some(1));
        assert!(party_visibility_matches(&before, &StoryState::at(11), &[PartyOp::Set([0, 1, 254])]));
        assert!(!party_visibility_matches(&before, &StoryState::at(11), &[]), "the actual assignment is required");
        assert!(party_visibility_matches(&StoryState::at(33), &StoryState::at(36), &[PartyOp::Remove(1)]));
        assert_eq!(reviewed_party_visibility(&StoryState::at(36)), Some(3));
        assert_eq!(before.encode(), unchanged, "a projection never fills unknown field bytes");
        assert_eq!(before.known_mask(3, 9), 0);
    }

    #[test]
    fn temporary_unsupported_unlocks_cannot_disappear_with_the_final_active_party() {
        let story = StoryState::at(36);
        let ops = [PartyOp::Add(2), PartyOp::Remove(2)];
        assert_eq!(super::super::party_after(&canon_party(&story), &ops), canon_party(&story));
        assert!(!party_visibility_matches(&story, &story, &ops));
        assert!(!party_visibility_matches(&story, &story, &[PartyOp::Set([0, 9, 254])]));
        assert!(party_visibility_matches(&story, &story, &[PartyOp::Remove(1)]));
    }

    #[test]
    fn no_visibility_is_invented_beyond_the_reviewed_route() {
        let last = StoryState::at(63);
        let later = StoryState::at(64);
        assert!(party_visibility_matches(&last, &last, &[]));
        assert_eq!(reviewed_party_visibility(&later), None);
        assert!(!party_visibility_matches(&last, &later, &[]));
        assert!(!party_visibility_matches(&later, &last, &[]));
    }
}
