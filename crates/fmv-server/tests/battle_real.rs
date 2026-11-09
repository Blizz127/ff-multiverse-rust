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
use fmv_server::companion::{self, Pacing, PlayerTurn};
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

/// The companions' turns after one step of the engine, as the server's `companions_act` takes them: while
/// `companion::next_move` (the one definition of who goes next) gives one, it is applied. The first `companion_count` actors are the
/// companions and the rest the players; `commands`, `ready_since` and `last_action` are the bookkeeping the server keeps (the commands
/// each fighter was given, the tick each full gauge has been full since, the tick of the last command of a party member). Returns the
/// events of each command applied, in order.
fn companions_go(
    state: &mut fmv_ff7_battle::rules::BattleState,
    pacing: Pacing,
    companion_count: usize,
    commands: &mut [u32],
    ready_since: &mut [Option<u64>],
    last_action: &mut Option<u64>,
) -> Vec<Vec<BattleEvent>> {
    use fmv_ff7_battle::rules::ActorId;
    let fighters = commands.len();
    let mut applied = Vec::new();
    for _ in 0..fighters {
        if state.outcome().is_some() {
            break;
        }
        let now = state.tick();
        let ready: Vec<companion::Ready> = state
            .ready_actors()
            .into_iter()
            .filter(|id| usize::from(id.0) < companion_count)
            .map(|actor| companion::Ready { actor, since: ready_since[usize::from(actor.0)].unwrap_or(now) })
            .collect();
        let players: Vec<PlayerTurn> = (companion_count..fighters)
            .filter(|&i| state.actor(ActorId(i as u8)).is_some_and(|c| c.is_alive()))
            .map(|i| PlayerTurn { commands: commands[i], ready_since: ready_since[i] })
            .collect();
        let Some((actor, command, _)) = companion::next_move(state, pacing, *last_action, &players, &ready) else { break };
        let Ok(events) = state.submit_command(actor, command) else { break };
        commands[usize::from(actor.0)] += 1;
        ready_since[usize::from(actor.0)] = None;
        *last_action = Some(state.tick());
        applied.push(events);
    }
    applied
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
    /// Attacks made by the canon companions, all together.
    companion_attacks: usize,
    /// A companion's command ended the battle while a living player had not had a turn: what the companions' pacing is there to
    /// prevent (`companion::may_act`, "the players' first turn").
    ended_before_a_players_turn: bool,
    /// Turns the enemies took (announced actions of theirs, counters included), and the tick of the first.
    enemy_actions: usize,
    first_enemy_action: Option<u64>,
}

