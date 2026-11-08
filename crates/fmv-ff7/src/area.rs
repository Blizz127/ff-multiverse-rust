//! FF7 area runtime: [`Ff7Adapter`] (a `GameAdapter` over one `flevel.lgp`) and
//! [`Ff7FieldArea`] (an `AreaRuntime` over one decoded field).
//!
//! **UNVERIFIED AGAINST OWNED MEDIA.** This adapter has only ever seen synthetic data. Its
//! behaviour on a real `flevel.lgp` is exactly as unproven as the decoders it sits on (see
//! the `lgp`, `lzss`, `field` and `walk` module docs). [`crate::VERIFICATION_STATUS`] is
//! available to any surface that wants to show the verification state.
//!
//! # FIXTURE labelling
//!
//! The adapter cannot tell synthetic data from real data by looking at bytes; it trusts the
//! provenance it was built with, which is what the library recorded at import.
//! [`AreaInfo::fixture`] follows the project's own predicate (the one `fmv-import` applies to
//! library sets): it is `true` when the provenance's game is the fixture game, its platform
//! is [`Platform::Synthetic`], or any of its sources is [`SourceRef::Fixture`]. FIXTURE
//! areas get a display name that starts with `FIXTURE:` and says they are synthetic, so FF7
//! support is never implied by them. Any other provenance (PC, or an unidentified platform)
//! is presented as real FF7 content; **a synthetic archive labelled as PC media is therefore
//! reported as real**, and must not be built that way (this crate's own tests label theirs
//! `Synthetic`).
//!
//! Accepted platforms: `Pc`, `Synthetic` and `Unknown`. The container is the PC LGP layout, so
//! `Unknown` only means the importer could not name the edition; the provenance is published
//! unchanged, so `ProvenanceSummary::compatible_with` still refuses to mix it with a `Pc`
//! peer. `Ps1` is rejected: PlayStation media is a different format family.
//!
//! # The one fingerprint rule
//!
//! [`area_fingerprint`] is the only place a field's fingerprint is computed, and **both**
//! `list_areas()` and `load_area()` use it. The fingerprint is the blake3 hash of
//!
//! ```text
//! b"fmv-ff7/area-fingerprint"  ||  u32le(FINGERPRINT_RULE_VERSION)
//! || for each of [game, platform, edition, region, revision]:  u64le(len) || utf8 bytes
//! || u64le(len(entry))  ||  entry
//! ```
//!
//! where the five fields are the ones `ProvenanceSummary::compatible_with` compares (`game`
//! and `platform` as the fixed lowercase tags `ff7`/`pc` and so on) and `entry` is the
//! **stored** `flevel.lgp` entry (the LZSS stream exactly as it sits in the archive).
//!
//! * Binding the provenance means two copies from different editions, regions or revisions
//!   never share a fingerprint even if the field bytes happen to be identical, which is what
//!   the server relies on (it only compares the fingerprint when a client joins).
//!   `language` and `modded` are deliberately not mixed in (`compatible_with` ignores them), and
//!   neither are install paths.
//! * Hashing the stored bytes means the fingerprint does not depend on the (unverified) LZSS
//!   decoder, so correcting the decoder never changes it, and listing needs no decompression.
//!   A re-packed archive with identical field content has a different fingerprint: re-packed
//!   archives are modded installs and a different provenance class anyway.
//! * [`FINGERPRINT_RULE_VERSION`] changes with the rule. [`adapter_version`] names it, for
//!   build strings and logs, so a mismatch between two builds can be told apart from a
//!   mismatch between two players' media.
//!
//! # Listing
//!
//! `list_areas()` offers one [`AreaInfo`] per archive entry that has a plausible name, in key
//! order. It reads and hashes every entry once and **does not decode anything**: an entry
//! that is not a usable field is still listed, and `load_area` then reports why (a decode
//! error naming the cause). Only these entries are left out, with the reason in
//! [`Ff7Adapter::skipped_entries`]: the name list (`maplist`), entries with an empty or
//! implausible name (anything other than ASCII letters, digits, `_` and `-`, at most 20
//! characters, so a key is safe to print, log and put in a line-based READY message),
//! duplicate names (the first entry wins everywhere), entries that could not be read, and
//! entries past the index work budget.
//!
//! The first call does the I/O for the whole archive (a real `flevel.lgp` is tens of MB; not
//! measured), so wiring code should call [`Ff7Adapter::prewarm`] on a blocking thread before
//! it starts networking instead of letting the first handshake pay for it. The work is
//! bounded: a single entry is at most [`crate::lgp::MAX_ENTRY_LEN`] bytes and the whole index
//! reads at most [`MAX_INDEX_READ_BYTES`]. The index is cached, except when an entry failed
//! with an I/O error: that may be transient, so the next call reads again.
//!
//! # Movement validation
//!
//! The walkmesh uses **x/y as the ground plane and z as height** (the task's assumption;
//! which axis is up in FF7 field space is not confirmed by the cited documentation,
//! UNVERIFIED). `validate_move(from, to)` asks whether the straight segment can be walked
//! through the triangle graph: it has to stay on the mesh and may only pass from one
//! triangle to another through an edge the access table links, so blocked edges, holes and
//! disconnected or stacked layers refuse the move. See the `walk` module docs for the exact
//! rule, tolerances and how an entity standing on a wall is handled.
//!
//! * `Accepted(p)`: `p.xy` is the destination (moved inward by a few thousandths of a unit, at
//!   most 0.05, if it was on a blocked edge) and `p.z` the surface height there; the caller's
//!   `to.z` is ignored. Where several surfaces overlap, `from.z` selects the layer.
//! * `Corrected(q)`: the move cannot be made; `q` is `from` with its height snapped to the
//!   surface. If `from` is not on the mesh either (or not finite), `q` is the spawn point
//!   nearest to `from` (nearest to `to` if `from` is not finite, the first spawn if neither
//!   is).
//! * Non-finite coordinates never panic and never propagate.
//!
//! Gaps: a `from` that is off the mesh or not finite has no starting triangle, so a move
//! from it is judged by its destination alone (the server never passes one); heights are not
//! constrained beyond what the links allow; the access-table layout is UNVERIFIED (see
//! [`Ff7FieldArea::link_report`] for a check to run on real data).
//!
//! # Speed
//!
//! [`FF7_FIELD_MAX_SPEED_PLACEHOLDER`] is a PLACEHOLDER of the same order of magnitude as the
//! FIXTURE adapter's value; it is not derived from FF7 data and must be replaced once field
//! walk/run speed is verified on owned media.

use crate::field::{Background, Camera, FieldError, FieldFile, Palette, Triggers, Vertex, Walkmesh};
use crate::lgp::{LgpArchive, LgpError};
use crate::lzss;
use crate::walk::{LinkReport, Route, Surface};
use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, Bounds, GameAdapter, MoveResult};
use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, Platform, Pos, ProvenanceSummary, SourceRef};
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// Cap on a decompressed field file (16 MiB). The largest part of a PC field is its
/// background (at most 42 texture pages of at most 128 KiB, about 5 MiB) and the largest
/// real fields are a few MiB, so this is generous without letting a hostile entry cost much.
pub const MAX_FIELD_BYTES: usize = 16 * 1024 * 1024;

/// Cap on the total entry bytes read while building the area index (512 MiB; a real
/// `flevel.lgp` is tens of MB). Entries past it are skipped, so a crafted archive whose
/// entries all point at the same large body cannot make `list_areas` run for long.
pub const MAX_INDEX_READ_BYTES: u64 = 512 * 1024 * 1024;

/// PLACEHOLDER maximum movement speed in walkmesh units per second. Not derived from FF7
/// data; replace once the field walk/run speed is verified on owned media.
pub const FF7_FIELD_MAX_SPEED_PLACEHOLDER: f32 = 600.0;

/// Version of the fingerprint rule (see the module docs). Bumped whenever the rule changes.
pub const FINGERPRINT_RULE_VERSION: u32 = 1;

/// Spawn points offered per area.
const MAX_SPAWNS: usize = 4;
/// The field name list entry in `flevel.lgp`; not a field.
const MAPLIST_NAME: &str = "maplist";
/// Longest accepted area key: the width of an LGP name field.
const MAX_KEY_LEN: usize = 20;
/// Domain separator of the fingerprint rule.
const FINGERPRINT_DOMAIN: &[u8] = b"fmv-ff7/area-fingerprint";

fn game_tag(game: GameId) -> &'static str {
    match game {
        GameId::Ff7 => "ff7",
        GameId::Ff8 => "ff8",
        GameId::Ff9 => "ff9",
        GameId::Ff10 => "ff10",
        GameId::Ff12 => "ff12",
        GameId::Fixture => "fixture",
    }
}

