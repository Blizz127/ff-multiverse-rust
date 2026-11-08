//! The random-encounter table of a PC field (section 6 of the field file).
//!
//! [`decode_encounter_section`] turns the section into the adapter-api's [`EncounterTable`];
//! [`crate::Ff7FieldArea`] decodes it once when a field is loaded and hands it out through
//! `AreaRuntime::encounters`. (It used to live in `fmv-server`, which attached the table to the
//! areas it loaded; the move changed nothing in the decoding.)
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08**: the layout below, taken from the
//! community documentation, was CHECKED against the owner's copy by the server's real-data test
//! `every_field_encounter_table_decodes_and_names_real_formations` (every table decodes and names
//! real battle formations), and `tests/area_real.rs` repeats the decoding through the area for all
//! 702 fields: section 6 is 48 bytes in every one of them, 182 fields have a random-encounter table
//! (714 entries) and the weights of every one of those tables add up to 64. Other editions are
//! unverified.
//!
//! # Layout
//!
//! Two tables of 24 bytes (the section is 48 bytes in every owned field). Each is
//!
//! ```text
//! u8 enabled | u8 rate | 6 x u16le standard entries | 4 x u16le "special" entries | padding
//! ```
//!
//! A standard entry holds the formation id in its low 10 bits (`scene * 4 + slot`) and the weight
//! in the high 6 bits; the weights of a used table add up to 64. The special entries (back attacks
//! and the like) and the padding are not decoded (APPROXIMATION: a battle is never a special one).
//! The first table that is enabled and has a usable entry is the area's; `None` when neither is
//! (no random battles there).

use fmv_adapter_api::{EncounterEntry, EncounterTable};

/// Bytes of one encounter table in a PC field's encounter section (the section holds two).
const ENCOUNTER_TABLE_LEN: usize = 24;
/// Standard encounters per table.
const ENCOUNTER_SLOTS: usize = 6;

