//! Per-tile depth of the PC field background: the depth rule, [`DepthTile`] quads for a depth-tested
//! renderer, and the matching depth of a world point.
//!
//! One flattened front image ([`super::decode_background_layers`]) draws every layer-2 object over
//! every character, which is wrong wherever the character stands in front of the object. The PC
//! engine does not do that: each background tile carries a depth and a character is hidden by a tile
//! exactly where the character is farther from the camera than the tile. This module exposes that
//! depth, in the same units as [`super::Projection::project`]'s `depth`, so a client can draw the
//! tiles as quads in the same depth-tested pass as the character models.
//!
//! # The rule
//!
//! **A tile's depth, in camera-space units, is `4 * id`** ([`TILE_DEPTH_PER_ID`]), where `id` is the
//! tile record's `id` field (offset +24), for layer-2 tiles. The other layers have fixed depths: layer 1
//! is [`LAYER1_DEPTH`] (id 4095), layer 3 is [`LAYER3_DEPTH`] (id 4096, behind layer 1) and layer 4 is
//! [`LAYER4_DEPTH`] (id 0, in front of everything), the ids that Makou Reactor assigns them. A world
//! point at camera depth `d` ([`depth_of_point`]) is hidden by a tile it overlaps on the screen
//! exactly when the tile is nearer: [`tile_hides_point`], `tile_depth < d`. Larger `id` is farther.
//!
//! Depth 0 (layer 4, and the layer-2 ids whose depth is below the engine's near plane, 12 and
//! below in the usual field) means "in front of everything": a client that maps depth to a
//! perspective z-buffer must clamp it to its near plane, not cull it.
//!
//! # Evidence
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08** over all 702 fields (the tools are
//! `examples/depth_survey.rs`, `examples/depth_composite.rs` and the ignored tests of
//! `tests/depth_real_data.rs`; counts and ratios only, no game bytes). Other editions are unverified.
//! The Makou Reactor source (<https://github.com/myst6re/makoureactor>,
//! `src/core/field/BackgroundTiles.h`, `BackgroundTilesIO.cpp` and `BackgroundTiles.cpp`) keys its tile
//! map by `4096 - id`, assigns layers 1, 3 and 4 the ids 4095, 4096 and 0 (the stored ids of all
//! 342 792, 7 062 and 6 965 such tiles of the owned data), keeps a PC-only `IDBig` value at record
//! offset +38 that it can only approximate, and contains, commented out, a decompilation of the
//! engine's own conversion that places a tile at camera depth `4 * id` (`sourceDir[2] = 4 * ID -
//! camera_position[2]`) and a dump of the engine's projection matrix. Aali's `ff7_opengl` driver
//! (<https://github.com/Aali132/ff7_opengl/blob/master/ff7/field.c>, `field_layer2_pick_tiles`) shows the
//! engine draws every layer-2 tile as one quad with its own `z` (`add_page_tile(x, y, tile.z, u, v,
//! palette, page)`), in the same z-buffered 3D pipeline as the models. The data confirm the conversion
//! twice, independently:
//!
//! 1. *The engine's own number.* `IDBig / 1e7` of a layer-2 tile is a Direct3D perspective z-buffer
//!    value `A - B / z` with `A = f / (f - n)` and `B = f * n / (f - n)`. Fitting that to the tiles of
//!    each field with `z = 4 * id` gives a near plane `n` of 50.0 and a far plane `f` of 32 000 in 394
//!    of the 451 fields that have four usable ids (29 more give `n` of about 512, 28 other values; 412 of
//!    the 451 fits have a residual below 1e-4), with residuals of 1e-7 to 1e-5: the stored value is the
//!    z-buffer value of a point at depth `4 * id`. The projection matrix dumped in Makou Reactor's
//!    comment (`0.438642`, `0.437956`, `-21.932079`) is this `A` (1.001566) and `B` (50.078) up to a
//!    common factor, near plane 50.0; a scale other than 4 would have fitted a near plane of
//!    `12.5 * k`, not that 50. Ids whose depth is below the near plane are stored as `10000 * id`
//!    instead (999 for id 0): in front of everything.
//! 2. *The walkmesh.* The nearest walkmesh surface is rasterised per pixel through
//!    [`super::Projection`]. For each connected object of layer-2 pixels (same visible id, 4 015 objects in
//!    630 fields) the floor just below its lowest pixel in each column (where it stands) has a depth
//!    of `0.89 / 0.98 / 1.01 / 1.06 / 1.20` times `4 * id` (10th / 25th / median / 75th / 90th
//!    percentile of the per-object medians; 83.6% of the objects within 0.8 to 1.25). The two sides of an
//!    object separate sharply at the scale 4 and nowhere else: the share of floor pixels under objects
//!    that are farther than the object is 89% / 85% / 81% / 70% / 27% / 21% / 18% / 17% for `k * id` with
//!    k = 1, 2, 3, 4, 5, 6, 8, 16, while the share of floor pixels just below an object's base that are
//!    nearer than it is 8% / 10% / 14% / 70% / 88% / 90% / 90% / 91%: only k = 4 puts the objects where
//!    they stand. The nearest floor under a tile is not nearer than `4 * id` by more than 3% for 75.5% of
//!    the 34 143 tiles that have floor under them, by more than 10% for 83.3%, 25% for 87.2%; the rest
//!    are presumably objects that extend in depth under one id, or hang above the floor, and tiles that
//!    are not objects at all (ids of 4000 and over are left out of these numbers). The deepest walkmesh
//!    vertex of any field is 15 423, below layer 1's 16 380.
//!
//! Examples: in `md1stin` the layer-2 ids are 1, 152, 294, 544, 4087 and 4088 (depths 4, 608, 1 176,
//! 2 176, 16 348 and 16 352) against walkmesh depths of 275 to 2 385, so the far left wall piece (id
//! 544) has walkable floor both nearer and farther than it; at the spot where the flat front layer hides
//! 1 296 of Cloud's 3 305 pixels, 1 270 of them under that wall piece (depth 468), the rule hides none,
//! and at the far end of the platform (depth 2 232) it does hide him (`examples/depth_composite.rs`). In `md1_1` and `mds7st1` the
//! objects that stand on the floor have `4 * id` within 0.97 to 1.11 of the floor depth at their base,
//! increasing with distance (`examples/depth_survey.rs md1_1 mds7st1`).
//!
//! # What is not modelled
//!
//! * Which animation groups are on is script state ([`super::GroupFilter`]); tiles of a hidden group
//!   are left out, as in [`super::decode_background`].
//! * The engine's own per-tile depth is not exactly `4 * id`: the fixed-point camera matrix adds a few
//!   units (the stored `IDBig` of `md1stin` fits best with `4 * id - 4.6`, and differs by about 0.03
//!   between the tiles of one id). `4 * id` is used as is; the real spread of objects around their
//!   ids (above) is far larger.
//! * Fields with several different camera records (only `mds5_1`) are measured with camera 0 only, like
//!   the rest of this crate.
//! * Depth is per tile, not per pixel, and the blended (glow, shadow, glass) tiles are drawn with the
//!   same rule: they should be drawn after the opaque tiles and the models, with the depth test and no
//!   depth write.
//!
//! # Safety against untrusted input
//!
//! Everything works on the parsed tile list, so the limits of [`crate::field`] apply; no index is used
//! unchecked and the atlas size is capped.

use super::{draw_depth, BackgroundOptions, BlendMode, Projection, ScreenRect};
use crate::field::{Background, FieldError, FieldFile, Palette};

/// Camera-space depth units per unit of a layer-2 tile's `id`.
pub const TILE_DEPTH_PER_ID: f32 = 4.0;
/// Depth of every layer-1 tile (id 4095): behind all walkable depths (the deepest walkmesh vertex of
/// the 702 owned fields is 15 423).
pub const LAYER1_DEPTH: f32 = 4095.0 * TILE_DEPTH_PER_ID;
/// Depth of every layer-3 tile (id 4096): behind layer 1.
pub const LAYER3_DEPTH: f32 = 4096.0 * TILE_DEPTH_PER_ID;
/// Depth of every layer-4 tile (id 0): in front of everything.
pub const LAYER4_DEPTH: f32 = 0.0;
/// The engine's usual near plane (50.0 in 394 of 451 fitted fields; about 512 in 29 more). Tiles deeper than
/// this and the characters share its perspective z-buffer; see the module docs.
pub const ENGINE_NEAR_PLANE: f32 = 50.0;
/// The engine's usual far plane (about 32 000 in the fields fitted).
pub const ENGINE_FAR_PLANE: f32 = 32_000.0;

/// Most RGBA bytes [`depth_tiles`] keeps for one field (64 MiB; the owned field with the most tile
/// pixels, `fship_2`, has 2 828 288 bytes in 1 322 tiles). A field over it is an error, not a partial
/// result.
pub const MAX_DEPTH_TILE_BYTES: usize = 64 * 1024 * 1024;
/// Atlas widths above this are refused ([`pack_atlas`]); no tile is wider than 32.
pub const MAX_ATLAS_WIDTH: u32 = 8192;
/// Atlas heights above this are refused ([`pack_atlas`]).
pub const MAX_ATLAS_HEIGHT: u32 = 8192;

/// The depth the rule gives a layer-2 tile with this `id`: [`TILE_DEPTH_PER_ID`] times it.
pub fn depth_of_id(id: u16) -> f32 {
    TILE_DEPTH_PER_ID * f32::from(id)
}

/// The depth of a tile: [`depth_of_id`] of its `id` for layer 2, the fixed depth of the layer
/// otherwise ([`LAYER1_DEPTH`], [`LAYER3_DEPTH`], [`LAYER4_DEPTH`]).
pub fn tile_depth(layer: u8, id: u16) -> f32 {
    match layer {
        1 => LAYER1_DEPTH,
        3 => LAYER3_DEPTH,
        4 => LAYER4_DEPTH,
        _ => depth_of_id(id),
    }
}

/// The depth of a field (walkmesh) point in the units of [`DepthTile::depth`]: its distance in front
/// of the camera ([`Projection::project`]'s `depth`). `None` if the point is at or behind the camera
/// plane (depth below 1), where it cannot be drawn.
pub fn depth_of_point(projection: &Projection, world: [f32; 3]) -> Option<f32> {
    projection.project(world).map(|p| p.depth)
}

/// Whether a tile at `tile_depth` hides a point at `point_depth` where they overlap on the screen:
/// the tile is nearer. A point at exactly the tile's depth is not hidden.
pub fn tile_hides_point(tile_depth: f32, point_depth: f32) -> bool {
    tile_depth < point_depth
}

/// One background tile prepared for a depth-tested quad: where it goes, what it looks like, how far
/// away it is and how it combines with what is behind it.
#[derive(Clone, PartialEq)]
pub struct DepthTile {
    /// 1..=4.
    pub layer: u8,
    /// Index within its layer in the file ([`crate::field::TileSprite::index`]).
    pub index: u16,
    /// The tile record's `id` as stored (layer 2's depth source; layers 1, 3 and 4 store 4095, 4096 and
    /// 0).
    pub id: u16,
    /// Destination in field-screen pixels (top-left inclusive, `max_*` exclusive; x right, y down; see
    /// [`super::Projection`]): `size` x `size`.
    pub rect: ScreenRect,
    /// Camera-space depth of the whole tile ([`tile_depth`]): smaller is nearer; 0 is "in front of
    /// everything" (clamp to the near plane, do not cull).
    pub depth: f32,
    /// `Some(mode)` for a blended tile (layers 2-4 with the record's `blending` set, when the options
    /// allow blending): combine with what is behind it as [`BlendMode`] says, over the opaque pixels
    /// of [`DepthTile::rgba`]. `None`: opaque.
    pub blend: Option<BlendMode>,
    /// Animation group and frame ([`crate::field::TileSprite::param`] and `state`): `param == 0` is
    /// always visible.
    pub param: u8,
    pub state: u8,
    /// `size * size` RGBA pixels, row-major, top row first, straight alpha (0 for the transparent
    /// pixels, 255 otherwise): for a blended tile the pixels of its secondary source, as drawn.
    pub rgba: Vec<u8>,
}

/// `Debug` prints the geometry and depth only, never pixels.
impl std::fmt::Debug for DepthTile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DepthTile")
            .field("layer", &self.layer)
            .field("index", &self.index)
            .field("id", &self.id)
            .field("rect", &self.rect)
            .field("depth", &self.depth)
            .field("blend", &self.blend)
            .field("group", &(self.param, self.state))
            .finish_non_exhaustive()
    }
}

