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
    Side, StartMode, Target,
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

/// The EXP curves of section 2 reproduce the thresholds the game is known for (this backs
/// `GrowthData::exp_for_level`'s documentation): Cloud needs 6, 33, 94, 202, 372 and 616 EXP in
/// total for levels 2 to 7, and Barret's first level costs 7 (his band 0 modifier is 70). The
/// curves are strictly increasing for every character, level 99 is far below the EXP cap, and
/// Cloud's own starting record (level 6, 610 EXP) is a level-6 value on his curve.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_exp_curves_reproduce_the_known_thresholds() {
    use fmv_ff7_battle::kernel::{MAX_EXP, MAX_LEVEL};
    let Some(path) = kernel_path() else { return };
    let kernel = Kernel::open(&path).expect("kernel.bin parses");
    let g = &kernel.growth;

    let cloud: Vec<u32> = (1..=7)
        .map(|l| g.exp_for_level(CharacterId::Cloud, l).unwrap())
        .collect();
    assert_eq!(
        cloud,
        [0, 6, 33, 94, 202, 372, 616],
        "Cloud's thresholds for levels 1 to 7"
    );
    assert_eq!(
        g.exp_for_level(CharacterId::Barret, 2),
        Some(7),
        "Barret's first level"
    );

    for who in CharacterId::ALL {
        // The base byte of an EXP curve is unused (zero): the modifier is the gradient byte.
        let curve = g
            .curve(g.characters[who.index()].exp_curve)
            .expect("EXP curve");
        assert!(
            curve.bases.iter().all(|b| *b == 0),
            "{who:?}: EXP curve bases {:?}",
            curve.bases
        );
        assert!(
            curve.gradients.iter().all(|m| (60..=90).contains(m)),
            "{who:?}: {:?}",
            curve.gradients
        );
        let mut prev = 0;
        for level in 1..=MAX_LEVEL {
            let t = g.exp_for_level(who, level).unwrap();
            assert!(level == 1 || t > prev, "{who:?} level {level}");
            assert_eq!(
                g.level_for_exp(who, t),
                Some(level),
                "{who:?} level {level}"
            );
            prev = t;
        }
        assert!(prev < MAX_EXP, "{who:?}: level 99 needs {prev}");
        assert_eq!(g.exp_to_next_level(who, MAX_LEVEL), Some(0));
    }

    // Cloud's initial record: level 6 with 610 EXP is a level-6 value (372 <= 610 < 616) on
    // his curve, six short of level 7.
    let rec = kernel.init_character_by_id(0).expect("Cloud's init record");
    assert_eq!(
        g.level_for_exp(CharacterId::Cloud, rec.exp),
        Some(rec.level),
        "level {} with {} EXP",
        rec.level,
        rec.exp
    );
    println!(
        "Cloud: {cloud:?}; his start: level {} with {} EXP",
        rec.level, rec.exp
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

/// Spells, items, materia and the starting stock of the real `kernel.bin` (this backs the
/// "checked against the owner's Steam copy" notes of `kernel` and `action`): the attack index of a
/// spell is the index of its name in "Names - magic", item 0 is Potion, the starting records equip
/// Lightning and Ice (Cloud) and Fire (Red XIII), and the starting stock is Potion x3, Ether x1 and
/// Phoenix Down x1.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_kernel_has_spells_items_materia_and_a_starting_stock() {
    use fmv_ff7_battle::action::{element, ActionKind, Aim, Formula, ItemKind, ItemTable, Spell};
    use fmv_ff7_battle::kernel::ItemStack;
    use fmv_ff7_battle::rules::{party_spells, restore_amount, STARTER_ALL_MATERIA, STARTER_MATERIA};

    let Some(path) = kernel_path() else { return };
    let kernel = Kernel::open(&path).expect("kernel.bin parses");

    // The names say which index is which spell, item and materia.
    for (index, name) in [
        (0u16, "Cure"),
        (1, "Cure2"),
        (2, "Cure3"),
        (7, "Life"),
        (27, "Fire"),
        (30, "Ice"),
        (33, "Bolt"),
        (36, "Quake"),
        (39, "Bio"),
        (42, "Demi"),
        (51, "FullCure"),
    ] {
        assert_eq!(
            kernel.magic_name(index).as_deref(),
            Some(name),
            "spell {index}"
        );
    }
    for (index, name) in [
        (0u16, "Potion"),
        (1, "Hi-Potion"),
        (2, "X-Potion"),
        (3, "Ether"),
        (5, "Elixir"),
        (7, "Phoenix Down"),
    ] {
        assert_eq!(
            kernel.item_name(index).as_deref(),
            Some(name),
            "item {index}"
        );
    }
    for (id, name) in [
        (49u8, "Fire"),
        (50, "Ice"),
        (52, "Lightning"),
        (53, "Restore"),
    ] {
        assert_eq!(
            kernel.materia_name(id).as_deref(),
            Some(name),
            "materia {id}"
        );
        assert!(
            kernel.materia(id).unwrap().is_magic(),
            "{name} is a magic materia"
        );
    }
    assert_eq!(
        STARTER_MATERIA,
        [53, 49, 50, 52],
        "Restore, Fire, Ice, Lightning"
    );

    // Restore teaches Cure at level 1; Fire, Ice and Lightning their first spell.
    let first = |id: u8| kernel.materia(id).unwrap().unlocked_spells(0);
    assert_eq!(
        [first(53), first(49), first(50), first(52)],
        [vec![0], vec![27], vec![30], vec![33]]
    );

    // Spells as the rules see them: Cure restores (Cure's own formula, MP 5), Fire damages with magic.
    let cure = Spell::from_attack(0, &kernel.attack(0).unwrap()).expect("Cure is castable");
    assert!(
        cure.spec.heals()
            && cure.spec.kind == ActionKind::Magical
            && cure.spec.formula == Formula::Restore
    );
    assert_eq!(cure.spec.mp_cost, 5);
    let fire = Spell::from_attack(27, &kernel.attack(27).unwrap()).expect("Fire is castable");
    assert!(!fire.spec.heals() && fire.spec.element_mask == element::FIRE);
    let life = Spell::from_attack(7, &kernel.attack(7).unwrap()).expect("Life is castable");
    assert!(life.spec.heals() && life.spec.hits_dead);

    // Whole sides (protocol v10). Cure, Life and Fire can be aimed at one target or at everyone (their records
    // toggle between the two: "All Tar/1 Tar", which takes an All materia), and Ultima only ever hits every
    // enemy ("All Tar (NS)"): castable now, at full strength, with no All materia.
    assert_eq!([cure.spec.aim, life.spec.aim, fire.spec.aim], [Aim::OneOrSide; 3]);
    assert!(cure.spec.splits() && fire.spec.splits(), "Cure and Fire split on a whole side");
    assert!(!life.spec.splits(), "a fraction of max HP never splits");
    assert_eq!(kernel.magic_name(52).as_deref(), Some("Ultima"));
    let ultima = Spell::from_attack(52, &kernel.attack(52).unwrap()).expect("Ultima is castable");
    assert_eq!(ultima.spec.aim, Aim::Side);
    assert!(!ultima.spec.splits() && !ultima.spec.heals());
    // The All materia: its name, and one use per level up to five, which is where a character's count comes from.
    assert_eq!(kernel.materia_name(STARTER_ALL_MATERIA).as_deref(), Some("All"));
    let all = kernel.materia(STARTER_ALL_MATERIA).unwrap();
    assert_eq!(all.level_for_ap(0), 1);
    assert_eq!((1..=5).map(|level| all.all_uses(level)).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    assert_eq!((all.all_uses(0), all.all_uses(6)), (0, 0));
    for who in CharacterId::ALL {
        let c = Combatant::from_character(&kernel, who, 10, None, None).unwrap();
        assert_eq!(c.all_left, 1, "{who:?}: one All materia at level 1");
    }

    // Items: a Potion restores 100 HP, a Hi-Potion 500, an Ether 100 MP, a Phoenix Down revives.
    let table = ItemTable::from_kernel(&kernel);
    let info = |i: u16| {
        table
            .info(i)
            .unwrap_or_else(|| panic!("item {i} is usable in battle"))
    };
    assert_eq!(info(0).kind, ItemKind::RestoreHp);
    assert_eq!(info(2).kind, ItemKind::RestoreHp);
    assert_eq!(info(3).kind, ItemKind::RestoreMp);
    assert_eq!(info(5).kind, ItemKind::RestoreAll);
    assert_eq!((info(7).kind, info(7).revives), (ItemKind::Revive, true));
    let healthy = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
    let amount = |i: u16| restore_amount(&table.get(i).unwrap().spec, &healthy, &healthy, 0);
    assert_eq!((amount(0), amount(1), amount(3)), (100, 500, 100));
    assert_eq!(amount(7), healthy.max_hp * 8 / 32);
    // Megalixir restores the whole party and only that (it is Elixir for every ally); a potion, an Elixir and a
    // Phoenix Down hit one; the status items are the next test's (the cures of the statuses the rules play are in).
    assert_eq!(kernel.item_name(6).as_deref(), Some("Megalixir"));
    assert_eq!((info(6).kind, info(6).aim, info(6).targets_allies), (ItemKind::RestoreAll, Aim::Side, true));
    assert!([0, 1, 2, 3, 4, 5, 7].iter().all(|i| info(*i).aim == Aim::One));
    assert_eq!((info(8).kind, kernel.item_name(8).as_deref()), (ItemKind::Status, Some("Antidote")));
    assert!(
        table.len() >= 8 && table.len() < 80,
        "{} usable items",
        table.len()
    );

    // The starting stock and the materia of the starting records.
    assert_eq!(
        kernel.starting_inventory(),
        vec![
            ItemStack { item: 0, count: 3 },
            ItemStack { item: 3, count: 1 },
            ItemStack { item: 7, count: 1 }
        ]
    );
    assert_eq!(
        kernel.equipped_spells(CharacterId::Cloud),
        vec![30, 33],
        "Cloud starts with Ice and Lightning"
    );
    assert_eq!(
        kernel.equipped_spells(CharacterId::RedXiii),
        vec![27],
        "Red XIII starts with Fire"
    );
    for who in [
        CharacterId::Barret,
        CharacterId::Tifa,
        CharacterId::Aeris,
        CharacterId::Yuffie,
        CharacterId::CaitSith,
        CharacterId::Vincent,
        CharacterId::Cid,
    ] {
        assert!(kernel.equipped_spells(who).is_empty(), "{who:?}");
    }
    // So every character knows the starter grimoire and Cloud and Red XIII no more than it.
    for who in CharacterId::ALL {
        let spells: Vec<u16> = party_spells(&kernel, who).iter().map(|s| s.index).collect();
        assert_eq!(spells, [0, 27, 30, 33], "{who:?}");
        let c = Combatant::from_character(&kernel, who, 10, None, None).unwrap();
        assert!(
            c.magic > 0 && c.magic_defense > 0,
            "{who:?}: magic {} / magic defense {}",
            c.magic,
            c.magic_defense
        );
        assert!(c.max_mp >= 5, "{who:?} can cast Cure: {} MP", c.max_mp);
    }
    println!(
        "kernel: {} items usable in battle, starting stock {:?}",
        table.len(),
        kernel
            .starting_inventory()
            .iter()
            .map(|s| (s.item, s.count))
            .collect::<Vec<_>>()
    );
}

/// The enemies of the real `scene.bin` with their own attacks and affinities.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_enemies_have_attacks_and_affinities_the_rules_can_use() {
    use fmv_ff7_battle::action::ActionKind;
    let Some(sp) = scene_path() else { return };
    let bin = SceneBin::open(sp).expect("scene.bin");

    let (mut enemies, mut with_actions, mut with_magic, mut with_cost, mut with_affinity) =
        (0, 0, 0, 0, 0);
    for g in 0..bin.formation_count() {
        let formation = bin.formation(g).unwrap();
        for c in enemies_for_formation(formation.scene, formation.slot) {
            enemies += 1;
            with_actions += usize::from(!c.actions.is_empty());
            with_magic += usize::from(c.actions.iter().any(|a| a.spec.kind == ActionKind::Magical));
            with_cost += usize::from(c.actions.iter().any(|a| a.spec.mp_cost > 0));
            with_affinity += usize::from(!c.affinities.is_empty());
            // Every action is a damaging one the rules can play; every affinity is an element.
            assert!(
                c.actions
                    .iter()
                    .all(|a| a.spec.power > 0 && !a.spec.heals()),
                "{}",
                c.name
            );
            assert!(c.affinities.iter().all(|a| a.element < 16), "{}", c.name);
            assert!(c.actions.len() <= 16);
        }
    }
    assert!(enemies > 500, "{enemies} enemy placements");
    // Most enemies have a damaging attack of their own, a good many have a spell, some have affinities.
    assert!(
        with_actions * 10 >= enemies * 8,
        "{with_actions} of {enemies} enemies have attacks"
    );
    assert!(
        with_magic > 20 && with_affinity > 100,
        "{with_magic} with magic, {with_affinity} with affinities"
    );

    // The first Midgar fights (the smoke formations 301 to 303): an MP with a gun and a tonfa, a Guard
    // Hound with a bite and a tentacle; physical, no MP, no affinities.
    for g in [301usize, 302, 303] {
        let formation = bin.formation(g).unwrap();
        for c in enemies_for_formation(formation.scene, formation.slot) {
            assert!(
                !c.actions.is_empty()
                    && c.actions
                        .iter()
                        .all(|a| a.spec.kind == ActionKind::Physical && a.spec.mp_cost == 0),
                "{}",
                c.name
            );
            assert!(c.affinities.is_empty(), "{}", c.name);
            match c.name.as_str() {
                "MP" => assert_eq!(
                    c.actions.iter().map(|a| a.id).collect::<Vec<_>>(),
                    [272, 273]
                ),
                "Guard Hound" => assert_eq!(
                    c.actions.iter().map(|a| a.id).collect::<Vec<_>>(),
                    [274, 275]
                ),
                other => panic!("unexpected enemy {other} in formation {g}"),
            }
        }
    }
    println!(
        "scene.bin: {with_actions} of {enemies} enemy placements have attacks ({with_magic} with magic, {with_cost} with an MP cost), {with_affinity} have affinities"
    );
}

