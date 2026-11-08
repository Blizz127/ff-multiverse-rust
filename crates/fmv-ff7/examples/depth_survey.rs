//! Calibration of the background tile depth rule against the owner's real data (needs
//! `FMV_FF7_DATA`; skips otherwise). Counts and ratios only are printed, never raw bytes.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example depth_survey -j16 -- --all
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example depth_survey -j16 -- md1stin md1_1 mds7st1
//! ```
//!
//! For every field it measures two independent things about the layer-2 tile `id` and the rule
//! "tile depth = `TILE_DEPTH_PER_ID` (4) x id, in the camera-space units of `Projection`":
//!
//! 1. **Engine constants** (needs the PC-only `IDBig` value stored in each tile record at +38, which
//!    `fmv_ff7::field` does not expose, so this file reads the raw records): with `z = 4 * id` the
//!    stored `IDBig / 1e7` of the tiles is `A - B / z` for constants `A`, `B` that reproduce a
//!    Direct3D perspective z of near plane `n` and far plane `f` (`A = f / (f - n)`, `B = f * n /
//!    (f - n)`). A least-squares fit per field gives `n = 50.0` and `f` = 32 000 in most fields with a
//!    residual of 1e-7..1e-5 (z-buffer units): the stored value *is* the engine's z-buffer value for a
//!    point at camera depth `4 * id`.
//! 2. **Walkmesh geometry**: the nearest walkmesh surface is rasterised to a per-pixel depth map
//!    (`Projection::project` depth). For each connected object of layer-2 pixels (same visible id)
//!    the floor pixels *under* the object are behind it (they must be farther than the object) and the
//!    floor pixels just *below its lowest pixel in each column* are in front of its base (they must
//!    be nearer than the object, and almost exactly as far as the object where it stands on the
//!    floor). The pass rates of both tests, and the ratio `floor depth at the base / object depth`,
//!    are reported for the scale `k` in `depth = k * id` for k in {1, 2, 3, 4, 5, 6, 8, 16}.
// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]
use fmv_ff7::background::Projection;
use fmv_ff7::field::{FieldFile, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

pub const SCALES: [f32; 8] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 16.0];

// ---- raw tile records (IDBig) ------------------------------------------------------------------

fn u16at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// `(layer, index within layer) -> IDBig` read straight from the background section.
fn raw_id_bigs(sec: &[u8]) -> Option<HashMap<(u8, u16), u32>> {
    let mut out = HashMap::new();
    let mut p = 0x28usize;
    let count = u16at(sec, p + 4)?;
    p += 10;
    for index in 0..count {
        out.insert((1, index), u32at(sec, p + 38)?);
        p += 52;
    }
    p += 2;
    for layer in 2..=4u8 {
        let flag = *sec.get(p)?;
        p += 1;
        if flag == 0 {
            continue;
        }
        let count = u16at(sec, p + 4)?;
        p += 6 + if layer == 2 { 16 } else { 10 } + 2;
        for index in 0..count {
            out.insert((layer, index), u32at(sec, p + 38)?);
            p += 52;
        }
        p += 2;
    }
    Some(out)
}

/// Least squares `y = a - b * x`; returns `(a, b, rms)`.
fn fit_inverse(xs: &[f64], ys: &[f64]) -> Option<(f64, f64, f64)> {
    let n = xs.len() as f64;
    let (sx, sy): (f64, f64) = (xs.iter().sum(), ys.iter().sum());
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-30 {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    let a = (sy - slope * sx) / n;
    let rms = (xs.iter().zip(ys).map(|(x, y)| (y - a - slope * x).powi(2)).sum::<f64>() / n).sqrt();
    Some((a, -slope, rms))
}

// ---- floor depth map ---------------------------------------------------------------------------

struct Grid {
    w: i32,
    h: i32,
    ox: i32,
    oy: i32,
}

impl Grid {
    fn at(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| (y * self.w + x) as usize)
    }
}

