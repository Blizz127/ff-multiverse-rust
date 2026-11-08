//! The per-set manifest: provenance plus the list of content-addressed entries.
//!
//! Manifests live on disk and are therefore treated as untrusted when loaded: size-capped,
//! parsed without panics, and validated (hashes are hex, paths are normalised, no duplicates)
//! before any value is used to build a filesystem path.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use fmv_core::{CoreError, EditionProvenance, Fingerprint, ProvenanceSummary, SourceRef};
use serde::{Deserialize, Serialize};

use crate::error::{io_err, LibraryError};
use crate::fsutil::atomic_write;
use crate::paths::{is_blake3_hex, normalize_logical_path};

pub const MANIFEST_FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
/// Hard cap on a manifest file read from disk.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
/// Hard cap on entries in one set. Enforced when reading AND when writing, so a set can never
/// grow into a state the reader would refuse.
pub const MAX_MANIFEST_ENTRIES: usize = 500_000;
/// Bounds for an [`Identification`] record (manifests are untrusted when loaded).
pub const MAX_IDENTIFICATION_METHOD: usize = 64;
pub const MAX_IDENTIFICATION_CAVEATS: usize = 16;
pub const MAX_IDENTIFICATION_TEXT: usize = 512;

/// How far the provenance labels of a set can be trusted.
///
/// `EditionProvenance` (frozen in `fmv-core`) only carries the labels themselves, with no field
/// saying whether they were checked. This record travels in the manifest next to them so that
/// a guess is never stored as if it were a fact. It is local to the library: it does not go on
/// the wire (the path-free `ProvenanceSummary` has no room for it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identification {
    /// True only when the labels were checked against known-good hashes of the real release.
    /// Nothing in this increment can do that, so every importer writes `false`.
    pub verified: bool,
    /// Short machine-readable method, e.g. `pc-file-presence` or `ps1-serial-table`.
    pub method: String,
    /// What is guessed, assumed or unchecked, in plain words. Must be deterministic for the same
    /// inputs, so that re-importing the same content does not rewrite the manifest.
    #[serde(default)]
    pub caveats: Vec<String>,
}

fn clamp_text(s: &str) -> String {
    s.chars().take(MAX_IDENTIFICATION_TEXT).collect()
}

impl Identification {
    /// An unverified identification. Texts are clamped to the stored bounds.
    pub fn unverified(method: &str, caveats: Vec<String>) -> Self {
        Identification {
            verified: false,
            method: method.chars().take(MAX_IDENTIFICATION_METHOD).collect(),
            caveats: caveats
                .iter()
                .take(MAX_IDENTIFICATION_CAVEATS)
                .map(|c| clamp_text(c))
                .collect(),
        }
    }

