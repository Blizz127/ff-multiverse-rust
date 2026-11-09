//! Offscreen checks and screenshots of the battle scene, drawn through the real renderer
//! ([`crate::render::Gpu::draw_battle`], the avatar pipelines, the UI overlay) into a texture, read
//! back and looked at.
//!
//! They need a GPU or a software Vulkan/GL driver (lavapipe/llvmpipe on a machine without a
//! display), so they only run on request: `cargo test -p fmv-client -- --ignored battle::scene::shots`.
//!
//! * The *synthetic* test draws stick-figure stand-ins and boxes (no game data) and checks where
//!   everybody lands.
//! * The *real-data* test builds the debug formation 301 from the owner's OWN `scene.bin`,
//!   `kernel.bin` and `char.lgp` (read-only): a real [`BattleClient`] replays a real engine's
//!   messages, so the placement, the camera and the reactions come through the code the window
//!   runs. It writes PNGs to `$FMV_SHOT_DIR` (`~/.cache/claude-tmp/fmv-client-shots` without it).
//!   It needs `FMV_FF7_DATA` (the game's `data/` directory) or `FMV_FF7_LIBRARY` (the state directory
//!   of the imported library); `FMV_BATTLE_FORMATION` picks another formation (default 301).
//!
//! The pictures of the presentation (the real clips, the numbers at the impact, the effects, the
//! fade, the victory pose) and the evidence for which slot is which are in [`super::sheets`]; its
//! fights use the helpers here ([`Real`], [`Server`], [`contact_sheet`]). **GPU tests must run one at a
//! time** (`--test-threads=1`): several lavapipe devices in one process crash it.

// `chunks_exact(4)` over RGBA bytes would be `as_chunks` on Rust 1.88+; the workspace keeps its MSRV
// lower (see `avatar`), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::*;
use crate::avatar::library::tests::Counting;
use crate::avatar::source::LgpLoader;
use crate::battle::scene::model::{ArchiveModels, BattleActorModel, BattleModelSource, EnemyLook, PartyLook, StandInModels};
use crate::battle::view::BAND_VERIFIED_RGB;
use crate::battle::{draw_battle_overlay, BattleClient};
use crate::ui::draw::DrawList;
use crate::ui::testkit::{pixel, save, Offscreen};
use fmv_core::{AccountId, EntityId};
use fmv_ff7_battle::rules::Command;
use fmv_protocol::{AppliedCommand, BattleRules, BattleSetup, Ff7BattleData, ServerMsg};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub(super) const W: u32 = 1100;
pub(super) const H: u32 = 680;
pub(super) const ME: AccountId = AccountId(1);

/// A frame as the window draws it: the scene, then the battle overlay over the fighters' heads.
pub(super) fn draw(gpu: &mut Offscreen, scene: &BattleScene, battle: &BattleView, w: u32, h: u32) -> Vec<u8> {
    let view = scene.view(w, h);
    let mut overlay = DrawList::new();
    draw_battle_overlay(battle, &scene.head_anchors(&view), w, h, &mut overlay);
    gpu.render_battle(w, h, &scene.frame(&view, false), &overlay)
}

/// The stage alone (no models, no marks) for the same window: what a pixel is when nobody stands on it.
fn stage_only(gpu: &mut Offscreen, scene: &BattleScene, w: u32, h: u32) -> Vec<u8> {
    let view = scene.view(w, h);
    let mut frame = scene.frame(&view, false);
    frame.models.models.clear();
    frame.models.ground.clear();
    gpu.render_battle(w, h, &frame, &DrawList::new())
}

fn differs(a: [u8; 3], b: [u8; 3], by: u8) -> bool {
    a.iter().zip(b).any(|(x, y)| x.abs_diff(y) > by)
}

/// How many pixels of two equally sized images differ by more than `by` in some channel.
fn changed_pixels(a: &[u8], b: &[u8], by: u8) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| differs([p[0], p[1], p[2]], [q[0], q[1], q[2]], by)).count()
}

