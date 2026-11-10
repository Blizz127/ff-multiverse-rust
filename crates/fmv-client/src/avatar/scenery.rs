//! The picture of an FF7 field as depth-tested tiles: what the renderer draws instead of one flat
//! picture when the characters are models.
//!
//! The PC engine does not draw the field as an image with characters pasted over it: every
//! background tile is a quad with its own depth, in the same z-buffered pipeline as the models, so
//! a character is hidden by exactly the tiles that are nearer to the camera than it is (see
//! [`fmv_ff7::background::depth`], where the rule, `4 * id` in camera-space units, is derived and
//! verified on the owned data). A [`Scenery`] is that picture prepared for the GPU once per field:
//!
//! * the pixels of every tile in **one atlas** (premultiplied, sampled with the linear filter and a
//!   per-tile clamp so a tile never reads its neighbour; the texels a nearer tile covers carry the
//!   colour of what covers them, see below), plus one opaque black texel row that the *base* quad
//!   samples;
//! * the **opaque tiles** as one static vertex list in the tiles' paint order (back to front), after
//!   the base quad that covers the whole picture in black (pixels no tile touches show black, as in
//!   the game and in [`fmv_ff7::background::decode_background`]);
//! * the **blended tiles** (glow, shadow, glass) as a second list in paint order, grouped into runs
//!   of one [`BlendMode`], drawn after the models with the depth test but no depth write.
//!
//! Every vertex carries its tile's camera-space depth. All the depth tests of a frame, the models'
//! ([`super::gpu`]), the tiles' and the ground marks', use [`buffer_depth`], one fixed mapping, so a
//! tile at camera depth `D` compares exactly like a model fragment at camera depth `D`.
//!
//! # Hidden texels
//!
//! A tile is drawn with the linear filter (the picture is shown at any window size, and the flat
//! picture it replaces was filtered too) and cut out at half alpha. Where an object stands in front
//! of the back layer, the back layer's tile still holds what is under the object: in `md1stin` 6 277
//! pure-green key-colour pixels that the artists left there. The filter reads those texels next to
//! the object's silhouette, so a bright green line would run round it. [`Scenery::choose`] therefore
//! gives every texel that a nearer opaque tile covers the colour of the tile that covers it
//! (`bake_hidden_texels`): the filter then blends the visible background with the object's own edge,
//! which is what the flat picture's filter does. At a scale that lines the pixels up (one window
//! pixel per picture pixel) the tiles reproduce the flat picture exactly, with or without this.
//!
//! # The depth mapping
//!
//! [`buffer_depth`] is the engine's own perspective z-buffer value, `A - B / depth` with
//! `A = far / (far - near)` and `B = far * near / (far - near)` for the engine's near plane 50 and
//! far plane 32 000 ([`DEPTH_NEAR`], [`DEPTH_FAR`]; fitted to the stored `IDBig` of the tiles of 394
//! of 451 owned fields, see `fmv_ff7`), clamped to `0..=1`. It is used for three reasons:
//!
//! * It is **fixed**: a mapping fitted to the models of one frame would squeeze every tile outside
//!   the models' depth range into the same value at either end, and then blended tiles, which are
//!   drawn after the opaque ones, could pass over nearer opaque tiles that they must not cover
//!   (blended and opaque tiles behind the models would compare equal). With this mapping every id
//!   of layer 2 and the layer 3 and 1 depths are distinct `f32` values (about 12 steps apart even
//!   at the far end, checked by a test), so tiles order the way the paint order does.
//! * It is **exact per fragment**: `1 / depth` is affine in screen space, so the hardware's linear
//!   interpolation of this value across a model's triangle is the true value at every pixel (a mapping
//!   linear in depth would be off by several units across a character-sized triangle, which matters
//!   right at an object's depth).
//! * Depths below the near plane (layer 4 at 0, the layer-2 ids 12 and below) clamp to 0: "in front
//!   of everything", as [`fmv_ff7::background::depth`] requires, not culled.

use bytemuck::{Pod, Zeroable};
use fmv_ff7::background::depth::{pack_atlas, DepthTile, TileAtlas, ENGINE_FAR_PLANE, ENGINE_NEAR_PLANE};
use fmv_ff7::background::{decode_background_tiles_with_depth_options, BackgroundOptions, BlendMode, ScreenRect};
use fmv_ff7::field::FieldFile;
use std::sync::atomic::{AtomicU64, Ordering};

