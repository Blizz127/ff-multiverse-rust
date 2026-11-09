//! A bounded, read-only reader for 7th Heaven's `.iro` mod archives.
//!
//! **Verified on the two archives this project's layers use (`Echo-s.iro`, 23 178 entries; and
//! `[Tsunamods]_Cosmo_Memory.iro`, 1 610 entries; both complete, 2026-11-09).** Counts and sizes only; no
//! bytes of either archive are stored in this repository.
//!
//! # The format
//!
//! Taken from the 7th Heaven mod manager's own reader and writer, `AppWrapper/IrosArc.cs`, pinned to commit
//! `f0d7da92beac69e21c59995fa13ea1b2b0e3cf1f` (tsunamods-codes/7th-Heaven, 2026-11-08):
//! <https://github.com/tsunamods-codes/7th-Heaven/blob/f0d7da92beac69e21c59995fa13ea1b2b0e3cf1f/AppWrapper/IrosArc.cs>
//! (`SIG`, `MIN_VERSION`, `MAX_VERSION`, `FileFlags` at lines 21 to 54; `ArcHeader` at 67 to 93; `DirectoryEntry`
//! at 95 to 137; the directory loop with its forwarder at 294 to 322; the LZMA payload layout at 177 to 183 and
//! 475 to 484; `Create`, the writer, at 218 to 274). The original developer is Iros; the source is under the
//! Microsoft Public License. Only the layout was taken; no code was copied.
//!
//! All integers are little endian.
//!
//! ```text
//! header (16 bytes)
//!   +0   i32  signature   0x534F5249  (the bytes "IROS")
//!   +4   i32  version     0x10000 (4-byte offsets), 0x10001 and 0x10002 (8-byte offsets)
//!   +8   i32  flags       bit 0 = Patch (a patch archive: "%IrosPatch:Deleted" and friends; not read here)
//!   +12  i32  directory   byte offset of the directory (16 in everything 7th Heaven writes)
//!
//! directory, at `directory`
//!   i32 count             -1 = a forwarder: an i64 absolute offset follows, and the count is read again there
//!                         (7th Heaven writes one when it appends to an archive of 2 GiB or more)
//!   count entries, each
//!     u16 size            bytes of this entry from here, the size field included (the next entry starts there)
//!     u16 name_bytes      length of the name in bytes
//!     name                UTF-16LE, folders separated by '\', looked up ignoring case
//!     i32 flags           low nibble = compression (0 stored, 1 LZS, 2 LZMA); bits 16..23 are reserved and
//!                         7th Heaven refuses an archive that sets one
//!     i32 or i64 offset   absolute offset of the payload (i32 for version 0x10000, else i64)
//!     i32 length          stored length of the payload
//! ```
//!
//! A payload is the file's bytes (flag 0), or an LZS stream, or an LZMA record: `i32 decompressed size,
//! i32 property size, the properties, the raw LZMA stream`. **This reader decodes stored payloads only**: every
//! entry of both archives is stored (the writer stores what does not shrink by a fifth, and `.ogg` and `.png`
//! never are compressed), and an entry with LZS or LZMA gives [`IroError::Unsupported`] instead of a decoder
//! nobody here has data to check against.
//!
//! # Bounds
//!
//! The archive is untrusted input and never allocates by what it says: the entry count, the length of a name,
//! the number of forwarders followed, the size of one read and the size of the directory are all capped, every
//! offset is checked against the file's real length (an entry past the end is how a truncated download
//! shows, and the whole archive is refused with the entry's name), and nothing indexes a slice. The file is only
//! ever opened for reading.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use thiserror::Error;

/// `"IROS"` as a little-endian `i32`.
pub const SIGNATURE: u32 = 0x534F_5249;
/// The oldest version 7th Heaven reads (4-byte offsets).
pub const MIN_VERSION: u32 = 0x1_0000;
/// The newest version 7th Heaven reads (8-byte offsets).
pub const MAX_VERSION: u32 = 0x1_0002;
/// The first version with 8-byte offsets.
const WIDE_OFFSET_VERSION: u32 = 0x1_0001;
/// Bytes of the header.
const HEADER_LEN: u64 = 16;
/// Most entries one archive may declare (the archives in use have 23 178 and 1 610).
pub const MAX_ENTRIES: usize = 1 << 20;
/// Longest entry name accepted, in bytes of UTF-16.
pub const MAX_NAME_BYTES: usize = 4096;
/// Forwarders followed before the directory is called a loop.
pub const MAX_FORWARDS: usize = 8;
/// The most one [`IroArchive::read`] allocates: 512 MiB (the biggest entry in use is a 324 MB movie that no
/// layer reads).
pub const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
/// The reserved flag bits 7th Heaven rejects (`FileFlags.RudeFlags`).
const RESERVED_FLAGS: u32 = 0x00FF_0000;
/// The compression nibble (`FileFlags.COMPRESSION_FLAGS`).
const COMPRESSION_MASK: u32 = 0xF;

