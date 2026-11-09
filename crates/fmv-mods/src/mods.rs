//! The opened mod layers and the lookups the client builds on.
//!
//! # The layers
//!
//! The owner's architecture: *the accurate base game comes first; the mods are layers over it and each can be
//! switched off.* [`Mods`] holds the opened archives and four independent switches ([`Layers`], all on by default):
//!
//! | Layer | Source | Replaces | Switched off |
//! |---|---|---|---|
//! | **text** | Echo-S's replacement field scripts | the field's script section (its dialog text and the flow of its conversations) | the original 1997 script |
//! | **voice** | Echo-S's field voices | nothing: the original has no voices | silence |
//! | **music** | Echo-S's "Echoes of the Planet" tracks | the original track of the same `music.idx` name | the original music |
//! | **sound** | Cosmo Memory's sound effects (Echo-S's own set after it) | the original `audio.dat` clip of the same sound id | the original clips |
//!
//! Every query below answers `None` when its layer is off, when no archive for it is configured and when the
//! archive has nothing for the asked name: **the caller then plays the original**, so nothing a layer lacks is ever
//! silent or blank.
//!
//! # Precedence, as 7th Heaven does it
//!
//! 7th Heaven maps each file name to the candidate files of every active mod folder in load order and serves the
//! **first** whose folder is active (`AppWrapper/VFile.cs`, `LGPWrapper.MapFile`, lines 291 to 302; the candidate list is
//! built in `Wrap.cs` lines 173 to 199, mods in profile order, a mod's conditional folders first, then its `ModFolder`s;
//! tsunamods-codes/7th-Heaven pinned to `f0d7da92beac69e21c59995fa13ea1b2b0e3cf1f`). Cosmo's `mod.xml` declares it
//! `Before` Echo-S, so Cosmo's files win where both have one, and there is one `sfx/config.toml` (Cosmo's). This crate does
//! the same: the sound layer's folders are searched Cosmo first, then Echo-S, and the section map is Cosmo's when Cosmo is
//! there. Where this crate chooses and 7th Heaven would ask the player, the choice is named in the docs of the method.
//!
//! # Why the text layer swaps the script section, not a line table
//!
//! Measured on the owner's FF7 Steam Edition against Echo-S 1.24's `Echo-S\flevel.lgp` (counts only; the examples
//! `echo_script_compare` and `echo_talk_compare` of this crate print them again for any copy): Echo-S has an entry for all 702
//! fields, and its script section is a **different script**, not the original with new strings:
//!
//! * the dialog strings go from 56 048 to 13 819 (the original tables are mostly leftovers that nothing opens: 114 strings in each
//!   of the first reactor fields against the 5 to 21 Echo-S keeps), while the lines the scripts do open are about as many
//!   (12 526 distinct window-and-dialog pairs in the original's, 12 613 in Echo-S's);
//! * 271 161 of the 328 675 scripts of the entities both have (82.5%) are the same to the dialogue walker, the rest are not;
//! * the dialog **numbers** change: of the 12 475 numbers the original's scripts open, 8 326 sit in a script that is the same in
//!   Echo-S's and 4 934 of those (59%) are another number there (the tables were compacted); the other 4 149 sit in scripts
//!   that differ;
//! * the conversations differ: at the start of a new game 1 421 characters can be talked to in both, 1 320 of them with the same
//!   number of windows and 101 with another (1 350 windows in the original, 1 480 in Echo-S's);
//! * Echo-S's section has 10 272 of the original's 10 534 entities by name (usually one more, appended), 2 302 of the 2 412
//!   characters of the original are found in it by the client's name-and-place rule, and 84 of its 360 512 scripts do not
//!   decode (the original's 337 059 all do).
//!
//! So `(field, original dialog number) -> text` cannot be built, and the voice files are named by Echo-S's *own* numbers.
//! The unit that is coherent is the section: text, flow and voice names all belong to Echo-S's section, which is also what
//! 7th Heaven does (it replaces the whole `flevel.lgp` entry). [`Mods::script_section`] gives that section; the client runs
//! its conversation walker on it for a character it finds there by name and place, and on the original section otherwise.