fn platform_tag(platform: Platform) -> &'static str {
    match platform {
        Platform::Pc => "pc",
        Platform::Ps1 => "ps1",
        Platform::Synthetic => "synthetic",
        Platform::Unknown => "unknown",
    }
}

/// THE fingerprint rule for FF7 areas (see the module docs for the exact recipe): a blake3
/// hash of the compatibility-relevant provenance (game, platform, edition, region, revision)
/// and the stored `flevel.lgp` entry. Used by `list_areas`, `load_area` and every other path
/// in this crate; server and client must not compute it any other way.
pub fn area_fingerprint(provenance: &ProvenanceSummary, stored_entry: &[u8]) -> Fingerprint {
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(&FINGERPRINT_RULE_VERSION.to_le_bytes());
    let fields = [
        game_tag(provenance.game),
        platform_tag(provenance.platform),
        provenance.edition.as_str(),
        provenance.region.as_str(),
        provenance.revision.as_str(),
    ];
    for field in fields {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.update(&(stored_entry.len() as u64).to_le_bytes());
    hasher.update(stored_entry);
    Fingerprint(*hasher.finalize().as_bytes())
}

/// The project's FIXTURE predicate (the one `fmv-import` applies to library sets): synthetic
/// content is never presented as real FF7.
fn is_fixture_provenance(p: &EditionProvenance) -> bool {
    p.game.is_fixture() || p.platform == Platform::Synthetic || p.sources.iter().any(|s| matches!(s, SourceRef::Fixture))
}

/// Why `name` cannot be an area key, if it cannot. Real field names are lowercase letters,
/// digits and `_`; the rule is wider than that (and narrower than "printable") so that a key
/// can be printed, logged and put into a whitespace-separated line without escaping.
fn check_key(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("the name is empty");
    }
    if name.len() > MAX_KEY_LEN {
        return Err("the name is longer than an LGP name field");
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("the name has characters other than ASCII letters, digits, '_' and '-'");
    }
    Ok(())
}

/// Identifies this adapter build (crate version and fingerprint rule version) for build
/// strings and logs. Two builds with different rule versions compute different fingerprints
/// for identical media; folding this into `client_build`/`server_build` lets a mismatch be
/// attributed to the builds instead of to the players' content.
pub fn adapter_version() -> String {
    format!("fmv-ff7/{} fingerprint-rule/{FINGERPRINT_RULE_VERSION}", env!("CARGO_PKG_VERSION"))
}

/// An `flevel.lgp` entry that was not offered as an area, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

struct AreaIndex {
    areas: Vec<AreaInfo>,
    skipped: Vec<SkippedEntry>,
}

/// One entry read, decompressed, fingerprinted and validated.
struct LoadedField {
    /// Entry name exactly as stored in the archive (validated by [`check_key`]).
    key: String,
    fingerprint: Fingerprint,
    field: FieldFile,
    walkmesh: Walkmesh,
    surface: Surface,
}

/// FF7 adapter over one owned `flevel.lgp` (read-only).
pub struct Ff7Adapter {
    archive: Mutex<LgpArchive<Box<dyn ReadSeek>>>,
    provenance: EditionProvenance,
    /// `provenance.summary()`, computed once for fingerprinting.
    summary: ProvenanceSummary,
    fixture: bool,
    /// Cached area index. Lock order: `index` before `archive`, never the reverse.
    index: Mutex<Option<Arc<AreaIndex>>>,
}

impl std::fmt::Debug for Ff7Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ff7Adapter").field("edition", &self.provenance.edition).field("fixture", &self.fixture).finish()
    }
}

fn lgp_error(e: LgpError) -> AdapterError {
    match e {
        LgpError::Io(e) => AdapterError::Io(e),
        LgpError::NotFound(name) => AdapterError::AreaNotFound(name),
        other => AdapterError::Decode(format!("flevel.lgp: {other}")),
    }
}

fn field_error(key: &str, e: &FieldError) -> AdapterError {
    AdapterError::Decode(format!("field {key}: {e}"))
}

impl Ff7Adapter {
    /// Build an adapter over any seekable source of `flevel.lgp` bytes (for example a
    /// library blob opened read-only). Only the archive's table of contents is read here.
    ///
    /// `provenance` describes where the data came from and is published unchanged. It must
    /// name FF7 and must not be PlayStation media (the container is the PC LGP layout).
    /// `Pc`, `Synthetic` and `Unknown` platforms are accepted: `Unknown` because the layout
    /// itself shows the data is a PC archive (peers with a `Pc` provenance still refuse it,
    /// since `compatible_with` compares platforms), and `Synthetic` so that synthetic
    /// FF7-format data can be hosted, flagged FIXTURE (see the module docs).
    pub fn from_flevel_reader<R>(reader: R, provenance: EditionProvenance) -> Result<Self, AdapterError>
    where
        R: Read + Seek + Send + 'static,
    {
        if provenance.game != GameId::Ff7 {
            return Err(AdapterError::Unsupported(format!(
                "provenance is for {:?}; this adapter reads FF7 data",
                provenance.game
            )));
        }
        match provenance.platform {
            Platform::Ps1 => {
                return Err(AdapterError::Unsupported(
                    "provenance platform is Ps1; this adapter decodes the PC flevel.lgp layout only".into(),
                ))
            }
            Platform::Pc | Platform::Synthetic | Platform::Unknown => {}
        }
        let reader: Box<dyn ReadSeek> = Box::new(reader);
        let archive = LgpArchive::open(reader).map_err(lgp_error)?;
        let fixture = is_fixture_provenance(&provenance);
        let summary = provenance.summary();
        Ok(Self { archive: Mutex::new(archive), provenance, summary, fixture, index: Mutex::new(None) })
    }

