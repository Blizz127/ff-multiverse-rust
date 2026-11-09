//! The pictures that decide what each clip of a real battle model is, and the pictures of the
//! presentation: contact sheets of the owner's own models and of whole fights, drawn through the real
//! renderer, read back, written as PNGs and LOOKED AT.
//!
//! Everything here is data-gated and `#[ignore]`d. It needs the owner's FF7 data (`FMV_FF7_DATA`, the
//! game's `data/` directory, or `FMV_FF7_LIBRARY`, the state directory of the imported library; read-only)
//! and, for the pictures, a GPU or a software Vulkan/GL driver (lavapipe works). The GPU tests must run one
//! at a time (`--test-threads=1`): lavapipe crashes when several devices run in one process.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_SHOT_DIR=~/.cache/claude-tmp/fmv-shots/sw-bpresent \
//!   cargo test -p fmv-client --bin fmv-client -- --ignored battle::scene::sheets --test-threads=1 --nocapture
//! ```
//!
//! * `party_every_slot_sheets` draws EVERY slot of each of the nine playable characters frame by frame
//!   (`slots-<name>-<n>.png`: one row per slot, the slot, the frame and the animation scripts of the character's
//!   own `ab` entry that play it written on each picture), `party_attack_chain_sheets` the clips of each
//!   character's two blows in the order they play (`attack-chain-<name>-<short|long>.png`),
//!   `party_roles_overview_sheets` one picture per role for all nine, and `enemy_slot_evidence_sheets` the MP and
//!   the Guard Hound: the evidence for the tables of [`super::clips`].
//! * `fight_party_attack_strips` plays each character's blow in a real fight (`attack-strip-<name>.png`).
//! * `enemy_attack_chain_sheets` draws the clips of the enemy attacks of the evidence set (the MP's Machine Gun and Tonfa, the
//!   Guard Hound's Bite and Tentacle, the Mono Drive's Drilldrive and Fire) in the order their scripts play them
//!   (`enemy-chain-<name>.png`); `fight_enemy_attack_sheets` and `fight_enemy_death_sheets` perform them and a beaten enemy in a
//!   real formation (`enemy-attack-<name>-wide.png` and `-close-0.png`, `enemy-death-<name>-close-0.png`); `fight_defend_sheets`
//!   the guard (`defend-close-<n>.png`).
//! * `party_clips_agree_across_the_characters`, `party_blows_are_the_scripts_clips`,
//!   `party_attack_range_by_starting_weapon`, `enemy_book_is_the_same_in_every_scene` and
//!   `enemy_attack_chains_are_the_scripts_clips` check the numbers behind the tables without a GPU.
//! * `fight_*` run real fights (a real engine, the client's own replay, the scene, the overlay) and write
//!   the sheets of the presentation: an MP's attack chain, a party attack with the number at its impact, a
//!   cast with its effect, the elements, a beaten enemy's fade and the victory pose.

// `chunks_exact(4)` over RGBA bytes would be `as_chunks` on Rust 1.88+; the workspace keeps its MSRV lower
// (see `avatar`), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::camera::{BattleCamera, View};
use super::clips::{Chain, ClipSet, Kit, Role};
use super::effects::Element;
use super::shots::{canon, contact_sheet, open_real, Real, Server, ME, H, W};
use super::*;
use crate::avatar::ground::{circle, GroundShape};
use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use crate::battle::{draw_battle_overlay, BattleClient};
use crate::ui::draw::DrawList;
use crate::ui::testkit::{pixel, save, Offscreen};
use fmv_ff7::battle_model::script::{code, AnimationScripts};
use fmv_ff7::battle_model::{BattleCharacter, BattleModel, BattleModels};
use fmv_ff7::model::mat4_translate;
use fmv_ff7::playable::Character;
use fmv_ff7_battle::rules::{Command, Target};
use fmv_protocol::{BattleSetup, ServerMsg};
use std::path::PathBuf;

// ---------------------------------------------------------------------------------------------
// Where the data is
// ---------------------------------------------------------------------------------------------

fn battle_lgp() -> Option<PathBuf> {
    let data = PathBuf::from(std::env::var_os("FMV_FF7_DATA")?);
    Some(data.join("battle").join("battle.lgp"))
}

fn scene_bin() -> Option<fmv_ff7_battle::scene::SceneBin> {
    let data = PathBuf::from(std::env::var_os("FMV_FF7_DATA")?);
    let path = ["lang-en/battle/scene.bin", "battle/scene.bin"].iter().map(|p| data.join(p)).find(|p| p.is_file())?;
    fmv_ff7_battle::scene::SceneBin::open(path).ok()
}

/// The pack of `model` as the scene sees it (the very function the scene's own rigs use).
fn clip_set(model: &BattleModel) -> ClipSet {
    model::clip_set(&model.animations, &model.slot_ok, 15.0)
}

// ---------------------------------------------------------------------------------------------
// The numbers behind the table (no GPU)
// ---------------------------------------------------------------------------------------------

/// The slots of a chain in the order they play: the approach, the strike, the way back.
fn chain_slots(chain: &Chain) -> Vec<usize> {
    chain.approach.iter().chain(&chain.strike).chain(&chain.back).map(|&s| usize::from(s)).collect()
}

/// All nine characters' packs give the shared slots every role, the scripts they all share say what those slots are,
/// and each death clip ends in the pose of the dead slot: the numbers behind "slot 18 ends where slot 6 is" of
/// [`super::clips`].
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn party_clips_agree_across_the_characters() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    for which in BattleCharacter::ALL {
        let model = models.load_character(which).expect("the character loads");
        let kit = Kit::party(clip_set(&model), &model.scripts);
        let name = which.display_name();
        eprintln!(
            "{name:20} slots {:2}, weapon tracks {:2}, scripts {:2}: attack {:?} shoot {:?} item {:?} {:?} cast {:?} hurt {:?} death {:?} dead {:?} victory {:?} defend {:?} run {:?} back {:?}",
            model.animations.len(),
            model.weapon_tracks.len(),
            model.scripts.len(),
            kit.attack.as_ref().map(chain_slots),
            kit.shoot.as_ref().map(chain_slots),
            kit.item,
            kit.item_at_enemy,
            kit.cast,
            kit.hurt,
            kit.death,
            kit.dead,
            kit.victory,
            kit.defend,
            kit.run,
            kit.back
        );
        // Sephiroth is not a party member: he has no use for the table here.
        if which == BattleCharacter::Sephiroth {
            continue;
        }
        assert!(kit.attack.is_some() && kit.shoot.is_some() && kit.hurt.is_some() && kit.death.is_some() && kit.dead.is_some(), "{name}");
        assert!(kit.victory.is_some() && kit.defend.is_some() && kit.item.is_some() && kit.item_at_enemy.is_some() && kit.cast.iter().all(Option::is_some), "{name}");
        // The scripts all nine share, with the clips of the shared table: hop forward, hop back, hurt, death, run, a
        // spell (the hop, begin, hold, end, the hop back) and the item thrown to an ally (9) and an enemy (10); the
        // victory script starts with the victory pose.
        let clips_of = |index: usize| model.scripts.get(index).map(|s| s.clips());
        for (index, expected) in [(0usize, vec![0u8]), (1, vec![1]), (3, vec![3]), (4, vec![4]), (5, vec![15]), (7, vec![18]), (8, vec![5]), (13, vec![7]), (17, vec![3, 12, 13, 14, 4]), (32, vec![3, 9, 4]), (33, vec![3, 10, 4])] {
            assert_eq!(clips_of(index), Some(expected), "{name}: script {index}");
        }
        assert_eq!(clips_of(12).and_then(|c| c.first().copied()), Some(2), "{name}: the victory script");
        // The guard is script 8's one clip (slot 5), not the held pose of the cover script (9: a move to the ally, slot 8, a
        // wait, the return).
        assert_eq!(kit.defend, Some(5), "{name}: the Defend command plays script 8's clip");
        assert_eq!(model.scripts[9].clips(), [8], "{name}: slot 8 is the cover script's pose");
        // The cast script hops forward (3) before the gathering (12, 13) and back (4) after its end (14). The plan can cover at
        // most MAX_SPEEDUP x MAX_IMPACT_DELAY of natural time before the impact; the gathering alone is longer, so `fit` would
        // always cut the hop forward whole (and a hop back with no hop forward is a jump in place): the hops are not played.
        let cap = plan::MAX_SPEEDUP * cue::MAX_IMPACT_DELAY * f64::from(kit.clips.fps);
        let gathering: f64 = [12u16, 13].iter().map(|&s| f64::from(kit.clips.frames_of(s))).sum();
        let hop = f64::from(kit.clips.frames_of(3));
        eprintln!("{name}: cast hop forward {hop} ticks, gathering {gathering} ticks, the plan covers {cap:.0} ticks before the impact");
        assert!(gathering > cap, "{name}: the gathering ({gathering} ticks) is longer than what the cap lets the plan cover ({cap:.0}) without the hop ({hop})");
        // The weapon rides on a track of its own for each slot played: every slot the kit uses, the clips of the blows
        // included, has a real one (a slot with a stand-in track would draw the weapon at some other slot's place).
        let mut played = vec![0usize, 2, 3, 4, 6, 7, 9, 10, 12, 13, 14, 15, 18];
        played.extend(kit.defend.map(usize::from));
        for chain in [kit.attack.as_ref(), kit.shoot.as_ref()].into_iter().flatten() {
            played.extend(chain_slots(chain));
        }
        for slot in played {
            assert!(model.weapon_track_ok.get(slot).copied().unwrap_or(false), "{name}: the weapon's track for slot {slot} is a stand-in or missing");
        }
        // The death clip lands lying down, in the pose the dead slot holds: the same height (±40 units) and the
        // same root height.
        let (death, dead) = (kit.death.unwrap(), kit.dead.unwrap());
        let top = |slot: u16, frame: usize| {
            let pose = model.pose(usize::from(slot), frame).expect("poses");
            model.model.posed_bounds(&pose, &fmv_ff7::model::mat4_identity()).map(|(lo, _)| -lo[1]).expect("bounds")
        };
        let last = model.animations[usize::from(death)].frame_count() - 1;
        let (from_clip, held) = (top(death, last), top(dead, 0));
        assert!((from_clip - held).abs() < 40.0, "{name}: the death clip ends {from_clip:.0} high, the dead pose is {held:.0}");
        let standing = top(0, 0);
        assert!(held < 0.7 * standing, "{name}: the dead pose is lying down (the moogle is round: 0.64) ({held:.0} high, standing {standing:.0})");
        // The root's height at the end of the death clip is the dead pose's.
        let root_y = |slot: u16, frame: usize| model.animations[usize::from(slot)].frame(frame).map_or(0.0, |f| f.root_translation[1]);
        assert!((root_y(death, last) - root_y(dead, 0)).abs() < 1.0, "{name}");
    }
}

