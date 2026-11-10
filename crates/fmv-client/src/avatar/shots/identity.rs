//! The player's identity colour on the field models, before and after, through the real renderer (`Gpu::draw_field`,
//! the avatar pipelines) with the owner's own `char.lgp` and `md1stin` (read-only): PNGs to look at and numbers.
//!
//! Every pair is drawn from ONE frame (the same models, poses and pixels) three ways: untinted (the model's own
//! colours), with the OLD look (the multiplicative `0.45 + 0.55 * c / 255` tint of `frame::body_tint`, kept in the
//! tests as the reference) and with the NEW one (the frame as `Avatars::frame` built it: an identity colour per body).
//! Run with `FMV_FF7_LIBRARY` (the state directory of the imported library) or `FMV_FF7_DATA`, and `FMV_SHOT_DIR`:
//!
//! ```text
//! cargo test -p fmv-client -- --ignored offscreen_field_identity --test-threads=1 --nocapture
//! ```

use super::real::{self, flat_front, open, save_png, spots, toward_camera, Real, Spot};
use super::*;
use crate::avatar::frame::{body_tint, SkinnedModel};
use crate::avatar::gpu::Tint;
use crate::avatar::identity::look;
use crate::avatar::identity::pics::{around, crop, hstack, vstack, Img};
use crate::avatar::identity::{measure, Identity};
use crate::backdrop::FieldView;
use fmv_ff7::playable::PLACEHOLDER_BODIES;

const COLOURS: [(&str, [u8; 3]); 4] = [("blue", [70, 130, 245]), ("orange", [240, 120, 40]), ("green", [60, 200, 90]), ("magenta", [210, 60, 200])];

/// The real `md1stin`: its camera and the places on the walkmesh where a character is a good size and nothing in front
/// covers it.
struct Fixture {
    view: FieldView,
    stage: crate::avatar::Stage,
    open_places: Vec<Spot>,
    toward: f32,
}

fn fixture(real: &Real, avatars: &mut Avatars) -> Fixture {
    fixture_of(real, avatars, "md1stin")
}

fn fixture_of(real: &Real, avatars: &mut Avatars, key: &str) -> Fixture {
    let view = FieldView::ff7_with(&real.adapter, key, true).expect("field view");
    let stage = view.stage.clone().expect("an FF7 field has a stage");
    let area = real.adapter.load_field(key).expect("field");
    let flat = flat_front(&area);
    let candidates = spots(avatars, &view, &stage, &area, flat.as_ref(), (450.0, 1300.0));
    let mut open_places: Vec<Spot> = candidates.iter().copied().filter(|s| s.covered == 0.0).collect();
    if open_places.is_empty() {
        // A field whose camera is far or near: any depth will do for a picture of one character.
        let wide = spots(avatars, &view, &stage, &area, flat.as_ref(), (150.0, 6000.0));
        let least = wide.iter().map(|s| s.covered).fold(f32::MAX, f32::min);
        open_places = wide.into_iter().filter(|s| s.covered <= least + 0.02).collect();
    }
    let toward = {
        let cams = area.cameras().expect("cameras");
        let axes = cams[0].axes_f32();
        toward_camera([f64::from(axes[2][0]), f64::from(axes[2][1]), f64::from(axes[2][2])])
    };
    Fixture { view, stage, open_places, toward }
}

fn body_player(id: u64, body: usize, at: Pos, facing: f32, tint: [u8; 3]) -> AvatarPlayer<'static> {
    AvatarPlayer {
        who: Who(id),
        pos: at,
        facing,
        anim: AnimState::Idle,
        scene_frame: None,
        appearance: Some(PLACEHOLDER_BODIES[body].model.appearance_ref),
        npc: None,
        tint,
        own: false,
        party: false,
    }
}

/// How a frame is drawn: the model's own colours, the OLD look (the multiplicative tint) or the NEW one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Look {
    Plain,
    Old,
    New,
}

/// `frame` as `look` draws it: the same models and positions, only their colours differ.
fn dressed(frame: &AvatarFrame, look: Look) -> AvatarFrame {
    let models = frame.models.iter().map(|m| SkinnedModel { asset: Arc::clone(&m.asset), positions: m.positions.clone() }).collect();
    let bytes = |i: &Identity| i.rgb.map(|c| (c * 255.0).round() as u8);
    let (tints, identities) = match look {
        Look::Plain => (Vec::new(), Vec::new()),
        Look::Old => (frame.identities.iter().map(|i| if i.strength > 0.0 { body_tint(bytes(i)) } else { Tint::NONE }).collect(), Vec::new()),
        Look::New => (frame.tints.clone(), frame.identities.clone()),
    };
    AvatarFrame { models, tints, identities, zoom: frame.zoom, overlay_bias: frame.overlay_bias, ground: frame.ground.clone(), scenery: frame.scenery.clone() }
}

const W1: u32 = 1920;
const H1: u32 = 1080;

/// One field frame through the window's own pass order.
fn render(gpu: &mut Offscreen, fx: &Fixture, frame: &AvatarFrame, backdrop: bool) -> Vec<u8> {
    let scene = Scene { bounds: fx.view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: if backdrop { fx.view.backdrop.as_ref() } else { None } };
    gpu.render_field(W1, H1, &scene, Some(frame), &DrawList::new())
}

