//! The content-addressed library and its import API.
//!
//! Layout under the state directory:
//!
//! ```text
//! <state>/library/blobs/<first 2 hex>/<blake3 hex>   content-addressed, deduplicated, read-only
//! <state>/library/sets/<set_id>/manifest.json        provenance + entries (atomically replaced)
//! <state>/library/sets/<set_id>/.lock                advisory lock held while a set is written
//! <state>/library/tmp/                               in-flight blob copies (same filesystem as blobs)
//! <state>/library/.gitignore                         `*`: owned game data never gets committed
//! ```
//!
//! Originals are only ever opened read-only. Every import verifies that the source did not
//! change while it was being copied. A blob that is already stored is only reused after its
//! content hash matches (size alone is not proof), and a damaged one is replaced.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fmv_core::{EditionProvenance, SourceRef};

use crate::error::{io_err, LibraryError};
use crate::fsutil::{
    atomic_write, create_private_dir_all, open_new_private, sync_dir, unique_suffix,
};
use crate::manifest::{
    read_manifest, write_manifest, Identification, Manifest, ManifestEntry, MANIFEST_FILE,
    MAX_MANIFEST_ENTRIES,
};
use crate::paths::{
    describe_provenance, is_blake3_hex, normalize_logical_path, resolve_state_dir, set_id_for,
    validate_set_id,
};

/// Largest single file the library will import (bytes).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024;
/// Default `importer_version` recorded in manifests.
pub const LIBRARY_VERSION: &str = concat!("fmv-library ", env!("CARGO_PKG_VERSION"));
const COPY_BUF: usize = 1 << 20;
const LOCK_FILE: &str = ".lock";
/// A `blob-*.part` file younger than this is never swept: it may belong to a copy that is only
/// just starting (between creating the file and locking it, a window of microseconds; the lock,
/// not this age, is what protects a running copy). Kept short so that a re-run right after a
/// killed import already reclaims its leftovers.
const STALE_PART_AFTER: Duration = Duration::from_secs(10);
const GITIGNORE_FILE: &str = ".gitignore";
const GITIGNORE_TEXT: &[u8] = b"# fmv-library: copies of the owner's game data. Never commit.\n*\n";

/// Size, mtime and (on unix) device+inode of a source file. Two stamps taken around a copy
/// must be equal, otherwise the source changed under us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
    #[cfg(unix)]
    pub dev: u64,
    #[cfg(unix)]
    pub ino: u64,
}

impl SourceStamp {
    pub fn from_metadata(m: &fs::Metadata) -> Self {
        SourceStamp {
            len: m.len(),
            modified: m.modified().ok(),
            #[cfg(unix)]
            dev: std::os::unix::fs::MetadataExt::dev(m),
            #[cfg(unix)]
            ino: std::os::unix::fs::MetadataExt::ino(m),
        }
    }

    pub fn capture(path: &Path) -> Result<Self, LibraryError> {
        fs::metadata(path)
            .map(|m| Self::from_metadata(&m))
            .map_err(io_err(path))
    }

    /// Err(`SourceChanged`) unless `path` still has exactly this stamp.
    pub fn verify(&self, path: &Path) -> Result<(), LibraryError> {
        match fs::metadata(path) {
            Ok(m) if Self::from_metadata(&m) == *self => Ok(()),
            _ => Err(LibraryError::SourceChanged(path.to_path_buf())),
        }
    }
}

/// Destination set: its id plus the provenance the caller claims for what it is adding.
/// `import_file` refuses the add if that provenance is incompatible with the set's manifest.
#[derive(Debug, Clone)]
pub struct SetHandle {
    pub set_id: String,
    pub provenance: EditionProvenance,
    /// How far `provenance` can be trusted; stored in the manifest (see [`Identification`]).
    pub identification: Option<Identification>,
}

impl SetHandle {
    /// Handle for the set that `provenance` identifies (id derived by [`set_id_for`]).
    pub fn new(provenance: EditionProvenance) -> Self {
        SetHandle {
            set_id: set_id_for(&provenance),
            provenance,
            identification: None,
        }
    }

    /// Handle for an explicit (existing) set id. The id is validated when used.
    pub fn with_id(set_id: impl Into<String>, provenance: EditionProvenance) -> Self {
        SetHandle {
            set_id: set_id.into(),
            provenance,
            identification: None,
        }
    }

    /// Say how far the provenance labels can be trusted (guessed, assumed, checked).
    pub fn with_identification(mut self, identification: Identification) -> Self {
        self.identification = Some(identification);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    /// A new logical path was added to the manifest.
    Added,
    /// The logical path already existed with identical content; nothing changed.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub logical_path: String,
    pub blake3: String,
    pub size: u64,
    /// false when identical, verified content was already stored (deduplicated).
    pub blob_created: bool,
    /// true when a blob of that name was already stored but damaged (wrong size or wrong
    /// content hash) and was replaced by the freshly verified copy. Implies `blob_created`.
    pub blob_repaired: bool,
    pub entry: EntryStatus,
}

/// What `store_blob` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobStore {
    Created,
    Deduplicated,
    Repaired,
}

#[derive(Debug, Clone)]
pub struct Library {
    state_dir: PathBuf,
    root: PathBuf,
    root_canonical: PathBuf,
    max_file_size: u64,
    importer_version: String,
    max_entries: usize,
    stale_parts_removed: u64,
}