/// Is somebody drawn at `anchor`? Somewhere on the line from the feet to the head the picture is not
/// what the stage alone shows (a thin figure seen edge-on is only a few pixels wide).
fn drawn_at(shot: &[u8], bare: &[u8], w: u32, a: &ActorScreen) -> bool {
    let (Some(feet), Some(head)) = (a.feet, a.head) else { return false };
    let h = (shot.len() / 4) as u32 / w;
    (1..20).any(|k| {
        let t = k as f32 / 20.0;
        let (x, y) = (feet[0] + (head[0] - feet[0]) * t, feet[1] + (head[1] - feet[1]) * t);
        (-4i32..=4).any(|dx| {
            let (px, py) = ((x as i32 + dx).clamp(0, w as i32 - 1) as u32, (y as i32).clamp(0, h as i32 - 1) as u32);
            differs(pixel(shot, w, px, py), pixel(bare, w, px, py), 12)
        })
    })
}

/// `frames` (each `w` x `h`) halved and laid out `columns` to a row: one picture of a sequence.
pub(super) fn contact_sheet(frames: &[Vec<u8>], w: u32, h: u32, columns: usize) -> (u32, u32, Vec<u8>) {
    let (cw, ch) = (w / 2, h / 2);
    let rows = frames.len().div_ceil(columns);
    let (sw, sh) = (cw * columns as u32, ch * rows as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (n, frame) in frames.iter().enumerate() {
        let (ox, oy) = ((n % columns) as u32 * cw, (n / columns) as u32 * ch);
        for y in 0..ch {
            for x in 0..cw {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let at = (((y * 2 + dy) * w + x * 2 + dx) * 4) as usize;
                    for k in 0..4 {
                        sum[k] += u32::from(frame[at + k]);
                    }
                }
                let to = (((oy + y) * sw + ox + x) * 4) as usize;
                for k in 0..4 {
                    sheet[to + k] = (sum[k] / 4) as u8;
                }
            }
        }
    }
    (sw, sh, sheet)
}

/// Hand the client a message (if any) and the scene what it did, as the window does each frame.
pub(super) fn feed(client: &mut BattleClient, scene: &mut BattleScene, msg: Option<ServerMsg>, now: f64) {
    if let Some(msg) = msg {
        client.on_message(now, msg);
    }
    scene.update(now, &client.take_events(), client.engine());
}

/// What the rules' server would say: one engine, its messages in the order a server sends them.
pub(super) struct Server {
    pub(super) id: BattleId,
    pub(super) state: BattleState,
}

impl Server {
    pub(super) fn start(&self, setup: &BattleSetup, fingerprint: fmv_core::Fingerprint) -> ServerMsg {
        ServerMsg::EncounterStart {
            battle_id: self.id,
            formation_id: setup.formation_id,
            seed: setup.seed,
            party: setup.party.clone(),
            preemptive: setup.preemptive,
            back_attack: setup.back_attack,
            battle_speed: setup.battle_speed,
            ticks_per_second: setup.ticks_per_second,
            data_fingerprint: fingerprint,
        }
    }

    pub(super) fn step(&mut self, ticks: u64) -> Option<ServerMsg> {
        let events = self.state.step_ticks(ticks);
        (!events.is_empty()).then(|| ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: None })
    }

    /// `actor` attacks the first living enemy.
    pub(super) fn attack(&mut self, actor: ActorId) -> Option<ServerMsg> {
        let target = self.first_enemy()?;
        self.command(actor, Command::Attack { target })
    }

    /// The first enemy that is alive.
    pub(super) fn first_enemy(&self) -> Option<ActorId> {
        self.state.enemy_ids().into_iter().find(|e| self.state.actor(*e).is_some_and(Combatant::is_alive))
    }

    /// `actor`'s command, if the engine takes it, as the server's message.
    pub(super) fn command(&mut self, actor: ActorId, command: Command) -> Option<ServerMsg> {
        let events = self.state.submit_command(actor, command).ok()?;
        Some(ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: Some(AppliedCommand { actor, command }) })
    }
}

pub(super) fn member(i: u64, character: u8) -> BattleMember {
    BattleMember { entity: EntityId(10 + i), account: if i == 0 { ME } else { AccountId(10 + i) }, name: format!("P{i}"), character, level: 12, weapon: None, armor: None, limit: 0 }
}

// ---------------------------------------------------------------------------------------------
// No game data
// ---------------------------------------------------------------------------------------------