/// Nearest walkmesh depth per pixel (`f32::INFINITY` where no triangle is seen).
fn floor_depth(walk: &Walkmesh, proj: &Projection, g: &Grid) -> Vec<f32> {
    let mut depth = vec![f32::INFINITY; (g.w * g.h) as usize];
    for tri in &walk.triangles {
        let mut p = Vec::with_capacity(3);
        for v in tri {
            let Some(s) = proj.project([f32::from(v.x), f32::from(v.y), f32::from(v.z)]) else { break };
            p.push((s.x + g.ox as f32, s.y + g.oy as f32, s.depth));
        }
        let [a, b, c] = p[..] else { continue };
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-6 {
            continue;
        }
        let x0 = a.0.min(b.0).min(c.0).floor().max(0.0) as i32;
        let x1 = (a.0.max(b.0).max(c.0).ceil() as i32).min(g.w - 1);
        let y0 = a.1.min(b.1).min(c.1).floor().max(0.0) as i32;
        let y1 = (a.1.max(b.1).max(c.1).ceil() as i32).min(g.h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
                let w1 = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let inv = w0 / a.2 + w1 / b.2 + w2 / c.2; // 1/z is linear in screen space
                if inv <= 0.0 {
                    continue;
                }
                let z = 1.0 / inv;
                if let Some(slot) = g.at(x, y).and_then(|i| depth.get_mut(i)) {
                    if z < *slot {
                        *slot = z;
                    }
                }
            }
        }
    }
    depth
}

// ---- per-field analysis ------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct ScaleStats {
    /// floor pixels under objects: (passes, total)
    pub behind: (u64, u64),
    /// floor pixels below object bases: (passes, total)
    pub front: (u64, u64),
    /// objects whose floor pixels under them all pass: (passes, total)
    pub objects_behind: (u64, u64),
    /// objects whose base samples mostly pass: (passes, total)
    pub objects_front: (u64, u64),
    /// tiles (>= 4 floor-covered opaque pixels) with >= 90% passing pixels: (passes, total)
    pub tiles: (u64, u64),
}

#[derive(Default)]
pub struct Totals {
    pub fields: usize,
    pub fields_with_walk_and_l2: usize,
    pub layer2_tiles: u64,
    pub layer2_static_tiles: u64,
    pub id_classes: [u64; 3], // z < 50 (always in front), mid, id >= 4000 (always behind)
    pub scale: Vec<ScaleStats>,
    pub ratio_samples: Vec<f32>,       // per object median(D_base / (4 * id)) for mid objects
    pub ratio_pairs: Vec<(f32, f32)>,  // per object (4 * id, median floor depth below its base)
    pub fits: Vec<(String, f64, f64, f64, usize)>, // name, near, far, rms, ids
    pub mid_in_range: (u64, u64),      // mid tiles with depth within the walkmesh depth range (+-25%)
    pub tile_nearest_ratio: Vec<f32>,  // per constrained tile: nearest floor depth under it / (4 * id)
    pub max_tile_bytes: (usize, usize, String), // most RGBA bytes of all tiles of one field, its tile count, name
    pub max_walk_depth: (f32, String), // deepest walkmesh vertex over all fields
    pub fields_deeper_than_layer1: Vec<(String, f32)>, // walkmesh vertex deeper than 4 * 4095
}

