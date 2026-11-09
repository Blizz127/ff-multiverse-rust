//! Survey of the script-driven exits of the owner's FF7 fields (needs the data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example exits_dump -- md1_2 nrthmk     # named fields
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example exits_dump -- --all            # every field, totals
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example exits_dump -- --gated md1stin  # the gated-exit list
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example exits_dump -- --chain md1stin  # walk the exit graph
//! ```
//!
//! For each named field it prints its gateways and every `MAPJUMP` of its scripts with the root that
//! runs it (see `fmv_ff7::script::ExitTrigger`), the line it belongs to, the scripts it passes
//! through and the conditions it sits under. `--all` prints totals over the 702 fields. `--gated`
//! lists, for the named fields (or every field with `--all`), the walk-on exits (the ones a server
//! treats as open) that have a condition on the game state. `--chain START` walks the graph of
//! gateways and walk-on script exits breadth first from START and prints each field with its
//! distance and the ways out. Only field names, entity names, variable names and numbers are
//! printed: no game text. Nothing is written to disk.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{resolve_gateways, ExitTrigger, FieldScript, Maplist, ScriptExit};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

struct Survey<'a> {
    lgp: &'a mut LgpArchive<std::fs::File>,
    maplist: &'a Maplist,
}

/// What one field says about its exits.
struct FieldExits {
    gateways: Vec<(usize, String, i16, i16, Option<usize>)>,
    script: Vec<ScriptExit>,
}

impl Survey<'_> {
    fn field_exits(&mut self, name: &str) -> Result<FieldExits, String> {
        let field = load_field(self.lgp, name)?;
        let script = FieldScript::from_field(&field).map_err(|e| e.to_string())?;
        let triggers = field.triggers().map_err(|e| e.to_string())?;
        let gateways = resolve_gateways(&triggers, self.maplist)
            .into_iter()
            .map(|g| (g.slot, g.target_field.unwrap_or_else(|| format!("#{}", g.target_field_id)), g.destination_x, g.destination_y, g.destination_triangle))
            .collect();
        Ok(FieldExits { gateways, script: script.script_exits(self.maplist) })
    }
}

fn guards_text(e: &ScriptExit) -> String {
    if e.guards.is_empty() {
        "-".to_string()
    } else {
        e.guards.iter().map(ToString::to_string).collect::<Vec<_>>().join(" AND ")
    }
}

