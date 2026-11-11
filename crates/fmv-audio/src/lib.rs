//! The audio engine of the FF Multiverse client: a music channel with crossfades and loop points,
//! sound effects (one-shots, and loops that run until their handle is stopped, apart from the one-shots so that a
//! burst of those cannot cut one), three volumes and a mute, played through the machine's output device
//! (cpal) or, when there is none, through a null sink that keeps the same time.
//!
//! This crate is game-agnostic: it knows tracks only by [`TrackId`] and plays whatever a
//! [`MusicSource`] or an [`SfxClip`] holds. Decoding a music file into memory with its loop tags is
//! here ([`decode_music_file`], [`DecodedMusic`]); which file belongs to which field, battle or menu,
//! and what the game's own sound-effect files hold, are the game's business (`fmv_ff7::music` and
//! `fmv_ff7::sound`, and the client's `audio` module that decides what plays when). No audio data is
//! ever stored in this repository.
//!
//! [`MoviePcm`] plays on a dedicated finite voice alongside music and effects. A
//! [`MovieHandle`] retains its own pause/end/cancellation identity and exact whole
//! source-frame intervals consumed by mixing. This render clock is not decoder
//! read-ahead, global output time or hardware-audible time; backend latency is not
//! measured. No native movie scheduler or container timestamp validation is implied.
//!
//! # How it fits together
//!
//! ```text
//!   game thread ──commands (a queue; never blocks)──▶ mixer ◀── device callback (cpal)
//!        │                                              ▲            or the null sink's clock
//!        └── status() / set_volumes() (atomics) ────────┘
//!                                    supervisor thread: opens the device off the game's thread,
//!                                    falls back to the null sink, writes the one log line
//! ```
//!
//! * [`AudioEngine::start`] returns at once; the device opens on another thread, and the engine
//!   gives up on it after [`AudioConfig::open_timeout`]. No device, a failed open, a device that
//!   disappears and a device whose callback stops all end the same way: **one** note through
//!   [`AudioConfig::logger`], then silence, with the mixer carrying on against a clock.
//! * The mixer (internal) runs on the audio thread and never blocks, allocates (apart
//!   from freeing a finished voice) or panics out of its callback: a panic in a source is caught,
//!   counted in [`AudioStatus::faults`] and costs that pass and the mixer's state, not the game.
//! * [`Mode::Off`] (`--mute`, `FMV_AUDIO=off`) builds nothing at all: no device, no threads.
//!
//! The one dependency on the machine is ALSA at run time (`libasound.so.2`); it is only linked
//! into a binary that uses this crate with the `device` feature (on by default).
//!
//! # Tests
//!
//! Every test uses the null sink or a fake device: [`AudioEngine::start_manual`] renders exactly the
//! frames the test asks for, and fake [`DeviceOpener`]s stand in for a sound card. Nothing in the
//! suite probes the machine's real audio. To hear the real device, run
//! `cargo run -p fmv-audio --example tone`.
//!
//! # Music files
//!
//! [`decode_music_file`] decodes a whole Ogg Vorbis, WAV, PCM or ADPCM file into interleaved stereo
//! `i16` frames behind an `Arc` ([`DecodedMusic`]), so that a [`MusicSource`] over it seeks for free (which
//! a sample-exact loop seam needs), a track can be cached and a fresh source per play costs nothing. It
//! reads the loop from the Vorbis comments `LOOPSTART` and `LOOPLENGTH` (counted in samples of the file;
//! [`loop_points_from_tags`]) and ends a WAV where its `fact` chunk says (an ADPCM file pads its last block).
//! It blocks for the length of the decode, so it belongs on a worker thread. Measured on the owner's FF7
//! Steam Edition files with the ignored tests of `tests/music_real.rs` (counts only, nothing stored):
//!
//! * all 94 Ogg files and all 4 WAV files decode; 82 of the Ogg files carry loop tags, 12 do not; every
//!   Ogg decode is exactly as long as its container says;
//! * the decode agrees with ffmpeg's from the very first sample (the largest difference over 20 s of
//!   `bat`, `fanfare` and `oa` is one least-significant bit), so the tags, which count samples of the
//!   canonical decode, point at the same audio here;
//! * ffmpeg cuts the last Ogg block where the final granule position says and this decoder keeps all of it
//!   (`bat`: 439 frames, 10 ms more), which only lengthens the very end of a track and moves no loop point.
//!
//! # libasound
//!
//! With no sound card libasound itself prints eight lines on stderr before cpal says there is no device.
//! The thread that opens the device installs the `alsa` crate's thread-local error handler first, so
//! the engine's one note is the only line (checked with `cargo run -p fmv-audio --example tone` and
//! `FMV_AUDIO=auto` on a machine without a card).

#![forbid(unsafe_code)]

mod config;
mod convert;
#[cfg(feature = "device")]
pub mod device;
mod engine;
mod host;
mod mixer;
mod movie;
mod music;
mod source;
mod status;
mod types;

pub use config::{resolve_mode, stderr_logger, AudioConfig, Logger, Mode, ENV_VAR};
pub use convert::{fill_device, write_frames, DeviceSample};
pub use engine::{AudioEngine, DeviceError, DeviceLink, DeviceOpener, LostReporter, OpenedDevice};
pub use host::MixerHandle;
pub use music::{decode_music_bytes, decode_music_file, loop_points_from_tags, DecodeError, DecodedMusic, MAX_FRAMES};
pub use movie::{MovieAudioError, MovieAudioState, MovieAudioStatus, MovieHandle, MoviePcm, MAX_MOVIE_PCM_BYTES};
pub use source::{LoopPoints, MusicSource, Pull, SfxClip, ToneSource};
pub use types::{AudioStatus, Backend, PlayOptions, SfxHandle, TrackId, Volumes};