pub fn analyze(name: &str, field: &FieldFile, verbose: bool, totals: &mut Totals) -> Option<()> {
    let bg = field.background().ok()?;
    let palette = field.palette().ok()?;
    let walk = field.walkmesh().ok()?;
    let cam = field.cameras().ok()?.first().copied()?;
    let proj = Projection::from_camera(&cam);
    let bigs = raw_id_bigs(field.section(8).ok()?).unwrap_or_default();

    // canvas of every kept tile
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for t in &bg.tiles {
        let s = i32::from(t.tile_size);
        x0 = x0.min(i32::from(t.dst_x));
        y0 = y0.min(i32::from(t.dst_y));
        x1 = x1.max(i32::from(t.dst_x) + s);
        y1 = y1.max(i32::from(t.dst_y) + s);
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let g = Grid { w: x1 - x0, h: y1 - y0, ox: -x0, oy: -y0 };
    totals.fields += 1;
    let rgba_bytes: usize = bg.tiles.iter().map(|t| usize::from(t.tile_size).pow(2) * 4).sum();
    if rgba_bytes > totals.max_tile_bytes.0 {
        totals.max_tile_bytes = (rgba_bytes, bg.tiles.len(), name.to_string());
    }

    // --- engine constants: IDBig = 1e7 * (A - B / (4 id))
    let mut by_id: BTreeMap<u16, Vec<f64>> = BTreeMap::new();
    for t in bg.tiles.iter().filter(|t| t.layer == 2 && t.param == 0) {
        if let Some(&big) = bigs.get(&(2, t.index)) {
            by_id.entry(t.id).or_default().push(f64::from(big));
        }
    }
    // ids whose depth is below the field's near plane are stored as 10000 * id (999 for id 0): the
    // engine's own "in front of everything" marker (rounding gives 49999 for id 5, 99999 for id 10)
    let front_ids: HashSet<u16> = by_id
        .iter()
        .filter(|(id, v)| {
            v.iter().any(|&b| if **id == 0 { b == 999.0 } else { (b - 10_000.0 * f64::from(**id)).abs() <= 2.0 })
        })
        .map(|(id, _)| *id)
        .collect();
    let is_front = |id: u16| if bigs.is_empty() { 4 * u32::from(id) < 50 } else { front_ids.contains(&id) };
    let pts: Vec<(f64, f64)> = by_id
        .iter_mut()
        .filter(|(id, _)| **id >= 14 && **id < 4090 && !front_ids.contains(*id))
        .filter_map(|(id, v)| {
            v.sort_by(|a, b| a.total_cmp(b));
            let med = *v.get(v.len() / 2)?;
            Some((f64::from(*id), med / 1e7))
        })
        .collect();
    if pts.len() >= 4 {
        let xs: Vec<f64> = pts.iter().map(|(id, _)| 1.0 / (4.0 * id)).collect();
        let ys: Vec<f64> = pts.iter().map(|(_, y)| *y).collect();
        if let Some((a, b, rms)) = fit_inverse(&xs, &ys) {
            totals.fits.push((name.to_string(), b / a, b / (a - 1.0), rms, pts.len()));
        }
    }

    // --- walkmesh depth range (vertices in front of the camera)
    let mut dmin = f32::INFINITY;
    let mut dmax = 0.0f32;
    for v in walk.triangles.iter().flatten() {
        if let Some(s) = proj.project([f32::from(v.x), f32::from(v.y), f32::from(v.z)]) {
            dmin = dmin.min(s.depth);
            dmax = dmax.max(s.depth);
        }
    }
    if dmax > totals.max_walk_depth.0 {
        totals.max_walk_depth = (dmax, name.to_string());
    }
    if dmax >= 16_380.0 {
        totals.fields_deeper_than_layer1.push((name.to_string(), dmax));
    }
    let floor = floor_depth(&walk, &proj, &g);

    // --- visible layer-2 object pixels (static group, not blended): the smallest id wins
    let npx = (g.w * g.h) as usize;
    let mut vis_id = vec![-1i32; npx];
    let mut covered = vec![false; npx]; // any opaque layer-2/4 pixel (blended or grouped tiles too)
    let mut tile_pixels: Vec<(u16, Vec<usize>)> = Vec::new(); // per tile: id, opaque floor-covered pixel indices
    for t in &bg.tiles {
        if t.layer < 2 {
            continue;
        }
        if t.layer == 2 {
            totals.layer2_tiles += 1;
        }
        let Some(rgba) = bg.tile_rgba(t, &palette) else { continue };
        let size = i32::from(t.tile_size);
        let counts_as_object = t.layer == 2 && t.param == 0 && t.blending == 0;
        if counts_as_object {
            totals.layer2_static_tiles += 1;
            let class = if is_front(t.id) { 0 } else if t.id >= 4000 { 2 } else { 1 };
            if let Some(c) = totals.id_classes.get_mut(class) {
                *c += 1;
            }
        }
        let mut floor_px = Vec::new();
        for (i, texel) in rgba.iter().enumerate() {
            if texel[3] == 0 {
                continue;
            }
            let (x, y) = (i as i32 % size + i32::from(t.dst_x) + g.ox, i as i32 / size + i32::from(t.dst_y) + g.oy);
            let Some(at) = g.at(x, y) else { continue };
            if let Some(c) = covered.get_mut(at) {
                *c = true;
            }
            if counts_as_object {
                if let Some(v) = vis_id.get_mut(at) {
                    if *v < 0 || i32::from(t.id) < *v {
                        *v = i32::from(t.id);
                    }
                }
                if floor.get(at).is_some_and(|d| d.is_finite()) {
                    floor_px.push(at);
                }
            }
        }
        if counts_as_object {
            tile_pixels.push((t.id, floor_px));
        }
    }

    // --- per tile one-sided test
    let mut local: Vec<ScaleStats> = vec![ScaleStats::default(); SCALES.len()];
    for (id, px) in &tile_pixels {
        if is_front(*id) || *id >= 4000 || px.len() < 4 {
            continue;
        }
        for (k, st) in SCALES.iter().zip(local.iter_mut()) {
            let z = k * f32::from(*id);
            let slack = (0.03 * z).max(8.0);
            let pass = px.iter().filter(|&&at| floor.get(at).is_some_and(|d| *d >= z - slack)).count();
            st.tiles.1 += 1;
            if pass * 10 >= px.len() * 9 {
                st.tiles.0 += 1;
            }
        }
        // how far off are the tiles that fail at k = 4: nearest floor under the tile against 4 * id
        totals.tile_nearest_ratio.push(
            px.iter().filter_map(|&at| floor.get(at).copied()).fold(f32::INFINITY, f32::min) / (4.0 * f32::from(*id)),
        );
    }
    // mid-range tile depths against the walkmesh depth range
    for (id, _) in &tile_pixels {
        if is_front(*id) || *id >= 4000 {
            continue;
        }
        totals.mid_in_range.1 += 1;
        let z = 4.0 * f32::from(*id);
        if z >= dmin * 0.75 && z <= dmax * 1.25 {
            totals.mid_in_range.0 += 1;
        }
    }

    // --- connected objects (same visible id, 8-connected)
    let mut seen = vec![false; npx];
    let mut objects = 0usize;
    let mut field_ratios: Vec<f32> = Vec::new();
    for start in 0..npx {
        let Some(&id) = vis_id.get(start) else { continue };
        if id < 0 || seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut stack = vec![start];
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        let mut under: Vec<f32> = Vec::new();
        let mut low: HashMap<i32, i32> = HashMap::new();
        while let Some(at) = stack.pop() {
            let (x, y) = (at as i32 % g.w, at as i32 / g.w);
            if let Some(d) = floor.get(at).copied().filter(|d| d.is_finite()) {
                under.push(d);
            }
            let e = low.entry(x).or_insert(y);
            *e = (*e).max(y);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let Some(n) = g.at(x + dx, y + dy) else { continue };
                    if vis_id.get(n) == Some(&id) && !seen.get(n).copied().unwrap_or(true) {
                        if let Some(s) = seen.get_mut(n) {
                            *s = true;
                        }
                        stack.push(n);
                    }
                }
            }
        }
        // floor just below the base of each column
        let mut base: Vec<f32> = Vec::new();
        for (&x, &y) in &low {
            for dy in 1..=3 {
                let Some(q) = g.at(x, y + dy) else { break };
                if covered.get(q).copied().unwrap_or(true) {
                    break;
                }
                if let Some(d) = floor.get(q).copied().filter(|d| d.is_finite()) {
                    base.push(d);
                    break;
                }
            }
        }
        let mid = !is_front(id as u16) && id < 4000;
        objects += 1;
        if verbose {
            let mut b = base.clone();
            b.sort_by(|a, c| a.total_cmp(c));
            let med = b.get(b.len() / 2).copied();
            let mut u = under.clone();
            u.sort_by(|a, c| a.total_cmp(c));
            println!(
                "  object id {id:4} (4*id = {:5}) columns {:3} base floor samples {:3} base depth min/med/max {:?}/{:?}/{:?} ratio {} | floor under it: {} px, depth min/med {:?}/{:?}",
                4 * id,
                low.len(),
                b.len(),
                b.first().map(|d| d.round()),
                med.map(f32::round),
                b.last().map(|d| d.round()),
                med.map_or("-".to_string(), |m| format!("{:.2}", m / (4.0 * id as f32))),
                u.len(),
                u.first().map(|d| d.round()),
                u.get(u.len() / 2).map(|d| d.round())
            );
        }
        if !mid || (under.is_empty() && base.is_empty()) {
            continue;
        }
        for (k, st) in SCALES.iter().zip(local.iter_mut()) {
            let z = k * id as f32;
            let slack = (0.03 * z).max(8.0);
            let bp = under.iter().filter(|&&d| d >= z - slack).count();
            let fp = base.iter().filter(|&&d| d <= z + slack).count();
            st.behind.0 += bp as u64;
            st.behind.1 += under.len() as u64;
            st.front.0 += fp as u64;
            st.front.1 += base.len() as u64;
            if !under.is_empty() {
                st.objects_behind.1 += 1;
                if bp * 10 >= under.len() * 9 {
                    st.objects_behind.0 += 1;
                }
            }
            if !base.is_empty() {
                st.objects_front.1 += 1;
                if fp * 4 >= base.len() * 3 {
                    st.objects_front.0 += 1;
                }
            }
        }
        if !base.is_empty() {
            let mut r: Vec<f32> = base.iter().map(|d| d / (4.0 * id as f32)).collect();
            r.sort_by(|a, b| a.total_cmp(b));
            if let Some(m) = r.get(r.len() / 2) {
                totals.ratio_samples.push(*m);
                field_ratios.push(*m);
                totals.ratio_pairs.push((4.0 * id as f32, *m * 4.0 * id as f32));
            }
        }
    }
    if verbose {
        field_ratios.sort_by(|a, b| a.total_cmp(b));
        println!(
            "{name}: contact ratios (floor depth below the base / (4 * id)) of {} ordinary objects: min {:?} median {:?} max {:?}",
            field_ratios.len(),
            field_ratios.first().map(|r| (r * 100.0).round() / 100.0),
            field_ratios.get(field_ratios.len() / 2).map(|r| (r * 100.0).round() / 100.0),
            field_ratios.last().map(|r| (r * 100.0).round() / 100.0)
        );
        println!(
            "{name}: camera zoom {} walkmesh depth {:.0}..{:.0} (vertices), {} triangles, canvas {}x{}, {} layer-2 objects",
            cam.zoom,
            dmin,
            dmax,
            walk.len(),
            g.w,
            g.h,
            objects
        );
        for (k, st) in SCALES.iter().zip(local.iter()) {
            println!(
                "  depth = {k:>4} * id: floor under objects farther than object {}/{}; floor below bases nearer {}/{}; objects ok behind {}/{} front {}/{}; tiles {}/{}",
                st.behind.0, st.behind.1, st.front.0, st.front.1, st.objects_behind.0, st.objects_behind.1,
                st.objects_front.0, st.objects_front.1, st.tiles.0, st.tiles.1
            );
        }
    }
    if dmax > 0.0 && !tile_pixels.is_empty() {
        totals.fields_with_walk_and_l2 += 1;
    }
    for (acc, st) in totals.scale.iter_mut().zip(local.iter()) {
        acc.behind.0 += st.behind.0;
        acc.behind.1 += st.behind.1;
        acc.front.0 += st.front.0;
        acc.front.1 += st.front.1;
        acc.objects_behind.0 += st.objects_behind.0;
        acc.objects_behind.1 += st.objects_behind.1;
        acc.objects_front.0 += st.objects_front.0;
        acc.objects_front.1 += st.objects_front.1;
        acc.tiles.0 += st.tiles.0;
        acc.tiles.1 += st.tiles.1;
    }
    Some(())
}

