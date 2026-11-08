//! The 8x8 bitmap font and the texture atlas built from it.
//!
//! # Licence of the glyph data
//!
//! The glyph bitmaps for the printable ASCII range (U+0020 ..= U+007E) in [`BASIC`] are the
//! "font8x8" basic set (`font8x8_basic.h`, collected by Daniel Hepper from the IBM PC BIOS
//! font). That data is **public domain**: it is used here without restriction and without
//! attribution requirements; the notice is kept so nobody has to wonder. The extra glyphs in
//! [`SPECIAL`] (solid block, the "missing character" box and four arrows) are this project's own.
//!
//! # Format
//!
//! One glyph is eight bytes, one per pixel row from the top; **bit 0 of a byte is the leftmost
//! pixel**. Letters use at most seven columns, so the eighth column is the gap between
//! characters. Every character advances by exactly [`GLYPH_W`] pixels (monospace).
//!
//! # Atlas
//!
//! [`Atlas`] lays the glyphs out in a grid of 8x8 cells ([`ATLAS_COLS`] x [`ATLAS_ROWS`]) in an
//! RGBA image whose colour is white and whose alpha is the glyph's coverage (0 or 255). Cell
//! `k` holds ASCII `0x20 + k` for `k < 95`, then the [`Special`] glyphs. Text is drawn by
//! sampling the cell of each character with NEAREST filtering at an integer scale, so the pixels
//! stay square and crisp. [`cell_uv`] gives the texture coordinates of a cell; [`solid_uv`]
//! points inside the solid block, which is how plain rectangles are drawn with the same
//! pipeline.

/// Width and height of a glyph cell in texels.
pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 8;

/// The atlas is a grid of cells.
pub const ATLAS_COLS: u32 = 16;
pub const ATLAS_ROWS: u32 = 8;
pub const ATLAS_W: u32 = ATLAS_COLS * GLYPH_W;
pub const ATLAS_H: u32 = ATLAS_ROWS * GLYPH_H;

/// First and last character of [`BASIC`].
const FIRST: u32 = 0x20;
const LAST: u32 = 0x7E;
/// Number of glyphs in [`BASIC`].
const BASIC_COUNT: usize = (LAST - FIRST + 1) as usize;

/// A glyph this project draws itself (not part of the public-domain set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    /// Every pixel set: the texel source of every plain rectangle.
    Block,
    /// A hollow box, drawn for a character the font has no glyph for.
    Missing,
    ArrowRight,
    ArrowLeft,
    ArrowUp,
    ArrowDown,
}

/// The special glyphs in cell order (cell `BASIC_COUNT + i`).
pub const SPECIAL: [(Special, [u8; 8]); 6] = [
    (Special::Block, [0xFF; 8]),
    (Special::Missing, [0x00, 0x7E, 0x42, 0x42, 0x42, 0x42, 0x7E, 0x00]),
    (Special::ArrowRight, [0x01, 0x03, 0x07, 0x0F, 0x0F, 0x07, 0x03, 0x01]),
    (Special::ArrowLeft, [0x80, 0xC0, 0xE0, 0xF0, 0xF0, 0xE0, 0xC0, 0x80]),
    (Special::ArrowUp, [0x00, 0x18, 0x3C, 0x7E, 0xFF, 0x00, 0x00, 0x00]),
    (Special::ArrowDown, [0x00, 0x00, 0x00, 0xFF, 0x7E, 0x3C, 0x18, 0x00]),
];

