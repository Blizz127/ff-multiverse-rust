//! Verification render of the per-tile depth rule (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example depth_composite -j16
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example depth_composite -j16 -- --field md1stin
//! ```
//!
//! Software-composites Cloud (idle pose, facing the camera) into a field twice at the same walkmesh
//! spots: with the *flat* front layer (what a client that draws layers 2 and 4 over the characters
//! does) and with *per-tile depth* ([`fmv_ff7::background::decode_background_tiles_with_depth`]: a tile
//! hides a character where the tile's depth, `4 * id`, is smaller than the character's depth). The
//! characters are rasterised with the field camera through [`fmv_ff7::background::Projection`], so
//! their depth is `Projection`'s `depth`, the unit of the tile depth.
//!
//! Spots (all printed with the numbers behind them):
//!
//! 1. **The earlier counter-example**, re-derived exactly as the client's `shots.rs::spots()` does:
//!    per walkmesh triangle the centroid and the three edge midpoints, camera depth 450..1300, the
//!    character's screen rectangle from its projected vertices (posed with facing 0), coverage by the
//!    flat front image's alpha, and the pick "coverage between 0.2 and 0.9 closest to 0.5, preferring the
//!    object `(2, 1)`". The reproduction gives the client's own figures for `md1stin` (the flat layer
//!    touches Cloud's rectangle at 59 of 73 places; dominant objects (2, 544) 23, (2, 152) 14, (2, 294)
//!    14, (2, 1) 7, (2, 4088) 1), and the dominant object at the pick is printed. Also every
//!    `spawn_points()` entry.
//! 2. **In front of the left wall piece** (2, 544): the walkmesh spot where the flat layer hides the
//!    most of Cloud behind that object although he is nearer than it.
//! 3. **Genuinely behind it**: walkmesh spots linked to a spawn point's triangle, with Cloud inside the
//!    camera range, where he is farther than the object's depth 2176 and so hidden by it.
//! 4. The same for the posts and the train, partly hidden.
//!
//! PNGs go to `/home/blizz/.cache/claude-tmp/ffmv-ff7-dump/` (outside the repository; they contain
//! game art: never commit or share them): `depth_<field>_<spot>.png` holds, left to right, the flat
//! composite, the per-tile depth composite and the depth composite with the front tiles tinted red where
//! they are nearer than Cloud (hide him) and blue where they are farther (he is in front), all cropped
//! around him; `depth_<field>_<spot>_full.png` is the whole picture with per-tile depth.
// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]
use fmv_adapter_api::AreaRuntime;
use fmv_core::{EditionProvenance, GameId, Platform};
use fmv_ff7::background::{
    decode_background_layers, decode_background_tiles_with_depth, depth_of_point, BlendMode, DepthTile, Projection,
};
use fmv_ff7::field::Walkmesh;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::model::{default_model_scale, load_field_animation, load_field_model, model_to_field, transform_point, FieldModel};
use fmv_ff7::playable::resolve_appearance;
use fmv_ff7::{Ff7Adapter, RgbaImage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const OUT_DIR: &str = "/home/blizz/.cache/claude-tmp/ffmv-ff7-dump";

/// An object of layer 2 (or 4): `(layer, id)`.
pub type Object = (u8, u16);

// ---- software rasteriser with a depth buffer ------------------------------------------------------

#[derive(Clone, Copy)]
struct Vtx {
    x: f32,
    y: f32,
    /// Camera depth (smaller is nearer).
    z: f32,
    rgb: [f32; 3],
    uv: [f32; 2],
}

struct Raster {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
    depth: Vec<f32>,
}

impl Raster {
    fn put(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let at = (y * self.w + x) * 4;
        if let Some(px) = self.rgba.get_mut(at..at + 4) {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }

    /// Draw one triangle with a depth test (`z <= buffer`), perspective-correct depth (1/z is linear
    /// in screen space), vertex colours modulated by the texture; texels with alpha below 128 are
    /// discarded.
    fn triangle(&mut self, v: [Vtx; 3], texture: Option<&RgbaImage>) {
        let [a, b, c] = v;
        let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if area.abs() < 1e-9 || !area.is_finite() || a.z <= 0.0 || b.z <= 0.0 || c.z <= 0.0 {
            return;
        }
        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
        let max_x = (a.x.max(b.x).max(c.x).ceil().max(0.0) as usize).min(self.w.saturating_sub(1));
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
        let max_y = (a.y.max(b.y).max(c.y).ceil().max(0.0) as usize).min(self.h.saturating_sub(1));
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((b.x - px) * (c.y - py) - (b.y - py) * (c.x - px)) / area;
                let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let inv = w0 / a.z + w1 / b.z + w2 / c.z;
                if inv <= 0.0 {
                    continue;
                }
                let z = 1.0 / inv;
                let Some(slot) = self.depth.get_mut(y * self.w + x) else { continue };
                if z > *slot {
                    continue;
                }
                let (p0, p1, p2) = (w0 / a.z / inv, w1 / b.z / inv, w2 / c.z / inv);
                let mix = |f: fn(&Vtx) -> f32| p0 * f(&a) + p1 * f(&b) + p2 * f(&c);
                let mut rgb = [mix(|p| p.rgb[0]), mix(|p| p.rgb[1]), mix(|p| p.rgb[2])];
                if let Some(tex) = texture {
                    let texel = tex.sample(mix(|p| p.uv[0]), mix(|p| p.uv[1]));
                    if texel[3] < 128 {
                        continue;
                    }
                    for (k, ch) in rgb.iter_mut().enumerate() {
                        *ch *= f32::from(texel.get(k).copied().unwrap_or(0)) / 255.0;
                    }
                }
                *slot = z;
                self.put(x, y, rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
            }
        }
    }
}

fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    println!("wrote {}", path.display());
    Ok(())
}