/// The engine's near plane (camera-space units): the depth the mapping sends to 0.
pub const DEPTH_NEAR: f32 = ENGINE_NEAR_PLANE;
/// The engine's far plane: the depth the mapping sends to 1.
pub const DEPTH_FAR: f32 = ENGINE_FAR_PLANE;

/// Widest atlas the CPU side packs. Every device this client runs on holds a texture this wide
/// (the downlevel minimum is 2048); the height follows from the tile count and is checked against
/// the device when the atlas is uploaded.
pub const ATLAS_MAX_WIDTH: u32 = 2048;

static NEXT_SCENERY_ID: AtomicU64 = AtomicU64::new(1);

/// `[A, B]` of the depth mapping: the z-buffer value of camera depth `d` is `A - B / d`.
pub fn depth_terms() -> [f32; 2] {
    let (near, far) = (f64::from(DEPTH_NEAR), f64::from(DEPTH_FAR));
    [(far / (far - near)) as f32, (far * near / (far - near)) as f32]
}

/// The depth-buffer value (0 nearest, 1 farthest) of a camera-space depth: what the shaders'
/// `buffer_depth` computes on the GPU, mirrored here so the mapping is tested without a GPU. A depth
/// below the near plane is 0, one beyond the far plane 1.
#[cfg(test)]
pub fn buffer_depth(depth: f32) -> f32 {
    let [a, b] = depth_terms();
    (a - b / depth.max(1.0)).clamp(0.0, 1.0)
}

/// One corner of a tile quad. Positions are field-screen units (x right, y DOWN, the space of
/// [`fmv_ff7::background::DepthTile::rect`]); the vertex shader maps them to the window with the
/// same fitted camera the picture and the walkmesh use.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct TileVertex {
    pub screen: [f32; 2],
    /// Texture coordinates in the atlas (normalised).
    pub uv: [f32; 2],
    /// `[u_min, v_min, u_max, v_max]`: the centres of the tile's outermost texels. The fragment
    /// shader clamps the coordinate into it, so the linear filter never reads another tile.
    pub clamp: [f32; 4],
    /// Camera-space depth of the whole quad, see [`buffer_depth`].
    pub depth: f32,
}

/// Consecutive blended tiles of one mode: `count` vertices from `first` in [`Scenery::blended`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendRun {
    pub mode: BlendMode,
    pub first: u32,
    pub count: u32,
}

/// What a scenery holds, for logs and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneryCounts {
    /// Tiles drawn (all layers).
    pub tiles: usize,
    /// Of them: opaque, and blended.
    pub opaque: usize,
    pub blended: usize,
    /// Tiles of layers 2 and 4, the ones that can be in front of a character.
    pub in_front: usize,
    /// Atlas size in texels (the height includes the black row).
    pub atlas: (u32, u32),
}

/// A field's background prepared for depth-tested drawing, built once (see the module docs).
pub struct Scenery {
    /// Distinguishes sceneries, so the GPU copy is replaced only when the field changed.
    pub id: u64,
    pub atlas_width: u32,
    pub atlas_height: u32,
    /// Premultiplied RGBA8, `atlas_width * atlas_height * 4` bytes; the last row is opaque black.
    pub atlas: Vec<u8>,
    /// The black base quad (6 vertices), then every opaque tile in paint order (6 vertices each).
    pub opaque: Vec<TileVertex>,
    /// Every blended tile in paint order (6 vertices each).
    pub blended: Vec<TileVertex>,
    pub runs: Vec<BlendRun>,
    pub counts: SceneryCounts,
}

/// `Debug` prints sizes only, never pixels.
impl std::fmt::Debug for Scenery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scenery").field("id", &self.id).field("counts", &self.counts).field("runs", &self.runs.len()).finish_non_exhaustive()
    }
}

