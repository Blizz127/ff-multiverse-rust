//! Test-only builders for synthetic LGP archives and field files.
//!
//! Everything here is invented data: no game bytes, only layouts as documented in the
//! module docs of `lgp` and `field`. Builders are deliberately independent of the parsers
//! (they write fields at explicit offsets) so the tests exercise the parsers against a
//! second, hand-written statement of the layout.

/// Build an LGP archive: header, table of contents, 3600-byte lookup table (zeros), an empty
/// path table, data entries, footer.
pub(crate) fn build_lgp(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let toc_end = 16 + entries.len() * 27;
    let data_start = toc_end + 3600 + 2;
    let mut offsets = Vec::new();
    let mut cursor = data_start;
    for (_, data) in entries {
        offsets.push(cursor);
        cursor += 24 + data.len();
    }
    let mut out = Vec::with_capacity(cursor + 14);
    out.extend_from_slice(b"\0\0SQUARESOFT");
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for ((name, _), offset) in entries.iter().zip(&offsets) {
        out.extend_from_slice(&padded_name(name));
        out.extend_from_slice(&(*offset as u32).to_le_bytes());
        out.push(0x0E);
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend(std::iter::repeat_n(0u8, 3600)); // lookup table (not used by the reader)
    out.extend_from_slice(&0u16.to_le_bytes()); // path table: zero groups
    for (name, data) in entries {
        out.extend_from_slice(&padded_name(name));
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out.extend_from_slice(b"FINAL FANTASY7");
    out
}

fn padded_name(name: &str) -> [u8; 20] {
    assert!(name.len() <= 19, "LGP names are at most 19 bytes + NUL");
    let mut n = [0u8; 20];
    n[..name.len()].copy_from_slice(name.as_bytes());
    n
}

/// One 38-byte camera record.
pub(crate) fn camera_record(axes: [[i16; 3]; 3], position: [i32; 3], zoom: u16) -> Vec<u8> {
    let mut r = Vec::with_capacity(38);
    for axis in axes {
        for c in axis {
            r.extend_from_slice(&c.to_le_bytes());
        }
    }
    r.extend_from_slice(&axes[2][2].to_le_bytes()); // duplicate of the Z axis' z
    for p in position {
        r.extend_from_slice(&p.to_le_bytes());
    }
    r.extend_from_slice(&0u32.to_le_bytes()); // blank
    r.extend_from_slice(&zoom.to_le_bytes());
    assert_eq!(r.len(), 38);
    r
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TileSpec {
    pub dst_x: i16,
    pub dst_y: i16,
    pub src_x: u8,
    pub src_y: u8,
    pub src_x2: u8,
    pub src_y2: u8,
    pub texture_id: u8,
    pub texture_id2: u8,
    pub palette_id: u8,
    pub blending: u8,
    pub type_trans: u8,
    pub depth: u8,
    pub id: u16,
    pub param: u8,
    pub state: u8,
}

impl TileSpec {
    /// 52-byte tile record written field by field at its documented offsets.
    fn record(&self) -> [u8; 52] {
        let mut r = [0u8; 52];
        r[2..4].copy_from_slice(&self.dst_x.to_le_bytes());
        r[4..6].copy_from_slice(&self.dst_y.to_le_bytes());
        r[10] = self.src_x;
        r[12] = self.src_y;
        r[14] = self.src_x2;
        r[16] = self.src_y2;
        r[18..20].copy_from_slice(&16u16.to_le_bytes());
        r[20..22].copy_from_slice(&16u16.to_le_bytes());
        r[22] = self.palette_id;
        r[24..26].copy_from_slice(&self.id.to_le_bytes());
        r[26] = self.param;
        r[27] = self.state;
        r[28] = self.blending;
        r[30] = self.type_trans;
        r[32] = self.texture_id;
        r[34] = self.texture_id2;
        r[36] = self.depth;
        r
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LayerSpec {
    pub width: u16,
    pub height: u16,
    pub tiles: Vec<TileSpec>,
}

/// Description of a synthetic field file. `Default` is a 100x100 square made of two triangles
/// on the plane z = x + y, one camera, two palette pages, a gateway/trigger table and a small
/// background (layers 1, 2 and 4; textures in slots 0 (8-bit) and 3 (16-bit)).
#[derive(Clone, Debug)]
pub(crate) struct FieldSpec {
    pub triangles: Vec<[(i16, i16, i16); 3]>,
    pub access: Vec<[u16; 3]>,
    pub l1_width: u16,
    pub l1_height: u16,
    pub tiles: Vec<TileSpec>,
    pub layer2: Option<LayerSpec>,
    pub layer3: Option<LayerSpec>,
    pub layer4: Option<LayerSpec>,
    /// `(slot, depth)` of the texture pages that exist.
    pub texture_slots: Vec<(usize, u16)>,
    pub corrupt_texture_tag: bool,
    pub bad_texture_depth: bool,
    /// Bytes to cut from the end of the background section.
    pub truncate_background_bytes: usize,
    /// Extra bytes appended to the script section (arbitrary content).
    pub script_len: usize,
}

impl Default for FieldSpec {
    fn default() -> Self {
        Self {
            triangles: vec![
                [(0, 0, 0), (100, 0, 100), (100, 100, 200)],
                [(0, 0, 0), (100, 100, 200), (0, 100, 100)],
            ],
            access: vec![[0xFFFF, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF]],
            l1_width: 320,
            l1_height: 240,
            tiles: vec![
                TileSpec { dst_x: -160, dst_y: -120, src_x: 16, src_y: 32, palette_id: 1, ..TileSpec::default() },
                TileSpec { dst_x: -144, dst_y: -120, src_x: 32, src_y: 32, palette_id: 1, ..TileSpec::default() },
            ],
            layer2: Some(LayerSpec {
                width: 640,
                height: 480,
                tiles: vec![TileSpec {
                    dst_x: 0,
                    dst_y: 16,
                    src_x: 1,
                    src_y: 2,
                    src_x2: 8,
                    src_y2: 9,
                    texture_id: 0,
                    texture_id2: 3,
                    blending: 1,
                    type_trans: 2,
                    id: 77,
                    param: 5,
                    state: 6,
                    ..TileSpec::default()
                }],
            }),
            layer3: None,
            layer4: Some(LayerSpec {
                width: 64,
                height: 64,
                tiles: vec![TileSpec { dst_x: 32, dst_y: 32, ..TileSpec::default() }],
            }),
            texture_slots: vec![(0, 1), (3, 2)],
            corrupt_texture_tag: false,
            bad_texture_depth: false,
            truncate_background_bytes: 0,
            script_len: 32,
        }
    }
}

impl FieldSpec {
    /// A spec with only the walkmesh differing from the default.
    pub(crate) fn with_mesh(triangles: Vec<[(i16, i16, i16); 3]>, access: Vec<[u16; 3]>) -> Self {
        Self { triangles, access, ..Self::default() }
    }

    /// The decompressed field file.
    pub(crate) fn build(&self) -> Vec<u8> {
        let sections: [Vec<u8>; 9] = [
            (0..self.script_len).map(|i| (i * 3 + 1) as u8).collect(), // script: arbitrary
            self.camera_section(),
            vec![0, 0, 0, 0, 0x10, 0x00], // model loader: always-zero, 0 models, scale
            self.palette_section(),
            self.walkmesh_section(),
            vec![0xEE; 8], // tilemap (unused on PC)
            vec![0u8; 48], // encounter: two 24-byte tables
            self.triggers_section(),
            self.background_section(),
        ];
        let table_end = 2 + 4 + 4 * 9;
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&9u32.to_le_bytes());
        let mut offset = table_end;
        for s in &sections {
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += 4 + s.len();
        }
        for s in &sections {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s);
        }
        out
    }

    fn camera_section(&self) -> Vec<u8> {
        camera_record([[4096, 0, 0], [0, -4096, 0], [0, 0, 4096]], [10, -20, 30], 0x0400)
    }

    fn palette_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u16.to_le_bytes()); // palX
        body.extend_from_slice(&480u16.to_le_bytes()); // palY
        body.extend_from_slice(&256u16.to_le_bytes()); // colours per page
        body.extend_from_slice(&2u16.to_le_bytes()); // pages
        for i in 0..256u16 {
            body.extend_from_slice(&i.to_le_bytes());
        }
        for i in 0..256u16 {
            body.extend_from_slice(&(0x7FFF - i).to_le_bytes());
        }
        let mut out = ((body.len() + 4) as u32).to_le_bytes().to_vec(); // duplicate length
        out.extend_from_slice(&body);
        out
    }

    fn walkmesh_section(&self) -> Vec<u8> {
        let n = self.triangles.len();
        let mut out = (n as u32).to_le_bytes().to_vec();
        for tri in &self.triangles {
            for &(x, y, z) in tri {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&z.to_le_bytes());
                out.extend_from_slice(&tri[0].2.to_le_bytes()); // "res" repeats v0.z
            }
        }
        for i in 0..n {
            let a = self.access.get(i).copied().unwrap_or([0xFFFF; 3]);
            for v in a {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    fn triggers_section(&self) -> Vec<u8> {
        let mut out = vec![0u8; 740];
        out[..7].copy_from_slice(b"testfld"); // 9-byte name field, NUL padded
        out[9] = 64; // control direction
        out[10..12].copy_from_slice(&(-12i16).to_le_bytes());
        for (i, v) in [-160i16, -120, 160, 120].iter().enumerate() {
            out[12 + 2 * i..14 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in [1024i16, 1024, 512, 256].iter().enumerate() {
            out[24 + 2 * i..26 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        // gateways: 12 x 24 at 56
        for g in 0..12 {
            let base = 56 + g * 24;
            let field_id: u16 = if g == 0 { 0x0123 } else { 0x7FFF };
            out[base + 18..base + 20].copy_from_slice(&field_id.to_le_bytes());
        }
        let g0 = 56;
        for (i, v) in [1i16, 2, 3, 4, 5, 6, 7, 8, 9].iter().enumerate() {
            out[g0 + 2 * i..g0 + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        out[g0 + 20..g0 + 24].copy_from_slice(&[0xAB; 4]);
        // triggers: 12 x 16 at 344; fill slot 2
        let t2 = 344 + 2 * 16;
        for (i, v) in [-1i16, -2, -3, 11, 12, 13].iter().enumerate() {
            out[t2 + 2 * i..t2 + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        out[t2 + 12..t2 + 16].copy_from_slice(&[3, 4, 2, 9]);
        out
    }

    fn layer_block(layer: &LayerSpec, unknown_len: usize, b: &mut Vec<u8>) {
        b.push(1); // present flag
        b.extend_from_slice(&layer.width.to_le_bytes());
        b.extend_from_slice(&layer.height.to_le_bytes());
        b.extend_from_slice(&(layer.tiles.len() as u16).to_le_bytes());
        b.extend(std::iter::repeat_n(0u8, unknown_len));
        b.extend_from_slice(&[0, 0]); // blank
        for t in &layer.tiles {
            b.extend_from_slice(&t.record());
        }
        b.extend_from_slice(&[0, 0]); // blank
    }

    fn background_section(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0u16.to_le_bytes()); // unknown
        b.extend_from_slice(&1u16.to_le_bytes()); // depth
        b.push(1); // unknown
        b.extend_from_slice(b"PALETTE");
        b.extend_from_slice(&0u32.to_le_bytes()); // size
        b.extend(std::iter::repeat_n(0u8, 8 + 12)); // palX/palY/w/h + 6 colours
        b.extend_from_slice(b"BACK");
        // layer 1 (no presence flag)
        b.extend_from_slice(&self.l1_width.to_le_bytes());
        b.extend_from_slice(&self.l1_height.to_le_bytes());
        b.extend_from_slice(&(self.tiles.len() as u16).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // depth
        b.extend_from_slice(&[0, 0]); // blank
        for t in &self.tiles {
            b.extend_from_slice(&t.record());
        }
        b.extend_from_slice(&[0, 0]); // blank
        for (layer, unknown_len) in [(&self.layer2, 16usize), (&self.layer3, 10), (&self.layer4, 10)] {
            match layer {
                None => b.push(0),
                Some(l) => Self::layer_block(l, unknown_len, &mut b),
            }
        }
        b.extend_from_slice(if self.corrupt_texture_tag { b"TEXTUR!" } else { b"TEXTURE" });
        let mut first = true;
        for slot in 0..42usize {
            match self.texture_slots.iter().find(|(s, _)| *s == slot) {
                None => b.extend_from_slice(&0u16.to_le_bytes()),
                Some(&(_, depth)) => {
                    b.extend_from_slice(&1u16.to_le_bytes()); // exists
                    b.extend_from_slice(&0u16.to_le_bytes()); // big-tile flag
                    let written_depth = if self.bad_texture_depth && first { 5 } else { depth };
                    first = false;
                    b.extend_from_slice(&written_depth.to_le_bytes());
                    // Pixel (x, y) is addressed as i = y * 256 + x. 8-bit pages hold `i % 251`;
                    // 16-bit pages hold `((y % 32) << 5) | (x % 32)`; depth-0 pages are 32768
                    // bytes of `i % 253`. All are periodic so the test-only compressor handles
                    // them quickly.
                    if depth == 0 {
                        for i in 0..32_768u32 {
                            b.push((i % 253) as u8);
                        }
                        continue;
                    }
                    for i in 0..65_536u32 {
                        let (x, y) = (i % 256, i / 256);
                        if depth == 1 {
                            b.push((i % 251) as u8);
                        } else {
                            b.extend_from_slice(&((((y % 32) << 5) | (x % 32)) as u16).to_le_bytes());
                        }
                    }
                }
            }
        }
        b.extend_from_slice(b"END");
        b.extend_from_slice(b"FINAL FANTASY7");
        let keep = b.len().saturating_sub(self.truncate_background_bytes);
        b.truncate(keep);
        b
    }
}