// ---- the field, the character and the screen ------------------------------------------------------

/// Everything of one field the composites need, in field-screen units with a shared origin.
pub struct Scene {
    proj: Projection,
    walk: Walkmesh,
    tris: Vec<[[f32; 3]; 3]>,
    /// Background canvas size and origin (pixel of field-screen (0, 0)).
    w: usize,
    h: usize,
    ox: i32,
    oy: i32,
    back_rgba: Vec<u8>,
    /// Flat front image (layers 2 and 4 with their blending, straight alpha).
    front_rgba: Vec<u8>,
    tiles: Vec<DepthTile>,
    /// Nearest opaque unblended front tile at each pixel: depth, colour and `(layer, id)`.
    tile_z: Vec<f32>,
    tile_color: Vec<[u8; 3]>,
    tile_obj: Vec<Object>,
    camera_range: (i32, i32, i32, i32),
    forward: [f32; 3],
    model_scale: f32,
    /// Triangles reachable from the triangles of the spawn points through the access links.
    reachable: Vec<bool>,
    spawns: Vec<[f32; 3]>,
    field_name: String,
}

pub struct Cloud {
    model: FieldModel,
    pose: Vec<[f32; 16]>,
}

/// Screen-space triangles of Cloud standing at `pos`: `(vertices, indices, texture)` per part.
type ScreenParts<'a> = Vec<(Vec<Option<Vtx>>, &'a [u32], Option<&'a RgbaImage>)>;

fn project_cloud<'a>(scene: &Scene, cloud: &'a Cloud, pos: [f32; 3], facing: f32, zoom: f32) -> (ScreenParts<'a>, [f32; 4]) {
    let place = model_to_field(pos, facing, scene.model_scale);
    let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut parts: ScreenParts = Vec::new();
    for part in &cloud.model.parts {
        let Some(bone) = cloud.pose.get(part.bone_index) else { continue };
        let verts: Vec<Option<Vtx>> = part
            .positions
            .iter()
            .zip(part.colors.iter().zip(part.uvs.iter()))
            .map(|(p, (c, uv))| {
                let s = scene.proj.project(transform_point(&place, transform_point(bone, *p)))?;
                rect = [rect[0].min(s.x), rect[1].min(s.y), rect[2].max(s.x), rect[3].max(s.y)];
                Some(Vtx {
                    x: (s.x + scene.ox as f32) * zoom,
                    y: (s.y + scene.oy as f32) * zoom,
                    z: s.depth,
                    rgb: [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0],
                    uv: *uv,
                })
            })
            .collect();
        parts.push((verts, part.indices.as_slice(), part.texture.as_deref()));
    }
    (parts, rect)
}