impl Library {
    /// Open (creating if needed) the library under `state_dir`. Also reclaims in-flight blob
    /// copies left behind by an import that was killed (see [`Library::stale_parts_removed`]).
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let state_dir = state_dir.as_ref().to_path_buf();
        let root = state_dir.join("library");
        for sub in ["blobs", "sets", "tmp"] {
            let p = root.join(sub);
            create_private_dir_all(&p).map_err(io_err(&p))?;
        }
        // Best effort: keep copies of the owner's game data out of git even when the state
        // directory sits inside a work tree. Failing to write this file must not fail the open.
        let gitignore = root.join(GITIGNORE_FILE);
        if !gitignore.exists() {
            let _ = atomic_write(&gitignore, GITIGNORE_TEXT);
        }
        let mut lib = Self::from_root(state_dir, root)?;
        lib.stale_parts_removed = sweep_stale_parts(&lib.root.join("tmp"), SystemTime::now());
        Ok(lib)
    }

    /// Open an existing library without creating anything (for read-only commands).
    pub fn open_existing(state_dir: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let state_dir = state_dir.as_ref().to_path_buf();
        let root = state_dir.join("library");
        if !root.join("sets").is_dir() {
            return Err(LibraryError::NoLibrary(root));
        }
        Self::from_root(state_dir, root)
    }

    /// [`Library::open`] on the resolved state dir (explicit, `$FMV_STATE_DIR`, user data dir).
    pub fn open_default(explicit: Option<&Path>) -> Result<Self, LibraryError> {
        Self::open(resolve_state_dir(explicit)?)
    }

    fn from_root(state_dir: PathBuf, root: PathBuf) -> Result<Self, LibraryError> {
        let root_canonical = fs::canonicalize(&root).map_err(io_err(&root))?;
        Ok(Library {
            state_dir,
            root,
            root_canonical,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            importer_version: LIBRARY_VERSION.to_string(),
            max_entries: MAX_MANIFEST_ENTRIES,
            stale_parts_removed: 0,
        })
    }

    pub fn with_max_file_size(mut self, bytes: u64) -> Self {
        self.max_file_size = bytes;
        self
    }

    pub fn with_importer_version(mut self, version: impl Into<String>) -> Self {
        self.importer_version = version.into();
        self
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn library_dir(&self) -> &Path {
        &self.root
    }

    pub fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    /// How many leftover in-flight copies (`tmp/blob-*.part` of killed imports) [`Library::open`]
    /// found and deleted. Always 0 for [`Library::open_existing`], which never writes.
    pub fn stale_parts_removed(&self) -> u64 {
        self.stale_parts_removed
    }

    /// Test seam: a small entry cap, so the cap can be exercised without half a million files.
    #[cfg(test)]
    pub(crate) fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    /// Path of the blob with this digest (the file may not exist).
    pub fn blob_path(&self, blake3_hex: &str) -> Result<PathBuf, LibraryError> {
        if !is_blake3_hex(blake3_hex) {
            return Err(LibraryError::BadDigest(
                blake3_hex.chars().take(80).collect(),
            ));
        }
        let shard = blake3_hex.get(..2).unwrap_or("00");
        Ok(self.root.join("blobs").join(shard).join(blake3_hex))
    }

    pub fn set_dir(&self, set_id: &str) -> Result<PathBuf, LibraryError> {
        validate_set_id(set_id)?;
        Ok(self.root.join("sets").join(set_id))
    }

    /// Err(`SourceInsideLibrary`) when `path` (symlinks resolved) lives under the library dir.
    pub fn ensure_not_inside_library(&self, path: &Path) -> Result<(), LibraryError> {
        let canonical = fs::canonicalize(path).map_err(io_err(path))?;
        if canonical.starts_with(&self.root_canonical) {
            return Err(LibraryError::SourceInsideLibrary(path.to_path_buf()));
        }
        Ok(())
    }

    /// Import one file into `set`: stream it (read-only open), hash while copying to a temp blob,
    /// rename into place if the content is new, and record the manifest entry atomically.
    pub fn import_file(
        &self,
        set: &SetHandle,
        logical_path: &str,
        source_path: &Path,
        source: SourceRef,
        imported_at: u64,
    ) -> Result<ImportOutcome, LibraryError> {
        let mut batch = self.begin_import(set, imported_at)?;
        let outcome = batch.add_file(logical_path, source_path, source)?;
        batch.commit()?;
        Ok(outcome)
    }

    /// Start a multi-file import into `set`. Takes the set's exclusive lock; the manifest is
    /// rewritten only at [`ImportBatch::checkpoint`] / [`ImportBatch::commit`].
    pub fn begin_import(
        &self,
        set: &SetHandle,
        imported_at: u64,
    ) -> Result<ImportBatch<'_>, LibraryError> {
        let set_dir = self.set_dir(&set.set_id)?;
        create_private_dir_all(&set_dir).map_err(io_err(&set_dir))?;
        let lock_path = set_dir.join(LOCK_FILE);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(io_err(&lock_path))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(LibraryError::SetBusy(set.set_id.clone()))
            }
            Err(fs::TryLockError::Error(e)) => return Err(io_err(&lock_path)(e)),
        }
        let (manifest, identification_changed) =
            match read_manifest(&set_dir.join(MANIFEST_FILE), &set.set_id) {
                Ok(mut existing) => {
                    check_compatible(&set.set_id, &existing.provenance, &set.provenance)?;
                    let changed =
                        merge_identification(&mut existing.identification, &set.identification);
                    (existing, changed)
                }
                Err(LibraryError::SetNotFound(_)) => (
                    Manifest::new(
                        &set.set_id,
                        &set.provenance,
                        &self.importer_version,
                        set.identification.clone(),
                    ),
                    false,
                ),
                Err(e) => return Err(e),
            };
        let index = manifest
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.logical_path.clone(), i))
            .collect();
        Ok(ImportBatch {
            lib: self,
            set_id: set.set_id.clone(),
            set_dir,
            manifest,
            index,
            imported_at,
            // a changed identification record is written even when no entry changes; an
            // unchanged one never rewrites the manifest (re-imports stay idempotent)
            dirty: identification_changed,
            _lock: lock,
            #[cfg(test)]
            after_copy_hook: None,
        })
    }

    /// Ids of all sets that have a manifest, sorted.
    pub fn list_sets(&self) -> Result<Vec<String>, LibraryError> {
        let dir = self.root.join("sets");
        let mut out = Vec::new();
        for ent in fs::read_dir(&dir).map_err(io_err(&dir))? {
            let ent = ent.map_err(io_err(&dir))?;
            let Ok(name) = ent.file_name().into_string() else {
                continue;
            };
            if validate_set_id(&name).is_ok() && ent.path().join(MANIFEST_FILE).is_file() {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn load_manifest(&self, set_id: &str) -> Result<Manifest, LibraryError> {
        let dir = self.set_dir(set_id)?;
        read_manifest(&dir.join(MANIFEST_FILE), set_id)
    }

    /// Path of the blob holding `logical_path` in `set_id`, for reading. `Ok(None)` when the set
    /// exists but has no such entry. The blob's size is checked against the manifest.
    pub fn get(&self, set_id: &str, logical_path: &str) -> Result<Option<PathBuf>, LibraryError> {
        let manifest = self.load_manifest(set_id)?;
        match lookup(&manifest, logical_path) {
            Some(entry) => self.verified_blob_path(&manifest, entry).map(Some),
            None => Ok(None),
        }
    }

    /// Open the blob for `logical_path` read-only. Errors with `EntryNotFound` if absent.
    pub fn open_reader(&self, set_id: &str, logical_path: &str) -> Result<File, LibraryError> {
        let manifest = self.load_manifest(set_id)?;
        let entry = lookup(&manifest, logical_path).ok_or_else(|| LibraryError::EntryNotFound {
            set_id: set_id.to_string(),
            logical_path: logical_path.chars().take(200).collect(),
        })?;
        let path = self.verified_blob_path(&manifest, entry)?;
        File::open(&path).map_err(io_err(&path))
    }

    /// Re-hash the blob behind `logical_path` and compare it with the manifest's blake3.
    ///
    /// [`Library::get`] and [`Library::open_reader`] only check the blob's size (they sit on the
    /// hot path of the runtime); this is the full content check. A blob that fails it is healed
    /// by re-importing the (still good) original.
    pub fn verify_entry(&self, set_id: &str, logical_path: &str) -> Result<(), LibraryError> {
        let manifest = self.load_manifest(set_id)?;
        let entry = lookup(&manifest, logical_path).ok_or_else(|| LibraryError::EntryNotFound {
            set_id: set_id.to_string(),
            logical_path: logical_path.chars().take(200).collect(),
        })?;
        self.verify_blob_content(&manifest, entry)
    }

    /// [`Library::verify_entry`] for every entry of a set. Each distinct blob is hashed once,
    /// but every logical path that points at a damaged blob is reported. Returns the problems
    /// found (empty when the whole set is intact).
    pub fn verify_set(&self, set_id: &str) -> Result<Vec<LibraryError>, LibraryError> {
        let manifest = self.load_manifest(set_id)?;
        let mut checked: HashMap<&str, Option<String>> = HashMap::new();
        let mut problems = Vec::new();
        for entry in &manifest.entries {
            let reason = checked
                .entry(entry.blake3.as_str())
                .or_insert_with(|| self.content_problem(entry))
                .clone();
            if let Some(reason) = reason {
                problems.push(LibraryError::BlobProblem {
                    set_id: manifest.set_id.clone(),
                    logical_path: entry.logical_path.clone(),
                    reason,
                });
            }
        }
        Ok(problems)
    }

    fn verify_blob_content(
        &self,
        manifest: &Manifest,
        entry: &ManifestEntry,
    ) -> Result<(), LibraryError> {
        match self.content_problem(entry) {
            None => Ok(()),
            Some(reason) => Err(LibraryError::BlobProblem {
                set_id: manifest.set_id.clone(),
                logical_path: entry.logical_path.clone(),
                reason,
            }),
        }
    }

    /// Why the blob of `entry` cannot be trusted (missing, wrong size, wrong content hash), or
    /// `None` when it is intact. Hashes the whole blob.
    fn content_problem(&self, entry: &ManifestEntry) -> Option<String> {
        let path = match self.blob_path(&entry.blake3) {
            Ok(p) => p,
            Err(e) => return Some(e.to_string()),
        };
        match fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == entry.size => {}
            Ok(m) if m.is_file() => {
                return Some(format!(
                    "blob is {} bytes but the manifest says {}",
                    m.len(),
                    entry.size
                ))
            }
            Ok(_) => return Some("blob is not a regular file".to_string()),
            Err(e) => return Some(format!("cannot stat blob: {e}")),
        }
        match file_blake3_hex(&path) {
            Ok(actual) if actual == entry.blake3 => None,
            Ok(actual) => Some(format!(
                "content hash {} does not match the manifest's {}",
                actual.get(..16).unwrap_or(&actual),
                entry.blake3.get(..16).unwrap_or(&entry.blake3)
            )),
            Err(e) => Some(format!("cannot read blob: {e}")),
        }
    }

    fn verified_blob_path(
        &self,
        manifest: &Manifest,
        entry: &ManifestEntry,
    ) -> Result<PathBuf, LibraryError> {
        let path = self.blob_path(&entry.blake3)?;
        let problem = |reason: String| LibraryError::BlobProblem {
            set_id: manifest.set_id.clone(),
            logical_path: entry.logical_path.clone(),
            reason,
        };
        match fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == entry.size => Ok(path),
            Ok(m) if m.is_file() => Err(problem(format!(
                "blob is {} bytes but the manifest says {}",
                m.len(),
                entry.size
            ))),
            Ok(_) => Err(problem("blob is not a regular file".to_string())),
            Err(e) => Err(problem(format!("cannot stat blob: {e}"))),
        }
    }

    /// Stream `reader` into a temp blob in `library/tmp`, hashing as it goes.
    fn copy_into_tmp(
        &self,
        reader: &mut dyn Read,
        what: &str,
    ) -> Result<PendingBlob, LibraryError> {
        let tmp_dir = self.root.join("tmp");
        create_private_dir_all(&tmp_dir).map_err(io_err(&tmp_dir))?;
        let tmp_path = tmp_dir.join(format!("blob-{}.part", unique_suffix()));
        let mut file = open_new_private(&tmp_path).map_err(io_err(&tmp_path))?;
        let guard = TempGuard {
            path: tmp_path.clone(),
            armed: true,
        };
        // Mark the copy as live: `sweep_stale_parts` never removes a locked file, and the OS
        // drops the lock when this process dies however it dies. Best effort: some filesystems
        // have no advisory locks, and an import must not depend on them.
        let _ = file.try_lock();
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; COPY_BUF];
        let mut total: u64 = 0;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(LibraryError::SourceRead(e)),
            };
            let chunk = buf.get(..n).ok_or_else(|| {
                LibraryError::SourceRead(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reader reported more bytes than its buffer holds",
                ))
            })?;
            total = total.saturating_add(n as u64);
            if total > self.max_file_size {
                return Err(LibraryError::TooLarge {
                    what: what.to_string(),
                    size: total,
                    limit: self.max_file_size,
                });
            }
            hasher.update(chunk);
            file.write_all(chunk).map_err(io_err(&tmp_path))?;
        }
        file.flush().map_err(io_err(&tmp_path))?;
        file.sync_all().map_err(io_err(&tmp_path))?;
        // Blobs are immutable: make the file read-only before it becomes visible.
        let mut perms = file.metadata().map_err(io_err(&tmp_path))?.permissions();
        perms.set_readonly(true);
        file.set_permissions(perms).map_err(io_err(&tmp_path))?;
        Ok(PendingBlob {
            guard,
            hash_hex: hasher.finalize().to_hex().to_string(),
            size: total,
            _live: file, // keeps the lock until the blob is stored or discarded
        })
    }

    /// Move a finished temp blob into its content-addressed place unless identical content is
    /// already stored. A blob that is already there is only trusted after its content hash was
    /// checked: a same-size blob with different bytes (bit rot, a stray write) is replaced.
    fn store_blob(&self, mut pending: PendingBlob) -> Result<BlobStore, LibraryError> {
        let dest = self.blob_path(&pending.hash_hex)?;
        let mut repaired = false;
        match fs::symlink_metadata(&dest) {
            Ok(m) if m.is_file() && m.len() == pending.size => {
                if file_blake3_hex(&dest).is_ok_and(|h| h == pending.hash_hex) {
                    return Ok(BlobStore::Deduplicated); // the temp file is removed by its guard
                }
                repaired = true;
            }
            // wrong size, a symlink or some other node: the verified new copy replaces it
            Ok(_) => repaired = true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&dest)(e)),
        }
        if let Some(shard) = dest.parent() {
            create_private_dir_all(shard).map_err(io_err(shard))?;
        }
        // rename() replaces a damaged blob atomically on unix; elsewhere a refusal is reported
        fs::rename(&pending.guard.path, &dest).map_err(io_err(&dest))?;
        pending.guard.armed = false;
        if let Some(shard) = dest.parent() {
            sync_dir(shard);
        }
        Ok(if repaired {
            BlobStore::Repaired
        } else {
            BlobStore::Created
        })
    }
}

