//! Verification dump of real FF7 PC field scripts (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example script_dump -j8 -- md1stin [md1_1 ...]
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example script_dump -j8 -- md1stin --disasm [ENTITY [SCRIPT]]
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example script_dump -j8 -- md1stin --dialogs
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example script_dump -j8 -- --survey
//! ```
//!
//! For each named field it prints the script header, the entities with their script-0 analysis
//! (model, placement, direction, visibility, player binding), the gateways joined with the
//! maplist names, the `MAPJUMP`s and a check of every placement against the walkmesh.
//! `--disasm` prints instruction listings (every distinct script when no entity is given);
//! `--dialogs` prints the decoded dialog text **to the terminal only** (game text: never copy
//! it into the repository). `--survey` walks all fields and prints the totals. Nothing is
//! written to disk.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field, survey_all};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{
    check_placement, parse_model_list, resolve_gateways, Arg, FieldScript, Maplist, ModelList, Phase, Tracked, WalkmeshFit, SCRIPTS_PER_ENTITY,
};
use std::collections::BTreeSet;

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: script_dump <field>... [--disasm [ENTITY [SCRIPT]]] [--dialogs] | --survey");
        return;
    }
    let path = flevel_path(&dir);
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    if args.first().map(String::as_str) == Some("--survey") {
        print_survey(&mut lgp);
        return;
    }
    let maplist = Maplist::from_archive(&mut lgp).ok();
    let mut fields = Vec::new();
    let (mut disasm, mut dialogs) = (None, false);
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--disasm" => {
                let e = args.get(i + 1).and_then(|s| s.parse::<usize>().ok());
                let s = e.and_then(|_| args.get(i + 2)).and_then(|s| s.parse::<usize>().ok());
                i += usize::from(e.is_some()) + usize::from(s.is_some());
                disasm = Some((e, s));
            }
            "--dialogs" => dialogs = true,
            name => fields.push(name.to_string()),
        }
        i += 1;
    }
    for name in &fields {
        if let Err(e) = dump_field(&mut lgp, name, maplist.as_ref(), disasm, dialogs) {
            eprintln!("{name}: {e}");
        }
    }
}

fn arg_u8(a: &Arg<u8>) -> String {
    match a {
        Arg::Literal(v) => v.to_string(),
        Arg::Variable { bank, address } => format!("Var[{bank}][{address}]"),
        Arg::Absent => "-".into(),
    }
}

fn arg_i16(a: &Arg<i16>) -> String {
    match a {
        Arg::Literal(v) => v.to_string(),
        Arg::Variable { bank, address } => format!("Var[{bank}][{address}]"),
        Arg::Absent => "-".into(),
    }
}

fn one(t: &Option<Tracked<Arg<u8>>>) -> String {
    t.as_ref().map_or("-".to_string(), |t| {
        format!("{}{}{}", arg_u8(&t.value), if t.conditional { "?" } else { "" }, if t.phase == Phase::Main { "m" } else { "" })
    })
}