fn draw_cloud(raster: &mut Raster, parts: &ScreenParts<'_>) {
    for (verts, indices, texture) in parts {
        for tri in indices.chunks_exact(3) {
            let get = |k: usize| tri.get(k).and_then(|&i| verts.get(i as usize).copied().flatten());
            if let (Some(a), Some(b), Some(c)) = (get(0), get(1), get(2)) {
                raster.triangle([a, b, c], *texture);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Cloud drawn, then the flat front layer over him.
    Flat,
    /// Front tiles in the depth buffer, Cloud tested against them.
    Depth,
    /// As `Depth`, with the front tiles tinted by their relation to Cloud's depth.
    DepthTinted,
}

/// Composite one spot at `zoom` pixels per field-screen pixel over the whole canvas.
fn composite(scene: &Scene, cloud: &Cloud, pos: [f32; 3], facing: f32, zoom: usize, mode: Mode) -> Raster {
    let (w, h) = (scene.w * zoom, scene.h * zoom);
    let mut r = Raster { w, h, rgba: vec![0; w * h * 4], depth: vec![f32::INFINITY; w * h] };
    // the back layers, over opaque black where nothing is drawn
    for y in 0..h {
        for x in 0..w {
            let at = ((y / zoom) * scene.w + x / zoom) * 4;
            let px = scene.back_rgba.get(at..at + 4).unwrap_or(&[0, 0, 0, 0]);
            let k = u16::from(px[3]);
            let c = |v: u8| u8::try_from(u16::from(v) * k / 255).unwrap_or(255);
            r.put(x, y, [c(px[0]), c(px[1]), c(px[2])]);
        }
    }
    let (parts, _) = project_cloud(scene, cloud, pos, facing, zoom as f32);
    if mode == Mode::Flat {
        draw_cloud(&mut r, &parts);
        for y in 0..h {
            for x in 0..w {
                let at = ((y / zoom) * scene.w + x / zoom) * 4;
                if let Some(&[fr, fg, fb, a]) = scene.front_rgba.get(at..at + 4).and_then(|s| <&[u8; 4]>::try_from(s).ok()) {
                    if a == 255 {
                        r.put(x, y, [fr, fg, fb]);
                    }
                }
            }
        }
        return r;
    }
    // opaque front tiles first: colour and depth; Cloud is then tested against that depth
    let cloud_depth = depth_of_point(&scene.proj, pos).unwrap_or(f32::MAX);
    for y in 0..h {
        for x in 0..w {
            let i = (y / zoom) * scene.w + x / zoom;
            let Some(&z) = scene.tile_z.get(i).filter(|z| z.is_finite()) else { continue };
            let mut rgb = scene.tile_color.get(i).copied().unwrap_or([0; 3]);
            if mode == Mode::DepthTinted {
                let tint: [u16; 3] = if z < cloud_depth { [255, 40, 40] } else { [40, 90, 255] };
                rgb = [0, 1, 2].map(|k| ((u16::from(rgb[k]) + tint[k]) / 2) as u8);
            }
            r.put(x, y, rgb);
            if let Some(slot) = r.depth.get_mut(y * w + x) {
                *slot = z;
            }
        }
    }
    draw_cloud(&mut r, &parts);
    // blended tiles last: they pass where they are nearer than what is there (no depth write)
    for t in scene.tiles.iter().filter(|t| t.blend.is_some() && (t.layer == 2 || t.layer == 4)) {
        let Some(mode) = t.blend else { continue };
        let size = t.size() as usize;
        for ty in 0..size {
            for tx in 0..size {
                let Some(texel) = t.pixel(tx as u32, ty as u32).filter(|p| p[3] != 0) else { continue };
                for dy in 0..zoom {
                    for dx in 0..zoom {
                        let (Ok(bx), Ok(by)) = (
                            usize::try_from(t.rect.min_x + tx as i32 + scene.ox),
                            usize::try_from(t.rect.min_y + ty as i32 + scene.oy),
                        ) else {
                            continue;
                        };
                        let (x, y) = (bx * zoom + dx, by * zoom + dy);
                        if x >= w || y >= h || r.depth.get(y * w + x).is_none_or(|d| t.depth > *d) {
                            continue;
                        }
                        let at = (y * w + x) * 4;
                        let back: [u8; 4] = r.rgba.get(at..at + 4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]);
                        let [br, bg, bb] = if back[3] != 0 { [back[0], back[1], back[2]] } else { [0, 0, 0] };
                        r.put(x, y, [mode.apply(br, texel[0]), mode.apply(bg, texel[1]), mode.apply(bb, texel[2])]);
                    }
                }
            }
        }
    }
    let _: BlendMode = BlendMode::Average;
    r
}

/// What the front layer does to Cloud at one spot, pixel by pixel at 1x.
#[derive(Default, Clone, Debug)]
pub struct Verdict {
    /// Cloud's silhouette pixels.
    pub pixels: usize,
    /// Of them, under an opaque pixel of the flat front image (hidden by the flat approach).
    pub flat_hidden: usize,
    /// Of them, hidden by a nearer front tile (hidden under the depth rule).
    pub depth_hidden: usize,
    /// Hidden under the depth rule, by object.
    pub hidden_by: BTreeMap<Object, usize>,
    /// Hidden by the flat layer but not under the depth rule, by object.
    pub freed_by: BTreeMap<Object, usize>,
    /// Hidden by the flat layer, by object (whatever the depth rule says).
    pub flat_by: BTreeMap<Object, usize>,
    /// Cloud's screen rectangle `[min_x, min_y, max_x, max_y]` in field-screen units.
    pub rect: [f32; 4],
    /// Camera depth of the spot.
    pub depth: f32,
    /// Fraction of the rectangle under an opaque pixel of the flat front image (the client's
    /// `front_coverage`).
    pub rect_coverage: f32,
}

impl Verdict {
    /// Pixels of Cloud hidden by object `o` under the depth rule.
    pub fn hidden(&self, o: Object) -> usize {
        self.hidden_by.get(&o).copied().unwrap_or(0)
    }
    /// Pixels of Cloud under object `o` in the flat front image.
    pub fn flat(&self, o: Object) -> usize {
        self.flat_by.get(&o).copied().unwrap_or(0)
    }
    fn fraction(&self, o: Object) -> f32 {
        self.hidden(o) as f32 / self.pixels.max(1) as f32
    }
}

impl Scene {
    /// Whether walkmesh triangle `t` is linked to the triangle of a spawn point.
    pub fn reachable_triangle(&self, t: usize) -> bool {
        self.reachable.get(t).copied().unwrap_or(false)
    }
}

fn rect_coverage_flat(scene: &Scene, rect: [f32; 4]) -> f32 {
    let x0 = (rect[0].floor() as i32 + scene.ox).max(0);
    let y0 = (rect[1].floor() as i32 + scene.oy).max(0);
    let x1 = ((rect[2].ceil() as i32 + scene.ox).max(0) as usize).min(scene.w) as i32;
    let y1 = ((rect[3].ceil() as i32 + scene.oy).max(0) as usize).min(scene.h) as i32;
    let (mut covered, mut total) = (0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            total += 1;
            let at = (y as usize * scene.w + x as usize) * 4 + 3;
            covered += u32::from(scene.front_rgba.get(at).is_some_and(|a| *a != 0));
        }
    }
    if total == 0 {
        0.0
    } else {
        covered as f32 / total as f32
    }
}

fn judge(scene: &Scene, cloud: &Cloud, pos: [f32; 3], facing: f32) -> Option<Verdict> {
    let (parts, rect) = project_cloud(scene, cloud, pos, facing, 1.0);
    let mut r = Raster { w: scene.w, h: scene.h, rgba: vec![0; scene.w * scene.h * 4], depth: vec![f32::INFINITY; scene.w * scene.h] };
    draw_cloud(&mut r, &parts);
    let mut v = Verdict { rect, depth: depth_of_point(&scene.proj, pos)?, rect_coverage: rect_coverage_flat(scene, rect), ..Verdict::default() };
    for (i, z) in r.depth.iter().enumerate() {
        if !z.is_finite() {
            continue;
        }
        v.pixels += 1;
        let tile = scene.tile_obj.get(i).copied().unwrap_or((0, 0));
        let flat = scene.front_rgba.get(i * 4 + 3).is_some_and(|a| *a != 0);
        let tz = scene.tile_z.get(i).copied().unwrap_or(f32::INFINITY);
        if flat {
            v.flat_hidden += 1;
            *v.flat_by.entry(tile).or_default() += 1;
        }
        if tz < *z {
            v.depth_hidden += 1;
            *v.hidden_by.entry(tile).or_default() += 1;
        } else if flat {
            *v.freed_by.entry(tile).or_default() += 1;
        }
    }
    Some(v)
}

// ---- walkmesh helpers -------------------------------------------------------------------------------

fn tri_points(walk: &Walkmesh) -> Vec<[[f32; 3]; 3]> {
    walk.triangles.iter().map(|t| t.map(|v| [f32::from(v.x), f32::from(v.y), f32::from(v.z)])).collect()
}

/// Centroid and edge midpoints of every triangle: the client's candidate set.
fn client_candidates(tris: &[[[f32; 3]; 3]]) -> Vec<[f32; 3]> {
    let mid = |a: [f32; 3], b: [f32; 3]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0];
    let mut out = Vec::new();
    for t in tris {
        out.push([
            (t[0][0] + t[1][0] + t[2][0]) / 3.0,
            (t[0][1] + t[1][1] + t[2][1]) / 3.0,
            (t[0][2] + t[1][2] + t[2][2]) / 3.0,
        ]);
        out.extend([mid(t[0], t[1]), mid(t[1], t[2]), mid(t[2], t[0])]);
    }
    out
}

