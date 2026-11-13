//! Exact source-frame time and a separately named finite video tail.
use super::ControllerError;
use fmv_media::VideoMetadata;
use std::time::Duration;

/// Exact nonnegative seconds. These values are observations, never a floating
/// frame-rate estimate. Construction is private so every denominator is nonzero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTime {
    pub numerator: u128,
    pub denominator: u128,
}

impl PresentationTime {
    pub(super) const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };
    pub(super) fn samples(frames: u64, rate: u32) -> Self {
        Self {
            numerator: u128::from(frames),
            denominator: u128::from(rate),
        }
    }
    pub(super) fn at_least(self, other: Self) -> bool {
        // Admission bounds all movie times to 600s and denominators to a u32
        // timebase, or sample-rate * 1e9 in the tail. These products fit u128.
        self.numerator * other.denominator >= other.numerator * self.denominator
    }
    pub(super) fn endpoint(video: &VideoMetadata) -> Result<Self, ControllerError> {
        let last = video
            .frames
            .last()
            .ok_or(ControllerError::Clock("no video endpoint"))?;
        let end = last
            .pts
            .checked_add(last.duration)
            .ok_or(ControllerError::Clock("video endpoint overflow"))?;
        if end <= 0 || video.time_base.numerator == 0 || video.time_base.denominator == 0 {
            return Err(ControllerError::Clock("invalid video endpoint"));
        }
        let time = Self {
            numerator: end as u128 * u128::from(video.time_base.numerator),
            denominator: u128::from(video.time_base.denominator),
        };
        if time.numerator > 600 * time.denominator {
            return Err(ControllerError::Clock(
                "movie exceeds the 600 second controller policy",
            ));
        }
        Ok(time)
    }
}

/// Starts only after valid finite AudioEnded. This is an explicit silent video
/// tail, not fabricated audio consumption, decoder progress or a fallback clock.
pub(super) struct VideoTailClock {
    pub(super) elapsed: Duration,
    pub(super) observed_at: Duration,
}

impl VideoTailClock {
    pub(super) fn position(
        &self,
        audio_frames: u64,
        rate: u32,
        endpoint: PresentationTime,
    ) -> PresentationTime {
        let time = PresentationTime {
            numerator: u128::from(audio_frames) * 1_000_000_000
                + self.elapsed.as_nanos() * u128::from(rate),
            denominator: u128::from(rate) * 1_000_000_000,
        };
        if time.at_least(endpoint) {
            endpoint
        } else {
            time
        }
    }
    pub(super) fn advance(&mut self, now: Duration, paused: bool) {
        if !paused {
            // The admission contract allows at most one second of video tail;
            // saturation also bounds arithmetic after an arbitrarily late tick.
            self.elapsed = self
                .elapsed
                .saturating_add(now - self.observed_at)
                .min(Duration::from_secs(1));
        }
        self.observed_at = now;
    }
}

/// A silent movie's clock: the real time it has played, not counting the time it was paused.
/// Unlike [`VideoTailClock`] it is unbounded here; the controller stops it at the video endpoint.
pub(super) struct SilentClock {
    elapsed: Duration,
    observed_at: Duration,
}

impl SilentClock {
    pub(super) fn start(now: Duration) -> Self {
        Self { elapsed: Duration::ZERO, observed_at: now }
    }
    pub(super) fn advance(&mut self, now: Duration, paused: bool) {
        if !paused {
            self.elapsed = self.elapsed.saturating_add(now.saturating_sub(self.observed_at));
        }
        self.observed_at = self.observed_at.max(now);
    }
    pub(super) fn position(&self) -> PresentationTime {
        PresentationTime {
            numerator: self.elapsed.as_nanos(),
            denominator: 1_000_000_000,
        }
    }
}
