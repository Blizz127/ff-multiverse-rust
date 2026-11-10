//! Field-screen shake, independent of the camera's follow/scroll target.
//!
//! The axis mask, unsigned operands and preserved phase follow the [PS1 opcode handler] and
//! [FFNx handler]. The per-axis segment machine follows [FieldBGShakeUpdate]: disabling an axis
//! finishes its current segment and then returns to zero. A segment's step count is not the
//! duration of the effect. Call `advance` in field frames (30 per second), applying instructions
//! in their authored order. The original field loop advances shake while dialogue is open;
//! whether the caller supplies that time is separate from this state machine.
//!
//! `Default` starts the private random-table indices at zero, as at original process startup.
//! The caller carries them across scene/field changes with `rng_indices` and `with_rng_indices`,
//! resetting the visual state while retaining the random stream's position. This does not recover
//! any effect history that the caller has not played. Easing uses the published fixed-point
//! direction table, verified against all 256 entries in the pinned field RAM dump. This state
//! is deterministic and never consumes the interpreter's gameplay random stream.
//! Enabled zero-step instructions are rejected: the original easing divides by its step count.
//!
//! [PS1 opcode handler]: https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L6354
//! [FFNx handler]: https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/opcode.cpp#L114
//! [FieldBGShakeUpdate]: https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_bg.c#L367

use std::collections::HashMap;

/// The independent X and Y shakes. Offsets are whole field-screen pixels (+X right, +Y down).
#[derive(Debug, Clone, Default)]
pub struct ShakeState {
    axes: [Axis; 2],
    fraction: f64,
}

impl ShakeState {
    /// Reset the visual state while retaining the independent axes' random-table positions.
    pub fn with_rng_indices(indices: [u8; 2]) -> Self {
        Self { axes: indices.map(|rng_index| Axis { rng_index, ..Axis::default() }), fraction: 0.0 }
    }

    /// Carry these indices into the next scene or field; a fresh process starts at `[0, 0]`.
    pub fn rng_indices(&self) -> [u8; 2] {
        self.axes.map(|axis| axis.rng_index)
    }

    /// Apply SHAKE without restarting either axis's phase. Disabled-axis operands are ignored.
    /// Returns false, preserving both axes, if an enabled axis has zero segment steps.
    pub fn apply(&mut self, mask: u8, x_amplitude: u8, x_steps: u8, y_amplitude: u8, y_steps: u8) -> bool {
        if (mask & 1 != 0 && x_steps == 0) || (mask & 2 != 0 && y_steps == 0) {
            return false;
        }
        for (index, (amplitude, steps)) in [(x_amplitude, x_steps), (y_amplitude, y_steps)].into_iter().enumerate() {
            let axis = &mut self.axes[index];
            axis.enabled = mask & (1 << index) != 0;
            if axis.enabled {
                axis.amplitude = amplitude;
                axis.steps = i16::from(steps);
            }
        }
        true
    }

    /// Advance by field frames. Invalid or negative time does nothing; partial frames accumulate.
    pub fn advance(&mut self, frames: f32) {
        if !frames.is_finite() || frames <= 0.0 { return; }
        // Add fractions separately: a huge whole count must neither erase a pending partial
        // frame nor saturate when converted to an integer. Every finite f32 is exact in f64.
        let frames = f64::from(frames);
        let fraction = self.fraction + frames.fract();
        self.fraction = fraction.fract();
        for axis in &mut self.axes {
            if fraction >= 1.0 { axis.tick(); }
            axis.advance_whole(frames.floor());
        }
    }