/// Average ranks (ties share the mean rank).
fn ranks(v: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..v.len()).collect();
    order.sort_by(|&a, &b| v.get(a).copied().unwrap_or(0.0).total_cmp(&v.get(b).copied().unwrap_or(0.0)));
    let mut out = vec![0.0; v.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len()
            && order.get(j + 1).and_then(|&k| v.get(k)) == order.get(i).and_then(|&k| v.get(k))
        {
            j += 1;
        }
        let rank = (i + j) as f64 / 2.0;
        for &k in order.get(i..=j).unwrap_or(&[]) {
            if let Some(slot) = out.get_mut(k) {
                *slot = rank;
            }
        }
        i = j + 1;
    }
    out
}

fn pct(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        100.0 * a as f64 / b as f64
    }
}

/// Analyse the named fields of `flevel.lgp` (entries that fail to read or parse are skipped).
pub fn survey_fields(lgp: &mut LgpArchive<std::fs::File>, names: &[String], verbose: bool) -> Totals {
    let mut totals = Totals { scale: vec![ScaleStats::default(); SCALES.len()], ..Totals::default() };
    for name in names {
        let Ok(stored) = lgp.read(name) else { continue };
        let Ok(bytes) = lzss::decompress(&stored) else { continue };
        let Ok(field) = FieldFile::parse(bytes) else { continue };
        if analyze(name, &field, verbose, &mut totals).is_none() && verbose {
            println!("{name}: nothing to analyse");
        }
    }
    totals
}

