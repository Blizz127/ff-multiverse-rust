//! Minimal reader of the model-loader section (section 2): just enough to turn the model index
//! a `CHAR` opcode names into the model's name and `.HRC` file.
//!
//! Layout (FFRTT "FF7/Field/Model_Loader" <https://wiki.ffrtt.ru/index.php/FF7/Field/Model_Loader>):
//!
//! ```text
//! u16 0, u16 model count M, u16 scale, then M records:
//!   u16 N, char[N] name, u16 (playable flag), char[8] HRC file name, char[4] scale text,
//!   u16 animation count A, 3 x (RGB + 3 x i16 light position), RGB global light,
//!   A x { u16 L, char[L] animation name, u16 unknown }
//! ```
//!
//! Only the names are kept. The full per-model data (lights, animation names) belongs to the
//! model/animation work; this reader walks past it so later records stay aligned, and reports
//! how many bytes of the section the records covered so callers can see whether the walk
//! landed exactly on the end.

use super::ScriptError;
use crate::util::{nul_padded_name, slice_at, u16_at};

/// Fixed bytes between a model's name and its animation list start (after the name): playable
/// flag (2) + HRC name (8) + scale text (4) + animation count (2) + 3 lights (3 + 6 each) + global
/// light colour (3).
const FIXED_AFTER_NAME: usize = 2 + 8 + 4 + 2 + 3 * (3 + 6) + 3;

/// One model-loader record (names only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    /// The record's descriptive name (not displayed by the game).
    pub name: String,
    /// The `.HRC` skeleton file name, e.g. `AAAA.HRC`.
    pub hrc: String,
    /// Number of animations the record lists.
    pub animation_count: u16,
}

/// Result of walking the model loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelList {
    pub models: Vec<ModelRef>,
    /// Header model count (may exceed `models.len()` if the section is damaged).
    pub declared: u16,
    /// Bytes of the section the header and records covered.
    pub covered: usize,
}

/// Walk the section. Fails only when even the six-byte header is missing; a record that does
/// not fit ends the list early (`models.len() < declared`).
pub fn parse_model_list(section: &[u8]) -> Result<ModelList, ScriptError> {
    let declared = u16_at(section, 2).ok_or(ScriptError::Truncated { what: "model loader header", at: 0, needed: 6, len: section.len() })?;
    let mut pos = 6usize;
    let mut models = Vec::new();
    for _ in 0..declared {
        match read_record(section, pos) {
            Some((model, next)) => {
                models.push(model);
                pos = next;
            }
            None => break,
        }
    }
    Ok(ModelList { models, declared, covered: pos.min(section.len()) })
}

fn read_record(section: &[u8], pos: usize) -> Option<(ModelRef, usize)> {
    let name_len = usize::from(u16_at(section, pos)?);
    let name_at = pos.checked_add(2)?;
    let name = nul_padded_name(slice_at(section, name_at, name_len)?);
    let fixed_at = name_at.checked_add(name_len)?;
    let hrc = nul_padded_name(slice_at(section, fixed_at.checked_add(2)?, 8)?);
    let animation_count = u16_at(section, fixed_at.checked_add(2 + 8 + 4)?)?;
    let mut next = fixed_at.checked_add(FIXED_AFTER_NAME)?;
    slice_at(section, fixed_at, FIXED_AFTER_NAME)?;
    for _ in 0..animation_count {
        let l = usize::from(u16_at(section, next)?);
        next = next.checked_add(2)?.checked_add(l)?.checked_add(2)?;
        // the animation record must lie inside the section
        if next > section.len() {
            return None;
        }
    }
    Some((ModelRef { name, hrc, animation_count }, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, hrc: &str, anims: &[&str]) -> Vec<u8> {
        let mut v = (name.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&[0, 0]); // playable flag
        let mut h = [0u8; 8];
        h[..hrc.len()].copy_from_slice(hrc.as_bytes());
        v.extend_from_slice(&h);
        v.extend_from_slice(b"512\0"); // scale text
        v.extend_from_slice(&(anims.len() as u16).to_le_bytes());
        v.extend_from_slice(&[0u8; 3 * (3 + 6) + 3]);
        for a in anims {
            v.extend_from_slice(&(a.len() as u16).to_le_bytes());
            v.extend_from_slice(a.as_bytes());
            v.extend_from_slice(&[0, 0]);
        }
        v
    }

    fn section(records: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0, 0];
        v.extend_from_slice(&(records.len() as u16).to_le_bytes());
        v.extend_from_slice(&[0, 2]); // scale 512
        for r in records {
            v.extend_from_slice(r);
        }
        v
    }

    #[test]
    fn reads_names_and_covers_the_section() {
        let s = section(&[record("hero", "AAAA.HRC", &["ONE.A", "TWO.A"]), record("npc", "BBBB.HRC", &[])]);
        let list = parse_model_list(&s).unwrap();
        assert_eq!(list.declared, 2);
        assert_eq!(list.covered, s.len());
        assert_eq!(list.models[0], ModelRef { name: "hero".into(), hrc: "AAAA.HRC".into(), animation_count: 2 });
        assert_eq!(list.models[1].hrc, "BBBB.HRC");
    }

    #[test]
    fn damaged_sections_end_the_list_early_without_panicking() {
        let s = section(&[record("hero", "AAAA.HRC", &["ONE.A"]), record("npc", "BBBB.HRC", &[])]);
        for cut in 0..s.len() {
            let cutoff = s.get(..cut).unwrap_or(&[]);
            match parse_model_list(cutoff) {
                Ok(list) => assert!(list.models.len() <= 2 && list.covered <= cutoff.len().max(6)),
                Err(_) => assert!(cut < 4),
            }
        }
        assert!(parse_model_list(&[]).is_err());
    }
}
