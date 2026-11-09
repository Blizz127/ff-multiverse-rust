//! Checks of the battle machinery against the owner's REAL FF7 data. Every test is `#[ignore]`d and
//! reads the library from `FMV_FF7_LIBRARY`: a state directory that holds the owner's imported
//! library (`<dir>/library`), opened read-only. Without the variable the tests print a note and
//! pass without doing anything.
//!
//! ```text
//! FMV_FF7_LIBRARY=<state dir with library/> \
//!   cargo test -p fmv-server --test battle_real -- --ignored --nocapture
//! ```
//!
//! Nothing from the data is asserted byte-for-byte or kept: the tests print counts and statistics
//! (how many fields have encounter tables, how often an auto-attacking party wins a formation and
//! how long it takes), which is how the debug formation and the battle pace of the smoke were chosen.

use std::path::PathBuf;
use std::sync::Arc;

use fmv_ff7::field::section;
use fmv_ff7_battle::rules::{BattleEvent, Command, Outcome};
use fmv_protocol::{BattleRules, BattleSetup, Ff7BattleData};
use fmv_server::ff7::{decode_encounter_section, open_battle_data, open_ff7, prewarm, Ff7Source};

fn source() -> Option<Ff7Source> {
    let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_LIBRARY is not set");
        return None;
    };
    Some(Ff7Source::Library { state_dir: Some(dir), set: None })
}

fn battle_data(source: &Ff7Source) -> Arc<Ff7BattleData> {
    open_battle_data(source).expect("the library has scene.bin and kernel.bin").0
}

/// One simulated battle with auto-attackers, driven like the server drives it: the engine advances
/// in slices of `slice` ticks and every ready party member attacks the first living enemy.
#[derive(Debug, Clone)]
struct Sim {
    outcome: Option<Outcome>,
    ticks: u64,
    /// Attacks (damage or miss) made by each party member.
    acted: Vec<usize>,
    /// Party members dead at the end.
    dead: usize,
}

fn simulate(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, seed: u64, speed: u8, tps: u32) -> Sim {
    let party = characters
        .iter()
        .enumerate()
        .map(|(i, &character)| {
            fmv_protocol::BattleMember::player(
                fmv_core::EntityId(i as u64 + 1),
                fmv_core::AccountId(i as u64 + 1),
                format!("P{i}"),
                character,
                level.unwrap_or_else(|| data.starting_level(character)),
                0,
            )
        })
        .collect();
    let info = data.formation(formation).expect("a real formation");
    let setup = BattleSetup {
        formation_id: formation,
        seed,
        party,
        preemptive: info.preemptive,
        back_attack: info.back_attack,
        battle_speed: speed,
        ticks_per_second: tps,
    };
    let mut state = data.build(&setup).expect("builds");
    let mut acted = vec![0usize; characters.len()];
    let slice = u64::from((tps / 20).max(1));
    let count = |events: &[BattleEvent], acted: &mut Vec<usize>| {
        for e in events {
            if let BattleEvent::Damage { actor, .. } | BattleEvent::Miss { actor, .. } = e {
                if let Some(n) = acted.get_mut(usize::from(actor.0)) {
                    *n += 1;
                }
            }
        }
    };
    for _ in 0..2_000_000u32 {
        if state.outcome().is_some() {
            break;
        }
        let events = state.step_ticks(slice);
        count(&events, &mut acted);
        for id in state.ready_actors() {
            let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else { break };
            if let Ok(events) = state.submit_command(id, Command::Attack { target }) {
                count(&events, &mut acted);
            }
        }
    }
    let dead = state.party_ids().into_iter().filter(|id| !state.actor(*id).is_some_and(|c| c.is_alive())).count();
    Sim { outcome: state.outcome(), ticks: state.tick(), acted, dead }
}

