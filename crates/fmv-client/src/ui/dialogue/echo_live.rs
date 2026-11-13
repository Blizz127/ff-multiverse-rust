//! Echo-S's own line for a window of a live field.
//!
//! The server runs the ORIGINAL field's scripts and names a window by the original's dialog number plus the instruction that
//! opened it (the script entity and the section offset of its `MESSAGE` or `ASK`). Echo-S replaces whole script sections and
//! numbers its dialogs differently, so its line is found by that instruction: the k-th `MESSAGE`/`ASK` (both kinds counted, in
//! decode order) of script slot S of the same entity **under the same branch conditions** (the `IF…` tests whose forward jump
//! skips it). Echo-S adds and drops lines inside a script: nmkin_3's Jessie loses the ladder tutorial, so counting across the
//! whole script gave the tutorial window her stuck-leg line. A message with no counterpart under the same conditions keeps
//! the original's text. The entity is matched by index when Echo-S's entity at that index has the
//! original's name, else by name when exactly one has it. [`build`] maps every message instruction of an area once; [`Skip`]
//! says why one has no Echo-S line (the window then shows the original's text).

use fmv_ff7::script::{FieldScript, Instruction, SCRIPTS_PER_ENTITY};
use std::collections::HashMap;

const MESSAGE: u8 = 0x40;
const ASK: u8 = 0x48;

/// What Echo-S's counterpart instruction says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoLine {
    /// The window number Echo-S's instruction names (what its voice files are keyed by).
    pub window: u8,
    /// The dialog's index in Echo-S's table.
    pub dialog: usize,
    /// An `ASK`'s first and last line (`None` for a `MESSAGE`).
    pub ask: Option<(u8, u8)>,
}

/// Why a window has no Echo-S line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The layer is off, Echo-S has no script for the area, or this machine has no original to compare.
    NoSection,
    /// No message instruction of the original sits there (the area's scripts differ from the server's).
    NoInstruction,
    /// The instruction there opens another dialog than the server said.
    DialogDiffers,
    /// Echo-S has no (single) entity of that name.
    NoEntity,
    /// Echo-S's entity has no such script, or fewer messages in it.
    NoCounterpart,
    /// The counterpart is a `MESSAGE` where the original has an `ASK`, or the other way round.
    KindDiffers,
    /// An operand of the counterpart is a variable, not a number.
    Banked,
    /// Echo-S's table has no such dialog.
    NoText,
    /// The questions have a different number of lines.
    AskDiffers,
}

impl Skip {
    /// A word for the one-line note (ids and reasons only).
    pub fn why(self) -> &'static str {
        match self {
            Self::NoSection => "not in echo-s",
            Self::NoInstruction => "no such instruction",
            Self::DialogDiffers => "dialog differs",
            Self::NoEntity => "no such entity in echo-s",
            Self::NoCounterpart => "no counterpart in echo-s",
            Self::KindDiffers => "message kind differs",
            Self::Banked => "variable operand",
            Self::NoText => "no such dialog in echo-s",
            Self::AskDiffers => "question differs",
        }
    }
}

/// One message instruction of the original and what Echo-S has for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The dialog the original's instruction opens (checked against what the server says).
    pub original_dialog: u8,
    pub echo: Result<EchoLine, Skip>,
}

/// The map of one area: `(entity, section offset)` of an original `MESSAGE`/`ASK` to its Echo-S counterpart.
pub type LiveMap = HashMap<(u16, u32), Entry>;

/// The message instructions of script `slot` of `entity`, in decode order (what decoded: a script that does not decode to its end
/// still counts its first k).
#[cfg(test)]
fn messages(script: &FieldScript<'_>, entity: usize, slot: usize) -> Vec<Instruction> {
    guarded_messages(script, entity, slot).into_iter().map(|(m, _)| m).collect()
}

