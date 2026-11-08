//! Verification render of a field character model (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example model_dump -j16
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example model_dump -j16 -- \
//!     --appearance ff7/tifa --field md1stin --anim walk --frame 7 --scale 4 --facing-deg -70
//! ```
//!
//! Options (all optional): `--appearance <id>` (a [`fmv_ff7::playable`] id, default `ff7/cloud`),
//! `--field <name>` (default `md1stin`), `--anim idle|walk|run` (the pose drawn in the field,
//! default `walk`), `--frame <n>` (default 7), `--scale <walkmesh units per model unit>` (default
//! [`fmv_ff7::model::DEFAULT_MODEL_SCALE_UNITS`]), `--facing-deg <degrees>` (heading in the field,
//! default -70: towards the camera, a little to the side), `--spawn <index>` (default: the spawn
//! point nearest to the camera, which gives the largest picture), `--at
//! <x>,<y>` (stand at this walkmesh position instead of a spawn point; the height is looked up),
//! `--zoom <n>` (pixel upscaling of the field picture, default 3).
//!
//! It writes, to `/home/blizz/.cache/claude-tmp/ffmv-ff7-dump/` (outside the repository; the
//! pictures contain game art, never commit or share them):
//!
//! * `<field>_<model>.png` (for example `md1stin_cloud.png`): the model in the chosen pose standing
//!   on a spawn point of the field, projected through the field's own camera and drawn between the
//!   background's back layers (1, 3) and front layers (2, 4), with a z-buffer, vertex colours and
//!   textures. If the front layers hold blended tiles (shadows), which a flat front image cannot
//!   show correctly, the model is drawn over the full composite instead. Also
//!   `<field>_<model>_closeup.png` (a crop around the model) and `<field>_<model>_over_all.png` /
//!   `_over_all_closeup.png` (the model drawn over the full composite of all layers, never hidden
//!   behind a layer-2 object).
//! * `<model>_bind.png`: the idle pose seen from the front (the character's left on the right of the
//!   picture), orthographic. HRC files store no angles, so the "bind pose" is frame 0 of the idle
//!   animation. Also `<model>_bind_views.png` (front, side from the left, back) and
//!   `<model>_walk_strip.png` (eight frames of the walk, side view).
//!
//! Everything is drawn by a small software rasterizer in this file (no GPU, no new dependencies).
// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]
use fmv_adapter_api::AreaRuntime;
use fmv_core::{EditionProvenance, GameId, Platform};
use fmv_ff7::background::{decode_background, decode_background_layers, Projection};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::model::{
    default_model_scale, load_field_animation, load_field_model, model_to_field, transform_point, FieldModel, MeshPart,
};
use fmv_ff7::playable::resolve_appearance;
use fmv_ff7::{Ff7Adapter, FieldAnimation, RgbaImage};
use std::path::{Path, PathBuf};

const OUT_DIR: &str = "/home/blizz/.cache/claude-tmp/ffmv-ff7-dump";

// ----- rasterizer -----------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Vtx {
    x: f32,
    y: f32,
    /// Depth: smaller is nearer.
    z: f32,
    rgb: [f32; 3],
    uv: [f32; 2],
}

struct Canvas {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
    depth: Vec<f32>,
}

impl Canvas {
    fn solid(w: usize, h: usize, fill: [u8; 4]) -> Self {
        let rgba = fill.iter().copied().cycle().take(w * h * 4).collect();
        Self { w, h, rgba, depth: vec![f32::INFINITY; w * h] }
    }

    /// Nearest-neighbour upscaling of an RGBA picture by `zoom`.
    fn upscaled(w: usize, h: usize, rgba: &[u8], zoom: usize) -> Self {
        let (ow, oh) = (w * zoom, h * zoom);
        let mut out = Vec::with_capacity(ow * oh * 4);
        for y in 0..oh {
            for x in 0..ow {
                let at = ((y / zoom) * w + x / zoom) * 4;
                out.extend_from_slice(rgba.get(at..at + 4).unwrap_or(&[0, 0, 0, 255]));
            }
        }
        Self { w: ow, h: oh, rgba: out, depth: vec![f32::INFINITY; ow * oh] }
    }

