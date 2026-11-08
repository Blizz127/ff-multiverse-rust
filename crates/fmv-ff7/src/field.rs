//! PC field file (the decompressed contents of one `flevel.lgp` entry).
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08** (the owner's `flevel.lgp`, 753
//! entries, 702 of them field files); other editions are unverified. Measured facts (counts and
//! sizes only, no game bytes):
//!
//! * Container: all 702 field entries parse. The nine section lengths plus the 78 header bytes
//!   equal the decompressed size exactly in 120 fields; the other 582 carry the 14-byte ASCII
//!   string `FINAL FANTASY7` after the last section (the same string that ends the archive),
//!   which [`FieldFile`] simply ignores. `md1stin`: 414 915 bytes, sections 18 944 / 38 / 1 314 /
//!   6 668 / 1 174 / 42 / 48 / 740 / 385 869.
//! * Walkmesh: 81 542 triangles over the 702 fields; `md1stin` has 39 (x 3490..4055, y
//!   27244..29405, z 310..432). All 181 202 access links point at a valid triangle, none points
//!   at its own triangle, **every link shares the two edge vertices of the edge it is stored
//!   under** (so "edge `e` runs `v[e]` -> `v[e+1]`" is exact), and 181 200 of them have a link
//!   back (two do not).
//! * Camera: one 38-byte record in 487 fields, two in 214, three in 1. In all 214 two-camera
//!   fields the second record is a byte-for-byte copy of the first; only `mds5_1` (three
//!   records) has differing ones. Use record 0. Projection model: see [`crate::background`].
//! * Palette: 256 colours per page, 13 pages in `md1stin`. 14 fields (direct-colour backgrounds
//!   such as `gldinfo`) have a 12-byte palette section with zero pages. Palette index 0 is the
//!   transparent colour whatever its RGB value (chosen by comparing renderings; see
//!   [`Palette::rgba`]).
//! * Triggers: 740 bytes everywhere; the 9-byte name equals the entry name in 687 of 702 fields
//!   (the other 15, such as `qa` or `min71`, carry a different internal name).
//!   `control_direction` is an unsigned angle (572 fields store
//!   a value above 127). `camera_range` is the extent of the background in field-screen
//!   coordinates (equal to the layer-1 tile bounds in 399 of 699 fields; in the others layer 1
//!   covers less than the range). Gateways: 1 036 in use (target `0x7FFF` marks an unused slot,
//!   whose bytes are all zero); `target_field_id` indexes the 788-entry `maplist` entry of
//!   `flevel.lgp` (`md1stin` -> 117 -> `md1_1`), and for all 978 gateways whose target is in
//!   the archive the arrival `(x, y)` lies inside the target walkmesh triangle named by the
//!   gateway's third destination value. 1 030 of 1 036 exit lines lie within 64 units of their
//!   own walkmesh's bounding box; of 119 trigger slots in use, 118 do.
//! * Background: layer presence (layers 1-4) is 1100 in 618 fields, 1110 in 34, 1111 in 50.
//!   `md1stin`: layer 1 528x528 header with 795 tiles, layer 2 with 320, five 8-bit pages (slots
//!   0-4), 13 palette pages, 1 115 tiles in total. The texture block ends at the section end
//!   in every field except for a 3-byte `END` tag (578 fields end right after it; 121 have the
//!   14 bytes `FINAL FANTASY7` after it inside the declared length; `blackbgb`, `las2_1` and
//!   `fship_4` have 131, 17 and 19 further bytes). Page depths seen: 8-bit (3 276 pages, slots
//!   0-25) and 16-bit direct colour (51 pages, slots 26-28); depth 0 never occurs.
//!   Layer 1-2 tiles always sit on pages whose big-tile flag is 0 and layer 3-4 tiles on pages
//!   whose flag is 1 (so tile size 16 / 32 by layer is exact); `texture_id` is the slot number.
//!   The destination rule of [`MAX_TILE_DST`] drops 650 records in 7 fields, all unusable
//!   (for example 640 odd-indexed layer-2 records of `md_e1`); no usable tile is ever dropped.
//!   312 tiles in two fields (`lastmap`, `fr_e`) name a palette page past the end of the
//!   palette section and cannot be decoded.
//!
//! # Sources
//!
//! * Container, section order: FFRTT wiki "FF7/Field Module"
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field_Module> and PyFF7 `PyFF7/field.py`
//!   <https://github.com/niemasd/PyFF7/blob/master/PyFF7/field.py> (they agree).
//! * Walkmesh: FFRTT "FF7/Field/Walkmesh" <https://wiki.ffrtt.ru/index.php/FF7/Field/Walkmesh>
//!   and PyFF7 (agree).
//! * Camera: FFRTT "FF7/Field/Camera Matrix"
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Camera_Matrix> and PyFF7 (agree on layout;
//!   the wiki itself says the vector order, centre and zoom are not fully understood).
//! * Palette: FFRTT "FF7/Field/Palette" <https://wiki.ffrtt.ru/index.php/FF7/Field/Palette>
//!   and PyFF7 (agree).
//! * Triggers / gateways: FFRTT "FF7/Field/Triggers"
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Triggers> and PyFF7 (agree on sizes and
//!   offsets; they **disagree on the naming/order of the three vertex components**, see
//!   below).
//! * Background: FFRTT "FF7/Field/Background"
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Background> (the older Fergusson write-up:
//!   52-byte tile records, 256x256 pages with a 6-byte header, BGR555 colours), PyFF7
//!   `field.py` (full section 9 layout: header tags, four layers, 52-byte tiles, 42 texture
//!   slots) and the PC tile reader in Makou Reactor's
//!   `src/core/field/BackgroundTilesIO.cpp` <https://github.com/myst6re/makoureactor>
//!   (layer framing, tile size 16/32, and the rule for choosing the second source set).
//!   Those three agree wherever they overlap (tile offsets 0x02/0x04 destination, 0x0A/0x0C
//!   source, 0x16 palette, 0x20 texture, layer-2 count at +7 after layer 1's tiles, ...).
//!   Makou Reactor's `src/core/field/BackgroundTexturesIO.cpp` and `BackgroundTextures.cpp`
//!   additionally show that texture pages are keyed by their **slot number** (the loop
//!   counter over the texture list, advancing on empty slots) and that a tile's `texture_id`
//!   is that key, that a page's size is `depth * 65536` bytes (depth 0 = 32768), that the
//!   first per-page `u16` is an "is big tile" flag, and that PC palette transparency is
//!   limited to palette index 0.
//!   Makou Reactor's `src/core/field/BackgroundTilesIO.h` and `BackgroundTilesIO.cpp` also
//!   define the destination sanity rule applied to every PC tile (see [`MAX_TILE_DST`]).
//!   Only layout *facts* were taken from the community sources; no code was copied.
//!
//! # Container
//!
//! ```text
//! u16  0 (blank)
//! u32  section count, expected 9
//! u32  x9 absolute offset of each section
//! each section:  u32 payload length, then the payload
//! ```
//!
//! Section order (index: name): 0 script, 1 camera, 2 model_loader, 3 palette, 4 walkmesh,
//! 5 tilemap (unused on PC), 6 encounter, 7 triggers, 8 background. The community pages
//! number sections from 1 (walkmesh is their "section 5"); this crate uses 0-based indices
//! everywhere, including in error messages.
//!
//! [`FieldFile::parse`] fails only on header problems (blank, count != 9, offsets out of
//! range or not increasing). A section whose declared length disagrees with its span is
//! reported when that section is *accessed*, so a damaged background never stops a server
//! from hosting the walkmesh.
//!
//! # What is parsed
//!
//! | Section | Parsed into | Notes |
//! |---|---|---|
//! | walkmesh | [`Walkmesh`] | triangles `(x, y, z)` as `i16` + 3 edge-access indices each |
//! | camera | [`Camera`] | 38-byte records: 3x3 `i16` axes (fixed point /4096), `i32` position, zoom |
//! | palette | [`Palette`] | header + 256-colour pages of 15-bit BGR colours |
//! | triggers | [`Triggers`] | name, camera range, 12 gateways, 12 triggers (arrows not parsed) |
//! | background | [`Background`] | header tags, layers 1-4 tile records ([`TileSprite`]), texture page locations |
//!
//! Script, model loader, tilemap and encounter sections are exposed only as raw payload
//! bytes via [`FieldFile::section`].
//!
//! # Known gaps and unverified points
//!
//! * Section 1 camera: only record 0 is interpreted (see above). The wiki still says the vector
//!   order and zoom are not fully understood; [`crate::background`] documents the projection
//!   that was checked against the real art.
//! * Trigger corners and gateway exit points are `(x, y, z)` in walkmesh order (verified, see
//!   above); a gateway's `destination` is `(x, y, triangle id)` in the **target** field's
//!   coordinates. Arrow data after the 12 triggers, `bg_animation_sizes` and the section 3/6/7
//!   internals are not decoded. The script, model-loader, tilemap and encounter sections are
//!   raw bytes.
//! * Background `blending` / `type_trans` / `param` / `state` / `id` are exposed as stored. How
//!   they are *used* is in [`crate::background`]: blend modes and draw order were checked by eye
//!   on real fields; which animation groups are on at field start is script state that this
//!   crate does not evaluate; how a layer-2 `id` maps to character depth is unverified.
//! * Depth-0 (4-bit) texture pages never occur in the owned data; their decode (low nibble =
//!   first pixel) rests on Makou Reactor's description only.
//! * Colour: palette entries and 16-bit direct-colour pixels use the same layout (red in the
//!   low five bits, verified by eye on the Gold Saucer page of `gldinfo`; blue-in-the-low-bits
//!   readings give blue "gold" text). Bit 15 is not interpreted. For direct colour a raw
//!   `0x0000` is treated as transparent (Makou Reactor's rule; no owned field tests it).
//! * Tile destinations are signed, top-left-corner, relative to the centre of the unscrolled
//!   320x240 screen, in the same space as the camera projection ([`crate::background`]).
//!   Records at or beyond `+-`[`MAX_TILE_DST`] (1024) on either axis are dropped while parsing
//!   on all four layers, as Makou Reactor's PC reader does; they are counted in
//!   [`Background::discarded_tiles`] and the surviving tiles keep their file `index`.
//!
//! # Safety against untrusted input
//!
//! No indexing or arithmetic on untrusted values without bounds/overflow checks; counts are
//! capped and checked against the bytes actually present before any allocation; parse
//! failures are errors, never panics.

use crate::util::{nul_padded_name, slice_at, u32_at};
use std::fmt;
use thiserror::Error;

/// Sections in a PC field file.
pub const SECTION_COUNT: usize = 9;
/// Bytes of container header: `u16` blank, `u32` count, nine `u32` offsets.
pub const HEADER_LEN: usize = 2 + 4 + 4 * SECTION_COUNT;
/// Section names in file order.
pub const SECTION_NAMES: [&str; SECTION_COUNT] = [
    "script",
    "camera",
    "model_loader",
    "palette",
    "walkmesh",
    "tilemap",
    "encounter",
    "triggers",
    "background",
];

/// Section indices.
pub mod section {
    pub const SCRIPT: usize = 0;
    pub const CAMERA: usize = 1;
    pub const MODEL_LOADER: usize = 2;
    pub const PALETTE: usize = 3;
    pub const WALKMESH: usize = 4;
    pub const TILEMAP: usize = 5;
    pub const ENCOUNTER: usize = 6;
    pub const TRIGGERS: usize = 7;
    pub const BACKGROUND: usize = 8;
}

/// Cap on walkmesh triangles. Access indices are `u16` with `0xFFFF` reserved, so no valid
/// mesh can have more.
pub const MAX_TRIANGLES: u32 = 65_535;
/// Access value meaning "this edge cannot be crossed".
pub const NO_ACCESS: u16 = 0xFFFF;
/// Bytes per camera record.
pub const CAMERA_RECORD_LEN: usize = 38;
/// Cap on camera records per field.
pub const MAX_CAMERAS: usize = 256;
/// Fixed-point scale of camera axis vectors.
pub const CAMERA_FIXED_POINT: f32 = 4096.0;
/// Bytes before the colour data in the palette section.
pub const PALETTE_HEADER_LEN: usize = 12;
/// Gateways and triggers per field (fixed-size tables).
pub const GATEWAY_SLOTS: usize = 12;
pub const TRIGGER_SLOTS: usize = 12;
/// Minimum triggers-section payload: 56-byte header + 12 x 24 gateways + 12 x 16 triggers.
/// (Non-Japanese releases append arrow data, for 740 bytes in total; not parsed.)
pub const TRIGGERS_MIN_LEN: usize = 56 + GATEWAY_SLOTS * 24 + TRIGGER_SLOTS * 16;
/// Bytes per background tile record.
pub const BG_TILE_RECORD_LEN: usize = 52;
/// Texture slots in the background section.
pub const BG_TEXTURE_SLOTS: usize = 42;
/// A tile is kept only if `|dst_x| < MAX_TILE_DST` and `|dst_y| < MAX_TILE_DST`. This is the
/// rule (and the value, 1024) of Makou Reactor's PC background reader, which discards the
/// other records as overflowed or unused; see the module docs.
pub const MAX_TILE_DST: u16 = 1024;
/// Pixels per texture page side.
pub const BG_PAGE_SIDE: usize = 256;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FieldError {
    #[error("field file is {len} bytes, shorter than the {HEADER_LEN}-byte header")]
    TooShort { len: usize },
    #[error("field header: {0}")]
    BadHeader(String),
    #[error("expected {SECTION_COUNT} sections, header declares {0}")]
    BadSectionCount(u32),
    #[error("section {index} ({name}) offset {offset}: {why}")]
    BadSectionOffset { index: usize, name: &'static str, offset: u64, why: &'static str },
    #[error("no such section: {0}")]
    NoSuchSection(String),
    #[error("section {index} ({name}): declared length {declared} exceeds the {available} bytes available")]
    SectionLength { index: usize, name: &'static str, declared: u32, available: usize },
    #[error("{what}: data ends early (needs {needed} more bytes at offset {at})")]
    Truncated { what: &'static str, at: usize, needed: usize },
    #[error("{what}: count {count} exceeds the limit of {limit}")]
    TooMany { what: &'static str, count: u64, limit: u64 },
    #[error("{what}: {why}")]
    Invalid { what: &'static str, why: String },
}

// ---------------------------------------------------------------------------------------
// Bounds-checked cursor
// ---------------------------------------------------------------------------------------

/// Little-endian cursor that returns an error instead of panicking when data runs out.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, pos: 0, what }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn truncated(&self, needed: usize) -> FieldError {
        FieldError::Truncated { what: self.what, at: self.pos, needed }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FieldError> {
        let slice = slice_at(self.data, self.pos, n).ok_or_else(|| self.truncated(n))?;
        // `slice_at` succeeded, so pos + n <= data.len() and cannot overflow.
        self.pos += n;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<(), FieldError> {
        self.take(n).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FieldError> {
        let s = self.take(N)?;
        s.try_into().map_err(|_| self.truncated(N))
    }

    fn u8(&mut self) -> Result<u8, FieldError> {
        Ok(u8::from_le_bytes(self.array()?))
    }
    fn u16(&mut self) -> Result<u16, FieldError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn i16(&mut self) -> Result<i16, FieldError> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, FieldError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn i32(&mut self) -> Result<i32, FieldError> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    fn i16x3(&mut self) -> Result<[i16; 3], FieldError> {
        Ok([self.i16()?, self.i16()?, self.i16()?])
    }
}

// ---------------------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct SectionSpan {
    /// Offset of the section's `u32` length field.
    start: usize,
    /// Value of that length field.
    declared_len: u32,
    /// Exclusive end of the span (next section's start, or end of file).
    span_end: usize,
}

/// A parsed PC field file: owns the decompressed bytes and the section table.
#[derive(Clone)]
pub struct FieldFile {
    data: Vec<u8>,
    spans: [SectionSpan; SECTION_COUNT],
}

impl fmt::Debug for FieldFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldFile").field("bytes", &self.data.len()).finish()
    }
}

impl FieldFile {
    /// Validate the container header and section table. Section contents are not parsed.
    pub fn parse(data: Vec<u8>) -> Result<Self, FieldError> {
        if data.len() < HEADER_LEN {
            return Err(FieldError::TooShort { len: data.len() });
        }
        let mut r = Reader::new(&data, "field header");
        if r.u16()? != 0 {
            return Err(FieldError::BadHeader("first two bytes must be zero".into()));
        }
        let count = r.u32()?;
        if usize::try_from(count).ok() != Some(SECTION_COUNT) {
            return Err(FieldError::BadSectionCount(count));
        }

        let mut starts = [0usize; SECTION_COUNT];
        // The first section may start right after the header; each later one must start
        // after the previous section's length field.
        let mut min_start = HEADER_LEN;
        for (index, slot) in starts.iter_mut().enumerate() {
            let raw = r.u32()?;
            let name = SECTION_NAMES.get(index).copied().unwrap_or("?");
            let bad = |why: &'static str| FieldError::BadSectionOffset { index, name, offset: u64::from(raw), why };
            let start = usize::try_from(raw).map_err(|_| bad("does not fit in memory"))?;
            if start < min_start {
                return Err(bad("overlaps the header or the previous section"));
            }
            // The 4-byte length field must lie inside the file.
            if start.checked_add(4).is_none_or(|end| end > data.len()) {
                return Err(bad("length field is outside the file"));
            }
            *slot = start;
            min_start = start + 4; // cannot overflow, checked just above
        }

        let mut spans = [SectionSpan { start: 0, declared_len: 0, span_end: 0 }; SECTION_COUNT];
        for (index, span) in spans.iter_mut().enumerate() {
            let start = starts.get(index).copied().unwrap_or(0);
            let span_end = starts.get(index + 1).copied().unwrap_or(data.len());
            let declared_len = u32_at(&data, start).ok_or(FieldError::Truncated {
                what: "section length",
                at: start,
                needed: 4,
            })?;
            *span = SectionSpan { start, declared_len, span_end };
        }
        Ok(Self { data, spans })
    }

    /// The whole decompressed file.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Name of section `index`, if in range.
    pub fn section_name(index: usize) -> Option<&'static str> {
        SECTION_NAMES.get(index).copied()
    }

    /// Index of the section called `name` (e.g. `"walkmesh"`).
    pub fn section_index(name: &str) -> Option<usize> {
        SECTION_NAMES.iter().position(|n| *n == name)
    }

    /// Payload of section `index` (without its length prefix).
    pub fn section(&self, index: usize) -> Result<&[u8], FieldError> {
        let span = self.spans.get(index).ok_or_else(|| FieldError::NoSuchSection(index.to_string()))?;
        let name = SECTION_NAMES.get(index).copied().unwrap_or("?");
        // start + 4 <= data.len() was validated in `parse`.
        let payload_start = span.start + 4;
        let available = span.span_end.saturating_sub(payload_start);
        let too_long = || FieldError::SectionLength { index, name, declared: span.declared_len, available };
        let declared = usize::try_from(span.declared_len).map_err(|_| too_long())?;
        if declared > available {
            return Err(too_long());
        }
        slice_at(&self.data, payload_start, declared).ok_or_else(too_long)
    }

    /// Payload of the section called `name`.
    pub fn section_by_name(&self, name: &str) -> Result<&[u8], FieldError> {
        let index = Self::section_index(name).ok_or_else(|| FieldError::NoSuchSection(name.to_string()))?;
        self.section(index)
    }

    /// Declared payload length of section `index`, whether or not it is consistent.
    pub fn declared_section_len(&self, index: usize) -> Option<u32> {
        self.spans.get(index).map(|s| s.declared_len)
    }

    pub fn walkmesh(&self) -> Result<Walkmesh, FieldError> {
        Walkmesh::parse(self.section(section::WALKMESH)?)
    }

    pub fn cameras(&self) -> Result<Vec<Camera>, FieldError> {
        parse_cameras(self.section(section::CAMERA)?)
    }

    pub fn palette(&self) -> Result<Palette, FieldError> {
        Palette::parse(self.section(section::PALETTE)?)
    }

    pub fn triggers(&self) -> Result<Triggers, FieldError> {
        Triggers::parse(self.section(section::TRIGGERS)?)
    }

    pub fn background(&self) -> Result<Background<'_>, FieldError> {
        Background::parse(self.section(section::BACKGROUND)?)
    }
}

// ---------------------------------------------------------------------------------------
// Walkmesh (section 4)
// ---------------------------------------------------------------------------------------

/// One walkmesh vertex. The file stores a fourth `i16` ("res", padding) which is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertex {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

/// Walkmesh: triangles plus, per triangle, the triangle you land in when you cross each edge.
///
/// Layout: `u32 count`, then `count` triangles of 3 vertices x `{i16 x, y, z, res}` (24
/// bytes each), then `count` access records of 3 x `u16` (6 bytes each). `access[i][e]` is
/// the triangle entered by crossing edge `e` of triangle `i` (edge 0 = v0->v1, 1 = v1->v2,
/// 2 = v2->v0), or [`NO_ACCESS`] if the edge is blocked.
///
/// Which axis is "up" is not stated by the cited pages. Callers in this crate treat x/y as
/// the ground plane and z as height (the task's assumption), UNVERIFIED.
#[derive(Clone, PartialEq, Eq)]
pub struct Walkmesh {
    pub triangles: Vec<[Vertex; 3]>,
    pub access: Vec<[u16; 3]>,
}

/// `Debug` prints counts only: decoded game geometry must not leak into logs by accident.
impl fmt::Debug for Walkmesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Walkmesh").field("triangles", &self.triangles.len()).finish_non_exhaustive()
    }
}

impl Walkmesh {
    pub fn parse(section: &[u8]) -> Result<Self, FieldError> {
        let mut r = Reader::new(section, "walkmesh");
        let count = r.u32()?;
        if count > MAX_TRIANGLES {
            return Err(FieldError::TooMany { what: "walkmesh triangles", count: u64::from(count), limit: u64::from(MAX_TRIANGLES) });
        }
        let n = count as usize;
        // 24 bytes of vertices + 6 bytes of access per triangle; n <= 65_535 so no overflow.
        let needed = n * 30;
        if r.remaining() < needed {
            return Err(r.truncated(needed));
        }
        let mut triangles = Vec::with_capacity(n);
        for _ in 0..n {
            let mut tri = [Vertex { x: 0, y: 0, z: 0 }; 3];
            for v in tri.iter_mut() {
                let (x, y, z, _res) = (r.i16()?, r.i16()?, r.i16()?, r.i16()?);
                *v = Vertex { x, y, z };
            }
            triangles.push(tri);
        }
        let mut access = Vec::with_capacity(n);
        for _ in 0..n {
            access.push([r.u16()?, r.u16()?, r.u16()?]);
        }
        Ok(Self { triangles, access })
    }

    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Triangle entered by crossing `edge` (0..3) of triangle `tri`, or `None` if the edge is
    /// blocked, the arguments are out of range, or the stored index points outside the mesh.
    pub fn neighbor(&self, tri: usize, edge: usize) -> Option<usize> {
        let value = *self.access.get(tri)?.get(edge)?;
        if value == NO_ACCESS {
            return None;
        }
        let target = usize::from(value);
        (target < self.triangles.len()).then_some(target)
    }
}

// ---------------------------------------------------------------------------------------
// Camera (section 1)
// ---------------------------------------------------------------------------------------

/// One 38-byte camera record. Layout: 3 axis vectors of 3 x `i16` (fixed point, divide by
/// [`CAMERA_FIXED_POINT`]), an `i16` duplicate of the Z vector's z (dropped), 3 x `i32`
/// position in camera space, a `u32` blank (dropped), and a 16-bit zoom.
///
/// UNVERIFIED points from the source pages: the order of the three vectors, the meaning of
/// the position triple, and whether zoom is signed (stored here as the raw `u16`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camera {
    pub axes: [[i16; 3]; 3],
    pub position: [i32; 3],
    pub zoom: u16,
}

impl Camera {
    /// Axis vectors converted from fixed point.
    pub fn axes_f32(&self) -> [[f32; 3]; 3] {
        self.axes.map(|v| v.map(|c| f32::from(c) / CAMERA_FIXED_POINT))
    }
}

/// Parse every complete 38-byte record in the camera section (some fields have several; the
/// count is the payload length divided by 38, trailing bytes are ignored).
pub fn parse_cameras(section: &[u8]) -> Result<Vec<Camera>, FieldError> {
    let count = section.len() / CAMERA_RECORD_LEN;
    if count == 0 {
        return Err(FieldError::Truncated { what: "camera", at: 0, needed: CAMERA_RECORD_LEN });
    }
    if count > MAX_CAMERAS {
        return Err(FieldError::TooMany { what: "cameras", count: count as u64, limit: MAX_CAMERAS as u64 });
    }
    let mut r = Reader::new(section, "camera");
    let mut cameras = Vec::with_capacity(count);
    for _ in 0..count {
        let axes = [r.i16x3()?, r.i16x3()?, r.i16x3()?];
        r.skip(2)?; // duplicate of axes[2][2]
        let position = [r.i32()?, r.i32()?, r.i32()?];
        r.skip(4)?; // blank
        let zoom = r.u16()?;
        cameras.push(Camera { axes, position, zoom });
    }
    Ok(cameras)
}

// ---------------------------------------------------------------------------------------
// Palette (section 3)
// ---------------------------------------------------------------------------------------

/// Convert a raw 15-bit field colour to RGBA8.
///
/// Bit layout (both sources agree, and verified by eye on real palette and direct-colour
/// pages): bits 0-4 red, 5-9 green, 10-14 blue, bit 15 "mask" (not interpreted). 5-bit channels
/// are expanded with `(v << 3) | (v >> 2)`. Alpha: raw `0x0000` is transparent, everything
/// else opaque; this is the rule for 16-bit direct-colour pixels (Makou Reactor's, untested by
/// the owned fields). Palette lookups use their own alpha rule, see [`Palette::rgba`].
pub fn rgb555_to_rgba(raw: u16) -> [u8; 4] {
    let expand = |v: u16| -> u8 {
        let v = (v & 0x1F) as u8;
        (v << 3) | (v >> 2)
    };
    let alpha = if raw == 0 { 0 } else { 255 };
    [expand(raw), expand(raw >> 5), expand(raw >> 10), alpha]
}

/// Alpha for a palette lookup: palette index 0 is the transparent colour, every other index
/// is opaque, whatever the RGB values are.
///
/// Verified on owned data by comparing renderings: many palette pages store a non-black key
/// colour at index 0 (pure green `0x03E0` in several fields, for example) that must not show,
/// while 2.2 million layer-2 pixels use a non-zero index whose colour is exactly `0x0000` and
/// must stay opaque. "Transparent only if index 0 is also black" paints green blotches over
/// `cosmin2` and fills the trestle gaps of `mtcrl_5`; "any black is transparent" punches holes
/// in dark foreground objects.
fn palette_alpha(index: usize) -> u8 {
    if index == 0 {
        0
    } else {
        255
    }
}

/// Palette section: header + pages of colours (256 per page in every owned field).
///
/// Layout after the section's own length prefix: `u32` duplicate length (ignored), `u16`
/// palX (0), `u16` palY (480; PlayStation VRAM position, ignored by PC), `u16` colours per page
/// (256), `u16` page count, then `u16` colours until the end of the section. All complete pages
/// present are kept even if the header page count disagrees (the header value is kept in
/// [`Palette::declared_pages`]). A section with no colours is valid (zero pages): the 14
/// direct-colour fields have one.
///
/// A tile's `palette_id` is the page number. That holds for every tile of 700 of the 702 owned
/// fields; `lastmap` and `fr_e` have 312 blended tiles that name a page past the end.
#[derive(Clone, PartialEq, Eq)]
pub struct Palette {
    pub pal_x: u16,
    pub pal_y: u16,
    pub colors_per_page: u16,
    pub declared_pages: u16,
    colors: Vec<u16>,
}

/// `Debug` prints the header and counts only, never the colours.
impl fmt::Debug for Palette {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Palette")
            .field("pal_x", &self.pal_x)
            .field("pal_y", &self.pal_y)
            .field("colors_per_page", &self.colors_per_page)
            .field("declared_pages", &self.declared_pages)
            .field("pages", &self.page_count())
            .finish_non_exhaustive()
    }
}