/// Names of the field entries of `flevel.lgp` (no `.` in the name, not the name list).
pub fn field_names(lgp: &LgpArchive<std::fs::File>) -> Vec<String> {
    lgp.list().iter().filter(|e| !e.name.contains('.') && !e.name.eq_ignore_ascii_case("maplist")).map(|e| e.name.clone()).collect()
}

impl Totals {
    /// The `f` quantile (0..1) of the per-object median `floor depth below the base / (4 * id)`.
    pub fn contact_ratio_quantile(&self, f: f32) -> Option<f32> {
        let mut r = self.ratio_samples.clone();
        r.sort_by(|a, b| a.total_cmp(b));
        r.get(((r.len().max(1) - 1) as f32 * f) as usize).copied()
    }

    /// Objects whose contact ratio is within `[lo, hi]`, and all objects with one.
    pub fn contact_ratio_band(&self, lo: f32, hi: f32) -> (usize, usize) {
        (self.ratio_samples.iter().filter(|r| **r >= lo && **r <= hi).count(), self.ratio_samples.len())
    }

    /// Fields whose fitted near plane is within `tol` of `near`.
    pub fn fits_with_near(&self, near: f64, tol: f64) -> usize {
        self.fits.iter().filter(|f| (f.1 - near).abs() < tol).count()
    }