/// font8x8 basic set, U+0020 ..= U+007E (public domain, see the module docs).
#[rustfmt::skip]
const BASIC: [[u8; 8]; BASIC_COUNT] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // U+0020 (space)
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00], // U+0021 (!)
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // U+0022 (")
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00], // U+0023 (#)
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00], // U+0024 ($)
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00], // U+0025 (%)
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00], // U+0026 (&)
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00], // U+0027 (')
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00], // U+0028 (()
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00], // U+0029 ())
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00], // U+002A (*)
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00], // U+002B (+)
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06], // U+002C (,)
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00], // U+002D (-)
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00], // U+002E (.)
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // U+002F (/)
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00], // U+0030 (0)
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00], // U+0031 (1)
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00], // U+0032 (2)
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00], // U+0033 (3)
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00], // U+0034 (4)
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00], // U+0035 (5)
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00], // U+0036 (6)
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00], // U+0037 (7)
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00], // U+0038 (8)
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00], // U+0039 (9)
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00], // U+003A (:)
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06], // U+003B (;)
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00], // U+003C (<)
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00], // U+003D (=)
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00], // U+003E (>)
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00], // U+003F (?)
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00], // U+0040 (@)
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00], // U+0041 (A)
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00], // U+0042 (B)
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00], // U+0043 (C)
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00], // U+0044 (D)
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00], // U+0045 (E)
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00], // U+0046 (F)
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00], // U+0047 (G)
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // U+0048 (H)
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // U+0049 (I)
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00], // U+004A (J)
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00], // U+004B (K)
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00], // U+004C (L)
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00], // U+004D (M)
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00], // U+004E (N)
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00], // U+004F (O)
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00], // U+0050 (P)
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00], // U+0051 (Q)
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00], // U+0052 (R)
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00], // U+0053 (S)
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // U+0054 (T)
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00], // U+0055 (U)
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // U+0056 (V)
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // U+0057 (W)
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00], // U+0058 (X)
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00], // U+0059 (Y)
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00], // U+005A (Z)
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00], // U+005B ([)
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00], // U+005C (\)
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00], // U+005D (])
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00], // U+005E (^)
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF], // U+005F (_)
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00], // U+0060 (`)
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00], // U+0061 (a)
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00], // U+0062 (b)
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00], // U+0063 (c)
    [0x38, 0x30, 0x30, 0x3E, 0x33, 0x33, 0x6E, 0x00], // U+0064 (d)
    [0x00, 0x00, 0x1E, 0x33, 0x3F, 0x03, 0x1E, 0x00], // U+0065 (e)
    [0x1C, 0x36, 0x06, 0x0F, 0x06, 0x06, 0x0F, 0x00], // U+0066 (f)
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F], // U+0067 (g)
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00], // U+0068 (h)
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // U+0069 (i)
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E], // U+006A (j)
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00], // U+006B (k)
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // U+006C (l)
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00], // U+006D (m)
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00], // U+006E (n)
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00], // U+006F (o)
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F], // U+0070 (p)
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78], // U+0071 (q)
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00], // U+0072 (r)
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00], // U+0073 (s)
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00], // U+0074 (t)
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00], // U+0075 (u)
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // U+0076 (v)
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00], // U+0077 (w)
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00], // U+0078 (x)
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F], // U+0079 (y)
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00], // U+007A (z)
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00], // U+007B ({)
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00], // U+007C (|)
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00], // U+007D (})
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // U+007E (~)
];

/// Can the font draw `c` with a real glyph? (Anything else is drawn as the "missing" box;
/// control characters are not drawn at all, see [`crate::ui::text`].)
pub fn is_drawable(c: char) -> bool {
    (FIRST..=LAST).contains(&u32::from(c))
}

/// The atlas cell (0-based, row-major) that holds the glyph for `c`: its own glyph for ASCII, the
/// "missing" box for everything else.
pub fn cell_of(c: char) -> u32 {
    if is_drawable(c) {
        u32::from(c) - FIRST
    } else {
        special_cell(Special::Missing)
    }
}

/// The atlas cell of a special glyph.
pub fn special_cell(s: Special) -> u32 {
    let index = SPECIAL.iter().position(|(kind, _)| *kind == s).unwrap_or(0);
    BASIC_COUNT as u32 + index as u32
}

/// The 8 row bytes of `c`'s glyph (the "missing" box for characters the font lacks).
#[cfg(test)]
pub fn glyph_rows(c: char) -> [u8; 8] {
    if is_drawable(c) {
        BASIC[(u32::from(c) - FIRST) as usize]
    } else {
        SPECIAL[1].1
    }
}

