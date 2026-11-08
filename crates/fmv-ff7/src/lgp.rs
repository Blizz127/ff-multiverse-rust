//! LGP archive reader.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08** against `field/flevel.lgp`
//! (131 431 170 bytes, 753 entries) and `field/char.lgp` (48 989 867 bytes, 12 649 entries);
//! other editions are unverified. Measured facts (counts and sizes only):
//!
//! * Both archives parse with the layout below: `\0\0SQUARESOFT` + `u32` count, 27-byte table of
//!   contents entries, every check code `0x0E`, every path index 0, and exactly 3 602 bytes
//!   between the table of contents and the first data header (the 3 600-byte lookup table plus a
//!   `u16` zero for an empty path table, so the path table really can be ignored).
//! * Each entry's 24-byte data header repeats its table-of-contents name (0 mismatches in
//!   either archive), entries are stored back to back in table order (no gaps), and the 14
//!   bytes `FINAL FANTASY7` end the file right after the last entry.
//! * No bare name repeats (case-insensitively) in either archive, so "first match wins" is
//!   unambiguous. The largest entries are 614 328 bytes (`flevel`) and 124 236 bytes (`char`),
//!   far below [`MAX_ENTRY_LEN`].
//! * `flevel.lgp` holds 702 field entries plus 51 others: the name list `maplist` (stored raw,
//!   not LZSS: a `u16` count, 788, then 32-byte NUL-padded names; `md1_1` is id 117) and 50
//!   names containing a `.` (`*.tex` placeholders, `*.tut` tutorials and `*.NX` / `*.XOne`
//!   console-port extras). `char.lgp` holds `.hrc` (385), `.a` (3 209), `.p` (4 180), `.rsd`
//!   (4 180) and `.tex` (695) entries.
//! * The table of contents is not sorted by name.
//!
//! # Sources
//!
//! * FFRTT wiki, "FF7/LGP format" <https://wiki.ffrtt.ru/index.php/FF7/LGP_format>
//!   (fetched through a summarising tool, not copied verbatim): 16-byte header, 27-byte
//!   table-of-contents entries, 900-bucket lookup table, path (conflict) table, per-file
//!   24-byte data header, `FINAL FANTASY7` footer.
//! * Cross-check: search-result summaries of the older Qhimm wiki page
//!   <https://qhimm-modding.fandom.com/wiki/FF7/LGP_format> (12-byte creator string
//!   right-aligned `SQUARESOFT`, 4-byte file count). The two descriptions agree: the FFRTT
//!   header is `u16 0 | "SQUARESOFT" | u16 count | u16 0`, which read as 12 bytes + `u32 LE`
//!   is the older form as long as the count is below 65536.
//!
//! # Layout used here (all integers little-endian)
//!
//! ```text
//! 0x00  12   creator string: 2 NUL bytes + "SQUARESOFT"           (checked)
//! 0x0C   4   u32 file count                                       (capped, see MAX_ENTRIES)
//! 0x10  27*N table of contents, one entry per file:
//!              20  file name, NUL padded (bare name, no directory)
//!               4  u32 absolute offset of the file's data header
//!               1  u8  "check code" (documented as always 0x0E; reported, not enforced)
//!               2  u16 path ("conflict") index, 0 = none
//!       3600       lookup table (900 x {u16 first TOC index, u16 count})  -- NOT used
//!       ...        path table (u16 groups, each u16 count + 130-byte entries) -- NOT used
//! data  at each TOC offset:  20-byte name (mirrors the TOC; ignored), u32 length, bytes
//! end   14-byte footer "FINAL FANTASY7"                                       -- NOT checked
//! ```
//!
//! # What is deliberately ignored
//!
//! * The lookup table is only an acceleration structure for the game; names are resolved
//!   from the table of contents.
//! * The path/conflict table is **not parsed**. Both owned archives need no directories (all
//!   path indices are 0 and the table is empty); an archive that does would be a gap. Each
//!   entry's raw path index is exposed as [`LgpEntry::path_index`]; when several entries
//!   share a bare name, [`LgpArchive::read`] returns the first one in table order.
//! * The data-header name is ignored (the wiki notes the game reads only TOC names).
//!
//! # Safety against untrusted input
//!
//! The reader never trusts a count, offset or length: the entry count is capped, the table
//! of contents must fit inside the file, each data offset must lie after the table of
//! contents and leave room for a data header, and each entry length is checked against both
//! the file size and [`MAX_ENTRY_LEN`] *before* any buffer is allocated. All offset
//! arithmetic is done in `u64` with checked operations. The reader only ever reads; callers
//! that open files do so read-only.

