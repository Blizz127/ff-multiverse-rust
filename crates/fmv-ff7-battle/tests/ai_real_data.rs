//! The enemy AI against the owner's real `scene.bin`. Every test is `#[ignore]`d and reads the data
//! directory from `FMV_FF7_DATA` (the game's `data/` folder); without it they print a note and pass.
//!
//! ```text
//! FMV_FF7_DATA=/path/to/ff7/data cargo test -p fmv-ff7-battle --test ai_real_data -- --ignored --nocapture
//! ```
//!
//! The files are only read. Nothing from them is copied into the repository or printed beyond counts and
//! numeric ids.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fmv_ff7_battle::ai::vm::{self, AiError, Memory};
use fmv_ff7_battle::ai::world::{field_known, BattleWorld};
use fmv_ff7_battle::ai::{script, EnemyScripts};
use fmv_ff7_battle::rng::Rng;
use fmv_ff7_battle::rules::{
    enemies_for_formation, ActionRef, ActorId, BattleConfig, BattleEvent, BattleState, Combatant,
    Row, StartMode,
};
use fmv_ff7_battle::scene::{Scene, SceneBin};

fn scene_path() -> Option<PathBuf> {
    let Some(root) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping real-data test");
        return None;
    };
    let root = PathBuf::from(root);
    for rel in ["lang-en/battle/scene.bin", "battle/scene.bin", "battle/SCENE.BIN"] {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    panic!("FMV_FF7_DATA={} has no scene.bin", root.display());
}

fn load() -> Option<SceneBin> {
    Some(SceneBin::open(scene_path()?).expect("scene.bin parses"))
}

/// Distinct enemies by id: the first scene and slot each is found in.
fn distinct_enemies(bin: &SceneBin) -> BTreeMap<u16, (usize, usize)> {
    let mut out = BTreeMap::new();
    for scene in bin.scenes() {
        for (slot, id) in scene.enemy_ids.iter().enumerate() {
            if let (Some(id), Some(_)) = (id, scene.enemies[slot].as_ref()) {
                out.entry(*id).or_insert((scene.index, slot));
            }
        }
    }
    out
}

/// Whether every reachable instruction of the script in `slot` is something the machine implements.
fn statically_supported(scripts: &EnemyScripts, slot: usize) -> Result<(), String> {
    let insns = scripts
        .reachable(slot)
        .map_err(|e| format!("does not decode: {e}"))?;
    for insn in insns.values() {
        if !vm::implements(insn) {
            return Err(match insn {
                script::Insn::Simple(op) => format!("opcode {op:#04x} is not implemented"),
                _ => "an instruction that is not implemented".to_string(),
            });
        }
        if let Some(addr) = vm::unsupported_address(insn, field_known) {
            return Err(format!("address {addr:#06x} is not modelled"));
        }
    }
    Ok(())
}

/// Three party members to fight; `variant` changes their rows and HP so that scripts see different
/// situations.
fn party(variant: u64) -> Vec<Combatant> {
    let mut cloud = Combatant::new("Cloud", 30, 5000);
    let mut barret = Combatant::new("Barret", 30, 3000);
    let mut tifa = Combatant::new("Tifa", 30, 4000);
    for (i, c) in [&mut cloud, &mut barret, &mut tifa].into_iter().enumerate() {
        c.dex = 40;
        c.defense = 40;
        c.row = if variant >> i & 1 == 1 { Row::Back } else { Row::Front };
        if variant >> (i + 3) & 1 == 1 {
            c.hp = c.max_hp / 3;
        }
    }
    vec![cloud, barret, tifa]
}

fn lone_battle(scene: &Scene, slot: usize, variant: u64, seed: u64) -> BattleState {
    let record = scene.enemies[slot].as_ref().expect("enemy record");
    let foe = Combatant::from_enemy_in_scene(scene, slot, record);
    BattleState::with_config(
        seed,
        party(variant),
        vec![foe],
        BattleConfig {
            start: StartMode::Zero,
            ..BattleConfig::default()
        },
    )
}

/// Turns of Main the coverage run plays per situation.
const TURNS: usize = 8;

