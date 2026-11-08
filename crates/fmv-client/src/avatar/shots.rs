//! Offscreen checks and screenshots of the avatars, drawn through the real renderer (`Gpu`, the
//! avatar pipelines, the UI overlay) into a texture, read back and looked at.
//!
//! They need a GPU or a software Vulkan/GL driver (lavapipe/llvmpipe on a machine without a
//! display), so they only run on request: `cargo test -p fmv-client -- --ignored offscreen`.
//!
//! * The *synthetic* tests (no game data) draw stick figures and hand-made models through the
//!   camera of [`frame::tests`] and check pixels: where the models land, that the nearer one wins
//!   whatever the draw order, that the scenery in front covers them, that texels the colour key
//!   made transparent are discarded, that blended parts blend, that an sRGB target gets the
//!   colours as authored.
//! * The *real-data* test draws the owner's own `md1stin` with their own `char.lgp` and writes the
//!   PNGs (`50-avatars-md1stin.png` and friends in `$FMV_SHOT_DIR`, default
//!   `~/.cache/claude-tmp/fmv-client-shots`). It needs `FMV_FF7_LIBRARY` (the state directory
//!   holding the imported library, read-only), or `FMV_FF7_DATA` (the game's `data/` directory),
//!   or `FMV_FF7_FLEVEL` (a `flevel.lgp` with a `char.lgp` beside it).

use super::anim::Who;
use super::frame::tests::{stage, view};
use super::frame::{AvatarFrame, AvatarPlayer, Avatars};
use super::gpu::projection_terms;
use super::library::tests::character;
use super::library::AvatarLibrary;
use super::source::CharacterLoader;
use crate::backdrop::Backdrop;
use crate::render::{Camera, Scene, CLEAR_RGB, MARGIN_PX};
use crate::ui::draw::DrawList;
use crate::ui::testkit::{pixel, save, Offscreen};
use fmv_adapter_api::Bounds;
use fmv_core::{AnimState, Pos};
use fmv_ff7::playable::{Character, PlayableModel};
use fmv_ff7::{PartBlend, RgbaImage};
use std::sync::Arc;

const W: u32 = 800;
const H: u32 = 600;

/// The view-space window of the synthetic scenes: 200 x 200 field-screen units around the origin.
fn bounds() -> Bounds {
    Bounds { min: Pos::new(-100.0, -100.0, 0.0), max: Pos::new(100.0, 100.0, 0.0) }
}

fn fitted() -> Camera {
    Camera::fit(&bounds(), W as f32, H as f32, MARGIN_PX)
}

/// Where a camera-space point lands in a `w` x `h` window fitted by `cam` for a camera of focal
/// length `zoom`, in pixels from the top left: the vertex shader's arithmetic on the CPU.
fn project_pixel(cam: &Camera, zoom: f32, w: u32, h: u32, cam_pos: [f32; 3]) -> (f32, f32) {
    let t = projection_terms(cam, zoom, w, h);
    let depth = cam_pos[2];
    let (x, y) = ((t[0] * cam_pos[0] + t[1] * cam_pos[2]) / depth, (t[2] * cam_pos[1] + t[3] * cam_pos[2]) / depth);
    ((x * 0.5 + 0.5) * w as f32, (0.5 - y * 0.5) * h as f32)
}

/// [`project_pixel`] in the synthetic scenes' window.
fn to_pixel(cam_pos: [f32; 3]) -> (f32, f32) {
    project_pixel(&fitted(), 400.0, W, H, cam_pos)
}

fn centroid(points: &[[f32; 3]]) -> (u32, u32) {
    let n = points.len() as f32;
    let (x, y) = points.iter().map(|&p| to_pixel(p)).fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    ((x / n).round() as u32, (y / n).round() as u32)
}

fn close(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

/// Characters of a known colour, by appearance id: Cloud is the red stick figure, Tifa the blue
/// one, Barret a quad with a half-transparent texture, Aerith a half-transparent red figure.
struct Dyed;

fn stick(appearance: PlayableModel, rgb: [u8; 3]) -> Character {
    let mut c = character(appearance, 1, 4, 3);
    for part in &mut c.model.parts {
        part.colors = vec![[rgb[0], rgb[1], rgb[2], 255]; part.positions.len()];
    }
    c
}

impl CharacterLoader for Dyed {
    fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
        Ok(match appearance.appearance_ref {
            "ff7/cloud" => stick(*appearance, [255, 0, 0]),
            "ff7/tifa" => stick(*appearance, [0, 0, 255]),
            "ff7/barret" => {
                // A quad facing the camera whose texture is transparent on its left texel (the
                // colour key) and green on its right one.
                let mut c = stick(*appearance, [255, 255, 255]);
                let texture = Arc::new(RgbaImage { width: 2, height: 1, rgba: vec![0, 0, 0, 0, 0, 255, 0, 255] });
                let mut quad = c.model.parts[0].clone();
                quad.positions = vec![[-6.0, 5.0, 0.0], [6.0, 5.0, 0.0], [6.0, -25.0, 0.0], [-6.0, -25.0, 0.0]];
                quad.uvs = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
                quad.normals = vec![[0.0; 3]; 4];
                quad.colors = vec![[255, 255, 255, 255]; 4];
                quad.indices = vec![0, 1, 2, 0, 2, 3];
                quad.texture = Some(texture);
                quad.texture_name = Some("face".into());
                c.model.parts = vec![quad];
                c
            }
            "ff7/aerith" => {
                let mut c = stick(*appearance, [255, 0, 0]);
                for part in &mut c.model.parts {
                    part.blend = PartBlend::Alpha;
                    part.alpha = 0.5;
                }
                c
            }
            other => return Err(format!("no test character {other}")),
        })
    }
}

fn avatars() -> Avatars {
    Avatars::new(AvatarLibrary::new(Box::new(Dyed)))
}

/// A player standing at `at` facing the camera (the camera looks along +y, so that is heading -y).
fn facing_camera(id: u64, appearance: &'static str, at: Pos) -> AvatarPlayer<'static> {
    AvatarPlayer {
        who: Who(id),
        pos: at,
        facing: -std::f32::consts::FRAC_PI_2,
        anim: AnimState::Idle,
        appearance: Some(appearance),
        npc: None,
        tint: [10, 200, 30],
        own: false,
        party: false,
    }
}

fn scene() -> Scene<'static> {
    Scene { bounds: bounds(), triangles: &[], players: &[], fixture_banner: false, backdrop: None }
}