fn bounds_scene(fx: &Fixture) -> Scene<'_> {
    Scene { bounds: fx.view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: None }
}

/// A frame of `players` as the window builds it, standing on the nearest open place (or the `at`th), and the same frame
/// with no scenery and no marks for the per-pixel comparisons.
fn frame_of(avatars: &mut Avatars, fx: &Fixture, players: &[AvatarPlayer<'_>]) -> AvatarFrame {
    avatars.frame(&fx.view, &fx.stage, players, 0.0).frame
}

fn bare(frame: &AvatarFrame) -> AvatarFrame {
    let mut f = dressed(frame, Look::New);
    f.scenery = None;
    f.ground.clear();
    f
}

fn to_f(px: &[u8]) -> [f32; 3] {
    [f32::from(px[0]) / 255.0, f32::from(px[1]) / 255.0, f32::from(px[2]) / 255.0]
}

fn mean_abs(a: &[u8], b: &[u8]) -> f32 {
    (0..3).map(|k| f32::from(a[k].abs_diff(b[k]))).sum::<f32>() / 3.0
}

/// The mean per-channel change between two pictures over `pixels` (byte offsets of RGBA).
fn mean_change(a: &[u8], b: &[u8], pixels: &[usize]) -> f32 {
    if pixels.is_empty() {
        return 0.0;
    }
    pixels.iter().map(|&i| mean_abs(&a[i..i + 3], &b[i..i + 3])).sum::<f32>() / pixels.len() as f32
}

/// The cloth colours of the numeric table: the dark blue cloak the owner measured, a dark suit grey, a skin tone, a
/// white shirt and a saturated red.
/// The mean per-channel change (levels of 255) accepted over skin and face pixels: the weak tint moves a channel by at
/// most 5% (13 levels on the brightest), which is at most 7 on average over the channels of the lightest skin tones; the
/// measured worst on the rendered bodies is 5.0, the old tint's 28 to 57.
const SKIN_ACCEPTED: f32 = 7.0;

const SAMPLES: [(&str, [u8; 3]); 5] = [("dark cloak", [57, 66, 109]), ("dark suit grey", [48, 48, 52]), ("skin", [230, 180, 150]), ("white shirt", [235, 235, 235]), ("saturated red", [200, 30, 30])];

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_identity_before_after() {
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut avatars = real::avatars(&real);
    let fx = fixture(&real, &mut avatars);
    let nearest = fx.open_places.iter().copied().min_by(|a, b| a.depth.total_cmp(&b.depth)).expect("an open place");
    eprintln!("close-ups stand at camera depth {:.0}", nearest.depth);

    // ---- the numeric table: the old function against the new, sample colours and player colours ---------------
    eprintln!("sample colours, old multiplicative tint -> new look (cloth, no face flag); the face decal row is the new look on a face part");
    for (cname, rgb) in COLOURS {
        let id = rgb.map(|c| f32::from(c) / 255.0);
        eprintln!("player colour {cname} {rgb:?}");
        for (sname, s) in SAMPLES {
            let c = to_f(&s);
            let to_u8 = |v: [f32; 3]| v.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
            eprintln!("  {sname:15} {s:?}  old {:?}  new {:?}  new-as-face-decal {:?}", to_u8(look::old(c, id)), to_u8(look::look(c, id, 1.0, 0.0)), to_u8(look::look(c, id, 1.0, 1.0)));
        }
    }

    // ---- the pictures: every body in every colour, untinted | old | new, at 3x --------------------------------
    let (mut worst_skin_new, mut worst_skin_old) = (0.0f32, 0.0f32);
    let mut oracle_total = (0usize, 0usize);
    let none = bounds_scene(&fx);
    let empty = gpu.render_field(W1, H1, &none, None, &DrawList::new());
    for (b, body) in PLACEHOLDER_BODIES.iter().enumerate() {
        for (cname, tint) in COLOURS {
            let player = body_player(1, b, nearest.pos, fx.toward + 0.45, tint);
            let frame = frame_of(&mut avatars, &fx, &[player]);
            assert_eq!(frame.identities, [Identity::of(tint)], "the body carries its player's colour");
            // The pictures, with the picture of the field behind (scenery and marks as the window draws them).
            let panels: Vec<Img> = [Look::Plain, Look::Old, Look::New]
                .into_iter()
                .map(|look| {
                    let img = render(&mut gpu, &fx, &dressed(&frame, look), true);
                    around(&img, W1, H1, model_bbox(&frame, 0, &none, W1, H1), 18.0, 3)
                })
                .collect();
            let sheet = hstack(&panels);
            save_png(&format!("field_tint_before_after_{}_{cname}", body.archetype), sheet.w, sheet.h, &sheet.px);

            // The numbers, on a flat background without marks: model pixels are those the model changed.
            let flat = bare(&frame);
            let (plain, old, new) = (
                render(&mut gpu, &fx, &dressed(&flat, Look::Plain), false),
                render(&mut gpu, &fx, &dressed(&flat, Look::Old), false),
                render(&mut gpu, &fx, &dressed(&flat, Look::New), false),
            );
            let model_px: Vec<usize> = (0..plain.len()).step_by(4).filter(|&i| plain[i..i + 3] != empty[i..i + 3]).collect();
            assert!(model_px.len() > 3000, "{} in {cname}: the body is on the screen ({} pixels)", body.archetype, model_px.len());
            // The oracle: the CPU mirror of the look, applied to the untinted pixel, is the new pixel (the vertex
            // colours of these bodies are flat, so the untinted pixel is the lit colour). The pixels it does not
            // reach are the ones the keep flag acts on (the face decals, and the hair and skin of the head bone),
            // which keep their own colours.
            let id = tint.map(|c| f32::from(c) / 255.0);
            let (mut agree, mut face_px) = (0usize, Vec::new());
            for &i in &model_px {
                let want = look::look(to_f(&plain[i..]), id, 1.0, 0.0).map(|c| (c.clamp(0.0, 1.0) * 255.0).round());
                if (0..3).all(|k| (want[k] - f32::from(new[i + k])).abs() <= 2.0) {
                    agree += 1;
                } else {
                    face_px.push(i);
                }
            }
            oracle_total = (oracle_total.0 + agree, oracle_total.1 + model_px.len());
            // The skin region: what the untinted picture says is skin.
            let skin_px: Vec<usize> = model_px.iter().copied().filter(|&i| look::skin_mask(to_f(&plain[i..])) > 0.5).collect();
            let (skin_new, skin_old) = (mean_change(&plain, &new, &skin_px), mean_change(&plain, &old, &skin_px));
            let (face_new, face_old) = (mean_change(&plain, &new, &face_px), mean_change(&plain, &old, &face_px));
            let changed = |img: &[u8]| model_px.iter().filter(|&&i| mean_abs(&plain[i..i + 3], &img[i..i + 3]) > 6.0).count();
            eprintln!(
                "{:7} {cname:8}: {} px; {:.1}% match the mirror; skin ({} px) mean |change| new {skin_new:.1} old {skin_old:.1}; pixels the face flag acted on ({}, a part of the face region) new {face_new:.1} old {face_old:.1}; changed by >6: new {} old {}",
                body.archetype,
                model_px.len(),
                100.0 * agree as f32 / model_px.len() as f32,
                skin_px.len(),
                face_px.len(),
                changed(&new),
                changed(&old),
            );
            worst_skin_new = worst_skin_new.max(skin_new);
            worst_skin_old = worst_skin_old.max(skin_old);
            assert!(skin_px.is_empty() || skin_new <= SKIN_ACCEPTED, "{} in {cname}: skin changes by {skin_new:.1} levels on average", body.archetype);
            assert!(face_px.is_empty() || face_new <= SKIN_ACCEPTED, "{} in {cname}: face decals change by {face_new:.1} levels on average", body.archetype);
        }
    }
    eprintln!("worst mean per-channel change over the skin region: new {worst_skin_new:.1} levels, old {worst_skin_old:.1}; the shader agrees with the mirror at {:.2}% of {} body pixels", 100.0 * oracle_total.0 as f32 / oracle_total.1 as f32, oracle_total.1);
    // (The pixels that do not match are those the keep flag acts on: the decals and the head's hair and skin, which keep
    // their own colours; each case above asserts how little they change. They are a head's worth of a body: under 12% here.)
    assert!(oracle_total.0 as f32 > 0.85 * oracle_total.1 as f32, "the shader is the mirror (but for the decals and the head's hair, which keep their colours)");
}

/// All three bodies at once on the real field, a player each in a different colour: the old look and the new at 1920x1080
/// (what the window shows), and a strip of the part of the screen they stand in at 1x, old above new: the owner's
/// criterion is that two players' colours are told apart at a glance at that scale.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_identity_overview() {
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut avatars = real::avatars(&real);
    let fx = fixture(&real, &mut avatars);
    // Side by side: the places of the walkmesh (a good size, nothing in front) whose characters stand furthest apart on
    // the screen, by the gap between the nearest two of their rectangles. Three players if the corridor lets them stand
    // without one covering another (by the projected boxes of their models), else two.
    let sized: Vec<Spot> = fx.open_places.iter().copied().filter(|s| s.depth < 1100.0).collect();
    let gap = |a: &Spot, b: &Spot| (b.view_rect.0 - a.view_rect.2).max(a.view_rect.0 - b.view_rect.2);
    let combos = |k: usize| -> Vec<Vec<usize>> {
        let mut out = vec![Vec::new()];
        for _ in 0..k {
            out = out.into_iter().flat_map(|c: Vec<usize>| (c.last().map_or(0, |l| l + 1)..sized.len()).map(move |i| [c.clone(), vec![i]].concat())).collect();
        }
        out
    };
    let cast = [(0usize, COLOURS[0].1), (1, COLOURS[1].1), (2, COLOURS[2].1)];
    let none = bounds_scene(&fx);
    let mut shown = None;
    for n in [3usize, 2] {
        let best = combos(n)
            .into_iter()
            .max_by(|a, b| {
                let widest = |c: &Vec<usize>| c.iter().enumerate().flat_map(|(x, &i)| c[x + 1..].iter().map(move |&j| (i, j))).map(|(i, j)| gap(&sized[i], &sized[j])).fold(f32::MAX, f32::min);
                widest(a).total_cmp(&widest(b))
            })
            .expect("places");
        let mut places: Vec<Spot> = best.iter().map(|&i| sized[i]).collect();
        places.sort_by(|a, b| a.view_rect.0.total_cmp(&b.view_rect.0));
        let players: Vec<AvatarPlayer<'static>> = cast
            .iter()
            .zip(&places)
            .enumerate()
            .map(|(i, ((body, tint), place))| body_player(i as u64 + 1, *body, place.pos, fx.toward + 0.3 - 0.3 * i as f32, *tint))
            .collect();
        let frame = frame_of(&mut avatars, &fx, &players);
        assert_eq!(frame.models.len(), n);
        let mut boxes: Vec<(f32, f32, f32, f32)> = (0..n).map(|m| model_bbox(&frame, m, &none, W1, H1)).collect();
        boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
        eprintln!("{n} players at camera depths {:?}: pixel boxes left to right {boxes:?}", places.iter().map(|p| p.depth.round()).collect::<Vec<_>>());
        if boxes.windows(2).all(|w| w[0].2 < w[1].0) {
            shown = Some((frame, n));
            break;
        }
        eprintln!("{n} players do not stand side by side in this corridor (one covers another)");
    }
    let (frame, n) = shown.expect("two players stand side by side");
    let (old, new) = (render(&mut gpu, &fx, &dressed(&frame, Look::Old), true), render(&mut gpu, &fx, &dressed(&frame, Look::New), true));
    save_png("field_tint_overview_old_1920x1080", W1, H1, &old);
    save_png("field_tint_overview_new_1920x1080", W1, H1, &new);
    // The strip: the part of the screen all three stand in, at 1x.
    let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for m in 0..n {
        let b = model_bbox(&frame, m, &none, W1, H1);
        bbox = (bbox.0.min(b.0), bbox.1.min(b.1), bbox.2.max(b.2), bbox.3.max(b.3));
    }
    let (x0, y0) = (((bbox.0 - 30.0).max(0.0)) as u32, ((bbox.1 - 20.0).max(0.0)) as u32);
    let (cw, ch) = ((((bbox.2 + 30.0) as u32).min(W1) - x0), (((bbox.3 + 20.0) as u32).min(H1) - y0));
    let strip = vstack(&[crop(&old, W1, (x0, y0), (cw, ch), 1), crop(&new, W1, (x0, y0), (cw, ch), 1)]);
    eprintln!("1x strip {}x{} at ({x0},{y0}); {n} players: blue lean, orange sturdy, green slight", strip.w, strip.h);
    save_png("field_tint_overview_strip_1x_old_over_new", strip.w, strip.h, &strip.px);
    // Told apart at a glance: the mean colour of each body's pixels (what the player sees at 1x) differs between the
    // three players by a good margin in the new look, where in the old one dark and saturated cloth hardly moved.
    let flat_old = render(&mut gpu, &fx, &bare_dressed(&frame, Look::Old), false);
    let flat_new = render(&mut gpu, &fx, &bare_dressed(&frame, Look::New), false);
    let flat_plain = render(&mut gpu, &fx, &bare_dressed(&frame, Look::Plain), false);
    let empty = gpu.render_field(W1, H1, &none, None, &DrawList::new());
    let mean_of = |img: &[u8], m: usize| -> [f32; 3] {
        let (a, b, c, d) = model_bbox(&frame, m, &none, W1, H1);
        let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
        for y in (b.max(0.0) as u32)..(d as u32).min(H1) {
            for x in (a.max(0.0) as u32)..(c as u32).min(W1) {
                let i = ((y * W1 + x) * 4) as usize;
                if flat_plain[i..i + 3] != empty[i..i + 3] {
                    let p = to_f(&img[i..]);
                    sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                    n += 1.0;
                }
            }
        }
        sum.map(|s| s * 255.0 / n.max(1.0))
    };
    let dist = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let (mut old_min, mut new_min) = (f32::MAX, f32::MAX);
    for (m, k) in (0..n).flat_map(|m| (m + 1..n).map(move |k| (m, k))) {
        let (od, nd) = (dist(mean_of(&flat_old, m), mean_of(&flat_old, k)), dist(mean_of(&flat_new, m), mean_of(&flat_new, k)));
        eprintln!("mean colour distance between models {m} and {k}: old {od:.0}, new {nd:.0} (of 441)");
        old_min = old_min.min(od);
        new_min = new_min.min(nd);
    }
    eprintln!("closest pair of players: old {old_min:.0}, new {new_min:.0}");
}