/// A blow of the table of [`super::clips`]: the approach, the strike, the tick the blow lands at (at most the strike's
/// length), the way back, and the index of the approach clip the actor travels from (`None`: the script does not move).
type Blow = (&'static [u16], &'static [u16], u16, &'static [u16], Option<usize>);

/// **The table of the playable characters' blows of [`super::clips`], held to the owner's own animation scripts**: what
/// each of the nine plays for a blow with a short-range weapon (script 20) and a long-range one (script 21), and
/// when it lands. The first version of the table took slot 10 for the attack of everyone and the owner saw an
/// uppercut: slot 10 is the item thrown at an enemy.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn party_blows_are_the_scripts_clips() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    use BattleCharacter::*;
    let table: [(BattleCharacter, Blow, Blow); 9] = [
        (Cloud, (&[26, 27], &[30, 28], 1, &[29], Some(1)), (&[3], &[31], 16, &[4], None)),
        (Barret, (&[26, 27], &[30, 28], 3, &[29], Some(1)), (&[3], &[34], 14, &[4], None)),
        (Tifa, (&[26, 27], &[30, 28], 8, &[29], Some(1)), (&[3], &[34], 18, &[4], None)),
        (Aerith, (&[25, 26], &[29, 27], 2, &[28], Some(1)), (&[3], &[34], 25, &[4], None)),
        (RedXiii, (&[26, 27], &[30, 28], 6, &[29], Some(1)), (&[3], &[33], 16, &[4], None)),
        (Yuffie, (&[3], &[30], 6, &[4], None), (&[3], &[34], 14, &[4], None)),
        (CaitSith, (&[26, 27], &[30, 28], 4, &[29], Some(1)), (&[3], &[31], 23, &[4], None)),
        (Vincent, (&[3], &[30, 31, 32], 6, &[4], None), (&[3], &[30, 31, 32], 6, &[4], None)),
        (Cid, (&[25, 26], &[29, 27], 1, &[28], Some(1)), (&[3], &[33], 20, &[4], None)),
    ];
    for (which, short, long) in table {
        let model = models.load_character(which).expect("the character loads");
        let clips = clip_set(&model);
        let kit = Kit::party(clips.clone(), &model.scripts);
        let name = which.display_name();
        for (what, blow, chain) in [("short range (script 20)", short, &kit.attack), ("long range (script 21)", long, &kit.shoot)] {
            let chain = chain.as_ref().unwrap_or_else(|| panic!("{name}: no chain for {what}"));
            let (approach, strike, impact, back, travel) = blow;
            let strike_ticks: u16 = strike.iter().map(|&s| clips.frames_of(s)).sum();
            eprintln!("{name:20} {what}: approach {:?} strike {:?} lands at tick {} of {strike_ticks} back {:?} travels from {:?}", chain.approach, chain.strike, chain.impact, chain.back, chain.closes_in.then_some(chain.travel_from));
            assert_eq!((chain.approach.as_slice(), chain.strike.as_slice(), chain.back.as_slice()), (approach, strike, back), "{name}: {what}");
            assert_eq!(chain.impact, impact.min(strike_ticks), "{name}: {what}: the blow lands at tick {impact} of the strike");
            assert_eq!(chain.closes_in.then_some(chain.travel_from), travel, "{name}: {what}");
        }
    }
}

/// Every enemy gets the same attacks and slots from every scene that has it (the scene's start does not say which
/// formation it is, so the book is keyed by the enemy's id), and the MP's are the ones scene.bin names.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn enemy_book_is_the_same_in_every_scene() {
    let Some(scenes) = scene_bin() else { return };
    let mut seen: std::collections::BTreeMap<u16, Vec<(u16, u8)>> = std::collections::BTreeMap::new();
    let mut disagreements = Vec::new();
    for scene in scenes.scenes() {
        for (id, record) in scene.enemy_ids.iter().zip(scene.enemies.iter()) {
            let (Some(id), Some(record)) = (id, record) else { continue };
            let attacks = model::EnemyBook::attacks_of_record(record);
            match seen.get(id) {
                Some(first) if *first != attacks => disagreements.push((scene.index, *id)),
                Some(_) => {}
                None => {
                    seen.insert(*id, attacks);
                }
            }
        }
    }
    eprintln!("{} distinct enemies; {} scene records that differ from the first: {:?}", seen.len(), disagreements.len(), disagreements);
    assert!(disagreements.is_empty(), "the book is keyed by enemy id: {disagreements:?}");
    let book = model::EnemyBook::from_scenes(&scenes);
    // The numbers are SCRIPT indices (3 is the MP's Machine Gun, which plays slots 2 to 5; see `enemy_attack_chains_are_the_scripts_clips`).
    assert_eq!(book.attacks_of(16), [(272, 3), (273, 4)], "the MP: Machine Gun is script 3, Tonfa script 4");
    assert_eq!(book.attacks_of(17), [(274, 3), (275, 4)], "the Guard Hound: Bite and Tentacle");
    assert_eq!(book.attacks_of(18), [(276, 3), (27, 4)], "the Mono Drive: Drilldrive and Fire");
    assert!(book.attacks_of(9999).is_empty());
}