    /// Whole field-screen pixels, to be added after the normal camera has been clamped.
    pub fn offset(&self) -> [f32; 2] {
        self.axes.map(|axis| f32::from(axis.offset))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Axis {
    enabled: bool,
    active: bool,
    rng_index: u8,
    offset: i8,
    amplitude: u8,
    start: i16,
    target: i16,
    steps: i16,
    step: i16,
}

impl Axis {
    fn settled(&self) -> bool {
        !self.enabled && !self.active && self.step == self.steps
    }

    fn advance_whole(&mut self, ticks: f64) {
        if ticks < 1.0 { return; }
        if self.settled() {
            self.offset = 0;
            return;
        }
        if ticks <= 1024.0 {
            for _ in 0..ticks as u16 { self.tick(); }
            return;
        }

        // An enabled axis has a finite deterministic state. Record only segment boundaries,
        // including their retained offset: the RNG byte wraps and targets alternate according
        // to the preceding sign. Work per axis so different durations cannot produce a large
        // combined period. Ordinary frame updates never allocate this table. The table's zero
        // sample erases the incoming sign once per 256 samples; two more boundaries replace
        // both endpoints and the retained offset. A cycle is found within 133,000 ticks for
        // any accepted duration, then at most one period (65,792 ticks) remains to simulate.
        let mut seen = HashMap::new();
        let mut spent = 0u64;
        while (spent as f64) < ticks {
            if self.settled() {
                self.offset = 0;
                return;
            }
            if self.step == 0 {
                if let Some(previous) = seen.insert(*self, spent) {
                    let period = spent - previous;
                    // Subtracting spent from a huge f64 would lose that prefix. Reduce each
                    // integer separately before subtracting; the remainder is small and exact.
                    let remaining = ((ticks % period as f64) - (spent % period) as f64)
                        .rem_euclid(period as f64) as u64;
                    for _ in 0..remaining { self.tick(); }
                    return;
                }
            }
            self.tick();
            spent += 1;
        }
    }

    fn random_target(&mut self) -> i16 {
        // The original narrows the product before division, including at large amplitudes.
        let product = (u16::from(RANDOM[usize::from(self.rng_index)]) * u16::from(self.amplitude)) as i16;
        self.rng_index = self.rng_index.wrapping_add(1);
        product / 256
    }

    fn interpolate(&mut self) {
        // Retiming an inactive axis and disabling it before the next tick can leave step above
        // steps. The original signed 16-bit counter then wraps before it reaches steps again.
        self.step = self.step.wrapping_add(1);
        self.offset = ease(self.start, self.target, self.steps, self.step) as i8;
    }

    fn tick(&mut self) {
        if self.enabled {
            if !self.active {
                self.step = 0;
                self.start = 0;
                self.target = self.random_target();
                self.active = true;
            } else if self.steps < self.step {
                self.step = 0;
                self.start = self.target;
                let target = self.random_target();
                self.target = if self.start < 0 { target } else { -target };
            } else {
                self.interpolate();
            }
        } else if self.active {
            if self.steps < self.step {
                self.step = 0;
                self.start = self.target;
                self.target = 0;
                self.rng_index = self.rng_index.wrapping_add(1);
                self.active = false;
            } else {
                self.interpolate();
            }
        } else if self.steps == self.step {
            self.offset = 0;
        } else {
            self.interpolate();
        }
    }
}

fn ease(start: i16, target: i16, steps: i16, step: i16) -> i32 {
    debug_assert!(steps > 0);
    // Preserve the integer angle arithmetic, its byte wrap and signed truncation of the result.
    let angle = (((i32::from(step) << 12) / i32::from(steps)) / 32 + 128) as u8;
    let cosine = i32::from(FIELD_COS[usize::from(angle)]);
    i32::from(start) + (cosine + 4096) * (i32::from(target) - i32::from(start)) / 8192
}

// Y components of the published FF7 direction table, not a floating-point cosine approximation.
// All 256 complete words match the pinned field RAM dump: 1024 bytes at file offset 0xDF120,
// virtual address 0x800DF120. SHA-256 of those bytes:
// 9fde688e1e28bf110c18381cac3eb81497b9151cfb8c62bb0f8b117426f70531.
// https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/mini/chocobo/sincos.h
// https://github.com/q-gears/q-gears-reversing-data/blob/2b155a8c5455f5fc8addd2eda9e5cd4c226abe34/reversing/ffvii/ffvii_field/ffvii_field_ram.bin
const FIELD_COS: [i16; 256] = [
    4096, 4094, 4091, 4084, 4076, 4065, 4051, 4035, 4017, 3996, 3973, 3947, 3919, 3889, 3856, 3821,
    3784, 3744, 3702, 3658, 3612, 3563, 3513, 3460, 3405, 3348, 3289, 3229, 3166, 3101, 3034, 2966,
    2896, 2824, 2750, 2675, 2598, 2519, 2439, 2358, 2275, 2191, 2105, 2018, 1930, 1841, 1751, 1659,
    1567, 1474, 1379, 1284, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 200, 100,
    0, -100, -200, -301, -401, -501, -601, -700, -799, -897, -995, -1092, -1189, -1284, -1379, -1474,
    -1567, -1659, -1751, -1841, -1930, -2018, -2105, -2191, -2275, -2358, -2439, -2519, -2598, -2675, -2750, -2824,
    -2896, -2966, -3034, -3101, -3166, -3229, -3289, -3348, -3405, -3460, -3513, -3563, -3612, -3658, -3702, -3744,
    -3784, -3821, -3856, -3889, -3919, -3947, -3973, -3996, -4017, -4035, -4051, -4065, -4076, -4084, -4091, -4094,
    -4096, -4094, -4091, -4084, -4076, -4065, -4051, -4035, -4017, -3996, -3973, -3947, -3919, -3889, -3856, -3821,
    -3784, -3744, -3702, -3658, -3612, -3563, -3513, -3460, -3405, -3348, -3289, -3229, -3166, -3101, -3034, -2966,
    -2896, -2824, -2750, -2675, -2598, -2519, -2439, -2358, -2275, -2191, -2105, -2018, -1930, -1841, -1751, -1659,
    -1567, -1474, -1379, -1284, -1189, -1092, -995, -897, -799, -700, -601, -501, -401, -301, -200, -100,
    0, 100, 200, 301, 401, 501, 601, 700, 799, 897, 995, 1092, 1189, 1284, 1379, 1474,
    1567, 1659, 1751, 1841, 1930, 2018, 2105, 2191, 2275, 2358, 2439, 2519, 2598, 2675, 2750, 2824,
    2896, 2966, 3034, 3101, 3166, 3229, 3289, 3348, 3405, 3460, 3513, 3563, 3612, 3658, 3702, 3744,
    3784, 3821, 3856, 3889, 3919, 3947, 3973, 3996, 4017, 4035, 4051, 4065, 4076, 4084, 4091, 4094,
];

// The fixed algorithm table, independent of the script RANDOM cursor.
// https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_rand.c#L5
const RANDOM: [u8; 256] = [
    177, 202, 238, 108, 90, 113, 46, 85, 214, 0, 204, 153, 144, 107, 125, 235, 79, 160, 7, 172, 223, 138,
    86, 158, 241, 154, 99, 117, 17, 145, 163, 184, 148, 115, 247, 84, 217, 110, 114, 192, 244, 128, 222, 185,
    187, 141, 102, 38, 208, 54, 225, 233, 112, 220, 205, 47, 74, 103, 93, 210, 96, 181, 157, 127, 69, 55,
    80, 68, 120, 4, 25, 44, 239, 253, 100, 129, 3, 218, 149, 76, 122, 11, 173, 31, 186, 221, 62, 249,
    215, 26, 41, 248, 24, 179, 32, 246, 209, 94, 52, 146, 123, 36, 67, 136, 151, 212, 15, 53, 170, 131,
    104, 39, 168, 213, 190, 250, 20, 49, 175, 16, 13, 216, 106, 206, 35, 97, 243, 61, 164, 8, 51, 227,
    169, 56, 230, 147, 29, 28, 240, 14, 135, 89, 101, 130, 188, 255, 254, 126, 143, 193, 30, 245, 203, 73,
    2, 50, 9, 196, 142, 198, 43, 64, 167, 23, 118, 59, 22, 42, 200, 251, 178, 88, 165, 21, 174, 37,
    207, 70, 199, 72, 180, 10, 63, 201, 6, 133, 81, 137, 98, 77, 18, 140, 234, 162, 152, 75, 121, 111,
    92, 71, 48, 27, 231, 197, 34, 156, 232, 150, 58, 228, 124, 224, 105, 161, 183, 5, 57, 116, 1, 159,
    189, 195, 132, 252, 119, 134, 19, 78, 191, 242, 83, 91, 237, 33, 139, 109, 194, 65, 182, 219, 60, 211,
    40, 236, 45, 226, 155, 166, 66, 82, 87, 95, 229, 171, 176, 12,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_updates_match_individual_frames_including_long_segments_and_extreme_amplitudes() {
        for (x_amplitude, x_steps, y_amplitude, y_steps) in [(0, 1, 255, 255), (255, 255, 128, 1), (20, 4, 5, 2)] {
            for ticks in [1025, 67_533] {
                let mut bulk = ShakeState::default();
                assert!(bulk.apply(3, x_amplitude, x_steps, y_amplitude, y_steps));
                bulk.advance(19.75);
                let mut single = bulk.clone();
                bulk.advance(ticks as f32);
                for _ in 0..ticks { single.advance(1.0); }
                assert_eq!(bulk.axes, single.axes, "{x_amplitude}/{x_steps}, {y_amplitude}/{y_steps}, {ticks}");
                assert_eq!(bulk.fraction, single.fraction);
            }
        }
    }

    #[test]
    fn disabling_an_inactive_retimed_axis_preserves_the_original_counter_wrap() {
        let mut shake = ShakeState::default();
        assert!(shake.apply(1, 20, 8, 0, 0));
        shake.advance(4.0);
        assert!(shake.apply(0, 0, 0, 0, 0));
        shake.advance(100.0);
        assert!(shake.axes[0].settled());
        assert_eq!(shake.axes[0].step, 8);
        // Both instructions happen between ticks, preserving the inactive phase at step 8.
        assert!(shake.apply(1, 20, 2, 0, 0));
        assert!(shake.apply(0, 0, 0, 0, 0));
        let indices = shake.rng_indices();
        shake.advance(32_760.0);
        assert_eq!(shake.axes[0].step, i16::MIN);
        shake.advance(f32::MAX);
        assert_eq!(shake.axes[0].step, 2);
        assert_eq!(shake.offset(), [0.0, 0.0]);
        assert_eq!(shake.rng_indices(), indices);
    }

    #[test]
    fn huge_finite_time_finishes_without_saturating_ticks_or_losing_fractional_time() {
        let mut quiet = ShakeState::default();
        quiet.advance(0.5);
        quiet.advance(1.0e30);
        assert_eq!(quiet.offset(), [0.0, 0.0]);
        assert_eq!(quiet.rng_indices(), [0, 0]);
        assert_eq!(quiet.fraction, 0.5);

        let mut start = ShakeState::default();
        assert!(start.apply(3, 20, 4, 5, 2));
        start.advance(7.5);
        // These axes repeat after 256 random entries times 6/4 ticks per segment.
        // Verify their combined 3072-tick steady period using only single-frame updates.
        let mut reference = start.clone();
        for _ in 0..3072 { reference.advance(1.0); }
        let period_start = reference.clone();
        for _ in 0..3072 { reference.advance(1.0); }
        assert_eq!(reference.axes, period_start.axes);

        let mut huge = start.clone();
        huge.advance(2.0f32.powi(100));
        // 2^100 mod 3072 = 1024; adding another whole period removes the startup prefix.
        assert_eq!((1u128 << 100) % 3072, 1024);
        for _ in 0..1024 { reference.advance(1.0); }
        assert_eq!(huge.axes, reference.axes);
        assert_eq!(huge.fraction, 0.5);
        huge.advance(0.5);
        reference.advance(0.5);
        assert_eq!(huge.axes, reference.axes);
        assert_eq!(huge.fraction, 0.0);

        // f32::MAX = (2^24 - 1) * 2^104, a multiple of 3072, beyond u64's range.
        assert_eq!((((1u128 << 24) - 1) << 104) % 3072, 0);
        let mut maximum = start;
        maximum.advance(f32::MAX);
        assert_eq!(maximum.axes, period_start.axes);
        assert_eq!(maximum.fraction, 0.5);
    }

    #[test]
    fn field_handoff_resets_the_picture_but_keeps_each_axes_next_random_sample() {
        let mut previous = ShakeState::default();
        assert!(previous.apply(3, 20, 8, 5, 2));
        previous.advance(7.5);
        assert_eq!(previous.rng_indices(), [1, 2], "different segment lengths advance independently");
        assert_ne!(previous.offset(), [0.0, 0.0]);

        let mut next = ShakeState::with_rng_indices(previous.rng_indices());
        assert_eq!(next.rng_indices(), [1, 2]);
        assert_eq!(next.offset(), [0.0, 0.0]);
        assert_eq!(next.fraction, 0.0);
        assert!(next.axes.iter().all(|axis| !axis.enabled && !axis.active));
        assert!(next.apply(3, 20, 2, 5, 2));
        next.advance(3.0);
        assert_eq!(next.offset(), [15.0, 4.0], "next samples are 202 on X and 238 on Y");
        assert_eq!(next.rng_indices(), [2, 3]);

        let mut restarted = ShakeState::default();
        assert!(restarted.apply(3, 20, 2, 5, 2));
        restarted.advance(3.0);
        assert_eq!(restarted.offset(), [13.0, 3.0], "restarting at sample 177 would lose the handoff");
    }

    #[test]
    fn easing_uses_the_published_fixed_point_values_and_integer_truncation() {
        assert_eq!(ease(0, 8192, 128, 1), 2, "the table has -4094, whereas rounded cosine gives -4095");
        assert_eq!(ease(0, 8192, 4, 1), 1200);
        assert_eq!(ease(-3, 4, 2, 1), 0, "signed division truncates toward zero");
        assert_eq!(ease(-3, 4, 2, 2), 4);
        assert_eq!(ease(-3, 4, 2, 3), 0, "the original samples one step beyond its duration");
    }

    #[test]
    fn the_train_uses_two_step_segments_until_disabled_not_a_two_frame_effect() {
        let mut state = ShakeState::default();
        assert!(state.apply(2, 0, 0, 5, 2));
        let mut offsets = Vec::new();
        for _ in 0..10 { state.advance(1.0); offsets.push(state.offset()); }
        assert_eq!(offsets, [[0.0, 0.0], [0.0, 1.0], [0.0, 3.0], [0.0, 1.0], [0.0, 1.0],
            [0.0, 0.0], [0.0, -3.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]);
        assert!(state.axes[1].enabled);
        assert!(state.apply(0, 0, 1, 0, 1));
        assert_eq!(state.axes[1].steps, 2, "a disabled-axis operand does not replace its duration");
        let mut tail = Vec::new();
        for _ in 0..6 { state.advance(1.0); tail.push(state.offset()[1]); }
        assert_eq!(tail, [4.0, 0.0, 0.0, 2.0, 0.0, 0.0], "finish the segment and ease to zero");
        state.advance(100.0);
        assert_eq!(state.offset(), [0.0, 0.0]);
    }

    #[test]
    fn stopping_one_axis_keeps_the_other_running_and_does_not_snap_the_offset() {
        let mut state = ShakeState::default();
        assert!(state.apply(3, 24, 4, 5, 2));
        state.advance(3.0);
        let stopped_at = state.offset()[0];
        assert!(stopped_at > 0.0);
        let mut uninterrupted = state.clone();
        assert!(state.apply(2, 0, 0, 5, 2));
        assert_eq!(state.offset()[0], stopped_at);
        for _ in 0..20 {
            state.advance(1.0);
            uninterrupted.advance(1.0);
            assert_eq!(state.axes[1], uninterrupted.axes[1]);
        }
        assert_eq!(state.offset()[0], 0.0);
        assert!(state.axes[1].enabled);
    }

    #[test]
    fn changing_parameters_preserves_phase_and_the_current_segment_endpoints() {
        let mut state = ShakeState::default();
        assert!(state.apply(2, 0, 0, 5, 2));
        state.advance(2.0);
        let before = state.axes[1];
        assert!(state.apply(2, 0, 0, 20, 4));
        assert_eq!(state.axes[1], Axis { amplitude: 20, steps: 4, ..before });
        state.advance(1.0);
        assert_eq!(state.axes[1].step, 2);
        assert_eq!(state.offset()[1], 1.0, "the old target remains while the duration changes");
        state.advance(4.0);
        assert_eq!(state.axes[1].target, -15, "the next target uses the new amplitude");
    }

    #[test]
    fn time_partitions_and_repeated_identical_instructions_preserve_the_same_result() {
        let mut one = ShakeState::default();
        assert!(one.apply(3, 10, 4, 5, 2));
        let mut split = one.clone();
        one.advance(32.0);
        for _ in 0..128 {
            split.advance(0.25);
            assert!(split.apply(3, 10, 4, 5, 2));
        }
        assert_eq!(one.axes, split.axes);
        assert_eq!(one.fraction, split.fraction);
    }

    #[test]
    fn invalid_time_and_enabled_zero_steps_leave_prior_state_unchanged() {
        let mut state = ShakeState::default();
        assert!(state.apply(2, 0, 0, 5, 2));
        state.advance(2.5);
        let before = state.clone();
        assert!(!state.apply(3, 20, 4, 8, 0));
        for frames in [f32::NAN, f32::INFINITY, -1.0, 0.0] { state.advance(frames); }
        assert_eq!(state.axes, before.axes);
        assert_eq!(state.fraction, before.fraction);
        assert!(state.apply(0xFC, 255, 0, 255, 0), "only the low mask bits select axes");
        state.advance(30.0);
        assert_eq!(state.offset(), [0.0, 0.0]);
    }
}