/// An `IF…` test as a key: its mnemonic and every operand but the jump.
fn condition(i: &Instruction) -> String {
    let mut key = i.mnemonic.to_string();
    for o in i.operands.iter().filter(|o| o.name != "jump") {
        key.push_str(&format!(" {}={}:{}", o.name, o.bank, o.value));
    }
    key
}

/// The message instructions of a script with the branch conditions they sit under: the `IF…` instructions whose forward jump
/// passes over them, outermost first.
fn guarded_messages(script: &FieldScript<'_>, entity: usize, slot: usize) -> Vec<(Instruction, Vec<String>)> {
    let Ok(d) = script.disassemble_partial(entity, slot) else { return Vec::new() };
    let tests: Vec<(usize, usize, String)> = d
        .instructions
        .iter()
        .filter(|i| i.mnemonic.starts_with("IF"))
        .filter_map(|i| i.jump_target.filter(|t| *t > i.offset).map(|t| (i.offset, t, condition(i))))
        .collect();
    d.instructions
        .into_iter()
        .filter(|i| matches!(i.opcode, MESSAGE | ASK))
        .map(|m| {
            let under = tests.iter().filter(|(at, to, _)| *at < m.offset && m.offset < *to).map(|(_, _, c)| c.clone()).collect();
            (m, under)
        })
        .collect()
}

/// The entity of `echo` that stands for entity `e` of `original`: the same index with the same name, else the one entity of
/// that name, else the same index when Echo-S renamed it there (its name there is no original entity's: elevtr1's `cl`, `ba`
/// and `av_j` are `Cloud`, `Barrett` and `Jessie` in Echo-S, in the same places).
fn echo_entity(original: &FieldScript<'_>, echo: &FieldScript<'_>, e: usize) -> Option<usize> {
    let name = original.entity_name(e)?;
    if echo.entity_name(e) == Some(name) {
        return Some(e);
    }
    let mut named = (0..echo.entity_count()).filter(|&i| echo.entity_name(i) == Some(name));
    match (named.next(), named.next()) {
        (Some(first), None) => return Some(first),
        (Some(_), Some(_)) => return None,
        (None, _) => {}
    }
    let theirs = echo.entity_name(e)?;
    let renamed = !(0..original.entity_count()).any(|i| original.entity_name(i) == Some(theirs));
    renamed.then_some(e)
}

fn counterpart(original: &Instruction, theirs: Option<&Instruction>) -> Result<EchoLine, Skip> {
    let theirs = theirs.ok_or(Skip::NoCounterpart)?;
    if theirs.opcode != original.opcode {
        return Err(Skip::KindDiffers);
    }
    let lit = |name: &str| theirs.literal(name).and_then(|v| u8::try_from(v).ok()).ok_or(Skip::Banked);
    let ask = if theirs.opcode == ASK { Some((lit("first")?, lit("last")?)) } else { None };
    Ok(EchoLine { window: lit("window")?, dialog: usize::from(lit("dialog")?), ask })
}

/// Map every message instruction of `original`'s scripts (both sections are script sections as the layout reads them).
pub fn build(original: &[u8], echo: &[u8]) -> LiveMap {
    let mut map = LiveMap::new();
    let (Ok(o), Ok(e)) = (FieldScript::parse(original), FieldScript::parse(echo)) else { return map };
    for entity in 0..o.entity_count() {
        let echo_at = echo_entity(&o, &e, entity);
        for slot in 0..SCRIPTS_PER_ENTITY {
            let ours = guarded_messages(&o, entity, slot);
            if ours.is_empty() {
                continue;
            }
            let theirs = match echo_at {
                Some(at) => guarded_messages(&e, at, slot),
                None => Vec::new(),
            };
            for (k, (instruction, under)) in ours.iter().enumerate() {
                // The k-th message under these conditions, and Echo-S's k-th under the same ones.
                let k = ours[..k].iter().filter(|(_, u)| u == under).count();
                let theirs_here: Vec<&Instruction> = theirs.iter().filter(|(_, u)| u == under).map(|(m, _)| m).collect();
                let (Ok(entity), Ok(offset), Some(dialog)) =
                    (u16::try_from(entity), u32::try_from(instruction.offset), instruction.literal("dialog").and_then(|d| u8::try_from(d).ok()))
                else {
                    continue;
                };
                // Slots can share code: the lowest slot that holds an instruction is the one that counts.
                map.entry((entity, offset)).or_insert_with(|| Entry {
                    original_dialog: dialog,
                    echo: if echo_at.is_none() { Err(Skip::NoEntity) } else { counterpart(instruction, theirs_here.get(k).copied()) },
                });
            }
        }
    }
    map
}