/// Texture coordinates `[u0, v0, u1, v1]` of atlas cell `cell` (`(0, 0)` is the top left).
pub fn cell_uv(cell: u32) -> [f32; 4] {
    let (cx, cy) = (cell % ATLAS_COLS, cell / ATLAS_COLS);
    let (w, h) = (ATLAS_W as f32, ATLAS_H as f32);
    [
        (cx * GLYPH_W) as f32 / w,
        (cy * GLYPH_H) as f32 / h,
        ((cx + 1) * GLYPH_W) as f32 / w,
        ((cy + 1) * GLYPH_H) as f32 / h,
    ]
}

/// A texture coordinate in the middle of the solid block, as `[u, v, u, v]` (a zero-size range, so
/// every pixel of a rectangle samples the same fully opaque texel).
pub fn solid_uv() -> [f32; 4] {
    let [u0, v0, u1, v1] = cell_uv(special_cell(Special::Block));
    let (u, v) = ((u0 + u1) * 0.5, (v0 + v1) * 0.5);
    [u, v, u, v]
}

/// The atlas image: RGBA8, white with the glyph coverage in alpha, `ATLAS_W` x `ATLAS_H`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Atlas {
    /// Rasterise every glyph into the grid.
    pub fn build() -> Self {
        // White and transparent everywhere (cells with no glyph included): the colour is always the
        // vertex colour, the alpha is the coverage.
        let mut rgba = [255_u8, 255, 255, 0].repeat((ATLAS_W * ATLAS_H) as usize);
        let mut put = |cell: u32, rows: &[u8; 8]| {
            let (cx, cy) = ((cell % ATLAS_COLS) * GLYPH_W, (cell / ATLAS_COLS) * GLYPH_H);
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..GLYPH_W {
                    let lit = (bits >> col) & 1 == 1;
                    let at = (((cy + row as u32) * ATLAS_W + cx + col) * 4) as usize;
                    // White everywhere so that the vertex colour is the only colour; the coverage
                    // is the alpha.
                    rgba[at..at + 4].copy_from_slice(&[255, 255, 255, if lit { 255 } else { 0 }]);
                }
            }
        };
        for (i, rows) in BASIC.iter().enumerate() {
            put(i as u32, rows);
        }
        for (kind, rows) in &SPECIAL {
            put(special_cell(*kind), rows);
        }
        Self { width: ATLAS_W, height: ATLAS_H, rgba }
    }

    /// Is the texel at (`x`, `y`) lit?
    #[cfg(test)]
    pub fn lit(&self, x: u32, y: u32) -> bool {
        self.rgba[((y * self.width + x) * 4 + 3) as usize] != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One glyph as rows of `#` and `.`, for readable assertions.
    fn art(rows: [u8; 8]) -> Vec<String> {
        rows.iter().map(|b| (0..8).map(|c| if (b >> c) & 1 == 1 { '#' } else { '.' }).collect()).collect()
    }

    #[test]
    fn the_table_covers_exactly_printable_ascii_and_the_cells_fit_the_atlas() {
        assert_eq!(BASIC.len(), 95);
        assert_eq!(FIRST, u32::from(' '));
        assert_eq!(LAST, u32::from('~'));
        assert!(BASIC_COUNT + SPECIAL.len() <= (ATLAS_COLS * ATLAS_ROWS) as usize);
        for c in ' '..='~' {
            assert!(is_drawable(c), "{c:?}");
        }
        for c in ['\u{1f}', '\u{7f}', '\u{e9}', '\u{4e2d}', '\u{1f600}'] {
            assert!(!is_drawable(c), "{c:?}");
            assert_eq!(cell_of(c), special_cell(Special::Missing), "{c:?} is drawn as the missing box");
        }
    }

    #[test]
    fn glyphs_keep_their_known_shapes() {
        // Bit 0 is the left pixel: a capital A has its apex at columns 2 and 3 and a full bar.
        assert_eq!(
            art(glyph_rows('A')),
            ["..##....", ".####...", "##..##..", "##..##..", "######..", "##..##..", "##..##..", "........"]
        );
        // A lower-case g has a descender in the last row, a comma hangs below the baseline.
        assert_eq!(art(glyph_rows('g'))[7], "#####...");
        assert_eq!(art(glyph_rows(','))[7], ".##.....");
        // The space is empty, the underscore is the bottom row only.
        assert!(art(glyph_rows(' ')).iter().all(|r| r == "........"));
        assert_eq!(art(glyph_rows('_'))[7], "########");
        assert!(art(glyph_rows('_'))[..7].iter().all(|r| r == "........"));
    }

    /// Structural checks that catch a mistyped byte in the table: every glyph keeps its last
    /// column free (the gap between characters; only the underscore and the asterisk run into it,
    /// as in the original), no glyph is blank except the space, and no two different characters
    /// share a bitmap.
    #[test]
    fn every_glyph_is_sane_and_distinct() {
        let mut seen = std::collections::HashMap::new();
        for c in ' '..='~' {
            let rows = glyph_rows(c);
            if !matches!(c, '_' | '*') {
                assert!(rows.iter().all(|b| b & 0x80 == 0), "{c:?} uses the gap column");
            }
            if c != ' ' {
                assert!(rows.iter().any(|b| *b != 0), "{c:?} is blank");
            }
            if let Some(prev) = seen.insert(rows, c) {
                panic!("{c:?} and {prev:?} have the same bitmap");
            }
        }
        // The widest letters use seven columns, narrow ones fewer: nothing is wider than that.
        for c in ['M', 'W', 'm', 'w', 'E', '#'] {
            let used = glyph_rows(c).iter().fold(0_u8, |acc, b| acc | b);
            assert!(used.leading_zeros() >= 1, "{c:?}");
        }
    }

    #[test]
    fn the_atlas_holds_every_glyph_where_the_cells_say() {
        let atlas = Atlas::build();
        assert_eq!((atlas.width, atlas.height), (ATLAS_W, ATLAS_H));
        assert_eq!(atlas.rgba.len(), (ATLAS_W * ATLAS_H * 4) as usize);
        let cell_pixels = |cell: u32| -> Vec<String> {
            let (ox, oy) = ((cell % ATLAS_COLS) * GLYPH_W, (cell / ATLAS_COLS) * GLYPH_H);
            (0..GLYPH_H)
                .map(|y| (0..GLYPH_W).map(|x| if atlas.lit(ox + x, oy + y) { '#' } else { '.' }).collect())
                .collect()
        };
        for c in ' '..='~' {
            assert_eq!(cell_pixels(cell_of(c)), art(glyph_rows(c)), "{c:?}");
        }
        for (kind, rows) in SPECIAL {
            assert_eq!(cell_pixels(special_cell(kind)), art(rows), "{kind:?}");
        }
        // White colour everywhere, so a vertex colour is never tinted by the atlas.
        assert!(atlas.rgba.chunks(4).all(|p| p[..3] == [255, 255, 255]));
        // Cells past the last glyph stay empty.
        assert!(cell_pixels(127).iter().all(|r| r == "........"));
    }

    #[test]
    fn cell_coordinates_tile_the_atlas_exactly_and_the_solid_point_is_inside_the_block() {
        // 16 columns of 8 texels in 128, 8 rows of 8 texels in 64: all exact binary fractions.
        assert_eq!(cell_uv(0), [0.0, 0.0, 0.0625, 0.125]);
        assert_eq!(cell_uv(1), [0.0625, 0.0, 0.125, 0.125]);
        assert_eq!(cell_uv(16), [0.0, 0.125, 0.0625, 0.25]);
        let [u0, v0, u1, v1] = cell_uv(17);
        assert!((u0 - 1.0 / 16.0).abs() < 1e-6 && (v0 - 1.0 / 8.0).abs() < 1e-6, "{u0} {v0}");
        assert!((u1 - 2.0 / 16.0).abs() < 1e-6 && (v1 - 2.0 / 8.0).abs() < 1e-6);
        let atlas = Atlas::build();
        let [u, v, u2, v2] = solid_uv();
        assert_eq!((u, v), (u2, v2));
        let (x, y) = ((u * ATLAS_W as f32) as u32, (v * ATLAS_H as f32) as u32);
        assert!(atlas.lit(x, y), "the solid point samples an opaque texel");
        // ...and so do its neighbours in the block, so a rounding error cannot hit a hole.
        for (dx, dy) in [(-3_i32, -3_i32), (3, 3), (-3, 3), (3, -3)] {
            assert!(atlas.lit((x as i32 + dx) as u32, (y as i32 + dy) as u32));
        }
    }
}
