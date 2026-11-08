//! Scratch probe (test-only, ignored): prints what the owner's clips look like in numbers and draws
//! contact sheets of slots, so that what each slot DOES can be looked at.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::camera::{BattleCamera, View};
use super::BattleFrame;
use crate::avatar::frame::{skin, AvatarFrame, SkinnedModel};
use crate::avatar::ground::{circle, GroundShape};
use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use crate::ui::draw::DrawList;
use crate::ui::testkit::{save, Offscreen};
use fmv_core::Pos;
use fmv_ff7::battle_model::{BattleCharacter, BattleModel, BattleModels};
use fmv_ff7::model::{mat4_identity, mat4_mul, mat4_translate};
use fmv_ff7::playable::Character;
use std::path::PathBuf;
use std::sync::Arc;

fn battle_lgp() -> Option<PathBuf> {
    let data = PathBuf::from(std::env::var_os("FMV_FF7_DATA")?);
    Some(data.join("battle").join("battle.lgp"))
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn probe_character_clips() {
    let Some(path) = battle_lgp() else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    for which in BattleCharacter::ALL {
        let Ok(model) = models.load_character(which) else { continue };
        eprintln!("== {} ({}): {} slots, {} weapon tracks", which.display_name(), model.prefix, model.animations.len(), model.weapon_tracks.len());
        for (slot, clip) in model.animations.iter().enumerate() {
            let frames = clip.frame_count();
            let first = clip.frame(0).map(|f| f.root_translation).unwrap_or([0.0; 3]);
            let last = clip.frame(frames - 1).map(|f| f.root_translation).unwrap_or([0.0; 3]);
            let (mut zmin, mut zmax, mut ymin, mut ymax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for f in clip.frames() {
                zmin = zmin.min(f.root_translation[2]);
                zmax = zmax.max(f.root_translation[2]);
                ymin = ymin.min(f.root_translation[1]);
                ymax = ymax.max(f.root_translation[1]);
            }
            let top = |frame: usize| -> f32 {
                let Some(pose) = model.model.pose(clip, frame) else { return f32::NAN };
                model.model.posed_bounds(&pose, &mat4_identity()).map_or(f32::NAN, |(lo, _)| -lo[1])
            };
            eprintln!(
                "slot {slot:2} ok={:5} frames={frames:3} root0=({:6.0},{:6.0},{:6.0}) rootN=({:6.0},{:6.0},{:6.0}) z[{:6.0},{:6.0}] y[{:6.0},{:6.0}] top0={:5.0} topN={:5.0}",
                model.slot_ok[slot], first[0], first[1], first[2], last[0], last[1], last[2], zmin, zmax, ymin, ymax, top(0), top(frames - 1)
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Contact sheets of slots
// ---------------------------------------------------------------------------------------------

const TILE_W: u32 = 168;
const TILE_H: u32 = 190;

struct Rig {
    asset: Arc<AvatarAsset>,
    weapon: Option<Arc<AvatarAsset>>,
    model: BattleModel,
    /// Mirror the model (face right) for enemies is not needed: tiles show the model's own heading.
    view: View,
    /// Keep the root's travel (a wide tile) instead of holding the figure in the middle.
    keep_root: bool,
}

impl Rig {
    fn new(model: BattleModel, dist: f64, height: f64) -> Self {
        Self::with(model, dist, height, false)
    }

    fn with(model: BattleModel, dist: f64, height: f64, keep_root: bool) -> Self {
        let stand = |m: &fmv_ff7::FieldModel, clip: &fmv_ff7::FieldAnimation| {
            Arc::new(
                AvatarAsset::new(Character { appearance: NON_PLAYABLE, model: m.clone(), idle: clip.clone(), walk: clip.clone(), run: clip.clone() }).expect("mesh"),
            )
        };
        let asset = stand(&model.model, &model.animations[0]);
        let weapon = match (model.weapons.first(), model.weapon_tracks.first()) {
            (Some(w), Some(t)) => Some(stand(w, t)),
            _ => None,
        };
        // From +x looking at the model: +z is the screen's right, so a model faces screen-left.
        // Kept root travel: look at a point 600 units ahead of the start (the model walks to -z, the
        // screen's left) and cover 2600 units across the tile.
        let centre_z = if keep_root { -600.0 } else { 0.0 };
        let camera = BattleCamera::look_at([dist, -height * 0.55, centre_z], [0.0, -height * 0.5, centre_z]).expect("camera");
        let zoom = if keep_root { f64::from(TILE_W) * dist / 2600.0 } else { f64::from(TILE_H) * dist / (height * 1.35) };
        let view = View { camera, width: TILE_W, height: TILE_H, zoom, shift: [0.0, 0.0] };
        Self { asset, weapon, model, view, keep_root }
    }

    /// The tile of `slot` at `frame`, root travel in x and z taken out so the figure stays in the tile.
    fn tile(&self, gpu: &mut Offscreen, slot: usize, frame: usize, label: &str) -> Vec<u8> {
        let view = &self.view;
        let clip = &self.model.animations[slot];
        let f = clip.frame(frame % clip.frame_count()).expect("frame");
        let place = if self.keep_root { mat4_identity() } else { mat4_translate([-f.root_translation[0], 0.0, -f.root_translation[2]]) };
        let camera = view.camera.matrix();
        let mut models = Vec::new();
        if let Some(pose) = self.model.model.pose(clip, frame) {
            let mut positions = Vec::new();
            if skin(&self.asset, &pose, &place, &camera, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(&self.asset), positions });
            }
        }
        if let (Some(weapon), Some(track)) = (&self.weapon, self.model.weapon_tracks.get(slot)) {
            if let Some(wp) = self.model.weapon_pose(0, slot, frame) {
                let _ = track;
                let mut positions = Vec::new();
                if skin(weapon, &[wp], &place, &camera, &mut positions) {
                    models.push(SkinnedModel { asset: Arc::clone(weapon), positions });
                }
            }
        }
        let to_view = |p: Pos| {
            let c = view.camera.to_camera([f64::from(p.x), 0.0, f64::from(p.y)]);
            view.pinhole(c, 100.0).map(|[u, v]| Pos::new(u, v, 0.0))
        };
        let depth = view.camera.to_camera([0.0, 0.0, 0.0])[2] as f32;
        let mut ground = Vec::new();
        if let Some(outline) = circle(&to_view, Pos::new(0.0, 0.0, 0.0), 420.0) {
            ground.push(GroundShape::disc(outline, [0, 0, 0], 0.45, depth));
        }
        let frame_data = BattleFrame {
            clear_rgb: [70, 78, 92],
            stage: Vec::new(),
            location: None,
            models: AvatarFrame { models, zoom: view.zoom as f32, overlay_bias: 3.0, ground, scenery: None },
            tints: Vec::new(),
            camera: view.gpu_camera(),
        };
        let mut overlay = DrawList::new();
        overlay.draw_text_shadowed(3, 3, 1, [255, 255, 120, 255], [0, 0, 0, 255], label);
        gpu.render_battle(TILE_W, TILE_H, &frame_data, &overlay)
    }
}

fn blit(sheet: &mut [u8], sw: u32, tile: &[u8], ox: u32, oy: u32) {
    for y in 0..TILE_H {
        let src = &tile[(y * TILE_W * 4) as usize..((y + 1) * TILE_W * 4) as usize];
        let dst = (((oy + y) * sw + ox) * 4) as usize;
        sheet[dst..dst + src.len()].copy_from_slice(src);
    }
}

/// A sheet: one row per slot in `slots`, `per_row` evenly spaced frames in each.
fn clip_sheet(rig: &Rig, gpu: &mut Offscreen, slots: &[usize], per_row: usize) -> (u32, u32, Vec<u8>) {
    let (sw, sh) = (TILE_W * per_row as u32, TILE_H * slots.len() as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (row, &slot) in slots.iter().enumerate() {
        let n = rig.model.animations[slot].frame_count();
        for col in 0..per_row {
            let frame = if per_row == 1 { 0 } else { (col * (n - 1).max(0)) / (per_row - 1) };
            let label = format!("s{slot} f{frame}/{n}");
            let tile = rig.tile(gpu, slot, frame, &label);
            blit(&mut sheet, sw, &tile, col as u32 * TILE_W, row as u32 * TILE_H);
        }
    }
    (sw, sh, sheet)
}

fn enemy_of(id: u16) -> Option<BattleModel> {
    let mut models = BattleModels::open(battle_lgp()?).expect("battle.lgp");
    models.load_enemy(id).ok()
}

/// Enemy `PROBE_ENEMY` (default 16, the MP): every slot, 8 frames each, the root's travel kept.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or software driver"]
fn probe_enemy_sheets() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let id: u16 = std::env::var("PROBE_ENEMY").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
    let keep = std::env::var("PROBE_KEEP").map_or(true, |v| v != "0");
    let Some(model) = enemy_of(id) else { return };
    let slots: Vec<usize> = (0..model.animations.len()).filter(|&s| model.slot_ok[s]).collect();
    eprintln!("enemy {id} ({}): {} slots, ok: {:?}", model.prefix, model.animations.len(), slots);
    let rig = Rig::with(model, 3600.0, 800.0, keep);
    for (n, chunk) in slots.chunks(6).enumerate() {
        let (w, h, sheet) = clip_sheet(&rig, &mut gpu, chunk, 8);
        save(&format!("probe-enemy{id}-{n}"), w, h, &sheet);
    }
}

fn model_of(which: BattleCharacter) -> Option<BattleModel> {
    let mut models = BattleModels::open(battle_lgp()?).expect("battle.lgp");
    models.load_character(which).ok()
}

/// Cloud's slots 0..=51 in rows of 8 frames, 6 slots to a picture.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA) and a GPU or software driver"]
fn probe_character_sheets() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let who = std::env::var("PROBE_CHARACTER").unwrap_or_else(|_| "cloud".into());
    let which = BattleCharacter::ALL.into_iter().find(|c| c.prefix() == who || c.display_name().to_ascii_lowercase().starts_with(&who)).expect("a character");
    let Some(model) = model_of(which) else { return };
    let slots_total = model.animations.len();
    let ok: Vec<usize> = match std::env::var("PROBE_SLOTS") {
        Ok(list) => list.split(',').filter_map(|s| s.parse::<usize>().ok()).filter(|&s| s < slots_total && model.slot_ok[s]).collect(),
        Err(_) => (0..slots_total).filter(|&s| model.slot_ok[s]).collect(),
    };
    let per_row: usize = std::env::var("PROBE_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let per_sheet: usize = std::env::var("PROBE_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let tag = std::env::var("PROBE_TAG").unwrap_or_default();
    let rig = Rig::new(model, 3600.0, 900.0);
    for (n, chunk) in ok.chunks(per_sheet).enumerate() {
        let (w, h, sheet) = clip_sheet(&rig, &mut gpu, chunk, per_row);
        save(&format!("probe-{}{tag}-{n}", which.prefix()), w, h, &sheet);
    }
    let _ = mat4_mul(&mat4_identity(), &mat4_identity());
}

fn scene_bin() -> Option<fmv_ff7_battle::scene::SceneBin> {
    let data = PathBuf::from(std::env::var_os("FMV_FF7_DATA")?);
    let path = ["lang-en/battle/scene.bin", "battle/scene.bin"].iter().map(|p| data.join(p)).find(|p| p.is_file())?;
    fmv_ff7_battle::scene::SceneBin::open(path).ok()
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn probe_enemy_records() {
    let (Some(scenes), Some(path)) = (scene_bin(), battle_lgp()) else { return };
    let mut models = BattleModels::open(path).expect("battle.lgp");
    let mut shown = 0;
    for scene in scenes.scenes() {
        for (k, id) in scene.enemy_ids.iter().enumerate() {
            let (Some(id), Some(rec)) = (id, scene.enemies[k].as_ref()) else { continue };
            let wanted = std::env::var("PROBE_ENEMIES").ok().map(|v| v.split(',').filter_map(|s| s.parse::<u16>().ok()).collect::<Vec<_>>());
            if let Some(wanted) = &wanted {
                if !wanted.contains(id) {
                    continue;
                }
            } else if scene.index > 90 || shown > 40 {
                continue;
            }
            let Ok(model) = models.load_enemy(*id) else { continue };
            shown += 1;
            let attacks: Vec<String> = rec
                .attack_ids
                .iter()
                .zip(rec.animation_indices.iter())
                .enumerate()
                .filter(|(_, (a, _))| **a != 0xFFFF)
                .map(|(i, (a, anim))| {
                    let root = model
                        .animations
                        .get(usize::from(*anim))
                        .map(|c| {
                            let zs: Vec<f32> = c.frames().iter().map(|f| f.root_translation[2]).collect();
                            let xs: Vec<f32> = c.frames().iter().map(|f| f.root_translation[0]).collect();
                            format!(
                                "frames {} z {:.0}->{:.0} [{:.0},{:.0}] x [{:.0},{:.0}] ok={}",
                                c.frame_count(),
                                zs[0],
                                zs[zs.len() - 1],
                                zs.iter().cloned().fold(f32::MAX, f32::min),
                                zs.iter().cloned().fold(f32::MIN, f32::max),
                                xs.iter().cloned().fold(f32::MAX, f32::min),
                                xs.iter().cloned().fold(f32::MIN, f32::max),
                                model.slot_ok[usize::from(*anim)]
                            )
                        })
                        .unwrap_or_else(|| "beyond".to_string());
                    let atk = scene.attack_by_id(*a);
                    format!(
                        "[{i}] atk {a} {:?} flags {:#04x} -> slot {anim} ({root})",
                        atk.map(|s| s.name.clone()).unwrap_or_default(),
                        atk.map_or(0, |s| s.record.target_flags)
                    )
                })
                .collect();
            let frames: Vec<usize> = model.animations.iter().map(|c| c.frame_count()).collect();
            eprintln!(
                "scene {:3} enemy {:3} {:18} model {} slots={} frames={:?}\n    anim_idx={:?}\n    {}",
                scene.index,
                id,
                rec.name,
                model.prefix,
                model.animations.len(),
                frames,
                rec.animation_indices,
                attacks.join("; ")
            );
        }
    }
}
