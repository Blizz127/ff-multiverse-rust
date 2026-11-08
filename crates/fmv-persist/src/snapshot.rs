//! Consistent copies of a state directory, for rollback before server upgrades.
//!
//! A snapshot is a valid state directory in its own right: it holds `index.json`, `accounts/`,
//! `avatars/` and `progression/` but never `LOCK` or temp files. `index.json` is written last
//! and acts as the completeness marker: opening a half-written snapshot is refused because it
//! has records but no index.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::lock::LOCK_FILE;
use crate::model::{MAX_INDEX_BYTES, MAX_RECORD_BYTES};
use crate::store::{IndexFile, ACCOUNTS_DIR, AVATARS_DIR, INDEX_FILE, PROGRESSION_DIR};

/// What a snapshot or restore copied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub files: usize,
    pub bytes: u64,
}

/// Copy a live store (caller holds its mutex) to `dest`.
pub(crate) fn snapshot_tree(root: &Path, dest: &Path) -> Result<SnapshotInfo> {
    check_destination(root, dest)?;
    copy_tree(root, dest)
}

/// Copy a snapshot into `target`, which must be absent or empty and outside the snapshot.
/// Refuses a snapshot whose `index.json` is missing, unreadable or from a newer build, and a
/// directory that holds a `LOCK` (that is a live or crashed state directory, not a snapshot;
/// copying it could capture a half-finished write). Does not touch any live state directory:
/// swap directories (with the server stopped) after it succeeds.
pub fn restore_snapshot(
    snapshot_dir: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<SnapshotInfo> {
    let (snapshot_dir, target) = (snapshot_dir.as_ref(), target.as_ref());
    if snapshot_dir.join(LOCK_FILE).exists() {
        return Err(PersistError::Snapshot(format!(
            "{} contains a LOCK file, so it is a state directory rather than a snapshot; \
             take a snapshot from a running server instead",
            snapshot_dir.display()
        )));
    }
    let index_path = snapshot_dir.join(INDEX_FILE);
    let index: IndexFile = fsio::read_json(&index_path, MAX_INDEX_BYTES)?.ok_or_else(|| {
        PersistError::Snapshot(format!(
            "{} has no index.json; it is not a complete snapshot",
            snapshot_dir.display()
        ))
    })?;
    index.validate(&index_path)?;
    check_destination(snapshot_dir, target)?;
    copy_tree(snapshot_dir, target)
}

fn copy_tree(src: &Path, dest: &Path) -> Result<SnapshotInfo> {
    let mut info = SnapshotInfo::default();
    fsio::ensure_dir(dest)?;

    for sub in [ACCOUNTS_DIR, AVATARS_DIR] {
        let (src_dir, dest_dir) = (src.join(sub), dest.join(sub));
        fsio::ensure_dir(&dest_dir)?;
        copy_dir_files(&src_dir, &dest_dir, &mut info)?;
    }

    let (src_prog, dest_prog) = (src.join(PROGRESSION_DIR), dest.join(PROGRESSION_DIR));
    fsio::ensure_dir(&dest_prog)?;
    for account_dir in fsio::list_subdirs(&src_prog)? {
        let dest_dir = dest_prog.join(&account_dir);
        fsio::ensure_dir(&dest_dir)?;
        copy_dir_files(&src_prog.join(&account_dir), &dest_dir, &mut info)?;
    }
    fsio::sync_dir(&dest_prog).map_err(|e| PersistError::io("fsync directory", &dest_prog, e))?;
    fsio::sync_dir(dest).map_err(|e| PersistError::io("fsync directory", dest, e))?;

    // Last, atomically: its presence says the copy above is complete.
    let index = fsio::read_capped(&src.join(INDEX_FILE), MAX_INDEX_BYTES)?.ok_or_else(|| {
        PersistError::Snapshot(format!(
            "{} vanished while copying",
            src.join(INDEX_FILE).display()
        ))
    })?;
    fsio::atomic_write(&dest.join(INDEX_FILE), &index)?;
    info.files = info.files.saturating_add(1);
    info.bytes = info.bytes.saturating_add(index.len() as u64);
    Ok(info)
}

/// Copy every `*.json` in `src_dir` into the fresh `dest_dir`: each file fsynced, the
/// directory once at the end. No temp+rename is needed because the destination is new and
/// only the final `index.json` marks the snapshot complete.
fn copy_dir_files(src_dir: &Path, dest_dir: &Path, info: &mut SnapshotInfo) -> Result<()> {
    for name in fsio::list_json_files(src_dir)? {
        let src = src_dir.join(&name);
        let bytes = fsio::read_capped(&src, MAX_RECORD_BYTES)?.ok_or_else(|| {
            PersistError::Snapshot(format!("{} vanished while copying", src.display()))
        })?;
        let dest = dest_dir.join(&name);
        let mut file = fsio::private_file_options()
            .open(&dest)
            .map_err(|e| PersistError::io("create", &dest, e))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| PersistError::io("write", &dest, e))?;
        info.files = info.files.saturating_add(1);
        info.bytes = info.bytes.saturating_add(bytes.len() as u64);
    }
    fsio::sync_dir(dest_dir).map_err(|e| PersistError::io("fsync directory", dest_dir, e))
}

/// `dest` must be absent or an empty directory, and must not be inside `src`.
fn check_destination(src: &Path, dest: &Path) -> Result<()> {
    match fs::read_dir(dest) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(PersistError::Snapshot(format!(
                    "destination {} is not empty",
                    dest.display()
                )));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(PersistError::io("list directory", dest, e)),
    }
    let src = resolve(src)?;
    let dest = resolve(dest)?;
    if dest.starts_with(&src) {
        return Err(PersistError::Snapshot(format!(
            "destination {} is inside the source {}",
            dest.display(),
            src.display()
        )));
    }
    Ok(())
}

/// Absolute path with symlinks resolved, also for paths that do not exist yet: canonicalize
/// the deepest existing ancestor and append the rest.
fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| PersistError::io("get current directory", path, e))?
            .join(path)
    };
    let mut existing = absolute.as_path();
    let mut tail: Vec<&OsStr> = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                resolved.extend(tail.iter().rev());
                return Ok(resolved);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match (existing.parent(), existing.file_name()) {
                    (Some(parent), Some(name)) => {
                        tail.push(name);
                        existing = parent;
                    }
                    _ => {
                        return Err(PersistError::Snapshot(format!(
                            "cannot resolve {}",
                            path.display()
                        )))
                    }
                }
            }
            Err(e) => return Err(PersistError::io("resolve path", existing, e)),
        }
    }
}
