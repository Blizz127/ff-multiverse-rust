//! Opt-in retained field entry and externally observed movies, not a live opening profile.
//!
//! Every original slot-0 initializer runs visibly in entity order, then the requested
//! original mains start. Initializers use the existing cooperative scheduler (including
//! its waits/requests), not a claim of native zero-time initializer dispatch. No cast pose
//! is invented. Ordinary quiet setup, scene identities and replay APIs are unchanged.
//!
//! The caller advances a field-frame horizon, handles returned boundaries, and supplies
//! movie observations only at that reached field frame. Movie ordinals never advance field
//! time. PMVIE and MOVIE block their issuer only; other jobs and motion remain retained.
//! Windows still use the existing whole-scene boundary: per-issuer dialogue, native-party
//! opening admission and release of live player control are separate follow-on work.

use super::*;
use crate::script::scene::{ScriptRef, Step};
use crate::script::scene::vm::entry::{EntryOutput, EntryWait, MovieCommand};

/// Opaque request capability, bound to source, run nonce, accepted inputs and instruction.
/// This is in-process replay evidence, not network authentication or save authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieToken([u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieRequestKind { Prepare, Play }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieRequest {
    pub token: MovieToken,
    pub kind: MovieRequestKind,
    pub movie: u8,
    pub entity: u8,
    pub instruction_offset: u32,
    pub frame: u32,
}

/// Verified media identity supplied by the host, never inferred from a movie number.
/// The fingerprint must bind asset bytes, timestamp/index metadata and the host's chosen
/// presentation-clock contract. The VM only checks frame bounds and binds this evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieAsset {
    pub frames: u32,
    pub timeline_fingerprint: [u8; 32],
    pub counter_profile: MovieCounterProfile,
}