/// One simulated battle (see [`Sim`]) with the server's companion pacing (`companion::may_act`: each companion on its own clock, a player's first turn kept) and players who take
/// `player_delay_ms` of battle time to choose once their gauge is full (0: a headless bot, which answers at once). The
/// order within a slice is the server's: the engine steps, the companions that may act do (`companions_act` runs after
/// every step), then the players' commands arrive.
#[allow(clippy::too_many_arguments)]
fn simulate_paced(
    data: &Ff7BattleData,
    formation: u16,
    characters: &[u8],
    level: Option<u8>,
    canon: bool,
    seed: u64,
    speed: u8,
    tps: u32,
    pacing: Pacing,
    player_delay_ms: u32,
) -> Sim {
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
    let fighters = companion_count + characters.len();
    let mut acted = vec![0usize; fighters];
    let (mut enemy_actions, mut first_enemy_action) = (0usize, None::<u64>);
    let mut ended_before_a_players_turn = false;
    let mut commands = vec![0u32; fighters];
    let mut ready_since: Vec<Option<u64>> = vec![None; fighters];
    let mut last_action: Option<u64> = None;
    let player_delay = (u64::from(player_delay_ms) * u64::from(tps)).div_ceil(1000);
    let slice = u64::from((tps / 20).max(1));
    // What each batch of events says: the attacks (damage or miss) of the party's members and the turns of the enemies.
    let mut count = |events: &[BattleEvent], tick: u64| {
        for e in events {
            match e {
                BattleEvent::Damage { actor, .. } | BattleEvent::Miss { actor, .. } if usize::from(actor.0) < fighters => acted[usize::from(actor.0)] += 1,
                BattleEvent::Action { actor, .. } if usize::from(actor.0) >= fighters => {
                    enemy_actions += 1;
                    first_enemy_action.get_or_insert(tick);
                }
                _ => {}
            }
        }
    };
    for _ in 0..2_000_000u32 {
        if state.outcome().is_some() {
            break;
        }
        let events = state.step_ticks(slice);
        count(&events, state.tick());
        // When each gauge filled (what the pacing measures from), as the server notes it after every step.
        let now = state.tick();
        let ready = state.ready_actors();
        for (i, since) in ready_since.iter_mut().enumerate() {
            *since = if ready.contains(&fmv_ff7_battle::rules::ActorId(i as u8)) { Some(since.unwrap_or(now)) } else { None };
        }
        // The companions first (the server's `companions_act` runs right after the step), each when the pacing lets it.
        for events in companions_go(&mut state, pacing, companion_count, &mut commands, &mut ready_since, &mut last_action) {
            count(&events, now);
            if events.contains(&BattleEvent::Victory) {
                // The command that won the battle was a companion's: did a living player still owe its first turn?
                ended_before_a_players_turn |= (companion_count..fighters)
                    .any(|i| commands[i] == 0 && state.actor(fmv_ff7_battle::rules::ActorId(i as u8)).is_some_and(|c| c.is_alive()));
            }
        }
        // Then the players' commands arrive, each once its player has chosen.
        for id in state.ready_actors().into_iter().filter(|id| usize::from(id.0) >= companion_count) {
            let i = usize::from(id.0);
            if now.saturating_sub(ready_since[i].unwrap_or(now)) < player_delay {
                continue;
            }
            let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else { break };
            if let Ok(events) = state.submit_command(id, Command::Attack { target }) {
                count(&events, now);
                commands[i] += 1;
                ready_since[i] = None;
                last_action = Some(state.tick());
            }
        }
    }
    let dead = state.party_ids().into_iter().filter(|id| !state.actor(*id).is_some_and(|c| c.is_alive())).count();
    let players = acted.split_off(companion_count);
    Sim {
        outcome: state.outcome(),
        ticks: state.tick(),
        companion_attacks: acted.iter().sum(),
        acted: players,
        dead,
        ended_before_a_players_turn,
        enemy_actions,
        first_enemy_action,
    }
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
    /// Won fights in which a companion's command ended the battle while a living player had not had a turn: the shut-out the
    /// companions' pacing is there to prevent.
    companions_ended_it_before_a_turn: u64,
    /// Fights (of the won ones) in which an enemy took at least one turn, and the turns they took in all.
    fights_with_enemy_turns: u64,
    enemy_turns: u64,
    /// The mean tick of the first turn an enemy took, over the fights in which one did (0 when none did).
    mean_first_enemy_tick: u64,
    /// Attacks (damage or miss) the party made in all the fights, the companions' and the players' together.
    party_attacks: u64,
}

fn stats(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, speed: u8, tps: u32, seeds: u64) -> Stats {
    stats_with(data, formation, characters, level, false, speed, tps, seeds)
}

#[allow(clippy::too_many_arguments)]
fn stats_with(data: &Ff7BattleData, formation: u16, characters: &[u8], level: Option<u8>, canon: bool, speed: u8, tps: u32, seeds: u64) -> Stats {
    stats_paced(data, formation, characters, level, canon, speed, tps, seeds, Pacing::OFF, 0)
}

