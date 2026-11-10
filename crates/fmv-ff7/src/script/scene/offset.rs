//! Type-1 OFST changes a model's render offset independently of its logical position.
//!
//! The [native opcode](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L3419)
//! replaces the target and captures the current integer offset as the start. The
//! [linear helper](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_bg.c#L332)
//! divides the signed product with truncation toward zero. The
//! [model updater](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_entity.c#L162)
//! evaluates steps `0..=duration`, marking completion after the endpoint update.
//!
//! This helper accepts only positive signed-16 durations. Zero reaches an unguarded
//! native division; negative operands become large unsigned durations in the native
//! state and can overflow its signed-32 product. Neither case is approximated here.
//! For the supported range, the largest product is `65535 * 32767 = 2147385345`,
//! which fits in `i32`. Type-2 table-based interpolation is outside this helper.

/// A type-1 model offset motion with a positive signed-16 duration.
/// Construct with [`OffsetMotion::new`] to validate the duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetMotion {
    from: [i16; 3],
    to: [i16; 3],
    steps: u16,
}

impl OffsetMotion {
    /// Start at the current integer offset and replace it with `to` over `duration` steps.
    pub fn new(from: [i16; 3], to: [i16; 3], duration: i16) -> Option<Self> {
        (duration > 0).then_some(Self { from, to, steps: duration as u16 })
    }

    pub fn start(self) -> [i16; 3] { self.from }

    pub fn steps(self) -> u16 { self.steps }

    /// The native integer offset at a step, holding the endpoint after completion.
    pub fn at(self, step: u32) -> [i16; 3] {
        let step = step.min(u32::from(self.steps)) as i32;
        let duration = i32::from(self.steps);
        let coordinate = |from: i16, to: i16| {
            let start = i32::from(from);
            let delta = i32::from(to) - start;
            (start + delta * step / duration) as i16
        };
        let [x0, y0, z0] = self.from;
        let [x1, y1, z1] = self.to;
        [coordinate(x0, x1), coordinate(y0, y1), coordinate(z0, z1)]
    }

    /// Updates through the endpoint, including step zero before the first increment.
    pub fn completion_frames(self) -> u32 { u32::from(self.steps) + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_team_talk_offsets_follow_the_original_signed_integer_steps() {
        let motion = OffsetMotion::new([0; 3], [-202, 184, 0], 50).unwrap();
        for (step, expected) in [
            (0, [0, 0, 0]),
            (1, [-4, 3, 0]),
            (10, [-40, 36, 0]),
            (25, [-101, 92, 0]),
            (49, [-197, 180, 0]),
            (50, [-202, 184, 0]),
        ] {
            assert_eq!(motion.at(step), expected, "step {step}");
        }
        assert_eq!(motion.completion_frames(), 51);
    }

    #[test]
    fn signed_deltas_truncate_toward_zero_before_being_added_to_the_start() {
        let motion = OffsetMotion::new([100, -100, -7], [-101, 101, -2], 10).unwrap();
        assert_eq!(motion.at(1), [80, -80, -7]);
        assert_eq!(motion.at(5), [0, 0, -5]);
        assert_eq!(motion.at(9), [-80, 80, -3]);
        assert_eq!(motion.at(10), [-101, 101, -2]);
    }

    #[test]
    fn extreme_endpoints_and_the_longest_supported_duration_fit_signed_arithmetic() {
        let from = [i16::MIN, i16::MAX, -123];
        let to = [i16::MAX, i16::MIN, 123];
        let motion = OffsetMotion::new(from, to, i16::MAX).unwrap();
        assert_eq!(motion.at(0), from);
        assert_eq!(motion.at(1), [-32766, 32765, -123]);
        assert_eq!(motion.at(16383), [-2, 1, -1]);
        assert_eq!(motion.at(16384), [0, -1, 0]);
        assert_eq!(motion.at(32767), to);
        assert_eq!(motion.at(u32::MAX), to, "a late update holds the endpoint");
        assert_eq!(motion.completion_frames(), 32768);
    }

    #[test]
    fn only_positive_signed_durations_are_supported() {
        for duration in [i16::MIN, -50, -1, 0] {
            assert_eq!(OffsetMotion::new([0; 3], [1; 3], duration), None);
        }
        let motion = OffsetMotion::new([-1; 3], [1; 3], 1).unwrap();
        assert_eq!(motion.at(0), [-1; 3]);
        assert_eq!(motion.at(1), [1; 3]);
        assert_eq!(motion.at(u32::MAX), [1; 3]);
        assert_eq!(motion.completion_frames(), 2);
    }
}