fn print_exit(field: &str, e: &ScriptExit) {
    let line = e.line.map_or(String::new(), |l| {
        format!(" line ({},{})-({},{}) enabled={:?}", l.a[0], l.a[1], l.b[0], l.b[1], l.enabled)
    });
    let path: Vec<String> = e.path.iter().map(|r| format!("{}.{}", r.entity, r.slot)).collect();
    println!(
        "  {field}: [{}] {} ({}){line} -> {} (id {}) arrive ({}, {}) tri {} dir {}{} @{:#06x} path {} guards: {}",
        e.trigger,
        e.entity_name,
        e.entity,
        e.target.as_deref().unwrap_or("?"),
        e.target_id,
        e.x,
        e.y,
        e.triangle,
        e.direction,
        if e.arrival_is_placeholder() { " PLACEHOLDER" } else { "" },
        e.offset,
        path.join(">"),
        guards_text(e),
    );
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lgp = match LgpArchive::open_path(flevel_path(&dir)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open flevel.lgp: {e}");
            return;
        }
    };
    let maplist = match Maplist::from_archive(&mut lgp) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("no maplist: {e}");
            return;
        }
    };
    let mut survey = Survey { lgp: &mut lgp, maplist: &maplist };
    let (mut all, mut gated, mut chain) = (false, false, None);
    let mut names = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" => all = true,
            "--gated" => gated = true,
            "--chain" => chain = it.next().cloned(),
            name => names.push(name.to_string()),
        }
    }
    if let Some(start) = chain {
        run_chain(&mut survey, &start);
        return;
    }
    if all {
        names = maplist.names().iter().filter(|n| !n.is_empty()).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    }
    if names.is_empty() {
        eprintln!("usage: exits_dump <field>... | --all | --gated (<field>... | --all) | --chain START");
        return;
    }
    let (mut fields, mut unreadable) = (0usize, Vec::new());
    let mut by_trigger: BTreeMap<String, usize> = BTreeMap::new();
    let (mut walk_on, mut walk_on_gated, mut placeholders, mut disabled) = (0usize, 0usize, 0usize, 0usize);
    let mut shared_lines = 0usize;
    let mut slots: BTreeMap<u8, usize> = BTreeMap::new();
    for name in &names {
        let exits = match survey.field_exits(name) {
            Ok(e) => e,
            Err(e) => {
                unreadable.push(format!("{name}: {e}"));
                continue;
            }
        };
        fields += 1;
        if !all && !gated {
            println!("=== {name}");
            for (slot, target, x, y, tri) in &exits.gateways {
                println!("  {name}: gateway slot {slot} -> {target} arrive ({x}, {y}) tri {tri:?}");
            }
        }
        // two or more walk-on exits on one line with different targets: the server would have to pick
        let mut lines: BTreeMap<([i16; 3], [i16; 3]), BTreeSet<String>> = BTreeMap::new();
        for e in &exits.script {
            *by_trigger.entry(e.trigger.to_string()).or_default() += 1;
            if let ExitTrigger::Line { slot } = e.trigger {
                *slots.entry(slot).or_default() += 1;
            }
            if e.trigger.is_walk_on() {
                walk_on += 1;
                walk_on_gated += usize::from(e.is_story_gated());
                placeholders += usize::from(e.arrival_is_placeholder());
                disabled += usize::from(e.line.is_some_and(|l| l.enabled == Some(false)));
                if let Some(l) = e.line {
                    lines.entry((l.a, l.b)).or_default().insert(e.target.clone().unwrap_or_default());
                }
            }
            if gated {
                if e.trigger.is_walk_on() && e.is_story_gated() {
                    print_exit(name, e);
                }
            } else if !all {
                print_exit(name, e);
            }
        }
        shared_lines += lines.values().filter(|t| t.len() > 1).count();
    }
    println!(
        "{fields} fields read, {} script exits ({:?}); walk-on {walk_on} ({walk_on_gated} with a condition on the game state, {placeholders} with a placeholder arrival, {disabled} on lines that start disabled); {shared_lines} lines lead to more than one field; line slots {slots:?}",
        by_trigger.values().sum::<usize>(),
        by_trigger
    );
    if !unreadable.is_empty() {
        println!("unreadable: {unreadable:?}");
    }
}

/// Breadth first over gateways and walk-on script exits from `start`.
fn run_chain(survey: &mut Survey<'_>, start: &str) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue = VecDeque::new();
    seen.insert(start.to_string(), 0);
    queue.push_back(start.to_string());
    while let Some(name) = queue.pop_front() {
        let depth = seen.get(&name).copied().unwrap_or(0);
        let exits = match survey.field_exits(&name) {
            Ok(e) => e,
            Err(e) => {
                println!("{depth:>3} {name}: unreadable ({e})");
                continue;
            }
        };
        let mut ways: Vec<String> = Vec::new();
        let mut next: Vec<String> = Vec::new();
        for (_, target, ..) in &exits.gateways {
            ways.push(format!("gw:{target}"));
            next.push(target.clone());
        }
        for e in exits.script.iter().filter(|e| e.trigger.is_walk_on()) {
            if let Some(t) = &e.target {
                ways.push(format!("{}{}:{t}", if e.is_story_gated() { "line*" } else { "line" }, e.trigger));
                next.push(t.clone());
            }
        }
        println!("{depth:>3} {name}: {}", ways.join(" "));
        for t in next {
            if !seen.contains_key(&t) {
                seen.insert(t.clone(), depth + 1);
                queue.push_back(t);
            }
        }
    }
    println!("{} fields reachable from {start}", seen.len());
}