fn bare_dressed(frame: &AvatarFrame, look: Look) -> AvatarFrame {
    let mut f = dressed(frame, look);
    f.scenery = None;
    f.ground.clear();
    f
}

/// A white, grey or black identity colour has no hue to paint with (the colour sliders allow them): through the real
/// renderer a white one leaves every body as it is, and grey and black darken it as the old tint did, without colouring
/// it (the pixels that were grey stay grey). The shader agrees with the mirror there too, on the cloth: what the model keeps
/// as its own colours (the head's skin and hair, the face decals) is its own.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_identity_white_grey_and_black_colours_keep_the_old_look() {
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut avatars = real::avatars(&real);
    let fx = fixture(&real, &mut avatars);
    let nearest = fx.open_places.iter().copied().min_by(|a, b| a.depth.total_cmp(&b.depth)).expect("an open place");
    let none = bounds_scene(&fx);
    let empty = gpu.render_field(W1, H1, &none, None, &DrawList::new());
    for (name, tint) in [("white", [255u8, 255, 255]), ("grey", [128, 128, 128]), ("black", [0, 0, 0])] {
        let id = tint.map(|c| f32::from(c) / 255.0);
        for (b, body) in PLACEHOLDER_BODIES.iter().enumerate() {
            let flat = bare(&frame_of(&mut avatars, &fx, &[body_player(1, b, nearest.pos, fx.toward + 0.45, tint)]));
            let plain = render(&mut gpu, &fx, &dressed(&flat, Look::Plain), false);
            let new = render(&mut gpu, &fx, &dressed(&flat, Look::New), false);
            let model_px: Vec<usize> = (0..plain.len()).step_by(4).filter(|&i| plain[i..i + 3] != empty[i..i + 3]).collect();
            // The mirror has two outcomes for a pixel: the look of CLOTH (`face` 0) and the look of what the model keeps
            // as its own colours (`face` 1: the face decals and the head's skin, hair and dark greys, flagged by
            // `avatar::identity::head_colour_is_own`, under a weak 5% tint). A pixel is right when it is the one or the
            // other, and the bulk of a body, its cloth, is the first.
            let mirror = |i: usize, face: f32| {
                let want = look::look(to_f(&plain[i..]), id, 1.0, face).map(|c| (c.clamp(0.0, 1.0) * 255.0).round());
                (0..3).all(|k| (want[k] - f32::from(new[i + k])).abs() <= 2.0)
            };
            let agree = model_px.iter().filter(|&&i| mirror(i, 0.0)).count();
            let agree_or_own = model_px.iter().filter(|&&i| mirror(i, 0.0) || mirror(i, 1.0)).count();
            let largest = model_px.iter().map(|&i| (0..3).map(|k| plain[i + k].abs_diff(new[i + k])).max().unwrap_or(0)).max().unwrap_or(0);
            let grey_stays_grey = model_px.iter().filter(|&&i| plain[i] == plain[i + 1] && plain[i + 1] == plain[i + 2]).all(|&i| new[i].abs_diff(new[i + 1]) <= 1 && new[i + 1].abs_diff(new[i + 2]) <= 1);
            let share = |n: usize| 100.0 * n as f32 / model_px.len() as f32;
            eprintln!(
                "{:7} {name:5}: {:.1}% of {} pixels match the mirror's cloth, {:.1}% match its cloth or its own colours; largest change from the untinted picture {largest}; grey stays grey: {grey_stays_grey}",
                body.archetype,
                share(agree),
                model_px.len(),
                share(agree_or_own)
            );
            assert!(agree as f32 > 0.85 * model_px.len() as f32, "{} {name}: the shader is the mirror on the cloth", body.archetype);
            assert!(agree_or_own as f32 > 0.97 * model_px.len() as f32, "{} {name}: every pixel is the mirror's cloth or its own colours", body.archetype);
            assert!(grey_stays_grey, "{} {name}: no hue is added to grey", body.archetype);
            if name == "white" {
                assert!(largest <= 2, "{}: a white identity colour leaves the model as it is (largest change {largest})", body.archetype);
            }
        }
    }
}