/// A grid of points over each triangle (`n` steps per edge), with the triangle index.
fn dense_samples(tris: &[[[f32; 3]; 3]], n: usize) -> Vec<(usize, [f32; 3])> {
    let mut out = Vec::new();
    for (ti, t) in tris.iter().enumerate() {
        for i in 0..=n {
            for j in 0..=(n - i) {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                let w = 1.0 - u - v;
                out.push((ti, [0, 1, 2].map(|k| w * t[0][k] + u * t[1][k] + v * t[2][k])));
            }
        }
    }
    out
}

fn inside_xy(t: &[[f32; 3]; 3], p: [f32; 2]) -> bool {
    let s = |a: [f32; 3], b: [f32; 3]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let (d0, d1, d2) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
    (d0 >= -1e-3 && d1 >= -1e-3 && d2 >= -1e-3) || (d0 <= 1e-3 && d1 <= 1e-3 && d2 <= 1e-3)
}

fn reachable_from(walk: &Walkmesh, tris: &[[[f32; 3]; 3]], spawns: &[[f32; 3]]) -> Vec<bool> {
    let mut seen = vec![false; walk.len()];
    let mut stack: Vec<usize> = Vec::new();
    for s in spawns {
        if let Some(ti) = tris.iter().position(|t| inside_xy(t, [s[0], s[1]])) {
            if let Some(slot) = seen.get_mut(ti) {
                if !*slot {
                    *slot = true;
                    stack.push(ti);
                }
            }
        }
    }
    while let Some(t) = stack.pop() {
        for e in 0..3 {
            if let Some(n) = walk.neighbor(t, e) {
                if let Some(slot) = seen.get_mut(n) {
                    if !*slot {
                        *slot = true;
                        stack.push(n);
                    }
                }
            }
        }
    }
    seen
}