/// Real party (with real spells and the real item table) against the real smoke formations, played with
/// every kind of command: deterministic, always finished, and the numbers stay sane.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_battles_with_every_kind_of_command_are_deterministic_and_sane() {
    use fmv_ff7_battle::action::ItemTable;
    use fmv_ff7_battle::rules::{ActionRef, ActorId};
    use std::sync::Arc;

    let (Some(sp), Some(kp)) = (scene_path(), kernel_path()) else {
        return;
    };
    let bin = SceneBin::open(sp).expect("scene.bin");
    let kernel = Kernel::open(kp).expect("kernel.bin");
    let items = Arc::new(ItemTable::from_kernel(&kernel));
    let level = kernel.init_character_by_id(0).unwrap().level;

    let build = |seed: u64, formation: usize| {
        let f = bin.formation(formation).unwrap();
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, level, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Aeris, level, None, None).unwrap(),
        ];
        BattleState::with_config(
            seed,
            party,
            enemies_for_formation(f.scene, f.slot),
            BattleConfig {
                start: StartMode::from_layout(f.setup().layout),
                ..BattleConfig::default()
            },
        )
        .with_items(Arc::clone(&items))
    };
    // A policy that uses everything: attack, Fire (Cloud and Aerith both know it), Cure on the weakest
    // ally, a Potion, Defend, Phoenix Down on the dead, and (each has an All use) Fire on every enemy and
    // Cure on the whole party.
    let policy = |state: &BattleState, actor: ActorId, round: u64| -> Command {
        let enemy = state
            .enemy_ids()
            .into_iter()
            .find(|e| state.actor(*e).is_some_and(|c| c.is_alive()))
            .unwrap();
        let weakest = state
            .party_ids()
            .into_iter()
            .filter(|a| state.actor(*a).is_some_and(|c| c.is_alive()))
            .min_by_key(|a| state.actor(*a).unwrap().hp)
            .unwrap();
        let dead = state
            .party_ids()
            .into_iter()
            .find(|a| state.actor(*a).is_some_and(|c| !c.is_alive()));
        let options = [
            Command::Attack { target: enemy },
            Command::Magic {
                spell: 27,
                target: Target::One(enemy),
            },
            Command::Magic {
                spell: 0,
                target: Target::One(weakest),
            },
            Command::Item {
                item: 0,
                target: Target::One(weakest),
            },
            Command::Defend,
            dead.map_or(Command::Attack { target: enemy }, |target| Command::Item {
                item: 7,
                target: Target::One(target),
            }),
            Command::Magic { spell: 27, target: Target::AllEnemies },
            Command::Magic { spell: 0, target: Target::AllAllies },
        ];
        let pick = options[(round + u64::from(actor.0)) as usize % options.len()];
        if state.check_command(actor, &pick).is_ok() {
            pick
        } else {
            Command::Attack { target: enemy }
        }
    };
    let play = |seed: u64, formation: usize| {
        let mut state = build(seed, formation);
        let mut log = Vec::new();
        let mut round = 0u64;
        for _ in 0..20_000 {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(100));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                round += 1;
                let command = policy(&state, id, round);
                log.extend(
                    state
                        .submit_command(id, command)
                        .expect("the policy only picks valid commands"),
                );
            }
            for c in state.actors() {
                assert!(c.hp <= c.max_hp && c.mp <= c.max_mp);
            }
        }
        (state, log)
    };
    let (mut wins, mut runs, mut casts, mut items_used, mut enemy_attacks) = (0, 0, 0, 0, 0);
    for formation in [301usize, 302, 303] {
        for seed in 0..40u64 {
            let (state, log) = play(seed, formation);
            let (again, log_again) = play(seed, formation);
            assert_eq!(log, log_again, "formation {formation} seed {seed}");
            assert_eq!(state, again);
            assert!(
                state.outcome().is_some(),
                "formation {formation} seed {seed} did not end"
            );
            runs += 1;
            wins += usize::from(state.outcome() == Some(Outcome::Victory));
            for e in &log {
                if let BattleEvent::Action { action, .. } = e {
                    match action {
                        ActionRef::Magic(_) => casts += 1,
                        ActionRef::Item(_) => items_used += 1,
                        ActionRef::EnemyAttack(_) => enemy_attacks += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    assert!(wins * 10 >= runs * 8, "the party won only {wins} of {runs}");
    assert!(
        casts > 0 && items_used > 0 && enemy_attacks > 0,
        "casts {casts}, items {items_used}, enemy attacks {enemy_attacks}"
    );
    println!("real battles with every command: {wins}/{runs} won; {casts} spells, {items_used} items, {enemy_attacks} enemy attacks");
}

/// Status effects over the owner's data (checkpoint 2 of "deeper battles"): which spells and items inflict or cure what, the
/// immunities of the enemies, how many of the scenes' attacks carry a status, and real fights that run with them. Ids,
/// names the game's own menus show, booleans and counts: nothing of the files is kept.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_statuses_decode_from_the_kernel_and_the_scenes_and_play_in_real_battles() {
    use fmv_ff7_battle::action::{ActionSpec, Aim, Formula, ItemKind, ItemTable, StatusEffect, StatusMode};
    use fmv_ff7_battle::status::{Status, StatusSet, MODELLED};
    use std::sync::Arc;

    let (Some(sp), Some(kp)) = (scene_path(), kernel_path()) else {
        return;
    };
    let bin = SceneBin::open(sp).expect("scene.bin");
    let kernel = Kernel::open(kp).expect("kernel.bin");
    let table = ItemTable::from_kernel(&kernel);
    let set = |list: &[Status]| -> StatusSet { list.iter().copied().collect() };

    // Items: what each does, to whom. (The names are the game's own, to pin the ids.)
    let item = |i: u16, name: &str| {
        assert_eq!(kernel.item_name(i).as_deref(), Some(name), "item {i}");
        table.get(i).unwrap_or_else(|| panic!("{name} can be used in battle")).spec
    };
    let cures = |list: &[Status]| StatusEffect { set: set(list), chance: 63, mode: StatusMode::Cure };
    let antidote = item(8, "Antidote");
    assert_eq!((antidote.status, antidote.formula, antidote.aims_at_allies(), antidote.aim), (Some(StatusEffect::cure(Status::Poison, 63)), Formula::None, true, Aim::One));
    assert_eq!(item(12, "Echo Screen").status, Some(StatusEffect::cure(Status::Silence, 63)));
    assert_eq!(item(52, "Eye drop").status, Some(StatusEffect::cure(Status::Darkness, 63)));
    let remedy = item(15, "Remedy");
    assert_eq!(
        remedy.status,
        Some(cures(&[Status::Sleep, Status::Poison, Status::Sadness, Status::Fury, Status::Silence, Status::Paralysis, Status::Darkness])),
        "a Remedy cures every ailment the rules play"
    );
    assert_eq!(item(17, "Speed Drink").status, Some(StatusEffect::inflict(Status::Haste, 63)));
    // The ones thrown at the enemies aim at them, a whole side at once for the powders and the gong.
    let dazers = item(59, "Dazers");
    assert_eq!((dazers.status, dazers.aims_at_allies(), dazers.aim), (Some(StatusEffect::inflict(Status::Paralysis, 63)), false, Aim::One));
    let powder = table.get(26).unwrap().spec;
    assert_eq!((powder.status.map(|s| (s.set, s.mode)), powder.aims_at_allies(), powder.aim), (Some((set(&[Status::Sleep]), StatusMode::Inflict)), false, Aim::Side));
    assert_eq!(table.get(23).unwrap().spec.status.map(|s| s.set), Some(set(&[Status::Stop])));
    assert_eq!(table.get(44).unwrap().spec.status.map(|s| (s.set, s.mode)), Some((set(&[Status::Barrier]), StatusMode::Inflict)));
    assert!(table.get(44).unwrap().spec.aims_at_allies() && table.get(44).unwrap().spec.aim == Aim::Side);
    // A damaging item with a status keeps its damage (Deadly Waste: a poison that sometimes sticks).
    let waste = table.get(38).unwrap().spec;
    assert_eq!((waste.status.map(|s| s.set), waste.formula != Formula::None, table.info(38).unwrap().kind), (Some(set(&[Status::Poison])), true, ItemKind::Damage));
    // The cures of the statuses the rules do not play, and the items with more to them, are not in the table.
    for gone in [9u16, 10, 11, 16, 18, 19, 24] {
        assert!(table.info(gone).is_none(), "item {gone} is not played");
    }
    let status_items = table.indexes().filter(|i| table.info(*i).is_some_and(|info| info.kind == ItemKind::Status)).count();
    assert!(status_items >= 15, "{status_items} items only change statuses");

    // Spells.
    let spell = |index: usize, name: &str| {
        assert_eq!(kernel.magic_name(index as u16).as_deref(), Some(name), "attack {index}");
        ActionSpec::from_attack(&kernel.attack(index).unwrap()).unwrap_or_else(|| panic!("{name} is decoded"))
    };
    let bio = spell(39, "Bio");
    assert_eq!((bio.formula, bio.status.map(|s| (s.set, s.chance, s.mode))), (Formula::Spell, Some((set(&[Status::Poison]), 12, StatusMode::Inflict))));
    for (index, name, status, on_allies) in [
        (3usize, "Regen", Status::Regen, true),
        (19, "Haste", Status::Haste, true),
        (15, "Barrier", Status::Barrier, true),
        (11, "Sleepel", Status::Sleep, false),
        (13, "Silence", Status::Silence, false),
        (20, "Slow", Status::Slow, false),
        (21, "Stop", Status::Stop, false),
    ] {
        let spec = spell(index, name);
        assert_eq!((spec.formula, spec.status.map(|s| s.set), spec.aims_at_allies(), spec.aim), (Formula::None, Some(set(&[status])), on_allies, Aim::OneOrSide), "{name}");
    }
    let poisona = spell(4, "Poisona");
    assert_eq!((poisona.status, poisona.aims_at_allies()), (Some(StatusEffect::cure(Status::Poison, 63)), true));
    assert_eq!(spell(5, "Esuna").status.map(|s| s.mode), Some(StatusMode::Cure));
    // The grimoire of a starting party has none of them (Regen and the rest come with the Restore and Status materia).
    for who in CharacterId::ALL {
        let spells = fmv_ff7_battle::rules::party_spells(&kernel, who);
        assert!(spells.iter().all(|s| s.spec.formula != Formula::None), "{who:?}");
    }

    // The enemies: a set bit says "affected", so a record with every bit set is immune to nothing (209 of them here), and
    // the statuses a good many enemies shrug off (Stop, Paralysis) are the ones that are clear.
    let (mut enemies, mut immune_to_nothing) = (0, 0);
    let mut immune = [0u32; 32];
    let (mut attacks, mut with_status, mut status_only, mut unplayed_only) = (0, 0, 0, 0);
    for scene in (0..MAX_SCENES).filter_map(|i| bin.scene(i)) {
        for record in scene.enemies.iter().flatten() {
            enemies += 1;
            let c = Combatant::from_enemy(record, None);
            immune_to_nothing += usize::from(c.status_immune.is_empty());
            for status in c.status_immune.iter() {
                immune[usize::from(status.index())] += 1;
            }
            assert!(c.status_immune.bits() & !record.status_immunities == c.status_immune.bits(), "a clear bit is an immunity and only that");
        }
        for attack in &scene.attacks {
            attacks += 1;
            let rec = &attack.record;
            match ActionSpec::from_attack(&rec.clone()) {
                Some(spec) if spec.status.is_some() => {
                    with_status += 1;
                    status_only += usize::from(spec.formula == Formula::None);
                    assert!(spec.status.unwrap().set.difference(MODELLED).is_empty());
                }
                _ => unplayed_only += usize::from(rec.status_chance_mode != 0xFF && rec.status_mask != 0xFFFF_FFFF && rec.status_mask != 0),
            }
        }
    }
    assert!(enemies > 500 && immune_to_nothing > 150, "{immune_to_nothing} of {enemies} enemies are affected by everything");
    assert!(immune[Status::Stop.index() as usize] > 100 && immune[Status::Paralysis.index() as usize] > 100);
    assert!(immune[Status::Stop.index() as usize] < enemies && immune[Status::Haste.index() as usize] == immune[Status::Slow.index() as usize]);
    assert!(attacks > 2000 && with_status > 200 && status_only > 80, "{with_status} of {attacks} scene attacks carry a status the rules play ({status_only} have nothing else)");
    println!(
        "statuses: {status_items} status items; {with_status} of {attacks} scene attacks carry one ({status_only} have nothing else, {unplayed_only} only ones the rules do not play); {immune_to_nothing} of {enemies} enemies are affected by everything"
    );

    // Real fights with real status attacks: formations whose enemies inflict something, played with the policy of the other
    // battle test plus the cures. Deterministic, finished, and what the engine says about statuses is sane at every step.
    let level = kernel.init_character_by_id(0).unwrap().level;
    let items = Arc::new(table);
    let sting_formations: Vec<usize> = (0..bin.formation_count())
        .filter(|g| {
            let f = bin.formation(*g).unwrap();
            enemies_for_formation(f.scene, f.slot).iter().any(|e| e.actions.iter().any(|a| a.spec.status.is_some()))
        })
        .collect();
    assert!(sting_formations.len() > 20, "{} formations have a stinging enemy", sting_formations.len());
    let play = |seed: u64, formation: usize| {
        let f = bin.formation(formation).unwrap();
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, level, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Aeris, level, None, None).unwrap(),
        ];
        let mut state = BattleState::with_config(
            seed,
            party,
            enemies_for_formation(f.scene, f.slot),
            BattleConfig { start: StartMode::from_layout(f.setup().layout), ..BattleConfig::default() },
        )
        .with_items(Arc::clone(&items));
        let mut log = Vec::new();
        let mut round = 0u64;
        for _ in 0..4_000 {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(100));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                round += 1;
                let enemy = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).unwrap();
                // Antidote, Echo Screen, Remedy and Eye drop on whoever has what they cure; else Fire and attack.
                let sick = state.party_ids().into_iter().find(|a| state.statuses(*a).intersects(set(&[Status::Poison, Status::Silence, Status::Darkness, Status::Sleep])));
                let candidates = [
                    sick.map(|target| Command::Item { item: 15, target: Target::One(target) }),
                    sick.map(|target| Command::Item { item: 8, target: Target::One(target) }),
                    Some(Command::Magic { spell: 27, target: Target::One(enemy) }),
                    Some(Command::Attack { target: enemy }),
                ];
                let pick = candidates[round as usize % candidates.len()].filter(|c| state.check_command(id, c).is_ok());
                let command = pick.unwrap_or(Command::Attack { target: enemy });
                log.extend(state.submit_command(id, command).unwrap_or_else(|e| panic!("{command:?}: {e}")));
            }
            for (i, c) in state.actors().iter().enumerate() {
                assert!(c.hp <= c.max_hp && c.mp <= c.max_mp);
                assert!(c.status.set().difference(MODELLED).is_empty(), "{} {i}: only the statuses the rules play are kept", c.name);
                assert!(c.is_alive() || c.status.is_empty(), "{} {i}: a knockout removes every status", c.name);
                assert_eq!(state.ready_actors().contains(&fmv_ff7_battle::rules::ActorId(i as u8)), i < 2 && c.ready && c.can_act());
            }
        }
        (state, log)
    };
    let (mut runs, mut inflicted, mut ticks, mut ended) = (0, 0, 0, 0);
    let mut kinds: std::collections::BTreeSet<Status> = std::collections::BTreeSet::new();
    for &formation in sting_formations.iter().step_by((sting_formations.len() / 12).max(1)) {
        for seed in 0..6u64 {
            let (state, log) = play(seed, formation);
            let (again, log_again) = play(seed, formation);
            assert_eq!((&state, &log), (&again, &log_again), "formation {formation} seed {seed}");
            runs += 1;
            for e in &log {
                match e {
                    BattleEvent::StatusInflicted { status, .. } => {
                        inflicted += 1;
                        kinds.insert(*status);
                    }
                    BattleEvent::StatusEnded { .. } => ended += 1,
                    BattleEvent::StatusTick { .. } => ticks += 1,
                    _ => {}
                }
            }
        }
    }
    assert!(inflicted > 0, "no status was ever inflicted in {runs} real fights");
    println!("real fights with statuses: {runs} runs over {} stinging formations, {inflicted} inflicted ({} kinds), {ended} ended, {ticks} turns", sting_formations.len(), kinds.len());
}

