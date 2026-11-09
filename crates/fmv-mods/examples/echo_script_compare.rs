//! Compares Echo-S's replacement field scripts with the original game's, field by field, and prints counts only
//! (never a byte of either, never a line of text).
//!
//! ```text
//! FMV_FF7_DATA=<.../data>  (the folder holding field/flevel.lgp)
//! cargo run --offline --release -p fmv-mods --example echo_script_compare -- <Echo-s.iro> [--verbose]
//! ```
//!
//! Both are opened read-only. For every field that both have, the script section (section 0 of the field file)
//! is decoded and compared entity by entity (matched by position when the original's entities are a prefix of
//! Echo-S's, else by name) and script by script (slot by slot), instruction by instruction, ignoring only the
//! absolute offsets and the `dialog` operand of a message.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::{FieldScript, Instruction, SCRIPTS_PER_ENTITY};
use fmv_mods::IroArchive;
use std::collections::{BTreeMap, BTreeSet};

fn load(bytes: &[u8]) -> Result<FieldFile, String> {
    let raw = lzss::decompress(bytes).map_err(|e| e.to_string())?;
    FieldFile::parse(raw).map_err(|e| e.to_string())
}

/// A comparable image of an instruction: everything but absolute offsets and the dialog operand of a message.
fn image(ins: &Instruction) -> String {
    let mut s = format!("{:02X}", ins.opcode);
    if let Some((code, _)) = ins.sub {
        s.push_str(&format!(".{code:02X}"));
    }
    for o in &ins.operands {
        if (ins.mnemonic == "MESSAGE" || ins.mnemonic == "ASK") && o.name == "dialog" {
            continue;
        }
        s.push_str(&format!(" {}={}:{}", o.name, o.bank, o.value));
    }
    if !ins.payload.is_empty() {
        s.push_str(&format!(" payload{}", ins.payload.len()));
    }
    s
}

/// Whether the dialogue walker interprets this opcode (everything else is a no-op to it: movement, animation,
/// sound, camera, windows' geometry, palettes, menus and money never change what is said).
fn walker_op(op: u8) -> bool {
    matches!(
        op,
        0x00..=0x03 | 0x07 | 0x10..=0x19 | 0x20 | 0x30..=0x32 | 0x39 | 0x40 | 0x48 | 0x58 | 0x5B | 0x60 | 0x70 | 0x74 | 0x76..=0x7E | 0x80..=0x9C | 0xA4 | 0xCB | 0xCC | 0xFF
    )
}

/// The walker's view of a script: its opcodes only, a jump's target named by the index of the first walker
/// instruction at or after it. Returns the images and, for each message instruction, `(index, dialog)`.
fn walker_stream(instructions: &[Instruction]) -> (Vec<String>, Vec<(usize, i32)>) {
    let kept: Vec<&Instruction> = instructions.iter().filter(|i| walker_op(i.opcode)).collect();
    let mut images = Vec::new();
    let mut messages = Vec::new();
    for (k, ins) in kept.iter().enumerate() {
        let mut s = image(ins);
        if let Some(target) = ins.jump_target {
            let idx = kept.iter().position(|j| j.offset >= target).unwrap_or(kept.len());
            // The jump operand itself is a distance in bytes: replace it with the walker index.
            s = s.split(' ').filter(|part| !part.starts_with("jump=")).collect::<Vec<_>>().join(" ");
            s.push_str(&format!(" ->{idx}"));
        }
        if ins.mnemonic == "MESSAGE" || ins.mnemonic == "ASK" {
            if let Some(d) = ins.literal("dialog") {
                messages.push((k, d));
            }
        }
        images.push(s);
    }
    (images, messages)
}

