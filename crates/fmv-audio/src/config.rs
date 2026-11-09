//! How the engine is started: whether it opens a device at all, the starting levels, and where its
//! one-line notes go.

use crate::types::Volumes;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

/// The environment variable that overrides the client's choice (`off`, `null` or `auto`).
pub const ENV_VAR: &str = "FMV_AUDIO";

/// A note goes to this, one line at a time, without the `audio: ` prefix.
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// The default logger: a line on stderr, like the rest of the client's notes. A stderr that cannot
/// be written to (closed by a launcher) loses the line instead of panicking the audio thread, which
/// `eprintln!` would.
pub fn stderr_logger() -> Logger {
    Arc::new(|line| {
        let _ = writeln!(std::io::stderr().lock(), "audio: {line}");
    })
}

/// Whether, and where, the sound plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Nothing: no device, no threads, no decoding (`--mute`, `FMV_AUDIO=off`, and headless bots).
    Off,
    /// The null sink only: the mixer runs against a clock and the samples are thrown away. Never
    /// touches a device, so it is what tests and CI use.
    Null,
    /// The default output device, and the null sink when there is none or it goes away.
    Auto,
}

impl Mode {
    /// What an `FMV_AUDIO` value means (`None` for anything not recognised). Case and surrounding
    /// spaces do not matter; an empty value is `auto`.
    pub fn parse(value: &str) -> Option<Mode> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" | "none" | "mute" | "muted" => Some(Mode::Off),
            "null" | "silent" | "dummy" => Some(Mode::Null),
            "" | "auto" | "on" | "1" | "true" | "yes" | "device" => Some(Mode::Auto),
            _ => None,
        }
    }
}

/// The mode for a run: `--mute` always means [`Mode::Off`]; otherwise `FMV_AUDIO` decides (`env`
/// is its value, if set). A value that is not recognised is `auto` plus a message to show.
pub fn resolve_mode(mute: bool, env: Option<&str>) -> (Mode, Option<String>) {
    if mute {
        return (Mode::Off, None);
    }
    match env {
        None => (Mode::Auto, None),
        Some(value) => match Mode::parse(value) {
            Some(mode) => (mode, None),
            None => (Mode::Auto, Some(format!("{ENV_VAR}=\"{value}\" is not off, null or auto; using auto"))),
        },
    }
}

/// Everything [`crate::AudioEngine::start`] takes.
#[derive(Clone)]
pub struct AudioConfig {
    pub mode: Mode,
    pub volumes: Volumes,
    /// Start muted: the mixer runs and the music moves on, but the output is silence until unmuted.
    pub muted: bool,
    /// The rate the mixer renders at without a device (the null sink, and [`crate::AudioEngine::start_manual`]);
    /// a device brings its own.
    pub sample_rate: u32,
    /// How long the supervisor waits for the device to open before it gives up and goes silent. The
    /// wait is on a thread of its own: it never delays the game.
    pub open_timeout: Duration,
    /// A device whose callback has not run for this long is treated as gone (the stream's thread
    /// died without saying so): the engine moves to the null sink.
    pub stall_timeout: Duration,
    pub logger: Logger,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            volumes: Volumes::default(),
            muted: false,
            sample_rate: 48_000,
            open_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(5),
            logger: stderr_logger(),
        }
    }
}

impl AudioConfig {
    /// The configuration of a run: [`resolve_mode`] on `--mute` and the real environment, with a
    /// note through the logger if `FMV_AUDIO` makes no sense.
    pub fn from_run(mute: bool) -> Self {
        let value = std::env::var(ENV_VAR).ok();
        let (mode, note) = resolve_mode(mute, value.as_deref());
        let config = Self { mode, ..Self::default() };
        if let Some(note) = note {
            (config.logger)(&note);
        }
        config
    }
}

impl std::fmt::Debug for AudioConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioConfig")
            .field("mode", &self.mode)
            .field("volumes", &self.volumes)
            .field("muted", &self.muted)
            .field("sample_rate", &self.sample_rate)
            .field("open_timeout", &self.open_timeout)
            .field("stall_timeout", &self.stall_timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_value_picks_the_mode() {
        for off in ["off", "OFF", " Off ", "0", "false", "no", "none", "mute"] {
            assert_eq!(Mode::parse(off), Some(Mode::Off), "{off:?}");
        }
        for null in ["null", "Silent", "dummy"] {
            assert_eq!(Mode::parse(null), Some(Mode::Null), "{null:?}");
        }
        for auto in ["", "auto", "ON", "1", "true", "device"] {
            assert_eq!(Mode::parse(auto), Some(Mode::Auto), "{auto:?}");
        }
        assert_eq!(Mode::parse("loud"), None);
    }

    #[test]
    fn mute_wins_and_an_unknown_value_means_auto_with_a_note() {
        assert_eq!(resolve_mode(true, Some("auto")), (Mode::Off, None));
        assert_eq!(resolve_mode(true, None), (Mode::Off, None));
        assert_eq!(resolve_mode(false, None), (Mode::Auto, None));
        assert_eq!(resolve_mode(false, Some("off")), (Mode::Off, None));
        assert_eq!(resolve_mode(false, Some("null")), (Mode::Null, None));
        let (mode, note) = resolve_mode(false, Some("loud"));
        assert_eq!(mode, Mode::Auto);
        assert_eq!(note.as_deref(), Some("FMV_AUDIO=\"loud\" is not off, null or auto; using auto"));
    }

    #[test]
    fn the_default_leaves_headroom_and_waits_a_while_for_a_device() {
        let config = AudioConfig::default();
        assert_eq!((config.mode, config.muted, config.sample_rate), (Mode::Auto, false, 48_000));
        assert_eq!(config.volumes, Volumes { master: 0.8, music: 0.8, sfx: 0.8 });
        assert!(config.open_timeout >= Duration::from_secs(1));
        assert!(format!("{config:?}").contains("AudioConfig"));
    }
}
