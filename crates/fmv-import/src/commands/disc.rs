//! `import-disc` and `import-status`: sequential multi-disc import sessions.
//!
//! The order of things matters for crash and refusal safety:
//! 1. read the image and identify the disc (no writes); an image inside the library is refused
//!    here, before any lock, directory or file exists;
//! 2. PLAN against the saved session (pure): family, disc number, every refusal that needs no
//!    lock. A refused disc leaves no trace;
//! 3. take the session lock, reload the session and plan again (another import may have
//!    finished meanwhile), open the library, take the set lock and cross-check the manifest;
//! 4. copy. Before the first manifest write (every checkpoint, and the final commit) the session
//!    gets an "in progress" record for the disc, so the invariant "the library holds files of
//!    disc N  =>  the session has a record for disc N" survives a failure or a kill at any point;
//! 5. record the disc as complete.
//!
//! A disc number the library already holds is CONFIRMED, never grown or mixed: once a disc is
//! imported the set records what it held (file count, bytes and one digest over every stored
//! file's path, size and content hash, see [`fmv_library::DiscContent`]). A later image for that
//! number may only match it. A new file, a file with other bytes or a file fewer is refused as
//! "different disc content for <serial> disc N" (the first mismatch ends the import: nothing is
//! written, and a file that is only new costs one temporary copy), so a crafted image claiming
//! a genuine serial cannot get its files into the genuine disc's set, nor shut the genuine disc
//! out with an `EntryConflict` that cannot be undone. `--replace-disc` is the explicit way to
//! put another image in the number's place: the old entries leave the set only when the new
//! image has been read in full.

use std::path::{Path, PathBuf};

use fmv_core::{GameId, Platform, SourceRef};
use fmv_library::{
    resolve_state_dir, DiscContent, DiscDigest, Identification, ImportOutcome, Library,
    LibraryError, SetHandle, SourceStamp,
};
use serde::Serialize;

use super::{
    importer_version, CommandError, SourceEvidence, Totals, CHECKPOINT_EVERY, MAX_LISTED,
};
use crate::image::{open_image, Layout};
use crate::iso9660::{Form2Sector, SkippedEntry, Volume};
use crate::serial::{
    expected_serials, identify, parse_boot_serial, DiscIdentity, MAX_SYSTEM_CNF,
    SERIAL_TABLE_VERIFIED,
};
use crate::session::{
    load_session, lock_session, save_session, validate_session_name, DiscRecord, Family, Session,
};

/// How the importer came to know what a disc is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscKind {
    /// The serial is in the FF7 serial table (which is itself UNVERIFIED).
    TableMatch,
    /// The serial is not in the table: the disc is ASSUMED to be FF7 and flagged.
    UnknownSerial,
    /// The volume label starts with `SYNTHETIC`: test data, imported as FIXTURE content and
    /// never presented as FF7.
    Fixture,
}

impl DiscKind {
    fn classify(volume_label: &str, ident: &DiscIdentity) -> DiscKind {
        if volume_label
            .trim()
            .to_ascii_uppercase()
            .starts_with("SYNTHETIC")
        {
            DiscKind::Fixture
        } else if ident.serial_known {
            DiscKind::TableMatch
        } else {
            DiscKind::UnknownSerial
        }
    }

    /// Short machine-readable method name stored in the manifest's identification record.
    fn method(self) -> &'static str {
        match self {
            DiscKind::TableMatch => "ps1-serial-table",
            DiscKind::UnknownSerial => "ps1-serial-unknown",
            DiscKind::Fixture => "synthetic-volume-label",
        }
    }
}