#[allow(clippy::too_many_arguments)]
fn stats_paced(
    data: &Ff7BattleData,
    formation: u16,
    characters: &[u8],
    level: Option<u8>,
    canon: bool,
    speed: u8,
    tps: u32,
    seeds: u64,
    pacing: Pacing,
    player_delay_ms: u32,
) -> Stats {
    let mut out = Stats {
        won: 0,
        lost: 0,
        with_losses: 0,
        longest_ticks: 0,
        mean_ticks: 0,
        fewest_attacks: usize::MAX,
        every_player_acted: 0,
        companions_ended_it_before_a_turn: 0,
        fights_with_enemy_turns: 0,
        enemy_turns: 0,
        mean_first_enemy_tick: 0,
        party_attacks: 0,
    };
    let (mut total, mut first_enemy_total) = (0u64, 0u64);
    for seed in 0..seeds {
        let s = simulate_paced(data, formation, characters, level, canon, seed, speed, tps, pacing, player_delay_ms);
        match s.outcome {
            Some(Outcome::Victory) => {
                out.won += 1;
                out.with_losses += u64::from(s.dead > 0);
                out.fewest_attacks = out.fewest_attacks.min(s.acted.iter().copied().min().unwrap_or(0));
                out.every_player_acted += u64::from(s.acted.iter().all(|&n| n >= 1));
                out.companions_ended_it_before_a_turn += u64::from(s.ended_before_a_players_turn);
            }
            Some(Outcome::Defeat) => out.lost += 1,
            Some(Outcome::Escaped) => panic!("formation {formation} seed {seed}: nobody gave Escape"),
            None => panic!("formation {formation} seed {seed}: the battle never ended"),
        }
        out.longest_ticks = out.longest_ticks.max(s.ticks);
        total += s.ticks;
        out.fights_with_enemy_turns += u64::from(s.enemy_actions > 0);
        out.enemy_turns += s.enemy_actions as u64;
        out.party_attacks += (s.companion_attacks + s.acted.iter().sum::<usize>()) as u64;
        first_enemy_total += s.first_enemy_action.unwrap_or(0);
    }
    out.mean_ticks = total / seeds.max(1);
    out.mean_first_enemy_tick = first_enemy_total / out.fights_with_enemy_turns.max(1);
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
/// hundred ticks, so a smoke whose every bot must ACT fights a sturdier one of the game's own data, chosen by
/// `which_formations_give_every_player_a_turn_...`. (It is one of the game's test formations: two enemies of 1000 HP that act
/// four times as often as a party member; no first-reactor fight is that long, which is why it is the one that lasts. The
/// first-reactor formations are gated by `with_pacing_every_player_acts_in_every_fight_of_the_first_field`.)
const SMOKE_FORMATION: u16 = 12;

/// The gate behind `scripts/battle-smoke.sh` (protocol v9): the smokes' players (headless bots on Barret's curve, at the
/// level the canon party leader has) fight [`SMOKE_FORMATION`] beside the canon Cloud and Barret, alone, with one other
/// player and with two, at the smoke's pace and at the engine's own, and must win it EVERY time (200 seeds each) with
/// EVERY player attacking at least once, in a fight that is not too long to sit through. The companions follow the
/// server's default pacing (`Pacing::default()`: each on its own clock, a player's first turn kept), as on a real server.
/// The formation can also be run from (the smoke's `--commands` mode escapes from it).
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
    // At the engine's own pace a lone player's 16 or so attacks, which the paced companions follow, take two minutes.
    for (speed, tps, limit_s) in [(0u8, 240u32, 20u64), (128, 60, 150)] {
        for (characters, what) in [(&[1u8][..], "one player"), (&[1, 1][..], "two players"), (&[1, 1, 1][..], "three players")] {
            let s = stats_paced(&data, SMOKE_FORMATION, characters, None, true, speed, tps, 200, Pacing::default(), 0);
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

/// MEASUREMENT (protocol v9, the first build: companions WITHOUT pacing), not a gate: with the canon Cloud and Barret
/// acting the instant their gauge fills (the server's own companion rule), do the players still get turns before the weak
/// enemies of md1stin's table die? (They did not, which is why the server paces them: see
/// `with_pacing_every_player_acts_in_every_fight_of_the_first_field`.) Prints, for every formation of
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

/// The fields of the first Mako reactor: the walk from the station to the reactor's core, whose encounter tables
/// (`md1stin`, `md1_1` and `md1_2` share one) send the fights of the first mission.
const FIRST_REACTOR_FIELDS: [&str; 8] = ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "nmkin_2", "nmkin_3", "nmkin_4"];

/// The formations the first reactor's random encounters send, sorted, from the real encounter tables of its fields.
fn first_reactor_formations(source: &Ff7Source) -> Vec<u16> {
    let opened = open_ff7(source).expect("the library opens");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(prewarm(&opened.adapter)).expect("prewarm");
    let mut formations: Vec<u16> = Vec::new();
    for key in FIRST_REACTOR_FIELDS {
        let field = opened.adapter.load_field(key).unwrap_or_else(|e| panic!("{key}: {e}"));
        let table = decode_encounter_section(field.field_file().section(section::ENCOUNTER).expect("section")).unwrap_or_else(|| panic!("{key} has a table"));
        formations.extend(table.entries.iter().map(|e| e.formation));
    }
    formations.sort_unstable();
    formations.dedup();
    formations
}

/// The gate behind the companions' pacing (`fmv_server::companion::may_act`, the server's default `Pacing`): beside the
/// canon Cloud and Barret, in every formation the first reactor sends (the encounter tables of its eight fields, 301 to 322), alone,
/// with one other player and with two, every fight is still won, no companion's command ever ends a fight in which a living player
/// has not had a turn (`companions_ended_it_before_a_turn`), and a SOLO player attacks in every fight, whether the players answer at
/// once (a headless bot) or take a second and a half to choose (a person with the menu that opens by itself); and no
/// fight is drawn out past what one sits through. With more players the first reactor's weak enemies sometimes die to the first
/// players' blows before the last one's turn, which is the players' own doing and is printed, not gated. Without pacing
/// (`how_the_players_fare_beside_the_canon_party`) a solo player attacks in only 44-87% of md1stin's fights, two players in
/// 13-47% and three in 0-12%. 200 seeds each. The smokes'
/// sturdier [`SMOKE_FORMATION`] is fought too (won, every player acting) but not timed: it is chosen to last.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn with_pacing_every_player_acts_in_every_fight_of_the_first_field() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let mut formations = first_reactor_formations(&source);
    let first_field = formations.clone();
    formations.push(SMOKE_FORMATION);
    let pacing = Pacing::default();
    eprintln!("formation | pace | players | chooser | won/200 | every player acted | companions ended it early | fewest attacks | mean ticks | longest ticks (s)");
    for formation in formations {
        // (speed, ticks per second, longest acceptable fight in seconds): the smoke's pace and the engine's own.
        for (speed, tps, limit_s) in [(0u8, 240u32, 60u64), (128, 60, 120)] {
            for (characters, label) in [(&[1u8][..], "solo"), (&[1, 1][..], "two"), (&[1, 1, 1][..], "three")] {
                for delay_ms in [0u32, 1_500] {
                    let s = stats_paced(&data, formation, characters, None, true, speed, tps, 200, pacing, delay_ms);
                    let longest_s = s.longest_ticks / u64::from(tps);
                    eprintln!(
                        "{formation:>9} | {speed}/{tps} | {label:>7} | {delay_ms:>5} ms | {:>7} | {:>17} | {:>25} | {:>14} | {:>10} | {} ({longest_s})",
                        s.won, s.every_player_acted, s.companions_ended_it_before_a_turn, s.fewest_attacks, s.mean_ticks, s.longest_ticks
                    );
                    assert_eq!((s.won, s.lost), (200, 0), "formation {formation}, {label}, {delay_ms} ms: every fight is won");
                    assert_eq!(s.companions_ended_it_before_a_turn, 0, "formation {formation}, {label}, {delay_ms} ms: a companion never ends a fight before a player's turn");
                    if characters.len() == 1 {
                        assert_eq!(s.every_player_acted, 200, "formation {formation}, {label}, {delay_ms} ms: a solo player attacks in every fight");
                    }
                    if first_field.contains(&formation) {
                        assert!(longest_s <= limit_s, "formation {formation}, {label}, {delay_ms} ms: the longest fight takes {longest_s} s (limit {limit_s} s)");
                    }
                }
            }
        }
    }
}

/// The enemies take their turns on their own gauges while a player sits in a menu (FF7's Active mode, `companion`, "Active, Recommended
/// and Wait"): three players on whom nobody ever gives a command (their gauges fill and stay full: three menus left open) are
/// attacked in every formation of the first reactor, the first enemy turn coming within the first seconds of the fight. (A formation
/// of Mono Drives alone is the exception that proves the script: their own AI lets their first turn pass.)
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_enemies_take_their_turns_while_the_players_sit_in_their_menus() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let formations: Vec<u16> = first_reactor_formations(&source);
    let (speed, tps) = (128u8, 60u32);
    eprintln!("formation | enemies | seeds with an enemy turn in the first 12 s (of 200) | mean first enemy turn (s) | latest first enemy turn (s)");
    for formation in formations {
        let info = data.formation(formation).expect("a formation");
        let members: Vec<fmv_protocol::BattleMember> = (0..3u64)
            .map(|i| fmv_protocol::BattleMember::player(fmv_core::EntityId(i + 1), fmv_core::AccountId(i + 1), format!("P{i}"), [0u8, 1, 1][i as usize], 6, 0))
            .collect();
        let (mut early, mut first_total, mut latest, mut acted) = (0u64, 0f64, 0f64, 0u64);
        for seed in 0..200u64 {
            let setup = BattleSetup { formation_id: formation, seed, party: members.clone(), preemptive: info.preemptive, back_attack: info.back_attack, battle_speed: speed, ticks_per_second: tps };
            let mut state = data.build(&setup).expect("builds");
            let mut first: Option<f64> = None;
            for _ in 0..(60 * u64::from(tps)) {
                for e in state.step_ticks(1) {
                    if let BattleEvent::Action { actor, .. } = e {
                        if usize::from(actor.0) >= members.len() && first.is_none() {
                            first = Some(state.tick() as f64 / f64::from(tps));
                        }
                    }
                }
                if first.is_some() || state.outcome().is_some() {
                    break;
                }
            }
            if let Some(t) = first {
                acted += 1;
                first_total += t;
                latest = latest.max(t);
                early += u64::from(t <= 12.0);
            }
        }
        let mean = first_total / acted.max(1) as f64;
        eprintln!("{formation:>9} | {:>7} | {early:>51} | {mean:>25.1} | {latest:.1}", info.enemies);
        assert_eq!(acted, 200, "formation {formation}: an enemy takes a turn in every fight within a minute, whatever the players do");
        if formation != 305 {
            assert!(early >= 190, "formation {formation}: in {early} of 200 fights an enemy took a turn within 12 s");
        }
    }
}