/// blake3 (64 lowercase hex characters) of a whole file, streamed. The file is opened read-only.
/// The importers use it to re-read a source after copying it, as evidence that it did not change.
pub fn hash_file(path: &Path) -> Result<String, LibraryError> {
    file_blake3_hex(path).map_err(io_err(path))
}

/// blake3 of a whole file, streamed.
fn file_blake3_hex(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Remove in-flight blob copies whose writer is gone. Ctrl-C, SIGKILL and crashes run no
/// destructors, so [`TempGuard`] alone would leak up to `max_file_size` per interrupted copy.
///
/// A copy is live while its writer holds an advisory lock on its `.part` file, and the OS
/// releases that lock when the process dies. A file is removed only when it is (a) a regular
/// `blob-*.part` file, (b) older than [`STALE_PART_AFTER`] and (c) lockable here, which means
/// nobody holds it. Anything uncertain (no lock support, unreadable mtime) is left alone.
fn sweep_stale_parts(tmp_dir: &Path, now: SystemTime) -> u64 {
    let Ok(entries) = fs::read_dir(tmp_dir) else {
        return 0;
    };
    let mut removed = 0u64;
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with("blob-") && name.ends_with(".part")) {
            continue;
        }
        let path = ent.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= STALE_PART_AFTER);
        if !old_enough {
            continue;
        }
        let Ok(file) = File::open(&path) else {
            continue;
        };
        if file.try_lock().is_err() {
            continue; // a live writer holds it (or locks are unsupported here)
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
        drop(file);
    }
    removed
}

/// Merge a new import's identification into the stored one. Returns true when the stored value
/// changed (and the manifest therefore has to be rewritten).
fn merge_identification(stored: &mut Option<Identification>, incoming: &Option<Identification>) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };
    let merged = match stored.as_ref() {
        None => incoming.clone(),
        Some(existing) => existing.merged_with(incoming),
    };
    if stored.as_ref() == Some(&merged) {
        return false;
    }
    *stored = Some(merged);
    true
}

fn lookup<'m>(manifest: &'m Manifest, logical_path: &str) -> Option<&'m ManifestEntry> {
    let key = normalize_logical_path(logical_path, manifest.provenance.platform).ok()?;
    manifest
        .find(&key)
        .or_else(|| manifest.find_ignore_case(&key))
}

fn check_compatible(
    set_id: &str,
    existing: &EditionProvenance,
    incoming: &EditionProvenance,
) -> Result<(), LibraryError> {
    // `compatible_with` (core) covers game/platform/edition/region/revision. Language and the
    // modded flag are additional set discriminators in this library.
    if existing.compatible_with(incoming)
        && existing.language == incoming.language
        && existing.modded == incoming.modded
    {
        return Ok(());
    }
    Err(LibraryError::IncompatibleSet {
        set_id: set_id.to_string(),
        existing: describe_provenance(existing),
        incoming: describe_provenance(incoming),
    })
}

struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct PendingBlob {
    guard: TempGuard,
    hash_hex: String,
    size: u64,
    /// The `.part` file's handle: holding it keeps the advisory lock that marks the copy live.
    _live: File,
}

/// A multi-file import into one set. Holds the set's exclusive lock until dropped. Entries
/// become durable at [`checkpoint`](Self::checkpoint) / [`commit`](Self::commit); dropping the
/// batch without committing discards entries added since the last checkpoint (blobs already
/// stored stay in the content-addressed store and are reused by a retry).
pub struct ImportBatch<'a> {
    lib: &'a Library,
    set_id: String,
    set_dir: PathBuf,
    manifest: Manifest,
    index: HashMap<String, usize>,
    imported_at: u64,
    dirty: bool,
    _lock: File,
    /// Test seam: runs after a file was copied and before the source is re-verified.
    #[cfg(test)]
    pub(crate) after_copy_hook: Option<Box<dyn FnMut() + 'a>>,
}

impl ImportBatch<'_> {
    pub fn set_id(&self) -> &str {
        &self.set_id
    }

    /// The in-memory manifest (including entries not yet checkpointed). Until the next
    /// checkpoint its entries are in insertion order, not sorted, so [`Manifest::find`] (a
    /// binary search) is unreliable on it: use [`ImportBatch::find_entry`] for lookups.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Exact lookup by logical path that also sees entries not yet checkpointed.
    pub fn find_entry(&self, logical_path: &str) -> Option<&ManifestEntry> {
        let key = normalize_logical_path(logical_path, self.manifest.provenance.platform).ok()?;
        self.index
            .get(&key)
            .and_then(|&i| self.manifest.entries.get(i))
    }

    /// Import a file from the filesystem. The source is opened read-only; its size, mtime and
    /// identity are compared before and after the copy and the file is rejected if they differ.
    pub fn add_file(
        &mut self,
        logical_path: &str,
        source_path: &Path,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        let logical = normalize_logical_path(logical_path, self.manifest.provenance.platform)?;
        self.lib.ensure_not_inside_library(source_path)?;
        // stat first: a FIFO or device must never be opened (it could block or never end)
        let meta = fs::metadata(source_path).map_err(io_err(source_path))?;
        if !meta.is_file() {
            return Err(LibraryError::NotRegularFile(source_path.to_path_buf()));
        }
        self.check_size(source_path.display(), meta.len())?;
        let mut file = File::open(source_path).map_err(io_err(source_path))?;
        let opened = file.metadata().map_err(io_err(source_path))?;
        if !opened.is_file() {
            return Err(LibraryError::NotRegularFile(source_path.to_path_buf()));
        }
        self.check_size(source_path.display(), opened.len())?;
        let before = SourceStamp::from_metadata(&opened);

        let pending = self
            .lib
            .copy_into_tmp(&mut file, &source_path.display().to_string())?;

        #[cfg(test)]
        if let Some(hook) = self.after_copy_hook.as_mut() {
            hook();
        }

        let after_handle =
            SourceStamp::from_metadata(&file.metadata().map_err(io_err(source_path))?);
        if after_handle != before || pending.size != before.len {
            return Err(LibraryError::SourceChanged(source_path.to_path_buf()));
        }
        before.verify(source_path)?;
        self.finish(logical, pending, source)
    }

    /// Import a stream that is not a filesystem path (a file extracted from a disc image).
    /// `expected_size`, when given, must match the number of bytes the stream yields. The
    /// caller is responsible for guarding the underlying container (see [`SourceStamp`]).
    pub fn add_reader(
        &mut self,
        logical_path: &str,
        reader: &mut dyn Read,
        expected_size: Option<u64>,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        let logical = normalize_logical_path(logical_path, self.manifest.provenance.platform)?;
        if let Some(n) = expected_size {
            self.check_size(&logical, n)?;
        }
        let pending = self.lib.copy_into_tmp(reader, &logical)?;
        if let Some(expected) = expected_size {
            if pending.size != expected {
                return Err(LibraryError::SizeMismatch {
                    logical_path: logical,
                    expected,
                    actual: pending.size,
                });
            }
        }
        self.finish(logical, pending, source)
    }

    fn check_size(&self, what: impl std::fmt::Display, size: u64) -> Result<(), LibraryError> {
        if size > self.lib.max_file_size {
            return Err(LibraryError::TooLarge {
                what: what.to_string(),
                size,
                limit: self.lib.max_file_size,
            });
        }
        Ok(())
    }

    fn finish(
        &mut self,
        logical: String,
        pending: PendingBlob,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        // The writer enforces the reader's entry cap: a set must never reach a state that
        // `load_manifest` would refuse. Checked BEFORE touching the store.
        if !self.index.contains_key(&logical) && self.manifest.entries.len() >= self.lib.max_entries
        {
            return Err(LibraryError::TooManyEntries {
                set_id: self.set_id.clone(),
                limit: self.lib.max_entries,
            });
        }
        // Check for a conflicting entry BEFORE touching the store.
        if let Some(existing) = self
            .index
            .get(&logical)
            .and_then(|&i| self.manifest.entries.get(i))
        {
            if existing.blake3 != pending.hash_hex || existing.size != pending.size {
                return Err(LibraryError::EntryConflict {
                    set_id: self.set_id.clone(),
                    logical_path: logical,
                    existing: existing.blake3.clone(),
                    incoming: pending.hash_hex.clone(),
                });
            }
        }
        let hash_hex = pending.hash_hex.clone();
        let size = pending.size;
        let stored = self.lib.store_blob(pending)?;
        let status = match self.index.get(&logical).copied() {
            Some(_) => {
                // identical content at this path: just remember the extra source, if new
                self.note_source(&source);
                EntryStatus::Unchanged
            }
            None => {
                self.note_source(&source);
                self.manifest.entries.push(ManifestEntry {
                    logical_path: logical.clone(),
                    blake3: hash_hex.clone(),
                    size,
                    source,
                });
                self.index.insert(
                    logical.clone(),
                    self.manifest.entries.len().saturating_sub(1),
                );
                self.dirty = true;
                EntryStatus::Added
            }
        };
        Ok(ImportOutcome {
            logical_path: logical,
            blake3: hash_hex,
            size,
            blob_created: stored != BlobStore::Deduplicated,
            blob_repaired: stored == BlobStore::Repaired,
            entry: status,
        })
    }

    fn note_source(&mut self, source: &SourceRef) {
        if !self.manifest.provenance.sources.contains(source) {
            self.manifest.provenance.sources.push(source.clone());
            self.dirty = true;
        }
    }

    /// Persist the manifest now if anything changed since the last checkpoint.
    pub fn checkpoint(&mut self) -> Result<(), LibraryError> {
        if !self.dirty {
            return Ok(());
        }
        self.manifest.sort_entries();
        self.index = self
            .manifest
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.logical_path.clone(), i))
            .collect();
        self.manifest.imported_at = self.imported_at;
        self.manifest.importer_version = self.lib.importer_version.clone();
        write_manifest(&self.set_dir.join(MANIFEST_FILE), &self.manifest)?;
        self.dirty = false;
        Ok(())
    }

    /// Checkpoint and release the lock, returning the final manifest.
    pub fn commit(mut self) -> Result<Manifest, LibraryError> {
        self.checkpoint()?;
        Ok(self.manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MANIFEST_FILE;
    use fmv_core::{GameId, Platform};
    use std::io::Cursor;

    fn pc_prov(region: &str) -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: "PC 1998".into(),
            region: region.into(),
            language: "en".into(),
            revision: "unknown".into(),
            sources: vec![],
            modded: false,
        }
    }

    fn install(path: &Path) -> SourceRef {
        SourceRef::Install {
            path: path.display().to_string(),
        }
    }

    fn setup() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::open(tmp.path().join("state")).unwrap();
        (tmp, lib)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn count_files(dir: &Path) -> usize {
        let mut n = 0;
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += count_files(&p);
                } else {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn dedups_identical_content_and_is_idempotent() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"hello world");
        let b = write(&src, "b.bin", b"hello world");
        let c = write(&src, "c.bin", b"different");
        let set = SetHandle::new(pc_prov("unknown"));
        let from = install(&src);

        let o1 = lib
            .import_file(&set, "Data/A.BIN", &a, from.clone(), 100)
            .unwrap();
        assert_eq!(o1.logical_path, "data/a.bin");
        assert_eq!(o1.entry, EntryStatus::Added);
        assert!(o1.blob_created);
        assert_eq!(o1.blake3, blake3::hash(b"hello world").to_hex().to_string());

        let o2 = lib
            .import_file(&set, "data/b.bin", &b, from.clone(), 101)
            .unwrap();
        assert!(!o2.blob_created, "identical content must reuse the blob");
        assert_eq!(o2.blake3, o1.blake3);
        let o3 = lib
            .import_file(&set, "data/c.bin", &c, from.clone(), 102)
            .unwrap();
        assert!(o3.blob_created);

        assert_eq!(count_files(&lib.library_dir().join("blobs")), 2);
        assert_eq!(
            count_files(&lib.library_dir().join("tmp")),
            0,
            "no temp blobs left behind"
        );

        // re-importing the same file is a no-op and does not rewrite the manifest
        let again = lib.import_file(&set, "data/a.bin", &a, from, 200).unwrap();
        assert_eq!(again.entry, EntryStatus::Unchanged);
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.entries.len(), 3);
        assert_eq!(m.imported_at, 102);
        assert_eq!(m.total_bytes(), 11 + 11 + 9);
        // sorted
        let names: Vec<_> = m.entries.iter().map(|e| e.logical_path.as_str()).collect();
        assert_eq!(names, ["data/a.bin", "data/b.bin", "data/c.bin"]);
        // no manifest temp files either
        let leftovers: Vec<_> = fs::read_dir(lib.set_dir(&set.set_id).unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn manifest_get_and_reader_round_trip() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "flevel.lgp", b"PLACEHOLDER-NOT-REAL-DATA");
        let before = SourceStamp::capture(&a).unwrap();
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/field/flevel.lgp", &a, install(&src), 7)
            .unwrap();
        before.verify(&a).unwrap(); // the original was not touched
        assert_eq!(fs::read(&a).unwrap(), b"PLACEHOLDER-NOT-REAL-DATA");

        assert_eq!(lib.list_sets().unwrap(), vec![set.set_id.clone()]);
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.format_version, 1);
        assert_eq!(m.set_id, set.set_id);
        assert_eq!(m.imported_at, 7);
        assert!(m.importer_version.starts_with("fmv-library "));
        assert_eq!(m.provenance.sources, vec![install(&src)]);
        assert_eq!(
            m.entries[0].fingerprint().unwrap().to_hex(),
            m.entries[0].blake3
        );

        let blob = lib
            .get(&set.set_id, "data/field/flevel.lgp")
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&blob).unwrap(), b"PLACEHOLDER-NOT-REAL-DATA");
        // lookups normalise like imports do
        assert_eq!(
            lib.get(&set.set_id, "DATA\\Field\\FLEVEL.LGP")
                .unwrap()
                .unwrap(),
            blob
        );
        assert!(lib
            .get(&set.set_id, "data/field/char.lgp")
            .unwrap()
            .is_none());
        assert!(matches!(
            lib.get("no-such-set-00000000", "x"),
            Err(LibraryError::SetNotFound(_))
        ));
        assert!(matches!(
            lib.get("../etc", "x"),
            Err(LibraryError::BadSetId(_))
        ));
        let mut buf = Vec::new();
        lib.open_reader(&set.set_id, "data/field/flevel.lgp")
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert_eq!(buf, b"PLACEHOLDER-NOT-REAL-DATA");
        assert!(matches!(
            lib.open_reader(&set.set_id, "nope"),
            Err(LibraryError::EntryNotFound { .. })
        ));
        assert!(matches!(
            lib.get(&set.set_id, "../escape"),
            Ok(None) // an unnormalisable query simply does not match anything
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&blob).unwrap().permissions().mode();
            assert_eq!(mode & 0o222, 0, "blobs are read-only");
        }
        // blobs that were damaged after import are reported, not silently served
        fs::remove_file(&blob).unwrap();
        assert!(matches!(
            lib.get(&set.set_id, "data/field/flevel.lgp"),
            Err(LibraryError::BlobProblem { .. })
        ));
    }

    #[test]
    fn incompatible_set_is_refused() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"aaa");
        let b = write(&src, "b.bin", b"bbb");
        let ntsc = SetHandle::new(pc_prov("NTSC-U"));
        lib.import_file(&ntsc, "data/a.bin", &a, install(&src), 1)
            .unwrap();

        // a PAL import aimed at the NTSC-U set id
        let wrong = SetHandle::with_id(ntsc.set_id.clone(), pc_prov("PAL"));
        let err = lib
            .import_file(&wrong, "data/b.bin", &b, install(&src), 2)
            .unwrap_err();
        assert!(matches!(err, LibraryError::IncompatibleSet { .. }), "{err}");
        assert!(err.to_string().contains("PAL"));
        // language and the modded flag are discriminators too
        let mut lang = pc_prov("NTSC-U");
        lang.language = "de".into();
        assert!(matches!(
            lib.import_file(
                &SetHandle::with_id(ntsc.set_id.clone(), lang),
                "data/b.bin",
                &b,
                install(&src),
                2
            ),
            Err(LibraryError::IncompatibleSet { .. })
        ));
        let mut modded = pc_prov("NTSC-U");
        modded.modded = true;
        assert!(matches!(
            lib.import_file(
                &SetHandle::with_id(ntsc.set_id.clone(), modded.clone()),
                "data/b.bin",
                &b,
                install(&src),
                2
            ),
            Err(LibraryError::IncompatibleSet { .. })
        ));

        // the NTSC-U set is untouched ...
        let m = lib.load_manifest(&ntsc.set_id).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 1);
        // ... and the same PAL / modded provenance simply gets its own set when imported normally
        let pal = SetHandle::new(pc_prov("PAL"));
        let modded = SetHandle::new(modded);
        assert_ne!(pal.set_id, ntsc.set_id);
        assert_ne!(modded.set_id, ntsc.set_id);
        lib.import_file(&pal, "data/b.bin", &b, install(&src), 3)
            .unwrap();
        lib.import_file(&modded, "data/b.bin", &b, install(&src), 3)
            .unwrap();
        assert_eq!(lib.list_sets().unwrap().len(), 3);
    }

    #[test]
    fn source_changed_during_copy_aborts_cleanly() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", &[7u8; 4096]);
        let set = SetHandle::new(pc_prov("unknown"));

        // (1) the file grows while it is being copied
        {
            let mut batch = lib.begin_import(&set, 1).unwrap();
            let target = a.clone();
            batch.after_copy_hook = Some(Box::new(move || {
                let mut f = fs::OpenOptions::new().append(true).open(&target).unwrap();
                f.write_all(b"x").unwrap();
            }));
            let err = batch.add_file("data/a.bin", &a, install(&src)).unwrap_err();
            assert!(matches!(err, LibraryError::SourceChanged(_)), "{err}");
            assert!(batch.manifest().entries.is_empty());
        }
        assert_eq!(
            count_files(&lib.library_dir().join("blobs")),
            0,
            "no blob for an aborted file"
        );
        assert_eq!(
            count_files(&lib.library_dir().join("tmp")),
            0,
            "temp blob removed"
        );
        assert!(lib.list_sets().unwrap().is_empty(), "nothing recorded");

        // (2) same size, but the mtime moves
        fs::write(&a, [7u8; 4096]).unwrap();
        {
            let mut batch = lib.begin_import(&set, 1).unwrap();
            let target = a.clone();
            batch.after_copy_hook = Some(Box::new(move || {
                let f = fs::OpenOptions::new().write(true).open(&target).unwrap();
                f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
                    .unwrap();
            }));
            let err = batch.add_file("data/a.bin", &a, install(&src)).unwrap_err();
            assert!(matches!(err, LibraryError::SourceChanged(_)), "{err}");
        }
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 0);
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);

        // (3) the path is replaced by a different file with the same size and mtime (inode differs)
        #[cfg(unix)]
        {
            fs::write(&a, [7u8; 4096]).unwrap();
            let stamp_time = fs::metadata(&a).unwrap().modified().unwrap();
            let mut batch = lib.begin_import(&set, 1).unwrap();
            let target = a.clone();
            let other = src.join("replacement.bin");
            batch.after_copy_hook = Some(Box::new(move || {
                fs::write(&other, [8u8; 4096]).unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&other)
                    .unwrap()
                    .set_modified(stamp_time)
                    .unwrap();
                fs::rename(&other, &target).unwrap();
            }));
            let err = batch.add_file("data/a.bin", &a, install(&src)).unwrap_err();
            assert!(matches!(err, LibraryError::SourceChanged(_)), "{err}");
        }

        // an unchanged source still imports fine afterwards
        fs::write(&a, [7u8; 4096]).unwrap();
        lib.import_file(&set, "data/a.bin", &a, install(&src), 5)
            .unwrap();
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 1);
    }

    #[test]
    fn sources_inside_the_library_are_refused() {
        let (_tmp, lib) = setup();
        let inside = write(&lib.library_dir().join("sets"), "sneaky.bin", b"x");
        let set = SetHandle::new(pc_prov("unknown"));
        let err = lib
            .import_file(&set, "data/x.bin", &inside, install(Path::new("/x")), 1)
            .unwrap_err();
        assert!(matches!(err, LibraryError::SourceInsideLibrary(_)), "{err}");
        #[cfg(unix)]
        {
            // a symlink pointing into the library is resolved and refused as well
            let link = lib.state_dir().join("link.bin");
            std::os::unix::fs::symlink(&inside, &link).unwrap();
            let err = lib
                .import_file(&set, "data/x.bin", &link, install(Path::new("/x")), 1)
                .unwrap_err();
            assert!(matches!(err, LibraryError::SourceInsideLibrary(_)), "{err}");
        }
        assert!(lib.list_sets().unwrap().is_empty());
    }

    #[test]
    fn rejects_non_files_bad_paths_and_oversize() {
        let (tmp, lib) = setup();
        let lib = lib.with_max_file_size(16);
        let src = tmp.path().join("src");
        let ok = write(&src, "ok.bin", b"0123456789abcdef"); // exactly 16
        let big = write(&src, "big.bin", b"0123456789abcdefX"); // 17
        let set = SetHandle::new(pc_prov("unknown"));
        let from = install(&src);

        lib.import_file(&set, "data/ok.bin", &ok, from.clone(), 1)
            .unwrap();
        assert!(matches!(
            lib.import_file(&set, "data/big.bin", &big, from.clone(), 1),
            Err(LibraryError::TooLarge { .. })
        ));
        assert!(matches!(
            lib.import_file(&set, "data/dir", &src, from.clone(), 1),
            Err(LibraryError::NotRegularFile(_))
        ));
        assert!(matches!(
            lib.import_file(&set, "../evil", &ok, from.clone(), 1),
            Err(LibraryError::BadLogicalPath { .. })
        ));
        assert!(matches!(
            lib.import_file(&set, "/abs", &ok, from.clone(), 1),
            Err(LibraryError::BadLogicalPath { .. })
        ));
        assert!(matches!(
            lib.import_file(
                &set,
                "data/missing.bin",
                &src.join("missing.bin"),
                from.clone(),
                1
            ),
            Err(LibraryError::Io { .. })
        ));
        // a stream is capped even when its size is not declared up front
        let mut batch = lib.begin_import(&set, 2).unwrap();
        let mut endless = io::repeat(1u8);
        assert!(matches!(
            batch.add_reader("data/stream.bin", &mut endless, None, from),
            Err(LibraryError::TooLarge { .. })
        ));
        drop(batch);
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 1);
    }

    #[test]
    fn conflicting_content_for_a_path_is_an_error() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let v1 = write(&src, "v1.bin", b"version one");
        let v2 = write(&src, "v2.bin", b"version two");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/x.bin", &v1, install(&src), 1)
            .unwrap();
        let err = lib
            .import_file(&set, "data/x.bin", &v2, install(&src), 2)
            .unwrap_err();
        assert!(matches!(err, LibraryError::EntryConflict { .. }), "{err}");
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(
            m.entries[0].blake3,
            blake3::hash(b"version one").to_hex().to_string()
        );
        assert_eq!(
            count_files(&lib.library_dir().join("blobs")),
            1,
            "the rejected content was not stored"
        );
    }

    #[test]
    fn add_reader_checks_declared_size_and_batches_persist_on_checkpoint() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        let disc = SourceRef::Disc {
            index: 1,
            serial: "TEST-00000".into(),
        };
        {
            let mut batch = lib.begin_import(&set, 9).unwrap();
            let err = batch
                .add_reader(
                    "disc1/A.BIN",
                    &mut Cursor::new(vec![1, 2, 3]),
                    Some(5),
                    disc.clone(),
                )
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    LibraryError::SizeMismatch {
                        expected: 5,
                        actual: 3,
                        ..
                    }
                ),
                "{err}"
            );
            assert_eq!(count_files(&lib.library_dir().join("blobs")), 0);
            batch
                .add_reader(
                    "disc1/A.BIN",
                    &mut Cursor::new(vec![1, 2, 3]),
                    Some(3),
                    disc.clone(),
                )
                .unwrap();
            batch.checkpoint().unwrap();
            batch
                .add_reader(
                    "disc1/B.BIN",
                    &mut Cursor::new(vec![4, 5]),
                    None,
                    disc.clone(),
                )
                .unwrap();
            // B is not checkpointed: dropping the batch discards it, A survives
        }
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].logical_path, "disc1/a.bin"); // PC sets are lowercased
        assert_eq!(m.provenance.sources, vec![disc.clone()]);
        assert_eq!(m.imported_at, 9);
    }

    #[test]
    fn second_writer_gets_set_busy() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        let first = lib.begin_import(&set, 1).unwrap();
        let err = lib
            .begin_import(&set, 1)
            .err()
            .expect("second writer must be refused");
        assert!(matches!(err, LibraryError::SetBusy(_)), "{err}");
        drop(first);
        lib.begin_import(&set, 1).unwrap();
    }

    #[test]
    fn empty_files_import_and_dedup() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.lgp", b"");
        let b = write(&src, "b.lgp", b"");
        let set = SetHandle::new(pc_prov("unknown"));
        let o1 = lib
            .import_file(&set, "data/a.lgp", &a, install(&src), 1)
            .unwrap();
        let o2 = lib
            .import_file(&set, "data/b.lgp", &b, install(&src), 1)
            .unwrap();
        assert_eq!(o1.size, 0);
        assert!(o1.blob_created && !o2.blob_created);
        let mut buf = Vec::new();
        lib.open_reader(&set.set_id, "data/a.lgp")
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn damaged_manifests_are_errors_not_panics() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"payload");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(&src), 1)
            .unwrap();
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let good = fs::read_to_string(&path).unwrap();
        let hash = blake3::hash(b"payload").to_hex().to_string();

        let cases: Vec<(&str, String)> = vec![
            ("garbage", "not json at all {{{".to_string()),
            ("empty", String::new()),
            (
                "traversal digest",
                good.replace(&hash, "../../../../etc/passwd"),
            ),
            ("short digest", good.replace(&hash, "abcd")),
            (
                "uppercase digest",
                good.replace(&hash, &hash.to_uppercase()),
            ),
            (
                "wrong version",
                good.replace("\"format_version\": 1", "\"format_version\": 99"),
            ),
            (
                "wrong set id",
                good.replace(&set.set_id, "other-set-00000000"),
            ),
            (
                "unnormalised path",
                good.replace("data/a.bin", "Data/../a.bin"),
            ),
            ("duplicate path", {
                // duplicate the single entry
                let m = lib.load_manifest(&set.set_id).unwrap();
                let mut dup = m.clone();
                dup.entries.push(m.entries[0].clone());
                serde_json::to_string(&dup).unwrap()
            }),
        ];
        for (name, text) in cases {
            fs::write(&path, &text).unwrap();
            let r = lib.load_manifest(&set.set_id);
            assert!(
                matches!(r, Err(LibraryError::BadManifest { .. })),
                "{name}: {r:?}"
            );
            assert!(lib.get(&set.set_id, "data/a.bin").is_err(), "{name}");
            // a damaged manifest also stops new imports into that set instead of overwriting it
            assert!(
                lib.import_file(&set, "data/b.bin", &a, install(&src), 2)
                    .is_err(),
                "{name}"
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                text,
                "{name}: manifest must not be rewritten"
            );
        }
        fs::write(&path, &good).unwrap();
        lib.load_manifest(&set.set_id).unwrap();
    }

    #[test]
    fn failed_manifest_write_keeps_the_old_manifest() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"one");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(&src), 1)
            .unwrap();
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let before = fs::read(&path).unwrap();
        // a writer that dies half way: the temp file goes away and the old content stays
        let r = crate::fsutil::atomic_write_with(&path, |f| {
            f.write_all(b"{ \"half\":")?;
            Err(io::Error::other("simulated crash"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        lib.load_manifest(&set.set_id).unwrap();
    }

    fn backdate(path: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    /// Overwrite a (read-only) blob in place, like bit rot or a stray write would.
    #[cfg(unix)]
    fn tamper(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn stale_part_files_are_swept_but_live_fresh_and_foreign_ones_are_kept() {
        let (_tmp, lib) = setup();
        let tmp_dir = lib.library_dir().join("tmp");
        let mk = |name: &str| {
            let p = tmp_dir.join(name);
            fs::write(&p, vec![1u8; 100]).unwrap();
            p
        };
        let dead = mk("blob-1-0-00000001.part");
        let finished = mk("blob-2-0-00000002.part"); // fully copied, made read-only, never renamed
        let live = mk("blob-3-0-00000003.part");
        let fresh = mk("blob-4-0-00000004.part");
        let foreign = mk("notes.txt");
        let named_dir = tmp_dir.join("blob-5-0-00000005.part");
        fs::create_dir(&named_dir).unwrap();
        for p in [&dead, &finished, &live, &foreign] {
            backdate(p, 7200);
        }
        let mut perms = fs::metadata(&finished).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&finished, perms).unwrap();
        // a live writer holds the advisory lock on its part file
        let holder = File::open(&live).unwrap();
        holder.try_lock().unwrap();

        // open_existing never deletes anything
        let ro = Library::open_existing(lib.state_dir()).unwrap();
        assert_eq!(ro.stale_parts_removed(), 0);
        assert!(dead.exists());

        // a fresh open (a new process, in real life) reclaims what no writer holds
        let again = Library::open(lib.state_dir()).unwrap();
        assert_eq!(again.stale_parts_removed(), 2);
        assert!(!dead.exists() && !finished.exists());
        assert!(live.exists(), "a locked part file belongs to a live writer");
        assert!(fresh.exists(), "a young part file is never swept");
        assert!(foreign.exists(), "only blob-*.part files are touched");
        assert!(named_dir.is_dir(), "only regular files are touched");

        // once the writer is gone its leftover is reclaimed as well
        drop(holder);
        let third = Library::open(lib.state_dir()).unwrap();
        assert_eq!(third.stale_parts_removed(), 1);
        assert!(!live.exists());
        assert!(fresh.exists() && foreign.exists());
    }

    #[test]
    fn a_copy_in_flight_is_never_swept_even_when_it_looks_old() {
        let (tmp, lib) = setup();
        let src = write(&tmp.path().join("src"), "a.bin", &[7u8; 4096]);
        let set = SetHandle::new(pc_prov("unknown"));
        let tmp_dir = lib.library_dir().join("tmp");
        let seen = std::rc::Rc::new(std::cell::Cell::new((usize::MAX, u64::MAX)));
        let mut batch = lib.begin_import(&set, 1).unwrap();
        let (probe, dir) = (seen.clone(), tmp_dir.clone());
        // runs after the copy finished and before the blob is stored: the .part file exists and
        // its writer (this very process) still holds the lock
        batch.after_copy_hook = Some(Box::new(move || {
            let far_future = SystemTime::now() + Duration::from_secs(86_400);
            let removed = sweep_stale_parts(&dir, far_future);
            probe.set((count_files(&dir), removed));
        }));
        batch.add_file("data/a.bin", &src, install(&src)).unwrap();
        assert_eq!(seen.get(), (1, 0), "the in-flight .part must survive a sweep");
        batch.commit().unwrap();
        assert_eq!(count_files(&tmp_dir), 0, "and is consumed by the import");
    }

    #[cfg(unix)]
    #[test]
    fn damaged_blobs_are_detected_and_healed_by_reimport() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"hello world");
        let set = SetHandle::new(pc_prov("unknown"));
        let first = lib
            .import_file(&set, "data/a.bin", &a, install(&src), 1)
            .unwrap();
        assert!(first.blob_created && !first.blob_repaired);
        let blob = lib.blob_path(&first.blake3).unwrap();
        lib.verify_entry(&set.set_id, "data/a.bin").unwrap();
        assert!(lib.verify_set(&set.set_id).unwrap().is_empty());

        // same size, different bytes: the cheap size check in get() cannot see it ...
        tamper(&blob, b"HELLO WORLD");
        assert!(lib.get(&set.set_id, "data/a.bin").unwrap().is_some());
        // ... the content check does
        let err = lib.verify_entry(&set.set_id, "data/a.bin").unwrap_err();
        assert!(
            matches!(err, LibraryError::BlobProblem { .. }) && err.to_string().contains("content hash"),
            "{err}"
        );
        assert_eq!(lib.verify_set(&set.set_id).unwrap().len(), 1);

        // re-importing the still-good original replaces the damaged blob
        let healed = lib
            .import_file(&set, "data/a.bin", &a, install(&src), 2)
            .unwrap();
        assert!(healed.blob_created && healed.blob_repaired);
        assert_eq!(healed.entry, EntryStatus::Unchanged);
        let path = lib.get(&set.set_id, "data/a.bin").unwrap().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello world");
        lib.verify_entry(&set.set_id, "data/a.bin").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o222, 0, "the replacement is read-only again");
        }

        // a blob of the wrong size is replaced the same way
        tamper(&blob, b"short");
        let again = lib
            .import_file(&set, "data/a.bin", &a, install(&src), 3)
            .unwrap();
        assert!(again.blob_repaired);
        lib.verify_entry(&set.set_id, "data/a.bin").unwrap();

        // a healthy blob is deduplicated, not rewritten
        let calm = lib
            .import_file(&set, "data/a.bin", &a, install(&src), 4)
            .unwrap();
        assert!(!calm.blob_created && !calm.blob_repaired);
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 1);
    }

    #[cfg(unix)]
    #[test]
    fn verify_set_hashes_a_shared_blob_once_but_reports_every_path() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"shared bytes");
        let b = write(&src, "b.bin", b"shared bytes");
        let c = write(&src, "c.bin", b"other bytes!");
        let set = SetHandle::new(pc_prov("unknown"));
        let mut blake = String::new();
        for (name, path) in [("data/a.bin", &a), ("data/b.bin", &b), ("data/c.bin", &c)] {
            let o = lib
                .import_file(&set, name, path, install(&src), 1)
                .unwrap();
            if name == "data/a.bin" {
                blake = o.blake3;
            }
        }
        assert_eq!(count_files(&lib.library_dir().join("blobs")), 2);
        tamper(&lib.blob_path(&blake).unwrap(), b"SHARED BYTES");
        let problems = lib.verify_set(&set.set_id).unwrap();
        let mut paths: Vec<String> = problems
            .iter()
            .map(|p| match p {
                LibraryError::BlobProblem { logical_path, .. } => logical_path.clone(),
                other => panic!("unexpected {other}"),
            })
            .collect();
        paths.sort();
        assert_eq!(paths, ["data/a.bin", "data/b.bin"]);
    }

    #[test]
    fn the_writer_enforces_the_entry_cap() {
        let (_tmp, lib) = setup();
        let lib = lib.with_max_entries(2);
        let set = SetHandle::new(pc_prov("unknown"));
        let mut batch = lib.begin_import(&set, 1).unwrap();
        let add = |batch: &mut ImportBatch<'_>, name: &str, bytes: &[u8]| {
            batch.add_reader(name, &mut Cursor::new(bytes.to_vec()), None, SourceRef::Fixture)
        };
        add(&mut batch, "d/one", b"1").unwrap();
        add(&mut batch, "d/two", b"2").unwrap();
        let err = add(&mut batch, "d/three", b"333").unwrap_err();
        assert!(matches!(err, LibraryError::TooManyEntries { limit: 2, .. }), "{err}");
        assert_eq!(
            count_files(&lib.library_dir().join("blobs")),
            2,
            "no blob is stored for an entry that cannot be recorded"
        );
        // re-adding an entry that is already there is not "one more"
        add(&mut batch, "d/one", b"1").unwrap();
        batch.commit().unwrap();
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().entries.len(), 2);
    }

    #[test]
    fn batch_lookup_sees_entries_that_are_not_checkpointed_yet() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        let mut batch = lib.begin_import(&set, 1).unwrap();
        for n in ["b", "c", "a"] {
            batch
                .add_reader(
                    &format!("data/{n}"),
                    &mut Cursor::new(n.as_bytes().to_vec()),
                    None,
                    SourceRef::Fixture,
                )
                .unwrap();
        }
        // insertion order, not sorted: Manifest::find (binary search) cannot be trusted here
        assert_eq!(batch.find_entry("data/a").unwrap().size, 1);
        assert_eq!(batch.find_entry("DATA\\B").unwrap().logical_path, "data/b");
        assert!(batch.find_entry("data/zzz").is_none());
        assert!(batch.find_entry("../nope").is_none());
        batch.checkpoint().unwrap();
        assert!(batch.manifest().find("data/a").is_some());
        assert!(batch.find_entry("data/a").is_some());
    }

    #[test]
    fn identification_is_stored_merged_and_never_rewrites_an_unchanged_manifest() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"payload");
        let b = write(&src, "b.bin", b"other payload");
        let ident = Identification::unverified("pc-file-presence", vec!["edition is a guess".into()]);
        let set = SetHandle::new(pc_prov("unknown")).with_identification(ident.clone());
        lib.import_file(&set, "data/a.bin", &a, install(&src), 100)
            .unwrap();
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.identification, Some(ident.clone()));
        assert!(!m.is_verified());
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"verified\": false") && text.contains("edition is a guess"));
        assert!(m.summary().compatible_with(&pc_prov("unknown").summary()));

        // the same import again: not one byte of the manifest changes (imported_at stays 100)
        let before = fs::read(&path).unwrap();
        lib.import_file(&set, "data/a.bin", &a, install(&src), 200)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);

        // a new caveat is merged in (and written, even though no entry changed)
        let more = Identification::unverified("pc-file-presence", vec!["region unknown".into()]);
        let set2 = SetHandle::new(pc_prov("unknown")).with_identification(more.clone());
        lib.import_file(&set2, "data/a.bin", &a, install(&src), 300)
            .unwrap();
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(
            m.identification.as_ref().unwrap().caveats,
            ["edition is a guess", "region unknown"]
        );
        assert_eq!(m.imported_at, 300);
        let settled = fs::read(&path).unwrap();
        lib.import_file(&set2, "data/b.bin", &b, install(&src), 300)
            .unwrap();
        lib.import_file(&set2, "data/b.bin", &b, install(&src), 400)
            .unwrap();
        assert_ne!(fs::read(&path).unwrap(), settled); // b.bin was added ...
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().imported_at, 300); // ... but not again

        // verification can only get weaker by merging, never stronger
        let checked = Identification {
            verified: true,
            method: "hash-match".into(),
            caveats: vec![],
        };
        let set3 = SetHandle::new(pc_prov("unknown")).with_identification(checked);
        lib.import_file(&set3, "data/a.bin", &a, install(&src), 500)
            .unwrap();
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert!(!m.is_verified());
        assert_eq!(m.identification.unwrap().method, "pc-file-presence");

        // a set written without an identification has none (and the key is absent)
        let plain = SetHandle::new(pc_prov("PAL"));
        lib.import_file(&plain, "data/a.bin", &a, install(&src), 1)
            .unwrap();
        let plain_text =
            fs::read_to_string(lib.set_dir(&plain.set_id).unwrap().join(MANIFEST_FILE)).unwrap();
        assert!(!plain_text.contains("identification"));
        let m = lib.load_manifest(&plain.set_id).unwrap();
        assert!(m.identification.is_none() && !m.is_verified());
    }

    #[test]
    fn oversized_identification_records_are_rejected_and_clamped() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"payload");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(&src), 1)
            .unwrap();
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let good = fs::read_to_string(&path).unwrap();
        let caveats: Vec<String> = (0..=crate::manifest::MAX_IDENTIFICATION_CAVEATS)
            .map(|i| format!("c{i}"))
            .collect();
        let too_many = good.replacen(
            "\"importer_version\"",
            &format!(
                "\"identification\": {{\"verified\": false, \"method\": \"x\", \"caveats\": {}}}, \"importer_version\"",
                serde_json::to_string(&caveats).unwrap()
            ),
            1,
        );
        fs::write(&path, too_many).unwrap();
        assert!(matches!(
            lib.load_manifest(&set.set_id),
            Err(LibraryError::BadManifest { .. })
        ));
        // the constructor clamps instead of failing
        let big = Identification::unverified(&"m".repeat(500), vec!["x".repeat(5000); 40]);
        assert!(big.method.chars().count() <= crate::manifest::MAX_IDENTIFICATION_METHOD);
        assert_eq!(big.caveats.len(), crate::manifest::MAX_IDENTIFICATION_CAVEATS);
        assert!(big
            .caveats
            .iter()
            .all(|c| c.chars().count() <= crate::manifest::MAX_IDENTIFICATION_TEXT));
        fs::write(&path, good).unwrap();
        lib.load_manifest(&set.set_id).unwrap();
    }

    #[test]
    fn the_library_directory_ignores_itself_in_git() {
        let (_tmp, lib) = setup();
        let gi = lib.library_dir().join(".gitignore");
        let text = fs::read_to_string(&gi).unwrap();
        assert_eq!(text.lines().last(), Some("*"), "{text}");
        // a file the user edited is left alone
        fs::write(&gi, "custom\n").unwrap();
        Library::open(lib.state_dir()).unwrap();
        assert_eq!(fs::read_to_string(&gi).unwrap(), "custom\n");
        // and the marker file is not mistaken for a set or a blob
        assert!(lib.list_sets().unwrap().is_empty());
    }

    #[test]
    fn open_existing_does_not_create_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("never-created");
        assert!(matches!(
            Library::open_existing(&state),
            Err(LibraryError::NoLibrary(_))
        ));
        assert!(!state.exists());
        Library::open(&state).unwrap();
        Library::open_existing(&state).unwrap();
    }
}