    /// Fields whose fit has a residual below `limit` (z-buffer units).
    pub fn fits_with_rms_below(&self, limit: f64) -> usize {
        self.fits.iter().filter(|f| f.3 < limit).count()
    }

    /// Index into [`SCALES`] and the stats of the scale `k`.
    pub fn scale_stats(&self, k: f32) -> Option<&ScaleStats> {
        SCALES.iter().position(|s| (*s - k).abs() < 1e-3).and_then(|i| self.scale.get(i))
    }
}

fn main() {
    let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: depth_survey --all | <field>...");
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
    let all = args.first().map(String::as_str) == Some("--all");
    let names: Vec<String> = if all { field_names(&lgp) } else { args.clone() };
    let totals = survey_fields(&mut lgp, &names, !all);
    print_totals(&totals);
}

/// Print the aggregate numbers of a survey.
pub fn print_totals(totals: &Totals) {
    println!("=== totals over {} fields ({} with a walkmesh and layer-2 objects)", totals.fields, totals.fields_with_walk_and_l2);
    println!(
        "layer-2 tiles {} (static, unblended {}): id classes: in front of the near plane (4*id < 50) {}, mid {}, id >= 4000 (behind everything) {}",
        totals.layer2_tiles, totals.layer2_static_tiles, totals.id_classes[0], totals.id_classes[1], totals.id_classes[2]
    );
    println!(
        "mid-range tile depths (4 * id) inside the field's walkmesh depth range +-25%: {}/{} ({:.1}%)",
        totals.mid_in_range.0,
        totals.mid_in_range.1,
        pct(totals.mid_in_range.0, totals.mid_in_range.1)
    );
    for (k, st) in SCALES.iter().zip(totals.scale.iter()) {
        println!(
            "depth = {k:>4} * id | floor under objects is farther: {:>7}/{:<7} {:5.1}% | floor below bases is nearer: {:>6}/{:<6} {:5.1}% | per object behind {:5.1}% front {:5.1}% | tiles consistent {:>6}/{:<6} {:5.1}%",
            st.behind.0,
            st.behind.1,
            pct(st.behind.0, st.behind.1),
            st.front.0,
            st.front.1,
            pct(st.front.0, st.front.1),
            pct(st.objects_behind.0, st.objects_behind.1),
            pct(st.objects_front.0, st.objects_front.1),
            st.tiles.0,
            st.tiles.1,
            pct(st.tiles.0, st.tiles.1)
        );
    }
    {
        // tiles whose nearest floor pixel is nearer than 4 * id by more than a given fraction
        let all = totals.tile_nearest_ratio.len() as u64;
        let mut line = format!("constrained tiles ({all}): nearest floor under a tile is not nearer than 4 * id by more than");
        for frac in [0.0f32, 0.03, 0.10, 0.25, 0.5] {
            let ok = totals.tile_nearest_ratio.iter().filter(|r| **r >= 1.0 - frac).count() as u64;
            line.push_str(&format!(" {:.0}%: {:.1}% |", frac * 100.0, pct(ok, all)));
        }
        println!("{line}");
    }
    let mut ratios = totals.ratio_samples.clone();
    ratios.sort_by(|a, b| a.total_cmp(b));
    if !ratios.is_empty() {
        let q = |f: f32| ratios.get(((ratios.len() - 1) as f32 * f) as usize).copied().unwrap_or(0.0);
        println!(
            "per-object median (floor depth just below the base) / (4 * id), {} objects: p10 {:.2} p25 {:.2} median {:.2} p75 {:.2} p90 {:.2}",
            ratios.len(),
            q(0.1),
            q(0.25),
            q(0.5),
            q(0.75),
            q(0.9)
        );
    }
    // rank correlation between 4 * id and the floor depth at the object's base: positive if larger
    // ids are farther (sign), robust to the hanging objects whose base is not on the floor
    if totals.ratio_pairs.len() > 2 {
        let xs: Vec<f64> = ranks(&totals.ratio_pairs.iter().map(|p| f64::from(p.0)).collect::<Vec<_>>());
        let ys: Vec<f64> = ranks(&totals.ratio_pairs.iter().map(|p| f64::from(p.1)).collect::<Vec<_>>());
        let n = xs.len() as f64;
        let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
        let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        let syy: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
        println!(
            "Spearman rank correlation of base depth with 4 * id over {} objects: {:.3} (1.0 = larger id is always farther)",
            xs.len(),
            sxy / (sxx * syy).sqrt()
        );
        let in_band = totals.ratio_samples.iter().filter(|r| **r >= 0.8 && **r <= 1.25).count();
        println!(
            "objects whose base depth is within 0.8..1.25 of 4 * id: {}/{} ({:.1}%)",
            in_band,
            totals.ratio_samples.len(),
            pct(in_band as u64, totals.ratio_samples.len() as u64)
        );
    }
    println!(
        "deepest walkmesh vertex over all fields: {:.0} ({}); fields with a vertex at or beyond layer 1's depth 16380: {:?}",
        totals.max_walk_depth.0, totals.max_walk_depth.1, totals.fields_deeper_than_layer1
    );
    println!(
        "most tile pixels in one field: {} RGBA bytes in {} tiles ({})",
        totals.max_tile_bytes.0, totals.max_tile_bytes.1, totals.max_tile_bytes.2
    );
    // engine constants
    let near_ok = totals.fits.iter().filter(|f| (f.1 - 50.0).abs() < 0.5).count();
    let rms_ok = totals.fits.iter().filter(|f| f.3 < 1e-4).count();
    println!(
        "IDBig fits (z = 4 * id): {} fields with >= 4 usable ids; near plane within 0.5 of 50: {}; residual rms < 1e-4: {}",
        totals.fits.len(),
        near_ok,
        rms_ok
    );
    let mut near_hist: BTreeMap<i64, usize> = BTreeMap::new();
    for f in &totals.fits {
        *near_hist.entry(f.1.round() as i64).or_insert(0) += 1;
    }
    println!("  fitted near plane (rounded) -> fields: {near_hist:?}");
    let mut far_hist: BTreeMap<i64, usize> = BTreeMap::new();
    for f in totals.fits.iter().filter(|f| f.3 < 1e-4) {
        *far_hist.entry((f.2 / 1000.0).round() as i64).or_insert(0) += 1;
    }
    println!("  fitted far plane (thousands) -> fields: {far_hist:?}");
    for f in totals.fits.iter().filter(|f| f.3 >= 1e-4) {
        println!("  poor fit: {} near {:.1} far {:.0} rms {:.2e} ({} ids)", f.0, f.1, f.2, f.3, f.4);
    }
}