fn built(players: &[AvatarPlayer<'_>], marks: bool) -> AvatarFrame {
    let mut frame = avatars().frame(&view(), &stage(), players, 0.0).frame;
    if !marks {
        frame.ground.clear();
    }
    frame
}

fn draw(gpu: &mut Offscreen, frame: &AvatarFrame) -> Vec<u8> {
    gpu.render_field(W, H, &scene(), Some(frame), &DrawList::new())
}

/// The three corners of a model's body triangle (its first three vertices), in camera space.
fn body(frame: &AvatarFrame, model: usize) -> [[f32; 3]; 3] {
    let p = &frame.models[model].positions;
    [p[0], p[1], p[2]]
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_a_model_lands_where_the_camera_puts_it_and_the_nearer_one_wins_in_any_order() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let red = facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0));
    // 120 units nearer to the camera and 5 to the right: they overlap on the screen.
    let blue = facing_camera(2, "ff7/tifa", Pos::new(5.0, -120.0, 0.0));

    // Alone, each shows its own colour at the centre of its body, and the clear colour away from it.
    let alone = built(&[red], false);
    let img = draw(&mut gpu, &alone);
    let (cx, cy) = centroid(&body(&alone, 0));
    assert!(close(pixel(&img, W, cx, cy), [255, 0, 0], 2), "red body at ({cx},{cy}): {:?}", pixel(&img, W, cx, cy));
    assert!(close(pixel(&img, W, 20, 20), CLEAR_RGB, 2), "background");
    save("50x-avatars-synthetic-one", W, H, &img);

    // Together: where they overlap the picture is the NEARER figure's colour, whatever the order
    // of the list (the frame itself lists the nearest model first).
    let both = built(&[red, blue], false);
    let swapped = built(&[blue, red], false);
    let (a, b) = (draw(&mut gpu, &both), draw(&mut gpu, &swapped));
    assert!(a == b, "the picture does not depend on the order of the players");
    let model_of = |frame: &AvatarFrame, id: &str| frame.models.iter().position(|m| m.asset.appearance.appearance_ref == id).unwrap();
    let red_tri = body(&both, model_of(&both, "ff7/cloud")).map(to_pixel);
    let blue_tri = body(&both, model_of(&both, "ff7/tifa")).map(to_pixel);
    let inside = |tri: &[(f32, f32); 3], p: (f32, f32)| {
        let sign = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| (a.0 - c.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - c.1);
        let (d1, d2, d3) = (sign(p, tri[0], tri[1]), sign(p, tri[1], tri[2]), sign(p, tri[2], tri[0]));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    // Each triangle shrunk a little around its centre, so a pixel on an edge (which may go either
    // way) is never judged.
    let shrink = |tri: &[(f32, f32); 3]| {
        let c = ((tri[0].0 + tri[1].0 + tri[2].0) / 3.0, (tri[0].1 + tri[1].1 + tri[2].1) / 3.0);
        tri.map(|v| (c.0 + (v.0 - c.0) * 0.8, c.1 + (v.1 - c.1) * 0.8))
    };
    let (red_core, blue_core) = (shrink(&red_tri), shrink(&blue_tri));
    let (mut only_red, mut only_blue, mut overlap) = (0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let got = pixel(&a, W, x, y);
            match (inside(&red_core, p), inside(&blue_core, p)) {
                (true, true) => {
                    overlap += 1;
                    assert!(close(got, [0, 0, 255], 2), "overlap is the nearer (blue) figure at ({x},{y}): {got:?}");
                }
                (true, false) => {
                    only_red += 1;
                    // Red, unless the nearer figure's small "nose" triangle happens to cover it.
                    assert!(close(got, [255, 0, 0], 2) || close(got, [0, 0, 255], 2), "red only at ({x},{y}): {got:?}");
                }
                (false, true) => {
                    only_blue += 1;
                    assert!(close(got, [0, 0, 255], 2), "blue only at ({x},{y}): {got:?}");
                }
                _ => {}
            }
        }
    }
    assert!(overlap > 50 && only_red > 50 && only_blue > 50, "the figures overlap partly: {only_red} {only_blue} {overlap}");
    save("50x-avatars-synthetic-two", W, H, &a);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_the_scenery_in_front_covers_a_model_and_the_ground_ring_is_under_it() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let red = facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0));
    let mut frame = built(&[red], false);
    let (cx, cy) = centroid(&body(&frame, 0));
    // Without a front layer the body shows.
    assert!(close(pixel(&draw(&mut gpu, &frame), W, cx, cy), [255, 0, 0], 2));
    // A front layer: one opaque green texel over the left half of the window rectangle (x < 0 in
    // view space). The figure straddles that edge: its left half is covered, its right half not.
    let green = Backdrop::new(1, 1, vec![0, 255, 0, 255], [-100.0, -100.0], [0.0, 100.0]).unwrap();
    frame.front = Some(Arc::new(green));
    let img = draw(&mut gpu, &frame);
    let left = (cx - 6, cy);
    let right = (cx + 6, cy);
    assert!(close(pixel(&img, W, left.0, left.1), [0, 255, 0], 2), "left of the edge: {:?}", pixel(&img, W, left.0, left.1));
    assert!(close(pixel(&img, W, right.0, right.1), [255, 0, 0], 2), "right of the edge: {:?}", pixel(&img, W, right.0, right.1));
    // Where nothing is in front, the clear colour is not painted over (the front is transparent
    // outside its rectangle).
    assert!(close(pixel(&img, W, W - 20, 20), CLEAR_RGB, 2));
    save("50x-avatars-synthetic-front", W, H, &img);

    // The ground marks (shadow and rings) are in the scene pass, under the model: a pixel of the
    // tint ring beside the feet shows the tint, and the body is still on top of what is under it.
    let with_marks = built(&[red], true);
    let img = draw(&mut gpu, &with_marks);
    let ring = &with_marks.ground[1];
    assert_eq!(ring.rgb, [10, 200, 30]);
    // The middle of the ring's band at its rightmost point (world +x of the feet).
    let at = |outline: &[[f32; 2]]| outline.iter().copied().max_by(|a, b| a[0].total_cmp(&b[0])).unwrap();
    let (outer, inner) = (at(&ring.outer), at(&ring.inner));
    let mid = fitted().project((outer[0] + inner[0]) / 2.0, (outer[1] + inner[1]) / 2.0);
    let (px, py) = ((W as f32 / 2.0 + mid[0]).round() as u32, (H as f32 / 2.0 - mid[1]).round() as u32);
    let got = pixel(&img, W, px, py);
    // Ring (alpha 0.85) over the shadow disc (alpha 0.3 black) over the clear colour.
    assert!(got[1] > 120 && got[0] < 80, "tint ring at ({px},{py}): {got:?}");
    assert!(close(pixel(&img, W, cx, cy), [255, 0, 0], 2), "the model is over its own ring");
    save("50x-avatars-synthetic-ring", W, H, &img);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_transparent_texels_are_discarded_and_blended_parts_blend() {
    let Some(mut gpu) = Offscreen::new() else { return };
    // Barret: a quad, left texel transparent (colour key), right texel green.
    let textured = built(&[facing_camera(1, "ff7/barret", Pos::new(0.0, 0.0, 0.0))], false);
    let img = draw(&mut gpu, &textured);
    let quad = &textured.models[0].positions;
    let (cx, cy) = centroid(&[quad[0], quad[1], quad[2], quad[3]]);
    let probe = |dx: i32| pixel(&img, W, (cx as i32 + dx) as u32, cy);
    assert!(close(probe(-14), CLEAR_RGB, 2), "the transparent texel shows what is behind: {:?}", probe(-14));
    assert!(close(probe(14), [0, 255, 0], 3), "the opaque texel shows its colour: {:?}", probe(14));
    save("50x-avatars-synthetic-texture", W, H, &img);

    // Aerith: a half-transparent red figure over the clear colour.
    let veil = built(&[facing_camera(2, "ff7/aerith", Pos::new(0.0, 0.0, 0.0))], false);
    let img = draw(&mut gpu, &veil);
    let (cx, cy) = centroid(&body(&veil, 0));
    let mix = |c: u8, b: u8| (f32::from(c) * 0.5 + f32::from(b) * 0.5).round() as u8;
    let want = [mix(255, CLEAR_RGB[0]), mix(0, CLEAR_RGB[1]), mix(0, CLEAR_RGB[2])];
    assert!(close(pixel(&img, W, cx, cy), want, 3), "half-transparent red over the background: {:?} want {want:?}", pixel(&img, W, cx, cy));
    // The two parts of the figure overlap at the nose, drawn without writing depth: no hole there.
    save("50x-avatars-synthetic-veil", W, H, &img);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_a_character_is_uploaded_once_however_many_frames_and_players_use_it() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let (view, stage) = (view(), stage());
    let mut avatars = avatars();
    assert_eq!(gpu.cached_avatar_models(), 0);
    // Frame after frame, three players of two looks (two share one): two characters on the GPU,
    // the same two on every later frame (the skinned positions are what is written per frame).
    let players = [
        facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0)),
        facing_camera(2, "ff7/tifa", Pos::new(40.0, -60.0, 0.0)),
        facing_camera(3, "ff7/cloud", Pos::new(-40.0, 30.0, 0.0)),
    ];
    let mut first = None;
    for i in 0..5 {
        let mut frame = avatars.frame(&view, &stage, &players, 0.05 * i as f32).frame;
        frame.ground.clear();
        let img = draw(&mut gpu, &frame);
        assert_eq!(gpu.cached_avatar_models(), 2, "frame {i}");
        // (the synthetic clips do not move, so every frame is the same picture)
        assert!(first.get_or_insert_with(|| img.clone()) == &img, "frame {i} differs");
    }
    // A third look is a third upload, once.
    let mut more = players.to_vec();
    more.push(facing_camera(4, "ff7/barret", Pos::new(10.0, 90.0, 0.0)));
    for _ in 0..3 {
        let mut frame = avatars.frame(&view, &stage, &more, 0.0).frame;
        frame.ground.clear();
        draw(&mut gpu, &frame);
        assert_eq!(gpu.cached_avatar_models(), 3);
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_an_srgb_target_gets_the_colours_as_authored() {
    let Some(mut gpu) = Offscreen::with_format(wgpu::TextureFormat::Rgba8UnormSrgb) else { return };
    let frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let img = draw(&mut gpu, &frame);
    let (cx, cy) = centroid(&body(&frame, 0));
    // The bytes that come back are what an sRGB surface would show: the authored colours.
    assert!(close(pixel(&img, W, cx, cy), [255, 0, 0], 2), "red: {:?}", pixel(&img, W, cx, cy));
    assert!(close(pixel(&img, W, 20, 20), CLEAR_RGB, 2), "background: {:?}", pixel(&img, W, 20, 20));
    // And a textured part: the texture is sampled as sRGB, so its green stays green.
    let textured = built(&[facing_camera(1, "ff7/barret", Pos::new(0.0, 0.0, 0.0))], false);
    let img = draw(&mut gpu, &textured);
    let quad = &textured.models[0].positions;
    let (cx, cy) = centroid(&[quad[0], quad[1], quad[2], quad[3]]);
    assert!(close(pixel(&img, W, cx + 14, cy), [0, 255, 0], 3), "{:?}", pixel(&img, W, cx + 14, cy));
}

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

mod real {
    use super::*;
    use crate::avatar::source::LgpLoader;
    use crate::ff7::{prewarm, resolve_char_lgp, resolve_flevel, Ff7Labels, Ff7Source};
    use crate::ui::hud::{HudInput, Label, LabelKind};
    use crate::ui::testkit::write_png;
    use fmv_adapter_api::AreaRuntime;
    use fmv_ff7::Ff7Adapter;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// The owner's FF7 data, opened read-only: the field adapter and the character archive.
    pub struct Real {
        pub adapter: Ff7Adapter,
        pub char_lgp: PathBuf,
    }

    /// `FMV_FF7_LIBRARY` (the state directory of the imported library), else `FMV_FF7_DATA` (the
    /// game's `data/` directory), else `FMV_FF7_FLEVEL` (a `flevel.lgp` with `char.lgp` beside
    /// it). `None` (with a note) when none is set.
    pub fn open() -> Option<Real> {
        let direct = |path: PathBuf| Ff7Source::Flevel { path, labels: Ff7Labels::default() };
        let source = if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
            Ff7Source::Library { state_dir: Some(dir.into()), set: None }
        } else if let Some(dir) = std::env::var_os("FMV_FF7_DATA") {
            direct(PathBuf::from(dir).join("field").join("flevel.lgp"))
        } else if let Some(path) = std::env::var_os("FMV_FF7_FLEVEL") {
            direct(path.into())
        } else {
            eprintln!("skipped: none of FMV_FF7_LIBRARY, FMV_FF7_DATA, FMV_FF7_FLEVEL is set");
            return None;
        };
        let resolved = resolve_flevel(&source).expect("the field archive is found");
        let adapter = Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance).expect("the field archive opens");
        prewarm(&adapter);
        let char_lgp = resolve_char_lgp(&source).expect("the character archive is looked for").expect("a char.lgp next to the field archive");
        Some(Real { adapter, char_lgp })
    }

    pub fn avatars(real: &Real) -> Avatars {
        Avatars::new(AvatarLibrary::new(Box::new(LgpLoader::new(&real.char_lgp))))
    }

    /// The heading (radians) that looks at the camera, along the camera's ground direction.
    pub fn toward_camera(view_forward: [f64; 3]) -> f32 {
        (-view_forward[1]).atan2(-view_forward[0]) as f32
    }

    /// A [`HudInput`] naming the given players, labelled at the anchors the frame computed.
    pub fn hud_for(view: &crate::backdrop::FieldView, built: &crate::avatar::frame::Built, names: &[(u64, &str, LabelKind)]) -> HudInput {
        let labels = names
            .iter()
            .filter_map(|(id, text, kind)| built.anchors.get(&Who(*id)).map(|a| Label { x: a.x, y: a.y, text: (*text).into(), kind: *kind }))
            .collect();
        HudInput {
            bounds: view.bounds,
            labels,
            area: "FF7 md1stin".into(),
            fixture: false,
            players: names.len(),
            rtt: Some(Duration::from_millis(14)),
            party: vec![],
            invite_from: None,
        }
    }

    /// Nearest-neighbour crop of `img` (`w` wide), scaled up `zoom` times, as a new RGBA image.
    pub fn zoom_crop(img: &[u8], w: u32, x0: u32, y0: u32, cw: u32, ch: u32, zoom: u32) -> (u32, u32, Vec<u8>) {
        let mut out = Vec::with_capacity((cw * ch * zoom * zoom * 4) as usize);
        for y in 0..ch * zoom {
            for x in 0..cw * zoom {
                let at = (((y0 + y / zoom) * w + x0 + x / zoom) * 4) as usize;
                out.extend_from_slice(&img[at..at + 4]);
            }
        }
        (cw * zoom, ch * zoom, out)
    }

    pub fn save_png(name: &str, w: u32, h: u32, rgba: &[u8]) {
        let dir = crate::ui::testkit::shot_dir();
        std::fs::create_dir_all(&dir).expect("shot dir");
        let file = dir.join(format!("{name}.png"));
        std::fs::write(&file, write_png(w, h, rgba)).expect("write png");
        eprintln!("wrote {}", file.display());
    }

    /// Up to `n` of `spots`, far apart on the screen: the nearest first, then each time the one
    /// farthest from those already chosen (in view space).
    pub fn spread(spots: &[Spot], n: usize) -> Vec<Spot> {
        let centre = |s: &Spot| ((s.view_rect.0 + s.view_rect.2) / 2.0, (s.view_rect.1 + s.view_rect.3) / 2.0);
        let mut chosen: Vec<Spot> = Vec::new();
        while chosen.len() < n {
            let next = spots.iter().copied().filter(|s| !chosen.iter().any(|c| c.pos == s.pos)).max_by(|a, b| {
                let score = |s: &Spot| {
                    chosen.iter().map(|c| (centre(c).0 - centre(s).0).hypot(centre(c).1 - centre(s).1)).fold(f32::MAX, f32::min)
                };
                // The first pick is the nearest to the camera (the biggest picture).
                if chosen.is_empty() { b.depth.total_cmp(&a.depth) } else { score(a).total_cmp(&score(b)) }
            });
            match next {
                Some(s) => chosen.push(s),
                None => break,
            }
        }
        chosen
    }

    pub fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let out = f();
        eprintln!("{label}: {:.2?}", started.elapsed());
        out
    }

    /// A place a character could stand, with where it would be seen.
    #[derive(Debug, Clone, Copy)]
    pub struct Spot {
        pub pos: Pos,
        /// Camera depth of the feet.
        pub depth: f32,
        /// The character's rectangle in view space (left, bottom, right, top).
        pub view_rect: (f32, f32, f32, f32),
        /// How much of that rectangle the scenery in front covers (0..1).
        pub covered: f32,
    }

    /// The fraction of the view-space `rect` (left, bottom, right, top) that `front` covers.
    pub fn front_coverage(front: &Backdrop, rect: (f32, f32, f32, f32)) -> f32 {
        let (w, h) = (front.width as f32, front.height as f32);
        let (sx, sy) = (w / (front.max[0] - front.min[0]), h / (front.max[1] - front.min[1]));
        let (x0, x1) = (((rect.0 - front.min[0]) * sx).floor().max(0.0) as u32, (((rect.2 - front.min[0]) * sx).ceil().max(0.0) as u32).min(front.width));
        let (y0, y1) = (((front.max[1] - rect.3) * sy).floor().max(0.0) as u32, (((front.max[1] - rect.1) * sy).ceil().max(0.0) as u32).min(front.height));
        let (mut covered, mut total) = (0u32, 0u32);
        for y in y0..y1 {
            for x in x0..x1 {
                total += 1;
                covered += u32::from(front.rgba.get(((y * front.width + x) * 4 + 3) as usize).is_some_and(|&a| a != 0));
            }
        }
        if total == 0 {
            0.0
        } else {
            covered as f32 / total as f32
        }
    }

    /// Points of the walkmesh (corners, edge midpoints and centres of its triangles) where a
    /// character would be a good size on the screen (camera depth `depth_range`), with what they
    /// would look like: found by skinning a character there and projecting it.
    pub fn spots(
        avatars: &mut Avatars,
        view: &crate::backdrop::FieldView,
        stage: &crate::avatar::Stage,
        area: &dyn AreaRuntime,
        depth_range: (f32, f32),
    ) -> Vec<Spot> {
        let mut points: Vec<Pos> = Vec::new();
        for t in area.walk_triangles() {
            let mid = |a: Pos, b: Pos| Pos::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0);
            let centre = Pos::new((t[0].x + t[1].x + t[2].x) / 3.0, (t[0].y + t[1].y + t[2].y) / 3.0, (t[0].z + t[1].z + t[2].z) / 3.0);
            points.extend([centre, mid(t[0], t[1]), mid(t[1], t[2]), mid(t[2], t[0])]);
        }
        let front = stage.front.as_deref();
        points
            .into_iter()
            .filter_map(|pos| {
                let depth = stage.camera.project([f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)])?[2] as f32;
                if depth < depth_range.0 || depth > depth_range.1 {
                    return None;
                }
                let player = AvatarPlayer { who: Who(0), pos, facing: 0.0, anim: AnimState::Idle, appearance: Some("ff7/cloud"), npc: None, tint: [0; 3], own: false, party: false };
                let built = avatars.frame(view, stage, &[player], 0.0);
                let model = built.frame.models.first()?;
                let mut rect = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for p in &model.positions {
                    let (vx, vy) = (p[0] * stage.camera.zoom as f32 / p[2], -p[1] * stage.camera.zoom as f32 / p[2]);
                    rect = (rect.0.min(vx), rect.1.min(vy), rect.2.max(vx), rect.3.max(vy));
                }
                let covered = front.map_or(0.0, |f| front_coverage(f, rect));
                Some(Spot { pos, depth, view_rect: rect, covered })
            })
            .collect()
    }
}

