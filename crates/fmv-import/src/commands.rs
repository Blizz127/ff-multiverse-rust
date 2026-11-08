//! The import commands. Each returns a serialisable result; the CLI prints it as JSON.
//!
//! Safety rules enforced here (on top of those in `fmv-library`):
//! - sources (installs, disc images, cue sheets) are opened read-only and re-checked after the
//!   import: size, mtime and identity must be unchanged (and, for PC installs, a second read of
//!   every file must hash to what the copy saw);
//! - the state directory must not live inside a PC install being imported, however the path is
//!   spelled (`..` after a symlink, a symlinked `library`, ...);
//! - `scan-pc`, `list` and `import-status` never create or modify anything;
//! - every refusal that can be decided from the source and the saved sessions alone (wrong
//!   edition, ambiguous language, disc number clashes, ...) happens BEFORE anything is written;
//! - one library set holds one language (PC) or one disc family (discs); content the importer
//!   cannot identify gets a set of its own, private to the import session;
//! - what the importer only GUESSED (edition, region, language, the game itself for an unknown
//!   serial) is stored as unverified in the manifest, never as a fact.
//!
//! The commands live in submodules: [`pc`] (`import-pc`), [`disc`] (`import-disc`,
//! `import-status`) and [`list`] (`list`).

use std::path::PathBuf;

use fmv_core::SourceRef;
use fmv_library::{EntryStatus, ImportOutcome, LibraryError, LIBRARY_VERSION};
use serde::Serialize;

use crate::image::ImageError;
use crate::iso9660::IsoError;
use crate::scan::{LanguageError, ScanError};
use crate::session::SessionError;

mod disc;
mod list;
mod pc;
#[cfg(test)]
mod tests;

pub use disc::{
    import_disc, import_status, DiscKind, DiscSummary, ImportDiscArgs, ImportDiscResult,
    MissingDisc, SessionStatus,
};
pub use list::{list_sets, ListResult, SetListing};
pub use pc::{import_pc, ImportPcArgs, ImportPcResult};

/// How many files an import processes between manifest checkpoints.
const CHECKPOINT_EVERY: usize = 200;
/// Most names listed in a result for skipped entries (counts are always exact).
const MAX_LISTED: usize = 25;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Language(#[from] LanguageError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Iso(#[from] IsoError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(
        "refusing to import: no flevel.lgp was found in {0} (run scan-pc to see what is there)"
    )]
    NoFlevel(String),
    #[error("refusing to import language {language:?}: the install has no flevel.lgp for it (it has one for: {have}); run scan-pc to see what is there")]
    NoFlevelForLanguage { language: String, have: String },
    #[error("the state directory {state} is inside the install directory {install}; refusing to write there (originals are never modified)")]
    StateInsideSource { state: PathBuf, install: PathBuf },
    #[error("disc belongs to {disc}, session is {session}")]
    FamilyMismatch { disc: String, session: String },
    #[error("{0} does not look like a PlayStation disc: no SYSTEM.CNF with a parsable BOOT line was found")]
    NotPlayStation(String),
    #[error("--disc {requested} contradicts the serial table, which lists {serial} as disc {table} [verify]")]
    DiscContradiction {
        requested: u8,
        serial: String,
        table: u8,
    },
    /// `session` names where the clash was found, e.g. `session ff7` or `library set <id>`.
    #[error("disc {index} of {session} was already imported from serial {existing}, but this disc has serial {incoming}; pass --disc N with a free number, or use another --set-name")]
    DiscIndexTaken {
        index: u8,
        session: String,
        existing: String,
        incoming: String,
    },
    #[error("this disc (serial {serial}) is already recorded as disc {existing} of {session}, so it cannot also be disc {requested}; if disc {existing} was numbered by mistake, import under a new --set-name")]
    SerialAlreadyImported {
        serial: String,
        existing: u8,
        requested: u8,
        session: String,
    },
    #[error("invalid disc number {0}: use 1-16")]
    BadDiscNumber(u8),
    #[error("--revision {given} contradicts the session's revision {session}")]
    RevisionMismatch { given: String, session: String },
    #[error("no files could be read from the disc image")]
    EmptyDisc,
    #[error("no import session named {0:?}")]
    NoSuchSession(String),
}

fn importer_version() -> String {
    format!(
        "fmv-import {} ({})",
        env!("CARGO_PKG_VERSION"),
        LIBRARY_VERSION
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedFile {
    pub logical_path: String,
    pub blake3: String,
    pub size: u64,
    /// `added` or `unchanged` (already in the set with identical content).
    pub entry: &'static str,
    /// `created`, `deduplicated` (identical, verified content was already stored) or `repaired`
    /// (a damaged blob of that name was replaced).
    pub blob: &'static str,
}

fn imported(o: &ImportOutcome) -> ImportedFile {
    ImportedFile {
        logical_path: o.logical_path.clone(),
        blake3: o.blake3.clone(),
        size: o.size,
        entry: match o.entry {
            EntryStatus::Added => "added",
            EntryStatus::Unchanged => "unchanged",
        },
        blob: if o.blob_repaired {
            "repaired"
        } else if o.blob_created {
            "created"
        } else {
            "deduplicated"
        },
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
    pub entries_added: u64,
    pub entries_unchanged: u64,
    pub blobs_created: u64,
    pub blobs_deduplicated: u64,
    /// Damaged blobs that were replaced (counted in `blobs_created` as well).
    pub blobs_repaired: u64,
}

impl Totals {
    fn add(&mut self, o: &ImportOutcome) {
        self.files += 1;
        self.bytes += o.size;
        match o.entry {
            EntryStatus::Added => self.entries_added += 1,
            EntryStatus::Unchanged => self.entries_unchanged += 1,
        }
        if o.blob_created {
            self.blobs_created += 1;
        } else {
            self.blobs_deduplicated += 1;
        }
        if o.blob_repaired {
            self.blobs_repaired += 1;
        }
    }
}

/// A provenance source without the local path: what the CLI prints by default. Install paths
/// (which usually contain the user's name) only appear when asked for (`list --show-paths`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceSummary {
    Install {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Disc {
        index: u8,
        serial: String,
    },
    Fixture,
}

impl SourceSummary {
    pub fn from_source(source: &SourceRef, show_paths: bool) -> Self {
        match source {
            SourceRef::Install { path } => SourceSummary::Install {
                path: show_paths.then(|| path.clone()),
            },
            SourceRef::Disc { index, serial } => SourceSummary::Disc {
                index: *index,
                serial: serial.clone(),
            },
            SourceRef::Fixture => SourceSummary::Fixture,
        }
    }
}

/// Evidence that the originals were not modified by an import, and how strong it is.
#[derive(Debug, Clone, Serialize)]
pub struct SourceEvidence {
    /// What was compared, in words.
    pub method: &'static str,
    pub files_checked: u64,
    /// Size, mtime and identity of every source were equal before and after the copy.
    pub stamps_unchanged: bool,
    /// `Some(true)`: every source was read again after the copy and hashed to the blake3 the copy
    /// saw. `None`: no content re-read was done (disc images are guarded by their stamps only).
    pub content_rehash_unchanged: Option<bool>,
}