/// What a fight of the first reactor looks like, and whether a third fighter changes it. FF7's own party there is TWO, Cloud and
/// Barret, both commanded by one person (a second and a half over each command: the reference); the server's is the canon pair on
/// their own clocks and one player on Barret's curve (the same second and a half). Prints, for every formation the reactor's fields
/// send, per party: how many of 200 fights are won, the mean length, the party's attacks and the enemies' turns per fight, in how many
/// fights an enemy took a turn at all, and when the first one came. The gates are the ones that hold whatever the balance: every
/// fight is won, and the enemies take their turns in both.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn how_the_first_reactors_fights_flow_for_the_canon_pair_alone_and_with_a_player() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let (speed, tps) = (128u8, 60u32);
    let secs = |ticks: u64| ticks as f64 / f64::from(tps);
    eprintln!("formation | enemies | party | won/200 | mean secs | party attacks | enemy turns | fights with an enemy turn | first enemy turn (s)");
    let mut totals = [(0u64, 0u64, 0u64, 0u64); 2];
    let formations = first_reactor_formations(&source);
    for &formation in &formations {
        let enemies = data.formation(formation).map_or(0, |f| f.enemies);
        // The reference: the pair as two players (Cloud, Barret), nobody to command for.
        let two = stats_paced(&data, formation, &[0, 1], None, false, speed, tps, 200, Pacing::default(), 1_500);
        let three = stats_paced(&data, formation, &[1], None, true, speed, tps, 200, Pacing::default(), 1_500);
        for (i, (label, s)) in [("canon pair only", two), ("pair + player", three)].into_iter().enumerate() {
            eprintln!(
                "{formation:>9} | {enemies:>7} | {label:>15} | {:>7} | {:>9.1} | {:>13.2} | {:>11.2} | {:>25} | {:.1}",
                s.won,
                secs(s.mean_ticks),
                s.party_attacks as f64 / 200.0,
                s.enemy_turns as f64 / 200.0,
                s.fights_with_enemy_turns,
                secs(s.mean_first_enemy_tick)
            );
            assert_eq!((s.won, s.lost), (200, 0), "formation {formation}, {label}: every fight is won");
            assert!(s.fights_with_enemy_turns > 0 || formation == 305, "formation {formation}, {label}: the enemies take turns");
            totals[i].0 += s.mean_ticks;
            totals[i].1 += s.enemy_turns;
            totals[i].2 += s.fights_with_enemy_turns;
            totals[i].3 += s.party_attacks;
        }
    }
    let n = formations.len() as f64;
    for (label, (ticks, turns, fights, attacks)) in ["canon pair only", "pair + player"].into_iter().zip(totals) {
        eprintln!(
            "{label}, averaged over the {} formations: {:.1} s a fight, {:.2} party attacks and {:.2} enemy turns a fight, an enemy turn in {:.0}% of the fights",
            formations.len(),
            secs(ticks) / n,
            attacks as f64 / 200.0 / n,
            turns as f64 / 200.0 / n,
            fights as f64 / 200.0 / n * 100.0
        );
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
/// run inside the commands that provoke them. Two rosters: four players (Cloud, Barret, Tifa and Aerith, so that the
/// enemy's script slots hold more fighters than the three the original has), and the real first-reactor shape, the
/// canon Cloud and Barret as the server's companions (full limit gauges, commanded by the server's own rule,
/// `fmv_server::companion::command`) and two players. They defend until the Scorpion's tail is up, then strike it (the
/// companions as the server's rule says, a limit break among it), the way a server drives a battle; every message
/// the server would send (the engine's tick, its events, the command that caused them) is then fed to a client's own
/// engine, which must reproduce each one exactly and end in the very same state. Run at the engine's own pace and at
/// the smoke's, with several seeds.
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn a_guard_scorpion_fight_with_its_counters_is_replayed_exactly_by_a_client_engine() {
    use fmv_ff7_battle::rules::ActorId;
    use fmv_protocol::{AppliedCommand, BattleReplay};

    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let scenes = data.scenes();
    let with_scorpion: Vec<u16> = (0..scenes.formation_count())
        .filter(|&f| scenes.formation(f).is_some_and(|fr| fr.enemies().iter().any(|e| e.record.name == "Guard Scorpion")))
        .filter_map(|f| u16::try_from(f).ok())
        .collect();
    // `scripts/battle-smoke.sh --boss` hard-codes this number (BOSS_FORMATION): it is the one formation that has the boss.
    assert_eq!(with_scorpion, [SMOKE_BOSS_FORMATION], "the Guard Scorpion's formation moved: change BOSS_FORMATION in scripts/battle-smoke.sh");
    let formation = with_scorpion[0];
    let info = data.formation(formation).expect("it has enemies");
    let (mut counter_batches, mut companion_counter_batches, mut limit_counter_batches) = (0usize, 0usize, 0usize);
    let mut tails_up = 0usize;
    let mut fights = 0usize;
    for canon in [false, true] {
        for (speed, tps) in [(128u8, 60u32), (0, 240)] {
            for seed in 0..6u64 {
                let party: Vec<fmv_protocol::BattleMember> = if canon {
                    let companions = [0u8, 1].map(|c| fmv_protocol::BattleMember::companion(data.character_name(c).expect("a canon name"), c, 20, 255));
                    let players = [2u8, 3].iter().enumerate().map(|(i, &character)| {
                        fmv_protocol::BattleMember::player(fmv_core::EntityId(i as u64 + 1), fmv_core::AccountId(i as u64 + 1), format!("P{i}"), character, 20, 0)
                    });
                    companions.into_iter().chain(players).collect()
                } else {
                    [0u8, 1, 2, 3]
                        .iter()
                        .enumerate()
                        .map(|(i, &character)| {
                            fmv_protocol::BattleMember::player(fmv_core::EntityId(i as u64 + 1), fmv_core::AccountId(i as u64 + 1), format!("P{i}"), character, 20, 0)
                        })
                        .collect()
                };
                let companion_count = party.iter().filter(|m| m.is_companion()).count();
                let setup = BattleSetup {
                    formation_id: formation,
                    seed,
                    party,
                    preemptive: info.preemptive,
                    back_attack: info.back_attack,
                    battle_speed: speed,
                    ticks_per_second: tps,
                };
                let foe = ActorId(setup.party.len() as u8);
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
                        let first_living = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive()));
                        let command = match first_living {
                            // The server's own rule for a canon companion, once the tail is up.
                            Some(_) if tail_up && usize::from(id.0) < companion_count => {
                                fmv_server::companion::command(&state, id).unwrap_or(Command::Defend)
                            }
                            Some(target) if tail_up => Command::Attack { target },
                            _ => Command::Defend,
                        };
                        if let Ok(events) = state.submit_command(id, command) {
                            let answered = events.iter().any(|e| matches!(e, BattleEvent::Action { actor, .. } if *actor == foe));
                            counter_batches += usize::from(answered);
                            companion_counter_batches += usize::from(answered && usize::from(id.0) < companion_count);
                            limit_counter_batches += usize::from(answered && matches!(command, Command::Limit { .. }));
                            log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                        }
                    }
                }
                tails_up += usize::from(state.actor(foe).and_then(|c| c.ai.as_ref()).is_some_and(|ai| ai.stats().turns >= 5));
                fights += 1;
                // The client: its own engine from its own data, advanced only by what the messages say.
                let mut client = BattleReplay::start(&*data, &setup).expect("the client builds the same battle");
                for (tick, events, command) in &log {
                    client
                        .apply(*tick, events, command.as_ref())
                        .unwrap_or_else(|d| panic!("canon {canon}, speed {speed}, seed {seed}: the client diverged: {d}"));
                }
                assert_eq!(client.state(), &state, "canon {canon}, speed {speed}, seed {seed}: the same final state");
            }
        }
    }
    eprintln!(
        "Guard Scorpion (formation {formation}): {counter_batches} commands were answered by a counter ({companion_counter_batches} \
         were a companion's, {limit_counter_batches} a limit break), in {tails_up} of {fights} fights its tail came up; every message replayed exactly"
    );
    assert!(tails_up > 0, "the Scorpion's tail came up in some fight");
    assert!(counter_batches > 0, "a counter came inside a command's own batch of events");
    assert!(companion_counter_batches > 0, "a counter came inside a canon companion's command");
}