use crate::util::{nul_padded_name, slice_at, u16_at, u32_at, u8_at};
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use thiserror::Error;

/// Bytes in the fixed header (12-byte creator string + `u32` count).
pub const HEADER_LEN: u64 = 16;
/// Width of a table-of-contents name field.
pub const NAME_LEN: usize = 20;
/// Bytes per table-of-contents entry.
pub const TOC_ENTRY_LEN: u64 = 27;
/// [`TOC_ENTRY_LEN`] as a `usize` (for chunking the table of contents).
const TOC_ENTRY_BYTES: usize = 27;
/// Bytes of a per-file data header (20-byte name + `u32` length).
pub const DATA_HEADER_LEN: u64 = 24;
/// Expected creator string (the two leading NUL bytes are not checked).
pub const MAGIC: &[u8; 10] = b"SQUARESOFT";
/// Cap on the entry count. The FFRTT layout stores the count as a `u16` plus an unused
/// `u16`, so anything above this is not a valid FF7 archive.
pub const MAX_ENTRIES: u32 = 65_535;
/// Cap on a single entry's stored length (16 MiB). Real `flevel.lgp` entries are well under
/// 1 MiB and the largest entries of any FF7 PC archive are a small fraction of this; a lower
/// cap bounds what a hostile table of contents can make the reader copy per entry (see also
/// the cumulative budget of `Ff7Adapter`'s index).
pub const MAX_ENTRY_LEN: u32 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum LgpError {
    #[error("io error reading archive: {0}")]
    Io(#[from] io::Error),
    #[error("archive is {0} bytes, shorter than the {HEADER_LEN}-byte header")]
    TooShort(u64),
    #[error("not an LGP archive (creator string is not SQUARESOFT)")]
    BadMagic,
    #[error("archive declares {count} entries, over the limit of {limit}")]
    TooManyEntries { count: u32, limit: u32 },
    #[error("table of contents ({count} entries) does not fit in a {file_len}-byte archive")]
    TocOutOfBounds { count: u32, file_len: u64 },
    #[error("table of contents entry {index} is malformed")]
    TocCorrupt { index: usize },
    #[error("entry {index} ({name:?}): data offset {offset} is outside the valid range {min}..={max}")]
    BadOffset { index: usize, name: String, offset: u32, min: u64, max: u64 },
    #[error("entry {name:?}: stored length {len} runs past the end of the {file_len}-byte archive")]
    EntryOutOfBounds { name: String, len: u32, file_len: u64 },
    #[error("entry {name:?}: stored length {len} is over the limit of {limit} bytes")]
    EntryTooLarge { name: String, len: u32, limit: u32 },
    #[error("no entry named {0:?} in the archive")]
    NotFound(String),
}

/// One table-of-contents entry. `name` is the bare file name exactly as stored (ASCII; any
/// byte >= 0x80 is mapped through Latin-1). Look-ups are ASCII case-insensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LgpEntry {
    pub name: String,
    /// Absolute offset of the entry's 24-byte data header.
    pub offset: u32,
    /// Raw "check code" byte (documented as always `0x0E`).
    pub check_code: u8,
    /// Raw path ("conflict") index; `0` means the entry has no directory. The path table
    /// itself is not parsed.
    pub path_index: u16,
}

/// A parsed LGP table of contents over a seekable reader. The data itself is read on demand.
pub struct LgpArchive<R> {
    reader: R,
    file_len: u64,
    entries: Vec<LgpEntry>,
    /// ASCII-lowercased bare name -> index of the FIRST entry with that name.
    by_name: HashMap<String, usize>,
}

impl<R> std::fmt::Debug for LgpArchive<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LgpArchive")
            .field("file_len", &self.file_len)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl LgpArchive<std::fs::File> {
    /// Open an archive file (for example a library blob) **read-only**.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self, LgpError> {
        Self::open(std::fs::File::open(path)?)
    }
}

impl<R: Read + Seek> LgpArchive<R> {
    /// Parse the header and table of contents. Reads nothing else.
    pub fn open(mut reader: R) -> Result<Self, LgpError> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        if file_len < HEADER_LEN {
            return Err(LgpError::TooShort(file_len));
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_LEN as usize];
        reader.read_exact(&mut header)?;
        if slice_at(&header, 2, MAGIC.len()) != Some(&MAGIC[..]) {
            return Err(LgpError::BadMagic);
        }
        let count = u32_at(&header, 12).ok_or(LgpError::TooShort(file_len))?;
        if count > MAX_ENTRIES {
            return Err(LgpError::TooManyEntries { count, limit: MAX_ENTRIES });
        }

