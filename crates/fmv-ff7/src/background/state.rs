//! Script-controlled visibility of a field's background tiles.
//!
//! The original engine keeps 64 byte masks, cleared on field entry before entity initialization.
//! `BGON`/`BGOFF` set/clear one bit; `BGROL`/`BGROL2` shift the whole byte without wrapping; `BGCLR`
//! clears it. The script's frame operand is a bit index, while a tile's `state` is a mask.
//!
//! Primary implementation evidence (semantics only; no implementation copied):
//! * [Original-engine opcode decompilation](https://github.com/Akari1982/q-gears_reverse/blob/e0fabf7e121818e5abe2f873e34a07d0eb32742d/ffvii/DISC/FIELD/FIELD.BIN_event_code_background.cpp#L346-L434).
//! * [Zero initialization](https://github.com/Akari1982/q-gears_reverse/blob/e0fabf7e121818e5abe2f873e34a07d0eb32742d/ffvii/DISC/FIELD/FIELD.BIN_event.cpp#L229-L232).
//! * [FFNx's PC tile selection](https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/background.cpp#L102-L122):
//!   group zero is unconditional; other tiles need an intersection between the group's mask and
//!   their state. PC group identifiers are used directly, without the PSX packed-identifier mask.
//!
//! This state belongs to the field, so ending a story scene does not reset it. Unsupported group
//! or frame indices are ignored, without aliasing another group or overflowing a shift.

/// A resolved background instruction. `frame` is an index from 0 to 7, not a tile's state mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundOp {
    On { group: u8, frame: u8 },
    Off { group: u8, frame: u8 },
    Clear { group: u8 },
    ShiftLeft { group: u8 },
    ShiftRight { group: u8 },
}

/// The 64 independent visibility masks of one field. Defaults to the field-entry state, before
/// its initialization scripts run. Clone it when taking a snapshot of the shown background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundState {
    masks: [u8; 64],
}

impl Default for BackgroundState {
    fn default() -> Self {
        Self { masks: [0; 64] }
    }
}

impl BackgroundState {
    /// Apply one instruction, returning whether a mask changed. Groups outside 0..64 and frame
    /// indices outside 0..8 are unsupported and leave the state unchanged.
    pub fn apply(&mut self, op: BackgroundOp) -> bool {
        let group = match op {
            BackgroundOp::On { group, .. } | BackgroundOp::Off { group, .. }
            | BackgroundOp::Clear { group } | BackgroundOp::ShiftLeft { group }
            | BackgroundOp::ShiftRight { group } => group,
        };
        let Some(mask) = self.masks.get_mut(usize::from(group)) else { return false };
        let old = *mask;
        match op {
            BackgroundOp::On { frame, .. } if frame < 8 => *mask |= 1u8 << frame,
            BackgroundOp::Off { frame, .. } if frame < 8 => *mask &= !(1u8 << frame),
            BackgroundOp::Clear { .. } => *mask = 0,
            BackgroundOp::ShiftLeft { .. } => *mask <<= 1,
            BackgroundOp::ShiftRight { .. } => *mask >>= 1,
            _ => {},
        }
        *mask != old
    }

    /// Whether a tile is visible. Layer 1 ignores group/state, and group zero is always visible.
    /// A grouped tile is shown when any of its state bits is enabled, including multibit states.
    pub fn visible(&self, layer: u8, param: u8, state: u8) -> bool {
        layer == 1 || param == 0 || self.masks.get(usize::from(param)).is_some_and(|mask| *mask & state != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_entry_shows_only_unconditional_tiles() {
        let state = BackgroundState::default();
        assert!(state.visible(1, 255, 255), "layer 1 ignores its group bytes");
        for layer in 2..=4 {
            assert!(state.visible(layer, 0, 0));
            assert!(state.visible(layer, 0, 255));
            for group in 1..64 {
                assert!(!state.visible(layer, group, 255));
            }
        }
    }

    #[test]
    fn on_and_off_preserve_other_bits_and_groups_and_tiles_test_intersection() {
        let mut state = BackgroundState::default();
        assert!(state.apply(BackgroundOp::On { group: 8, frame: 2 }));
        assert!(!state.apply(BackgroundOp::On { group: 8, frame: 2 }), "already on");
        assert!(state.visible(2, 8, 4), "the frame operand is an index");
        assert!(state.visible(2, 8, 6), "a tile may accept more than one frame");
        assert!(!state.visible(2, 8, 2));
        assert!(!state.visible(2, 8, 0), "state zero is hidden for a nonzero group");
        state.apply(BackgroundOp::On { group: 8, frame: 1 });
        state.apply(BackgroundOp::On { group: 9, frame: 2 });
        assert!(state.apply(BackgroundOp::Off { group: 8, frame: 2 }));
        assert!(!state.visible(2, 8, 4));
        assert!(state.visible(2, 8, 2) && state.visible(2, 8, 6));
        assert!(state.visible(2, 9, 4), "groups are independent");
        assert!(state.apply(BackgroundOp::Clear { group: 8 }));
        assert!(!state.visible(2, 8, 255));
        assert!(!state.apply(BackgroundOp::Clear { group: 8 }));
        assert!(state.visible(2, 9, 4));
    }

    #[test]
    fn shifts_move_all_enabled_bits_and_discard_the_bit_that_leaves_the_byte() {
        let mut state = BackgroundState::default();
        for frame in [0, 3, 7] {
            state.apply(BackgroundOp::On { group: 1, frame });
        }
        let mut right = state.clone();
        assert!(state.apply(BackgroundOp::ShiftLeft { group: 1 }));
        assert!(state.visible(2, 1, 2) && state.visible(2, 1, 16));
        assert!(!state.visible(2, 1, !(2 | 16)), "the high bit does not wrap to bit zero");
        assert!(right.apply(BackgroundOp::ShiftRight { group: 1 }));
        assert!(right.visible(2, 1, 4) && right.visible(2, 1, 64));
        assert!(!right.visible(2, 1, !(4 | 64)), "the low bit does not wrap to bit seven");
        for _ in 0..8 {
            state.apply(BackgroundOp::ShiftLeft { group: 1 });
            right.apply(BackgroundOp::ShiftRight { group: 1 });
        }
        assert_eq!(state, BackgroundState::default());
        assert_eq!(right, BackgroundState::default());
    }

    #[test]
    fn unsupported_indices_do_not_alias_or_change_any_group() {
        let mut state = BackgroundState::default();
        state.apply(BackgroundOp::On { group: 63, frame: 7 });
        assert!(state.visible(4, 63, 128), "the final supported group and bit work");
        let before = state.clone();
        for group in [64, 255] {
            for op in [BackgroundOp::On { group, frame: 7 }, BackgroundOp::Off { group, frame: 7 },
                BackgroundOp::Clear { group }, BackgroundOp::ShiftLeft { group }, BackgroundOp::ShiftRight { group }] {
                assert!(!state.apply(op));
            }
            assert!(!state.visible(2, group, 255));
        }
        for frame in [8, 255] {
            assert!(!state.apply(BackgroundOp::On { group: 63, frame }));
            assert!(!state.apply(BackgroundOp::Off { group: 63, frame }));
        }
        assert_eq!(state, before);
    }
}