/// Decode the random-encounter table of an FF7 PC field from its encounter section (index 6, see
/// [`crate::field::section::ENCOUNTER`]).
///
/// Layout: see the module docs. The first table that is enabled, has a non-zero rate and at least
/// one entry with a non-zero weight is the area's; a section that is too short for a table, or in
/// which no table qualifies, decodes to `None`. Trailing bytes after the tables are ignored.
/// Never panics, whatever the bytes are.
// `as_chunks` would satisfy this lint but needs Rust 1.88; wgpu 30 (a workspace dependency) only
// needs 1.87, so keep `chunks_exact` rather than raise the MSRV (see `lgp` for the same choice).
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn decode_encounter_section(bytes: &[u8]) -> Option<EncounterTable> {
    bytes.chunks_exact(ENCOUNTER_TABLE_LEN).find_map(|table| {
        let (enabled, rate) = (*table.first()?, *table.get(1)?);
        if enabled == 0 || rate == 0 {
            return None;
        }
        let entries: Vec<EncounterEntry> = table
            .get(2..2 + 2 * ENCOUNTER_SLOTS)?
            .chunks_exact(2)
            .filter_map(|pair| {
                let raw = u16::from_le_bytes([*pair.first()?, *pair.get(1)?]);
                let (formation, weight) = (raw & 0x03FF, raw >> 10);
                (weight > 0).then_some(EncounterEntry { formation, weight })
            })
            .collect();
        let table = EncounterTable { rate, entries };
        (!table.is_empty()).then_some(table)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 24-byte table: enabled flag, rate, up to six `(formation, weight)` standard entries.
    fn table_bytes(enabled: u8, rate: u8, entries: &[(u16, u16)]) -> Vec<u8> {
        let mut t = vec![enabled, rate];
        for slot in 0..ENCOUNTER_SLOTS {
            let raw = entries.get(slot).map_or(0, |&(formation, weight)| {
                assert!(formation < 1024 && weight < 64, "a formation is 10 bits and a weight 6 bits");
                (weight << 10) | formation
            });
            t.extend_from_slice(&raw.to_le_bytes());
        }
        t.resize(ENCOUNTER_TABLE_LEN, 0);
        t
    }

    #[test]
    fn an_enabled_table_decodes_to_formations_and_weights() {
        let mut section = table_bytes(1, 128, &[(301, 24), (302, 22), (303, 18)]);
        section.extend(table_bytes(0, 0, &[]));
        let t = decode_encounter_section(&section).expect("a table");
        assert_eq!(t.rate, 128);
        assert_eq!(
            t.entries,
            vec![
                EncounterEntry { formation: 301, weight: 24 },
                EncounterEntry { formation: 302, weight: 22 },
                EncounterEntry { formation: 303, weight: 18 },
            ]
        );
        assert_eq!(t.total_weight(), 64, "the weights of a real table add up to 64");
        // The low 10 bits are the formation: the largest id fits, the weight is only the high 6 bits.
        let big = decode_encounter_section(&table_bytes(1, 5, &[(1023, 63)])).unwrap();
        assert_eq!(big.entries, vec![EncounterEntry { formation: 1023, weight: 63 }]);
    }

    #[test]
    fn the_first_usable_table_wins_and_disabled_or_empty_ones_mean_no_battles() {
        // Table 1 disabled, table 2 enabled: table 2.
        let mut s = table_bytes(0, 200, &[(1, 63)]);
        s.extend(table_bytes(1, 10, &[(7, 63)]));
        assert_eq!(decode_encounter_section(&s).unwrap().entries, vec![EncounterEntry { formation: 7, weight: 63 }]);
        // Both enabled: table 1.
        let mut s = table_bytes(1, 10, &[(1, 63)]);
        s.extend(table_bytes(1, 10, &[(7, 63)]));
        assert_eq!(decode_encounter_section(&s).unwrap().entries[0].formation, 1);
        // Enabled but rate 0, or enabled with nothing in it, or all zero: no battles.
        assert_eq!(decode_encounter_section(&table_bytes(1, 0, &[(1, 63)])), None);
        assert_eq!(decode_encounter_section(&table_bytes(1, 9, &[])), None);
        assert_eq!(decode_encounter_section(&[0u8; 48]), None);
        // Unused slots (zero) are not entries.
        let sparse = decode_encounter_section(&table_bytes(1, 9, &[(0, 0), (5, 63)])).unwrap();
        assert_eq!(sparse.entries, vec![EncounterEntry { formation: 5, weight: 63 }]);
    }

    #[test]
    fn a_section_of_the_wrong_shape_decodes_to_nothing_instead_of_panicking() {
        for bytes in [&[][..], &[1u8][..], &[1, 2, 3][..], &[1u8; 23][..]] {
            assert_eq!(decode_encounter_section(bytes), None, "{} bytes", bytes.len());
        }
        // Trailing bytes after the tables are ignored; a short tail is not a table.
        let mut s = table_bytes(1, 9, &[(5, 63)]);
        s.extend([9u8; 7]);
        assert!(decode_encounter_section(&s).is_some());
        // A hostile section of all-ones is a table of formation 1023, weight 63: harmless data.
        assert!(decode_encounter_section(&[0xffu8; 48]).is_some());
    }

    #[test]
    fn every_truncation_and_filler_of_a_section_is_harmless() {
        let mut s = table_bytes(1, 9, &[(5, 31), (6, 33)]);
        s.extend(table_bytes(1, 3, &[(8, 63)]));
        for cut in 0..=s.len() {
            let _ = decode_encounter_section(s.get(..cut).unwrap_or(&[]));
        }
        for filler in [0x00u8, 0x01, 0x7F, 0x80, 0xFF] {
            for len in 0..=100 {
                let _ = decode_encounter_section(&vec![filler; len]);
            }
        }
    }
}
