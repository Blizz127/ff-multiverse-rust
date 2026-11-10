//! A QA picture of a story scene's floor plan (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example scene_map -j8                       # the proof scene
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example scene_map -j8 -- ff7/mds7pb_1/tifa-bar
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example scene_map -j8 -- ff7/tin_1/sector-seven-train
//! ```
//!
//! Writes `<scene>_map.png` to `/home/blizz/.cache/claude-tmp/ffmv-ff7-dump/` (outside the repository; it contains game art:
//! never commit or share it): the field's picture with the walkmesh drawn over it through the field camera, and over that the
//! places of the scene: every place the cast takes (a red body ring for Cloud, a green one for Tifa, a line for each walk), the
//! witness spots (yellow for the owner, orange and magenta for the watchers), the preview arrival (cyan) and, standing on each, a stick of the
//! height of Cloud's model, so the size of a body in the picture is plain. Nothing is printed but counts and ids.
//! Post-escape, station and train scene definitions can be surveyed here without registering them for live play.
#![allow(clippy::chunks_exact_to_as_chunks)]
use fmv_ff7::background::{decode_background, BackgroundImage, Projection};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::scene::{scene, Ground, ScenePlay, Stage, Then, PROOF_SCENE_ID};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use std::path::{Path, PathBuf};

const OUT_DIR: &str = "/home/blizz/.cache/claude-tmp/ffmv-ff7-dump";

struct Raster {
    w: i32,
    h: i32,
    rgba: Vec<u8>,
}

impl Raster {
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

    fn dot(&mut self, p: (f32, f32), r: i32, c: [u8; 4]) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.put(p.0.round() as i32 + dx, p.1.round() as i32 + dy, c);
                }
            }
        }
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

fn main() {
    let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let id = std::env::args().nth(1).unwrap_or_else(|| PROOF_SCENE_ID.to_string());
    // Survey-only scene data remains absent from the live registry.
    let def = match id.as_str() {
        fmv_ff7::script::scene::TRAIN_SCENE_ID => Some(fmv_ff7::script::scene::train_scene()),
        fmv_ff7::script::scene::STATION_SCENE_ID => Some(fmv_ff7::script::scene::station_scene()),
        fmv_ff7::script::scene::POST_ESCAPE_SCENE_ID => Some(fmv_ff7::script::scene::post_escape_scene()),
        fmv_ff7::script::scene::EARLY_BAR_SCENE_ID => Some(fmv_ff7::script::scene::early_bar_scene()),
        _ => scene(&id),
    };
    let Some(def) = def else {
        eprintln!("no scene {id}");
        return;
    };
    let path = PathBuf::from(dir).join("field").join("flevel.lgp");
    let mut lgp = LgpArchive::open_path(&path).expect("flevel.lgp opens");
    let raw = lgp.read(&def.field).expect("the field is in the archive");
    let field = FieldFile::parse(lzss::decompress(&raw).expect("decompresses")).expect("parses");
    if let Err(e) = run(&field, &def, &id) {
        eprintln!("{e}");
    }
}