/// Three players of different looks in the real `md1stin`, with the real models, through the real
/// renderer, as the window draws them: PNGs to look at, plus numbers (where the pixels that
/// changed are against where the model's vertices project, the picture of the scenery in front,
/// that nothing of the back half shows). `FMV_FF7_AREA` picks another field.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_avatars_in_the_field() {
    use crate::backdrop::FieldView;
    use crate::ui::hud::{self, LabelKind};
    use fmv_adapter_api::AreaRuntime;
    use real::*;
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "md1stin".to_string());
    let view = timed("field view with the scenery in front", || FieldView::ff7_with(&real.adapter, &key, true)).expect("field view");
    eprintln!("{}", view.description);
    let stage = view.stage.clone().expect("an FF7 field has a stage");
    let area = real.adapter.load_field(&key).expect("field");
    eprintln!("field scale {}, model scale {}, walk {:.1} run {:.1}", area.field_scale(), stage.model_scale, stage.walk_speed, stage.run_speed);

    if let Some(front) = &stage.front {
        // The scenery in front of the characters, on black, to see what covers whom.
        let mut flat = front.rgba.clone();
        for px in flat.chunks_exact_mut(4) {
            px[3] = 255;
        }
        save_png(&format!("53-front-layer-{key}"), front.width, front.height, &flat);
    }
    // Where to stand: places on the walkmesh where a character is a good size and the scenery in
    // front does not cover it, spread over the screen; and one place where it is half behind
    // the scenery.
    let mut avatars = avatars(&real);
    let candidates = spots(&mut avatars, &view, &stage, &area, (450.0, 1300.0));
    eprintln!("{} candidate places in depth 450..1300", candidates.len());
    let clear = spread(&candidates.iter().copied().filter(|s| s.covered == 0.0).collect::<Vec<_>>(), 4);
    assert_eq!(clear.len(), 4, "four uncovered places on the walkmesh");
    // How far does the flat front layer reach into the walkable places? Count the places where it
    // covers part of a character, and attribute each to the object (front-layer tiles sharing a
    // layer and an id) that overlaps the character's rectangle most.
    let background = area.field_file().background().expect("the field's background");
    let tiles: Vec<((u8, u16), [f32; 4])> = background
        .tiles
        .iter()
        .filter(|t| (t.layer == 2 || t.layer == 4) && t.param == 0)
        .map(|t| {
            let (x, y, size) = (f32::from(t.dst_x), f32::from(t.dst_y), f32::from(t.tile_size));
            ((t.layer, t.id), [x, -(y + size), x + size, -y])
        })
        .collect();
    let dominant = |rect: (f32, f32, f32, f32)| -> Option<(u8, u16)> {
        let mut by_object: std::collections::BTreeMap<(u8, u16), f32> = std::collections::BTreeMap::new();
        for (object, t) in &tiles {
            let (w, h) = ((rect.2.min(t[2]) - rect.0.max(t[0])).max(0.0), (rect.3.min(t[3]) - rect.1.max(t[1])).max(0.0));
            if w * h > 0.0 {
                *by_object.entry(*object).or_default() += w * h;
            }
        }
        by_object.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(object, _)| object)
    };
    let touched: Vec<&Spot> = candidates.iter().filter(|s| s.covered > 0.0).collect();
    let mut blame: std::collections::BTreeMap<(u8, u16), usize> = std::collections::BTreeMap::new();
    for s in &touched {
        if let Some(object) = dominant(s.view_rect) {
            *blame.entry(object).or_default() += 1;
        }
    }
    let mut blame: Vec<_> = blame.into_iter().collect();
    blame.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    eprintln!(
        "FLAT FRONT LAYER REACH in {key}: it covers part of a character at {} of {} walkable places in depth 450..1300 ({} fully clear); by object (layer, id): {:?}",
        touched.len(),
        candidates.len(),
        candidates.len() - touched.len(),
        blame
    );
    // For the demonstration of the front layer being drawn over a character: a place where the
    // character is about half covered by the near train (layer 2, id 1) if there is one.
    let pick = |filter: &dyn Fn(&Spot) -> bool| {
        candidates.iter().copied().filter(|s| filter(s)).min_by(|a, b| (a.covered - 0.5).abs().total_cmp(&(b.covered - 0.5).abs()))
    };
    let behind = pick(&|s| s.covered > 0.2 && s.covered < 0.9 && dominant(s.view_rect) == Some((2, 1)))
        .or_else(|| pick(&|s| s.covered > 0.2 && s.covered < 0.9));
    let spots = clear.iter().map(|s| s.pos).collect::<Vec<_>>();
    for s in &clear {
        eprintln!("clear spot {:?}: depth {:.0}, rect {:?}", s.pos, s.depth, s.view_rect);
    }
    let forward = {
        let cams = area.cameras().expect("cameras");
        let axes = cams[0].axes_f32();
        [f64::from(axes[2][0]), f64::from(axes[2][1]), f64::from(axes[2][2])]
    };
    let toward = toward_camera(forward);
    eprintln!("camera forward in the field {forward:?}; heading towards the camera {:.1} deg", toward.to_degrees());
    for (i, s) in area.spawn_points().iter().enumerate() {
        eprintln!("spawn {i}: {s:?}");
    }
    let quarter = std::f32::consts::FRAC_PI_2;
    let player = |id: u64, appearance: Option<&'static str>, at: Pos, facing: f32, anim: AnimState, own: bool, party: bool| AvatarPlayer {
        who: Who(id),
        pos: at,
        facing,
        anim,
        appearance,
        npc: None,
        tint: [[90, 125, 245], [235, 80, 80], [60, 200, 90], [250, 190, 40]][(id as usize - 1) % 4],
        own,
        party,
    };
    // Cloud walks towards the camera (the local player), Tifa stands in profile in his party,
    // Barret runs away from the camera, and someone with no known look stands in as Cloud.
    let players = [
        player(1, Some("ff7/cloud"), spots[0], toward + 0.47, AnimState::Walk, true, true),
        player(2, Some("ff7/tifa"), spots[1], toward + quarter, AnimState::Idle, false, true),
        player(3, Some("ff7/barret"), spots[2], toward + 2.0 * quarter + 0.4, AnimState::Run, false, false),
        player(4, None, spots[3], toward - 0.6, AnimState::Idle, false, false),
    ];
    // Let the walk and run loops get to frame 7 of the walk (the reference render's) and a
    // quarter of the way round the run.
    avatars.frame(&view, &stage, &players, 0.0);
    let built = timed("one frame of four avatars (pose, skin, marks)", || avatars.frame(&view, &stage, &players, 7.0 / 30.0));
    eprintln!("{:?}; markers {:?}", built.frame, built.markers);
    assert_eq!(built.frame.models.len(), 4, "all four are models");

    let names = [(1, "Cloud", LabelKind::Own), (2, "Tifa", LabelKind::Party), (3, "Barret", LabelKind::Other), (4, "Stranger", LabelKind::Other)];
    let scene = Scene { bounds: view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: view.backdrop.as_ref() };
    for (w, h, name) in [(1100u32, 680u32, "50-avatars-md1stin"), (1920, 1080, "51-avatars-md1stin-1920x1080")] {
        let mut overlay = DrawList::new();
        hud::draw_hud(&hud_for(&view, &built, &names), w, h, &mut overlay);
        let img = timed(&format!("render {w}x{h}"), || gpu.render_field(w, h, &scene, Some(&built.frame), &overlay));
        save(name, w, h, &img);
        if w == 1920 {
            // A 3x crop around each model, to look at the face, the pose and the layering.
            for (i, model) in built.frame.models.iter().enumerate() {
                let (x0, y0, x1, y1) = model_bbox(&built.frame, i, &scene, w, h);
                let (cx, cy) = (((x0 + x1) / 2.0) as i64, ((y0 + y1) / 2.0) as i64);
                let half = (((x1 - x0).max(y1 - y0) / 2.0) as i64 + 12).clamp(40, 300);
                let (ox, oy) = ((cx - half).clamp(0, i64::from(w) - 2 * half) as u32, (cy - half).clamp(0, i64::from(h) - 2 * half) as u32);
                let (cw, ch, crop) = zoom_crop(&img, w, ox, oy, 2 * half as u32, 2 * half as u32, 3);
                let id = model.asset.appearance.appearance_ref.rsplit('/').next().unwrap_or("model");
                save_png(&format!("52-avatars-closeup-{i}-{id}"), cw, ch, &crop);
            }
        }
    }

    // ---- numbers -------------------------------------------------------------------------------
    let (w, h) = (1920u32, 1080u32);
    let none = Scene { backdrop: view.backdrop.as_ref(), ..scene };
    let base = gpu.render_field(w, h, &none, None, &DrawList::new());

    // 1. Where the pixels that change are, against where the vertices project: each player alone,
    // over the whole picture (no scenery in front), no ground marks.
    for (n, p) in players.iter().enumerate() {
        let mut alone = avatars.frame(&view, &stage, &[*p], 0.0).frame;
        alone.front = None;
        alone.ground.clear();
        let img = gpu.render_field(w, h, &none, Some(&alone), &DrawList::new());
        let (x0, y0, x1, y1) = model_bbox(&alone, 0, &none, w, h);
        let mut changed = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                if img[i..i + 3].iter().zip(&base[i..i + 3]).any(|(a, b)| a.abs_diff(*b) > 6) {
                    changed = (changed.0.min(x), changed.1.min(y), changed.2.max(x), changed.3.max(y));
                }
            }
        }
        eprintln!("player {}: model projects to ({x0:.1},{y0:.1})-({x1:.1},{y1:.1}); pixels changed ({},{})-({},{})", p.who.0, changed.0, changed.1, changed.2, changed.3);
        assert!(changed.0 != u32::MAX, "player {} is on the screen", p.who.0);
        // The model's own bounding box agrees with the pixels it changed to within a few pixels
        // (a hair tip thinner than a pixel may not light one).
        for (got, want) in [(changed.0 as f32, x0), (changed.1 as f32, y0), (changed.2 as f32 + 1.0, x1), (changed.3 as f32 + 1.0, y1)] {
            assert!((got - want).abs() <= 4.0, "player {}: pixels {got} against projected {want}", p.who.0);
        }
        // Upright: taller than wide, and the head above the feet is a fact of the projection.
        assert!(y1 - y0 > 20.0 && y1 - y0 < 600.0, "player {} is {:.0} px tall", p.who.0, y1 - y0);
        let _ = n;
    }

    // 2. The back half never shows: the picture has the same pure-green key pixels with the
    // avatars as without (md1stin has none in the whole picture; the model is not green).
    let green = |img: &[u8]| img.chunks_exact(4).filter(|p| p[..3] == [0, 255, 0]).count();
    let with = gpu.render_field(w, h, &scene, Some(&built.frame), &DrawList::new());
    assert_eq!(green(&with), green(&base), "no key-colour pixels appear");

    // 3. The front layer is drawn over a character wherever it is opaque (the flat split: see the
    // limits in `avatar`'s docs; this says nothing about whether the GAME would hide the character
    // there). The same frame without the front layer differs from the one with it inside the
    // character's rectangle, and the layer changes nothing where it covers no character, apart
    // from the edge pixels of its silhouettes, where it blends over an identical picture.
    let behind = behind.expect("a place where the front layer covers about half of a character");
    let stand = player(5, Some("ff7/tifa"), behind.pos, toward, AnimState::Idle, false, false);
    let with_front = avatars.frame(&view, &stage, &[stand], 0.0).frame;
    let covered = gpu.render_field(w, h, &none, Some(&with_front), &DrawList::new());
    let mut no_front = avatars.frame(&view, &stage, &[stand], 0.0).frame;
    no_front.front = None;
    let uncovered = gpu.render_field(w, h, &none, Some(&no_front), &DrawList::new());
    let (x0, y0, x1, y1) = model_bbox(&with_front, 0, &none, w, h);
    let differing = (y0 as u32..y1 as u32)
        .flat_map(|y| (x0 as u32..x1 as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let i = ((y * w + x) * 4) as usize;
            covered[i..i + 3].iter().zip(&uncovered[i..i + 3]).any(|(a, b)| a.abs_diff(*b) > 24)
        })
        .count();
    eprintln!(
        "front layer over a character: {:.0}% of its rectangle is under the layer (mostly {:?}); {differing} pixels differ with it",
        behind.covered * 100.0,
        dominant(behind.view_rect)
    );
    assert!(differing > 100, "the front layer is drawn over the character where it is opaque: {differing} pixels");
    let (cx, cy) = (((x0 + x1) / 2.0) as i64, ((y0 + y1) / 2.0) as i64);
    let half = 90i64;
    let (ox, oy) = ((cx - half).clamp(0, i64::from(w) - 2 * half) as u32, (cy - half).clamp(0, i64::from(h) - 2 * half) as u32);
    let (cw, ch, crop) = zoom_crop(&covered, w, ox, oy, 2 * half as u32, 2 * half as u32, 3);
    save_png("54-avatars-front-layer-over-character", cw, ch, &crop);
    let (cw, ch, crop) = zoom_crop(&uncovered, w, ox, oy, 2 * half as u32, 2 * half as u32, 3);
    save_png("54-avatars-front-layer-over-character-without-it", cw, ch, &crop);
    save("54-avatars-front-layer-over-character-full", w, h, &covered);

    // 4. The premise of drawing over the whole picture and then the front half over it: where
    // the front half is opaque, it IS the picture (so redrawing it changes nothing).
    let picture = view.backdrop.as_ref().expect("the picture");
    let front = stage.front.as_ref().expect("a front layer");
    assert_eq!((picture.width, picture.height), (front.width, front.height));
    let mut opaque = 0;
    for (a, b) in picture.rgba.chunks_exact(4).zip(front.rgba.chunks_exact(4)) {
        if b[3] == 255 {
            opaque += 1;
            assert_eq!(a[..3], b[..3], "the front layer's colour is the picture's");
        } else {
            assert_eq!(b, [0, 0, 0, 0], "everything else of it is untouched (premultiplied)");
        }
    }
    assert!(opaque > 10_000, "{opaque} px of scenery in front");
}