/// The limit data of the owner's `kernel.bin`: the first limit of every character and what the engine
/// builds from it, the requirements that reproduce GideonD's published table (so the layout is the one
/// the guide documents) and the two attack records the damage table leans on. Plain `assert!`s: a
/// failure names the character or the id, never what the file holds.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_limits_have_the_documented_ids_names_requirements_and_records() {
    use fmv_ff7_battle::action::element;
    use fmv_ff7_battle::limit::LimitBreak;

    let Some(path) = kernel_path() else { return };
    let kernel = Kernel::open(&path).expect("kernel.bin parses");

    // (character, first limit id, its name, kills for levels 2 and 3, uses for 1-2, 2-2 and 3-2).
    type Row = (CharacterId, u16, &'static str, [u16; 2], [u16; 3]);
    let table: [Row; 9] = [
        (CharacterId::Cloud, 128, "Braver", [120, 320], [8, 7, 6]),
        (CharacterId::Barret, 135, "Big Shot", [80, 160], [9, 8, 6]),
        (CharacterId::Tifa, 149, "Beat Rush", [96, 192], [9, 7, 6]),
        (CharacterId::Aeris, 142, "Healing Wind", [80, 160], [8, 6, 5]),
        (CharacterId::RedXiii, 163, "Sled Fang", [72, 144], [8, 7, 6]),
        (CharacterId::Yuffie, 177, "Greased Lightning", [64, 128], [8, 7, 6]),
        (CharacterId::CaitSith, 170, "Dice", [40, 0xFFFF], [0xFFFF; 3]),
        (CharacterId::Vincent, 173, "Galian Beast", [40, 96], [0xFFFF; 3]),
        (CharacterId::Cid, 156, "Boost Jump", [60, 136], [7, 6, 5]),
    ];
    for (who, id, name, kills, uses) in table {
        let limits = kernel.limits(who).expect("a record");
        assert!(limits.first() == Some(id as u8), "{who:?}: the first limit");
        let shown = kernel.magic_name(id).unwrap_or_default();
        assert!(shown == name, "{who:?}: the name of limit {id} is not {name:?}");
        assert!(shown.chars().all(|c| (' '..='~').contains(&c)), "{who:?}: the name has an escape left in it");
        assert!(limits.kills == kills, "{who:?}: kills");
        assert!(limits.uses == uses, "{who:?}: uses");
        assert!(limits.divisors[0] > 0, "{who:?}: a level 1 divisor");
        // The levels' divisors rise: a higher level fills more slowly.
        let levels = if who == CharacterId::CaitSith { 2 } else { 4 };
        assert!(limits.divisors[..levels].windows(2).all(|w| w[0] < w[1]), "{who:?}: divisors");
        // What the engine builds: the id and divisor are the file's, and every one but Cait Sith plays.
        let built = LimitBreak::for_character(&kernel, who);
        assert!(built.is_some() == (who != CharacterId::CaitSith), "{who:?}: a limit break");
        if let Some(limit) = built {
            assert!(limit.attack == id && limit.divisor == limits.divisors[0], "{who:?}");
        }
    }
    // Cloud's four divisors, and the divisors behind the two documented anchors of the gauge: at level 2
    // Red XIII and Yuffie (300) and Cait Sith (180).
    assert!(kernel.limits(CharacterId::Cloud).unwrap().divisors == [140, 324, 435, 506]);
    assert!(kernel.limits(CharacterId::RedXiii).unwrap().divisors[1] == 300);
    assert!(kernel.limits(CharacterId::Yuffie).unwrap().divisors[1] == 300);
    assert!(kernel.limits(CharacterId::CaitSith).unwrap().divisors[1] == 180);

    // The records the damage table cites: Tifa's Beat Rush (a Hit is 1.25x: Power 20) and Vincent's
    // Beast Flare (3.75x, fire, magical: Power 60).
    let beat_rush = kernel.attack(98).expect("record 98");
    assert!(kernel.magic_name(98).unwrap_or_default().contains("Beat Rush") && beat_rush.power == 20);
    let beast_flare = kernel.attack(113).expect("record 113");
    assert!(kernel.magic_name(113).unwrap_or_default().contains("Beast Flare"));
    assert!(beast_flare.power == 60 && beast_flare.element_mask == element::FIRE && beast_flare.damage_calc >> 4 == 2);
    // The limit ids themselves (128 and up) are in no attack record of the kernel: there is nothing to decode.
    assert!(kernel.attack(128).is_none(), "the kernel's attack data ends at record 127");
}