impl DepthTile {
    /// Edge length in pixels (16 or 32).
    pub fn size(&self) -> u32 {
        u32::try_from(self.rect.width()).unwrap_or(0)
    }

    /// RGBA of the tile's pixel `(x, y)`, or `None` outside the tile.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let size = self.size();
        if x >= size || y >= size {
            return None;
        }
        let at = usize::try_from(y.checked_mul(size)?.checked_add(x)?).ok()?.checked_mul(4)?;
        self.rgba.get(at..at.checked_add(4)?)?.try_into().ok()
    }

    /// Whether the tile hides a point at `point_depth` that projects inside its rectangle
    /// ([`tile_hides_point`]). It ignores the transparent pixels: check [`DepthTile::pixel`] too.
    pub fn hides_depth(&self, point_depth: f32) -> bool {
        tile_hides_point(self.depth, point_depth)
    }
}

/// Decode every tile of the field's background that [`super::decode_background`] would draw, with its
/// depth, in back-to-front order (the order of [`super::compose`]: layer 3, layer 1, layer 2 by
/// descending `id`, layer 4; ties in file order). Drawing them in this order without a depth test
/// reproduces [`super::decode_background`]'s picture; with a depth test against characters it
/// reproduces the engine's layering. Default options: all layers, static animation groups, blending on.
///
/// Tiles whose texture page, palette page or source rectangle is missing are left out, as are tiles
/// with no opaque pixel. A damaged texture section is the same error as [`super::decode_background`].
pub fn decode_background_tiles_with_depth(field: &FieldFile) -> Result<Vec<DepthTile>, FieldError> {
    decode_background_tiles_with_depth_options(field, &BackgroundOptions::default())
}

