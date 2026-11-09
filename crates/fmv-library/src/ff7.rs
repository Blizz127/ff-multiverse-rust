//! Finding the owner's FF7 field archive (`flevel.lgp`) and the provenance it is presented
//! with. The one resolver the server and every client share.
//!
//! The area fingerprint binds the provenance (edition, region, revision), so a client that
//! resolved a different library set than the server's would be refused at join. Both binaries
//! therefore call the same function instead of each keeping its own copy of the rules. Nothing
//! here opens or decodes the archive: that is `fmv-ff7`'s job, and this crate does not depend
//! on it.
//!
//! Two sources:
//!
//! * [`Ff7Source::Library`]: the content-addressed library `fmv-import` wrote under a state
//!   directory. The set is the one named explicitly, or the only FF7 PC set; the blob is the
//!   set's `data/lang-<language>/field/flevel.lgp` (Steam layout), else `data/field/flevel.lgp`
//!   (classic layout), else the only `flevel.lgp` anywhere in the set. The set's provenance is
//!   returned unchanged (install path included: it stays on this machine, the wire carries
//!   `EditionProvenance::summary()`).
//! * [`Ff7Source::Flevel`]: a `flevel.lgp` read directly and read-only. There is no manifest to
//!   say what edition it is, so the operator may supply the labels (they default to
//!   `unknown`). They are part of the fingerprint: two peers that read the same bytes but label
//!   them differently cannot share an instance, which is the safe direction.
//!
//! The owner's audio (music and sound effects) is found the same way, by [`resolve_audio`]: from
//! the same library set, or, for a direct `flevel.lgp`, from the folders of the install it sits in.
//!
//! Everything here is read-only. Local paths appear in errors shown on this machine only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fmv_core::{EditionProvenance, GameId, Platform, SourceRef};

use crate::error::LibraryError;
use crate::library::Library;
use crate::manifest::Manifest;
use crate::paths::resolve_state_dir;

const UNKNOWN: &str = "unknown";

/// Edition labels for a `flevel.lgp` that was not imported through the library.
///
/// They stand in for the manifest's provenance, and the area fingerprint binds `edition`,
/// `region` and `revision` (not `language`). To share an instance with a peer that uses a
/// library set, give the same three labels as that set (`fmv-import list` shows them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ff7Labels {
    pub edition: String,
    pub region: String,
    pub language: String,
    pub revision: String,
}

impl Default for Ff7Labels {
    fn default() -> Self {
        let unknown = || UNKNOWN.to_string();
        Self {
            edition: unknown(),
            region: unknown(),
            language: unknown(),
            revision: unknown(),
        }
    }
}

/// Where the FF7 field archive comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ff7Source {
    /// An imported library. `state_dir` of `None` means the default (`$FMV_STATE_DIR`, else the
    /// user data directory); `set` of `None` means the only FF7 PC set.
    Library {
        state_dir: Option<PathBuf>,
        set: Option<String>,
    },
    /// A `flevel.lgp` opened directly (read-only), with operator-supplied labels.
    Flevel { path: PathBuf, labels: Ff7Labels },
}

#[derive(Debug, thiserror::Error)]
pub enum Ff7ResolveError {
    #[error("no owned-content library at {dir}: needs owned FF7 content imported via fmv-import (for example `fmv-import import-pc <FF7 install> --state-dir {dir}`)")]
    NoLibrary { dir: String },
    #[error("cannot read the owned-content library: {0}")]
    Library(#[from] LibraryError),
    #[error("the library has no FF7 PC set: needs owned FF7 content imported via fmv-import")]
    NoFf7Set,
    #[error("the library holds several FF7 PC sets ({0}); choose one with --ff7-set")]
    AmbiguousSet(String),
    #[error("library set {set:?} cannot be used for FF7 fields: {why}")]
    UnusableSet { set: String, why: String },
    #[error("library set {0:?} has no flevel.lgp (the FF7 field archive): import it again from a complete install")]
    NoFlevel(String),
    /// A set without `scene.bin` or `kernel.bin` (the FF7 battle data).
    #[error("library set {set:?} has no {file} (FF7 battle data): import it again from a complete install")]
    NoBattleFile { set: String, file: &'static str },
    /// Battle data is only found through a library set, never through a bare `flevel.lgp`.
    #[error("battle data (scene.bin, kernel.bin) is only read from an imported library set; a direct --ff7-flevel has none")]
    BattleNeedsLibrary,
}

/// A `flevel.lgp` found on this machine and the provenance it is to be presented with.
#[derive(Debug, Clone)]
pub struct ResolvedFlevel {
    pub path: PathBuf,
    /// Unchanged from the library manifest (or built from the operator's labels), install path
    /// included. Never put it on the wire: send `provenance.summary()`.
    pub provenance: EditionProvenance,
    /// Path-free description for logs ("library set <id>" / "direct flevel.lgp").
    pub origin: String,
}

/// A set the FF7 PC adapter can read: FF7, and not PlayStation media.
fn usable_for_fields(m: &Manifest) -> Result<(), String> {
    if m.provenance.game != GameId::Ff7 {
        return Err(format!("it holds {:?} content", m.provenance.game));
    }
    if m.provenance.platform == Platform::Ps1 {
        return Err("it is PlayStation media; only the PC field archive layout is supported".into());
    }
    Ok(())
}

/// The project's FIXTURE predicate (synthetic content is never real FF7).
fn is_fixture(m: &Manifest) -> bool {
    m.provenance.game.is_fixture()
        || m.provenance.platform == Platform::Synthetic
        || m.provenance
            .sources
            .iter()
            .any(|s| matches!(s, SourceRef::Fixture))
}

/// Which set to read. An explicit id is taken as given (a synthetic FF7-format set is allowed
/// then, and the server labels it FIXTURE); without one, only real FF7 PC sets count, and there
/// must be exactly one.
fn pick_set(lib: &Library, requested: Option<&str>) -> Result<(String, Manifest), Ff7ResolveError> {
    if let Some(id) = requested {
        let manifest = lib.load_manifest(id)?;
        usable_for_fields(&manifest).map_err(|why| Ff7ResolveError::UnusableSet {
            set: id.to_string(),
            why,
        })?;
        return Ok((id.to_string(), manifest));
    }
    let mut found: Vec<(String, Manifest)> = Vec::new();
    for id in lib.list_sets()? {
        // A set whose manifest cannot be read is not a candidate; naming it explicitly shows why.
        let Ok(manifest) = lib.load_manifest(&id) else {
            continue;
        };
        if usable_for_fields(&manifest).is_ok() && !is_fixture(&manifest) {
            found.push((id, manifest));
        }
    }
    if found.len() > 1 {
        let ids = found.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        return Err(Ff7ResolveError::AmbiguousSet(ids.join(", ")));
    }
    found.into_iter().next().ok_or(Ff7ResolveError::NoFf7Set)
}

/// The logical path of the set's field archive: the language directory of the Steam layout
/// first, then the classic layout, then a single `flevel.lgp` anywhere else.
fn flevel_logical_path(m: &Manifest) -> Option<String> {
    let language = m.provenance.language.to_ascii_lowercase();
    let preferred = [
        format!("data/lang-{language}/field/flevel.lgp"),
        "data/field/flevel.lgp".to_string(),
    ];
    if let Some(path) = preferred.into_iter().find(|p| m.find(p).is_some()) {
        return Some(path);
    }
    let mut any = m
        .entries
        .iter()
        .filter(|e| e.logical_path.rsplit('/').next() == Some("flevel.lgp"));
    match (any.next(), any.next()) {
        (Some(only), None) => Some(only.logical_path.clone()),
        _ => None,
    }
}

/// The logical path of the set's field *character* archive (`field/char.lgp`, the models and
/// animations of the playable characters), found by the same rule as [`flevel_logical_path`].
fn char_logical_path(m: &Manifest) -> Option<String> {
    let language = m.provenance.language.to_ascii_lowercase();
    let preferred = [
        format!("data/lang-{language}/field/char.lgp"),
        "data/field/char.lgp".to_string(),
    ];
    if let Some(path) = preferred.into_iter().find(|p| m.find(p).is_some()) {
        return Some(path);
    }
    let mut any = m
        .entries
        .iter()
        .filter(|e| e.logical_path.rsplit('/').next() == Some("char.lgp"));
    match (any.next(), any.next()) {
        (Some(only), None) => Some(only.logical_path.clone()),
        _ => None,
    }
}

/// Find the field character archive (`char.lgp`) that belongs with the field archive
/// [`resolve_flevel`] finds for `source`, read-only; `Ok(None)` when there is none.
///
/// A client draws the players as the models in that archive and falls back to plain markers
/// without it, so a missing archive is not an error. A library set is chosen by exactly the rules
/// of [`resolve_library_flevel`] (call it with the same `source`, and both read the same set; a
/// wrong or ambiguous set is the same error). A direct `flevel.lgp` has the archive only when a
/// file named `char.lgp` (any case) sits in the same directory, as in an install's `field/`
/// folder; a library blob has no siblings and so no archive. The archive is never sent over the
/// network: it is decoded on this machine only.
pub fn resolve_char_lgp(source: &Ff7Source) -> Result<Option<PathBuf>, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => {
            let dir = resolve_state_dir(state_dir.as_deref())?;
            let lib = Library::open_existing(&dir).map_err(|e| match e {
                LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
                    dir: dir.display().to_string(),
                },
                other => Ff7ResolveError::Library(other),
            })?;
            let (set_id, manifest) = pick_set(&lib, set.as_deref())?;
            match char_logical_path(&manifest) {
                Some(logical) => Ok(lib.get(&set_id, &logical)?),
                None => Ok(None),
            }
        }
        Ff7Source::Flevel { path, .. } => {
            let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
                return Ok(None);
            };
            let Ok(entries) = std::fs::read_dir(parent) else {
                return Ok(None);
            };
            let mut found: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("char.lgp"))
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            // Several spellings next to each other: pick the same one every time.
            found.sort();
            Ok(found.into_iter().next())
        }
    }
}

