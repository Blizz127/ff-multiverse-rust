//! Read-only movie decoding, independent of a game, window, audio device or script VM.
//!
//! An explicitly selected ffprobe records decoded video timestamps; ffmpeg emits matching
//! RGBA frames with timestamp passthrough and finite stereo PCM. The worker bounds its
//! metadata, PCM, queued frames, stderr and subprocess lifetime. It never installs tools,
//! writes decoded assets, seeks into a movie, or derives a clock from decoding speed.
//!
//! [`Event::DecodedEof`] means both decoders exited successfully and the video count
//! matches the timestamp inventory. It is NOT playback completion. [`PlaybackTimeline`]
//! accepts an external playback position and explicit presentation acknowledgements.
//! No authoritative audio-device clock, native FF7 movie scheduler, GPU rendering, movie
//! registration or save/admission behavior is implemented here.
//!
//! The initial bounded profile requires one video and one stereo audio stream, both
//! starting at timestamp zero. Other layouts fail explicitly; they are not normalized.
//! Timestamps come from the selected decoder, not an inferred constant frame duration.
//! Every decoded audio frame retains its source PTS and exact PCM range. A separate
//! one-source-tick observation test classifies compatibility with a continuous clock;
//! valid discontinuous media still decodes, but simple-clock admission refuses it.
//! No timestamp is normalized and no silence is inserted to conceal gaps or overlaps.
//! Presentation completion here is video-only; a future movie controller must also
//! require successful audio playback completion. Limits bound our buffers, not total
//! memory inside the external codec process. The subprocess reader currently requires Unix.

#![forbid(unsafe_code)]

mod metadata;
mod process;
mod timeline;
mod worker;

pub use metadata::{
    AudioContinuity, AudioFrameTiming, AudioMetadata, FrameTiming, MediaMetadata, TimeBase,
    VideoMetadata,
};
pub use timeline::PlaybackTimeline;
pub use worker::{DecodedAudio, DecodedFrame, Event, MovieWorker, Request, Tools};

use std::fmt;
use std::time::Duration;

/// Resource policy of this external-decoder worker, not limits of the original game.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_media_bytes: u64,
    pub max_width: u32,
    pub max_height: u32,
    pub max_frames: usize,
    pub max_audio_frames: usize,
    pub max_frame_bytes: usize,
    pub max_audio_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_stderr_bytes: usize,
    /// Worker-owned queue only. A consumer that retains frames owns that extra memory.
    pub queued_frames: usize,
    /// Includes preparation, decoding and consumer backpressure. Expiry kills/reaps
    /// this worker's current subprocess and returns an error, never a successful EOF.
    pub lifetime: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_media_bytes: 512 * 1024 * 1024,
            max_width: 2048,
            max_height: 2048,
            max_frames: 20_000,
            max_audio_frames: 40_000,
            max_frame_bytes: 16 * 1024 * 1024,
            max_audio_bytes: 32 * 1024 * 1024,
            max_metadata_bytes: 4 * 1024 * 1024,
            max_stderr_bytes: 16 * 1024,
            queued_frames: 2,
            lifetime: Duration::from_secs(300),
        }
    }
}

impl Limits {
    pub(crate) fn validate(&self) -> Result<(), MediaError> {
        if self.max_media_bytes == 0
            || self.max_media_bytes > 2 * 1024 * 1024 * 1024
            || self.max_width == 0
            || self.max_width > 4096
            || self.max_height == 0
            || self.max_height > 4096
            || self.max_frames == 0
            || self.max_frames > 100_000
            || self.max_audio_frames == 0
            || self.max_audio_frames > 100_000
            || self.max_frame_bytes == 0
            || self.max_frame_bytes > 64 * 1024 * 1024
            || self.max_audio_bytes == 0
            || self.max_audio_bytes > 128 * 1024 * 1024
            || self.max_metadata_bytes == 0
            || self.max_metadata_bytes > 16 * 1024 * 1024
            || self.max_stderr_bytes == 0
            || self.max_stderr_bytes > 64 * 1024
            || self.queued_frames == 0
            || self.queued_frames > 4
            || self.lifetime.is_zero()
            || self.lifetime > Duration::from_secs(600)
        {
            return Err(MediaError::Invalid(
                "worker limits exceed the bounded profile".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    Invalid(String),
    Io(String),
    Limit(&'static str),
    ToolFailed {
        tool: String,
        code: Option<i32>,
        stderr: String,
    },
    Cancelled,
    TimedOut,
    WorkerPanic,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid movie input: {s}"),
            Self::Io(s) => write!(f, "movie I/O: {s}"),
            Self::Limit(s) => write!(f, "movie resource limit: {s}"),
            Self::ToolFailed { tool, code, stderr } => {
                write!(f, "{tool} failed ({code:?}): {stderr}")
            }
            Self::Cancelled => write!(f, "movie decoding cancelled"),
            Self::TimedOut => write!(f, "movie decoding exceeded its lifetime"),
            Self::WorkerPanic => write!(f, "movie worker thread panicked"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<std::io::Error> for MediaError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}
