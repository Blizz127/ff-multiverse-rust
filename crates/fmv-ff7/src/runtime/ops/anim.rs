//! Model animation: `DFANM`, `ANIME1`, `ANIME2`, `ANIMW`, `ANIM!1`, `ANIM!2`, `CANIM1`, `CANIM2`, `CANM!1`, `CANM!2`, `CCANM`, `ANIMB`, `ASPED`, `BLINK`, `KAWAI`, `KAWIW`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).

use super::super::{FieldRuntime, Flow};

/// This family's state (per model where needed, indexed by model id).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimState {}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    let _ = op;
    false
}

impl FieldRuntime {
    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_anim(&mut self, op: u8) -> Option<Flow> {
        let _ = op;
        None
    }

    /// Per-frame update after every entity's turn (movement, animation, effects advancing).
    pub(in crate::runtime) fn update_anim(&mut self) {}
}