#[derive(Default)]
struct Totals {
    inserted_ops: BTreeMap<String, usize>,
    deleted_ops: BTreeMap<String, usize>,
    edit_kinds: BTreeMap<String, usize>,
    walker_scripts: usize,
    walker_equivalent: usize,
    walker_differ_first: BTreeMap<String, usize>,
    messages_in_equivalent: usize,
    messages_in_other: usize,
    pairs_same: usize,
    pairs_changed: usize,
    fields_all_equivalent: usize,
    fields_all_equivalent_same_numbering: usize,
    fields_both: usize,
    unreadable: usize,
    entity_delta: BTreeMap<i64, usize>,
    names_prefix_equal: usize,
    names_not_prefix: usize,
    dialog_delta: BTreeMap<i64, usize>,
    scripts_compared: usize,
    scripts_identical: usize,
    scripts_msg_operand_only: usize,
    scripts_differ: usize,
    scripts_one_side_only: usize,
    first_diff: BTreeMap<String, usize>,
    fields_logic_identical_on_common_entities: usize,
    fields_with_message_operand_changes: usize,
    map_not_function: usize,
    dialogs_orig: usize,
    dialogs_text_differs_by_index: usize,
    asks_orig: usize,
    asks_choice_range_differs: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(iro_path) = args.next() else {
        eprintln!("usage: echo_script_compare <Echo-s.iro> [--verbose]   (with FMV_FF7_DATA set)");
        std::process::exit(2);
    };
    let verbose = args.any(|a| a == "--verbose");
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set");
        std::process::exit(2);
    };
    let flevel = std::path::Path::new(&data).join("field").join("flevel.lgp");
    let mut orig = LgpArchive::open_path(&flevel).expect("open the original flevel.lgp");
    let echo = IroArchive::open_path(&iro_path).expect("open the Echo-S archive");

    let origin_names: BTreeSet<String> = orig.list().iter().map(|e| e.name.to_ascii_lowercase()).filter(|n| !n.contains('.') && n != "maplist").collect();
    let echo_names: BTreeSet<String> = echo
        .entries()
        .iter()
        .filter(|e| e.folder().eq_ignore_ascii_case("Echo-S\\flevel.lgp"))
        .map(|e| e.file_name().to_ascii_lowercase())
        .collect();
    println!("original fields: {}  Echo-S flevel entries: {}  both: {}", origin_names.len(), echo_names.len(), origin_names.intersection(&echo_names).count());

    let mut t = Totals::default();
    for name in origin_names.intersection(&echo_names) {
        t.fields_both += 1;
        let Ok(o_bytes) = orig.read(name) else {
            t.unreadable += 1;
            continue;
        };
        let Ok(e_bytes) = echo.read_named(&format!("Echo-S\\flevel.lgp\\{name}")) else {
            t.unreadable += 1;
            continue;
        };
        let (Ok(of), Ok(ef)) = (load(&o_bytes), load(&e_bytes)) else {
            t.unreadable += 1;
            continue;
        };
        let (Ok(os), Ok(es)) = (FieldScript::from_field(&of), FieldScript::from_field(&ef)) else {
            t.unreadable += 1;
            continue;
        };
        t.dialogs_orig += os.dialogs().len();
        *t.entity_delta.entry(es.entity_count() as i64 - os.entity_count() as i64).or_default() += 1;
        *t.dialog_delta.entry(es.dialogs().len() as i64 - os.dialogs().len() as i64).or_default() += 1;

        // Match entities: by position when the original's names are a prefix of Echo-S's, else by name.
        let n = os.entity_count();
        let prefix = es.entity_count() >= n && (0..n).all(|i| os.entity_name(i) == es.entity_name(i));
        if prefix {
            t.names_prefix_equal += 1;
        } else {
            t.names_not_prefix += 1;
        }
        let pairs: Vec<(usize, usize)> = if prefix {
            (0..n).map(|i| (i, i)).collect()
        } else {
            let mut used = BTreeSet::new();
            (0..n)
                .filter_map(|i| {
                    let nm = os.entity_name(i)?;
                    let j = (0..es.entity_count()).find(|j| !used.contains(j) && es.entity_name(*j) == Some(nm))?;
                    used.insert(j);
                    Some((i, j))
                })
                .collect()
        };
        let mut all_identical = pairs.len() == n;
        let mut any_msg_change = false;
        let mut field_all_equivalent = pairs.len() == n;
        let mut field_same_numbering = true;
        let mut map: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for &(i, j) in &pairs {
            for slot in 0..SCRIPTS_PER_ENTITY {
                let (a, b) = (os.disassemble_partial(i, slot), es.disassemble_partial(j, slot));
                let (a, b) = match (a, b) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(_), Err(_)) => continue,
                    _ => {
                        t.scripts_one_side_only += 1;
                        all_identical = false;
                        field_all_equivalent = false;
                        continue;
                    }
                };
                // The walker's view of the two scripts.
                {
                    let (wa, ma) = walker_stream(&a.instructions);
                    let (wb, mb) = walker_stream(&b.instructions);
                    t.walker_scripts += 1;
                    if wa == wb {
                        t.walker_equivalent += 1;
                        t.messages_in_equivalent += ma.len();
                        for ((_, p), (_, q)) in ma.iter().zip(&mb) {
                            if p == q {
                                t.pairs_same += 1;
                            } else {
                                t.pairs_changed += 1;
                                field_same_numbering = false;
                            }
                        }
                    } else {
                        t.messages_in_other += ma.len();
                        field_all_equivalent = false;
                        if !ma.is_empty() || !mb.is_empty() {
                            // Edit script by LCS over the opcode+operand images without jump targets.
                            let strip = |v: &Vec<String>| -> Vec<String> { v.iter().map(|s| s.split(" ->").next().unwrap_or("").to_string()).collect() };
                            let (sa, sb) = (strip(&wa), strip(&wb));
                            let mut dp = vec![vec![0u16; sb.len() + 1]; sa.len() + 1];
                            for x in (0..sa.len()).rev() {
                                for y in (0..sb.len()).rev() {
                                    dp[x][y] = if sa[x] == sb[y] { dp[x + 1][y + 1] + 1 } else { dp[x + 1][y].max(dp[x][y + 1]) };
                                }
                            }
                            let (mut x, mut y) = (0, 0);
                            let (mut ins_n, mut del_n) = (0usize, 0usize);
                            let name_of = |s: &str| s.split(' ').next().unwrap_or("").to_string();
                            while x < sa.len() || y < sb.len() {
                                if x < sa.len() && y < sb.len() && sa[x] == sb[y] {
                                    x += 1;
                                    y += 1;
                                } else if y < sb.len() && (x == sa.len() || dp[x][y + 1] >= dp[x + 1][y]) {
                                    ins_n += 1;
                                    *t.inserted_ops.entry(name_of(&sb[y])).or_default() += 1;
                                    y += 1;
                                } else {
                                    del_n += 1;
                                    *t.deleted_ops.entry(name_of(&sa[x])).or_default() += 1;
                                    x += 1;
                                }
                            }
                            let kind = match (ins_n > 0, del_n > 0) {
                                (true, false) => "message-bearing: Echo-S only inserts walker ops",
                                (false, true) => "message-bearing: Echo-S only deletes walker ops",
                                _ => "message-bearing: inserts and deletes",
                            };
                            *t.edit_kinds.entry(kind.to_string()).or_default() += 1;
                            if ma.len() != mb.len() {
                                *t.edit_kinds.entry("  ... and the number of MESSAGE/ASK differs".to_string()).or_default() += 1;
                            }
                        }
                        let first = wa.iter().zip(&wb).position(|(x, y)| x != y);
                        let key = match first {
                            Some(k) => {
                                let m = |s: &String| s.split(' ').next().unwrap_or("").to_string();
                                format!("first walker difference: {} vs {}", m(wa.get(k).unwrap_or(&String::new())), m(wb.get(k).unwrap_or(&String::new())))
                            }
                            None => format!("walker streams differ in length ({} vs {})", wa.len(), wb.len()),
                        };
                        *t.walker_differ_first.entry(key).or_default() += 1;
                    }
                }
                t.scripts_compared += 1;
                if a.instructions.len() != b.instructions.len() {
                    t.scripts_differ += 1;
                    all_identical = false;
                    *t.first_diff.entry("instruction count differs".into()).or_default() += 1;
                    continue;
                }
                let mut differs = None;
                let mut msg_only = false;
                for (x, y) in a.instructions.iter().zip(&b.instructions) {
                    if image(x) != image(y) {
                        differs = Some(format!("{} -> {}", x.mnemonic, y.mnemonic));
                        break;
                    }
                    if x.mnemonic == "MESSAGE" || x.mnemonic == "ASK" {
                        if let (Some(p), Some(q)) = (x.literal("dialog"), y.literal("dialog")) {
                            map.entry(p).or_default().insert(q);
                            if p != q {
                                msg_only = true;
                            }
                        }
                    }
                }
                match differs {
                    Some(d) => {
                        t.scripts_differ += 1;
                        all_identical = false;
                        *t.first_diff.entry(d).or_default() += 1;
                    }
                    None if msg_only => {
                        t.scripts_msg_operand_only += 1;
                        any_msg_change = true;
                    }
                    None => t.scripts_identical += 1,
                }
            }
        }
        if all_identical {
            t.fields_logic_identical_on_common_entities += 1;
        }
        if field_all_equivalent {
            t.fields_all_equivalent += 1;
            if field_same_numbering && os.dialogs().len() == es.dialogs().len() {
                t.fields_all_equivalent_same_numbering += 1;
            }
        }
        if any_msg_change {
            t.fields_with_message_operand_changes += 1;
        }
        if map.values().any(|v| v.len() > 1) {
            t.map_not_function += 1;
        }
        for i in 0..os.dialogs().len() {
            let a = os.dialogs().get(i).and_then(|d| os.bytes().get(d.offset..d.offset + d.len));
            let b = es.dialogs().get(i).and_then(|d| es.bytes().get(d.offset..d.offset + d.len));
            if a != b {
                t.dialogs_text_differs_by_index += 1;
            }
        }
        if verbose && !all_identical {
            println!("  {name}: entities {} -> {} dialogs {} -> {} prefix {prefix}", os.entity_count(), es.entity_count(), os.dialogs().len(), es.dialogs().len());
        }
    }
    println!("fields compared    : {} (unreadable {})", t.fields_both, t.unreadable);
    println!("entity count delta (echo - orig) -> fields: {:?}", t.entity_delta);
    println!("dialog count delta (echo - orig) -> fields: {:?}", t.dialog_delta);
    println!("original entities a name-prefix of Echo-S's: {}   not: {}", t.names_prefix_equal, t.names_not_prefix);
    println!("scripts compared on matched entities : {}", t.scripts_compared);
    println!("  identical instruction streams      : {}", t.scripts_identical);
    println!("  identical but a MESSAGE/ASK operand : {}", t.scripts_msg_operand_only);
    println!("  differ                             : {}   (one side only: {})", t.scripts_differ, t.scripts_one_side_only);
    println!("fields whose matched entities are all identical in logic (message operands aside): {}", t.fields_logic_identical_on_common_entities);
    println!("fields with MESSAGE/ASK operand changes: {}   with a non-function map: {}", t.fields_with_message_operand_changes, t.map_not_function);
    println!("dialogs (original): {}   text bytes differ at the same index: {}", t.dialogs_orig, t.dialogs_text_differs_by_index);
    println!("--- the walker's view (only the opcodes the dialogue walker interprets, jumps by walker index) ---");
    println!("scripts compared               : {}", t.walker_scripts);
    println!("  walker-equivalent            : {}", t.walker_equivalent);
    println!("  walker-different             : {}", t.walker_scripts - t.walker_equivalent);
    println!("MESSAGE/ASK ops (original) in equivalent scripts: {}   in other scripts: {}", t.messages_in_equivalent, t.messages_in_other);
    println!("  in equivalent scripts: same dialog number {}  changed number {}", t.pairs_same, t.pairs_changed);
    println!("fields whose every matched script is walker-equivalent: {} (of which dialog numbering and count are the same: {})", t.fields_all_equivalent, t.fields_all_equivalent_same_numbering);
    println!("message-bearing scripts that differ in walker ops: {:?}", t.edit_kinds);
    let top = |m: &BTreeMap<String, usize>| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter().take(14).map(|(k, n)| format!("{k}:{n}")).collect::<Vec<_>>().join(" ")
    };
    println!("  inserted by Echo-S (opcode:count): {}", top(&t.inserted_ops));
    println!("  deleted by Echo-S  (opcode:count): {}", top(&t.deleted_ops));
    let mut wd: Vec<_> = t.walker_differ_first.iter().collect();
    wd.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in wd.iter().take(20) {
        println!("  {v:6}  {k}");
    }
    let mut diffs: Vec<_> = t.first_diff.iter().collect();
    diffs.sort_by(|a, b| b.1.cmp(a.1));
    println!("first difference of a differing script, by mnemonic pair (top 25):");
    for (k, v) in diffs.iter().take(25) {
        println!("  {v:6}  {k}");
    }
    let _ = (t.asks_orig, t.asks_choice_range_differs);
}
