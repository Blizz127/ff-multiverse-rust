//! SYNTHETIC / TEST-ONLY data builders: tiny fake ISO9660 images and raw-sector wrappers.
//!
//! Nothing here is, or is derived from, game data, and nothing here is surfaced by the CLI.
//! It exists so tests (unit and integration) can build disc images from scratch.
//!
//! The builders only handle buffers they construct themselves, so indexing is fine here.
#![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use crate::iso9660::{IsoError, SectorSource, Volume, SECTOR_SIZE};

/// Volume label of the images [`build_iso`] makes. The importer treats an image whose label
/// starts with `SYNTHETIC` as FIXTURE content (never as a real game disc).
pub const SYNTHETIC_LABEL: &str = "SYNTHETIC_TEST";
/// A label that does NOT mark an image as synthetic. Tests that must drive the code path of a
/// real disc (serial table, sessions, family checks) with made-up bytes build their images with
/// it, via [`build_iso_labeled`]; the bytes are still test data and never game data.
pub const DISC_LABEL: &str = "FF7_DISC_TEST";

pub const TEST_PAYLOAD_A: &[u8] = b"SYNTHETIC-PAYLOAD-A: not game data";
pub const TEST_PAYLOAD_B: &[u8] = b"SYNTHETIC-PAYLOAD-B: also not game data, a little longer";

/// A cooked (2048-byte sector) image held in memory.
pub struct MemImage(pub Vec<u8>);

impl SectorSource for MemImage {
    fn sector_count(&self) -> u64 {
        (self.0.len() / SECTOR_SIZE) as u64
    }
    fn read_sector(&mut self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), IsoError> {
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(SECTOR_SIZE));
        match start.and_then(|s| self.0.get(s..s + SECTOR_SIZE)) {
            Some(src) => {
                out.copy_from_slice(src);
                Ok(())
            }
            None => Err(IsoError::OutOfRange {
                lba,
                sectors: self.sector_count(),
            }),
        }
    }
}

/// SYSTEM.CNF text for a synthetic disc whose boot executable is `exe` (e.g. `SCUS_941.63`).
pub fn system_cnf(exe: &str) -> Vec<u8> {
    format!("BOOT = cdrom:\\{exe};1\r\nTCB = 4\r\nEVENT = 16\r\nSTACK = 801FFF00\r\n").into_bytes()
}

/// A cue sheet for a single-track image.
pub fn cue_for(bin_name: &str, mode: &str) -> String {
    format!("FILE \"{bin_name}\" BINARY\r\n  TRACK 01 {mode}\r\n    INDEX 01 00:00:00\r\n")
}

enum Node {
    File(Vec<u8>),
    Dir(BTreeMap<String, Node>),
}

fn insert(dir: &mut BTreeMap<String, Node>, parts: &[&str], data: &[u8]) {
    match parts {
        [] => {}
        [name] => {
            dir.insert((*name).to_string(), Node::File(data.to_vec()));
        }
        [first, rest @ ..] => {
            let e = dir
                .entry((*first).to_string())
                .or_insert_with(|| Node::Dir(BTreeMap::new()));
            if let Node::Dir(d) = e {
                insert(d, rest, data);
            }
        }
    }
}

enum Child {
    File(usize),
    Dir(usize),
}

struct DirPlan {
    entries: Vec<(String, Child)>,
    parent: usize,
    lba: u32,
    size: u32,
}

fn plan_dir(
    map: &BTreeMap<String, Node>,
    parent: Option<usize>,
    dirs: &mut Vec<DirPlan>,
    files: &mut Vec<Vec<u8>>,
) -> usize {
    let me = dirs.len();
    dirs.push(DirPlan {
        entries: Vec::new(),
        parent: parent.unwrap_or(me),
        lba: 0,
        size: 0,
    });
    let mut entries = Vec::new();
    for (name, node) in map {
        match node {
            Node::File(data) => {
                files.push(data.clone());
                entries.push((name.clone(), Child::File(files.len() - 1)));
            }
            Node::Dir(sub) => {
                let idx = plan_dir(sub, Some(me), dirs, files);
                entries.push((name.clone(), Child::Dir(idx)));
            }
        }
    }
    dirs[me].entries = entries;
    me
}