/// Every formation of the owner's `scene.bin`: the escapable flag and the escape counter agree on which
/// fights cannot be run from, and the counters are the ones the community documents.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_formations_agree_on_which_fights_cannot_be_escaped() {
    use fmv_ff7_battle::scene::{BattleLayout, BattleSetup};

    let Some(path) = scene_path() else { return };
    let bin = SceneBin::open(&path).expect("scene.bin parses");
    let (mut escapable, mut bosses, mut sure) = (0, 0, 0);
    for id in 0..bin.formation_count() {
        let Some(formation) = bin.formation(id) else { continue };
        if formation.enemies().is_empty() {
            continue;
        }
        let setup = formation.setup();
        let flag = setup.flags & BattleSetup::ESCAPABLE_FLAG != 0;
        let counter = setup.escape_counter;
        assert!(flag == (counter < 9), "formation {id}: the flag and the counter disagree");
        assert!(setup.run_difficulty().is_some() == flag, "formation {id}");
        if flag {
            assert!((1..=4).contains(&counter), "formation {id}: counter {counter}");
            escapable += 1;
            sure += usize::from(setup.layout.makes_running_sure());
        } else {
            assert!(counter == 9 || counter == 10, "formation {id}: counter {counter}");
            bosses += 1;
        }
        // A back attack is never as easy to run from as a normal fight.
        if setup.layout == BattleLayout::BackAttack {
            assert!(counter != 1, "formation {id}: a back attack with difficulty 1");
        }
    }
    assert!(escapable > 600 && bosses > 50 && sure > 5, "{escapable} escapable, {bosses} not, {sure} sure");
    // md1stin's own formations (301, 302, 303) can be run from at difficulty 1.
    for id in [301usize, 302, 303] {
        assert!(bin.formation(id).unwrap().setup().run_difficulty() == Some(1), "formation {id}");
    }
}