/// On an sRGB target the look is worked out on the authored colours too (the lit colour is re-encoded and the result
/// decoded again), so both kinds of target show the same picture: the same frame on an `Rgba8Unorm` and an
/// `Rgba8UnormSrgb` target agrees within a few levels over the untextured parts of every body in every colour.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_identity_srgb_target_matches_the_plain_one() {
    let Some(real) = open() else { return };
    let mut avatars = real::avatars(&real);
    let fx = fixture(&real, &mut avatars);
    let nearest = fx.open_places.iter().copied().min_by(|a, b| a.depth.total_cmp(&b.depth)).expect("an open place");
    let frames: Vec<(usize, &str, AvatarFrame)> = PLACEHOLDER_BODIES
        .iter()
        .enumerate()
        .flat_map(|(b, _)| COLOURS.iter().map(move |(name, tint)| (b, *name, *tint)))
        .map(|(b, name, tint)| (b, name, bare(&frame_of(&mut avatars, &fx, &[body_player(1, b, nearest.pos, fx.toward + 0.45, tint)]))))
        .collect();
    let plain_target: Vec<Vec<u8>> = {
        let Some(mut gpu) = Offscreen::new() else { return };
        frames.iter().map(|(_, _, f)| render(&mut gpu, &fx, f, false)).collect()
    };
    let Some(mut gpu) = Offscreen::with_format(wgpu::TextureFormat::Rgba8UnormSrgb) else { return };
    let (mut agree, mut total, mut worst) = (0usize, 0usize, 0.0f32);
    for ((b, name, frame), want) in frames.iter().zip(&plain_target) {
        let got = render(&mut gpu, &fx, frame, false);
        let (mut within, mut n) = (0usize, 0usize);
        for i in (0..got.len()).step_by(4) {
            // (the background is the clear colour on both)
            if want[i..i + 3] != got[i..i + 3] || want[i] != CLEAR_RGB[0] {
                n += 1;
                let d = mean_abs(&got[i..i + 3], &want[i..i + 3]);
                worst = worst.max(d);
                within += usize::from((0..3).all(|k| got[i + k].abs_diff(want[i + k]) <= 4));
            }
        }
        eprintln!("{:7} {name:8}: {:.1}% of {n} pixels within 4 levels of the plain target", PLACEHOLDER_BODIES[*b].archetype, 100.0 * within as f32 / n.max(1) as f32);
        agree += within;
        total += n;
    }
    eprintln!("sRGB target against plain target: {:.2}% of {total} pixels within 4 levels (largest mean |difference| {worst:.1})", 100.0 * agree as f32 / total.max(1) as f32);
    assert!(agree as f32 > 0.97 * total as f32, "both kinds of target show the same picture");
}