/// The family a disc claims to belong to. For serials the table does not know and for synthetic
/// images the family is scoped to the session (see [`Family::scope`]).
fn disc_family(kind: DiscKind, ident: &DiscIdentity, revision: &str, set_name: &str) -> Family {
    match kind {
        DiscKind::TableMatch => Family {
            game: GameId::Ff7,
            platform: Platform::Ps1,
            edition: ident.edition.clone(),
            region: ident.region.clone(),
            language: ident.language.clone(),
            revision: revision.to_string(),
            serial_known: true,
            region_hint: None,
            scope: None,
        },
        // The importer is the FF7 importer, so an unknown disc is assumed to be FF7 (the
        // identification record says so). Region, edition and language stay unknown.
        DiscKind::UnknownSerial => Family {
            game: GameId::Ff7,
            platform: Platform::Ps1,
            edition: ident.edition.clone(),
            region: "unknown".to_string(),
            language: "unknown".to_string(),
            revision: revision.to_string(),
            serial_known: false,
            region_hint: ident.region_hint.clone(),
            scope: Some(set_name.to_string()),
        },
        DiscKind::Fixture => Family {
            game: GameId::Fixture,
            platform: Platform::Synthetic,
            edition: "fixture".to_string(),
            region: "none".to_string(),
            language: "none".to_string(),
            revision: revision.to_string(),
            serial_known: false,
            region_hint: None,
            scope: Some(set_name.to_string()),
        },
    }
}

/// What the manifest records about how far a disc set's labels can be trusted. Set-level and
/// deterministic: the same kind of disc always yields the same text.
fn disc_identification(kind: DiscKind) -> Identification {
    let caveats = match kind {
        DiscKind::TableMatch => vec![
            "edition, region and language come from the FF7 serial table, which is UNVERIFIED [verify]: it was written from recall and not checked against real discs or known dumps"
                .to_string(),
        ],
        DiscKind::UnknownSerial => vec![
            "the disc's serial is not in the FF7 serial table: the game is ASSUMED to be FINAL FANTASY VII, nothing confirms it"
                .to_string(),
            "edition, region and language are unknown (a region read off the serial prefix is only a guess and is not recorded as the region)"
                .to_string(),
            "this set belongs to one import session: unidentified discs are never merged across sessions"
                .to_string(),
        ],
        DiscKind::Fixture => vec![
            "the volume label starts with SYNTHETIC: this is FIXTURE test data, not a real disc and not FF7 content"
                .to_string(),
        ],
    };
    Identification::unverified(kind.method(), caveats)
}

#[derive(Debug, Clone)]
pub struct ImportDiscArgs {
    pub image: PathBuf,
    pub set_name: String,
    pub state_dir: Option<PathBuf>,
    pub disc: Option<u8>,
    pub revision: Option<String>,
    /// `--replace-disc`: the image takes the place of whatever the set (and the session) record
    /// under its disc number, even when that is other content or another serial. Without it an
    /// image that does not match what is recorded is refused.
    pub replace_disc: bool,
}

