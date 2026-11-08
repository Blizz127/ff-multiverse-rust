//! `.p`: the binary polygon model used for field characters (and for battle models and battle
//! scenery) in PC FF7.
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 4 180 `.p`
//! entries of `field/char.lgp`; the ignored tests `char_models_all_parse` and
//! `p_models_index_spaces_normals_winding_and_render_state` in `tests/real_data.rs` assert every
//! number quoted here. Measured facts:
//!
//! * Every file has version 1, `off04` 1 and vertex type 1 (LVERTEX: pre-lit vertices that carry a
//!   colour), exactly one vertex colour per vertex, and no "unknown1", "unknown2" or "unknown3"
//!   entries; polygons are always triangles (24-byte records whose first `u16` is zero in all
//!   156 551 of them).
//! * The documented section sizes, with **100 bytes per "hundred"** record (Makou Reactor's value;
//!   the FFRTT page gives none), reproduce each file's length exactly for the 8 files that have no
//!   bounding box and fall 4 bytes short for the 4 172 files that have one: the file ends with the
//!   normal index table (4 bytes per vertex) after bounding boxes of 28 bytes each (24 documented
//!   plus 4), or the 4 bytes sit elsewhere in that tail. Everything this module reads comes before
//!   those sections, so the tail is not needed and is not required to be present.
//! * The 4 875 groups: 4 180 untextured and 695 textured (texture numbers 0 to 4). Vertex indices
//!   of a polygon are **group-relative**: in all 4 875 groups every index is below the group's
//!   vertex count, and `vertex_start + index` is below the file's vertex count. A group's polygons
//!   are `polygons[polygon_start..polygon_start + num_polygons]` and none is missing. Texture
//!   coordinates are indexed like vertices with the group's `texcoord_start` as base; every corner
//!   of all 695 textured groups stays inside the texcoord array. Vertex colours use the vertex
//!   index.
//! * **Normal indices are absolute** (not group-relative): all three normal indices of every
//!   polygon are below the file's normal count. They are not the vertex indices either: of the
//!   2 426 groups in the 2 423 files that have as many normals as vertices, only 5 have normal index
//!   equal to `vertex_start + vertex index` (1 567 files have fewer normals than vertices, 190 more
//!   than vertices). Of the 96 701 stored normals 96 110 have length 1 (within 0.01), 268 are zero
//!   and 323 are something else, so a renderer that lights with them should normalise and ignore
//!   zero ones.
//! * Winding: `(v1 - v0) x (v2 - v0)`, computed on the raw coordinates, points **against** the
//!   first corner's stored normal for 152 710 of the 156 551 polygons (97.5%), with it for 3 393,
//!   and is degenerate (a zero-area triangle or a zero normal) for 448: the triangles are clockwise
//!   seen from the side the normal points to. [`crate::model`] swaps two corners for its output.
//! * `primitive_type` is 0 (67 groups), 1 (4 113), 2 (687) or 4 (8). It follows the texture flag
//!   exactly: untextured groups are 0 or 1 and textured groups 2 or 4. Its meaning is otherwise
//!   unknown and it is not used. The four `u` words of every group are zero.
//! * The render-state ("hundred") words are described at [`PRenderState`]: 23 groups blend.
//!
//! # Sources
//!
//! * FFRTT "FF7/P" <https://wiki.ffrtt.ru/index.php/FF7/P> (mirror: ff7-flat-wiki
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/P.html>): 128-byte header, section order, entry
//!   sizes, polygon and group records. The page does not give the size of a "hundred" or say how
//!   normals are indexed.
//! * Makou Reactor `src/core/field/PFile.cpp` <https://github.com/myst6re/makoureactor>: version
//!   and `off04` must be 1, vertex type at most 1, 100-byte hundreds, group-relative vertex and
//!   texcoord indexing, vertex colour per vertex. Only layout facts were taken.
//!
//! # Layout (all integers little-endian)
//!
//! ```text
//! 0x00 header, 16 x u32 then 64 bytes of run-time data (ignored):
//!        version(1) off04(1) vertex_type num_vertices num_normals num_unknown1 num_texcoords
//!        num_vertex_colors num_edges num_polygons num_unknown2 num_unknown3 num_hundreds
//!        num_groups num_bounding_boxes norm_index_table_flag
//! 0x80 vertices         num_vertices   x 12   f32 x, y, z
//!      normals          num_normals    x 12   f32 x, y, z
//!      unknown1         num_unknown1   x 12
//!      texcoords        num_texcoords  x  8   f32 u, v
//!      vertex colours   num_vertex_colors x 4 u8 b, g, r, a
//!      polygon colours  num_polygons   x  4   (not read)
//!      edges            num_edges      x  4   (not read)
//!      polygons         num_polygons   x 24   u16 zero, vertex[3], normal[3], edge[3], 2 x u16 junk
//!      unknown2         num_unknown2   x 24   (not read)
//!      unknown3         num_unknown3   x  3   (not read)
//!      hundreds         num_hundreds   x 100  25 x u32 render state, see PRenderState
//!      groups           num_groups     x 56   14 x u32, see PGroup
//!      bounding boxes, normal index table: not read
//! ```
//!
//! # Safety against untrusted input
//!
//! Every section is located with checked `u64` arithmetic and must lie inside the file before
//! any buffer is allocated, so allocations are bounded by the file size; no count is trusted.

