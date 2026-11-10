//! Explicit initialization of newly created FF7 state. These constructors never inspect a
//! prior save or infer initialization from its moment. Legacy decoding and promotion remain
//! separate: a missing byte in an existing account is still unknown.
//!
//! The reset image is positively established by the original PSX US `SysSavemapReset`,
//! pinned at `d4db3de3c3aaf61269e048cf3f6a458996290de0`:
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/cd.c#L39-L68>.
//! It clears the contiguous five 256-byte field banks, then sets the exceptions below.
//! This establishes the instant after reset, not the later first-control state: the new-game
//! entry, field initialization, and opening scripts still run afterward.
//!
//! [`StoryState::fresh_first_control`] is the port's explicit reset/opening profile, not a
//! captured original-engine snapshot or a claim of arbitrary-byte PC equivalence. After
//! reset, the pinned engine calls an unavailable LIMTMENU routine (`func_801D11A8`):
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/1F6B4.c#L920-L927>.
//! Its write set is unknown. The profile deliberately adopts the proven reset defaults,
//! overlays positively identified new-game/opening assignments, and withholds every
//! identified engine-owned field whose writer this port does not maintain. That inventory
//! is not proof that the opaque routine leaves every other byte unchanged.
//!
//! The owned original `md1stin` director sets byte bank 1 offsets 3..6 to 50/30/10/0,
//! offsets 80..83 to 100, menu visibility word 28 to 763, then moment 1 immediately before
//! UC0/MENU2=0/CC1 hand control to the player (script offsets 1016, 1027..1055, 1176..1185).
//! Its later moment 6 write belongs to the first battle trigger and is not initialization.
//! The same director's `PRTYE(0,254,254)` at offset 1021 (`0x03FD`) explicitly overwrites
//! physical party bytes 3:9..11 with `[0,255,255]` before first control. Native PartyReplace
//! converts forced-empty 254 to 255 after its old-member merge, then assigns every slot:
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L5636-L5673>.
//! There are no other party mutations or direct bank 3/4 operands in the owned opening
//! field. This later authored overwrite establishes these bytes despite the earlier opaque
//! startup call; it does not establish arbitrary main-party slot ordering or repair old saves.
//! `SysInitNewGame` separately sets physical bank 4 byte 0 (script bank 13) to 1:
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/110B8.c#L263-L274>.
//! No gil, inventory, character, or party resource record is synthesized here.

use super::{banks::KnownByte, StoryState};
use std::collections::BTreeMap;

/// Canonical selectors for the five physical 256-byte persistent banks. Physical bank 5
/// uses byte selector 15 / word selector 7; the stored representation canonicalizes it to 7.
const BANKS: [u8; 5] = [1, 3, 7, 11, 13];

/// Nonzero bytes assigned after the original contiguous clear. The party's first slot is
/// explicitly put back to Cloud (zero); its other two slots remain FF. The location-name
/// buffer begins with an FF terminator, and both menu visibility bytes are FF.
const RESET_EXCEPTIONS: [(u8, u8, u8); 5] = [
    (1, 28, 0xFF), (1, 29, 0xFF),
    (3, 10, 0xFF), (3, 11, 0xFF),
    (13, 104, 0xFF),
];

/// Positive new-game and original opening assignments; moment 1 is held in the header.
const FIRST_CONTROL_BYTES: [(u8, u8, u8); 14] = [
    (13, 0, 1),
    (1, 28, 0xFB), (1, 29, 0x02),
    (1, 3, 50), (1, 4, 30), (1, 5, 10), (1, 6, 0),
    (1, 80, 100), (1, 81, 100), (1, 82, 100), (1, 83, 100),
    (3, 9, 0), (3, 10, 255), (3, 11, 255), // PRTYE(0,254,254), not an inferred party table
];

