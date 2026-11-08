//! ISO9660 reader over a 2048-byte-sector source.
//!
//! Everything read from a disc image is untrusted. This module never indexes, unwraps or
//! trusts a length from the image: every slice goes through `get`, every extent is checked
//! against the real number of sectors in the image (not the volume size the PVD claims),
//! directory sizes, nesting depth, entry counts, directory counts, total directory bytes and
//! path lengths are bounded, and directory loops are cut.
//!
//! Two kinds of trouble are told apart while walking:
//! - a *format fault* (a malformed record, an unreadable extent) in a nested directory only
//!   costs that directory: it is reported in [`WalkResult::skipped`] and the walk goes on. A
//!   fault in the root directory leaves nothing to list, so it fails the whole walk;
//! - a *limit breach* (too many records, directories or directory bytes) always fails the whole
//!   walk, at any depth: those limits are what keeps a crafted image from growing memory or
//!   time with its size, so they must not be skippable.

use std::collections::HashSet;
use std::io::{self, Read};

pub const SECTOR_SIZE: usize = 2048;
/// Largest directory extent that will be read (real directories are a few sectors).
pub const MAX_DIR_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_DEPTH: usize = 32;
/// Most directory records (files, directories and skipped names, over ALL directories of the
/// image) the walker will look at.
pub const MAX_ENTRIES: usize = 200_000;
/// Most directories the walker will read.
pub const MAX_DIRS: usize = 20_000;
/// Most directory bytes the walker will read in total (a real disc has tens of KiB).
pub const MAX_TOTAL_DIR_BYTES: u64 = 256 * 1024 * 1024;
/// Longest path (bytes) the walker will list or descend into. Longer ones are reported as
/// skipped: they could not be stored in the library anyway (its limit is 1024 bytes including
/// the `disc<N>/` prefix), and the cap bounds the memory a crafted tree can make us hold.
pub const MAX_PATH_LEN: usize = 512;
const MIN_RECORD: usize = 34;

/// The walker's resource limits. [`Volume::walk`] uses the defaults (the constants above);
/// tests pass small ones to exercise the checks cheaply.
#[derive(Debug, Clone, Copy)]
pub struct WalkLimits {
    pub max_entries: usize,
    pub max_dirs: usize,
    pub max_total_dir_bytes: u64,
    pub max_path_len: usize,
}

impl Default for WalkLimits {
    fn default() -> Self {
        WalkLimits {
            max_entries: MAX_ENTRIES,
            max_dirs: MAX_DIRS,
            max_total_dir_bytes: MAX_TOTAL_DIR_BYTES,
            max_path_len: MAX_PATH_LEN,
        }
    }
}

/// A Mode 2 Form 2 sector (CD-XA audio/video stream data) cannot be read as 2048 bytes of file
/// data. It is carried inside an `io::Error` so callers of [`ExtentReader`] can recognise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form2Sector {
    pub lba: u64,
}

impl std::fmt::Display for Form2Sector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sector {} is Mode 2 Form 2 (XA stream) data", self.lba)
    }
}

impl std::error::Error for Form2Sector {}

