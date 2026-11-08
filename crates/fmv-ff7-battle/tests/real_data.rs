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
    use fmv_ff7_battle::action::{element, ActionKind, Formula, ItemKind, ItemTable, Spell};
    use fmv_ff7_battle::kernel::ItemStack;
    use fmv_ff7_battle::rules::{party_spells, restore_amount, STARTER_MATERIA};

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
    // Megalixir (every ally) is left out, and so are the status cures.
    assert!(table.info(6).is_none() && table.info(8).is_none());
    assert!(
        table.len() >= 8 && table.len() < 60,
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
    // ally, a Potion, Defend, Phoenix Down on the dead.
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
                target: enemy,
            },
            Command::Magic {
                spell: 0,
                target: weakest,
            },
            Command::Item {
                item: 0,
                target: weakest,
            },
            Command::Defend,
            dead.map_or(Command::Attack { target: enemy }, |target| Command::Item {
                item: 7,
                target,
            }),
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
