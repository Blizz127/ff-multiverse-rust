//! Same-field, noncombat continuations bound to an existing visit. Attach records the
//! original input; each Advance durably accepts one exact window acknowledgment while
//! retaining the original story/resources/scratch. Complete commits the replayed effects
//! and clears the active link together. No runtime admission or anchor reconstruction.

#[cfg(test)]
mod size_bounds;

use fmv_core::AccountId;
use fmv_ff7::script::{FieldScratch, FieldScratchOutcome};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{with_trigger_position, Limits, Outcome, SceneBoundary, SceneDef,
    ScenePlay, SceneResources, WindowAck, REPLAY_VERSION};
use fmv_ff7::story::StoryState;
use fmv_persist::{FieldVisitV1, SceneCheckpoint, SceneOperation, SceneOperationKind,
    ScenePhase, SceneState, MAX_SCENE_REPLAY_BYTES};
use fmv_protocol::{SceneOrigin, SceneReplay, SceneReplayEvent, MAX_SCENE_REPLAY_EVENTS, MAX_SCENE_STEPS};
use serde::{Deserialize, Serialize};

use super::{checked_effect, next_state, IdleVisitError, IdleVisitSource};
use crate::saver::{validate_field_visit, SceneBatchJob, SceneBatchMember};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActiveVisitBoundary {
    Window { step: u32, identity: Vec<u8>, choices: Option<[u8; 2]> },
    Complete,
}

/// Private versioned replay data, never a SceneStart wire payload. The original visit is
/// immutable and includes all knowledge, entry and anchor bytes. Only accepted events grow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    version: u32,
    account: AccountId,
    visit: FieldVisitV1,
    origin: Option<SceneOrigin>,
    events: Vec<SceneReplayEvent>,
}

impl Input {
    fn encode(&self) -> Result<Vec<u8>, IdleVisitError> {
        let bytes = serde_json::to_vec(self).map_err(|_| IdleVisitError::State)?;
        if bytes.len() > MAX_SCENE_REPLAY_BYTES { return Err(IdleVisitError::State); }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, IdleVisitError> {
        if bytes.len() > MAX_SCENE_REPLAY_BYTES { return Err(IdleVisitError::State); }
        let input: Self = serde_json::from_slice(bytes).map_err(|_| IdleVisitError::State)?;
        if input.version != 1 || input.encode()? != bytes { return Err(IdleVisitError::State); }
        validate_field_visit(&input.visit)?; // Original input must be a coherent idle visit.
        Ok(input)
    }

    fn active_visit(&self, instance: [u8; 32]) -> Result<FieldVisitV1, IdleVisitError> {
        let mut visit = self.visit.clone();
        visit.revision = visit.revision.checked_add(1)
            .and_then(|revision| revision.checked_add(self.events.len() as u64)).ok_or(IdleVisitError::State)?;
        visit.active_continuation = Some(instance);
        Ok(visit)
    }
}

struct Execution {
    input: Input,
    play: ScenePlay,
    story: StoryState,
    scratch: FieldScratch,
    gil: u32,
}

impl IdleVisitSource<'_> {
    /// The caller has accepted the trigger and frozen its owner. Dynamic activation poses
    /// must be explicit; the opaque visit anchor is never guessed to be a SceneOrigin.
    pub(crate) fn attach_scene_job(&self, def: &SceneDef, expected: &SceneState,
        instance: [u8; 32], seed: u32, origin: Option<SceneOrigin>) -> Result<SceneBatchJob, IdleVisitError> {
        if instance == [0; 32] { return Err(IdleVisitError::State); }
        let visit = expected.field_visit.as_ref().ok_or(IdleVisitError::State)?;
        validate_field_visit(visit)?;
        let input = Input { version: 1, account: expected.pair.story.account_id,
            visit: visit.clone(), origin, events: Vec::new() };
        let mut execution = self.start_execution(def, expected, input, seed)?;
        let source_hash = self.binding(def, instance, &execution)?;
        replay(&mut execution)?;
        let boundary = audit(&execution)?.0; // No unsafe speculative prefix can attach.
        let visit = execution.input.active_visit(instance)?;
        ensure_capacity(&boundary, &visit, execution.input.events.len())?;
        let checkpoint = SceneCheckpoint {
            instance, scene_key: def.id.clone(), source_hash, replay_version: REPLAY_VERSION,
            original_story: expected.pair.story.blob.clone(), original_gil: execution.gil,
            seed, replay: execution.input.encode()?, phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
        };
        batch(expected, SceneOperationKind::AttachVisit, 0, instance, Some(checkpoint),
            visit, None)
    }

    /// Reconstruct only from the stored original and accepted prefix. A valid link alone
    /// is insufficient: source, definition, origin, revision and every window are checked.
    pub(crate) fn active_scene_boundary(&self, def: &SceneDef, expected: &SceneState)
        -> Result<ActiveVisitBoundary, IdleVisitError> {
        let execution = self.load_execution(def, expected)?;
        Ok(audit(&execution)?.0)
    }