/// The synthetic battle: a party of three against the two dummies of the synthetic rules' formation 1,
/// with the formation's own placement and a camera to the side (made-up numbers), at `location`.
fn synthetic_scene(kit: &mut ModelKit, location: u16) -> BattleScene {
    let setup = BattleSetup {
        formation_id: 1,
        seed: 3,
        party: vec![member(0, 0), member(1, 2), member(2, 1)],
        preemptive: true,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
    };
    let engine = fmv_protocol::SyntheticRules::default().build(&setup).unwrap();
    let formation = FormationScene {
        location,
        layout: fmv_ff7_battle::scene::BattleLayout::Normal,
        camera: fmv_protocol::battle::CameraPose { position: [9000, -2400, 1800], target: [0, -300, 100] },
        enemies: vec![
            fmv_protocol::battle::EnemyPlacement { enemy_id: 1, position: [-650, 0, -1450], row: 1 },
            fmv_protocol::battle::EnemyPlacement { enemy_id: 2, position: [600, 0, -2100], row: 1 },
        ],
    };
    let start = SceneStart { id: BattleId(1), party: setup.party.clone(), my_actor: Some(ActorId(0)), formation: Some(formation) };
    let mut scene = BattleScene::new(&start, Some(&engine), kit).expect("a scene");
    scene.update(1.0, &[], Some(&engine));
    scene
}

fn stand_ins() -> ModelKit {
    ModelKit::new(Box::new(StandInModels), Some(Box::new(Counting::new(Rc::new(RefCell::new(Vec::new())), vec![]))))
}

fn verified_view() -> BattleView {
    let mut battle = BattleView::unverified(BattleId(1));
    battle.status = Status::Verified;
    battle
}

/// A source with a location: a big flat slab for the floor (the stand-ins for everything else), to
/// run the location's own pass through the real renderer.
struct Slab;

const SLAB_RGB: [u8; 3] = [60, 130, 70];

impl BattleModelSource for Slab {
    fn party_member(&mut self, who: &PartyLook) -> BattleActorModel {
        StandInModels.party_member(who)
    }

    fn enemy(&mut self, who: &EnemyLook) -> BattleActorModel {
        StandInModels.enemy(who)
    }

    fn location(&mut self, location: u16) -> Option<BattleActorModel> {
        (location == 5).then_some(BattleActorModel::Box(BoxSpec { width: 40_000.0, depth: 40_000.0, height: 30.0, rgb: SLAB_RGB }))
    }
}

/// Stick-figure party, box enemies, the formation's own placement (made-up numbers) and a camera
/// to the side: the composition of a normal battle, checked on pixels.
#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_synthetic_battle_scene() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let scene = synthetic_scene(&mut stand_ins(), 0);
    let battle = verified_view();
    let shot = draw(&mut gpu, &scene, &battle, W, H);
    save("synthetic-battle", W, H, &shot);
    let bare = stage_only(&mut gpu, &scene, W, H);

    // The status band is across the top.
    for x in [2, W / 2, W - 3] {
        assert_eq!(pixel(&shot, W, x, 4), BAND_VERIFIED_RGB, "the verified band at x = {x}");
    }
    // Everyone is drawn where the scene says they are, enemies left of the party.
    let view = scene.view(W, H);
    let anchors = scene.screen_anchors(&view);
    assert_eq!(anchors.len(), 5);
    for a in &anchors {
        assert!(drawn_at(&shot, &bare, W, a), "somebody is drawn at {a:?}");
    }
    let mean_x = |side: Side| {
        let xs: Vec<f32> = anchors.iter().filter(|a| a.side == side).map(|a| a.feet.unwrap()[0]).collect();
        xs.iter().sum::<f32>() / xs.len() as f32
    };
    assert!(mean_x(Side::Enemy) < W as f32 * 0.5 && mean_x(Side::Party) > W as f32 * 0.5, "enemies left, party right");
    // The ring under our own ready character is on the floor: a gold pixel near its feet.
    let me = anchors[0].feet.unwrap();
    let gold = (-40i32..=40)
        .flat_map(|dx| (-30i32..=30).map(move |dy| (dx, dy)))
        .filter(|&(dx, dy)| {
            let (x, y) = ((me[0] as i32 + dx).clamp(0, W as i32 - 1) as u32, (me[1] as i32 + dy).clamp(0, H as i32 - 1) as u32);
            let p = pixel(&shot, W, x, y);
            p[0] > 180 && p[1] > 140 && p[2] < 120
        })
        .count();
    assert!(gold > 10, "the ready ring is gold: {gold} px near {me:?}");
}

