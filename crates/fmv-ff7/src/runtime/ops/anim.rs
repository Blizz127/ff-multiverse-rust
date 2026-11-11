//! Model animation: `DFANM`, `ANIME1`, `ANIME2`, `ANIMW`, `ANIM!1`, `ANIM!2`, `CANIM1`, `CANIM2`, `CANM!1`, `CANM!2`, `CCANM`, `ANIMB`, `ASPED`, `BLINK`, `KAWAI`, `KAWIW`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).
//!
//! # The model
//!
//! A model has a *default* animation (`DFANM`, looping) and a *scripted* one with a status ([`AnimStatus`]):
//!
//! * `DefaultLoop`: the model plays its default animation and follows `DFANM` changes.
//! * `ScriptedLoop`: a loop another family asked for (a scripted walk); it is not re-tracked to the default.
//! * `PlayOnce` / `PlayOnceDone`: a play-once started by a *waiting* instruction, and its finished state, which the
//!   instruction consumes on its next visit.
//! * `PlayOnceThenReset` / `PlayOnceThenHold`: play-once started by a *non-waiting* instruction; the model returns to
//!   the default animation, or holds the last frame, when it ends.
//! * `Hold`: frozen on the last frame.
//!
//! The model's animation id, speed and status live on [`Model`](crate::runtime::Model) (`anim`, `anim_speed`,
//! `anim_status`), so the motion family (scripted walks) shares them; the frame counter and the rest live here.
//! Frames are counted in sixteenths: the counter advances by the model's speed each frame (16 = one animation frame per
//! field frame) and the animation frame shown is the counter divided by 16.
//!
//! One frame, after every entity's script turn, per model: the status machine runs first (it ends play-once
//! animations, loops and resets), then the counter advances and is clamped to the animation's last frame. That is the
//! original's order (script turns, then the animation-state pass, then the movement/animation pass), so a waiting play
//! of an `n`-frame animation at normal speed releases its script on the visit `n` frames after it started.
//!
//! # Which opcode waits
//!
//! The decompiled engine, not the community names, decides: `ANIME1` (0xA3), `ANIM!2` (0xBA), `CANIM2` (0xBB) and
//! `CANM!2` (0xBC) **wait** for the animation to finish; `ANIME2` (0xAE), `ANIM!1` (0xAF), `CANIM1` (0xB0) and
//! `CANM!1` (0xB1) start it and continue. `ANIME*` return to the default animation afterwards; `ANIM!*` and `CANM!*`
//! hold the last frame; `CANIM*` return to the default. A play instruction only starts when the model is looping or
//! holding; while a previous play-once is still running it simply yields until that one ends.
//!
//! # Frame counts
//!
//! An animation's length comes from the animation file, which the runtime does not read. The host supplies frame
//! counts per model-loader index (what `CHAR` names) with [`AnimState::set_animation_frames`] (or
//! [`FieldRuntime::set_animation_frame_counts`], with [`AnimState::frame_counts_from`] reading them from the
//! character archive). It works before [`FieldRuntime::init`](crate::runtime::FieldRuntime::init), because `CHAR` and animation
//! instructions already run during initialisation. A count the host did not give (or gave as 0) is **unknown**: a
//! whole-animation play and a default loop then last [`AnimState::DEFAULT_ANIMATION_FRAMES`] frames so a script never hangs, and
//! a partial play (`CANIM*`/`CANM!*`) uses the last frame the script asked for.
//!
//! # Initialisation
//!
//! Entity init parts run without frames passing, so a waiting instruction there cannot ever be released by the frame
//! update. During initialisation (frame 0) every animation instruction therefore completes the play-once it meets
//! at once (the model ends on the final state: default loop, or held on the last frame) and never yields.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use super::super::{AnimStatus, FieldRuntime, Flow, MoveMode};
use crate::field::ModelLoader;
use crate::lgp::LgpArchive;

const DFANM: u8 = 0xA2;
const ANIME1: u8 = 0xA3;
const ANIMW: u8 = 0xAC;
const ANIME2: u8 = 0xAE;
const ANIM_HOLD_NOWAIT: u8 = 0xAF; // ANIM!1
const CANIM_NOWAIT: u8 = 0xB0; // CANIM1
const CANM_HOLD_NOWAIT: u8 = 0xB1; // CANM!1
const ANIM_HOLD_WAIT: u8 = 0xBA; // ANIM!2
const CANIM_WAIT: u8 = 0xBB; // CANIM2
const CANM_HOLD_WAIT: u8 = 0xBC; // CANM!2
const ASPED: u8 = 0xBD;
const CCANM: u8 = 0xDC;
const ANIMB: u8 = 0xDD;
const BLINK: u8 = 0x26;
const KAWAI: u8 = 0x28;
const KAWIW: u8 = 0x29;