/// **What an enemy plays, held to the owner's own files**: the numbers of `scene.bin`'s `animation_indices` are SCRIPT indices
/// of the enemy model's own `ab` entry, not slots (the MP's Machine Gun is script 3, which plays slots 2, 3, 4 and 5 of
/// its pack in turn; its Tonfa is script 4: 6, 7, 8 and 9, and slot 8 is the baton's swing, which the first version never
/// played). So the chain of an attack is read from its script ([`Chain::from_script`]), the hurt clip from script 1 and the
/// death clip, when the enemy has one, from script 2. Pins the chains of the MP, the Guard Hound and the Mono Drive, and
/// tallies every attack the engine can use of every enemy of the archive (the rest are procedural).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn enemy_attack_chains_are_the_scripts_clips() {
    let (Some(scenes), Some(path)) = (scene_bin(), battle_lgp()) else { return };
    let book = model::EnemyBook::from_scenes(&scenes);
    let mut models = BattleModels::open(path).expect("battle.lgp");
    // An enemy's model and its kit (`None`: the archive has no model for it: the scene has more enemy records than it has models).
    let mut load = |id: u16| -> Option<(BattleModel, Kit)> {
        let m = models.load_enemy(id).ok()?;
        let kit = Kit::enemy(clip_set(&m), &m.scripts, &book.attacks_of(id));
        Some((m, kit))
    };
    // The shape of a chain: approach, strike, the tick the blow lands at, way back, whether the script moves the actor to its
    // target, the clip it travels from, how far its clips carry it (battle units), the frame the strike's last clip runs home from.
    type ChainShape = (Vec<u16>, Vec<u16>, u16, Vec<u16>, bool, usize, u32, Option<u16>);
    let shape = |c: &Chain| -> ChainShape { (c.approach.clone(), c.strike.clone(), c.impact, c.back.clone(), c.closes_in, c.travel_from, c.carries, c.home_run) };

    // The MP: the Machine Gun runs up (slot 2, which carries it 970 units: the script does not move it to the target), holds
    // two poses (3, 4) and a long clip (5) that keeps the aim for ten frames and then runs home; the shot lands 8 ticks after
    // the run-up, 5 ticks into the long clip. The Tonfa leaps (6: its root carries it 581 units), moves to the target, winds up
    // (7), swings (8: it lands 4 ticks in), returns and recovers (9).
    let (mp, kit) = load(16).expect("the MP");
    eprintln!("the MP: hurt {:?} death {:?}, {:?}", kit.hurt, kit.death, kit.attacks.iter().map(|(id, c)| (*id, shape(c))).collect::<Vec<_>>());
    assert_eq!((kit.hurt, kit.death, kit.unplayable.len()), (Some(1), None, 0));
    assert_eq!(shape(kit.enemy_chain(Some(272)).expect("the Machine Gun")), (vec![2], vec![3, 4, 5], 8, vec![], false, 0, 970, Some(10)));
    assert_eq!(shape(kit.enemy_chain(Some(273)).expect("the Tonfa")), (vec![6, 7], vec![8], 4, vec![9], true, 0, 581, None));
    // The baton rides on a real track of its own in every slot the MP plays.
    for slot in 0..=9usize {
        assert!(mp.weapon_track_ok.get(slot).copied().unwrap_or(false), "the MP's baton track for slot {slot}");
    }
    // The Guard Hound: the Bite (script 3) crouches (2), moves to the target, leaps (3), bites (4: it lands 15 ticks in) and
    // settles (5); the Tentacle (script 4) winds up (6), moves, strikes (7, 8: 5 ticks in) and recovers (9); it dies with slot 14.
    let (_, kit) = load(17).expect("the Guard Hound");
    assert_eq!((kit.hurt, kit.death, kit.unplayable.len()), (Some(1), Some(14), 0));
    let bite = shape(kit.enemy_chain(Some(274)).expect("the Bite"));
    assert_eq!((bite.0, bite.1, bite.2, bite.3, bite.4, bite.5), (vec![2, 3], vec![4], 15, vec![5], true, 1));
    let tentacle = shape(kit.enemy_chain(Some(275)).expect("the Tentacle"));
    assert_eq!((tentacle.0, tentacle.1, tentacle.2, tentacle.3, tentacle.4, tentacle.5), (vec![6, 7], vec![8], 5, vec![9], true, 1));
    // The Mono Drive: the Drilldrive (script 3) crouches (2), moves to the target, drills (3, 4: the blow lands 2 ticks in) and
    // recovers (5); its Fire (script 4, the spell Fire) gathers (6, 7, 8), runs the effect and ends (9), where it hovers: it
    // casts where it is (the script places it far from the target, which is no run).
    let (_, kit) = load(18).expect("the Mono Drive");
    assert_eq!((kit.hurt, kit.death, kit.unplayable.len()), (Some(1), None, 0));
    let drill = shape(kit.enemy_chain(Some(276)).expect("the Drilldrive"));
    assert_eq!((drill.0, drill.1, drill.2, drill.3, drill.4, drill.5), (vec![2], vec![3, 4], 2, vec![5], true, 1));
    assert_eq!(shape(kit.enemy_chain(Some(27)).expect("the Fire")), (vec![6, 7, 8], vec![9], 0, vec![], false, 0, 0, None));

    // Every attack the engine can use of every enemy (it models the damaging ones): how many have a script that plays, and why
    // the others do not. The scene has records for unused test enemies whose names are not text: those are counted apart.
    let all_slots = ClipSet { frames: vec![1; 256], ok: vec![true; 256], travel: vec![0.0; 256], lead: vec![0; 256], fps: 15.0 };
    let why_not = |m: &BattleModel, script: u8| -> &'static str {
        let Some(s) = m.scripts.get(usize::from(script)) else { return "no such script" };
        let marks = s.uses(code::DAMAGE) || s.uses(code::DAMAGE_QUIET) || s.uses(code::EFFECT);
        match (Chain::from_script(s, &all_slots, Role::Enemy), marks) {
            (Some(_), _) => "a clip that is not the pack's own",
            (None, true) => "nothing is struck with after the mark",
            (None, false) => "no mark of where it lands",
        }
    };
    let mut reasons: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let (mut hurts, mut deaths): (std::collections::BTreeMap<Option<u16>, usize>, std::collections::BTreeMap<Option<u16>, usize>) = Default::default();
    // The attacks of the named enemies that play, by shape: a damage mark and a return, a damage mark and no return, an effect.
    let mut shapes = [0usize; 3];
    let mut seen = std::collections::BTreeSet::new();
    let (mut total, mut playable, mut at_once, mut closing, mut homing, mut idles) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut named_total, mut named_playable) = (0usize, 0usize);
    let (mut weapon_clips, mut weaponless_clips) = (0usize, 0usize);
    for scene in scenes.scenes() {
        for (id, record) in scene.enemy_ids.iter().zip(scene.enemies.iter()) {
            let (Some(id), Some(record)) = (id, record) else { continue };
            if !seen.insert(*id) {
                continue;
            }
            let Some((m, kit)) = load(*id) else { continue };
            let named = !record.name.contains('\u{FFFD}');
            if named {
                *hurts.entry(kit.hurt).or_default() += 1;
                *deaths.entry(kit.death).or_default() += 1;
            }
            for (&attack, &script) in record.attack_ids.iter().zip(record.animation_indices.iter()) {
                if attack == 0xFFFF || script == 0xFF || scene.attack_by_id(attack).and_then(fmv_ff7_battle::action::EnemyAction::from_scene).is_none() {
                    continue;
                }
                total += 1;
                named_total += usize::from(named);
                let Some(chain) = kit.enemy_chain(Some(attack)).filter(|_| !kit.unplayable.contains(&attack)) else {
                    if named {
                        *reasons.entry(why_not(&m, script)).or_default() += 1;
                    }
                    continue;
                };
                playable += 1;
                named_playable += usize::from(named);
                // Whatever plays is whole: every slot is the pack's own clip, the blow lands inside the strike, the strike is not empty.
                let slots: Vec<u16> = chain.approach.iter().chain(&chain.strike).chain(&chain.back).copied().collect();
                assert!(slots.iter().all(|&s| kit.clips.usable(usize::from(s)).is_some() && s != 0), "enemy {id} attack {attack}: {chain:?}");
                let strike_ticks: u16 = chain.strike.iter().map(|&s| kit.clips.frames_of(s)).sum();
                assert!(!chain.strike.is_empty() && chain.impact <= strike_ticks, "enemy {id} attack {attack}: {chain:?}");
                assert!(chain.travel_from <= chain.approach.len(), "enemy {id} attack {attack}: {chain:?}");
                if named {
                    let script = m.scripts.get(usize::from(script));
                    let first_mark = script.and_then(|s| s.ops.iter().find_map(|op| op.code().filter(|c| matches!(*c, code::DAMAGE | code::DAMAGE_QUIET | code::EFFECT))));
                    let returns = script.is_some_and(|s| s.uses(code::RETURN));
                    shapes[match (first_mark, returns) {
                        (Some(code::EFFECT), _) => 2,
                        (_, true) => 0,
                        _ => 1,
                    }] += 1;
                }
                // An enemy that carries a weapon draws it on a track of its own for each slot it plays: how many of the clips played lack one.
                if !m.weapons.is_empty() {
                    weapon_clips += slots.len();
                    weaponless_clips += slots.iter().filter(|&&s| !m.weapon_track_ok.get(usize::from(s)).copied().unwrap_or(false)).count();
                }
                at_once += usize::from(chain.impact == 0);
                closing += usize::from(chain.closes_in);
                homing += usize::from(chain.home_run.is_some());
                idles += usize::from(m.scripts.get(usize::from(script)).is_some_and(|s| s.clips().contains(&0)));
            }
        }
    }
    eprintln!(
        "{} enemies, {total} attacks the engine can use: {playable} play their script ({at_once} land at once, at an effect or a damage mark of 0; {closing} close in; {homing} run home with their last clip; {idles} scripts play the idle clip, which is dropped); the other {} are procedural",
        seen.len(),
        total - playable
    );
    eprintln!("of the enemies with names: {named_playable} of {named_total} play their script ({} with a damage mark and a return, {} with a damage mark and no return, {} with an effect); those that do not: {reasons:?}", shapes[0], shapes[1], shapes[2]);
    eprintln!("their hurt clips: {hurts:?}; their death clips: {deaths:?}");
    eprintln!("the enemies that carry a weapon play {weapon_clips} clips in their chains, {weaponless_clips} of them without a track for the weapon");
    assert!(named_playable * 100 >= named_total * 97, "{named_playable} of {named_total} play their script: {reasons:?}");
    // Every enemy that has a name is hurt by script 1's clip, slot 1, but three (their hurt script plays nothing: no reaction
    // of their own); some (not most) have a death clip of their own.
    assert_eq!((hurts.get(&Some(1)).copied(), hurts.get(&None).copied(), hurts.len()), (Some(326), Some(3), 2), "{hurts:?}");
    let with_death: usize = deaths.iter().filter(|(slot, _)| slot.is_some()).map(|(_, n)| n).sum();
    assert_eq!((with_death, deaths.get(&None).copied()), (45, Some(284)), "{deaths:?}");
    assert_eq!((deaths.get(&Some(10)).copied(), deaths.get(&Some(14)).copied()), (Some(23), Some(12)), "{deaths:?}");
    assert_eq!(shapes, [362, 79, 354]);
}

// ---------------------------------------------------------------------------------------------
// Sheets of slots
// ---------------------------------------------------------------------------------------------

const TILE_W: u32 = 150;
const TILE_H: u32 = 170;

/// A model that can be drawn in any slot at any frame from the side, for the sheets.
struct Rig {
    asset: Arc<AvatarAsset>,
    weapon: Option<Arc<AvatarAsset>>,
    model: BattleModel,
    view: View,
}

impl Rig {
    /// `height` is how tall the model is (the tile is made to fit it).
    fn new(model: BattleModel, height: f64) -> Self {
        let stand = |m: &fmv_ff7::FieldModel, clip: &fmv_ff7::FieldAnimation| {
            let character = Character { appearance: NON_PLAYABLE, model: m.clone(), idle: clip.clone(), walk: clip.clone(), run: clip.clone() };
            Arc::new(AvatarAsset::new(character).expect("the model has a mesh"))
        };
        let asset = stand(&model.model, &model.animations[0]);
        let weapon = model.weapons.first().zip(model.weapon_tracks.first()).map(|(w, t)| stand(w, t));
        // From +x looking at the model: +z is the screen's right, so a model faces the left of the picture.
        let dist = 3600.0;
        let camera = BattleCamera::look_at([dist, -height * 0.55, 0.0], [0.0, -height * 0.5, 0.0]).expect("a camera");
        let view = View { camera, width: TILE_W, height: TILE_H, zoom: f64::from(TILE_H) * dist / (height * 1.35), shift: [0.0, 0.0] };
        Self { asset, weapon, model, view }
    }

    /// One picture of `slot` at `frame`, standing in place (the root's travel along the ground taken out, as
    /// the scene plays it), with `label` written on it.
    fn tile(&self, gpu: &mut Offscreen, slot: usize, frame: usize, label: &str) -> Vec<u8> {
        let view = &self.view;
        let clip = &self.model.animations[slot];
        let root = clip.frame(frame % clip.frame_count()).map_or([0.0; 3], |f| f.root_translation);
        let place = mat4_translate([-root[0], 0.0, -root[2]]);
        let camera = view.camera.matrix();
        let mut models = Vec::new();
        if let Some(pose) = self.model.model.pose(clip, frame) {
            let mut positions = Vec::new();
            if skin(&self.asset, &pose, &place, &camera, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(&self.asset), positions });
            }
        }
        if let (Some(weapon), Some(m)) = (&self.weapon, self.model.weapon_pose(0, slot, frame)) {
            let mut positions = Vec::new();
            if skin(weapon, &[m], &place, &camera, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(weapon), positions });
            }
        }
        let to_view = |p: Pos| {
            let c = view.camera.to_camera([f64::from(p.x), 0.0, f64::from(p.y)]);
            view.pinhole(c, 100.0).map(|[u, v]| Pos::new(u, v, 0.0))
        };
        let depth = view.camera.to_camera([0.0, 0.0, 0.0])[2] as f32;
        let ground: Vec<GroundShape> = circle(&to_view, Pos::new(0.0, 0.0, 0.0), 420.0).map(|o| GroundShape::disc(o, [0, 0, 0], 0.45, depth)).into_iter().collect();
        let frame_data = BattleFrame {
            clear_rgb: [70, 78, 92],
            stage: Vec::new(),
            location: None,
            models: AvatarFrame { models, tints: Vec::new(), identities: Vec::new(), zoom: view.zoom as f32, overlay_bias: 3.0, ground, scenery: None },
            tints: Vec::new(),
            identities: Vec::new(),
            camera: view.gpu_camera(),
        };
        let mut overlay = DrawList::new();
        // One line of text per line of the label (the first yellow, the others white).
        for (i, line) in label.lines().enumerate() {
            let color = if i == 0 { [255, 255, 120, 255] } else { [255, 255, 255, 255] };
            overlay.draw_text_shadowed(3, 3 + 10 * i as i32, 1, color, [0, 0, 0, 255], line);
        }
        gpu.render_battle(TILE_W, TILE_H, &frame_data, &overlay)
    }

    /// A sheet with a row per slot of `slots` and `per_row` frames spread over each.
    fn sheet(&self, gpu: &mut Offscreen, slots: &[usize], per_row: usize) -> (u32, u32, Vec<u8>) {
        let (sw, sh) = (TILE_W * per_row as u32, TILE_H * slots.len() as u32);
        let mut sheet = vec![0u8; (sw * sh * 4) as usize];
        for (row, &slot) in slots.iter().enumerate() {
            let n = self.model.animations[slot].frame_count();
            for col in 0..per_row {
                let frame = if per_row == 1 { 0 } else { col * (n - 1) / (per_row - 1) };
                let tile = self.tile(gpu, slot, frame, &format!("s{slot} f{frame}/{n}"));
                for y in 0..TILE_H {
                    let src = &tile[(y * TILE_W * 4) as usize..((y + 1) * TILE_W * 4) as usize];
                    let dst = (((row as u32 * TILE_H + y) * sw + col as u32 * TILE_W) * 4) as usize;
                    sheet[dst..dst + src.len()].copy_from_slice(src);
                }
            }
        }
        (sw, sh, sheet)
    }
}