/// The logical path of the set's battle model archive (`battle/battle.lgp`: the enemy, battle
/// location and playable character models and their animations), found by the same rule as
/// [`flevel_logical_path`] (the Steam language directory, the classic layout, a lone file).
fn battle_lgp_logical_path(m: &Manifest) -> Option<String> {
    let language = m.provenance.language.to_ascii_lowercase();
    battle_logical_path(
        m,
        &[
            format!("data/lang-{language}/battle/battle.lgp"),
            "data/battle/battle.lgp".to_string(),
        ],
        "battle.lgp",
    )
}

/// The entry of `dir` named `name` in any letter case (the first in name order), a directory when
/// `want_dir` and a file otherwise. Installs differ in the case of their folder and file names.
fn child_ignoring_case(dir: &Path, name: &str, want_dir: bool) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
        .filter(|p| if want_dir { p.is_dir() } else { p.is_file() })
        .collect();
    // Several spellings next to each other: pick the same one every time.
    found.sort();
    found.into_iter().next()
}

/// A directory path that can be listed: the empty path (a relative path's missing parent) is `.`.
fn or_here(p: &Path) -> PathBuf {
    if p.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        p.to_path_buf()
    }
}

/// The `battle.lgp` of the install a `flevel.lgp` sits in: `field/flevel.lgp` and
/// `battle/battle.lgp` are sibling folders (of the install's `data` directory), and for a
/// `lang-xx/field/flevel.lgp` the language folder's own `battle/` is tried first, then the one
/// above it.
fn battle_lgp_beside_install(flevel: &Path) -> Option<PathBuf> {
    let field_dir = flevel.parent().filter(|p| !p.as_os_str().is_empty())?;
    let install = field_dir.parent()?;
    let mut roots = vec![or_here(install)];
    let is_language_dir = install
        .file_name()
        .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("lang-"));
    if is_language_dir {
        if let Some(up) = install.parent() {
            roots.push(or_here(up));
        }
    }
    roots.into_iter().find_map(|root| {
        let battle = child_ignoring_case(&root, "battle", true)?;
        child_ignoring_case(&battle, "battle.lgp", false)
    })
}

/// Find the battle model archive (`battle.lgp`) that belongs with the field archive
/// [`resolve_flevel`] finds for `source`, read-only; `Ok(None)` when there is none.
///
/// A client draws battles with the models in that archive and has none to draw without it, so a
/// missing archive is not an error here. A library set is chosen by exactly the rules of
/// [`resolve_library_flevel`] (call it with the same `source`, and both read the same set; a wrong
/// or ambiguous set is the same error). A direct `flevel.lgp` has the archive when its install has
/// one in the layout `field/flevel.lgp` + `battle/battle.lgp` (folder and file names in any letter
/// case; for a `lang-xx/field/flevel.lgp` also the language folder's own `battle/`): **not** a
/// sibling file as `char.lgp` is, because `battle.lgp` lives in another folder of the install. Like
/// `char.lgp` it is presentation only (decoded on this machine, never sent over the network, part
/// of no fingerprint), unlike the battle *data* files of [`resolve_battle_files`], which a direct
/// `flevel.lgp` cannot supply.
pub fn resolve_battle_lgp(source: &Ff7Source) -> Result<Option<PathBuf>, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => {
            let dir = resolve_state_dir(state_dir.as_deref())?;
            let lib = Library::open_existing(&dir).map_err(|e| match e {
                LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
                    dir: dir.display().to_string(),
                },
                other => Ff7ResolveError::Library(other),
            })?;
            let (set_id, manifest) = pick_set(&lib, set.as_deref())?;
            match battle_lgp_logical_path(&manifest) {
                Some(logical) => Ok(lib.get(&set_id, &logical)?),
                None => Ok(None),
            }
        }
        Ff7Source::Flevel { path, .. } => Ok(battle_lgp_beside_install(path)),
    }
}

/// Find the `flevel.lgp` of an imported library set and its provenance. `state_dir` of `None`
/// is the default state directory; `set` of `None` is the only FF7 PC set. Opens the library
/// read-only (nothing is created or written).
pub fn resolve_library_flevel(
    state_dir: Option<&Path>,
    set: Option<&str>,
) -> Result<ResolvedFlevel, Ff7ResolveError> {
    let dir = resolve_state_dir(state_dir)?;
    let lib = Library::open_existing(&dir).map_err(|e| match e {
        LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
            dir: dir.display().to_string(),
        },
        other => Ff7ResolveError::Library(other),
    })?;
    let (set_id, manifest) = pick_set(&lib, set)?;
    let logical = flevel_logical_path(&manifest).ok_or_else(|| Ff7ResolveError::NoFlevel(set_id.clone()))?;
    // `get` checks the blob's size against the manifest (the full content check is
    // `Library::verify_entry`; the area fingerprint hashes the archive entry anyway).
    let path = lib
        .get(&set_id, &logical)?
        .ok_or_else(|| Ff7ResolveError::NoFlevel(set_id.clone()))?;
    Ok(ResolvedFlevel {
        path,
        provenance: manifest.provenance,
        origin: format!("library set {set_id}"),
    })
}