// ---- preparing a field -------------------------------------------------------------------------------

/// Load a field and Cloud. `dir` is the game's `data/` directory.
pub fn prepare(dir: &Path, field_name: &str) -> Result<(Scene, Cloud), String> {
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "steam".into(),
        region: "any".into(),
        language: "en".into(),
        revision: "test".into(),
        sources: vec![],
        modded: false,
    };
    let adapter = Ff7Adapter::from_flevel_path(dir.join("field").join("flevel.lgp"), provenance).map_err(|e| e.to_string())?;
    let area = adapter.load_field(field_name).map_err(|e| e.to_string())?;
    let field = area.field_file();
    let cameras = area.cameras().map_err(|e| e.to_string())?;
    let camera = cameras.first().ok_or("the field has no camera")?;
    let walk = field.walkmesh().map_err(|e| e.to_string())?;
    let tris = tri_points(&walk);
    let layers = decode_background_layers(field).map_err(|e| e.to_string())?;
    let tiles = decode_background_tiles_with_depth(field).map_err(|e| e.to_string())?;
    let (w, h) = (layers.back.width as usize, layers.back.height as usize);
    let (ox, oy) = (layers.back.origin_x, layers.back.origin_y);

    // nearest opaque unblended front tile per pixel, painted in the tiles' back-to-front order
    let mut tile_z = vec![f32::INFINITY; w * h];
    let mut tile_color = vec![[0u8; 3]; w * h];
    let mut tile_obj = vec![(0u8, 0u16); w * h];
    for t in tiles.iter().filter(|t| (t.layer == 2 || t.layer == 4) && t.blend.is_none()) {
        for y in 0..t.size() {
            for x in 0..t.size() {
                let Some(p) = t.pixel(x, y).filter(|p| p[3] != 0) else { continue };
                let (Ok(px), Ok(py)) = (usize::try_from(t.rect.min_x + x as i32 + ox), usize::try_from(t.rect.min_y + y as i32 + oy)) else {
                    continue;
                };
                let i = py * w + px;
                if px < w && py < h && tile_z.get(i).is_some_and(|z| t.depth <= *z) {
                    if let (Some(z), Some(c), Some(o)) = (tile_z.get_mut(i), tile_color.get_mut(i), tile_obj.get_mut(i)) {
                        *z = t.depth;
                        *c = [p[0], p[1], p[2]];
                        *o = (t.layer, t.id);
                    }
                }
            }
        }
    }
    let spawns: Vec<[f32; 3]> = area.spawn_points().iter().map(|p| [p.x, p.y, p.z]).collect();
    let scene = Scene {
        proj: Projection::from_camera(camera),
        reachable: reachable_from(&walk, &tris, &spawns),
        walk,
        tris,
        w,
        h,
        ox,
        oy,
        back_rgba: layers.back.rgba.clone(),
        front_rgba: layers.front.rgba.clone(),
        tiles,
        tile_z,
        tile_color,
        tile_obj,
        camera_range: field.triggers().map(|t| t.camera_range.bounds()).unwrap_or((-160, -120, 160, 120)),
        forward: camera.axes_f32()[2],
        model_scale: default_model_scale(area.field_scale()),
        spawns,
        field_name: field_name.to_string(),
    };
    println!(
        "{field_name}: canvas {w}x{h} origin ({ox}, {oy}), {} depth tiles ({} layer 2, {} blended front tiles), {} triangles ({} linked to a spawn point's triangle), camera zoom {}, camera range {:?}, model scale {}",
        scene.tiles.len(),
        scene.tiles.iter().filter(|t| t.layer == 2).count(),
        scene.tiles.iter().filter(|t| t.blend.is_some() && t.layer >= 2).count(),
        scene.walk.len(),
        scene.reachable.iter().filter(|r| **r).count(),
        camera.zoom,
        scene.camera_range,
        scene.model_scale
    );
    // Cloud, idle pose frame 0 (what the client's spot search poses)
    let playable = resolve_appearance("ff7/cloud").ok_or("no Cloud")?;
    let mut char_lgp = LgpArchive::open_path(dir.join("field").join("char.lgp")).map_err(|e| e.to_string())?;
    let model = load_field_model(&mut char_lgp, playable.hrc).map_err(|e| e.to_string())?;
    let idle = load_field_animation(&mut char_lgp, playable.idle_anim).map_err(|e| e.to_string())?;
    let pose = model.pose(&idle, 0).ok_or("the idle animation does not fit Cloud")?;
    Ok((scene, Cloud { model, pose }))
}