    fn put(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let at = (y * self.w + x) * 4;
        if let Some(px) = self.rgba.get_mut(at..at + 4) {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }

    /// Draw one triangle with depth test (`<=`, with a small bias so coplanar overlays such as the
    /// face polygons win over the skin they lie on, as with the game's less-or-equal depth test).
    /// No culling. Colour is the interpolated vertex colour, modulated by the texture if given;
    /// texels with alpha below 128 are discarded.
    fn triangle(&mut self, v: [Vtx; 3], texture: Option<&RgbaImage>) {
        let [a, b, c] = v;
        let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if area.abs() < 1e-9 || !area.is_finite() {
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
                let z = w0 * a.z + w1 * b.z + w2 * c.z;
                let idx = y * self.w + x;
                let Some(slot) = self.depth.get_mut(idx) else { continue };
                if z > *slot + 1e-3 {
                    continue;
                }
                let mix = |f: fn(&Vtx) -> f32| w0 * f(&a) + w1 * f(&b) + w2 * f(&c);
                let mut rgb = [mix(|p| p.rgb[0]), mix(|p| p.rgb[1]), mix(|p| p.rgb[2])];
                if let Some(tex) = texture {
                    let texel = tex.sample(mix(|p| p.uv[0]), mix(|p| p.uv[1]));
                    if texel[3] < 128 {
                        continue;
                    }
                    for (k, c) in rgb.iter_mut().enumerate() {
                        *c *= f32::from(texel.get(k).copied().unwrap_or(0)) / 255.0;
                    }
                }
                *slot = z;
                self.put(x, y, rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
            }
        }
    }

    /// Alpha-over `front` (a picture of `fw` x `fh` pixels, scaled up by `zoom`).
    fn over(&mut self, fw: usize, fh: usize, front: &[u8], zoom: usize) {
        for y in 0..self.h {
            for x in 0..self.w {
                let at = ((y / zoom).min(fh.saturating_sub(1)) * fw + (x / zoom).min(fw.saturating_sub(1))) * 4;
                if let Some([r, g, b, a]) = front.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()) {
                    if a == 255 {
                        self.put(x, y, [r, g, b]);
                    }
                }
            }
        }
    }

    fn crop(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (usize, usize, Vec<u8>) {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        let (x0, y0) = (x0.min(x1), y0.min(y1));
        let mut out = Vec::new();
        for y in y0..y1 {
            let at = (y * self.w + x0) * 4;
            out.extend_from_slice(self.rgba.get(at..at + (x1 - x0) * 4).unwrap_or(&[]));
        }
        (x1 - x0, y1 - y0, out)
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

/// Draw every part of `model` in `pose`: `to_screen` maps a point in native model space to
/// `(x, y, depth)` or `None`. Light is flat white (the vertex colours carry the shading).
fn draw_model(
    canvas: &mut Canvas,
    model: &FieldModel,
    pose: &[[f32; 16]],
    to_screen: &dyn Fn([f32; 3]) -> Option<[f32; 3]>,
) {
    for part in &model.parts {
        let Some(bone) = pose.get(part.bone_index) else { continue };
        draw_part(canvas, part, bone, to_screen);
    }
}

fn draw_part(canvas: &mut Canvas, part: &MeshPart, bone: &[f32; 16], to_screen: &dyn Fn([f32; 3]) -> Option<[f32; 3]>) {
    let vtx: Vec<Option<Vtx>> = part
        .positions
        .iter()
        .zip(part.colors.iter().zip(part.uvs.iter()))
        .map(|(p, (c, uv))| {
            let s = to_screen(transform_point(bone, *p))?;
            Some(Vtx {
                x: s[0],
                y: s[1],
                z: s[2],
                rgb: [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0],
                uv: *uv,
            })
        })
        .collect();
    for tri in part.indices.chunks_exact(3) {
        let get = |k: usize| tri.get(k).and_then(|&i| vtx.get(i as usize).copied().flatten());
        if let (Some(a), Some(b), Some(c)) = (get(0), get(1), get(2)) {
            canvas.triangle([a, b, c], part.texture.as_deref());
        }
    }
}

// ----- views ------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum View {
    Front,
    SideLeft,
    Back,
}

/// Orthographic picture of the native model space: y points down in native space, which is also
/// the picture's direction. Front: viewer in front of the character (looking along +z), so the
/// character's left (+x) is on the right of the picture.
fn ortho(view: View, centre: [f32; 3], scale: f32, size: usize) -> impl Fn([f32; 3]) -> Option<[f32; 3]> {
    move |p: [f32; 3]| {
        let (u, v, depth) = match view {
            View::Front => (p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]),
            View::Back => (-(p[0] - centre[0]), p[1] - centre[1], -(p[2] - centre[2])),
            View::SideLeft => (p[2] - centre[2], p[1] - centre[1], -(p[0] - centre[0])),
        };
        let half = size as f32 / 2.0;
        Some([half + u * scale, half + v * scale, depth])
    }
}

fn render_views(model: &FieldModel, pose: &[[f32; 16]], views: &[View], size: usize) -> (usize, usize, Vec<u8>) {
    let identity = fmv_ff7::model::mat4_identity();
    let (lo, hi) = model.posed_bounds(pose, &identity).unwrap_or(([0.0; 3], [1.0; 3]));
    let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]).max(1e-3);
    let scale = size as f32 * 0.92 / extent;
    let mut sheet = Canvas::solid(size * views.len(), size, [30, 30, 40, 255]);
    for (i, view) in views.iter().enumerate() {
        let mut panel = Canvas::solid(size, size, [30, 30, 40, 255]);
        let f = ortho(*view, centre, scale, size);
        draw_model(&mut panel, model, pose, &f);
        for y in 0..size {
            let from = panel.rgba.get(y * size * 4..(y + 1) * size * 4).unwrap_or(&[]).to_vec();
            let at = (y * sheet.w + i * size) * 4;
            if let Some(dst) = sheet.rgba.get_mut(at..at + from.len()) {
                dst.copy_from_slice(&from);
            }
        }
    }
    (sheet.w, sheet.h, sheet.rgba)
}

