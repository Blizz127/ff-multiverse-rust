//! Video frame selection on a clock supplied by the caller.
//!
//! Decoding the last frame does not finish presentation: that frame must also have been shown,
//! and its authored duration must have elapsed. This module never advances a clock itself.

use crate::metadata::VideoMetadata;
use crate::MediaError;
use std::sync::Arc;
use std::time::Duration;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Selects video frames and records decoded and presented completion independently.
pub struct PlaybackTimeline {
    video: Arc<VideoMetadata>,
    end_ticks: i64,
    decoded_eof: bool,
    presented: Option<usize>,
}

impl PlaybackTimeline {
    /// Validate even metadata constructed directly through its public fields.
    pub fn new(video: Arc<VideoMetadata>) -> Result<Self, MediaError> {
        video.validate_timing()?;
        video.frame_bytes()?;
        let last = video
            .frames
            .last()
            .ok_or_else(|| MediaError::Invalid("video has no frames".into()))?;
        let end_ticks = last
            .pts
            .checked_add(last.duration)
            .ok_or_else(|| MediaError::Invalid("last video frame endpoint overflows".into()))?;
        Ok(Self {
            video,
            end_ticks,
            decoded_eof: false,
            presented: None,
        })
    }

    /// The last frame whose PTS is at or before `elapsed`; hold the final frame afterwards.
    ///
    /// Comparisons retain the exact rational time base. No rounded frame rate, floating point
    /// timestamp, or truncated nanosecond duration participates in frame selection.
    pub fn target_ordinal(&self, elapsed: Duration) -> usize {
        let now = self.elapsed_units(elapsed);
        // Validation guarantees a nonempty list, first PTS zero and increasing positive PTS.
        // Thus the first frame is always due, including at Duration::ZERO.
        self.video
            .frames
            .partition_point(|frame| self.tick_units(frame.pts) <= now)
            - 1
    }

    /// Accept decoder EOF only when its complete frame count matches the inspected metadata.
    pub fn mark_decoded_eof(&mut self, frame_count: usize) -> Result<(), MediaError> {
        if frame_count != self.video.frames.len() {
            return Err(MediaError::Invalid(format!(
                "decoded video frame count {frame_count} does not match metadata count {}",
                self.video.frames.len(),
            )));
        }
        self.decoded_eof = true;
        Ok(())
    }

    /// Record a frame actually presented. Repeats and skipped frames are allowed; rewinding is not.
    pub fn mark_presented(&mut self, ordinal: usize) -> Result<(), MediaError> {
        if ordinal >= self.video.frames.len() {
            return Err(MediaError::Invalid(format!(
                "presented video frame {ordinal} is out of bounds"
            )));
        }
        if self.presented.is_some_and(|previous| ordinal < previous) {
            return Err(MediaError::Invalid(format!(
                "presented video frame {ordinal} moves backwards"
            )));
        }
        self.presented = Some(ordinal);
        Ok(())
    }

    /// True only after verified decoder EOF, final-frame presentation, and its complete duration.
    pub fn presented_eof(&self, elapsed: Duration) -> bool {
        self.decoded_eof
            && self.presented == Some(self.video.frames.len() - 1)
            && self.elapsed_units(elapsed) >= self.tick_units(self.end_ticks)
    }

    fn elapsed_units(&self, elapsed: Duration) -> u128 {
        // Even Duration::MAX in nanoseconds times a u32 denominator fits u128.
        elapsed.as_nanos() * u128::from(self.video.time_base.denominator)
    }

