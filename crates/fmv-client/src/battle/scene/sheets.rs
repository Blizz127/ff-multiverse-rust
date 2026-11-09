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
//! * `party_slot_evidence_sheets` draws, for each of the nine playable characters, the slots of the shared
//!   table of [`super::clips`] frame by frame (`party-clips-<name>.png`: one row per slot, the slot and the
//!   frame written on each picture), and `enemy_slot_evidence_sheets` the MP and the Guard Hound.
//! * `party_clips_agree_across_the_characters` and `enemy_book_is_the_same_in_every_scene` check the
//!   numbers behind the table without a GPU.
//! * `fight_*` run real fights (a real engine, the client's own replay, the scene, the overlay) and write
//!   the sheets of the presentation: an MP's attack chain, a party attack with the number at its impact, a
//!   cast with its effect, the elements, a beaten enemy's fade and the victory pose.

// `chunks_exact(4)` over RGBA bytes would be `as_chunks` on Rust 1.88+; the workspace keeps its MSRV lower
// (see `avatar`), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::camera::{BattleCamera, View};
use super::clips::{ClipSet, Kit};
use super::effects::Element;
use super::shots::{contact_sheet, member, open_real, Real, Server, ME, H, W};
use super::*;
use crate::avatar::ground::{circle, GroundShape};
use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use crate::battle::{draw_battle_overlay, BattleClient};
use crate::ui::draw::DrawList;
use crate::ui::testkit::{pixel, save, Offscreen};
use fmv_ff7::battle_model::{BattleCharacter, BattleModel, BattleModels};
use fmv_ff7::model::mat4_translate;
use fmv_ff7::playable::Character;
use fmv_ff7_battle::rules::Command;
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

fn clip_set(model: &BattleModel) -> ClipSet {
    ClipSet {
        frames: model.animations.iter().map(|c| c.frame_count() as u16).collect(),
        ok: model.slot_ok.clone(),
        travel: model.animations.iter().map(|c| c.frames().last().map_or(0.0, |l| l.root_translation[2]) - c.frames().first().map_or(0.0, |f| f.root_translation[2])).collect(),
        fps: 15.0,
    }
}

// ---------------------------------------------------------------------------------------------
// The numbers behind the table (no GPU)
// ---------------------------------------------------------------------------------------------