    /// Combine what a set already says with what a new import says: the result is verified only
    /// if both are, keeps the first method, and holds the union of the caveats (bounded).
    pub(crate) fn merged_with(&self, incoming: &Identification) -> Identification {
        let mut out = self.clone();
        out.verified = self.verified && incoming.verified;
        for c in &incoming.caveats {
            if out.caveats.len() >= MAX_IDENTIFICATION_CAVEATS {
                break;
            }
            let c = clamp_text(c);
            if !out.caveats.contains(&c) {
                out.caveats.push(c);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Normalised, `/` separated; lowercase for PC sets.
    pub logical_path: String,
    /// blake3 of the content, 64 lowercase hex characters.
    pub blake3: String,
    pub size: u64,
    pub source: SourceRef,
}

impl ManifestEntry {
    /// The content fingerprint (what the protocol exchanges instead of bytes).
    pub fn fingerprint(&self) -> Result<Fingerprint, CoreError> {
        Fingerprint::from_hex(&self.blake3)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub set_id: String,
    pub provenance: EditionProvenance,
    /// Sorted by `logical_path`.
    pub entries: Vec<ManifestEntry>,
    /// Unix seconds, supplied by the importing caller (last import that changed the set).
    pub imported_at: u64,
    /// Which importer wrote the set, e.g. `fmv-import 0.1.0 (fmv-library 0.1.0)`.
    pub importer_version: String,
    /// How far the provenance labels can be trusted. `None` for sets written by a caller that
    /// did not say (and for manifests from before this field existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identification: Option<Identification>,
}

impl Manifest {
    pub(crate) fn new(
        set_id: &str,
        provenance: &EditionProvenance,
        importer_version: &str,
        identification: Option<Identification>,
    ) -> Self {
        let mut provenance = provenance.clone();
        // The set's sources are derived from what was actually imported.
        provenance.sources.clear();
        Manifest {
            format_version: MANIFEST_FORMAT_VERSION,
            set_id: set_id.to_string(),
            provenance,
            entries: Vec::new(),
            imported_at: 0,
            importer_version: importer_version.to_string(),
            identification,
        }
    }

    /// The path-free provenance (what may be shown to other players). Install paths recorded in
    /// `provenance.sources` are left out.
    pub fn summary(&self) -> ProvenanceSummary {
        self.provenance.summary()
    }

    /// True when the labels were checked against known-good hashes (see [`Identification`]).
    /// A manifest without an identification record counts as unverified.
    pub fn is_verified(&self) -> bool {
        self.identification.as_ref().is_some_and(|i| i.verified)
    }

    /// Exact lookup by normalised logical path (entries are sorted).
    pub fn find(&self, logical_path: &str) -> Option<&ManifestEntry> {
        self.entries
            .binary_search_by(|e| e.logical_path.as_str().cmp(logical_path))
            .ok()
            .and_then(|i| self.entries.get(i))
    }

    /// ASCII-case-insensitive lookup that succeeds only when exactly one entry matches.
    pub fn find_ignore_case(&self, logical_path: &str) -> Option<&ManifestEntry> {
        let mut hits = self
            .entries
            .iter()
            .filter(|e| e.logical_path.eq_ignore_ascii_case(logical_path));
        match (hits.next(), hits.next()) {
            (Some(e), None) => Some(e),
            _ => None,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |acc, e| acc.saturating_add(e.size))
    }

    pub(crate) fn sort_entries(&mut self) {
        self.entries
            .sort_by(|a, b| a.logical_path.cmp(&b.logical_path));
    }
}

fn bad(set_id: &str, reason: impl Into<String>) -> LibraryError {
    LibraryError::BadManifest {
        set_id: set_id.to_string(),
        reason: reason.into(),
    }
}

/// Validate a freshly parsed manifest and put its entries in canonical order.
fn validate(m: &mut Manifest, set_id: &str) -> Result<(), LibraryError> {
    if m.format_version != MANIFEST_FORMAT_VERSION {
        return Err(bad(
            set_id,
            format!(
                "unsupported manifest format version {} (this build reads version {})",
                m.format_version, MANIFEST_FORMAT_VERSION
            ),
        ));
    }
    if m.set_id != set_id {
        return Err(bad(
            set_id,
            "set_id inside the manifest does not match its directory",
        ));
    }
    if m.entries.len() > MAX_MANIFEST_ENTRIES {
        return Err(bad(set_id, "too many entries"));
    }
    if let Some(i) = &m.identification {
        if i.method.chars().count() > MAX_IDENTIFICATION_METHOD
            || i.caveats.len() > MAX_IDENTIFICATION_CAVEATS
            || i.caveats
                .iter()
                .any(|c| c.chars().count() > MAX_IDENTIFICATION_TEXT)
        {
            return Err(bad(set_id, "identification record exceeds its size bounds"));
        }
    }
    for e in &m.entries {
        if !is_blake3_hex(&e.blake3) {
            return Err(bad(set_id, "entry with a malformed blake3 digest"));
        }
        let normal = normalize_logical_path(&e.logical_path, m.provenance.platform)
            .map_err(|err| bad(set_id, format!("entry with an invalid logical path: {err}")))?;
        if normal != e.logical_path {
            return Err(bad(set_id, "entry logical path is not in normalised form"));
        }
    }
    m.sort_entries();
    if m.entries.windows(2).any(|w| match w {
        [a, b] => a.logical_path == b.logical_path,
        _ => false,
    }) {
        return Err(bad(set_id, "duplicate logical path"));
    }
    Ok(())
}

/// Load and validate `path` as the manifest of `set_id`.
pub(crate) fn read_manifest(path: &Path, set_id: &str) -> Result<Manifest, LibraryError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LibraryError::SetNotFound(set_id.to_string()))
        }
        Err(e) => return Err(io_err(path)(e)),
    };
    let len = file.metadata().map_err(io_err(path))?.len();
    if len > MAX_MANIFEST_BYTES {
        return Err(bad(set_id, "manifest file is too large"));
    }
    let mut buf = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(io_err(path))?;
    if buf.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(bad(set_id, "manifest file is too large"));
    }
    let mut manifest: Manifest = serde_json::from_slice(&buf)
        .map_err(|e| bad(set_id, format!("cannot parse manifest: {e}")))?;
    validate(&mut manifest, set_id)?;
    Ok(manifest)
}

/// Serialise and atomically replace the manifest file.
pub(crate) fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), LibraryError> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| bad(&manifest.set_id, format!("cannot serialise manifest: {e}")))?;
    atomic_write(path, &bytes).map_err(io_err(path))
}
