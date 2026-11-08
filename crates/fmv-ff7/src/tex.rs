//! `.tex`: the PC texture format of field character models, decoded to RGBA.
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 695 `.tex`
//! entries of `field/char.lgp` (the ignored test `char_models_all_parse` in `tests/real_data.rs`
//! decodes them all and asserts the sizes; the header statistics below come from a survey of the
//! same files). Measured facts (counts only):
//!
//! * Every file has version 1, one palette, an 8-bit index per pixel (`bytes per pixel` 1, pitch
//!   field 0) and no colour-key array. 611 have a 16-colour palette (stored one byte per pixel
//!   all the same) and 84 a 256-colour one; 603 are 32x32 and the rest range over twelve other
//!   sizes from 64x16 to 256x256.
//! * The file length is exactly `0xEC + 4 * palette_size + width * height` for 694 files and 7
//!   bytes longer for one (trailing bytes are ignored).
//! * Palette entries are 4 bytes in the order blue, green, red, alpha. Alpha is 255 (28 209
//!   entries), 0 (2 213) or 254 (858; the "reference alpha" substitution, whose reference value is
//!   255 in every file, so 254 means opaque). The colour-key flag (header `0x08`) is set in 627
//!   files, and in exactly those files palette entry 0 is `(0, 0, 0)` with alpha 0; the other 68
//!   files have an opaque entry 0. Entry 0 is therefore transparent exactly when keying is on.
//!
//! # Sources
//!
//! * FFRTT "FF7/TEX format" <https://wiki.ffrtt.ru/index.php/FF7/TEX_format>: 0xEC-byte header of
//!   59 `u32`, palette at 0xEC, pixel data after it, colour-key array last; the field offsets used
//!   here (width 0x3C, height 0x40, palette flag 0x4C, palette entries 0x58, pixel bytes 0x68,
//!   colour-key flag 0x08, key-array flag 0xBC, reference alpha 0xC4, pixel-format block 0x6C to
//!   0xB8). Direct-colour decoding follows the page's bit-count / mask / shift description and
//!   **no owned file uses it, so it is unverified**.
//!
//! # Layout (all integers little-endian)
//!
//! ```text
//! 0x00 header, 59 x u32 (0xEC bytes): version(1) .. colour_key_flag(0x08) .. width(0x3C)
//!      height(0x40) .. has_palette(0x4C) .. palette_entries(0x58) .. bytes_per_pixel(0x68)
//!      pixel format block 0x6C..0xB8 (bit counts, masks, shifts; zero when paletted)
//!      has_key_array(0xBC) reference_alpha(0xC4) .. active_palette(0xD0, run-time)
//! 0xEC palette: palette_entries x 4 bytes b, g, r, a            (only when has_palette)
//!      pixels:  width x height x bytes_per_pixel                 (an index, or a packed colour)
//!      colour-key array: one byte per palette                    (only when has_key_array)
//! ```
//!
//! # Safety against untrusted input
//!
//! The size of the output is limited ([`MAX_TEX_SIDE`], [`MAX_TEX_PIXELS`]) and the byte count
//! the file must hold is computed with checked arithmetic before anything is allocated.

use crate::model::ModelError;
use crate::util::{slice_at, u32_at};

/// Bytes of the fixed header.
pub const TEX_HEADER_LEN: usize = 0xEC;
/// Widest or tallest texture accepted (the owned ones are at most 256).
pub const MAX_TEX_SIDE: u32 = 2048;
/// Most pixels accepted (the real ones have at most 65 536).
pub const MAX_TEX_PIXELS: u64 = 4 * 1024 * 1024;
/// Most palette entries accepted.
pub const MAX_TEX_PALETTE: u32 = 65_536;

/// A decoded RGBA8 image: `rgba` is `width * height * 4` bytes, row-major, top row first,
/// straight (non-premultiplied) alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `Debug` prints the size only, never pixels.
impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RgbaImage").field("width", &self.width).field("height", &self.height).finish_non_exhaustive()
    }
}