use crate::echo;
use crate::ffnx::{Entry, LayerConfig, Mode, Pick, Picker};
use crate::iro::{IroArchive, IroEntry};
use crate::modxml::{self, ModInfo, COSMO_ID, ECHO_S_ID, MAX_MOD_XML};
use fmv_ff7::field::{section, FieldFile};
use fmv_ff7::lzss;
use fmv_ff7::script::FieldScript;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The biggest voice, sound or script entry read (a real voice clip is under 1 MB).
pub const MAX_CLIP_BYTES: u64 = 32 * 1024 * 1024;
/// The biggest music track read (the largest of Echo-S's is 6.6 MB).
pub const MAX_TRACK_BYTES: u64 = 128 * 1024 * 1024;
/// The most `*.iro` files looked at in a mods folder.
const MAX_CANDIDATES: usize = 64;
/// Script sections kept in memory (a field's is at most 60 KB).
const SCRIPT_CACHE: usize = 48;

/// Which layers are on. Every one is on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layers {
    /// Echo-S's script as the dialog text (off: the original script).
    pub text: bool,
    /// Echo-S's voices (off: no voices, and the plain attack sounds instead of the voiced ones).
    pub voice: bool,
    /// Echo-S's music (off: the original music).
    pub music: bool,
    /// Cosmo Memory's sound effects (off: the original sounds).
    pub sound: bool,
}

impl Layers {
    /// Everything on (the default).
    pub const ALL: Self = Self { text: true, voice: true, music: true, sound: true };
    /// Everything off: the base game exactly.
    pub const NONE: Self = Self { text: false, voice: false, music: false, sound: false };
}

impl Default for Layers {
    fn default() -> Self {
        Self::ALL
    }
}

/// Where the archives are to be found. Nothing here is ever created or written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModSources {
    /// A folder holding the `.iro` files, or folders that do (one level down); each archive is told apart by the ID in its own
    /// `mod.xml`, never by its file name.
    pub dir: Option<PathBuf>,
    /// The Echo-S archive itself, when it is not in `dir`.
    pub echo_s: Option<PathBuf>,
    /// The Cosmo Memory archive itself, when it is not in `dir`.
    pub cosmo: Option<PathBuf>,
}

/// An archive that is open and has said which mod it is.
#[derive(Clone)]
struct Opened {
    archive: Arc<IroArchive>,
    info: ModInfo,
}

impl Opened {
    fn bytes(&self, path: &str, limit: u64) -> Option<Vec<u8>> {
        let entry = self.archive.find(path)?;
        self.archive.read_limited(entry, limit).ok()
    }

    /// The short name of the mod this archive is (`echo-s` or `cosmo`), for logs.
    fn label(&self) -> &'static str {
        if self.info.id == ECHO_S_ID {
            "echo-s"
        } else {
            "cosmo"
        }
    }
}

/// The two layers' archives as discovery finds them.
#[derive(Default)]
struct Slots {
    echo: Option<Opened>,
    cosmo: Option<Opened>,
}

impl Slots {
    /// Keep `found` in the slot of the mod it says it is (the first of each wins). `explicit` names the slot an archive was given for:
    /// one that is another mod is refused.
    fn place(&mut self, found: Opened, notes: &mut Vec<String>, explicit: Option<&str>) {
        let which = match found.info.id.as_str() {
            ECHO_S_ID => "echo-s",
            COSMO_ID => "cosmo",
            _ => {
                notes.push(format!("ignored an archive that is neither Echo-S nor Cosmo Memory ({:?})", found.info.name));
                return;
            }
        };
        if let Some(want) = explicit.filter(|want| *want != which) {
            notes.push(format!("the archive given for {want} is {:?}", found.info.name));
            return;
        }
        let slot = if which == "echo-s" { &mut self.echo } else { &mut self.cosmo };
        if slot.is_none() {
            *slot = Some(found);
        }
    }
}

/// A clip of sound read from a layer: its path in the archive and its bytes (Ogg Vorbis in everything in use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// The path inside the archive (a name for logs; never a local path).
    pub name: String,
    pub bytes: Vec<u8>,
    /// Which mod's archive it came from: `echo-s` or `cosmo`.
    pub source: &'static str,
}

/// The clips a sound id plays instead of the original, from the layer's `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfxSet {
    /// The config section that named them (`6`, or `<field>_<id>`), for logs.
    pub section: String,
    /// How a repeating sound moves through `clips`: in turn, or at random.
    pub mode: Mode,
    /// The clips, in the section's order (one or several).
    pub clips: Vec<Clip>,
}

