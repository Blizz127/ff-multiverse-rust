//! The optional community-mod layers in the client: where the archives are, which layers are on, and the one place
//! the rest of the client asks for them.
//!
//! The architecture is the owner's: *the accurate base game comes first, the mods are layers over it, and each can be
//! switched off.* Four layers, all **on by default** (see `fmv_mods` for what each replaces and how):
//!
//! | Layer | What | Switch |
//! |---|---|---|
//! | `text` | Echo-S's retranslated script as the dialog text (its own script section, see below) | `--mods-text off` |
//! | `voice` | Echo-S's voice acting, played when a dialog window opens (it rides on the Echo-S text: a voice is named by Echo-S's own dialog numbers) | `--mods-voice off` |
//! | `music` | Echo-S's "Echoes of the Planet" over the original track of the same name | `--mods-music off` |
//! | `sound` | Cosmo Memory's sound effects over the original clip of the same sound id | `--mods-sound off` |
//!
//! # Where the archives are
//!
//! They are the owner's own files, fetched separately onto each machine and **never copied into this repository, a library
//! or between machines by this program** (it only reads them). In order: `--echo-s FILE` / `$FMV_ECHO_S_IRO` and
//! `--cosmo-memory FILE` / `$FMV_COSMO_IRO` name an archive each; `--mods-dir DIR` / `$FMV_MODS_DIR` names a folder that
//! holds them (the files or folders that do, one level down); the default folder is `mods` in the library's state directory
//! (`--ff7-library`, else `$FMV_STATE_DIR`, else `~/.local/share/ff-multiverse/mods`), which is looked in and never created. An
//! archive is told from the other by the ID in its own `mod.xml`, not by its file name.
//!
//! # Where the switches are
//!
//! One order of precedence, the same for each layer: a flag, else an environment variable, else the settings file, else on.
//!
//! * flags: `--mods on|off` (every layer) and `--mods-text`, `--mods-voice`, `--mods-music`, `--mods-sound` (`on` or `off`;
//!   `on`, `true`, `yes`, `1` and `off`, `false`, `no`, `0` all work);
//! * environment: `FMV_MODS` and `FMV_MODS_TEXT`, `FMV_MODS_VOICE`, `FMV_MODS_MUSIC`, `FMV_MODS_SOUND`, the same values;
//! * the settings file, `settings.json` in the mods folder (`--mods-settings FILE` / `$FMV_MODS_SETTINGS` name another): a JSON
//!   object of booleans, for example `{"voice": false, "music": false}`; a key that is left out is on. The client only
//!   reads it.
//!
//! One line says the effective source of each layer at start-up (`mods: text=echo-s voice=echo-s music=echo-s sound=cosmo
//! [echo-s 1.24, cosmo 1.22]`, with `original (off)` or `original (no archive)` for a layer that plays the base game), and
//! one per damaged or misnamed archive.
//!
//! # For the scene player
//!
//! [`script_source`] is the text layer's interface: the script section to run in place of the original's for a field, or `None`.
//! Echo-S's section is a different script from the original's (its dialog numbers, its windows and its entity indices differ), so
//! a scene or a conversation that takes it takes ALL of it: text, flow and, through [`VoiceCue`], the voices keyed to it.

use clap::Args as ClapArgs;
use fmv_mods::{Layers, ModSources, Mods, ScriptSource};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// The environment variables of the sources and the switches.
pub const DIR_ENV: &str = "FMV_MODS_DIR";
pub const ECHO_S_ENV: &str = "FMV_ECHO_S_IRO";
pub const COSMO_ENV: &str = "FMV_COSMO_IRO";
pub const SETTINGS_ENV: &str = "FMV_MODS_SETTINGS";
pub const ALL_ENV: &str = "FMV_MODS";
pub const TEXT_ENV: &str = "FMV_MODS_TEXT";
pub const VOICE_ENV: &str = "FMV_MODS_VOICE";
pub const MUSIC_ENV: &str = "FMV_MODS_MUSIC";
pub const SOUND_ENV: &str = "FMV_MODS_SOUND";
pub const MODELS_ENV: &str = "FMV_MODS_MODELS";
/// The biggest settings file read.
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;