#[derive(Debug, thiserror::Error)]
pub enum IsoError {
    #[error("I/O error reading the image: {0}")]
    Io(#[from] io::Error),
    #[error("sector {lba} is outside the image ({sectors} sectors)")]
    OutOfRange { lba: u64, sectors: u64 },
    #[error("not an ISO9660 image: {0}")]
    NotIso(&'static str),
    #[error("unsupported or corrupt directory structure: {0}")]
    BadDirectory(&'static str),
    /// A resource limit of the walker was exceeded (see the module docs): always fatal.
    #[error("the image exceeds a safety limit of the importer: {0}")]
    LimitExceeded(&'static str),
    #[error("{0}")]
    Form2(Form2Sector),
    #[error("sector {lba} of the raw image is malformed ({why})")]
    BadRawSector { lba: u64, why: &'static str },
    #[error("file is larger than the allowed {limit} bytes")]
    TooBig { limit: u64 },
}

impl From<IsoError> for io::Error {
    fn from(e: IsoError) -> io::Error {
        match e {
            IsoError::Io(e) => e,
            IsoError::Form2(f) => io::Error::new(io::ErrorKind::InvalidData, f),
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

/// Anything that can hand out the 2048 user-data bytes of a sector.
pub trait SectorSource {
    /// Number of addressable sectors.
    fn sector_count(&self) -> u64;
    fn read_sector(&mut self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), IsoError>;
}

/// One file found on the volume. `path` has no `;1` version suffix and uses `/` separators;
/// the case is whatever the disc stores (normally upper case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoFile {
    pub path: String,
    pub lba: u32,
    pub size: u32,
}

/// An entry the walker refused to list, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkippedEntry {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct WalkResult {
    /// Sorted by path.
    pub files: Vec<IsoFile>,
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Debug, Clone)]
struct Record {
    len: usize,
    lba: u32,
    size: u32,
    flags: u8,
    name: Vec<u8>,
}

fn le_u16(buf: &[u8], off: usize) -> Option<u16> {
    let b: [u8; 2] = buf.get(off..off.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(b))
}

fn le_u32(buf: &[u8], off: usize) -> Option<u32> {
    let b: [u8; 4] = buf.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

fn bad_dir(why: &'static str) -> IsoError {
    IsoError::BadDirectory(why)
}

/// Parse the directory record at `off`. `Ok(None)` means padding (a zero length byte) or the end
/// of the buffer. The error is the reason the record is malformed.
fn parse_record_raw(buf: &[u8], off: usize) -> Result<Option<Record>, &'static str> {
    let Some(&len) = buf.get(off) else {
        return Ok(None);
    };
    let len = usize::from(len);
    if len == 0 {
        return Ok(None);
    }
    if len < MIN_RECORD {
        return Err("directory record shorter than 34 bytes");
    }
    let end = off.checked_add(len).ok_or("record offset overflow")?;
    let rec = buf
        .get(off..end)
        .ok_or("directory record runs past the end of its extent")?;
    let lba = le_u32(rec, 2).ok_or("truncated record")?;
    let size = le_u32(rec, 10).ok_or("truncated record")?;
    let flags = *rec.get(25).ok_or("truncated record")?;
    let name_len = usize::from(*rec.get(32).ok_or("truncated record")?);
    let name = rec
        .get(33..33 + name_len)
        .ok_or("file name overruns its record")?;
    Ok(Some(Record {
        len,
        lba,
        size,
        flags,
        name: name.to_vec(),
    }))
}

/// [`parse_record_raw`] for the root record in the volume descriptor.
fn parse_record(buf: &[u8], off: usize) -> Result<Option<Record>, IsoError> {
    parse_record_raw(buf, off).map_err(bad_dir)
}

/// Printable-ASCII file name without `;version` and trailing dot. None if unsafe.
fn clean_name(raw: &[u8]) -> Option<String> {
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    if !raw.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return None;
    }
    let s = std::str::from_utf8(raw).ok()?;
    if s.contains(['/', '\\']) {
        return None;
    }
    let s = match s.rsplit_once(';') {
        Some((base, ver)) if !ver.is_empty() && ver.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => s,
    };
    let s = s.strip_suffix('.').unwrap_or(s);
    if s.is_empty() || s == "." || s == ".." {
        return None;
    }
    Some(s.to_string())
}

fn clean_label(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| {
            if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Err unless the extent `lba .. lba + nsec` lies inside an image of `sectors` sectors.
fn check_extent_in(sectors: u64, lba: u32, nsec: u64) -> Result<(), IsoError> {
    let out_of_range = IsoError::OutOfRange {
        lba: u64::from(lba),
        sectors,
    };
    let end = u64::from(lba).checked_add(nsec).ok_or(out_of_range)?;
    if end > sectors {
        return Err(IsoError::OutOfRange {
            lba: u64::from(lba),
            sectors,
        });
    }
    Ok(())
}

/// A fault found while scanning one directory's records.
enum DirFault {
    /// A malformed record: costs only this directory (unless it is the root).
    Format(&'static str),
    /// A resource limit was exceeded: always fatal.
    Limit(&'static str),
}

/// Everything the directory scanner accumulates across the whole walk.
struct WalkState {
    result: WalkResult,
    visited: HashSet<u32>,
    /// (extent lba, extent size, path prefix, depth)
    stack: Vec<(u32, u32, String, usize)>,
    /// Directory records seen so far in all directories (not counting `.` and `..`).
    records: usize,
}

/// Scan the records of one directory extent. Files and sub-directories found are added to
/// `st`; on a fault the entries found before the bad record stay in `st`.
fn scan_dir(
    data: &[u8],
    prefix: &str,
    depth: usize,
    sectors: u64,
    limits: &WalkLimits,
    st: &mut WalkState,
) -> Result<(), DirFault> {
    // names already used in this directory (two records can clean to one name: `A.TXT;1`,
    // `A.TXT.;1`); the first record, which ISO9660 orders as the highest version, wins
    let mut seen: HashSet<String> = HashSet::new();
    let mut off = 0usize;
    while off < data.len() {
        let Some(rec) = parse_record_raw(data, off).map_err(DirFault::Format)? else {
            // padding: records never straddle sectors, so continue at the next sector
            off = (off / SECTOR_SIZE + 1) * SECTOR_SIZE;
            continue;
        };
        if off % SECTOR_SIZE + rec.len > SECTOR_SIZE {
            return Err(DirFault::Format("directory record crosses a sector boundary"));
        }
        off += rec.len;
        if rec.name == [0u8] || rec.name == [1u8] {
            continue; // "." and ".."
        }
        st.records += 1;
        if st.records > limits.max_entries {
            return Err(DirFault::Limit("too many directory entries"));
        }
        let Some(name) = clean_name(&rec.name) else {
            let shown: String = String::from_utf8_lossy(&rec.name)
                .chars()
                .take(40)
                .collect();
            st.result.skipped.push(SkippedEntry {
                path: format!("{prefix}{shown}"),
                reason: "unsafe or non-ASCII file name".to_string(),
            });
            continue;
        };
        let path = format!("{prefix}{name}");
        if path.len() > limits.max_path_len {
            st.result.skipped.push(SkippedEntry {
                path: path.chars().take(80).collect(),
                reason: format!(
                    "path is longer than {} bytes (too long to store)",
                    limits.max_path_len
                ),
            });
            continue;
        }
        if !seen.insert(name) {
            st.result.skipped.push(SkippedEntry {
                path,
                reason: "same name as an earlier record in this directory once the ;version suffix is removed (kept the first)".to_string(),
            });
            continue;
        }
        if rec.flags & 0x02 != 0 {
            if depth + 1 >= MAX_DEPTH {
                st.result.skipped.push(SkippedEntry {
                    path,
                    reason: "directory nesting too deep".to_string(),
                });
            } else if !st.visited.insert(rec.lba) {
                st.result.skipped.push(SkippedEntry {
                    path,
                    reason: "directory loop or shared directory extent".to_string(),
                });
            } else {
                st.stack
                    .push((rec.lba, rec.size, format!("{path}/"), depth + 1));
            }
        } else if rec.flags & 0x80 != 0 {
            st.result.skipped.push(SkippedEntry {
                path,
                reason: "multi-extent file (not supported)".to_string(),
            });
        } else if check_extent_in(
            sectors,
            rec.lba,
            u64::from(rec.size).div_ceil(SECTOR_SIZE as u64),
        )
        .is_err()
        {
            st.result.skipped.push(SkippedEntry {
                path,
                reason: "file extent lies outside the image".to_string(),
            });
        } else {
            st.result.files.push(IsoFile {
                path,
                lba: rec.lba,
                size: rec.size,
            });
        }
    }
    Ok(())
}

/// An opened ISO9660 volume.
pub struct Volume<S> {
    src: S,
    label: String,
    root_lba: u32,
    root_size: u32,
    sectors: u64,
}

impl<S: SectorSource> Volume<S> {
    /// Read the primary volume descriptor at sector 16 and the root directory record.
    pub fn open(mut src: S) -> Result<Self, IsoError> {
        let sectors = src.sector_count();
        if sectors <= 16 {
            return Err(IsoError::NotIso(
                "image is too small to hold a volume descriptor",
            ));
        }
        let mut sec = [0u8; SECTOR_SIZE];
        src.read_sector(16, &mut sec)?;
        let signature: &[u8] = &[1, b'C', b'D', b'0', b'0', b'1', 1];
        if sec.get(..7) != Some(signature) {
            return Err(IsoError::NotIso(
                "no primary volume descriptor at sector 16",
            ));
        }
        if le_u16(&sec, 128) != Some(SECTOR_SIZE as u16) {
            return Err(IsoError::NotIso("logical block size is not 2048"));
        }
        let label = clean_label(sec.get(40..72).unwrap_or(&[]));
        let root_rec = sec
            .get(156..156 + MIN_RECORD)
            .ok_or(IsoError::NotIso("truncated volume descriptor"))?;
        let root =
            parse_record(root_rec, 0)?.ok_or(IsoError::NotIso("missing root directory record"))?;
        if root.flags & 0x02 == 0 {
            return Err(IsoError::NotIso("root directory record is not a directory"));
        }
        Ok(Volume {
            src,
            label,
            root_lba: root.lba,
            root_size: root.size,
            sectors,
        })
    }

    pub fn volume_label(&self) -> &str {
        &self.label
    }

    pub fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn check_extent(&self, lba: u32, nsec: u64) -> Result<(), IsoError> {
        check_extent_in(self.sectors, lba, nsec)
    }

    fn read_dir_extent(&mut self, lba: u32, size: u32) -> Result<Vec<u8>, IsoError> {
        let size = u64::from(size);
        if size > MAX_DIR_BYTES {
            return Err(bad_dir("directory extent is unreasonably large"));
        }
        let nsec = size.div_ceil(SECTOR_SIZE as u64);
        self.check_extent(lba, nsec)?;
        let mut out = Vec::with_capacity(
            usize::try_from(nsec)
                .unwrap_or(0)
                .saturating_mul(SECTOR_SIZE),
        );
        let mut sec = [0u8; SECTOR_SIZE];
        for i in 0..nsec {
            self.src.read_sector(u64::from(lba) + i, &mut sec)?;
            out.extend_from_slice(&sec);
        }
        out.truncate(usize::try_from(size).unwrap_or(0));
        Ok(out)
    }

    /// List every file under the root directory, within the default [`WalkLimits`].
    pub fn walk(&mut self) -> Result<WalkResult, IsoError> {
        self.walk_with_limits(&WalkLimits::default())
    }

    /// [`Volume::walk`] with explicit limits.
    pub fn walk_with_limits(&mut self, limits: &WalkLimits) -> Result<WalkResult, IsoError> {
        let sectors = self.sectors;
        let mut st = WalkState {
            result: WalkResult::default(),
            visited: HashSet::new(),
            stack: vec![(self.root_lba, self.root_size, String::new(), 0)],
            records: 0,
        };
        st.visited.insert(self.root_lba);
        let mut dirs_read = 0usize;
        let mut dir_bytes = 0u64;
        while let Some((lba, size, prefix, depth)) = st.stack.pop() {
            let is_root = depth == 0;
            let shown = prefix.trim_end_matches('/').to_string();
            dirs_read += 1;
            if dirs_read > limits.max_dirs {
                return Err(IsoError::LimitExceeded("too many directories"));
            }
            let data = match self.read_dir_extent(lba, size) {
                Ok(d) => d,
                Err(e) if is_root => return Err(e),
                Err(e) => {
                    st.result.skipped.push(SkippedEntry {
                        path: shown,
                        reason: format!("unreadable directory: {e}"),
                    });
                    continue;
                }
            };
            dir_bytes = dir_bytes.saturating_add(data.len() as u64);
            if dir_bytes > limits.max_total_dir_bytes {
                return Err(IsoError::LimitExceeded(
                    "directories hold too many bytes in total",
                ));
            }
            match scan_dir(&data, &prefix, depth, sectors, limits, &mut st) {
                Ok(()) => {}
                Err(DirFault::Limit(why)) => return Err(IsoError::LimitExceeded(why)),
                Err(DirFault::Format(why)) if is_root => return Err(bad_dir(why)),
                Err(DirFault::Format(why)) => st.result.skipped.push(SkippedEntry {
                    path: shown,
                    reason: format!(
                        "malformed directory listing ({why}); the records after the bad one were not read"
                    ),
                }),
            }
        }
        let mut result = st.result;
        result.files.sort_by(|a, b| a.path.cmp(&b.path));
        result.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }

    /// Streaming reader over a file's extent. Fails up front if the extent is outside the image.
    pub fn file_reader(&mut self, file: &IsoFile) -> Result<ExtentReader<'_, S>, IsoError> {
        self.check_extent(file.lba, u64::from(file.size).div_ceil(SECTOR_SIZE as u64))?;
        Ok(ExtentReader {
            src: &mut self.src,
            next_lba: u64::from(file.lba),
            remaining: u64::from(file.size),
            buf: [0u8; SECTOR_SIZE],
            pos: 0,
            avail: 0,
        })
    }

    /// Read a small file fully into memory (SYSTEM.CNF and friends).
    pub fn read_file_limited(&mut self, file: &IsoFile, limit: u64) -> Result<Vec<u8>, IsoError> {
        if u64::from(file.size) > limit {
            return Err(IsoError::TooBig { limit });
        }
        let mut out = Vec::with_capacity(usize::try_from(file.size).unwrap_or(0));
        let mut reader = self.file_reader(file)?;
        reader.read_to_end(&mut out)?;
        Ok(out)
    }
}

/// `Read` over the bytes of one file extent, one sector at a time.
pub struct ExtentReader<'a, S> {
    src: &'a mut S,
    next_lba: u64,
    remaining: u64,
    buf: [u8; SECTOR_SIZE],
    pos: usize,
    avail: usize,
}

impl<S: SectorSource> Read for ExtentReader<'_, S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.avail {
            if self.remaining == 0 {
                return Ok(0);
            }
            self.src.read_sector(self.next_lba, &mut self.buf)?;
            self.next_lba += 1;
            let take = self.remaining.min(SECTOR_SIZE as u64);
            self.avail = usize::try_from(take).unwrap_or(SECTOR_SIZE);
            self.remaining -= take;
            self.pos = 0;
        }
        let n = self.avail.saturating_sub(self.pos).min(out.len());
        let src = self
            .buf
            .get(self.pos..self.pos + n)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sector buffer overrun"))?;
        let dst = out
            .get_mut(..n)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "output buffer overrun"))?;
        dst.copy_from_slice(src);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{build_iso, system_cnf, MemImage, TEST_PAYLOAD_A};

    fn sample() -> Vec<u8> {
        build_iso(&[
            ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("FIELD/SUB/DEEP.BIN", b"deep"),
            ("README.TXT", b""),
            ("NOEXT", b"x"),
        ])
    }

    fn read_all(vol: &mut Volume<MemImage>, f: &IsoFile) -> Vec<u8> {
        let mut v = Vec::new();
        vol.file_reader(f).unwrap().read_to_end(&mut v).unwrap();
        v
    }

    #[test]
    fn lists_and_reads_files() {
        let mut vol = Volume::open(MemImage(sample())).unwrap();
        assert_eq!(vol.volume_label(), "SYNTHETIC_TEST");
        let walk = vol.walk().unwrap();
        assert!(walk.skipped.is_empty(), "{:?}", walk.skipped);
        let names: Vec<&str> = walk.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "FIELD/MD1STIN.DAT",
                "FIELD/SUB/DEEP.BIN",
                "NOEXT",
                "README.TXT",
                "SYSTEM.CNF"
            ]
        );
        let by = |n: &str| walk.files.iter().find(|f| f.path == n).unwrap().clone();
        assert_eq!(read_all(&mut vol, &by("FIELD/MD1STIN.DAT")), TEST_PAYLOAD_A);
        assert_eq!(read_all(&mut vol, &by("FIELD/SUB/DEEP.BIN")), b"deep");
        assert_eq!(read_all(&mut vol, &by("README.TXT")), b"");
        assert_eq!(read_all(&mut vol, &by("NOEXT")), b"x");
        let cnf = vol.read_file_limited(&by("SYSTEM.CNF"), 4096).unwrap();
        assert_eq!(
            crate::serial::parse_boot_serial(&cnf).unwrap().serial,
            "SCUS-94163"
        );
        assert!(matches!(
            vol.read_file_limited(&by("FIELD/MD1STIN.DAT"), 10),
            Err(IsoError::TooBig { .. })
        ));
    }

