//! Plain types the callers, the mixer and the engine share.

use std::time::Duration;

/// A piece of music, named by the game that owns the files. The engine never interprets it: it is
/// only carried through to [`crate::AudioStatus::music`] so a caller can ask what is playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u32);

/// Output levels, each in `0.0..=1.0` (there is no gain above unity: the sum is hard limited, and
/// a level above one would only make that limiter audible).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volumes {
    /// Applied last, to everything.
    pub master: f32,
    /// The music channel.
    pub music: f32,
    /// The sound-effect channel.
    pub sfx: f32,
}

impl Default for Volumes {
    /// A little under full scale: music is mastered hot and the effects are added on top of it.
    fn default() -> Self {
        Self { master: 0.8, music: 0.8, sfx: 0.8 }
    }
}

impl Volumes {
    /// Every level forced into `0.0..=1.0`; a NaN becomes silence.
    pub fn clamped(self) -> Self {
        let level = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        Self { master: level(self.master), music: level(self.music), sfx: level(self.sfx) }
    }
}

/// Where the sound goes right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Audio is switched off (`--mute`, `FMV_AUDIO=off`): nothing is opened, decoded or mixed.
    Off,
    /// The output device is being opened (off the caller's thread; it can take a while).
    Opening,
    /// Playing through a real output device.
    Device,
    /// No device: the mixer runs against a clock and the samples are thrown away, so that track
    /// ids, fades and track ends behave exactly as they would with a device.
    Null,
}

impl Backend {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Backend::Off => 0,
            Backend::Opening => 1,
            Backend::Device => 2,
            Backend::Null => 3,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Backend::Opening,
            2 => Backend::Device,
            3 => Backend::Null,
            _ => Backend::Off,
        }
    }
}

/// An effect's stop handle, returned by [`crate::AudioEngine::play_sfx_loop`] or
/// [`crate::AudioEngine::play_sfx_controlled`] and accepted by [`crate::AudioEngine::stop_sfx_loop`].
/// Opaque; a handle whose effect has ended is stale and stopping it again does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SfxHandle(pub(crate) u64);

/// How to start a piece of music (see [`crate::AudioEngine::play_music`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayOptions {
    /// The new track fades in and the one it replaces fades out over this long (equal power).
    /// Zero is a hard cut: nothing is smoothed, so the very first frame is the track's first frame.
    pub fade: Duration,
    /// Loop: between the source's loop points, or the whole track when it has none. Off for a
    /// one-shot jingle, which then ends (and counts in [`crate::AudioStatus::music_ended`]).
    pub repeat: bool,
    /// Extra gain for this track alone, `0.0..=1.0`, for evening out loud masters.
    pub gain: f32,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self { fade: Duration::from_secs(1), repeat: true, gain: 1.0 }
    }
}

/// A snapshot of the engine, read without blocking anything (see [`crate::AudioEngine::status`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioStatus {
    pub backend: Backend,
    /// The output device's name, while [`Backend::Device`].
    pub device: Option<String>,
    /// The rate the mixer renders at, in Hz (the device's, or the null sink's own).
    pub sample_rate: u32,
    /// The track that is playing, as of the mixer's last pass. `None` when nothing is, and from the
    /// moment a stop is applied (a track that is only fading out no longer counts).
    pub music: Option<TrackId>,
    /// One-shot tracks that played to their end (not stopped, not replaced).
    pub music_ended: u64,
    /// One-shot sound effects playing now.
    pub sfx_playing: u32,
    /// Looping sound effects playing now (a loop that is fading out still counts until it is over).
    pub loops_playing: u32,
    pub volumes: Volumes,
    pub muted: bool,
    /// Frames the mixer has rendered since the engine started.
    pub frames_rendered: u64,
    /// Passes in which a source had nothing ready (it played silence meanwhile).
    pub underruns: u64,
    /// Panics the render path caught (a faulty source): each one silenced that pass and reset the mixer.
    pub faults: u32,
}