    /// Persist exactly the next window acknowledgment. Story/resources/scratch are still
    /// speculative until completion; reconnect replays this durable prefix, not a new choice.
    pub(crate) fn advance_scene_job(&self, def: &SceneDef, expected: &SceneState,
        ack: SceneReplayEvent) -> Result<SceneBatchJob, IdleVisitError> {
        let mut execution = self.load_execution(def, expected)?;
        if execution.input.events.len() >= MAX_SCENE_REPLAY_EVENTS { return Err(IdleVisitError::Result); }
        accept(&mut execution.play, &ack)?;
        let boundary = audit(&execution)?.0;
        execution.input.events.push(ack);
        let mut checkpoint = expected.checkpoint.clone().ok_or(IdleVisitError::State)?;
        checkpoint.replay = execution.input.encode()?;
        let ordinal = u32::try_from(execution.input.events.len()).map_err(|_| IdleVisitError::State)?;
        let visit = execution.input.active_visit(checkpoint.instance)?;
        ensure_capacity(&boundary, &visit, execution.input.events.len())?;
        batch(expected, SceneOperationKind::AdvanceVisit, ordinal, checkpoint.instance,
            Some(checkpoint), visit, None)
    }

    /// Complete only a stored prefix that already reaches Finished. The last ACK may have
    /// committed before a crash; recovery needs no new client input to commit these effects.
    pub(crate) fn complete_scene_job(&self, def: &SceneDef, expected: &SceneState)
        -> Result<SceneBatchJob, IdleVisitError> {
        let execution = self.load_execution(def, expected)?;
        let (boundary, after_story, after_gil, effect) = audit(&execution)?;
        if boundary != ActiveVisitBoundary::Complete { return Err(IdleVisitError::Result); }
        let mut next = next_state(expected, &execution.scratch, &effect, after_story, after_gil)?;
        let visit = next.field_visit.as_mut().ok_or(IdleVisitError::State)?;
        visit.active_continuation = None;
        let instance = expected.checkpoint.as_ref().ok_or(IdleVisitError::State)?.instance;
        batch(expected, SceneOperationKind::CompleteVisit, 0, instance, None,
            visit.clone(), Some((next.pair.story.blob, after_gil)))
    }

    fn start_execution(&self, def: &SceneDef, original: &SceneState, input: Input, seed: u32)
        -> Result<Execution, IdleVisitError> {
        let (story, scratch, gil) = self.predecessor(original)?;
        if def.game != "ff7" || def.field != self.key || def.setup != self.init
            || !def.offered_to(&story) || def.prepare_story(&story).map_err(|_| IdleVisitError::State)? != story {
            return Err(IdleVisitError::Source);
        }
        let bound = match (def.origin_actor(), input.origin) {
            (Some(_), Some(origin)) => {
                origin.validate().map_err(|_| IdleVisitError::Source)?;
                with_trigger_position(self.field, def, origin.pos, origin.facing, origin.triangle)?
            }
            (None, None) => def.clone(),
            _ => return Err(IdleVisitError::Source),
        };
        let limits = Limits { windows: MAX_SCENE_REPLAY_EVENTS, ..Limits::default() };
        let play = ScenePlay::with_scratch_and_limits(self.field, &bound, &story, seed,
            SceneResources { gil: Some(gil) }, &scratch, limits)?;
        Ok(Execution { input, play, story, scratch, gil })
    }

    fn binding(&self, def: &SceneDef, instance: [u8; 32], execution: &Execution) -> Result<[u8; 32], IdleVisitError> {
        let identity = execution.play.replay_identity();
        let limits = Limits { windows: MAX_SCENE_REPLAY_EVENTS, ..Limits::default() };
        let bytes = serde_json::to_vec(&(self.identity, execution.input.account, instance,
            &def.id, identity.version, identity.fingerprint, identity.seed,
            (limits.steps, limits.windows, limits.threads, limits.frames), execution.story.encode(),
            execution.gil, &execution.input.visit, execution.input.origin)).map_err(|_| IdleVisitError::State)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"fmv-ff7/active-visit-input\0v1\0");
        hash.update(&bytes);
        Ok(*hash.finalize().as_bytes())
    }

    fn load_execution(&self, def: &SceneDef, expected: &SceneState) -> Result<Execution, IdleVisitError> {
        let checkpoint = expected.checkpoint.as_ref().ok_or(IdleVisitError::State)?;
        if expected.reservation.is_some() || checkpoint.instance == [0; 32]
            || checkpoint.scene_key != def.id || checkpoint.replay_version != REPLAY_VERSION
            || checkpoint.consumed_battles != 0 || checkpoint.phase != (ScenePhase::Resume { segment: 0 })
            || checkpoint.original_story != expected.pair.story.blob
            || expected.pair.character.as_ref().is_none_or(|record| checkpoint.original_gil != record.gil) {
            return Err(IdleVisitError::State);
        }
        let input = Input::decode(&checkpoint.replay)?;
        if input.account != expected.pair.story.account_id
            || expected.field_visit.as_ref() != Some(&input.active_visit(checkpoint.instance)?) {
            return Err(IdleVisitError::State);
        }
        let mut original = expected.clone();
        original.checkpoint = None;
        original.field_visit = Some(input.visit.clone());
        let mut execution = self.start_execution(def, &original, input, checkpoint.seed)?;
        if self.binding(def, checkpoint.instance, &execution)? != checkpoint.source_hash {
            return Err(IdleVisitError::SourceChanged);
        }
        replay(&mut execution)?;
        let boundary = audit(&execution)?.0;
        let visit = expected.field_visit.as_ref().ok_or(IdleVisitError::State)?;
        ensure_capacity(&boundary, visit, execution.input.events.len())?;
        Ok(execution)
    }
}

