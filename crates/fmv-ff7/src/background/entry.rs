//! Reviewed background initialization before a field's first story scene.
//!
//! Field entry clears the native group masks, then executes each entity's initializer in entity
//! order. The cargo and passenger-car backgrounds have small, unconditional initializers. Read
//! their commands from the owner's original scripts rather than substituting group/frame values.
//! This is deliberately not a general field-script interpreter: an unexpected instruction,
//! variable operand or control-flow path refuses the entire baseline with a diagnostic.

use super::BackgroundOp;
use crate::field::FieldFile;
use crate::script::{decode_at, FieldScript, Instruction};

/// The reviewed entities whose initializers establish background visibility, in native order.
/// Names also fence a different field-script layout; nothing silently binds to a different entity.
fn initializers(key: &str) -> &'static [(usize, &'static str)] {
    if key.eq_ignore_ascii_case("cargoin") {
        &[(0, "door"), (1, "wall"), (4, "lg")]
    } else if key.eq_ignore_ascii_case("tin_1") {
        &[(1, "DR_AN2"), (6, "TURI1"), (7, "TURI2"), (10, "TURI1-2"), (11, "TURI2-2"), (13, "TURI1-3"), (14, "TURI2-3")]
    } else {
        &[]
    }
}

/// Read the reviewed field's literal background initializer commands. Other fields return an
/// empty list and retain the existing static baseline. Apply the returned operations to a fresh
/// [`super::BackgroundState`] on field entry only, never on every scene start or scene end.
///
/// An error means the reviewed initializer could not be reconstructed completely. The caller may
/// show its static fallback, but must retain the diagnostic rather than claim the authored baseline.
pub fn entry_background_ops(field: &FieldFile, key: &str) -> Result<Vec<BackgroundOp>, String> {
    let entries = initializers(key);
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let script = FieldScript::from_field(field).map_err(|e| format!("background init {key}: {e}"))?;
    let mut result = Vec::new();
    for &(entity, name) in entries {
        if script.entity_name(entity) != Some(name) {
            return Err(format!("background init {key}: entity {entity} is not {name}"));
        }
        let code = script.script_bytes(entity, 0)
            .ok_or_else(|| format!("background init {key}/{name}: no initializer"))?;
        let scratch = key.eq_ignore_ascii_case("cargoin") && entity == 4;
        let changes = initializer_ops(code, scratch)
            .map_err(|e| format!("background init {key}/{name}: {e}"))?;
        result.extend(changes);
    }
    Ok(result)
}

/// Parse through the first RET, without following a branch or executing a variable read. The
/// cargo light initializer also clears scratch bytes 5:0 and 5:1; these cannot affect any operand
/// accepted here, since every background operand must be literal.
fn initializer_ops(code: &[u8], cargo_light_scratch: bool) -> Result<Vec<BackgroundOp>, String> {
    let mut changes = Vec::new();
    let mut scratch_writes = 0usize;
    let mut at = 0;
    while at < code.len() {
        let ins = decode_at(code, at, 0).map_err(|e| e.to_string())?;
        if ins.stray_banks {
            return Err(format!("unexpected bank selector at {at:#x}"));
        }
        match ins.opcode {
            0x00 => {
                if changes.is_empty() {
                    return Err("initializer contains no background instruction".into());
                }
                if cargo_light_scratch && scratch_writes != 2 {
                    return Err("cargo light initializer did not clear both reviewed scratch bytes".into());
                }
                return Ok(changes);
            }
            0xE0..=0xE4 => changes.push(background_op(&ins)?),
            0x80 if cargo_light_scratch => {
                let target = ins.operand("var").ok_or("SETBYTE has no destination")?;
                if target.bank != 5 || target.value != scratch_writes as i32 || scratch_writes >= 2 || ins.literal("value") != Some(0) {
                    return Err(format!("unreviewed scratch assignment at {at:#x}"));
                }
                scratch_writes += 1;
            }
            _ => return Err(format!("unreviewed {} at {at:#x}", ins.mnemonic)),
        }
        at = ins.end();
    }
    Err("initializer has no RET".into())
}

