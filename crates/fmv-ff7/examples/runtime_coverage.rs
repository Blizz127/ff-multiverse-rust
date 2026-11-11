//! How much of the owner's field scripts the live field runtime can run (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example runtime_coverage -- [--frames N] [--list FILE | FIELD...]
//! ```
//!
//! For every field (all of them, the names in `FILE` one per line, or the ones given) it reports:
//!
//! * **static**: every instruction of every script, counted by mnemonic, split into implemented and not;
//! * **dynamic**: what happened when the field ran: init plus `N` frames (default 300) with nobody controlling the
//!   player and a fresh (all-zero) story, which instructions executed, which were skipped as unsupported, how many
//!   windows opened and which commands were issued.
//!
//! Nothing is written; no game text or bytes are printed (mnemonics and counts only).
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, is_field_name, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{is_implemented, Banks, Coverage, Event, FieldRuntime};
use fmv_ff7::script::{FieldScript, SCRIPTS_PER_ENTITY};
use std::collections::BTreeMap;

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut args = std::env::args().skip(1).peekable();
    let mut frames = 300u32;
    let mut names: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--list" => {
                if let Some(f) = args.next() {
                    names.extend(std::fs::read_to_string(f).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
                }
            }
            other => names.push(other.to_string()),
        }
    }
    let mut lgp = LgpArchive::open_path(&flevel_path(&dir)).expect("open flevel.lgp");
    if names.is_empty() {
        names = lgp.list().iter().map(|e| e.name.clone()).filter(|n| is_field_name(n)).collect();
    }

    let mut static_ok: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut static_missing: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new(); // (instructions, fields)
    let mut total = Coverage::default();
    let (mut fields, mut failed, mut windows, mut broken_entities) = (0u32, 0u32, 0u64, 0u64);
    let mut commands: BTreeMap<String, u64> = BTreeMap::new();
    let mut per_field: Vec<(String, u64)> = Vec::new();

    for name in &names {
        let Ok(field) = load_field(&mut lgp, name) else {
            failed += 1;
            continue;
        };
        let Ok(section) = field.section(fmv_ff7::field::section::SCRIPT) else {
            failed += 1;
            continue;
        };
        let Ok(script) = FieldScript::parse(section) else {
            failed += 1;
            continue;
        };
        fields += 1;
        let mut missing_here: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut seen_spans = std::collections::BTreeSet::new();
        for e in 0..script.entity_count() {
            for s in 0..SCRIPTS_PER_ENTITY {
                let Some(span) = script.span(e, s) else { continue };
                if !seen_spans.insert(span.start) {
                    continue;
                }
                let Ok(d) = script.disassemble_partial(e, s) else { continue };
                for i in d.instructions {
                    if is_implemented(i.opcode) {
                        *static_ok.entry(i.mnemonic).or_default() += 1;
                    } else {
                        *missing_here.entry(i.mnemonic).or_default() += 1;
                    }
                }
            }
        }
        for (k, v) in &missing_here {
            let entry = static_missing.entry(k).or_default();
            entry.0 += v;
            entry.1 += 1;
        }
        per_field.push((name.clone(), missing_here.values().sum()));

        let Ok(mut rt) = FieldRuntime::new(section, Banks::default()) else {
            failed += 1;
            continue;
        };
        rt.init();
        for _ in 0..frames {
            rt.tick(None);
            for ev in rt.drain_events() {
                match ev {
                    Event::Window { window, .. } => {
                        windows += 1;
                        rt.close_window(window);
                        rt.answer(window, 0);
                    }
                    Event::Command(c) => {
                        let key = format!("{c:?}").split([' ', '{']).next().unwrap_or("?").to_string();
                        *commands.entry(key).or_default() += 1;
                        if c.returns_to_field() {
                            rt.complete_command();
                        }
                    }
                    _ => {}
                }
            }
            if rt.command().is_some_and(|c| !c.returns_to_field()) {
                break;
            }
        }
        broken_entities += (0..rt.entity_count()).filter(|&e| rt.entity_state(e).is_none()).count() as u64;
        total.merge(rt.coverage());
    }

    let ok: u64 = static_ok.values().sum();
    let miss: u64 = static_missing.values().map(|v| v.0).sum();
    println!("fields: {fields} (failed to load: {failed}); frames per field: {frames}");
    println!("static instructions: {} implemented, {} not ({:.1}% implemented)", ok, miss, 100.0 * ok as f64 / (ok + miss).max(1) as f64);
    println!("\nnot implemented, by instructions (fields):");
    let mut v: Vec<_> = static_missing.iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (k, (n, f)) in v {
        println!("  {k:<10} {n:>7} ({f})");
    }
    let exec: u64 = total.executed.values().sum();
    let unsup: u64 = total.unsupported.values().sum();
    println!("\ndynamic: {exec} executed, {unsup} skipped as unsupported, {} undecodable, {} anomalies, {windows} windows, {broken_entities} broken entities", total.undecodable, total.anomalies);
    println!("commands: {commands:?}");
    let mut u: Vec<_> = total.unsupported.iter().collect();
    u.sort_by(|a, b| b.1.cmp(a.1));
    println!("top skipped at run time: {:?}", u.iter().take(25).collect::<Vec<_>>());
    per_field.sort_by(|a, b| b.1.cmp(&a.1));
    println!("fields with the most unimplemented instructions: {:?}", per_field.iter().take(10).collect::<Vec<_>>());
}
