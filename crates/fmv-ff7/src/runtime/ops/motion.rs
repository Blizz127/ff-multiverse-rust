//! Model movement, turning, position queries, jumps, ladders, party split/join and walkmesh locks: `MOVE`, `CMOVE`, `FMOVE`, `MOVA`, `PMOVA`, `TURA`, `PTURA`, `TURNGEN`, `TURN`, `TURNW`, `DIRA`, `PDIRA`, `GETDIR`, `PGTDR`, `GETAI`, `GETAXY`, `AXYZI`, `PXYZI`, `JUMP`, `LADER`, `OFST`, `OFSTW`, `SPLIT`, `JOIN`, `IDLCK`, `SLIP`, `FCFIX`, `SIN`, `COS`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).

use super::super::{FieldRuntime, Flow};

/// This family's state (per model where needed, indexed by model id).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MotionState {}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    let _ = op;
    false
}

impl FieldRuntime {
    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_motion(&mut self, op: u8) -> Option<Flow> {
        let _ = op;
        None
    }

    /// Per-frame update after every entity's turn (movement, animation, effects advancing).
    pub(in crate::runtime) fn update_motion(&mut self) {}
}