/// What a batch of simulated fights of one formation came to.
#[derive(Debug, Clone, Copy)]
struct Stats {
    won: u64,
    lost: u64,
    /// Fights (of the won ones) in which at least one party member died.
    with_losses: u64,
    longest_ticks: u64,
    mean_ticks: u64,
    /// The fewest attacks any party member made in a won fight.
    fewest_attacks: usize,
}

fn stats(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, speed: u8, tps: u32, seeds: u64) -> Stats {
    let mut out = Stats { won: 0, lost: 0, with_losses: 0, longest_ticks: 0, mean_ticks: 0, fewest_attacks: usize::MAX };
    let mut total = 0u64;
    for seed in 0..seeds {
        let s = simulate(data, formation, characters, level, seed, speed, tps);
        match s.outcome {
            Some(Outcome::Victory) => {
                out.won += 1;
                out.with_losses += u64::from(s.dead > 0);
                out.fewest_attacks = out.fewest_attacks.min(s.acted.iter().copied().min().unwrap_or(0));
            }
            Some(Outcome::Defeat) => out.lost += 1,
            Some(Outcome::Escaped) => panic!("formation {formation} seed {seed}: nobody gave Escape"),
            None => panic!("formation {formation} seed {seed}: the battle never ended"),
        }
        out.longest_ticks = out.longest_ticks.max(s.ticks);
        total += s.ticks;
    }
    out.mean_ticks = total / seeds.max(1);
    if out.fewest_attacks == usize::MAX {
        out.fewest_attacks = 0;
    }
    out
}

#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn every_field_encounter_table_decodes_and_names_real_formations() {
    let Some(source) = source() else { return };
    let opened = open_ff7(&source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let report = rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let data = battle_data(&source);
    eprintln!("{} fields indexed", report.areas);

    let (mut fields, mut with_table, mut entries, mut sums_to_64) = (0usize, 0usize, 0usize, 0usize);
    let mut sample = Vec::new();
    for info in fmv_adapter_api::GameAdapter::list_areas(&*opened.adapter) {
        let Ok(field) = opened.adapter.load_field(&info.id.key) else { continue };
        let Ok(bytes) = field.field_file().section(section::ENCOUNTER) else { continue };
        fields += 1;
        let Some(table) = decode_encounter_section(bytes) else { continue };
        with_table += 1;
        if table.total_weight() == 64 {
            sums_to_64 += 1;
        }
        for e in &table.entries {
            entries += 1;
            assert!(
                data.formation(e.formation).is_some(),
                "field {}: formation {} of its encounter table is not a formation with enemies in scene.bin (the layout guess is wrong?)",
                info.id.key,
                e.formation
            );
        }
        if info.id.key == "md1stin" || sample.len() < 3 {
            sample.push((info.id.key.clone(), table));
        }
    }
    eprintln!("{fields} fields have an encounter section; {with_table} of them have a random-encounter table ({entries} entries, {sums_to_64} tables whose weights add up to 64)");
    for (key, table) in &sample {
        let formations: Vec<String> = table
            .entries
            .iter()
            .map(|e| format!("{} (weight {}, {} enemies)", e.formation, e.weight, data.formation(e.formation).map_or(0, |f| f.enemies)))
            .collect();
        eprintln!("  {key}: rate {}, formations [{}]", table.rate, formations.join(", "));
    }
    assert!(fields > 600, "a whole game has hundreds of fields");
    assert!(with_table > 50 && with_table < fields, "some but not all fields have random battles: {with_table} of {fields}");
    assert_eq!(with_table, sums_to_64, "every real table's weights add up to 64: the decoder reads the right bits");
    // md1stin, the default field of the server, has a table.
    let md1stin = sample.iter().find(|(k, _)| k == "md1stin").expect("md1stin was scanned");
    assert!(!md1stin.1.entries.is_empty());
}