/// The assumption the face rule rests on, measured: in the owned models the `overlay` flag (textured opaque parts: the
/// face decals) covers well under 1% of a model, field bodies and canon characters alike, and the models are
/// vertex-coloured (the colour is in the polygons, not in textures). Aggregates only.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA)"]
fn real_identity_field_models_are_vertex_coloured_with_face_decals_only() {
    use crate::avatar::library::AvatarAsset;
    use crate::avatar::source::{CharacterLoader, LgpLoader};
    let Some(real) = open() else { return };
    let mut loader = LgpLoader::new(&real.char_lgp);
    let models: Vec<_> = PLACEHOLDER_BODIES.iter().map(|b| b.model).chain(fmv_ff7::playable_field_models()).collect();
    for model in models {
        let asset = AvatarAsset::new(loader.load(&model).expect("loads")).expect("merges");
        let s = measure::shares(&asset.mesh);
        eprintln!(
            "{:18} {} triangles: overlay {:.1}% hair (own colours) {:.1}% textured {:.1}% skin-coloured {:.1}% pure black (under 8 of 255) {:.1}%",
            model.appearance_ref,
            s.triangles,
            s.overlay * 100.0,
            s.own * 100.0,
            s.textured * 100.0,
            s.skin * 100.0,
            s.black * 100.0
        );
        // Only the three placeholder bodies are drawn in a player's colour, so only they must have a head's hair (and the
        // head's skin, which is flagged with it) kept, and nothing but the head: a sixth of the model at most, never the body.
        if PLACEHOLDER_BODIES.iter().any(|b| b.model.appearance_ref == model.appearance_ref) {
            assert!(s.own < 0.25, "{}: the head's hair and skin are a fraction of a model, not {:.1}%", model.appearance_ref, s.own * 100.0);
            let bone_of = |vertex: usize| asset.mesh.bone_runs.iter().find(|r| vertex >= r.start && vertex < r.start + r.len).map(|r| r.bone);
            let flagged_off_head = (0..asset.mesh.own.len()).filter(|&i| asset.mesh.own[i] > 0.5).filter(|&i| asset.character.model.bones.get(bone_of(i).unwrap_or(usize::MAX)).map(|b| b.name.as_str()) != Some("head")).count();
            assert_eq!(flagged_off_head, 0, "{}: nothing off the head keeps its colours as hair", model.appearance_ref);
        }
        assert!(s.overlay < 0.02 && (s.overlay - s.textured).abs() < 1e-6, "{}: the overlay flag is the face decals", model.appearance_ref);
    }
}