    #[test]
    fn multi_sector_files_and_directories() {
        // 70 files forces the root directory over a sector boundary (records are padded, never split)
        let names: Vec<String> = (0..70).map(|i| format!("FILE{i:03}.DAT")).collect();
        let big: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut files: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), &big[..10])).collect();
        files.push(("BIG.BIN", big.as_slice()));
        let mut vol = Volume::open(MemImage(build_iso(&files))).unwrap();
        let walk = vol.walk().unwrap();
        assert_eq!(walk.files.len(), 71);
        let f = walk
            .files
            .iter()
            .find(|f| f.path == "BIG.BIN")
            .unwrap()
            .clone();
        assert_eq!(f.size, 5000);
        assert_eq!(read_all(&mut vol, &f), big);
    }

    #[test]
    fn rejects_non_iso_images_without_panicking() {
        assert!(matches!(
            Volume::open(MemImage(vec![])),
            Err(IsoError::NotIso(_))
        ));
        assert!(matches!(
            Volume::open(MemImage(vec![0; 16 * 2048])),
            Err(IsoError::NotIso(_))
        ));
        assert!(matches!(
            Volume::open(MemImage(vec![0; 40 * 2048])),
            Err(IsoError::NotIso(_))
        ));
        let mut img = sample();
        img[16 * 2048 + 129] = 0x04; // block size field: 2048 -> 1024
        assert!(matches!(
            Volume::open(MemImage(img)),
            Err(IsoError::NotIso(_))
        ));
    }

    #[test]
    fn hostile_directory_structures_are_contained() {
        let base = sample();
        let pvd = 16 * 2048;
        let root_lba = u32::from_le_bytes(base[pvd + 158..pvd + 162].try_into().unwrap()) as usize;
        let root = root_lba * 2048;

        // root extent claims to be enormous
        let mut img = base.clone();
        img[pvd + 166..pvd + 170].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Volume::open(MemImage(img)).and_then(|mut v| v.walk()),
            Err(IsoError::BadDirectory(_))
        ));

        // root extent outside the image
        let mut img = base.clone();
        img[pvd + 158..pvd + 162].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
        assert!(matches!(
            Volume::open(MemImage(img)).and_then(|mut v| v.walk()),
            Err(IsoError::OutOfRange { .. })
        ));

        // a record shorter than the 34-byte minimum
        let mut img = base.clone();
        // first real entry sits after "." and ".." (34 bytes each)
        img[root + 68] = 10;
        assert!(matches!(
            Volume::open(MemImage(img)).and_then(|mut v| v.walk()),
            Err(IsoError::BadDirectory(_))
        ));

        // a directory whose declared size cuts its first real record in half
        let mut img = base.clone();
        img[pvd + 166..pvd + 170].copy_from_slice(&100u32.to_le_bytes());
        let r = Volume::open(MemImage(img)).and_then(|mut v| v.walk());
        assert!(matches!(r, Err(IsoError::BadDirectory(_))), "{r:?}");

        // a subdirectory that points back at the root: the loop is cut, the walk ends
        let mut img = base.clone();
        let dirs = find_dir_records(&img, root);
        for off in dirs {
            img[off + 2..off + 6].copy_from_slice(&(root_lba as u32).to_le_bytes());
        }
        let mut vol = Volume::open(MemImage(img)).unwrap();
        let walk = vol.walk().unwrap();
        assert!(
            walk.skipped.iter().any(|s| s.reason.contains("loop")),
            "{:?}",
            walk.skipped
        );

        // root-level files pointing outside the image are skipped, not listed
        let mut img = base.clone();
        for off in find_file_records(&img, root) {
            img[off + 2..off + 6].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
        }
        let mut vol = Volume::open(MemImage(img)).unwrap();
        let walk = vol.walk().unwrap();
        assert!(
            walk.files.iter().all(|f| f.path.starts_with("FIELD/")),
            "{:?}",
            walk.files
        );
        assert_eq!(walk.skipped.len(), 3, "{:?}", walk.skipped);
        assert!(walk.skipped.iter().all(|s| s.reason.contains("outside")));
    }

    #[test]
    fn record_crossing_a_sector_boundary_is_rejected() {
        let names: Vec<String> = (0..70).map(|i| format!("FILE{i:03}.DAT")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let mut img = build_iso(&files);
        let root_lba =
            u32::from_le_bytes(img[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap()) as usize;
        let root = root_lba * 2048;
        // find the last record that starts inside sector 0 of the root directory
        let mut off = 0usize;
        let mut last = 0usize;
        while off < 2048 && img[root + off] != 0 {
            last = off;
            off += img[root + off] as usize;
        }
        assert!(
            last > 1900,
            "sector 0 should be nearly full, last record at {last}"
        );
        img[root + last] = 255;
        let r = Volume::open(MemImage(img)).and_then(|mut v| v.walk());
        assert!(matches!(r, Err(IsoError::BadDirectory(_))), "{r:?}");
    }

    /// Offsets of directory records (flag 0x02) in the root extent, excluding "." and "..".
    fn find_dir_records(img: &[u8], root: usize) -> Vec<usize> {
        records_with(img, root, |flags| flags & 0x02 != 0)
    }

    fn find_file_records(img: &[u8], root: usize) -> Vec<usize> {
        records_with(img, root, |flags| flags & 0x02 == 0)
    }

    fn records_with(img: &[u8], root: usize, pred: impl Fn(u8) -> bool) -> Vec<usize> {
        let mut out = Vec::new();
        let mut off = 0;
        while off < 2048 {
            let len = img[root + off] as usize;
            if len == 0 {
                break;
            }
            let name_len = img[root + off + 32] as usize;
            let name = &img[root + off + 33..root + off + 33 + name_len];
            if name != [0u8] && name != [1u8] && pred(img[root + off + 25]) {
                out.push(root + off);
            }
            off += len;
        }
        out
    }

    #[test]
    fn unsafe_names_are_skipped() {
        let mut img = sample();
        let root_lba =
            u32::from_le_bytes(img[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap()) as usize;
        let root = root_lba * 2048;
        // overwrite the first byte of the first real entry's name with a control char
        let off = 68 + 33;
        img[root + off] = 0x07;
        let mut vol = Volume::open(MemImage(img)).unwrap();
        let walk = vol.walk().unwrap();
        assert_eq!(walk.skipped.len(), 1, "{:?}", walk.skipped);
        assert!(walk.skipped[0].reason.contains("unsafe"));
    }

    #[test]
    fn name_cleaning() {
        assert_eq!(clean_name(b"SYSTEM.CNF;1").as_deref(), Some("SYSTEM.CNF"));
        assert_eq!(clean_name(b"NOEXT.;1").as_deref(), Some("NOEXT"));
        assert_eq!(clean_name(b"A B.TXT").as_deref(), Some("A B.TXT"));
        assert_eq!(clean_name(b"NAME;ABC").as_deref(), Some("NAME;ABC"));
        let bad: [&[u8]; 8] = [
            b"",
            b"..",
            b".",
            b"A/B",
            b"A\\B",
            b"\x01",
            b"\xe9.TXT",
            b";1",
        ];
        for name in bad {
            assert_eq!(clean_name(name), None, "{name:?}");
        }
    }

    /// (lba, size) of the first directory called `name` in the (single-sector) root extent.
    fn dir_extent(img: &[u8], name: &str) -> (usize, usize) {
        let pvd = 16 * 2048;
        let root_lba = u32::from_le_bytes(img[pvd + 158..pvd + 162].try_into().unwrap()) as usize;
        let root = root_lba * 2048;
        let mut off = 0usize;
        while off < 2048 && img[root + off] != 0 {
            let len = img[root + off] as usize;
            let name_len = img[root + off + 32] as usize;
            if &img[root + off + 33..root + off + 33 + name_len] == name.as_bytes() {
                let word = |at: usize| u32::from_le_bytes(img[at..at + 4].try_into().unwrap()) as usize;
                return (word(root + off + 2), word(root + off + 10));
            }
            off += len;
        }
        panic!("no directory {name}");
    }

    fn listed(walk: &WalkResult) -> Vec<&str> {
        walk.files.iter().map(|f| f.path.as_str()).collect()
    }

    /// A format fault in a NESTED directory costs only that directory; the root stays fatal.
    #[test]
    fn nested_format_faults_cost_only_that_directory() {
        let base = build_iso(&[
            ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
            ("FIELD/A.DAT", b"a"),
            ("FIELD/B.DAT", b"b"),
            ("OTHER/C.DAT", b"c"),
        ]);
        let (field_lba, _) = dir_extent(&base, "FIELD");
        let field = field_lba * 2048;

        // the first real record of FIELD gets an impossible length: FIELD lists nothing
        let mut img = base.clone();
        img[field + 68] = 10;
        let walk = Volume::open(MemImage(img)).unwrap().walk().unwrap();
        assert_eq!(listed(&walk), ["OTHER/C.DAT", "SYSTEM.CNF"]);
        assert_eq!(walk.skipped.len(), 1, "{:?}", walk.skipped);
        assert_eq!(walk.skipped[0].path, "FIELD");
        assert!(walk.skipped[0].reason.contains("malformed"), "{:?}", walk.skipped);

        // the SECOND record is the bad one: what came before it is kept, the rest is lost
        let mut img = base.clone();
        img[field + 108] = 10; // "." and ".." take 68 bytes, A.DAT;1 takes 40
        let walk = Volume::open(MemImage(img)).unwrap().walk().unwrap();
        assert_eq!(listed(&walk), ["FIELD/A.DAT", "OTHER/C.DAT", "SYSTEM.CNF"]);
        assert_eq!(walk.skipped.len(), 1);

        // a record that crosses a sector boundary inside a nested directory is the same story
        let names: Vec<String> = (0..70).map(|i| format!("FILE{i:03}.DAT")).collect();
        let mut files: Vec<(&str, &[u8])> = vec![("SYSTEM.CNF", b"cnf"), ("OTHER/C.DAT", b"c")];
        let nested: Vec<String> = names.iter().map(|n| format!("BIG/{n}")).collect();
        files.extend(nested.iter().map(|n| (n.as_str(), &b"x"[..])));
        let mut img = build_iso(&files);
        let (big_lba, _) = dir_extent(&img, "BIG");
        let big = big_lba * 2048;
        let (mut off, mut last) = (0usize, 0usize);
        while off < 2048 && img[big + off] != 0 {
            last = off;
            off += img[big + off] as usize;
        }
        assert!(last > 1900, "sector 0 of BIG should be nearly full ({last})");
        img[big + last] = 255;
        let walk = Volume::open(MemImage(img)).unwrap().walk().unwrap();
        assert!(listed(&walk).contains(&"OTHER/C.DAT") && listed(&walk).contains(&"SYSTEM.CNF"));
        assert!(
            walk.skipped.iter().any(|s| s.path == "BIG" && s.reason.contains("sector boundary")),
            "{:?}",
            walk.skipped
        );
        assert!(
            listed(&walk).iter().any(|p| p.starts_with("BIG/FILE")),
            "the records before the bad one are kept"
        );

        // ... whereas the same fault in the ROOT leaves nothing to list and fails the walk
        let mut img = base.clone();
        let pvd = 16 * 2048;
        let root = u32::from_le_bytes(img[pvd + 158..pvd + 162].try_into().unwrap()) as usize * 2048;
        img[root + 68] = 10;
        assert!(matches!(
            Volume::open(MemImage(img)).unwrap().walk(),
            Err(IsoError::BadDirectory(_))
        ));
    }

    /// Limit breaches are fatal at ANY depth: skipping them would defeat the limits.
    #[test]
    fn limit_breaches_are_fatal_even_inside_nested_directories() {
        let files: Vec<(String, Vec<u8>)> = (0..10)
            .map(|i| (format!("SUB/F{i}.DAT"), vec![b'x']))
            .chain([("SYSTEM.CNF".to_string(), b"cnf".to_vec())])
            .chain((0..3).map(|i| (format!("D{i}/X.DAT"), vec![b'y'])))
            .collect();
        let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        let img = build_iso(&refs);
        let open = || Volume::open(MemImage(img.clone())).unwrap();

        // the defaults are nowhere near a normal image
        assert_eq!(open().walk().unwrap().files.len(), 14);

        // too many records: the root only has 5 records, the breach happens inside SUB
        let tight = WalkLimits { max_entries: 8, ..WalkLimits::default() };
        assert!(matches!(
            open().walk_with_limits(&tight),
            Err(IsoError::LimitExceeded(_))
        ));
        // too many directories (root + SUB + D0..D2 = 5)
        let few_dirs = WalkLimits { max_dirs: 4, ..WalkLimits::default() };
        assert!(matches!(
            open().walk_with_limits(&few_dirs),
            Err(IsoError::LimitExceeded(m)) if m.contains("directories")
        ));
        // too many directory bytes in total (5 directories of one sector each)
        let few_bytes = WalkLimits { max_total_dir_bytes: 4 * 2048, ..WalkLimits::default() };
        assert!(matches!(
            open().walk_with_limits(&few_bytes),
            Err(IsoError::LimitExceeded(m)) if m.contains("bytes")
        ));
        // exactly at the limits is fine
        let exact = WalkLimits { max_entries: 18, max_dirs: 5, max_total_dir_bytes: 5 * 2048, ..WalkLimits::default() };
        assert_eq!(open().walk_with_limits(&exact).unwrap().files.len(), 14);
    }

    fn both32(v: u32) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&v.to_le_bytes());
        b[4..].copy_from_slice(&v.to_be_bytes());
        b
    }

    fn dir_record(name: &[u8], lba: u32, size: u32, is_dir: bool) -> Vec<u8> {
        let mut len = 33 + name.len();
        if len % 2 == 1 {
            len += 1;
        }
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        r[2..10].copy_from_slice(&both32(lba));
        r[10..18].copy_from_slice(&both32(size));
        r[25] = if is_dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// An image with an 8 MiB directory (the largest accepted extent) at LBA 19 whose records are
    /// all directories pointing far outside the image. `nested`: the big directory hangs below
    /// a small root instead of being the root.
    fn image_with_a_huge_directory(nested: bool) -> Vec<u8> {
        const DIR_SECTORS: usize = 4096;
        let mut img = vec![0u8; (19 + DIR_SECTORS) * 2048];
        let (big_lba, big_off) = if nested { (19usize, 19 * 2048) } else { (18, 18 * 2048) };
        {
            let p = &mut img[16 * 2048..17 * 2048];
            p[0] = 1;
            p[1..6].copy_from_slice(b"CD001");
            p[6] = 1;
            p[128..130].copy_from_slice(&2048u16.to_le_bytes());
            let (root_lba, root_size) = if nested { (18, 2048) } else { (18, DIR_SECTORS * 2048) };
            p[156..190].copy_from_slice(&dir_record(&[0], root_lba, root_size as u32, true));
        }
        if nested {
            let mut off = 18 * 2048;
            for rec in [
                dir_record(&[0], 18, 2048, true),
                dir_record(&[1], 18, 2048, true),
                dir_record(b"HUGE", big_lba as u32, (DIR_SECTORS * 2048) as u32, true),
            ] {
                img[off..off + rec.len()].copy_from_slice(&rec);
                off += rec.len();
            }
        }
        let mut off = 0usize;
        let mut put = |img: &mut Vec<u8>, rec: &[u8]| {
            let in_sector = off % 2048;
            if in_sector + rec.len() > 2048 {
                off += 2048 - in_sector;
            }
            if off + rec.len() > DIR_SECTORS * 2048 {
                return false;
            }
            img[big_off + off..big_off + off + rec.len()].copy_from_slice(rec);
            off += rec.len();
            true
        };
        put(&mut img, &dir_record(&[0], big_lba as u32, 0, true));
        put(&mut img, &dir_record(&[1], 18, 0, true));
        let mut n = 0u32;
        while put(&mut img, &dir_record(b"A", 0x0010_0000 + n, 2048, true)) {
            n += 1;
        }
        assert!(n > MAX_ENTRIES as u32, "the directory must hold more records than the cap");
        img
    }

    /// Finding 7: directories that are only ever skipped (unreadable) still count toward the cap.
    #[test]
    fn unreadable_directories_count_toward_the_entry_cap() {
        for nested in [false, true] {
            let img = image_with_a_huge_directory(nested);
            let r = Volume::open(MemImage(img)).unwrap().walk();
            assert!(
                matches!(r, Err(IsoError::LimitExceeded(m)) if m.contains("entries")),
                "nested={nested}: {r:?}"
            );
        }
    }

    #[test]
    fn long_paths_and_duplicate_names_are_skipped_not_fatal() {
        let long = "D".repeat(200);
        let deep = format!("{long}/{long}/{long}/LEAF.DAT");
        let mid = format!("{long}/{long}/MID.DAT");
        let base = build_iso(&[
            ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
            ("FIELD/OK.DAT", b"fine"),
            (deep.as_str(), b"too deep to store"),
            (mid.as_str(), b"still short enough"),
            ("DUP.TXT", b"first"),
            ("DUP.TXT.", b"same cleaned name"),
        ]);
        let walk = Volume::open(MemImage(base)).unwrap().walk().unwrap();
        let names = listed(&walk);
        assert!(names.contains(&"FIELD/OK.DAT") && names.contains(&mid.as_str()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("LEAF")), "{names:?}");
        assert!(
            walk.skipped.iter().any(|s| s.reason.contains("longer than 512")),
            "{:?}",
            walk.skipped
        );
        // exactly one of the two DUP records is listed; the other is reported, not fatal
        assert_eq!(names.iter().filter(|n| **n == "DUP.TXT").count(), 1, "{names:?}");
        assert!(
            walk.skipped.iter().any(|s| s.path == "DUP.TXT" && s.reason.contains("same name")),
            "{:?}",
            walk.skipped
        );
        // the path limit is a parameter
        let img = build_iso(&[("A/B/C/D.DAT", b"x"), ("TOP.DAT", b"y")]);
        let tiny = WalkLimits { max_path_len: 8, ..WalkLimits::default() };
        let walk = Volume::open(MemImage(img)).unwrap().walk_with_limits(&tiny).unwrap();
        assert_eq!(listed(&walk), ["TOP.DAT"]);
        assert!(walk.skipped.iter().any(|s| s.path.starts_with("A/B/C")));
    }

    #[test]
    fn mutation_fuzz_never_panics() {
        // Deterministic byte flips across the descriptor and directory sectors: every outcome is
        // fine as long as nothing panics, allocates without bound, or loops forever.
        let base = sample();
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..1500 {
            let mut img = base.clone();
            for _ in 0..(1 + next() % 6) {
                let sector = 16 + (next() % 6) as usize;
                let off = (next() % 2048) as usize;
                if let Some(b) = img.get_mut(sector * 2048 + off) {
                    *b = next() as u8;
                }
            }
            if let Ok(mut vol) = Volume::open(MemImage(img)) {
                if let Ok(walk) = vol.walk() {
                    for f in walk.files.iter().take(20) {
                        if let Ok(mut r) = vol.file_reader(f) {
                            let mut sink = Vec::new();
                            let _ = r.read_to_end(&mut sink);
                        }
                    }
                }
            }
        }
    }
}
