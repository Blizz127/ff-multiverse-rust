//! Survey of how the FF7 field scripts read and write the story's main progress counter (the "game
//! moment", `Var[2][0]`; needs the owner's data, skips otherwise). Prints field names, offsets and
//! small numbers only (never game text), and writes nothing.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example story_survey -j8 -- [FIELD...]
//! ```
//!
//! Without field names it walks all fields and prints one line per field that writes or compares the
//! counter, then the totals; with names it prints every access of those fields.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, is_field_name, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::FieldScript;
use std::collections::BTreeMap;

/// The counter's variable: bank 2, address 0.
const MOMENT: (u8, u8) = (2, 0);

#[derive(Default)]
struct Access {
    writes: Vec<(usize, usize, usize, String)>,
    compares: Vec<(usize, usize, usize, String)>,
}

fn is_moment(o: &fmv_ff7::script::Operand) -> bool {
    o.bank == MOMENT.0 && (o.value & 0xFF) as u8 == MOMENT.1
}

fn survey_field(name: &str, field: &fmv_ff7::field::FieldFile) -> Result<Access, String> {
    let script = FieldScript::from_field(field).map_err(|e| e.to_string())?;
    let mut out = Access::default();
    for entity in 0..script.entity_count() {
        for slot in 0..fmv_ff7::script::SCRIPTS_PER_ENTITY {
            let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
            for ins in &d.instructions {
                match ins.mnemonic {
                    "SETBYTE" | "SETWORD" | "PLUS" | "PLUS2" | "MINUS" | "MINUS2" | "PLUS!" | "PLUS2!" | "MINUS!" | "MINUS2!" | "BITON"
                    | "BITOFF" | "INC" | "INC2" | "DEC" | "DEC2" | "INC!" | "INC2!" | "DEC!" | "DEC2!" => {
                        let var = ins.operand("var");
                        if var.is_some_and(is_moment) {
                            let text = ins.to_string();
                            out.writes.push((entity, slot, ins.offset, text));
                        }
                    }
                    "IFUB" | "IFUBL" | "IFSW" | "IFSWL" | "IFUW" | "IFUWL" => {
                        let (l, r) = (ins.operand("left"), ins.operand("right"));
                        if l.is_some_and(is_moment) || r.is_some_and(is_moment) {
                            out.compares.push((entity, slot, ins.offset, ins.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = name;
    Ok(out)
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let path = flevel_path(&dir);
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let names: Vec<String> = if args.is_empty() {
        let mut v: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).filter(|n| is_field_name(n)).collect();
        v.sort();
        v
    } else {
        args.clone()
    };
    let verbose = !args.is_empty();
    let mut total_writes = 0usize;
    let mut fields_writing = 0usize;
    let mut fields_comparing = 0usize;
    let mut written_values: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for name in &names {
        let field = match load_field(&mut lgp, name) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{name}: {e}");
                continue;
            }
        };
        let acc = match survey_field(name, &field) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{name}: {e}");
                continue;
            }
        };
        if acc.writes.is_empty() && acc.compares.is_empty() {
            continue;
        }
        fields_writing += usize::from(!acc.writes.is_empty());
        fields_comparing += usize::from(!acc.compares.is_empty());
        total_writes += acc.writes.len();
        for (_, _, _, text) in &acc.writes {
            let kind = text.split_whitespace().nth(1).unwrap_or("?").to_string();
            *kinds.entry(kind).or_default() += 1;
            *written_values.entry(text.split("value=").nth(1).unwrap_or("?").to_string()).or_default() += 1;
        }
        if verbose {
            println!("=== {name}: {} writes, {} comparisons", acc.writes.len(), acc.compares.len());
            for (e, s, _, text) in &acc.writes {
                println!("  write   entity {e} script {s}: {text}");
            }
            for (e, s, _, text) in &acc.compares {
                println!("  compare entity {e} script {s}: {text}");
            }
        } else {
            let values: Vec<String> = acc.writes.iter().map(|w| w.3.split("value=").nth(1).unwrap_or("?").to_string()).collect();
            println!("{name}: writes {} {:?} comparisons {}", acc.writes.len(), values, acc.compares.len());
        }
    }
    println!("fields: {} scanned, {fields_writing} write the counter, {fields_comparing} compare it, {total_writes} writes", names.len());
    println!("write kinds: {kinds:?}");
    println!("distinct written operands: {}", written_values.len());
}