/// The Guard Scorpion's formation, the level, the pace, the script and the Tail Laser's attack id of `scripts/battle-smoke.sh --boss`
/// (`BOSS_FORMATION`, `BOSS_LEVEL`, `BATTLE_SPEED`/`TICK_RATE`, `BOSS_SCRIPT`, `BOSS_COUNTER_ATTACK`): the numbers its header quotes are
/// measured here.
const SMOKE_BOSS_FORMATION: u16 = 324;
const SMOKE_BOSS_LEVEL: u8 = 8;
const SMOKE_BOSS_DEFENDS: usize = 2;
const SMOKE_BOSS_TAIL_LASER: u16 = 286;

/// One fight of the boss smoke at the engine level: the canon Cloud and Barret as the server's companions (its own rule, its own
/// pacing), two players on the Barret curve who defend `defends` times and then attack the first living enemy, all at `level`, at the
/// smoke's pace (battle speed 0, 240 ticks a second). Returns the outcome and how many of the boss's attacks that answered a command
/// were `attack` (a counter: a command never advances the clock).
fn boss_fight(data: &Ff7BattleData, formation: u16, level: u8, defends: usize, seed: u64, attack: u16) -> (Option<Outcome>, usize) {
    use fmv_ff7_battle::rules::{ActionRef, ActorId};
    let (speed, tps) = (0u8, 240u32);
    let players = (0..2u64).map(|i| fmv_protocol::BattleMember::player(fmv_core::EntityId(i + 1), fmv_core::AccountId(i + 1), format!("P{i}"), 1, level, 0));
    let companions = [0u8, 1].map(|c| fmv_protocol::BattleMember::companion(data.character_name(c).expect("a canon name"), c, level, 0));
    let companion_count = companions.len();
    let party: Vec<fmv_protocol::BattleMember> = companions.into_iter().chain(players).collect();
    let fighters = party.len();
    let info = data.formation(formation).expect("a real formation");
    let setup = BattleSetup { formation_id: formation, seed, party, preemptive: info.preemptive, back_attack: info.back_attack, battle_speed: speed, ticks_per_second: tps };
    let mut state = data.build(&setup).expect("builds");
    let foe = ActorId(fighters as u8);
    let pacing = Pacing::default();
    let (mut commands, mut ready_since) = (vec![0u32; fighters], vec![None::<u64>; fighters]);
    let mut last_action: Option<u64> = None;
    let mut defends_left = vec![defends; fighters];
    let mut answered = 0usize;
    let slice = u64::from((tps / 20).max(1));
    let mut count = |events: &[BattleEvent]| {
        answered += events.iter().filter(|e| matches!(e, BattleEvent::Action { actor, action: ActionRef::EnemyAttack(id) } if *actor == foe && *id == attack)).count();
    };
    for _ in 0..2_000_000u32 {
        if state.outcome().is_some() {
            break;
        }
        state.step_ticks(slice);
        let now = state.tick();
        let ready = state.ready_actors();
        for (i, since) in ready_since.iter_mut().enumerate() {
            *since = if ready.contains(&ActorId(i as u8)) { Some(since.unwrap_or(now)) } else { None };
        }
        // The companions first, as the server's `companions_act` runs right after every step, each when the pacing lets it.
        for events in companions_go(&mut state, pacing, companion_count, &mut commands, &mut ready_since, &mut last_action) {
            count(&events);
        }
        // Then the players' commands arrive at once (a headless bot answers as soon as its gauge is full).
        for id in state.ready_actors().into_iter().filter(|id| usize::from(id.0) >= companion_count) {
            let i = usize::from(id.0);
            let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else { break };
            let command = if defends_left[i] > 0 {
                defends_left[i] -= 1;
                Command::Defend
            } else {
                Command::Attack { target }
            };
            if let Ok(events) = state.submit_command(id, command) {
                count(&events);
                commands[i] += 1;
                ready_since[i] = None;
                last_action = Some(state.tick());
            }
        }
    }
    (state.outcome(), answered)
}

