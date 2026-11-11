//! Video frame selection on a clock supplied by the caller.
//!
//! Decoding the last frame does not finish presentation: that frame must also have been shown,
//! and its authored duration must have elapsed. This module never advances a clock itself.

use crate::metadata::VideoMetadata;
use crate::MediaError;
use std::cmp::Ordering;
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

    /// The last frame due at the exact nonnegative rational time `numerator / denominator`
    /// seconds. For an audio clock these can be consumed sample frames and their sample rate.
    /// The denominator must be positive; even full-width `u128` inputs compare without
    /// overflowing, rounding to nanoseconds, or changing the timeline's presentation state.
    pub fn target_ordinal_ratio(
        &self,
        numerator: u128,
        denominator: u128,
    ) -> Result<usize, MediaError> {
        validate_denominator(denominator)?;
        // The validated first PTS is zero, so at least one frame is due at every valid time.
        Ok(self.video.frames.partition_point(|frame| {
            self.compare_tick_to_ratio(frame.pts, numerator, denominator) != Ordering::Greater
        }) - 1)
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

    /// The same completion requirements as [`Self::presented_eof`], at an exact rational
    /// number of seconds. A zero denominator is rejected even before decoder EOF or final
    /// presentation; this query never acknowledges a frame or advances a clock.
    pub fn presented_eof_ratio(
        &self,
        numerator: u128,
        denominator: u128,
    ) -> Result<bool, MediaError> {
        validate_denominator(denominator)?;
        Ok(self.decoded_eof
            && self.presented == Some(self.video.frames.len() - 1)
            && self.compare_tick_to_ratio(self.end_ticks, numerator, denominator)
                != Ordering::Greater)
    }

    fn compare_tick_to_ratio(&self, ticks: i64, numerator: u128, denominator: u128) -> Ordering {
        // Validated nonnegative i64 ticks times a u32 numerator fit within 95 bits.
        compare_ratios(
            ticks as u128 * u128::from(self.video.time_base.numerator),
            u128::from(self.video.time_base.denominator),
            numerator,
            denominator,
        )
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

fn validate_denominator(denominator: u128) -> Result<(), MediaError> {
    if denominator == 0 {
        Err(MediaError::Invalid(
            "playback time denominator must be positive".into(),
        ))
    } else {
        Ok(())
    }
}

/// Compare nonnegative fractions with positive denominators, without a cross product.
/// Equal integer parts leave only the proper fractions to compare. Taking their reciprocals
/// reverses the order; repeating this is Euclid's algorithm, so the remainders reach zero.
fn compare_ratios(mut a: u128, mut b: u128, mut c: u128, mut d: u128) -> Ordering {
    debug_assert!(b > 0 && d > 0);
    let mut reversed = false;
    loop {
        let order = (a / b).cmp(&(c / d));
        if order != Ordering::Equal {
            return if reversed { order.reverse() } else { order };
        }
        let (left, right) = (a % b, c % d);
        if left == 0 || right == 0 {
            let order = left.cmp(&right);
            return if reversed { order.reverse() } else { order };
        }
        (a, b) = (b, left);
        (c, d) = (d, right);
        reversed = !reversed;
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

    #[test]
    fn rational_clock_selects_exact_thirds_and_consumed_audio_sample_boundaries() {
        let timeline = make_timeline(1, 3, &[(0, 1), (1, 1), (2, 1)]);
        for (numerator, denominator, expected) in [
            (0, 1, 0),
            (1, 3, 1),
            (2, 3, 2),
            (7_349, 22_050, 0),
            (7_350, 22_050, 1),
            (14_699, 22_050, 1),
            (14_700, 22_050, 2),
            (22_050, 22_050, 2),
        ] {
            assert_eq!(
                timeline
                    .target_ordinal_ratio(numerator, denominator)
                    .unwrap(),
                expected,
                "{numerator}/{denominator} seconds"
            );
        }
        assert_eq!(
            timeline.target_ordinal(Duration::from_nanos(333_333_333)),
            0,
            "truncating a third of a second would select the previous frame"
        );
    }

    #[test]
    fn rational_completion_requires_presentation_and_the_opening_video_endpoint_after_audio() {
        // The inspected opening's final frame ends at 119466/1000 seconds. Its decoded audio has
        // 2632960 sample frames at 22050 Hz, which ends earlier; neither EOF alone
        // nor the shorter audio clock can stand in for the final video's full duration.
        let mut timeline = make_timeline(1, 1_000, &[(0, 66), (119_400, 66)]);
        assert!(!timeline.presented_eof_ratio(119_466, 1_000).unwrap());
        timeline.mark_decoded_eof(2).unwrap();
        assert!(!timeline.presented_eof_ratio(119_466, 1_000).unwrap());
        timeline.mark_presented(1).unwrap();
        assert_eq!(timeline.target_ordinal_ratio(2_632_960, 22_050).unwrap(), 1);
        assert!(!timeline.presented_eof_ratio(2_632_960, 22_050).unwrap());
        assert!(!timeline
            .presented_eof_ratio(119_465_999, 1_000_000)
            .unwrap());
        assert!(timeline.presented_eof_ratio(119_466, 1_000).unwrap());
        assert!(timeline
            .presented_eof_ratio(119_466_000, 1_000_000)
            .unwrap());
    }

    #[test]
    fn rational_queries_reject_zero_denominators_without_changing_completion() {
        let mut timeline = make_timeline(1, 3, &[(0, 1), (1, 1)]);
        for complete in [false, true] {
            if complete {
                timeline.mark_decoded_eof(2).unwrap();
                timeline.mark_presented(1).unwrap();
            }
            let before = (timeline.decoded_eof, timeline.presented);
            for numerator in [0, 1, u128::MAX] {
                assert!(timeline.target_ordinal_ratio(numerator, 0).is_err());
                assert!(timeline.presented_eof_ratio(numerator, 0).is_err());
            }
            assert_eq!((timeline.decoded_eof, timeline.presented), before);
            assert_eq!(timeline.presented_eof_ratio(2, 3).unwrap(), complete);
            assert_eq!(timeline.target_ordinal_ratio(1, 3).unwrap(), 1);
        }
    }

    #[test]
    fn rational_clock_accepts_full_u128_inputs_without_overflow_or_lost_boundary_bits() {
        let mut timeline = make_timeline(1, 3, &[(0, 1), (1, 1), (2, 1)]);
        let third = u128::MAX / 3; // 2^128 - 1 is exactly divisible by 3.
        assert_eq!(
            timeline.target_ordinal_ratio(third - 1, u128::MAX).unwrap(),
            0
        );
        assert_eq!(timeline.target_ordinal_ratio(third, u128::MAX).unwrap(), 1);
        assert_eq!(
            timeline
                .target_ordinal_ratio(2 * third - 1, u128::MAX)
                .unwrap(),
            1
        );
        assert_eq!(
            timeline.target_ordinal_ratio(2 * third, u128::MAX).unwrap(),
            2
        );
        timeline.mark_decoded_eof(3).unwrap();
        timeline.mark_presented(2).unwrap();
        assert!(!timeline
            .presented_eof_ratio(u128::MAX - 1, u128::MAX)
            .unwrap());
        assert!(timeline.presented_eof_ratio(u128::MAX, u128::MAX).unwrap());
        assert!(timeline.presented_eof_ratio(u128::MAX, 1).unwrap());

        let mut large = make_timeline(u32::MAX, 1, &[(0, 1), (i64::MAX - 1, 1)]);
        let last = (i64::MAX - 1) as u128 * u128::from(u32::MAX);
        assert_eq!(large.target_ordinal_ratio(last - 1, 1).unwrap(), 0);
        assert_eq!(large.target_ordinal_ratio(last, 1).unwrap(), 1);
        assert_eq!(large.target_ordinal_ratio(u128::MAX, u128::MAX).unwrap(), 0);
        large.mark_decoded_eof(2).unwrap();
        large.mark_presented(1).unwrap();
        assert!(!large.presented_eof_ratio(last, 1).unwrap());
        assert!(large.presented_eof_ratio(u128::MAX, 1).unwrap());
    }

    #[test]
    fn continued_fraction_comparison_handles_large_equivalent_and_adjacent_ratios() {
        let max = u128::MAX;
        for (a, b, c, d, expected) in [
            (0, max, 0, 1, Ordering::Equal),
            (max, max, 1, 1, Ordering::Equal),
            (max / 3, max, 1, 3, Ordering::Equal),
            (max, max - 1, max - 1, max, Ordering::Greater),
            (max - 1, max, max - 2, max - 1, Ordering::Greater),
            (max - 1, max - 2, max, max - 1, Ordering::Greater),
        ] {
            assert_eq!(compare_ratios(a, b, c, d), expected);
            assert_eq!(compare_ratios(c, d, a, b), expected.reverse());
        }
        // For small values, ordinary exact integer cross products provide an independent oracle.
        for a in 0u128..16 {
            for b in 1u128..16 {
                for c in 0u128..16 {
                    for d in 1u128..16 {
                        assert_eq!(compare_ratios(a, b, c, d), (a * d).cmp(&(c * b)));
                    }
                }
            }
        }
    }
}