#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_default_debug_formation_exists_and_every_formation_builds_for_every_character() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let default = data.default_formation().expect("some formation has enemies");
    eprintln!("the data-derived default formation: {default} ({} enemies)", data.formation(default).unwrap().enemies);
    let mut buildable = 0;
    for id in 0..1024u16 {
        let Some(info) = data.formation(id) else { continue };
        assert!(info.enemies >= 1);
        for character in 0..9u8 {
            let setup = BattleSetup {
                formation_id: id,
                seed: 1,
                party: vec![fmv_protocol::BattleMember::player(
                    fmv_core::EntityId(1),
                    fmv_core::AccountId(1),
                    "Solo",
                    character,
                    data.starting_level(character),
                    0,
                )],
                preemptive: info.preemptive,
                back_attack: info.back_attack,
                battle_speed: 128,
                ticks_per_second: 60,
            };
            data.build(&setup).unwrap_or_else(|e| panic!("formation {id}, character {character}: {e}"));
        }
        buildable += 1;
    }
    eprintln!("{buildable} formations have enemies and build for all nine characters");
    assert!(buildable > 400);
    for character in 0..9u8 {
        eprintln!("character {character}: starting level {}", data.starting_level(character));
    }
}

/// The gate behind `scripts/battle-smoke.sh`: two auto-attackers must beat each of md1stin's
/// two-enemy formations EVERY time, both of them must attack in every fight, and the fight must be
/// short at the smoke's pace and still reasonable at the engine's default pace. 200 seeds each.
///
/// Who the two are: since accounts play their own character (M2-d) the smoke's headless bots both
/// register without a look, so both are Cloud, at the level he starts at (6) in the first fight and
/// at 7 in the second one (the first win levels him up on the real data: he starts six experience
/// short of level 7). Both are checked, and Cloud with Barret (the pair this gate was first written
/// for, still what two different looks field) at their starting levels.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_smoke_formations_are_won_every_time_by_both_members_and_quickly() {
    let Some(source) = source() else { return };
    let opened = open_ff7(&source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let data = battle_data(&source);
    let table = {
        let field = opened.adapter.load_field("md1stin").expect("md1stin");
        decode_encounter_section(field.field_file().section(section::ENCOUNTER).expect("section")).expect("md1stin has a table")
    };
    let two_enemy: Vec<u16> = table.entries.iter().map(|e| e.formation).filter(|&f| data.formation(f).is_some_and(|i| i.enemies == 2)).collect();
    assert!(!two_enemy.is_empty(), "md1stin's table has two-enemy formations");
    for formation in two_enemy {
        // (speed, ticks per second, longest acceptable fight in seconds)
        for (speed, tps, limit_s) in [(0u8, 240u32, 10u64), (128, 60, 45)] {
            // (who fights, at which level: `None` is each one's starting level)
            for (characters, level, what) in [
                (&[0u8, 1][..], None, "Cloud and Barret at their starting levels"),
                (&[0, 0][..], Some(6), "Cloud and Cloud at level 6 (the smoke's first fight)"),
                (&[0, 0][..], Some(7), "Cloud and Cloud at level 7 (after the first win's level-up)"),
            ] {
                let s = stats(&data, formation, characters, level, speed, tps, 200);
                let longest_s = s.longest_ticks / u64::from(tps);
                eprintln!(
                    "formation {formation} at speed {speed}, {tps} ticks/s, {what}: won {}/200, deaths in {} wins, longest {} ticks ({longest_s} s), mean {} ticks, fewest attacks by a member {}",
                    s.won, s.with_losses, s.longest_ticks, s.mean_ticks, s.fewest_attacks
                );
                assert_eq!((s.won, s.lost), (200, 0), "formation {formation}, {what}: every fight is won");
                assert!(s.fewest_attacks >= 1, "formation {formation}, {what}: in every fight both members attack at least once");
                assert!(longest_s <= limit_s, "formation {formation}, {what}: the longest fight takes {longest_s} s (limit {limit_s} s)");
            }
        }
    }
}

