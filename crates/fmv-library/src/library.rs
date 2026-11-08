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
use std::time::SystemTime;

use fmv_core::{EditionProvenance, SourceRef};

use crate::error::{io_err, LibraryError};
use crate::fsutil::{
    atomic_write, create_private_dir_durable, lock_names_path, open_lock_file, open_new_private,
    same_file, sweep_stale_temps, sync_dir, try_lock_exclusive, unique_suffix, LockOutcome,
    STALE_AFTER,
};
use crate::manifest::{
    entry_json_cost, fixed_json_size, read_manifest, write_manifest, DiscContent, Identification,
    Manifest, ManifestEntry, MANIFEST_FILE, MAX_DISC_RECORDS, MAX_MANIFEST_BYTES,
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
/// A `blob-*.part` file younger than [`STALE_AFTER`] is never swept: it may belong to a copy that
/// is only just starting (between creating the file and locking it, a window of microseconds;
/// the lock, not this age, is what protects a running copy). Kept short so that a re-run right
/// after a killed import already reclaims its leftovers.
const STALE_PART_AFTER: std::time::Duration = STALE_AFTER;
/// How often `begin_import` retries when the lock file it locked turned out to have been removed
/// (by [`Library::remove_set`]) in the meantime.
const LOCK_ATTEMPTS: usize = 3;
/// Room the early manifest-size gate leaves for what is not an entry (sources, identification,
/// disc records, longer numbers): the exact check at write time has the last word.
const MANIFEST_SLACK: u64 = 64 * 1024;
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

/// What [`Library::remove_set`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedSet {
    pub set_id: String,
    /// Entries the set's manifest listed; `None` when it could not be read (see
    /// `manifest_problem`).
    pub entries: Option<usize>,
    /// Bytes those entries add up to (the blobs themselves stay in the store).
    pub bytes: Option<u64>,
    /// Why the manifest could not be read: damaged, too large, missing.
    pub manifest_problem: Option<String>,
    /// False when this filesystem has no advisory locks: the removal went ahead without the
    /// writer lock and could not have noticed a concurrent import.
    pub locked: bool,
}

/// What [`ImportBatch::drop_disc`] took out of the in-memory manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedDisc {
    /// Entries removed.
    pub entries: usize,
    pub bytes: u64,
    /// The serial the removed disc was recorded with (its content record, else its source).
    pub serial: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Library {
    state_dir: PathBuf,
    root: PathBuf,
    root_canonical: PathBuf,
    max_file_size: u64,
    importer_version: String,
    max_entries: usize,
    /// The manifest size the writer enforces: the reader's cap, [`MAX_MANIFEST_BYTES`] (only a
    /// test can lower it).
    max_manifest_bytes: u64,
    stale_parts_removed: u64,
}

impl Library {
    /// Open (creating if needed) the library under `state_dir`. Also reclaims what an import that
    /// was killed left behind: in-flight blob copies and half-written manifest temp files (see
    /// [`Library::stale_parts_removed`]).
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let state_dir = state_dir.as_ref().to_path_buf();
        let root = state_dir.join("library");
        for sub in ["blobs", "sets", "tmp"] {
            let p = root.join(sub);
            create_private_dir_durable(&p).map_err(io_err(&p))?;
        }
        // Best effort: keep copies of the owner's game data out of git even when the state
        // directory sits inside a work tree. Failing to write this file must not fail the open.
        let gitignore = root.join(GITIGNORE_FILE);
        if !gitignore.exists() {
            let _ = atomic_write(&gitignore, GITIGNORE_TEXT);
        }
        let mut lib = Self::from_root(state_dir, root)?;
        let now = SystemTime::now();
        lib.stale_parts_removed = sweep_stale_parts(&lib.root.join("tmp"), now)
            + sweep_stale_manifest_temps(&lib.root.join("sets"), now)
            + sweep_stale_temps(&lib.root, GITIGNORE_FILE, now);
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
            max_manifest_bytes: MAX_MANIFEST_BYTES,
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

    /// How many leftovers of killed imports [`Library::open`] found and deleted: in-flight blob
    /// copies (`tmp/blob-*.part`) and half-written manifest temp files (`.manifest.json.tmp-*`
    /// next to a manifest). Always 0 for [`Library::open_existing`], which never writes.
    pub fn stale_parts_removed(&self) -> u64 {
        self.stale_parts_removed
    }

    /// Test seam: a small entry cap, so the cap can be exercised without half a million files.
    #[cfg(test)]
    pub(crate) fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    /// Test seam: a small manifest byte cap, so the cap can be exercised without 64 MiB.
    #[cfg(test)]
    pub(crate) fn with_max_manifest_bytes(mut self, n: u64) -> Self {
        self.max_manifest_bytes = n;
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

    /// [`Library::ensure_not_inside_library`] for the library under `state_dir`, decided WITHOUT
    /// opening or creating anything: a library that does not exist yet cannot hold the source.
    /// Importers call it before they lock or create a single file, so that this refusal (like
    /// every refusal that can be decided up front) leaves no trace.
    pub fn ensure_source_outside_library(
        state_dir: &Path,
        path: &Path,
    ) -> Result<(), LibraryError> {
        let Ok(root) = fs::canonicalize(state_dir.join("library")) else {
            return Ok(());
        };
        let canonical = fs::canonicalize(path).map_err(io_err(path))?;
        if canonical.starts_with(&root) {
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
    ///
    /// Where the filesystem has no advisory locks (some network and FUSE mounts), the import goes
    /// ahead UNLOCKED instead of failing: [`ImportBatch::is_locked`] is then false and the caller
    /// is expected to say so. Nothing then stops a second import into the same set at the same
    /// time; blobs are content-addressed and the manifest is replaced atomically, so the worst
    /// outcome of such a collision is entries that have to be imported again.
    pub fn begin_import(
        &self,
        set: &SetHandle,
        imported_at: u64,
    ) -> Result<ImportBatch<'_>, LibraryError> {
        let set_dir = self.set_dir(&set.set_id)?;
        let (lock, locked) = lock_set(&set_dir, &set.set_id)?;
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
        // what the entries it already holds cost in the manifest file: the early size gate adds
        // to this as entries are added
        let entries_cost = manifest
            .entries
            .iter()
            .fold(0u64, |sum, e| sum.saturating_add(entry_json_cost(e)));
        let index = index_of(&manifest);
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
            locked,
            frozen: Vec::new(),
            entries_cost,
            _lock: lock,
            #[cfg(test)]
            after_copy_hook: None,
        })
    }

