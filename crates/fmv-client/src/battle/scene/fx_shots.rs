//! Pictures of every effect, to look at: the synthetic scene (stick figures and boxes, no game data) with the effects of
//! [`super::effects`] put on it directly, each at several instants of its life, laid out as contact sheets in `$FMV_SHOT_DIR`.
//!
//! They need a GPU or a software Vulkan/GL driver, so they only run on request:
//! `cargo test -p fmv-client --bin fmv-client -- --ignored offscreen_battle_effects --test-threads=1`.
//! The sheets are `fx-blows`, `fx-elements`, `fx-restoring`, `fx-statuses-curses`, `fx-statuses-buffs` (a close-up of a
//! fighter, a row for each effect and a tile for each instant) and `fx-staging` (what flies, what is gathered, the flash, over
//! the whole scene); `sheets` has the same effects in real fights, with the real models.

use super::effects::{Element, Fx, Kind, Strike};
use super::shots::{contact_sheet, stand_ins, synthetic_scene};
use super::*;
use crate::ui::draw::DrawList;
use crate::ui::testkit::{save, Offscreen};
use fmv_ff7_battle::status::Status;

/// A tile of a sheet: a window this big with a fighter in the middle of it.
const TILE: u32 = 360;
/// How tall the fighter is in a tile, in pixels.
const FIGURE_PX: f64 = 150.0;

/// The scene's actors: the party is 0 to 2 (stick figures) and the enemies 3 and 4 (boxes).
const TARGET: usize = 1;
const SHOOTER: usize = 3;

/// `kind` as an effect that lands at 10.6 (it begins with its performance at 10.0 when it has a lead).
fn fx(kind: Kind, element: Element) -> Fx {
    let lead = matches!(kind, Kind::Blast | Kind::Mend | Kind::Mana | Kind::Raise | Kind::Afflict(_) | Kind::Boost(_) | Kind::Charge | Kind::Toss);
    let bolt = matches!(kind, Kind::Bolt { .. });
    let start = if lead { 10.0 } else if bolt { 10.46 } else { 10.6 };
    Fx { kind, element, target: TARGET, from: Some(SHOOTER), start, land: 10.6, scale: 1.0, seed: 41 }
}

/// A moment of an effect: a fraction of its lead (`Lead(0.5)`) or of its life after the landing (`Age(0.2)`).
#[derive(Clone, Copy)]
enum At {
    Lead(f64),
    Age(f64),
}

impl At {
    fn time(self, fx: &Fx) -> f64 {
        match self {
            At::Lead(p) => fx.start + (fx.land - fx.start) * p,
            At::Age(a) => fx.land + fx.life() * a,
        }
    }

    fn label(self) -> String {
        match self {
            At::Lead(p) => format!("lead {p:.2}"),
            At::Age(a) => format!("age {a:.2}"),
        }
    }
}

/// The lens of a close-up of actor `who`: its chest in the middle of a `TILE` window and [`FIGURE_PX`] tall from head to feet.
fn close_view(scene: &BattleScene, who: usize) -> View {
    let base = scene.view(TILE, TILE);
    let (pos, ..) = scene.pose_of(who).expect("a pose");
    let height = f64::from(scene.actors[who].rig.height);
    let (feet, head) = (base.to_pixels(pos).expect("on screen"), base.to_pixels([pos[0], pos[1] - height, pos[2]]).expect("on screen"));
    let now_px = f64::from(feet[0] - head[0]).hypot(f64::from(feet[1] - head[1]));
    let mut view = View { zoom: base.zoom * FIGURE_PX / now_px.max(1.0), shift: [0.0, 0.0], ..base };
    let chest = [pos[0], pos[1] - height * 0.5, pos[2]];
    let [u, v] = view.pinhole(view.camera.to_camera(chest), 1.0).expect("in front");
    view.shift = [-f64::from(u), -f64::from(v)];
    view
}

/// One close-up of the scene at `at` of `fx`, `label` written in its corner.
fn tile(gpu: &mut Offscreen, scene: &mut BattleScene, fx: Fx, at: At, label: &str) -> Vec<u8> {
    scene.director.set_fx(vec![fx]);
    scene.update(at.time(&fx), &[], None);
    let view = close_view(scene, TARGET);
    let mut overlay = DrawList::new();
    overlay.draw_text_shadowed(6, 6, 2, [255, 255, 120, 255], [0, 0, 0, 255], label);
    gpu.render_battle(TILE, TILE, &scene.frame(&view, false), &overlay)
}