fn dump_field(
    lgp: &mut LgpArchive<std::fs::File>,
    name: &str,
    maplist: Option<&Maplist>,
    disasm: Option<(Option<usize>, Option<usize>)>,
    dialogs: bool,
) -> Result<(), String> {
    println!("=== {name}");
    let field = load_field(lgp, name)?;
    let script = FieldScript::from_field(&field).map_err(|e| e.to_string())?;
    let h = script.header();
    println!(
        "script section {} bytes: version {}/{} entities {} models {} scale {} creator {:?} name {:?}",
        script.bytes().len(),
        h.data_version,
        h.event_version,
        h.entity_count,
        h.model_count,
        h.scale,
        h.creator,
        h.name
    );
    let layout = script.layout();
    println!(
        "code {:#06x}..{:#06x}, dialogs {} (table at {:#06x}{}), AKAO/tutorial blocks {}",
        layout.code_start,
        layout.code_end,
        script.dialogs().len(),
        layout.dialog_table,
        script.dialog_error().map_or(String::new(), |e| format!(", ERROR {e}")),
        script.akao_blocks().len()
    );
    for b in script.akao_blocks() {
        println!("  block {} at {:#06x} len {} tag {:?}", b.index, b.offset, b.len, String::from_utf8_lossy(&b.tag));
    }

    let models: Option<ModelList> = field.section(2).ok().and_then(|s| parse_model_list(s).ok());
    if let Some(m) = &models {
        println!(
            "model loader: {} models declared, {} read, {} bytes covered of {}",
            m.declared,
            m.models.len(),
            m.covered,
            field.section(2).map(|s| s.len()).unwrap_or(0)
        );
    }
    let mesh = field.walkmesh().map_err(|e| e.to_string())?;
    println!("walkmesh: {} triangles", mesh.len());

    println!("entities (suffix ? = only on some paths, m = in the main part of script 0):");
    for init in &script.entity_inits(models.as_ref()) {
        let code_slots = (0..SCRIPTS_PER_ENTITY).filter(|&s| script.span(init.entity, s).is_some()).count();
        let model = match (init.model_index, &init.model_ref, &init.char_id) {
            (Some(i), Some(r), Some(c)) => format!("CHAR({}) -> model #{i} {} ({})", arg_u8(&c.value), r.hrc, r.name),
            (Some(i), None, Some(c)) => format!("CHAR({}) -> model #{i}", arg_u8(&c.value)),
            _ => "-".into(),
        };
        println!(
            "  [{:2}] {:<8} slots {:2} | {model} | PC {} | dir {} visi {} solid {} talk {} slide {}{}",
            init.entity,
            init.name,
            code_slots,
            one(&init.player),
            one(&init.direction),
            one(&init.visible),
            one(&init.solid),
            one(&init.talk_radius),
            one(&init.solid_radius),
            init.decode_error.as_ref().map_or(String::new(), |e| format!(" DECODE ERROR: {e}")),
        );
        for p in &init.placements {
            let tri = match p.triangle {
                Arg::Literal(t) => t.to_string(),
                Arg::Variable { bank, address } => format!("Var[{bank}][{address}]"),
                Arg::Absent => "-".into(),
            };
            let fit = match check_placement(&mesh, p) {
                Some(WalkmeshFit::Inside { height_error }) => format!("on triangle (dz {})", height_error.map_or("n/a".into(), |h| format!("{h:.1}"))),
                Some(WalkmeshFit::Outside { distance, containing }) => format!("OUTSIDE by {distance:.1} (inside {containing:?})"),
                Some(WalkmeshFit::NoSuchTriangle) => "NO SUCH TRIANGLE".into(),
                Some(WalkmeshFit::Unnamed { found }) => format!("no triangle named; point is in {found:?}"),
                None => "variable operands".into(),
            };
            println!(
                "         {:?} x {} y {} z {} tri {} @{:#06x}{}{} -> {fit}",
                p.kind,
                arg_i16(&p.x),
                arg_i16(&p.y),
                arg_i16(&p.z),
                tri,
                p.offset,
                if p.conditional { " (conditional)" } else { "" },
                if p.phase == Phase::Main { " (main part)" } else { "" }
            );
        }
    }
    let players = script.player_entities();
    println!("player-bound entities (entity, character id): {players:?}");

    // gateways
    if let (Some(maplist), Ok(triggers)) = (maplist, field.triggers()) {
        let gws = resolve_gateways(&triggers, maplist);
        println!("gateways ({}):", gws.len());
        for g in gws {
            println!(
                "  slot {:2}: -> {} (id {}) arrive x {} y {} triangle {:?}",
                g.slot,
                g.target_field.as_deref().unwrap_or("?"),
                g.target_field_id,
                g.destination_x,
                g.destination_y,
                g.destination_triangle
            );
        }
    }

    // MAPJUMP opcodes anywhere in the field's scripts (one per distinct script)
    let jumps = script.map_jumps();
    for j in &jumps {
        let target = maplist.and_then(|m| m.name(j.field_id));
        println!(
            "  MAPJUMP in {}.{} @{:#06x}{}: -> {target:?} (id {}) x {} y {} triangle {} dir {}",
            j.entity_name,
            j.script,
            j.offset,
            if j.conditional { " (conditional)" } else { "" },
            j.field_id,
            j.x,
            j.y,
            j.triangle,
            j.direction
        );
    }
    println!("MAPJUMP instructions (distinct scripts, literal operands): {}", jumps.len());

    if dialogs {
        println!("dialogs (terminal only):");
        for d in script.dialogs() {
            if let Some(t) = script.dialog_text(d.index) {
                println!("  #{:3} @{:#06x} {} bytes: {:?}", d.index, d.offset, d.len, t.text);
            }
        }
    }
    if let Some((e, s)) = disasm {
        let mut seen = BTreeSet::new();
        for entity in 0..script.entity_count() {
            if e.is_some_and(|e| e != entity) {
                continue;
            }
            for slot in 0..SCRIPTS_PER_ENTITY {
                if s.is_some_and(|s| s != slot) {
                    continue;
                }
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) && s.is_none() {
                    continue; // slots sharing an entry point print once
                }
                println!("--- entity {entity} ({}) script {slot} {:#06x}..{:#06x}", script.entity_name(entity).unwrap_or("?"), span.start, span.end);
                match script.disassemble_partial(entity, slot) {
                    Ok(d) => {
                        for ins in &d.instructions {
                            println!("  {ins}");
                        }
                        if let Some(err) = d.error {
                            println!("  !! {err}");
                        }
                    }
                    Err(e) => println!("  !! {e}"),
                }
            }
        }
    }
    Ok(())
}

fn print_survey(lgp: &mut LgpArchive<std::fs::File>) {
    let s = survey_all(lgp);
    print!("{}", s.summary());
    let show = |title: &str, list: &[common::Problem]| {
        println!("{title}: {}", list.len());
        for p in list.iter().take(25) {
            println!("  {} entity {} script {}: {}", p.field, p.entity, p.script, p.what);
        }
    };
    show("desyncs", &s.desyncs);
    show("bad jumps", &s.bad_jumps);
    show("slots outside the code area", &s.slots_without_code);
    show("no-terminator scripts falling into something other than RET", &s.falls_through_into_other);
    show("placements outside their triangle", &s.placement_outside);
    show("placements naming a missing triangle", &s.placement_no_triangle);
    println!("MAPJUMP destinations off their triangle: {:?}", s.map_jump_destinations_off_triangle.iter().take(25).collect::<Vec<_>>());
}