/// What went wrong with an archive. Messages never contain the archive's own path.
#[derive(Debug, Error)]
pub enum IroError {
    #[error("the archive cannot be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an .iro archive (signature {found:#010x}, expected {SIGNATURE:#010x})")]
    BadSignature { found: u32 },
    #[error("unsupported .iro version {found:#x} (7th Heaven reads {MIN_VERSION:#x} to {MAX_VERSION:#x})")]
    BadVersion { found: u32 },
    #[error("the archive's directory is damaged: {0}")]
    Directory(&'static str),
    #[error("entry {name:?} ends at byte {end} but the file has {len}: the archive is truncated")]
    PastEnd { name: String, end: u64, len: u64 },
    #[error("entry {name:?} sets the reserved flag bits {flags:#x}; 7th Heaven refuses such an archive too")]
    ReservedFlags { name: String, flags: u32 },
    #[error("entry {name:?} is compressed with {kind}, which this reader does not decode")]
    Unsupported { name: String, kind: String },
    #[error("entry {name:?} is {length} bytes, more than the {limit}-byte limit of this read")]
    TooLarge { name: String, length: u64, limit: u64 },
    #[error("the archive has no entry named {0:?}")]
    NotFound(String),
}

/// How an entry's payload is stored (the low nibble of its flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Stored,
    /// FF7's own LZSS (7th Heaven's `Lzs.cs`); not decoded here.
    Lzs,
    /// LZMA with a 7-Zip property block; not decoded here.
    Lzma,
    /// Any other value: 7th Heaven calls it "bad compression flags".
    Other(u32),
}

impl Compression {
    fn of(flags: u32) -> Self {
        match flags & COMPRESSION_MASK {
            0 => Self::Stored,
            1 => Self::Lzs,
            2 => Self::Lzma,
            other => Self::Other(other),
        }
    }

    /// A short name for messages.
    pub fn label(self) -> String {
        match self {
            Self::Stored => "no compression".to_string(),
            Self::Lzs => "LZS".to_string(),
            Self::Lzma => "LZMA".to_string(),
            Self::Other(n) => format!("unknown compression {n}"),
        }
    }
}

/// One file of an archive, as the directory lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IroEntry {
    /// The name as stored: folders separated by `\`, the case the author gave it.
    pub name: String,
    pub flags: u32,
    /// Absolute offset of the payload.
    pub offset: u64,
    /// Stored length of the payload (the file's size, for a stored entry).
    pub length: u32,
}

impl IroEntry {
    pub fn compression(&self) -> Compression {
        Compression::of(self.flags)
    }

    /// The folder part of the name (everything before the last `\`), empty at the root.
    pub fn folder(&self) -> &str {
        self.name.rsplit_once('\\').map_or("", |(folder, _)| folder)
    }

    /// The file part of the name.
    pub fn file_name(&self) -> &str {
        self.name.rsplit_once('\\').map_or(self.name.as_str(), |(_, file)| file)
    }
}

/// The key a name is looked up by: `/` read as `\`, case ignored (7th Heaven compares names ignoring case).
pub fn normalize(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for c in name.chars() {
        if c == '/' {
            key.push('\\');
        } else {
            key.extend(c.to_lowercase());
        }
    }
    key
}

/// An open `.iro` archive: its directory in memory, its payloads read on demand.
///
/// `R` is the file (anything `Read + Seek`; the tests use a cursor). Reads take a lock for the seek and the read,
/// so one archive is shared between threads (a voice decode and a dialog text read at once).
pub struct IroArchive<R = File> {
    reader: Mutex<R>,
    file_len: u64,
    version: u32,
    flags: u32,
    entries: Vec<IroEntry>,
    /// Normalised name -> index of the LAST entry of that name (7th Heaven's lookup keeps the last).
    index: HashMap<String, usize>,
    duplicates: usize,
}

