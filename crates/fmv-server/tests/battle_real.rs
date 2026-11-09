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
/// in slices of `slice` ticks and every ready player attacks the first living enemy. With `canon` the story's
/// own party (Cloud and Barret, at the first player's level) fights first as the server's companions do (the
/// server's own rule, `fmv_server::companion::command`).
#[derive(Debug, Clone)]
struct Sim {
    outcome: Option<Outcome>,
    ticks: u64,
    /// Attacks (damage or miss) made by each PLAYER (the companions' are not counted here).
    acted: Vec<usize>,
    /// Party members dead at the end (companions included).
    dead: usize,
}

#[allow(clippy::too_many_arguments)]
fn simulate_with(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, canon: bool, seed: u64, speed: u8, tps: u32) -> Sim {
    let players: Vec<fmv_protocol::BattleMember> = characters
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
    let leader_level = players.first().map_or_else(|| data.starting_level(0), |p| p.level);
    let companions: Vec<fmv_protocol::BattleMember> = if canon {
        [0u8, 1].iter().map(|&c| fmv_protocol::BattleMember::companion(data.character_name(c).expect("a canon name"), c, leader_level, 0)).collect()
    } else {
        Vec::new()
    };
    let companion_count = companions.len();
    let party: Vec<fmv_protocol::BattleMember> = companions.into_iter().chain(players).collect();
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
    let mut acted = vec![0usize; companion_count + characters.len()];
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
            let command = if usize::from(id.0) < companion_count {
                // The server's rule for a canon companion (heal, limit, attack), as the server applies it.
                let Some(command) = fmv_server::companion::command(&state, id) else { continue };
                command
            } else {
                let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else { break };
                Command::Attack { target }
            };
            if let Ok(events) = state.submit_command(id, command) {
                count(&events, &mut acted);
            }
        }
    }
    let dead = state.party_ids().into_iter().filter(|id| !state.actor(*id).is_some_and(|c| c.is_alive())).count();
    Sim { outcome: state.outcome(), ticks: state.tick(), acted: acted.split_off(companion_count), dead }
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
    /// The fewest attacks any player made in a won fight.
    fewest_attacks: usize,
    /// Won fights in which every player attacked at least once.
    every_player_acted: u64,
}

fn stats(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, speed: u8, tps: u32, seeds: u64) -> Stats {
    stats_with(data, formation, characters, level, false, speed, tps, seeds)
}

