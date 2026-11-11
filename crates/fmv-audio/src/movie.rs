//! A finite movie voice, separate from music and the evictable sound-effect pools.
//!
//! The clock counts whole source-frame intervals consumed when the mixer produces
//! movie samples. It is neither decoder read-ahead nor hardware-audible time: the
//! backend may still buffer or discard the callback's output. Fractional resampling
//! phase is retained exactly across output-rate changes. No wall clock fills gaps.
//!
//! This accepts already-decoded continuous PCM. It does not prove that a container's
//! audio PTS are continuous or establish native FF7 movie/audio timing.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Allocation policy for one prepared stereo i16 movie, not a game-format limit.
pub const MAX_MOVIE_PCM_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MoviePcm {
    rate: u32,
    // Arc<Vec> retains the caller's allocation instead of copying a whole movie
    // into a new Arc slice. Construction validates size before allocating the Arc.
    bytes: Arc<Vec<u8>>,
}

impl MoviePcm {
    pub fn from_s16le(source_rate: u32, bytes: Vec<u8>) -> Result<Self, MovieAudioError> {
        if source_rate == 0
            || source_rate > 192_000
            || bytes.is_empty()
            || bytes.len() > MAX_MOVIE_PCM_BYTES
            || !bytes.len().is_multiple_of(4)
        {
            return Err(MovieAudioError::InvalidPcm);
        }
        Ok(Self {
            rate: source_rate,
            bytes: Arc::new(bytes),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn frames(&self) -> u64 {
        (self.bytes.len() / 4) as u64
    }

    fn sample(&self, frame: u64) -> Option<[f32; 2]> {
        let start = usize::try_from(frame).ok()?.checked_mul(4)?;
        let bytes: [u8; 4] = self
            .bytes
            .get(start..start.checked_add(4)?)?
            .try_into()
            .ok()?;
        Some([
            f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0,
            f32::from(i16::from_le_bytes([bytes[2], bytes[3]])) / 32768.0,
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieAudioError {
    InvalidPcm,
    InvalidGain,
    Disabled,
    Closed,
}

impl fmt::Display for MovieAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPcm => {
                "movie PCM requires bounded nonempty stereo i16 data and a valid sample rate"
            }
            Self::InvalidGain => "movie gain must be finite and between zero and one",
            Self::Disabled => "audio is disabled; no movie render clock exists",
            Self::Closed => "the movie audio command queue is closed",
        })
    }
}

impl std::error::Error for MovieAudioError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MovieAudioState {
    Queued,
    Playing,
    Paused,
    /// The final source interval was mixed and that whole render call succeeded.
    Ended,
    Cancelled,
    Replaced,
    Faulted,
}

impl MovieAudioState {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Queued,
            1 => Self::Playing,
            2 => Self::Paused,
            3 => Self::Ended,
            4 => Self::Cancelled,
            5 => Self::Replaced,
            _ => Self::Faulted,
        }
    }
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Ended | Self::Cancelled | Self::Replaced | Self::Faulted
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieAudioStatus {
    pub state: MovieAudioState,
    pub source_rate: u32,
    pub total_frames: u64,
    /// Whole source-frame intervals consumed by movie mixing, capped at total_frames.
    /// Sub-frame phase is retained internally. This does not measure device latency,
    /// hardware presentation, global output frames, or decoder reads.
    /// Reaching total_frames is not sufficient for EOF: wait for state Ended, since
    /// the enclosing render call can still fault and discard its output.
    pub consumed_frames: u64,
}

#[derive(Debug)]
struct Clock {
    rate: u32,
    total: u64,
    // A single coherent observation: whole source-frame position above eight state
    // bits. The PCM cap keeps the position far below the 56-bit packing bound.
    rendered: AtomicU64,
}

/// One opaque playback identity. Clones refer to the same voice; two independent
/// plays (even on different engines) never compare equal. Its final status survives
/// replacement/end and cannot be confused with the next movie's clock.
#[derive(Debug, Clone)]
pub struct MovieHandle(Arc<Clock>);

impl PartialEq for MovieHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for MovieHandle {}

impl MovieHandle {
    pub fn status(&self) -> MovieAudioStatus {
        let packed = self.0.rendered.load(Ordering::Acquire);
        MovieAudioStatus {
            state: MovieAudioState::from_byte(packed as u8),
            source_rate: self.0.rate,
            total_frames: self.0.total,
            consumed_frames: packed >> 8,
        }
    }

    fn publish(&self, state: MovieAudioState, consumed: u64) {
        self.0.rendered.store(
            (consumed.min(self.0.total) << 8) | state as u64,
            Ordering::Release,
        );
    }

    pub(crate) fn fault_rendered_callback(&self) {
        self.publish(MovieAudioState::Faulted, self.status().consumed_frames);
    }

    pub(crate) fn complete_rendered_callback(&self) {
        let status = self.status();
        if status.state == MovieAudioState::Playing && status.consumed_frames == status.total_frames
        {
            self.publish(MovieAudioState::Ended, status.total_frames);
        }
    }
}

/// Exact source position: whole + numerator/denominator. Reduction at each block
/// bounds ordinary rate changes. An adversarial series of mutually prime rates can
/// exceed u64; that explicitly faults the voice rather than rounding its clock.
#[derive(Debug, Default)]
struct Position {
    whole: u64,
    numerator: u64,
    denominator: u64,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

impl Position {
    fn step_for(&mut self, output_rate: u32, source_rate: u32) -> Option<u128> {
        if output_rate == 0 {
            return None;
        }
        if self.numerator == 0 {
            self.denominator = 1;
        } else {
            let divisor = gcd(self.numerator, self.denominator);
            self.numerator /= divisor;
            self.denominator /= divisor;
        }
        let output_rate = u64::from(output_rate);
        let denominator =
            (self.denominator / gcd(self.denominator, output_rate)).checked_mul(output_rate)?;
        self.numerator = self.numerator.checked_mul(denominator / self.denominator)?;
        self.denominator = denominator;
        Some(u128::from(source_rate) * u128::from(denominator / output_rate))
    }

    fn advance(&mut self, step: u128, total: u64) -> Option<()> {
        let sum = u128::from(self.numerator).checked_add(step)?;
        let delta = u64::try_from(sum / u128::from(self.denominator)).ok()?;
        let whole = self.whole.checked_add(delta)?;
        if whole >= total {
            self.whole = total;
            self.numerator = 0;
            self.denominator = 1;
        } else {
            self.whole = whole;
            self.numerator = u64::try_from(sum % u128::from(self.denominator)).ok()?;
        }
        Some(())
    }
}

pub(crate) struct MovieVoice {
    pcm: MoviePcm,
    gain: f32,
    handle: MovieHandle,
    position: Position,
    paused: bool,
    /// Rendering reached the end, but a later source may still fault this callback.
    /// Mixer publishes Ended only after its entire render succeeds.
    pending_end: bool,
}

impl MovieVoice {
    pub(crate) fn new(pcm: MoviePcm, gain: f32) -> Result<(Self, MovieHandle), MovieAudioError> {
        if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            return Err(MovieAudioError::InvalidGain);
        }
        let handle = MovieHandle(Arc::new(Clock {
            rate: pcm.rate,
            total: pcm.frames(),
            rendered: AtomicU64::new(MovieAudioState::Queued as u64),
        }));
        let voice = Self {
            pcm,
            gain,
            handle: handle.clone(),
            position: Position::default(),
            paused: false,
            pending_end: false,
        };
        Ok((voice, handle))
    }

    pub(crate) fn is(&self, handle: &MovieHandle) -> bool {
        self.handle == *handle
    }
    pub(crate) fn handle(&self) -> MovieHandle {
        self.handle.clone()
    }
    pub(crate) fn start(&mut self) {
        self.handle
            .publish(MovieAudioState::Playing, self.position.whole);
    }
    pub(crate) fn pause(&mut self, paused: bool) {
        self.paused = paused;
        self.handle.publish(
            if paused {
                MovieAudioState::Paused
            } else {
                MovieAudioState::Playing
            },
            self.position.whole,
        );
    }
    pub(crate) fn finish(&mut self, state: MovieAudioState) {
        self.handle.publish(state, self.position.whole);
    }

    /// Add finite movie samples into its independent accumulator. true removes the voice.
    pub(crate) fn render(&mut self, output_rate: u32, out: &mut [f32]) -> bool {
        if self.paused {
            return false;
        }
        let Some(step) = self.position.step_for(output_rate, self.pcm.rate) else {
            self.finish(MovieAudioState::Faulted);
            return true;
        };
        let total = self.pcm.frames();
        for frame in out.as_chunks_mut::<2>().0 {
            let Some(a) = self.pcm.sample(self.position.whole) else {
                self.finish(MovieAudioState::Faulted);
                return true;
            };
            let next = (self.position.whole + 1).min(total - 1);
            let Some(b) = self.pcm.sample(next) else {
                self.finish(MovieAudioState::Faulted);
                return true;
            };
            let fraction = self.position.numerator as f64 / self.position.denominator as f64;
            for ((dest, a), b) in frame.iter_mut().zip(a).zip(b) {
                *dest += (a + (b - a) * fraction as f32) * self.gain;
            }
            if self.position.advance(step, total).is_none() {
                self.finish(MovieAudioState::Faulted);
                return true;
            }
            if self.position.whole == total {
                self.pending_end = true;
                self.handle.publish(MovieAudioState::Playing, total);
                return true;
            }
        }
        self.handle
            .publish(MovieAudioState::Playing, self.position.whole);
        false
    }
}

impl Drop for MovieVoice {
    fn drop(&mut self) {
        if !self.pending_end && !self.handle.status().state.terminal() {
            self.finish(MovieAudioState::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests;
