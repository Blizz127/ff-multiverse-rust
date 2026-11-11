//! Read-only instruction checker used by `scripts/validate-disc1-manifest.py`.
//!
//! Reuses the production LGP, LZSS, field, maplist and instruction decoders. The Python
//! wrapper reads JSON and sends one numeric expectation per tab-separated stdin line:
//! field, maplist id, entity, slot, section offset, mnemonic, then name=bank:value operands.
//! Prints only counts and mismatch metadata; never writes or emits asset payloads.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{FieldScript, Maplist};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::process::ExitCode;
use std::str::FromStr;

struct Expected {
    line: usize,
    field_id: u16,
    entity: usize,
    slot: usize,
    offset: usize,
    op: String,
    operands: Vec<(String, u8, i32)>,
}

fn number<T: FromStr>(s: &str, line: usize) -> Result<T, String> {
    s.parse().map_err(|_| format!("invalid number on expectation line {line}"))
}

fn check() -> Result<bool, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: manifest_check OWNED_FLEVEL_LGP < expectations.tsv; use scripts/validate-disc1-manifest.py".into());
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())?;
    let mut groups: BTreeMap<String, Vec<Expected>> = BTreeMap::new();
    for (index, row) in input.lines().enumerate() {
        let line = index + 1;
        let parts: Vec<_> = row.split('\t').collect();
        if parts.len() < 6 {
            return Err(format!("short expectation line {line}"));
        }
        let mut operands = Vec::new();
        for raw in &parts[6..] {
            let (name, value) = raw.split_once('=').ok_or_else(|| format!("bad operand on line {line}"))?;
            let (bank, value) = value.split_once(':').ok_or_else(|| format!("bad operand on line {line}"))?;
            operands.push((name.to_string(), number(bank, line)?, number(value, line)?));
        }
        groups.entry(parts[0].to_string()).or_default().push(Expected {
            line, field_id: number(parts[1], line)?, entity: number(parts[2], line)?,
            slot: number(parts[3], line)?, offset: number(parts[4], line)?, op: parts[5].to_string(), operands,
        });
    }
    if groups.is_empty() {
        return Err("no instruction expectations supplied".into());
    }
    let mut archive = LgpArchive::open_path(&args[0]).map_err(|e| e.to_string())?;
    let maplist = Maplist::from_archive(&mut archive).map_err(|e| e.to_string())?;
    let (mut references, mut resolved, mut mismatches) = (0usize, 0usize, 0usize);
    for (field, sites) in groups {
        let raw = archive.read(&field).map_err(|e| format!("{field}: {e}"))?;
        let bytes = fmv_ff7::lzss::decompress(&raw).map_err(|e| format!("{field}: {e}"))?;
        let file = FieldFile::parse(bytes).map_err(|e| format!("{field}: {e}"))?;
        let script = FieldScript::from_field(&file).map_err(|e| format!("{field}: {e}"))?;
        let mut decoded = BTreeMap::new();
        for site in sites {
            references += 1;
            let mut fail = |reason: String| {
                mismatches += 1;
                eprintln!("reference {}: {field} entity {} slot {} at {:#06x}: {reason}", site.line, site.entity, site.slot, site.offset);
            };
            if maplist.id_of(&field) != Some(site.field_id) {
                fail(format!("maplist id {:?}, expected {}", maplist.id_of(&field), site.field_id));
            }
            let code = decoded.entry((site.entity, site.slot)).or_insert_with(|| script.disassemble(site.entity, site.slot));
            let code = match code {
                Ok(code) => code,
                Err(e) => { fail(format!("cannot decode script: {e}")); continue; }
            };
            let Some(ins) = code.iter().find(|ins| ins.offset == site.offset) else {
                fail("instruction offset not found in this script".into());
                continue;
            };
            resolved += 1;
            if ins.mnemonic != site.op {
                fail(format!("opcode {}, expected {}", ins.mnemonic, site.op));
            }
            for (name, bank, value) in &site.operands {
                match ins.operand(name) {
                    Some(actual) if (actual.bank, actual.value) == (*bank, *value) => {}
                    Some(actual) => fail(format!("{name}={}:{}, expected {bank}:{value}", actual.bank, actual.value)),
                    None => fail(format!("operand {name} not found")),
                }
            }
        }
    }
    println!("{references} instruction references checked; {resolved} resolved; {mismatches} mismatches");
    Ok(mismatches == 0)
}

fn main() -> ExitCode {
    match check() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => { eprintln!("manifest_check: {e}"); ExitCode::from(2) }
    }
}