/// The non-player characters of two real fields (`md1stin`, the platform with its two guards, and
/// `md1_1`, the next field, with five characters), drawn as the models their data names, through
/// the real renderer, by the window's own `field_draw` and label anchors, next to two players for
/// scale: PNGs to look at (`60-npcs-<field>.png`, a 1920x1080 one, and a 3x close-up of every
/// character, `61-...`) and numbers: how many characters the data reported, how many became models,
/// markers or were culled, where each one's feet are on the screen against where it stands, whether
/// its name is over its head, and which way each faces against the camera. `FMV_FF7_AREAS` (comma
/// separated) picks other fields.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA, FMV_FF7_FLEVEL or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_npcs_in_two_fields() {
    use crate::backdrop::FieldView;
    use crate::npc::npc_who;
    use crate::session::SessionInfo;
    use crate::state::ClientState;
    use crate::ui::hud::{self, Subject};
    use crate::window::{field_draw, label_anchor};
    use fmv_adapter_api::AreaRuntime;
    use fmv_core::{AccountId, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId};
    use fmv_protocol::{NpcSnapshot, ServerMsg, NPC_NAME_CHARS, NPC_REF_CHARS};
    use real::*;

    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let keys = std::env::var("FMV_FF7_AREAS").unwrap_or_else(|_| "md1stin,md1_1".to_string());
    for key in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        let view = timed(&format!("{key}: field view with the scenery in front"), || FieldView::ff7_with(&real.adapter, key, true)).expect("field view");
        let stage = view.stage.clone().expect("an FF7 field has a stage");
        let area = real.adapter.load_field(key).expect("field");

        // What the server announces (its own rule: the unconditional characters, texts clipped),
        // as the wire carries it; the client reads it through `NpcView::from_snapshot` like live.
        let clip = |text: &str, max: usize| -> String { text.chars().filter(|c| !c.is_control()).take(max).collect() };
        let reported = area.npcs();
        let announced: Vec<NpcSnapshot> = reported
            .iter()
            .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
            .enumerate()
            .map(|(i, n)| NpcSnapshot {
                id: i as u32,
                name: clip(&n.entity_name, NPC_NAME_CHARS),
                model: clip(&n.appearance.model, NPC_REF_CHARS),
                idle_animation: n.appearance.idle_animation.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
                pos: n.pos,
                facing: n.facing,
            })
            .collect();
        eprintln!(
            "{key}: the data reports {} character(s), {} announced: {:?}",
            reported.len(),
            announced.len(),
            announced.iter().map(|n| (n.name.as_str(), n.model.as_str(), n.idle_animation.as_deref())).collect::<Vec<_>>()
        );
        assert!(!announced.is_empty(), "{key} has characters to draw");

        // Two players for scale: Cloud (the local one) beside the first character, and Tifa at the
        // spot that is farthest on the screen from every character and from Cloud, among the spots
        // around the characters that the area accepts as they are and that are not much farther
        // from the camera than Cloud (so she is not tiny).
        let ring = |npc: Pos| -> Vec<Pos> {
            [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7), (-0.7, -0.7)]
                .iter()
                .filter_map(|(dx, dy)| {
                    let want = Pos::new(npc.x + dx * 170.0, npc.y + dy * 170.0, npc.z);
                    let placed = area.place_arrival(want, None);
                    (placed.is_finite() && placed.dist_xy(want) < 1.0).then_some(placed)
                })
                .collect()
        };
        let spawn = area.spawn_points()[0];
        let own_pos = ring(announced[0].pos).first().copied().unwrap_or(spawn);
        let depth = |p: Pos| stage.camera.project([f64::from(p.x), f64::from(p.y), f64::from(p.z)]).map_or(f64::MAX, |s| s[2]);
        let on_screen = |p: Pos| view.to_view(p).map_or((f32::MAX, f32::MAX), |v| (v.x, v.y));
        let taken: Vec<(f32, f32)> = announced.iter().map(|n| on_screen(n.pos)).chain([on_screen(own_pos)]).collect();
        let room = |p: Pos| {
            let (x, y) = on_screen(p);
            taken.iter().map(|t| (t.0 - x).hypot(t.1 - y)).fold(f32::MAX, f32::min)
        };
        let peer_pos = announced
            .iter()
            .flat_map(|n| ring(n.pos))
            .filter(|p| depth(*p) < 1.6 * depth(own_pos))
            .max_by(|a, b| room(*a).total_cmp(&room(*b)))
            .unwrap_or(spawn);
        let forward = {
            let cams = area.cameras().expect("cameras");
            let axes = cams[0].axes_f32();
            [f64::from(axes[2][0]), f64::from(axes[2][1]), f64::from(axes[2][2])]
        };
        let toward = toward_camera(forward);
        let area_id = AreaId { game: GameId::Ff7, key: key.to_string() };
        let own_tint = [90, 125, 245];
        let mut state = ClientState::new(EntityId(1), AccountId(1), own_pos, area_id.clone());
        state.change_area(area_id.clone(), own_pos, toward, false);
        let entity = |id: u64, pos: Pos, facing: f32, name: &str, look: &str, tint: [u8; 3]| EntitySnapshot {
            id: EntityId(id),
            pos,
            facing,
            anim: AnimState::Idle,
            avatar: AvatarAppearance::new(name, tint, Some(look.to_string())),
        };
        let snapshot = ServerMsg::AreaSnapshot {
            area: area_id.clone(),
            tick: 1,
            entities: vec![
                entity(1, own_pos, toward, "Cloud", "ff7/cloud", own_tint),
                entity(2, peer_pos, toward + 0.9, "Tifa", "ff7/tifa", [235, 80, 80]),
            ],
            npcs: announced.clone(),
        };
        assert!(state.apply(0.0, snapshot).is_none());
        let npcs = state.npc_views().to_vec();
        assert_eq!(npcs.len(), announced.len(), "the client keeps every character it was told of");

        // The window's own composition of the frame, twice: the first call loads the models the
        // area names (the epoch moved), the second is the steady state.
        let mut avatars = avatars(&real);
        let mut epoch = 0;
        let render_t = 1.0;
        timed(&format!("{key}: first frame (loads the characters)"), || field_draw(Some(&mut avatars), &mut epoch, &view, &state, own_tint, render_t, 0.0));
        let draw = timed(&format!("{key}: a frame in the steady state"), || field_draw(Some(&mut avatars), &mut epoch, &view, &state, own_tint, render_t, 0.0));
        let built = draw.built.as_ref().expect("the models were built");
        let drawn: Vec<usize> =
            built.frame.models.iter().enumerate().filter(|(_, m)| m.asset.appearance.appearance_ref == "npc").map(|(i, _)| i).collect();
        let marker_npcs: Vec<usize> = built.markers.iter().copied().filter(|&i| i < npcs.len()).collect();
        let culled = npcs.len() - drawn.len() - marker_npcs.len();
        eprintln!(
            "{key}: {} announced = {} drawn as models + {} markers {:?} + {culled} culled (behind the camera); {} avatar model(s) in all, {} player(s)",
            npcs.len(),
            drawn.len(),
            marker_npcs.len(),
            marker_npcs.iter().map(|&i| npcs[i].name.as_str()).collect::<Vec<_>>(),
            built.frame.models.len(),
            built.frame.models.len() - drawn.len(),
        );
        assert!(!drawn.is_empty(), "{key}: at least one character is a model");
        if matches!(key, "md1stin" | "md1_1") {
            // Measured on the owner's data: all of them load (the guards, the three AVALANCHE
            // members and the two door leaves), none is left a marker, none is out of view.
            assert_eq!((drawn.len(), marker_npcs.len(), culled), (npcs.len(), 0, 0), "{key}: every character is a model");
        }
        assert_eq!(built.frame.models.len() - drawn.len(), 2, "both players are models too");

        // Pixels.
        let scene = Scene { bounds: view.bounds, triangles: &[], players: &draw.markers, fixture_banner: false, backdrop: view.backdrop.as_ref() };
        let info = SessionInfo { name: "Cloud".into(), tint: own_tint, area_id: area_id.clone(), fixture: false };
        let anchor = |subject: Subject, at: Pos| label_anchor(draw.built.as_ref(), &view, draw.own_who, subject, at);
        let hud_input = hud::gather_anchored(&state, &info, &view, render_t, &anchor);
        for npc in &npcs {
            assert!(hud_input.labels.iter().any(|l| l.text == npc.name && l.kind == hud::LabelKind::Npc), "{key}: {} is named", npc.name);
        }
        for (w, h, tag) in [(1100u32, 680u32, ""), (1920, 1080, "-1920x1080")] {
            let mut overlay = DrawList::new();
            hud::draw_hud(&hud_input, w, h, &mut overlay);
            let img = timed(&format!("{key}: render {w}x{h}"), || gpu.render_field(w, h, &scene, Some(&built.frame), &overlay));
            save(&format!("60-npcs-{key}{tag}"), w, h, &img);
            if w != 1920 {
                continue;
            }
            // Where the feet should be on this screen, from the character's own position and the
            // field camera, against where its model's lowest vertices are.
            let cam = Camera::fit(&scene.bounds, w as f32, h as f32, MARGIN_PX);
            let feet = |p: Pos| {
                let c = stage.camera.to_camera([f64::from(p.x), f64::from(p.y), f64::from(p.z)]);
                project_pixel(&cam, built.frame.zoom, w, h, [c[0] as f32, c[1] as f32, c[2] as f32])
            };
            let boxes: Vec<(f32, f32, f32, f32)> = drawn.iter().map(|&m| model_bbox(&built.frame, m, &scene, w, h)).collect();
            let player_heights: Vec<f32> =
                (0..built.frame.models.len()).filter(|i| !drawn.contains(i)).map(|m| { let b = model_bbox(&built.frame, m, &scene, w, h); b.3 - b.1 }).collect();
            for (i, npc) in npcs.iter().enumerate() {
                if marker_npcs.contains(&i) {
                    continue;
                }
                let (fx, fy) = feet(npc.pos);
                // The model of this character is the one whose bottom centre is nearest to its feet.
                let Some((n, near)) = boxes
                    .iter()
                    .enumerate()
                    .map(|(n, b)| (n, ((b.0 + b.2) / 2.0 - fx).hypot(b.3 - fy)))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                else {
                    continue;
                };
                let (x0, y0, x1, y1) = boxes[n];
                // Which way it faces: the heading of the character against the one that looks
                // at the camera, in degrees (0 = at the camera, 180 = away from it).
                let mut turn = (npc.facing - toward).to_degrees().rem_euclid(360.0);
                if turn > 180.0 {
                    turn -= 360.0;
                }
                let anchor_y = built.anchors.get(&npc_who(npc.id)).map(|a| a.y);
                eprintln!(
                    "{key}: {} ({}, clip {:?}) at ({:.0}, {:.0}, {:.0}) facing {:.0} deg = {:+.0} deg from the camera heading; feet at pixel ({fx:.0}, {fy:.0}), model box ({x0:.0},{y0:.0})-({x1:.0},{y1:.0}) = {:.0} px tall (players {:?}); label anchor y {:?}; model-to-feet distance {near:.1} px",
                    npc.name,
                    npc.model,
                    npc.idle_animation,
                    npc.pos.x,
                    npc.pos.y,
                    npc.pos.z,
                    npc.facing.to_degrees(),
                    turn,
                    y1 - y0,
                    player_heights.iter().map(|h| h.round()).collect::<Vec<_>>(),
                    anchor_y,
                );
                // (A humanoid's soles are within a few pixels; a door or the like has its origin
                // elsewhere. A wrong height would be off by far more than this.)
                assert!(near < 20.0, "{key}: the feet of {} are on the floor where it stands ({near:.1} px off)", npc.name);
                assert!(anchor_y.is_some_and(|y| y > view.to_view(npc.pos).map_or(0.0, |p| p.y) + 10.0), "{key}: the name of {} is over its head", npc.name);
                // A 3x close-up around it.
                let (cx, cy) = (((x0 + x1) / 2.0) as i64, ((y0 + y1) / 2.0) as i64);
                let half = (((x1 - x0).max(y1 - y0) / 2.0) as i64 + 20).clamp(40, 300);
                let (ox, oy) = ((cx - half).clamp(0, i64::from(w) - 2 * half) as u32, (cy - half).clamp(0, i64::from(h) - 2 * half) as u32);
                let (cw, ch, crop) = zoom_crop(&img, w, ox, oy, 2 * half as u32, 2 * half as u32, 3);
                save_png(&format!("61-npcs-{key}-closeup-{i}-{}", npc.name), cw, ch, &crop);
            }
        }
    }
}