impl Rig {
    /// Every slot of the model in `slots`, one row each, up to `per_row` frames spread over the clip (a clip of
    /// fewer frames gets one tile per frame, the rest of the row stays dark), each labelled with the slot, the
    /// frame, the clip's length and `note(slot)`.
    fn every_slot(&self, gpu: &mut Offscreen, slots: &[usize], per_row: usize, note: &dyn Fn(usize) -> String) -> (u32, u32, Vec<u8>) {
        let (sw, sh) = (TILE_W * per_row as u32, TILE_H * slots.len() as u32);
        let mut sheet = vec![0u8; (sw * sh * 4) as usize];
        for (row, &slot) in slots.iter().enumerate() {
            let n = self.model.animations[slot].frame_count();
            let tiles = n.min(per_row);
            for col in 0..tiles {
                let frame = if tiles == 1 { 0 } else { col * (n - 1) / (tiles - 1) };
                let label = format!("s{slot} f{frame}/{n}\n{}", note(slot));
                let tile = self.tile(gpu, slot, frame, &label);
                for y in 0..TILE_H {
                    let src = &tile[(y * TILE_W * 4) as usize..((y + 1) * TILE_W * 4) as usize];
                    let dst = (((row as u32 * TILE_H + y) * sw + col as u32 * TILE_W) * 4) as usize;
                    sheet[dst..dst + src.len()].copy_from_slice(src);
                }
            }
        }
        (sw, sh, sheet)
    }
}

/// Equal tiles laid out `per_row` to a row.
fn grid(tiles: &[Vec<u8>], per_row: usize) -> (u32, u32, Vec<u8>) {
    let rows = tiles.len().div_ceil(per_row);
    let (sw, sh) = (TILE_W * per_row as u32, TILE_H * rows as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (n, tile) in tiles.iter().enumerate() {
        let (ox, oy) = ((n % per_row) as u32 * TILE_W, (n / per_row) as u32 * TILE_H);
        for y in 0..TILE_H {
            let src = &tile[(y * TILE_W * 4) as usize..((y + 1) * TILE_W * 4) as usize];
            let dst = (((oy + y) * sw + ox) * 4) as usize;
            sheet[dst..dst + src.len()].copy_from_slice(src);
        }
    }
    (sw, sh, sheet)
}

/// What the scene plays for each role, for all nine characters at once: one row per character, one picture per
/// role (the blow with a short-range weapon and with a long-range one each at the frame it lands in, the item thrown
/// up to an ally and forward to an enemy, the cast's hold, a hurt flinch, a fall, the end of the victory): the one
/// picture that says the shared slots and the character's own blows of [`super::clips`] hold for everybody.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn party_roles_overview_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let mut rows: Vec<Vec<Vec<u8>>> = Vec::new();
    for which in BattleCharacter::ALL.into_iter().filter(|c| *c != BattleCharacter::Sephiroth) {
        let model = models.load_character(which).expect("the character loads");
        let clips = clip_set(&model);
        let kit = Kit::party(clips.clone(), &model.scripts);
        let standing = model.pose(0, 0).and_then(|p| model.model.posed_bounds(&p, &fmv_ff7::model::mat4_identity())).map_or(900.0, |(lo, _)| f64::from(-lo[1]));
        let rig = Rig::new(model, standing);
        let name = which.display_name().to_string();
        let landing = |chain: &Option<Chain>| chain.as_ref().map(|c| c.landing(&clips)).map(|(slot, frame)| (slot, usize::from(frame)));
        let at = |slot: Option<u16>, fraction: f32| slot.map(|slot| (slot, (fraction * (clips.frames_of(slot) - 1) as f32).round() as usize));
        let roles = [
            ("blow, short range", landing(&kit.attack)),
            ("blow, long range", landing(&kit.shoot)),
            ("item to an ally", at(kit.item, 0.5)),
            ("item to an enemy", at(kit.item_at_enemy, 0.5)),
            ("cast hold", at(kit.cast[1], 0.5)),
            ("hurt", at(kit.hurt, 0.4)),
            ("death", at(kit.death, 0.35)),
            ("victory", at(kit.victory, 1.0)),
        ];
        let tiles = roles
            .iter()
            .map(|&(role, picked)| {
                let (slot, frame) = picked.expect("the role has a slot");
                let slot = usize::from(slot);
                rig.tile(&mut gpu, slot, frame, &format!("{role} s{slot} f{frame}/{}\n{name}", clips.frames_of(slot as u16)))
            })
            .collect();
        rows.push(tiles);
    }
    for (half, chunk) in rows.chunks(5).enumerate() {
        let tiles: Vec<Vec<u8>> = chunk.iter().flatten().cloned().collect();
        let (w, h, sheet) = grid(&tiles, 8);
        save(&format!("party-roles-overview-{half}"), w, h, &sheet);
    }
}

/// The clips of each character's two blows, in the order they play, eight frames spread over each clip, each labelled
/// with its part of the blow (approach, strike with the tick the blow lands at, way back):
/// `attack-chain-<name>-<short|long>.png`, the evidence of the per-character table of [`super::clips`] drawn.
/// `FMV_SHEET_CHARACTERS` (prefixes, comma separated) picks characters (all nine without it).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn party_attack_chain_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let wanted: Option<Vec<String>> = std::env::var("FMV_SHEET_CHARACTERS").ok().map(|s| s.split(',').map(|p| p.trim().to_ascii_lowercase()).collect());
    for which in BattleCharacter::ALL.into_iter().filter(|c| *c != BattleCharacter::Sephiroth) {
        if wanted.as_ref().is_some_and(|w| !w.iter().any(|p| p == which.prefix())) {
            continue;
        }
        let model = models.load_character(which).expect("the character loads");
        let kit = Kit::party(clip_set(&model), &model.scripts);
        let rig = Rig::new(model, 900.0);
        let name = which.display_name().split(' ').next().unwrap_or("x").to_ascii_lowercase();
        for (range, chain) in [("short", &kit.attack), ("long", &kit.shoot)] {
            let chain = chain.as_ref().expect("the character has the blow");
            let slots = chain_slots(chain);
            let note = |slot: usize| {
                let slot = slot as u16;
                if chain.approach.contains(&slot) {
                    let travels = chain.closes_in && chain.approach.get(chain.travel_from) == Some(&slot);
                    if travels || (!chain.closes_in && chain.approach.first() == Some(&slot)) { "approach, travels" } else { "approach" }.to_string()
                } else if chain.strike.contains(&slot) {
                    format!("strike, lands @{}", chain.impact)
                } else {
                    "way back".to_string()
                }
            };
            let (w, h, sheet) = rig.every_slot(&mut gpu, &slots, 8, &note);
            save(&format!("attack-chain-{name}-{range}"), w, h, &sheet);
        }
    }
}

/// The clips of the evidence set's enemy attacks (the MP's Machine Gun and Tonfa, the Guard Hound's Bite and Tentacle, the Mono Drive's
/// Drilldrive and Fire), in the order their scripts play them, eight frames spread over each clip, each labelled with its part of the
/// attack (approach, strike with the tick the blow lands at, way back, and where a clip carries the actor home):
/// `enemy-chain-<name>.png`, the evidence of the rule of [`super::clips`] drawn.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn enemy_attack_chain_sheets() {
    let (Some(path), Some(scenes), Some(mut gpu)) = (battle_lgp(), scene_bin(), Offscreen::new()) else { return };
    let book = model::EnemyBook::from_scenes(&scenes);
    let mut models = BattleModels::open(path).expect("battle.lgp");
    for (name, enemy, attack) in [("mp-machine-gun", 16u16, 272u16), ("mp-tonfa", 16, 273), ("guard-hound-bite", 17, 274), ("guard-hound-tentacle", 17, 275), ("mono-drive-drilldrive", 18, 276), ("mono-drive-fire", 18, 27)] {
        let model = models.load_enemy(enemy).expect("the enemy loads");
        let kit = Kit::enemy(clip_set(&model), &model.scripts, &book.attacks_of(enemy));
        let chain = kit.enemy_chain(Some(attack)).expect("the attack plays its script").clone();
        let slots = chain_slots(&chain);
        let rig = Rig::new(model, 800.0);
        let note = |slot: usize| {
            let slot = slot as u16;
            if let Some(at) = chain.approach.iter().position(|&s| s == slot) {
                if at == chain.travel_from && (chain.closes_in || chain.carries > 0) { "approach, travels" } else { "approach" }.to_string()
            } else if chain.strike.contains(&slot) {
                // (The tile is 18 characters wide.)
                match (chain.home_run, chain.strike.last() == Some(&slot)) {
                    (Some(from), true) => format!("strike @{} home f{from}", chain.impact),
                    _ => format!("strike, lands @{}", chain.impact),
                }
            } else {
                "way back".to_string()
            }
        };
        let (w, h, sheet) = rig.every_slot(&mut gpu, &slots, 8, &note);
        save(&format!("enemy-chain-{name}"), w, h, &sheet);
    }
}

/// For each slot of the pack: the indices of the animation scripts of the model's own `ab` entry that play it
/// (a script that shares its offset with an earlier one is that one). The evidence the sheets are labelled with.
fn slots_played_by(prefix: &str, slots: usize) -> Vec<Vec<usize>> {
    let mut by_slot = vec![Vec::new(); slots];
    let Some(path) = battle_lgp() else { return by_slot };
    let Ok(mut lgp) = fmv_ff7::lgp::LgpArchive::open_path(path) else { return by_slot };
    let Ok(bytes) = lgp.read(&format!("{prefix}ab")) else { return by_slot };
    let Ok(scripts) = AnimationScripts::parse(&bytes) else { return by_slot };
    let mut seen = Vec::new();
    for i in 0..scripts.len() {
        let offset = scripts.offset(i);
        if seen.contains(&offset) {
            continue;
        }
        seen.push(offset);
        let Some(Ok(script)) = scripts.script(i) else { continue };
        for clip in script.clips() {
            if let Some(list) = by_slot.get_mut(usize::from(clip)) {
                if !list.contains(&i) {
                    list.push(i);
                }
            }
        }
    }
    by_slot
}