/// The owner's scene as the field has it: the field's non-player character at its own place and a body at each spawn point
/// of the field, with the camera depth and the height on the screen of each. The owner's screenshot (`elevtr1`) has the
/// player a quarter taller than `av_j`; this says how much of that is where they stand.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_the_owners_scene_depths_and_heights() {
    use crate::avatar::frame::NpcRef;
    use fmv_adapter_api::AreaRuntime;
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "elevtr1".to_string());
    let mut avatars = real::avatars(&real);
    let fx = fixture_of(&real, &mut avatars, &key);
    let area = real.adapter.load_field(&key).expect("field");
    let none = bounds_scene(&fx);
    let depth = |p: Pos| fx.stage.camera.project([f64::from(p.x), f64::from(p.y), f64::from(p.z)]).map_or(f64::NAN, |s| s[2]);
    let spawns = area.spawn_points();
    eprintln!("{key}: {} spawn point(s), field scale {}, model scale {}", spawns.len(), area.field_scale(), fx.stage.model_scale);
    let npcs = area.npcs();
    let mut all = Vec::new();
    for n in &npcs {
        let idle = n.appearance.idle_animation.clone().unwrap_or_default();
        let player = AvatarPlayer {
            who: Who(1),
            pos: n.pos,
            facing: n.facing,
            anim: AnimState::Idle,
            scene_frame: None,
            appearance: None,
            npc: Some(NpcRef { model: &n.appearance.model, idle: Some(&idle) }),
            tint: [0; 3],
            own: false,
            party: false,
        };
        let frame = frame_of(&mut avatars, &fx, std::slice::from_ref(&player));
        let Some(_) = frame.models.first() else { continue };
        let b = model_bbox(&frame, 0, &none, W1, H1);
        eprintln!(
            "npc {:6} {:5} at ({:.0}, {:.0}, {:.0}) camera depth {:.0}: {:.1} px tall, feet at y {:.0}, unconditional {}",
            n.entity_name,
            n.appearance.model,
            n.pos.x,
            n.pos.y,
            n.pos.z,
            depth(n.pos),
            b.3 - b.1,
            b.3,
            n.unconditional
        );
        all.push((n.entity_name.clone(), b.3 - b.1, depth(n.pos), b.3));
    }
    for (i, s) in spawns.iter().enumerate() {
        for (b, body) in PLACEHOLDER_BODIES.iter().enumerate() {
            let player = body_player(2, b, *s, fx.toward, [240, 120, 40]);
            let frame = frame_of(&mut avatars, &fx, &[player]);
            let bb = model_bbox(&frame, 0, &none, W1, H1);
            eprintln!(
                "spawn {i} body {:7} at ({:.0}, {:.0}, {:.0}) camera depth {:.0}: {:.1} px tall, feet at y {:.0}",
                body.archetype, s.x, s.y, s.z, depth(*s), bb.3 - bb.1, bb.3
            );
        }
    }
    // One picture of the scene: the character and a body at the first spawn point, drawn as the window does.
    if let (Some(npc), Some(spawn)) = (npcs.first(), spawns.first()) {
        let idle = npc.appearance.idle_animation.clone().unwrap_or_default();
        let npc_player = AvatarPlayer {
            who: Who(1),
            pos: npc.pos,
            facing: npc.facing,
            anim: AnimState::Idle,
            scene_frame: None,
            appearance: None,
            npc: Some(NpcRef { model: &npc.appearance.model, idle: Some(&idle) }),
            tint: [0; 3],
            own: false,
            party: false,
        };
        let players = [npc_player, body_player(2, 2, *spawn, fx.toward + 0.45, [240, 120, 40])];
        let frame = frame_of(&mut avatars, &fx, &players);
        let img = render(&mut gpu, &fx, &frame, true);
        save_png(&format!("field_scene_{key}_npc_and_slight_at_spawn"), W1, H1, &img);
    }
    let _ = all;
}

