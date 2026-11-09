//! Analysis helpers: how an Echo-S script section relates to the original's, in what the dialogue walker can see.
//!
//! Echo-S's field scripts are not the original scripts with other strings (see [`crate::mods`]), so these helpers exist to
//! *measure* the difference and to name the lines the two share, never to build a line table the client would use. They
//! compare what the walker interprets (its opcodes, [`walker_op`]) and ignore the rest of a script: movement, animation,
//! sound, the camera, the geometry of windows, palettes, menus and money never change which messages show or in what order.

use fmv_ff7::script::{FieldScript, Instruction, SCRIPTS_PER_ENTITY};
use std::collections::{BTreeMap, BTreeSet};

/// Whether the dialogue walker (`fmv_ff7::script::dialogue`) interprets this opcode. Everything else is a no-op to it.
///
/// The set is read off the walker's own dispatch (`walk.rs`, `Walk::step` and `arith_of`): `RET`, `REQ`/`REQSW`/`REQEW`, `RETTO`,
/// the jumps and the `IF*` family, `MINIGAME`, `IFKEY*`, the gifts (`GOLDu`, `STITM`, `SMTRA`), `MESSAGE`, `ASK`, `MAPJUMP`,
/// `BATTLE`, `GETPC`, the saturating and plain arithmetic, `TLKON`, the bit and byte operations, `RANDOM`, `VISI`,
/// `IFPRTYQ`, `IFMEMBQ` and `GAMEOVER`.
pub fn walker_op(op: u8) -> bool {
    matches!(
        op,
        0x00..=0x03 | 0x07 | 0x10..=0x19 | 0x20 | 0x30..=0x32 | 0x39 | 0x40 | 0x48 | 0x58 | 0x5B | 0x60 | 0x70 | 0x74 | 0x76..=0x7E | 0x80..=0x9C | 0xA4 | 0xCB | 0xCC | 0xFF
    )
}

/// A comparable image of an instruction: the opcode, its sub-opcode and its operands, without absolute offsets and without the
/// `dialog` operand of a message (the number is what is being matched, not what is compared).
pub fn image(ins: &Instruction) -> String {
    let mut s = format!("{:02X}", ins.opcode);
    if let Some((code, _)) = ins.sub {
        s.push_str(&format!(".{code:02X}"));
    }
    for o in &ins.operands {
        if matches!(ins.opcode, 0x40 | 0x48) && o.name == "dialog" {
            continue;
        }
        s.push_str(&format!(" {}={}:{}", o.name, o.bank, o.value));
    }
    if !ins.payload.is_empty() {
        s.push_str(&format!(" payload{}", ins.payload.len()));
    }
    s
}

/// The walker's view of one script: the images of the instructions it interprets, a jump's target named by the index of the first
/// such instruction at or after it (a distance in bytes means nothing once other instructions are inserted), and for each
/// message instruction `(its index, its dialog number)`.
pub fn walker_stream(instructions: &[Instruction]) -> (Vec<String>, Vec<(usize, i32)>) {
    let kept: Vec<&Instruction> = instructions.iter().filter(|i| walker_op(i.opcode)).collect();
    let mut images = Vec::with_capacity(kept.len());
    let mut messages = Vec::new();
    for (k, ins) in kept.iter().enumerate() {
        let mut s = image(ins);
        if let Some(target) = ins.jump_target {
            let idx = kept.iter().position(|j| j.offset >= target).unwrap_or(kept.len());
            s = s.split(' ').filter(|part| !part.starts_with("jump=")).collect::<Vec<_>>().join(" ");
            s.push_str(&format!(" ->{idx}"));
        }
        if matches!(ins.opcode, 0x40 | 0x48) {
            if let Some(d) = ins.literal("dialog") {
                messages.push((k, d));
            }
        }
        images.push(s);
    }
    (images, messages)
}

/// Every `(window, dialog)` a `MESSAGE` or `ASK` with literal operands opens, in any script of any entity.
pub fn message_lines(script: &FieldScript<'_>) -> BTreeSet<(u8, usize)> {
    let mut out = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
            for ins in d.instructions.iter().filter(|i| matches!(i.opcode, 0x40 | 0x48)) {
                if let (Some(w), Some(d)) = (ins.literal("window"), ins.literal("dialog")) {
                    if let (Ok(w), Ok(d)) = (u8::try_from(w), usize::try_from(d)) {
                        out.insert((w, d));
                    }
                }
            }
        }
    }
    out
}

/// How much of the original's script Echo-S's keeps, in what the walker sees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Alignment {
    /// Entities of the original, and how many have a namesake in Echo-S's script (by position when the original's names are a
    /// prefix of Echo-S's, else by name).
    pub entities: usize,
    pub entities_matched: usize,
    /// Scripts (an entity's slots that have code) of the matched entities, and how many are the same to the walker.
    pub scripts: usize,
    pub scripts_equivalent: usize,
    /// Distinct dialog numbers the original's scripts open, and how many of them sit in a script that is the same to the walker in
    /// Echo-S's (the number it opens there may differ: that is the renumbering).
    pub original_lines: usize,
    pub lines_matched: usize,
    /// Of those, the ones whose number is the same in both.
    pub lines_same_number: usize,
}