/// A script section read from Echo-S's archive (or `None`: it has none for that field, or it does not decode), by field name.
type CachedSection = (String, Option<Arc<Vec<u8>>>);

struct Shared {
    scripts: Mutex<VecDeque<CachedSection>>,
    /// The sound layer's section map: Cosmo's `Base\sfx\config.toml`, else Echo-S's `Sound\sfx\config.toml`.
    sfx_config: LayerConfig,
    /// The music layer's section map (`bat`, `bee` and `gun` shuffle among variants).
    music_config: LayerConfig,
    picker: Picker,
}

/// The opened layers. Cheap to clone (the archives are shared); [`Mods::with_layers`] changes the switches only.
#[derive(Clone)]
pub struct Mods {
    echo: Option<Opened>,
    cosmo: Option<Opened>,
    layers: Layers,
    notes: Arc<Vec<String>>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Mods {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mods").field("layers", &self.layers).field("echo", &self.echo.is_some()).field("cosmo", &self.cosmo.is_some()).finish()
    }
}

/// What a source of field scripts gives the dialogue and scene code: the script section to run in place of the original's.
pub trait ScriptSource: Send + Sync {
    /// Which source this is, for logs (`echo-s 1.24`).
    fn label(&self) -> String;

    /// Section 0 of the replacement field `field` (what `FieldScript::parse` takes), or `None`: the layer is off, the field is
    /// not in the archive, or its section does not parse. `None` always means "run the original".
    fn section(&self, field: &str) -> Option<Arc<Vec<u8>>>;
}

impl ScriptSource for Mods {
    fn label(&self) -> String {
        self.echo.as_ref().map_or_else(|| "original".to_string(), |e| format!("echo-s {}", e.info.version))
    }

    fn section(&self, field: &str) -> Option<Arc<Vec<u8>>> {
        self.script_section(field)
    }
}

/// `*.iro` files directly in `dir` and one folder down, sorted (a folder that cannot be read has none).
fn candidates(dir: &Path) -> Vec<PathBuf> {
    fn iro_files(dir: &Path, out: &mut Vec<PathBuf>, folders: Option<&mut Vec<PathBuf>>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut folders = folders;
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if let Some(f) = folders.as_deref_mut() {
                    f.push(path);
                }
            } else if kind.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iro")) {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    let mut folders = Vec::new();
    iro_files(dir, &mut files, Some(&mut folders));
    folders.sort();
    for folder in folders.into_iter().take(MAX_CANDIDATES) {
        iro_files(&folder, &mut files, None);
    }
    files.sort();
    files.truncate(MAX_CANDIDATES);
    files
}

/// Open `path` and read which mod it is.
fn identify(path: &Path) -> Result<Opened, String> {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let archive = IroArchive::open_path(path).map_err(|e| format!("{name}: {e}"))?;
    let entry = archive.find("mod.xml").ok_or_else(|| format!("{name}: no mod.xml"))?;
    let bytes = archive.read_limited(entry, MAX_MOD_XML).map_err(|e| format!("{name}: no readable mod.xml ({e})"))?;
    let info = modxml::parse(&bytes).ok_or_else(|| format!("{name}: its mod.xml has no <ID>"))?;
    Ok(Opened { archive: Arc::new(archive), info })
}

impl Mods {
    /// No archives at all: every query answers `None`, so the game is the base game.
    pub fn none() -> Self {
        Self::assemble(None, None, Layers::ALL, Vec::new())
    }

    /// Open what `sources` names: explicit paths first, then the archives found in `sources.dir`. Never fails: an archive that
    /// is missing, damaged, truncated (the entry that runs past the end is named) or not the mod it should be is a note
    /// ([`Mods::notes`]) and its layer is absent.
    pub fn open(sources: &ModSources, layers: Layers) -> Self {
        let mut notes: Vec<String> = Vec::new();
        let mut slots = Slots::default();
        for (path, want) in [(&sources.echo_s, "echo-s"), (&sources.cosmo, "cosmo")] {
            if let Some(path) = path {
                match identify(path) {
                    Ok(found) => slots.place(found, &mut notes, Some(want)),
                    Err(why) => notes.push(format!("cannot use the {want} archive: {why}")),
                }
            }
        }
        if let Some(dir) = &sources.dir {
            for path in candidates(dir) {
                // Both layers are taken (or named by an explicit path): nothing else is worth opening.
                if (slots.echo.is_some() || sources.echo_s.is_some()) && (slots.cosmo.is_some() || sources.cosmo.is_some()) {
                    break;
                }
                match identify(&path) {
                    Ok(found) => slots.place(found, &mut notes, None),
                    Err(why) => notes.push(format!("skipped {why}")),
                }
            }
        }
        Self::assemble(slots.echo, slots.cosmo, layers, notes)
    }

