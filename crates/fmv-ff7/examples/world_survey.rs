//! Survey of what the owner's FF7 fields hold for a server: the exits their scripts decide and the story points
//! the adapter reads (needs the data; skips otherwise). Prints field names, entity-free labels and numbers only
//! (never game text), and writes nothing.
//!
//! The survey loads every field with everything on (`Ff7Adapter::survey_field`), and says for each whether the field
//! is one of the reviewed fields (`fmv_ff7::REVIEWED_FIELDS`), the only ones whose script exits and story points a
//! server offers: for any other it is a list of what is left off, and the way to review a field before it joins the
//! list (see that constant's docs).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example world_survey -- --all          # totals, every field
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example world_survey -- --points       # every story point
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example world_survey -- md1_2 elevtr1  # named fields in detail
//! ```
//!
//! For a named field: its exits (gateways and script exits, with whether each is open, closed or unjudgeable for a
//! new account and for the moment 12 and 40) and its story points. `--all` counts them over the archive;
//! `--points` lists every point (which is what to read to check that an entry point is one a script really runs by
//! itself).
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, is_field_name};
use fmv_adapter_api::{AreaRuntime, ExitKind, ExitState, StoryTrigger};
use fmv_core::{EditionProvenance, GameId, Platform};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use std::collections::BTreeMap;

fn state_name(s: &ExitState) -> String {
    match s {
        ExitState::Open => "open".into(),
        ExitState::Closed => "closed".into(),
        ExitState::Unknown(why) => format!("unjudged ({why})"),
    }
}

fn trigger_name(t: &StoryTrigger) -> &'static str {
    match t {
        StoryTrigger::Region(_) => "region",
        StoryTrigger::Line(..) => "line",
        StoryTrigger::Enter => "entry",
        _ => "other",
    }
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = flevel_path(&dir);
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "steam".into(),
        region: "any".into(),
        language: "en".into(),
        revision: "survey".into(),
        sources: vec![],
        modded: false,
    };
    let adapter = match Ff7Adapter::from_flevel_path(&path, provenance) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    let names: Vec<String> = match LgpArchive::open_path(&path) {
        Ok(l) => l.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect(),
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    let all = args.iter().any(|a| a == "--all");
    let points = args.iter().any(|a| a == "--points");
    let wanted: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let (mut fields, mut with_exits, mut script_exits, mut max_script, mut max_field) = (0usize, 0usize, 0usize, 0usize, String::new());
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    let mut open_new: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut by_trigger: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut fields_with_points = 0usize;
    let mut most_points = (0usize, String::new());
    let (mut ok_points, mut battle_points) = (0usize, 0usize);
    let (mut active_totals, mut off_totals) = ((0usize, 0usize, 0usize), (0usize, 0usize, 0usize));
    for name in &names {
        if !all && !points && !wanted.iter().any(|w| w.as_str() == name) {
            continue;
        }
        let Ok(area) = adapter.survey_field(name) else { continue };
        let reviewed = fmv_ff7::is_reviewed_field(name);
        fields += 1;
        let exits = area.exits();
        let scripts: Vec<_> = exits.iter().filter(|e| matches!(e.kind, ExitKind::Script { .. })).collect();
        with_exits += usize::from(!exits.is_empty());
        script_exits += scripts.len();
        if scripts.len() > max_script {
            (max_script, max_field) = (scripts.len(), name.clone());
        }
        let story_points = area.story_points();
        // (script exits, story points, fields that have either) of the reviewed fields, which a server offers, and of
        // the others, which it leaves off
        let totals = if reviewed { &mut active_totals } else { &mut off_totals };
        totals.0 += scripts.len();
        totals.1 += story_points.len();
        totals.2 += usize::from(!scripts.is_empty() || !story_points.is_empty());
        if !story_points.is_empty() {
            fields_with_points += 1;
        }
        if story_points.len() > most_points.0 {
            most_points = (story_points.len(), name.clone());
        }
        for e in &scripts {
            let verdict = state_name(&area.exit_state(e, &[]));
            let key = verdict.split(" (").next().unwrap_or("").to_string();
            *verdicts.entry(key).or_default() += 1;
            if verdict.starts_with("unjudged") {
                *verdicts.entry(verdict.clone()).or_default() += 1;
            }
            if verdict == "open" {
                open_new.entry(name.clone()).or_default().push(e.target.clone());
            }
        }
        for p in &story_points {
            *by_trigger.entry(trigger_name(&p.trigger)).or_default() += 1;
            ok_points += usize::from(p.label.contains("(OK)"));
            battle_points += usize::from(p.label.contains("battle"));
        }
        if wanted.iter().any(|w| w.as_str() == name) {
            println!(
                "== {name} ({}): {} gateways, {} script exits, {} story points",
                if reviewed { "reviewed: a server offers them" } else { "NOT reviewed: a server leaves the script exits and points off" },
                exits.len() - scripts.len(),
                scripts.len(),
                story_points.len()
            );
            for e in &exits {
                let at = |m: u16| state_name(&area.exit_state(e, &StoryState::at(m).encode()));
                println!(
                    "   {:?} line ({:.0},{:.0})-({:.0},{:.0}) -> {} arrive ({:.0},{:.0}) hint {:?}: new game {} | moment 12 {} | moment 40 {}",
                    e.kind,
                    e.line.0.x,
                    e.line.0.y,
                    e.line.1.x,
                    e.line.1.y,
                    e.target,
                    e.arrival.x,
                    e.arrival.y,
                    e.arrival_hint,
                    state_name(&area.exit_state(e, &[])),
                    at(12),
                    at(40)
                );
            }
        }
        if points || wanted.iter().any(|w| w.as_str() == name) {
            for p in &story_points {
                println!("   point {name}#{} [{}] {}", p.id, trigger_name(&p.trigger), p.label);
            }
        }
    }
    if all {
        println!("{fields} fields: {with_exits} have exits; {script_exits} script exits (at most {max_script}, in {max_field}); for a new account {verdicts:?}");
        println!("fields whose script exits a new account may take: {}", open_new.len());
        println!("story points by trigger {by_trigger:?} in {fields_with_points} fields (most: {} in {}); {ok_points} want OK, {battle_points} name a battle", most_points.0, most_points.1);
        println!(
            "offered by a server (the {} reviewed fields): {} script exits, {} story points, in {} fields; left off ({} other fields have some): {} script exits, {} story points",
            fmv_ff7::REVIEWED_FIELDS.len(),
            active_totals.0,
            active_totals.1,
            active_totals.2,
            off_totals.2,
            off_totals.0,
            off_totals.1
        );
    }
}
