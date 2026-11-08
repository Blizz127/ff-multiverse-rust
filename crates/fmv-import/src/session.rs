//! Sequential disc-import sessions, stored as `<state>/import-sessions/<name>.json`.
//!
//! The first disc of a session fixes its family (edition, region, language, revision). Every
//! later disc must belong to the same family. Session files live in the state directory and are
//! treated as untrusted when loaded: size-capped, parsed without panics and cross-checked.
//!
//! The directory is created private (0700) and so are the session files and the per-session
//! lock files (0600): they name the owner's images. A session's lock is advisory; where the
//! filesystem has none the import goes ahead UNLOCKED and says so ([`SessionLock::is_held`]).
//! The temp files of a session writer that was killed are swept the next time the session is
//! locked.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fmv_core::{EditionProvenance, GameId, Platform, SourceRef};
use fmv_library::{
    atomic_write, create_private_dir_durable, open_lock_file, set_id_for, sweep_stale_temps,
    try_lock_exclusive, validate_set_id, LockOutcome,
};
use serde::{Deserialize, Serialize};

use crate::serial::EXPECTED_DISCS;

pub const SESSION_FORMAT_VERSION: u32 = 1;
pub const MAX_SESSION_BYTES: u64 = 1024 * 1024;
const MAX_DISCS: usize = 64;
const SESSIONS_DIR: &str = "import-sessions";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid set name {0:?}: use 1-64 characters from A-Z a-z 0-9 . _ - and do not start with '.'")]
    BadName(String),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("session file {path} is invalid: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("import session {0:?} is being updated by another import; retry when it finishes")]
    Busy(String),
}

fn default_true() -> bool {
    true
}