/// The pixel rectangle (left, top, right, bottom) that model `model` of `frame` covers in a `w` x
/// `h` window drawn for `scene`, from the projected vertices of every triangle.
fn model_bbox(frame: &AvatarFrame, model: usize, scene: &Scene<'_>, w: u32, h: u32) -> (f32, f32, f32, f32) {
    let cam = Camera::fit(&scene.bounds, w as f32, h as f32, MARGIN_PX);
    let m = &frame.models[model];
    let mut used = vec![false; m.positions.len()];
    for &i in &m.asset.mesh.indices {
        used[i as usize] = true;
    }
    let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (p, _) in m.positions.iter().zip(&used).filter(|(_, u)| **u) {
        let (x, y) = project_pixel(&cam, frame.zoom, w, h, *p);
        bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
    }
    bbox
}

/// All eight playable characters of the real archive load, merge and fit their clips; prints what
/// each costs (load and merge time, vertices, triangles, draw batches) and checks the merged mesh
/// is whole. No GPU needed.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL)"]
fn real_characters_load_merge_and_measure() {
    use crate::avatar::library::AvatarAsset;
    use crate::avatar::source::{CharacterLoader, LgpLoader};
    use std::time::Instant;
    let Some(real) = real::open() else { return };
    let mut loader = LgpLoader::new(&real.char_lgp);
    let started = Instant::now();
    for model in fmv_ff7::playable_field_models() {
        let t = Instant::now();
        let character = loader.load(&model).expect("the playable character loads");
        let loaded = t.elapsed();
        let t = Instant::now();
        let asset = AvatarAsset::new(character).expect("the model merges");
        let merged = t.elapsed();
        let mesh = &asset.mesh;
        let textured = mesh.batches.iter().filter(|b| b.texture.is_some()).count();
        let blended = mesh.batches.iter().filter(|b| b.kind != crate::avatar::mesh::BatchKind::Opaque).count();
        eprintln!(
            "{:12} load {:5.1?} merge {:5.1?}: {:5} vertices, {:5} triangles, {} batches ({textured} textured, {blended} blended), head {:.1} units",
            model.appearance_ref,
            loaded,
            merged,
            mesh.positions.len(),
            mesh.triangle_count(),
            mesh.batches.len(),
            asset.height_units
        );
        // A whole mesh: every vertex belongs to one bone run, runs tile the vertices in order, and
        // every index is in range.
        let mut next = 0;
        for run in &mesh.bone_runs {
            assert_eq!(run.start, next);
            assert!(run.bone < asset.character.model.bones.len());
            next += run.len;
        }
        assert_eq!(next, mesh.positions.len());
        assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
        assert!(asset.height_units > 5.0 && asset.height_units < 60.0, "{}", asset.height_units);
        // All three clips fit the skeleton (the loader checked) and pose without trouble.
        for state in [AnimState::Idle, AnimState::Walk, AnimState::Run] {
            let clip = asset.character.animation(state);
            assert!(clip.pose_at(&asset.character.model.bones, 0.5).is_some());
        }
    }
    eprintln!("all eight playable characters: {:.1?}", started.elapsed());
}

