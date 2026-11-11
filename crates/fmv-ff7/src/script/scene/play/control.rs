//! Opt-in cuts at executed control instructions, with the original scheduler retained.
//!
//! A cut is replay evidence, not permission to release player input. The host must review
//! the whole control policy, pending actor work, persistence and ownership before admission.
//! Nothing here detaches actors, drains their jobs, or changes the legacy scene API/identity.

use super::*;
use crate::script::Instruction;

/// Which authored instructions cause a cut. Locks are always observed, but never cut.
/// Each selected instruction yields separately, including repeated instructions at one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPolicy {
    pub user_release: bool,
    pub menu_release: bool,
    pub actor_selection: bool,
}

impl ControlPolicy {
    pub(crate) fn yields(self, op: ControlOp) -> bool {
        match op {
            ControlOp::User { disabled: 0 } => self.user_release,
            ControlOp::Menu { disabled: 0 } => self.menu_release,
            ControlOp::Actor { .. } => self.actor_selection,
            _ => false,
        }
    }
}

/// Literal operands are retained: no nonzero value is normalized into an assumed enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOp {
    User { disabled: u8 },
    Menu { disabled: u8 },
    Actor { entity: u8 },
}

impl ControlOp {
    pub(crate) fn from_instruction(ins: &Instruction) -> Option<Self> {
        let literal = |name| ins.literal(name).and_then(|n| u8::try_from(n).ok());
        match ins.opcode {
            0x33 => Some(Self::User { disabled: literal("disabled")? }),
            0x4A => Some(Self::Menu { disabled: literal("disabled")? }),
            0xBF => Some(Self::Actor { entity: literal("entity")? }),
            _ => None,
        }
    }
}

/// Last actually executed instruction for each control setting; absent remains unknown.
/// Quiet setup contributes observations but never produces a control boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlState {
    pub user_disabled: Option<u8>,
    pub menu_disabled: Option<u8>,
    pub actor: Option<u8>,
}

impl ControlState {
    pub(crate) fn observe(&mut self, op: ControlOp) {
        match op {
            ControlOp::User { disabled } => self.user_disabled = Some(disabled),
            ControlOp::Menu { disabled } => self.menu_disabled = Some(disabled),
            ControlOp::Actor { entity } => self.actor = Some(entity),
        }
    }
}

/// Source/input-bound evidence of a cut immediately after this instruction executed.
/// The ordinal counts all foreground control settings, including non-yielding locks.
/// It is not a durable instance ID: the host must bind its own account/visit/operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlCheckpoint {
    pub identity: ReplayIdentity,
    /// Canonical accepted-input chain, binding the deterministic retained scheduler even
    /// when different choices converge on the same instruction, frame and control flags.
    pub accepted_prefix: [u8; 32],
    pub ordinal: u64,
    pub entity: u8,
    pub instruction_offset: u32,
    pub frame: u32,
    pub operation: ControlOp,
    pub after: ControlState,
}