use crate::model::ModelError;
use crate::util::{finite_f32_at, u16_at, u32_at};

/// Bytes of the fixed header (64 bytes of fields plus 64 bytes of run-time data).
pub const P_HEADER_LEN: usize = 128;
/// Bytes of one "hundred" (render-state) record.
pub const P_HUNDRED_LEN: usize = 100;
/// Bytes of one group record.
pub const P_GROUP_LEN: usize = 56;
/// Bytes of one polygon record.
pub const P_POLYGON_LEN: usize = 24;
/// Largest `.p` accepted (the archive reader's own per-entry cap).
pub const MAX_P_BYTES: usize = crate::lgp::MAX_ENTRY_LEN as usize;

/// One triangle: group-relative vertex indices and absolute normal indices (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PPolygon {
    pub vertex: [u16; 3],
    pub normal: [u16; 3],
}

/// One group (a run of polygons sharing a texture and render state), a decoded 56-byte record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PGroup {
    /// Stored as found; the meaning is unknown (0, 1, 2 and 4 occur) and it is not used.
    pub primitive_type: u32,
    /// Absolute index of the group's first polygon.
    pub polygon_start: u32,
    pub num_polygons: u32,
    /// Base added to the group-relative vertex indices of its polygons (also indexes the vertex
    /// colours).
    pub vertex_start: u32,
    pub num_vertices: u32,
    /// Base added to the group-relative vertex indices to index the texture coordinates.
    pub texcoord_start: u32,
    /// The "textures used" flag: the group is drawn with a texture.
    pub textured: bool,
    /// Which of the owning RSD's textures the group uses (meaningful when `textured`).
    pub texture_number: u32,
}

/// The blending part of a group's 100-byte render-state ("hundred") record, read from fixed word
/// positions. **The interpretation is UNVERIFIED**: no document describes the record, the values
/// were found by comparing columns across all 4 875 groups, and they match Direct3D enums.
///
/// In the owned data word 2 is `0x3_8200` (plain) or `0x3_820E` (textured), plus bit `0x400` for
/// exactly the 23 groups whose word 23 is below 255; words 13 and 14 are `(2, 1)` (D3D
/// source `ONE`, destination `ZERO`: opaque) for 4 852 groups and `(5, 6)`, `(5, 2)` (source
/// `SRCALPHA` with destination `INVSRCALPHA` or `ONE`: alpha or additive blending) for the 23.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PRenderState {
    /// Word 2 bit `0x400`: blending is on.
    pub blend: bool,
    /// Word 13 (a Direct3D blend factor enum value, 2 = ONE, 5 = SRCALPHA).
    pub src_blend: u32,
    /// Word 14 (1 = ZERO, 2 = ONE, 6 = INVSRCALPHA).
    pub dst_blend: u32,
    /// Word 23: constant alpha (255 = opaque; 128 and 64 occur).
    pub alpha: u8,
}

impl PRenderState {
    /// What an opaque group stores (used for groups that have no record).
    pub const OPAQUE: Self = Self { blend: false, src_blend: 2, dst_blend: 1, alpha: 255 };
}