fn run(field: &FieldFile, def: &fmv_ff7::script::scene::SceneDef, id: &str) -> Result<(), String> {
    let image: BackgroundImage = decode_background(field).map_err(|e| e.to_string())?;
    let camera = field.cameras().map_err(|e| e.to_string())?.into_iter().next().ok_or("no camera")?;
    let proj = Projection::from_camera(&camera);
    let mesh = field.walkmesh().map_err(|e| e.to_string())?;
    let ground = Ground::new(mesh.clone());
    let script = FieldScript::from_field(field).map_err(|e| e.to_string())?;
    let scale = fmv_ff7::default_model_scale(script.header().scale);
    let cloud = fmv_ff7::resolve_appearance("ff7/cloud").ok_or("no cloud")?;
    let _ = cloud;
    // Cloud's model is about 33 model units tall (the ring is 5.5 model units across its radius).
    let (ring, stick) = (5.5 * scale, 33.2 * scale);

    let mut r = Raster { w: image.width as i32, h: image.height as i32, rgba: image.flatten_on_black() };
    let to_px = |x: f32, y: f32, z: f32| proj.project([x, y, z]).map(|p| image.screen_to_pixel(p.x, p.y));
    // the walkmesh, faintly
    for (i, tri) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            let (a, b) = (tri[e], tri[(e + 1) % 3]);
            let (Some(pa), Some(pb)) = (to_px(f32::from(a.x), f32::from(a.y), f32::from(a.z)), to_px(f32::from(b.x), f32::from(b.y), f32::from(b.z))) else {
                continue;
            };
            r.line(pa, pb, if mesh.neighbor(i, e).is_none() { [255, 80, 80, 255] } else { [90, 90, 120, 255] });
        }
    }
    let body = |r: &mut Raster, x: f32, y: f32, colour: [u8; 4]| {
        let z = ground.height(x as i16, y as i16, None, None).unwrap_or(0.0);
        let mut prev = None;
        for k in 0..=24 {
            let t = k as f32 / 24.0 * std::f32::consts::TAU;
            let p = to_px(x + ring * t.cos(), y + ring * t.sin(), z);
            if let (Some(a), Some(b)) = (prev, p) {
                r.line(a, b, colour);
            }
            prev = p;
        }
        if let (Some(foot), Some(head)) = (to_px(x, y, z), to_px(x, y, z + stick)) {
            r.line(foot, head, colour);
            r.dot(head, 2, colour);
        }
    };

    // everything the cast does, over both ways through the scene
    let story = StoryState::at(*def.trigger.moments.start());
    let mut walks = 0usize;
    for answers in [vec![2usize], vec![1usize]] {
        let mut play = ScenePlay::new(field, def, &story, 1).map_err(|e| e.to_string())?;
        let mut at: std::collections::BTreeMap<u8, (f32, f32)> = Default::default();
        let colour_of = |e: u8| if e == def.trigger.entity { [60, 255, 60, 255] } else { [255, 60, 60, 255] };
        for s in play.initial_stage_events().to_vec() {
            if let Stage::Place { entity, x, y, .. } = s {
                at.insert(entity, (f32::from(x), f32::from(y)));
                body(&mut r, f32::from(x), f32::from(y), colour_of(entity));
            }
        }
        let mut answers = answers.into_iter();
        let mut given = None;
        loop {
            let beat = play.next(given.take());
            for t in &beat.stage {
                match &t.what {
                    Stage::Place { entity, x, y, .. } => {
                        at.insert(*entity, (f32::from(*x), f32::from(*y)));
                        body(&mut r, f32::from(*x), f32::from(*y), colour_of(*entity));
                    }
                    Stage::Move { entity, x, y, .. } => {
                        if let Some(&(fx, fy)) = at.get(entity) {
                            let (z0, z1) = (ground.height(fx as i16, fy as i16, None, None).unwrap_or(0.0), ground.height(*x, *y, None, None).unwrap_or(0.0));
                            if let (Some(a), Some(b)) = (to_px(fx, fy, z0), to_px(f32::from(*x), f32::from(*y), z1)) {
                                r.line(a, b, colour_of(*entity));
                                walks += 1;
                            }
                        }
                        at.insert(*entity, (f32::from(*x), f32::from(*y)));
                        body(&mut r, f32::from(*x), f32::from(*y), colour_of(*entity));
                    }
                    _ => {}
                }
            }
            match beat.then {
                Then::End { .. } => break,
                Then::Window(w) => {
                    if w.message.choice.is_some() {
                        given = answers.next();
                    }
                }
            }
        }
    }
    for (i, s) in def.witness.iter().enumerate() {
        body(&mut r, f32::from(s.x), f32::from(s.y), match i { 0 => [255, 235, 0, 255], 1 => [255, 140, 0, 255], _ => [255, 60, 255, 255] });
    }
    body(&mut r, f32::from(def.arrival.x), f32::from(def.arrival.y), [0, 230, 255, 255]);

    std::fs::create_dir_all(OUT_DIR).map_err(|e| e.to_string())?;
    let out = Path::new(OUT_DIR).join(format!("{}_map.png", id.replace('/', "_")));
    write_png(&out, image.width, image.height, &r.rgba)?;
    // SCENE_MAP_CROP=x0,y0,x1,y1,scale also writes a magnified crop of the picture (to look at the spots closely)
    if let Some(crop) = std::env::var("SCENE_MAP_CROP").ok().and_then(|c| c.split(',').map(|n| n.trim().parse::<i32>().ok()).collect::<Option<Vec<_>>>()) {
        if let [x0, y0, x1, y1, k] = crop[..] {
            let (cw, ch) = ((x1 - x0) * k, (y1 - y0) * k);
            let mut out_px = vec![0u8; (cw * ch * 4) as usize];
            for y in 0..ch {
                for x in 0..cw {
                    let (sx, sy) = (x0 + x / k, y0 + y / k);
                    if sx >= 0 && sy >= 0 && sx < r.w && sy < r.h {
                        let from = ((sy * r.w + sx) * 4) as usize;
                        let to = ((y * cw + x) * 4) as usize;
                        out_px[to..to + 4].copy_from_slice(&r.rgba[from..from + 4]);
                    }
                }
            }
            let zoomed = Path::new(OUT_DIR).join(format!("{}_map_zoom.png", id.replace('/', "_")));
            write_png(&zoomed, cw as u32, ch as u32, &out_px)?;
            println!("wrote {}", zoomed.display());
        }
    }
    println!("wrote {} ({}x{}, {walks} walks drawn, {} witness spots)", out.display(), image.width, image.height, def.witness.len());
    Ok(())
}