#[derive(Debug, Serialize)]
pub struct DiscSummary {
    pub index: u8,
    pub index_assumed: bool,
    pub serial: String,
    pub kind: DiscKind,
    /// The serial matched an entry of the FF7 serial table. The table is UNVERIFIED.
    pub serial_known: bool,
    pub serial_table_verified: bool,
    /// Synthetic test data, imported as FIXTURE content (never as FF7).
    pub fixture: bool,
    pub volume_label: String,
    pub layout: &'static str,
    pub image_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MissingDisc {
    pub index: u8,
    /// Serial the (unverified) table expects for this disc of the session's family.
    pub expected_serial: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub name: String,
    pub set_id: String,
    pub family: Family,
    pub serial_table_verified: bool,
    pub expected_discs: u8,
    pub imported: Vec<DiscRecord>,
    pub missing: Vec<MissingDisc>,
    /// Discs imported with entries skipped for reasons other than XA stream data.
    pub needs_attention: Vec<u8>,
    /// Discs whose import started but did not finish: the library may hold part of them.
    pub incomplete: Vec<u8>,
    /// Every expected disc is imported and none is half-done or has unexplained skips. This is
    /// deliberately about discs, not bytes: see `all_files_copied`.
    pub complete: bool,
    /// The stricter question: no XA/STR stream file was left out either. Always false for real
    /// FF7 discs in this increment, which skip Mode 2 Form 2 streams (FMV, audio).
    pub all_files_copied: bool,
    pub skipped_xa_total: u64,
    pub notes: Vec<String>,
}

fn status_of(session: &Session) -> SessionStatus {
    let table = expected_serials(
        &session.family.edition,
        &session.family.region,
        &session.family.language,
    );
    let mut notes = Vec::new();
    for index in session.incomplete_discs() {
        notes.push(format!(
            "disc {index}: its import did not finish and the library may hold part of it; run import-disc again with the same disc to finish it"
        ));
    }
    if session.skipped_xa_total() > 0 {
        notes.push(format!(
            "{} XA/STR stream file(s) (Mode 2 Form 2: FMV, audio) were NOT copied. This is a known deviation from \"copy all files\": the field and battle data the milestones need is not among them, so the set counts as complete, but it is not byte-complete (all_files_copied = false)",
            session.skipped_xa_total()
        ));
    }
    SessionStatus {
        name: session.name.clone(),
        set_id: session.set_id.clone(),
        family: session.family.clone(),
        serial_table_verified: SERIAL_TABLE_VERIFIED,
        expected_discs: session.expected_discs,
        imported: session.discs.clone(),
        missing: session
            .missing_discs()
            .into_iter()
            .map(|index| MissingDisc {
                index,
                expected_serial: table
                    .iter()
                    .find(|k| k.disc == index)
                    .map(|k| k.serial.to_string()),
            })
            .collect(),
        needs_attention: session.needs_attention(),
        incomplete: session.incomplete_discs(),
        complete: session.is_complete(),
        all_files_copied: session.all_files_copied(),
        skipped_xa_total: session.skipped_xa_total(),
        notes,
    }
}

#[derive(Debug, Serialize)]
pub struct ImportDiscResult {
    pub command: &'static str,
    pub session: String,
    pub set_id: String,
    pub state_dir: String,
    pub disc: DiscSummary,
    /// How far the set's labels can be trusted. Also stored in the manifest.
    pub identification: Identification,
    /// What this disc held, as recorded in the set: later images for this disc number are
    /// compared with it.
    pub content: DiscContent,
    /// Set when `--replace-disc` replaced a disc the set held under this number.
    pub replaced: Option<ReplacedDisc>,
    pub totals: Totals,
    /// XA/STR stream files (Mode 2 Form 2) that were NOT imported, by design.
    pub skipped_xa_count: u64,
    pub skipped_xa: Vec<String>,
    /// Anything else the importer could not take (unsafe names, extents outside the image,
    /// paths too long to store, ...).
    pub skipped_other_count: u64,
    pub skipped_other: Vec<SkippedEntry>,
    pub source_evidence: SourceEvidence,
    pub status: SessionStatus,
    pub warnings: Vec<String>,
}

fn is_form2(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Form2Sector>())
}

/// What became of one file of the disc.
#[derive(Debug)]
pub(super) enum Copied {
    Imported(ImportOutcome),
    /// A CD-XA stream (Mode 2 Form 2) cannot be read as 2048-byte sectors: skipped, reported.
    Stream,
    /// The library cannot store this name: that costs the one file, not the whole disc.
    Unstorable(SkippedEntry),
}

/// Sort the result of copying one file into "carry on" and "stop the import". Only trouble that
/// is about this one file's name or kind is carried on from. Everything else stops the import:
/// in particular a conflict with content the set already holds (`EntryConflict`), which means
/// two different discs are being mixed, and every read or write error.
pub(super) fn classify_copy(
    result: Result<ImportOutcome, LibraryError>,
    path: &str,
) -> Result<Copied, LibraryError> {
    match result {
        Ok(outcome) => Ok(Copied::Imported(outcome)),
        Err(LibraryError::SourceRead(e)) if is_form2(&e) => Ok(Copied::Stream),
        Err(LibraryError::BadLogicalPath { reason, .. }) => {
            Ok(Copied::Unstorable(SkippedEntry {
                path: path.to_string(),
                reason: format!("cannot be stored in the library: {reason}"),
            }))
        }
        Err(e) => Err(e),
    }
}

/// The outcome of planning: the session as it will be (possibly new) and the disc's number.
#[derive(Debug)]
struct Plan {
    session: Session,
    index: u8,
    index_assumed: bool,
}

