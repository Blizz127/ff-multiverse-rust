//! `remove-set`: drop one library set, even a broken one, and the import sessions that point at it.
//!
//! This is the way out of a set that cannot be used any more (a damaged manifest, one over the
//! reader's size cap, a directory a killed first import left) and of a set that was imported
//! wrongly. It needs `--yes`; without it nothing is changed and the command says what it would
//! do. What it touches, exactly:
//!
//! - `library/sets/<set-id>/` (the manifest, the lock file, leftover temp files), and
//! - the session files `import-sessions/<name>.json` whose `set_id` is the removed set (a
//!   session that outlived its set would claim discs the library no longer has). Their lock
//!   files stay: lock files are never deleted.
//!
//! What it never touches: the blob store (blobs are shared between sets and are reused when the
//! same content is imported again), other sets, and every original (install, disc image, cue
//! sheet): nothing outside the state directory is opened for writing, and the sources a manifest
//! names are never followed. It refuses while an import holds the set or one of the sessions.

use std::fs;
use std::path::PathBuf;

use fmv_library::{resolve_state_dir, validate_set_id, Library, LibraryError};
use serde::Serialize;

use super::CommandError;
use crate::session::{lock_session, remove_session, sessions_using_set};

#[derive(Debug, Clone)]
pub struct RemoveSetArgs {
    pub set_id: String,
    pub state_dir: Option<PathBuf>,
    /// `--yes`. Without it nothing is removed and the error says what would be.
    pub confirmed: bool,
}

#[derive(Debug, Serialize)]
pub struct RemoveSetResult {
    pub command: &'static str,
    pub set_id: String,
    pub state_dir: String,
    /// Entries the set's manifest listed; absent when it could not be read.
    pub entries: Option<usize>,
    pub bytes: Option<u64>,
    /// Why the manifest could not be read (a broken set); absent for a healthy one.
    pub manifest_problem: Option<String>,
    /// Import sessions that pointed at the set and were removed with it.
    pub sessions_removed: Vec<String>,
    /// Session files that could not be read, so it is unknown whether they pointed at the set:
    /// left alone.
    pub sessions_unreadable: Vec<String>,
    pub blobs: &'static str,
    pub originals: &'static str,
    pub warnings: Vec<String>,
}

const BLOBS_KEPT: &str =
    "kept: blobs are shared between sets and reused when the same content is imported again";
const ORIGINALS_UNTOUCHED: &str = "untouched: nothing outside the state directory is ever written";

pub fn remove_set(args: &RemoveSetArgs) -> Result<RemoveSetResult, CommandError> {
    validate_set_id(&args.set_id)?;
    let state_dir = resolve_state_dir(args.state_dir.as_deref())?;
    let lib = Library::open_existing(&state_dir)?;
    let set_dir = lib.set_dir(&args.set_id)?;
    // look before anything is touched: does the set exist, what does it hold, who points at it
    match fs::symlink_metadata(&set_dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LibraryError::SetNotFound(args.set_id.clone()).into())
        }
        Err(source) => {
            return Err(LibraryError::Io {
                path: set_dir,
                source,
            }
            .into())
        }
    }
    let (entries, bytes, manifest_problem) = match lib.load_manifest(&args.set_id) {
        Ok(m) => (Some(m.entries.len()), Some(m.total_bytes()), None),
        Err(LibraryError::SetNotFound(_)) => {
            (None, None, Some("the set has no manifest".to_string()))
        }
        Err(e) => (None, None, Some(e.to_string())),
    };
    let (using, unreadable) = sessions_using_set(&state_dir, &args.set_id);

    if !args.confirmed {
        let what = match (&entries, &manifest_problem) {
            (Some(n), _) => format!("{n} files, {} bytes in its manifest", bytes.unwrap_or(0)),
            (None, Some(why)) => format!("its manifest is unusable: {why}"),
            (None, None) => "no manifest".to_string(),
        };
        let sessions = if using.is_empty() {
            "no import session points at it".to_string()
        } else {
            format!(
                "the import session(s) {} that point at it would be removed too",
                using.join(", ")
            )
        };
        return Err(CommandError::ConfirmationRequired(format!(
            "remove-set would delete library set {} ({what}); {sessions}. The blob store is kept (blobs are shared and content-addressed) and no original is ever touched. Nothing was changed: pass --yes to remove it",
            args.set_id
        )));
    }

    // Take the sessions first (the order import-disc takes them in), then the set; a refusal
    // anywhere leaves everything as it was. The locks are held until the function returns.
    let mut locks = Vec::new();
    for name in &using {
        locks.push(lock_session(&state_dir, name)?);
    }
    let removed = lib.remove_set(&args.set_id)?;
    let mut warnings = Vec::new();
    let mut sessions_removed = Vec::new();
    for name in &using {
        match remove_session(&state_dir, name) {
            Ok(true) => sessions_removed.push(name.clone()),
            Ok(false) => {}
            Err(e) => warnings.push(format!(
                "the import session {name:?} could not be removed: {e}"
            )),
        }
    }
    if !removed.locked || locks.iter().any(|l| !l.is_held()) {
        warnings.push("this filesystem does not support advisory file locks, so the removal could not tell whether an import was running into the set; make sure none was".to_string());
    }
    if !unreadable.is_empty() {
        warnings.push(format!(
            "{} session file(s) could not be read and were left alone (they may still name the removed set): {}",
            unreadable.len(),
            unreadable.join(", ")
        ));
    }
    Ok(RemoveSetResult {
        command: "remove-set",
        set_id: removed.set_id,
        state_dir: state_dir.display().to_string(),
        entries: removed.entries,
        bytes: removed.bytes,
        manifest_problem: removed.manifest_problem,
        sessions_removed,
        sessions_unreadable: unreadable,
        blobs: BLOBS_KEPT,
        originals: ORIGINALS_UNTOUCHED,
        warnings,
    })
}