/// All nine characters' packs give the shared table every role, and each death clip ends in the pose of the
/// dead slot: the numbers behind "slot 18 ends where slot 6 is" of [`super::clips`].
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn party_clips_agree_across_the_characters() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    for which in BattleCharacter::ALL {
        let model = models.load_character(which).expect("the character loads");
        let kit = Kit::party(clip_set(&model));
        let name = which.display_name();
        eprintln!(
            "{name:20} slots {:2}, weapon tracks {:2}: attack {:?} item {:?} cast {:?} hurt {:?} death {:?} dead {:?} victory {:?} defend {:?} run {:?} back {:?}",
            model.animations.len(),
            model.weapon_tracks.len(),
            kit.attack,
            kit.item,
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
        assert!(kit.attack.is_some() && kit.hurt.is_some() && kit.death.is_some() && kit.dead.is_some(), "{name}");
        assert!(kit.victory.is_some() && kit.defend.is_some() && kit.item.is_some() && kit.cast.iter().all(Option::is_some), "{name}");
        // The weapon rides on a track of its own for each slot played: every slot the kit uses has a real one (a
        // slot with a stand-in track would draw the weapon at some other slot's place).
        for slot in [0usize, 2, 4, 6, 7, 8, 9, 10, 12, 13, 14, 15, 18] {
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
    assert_eq!(book.attacks_of(16), [(272, 3), (273, 4)], "the MP: Machine Gun plays slot 3, Tonfa slot 4");
    assert_eq!(book.attacks_of(17), [(274, 3), (275, 4)], "the Guard Hound: Bite and Tentacle");
    assert!(book.attacks_of(9999).is_empty());
    // The kits that come of it: the MP runs up with slot 2 and comes back with slot 5.
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let mp = models.load_enemy(16).expect("the MP");
    // The baton rides on a real track of its own in every slot the MP plays.
    for slot in 0..=5usize {
        assert!(mp.weapon_track_ok.get(slot).copied().unwrap_or(false), "the MP's baton track for slot {slot}");
    }
    let kit = Kit::enemy(clip_set(&mp), &book.attacks_of(16));
    eprintln!("the MP: hurt {:?}, run {:?}, back {:?}, attacks {:?}", kit.hurt, kit.run, kit.back, kit.attacks);
    assert_eq!((kit.hurt, kit.run, kit.back), (Some(1), Some(2), Some(5)));
    assert_eq!(kit.attacks, [(272, 3), (273, 4)]);
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
            models: AvatarFrame { models, tints: Vec::new(), zoom: view.zoom as f32, overlay_bias: 3.0, ground, scenery: None },
            tints: Vec::new(),
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
/// role (the attack at its impact frame, the cast's hold, the item gesture, a hurt flinch, a fall, the end of the
/// victory): the one picture that says the shared table of [`super::clips`] holds for everybody.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn party_roles_overview_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let mut rows: Vec<Vec<Vec<u8>>> = Vec::new();
    for which in BattleCharacter::ALL.into_iter().filter(|c| *c != BattleCharacter::Sephiroth) {
        let model = models.load_character(which).expect("the character loads");
        let kit = Kit::party(clip_set(&model));
        let standing = model.pose(0, 0).and_then(|p| model.model.posed_bounds(&p, &fmv_ff7::model::mat4_identity())).map_or(900.0, |(lo, _)| f64::from(-lo[1]));
        let rig = Rig::new(model, standing);
        let name = which.display_name().to_string();
        let roles = [
            ("attack", kit.attack, party_impact()),
            ("cast hold", kit.cast[1], 0.5),
            ("item", kit.item, 0.5),
            ("hurt", kit.hurt, 0.4),
            ("death", kit.death, 0.35),
            ("victory", kit.victory, 1.0),
        ];
        let tiles = roles
            .iter()
            .map(|&(role, slot, at)| {
                let slot = usize::from(slot.expect("the role has a slot"));
                let frames = rig.model.animations[slot].frame_count();
                let frame = (at * (frames - 1) as f32).round() as usize;
                rig.tile(&mut gpu, slot, frame, &format!("{role} s{slot} f{frame}/{frames}\n{name}"))
            })
            .collect();
        rows.push(tiles);
    }
    for (half, chunk) in rows.chunks(5).enumerate() {
        let tiles: Vec<Vec<u8>> = chunk.iter().flatten().cloned().collect();
        let (w, h, sheet) = grid(&tiles, 6);
        save(&format!("party-roles-overview-{half}"), w, h, &sheet);
    }
}

fn party_impact() -> f32 {
    super::clips::party::ATTACK_IMPACT
}

/// The slots the choreography plays, as rows, for every one of the nine characters: the evidence for the
/// table of [`super::clips`]. One PNG per character.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or a software Vulkan/GL driver"]
fn party_slot_evidence_sheets() {
    let (Some(path), Some(mut gpu)) = (battle_lgp(), Offscreen::new()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    // idle, victory, run, defend, item, attack, cast (begin, hold, end), hurt, death, dead.
    let rows = [0usize, 2, 7, 8, 9, 10, 12, 13, 14, 15, 18, 6];
    for which in BattleCharacter::ALL {
        let model = models.load_character(which).expect("the character loads");
        let used: Vec<usize> = rows.iter().copied().filter(|&s| s < model.animations.len() && model.slot_ok[s]).collect();
        let rig = Rig::new(model, 900.0);
        for (half, chunk) in used.chunks(6).enumerate() {
            let (w, h, sheet) = rig.sheet(&mut gpu, chunk, 6);
            save(&format!("party-clips-{}-{half}", which.display_name().split(' ').next().unwrap_or("x").to_ascii_lowercase()), w, h, &sheet);
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
    /// Formation `formation` (301: two MPs) against Cloud, Tifa and Barret at level 12, the party ready (a
    /// preemptive start), the engine seeded with `seed`.
    fn new(real: &Real, formation: u16, seed: u64) -> Option<Self> {
        Self::with_limits(real, formation, seed, [0; 3])
    }

    /// The same fight with the party starting with the limit gauges `limits` (Cloud, Tifa, Barret; 255 is
    /// full: protocol v7).
    fn with_limits(real: &Real, formation: u16, seed: u64, limits: [u16; 3]) -> Option<Self> {
        let gpu = Offscreen::new()?;
        let limited = |i: u64, character: u8| BattleMember { limit: limits[i as usize], ..member(i, character) };
        let setup = BattleSetup {
            formation_id: formation,
            seed,
            party: vec![limited(0, 0), limited(1, 2), limited(2, 1)],
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
        Some(fight)
    }

    /// The server says `msg` at the fight's clock: the client mirrors it and the scene is told.
    fn say(&mut self, msg: Option<ServerMsg>) {
        if let Some(msg) = msg {
            self.client.on_message(self.now, msg);
        }
        let events = client_events(&mut self.client);
        self.scene.update(self.now, &events, self.client.engine());
    }

    /// The fight at time `t` (never before an earlier call): the scene brought up to `t`, drawn with the
    /// overlay as the window draws it, `label` written in the corner.
    fn frame(&mut self, t: f64, label: &str) -> Vec<u8> {
        self.scene.update(t, &[], self.client.engine());
        let view = self.client.view(t).expect("a view");
        let scene_view = self.scene.view(W, H);
        let mut overlay = DrawList::new();
        draw_battle_overlay(&view, &self.scene.head_anchors(&scene_view), W, H, &mut overlay);
        overlay.draw_text_shadowed(8, 24, 2, [255, 255, 255, 255], [0, 0, 0, 255], label);
        self.gpu.render_battle(W, H, &self.scene.frame(&scene_view, false), &overlay)
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

/// The MP's attack chain: the run-up (slot 2), the attack it names (Machine Gun, slot 3; Tonfa, slot 4), the
/// way back (slot 5), with the hurt clip of whoever it hits and the number at the impact.
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
        assert_eq!(slots.first(), Some(&2), "it runs up with slot 2");
        assert_eq!(slots.get(1), Some(&if attack == 272 { 3 } else { 4 }), "and plays the clip scene.bin names for the attack");
        assert_eq!(slots.last(), Some(&5), "and comes back with slot 5");
    }
    fight.sheet("mp-attack-chain-machine-gun-and-tonfa", arrival, &[0.05, 0.2, 0.35, 0.55, 0.7, 0.9, 1.2, 1.7], 2);
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
    // The scene: Cloud swings (slot 10) up to its impact frame at the impact; the MP's hurt clip (slot 1) starts then.
    let swing = fight.scene.director.motion(0, impact - 0.001).clip.expect("Cloud swings");
    assert_eq!(swing.slot, 10, "{swing:?}");
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
    let msg = fight.server.command(ActorId(0), Command::Magic { spell: 27, target });
    assert!(msg.is_some(), "the engine takes the spell");
    fight.say(msg);
    let impact = arrival + cue::impact_delay(Some(ActionRef::Magic(27)));
    assert!(fight.scene.director.charges().iter().any(|c| c.element == Element::Fire), "the caster gathers a fire glow");
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
        let Some(msg) = fight.server.command(ActorId(0), Command::Magic { spell, target }) else {
            eprintln!("the engine does not take spell {spell}: skipped");
            continue;
        };
        fight.say(Some(msg));
        let burst = fight.scene.director.bursts().first().copied();
        eprintln!("spell {spell}: burst {burst:?}");
        shots.push(fight.frame(arrival + cue::impact_delay(Some(ActionRef::Magic(spell))) + 0.12, &format!("spell {spell}")));
    }
    if shots.len() == 4 {
        let (sw, sh, sheet) = contact_sheet(&shots, W, H, 2);
        save("cast-elements-fire-ice-bolt-cure", sw, sh, &sheet);
    }
}

/// An item and a guard: Cloud uses a Potion on Tifa (the item gesture, slot 9, and a sparkle on her at the
/// impact) while Barret holds his guard (slot 8).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or a software Vulkan/GL driver"]
fn fight_item_and_defend() {
    let Some(real) = open_real() else { return };
    let Some(mut fight) = Fight::new(&real, 301, SEED) else { return };
    fight.now = 2.0;
    let first = fight.server.step(1);
    fight.say(first);
    let arrival = fight.now;
    let potion = fight.server.command(ActorId(0), Command::Item { item: 0, target: ActorId(1) });
    assert!(potion.is_some(), "the engine takes a Potion");
    fight.say(potion);
    let guard = fight.server.command(ActorId(2), Command::Defend);
    assert!(guard.is_some(), "and Barret's guard");
    fight.say(guard);
    assert_eq!(fight.scene.director.motion(0, arrival + 0.2).clip.map(|c| c.slot), Some(9), "Cloud's item gesture");
    assert_eq!(fight.scene.director.motion(2, arrival + 0.3).clip.map(|c| c.slot), Some(8), "Barret guards");
    assert!(fight.scene.director.bursts().iter().any(|b| b.kind == effects::BurstKind::Item && b.target == 1), "a sparkle on Tifa");
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
    let blow = fight.server.command(ActorId(0), Command::Limit { target });
    assert!(blow.is_some(), "a full gauge gives the limit");
    fight.say(blow);
    assert!(fight.server.state.limit_gauge(ActorId(0)) < 255, "the limit spent the gauge");
    // The glow gathers from the arrival to the cap of the delay, and the burst pair breaks on the target then.
    let impact = arrival + cue::MAX_IMPACT_DELAY;
    let charges = fight.scene.director.charges().to_vec();
    assert_eq!((charges.len(), charges[0].caster), (1, 0));
    assert!((charges[0].end - impact).abs() < 1e-9, "{charges:?}");
    let bursts = fight.scene.director.bursts().to_vec();
    assert!(bursts.len() >= 2 && bursts.iter().all(|b| (b.at - impact).abs() < 1e-9 && b.target == usize::from(target.0)), "{bursts:?}");
    assert!(bursts.iter().any(|b| b.kind == effects::BurstKind::Spell) && bursts.iter().any(|b| matches!(b.kind, effects::BurstKind::Hit { critical: true })));
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