    fn assemble(echo: Option<Opened>, cosmo: Option<Opened>, layers: Layers, notes: Vec<String>) -> Self {
        let read_config = |opened: &Option<Opened>, path: &str| -> Option<LayerConfig> {
            let bytes = opened.as_ref()?.bytes(path, MAX_CLIP_BYTES)?;
            Some(LayerConfig::parse(&String::from_utf8_lossy(&bytes)))
        };
        let sfx_config =
            read_config(&cosmo, "Base\\sfx\\config.toml").or_else(|| read_config(&echo, "Sound\\sfx\\config.toml")).unwrap_or_default();
        let music_config = read_config(&echo, "EchoeS Of The Planet\\vgmstream\\config.toml").unwrap_or_default();
        let shared = Shared { scripts: Mutex::new(VecDeque::new()), sfx_config, music_config, picker: Picker::new(0x5EED_F00D_CAFE_1997) };
        Self { echo, cosmo, layers, notes: Arc::new(notes), shared: Arc::new(shared) }
    }

    /// The same archives with other switches.
    pub fn with_layers(&self, layers: Layers) -> Self {
        Self { layers, ..self.clone() }
    }

    pub fn layers(&self) -> Layers {
        self.layers
    }

    /// What opening the archives said that is worth a line (a damaged or misnamed archive).
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Whether the Echo-S archive is open.
    pub fn has_echo(&self) -> bool {
        self.echo.is_some()
    }

    /// Whether the Cosmo Memory archive is open.
    pub fn has_cosmo(&self) -> bool {
        self.cosmo.is_some()
    }

    /// The Echo-S version (`1.24`), if the archive is open.
    pub fn echo_version(&self) -> Option<&str> {
        self.echo.as_ref().map(|e| e.info.version.as_str())
    }

    /// The effective source of each layer, in one short line (what the receipts of a run show):
    /// `mods: text=echo-s voice=echo-s music=echo-s sound=cosmo [echo-s 1.24, cosmo 1.22]`, with `original (off)` for a layer that
    /// is switched off and `original (no archive)` for one that has nothing to play (the base game plays then).
    pub fn describe(&self) -> String {
        let pick = |on: bool, source: Option<&str>| match (on, source) {
            (false, _) => "original (off)".to_string(),
            (true, None) => "original (no archive)".to_string(),
            (true, Some(s)) => s.to_string(),
        };
        let echo = self.echo.is_some().then_some("echo-s");
        let sound = if self.cosmo.is_some() { Some("cosmo") } else { echo };
        let versions: Vec<String> = self
            .echo
            .iter()
            .map(|e| format!("echo-s {}", e.info.version))
            .chain(self.cosmo.iter().map(|c| format!("cosmo {}", c.info.version)))
            .collect();
        format!(
            "mods: text={} voice={} music={} sound={} [{}]",
            pick(self.layers.text, echo),
            pick(self.layers.voice, echo),
            pick(self.layers.music, echo),
            pick(self.layers.sound, sound),
            if versions.is_empty() { "no archives".to_string() } else { versions.join(", ") },
        )
    }

    // ---- text -----------------------------------------------------------------------------------

