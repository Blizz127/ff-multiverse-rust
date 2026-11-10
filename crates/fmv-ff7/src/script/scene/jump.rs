//! Authored JUMP motion, in the original field's Q12 coordinates.
//!
//! Primary implementation: Xeeynamo/ff7-decomp `field_opcodes.c::OpcodeFuncJump`,
//! `field_entity.c::FieldEntityMovementUpdate`, and `field_bg.c::FieldCalcLinearStep`.
//! Source revision: `d4db3de3c3aaf61269e048cf3f6a458996290de0`,
//! <https://github.com/Xeeynamo/ff7-decomp/tree/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field>.
//! Script execution precedes movement: the issuing frame initializes step zero; the next N
//! updates advance steps 1..N; the following update marks completion, and the script consumes
//! that completion on its next pass. Thus a positive operand N blocks N+2 field frames.
//!
//! These formulas and phases are established by the PSX decompilation. FFNx's PC hooks multiply
//! the JUMP operand and divide its gravity by the square of the frame multiplier for 60 FPS,
//! corroborating the model; this is not a direct trace of the unmodified PC movement routine.

const Q12: f64 = 4096.0;

/// JUMP's signed operand is the number of movement steps. Nonpositive values have no supported
/// motion: zero divides by zero in the original; negative durations are unsupported.
pub fn jump_frames(steps: i16) -> Option<u32> {
    if steps > 0 { Some(steps as u32 + 2) } else { None }
}

/// The authored triangle's infinite plane, including destinations outside its three edges.
/// This follows FieldEntityCalculateZ's signed integer operations and truncation, not a
/// walkability query. The scene event's existing i16 height representation bounds the result.
pub(crate) fn landing_height(tri: &[crate::field::Vertex; 3], x: i16, y: i16) -> Option<i16> {
    let [[x0, y0, z0], [x1, y1, z1], [x2, y2, z2]] =
        tri.map(|p| [i32::from(p.x), i32::from(p.y), i32::from(p.z)]);
    let [ax, ay, az] = [x1 - x0, y1 - y0, z1 - z0];
    let [bx, by, bz] = [x2 - x1, y2 - y1, z2 - z1];
    let nx = by.wrapping_mul(az).wrapping_sub(ay.wrapping_mul(bz));
    let ny = bz.wrapping_mul(ax).wrapping_sub(az.wrapping_mul(bx));
    let nz = bx.wrapping_mul(ay).wrapping_sub(ax.wrapping_mul(by));
    if nz == 0 { return None; }
    let numerator = nx.wrapping_mul(x0).wrapping_add(ny.wrapping_mul(y0))
        .wrapping_add(nz.wrapping_mul(z0))
        .wrapping_sub(nx.wrapping_mul(i32::from(x))).wrapping_sub(ny.wrapping_mul(i32::from(y)));
    i16::try_from(numerator.wrapping_div(nz)).ok()
}

/// One jump. `Stage::Move::frames` for `MoveHow::Jump` is the blocked duration, N+2;
/// [`Self::from_stage_frames`] explicitly recovers the authored N rather than guessing an arc.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JumpMotion {
    from: [i32; 3],
    to: [i32; 3],
    steps: i32,
    velocity: i32,
}

impl JumpMotion {
    /// Exact original integer input, in 1/4096 field units. Arithmetic wraps at 32 bits as on
    /// the original CPU; division truncates toward zero, including downhill jumps.
    pub fn from_fixed(from: [i32; 3], to: [i32; 3], steps: i16) -> Option<Self> {
        jump_frames(steps)?;
        let steps = i32::from(steps);
        let velocity = to[2].wrapping_sub(from[2]).wrapping_div(steps).wrapping_add(8000 * steps);
        Some(Self { from, to, steps, velocity })
    }

    /// Adapt the renderer's field-space positions and the VM's blocked duration. The fixed
    /// calculation remains exact; converting its result back to f32 is a presentation boundary.
    pub fn from_stage_frames(from: [f32; 3], to: [f32; 3], frames: u32) -> Option<Self> {
        let steps = i16::try_from(frames.checked_sub(2)?).ok()?;
        let fixed = |at: [f32; 3]| -> Option<[i32; 3]> {
            let values = at.map(|n| f64::from(n) * Q12);
            values.iter().all(|v| v.is_finite() && *v >= f64::from(i32::MIN) && *v <= f64::from(i32::MAX))
                .then(|| values.map(|v| v as i32))
        };
        Self::from_fixed(fixed(from)?, fixed(to)?, steps)
    }

    pub fn frames(&self) -> u32 { self.steps as u32 + 2 }