    /// Remove the set `set_id` from the library: its directory (manifest, lock, leftovers),
    /// whether or not the manifest can still be read. Works on a broken set (a damaged or
    /// oversized manifest, no manifest at all) because that is when it is needed.
    ///
    /// What it does NOT touch: the blob store (blobs are shared between sets and are reused when
    /// the same content is imported again, so they stay), other sets, and every original the set
    /// was imported from (nothing outside `library/sets/<set_id>/` is opened for writing).
    /// Refuses while an import holds the set ([`LibraryError::SetBusy`]) and refuses a set
    /// directory that is a symbolic link or not a directory.
    pub fn remove_set(&self, set_id: &str) -> Result<RemovedSet, LibraryError> {
        let set_dir = self.set_dir(set_id)?;
        let meta = match fs::symlink_metadata(&set_dir) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(LibraryError::SetNotFound(set_id.to_string()))
            }
            Err(e) => return Err(io_err(&set_dir)(e)),
        };
        if !meta.is_dir() {
            return Err(LibraryError::NotASetDirectory(set_dir));
        }
        // the writer lock: nothing may be importing into the set while it goes
        let lock_path = set_dir.join(LOCK_FILE);
        let lock = open_lock_file(&lock_path).map_err(io_err(&lock_path))?;
        let locked = match try_lock_exclusive(&lock).map_err(io_err(&lock_path))? {
            LockOutcome::Held => true,
            LockOutcome::Busy => return Err(LibraryError::SetBusy(set_id.to_string())),
            LockOutcome::Unsupported => false,
        };
        let (entries, bytes, manifest_problem) =
            match read_manifest(&set_dir.join(MANIFEST_FILE), set_id) {
                Ok(m) => (Some(m.entries.len()), Some(m.total_bytes()), None),
                Err(LibraryError::SetNotFound(_)) => {
                    (None, None, Some("the set has no manifest".to_string()))
                }
                Err(e) => (None, None, Some(e.to_string())),
            };
        // An open file cannot be deleted on some platforms; on unix the lock is held until the
        // directory is gone. A writer that opened the (now unlinked) lock file in the meantime
        // notices that the lock no longer names its path (see `lock_set`) and starts over.
        #[cfg(not(unix))]
        drop(lock);
        fs::remove_dir_all(&set_dir).map_err(io_err(&set_dir))?;
        #[cfg(unix)]
        drop(lock);
        sync_dir(&self.root.join("sets"));
        Ok(RemovedSet {
            set_id: set_id.to_string(),
            entries,
            bytes,
            manifest_problem,
            locked,
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
        create_private_dir_durable(&tmp_dir).map_err(io_err(&tmp_dir))?;
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
            // a new shard directory is fsynced in `blobs/` as well, not only the blob in it
            create_private_dir_durable(shard).map_err(io_err(shard))?;
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

/// Create the set's directory if needed and take its exclusive writer lock: the file and whether
/// the lock is really held (`false`: this filesystem has no advisory locks, see
/// [`Library::begin_import`]).
///
/// [`Library::remove_set`] deletes lock files, so a lock that was won is only believed when the
/// lock path still names the file that was locked; if it does not, the file is a ghost and the
/// whole step starts over (a directory removed meanwhile is simply created again).
fn lock_set(set_dir: &Path, set_id: &str) -> Result<(File, bool), LibraryError> {
    let lock_path = set_dir.join(LOCK_FILE);
    for _ in 0..LOCK_ATTEMPTS {
        create_private_dir_durable(set_dir).map_err(io_err(set_dir))?;
        let file = match open_lock_file(&lock_path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(&lock_path)(e)),
        };
        match try_lock_exclusive(&file).map_err(io_err(&lock_path))? {
            LockOutcome::Held if lock_names_path(&file, &lock_path) => return Ok((file, true)),
            LockOutcome::Held => {} // a ghost: locked a file that no longer has this name
            LockOutcome::Busy => return Err(LibraryError::SetBusy(set_id.to_string())),
            LockOutcome::Unsupported => return Ok((file, false)),
        }
    }
    Err(LibraryError::SetBusy(set_id.to_string()))
}

fn index_of(manifest: &Manifest) -> HashMap<String, usize> {
    manifest
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.logical_path.clone(), i))
        .collect()
}

/// Remove `.manifest.json.tmp-*` files that killed writers left in the set directories. A set is
/// only swept while nobody writes it, which is exactly when its lock can be taken; a set whose
/// lock cannot be taken (busy, or no advisory locks here) is left alone, and so is a directory
/// without a lock file (this code never wrote there).
fn sweep_stale_manifest_temps(sets_dir: &Path, now: SystemTime) -> u64 {
    let Ok(entries) = fs::read_dir(sets_dir) else {
        return 0;
    };
    let mut removed = 0u64;
    for ent in entries.flatten() {
        let Some(name) = ent.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let dir = ent.path();
        if validate_set_id(&name).is_err() || !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        let Ok(lock) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(LOCK_FILE))
        else {
            continue;
        };
        if matches!(try_lock_exclusive(&lock), Ok(LockOutcome::Held)) {
            removed += sweep_stale_temps(&dir, MANIFEST_FILE, now);
        }
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
    /// False when the filesystem has no advisory locks: nothing is held (see
    /// [`Library::begin_import`]).
    locked: bool,
    /// Logical-path prefixes under which no NEW entry may be added (see [`Self::freeze_prefix`]).
    frozen: Vec<String>,
    /// What the entries of `manifest` add to the manifest file (see `entry_json_cost`): the
    /// early size gate adds to this as entries are added.
    entries_cost: u64,
    _lock: File,
    /// Test seam: runs after a file was copied and before the source is re-verified.
    #[cfg(test)]
    pub(crate) after_copy_hook: Option<Box<dyn FnMut() + 'a>>,
}