/// The same battle with a location model (a flat slab standing in for one): drawn through the
/// location's own pass, under the actors, whose shadows and rings are then not covered by its floor.
#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_synthetic_battle_with_a_location_model() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut kit = ModelKit::new(Box::new(Slab), Some(Box::new(Counting::new(Rc::new(RefCell::new(Vec::new())), vec![]))));
    let scene = synthetic_scene(&mut kit, 5);
    let battle = verified_view();
    let shot = draw(&mut gpu, &scene, &battle, W, H);
    save("synthetic-battle-location", W, H, &shot);
    let bare = stage_only(&mut gpu, &scene, W, H);
    let view = scene.view(W, H);
    let anchors = scene.screen_anchors(&view);

    // The slab is the floor: a ground pixel low in the window is its (lit) top colour, and where the
    // stand-in floor's tiles would be there is nothing of them.
    let slab_top = [SLAB_RGB[0], SLAB_RGB[1], SLAB_RGB[2]].map(|c| (f32::from(c) * 1.18).round() as u8);
    let floor = pixel(&shot, W, W / 2, H - 150);
    assert!(floor.iter().zip(slab_top).all(|(a, b)| a.abs_diff(b) <= 3), "the floor is the slab: {floor:?} vs {slab_top:?}");
    // The actors are drawn over it...
    for a in &anchors {
        assert!(drawn_at(&shot, &bare, W, a), "{a:?} is drawn over the location");
    }
    // ...with their shadows on it: under an enemy's feet the slab is darker than beside it.
    let enemy = anchors.iter().find(|a| a.side == Side::Enemy).unwrap().feet.unwrap();
    let (under, beside) = (pixel(&shot, W, enemy[0] as u32, enemy[1] as u32 + 3), pixel(&shot, W, (enemy[0] as u32).saturating_sub(300), enemy[1] as u32 + 3));
    assert!(under[1] + 12 < beside[1], "the shadow lies on the location's floor: {under:?} under, {beside:?} beside");
}

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

pub(super) struct Real {
    pub(super) rules: Arc<dyn BattleRules>,
    pub(super) char_lgp: PathBuf,
    /// The battle model archive, when this machine has one (the models of the picture are real then).
    pub(super) battle_lgp: Option<PathBuf>,
    /// `scene.bin`, which names the clips of the enemies' attacks.
    pub(super) scene_bin: Option<PathBuf>,
}

impl Real {
    /// The models of the picture as the window has them: the archive's own battle models, the enemies'
    /// attacks playing the clips `scene.bin` names; stand-ins without an archive.
    pub(super) fn kit(&self) -> ModelKit {
        let source: Box<dyn BattleModelSource> = match &self.battle_lgp {
            Some(path) => {
                let archive = ArchiveModels::open(path).expect("battle.lgp opens");
                let book = self.scene_bin.as_ref().and_then(|p| fmv_ff7_battle::scene::SceneBin::open(p).ok()).map(|s| model::EnemyBook::from_scenes(&s)).unwrap_or_default();
                Box::new(archive.with_enemy_book(book))
            }
            None => Box::new(StandInModels),
        };
        ModelKit::new(source, Some(Box::new(LgpLoader::new(&self.char_lgp))))
    }
}

/// `FMV_FF7_DATA` (the game's `data/`: `lang-en/battle/scene.bin`, `lang-en/kernel/kernel.bin` or the
/// classic layout, `field/char.lgp` and `battle/battle.lgp`), else `FMV_FF7_LIBRARY` (the state
/// directory of the imported library, read-only). `None` (with a note) when neither is set.
pub(super) fn open_real() -> Option<Real> {
    if let Some(data) = std::env::var_os("FMV_FF7_DATA").map(PathBuf::from) {
        let first = |paths: &[&str]| paths.iter().map(|p| data.join(p)).find(|p| p.is_file());
        let scene = first(&["lang-en/battle/scene.bin", "battle/scene.bin"]).expect("scene.bin under FMV_FF7_DATA");
        let kernel = first(&["lang-en/kernel/kernel.bin", "kernel/KERNEL.BIN", "kernel/kernel.bin"]).expect("kernel.bin under FMV_FF7_DATA");
        let char_lgp = first(&["field/char.lgp", "field/CHAR.LGP"]).expect("char.lgp under FMV_FF7_DATA");
        let battle_lgp = first(&["battle/battle.lgp", "battle/BATTLE.LGP"]);
        let rules: Arc<dyn BattleRules> = Arc::new(Ff7BattleData::open(&scene, &kernel).expect("the battle data opens"));
        return Some(Real { rules, char_lgp, battle_lgp, scene_bin: Some(scene) });
    }
    if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        let source = crate::ff7::Ff7Source::Library { state_dir: Some(dir.into()), set: None };
        let (rules, _) = crate::battle::open_rules(&source).expect("the battle data opens");
        let char_lgp = crate::ff7::resolve_char_lgp(&source).expect("looked for").expect("a char.lgp");
        let battle_lgp = crate::ff7::resolve_battle_lgp(&source).expect("looked for");
        let scene_bin = fmv_library::ff7::resolve_battle_files(&source).ok().map(|files| files.scene);
        return Some(Real { rules, char_lgp, battle_lgp, scene_bin });
    }
    eprintln!("skipped: neither FMV_FF7_DATA nor FMV_FF7_LIBRARY is set");
    None
}