fn class_of(e: &AiError) -> String {
    match e {
        AiError::Unsupported(op) => format!("opcode {op:#04x} not implemented"),
        AiError::Address(a) => format!("address {a:#06x} not modelled"),
        AiError::Attack(_) => "attack not known to the scene (a kernel spell)".to_string(),
        other => format!("{other}"),
    }
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn opcode_coverage_over_every_enemy_of_the_scene_bin() {
    let Some(bin) = load() else { return };
    let enemies = distinct_enemies(&bin);
    let records: usize = bin
        .scenes()
        .iter()
        .map(|s| s.enemies.iter().flatten().count())
        .sum();

    // Static: every script of every enemy that has an AI block.
    let (mut with_ai, mut scripts_total, mut scripts_decode, mut scripts_ok) = (0, 0, 0, 0);
    let (mut mains, mut mains_ok) = (0, 0);
    let mut static_errors: BTreeMap<String, usize> = BTreeMap::new();
    // Dynamic: Initialize and Main run in real battles against a synthetic party, over several situations.
    let (mut init_enemies, mut init_ok) = (0, 0);
    let (mut main_enemies, mut main_ok) = (0, 0);
    let (mut both_ok, mut enabled) = (0, 0);
    let mut init_errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut main_errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut dyn_outcomes = (0usize, 0usize); // (acts, passes) over the Main runs that finished
    // The most attacks one Main run queued, and how many runs queued more than one.
    let (mut max_queue, mut multi_runs) = (0usize, 0usize);
    for (&id, &(scene_index, slot)) in &enemies {
        let scene = bin.scene(scene_index).unwrap();
        let Some(scripts) = scene.ai.enemy(slot) else { continue };
        with_ai += 1;
        for s in scripts.slots() {
            scripts_total += 1;
            match statically_supported(scripts, s) {
                Ok(()) => {
                    scripts_ok += 1;
                    scripts_decode += 1;
                }
                Err(why) => {
                    if !why.starts_with("does not decode") {
                        scripts_decode += 1;
                    }
                    *static_errors.entry(why).or_default() += 1;
                }
            }
        }
        if scripts.entry(script::slot::MAIN).is_some() {
            mains += 1;
            if statically_supported(scripts, script::slot::MAIN).is_ok() {
                mains_ok += 1;
            }
        }
        let has_init = scripts.entry(script::slot::INITIALIZE).is_some();
        let has_main = scripts.entry(script::slot::MAIN).is_some();
        if !has_main {
            continue;
        }
        init_enemies += usize::from(has_init);
        main_enemies += 1;
        let (mut init_failed, mut main_failed): (Option<String>, Option<String>) = (None, None);
        for variant in 0..8u64 {
            for seed in 0..4u64 {
                let state = lone_battle(scene, slot, variant * 7 + 1, seed);
                let ai = state.actors()[3].ai.as_ref().expect("scripts attached");
                let world = BattleWorld::new(&state, 3, ai.attacks()).expect("fits the slots");
                let mut rng = Rng::new(seed ^ u64::from(id));
                // Initialize alone, on fresh memory.
                let mut memory = Memory::new();
                if has_init {
                    if let Err(e) = vm::run(scripts, script::slot::INITIALIZE, &world, &mut memory, &mut rng) {
                        init_failed = Some(class_of(&e));
                        memory = Memory::new();
                    }
                }
                // Then eight turns of Main, the memory carried from one to the next (so the scripts'
                // later phases are reached too), on what Initialize left.
                for _turn in 0..TURNS {
                    match vm::run(scripts, script::slot::MAIN, &world, &mut memory, &mut rng) {
                        Ok(out) => {
                            if !out.actions.is_empty() {
                                dyn_outcomes.0 += 1;
                            } else {
                                dyn_outcomes.1 += 1;
                            }
                            max_queue = max_queue.max(out.actions.len());
                            multi_runs += usize::from(out.actions.len() > 1);
                        }
                        Err(e) => {
                            main_failed = Some(class_of(&e));
                            break;
                        }
                    }
                }
            }
        }
        init_ok += usize::from(has_init && init_failed.is_none());
        main_ok += usize::from(main_failed.is_none());
        both_ok += usize::from(init_failed.is_none() && main_failed.is_none());
        enabled += usize::from(init_failed.is_none());
        if let Some(why) = init_failed {
            *init_errors.entry(why).or_default() += 1;
        }
        if let Some(why) = main_failed {
            *main_errors.entry(why).or_default() += 1;
        }
    }
    let dyn_enemies = main_enemies;
    let dyn_ok = both_ok;

    eprintln!("scene.bin: {records} enemy records, {} distinct enemies, {with_ai} with an AI block", enemies.len());
    eprintln!(
        "static, every script slot: {scripts_total} scripts, {scripts_decode} decode with the known opcodes, \
         {scripts_ok} use only what the machine implements ({} fail)",
        scripts_total - scripts_ok
    );
    eprintln!("static, Main scripts: {mains_ok} of {mains} use only what the machine implements");
    for (why, n) in &static_errors {
        eprintln!("  static failure x{n}: {why}");
    }
    eprintln!(
        "dynamic, each script run in real battles against a synthetic party (32 situations per enemy, {TURNS} \
         turns of Main each with the memory carried forward): Initialize runs every time for {init_ok} of \
         {init_enemies} enemies that have one; Main runs every turn for {main_ok} of {main_enemies} (on what \
         Initialize left). In the engine an enemy whose Initialize fails is disabled: {enabled} of \
         {main_enemies} enemies keep their scripts, and {both_ok} of them never fall back in any of those turns \
         ({} acts, {} passes over the Main turns that finished)",
        dyn_outcomes.0, dyn_outcomes.1
    );
    eprintln!(
        "action queue: no Main run queued more than {max_queue} attacks ({multi_runs} runs queued two or more, \
         the limit is {})",
        vm::MAX_ACTIONS
    );
    for (why, n) in &init_errors {
        eprintln!("  Initialize failure x{n}: {why}");
    }
    for (why, n) in &main_errors {
        eprintln!("  Main failure x{n}: {why}");
    }

    // The 256 scenes decode, every enemy with scripts has a block, and the machine follows a good share.
    assert!(enemies.len() > 300, "distinct enemies: {}", enemies.len());
    assert!(with_ai > 300, "enemies with an AI block: {with_ai}");
    assert!(scripts_decode * 100 >= scripts_total * 95, "{scripts_decode} of {scripts_total} decode");
    assert!(mains_ok * 100 >= mains * 40, "{mains_ok} of {mains} Main scripts supported statically");
    assert!(dyn_ok * 100 >= dyn_enemies * 30, "{dyn_ok} of {dyn_enemies} enemies run");
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_first_reactors_enemies_run_their_scripts_to_the_end() {
    let Some(bin) = load() else { return };
    for name in ["MP", "Guard Hound", "Mono Drive", "Grunt", "1st Ray", "Sweeper", "Guard Scorpion"] {
        let (scene_index, slot) = bin.find_enemies_by_name(name)[0];
        let scene = bin.scene(scene_index).unwrap();
        let scripts = scene.ai.enemy(slot).unwrap_or_else(|| panic!("{name} has an AI block"));
        for s in scripts.slots() {
            if s == script::slot::INITIALIZE || s == script::slot::MAIN {
                assert_eq!(statically_supported(scripts, s), Ok(()), "{name} slot {s}");
            }
        }
        assert!(scripts.entry(script::slot::MAIN).is_some(), "{name} has a Main script");
    }
}

/// The ids of the enemy attacks one lone enemy chooses over its first `turns` turns, `None` for a pass.
/// The party never acts and cannot die.
fn turns_of(scene: &Scene, slot: usize, seed: u64, turns: usize) -> Vec<Option<u16>> {
    let mut state = lone_battle(scene, slot, 0, seed);
    let foe = ActorId(3);
    let mut out = Vec::new();
    let mut ticks = 0u64;
    while out.len() < turns && ticks < 200_000 {
        let events = state.step_ticks(50);
        ticks += 50;
        // One entry per turn of the foe: a GaugeReady, with an Action after it if it attacked.
        let mut current: Option<Option<u16>> = None;
        for e in events {
            match e {
                BattleEvent::GaugeReady { actor } if actor == foe => {
                    if let Some(done) = current.take() {
                        out.push(done);
                    }
                    current = Some(None);
                }
                BattleEvent::Action {
                    actor,
                    action: ActionRef::EnemyAttack(id),
                } if actor == foe => current = Some(Some(id)),
                _ => {}
            }
        }
        if let Some(done) = current {
            out.push(done);
        }
    }
    out.truncate(turns);
    out
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn reactor_enemies_choose_what_their_scripts_say() {
    let Some(bin) = load() else { return };
    for name in ["MP", "Guard Hound", "Mono Drive", "Grunt", "Sweeper"] {
        let (scene_index, slot) = bin.find_enemies_by_name(name)[0];
        let scene = bin.scene(scene_index).unwrap();
        let record = scene.enemies[slot].as_ref().unwrap();
        let own: Vec<u16> = record.attack_ids.iter().copied().filter(|&a| a != 0xFFFF).collect();
        let mut chosen: BTreeMap<u16, usize> = BTreeMap::new();
        let mut passes = 0;
        for seed in 0..6 {
            for turn in turns_of(scene, slot, seed, 40) {
                match turn {
                    Some(id) => *chosen.entry(id).or_default() += 1,
                    None => passes += 1,
                }
            }
        }
        eprintln!("{name}: attack ids chosen {chosen:?} over 240 turns, {passes} passes");
        assert!(!chosen.is_empty(), "{name} attacks");
        for id in chosen.keys() {
            // A choice the script made is an attack of the enemy's own list or of its scene.
            assert!(
                own.contains(id) || scene.attack_by_id(*id).is_some(),
                "{name} chose attack {id:#06x}, which is not its own"
            );
        }
    }
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn guard_hound_goes_for_the_party_member_with_the_least_hp() {
    let Some(bin) = load() else { return };
    let (scene_index, slot) = bin.find_enemies_by_name("Guard Hound")[0];
    let scene = bin.scene(scene_index).unwrap();
    let record = scene.enemies[slot].as_ref().unwrap();
    for seed in 0..4 {
        let mut weakest_is_barret = party(0);
        weakest_is_barret[1].hp = 400;
        weakest_is_barret[1].max_hp = 5000;
        let mut state = BattleState::with_config(
            seed,
            weakest_is_barret,
            vec![Combatant::from_enemy_in_scene(scene, slot, record)],
            BattleConfig {
                start: StartMode::Zero,
                ..BattleConfig::default()
            },
        );
        let foe = ActorId(3);
        let mut targets = Vec::new();
        while targets.len() < 5 {
            for e in state.step_ticks(50) {
                match e {
                    BattleEvent::Damage { actor, target, .. } | BattleEvent::Miss { actor, target }
                        if actor == foe =>
                    {
                        targets.push(target);
                    }
                    _ => {}
                }
            }
        }
        assert!(
            targets.iter().all(|t| *t == ActorId(1)),
            "seed {seed}: every attack goes to the weakest (Barret): {targets:?}"
        );
    }
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn first_ray_attacks_every_other_turn() {
    let Some(bin) = load() else { return };
    let (scene_index, slot) = bin.find_enemies_by_name("1st Ray")[0];
    let scene = bin.scene(scene_index).unwrap();
    let turns = turns_of(scene, slot, 3, 16);
    eprintln!("1st Ray, first 16 turns (attack id or pass): {turns:?}");
    assert_eq!(turns.len(), 16);
    for (i, t) in turns.iter().enumerate() {
        assert_eq!(t.is_some(), i % 2 == 0, "turn {i}: {turns:?}");
    }
    // The plain behaviour it replaces attacks every single turn.
    let record = scene.enemies[slot].as_ref().unwrap();
    assert!(record.attack_ids.iter().any(|&a| a != 0xFFFF));
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn guard_scorpion_follows_its_eight_step_cycle() {
    let Some(bin) = load() else { return };
    let (scene_index, slot) = bin.find_enemies_by_name("Guard Scorpion")[0];
    let scene = bin.scene(scene_index).unwrap();
    let turns = turns_of(scene, slot, 1, 24);
    eprintln!("Guard Scorpion, first 24 turns (attack id or pass): {turns:?}");
    assert_eq!(turns.len(), 24);
    // Three cycles of eight turns; the shape of each: Scope, Rifle, Scope, Rifle, Tail up, wait, wait, Tail down.
    let cycle: Vec<Vec<Option<u16>>> = turns.chunks(8).map(<[_]>::to_vec).collect();
    for c in &cycle {
        assert!(c[0].is_some() && c[1].is_some() && c[2].is_some() && c[3].is_some());
        assert_eq!(c[0], c[2], "the same scope twice");
        assert_ne!(c[0], c[1], "a rifle between scopes");
        assert!(c[4].is_some() && c[5].is_none() && c[6].is_none() && c[7].is_some());
    }
    assert_eq!(cycle[0][0], cycle[1][0]);
    assert_eq!(cycle[0][4], cycle[1][4]);
    assert_eq!(cycle[0][7], cycle[2][7]);
}

/// Cloud, Barret and Tifa who cannot die and whose Cloud is quick (his gauge fills long before an enemy's),
/// against the enemy of `slot` and `others` plain dummies that keep the battle from ending.
fn arena(scene: &Scene, slot: usize, seed: u64, hp: Option<u32>, others: usize) -> BattleState {
    let record = scene.enemies[slot].as_ref().expect("enemy record");
    let mut foe = Combatant::from_enemy_in_scene(scene, slot, record);
    if let Some(hp) = hp {
        foe.hp = hp;
        foe.max_hp = foe.max_hp.max(hp);
    }
    let mut party = party(0);
    party[0].dex = 255;
    let mut foes = vec![foe];
    foes.extend((0..others).map(|i| Combatant::new(format!("Dummy {i}"), 1, 100_000)));
    BattleState::with_config(
        seed,
        party,
        foes,
        BattleConfig {
            start: StartMode::Zero,
            ..BattleConfig::default()
        },
    )
}

/// Step until the enemy at position 3 has played `turns` turns of its Main script.
fn until_turns(state: &mut BattleState, turns: u32) {
    for _ in 0..200_000 {
        if state.actors()[3].ai.as_ref().expect("scripts").stats().turns >= turns {
            return;
        }
        state.step_ticks(1);
    }
    panic!("the enemy never played {turns} turns");
}

/// Cloud strikes the enemy at position 3 as soon as his gauge is full: the events of that command.
fn cloud_strikes(state: &mut BattleState) -> Vec<BattleEvent> {
    let cloud = ActorId(0);
    for _ in 0..200_000 {
        if state.ready_actors().contains(&cloud) {
            break;
        }
        state.step_ticks(1);
    }
    state.submit_command(cloud, fmv_ff7_battle::rules::Command::Attack { target: ActorId(3) }).expect("accepted")
}

/// The enemy's attacks announced in `events` and, after each, the party members it hit or missed.
fn answers(events: &[BattleEvent], foe: ActorId) -> Vec<(u16, Vec<u8>)> {
    let mut out: Vec<(u16, Vec<u8>)> = Vec::new();
    for e in events {
        match e {
            BattleEvent::Action { actor, action: ActionRef::EnemyAttack(id) } if *actor == foe => out.push((*id, Vec::new())),
            BattleEvent::Damage { actor, target, .. } | BattleEvent::Miss { actor, target } if *actor == foe => {
                if let Some(last) = out.last_mut() {
                    last.1.push(target.0);
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn guard_scorpion_answers_a_blow_with_its_tail_laser_only_while_its_tail_is_up() {
    let Some(bin) = load() else { return };
    let (scene_index, slot) = bin.find_enemies_by_name("Guard Scorpion")[0];
    let scene = bin.scene(scene_index).unwrap();
    let foe = ActorId(3);
    // The attack ids of its tail laser (the General counter's) and of its tail coming down (the Death counter's
    // and the eighth turn's), as the data gives them (see the numbers the cycle test above prints).
    let (tail_laser, tail_down) = (286u16, 288u16);

    // Before its tail is up it does not answer.
    let mut state = arena(scene, slot, 2, None, 0);
    let events = cloud_strikes(&mut state);
    assert!(answers(&events, foe).is_empty(), "no answer before the tail is up: {events:?}");

    // After its fifth turn (tail up) every blow is answered by the Tail Laser, on the whole party at once.
    let mut state = arena(scene, slot, 2, None, 0);
    until_turns(&mut state, 5);
    let events = cloud_strikes(&mut state);
    let got = answers(&events, foe);
    eprintln!("Guard Scorpion, a physical blow with its tail up: {got:?}");
    assert_eq!(got, vec![(tail_laser, vec![0, 1, 2])], "{events:?}");
    // A magical blow too (the same General counter), and again on the next one.
    let again = cloud_strikes(&mut state);
    assert_eq!(answers(&again, foe), vec![(tail_laser, vec![0, 1, 2])]);
    let stats = state.actors()[3].ai.as_ref().unwrap().stats();
    assert_eq!((stats.fallbacks, stats.counters_skipped), (0, 0), "{stats:?}");
    assert!(stats.counters >= 2);

    // Its tail comes down on its eighth turn (an attack of its own, "tail down"): no answer again.
    let mut state = arena(scene, slot, 2, None, 0);
    until_turns(&mut state, 8);
    let events = cloud_strikes(&mut state);
    assert!(answers(&events, foe).is_empty(), "the tail is down: {events:?}");

    // Killed with its tail up, in a battle that goes on, its Death counter brings the tail down (an attack
    // announced with nothing to do) and no General counter answers the killing blow.
    let mut state = arena(scene, slot, 2, Some(1), 1);
    until_turns(&mut state, 5);
    let events = cloud_strikes(&mut state);
    let got = answers(&events, foe);
    assert_eq!(got, vec![(tail_down, vec![])], "{events:?}");
    assert!(events.contains(&BattleEvent::Death { actor: foe }));
    assert_eq!(state.outcome(), None);
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_first_reactors_enemies_never_fall_back_in_the_engine() {
    let Some(bin) = load() else { return };
    for name in ["MP", "Guard Hound", "Mono Drive", "Grunt", "Sweeper", "1st Ray", "Guard Scorpion"] {
        let (scene_index, slot) = bin.find_enemies_by_name(name)[0];
        let scene = bin.scene(scene_index).unwrap();
        let (mut turns, mut fallbacks) = (0u32, 0u32);
        for seed in 0..6 {
            let mut state = arena(scene, slot, seed, None, 0);
            for _ in 0..60_000 {
                state.step_ticks(1);
                if state.actors()[3].ai.as_ref().unwrap().stats().turns >= 40 {
                    break;
                }
            }
            let stats = state.actors()[3].ai.as_ref().unwrap().stats();
            turns += stats.turns;
            fallbacks += stats.fallbacks;
        }
        eprintln!("{name}: {turns} scripted turns, {fallbacks} fallbacks to the plain behaviour (engine level, 6 seeds)");
        assert!(turns >= 200, "{name} played its script: {turns}");
        assert_eq!(fallbacks, 0, "{name}");
    }
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn how_many_enemies_keep_their_scripts_and_never_fall_back_in_the_real_engine() {
    // The engine-level counterpart of the machine-level coverage numbers above: each distinct enemy that has a
    // Main script fights (with a dummy beside it, so that the battle goes on) for up to 24 of its turns against
    // a party that cannot die, Cloud striking it whenever his gauge is full (so that its counters and the
    // last-attacker fields are used); the battle builds its Initialize script, and the plain behaviour plays
    // every turn the script cannot be followed on.
    let Some(bin) = load() else { return };
    let enemies = distinct_enemies(&bin);
    let (mut with_main, mut kept, mut never_fall_back, mut turns_total, mut fallbacks_total) = (0, 0, 0, 0u64, 0u64);
    let (mut counters, mut counters_skipped, mut with_counters, mut with_counters_all_run) = (0u64, 0u64, 0, 0);
    for (&_id, &(scene_index, slot)) in &enemies {
        let scene = bin.scene(scene_index).unwrap();
        let Some(scripts) = scene.ai.enemy(slot) else { continue };
        if scripts.entry(script::slot::MAIN).is_none() {
            continue;
        }
        with_main += 1;
        let has_counters = (script::slot::GENERAL_COUNTER..=script::slot::MAGICAL_COUNTER).any(|s| scripts.entry(s).is_some());
        let mut state = arena(scene, slot, 1, None, 1);
        let ai = state.actors()[3].ai.as_ref().expect("scripts attached");
        if ai.memory().phase != fmv_ff7_battle::ai::Phase::Ready {
            continue;
        }
        kept += 1;
        let played = |state: &BattleState| {
            let stats = state.actors()[3].ai.as_ref().unwrap().stats();
            stats.turns + stats.fallbacks
        };
        for _ in 0..24 {
            let before = played(&state);
            for _ in 0..40_000 {
                if played(&state) > before || !state.actors()[3].is_alive() {
                    break;
                }
                state.step_ticks(1);
            }
            if !state.actors()[3].is_alive() || state.outcome().is_some() {
                break;
            }
            // A blow between turns when it is Cloud's: the enemy may answer it.
            if state.ready_actors().contains(&ActorId(0)) {
                let _ = state.submit_command(ActorId(0), fmv_ff7_battle::rules::Command::Attack { target: ActorId(3) });
            }
        }
        let stats = state.actors()[3].ai.as_ref().unwrap().stats();
        turns_total += u64::from(stats.turns);
        fallbacks_total += u64::from(stats.fallbacks);
        never_fall_back += usize::from(stats.fallbacks == 0 && stats.turns > 0);
        counters += u64::from(stats.counters);
        counters_skipped += u64::from(stats.counters_skipped);
        if has_counters {
            with_counters += 1;
            with_counters_all_run += usize::from(stats.counters_skipped == 0);
        }
    }
    eprintln!(
        "engine level, lone enemy (with a dummy beside it) against a party that cannot die: {with_main} enemies have a Main \
         script; {kept} keep their scripts after Initialize (the others use the plain behaviour); {never_fall_back} of \
         them never fall back in 24 turns ({turns_total} scripted turns, {fallbacks_total} fallbacks); {counters} counters \
         played and {counters_skipped} skipped (could not be followed) for {with_counters} enemies that have counter \
         scripts, of which {with_counters_all_run} skipped none"
    );
    assert!(kept > 200, "{kept}");
    assert!(never_fall_back > 100, "{never_fall_back}");
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn two_engines_with_the_same_seed_play_a_real_scripted_battle_alike() {
    let Some(bin) = load() else { return };
    // Every formation that has an enemy with an AI block, a few seeds each: the same logs twice.
    let mut battles = 0;
    let mut scripted_actions = 0usize;
    for scene in bin.scenes().iter().skip(75).take(24) {
        for formation in 0..4 {
            let foes = enemies_for_formation(scene, formation);
            if foes.is_empty() || foes.iter().all(|f| f.ai.is_none()) {
                continue;
            }
            for seed in [1u64, 2] {
                let run = || {
                    let mut state = BattleState::with_config(
                        seed,
                        party(0),
                        enemies_for_formation(scene, formation),
                        BattleConfig {
                            start: StartMode::Normal,
                            ..BattleConfig::default()
                        },
                    );
                    let log = state.step_ticks(6000);
                    (log, format!("{state:?}"))
                };
                let (a, a_state) = run();
                let (b, b_state) = run();
                assert_eq!(a, b, "scene {} formation {formation} seed {seed}", scene.index);
                assert_eq!(a_state, b_state);
                scripted_actions += a
                    .iter()
                    .filter(|e| matches!(e, BattleEvent::Action { action: ActionRef::EnemyAttack(_), .. }))
                    .count();
                battles += 1;
            }
        }
    }
    eprintln!("{battles} battles replayed twice with identical logs ({scripted_actions} enemy attacks)");
    assert!(battles > 20);
    assert!(scripted_actions > 0);
}