/// What the formations of md1stin's table drop (protocol v6): for each, how many of the first 2000 seeds
/// drop at least one item, and the first seeds that do, with what they drop (item ids and counts, nothing
/// else). It is how `scripts/battle-smoke.sh --seed N` gets a seed under which the drop chain (rolled by the
/// server after a win, dealt to the living members, saved, told, kept across a reconnect) runs over real
/// processes: a random seed often drops nothing from these weak enemies. The gate is that the drop tables
/// are read at all (some seed of some formation drops something) and that a roll never drops more than
/// one item per enemy.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_formations_of_the_first_field_drop_items_for_some_seeds() {
    let Some(source) = source() else { return };
    let opened = open_ff7(&source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let data = battle_data(&source);
    let table = {
        let field = opened.adapter.load_field("md1stin").expect("md1stin");
        decode_encounter_section(field.field_file().section(section::ENCOUNTER).expect("section")).expect("md1stin has a table")
    };
    let mut any = false;
    let mut formations: Vec<u16> = table.entries.iter().map(|e| e.formation).collect();
    formations.sort_unstable();
    formations.dedup();
    for formation in formations {
        let enemies = data.formation(formation).map_or(0, |f| f.enemies);
        let mut dropping = Vec::new();
        for seed in 0..2000u64 {
            let drops = data.roll_drops(formation, seed);
            assert!(drops.iter().map(|d| usize::from(d.count)).sum::<usize>() <= enemies, "formation {formation}, seed {seed}: at most one item per enemy");
            if !drops.is_empty() {
                dropping.push((seed, drops.iter().map(|d| (d.item, d.count)).collect::<Vec<_>>()));
            }
        }
        any |= !dropping.is_empty();
        eprintln!("formation {formation} ({enemies} enemies): {} of 2000 seeds drop something; the first: {:?}", dropping.len(), &dropping[..dropping.len().min(3)]);
    }
    assert!(any, "some formation of the table drops something for some seed");
}

/// The server's default debug formation (the weakest one in the data) is a real fight that a
/// starting party wins.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_default_debug_formation_is_winnable_by_a_starting_party() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let id = data.default_formation().expect("a default formation");
    let s = stats(&data, id, &[0, 1], None, 128, 60, 100);
    eprintln!(
        "default formation {id} ({} enemies): won {}/100, deaths in {} wins, longest {} ticks, mean {}",
        data.formation(id).unwrap().enemies,
        s.won,
        s.with_losses,
        s.longest_ticks,
        s.mean_ticks
    );
    assert_eq!(s.won, 100, "the weakest formation is won every time by Cloud and Barret");
}

/// Exploration, not a gate: how an auto-attacking Cloud + Barret does against the formations of
/// md1stin's table and the first formations of the game, at the default pace. Prints a table.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn how_auto_attackers_fare_against_early_formations() {
    let Some(source) = source() else { return };
    let opened = open_ff7(&source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let data = battle_data(&source);
    let table = {
        let field = opened.adapter.load_field("md1stin").expect("md1stin");
        decode_encounter_section(field.field_file().section(section::ENCOUNTER).expect("section")).expect("md1stin has a table")
    };
    let mut candidates: Vec<u16> = table.entries.iter().map(|e| e.formation).collect();
    candidates.extend((0..1024u16).filter(|&id| data.formation(id).is_some()).take(24));
    candidates.sort_unstable();
    candidates.dedup();
    eprintln!("formation | enemies | won/lost of 40 | wins with a death | longest ticks | mean ticks | fewest attacks by a member in a win");
    for id in candidates {
        let info = data.formation(id).unwrap();
        let s = stats(&data, id, &[0, 1], None, 128, 60, 40);
        eprintln!(
            "{id:>9} | {:>7} | {:>3}/{:<3} | {:>17} | {:>13} | {:>10} | {}",
            info.enemies, s.won, s.lost, s.with_losses, s.longest_ticks, s.mean_ticks, s.fewest_attacks
        );
    }
}