/// Keep room for every still-permitted ACK and completion. A final ACK may persist
/// Finished, but must never use the last revision or leave a window beyond the input cap.
fn ensure_capacity(boundary: &ActiveVisitBoundary, visit: &FieldVisitV1, events: usize) -> Result<(), IdleVisitError> {
    let remaining = match boundary {
        ActiveVisitBoundary::Window { .. } => {
            let remaining = MAX_SCENE_REPLAY_EVENTS.checked_sub(events).filter(|n| *n > 0)
                .ok_or(IdleVisitError::Result)?;
            remaining as u64 + 1
        }
        ActiveVisitBoundary::Complete => 1,
    };
    visit.revision.checked_add(remaining).ok_or(IdleVisitError::State)?;
    Ok(())
}

fn replay(execution: &mut Execution) -> Result<(), IdleVisitError> {
    SceneReplay { source: execution.play.replay_identity().fingerprint,
        origin: execution.input.origin, events: execution.input.events.clone() }
        .validate().map_err(|_| IdleVisitError::Result)?;
    execution.play.next_strict(None)?;
    audit(execution)?;
    for index in 0..execution.input.events.len() {
        accept(&mut execution.play, &execution.input.events[index])?;
        audit(execution)?;
    }
    Ok(())
}

fn accept(play: &mut ScenePlay, event: &SceneReplayEvent) -> Result<(), IdleVisitError> {
    let (Some(SceneBoundary::Window(mark)), SceneReplayEvent::Window { step, identity, answer }) = (play.boundary(), event)
        else { return Err(IdleVisitError::Result); };
    if mark.ordinal.checked_sub(1) != Some(*step) || *step >= MAX_SCENE_STEPS || mark.identity_bytes() != *identity {
        return Err(IdleVisitError::Result);
    }
    play.next_strict(Some(WindowAck { checkpoint: mark, answer: *answer }))?;
    Ok(())
}

fn audit(execution: &Execution) -> Result<(ActiveVisitBoundary, StoryState, u32, FieldScratchOutcome), IdleVisitError> {
    let snapshot = execution.play.snapshot().ok_or(IdleVisitError::Result)?;
    let boundary = match snapshot.boundary {
        SceneBoundary::Window(mark) => {
            let step = mark.ordinal.checked_sub(1).ok_or(IdleVisitError::Result)?;
            if step >= MAX_SCENE_STEPS { return Err(IdleVisitError::Result); }
            ActiveVisitBoundary::Window { step, identity: mark.identity_bytes(), choices: mark.choices }
        }
        SceneBoundary::End(End::Finished) => ActiveVisitBoundary::Complete,
        _ => return Err(IdleVisitError::Result),
    };
    let outcome = Outcome {
        end: match snapshot.boundary { SceneBoundary::End(end) => end, _ => End::Battle },
        windows: snapshot.stats.windows, questions: 0, writes: snapshot.writes, jump: snapshot.jump,
        setup_stats: snapshot.setup_stats, unknown_persistent_reads: snapshot.unknown_persistent_reads,
        party: snapshot.party, resources: snapshot.resources, stats: snapshot.stats, scratch: snapshot.scratch,
    };
    let (story, gil, effect) = checked_effect(&outcome, &execution.story, execution.gil)?;
    let mut scratch = execution.scratch.clone();
    effect.apply_to(&mut scratch, &execution.scratch)?;
    Ok((boundary, story, gil, effect))
}

#[allow(clippy::too_many_arguments)]
fn batch(expected: &SceneState, kind: SceneOperationKind, ordinal: u32, instance: [u8; 32],
    checkpoint: Option<SceneCheckpoint>, visit: FieldVisitV1, result: Option<(Vec<u8>, u32)>)
    -> Result<SceneBatchJob, IdleVisitError> {
    let (next_story, next_gil) = result.map_or((None, None), |(story, gil)| (Some(story), Some(gil)));
    let job = SceneBatchJob { operation: SceneOperation { kind, instance, ordinal, attempt: 0 },
        members: vec![SceneBatchMember { expected: expected.clone(), next_story, next_gil,
            checkpoint, reservation: None, battle: None, next_visit: Some(visit) }] };
    job.validate()?;
    Ok(job)
}