/// The six vertices of a quad over `rect` showing the atlas square `at` (a `size` x `size` square at
/// `(x, y)`) of a `tex` sized texture, at camera depth `depth`.
fn quad(rect: ScreenRect, at: (u32, u32, u32), tex: (u32, u32), depth: f32) -> [TileVertex; 6] {
    let (x, y, size) = at;
    let (tw, th) = (tex.0 as f32, tex.1 as f32);
    let (x0, y0, x1, y1) = (x as f32, y as f32, (x + size) as f32, (y + size) as f32);
    let clamp = [(x0 + 0.5) / tw, (y0 + 0.5) / th, (x1 - 0.5) / tw, (y1 - 0.5) / th];
    let (u0, v0, u1, v1) = (x0 / tw, y0 / th, x1 / tw, y1 / th);
    let (left, top, right, bottom) = (rect.min_x as f32, rect.min_y as f32, rect.max_x as f32, rect.max_y as f32);
    let corner = |sx: f32, sy: f32, u: f32, v: f32| TileVertex { screen: [sx, sy], uv: [u, v], clamp, depth };
    // (top left, bottom left, bottom right) and (top left, bottom right, top right): the picture's own quad
    [
        corner(left, top, u0, v0),
        corner(left, bottom, u0, v1),
        corner(right, bottom, u1, v1),
        corner(left, top, u0, v0),
        corner(right, bottom, u1, v1),
        corner(right, top, u1, v0),
    ]
}

/// Give every texel of an opaque tile that a nearer (or later, at equal depth) opaque tile covers the
/// colour of what covers it (see the module docs, "Hidden texels"). The tiles are in paint order, so
/// the owner of a pixel is the last opaque tile with a drawn texel there, as in the flat picture and
/// as the depth test decides it. Blended tiles neither cover nor are changed.
fn bake_hidden_texels(tiles: &[DepthTile], atlas: &mut TileAtlas, canvas: ScreenRect) {
    let (Ok(cw), Ok(ch)) = (usize::try_from(canvas.width()), usize::try_from(canvas.height())) else { return };
    let Some(pixels) = cw.checked_mul(ch) else { return };
    let aw = atlas.width as usize;
    // The canvas index of texel `(x, y)` of a tile, if it is on the canvas.
    let place = |t: &DepthTile, x: usize, y: usize| -> Option<usize> {
        let px = usize::try_from(i64::from(t.rect.min_x) - i64::from(canvas.min_x) + i64::try_from(x).ok()?).ok()?;
        let py = usize::try_from(i64::from(t.rect.min_y) - i64::from(canvas.min_y) + i64::try_from(y).ok()?).ok()?;
        (px < cw && py < ch).then_some(py * cw + px)
    };
    let mut owner = vec![0u32; pixels];
    let mut colour = vec![[0u8; 3]; pixels];
    for (i, t) in tiles.iter().enumerate().filter(|(_, t)| t.blend.is_none()) {
        let size = t.size() as usize;
        let tag = u32::try_from(i + 1).unwrap_or(u32::MAX);
        for (n, texel) in t.rgba.chunks_exact(4).enumerate() {
            if texel[3] == 0 {
                continue;
            }
            if let Some(at) = place(t, n % size, n / size) {
                owner[at] = tag;
                colour[at] = [texel[0], texel[1], texel[2]];
            }
        }
    }
    for (i, (t, rect)) in tiles.iter().zip(&atlas.rects).enumerate().filter(|(_, (t, _))| t.blend.is_none()) {
        let size = t.size() as usize;
        let tag = u32::try_from(i + 1).unwrap_or(u32::MAX);
        for (n, texel) in t.rgba.chunks_exact(4).enumerate() {
            let (x, y) = (n % size, n / size);
            let Some(at) = place(t, x, y).filter(|_| texel[3] != 0) else { continue };
            if owner[at] == tag {
                continue;
            }
            let to = ((rect.y as usize + y) * aw + rect.x as usize + x) * 4;
            if let Some(dst) = atlas.rgba.get_mut(to..to + 3) {
                dst.copy_from_slice(&colour[at]);
            }
        }
    }
}