/// What a disc set is: everything that must agree between discs of one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Family {
    pub game: GameId,
    pub platform: Platform,
    pub edition: String,
    pub region: String,
    pub language: String,
    pub revision: String,
    /// True when the first disc's serial matched the serial table (which is itself UNVERIFIED).
    pub serial_known: bool,
    /// For a serial the table does not know: the region its prefix implies (`PAL` for `SCES-`).
    /// A GUESS. It is kept out of `region` (which stays `unknown`) and only used to stop discs
    /// of apparently different regions from joining one session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_hint: Option<String>,
    /// The session name, for families whose identity the importer cannot establish from the
    /// discs (serials the table does not know, synthetic images). It becomes part of the library
    /// set's edition label, so two unrelated sessions never share (and never mix files in) one
    /// library set. `None` for table matches, which are shared by every session of that edition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Family {
    pub fn label(&self) -> String {
        format!(
            "{} / {} / {} / revision {}{}{}",
            self.edition,
            self.region,
            self.language,
            self.revision,
            match &self.region_hint {
                Some(h) => format!(" (region hint from the serial prefix, a guess: {h})"),
                None => String::new(),
            },
            if self.serial_known {
                ""
            } else {
                " [serial not in the unverified table]"
            }
        )
    }

    /// Discs belong together when game, platform, edition, region, language, revision and region
    /// hint agree. `scope` and `serial_known` are not compared: they follow from the others.
    pub fn same_family(&self, other: &Family) -> bool {
        self.game == other.game
            && self.platform == other.platform
            && self.edition == other.edition
            && self.region == other.region
            && self.language == other.language
            && self.revision == other.revision
            && self.region_hint == other.region_hint
    }

    /// The edition label of the library set: `edition`, plus the session name when the family is
    /// scoped to one (see `scope`). It travels in the path-free `ProvenanceSummary`, so two
    /// players with the same discs but different set names are deliberately NOT compatible: the
    /// frozen provenance type has no field to say "unverified, session-local" any other way.
    pub fn set_edition(&self) -> String {
        match &self.scope {
            Some(s) => format!("{} [set {}]", self.edition, s),
            None => self.edition.clone(),
        }
    }

    pub fn provenance(&self, sources: Vec<SourceRef>) -> EditionProvenance {
        EditionProvenance {
            game: self.game,
            platform: self.platform,
            edition: self.set_edition(),
            region: self.region.clone(),
            language: self.language.clone(),
            revision: self.revision.clone(),
            sources,
            modded: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscRecord {
    pub index: u8,
    pub serial: String,
    pub volume_label: String,
    /// Files present in the library from this disc (new or already stored).
    pub files_imported: u64,
    pub bytes_imported: u64,
    /// XA/STR stream files (Mode 2 Form 2) that were not imported.
    pub skipped_xa: u64,
    /// Entries skipped for any other reason (unsafe names, extents outside the image, ...).
    pub skipped_other: u64,
    pub imported_at: u64,
    /// File name only (no directories) of the image that was imported.
    pub image_file: String,
    /// True when the disc number was assumed (serial unknown and no --disc given).
    pub index_assumed: bool,
    /// False while the disc is being copied, and for a disc whose import stopped half way: the
    /// library may already hold some of its files, so the record must exist (it reserves the
    /// disc number for this serial) but the disc does not count as imported. Files written by
    /// older versions have no such field and are complete.
    #[serde(default = "default_true")]
    pub complete: bool,
}

impl DiscRecord {
    /// The record written BEFORE a disc's files first reach the library manifest.
    pub fn in_progress(
        index: u8,
        serial: &str,
        volume_label: &str,
        image_file: &str,
        index_assumed: bool,
        now: u64,
    ) -> DiscRecord {
        DiscRecord {
            index,
            serial: serial.to_string(),
            volume_label: volume_label.to_string(),
            files_imported: 0,
            bytes_imported: 0,
            skipped_xa: 0,
            skipped_other: 0,
            imported_at: now,
            image_file: image_file.to_string(),
            index_assumed,
            complete: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub format_version: u32,
    pub name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub family: Family,
    pub set_id: String,
    pub expected_discs: u8,
    pub discs: Vec<DiscRecord>,
}

impl Session {
    pub fn new(name: &str, family: Family, now: u64) -> Session {
        let set_id = set_id_for(&family.provenance(Vec::new()));
        Session {
            format_version: SESSION_FORMAT_VERSION,
            name: name.to_string(),
            created_at: now,
            updated_at: now,
            family,
            set_id,
            expected_discs: EXPECTED_DISCS,
            discs: Vec::new(),
        }
    }

    pub fn disc(&self, index: u8) -> Option<&DiscRecord> {
        self.discs.iter().find(|d| d.index == index)
    }

    /// Insert or replace the record for `rec.index`; records stay ordered by disc number.
    pub fn upsert_disc(&mut self, rec: DiscRecord) {
        self.discs.retain(|d| d.index != rec.index);
        self.discs.push(rec);
        self.discs.sort_by_key(|d| d.index);
    }

    pub fn missing_discs(&self) -> Vec<u8> {
        (1..=self.expected_discs)
            .filter(|i| self.disc(*i).is_none())
            .collect()
    }

    /// Discs that were imported with entries skipped for reasons other than XA stream data.
    pub fn needs_attention(&self) -> Vec<u8> {
        self.discs
            .iter()
            .filter(|d| d.skipped_other > 0)
            .map(|d| d.index)
            .collect()
    }

    /// Discs whose import started but did not finish (see [`DiscRecord::complete`]).
    pub fn incomplete_discs(&self) -> Vec<u8> {
        self.discs
            .iter()
            .filter(|d| !d.complete)
            .map(|d| d.index)
            .collect()
    }

    /// Every expected disc is imported, none half-done, none with unexplained skips. XA/STR
    /// stream files are deliberately not part of this: every real disc has some (FMV), and the
    /// field and battle data the milestones need is not among them. See [`Session::all_files_copied`].
    pub fn is_complete(&self) -> bool {
        self.missing_discs().is_empty()
            && self.needs_attention().is_empty()
            && self.incomplete_discs().is_empty()
    }

    /// XA/STR stream files (Mode 2 Form 2) left out, summed over all discs.
    pub fn skipped_xa_total(&self) -> u64 {
        self.discs
            .iter()
            .fold(0u64, |acc, d| acc.saturating_add(d.skipped_xa))
    }

    /// The stricter question: is every file of every disc in the library? False as soon as any
    /// XA/STR stream was skipped, even when [`Session::is_complete`] is true.
    pub fn all_files_copied(&self) -> bool {
        self.is_complete() && self.skipped_xa_total() == 0
    }
}

/// Exclusive hold on one session while a disc is imported into it, so that two imports into the
/// same session cannot overwrite each other's updates. Released on drop (and by the OS when the
/// process dies, however it dies).
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
    held: bool,
}

impl SessionLock {
    /// False when the filesystem has no advisory locks: nothing is held, and a second import
    /// into the same session at the same time cannot be told apart. The caller should say so.
    pub fn is_held(&self) -> bool {
        self.held
    }
}

/// Take the session's lock (creating `import-sessions/`, private, if needed). Never blocks: a
/// session that is being updated by another import is reported as [`SessionError::Busy`]. On a
/// filesystem without advisory locks it succeeds without holding anything
/// ([`SessionLock::is_held`] is then false). While the lock is held, the temp files of a session
/// writer that was killed are swept.
pub fn lock_session(state_dir: &Path, name: &str) -> Result<SessionLock, SessionError> {
    validate_session_name(name)?;
    let dir = state_dir.join(SESSIONS_DIR);
    create_private_dir_durable(&dir).map_err(|source| SessionError::Io {
        path: dir.clone(),
        source,
    })?;
    // starts with '.', which no session name may: it can never collide with `<name>.json`
    let path = dir.join(format!(".{name}.lock"));
    let file = open_lock_file(&path).map_err(|source| SessionError::Io {
        path: path.clone(),
        source,
    })?;
    match try_lock_exclusive(&file) {
        Ok(LockOutcome::Held) => {
            // nobody else writes this session now, so a temp file of it is a corpse's
            sweep_stale_temps(&dir, &format!("{name}.json"), SystemTime::now());
            Ok(SessionLock {
                _file: file,
                held: true,
            })
        }
        Ok(LockOutcome::Busy) => Err(SessionError::Busy(name.to_string())),
        Ok(LockOutcome::Unsupported) => Ok(SessionLock {
            _file: file,
            held: false,
        }),
        Err(source) => Err(SessionError::Io { path, source }),
    }
}

/// Session names become file names: no separators, no leading dot, bounded length.
pub fn validate_session_name(name: &str) -> Result<(), SessionError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(SessionError::BadName(name.chars().take(80).collect()))
    }
}

pub fn session_path(state_dir: &Path, name: &str) -> Result<PathBuf, SessionError> {
    validate_session_name(name)?;
    Ok(state_dir.join(SESSIONS_DIR).join(format!("{name}.json")))
}

/// Load a session. `Ok(None)` when no such session exists. Never writes.
pub fn load_session(state_dir: &Path, name: &str) -> Result<Option<Session>, SessionError> {
    let path = session_path(state_dir, name)?;
    let invalid = |reason: String| SessionError::Invalid {
        path: path.clone(),
        reason,
    };
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SessionError::Io {
                path: path.clone(),
                source,
            })
        }
    };
    let len = file
        .metadata()
        .map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?
        .len();
    if len > MAX_SESSION_BYTES {
        return Err(invalid("file is too large".to_string()));
    }
    let mut buf = Vec::new();
    file.take(MAX_SESSION_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
    if buf.len() as u64 > MAX_SESSION_BYTES {
        return Err(invalid("file is too large".to_string()));
    }
    let session: Session =
        serde_json::from_slice(&buf).map_err(|e| invalid(format!("cannot parse: {e}")))?;
    if session.format_version != SESSION_FORMAT_VERSION {
        return Err(invalid(format!(
            "unsupported format version {}",
            session.format_version
        )));
    }
    if session.name != name {
        return Err(invalid(
            "name inside the file does not match its file name".to_string(),
        ));
    }
    if session.expected_discs == 0 || session.expected_discs > 16 {
        return Err(invalid("expected_discs is out of range".to_string()));
    }
    if session.discs.len() > MAX_DISCS {
        return Err(invalid("too many disc records".to_string()));
    }
    let mut seen = std::collections::HashSet::new();
    for d in &session.discs {
        if d.index == 0 || d.index > 16 || !seen.insert(d.index) {
            return Err(invalid(
                "disc records have an invalid or duplicate disc number".to_string(),
            ));
        }
    }
    if validate_set_id(&session.set_id).is_err()
        || session.set_id != set_id_for(&session.family.provenance(Vec::new()))
    {
        return Err(invalid(
            "set_id does not match the session's family".to_string(),
        ));
    }
    Ok(Some(session))
}