fn background_op(ins: &Instruction) -> Result<BackgroundOp, String> {
    let literal = |name: &str, limit: u8| -> Result<u8, String> {
        ins.literal(name).and_then(|value| u8::try_from(value).ok()).filter(|value| *value < limit)
            .ok_or_else(|| format!("{} has an unsupported or nonliteral {name} at {:#x}", ins.mnemonic, ins.offset))
    };
    let group = literal("group", 64)?;
    Ok(match ins.opcode {
        0xE0 => BackgroundOp::On { group, frame: literal("frame", 8)? },
        0xE1 => BackgroundOp::Off { group, frame: literal("frame", 8)? },
        0xE2 => BackgroundOp::ShiftLeft { group },
        0xE3 => BackgroundOp::ShiftRight { group },
        0xE4 => BackgroundOp::Clear { group },
        _ => return Err(format!("{} is not a background instruction", ins.mnemonic)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::{decode_background_with, BackgroundOptions, BackgroundState, GroupFilter};
    use crate::lgp::LgpArchive;
    use std::path::PathBuf;

    #[test]
    fn initializers_read_literals_in_order_and_stop_at_the_first_return() {
        let code = [0xE0, 0, 8, 2, 0xE2, 0, 8, 0xE3, 0, 8, 0xE1, 0, 8, 2, 0xE4, 0, 8, 0, 0x10, 99];
        assert_eq!(initializer_ops(&code, false).unwrap(), [
            BackgroundOp::On { group: 8, frame: 2 }, BackgroundOp::ShiftLeft { group: 8 },
            BackgroundOp::ShiftRight { group: 8 }, BackgroundOp::Off { group: 8, frame: 2 },
            BackgroundOp::Clear { group: 8 },
        ]);
        let light = [0xE4, 0, 1, 0x80, 0x50, 0, 0, 0x80, 0x50, 1, 0, 0];
        assert_eq!(initializer_ops(&light, true).unwrap(), [BackgroundOp::Clear { group: 1 }]);
        assert!(initializer_ops(&light, false).is_err(), "scratch writes are reviewed only for the cargo light");
    }

    #[test]
    fn uncertain_initialization_returns_no_partial_background_state() {
        for code in [
            vec![0xE0, 0, 2, 0, 0x10, 1, 0], // branch after a valid operation
            vec![0xE0, 0x50, 2, 0, 0], // variable group
            vec![0xE0, 0x05, 2, 0, 0], // variable frame
            vec![0xE0, 0, 64, 0, 0], // unsupported group
            vec![0xE0, 0, 2, 8, 0], // unsupported frame
            vec![0xE0, 0, 2, 0], // no return
            vec![0xE0, 0, 2], // truncated opcode
            vec![0], // missing expected background setup
        ] {
            assert!(initializer_ops(&code, false).is_err(), "accepted {code:?}");
        }
        for code in [
            vec![0xE4, 0, 1, 0], // omitted scratch initialization
            vec![0xE4, 0, 1, 0x80, 0x50, 0, 0, 0], // only one scratch byte
            vec![0xE4, 0, 1, 0x80, 0x50, 0, 1, 0x80, 0x50, 1, 0, 0], // changed value
            vec![0xE4, 0, 1, 0x80, 0x51, 0, 0, 0x80, 0x50, 1, 0, 0], // variable value
        ] {
            assert!(initializer_ops(&code, true).is_err(), "accepted {code:?}");
        }
    }

    fn owned_fields() -> Option<LgpArchive<std::fs::File>> {
        let path = PathBuf::from(std::env::var_os("FMV_FF7_DATA")?).join("field/flevel.lgp");
        Some(LgpArchive::open_path(path).expect("owned field archive"))
    }

    fn field(archive: &mut LgpArchive<std::fs::File>, name: &str) -> FieldFile {
        let stored = archive.read(name).expect("owned field");
        FieldFile::parse(crate::lzss::decompress(&stored).expect("field compression")).expect("field container")
    }

    fn state(ops: &[BackgroundOp]) -> BackgroundState {
        let mut state = BackgroundState::default();
        for &op in ops { state.apply(op); }
        state
    }

    #[test]
    #[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
    fn owned_cargo_and_train_initializers_and_door_frames_are_fully_accounted() {
        let Some(mut archive) = owned_fields() else { eprintln!("FMV_FF7_DATA is needed; skipping"); return };
        let cargo = field(&mut archive, "cargoin");
        let cargo_ops = entry_background_ops(&cargo, "cargoin").expect("complete cargo initialization");
        assert_eq!(cargo_ops, [BackgroundOp::On { group: 2, frame: 0 }, BackgroundOp::Clear { group: 8 }, BackgroundOp::Clear { group: 1 }]);
        let train = field(&mut archive, "tin_1");
        let train_ops = entry_background_ops(&train, "tin_1").expect("complete train initialization");
        assert_eq!(train_ops, [
            BackgroundOp::On { group: 6, frame: 1 }, BackgroundOp::On { group: 4, frame: 1 }, BackgroundOp::On { group: 5, frame: 1 },
            BackgroundOp::On { group: 4, frame: 1 }, BackgroundOp::On { group: 5, frame: 1 },
            BackgroundOp::On { group: 4, frame: 1 }, BackgroundOp::On { group: 5, frame: 1 },
        ]);
        assert_eq!(initializers("cargoin").len(), 3);
        assert_eq!(initializers("tin_1").len(), 7);
        for (key, field, baseline, group, frames) in [
            ("cargoin", &cargo, state(&cargo_ops), 8, &[2u8][..]),
            ("tin_1", &train, state(&train_ops), 6, &[1u8, 2, 3, 4][..]),
        ] {
            let tiles = field.background().expect("field background");
            let options = |state| BackgroundOptions { groups: GroupFilter::State(state), ..BackgroundOptions::default() };
            let before = decode_background_with(field, &options(baseline.clone())).expect("baseline pixels");
            for &frame in frames {
                let selected: Vec<_> = tiles.tiles.iter().filter(|tile| tile.layer > 1 && tile.param == group && tile.state & (1 << frame) != 0).collect();
                assert!(!selected.is_empty(), "{key}: no tiles for group {group} frame {frame}");
                let mut shown = baseline.clone();
                shown.apply(BackgroundOp::Clear { group });
                shown.apply(BackgroundOp::On { group, frame });
                let after = decode_background_with(field, &options(shown)).expect("authored frame pixels");
                assert_eq!((before.width, before.height), (after.width, after.height));
                let changed = before.rgba.chunks_exact(4).zip(after.rgba.chunks_exact(4)).filter(|(a, b)| a != b).count();
                let expected = match (key, frame) {
                    ("cargoin", 2) => (16, 93),
                    ("tin_1", 1) => (37, 0),
                    ("tin_1", 2) => (36, 6_219),
                    ("tin_1", 3) => (30, 6_955),
                    ("tin_1", 4) => (19, 7_094),
                    _ => unreachable!(),
                };
                assert_eq!((selected.len(), changed), expected, "{key}: authored frame {frame}");
                let layers: std::collections::BTreeSet<_> = selected.iter().map(|tile| tile.layer).collect();
                assert_eq!(layers, std::collections::BTreeSet::from([2]), "the authored door variants are depth-tested layer 2");
                eprintln!("{key}: group {group} frame {frame}: {} tiles, layers {layers:?}, {changed} changed pixels", selected.len());
            }
        }
    }
}