    fn tick_units(&self, ticks: i64) -> u128 {
        // Validation makes ticks nonnegative. i64::MAX * u32::MAX * 1e9 fits u128.
        ticks as u128 * u128::from(self.video.time_base.numerator) * NANOS_PER_SECOND
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{FrameTiming, TimeBase};

    fn video(numerator: u32, denominator: u32, timings: &[(i64, i64)]) -> VideoMetadata {
        VideoMetadata {
            width: 2,
            height: 2,
            time_base: TimeBase {
                numerator,
                denominator,
            },
            frames: timings
                .iter()
                .map(|&(pts, duration)| FrameTiming { pts, duration })
                .collect(),
        }
    }

    fn make_timeline(numerator: u32, denominator: u32, timings: &[(i64, i64)]) -> PlaybackTimeline {
        PlaybackTimeline::new(Arc::new(video(numerator, denominator, timings))).unwrap()
    }

    #[test]
    fn irregular_pts_select_the_due_frame_and_hold_the_last() {
        let timeline = make_timeline(1, 10, &[(0, 2), (2, 7), (9, 3)]);
        for (millis, expected) in [
            (0, 0),
            (199, 0),
            (200, 1),
            (899, 1),
            (900, 2),
            (1_200, 2),
            (10_000, 2),
        ] {
            assert_eq!(
                timeline.target_ordinal(Duration::from_millis(millis)),
                expected
            );
        }
        assert_eq!(timeline.target_ordinal(Duration::MAX), 2);
    }

    #[test]
    fn rational_boundaries_are_not_rounded_to_a_nanosecond_or_frame_rate() {
        let timeline = make_timeline(1, 3, &[(0, 1), (1, 1), (2, 1)]);
        assert_eq!(
            timeline.target_ordinal(Duration::from_nanos(333_333_333)),
            0
        );
        assert_eq!(
            timeline.target_ordinal(Duration::from_nanos(333_333_334)),
            1
        );
        assert_eq!(
            timeline.target_ordinal(Duration::from_nanos(666_666_666)),
            1
        );
        assert_eq!(
            timeline.target_ordinal(Duration::from_nanos(666_666_667)),
            2
        );

        let timeline = make_timeline(1_001, 30_000, &[(0, 1), (1, 1)]);
        assert_eq!(timeline.target_ordinal(Duration::from_nanos(33_366_666)), 0);
        assert_eq!(timeline.target_ordinal(Duration::from_nanos(33_366_667)), 1);
    }

    #[test]
    fn decoder_eof_presentation_and_last_duration_are_all_required() {
        let mut timeline = make_timeline(1, 3, &[(0, 1), (1, 1)]);
        assert!(!timeline.presented_eof(Duration::MAX));
        assert!(timeline.mark_decoded_eof(1).is_err());
        assert!(timeline.mark_decoded_eof(3).is_err());
        timeline.mark_presented(1).unwrap();
        assert!(
            !timeline.presented_eof(Duration::MAX),
            "presenting the last frame is not decoder EOF"
        );
        timeline.mark_decoded_eof(2).unwrap();
        timeline.mark_decoded_eof(2).unwrap();
        assert!(!timeline.presented_eof(Duration::from_nanos(666_666_666)));
        assert!(timeline.presented_eof(Duration::from_nanos(666_666_667)));

        let mut decoded_first = make_timeline(1, 1, &[(0, 1), (1, 1)]);
        decoded_first.mark_decoded_eof(2).unwrap();
        assert!(
            !decoded_first.presented_eof(Duration::MAX),
            "decoder EOF is not presentation"
        );
        decoded_first.mark_presented(0).unwrap();
        assert!(!decoded_first.presented_eof(Duration::MAX));
        decoded_first.mark_presented(1).unwrap();
        assert!(decoded_first.presented_eof(Duration::from_secs(2)));
        assert!(
            !decoded_first.presented_eof(Duration::from_secs(1)),
            "the caller still owns time"
        );
    }

    #[test]
    fn presentation_accepts_repeats_and_skips_but_rejects_backward_or_out_of_bounds() {
        let mut timeline = make_timeline(1, 1, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
        assert!(timeline.mark_presented(4).is_err());
        assert!(timeline.mark_presented(usize::MAX).is_err());
        timeline.mark_presented(0).unwrap();
        timeline.mark_presented(2).unwrap();
        timeline.mark_presented(2).unwrap();
        assert!(timeline.mark_presented(1).is_err());
        timeline.mark_decoded_eof(4).unwrap();
        assert!(!timeline.presented_eof(Duration::MAX));
        timeline.mark_presented(3).unwrap();
        assert!(timeline.mark_presented(2).is_err());
        assert!(timeline.mark_presented(4).is_err());
        assert!(
            timeline.presented_eof(Duration::from_secs(4)),
            "rejected updates do not alter the last presented frame"
        );
    }

    #[test]
    fn a_single_frame_finishes_only_after_its_duration() {
        let mut timeline = make_timeline(3, 2, &[(0, 1)]);
        assert_eq!(timeline.target_ordinal(Duration::ZERO), 0);
        timeline.mark_decoded_eof(1).unwrap();
        timeline.mark_presented(0).unwrap();
        assert!(!timeline.presented_eof(Duration::from_nanos(1_499_999_999)));
        assert!(timeline.presented_eof(Duration::from_millis(1_500)));
    }

    #[test]
    fn public_metadata_must_obey_timing_and_frame_layout_invariants() {
        for timings in [
            vec![],
            vec![(1, 1)],
            vec![(-1, 1)],
            vec![(0, 0)],
            vec![(0, -1)],
            vec![(0, 1), (0, 1)],
            vec![(0, 1), (-1, 1)],
            vec![(0, 1), (2, 1), (1, 1)],
            vec![(0, 1), (i64::MAX, 1)],
        ] {
            assert!(
                PlaybackTimeline::new(Arc::new(video(1, 1, &timings))).is_err(),
                "{timings:?}"
            );
        }
        for (numerator, denominator) in [(0, 1), (1, 0), (0, 0)] {
            assert!(
                PlaybackTimeline::new(Arc::new(video(numerator, denominator, &[(0, 1)]))).is_err()
            );
        }
        for (width, height) in [(0, 2), (2, 0)] {
            let mut invalid = video(1, 1, &[(0, 1)]);
            invalid.width = width;
            invalid.height = height;
            assert!(PlaybackTimeline::new(Arc::new(invalid)).is_err());
        }
    }

    #[test]
    fn largest_supported_values_compare_without_overflow() {
        let mut timeline = make_timeline(u32::MAX, u32::MAX, &[(0, 1), (i64::MAX - 1, 1)]);
        assert_eq!(
            timeline.target_ordinal(Duration::from_secs((i64::MAX - 2) as u64)),
            0
        );
        assert_eq!(
            timeline.target_ordinal(Duration::from_secs((i64::MAX - 1) as u64)),
            1
        );
        timeline.mark_decoded_eof(2).unwrap();
        timeline.mark_presented(1).unwrap();
        assert!(!timeline.presented_eof(Duration::from_secs((i64::MAX - 1) as u64)));
        assert!(timeline.presented_eof(Duration::from_secs(i64::MAX as u64)));
        assert!(timeline.presented_eof(Duration::MAX));
    }
}
