//! The audio engine of the FF Multiverse client: a music channel with crossfades and loop points,
//! a sound-effect channel, three volumes and a mute, played through the machine's output device
//! (cpal) or, when there is none, through a null sink that keeps the same time.
//!
//! This crate is game-agnostic: it knows tracks only by [`TrackId`] and plays whatever a
//! [`MusicSource`] or an [`SfxClip`] holds. Reading the owner's own sound files, and deciding which
//! track belongs to which field, battle or menu, are the callers' business (none of that is here
//! yet), and no audio data is ever stored in this repository.
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
//! # Not built yet: the FF7 sound files (notes for the next slice)
//!
//! What the owner's Steam edition has, from a read-only listing (nothing was copied, opened or
//! decoded when this was written; the game's `data/` is nested under `ff7/workingdir/` there):
//!
//! * `music_ogg/`: 94 Ogg Vorbis files named like the classic PC tracks (`bat`, `fanfare`, `aseri`...).
//!   Their loop points are Vorbis comments `LOOPSTART` and `LOOPLENGTH`, counted in **samples**: that
//!   is [`LoopPoints`] with `start` and `end: Some(start + length)`, in the file's own rate.
//! * `music/`: four WAV files (`heart`, `sato`, `sensui`, `wind`) and a 647-byte `music.idx`.
//! * `sound/`: the classic sound-effect pair `audio.fmt` and `audio.dat`.
//!
//! How a field picks its music, measured on the owner's `flevel.lgp` (702 fields, read-only,
//! 2026-11-09; counts and ids only):
//!
//! * A field script's `MUSIC` (`0xF0`), `BMUSC` (`0xF6`, the music of the fight the next `BATTLE`
//!   starts) and `FMUSC` (`0xFC`, the music after the next fight) take a one-byte index into **that
//!   field's own list of AKAO blocks** (`fmv_ff7::script` already splits them out), not a global
//!   track number. 613 fields have blocks (1,342 in all, 1,320 of them real `"AKAO"` frames) and
//!   every one of the 1,355 distinct (field, opcode, operand) uses is a valid index into its own
//!   field's list.
//! * An AKAO frame is `"AKAO"` and a `u16` id at +4: 89 distinct values in `1..=96`. (A `u16` at +6
//!   is smaller than its block in all 1,320 frames but equals the block's length minus 8 in none, so
//!   its meaning is unconfirmed.) `md1stin` lists `[3, 2]` (`MUSIC 0`, `MUSIC 1`, `FMUSC 0`), `md1_1`
//!   `[3]`, `mds7st1` `[10, 8]` (`MUSIC 0`, `BMUSC 1`); of the 313 distinct (field, `BMUSC` operand)
//!   uses, 250 pick id 8 and 26 pick id 11.
//!
//! **\[verify\]** from here on: community pages and forum threads, not the owner's files.
//!
//! * A forum-kept list pairs the classic PC track numbers (1 to 98) with the file names: `oa` is 1,
//!   `ob` is 2, `bat` 7, `kurai` 9, `chu` 10 ... `roll` 98. The frame ids above sit exactly one higher
//!   than that list in every case checked (`md1stin` and `md1_1`, the bombing mission, hold `ob` and
//!   `oa` as 3 and 2; `mds7st1` holds `kurai` as 10 and `bat` as 8; 250 `BMUSC` uses pick id 8), so
//!   **frame id = list number + 1** is the working rule. Confirm it against
//!   `music.idx` (not looked at: it may be the game's own number-to-name list, and the 1998 release has
//!   it unchanged) and by ear before building on it.
//! * Which file is the victory fanfare (`fanfare` or `fan2`) has to be read off the real battle
//!   data, not guessed.
//! * `audio.fmt` and `audio.dat` are indexed by the field `SOUND` opcode's id; their sample format
//!   is unknown. The decoders for Vorbis, WAV, PCM and ADPCM are already compiled in
//!   (`tests/decoders.rs`).
//!
//! Plan: decode a whole music file into memory off the audio thread (a [`MusicSource`] over an
//! `Arc<[i16]>` seeks for free, which loop seams need) and read the tags for the loop points; keep
//! every file read-only and never write decoded audio anywhere.

#![forbid(unsafe_code)]

mod config;
mod convert;
#[cfg(feature = "device")]
pub mod device;
mod engine;
mod host;
mod mixer;
mod music;
mod source;
mod status;
mod types;

pub use config::{resolve_mode, stderr_logger, AudioConfig, Logger, Mode, ENV_VAR};
pub use convert::{fill_device, write_frames, DeviceSample};
pub use engine::{AudioEngine, DeviceError, DeviceLink, DeviceOpener, LostReporter, OpenedDevice};
pub use host::MixerHandle;
pub use music::{decode_music_bytes, decode_music_file, loop_points_from_tags, DecodeError, DecodedMusic, MAX_FRAMES};
pub use source::{LoopPoints, MusicSource, Pull, SfxClip, ToneSource};
pub use types::{AudioStatus, Backend, PlayOptions, TrackId, Volumes};