impl<R> std::fmt::Debug for IroArchive<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IroArchive")
            .field("version", &format_args!("{:#x}", self.version))
            .field("entries", &self.entries.len())
            .field("bytes", &self.file_len)
            .finish_non_exhaustive()
    }
}

impl IroArchive<File> {
    /// Open the archive at `path`, read-only, and read its directory.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self, IroError> {
        let file = File::open(path.as_ref())?;
        Self::open(file)
    }
}

fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    let b = buf.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*b.first()?, *b.get(1)?]))
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    let b = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
}

fn u64_at(buf: &[u8], at: usize) -> Option<u64> {
    let lo = u64::from(u32_at(buf, at)?);
    let hi = u64::from(u32_at(buf, at.checked_add(4)?)?);
    Some(lo | (hi << 32))
}

/// A signed 32-bit field as the non-negative number it must be.
fn non_negative(raw: u32) -> Option<u32> {
    (raw <= i32::MAX as u32).then_some(raw)
}

impl<R: Read + Seek> IroArchive<R> {
    /// Read the header and the directory of the archive behind `reader`.
    pub fn open(mut reader: R) -> Result<Self, IroError> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        if file_len < HEADER_LEN {
            return Err(IroError::Directory("the file is shorter than the 16-byte header"));
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; 16];
        reader.read_exact(&mut head)?;
        let signature = u32_at(&head, 0).unwrap_or(0);
        if signature != SIGNATURE {
            return Err(IroError::BadSignature { found: signature });
        }
        let version = u32_at(&head, 4).unwrap_or(0);
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(IroError::BadVersion { found: version });
        }
        let flags = u32_at(&head, 8).unwrap_or(0);
        let directory = u64::from(u32_at(&head, 12).unwrap_or(0));
        if directory < HEADER_LEN || directory.saturating_add(4) > file_len {
            return Err(IroError::Directory("the directory starts outside the file"));
        }

        // The count, following forwarders (a negative one-and-only value) a bounded number of times.
        let mut at = directory;
        let mut forwards = 0usize;
        let count = loop {
            reader.seek(SeekFrom::Start(at))?;
            let mut word = [0u8; 4];
            read_exact_or(&mut reader, &mut word, "the entry count is cut off")?;
            let raw = u32::from_le_bytes(word);
            if raw == u32::MAX {
                forwards += 1;
                if forwards > MAX_FORWARDS {
                    return Err(IroError::Directory("too many forwarders (a loop?)"));
                }
                let mut wide = [0u8; 8];
                read_exact_or(&mut reader, &mut wide, "a forwarder is cut off")?;
                at = u64::from_le_bytes(wide);
                if at < HEADER_LEN || at.saturating_add(4) > file_len {
                    return Err(IroError::Directory("a forwarder points outside the file"));
                }
                continue;
            }
            break non_negative(raw).ok_or(IroError::Directory("a negative entry count"))? as usize;
        };
        if count > MAX_ENTRIES {
            return Err(IroError::Directory("more entries than any archive in use has"));
        }
        // An entry is at least 4 bytes (its two size fields): a count the rest of the file cannot hold is a lie.
        let after_count = at.saturating_add(4);
        if (count as u64).saturating_mul(4) > file_len.saturating_sub(after_count) {
            return Err(IroError::Directory("the entry count is larger than the file can hold"));
        }

        let wide = version >= WIDE_OFFSET_VERSION;
        let mut entries: Vec<IroEntry> = Vec::with_capacity(count.min(1 << 16));
        let mut index: HashMap<String, usize> = HashMap::with_capacity(count.min(1 << 16));
        let mut duplicates = 0usize;
        let mut buffered = BufReader::new(reader);
        buffered.seek(SeekFrom::Start(after_count))?;
        let mut position = after_count;
        for _ in 0..count {
            let mut sizes = [0u8; 4];
            read_exact_or(&mut buffered, &mut sizes, "an entry is cut off")?;
            let size = usize::from(u16_at(&sizes, 0).unwrap_or(0));
            let name_bytes = usize::from(u16_at(&sizes, 2).unwrap_or(0));
            let fixed: usize = 4 + (4 + if wide { 8 } else { 4 } + 4);
            if name_bytes > MAX_NAME_BYTES || name_bytes % 2 != 0 {
                return Err(IroError::Directory("an entry name has an impossible length"));
            }
            if size < fixed.saturating_add(name_bytes) {
                return Err(IroError::Directory("an entry is shorter than its own fields"));
            }
            let mut body = vec![0u8; size - 4];
            read_exact_or(&mut buffered, &mut body, "an entry is cut off")?;
            let name = decode_name(body.get(..name_bytes).unwrap_or(&[]));
            let mut p = name_bytes;
            let entry_flags = u32_at(&body, p).unwrap_or(0);
            p += 4;
            let offset = if wide {
                let v = u64_at(&body, p).unwrap_or(u64::MAX);
                p += 8;
                if v > i64::MAX as u64 {
                    return Err(IroError::Directory("an entry has a negative offset"));
                }
                v
            } else {
                let v = non_negative(u32_at(&body, p).unwrap_or(u32::MAX)).ok_or(IroError::Directory("an entry has a negative offset"))?;
                p += 4;
                u64::from(v)
            };
            let length = non_negative(u32_at(&body, p).unwrap_or(u32::MAX)).ok_or(IroError::Directory("an entry has a negative length"))?;
            position = position.saturating_add(size as u64);
            if entry_flags & RESERVED_FLAGS != 0 {
                return Err(IroError::ReservedFlags { name, flags: entry_flags });
            }
            let end = offset.saturating_add(u64::from(length));
            if end > file_len {
                return Err(IroError::PastEnd { name, end, len: file_len });
            }
            let key = normalize(&name);
            let slot = entries.len();
            if index.insert(key, slot).is_some() {
                duplicates += 1;
            }
            entries.push(IroEntry { name, flags: entry_flags, offset, length });
        }
        // `position` is where the directory ends; nothing reads it, but a directory that runs past the file
        // would already have failed on a cut-off read above.
        let _ = position;
        Ok(Self { reader: Mutex::new(buffered.into_inner()), file_len, version, flags, entries, index, duplicates })
    }

    /// The version word of the header (`0x10000` to `0x10002`).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The archive flags (bit 0 is the patch flag).
    pub fn archive_flags(&self) -> u32 {
        self.flags
    }

    /// Whether this is a 7th Heaven patch archive (not read as a mod by this project).
    pub fn is_patch(&self) -> bool {
        self.flags & 1 != 0
    }

    /// The file's length in bytes.
    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Entries, in directory order.
    pub fn entries(&self) -> &[IroEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries repeated an earlier name (ignoring case; the last one is what a lookup finds).
    pub fn duplicate_names(&self) -> usize {
        self.duplicates
    }

    /// The entry named `name` (`\` or `/` between folders, any case).
    pub fn find(&self, name: &str) -> Option<&IroEntry> {
        self.index.get(&normalize(name)).and_then(|&i| self.entries.get(i))
    }

    /// Whether the archive has an entry named `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// The payload of `entry`, at most [`MAX_READ_BYTES`].
    pub fn read(&self, entry: &IroEntry) -> Result<Vec<u8>, IroError> {
        self.read_limited(entry, MAX_READ_BYTES)
    }

    /// The payload of `entry`, refusing one over `limit` bytes. Stored entries only.
    pub fn read_limited(&self, entry: &IroEntry, limit: u64) -> Result<Vec<u8>, IroError> {
        match entry.compression() {
            Compression::Stored => {}
            other => return Err(IroError::Unsupported { name: entry.name.clone(), kind: other.label() }),
        }
        let length = u64::from(entry.length);
        if length > limit {
            return Err(IroError::TooLarge { name: entry.name.clone(), length, limit });
        }
        let mut buf = vec![0u8; entry.length as usize];
        let mut reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        reader.seek(SeekFrom::Start(entry.offset))?;
        reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// The payload of the entry named `name`.
    pub fn read_named(&self, name: &str) -> Result<Vec<u8>, IroError> {
        let entry = self.find(name).ok_or_else(|| IroError::NotFound(name.to_string()))?;
        self.read(entry)
    }
}

/// `read_exact` whose end-of-file is a [`IroError::Directory`] with `what`.
fn read_exact_or<T: Read>(reader: &mut T, buf: &mut [u8], what: &'static str) -> Result<(), IroError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(IroError::Directory(what)),
        Err(e) => Err(IroError::Io(e)),
    }
}