// ---- finding the spots ----------------------------------------------------------------------------------

/// A walkmesh spot with what the front layer does to Cloud there.
#[derive(Clone, Debug)]
pub struct Spot {
    pub name: String,
    pub triangle: Option<usize>,
    pub pos: [f32; 3],
    pub verdict: Verdict,
}

#[derive(Default, Debug)]
pub struct Findings {
    /// Candidate places of the client's search (depth 450..1300): how many, how many the flat layer
    /// touches, the dominant object at each touched place.
    pub client_places: usize,
    pub client_touched: usize,
    pub client_blame: Vec<(Object, usize)>,
    /// Cloud's pixels over the client's places: all, under the flat layer, hidden under the depth rule.
    pub client_pixels: (usize, usize, usize),
    pub client_pick: Option<Spot>,
    pub spawns: Vec<Spot>,
    /// Dense samples in the main mesh with Cloud inside the camera range.
    pub usable_samples: usize,
    pub deepest_sample_depth: f32,
    /// Usable samples where each object hides part of Cloud under the depth rule.
    pub hiders: BTreeMap<Object, usize>,
    /// Over the usable samples: Cloud pixels hidden by the flat layer, and of them freed by the rule.
    pub flat_pixels: usize,
    pub freed_pixels: usize,
    pub spots: Vec<Spot>,
}

/// Cloud faces the camera: heading `atan2(-forward_y, -forward_x)` as the client's `toward_camera`.
fn facing_camera(scene: &Scene) -> f32 {
    (-scene.forward[1]).atan2(-scene.forward[0])
}

/// The wall piece of `md1stin`: layer 2, id 544, depth 2176.
pub const WALL: Object = (2, 544);