/// Everything planning needs to know about the disc and the command line.
struct Facts<'a> {
    set_name: &'a str,
    kind: DiscKind,
    ident: &'a DiscIdentity,
    /// The normalised boot serial, e.g. `SCUS-94163`.
    serial: &'a str,
    /// `--disc`
    disc: Option<u8>,
    /// `--revision`
    revision: Option<&'a str>,
    /// `--replace-disc`
    replace: bool,
}

/// Decide, from the saved session alone, whether and as which disc this disc may be imported.
/// Pure: touches nothing, so every refusal here leaves no trace.
fn plan_disc(existing: Option<Session>, f: &Facts<'_>, now: u64) -> Result<Plan, CommandError> {
    let Facts {
        set_name,
        kind,
        ident,
        serial,
        disc,
        revision,
        replace,
    } = *f;
    let session = match existing {
        Some(s) => {
            if let Some(given) = revision {
                if given != s.family.revision {
                    return Err(CommandError::RevisionMismatch {
                        given: given.to_string(),
                        session: s.family.revision.clone(),
                    });
                }
            }
            let this_disc = disc_family(kind, ident, &s.family.revision, set_name);
            if !this_disc.same_family(&s.family) {
                return Err(CommandError::FamilyMismatch {
                    disc: format!("{} (serial {serial})", this_disc.label()),
                    session: s.family.label(),
                });
            }
            s
        }
        None => Session::new(
            set_name,
            disc_family(kind, ident, revision.unwrap_or("unknown"), set_name),
            now,
        ),
    };
    // the serial table only speaks for real discs
    let table_disc = (kind == DiscKind::TableMatch)
        .then_some(ident.table_disc)
        .flatten();
    let (index, index_assumed) = choose_index(&session, serial, disc, table_disc, replace)?;
    Ok(Plan {
        session,
        index,
        index_assumed,
    })
}

/// Pick the disc number and refuse contradictions: a number the table disagrees with, a number
/// already taken by another serial (unless `replace`: `--replace-disc` takes the number over),
/// and the same serial under a second number (which would let a set count as complete although
/// a disc was never imported).
fn choose_index(
    session: &Session,
    serial: &str,
    explicit: Option<u8>,
    table_disc: Option<u8>,
    replace: bool,
) -> Result<(u8, bool), CommandError> {
    let same_serial = session.discs.iter().find(|d| d.serial == serial);
    let (index, assumed) = match (explicit, table_disc) {
        (Some(d), Some(t)) if d != t => {
            return Err(CommandError::DiscContradiction {
                requested: d,
                serial: serial.to_string(),
                table: t,
            })
        }
        (Some(d), _) => (d, false),
        (None, Some(t)) => (t, false),
        // unknown serial: the same serial keeps its number, otherwise the next free one
        (None, None) => match same_serial {
            Some(d) => (d.index, d.index_assumed),
            None => (
                (1..=16u8)
                    .find(|i| session.disc(*i).is_none())
                    .ok_or(CommandError::BadDiscNumber(17))?,
                true,
            ),
        },
    };
    if let Some(d) = same_serial {
        if d.index != index {
            return Err(CommandError::SerialAlreadyImported {
                serial: serial.to_string(),
                existing: d.index,
                requested: index,
                session: format!("session {}", session.name),
            });
        }
    }
    if let Some(prev) = session.disc(index) {
        if prev.serial != serial && !replace {
            return Err(CommandError::DiscIndexTaken {
                index,
                session: format!("session {}", session.name),
                existing: prev.serial.clone(),
                incoming: serial.to_string(),
            });
        }
    }
    Ok((index, assumed))
}

/// The same two checks against what the library set itself records (its manifest's disc sources).
/// Catches a set shared with another session, or left over from an import that no session knows
/// about: it must not be possible to merge two different discs under one disc number.
pub(super) fn check_library_consistency(
    set_id: &str,
    sources: &[SourceRef],
    index: u8,
    serial: &str,
) -> Result<(), CommandError> {
    for source in sources {
        if let SourceRef::Disc {
            index: i,
            serial: recorded,
        } = source
        {
            if *i == index && recorded != serial {
                return Err(CommandError::DiscIndexTaken {
                    index,
                    session: format!("library set {set_id}"),
                    existing: recorded.clone(),
                    incoming: serial.to_string(),
                });
            }
            if recorded == serial && *i != index {
                return Err(CommandError::SerialAlreadyImported {
                    serial: serial.to_string(),
                    existing: *i,
                    requested: index,
                    session: format!("library set {set_id}"),
                });
            }
        }
    }
    Ok(())
}

