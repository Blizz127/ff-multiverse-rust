//! What the mixer plays from: streaming music sources and in-memory sound-effect clips.
//!
//! Both are interleaved stereo `f32` (a mono file is widened by whoever decodes it), each at its
//! own sample rate: the mixer resamples, so a source never needs to know what the device runs at.
//! Loop points are in frames of the source's own rate for the same reason: they come from the
//! file's tags in samples, and the seam stays exact whatever the device does.

use std::sync::Arc;
use std::time::Duration;

/// What a [`MusicSource`] gave back for a request of frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// This many frames were written: at least one, at most the number asked for.
    Frames(usize),
    /// Nothing is ready yet (a streaming decoder fell behind). The mixer plays silence for the
    /// rest of the pass and asks again in the next one.
    Pending,
    /// The track is over.
    End,
}

/// The part of a track that repeats, in frames of the source: playback jumps from `end` back to
/// `start` without a gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPoints {
    pub start: u64,
    /// The first frame after the loop; `None` is the end of the track.
    pub end: Option<u64>,
}

/// A track the mixer streams from. It runs on the audio thread, so none of these may block, wait
/// for I/O or panic: decode ahead on another thread, or into memory, and hand the result over.
/// (A panic is caught and costs the music, not the game; see [`crate::AudioStatus::faults`].)
pub trait MusicSource: Send {
    /// The rate of the frames [`MusicSource::read`] produces, in Hz.
    fn sample_rate(&self) -> u32;

    /// Fill `out` with interleaved stereo frames (`out.len() / 2` at most) and say how many.
    fn read(&mut self, out: &mut [f32]) -> Pull;

    /// Jump to `frame`, counted from the start of the track. `false` when the source cannot seek,
    /// which also means it cannot loop. Must be cheap: a loop seam calls it.
    fn seek(&mut self, frame: u64) -> bool;

    /// The loop region the track carries, if any (the FF7 files keep it in their tags).
    fn loop_points(&self) -> Option<LoopPoints> {
        None
    }
}

/// A sound effect held in memory, ready to mix.
#[derive(Clone, Debug)]
pub struct SfxClip {
    rate: u32,
    /// Interleaved stereo.
    samples: Arc<[f32]>,
}

impl SfxClip {
    /// A clip from interleaved stereo samples at `rate` Hz (`None` for a zero rate or an odd
    /// sample count).
    pub fn from_stereo(rate: u32, interleaved: Vec<f32>) -> Option<Self> {
        if rate == 0 || !interleaved.len().is_multiple_of(2) {
            return None;
        }
        Some(Self { rate, samples: interleaved.into() })
    }

    /// A clip from mono samples at `rate` Hz, played equally on both sides (`None` for a zero rate).
    pub fn from_mono(rate: u32, mono: &[f32]) -> Option<Self> {
        let both: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        Self::from_stereo(rate, both)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Length in frames.
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames() as f64 / f64::from(self.rate))
    }

    pub(crate) fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// A sine tone, for the `tone` example, for checking a device by ear and for tests.
#[derive(Debug, Clone)]
pub struct ToneSource {
    rate: u32,
    /// Radians per frame.
    step: f64,
    phase: f64,
    amp: f32,
    /// Frames in the whole tone (`None`: it never ends).
    total: Option<u64>,
    /// The next frame's index.
    pos: u64,
}

impl ToneSource {
    /// A tone of `hz` at amplitude `amp` (`0.0..=1.0`), `length` long or endless.
    pub fn new(rate: u32, hz: f32, amp: f32, length: Option<Duration>) -> Self {
        let rate = rate.max(1);
        Self {
            rate,
            step: std::f64::consts::TAU * f64::from(hz) / f64::from(rate),
            phase: 0.0,
            amp,
            total: length.map(|d| (d.as_secs_f64() * f64::from(rate)).round() as u64),
            pos: 0,
        }
    }
}

impl MusicSource for ToneSource {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn read(&mut self, out: &mut [f32]) -> Pull {
        let asked = out.len() / 2;
        if asked == 0 {
            return Pull::Pending;
        }
        let left = self.total.map_or(u64::MAX, |t| t.saturating_sub(self.pos));
        if left == 0 {
            return Pull::End;
        }
        let n = usize::try_from(left).map_or(asked, |l| l.min(asked));
        for frame in out.as_chunks_mut::<2>().0.iter_mut().take(n) {
            let v = self.amp * (self.phase as f32).sin();
            frame[0] = v;
            frame[1] = v;
            self.phase = (self.phase + self.step) % std::f64::consts::TAU;
        }
        self.pos += n as u64;
        Pull::Frames(n)
    }

    fn seek(&mut self, frame: u64) -> bool {
        self.pos = frame;
        self.phase = (self.step * frame as f64) % std::f64::consts::TAU;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_needs_a_rate_and_whole_frames() {
        assert!(SfxClip::from_stereo(0, vec![0.0, 0.0]).is_none());
        assert!(SfxClip::from_stereo(22_050, vec![0.0; 3]).is_none());
        assert!(SfxClip::from_mono(0, &[0.0]).is_none());
        let clip = SfxClip::from_mono(22_050, &[0.25, -0.5]).expect("a mono clip");
        assert_eq!(clip.frames(), 2);
        assert_eq!(clip.samples(), &[0.25, 0.25, -0.5, -0.5]);
        assert_eq!(clip.rate(), 22_050);
        assert_eq!(SfxClip::from_stereo(100, vec![0.0; 200]).map(|c| c.duration()), Some(Duration::from_secs(1)));
    }

    #[test]
    fn a_finite_tone_ends_after_its_length_and_can_seek() {
        let mut tone = ToneSource::new(1_000, 100.0, 1.0, Some(Duration::from_millis(10)));
        let mut buf = [0.0f32; 40];
        assert_eq!(tone.read(&mut buf), Pull::Frames(10));
        assert_eq!(tone.read(&mut buf), Pull::End);
        assert!(tone.seek(5));
        assert_eq!(tone.read(&mut buf), Pull::Frames(5));
        let mut forever = ToneSource::new(1_000, 100.0, 0.5, None);
        assert_eq!(forever.read(&mut buf), Pull::Frames(20));
        assert!(buf.iter().all(|v| v.abs() <= 0.5 + 1e-6));
        assert_eq!(forever.read(&mut buf[..1]), Pull::Pending, "a request for no whole frame is not an end");
    }
}