/// [`decode_background_tiles_with_depth`] with explicit options: `layers` selects the layers kept,
/// `groups` the animation groups, and `blending == false` makes every tile opaque (`blend` is `None`).
pub fn decode_background_tiles_with_depth_options(
    field: &FieldFile,
    options: &BackgroundOptions,
) -> Result<Vec<DepthTile>, FieldError> {
    let palette = field.palette()?;
    decode_background_tiles_with_depth_palette(field, &palette, options)
}

/// The depth-tested path with the same live palette as the flat background decoder.
pub fn decode_background_tiles_with_depth_palette(
    field: &FieldFile,
    palette: &Palette,
    options: &BackgroundOptions,
) -> Result<Vec<DepthTile>, FieldError> {
    let background = field.background()?;
    depth_tiles(&background, palette, options)
}

/// Same as [`decode_background_tiles_with_depth_options`] for an already parsed background and palette.
pub fn depth_tiles(
    background: &Background<'_>,
    palette: &Palette,
    options: &BackgroundOptions,
) -> Result<Vec<DepthTile>, FieldError> {
    if let Some(err) = &background.texture_error {
        return Err(err.clone());
    }
    let mut order: Vec<&crate::field::TileSprite> = background.tiles.iter().collect();
    order.sort_by_key(|t| std::cmp::Reverse(draw_depth(t)));
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for tile in order {
        let layer_on =
            usize::from(tile.layer).checked_sub(1).and_then(|i| options.layers.get(i)).copied().unwrap_or(false);
        if !layer_on || !options.groups.allows(tile) {
            continue;
        }
        let Some(texels) = background.tile_rgba(tile, palette) else { continue };
        if texels.iter().all(|t| t[3] == 0) {
            continue;
        }
        // The pixels of every tile are kept at once, unlike the one canvas of `compose`, so a hostile
        // field with tens of thousands of tiles must not be able to ask for gigabytes.
        bytes = bytes.saturating_add(texels.len().saturating_mul(4));
        if bytes > MAX_DEPTH_TILE_BYTES {
            return Err(FieldError::TooMany {
                what: "background tile pixels (bytes)",
                count: u64::try_from(bytes).unwrap_or(u64::MAX),
                limit: u64::try_from(MAX_DEPTH_TILE_BYTES).unwrap_or(u64::MAX),
            });
        }
        let size = i32::from(tile.tile_size);
        let (min_x, min_y) = (i32::from(tile.dst_x), i32::from(tile.dst_y));
        out.push(DepthTile {
            layer: tile.layer,
            index: tile.index,
            id: tile.id,
            rect: ScreenRect { min_x, min_y, max_x: min_x + size, max_y: min_y + size },
            depth: tile_depth(tile.layer, tile.id),
            blend: (options.blending && tile.layer > 1 && tile.blending != 0)
                .then(|| BlendMode::from_type_trans(tile.type_trans)),
            param: tile.param,
            state: tile.state,
            rgba: texels.iter().flatten().copied().collect(),
        });
    }
    Ok(out)
}