/// Writes the session's "disc N is being imported" record before anything about the disc
/// reaches the library manifest (see the module docs). A no-op when the session already has a
/// record for this disc number (a re-import, or a retry of an earlier attempt).
struct Intent<'a> {
    state_dir: &'a Path,
    pending: Option<DiscRecord>,
}

impl Intent<'_> {
    fn ensure_written(&mut self, session: &mut Session, now: u64) -> Result<(), CommandError> {
        if let Some(record) = self.pending.take() {
            session.upsert_disc(record);
            session.updated_at = now;
            save_session(self.state_dir, session)?;
        }
        Ok(())
    }
}

/// How an import treats the disc number it is about to fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Nothing is recorded for the number: copy, checkpoint as usual, record what the disc held.
    Normal,
    /// The library holds a content record for the number: this image may only CONFIRM it. New
    /// files are refused, other content is refused, and nothing is written (so no checkpoints)
    /// until the whole image has been checked against the record.
    Compare,
    /// `--replace-disc`: what is recorded under the number is dropped and replaced by this image.
    /// Nothing is written until the whole image has been read (no checkpoints), so a replacement
    /// that fails halfway leaves the old disc exactly as it was.
    Replace,
}

/// What `--replace-disc` took out of the set.
#[derive(Debug, Serialize)]
pub struct ReplacedDisc {
    /// The serial the replaced disc was recorded with.
    pub serial: Option<String>,
    /// Entries removed from the set (the blobs stay in the store).
    pub entries: usize,
    pub bytes: u64,
}

/// Turn the library's refusals to add to a disc number that is already there into the plain
/// "different disc content" error.
fn disc_error(
    e: LibraryError,
    index: u8,
    serial: &str,
    prefix: &str,
    recorded: Option<&DiscContent>,
) -> CommandError {
    let shown = |p: &str| p.strip_prefix(prefix).unwrap_or(p).chars().take(80).collect::<String>();
    let about = recorded.map_or(String::new(), |r| {
        format!(" (the disc as imported: {} files, {} bytes)", r.files, r.bytes)
    });
    match &e {
        LibraryError::PrefixFrozen { logical_path, .. } => CommandError::DiscContentDiffers {
            serial: serial.to_string(),
            index,
            detail: format!(
                "this image has a file the disc as imported did not have, {}{about}",
                shown(logical_path)
            ),
        },
        LibraryError::EntryConflict {
            logical_path,
            existing,
            incoming,
            ..
        } if logical_path.starts_with(prefix) => CommandError::DiscContentDiffers {
            serial: serial.to_string(),
            index,
            detail: format!(
                "{} holds other bytes on this image than on the disc as imported (blake3 {} there, {} here){about}",
                shown(logical_path),
                existing.get(..16).unwrap_or(existing),
                incoming.get(..16).unwrap_or(incoming)
            ),
        },
        _ => CommandError::Library(e),
    }
}