impl Palette {
    pub fn parse(section: &[u8]) -> Result<Self, FieldError> {
        let mut r = Reader::new(section, "palette");
        r.skip(4)?; // duplicate of the section length
        let pal_x = r.u16()?;
        let pal_y = r.u16()?;
        let colors_per_page = r.u16()?;
        let declared_pages = r.u16()?;
        if colors_per_page == 0 {
            return Err(FieldError::Invalid { what: "palette", why: "colours per page is zero".into() });
        }
        let per_page = usize::from(colors_per_page);
        let available = r.remaining() / 2;
        // A header with no colours is valid real data: 14 owned fields (direct-colour
        // backgrounds such as `gldinfo`) carry a 12-byte palette section with zero pages, so
        // zero complete pages is an empty palette, not an error.
        let pages = available / per_page;
        let total = pages * per_page; // <= available, so bounded by the section size
        let mut colors = Vec::with_capacity(total);
        for _ in 0..total {
            colors.push(r.u16()?);
        }
        Ok(Self { pal_x, pal_y, colors_per_page, declared_pages, colors })
    }

    /// Number of complete pages actually present.
    pub fn page_count(&self) -> usize {
        self.colors.len() / usize::from(self.colors_per_page.max(1))
    }

    /// Raw 15-bit colour `index` of `page`.
    pub fn raw(&self, page: usize, index: usize) -> Option<u16> {
        let per_page = usize::from(self.colors_per_page);
        if index >= per_page {
            return None;
        }
        let at = page.checked_mul(per_page)?.checked_add(index)?;
        self.colors.get(at).copied()
    }