/// The path the window takes to its models, with nothing but the environment's source: `open_ff7`
/// resolves `char.lgp` and `battle.lgp` from the same library set (or install) as the field archive and
/// remembers them, `ModelKit::from_local_content` (what `scene_for` makes the window's kit with) opens
/// them. No GPU needed.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY)"]
fn real_window_path_finds_the_battle_archive_through_open_ff7() {
    let source = if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        crate::ff7::Ff7Source::Library { state_dir: Some(dir.into()), set: None }
    } else if let Some(dir) = std::env::var_os("FMV_FF7_DATA") {
        crate::ff7::Ff7Source::Flevel { path: PathBuf::from(dir).join("field").join("flevel.lgp"), labels: crate::ff7::Ff7Labels::default() }
    } else {
        eprintln!("skipped: neither FMV_FF7_DATA nor FMV_FF7_LIBRARY is set");
        return;
    };
    crate::ff7::open_ff7(&source).expect("the FF7 data opens");
    let mut kit = ModelKit::from_local_content();
    let resolve = |kit: &mut ModelKit, model: BattleActorModel| kit.rigs.resolve(&model, BoxSpec::for_party(0));
    // an enemy by its id: the MP of the formation, with its baton
    let mp = kit.source.enemy(&EnemyLook { actor: ActorId(3), enemy_id: Some(16), name: "MP".into(), max_hp: 30, level: 2 });
    let rig = resolve(&mut kit, mp);
    eprintln!("MP: scale {}, height {:.0}, {} attachment(s)", rig.scale, rig.height, rig.attachments.len());
    assert!(rig.scale == 1.0 && rig.attachments.len() == 1 && (700.0..900.0).contains(&rig.height), "the real MP");
    // The clips: slot 1 is its hurt clip whatever the source, and from a library (which has scene.bin) the
    // attacks scene.bin names and the run-up and way back that go with them. A bare flevel.lgp has no scene.bin.
    assert_eq!(rig.kit.hurt, Some(1));
    if matches!(source, crate::ff7::Ff7Source::Library { .. }) {
        eprintln!("MP kit: run {:?}, back {:?}, attacks {:?}", rig.kit.run, rig.kit.back, rig.kit.attacks);
        assert_eq!((rig.kit.run, rig.kit.back), (Some(2), Some(5)));
        assert_eq!(rig.kit.attacks, [(272, 3), (273, 4)], "Machine Gun and Tonfa, as scene.bin names them");
    } else {
        assert!(rig.kit.attacks.is_empty(), "without a library there is no scene.bin to name the attacks");
    }
    // A party member's kit is the shared table.
    let cloud = kit.source.party_member(&PartyLook { actor: ActorId(0), character: 0, mine: true });
    let cloud = resolve(&mut kit, cloud);
    assert_eq!((cloud.kit.attack, cloud.kit.hurt, cloud.kit.death, cloud.kit.victory), (Some(10), Some(15), Some(18), Some(2)));
    // a party member by the combat slot it fights in
    for (character, height) in [(0u8, 862.0), (2, 837.0), (1, 919.0)] {
        let model = kit.source.party_member(&PartyLook { actor: ActorId(0), character, mine: true });
        let rig = resolve(&mut kit, model);
        assert!(rig.scale == 1.0 && rig.attachments.len() == 1, "character {character} is a real battle model with its weapon");
        assert!((rig.height - height).abs() < 5.0, "character {character}: {}", rig.height);
    }
    // the location of formation 301, and none for the 'no location' code
    let arena = kit.source.location(9).expect("battle location 9 is a model");
    assert!(kit.rigs.resolve_optional(&arena).is_some());
    assert!(kit.source.location(0xFFFF).is_none());
}