/// The command-line flags of the mod layers (flattened into the client's `Args`).
#[derive(ClapArgs, Debug, Clone, Default, PartialEq, Eq)]
pub struct ModFlags {
    /// Folder holding the optional mod archives (Echo-S's `Echo-s.iro`, Cosmo Memory's `[Tsunamods]_Cosmo_Memory.iro`), the
    /// files or folders that do (one level down). Each is told by the ID in its own mod.xml. Default: $FMV_MODS_DIR, else `mods`
    /// in the library's state directory (looked in, never created). The archives are only read.
    #[arg(long = "mods-dir", value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// The Echo-S archive itself (default: $FMV_ECHO_S_IRO, else the one found in the mods folder): its retranslated script as
    /// the dialog text, its voices and its music.
    #[arg(long = "echo-s", value_name = "FILE")]
    pub echo_s: Option<PathBuf>,

    /// The Cosmo Memory archive itself (default: $FMV_COSMO_IRO, else the one found in the mods folder): its sound effects.
    #[arg(long = "cosmo-memory", value_name = "FILE")]
    pub cosmo: Option<PathBuf>,

    /// Every mod layer on or off (the base game exactly, with `off`). The layers are on by default; a per-layer flag below
    /// overrides this one.
    #[arg(long = "mods", value_name = "on|off", value_parser = parse_switch)]
    pub all: Option<bool>,

    /// Echo-S's script as the dialog text, on or off (`off`: the original 1997 script). Default on.
    #[arg(long = "mods-text", value_name = "on|off", value_parser = parse_switch)]
    pub text: Option<bool>,

    /// Echo-S's voices, on or off (they play with Echo-S's text, which they are named for). Default on.
    #[arg(long = "mods-voice", value_name = "on|off", value_parser = parse_switch)]
    pub voice: Option<bool>,

    /// Echo-S's music, on or off (`off`: the original music). Default on.
    #[arg(long = "mods-music", value_name = "on|off", value_parser = parse_switch)]
    pub music: Option<bool>,

    /// Cosmo Memory's sound effects, on or off (`off`: the original sounds). Default on.
    #[arg(long = "mods-sound", value_name = "on|off", value_parser = parse_switch)]
    pub sound: Option<bool>,

    /// Ninostyle models, on by default; off uses the original models.
    #[arg(long = "mods-models", value_name = "on|off", value_parser = parse_switch)]
    pub models: Option<bool>,

    /// A JSON settings file of booleans (`{"voice": false}`, keys text, voice, music, sound, models; a left-out key is on). Default:
    /// $FMV_MODS_SETTINGS, else `settings.json` in the mods folder. Read only; flags and environment override it.
    #[arg(long = "mods-settings", value_name = "FILE")]
    pub settings: Option<PathBuf>,
}

/// `on` or `off` (also `true`/`false`, `yes`/`no`, `1`/`0`, any case).
pub fn parse_switch(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(format!("{other:?} is not on or off")),
    }
}

/// What the flags, the environment and the settings file came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub sources: ModSources,
    pub layers: Layers,
    pub models: bool,
    /// The settings file that was looked for.
    pub settings: Option<PathBuf>,
    /// Things worth a line: a value that was not on or off, a settings file that did not parse.
    pub notes: Vec<String>,
}

/// The switches a settings file sets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Partial {
    text: Option<bool>,
    voice: Option<bool>,
    music: Option<bool>,
    sound: Option<bool>,
}

impl Partial {
    fn apply(self, layers: &mut Layers) {
        for (value, slot) in [(self.text, &mut layers.text), (self.voice, &mut layers.voice), (self.music, &mut layers.music), (self.sound, &mut layers.sound)] {
            if let Some(v) = value {
                *slot = v;
            }
        }
    }
}

/// The booleans of a settings file's JSON object (`None`, with a note, for a file that is not one).
fn parse_settings(bytes: &[u8], notes: &mut Vec<String>) -> Partial {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        notes.push("the settings file is not a JSON object: ignored".to_string());
        return Partial::default();
    };
    let mut one = |key: &str| match map.get(key) {
        None => None,
        Some(serde_json::Value::Bool(b)) => Some(*b),
        Some(_) => {
            notes.push(format!("the settings file's {key:?} is not true or false: ignored"));
            None
        }
    };
    Partial { text: one("text"), voice: one("voice"), music: one("music"), sound: one("sound") }
}