    /// RGBA8 colour `index` of `page`. **Index 0 is always transparent** (alpha 0, RGB as
    /// stored), every other index is opaque. See `palette_alpha` for the evidence.
    pub fn rgba(&self, page: usize, index: usize) -> Option<[u8; 4]> {
        let raw = self.raw(page, index)?;
        let [r, g, b, _] = rgb555_to_rgba(raw);
        Some([r, g, b, palette_alpha(index)])
    }

    /// Every colour of `page` as RGBA8 (same alpha rule as [`Palette::rgba`]).
    pub fn page_rgba(&self, page: usize) -> Option<Vec<[u8; 4]>> {
        let per_page = usize::from(self.colors_per_page);
        let start = page.checked_mul(per_page)?;
        let end = start.checked_add(per_page)?;
        let colors = self.colors.get(start..end)?;
        Some(
            colors
                .iter()
                .enumerate()
                .map(|(index, &raw)| {
                    let [r, g, b, _] = rgb555_to_rgba(raw);
                    [r, g, b, palette_alpha(index)]
                })
                .collect(),
        )
    }
}

// ---------------------------------------------------------------------------------------
// Triggers / gateways (section 7)
// ---------------------------------------------------------------------------------------

/// Extent of the background in field-screen coordinates (`left, bottom, right, top`). The
/// stored order is `bottom <= top` numerically in a y-down space (`md1stin`: -256..256), so
/// "bottom" is not the screen's bottom; use [`CameraRange::bounds`] for a normalised rectangle.
/// This is the area the view may scroll over: 320x240 for non-scrolling fields (176 owned
/// fields), 512x512 for `md1stin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraRange {
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
    pub top: i16,
}