// ----- main -------------------------------------------------------------------------------------

struct Options {
    appearance: String,
    field: String,
    anim: String,
    frame: usize,
    scale: Option<f32>,
    facing_deg: f32,
    spawn: Option<usize>,
    at: Option<(f32, f32)>,
    zoom: usize,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        appearance: "ff7/cloud".into(),
        field: "md1stin".into(),
        anim: "walk".into(),
        frame: 7,
        scale: None,
        facing_deg: -70.0,
        spawn: None,
        at: None,
        zoom: 3,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--appearance" => o.appearance = value()?,
            "--field" => o.field = value()?,
            "--anim" => o.anim = value()?,
            "--frame" => o.frame = value()?.parse().map_err(|e| format!("--frame: {e}"))?,
            "--scale" => o.scale = Some(value()?.parse().map_err(|e| format!("--scale: {e}"))?),
            "--facing-deg" => o.facing_deg = value()?.parse().map_err(|e| format!("--facing-deg: {e}"))?,
            "--spawn" => o.spawn = Some(value()?.parse().map_err(|e| format!("--spawn: {e}"))?),
            "--at" => {
                let v = value()?;
                let (x, y) = v.split_once(',').ok_or("--at needs x,y")?;
                o.at = Some((
                    x.trim().parse().map_err(|e| format!("--at x: {e}"))?,
                    y.trim().parse().map_err(|e| format!("--at y: {e}"))?,
                ));
            }
            "--zoom" => o.zoom = value()?.parse::<usize>().map_err(|e| format!("--zoom: {e}"))?.clamp(1, 6),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

fn main() {
    let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let options = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    if let Err(e) = run(&PathBuf::from(dir), &options) {
        eprintln!("model_dump: {e}");
        std::process::exit(1);
    }
}

fn run(dir: &Path, o: &Options) -> Result<(), String> {
    std::fs::create_dir_all(OUT_DIR).map_err(|e| format!("cannot create {OUT_DIR}: {e}"))?;
    let playable = resolve_appearance(&o.appearance).ok_or_else(|| format!("unknown appearance {:?}", o.appearance))?;
    let short = o.appearance.rsplit('/').next().unwrap_or("model").to_string();
    let mut char_lgp = LgpArchive::open_path(dir.join("field").join("char.lgp")).map_err(|e| e.to_string())?;
    let model = load_field_model(&mut char_lgp, playable.hrc).map_err(|e| e.to_string())?;
    println!(
        "{} ({}): skeleton {:?}, {} bones, {} parts, {} triangles, {} dropped, missing textures {:?}",
        playable.appearance_ref,
        playable.display_name,
        model.skeleton_name,
        model.bones.len(),
        model.parts.len(),
        model.triangle_count(),
        model.dropped_triangles,
        model.missing_textures
    );
    for part in &model.parts {
        if part.texture_name.is_some() || part.blend != fmv_ff7::PartBlend::Opaque {
            println!(
                "  part on bone {} ({}): {part:?}",
                part.bone_index,
                model.bones.get(part.bone_index).map_or("?", |b| b.name.as_str())
            );
        }
    }
    let load = |l: &mut LgpArchive<std::fs::File>, stem: &str| -> Result<FieldAnimation, String> {
        let a = load_field_animation(l, stem).map_err(|e| format!("{stem}: {e}"))?;
        println!(
            "animation {stem}: {} frames, {} bones, fits {}: {}",
            a.frame_count(),
            a.bone_count(),
            model.hrc_name,
            model.fits(&a)
        );
        Ok(a)
    };
    let idle = load(&mut char_lgp, playable.idle_anim)?;
    let walk = load(&mut char_lgp, playable.walk_anim)?;
    let run = load(&mut char_lgp, playable.run_anim)?;
    let chosen = match o.anim.as_str() {
        "idle" => &idle,
        "run" => &run,
        _ => &walk,
    };

    // ---- bind pose views
    let idle_pose = model.pose(&idle, 0).ok_or("the idle animation does not fit the model")?;
    let (w, h, rgba) = render_views(&model, &idle_pose, &[View::Front], 640);
    write_png(&Path::new(OUT_DIR).join(format!("{short}_bind.png")), w, h, &rgba)?;
    let (w, h, rgba) = render_views(&model, &idle_pose, &[View::Front, View::SideLeft, View::Back], 420);
    write_png(&Path::new(OUT_DIR).join(format!("{short}_bind_views.png")), w, h, &rgba)?;
    // eight walk frames, side view, one shared framing (that of the idle pose)
    let panel_size = 300;
    let identity = fmv_ff7::model::mat4_identity();
    let (lo, hi) = model.posed_bounds(&idle_pose, &identity).ok_or("the model has no vertices")?;
    let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]).max(1e-3);
    let side = ortho(View::SideLeft, centre, panel_size as f32 * 0.92 / extent, panel_size);
    let mut strip = Canvas::solid(panel_size * 8, panel_size, [30, 30, 40, 255]);
    for i in 0..8 {
        let pose = model.pose(&walk, i * walk.frame_count() / 8).ok_or("the walk animation does not fit the model")?;
        let mut panel = Canvas::solid(panel_size, panel_size, [30, 30, 40, 255]);
        draw_model(&mut panel, &model, &pose, &side);
        for y in 0..panel_size {
            let from = panel.rgba.get(y * panel_size * 4..(y + 1) * panel_size * 4).unwrap_or(&[]).to_vec();
            let at = (y * strip.w + i * panel_size) * 4;
            if let Some(dst) = strip.rgba.get_mut(at..at + from.len()) {
                dst.copy_from_slice(&from);
            }
        }
    }
    write_png(&Path::new(OUT_DIR).join(format!("{short}_walk_strip.png")), strip.w, strip.h, &strip.rgba)?;

    // ---- the field render
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
    let adapter =
        Ff7Adapter::from_flevel_path(dir.join("field").join("flevel.lgp"), provenance).map_err(|e| e.to_string())?;
    let area = adapter.load_field(&o.field).map_err(|e| e.to_string())?;
    let spawns = area.spawn_points();
    let cameras = area.cameras().map_err(|e| e.to_string())?;
    let camera = cameras.first().ok_or("the field has no camera")?;
    let projection = Projection::from_camera(camera);
    let spawn = match (o.at, o.spawn) {
        (Some((x, y)), _) => {
            let z = area.height_at(x, y, None).ok_or("--at: that point is not on the walkmesh")?;
            fmv_core::Pos::new(x, y, z)
        }
        (None, Some(i)) => spawns.get(i).copied().ok_or("--spawn: no such spawn point")?,
        (None, None) => spawns
            .iter()
            .copied()
            .filter_map(|p| projection.project([p.x, p.y, p.z]).map(|s| (s.depth, p)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p)
            .or(spawns.first().copied())
            .ok_or("the field has no spawn point")?,
    };
    let layers = decode_background_layers(area.field_file()).map_err(|e| e.to_string())?;
    let scale = o.scale.unwrap_or_else(|| default_model_scale(area.field_scale()));
    let facing = o.facing_deg.to_radians();
    println!(
        "field {}: scale {}, camera zoom {}, spawn {:?}, model scale {scale}, facing {} deg, pose {:?} frame {}",
        o.field,
        area.field_scale(),
        camera.zoom,
        spawn,
        o.facing_deg,
        o.anim,
        o.frame
    );
    let pose = model.pose(chosen, o.frame).ok_or("the chosen animation does not fit the model")?;
    let place = model_to_field([spawn.x, spawn.y, spawn.z], facing, scale);
    let (bw, bh) = (layers.back.width as usize, layers.back.height as usize);
    let zoom = o.zoom;
    let to_pixels = |p: [f32; 3]| -> Option<[f32; 3]> {
        let s = projection.project(transform_point(&place, p))?;
        let (px, py) = layers.back.screen_to_pixel(s.x, s.y);
        Some([px * zoom as f32, py * zoom as f32, s.depth])
    };
    // the on-screen size of the model (field-screen units, i.e. pixels of the 320x240 screen)
    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    for part in &model.parts {
        if let Some(bone) = pose.get(part.bone_index) {
            for p in &part.positions {
                if let Some(s) = projection.project(transform_point(&place, transform_point(bone, *p))) {
                    min = [min[0].min(s.x), min[1].min(s.y)];
                    max = [max[0].max(s.x), max[1].max(s.y)];
                }
            }
        }
    }
    println!(
        "model on screen: {:.0} x {:.0} field-screen pixels (the logical screen is 320 x 240)",
        max[0] - min[0],
        max[1] - min[1]
    );

    // Variant 1 draws the model between the back and front layers. The front half cannot show
    // blended tiles (shadows, glows) correctly, so if it has any the model is drawn over the full
    // composite instead and nothing is drawn over it. Variant 2 is always the model over the full
    // composite ("over all": never hidden behind a layer-2 object).
    let full = decode_background(area.field_file()).map_err(|e| e.to_string())?.flatten_on_black();
    let front_blended = layers.front.layers.iter().any(|l| l.tiles_blended > 0);
    if front_blended {
        println!("the front layers contain blended tiles, so the model is drawn over the full composite, not between the layers");
    }
    let back_rgba = layers.back.flatten_on_black();
    for (suffix, between_layers) in [("", !front_blended), ("_over_all", false)] {
        let mut canvas = Canvas::upscaled(bw, bh, if between_layers { &back_rgba } else { &full }, zoom);
        draw_model(&mut canvas, &model, &pose, &to_pixels);
        if between_layers {
            canvas.over(bw, bh, &layers.front.rgba, zoom);
        }
        write_png(
            &Path::new(OUT_DIR).join(format!("{}_{short}{suffix}.png", o.field)),
            canvas.w,
            canvas.h,
            &canvas.rgba,
        )?;
        // a crop around the model: 100 x 120 field-screen pixels
        let (cx, cy) = (f32::midpoint(min[0], max[0]), f32::midpoint(min[1], max[1]));
        let (px, py) = layers.back.screen_to_pixel(cx, cy);
        let (hw, hh) = (50.0 * zoom as f32, 60.0 * zoom as f32);
        let x0 = (px * zoom as f32 - hw).max(0.0) as usize;
        let y0 = (py * zoom as f32 - hh).max(0.0) as usize;
        let (w, h, rgba) = canvas.crop(x0, y0, x0 + (2.0 * hw) as usize, y0 + (2.0 * hh) as usize);
        write_png(&Path::new(OUT_DIR).join(format!("{}_{short}{suffix}_closeup.png", o.field)), w, h, &rgba)?;
    }
    Ok(())
}