/// UTF-16LE to a `String` (an unpaired surrogate becomes U+FFFD).
fn decode_name(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut at = 0usize;
    while let (Some(&lo), Some(&hi)) = (bytes.get(at), bytes.get(at + 1)) {
        units.push(u16::from_le_bytes([lo, hi]));
        at += 2;
    }
    char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

/// Test support: an in-memory `.iro` writer (synthetic names and bytes only), used by this crate's tests and by
/// the integration tests of the crates that build on it.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    use super::SIGNATURE;

    /// One file of a synthetic archive.
    pub struct Spec<'a> {
        pub name: &'a str,
        pub flags: u32,
        pub data: &'a [u8],
    }

    /// Stored files (flags 0).
    pub fn stored<'a>(name: &'a str, data: &'a [u8]) -> Spec<'a> {
        Spec { name, flags: 0, data }
    }

    /// A version `0x10002` archive of `files` (directory right after the header, payloads after it, as 7th
    /// Heaven's writer lays them out).
    pub fn build(files: &[Spec<'_>]) -> Vec<u8> {
        build_version(0x1_0002, files)
    }

    /// An archive of the given version (`0x10000` writes 4-byte offsets).
    pub fn build_version(version: u32, files: &[Spec<'_>]) -> Vec<u8> {
        let wide = version >= 0x1_0001;
        let names: Vec<Vec<u8>> = files.iter().map(|f| f.name.encode_utf16().flat_map(u16::to_le_bytes).collect()).collect();
        let entry_len = |n: &Vec<u8>| 4 + n.len() + 4 + if wide { 8 } else { 4 } + 4;
        let dir_len: usize = 4 + names.iter().map(entry_len).sum::<usize>();
        let mut offset = 16 + dir_len;
        let mut out = Vec::new();
        out.extend_from_slice(&SIGNATURE.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&u32::try_from(files.len()).unwrap_or(0).to_le_bytes());
        for (file, name) in files.iter().zip(&names) {
            out.extend_from_slice(&u16::try_from(entry_len(name)).unwrap_or(0).to_le_bytes());
            out.extend_from_slice(&u16::try_from(name.len()).unwrap_or(0).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&file.flags.to_le_bytes());
            if wide {
                out.extend_from_slice(&(offset as u64).to_le_bytes());
            } else {
                out.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_le_bytes());
            }
            out.extend_from_slice(&u32::try_from(file.data.len()).unwrap_or(0).to_le_bytes());
            offset += file.data.len();
        }
        for file in files {
            out.extend_from_slice(file.data);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{build, build_version, stored, Spec};
    use super::*;
    use std::io::Cursor;

    fn open(bytes: Vec<u8>) -> Result<IroArchive<Cursor<Vec<u8>>>, IroError> {
        IroArchive::open(Cursor::new(bytes))
    }

    #[test]
    fn lists_and_reads_stored_entries_ignoring_case_and_separators() {
        let bytes = build(&[
            stored("mod.xml", b"<ModInfo/>"),
            stored("VO\\Voice\\md1stin\\12.ogg", b"OggS-synthetic-one"),
            stored("Echo-S\\flevel.lgp\\md1stin", b"field-synthetic"),
        ]);
        let a = open(bytes).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a.version(), 0x1_0002);
        assert!(!a.is_patch());
        assert_eq!(a.read_named("MOD.XML").unwrap(), b"<ModInfo/>");
        assert_eq!(a.read_named("vo/voice/MD1STIN/12.OGG").unwrap(), b"OggS-synthetic-one");
        let e = a.find("echo-s\\flevel.lgp\\md1stin").unwrap();
        assert_eq!(e.folder(), "Echo-S\\flevel.lgp");
        assert_eq!(e.file_name(), "md1stin");
        assert_eq!(e.compression(), Compression::Stored);
        assert!(a.find("nothing").is_none());
        assert!(matches!(a.read_named("nothing"), Err(IroError::NotFound(_))));
    }

    #[test]
    fn version_0x10000_has_four_byte_offsets() {
        let bytes = build_version(0x1_0000, &[stored("a", b"AAAA"), stored("b\\c", b"BB")]);
        let a = open(bytes).unwrap();
        assert_eq!(a.version(), 0x1_0000);
        assert_eq!(a.read_named("a").unwrap(), b"AAAA");
        assert_eq!(a.read_named("b\\c").unwrap(), b"BB");
    }

    #[test]
    fn non_ascii_names_round_trip() {
        let a = open(build(&[stored("Voice\\caf\u{e9}\\1.ogg", b"x")])).unwrap();
        assert!(a.contains("voice\\CAF\u{c9}\\1.ogg"));
    }

    #[test]
    fn a_duplicate_name_keeps_the_last_entry() {
        let a = open(build(&[stored("x\\y", b"first"), stored("X\\Y", b"second")])).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a.duplicate_names(), 1);
        assert_eq!(a.read_named("x\\y").unwrap(), b"second");
    }

    #[test]
    fn compressed_entries_are_listed_but_not_decoded() {
        let lzma = Spec { name: "c", flags: 2, data: b"\x04\0\0\0\x05\0\0\0" };
        let lzs = Spec { name: "d", flags: 1, data: b"zz" };
        let weird = Spec { name: "e", flags: 7, data: b"zz" };
        let a = open(build(&[stored("s", b"plain"), lzma, lzs, weird])).unwrap();
        assert_eq!(a.read_named("s").unwrap(), b"plain");
        assert!(matches!(a.read_named("c"), Err(IroError::Unsupported { ref kind, .. }) if kind == "LZMA"));
        assert!(matches!(a.read_named("d"), Err(IroError::Unsupported { ref kind, .. }) if kind == "LZS"));
        assert!(matches!(a.read_named("e"), Err(IroError::Unsupported { .. })));
        assert_eq!(a.find("e").unwrap().compression(), Compression::Other(7));
    }

    #[test]
    fn a_read_over_the_limit_is_refused_before_it_allocates() {
        let a = open(build(&[stored("big", &[7u8; 64])])).unwrap();
        let e = a.find("big").unwrap();
        assert_eq!(a.read_limited(e, 64).unwrap().len(), 64);
        assert!(matches!(a.read_limited(e, 63), Err(IroError::TooLarge { length: 64, limit: 63, .. })));
    }

    #[test]
    fn reserved_flag_bits_refuse_the_archive() {
        let bad = Spec { name: "x", flags: 0x0001_0000, data: b"1" };
        assert!(matches!(open(build(&[bad])), Err(IroError::ReservedFlags { .. })));
        // Bits 4..15 and the top byte are not reserved: 7th Heaven ignores them.
        let fine = Spec { name: "x", flags: 0x0000_0100, data: b"1" };
        assert!(open(build(&[fine])).is_ok());
    }

    #[test]
    fn a_truncated_archive_names_the_entry_that_runs_past_the_end() {
        let mut bytes = build(&[stored("first", b"0123456789"), stored("second\\file.ogg", b"abcdefghij")]);
        bytes.truncate(bytes.len() - 4);
        match open(bytes) {
            Err(IroError::PastEnd { name, end, len }) => {
                assert_eq!(name, "second\\file.ogg");
                assert_eq!(end, len + 4);
            }
            other => panic!("expected PastEnd, got {other:?}"),
        }
    }

    #[test]
    fn a_forwarder_leads_to_the_real_directory() {
        // Header, then at 16 a forwarder (-1, i64 position), the real directory at the end.
        let inner = build(&[stored("moved", b"payload")]);
        let dir_start = 16usize;
        let tail_at = inner.len();
        let mut bytes = inner.clone();
        // Rewrite the first directory slot as a forwarder to a copy of the directory placed at the end.
        let real_count_and_entries = inner.get(dir_start..dir_start + 4 + (4 + 10 + 4 + 8 + 4)).unwrap().to_vec();
        bytes.extend_from_slice(&real_count_and_entries);
        bytes.get_mut(16..20).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());
        bytes.get_mut(20..28).unwrap().copy_from_slice(&(tail_at as u64).to_le_bytes());
        let a = open(bytes).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a.read_named("moved").unwrap(), b"payload");
    }

    #[test]
    fn forwarder_loops_and_wild_forwarders_are_refused() {
        let mut bytes = build(&[stored("x", b"1")]);
        // A forwarder at 16 that points at itself.
        bytes.get_mut(16..20).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());
        bytes.get_mut(20..28).unwrap().copy_from_slice(&16u64.to_le_bytes());
        assert!(matches!(open(bytes.clone()), Err(IroError::Directory(m)) if m.contains("loop")));
        bytes.get_mut(20..28).unwrap().copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(open(bytes), Err(IroError::Directory(_))));
    }

    #[test]
    fn damaged_headers_are_refused_not_trusted() {
        let good = build(&[stored("x", b"1")]);
        // Too short for a header, wrong signature, wrong versions.
        assert!(matches!(open(vec![0; 10]), Err(IroError::Directory(_))));
        let mut b = good.clone();
        b[0] = b'X';
        assert!(matches!(open(b), Err(IroError::BadSignature { .. })));
        for v in [0u32, 0xFFFF, 0x1_0003, u32::MAX] {
            let mut b = good.clone();
            b.get_mut(4..8).unwrap().copy_from_slice(&v.to_le_bytes());
            assert!(matches!(open(b), Err(IroError::BadVersion { found }) if found == v), "version {v:#x}");
        }
        // The directory pointer outside the file, or inside the header.
        for d in [0u32, 8, 15, 1 << 30, u32::MAX] {
            let mut b = good.clone();
            b.get_mut(12..16).unwrap().copy_from_slice(&d.to_le_bytes());
            assert!(matches!(open(b), Err(IroError::Directory(_))), "directory {d}");
        }
    }

    #[test]
    fn an_absurd_entry_count_never_allocates_by_it() {
        for count in [i32::MAX as u32, 1 << 21, 1 << 20, 10_000, u32::MAX - 1] {
            let mut b = build(&[stored("x", b"1")]);
            b.get_mut(16..20).unwrap().copy_from_slice(&count.to_le_bytes());
            assert!(matches!(open(b), Err(IroError::Directory(_))), "count {count}");
        }
    }

    #[test]
    fn broken_entries_are_refused() {
        let good = build(&[stored("ab", b"1")]);
        // Entry at 20: u16 size, u16 name bytes.
        let patch = |at: usize, v: u16| {
            let mut b = good.clone();
            b.get_mut(at..at + 2).unwrap().copy_from_slice(&v.to_le_bytes());
            b
        };
        assert!(matches!(open(patch(20, 3)), Err(IroError::Directory(_))), "size under the fixed fields");
        assert!(matches!(open(patch(22, 5)), Err(IroError::Directory(_))), "odd name length");
        assert!(matches!(open(patch(22, 8192)), Err(IroError::Directory(_))), "name over the cap");
        assert!(matches!(open(patch(22, 2000)), Err(IroError::Directory(_))), "name longer than the entry");
        // Negative length and negative offset.
        let name_end = 24 + 4;
        let mut b = good.clone();
        b.get_mut(name_end + 12..name_end + 16).unwrap().copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(open(b), Err(IroError::Directory(m)) if m.contains("length")));
        let mut b = good.clone();
        b.get_mut(name_end + 4..name_end + 12).unwrap().copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(open(b), Err(IroError::Directory(m)) if m.contains("offset")));
    }

    #[test]
    fn a_cut_off_directory_is_an_error() {
        let good = build(&[stored("one", b"1"), stored("two", b"2")]);
        for cut in [17usize, 19, 22, 30, 40] {
            let bytes = good.get(..cut).unwrap().to_vec();
            assert!(open(bytes).is_err(), "cut at {cut}");
        }
    }

    /// Mutated and random archives never panic and never allocate by what they claim.
    #[test]
    fn mutated_and_random_input_never_panics() {
        let good = build(&[stored("a\\b\\c.ogg", b"0123"), stored("mod.xml", b"<x/>"), Spec { name: "z", flags: 2, data: b"12345678" }]);
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..4000 {
            let mut bytes = good.clone();
            for _ in 0..1 + round % 6 {
                let at = (next() % bytes.len() as u64) as usize;
                if let Some(b) = bytes.get_mut(at) {
                    *b = (next() & 0xFF) as u8;
                }
            }
            if round % 5 == 0 {
                bytes.truncate((next() % (bytes.len() as u64 + 1)) as usize);
            }
            if let Ok(a) = open(bytes) {
                for e in a.entries().to_vec() {
                    let _ = a.read_limited(&e, 1 << 20);
                }
            }
        }
        for _ in 0..500 {
            let n = (next() % 200) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let _ = open(bytes);
        }
        // Random bytes behind a valid header too.
        for _ in 0..500 {
            let mut bytes = good.get(..16).unwrap().to_vec();
            let n = (next() % 300) as usize;
            bytes.extend((0..n).map(|_| (next() & 0xFF) as u8));
            let _ = open(bytes);
        }
    }

    #[test]
    fn normalize_ignores_case_and_slashes() {
        assert_eq!(normalize("A/B\\C"), "a\\b\\c");
        assert_eq!(normalize("Vo\\Voice"), normalize("vo/voice"));
    }
}