/// Canonical bank, inclusive byte range, unknown bits. Even when reset establishes an
/// initial value, durable knowledge would become stale without the corresponding runtime
/// writer. Ordinary v3 CAS intentionally cannot forget established knowledge later. Proven
/// opening assignments below may reestablish bytes after this general engine-image mask.
///
/// Sources below refer to the same pinned PSX repository as the module documentation.
const UNSUPPORTED_ENGINE_FIELDS: [(u8, u8, u8, u8); 16] = [
    (1, 16, 23, 0xFF), // elapsed/countdown mirrors: field/field_event.c:303-337
    (1, 24, 25, 0xFF), // battle count: main/110B8.c:337-339
    (1, 26, 27, 0xFF), // battle-exit mode 8 count: main/110B8.c:359-361,388-390
    (1, 31, 31, 0x03), // PHS/save menu locks: field/field_event.c:240
    (1, 75, 75, 0x03), // materia-acquisition flags: main/1F6B4.c:654-693
    (1, 122, 122, 0x80), // derived equipment/materia flag: main/17238.c:187-209
    (3, 9, 11, 0xFF), // current party IDs: field/field_opcodes.c:5636-5674,5722-5728
    (3, 98, 99, 0xFF), // Jet minigame result: main/110B8.c:577-578
    (3, 196, 203, 0xFF), // MENU 0x18 party-level snapshot: main/1F6B4.c:609-621
    (7, 111, 111, 0xFF), // Master Materia result (alias 15): menu/bginmenu.c:208,239
    (13, 6, 7, 0xFF), // walking steps: field/field_battle.c:44-59
    (13, 30, 30, 0x01), // input-controlled position cursor: field/field_event.c:612-618
    (13, 31, 31, 0xFF), // SMTRA success result: field/field_opcodes.c:6594-6601
    (13, 94, 95, 0xFF), // submarine minigame result: main/110B8.c:588-590
    (13, 96, 96, 0x01), // Save Crystal used: menu/itemmenu.c:74,1020
    (13, 104, 128, 0xFF), // MPNAM's expanded location-name buffer: field/field_dialog.c:1061-1157
];

impl StoryState {
    /// The exact persistent-bank reset image, at moment zero, for an explicit fresh-game
    /// bootstrap. Every bit is known because `SysSavemapReset` writes every byte, including
    /// zeros. This is not a first-control save: later initialization may replace values or
    /// require unsupported engine-owned fields to remain unknown.
    ///
    /// Never use this constructor to fill gaps in an existing or imported account. Neither
    /// [`StoryState::decode`] nor [`StoryState::into_extended`] calls it.
    pub fn fresh_reset() -> Self {
        let mut banks = BTreeMap::new();
        for bank in BANKS {
            for address in 0..=u8::MAX {
                if bank == 1 && address < 2 { continue; } // moment is carried in the header
                banks.insert((bank, address), KnownByte { value: 0, mask: u8::MAX });
            }
        }
        for (bank, address, value) in RESET_EXCEPTIONS {
            banks.insert((bank, address), KnownByte { value, mask: u8::MAX });
        }
        Self { moment: 0, vars: BTreeMap::new(), banks: Some(banks) }
    }