/// Host-selected interpretation of MVIEF observations. Native graph-frame position and
/// FFNx's nominal one-based slot are not interchangeable with decoded/presented ordinal.
/// This slice supplies no default mapping. The fingerprint binds the chosen convention;
/// `maximum` bounds its monotone u16 values (wrapping conventions are unsupported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieCounterProfile {
    pub fingerprint: [u8; 32],
    pub maximum: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieCounterObservation {
    pub value: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieAck {
    Prepared { token: MovieToken, asset: MovieAsset },
    /// An actually presented ordinal, not a decoded frame or guessed field-time conversion.
    /// Repeated/rewound observations are rejected; skipped ordinals are allowed.
    Presented { token: MovieToken, ordinal: u16 },
    /// The selected profile's movie counter, independently observed from presented ordinal.
    /// Only this input updates MVIEF; no frame-rate, +1, or ordinal mapping is inferred.
    Counter { token: MovieToken, observation: MovieCounterObservation },
    /// Explicit host attestation of successful decoder EOF AND the final presented frame's
    /// presentation endpoint, after satisfying the separately chosen audio completion policy.
    /// The final ordinal alone never proves this and never releases MOVIE. The VM validates
    /// matching request and final ordinal, not the host's historical display/decoder evidence.
    Completed { token: MovieToken },
    Cancelled { token: MovieToken },
    Refused { token: MovieToken },
}

impl MovieAck {
    fn token(self) -> MovieToken {
        match self {
            Self::Prepared { token, .. } | Self::Presented { token, .. } | Self::Counter { token, .. } | Self::Completed { token }
                | Self::Cancelled { token } | Self::Refused { token } => token,
        }
    }
}

/// Source music/AKAO observations that the legacy stage does not execute. Literal operands
/// remain literal; banked operands remain source addresses and are NOT resolved audio commands.
/// A future audio adapter must explicitly support/refuse them, including MULCK semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAudio {
    pub frame: u32,
    pub entity: u8,
    pub instruction: Instruction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryYield {
    Scene(Then),
    Control(ControlCheckpoint),
    Movie(MovieRequest),
    /// Unsupported overlap, refused before executing BATTLE or publishing its checkpoint.
    /// The driver stops; the outstanding movie can still be explicitly cancelled/refused.
    MovieBattleRefused { entity: u8, instruction_offset: u32 },
    /// Scheduled work remains after the supplied horizon.
    Horizon,
    /// A movie observation is outstanding; no work is due through the current horizon.
    /// Other retained jobs may be scheduled later and remain runnable on later advances.
    AwaitExternal,
    /// Parked/non-runnable work remains, with no movie request. No synthetic RET is applied.
    Idle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntryBeat {
    pub stage: Vec<Timed<Stage>>,
    pub audio: Vec<EntryAudio>,
    /// Elapsed field frames since the previous yielded beat.
    pub at: u32,
    /// Absolute reached field frame. A boundary can precede the requested horizon.
    pub frame: u32,
    pub then: EntryYield,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryInput {
    Advance(u32),
    AdvanceBefore(u32),
    Control(ControlInput),
    Movie { frame: u32, acknowledgement: MovieAck },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EntryError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error("the input does not match the retained entry boundary or movie observation")]
    Input,
    #[error("the field horizon rewinds or exceeds the retained entry budget")]
    Horizon,
    #[error("the original initializer/main profile cannot be constructed")]
    Profile,
    #[error("the retained entry was explicitly cancelled")]
    Cancelled,
    #[error("the host refused the retained entry's movie request")]
    Refused,
    #[error("a battle while a movie request is outstanding is unsupported by retained entry")]
    MovieBattle,
}

#[derive(Clone, Copy)]
struct PendingMovie {
    request: MovieRequest,
    command: MovieCommand,
    presented: Option<u16>,
    counter: Option<u16>,
}

/// `initial_counter` is an explicit original input, including inherited movie state.
/// PMVIE does not reset it; only a profiled Counter acknowledgement changes MVIEF.
pub struct EntryScenePlay {
    inner: ControlScenePlay,
    run: [u8; 16],
    limits: Limits,
    pending_movie: Option<PendingMovie>,
    asset: Option<MovieAsset>,
    stopped: Option<EntryError>,
    transcript: Vec<EntryInput>,
}

impl EntryScenePlay {
    #[allow(clippy::too_many_arguments)]
    pub fn with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32,
        resources: SceneResources, mains: &[u8], run: [u8; 16], initial_counter: MovieCounterObservation,
        policy: ControlPolicy, limits: Limits) -> Result<Self, EntryError> {
        Self::build(field, def, story, seed, resources, None, mains, run, initial_counter, policy, limits)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_scratch(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32,
        resources: SceneResources, scratch: &FieldScratch, mains: &[u8], run: [u8; 16], initial_counter: MovieCounterObservation,
        policy: ControlPolicy, limits: Limits) -> Result<Self, EntryError> {
        Self::build(field, def, story, seed, resources, Some(scratch), mains, run, initial_counter, policy, limits)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources,
        scratch: Option<&FieldScratch>, mains: &[u8], run: [u8; 16], initial_counter: MovieCounterObservation,
        policy: ControlPolicy, limits: Limits) -> Result<Self, EntryError> {
        if def.resume_after_battle.is_some() || mains.is_empty() || limits.threads < 2 || def.cast.iter().any(|c| c.start.is_some()) {
            return Err(EntryError::Profile);
        }
        let script = FieldScript::from_field(field).map_err(|e| ControlError::Scene(SceneError::Script(e.to_string())))?;
        if mains.iter().copied().collect::<BTreeSet<_>>().len() != mains.len()
            || mains.iter().any(|&entity| !script.entity_inits(None).iter()
                .any(|init| init.entity == usize::from(entity) && init.main_start.is_some())) {
            return Err(EntryError::Profile);
        }
        let mut retained = def.clone();
        retained.setup.clear();
        retained.root.clear();
        for entity in 0..script.entity_count() {
            let entity = u8::try_from(entity).map_err(|_| EntryError::Profile)?;
            if script.span(usize::from(entity), 0).is_none() { return Err(EntryError::Profile); }
            retained.root.push(Step::Await(ScriptRef::new(entity, 0)));
        }
        retained.root.extend(mains.iter().copied().map(Step::StartMain));
        let play = ScenePlay::with_script(field, &retained, story, seed, limits, &script, resources, scratch)
            .map_err(ControlError::Scene)?;
        let mut inner = ControlScenePlay::wrap(play, field, story, resources, policy, limits);
        let mut hash = blake3::Hasher::new();
        hash.update(b"fmv-ff7/retained-entry\0v1\0ordered-visible-init-cooperative-main\0");
        hash.update(&inner.identity.fingerprint);
        hash.update(&run);
        hash.update(&initial_counter.value.to_le_bytes());
        // Bind the caller's original definition as well as the effective entry plan.
        hash.update(&crate::script::scene::replay_identity::fingerprint_with_scratch(script.bytes(), def, scratch));
        inner.identity.fingerprint = *hash.finalize().as_bytes();
        let mut prefix = blake3::Hasher::new();
        prefix.update(b"fmv-ff7/retained-entry-prefix\0v1\0");
        prefix.update(&inner.identity.version.to_le_bytes());
        prefix.update(&inner.identity.fingerprint);
        prefix.update(&inner.identity.seed.to_le_bytes());
        prefix.update(&run);
        inner.accepted_prefix = *prefix.finalize().as_bytes();
        inner.play.vm.enable_entry(initial_counter.value);
        Ok(Self { inner, run, limits, pending_movie: None, asset: None, stopped: None, transcript: Vec::new() })
    }

    pub fn scene(&self) -> &ScenePlay { self.inner.scene() }
    pub fn control_state(&self) -> ControlState { self.inner.control_state() }
    pub fn replay_identity(&self) -> ReplayIdentity { self.inner.identity }
    pub fn transcript(&self) -> &[EntryInput] { &self.transcript }
    pub fn field_frame(&self) -> u32 { self.inner.play.vm.entry_clock() }
    pub fn movie_counter(&self) -> MovieCounterObservation {
        MovieCounterObservation { value: self.inner.play.vm.entry_movie_counter() }
    }
    pub fn cancelled(&self) -> bool { self.stopped == Some(EntryError::Cancelled) }
    pub fn boundary(&self) -> Option<ControlBoundary> { self.inner.boundary() }
    /// The still-outstanding request, including after replay stops before its ACK. This
    /// exposes its existing token without issuing a new request or advancing the VM.
    pub fn pending_movie_request(&self) -> Option<MovieRequest> {
        self.pending_movie.map(|pending| pending.request)
    }

    fn live(&self) -> Result<(), EntryError> {
        if let Some(error) = &self.stopped { return Err(error.clone()); }
        self.inner.validate()?;
        Ok(())
    }

    /// Drain only work due at or before this field frame. Call again after an intermediate
    /// boundary to reach the horizon. External observations may then be acknowledged at the
    /// reached tick before further dispatch. A pending movie is not a whole-scene barrier.
    pub fn advance_to(&mut self, horizon: u32) -> Result<EntryBeat, EntryError> {
        self.advance(horizon, true)
    }

    /// Drain jobs strictly before `tick`, leaving that tick's source instructions untouched.
    /// On reaching it, acknowledge external observations and then call `advance_to(tick)`:
    /// due MVIEF instructions see the sampled counter before dispatch, not one tick late.
    /// Tick zero runs no initializer. Earlier control/movie/window boundaries still yield
    /// at their actual frame; handle them and call again to reach the requested tick.
    /// This does not undo jobs already dispatched by an earlier inclusive call at this tick.
    pub fn advance_before(&mut self, tick: u32) -> Result<EntryBeat, EntryError> {
        self.advance(tick, false)
    }

    fn advance(&mut self, horizon: u32, inclusive: bool) -> Result<EntryBeat, EntryError> {
        self.live()?;
        if horizon < self.field_frame() || horizon > self.limits.frames { return Err(EntryError::Horizon); }
        if self.inner.boundary().is_some() { return Err(EntryError::Input); }
        let script = FieldScript::parse(&self.inner.play.section)
            .map_err(|e| ControlError::Scene(SceneError::Script(e.to_string())))?;
        drop(script);
        self.record(if inclusive { EntryInput::Advance(horizon) } else { EntryInput::AdvanceBefore(horizon) });
        self.inner.play.strict = true;
        let script = FieldScript::parse(&self.inner.play.section)
            .map_err(|e| ControlError::Scene(SceneError::Script(e.to_string())))?;
        let output = self.inner.play.vm.resume_entry(&script, self.inner.policy, horizon, inclusive);
        drop(script);
        let (mut stage, at, then) = match output {
            EntryOutput::Boundary(VmBeat::Scene(mut beat)) => {
                self.inner.play.refine(&mut beat);
                let at = match &beat.then { Then::Window(w) => w.at, Then::End { at, .. } => *at };
                if let Then::End { how, .. } = beat.then { self.inner.play.ended = Some(how); }
                (beat.stage, at, EntryYield::Scene(beat.then))
            }
            EntryOutput::Boundary(VmBeat::Control { stage, at, checkpoint: c }) => {
                let checkpoint = ControlCheckpoint { identity: self.inner.identity, accepted_prefix: self.inner.accepted_prefix,
                    ordinal: c.ordinal, entity: c.entity, instruction_offset: c.instruction_offset, frame: c.frame,
                    operation: c.operation, after: c.after };
                self.inner.pending = Some(checkpoint);
                (stage, at, EntryYield::Control(checkpoint))
            }
            EntryOutput::Movie { stage, at, command } => {
                let request = self.movie_request(command);
                self.pending_movie = Some(PendingMovie { request, command, presented: None, counter: None });
                (stage, at, EntryYield::Movie(request))
            }
            EntryOutput::MovieBattleRefused { stage, at, entity, instruction_offset } => {
                self.stopped = Some(EntryError::MovieBattle);
                (stage, at, EntryYield::MovieBattleRefused { entity, instruction_offset })
            }
            EntryOutput::Yield { stage, at, reason } => (stage, at, match reason {
                EntryWait::Horizon => EntryYield::Horizon,
                EntryWait::External => EntryYield::AwaitExternal,
                EntryWait::Idle => EntryYield::Idle,
            }),
        };
        // Scene beats were already refined above (including their window's speaker); repeating
        // place refinement is harmless but would double off-cast accounting, so only refine
        // the other output kinds here.
        if !matches!(then, EntryYield::Scene(_)) { self.inner.play.refine_stage(&mut stage); }
        let audio = self.inner.play.vm.take_entry_audio();
        self.inner.validate()?;
        Ok(EntryBeat { stage, audio, at, frame: self.field_frame(), then })
    }

    pub fn acknowledge(&mut self, ack: ControlAck) -> Result<(), EntryError> {
        self.live()?;
        match (self.inner.boundary(), ack) {
            (Some(ControlBoundary::Control(expected)), ControlAck::Control(actual)) if expected == actual => {},
            (Some(ControlBoundary::Scene(SceneBoundary::Window(expected))), ControlAck::Window(actual))
                if expected == actual.checkpoint => match (expected.choices, actual.answer) {
                    (None, None) => {},
                    (Some([first, last]), Some(answer)) if (first..=last).contains(&answer) => {},
                    _ => return Err(EntryError::Input),
                },
            _ => return Err(EntryError::Input),
        }
        match ack {
            ControlAck::Control(_) => self.inner.pending = None,
            ControlAck::Window(ack) => {
                self.inner.play.vm.accept_window(ack.answer);
                self.inner.play.transcript.push(SceneInput::Window(ack));
            }
        }
        self.record(EntryInput::Control(ControlInput::Ack(ack)));
        Ok(())
    }

    pub fn resume_battle_checked(&mut self, resume: BattleResume) -> Result<(), EntryError> {
        self.live()?;
        if self.pending_movie.is_some() { return Err(EntryError::Input); }
        self.inner.resume_battle_checked(resume.clone())?;
        self.transcript.push(EntryInput::Control(ControlInput::Battle(resume)));
        Ok(())
    }

    /// Validate the entire observation before touching retained PC, clocks, scratch, RNG,
    /// stage effects or transcript. Cancellation/refusal never pretends the movie finished:
    /// it latches a driver error while leaving the suspended VM itself untouched.
    pub fn acknowledge_movie(&mut self, ack: MovieAck) -> Result<(), EntryError> {
        let stopping = matches!(ack, MovieAck::Cancelled { .. } | MovieAck::Refused { .. });
        if self.stopped == Some(EntryError::MovieBattle) && stopping {
            // Unsupported coexistence still permits cleanup of its outstanding media.
            // Nothing acknowledges BATTLE or resumes source execution here.
            self.inner.validate()?;
        } else { self.live()?; }
        // A global control/window boundary pauses observations in this bounded slice, but
        // explicit cancellation/refusal may stop it there without consuming that boundary.
        if self.inner.boundary().is_some() && !stopping {
            return Err(EntryError::Input);
        }
        let pending = self.pending_movie.filter(|p| p.request.token == ack.token()).ok_or(EntryError::Input)?;
        match ack {
            MovieAck::Prepared { asset, .. } => {
                if pending.request.kind != MovieRequestKind::Prepare || asset.frames == 0 || asset.frames > 65_536 {
                    return Err(EntryError::Input);
                }
                if !self.inner.play.vm.accept_entry_movie(pending.command.ordinal) { return Err(EntryError::Input); }
                self.asset = Some(asset);
                self.pending_movie = None;
            }
            MovieAck::Presented { ordinal, .. } => {
                if pending.request.kind != MovieRequestKind::Play
                    || self.asset.is_none_or(|a| u32::from(ordinal) >= a.frames)
                    || pending.presented.is_some_and(|old| ordinal <= old) { return Err(EntryError::Input); }
                let Some(current) = self.pending_movie.as_mut() else { return Err(EntryError::Input) };
                current.presented = Some(ordinal);
            }
            MovieAck::Counter { observation, .. } => {
                if pending.request.kind != MovieRequestKind::Play
                    || self.asset.is_none_or(|a| observation.value > a.counter_profile.maximum)
                    || pending.counter.is_some_and(|old| observation.value <= old) { return Err(EntryError::Input); }
                let Some(current) = self.pending_movie.as_mut() else { return Err(EntryError::Input) };
                self.inner.play.vm.observe_entry_movie_counter(observation.value);
                current.counter = Some(observation.value);
            }
            MovieAck::Completed { .. } => {
                if pending.request.kind != MovieRequestKind::Play
                    || self.asset.is_none_or(|a| pending.presented.map(u32::from) != Some(a.frames - 1)) {
                    return Err(EntryError::Input);
                }
                if !self.inner.play.vm.accept_entry_movie(pending.command.ordinal) { return Err(EntryError::Input); }
                self.pending_movie = None;
            }
            MovieAck::Cancelled { .. } => self.stopped = Some(EntryError::Cancelled),
            MovieAck::Refused { .. } => self.stopped = Some(EntryError::Refused),
        }
        self.record(EntryInput::Movie { frame: self.field_frame(), acknowledgement: ack });
        Ok(())
    }

    fn movie_request(&self, command: MovieCommand) -> MovieRequest {
        let mut hash = blake3::Hasher::new();
        hash.update(b"fmv-ff7/retained-entry-movie\0v1\0");
        hash.update(&self.inner.identity.fingerprint);
        hash.update(&self.inner.identity.seed.to_le_bytes());
        hash.update(&self.run);
        hash.update(&self.inner.accepted_prefix);
        hash.update(&command.ordinal.to_le_bytes());
        hash.update(&[command.kind as u8, command.movie, command.entity]);
        hash.update(&command.instruction_offset.to_le_bytes());
        hash.update(&command.frame.to_le_bytes());
        if let Some(asset) = self.asset {
            hash.update(&asset.frames.to_le_bytes());
            hash.update(&asset.timeline_fingerprint);
            hash.update(&asset.counter_profile.fingerprint);
            hash.update(&asset.counter_profile.maximum.to_le_bytes());
        }
        MovieRequest { token: MovieToken(*hash.finalize().as_bytes()), kind: command.kind,
            movie: command.movie, entity: command.entity, instruction_offset: command.instruction_offset, frame: command.frame }
    }

    fn record(&mut self, input: EntryInput) {
        if let EntryInput::Control(input) = &input { self.inner.record(input.clone()); }
        else {
            let mut hash = blake3::Hasher::new();
            hash.update(b"fmv-ff7/retained-entry-input\0v1\0");
            hash.update(&self.inner.accepted_prefix);
            match input {
                EntryInput::Advance(frame) => { hash.update(&[0]); hash.update(&frame.to_le_bytes()); }
                EntryInput::AdvanceBefore(frame) => { hash.update(&[2]); hash.update(&frame.to_le_bytes()); }
                EntryInput::Movie { frame, acknowledgement } => {
                    hash.update(&[1]); hash.update(&frame.to_le_bytes()); hash.update(&acknowledgement.token().0);
                    match acknowledgement {
                        MovieAck::Prepared { asset, .. } => {
                            hash.update(&[0]); hash.update(&asset.frames.to_le_bytes()); hash.update(&asset.timeline_fingerprint);
                            hash.update(&asset.counter_profile.fingerprint); hash.update(&asset.counter_profile.maximum.to_le_bytes());
                        }
                        MovieAck::Presented { ordinal, .. } => { hash.update(&[1]); hash.update(&ordinal.to_le_bytes()); }
                        MovieAck::Counter { observation, .. } => { hash.update(&[5]); hash.update(&observation.value.to_le_bytes()); }
                        MovieAck::Completed { .. } => { hash.update(&[2]); }
                        MovieAck::Cancelled { .. } => { hash.update(&[3]); }
                        MovieAck::Refused { .. } => { hash.update(&[4]); }
                    }
                }
                EntryInput::Control(_) => unreachable!("handled above"),
            }
            self.inner.accepted_prefix = *hash.finalize().as_bytes();
        }
        self.transcript.push(input);
    }

    /// Replay requires a fresh driver with the same source/profile, run nonce and original
    /// inputs. Consuming self prevents rejected history from exposing partial execution.
    /// Media ACKs are replay evidence supplied by the host, never live media verification.
    pub fn replay(mut self, identity: ReplayIdentity, transcript: &[EntryInput]) -> Result<Self, EntryError> {
        if self.inner.identity != identity || !self.transcript.is_empty() || self.inner.play.strict {
            return Err(ControlError::Identity.into());
        }
        for input in transcript {
            match input {
                EntryInput::Advance(frame) => { self.advance_to(*frame)?; }
                EntryInput::AdvanceBefore(frame) => { self.advance_before(*frame)?; }
                EntryInput::Control(ControlInput::Ack(ack)) => self.acknowledge(*ack)?,
                EntryInput::Control(ControlInput::Battle(resume)) => self.resume_battle_checked(resume.clone())?,
                EntryInput::Movie { frame, acknowledgement } => {
                    if *frame != self.field_frame() { return Err(EntryError::Input); }
                    self.acknowledge_movie(*acknowledgement)?;
                }
            }
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