/// Find the `flevel.lgp` and its provenance without opening the archive.
pub fn resolve_flevel(source: &Ff7Source) -> Result<ResolvedFlevel, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => resolve_library_flevel(state_dir.as_deref(), set.as_deref()),
        Ff7Source::Flevel { path, labels } => {
            // A flevel.lgp is the PC archive layout, so the platform is PC; everything else the
            // operator knows is in the labels.
            let provenance = EditionProvenance {
                game: GameId::Ff7,
                platform: Platform::Pc,
                edition: labels.edition.clone(),
                region: labels.region.clone(),
                language: labels.language.clone(),
                revision: labels.revision.clone(),
                sources: vec![SourceRef::Install {
                    path: path.display().to_string(),
                }],
                modded: false,
            };
            Ok(ResolvedFlevel {
                path: path.clone(),
                provenance,
                origin: "direct flevel.lgp".to_string(),
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Battle data (scene.bin, kernel.bin)
// ---------------------------------------------------------------------------------------------

/// The FF7 battle data files of a library set, and the provenance of that set. The server and
/// every client resolve their own copy with [`resolve_battle_files`]; the files are decoded by
/// `fmv-ff7-battle` (this crate does not depend on it) and are never sent over the network.
#[derive(Debug, Clone)]
pub struct ResolvedBattleFiles {
    /// `scene.bin`: the enemy formations and stat records.
    pub scene: PathBuf,
    /// `kernel.bin`: character growth curves, starting data, weapons and armor.
    pub kernel: PathBuf,
    /// Unchanged from the library manifest (install path included): never put it on the wire.
    pub provenance: EditionProvenance,
    /// Path-free description for logs ("library set <id>").
    pub origin: String,
}

/// The logical path of `file_name` in `m`: the preferred paths first (the Steam language
/// directory, then the classic layout), then the only entry of that name anywhere else.
fn battle_logical_path(m: &Manifest, preferred: &[String], file_name: &str) -> Option<String> {
    if let Some(path) = preferred.iter().find(|p| m.find(p).is_some()) {
        return Some(path.clone());
    }
    let mut any = m
        .entries
        .iter()
        .filter(|e| e.logical_path.rsplit('/').next() == Some(file_name));
    match (any.next(), any.next()) {
        (Some(only), None) => Some(only.logical_path.clone()),
        _ => None,
    }
}

/// Find `scene.bin` and `kernel.bin` of an imported library set. `state_dir` of `None` is the
/// default state directory; `set` of `None` is the only FF7 PC set (call it with the same
/// arguments as [`resolve_library_flevel`] so both read the same set). Opens the library
/// read-only (nothing is created or written).
pub fn resolve_library_battle_files(
    state_dir: Option<&Path>,
    set: Option<&str>,
) -> Result<ResolvedBattleFiles, Ff7ResolveError> {
    let dir = resolve_state_dir(state_dir)?;
    let lib = Library::open_existing(&dir).map_err(|e| match e {
        LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
            dir: dir.display().to_string(),
        },
        other => Ff7ResolveError::Library(other),
    })?;
    let (set_id, manifest) = pick_set(&lib, set)?;
    let language = manifest.provenance.language.to_ascii_lowercase();
    let scene_logical = battle_logical_path(
        &manifest,
        &[
            format!("data/lang-{language}/battle/scene.bin"),
            "data/battle/scene.bin".to_string(),
        ],
        "scene.bin",
    )
    .ok_or_else(|| Ff7ResolveError::NoBattleFile {
        set: set_id.clone(),
        file: "scene.bin",
    })?;
    let kernel_logical = battle_logical_path(
        &manifest,
        &[
            format!("data/lang-{language}/kernel/kernel.bin"),
            "data/kernel/kernel.bin".to_string(),
        ],
        "kernel.bin",
    )
    .ok_or_else(|| Ff7ResolveError::NoBattleFile {
        set: set_id.clone(),
        file: "kernel.bin",
    })?;
    let blob = |logical: &str, file: &'static str| -> Result<PathBuf, Ff7ResolveError> {
        lib.get(&set_id, logical)?
            .ok_or_else(|| Ff7ResolveError::NoBattleFile {
                set: set_id.clone(),
                file,
            })
    };
    Ok(ResolvedBattleFiles {
        scene: blob(&scene_logical, "scene.bin")?,
        kernel: blob(&kernel_logical, "kernel.bin")?,
        provenance: manifest.provenance,
        origin: format!("library set {set_id}"),
    })
}

/// Find the battle data files for `source`: a library set has them, a bare `flevel.lgp` does
/// not ([`Ff7ResolveError::BattleNeedsLibrary`]).
pub fn resolve_battle_files(source: &Ff7Source) -> Result<ResolvedBattleFiles, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => resolve_library_battle_files(state_dir.as_deref(), set.as_deref()),
        Ff7Source::Flevel { .. } => Err(Ff7ResolveError::BattleNeedsLibrary),
    }
}

// ---------------------------------------------------------------------------------------------
// Audio (music and sound effects)
// ---------------------------------------------------------------------------------------------

/// The FF7 audio files found for a source (see [`resolve_audio`]); every field is empty for an
/// install or library without audio. The files are decoded on this machine only and never sent
/// over the network.
#[derive(Debug, Clone, Default)]
pub struct ResolvedAudio {
    /// `data/music/music.idx`
    pub music_idx: Option<PathBuf>,
    /// `data/music_ogg/<name>.ogg`, keyed by the lower-case file stem (`bat`).
    pub music_ogg: BTreeMap<String, PathBuf>,
    /// `data/music/<name>.wav`, keyed the same way.
    pub music_wav: BTreeMap<String, PathBuf>,
    /// `data/sound/audio.dat`
    pub sfx_dat: Option<PathBuf>,
    /// `data/sound/audio.fmt`
    pub sfx_fmt: Option<PathBuf>,
}

impl ResolvedAudio {
    /// True when there is no audio file at all.
    pub fn is_empty(&self) -> bool {
        self.music_idx.is_none()
            && self.music_ogg.is_empty()
            && self.music_wav.is_empty()
            && self.sfx_dat.is_none()
            && self.sfx_fmt.is_none()
    }

    /// The file of the music track `name` (matched ASCII-case-insensitively): the `.ogg` first,
    /// else the `.wav`.
    pub fn music_file(&self, name: &str) -> Option<&Path> {
        let key = name.to_ascii_lowercase();
        self.music_ogg
            .get(&key)
            .or_else(|| self.music_wav.get(&key))
            .map(PathBuf::as_path)
    }

    /// Fill `slot` with `path`, unless a file already did (the first one found stays).
    fn fill(&mut self, slot: AudioSlot, path: PathBuf) {
        match slot {
            AudioSlot::MusicIdx => {
                self.music_idx.get_or_insert(path);
            }
            AudioSlot::SfxDat => {
                self.sfx_dat.get_or_insert(path);
            }
            AudioSlot::SfxFmt => {
                self.sfx_fmt.get_or_insert(path);
            }
            AudioSlot::MusicOgg(stem) => {
                self.music_ogg.entry(stem).or_insert(path);
            }
            AudioSlot::MusicWav(stem) => {
                self.music_wav.entry(stem).or_insert(path);
            }
        }
    }
}

/// What a file of a data directory is, as far as audio goes.
enum AudioSlot {
    MusicIdx,
    MusicOgg(String),
    MusicWav(String),
    SfxDat,
    SfxFmt,
}

/// The slot of file `file` in folder `dir` of a data directory (both lower case): the places
/// `fmv-import` stores audio at, and nothing else.
fn audio_slot(dir: &str, file: &str) -> Option<AudioSlot> {
    let stem_of = |ext: &str| {
        file.strip_suffix(ext)
            .filter(|stem| !stem.is_empty())
            .map(str::to_string)
    };
    match (dir, file) {
        ("music", "music.idx") => Some(AudioSlot::MusicIdx),
        ("sound", "audio.dat") => Some(AudioSlot::SfxDat),
        ("sound", "audio.fmt") => Some(AudioSlot::SfxFmt),
        ("music_ogg", _) => stem_of(".ogg").map(AudioSlot::MusicOgg),
        ("music", _) => stem_of(".wav").map(AudioSlot::MusicWav),
        _ => None,
    }
}

/// The slot of a library path (`data/music_ogg/bat.ogg`, ...), in any letter case.
fn audio_slot_of_logical(logical: &str) -> Option<AudioSlot> {
    let lower = logical.to_ascii_lowercase();
    let mut parts = lower.split('/');
    let (data, dir, file, rest) = (parts.next()?, parts.next()?, parts.next()?, parts.next());
    if data != "data" || rest.is_some() {
        return None;
    }
    audio_slot(dir, file)
}

/// The importer's rule for an audio file name it takes (`fmv-import`'s scan): letters, digits, `_`,
/// `.` and `-` only, at most 64 bytes, no leading dot. A direct folder is held to the same names (not
/// to the importer's other limits: it reads a local folder in place, so links are followed and there
/// is no cap on the number of files, as for `char.lgp` and `battle.lgp`).
fn is_plain_audio_name(name: &str) -> bool {
    !name.starts_with('.')
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Most entries of one audio folder looked at (a real one holds under a hundred).
const MAX_AUDIO_FOLDER_ENTRIES: usize = 4096;

/// The audio of a `data` directory: the folders `music_ogg`, `music` and `sound` in any letter
/// case, and in them the regular, non-empty files the importer would take.
fn audio_in_data_dir(data: &Path) -> ResolvedAudio {
    let mut out = ResolvedAudio::default();
    for dir in ["music_ogg", "music", "sound"] {
        let Some(folder) = child_ignoring_case(data, dir, true) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .take(MAX_AUDIO_FOLDER_ENTRIES)
            .map(|e| e.path())
            .collect();
        // Several spellings of one name: pick the same one every time.
        files.sort();
        for path in files {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if !is_plain_audio_name(&name) {
                continue;
            }
            let Some(slot) = audio_slot(dir, &name.to_ascii_lowercase()) else {
                continue;
            };
            if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0) {
                out.fill(slot, path);
            }
        }
    }
    out
}