impl ImportBatch<'_> {
    pub fn set_id(&self) -> &str {
        &self.set_id
    }

    /// True when this batch holds the set's advisory lock. False on a filesystem without
    /// advisory locks (some network and FUSE mounts): the import then goes ahead unlocked, and
    /// the caller should tell the user not to run two imports into one set at once.
    pub fn is_locked(&self) -> bool {
        self.locked
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

    /// From now on this batch refuses every NEW entry whose logical path starts with `prefix`
    /// ([`LibraryError::PrefixFrozen`], before anything is stored): entries that already exist
    /// may still be added again with identical content. This is how an importer re-importing a
    /// disc number it already holds makes sure it can only ever confirm that disc, never grow it
    /// into a mixture of two. Files the importer skips by itself (streams, unstorable names)
    /// never get this far, so skipping them stays free.
    pub fn freeze_prefix(&mut self, prefix: impl Into<String>) {
        self.frozen.push(prefix.into());
    }

    /// The content record stored for disc `index` of this set, if any.
    pub fn disc_content(&self, index: u8) -> Option<&DiscContent> {
        self.manifest.disc(index)
    }

    /// Store (or replace) the content record of one disc; written with the next checkpoint.
    /// Recording what is already recorded changes nothing, so a re-import stays a no-op.
    pub fn record_disc(&mut self, record: DiscContent) -> Result<(), LibraryError> {
        let bad = |reason: String| LibraryError::BadManifest {
            set_id: self.set_id.clone(),
            reason,
        };
        record.check().map_err(bad)?;
        if self.manifest.disc(record.index) == Some(&record) {
            return Ok(());
        }
        let others = self
            .manifest
            .discs
            .iter()
            .filter(|d| d.index != record.index)
            .count();
        if others >= MAX_DISC_RECORDS {
            return Err(LibraryError::BadManifest {
                set_id: self.set_id.clone(),
                reason: "too many disc records".to_string(),
            });
        }
        self.manifest.discs.retain(|d| d.index != record.index);
        self.manifest.discs.push(record);
        self.manifest.discs.sort_by_key(|d| d.index);
        self.dirty = true;
        Ok(())
    }

    /// Take disc `index` out of the in-memory manifest: its entries (every logical path that
    /// starts with `entry_prefix`), its content record and its disc sources, except
    /// `keep_source` (the source the replacement disc is about to be recorded with, so that an
    /// unchanged source keeps its place). Nothing is written until the next checkpoint, and no
    /// blob is deleted (they are shared and content-addressed). The importer uses this to
    /// replace a disc that was imported wrongly.
    pub fn drop_disc(
        &mut self,
        index: u8,
        entry_prefix: &str,
        keep_source: Option<&SourceRef>,
    ) -> DroppedDisc {
        let mut entries = 0usize;
        let mut bytes = 0u64;
        let mut removed_cost = 0u64;
        self.manifest.entries.retain(|e| {
            if e.logical_path.starts_with(entry_prefix) {
                entries += 1;
                bytes = bytes.saturating_add(e.size);
                removed_cost = removed_cost.saturating_add(entry_json_cost(e));
                false
            } else {
                true
            }
        });
        let mut serial = self.manifest.disc(index).map(|d| d.serial.clone());
        let sources_before = self.manifest.provenance.sources.len();
        self.manifest.provenance.sources.retain(|s| match s {
            SourceRef::Disc { index: i, serial: sr } if *i == index => {
                let keep = keep_source == Some(s);
                if !keep && serial.is_none() {
                    serial = Some(sr.clone());
                }
                keep
            }
            _ => true,
        });
        let sources_removed = self.manifest.provenance.sources.len() != sources_before;
        let records_before = self.manifest.discs.len();
        self.manifest.discs.retain(|d| d.index != index);
        let record_removed = self.manifest.discs.len() != records_before;
        if entries > 0 || sources_removed || record_removed {
            self.dirty = true;
            self.index = index_of(&self.manifest);
            self.entries_cost = self.entries_cost.saturating_sub(removed_cost);
        }
        DroppedDisc {
            entries,
            bytes,
            serial,
        }
    }

    /// Import a file from the filesystem. The source is opened read-only; its size, mtime and
    /// identity are compared before and after the copy and the file is rejected if they differ.
    ///
    /// Symbolic links are resolved first, and what is imported (and re-checked afterwards) is
    /// the file they lead to; nothing confines it to a tree, so a caller importing from a tree
    /// it does not trust uses [`ImportBatch::add_file_within`].
    pub fn add_file(
        &mut self,
        logical_path: &str,
        source_path: &Path,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        self.add_file_resolved(logical_path, source_path, None, source)
    }

    /// [`ImportBatch::add_file`] for a source that must stay inside `root`: the source is
    /// resolved (every symbolic link followed) and refused with
    /// [`LibraryError::SourceOutsideRoot`] unless the real file lies inside the real `root`.
    /// A link that leads somewhere else inside the tree is fine; one that leads out of it is
    /// never followed. The importer of PC installs uses this with the install directory.
    pub fn add_file_within(
        &mut self,
        logical_path: &str,
        source_path: &Path,
        root: &Path,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        self.add_file_resolved(logical_path, source_path, Some(root), source)
    }

    fn add_file_resolved(
        &mut self,
        logical_path: &str,
        source_path: &Path,
        root: Option<&Path>,
        source: SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        let logical = normalize_logical_path(logical_path, self.manifest.provenance.platform)?;
        // Resolve every symbolic link ONCE. What is checked, opened, stamped and re-checked
        // below is the real file, never a name that could be pointed somewhere else meanwhile.
        let real = fs::canonicalize(source_path).map_err(io_err(source_path))?;
        if real.starts_with(&self.lib.root_canonical) {
            return Err(LibraryError::SourceInsideLibrary(source_path.to_path_buf()));
        }
        if let Some(root) = root {
            let root_real = fs::canonicalize(root).map_err(io_err(root))?;
            if !real.starts_with(&root_real) {
                return Err(LibraryError::SourceOutsideRoot {
                    path: source_path.to_path_buf(),
                    root: root.to_path_buf(),
                });
            }
        }
        // stat first: a FIFO or device must never be opened (it could block or never end)
        let meta = fs::metadata(&real).map_err(io_err(source_path))?;
        if !meta.is_file() {
            return Err(LibraryError::NotRegularFile(source_path.to_path_buf()));
        }
        self.check_size(source_path.display(), meta.len())?;
        let mut file = File::open(&real).map_err(io_err(source_path))?;
        let opened = file.metadata().map_err(io_err(source_path))?;
        if !opened.is_file() {
            return Err(LibraryError::NotRegularFile(source_path.to_path_buf()));
        }
        // What was opened must be the very entry the resolved name denotes. A symbolic link put
        // in its place after the resolution above would have been followed by the open; here it
        // shows up as a name that is a link, or that is another file than the one opened.
        match fs::symlink_metadata(&real) {
            Ok(by_name) if by_name.is_file() && same_file(&by_name, &opened) => {}
            _ => return Err(LibraryError::SourceChanged(source_path.to_path_buf())),
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
        before
            .verify(&real)
            .map_err(|_| LibraryError::SourceChanged(source_path.to_path_buf()))?;
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

    /// Refuse to let the manifest grow past what [`write_manifest`] would accept, BEFORE the
    /// entry's blob is stored (an entry that cannot be recorded must not cost a blob). `added`
    /// is what the entry about to be added costs. The estimate leaves some room for what is not
    /// an entry; the exact check at write time has the last word.
    fn gate_manifest_size(&mut self, added: u64) -> Result<(), LibraryError> {
        let limit = self.lib.max_manifest_bytes;
        let size = fixed_json_size(&mut self.manifest)
            .saturating_add(self.entries_cost)
            .saturating_add(added);
        let slack = MANIFEST_SLACK.min(limit / 16);
        // what is reported includes the headroom, so it is always over the limit it is shown
        // next to
        let needed = size.saturating_add(slack);
        if needed > limit {
            return Err(LibraryError::ManifestTooLarge {
                set_id: self.set_id.clone(),
                size: needed,
                limit,
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
        let known = self.index.get(&logical).copied();
        if known.is_none() {
            // A prefix the importer froze takes no new entries. Checked BEFORE touching the store.
            if let Some(prefix) = self.frozen.iter().find(|p| logical.starts_with(p.as_str())) {
                return Err(LibraryError::PrefixFrozen {
                    set_id: self.set_id.clone(),
                    logical_path: logical,
                    prefix: prefix.clone(),
                });
            }
            // The writer enforces the reader's entry cap: a set must never reach a state that
            // `load_manifest` would refuse. Checked BEFORE touching the store.
            if self.manifest.entries.len() >= self.lib.max_entries {
                return Err(LibraryError::TooManyEntries {
                    set_id: self.set_id.clone(),
                    limit: self.lib.max_entries,
                });
            }
        }
        // Check for a conflicting entry BEFORE touching the store.
        if let Some(existing) = known.and_then(|i| self.manifest.entries.get(i)) {
            if existing.blake3 != pending.hash_hex || existing.size != pending.size {
                return Err(LibraryError::EntryConflict {
                    set_id: self.set_id.clone(),
                    logical_path: logical,
                    existing: existing.blake3.clone(),
                    incoming: pending.hash_hex.clone(),
                });
            }
        }
        // ... and for the manifest still fitting the byte cap the reader enforces
        let new_entry = known.is_none().then(|| ManifestEntry {
            logical_path: logical.clone(),
            blake3: pending.hash_hex.clone(),
            size: pending.size,
            source: source.clone(),
        });
        let added_cost = new_entry.as_ref().map_or(0, entry_json_cost);
        if new_entry.is_some() {
            self.gate_manifest_size(added_cost)?;
        }
        let hash_hex = pending.hash_hex.clone();
        let size = pending.size;
        let stored = self.lib.store_blob(pending)?;
        self.note_source(&source);
        let status = match new_entry {
            // identical content at this path: just the extra source, if new, was remembered
            None => EntryStatus::Unchanged,
            Some(entry) => {
                self.manifest.entries.push(entry);
                self.index.insert(
                    logical.clone(),
                    self.manifest.entries.len().saturating_sub(1),
                );
                self.entries_cost = self.entries_cost.saturating_add(added_cost);
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

    /// Persist the manifest now if anything changed since the last checkpoint. Refuses (and
    /// leaves the manifest on disk as it was) when the result would be more than the reader's
    /// size cap, [`crate::MAX_MANIFEST_BYTES`].
    pub fn checkpoint(&mut self) -> Result<(), LibraryError> {
        if !self.dirty {
            return Ok(());
        }
        self.manifest.sort_entries();
        self.index = index_of(&self.manifest);
        self.manifest.imported_at = self.imported_at;
        self.manifest.importer_version = self.lib.importer_version.clone();
        write_manifest(
            &self.set_dir.join(MANIFEST_FILE),
            &self.manifest,
            self.lib.max_manifest_bytes,
        )?;
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
    use std::time::Duration;

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

    // -----------------------------------------------------------------------------------------
    // findings 7-10: writer-side caps, symlinks, disc records, set removal, locks, sweeps
    // -----------------------------------------------------------------------------------------

    fn ps1_prov() -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Ps1,
            edition: "PS1 original".into(),
            region: "NTSC-U".into(),
            language: "en".into(),
            revision: "unknown".into(),
            sources: vec![],
            modded: false,
        }
    }

    fn disc_src(index: u8, serial: &str) -> SourceRef {
        SourceRef::Disc {
            index,
            serial: serial.into(),
        }
    }

    fn put(
        batch: &mut ImportBatch<'_>,
        name: &str,
        bytes: &[u8],
        source: &SourceRef,
    ) -> Result<ImportOutcome, LibraryError> {
        batch.add_reader(
            name,
            &mut Cursor::new(bytes.to_vec()),
            Some(bytes.len() as u64),
            source.clone(),
        )
    }

    /// Resets the thread's "no advisory locks here" switch even when the test fails.
    struct NoLocks;
    impl NoLocks {
        fn on() -> NoLocks {
            crate::fsutil::pretend_locks_are_unsupported(true);
            NoLocks
        }
    }
    impl Drop for NoLocks {
        fn drop(&mut self) {
            crate::fsutil::pretend_locks_are_unsupported(false);
        }
    }

    /// Finding 7: the writer enforces the byte cap, and does so before a blob is stored for an
    /// entry that cannot be recorded.
    #[test]
    fn an_entry_that_would_burst_the_manifest_cap_is_refused_before_its_blob_is_stored() {
        let (_tmp, lib) = setup();
        let lib = lib.with_max_manifest_bytes(2_500);
        let set = SetHandle::new(pc_prov("unknown"));
        let mut batch = lib.begin_import(&set, 1).unwrap();
        let mut accepted = 0usize;
        let err = loop {
            let body = format!("body of entry {accepted}");
            match put(&mut batch, &format!("d/{accepted:03}"), body.as_bytes(), &SourceRef::Fixture) {
                Ok(_) => accepted += 1,
                Err(e) => break e,
            }
            assert!(accepted < 100, "the cap never fired");
        };
        assert!(
            matches!(err, LibraryError::ManifestTooLarge { limit: 2_500, size, .. } if size > 2_500),
            "{err}"
        );
        assert!(err.to_string().contains("set on disk is unchanged"), "{err}");
        assert!(accepted >= 3, "a few entries fit ({accepted})");
        assert_eq!(
            count_files(&lib.library_dir().join("blobs")),
            accepted,
            "no blob for the entry that could not be recorded"
        );
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);
        // what was accepted is recorded, the file honours the cap, and the set is not bricked
        batch.commit().unwrap();
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        assert!(fs::metadata(&path).unwrap().len() <= 2_500);
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().entries.len(), accepted);
        let mut again = lib.begin_import(&set, 2).unwrap();
        // re-adding an entry that is already there costs nothing: it is not "one more"
        put(&mut again, "d/000", b"body of entry 0", &SourceRef::Fixture).unwrap();
    }

    /// The exact check at write time: a manifest over the cap is never written, and the manifest
    /// that is on disk stays as it was.
    #[test]
    fn the_writer_never_replaces_a_manifest_with_one_over_the_cap() {
        let (tmp, lib) = setup();
        let src = write(&tmp.path().join("src"), "a.bin", b"payload");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &src, install(&tmp.path().join("src")), 1)
            .unwrap();
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let before = fs::read(&path).unwrap();
        let mut big = lib.load_manifest(&set.set_id).unwrap();
        for i in 0..40 {
            let mut e = big.entries[0].clone();
            e.logical_path = format!("data/pad-{i:03}");
            big.entries.push(e);
        }
        let err = crate::manifest::write_manifest(&path, &big, 3_000).unwrap_err();
        match err {
            LibraryError::ManifestTooLarge { size, limit, .. } => {
                assert_eq!(limit, 3_000);
                assert!(size > 3_000);
            }
            other => panic!("{other}"),
        }
        assert_eq!(fs::read(&path).unwrap(), before, "the manifest on disk is untouched");
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0, "no temp file is left behind either");
        // the same manifest is written when the cap allows it
        crate::manifest::write_manifest(&path, &big, 1 << 20).unwrap();
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().entries.len(), 41);
    }

    /// The writer's cap IS the reader's cap, to the byte: a manifest of exactly
    /// `MAX_MANIFEST_BYTES` is written and read back, one byte more is refused by both.
    #[test]
    fn the_writer_and_the_reader_agree_on_the_cap_to_the_byte() {
        use crate::manifest::{fixed_json_size, entry_json_cost, MAX_MANIFEST_BYTES};
        let (_tmp, lib) = setup();
        assert_eq!(lib.max_manifest_bytes, MAX_MANIFEST_BYTES);
        let set = SetHandle::new(pc_prov("unknown"));
        let dir = lib.set_dir(&set.set_id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE);

        let mut m = Manifest::new(&set.set_id, &set.provenance, "pad", None);
        let comp = "x".repeat(200);
        let entry = |i: usize| ManifestEntry {
            logical_path: format!("p{i:06}/{comp}/{comp}/{comp}/{comp}"),
            blake3: blake3::hash(&i.to_le_bytes()).to_hex().to_string(),
            size: i as u64,
            source: SourceRef::Fixture,
        };
        // an entry with the widest numbers any of them will have, so that `n` cannot overshoot
        let per_entry = entry_json_cost(&entry(99_999));
        let n = ((MAX_MANIFEST_BYTES - fixed_json_size(&mut m)) / per_entry) as usize;
        m.entries = (0..n).map(entry).collect();
        // pad the one free-text field ("pad", 3 bytes) until the file is EXACTLY the cap
        let len = serde_json::to_vec_pretty(&m).unwrap().len() as u64;
        assert!(len <= MAX_MANIFEST_BYTES, "{len}");
        m.importer_version = "v".repeat((MAX_MANIFEST_BYTES - len) as usize + 3);
        assert_eq!(serde_json::to_vec_pretty(&m).unwrap().len() as u64, MAX_MANIFEST_BYTES);

        // exactly the cap: written, and read back
        crate::manifest::write_manifest(&path, &m, MAX_MANIFEST_BYTES).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_MANIFEST_BYTES);
        let back = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(back.entries.len(), n);

        // one byte more: refused by the writer (nothing changes) and by the reader
        m.importer_version.push('v');
        let err = crate::manifest::write_manifest(&path, &m, MAX_MANIFEST_BYTES).unwrap_err();
        assert!(
            matches!(err, LibraryError::ManifestTooLarge { size, .. } if size == MAX_MANIFEST_BYTES + 1),
            "{err}"
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_MANIFEST_BYTES);
        let bytes = serde_json::to_vec_pretty(&m).unwrap();
        fs::write(&path, bytes).unwrap();
        let err = lib.load_manifest(&set.set_id).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    /// The estimate that gates an entry is never below what the entry really costs, and
    /// close enough to be useful.
    #[test]
    fn the_size_estimate_never_undercounts_the_pretty_printed_manifest() {
        use crate::manifest::{entry_json_cost, fixed_json_size};
        let set = SetHandle::new(pc_prov("unknown"));
        for (n, plen) in [(0usize, 10usize), (1, 10), (7, 40), (50, 300), (200, 1)] {
            let mut m = Manifest::new(&set.set_id, &set.provenance, "importer", None);
            m.entries = (0..n)
                .map(|i| ManifestEntry {
                    logical_path: format!("d/{i:05}{}", "y".repeat(plen)),
                    blake3: blake3::hash(&[i as u8]).to_hex().to_string(),
                    size: (i * 1_000_003) as u64,
                    source: if i % 2 == 0 {
                        SourceRef::Fixture
                    } else {
                        SourceRef::Install { path: "/some/where".into() }
                    },
                })
                .collect();
            let real = serde_json::to_vec_pretty(&m).unwrap().len() as u64;
            let entries: u64 = m.entries.iter().map(entry_json_cost).sum();
            let estimate = fixed_json_size(&mut m) + entries;
            assert!(estimate >= real, "n={n}: estimate {estimate} < real {real}");
            assert!(estimate <= real + 64 + 8 * n as u64, "n={n}: estimate {estimate} far above real {real}");
        }
    }

    /// Finding 9 (library half): a frozen prefix only confirms what is there.
    #[test]
    fn a_frozen_prefix_confirms_what_is_there_and_takes_nothing_new() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        let mut b = lib.begin_import(&set, 1).unwrap();
        for (p, bytes) in [("disc1/A", &b"a"[..]), ("disc1/B", b"b"), ("disc2/C", b"c")] {
            put(&mut b, p, bytes, &SourceRef::Fixture).unwrap();
        }
        b.commit().unwrap();
        let blobs = lib.library_dir().join("blobs");
        assert_eq!(count_files(&blobs), 3);

        let mut b = lib.begin_import(&set, 2).unwrap();
        b.freeze_prefix("disc1/");
        // identical content at an existing path: fine, and nothing changes
        let same = put(&mut b, "disc1/A", b"a", &SourceRef::Fixture).unwrap();
        assert_eq!(same.entry, EntryStatus::Unchanged);
        // a NEW path under the prefix is refused before anything is stored
        let err = put(&mut b, "disc1/NEW", b"brand new bytes", &SourceRef::Fixture).unwrap_err();
        assert!(
            matches!(&err, LibraryError::PrefixFrozen { prefix, logical_path, .. }
                if prefix == "disc1/" && logical_path == "disc1/new"),
            "{err}"
        );
        assert_eq!(count_files(&blobs), 3, "no blob was stored for the refused file");
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);
        // different content at an existing path is the conflict it always was
        assert!(matches!(
            put(&mut b, "disc1/B", b"BB", &SourceRef::Fixture),
            Err(LibraryError::EntryConflict { .. })
        ));
        // another prefix is not affected
        put(&mut b, "disc3/D", b"d", &SourceRef::Fixture).unwrap();
        put(&mut b, "disc2/NEW", b"e", &SourceRef::Fixture).unwrap();
        b.commit().unwrap();
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().entries.len(), 5);
    }

    #[test]
    fn disc_digests_depend_on_content_and_not_on_order() {
        use crate::manifest::DiscDigest;
        let h = |s: &str| blake3::hash(s.as_bytes()).to_hex().to_string();
        let digest = |files: &[(&str, u64, &str)]| {
            let mut d = DiscDigest::new();
            for (p, s, c) in files {
                d.add(p, *s, &h(c));
            }
            d.finish(1, "SCUS-94163")
        };
        let a = digest(&[("disc1/A", 3, "a"), ("disc1/B", 4, "b"), ("disc1/C", 5, "c")]);
        let shuffled = digest(&[("disc1/C", 5, "c"), ("disc1/A", 3, "a"), ("disc1/B", 4, "b")]);
        assert_eq!(a, shuffled, "the order files were imported in does not matter");
        assert_eq!((a.files, a.bytes), (3, 12));
        assert!(crate::paths::is_blake3_hex(&a.blake3));
        for other in [
            digest(&[("disc1/A", 3, "a"), ("disc1/B", 4, "b")]),                       // a file fewer
            digest(&[("disc1/A", 3, "a"), ("disc1/B", 4, "b"), ("disc1/D", 5, "c")]),  // another name
            digest(&[("disc1/A", 3, "a"), ("disc1/B", 4, "b"), ("disc1/C", 6, "c")]),  // another size
            digest(&[("disc1/A", 3, "a"), ("disc1/B", 4, "b"), ("disc1/C", 5, "X")]),  // other bytes
        ] {
            assert_ne!(a.blake3, other.blake3);
        }
        // the serial and the number are carried, not hashed: they are checked separately
        let mut d = DiscDigest::new();
        d.add("disc1/A", 3, &h("a"));
        let c = d.finish(2, "SLPS-91440");
        assert_eq!((c.index, c.serial.as_str()), (2, "SLPS-91440"));
    }

    #[test]
    fn disc_records_are_stored_replaced_dropped_and_validated() {
        use crate::manifest::DiscDigest;
        let (_tmp, lib) = setup();
        let set = SetHandle::new(ps1_prov());
        let s1 = disc_src(1, "SCUS-94163");
        let s2 = disc_src(2, "SCUS-94164");
        let mut b = lib.begin_import(&set, 1).unwrap();
        let record = |b: &mut ImportBatch<'_>, idx: u8, serial: &str, src: &SourceRef| {
            let mut digest = DiscDigest::new();
            for name in ["SYSTEM.CNF", "FIELD/X.DAT"] {
                let body = format!("disc {idx} {name}");
                let o = put(b, &format!("disc{idx}/{name}"), body.as_bytes(), src).unwrap();
                digest.add(&o.logical_path, o.size, &o.blake3);
            }
            let rec = digest.finish(idx, serial);
            b.record_disc(rec.clone()).unwrap();
            rec
        };
        let r2 = record(&mut b, 2, "SCUS-94164", &s2);
        let r1 = record(&mut b, 1, "SCUS-94163", &s1);
        b.commit().unwrap();

        // stored, sorted by disc number, and read back
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.discs, vec![r1.clone(), r2.clone()]);
        assert_eq!(m.disc(2), Some(&r2));
        assert!(m.disc(3).is_none());
        let text = fs::read_to_string(lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE)).unwrap();
        assert!(text.contains("\"discs\""), "{text}");

        // recording what is recorded already changes nothing (a re-import stays a no-op)
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let before = fs::read(&path).unwrap();
        let mut b = lib.begin_import(&set, 2).unwrap();
        assert_eq!(b.disc_content(1), Some(&r1));
        b.record_disc(r1.clone()).unwrap();
        b.commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);

        // dropping a disc takes its entries, record and source with it, keeps blobs and others
        let blobs = count_files(&lib.library_dir().join("blobs"));
        let mut b = lib.begin_import(&set, 3).unwrap();
        let dropped = b.drop_disc(1, "disc1/", Some(&s1));
        assert_eq!(dropped.entries, 2);
        assert_eq!(dropped.serial.as_deref(), Some("SCUS-94163"));
        assert!(b.find_entry("disc1/SYSTEM.CNF").is_none());
        assert!(b.find_entry("disc2/SYSTEM.CNF").is_some(), "the index was rebuilt");
        assert!(b.disc_content(1).is_none() && b.disc_content(2).is_some());
        // the kept source stays where it was (disc 2 was imported first); dropping again finds
        // nothing to do
        assert_eq!(b.manifest().provenance.sources, vec![s2.clone(), s1.clone()]);
        let nothing = b.drop_disc(1, "disc1/", Some(&s1));
        assert_eq!((nothing.entries, nothing.serial), (0, None));
        // a replacement disc 1 (another serial) can then be added under the same number
        let replacement = disc_src(1, "SCUS-99999");
        put(&mut b, "disc1/SYSTEM.CNF", b"another disc", &replacement).unwrap();
        b.commit().unwrap();
        let m = lib.load_manifest(&set.set_id).unwrap();
        assert_eq!(m.entries.iter().filter(|e| e.logical_path.starts_with("disc1/")).count(), 1);
        assert_eq!(m.discs, vec![r2.clone()]);
        assert_eq!(count_files(&lib.library_dir().join("blobs")), blobs + 1, "old blobs stay in the store");

        // without keep_source the disc's sources go too
        let mut b = lib.begin_import(&set, 4).unwrap();
        let dropped = b.drop_disc(2, "disc2/", None);
        assert_eq!(dropped.entries, 2);
        assert_eq!(dropped.bytes, r2.bytes);
        assert!(!b.manifest().provenance.sources.contains(&s2));
        assert!(b.manifest().provenance.sources.contains(&replacement));
        drop(b);
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().discs, vec![r2], "dropped without a commit: nothing changed on disk");
    }

    #[test]
    fn invalid_disc_records_are_refused_when_added_and_when_read() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(ps1_prov());
        let good = crate::manifest::DiscContent {
            index: 1,
            serial: "SCUS-94163".into(),
            files: 2,
            bytes: 10,
            blake3: blake3::hash(b"x").to_hex().to_string(),
        };
        let mut b = lib.begin_import(&set, 1).unwrap();
        let bad: Vec<crate::manifest::DiscContent> = vec![
            crate::manifest::DiscContent { index: 0, ..good.clone() },
            crate::manifest::DiscContent { serial: String::new(), ..good.clone() },
            crate::manifest::DiscContent { serial: "has space".into(), ..good.clone() },
            crate::manifest::DiscContent { serial: "x".repeat(crate::manifest::MAX_DISC_SERIAL + 1), ..good.clone() },
            crate::manifest::DiscContent { serial: "ctl\u{7}".into(), ..good.clone() },
            crate::manifest::DiscContent { blake3: "abc".into(), ..good.clone() },
            crate::manifest::DiscContent { blake3: good.blake3.to_uppercase(), ..good.clone() },
        ];
        for rec in bad {
            assert!(
                matches!(b.record_disc(rec.clone()), Err(LibraryError::BadManifest { .. })),
                "{rec:?}"
            );
        }
        b.record_disc(good.clone()).unwrap();
        b.commit().unwrap();

        // what the writer accepts is what the reader accepts: and a hand-edited manifest that
        // breaks the same rules is refused on read
        let path = lib.set_dir(&set.set_id).unwrap().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        let cases = [
            ("duplicate number", text.replacen("\"discs\": [", &format!("\"discs\": [{{\"index\": 1, \"serial\": \"SCUS-94163\", \"files\": 1, \"bytes\": 1, \"blake3\": \"{}\"}},", good.blake3), 1)),
            ("disc number 0", text.replace("\"index\": 1,\n      \"serial\"", "\"index\": 0,\n      \"serial\"")),
            ("bad digest", text.replace(&good.blake3, "nothex")),
        ];
        for (name, bad_text) in cases {
            assert_ne!(bad_text, text, "{name}: the edit did not apply");
            fs::write(&path, &bad_text).unwrap();
            assert!(
                matches!(lib.load_manifest(&set.set_id), Err(LibraryError::BadManifest { .. })),
                "{name}"
            );
        }
        // too many records
        let mut m: Manifest = serde_json::from_str(&text).unwrap();
        m.discs = (1..=(crate::manifest::MAX_DISC_RECORDS as u8 + 1))
            .map(|i| crate::manifest::DiscContent { index: i, ..good.clone() })
            .collect();
        fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        assert!(matches!(lib.load_manifest(&set.set_id), Err(LibraryError::BadManifest { .. })));
        assert!(crate::manifest::write_manifest(&path, &m, 1 << 24).is_err(), "the writer refuses it too");
        fs::write(&path, &text).unwrap();
        lib.load_manifest(&set.set_id).unwrap();
    }

    /// Finding 8 (library half): a symbolic link cannot lead an import out of its tree.
    #[cfg(unix)]
    #[test]
    fn add_file_within_follows_links_inside_the_tree_and_refuses_links_out_of_it() {
        use std::os::unix::fs::symlink;
        let (tmp, lib) = setup();
        let root = tmp.path().join("install");
        let outside = tmp.path().join("elsewhere");
        write(&root.join("data"), "real.lgp", b"the real file");
        write(&outside, "secret.txt", b"NOT-A-REAL-SECRET: a file outside the tree");
        write(&outside.join("dir"), "inner.bin", b"inside a linked directory");
        symlink(outside.join("secret.txt"), root.join("data/leak.lgp")).unwrap();
        symlink(root.join("data/real.lgp"), root.join("data/alias.lgp")).unwrap(); // stays inside
        symlink(Path::new("real.lgp"), root.join("data/relative.lgp")).unwrap();
        symlink(outside.join("dir"), root.join("data/linkdir")).unwrap();
        symlink(root.join("data/loop-b"), root.join("data/loop-a")).unwrap();
        symlink(root.join("data/loop-a"), root.join("data/loop-b")).unwrap();
        symlink(root.join("data/nothing"), root.join("data/dangling")).unwrap();

        let set = SetHandle::new(pc_prov("unknown"));
        let from = install(&root);
        let mut b = lib.begin_import(&set, 1).unwrap();
        let blobs = lib.library_dir().join("blobs");

        // out of the tree: refused, nothing stored, the target was not even opened for copying
        for leak in ["data/leak.lgp", "data/linkdir/inner.bin"] {
            let err = b
                .add_file_within("data/x.lgp", &root.join(leak), &root, from.clone())
                .unwrap_err();
            assert!(matches!(err, LibraryError::SourceOutsideRoot { .. }), "{leak}: {err}");
            assert!(err.to_string().contains("outside"), "{err}");
        }
        assert_eq!(count_files(&blobs), 0);
        assert_eq!(count_files(&lib.library_dir().join("tmp")), 0);

        // links that stay inside are followed: the blob holds the target's bytes
        for name in ["data/real.lgp", "data/alias.lgp", "data/relative.lgp"] {
            let o = b
                .add_file_within(&format!("data/{}.out", name.rsplit('/').next().unwrap()), &root.join(name), &root, from.clone())
                .unwrap();
            assert_eq!(o.blake3, blake3::hash(b"the real file").to_hex().to_string(), "{name}");
        }
        assert_eq!(count_files(&blobs), 1, "three names, one content");

        // loops and dangling links are errors, not hangs or panics
        for broken in ["data/loop-a", "data/dangling"] {
            assert!(
                matches!(
                    b.add_file_within("data/y.lgp", &root.join(broken), &root, from.clone()),
                    Err(LibraryError::Io { .. })
                ),
                "{broken}"
            );
        }
        // the root itself may be reached through a link
        let via = tmp.path().join("via-root");
        symlink(&root, &via).unwrap();
        b.add_file_within("data/z.lgp", &via.join("data/real.lgp"), &via, from.clone())
            .unwrap();
        // a path that merely starts with the root's name is not inside it
        write(&tmp.path().join("install-evil"), "f.bin", b"sibling");
        assert!(matches!(
            b.add_file_within("data/w.lgp", &tmp.path().join("install-evil/f.bin"), &root, from.clone()),
            Err(LibraryError::SourceOutsideRoot { .. })
        ));
        b.commit().unwrap();
        // plain add_file resolves a link and imports the real file (and says so in its docs)
        lib.import_file(&set, "data/plain.lgp", &root.join("data/leak.lgp"), from, 2).unwrap();
        // the originals were not touched
        assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"NOT-A-REAL-SECRET: a file outside the tree");
        assert_eq!(fs::read(root.join("data/real.lgp")).unwrap(), b"the real file");
    }

    /// A link into the library itself is refused whichever way it is spelled.
    #[cfg(unix)]
    #[test]
    fn links_into_the_library_are_refused_even_from_a_confined_import() {
        let (tmp, lib) = setup();
        let inside = write(&lib.library_dir().join("sets"), "sneaky.bin", b"x");
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&inside, root.join("l.bin")).unwrap();
        let set = SetHandle::new(pc_prov("unknown"));
        let mut b = lib.begin_import(&set, 1).unwrap();
        let err = b
            .add_file_within("data/l.bin", &root.join("l.bin"), &root, SourceRef::Fixture)
            .unwrap_err();
        assert!(matches!(err, LibraryError::SourceInsideLibrary(_)), "{err}");
    }

    #[test]
    fn the_no_trace_library_check_creates_nothing_and_sees_inside_links() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state-that-does-not-exist");
        let img = write(&tmp.path().join("images"), "a.iso", b"x");
        // no library yet: nothing can be inside it, and nothing is created
        Library::ensure_source_outside_library(&state, &img).unwrap();
        assert!(!state.exists());
        // once there is one, a source inside it is refused, a source beside it is not
        let lib = Library::open(&state).unwrap();
        let inside = write(lib.library_dir(), "evil.iso", b"y");
        assert!(matches!(
            Library::ensure_source_outside_library(&state, &inside),
            Err(LibraryError::SourceInsideLibrary(_))
        ));
        Library::ensure_source_outside_library(&state, &img).unwrap();
        let beside = write(&state, "beside.iso", b"z");
        Library::ensure_source_outside_library(&state, &beside).unwrap();
        #[cfg(unix)]
        {
            let link = tmp.path().join("l.iso");
            std::os::unix::fs::symlink(&inside, &link).unwrap();
            assert!(matches!(
                Library::ensure_source_outside_library(&state, &link),
                Err(LibraryError::SourceInsideLibrary(_))
            ));
        }
        // a source that does not exist is an I/O error, as it always was
        assert!(matches!(
            Library::ensure_source_outside_library(&state, &tmp.path().join("missing")),
            Err(LibraryError::Io { .. })
        ));
    }

    /// Finding 10: without advisory locks an import goes ahead and says it is unlocked.
    #[test]
    fn imports_fall_back_to_unlocked_where_the_filesystem_has_no_locks() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        {
            let normal = lib.begin_import(&set, 1).unwrap();
            assert!(normal.is_locked());
            assert!(matches!(lib.begin_import(&set, 1), Err(LibraryError::SetBusy(_))));
        }
        let _no_locks = NoLocks::on();
        let mut a = lib.begin_import(&set, 2).unwrap();
        assert!(!a.is_locked(), "the batch says that it is not holding a lock");
        put(&mut a, "d/one", b"1", &SourceRef::Fixture).unwrap();
        // nothing excludes a second import in this mode (that is what "unlocked" means) ...
        let b = lib.begin_import(&set, 2).unwrap();
        assert!(!b.is_locked());
        drop(b);
        // ... but it still works end to end
        a.commit().unwrap();
        assert_eq!(lib.load_manifest(&set.set_id).unwrap().entries.len(), 1);
    }

    /// A lock that is lost to `remove_set` is not mistaken for a held one.
    #[test]
    fn a_new_import_after_remove_set_gets_a_fresh_lock_and_the_old_handle_is_a_ghost() {
        let (_tmp, lib) = setup();
        let set = SetHandle::new(pc_prov("unknown"));
        {
            let mut b = lib.begin_import(&set, 1).unwrap();
            put(&mut b, "d/one", b"1", &SourceRef::Fixture).unwrap();
            b.commit().unwrap();
        }
        let dir = lib.set_dir(&set.set_id).unwrap();
        let lock_path = dir.join(LOCK_FILE);
        // a writer that opened the lock file just before the removal ...
        let stale = crate::fsutil::open_lock_file(&lock_path).unwrap();
        lib.remove_set(&set.set_id).unwrap();
        let fresh = lib.begin_import(&set, 2).unwrap();
        assert!(fresh.is_locked());
        // ... can still "win" a lock on the file that no longer has a name, and must notice
        assert_eq!(crate::fsutil::try_lock_exclusive(&stale).unwrap(), LockOutcome::Held);
        assert!(!lock_names_path(&stale, &lock_path));
        // while a real second writer is still kept out by the file that does
        assert!(matches!(lib.begin_import(&set, 3), Err(LibraryError::SetBusy(_))));
        drop(fresh);
    }

    #[test]
    fn remove_set_drops_a_set_and_nothing_else() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"alpha");
        let b = write(&src, "b.bin", b"beta");
        let mtime_a = fs::metadata(&a).unwrap().modified().unwrap();
        let keep = SetHandle::new(pc_prov("PAL"));
        let gone = SetHandle::new(pc_prov("NTSC-U"));
        lib.import_file(&keep, "data/a.bin", &a, install(&src), 1).unwrap();
        lib.import_file(&gone, "data/b.bin", &b, install(&src), 1).unwrap();
        lib.import_file(&gone, "data/a.bin", &a, install(&src), 1).unwrap();
        let blobs_before = count_files(&lib.library_dir().join("blobs"));
        let kept_manifest = fs::read(lib.set_dir(&keep.set_id).unwrap().join(MANIFEST_FILE)).unwrap();

        let r = lib.remove_set(&gone.set_id).unwrap();
        assert_eq!(r.set_id, gone.set_id);
        assert_eq!((r.entries, r.bytes), (Some(2), Some(9)));
        assert!(r.manifest_problem.is_none() && r.locked);
        assert!(!lib.set_dir(&gone.set_id).unwrap().exists());
        assert_eq!(lib.list_sets().unwrap(), vec![keep.set_id.clone()]);
        // the other set, every blob and the originals are exactly as they were
        assert_eq!(count_files(&lib.library_dir().join("blobs")), blobs_before);
        assert_eq!(fs::read(lib.set_dir(&keep.set_id).unwrap().join(MANIFEST_FILE)).unwrap(), kept_manifest);
        assert!(lib.verify_set(&keep.set_id).unwrap().is_empty());
        assert_eq!(fs::read(&a).unwrap(), b"alpha");
        assert_eq!(fs::read(&b).unwrap(), b"beta");
        assert_eq!(fs::metadata(&a).unwrap().modified().unwrap(), mtime_a);
        // removing it again says there is no such set; the set can be imported afresh, and the
        // blobs that were kept are reused
        assert!(matches!(lib.remove_set(&gone.set_id), Err(LibraryError::SetNotFound(_))));
        let again = lib.import_file(&gone, "data/b.bin", &b, install(&src), 5).unwrap();
        assert!(!again.blob_created, "the blob of the removed set was kept and reused");
    }

    /// Finding 7: a set whose manifest can no longer be read can still be dropped.
    #[test]
    fn a_bricked_or_damaged_set_can_be_removed() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"alpha");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(&src), 1).unwrap();
        let dir = lib.set_dir(&set.set_id).unwrap();
        let manifest = dir.join(MANIFEST_FILE);

        // a manifest over the reader's cap (what an old writer could produce): sparse, instant
        let f = fs::OpenOptions::new().write(true).open(&manifest).unwrap();
        f.set_len(crate::manifest::MAX_MANIFEST_BYTES + 1).unwrap();
        drop(f);
        let err = lib.load_manifest(&set.set_id).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(lib.begin_import(&set, 2).is_err(), "the set really is stuck");
        fs::write(dir.join(".manifest.json.tmp-1-0-aaaa"), b"half").unwrap();
        let r = lib.remove_set(&set.set_id).unwrap();
        assert_eq!(r.entries, None);
        assert!(r.manifest_problem.unwrap().contains("too large"));
        assert!(!dir.exists(), "temp files and all");
        assert_eq!(fs::read(&a).unwrap(), b"alpha");

        // garbage, and no manifest at all (a directory a killed first import left)
        lib.import_file(&set, "data/a.bin", &a, install(&src), 3).unwrap();
        fs::write(&manifest, b"{ not json").unwrap();
        let r = lib.remove_set(&set.set_id).unwrap();
        assert!(r.manifest_problem.unwrap().contains("cannot parse"));
        let headless = lib.set_dir("headless-00000000").unwrap();
        fs::create_dir_all(&headless).unwrap();
        let r = lib.remove_set("headless-00000000").unwrap();
        assert_eq!(r.entries, None);
        assert!(r.manifest_problem.unwrap().contains("no manifest"));
        assert!(!headless.exists());
    }

    #[test]
    fn remove_set_refuses_a_busy_set_a_bad_id_and_a_link() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"alpha");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(&src), 1).unwrap();
        // an import holds the set: refused, and nothing is removed
        let writer = lib.begin_import(&set, 2).unwrap();
        assert!(matches!(lib.remove_set(&set.set_id), Err(LibraryError::SetBusy(_))));
        assert!(lib.load_manifest(&set.set_id).is_ok());
        drop(writer);
        // ids are validated like everywhere else
        for bad in ["../etc", "", "UPPER", "a/b", ".."] {
            assert!(matches!(lib.remove_set(bad), Err(LibraryError::BadSetId(_))), "{bad:?}");
        }
        assert!(matches!(lib.remove_set("no-such-set-00000000"), Err(LibraryError::SetNotFound(_))));
        // a set directory that is a link (or a file) is never deleted through
        #[cfg(unix)]
        {
            let victim = tmp.path().join("victim-dir");
            write(&victim, "precious.txt", b"keep me");
            let link = lib.set_dir("linked-00000000").unwrap();
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            assert!(matches!(lib.remove_set("linked-00000000"), Err(LibraryError::NotASetDirectory(_))));
            assert_eq!(fs::read(victim.join("precious.txt")).unwrap(), b"keep me");
            assert!(fs::symlink_metadata(&link).is_ok());
        }
        let file = lib.set_dir("afile-00000000").unwrap();
        fs::write(&file, b"x").unwrap();
        assert!(matches!(lib.remove_set("afile-00000000"), Err(LibraryError::NotASetDirectory(_))));
        // the set survived all of it
        assert!(lib.verify_set(&set.set_id).unwrap().is_empty());
    }

    #[test]
    fn remove_set_without_advisory_locks_still_works_and_says_so() {
        let (tmp, lib) = setup();
        let a = write(&tmp.path().join("src"), "a.bin", b"alpha");
        let set = SetHandle::new(pc_prov("unknown"));
        lib.import_file(&set, "data/a.bin", &a, install(tmp.path()), 1).unwrap();
        let _no_locks = NoLocks::on();
        let r = lib.remove_set(&set.set_id).unwrap();
        assert!(!r.locked);
        assert!(lib.list_sets().unwrap().is_empty());
    }

    /// Finding 10: the manifest temp files of a killed writer are swept on the next open, but
    /// never from under a live writer and never anything else.
    #[test]
    fn leftover_manifest_temp_files_are_swept_on_open_unless_a_writer_holds_the_set() {
        let (tmp, lib) = setup();
        let src = tmp.path().join("src");
        let a = write(&src, "a.bin", b"alpha");
        let idle = SetHandle::new(pc_prov("PAL"));
        let busy = SetHandle::new(pc_prov("NTSC-U"));
        lib.import_file(&idle, "data/a.bin", &a, install(&src), 1).unwrap();
        lib.import_file(&busy, "data/a.bin", &a, install(&src), 1).unwrap();
        let idle_dir = lib.set_dir(&idle.set_id).unwrap();
        let busy_dir = lib.set_dir(&busy.set_id).unwrap();
        let mk = |dir: &Path, name: &str, age: u64| {
            let p = dir.join(name);
            fs::write(&p, vec![9u8; 128]).unwrap();
            backdate(&p, age);
            p
        };
        let idle_stale = mk(&idle_dir, ".manifest.json.tmp-11-0-aaaaaaaa", 7200);
        let idle_fresh = mk(&idle_dir, ".manifest.json.tmp-12-1-bbbbbbbb", 0);
        // (dated ahead, so that no slow machine can age it past the sweep threshold)
        fs::OpenOptions::new()
            .write(true)
            .open(&idle_fresh)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        let idle_foreign = mk(&idle_dir, "notes.txt", 7200);
        let busy_stale = mk(&busy_dir, ".manifest.json.tmp-13-2-cccccccc", 7200);
        // atomic_write names the temp file `.<file name>.tmp-*`, and the file name is `.gitignore`
        let root_stale = mk(lib.library_dir(), "..gitignore.tmp-14-3-dddddddd", 7200);
        let manifest_before = fs::read(idle_dir.join(MANIFEST_FILE)).unwrap();

        // a live writer holds the busy set: its temp file may be the one being written right now
        let writer = lib.begin_import(&busy, 2).unwrap();
        let reopened = Library::open(lib.state_dir()).unwrap();
        assert_eq!(reopened.stale_parts_removed(), 2, "the idle set's temp file and the root's");
        assert!(!idle_stale.exists() && !root_stale.exists());
        assert!(idle_fresh.exists(), "a young temp file is never swept");
        assert!(idle_foreign.exists(), "only temp files of the manifest are touched");
        assert!(busy_stale.exists(), "a set a writer holds is left alone");
        assert_eq!(fs::read(idle_dir.join(MANIFEST_FILE)).unwrap(), manifest_before);

        // once the writer is gone the leftover goes as well
        drop(writer);
        let third = Library::open(lib.state_dir()).unwrap();
        assert_eq!(third.stale_parts_removed(), 1);
        assert!(!busy_stale.exists());
        // open_existing never deletes anything
        let again = mk(&idle_dir, ".manifest.json.tmp-15-4-eeeeeeee", 7200);
        let ro = Library::open_existing(lib.state_dir()).unwrap();
        assert_eq!(ro.stale_parts_removed(), 0);
        assert!(again.exists());
        // a set without a lock file (this code never wrote there) is not touched, and
        // without advisory locks nothing is swept (a writer cannot be told from a corpse)
        let _no_locks = NoLocks::on();
        assert_eq!(Library::open(lib.state_dir()).unwrap().stale_parts_removed(), 0);
        assert!(again.exists());
    }

    /// The directories an import creates are private, and a new library says so on disk.
    #[cfg(unix)]
    #[test]
    fn a_new_library_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("fresh").join("state");
        let lib = Library::open(&state).unwrap();
        let set = SetHandle::new(pc_prov("unknown"));
        let a = write(&tmp.path().join("src"), "a.bin", b"alpha");
        lib.import_file(&set, "data/a.bin", &a, install(tmp.path()), 1).unwrap();
        let mut stack = vec![state.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let m = e.metadata().unwrap();
                let mode = m.permissions().mode() & 0o777;
                assert_eq!(mode & 0o077, 0, "{:?} is {mode:o}: not private", e.path());
                if m.is_dir() {
                    stack.push(e.path());
                }
            }
        }
        for parent in [state.parent().unwrap().to_path_buf(), state.clone()] {
            assert_eq!(fs::metadata(&parent).unwrap().permissions().mode() & 0o077, 0, "{parent:?}");
        }
    }
}