        // count <= 65_535, so these u64 products cannot overflow.
        let toc_len = u64::from(count) * TOC_ENTRY_LEN;
        let toc_end = HEADER_LEN + toc_len;
        if toc_end > file_len {
            return Err(LgpError::TocOutOfBounds { count, file_len });
        }
        // Bounded by file_len and by the cap above (<= ~1.7 MB).
        let mut toc = vec![0u8; usize::try_from(toc_len).map_err(|_| LgpError::TocOutOfBounds { count, file_len })?];
        reader.read_exact(&mut toc)?;

        // A data header must fit entirely before the end of the file.
        let max_offset = file_len.saturating_sub(DATA_HEADER_LEN);
        let record_count = toc.len() / TOC_ENTRY_BYTES;
        let mut entries = Vec::with_capacity(record_count);
        let mut by_name: HashMap<String, usize> = HashMap::with_capacity(record_count);
        // `as_chunks` would satisfy this lint but needs Rust 1.88; wgpu 30 (a workspace
        // dependency) only needs 1.87, so keep `chunks_exact` rather than raise the MSRV.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for (index, raw) in toc.chunks_exact(TOC_ENTRY_BYTES).enumerate() {
            let corrupt = || LgpError::TocCorrupt { index };
            let name = nul_padded_name(slice_at(raw, 0, NAME_LEN).ok_or_else(corrupt)?);
            let offset = u32_at(raw, NAME_LEN).ok_or_else(corrupt)?;
            let check_code = u8_at(raw, NAME_LEN + 4).ok_or_else(corrupt)?;
            let path_index = u16_at(raw, NAME_LEN + 5).ok_or_else(corrupt)?;
            let off64 = u64::from(offset);
            if off64 < toc_end || off64 > max_offset {
                return Err(LgpError::BadOffset { index, name, offset, min: toc_end, max: max_offset });
            }
            if !name.is_empty() {
                by_name.entry(name.to_ascii_lowercase()).or_insert(index);
            }
            entries.push(LgpEntry { name, offset, check_code, path_index });
        }

        Ok(Self { reader, file_len, entries, by_name })
    }

    /// All table-of-contents entries in stored order.
    pub fn list(&self) -> &[LgpEntry] {
        &self.entries
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Size in bytes of the archive this table of contents was read from.
    pub fn archive_len(&self) -> u64 {
        self.file_len
    }

    /// Case-insensitive look-up by bare name (first match in table order).
    pub fn find(&self, name: &str) -> Option<&LgpEntry> {
        self.find_index(name).and_then(|i| self.entries.get(i))
    }

    fn find_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// Read the named entry's bytes (case-insensitive name).
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, LgpError> {
        let index = self.find_index(name).ok_or_else(|| LgpError::NotFound(name.to_string()))?;
        self.read_index(index)
    }

    /// Read the entry at `index` in table order.
    pub fn read_index(&mut self, index: usize) -> Result<Vec<u8>, LgpError> {
        let entry = self.entries.get(index).ok_or_else(|| LgpError::NotFound(format!("#{index}")))?;
        let name = entry.name.clone();
        let offset = u64::from(entry.offset);

        self.reader.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; DATA_HEADER_LEN as usize];
        self.reader.read_exact(&mut header)?;
        let len = u32_at(&header, NAME_LEN).ok_or(LgpError::TocCorrupt { index })?;

        // Validate against the real file size and the hard cap BEFORE allocating.
        let end = offset
            .checked_add(DATA_HEADER_LEN)
            .and_then(|start| start.checked_add(u64::from(len)));
        match end {
            Some(end) if end <= self.file_len => {}
            _ => return Err(LgpError::EntryOutOfBounds { name, len, file_len: self.file_len }),
        }
        if len > MAX_ENTRY_LEN {
            return Err(LgpError::EntryTooLarge { name, len, limit: MAX_ENTRY_LEN });
        }
        let mut buf = vec![0u8; len as usize];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Give the underlying reader back.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::build_lgp;
    use std::io::Cursor;

    fn open(bytes: Vec<u8>) -> Result<LgpArchive<Cursor<Vec<u8>>>, LgpError> {
        LgpArchive::open(Cursor::new(bytes))
    }

    fn sample() -> Vec<u8> {
        build_lgp(&[("md1stin", b"first field"), ("MAPLIST", b"names"), ("empty", b""), ("md1stin", b"duplicate")])
    }

    #[test]
    fn lists_and_reads_case_insensitively() {
        let mut a = open(sample()).unwrap();
        assert_eq!(a.len(), 4);
        assert!(!a.is_empty());
        let names: Vec<&str> = a.list().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["md1stin", "MAPLIST", "empty", "md1stin"]);
        assert_eq!(a.read("md1stin").unwrap(), b"first field");
        assert_eq!(a.read("MD1STIN").unwrap(), b"first field");
        assert_eq!(a.read("maplist").unwrap(), b"names");
        assert_eq!(a.read("Empty").unwrap(), b"");
        // duplicate bare names: first entry wins by name, the other stays reachable by index
        assert_eq!(a.read_index(3).unwrap(), b"duplicate");
        assert_eq!(a.find("maplist").unwrap().check_code, 0x0E);
        assert_eq!(a.find("maplist").unwrap().path_index, 0);
        assert!(a.archive_len() > 3600);
        assert!(format!("{a:?}").contains("entries"));
    }

    #[test]
    fn open_path_reads_a_file_without_modifying_it() {
        let bytes = sample();
        let path = std::env::temp_dir().join(format!("fmv-ff7-lgp-{}-{:?}.lgp", std::process::id(), std::thread::current().id()));
        std::fs::write(&path, &bytes).unwrap();
        {
            let mut a = LgpArchive::open_path(&path).unwrap();
            assert_eq!(a.read("maplist").unwrap(), b"names");
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(LgpArchive::open_path(&path), Err(LgpError::Io(_))));
    }

    #[test]
    fn missing_entries_are_errors() {
        let mut a = open(sample()).unwrap();
        assert!(matches!(a.read("nope"), Err(LgpError::NotFound(n)) if n == "nope"));
        assert!(matches!(a.read_index(99), Err(LgpError::NotFound(_))));
        assert!(a.find("nope").is_none());
    }

    #[test]
    fn empty_archive_is_valid() {
        let a = open(build_lgp(&[])).unwrap();
        assert!(a.is_empty());
        assert!(a.list().is_empty());
    }

    #[test]
    fn reader_can_be_recovered() {
        let a = open(sample()).unwrap();
        let cursor = a.into_inner();
        assert!(cursor.get_ref().len() > 16);
    }

    #[test]
    fn rejects_bad_magic_and_short_files() {
        assert!(matches!(open(vec![]), Err(LgpError::TooShort(0))));
        assert!(matches!(open(vec![0; 15]), Err(LgpError::TooShort(15))));
        let mut bytes = sample();
        bytes[5] = b'X';
        assert!(matches!(open(bytes), Err(LgpError::BadMagic)));
        assert!(matches!(open(vec![0; 64]), Err(LgpError::BadMagic)));
    }

    #[test]
    fn rejects_huge_counts_without_allocating() {
        let mut bytes = sample();
        // count = 0xFFFF_FFFF
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(open(bytes.clone()), Err(LgpError::TooManyEntries { count: u32::MAX, .. })));
        // count just over the cap
        bytes[12..16].copy_from_slice(&(MAX_ENTRIES + 1).to_le_bytes());
        assert!(matches!(open(bytes.clone()), Err(LgpError::TooManyEntries { .. })));
        // count within the cap but the table of contents cannot fit in the file
        bytes[12..16].copy_from_slice(&MAX_ENTRIES.to_le_bytes());
        assert!(matches!(open(bytes), Err(LgpError::TocOutOfBounds { count: MAX_ENTRIES, .. })));
    }

    #[test]
    fn rejects_bad_offsets() {
        let toc_entry = |i: usize| 16 + i * 27 + 20; // position of entry i's offset field
        // offset inside the header/TOC
        let mut bytes = sample();
        bytes[toc_entry(0)..toc_entry(0) + 4].copy_from_slice(&8u32.to_le_bytes());
        assert!(matches!(open(bytes), Err(LgpError::BadOffset { index: 0, .. })));
        // offset past the end of the file
        let mut bytes = sample();
        bytes[toc_entry(1)..toc_entry(1) + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(open(bytes), Err(LgpError::BadOffset { index: 1, .. })));
        // offset that leaves no room for the 24-byte data header
        let mut bytes = sample();
        let near_end = (bytes.len() - 10) as u32;
        bytes[toc_entry(2)..toc_entry(2) + 4].copy_from_slice(&near_end.to_le_bytes());
        assert!(matches!(open(bytes), Err(LgpError::BadOffset { index: 2, .. })));
    }

    #[test]
    fn rejects_entry_lengths_past_end_of_file() {
        let bytes = sample();
        let a = open(bytes.clone()).unwrap();
        let off = a.find("md1stin").unwrap().offset as usize;
        for bad in [bytes.len() as u32, u32::MAX, (MAX_ENTRY_LEN + 1)] {
            let mut b = bytes.clone();
            b[off + 20..off + 24].copy_from_slice(&bad.to_le_bytes());
            let mut a = open(b).unwrap();
            let err = a.read("md1stin").unwrap_err();
            assert!(matches!(err, LgpError::EntryOutOfBounds { .. }), "{err}");
        }
        // A length that exactly reaches the end of the file is fine.
        let mut b = bytes.clone();
        b[off + 20..off + 24].copy_from_slice(&((bytes.len() - off - 24) as u32).to_le_bytes());
        let mut a = open(b).unwrap();
        assert!(a.read("md1stin").is_ok());
    }

    /// A reader that pretends to be a huge, mostly-zero file so the hard length cap can be
    /// exercised without allocating or writing anything.
    struct Sparse {
        base: Vec<u8>,
        total: u64,
        pos: u64,
    }
    impl Read for Sparse {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = (buf.len() as u64).min(self.total.saturating_sub(self.pos)) as usize;
            for (i, slot) in buf.iter_mut().take(n).enumerate() {
                *slot = self.base.get(self.pos as usize + i).copied().unwrap_or(0);
            }
            self.pos += n as u64;
            Ok(n)
        }
    }
    impl Seek for Sparse {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            let new = match from {
                SeekFrom::Start(p) => p as i128,
                SeekFrom::End(d) => self.total as i128 + d as i128,
                SeekFrom::Current(d) => self.pos as i128 + d as i128,
            };
            if new < 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
            }
            self.pos = new as u64;
            Ok(self.pos)
        }
    }

    #[test]
    fn entry_length_cap_is_enforced_before_allocation() {
        let bytes = build_lgp(&[("big", b"x")]);
        let off = LgpArchive::open(Cursor::new(bytes.clone())).unwrap().find("big").unwrap().offset as usize;
        let mut base = bytes;
        let len = MAX_ENTRY_LEN + 1;
        base[off + 20..off + 24].copy_from_slice(&len.to_le_bytes());
        // Virtual file is big enough that the length fits, so only the cap can reject it.
        let total = off as u64 + DATA_HEADER_LEN + u64::from(len) + 100;
        let mut a = LgpArchive::open(Sparse { base: base.clone(), total, pos: 0 }).unwrap();
        assert!(matches!(a.read("big"), Err(LgpError::EntryTooLarge { limit: MAX_ENTRY_LEN, .. })));
        // Exactly at the cap is allowed by the cap check (it would be allocated, so only check
        // the smaller neighbour to keep the test light).
        let len_ok = 1024u32;
        base[off + 20..off + 24].copy_from_slice(&len_ok.to_le_bytes());
        let mut a = LgpArchive::open(Sparse { base, total, pos: 0 }).unwrap();
        assert_eq!(a.read("big").unwrap().len(), 1024);
    }

    #[test]
    fn truncated_archive_reports_errors_not_panics() {
        let bytes = sample();
        for n in 0..bytes.len() {
            let prefix = bytes[..n].to_vec();
            if let Ok(mut a) = open(prefix) {
                for i in 0..a.len() {
                    let _ = a.read_index(i);
                }
            }
        }
    }

    #[test]
    fn single_byte_flips_never_panic() {
        let bytes = sample();
        // Flip each byte of the header, TOC and the first data headers; other bytes are payload.
        let limit = 16 + 4 * 27 + 40;
        for i in 0..limit.min(bytes.len()) {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = bytes.clone();
                b[i] ^= flip;
                if let Ok(mut a) = open(b) {
                    for idx in 0..a.len() {
                        let _ = a.read_index(idx);
                    }
                    let _ = a.read("md1stin");
                }
            }
        }
    }

    #[test]
    fn names_with_high_bytes_do_not_fail() {
        let mut bytes = build_lgp(&[("abc", b"x")]);
        bytes[16] = 0xE9; // first name byte
        let a = open(bytes).unwrap();
        assert_eq!(a.list()[0].name.chars().next(), Some('\u{e9}'));
    }
}
