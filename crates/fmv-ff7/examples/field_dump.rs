//! Verification dump of real FF7 PC fields (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example field_dump -j8 -- md1stin [md1_1 ...]
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example field_dump -j8 -- --survey
//! ```
//!
//! For each named field it prints a summary (counts and sizes only, never raw bytes) and writes
//! `<field>_bg.png` (composited background), `<field>_walk.png` (walkmesh seen from above) and
//! `<field>_overlay.png` (walkmesh projected through the camera onto the background) to
//! `/home/blizz/.cache/claude-tmp/ffmv-ff7-dump/`, outside the repository. The PNGs contain
//! game art: never commit or share them.
// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]
use fmv_ff7::background::{decode_background, BackgroundImage, Projection};
use fmv_ff7::field::{FieldFile, Triggers, Walkmesh, NO_ACCESS, SECTION_NAMES};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use std::path::{Path, PathBuf};

const OUT_DIR: &str = "/home/blizz/.cache/claude-tmp/ffmv-ff7-dump";

fn main() {
    let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: field_dump <field>... | --survey");
        return;
    }
    let path = PathBuf::from(dir).join("field").join("flevel.lgp");
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    println!("flevel.lgp: {} entries, {} bytes", lgp.len(), lgp.archive_len());
    if args.first().map(String::as_str) == Some("--survey") {
        survey(&mut lgp);
        return;
    }
    if let Err(e) = std::fs::create_dir_all(OUT_DIR) {
        eprintln!("cannot create {OUT_DIR}: {e}");
        return;
    }
    for name in &args {
        if let Err(e) = dump_field(&mut lgp, name) {
            eprintln!("{name}: {e}");
        }
    }
}

fn dump_field(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Result<(), String> {
    println!("=== {name}");
    let raw = lgp.read(name).map_err(|e| e.to_string())?;
    let header = raw.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes).unwrap_or(0);
    println!("stored {} bytes, lzss header {header}", raw.len());
    let out = lzss::decompress(&raw).map_err(|e| e.to_string())?;
    println!("decompressed {} bytes", out.len());
    let field = FieldFile::parse(out).map_err(|e| e.to_string())?;
    for (i, section) in SECTION_NAMES.iter().enumerate() {
        let declared = field.declared_section_len(i).unwrap_or(0);
        let ok = field.section(i).is_ok();
        println!("  section {i} {section:<13} declared {declared:>7} {}", if ok { "ok" } else { "BAD" });
    }

    let walk = field.walkmesh().map_err(|e| e.to_string())?;
    summarize_walkmesh(&walk);
    let cameras = field.cameras().map_err(|e| e.to_string())?;
    println!("cameras: {}", cameras.len());
    for c in &cameras {
        println!("  axes {:?} position {:?} zoom {}", c.axes, c.position, c.zoom);
    }
    let triggers = field.triggers().map_err(|e| e.to_string())?;
    println!(
        "triggers: name {:?} range {:?} gateways in use {} triggers in use {}",
        triggers.name,
        triggers.camera_range,
        triggers.gateways.iter().filter(|g| g.target_field_id != 0x7FFF).count(),
        triggers.triggers.iter().filter(|t| t.corner_a != [0; 3] || t.corner_b != [0; 3]).count()
    );
    let loader = field.model_loader_header().map_err(|e| e.to_string())?;
    println!("model loader: {} models, field scale {} (speed factor {:.3})", loader.model_count, loader.scale, loader.scale_factor());
    let palette = field.palette().map_err(|e| e.to_string())?;
    println!("palette: {palette:?}");
    let bg = field.background().map_err(|e| e.to_string())?;
    println!("background: {bg:?}");

    let image = decode_background(&field).map_err(|e| e.to_string())?;
    println!("composite: {}x{} origin ({}, {})", image.width, image.height, image.origin_x, image.origin_y);
    for l in &image.layers {
        println!(
            "  layer {}: tile {} header {}x{} total {} drawn {} blended {} hidden {} unrenderable {} bounds {:?} groups {:?}",
            l.layer,
            l.tile_size,
            l.header_width,
            l.header_height,
            l.tiles_total,
            l.tiles_drawn,
            l.tiles_blended,
            l.tiles_hidden,
            l.tiles_unrenderable,
            l.bounds,
            l.groups
        );
    }

    let bg_path = Path::new(OUT_DIR).join(format!("{name}_bg.png"));
    write_png(&bg_path, image.width, image.height, &image.flatten_on_black())?;
    println!("wrote {}", bg_path.display());

    let (w, h, walk_rgba) = render_walkmesh(&walk);
    let walk_path = Path::new(OUT_DIR).join(format!("{name}_walk.png"));
    write_png(&walk_path, w, h, &walk_rgba)?;
    println!("wrote {}", walk_path.display());

    if let Some(camera) = cameras.first() {
        let overlay = render_overlay(&image, &walk, &triggers, &Projection::from_camera(camera));
        let overlay_path = Path::new(OUT_DIR).join(format!("{name}_overlay.png"));
        write_png(&overlay_path, image.width, image.height, &overlay)?;
        println!("wrote {}", overlay_path.display());
    }
    Ok(())
}