/// The debug formation (301: `md1stin`'s) from the owner's own data, as the window shows it: a real
/// `BattleClient` replays a real engine, the scene is made from what the client hands it, and the
/// frames go through the renderer. Writes the PNGs and checks the composition.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_battle_formation() {
    let Some(real) = open_real() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let formation_id: u16 = std::env::var("FMV_BATTLE_FORMATION").ok().and_then(|v| v.parse().ok()).unwrap_or(301);

    // The protocol's placement accessor and the engine agree on who the enemies are.
    let placement = real.rules.formation_scene(formation_id).expect("the formation has placement data");
    let setup = BattleSetup {
        formation_id,
        seed: 0x5CE7E,
        party: vec![member(0, 0), member(1, 2), member(2, 1)],
        preemptive: true,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
    };
    let mut server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
    assert_eq!(server.state.enemy_ids().len(), placement.enemies.len(), "one placement per engine enemy, in the engine's order");

    // The client, with the rules of this machine, takes the server's messages.
    let mut client = BattleClient::new(ME);
    client.set_rules(Some(Arc::clone(&real.rules)));
    client.on_message(0.0, server.start(&setup, real.rules.data_fingerprint()));
    assert!(client.in_battle());
    let start = client.scene_start().expect("a battle to draw");
    assert_eq!(start.formation.as_ref(), Some(&placement));
    // The real battle models of the owner's own battle.lgp when it is there (as the window has them,
    // with the clips scene.bin names for the enemies' attacks), the stand-ins otherwise.
    let mut kit = real.kit();
    let mut scene = BattleScene::new(&start, client.engine(), &mut kit).expect("a scene");
    if real.battle_lgp.is_some() {
        // Who is a real battle model: scale 1 (battle units, which a field stand-in at scale 36 is not;
        // a box is 1 too), the party carrying its weapons, the location. For the default formation
        // (the MPs) every actor also stands at a person's height and plays the archive's own idle
        // loop; other formations have bosses, one-frame idles and the like, which these do not hold for.
        for (i, actor) in scene.actors.iter().enumerate() {
            let r = &actor.rig;
            eprintln!(
                "actor {i} ({:?}): scale {}, height {:.0}, radius {:.0}, {} attachment(s), idle {}",
                actor.side,
                r.scale,
                r.height,
                r.radius,
                r.attachments.len(),
                if r.procedural_idle { "swayed (a still pose)" } else { "plays the clip" }
            );
            assert_eq!(r.scale, 1.0, "actor {i} is in battle units, not a field stand-in");
            if actor.side == Side::Party {
                assert_eq!(r.attachments.len(), 1, "party member {i} carries its weapon");
            }
            if formation_id == 301 {
                assert!(r.height > 600.0 && r.height < 1300.0, "actor {i}: {}", r.height);
                assert!(!r.procedural_idle, "actor {i}'s idle loop is the archive's clip");
            }
        }
        if formation_id == 301 {
            let armed_enemies = scene.actors.iter().filter(|a| a.side == Side::Enemy && !a.rig.attachments.is_empty()).count();
            assert_eq!(armed_enemies, 2, "both MPs carry their batons");
        }
        if placement.location <= 89 {
            assert!(scene.location.is_some(), "the formation's battle location {} is the real model", placement.location);
        }
    }
    let battle_view = |client: &BattleClient, now: f64| client.view(now).expect("a view");

    // 1. The picture of a battle that has just begun, at three window sizes.
    scene.update(1.0, &client.take_events(), client.engine());
    let view = battle_view(&client, 1.0);
    let idle = draw(&mut gpu, &scene, &view, W, H);
    save(&format!("{formation_id}-idle"), W, H, &idle);
    save(&format!("{formation_id}-idle-1920x1080"), 1920, 1080, &draw(&mut gpu, &scene, &view, 1920, 1080));
    save(&format!("{formation_id}-idle-640x480"), 640, 480, &draw(&mut gpu, &scene, &view, 640, 480));
    let bare = stage_only(&mut gpu, &scene, W, H);

    // The composition, as numbers: everybody inside the margins with the bottom free, the enemies on
    // the left, the party on the right, each drawn where it stands.
    let scene_view = scene.view(W, H);
    let anchors = scene.screen_anchors(&scene_view);
    assert_eq!(anchors.len(), server.state.actors().len());
    for a in &anchors {
        let (feet, head) = (a.feet.unwrap(), a.head.unwrap());
        assert!(feet[0] > W as f32 * 0.05 && feet[0] < W as f32 * 0.95, "{a:?} is inside the side margins");
        assert!(head[1] > 0.0 && feet[1] < H as f32 * 0.82, "{a:?} is inside, and the bottom is free");
        assert!(drawn_at(&idle, &bare, W, a), "{a:?} is drawn");
    }
    let mean = |side: Side| {
        let xs: Vec<f32> = anchors.iter().filter(|a| a.side == side).map(|a| a.feet.unwrap()[0]).collect();
        xs.iter().sum::<f32>() / xs.len() as f32
    };
    assert!(mean(Side::Enemy) < mean(Side::Party), "enemies on the left: {} vs {}", mean(Side::Enemy), mean(Side::Party));
    // The band and the overlay are over the scene, which is not the old flat backdrop.
    assert_eq!(pixel(&idle, W, W / 2, 3), BAND_VERIFIED_RGB);
    // The turn line is yellow text (ACCENT) in the top rows: light pixels in a row the scene's own
    // colours (a dark sky) do not have.
    let bright = idle.chunks_exact(4).enumerate().filter(|(i, p)| (*i as u32) / W < 60 && p[0] > 230 && p[1] > 190).count();
    assert!(bright > 50, "the status line's text is on top: {bright} bright pixels in the top rows");
    let colours: std::collections::BTreeSet<[u8; 3]> = idle.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
    assert!(colours.len() > 300, "a scene, not a few flat rectangles: {} distinct colours", colours.len());

    // 2. A fight: our character attacks, the engine's events reach the scene through the client.
    let mut now = 2.0;
    let first_step = server.step(1);
    feed(&mut client, &mut scene, first_step, now);
    let ready = server.state.ready_actors();
    assert!(!ready.is_empty(), "a preemptive start has the party ready");
    let first_blow = server.attack(ActorId(0));
    feed(&mut client, &mut scene, first_blow, now);
    assert_eq!(client.divergences(), 0, "the client's own engine reproduced the server's");
    let blow_at = now;
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for offset in [0.0, 0.30, 0.60, 0.80, 1.10, 1.60] {
        now = blow_at + offset;
        feed(&mut client, &mut scene, None, now);
        frames.push(draw(&mut gpu, &scene, &battle_view(&client, now), W, H));
    }
    assert!(changed_pixels(&frames[0], &frames[2], 20) > 2000, "the picture moves during a blow");
    let (sw, sh, sheet) = contact_sheet(&frames, W, H, 2);
    save(&format!("{formation_id}-blow"), sw, sh, &sheet);

    // 3. Keep fighting until an enemy dies and look at its collapse.
    let mut death_at = None;
    'fight: for _ in 0..4000 {
        now += 0.03;
        let mut messages: Vec<ServerMsg> = server.step(30).into_iter().collect();
        for id in server.state.ready_actors() {
            messages.extend(server.attack(id));
        }
        for msg in messages {
            let died = matches!(&msg, ServerMsg::BattleEvent { events, .. } if events.iter().any(|e| matches!(e, BattleEvent::Death { actor } if server.state.actor(*actor).is_some_and(|c| c.side == Side::Enemy))));
            client.on_message(now, msg);
            if died {
                death_at = Some(now);
                scene.update(now, &client.take_events(), client.engine());
                break 'fight;
            }
        }
        scene.update(now, &client.take_events(), client.engine());
        if server.state.outcome().is_some() {
            break;
        }
    }
    let died = death_at.expect("an enemy died within the fight");
    let mut death_frames: Vec<Vec<u8>> = Vec::new();
    for offset in [0.0, 0.7, 1.0, 1.3, 1.7, 2.6] {
        now = died + offset;
        scene.update(now, &client.take_events(), client.engine());
        death_frames.push(draw(&mut gpu, &scene, &battle_view(&client, now), W, H));
    }
    let (sw, sh, sheet) = contact_sheet(&death_frames, W, H, 2);
    save(&format!("{formation_id}-death"), sw, sh, &sheet);
    let after = scene.screen_anchors(&scene.view(W, H));
    let gone = after.iter().filter(|a| a.side == Side::Enemy && !a.drawn).count();
    assert!(gone >= 1, "the enemy that died is no longer drawn");
    assert_eq!(client.divergences(), 0);
}