impl RgbaImage {
    /// RGBA of pixel `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?;
        let at = index.checked_mul(4)?;
        self.rgba.get(at..at.checked_add(4)?)?.try_into().ok()
    }

    /// Nearest-texel sample at texture coordinates `(u, v)` (0..1 across the image, wrapped).
    /// Non-finite coordinates sample the first texel.
    pub fn sample(&self, u: f32, v: f32) -> [u8; 4] {
        let wrap = |t: f32, n: u32| -> u32 {
            if !t.is_finite() || n == 0 {
                return 0;
            }
            let x = (t - t.floor()) * n as f32;
            (x as u32).min(n - 1)
        };
        self.pixel(wrap(u, self.width), wrap(v, self.height)).unwrap_or([0, 0, 0, 0])
    }
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "tex", why: why.into() }
}

/// Decode a `.tex` file. Alpha follows the colour-key flag and the palette's own alpha (see the
/// module docs).
pub fn parse_tex(bytes: &[u8]) -> Result<RgbaImage, ModelError> {
    if bytes.len() < TEX_HEADER_LEN {
        return Err(ModelError::Truncated { what: "tex header", at: 0, needed: TEX_HEADER_LEN - bytes.len() });
    }
    let word = |off: usize| u32_at(bytes, off).ok_or(ModelError::Truncated { what: "tex header", at: off, needed: 4 });
    let version = word(0x00)?;
    if version != 1 {
        return Err(invalid(format!("version {version} (expected 1)")));
    }
    let key_flag = word(0x08)? != 0;
    let (width, height) = (word(0x3C)?, word(0x40)?);
    let has_palette = word(0x4C)? != 0;
    let palette_entries = word(0x58)?;
    let bytes_per_pixel = word(0x68)?;
    let has_key_array = word(0xBC)? != 0;
    let reference_alpha = u8::try_from(word(0xC4)?.min(255)).unwrap_or(255);

    if width == 0 || height == 0 || width > MAX_TEX_SIDE || height > MAX_TEX_SIDE {
        return Err(invalid(format!("size {width}x{height} is outside 1..={MAX_TEX_SIDE}")));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_TEX_PIXELS {
        return Err(ModelError::TooMany { what: "tex pixels", count: pixels, limit: MAX_TEX_PIXELS });
    }
    if !(1..=4).contains(&bytes_per_pixel) {
        return Err(invalid(format!("{bytes_per_pixel} bytes per pixel (expected 1 to 4)")));
    }
    if has_palette && bytes_per_pixel != 1 {
        return Err(invalid(format!("paletted textures must use 1 byte per pixel, not {bytes_per_pixel}")));
    }
    if palette_entries > MAX_TEX_PALETTE {
        return Err(ModelError::TooMany {
            what: "tex palette entries",
            count: u64::from(palette_entries),
            limit: u64::from(MAX_TEX_PALETTE),
        });
    }
    let palette_len = if has_palette { u64::from(palette_entries) * 4 } else { 0 };
    let pixel_len = pixels * u64::from(bytes_per_pixel);
    let needed = (TEX_HEADER_LEN as u64) + palette_len + pixel_len;
    if needed > bytes.len() as u64 {
        return Err(ModelError::Truncated {
            what: "tex pixels",
            at: bytes.len(),
            needed: usize::try_from(needed - bytes.len() as u64).unwrap_or(usize::MAX),
        });
    }
    // needed <= bytes.len(), so every conversion below fits in usize.
    let palette_at = TEX_HEADER_LEN;
    let pixels_at = palette_at + usize::try_from(palette_len).unwrap_or(0);
    let pixel_count = usize::try_from(pixels).map_err(|_| invalid("too many pixels"))?;
    let bpp = bytes_per_pixel as usize;
    let data = slice_at(bytes, pixels_at, pixel_count * bpp).ok_or(ModelError::Truncated {
        what: "tex pixels",
        at: pixels_at,
        needed: pixel_count * bpp,
    })?;
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    if has_palette {
        let palette_bytes = slice_at(bytes, palette_at, usize::try_from(palette_len).unwrap_or(0))
            .ok_or(ModelError::Truncated { what: "tex palette", at: palette_at, needed: 0 })?;
        // Colour keying: header flag, overridden by the key array's byte for the active palette
        // when there is one (always palette 0 here: one palette, run-time index zero).
        let keyed = key_flag && !key_array_disables_keying(bytes, has_key_array, pixels_at, pixel_count);
        // (`as_chunks` would satisfy the lint but needs Rust 1.88; see `lgp` for why the MSRV stays put)
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let palette: Vec<[u8; 4]> = palette_bytes
            .chunks_exact(4)
            .map(|c| match c {
                [b, g, r, a] => [*r, *g, *b, if *a == 0xFE { reference_alpha } else { *a }],
                _ => [0, 0, 0, 0],
            })
            .collect();
        for &index in data {
            let mut px = palette.get(usize::from(index)).copied().unwrap_or([0, 0, 0, 0]);
            if keyed && index == 0 {
                px[3] = 0;
            }
            rgba.extend_from_slice(&px);
        }
    } else {
        // Direct colour: pack of `bpp` little-endian bytes decoded through the pixel-format block.
        let channel = |mask_at: usize, shift_at: usize| -> Result<(u32, u32), ModelError> {
            Ok((word(mask_at)?, word(shift_at)?))
        };
        let (red, green, blue, alpha) =
            (channel(0x7C, 0x8C)?, channel(0x80, 0x90)?, channel(0x84, 0x94)?, channel(0x88, 0x98)?);
        let extract = |pixel: u32, (mask, shift): (u32, u32)| -> Option<u8> {
            if mask == 0 || shift >= 32 {
                return None;
            }
            let max = mask >> shift;
            if max == 0 {
                return None;
            }
            let value = (pixel & mask) >> shift;
            u8::try_from(u64::from(value) * 255 / u64::from(max)).ok()
        };
        for chunk in data.chunks_exact(bpp) {
            let pixel = chunk.iter().rev().fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
            let (r, g, b) = (
                extract(pixel, red).unwrap_or(0),
                extract(pixel, green).unwrap_or(0),
                extract(pixel, blue).unwrap_or(0),
            );
            let mut a = extract(pixel, alpha).unwrap_or(255);
            if key_flag && pixel == 0 {
                a = 0;
            }
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(RgbaImage { width, height, rgba })
}

/// Whether a colour-key array exists AND switches keying off for palette 0 (its first byte is
/// zero). With no array the header flag alone decides. (Paletted textures have one byte per
/// pixel, so the array starts right after `pixel_count` bytes.)
fn key_array_disables_keying(bytes: &[u8], has_key_array: bool, pixels_at: usize, pixel_count: usize) -> bool {
    if !has_key_array {
        return false;
    }
    let array_at = pixels_at.saturating_add(pixel_count);
    matches!(bytes.get(array_at), Some(0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic TEX in the documented layout, written field by field.
    pub(crate) struct TexSpec {
        pub width: u32,
        pub height: u32,
        /// `(b, g, r, a)` palette entries; empty for a direct-colour texture.
        pub palette: Vec<[u8; 4]>,
        pub pixels: Vec<u8>,
        pub key_flag: bool,
        pub key_array: Option<Vec<u8>>,
        pub bytes_per_pixel: u32,
        /// (red, green, blue, alpha) masks for direct colour
        pub masks: [u32; 4],
        pub shifts: [u32; 4],
        pub trailing: usize,
    }

    impl Default for TexSpec {
        fn default() -> Self {
            Self {
                width: 2,
                height: 2,
                palette: vec![[0, 0, 0, 0], [30, 20, 10, 255], [3, 2, 1, 254], [9, 8, 7, 0]],
                pixels: vec![0, 1, 2, 3],
                key_flag: true,
                key_array: None,
                bytes_per_pixel: 1,
                masks: [0; 4],
                shifts: [0; 4],
                trailing: 0,
            }
        }
    }

    impl TexSpec {
        pub(crate) fn build(&self) -> Vec<u8> {
            let mut h = [0u32; 59];
            h[0] = 1;
            h[2] = u32::from(self.key_flag);
            h[12] = 1; // palettes
            h[13] = self.palette.len() as u32;
            h[15] = self.width;
            h[16] = self.height;
            h[19] = u32::from(!self.palette.is_empty());
            h[20] = if self.palette.is_empty() { 0 } else { 8 };
            h[22] = self.palette.len() as u32;
            h[26] = self.bytes_per_pixel;
            h[31..35].copy_from_slice(&self.masks); // 0x7C..: masks
            h[35..39].copy_from_slice(&self.shifts); // 0x8C..: shifts
            h[47] = u32::from(self.key_array.is_some());
            h[49] = 255; // reference alpha
            let mut out: Vec<u8> = h.iter().flat_map(|w| w.to_le_bytes()).collect();
            assert_eq!(out.len(), TEX_HEADER_LEN);
            for p in &self.palette {
                out.extend_from_slice(p);
            }
            out.extend_from_slice(&self.pixels);
            if let Some(k) = &self.key_array {
                out.extend_from_slice(k);
            }
            out.extend(std::iter::repeat_n(0x5Au8, self.trailing));
            out
        }
    }

    #[test]
    fn decodes_a_paletted_texture_with_alpha_rules() {
        let img = parse_tex(&TexSpec::default().build()).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.pixel(0, 0), Some([0, 0, 0, 0]), "entry 0 is transparent under the colour key");
        assert_eq!(img.pixel(1, 0), Some([10, 20, 30, 255]), "stored b, g, r becomes r, g, b");
        assert_eq!(img.pixel(0, 1), Some([1, 2, 3, 255]), "alpha 254 is the reference alpha (255)");
        assert_eq!(img.pixel(1, 1), Some([7, 8, 9, 0]), "an explicit alpha of 0 stays transparent");
        assert_eq!(img.pixel(2, 0), None);
        assert_eq!(img.pixel(0, 2), None);
        assert!(format!("{img:?}").contains("width"));
        // without the colour key an opaque entry 0 stays opaque
        let spec = TexSpec {
            key_flag: false,
            palette: vec![[5, 6, 7, 255], [30, 20, 10, 255], [3, 2, 1, 254], [9, 8, 7, 0]],
            ..TexSpec::default()
        };
        assert_eq!(parse_tex(&spec.build()).unwrap().pixel(0, 0), Some([7, 6, 5, 255]));
        // a key array whose first byte is zero switches keying off for the palette
        let spec = TexSpec {
            key_array: Some(vec![0]),
            palette: vec![[5, 6, 7, 255], [0; 4], [0; 4], [0; 4]],
            ..TexSpec::default()
        };
        assert_eq!(parse_tex(&spec.build()).unwrap().pixel(0, 0), Some([7, 6, 5, 255]));
        let spec = TexSpec {
            key_array: Some(vec![1]),
            palette: vec![[5, 6, 7, 255], [0; 4], [0; 4], [0; 4]],
            ..TexSpec::default()
        };
        assert_eq!(parse_tex(&spec.build()).unwrap().pixel(0, 0).map(|p| p[3]), Some(0));
        // out-of-palette indices are transparent black, trailing bytes are ignored
        let spec = TexSpec { pixels: vec![9, 1, 0, 255], trailing: 7, ..TexSpec::default() };
        let img = parse_tex(&spec.build()).unwrap();
        assert_eq!((img.pixel(0, 0), img.pixel(1, 1)), (Some([0, 0, 0, 0]), Some([0, 0, 0, 0])));
    }

    #[test]
    fn decodes_direct_colour_through_the_pixel_format_block() {
        // RGB565-like layout, 2 bytes per pixel, no alpha mask; the pixel 0 is keyed out
        let spec = TexSpec {
            palette: vec![],
            bytes_per_pixel: 2,
            masks: [0xF800, 0x07E0, 0x001F, 0],
            shifts: [11, 5, 0, 0],
            pixels: [0x0000u16, 0xF800, 0x07E0, 0x001F].iter().flat_map(|p| p.to_le_bytes()).collect(),
            ..TexSpec::default()
        };
        let img = parse_tex(&spec.build()).unwrap();
        assert_eq!(img.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(img.pixel(1, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.pixel(0, 1), Some([0, 255, 0, 255]));
        assert_eq!(img.pixel(1, 1), Some([0, 0, 255, 255]));
        // an alpha mask is honoured (ARGB8888)
        let spec = TexSpec {
            palette: vec![],
            bytes_per_pixel: 4,
            key_flag: false,
            masks: [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000],
            shifts: [16, 8, 0, 24],
            pixels: [0x80FF_0010u32, 0, 0, 0].iter().flat_map(|p| p.to_le_bytes()).collect(),
            ..TexSpec::default()
        };
        assert_eq!(parse_tex(&spec.build()).unwrap().pixel(0, 0), Some([255, 0, 16, 128]));
    }

    #[test]
    fn rejects_bad_headers_sizes_and_short_files() {
        let good = TexSpec::default().build();
        assert!(matches!(parse_tex(&[]), Err(ModelError::Truncated { .. })));
        assert!(matches!(parse_tex(good.get(..TEX_HEADER_LEN - 1).unwrap()), Err(ModelError::Truncated { .. })));
        assert!(matches!(parse_tex(good.get(..good.len() - 1).unwrap()), Err(ModelError::Truncated { .. })));
        let poke = |word: usize, value: u32| {
            let mut b = good.clone();
            b[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            parse_tex(&b)
        };
        assert!(matches!(poke(0, 2), Err(ModelError::Invalid { .. })), "version");
        assert!(matches!(poke(15, 0), Err(ModelError::Invalid { .. })), "zero width");
        assert!(matches!(poke(16, 0), Err(ModelError::Invalid { .. })), "zero height");
        assert!(matches!(poke(15, MAX_TEX_SIDE + 1), Err(ModelError::Invalid { .. })));
        assert!(matches!(poke(15, u32::MAX), Err(ModelError::Invalid { .. })));
        assert!(matches!(poke(26, 0), Err(ModelError::Invalid { .. })), "bytes per pixel");
        assert!(matches!(poke(26, 5), Err(ModelError::Invalid { .. })));
        assert!(matches!(poke(26, 2), Err(ModelError::Invalid { .. })), "paletted textures need 1 byte per pixel");
        assert!(matches!(poke(22, MAX_TEX_PALETTE + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(22, u32::MAX), Err(ModelError::TooMany { .. })));
        // pixel count over the cap
        let mut b = good.clone();
        b[15 * 4..15 * 4 + 4].copy_from_slice(&MAX_TEX_SIDE.to_le_bytes());
        b[16 * 4..16 * 4 + 4].copy_from_slice(&MAX_TEX_SIDE.to_le_bytes());
        b[26 * 4..26 * 4 + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse_tex(&b).is_err());
        // a count that fits the cap but not the file
        assert!(matches!(poke(22, 60_000), Err(ModelError::Truncated { .. })));
    }

    #[test]
    fn sampling_wraps_and_never_panics() {
        let img = parse_tex(&TexSpec::default().build()).unwrap();
        assert_eq!(img.sample(0.75, 0.0), img.pixel(1, 0).unwrap());
        assert_eq!(img.sample(1.75, 2.0), img.pixel(1, 0).unwrap(), "coordinates wrap");
        assert_eq!(img.sample(-0.25, 0.75), img.pixel(1, 1).unwrap());
        assert_eq!(img.sample(f32::NAN, f32::INFINITY), img.pixel(0, 0).unwrap());
        assert_eq!(img.sample(1.0, 1.0), img.pixel(0, 0).unwrap(), "exactly 1.0 wraps to 0");
        let empty = RgbaImage { width: 0, height: 0, rgba: vec![] };
        assert_eq!(empty.sample(0.5, 0.5), [0, 0, 0, 0]);
    }

    #[test]
    fn damaged_files_never_panic() {
        let good = TexSpec::default().build();
        for n in 0..good.len() {
            let _ = parse_tex(good.get(..n).unwrap_or(&[]));
        }
        for i in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                let _ = parse_tex(&b);
            }
        }
    }
}