/// A `KAWAI` effect a script issued, for the host to apply to the model (eyes, mouth, transparency, shading...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KawaiEffect {
    /// The runtime model index.
    pub model: usize,
    /// The sub-opcode (`0` eye/mouth textures, `1` transparency, `2` ambient light, `0xFF` reset...).
    pub sub: u8,
    /// The sub-opcode's argument bytes, as in the script.
    pub payload: Vec<u8>,
}

/// One model's animation state beyond what [`Model`](crate::runtime::Model) carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAnim {
    /// The animation `DFANM` made the default.
    pub default_id: u8,
    /// The default animation's speed (the base speed divided by `DFANM`'s speed operand).
    pub default_speed: u16,
    /// The speed `ASPED` set; later speed operands divide it.
    pub base_speed: u16,
    /// The animation the frame counter belongs to (a change of `Model::anim` restarts the counter).
    active: u8,
    /// The frame counter in sixteenths of an animation frame.
    pub counter: i32,
    /// The last animation frame the current play reaches.
    pub last_frame: i32,
    /// `BLINK`'s operand: 1 disables the automatic blinking, anything else leaves it on. A `KAWAI` eye-texture
    /// effect sets it the same way.
    pub blink: u8,
    /// A `KAWAI` effect is still running (`KAWIW` waits for it).
    kawai_running: bool,
    /// The type byte of the last `KAWAI`.
    pub kawai_type: Option<u8>,
}

impl Default for ModelAnim {
    fn default() -> Self {
        ModelAnim {
            default_id: 0,
            default_speed: 16,
            base_speed: 16,
            active: 0,
            counter: 0,
            last_frame: 0,
            blink: 0,
            kawai_running: false,
            kawai_type: None,
        }
    }
}

/// What the host needs to draw a model's current pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationView {
    /// The animation id (an index into the model loader's animation list).
    pub animation: u8,
    /// The animation frame (fractional: sixteenths).
    pub frame: f32,
    pub status: AnimStatus,
    /// The playback speed (16 = one frame per field frame).
    pub speed: u16,
    /// Whether the engine blinks the model's eyes by itself.
    pub auto_blink: bool,
}

/// This family's state (per model, indexed by the runtime's model index).
#[derive(Debug, Clone, PartialEq)]
pub struct AnimState {
    models: Vec<ModelAnim>,
    /// Frame counts per model-loader index, then per animation (0 = unknown).
    frames: BTreeMap<u8, Vec<u16>>,
    /// `CCANM`: the animations the player's model uses while controlled (idle, walk, run).
    pub player_anims: [u8; 3],
    /// `KAWAI` effects not yet taken by the host.
    effects: Vec<KawaiEffect>,
    /// Never read; stands in for a slot that cannot exist.
    spare: ModelAnim,
}

impl Default for AnimState {
    fn default() -> Self {
        AnimState { models: Vec::new(), frames: BTreeMap::new(), player_anims: [0, 1, 2], effects: Vec::new(), spare: ModelAnim::default() }
    }
}

impl AnimState {
    /// Length, in animation frames, of an animation whose frame count the host did not supply.
    pub const DEFAULT_ANIMATION_FRAMES: u16 = 16;

    /// Every animation's frame count of every record of a field's model loader, read from the character archive
    /// (`char.lgp`), for [`FieldRuntime::set_animation_frame_counts`]. An animation that cannot be read counts 0
    /// (unknown). Only the frame count is used, never the poses.
    pub fn frame_counts_from<R: Read + Seek>(char_lgp: &mut LgpArchive<R>, loader: &ModelLoader) -> Vec<Vec<u16>> {
        loader
            .models
            .iter()
            .map(|model| {
                model
                    .animations
                    .iter()
                    .map(|a| {
                        let count = crate::model::load_field_animation(char_lgp, &a.name).map(|f| f.frame_count());
                        count.ok().and_then(|n| u16::try_from(n).ok()).unwrap_or(0)
                    })
                    .collect()
            })
            .collect()
    }

    /// Give the frame count of every animation of the model-loader record `char_id` (what `CHAR char_id` loads), in
    /// the record's animation order. A count of 0 means unknown.
    pub fn set_animation_frames(&mut self, char_id: u8, frames: Vec<u16>) {
        self.frames.insert(char_id, frames);
    }

    /// The known frame count of animation `anim` of loader record `char_id`.
    pub fn animation_frames(&self, char_id: u8, anim: u8) -> Option<u16> {
        self.frames.get(&char_id).and_then(|f| f.get(usize::from(anim))).copied().filter(|n| *n > 0)
    }

    /// The state of runtime model `model`, if its animation was ever touched.
    pub fn model(&self, model: usize) -> Option<&ModelAnim> {
        self.models.get(model)
    }