/// EVERY slot of each playable character, eight frames spread over each clip, the scripts that play it written
/// on each picture: `slots-<name>-<n>.png`, seven slots to a picture. The evidence for the per-character table
/// of [`super::clips`]. `FMV_SHEET_CHARACTERS` (prefixes, comma separated: `rt,sb`) picks characters (all nine
/// without it).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn party_every_slot_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let wanted: Option<Vec<String>> = std::env::var("FMV_SHEET_CHARACTERS").ok().map(|s| s.split(',').map(|p| p.trim().to_ascii_lowercase()).collect());
    for which in BattleCharacter::ALL.into_iter().filter(|c| *c != BattleCharacter::Sephiroth) {
        if wanted.as_ref().is_some_and(|w| !w.iter().any(|p| p == which.prefix())) {
            continue;
        }
        let model = models.load_character(which).expect("the character loads");
        let played = slots_played_by(which.prefix(), model.animations.len());
        let all: Vec<usize> = (0..model.animations.len()).filter(|&s| model.slot_ok[s]).collect();
        let rig = Rig::new(model, 900.0);
        let name = which.display_name().split(' ').next().unwrap_or("x").to_ascii_lowercase();
        for (n, chunk) in all.chunks(7).enumerate() {
            let note = |slot: usize| {
                let by = &played[slot];
                let tracked = if rig.model.weapon_track_ok.get(slot).copied().unwrap_or(false) { "" } else { " no weapon track" };
                if by.is_empty() { format!("no script plays it{tracked}") } else { format!("scripts {by:?}{tracked}") }
            };
            let (w, h, sheet) = rig.every_slot(&mut gpu, chunk, 8, &note);
            save(&format!("slots-{name}-{n}"), w, h, &sheet);
        }
    }
}

/// The MP's and the Guard Hound's slots, with the root's travel kept for the MP's run-up (its slot 2 carries
/// it 1 140 units forward).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn enemy_slot_evidence_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    for (id, name) in [(16u16, "mp"), (17, "guard-hound")] {
        let model = models.load_enemy(id).expect("the enemy loads");
        let used: Vec<usize> = (0..model.animations.len()).filter(|&s| model.slot_ok[s] && model.animations[s].frame_count() > 1).take(12).collect();
        let rig = Rig::new(model, 800.0);
        for (n, chunk) in used.chunks(6).enumerate() {
            let (w, h, sheet) = rig.sheet(&mut gpu, chunk, 6);
            save(&format!("enemy-clips-{name}-{n}"), w, h, &sheet);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Whole fights
// ---------------------------------------------------------------------------------------------

/// A real fight: the owner's rules, a server, the client's own mirror, the scene and a GPU.
struct Fight {
    gpu: Offscreen,
    client: BattleClient,
    server: Server,
    scene: BattleScene,
    /// The clock of the fight, seconds.
    now: f64,
}

impl Fight {
    /// Formation `formation` (301: two MPs) against the canon Cloud, Tifa and Barret at level 12 (companions: the canon
    /// characters' own battle models, clips and blows are what the sheets show), the party ready (a preemptive start),
    /// the engine seeded with `seed`.
    fn new(real: &Real, formation: u16, seed: u64) -> Option<Self> {
        Self::with_limits(real, formation, seed, [0; 3])
    }

    /// The same fight with the party starting with the limit gauges `limits` (Cloud, Tifa, Barret; 255 is
    /// full: protocol v7).
    fn with_limits(real: &Real, formation: u16, seed: u64, limits: [u16; 3]) -> Option<Self> {
        Self::with_party(real, formation, seed, [0, 2, 1], limits)
    }

    /// The same fight with a party of the canon characters `characters` (FF7's ids: Cloud 0, Barret 1, Tifa 2, Aerith 3, Red
    /// XIII 4, Yuffie 5, Cait Sith 6, Vincent 7, Cid 8), all of them companions (no player is in these battles).
    fn with_party(real: &Real, formation: u16, seed: u64, characters: [u8; 3], limits: [u16; 3]) -> Option<Self> {
        let gpu = Offscreen::new()?;
        let limited = |i: usize, character: u8| BattleMember { limit: limits[i], ..canon(character) };
        let setup = BattleSetup {
            formation_id: formation,
            seed,
            party: characters.iter().enumerate().map(|(i, &c)| limited(i, c)).collect(),
            preemptive: true,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
        };
        let server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
        let mut client = BattleClient::new(ME);
        client.set_rules(Some(Arc::clone(&real.rules)));
        client.on_message(0.0, server.start(&setup, real.rules.data_fingerprint()));
        let start = client.scene_start().expect("a battle to draw");
        let mut kit = real.kit();
        let scene = BattleScene::new(&start, client.engine(), &mut kit).expect("a scene");
        let mut fight = Self { gpu, client, server, scene, now: 1.0 };
        let events = client_events(&mut fight.client);
        fight.scene.update(1.0, &events, fight.client.engine());
        fight.sync_statuses(1.0);
        Some(fight)
    }

    /// Give the scene the statuses the panels show at `t`, as the window does after every update (`scene_for`): they change when
    /// the performance that changes them lands, not when the message arrives.
    fn sync_statuses(&mut self, t: f64) {
        if self.client.engine().is_some() {
            if let Some(view) = self.client.view(t) {
                self.scene.show_statuses(&view);
            }
        }
    }

    /// The server says `msg` at the fight's clock: the client mirrors it and the scene is told.
    fn say(&mut self, msg: Option<ServerMsg>) {
        if let Some(msg) = msg {
            self.client.on_message(self.now, msg);
        }
        let events = client_events(&mut self.client);
        self.scene.update(self.now, &events, self.client.engine());
        self.sync_statuses(self.now);
    }

    /// The fight at time `t` (never before an earlier call): the scene brought up to `t`, drawn with the
    /// overlay as the window draws it, `label` written in the corner.
    fn frame(&mut self, t: f64, label: &str) -> Vec<u8> {
        self.scene.update(t, &[], self.client.engine());
        self.sync_statuses(t);
        let view = self.client.view(t).expect("a view");
        let scene_view = self.scene.view(W, H);
        let mut overlay = DrawList::new();
        draw_battle_overlay(&view, &self.scene.head_anchors(&scene_view), W, H, &mut overlay);
        overlay.draw_text_shadowed(8, 24, 2, [255, 255, 255, 255], [0, 0, 0, 255], label);
        self.gpu.render_battle(W, H, &self.scene.frame(&scene_view, false), &overlay)
    }

    /// A close-up of actor `i` at time `t`: the fight drawn `w` x `h` (without the overlay) and the `cw` x `ch` of it around
    /// the actor, `label` written in the corner of the crop.
    fn close_up(&mut self, t: f64, i: usize, label: &str, (w, h): (u32, u32), (cw, ch): (u32, u32)) -> Vec<u8> {
        self.scene.update(t, &[], self.client.engine());
        self.sync_statuses(t);
        let view = self.scene.view(w, h);
        let at = self.scene.pose_of(i).and_then(|(pos, ..)| view.to_pixels([pos[0], pos[1] - 380.0, pos[2]])).unwrap_or([w as f32 / 2.0, h as f32 / 2.0]);
        let x0 = (at[0] as i32 - cw as i32 / 2).clamp(0, (w - cw) as i32) as u32;
        let y0 = (at[1] as i32 - ch as i32 / 2).clamp(0, (h - ch) as i32) as u32;
        let mut overlay = DrawList::new();
        overlay.draw_text_shadowed(x0 as i32 + 6, y0 as i32 + 6, 2, [255, 255, 120, 255], [0, 0, 0, 255], label);
        let full = self.gpu.render_battle(w, h, &self.scene.frame(&view, false), &overlay);
        let mut out = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            let from = (((y0 + y) * w + x0) * 4) as usize;
            out[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize].copy_from_slice(&full[from..from + (cw * 4) as usize]);
        }
        out
    }

    /// Frames at `arrival + offset` for each offset, written as a contact sheet named `name`.
    fn sheet(&mut self, name: &str, arrival: f64, offsets: &[f64], columns: usize) -> Vec<Vec<u8>> {
        let frames: Vec<Vec<u8>> = offsets.iter().map(|&o| self.frame(arrival + o, &format!("+{o:.2} s"))).collect();
        let (sw, sh, sheet) = contact_sheet(&frames, W, H, columns);
        save(name, sw, sh, &sheet);
        frames
    }

    /// The server steps (the party is ready from the start; the enemies' gauges fill) until `found` likes a
    /// message, delivering each at the fight's clock; `None` after too long.
    fn until(&mut self, mut found: impl FnMut(&ServerMsg) -> bool) -> Option<ServerMsg> {
        for _ in 0..4000 {
            self.now += 0.03;
            let Some(msg) = self.server.step(30) else { continue };
            let hit = found(&msg);
            self.say(Some(msg.clone()));
            if hit {
                return Some(msg);
            }
        }
        None
    }
}

fn client_events(client: &mut BattleClient) -> Vec<BattleEvent> {
    client.take_events()
}

/// Is this message the start of an action by an enemy with `attack`?
fn enemy_attacks_with(msg: &ServerMsg, attack: u16) -> bool {
    matches!(msg, ServerMsg::BattleEvent { events, .. } if events.iter().any(|e| matches!(e, BattleEvent::Action { action: ActionRef::EnemyAttack(id), .. } if *id == attack)))
}

/// How many pixels of two equally sized pictures differ by more than `by` in some channel.
fn changed(a: &[u8], b: &[u8], by: u8) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| (0..3).any(|k| p[k].abs_diff(q[k]) > by)).count()
}

/// How many pixels of `shot` near `(x, y)` (within `r`) are brighter than `level` in every channel.
fn bright_near(shot: &[u8], x: f32, y: f32, r: i32, level: u8) -> usize {
    let mut n = 0;
    for dy in -r..=r {
        for dx in -r..=r {
            let (px, py) = ((x as i32 + dx).clamp(0, W as i32 - 1) as u32, (y as i32 + dy).clamp(0, H as i32 - 1) as u32);
            n += usize::from(pixel(shot, W, px, py).iter().all(|&c| c > level));
        }
    }
    n
}

const SEED: u64 = 0x5CE7E;