    /// Section 0 (the script section) of Echo-S's replacement for `field`, validated (`FieldScript::parse` accepts it).
    /// `None`: the text layer is off, there is no Echo-S archive, the field has no entry, or the entry does not decode.
    pub fn script_section(&self, field: &str) -> Option<Arc<Vec<u8>>> {
        if !self.layers.text {
            return None;
        }
        let echo = self.echo.as_ref()?;
        let key = field.to_ascii_lowercase();
        {
            let cache = self.shared.scripts.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((_, found)) = cache.iter().find(|(k, _)| *k == key) {
                return found.clone();
            }
        }
        let read = || -> Option<Arc<Vec<u8>>> {
            let stored = echo.bytes(&echo::flevel_path(&key), MAX_CLIP_BYTES)?;
            let raw = lzss::decompress_with_limit(&stored, fmv_ff7::MAX_FIELD_BYTES).ok()?;
            let file = FieldFile::parse(raw).ok()?;
            let section = file.section(section::SCRIPT).ok()?.to_vec();
            FieldScript::parse(&section).ok()?;
            Some(Arc::new(section))
        };
        let found = read();
        let mut cache = self.shared.scripts.lock().unwrap_or_else(PoisonError::into_inner);
        cache.push_back((key, found.clone()));
        while cache.len() > SCRIPT_CACHE {
            cache.pop_front();
        }
        found
    }

    // ---- voices ---------------------------------------------------------------------------------

    fn voice_entry(&self, field: &str, names: &[String]) -> Option<(&Opened, &IroEntry)> {
        if !self.layers.voice {
            return None;
        }
        let echo = self.echo.as_ref()?;
        names.iter().find_map(|name| {
            echo::CLIP_EXTENSIONS.iter().find_map(|ext| echo.archive.find(&echo::voice_path(field, name, ext)).map(|entry| (echo, entry)))
        })
    }

    /// The archive path of the voice of `dialog`, shown as window `window` of `field` at game page `page` (0 for the first page of
    /// a message), by FFNx's lookup order ([`echo::voice_names`]); `None` when the layer is off or no such clip exists.
    pub fn voice_path(&self, field: &str, window: u8, dialog: usize, page: usize) -> Option<String> {
        self.voice_entry(field, &echo::voice_names(window, dialog, page)).map(|(_, e)| e.name.clone())
    }

    /// The voice clip of that message page (see [`Mods::voice_path`]).
    pub fn voice(&self, field: &str, window: u8, dialog: usize, page: usize) -> Option<Clip> {
        let (echo, entry) = self.voice_entry(field, &echo::voice_names(window, dialog, page))?;
        let bytes = echo.archive.read_limited(entry, MAX_CLIP_BYTES).ok()?;
        Some(Clip { name: entry.name.clone(), bytes, source: echo.label() })
    }

    /// The voice played when the cursor of the `ASK` of `dialog` reaches the absolute `line`.
    pub fn option_voice(&self, field: &str, dialog: usize, line: usize) -> Option<Clip> {
        let names = [echo::option_voice_name(dialog, line)];
        let (echo, entry) = self.voice_entry(field, &names)?;
        let bytes = echo.archive.read_limited(entry, MAX_CLIP_BYTES).ok()?;
        Some(Clip { name: entry.name.clone(), bytes, source: echo.label() })
    }

    // ---- music ----------------------------------------------------------------------------------

    /// The track `name` (a `music.idx` name such as `bat`) from Echo-S's "Echoes of the Planet", or `None` (layer off, no archive, no
    /// such track: the caller plays the original). The three names the layer's `config.toml` shuffles (`bat`, `bee`, `gun`) pick
    /// one of their variants at random each time.
    pub fn music(&self, name: &str) -> Option<Clip> {
        if !self.layers.music {
            return None;
        }
        let echo = self.echo.as_ref()?;
        let key = name.to_ascii_lowercase();
        let stem = match self.shared.music_config.section(&key) {
            Some(section) => match self.shared.picker.pick(&format!("music/{key}"), section) {
                Some(Pick::Entry(entry)) => entry.stem(),
                Some(Pick::Silent) => return None,
                None => key.clone(),
            },
            None => key.clone(),
        };
        // A shuffled name that is not in the folders falls back to the plain one.
        for stem in [stem.as_str(), key.as_str()] {
            for dir in echo::MUSIC_DIRS {
                for ext in echo::CLIP_EXTENSIONS {
                    let path = format!("{dir}{stem}.{ext}");
                    if let Some(entry) = echo.archive.find(&path) {
                        let bytes = echo.archive.read_limited(entry, MAX_TRACK_BYTES).ok()?;
                        return Some(Clip { name: entry.name.clone(), bytes, source: echo.label() });
                    }
                }
            }
        }
        None
    }