    fn slot(&mut self, model: usize) -> &mut ModelAnim {
        if self.models.len() <= model {
            self.models.resize(model + 1, ModelAnim::default());
        }
        match self.models.get_mut(model) {
            Some(slot) => slot,
            None => &mut self.spare,
        }
    }

    /// Frames of animation `anim` for loader record `char_id`, or the default length.
    fn whole_frames(&self, char_id: u8, anim: u8) -> i32 {
        i32::from(self.animation_frames(char_id, anim).unwrap_or(AnimState::DEFAULT_ANIMATION_FRAMES))
    }
}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    matches!(
        op,
        DFANM | ANIME1 | ANIMW | ANIME2 | ANIM_HOLD_NOWAIT | CANIM_NOWAIT | CANM_HOLD_NOWAIT | ANIM_HOLD_WAIT | CANIM_WAIT | CANM_HOLD_WAIT | ASPED | CCANM | ANIMB | BLINK | KAWAI | KAWIW
    )
}

/// Which instruction family shapes a play-once.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Play {
    /// `ANIME*`, `ANIM!*`: animation and speed operands.
    Whole,
    /// `CANIM*`: first and last frames, divided by the speed.
    PartialDivided,
    /// `CANM!*`: first frame as is, last frame divided by the speed.
    Partial,
}

impl FieldRuntime {
    /// Supply the frame counts of every model-loader record (`counts[i]` for `CHAR i`), as
    /// [`AnimState::frame_counts_from`] reads them. May be called before or after [`FieldRuntime::init`].
    pub fn set_animation_frame_counts(&mut self, counts: &[Vec<u16>]) {
        for (i, frames) in counts.iter().enumerate() {
            if let Ok(id) = u8::try_from(i) {
                self.anim.set_animation_frames(id, frames.clone());
            }
        }
    }

    /// The pose to draw for runtime model `model`.
    pub fn model_animation(&self, model: usize) -> Option<AnimationView> {
        let m = self.models.get(model)?;
        let default = ModelAnim::default();
        let st = self.anim.models.get(model).unwrap_or(&default);
        Some(AnimationView {
            animation: m.anim,
            frame: st.counter as f32 / 16.0,
            status: m.anim_status,
            speed: m.anim_speed,
            auto_blink: st.blink != 1,
        })
    }

    /// `CCANM`'s animations for the player's model: idle, walk, run.
    pub fn player_animations(&self) -> [u8; 3] {
        self.anim.player_anims
    }

    /// The `KAWAI` effects scripts issued since the last call, in order.
    pub fn drain_kawai_effects(&mut self) -> Vec<KawaiEffect> {
        std::mem::take(&mut self.anim.effects)
    }

    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_anim(&mut self, op: u8) -> Option<Flow> {
        Some(match op {
            DFANM => self.op_dfanm(),
            ANIME1 | ANIM_HOLD_WAIT => self.op_play(Play::Whole, false, op == ANIM_HOLD_WAIT),
            ANIME2 | ANIM_HOLD_NOWAIT => self.op_play(Play::Whole, true, op == ANIM_HOLD_NOWAIT),
            CANIM_WAIT => self.op_play(Play::PartialDivided, false, false),
            CANIM_NOWAIT => self.op_play(Play::PartialDivided, true, false),
            CANM_HOLD_WAIT => self.op_play(Play::Partial, false, true),
            CANM_HOLD_NOWAIT => self.op_play(Play::Partial, true, true),
            ANIMW => self.op_animw(),
            ANIMB => self.op_animb(),
            ASPED => self.op_asped(),
            CCANM => self.op_ccanm(),
            BLINK => self.op_blink(),
            KAWAI => self.op_kawai(),
            KAWIW => self.op_kawiw(),
            _ => return None,
        })
    }

    /// The current entity's model, if it has one.
    fn anim_model(&self) -> Option<usize> {
        self.model_of(self.current).filter(|m| *m < self.models.len())
    }

    /// Whether the entity init parts are running (no frame has passed yet).
    fn initialising(&self) -> bool {
        self.frame == 0
    }

    /// End the model's running play-once at once (initialisation only).
    fn finish_play_now(&mut self, mi: usize) {
        let last = self.anim.slot(mi).last_frame;
        let status = self.status(mi);
        let end = match status {
            AnimStatus::PlayOnce => AnimStatus::PlayOnceDone,
            AnimStatus::PlayOnceThenReset => AnimStatus::DefaultLoop,
            AnimStatus::PlayOnceThenHold => AnimStatus::Hold,
            _ => return,
        };
        self.set_status(mi, end);
        if end != AnimStatus::DefaultLoop {
            self.anim.slot(mi).counter = last.saturating_mul(16);
        }
    }

    fn status(&self, mi: usize) -> AnimStatus {
        self.models.get(mi).map(|m| m.anim_status).unwrap_or_default()
    }

    fn set_status(&mut self, mi: usize, status: AnimStatus) {
        if let Some(m) = self.models.get_mut(mi) {
            m.anim_status = status;
        }
    }

