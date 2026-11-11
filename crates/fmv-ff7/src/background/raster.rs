//! Optional higher-resolution tile pixels over native background geometry.
//!
//! A replacement changes only the raster attached to a selected tile. Its logical rectangle,
//! camera depth, blend mode and animation selection remain those of [`DepthTile`]. In particular,
//! neither [`DepthTile::size`] nor [`super::BackgroundImage`] changes meaning. File formats,
//! replacement lookup and palette-animation fallback belong to the caller.
//!
//! Replacement alpha is clipped at 128, matching the depth renderer's half-alpha cutoff.
//! Packing and flat composition apply the same rule; source buffers are never modified.

use super::depth::DepthTile;
use super::{ScreenRect, MAX_CANVAS_SIDE};
use crate::field::FieldError;

/// Maximum byte allocation for a raster canvas, an atlas, or one set of replacement tiles.
pub const MAX_RASTER_BYTES: usize = 64 * 1024 * 1024;
/// Maximum raster canvas/atlas side. A device may impose a smaller texture limit.
pub const MAX_RASTER_SIDE: u32 = 8192;
/// Largest replacement tile (a native 32-unit tile at four times its raster resolution).
pub const MAX_RASTER_TILE_SIDE: u32 = 128;

/// Borrowed straight-alpha pixels, separate from the tile's logical geometry. The square
/// side is in texels: 16/32 for original pixels, up to 64/128 for the four-times trial.
#[derive(Clone, Copy)]
pub struct TilePixels<'a> {
    pub side: u32,
    pub rgba: &'a [u8],
}

impl std::fmt::Debug for TilePixels<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TilePixels").field("side", &self.side).field("bytes", &self.rgba.len()).finish()
    }
}

impl TilePixels<'_> {
    pub(crate) fn valid(&self) -> bool {
        matches!(self.side, 16 | 32 | 64 | 128)
            && usize::try_from(self.side).ok().and_then(|s| s.checked_mul(s)?.checked_mul(4)) == Some(self.rgba.len())
    }
}

/// A flat raster covering native field-screen bounds. Pixels are opaque over black. The
/// raster may have four times as many pixels per axis while covering exactly the same field.
#[derive(Clone, PartialEq, Eq)]
pub struct RasterBackground {
    pub logical_bounds: ScreenRect,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RasterBackground {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterBackground").field("logical_bounds", &self.logical_bounds)
            .field("width", &self.width).field("height", &self.height).finish_non_exhaustive()
    }
}

pub(crate) fn clipped_alpha(alpha: u8) -> u8 {
    if alpha < 128 { 0 } else { 255 }
}

fn invalid(why: &str) -> FieldError {
    FieldError::Invalid { what: "background raster", why: why.into() }
}

/// Composite already-selected native tiles in their supplied back-to-front order, using one
/// raster per tile. `scale` must be 1, 2 or 4, and every raster must be exactly the tile's
/// logical side times that scale. All validation precedes allocation or painting: malformed,
/// mixed-scale, out-of-canvas or over-budget inputs produce no partial image.
pub fn compose_tile_pixels(
    tiles: &[DepthTile],
    pixels: &[TilePixels<'_>],
    canvas: ScreenRect,
    scale: u32,
) -> Result<RasterBackground, FieldError> {
    if !matches!(scale, 1 | 2 | 4) || tiles.len() != pixels.len() {
        return Err(invalid("unsupported scale or mismatched tile/pixel count"));
    }
    let logical_width = i64::from(canvas.max_x) - i64::from(canvas.min_x);
    let logical_height = i64::from(canvas.max_y) - i64::from(canvas.min_y);
    if !(1..=i64::from(MAX_CANVAS_SIDE)).contains(&logical_width)
        || !(1..=i64::from(MAX_CANVAS_SIDE)).contains(&logical_height) {
        return Err(invalid("empty or oversized logical canvas"));
    }
    let width = u32::try_from(logical_width).ok().and_then(|w| w.checked_mul(scale)).ok_or_else(|| invalid("raster width overflow"))?;
    let height = u32::try_from(logical_height).ok().and_then(|h| h.checked_mul(scale)).ok_or_else(|| invalid("raster height overflow"))?;
    if width > MAX_RASTER_SIDE || height > MAX_RASTER_SIDE {
        return Err(invalid("raster side exceeds its limit"));
    }
    let bytes = u64::from(width).checked_mul(u64::from(height)).and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok()).filter(|n| *n <= MAX_RASTER_BYTES)
        .ok_or_else(|| invalid("raster canvas exceeds its byte budget"))?;
    let mut input_bytes = 0usize;
    for (tile, raster) in tiles.iter().zip(pixels) {
        let side = i64::from(tile.rect.max_x) - i64::from(tile.rect.min_x);
        let tall = i64::from(tile.rect.max_y) - i64::from(tile.rect.min_y);
        if !matches!(side, 16 | 32) || tall != side || !raster.valid()
            || u64::from(raster.side) != side as u64 * u64::from(scale) {
            return Err(invalid("tile geometry, scale or pixel length does not match"));
        }
        if tile.rect.min_x < canvas.min_x || tile.rect.min_y < canvas.min_y
            || tile.rect.max_x > canvas.max_x || tile.rect.max_y > canvas.max_y {
            return Err(invalid("tile lies outside the logical canvas"));
        }
        input_bytes = input_bytes.checked_add(raster.rgba.len()).filter(|n| *n <= MAX_RASTER_BYTES)
            .ok_or_else(|| invalid("tile rasters exceed their byte budget"))?;
    }
    let mut rgba = vec![0u8; bytes];
    for alpha in rgba.iter_mut().skip(3).step_by(4) { *alpha = 255; }
    // Every conversion and address below is bounded by the validated canvas and tile spans.
    let row_width = width as usize;
    for (tile, raster) in tiles.iter().zip(pixels) {
        let left = ((i64::from(tile.rect.min_x) - i64::from(canvas.min_x)) * i64::from(scale)) as usize;
        let top = ((i64::from(tile.rect.min_y) - i64::from(canvas.min_y)) * i64::from(scale)) as usize;
        let side = raster.side as usize;
        for (n, &[red, green, blue, alpha]) in raster.rgba.as_chunks::<4>().0.iter().enumerate() {
            if clipped_alpha(alpha) == 0 { continue; }
            let at = ((top + n / side) * row_width + left + n % side) * 4;
            let Some([dr, dg, db, _]) = rgba.get_mut(at..at + 4) else {
                return Err(invalid("tile pixel lies outside the raster canvas"));
            };
            for (destination, source) in [(dr, red), (dg, green), (db, blue)] {
                *destination = match tile.blend {
                    Some(mode) => mode.apply(*destination, source),
                    None => source,
                };
            }
        }
    }
    Ok(RasterBackground { logical_bounds: canvas, width, height, rgba })
}

#[cfg(test)]
mod tests;