/// A real battle of two players named for themselves: "Blizz", who holds Barret's combat slot, and "Wedge", who holds
/// Tifa's. The overlay (the party panel, the HP list, the target list, the command menu) calls them by their own
/// names and never by Barret's or Tifa's, which the engine's combat templates are called; their models in battle are
/// their slots' (Barret's and Tifa's battle models, a stand-in for the slot's). Made the way the window makes it: the
/// server's `EncounterStart` goes to the client state, `scene_for` builds the scene from it, and the overlay is drawn
/// over it through the real renderer: `battle_names.png` in `$FMV_SHOT_DIR`. Needs the owner's own `scene.bin`,
/// `kernel.bin`, `char.lgp` and, for the real battle models, `battle.lgp` (`FMV_FF7_DATA`).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_battle_names() {
    use crate::state::ClientState;
    use fmv_core::{AreaId, GameId};
    let Some(real) = open_real() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };

    let member = |entity: u64, account: AccountId, name: &str, character: u8| BattleMember {
        entity: EntityId(entity),
        account,
        name: name.into(),
        character,
        level: 6,
        weapon: None,
        armor: None,
        limit: 0,
    };
    let party = vec![member(1, ME, "Blizz", 1), member(2, AccountId(2), "Wedge", 2)];
    let setup = BattleSetup { formation_id: 301, seed: 0xB11, party, preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
    let server = Server { id: BattleId(1), state: real.rules.build(&setup).expect("the formation builds") };
    // The engine fights them as the canon characters of their slots: those are the names the engine knows.
    let engine_names: Vec<String> = server.state.party_ids().iter().filter_map(|id| server.state.actor(*id)).map(|c| c.name.clone()).collect();
    assert_eq!(engine_names, ["Barret", "Tifa"], "the slots' combat templates");

    // The window's own path: the client state takes the server's message, the battle is viewed, the scene is built.
    let area = AreaId { game: GameId::Ff7, key: "md1stin".into() };
    let mut state = ClientState::new(EntityId(1), ME, Pos::default(), area);
    state.battle.set_rules(Some(Arc::clone(&real.rules)));
    assert!(state.apply(0.0, server.start(&setup, real.rules.data_fingerprint())).is_none());
    let notes = state.battle.drain_notes();
    for line in ["roster actor=0 name=Blizz slot=1 slot_name=Barret level=6", "roster actor=1 name=Wedge slot=2 slot_name=Tifa level=6"] {
        assert!(notes.iter().any(|n| n == line), "{line:?} in {notes:?}");
    }
    let (mut slot, mut kit) = (None, Some(real.kit()));
    let view = state.battle.view(1.0).expect("a view of the battle");
    let events = state.battle.take_events();
    let scene = scene_for(&mut slot, &mut kit, &state, &view, 1.0, &events).expect("a scene");

    // The party in the view: the players' own names, our own marked.
    let party_names: Vec<&str> = view.party.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(party_names, ["Blizz", "Wedge"]);
    assert!(view.party[0].mine && !view.party[1].mine);
    assert!(!view.enemies.iter().any(|b| ["Barret", "Tifa", "Blizz", "Wedge"].contains(&b.name.as_str())), "the enemies keep the engine's names");
    assert!(!state.battle.title_suffix(1.0).contains("Barret") && !state.battle.title_suffix(1.0).contains("Tifa"));

    // The picture: the scene, then the overlay over the fighters' heads, as the window draws a frame.
    for (w, h, name) in [(W, H, "battle_names"), (1920, 1080, "battle_names_1920x1080")] {
        let scene_view = scene.view(w, h);
        let mut overlay = DrawList::new();
        draw_battle_overlay(&view, &scene.head_anchors(&scene_view), w, h, &mut overlay);
        let said: Vec<&str> = overlay.texts().iter().map(|t| t.text.as_str()).collect();
        assert!(said.iter().any(|t| t.contains("Blizz")) && said.iter().any(|t| t.contains("Wedge")), "the party panel names them: {said:?}");
        for canon in ["Barret", "Tifa"] {
            assert!(!said.iter().any(|t| t.contains(canon)), "the overlay names {canon}: {said:?}");
        }
        let img = gpu.render_battle(w, h, &scene.frame(&scene_view, false), &overlay);
        save(name, w, h, &img);
    }
}