    /// The model's frame count for the animation it is playing, when known.
    fn known_frames(&self, mi: usize, anim: u8) -> Option<i32> {
        let char_id = self.models.get(mi)?.char_id;
        self.anim.animation_frames(char_id, anim).map(i32::from)
    }

    fn divisor(&mut self, raw: u8) -> u16 {
        if raw == 0 {
            self.coverage.anomalies += 1;
        }
        u16::from(raw).max(1)
    }

    fn op_dfanm(&mut self) -> Flow {
        let (id, speed) = (self.p8(1), self.p8(2));
        if let Some(mi) = self.anim_model() {
            let div = self.divisor(speed);
            let st = self.anim.slot(mi);
            st.default_id = id;
            st.default_speed = st.base_speed / div;
            if self.status(mi) == AnimStatus::Hold {
                self.set_status(mi, AnimStatus::DefaultLoop);
            }
        }
        self.advance(3);
        Flow::Yield
    }

    /// Begin the animation the current play instruction asks for on model `mi`.
    fn start_play(&mut self, mi: usize, kind: Play) {
        let id = self.p8(1);
        let known = self.known_frames(mi, id);
        let (counter, last, speed) = match kind {
            Play::Whole => {
                let div = self.divisor(self.p8(2));
                let frames = known.unwrap_or(i32::from(AnimState::DEFAULT_ANIMATION_FRAMES));
                (0, (frames - 1).max(0), self.anim.slot(mi).base_speed / div)
            }
            Play::PartialDivided | Play::Partial => {
                let div = self.divisor(self.p8(4));
                let (first, last) = (i32::from(self.p8(2)), i32::from(self.p8(3)));
                let d = i32::from(div);
                let first = if kind == Play::PartialDivided { first / d } else { first };
                let requested = last / d;
                let last = match known {
                    Some(frames) => (frames - 1).max(0).min(requested),
                    None => requested,
                };
                (16 * first, last, self.anim.slot(mi).base_speed / div)
            }
        };
        if let Some(m) = self.models.get_mut(mi) {
            m.anim = id;
            m.anim_speed = speed;
        }
        let st = self.anim.slot(mi);
        st.active = id;
        st.counter = counter;
        st.last_frame = last;
    }

    fn op_play(&mut self, kind: Play, no_wait: bool, hold: bool) -> Flow {
        let len = if kind == Play::Whole { 3 } else { 5 };
        let Some(mi) = self.anim_model() else {
            self.advance(len);
            return Flow::Continue;
        };
        let init = self.initialising();
        if init {
            self.finish_play_now(mi);
        }
        match self.status(mi) {
            AnimStatus::DefaultLoop | AnimStatus::ScriptedLoop | AnimStatus::Hold => {
                self.start_play(mi, kind);
                if no_wait {
                    self.set_status(mi, if hold { AnimStatus::PlayOnceThenHold } else { AnimStatus::PlayOnceThenReset });
                    if init {
                        self.finish_play_now(mi);
                    }
                    self.advance(len);
                    return Flow::Continue;
                }
                self.set_status(mi, AnimStatus::PlayOnce);
                if init {
                    self.finish_play_now(mi);
                    return self.consume_done(mi, hold, len);
                }
                Flow::Yield
            }
            AnimStatus::PlayOnceDone => self.consume_done(mi, hold, len),
            _ => Flow::Yield,
        }
    }

    /// A waiting play finished: release the script, and leave the model looping or holding.
    fn consume_done(&mut self, mi: usize, hold: bool, len: usize) -> Flow {
        self.set_status(mi, if hold { AnimStatus::Hold } else { AnimStatus::DefaultLoop });
        self.advance(len);
        Flow::Continue
    }

    fn op_animw(&mut self) -> Flow {
        let Some(mi) = self.anim_model() else {
            self.advance(1);
            return Flow::Continue;
        };
        if self.initialising() {
            self.finish_play_now(mi);
        }
        match self.status(mi) {
            AnimStatus::PlayOnce | AnimStatus::PlayOnceThenReset | AnimStatus::PlayOnceThenHold => return Flow::Yield,
            AnimStatus::PlayOnceDone => self.set_status(mi, AnimStatus::DefaultLoop),
            _ => {}
        }
        self.advance(1);
        Flow::Continue
    }

    fn op_animb(&mut self) -> Flow {
        if let Some(mi) = self.anim_model() {
            let st = self.anim.slot(mi);
            st.last_frame = st.counter >> 4;
            self.set_status(mi, AnimStatus::Hold);
        }
        self.advance(1);
        Flow::Continue
    }

    fn op_asped(&mut self) -> Flow {
        let speed = self.rd16(2, 2) as u16;
        if let Some(mi) = self.anim_model() {
            if let Some(m) = self.models.get_mut(mi) {
                m.anim_speed = speed;
            }
            self.anim.slot(mi).base_speed = speed;
        }
        self.advance(4);
        Flow::Continue
    }