/// The numbers the `--boss` smoke's header quotes: at level 8, with the bots defending twice, the Guard Scorpion's Tail Laser answers a
/// command in EVERY one of 40 seeds, and the party wins nearly all of them (37 of 40 measured; a loss is a fine outcome for the smoke
/// but a pace at which most fights were lost would not be one). At the starting level 6 the Laser still fires every time and about one
/// fight in eight is lost (35 of 40), at level 10 none is (40 of 40), which is why the smoke pins the level. (The bots' strategy is
/// the fixed one the smoke gives them, defend twice and then strike whatever the tail is doing: each blow with the tail up draws a Laser
/// on the whole party, so how many of those the party can take, its level, is what the fight is about, as in the game.)
#[test]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
fn the_boss_smoke_fight_answers_with_the_tail_laser_in_every_seed() {
    let Some(source) = source() else { return };
    let data = battle_data(&source);
    let (mut wins, mut lasers_in) = (0, 0);
    let seeds = 40u64;
    for seed in 0..seeds {
        let (outcome, lasers) = boss_fight(&data, SMOKE_BOSS_FORMATION, SMOKE_BOSS_LEVEL, SMOKE_BOSS_DEFENDS, seed, SMOKE_BOSS_TAIL_LASER);
        assert!(outcome.is_some(), "seed {seed}: the fight was never decided");
        wins += usize::from(outcome == Some(Outcome::Victory));
        lasers_in += usize::from(lasers > 0);
    }
    eprintln!("boss smoke fight (formation {SMOKE_BOSS_FORMATION}, level {SMOKE_BOSS_LEVEL}, {SMOKE_BOSS_DEFENDS} defends): won {wins} of {seeds}, a Tail Laser counter in {lasers_in}");
    assert_eq!(lasers_in as u64, seeds, "the Tail Laser answered a command in every seed");
    assert!(wins as u64 * 10 >= seeds * 9, "at least nine fights in ten are won at the smoke's level: {wins} of {seeds}");
}