/// The atlas as premultiplied RGBA8 with one opaque black row appended (see [`Scenery::atlas`]).
/// Premultiplied so that the linear filter at a silhouette gives the colour of the opaque texels
/// only (the shader divides the alpha out again) instead of darkening it with the transparent ones.
fn premultiplied_with_black_row(atlas: &TileAtlas) -> Option<Vec<u8>> {
    let (w, h) = (usize::try_from(atlas.width).ok()?, usize::try_from(atlas.height).ok()?);
    if atlas.rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let mut out = Vec::with_capacity(w.checked_mul(h.checked_add(1)?)?.checked_mul(4)?);
    for px in atlas.rgba.chunks_exact(4) {
        let a = u16::from(px[3]);
        let scale = |c: u8| u8::try_from((u16::from(c) * a + 127) / 255).unwrap_or(u8::MAX);
        out.extend_from_slice(&[scale(px[0]), scale(px[1]), scale(px[2]), px[3]]);
    }
    for _ in 0..w {
        out.extend_from_slice(&[0, 0, 0, 255]);
    }
    Some(out)
}

impl Scenery {
    /// Prepare `tiles` (from [`decode_background_tiles_with_depth_options`], in its back-to-front order) for
    /// drawing on a picture covering `canvas` (field-screen units). `None`, with the reason for the
    /// log, when there is nothing in front of the characters (no tile of layer 2 or 4: the flat
    /// picture is then exactly right and cheaper), the canvas is empty, or the tiles do not fit an
    /// atlas.
    pub fn choose(tiles: &[DepthTile], canvas: ScreenRect) -> (Option<Self>, String) {
        if canvas.width() <= 0 || canvas.height() <= 0 {
            return (None, "no picture, so nothing goes in front of the characters".to_string());
        }
        let in_front = tiles.iter().filter(|t| matches!(t.layer, 2 | 4)).count();
        if in_front == 0 {
            return (None, "nothing in front of the characters".to_string());
        }
        let Some(mut atlas) = pack_atlas(tiles, ATLAS_MAX_WIDTH) else {
            return (None, "the tiles do not fit one atlas; characters over the whole picture".to_string());
        };
        bake_hidden_texels(tiles, &mut atlas, canvas);
        let Some(pixels) = premultiplied_with_black_row(&atlas) else {
            return (None, "the tile atlas is malformed; characters over the whole picture".to_string());
        };
        let tex = (atlas.width, atlas.height + 1);
        // The base quad samples the black row; its depth is the far plane (behind every tile).
        let black = (0, atlas.height, 1);
        let mut opaque = Vec::with_capacity((tiles.len() + 1) * 6);
        opaque.extend(quad(canvas, black, tex, DEPTH_FAR));
        let mut blended = Vec::new();
        let mut runs: Vec<BlendRun> = Vec::new();
        for (tile, at) in tiles.iter().zip(&atlas.rects) {
            let vertices = quad(tile.rect, (at.x, at.y, at.size), tex, tile.depth);
            match tile.blend {
                None => opaque.extend(vertices),
                Some(mode) => {
                    let first = u32::try_from(blended.len()).unwrap_or(u32::MAX);
                    blended.extend(vertices);
                    match runs.last_mut() {
                        Some(run) if run.mode == mode => run.count += 6,
                        _ => runs.push(BlendRun { mode, first, count: 6 }),
                    }
                }
            }
        }
        let counts = SceneryCounts {
            tiles: tiles.len(),
            opaque: tiles.iter().filter(|t| t.blend.is_none()).count(),
            blended: tiles.iter().filter(|t| t.blend.is_some()).count(),
            in_front,
            atlas: tex,
        };
        let note = format!(
            "{} tiles ({} opaque, {} blended in {} runs; {in_front} of layers 2 and 4) drawn with depth from a {}x{} atlas",
            counts.tiles,
            counts.opaque,
            counts.blended,
            runs.len(),
            tex.0,
            tex.1
        );
        let scenery = Self {
            id: NEXT_SCENERY_ID.fetch_add(1, Ordering::Relaxed),
            atlas_width: tex.0,
            atlas_height: tex.1,
            atlas: pixels,
            opaque,
            blended,
            runs,
            counts,
        };
        (Some(scenery), note)
    }