/// Echo-S's line for the window opened by the instruction at (`entity`, `offset`) that the server says opens `dialog`.
pub fn lookup(map: &LiveMap, entity: u16, offset: u32, dialog: u16) -> Result<EchoLine, Skip> {
    let entry = map.get(&(entity, offset)).ok_or(Skip::NoInstruction)?;
    if u16::from(entry.original_dialog) != dialog {
        return Err(Skip::DialogDiffers);
    }
    entry.echo
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `(slot, code)` of each script an entity has.
    pub type Slots = Vec<(usize, Vec<u8>)>;

    pub fn msg(window: u8, dialog: u8) -> Vec<u8> {
        vec![MESSAGE, window, dialog]
    }

    pub fn ask(window: u8, dialog: u8, first: u8, last: u8) -> Vec<u8> {
        vec![ASK, 0, window, dialog, first, last, 0]
    }

    /// A script section of the given entities (name, `(slot, code)` ending in a `RET`) and dialog texts; slots not listed are a bare `RET`.
    pub fn section(entities: &[(&str, Slots)], dialogs: &[&str]) -> Vec<u8> {
        const HEADER: usize = 0x20;
        const NAME: usize = 8;
        let code_start = HEADER + entities.len() * NAME + entities.len() * SCRIPTS_PER_ENTITY * 2;
        let mut code = Vec::new();
        let mut entries = Vec::new();
        for (_, scripts) in entities {
            for slot in 0..SCRIPTS_PER_ENTITY {
                entries.push(u16::try_from(code_start + code.len()).unwrap());
                code.extend(scripts.iter().find(|(s, _)| *s == slot).map_or_else(|| vec![0x00], |(_, c)| c.clone()));
            }
        }
        let mut out = vec![0u8; HEADER];
        out[0] = 2;
        out[1] = 5;
        out[2] = u8::try_from(entities.len()).unwrap();
        out[4..6].copy_from_slice(&u16::try_from(code_start + code.len()).unwrap().to_le_bytes());
        out[8..10].copy_from_slice(&512u16.to_le_bytes());
        for (name, _) in entities {
            let mut n = [0u8; NAME];
            n[..name.len()].copy_from_slice(name.as_bytes());
            out.extend_from_slice(&n);
        }
        for e in entries {
            out.extend_from_slice(&e.to_le_bytes());
        }
        out.extend_from_slice(&code);
        // Printable ASCII; '\n' is a new line and '\t' a choice marker.
        let strings: Vec<Vec<u8>> = dialogs
            .iter()
            .map(|d| {
                let mut v: Vec<u8> = d.bytes().map(|b| match b {
                    b'\n' => 0xE7,
                    b'\t' => 0xE0,
                    b => b - 0x20,
                }).collect();
                v.push(0xFF);
                v
            })
            .collect();
        out.extend_from_slice(&u16::try_from(strings.len()).unwrap().to_le_bytes());
        let mut ptr = 2 + 2 * strings.len();
        for s in &strings {
            out.extend_from_slice(&u16::try_from(ptr).unwrap().to_le_bytes());
            ptr += s.len();
        }
        for s in &strings {
            out.extend_from_slice(s);
        }
        out
    }

    pub fn code(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().chain([0x00]).collect()
    }

    /// The section offset of the k-th message instruction of slot `slot` of `entity`.
    pub fn offset_of(section: &[u8], entity: usize, slot: usize, k: usize) -> u32 {
        let script = FieldScript::parse(section).unwrap();
        u32::try_from(messages(&script, entity, slot)[k].offset).unwrap()
    }

    fn line(window: u8, dialog: usize, ask: Option<(u8, u8)>) -> EchoLine {
        EchoLine { window, dialog, ask }
    }

    #[test]
    fn the_kth_message_of_the_same_script_is_the_counterpart_whatever_the_numbering() {
        let original = section(&[("a", vec![(1, code(&[msg(0, 0), ask(1, 1, 1, 2), msg(0, 2)]))])], &["x", "y", "z"]);
        let echo = section(&[("a", vec![(1, code(&[msg(0, 7), ask(1, 4, 2, 3), msg(2, 5)]))])], &["0", "1", "2", "3", "4", "5", "6", "7"]);
        let map = build(&original, &echo);
        assert_eq!(map.len(), 3);
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 0), 0), Ok(line(0, 7, None)));
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 1), 1), Ok(line(1, 4, Some((2, 3)))));
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 2), 2), Ok(line(2, 5, None)));
    }

    #[test]
    fn the_server_s_dialog_and_offset_are_checked_against_the_original() {
        let original = section(&[("a", vec![(1, code(&[msg(0, 0)]))])], &["x"]);
        let echo = section(&[("a", vec![(1, code(&[msg(0, 0)]))])], &["y"]);
        let map = build(&original, &echo);
        let at = offset_of(&original, 0, 1, 0);
        assert_eq!(lookup(&map, 0, at, 0), Ok(line(0, 0, None)));
        assert_eq!(lookup(&map, 0, at, 3), Err(Skip::DialogDiffers));
        assert_eq!(lookup(&map, 0, at + 1, 0), Err(Skip::NoInstruction));
        assert_eq!(lookup(&map, 1, at, 0), Err(Skip::NoInstruction));
    }

    #[test]
    fn scripts_are_matched_by_slot_and_extra_lines_do_not_shift_other_scripts() {
        let original = section(&[("a", vec![(1, code(&[msg(0, 0)])), (3, code(&[msg(0, 1), msg(0, 2)]))])], &["x", "y", "z"]);
        // Echo-S's slot 1 has an extra message first; slot 3 is the same shape.
        let echo = section(&[("a", vec![(1, code(&[msg(0, 5), msg(0, 6)])), (3, code(&[msg(0, 8), msg(0, 9)]))])], &["0"; 10]);
        let map = build(&original, &echo);
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 0), 0), Ok(line(0, 5, None)));
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 3, 1), 2), Ok(line(0, 9, None)));
    }

    #[test]
    fn a_missing_or_other_kind_counterpart_is_a_skip_with_a_reason() {
        let original = section(&[("a", vec![(1, code(&[msg(0, 0), ask(0, 1, 1, 2), msg(0, 2)]))])], &["x", "y", "z"]);
        let echo = section(&[("a", vec![(1, code(&[ask(0, 0, 1, 2), msg(0, 1)]))])], &["p", "q"]);
        let map = build(&original, &echo);
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 0), 0), Err(Skip::KindDiffers));
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 1), 1), Err(Skip::KindDiffers));
        assert_eq!(lookup(&map, 0, offset_of(&original, 0, 1, 2), 2), Err(Skip::NoCounterpart));
    }

    #[test]
    fn entities_match_by_index_then_by_a_single_name_else_not_at_all() {
        let one = |d: u8| vec![(1, code(&[msg(0, d)]))];
        let original = section(&[("a", one(0)), ("b", one(1)), ("c", one(2)), ("c", one(3))], &["0", "1", "2", "3"]);
        // Echo-S: same names in another order, plus a new entity; "c" appears twice (ambiguous by name, but index 3 still matches).
        let echo = section(&[("z", one(9)), ("b", one(8)), ("a", one(7)), ("c", one(6)), ("c", one(5))], &["0"; 10]);
        let map = build(&original, &echo);
        let at = |e: usize| offset_of(&original, e, 1, 0);
        assert_eq!(lookup(&map, 0, at(0), 0), Ok(line(0, 7, None)), "'a' moved: found by its name");
        assert_eq!(lookup(&map, 1, at(1), 1), Ok(line(0, 8, None)), "'b' moved: found by its name");
        assert_eq!(lookup(&map, 3, at(3), 3), Ok(line(0, 6, None)), "index 3 is a 'c' there too");
        // At index 2 Echo-S has the name 'a', not 'c', and two entities are called 'c': ambiguous.
        assert_eq!(lookup(&map, 2, at(2), 2), Err(Skip::NoEntity));
        // Same index, same name: no further question.
        let same = section(&[("a", one(4))], &["0"; 5]);
        let only = section(&[("a", one(0))], &["0"]);
        assert_eq!(lookup(&build(&only, &same), 0, offset_of(&only, 0, 1, 0), 0), Ok(line(0, 4, None)));
        // No entity of the name, but Echo-S renamed the one in its place (elevtr1's cl is Cloud there): the same index.
        let other = section(&[("q", one(4))], &["0"; 5]);
        assert_eq!(lookup(&build(&only, &other), 0, offset_of(&only, 0, 1, 0), 0), Ok(line(0, 4, None)));
        // Nothing in its place.
        let none = section(&[], &["0"]);
        assert_eq!(lookup(&build(&only, &none), 0, offset_of(&only, 0, 1, 0), 0), Err(Skip::NoEntity));
        // In its place an entity named like another of the original's: not a rename.
        let two = section(&[("a", one(0)), ("b", one(1))], &["0", "1"]);
        let swapped = section(&[("b", one(4)), ("x", one(5))], &["0"; 6]);
        let map = build(&two, &swapped);
        assert_eq!(lookup(&map, 0, offset_of(&two, 0, 1, 0), 0), Err(Skip::NoEntity), "'a' has no namesake and 'b' is not a rename of it");
    }

    #[test]
    fn garbage_sections_map_nothing() {
        assert!(build(&[1, 2, 3], &[4, 5]).is_empty());
        let original = section(&[("a", vec![(1, code(&[msg(0, 0)]))])], &["x"]);
        assert!(build(&original, &[]).is_empty());
    }
}

