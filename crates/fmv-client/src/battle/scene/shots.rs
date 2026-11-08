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

// `chunks_exact(4)` over RGBA bytes would be `as_chunks` on Rust 1.88+; the workspace keeps its MSRV
// lower (see `avatar`), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::*;
use crate::avatar::library::tests::Counting;
use crate::avatar::source::LgpLoader;
use crate::battle::scene::model::StandInModels;
use crate::battle::{draw_battle_text, BattleClient};
use crate::ui::testkit::{pixel, save, Offscreen};
use fmv_core::{AccountId, EntityId};
use fmv_ff7_battle::rules::Command;
use fmv_protocol::{AppliedCommand, BattleRules, BattleSetup, Ff7BattleData, ServerMsg};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const W: u32 = 1100;
const H: u32 = 680;
const ME: AccountId = AccountId(1);

/// A frame as the window draws it: the scene, the popups and band, then the battle text.
fn draw(gpu: &mut Offscreen, scene: &BattleScene, battle: &BattleView, w: u32, h: u32) -> Vec<u8> {
    let view = scene.view(w, h);
    let mut overlay = DrawList::new();
    scene.draw_overlay(&view, battle.status, &mut overlay);
    draw_battle_text(battle, w, h, &mut overlay);
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
fn contact_sheet(frames: &[Vec<u8>], w: u32, h: u32, columns: usize) -> (u32, u32, Vec<u8>) {
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
fn feed(client: &mut BattleClient, scene: &mut BattleScene, msg: Option<ServerMsg>, now: f64) {
    if let Some(msg) = msg {
        client.on_message(now, msg);
    }
    scene.update(now, &client.take_events(), client.engine());
}

/// What the rules' server would say: one engine, its messages in the order a server sends them.
struct Server {
    id: BattleId,
    state: BattleState,
}

impl Server {
    fn start(&self, setup: &BattleSetup, fingerprint: fmv_core::Fingerprint) -> ServerMsg {
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

    fn step(&mut self, ticks: u64) -> Option<ServerMsg> {
        let events = self.state.step_ticks(ticks);
        (!events.is_empty()).then(|| ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: None })
    }

    /// `actor` attacks the first living enemy.
    fn attack(&mut self, actor: ActorId) -> Option<ServerMsg> {
        let target = self.state.enemy_ids().into_iter().find(|e| self.state.actor(*e).is_some_and(Combatant::is_alive))?;
        let command = Command::Attack { target };
        let events = self.state.submit_command(actor, command).ok()?;
        Some(ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: Some(AppliedCommand { actor, command }) })
    }
}

fn member(i: u64, character: u8) -> BattleMember {
    BattleMember { entity: EntityId(10 + i), account: if i == 0 { ME } else { AccountId(10 + i) }, character, level: 12, weapon: None, armor: None }
}

// ---------------------------------------------------------------------------------------------
// No game data
// ---------------------------------------------------------------------------------------------

/// Stick-figure party, box enemies, the formation's own placement (made-up numbers) and a camera
/// to the side: the composition of a normal battle, checked on pixels.
#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_synthetic_battle_scene() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let rules = fmv_protocol::SyntheticRules::default();
    let setup = BattleSetup {
        formation_id: 1,
        seed: 3,
        party: vec![member(0, 0), member(1, 2), member(2, 1)],
        preemptive: true,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
    };
    let engine = rules.build(&setup).unwrap();
    let formation = FormationScene {
        location: 0,
        layout: fmv_ff7_battle::scene::BattleLayout::Normal,
        camera: fmv_protocol::battle::CameraPose { position: [9000, -2400, 1800], target: [0, -300, 100] },
        enemies: vec![
            fmv_protocol::battle::EnemyPlacement { enemy_id: 1, position: [-650, 0, -1450], row: 1 },
            fmv_protocol::battle::EnemyPlacement { enemy_id: 2, position: [600, 0, -2100], row: 1 },
        ],
    };
    let start = SceneStart { id: BattleId(1), party: setup.party.clone(), my_actor: Some(ActorId(0)), formation: Some(formation) };
    let mut kit = ModelKit::new(Box::new(StandInModels), Some(Box::new(Counting::new(Rc::new(RefCell::new(Vec::new())), vec![]))));
    let mut scene = BattleScene::new(&start, Some(&engine), &[], &mut kit).expect("a scene");
    scene.update(1.0, &[], Some(&engine));
    let battle = BattleView {
        id: BattleId(1),
        party: Vec::new(),
        enemies: Vec::new(),
        status: Status::Verified,
        waiting_for_answer: false,
    };
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

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

struct Real {
    rules: Arc<dyn BattleRules>,
    char_lgp: PathBuf,
}

/// `FMV_FF7_DATA` (the game's `data/`: `lang-en/battle/scene.bin`, `lang-en/kernel/kernel.bin` or the
/// classic layout, and `field/char.lgp`), else `FMV_FF7_LIBRARY` (the state directory of the
/// imported library, read-only). `None` (with a note) when neither is set.
fn open_real() -> Option<Real> {
    if let Some(data) = std::env::var_os("FMV_FF7_DATA").map(PathBuf::from) {
        let first = |paths: &[&str]| paths.iter().map(|p| data.join(p)).find(|p| p.is_file());
        let scene = first(&["lang-en/battle/scene.bin", "battle/scene.bin"]).expect("scene.bin under FMV_FF7_DATA");
        let kernel = first(&["lang-en/kernel/kernel.bin", "kernel/KERNEL.BIN", "kernel/kernel.bin"]).expect("kernel.bin under FMV_FF7_DATA");
        let char_lgp = first(&["field/char.lgp", "field/CHAR.LGP"]).expect("char.lgp under FMV_FF7_DATA");
        let rules: Arc<dyn BattleRules> = Arc::new(Ff7BattleData::open(&scene, &kernel).expect("the battle data opens"));
        return Some(Real { rules, char_lgp });
    }
    if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        let source = crate::ff7::Ff7Source::Library { state_dir: Some(dir.into()), set: None };
        let (rules, _) = crate::battle::open_rules(&source).expect("the battle data opens");
        let char_lgp = crate::ff7::resolve_char_lgp(&source).expect("looked for").expect("a char.lgp");
        return Some(Real { rules, char_lgp });
    }
    eprintln!("skipped: neither FMV_FF7_DATA nor FMV_FF7_LIBRARY is set");
    None
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
    let mut kit = ModelKit::new(Box::new(StandInModels), Some(Box::new(LgpLoader::new(&real.char_lgp))));
    let looks = [None, None, None];
    let mut scene = BattleScene::new(&start, client.engine(), &looks, &mut kit).expect("a scene");
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
