//! Validation against the owner's real FF7 files. Every test is `#[ignore]`d and reads the
//! data directory from the `FMV_FF7_DATA` environment variable (the game's `data/` folder).
//! If the variable is unset the tests print a note and pass without doing anything.
//!
//! ```text
//! FMV_FF7_DATA=/path/to/ff7/data cargo test -p fmv-ff7-battle --test real_data -- --ignored --nocapture
//! ```
//!
//! The files are only ever read. No game bytes are copied into the repository; the
//! assertions are structural invariants and cross-checks, plus a handful of well-known
//! early enemy names.

use std::path::PathBuf;

use fmv_ff7_battle::kernel::{decode_kernel2, CharacterId, Kernel, COMMAND_ATTACK};
use fmv_ff7_battle::rules::{
    enemies_for_formation, BattleConfig, BattleEvent, BattleState, Combatant, Command, Outcome,
    Side, StartMode,
};
use fmv_ff7_battle::scene::{SceneBin, FORMATIONS_PER_SCENE, MAX_SCENES, SCENE_SIZE};
use fmv_ff7_battle::text;

/// Locate a file under `FMV_FF7_DATA`, trying the Steam (`lang-en/`) layout first and then
/// the 1998 layout. Returns `None` (after a note) when the variable is unset.
fn data_file(candidates: &[&str]) -> Option<PathBuf> {
    let Some(root) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping real-data test");
        return None;
    };
    let root = PathBuf::from(root);
    for rel in candidates {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    panic!(
        "FMV_FF7_DATA={} does not contain any of {:?}",
        root.display(),
        candidates
    );
}

fn scene_path() -> Option<PathBuf> {
    data_file(&[
        "lang-en/battle/scene.bin",
        "battle/scene.bin",
        "battle/SCENE.BIN",
    ])
}

fn kernel_path() -> Option<PathBuf> {
    data_file(&[
        "lang-en/kernel/kernel.bin",
        "kernel/kernel.bin",
        "kernel/KERNEL.BIN",
    ])
}