/// The MP's attack chain: the clips of the script `scene.bin` names for each attack (Machine Gun, script 3: slots 2, 3, 4, 5;
/// Tonfa, script 4: 6, 7, 8, 9), with the hurt clip of whoever it hits and the number at the impact.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_mp_attack_chain() {
    let Some(real) = open_real() else { return };
    // The two MPs act in one step: look for a seed where one fires its Machine Gun and the other swings its
    // Tonfa, so that one picture shows both chains.
    let mut found = None;
    for seed in SEED..SEED + 60 {
        let Some(mut fight) = Fight::new(&real, 301, seed) else { return };
        let Some(msg) = fight.until(|m| enemy_attacks_with(m, 272) || enemy_attacks_with(m, 273)) else { continue };
        let ServerMsg::BattleEvent { events, .. } = &msg else { unreachable!("a battle message") };
        let used: Vec<(usize, u16)> = events
            .iter()
            .filter_map(|e| match e {
                BattleEvent::Action { actor, action: ActionRef::EnemyAttack(id) } => Some((usize::from(actor.0), *id)),
                _ => None,
            })
            .collect();
        if used.iter().any(|&(_, id)| id == 272) && used.iter().any(|&(_, id)| id == 273) {
            found = Some((fight, used, seed));
            break;
        }
    }
    let Some((mut fight, used, seed)) = found else {
        eprintln!("no seed in 60 made the two MPs use both attacks at once: skipped");
        return;
    };
    let arrival = fight.now;
    eprintln!("seed {seed:#x}: the MPs' attacks at {arrival:.2}: {used:?}");
    for (mp, attack) in used {
        let slots: Vec<u16> = fight.scene.director.segments_of(mp).iter().map(|s| s.slot).collect();
        eprintln!("MP {mp}: attack {attack} plays slots {slots:?}");
        // The Machine Gun is script 3: the run-up (2), two poses (3, 4), the long clip (5) that holds the aim for ten frames
        // and runs home; the Tonfa script 4: the leap (6), the wind-up (7), the swing (8) that lands four ticks in, the
        // recovery (9). The numbers scene.bin names (3, 4) are the scripts, not slots.
        let expected: &[u16] = if attack == 272 { &[2, 3, 4, 5, 5, 5] } else { &[6, 7, 8, 8, 9] };
        assert_eq!(slots, expected, "attack {attack} plays the clips of its script");
    }
    fight.sheet("mp-attack-chain-machine-gun-and-tonfa", arrival, &[0.05, 0.2, 0.35, 0.55, 0.7, 0.9, 1.2, 1.7], 2);
}

/// Whether each of the nine characters' starting weapon is short range, as the real rules say (what decides if the
/// character runs up to its target or stands and shoots: [`super::super::look_in`]). Printed, and held to what the
/// game's own weapons are: swords, gloves, staves, spears and claws are short range, guns and shuriken are not.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY)"]
fn party_attack_range_by_starting_weapon() {
    let Some(real) = open_real() else { return };
    let mut ranges = Vec::new();
    for ids in [[0u8, 1, 2], [3, 4, 5], [6, 7, 8]] {
        let setup = BattleSetup {
            formation_id: 301,
            seed: SEED,
            party: ids.iter().map(|&c| canon(c)).collect(),
            preemptive: true,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
        };
        let state = real.rules.build(&setup).expect("the formation builds");
        for (i, &c) in ids.iter().enumerate() {
            let short = state.actor(ActorId(i as u8)).expect("the actor").short_range;
            eprintln!("character {c}: starting weapon short range: {short}");
            ranges.push((c, short));
        }
    }
    let of = |c: u8| ranges.iter().find(|r| r.0 == c).map(|r| r.1);
    assert_eq!((of(0), of(2), of(3)), (Some(true), Some(true), Some(true)), "Cloud, Tifa and Aerith strike in melee");
    assert_eq!(of(1), Some(false), "Barret's gun arm is long range");
}

/// One party member's attack at the first MP in a real fight, ten frames over the whole performance (the approach,
/// the blow, the follow-through, the way back): `attack-strip-<name>.png`, to look at. `FMV_STRIP_CHARACTERS` (FF7
/// combat slot ids, comma separated) picks the characters (Cloud, Barret, Tifa and Aerith without it).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_party_attack_strips() {
    let Some(real) = open_real() else { return };
    let wanted: Vec<u8> = std::env::var("FMV_STRIP_CHARACTERS").ok().map(|s| s.split(',').filter_map(|p| p.trim().parse().ok()).collect()).unwrap_or_else(|| vec![0, 1, 2, 3]);
    for character in wanted {
        let others: Vec<u8> = [0u8, 2, 1].into_iter().filter(|&c| c != character).collect();
        let Some(mut fight) = Fight::with_party(&real, 301, SEED, [character, others[0], others[1]], [0; 3]) else { return };
        fight.now = 2.0;
        let first = fight.server.step(1);
        fight.say(first);
        let arrival = fight.now;
        let blow = fight.server.attack(ActorId(0));
        fight.say(blow);
        let name = ["cloud", "barret", "tifa", "aerith", "red", "yuffie", "cait", "vincent", "cid"].get(usize::from(character)).copied().unwrap_or("x");
        let offsets = [0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.7, 0.9, 1.2, 1.6];
        // What the director has the performer play at each instant: the clip and the frame (the evidence of the pictures).
        let played: Vec<String> = offsets.iter().map(|&o| fight.scene.director.motion(0, arrival + o).clip.map_or("idle".to_string(), |c| format!("s{} f{:.1}", c.slot, c.frame))).collect();
        eprintln!("{name}: {}", played.join(", "));
        // Each instant twice, in one pass (the scene's clock never goes back): the whole fight, and a close-up at twice the
        // resolution with the performer in the middle of the picture.
        let (mut wide, mut close) = (Vec::new(), Vec::new());
        for &o in &offsets {
            wide.push(fight.frame(arrival + o, &format!("+{o:.2} s")));
            close.push(fight.close_up(arrival + o, 0, &format!("+{o:.2} s"), (2 * W, 2 * H), (CLOSE_W, CLOSE_H)));
        }
        let (sw, sh, sheet) = contact_sheet(&wide, W, H, 2);
        save(&format!("attack-strip-{name}"), sw, sh, &sheet);
        let (zw, zh, zoom) = tile_sheet(&close, CLOSE_W, CLOSE_H, 5);
        save(&format!("attack-close-{name}"), zw, zh, &zoom);
    }
}

/// The size of a close-up of a fight (see [`Fight::close_up`]).
const CLOSE_W: u32 = 400;
const CLOSE_H: u32 = 330;

/// Equal `tw` x `th` tiles laid out `per_row` to a row.
fn tile_sheet(tiles: &[Vec<u8>], tw: u32, th: u32, per_row: usize) -> (u32, u32, Vec<u8>) {
    let rows = tiles.len().div_ceil(per_row);
    let (sw, sh) = (tw * per_row as u32, th * rows as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (n, tile) in tiles.iter().enumerate() {
        let (ox, oy) = ((n % per_row) as u32 * tw, (n / per_row) as u32 * th);
        for y in 0..th {
            let src = &tile[(y * tw * 4) as usize..((y + 1) * tw * 4) as usize];
            let dst = (((oy + y) * sw + ox) * 4) as usize;
            sheet[dst..dst + src.len()].copy_from_slice(src);
        }
    }
    (sw, sh, sheet)
}

/// A party member's blow: Cloud runs up, swings to the impact frame, the MP flinches with its hurt clip, sparks
/// fly and the number floats up at the very frame of the impact and not before.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_party_attack_with_the_number_at_the_impact() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let blow = fight.server.attack(ActorId(0));
    fight.say(blow);
    let impact = arrival + cue::impact_delay(Some(ActionRef::Attack));
    // The numbers: none before the blow lands, one at the instant it does, over the MP that was hit.
    assert!(fight.client.view(impact - 0.02).expect("a view").popups.is_empty(), "no number before the impact");
    let numbers = fight.client.view(impact).expect("a view").popups;
    assert!(numbers.iter().any(|p| p.target.0 >= 3 && p.age_ms == 0), "a number is born over the MP at the impact: {numbers:?}");
    // The scene: Cloud's own blow (his script 20: a crouch, a run, the chop of slot 30 that lands one tick in); at the
    // impact he is in the chop. The MP's hurt clip (slot 1) starts then. Slot 10 (an item thrown at an enemy) is never his blow.
    let swing = fight.scene.director.motion(0, impact - 0.001).clip.expect("Cloud swings");
    assert_eq!(swing.slot, 30, "{swing:?}");
    assert!((swing.frame - 1.0).abs() < 0.1, "the blow lands one tick into the chop: {swing:?}");
    assert!(fight.scene.director.segments_of(0).iter().all(|s| s.slot != 10));
    let target = numbers.iter().find(|p| p.target.0 >= 3).map(|p| usize::from(p.target.0)).expect("a target");
    assert_eq!(fight.scene.director.motion(target, impact - 0.01).clip, None);
    assert_eq!(fight.scene.director.motion(target, impact + 0.05).clip.map(|c| c.slot), Some(1));
    // The picture: sparks (bright pixels over the MP's chest) at the impact and not just before.
    let view = fight.scene.view(W, H);
    let chest = {
        let (pos, _, _, _) = fight.scene.pose_of(target).expect("a pose");
        view.to_pixels([pos[0], pos[1] - f64::from(fight.scene.actors[target].rig.height) * 0.55, pos[2]]).expect("on screen")
    };
    let frames = fight.sheet("party-attack-number-at-the-impact", arrival, &[0.0, 0.18, 0.36, 0.47, 0.56, 0.70, 0.95, 1.5], 2);
    let (before, after) = (&frames[3], &frames[4]);
    let (b, a) = (bright_near(before, chest[0], chest[1], 60, 235), bright_near(after, chest[0], chest[1], 60, 235));
    eprintln!("bright pixels over the MP's chest: {b} just before the impact, {a} just after");
    assert!(a > b + 40, "sparks at the impact: {b} -> {a}");
    assert!(changed(&frames[2], &frames[3], 20) > 1500, "the picture moves into the impact");
}