impl CameraRange {
    /// `(min_x, min_y, max_x, max_y)` regardless of how the corners are stored.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        (
            i32::from(self.left.min(self.right)),
            i32::from(self.bottom.min(self.top)),
            i32::from(self.left.max(self.right)),
            i32::from(self.bottom.max(self.top)),
        )
    }
}

/// One gateway slot (24 bytes), verified on the owned data.
///
/// * `exit_a`/`exit_b`: the exit line's end points as `(x, y, z)` in this field's walkmesh
///   coordinates.
/// * `destination`: `(x, y, triangle)` in the **target** field's walkmesh coordinates; the
///   third value is the index of the target walkmesh triangle containing the arrival point (it
///   is not a height). See [`Gateway::destination_xy`] and [`Gateway::destination_triangle`].
/// * `target_field_id`: index into the archive's `maplist` entry (a `u16` count followed by
///   32-byte NUL-padded field names). [`Gateway::UNUSED_FIELD_ID`] marks an unused slot, whose
///   other bytes are all zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gateway {
    pub exit_a: [i16; 3],
    pub exit_b: [i16; 3],
    pub destination: [i16; 3],
    pub target_field_id: u16,
    pub unknown: [u8; 4],
}

impl Gateway {
    /// `target_field_id` of an unused slot.
    pub const UNUSED_FIELD_ID: u16 = 0x7FFF;

    /// Whether the slot describes a real gateway.
    pub fn is_used(&self) -> bool {
        self.target_field_id != Self::UNUSED_FIELD_ID
    }

    /// Arrival `(x, y)` in the target field's walkmesh coordinates.
    pub fn destination_xy(&self) -> [i16; 2] {
        let [x, y, _] = self.destination;
        [x, y]
    }

    /// Index of the target field's walkmesh triangle that contains the arrival point.
    pub fn destination_triangle(&self) -> Option<usize> {
        usize::try_from(self.destination[2]).ok()
    }
}

/// One trigger slot (16 bytes): two corners (3 x `i16`, file order), background group and
/// frame to toggle, behaviour mode (0-5) and sound id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub corner_a: [i16; 3],
    pub corner_b: [i16; 3],
    pub bg_group_id: u8,
    pub bg_frame_id: u8,
    pub behavior: u8,
    pub sound_id: u8,
}

/// Triggers section: 56-byte header, 12 gateways, 12 triggers (arrow data, present in
/// non-Japanese releases, is not parsed). The section is 740 bytes in every owned field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triggers {
    /// Field name stored in the section (9-byte NUL-terminated field).
    pub name: String,
    /// Direction of the field's "up" for movement controls, an unsigned angle where 256 is a
    /// full turn (`124` in `md1stin`; 572 of 702 owned fields exceed 127, so it is not signed).
    pub control_direction: u8,
    /// Camera focus height relative to the character (0 = default).
    pub focus_height: i16,
    pub camera_range: CameraRange,
    /// Layer 3 and layer 4 animation sizes: `[l3_w, l3_h, l4_w, l4_h]`.
    pub bg_animation_sizes: [i16; 4],
    pub gateways: Vec<Gateway>,
    pub triggers: Vec<Trigger>,
}

impl Triggers {
    /// The gateway slots that are in use.
    pub fn used_gateways(&self) -> impl Iterator<Item = &Gateway> {
        self.gateways.iter().filter(|g| g.is_used())
    }

    pub fn parse(section: &[u8]) -> Result<Self, FieldError> {
        let mut r = Reader::new(section, "triggers");
        if r.remaining() < TRIGGERS_MIN_LEN {
            return Err(r.truncated(TRIGGERS_MIN_LEN));
        }
        let name = nul_padded_name(r.take(9)?);
        let control_direction = r.u8()?;
        let focus_height = r.i16()?;
        let camera_range = CameraRange { left: r.i16()?, bottom: r.i16()?, right: r.i16()?, top: r.i16()? };
        r.skip(4)?; // 2 x u16 unknown, tied to layers 3/4
        let bg_animation_sizes = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];
        r.skip(24)?; // unknown, tied to layers 3/4
        let mut gateways = Vec::with_capacity(GATEWAY_SLOTS);
        for _ in 0..GATEWAY_SLOTS {
            gateways.push(Gateway {
                exit_a: r.i16x3()?,
                exit_b: r.i16x3()?,
                destination: r.i16x3()?,
                target_field_id: r.u16()?,
                unknown: r.array()?,
            });
        }
        let mut triggers = Vec::with_capacity(TRIGGER_SLOTS);
        for _ in 0..TRIGGER_SLOTS {
            triggers.push(Trigger {
                corner_a: r.i16x3()?,
                corner_b: r.i16x3()?,
                bg_group_id: r.u8()?,
                bg_frame_id: r.u8()?,
                behavior: r.u8()?,
                sound_id: r.u8()?,
            });
        }
        Ok(Self { name, control_direction, focus_height, camera_range, bg_animation_sizes, gateways, triggers })
    }
}

// ---------------------------------------------------------------------------------------
// Background (section 8)
// ---------------------------------------------------------------------------------------

/// One background tile placement (a decoded 52-byte record). Layer numbers are 1-4.
///
/// Raw fields are kept as stored; see [`TileSprite::effective_source`] for the rule the
/// community readers apply to choose between the two source sets. Tile pixels are
/// `tile_size` x `tile_size` (16 for layers 1-2, 32 for layers 3-4 per Makou Reactor's
/// reader; the record's own width/height fields are "normally unused").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileSprite {
    /// 1..=4.
    pub layer: u8,
    /// Index within its layer.
    pub index: u16,
    /// Destination x/y in background pixels, signed, origin at the image centre per the
    /// community notes.
    pub dst_x: i16,
    pub dst_y: i16,
    /// Primary source position inside the texture page.
    pub src_x: u8,
    pub src_y: u8,
    /// Secondary source position (used instead of the primary one by blended tiles in
    /// layers 2-4).
    pub src_x2: u8,
    pub src_y2: u8,
    /// Primary / secondary texture slot (0..42), see the module's known gaps.
    pub texture_id: u8,
    pub texture_id2: u8,
    /// Palette page in the palette section (256 colours per page).
    pub palette_id: u8,
    pub blending: u8,
    pub type_trans: u8,
    pub depth: u8,
    /// Draw-order / identifier field as stored.
    pub id: u16,
    /// Animation/visibility group and frame, as stored (layers 2-4).
    pub param: u8,
    pub state: u8,
    /// Tile edge length in pixels (16 or 32).
    pub tile_size: u8,
}

impl TileSprite {
    /// `(texture slot, src_x, src_y)` actually used when drawing: blended tiles in layers
    /// 2-4 use the secondary set, everything else the primary set (rule taken from Makou
    /// Reactor's PC tile reader).
    pub fn effective_source(&self) -> (u8, u8, u8) {
        if self.layer > 1 && self.blending != 0 {
            (self.texture_id2, self.src_x2, self.src_y2)
        } else {
            (self.texture_id, self.src_x, self.src_y)
        }
    }
}

/// Header of one background layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerHeader {
    pub width: u16,
    pub height: u16,
    pub tile_count: u16,
}

/// Location of one 256x256 texture page inside the background section. `pixels` borrows
/// the field bytes (no copy), row-major, 256 pixels per row, and is `depth * 65536` bytes
/// (32768 for depth 0). Depth 1 = 8-bit palette indices (every paletted page in the owned
/// data); depth 2 = 16-bit direct colour (same 15-bit layout as the palette; slots 26-28 in the
/// owned data); depth 0 = 4-bit palette indices, low nibble first (never occurs in the owned
/// data, so unverified). `big_tile_flag` is the first per-page `u16`, stored as found: 0 for
/// pages holding 16x16 tiles (layers 1-2), 1 for pages holding 32x32 tiles (layers 3-4), which
/// is exactly how the owned data uses it.
#[derive(Clone, Copy)]
pub struct TexturePage<'a> {
    pub slot: u8,
    pub big_tile_flag: u16,
    pub depth: u16,
    pub pixels: &'a [u8],
}

/// `Debug` prints the page's metadata and byte count, never the pixels.
impl fmt::Debug for TexturePage<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TexturePage")
            .field("slot", &self.slot)
            .field("big_tile_flag", &self.big_tile_flag)
            .field("depth", &self.depth)
            .field("pixel_bytes", &self.pixels.len())
            .finish()
    }
}

/// Decoded background section: layers 1-4 as a flat tile list plus texture page locations.
///
/// Layout (offsets from the section payload; PyFF7 + Makou + Fergusson agree):
///
/// ```text
/// 0x00 u16 unknown   0x02 u16 depth   0x04 u8 unknown   0x05 "PALETTE" (7)   0x0C u32 size
/// 0x10 4 x u16 + 6 x u16 (palette-like header, ignored)   0x24 "BACK" (4)
/// layer 1 @0x28: u16 width, u16 height, u16 tile count, u16 depth, u16 blank, tiles, u16 blank
/// layers 2-4 each: u8 present flag (0 or 1); if 1: u16 w, u16 h, u16 count,
///                  16 (layer 2) or 10 (layers 3, 4) unknown bytes, u16 blank, tiles, u16 blank
/// "TEXTURE" (7), then 42 slots: u16 exists; if nonzero: u16 big-tile flag, u16 depth,
///                  depth*65536 bytes (32768 if depth is 0)
/// ```
///
/// Tile records are 52 bytes; see [`TileSprite`]. Records whose destination is at or beyond
/// `+-`[`MAX_TILE_DST`] are dropped (see the module docs). A texture-section problem does NOT
/// fail the parse: the tiles are still returned and [`Background::texture_error`] says why the
/// pages are missing. Pixels: [`Background::tile_rgba`] decodes one tile;
/// [`crate::background`] composites all of them.
#[derive(Clone)]
pub struct Background<'a> {
    /// Section-level `depth` field (almost always 1).
    pub depth: u16,
    /// Layer headers for layers 1-4 (`None` = layer absent).
    pub layers: [Option<LayerHeader>; 4],
    /// All tiles that passed the destination rule ([`MAX_TILE_DST`]), layer 1 first. A
    /// tile's `index` is its position in the file's layer, so discarded records leave gaps.
    pub tiles: Vec<TileSprite>,
    /// Tile records dropped by the destination rule, over all layers. `tiles.len() +
    /// discarded_tiles` is the sum of the layer headers' tile counts.
    pub discarded_tiles: usize,
    /// Texture pages by slot (length [`BG_TEXTURE_SLOTS`] when they parsed, else empty).
    pub textures: Vec<Option<TexturePage<'a>>>,
    /// Why `textures` is empty, if the texture section failed to parse.
    pub texture_error: Option<FieldError>,
    /// Whether the 3-byte tag `END` follows the last texture slot (it does in every owned
    /// field). Only meaningful when `texture_error` is `None`.
    pub end_tag: bool,
    /// Bytes of the section after the texture slots and the `END` tag: 0 in 578 owned fields,
    /// 14 (`FINAL FANTASY7`) in 121 and 17, 19 or 131 in three. Only meaningful when
    /// `texture_error` is `None`.
    pub trailing_bytes: usize,
}