    /// Position after a whole field frame. Step N reaches the landing; the completion update
    /// and subsequent script pass do not move it or snap away the fixed-point remainder.
    pub fn fixed_at(&self, frame: u32) -> [i32; 3] {
        let n = frame.min(self.steps as u32) as i32;
        let linear = |start: i32, target: i32| {
            let delta = target.wrapping_sub(start);
            let offset = if delta > -524_288 && delta < 524_288 {
                delta.wrapping_mul(n).wrapping_div(self.steps)
            } else {
                delta.wrapping_div(self.steps).wrapping_mul(n)
            };
            start.wrapping_add(offset)
        };
        [linear(self.from[0], self.to[0]), linear(self.from[1], self.to[1]),
            self.from[2].wrapping_add(n.wrapping_mul(self.velocity))
                .wrapping_add(n.wrapping_mul(-16000).wrapping_mul(n).wrapping_div(2))]
    }

    /// Render the original discrete field-frame samples. A late event and ordinary advancement
    /// use the same scene clock; a paused scene cannot advance this trajectory.
    pub fn at(&self, elapsed: f32) -> [f32; 3] {
        let frame = if elapsed.is_nan() || elapsed <= 0.0 { 0 } else { elapsed.floor() as u32 };
        self.fixed_at(frame).map(|n| (f64::from(n) / Q12) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_steps_have_fourteen_blocked_frames_and_the_original_apex() {
        let jump = JumpMotion::from_stage_frames([9.0, -105.0, 0.0], [9.0, -247.0, 0.0], jump_frames(12).unwrap()).unwrap();
        assert_eq!(jump.frames(), 14);
        assert_eq!(jump.at(0.0), [9.0, -105.0, 0.0]);
        assert_eq!(jump.at(0.99), jump.at(0.0));
        assert_eq!(jump.fixed_at(6), [9 * 4096, -720894, 288000]);
        assert_eq!(jump.at(6.0)[2], 70.3125);
        assert_eq!(jump.fixed_at(12), [9 * 4096, -1011708, 0]);
        assert_eq!(jump.fixed_at(13), jump.fixed_at(12));
        assert_eq!(jump.fixed_at(14), jump.fixed_at(12));
        assert_eq!(jump.at(f32::INFINITY), jump.at(12.0));
    }

    #[test]
    fn uphill_downhill_and_long_linear_steps_keep_signed_integer_rounding() {
        for z in [4096, -4096] {
            let jump = JumpMotion::from_fixed([0, 0, 0], [524288, -524288, z], 3).unwrap();
            assert_eq!(jump.fixed_at(0), [0; 3]);
            assert_eq!(jump.fixed_at(1), [174762, -174762, z / 3 + 16000]);
            assert_eq!(jump.fixed_at(3), [524286, -524286, (z / 3) * 3]);
        }
        let short = JumpMotion::from_fixed([0; 3], [524287, -524287, 0], 3).unwrap();
        assert_eq!(short.fixed_at(3), [524287, -524287, 0]);
        // The source multiplies the quadratic term before dividing by two. Simplifying it
        // algebraically to -8000*n*n would change the original 32-bit overflow behaviour.
        assert_eq!(JumpMotion::from_fixed([0; 3], [0; 3], 366).unwrap().fixed_at(366)[2], 0);
        assert_eq!(JumpMotion::from_fixed([0; 3], [0; 3], 367).unwrap().fixed_at(367)[2], i32::MIN);
    }

    #[test]
    fn invalid_operands_durations_and_positions_are_not_guessed() {
        for steps in [i16::MIN, -1, 0] { assert!(jump_frames(steps).is_none()); }
        assert_eq!(jump_frames(1), Some(3));
        assert_eq!(jump_frames(i16::MAX), Some(32769));
        for frames in [0, 1, 2, 32770, u32::MAX] {
            assert!(JumpMotion::from_stage_frames([0.0; 3], [1.0; 3], frames).is_none());
        }
        assert!(JumpMotion::from_stage_frames([f32::NAN; 3], [0.0; 3], 14).is_none());
    }

    #[test]
    fn landing_uses_the_authored_plane_even_beyond_its_edges_and_truncates_height() {
        use crate::field::Vertex;
        let tri = [Vertex { x: 0, y: 0, z: 0 }, Vertex { x: 10, y: 0, z: 5 }, Vertex { x: 0, y: 10, z: 0 }];
        assert_eq!(landing_height(&tri, 20, -50), Some(10));
        assert_eq!(landing_height(&tri, 3, 0), Some(1));
        assert_eq!(landing_height(&tri, -3, 0), Some(-1));
        assert_eq!(landing_height(&[tri[0]; 3], 0, 0), None);
    }
}