/// A decoded `.p` model. See the module docs for the layout and what was verified.
#[derive(Clone, PartialEq)]
pub struct PModel {
    /// Vertex positions in the model's own units (as stored; no scale applied).
    pub vertices: Vec<[f32; 3]>,
    /// Unit normals (absolute index space, see [`PPolygon::normal`]); may be shorter or longer
    /// than `vertices`.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates `[u, v]` (0..1 across the texture).
    pub texcoords: Vec<[f32; 2]>,
    /// One RGBA colour per vertex (stored as B, G, R, A).
    pub vertex_colors: Vec<[u8; 4]>,
    pub polygons: Vec<PPolygon>,
    pub groups: Vec<PGroup>,
    /// One render state per hundred record (the owned data has one per group, in group order).
    pub render_states: Vec<PRenderState>,
}

/// `Debug` prints counts only: decoded game geometry must not leak into logs by accident.
impl std::fmt::Debug for PModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PModel")
            .field("vertices", &self.vertices.len())
            .field("normals", &self.normals.len())
            .field("polygons", &self.polygons.len())
            .field("groups", &self.groups.len())
            .finish_non_exhaustive()
    }
}

/// One corner of a triangle with every index made absolute and checked against the arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PCorner {
    /// Index into [`PModel::vertices`] and [`PModel::vertex_colors`].
    pub vertex: usize,
    /// Index into [`PModel::normals`], if in range.
    pub normal: Option<usize>,
    /// Index into [`PModel::texcoords`], if the group is textured and the index is in range.
    pub texcoord: Option<usize>,
}

/// The triangles of one group, resolved. `dropped` counts triangles left out because a corner's
/// vertex (or vertex colour) index was out of range or the polygon lies past the polygon array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupTriangles {
    /// Corners in file order (clockwise seen from the normal's side, see the module docs).
    pub triangles: Vec<[PCorner; 3]>,
    pub dropped: usize,
}

fn truncated(what: &'static str, at: usize, needed: usize) -> ModelError {
    ModelError::Truncated { what, at, needed }
}

impl PModel {
    /// Parse a `.p` file.
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() > MAX_P_BYTES {
            return Err(ModelError::TooMany { what: "p bytes", count: bytes.len() as u64, limit: MAX_P_BYTES as u64 });
        }
        if bytes.len() < P_HEADER_LEN {
            return Err(truncated("p header", 0, P_HEADER_LEN - bytes.len()));
        }
        let word = |i: usize| u32_at(bytes, i * 4).ok_or_else(|| truncated("p header", i * 4, 4));
        let (version, off04, vertex_type) = (word(0)?, word(1)?, word(2)?);
        if version != 1 || off04 != 1 || vertex_type > 1 {
            return Err(ModelError::Invalid {
                what: "p header",
                why: format!("version {version}, off04 {off04}, vertex type {vertex_type} (expected 1, 1, 0 or 1)"),
            });
        }
        let (n_vertices, n_normals, n_unknown1, n_texcoords) = (word(3)?, word(4)?, word(5)?, word(6)?);
        let (n_colors, n_edges, n_polygons, n_unknown2) = (word(7)?, word(8)?, word(9)?, word(10)?);
        let (n_unknown3, n_hundreds, n_groups) = (word(11)?, word(12)?, word(13)?);