    fn op_ccanm(&mut self) -> Flow {
        let (id, which) = (self.p8(1), usize::from(self.p8(3)));
        if let Some(slot) = self.anim.player_anims.get_mut(which) {
            *slot = id;
        }
        self.advance(4);
        Flow::Continue
    }

    fn op_blink(&mut self) -> Flow {
        let v = self.p8(1);
        if let Some(mi) = self.anim_model() {
            self.anim.slot(mi).blink = v;
        }
        self.advance(2);
        Flow::Continue
    }

    fn op_kawai(&mut self) -> Flow {
        let declared = usize::from(self.p8(1));
        // The length byte covers the opcode, itself and the sub-opcode at least.
        let len = if declared < 3 {
            self.coverage.anomalies += 1;
            declared.max(3)
        } else {
            declared
        };
        if let Some(mi) = self.anim_model() {
            let sub = self.p8(2);
            let start = self.pc() + 3;
            let payload = self.code.get(start..(self.pc() + len).max(start)).map(<[u8]>::to_vec).unwrap_or_default();
            if sub == 0 {
                // Eye textures: the default eyes hand the blinking back to the engine, others take it over.
                let default_eyes = payload.first() == Some(&1) && payload.get(1) == Some(&1) && payload.get(2) == Some(&0);
                self.anim.slot(mi).blink = if default_eyes { 0 } else { 1 };
            }
            let st = self.anim.slot(mi);
            st.kawai_type = Some(sub);
            st.kawai_running = true;
            self.anim.effects.push(KawaiEffect { model: mi, sub, payload });
        }
        self.advance(len);
        Flow::Continue
    }

    fn op_kawiw(&mut self) -> Flow {
        if let Some(mi) = self.anim_model() {
            if self.initialising() {
                self.anim.slot(mi).kawai_running = false;
            }
            if self.anim.slot(mi).kawai_running {
                return Flow::Yield;
            }
        }
        self.advance(1);
        Flow::Continue
    }