pub fn import_disc(args: &ImportDiscArgs, now: u64) -> Result<ImportDiscResult, CommandError> {
    validate_session_name(&args.set_name)?;
    if let Some(d) = args.disc {
        if d == 0 || d > 16 {
            return Err(CommandError::BadDiscNumber(d));
        }
    }
    let state_dir = resolve_state_dir(args.state_dir.as_deref())?;

    // 1. read the image (read-only) and identify the disc before anything is written. Links are
    //    resolved once, here: the file that is read is the file that is guarded
    let opened = open_image(&args.image)?;
    let layout = opened.layout;
    let image_note = opened.note.clone();
    let mut guards: Vec<(PathBuf, SourceStamp)> =
        vec![(opened.data_path.clone(), opened.data_stamp.clone())];
    if let (Some(c), Some(stamp)) = (&opened.cue_path, &opened.cue_stamp) {
        guards.push((c.clone(), stamp.clone()));
    }
    // An image inside the library is refused before a lock, a directory or a file is created
    // for it (nothing needs to exist yet for that to be decided).
    for (path, _) in &guards {
        Library::ensure_source_outside_library(&state_dir, path)?;
    }
    let mut vol = Volume::open(opened.source)?;
    let walk = vol.walk()?;
    let volume_label = vol.volume_label().to_string();
    let shown_image = args.image.display().to_string();
    let boot = walk
        .files
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("SYSTEM.CNF"))
        .map(|f| vol.read_file_limited(f, MAX_SYSTEM_CNF))
        .transpose()?
        .and_then(|bytes| parse_boot_serial(&bytes))
        .ok_or_else(|| CommandError::NotPlayStation(shown_image.clone()))?;
    if walk.files.is_empty() {
        return Err(CommandError::EmptyDisc);
    }
    let ident = identify(&boot.serial);
    let kind = DiscKind::classify(&volume_label, &ident);
    let facts = Facts {
        set_name: &args.set_name,
        kind,
        ident: &ident,
        serial: &boot.serial,
        disc: args.disc,
        revision: args.revision.as_deref(),
        replace: args.replace_disc,
    };

    // 2. plan against the saved session: every refusal that needs no lock, with nothing written
    plan_disc(load_session(&state_dir, &args.set_name)?, &facts, now)?;

    // 3. lock the session, then plan again on a fresh copy: another import into the same
    //    session may have finished since the first look
    let session_lock = lock_session(&state_dir, &args.set_name)?;
    let Plan {
        mut session,
        index,
        index_assumed,
    } = plan_disc(load_session(&state_dir, &args.set_name)?, &facts, now)?;
    // `choose_index` guarantees that a record at this number belongs to this very serial (unless
    // `--replace-disc` is replacing it), so a record here means this disc was seen before (a
    // re-import, or a retry of a failed attempt)
    let had_record = session.disc(index).is_some();

    let lib = Library::open(&state_dir)?.with_importer_version(importer_version());
    for (path, _) in &guards {
        lib.ensure_not_inside_library(path)?;
    }
    let identification = disc_identification(kind);
    let handle = SetHandle::with_id(
        session.set_id.clone(),
        session.family.provenance(Vec::new()),
    )
    .with_identification(identification.clone());
    let source_ref = SourceRef::Disc {
        index,
        serial: boot.serial.clone(),
    };
    let mut batch = lib.begin_import(&handle, now)?;

    // How this import treats the number: confirm what the library holds for it, replace it
    // (`--replace-disc`), or fill it
    let prefix = format!("disc{index}/");
    let recorded = batch.disc_content(index).cloned();
    let mode = if args.replace_disc {
        Mode::Replace
    } else if recorded.is_some() {
        Mode::Compare
    } else {
        Mode::Normal
    };
    let mut warnings = Vec::new();
    let mut replaced = None;
    match mode {
        Mode::Replace => {
            let dropped = batch.drop_disc(index, &prefix, Some(&source_ref));
            if dropped.entries > 0 || dropped.serial.is_some() {
                warnings.push(format!(
                    "--replace-disc: disc {index} as imported ({} files, {} bytes{}) was replaced by this image; its files stay in the blob store but are no longer part of the set",
                    dropped.entries,
                    dropped.bytes,
                    dropped
                        .serial
                        .as_deref()
                        .map_or(String::new(), |s| format!(", serial {s}"))
                ));
                replaced = Some(ReplacedDisc {
                    serial: dropped.serial,
                    entries: dropped.entries,
                    bytes: dropped.bytes,
                });
            } else {
                warnings.push(format!(
                    "--replace-disc: nothing was recorded as disc {index} of this set, so there was nothing to replace; the disc was imported as usual"
                ));
            }
        }
        Mode::Compare => batch.freeze_prefix(prefix.clone()),
        Mode::Normal => {}
    }
    check_library_consistency(
        &session.set_id,
        &batch.manifest().provenance.sources,
        index,
        &boot.serial,
    )?;

    // 4. copy every file into the library set
    let image_file = args
        .image
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut intent = Intent {
        state_dir: &state_dir,
        pending: (!had_record || args.replace_disc).then(|| {
            DiscRecord::in_progress(
                index,
                &boot.serial,
                &volume_label,
                &image_file,
                index_assumed,
                now,
            )
        }),
    };
    let mut totals = Totals::default();
    let mut digest = DiscDigest::new();
    let mut skipped_xa: Vec<String> = Vec::new();
    let mut unstorable: Vec<SkippedEntry> = Vec::new();
    // Checkpoints write the manifest before the disc is known to be right: only a disc that is
    // being FILLED does that. Confirming or replacing one writes once, at the very end.
    let checkpoints = mode == Mode::Normal;
    for (n, file) in walk.files.iter().enumerate() {
        let logical = format!("disc{index}/{}", file.path);
        let mut reader = vol.file_reader(file)?;
        let copied = batch.add_reader(
            &logical,
            &mut reader,
            Some(u64::from(file.size)),
            source_ref.clone(),
        );
        let copied = classify_copy(copied, &file.path)
            .map_err(|e| disc_error(e, index, &boot.serial, &prefix, recorded.as_ref()))?;
        match copied {
            Copied::Imported(outcome) => {
                digest.add(&outcome.logical_path, outcome.size, &outcome.blake3);
                totals.add(&outcome);
            }
            Copied::Stream => skipped_xa.push(file.path.clone()),
            Copied::Unstorable(entry) => unstorable.push(entry),
        }
        if checkpoints && (n + 1) % CHECKPOINT_EVERY == 0 {
            for (path, stamp) in &guards {
                stamp.verify(path)?;
            }
            intent.ensure_written(&mut session, now)?;
            batch.checkpoint()?;
        }
    }
    for (path, stamp) in &guards {
        stamp.verify(path)?;
    }
    let content = digest.finish(index, &boot.serial);
    match (&mode, &recorded) {
        (Mode::Compare, Some(rec)) => {
            // every file of this image matched what the library holds, and none was new; what
            // can still differ is that the image has FEWER files than the disc as imported
            if rec.blake3 != content.blake3 {
                return Err(CommandError::DiscContentDiffers {
                    serial: boot.serial.clone(),
                    index,
                    detail: format!(
                        "this image has {} files ({} bytes), all of them as imported, but the disc as imported had {} files ({} bytes): files are missing from this image",
                        content.files, content.bytes, rec.files, rec.bytes
                    ),
                });
            }
        }
        _ => batch.record_disc(content.clone())?,
    }
    intent.ensure_written(&mut session, now)?;
    let batch_locked = batch.is_locked();
    batch.commit()?;
    // the crash window: the manifest holds the disc, the session says "complete" nowhere yet
    #[cfg(test)]
    seam::after_commit(&state_dir);

    // 5. record the disc as imported
    let mut skipped_other = walk.skipped;
    skipped_other.extend(unstorable);
    let skipped_other_count = skipped_other.len() as u64;
    session.upsert_disc(DiscRecord {
        index,
        serial: boot.serial.clone(),
        volume_label: volume_label.clone(),
        files_imported: totals.files,
        bytes_imported: totals.bytes,
        skipped_xa: skipped_xa.len() as u64,
        skipped_other: skipped_other_count,
        imported_at: now,
        image_file,
        index_assumed,
        complete: true,
    });
    session.updated_at = now;
    save_session(&state_dir, &session)?;

    match kind {
        DiscKind::TableMatch => warnings.push("the FF7 serial table is UNVERIFIED [verify]: the match above is a guess until checked against the real disc".to_string()),
        DiscKind::UnknownSerial => {
            warnings.push(format!(
                "serial {} is not in the FF7 serial table: UNVERIFIED. The disc is ASSUMED to be FINAL FANTASY VII; edition, region and language are recorded as unknown{}. The set is private to session {:?}",
                boot.serial,
                match &session.family.region_hint {
                    Some(h) => format!(" (the serial prefix suggests {h}, only a guess)"),
                    None => String::new(),
                },
                session.name
            ));
            if session.discs.len() > 1 {
                warnings.push(
                    "nothing can confirm that this disc belongs with the others of the session: the importer trusts --set-name".to_string(),
                );
            }
        }
        DiscKind::Fixture => warnings.push(format!(
            "the volume label {volume_label:?} marks this image as SYNTHETIC test data: it was imported as FIXTURE content, NOT as FINAL FANTASY VII"
        )),
    }
    if index_assumed {
        warnings.push(format!(
            "disc number {index} was assumed; pass --disc N to set it explicitly"
        ));
    }
    if skipped_other_count > 0 {
        warnings.push(format!(
            "{skipped_other_count} entries could not be imported (see skipped_other); this disc is marked as needing attention"
        ));
    }
    if !skipped_xa.is_empty() {
        warnings.push(format!(
            "{} XA/STR stream files (Mode 2 Form 2) were NOT copied: they cannot be read as 2048-byte sectors. This is a known deviation from \"copy all files\" (FMV and audio are not needed for M1/M2)",
            skipped_xa.len()
        ));
    }
    if matches!(layout, Layout::Cooked2048) {
        warnings.push("the image has plain 2048-byte sectors, so XA stream files could not be detected and were copied as-is".to_string());
    }
    if lib.stale_parts_removed() > 0 {
        warnings.push(format!(
            "removed {} leftover temporary file(s) of an interrupted earlier import from the library",
            lib.stale_parts_removed()
        ));
    }
    if totals.blobs_repaired > 0 {
        warnings.push(format!(
            "{} damaged blob(s) already in the library were replaced from this disc",
            totals.blobs_repaired
        ));
    }
    if !session_lock.is_held() || !batch_locked {
        warnings.push("this filesystem does not support advisory file locks, so nothing could stop a second import into the same session or set from running at the same time: do not run two imports at once (a collision can lose entries, which importing again restores)".to_string());
    }
    let skipped_xa_count = skipped_xa.len() as u64;
    skipped_xa.truncate(MAX_LISTED);
    skipped_other.truncate(MAX_LISTED);

    Ok(ImportDiscResult {
        command: "import-disc",
        session: session.name.clone(),
        set_id: session.set_id.clone(),
        state_dir: state_dir.display().to_string(),
        disc: DiscSummary {
            index,
            index_assumed,
            serial: boot.serial,
            kind,
            // a synthetic image can carry a table serial; it is not a table match for all that
            serial_known: kind == DiscKind::TableMatch,
            serial_table_verified: SERIAL_TABLE_VERIFIED,
            fixture: kind == DiscKind::Fixture,
            volume_label,
            layout: layout.label(),
            image_note,
        },
        identification,
        content,
        replaced,
        totals,
        skipped_xa_count,
        skipped_xa,
        skipped_other_count,
        skipped_other,
        source_evidence: SourceEvidence {
            method: "size, mtime and inode of the image (and cue sheet), taken from the files as opened, compared before and after the copy; disc images are not re-read",
            files_checked: guards.len() as u64,
            stamps_unchanged: true,
            content_rehash_unchanged: None,
        },
        status: status_of(&session),
        warnings,
    })
}

/// Report on a session (3 discs expected for FF7). Never writes.
pub fn import_status(name: &str, state_dir: Option<&Path>) -> Result<SessionStatus, CommandError> {
    validate_session_name(name)?;
    let state_dir = resolve_state_dir(state_dir)?;
    let session = load_session(&state_dir, name)?
        .ok_or_else(|| CommandError::NoSuchSession(name.to_string()))?;
    Ok(status_of(&session))
}

/// Test seam: lets a test observe the disk at the moment between the manifest commit and the
/// final session record (where a kill would leave a disc in the library but not in the session).
#[cfg(test)]
pub(super) mod seam {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn Fn(&Path)>;

    thread_local! {
        pub(in crate::commands) static AFTER_COMMIT: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn after_commit(state_dir: &Path) {
        AFTER_COMMIT.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f(state_dir);
            }
        });
    }
}