/// Settle the sources and the switches. `env` reads an environment variable (injected for the tests) and `state_dir` is the library's
/// state directory, whose `mods` folder is the default place to look.
pub fn resolve(flags: &ModFlags, env: &dyn Fn(&str) -> Option<String>, state_dir: Option<&Path>) -> Resolved {
    let mut notes = Vec::new();
    let non_empty = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    let path_env = |name: &str| non_empty(name).map(PathBuf::from);
    let dir = flags.dir.clone().or_else(|| path_env(DIR_ENV)).or_else(|| state_dir.map(|s| s.join("mods")));
    let sources = ModSources {
        dir: dir.clone(),
        echo_s: flags.echo_s.clone().or_else(|| path_env(ECHO_S_ENV)),
        cosmo: flags.cosmo.clone().or_else(|| path_env(COSMO_ENV)),
    };
    let settings = flags.settings.clone().or_else(|| path_env(SETTINGS_ENV)).or_else(|| dir.map(|d| d.join("settings.json")));
    let mut layers = Layers::ALL;
    let mut models = true;
    // Lowest precedence first: the file, then the environment (every layer, then each), then the flags the same way.
    if let Some(path) = &settings {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_SETTINGS_BYTES => match std::fs::read(path) {
                Ok(bytes) => {
                    parse_settings(&bytes, &mut notes).apply(&mut layers);
                    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        if let Some(value) = map.get("models") {
                            if let Some(value) = value.as_bool() { models = value; }
                            else { notes.push("the settings file's models is not true or false: ignored".to_string()); }
                        }
                    }
                },
                Err(e) => notes.push(format!("the settings file cannot be read ({}): ignored", e.kind())),
            },
            Ok(meta) if meta.is_file() => notes.push("the settings file is too big: ignored".to_string()),
            _ => {}
        }
    }
    let mut from_env = |name: &str| -> Option<bool> {
        let value = non_empty(name)?;
        match parse_switch(&value) {
            Ok(b) => Some(b),
            Err(e) => {
                notes.push(format!("${name}: {e}, ignored"));
                None
            }
        }
    };
    let all = from_env(ALL_ENV);
    let by_env = Partial { text: from_env(TEXT_ENV), voice: from_env(VOICE_ENV), music: from_env(MUSIC_ENV), sound: from_env(SOUND_ENV) };
    let everything = |value: Option<bool>| Partial { text: value, voice: value, music: value, sound: value };
    everything(all).apply(&mut layers);
    by_env.apply(&mut layers);
    everything(flags.all).apply(&mut layers);
    Partial { text: flags.text, voice: flags.voice, music: flags.music, sound: flags.sound }.apply(&mut layers);
    if let Some(value) = all { models = value; }
    if let Some(value) = from_env(MODELS_ENV) { models = value; }
    if let Some(value) = flags.all { models = value; }
    if let Some(value) = flags.models { models = value; }
    Resolved { sources, layers, models, settings, notes }
}

// ---- the opened layers, for the whole process ---------------------------------------------------------

static ACTIVE: RwLock<Option<Arc<Mods>>> = RwLock::new(None);

/// Make `mods` the layers this process uses (once, at start-up; tests build their own and pass them explicitly).
pub fn install(mods: Option<Arc<Mods>>) {
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = mods;
}