pub fn find_spots(scene: &Scene, cloud: &Cloud) -> Findings {
    let mut out = Findings::default();
    let facing = facing_camera(scene);
    let depth_of = |p: [f32; 3]| depth_of_point(&scene.proj, p);

    // ---- 1. the client's candidate set (posed with facing 0.0, as the client does) ----
    let in_range: Vec<([f32; 3], Verdict)> = client_candidates(&scene.tris)
        .into_iter()
        .filter_map(|p| {
            let d = depth_of(p)?;
            (450.0..=1300.0).contains(&d).then_some(())?;
            Some((p, judge(scene, cloud, p, 0.0)?))
        })
        .collect();
    // the object whose tiles overlap the character's rectangle most, as the client attributes it
    let dominant = |rect: [f32; 4]| -> Option<Object> {
        let mut by_object: BTreeMap<Object, f32> = BTreeMap::new();
        for t in scene.tiles.iter().filter(|t| (t.layer == 2 || t.layer == 4) && t.param == 0) {
            let (tx0, ty0, tx1, ty1) = (t.rect.min_x as f32, t.rect.min_y as f32, t.rect.max_x as f32, t.rect.max_y as f32);
            let (ww, hh) = ((rect[2].min(tx1) - rect[0].max(tx0)).max(0.0), (rect[3].min(ty1) - rect[1].max(ty0)).max(0.0));
            if ww * hh > 0.0 {
                *by_object.entry((t.layer, t.id)).or_default() += ww * hh;
            }
        }
        by_object.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(o, _)| o)
    };
    let mut blame: BTreeMap<Object, usize> = BTreeMap::new();
    for (_, v) in in_range.iter().filter(|(_, v)| v.rect_coverage > 0.0) {
        if let Some(o) = dominant(v.rect) {
            *blame.entry(o).or_default() += 1;
        }
    }
    out.client_places = in_range.len();
    out.client_touched = in_range.iter().filter(|(_, v)| v.rect_coverage > 0.0).count();
    out.client_blame = blame.into_iter().collect();
    out.client_blame.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    out.client_pixels = in_range.iter().fold((0, 0, 0), |a, (_, v)| (a.0 + v.pixels, a.1 + v.flat_hidden, a.2 + v.depth_hidden));
    // the client's pick: coverage in (0.2, 0.9) closest to 0.5, preferring the dominant object (2, 1)
    let pick = |filter: &dyn Fn(&Verdict) -> bool| {
        in_range
            .iter()
            .filter(|(_, v)| filter(v))
            .min_by(|a, b| (a.1.rect_coverage - 0.5).abs().total_cmp(&(b.1.rect_coverage - 0.5).abs()))
    };
    let mid = |v: &Verdict| v.rect_coverage > 0.2 && v.rect_coverage < 0.9;
    if let Some((p, _)) = pick(&|v| mid(v) && dominant(v.rect) == Some((2, 1))).or_else(|| pick(&|v| mid(v))) {
        // judged again with Cloud turned to the camera, as in the client's screenshot
        if let Some(v) = judge(scene, cloud, *p, facing) {
            out.client_pick = Some(Spot { name: "counterexample".into(), triangle: None, pos: *p, verdict: v });
        }
    }
    for (i, s) in scene.spawns.iter().enumerate() {
        if let Some(v) = judge(scene, cloud, *s, facing) {
            out.spawns.push(Spot { name: format!("spawn{i}"), triangle: None, pos: *s, verdict: v });
        }
    }

    // ---- 2-4. dense scan of the walkmesh for the object spots ----
    // A sample counts only if its triangle is linked to a spawn point's triangle (the main mesh)
    // and Cloud's whole rectangle is inside the camera range.
    let (rx0, ry0, rx1, ry1) = scene.camera_range;
    let usable: Vec<(usize, [f32; 3], Verdict)> = dense_samples(&scene.tris, 10)
        .into_iter()
        .filter_map(|(ti, p)| {
            if depth_of(p)? < 300.0 || !scene.reachable.get(ti).copied().unwrap_or(false) {
                return None;
            }
            let v = judge(scene, cloud, p, facing)?;
            let inside = v.rect[0] >= rx0 as f32 && v.rect[1] >= ry0 as f32 && v.rect[2] <= rx1 as f32 && v.rect[3] <= ry1 as f32;
            inside.then_some((ti, p, v))
        })
        .collect();
    out.usable_samples = usable.len();
    out.deepest_sample_depth = usable.iter().map(|(_, _, v)| v.depth).fold(0.0, f32::max);
    for (_, _, v) in &usable {
        for o in v.hidden_by.keys() {
            *out.hiders.entry(*o).or_default() += 1;
        }
        out.flat_pixels += v.flat_hidden;
        out.freed_pixels += v.flat_hidden.saturating_sub(v.depth_hidden);
    }
    let wall_depth = 4.0 * f32::from(WALL.1);
    let mut add = |name: &str, found: Option<&(usize, [f32; 3], Verdict)>| {
        if let Some((ti, p, v)) = found {
            out.spots.push(Spot { name: name.to_string(), triangle: Some(*ti), pos: *p, verdict: v.clone() });
        }
    };
    add("behind_wall_544_hidden", usable.iter().filter(|(_, _, v)| v.fraction(WALL) > 0.9).max_by_key(|(_, _, v)| v.pixels));
    add(
        "behind_wall_544_partly",
        usable.iter().filter(|(_, _, v)| (0.25..=0.9).contains(&v.fraction(WALL))).max_by_key(|(_, _, v)| v.pixels),
    );
    add(
        "in_front_of_wall_544",
        usable
            .iter()
            .filter(|(_, _, v)| v.depth < wall_depth && (450.0..=1300.0).contains(&v.depth) && v.hidden(WALL) == 0)
            .max_by_key(|(_, _, v)| v.flat(WALL)),
    );
    add(
        "in_front_of_wall_544_near",
        usable.iter().filter(|(_, _, v)| v.depth < wall_depth && v.hidden(WALL) == 0).max_by_key(|(_, _, v)| v.flat(WALL)),
    );
    for (name, o) in [("partly_behind_152", (2u8, 152u16)), ("partly_behind_294", (2, 294)), ("partly_behind_train_1", (2, 1))] {
        add(
            name,
            usable
                .iter()
                .filter(|(_, _, v)| (0.25..=0.9).contains(&v.fraction(o)) && (450.0..=1300.0).contains(&v.depth))
                .max_by_key(|(_, _, v)| v.pixels),
        );
    }
    out
}