/// Where one tile sits in a [`TileAtlas`]: a `size` x `size` square at `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

/// The pixels of many tiles in one RGBA image (row-major, straight alpha), so a renderer needs one
/// texture for all the quads. `rects[i]` is where `tiles[i]` of the slice given to [`pack_atlas`] went.
/// Texture coordinates of a tile are `rect.x / width`, `(rect.x + rect.size) / width` and likewise for
/// y; sample with nearest filtering (tiles touch each other).
#[derive(Clone, PartialEq, Eq)]
pub struct TileAtlas {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub rects: Vec<AtlasRect>,
}

/// `Debug` prints sizes only, never pixels.
impl std::fmt::Debug for TileAtlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileAtlas")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("tiles", &self.rects.len())
            .finish_non_exhaustive()
    }
}

/// Pack the tiles into one image at most `max_width` pixels wide (at least 32, at most
/// [`MAX_ATLAS_WIDTH`]): the 32-pixel tiles first, then the 16-pixel ones, each in rows, in the order
/// of `tiles` within a size. The image is only as wide as the widest row. `None` if `max_width` is out
/// of range, a tile is not square with a side of 16 or 32 or has the wrong number of pixels, or the
/// result would be taller than [`MAX_ATLAS_HEIGHT`] or exceed the raster byte budget. Native
/// pixel bytes are preserved, including alpha. No tiles give an empty atlas.
pub fn pack_atlas(tiles: &[DepthTile], max_width: u32) -> Option<TileAtlas> {
    for tile in tiles {
        let side = i64::from(tile.rect.max_x) - i64::from(tile.rect.min_x);
        let tall = i64::from(tile.rect.max_y) - i64::from(tile.rect.min_y);
        if !matches!(side, 16 | 32) || tall != side { return None; }
    }
    let pixels: Vec<_> = tiles.iter().map(|tile| super::raster::TilePixels { side: tile.size(), rgba: &tile.rgba }).collect();
    pack_pixels(&pixels, max_width, false)
}

