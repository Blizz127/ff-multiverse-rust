//! Consistent copies of a state directory, for rollback before server upgrades.
//!
//! A snapshot is a valid state directory in its own right: it holds `index.log` (and the small
//! `index.json` marker), `accounts/`, `avatars/`, `characters/` and `progression/` but never `LOCK`, temp files
//! or the `index.json.v1.bak` backup of a migration. `index.log` is written last and acts as the
//! completeness marker: opening a half-written snapshot is refused because it has records but
//! no index. (A snapshot taken by a build that still had the version 1 `index.json` is restored
//! as it is, with `index.json` last; opening it migrates it like any other.)

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::index::{self, LogWriter, INDEX_LOG_FILE};
use crate::legacy::{self, LegacyState, LEGACY_INDEX_FILE};
use crate::lock::LOCK_FILE;
use crate::model::MAX_RECORD_BYTES;
use crate::store::{ACCOUNTS_DIR, AVATARS_DIR, CHARACTERS_DIR, PROGRESSION_DIR};

/// What a snapshot or restore copied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub files: usize,
    pub bytes: u64,
}

/// Copy a live store (the caller holds its writer mutex; `live_log` is its log) to `dest`.
pub(crate) fn snapshot_tree(
    root: &Path,
    dest: &Path,
    live_log: &Mutex<LogWriter>,
) -> Result<SnapshotInfo> {
    check_destination(root, dest)?;
    copy_tree(root, dest, Some(live_log))
}

/// Copy a snapshot into `target`, which must be absent or empty and outside the snapshot.
/// Refuses a snapshot whose index is missing, unreadable or from a newer build, and a
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
    check_snapshot_index(snapshot_dir)?;
    check_destination(snapshot_dir, target)?;
    copy_tree(snapshot_dir, target, None)
}

/// The snapshot's index must be there and replay cleanly (a snapshot is written whole, so even
/// a torn final entry means it is damaged).
fn check_snapshot_index(snapshot_dir: &Path) -> Result<()> {
    let log_path = snapshot_dir.join(INDEX_LOG_FILE);
    if index::load_log(&log_path, false)?.is_some() {
        return Ok(());
    }
    // A snapshot from before the index became a log has a complete version 1 index.json.
    match legacy::read(snapshot_dir) {
        Ok(LegacyState::Full(_)) => Ok(()),
        Ok(_) | Err(PersistError::Corrupt { .. }) => Err(PersistError::Snapshot(format!(
            "{} has no usable index.log; it is not a complete snapshot",
            snapshot_dir.display()
        ))),
        Err(e) => Err(e),
    }
}

fn copy_tree(src: &Path, dest: &Path, live_log: Option<&Mutex<LogWriter>>) -> Result<SnapshotInfo> {
    let mut info = SnapshotInfo::default();
    fsio::ensure_dir(dest)?;

    for sub in [ACCOUNTS_DIR, AVATARS_DIR, CHARACTERS_DIR] {
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
    let log_src = src.join(INDEX_LOG_FILE);
    let marker_src = src.join(LEGACY_INDEX_FILE);
    if live_log.is_some() || log_src.exists() {
        // The marker is not what says "complete", so it goes first.
        if marker_src.is_file() {
            copy_file(&marker_src, &dest.join(LEGACY_INDEX_FILE), None, &mut info)?;
        }
        match live_log {
            Some(gate) => {
                // Hold the log lock for the copy, and copy exactly the entries written so far,
                // so a first login recording itself meanwhile cannot leave a torn last line.
                // Registrations are already excluded by the caller's writer lock.
                let log = gate.lock().unwrap_or_else(PoisonError::into_inner);
                copy_file(
                    &log_src,
                    &dest.join(INDEX_LOG_FILE),
                    Some(log.len()),
                    &mut info,
                )?;
            }
            None => copy_file(&log_src, &dest.join(INDEX_LOG_FILE), None, &mut info)?,
        }
    } else {
        // A snapshot from before the index became a log: its index.json is the marker.
        copy_file(&marker_src, &dest.join(LEGACY_INDEX_FILE), None, &mut info)?;
    }
    Ok(info)
}

/// Copy `src` over `dest` atomically (temp file, fsync, rename, fsync the directory), streaming,
/// optionally only the first `limit` bytes.
fn copy_file(src: &Path, dest: &Path, limit: Option<u64>, info: &mut SnapshotInfo) -> Result<()> {
    let file = File::open(src).map_err(|e| PersistError::io("open", src, e))?;
    let mut copied = 0u64;
    fsio::atomic_write_with(dest, |out| {
        copied = match limit {
            Some(max) => io::copy(&mut (&file).take(max), out)?,
            None => io::copy(&mut &file, out)?,
        };
        Ok(())
    })?;
    if let Some(max) = limit {
        if copied != max {
            return Err(PersistError::Snapshot(format!(
                "{} is shorter ({copied} bytes) than the {max} bytes written to it",
                src.display()
            )));
        }
    }
    info.files = info.files.saturating_add(1);
    info.bytes = info.bytes.saturating_add(copied);
    Ok(())
}

/// Copy every `*.json` in `src_dir` into the fresh `dest_dir`: each file fsynced, the
/// directory once at the end. No temp+rename is needed because the destination is new and
/// only the final `index.log` marks the snapshot complete.
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
        io::Write::write_all(&mut file, &bytes)
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