fn summarize_walkmesh(w: &Walkmesh) {
    let mut mn = [i16::MAX; 3];
    let mut mx = [i16::MIN; 3];
    for t in &w.triangles {
        for v in t {
            for (k, c) in [v.x, v.y, v.z].into_iter().enumerate() {
                if let (Some(lo), Some(hi)) = (mn.get_mut(k), mx.get_mut(k)) {
                    *lo = (*lo).min(c);
                    *hi = (*hi).max(c);
                }
            }
        }
    }
    let (mut blocked, mut out_of_range) = (0, 0);
    for a in &w.access {
        for &e in a {
            if e == NO_ACCESS {
                blocked += 1;
            } else if usize::from(e) >= w.len() {
                out_of_range += 1;
            }
        }
    }
    println!("walkmesh: {} triangles, vertex min {mn:?} max {mx:?}, edges blocked {blocked}, access out of range {out_of_range}", w.len());
}

// ----- PNG + tiny raster helpers ---------------------------------------------------------

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

struct Raster {
    w: i32,
    h: i32,
    rgba: Vec<u8>,
}

impl Raster {
    fn new(w: i32, h: i32, fill: [u8; 4]) -> Self {
        let rgba = fill.iter().copied().cycle().take((w * h * 4) as usize).collect();
        Self { w, h, rgba }
    }
    fn put(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            let at = ((y * self.w + x) * 4) as usize;
            if let Some(px) = self.rgba.get_mut(at..at + 4) {
                px.copy_from_slice(&c);
            }
        }
    }
    fn line(&mut self, a: (f32, f32), b: (f32, f32), c: [u8; 4]) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.put((a.0 + dx * t).round() as i32, (a.1 + dy * t).round() as i32, c);
        }
    }
    fn fill_tri(&mut self, p: [(f32, f32); 3], c: [u8; 4]) {
        let min_x = p.iter().map(|q| q.0).fold(f32::MAX, f32::min).floor() as i32;
        let max_x = p.iter().map(|q| q.0).fold(f32::MIN, f32::max).ceil() as i32;
        let min_y = p.iter().map(|q| q.1).fold(f32::MAX, f32::min).floor() as i32;
        let max_y = p.iter().map(|q| q.1).fold(f32::MIN, f32::max).ceil() as i32;
        let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = edge(p[1], p[2], fx, fy);
                let w1 = edge(p[2], p[0], fx, fy);
                let w2 = edge(p[0], p[1], fx, fy);
                if (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0) {
                    self.put(x, y, c);
                }
            }
        }
    }
}