    // ---- sound effects --------------------------------------------------------------------------

    /// The folders the sound layer's clips are looked up in, in the order of 7th Heaven's first-wins rule: Cosmo's menu set 1 (the
    /// default "Updated Vanilla"), Cosmo's `Base`, then the attack folders (voiced when the voice layer is on, plain when it is
    /// off), then Echo-S's own `Sound` folder. Echo-S's attack folders come before its `Sound`.
    fn sfx_dirs(&self) -> Vec<(&Opened, &'static str)> {
        let attacks = if self.layers.voice { echo::VOICED_ATTACKS_DIR } else { echo::PLAIN_ATTACKS_DIR };
        let mut dirs = Vec::new();
        if let Some(c) = &self.cosmo {
            dirs.push((c, "UI_Dissidia\\sfx\\"));
            dirs.push((c, "Base\\sfx\\"));
            dirs.push((c, attacks));
        }
        if let Some(e) = &self.echo {
            dirs.push((e, attacks));
            dirs.push((e, echo::SOUND_DIR));
        }
        dirs
    }

    fn sfx_clip(&self, stem: &str) -> Option<Clip> {
        for (opened, dir) in self.sfx_dirs() {
            for ext in echo::CLIP_EXTENSIONS {
                if let Some(entry) = opened.archive.find(&format!("{dir}{stem}.{ext}")) {
                    let bytes = opened.archive.read_limited(entry, MAX_CLIP_BYTES).ok()?;
                    return Some(Clip { name: entry.name.clone(), bytes, source: opened.label() });
                }
            }
        }
        None
    }