/// A sheet: a row for each of `rows` (a name and its effect), a tile for each of `moments`.
fn sheet(gpu: &mut Offscreen, scene: &mut BattleScene, name: &str, rows: &[(String, Fx)], moments: &[At]) {
    let mut tiles = Vec::new();
    for (label, f) in rows {
        for &m in moments {
            tiles.push(tile(gpu, scene, *f, m, &format!("{label} {}", m.label())));
        }
    }
    let per_row = moments.len();
    let (sw, sh) = (TILE * per_row as u32, TILE * tiles.len().div_ceil(per_row) as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (n, t) in tiles.iter().enumerate() {
        let (ox, oy) = ((n % per_row) as u32 * TILE, (n / per_row) as u32 * TILE);
        for y in 0..TILE {
            let src = &t[(y * TILE * 4) as usize..((y + 1) * TILE * 4) as usize];
            let dst = (((oy + y) * sw + ox) * 4) as usize;
            sheet[dst..dst + src.len()].copy_from_slice(src);
        }
    }
    save(name, sw, sh, &sheet);
}

/// The scene the sheets are drawn on: the party against two dummies, no engine (nothing but the effect moves).
fn stage() -> Option<(Offscreen, BattleScene)> {
    let gpu = Offscreen::new()?;
    Some((gpu, synthetic_scene(&mut stand_ins(), 0)))
}

#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_battle_effects_blows_and_shots() {
    let Some((mut gpu, mut scene)) = stage() else { return };
    let plain = Element::Plain;
    let rows: Vec<(String, Fx)> = vec![
        ("slash".into(), fx(Kind::Strike { how: Strike::Slash, critical: false }, plain)),
        ("slash!".into(), fx(Kind::Strike { how: Strike::Slash, critical: true }, plain)),
        ("thump".into(), fx(Kind::Strike { how: Strike::Thump, critical: false }, plain)),
        ("punch".into(), fx(Kind::Strike { how: Strike::Punch, critical: false }, plain)),
        ("fire slash".into(), fx(Kind::Strike { how: Strike::Slash, critical: false }, Element::Fire)),
        ("shot".into(), fx(Kind::Impact { critical: false }, plain)),
        ("shot!".into(), fx(Kind::Impact { critical: true }, plain)),
        ("whiff".into(), fx(Kind::Whiff, plain)),
    ];
    sheet(&mut gpu, &mut scene, "fx-blows", &rows, &[At::Age(0.02), At::Age(0.15), At::Age(0.35), At::Age(0.6), At::Age(0.85)]);
}

#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_battle_effects_elements() {
    let Some((mut gpu, mut scene)) = stage() else { return };
    let rows: Vec<(String, Fx)> = [Element::Fire, Element::Ice, Element::Bolt, Element::Earth, Element::Wind, Element::Water, Element::Poison, Element::Gravity, Element::Holy, Element::Plain]
        .iter()
        .map(|e| (format!("{e:?}"), fx(Kind::Blast, *e)))
        .collect();
    sheet(&mut gpu, &mut scene, "fx-elements", &rows, &[At::Lead(0.8), At::Age(0.0), At::Age(0.12), At::Age(0.3), At::Age(0.55), At::Age(0.8)]);
}