/// `Debug` prints layer headers and counts only (no tiles, no pixels).
impl fmt::Debug for Background<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let present: Vec<u8> = self.textures.iter().flatten().map(|t| t.slot).collect();
        f.debug_struct("Background")
            .field("depth", &self.depth)
            .field("layers", &self.layers)
            .field("tiles", &self.tiles.len())
            .field("discarded_tiles", &self.discarded_tiles)
            .field("texture_slots", &present)
            .field("texture_error", &self.texture_error)
            .field("end_tag", &self.end_tag)
            .field("trailing_bytes", &self.trailing_bytes)
            .finish()
    }
}

fn expect_tag(r: &mut Reader<'_>, tag: &'static [u8]) -> Result<(), FieldError> {
    if r.take(tag.len())? == tag {
        Ok(())
    } else {
        Err(FieldError::Invalid {
            what: "background",
            why: format!("expected the tag {:?} at offset {}", String::from_utf8_lossy(tag), r.pos.saturating_sub(tag.len())),
        })
    }
}

fn read_tile(r: &mut Reader<'_>, layer: u8, index: u16) -> Result<TileSprite, FieldError> {
    let record = r.take(BG_TILE_RECORD_LEN)?;
    let mut t = Reader::new(record, "background tile");
    t.skip(2)?; // +0 blank
    let dst_x = t.i16()?; // +2
    let dst_y = t.i16()?; // +4
    t.skip(4)?; // +6 unknown
    let src_x = t.u8()?; // +10
    t.skip(1)?;
    let src_y = t.u8()?; // +12
    t.skip(1)?;
    let src_x2 = t.u8()?; // +14
    t.skip(1)?;
    let src_y2 = t.u8()?; // +16
    t.skip(1)?;
    t.skip(4)?; // +18 width, height (normally unused)
    let palette_id = t.u8()?; // +22
    t.skip(1)?;
    let id = t.u16()?; // +24
    let param = t.u8()?; // +26
    let state = t.u8()?; // +27
    let blending = t.u8()?; // +28
    t.skip(1)?;
    let type_trans = t.u8()?; // +30
    t.skip(1)?;
    let texture_id = t.u8()?; // +32
    t.skip(1)?;
    let texture_id2 = t.u8()?; // +34
    t.skip(1)?;
    let depth = t.u8()?; // +36
    // +37 pad, +38 id_big u32, +42 src_x_big u32, +46 src_y_big u32, +50 blank u16: ignored
    Ok(TileSprite {
        layer,
        index,
        dst_x,
        dst_y,
        src_x,
        src_y,
        src_x2,
        src_y2,
        texture_id,
        texture_id2,
        palette_id,
        blending,
        type_trans,
        depth,
        id,
        param,
        state,
        tile_size: if layer > 2 { 32 } else { 16 },
    })
}

/// The destination sanity rule of the reference reader (see [`MAX_TILE_DST`]).
fn destination_in_range(tile: &TileSprite) -> bool {
    tile.dst_x.unsigned_abs() < MAX_TILE_DST && tile.dst_y.unsigned_abs() < MAX_TILE_DST
}

/// Read `count` tile records of one layer, keeping those that pass the destination rule and
/// adding the number of dropped ones to `discarded`.
fn read_tiles(
    r: &mut Reader<'_>,
    layer: u8,
    count: u16,
    out: &mut Vec<TileSprite>,
    discarded: &mut usize,
) -> Result<(), FieldError> {
    // count <= 65_535, so 52 * count fits in usize on every supported target.
    let needed = usize::from(count) * BG_TILE_RECORD_LEN;
    if r.remaining() < needed {
        return Err(r.truncated(needed));
    }
    out.reserve(usize::from(count));
    for index in 0..count {
        let tile = read_tile(r, layer, index)?;
        if destination_in_range(&tile) {
            out.push(tile);
        } else {
            *discarded += 1;
        }
    }
    Ok(())
}

fn read_textures<'a>(r: &mut Reader<'a>) -> Result<Vec<Option<TexturePage<'a>>>, FieldError> {
    expect_tag(r, b"TEXTURE")?;
    let mut slots = Vec::with_capacity(BG_TEXTURE_SLOTS);
    for slot in 0..BG_TEXTURE_SLOTS {
        if r.u16()? == 0 {
            slots.push(None);
            continue;
        }
        let big_tile_flag = r.u16()?;
        let depth = r.u16()?;
        let page_bytes = match depth {
            0 => BG_PAGE_SIDE * BG_PAGE_SIDE / 2,
            1 | 2 => usize::from(depth) * BG_PAGE_SIDE * BG_PAGE_SIDE,
            _ => {
                return Err(FieldError::Invalid {
                    what: "background texture",
                    why: format!("slot {slot} has unsupported depth {depth} (expected 0, 1 or 2)"),
                })
            }
        };
        let pixels = r.take(page_bytes)?;
        slots.push(Some(TexturePage { slot: slot as u8, big_tile_flag, depth, pixels }));
    }
    Ok(slots)
}