/// Height (z) to a blue -> green -> yellow ramp.
fn height_colour(z: f32, lo: f32, hi: f32) -> [u8; 4] {
    let t = if hi > lo { ((z - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.5 };
    let r = (255.0 * t) as u8;
    let g = (120.0 + 100.0 * t) as u8;
    let b = (255.0 * (1.0 - t)) as u8;
    [r / 2 + 40, g / 2 + 40, b / 2 + 40, 255]
}

/// Walkmesh seen from above: x to the right, y up the image; edges that cannot be crossed
/// are red, edges linked to another triangle are white.
fn render_walkmesh(w: &Walkmesh) -> (u32, u32, Vec<u8>) {
    let pts = || w.triangles.iter().flatten();
    let (mut min_x, mut max_x, mut min_y, mut max_y, mut min_z, mut max_z) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for v in pts() {
        min_x = min_x.min(f32::from(v.x));
        max_x = max_x.max(f32::from(v.x));
        min_y = min_y.min(f32::from(v.y));
        max_y = max_y.max(f32::from(v.y));
        min_z = min_z.min(f32::from(v.z));
        max_z = max_z.max(f32::from(v.z));
    }
    if w.is_empty() {
        return (1, 1, vec![0, 0, 0, 255]);
    }
    let margin = 24.0;
    let span = (max_x - min_x).max(max_y - min_y).max(1.0);
    let scale = (1000.0 / span).min(4.0);
    let width = ((max_x - min_x) * scale + 2.0 * margin).ceil() as i32;
    let height = ((max_y - min_y) * scale + 2.0 * margin).ceil() as i32;
    let map = |v: &fmv_ff7::field::Vertex| ((f32::from(v.x) - min_x) * scale + margin, (max_y - f32::from(v.y)) * scale + margin);
    let mut r = Raster::new(width, height, [16, 16, 24, 255]);
    for tri in &w.triangles {
        let z = tri.iter().map(|v| f32::from(v.z)).sum::<f32>() / 3.0;
        r.fill_tri([map(&tri[0]), map(&tri[1]), map(&tri[2])], height_colour(z, min_z, max_z));
    }
    for (i, tri) in w.triangles.iter().enumerate() {
        for e in 0..3 {
            let (a, b) = (map(&tri[e]), map(&tri[(e + 1) % 3]));
            let blocked = w.neighbor(i, e).is_none();
            r.line(a, b, if blocked { [255, 60, 60, 255] } else { [235, 235, 235, 255] });
        }
    }
    (width as u32, height as u32, r.rgba)
}

/// Walkmesh edges (red = blocked, white = linked), gateway exit lines (cyan), the camera range
/// rectangle (yellow) and the 320x240 window around the walkmesh centre (green), drawn in
/// background pixel space through the field camera.
fn render_overlay(img: &BackgroundImage, w: &Walkmesh, trig: &Triggers, proj: &Projection) -> Vec<u8> {
    let mut r = Raster { w: img.width as i32, h: img.height as i32, rgba: img.flatten_on_black() };
    let to_px = |v: [i16; 3]| proj.project_i16(v).map(|p| img.screen_to_pixel(p.x, p.y));
    for (i, tri) in w.triangles.iter().enumerate() {
        for e in 0..3 {
            let (Some(a), Some(b)) = (to_px([tri[e].x, tri[e].y, tri[e].z]), to_px([tri[(e + 1) % 3].x, tri[(e + 1) % 3].y, tri[(e + 1) % 3].z])) else {
                continue;
            };
            r.line(a, b, if w.neighbor(i, e).is_none() { [255, 60, 60, 255] } else { [235, 235, 235, 255] });
        }
    }
    for g in trig.gateways.iter().filter(|g| g.target_field_id != 0x7FFF) {
        if let (Some(a), Some(b)) = (to_px(g.exit_a), to_px(g.exit_b)) {
            r.line(a, b, [0, 255, 255, 255]);
        }
    }
    let cr = trig.camera_range;
    let corners = [(cr.left, cr.bottom), (cr.right, cr.bottom), (cr.right, cr.top), (cr.left, cr.top)];
    for k in 0..4 {
        let (a, b) = (corners[k], corners[(k + 1) % 4]);
        r.line(
            img.screen_to_pixel(f32::from(a.0), f32::from(a.1)),
            img.screen_to_pixel(f32::from(b.0), f32::from(b.1)),
            [255, 255, 0, 255],
        );
    }
    r.rgba
}

// ----- survey ------------------------------------------------------------------------------

fn survey(lgp: &mut LgpArchive<std::fs::File>) {
    use std::collections::BTreeMap;
    let mut problems: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut note = |k: String, who: &str| problems.entry(k).or_default().push(who.to_string());
    let names: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).collect();
    let (mut fields, mut skipped_non_field) = (0usize, 0usize);
    let mut layer_presence: BTreeMap<String, usize> = BTreeMap::new();
    let mut tex_depth: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut tiles, mut drawn, mut unrenderable, mut trailer14, mut decompressed) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut cameras: BTreeMap<usize, usize> = BTreeMap::new();
    let mut trailing: BTreeMap<usize, usize> = BTreeMap::new();
    let mut scales: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut range_eq_layer1, mut range_total) = (0usize, 0usize);
    let (mut multi_camera, mut multi_camera_copies) = (0usize, 0usize);
    let mut multi_camera_differ: Vec<String> = Vec::new();
    let (mut proj_total, mut proj_inside, mut proj_fields_all_inside) = (0usize, 0usize, 0usize);
    let mut proj_outliers: Vec<String> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        // Non-field entries: the name list and the *.tex/*.tut/*.NX/*.XOne extras.
        if name.eq_ignore_ascii_case("maplist") || name.contains('.') {
            skipped_non_field += 1;
            continue;
        }
        fields += 1;
        let raw = match lgp.read_index(i) {
            Ok(r) => r,
            Err(e) => {
                note(format!("lgp read: {e}"), name);
                continue;
            }
        };
        let out = match lzss::decompress(&raw) {
            Ok(o) => o,
            Err(e) => {
                note(format!("lzss: {e}"), name);
                continue;
            }
        };
        decompressed += out.len();
        let field = match FieldFile::parse(out) {
            Ok(f) => f,
            Err(e) => {
                note(format!("field parse: {e}"), name);
                continue;
            }
        };
        let declared: usize = (0..9).map(|k| field.declared_section_len(k).unwrap_or(0) as usize).sum::<usize>() + 78;
        match field.bytes().len().checked_sub(declared) {
            Some(0) => {}
            Some(14) if field.bytes().get(declared..) == Some(b"FINAL FANTASY7") => trailer14 += 1,
            _ => note("sections do not tile the file".into(), name),
        }
        match field.field_scale() {
            Ok(scale) => *scales.entry(scale).or_default() += 1,
            Err(e) => note(format!("field scale: {e}"), name),
        }
        if (0..9).any(|k| field.section(k).is_err()) {
            note("a section is inaccessible".into(), name);
        }
        match field.walkmesh() {
            Ok(w) => {
                if w.access.iter().flatten().any(|&e| e != NO_ACCESS && usize::from(e) >= w.len()) {
                    note("walkmesh access index out of range".into(), name);
                }
            }
            Err(e) => note(format!("walkmesh: {e}"), name),
        }
        match field.cameras() {
            Ok(c) => *cameras.entry(c.len()).or_default() += 1,
            Err(e) => note(format!("cameras: {e}"), name),
        }
        if let Ok(section) = field.section(1) {
            let records: Vec<&[u8]> = section.chunks_exact(38).collect();
            if records.len() > 1 {
                multi_camera += 1;
                if records.iter().all(|r| Some(r) == records.first()) {
                    multi_camera_copies += 1;
                } else {
                    multi_camera_differ.push(name.clone());
                }
            }
        }
        match field.triggers() {
            Ok(t) if !t.name.eq_ignore_ascii_case(name) => note("trigger name differs from entry name".into(), name),
            Ok(_) => {}
            Err(e) => note(format!("triggers: {e}"), name),
        }
        if let Err(e) = field.palette() {
            note(format!("palette: {e}"), name);
        }
        match field.background() {
            Ok(b) => {
                let key: String = b.layers.iter().map(|l| if l.is_some() { '1' } else { '0' }).collect();
                *layer_presence.entry(key).or_default() += 1;
                for t in b.textures.iter().flatten() {
                    *tex_depth.entry(t.depth).or_default() += 1;
                }
                if b.discarded_tiles > 0 {
                    note("tiles discarded by the destination rule".into(), name);
                }
                if let Some(e) = &b.texture_error {
                    note(format!("texture block: {e}"), name);
                }
                if !b.end_tag {
                    note("background section has no END tag after the texture slots".into(), name);
                }
                *trailing.entry(b.trailing_bytes).or_default() += 1;
                tiles += b.tiles.len();
            }
            Err(e) => note(format!("background: {e}"), name),
        }
        match decode_background(&field) {
            Ok(img) => {
                // Camera model check: walkmesh vertices should project inside the camera range.
                if let (Ok(w), Ok(cams)) = (field.walkmesh(), field.cameras()) {
                    if let Some(cam) = cams.first() {
                        let proj = Projection::from_camera(cam);
                        let b = img.view_bounds;
                        let (mut inside, mut total) = (0usize, 0usize);
                        for v in w.triangles.iter().flatten() {
                            total += 1;
                            if let Some(p) = proj.project_i16([v.x, v.y, v.z]) {
                                if p.x >= b.min_x as f32 - 2.0 && p.x < b.max_x as f32 + 2.0 && p.y >= b.min_y as f32 - 2.0 && p.y < b.max_y as f32 + 2.0 {
                                    inside += 1;
                                }
                            }
                        }
                        proj_total += total;
                        proj_inside += inside;
                        if total > 0 && inside == total {
                            proj_fields_all_inside += 1;
                        } else if total > 0 {
                            proj_outliers.push(format!("{name}:{inside}/{total}"));
                        }
                    }
                }
                if let Some(l1) = img.layers.first().and_then(|l| l.bounds) {
                    range_total += 1;
                    if l1 == img.view_bounds {
                        range_eq_layer1 += 1;
                    }
                }
                for l in &img.layers {
                    drawn += l.tiles_drawn;
                    unrenderable += l.tiles_unrenderable;
                    if l.tiles_unrenderable > 0 {
                        note(format!("layer {} has unrenderable tiles", l.layer), name);
                    }
                }
            }
            Err(e) => note(format!("decode_background: {e}"), name),
        }
    }
    println!("entries: {} ({fields} field entries, {skipped_non_field} non-field entries skipped: maplist and names containing '.')", names.len());
    println!("decompressed total {decompressed} bytes; {trailer14} fields carry the 14-byte FINAL FANTASY7 trailer");
    println!("layer presence (L1..L4): {layer_presence:?}; texture depths {tex_depth:?}; camera records per field {cameras:?}");
    println!("bytes after the END tag of the background section -> fields: {trailing:?}");
    println!("field scale (512 = 1.0) -> fields: {scales:?}");
    println!("tiles kept {tiles}; drawn with default options {drawn}; unrenderable {unrenderable}");
    println!("camera range == layer-1 tile bounds in {range_eq_layer1}/{range_total} fields that have layer-1 tiles");
    println!("fields with several camera records: {multi_camera}; all records byte-identical in {multi_camera_copies}; differing: {multi_camera_differ:?}");
    println!(
        "camera model: {proj_inside}/{proj_total} walkmesh vertices project inside the camera range (+-2px); {proj_fields_all_inside}/{fields} fields fully inside; outliers {} e.g. {:?}",
        proj_outliers.len(),
        proj_outliers.iter().take(12).collect::<Vec<_>>()
    );
    for (k, v) in &problems {
        println!("PROBLEM {k}: {} fields, e.g. {:?}", v.len(), v.iter().take(8).collect::<Vec<_>>());
    }
    if problems.is_empty() {
        println!("no problems");
    }
}