/// Pack raster pixels independently of logical tile geometry. Sides 16, 32, 64 and 128 are
/// accepted; larger squares go first and ties keep input order. The returned rectangles are
/// texel coordinates, while a renderer continues to use the native `DepthTile::rect`.
/// Alpha is clipped at 128 consistently with `raster::compose_tile_pixels`; RGB is preserved.
/// Invalid lengths, widths, total input bytes or atlas allocations return `None` before a
/// pixel buffer is allocated. Empty input produces the same empty atlas as `pack_atlas`.
pub fn pack_tile_pixels(tiles: &[super::raster::TilePixels<'_>], max_width: u32) -> Option<TileAtlas> {
    pack_pixels(tiles, max_width, true)
}

fn pack_pixels(tiles: &[super::raster::TilePixels<'_>], max_width: u32, clip_alpha: bool) -> Option<TileAtlas> {
    use super::raster::{clipped_alpha, MAX_RASTER_BYTES};
    if !(32..=MAX_ATLAS_WIDTH).contains(&max_width) {
        return None;
    }
    let mut total = 0usize;
    for tile in tiles {
        if !tile.valid() || tile.side > max_width { return None; }
        total = total.checked_add(tile.rgba.len())?;
        if total > MAX_RASTER_BYTES { return None; }
    }
    let mut placed: Vec<AtlasRect> = vec![AtlasRect { x: 0, y: 0, size: 0 }; tiles.len()];
    let (mut x, mut y, mut row_height, mut widest) = (0u32, 0u32, 0u32, 0u32);
    for size in [128u32, 64, 32, 16] {
        if row_height != 0 {
            y = y.checked_add(row_height)?;
            x = 0;
            row_height = 0;
        }
        for (tile, slot) in tiles.iter().zip(placed.iter_mut()) {
            if tile.side != size { continue; }
            if x.checked_add(size)? > max_width {
                y = y.checked_add(row_height)?;
                x = 0;
                row_height = 0;
            }
            *slot = AtlasRect { x, y, size };
            x = x.checked_add(size)?;
            widest = widest.max(x);
            row_height = row_height.max(size);
        }
    }
    let height = y.checked_add(row_height)?;
    if height > MAX_ATLAS_HEIGHT { return None; }
    let width = widest;
    let (w, h) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    let bytes = w.checked_mul(h)?.checked_mul(4)?;
    if bytes > MAX_RASTER_BYTES { return None; }
    let mut rgba = vec![0u8; bytes];
    for (tile, rect) in tiles.iter().zip(placed.iter()) {
        let (size, rx, ry) = (usize::try_from(rect.size).ok()?, usize::try_from(rect.x).ok()?, usize::try_from(rect.y).ok()?);
        let row_bytes = size.checked_mul(4)?;
        for row in 0..size {
            let from = row.checked_mul(row_bytes)?;
            let src = tile.rgba.get(from..from.checked_add(row_bytes)?)?;
            let at = ry.checked_add(row)?.checked_mul(w)?.checked_add(rx)?.checked_mul(4)?;
            let dst = rgba.get_mut(at..at.checked_add(row_bytes)?)?;
            dst.copy_from_slice(src);
            if clip_alpha {
                for alpha in dst.iter_mut().skip(3).step_by(4) { *alpha = clipped_alpha(*alpha); }
            }
        }
    }
    Some(TileAtlas { width, height, rgba, rects: placed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::{decode_background, decode_background_layers, GroupFilter};
    use crate::field::{Camera, FieldFile};
    use crate::testutil::{FieldSpec, LayerSpec, TileSpec};

    fn parse(spec: &FieldSpec) -> FieldFile {
        FieldFile::parse(spec.build()).unwrap()
    }

    /// Layer 1: two tiles; layer 2: ids 5 and 50 over layer 1's second tile; layer 3 and 4: one tile
    /// each; nothing blended, no animation groups (the same shape as the background tests').
    fn four_layer_spec() -> FieldSpec {
        let t = |dst_x: i16, dst_y: i16, src_x: u8, page: u8, id: u16| TileSpec {
            dst_x,
            dst_y,
            src_x,
            src_y: 32,
            palette_id: page,
            id,
            ..TileSpec::default()
        };
        let layer = |tiles: Vec<TileSpec>| Some(LayerSpec { width: 640, height: 480, tiles });
        FieldSpec {
            tiles: vec![t(0, 0, 16, 0, 0), t(16, 0, 16, 0, 0)],
            layer2: layer(vec![t(16, 0, 64, 1, 50), t(48, 16, 48, 1, 5)]),
            layer3: layer(vec![t(-32, -32, 32, 0, 0)]),
            layer4: layer(vec![t(48, 48, 80, 1, 0)]),
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        }
    }

    #[test]
    fn the_rule_is_four_per_id_and_the_other_layers_are_fixed() {
        assert_eq!(TILE_DEPTH_PER_ID, 4.0);
        assert_eq!(depth_of_id(0), 0.0);
        assert_eq!(depth_of_id(544), 2176.0);
        assert_eq!(depth_of_id(u16::MAX), 262_140.0);
        assert_eq!(tile_depth(2, 152), 608.0);
        assert_eq!(tile_depth(1, 0), 16_380.0);
        assert_eq!(tile_depth(1, 777), LAYER1_DEPTH, "layer 1 ignores the stored id");
        assert_eq!(tile_depth(3, 0), 16_384.0);
        assert_eq!(tile_depth(4, 999), 0.0, "layer 4 ignores the stored id");
        // behind layer 2's deepest id, in front of nothing
        assert!(LAYER3_DEPTH > LAYER1_DEPTH && LAYER1_DEPTH >= depth_of_id(4095));
        // (through `black_box` so that clippy does not take the comparison of constants for a typo)
        let (near, far) = (std::hint::black_box(ENGINE_NEAR_PLANE), std::hint::black_box(ENGINE_FAR_PLANE));
        assert!(LAYER4_DEPTH < near && far > LAYER3_DEPTH);
    }

    #[test]
    fn a_point_is_hidden_by_a_nearer_tile_only() {
        assert!(tile_hides_point(608.0, 700.0), "the tile is nearer than the point");
        assert!(!tile_hides_point(608.0, 500.0), "the point is nearer than the tile");
        assert!(!tile_hides_point(608.0, 608.0), "a tie keeps the point visible");
        assert!(tile_hides_point(LAYER4_DEPTH, 1.0), "layer 4 hides everything it overlaps");
        assert!(!tile_hides_point(LAYER1_DEPTH, 15_000.0), "layer 1 hides nothing walkable");
    }

    #[test]
    fn depth_of_point_is_the_projection_depth() {
        // looking along +y with +z up, as in the projection tests
        let cam = Camera { axes: [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]], position: [0, 0, 1000], zoom: 500 };
        let p = Projection::from_camera(&cam);
        assert_eq!(depth_of_point(&p, [200.0, 1000.0, 0.0]), Some(2000.0));
        assert_eq!(depth_of_point(&p, [0.0, 600.0, 0.0]), Some(1600.0));
        assert_eq!(depth_of_point(&p, [0.0, -1500.0, 0.0]), None, "behind the camera");
        // it is the number to compare with a tile: a point 1600 away is behind a tile of id 300
        // (depth 1200) and in front of a tile of id 500 (depth 2000)
        let d = depth_of_point(&p, [0.0, 600.0, 0.0]).unwrap();
        assert!(depth_of_id(300) < d && d < depth_of_id(500));
        assert!(tile_hides_point(depth_of_id(300), d) && !tile_hides_point(depth_of_id(500), d));
    }

    #[test]
    fn tiles_carry_their_layer_depth_rect_and_order() {
        let f = parse(&four_layer_spec());
        let tiles = decode_background_tiles_with_depth(&f).unwrap();
        // back to front: layer 3, layer 1 (file order), layer 2 by descending id, layer 4
        let order: Vec<(u8, u16, u16)> = tiles.iter().map(|t| (t.layer, t.index, t.id)).collect();
        // (the ids of layers 1, 3 and 4 are as the spec stored them, 0; their depth does not use them)
        assert_eq!(order, [(3, 0, 0), (1, 0, 0), (1, 1, 0), (2, 0, 50), (2, 1, 5), (4, 0, 0)]);
        let depths: Vec<f32> = tiles.iter().map(|t| t.depth).collect();
        assert_eq!(depths, [16_384.0, 16_380.0, 16_380.0, 200.0, 20.0, 0.0]);
        assert!(depths.windows(2).all(|w| w[0] >= w[1]), "back to front means non-increasing depth here");
        let sizes: Vec<u32> = tiles.iter().map(DepthTile::size).collect();
        assert_eq!(sizes, [32, 16, 16, 16, 16, 32]);
        let r = tiles.iter().find(|t| t.layer == 2 && t.index == 1).unwrap().rect;
        assert_eq!(r, ScreenRect { min_x: 48, min_y: 16, max_x: 64, max_y: 32 });
        assert!(tiles.iter().all(|t| t.blend.is_none() && (t.param, t.state) == (0, 0)));
        assert!(tiles.iter().all(|t| t.rgba.len() == (t.size() * t.size() * 4) as usize));
        // the nearest layer-2 tile (smallest id) hides a character at depth 100, the other does not
        let hides: Vec<(u16, bool)> =
            tiles.iter().filter(|t| t.layer == 2).map(|t| (t.id, t.hides_depth(100.0))).collect();
        assert_eq!(hides, [(50, false), (5, true)]);
    }

    #[test]
    fn painting_the_tiles_in_order_reproduces_the_flat_background() {
        let f = parse(&four_layer_spec());
        let full = decode_background(&f).unwrap();
        let tiles = decode_background_tiles_with_depth(&f).unwrap();
        let mut canvas = vec![0u8; full.rgba.len()];
        for t in &tiles {
            let size = t.size() as i32;
            for y in 0..size {
                for x in 0..size {
                    let px = t.pixel(x as u32, y as u32).unwrap();
                    if px[3] == 0 {
                        continue;
                    }
                    let (cx, cy) = (t.rect.min_x + x + full.origin_x, t.rect.min_y + y + full.origin_y);
                    let at = ((cy as usize) * full.width as usize + cx as usize) * 4;
                    canvas[at..at + 4].copy_from_slice(&px);
                }
            }
        }
        assert_eq!(canvas, full.rgba);
    }

    #[test]
    fn options_select_layers_groups_and_blending() {
        // the default spec: a layer-2 tile in animation group 5 that is blended, plus layers 1 and 4
        let f = parse(&FieldSpec::default());
        let plain = decode_background_tiles_with_depth(&f).unwrap();
        assert!(plain.iter().all(|t| t.layer != 2), "the grouped tile is hidden by default");
        let all = BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() };
        let tiles = decode_background_tiles_with_depth_options(&f, &all).unwrap();
        let grouped = tiles.iter().find(|t| t.layer == 2).unwrap();
        assert_eq!((grouped.param, grouped.state), (5, 6));
        assert!(grouped.blend.is_some(), "the default spec's layer-2 tile is blended");
        let opaque = BackgroundOptions { blending: false, ..all.clone() };
        let tiles = decode_background_tiles_with_depth_options(&f, &opaque).unwrap();
        assert!(tiles.iter().all(|t| t.blend.is_none()));
        let only_4 = BackgroundOptions { layers: [false, false, false, true], ..all };
        let tiles = decode_background_tiles_with_depth_options(&f, &only_4).unwrap();
        assert!(!tiles.is_empty() && tiles.iter().all(|t| t.layer == 4 && t.depth == LAYER4_DEPTH));
    }

    #[test]
    fn blend_modes_follow_the_tile_record_and_layer_1_is_never_blended() {
        for (type_trans, mode) in
            [(0u8, BlendMode::Average), (1, BlendMode::Additive), (2, BlendMode::Subtractive), (3, BlendMode::AddQuarter)]
        {
            let over = TileSpec {
                dst_x: 0,
                dst_y: 0,
                src_x: 16,
                src_y: 32,
                src_x2: 16,
                src_y2: 32,
                palette_id: 1,
                blending: 1,
                type_trans,
                id: 9,
                ..TileSpec::default()
            };
            let layer1 = TileSpec { blending: 1, type_trans, ..over.clone() };
            let spec = FieldSpec {
                tiles: vec![layer1],
                layer2: Some(LayerSpec { width: 640, height: 480, tiles: vec![over] }),
                layer4: None,
                texture_slots: vec![(0, 1)],
                ..FieldSpec::default()
            };
            let tiles = decode_background_tiles_with_depth(&parse(&spec)).unwrap();
            assert_eq!(tiles.iter().find(|t| t.layer == 2).unwrap().blend, Some(mode), "{mode:?}");
            assert_eq!(tiles.iter().find(|t| t.layer == 1).unwrap().blend, None, "layer 1 ignores the field");
        }
    }

    #[test]
    fn unrenderable_and_transparent_tiles_are_left_out_and_damage_is_an_error() {
        // index 0 of palette page 1 is transparent: a source rectangle of only index-0 pixels draws
        // nothing (page offset 0 holds index 0, but its neighbours are not 0, so use the corner of a
        // tile whose texels are all index 0 by pointing at a page of zeros)
        let bad_palette = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 9, ..TileSpec::default() };
        let missing_slot = TileSpec { dst_x: 16, dst_y: 0, texture_id: 9, ..TileSpec::default() };
        let ok = TileSpec { dst_x: 48, dst_y: 0, src_x: 16, src_y: 32, ..TileSpec::default() };
        let spec = FieldSpec {
            tiles: vec![bad_palette, missing_slot, ok],
            layer2: None,
            layer4: None,
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        };
        let tiles = decode_background_tiles_with_depth(&parse(&spec)).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].rect.min_x, 48);
        let broken = FieldSpec { corrupt_texture_tag: true, ..FieldSpec::default() };
        assert!(matches!(decode_background_tiles_with_depth(&parse(&broken)), Err(FieldError::Invalid { .. })));
        let empty = FieldSpec { tiles: vec![], layer2: None, layer4: None, texture_slots: vec![], ..FieldSpec::default() };
        assert!(decode_background_tiles_with_depth(&parse(&empty)).unwrap().is_empty());
    }

    #[test]
    fn the_front_layers_depth_matches_the_split_images_coverage() {
        // every pixel the front half draws is covered by a layer-2 or layer-4 depth tile and no
        // other tile draws there (so a client can replace the flat front image by these tiles)
        let f = parse(&four_layer_spec());
        let split = decode_background_layers(&f).unwrap();
        let tiles = decode_background_tiles_with_depth(&f).unwrap();
        let mut covered = vec![false; split.front.rgba.len() / 4];
        for t in tiles.iter().filter(|t| t.layer == 2 || t.layer == 4) {
            for y in 0..t.size() {
                for x in 0..t.size() {
                    if t.pixel(x, y).is_some_and(|p| p[3] != 0) {
                        let (cx, cy) = (t.rect.min_x + x as i32 + split.front.origin_x, t.rect.min_y + y as i32 + split.front.origin_y);
                        covered[cy as usize * split.front.width as usize + cx as usize] = true;
                    }
                }
            }
        }
        for (i, px) in split.front.rgba.chunks_exact(4).enumerate() {
            assert_eq!(px[3] != 0, covered[i], "pixel {i}");
        }
    }

    #[test]
    fn the_atlas_holds_every_tile_without_overlap() {
        let f = parse(&four_layer_spec());
        let tiles = decode_background_tiles_with_depth(&f).unwrap();
        let atlas = pack_atlas(&tiles, 64).unwrap();
        assert_eq!(atlas.rects.len(), tiles.len());
        assert!(atlas.width <= 64 && atlas.width >= 32);
        assert_eq!(atlas.rgba.len(), (atlas.width * atlas.height * 4) as usize);
        // 32-pixel tiles come first, then 16-pixel ones; no two overlap and every pixel matches
        let mut owner = vec![usize::MAX; (atlas.width * atlas.height) as usize];
        for (i, (t, r)) in tiles.iter().zip(&atlas.rects).enumerate() {
            assert_eq!(r.size, t.size());
            assert!(r.x + r.size <= atlas.width && r.y + r.size <= atlas.height);
            for y in 0..r.size {
                for x in 0..r.size {
                    let at = ((r.y + y) * atlas.width + r.x + x) as usize;
                    assert_eq!(owner[at], usize::MAX, "tile {i} overlaps tile {}", owner[at]);
                    owner[at] = i;
                    assert_eq!(atlas.rgba[at * 4..at * 4 + 4], t.pixel(x, y).unwrap());
                }
            }
        }
        let first_16 = tiles.iter().position(|t| t.size() == 16).unwrap();
        let last_32_y = tiles.iter().zip(&atlas.rects).filter(|(t, _)| t.size() == 32).map(|(_, r)| r.y).max().unwrap();
        assert!(atlas.rects[first_16].y > last_32_y, "16-pixel tiles go below the 32-pixel ones");
        // out-of-range widths and an empty slice
        assert!(pack_atlas(&tiles, 31).is_none() && pack_atlas(&tiles, MAX_ATLAS_WIDTH + 1).is_none());
        let none = pack_atlas(&[], 64).unwrap();
        assert_eq!((none.width, none.height, none.rgba.len(), none.rects.len()), (0, 0, 0, 0));
        // two 32-pixel tiles side by side, then four 16-pixel ones in one row: 64 x 48; one column of
        // 32-pixel width is two 32-pixel rows and two rows of two 16-pixel tiles: 32 x 96
        assert_eq!((atlas.width, atlas.height), (64, 48));
        let narrow = pack_atlas(&tiles, 32).unwrap();
        assert_eq!((narrow.width, narrow.height), (32, 96));
        assert_eq!(pack_atlas(&tiles, 256).unwrap().width, 64, "the image is only as wide as its widest row");
        // a malformed tile is refused instead of panicking
        let mut bad = tiles.clone();
        bad[0].rgba.pop();
        assert!(pack_atlas(&bad, 64).is_none());
        let mut odd = tiles.clone();
        odd[1].rect.max_x += 1;
        assert!(pack_atlas(&odd, 64).is_none());
    }

    #[test]
    fn tall_atlases_are_refused() {
        // 32-wide atlas of many 16-pixel tiles: 2 per row; enough tiles exceed the height cap
        let tile = DepthTile {
            layer: 2,
            index: 0,
            id: 1,
            rect: ScreenRect { min_x: 0, min_y: 0, max_x: 16, max_y: 16 },
            depth: 4.0,
            blend: None,
            param: 0,
            state: 0,
            rgba: vec![255; 16 * 16 * 4],
        };
        let many = vec![tile.clone(); 2 * (MAX_ATLAS_HEIGHT as usize / 16) + 2];
        assert!(pack_atlas(&many, 32).is_none());
        let fits = vec![tile; 2 * (MAX_ATLAS_HEIGHT as usize / 16)];
        let atlas = pack_atlas(&fits, 32).unwrap();
        assert_eq!((atlas.width, atlas.height), (32, MAX_ATLAS_HEIGHT));
    }

    #[test]
    fn decoding_damaged_fields_never_panics() {
        let good = FieldSpec { texture_slots: vec![(0, 1)], ..FieldSpec::default() }.build();
        let exercise = |bytes: Vec<u8>| {
            if let Ok(f) = FieldFile::parse(bytes) {
                if let Ok(tiles) = decode_background_tiles_with_depth(&f) {
                    let _ = pack_atlas(&tiles, 128);
                }
                let _ = decode_background_tiles_with_depth_options(
                    &f,
                    &BackgroundOptions { groups: GroupFilter::All, layers: [true; 4], blending: true },
                );
            }
        };
        let structured = good.len().min(2600);
        let mut lens: Vec<usize> = (0..structured).collect();
        lens.extend((structured..good.len()).step_by(4099));
        lens.push(good.len() - 1);
        for n in lens {
            exercise(good[..n].to_vec());
        }
        for i in 0..structured {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                exercise(b);
            }
        }
    }
}