/// A real battle with a limit and an escape: Cloud at level 6 has Braver from the file, a full gauge
/// lets him use it on the first enemy of 301, and an escape ends the battle with nothing paid.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn real_cloud_uses_braver_and_the_party_can_run() {
    use fmv_ff7_battle::rules::{ActionRef, ActorId, EscapeRule, Reward, LIMIT_MAX};

    let (Some(scene), Some(kernel)) = (scene_path(), kernel_path()) else { return };
    let scenes = SceneBin::open(&scene).expect("scene.bin parses");
    let kernel = Kernel::open(&kernel).expect("kernel.bin parses");
    let formation = scenes.formation(301).expect("formation 301");
    let build = |limit: u16| {
        let mut cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).expect("Cloud");
        cloud.limit = limit;
        let enemies = enemies_for_formation(formation.scene, formation.slot);
        let config = BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() };
        let setup = formation.setup();
        let rule = setup.run_difficulty().map(|difficulty| EscapeRule { difficulty, sure: setup.layout.makes_running_sure() });
        BattleState::with_config(7, vec![cloud], enemies, config).with_escape(rule)
    };
    let mut battle = build(LIMIT_MAX);
    let enemy = battle.enemy_ids()[0];
    let info = battle.limit_for(ActorId(0)).expect("Cloud has a limit");
    assert!(info.attack == 128 && !info.targets_allies);
    let events = battle.submit_command(ActorId(0), Command::Limit { target: Target::One(enemy) }).expect("Braver");
    assert!(events.contains(&BattleEvent::Action { actor: ActorId(0), action: ActionRef::Limit(128) }));
    assert!(battle.limit_gauge(ActorId(0)) == 0);
    let hp_start = formation.enemies()[0].record.hp;
    assert!(battle.actor(enemy).unwrap().hp < hp_start, "Braver hurt the first enemy");
    // Braver (3x) hurts more than a plain attack of the same Cloud.
    let mut plain = build(0);
    plain.submit_command(ActorId(0), Command::Attack { target: enemy }).expect("attack");
    assert!(battle.actor(enemy).unwrap().hp <= plain.actor(enemy).unwrap().hp);

    // Aerith's Healing Wind is the whole party (its record is "All Al (NS)"), and nothing smaller: the one
    // limit that cannot be aimed at one target.
    {
        use fmv_ff7_battle::action::Aim;
        let mut aerith = Combatant::from_character(&kernel, CharacterId::Aeris, 6, None, None).expect("Aerith");
        aerith.limit = LIMIT_MAX;
        let mut hurt = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).expect("Cloud");
        hurt.hp = hurt.max_hp / 4;
        let config = BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() };
        let enemies = enemies_for_formation(formation.scene, formation.slot);
        let mut party = BattleState::with_config(7, vec![aerith, hurt], enemies, config);
        let wind = party.limit_for(ActorId(0)).expect("Aerith has a limit");
        assert_eq!((wind.attack, wind.targets_allies, wind.aim), (142, true, Aim::Side));
        assert!(party.check_command(ActorId(0), &Command::Limit { target: Target::One(ActorId(1)) }).is_err());
        let hp_before = party.actor(ActorId(1)).unwrap().hp;
        let events = party.submit_command(ActorId(0), Command::Limit { target: Target::AllAllies }).expect("Healing Wind");
        let healed: Vec<ActorId> = events.iter().filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(*target) } else { None }).collect();
        assert_eq!(healed, [ActorId(0), ActorId(1)], "every ally, in order");
        assert!(party.actor(ActorId(1)).unwrap().hp > hp_before);
    }

    // Escape: the formation can be run from at difficulty 1, and with the party's first strike every check
    // succeeds; the battle ends with nothing paid.
    let mut running = build(0);
    assert!(running.can_escape());
    running.submit_command(ActorId(0), Command::Escape).expect("run");
    assert!(running.outcome() == Some(Outcome::Escaped));
    assert!(running.rewards() == Reward::default());
}