    /// The clips the sound layer plays for sound id `id` (the game's own, `1..=750`, the id space of `audio.fmt`), in the field
    /// `field` when it is known: FFNx's lookup order `[<field>_<id>]`, then `[<id>]` (the triangle-specific `[<field>_<tri>_<id>]`
    /// sections are for footsteps and need the walkmesh triangle the player stands on at that moment, which the client does
    /// not pass: a gap), the first section whose clips exist wins; then a bare `<id>.ogg`. `None`: the layer is off, nothing
    /// names this id, or none of the named files exist (the original clip plays, as FFNx does).
    pub fn sfx(&self, id: u16, field: Option<&str>) -> Option<SfxSet> {
        if !self.layers.sound || (self.cosmo.is_none() && self.echo.is_none()) {
            return None;
        }
        let mut names: Vec<String> = Vec::new();
        if let Some(f) = field {
            names.push(format!("{}_{id}", f.to_ascii_lowercase()));
        }
        names.push(id.to_string());
        for name in &names {
            let Some(section) = self.shared.sfx_config.section(name) else { continue };
            if section.skip {
                continue;
            }
            let clips: Vec<Clip> = section.entries().iter().filter_map(|e: &Entry| self.sfx_clip(&e.stem())).collect();
            if !clips.is_empty() {
                return Some(SfxSet { section: name.clone(), mode: section.mode().unwrap_or(Mode::Sequential), clips });
            }
        }
        let clip = self.sfx_clip(&id.to_string())?;
        Some(SfxSet { section: id.to_string(), mode: Mode::Sequential, clips: vec![clip] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{echo_archive, EchoSpec};
    use std::io::Write;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::File::create(&path).unwrap().write_all(bytes).unwrap();
        path
    }

    fn spec() -> EchoSpec<'static> {
        let mut s = EchoSpec::default();
        s.fields.push(("md1stin", vec!["First line", "Second line"]));
        s.voices.push("VO\\Voice\\md1stin\\1a.ogg");
        s.voices.push("VO\\Voice\\md1stin\\1b.ogg");
        s.voices.push("VO\\Voice\\md1stin\\0.ogg");
        s.voices.push("VO\\Voice\\md1stin\\w1_2.ogg");
        s.voices.push("VO\\Voice\\md1stin\\3_1.ogg");
        s.music.push("EchoeS Of The Planet\\vgmstream\\BAT.ogg");
        s.music.push("EchoeS Of The Planet\\vgmstream\\bat1.ogg");
        s.music.push("Arranged\\vgmstream\\cannon.ogg");
        s.sounds.push("Sound\\sfx\\6303.ogg");
        s.sounds.push("Sound\\sfx\\12.ogg");
        s.sfx_config = Some("[6]\nsequential = [ 6303 ]\n[md1stin_7]\nsequential = [ 12, 13 ]\n".to_string());
        s.music_config = Some("[bat]\nshuffle = [ \"bat1\", \"bat\" ]\n".to_string());
        s
    }

    #[test]
    fn nothing_configured_is_the_base_game() {
        let m = Mods::none();
        assert!(m.script_section("md1stin").is_none());
        assert!(m.voice("md1stin", 0, 1, 0).is_none());
        assert!(m.music("bat").is_none());
        assert!(m.sfx(6, None).is_none());
        assert_eq!(
            m.describe(),
            "mods: text=original (no archive) voice=original (no archive) music=original (no archive) sound=original (no archive) [no archives]"
        );
        assert!(m.notes().is_empty());
    }

    #[test]
    fn archives_are_found_by_their_mod_id_not_their_name() {
        let tmp = tempfile::tempdir().unwrap();
        // Named like nothing in particular, one folder down.
        write(&tmp.path().join("somewhere"), "whatever.IRO", &echo_archive(&spec()));
        write(tmp.path(), "notes.txt", b"not an archive");
        let m = Mods::open(&ModSources { dir: Some(tmp.path().to_path_buf()), ..Default::default() }, Layers::ALL);
        assert!(m.has_echo(), "{:?}", m.notes());
        assert!(!m.has_cosmo());
        assert_eq!(m.echo_version(), Some("1.24"));
        assert_eq!(m.describe(), "mods: text=echo-s voice=echo-s music=echo-s sound=echo-s [echo-s 1.24]");
    }

    #[test]
    fn a_damaged_or_foreign_archive_is_a_note_and_an_absent_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let mut bytes = echo_archive(&spec());
        bytes.truncate(bytes.len() - 5);
        write(tmp.path(), "truncated.iro", &bytes);
        write(tmp.path(), "garbage.iro", b"nothing like an archive at all");
        let m = Mods::open(&ModSources { dir: Some(tmp.path().to_path_buf()), ..Default::default() }, Layers::ALL);
        assert!(!m.has_echo());
        assert_eq!(m.notes().len(), 2, "{:?}", m.notes());
        assert!(m.notes().iter().any(|n| n.contains("truncated")), "{:?}", m.notes());
        // An explicit path that is the wrong mod is refused too.
        let m = Mods::open(&ModSources { cosmo: Some(write(tmp.path(), "echo.iro", &echo_archive(&spec()))), ..Default::default() }, Layers::ALL);
        assert!(!m.has_cosmo() && !m.has_echo());
        assert!(m.notes().iter().any(|n| n.contains("the archive given for cosmo")), "{:?}", m.notes());
        // A folder that does not exist is nothing, not an error, and is not created.
        let missing = tmp.path().join("no-such-folder");
        let m = Mods::open(&ModSources { dir: Some(missing.clone()), ..Default::default() }, Layers::ALL);
        assert!(!m.has_echo() && !missing.exists());
    }