/// The layers this process uses, if any archive is open.
pub fn active() -> Option<Arc<Mods>> {
    ACTIVE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The text layer's interface for anything that runs field scripts (the conversation walker, the scene player): the script section
/// of Echo-S's replacement for a field, from [`ScriptSource::section`]. `None` when no archive is open or the text layer is
/// switched off, so a caller that asks every time needs no check of its own.
///
/// Nothing in this crate's own code calls it yet: it is the interface the scene player (another lane's code) takes the text layer
/// through, and it is tested here.
#[allow(dead_code)]
pub fn script_source() -> Option<Arc<dyn ScriptSource>> {
    let mods = active()?;
    (mods.layers().text && mods.has_echo()).then_some(mods as Arc<dyn ScriptSource>)
}

/// Open the layers `flags` and the environment name, install them and say, on stderr, what each layer plays (one line) and what
/// was wrong with an archive (a line each). `state_dir` is the library's state directory (`mods` in it is the default folder).
/// Never fails: with no archive every layer plays the base game.
pub fn setup(flags: &ModFlags, state_dir: Option<&Path>) -> Arc<Mods> {
    let resolved = resolve(flags, &|name| std::env::var(name).ok(), state_dir);
    crate::model_layer::install(resolved.sources.dir.as_deref(), resolved.models);
    let mods = Arc::new(Mods::open(&resolved.sources, resolved.layers));
    for note in resolved.notes.iter().chain(mods.notes()) {
        eprintln!("mods: note: {note}");
    }
    eprintln!("{}", mods.describe());
    install(Some(Arc::clone(&mods)));
    mods
}

/// What a run owes when its soundtrack is off while mod layers that need it are on: the `mods:` line at start-up says which layers are on
/// and the owner reads it as what plays, but a voice, a track and a sound cannot play without the soundtrack. `None` when no audible layer
/// has anything to play (the text layer needs no sound). `why` says why the soundtrack is off.
pub fn silent_audio_note(mods: Option<&Mods>, why: &str) -> Option<String> {
    let mods = mods?;
    let layers = mods.layers();
    let mut silent: Vec<&str> = Vec::new();
    if layers.voice && mods.has_echo() {
        silent.push("voice");
    }
    if layers.music && mods.has_echo() {
        silent.push("music");
    }
    if layers.sound && (mods.has_echo() || mods.has_cosmo()) {
        silent.push("sound");
    }
    let list = match silent.as_slice() {
        [] => return None,
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    let (noun, verb) = if silent.len() == 1 { ("layer", "plays") } else { ("layers", "play") };
    Some(format!("mods: the {list} {noun} {verb} nothing in this run ({why})"))
}

// ---- what the layers ask of the audio ----------------------------------------------------------------

/// What a conversation asks of the voice layer, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceCue {
    /// A window opened on `dialog` of `field` (as window `window`), at game page `page` of its message: play its voice.
    Say { field: String, window: u8, dialog: usize, page: usize },
    /// The cursor of the question `dialog` of `field` reached the absolute line `line`: play that choice's voice.
    Choice { field: String, dialog: usize, line: usize },
    /// The conversation is over: stop the voice that is playing.
    Stop,
}

/// Where a conversation sends its voice cues and its one-line notes. In the game it is [`Global`]; a test collects them.
pub trait ModSink: Send + Sync {
    fn cue(&self, cue: VoiceCue);
    fn note(&self, line: &str);
}

/// The most cues kept waiting for the audio (a frame takes them all; this only bounds a stalled one).
const MAX_QUEUED: usize = 64;

static QUEUE: Mutex<VecDeque<VoiceCue>> = Mutex::new(VecDeque::new());

/// The sink of the running game: cues wait in a queue the audio takes every frame ([`take_voices`]), notes go to stderr.
#[derive(Debug, Clone, Copy, Default)]
pub struct Global;

impl ModSink for Global {
    fn cue(&self, cue: VoiceCue) {
        let mut queue = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        queue.push_back(cue);
        while queue.len() > MAX_QUEUED {
            queue.pop_front();
        }
    }

    fn note(&self, line: &str) {
        eprintln!("{line}");
    }
}

/// The cues waiting for the audio, oldest first (the audio calls this every frame).
pub fn take_voices() -> Vec<VoiceCue> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| map.get(name).cloned()
    }

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::File::create(&path).unwrap().write_all(text.as_bytes()).unwrap();
        path
    }

    #[test]
    fn everything_is_on_by_default_and_the_default_folder_is_mods_in_the_state_dir() {
        let r = resolve(&ModFlags::default(), &env_of(&[]), Some(Path::new("/state")));
        assert!(r.models);
        assert_eq!(r.layers, Layers::ALL);
        assert_eq!(r.sources.dir.as_deref(), Some(Path::new("/state/mods")));
        assert_eq!(r.settings.as_deref(), Some(Path::new("/state/mods/settings.json")));
        assert!(r.sources.echo_s.is_none() && r.sources.cosmo.is_none() && r.notes.is_empty());
        // Without a state directory there is no default folder.
        assert_eq!(resolve(&ModFlags::default(), &env_of(&[]), None).sources.dir, None);
    }

    #[test]
    fn model_switch_respects_file_environment_and_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = file(tmp.path(), "settings.json", r#"{"models":false}"#);
        let flags = ModFlags { settings: Some(settings), ..Default::default() };
        assert!(!resolve(&flags, &env_of(&[]), None).models);
        assert!(resolve(&flags, &env_of(&[(MODELS_ENV, "on")]), None).models);
        let flags = ModFlags { all: Some(false), models: Some(true), ..flags };
        assert!(resolve(&flags, &env_of(&[(MODELS_ENV, "off")]), None).models);
        assert!(!resolve(&ModFlags { all: Some(false), ..Default::default() }, &env_of(&[]), None).models);
    }

    #[test]
    fn sources_come_from_a_flag_else_the_environment_else_the_default() {
        let env = env_of(&[(DIR_ENV, "/env-dir"), (ECHO_S_ENV, "/env/echo.iro"), (COSMO_ENV, "/env/cosmo.iro"), (SETTINGS_ENV, "/env/s.json")]);
        let r = resolve(&ModFlags::default(), &env, Some(Path::new("/state")));
        assert_eq!(r.sources.dir.as_deref(), Some(Path::new("/env-dir")));
        assert_eq!(r.sources.echo_s.as_deref(), Some(Path::new("/env/echo.iro")));
        assert_eq!(r.sources.cosmo.as_deref(), Some(Path::new("/env/cosmo.iro")));
        assert_eq!(r.settings.as_deref(), Some(Path::new("/env/s.json")));
        let flags = ModFlags { dir: Some("/flag-dir".into()), echo_s: Some("/flag/echo.iro".into()), settings: Some("/flag/s.json".into()), ..Default::default() };
        let r = resolve(&flags, &env, None);
        assert_eq!(r.sources.dir.as_deref(), Some(Path::new("/flag-dir")));
        assert_eq!(r.sources.echo_s.as_deref(), Some(Path::new("/flag/echo.iro")));
        assert_eq!(r.sources.cosmo.as_deref(), Some(Path::new("/env/cosmo.iro")));
        assert_eq!(r.settings.as_deref(), Some(Path::new("/flag/s.json")));
        // An empty variable is unset.
        assert_eq!(resolve(&ModFlags::default(), &env_of(&[(DIR_ENV, "  ")]), None).sources.dir, None);
    }

    #[test]
    fn switches_follow_one_precedence_flag_over_environment_over_file() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = file(tmp.path(), "s.json", r#"{"text": false, "voice": false, "music": false, "sound": true, "other": 3}"#);
        let flags = ModFlags { settings: Some(settings), ..Default::default() };
        // The file alone.
        let r = resolve(&flags, &env_of(&[]), None);
        assert_eq!(r.layers, Layers { text: false, voice: false, music: false, sound: true });
        assert!(r.notes.is_empty(), "{:?}", r.notes);
        // The environment over the file: the master first, then a layer.
        let r = resolve(&flags, &env_of(&[(ALL_ENV, "on"), (SOUND_ENV, "off")]), None);
        assert_eq!(r.layers, Layers { text: true, voice: true, music: true, sound: false });
        // A flag over the environment, the master first, then a layer.
        let flags = ModFlags { all: Some(false), music: Some(true), ..flags };
        let r = resolve(&flags, &env_of(&[(ALL_ENV, "on"), (TEXT_ENV, "on")]), None);
        assert_eq!(r.layers, Layers { text: false, voice: false, music: true, sound: false });
        // `--mods off` is the base game.
        let r = resolve(&ModFlags { all: Some(false), ..Default::default() }, &env_of(&[]), None);
        assert_eq!(r.layers, Layers::NONE);
    }

    #[test]
    fn a_bad_setting_is_a_note_and_the_layer_stays_on() {
        let tmp = tempfile::tempdir().unwrap();
        let flags = |name: &str, text: &str| ModFlags { settings: Some(file(tmp.path(), name, text)), ..Default::default() };
        let r = resolve(&flags("a.json", r#"{"voice": "no", "music": false}"#), &env_of(&[(TEXT_ENV, "maybe")]), None);
        assert_eq!(r.layers, Layers { music: false, ..Layers::ALL });
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
        let r = resolve(&flags("b.json", "not json at all"), &env_of(&[]), None);
        assert_eq!(r.layers, Layers::ALL);
        assert!(r.notes[0].contains("not a JSON object"), "{:?}", r.notes);
        let r = resolve(&flags("c.json", "[true]"), &env_of(&[]), None);
        assert!(r.notes[0].contains("not a JSON object"));
        // A missing file is no note: most people have none.
        let r = resolve(&ModFlags { settings: Some(tmp.path().join("nothing.json")), ..Default::default() }, &env_of(&[]), None);
        assert!(r.notes.is_empty());
        // A file over the cap is refused, not read.
        let big = file(tmp.path(), "big.json", &format!("{{\"text\": false, \"pad\": \"{}\"}}", "x".repeat(20_000)));
        let r = resolve(&ModFlags { settings: Some(big), ..Default::default() }, &env_of(&[]), None);
        assert_eq!(r.layers, Layers::ALL);
        assert!(r.notes[0].contains("too big"));
    }

    #[test]
    fn switch_values_accept_the_usual_spellings() {
        for on in ["on", "ON", "true", "Yes", "1", " on "] {
            assert_eq!(parse_switch(on), Ok(true), "{on:?}");
        }
        for off in ["off", "OFF", "false", "No", "0"] {
            assert_eq!(parse_switch(off), Ok(false), "{off:?}");
        }
        assert!(parse_switch("maybe").is_err() && parse_switch("").is_err());
    }

    #[test]
    fn a_run_whose_soundtrack_is_off_says_which_layers_play_nothing() {
        use fmv_mods::testkit::{echo_archive, EchoSpec};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echo.iro");
        std::fs::write(&path, echo_archive(&EchoSpec::default())).unwrap();
        let sources = ModSources { echo_s: Some(path), ..Default::default() };
        let all = Mods::open(&sources, Layers::ALL);
        assert_eq!(
            silent_audio_note(Some(&all), "--mute").as_deref(),
            Some("mods: the voice, music and sound layers play nothing in this run (--mute)")
        );
        // One audible layer left, or only the text: the sentence follows (the text layer needs no sound).
        let only_music = all.with_layers(Layers { text: true, voice: false, music: true, sound: false });
        assert_eq!(silent_audio_note(Some(&only_music), "x").as_deref(), Some("mods: the music layer plays nothing in this run (x)"));
        let text_only = all.with_layers(Layers { text: true, voice: false, music: false, sound: false });
        assert_eq!(silent_audio_note(Some(&text_only), "x"), None);
        // Nothing to play from: no archive, and no mods at all.
        assert_eq!(silent_audio_note(Some(&Mods::none()), "x"), None);
        assert_eq!(silent_audio_note(None, "x"), None);
    }

    #[test]
    fn the_installed_layers_are_what_the_scene_player_asks_for() {
        // (Installs and removes the process's layers; no other test reads them with an adapter that has anything to say.)
        use fmv_mods::testkit::{echo_archive, EchoSpec};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echo.iro");
        let mut spec = EchoSpec::default();
        spec.fields.push(("md1stin", vec!["A line"]));
        std::fs::write(&path, echo_archive(&spec)).unwrap();
        let sources = ModSources { echo_s: Some(path), ..Default::default() };
        install(None);
        assert!(active().is_none() && script_source().is_none());
        install(Some(Arc::new(Mods::open(&sources, Layers::ALL))));
        let source = script_source().expect("the text layer is on and Echo-S is open");
        assert_eq!(source.label(), "echo-s 1.24");
        assert!(source.section("md1stin").is_some() && source.section("nowhere").is_none());
        // Switched off, or without an Echo-S archive: none (the caller runs the original).
        install(Some(Arc::new(Mods::open(&sources, Layers { text: false, ..Layers::ALL }))));
        assert!(script_source().is_none() && active().is_some());
        install(Some(Arc::new(Mods::none())));
        assert!(script_source().is_none());
        install(None);
    }

    #[test]
    fn the_global_sink_queues_cues_for_the_audio_and_bounds_them() {
        // (The only test that touches the process queue.)
        take_voices();
        let sink = Global;
        sink.cue(VoiceCue::Say { field: "md1stin".into(), window: 0, dialog: 3, page: 0 });
        sink.cue(VoiceCue::Stop);
        assert_eq!(take_voices(), vec![VoiceCue::Say { field: "md1stin".into(), window: 0, dialog: 3, page: 0 }, VoiceCue::Stop]);
        assert!(take_voices().is_empty());
        for n in 0..MAX_QUEUED + 10 {
            sink.cue(VoiceCue::Choice { field: "f".into(), dialog: n, line: 0 });
        }
        let taken = take_voices();
        assert_eq!(taken.len(), MAX_QUEUED);
        assert_eq!(taken.first(), Some(&VoiceCue::Choice { field: "f".into(), dialog: 10, line: 0 }), "the oldest went");
    }
}