/// Atomically write the session file (creating `import-sessions/`, private, if needed).
pub fn save_session(state_dir: &Path, session: &Session) -> Result<(), SessionError> {
    let path = session_path(state_dir, &session.name)?;
    if let Some(dir) = path.parent() {
        create_private_dir_durable(dir).map_err(|source| SessionError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    }
    let bytes = serde_json::to_vec_pretty(session).map_err(|e| SessionError::Invalid {
        path: path.clone(),
        reason: format!("cannot serialise: {e}"),
    })?;
    atomic_write(&path, &bytes).map_err(|source| SessionError::Io { path, source })
}

/// The sessions that record `set_id` as their library set, and the files in `import-sessions/`
/// that could not be read as sessions (so it is unknown what they point at), both sorted.
/// Never writes. `remove-set` uses it to find the session records a removed set leaves dangling.
pub fn sessions_using_set(state_dir: &Path, set_id: &str) -> (Vec<String>, Vec<String>) {
    let dir = state_dir.join(SESSIONS_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return (Vec::new(), Vec::new());
    };
    let (mut using, mut unreadable) = (Vec::new(), Vec::new());
    for ent in entries.flatten() {
        let file_name = ent.file_name().to_string_lossy().into_owned();
        // lock files and temp files start with '.', which no session name may
        let Some(name) = file_name.strip_suffix(".json") else {
            continue;
        };
        if validate_session_name(name).is_err() {
            continue;
        }
        match load_session(state_dir, name) {
            Ok(Some(session)) if session.set_id == set_id => using.push(name.to_string()),
            Ok(_) => {}
            Err(_) => unreadable.push(name.to_string()),
        }
    }
    using.sort();
    unreadable.sort();
    (using, unreadable)
}

/// Delete the session file `name`. `Ok(false)` when there was none. The session's lock file
/// stays (lock files are never deleted).
pub fn remove_session(state_dir: &Path, name: &str) -> Result<bool, SessionError> {
    let path = session_path(state_dir, name)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SessionError::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(region: &str) -> Family {
        Family {
            game: GameId::Ff7,
            platform: Platform::Ps1,
            edition: "PS1 original".into(),
            region: region.into(),
            language: "en".into(),
            revision: "unknown".into(),
            serial_known: true,
            region_hint: None,
            scope: None,
        }
    }

    fn disc(index: u8, serial: &str) -> DiscRecord {
        DiscRecord {
            index,
            serial: serial.into(),
            volume_label: "LABEL".into(),
            files_imported: 3,
            bytes_imported: 99,
            skipped_xa: 1,
            skipped_other: 0,
            imported_at: 5,
            image_file: "disc.cue".into(),
            index_assumed: false,
            complete: true,
        }
    }

    #[test]
    fn round_trip_and_completeness() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_session(dir.path(), "ff7").unwrap(), None);
        let mut s = Session::new("ff7", family("NTSC-U"), 10);
        s.upsert_disc(disc(2, "SCUS-94164"));
        s.upsert_disc(disc(1, "SCUS-94163"));
        assert_eq!(s.discs.iter().map(|d| d.index).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(s.missing_discs(), [3]);
        assert!(!s.is_complete());
        save_session(dir.path(), &s).unwrap();
        assert_eq!(load_session(dir.path(), "ff7").unwrap(), Some(s.clone()));
        s.upsert_disc(disc(3, "SCUS-94165"));
        assert!(s.is_complete());
        // re-importing a disc replaces its record
        let mut again = disc(3, "SCUS-94165");
        again.skipped_other = 2;
        s.upsert_disc(again);
        assert_eq!(s.discs.len(), 3);
        assert_eq!(s.needs_attention(), [3]);
        assert!(!s.is_complete());
        // no temp files left in the session dir
        let leftovers = fs::read_dir(dir.path().join(SESSIONS_DIR))
            .unwrap()
            .flatten()
            .count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn half_done_discs_block_completeness_and_xa_skips_do_not() {
        let mut s = Session::new("ff7", family("NTSC-U"), 10);
        s.upsert_disc(disc(1, "SCUS-94163"));
        s.upsert_disc(disc(2, "SCUS-94164"));
        s.upsert_disc(DiscRecord::in_progress(3, "SCUS-94165", "L", "d3.cue", false, 11));
        assert!(s.missing_discs().is_empty(), "disc 3 has a record, so it is not 'missing' ...");
        assert_eq!(s.incomplete_discs(), [3]);
        assert!(!s.is_complete(), "... but a half-done disc does not count as imported");
        assert!(!s.all_files_copied());
        // finishing it (a complete record replaces the in-progress one) completes the session
        s.upsert_disc(DiscRecord { skipped_xa: 0, ..disc(3, "SCUS-94165") });
        assert!(s.incomplete_discs().is_empty());
        assert!(s.is_complete());
        // every `disc()` above has one skipped XA stream: complete, yet not every file was copied
        assert_eq!(s.skipped_xa_total(), 2);
        assert!(s.is_complete() && !s.all_files_copied());
        for d in 1..=2 {
            s.upsert_disc(DiscRecord { skipped_xa: 0, ..disc(d, &format!("SCUS-9416{}", d + 2)) });
        }
        assert!(s.all_files_copied());
    }

    #[test]
    fn scoped_families_name_their_session_in_the_set_edition() {
        let known = family("NTSC-U");
        assert_eq!(known.set_edition(), "PS1 original");
        let unknown = Family {
            edition: "PS1 unknown".into(),
            region: "unknown".into(),
            language: "unknown".into(),
            serial_known: false,
            region_hint: Some("PAL".into()),
            scope: Some("mystery".into()),
            ..family("unknown")
        };
        assert_eq!(unknown.set_edition(), "PS1 unknown [set mystery]");
        assert_eq!(unknown.provenance(vec![]).edition, "PS1 unknown [set mystery]");
        assert_eq!(unknown.provenance(vec![]).region, "unknown", "a hint is not a region");
        assert!(unknown.label().contains("a guess: PAL"));
        // two sessions with the same discs get different library sets
        let other = Family { scope: Some("other".into()), ..unknown.clone() };
        assert!(unknown.same_family(&other), "same discs, whatever the session is called");
        assert_ne!(
            Session::new("mystery", unknown.clone(), 0).set_id,
            Session::new("other", other, 0).set_id
        );
        // the hint takes part in "same family": a PAL-looking disc cannot join an NTSC-looking set
        let ntsc = Family { region_hint: Some("NTSC-U".into()), ..unknown.clone() };
        assert!(!unknown.same_family(&ntsc));
        // the set id check on load still passes for scoped families
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("mystery", unknown, 5);
        s.upsert_disc(disc(1, "SCES-99999"));
        save_session(dir.path(), &s).unwrap();
        assert_eq!(load_session(dir.path(), "mystery").unwrap(), Some(s));
    }

    #[test]
    fn session_files_from_before_the_new_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("ff7", family("NTSC-U"), 10);
        s.upsert_disc(disc(1, "SCUS-94163"));
        save_session(dir.path(), &s).unwrap();
        let path = session_path(dir.path(), "ff7").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("region_hint") && !text.contains("scope"), "unset options are omitted");
        // an old file has no `complete` field: it means complete
        let old = text.replace("\"complete\": true,", "").replace(",\n      \"complete\": true", "");
        assert!(!old.contains("complete"), "{old}");
        fs::write(&path, old).unwrap();
        let loaded = load_session(dir.path(), "ff7").unwrap().unwrap();
        assert!(loaded.discs[0].complete);
        assert!(loaded.family.region_hint.is_none() && loaded.family.scope.is_none());
    }

    #[test]
    fn a_session_can_be_locked_by_one_import_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock_session(dir.path(), "ff7").unwrap();
        assert!(matches!(
            lock_session(dir.path(), "ff7"),
            Err(SessionError::Busy(n)) if n == "ff7"
        ));
        // other sessions are independent
        let _other = lock_session(dir.path(), "second").unwrap();
        drop(first);
        lock_session(dir.path(), "ff7").unwrap();
        assert!(lock_session(dir.path(), "../x").is_err());
        // a lock file is not mistaken for a session
        assert_eq!(load_session(dir.path(), "ff7").unwrap(), None);
    }

    #[test]
    fn names_are_validated() {
        let long_ok = "x".repeat(64);
        let too_long = "x".repeat(65);
        for ok in ["ff7", "FF7-ntsc_u.1", "a", long_ok.as_str()] {
            assert!(validate_session_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "..",
            "a/b",
            "a\\b",
            "../x",
            "a b",
            "é",
            too_long.as_str(),
            "a\0b",
            "x.json/../y",
        ] {
            assert!(validate_session_name(bad).is_err(), "{bad:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(load_session(dir.path(), "../escape").is_err());
        assert!(save_session(dir.path(), &Session::new("../escape", family("PAL"), 0)).is_err());
    }

    #[test]
    fn family_comparison_and_labels() {
        let a = family("NTSC-U");
        assert!(a.same_family(&a.clone()));
        assert!(!a.same_family(&family("PAL")));
        let mut b = a.clone();
        b.serial_known = false; // does not matter for sameness
        assert!(a.same_family(&b));
        assert!(b.label().contains("unverified"));
        assert!(!a.label().contains("unverified"));
        assert_eq!(a.provenance(vec![]).platform, Platform::Ps1);
        assert!(!a.provenance(vec![]).modded);
    }

    #[test]
    fn damaged_session_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("ff7", family("NTSC-U"), 10);
        s.upsert_disc(disc(1, "SCUS-94163"));
        save_session(dir.path(), &s).unwrap();
        let path = session_path(dir.path(), "ff7").unwrap();
        let good = fs::read_to_string(&path).unwrap();

        let dup = {
            let mut d = s.clone();
            d.discs.push(disc(1, "SCUS-94163"));
            serde_json::to_string(&d).unwrap()
        };
        let cases: Vec<(&str, String)> = vec![
            ("garbage", "{ nope".to_string()),
            ("empty", String::new()),
            (
                "wrong version",
                good.replace("\"format_version\": 1", "\"format_version\": 7"),
            ),
            (
                "wrong name",
                good.replace("\"name\": \"ff7\"", "\"name\": \"other\""),
            ),
            (
                "tampered set id",
                good.replace(&s.set_id, "ff7-ps1-other-0000000000"),
            ),
            ("traversal set id", good.replace(&s.set_id, "../../etc")),
            ("duplicate discs", dup),
            (
                "zero disc number",
                good.replace("\"index\": 1", "\"index\": 0"),
            ),
            (
                "expected 0",
                good.replace("\"expected_discs\": 3", "\"expected_discs\": 0"),
            ),
            ("oversize", " ".repeat(MAX_SESSION_BYTES as usize + 1)),
        ];
        for (name, text) in cases {
            fs::write(&path, text).unwrap();
            assert!(
                matches!(
                    load_session(dir.path(), "ff7"),
                    Err(SessionError::Invalid { .. })
                ),
                "{name}"
            );
        }
        fs::write(&path, good).unwrap();
        assert!(load_session(dir.path(), "ff7").unwrap().is_some());
    }
}