/// How much do the frames of the real idle clips differ? (They do not: idle is a still pose, see
/// `anim`.) Prints the largest bone angle step and root move between consecutive frames (looping)
/// for every playable character, and checks that none of them moves. No GPU needed.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL)"]
fn real_idle_clips_measured() {
    use crate::avatar::source::{CharacterLoader, LgpLoader};
    let Some(real) = real::open() else { return };
    let mut loader = LgpLoader::new(&real.char_lgp);
    for model in fmv_ff7::playable_field_models() {
        let character = loader.load(&model).expect("the playable character loads");
        let frames = character.idle.frames();
        let (mut angle, mut root) = (0.0f32, 0.0f32);
        for i in 0..frames.len() {
            let (a, b) = (&frames[i], &frames[(i + 1) % frames.len()]);
            for (x, y) in a.rotations.iter().zip(&b.rotations) {
                for k in 0..3 {
                    angle = angle.max(((x[k] - y[k] + 540.0).rem_euclid(360.0) - 180.0).abs());
                }
            }
            root = root.max((0..3).map(|k| (a.root_translation[k] - b.root_translation[k]).abs()).fold(0.0, f32::max));
        }
        eprintln!(
            "{:12} idle {} frames: largest bone angle step {angle:6.2} deg, largest root move {root:.3} model units; walk {} frames, run {} frames",
            model.appearance_ref,
            frames.len(),
            character.walk.frame_count(),
            character.run.frame_count()
        );
        assert!(angle < 0.01 && root < 0.001, "{}: the idle clip moves, so `anim` must play it", model.appearance_ref);
    }
}
