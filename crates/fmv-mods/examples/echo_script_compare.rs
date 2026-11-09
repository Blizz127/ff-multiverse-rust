//! Compares Echo-S's replacement field scripts with the original game's, field by field, and prints counts only
//! (never a byte of either, never a line of text).
//!
//! ```text
//! FMV_FF7_DATA=<.../data>  (the folder holding field/flevel.lgp)
//! cargo run --offline --release -p fmv-mods --example echo_script_compare -- <Echo-s.iro> [field ...]
//! ```
//!
//! Both are opened read-only. For every field both have, the script sections are compared in what the dialogue walker can see
//! ([`fmv_mods::compare::align`]): which entities have a namesake, which scripts are the same to the walker, which dialog
//! numbers the original's scripts open and how many of those sit in a script that is the same in Echo-S's, and under which
//! number. Without field names it walks all 702 fields; with some, only those, one line each.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::FieldScript;
use fmv_mods::compare::{align, message_lines, Alignment};
use fmv_mods::IroArchive;
use std::collections::BTreeSet;

fn section_of(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let raw = lzss::decompress(bytes).map_err(|e| e.to_string())?;
    let file = FieldFile::parse(raw).map_err(|e| e.to_string())?;
    Ok(FieldScript::from_field(&file).map_err(|e| e.to_string())?.bytes().to_vec())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(iro_path) = args.next() else {
        eprintln!("usage: echo_script_compare <Echo-s.iro> [field ...]   (with FMV_FF7_DATA set)");
        std::process::exit(2);
    };
    let only: Vec<String> = args.collect();
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set");
        std::process::exit(2);
    };
    let flevel = std::path::Path::new(&data).join("field").join("flevel.lgp");
    let mut orig = LgpArchive::open_path(&flevel).expect("open the original flevel.lgp");
    let echo = IroArchive::open_path(&iro_path).expect("open the Echo-S archive");
    let names: BTreeSet<String> = orig.list().iter().map(|e| e.name.to_ascii_lowercase()).filter(|n| !n.contains('.') && n != "maplist").collect();

    let mut total = Alignment::default();
    let (mut fields, mut unreadable) = (0usize, 0usize);
    let (mut dialogs_orig, mut dialogs_echo, mut lines_orig, mut lines_echo) = (0usize, 0usize, 0usize, 0usize);
    for name in names.iter().filter(|n| only.is_empty() || only.contains(n)) {
        let (Ok(ob), Ok(eb)) = (orig.read(name), echo.read_named(&format!("Echo-S\\flevel.lgp\\{name}"))) else {
            unreadable += 1;
            continue;
        };
        let (Ok(os), Ok(es)) = (section_of(&ob), section_of(&eb)) else {
            unreadable += 1;
            continue;
        };
        let (Ok(o), Ok(e)) = (FieldScript::parse(&os), FieldScript::parse(&es)) else {
            unreadable += 1;
            continue;
        };
        fields += 1;
        let a = align(&o, &e);
        let (lo, le) = (message_lines(&o).len(), message_lines(&e).len());
        dialogs_orig += o.dialogs().len();
        dialogs_echo += e.dialogs().len();
        lines_orig += lo;
        lines_echo += le;
        if !only.is_empty() {
            println!(
                "{name}: dialogs {} -> {}; message lines opened {lo} -> {le}; entities {}/{} matched; scripts {}/{} the same to the walker; original lines matched {}/{} (same number {})",
                o.dialogs().len(),
                e.dialogs().len(),
                a.entities_matched,
                a.entities,
                a.scripts_equivalent,
                a.scripts,
                a.lines_matched,
                a.original_lines,
                a.lines_same_number
            );
        }
        total.entities += a.entities;
        total.entities_matched += a.entities_matched;
        total.scripts += a.scripts;
        total.scripts_equivalent += a.scripts_equivalent;
        total.original_lines += a.original_lines;
        total.lines_matched += a.lines_matched;
        total.lines_same_number += a.lines_same_number;
    }
    println!("fields compared {fields} (unreadable {unreadable})");
    println!("dialog strings: original {dialogs_orig}, Echo-S {dialogs_echo}");
    println!("message lines opened by scripts (distinct window+dialog): original {lines_orig}, Echo-S {lines_echo}");
    println!("entities of the original with a namesake in Echo-S: {}/{}", total.entities_matched, total.entities);
    println!("scripts of those entities that are the same to the walker: {}/{}", total.scripts_equivalent, total.scripts);
    println!(
        "dialog numbers the original opens: {}; in a script that is the same in Echo-S: {} ({} under the same number, {} renumbered)",
        total.original_lines,
        total.lines_matched,
        total.lines_same_number,
        total.lines_matched - total.lines_same_number
    );
}