/// Coverage of the map on the owner's own files (`FMV_FF7_DATA` = the folder with `field/flevel.lgp`, `FMV_ECHO_IRO` = Echo-S's
/// archive): how many of the original's messages have an Echo-S line, and how many lines the old whole-script count gave that
/// the conditions now refuse or change. Counts only, no text.
#[cfg(test)]
mod owned_coverage {
    use super::*;
    use fmv_ff7::field::FieldFile;
    use fmv_ff7::lgp::LgpArchive;
    use fmv_ff7::lzss;

    fn section_of(bytes: &[u8]) -> Option<Vec<u8>> {
        let file = FieldFile::parse(lzss::decompress(bytes).ok()?).ok()?;
        Some(FieldScript::from_field(&file).ok()?.bytes().to_vec())
    }

    #[test]
    #[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and Echo-S (FMV_ECHO_IRO)"]
    fn conditions_keep_jessie_s_tutorial_off_her_stuck_leg_line_and_report_coverage() {
        let (Some(data), Some(iro)) = (std::env::var_os("FMV_FF7_DATA"), std::env::var_os("FMV_ECHO_IRO")) else { return };
        let mut orig = LgpArchive::open_path(std::path::Path::new(&data).join("field/flevel.lgp")).unwrap();
        let echo = fmv_mods::IroArchive::open_path(&iro).unwrap();
        let names: Vec<String> = orig.list().iter().map(|entry| entry.name.clone()).collect();
        let (mut total, mut mapped, mut positional, mut switched, mut refused, mut gained) = (0, 0, 0, 0, 0, 0);
        // Speaker agreement (the name on a line's first row): [old agrees, old total, new agrees, new total] per group.
        let mut agree = std::collections::BTreeMap::<&str, [u32; 4]>::new();
        fn speaker(s: &FieldScript<'_>, d: usize) -> Option<String> {
            let t = s.dialog_text(d)?.text;
            let first = t.split('\n').next()?.trim().to_string();
            (!first.is_empty() && first.chars().count() <= 16 && !first.contains('“')).then_some(first)
        }
        for name in names {
            let (Ok(ob), Ok(eb)) = (orig.read(&name), echo.read_named(&format!("Echo-S\\flevel.lgp\\{name}"))) else { continue };
            let (Some(os), Some(es)) = (section_of(&ob), section_of(&eb)) else { continue };
            let map = build(&os, &es);
            let (Ok(o), Ok(e)) = (FieldScript::parse(&os), FieldScript::parse(&es)) else { continue };
            for entity in 0..o.entity_count() {
                let at = echo_entity(&o, &e, entity);
                for slot in 0..SCRIPTS_PER_ENTITY {
                    let ours = messages(&o, entity, slot);
                    let theirs = at.map(|a| messages(&e, a, slot)).unwrap_or_default();
                    for (k, m) in ours.iter().enumerate() {
                        let Some(entry) = map.get(&(entity as u16, m.offset as u32)) else { continue };
                        total += 1;
                        let old = at.map(|_| counterpart(m, theirs.get(k)));
                        if entry.echo.is_ok() { mapped += 1; }
                        let od = usize::try_from(m.literal("dialog").unwrap_or(-1)).ok();
                        let same = |l: &EchoLine| od.and_then(|d| speaker(&o, d)).zip(speaker(&e, l.dialog)).map(|(a, b)| a == b);
                        let mut tally = |group: &'static str, l_old: Option<&EchoLine>, l_new: Option<&EchoLine>| {
                            let slot = agree.entry(group).or_default();
                            if let Some(v) = l_old.and_then(|l| same(l)) { slot[1] += 1; if v { slot[0] += 1; } }
                            if let Some(v) = l_new.and_then(|l| same(l)) { slot[3] += 1; if v { slot[2] += 1; } }
                        };
                        match (&old, &entry.echo) {
                            (Some(Ok(a)), Ok(b)) if a == b => tally("same", Some(a), Some(b)),
                            (Some(Ok(a)), Ok(b)) => tally("switched", Some(a), Some(b)),
                            (Some(Ok(a)), Err(_)) => tally("refused", Some(a), None),
                            (_, Ok(b)) => tally("gained", None, Some(b)),
                            _ => {}
                        }
                        match (old, entry.echo) {
                            (Some(Ok(old)), Ok(now)) => { positional += 1; if now != old { switched += 1; } }
                            (Some(Ok(_)), Err(_)) => { positional += 1; refused += 1; }
                            (_, Ok(_)) => gained += 1,
                            _ => {}
                        }
                    }
                }
            }
            if name == "nmkin_3" {
                let o = FieldScript::parse(&os).unwrap();
                let tutorial = messages(&o, 4, 1)[0].offset as u32;
                let line = lookup(&map, 4, tutorial, 72);
                eprintln!("nmkin_3 Jessie tutorial (dialog 72) -> {line:?}");
                assert!(line.map_or(true, |l| l.dialog != 5), "the tutorial must not take Echo-S's stuck-leg line (#5)");
            }
        }
        eprintln!("messages {total}; with an Echo-S line now {mapped}; by the old whole-script count {positional}; switched {switched}, refused {refused}, gained {gained}");
        for (group, [oa, ot, na, nt]) in &agree {
            eprintln!("  {group}: speaker agrees old {oa}/{ot}, new {na}/{nt}");
        }
    }
}