    /// The port's explicit fresh-account reset/opening profile at the first controllable
    /// moment (1). This constructor is only for creating a genuinely new account; it must
    /// never fill missing bytes in existing, empty-record, or imported saves.
    ///
    /// The original reset supplies defaults, and proven new-game/owned opening assignments
    /// supply the overrides. Unsupported engine-owned fields remain unknown from creation:
    /// clocks and battle counters, menu/derived flags, movement and minigame results, and the
    /// encoded location-name buffer. The opening's explicit PRTYE establishes all three raw
    /// party bytes after the general engine-image mask. Partial masks retain unrelated reset
    /// bits. This avoids storing stale authority or splitting co-op phases on those mirrors.
    ///
    /// This is not verified arbitrary-byte PC first-control equivalence: the pinned PSX
    /// startup invokes an opaque LIMTMENU routine whose write set is unavailable. The port
    /// deliberately adopts the reset/opening profile rather than claiming that routine was
    /// reproduced. Existing save decoding and promotion do not invoke this constructor.
    pub fn fresh_first_control() -> Self {
        let mut state = Self::fresh_reset();
        state.moment = super::NEW_GAME_MOMENT;
        if let Some(banks) = &mut state.banks {
            for (bank, first, last, unknown) in UNSUPPORTED_ENGINE_FIELDS {
                for address in first..=last {
                    let key = (bank, address);
                    let Some(mut byte) = banks.get(&key).copied() else { continue };
                    byte.mask &= !unknown;
                    byte.value &= byte.mask;
                    if byte.mask == 0 { banks.remove(&key); } else { banks.insert(key, byte); }
                }
            }
            // These are positive authored assignments, applied after uncertain engine-owned
            // defaults are masked. In particular PRTYE establishes the complete raw party.
            for (bank, address, value) in FIRST_CONTROL_BYTES {
                banks.insert((bank, address), KnownByte { value, mask: u8::MAX });
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story::{BLOB_VERSION_BANKS, MAX_BANK_BLOB_BYTES};

    #[test]
    fn reset_positively_initializes_every_persistent_bit_and_all_documented_exceptions() {
        let reset = StoryState::fresh_reset();
        assert!(reset.supports_extended());
        assert_eq!(reset.moment, 0);
        for (byte, word) in [(1, 2), (3, 4), (15, 7), (11, 12), (13, 14)] {
            for address in 0..=u8::MAX {
                let bank = super::super::canonical_persistent_bank(byte).unwrap();
                let expected = RESET_EXCEPTIONS.iter().find_map(|&(b, a, v)| (b == bank && a == address).then_some(v)).unwrap_or(0);
                assert_eq!(reset.known_byte(byte, address), Some(expected), "{byte}:{address}");
                assert_eq!(reset.known_byte(word, address), Some(expected), "alias {word}:{address}");
                assert_eq!(reset.known_mask(word, address), 0xFF);
            }
        }
        for (bank, address) in [(1, 28), (1, 29), (3, 10), (3, 11), (13, 104)] {
            assert_eq!(reset.known_byte(bank, address), Some(0xFF), "the source explicitly assigns this reset exception");
        }
        assert_eq!(reset.known_byte(3, 9), Some(0), "the reset restores Cloud to party slot zero");
        assert_eq!(reset.known_byte(13, 0), Some(0), "SysInitNewGame's later disc assignment is not part of reset");
        assert_eq!(reset.known_mask(5, 9), 0, "scratch variables are not persistent reset payload");
    }

    #[test]
    fn full_known_reset_payload_round_trips_at_the_declared_v3_bound() {
        let reset = StoryState::fresh_reset();
        let encoded = reset.encode();
        assert_eq!(encoded.first(), Some(&BLOB_VERSION_BANKS));
        assert_eq!(encoded.len(), MAX_BANK_BLOB_BYTES);
        assert_eq!(StoryState::decode(&encoded), Some(reset));
    }

    #[test]
    fn reset_is_never_inferred_when_reading_or_promoting_an_existing_save() {
        for blob in [vec![], vec![1, 0, 0], vec![1, 1, 0], vec![1, 51, 0]] {
            let old = StoryState::decode(&blob).unwrap();
            let promoted = old.into_extended().unwrap();
            assert_eq!(promoted.known_byte(3, 223), None);
            assert_eq!(promoted.known_byte(1, 4), None);
            assert_eq!(promoted.known_byte(13, 104), None);
            for address in 9..=11 { assert_eq!(promoted.known_mask(3, address), 0); }
            assert_ne!(promoted, StoryState::fresh_reset());
        }
    }

    #[test]
    fn first_control_profile_applies_authored_values_without_advancing_into_the_first_battle() {
        let state = StoryState::fresh_first_control();
        assert!(state.supports_extended());
        assert_eq!(state.moment, 1);
        for (address, expected) in [(0, 1), (1, 0), (3, 50), (4, 30), (5, 10), (6, 0),
            (28, 251), (29, 2), (80, 100), (81, 100), (82, 100), (83, 100)] {
            assert_eq!(state.known_byte(1, address), Some(expected));
            assert_eq!(state.known_byte(2, address), Some(expected), "overlapping word-bank byte alias");
        }
        assert_eq!(state.known_byte(14, 0), Some(1), "the new-game disc assignment survives its alias");
        assert_eq!(state.known_byte(3, 223), Some(0), "ordinary story flags receive the explicit fresh reset default");
        assert_eq!(state.known_byte(11, 255), Some(0), "initialization covers the general persistent image");
        assert_eq!(state.known_byte(15, 32), Some(0), "a later guard-talk item write is not an opening default");
        let blob = state.encode();
        assert!(blob.len() < MAX_BANK_BLOB_BYTES);
        assert_eq!(StoryState::decode(&blob), Some(state));
    }

    #[test]
    fn unsupported_engine_fields_are_unknown_from_creation_without_erasing_unrelated_bits() {
        let state = StoryState::fresh_first_control();
        for (bank, first, last) in [(1, 16, 27), (3, 98, 99), (3, 196, 203),
            (15, 111, 111), (13, 6, 7), (13, 31, 31), (13, 94, 95), (13, 104, 128)] {
            for address in first..=last {
                assert_eq!(state.known_mask(bank, address), 0, "unsupported engine authority at {bank}:{address}");
                assert_eq!(state.known_byte(bank, address), None);
            }
        }
        for (bank, address, known) in [(1, 31, 0xFC), (1, 75, 0xFC), (1, 122, 0x7F),
            (13, 30, 0xFE), (13, 96, 0xFE)] {
            assert_eq!(state.known_mask(bank, address), known);
            assert_eq!(state.var(bank, address), 0, "unknown bits have no serialized value");
            assert_eq!(state.known_byte(bank, address), None, "a partial byte cannot satisfy a numeric read");
        }
        assert_eq!(state.known_bit(1, 31, 0), None, "field menu locks are not modeled by the port");
        assert_eq!(state.known_bit(1, 31, 2), Some(false), "unrelated reset bits remain known");
        assert_eq!(state.known_bit(13, 30, 0), None, "Select may have toggled this bit during the opening");
        assert_eq!(state.known_byte(13, 129), Some(0), "the full MPNAM output ends at 128");
        assert_eq!(state.known_byte(3, 222), Some(0));
        assert_eq!(state.known_byte(3, 224), Some(0));
    }

    #[test]
    fn opening_party_assignment_establishes_raw_slots_after_the_engine_image_mask() {
        assert!(UNSUPPORTED_ENGINE_FIELDS.contains(&(3, 9, 11, 255)), "the general engine mask is retained");
        let fresh = StoryState::fresh_first_control();
        for bank in [3, 4] {
            for (address, expected) in [(9, 0), (10, 255), (11, 255)] {
                assert_eq!(fresh.known_byte(bank, address), Some(expected));
                assert_eq!(fresh.known_mask(bank, address), 255);
            }
        }
        assert_eq!(fresh.moment, 1);
        assert_eq!(StoryState::decode(&fresh.encode()), Some(fresh));
    }

    #[test]
    fn fresh_first_control_is_a_new_profile_not_a_legacy_upgrade_or_a_reset_snapshot() {
        let fresh = StoryState::fresh_first_control();
        let old = StoryState::new_game();
        assert_eq!(old.encode(), [1, 1, 0]);
        assert_eq!(StoryState::decode(&[]), Some(old.clone()));
        assert_eq!(old.clone().into_extended().unwrap().known_byte(1, 4), None);
        assert_ne!(old.into_extended().unwrap(), fresh);
        assert_ne!(StoryState::fresh_reset(), fresh);
    }
}