#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_battle_effects_restoring_and_statuses() {
    let Some((mut gpu, mut scene)) = stage() else { return };
    let rows: Vec<(String, Fx)> = vec![
        ("mend".into(), fx(Kind::Mend, Element::Cure)),
        ("mana".into(), fx(Kind::Mana, Element::Water)),
        ("raise".into(), fx(Kind::Raise, Element::Holy)),
        ("cleanse poison".into(), fx(Kind::Cleanse(Status::Poison), Element::Plain)),
        ("cleanse sleep".into(), fx(Kind::Cleanse(Status::Sleep), Element::Plain)),
    ];
    sheet(&mut gpu, &mut scene, "fx-restoring", &rows, &[At::Lead(0.8), At::Age(0.02), At::Age(0.15), At::Age(0.35), At::Age(0.6), At::Age(0.85)]);
    let curses = [Status::Sleep, Status::Poison, Status::Silence, Status::Darkness, Status::Slow, Status::Stop, Status::Paralysis, Status::Sadness, Status::Fury];
    let rows: Vec<(String, Fx)> = curses.iter().map(|s| (s.name().to_string(), fx(Kind::Afflict(*s), Element::Plain))).collect();
    sheet(&mut gpu, &mut scene, "fx-statuses-curses", &rows, &[At::Age(0.03), At::Age(0.15), At::Age(0.3), At::Age(0.5), At::Age(0.75)]);
    let buffs = [Status::Haste, Status::Regen, Status::Barrier, Status::MBarrier];
    let rows: Vec<(String, Fx)> = buffs.iter().map(|s| (s.name().to_string(), fx(Kind::Boost(*s), Element::Plain))).collect();
    sheet(&mut gpu, &mut scene, "fx-statuses-buffs", &rows, &[At::Age(0.03), At::Age(0.15), At::Age(0.3), At::Age(0.5), At::Age(0.75)]);
}

/// What flies, what is gathered and the flash are drawn over the whole scene: a 1100 x 680 frame each.
#[test]
#[ignore = "needs a GPU or a software Vulkan/GL driver"]
fn offscreen_battle_effects_staging() {
    let Some((mut gpu, mut scene)) = stage() else { return };
    let mut frames = Vec::new();
    let mut frame = |scene: &mut BattleScene, fx: Fx, now: f64, label: &str| {
        scene.director.set_fx(vec![fx]);
        scene.update(now, &[], None);
        let view = scene.view(1100, 680);
        let mut overlay = DrawList::new();
        overlay.draw_text_shadowed(8, 24, 2, [255, 255, 120, 255], [0, 0, 0, 255], label);
        frames.push(gpu.render_battle(1100, 680, &scene.frame(&view, false), &overlay));
    };
    // The enemy dummy at the left shoots the party member in the middle.
    let bolt = fx(Kind::Bolt { hit: true }, Element::Plain);
    for (p, label) in [(0.15, "bolt 0.15"), (0.5, "bolt 0.5"), (0.9, "bolt 0.9")] {
        frame(&mut scene, bolt, bolt.start + (bolt.land - bolt.start) * p, label);
    }
    frame(&mut scene, bolt, bolt.land + 0.08, "bolt after");
    let miss = fx(Kind::Bolt { hit: false }, Element::Plain);
    frame(&mut scene, miss, miss.land - 0.03, "bolt past");
    let toss = Fx { from: Some(0), ..fx(Kind::Toss, Element::Plain) };
    for (p, label) in [(0.2, "toss 0.2"), (0.5, "toss 0.5"), (0.85, "toss 0.85")] {
        frame(&mut scene, toss, toss.start + (toss.land - toss.start) * p, label);
    }
    let charge = Fx { target: 0, ..fx(Kind::Charge, Element::Fire) };
    for (p, label) in [(0.3, "charge 0.3"), (0.7, "charge 0.7"), (0.98, "charge 0.98")] {
        frame(&mut scene, charge, charge.start + (charge.land - charge.start) * p, label);
    }
    let big = Fx { scale: 1.6, ..charge };
    frame(&mut scene, big, big.start + (big.land - big.start) * 0.8, "limit charge 0.8");
    let flash = fx(Kind::Flash, Element::Plain);
    for (a, label) in [(0.02, "flash 0.02"), (0.2, "flash 0.2"), (0.5, "flash 0.5")] {
        frame(&mut scene, flash, flash.land + flash.life() * a, label);
    }
    let guard = Fx { target: 2, ..fx(Kind::Guard, Element::Plain) };
    frame(&mut scene, guard, guard.land + guard.life() * 0.2, "guard 0.2");
    let fizzle = Fx { target: 2, ..fx(Kind::Fizzle, Element::Ice) };
    frame(&mut scene, fizzle, fizzle.land + fizzle.life() * 0.25, "fizzle 0.25");
    let (sw, sh, sheet) = contact_sheet(&frames, 1100, 680, 3);
    save("fx-staging", sw, sh, &sheet);
}