impl ControlCheckpoint {
    /// Canonical comparison key. This is replay evidence, not authentication or a save token.
    pub fn identity_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![1, 3];
        bytes.extend(self.identity.version.to_le_bytes());
        bytes.extend(self.identity.fingerprint);
        bytes.extend(self.identity.seed.to_le_bytes());
        bytes.extend(self.accepted_prefix);
        bytes.extend(self.ordinal.to_le_bytes());
        bytes.push(self.entity);
        bytes.extend(self.instruction_offset.to_le_bytes());
        bytes.extend(self.frame.to_le_bytes());
        bytes.extend(match self.operation {
            ControlOp::User { disabled } => [0, disabled],
            ControlOp::Menu { disabled } => [1, disabled],
            ControlOp::Actor { entity } => [2, entity],
        });
        for value in [self.after.user_disabled, self.after.menu_disabled, self.after.actor] {
            bytes.push(u8::from(value.is_some()));
            if let Some(value) = value { bytes.push(value); }
        }
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlBoundary { Scene(SceneBoundary), Control(ControlCheckpoint) }

/// Acknowledgements cannot skip a boundary or substitute a different kind of input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAck { Window(WindowAck), Control(ControlCheckpoint) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlInput { Ack(ControlAck), Battle(BattleResume) }

/// Stage times remain relative to the preceding boundary, including a control cut.
/// Motion already emitted may extend beyond the cut and continues in the retained VM.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlBeat {
    Scene(Beat),
    Control { stage: Vec<Timed<Stage>>, at: u32, checkpoint: ControlCheckpoint },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ControlError {
    #[error(transparent)]
    Scene(#[from] SceneError),
    #[error("the acknowledgement does not match the pending control or window boundary")]
    Acknowledgement,
    #[error("the control replay source, original inputs, policy or limits differ")]
    Identity,
    #[error("the control replay does not end at the recorded boundary")]
    Boundary,
}

/// Private scheduler output: it is never a terminal `Then` and cannot call `Vm::finish`.
pub(crate) enum VmBeat {
    Scene(Beat),
    Control { stage: Vec<Timed<Stage>>, at: u32, checkpoint: ExecutedControl },
}
impl From<Beat> for VmBeat { fn from(beat: Beat) -> Self { Self::Scene(beat) } }

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecutedControl {
    pub ordinal: u64,
    pub entity: u8,
    pub instruction_offset: u32,
    pub frame: u32,
    pub operation: ControlOp,
    pub after: ControlState,
}

/// Checked scene playback with an explicit opt-in control policy. The inner play is never
/// exposed mutably, so its legacy API cannot bypass a pending cut. Replay reconstructs the
/// entire scheduler from original inputs and accepted events; a checkpoint alone is no save.
pub struct ControlScenePlay {
    play: ScenePlay,
    policy: ControlPolicy,
    identity: ReplayIdentity,
    accepted_prefix: [u8; 32],
    pending: Option<ControlCheckpoint>,
    transcript: Vec<ControlInput>,
}

impl ControlScenePlay {
    #[allow(clippy::too_many_arguments)]
    pub fn with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32,
        resources: SceneResources, policy: ControlPolicy, limits: Limits) -> Result<Self, ControlError> {
        if def.resume_after_battle.is_some() { return Err(SceneError::UncheckedReplay.into()); }
        let script = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        let play = ScenePlay::with_script(field, def, story, seed, limits, &script, resources, None)?;
        Ok(Self::wrap(play, field, story, resources, policy, limits))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_scratch(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32,
        resources: SceneResources, scratch: &FieldScratch, policy: ControlPolicy, limits: Limits) -> Result<Self, ControlError> {
        if def.resume_after_battle.is_some() { return Err(SceneError::UncheckedReplay.into()); }
        let play = ScenePlay::with_scratch_and_limits(field, def, story, seed, resources, scratch, limits)?;
        Ok(Self::wrap(play, field, story, resources, policy, limits))
    }

    fn wrap(play: ScenePlay, field: &FieldFile, story: &StoryState, resources: SceneResources,
        policy: ControlPolicy, limits: Limits) -> Self {
        // A separate domain preserves every legacy identity. Full field bytes additionally
        // bind the floor/model data; the base binds the actual script, definition and scratch.
        let mut hash = blake3::Hasher::new();
        hash.update(b"fmv-ff7/control-scene-replay\0v1\0");
        let base = play.replay_identity();
        hash.update(&base.version.to_le_bytes());
        hash.update(&base.fingerprint);
        hash.update(&(field.bytes().len() as u64).to_le_bytes());
        hash.update(field.bytes());
        let story = story.encode();
        hash.update(&(story.len() as u64).to_le_bytes());
        hash.update(&story);
        hash.update(&[u8::from(resources.gil.is_some())]);
        if let Some(gil) = resources.gil { hash.update(&gil.to_le_bytes()); }
        hash.update(&[u8::from(policy.user_release), u8::from(policy.menu_release), u8::from(policy.actor_selection)]);
        for limit in [limits.steps as u64, limits.windows as u64, limits.threads as u64, u64::from(limits.frames)] {
            hash.update(&limit.to_le_bytes());
        }
        let identity = ReplayIdentity { fingerprint: *hash.finalize().as_bytes(), ..base };
        let mut prefix = blake3::Hasher::new();
        prefix.update(b"fmv-ff7/control-scene-prefix\0v1\0initial\0");
        prefix.update(&identity.version.to_le_bytes());
        prefix.update(&identity.fingerprint);
        prefix.update(&identity.seed.to_le_bytes());
        let accepted_prefix = *prefix.finalize().as_bytes();
        Self { play, policy, identity, accepted_prefix, pending: None, transcript: Vec::new() }
    }

    /// Read-only cast/animation/ground/effect inspection. Use this wrapper's boundary and
    /// transcript: a control cut is intentionally absent from the legacy boundary enum.
    pub fn scene(&self) -> &ScenePlay { &self.play }
    pub fn replay_identity(&self) -> ReplayIdentity { self.identity }
    pub fn transcript(&self) -> &[ControlInput] { &self.transcript }
    pub fn control_state(&self) -> ControlState { self.play.vm.observed_control() }
    pub fn boundary(&self) -> Option<ControlBoundary> {
        self.pending.map(ControlBoundary::Control).or_else(|| self.play.boundary().map(ControlBoundary::Scene))
    }

    fn validate(&self) -> Result<(), ControlError> {
        self.play.validate_checked_execution()?;
        if self.play.vm.has_implicit_choice() { return Err(SceneError::ImplicitAnswer.into()); }
        if self.play.vm.has_invalid_message() { return Err(SceneError::WindowData.into()); }
        Ok(())
    }

    /// Execute through the next selected control instruction, window, or ordinary termination.
    /// A rejected acknowledgement changes no thread, clock, RNG, effect, or transcript.
    pub fn next_strict(&mut self, ack: Option<ControlAck>) -> Result<ControlBeat, ControlError> {
        self.validate()?;
        match (self.boundary(), ack) {
            (None, None) => {},
            (Some(ControlBoundary::Control(expected)), Some(ControlAck::Control(actual))) if expected == actual => {},
            (Some(ControlBoundary::Scene(SceneBoundary::Window(expected))), Some(ControlAck::Window(actual)))
                if expected == actual.checkpoint => match (expected.choices, actual.answer) {
                    (None, None) => {},
                    (Some([first, last]), Some(answer)) if (first..=last).contains(&answer) => {},
                    _ => return Err(ControlError::Acknowledgement),
                },
            _ => return Err(ControlError::Acknowledgement),
        }
        if let Some(ack) = ack {
            match ack {
                ControlAck::Control(_) => self.pending = None,
                ControlAck::Window(ack) => {
                    self.play.vm.accept_window(ack.answer);
                    self.play.transcript.push(SceneInput::Window(ack));
                }
            }
            self.record(ControlInput::Ack(ack));
        }
        self.play.strict = true;
        let script = FieldScript::parse(&self.play.section).map_err(|e| SceneError::Script(e.to_string()))?;
        let output = self.play.vm.resume_control(&script, self.policy);
        drop(script);
        let beat = match output {
            VmBeat::Scene(mut beat) => {
                self.play.refine(&mut beat);
                if let Then::End { how, .. } = beat.then { self.play.ended = Some(how); }
                ControlBeat::Scene(beat)
            }
            VmBeat::Control { mut stage, at, checkpoint: c } => {
                self.play.refine_stage(&mut stage);
                let checkpoint = ControlCheckpoint { identity: self.identity, accepted_prefix: self.accepted_prefix, ordinal: c.ordinal,
                    entity: c.entity, instruction_offset: c.instruction_offset, frame: c.frame,
                    operation: c.operation, after: c.after };
                self.pending = Some(checkpoint);
                ControlBeat::Control { stage, at, checkpoint }
            }
        };
        self.validate()?;
        Ok(beat)
    }

    /// Retains the same battle authority requirements as ScenePlay; a control ACK cannot
    /// substitute for an authoritative victorious afterimage.
    pub fn resume_battle_checked(&mut self, resume: BattleResume) -> Result<(), ControlError> {
        if self.pending.is_some() { return Err(ControlError::Acknowledgement); }
        self.play.resume_battle_checked(resume.clone())?;
        self.record(ControlInput::Battle(resume));
        Ok(())
    }

    fn record(&mut self, input: ControlInput) {
        let mut hash = blake3::Hasher::new();
        hash.update(b"fmv-ff7/control-scene-prefix\0v1\0event\0");
        hash.update(&self.accepted_prefix);
        // Every variable-sized value is framed. Optional values retain presence, and
        // event kinds distinguish a control ACK from a window ACK or battle resume.
        let blob = |hash: &mut blake3::Hasher, bytes: &[u8]| {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        };
        match &input {
            ControlInput::Ack(ControlAck::Window(ack)) => {
                hash.update(&[0]);
                blob(&mut hash, &ack.checkpoint.identity_bytes());
                hash.update(&[u8::from(ack.answer.is_some())]);
                if let Some(answer) = ack.answer { hash.update(&[answer]); }
            }
            ControlInput::Ack(ControlAck::Control(checkpoint)) => {
                hash.update(&[1]);
                blob(&mut hash, &checkpoint.identity_bytes());
            }
            ControlInput::Battle(resume) => {
                hash.update(&[2]);
                blob(&mut hash, &resume.checkpoint.identity_bytes());
                hash.update(&[match resume.resolution { BattleResolution::Victory => 0 }]);
                blob(&mut hash, &resume.story.encode());
                hash.update(&[u8::from(resume.resources.gil.is_some())]);
                if let Some(gil) = resume.resources.gil { hash.update(&gil.to_le_bytes()); }
            }
        }
        self.accepted_prefix = *hash.finalize().as_bytes();
        self.transcript.push(input);
    }

    /// Reconstruct an exact endpoint from a fresh play built with the original inputs.
    /// Consumes self so failure cannot expose a partially reconstructed runnable object.
    pub fn replay_to(mut self, identity: ReplayIdentity, transcript: &[ControlInput], target: ControlBoundary) -> Result<Self, ControlError> {
        if self.identity != identity { return Err(ControlError::Identity); }
        if self.boundary().is_some() || !self.transcript.is_empty() || self.play.strict {
            return Err(SceneError::UncheckedReplay.into());
        }
        self.next_strict(None)?;
        for input in transcript {
            match input {
                ControlInput::Ack(ack) => { self.next_strict(Some(*ack))?; }
                ControlInput::Battle(resume) => {
                    self.resume_battle_checked(resume.clone())?;
                    self.next_strict(None)?;
                }
            }
        }
        if self.boundary() != Some(target) { return Err(ControlError::Boundary); }
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
