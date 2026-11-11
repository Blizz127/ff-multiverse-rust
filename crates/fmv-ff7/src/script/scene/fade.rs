//! Authored field fades, in the original 30 Hz field clock and display colour space.
//!
//! Operand/counter rules: [PS1 opcode implementation](https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L6215).
//! Update/blend arithmetic: [original updater analysis](https://github.com/q-gears/q-gears-reversing-data/blob/2b155a8c5455f5fc8addd2eda9e5cd4c226abe34/reversing/ffvii/ffvii_system/system_fade.txt).
//! The scene is composited before dialogue; subtracting inverse RGB is distinct from multiplying
//! by an alpha overlay (also corrected in [FFNx](https://github.com/julianxhokaxhiu/FFNx/commit/f9100649b2b5140f172a6b43d3e23b7aca52e771)).
//! Types 3/13 are system transitions, not implemented here. Nonpositive NFADE durations are
//! rejected: the original interpolator divides by that duration. FADE speed zero is a valid hold.
//!
//! The VM has no elapsed user-dialogue time, whereas the visible client advances these effects
//! while a window is open. A future FADEW after such a window can therefore wait longer in the
//! VM than in the original engine. Current registered FADEW paths precede their dialogue.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeOp {
    Fade { kind: u8, rgb: [u8; 3], speed: u8, adjust: u8 },
    Nfade { kind: u8, rgb: [u8; 3], frames: i16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeBlend { Add, Subtract, Black }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FadeColor {
    pub blend: FadeBlend,
    pub rgb: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FadeState {
    kind: u8,
    adjust: u16,
    speed: u16,
    rgb: [u8; 3],
    // NFADE interpolates from the previous completed NFADE target, not from a FADE colour.
    start: [u8; 3],
    target: [u8; 3],
    fraction: f32,
}

impl FadeState {
    /// Exact presentation state for an enclosing, versioned terminal checkpoint. The
    /// fixed 18-byte payload preserves the prior NFADE target and fractional-frame bits;
    /// a rendered colour alone cannot reproduce the next update or replacement fade.
    pub fn checkpoint_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(18);
        bytes.push(self.kind);
        bytes.extend_from_slice(&self.adjust.to_le_bytes());
        bytes.extend_from_slice(&self.speed.to_le_bytes());
        bytes.extend_from_slice(&self.rgb);
        bytes.extend_from_slice(&self.start);
        bytes.extend_from_slice(&self.target);
        bytes.extend_from_slice(&self.fraction.to_bits().to_le_bytes());
        bytes
    }

    /// Decode a bounded, safe state without advancing it. Source identity and the
    /// provenance of the recorded presentation phase belong to the enclosing checkpoint.
    pub fn from_checkpoint_bytes(bytes: &[u8]) -> Result<Self, String> {
        let bytes: [u8; 18] = bytes.try_into().map_err(|_| "fade checkpoint has the wrong length")?;
        let [kind, a0, a1, s0, s1, r, g, b, sr, sg, sb, tr, tg, tb, f0, f1, f2, f3] = bytes;
        let adjust = u16::from_le_bytes([a0, a1]);
        let speed = u16::from_le_bytes([s0, s1]);
        let fraction = f32::from_bits(u32::from_le_bytes([f0, f1, f2, f3]));
        if !fraction.is_finite() || fraction.is_sign_negative() || !(0.0..1.0).contains(&fraction) {
            return Err("fade checkpoint has an invalid fractional frame".into());
        }
        let valid = match kind {
            // Clearing a fade changes its kind before the next update and retains the
            // preceding mode's counters, including an NFADE duration up to i16::MAX.
            0 => adjust <= i16::MAX as u16 && speed <= i16::MAX as u16,
            1 | 5 | 7 | 9 => adjust <= 256 && speed <= 255,
            2 | 4 | 6 | 8 | 10 => adjust <= 255 && speed <= 255,
            11 | 12 => (1..=i16::MAX as u16).contains(&speed) && adjust <= speed,
            _ => false,
        };
        if !valid { return Err("fade checkpoint has invalid mode or counters".into()); }
        Ok(Self { kind, adjust, speed, rgb: [r, g, b], start: [sr, sg, sb], target: [tr, tg, tb], fraction })
    }

    /// Replace the one global fade. An unsupported instruction leaves the previous effect intact.
    pub fn apply(&mut self, op: FadeOp) -> bool {
        match op {
            FadeOp::Fade { kind: 0, .. } | FadeOp::Nfade { kind: 0, .. } => {
                // The opcode changes the mode. Its next field update clears NFADE history;
                // a second opcode in this same frame can replace it before that happens.
                self.kind = 0;
                self.fraction = 0.0;
            }
            FadeOp::Fade { kind, rgb, speed, adjust } if matches!(kind, 1 | 2 | 4..=10) => {
                self.kind = kind;
                self.rgb = rgb;
                self.speed = u16::from(speed);
                self.adjust = u16::from(adjust) + u16::from(matches!(kind, 1 | 5 | 7 | 9));
                self.fraction = 0.0;
            }
            FadeOp::Nfade { kind: kind @ (11 | 12), rgb, frames } if frames > 0 => {
                self.kind = kind;
                self.target = rgb;
                self.adjust = 0;
                self.speed = frames as u16;
                self.rgb = self.start;
                self.fraction = 0.0;
            }
            _ => return false,
        }
        true
    }

    /// The field updater runs once per whole frame, independently of script/dialogue blocking.
    pub fn advance(&mut self, frames: f32) {
        if !frames.is_finite() || frames <= 0.0 { return; }
        let total = self.fraction + frames;
        let ticks = total.floor().min(u32::MAX as f32) as u32;
        self.fraction = total.fract();
        if ticks == 0 { return; }
        match self.kind {
            0 => self.start = [0; 3],
            1 | 5 | 7 | 9 => {
                let completion = (self.speed != 0).then(|| u32::from(self.adjust).div_ceil(u32::from(self.speed)));
                self.adjust = u32::from(self.adjust).saturating_sub(u32::from(self.speed).saturating_mul(ticks)) as u16;
                if self.adjust == 0 {
                    self.kind = 0;
                    // Returning fades set mode 0 on their final update. Mode 0 itself runs
                    // only on a subsequent field frame, including extra frames in this batch.
                    if completion.is_some_and(|at| ticks > at) { self.start = [0; 3]; }
                }
            }
            2 | 6 | 8 | 10 => {
                self.adjust = u32::from(self.adjust).saturating_add(u32::from(self.speed).saturating_mul(ticks)).min(255) as u16;
            }
            11 | 12 => {
                self.adjust = u32::from(self.adjust).saturating_add(ticks).min(u32::from(self.speed)) as u16;
                let channel = |start: u8, target: u8| {
                    let start = i32::from(start);
                    (start + (i32::from(target) - start) * i32::from(self.adjust) / i32::from(self.speed)) as u8
                };
                let [sr, sg, sb] = self.start;
                let [tr, tg, tb] = self.target;
                self.rgb = [channel(sr, tr), channel(sg, tg), channel(sb, tb)];
                if self.adjust == self.speed { self.start = self.target; }
            }
            _ => {}
        }
    }

    /// FADEW's completion predicate, including indefinite zero-speed FADE holds.
    pub fn complete(&self) -> bool {
        match self.kind {
            0 | 4 => true,
            1 | 5 | 7 | 9 => self.adjust == 0,
            2 | 6 | 8 | 10 => self.adjust >= 255,
            11 | 12 => self.adjust == self.speed,
            _ => false,
        }
    }

    pub fn color(&self) -> Option<FadeColor> {
        let blend = match self.kind {
            1 | 2 | 7 | 8 | 12 => FadeBlend::Subtract,
            5 | 6 | 9 | 10 | 11 => FadeBlend::Add,
            4 => return Some(FadeColor { blend: FadeBlend::Black, rgb: [0; 3] }),
            _ => return None,
        };
        let rgb = match self.kind {
            // The original fixed point multiply uses 256, including the inverse. The renderer
            // can sample just before the first native tick (adjust=256); saturate that endpoint
            // instead of wrapping full black to transparent for one frame.
            1 | 2 => self.rgb.map(|c| ((((256 - u32::from(c)) * u32::from(self.adjust)) >> 8).min(255)) as u8),
            5 | 6 => self.rgb.map(|c| ((u32::from(c) * u32::from(self.adjust)) >> 8) as u8),
            7 | 8 => self.rgb.map(|c| (256u16 - u16::from(c)) as u8),
            _ => self.rgb,
        };
        Some(FadeColor { blend, rgb })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_keep_fraction_and_nfade_history_through_future_updates() {
        let mut original = FadeState::default();
        assert!(original.apply(FadeOp::Nfade { kind: 11, rgb: [20, 80, 160], frames: 7 }));
        original.advance(7.25);
        assert!(original.apply(FadeOp::Nfade { kind: 12, rgb: [230, 100, 9], frames: 31 }));
        original.advance(3.17);
        let bytes = original.checkpoint_bytes();
        assert_eq!(bytes.len(), 18);
        let mut restored = FadeState::from_checkpoint_bytes(&bytes).unwrap();
        assert_eq!(restored.checkpoint_bytes(), bytes);
        for frames in [0.37, 5.5, 0.01, 31.25] {
            original.advance(frames);
            restored.advance(frames);
            assert_eq!(restored.checkpoint_bytes(), original.checkpoint_bytes());
            assert_eq!(restored.color(), original.color());
        }
        let replacement = FadeOp::Nfade { kind: 11, rgb: [60, 7, 99], frames: 9 };
        assert!(original.apply(replacement)); assert!(restored.apply(replacement));
        original.advance(2.3); restored.advance(2.3);
        assert_eq!(restored.checkpoint_bytes(), original.checkpoint_bytes(), "prior target survives replacement");

        assert!(original.apply(FadeOp::Fade { kind: 1, rgb: [0; 3], speed: 0, adjust: 255 }));
        assert_eq!(original.adjust, 256);
        assert_eq!(FadeState::from_checkpoint_bytes(&original.checkpoint_bytes()).unwrap(), original);
        assert!(original.apply(FadeOp::Nfade { kind: 11, rgb: [9; 3], frames: i16::MAX }));
        original.advance(500.0);
        assert!(original.apply(FadeOp::Fade { kind: 0, rgb: [0; 3], speed: 0, adjust: 0 }));
        assert_eq!(FadeState::from_checkpoint_bytes(&original.checkpoint_bytes()).unwrap(), original,
            "mode zero may retain the previous NFADE counters until a field update");
    }

    #[test]
    fn checkpoints_reject_truncation_modes_counters_and_nonfinite_fraction() {
        let quiet = FadeState::default().checkpoint_bytes();
        for length in 0..quiet.len() { assert!(FadeState::from_checkpoint_bytes(&quiet[..length]).is_err()); }
        let mut longer = quiet.clone(); longer.push(0);
        assert!(FadeState::from_checkpoint_bytes(&longer).is_err());
        for kind in [3, 13, 255] {
            let mut invalid = quiet.clone(); invalid[0] = kind;
            assert!(FadeState::from_checkpoint_bytes(&invalid).is_err());
        }
        for (kind, adjust, speed) in [(1, 257u16, 0u16), (1, 0, 256), (2, 256, 0),
            (4, 256, 0), (11, 0, 0), (12, 2, 1), (12, 0, 32768), (0, 32768, 0)] {
            let mut invalid = quiet.clone(); invalid[0] = kind;
            invalid[1..3].copy_from_slice(&adjust.to_le_bytes());
            invalid[3..5].copy_from_slice(&speed.to_le_bytes());
            assert!(FadeState::from_checkpoint_bytes(&invalid).is_err(), "kind {kind}, adjust {adjust}, speed {speed}");
        }
        for fraction in [f32::NAN, f32::INFINITY, -0.25, -0.0, 1.0] {
            let mut invalid = quiet.clone(); invalid[14..].copy_from_slice(&fraction.to_bits().to_le_bytes());
            assert!(FadeState::from_checkpoint_bytes(&invalid).is_err());
        }
    }

    #[test]
    fn fade_blue_uses_the_fourth_bank_and_nfade_duration_is_signed() {
        let fade = crate::script::decode_at(&[0x6B, 0x12, 0x34, 5, 6, 7, 8, 2, 0], 0, 0).unwrap();
        let bank = |name| fade.operands.iter().find(|p| p.name == name).unwrap().bank;
        assert_eq!([bank("r"), bank("g"), bank("b")], [1, 2, 4]);
        let nfade = crate::script::decode_at(&[0x25, 0, 0, 12, 0, 0, 0, 0xFF, 0xFF], 0, 0).unwrap();
        assert_eq!(nfade.literal("speed"), Some(-1));
    }

    #[test]
    fn fadew_observes_the_global_fade_and_another_threads_replacement() {
        use crate::script::{FieldScript, synth::SynthScript};
        use crate::script::scene::{Vm, SceneVars, Limits, Step, ScriptRef, Stage, Then};
        use crate::story::StoryState;
        // Fade-in would take 128 frames, but another entity replaces it with instant black at 7.
        let code = vec![0x6B, 0, 0, 0, 0, 0, 2, 1, 255, 0x01, 1, 1, 0x6C, 0];
        let replacement = vec![0x24, 7, 0, 0x6B, 0, 0, 0, 0, 0, 1, 4, 0, 0];
        let bytes = SynthScript::default().entity("director", vec![(1, code)]).entity("other", vec![(1, replacement)]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, Limits::default());
        vm.begin(vec![Step::Start(ScriptRef::new(0, 1))]);
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::End { at: 7 | 8, .. }), "FADEW polls the current effect: {:?}", beat.then);
        assert_eq!(beat.stage.iter().filter(|e| matches!(e.what, Stage::Fade(_))).count(), 2);
        assert!(vm.stats().handled.contains_key("FADEW"));
        assert_eq!(vm.stats().unresolved, 0);
    }

    #[test]
    fn fadew_waits_the_authored_duration_and_zero_speed_is_bounded() {
        use crate::script::{FieldScript, synth::SynthScript};
        use crate::script::scene::{Vm, SceneVars, Limits, Step, ScriptRef, Then};
        use crate::script::dialogue::End;
        use crate::story::StoryState;
        for (speed, expected) in [(2, Some(128)), (0, None)] {
            let bytes = SynthScript::default().entity("director", vec![(1, vec![0x6B, 0, 0, 0, 0, 0, speed, 1, 255, 0x6C, 0])]).build();
            let script = FieldScript::parse(&bytes).unwrap();
            let mut vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, Limits::default());
            vm.begin(vec![Step::Start(ScriptRef::new(0, 1))]);
            let beat = vm.resume(&script, None);
            match expected {
                Some(expected) => assert!(matches!(beat.then, Then::End { at, .. } if at == expected), "{:?}", beat.then),
                None => assert!(matches!(beat.then, Then::End { how: End::Budget, .. }), "{:?}", beat.then),
            }
        }
    }

    #[test]
    fn a_fade_in_consumes_adjust_plus_one_and_a_fade_out_holds_its_colour() {
        let mut f = FadeState::default();
        assert!(f.apply(FadeOp::Fade { kind: 1, rgb: [0; 3], speed: 2, adjust: 255 }));
        assert_eq!(f.color().unwrap().rgb, [255; 3], "no clear flash before the first field tick");
        f.advance(127.0);
        assert_eq!(f.color(), Some(FadeColor { blend: FadeBlend::Subtract, rgb: [2; 3] }));
        assert!(!f.complete());
        f.advance(1.0);
        assert!(f.complete() && f.color().is_none());
        f.apply(FadeOp::Fade { kind: 2, rgb: [255, 200, 200], speed: 16, adjust: 0 });
        f.advance(8.0);
        assert_eq!(f.color().unwrap().rgb, [0, 28, 28], "Scorpion's red tint subtracts green/blue");
        f.advance(8.0);
        assert_eq!(f.color().unwrap().rgb, [0, 55, 55]);
        assert!(f.complete());
        f.advance(100.0);
        assert_eq!(f.color().unwrap().rgb, [0, 55, 55]);
    }

    #[test]
    fn instantaneous_holds_zero_speed_and_unsupported_instructions_are_distinct() {
        let mut f = FadeState::default();
        f.apply(FadeOp::Fade { kind: 9, rgb: [160, 0, 0], speed: 64, adjust: 255 });
        assert_eq!(f.color().unwrap().rgb, [160, 0, 0]);
        f.advance(3.0);
        assert!(!f.complete());
        f.advance(1.0);
        assert!(f.color().is_none());
        f.apply(FadeOp::Fade { kind: 1, rgb: [0; 3], speed: 0, adjust: 127 });
        f.advance(1_000.0);
        assert!(!f.complete());
        let before = f.clone();
        assert!(!f.apply(FadeOp::Nfade { kind: 12, rgb: [9; 3], frames: 0 }));
        assert!(!f.apply(FadeOp::Fade { kind: 3, rgb: [0; 3], speed: 9, adjust: 0 }));
        assert_eq!(f, before);
        f.apply(FadeOp::Fade { kind: 4, rgb: [255; 3], speed: 0, adjust: 0 });
        assert!(f.complete());
        assert_eq!(f.color().unwrap().blend, FadeBlend::Black);
        f.apply(FadeOp::Nfade { kind: 0, rgb: [0; 3], frames: 0 });
        assert!(f.color().is_none());
    }

    #[test]
    fn every_verified_additive_and_instant_mode_has_its_authored_endpoint() {
        for kind in [1, 2, 5, 6, 7, 8, 9, 10] {
            let out = matches!(kind, 2 | 6 | 8 | 10);
            let mut f = FadeState::default();
            assert!(f.apply(FadeOp::Fade { kind, rgb: [100, 140, 180], speed: 16, adjust: if out { 0 } else { 255 } }));
            f.advance(15.0);
            assert!(!f.complete(), "type {kind}");
            f.advance(1.0);
            assert!(f.complete(), "type {kind}");
            match kind {
                2 => assert_eq!(f.color().unwrap().rgb, [155, 115, 75]),
                6 => assert_eq!(f.color().unwrap().rgb, [99, 139, 179]),
                8 => assert_eq!(f.color().unwrap().rgb, [156, 116, 76]),
                10 => assert_eq!(f.color().unwrap().rgb, [100, 140, 180]),
                _ => assert!(f.color().is_none()),
            }
        }
        let mut f = FadeState::default();
        f.apply(FadeOp::Nfade { kind: 11, rgb: [100, 140, 180], frames: 5 });
        f.advance(5.0);
        assert_eq!(f.color(), Some(FadeColor { blend: FadeBlend::Add, rgb: [100, 140, 180] }));
    }

    #[test]
    fn nfade_interpolates_direct_rgb_from_its_last_completed_target() {
        let mut f = FadeState::default();
        f.apply(FadeOp::Nfade { kind: 12, rgb: [0, 90, 90], frames: 6 });
        f.advance(3.0);
        assert_eq!(f.color().unwrap(), FadeColor { blend: FadeBlend::Subtract, rgb: [0, 45, 45] });
        f.advance(3.0);
        f.apply(FadeOp::Nfade { kind: 12, rgb: [0; 3], frames: 60 });
        let mut partitioned = f.clone();
        for _ in 0..120 { partitioned.advance(0.25); }
        f.advance(30.0);
        assert_eq!(f, partitioned);
        assert_eq!(f.color().unwrap().rgb, [0, 45, 45]);
        f.advance(f32::NAN);
        f.advance(f32::INFINITY);
        f.advance(-1.0);
        assert_eq!(f, partitioned);
        f.advance(30.0);
        assert!(f.complete());
        assert_eq!(f.color().unwrap().rgb, [0; 3]);
    }

    #[test]
    fn resetting_interpolation_history_is_a_field_update_not_an_opcode_side_effect() {
        let mut f = FadeState::default();
        f.apply(FadeOp::Nfade { kind: 11, rgb: [100; 3], frames: 1 });
        f.advance(1.0);
        f.apply(FadeOp::Nfade { kind: 0, rgb: [0; 3], frames: 0 });
        let mut next_frame = f.clone();
        f.apply(FadeOp::Nfade { kind: 11, rgb: [200; 3], frames: 10 });
        f.advance(1.0);
        assert_eq!(f.color().unwrap().rgb, [110; 3], "another opcode in the same frame retains history");
        next_frame.advance(1.0);
        next_frame.apply(FadeOp::Nfade { kind: 11, rgb: [200; 3], frames: 10 });
        next_frame.advance(1.0);
        assert_eq!(next_frame.color().unwrap().rgb, [20; 3], "a type-0 field tick clears history");
    }

    #[test]
    fn enormous_finite_deltas_finish_bounded_fades_and_leave_zero_speed_holds_in_place() {
        for kind in [0, 1, 2, 4, 5, 6, 7, 8, 9, 10] {
            let mut huge = FadeState::default();
            huge.apply(FadeOp::Fade { kind, rgb: [30, 90, 200], speed: 1, adjust: 127 });
            let mut bounded = huge.clone();
            huge.advance(1e30);
            bounded.advance(1_000.0);
            assert_eq!(huge, bounded, "type {kind}");
        }
        for kind in [11, 12] {
            let mut f = FadeState::default();
            f.apply(FadeOp::Nfade { kind, rgb: [30, 90, 200], frames: i16::MAX });
            f.advance(f32::MAX);
            assert!(f.complete());
            assert_eq!(f.color().unwrap().rgb, [30, 90, 200]);
        }
        let mut f = FadeState::default();
        f.apply(FadeOp::Fade { kind: 1, rgb: [30, 90, 200], speed: 0, adjust: 127 });
        let before = f.clone();
        f.advance(1e30);
        assert_eq!(f, before);
        FadeState::default().advance(f32::MAX);
    }
}