pub fn print_findings(scene: &Scene, f: &Findings) {
    println!(
        "FLAT FRONT LAYER REACH: it covers part of Cloud's rectangle at {} of {} candidate places in depth 450..1300; dominant object (layer, id): {:?}",
        f.client_touched, f.client_places, f.client_blame
    );
    println!(
        "over those places Cloud has {} pixels: {} are under the flat front layer, {} are hidden under the depth rule ({:.0}% of the flat layer's)",
        f.client_pixels.0,
        f.client_pixels.1,
        f.client_pixels.2,
        100.0 * f.client_pixels.2 as f64 / f.client_pixels.1.max(1) as f64
    );
    if let Some(s) = &f.client_pick {
        let v = &s.verdict;
        println!(
            "client counter-example spot {:?}: depth {:.0}, rectangle coverage {:.2}; Cloud's {} pixels: flat layer hides {} ({:?}), depth rule hides {} ({:?})",
            s.pos, v.depth, v.rect_coverage, v.pixels, v.flat_hidden, v.flat_by, v.depth_hidden, v.hidden_by
        );
    }
    for s in &f.spawns {
        let v = &s.verdict;
        println!(
            "{} {:?}: depth {:.0}; flat layer hides {}/{} px, depth rule hides {} px ({:?})",
            s.name, s.pos, v.depth, v.flat_hidden, v.pixels, v.depth_hidden, v.hidden_by
        );
    }
    println!(
        "{} usable dense walkmesh samples (main mesh, Cloud inside the camera range {:?}); deepest at depth {:.0}; objects that hide Cloud somewhere: {:?}",
        f.usable_samples, scene.camera_range, f.deepest_sample_depth, f.hiders
    );
    println!(
        "over the usable samples the depth rule frees {} of the {} Cloud pixels the flat front layer hides ({:.0}%)",
        f.freed_pixels,
        f.flat_pixels,
        100.0 * f.freed_pixels as f64 / f.flat_pixels.max(1) as f64
    );
    for s in &f.spots {
        let v = &s.verdict;
        println!(
            "{}: spot {:?} on triangle {:?} (linked to a spawn point's triangle: {}), depth {:.0}; Cloud has {} px: flat layer hides {} ({:?}), depth rule hides {} ({:?})",
            s.name,
            s.pos,
            s.triangle,
            s.triangle.is_some_and(|t| scene.reachable.get(t).copied().unwrap_or(false)),
            v.depth,
            v.pixels,
            v.flat_hidden,
            v.flat_by,
            v.depth_hidden,
            v.hidden_by
        );
    }
}

// ---- pictures ---------------------------------------------------------------------------------------------

pub fn render_spots(scene: &Scene, cloud: &Cloud, f: &Findings) -> Result<(), String> {
    let facing = facing_camera(scene);
    let all: Vec<&Spot> = f.client_pick.iter().chain(f.spots.iter()).collect();
    for spot in all {
        let (_, rect) = project_cloud(scene, cloud, spot.pos, facing, 1.0);
        let height = (rect[3] - rect[1]).max(1.0);
        // a tiny Cloud is drawn larger so that the hidden part can be seen
        let zoom = ((200.0 / height).round() as usize).clamp(3, 8);
        let half = (height * 1.3).clamp(45.0, 75.0);
        let flat = composite(scene, cloud, spot.pos, facing, zoom, Mode::Flat);
        let depth = composite(scene, cloud, spot.pos, facing, zoom, Mode::Depth);
        let tinted = composite(scene, cloud, spot.pos, facing, zoom, Mode::DepthTinted);
        let (cx, cy) = ((rect[0] + rect[2]) / 2.0 + scene.ox as f32, (rect[1] + rect[3]) / 2.0 + scene.oy as f32);
        let x0 = (((cx - half) * zoom as f32).max(0.0) as usize).min(flat.w.saturating_sub(1));
        let y0 = (((cy - half) * zoom as f32).max(0.0) as usize).min(flat.h.saturating_sub(1));
        let side = (2.0 * half) as usize * zoom;
        let (cw, ch) = (side.min(flat.w - x0), side.min(flat.h - y0));
        let mut sheet = vec![0u8; cw * 3 * ch * 4];
        for (k, img) in [&flat, &depth, &tinted].into_iter().enumerate() {
            for y in 0..ch {
                let src = ((y0 + y) * img.w + x0) * 4;
                let dst = (y * cw * 3 + k * cw) * 4;
                if let (Some(s), Some(d)) = (img.rgba.get(src..src + cw * 4), sheet.get_mut(dst..dst + cw * 4)) {
                    d.copy_from_slice(s);
                }
            }
        }
        let base = format!("depth_{}_{}", scene.field_name, spot.name);
        write_png(&Path::new(OUT_DIR).join(format!("{base}.png")), cw * 3, ch, &sheet)?;
        write_png(&Path::new(OUT_DIR).join(format!("{base}_full.png")), depth.w, depth.h, &depth.rgba)?;
    }
    Ok(())
}

// ---- main ---------------------------------------------------------------------------------------------

fn main() {
    let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut field_name = "md1stin".to_string();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match (flag.as_str(), it.next()) {
            ("--field", Some(v)) => field_name = v.clone(),
            _ => {
                eprintln!("usage: depth_composite [--field <name>]");
                return;
            }
        }
    }
    let result = std::fs::create_dir_all(OUT_DIR).map_err(|e| format!("cannot create {OUT_DIR}: {e}")).and_then(|()| {
        let (scene, cloud) = prepare(&PathBuf::from(dir), &field_name)?;
        let findings = find_spots(&scene, &cloud);
        print_findings(&scene, &findings);
        render_spots(&scene, &cloud, &findings)
    });
    if let Err(e) = result {
        eprintln!("depth_composite: {e}");
        std::process::exit(1);
    }
}