impl<'a> Background<'a> {
    pub fn parse(section: &'a [u8]) -> Result<Self, FieldError> {
        let mut r = Reader::new(section, "background");
        r.skip(2)?; // unknown (seen as 0)
        let depth = r.u16()?;
        r.skip(1)?; // unknown (seen as 1)
        expect_tag(&mut r, b"PALETTE")?;
        r.skip(4 + 8 + 12)?; // u32 size, palX/palY/width/height, 6 colours: ignored
        expect_tag(&mut r, b"BACK")?;

        let mut layers = [None; 4];
        let mut tiles = Vec::new();
        let mut discarded_tiles = 0usize;

        // Layer 1 has no presence flag.
        let (width, height, count) = (r.u16()?, r.u16()?, r.u16()?);
        r.skip(4)?; // u16 depth, u16 blank
        if let Some(slot) = layers.get_mut(0) {
            *slot = Some(LayerHeader { width, height, tile_count: count });
        }
        read_tiles(&mut r, 1, count, &mut tiles, &mut discarded_tiles)?;
        r.skip(2)?; // blank after the tiles

        for layer in 2..=4u8 {
            match r.u8()? {
                0 => continue,
                1 => {}
                other => {
                    return Err(FieldError::Invalid {
                        what: "background",
                        why: format!("layer {layer} presence flag is {other}, expected 0 or 1"),
                    })
                }
            }
            let (width, height, count) = (r.u16()?, r.u16()?, r.u16()?);
            r.skip(if layer == 2 { 16 } else { 10 })?; // unknown
            r.skip(2)?; // blank
            if let Some(slot) = layers.get_mut(usize::from(layer) - 1) {
                *slot = Some(LayerHeader { width, height, tile_count: count });
            }
            read_tiles(&mut r, layer, count, &mut tiles, &mut discarded_tiles)?;
            r.skip(2)?; // blank after the tiles
        }

        let (textures, texture_error) = match read_textures(&mut r) {
            Ok(t) => (t, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let (end_tag, trailing_bytes) = if texture_error.is_none() {
            let end_tag = slice_at(section, r.pos, 3) == Some(b"END".as_slice());
            if end_tag {
                r.pos += 3; // slice_at succeeded, so pos + 3 <= len
            }
            (end_tag, r.remaining())
        } else {
            (false, 0)
        };
        Ok(Self { depth, layers, tiles, discarded_tiles, textures, texture_error, end_tag, trailing_bytes })
    }

    /// Tiles of one layer (1-4) in file order.
    pub fn tiles_in_layer(&self, layer: u8) -> impl Iterator<Item = &TileSprite> {
        self.tiles.iter().filter(move |t| t.layer == layer)
    }

    /// Texture page in `slot`, if present.
    pub fn texture(&self, slot: u8) -> Option<&TexturePage<'a>> {
        self.textures.get(usize::from(slot))?.as_ref()
    }

    /// Decode one tile to `tile_size * tile_size` RGBA8 pixels (row-major) using the tile's
    /// effective source (see [`TileSprite::effective_source`]).
    ///
    /// Depth-0 and depth-1 pages are looked up in palette page `palette_id` (palette index 0 is
    /// transparent, see [`Palette::rgba`]); depth-2 pages are read as direct 15-bit colour
    /// ([`rgb555_to_rgba`], raw `0x0000` transparent). `blending` is not applied here.
    ///
    /// Returns `None` if the texture slot or palette page is missing, the page depth is
    /// unknown, or the tile would read outside its 256x256 page (for the 312 owned tiles that
    /// name a palette page past the palette section this is the answer).
    pub fn tile_rgba(&self, tile: &TileSprite, palette: &Palette) -> Option<Vec<[u8; 4]>> {
        let (slot, src_x, src_y) = tile.effective_source();
        let page = self.texture(slot)?;
        let size = usize::from(tile.tile_size);
        let (src_x, src_y) = (usize::from(src_x), usize::from(src_y));
        if src_x.checked_add(size)? > BG_PAGE_SIDE || src_y.checked_add(size)? > BG_PAGE_SIDE {
            return None;
        }
        let palette_page = usize::from(tile.palette_id);
        let mut out = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                let pixel = (src_y + y) * BG_PAGE_SIDE + (src_x + x);
                let rgba = match page.depth {
                    0 => {
                        let byte = *page.pixels.get(pixel / 2)?;
                        let index = if pixel % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                        palette.rgba(palette_page, usize::from(index))?
                    }
                    1 => palette.rgba(palette_page, usize::from(*page.pixels.get(pixel)?))?,
                    2 => {
                        let at = pixel.checked_mul(2)?;
                        let raw = u16::from_le_bytes([*page.pixels.get(at)?, *page.pixels.get(at + 1)?]);
                        rgb555_to_rgba(raw)
                    }
                    _ => return None,
                };
                out.push(rgba);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FieldSpec, TileSpec};

    fn parse(spec: &FieldSpec) -> FieldFile {
        FieldFile::parse(spec.build()).expect("synthetic field parses")
    }

    #[test]
    fn container_exposes_nine_named_sections() {
        let f = parse(&FieldSpec::default());
        assert_eq!(SECTION_NAMES.len(), 9);
        for (i, name) in SECTION_NAMES.iter().enumerate() {
            assert_eq!(FieldFile::section_name(i), Some(*name));
            assert_eq!(FieldFile::section_index(name), Some(i));
            assert!(f.section(i).is_ok(), "section {i} {name}");
            assert_eq!(f.section_by_name(name).unwrap(), f.section(i).unwrap());
        }
        assert_eq!(FieldFile::section_name(9), None);
        assert_eq!(FieldFile::section_index("nope"), None);
        assert!(matches!(f.section(9), Err(FieldError::NoSuchSection(_))));
        assert!(matches!(f.section_by_name("nope"), Err(FieldError::NoSuchSection(_))));
        assert_eq!(f.declared_section_len(4), Some(f.section(section::WALKMESH).unwrap().len() as u32));
        assert_eq!(f.declared_section_len(9), None);
        assert!(f.bytes().len() > HEADER_LEN);
        assert!(format!("{f:?}").contains("bytes"));
    }

    #[test]
    fn walkmesh_parses_vertices_and_access() {
        let f = parse(&FieldSpec::default());
        let w = f.walkmesh().unwrap();
        assert_eq!(w.len(), 2);
        assert!(!w.is_empty());
        assert_eq!(w.triangles[0][1], Vertex { x: 100, y: 0, z: 100 });
        assert_eq!(w.triangles[1][2], Vertex { x: 0, y: 100, z: 100 });
        // triangle 0: edges 0 (0,0)->(100,0) and 1 (100,0)->(100,100) are blocked, edge 2 (the
        // diagonal back to v0) crosses into triangle 1; triangle 1's edge 0 is the same diagonal
        assert_eq!(w.neighbor(0, 0), None);
        assert_eq!(w.neighbor(0, 1), None);
        assert_eq!(w.neighbor(0, 2), Some(1));
        assert_eq!(w.neighbor(1, 0), Some(0));
        assert_eq!(w.neighbor(0, 3), None);
        assert_eq!(w.neighbor(9, 0), None);
    }

    #[test]
    fn walkmesh_neighbor_ignores_out_of_range_indices() {
        let w = Walkmesh {
            triangles: vec![[Vertex { x: 0, y: 0, z: 0 }; 3]],
            access: vec![[5, NO_ACCESS, 0]],
        };
        assert_eq!(w.neighbor(0, 0), None); // points past the mesh
        assert_eq!(w.neighbor(0, 1), None); // blocked
        assert_eq!(w.neighbor(0, 2), Some(0));
    }

    #[test]
    fn camera_record_decodes() {
        let f = parse(&FieldSpec::default());
        let cams = f.cameras().unwrap();
        assert_eq!(cams.len(), 1);
        let c = cams[0];
        assert_eq!(c.axes, [[4096, 0, 0], [0, -4096, 0], [0, 0, 4096]]);
        assert_eq!(c.position, [10, -20, 30]);
        assert_eq!(c.zoom, 0x0400);
        assert_eq!(c.axes_f32()[0], [1.0, 0.0, 0.0]);
        assert_eq!(c.axes_f32()[1], [0.0, -1.0, 0.0]);
    }

    #[test]
    fn several_cameras_and_trailing_bytes() {
        let mut one = Vec::new();
        for i in 0..3i16 {
            one.extend_from_slice(&crate::testutil::camera_record([[i, 0, 0], [0, 1, 0], [0, 0, 1]], [0, 0, 0], 7));
        }
        one.extend_from_slice(&[9, 9, 9]); // partial trailing record is ignored
        let cams = parse_cameras(&one).unwrap();
        assert_eq!(cams.len(), 3);
        assert_eq!(cams[2].axes[0], [2, 0, 0]);
        assert!(matches!(parse_cameras(&[0; 37]), Err(FieldError::Truncated { .. })));
        assert!(matches!(parse_cameras(&[]), Err(FieldError::Truncated { .. })));
        let huge = vec![0u8; CAMERA_RECORD_LEN * (MAX_CAMERAS + 1)];
        assert!(matches!(parse_cameras(&huge), Err(FieldError::TooMany { .. })));
    }

    #[test]
    fn palette_pages_and_colour_conversion() {
        let f = parse(&FieldSpec::default());
        let p = f.palette().unwrap();
        assert_eq!((p.pal_x, p.pal_y, p.colors_per_page, p.declared_pages), (0, 480, 256, 2));
        assert_eq!(p.page_count(), 2);
        // synthetic colours: page0[i] = i, page1[i] = 0x7FFF - i
        assert_eq!(p.raw(0, 5), Some(5));
        assert_eq!(p.raw(1, 3), Some(0x7FFF - 3));
        assert_eq!(p.raw(2, 0), None);
        assert_eq!(p.raw(0, 256), None);
        assert_eq!(p.raw(usize::MAX, 1), None);
        assert_eq!(p.rgba(0, 0), Some([0, 0, 0, 0])); // raw 0 is transparent black
        assert_eq!(p.page_rgba(1).unwrap().len(), 256);
        assert!(p.page_rgba(2).is_none());
        assert!(p.page_rgba(usize::MAX).is_none());
        // bit layout: red = bits 0-4, green = 5-9, blue = 10-14
        assert_eq!(rgb555_to_rgba(0x001F), [255, 0, 0, 255]);
        assert_eq!(rgb555_to_rgba(0x03E0), [0, 255, 0, 255]);
        assert_eq!(rgb555_to_rgba(0x7C00), [0, 0, 255, 255]);
        assert_eq!(rgb555_to_rgba(0x7FFF), [255, 255, 255, 255]);
        assert_eq!(rgb555_to_rgba(0x0001), [8, 0, 0, 255]);
        assert_eq!(rgb555_to_rgba(0x8000), [0, 0, 0, 255]); // bit 15 not interpreted
    }

    #[test]
    fn palette_alpha_rule_only_index_zero_can_be_transparent() {
        // one page of 4 colours: [0x0000, 0x0000, 0x001F, 0x0000]
        let mut bytes = vec![0u8; 4];
        for v in [0u16, 480, 4, 1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for c in [0x0000u16, 0x0000, 0x001F, 0x0000] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let p = Palette::parse(&bytes).unwrap();
        assert_eq!(p.rgba(0, 0), Some([0, 0, 0, 0])); // index 0, raw 0: transparent
        assert_eq!(p.rgba(0, 1), Some([0, 0, 0, 255])); // raw 0 at another index: opaque black
        assert_eq!(p.rgba(0, 2), Some([255, 0, 0, 255]));
        let page = p.page_rgba(0).unwrap();
        assert_eq!(page, vec![[0, 0, 0, 0], [0, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 255]]);
        // direct colour: raw 0 is transparent regardless of position
        assert_eq!(rgb555_to_rgba(0)[3], 0);
    }

    #[test]
    fn palette_index_zero_is_transparent_even_when_its_colour_is_not_black() {
        // Owned data stores key colours (pure green 0x03E0, white) at index 0 and uses a non-zero
        // index for opaque black: index 0 is transparent, nothing else is.
        let mut bytes = vec![0u8; 4];
        for v in [0u16, 480, 3, 1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for c in [0x03E0u16, 0x0000, 0x7FFF] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let p = Palette::parse(&bytes).unwrap();
        assert_eq!(p.rgba(0, 0), Some([0, 255, 0, 0]));
        assert_eq!(p.rgba(0, 1), Some([0, 0, 0, 255]));
        assert_eq!(p.rgba(0, 2), Some([255, 255, 255, 255]));
        assert_eq!(p.page_rgba(0).unwrap(), vec![[0, 255, 0, 0], [0, 0, 0, 255], [255, 255, 255, 255]]);
    }

    #[test]
    fn palette_rejects_bad_headers() {
        assert!(matches!(Palette::parse(&[]), Err(FieldError::Truncated { .. })));
        let mut zero_cpp = vec![0u8; 12];
        zero_cpp[8..10].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(Palette::parse(&zero_cpp), Err(FieldError::Invalid { .. })));
        // Zero complete pages is a valid, empty palette: 14 owned direct-colour fields have a
        // header-only section; a partial page is dropped.
        for len in [12usize, 12 + 20] {
            let mut no_page = vec![0u8; len];
            no_page[8..10].copy_from_slice(&256u16.to_le_bytes());
            let p = Palette::parse(&no_page).unwrap();
            assert_eq!((p.page_count(), p.declared_pages), (0, 0));
            assert_eq!((p.raw(0, 0), p.rgba(0, 0), p.page_rgba(0)), (None, None, None));
        }
        // a header that claims more pages than exist keeps the complete pages present
        let mut short = vec![0u8; 12 + 2 * 256];
        short[8..10].copy_from_slice(&256u16.to_le_bytes());
        short[10..12].copy_from_slice(&40u16.to_le_bytes());
        let p = Palette::parse(&short).unwrap();
        assert_eq!((p.page_count(), p.declared_pages), (1, 40));
    }

    #[test]
    fn triggers_header_gateways_and_triggers() {
        let f = parse(&FieldSpec::default());
        let t = f.triggers().unwrap();
        assert_eq!(t.name, "testfld");
        assert_eq!(t.control_direction, 64);
        assert_eq!(t.focus_height, -12);
        assert_eq!(t.camera_range, CameraRange { left: -160, bottom: -120, right: 160, top: 120 });
        assert_eq!(t.bg_animation_sizes, [1024, 1024, 512, 256]);
        assert_eq!(t.gateways.len(), GATEWAY_SLOTS);
        assert_eq!(t.triggers.len(), TRIGGER_SLOTS);
        let g = t.gateways[0];
        assert_eq!(g.exit_a, [1, 2, 3]);
        assert_eq!(g.exit_b, [4, 5, 6]);
        assert_eq!(g.destination, [7, 8, 9]);
        assert_eq!(g.target_field_id, 0x0123);
        assert_eq!(g.unknown, [0xAB; 4]);
        assert_eq!(t.gateways[1].target_field_id, 0x7FFF);
        let tr = t.triggers[2];
        assert_eq!((tr.corner_a, tr.corner_b), ([-1, -2, -3], [11, 12, 13]));
        assert_eq!((tr.bg_group_id, tr.bg_frame_id, tr.behavior, tr.sound_id), (3, 4, 2, 9));
    }

    #[test]
    fn control_direction_is_an_unsigned_angle() {
        let mut raw = vec![0u8; 740];
        for value in [0u8, 124, 127, 128, 200, 255] {
            raw[9] = value;
            assert_eq!(Triggers::parse(&raw).unwrap().control_direction, value);
        }
    }

    #[test]
    fn gateway_helpers_follow_the_verified_layout() {
        let t = parse(&FieldSpec::default()).triggers().unwrap();
        assert_eq!(t.used_gateways().count(), 1);
        let g = t.gateways[0];
        assert!(g.is_used());
        assert_eq!(g.destination_xy(), [7, 8]); // destination = (x, y, triangle id)
        assert_eq!(g.destination_triangle(), Some(9));
        assert!(!t.gateways[1].is_used());
        assert_eq!(Gateway::UNUSED_FIELD_ID, 0x7FFF);
        let negative = Gateway { destination: [1, 2, -1], ..g };
        assert_eq!(negative.destination_triangle(), None);
    }

    #[test]
    fn camera_range_bounds_are_normalised() {
        // md1stin stores bottom = -256 and top = +256 in a y-down space
        let r = CameraRange { left: -256, bottom: -256, right: 256, top: 256 };
        assert_eq!(r.bounds(), (-256, -256, 256, 256));
        let swapped = CameraRange { left: 10, bottom: 20, right: -10, top: -20 };
        assert_eq!(swapped.bounds(), (-10, -20, 10, 20));
    }

    #[test]
    fn triggers_reject_short_sections() {
        assert!(matches!(Triggers::parse(&[]), Err(FieldError::Truncated { .. })));
        assert!(matches!(Triggers::parse(&vec![0; TRIGGERS_MIN_LEN - 1]), Err(FieldError::Truncated { .. })));
        assert!(Triggers::parse(&vec![0; TRIGGERS_MIN_LEN]).is_ok());
        assert!(Triggers::parse(&vec![0; 740]).is_ok());
    }

    #[test]
    fn background_layers_tiles_and_textures() {
        let f = parse(&FieldSpec::default());
        let bg = f.background().unwrap();
        assert_eq!(bg.depth, 1);
        assert_eq!(bg.layers[0], Some(LayerHeader { width: 320, height: 240, tile_count: 2 }));
        assert_eq!(bg.layers[1], Some(LayerHeader { width: 640, height: 480, tile_count: 1 }));
        assert_eq!(bg.layers[2], None);
        assert_eq!(bg.layers[3], Some(LayerHeader { width: 64, height: 64, tile_count: 1 }));
        assert_eq!(bg.tiles.len(), 4);
        assert_eq!(bg.tiles_in_layer(1).count(), 2);
        assert_eq!(bg.tiles_in_layer(3).count(), 0);

        let t0 = bg.tiles[0];
        assert_eq!((t0.layer, t0.index, t0.dst_x, t0.dst_y), (1, 0, -160, -120));
        assert_eq!((t0.src_x, t0.src_y, t0.texture_id, t0.palette_id), (16, 32, 0, 1));
        assert_eq!(t0.tile_size, 16);
        let t1 = bg.tiles[1];
        assert_eq!((t1.layer, t1.index, t1.dst_x, t1.dst_y), (1, 1, -144, -120));

        // layer 2 tile: blended, so the effective source is the secondary set
        let l2 = bg.tiles[2];
        assert_eq!(l2.layer, 2);
        assert_eq!((l2.blending, l2.type_trans, l2.param, l2.state, l2.id), (1, 2, 5, 6, 77));
        assert_eq!((l2.texture_id, l2.texture_id2), (0, 3));
        assert_eq!(l2.effective_source(), (3, 8, 9));
        assert_eq!(bg.tiles[0].effective_source(), (0, 16, 32));
        assert_eq!(l2.tile_size, 16);

        // layer 4 tile has 32x32 tiles
        let l4 = bg.tiles[3];
        assert_eq!((l4.layer, l4.tile_size), (4, 32));

        assert!(bg.texture_error.is_none());
        assert_eq!(bg.textures.len(), BG_TEXTURE_SLOTS);
        assert!(bg.texture(0).is_some());
        assert!(bg.texture(1).is_none());
        assert!(bg.texture(200).is_none());
        let page = bg.texture(0).unwrap();
        assert_eq!((page.slot, page.depth, page.pixels.len()), (0, 1, 65536));
        let page3 = bg.texture(3).unwrap();
        assert_eq!((page3.slot, page3.depth, page3.pixels.len()), (3, 2, 131_072));
    }

    #[test]
    fn tiles_with_overflowing_destinations_are_dropped_like_the_reference_reader_does() {
        // Layer 1: in range, -32768 (the classic garbage record), exactly +-1024 (dropped:
        // the rule is strict), 1023 / -1023 (kept), a bad y only, a bad x only. Layers 2 and 4
        // get one bad and one good record each, so the rule is checked on all of them.
        let tile = |dst_x: i16, dst_y: i16| TileSpec { dst_x, dst_y, ..TileSpec::default() };
        let spec = FieldSpec {
            tiles: vec![
                tile(-160, -120),
                tile(i16::MIN, 0),
                tile(1024, 0),
                tile(-1024, 5),
                tile(1023, -1023),
                tile(0, i16::MAX),
                tile(i16::MAX, 0),
                tile(-1023, 1023),
            ],
            layer2: Some(crate::testutil::LayerSpec { width: 640, height: 480, tiles: vec![tile(2000, 0), tile(7, 8)] }),
            layer4: Some(crate::testutil::LayerSpec { width: 64, height: 64, tiles: vec![tile(0, -2000), tile(-9, -9)] }),
            ..FieldSpec::default()
        };
        let f = parse(&spec);
        let bg = f.background().unwrap();
        let kept: Vec<(u8, u16, i16, i16)> = bg.tiles.iter().map(|t| (t.layer, t.index, t.dst_x, t.dst_y)).collect();
        assert_eq!(
            kept,
            [
                (1, 0, -160, -120),
                (1, 4, 1023, -1023),
                (1, 7, -1023, 1023),
                (2, 1, 7, 8),
                (4, 1, -9, -9),
            ]
        );
        assert_eq!(bg.discarded_tiles, 7);
        // the layer headers still say what the file says, and nothing is lost in the accounting
        let declared: usize = bg.layers.iter().flatten().map(|l| usize::from(l.tile_count)).sum();
        assert_eq!(declared, 12);
        assert_eq!(bg.tiles.len() + bg.discarded_tiles, declared);
        assert!(bg.tiles.iter().all(|t| t.dst_x.unsigned_abs() < MAX_TILE_DST && t.dst_y.unsigned_abs() < MAX_TILE_DST));
        assert!(format!("{bg:?}").contains("discarded_tiles"));
        // the default synthetic field has nothing to drop
        assert_eq!(parse(&FieldSpec::default()).background().unwrap().discarded_tiles, 0);
        assert_eq!(MAX_TILE_DST, 1024);
    }

    #[test]
    fn tile_rgba_decodes_paletted_and_direct_pages() {
        let f = parse(&FieldSpec::default());
        let bg = f.background().unwrap();
        let pal = f.palette().unwrap();
        // tile 0: slot 0 (8-bit), src (16,32), palette page 1. Pixel (x,y) of the page holds
        // ((y * 256 + x) % 251) as u8; colour = page1[index] = 0x7FFF - index.
        let px = bg.tile_rgba(&bg.tiles[0], &pal).unwrap();
        assert_eq!(px.len(), 256);
        let index = ((32usize * 256 + 16) % 251) as u16;
        assert_eq!(px[0], rgb555_to_rgba(0x7FFF - index));
        let index_last = (((32usize + 15) * 256 + 16 + 15) % 251) as u16;
        assert_eq!(px[255], rgb555_to_rgba(0x7FFF - index_last));
        // layer-2 tile is blended -> slot 3, a 16-bit page whose pixel (x, y) is
        // ((y % 32) << 5) | (x % 32); the tile's secondary source is (8, 9)
        let direct = bg.tile_rgba(&bg.tiles[2], &pal).unwrap();
        assert_eq!(direct.len(), 256);
        // (coordinates are below 32, so the modulo in the builder's formula is the identity)
        assert_eq!(direct[0], rgb555_to_rgba((9 << 5) | 8));
        assert_eq!(direct[17], rgb555_to_rgba((10 << 5) | 9)); // one row down, one right
        // missing texture slot / palette page / out-of-page source -> None
        let mut bad = bg.tiles[0];
        bad.texture_id = 1;
        assert!(bg.tile_rgba(&bad, &pal).is_none());
        let mut bad = bg.tiles[0];
        bad.palette_id = 9;
        assert!(bg.tile_rgba(&bad, &pal).is_none());
        let mut bad = bg.tiles[0];
        bad.src_x = 250;
        assert!(bg.tile_rgba(&bad, &pal).is_none());
    }

    #[test]
    fn depth_zero_pages_are_located_and_decoded_low_nibble_first() {
        // slot 1 is a depth-0 page (32768 bytes); slots after it must still be found
        let spec = FieldSpec { texture_slots: vec![(0, 1), (1, 0), (3, 2)], ..FieldSpec::default() };
        let f = parse(&spec);
        let bg = f.background().unwrap();
        assert!(bg.texture_error.is_none(), "{:?}", bg.texture_error);
        let zero = bg.texture(1).unwrap();
        assert_eq!((zero.depth, zero.pixels.len()), (0, 32_768));
        assert_eq!(bg.texture(3).unwrap().pixels.len(), 131_072);
        let pal = f.palette().unwrap();
        // tile 0 on the 4-bit page, source (16, 32), palette page 0 (colour = index). Page pixel
        // p = y * 256 + x is nibble (p & 1) of byte p / 2, and byte i holds `i % 253`.
        let mut tile = bg.tiles[0];
        tile.texture_id = 1;
        tile.palette_id = 0;
        let px = bg.tile_rgba(&tile, &pal).unwrap();
        assert_eq!(px.len(), 256);
        let nibble = |x: usize, y: usize| -> u16 {
            let p = (32 + y) * 256 + 16 + x;
            let byte = ((p / 2) % 253) as u16;
            if p % 2 == 0 { byte & 15 } else { byte >> 4 }
        };
        for (x, y) in [(0usize, 0usize), (1, 0), (2, 0), (15, 15), (7, 3)] {
            let index = nibble(x, y);
            let expect = pal.rgba(0, index as usize).unwrap();
            assert_eq!(px[y * 16 + x], expect, "pixel ({x}, {y}) index {index}");
        }
        // a palette page past the end is still reported as None
        tile.palette_id = 9;
        assert!(bg.tile_rgba(&tile, &pal).is_none());
    }

    #[test]
    fn background_end_tag_and_trailing_bytes() {
        // the synthetic section ends "END" + "FINAL FANTASY7" like the owned files
        let bg_of = |cut: usize| {
            let spec = FieldSpec { truncate_background_bytes: cut, ..FieldSpec::default() };
            let f = parse(&spec);
            let bg = f.background().unwrap();
            (bg.texture_error.is_none(), bg.end_tag, bg.trailing_bytes)
        };
        assert_eq!(bg_of(0), (true, true, 14));
        assert_eq!(bg_of(14), (true, true, 0)); // exactly the layout of 578 owned fields
        assert_eq!(bg_of(17), (true, false, 0)); // no tag at all
        assert_eq!(bg_of(15), (true, false, 2)); // a partial "EN" is not a tag
    }

    #[test]
    fn background_with_only_layer_one_and_no_textures() {
        let spec = FieldSpec { tiles: vec![TileSpec::default()], layer2: None, layer4: None, texture_slots: vec![], ..FieldSpec::default() };
        let f = parse(&spec);
        let bg = f.background().unwrap();
        assert_eq!(bg.tiles.len(), 1);
        assert_eq!(bg.layers[1], None);
        assert!(bg.texture_error.is_none());
        assert!(bg.textures.iter().all(Option::is_none));
    }

    #[test]
    fn background_texture_problems_do_not_lose_tiles() {
        // Corrupt the "TEXTURE" tag: tiles must still parse, textures report an error.
        let spec = FieldSpec { corrupt_texture_tag: true, ..FieldSpec::default() };
        let f = parse(&spec);
        let bg = f.background().unwrap();
        assert_eq!(bg.tiles.len(), 4);
        assert!(bg.textures.is_empty());
        assert!(matches!(bg.texture_error, Some(FieldError::Invalid { .. })));
        // unsupported depth
        let spec = FieldSpec { bad_texture_depth: true, ..FieldSpec::default() };
        let broken = parse(&spec);
        let bg = broken.background().unwrap();
        assert_eq!(bg.tiles.len(), 4);
        assert!(matches!(bg.texture_error, Some(FieldError::Invalid { .. })));
        // truncated texture data
        let spec = FieldSpec { truncate_background_bytes: 100, ..FieldSpec::default() };
        let f = parse(&spec);
        let bg = f.background().unwrap();
        assert!(matches!(bg.texture_error, Some(FieldError::Truncated { .. })));
    }

    #[test]
    fn background_rejects_structural_damage() {
        assert!(matches!(Background::parse(&[]), Err(FieldError::Truncated { .. })));
        let f = parse(&FieldSpec::default());
        let good = f.section(section::BACKGROUND).unwrap().to_vec();
        // bad PALETTE tag
        let mut b = good.clone();
        b[5] = b'X';
        assert!(matches!(Background::parse(&b), Err(FieldError::Invalid { .. })));
        // bad BACK tag
        let mut b = good.clone();
        b[0x24] = b'X';
        assert!(matches!(Background::parse(&b), Err(FieldError::Invalid { .. })));
        // layer-1 tile count far larger than the data
        let mut b = good.clone();
        b[0x2C..0x2E].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(Background::parse(&b), Err(FieldError::Truncated { .. })));
        // layer-2 flag that is neither 0 nor 1
        let flag2 = 0x32 + 2 * BG_TILE_RECORD_LEN + 2;
        let mut b = good.clone();
        b[flag2] = 7;
        assert!(matches!(Background::parse(&b), Err(FieldError::Invalid { .. })));
        // every prefix is an error or a partial success, never a panic
        for n in 0..good.len().min(2200) {
            let _ = Background::parse(&good[..n]);
        }
    }

    // ---- container damage ----

    #[test]
    fn rejects_bad_headers() {
        let good = FieldSpec::default().build();
        assert!(matches!(FieldFile::parse(vec![]), Err(FieldError::TooShort { len: 0 })));
        assert!(matches!(FieldFile::parse(good[..41].to_vec()), Err(FieldError::TooShort { len: 41 })));
        let mut b = good.clone();
        b[0] = 1; // blank must be zero
        assert!(matches!(FieldFile::parse(b), Err(FieldError::BadHeader(_))));
        for count in [0u32, 8, 10, u32::MAX] {
            let mut b = good.clone();
            b[2..6].copy_from_slice(&count.to_le_bytes());
            assert_eq!(FieldFile::parse(b).unwrap_err(), FieldError::BadSectionCount(count));
        }
    }

    #[test]
    fn rejects_bad_section_offsets() {
        let good = FieldSpec::default().build();
        let set = |i: usize, v: u32| {
            let mut b = good.clone();
            b[6 + 4 * i..10 + 4 * i].copy_from_slice(&v.to_le_bytes());
            b
        };
        // first section inside the header
        assert!(matches!(FieldFile::parse(set(0, 10)), Err(FieldError::BadSectionOffset { index: 0, .. })));
        // out of the file
        assert!(matches!(FieldFile::parse(set(3, u32::MAX)), Err(FieldError::BadSectionOffset { index: 3, .. })));
        assert!(matches!(FieldFile::parse(set(8, good.len() as u32)), Err(FieldError::BadSectionOffset { index: 8, .. })));
        // not increasing
        let first = u32::from_le_bytes(good[6..10].try_into().unwrap());
        assert!(matches!(FieldFile::parse(set(1, first)), Err(FieldError::BadSectionOffset { index: 1, .. })));
        // length field would run past the end of the file
        assert!(matches!(FieldFile::parse(set(8, good.len() as u32 - 3)), Err(FieldError::BadSectionOffset { .. })));
    }

    #[test]
    fn section_length_problems_surface_only_on_access() {
        let good = FieldSpec::default().build();
        let f = FieldFile::parse(good.clone()).unwrap();
        // Corrupt the walkmesh section's own length field.
        let walk_start = u32::from_le_bytes(good[6 + 4 * 4..10 + 4 * 4].try_into().unwrap()) as usize;
        let mut b = good.clone();
        b[walk_start..walk_start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let broken = FieldFile::parse(b).expect("header is still fine");
        assert!(matches!(broken.walkmesh(), Err(FieldError::SectionLength { index: 4, .. })));
        // the other sections still work
        assert!(broken.palette().is_ok());
        assert!(broken.triggers().is_ok());
        assert!(f.walkmesh().is_ok());
        // a declared length SHORTER than the span is fine (trailing slack is ignored): shrink
        // the script section (index 0), whose span is 32 bytes
        let script_start = u32::from_le_bytes(good[6..10].try_into().unwrap()) as usize;
        let mut b = good.clone();
        b[script_start..script_start + 4].copy_from_slice(&3u32.to_le_bytes());
        let shrunk = FieldFile::parse(b).unwrap();
        assert_eq!(shrunk.section(section::SCRIPT).unwrap().len(), 3);
        assert!(shrunk.walkmesh().is_ok());
    }

    #[test]
    fn walkmesh_rejects_bad_counts() {
        assert!(matches!(Walkmesh::parse(&[]), Err(FieldError::Truncated { .. })));
        assert!(matches!(Walkmesh::parse(&u32::MAX.to_le_bytes()), Err(FieldError::TooMany { .. })));
        assert!(matches!(Walkmesh::parse(&(MAX_TRIANGLES + 1).to_le_bytes()), Err(FieldError::TooMany { .. })));
        // count within the cap but data absent: truncated, with no big allocation
        assert!(matches!(Walkmesh::parse(&MAX_TRIANGLES.to_le_bytes()), Err(FieldError::Truncated { .. })));
        // count 3 but only 2 triangles worth of data
        let mut d = 3u32.to_le_bytes().to_vec();
        d.extend(vec![0u8; 30 * 2]);
        assert!(matches!(Walkmesh::parse(&d), Err(FieldError::Truncated { .. })));
        // zero triangles is a valid (empty) mesh at this layer
        assert!(Walkmesh::parse(&0u32.to_le_bytes()).unwrap().is_empty());
    }

    #[test]
    fn every_prefix_and_byte_flip_of_a_field_never_panics() {
        // One 64 KiB texture page keeps the clone-per-case loops cheap.
        let good = FieldSpec { texture_slots: vec![(0, 1)], ..FieldSpec::default() }.build();
        let exercise = |bytes: Vec<u8>| {
            if let Ok(f) = FieldFile::parse(bytes) {
                for i in 0..SECTION_COUNT {
                    let _ = f.section(i);
                }
                let _ = f.walkmesh();
                let _ = f.cameras();
                let _ = f.triggers();
                if let (Ok(pal), Ok(bg)) = (f.palette(), f.background()) {
                    for tile in &bg.tiles {
                        let _ = bg.tile_rgba(tile, &pal);
                    }
                }
            }
        };
        // Every prefix through the structured part of the file (header, small sections, tiles,
        // texture slot flags), then a sparse sample through the pixel data.
        let structured = good.len().min(2600);
        let mut lens: Vec<usize> = (0..structured).collect();
        lens.extend((structured..good.len()).step_by(4099));
        lens.push(good.len() - 1);
        for n in lens {
            exercise(good[..n].to_vec());
        }
        // Flip bytes in the same structured region (pixel data has no structure to break).
        let interesting = structured;
        for i in 0..interesting {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                exercise(b);
            }
        }
    }
}