/// The audio of the install a `flevel.lgp` sits in: `field/flevel.lgp` and the folders
/// `music_ogg/`, `music/` and `sound/` are siblings (of the install's `data` directory), and for a
/// `lang-xx/field/flevel.lgp` the language folder's own folders are tried first, then the ones in
/// the folder above it. Of the candidates the first with any audio at all is taken whole, never a
/// mix of two.
///
/// Only the `field` folder of an install says where its `data` directory is: a `flevel.lgp` lying
/// in some other folder must not make that folder's neighbours "the game's audio" (a `Music` folder
/// beside the `Downloads` it was put in).
fn audio_beside_install(flevel: &Path) -> ResolvedAudio {
    let Some(field_dir) = flevel.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return ResolvedAudio::default();
    };
    let is_field_folder = field_dir
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("field"));
    let Some(install) = field_dir.parent().filter(|_| is_field_folder) else {
        return ResolvedAudio::default();
    };
    let mut roots = vec![or_here(install)];
    let is_language_dir = install.file_name().is_some_and(|n| {
        n.to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("lang-")
    });
    if is_language_dir {
        if let Some(up) = install.parent() {
            roots.push(or_here(up));
        }
    }
    roots
        .into_iter()
        .map(|root| audio_in_data_dir(&root))
        .find(|audio| !audio.is_empty())
        .unwrap_or_default()
}