    /// Open `flevel.lgp` at `path` **read-only** and build an adapter over it.
    pub fn from_flevel_path(path: impl AsRef<Path>, provenance: EditionProvenance) -> Result<Self, AdapterError> {
        let file = std::fs::File::open(path.as_ref()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                // No path in the message: it is a local path and the caller already holds it.
                AdapterError::MissingContent("the FF7 field archive (flevel.lgp) does not exist at the given location".into())
            } else {
                AdapterError::Io(e)
            }
        })?;
        Self::from_flevel_reader(file, provenance)
    }

    /// Provenance this adapter was built with.
    pub fn provenance(&self) -> &EditionProvenance {
        &self.provenance
    }

    /// Whether every area of this adapter is labelled FIXTURE (see the module docs).
    pub fn is_fixture(&self) -> bool {
        self.fixture
    }

    /// Build the area index now, blocking until it is done, and return how many areas it
    /// offers. Call this from a blocking thread at startup so the first `list_areas` (which
    /// reads the whole archive, see the module docs) does not stall a handshake.
    pub fn prewarm(&self) -> usize {
        self.index().areas.len()
    }

    /// Entries of the archive that `list_areas` did not offer, with the reason. Builds the
    /// area index on first use (see the module docs).
    pub fn skipped_entries(&self) -> Vec<SkippedEntry> {
        self.index().skipped.clone()
    }

    /// Load a field as the concrete [`Ff7FieldArea`], which, unlike the `AreaRuntime` trait
    /// object, also exposes camera, palette, triggers and background data for rendering.
    pub fn load_field(&self, key: &str) -> Result<Ff7FieldArea, AdapterError> {
        let loaded = self.load_validated(key)?;
        let info = self.info_for(&loaded.key, loaded.fingerprint);
        Ok(Ff7FieldArea::new(info, loaded.field, loaded.walkmesh, loaded.surface))
    }

    fn info_for(&self, key: &str, fingerprint: Fingerprint) -> AreaInfo {
        let display_name = if self.fixture {
            format!("FIXTURE: FF7-format field {key} (synthetic, not FF7 content)")
        } else {
            format!("FF7 field {key}")
        };
        AreaInfo {
            id: AreaId { game: GameId::Ff7, key: key.to_string() },
            display_name,
            fingerprint,
            provenance: self.provenance.clone(),
            fixture: self.fixture,
        }
    }

    /// Look up `name`, check that the archive's spelling is a usable key, and read the stored
    /// entry. Returns the key exactly as the archive spells it.
    fn read_entry(&self, name: &str) -> Result<(String, Vec<u8>), AdapterError> {
        let mut archive = self.archive.lock().unwrap_or_else(PoisonError::into_inner);
        let key = archive
            .find(name)
            .map(|e| e.name.clone())
            .ok_or_else(|| AdapterError::AreaNotFound(name.to_string()))?;
        if let Err(why) = check_key(&key) {
            return Err(AdapterError::Unsupported(format!("archive entry {key:?} is not offered as an area: {why}")));
        }
        let raw = archive.read(&key).map_err(lgp_error)?;
        Ok((key, raw))
    }

    /// Read, fingerprint, decompress and validate one entry. The single code path behind
    /// `load_area`; its fingerprint is the one `list_areas` reports.
    fn load_validated(&self, name: &str) -> Result<LoadedField, AdapterError> {
        let (key, raw) = self.read_entry(name)?;
        let fingerprint = area_fingerprint(&self.summary, &raw);
        let decompressed = lzss::decompress_with_limit(&raw, MAX_FIELD_BYTES)
            .map_err(|e| AdapterError::Decode(format!("field {key}: {e}")))?;
        drop(raw);
        let field = FieldFile::parse(decompressed).map_err(|e| field_error(&key, &e))?;
        let walkmesh = field.walkmesh().map_err(|e| field_error(&key, &e))?;
        if walkmesh.is_empty() {
            return Err(AdapterError::Decode(format!("field {key}: walkmesh has no triangles")));
        }
        let surface = Surface::new(&walkmesh);
        if surface.usable_count() == 0 {
            return Err(AdapterError::Decode(format!(
                "field {key}: walkmesh has no usable (non-degenerate) triangles"
            )));
        }
        Ok(LoadedField { key, fingerprint, field, walkmesh, surface })
    }

    fn index(&self) -> Arc<AreaIndex> {
        let mut slot = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = slot.as_ref() {
            return Arc::clone(index);
        }
        let (index, cacheable) = self.build_index(MAX_INDEX_READ_BYTES);
        let index = Arc::new(index);
        if cacheable {
            *slot = Some(Arc::clone(&index));
        }
        index
    }

    /// Read and fingerprint every plausibly named entry, stopping once `budget` entry bytes
    /// have been read. The flag is `false` if some entry failed with an I/O error (possibly
    /// transient), in which case the result must not be cached.
    fn build_index(&self, budget: u64) -> (AreaIndex, bool) {
        let names: Vec<(usize, String)> = {
            let archive = self.archive.lock().unwrap_or_else(PoisonError::into_inner);
            archive.list().iter().enumerate().map(|(i, e)| (i, e.name.clone())).collect()
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut areas = Vec::new();
        let mut skipped = Vec::new();
        let mut read_bytes: u64 = 0;
        let mut cacheable = true;
        for (index, name) in names {
            let lower = name.to_ascii_lowercase();
            let mut skip = |reason: String| skipped.push(SkippedEntry { name: name.clone(), reason });
            if name.is_empty() {
                skip("entry has an empty name".into());
                continue;
            }
            if !seen.insert(lower.clone()) {
                skip("duplicate entry name (the first one is used)".into());
                continue;
            }
            if lower == MAPLIST_NAME {
                skip("not a field file (the field name list)".into());
                continue;
            }
            if let Err(why) = check_key(&name) {
                skip(format!("not offered as an area: {why}"));
                continue;
            }
            if read_bytes >= budget {
                skip(format!("index work budget of {budget} bytes exhausted before this entry"));
                continue;
            }
            let raw = {
                let mut archive = self.archive.lock().unwrap_or_else(PoisonError::into_inner);
                archive.read_index(index)
            };
            match raw {
                Ok(raw) => {
                    read_bytes = read_bytes.saturating_add(raw.len() as u64);
                    areas.push(self.info_for(&name, area_fingerprint(&self.summary, &raw)));
                }
                Err(LgpError::Io(e)) => {
                    cacheable = false;
                    skip(format!("i/o error while reading the entry: {e}"));
                }
                Err(other) => skip(lgp_error(other).to_string()),
            }
        }
        areas.sort_by(|a, b| a.id.key.cmp(&b.id.key));
        (AreaIndex { areas, skipped }, cacheable)
    }
}

impl GameAdapter for Ff7Adapter {
    fn game(&self) -> GameId {
        GameId::Ff7
    }

    fn list_areas(&self) -> Vec<AreaInfo> {
        self.index().areas.clone()
    }

    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        Ok(Box::new(self.load_field(key)?))
    }
}

fn finite_xy(p: Pos) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn pos(x: f64, y: f64, z: f64) -> Pos {
    Pos::new(x as f32, y as f32, z as f32)
}

/// One decoded FF7 field as an `AreaRuntime`. Owns the decompressed field file and the
/// parsed walkmesh; nothing here is ever sent over the network.
pub struct Ff7FieldArea {
    info: AreaInfo,
    field: FieldFile,
    walkmesh: Walkmesh,
    surface: Surface,
    bounds: Bounds,
    spawns: Vec<Pos>,
}

impl std::fmt::Debug for Ff7FieldArea {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ff7FieldArea").field("key", &self.info.id.key).field("triangles", &self.walkmesh.len()).finish()
    }
}

impl Ff7FieldArea {
    /// `surface` must have at least one usable triangle (checked by
    /// `Ff7Adapter::load_validated`, the only caller).
    fn new(info: AreaInfo, field: FieldFile, walkmesh: Walkmesh, surface: Surface) -> Self {
        // Bounds over every vertex of every triangle, including degenerate ones.
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in walkmesh.triangles.iter().flatten() {
            for (i, c) in [v.x, v.y, v.z].into_iter().enumerate() {
                let c = f32::from(c);
                if let (Some(lo), Some(hi)) = (min.get_mut(i), max.get_mut(i)) {
                    *lo = lo.min(c);
                    *hi = hi.max(c);
                }
            }
        }
        let bounds = Bounds { min: Pos::new(min[0], min[1], min[2]), max: Pos::new(max[0], max[1], max[2]) };
        // Everyone who spawns must be able to reach everyone else: the walk module picks them
        // from the largest connected region only.
        let spawns = surface.spawn_points(MAX_SPAWNS);
        Self { info, field, walkmesh, surface, bounds, spawns }
    }

    /// The decoded field file (all sections, raw).
    pub fn field_file(&self) -> &FieldFile {
        &self.field
    }

    /// The parsed walkmesh (triangles and edge-access data).
    pub fn walkmesh(&self) -> &Walkmesh {
        &self.walkmesh
    }

    /// How well the walkmesh's access words match the layout this crate assumes (symmetric,
    /// geometrically adjacent links). Meant to be checked once on real media: the layout is
    /// UNVERIFIED, and `links == symmetric == adjacent` on a real field supports it.
    pub fn link_report(&self) -> LinkReport {
        self.surface.link_report()
    }

    /// Camera records. Parsed on demand; a damaged section does not affect the area.
    pub fn cameras(&self) -> Result<Vec<Camera>, FieldError> {
        self.field.cameras()
    }

    /// Palette pages (parsed on demand).
    pub fn palette(&self) -> Result<Palette, FieldError> {
        self.field.palette()
    }

    /// Triggers and gateways (parsed on demand).
    pub fn triggers(&self) -> Result<Triggers, FieldError> {
        self.field.triggers()
    }

    /// Background tiles and texture page locations (parsed on demand).
    pub fn background(&self) -> Result<Background<'_>, FieldError> {
        self.field.background()
    }

    /// Height of the walkmesh surface at `(x, y)`, preferring the surface closest to
    /// `hint_z` where triangles overlap. `None` if `(x, y)` is off the mesh or not finite.
    /// A pure point query: it does not look at edge links.
    pub fn height_at(&self, x: f32, y: f32, hint_z: Option<f32>) -> Option<f32> {
        if !(x.is_finite() && y.is_finite()) {
            return None;
        }
        let hint = hint_z.filter(|z| z.is_finite()).map(f64::from);
        self.surface.locate(f64::from(x), f64::from(y), hint).map(|(_, z)| z as f32)
    }

    fn nearest_spawn(&self, reference: Option<(f32, f32)>) -> Pos {
        let first = self.spawns.first().copied().unwrap_or_default();
        let Some((rx, ry)) = reference else {
            return first;
        };
        let dist2 = |p: &Pos| (f64::from(p.x) - f64::from(rx)).powi(2) + (f64::from(p.y) - f64::from(ry)).powi(2);
        self.spawns.iter().copied().min_by(|a, b| dist2(a).total_cmp(&dist2(b))).unwrap_or(first)
    }
}

impl AreaRuntime for Ff7FieldArea {
    fn info(&self) -> &AreaInfo {
        &self.info
    }

    fn spawn_points(&self) -> Vec<Pos> {
        self.spawns.clone()
    }

    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        let hint = from.z.is_finite().then(|| f64::from(from.z));
        let from_xy = finite_xy(from).then(|| (f64::from(from.x), f64::from(from.y)));
        let to_xy = finite_xy(to).then(|| (f64::from(to.x), f64::from(to.y)));

