//! Background layers, palettes, camera, fades, shake and background movies: `BGON`, `BGOFF`, `BGROL`, `BGROL2`, `BGCLR`, `BGSCR`, `BGPDH`, `STPAL`, `LDPAL`, `CPPAL`, `RTPAL`, `ADPAL`, `MPPAL`, `STPLS`, `LDPLS`, `CPPAL2`, `RTPAL2`, `ADPAL2`, `MPPAL2`, `SCRLO`, `SCRLC`, `SCRLA`, `SCR2D`, `SCRCC`, `SCR2DC`, `SCRLW`, `SCR2DL`, `SCRLP`, `VWOFT`, `FADE`, `FADEW`, `NFADE`, `SHAKE`, `MVIEF`, `MVCAM`, `BGMOVIE`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).

use super::super::{FieldRuntime, Flow};

/// This family's state (per model where needed, indexed by model id).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageState {}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    let _ = op;
    false
}

impl FieldRuntime {
    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_stage(&mut self, op: u8) -> Option<Flow> {
        let _ = op;
        None
    }

    /// Per-frame update after every entity's turn (movement, animation, effects advancing).
    pub(in crate::runtime) fn update_stage(&mut self) {}
}