/// A cast with its effect: Cloud gathers a glow, begins the spell (slot 12) to the impact, and the burst breaks
/// over the MP in fire's colour; its number floats up then.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_cast_with_an_effect() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let target = fight.server.first_enemy().expect("an enemy");
    let spell = fight.server.state.actor(ActorId(0)).and_then(|c| c.spells.iter().find(|s| s.index == 27)).copied().expect("Cloud knows Fire");
    assert_eq!(Element::from_mask(spell.spec.element_mask, spell.spec.heals()), Element::Fire);
    let msg = fight.server.command(ActorId(0), Command::Magic { spell: 27, target: Target::One(target) });
    assert!(msg.is_some(), "the engine takes the spell");
    fight.say(msg);
    let impact = arrival + cue::impact_delay(Some(ActionRef::Magic(27)));
    assert!(fight.scene.director.fx().iter().any(|f| f.kind == effects::Kind::Charge && f.element == Element::Fire), "the caster gathers a fire glow");
    assert_eq!(fight.scene.director.motion(0, arrival + 0.1).clip.map(|c| c.slot), Some(12), "the cast begins");
    let numbers = fight.client.view(impact).expect("a view").popups;
    assert!(numbers.iter().any(|p| p.target.0 >= 3), "the number appears at the impact: {numbers:?}");
    let frames = fight.sheet("cast-fire-with-an-effect", arrival, &[0.0, 0.15, 0.30, 0.45, 0.60, 0.72, 0.9, 1.3], 2);
    // The burst is orange: more red than blue over the MP at the impact, where the frame before it had none.
    let impact_frame = &frames[4];
    let warm = impact_frame.chunks_exact(4).filter(|p| p[0] > 230 && p[1] > 90 && p[1] < 190 && p[2] < 110).count();
    assert!(warm > 200, "an orange burst over the MP: {warm} orange pixels");
    // The GPU kept every model it needed: effects share four meshes and did not push the actors out. Three
    // characters and their weapons, an MP and its baton, the location, four effect meshes: 13.
    let on_the_gpu = fight.gpu.cached_avatar_models();
    eprintln!("{on_the_gpu} models on the GPU after the cast");
    assert!((9..=16).contains(&on_the_gpu), "{on_the_gpu} models on the GPU (the cache starts over at 32)");
}

/// The four elements of the lead's list, one picture each at the moment of the burst: a fire bloom with embers,
/// ice's star of shards, bolt's streaks, cure's green sparkles.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_the_elements() {
    let Some(real) = open_real() else { return };
    let mut shots = Vec::new();
    // Cloud's spells: Fire 27, Ice 30, Bolt 33 on the MP and Cure 0 on Tifa.
    for (spell, ally) in [(27u16, false), (30, false), (33, false), (0, true)] {
        let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
        fight.now = 2.0;
        let first = fight.server.step(1);
        fight.say(first);
        let arrival = fight.now;
        let target = if ally { ActorId(1) } else { fight.server.first_enemy().expect("an enemy") };
        let Some(msg) = fight.server.command(ActorId(0), Command::Magic { spell, target: Target::One(target) }) else {
            eprintln!("the engine does not take spell {spell}: skipped");
            continue;
        };
        fight.say(Some(msg));
        let burst = fight.scene.director.fx().iter().find(|f| f.kind == effects::Kind::Blast || f.kind == effects::Kind::Mend).copied();
        eprintln!("spell {spell}: burst {burst:?}");
        shots.push(fight.frame(arrival + cue::impact_delay(Some(ActionRef::Magic(spell))) + 0.12, &format!("spell {spell}")));
    }
    if shots.len() == 4 {
        let (sw, sh, sheet) = contact_sheet(&shots, W, H, 2);
        save("cast-elements-fire-ice-bolt-cure", sw, sh, &sheet);
    }
}

/// An item and a guard: Cloud uses a Potion on Tifa (the item gesture, slot 9, and a sparkle on her at the
/// impact) while Barret guards (script 8's clip, slot 5).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_item_and_defend() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let potion = fight.server.command(ActorId(0), Command::Item { item: 0, target: Target::One(ActorId(1)) });
    assert!(potion.is_some(), "the engine takes a Potion");
    fight.say(potion);
    let guard = fight.server.command(ActorId(2), Command::Defend);
    assert!(guard.is_some(), "and Barret's guard");
    fight.say(guard);
    assert_eq!(fight.scene.director.motion(0, arrival + 0.2).clip.map(|c| c.slot), Some(9), "Cloud's item gesture");
    assert_eq!(fight.scene.director.motion(2, arrival + 0.3).clip.map(|c| c.slot), Some(5), "Barret guards with the clip of script 8");
    assert!(fight.scene.director.fx().iter().any(|f| f.kind == effects::Kind::Mend && f.target == 1 && f.scale < 1.0), "a lesser restoring sparkle on Tifa");
    assert!(fight.scene.director.fx().iter().any(|f| f.kind == effects::Kind::Toss && f.from == Some(0) && f.target == 1), "and the Potion in the air from Cloud");
    assert!(fight.scene.director.fx().iter().any(|f| f.kind == effects::Kind::Guard && f.target == 2), "Barret's guard shimmers");
    fight.sheet("item-and-defend", arrival, &[0.1, 0.3, 0.46, 0.6, 0.8, 1.1], 2);
}

/// A beaten enemy: it flinches with its hurt clip, leaves the list when its collapse begins, fades with a real
/// alpha and is gone; its shadow goes with it.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_an_enemy_fades_away() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let blow = fight.server.attack(ActorId(0));
    fight.say(blow);
    let dead: Vec<usize> = (3..5).filter(|&i| fight.scene.director.is_dying_or_dead(i, arrival + 10.0)).collect();
    assert_eq!(dead.len(), 1, "Cloud's first blow beats one MP at this seed (the same as the first real test)");
    let victim = dead[0];
    let begins = arrival + cue::impact_delay(Some(ActionRef::Attack)) + cue::DEATH_BEAT;
    // The list keeps the name until the collapse begins, and drops it then.
    let listed = |fight: &Fight, t: f64| fight.client.view(t).expect("a view").enemies.iter().filter(|b| b.alive).count();
    assert_eq!((listed(&fight, begins - 0.02), listed(&fight, begins + 0.02)), (2, 1), "the enemy list drops it as its collapse begins");
    let frames = fight.sheet("enemy-death-fade", arrival, &[0.45, 0.62, 0.75, 0.9, 1.05, 1.2, 1.35, 1.6], 2);
    // The fade is real: the pixels of the victim's model fade away steadily (the picture comes to be the bare
    // stage's where it stood), not in a blink.
    let alphas: Vec<f32> = [0.0, 0.15, 0.3, 0.45, 0.6].iter().map(|u| fight.scene.director.motion(victim, begins + u).alpha).collect();
    eprintln!("alpha during the fade: {alphas:?}");
    assert!(alphas.windows(2).all(|w| w[1] < w[0]) && alphas[4] < 0.05 && alphas[0] > 0.99, "{alphas:?}");
    assert!(changed(&frames[2], &frames[5], 10) > 800, "the victim's pixels change while it fades");
    assert!(fight.scene.director.motion(victim, begins + 0.7).gone);
}

/// The victory pose: the party beat the MPs, and each member still standing plays its victory clip one after the
/// other, held on its last frame, under the victory panel.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_the_victory_pose() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    // Attack with everyone who is ready until the MPs are down.
    for _ in 0..4000 {
        if fight.server.state.outcome().is_some() {
            break;
        }
        fight.now += 0.03;
        for id in fight.server.state.ready_actors() {
            let msg = fight.server.attack(id);
            fight.say(msg);
        }
        let step = fight.server.step(30);
        fight.say(step);
    }
    assert_eq!(fight.server.state.outcome(), Some(fmv_ff7_battle::rules::Outcome::Victory), "the party wins");
    let won = fight.now;
    // Everyone who stands plays slot 2 after the last blow, held on its last frame, in turn.
    let last = fight.scene.director.last_impact();
    let first_pose = last + 0.45;
    let standing: Vec<usize> = (0..3).filter(|&i| !fight.scene.director.is_dying_or_dead(i, won + 10.0)).collect();
    assert!(!standing.is_empty(), "somebody stands");
    for (k, &i) in standing.iter().enumerate() {
        let at = first_pose + 0.18 * k as f64 + 0.4;
        assert_eq!(fight.scene.director.motion(i, at).clip.map(|c| c.slot), Some(2), "member {i} rejoices");
    }
    // The poses themselves, before the server's end of the battle puts its panel over the middle of the picture.
    fight.sheet("victory-pose", first_pose, &[-0.4, 0.3, 0.8, 1.4, 2.0, 2.8, 4.0, 5.5], 2);
    // The server frees us: the end of the battle with our reward, and the panel over the poses.
    let rewards = fmv_protocol::split_rewards(fight.server.state.rewards(), &[(ME, true)]);
    let ended = fight.server.id;
    fight.now = first_pose + 5.5;
    fight.say(Some(ServerMsg::BattleEnd { battle_id: ended, outcome: fmv_ff7_battle::rules::Outcome::Victory, rewards }));
    let under_the_panel = fight.now;
    fight.sheet("victory-pose-under-the-panel", under_the_panel, &[0.2, 1.6], 2);
    let held = fight.scene.director.motion(standing[0], first_pose + 30.0).clip.expect("holding the pose");
    assert_eq!(held.slot, 2);
    assert!(held.frame > 20.0, "held on the last frame of a long clip: {held:?}");
}

// ----- protocol v7: limit breaks and escape ----------------------------------------------------------

/// A limit break: Cloud's gauge is full and he gives his limit (Braver) to the first MP. He gathers it with the
/// cast clips under a glow, and it breaks over the target at the cap of the impact delay, hard: the bloom of a
/// spell and the gold star of a critical hit in the same instant, with the number landing then.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_a_limit_break() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::with_limits(&real, 301, SEED, [255, 0, 0]) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let target = fight.server.first_enemy().expect("an enemy");
    assert!(fight.server.state.limit_ready(ActorId(0)), "Cloud starts with a full gauge");
    let blow = fight.server.command(ActorId(0), Command::Limit { target: Target::One(target) });
    assert!(blow.is_some(), "a full gauge gives the limit");
    fight.say(blow);
    assert!(fight.server.state.limit_gauge(ActorId(0)) < 255, "the limit spent the gauge");
    // The glow gathers from the arrival to the cap of the delay, and the burst pair breaks on the target then.
    let impact = arrival + cue::MAX_IMPACT_DELAY;
    let charges: Vec<_> = fight.scene.director.fx().iter().filter(|f| f.kind == effects::Kind::Charge).copied().collect();
    assert_eq!((charges.len(), charges[0].target), (1, 0));
    assert!((charges[0].land - impact).abs() < 1e-9, "{charges:?}");
    let at_target: Vec<_> = fight.scene.director.fx().iter().filter(|f| f.kind != effects::Kind::Charge).copied().collect();
    assert!(at_target.len() >= 2 && at_target.iter().all(|f| (f.land - impact).abs() < 1e-9 && f.target == usize::from(target.0)), "{at_target:?}");
    assert!(at_target.iter().any(|f| f.kind == effects::Kind::Flash) && at_target.iter().any(|f| matches!(f.kind, effects::Kind::Strike { critical: true, .. })), "a strike as hard as a critical hit and a flash: {at_target:?}");
    fight.sheet("limit-break-braver", arrival, &[0.05, 0.25, 0.45, 0.58, 0.64, 0.75, 0.95, 1.4], 2);
}