        if let (Some(f), Some(t)) = (from_xy, to_xy) {
            match self.surface.route(f, hint, t) {
                Route::Reached { x, y, z } => return MoveResult::Accepted(pos(x, y, z)),
                Route::Blocked { z } => return MoveResult::Corrected(Pos::new(from.x, from.y, z as f32)),
                Route::OffMesh => {}
            }
        }
        // Past this point `from` has no starting triangle (not finite, or off the mesh) or `to`
        // is not finite. With nothing to route from, only the destination can be checked.
        if let Some(t) = to_xy {
            if let Some((tri, _)) = self.surface.locate(t.0, t.1, hint) {
                let (x, y, z) = self.surface.settle(tri, t);
                return MoveResult::Accepted(pos(x, y, z));
            }
        }
        if let Some(f) = from_xy {
            if let Some((_, z)) = self.surface.locate(f.0, f.1, hint) {
                return MoveResult::Corrected(Pos::new(from.x, from.y, z as f32));
            }
        }
        let reference = from_xy.is_some().then_some((from.x, from.y)).or_else(|| to_xy.is_some().then_some((to.x, to.y)));
        MoveResult::Corrected(self.nearest_spawn(reference))
    }

    fn bounds(&self) -> Bounds {
        self.bounds
    }

    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        let p = |v: &Vertex| Pos::new(f32::from(v.x), f32::from(v.y), f32::from(v.z));
        self.walkmesh.triangles.iter().map(|t| [p(&t[0]), p(&t[1]), p(&t[2])]).collect()
    }

    fn max_speed(&self) -> f32 {
        FF7_FIELD_MAX_SPEED_PLACEHOLDER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lzss;
    use crate::testutil::{build_lgp, FieldSpec};
    use std::io::{self, Cursor, SeekFrom};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Synthetic data labelled the way the import pipeline labels it: FIXTURE.
    fn provenance() -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Synthetic,
            edition: "synthetic test edition".into(),
            region: "none".into(),
            language: "none".into(),
            revision: "test".into(),
            sources: vec![SourceRef::Fixture],
            modded: false,
        }
    }

    /// The same data claiming to be real PC media (the adapter has to trust that).
    fn pc_provenance() -> EditionProvenance {
        EditionProvenance { platform: Platform::Pc, sources: vec![], ..provenance() }
    }

    fn packed(spec: &FieldSpec) -> Vec<u8> {
        lzss::compress(&spec.build())
    }

    /// The compressed default field. Compressing a ~200 KiB field is the slow part of these
    /// tests; do it once.
    fn good_entry() -> &'static [u8] {
        static CACHE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| packed(&FieldSpec::default()))
    }

    /// An archive with a name list, a good field, a non-field entry, an empty-mesh field and
    /// a field whose compressed stream is cut short.
    fn archive_bytes() -> Vec<u8> {
        static CACHE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| {
                let good = good_entry();
                let empty_mesh = packed(&FieldSpec::with_mesh(vec![], vec![]));
                let mut cut = good.to_vec();
                cut.truncate(cut.len() / 2);
                build_lgp(&[
                    ("maplist", b"\x01\x00not a field"),
                    ("testfld", good),
                    ("junk", b"definitely not lzss"),
                    ("emptymesh", &empty_mesh),
                    ("cutshort", &cut),
                ])
            })
            .clone()
    }

    fn adapter() -> Ff7Adapter {
        Ff7Adapter::from_flevel_reader(Cursor::new(archive_bytes()), provenance()).unwrap()
    }

    fn area() -> Box<dyn AreaRuntime> {
        adapter().load_area("testfld").unwrap()
    }

    /// A one-field archive with a custom walkmesh (everything else is the default field).
    fn adapter_with_mesh(triangles: Vec<[(i16, i16, i16); 3]>, access: Vec<[u16; 3]>) -> Ff7Adapter {
        let entry = packed(&FieldSpec::with_mesh(triangles, access));
        let lgp = build_lgp(&[("meshfld", &entry)]);
        Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap()
    }

    #[test]
    fn adapter_and_area_are_send_sync_and_object_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Ff7Adapter>();
        assert_send_sync::<Ff7FieldArea>();
        let _boxed: Box<dyn GameAdapter> = Box::new(adapter());
    }

    // ---- listing, fingerprints, labels (F2, F3, F4, F6, F7, F8, F9, F10, F11) ----

    #[test]
    fn lists_every_plausibly_named_entry_without_decoding_any_of_them() {
        let a = adapter();
        assert_eq!(a.game(), GameId::Ff7);
        let areas = a.list_areas();
        let keys: Vec<&str> = areas.iter().map(|x| x.id.key.as_str()).collect();
        // sorted; nothing is decoded, so entries that are not usable fields are listed too
        assert_eq!(keys, ["cutshort", "emptymesh", "junk", "testfld"]);
        for info in &areas {
            assert_eq!(info.id.game, GameId::Ff7);
            assert_eq!(info.provenance, provenance());
        }
        // only the name list is left out, with its reason
        let skipped = a.skipped_entries();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "maplist");
        assert!(skipped[0].reason.contains("not a field"));
        // list_areas is cached and stable
        assert_eq!(a.list_areas().len(), 4);
        assert_eq!(a.prewarm(), 4);
        // load_area is where an unusable entry says why
        assert!(matches!(a.load_area("junk"), Err(AdapterError::Decode(m)) if m.contains("junk")));
        assert!(matches!(a.load_area("emptymesh"), Err(AdapterError::Decode(m)) if m.contains("no triangles")));
        assert!(matches!(a.load_area("cutshort"), Err(AdapterError::Decode(_))));
        assert!(a.load_area("testfld").is_ok());
    }

    #[test]
    fn list_and_load_use_the_same_fingerprint_rule() {
        let a = adapter();
        let listed = a.list_areas().into_iter().find(|x| x.id.key == "testfld").unwrap().fingerprint;
        let loaded = a.load_area("testfld").unwrap().info().fingerprint;
        let expected = area_fingerprint(&provenance().summary(), good_entry());
        assert_eq!(listed, expected);
        assert_eq!(loaded, expected);
        // an entry that cannot even be decoded still has the fingerprint of its stored bytes
        let junk = a.list_areas().into_iter().find(|x| x.id.key == "junk").unwrap().fingerprint;
        assert_eq!(junk, area_fingerprint(&provenance().summary(), b"definitely not lzss"));
    }

    #[test]
    fn the_fingerprint_rule_is_exactly_the_documented_recipe() {
        // An independent statement of the recipe in the module docs.
        let p = ProvenanceSummary {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: "PC 1998".into(),
            region: "NTSC-U".into(),
            language: "en".into(),
            revision: "1.02".into(),
            modded: false,
        };
        let entry = b"stored entry bytes";
        let mut recipe = Vec::new();
        recipe.extend_from_slice(b"fmv-ff7/area-fingerprint");
        recipe.extend_from_slice(&1u32.to_le_bytes());
        for field in ["ff7", "pc", "PC 1998", "NTSC-U", "1.02"] {
            recipe.extend_from_slice(&(field.len() as u64).to_le_bytes());
            recipe.extend_from_slice(field.as_bytes());
        }
        recipe.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        recipe.extend_from_slice(entry);
        assert_eq!(FINGERPRINT_RULE_VERSION, 1, "update this test together with the rule");
        assert_eq!(area_fingerprint(&p, entry).0, *blake3::hash(&recipe).as_bytes());
    }

    #[test]
    fn the_fingerprint_binds_exactly_what_compatible_with_compares() {
        let base = pc_provenance().summary();
        let entry = good_entry();
        let fp = |p: &ProvenanceSummary| area_fingerprint(p, entry);
        let reference = fp(&base);
        // identical bytes, different edition / region / revision / platform / game: different
        for changed in [
            ProvenanceSummary { edition: "other edition".into(), ..base.clone() },
            ProvenanceSummary { region: "PAL".into(), ..base.clone() },
            ProvenanceSummary { revision: "other".into(), ..base.clone() },
            ProvenanceSummary { platform: Platform::Unknown, ..base.clone() },
            ProvenanceSummary { platform: Platform::Synthetic, ..base.clone() },
            ProvenanceSummary { game: GameId::Fixture, ..base.clone() },
        ] {
            assert_ne!(fp(&changed), reference, "{changed:?}");
            assert!(!changed.compatible_with(&base), "every variant differs in a field compatible_with compares");
        }
        // language and modded are ignored by compatible_with, so they are ignored here too:
        // two peers that the project considers compatible must agree on the fingerprint
        let compatible = ProvenanceSummary { language: "de".into(), modded: true, ..base.clone() };
        assert!(compatible.compatible_with(&base));
        assert_eq!(fp(&compatible), reference);
        // different bytes: different fingerprint
        assert_ne!(area_fingerprint(&base, b"other"), reference);
        // field boundaries are unambiguous ("ab"+"c" is not "a"+"bc")
        let split = |a: &str, b: &str| ProvenanceSummary { edition: a.into(), region: b.into(), ..base.clone() };
        assert_ne!(fp(&split("ab", "c")), fp(&split("a", "bc")));
    }

    #[test]
    fn two_editions_with_identical_field_bytes_get_different_fingerprints() {
        // The server compares fingerprints only: the provenance has to be inside them.
        let entry = good_entry();
        let lgp = build_lgp(&[("testfld", entry)]);
        let make = |edition: &str, revision: &str| {
            let p = EditionProvenance { edition: edition.into(), revision: revision.into(), ..pc_provenance() };
            Ff7Adapter::from_flevel_reader(Cursor::new(lgp.clone()), p).unwrap().list_areas().remove(0).fingerprint
        };
        assert_eq!(make("PC 1998", "1.0"), make("PC 1998", "1.0"));
        assert_ne!(make("PC 1998", "1.0"), make("Steam 2013", "1.0"));
        assert_ne!(make("PC 1998", "1.0"), make("PC 1998", "1.1"));
    }

    #[test]
    fn the_fingerprint_covers_the_stored_bytes_not_the_decoded_ones() {
        // The same field, stored literal-only instead of with the test compressor: both decode
        // to the same field, but the fingerprint is a property of the stored entry.
        let bytes = FieldSpec::default().build();
        let mut literal_only = Vec::new();
        for chunk in bytes.chunks(8) {
            literal_only.push(0xFFu8 >> (8 - chunk.len()));
            literal_only.extend_from_slice(chunk);
        }
        let mut stream = (literal_only.len() as u32).to_le_bytes().to_vec();
        stream.extend_from_slice(&literal_only);
        let lgp = build_lgp(&[("testfld", &stream)]);
        let b = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        let loaded = b.load_field("testfld").unwrap();
        assert_eq!(loaded.field_file().bytes(), bytes.as_slice());
        assert_eq!(loaded.info().fingerprint, area_fingerprint(&provenance().summary(), &stream));
        assert_ne!(loaded.info().fingerprint, adapter().list_areas().into_iter().find(|x| x.id.key == "testfld").unwrap().fingerprint);
    }

    #[test]
    fn adapter_version_names_the_crate_and_the_rule() {
        let v = adapter_version();
        assert!(v.starts_with("fmv-ff7/"), "{v}");
        assert!(v.contains(env!("CARGO_PKG_VERSION")), "{v}");
        assert!(v.ends_with(&format!("fingerprint-rule/{FINGERPRINT_RULE_VERSION}")), "{v}");
    }

    #[test]
    fn the_fixture_label_follows_the_provenance() {
        let lgp = build_lgp(&[("testfld", good_entry())]);
        let first = |p: EditionProvenance| {
            let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp.clone()), p).unwrap();
            let listed = a.list_areas().remove(0);
            let loaded = a.load_area("testfld").unwrap().info().clone();
            // listing and loading must agree on the label
            assert_eq!((listed.fixture, &listed.display_name), (loaded.fixture, &loaded.display_name));
            assert_eq!(a.is_fixture(), listed.fixture);
            (listed.fixture, listed.display_name)
        };
        // synthetic platform: FIXTURE, named as such
        let (fixture, name) = first(provenance());
        assert!(fixture);
        assert!(name.starts_with("FIXTURE:") && name.contains("testfld") && name.contains("not FF7"), "{name}");
        // a fixture source alone is enough, whatever the platform says
        let (fixture, name) = first(EditionProvenance { sources: vec![SourceRef::Fixture], ..pc_provenance() });
        assert!(fixture && name.starts_with("FIXTURE:"));
        let (fixture, _) = first(EditionProvenance { platform: Platform::Synthetic, sources: vec![], ..provenance() });
        assert!(fixture);
        // real-looking provenance (the adapter trusts it): not a fixture, plain name
        let (fixture, name) = first(pc_provenance());
        assert!(!fixture);
        assert_eq!(name, "FF7 field testfld");
        assert!(!name.to_ascii_uppercase().contains("FIXTURE"));
        let (fixture, _) = first(EditionProvenance { platform: Platform::Unknown, sources: vec![], ..provenance() });
        assert!(!fixture);
    }

    #[test]
    fn provenance_gate_accepts_ff7_pc_layout_and_rejects_the_rest() {
        let bytes = archive_bytes();
        let open = |p: EditionProvenance| Ff7Adapter::from_flevel_reader(Cursor::new(bytes.clone()), p);
        // not FF7
        for game in [GameId::Fixture, GameId::Ff8, GameId::Ff9] {
            assert!(matches!(open(EditionProvenance { game, ..provenance() }), Err(AdapterError::Unsupported(_))));
        }
        // PlayStation media is another format family
        let err = open(EditionProvenance { platform: Platform::Ps1, ..provenance() }).unwrap_err();
        assert!(matches!(&err, AdapterError::Unsupported(m) if m.contains("Ps1") && m.contains("PC")), "{err}");
        // PC, synthetic and unidentified platforms are accepted, and published unchanged
        for platform in [Platform::Pc, Platform::Synthetic, Platform::Unknown] {
            let p = EditionProvenance { platform, ..provenance() };
            let a = open(p.clone()).unwrap();
            assert_eq!(a.provenance(), &p);
            assert_eq!(a.list_areas()[0].provenance.platform, platform);
        }
        // and an `Unknown` library never matches a `Pc` peer, which is what keeps the sets apart
        let unknown = EditionProvenance { platform: Platform::Unknown, ..provenance() }.summary();
        assert!(!unknown.compatible_with(&pc_provenance().summary()));
    }

    #[test]
    fn a_missing_file_error_does_not_contain_the_local_path() {
        let missing = std::env::temp_dir().join("fmv-ff7-private-dir-name").join("fmv-ff7-definitely-missing-flevel.lgp");
        let err = Ff7Adapter::from_flevel_path(&missing, provenance()).unwrap_err();
        assert!(matches!(err, AdapterError::MissingContent(_)));
        let text = format!("{err} / {err:?}");
        assert!(!text.contains("fmv-ff7-private-dir-name"), "{text}");
        assert!(!text.contains("fmv-ff7-definitely-missing"), "{text}");
        assert!(!text.contains(&*std::env::temp_dir().to_string_lossy()), "{text}");
        assert!(text.contains("flevel.lgp"), "still says what is missing: {text}");
    }

    #[test]
    fn implausible_entry_names_are_neither_listed_nor_loadable() {
        let entry = good_entry();
        let lgp = build_lgp(&[
            ("good_1-b", entry),
            ("bad\nname", entry),
            ("SMOKE name=a", entry),
            ("sp ace", entry),
            ("caf\u{e9}", entry),
            ("\u{1b}[31m", entry),
            ("a/b", entry),
        ]);
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        let keys: Vec<String> = a.list_areas().into_iter().map(|x| x.id.key).collect();
        assert_eq!(keys, ["good_1-b"]);
        let skipped = a.skipped_entries();
        assert_eq!(skipped.len(), 6);
        assert!(skipped.iter().all(|s| s.reason.contains("not offered as an area")), "{skipped:?}");
        // every key that is offered is safe to print on one line
        assert!(keys.iter().all(|k| !k.chars().any(|c| c.is_control() || c.is_whitespace() || c == '=')));
        // load_area refuses them too, with a message that cannot break a line either
        // (the archive's key for "café" is the Latin-1 reading of its UTF-8 bytes)
        for name in ["bad\nname", "SMOKE name=a", "sp ace", "caf\u{c3}\u{a9}", "\u{1b}[31m", "a/b"] {
            let err = a.load_area(name).err().expect("an implausible name must be refused");
            assert!(matches!(err, AdapterError::Unsupported(_)), "{name:?}: {err}");
            assert!(!err.to_string().contains('\n') && !err.to_string().contains('\u{1b}'), "{name:?}: {err:?}");
        }
        assert!(a.load_area("good_1-b").is_ok());
        assert!(check_key("abcdefghijklmnopqrst").is_ok());
        assert!(check_key("abcdefghijklmnopqrstu").is_err());
        assert!(check_key("").is_err());
    }

    #[test]
    fn index_work_is_bounded_by_a_cumulative_budget() {
        // 6 entries of ~600 bytes each; a budget of 1000 stops reading after the second.
        let body = vec![7u8; 600];
        let names = ["a1", "a2", "a3", "a4", "a5", "a6"];
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (*n, body.as_slice())).collect();
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(build_lgp(&entries)), provenance()).unwrap();
        let (index, cacheable) = a.build_index(1000);
        assert!(cacheable);
        let keys: Vec<&str> = index.areas.iter().map(|x| x.id.key.as_str()).collect();
        assert_eq!(keys, ["a1", "a2"]);
        assert_eq!(index.skipped.len(), 4);
        assert!(index.skipped.iter().all(|s| s.reason.contains("budget")), "{:?}", index.skipped);
        // a zero budget reads nothing; an ample one reads everything
        assert!(a.build_index(0).0.areas.is_empty());
        assert_eq!(a.build_index(u64::MAX).0.areas.len(), 6);
        // the public limits are what the docs say
        assert_eq!(MAX_INDEX_READ_BYTES, 512 * 1024 * 1024);
        assert_eq!(MAX_FIELD_BYTES, 16 * 1024 * 1024);
        assert_eq!(crate::lgp::MAX_ENTRY_LEN, 16 * 1024 * 1024);
    }

    /// An archive in which every table-of-contents entry points at the same body (the shape a
    /// hostile archive uses to multiply work). The format allows it; the reader must not care.
    fn aliasing_archive(names: &[String], body: &[u8]) -> Vec<u8> {
        let data_start = 16 + names.len() * 27 + 3600 + 2;
        let mut out = b"\0\0SQUARESOFT".to_vec();
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for name in names {
            let mut n = [0u8; 20];
            n[..name.len()].copy_from_slice(name.as_bytes());
            out.extend_from_slice(&n);
            out.extend_from_slice(&(data_start as u32).to_le_bytes());
            out.push(0x0E);
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out.extend(std::iter::repeat_n(0u8, 3600));
        out.extend_from_slice(&0u16.to_le_bytes());
        let mut n = [0u8; 20];
        n[..4].copy_from_slice(b"body");
        out.extend_from_slice(&n);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn many_entries_aliasing_one_body_cannot_multiply_the_index_work() {
        let names: Vec<String> = (0..300).map(|i| format!("e{i:03}")).collect();
        let body = vec![0xA5u8; 10_000];
        let lgp = aliasing_archive(&names, &body);
        // 300 entries x 10 kB would be 3 MB of reads, out of a file of about 22 kB
        assert!(lgp.len() < 30_000, "the archive itself is tiny: {} bytes", lgp.len());
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        // unbounded, all 300 entries are read (3 MB of work from a 22 KB file)
        assert_eq!(a.build_index(u64::MAX).0.areas.len(), 300);
        // bounded, the work stops after the budget (plus at most the entry that crossed it)
        let (index, cacheable) = a.build_index(35_000);
        assert!(cacheable);
        assert_eq!(index.areas.len(), 4);
        assert_eq!(index.skipped.len(), 296);
        assert!(index.skipped.iter().all(|s| s.reason.contains("budget")));
    }

    /// A reader whose reads can be made to fail, like a flaky network share.
    struct Flaky {
        inner: Cursor<Vec<u8>>,
        fail: Arc<AtomicBool>,
    }
    impl Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "share went away"));
            }
            self.inner.read(buf)
        }
    }
    impl Seek for Flaky {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.inner.seek(from)
        }
    }

    #[test]
    fn a_transient_read_error_is_not_cached_in_the_index() {
        let fail = Arc::new(AtomicBool::new(false));
        let reader = Flaky { inner: Cursor::new(archive_bytes()), fail: Arc::clone(&fail) };
        let a = Ff7Adapter::from_flevel_reader(reader, provenance()).unwrap();
        // the share goes away during the first listing: nothing can be read, and that is not
        // remembered
        fail.store(true, Ordering::SeqCst);
        assert!(a.list_areas().is_empty());
        let skipped = a.skipped_entries();
        assert!(skipped.iter().any(|s| s.name == "testfld" && s.reason.contains("i/o error")), "{skipped:?}");
        // it comes back: the next call reads again and lists everything
        fail.store(false, Ordering::SeqCst);
        assert_eq!(a.list_areas().len(), 4);
        // a complete index is cached: breaking the reader again changes nothing
        fail.store(true, Ordering::SeqCst);
        assert_eq!(a.list_areas().len(), 4);
        assert_eq!(a.prewarm(), 4);
        // loading is not cached and does see the failure
        assert!(matches!(a.load_area("testfld"), Err(AdapterError::Io(_))));
    }

    #[test]
    fn load_area_is_case_insensitive_and_reports_missing() {
        let a = adapter();
        let upper = a.load_area("TESTFLD").unwrap();
        assert_eq!(upper.info().id.key, "testfld"); // canonical archive spelling
        assert!(matches!(a.load_area("nope"), Err(AdapterError::AreaNotFound(k)) if k == "nope"));
        assert!(matches!(a.load_area("maplist"), Err(AdapterError::Decode(_))));
        assert!(matches!(a.load_area("junk"), Err(AdapterError::Decode(_))));
        assert!(matches!(a.load_area("emptymesh"), Err(AdapterError::Decode(m)) if m.contains("no triangles")));
        assert!(matches!(a.load_area("cutshort"), Err(AdapterError::Decode(_))));
    }

    // ---- movement (F1) ----

    #[test]
    fn validate_move_accepts_on_mesh_and_interpolates_height() {
        let area = area();
        let from = Pos::new(10.0, 10.0, 20.0);
        // z = x + y on both triangles; the caller's z is ignored
        match area.validate_move(from, Pos::new(30.0, 20.0, 999.0)) {
            MoveResult::Accepted(p) => {
                assert_eq!((p.x, p.y), (30.0, 20.0));
                assert!((p.z - 50.0).abs() < 1e-3, "z = {}", p.z);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
        // second triangle
        match area.validate_move(from, Pos::new(20.0, 70.0, 0.0)) {
            MoveResult::Accepted(p) => assert!((p.z - 90.0).abs() < 1e-3),
            other => panic!("expected Accepted, got {other:?}"),
        }
        // on the shared diagonal, a corner, and the outer boundary (positions on a wall are
        // moved a hair inside, so the height may differ by a few thousandths)
        for (x, y, z) in [(50.0, 50.0, 100.0), (0.0, 0.0, 0.0), (100.0, 100.0, 200.0), (100.0, 40.0, 140.0), (0.0, 75.0, 75.0)] {
            match area.validate_move(from, Pos::new(x, y, 0.0)) {
                MoveResult::Accepted(p) => {
                    assert!((p.z - z).abs() < 1e-2, "({x},{y}) z = {} expected {z}", p.z);
                    assert!((p.x - x).abs() < 0.05 && (p.y - y).abs() < 0.05);
                }
                other => panic!("({x},{y}) expected Accepted, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_move_corrects_off_mesh_destinations() {
        let area = area();
        let from = Pos::new(10.0, 10.0, 20.0);
        for to in [
            Pos::new(101.0, 50.0, 0.0),
            Pos::new(-0.5, 50.0, 0.0),
            Pos::new(50.0, 100.5, 0.0),
            Pos::new(500.0, 500.0, 0.0),
            Pos::new(1e30, -1e30, 0.0),
            Pos::new(f32::MAX, f32::MIN, 0.0),
            Pos::new(f32::NAN, 5.0, 0.0),
            Pos::new(5.0, f32::INFINITY, 0.0),
            Pos::new(f32::NEG_INFINITY, f32::NAN, f32::NAN),
        ] {
            assert_eq!(area.validate_move(from, to), MoveResult::Corrected(from), "to = {to:?}");
        }
        // from with a wrong height is snapped back onto the surface
        match area.validate_move(Pos::new(10.0, 10.0, 12345.0), Pos::new(500.0, 0.0, 0.0)) {
            MoveResult::Corrected(p) => {
                assert_eq!((p.x, p.y), (10.0, 10.0));
                assert!((p.z - 20.0).abs() < 1e-3);
            }
            other => panic!("expected Corrected, got {other:?}"),
        }
    }

    #[test]
    fn invalid_from_falls_back_to_the_nearest_spawn() {
        let area = area();
        let spawns = area.spawn_points();
        assert!(!spawns.is_empty());
        let nearest = |x: f32, y: f32| {
            *spawns
                .iter()
                .min_by(|a, b| {
                    let d = |p: &Pos| (p.x - x).powi(2) + (p.y - y).powi(2);
                    d(a).total_cmp(&d(b))
                })
                .unwrap()
        };
        // (the two spawns are mirror images across y = x, so pick a point that is not a tie)
        let off = Pos::new(900.0, 100.0, 0.0);
        let bad_to = Pos::new(1000.0, 100.0, 0.0);
        assert_eq!(area.validate_move(off, bad_to), MoveResult::Corrected(nearest(900.0, 100.0)));
        assert_eq!(nearest(900.0, 100.0), spawns[0]);
        // non-finite from: nearest to the (finite) destination, here also off-mesh
        let nan_from = Pos::new(f32::NAN, f32::NAN, f32::NAN);
        assert_eq!(area.validate_move(nan_from, Pos::new(-40.0, 120.0, 0.0)), MoveResult::Corrected(nearest(-40.0, 120.0)));
        // neither usable: first spawn
        assert_eq!(area.validate_move(nan_from, nan_from), MoveResult::Corrected(spawns[0]));
        // a non-finite `from` has no starting triangle: only the destination is checked
        assert!(matches!(area.validate_move(nan_from, Pos::new(10.0, 10.0, 0.0)), MoveResult::Accepted(_)));
        // the same for a `from` that is merely off the mesh
        assert!(matches!(area.validate_move(off, Pos::new(10.0, 10.0, 0.0)), MoveResult::Accepted(_)));
    }

    #[test]
    fn spawn_points_are_distinct_and_walkable() {
        let area = area();
        let spawns = area.spawn_points();
        assert_eq!(spawns.len(), 2); // two usable triangles -> two spawns
        assert_ne!(spawns[0], spawns[1]);
        for s in &spawns {
            match area.validate_move(*s, *s) {
                MoveResult::Accepted(p) => {
                    assert_eq!((p.x, p.y), (s.x, s.y));
                    assert!((p.z - s.z).abs() < 1e-2);
                }
                other => panic!("spawn {s:?} is not walkable: {other:?}"),
            }
        }
        // and they can reach each other: two joiners can meet
        assert!(matches!(area.validate_move(spawns[0], spawns[1]), MoveResult::Accepted(_)));
        assert!(matches!(area.validate_move(spawns[1], spawns[0]), MoveResult::Accepted(_)));
    }

    #[test]
    fn spawns_are_chosen_from_the_largest_connected_region_only() {
        // Region A: a 300 x 300 square (two linked triangles, 45 000 each). Region B: one
        // isolated, bigger single triangle (80 000) that nothing links to. The four largest
        // triangles overall include B's, but a joiner placed there could never meet one in A.
        let tris = vec![
            [(0, 0, 0), (300, 0, 0), (300, 300, 0)],
            [(0, 0, 0), (300, 300, 0), (0, 300, 0)],
            [(1000, 1000, 0), (1400, 1000, 0), (1000, 1400, 0)],
        ];
        let access = vec![[0xFFFF, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF], [0xFFFF; 3]];
        let area = adapter_with_mesh(tris, access).load_area("meshfld").unwrap();
        let spawns = area.spawn_points();
        assert_eq!(spawns.len(), 2);
        assert!(spawns.iter().all(|p| p.x < 300.0 && p.y < 300.0), "{spawns:?}");
        for a in &spawns {
            for b in &spawns {
                assert!(matches!(area.validate_move(*a, *b), MoveResult::Accepted(_)), "{a:?} -> {b:?}");
            }
        }
        // the isolated triangle is still part of the mesh; a move onto it is refused
        let from = spawns[0];
        assert!(matches!(area.validate_move(from, Pos::new(1100.0, 1100.0, 0.0)), MoveResult::Corrected(_)));
    }

    #[test]
    fn climbing_through_a_blocked_edge_is_refused_through_the_whole_adapter() {
        // Floor (z = 0, x 0..100) and an adjacent raised platform (z = 100, x 100..200). In the
        // first mesh the shared x = 100 edge is blocked on both sides; a destination-only check
        // would accept a 100-unit climb across it.
        let tris = vec![
            [(0, 0, 0), (100, 0, 0), (100, 100, 0)],
            [(0, 0, 0), (100, 100, 0), (0, 100, 0)],
            [(100, 0, 100), (200, 0, 100), (200, 100, 100)],
            [(100, 0, 100), (200, 100, 100), (100, 100, 100)],
        ];
        let walled = vec![[0xFFFF, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF], [0xFFFF, 0xFFFF, 3], [2, 0xFFFF, 0xFFFF]];
        let area = adapter_with_mesh(tris.clone(), walled).load_area("meshfld").unwrap();
        let from = Pos::new(95.0, 50.0, 0.0);
        assert_eq!(area.validate_move(from, Pos::new(105.0, 50.0, 0.0)), MoveResult::Corrected(from));
        // lying about the height does not help: the client's z is never used
        assert_eq!(area.validate_move(from, Pos::new(105.0, 50.0, 100.0)), MoveResult::Corrected(from));
        // the way back down is refused too
        let up = Pos::new(105.0, 50.0, 100.0);
        assert_eq!(area.validate_move(up, Pos::new(95.0, 50.0, 0.0)), MoveResult::Corrected(up));
        // each side is walkable on its own
        assert!(matches!(area.validate_move(from, Pos::new(10.0, 90.0, 0.0)), MoveResult::Accepted(_)));
        // the server's own sub-stepping cannot get across either: ten-unit steps from the edge
        let mut cur = from;
        for step in 1u8..=3 {
            match area.validate_move(cur, Pos::new(95.0 + 10.0 * f32::from(step), 50.0, cur.z)) {
                MoveResult::Accepted(p) => cur = p,
                MoveResult::Corrected(_) => break,
            }
        }
        assert!(cur.x < 100.0, "ended at {cur:?}");

        // positive control: with the edge linked in both directions the same step is accepted,
        // at the platform's height
        let linked = vec![[0xFFFF, 3, 1], [0, 0xFFFF, 0xFFFF], [0xFFFF, 0xFFFF, 3], [2, 0xFFFF, 0]];
        let area = adapter_with_mesh(tris, linked).load_area("meshfld").unwrap();
        match area.validate_move(from, Pos::new(105.0, 50.0, 0.0)) {
            MoveResult::Accepted(p) => assert!((p.z - 100.0).abs() < 1e-3, "{p:?}"),
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn a_wall_cannot_be_crossed_by_stopping_on_it_first() {
        // two squares with a wall at x = 100: step onto the wall, then try to step across
        let tris = vec![
            [(0, 0, 0), (100, 0, 0), (100, 100, 0)],
            [(0, 0, 0), (100, 100, 0), (0, 100, 0)],
            [(100, 0, 0), (200, 0, 0), (200, 100, 0)],
            [(100, 0, 0), (200, 100, 0), (100, 100, 0)],
        ];
        let access = vec![[0xFFFF, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF], [0xFFFF, 0xFFFF, 3], [2, 0xFFFF, 0xFFFF]];
        let area = adapter_with_mesh(tris, access).load_area("meshfld").unwrap();
        let start = Pos::new(50.0, 50.0, 0.0);
        let on_wall = match area.validate_move(start, Pos::new(100.0, 50.0, 0.0)) {
            MoveResult::Accepted(p) => p,
            other => panic!("expected Accepted, got {other:?}"),
        };
        assert!(on_wall.x < 100.0, "an accepted position is kept off the wall: {on_wall:?}");
        assert_eq!(area.validate_move(on_wall, Pos::new(150.0, 50.0, 0.0)), MoveResult::Corrected(on_wall));
    }

    #[test]
    fn walk_geometry_is_exposed_for_the_client() {
        let a = adapter();
        let area = a.load_field("testfld").unwrap();
        let report = area.link_report();
        assert_eq!(report, LinkReport { links: 2, symmetric: 2, adjacent: 2, blocked_edges: 4 });
    }

    #[test]
    fn bounds_triangles_speed_and_info() {
        let area = area();
        let b = area.bounds();
        assert_eq!(b.min, Pos::new(0.0, 0.0, 0.0));
        assert_eq!(b.max, Pos::new(100.0, 100.0, 200.0));
        let tris = area.walk_triangles();
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0][1], Pos::new(100.0, 0.0, 100.0));
        assert_eq!(area.max_speed(), FF7_FIELD_MAX_SPEED_PLACEHOLDER);
        assert!(area.max_speed() > 0.0);
        assert!(area.info().fixture, "the test archive is synthetic and labelled so");
        assert_eq!(area.info().id.game, GameId::Ff7);
    }

    #[test]
    fn concrete_area_exposes_field_data_for_rendering() {
        let a = adapter();
        let area = a.load_field("testfld").unwrap();
        assert!(format!("{area:?}").contains("testfld"));
        assert_eq!(area.walkmesh().len(), 2);
        assert_eq!(area.cameras().unwrap().len(), 1);
        assert_eq!(area.palette().unwrap().page_count(), 2);
        assert_eq!(area.triggers().unwrap().name, "testfld");
        assert_eq!(area.background().unwrap().tiles.len(), 4);
        assert_eq!(area.field_file().bytes(), FieldSpec::default().build().as_slice());
        assert!((area.height_at(30.0, 20.0, None).unwrap() - 50.0).abs() < 1e-3);
        assert!(area.height_at(500.0, 0.0, None).is_none());
        assert!(area.height_at(f32::NAN, 0.0, None).is_none());
        assert!(area.height_at(10.0, 10.0, Some(f32::NAN)).is_some());
    }

    #[test]
    fn damaged_non_walkmesh_sections_do_not_block_hosting() {
        // Corrupt the background tag and drop the palette header: the area still loads.
        let spec = FieldSpec { corrupt_texture_tag: true, ..FieldSpec::default() };
        let mut bytes = spec.build();
        // palette section is index 3: wreck its colours-per-page field
        let pal_start = u32::from_le_bytes(bytes[6 + 12..6 + 16].try_into().unwrap()) as usize;
        bytes[pal_start + 4 + 8..pal_start + 4 + 10].copy_from_slice(&0u16.to_le_bytes());
        let lgp = build_lgp(&[("testfld", &lzss::compress(&bytes))]);
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        let area = a.load_field("testfld").unwrap();
        assert!(area.palette().is_err());
        assert!(area.background().unwrap().texture_error.is_some());
        assert!(matches!(area.validate_move(Pos::default(), Pos::new(10.0, 10.0, 0.0)), MoveResult::Accepted(_)));
    }

    #[test]
    fn overlapping_triangles_choose_the_surface_nearest_the_hint() {
        // Two stacked 100x100 squares: floor at z = 0, balcony at z = 100.
        let flat = |z: i16| -> [[(i16, i16, i16); 3]; 2] {
            [[(0, 0, z), (100, 0, z), (100, 100, z)], [(0, 0, z), (100, 100, z), (0, 100, z)]]
        };
        let [f0, f1] = flat(0);
        let [b0, b1] = flat(100);
        let access = vec![[0xFFFF, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF], [0xFFFF, 0xFFFF, 3], [2, 0xFFFF, 0xFFFF]];
        let a = adapter_with_mesh(vec![f0, f1, b0, b1], access);
        let area = a.load_area("meshfld").unwrap();
        let at = Pos::new(50.0, 25.0, 0.0);
        for (from_z, want) in [(0.0, 0.0), (100.0, 100.0), (30.0, 0.0), (80.0, 100.0)] {
            match area.validate_move(Pos::new(10.0, 10.0, from_z), at) {
                MoveResult::Accepted(p) => assert_eq!(p.z, want, "from.z = {from_z}"),
                other => panic!("expected Accepted, got {other:?}"),
            }
        }
        // crossing the diagonal on a layer stays on that layer
        for (from_z, want) in [(0.0, 0.0), (100.0, 100.0)] {
            match area.validate_move(Pos::new(80.0, 20.0, from_z), Pos::new(20.0, 80.0, 0.0)) {
                MoveResult::Accepted(p) => assert_eq!(p.z, want, "from.z = {from_z}"),
                other => panic!("expected Accepted, got {other:?}"),
            }
        }
        // no usable hint: the first triangle (the floor) wins
        match area.validate_move(Pos::new(f32::NAN, 0.0, f32::NAN), at) {
            MoveResult::Accepted(p) => assert_eq!(p.z, 0.0),
            other => panic!("expected Accepted, got {other:?}"),
        }
        match area.validate_move(Pos::new(10.0, 10.0, f32::NAN), at) {
            MoveResult::Accepted(p) => assert_eq!(p.z, 0.0),
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn degenerate_triangles_are_ignored() {
        let a = adapter_with_mesh(
            vec![
                [(0, 0, 0), (50, 50, 0), (100, 100, 0)], // collinear
                [(5, 5, 0), (5, 5, 0), (5, 5, 0)],       // a point
                [(0, 0, 0), (100, 0, 0), (0, 100, 0)],
            ],
            vec![[0xFFFF; 3]; 3],
        );
        let area = a.load_area("meshfld").unwrap();
        assert_eq!(area.spawn_points().len(), 1);
        assert_eq!(area.walk_triangles().len(), 3); // debug overlay still sees all of them
        assert!(matches!(area.validate_move(Pos::new(10.0, 10.0, 0.0), Pos::new(20.0, 20.0, 0.0)), MoveResult::Accepted(_)));
        assert_eq!(
            area.validate_move(Pos::new(10.0, 10.0, 0.0), Pos::new(90.0, 90.0, 0.0)),
            MoveResult::Corrected(Pos::new(10.0, 10.0, 0.0))
        );
        // a mesh with ONLY degenerate triangles cannot host an area (it is still listed:
        // listing does not decode, loading says why)
        let spec = FieldSpec::with_mesh(vec![[(0, 0, 0), (50, 50, 0), (100, 100, 0)]], vec![[0xFFFF; 3]]);
        let lgp = build_lgp(&[("flat", &lzss::compress(&spec.build()))]);
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        assert_eq!(a.list_areas().len(), 1);
        assert!(matches!(a.load_area("flat"), Err(AdapterError::Decode(m)) if m.contains("non-degenerate")));
    }

    #[test]
    fn duplicate_entry_names_resolve_to_the_first_entry_everywhere() {
        let first = good_entry().to_vec();
        let second = packed(&FieldSpec::with_mesh(vec![[(0, 0, 0), (10, 0, 0), (0, 10, 0)]], vec![[0xFFFF; 3]]));
        let lgp = build_lgp(&[("dupe", &first), ("DUPE", &second), ("alpha", &second)]);
        let a = Ff7Adapter::from_flevel_reader(Cursor::new(lgp), provenance()).unwrap();
        let areas = a.list_areas();
        // sorted by key, duplicate dropped
        let keys: Vec<&str> = areas.iter().map(|x| x.id.key.as_str()).collect();
        assert_eq!(keys, ["alpha", "dupe"]);
        let dupe = areas.iter().find(|x| x.id.key == "dupe").unwrap();
        assert_eq!(dupe.fingerprint, area_fingerprint(&provenance().summary(), &first));
        assert_eq!(a.load_area("DUPE").unwrap().info().fingerprint, dupe.fingerprint);
        let skipped = a.skipped_entries();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "DUPE");
        assert!(skipped[0].reason.contains("duplicate"));
    }

    #[test]
    fn edge_tolerance_is_tiny() {
        let area = area();
        let from = Pos::new(10.0, 10.0, 20.0);
        // 0.0005 units outside the x = 100 edge is absorbed, 0.01 is not
        assert!(matches!(area.validate_move(from, Pos::new(100.0005, 50.0, 0.0)), MoveResult::Accepted(_)));
        assert!(matches!(area.validate_move(from, Pos::new(100.01, 50.0, 0.0)), MoveResult::Corrected(_)));
    }

    #[test]
    fn broken_archives_are_errors_not_panics() {
        assert!(matches!(
            Ff7Adapter::from_flevel_reader(Cursor::new(Vec::<u8>::new()), provenance()),
            Err(AdapterError::Decode(_))
        ));
        assert!(matches!(
            Ff7Adapter::from_flevel_reader(Cursor::new(vec![7u8; 4096]), provenance()),
            Err(AdapterError::Decode(_))
        ));
        let missing = std::env::temp_dir().join("fmv-ff7-definitely-missing-flevel.lgp");
        assert!(matches!(Ff7Adapter::from_flevel_path(&missing, provenance()), Err(AdapterError::MissingContent(_))));
        // every prefix of a real archive either fails to open or lists/loads without panic
        let bytes = archive_bytes();
        for n in (0..bytes.len()).step_by(97) {
            if let Ok(a) = Ff7Adapter::from_flevel_reader(Cursor::new(bytes[..n].to_vec()), provenance()) {
                let _ = a.list_areas();
                let _ = a.load_area("testfld");
            }
        }
    }

    #[test]
    fn from_path_opens_read_only_and_leaves_the_file_untouched() {
        let bytes = archive_bytes();
        let path = std::env::temp_dir().join(format!("fmv-ff7-test-{}-{:?}.lgp", std::process::id(), std::thread::current().id()));
        std::fs::write(&path, &bytes).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        {
            let a = Ff7Adapter::from_flevel_path(&path, provenance()).unwrap();
            assert_eq!(a.list_areas().len(), 4);
            assert!(a.load_area("testfld").is_ok());
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        std::fs::remove_file(&path).unwrap();
    }
}