/// Find the audio (music tracks, music index, sound effects) that belongs with the field archive
/// [`resolve_flevel`] finds for `source`, read-only; an empty [`ResolvedAudio`] when there is none.
///
/// The client plays music and sound effects when it has them and is silent without, so no audio is
/// not an error here. A library set is chosen by exactly the rules of [`resolve_library_flevel`]
/// (call it with the same `source`, and both read the same set; a wrong or ambiguous set is the
/// same error), and the files are the set's entries `data/music_ogg/<name>.ogg`,
/// `data/music/<name>.wav`, `data/music/music.idx`, `data/sound/audio.dat` and
/// `data/sound/audio.fmt` (the paths `fmv-import` stores them at); a set imported before audio was
/// supported has none. A damaged blob (one that is missing, or of another size than its manifest
/// says) is the library's error, not a silent gap. A direct `flevel.lgp` has audio when its install
/// does, in the layout `field/flevel.lgp` + `music_ogg/`, `music/`, `sound/` as siblings (folder and
/// file names in any letter case; for a `lang-xx/field/flevel.lgp` the language folder's own
/// folders first, then the ones above it): only regular, non-empty files with plain names count
/// (the importer's name rule; unlike an import this reads the folder in place, so a link is followed
/// and there is no cap on the number of files). A `flevel.lgp` that does not sit in a folder named
/// `field` says nothing about where an install is, so it has no audio. Presentation only: never
/// sent over the network, part of no fingerprint.
pub fn resolve_audio(source: &Ff7Source) -> Result<ResolvedAudio, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => {
            let dir = resolve_state_dir(state_dir.as_deref())?;
            let lib = Library::open_existing(&dir).map_err(|e| match e {
                LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
                    dir: dir.display().to_string(),
                },
                other => Ff7ResolveError::Library(other),
            })?;
            let (set_id, manifest) = pick_set(&lib, set.as_deref())?;
            let mut out = ResolvedAudio::default();
            for entry in &manifest.entries {
                let Some(slot) = audio_slot_of_logical(&entry.logical_path) else {
                    continue;
                };
                if let Some(path) = lib.get(&set_id, &entry.logical_path)? {
                    out.fill(slot, path);
                }
            }
            Ok(out)
        }
        Ff7Source::Flevel { path, .. } => Ok(audio_beside_install(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::SetHandle;
    use std::fs;

    fn provenance(edition: &str, language: &str) -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: edition.into(),
            region: "unknown".into(),
            language: language.into(),
            revision: "rev1".into(),
            sources: vec![SourceRef::Install {
                path: "/somewhere/private".into(),
            }],
            modded: false,
        }
    }

    /// Import `files` (logical path, bytes) as a set with `p` into `lib`; returns the set id.
    fn make_set(lib: &Library, p: EditionProvenance, files: &[(&str, &[u8])], scratch: &Path) -> String {
        let handle = SetHandle::new(p);
        for (i, (logical, bytes)) in files.iter().enumerate() {
            let src = scratch.join(format!("src-{i}"));
            fs::write(&src, bytes).unwrap();
            lib.import_file(
                &handle,
                logical,
                &src,
                SourceRef::Install {
                    path: "/somewhere/private".into(),
                },
                1,
            )
            .unwrap();
        }
        handle.set_id
    }

    fn lib_source(dir: &Path, set: Option<&str>) -> Ff7Source {
        Ff7Source::Library {
            state_dir: Some(dir.to_path_buf()),
            set: set.map(str::to_string),
        }
    }

    #[test]
    fn the_steam_language_layout_wins_over_the_classic_one_and_blobs_are_found() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(
            &lib,
            provenance("Steam", "en"),
            &[
                ("data/field/flevel.lgp", b"classic"),
                ("data/lang-en/field/flevel.lgp", b"steam-language"),
            ],
            scratch.path(),
        );
        let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"steam-language");
        assert_eq!(r.origin, format!("library set {id}"));
        assert_eq!(r.provenance.edition, "Steam");
        // The provenance goes through unchanged, install path included (it never reaches the wire).
        assert!(r
            .provenance
            .sources
            .iter()
            .any(|s| matches!(s, SourceRef::Install { .. })));
        assert!(!format!("{:?}", r.provenance.summary()).contains("private"));
    }

    #[test]
    fn the_classic_layout_and_a_lone_flevel_elsewhere_are_found() {
        let scratch = tempfile::tempdir().unwrap();
        for (logical, body) in [
            ("data/field/flevel.lgp", &b"classic"[..]),
            ("data/odd/place/flevel.lgp", &b"odd"[..]),
        ] {
            let state = tempfile::tempdir().unwrap();
            let lib = Library::open(state.path()).unwrap();
            make_set(&lib, provenance("PC 1998", "en"), &[(logical, body)], scratch.path());
            let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
            assert_eq!(fs::read(&r.path).unwrap(), body, "{logical}");
        }
    }

    #[test]
    fn the_character_archive_comes_from_the_same_set_by_the_same_layout_rules() {
        let scratch = tempfile::tempdir().unwrap();
        // The Steam language directory wins over the classic layout, as for the field archive.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("Steam", "en"),
            &[
                ("data/field/flevel.lgp", b"field"),
                ("data/field/char.lgp", b"classic-char"),
                ("data/lang-en/field/char.lgp", b"steam-char"),
            ],
            scratch.path(),
        );
        let found = resolve_char_lgp(&lib_source(state.path(), None)).unwrap().expect("a char.lgp");
        assert_eq!(fs::read(found).unwrap(), b"steam-char");

        // The classic layout, and a lone char.lgp anywhere else, are found too.
        for (logical, body) in [("data/field/char.lgp", &b"classic"[..]), ("data/odd/place/char.lgp", &b"odd"[..])] {
            let state = tempfile::tempdir().unwrap();
            let lib = Library::open(state.path()).unwrap();
            make_set(&lib, provenance("PC 1998", "en"), &[("data/field/flevel.lgp", b"f"), (logical, body)], scratch.path());
            let found = resolve_char_lgp(&lib_source(state.path(), None)).unwrap().expect("a char.lgp");
            assert_eq!(fs::read(found).unwrap(), body, "{logical}");
        }

        // Two of them and no preferred place: never guessed, so no archive (markers, not a guess).
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"f"), ("data/a/char.lgp", b"a"), ("data/b/char.lgp", b"b")],
            scratch.path(),
        );
        assert_eq!(resolve_char_lgp(&lib_source(state.path(), None)).unwrap(), None);
    }

    #[test]
    fn a_set_without_character_models_has_none_and_set_errors_are_the_field_archives() {
        let scratch = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(&lib, provenance("Steam", "en"), &[("data/field/flevel.lgp", b"field")], scratch.path());
        // Not an error: the client then shows markers.
        assert_eq!(resolve_char_lgp(&lib_source(state.path(), None)).unwrap(), None);
        assert_eq!(resolve_char_lgp(&lib_source(state.path(), Some(&id))).unwrap(), None);

        // The same set choice as `resolve_flevel`: several sets need an explicit id, naming a set
        // picks it, and the char.lgp is that set's own.
        let other = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"f2"), ("data/field/char.lgp", b"other-char")],
            scratch.path(),
        );
        let err = resolve_char_lgp(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::AmbiguousSet(_)), "{err}");
        let found = resolve_char_lgp(&lib_source(state.path(), Some(&other))).unwrap().expect("char.lgp");
        assert_eq!(fs::read(found).unwrap(), b"other-char");
        assert_eq!(resolve_char_lgp(&lib_source(state.path(), Some(&id))).unwrap(), None);

        // No library at all: the same pointer at fmv-import, and nothing is created.
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_char_lgp(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(fs::read_dir(empty.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_direct_flevel_has_a_character_archive_only_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let flevel = dir.path().join("flevel.lgp");
        fs::write(&flevel, b"field").unwrap();
        let source = |path: &Path| Ff7Source::Flevel { path: path.to_path_buf(), labels: Ff7Labels::default() };
        assert_eq!(resolve_char_lgp(&source(&flevel)).unwrap(), None, "nothing beside it");
        // Any case of the name counts (installs differ), a directory of that name does not.
        fs::create_dir(dir.path().join("char.lgp")).unwrap();
        assert_eq!(resolve_char_lgp(&source(&flevel)).unwrap(), None);
        fs::write(dir.path().join("CHAR.LGP"), b"models").unwrap();
        let found = resolve_char_lgp(&source(&flevel)).unwrap().expect("sibling");
        assert_eq!(fs::read(found).unwrap(), b"models");
        // A path without a directory, or one that is not there: no archive, no error.
        assert_eq!(resolve_char_lgp(&source(Path::new("flevel.lgp"))).unwrap(), None);
        assert_eq!(resolve_char_lgp(&source(Path::new("/definitely/not/here/flevel.lgp"))).unwrap(), None);
    }

    #[test]
    fn several_flevels_in_one_set_without_a_preferred_location_are_not_guessed() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/a/flevel.lgp", b"a"), ("data/b/flevel.lgp", b"b")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFlevel(_)), "{err}");
    }

    #[test]
    fn missing_libraries_sets_and_archives_say_what_to_do() {
        // No library at all: the message points at fmv-import.
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_flevel(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(
            err.to_string().contains("needs owned FF7 content imported via fmv-import"),
            "{err}"
        );
        // Resolving never creates anything.
        assert!(fs::read_dir(empty.path()).unwrap().next().is_none());

        // A library without any FF7 set.
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFf7Set), "{err}");

        // An FF7 set that never got its field archive.
        let id = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/char.lgp", b"x")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFlevel(ref s) if *s == id), "{err}");

        // An explicit set id that does not exist, or is not a set id at all.
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), Some("ff7-pc-nope-00000000"))).unwrap_err(),
            Ff7ResolveError::Library(LibraryError::SetNotFound(_))
        ));
        assert!(resolve_flevel(&lib_source(state.path(), Some("../escape"))).is_err());
    }

    #[test]
    fn several_ff7_sets_need_an_explicit_choice_and_fixture_sets_are_never_picked_silently() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let a = make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/field/flevel.lgp", b"a")],
            scratch.path(),
        );
        let synthetic = EditionProvenance {
            platform: Platform::Synthetic,
            sources: vec![SourceRef::Fixture],
            edition: "synthetic".into(),
            ..provenance("x", "none")
        };
        let fixture = make_set(
            &lib,
            synthetic,
            &[("data/field/flevel.lgp", b"fixture")],
            scratch.path(),
        );

        // The synthetic set is ignored without an explicit id, so the real one is the only candidate.
        let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"a");
        // Naming it explicitly works (the server then labels the area FIXTURE).
        let r = resolve_flevel(&lib_source(state.path(), Some(&fixture))).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"fixture");

        // A second real set makes the choice ambiguous, and the error lists both.
        let b = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"b")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::AmbiguousSet(_)), "{err}");
        assert!(err.to_string().contains(&a) && err.to_string().contains(&b), "{err}");
        assert!(err.to_string().contains("--ff7-set"), "{err}");
        let r = resolve_flevel(&lib_source(state.path(), Some(&b))).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"b");
    }

    #[test]
    fn non_ff7_and_playstation_sets_are_refused_even_when_named() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let ps1 = EditionProvenance {
            platform: Platform::Ps1,
            ..provenance("PS1 original", "en")
        };
        let id = make_set(&lib, ps1, &[("data/field/flevel.lgp", b"x")], scratch.path());
        let err = resolve_flevel(&lib_source(state.path(), Some(&id))).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::UnusableSet { .. }), "{err}");
        assert!(err.to_string().contains("PlayStation"), "{err}");
        // ...and it is not a candidate for the automatic choice either.
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), None)).unwrap_err(),
            Ff7ResolveError::NoFf7Set
        ));

        let ff8 = EditionProvenance {
            game: GameId::Ff8,
            ..provenance("FF8", "en")
        };
        let id = make_set(&lib, ff8, &[("data/field/flevel.lgp", b"x")], scratch.path());
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), Some(&id))).unwrap_err(),
            Ff7ResolveError::UnusableSet { .. }
        ));
    }

    #[test]
    fn a_direct_flevel_carries_the_operators_labels_and_is_not_synthetic() {
        let labels = Ff7Labels {
            edition: "Steam".into(),
            region: "unknown".into(),
            language: "en".into(),
            revision: "rev1".into(),
        };
        let r = resolve_flevel(&Ff7Source::Flevel {
            path: "/x/flevel.lgp".into(),
            labels: labels.clone(),
        })
        .unwrap();
        assert_eq!(r.path, Path::new("/x/flevel.lgp"));
        assert_eq!(r.provenance.game, GameId::Ff7);
        assert_eq!(r.provenance.platform, Platform::Pc);
        assert_eq!((r.provenance.edition.as_str(), r.provenance.revision.as_str()), ("Steam", "rev1"));
        assert_eq!(r.origin, "direct flevel.lgp");

        // Defaults are `unknown`, which never matches a labelled library set.
        let d = resolve_flevel(&Ff7Source::Flevel {
            path: "/x".into(),
            labels: Ff7Labels::default(),
        })
        .unwrap();
        assert!(!d.provenance.compatible_with(&r.provenance));
        assert_eq!(d.provenance.edition, "unknown");
    }

    #[test]
    fn a_library_set_and_a_direct_flevel_with_its_labels_are_compatible_peers() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/field/flevel.lgp", b"a")],
            scratch.path(),
        );
        let from_library = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        let direct = resolve_flevel(&Ff7Source::Flevel {
            path: "/anywhere/flevel.lgp".into(),
            labels: Ff7Labels {
                edition: "Steam".into(),
                region: "unknown".into(),
                language: "en".into(),
                revision: "rev1".into(),
            },
        })
        .unwrap();
        assert!(from_library.provenance.compatible_with(&direct.provenance));
    }

    #[test]
    fn battle_files_follow_the_same_set_and_layout_rules_as_the_field_archive() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(
            &lib,
            provenance("Steam", "en"),
            &[
                ("data/field/flevel.lgp", b"field"),
                ("data/battle/scene.bin", b"classic-scene"),
                ("data/lang-en/battle/scene.bin", b"steam-scene"),
                ("data/lang-en/kernel/kernel.bin", b"steam-kernel"),
                ("data/lang-en/kernel/kernel2.bin", b"text"),
            ],
            scratch.path(),
        );
        let r = resolve_battle_files(&lib_source(state.path(), None)).unwrap();
        // The language directory wins over the classic layout, and kernel2.bin is not kernel.bin.
        assert_eq!(fs::read(&r.scene).unwrap(), b"steam-scene");
        assert_eq!(fs::read(&r.kernel).unwrap(), b"steam-kernel");
        assert_eq!(r.origin, format!("library set {id}"));
        assert_eq!(r.provenance.edition, "Steam");
        // It is the very set the field archive comes from.
        let field = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        assert_eq!(field.origin, r.origin);
        assert!(!r.origin.contains("private"));
    }

    #[test]
    fn battle_files_in_the_classic_layout_or_alone_elsewhere_are_found() {
        let scratch = tempfile::tempdir().unwrap();
        for (scene, kernel) in [
            ("data/battle/scene.bin", "data/kernel/kernel.bin"),
            ("data/odd/scene.bin", "data/else/where/kernel.bin"),
        ] {
            let state = tempfile::tempdir().unwrap();
            let lib = Library::open(state.path()).unwrap();
            make_set(
                &lib,
                provenance("PC 1998", "en"),
                &[(scene, b"s"), (kernel, b"k"), ("data/field/flevel.lgp", b"f")],
                scratch.path(),
            );
            let r = resolve_battle_files(&lib_source(state.path(), None)).unwrap();
            assert_eq!((fs::read(&r.scene).unwrap(), fs::read(&r.kernel).unwrap()), (b"s".to_vec(), b"k".to_vec()), "{scene}");
        }
    }

    #[test]
    fn missing_or_ambiguous_battle_files_name_what_is_missing() {
        let scratch = tempfile::tempdir().unwrap();
        // No kernel.bin.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/lang-en/battle/scene.bin", b"s"), ("data/field/flevel.lgp", b"f")],
            scratch.path(),
        );
        let err = resolve_battle_files(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoBattleFile { ref set, file: "kernel.bin" } if *set == id), "{err}");
        assert!(err.to_string().contains("kernel.bin"), "{err}");

        // Two scene.bin files and none in a preferred place: never guessed.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/a/scene.bin", b"a"), ("data/b/scene.bin", b"b"), ("data/lang-en/kernel/kernel.bin", b"k")],
            scratch.path(),
        );
        let err = resolve_battle_files(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoBattleFile { file: "scene.bin", .. }), "{err}");

        // No library at all points at fmv-import, and nothing is created.
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_battle_files(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(fs::read_dir(empty.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_direct_flevel_has_no_battle_files() {
        let err = resolve_battle_files(&Ff7Source::Flevel { path: "/x/flevel.lgp".into(), labels: Ff7Labels::default() }).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::BattleNeedsLibrary), "{err}");
        assert!(err.to_string().contains("library"), "{err}");
    }

    #[test]
    fn the_battle_model_archive_comes_from_the_same_set_by_the_same_layout_rules() {
        let scratch = tempfile::tempdir().unwrap();
        // The Steam language directory wins over the classic layout, as for the field archive.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("Steam", "en"),
            &[
                ("data/field/flevel.lgp", b"field"),
                ("data/battle/battle.lgp", b"classic-battle"),
                ("data/lang-en/battle/battle.lgp", b"steam-battle"),
                ("data/battle/scene.bin", b"not the model archive"),
            ],
            scratch.path(),
        );
        let found = resolve_battle_lgp(&lib_source(state.path(), None)).unwrap().expect("a battle.lgp");
        assert_eq!(fs::read(found).unwrap(), b"steam-battle");

        // The classic layout, and a lone battle.lgp anywhere else, are found too (the real Steam
        // library keeps it in `data/battle/`).
        for (logical, body) in [("data/battle/battle.lgp", &b"classic"[..]), ("data/odd/place/battle.lgp", &b"odd"[..])] {
            let state = tempfile::tempdir().unwrap();
            let lib = Library::open(state.path()).unwrap();
            make_set(&lib, provenance("PC 1998", "en"), &[("data/field/flevel.lgp", b"f"), (logical, body)], scratch.path());
            let found = resolve_battle_lgp(&lib_source(state.path(), None)).unwrap().expect("a battle.lgp");
            assert_eq!(fs::read(found).unwrap(), body, "{logical}");
        }

        // Two of them and no preferred place: never guessed, so no archive.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"f"), ("data/a/battle.lgp", b"a"), ("data/b/battle.lgp", b"b")],
            scratch.path(),
        );
        assert_eq!(resolve_battle_lgp(&lib_source(state.path(), None)).unwrap(), None);

        // A set without one has none (not an error); the set choice and the missing-library error
        // are the field archive's.
        let state = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(&lib, provenance("Steam", "en"), &[("data/field/flevel.lgp", b"field")], scratch.path());
        assert_eq!(resolve_battle_lgp(&lib_source(state.path(), None)).unwrap(), None);
        assert_eq!(resolve_battle_lgp(&lib_source(state.path(), Some(&id))).unwrap(), None);
        let other = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"f2"), ("data/battle/battle.lgp", b"other-battle")],
            scratch.path(),
        );
        let err = resolve_battle_lgp(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::AmbiguousSet(_)), "{err}");
        let found = resolve_battle_lgp(&lib_source(state.path(), Some(&other))).unwrap().expect("battle.lgp");
        assert_eq!(fs::read(found).unwrap(), b"other-battle");
        assert_eq!(resolve_battle_lgp(&lib_source(state.path(), Some(&id))).unwrap(), None);
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_battle_lgp(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(fs::read_dir(empty.path()).unwrap().next().is_none(), "nothing is created");
    }

    #[test]
    fn a_direct_flevel_finds_the_battle_archive_in_its_installs_battle_folder_not_beside_it() {
        let source = |path: &Path| Ff7Source::Flevel { path: path.to_path_buf(), labels: Ff7Labels::default() };
        let install = tempfile::tempdir().unwrap();
        let data = install.path().join("data");
        fs::create_dir_all(data.join("field")).unwrap();
        let flevel = data.join("field").join("flevel.lgp");
        fs::write(&flevel, b"field").unwrap();
        assert_eq!(resolve_battle_lgp(&source(&flevel)).unwrap(), None, "no battle folder");

        // A battle.lgp *beside* the flevel is not the model archive's place.
        fs::write(data.join("field").join("battle.lgp"), b"beside").unwrap();
        assert_eq!(resolve_battle_lgp(&source(&flevel)).unwrap(), None);

        // `data/battle/battle.lgp`, in any case for the folder and the file; a directory of the file's name does not count.
        fs::create_dir_all(data.join("Battle").join("battle.lgp")).unwrap();
        assert_eq!(resolve_battle_lgp(&source(&flevel)).unwrap(), None);
        fs::write(data.join("Battle").join("BATTLE.LGP"), b"models").unwrap();
        let found = resolve_battle_lgp(&source(&flevel)).unwrap().expect("install layout");
        assert_eq!(fs::read(found).unwrap(), b"models");

        // A language directory's field folder: its own battle folder first, then the one above.
        let steam = tempfile::tempdir().unwrap();
        let lang_field = steam.path().join("data").join("lang-en").join("field");
        fs::create_dir_all(&lang_field).unwrap();
        let lang_flevel = lang_field.join("flevel.lgp");
        fs::write(&lang_flevel, b"field").unwrap();
        assert_eq!(resolve_battle_lgp(&source(&lang_flevel)).unwrap(), None);
        fs::create_dir_all(steam.path().join("data").join("battle")).unwrap();
        fs::write(steam.path().join("data").join("battle").join("battle.lgp"), b"above").unwrap();
        let found = resolve_battle_lgp(&source(&lang_flevel)).unwrap().expect("the data folder's battle");
        assert_eq!(fs::read(found).unwrap(), b"above");
        fs::create_dir_all(steam.path().join("data").join("lang-en").join("battle")).unwrap();
        fs::write(steam.path().join("data").join("lang-en").join("battle").join("battle.lgp"), b"own").unwrap();
        let found = resolve_battle_lgp(&source(&lang_flevel)).unwrap().expect("the language folder's battle");
        assert_eq!(fs::read(found).unwrap(), b"own");

        // A path without a directory, one without an install above it, or one that is not there:
        // no archive, no error.
        assert_eq!(resolve_battle_lgp(&source(Path::new("flevel.lgp"))).unwrap(), None);
        assert_eq!(resolve_battle_lgp(&source(Path::new("/flevel.lgp"))).unwrap(), None);
        assert_eq!(resolve_battle_lgp(&source(Path::new("field/flevel.lgp"))).unwrap(), None);
        assert_eq!(resolve_battle_lgp(&source(Path::new("/definitely/not/here/field/flevel.lgp"))).unwrap(), None);
    }

    // -----------------------------------------------------------------------------------------
    // audio
    // -----------------------------------------------------------------------------------------

    /// The five kinds of audio file as `fmv-import` stores them.
    const AUDIO_FILES: [(&str, &[u8]); 7] = [
        ("data/music_ogg/bat.ogg", b"ogg-bat"),
        ("data/music_ogg/victory.ogg", b"ogg-victory"),
        ("data/music/bat.wav", b"wav-bat"),
        ("data/music/heart.wav", b"wav-heart"),
        ("data/music/music.idx", b"idx"),
        ("data/sound/audio.dat", b"sfx-dat"),
        ("data/sound/audio.fmt", b"sfx-fmt"),
    ];

    fn read(p: &Path) -> Vec<u8> {
        fs::read(p).unwrap()
    }

    fn read_opt(p: &Option<PathBuf>) -> Vec<u8> {
        read(p.as_deref().expect("a file"))
    }

    #[test]
    fn the_audio_of_a_library_set_is_found_by_the_importers_paths_and_nothing_else_counts() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let mut files: Vec<(&str, &[u8])> = vec![("data/field/flevel.lgp", b"field")];
        files.extend(AUDIO_FILES);
        // look-alikes that are not audio of the set: another language folder, a deeper folder, a
        // wrong extension in the right folder, an ogg in the wav folder, a file called like a folder
        files.extend([
            ("data/lang-en/music_ogg/lang.ogg", &b"lang"[..]),
            ("data/music_ogg/sub/deep.ogg", b"deep"),
            ("data/music_ogg/readme.txt", b"text"),
            ("data/music_ogg/.ogg", b"no stem"),
            ("data/music/clip.ogg", b"ogg in the wav folder"),
            ("data/music_ogg/bat.wav", b"wav in the ogg folder"),
            ("data/sound/other.dat", b"not audio.dat"),
            ("data/music.idx", b"not in the folder"),
        ]);
        let id = make_set(&lib, provenance("Steam", "en"), &files, scratch.path());

        let audio = resolve_audio(&lib_source(state.path(), None)).unwrap();
        assert!(!audio.is_empty());
        assert_eq!(
            audio.music_ogg.keys().collect::<Vec<_>>(),
            ["bat", "victory"]
        );
        assert_eq!(audio.music_wav.keys().collect::<Vec<_>>(), ["bat", "heart"]);
        assert_eq!(read(&audio.music_ogg["bat"]), b"ogg-bat");
        assert_eq!(read(&audio.music_ogg["victory"]), b"ogg-victory");
        assert_eq!(read(&audio.music_wav["heart"]), b"wav-heart");
        assert_eq!(read_opt(&audio.music_idx), b"idx");
        assert_eq!(read_opt(&audio.sfx_dat), b"sfx-dat");
        assert_eq!(read_opt(&audio.sfx_fmt), b"sfx-fmt");
        // the paths are the library's blobs: the same ones `Library::get` gives
        assert_eq!(
            audio.music_ogg["bat"],
            lib.get(&id, "data/music_ogg/bat.ogg").unwrap().unwrap()
        );
        // an explicit set id reads the same set
        let named = resolve_audio(&lib_source(state.path(), Some(&id))).unwrap();
        assert_eq!(named.music_ogg, audio.music_ogg);

        // a track by name: the ogg first, else the wav; any letter case
        assert_eq!(read(audio.music_file("bat").unwrap()), b"ogg-bat");
        assert_eq!(read(audio.music_file("BAT").unwrap()), b"ogg-bat");
        assert_eq!(read(audio.music_file("Victory").unwrap()), b"ogg-victory");
        assert_eq!(read(audio.music_file("HEART").unwrap()), b"wav-heart");
        assert_eq!(audio.music_file("clip"), None);
        assert_eq!(audio.music_file(""), None);
    }

    #[test]
    fn a_library_without_audio_resolves_to_nothing_and_the_set_errors_are_the_field_archives() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        // a library that holds no FF7 set at all
        let err = resolve_audio(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFf7Set), "{err}");

        // a set from before audio was imported: no error, nothing found
        let id = make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/field/flevel.lgp", b"field")],
            scratch.path(),
        );
        let none = resolve_audio(&lib_source(state.path(), None)).unwrap();
        assert!(none.is_empty());
        assert!(none.music_file("bat").is_none());
        assert!(resolve_audio(&lib_source(state.path(), Some(&id)))
            .unwrap()
            .is_empty());

        // a second real set: the choice is ambiguous exactly as for the field archive, and an
        // explicit id picks that set's own audio
        let mut with_audio: Vec<(&str, &[u8])> = vec![("data/field/flevel.lgp", b"f2")];
        with_audio.extend(AUDIO_FILES);
        let other = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &with_audio,
            scratch.path(),
        );
        let err = resolve_audio(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::AmbiguousSet(_)), "{err}");
        assert!(
            err.to_string().contains(&id) && err.to_string().contains(&other),
            "{err}"
        );
        assert!(err.to_string().contains("--ff7-set"), "{err}");
        let found = resolve_audio(&lib_source(state.path(), Some(&other))).unwrap();
        assert_eq!(read_opt(&found.sfx_dat), b"sfx-dat");
        assert!(resolve_audio(&lib_source(state.path(), Some(&id)))
            .unwrap()
            .is_empty());

        // PlayStation media and other games are refused when named, like the field archive
        let ps1 = EditionProvenance {
            platform: Platform::Ps1,
            ..provenance("PS1 original", "en")
        };
        let ps1_id = make_set(&lib, ps1, &[("data/sound/audio.dat", b"x")], scratch.path());
        let err = resolve_audio(&lib_source(state.path(), Some(&ps1_id))).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::UnusableSet { .. }), "{err}");

        // a set that is not there, and one that is not a set id
        assert!(matches!(
            resolve_audio(&lib_source(state.path(), Some("ff7-pc-nope-00000000"))).unwrap_err(),
            Ff7ResolveError::Library(LibraryError::SetNotFound(_))
        ));
        assert!(resolve_audio(&lib_source(state.path(), Some("../escape"))).is_err());
    }

    #[test]
    fn no_library_points_at_fmv_import_and_resolving_audio_creates_nothing() {
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_audio(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(err.to_string().contains("fmv-import"), "{err}");
        assert!(
            fs::read_dir(empty.path()).unwrap().next().is_none(),
            "nothing is created"
        );
        // a directory that is not there at all
        let missing = empty.path().join("not-yet");
        assert!(matches!(
            resolve_audio(&lib_source(&missing, None)).unwrap_err(),
            Ff7ResolveError::NoLibrary { .. }
        ));
        assert!(!missing.exists());
    }

    #[test]
    fn a_damaged_audio_blob_is_the_librarys_error_and_not_a_silent_gap() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let mut files: Vec<(&str, &[u8])> = vec![("data/field/flevel.lgp", b"field")];
        files.extend(AUDIO_FILES);
        let id = make_set(&lib, provenance("Steam", "en"), &files, scratch.path());
        let blob = lib.get(&id, "data/sound/audio.dat").unwrap().unwrap();
        fs::remove_file(blob).unwrap();
        let err = resolve_audio(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::Library(_)), "{err}");
    }

    #[test]
    fn music_by_name_prefers_the_ogg_and_ignores_letter_case() {
        let mut audio = ResolvedAudio::default();
        assert!(audio.is_empty());
        audio
            .music_wav
            .insert("tune".into(), PathBuf::from("/x/tune.wav"));
        assert!(!audio.is_empty());
        assert_eq!(audio.music_file("TUNE"), Some(Path::new("/x/tune.wav")));
        audio
            .music_ogg
            .insert("tune".into(), PathBuf::from("/x/tune.ogg"));
        assert_eq!(audio.music_file("Tune"), Some(Path::new("/x/tune.ogg")));
        assert_eq!(
            audio.music_file("tune.ogg"),
            None,
            "a name, not a file name"
        );
        // each of the single files alone makes it non-empty
        for fill in [
            |a: &mut ResolvedAudio| a.music_idx = Some("/x".into()),
            |a: &mut ResolvedAudio| a.sfx_dat = Some("/x".into()),
            |a: &mut ResolvedAudio| a.sfx_fmt = Some("/x".into()),
        ] {
            let mut a = ResolvedAudio::default();
            fill(&mut a);
            assert!(!a.is_empty());
        }
    }

    /// An install in the layout of the real game, with every name in odd letter cases.
    fn odd_case_install(root: &Path) -> PathBuf {
        let data = root.join("Data");
        for (rel, body) in [
            ("Field/FLEVEL.LGP", &b"field"[..]),
            ("MUSIC_OGG/BAT.OGG", b"ogg-bat"),
            ("MUSIC_OGG/Victory.Ogg", b"ogg-victory"),
            ("Music/Heart.WAV", b"wav-heart"),
            ("Music/MUSIC.IDX", b"idx"),
            ("SOUND/Audio.Dat", b"sfx-dat"),
            ("SOUND/AUDIO.fmt", b"sfx-fmt"),
        ] {
            let p = data.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        data.join("Field").join("FLEVEL.LGP")
    }

    fn direct(path: &Path) -> Ff7Source {
        Ff7Source::Flevel {
            path: path.to_path_buf(),
            labels: Ff7Labels::default(),
        }
    }

    #[test]
    fn a_direct_flevel_finds_the_audio_in_its_installs_folders_in_any_letter_case() {
        let install = tempfile::tempdir().unwrap();
        let flevel = odd_case_install(install.path());
        let audio = resolve_audio(&direct(&flevel)).unwrap();
        assert_eq!(
            audio.music_ogg.keys().collect::<Vec<_>>(),
            ["bat", "victory"]
        );
        assert_eq!(audio.music_wav.keys().collect::<Vec<_>>(), ["heart"]);
        assert_eq!(read(&audio.music_ogg["bat"]), b"ogg-bat");
        assert_eq!(read(&audio.music_ogg["victory"]), b"ogg-victory");
        assert_eq!(read(&audio.music_wav["heart"]), b"wav-heart");
        assert_eq!(read_opt(&audio.music_idx), b"idx");
        assert_eq!(read_opt(&audio.sfx_dat), b"sfx-dat");
        assert_eq!(read_opt(&audio.sfx_fmt), b"sfx-fmt");
        // the files are the install's own (read-only: nothing is copied or created)
        assert!(audio.music_ogg["bat"].starts_with(install.path()));
        assert_eq!(read(audio.music_file("VICTORY").unwrap()), b"ogg-victory");
        // the field folder may be spelled any way too
        let other = tempfile::tempdir().unwrap();
        let moved = other.path().join("data").join("FIELD");
        fs::create_dir_all(&moved).unwrap();
        fs::create_dir_all(other.path().join("data").join("sound")).unwrap();
        fs::write(
            other.path().join("data").join("sound").join("audio.dat"),
            b"d",
        )
        .unwrap();
        assert!(resolve_audio(&direct(&moved.join("flevel.lgp")))
            .unwrap()
            .sfx_dat
            .is_some());
    }

    #[test]
    fn a_direct_folder_is_read_by_the_importers_rules() {
        let install = tempfile::tempdir().unwrap();
        let flevel = odd_case_install(install.path());
        let data = install.path().join("Data");
        // what an import would not take is not found either: empty files, folders that carry a
        // file's name, names that are not plain, other extensions, files in the wrong folder
        fs::write(data.join("MUSIC_OGG").join("empty.ogg"), b"").unwrap();
        fs::create_dir(data.join("MUSIC_OGG").join("folder.ogg")).unwrap();
        fs::write(data.join("MUSIC_OGG").join("odd name.ogg"), b"x").unwrap();
        fs::write(data.join("MUSIC_OGG").join(".hidden.ogg"), b"x").unwrap();
        fs::write(data.join("MUSIC_OGG").join("note.txt"), b"x").unwrap();
        fs::write(data.join("MUSIC_OGG").join("wrong.wav"), b"x").unwrap();
        fs::write(data.join("Music").join("wrong.ogg"), b"x").unwrap();
        fs::write(data.join("SOUND").join("other.dat"), b"x").unwrap();
        let audio = resolve_audio(&direct(&flevel)).unwrap();
        assert_eq!(
            audio.music_ogg.keys().collect::<Vec<_>>(),
            ["bat", "victory"]
        );
        assert_eq!(audio.music_wav.keys().collect::<Vec<_>>(), ["heart"]);
        // an empty index file is not an index
        fs::write(data.join("Music").join("MUSIC.IDX"), b"").unwrap();
        assert!(resolve_audio(&direct(&flevel)).unwrap().music_idx.is_none());
        // two spellings of one name: always the same one
        fs::write(
            data.join("MUSIC_OGG").join("bat.ogg"),
            b"lower-case spelling",
        )
        .unwrap();
        if fs::read_dir(data.join("MUSIC_OGG"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("bat.ogg")
            })
            .count()
            == 2
        {
            let first = resolve_audio(&direct(&flevel)).unwrap();
            let second = resolve_audio(&direct(&flevel)).unwrap();
            assert_eq!(first.music_ogg["bat"], second.music_ogg["bat"]);
            assert!(
                first.music_ogg["bat"].ends_with("BAT.OGG"),
                "upper case sorts first: {:?}",
                first.music_ogg["bat"]
            );
        }
    }

    #[test]
    fn a_direct_flevel_in_a_language_folder_looks_there_first_and_then_above() {
        let steam = tempfile::tempdir().unwrap();
        let data = steam.path().join("data");
        let lang_field = data.join("lang-en").join("field");
        fs::create_dir_all(&lang_field).unwrap();
        let flevel = lang_field.join("flevel.lgp");
        fs::write(&flevel, b"field").unwrap();
        assert!(
            resolve_audio(&direct(&flevel)).unwrap().is_empty(),
            "no audio anywhere"
        );

        // the audio of the data folder, above the language folder
        fs::create_dir_all(data.join("music_ogg")).unwrap();
        fs::write(data.join("music_ogg").join("bat.ogg"), b"above").unwrap();
        fs::create_dir_all(data.join("sound")).unwrap();
        fs::write(data.join("sound").join("audio.dat"), b"above-dat").unwrap();
        let audio = resolve_audio(&direct(&flevel)).unwrap();
        assert_eq!(read(&audio.music_ogg["bat"]), b"above");
        assert_eq!(read_opt(&audio.sfx_dat), b"above-dat");

        // the language folder's own audio wins, whole: nothing is mixed in from above
        fs::create_dir_all(data.join("lang-en").join("music_ogg")).unwrap();
        fs::write(
            data.join("lang-en").join("music_ogg").join("bat.ogg"),
            b"own",
        )
        .unwrap();
        let audio = resolve_audio(&direct(&flevel)).unwrap();
        assert_eq!(read(&audio.music_ogg["bat"]), b"own");
        assert!(audio.sfx_dat.is_none(), "the folder above is not mixed in");
    }

    #[test]
    fn a_direct_flevel_that_is_not_in_a_field_folder_says_nothing_about_audio() {
        // a field archive somebody put into a download folder, with a music folder next to it
        let home = tempfile::tempdir().unwrap();
        let downloads = home.path().join("Downloads");
        fs::create_dir_all(&downloads).unwrap();
        fs::write(downloads.join("flevel.lgp"), b"field").unwrap();
        fs::create_dir_all(home.path().join("Music")).unwrap();
        fs::write(
            home.path().join("Music").join("song.wav"),
            b"a song of the user's",
        )
        .unwrap();
        assert!(resolve_audio(&direct(&downloads.join("flevel.lgp")))
            .unwrap()
            .is_empty());

        // a field folder without audio around it, and a data folder without those folders
        let install = tempfile::tempdir().unwrap();
        fs::create_dir_all(install.path().join("data").join("field")).unwrap();
        fs::create_dir_all(install.path().join("data").join("battle")).unwrap();
        let flevel = install.path().join("data").join("field").join("flevel.lgp");
        fs::write(&flevel, b"field").unwrap();
        assert!(resolve_audio(&direct(&flevel)).unwrap().is_empty());

        // a path without a directory, one without an install above it, one that is not there
        for p in [
            "flevel.lgp",
            "/flevel.lgp",
            "field/flevel.lgp",
            "/definitely/not/here/field/flevel.lgp",
        ] {
            assert!(
                resolve_audio(&direct(Path::new(p))).unwrap().is_empty(),
                "{p}"
            );
        }
    }
}