    /// Per-frame update after every entity's turn (movement, animation, effects advancing).
    pub(in crate::runtime) fn update_anim(&mut self) {
        let lock = self.state.character_lock;
        for mi in 0..self.models.len() {
            let Some(m) = self.models.get(mi) else { continue };
            let (char_id, mut anim, mut speed, mut status, mode) = (m.char_id, m.anim, m.anim_speed, m.anim_status, m.move_mode);
            // The host drives the player's model while the player has control.
            let host_driven = self.state.pc_model == Some(mi) && !lock;
            let mut st = self.anim.slot(mi).clone();
            let whole = |a: &AnimState, id: u8| (a.whole_frames(char_id, id) - 1).max(0);
            // Another family switched the animation: start counting from its first frame.
            if st.active != anim {
                st.active = anim;
                st.counter = 0;
                st.last_frame = whole(&self.anim, anim);
            }
            if !host_driven {
                let finished = (st.counter >> 4) >= st.last_frame;
                match status {
                    // A model moving under script control (a walk, jump or ladder) plays the clip its motion chose.
                    AnimStatus::DefaultLoop if mode != MoveMode::None => {
                        st.last_frame = whole(&self.anim, anim);
                        if (st.counter >> 4) >= st.last_frame {
                            st.counter = 0;
                        }
                    }
                    AnimStatus::DefaultLoop => {
                        if anim != st.default_id {
                            anim = st.default_id;
                            speed = st.default_speed;
                            st.active = anim;
                            st.counter = 0;
                            st.last_frame = whole(&self.anim, anim);
                        } else {
                            st.last_frame = whole(&self.anim, anim);
                            if (st.counter >> 4) >= st.last_frame {
                                st.counter = 0;
                            }
                        }
                    }
                    AnimStatus::ScriptedLoop => {
                        if finished {
                            st.counter = 0;
                        }
                    }
                    AnimStatus::PlayOnce => {
                        if finished {
                            st.counter = st.last_frame.saturating_mul(16);
                            status = AnimStatus::PlayOnceDone;
                        }
                    }
                    AnimStatus::Hold | AnimStatus::PlayOnceDone => st.counter = st.last_frame.saturating_mul(16),
                    AnimStatus::PlayOnceThenReset => {
                        if finished {
                            status = AnimStatus::DefaultLoop;
                        }
                    }
                    AnimStatus::PlayOnceThenHold => {
                        if finished {
                            status = AnimStatus::Hold;
                        }
                    }
                }
            }
            // The counter advances after the state pass, except for the host's player and a model on a ladder.
            if !host_driven && mode != MoveMode::Ladder {
                st.counter = st.counter.saturating_add(i32::from(speed)).min(st.last_frame.saturating_mul(16));
            }
            *self.anim.slot(mi) = st;
            if let Some(m) = self.models.get_mut(mi) {
                (m.anim, m.anim_speed, m.anim_status) = (anim, speed, status);
            }
        }
        // KAWAI effects are applied by the host as soon as it sees them; they are done by the next frame.
        for st in &mut self.anim.models {
            st.kawai_running = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Banks;
    use crate::script::synth::SynthScript;

    /// `[init..., RET, main...]` as script 0.
    fn script0(init: &[u8], main: &[u8]) -> (usize, Vec<u8>) {
        let mut v = init.to_vec();
        v.push(0x00);
        v.extend_from_slice(main);
        (0, v)
    }

    const CHAR: [u8; 2] = [0xA1, 0];
    /// `WAIT1; JMPB 1`.
    const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

    fn setb(addr: u8, v: u8) -> Vec<u8> {
        vec![0x80, 0x10, addr, v]
    }

    fn runtime_with(init: &[u8], main: &[u8], frames: &[u16]) -> FieldRuntime {
        let mut s = SynthScript::default().entity("m", vec![script0(init, main)]);
        s.scale = 512;
        let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
        rt.anim.set_animation_frames(0, frames.to_vec());
        rt.init();
        rt
    }

    fn runtime(main: &[u8], frames: &[u16]) -> FieldRuntime {
        runtime_with(&CHAR, main, frames)
    }

    fn var(rt: &FieldRuntime, addr: u8) -> u8 {
        rt.mem.banks.get(usize::from(addr))
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn a_waiting_play_releases_the_script_after_the_animation_ends() {
        // ANIME1 anim 2 speed 1, then set var 0x30.
        let main = cat(&[&[0xA3, 2, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4, 5]);
        let mut at = None;
        for f in 1..=20 {
            rt.tick(None);
            if var(&rt, 0x30) == 1 {
                at = Some(f);
                break;
            }
        }
        // Started on tick 1; the counter reaches frame 4 after tick 4, the state pass notices on tick 5 and the
        // script is released on tick 6.
        assert_eq!(at, Some(6));
        assert_eq!(rt.models()[0].anim_status, AnimStatus::DefaultLoop);
        assert!(rt.coverage().unsupported.is_empty());
    }

    #[test]
    fn a_no_wait_play_continues_and_animw_waits_for_it() {
        // ANIME2 anim 1; set var 0x30; ANIMW; set var 0x31.
        let main = cat(&[&[0xAE, 1, 1], &setb(0x30, 1), &[0xAC], &setb(0x31, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4, 5]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1, "ANIME2 continues");
        assert_eq!(var(&rt, 0x31), 0, "ANIMW waits");
        assert_eq!(rt.models()[0].anim_status, AnimStatus::PlayOnceThenReset);
        let mut at = None;
        for f in 2..=20 {
            rt.tick(None);
            if var(&rt, 0x31) == 1 {
                at = Some(f);
                break;
            }
        }
        assert_eq!(at, Some(5), "a four-frame animation releases ANIMW four frames after it started");
        assert_eq!(rt.models()[0].anim_status, AnimStatus::DefaultLoop);
    }

    #[test]
    fn a_second_play_blocks_until_the_first_ends() {
        // ANIME2 anim 1 (4 frames), ANIME2 anim 2 immediately, then set var 0x30.
        let main = cat(&[&[0xAE, 1, 1], &[0xAE, 2, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4, 9]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 0);
        assert_eq!(rt.models()[0].anim, 1, "the second play has not started");
        for _ in 0..10 {
            rt.tick(None);
        }
        assert_eq!(var(&rt, 0x30), 1);
        assert_eq!(rt.models()[0].anim, 2);
    }

    #[test]
    fn hold_variants_end_on_the_last_frame() {
        // ANIM!2 (waits, holds), ANIM!1 (continues, holds).
        let main = cat(&[&[0xBA, 1, 1], &setb(0x30, 1), &[0xAF, 2, 1], &[0xAC], &setb(0x31, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4, 6]);
        for _ in 0..30 {
            rt.tick(None);
        }
        assert_eq!(var(&rt, 0x30), 1);
        assert_eq!(var(&rt, 0x31), 1);
        assert_eq!(rt.models()[0].anim_status, AnimStatus::Hold);
        let view = rt.model_animation(0).unwrap();
        assert_eq!((view.animation, view.frame), (2, 5.0));
    }

    #[test]
    fn dfanm_yields_after_advancing_and_releases_hold() {
        // ANIM!1 anim 1 then ANIMW (ends in Hold), DFANM 2 speed 2, set var.
        let main = cat(&[&[0xAF, 1, 1], &[0xAC], &[0xA2, 2, 2], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4, 6]);
        for _ in 0..8 {
            rt.tick(None);
        }
        let st = rt.anim.model(0).unwrap();
        assert_eq!((st.default_id, st.default_speed), (2, 8));
        assert_eq!(var(&rt, 0x30), 1);
        let m = &rt.models()[0];
        assert_eq!(m.anim_status, AnimStatus::DefaultLoop, "DFANM released the hold");
        assert_eq!((m.anim, m.anim_speed), (2, 8), "and the default animation took over");
        // DFANM yields: the instruction after it did not run in the same frame.
        let main = cat(&[&[0xA2, 1, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4, 4]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 0);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1);
    }

    #[test]
    fn canim_divides_the_first_frame_and_canm_does_not() {
        // CANIM2 (waits) anim 1, first 8, last 12, speed 2 -> counter starts at frame 4, last 6.
        let main = cat(&[&[0xBB, 1, 8, 12, 2], IDLE]);
        let mut rt = runtime(&main, &[20, 20]);
        rt.tick(None);
        let st = rt.anim.model(0).unwrap();
        assert_eq!(st.last_frame, 6);
        assert_eq!(rt.models()[0].anim_speed, 8, "16 / 2");
        let main = cat(&[&[0xBC, 1, 8, 12, 2], IDLE]);
        let mut rt = runtime(&main, &[20, 20]);
        rt.tick(None);
        let st = rt.anim.model(0).unwrap();
        assert_eq!(st.last_frame, 6);
        // CANM! keeps 8 as the first frame, past the last frame 6: it clamps and ends at once.
        assert_eq!(rt.models()[0].anim_status, AnimStatus::PlayOnceDone);
        // The requested last frame is capped by the animation's length.
        let main = cat(&[&[0xBB, 1, 0, 200, 1], IDLE]);
        let mut rt = runtime(&main, &[20, 10]);
        rt.tick(None);
        assert_eq!(rt.anim.model(0).unwrap().last_frame, 9);
    }

    #[test]
    fn unknown_frame_counts_never_hang() {
        let main = cat(&[&[0xA3, 5, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[]);
        for _ in 0..40 {
            rt.tick(None);
        }
        assert_eq!(var(&rt, 0x30), 1);
        // A partial play of an unknown animation uses the requested last frame.
        let main = cat(&[&[0xBB, 5, 0, 3, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[]);
        rt.tick(None);
        assert_eq!(rt.anim.model(0).unwrap().last_frame, 3);
    }

    #[test]
    fn animb_freezes_the_frame() {
        let main = cat(&[&[0xAE, 1, 1], &[0xDD], IDLE]);
        let mut rt = runtime(&main, &[10, 10]);
        for _ in 0..6 {
            rt.tick(None);
        }
        assert_eq!(rt.models()[0].anim_status, AnimStatus::Hold);
        let view = rt.model_animation(0).unwrap();
        assert!(view.frame <= 1.0, "frozen near where it was stopped, got {}", view.frame);
        let later = view.frame;
        for _ in 0..6 {
            rt.tick(None);
        }
        assert_eq!(rt.model_animation(0).unwrap().frame, later);
    }

    #[test]
    fn asped_sets_the_speed_later_plays_divide() {
        // ASPED 32 (banks byte 0, then the 16-bit literal), then ANIME2 anim 1 speed 2.
        let main = cat(&[&[0xBD, 0, 32, 0], &[0xAE, 1, 2], IDLE]);
        let mut rt = runtime(&main, &[10, 10]);
        rt.tick(None);
        assert_eq!(rt.anim.model(0).unwrap().base_speed, 32);
        assert_eq!(rt.models()[0].anim_speed, 16, "32 / 2");
    }

    #[test]
    fn a_faster_speed_ends_the_play_sooner() {
        let slow = cat(&[&[0xA3, 1, 1], &setb(0x30, 1), IDLE]);
        let fast = cat(&[&[0xBD, 0, 32, 0], &[0xA3, 1, 1], &setb(0x30, 1), IDLE]);
        let count = |main: &[u8]| {
            let mut rt = runtime(main, &[11, 11]);
            (1..=40).find(|_| {
                rt.tick(None);
                var(&rt, 0x30) == 1
            })
        };
        assert_eq!(count(&slow), Some(12));
        assert_eq!(count(&fast), Some(7));
    }

    #[test]
    fn kawai_advances_by_its_length_byte_and_kawiw_waits_once() {
        // KAWAI len 6, sub 1 (transparency), 3 payload bytes; KAWIW; set var.
        let main = cat(&[&[0x28, 6, 1, 7, 8, 9], &[0x29], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 0, "KAWIW yields while the effect runs");
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1);
        let fx = rt.drain_kawai_effects();
        assert_eq!(fx, vec![KawaiEffect { model: 0, sub: 1, payload: vec![7, 8, 9] }]);
        assert!(rt.coverage().unsupported.is_empty());
        assert_eq!(rt.coverage().anomalies, 0);
    }

    #[test]
    fn a_kawai_shorter_than_its_header_counts_an_anomaly_and_does_not_hang() {
        let main = cat(&[&[0x28, 0, 0], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1);
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn eye_texture_kawai_and_blink_set_the_blink_flag() {
        // BLINK 1 disables auto blink; KAWAI eyes (1,1,0,..) restores it.
        let main = cat(&[&[0x26, 1], &setb(0x30, 1), &[0x28, 7, 0, 1, 1, 0, 0], IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1);
        assert!(rt.model_animation(0).unwrap().auto_blink, "default eyes hand blinking back to the engine");
        let main = cat(&[&[0x26, 1], IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert!(!rt.model_animation(0).unwrap().auto_blink);
        let main = cat(&[&[0x28, 7, 0, 2, 2, 0, 0], IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert!(!rt.model_animation(0).unwrap().auto_blink, "other eyes take blinking over");
    }

    #[test]
    fn ccanm_records_the_player_animations() {
        let main = cat(&[&[0xDC, 9, 1, 1], &[0xDC, 8, 1, 2], &[0xDC, 7, 1, 0], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[4]);
        rt.tick(None);
        assert_eq!(rt.player_animations(), [7, 9, 8]);
        assert_eq!(var(&rt, 0x30), 1, "CCANM does not yield");
    }

    #[test]
    fn every_op_on_an_entity_without_a_model_advances_by_its_own_length() {
        let ops: [&[u8]; 14] = [
            &[0xA2, 1, 1],
            &[0xA3, 1, 1],
            &[0xAC],
            &[0xAE, 1, 1],
            &[0xAF, 1, 1],
            &[0xB0, 1, 0, 3, 1],
            &[0xB1, 1, 0, 3, 1],
            &[0xBA, 1, 1],
            &[0xBB, 1, 0, 3, 1],
            &[0xBC, 1, 0, 3, 1],
            &[0xBD, 0, 8, 0],
            &[0xDD],
            &[0x26, 1],
            &[0x28, 5, 1, 2, 3],
        ];
        for op in ops {
            // Not even one DFANM-style yield may be needed: every op but DFANM continues.
            let main = cat(&[op, &[0x29], &setb(0x30, 1), IDLE]);
            let mut s = SynthScript::default().entity("m", vec![script0(&[], &main)]);
            s.scale = 512;
            let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
            rt.init();
            rt.tick(None);
            rt.tick(None);
            assert_eq!(var(&rt, 0x30), 1, "op {:#04X}", op[0]);
            assert_eq!(rt.coverage().anomalies, 0);
        }
    }

    #[test]
    fn waiting_plays_and_kawiw_in_an_init_part_do_not_break_the_entity() {
        let init = cat(&[
            &CHAR,
            &[0xA3, 1, 1],
            &[0xBA, 1, 1],
            &[0xAE, 1, 1],
            &[0xAC],
            &[0xAF, 2, 1],
            &[0xAF, 2, 1],
            &[0x28, 5, 1, 2, 3],
            &[0x29],
            &[0xBB, 1, 0, 3, 1],
            &setb(0x31, 1),
        ]);
        let mut rt = runtime_with(&init, &cat(&[&setb(0x30, 1), IDLE]), &[6, 6, 6]);
        assert_eq!(var(&rt, 0x31), 1, "init ran to its RET");
        rt.tick(None);
        assert_eq!(var(&rt, 0x30), 1, "main runs");
        assert_eq!(rt.coverage().undecodable, 0);
    }

    #[test]
    fn the_models_status_machine_loops_the_default_animation() {
        let main = cat(&[&[0xA2, 1, 1], IDLE]);
        let mut rt = runtime(&main, &[4, 4]);
        let mut seen = Vec::new();
        for _ in 0..12 {
            rt.tick(None);
            seen.push(rt.model_animation(0).unwrap().frame);
        }
        // Shown after the advance: frames 1, 2, 3 (the last), then the state pass wraps the counter to 0.
        assert_eq!(seen, [1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn frame_counts_are_keyed_by_loader_index_and_zero_means_unknown() {
        let main = cat(&[&[0xA3, 1, 1], &setb(0x30, 1), IDLE]);
        let mut rt = runtime(&main, &[]);
        rt.set_animation_frame_counts(&[vec![3, 3, 0]]);
        assert_eq!(rt.anim.animation_frames(0, 1), Some(3));
        assert_eq!(rt.anim.animation_frames(0, 2), None, "0 is unknown");
        assert_eq!(rt.anim.animation_frames(0, 7), None);
        assert_eq!(rt.anim.animation_frames(9, 0), None);
        // Counts given after init still apply to plays that start later: a 3-frame play releases on tick 4.
        let released = (1..=30).find(|_| {
            rt.tick(None);
            var(&rt, 0x30) == 1
        });
        assert_eq!(released, Some(4));
    }
}