/// Compare `original` with `echo`, entity by entity and script by script.
pub fn align(original: &FieldScript<'_>, echo: &FieldScript<'_>) -> Alignment {
    let n = original.entity_count();
    let prefix = echo.entity_count() >= n && (0..n).all(|i| original.entity_name(i) == echo.entity_name(i));
    let pairs: Vec<(usize, Option<usize>)> = if prefix {
        (0..n).map(|i| (i, Some(i))).collect()
    } else {
        let mut used = BTreeSet::new();
        (0..n)
            .map(|i| {
                let found = original.entity_name(i).and_then(|name| (0..echo.entity_count()).find(|j| !used.contains(j) && echo.entity_name(*j) == Some(name)));
                if let Some(j) = found {
                    used.insert(j);
                }
                (i, found)
            })
            .collect()
    };
    let mut a = Alignment { entities: n, ..Alignment::default() };
    let mut all_lines: BTreeSet<i32> = BTreeSet::new();
    let mut matched: BTreeMap<i32, bool> = BTreeMap::new(); // original dialog -> some pairing had the same number
    for (i, j) in pairs {
        let echo_entity = j;
        if echo_entity.is_some() {
            a.entities_matched += 1;
        }
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Ok(o) = original.disassemble_partial(i, slot) else { continue };
            let (wo, mo) = walker_stream(&o.instructions);
            for (_, d) in &mo {
                all_lines.insert(*d);
            }
            let Some(j) = echo_entity else { continue };
            let Ok(e) = echo.disassemble_partial(j, slot) else { continue };
            a.scripts += 1;
            let (we, me) = walker_stream(&e.instructions);
            if wo == we {
                a.scripts_equivalent += 1;
                for ((_, p), (_, q)) in mo.iter().zip(&me) {
                    let same = matched.entry(*p).or_insert(false);
                    *same |= p == q;
                }
            }
        }
    }
    a.original_lines = all_lines.len();
    a.lines_matched = matched.len();
    a.lines_same_number = matched.values().filter(|same| **same).count();
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::script_section_code;

    fn msg(window: u8, dialog: u8) -> Vec<u8> {
        vec![0x40, window, dialog]
    }

    fn parse(section: &[u8]) -> FieldScript<'_> {
        FieldScript::parse(section).unwrap()
    }

    #[test]
    fn identical_scripts_match_every_line_with_the_same_numbers() {
        let code: Vec<u8> = [msg(0, 0), msg(0, 1), vec![0x00]].concat();
        let (a, b) = (script_section_code("npc", &code, &["A", "B"]), script_section_code("npc", &code, &["C", "D"]));
        let al = align(&parse(&a), &parse(&b));
        assert_eq!((al.entities, al.entities_matched, al.scripts, al.scripts_equivalent), (1, 1, 32, 32));
        assert_eq!((al.original_lines, al.lines_matched, al.lines_same_number), (2, 2, 2));
        assert_eq!(message_lines(&parse(&a)), BTreeSet::from([(0, 0), (0, 1)]));
    }

    #[test]
    fn a_renumbered_message_is_matched_in_lockstep_but_not_the_same_number() {
        let (a, b) = (
            script_section_code("npc", &[msg(0, 0), vec![0x00]].concat(), &["A"]),
            script_section_code("npc", &[msg(0, 3), vec![0x00]].concat(), &["x", "y", "z", "D"]),
        );
        let al = align(&parse(&a), &parse(&b));
        assert_eq!((al.original_lines, al.lines_matched, al.lines_same_number), (1, 1, 0));
    }

    #[test]
    fn instructions_the_walker_ignores_do_not_break_the_match() {
        // A WAIT (0x24, a u16) inserted before the message is invisible to the walker.
        let (a, b) = (
            script_section_code("npc", &[msg(0, 0), vec![0x00]].concat(), &["A"]),
            script_section_code("npc", &[vec![0x24, 5, 0], msg(0, 0), vec![0x00]].concat(), &["B"]),
        );
        let al = align(&parse(&a), &parse(&b));
        assert_eq!((al.scripts, al.scripts_equivalent, al.lines_matched), (32, 32, 1));
    }

    #[test]
    fn a_message_added_or_removed_breaks_the_equivalence() {
        let (a, b) = (
            script_section_code("npc", &[msg(0, 0), vec![0x00]].concat(), &["A"]),
            script_section_code("npc", &[msg(0, 0), msg(0, 1), vec![0x00]].concat(), &["B", "C"]),
        );
        let al = align(&parse(&a), &parse(&b));
        assert_eq!((al.scripts_equivalent, al.lines_matched), (31, 0), "the talk script differs, the other 31 slots are the same RET");
        assert_eq!(al.original_lines, 1);
    }

    #[test]
    fn entities_are_matched_by_name_when_the_order_changed() {
        let a = script_section_code("alpha", &[msg(0, 0), vec![0x00]].concat(), &["A"]);
        let b = script_section_code("beta", &[msg(0, 0), vec![0x00]].concat(), &["B"]);
        let al = align(&parse(&a), &parse(&b));
        assert_eq!((al.entities, al.entities_matched, al.lines_matched), (1, 0, 0));
    }
}