#[allow(clippy::too_many_arguments)]
fn stats_with(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, canon: bool, speed: u8, tps: u32, seeds: u64) -> Stats {
    let mut out = Stats { won: 0, lost: 0, with_losses: 0, longest_ticks: 0, mean_ticks: 0, fewest_attacks: usize::MAX, every_player_acted: 0 };
    let mut total = 0u64;
    for seed in 0..seeds {
        let s = simulate_with(data, formation, characters, level, canon, seed, speed, tps);
        match s.outcome {
            Some(Outcome::Victory) => {
                out.won += 1;
                out.with_losses += u64::from(s.dead > 0);
                out.fewest_attacks = out.fewest_attacks.min(s.acted.iter().copied().min().unwrap_or(0));
                out.every_player_acted += u64::from(s.acted.iter().all(|&n| n >= 1));
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

/// The formation the real-process smokes fight (`scripts/battle-smoke.sh --formation`, `slice-smoke.sh`): since protocol
/// v9 the players fight beside the canon Cloud and Barret, who kill md1stin's weak formations (301 to 303) in about a
/// hundred ticks, before every player has had a turn (measured by `how_the_players_fare_beside_the_canon_party`), so the
/// smokes fight a sturdier one of the game's own data, chosen by `which_formations_give_every_player_a_turn_...`.
const SMOKE_FORMATION: u16 = 12;

/// The gate behind `scripts/battle-smoke.sh` (protocol v9): the smokes' players (headless bots on Barret's curve, at the
/// level the canon party leader has) fight [`SMOKE_FORMATION`] beside the canon Cloud and Barret, alone, with one other
/// player and with two, at the smoke's pace and at the engine's own, and must win it EVERY time (200 seeds each) with
/// EVERY player attacking at least once, in a fight that is not too long to sit through. The formation can also be run
/// from (the smoke's `--commands` mode escapes from it).
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_smoke_formation_is_won_every_time_with_every_player_acting_beside_the_canon_party() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let formation = data.formation(SMOKE_FORMATION).expect("the smoke formation exists");
    assert!(!formation.preemptive && !formation.back_attack && formation.enemies >= 1);
    let scene = data.scenes().formation(usize::from(SMOKE_FORMATION)).expect("it is in scene.bin");
    assert!(scene.setup().run_difficulty().is_some(), "the smoke's escape run needs a formation that can be run from");
    // (speed, ticks per second, longest acceptable fight in seconds)
    for (speed, tps, limit_s) in [(0u8, 240u32, 20u64), (128, 60, 120)] {
        for (characters, what) in [(&[1u8][..], "one player"), (&[1, 1][..], "two players"), (&[1, 1, 1][..], "three players")] {
            let s = stats_with(&data, SMOKE_FORMATION, characters, None, true, speed, tps, 200);
            let longest_s = s.longest_ticks / u64::from(tps);
            eprintln!(
                "formation {SMOKE_FORMATION} at speed {speed}, {tps} ticks/s, {what} beside Cloud and Barret: won {}/200, every player acted in {}, fewest attacks by a player {}, longest {} ticks ({longest_s} s), mean {} ticks",
                s.won, s.every_player_acted, s.fewest_attacks, s.longest_ticks, s.mean_ticks
            );
            assert_eq!((s.won, s.lost), (200, 0), "{what}: every fight is won");
            assert_eq!(s.every_player_acted, 200, "{what}: in every fight every player attacks at least once");
            assert!(longest_s <= limit_s, "{what}: the longest fight takes {longest_s} s (limit {limit_s} s)");
        }
    }
}

/// The engine ALONE (no canon party; before protocol v9 this was the gate behind `scripts/battle-smoke.sh`): two
/// auto-attackers must beat each of md1stin's two-enemy formations EVERY time, both of them must attack in every fight,
/// and the fight must be short at the smoke's pace and still reasonable at the engine's default pace. 200 seeds each.
///
/// Who the two are: since accounts play their own character (M2-d) the smoke's headless bots both
/// register without a look, so both are Cloud, at the level he starts at (6) in the first fight and
/// at 7 in the second one (the first win levels him up on the real data: he starts six experience
/// short of level 7). Both are checked, and Cloud with Barret (the pair this gate was first written
/// for, still what two different looks field) at their starting levels.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_first_fields_two_enemy_formations_are_won_every_time_by_two_players_alone_and_quickly() {
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

/// MEASUREMENT (protocol v9), not a gate: with the canon Cloud and Barret fighting first (the server's own companion
/// rule), do the players still get turns before the weak enemies of md1stin's table die? Prints, for every formation of
/// the table, solo and with two players on Barret's curve (what a headless bot is), at the smoke's pace and at the engine's
/// own, the share of fights (of 200) in which every player attacked at least once and the fewest attacks any made.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn how_the_players_fare_beside_the_canon_party() {
    let Some(source) = source() else { return };
    let opened = open_ff7(&source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let data = battle_data(&source);
    let table = {
        let field = opened.adapter.load_field("md1stin").expect("md1stin");
        decode_encounter_section(field.field_file().section(section::ENCOUNTER).expect("section")).expect("md1stin has a table")
    };
    let mut formations: Vec<u16> = table.entries.iter().map(|e| e.formation).collect();
    // Formations to compare with, named with FMV_EXTRA_FORMATIONS=12,140 (the sturdier ones the scan above finds).
    formations.extend(std::env::var("FMV_EXTRA_FORMATIONS").unwrap_or_default().split(',').filter_map(|f| f.trim().parse::<u16>().ok()));
    formations.sort_unstable();
    formations.dedup();
    eprintln!("formation | enemies | pace | players | won/200 | every player acted | fewest attacks | mean ticks | longest ticks | wins with a death");
    for formation in formations {
        let enemies = data.formation(formation).map_or(0, |f| f.enemies);
        for (speed, tps) in [(0u8, 240u32), (128, 60)] {
            for (characters, label) in [(&[1u8][..], "solo"), (&[1, 1][..], "two"), (&[1, 1, 1][..], "three")] {
                let s = stats_with(&data, formation, characters, None, true, speed, tps, 200);
                eprintln!(
                    "{formation:>9} | {enemies:>7} | {speed}/{tps} | {label:>7} | {:>7} | {:>18} | {:>14} | {:>10} | {:>13} | {}",
                    s.won, s.every_player_acted, s.fewest_attacks, s.mean_ticks, s.longest_ticks, s.with_losses
                );
            }
        }
    }
}

/// MEASUREMENT (protocol v9), not a gate: which formations of the game keep a fight going long enough that EVERY player
/// beside the canon party gets a turn, and are still won every time? Scans the formations that exist, with two players on
/// Barret's curve and the canon Cloud and Barret, at the smoke's pace (40 seeds each), and prints the ones that are won
/// 40 of 40 times with every player acting in all 40, the sturdiest first. It is how a formation for the real-process smokes
/// that need the players to act (`scripts/battle-smoke.sh`) was chosen.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn which_formations_give_every_player_a_turn_beside_the_canon_party() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let mut found: Vec<(u64, u16, usize, u64, bool, bool)> = Vec::new();
    let mut scanned = 0usize;
    for id in 0..1024u16 {
        let Some(info) = data.formation(id) else { continue };
        scanned += 1;
        let hp: u64 = data.scenes().formation(usize::from(id)).map_or(0, |f| f.enemies().iter().map(|e| u64::from(e.record.hp)).sum());
        let s = stats_with(&data, id, &[1, 1], None, true, 0, 240, 40);
        if s.won == 40 && s.every_player_acted == 40 {
            found.push((s.mean_ticks, id, info.enemies, hp, info.preemptive, info.back_attack));
        }
    }
    found.sort_unstable_by_key(|f| std::cmp::Reverse(f.0));
    eprintln!("{scanned} formations scanned; won 40/40 with both players acting in all 40: {}", found.len());
    eprintln!("mean ticks (at 240/s) | formation | enemies | total enemy HP | preemptive | back attack");
    for (ticks, id, enemies, hp, pre, back) in found.iter().take(40) {
        let escape = data.scenes().formation(usize::from(*id)).and_then(|f| f.setup().run_difficulty());
        eprintln!("{ticks:>21} | {id:>9} | {enemies:>7} | {hp:>14} | {pre:>10} | {back:>11} | run difficulty {escape:?}");
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

/// A fight with the real Guard Scorpion, whose General counter (the Tail Laser, on the whole party) and Death counter
/// run inside the commands that provoke them: four players (Cloud, Barret, Tifa and Aerith, so that the enemy's
/// script slots hold more fighters than the three the original has) defend until the Scorpion's tail is up, then
/// strike it, the way a server drives a battle; every message the server would send (the engine's tick, its events,
/// the command that caused them) is then fed to a client's own engine, which must reproduce each one exactly and end
/// in the very same state. Run at the engine's own pace and at the smoke's, with several seeds.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn a_guard_scorpion_fight_with_its_counters_is_replayed_exactly_by_a_client_engine() {
    use fmv_ff7_battle::rules::ActorId;
    use fmv_protocol::{AppliedCommand, BattleReplay};

    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let scenes = data.scenes();
    let formation = (0..scenes.formation_count())
        .find(|&f| scenes.formation(f).is_some_and(|fr| fr.enemies().iter().any(|e| e.record.name == "Guard Scorpion")))
        .and_then(|f| u16::try_from(f).ok())
        .expect("a formation has the Guard Scorpion");
    let info = data.formation(formation).expect("it has enemies");
    let characters = [0u8, 1, 2, 3];
    let mut counter_batches = 0usize;
    let mut tails_up = 0usize;
    for (speed, tps) in [(128u8, 60u32), (0, 240)] {
        for seed in 0..6u64 {
            let party: Vec<fmv_protocol::BattleMember> = characters
                .iter()
                .enumerate()
                .map(|(i, &character)| {
                    fmv_protocol::BattleMember::player(fmv_core::EntityId(i as u64 + 1), fmv_core::AccountId(i as u64 + 1), format!("P{i}"), character, 20, 0)
                })
                .collect();
            let setup = BattleSetup {
                formation_id: formation,
                seed,
                party,
                preemptive: info.preemptive,
                back_attack: info.back_attack,
                battle_speed: speed,
                ticks_per_second: tps,
            };
            let foe = ActorId(characters.len() as u8);
            let mut state = data.build(&setup).expect("builds");
            assert!(state.actor(foe).is_some_and(|c| c.ai.is_some()), "the Scorpion has its scripts");
            let mut log: Vec<(u64, Vec<BattleEvent>, Option<AppliedCommand>)> = Vec::new();
            let slice = u64::from((tps / 20).max(1));
            for _ in 0..400_000u32 {
                if state.outcome().is_some() {
                    break;
                }
                let events = state.step_ticks(slice);
                if !events.is_empty() {
                    log.push((state.tick(), events, None));
                }
                for id in state.ready_actors() {
                    let tail_up = state.actor(foe).and_then(|c| c.ai.as_ref()).is_some_and(|ai| ai.stats().turns >= 5);
                    let command = match state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) {
                        Some(target) if tail_up => Command::Attack { target },
                        _ => Command::Defend,
                    };
                    if let Ok(events) = state.submit_command(id, command) {
                        counter_batches += usize::from(events.iter().any(|e| matches!(e, BattleEvent::Action { actor, .. } if *actor == foe)));
                        log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                    }
                }
            }
            tails_up += usize::from(state.actor(foe).and_then(|c| c.ai.as_ref()).is_some_and(|ai| ai.stats().turns >= 5));
            // The client: its own engine from its own data, advanced only by what the messages say.
            let mut client = BattleReplay::start(&*data, &setup).expect("the client builds the same battle");
            for (tick, events, command) in &log {
                client.apply(*tick, events, command.as_ref()).unwrap_or_else(|d| panic!("speed {speed}, seed {seed}: the client diverged: {d}"));
            }
            assert_eq!(client.state(), &state, "speed {speed}, seed {seed}: the same final state");
        }
    }
    eprintln!("Guard Scorpion (formation {formation}): {counter_batches} commands were answered by a counter, in {tails_up} of 12 fights its tail came up; every message replayed exactly");
    assert!(tails_up > 0, "the Scorpion's tail came up in some fight");
    assert!(counter_batches > 0, "a counter came inside a command's own batch of events");
}