        // Locate every section with checked u64 arithmetic; each must fit inside the file.
        let len = bytes.len() as u64;
        let mut pos = P_HEADER_LEN as u64;
        let mut section = |what: &'static str, count: u32, entry: usize| -> Result<usize, ModelError> {
            let start = pos;
            let size = u64::from(count) * entry as u64; // < 2^32 * 128, cannot overflow u64
            let end = start.checked_add(size).filter(|&e| e <= len).ok_or_else(|| ModelError::Invalid {
                what,
                why: format!("{count} entries of {entry} bytes do not fit in the {len}-byte file"),
            })?;
            pos = end;
            usize::try_from(start).map_err(|_| truncated(what, 0, 0))
        };
        let at_vertices = section("p vertices", n_vertices, 12)?;
        let at_normals = section("p normals", n_normals, 12)?;
        section("p unknown1", n_unknown1, 12)?;
        let at_texcoords = section("p texcoords", n_texcoords, 8)?;
        let at_colors = section("p vertex colours", n_colors, 4)?;
        section("p polygon colours", n_polygons, 4)?;
        section("p edges", n_edges, 4)?;
        let at_polygons = section("p polygons", n_polygons, P_POLYGON_LEN)?;
        section("p unknown2", n_unknown2, 24)?;
        section("p unknown3", n_unknown3, 3)?;
        let at_hundreds = section("p hundreds", n_hundreds, P_HUNDRED_LEN)?;
        let at_groups = section("p groups", n_groups, P_GROUP_LEN)?;

        let float3 = |at: usize, i: usize| -> Option<[f32; 3]> {
            let o = at.checked_add(i.checked_mul(12)?)?;
            Some([finite_f32_at(bytes, o)?, finite_f32_at(bytes, o + 4)?, finite_f32_at(bytes, o + 8)?])
        };
        let vertices = (0..n_vertices as usize).map(|i| float3(at_vertices, i)).collect::<Option<Vec<_>>>();
        let normals = (0..n_normals as usize).map(|i| float3(at_normals, i)).collect::<Option<Vec<_>>>();
        let texcoords = (0..n_texcoords as usize)
            .map(|i| {
                let o = at_texcoords.checked_add(i.checked_mul(8)?)?;
                Some([finite_f32_at(bytes, o)?, finite_f32_at(bytes, o + 4)?])
            })
            .collect::<Option<Vec<_>>>();
        let vertex_colors = (0..n_colors as usize)
            .map(|i| {
                let o = at_colors.checked_add(i.checked_mul(4)?)?;
                let px = bytes.get(o..o.checked_add(4)?)?;
                match px {
                    [b, g, r, a] => Some([*r, *g, *b, *a]),
                    _ => None,
                }
            })
            .collect::<Option<Vec<_>>>();
        let polygons = (0..n_polygons as usize)
            .map(|i| {
                let o = at_polygons.checked_add(i.checked_mul(P_POLYGON_LEN)?)?;
                let h = |k: usize| u16_at(bytes, o.checked_add(2 * k)?);
                Some(PPolygon { vertex: [h(1)?, h(2)?, h(3)?], normal: [h(4)?, h(5)?, h(6)?] })
            })
            .collect::<Option<Vec<_>>>();
        let groups = (0..n_groups as usize)
            .map(|i| {
                let o = at_groups.checked_add(i.checked_mul(P_GROUP_LEN)?)?;
                let d = |k: usize| u32_at(bytes, o.checked_add(4 * k)?);
                Some(PGroup {
                    primitive_type: d(0)?,
                    polygon_start: d(1)?,
                    num_polygons: d(2)?,
                    vertex_start: d(3)?,
                    num_vertices: d(4)?,
                    texcoord_start: d(11)?,
                    textured: d(12)? == 1,
                    texture_number: d(13)?,
                })
            })
            .collect::<Option<Vec<_>>>();
        let render_states = (0..n_hundreds as usize)
            .map(|i| {
                let o = at_hundreds.checked_add(i.checked_mul(P_HUNDRED_LEN)?)?;
                let d = |k: usize| u32_at(bytes, o.checked_add(4 * k)?);
                Some(PRenderState {
                    blend: d(2)? & 0x400 != 0,
                    src_blend: d(13)?,
                    dst_blend: d(14)?,
                    alpha: u8::try_from(d(23)?.min(255)).unwrap_or(255),
                })
            })
            .collect::<Option<Vec<_>>>();

        // Every section was checked to lie inside the file, so the reads above cannot fail; stay
        // total anyway.
        match (vertices, normals, texcoords, vertex_colors, polygons, groups, render_states) {
            (
                Some(vertices),
                Some(normals),
                Some(texcoords),
                Some(vertex_colors),
                Some(polygons),
                Some(groups),
                Some(render_states),
            ) => Ok(Self { vertices, normals, texcoords, vertex_colors, polygons, groups, render_states }),
            _ => Err(truncated("p sections", 0, 0)),
        }
    }

    /// The render state of group `index` ([`PRenderState::OPAQUE`] if the file has none for it).
    pub fn render_state(&self, group: usize) -> PRenderState {
        self.render_states.get(group).copied().unwrap_or(PRenderState::OPAQUE)
    }

    /// Resolve the triangles of `group` to absolute indices (see [`PCorner`]); triangles with an
    /// out-of-range vertex are dropped and counted.
    pub fn group_triangles(&self, group: &PGroup) -> GroupTriangles {
        let mut triangles = Vec::new();
        let mut dropped = 0usize;
        let start = group.polygon_start as usize;
        let wanted = group.num_polygons as usize;
        let available = self.polygons.len().saturating_sub(start).min(wanted);
        dropped += wanted - available; // polygons the group claims but the file does not have (wanted >= available)
        let polys = self.polygons.get(start..start.saturating_add(available)).unwrap_or(&[]);
        for poly in polys {
            let corner = |k: usize| -> Option<PCorner> {
                let v = usize::from(*poly.vertex.get(k)?);
                let vertex = (group.vertex_start as usize).checked_add(v)?;
                if vertex >= self.vertices.len() || vertex >= self.vertex_colors.len() {
                    return None;
                }
                let normal = poly.normal.get(k).map(|&n| usize::from(n)).filter(|&n| n < self.normals.len());
                let texcoord = if group.textured {
                    (group.texcoord_start as usize).checked_add(v).filter(|&t| t < self.texcoords.len())
                } else {
                    None
                };
                Some(PCorner { vertex, normal, texcoord })
            };
            match (corner(0), corner(1), corner(2)) {
                (Some(a), Some(b), Some(c)) => triangles.push([a, b, c]),
                _ => dropped += 1,
            }
        }
        GroupTriangles { triangles, dropped }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic model in the documented layout, built field by field (independent of the
    /// parser): a unit square split into two triangles in two groups (one textured), with
    /// distinct group bases so that group-relative indexing is exercised.
    pub(crate) struct PSpec {
        pub vertices: Vec<[f32; 3]>,
        pub normals: Vec<[f32; 3]>,
        pub texcoords: Vec<[f32; 2]>,
        pub colors: Vec<[u8; 4]>, // stored order: b, g, r, a
        /// (vertex[3] group-relative, normal[3] absolute)
        pub polygons: Vec<([u16; 3], [u16; 3])>,
        /// (polygon_start, num_polygons, vertex_start, num_vertices, texcoord_start, textured, texture_number)
        pub groups: Vec<(u32, u32, u32, u32, u32, bool, u32)>,
        pub bounding_box: bool,
    }

    impl Default for PSpec {
        fn default() -> Self {
            Self {
                vertices: vec![
                    [0.0, 0.0, 0.0],
                    [10.0, 0.0, 0.0],
                    [10.0, 10.0, 0.0],
                    [0.0, 10.0, 0.0],
                    [20.0, 0.0, 5.0],
                    [20.0, 10.0, 5.0],
                ],
                normals: vec![[0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
                texcoords: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                colors: vec![
                    [1, 2, 3, 255],
                    [4, 5, 6, 255],
                    [7, 8, 9, 255],
                    [10, 11, 12, 255],
                    [13, 14, 15, 128],
                    [16, 17, 18, 255],
                ],
                polygons: vec![([0, 1, 2], [0, 0, 0]), ([0, 2, 3], [0, 0, 0]), ([0, 1, 2], [1, 1, 1])],
                // group 0: polygons 0..2 on vertices 0..4 with texcoords 0..4; group 1: polygon 2 on vertices 3..6
                groups: vec![(0, 2, 0, 4, 0, true, 1), (2, 1, 3, 3, 0, false, 0)],
                bounding_box: true,
            }
        }
    }

    impl PSpec {
        pub(crate) fn build(&self) -> Vec<u8> {
            let mut out = Vec::new();
            let words: [u32; 16] = [
                1,
                1,
                1,
                self.vertices.len() as u32,
                self.normals.len() as u32,
                0,
                self.texcoords.len() as u32,
                self.colors.len() as u32,
                0,
                self.polygons.len() as u32,
                0,
                0,
                self.groups.len() as u32, // hundreds: one per group
                self.groups.len() as u32,
                u32::from(self.bounding_box),
                1,
            ];
            for w in words {
                out.extend_from_slice(&w.to_le_bytes());
            }
            out.extend(std::iter::repeat_n(0xAB, 64)); // run-time data
            for v in &self.vertices {
                v.iter().for_each(|c| out.extend_from_slice(&c.to_le_bytes()));
            }
            for n in &self.normals {
                n.iter().for_each(|c| out.extend_from_slice(&c.to_le_bytes()));
            }
            for t in &self.texcoords {
                t.iter().for_each(|c| out.extend_from_slice(&c.to_le_bytes()));
            }
            for c in &self.colors {
                out.extend_from_slice(c);
            }
            out.extend(std::iter::repeat_n(0u8, 4 * self.polygons.len())); // polygon colours
            for (vertex, normal) in &self.polygons {
                out.extend_from_slice(&0u16.to_le_bytes());
                vertex.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
                normal.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
                out.extend(std::iter::repeat_n(0x77u8, 10)); // edges + junk
            }
            for i in 0..self.groups.len() {
                // hundred i: word 2 flags (blend on for group 1), words 13/14 blend factors, word 23 alpha
                let mut h = [0u32; 25];
                h[2] = 0x3_8200 | if i == 1 { 0x400 } else { 0 };
                h[13] = if i == 1 { 5 } else { 2 };
                h[14] = if i == 1 { 6 } else { 1 };
                h[23] = if i == 1 { 128 } else { 255 };
                for w in h {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
            for &(poly_start, n_polys, vertex_start, n_vertices, tc_start, textured, tex) in &self.groups {
                let words: [u32; 14] = [
                    1,
                    poly_start,
                    n_polys,
                    vertex_start,
                    n_vertices,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    tc_start,
                    u32::from(textured),
                    tex,
                ];
                for w in words {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
            if self.bounding_box {
                out.extend(std::iter::repeat_n(0u8, 28)); // box + 4 extra bytes, as in the owned data
            }
            out
        }
    }

    #[test]
    fn parses_the_documented_layout_with_group_relative_indexing() {
        let spec = PSpec::default();
        let p = PModel::parse(&spec.build()).unwrap();
        assert_eq!((p.vertices.len(), p.normals.len(), p.texcoords.len(), p.vertex_colors.len()), (6, 2, 4, 6));
        assert_eq!(p.vertices[4], [20.0, 0.0, 5.0]);
        assert_eq!(p.vertex_colors[0], [3, 2, 1, 255], "stored B, G, R, A becomes R, G, B, A");
        assert_eq!(p.vertex_colors[4], [15, 14, 13, 128]);
        assert_eq!(p.polygons.len(), 3);
        assert_eq!(p.polygons[2], PPolygon { vertex: [0, 1, 2], normal: [1, 1, 1] });
        assert_eq!(p.groups.len(), 2);
        assert_eq!(
            p.groups[0],
            PGroup {
                primitive_type: 1,
                polygon_start: 0,
                num_polygons: 2,
                vertex_start: 0,
                num_vertices: 4,
                texcoord_start: 0,
                textured: true,
                texture_number: 1
            }
        );
        assert!(!p.groups[1].textured);
        // resolved triangles: group 1's polygon indexes vertices 3, 4, 5 through its base of 3
        let g0 = p.group_triangles(&p.groups[0]);
        assert_eq!(g0.dropped, 0);
        assert_eq!(g0.triangles.len(), 2);
        assert_eq!(g0.triangles[1].map(|c| c.vertex), [0, 2, 3]);
        assert_eq!(g0.triangles[1].map(|c| c.texcoord), [Some(0), Some(2), Some(3)]);
        assert_eq!(g0.triangles[0].map(|c| c.normal), [Some(0); 3]);
        let g1 = p.group_triangles(&p.groups[1]);
        assert_eq!(g1.triangles.len(), 1);
        assert_eq!(g1.triangles[0].map(|c| c.vertex), [3, 4, 5]);
        assert_eq!(g1.triangles[0].map(|c| c.texcoord), [None; 3], "untextured groups have no texcoords");
        assert_eq!(g1.triangles[0].map(|c| c.normal), [Some(1); 3], "normal indices are absolute");
        // render states: group 1 blends
        assert_eq!(p.render_state(0), PRenderState::OPAQUE);
        assert_eq!(p.render_state(1), PRenderState { blend: true, src_blend: 5, dst_blend: 6, alpha: 128 });
        assert_eq!(p.render_state(9), PRenderState::OPAQUE);
        assert!(format!("{p:?}").contains("polygons"));
    }

    #[test]
    fn the_bounding_box_tail_is_optional() {
        let with = PSpec::default().build();
        let without = PSpec { bounding_box: false, ..PSpec::default() }.build();
        assert!(with.len() > without.len());
        assert_eq!(PModel::parse(&with).unwrap(), PModel::parse(&without).unwrap());
        // cutting everything after the groups changes nothing; cutting into the groups is an error
        let groups_end = without.len();
        assert!(PModel::parse(without.get(..groups_end).unwrap()).is_ok());
        assert!(PModel::parse(without.get(..groups_end - 1).unwrap()).is_err());
    }

    #[test]
    fn out_of_range_indices_drop_triangles_instead_of_panicking() {
        let spec = PSpec {
            polygons: vec![([0, 1, 2], [0, 0, 0]), ([0, 1, 200], [0, 0, 0]), ([0, 1, 2], [0, 9, 9])],
            groups: vec![
                (0, 3, 0, 4, 0, true, 0),
                (2, 5, 0, 4, 2, true, 0),
                (7, 1, 0, 4, 0, false, 0),
                (0, 1, 5, 4, 0, true, 0),
            ],
            ..PSpec::default()
        };
        let p = PModel::parse(&spec.build()).unwrap();
        let g = p.group_triangles(&p.groups[0]);
        assert_eq!((g.triangles.len(), g.dropped), (2, 1), "the polygon with vertex 200 is dropped");
        assert_eq!(g.triangles[1].map(|c| c.normal), [Some(0), None, None], "a bad normal index only loses the normal");
        // a group that claims more polygons than exist: the rest are counted as dropped
        let g = p.group_triangles(&p.groups[1]);
        assert_eq!((g.triangles.len(), g.dropped), (1, 4));
        // a group that starts past the polygon array
        let g = p.group_triangles(&p.groups[2]);
        assert_eq!((g.triangles.len(), g.dropped), (0, 1));
        // a vertex base that pushes a corner past the vertex array
        let g = p.group_triangles(&p.groups[3]);
        assert_eq!((g.triangles.len(), g.dropped), (0, 1));
        // texcoord base that pushes indices out of range: the triangle stays, texcoords go
        let spec = PSpec { groups: vec![(0, 1, 0, 4, 3, true, 0)], ..PSpec::default() };
        let p = PModel::parse(&spec.build()).unwrap();
        assert_eq!(p.group_triangles(&p.groups[0]).triangles[0].map(|c| c.texcoord), [Some(3), None, None]);
        // an absurd group never allocates or loops over the claimed size
        let huge = PGroup { polygon_start: u32::MAX, num_polygons: u32::MAX, vertex_start: u32::MAX, ..p.groups[0] };
        assert_eq!(p.group_triangles(&huge).triangles.len(), 0);
    }

    #[test]
    fn rejects_bad_headers_and_oversized_counts_before_allocating() {
        let good = PSpec::default().build();
        assert!(matches!(PModel::parse(&[]), Err(ModelError::Truncated { .. })));
        assert!(matches!(PModel::parse(good.get(..127).unwrap()), Err(ModelError::Truncated { .. })));
        for (word, value) in [(0usize, 2u32), (1, 0), (2, 2)] {
            let mut b = good.clone();
            b[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
            assert!(matches!(PModel::parse(&b), Err(ModelError::Invalid { .. })), "word {word} = {value}");
        }
        // a count of four billion in any section cannot fit the file
        for word in 3..14 {
            let mut b = good.clone();
            b[word * 4..word * 4 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(PModel::parse(&b).is_err(), "word {word}");
        }
        assert!(matches!(PModel::parse(&vec![0u8; MAX_P_BYTES + 1]), Err(ModelError::TooMany { .. })));
    }

    #[test]
    fn non_finite_floats_become_zero() {
        let mut spec = PSpec::default();
        spec.vertices[0] = [f32::NAN, f32::INFINITY, -f32::INFINITY];
        spec.texcoords[0] = [f32::NAN, 0.5];
        let p = PModel::parse(&spec.build()).unwrap();
        assert_eq!(p.vertices[0], [0.0, 0.0, 0.0]);
        assert_eq!(p.texcoords[0], [0.0, 0.5]);
    }

    #[test]
    fn damaged_files_never_panic() {
        let good = PSpec::default().build();
        for n in 0..good.len() {
            let _ = PModel::parse(good.get(..n).unwrap_or(&[]));
        }
        for i in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                if let Ok(p) = PModel::parse(&b) {
                    for g in &p.groups {
                        let _ = p.group_triangles(g);
                    }
                }
            }
        }
    }
}