/// A failed escape (Cloud tries to run and is thrown back a little, the others stay) and a successful one (every
/// member turns its back, runs off with the run clip and leaves the stage; no victory pose follows).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_a_failed_escape_and_a_successful_one() {
    let Some(real) = open_real() else { return };
    // The failure: the events a server sends when the Run Checks fail (the picture needs no engine for it).
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let failed = [BattleEvent::Action { actor: ActorId(0), action: ActionRef::Escape }, BattleEvent::EscapeFailed { actor: ActorId(0) }];
    fight.scene.update(arrival, &failed, fight.client.engine());
    let thrown = fight.scene.director.motion(0, arrival + 0.17).offset;
    assert!(thrown.iter().any(|c| c.abs() > 50.0), "thrown back: {thrown:?}");
    assert!(fight.scene.director.motion(1, arrival + 0.17).offset.iter().all(|c| c.abs() < 1e-3), "the others stay");
    fight.sheet("escape-failed", arrival, &[0.05, 0.17, 0.3, 0.45, 0.6, 1.0], 2);

    // The success: this fight started with a preemptive strike, a sure escape.
    let Some(mut fight) = Fight::new(&real, 301, SEED + 1) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let run = fight.server.command(ActorId(0), Command::Escape);
    assert!(run.is_some(), "the engine takes the command");
    fight.say(run);
    assert_eq!(fight.server.state.outcome(), Some(fmv_ff7_battle::rules::Outcome::Escaped), "a preemptive start gets away");
    let ended = fight.server.id;
    fight.say(Some(ServerMsg::BattleEnd { battle_id: ended, outcome: fmv_ff7_battle::rules::Outcome::Escaped, rewards: vec![] }));
    for i in 0..3 {
        let at = arrival + 0.12 * i as f64 + 0.6;
        let running = fight.scene.director.motion(i, at);
        assert_eq!(running.clip.map(|c| c.slot), Some(clips::party::RUN), "member {i} runs");
        assert!((running.turn - std::f32::consts::PI).abs() < 1e-6, "with its back to the fight");
        assert!(fight.scene.director.motion(i, arrival + 3.0).gone, "member {i} is off the stage");
    }
    for foe in 3..5 {
        assert!(!fight.scene.director.motion(foe, arrival + 3.0).gone, "the enemies stay");
    }
    fight.sheet("escape-run-off", arrival, &[0.0, 0.3, 0.6, 0.9, 1.2, 1.6, 2.0, 3.0], 2);
}

// ----- enemy attacks and the guard, performed on cue ----------------------------------------------------------

/// Performs `events` in a fresh fight at its clock and draws the `performers` over `offsets` seconds: for each instant, the
/// whole fight (`<name>-wide.png`, at the instants of `wide`) and a close-up of each performer (`<name>-close-<n>.png`, four
/// to a row, labelled with the instant and the clip and frame the director plays), and returns what each performer played
/// at each instant (printed too: the evidence of the pictures).
fn perform_and_draw(fight: &mut Fight, name: &str, events: &[BattleEvent], performers: &[usize], offsets: &[f64], wide: &[f64]) -> Vec<Vec<String>> {
    let arrival = fight.now;
    fight.scene.update(arrival, events, fight.client.engine());
    let mut played: Vec<Vec<String>> = vec![Vec::new(); performers.len()];
    let mut closes: Vec<Vec<Vec<u8>>> = vec![Vec::new(); performers.len()];
    let mut wides: Vec<Vec<u8>> = Vec::new();
    for &o in offsets {
        let t = arrival + o;
        for (k, &i) in performers.iter().enumerate() {
            let what = fight.scene.director.motion(i, t).clip.map_or("idle".to_string(), |c| format!("s{} f{:.1}", c.slot, c.frame));
            played[k].push(format!("+{o:.2} {what}"));
            closes[k].push(fight.close_up(t, i, &format!("+{o:.2}s {what}"), (2 * W, 2 * H), (CLOSE_W, CLOSE_H)));
        }
        if wide.iter().any(|w| (w - o).abs() < 1e-9) {
            wides.push(fight.frame(t, &format!("+{o:.2} s")));
        }
    }
    for (k, tiles) in closes.iter().enumerate() {
        let (zw, zh, sheet) = tile_sheet(tiles, CLOSE_W, CLOSE_H, 4);
        save(&format!("{name}-close-{k}"), zw, zh, &sheet);
        eprintln!("{name} performer {}: {}", performers[k], played[k].join(", "));
    }
    let (sw, sh, sheet) = contact_sheet(&wides, W, H, 2);
    save(&format!("{name}-wide"), sw, sh, &sheet);
    played
}

/// The enemy attacks of the evidence set (the MP's Machine Gun and Tonfa, the Guard Hound's Bite and Tentacle, the Mono
/// Drive's Drilldrive and its Fire), each performed on cue in a real formation at the party member Cloud: pictures of what
/// the scene plays for them, the whole fight and a close-up: `enemy-attack-<name>-wide.png` and `-close-0.png`.
/// `FMV_SHEET_ATTACKS` (picture names, comma separated) picks some of them.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_enemy_attack_sheets() {
    let Some(real) = open_real() else { return };
    // (picture name, formation, enemy id, attack id): formation 301 is two MPs, 302 a Guard Hound, 305 two Mono Drives.
    let cases: [(&str, u16, u16, u16); 6] = [
        ("mp-machine-gun", 301, 16, 272),
        ("mp-tonfa", 301, 16, 273),
        ("guard-hound-bite", 302, 17, 274),
        ("guard-hound-tentacle", 302, 17, 275),
        ("mono-drive-drilldrive", 305, 18, 276),
        ("mono-drive-fire", 305, 18, 27),
    ];
    let wanted: Option<Vec<String>> = std::env::var("FMV_SHEET_ATTACKS").ok().map(|s| s.split(',').map(|p| p.trim().to_string()).collect());
    for (name, formation, enemy, attack) in cases {
        if wanted.as_ref().is_some_and(|w| !w.iter().any(|p| p == name)) {
            continue;
        }
        let Some(mut fight) = Fight::new(&real, formation, SEED) else { return };
        let placement = real.rules.formation_scene(formation).expect("the formation has placement data");
        let at = placement.enemies.iter().position(|e| e.enemy_id == enemy).expect("the enemy is in the formation");
        let performer = 3 + at;
        fight.now = 2.0;
        let events = [
            BattleEvent::Action { actor: ActorId(performer as u8), action: ActionRef::EnemyAttack(attack) },
            BattleEvent::Damage { actor: ActorId(performer as u8), target: ActorId(0), amount: 25, critical: false },
        ];
        let offsets: Vec<f64> = (0..16).map(|k| f64::from(k) * 0.12).collect();
        perform_and_draw(&mut fight, &format!("enemy-attack-{name}"), &events, &[performer], &offsets, &[0.0, 0.12, 0.24, 0.36, 0.48, 0.6, 0.96, 1.44]);
    }
}

/// An enemy that is beaten: the Guard Hound's death script plays a clip (slot 14: it falls), which it plays from the start of its
/// death before it fades; the MP's plays none (it flinches, then fades). The party attacks (real engine) until the first enemy
/// is down (`enemy-death-<name>-close-0.png`, `-wide.png`, from the batch that brings the death).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_enemy_death_sheets() {
    let Some(real) = open_real() else { return };
    // (picture name, formation, enemy id, the clip its death plays)
    for (name, formation, enemy, death) in [("guard-hound", 302u16, 17u16, Some(14u16)), ("mp", 301, 16, None)] {
        let Some(mut fight) = Fight::new(&real, formation, SEED) else { return };
        let placement = real.rules.formation_scene(formation).expect("the formation has placement data");
        let victim = 3 + placement.enemies.iter().position(|e| e.enemy_id == enemy).expect("the enemy is in the formation");
        fight.now = 2.0;
        let first = fight.server.step(1);
        fight.say(first);
        for _ in 0..4000 {
            if !fight.server.state.actor(ActorId(victim as u8)).is_some_and(|c| c.is_alive()) {
                break;
            }
            fight.now += 0.03;
            for id in fight.server.state.ready_actors() {
                let msg = fight.server.attack(id);
                fight.say(msg);
            }
            let step = fight.server.step(30);
            fight.say(step);
        }
        assert!(!fight.server.state.actor(ActorId(victim as u8)).is_some_and(|c| c.is_alive()), "{name} is beaten");
        // `perform_and_draw` draws from the instant of the batch that brought the death (the clock now).
        let offsets: Vec<f64> = (0..16).map(|k| 0.4 + f64::from(k) * 0.12).collect();
        let played = perform_and_draw(&mut fight, &format!("enemy-death-{name}"), &[], &[victim], &offsets, &[0.4, 0.64, 0.88, 1.12, 1.36, 1.6, 1.84, 2.2]);
        let slots: std::collections::BTreeSet<u16> = played[0].iter().filter_map(|s| s.split(" s").nth(1)).filter_map(|s| s.split(' ').next()?.parse().ok()).collect();
        eprintln!("{name}: the clips played: {slots:?}");
        assert_eq!(slots.contains(&14), death == Some(14), "{name}: {played:?}");
        assert!(slots.contains(&1), "{name} flinches first (its hurt clip): {played:?}");
        // Gone in the end, whatever it played.
        assert!(fight.scene.director.motion(victim, fight.now + 3.0).gone, "{name}");
    }
}

/// Defend, as the scene plays it: Cloud, Tifa and Barret guard at the same instant (`defend-close-<n>.png`, one picture
/// each, and the whole fight in `defend-wide.png`).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_defend_sheets() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let events: Vec<BattleEvent> = (0..3u8).map(|i| BattleEvent::Action { actor: ActorId(i), action: ActionRef::Defend }).collect();
    let offsets: Vec<f64> = (0..16).map(|k| f64::from(k) * 0.1).collect();
    perform_and_draw(&mut fight, "defend", &events, &[0, 1, 2], &offsets, &[0.0, 0.2, 0.4, 0.6, 0.8, 1.2, 1.6, 2.0]);
}