fn kernel2_path() -> Option<PathBuf> {
    data_file(&[
        "lang-en/kernel/kernel2.bin",
        "kernel/kernel2.bin",
        "kernel/KERNEL2.BIN",
    ])
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_scene_bin_decodes() {
    let Some(path) = scene_path() else { return };
    let bin = SceneBin::open(&path).expect("scene.bin parses");

    // Structure: 256 scenes of exactly 7808 bytes (enforced by the parser), 1024 formations.
    assert_eq!(bin.scenes().len(), MAX_SCENES);
    assert_eq!(bin.formation_count(), MAX_SCENES * FORMATIONS_PER_SCENE);
    assert!(bin
        .block_scene_counts()
        .iter()
        .all(|c| (1..=16).contains(c)));
    assert_eq!(
        bin.block_scene_counts().iter().sum::<usize>(),
        bin.scenes().len()
    );
    assert_eq!(SCENE_SIZE, 7808);

    // Formation entries all resolve to one of their scene's three enemy records.
    let mut used_entries = 0usize;
    let mut resolved_entries = 0usize;
    let mut formations_with_enemies = 0usize;
    for g in 0..bin.formation_count() {
        let f = bin.formation(g).expect("formation index in range");
        let entries = &f.formation().entries;
        used_entries += entries.len();
        resolved_entries += f.enemies().len();
        if !entries.is_empty() {
            formations_with_enemies += 1;
        }
    }
    assert!(used_entries > 0);
    assert_eq!(used_entries, resolved_entries, "every entry resolves");
    assert!(formations_with_enemies > 400);

    // Enemy names: plenty decode to plain ASCII, and known early enemies are present.
    let mut named = 0usize;
    let mut plain = 0usize;
    for scene in bin.scenes() {
        for enemy in scene.enemies.iter().flatten() {
            if !enemy.name.is_empty() {
                named += 1;
                if text::is_plain_ascii(&enemy.name) {
                    plain += 1;
                }
            }
        }
    }
    assert!(named > 100);
    // A few unused test entries use a non-English encoding; everything else is ASCII.
    assert!(
        plain * 10 >= named * 9,
        "{plain} of {named} names are plain ASCII"
    );
    for name in [
        "MP",
        "Guard Hound",
        "Sweeper",
        "Grunt",
        "Mighty Grunt",
        "Smogger",
        "Hedgehog Pie",
        "Mono Drive",
        "Special Combatant",
        "Guard Scorpion",
        "Air Buster",
        "Bottomswell",
    ] {
        let hits = bin.find_enemies_by_name(name);
        assert!(!hits.is_empty(), "enemy {name:?} not found");
        // Records of real enemies are sane: level 1..=99, nonzero HP.
        for (scene_index, slot) in hits {
            let rec = bin.scene(scene_index).unwrap().enemies[slot]
                .as_ref()
                .unwrap();
            assert!((1..=99).contains(&rec.level), "{name}: level {}", rec.level);
            assert!(rec.hp > 0, "{name}: hp");
        }
    }

    // Enemies referenced by formations have attacks that resolve in their scene.
    let (scene_index, slot) = bin.find_enemies_by_name("Guard Hound")[0];
    let scene = bin.scene(scene_index).unwrap();
    assert!(!scene.enemy_attacks(slot).is_empty());
    assert!(scene.basic_attack(slot).is_some());

    println!(
        "scene.bin: {} scenes, {} formations ({} with enemies, {} placements), \
         {} of {} enemy names plain ASCII, blocks {:?}",
        bin.scenes().len(),
        bin.formation_count(),
        formations_with_enemies,
        used_entries,
        plain,
        named,
        bin.block_scene_counts().len()
    );
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_kernel_bin_decodes() {
    let Some(path) = kernel_path() else { return };
    let kernel = Kernel::open(&path).expect("kernel.bin parses");

    assert_eq!(kernel.sections().len(), 27);
    assert_eq!(kernel.section(0).unwrap().len(), 256);
    assert_eq!(kernel.commands.len(), 32);
    // Sections 0..=8 are data (types 0..=8), the rest text (type 9).
    for (i, s) in kernel.sections().iter().enumerate() {
        let want = if i < 9 { i as u16 } else { 9 };
        assert_eq!(s.file_type, want, "section {i} file type");
    }
    assert!(kernel.trailing_bytes() < 6);

    // Command 1 is Attack: it asks for a target and takes targeting from the weapon.
    assert_eq!(
        kernel.command_name(COMMAND_ATTACK).as_deref(),
        Some("Attack")
    );
    let attack = kernel.command(COMMAND_ATTACK).unwrap();
    assert_eq!(attack.cursor_action, 7);
    assert_eq!(attack.target_flags, 0xFF);

    // Growth data: nine characters, 64 curves, and a sane stat block progression.
    assert_eq!(kernel.growth.characters.len(), 9);
    assert_eq!(kernel.growth.curves.len(), 64);
    for who in CharacterId::ALL {
        let mut prev_hp = 0;
        for level in [1u8, 5, 10, 20, 40, 60, 80, 99] {
            let b = kernel.growth.stat_block(who, level).expect("stat block");
            assert!(
                b.max_hp >= prev_hp,
                "{who:?} HP not monotone at level {level}"
            );
            assert!(
                b.max_hp >= 100 && b.max_hp <= 9999,
                "{who:?} lv{level} hp {}",
                b.max_hp
            );
            assert!(b.strength >= 1 && b.vitality >= 1 && b.dexterity >= 1);
            prev_hp = b.max_hp;
        }
    }
    // Level 99 is far stronger than level 1 for the main fighters.
    let lo = kernel.growth.stat_block(CharacterId::Cloud, 1).unwrap();
    let hi = kernel.growth.stat_block(CharacterId::Cloud, 99).unwrap();
    assert!(hi.max_hp > lo.max_hp * 5 && hi.strength > lo.strength * 3);

    // The closed-form stat derivation agrees with the shipped starting records where those
    // are formula-derived (this backs the claims in the `kernel` module docs): the level-1
    // HP and MP of the populated joiner records, and Cloud's primary stats and HP.
    let mut checked_joiners = 0;
    for rec in &kernel.init_characters {
        let Some(who) = CharacterId::from_index(usize::from(rec.id)) else {
            continue;
        };
        if rec.level == 1 && rec.stats.iter().any(|s| *s != 0) {
            let b = kernel.growth.stat_block(who, 1).unwrap();
            assert_eq!(
                (b.max_hp, b.max_mp),
                (rec.base_hp, rec.base_mp),
                "{who:?} level-1 HP/MP"
            );
            checked_joiners += 1;
        }
    }
    assert!(
        checked_joiners >= 3,
        "only {checked_joiners} joiner records checked"
    );
    let cloud_rec = kernel.init_character_by_id(0).expect("Cloud init record");
    let cloud_b = kernel
        .growth
        .stat_block(CharacterId::Cloud, cloud_rec.level)
        .unwrap();
    assert_eq!(
        [
            cloud_b.strength,
            cloud_b.vitality,
            cloud_b.magic,
            cloud_b.spirit
        ]
        .map(|v| v as u8),
        cloud_rec.stats[..4],
        "Cloud primary stats at his starting level"
    );
    assert_eq!(
        cloud_b.max_hp, cloud_rec.base_hp,
        "Cloud HP at his starting level"
    );

    // Cloud's shipped initialization record is a plausible starting character.
    let cloud = kernel.init_character_by_id(0).expect("Cloud init record");
    assert!(cloud.level >= 1);
    let weapon = kernel.weapon(cloud.weapon).expect("starting weapon");
    assert!(weapon.attack > 0 && weapon.accuracy > 0);
    assert!(kernel
        .weapon_name(cloud.weapon)
        .is_some_and(|n| !n.is_empty()));
    assert!(kernel.armor(cloud.armor).is_some());

    // Cross-check with scene.bin: the kernel's scene lookup table lists the first scene of
    // each block, which must equal the cumulative per-block scene counts we decoded.
    if let Some(sp) = scene_path() {
        let bin = SceneBin::open(&sp).unwrap();
        let mut first = 0usize;
        let mut expected = Vec::new();
        for count in bin.block_scene_counts() {
            expected.push(first as u8);
            first += count;
        }
        assert_eq!(kernel.growth.scene_block_starts(), expected);
    }

    println!(
        "kernel.bin: {} sections, {} trailing byte(s), Attack command cursor action {}",
        kernel.sections().len(),
        kernel.trailing_bytes(),
        attack.cursor_action
    );
}

/// Run a battle where ready party members attack the first living enemy, stepping 100 ms at
/// a time. Returns the full event log.
fn play_out(state: &mut BattleState, max_slices: usize) -> Vec<BattleEvent> {
    let mut log = Vec::new();
    for _ in 0..max_slices {
        if state.outcome().is_some() {
            break;
        }
        log.extend(state.step(100));
        for id in state.ready_actors() {
            let Some(target) = state
                .enemy_ids()
                .into_iter()
                .find(|e| state.actor(*e).is_some_and(|c| c.is_alive()))
            else {
                break;
            };
            if let Ok(events) = state.submit_command(id, Command::Attack { target }) {
                log.extend(events);
            }
        }
    }
    log
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_shared_battle_is_deterministic_and_winnable() {
    let (Some(sp), Some(kp)) = (scene_path(), kernel_path()) else {
        return;
    };
    let bin = SceneBin::open(sp).expect("scene.bin");
    let kernel = Kernel::open(kp).expect("kernel.bin");

    // Every character can be built at a few levels.
    for who in CharacterId::ALL {
        for level in [1u8, 20, 99] {
            let c = Combatant::from_character(&kernel, who, level, None, None).expect("combatant");
            assert!(c.max_hp > 0 && c.attack > 0 && c.defense > 0 && c.dex > 0);
            assert_eq!(c.side, Side::Party);
        }
    }
    // Cloud's starting weapon adds to his strength.
    let cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
    let stats = kernel.growth.stat_block(CharacterId::Cloud, 6).unwrap();
    assert!(cloud.attack > u32::from(stats.strength));

    // The very first Midgar fights: a formation with exactly one MP and one Guard Hound.
    let global = (0..bin.formation_count())
        .find(|&g| {
            let names: Vec<String> = bin
                .formation(g)
                .unwrap()
                .enemies()
                .iter()
                .map(|e| e.record.name.clone())
                .collect();
            names.len() == 2
                && names.iter().any(|n| n == "MP")
                && names.iter().any(|n| n == "Guard Hound")
        })
        .expect("an MP + Guard Hound formation exists");
    let formation = bin.formation(global).unwrap();
    let layout_start = StartMode::from_layout(formation.setup().layout);

    // Two players: Cloud and Barret at Cloud's starting level.
    let level = kernel.init_character_by_id(0).unwrap().level;
    let build = |seed: u64| {
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, level, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Barret, level, None, None).unwrap(),
        ];
        let enemies = enemies_for_formation(formation.scene, formation.slot);
        assert_eq!(enemies.len(), 2);
        BattleState::with_config(
            seed,
            party,
            enemies,
            BattleConfig {
                start: layout_start,
                ..BattleConfig::default()
            },
        )
    };

    // Determinism: two independent runs with one seed agree event for event.
    let mut a = build(0xC0FFEE);
    let mut b = build(0xC0FFEE);
    let log_a = play_out(&mut a, 6000);
    let log_b = play_out(&mut b, 6000);
    assert_eq!(log_a, log_b);
    assert!(log_a
        .iter()
        .any(|e| matches!(e, BattleEvent::Damage { .. })));
    assert!(a.outcome().is_some(), "the battle ends");

    // Balance sanity: an even-level two-player party beats these first enemies in most runs.
    let mut wins = 0;
    let runs = 60;
    for seed in 0..runs {
        let mut s = build(seed);
        play_out(&mut s, 6000);
        if s.outcome() == Some(Outcome::Victory) {
            wins += 1;
        }
    }
    assert!(wins * 10 >= runs * 8, "party won only {wins} of {runs}");
    println!(
        "shared battle: formation {global} (layout {:?}), seed 0xC0FFEE -> {:?} after {} events / {} ticks; \
         {wins}/{runs} seeds won by the party",
        formation.setup().layout,
        a.outcome().unwrap(),
        log_a.len(),
        a.tick(),
    );
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_kernel2_decodes() {
    let Some(path) = kernel2_path() else { return };
    let bytes = std::fs::read(&path).unwrap();
    let blob = decode_kernel2(&bytes).expect("kernel2.bin decodes");
    // The text sections must stay under 27 KiB on PC and contain recognisable strings.
    assert!(
        !blob.is_empty() && blob.len() < 27 * 1024,
        "decoded {} bytes",
        blob.len()
    );
    let contains = |needle: &str| {
        let enc = text::encode(needle).unwrap();
        blob.windows(enc.len()).any(|w| w == enc.as_slice())
    };
    assert!(contains("Potion"), "item name present");
    assert!(contains("Attack"), "command name present");

    // Cross-check the LZSS decoder against kernel.bin: the blob is the concatenation of the
    // text sections, so at least one of kernel.bin's own text sections appears verbatim
    // (the Steam edition changed some of them, so not necessarily all).
    let mut identical = 0usize;
    if let Some(kp) = kernel_path() {
        let kernel = Kernel::open(kp).unwrap();
        for i in 9..kernel.sections().len() {
            let section = kernel.section(i).unwrap();
            if section.len() <= blob.len()
                && (0..=blob.len() - section.len()).any(|at| blob[at..].starts_with(section))
            {
                identical += 1;
            }
        }
        assert!(
            identical >= 1,
            "no kernel.bin text section found verbatim in kernel2.bin"
        );
    }
    println!(
        "kernel2.bin: decoded text blob of {} bytes; {identical} kernel.bin text section(s) appear verbatim",
        blob.len()
    );
}