fn both32(v: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&v.to_le_bytes());
    b[4..].copy_from_slice(&v.to_be_bytes());
    b
}

fn record(name: &[u8], lba: u32, size: u32, is_dir: bool) -> Vec<u8> {
    let mut len = 33 + name.len();
    if len % 2 == 1 {
        len += 1;
    }
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    r[2..10].copy_from_slice(&both32(lba));
    r[10..18].copy_from_slice(&both32(size));
    r[18..25].copy_from_slice(&[98, 1, 1, 0, 0, 0, 0]);
    r[25] = if is_dir { 2 } else { 0 };
    r[28..30].copy_from_slice(&1u16.to_le_bytes());
    r[30..32].copy_from_slice(&1u16.to_be_bytes());
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

fn push_record(out: &mut Vec<u8>, rec: &[u8]) {
    let in_sector = out.len() % SECTOR_SIZE;
    if in_sector + rec.len() > SECTOR_SIZE {
        out.resize(out.len() + (SECTOR_SIZE - in_sector), 0);
    }
    out.extend_from_slice(rec);
}

fn file_name_on_disc(name: &str) -> Vec<u8> {
    if name.contains('.') {
        format!("{name};1").into_bytes()
    } else {
        format!("{name}.;1").into_bytes()
    }
}

fn dir_bytes(dirs: &[DirPlan], file_lbas: &[(usize, u32)], idx: usize) -> Vec<u8> {
    let d = &dirs[idx];
    let parent = &dirs[d.parent];
    let mut out = Vec::new();
    push_record(&mut out, &record(&[0], d.lba, d.size, true));
    push_record(&mut out, &record(&[1], parent.lba, parent.size, true));
    for (name, child) in &d.entries {
        match child {
            Child::Dir(i) => push_record(
                &mut out,
                &record(name.as_bytes(), dirs[*i].lba, dirs[*i].size, true),
            ),
            Child::File(i) => {
                let (len, lba) = file_lbas[*i];
                push_record(
                    &mut out,
                    &record(&file_name_on_disc(name), lba, len as u32, false),
                );
            }
        }
    }
    let rem = out.len() % SECTOR_SIZE;
    if rem != 0 {
        out.resize(out.len() + SECTOR_SIZE - rem, 0);
    }
    out
}

/// [`build_iso_labeled`] with the [`SYNTHETIC_LABEL`] volume label.
pub fn build_iso(files: &[(&str, &[u8])]) -> Vec<u8> {
    build_iso_labeled(SYNTHETIC_LABEL, files)
}

/// Build a cooked 2048-byte-sector ISO9660 image. Paths use `/` and upper-case names, e.g.
/// `("FIELD/MD1STIN.DAT", bytes)`. Layout: system area, PVD (sector 16), terminator (17), then
/// directory extents (root at 18), then file data. `label` is the volume label (32 bytes at
/// most are stored).
pub fn build_iso_labeled(label: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tree: BTreeMap<String, Node> = BTreeMap::new();
    for (path, data) in files {
        let parts: Vec<&str> = path.split('/').collect();
        insert(&mut tree, &parts, data);
    }
    let mut dirs = Vec::new();
    let mut contents = Vec::new();
    plan_dir(&tree, None, &mut dirs, &mut contents);

    // pass 1: sizes (serialised size does not depend on the LBA values)
    let placeholder: Vec<(usize, u32)> = contents.iter().map(|c| (c.len(), 0)).collect();
    for i in 0..dirs.len() {
        dirs[i].size = dir_bytes(&dirs, &placeholder, i).len() as u32;
    }
    // LBAs: directories first (root at 18), then files
    let mut next = 18u32;
    for d in dirs.iter_mut() {
        d.lba = next;
        next += d.size / SECTOR_SIZE as u32;
    }
    let mut file_lbas = Vec::new();
    for c in &contents {
        file_lbas.push((c.len(), next));
        next += c.len().div_ceil(SECTOR_SIZE) as u32;
    }
    let total_sectors = next;

    let mut img = vec![0u8; total_sectors as usize * SECTOR_SIZE];
    // PVD
    {
        let p = &mut img[16 * SECTOR_SIZE..17 * SECTOR_SIZE];
        p[0] = 1;
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        p[8..40].copy_from_slice(&[b' '; 32]);
        p[40..72].copy_from_slice(&[b' '; 32]);
        let label = label.as_bytes();
        let label = &label[..label.len().min(32)];
        p[40..40 + label.len()].copy_from_slice(label);
        p[80..88].copy_from_slice(&both32(total_sectors));
        p[120..122].copy_from_slice(&1u16.to_le_bytes());
        p[122..124].copy_from_slice(&1u16.to_be_bytes());
        p[124..126].copy_from_slice(&1u16.to_le_bytes());
        p[126..128].copy_from_slice(&1u16.to_be_bytes());
        p[128..130].copy_from_slice(&2048u16.to_le_bytes());
        p[130..132].copy_from_slice(&2048u16.to_be_bytes());
        p[156..190].copy_from_slice(&record(&[0], dirs[0].lba, dirs[0].size, true));
        p[881] = 1;
    }
    // terminator
    {
        let t = &mut img[17 * SECTOR_SIZE..18 * SECTOR_SIZE];
        t[0] = 255;
        t[1..6].copy_from_slice(b"CD001");
        t[6] = 1;
    }
    // directories
    for i in 0..dirs.len() {
        let bytes = dir_bytes(&dirs, &file_lbas, i);
        let start = dirs[i].lba as usize * SECTOR_SIZE;
        img[start..start + bytes.len()].copy_from_slice(&bytes);
    }
    // file data
    for (c, (_, lba)) in contents.iter().zip(&file_lbas) {
        let start = *lba as usize * SECTOR_SIZE;
        img[start..start + c.len()].copy_from_slice(c);
    }
    img
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawMode {
    /// MODE1/2352: sync, header (mode 1), 2048 data, EDC/ECC area.
    Mode1,
    /// MODE2/2352 Form 1: sync, header (mode 2), 8-byte subheader (submode 0x08), 2048 data.
    Mode2Form1,
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Wrap a cooked image into raw 2352-byte sectors with realistic sync/header (BCD MSF with the
/// 150-frame offset) and, for Mode 2, a Form 1 subheader.
pub fn wrap_raw(iso: &[u8], mode: RawMode) -> Vec<u8> {
    let mut out = Vec::with_capacity(iso.len() / SECTOR_SIZE * 2352);
    for (n, data) in iso.chunks(SECTOR_SIZE).enumerate() {
        let mut s = vec![0u8; 2352];
        s[1..11].fill(0xff);
        let frames = n as u32 + 150;
        s[12] = bcd(frames / (60 * 75));
        s[13] = bcd(frames / 75 % 60);
        s[14] = bcd(frames % 75);
        match mode {
            RawMode::Mode1 => {
                s[15] = 1;
                s[16..16 + data.len()].copy_from_slice(data);
            }
            RawMode::Mode2Form1 => {
                s[15] = 2;
                s[16..24].copy_from_slice(&[0, 0, 0x08, 0, 0, 0, 0x08, 0]);
                s[24..24 + data.len()].copy_from_slice(data);
            }
        }
        out.extend_from_slice(&s);
    }
    out
}

/// Flip sector `lba` of a raw Mode 2 image to Form 2 (submode bit 0x20), as CD-XA stream data is.
pub fn mark_form2(raw: &mut [u8], lba: usize) {
    raw[lba * 2352 + 18] = 0x20;
    raw[lba * 2352 + 22] = 0x20;
}

/// LBA of `path` inside a cooked image (for tests that need to poke a specific file's sectors).
pub fn lba_of(iso: &[u8], path: &str) -> Option<u32> {
    let mut vol = Volume::open(MemImage(iso.to_vec())).ok()?;
    let walk = vol.walk().ok()?;
    walk.files.iter().find(|f| f.path == path).map(|f| f.lba)
}
