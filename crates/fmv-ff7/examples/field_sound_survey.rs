//! Survey of what the owner's real field scripts say about sound effects, and of what `fmv_ff7::field_sound` makes of it
//! (needs the owner's data; skips otherwise). Prints ids, offsets, counts and field names only, never game bytes.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_FF7_AUDIO_DATA=<.../audio data> \
//!     cargo run --release -p fmv-ff7 --example field_sound_survey -- [--census] [--ambient [MOMENT ...]] [--reviewed] [--plans]
//! ```
//!
//! `--census` counts every `SOUND` and every `AKAO` sound play (operation 40 to 43) and where they lie; `--ambient`
//! runs every field's start-up for the given story moments (default 1, 15 and 27) and lists the fields that have ambient
//! loops, the ids, and why runs stop; `--reviewed` prints what each of the ten reviewed fields plays, by every plan;
//! `--plans` counts the link, flip and exit plans over the archive. With no flag it prints all of them.
#[path = "../tests/field_sound_common.rs"]
mod common;

use common::{ambient_survey, audio_dir, census, census_report, data_dir, load_archive, open_bank, reviewed_field, show_ambient, show_events};
use fmv_ff7::REVIEWED_FIELDS;

fn main() {
    let (Some(data), Some(audio)) = (data_dir(), audio_dir()) else {
        eprintln!("FMV_FF7_DATA and FMV_FF7_AUDIO_DATA are not set; skipping (point them at the game's data/ directory and the audio folder)");
        return;
    };
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let all = args.is_empty();
    let wants = |flag: &str, args: &[String]| all || args.iter().any(|a| a == flag);
    let moments: Vec<u16> = {
        let after: Vec<u16> = args.iter().skip_while(|a| *a != "--ambient").skip(1).map_while(|a| a.parse().ok()).collect();
        if after.is_empty() {
            vec![1, 15, 27]
        } else {
            after
        }
    };
    args.retain(|a| a.starts_with("--"));
    let (archive, bank) = match (load_archive(&data), open_bank(&audio)) {
        (Ok(archive), Ok(bank)) => (archive, bank),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("cannot read the data: {e}");
            return;
        }
    };
    if wants("--census", &args) {
        let census = census(&archive.fields);
        println!("== census\n{}", census_report(&census, Some(&bank)));
    }
    if wants("--ambient", &args) {
        for &moment in &moments {
            let survey = ambient_survey(&archive.fields, &bank, moment);
            let loops: usize = survey.per_field.values().map(Vec::len).sum();
            println!(
                "== start-ups at moment {moment}: {} fields have ambient loops ({loops} loops); {} conditions could not be judged in {} fields; {} loops left out (channel still open); banked {}",
                survey.per_field.len(),
                survey.unjudged,
                survey.unjudged_fields,
                survey.left_open,
                survey.banked
            );
            println!("   how the runs ended: {:?}; untimed opcodes that stopped runs: {:?}", survey.stops, survey.untimed);
            let mut tests: Vec<(&String, &usize)> = survey.tests.iter().collect();
            tests.sort_by(|a, b| b.1.cmp(a.1));
            println!("   tests that could not be judged (most common): {:?}", tests.iter().take(12).collect::<Vec<_>>());
            for (field, list) in &survey.per_field {
                println!("   {field}: {}", show_ambient(list));
            }
        }
    }
    if wants("--reviewed", &args) {
        println!("== the reviewed fields");
        for name in REVIEWED_FIELDS {
            let Some(r) = reviewed_field(&archive, &bank, name, &moments) else {
                println!("{name}: not in the archive");
                continue;
            };
            println!("{name}:");
            for (moment, ambient) in &r.ambient {
                println!("   ambient at moment {moment}: {}", if ambient.is_empty() { "-".to_string() } else { show_ambient(ambient) });
            }
            for (ok, events) in &r.links {
                println!("   link{}: {}", if *ok { " (OK)" } else { "" }, show_events(events));
            }
            for (on, events) in &r.flips {
                println!("   lift bit {}: {}", if *on { "on" } else { "off" }, show_events(events));
            }
            for (target, events) in &r.exits {
                println!("   exit to {target}: {}", show_events(events));
            }
            let empty: Vec<u16> = r.ids.iter().copied().filter(|id| bank.decode_sound_id(*id).map_or(true, |c| c.samples.is_empty())).collect();
            println!("   ids {:?}; without a clip: {empty:?}", r.ids);
        }
    }
    if wants("--plans", &args) {
        let exits = common::exit_census(&archive);
        println!("== script exits: {} walk-on; {} play a sound before the jump in the script that holds it, in {} fields (a static count)", exits.walk_on, exits.with_a_sound, exits.fields.len());
        for &moment in &moments {
            let plans = common::all_plans(&archive, &bank, moment);
            println!(
                "== plans at moment {moment}: links {} in {} fields; flips {} in {} fields; walk-on exits with a sound {} in {} fields; {} distinct ids; the callees of links stop: {:?}",
                plans.links,
                plans.fields_with_links.len(),
                plans.flips,
                plans.fields_with_flips.len(),
                plans.exits,
                plans.fields_with_exits.len(),
                plans.ids.len(),
                plans.link_stops
            );
        }
    }
}