/// The characters of a field standing at ONE place under the same camera, one picture each, side by side: the canon Cloud,
/// the field's own non-player character (`av_j`, Jessie, in `elevtr1`; `FMV_FF7_AREA` and `FMV_FF7_NPC` pick others) and the
/// placeholder bodies, so that scale, proportions and faces compare directly. Two sheets (the front and a three-quarter view),
/// each with two rows: the models' own colours over the look the window draws a player in (the orange of the owner's
/// character). Numbers: each model's height on the screen against Cloud's and the character's.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA) and a GPU or software Vulkan/GL driver"]
fn offscreen_field_lineup_bodies_beside_cloud_and_an_npc() {
    use crate::avatar::frame::NpcRef;
    use fmv_adapter_api::AreaRuntime;
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "elevtr1".to_string());
    let want = std::env::var("FMV_FF7_NPC").unwrap_or_else(|_| "av_j".to_string());
    let mut avatars = real::avatars(&real);
    let fx = fixture_of(&real, &mut avatars, &key);
    let nearest = fx.open_places.iter().copied().min_by(|a, b| a.depth.total_cmp(&b.depth)).expect("an open place");
    eprintln!("{key}: the characters stand at camera depth {:.0}", nearest.depth);
    let area = real.adapter.load_field(&key).expect("field");
    let spawns = area.npcs();
    let npc = spawns.iter().find(|n| n.entity_name == want).or_else(|| spawns.first()).expect("the field has a character");
    let (npc_model, npc_idle) = (npc.appearance.model.clone(), npc.appearance.idle_animation.clone().unwrap_or_default());
    eprintln!("the character: {} ({npc_model}, standing clip {npc_idle})", npc.entity_name);

    let none = bounds_scene(&fx);
    // (label, a player that stands for it)
    let lineup = |facing: f32, tint: [u8; 3]| -> Vec<(String, AvatarPlayer<'_>)> {
        let canon = AvatarPlayer {
            who: Who(1),
            pos: nearest.pos,
            facing,
            anim: AnimState::Idle,
            scene_frame: None,
            appearance: Some("ff7/cloud"),
            npc: None,
            tint,
            own: false,
            party: false,
        };
        let npc_player = AvatarPlayer { who: Who(2), appearance: None, npc: Some(NpcRef { model: &npc_model, idle: Some(&npc_idle) }), ..canon };
        let mut out = vec![("cloud".to_string(), canon), (format!("npc {}", npc.entity_name), npc_player)];
        out.extend(PLACEHOLDER_BODIES.iter().enumerate().map(|(b, body)| (body.archetype.to_string(), body_player(3 + b as u64, b, nearest.pos, facing, tint))));
        out
    };

    let mut heights: Vec<(String, f32)> = Vec::new();
    for ((view_name, facing), (colour, tint)) in [("front", fx.toward), ("three_quarter", fx.toward + 0.45)].into_iter().flat_map(|v| COLOURS.into_iter().take(2).map(move |c| (v, c))) {
        let players = lineup(facing, tint);
        let frames: Vec<AvatarFrame> = players.iter().map(|(_, p)| frame_of(&mut avatars, &fx, std::slice::from_ref(p))).collect();
        let boxes: Vec<(f32, f32, f32, f32)> = frames.iter().map(|f| model_bbox(f, 0, &none, W1, H1)).collect();
        let union = boxes.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |u, b| (u.0.min(b.0), u.1.min(b.1), u.2.max(b.2), u.3.max(b.3)));
        let (x0, y0) = (((union.0 - 14.0).max(0.0)) as u32, ((union.1 - 14.0).max(0.0)) as u32);
        let (cw, ch) = ((((union.2 + 14.0) as u32).min(W1) - x0), (((union.3 + 14.0) as u32).min(H1) - y0));
        if view_name == "front" {
            heights = players.iter().zip(&boxes).map(|((label, _), b)| (label.clone(), b.3 - b.1)).collect();
        }
        // BOTH EYES: seen from the front, the model's own colours, the two eye quads each show their dark texels (pupil,
        // lashes, brow), about as many on each side. One of them used to show none (the second eye of every model was not
        // drawn: its texture coordinates are shifted by a whole number, see `mesh::whole_cell_offsets`).
        if view_name == "front" && colour == "blue" {
            let cam = Camera::fit(&none.bounds, W1 as f32, H1 as f32, MARGIN_PX);
            for ((label, _), frame) in players.iter().zip(&frames) {
                let model = &frame.models[0];
                let img = render(&mut gpu, &fx, &dressed(frame, Look::Plain), false);
                // The textured parts in file order: the right eye, the left eye and the mouth (the covered face of `sturdy` has only a mouth).
                let (mut base, mut eyes) = (0usize, Vec::new());
                for part in &model.asset.character.model.parts {
                    let vertices = model.positions.get(base..base + part.positions.len()).unwrap_or(&[]);
                    if part.texture.is_some() && eyes.len() < 2 && !vertices.is_empty() {
                        let projected: Vec<(f32, f32)> = vertices.iter().map(|p| project_pixel(&cam, frame.zoom, W1, H1, *p)).collect();
                        let lo = projected.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
                        let hi = projected.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
                        eyes.push((lo, hi));
                    }
                    base += part.positions.len();
                }
                if eyes.len() < 2 {
                    eprintln!("{label}: no pair of eyes in the data (a covered face)");
                    continue;
                }
                let dark_in = |(lo, hi): ((f32, f32), (f32, f32))| -> usize {
                    (lo.1.floor().max(0.0) as u32..hi.1.ceil().min(H1 as f32) as u32)
                        .flat_map(|y| (lo.0.floor().max(0.0) as u32..hi.0.ceil().min(W1 as f32) as u32).map(move |x| (x, y)))
                        .filter(|&(x, y)| {
                            let at = ((y * W1 + x) * 4) as usize;
                            img.get(at..at + 3).is_some_and(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) < 230)
                        })
                        .count()
                };
                let (a, b) = (dark_in(eyes[0]), dark_in(eyes[1]));
                eprintln!("{label}: dark pixels in the two eye quads: {a} and {b}");
                assert!(a >= 15 && b >= 15, "{label}: both eyes are drawn ({a} and {b} dark pixels)");
                assert!(a.min(b) as f32 >= 0.4 * a.max(b) as f32, "{label}: the two eyes are about as visible as each other ({a} and {b})");
            }
        }
        let mut rows = Vec::new();
        for look in [Look::Plain, Look::New] {
            let panels: Vec<Img> = frames
                .iter()
                .map(|frame| {
                    // Nothing in front of them and no ring on the ground: the models alone, over the field's picture.
                    let mut dressed = dressed(frame, look);
                    dressed.ground.clear();
                    dressed.scenery = None;
                    crop(&render(&mut gpu, &fx, &dressed, true), W1, (x0, y0), (cw, ch), 3)
                })
                .collect();
            rows.push(hstack(&panels));
        }
        let sheet = vstack(&rows);
        eprintln!("{view_name} {colour}: {} characters, {}x{} px each at 3x; top row the models' own colours, bottom row the player look", players.len(), cw * 3, ch * 3);
        save_png(&format!("field_lineup_{key}_{view_name}_{colour}"), sheet.w, sheet.h, &sheet.px);
    }
    let cloud_h = heights.iter().find(|(l, _)| l == "cloud").map_or(1.0, |h| h.1);
    let npc_h = heights.iter().find(|(l, _)| l.starts_with("npc")).map_or(1.0, |h| h.1);
    for (label, h) in &heights {
        eprintln!("{label:12} {h:6.1} px tall at this spot: {:3.0}% of Cloud, {:3.0}% of the character", 100.0 * h / cloud_h, 100.0 * h / npc_h);
    }
}