    /// Select authored frames before atlas packing and hidden-texel baking. Hidden variants
    /// never contribute either colour or depth to this scenery. A background that does not
    /// decode gives `None` and the reason.
    pub fn from_field_with_options(field: &FieldFile, canvas: ScreenRect, options: &BackgroundOptions) -> (Option<Self>, String) {
        match decode_background_tiles_with_depth_options(field, options) {
            Ok(tiles) => Self::choose(&tiles, canvas),
            Err(e) => (None, format!("background tiles not decoded ({e}); characters over the whole picture")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fmv_ff7::background::depth::{depth_of_id, tile_depth, AtlasRect};

    /// A `size` x `size` tile of one opaque colour.
    pub(crate) fn tile(layer: u8, id: u16, at: (i32, i32), size: i32, rgb: [u8; 3], blend: Option<BlendMode>) -> DepthTile {
        let n = (size * size) as usize;
        let mut rgba = Vec::with_capacity(n * 4);
        for _ in 0..n {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        DepthTile {
            layer,
            index: 0,
            id,
            rect: ScreenRect { min_x: at.0, min_y: at.1, max_x: at.0 + size, max_y: at.1 + size },
            depth: tile_depth(layer, id),
            blend,
            param: 0,
            state: 0,
            rgba,
        }
    }

    fn canvas() -> ScreenRect {
        ScreenRect { min_x: -64, min_y: -48, max_x: 64, max_y: 48 }
    }

    fn sample() -> Vec<DepthTile> {
        let add = Some(BlendMode::Additive);
        vec![
            tile(3, 0, (-64, -48), 32, [10, 10, 10], None),
            tile(1, 4095, (-32, 0), 16, [20, 20, 20], None),
            tile(1, 4095, (-16, 0), 16, [30, 30, 30], None),
            tile(2, 50, (0, 0), 16, [40, 40, 40], None),
            tile(2, 40, (16, 0), 16, [50, 50, 50], add),
            tile(2, 30, (32, 0), 16, [60, 60, 60], add),
            tile(2, 20, (48, 0), 16, [70, 70, 70], Some(BlendMode::Average)),
            tile(4, 0, (0, 16), 32, [80, 80, 80], None),
        ]
    }

    #[test]
    fn the_scenery_keeps_the_paint_order_and_groups_blended_tiles_into_runs() {
        let (scenery, note) = Scenery::choose(&sample(), canvas());
        let s = scenery.expect(&note);
        // Base quad first, then the five opaque tiles in the order given.
        assert_eq!(s.opaque.len(), 6 * (1 + 5));
        let depths: Vec<f32> = s.opaque.chunks(6).map(|q| q[0].depth).collect();
        assert_eq!(depths, [DEPTH_FAR, 16_384.0, 16_380.0, 16_380.0, 200.0, 0.0]);
        // Every vertex of a quad carries its depth.
        assert!(s.opaque.chunks(6).all(|q| q.iter().all(|v| v.depth == q[0].depth)));
        // Three blended tiles: two additive in a row, then one average.
        assert_eq!(s.blended.len(), 18);
        assert_eq!(s.runs, [BlendRun { mode: BlendMode::Additive, first: 0, count: 12 }, BlendRun { mode: BlendMode::Average, first: 12, count: 6 }]);
        let blended_depths: Vec<f32> = s.blended.chunks(6).map(|q| q[0].depth).collect();
        assert_eq!(blended_depths, [160.0, 120.0, 80.0]);
        assert_eq!(s.counts, SceneryCounts { tiles: 8, opaque: 5, blended: 3, in_front: 5, atlas: s.counts.atlas });
        assert!(note.contains("8 tiles") && note.contains("3 blended in 2 runs"), "{note}");
        // The base covers the whole canvas in field-screen units.
        let base = &s.opaque[..6];
        let xs = base.iter().map(|v| v.screen[0]);
        let ys = base.iter().map(|v| v.screen[1]);
        assert_eq!((xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max)), (-64.0, 64.0));
        assert_eq!((ys.clone().fold(f32::MAX, f32::min), ys.fold(f32::MIN, f32::max)), (-48.0, 48.0));
        // Debug never prints pixels.
        assert!(format!("{s:?}").len() < 300, "{s:?}");
    }

    #[test]
    fn runs_alternate_with_the_modes_so_paint_order_is_never_reordered() {
        let modes = [BlendMode::Additive, BlendMode::Average, BlendMode::Additive, BlendMode::Subtractive, BlendMode::Subtractive];
        let mut tiles = vec![tile(1, 4095, (0, 0), 16, [1, 2, 3], None)];
        tiles.extend(modes.iter().enumerate().map(|(i, m)| tile(2, 100 - i as u16, (16 * i as i32, 0), 16, [9, 9, 9], Some(*m))));
        let s = Scenery::choose(&tiles, canvas()).0.unwrap();
        let got: Vec<(BlendMode, u32)> = s.runs.iter().map(|r| (r.mode, r.count / 6)).collect();
        assert_eq!(got, [(BlendMode::Additive, 1), (BlendMode::Average, 1), (BlendMode::Additive, 1), (BlendMode::Subtractive, 2)]);
        // The runs tile the blended list exactly.
        let mut next = 0;
        for r in &s.runs {
            assert_eq!(r.first, next);
            next += r.count;
        }
        assert_eq!(next as usize, s.blended.len());
    }

    #[test]
    fn each_tile_samples_exactly_its_own_atlas_square_and_never_beyond_its_edge_texels() {
        let tiles = sample();
        let atlas = pack_atlas(&tiles, ATLAS_MAX_WIDTH).unwrap();
        let s = Scenery::choose(&tiles, canvas()).0.unwrap();
        let (tw, th) = (s.atlas_width as f32, s.atlas_height as f32);
        assert_eq!((s.atlas_width, s.atlas_height), (atlas.width, atlas.height + 1), "one row more than the packed atlas");
        assert_eq!(s.atlas.len(), (s.atlas_width * s.atlas_height * 4) as usize);
        // The 4th tile (the first layer-2 one) is the 4th quad after the base.
        let q = &s.opaque[6 * 4..6 * 5];
        let AtlasRect { x, y, size } = atlas.rects[3];
        let (u0, v0, u1, v1) = (x as f32 / tw, y as f32 / th, (x + size) as f32 / tw, (y + size) as f32 / th);
        assert_eq!(q[0].uv, [u0, v0], "top left");
        assert_eq!(q[2].uv, [u1, v1], "bottom right");
        assert_eq!(q[5].uv, [u1, v0], "top right");
        // Screen corners are the tile's rectangle: x right, y down.
        assert_eq!(q[0].screen, [0.0, 0.0]);
        assert_eq!(q[2].screen, [16.0, 16.0]);
        // The clamp is the square of texel CENTRES: half a texel inside on every side.
        assert_eq!(q[0].clamp, [(x as f32 + 0.5) / tw, (y as f32 + 0.5) / th, ((x + size) as f32 - 0.5) / tw, ((y + size) as f32 - 0.5) / th]);
        assert!(q.iter().all(|v| v.clamp == q[0].clamp));
        assert!(q[0].clamp[0] > u0 && q[0].clamp[2] < u1 && q[0].clamp[1] > v0 && q[0].clamp[3] < v1);
        // The pixels are in the atlas where the packer put them.
        let at = ((y * s.atlas_width + x) * 4) as usize;
        assert_eq!(s.atlas[at..at + 4], [40, 40, 40, 255]);
        // The base quad's clamp is one point: the centre of the black texel in the last row.
        let black = &s.opaque[..6];
        let want = [0.5 / tw, (th - 0.5) / th, 0.5 / tw, (th - 0.5) / th];
        assert!(black.iter().all(|v| v.clamp == want), "{:?} {want:?}", black[0].clamp);
        let (bx, by) = ((want[0] * tw) as usize, (want[1] * th) as usize);
        assert_eq!(by, (s.atlas_height - 1) as usize);
        let at = (by * s.atlas_width as usize + bx) * 4;
        assert_eq!(s.atlas[at..at + 4], [0, 0, 0, 255]);
    }

    /// The atlas texel `(x, y)` of the `n`th tile of a list of tiles that all have the one size `size`
    /// (they are packed in order along a row).
    fn texel(s: &Scenery, n: usize, size: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * s.atlas_width as usize + n * size + x) * 4;
        [s.atlas[at], s.atlas[at + 1], s.atlas[at + 2], s.atlas[at + 3]]
    }

    #[test]
    fn texels_hidden_by_a_nearer_tile_take_the_colour_of_what_hides_them() {
        const KEY: [u8; 3] = [0, 255, 0];
        // The back layer's key green under an object: a far tile of pure green, a nearer tile of
        // magenta over its right part whose first column is a hole (index 0).
        let far = tile(1, 4095, (0, 0), 16, KEY, None);
        let mut near = tile(2, 50, (8, 0), 16, [200, 0, 200], None);
        for y in 0..16 {
            near.rgba[y * 16 * 4..y * 16 * 4 + 4].copy_from_slice(&[9, 9, 9, 0]);
        }
        // A blended tile over the far tile's left edge covers nothing.
        let glow = tile(2, 60, (0, 0), 16, [255, 255, 255], Some(BlendMode::Additive));
        let s = Scenery::choose(&[far, near, glow], canvas()).0.unwrap();
        for y in [0usize, 7, 15] {
            for x in 0..16usize {
                // the far tile spans x = 0..16 on the canvas; the near tile covers x = 9..16 (its
                // column 0, at x = 8, is a hole: the far tile shows there)
                let want = if x >= 9 { [200, 0, 200, 255] } else { [KEY[0], KEY[1], KEY[2], 255] };
                assert_eq!(texel(&s, 0, 16, x, y), want, "far tile at ({x}, {y})");
            }
        }
        // The near tile itself is as it was: its hole still a hole, the rest its colour.
        assert_eq!(texel(&s, 1, 16, 0, 3), [0, 0, 0, 0]);
        assert_eq!(texel(&s, 1, 16, 5, 3), [200, 0, 200, 255]);
        // No key-colour texel is left where something covers it.
        let covered_key = (9..16usize).filter(|&x| texel(&s, 0, 16, x, 4)[..3] == KEY).count();
        assert_eq!(covered_key, 0);
        // The blended tile is untouched (white, alpha 255).
        assert_eq!(texel(&s, 2, 16, 2, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn a_tile_hidden_by_a_later_tile_of_the_same_depth_and_by_nothing_else_is_baked_exactly_there() {
        // Same id: the later one in the file paints over the earlier, so it owns the overlap; a tile
        // that is only partly on the canvas is baked where it is on it and never panics.
        let a = tile(2, 100, (0, 0), 16, [10, 20, 30], None);
        let b = tile(2, 100, (0, 0), 16, [200, 100, 50], None);
        let s = Scenery::choose(&[a, b], canvas()).0.unwrap();
        assert_eq!(texel(&s, 0, 16, 5, 5), [200, 100, 50, 255], "the earlier tile of the pair is all hidden");
        assert_eq!(texel(&s, 1, 16, 5, 5), [200, 100, 50, 255]);
        let edge = tile(2, 100, (canvas().max_x - 8, 0), 16, [1, 2, 3], None);
        let over = tile(2, 90, (canvas().max_x - 8, 0), 16, [4, 5, 6], None);
        let s = Scenery::choose(&[edge, over], canvas()).0.unwrap();
        assert_eq!(texel(&s, 0, 16, 2, 2), [4, 5, 6, 255], "on the canvas: hidden, baked");
        assert_eq!(texel(&s, 0, 16, 12, 2), [1, 2, 3, 255], "off the canvas: nothing to bake from");
    }

    #[test]
    fn the_atlas_is_premultiplied_so_transparent_texels_carry_no_colour() {
        let mut t = tile(2, 10, (0, 0), 16, [200, 100, 50], None);
        // One transparent texel with a loud colour, one half-covered texel (never produced by the
        // decoder, but the arithmetic is general).
        t.rgba[0..4].copy_from_slice(&[255, 255, 255, 0]);
        t.rgba[4..8].copy_from_slice(&[200, 100, 50, 128]);
        let s = Scenery::choose(&[t], canvas()).0.unwrap();
        // (a single tile is packed at the atlas's origin)
        let at = 0usize;
        assert_eq!(s.atlas[at..at + 4], [0, 0, 0, 0], "transparent: no colour");
        assert_eq!(s.atlas[at + 4..at + 8], [100, 50, 25, 128], "half alpha: colour halved");
        assert_eq!(s.atlas[at + 8..at + 12], [200, 100, 50, 255], "opaque: untouched");
    }

    #[test]
    fn nothing_in_front_an_empty_canvas_and_unpackable_tiles_all_fall_back_to_the_flat_picture() {
        // Only layers 1 and 3: the flat picture is exact, so no scenery.
        let behind = [tile(1, 4095, (0, 0), 16, [1, 1, 1], None), tile(3, 0, (0, 0), 32, [2, 2, 2], None)];
        let (none, note) = Scenery::choose(&behind, canvas());
        assert!(none.is_none() && note.contains("nothing in front"), "{note}");
        // A layer-4 tile alone is something in front.
        assert!(Scenery::choose(&[tile(4, 0, (0, 0), 32, [1, 1, 1], None)], canvas()).0.is_some());
        // No picture to put it on.
        let flat = ScreenRect { min_x: 0, min_y: 0, max_x: 0, max_y: 10 };
        assert!(Scenery::choose(&sample(), flat).0.is_none());
        // A tile the packer refuses (not square) is not drawn crooked.
        let mut odd = sample();
        odd[3].rect.max_x += 1;
        let (none, note) = Scenery::choose(&odd, canvas());
        assert!(none.is_none() && note.contains("atlas"), "{note}");
        // And no tiles at all.
        assert!(Scenery::choose(&[], canvas()).0.is_none());
        // Every scenery is its own: a changed field is uploaded again.
        let (a, b) = (Scenery::choose(&sample(), canvas()).0.unwrap(), Scenery::choose(&sample(), canvas()).0.unwrap());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn the_depth_mapping_is_the_engines_perspective_value_clamped() {
        let [a, b] = depth_terms();
        // A = f / (f - n) = 1.0015649, B = f n / (f - n) = 50.078247 (the dumped matrix of Makou Reactor
        // is these up to a common factor).
        assert!((a - 1.001_565).abs() < 1e-6 && (b - 50.078_25).abs() < 1e-3, "{a} {b}");
        // The planes map to the ends of the buffer; beyond them it clamps.
        assert!(buffer_depth(DEPTH_NEAR) < 1e-6);
        assert!((buffer_depth(DEPTH_FAR) - 1.0).abs() < 1e-6);
        assert_eq!(buffer_depth(DEPTH_FAR * 3.0), 1.0);
        // "In front of everything": depth 0, a depth below the near plane, a camera-plane touching
        // point, all the nearest value (never culled, never NaN).
        for d in [0.0, 0.5, 4.0, 48.0, -10.0, DEPTH_NEAR] {
            assert!(buffer_depth(d) < 1e-6, "{d}");
        }
        assert_eq!(buffer_depth(f32::MAX), 1.0);
        assert!(!buffer_depth(f32::NAN).is_nan(), "a damaged depth never reaches the buffer as NaN");
        // Increasing, so "nearer in the camera" is "nearer in the buffer".
        let mut last = -1.0f32;
        for d in (51..32_000).step_by(37) {
            let v = buffer_depth(d as f32);
            assert!(v > last, "{d}");
            last = v;
        }
    }

    #[test]
    fn every_tile_depth_is_its_own_buffer_value_even_at_the_far_end() {
        // The reason for a fixed mapping: layer 3, layer 1 and every id of layer 2 stay distinct and
        // ordered, so the depth test orders tiles as the paint order does (a mapping fitted to the
        // models of a frame would send them all to 1.0).
        let layer3 = buffer_depth(tile_depth(3, 0));
        let layer1 = buffer_depth(tile_depth(1, 0));
        assert!(layer3 > layer1 && layer1 < 1.0 && layer3 < 1.0, "{layer1} {layer3}");
        let mut last = layer1;
        // Walk ids down from 4095 (depth 16 380): each is nearer than the one before and distinct.
        let mut smallest_step = f32::MAX;
        for id in (13..=4095u16).rev() {
            let v = buffer_depth(depth_of_id(id));
            assert!(v <= last, "id {id}");
            if id < 4095 {
                assert!(v < last, "id {id} has a buffer value of its own");
                smallest_step = smallest_step.min(last - v);
            }
            last = v;
        }
        // About 12 f32 steps between neighbouring ids at the far end (just below 1.0 a step is 6e-8).
        assert!(smallest_step > 10.0 * 5.9e-8, "{smallest_step}");
        // Ids of 12 and below are nearer than the near plane: all in front of everything.
        assert!((1..=12u16).all(|id| buffer_depth(depth_of_id(id)) < 1e-6));
        // Walkable depths (275 to 6650 in the fields measured): one unit of depth is far above the f32
        // step, so a model that is a unit nearer than a tile is in front of it.
        for d in [275.0f32, 468.0, 945.0, 2176.0, 2385.0, 6650.0] {
            assert!(buffer_depth(d + 1.0) - buffer_depth(d) > 3.0 * f32::EPSILON, "{d}");
        }
    }
}