    fn open_echo(layers: Layers) -> (tempfile::TempDir, Mods) {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), "echo.iro", &echo_archive(&spec()));
        let m = Mods::open(&ModSources { echo_s: Some(path), ..Default::default() }, layers);
        assert!(m.has_echo(), "{:?}", m.notes());
        (tmp, m)
    }

    #[test]
    fn the_script_section_of_a_field_is_echo_s_own_and_is_cached() {
        let (_t, m) = open_echo(Layers::ALL);
        let a = m.script_section("MD1STIN").expect("the synthetic field");
        let script = FieldScript::parse(&a).unwrap();
        assert_eq!(script.dialogs().len(), 2);
        let b = m.script_section("md1stin").unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the second read is the cached section");
        assert!(m.script_section("nosuchfield").is_none());
        assert_eq!(m.label(), "echo-s 1.24");
    }

    #[test]
    fn every_layer_has_its_own_switch() {
        let (_t, m) = open_echo(Layers::ALL);
        let off = |l: Layers| m.with_layers(l);
        assert!(off(Layers { text: false, ..Layers::ALL }).script_section("md1stin").is_none());
        assert!(off(Layers { text: false, ..Layers::ALL }).voice("md1stin", 0, 1, 0).is_some());
        assert!(off(Layers { voice: false, ..Layers::ALL }).voice("md1stin", 0, 1, 0).is_none());
        assert!(off(Layers { voice: false, ..Layers::ALL }).music("bat").is_some());
        assert!(off(Layers { music: false, ..Layers::ALL }).music("bat").is_none());
        assert!(off(Layers { sound: false, ..Layers::ALL }).sfx(6, None).is_none());
        assert!(off(Layers { sound: false, ..Layers::ALL }).script_section("md1stin").is_some());
        let none = off(Layers::NONE);
        assert!(none.script_section("md1stin").is_none() && none.music("bat").is_none() && none.sfx(6, None).is_none());
        assert_eq!(none.describe(), "mods: text=original (off) voice=original (off) music=original (off) sound=original (off) [echo-s 1.24]");
    }

    #[test]
    fn voices_follow_ffnx_lookup_order() {
        let (_t, m) = open_echo(Layers::ALL);
        // Page 0 of dialog 1: the lettered name first.
        assert_eq!(m.voice_path("md1stin", 0, 1, 0).as_deref(), Some("VO\\Voice\\md1stin\\1a.ogg"));
        // Page 1: only lettered names.
        assert_eq!(m.voice_path("md1stin", 0, 1, 1).as_deref(), Some("VO\\Voice\\md1stin\\1b.ogg"));
        assert_eq!(m.voice_path("md1stin", 0, 1, 2), None);
        // Dialog 0 exists only as the plain name, on the first page.
        assert_eq!(m.voice_path("md1stin", 0, 0, 0).as_deref(), Some("VO\\Voice\\md1stin\\0.ogg"));
        assert_eq!(m.voice_path("md1stin", 0, 0, 1), None);
        // A window-specific file is found for that window only.
        assert_eq!(m.voice_path("md1stin", 1, 2, 0).as_deref(), Some("VO\\Voice\\md1stin\\w1_2.ogg"));
        assert_eq!(m.voice_path("md1stin", 0, 2, 0), None);
        assert!(m.voice("md1stin", 0, 1, 0).unwrap().bytes.starts_with(b"OggS"));
        // The option voice, and a field with no voices.
        assert!(m.option_voice("md1stin", 3, 1).is_some());
        assert!(m.option_voice("md1stin", 3, 0).is_none());
        assert!(m.voice("nmkin_5", 0, 1, 0).is_none());
    }

    #[test]
    fn music_names_match_ignoring_case_and_the_config_shuffles_the_variants() {
        let (_t, m) = open_echo(Layers::ALL);
        assert_eq!(m.music("cannon").unwrap().name, "Arranged\\vgmstream\\cannon.ogg");
        assert!(m.music("aseri").is_none());
        // `bat` shuffles between `bat1` and `bat`: both are heard, and nothing else.
        let heard: std::collections::BTreeSet<String> = (0..40).filter_map(|_| m.music("BAT")).map(|c| c.name).collect();
        assert_eq!(heard.len(), 2, "{heard:?}");
        assert!(heard.iter().all(|n| n.ends_with("BAT.ogg") || n.ends_with("bat1.ogg")));
    }

    #[test]
    fn sound_ids_map_through_the_config_and_fall_back_to_nothing() {
        let (_t, m) = open_echo(Layers::ALL);
        // [6] sequential = [6303]: id 6 plays the file 6303.
        let set = m.sfx(6, None).unwrap();
        assert_eq!((set.section.as_str(), set.mode), ("6", Mode::Sequential));
        assert_eq!(set.clips.len(), 1);
        assert_eq!(set.clips[0].name, "Sound\\sfx\\6303.ogg");
        // A field-specific section wins in that field only; a clip it names that is missing is dropped (13 is not in the folder).
        let set = m.sfx(7, Some("md1stin")).unwrap();
        assert_eq!(set.section, "md1stin_7");
        assert_eq!(set.clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Sound\\sfx\\12.ogg"]);
        assert!(m.sfx(7, Some("nmkin_5")).is_none());
        // No section, no bare file: the original plays.
        assert!(m.sfx(99, None).is_none());
        // A bare file named by the id plays when no section names it.
        assert_eq!(m.sfx(12, None).unwrap().clips[0].name, "Sound\\sfx\\12.ogg");
    }
}
