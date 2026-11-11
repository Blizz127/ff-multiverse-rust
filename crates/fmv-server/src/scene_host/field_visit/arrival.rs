//! Staged checked train entry. A saved, source-verified cargo MAPJUMP is replaced by a
//! distinct checked checkpoint. Two exact ACKs retain the original pair; a separate seal
//! retains the validated cosmetic presentation. Completion consumes only saved inputs.
//! This module does not admit players, interpret a legacy train checkpoint as checked, or
//! manufacture the presentation clocks that elapsed while a human read the windows.

use fmv_core::{AccountId, GameId};
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{train_arrival_ready, train_scene, SceneBoundary, ScenePlay,
    SceneResources, TrainArrival, TrainArrivalPose, WindowAck, CARGO_DOOR_SCENE_ID,
    TRAIN_ARRIVAL_LANDING, TRAIN_SCENE_ID, REPLAY_VERSION};
use fmv_ff7::script::scene::train_arrival::codec::encode_train_arrival;
use fmv_ff7::script::scene::train_arrival::presentation::{validate_train_presentation,
    TrainPresentationEntry, ValidatedTrainPresentation, TRAIN_PRESENTATION_PROFILE};
use fmv_ff7::story::StoryState;
use fmv_persist::{FieldVisitEntryV1, FieldVisitV1, SceneCheckpoint, SceneOperation,
    SceneOperationKind, ScenePhase, SceneState, MAX_SCENE_REPLAY_BYTES};
use fmv_protocol::SceneReplayEvent;
use serde::{Deserialize, Serialize};

use super::{checked_effect, IdleVisitError, IdleVisitSource};
use crate::saver::{SceneBatchJob, SceneBatchMember};
use crate::scene_host::Ff7SceneHost;
use crate::world::{saved_landing_evidence, SceneBoundaryKind, SceneHost};

/// This fixed profile names the actual train init roots and checked entry reconstruction.
/// Its hash has the idle-source domain; neither the entry-source nor replay hash substitutes
/// for it. Field audio and live visit admission remain separately unimplemented.
const IDLE_PROFILE: &[u8] = b"fmv-server/tin_1-idle-after-checked-arrival\0v2\0train-entry-v1\0immutable-entry-rng-v1\0retained-presentation-v1\0";
const INPUT_VERSION: u32 = 1;
const CHECKPOINT_DOMAIN: &[u8] = b"fmv-server/checked-train-arrival\0v1\0";

pub(crate) struct CheckedTrainArrivalSource<'a> {
    field: &'a FieldFile,
    idle: IdleVisitSource<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrivalBoundary {
    Window { step: u32, identity: Vec<u8> },
    AwaitingPresentation,
    Complete,
}

/// Immutable decision plus an independently source-derived control position. Only the
/// acknowledged decision may authorize a later runtime to publish this position.
pub(crate) struct ArrivalCompletion {
    pub(crate) job: SceneBatchJob,
    pub(crate) pose: TrainArrivalPose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    version: u32,
    account: AccountId,
    cargo: SceneCheckpoint,
    entry_source: [u8; 32],
    fingerprint: [u8; 32],
    idle_source: [u8; 32],
    visit_id: [u8; 16],
    entry_operation: [u8; 16],
    initial_shake_rng: [u8; 2],
    events: Vec<SceneReplayEvent>,
    /// Sealed only after both ACKs; absence after Finished explicitly awaits capture.
    presentation: Option<Vec<u8>>,
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
        if input.version != INPUT_VERSION || input.events.len() > 2
            || (input.presentation.is_some() && input.events.len() != 2)
            || input.encode()? != bytes { return Err(IdleVisitError::State); }
        Ok(input)
    }
}

struct Execution { input: Input, play: ScenePlay, story: StoryState, gil: u32 }

impl<'a> CheckedTrainArrivalSource<'a> {
    pub(crate) fn new(field: &'a FieldFile) -> Result<Self, IdleVisitError> {
        let mut profile = IDLE_PROFILE.to_vec();
        profile.extend_from_slice(TRAIN_PRESENTATION_PROFILE);
        let idle = IdleVisitSource::new("tin_1", field, &train_scene().setup, &profile)?;
        Ok(Self { field, idle })
    }

    /// Future idle operations must use this exact reviewed factory, not a hash copied from
    /// the entry profile or an arbitrary descriptor selected to match a saved visit.
    pub(crate) fn idle_source(&self) -> &IdleVisitSource<'a> { &self.idle }

    pub(crate) fn begin_from_landing(&self, host: &Ff7SceneHost, expected: &SceneState,
        instance: [u8; 32], seed: u32, initial_shake_rng: [u8; 2])
        -> Result<SceneBatchJob, IdleVisitError> {
        let story = accepted_cargo(host, expected)?;
        let cargo = expected.checkpoint.as_ref().ok_or(IdleVisitError::State)?;
        if instance == [0; 32] || instance == cargo.instance { return Err(IdleVisitError::State); }
        let gil = expected.pair.character.as_ref().ok_or(IdleVisitError::State)?.gil;
        let mut play = TrainArrival::new(self.field, &story, seed,
            SceneResources { gil: Some(gil) }, TRAIN_ARRIVAL_LANDING)?.into_play();
        let entry_source = play.train_entry().ok_or(IdleVisitError::Result)?.source_identity();
        let input = Input { version: INPUT_VERSION, account: expected.pair.story.account_id,
            cargo: cargo.clone(), entry_source, fingerprint: play.replay_identity().fingerprint,
            idle_source: self.idle.identity(), visit_id: stable_id(b"visit", instance),
            entry_operation: stable_id(b"entry-operation", instance), initial_shake_rng,
            events: Vec::new(), presentation: None };
        play.next_strict(None)?;
        audit(&play, &story, gil)?;
        let source_hash = binding(&input, instance, &expected.pair.story.blob, gil, seed)?;
        let checkpoint = SceneCheckpoint { instance, scene_key: TRAIN_SCENE_ID.into(), source_hash,
            replay_version: REPLAY_VERSION, original_story: expected.pair.story.blob.clone(),
            original_gil: gil, seed, replay: input.encode()?, phase: ScenePhase::Resume { segment: 0 },
            consumed_battles: 0 };
        batch(expected, SceneOperationKind::BeginArrival, cargo.instance, cargo.consumed_battles,
            Some(checkpoint), None, None)
    }

    pub(crate) fn boundary(&self, host: &Ff7SceneHost, expected: &SceneState)
        -> Result<ArrivalBoundary, IdleVisitError> {
        let execution = self.load(host, expected)?;
        let boundary = audit(&execution.play, &execution.story, execution.gil)?;
        Ok(if boundary == ArrivalBoundary::AwaitingPresentation && execution.input.presentation.is_some() {
            ArrivalBoundary::Complete
        } else { boundary })
    }

    /// The exact entry record after both ACKs are durable. It contains no invented
    /// presentation clocks and cannot itself authorize completion.
    pub(crate) fn terminal_entry(&self, host: &Ff7SceneHost, expected: &SceneState)
        -> Result<(Vec<u8>, [u8; 2]), IdleVisitError> {
        let execution = self.load(host, expected)?;
        train_arrival_ready(&execution.play)?;
        let record = encode_train_arrival(&execution.play).map_err(|_| IdleVisitError::Result)?;
        Ok((record, execution.input.initial_shake_rng))
    }

    /// Each exact window is durable separately. The second ACK reaches Finished but
    /// remains held until the actual terminal presentation has been validated and sealed.
    pub(crate) fn acknowledge(&self, host: &Ff7SceneHost, expected: &SceneState,
        ack: SceneReplayEvent) -> Result<SceneBatchJob, IdleVisitError> {
        let mut execution = self.load(host, expected)?;
        if execution.input.events.len() >= 2 || execution.input.presentation.is_some() {
            return Err(IdleVisitError::Result);
        }
        accept(&mut execution.play, &ack)?;
        audit(&execution.play, &execution.story, execution.gil)?;
        execution.input.events.push(ack);
        let mut checkpoint = expected.checkpoint.clone().ok_or(IdleVisitError::State)?;
        checkpoint.replay = execution.input.encode()?;
        batch(expected, SceneOperationKind::AdvanceArrival, checkpoint.instance,
            execution.input.events.len() as u32, Some(checkpoint), None, None)
    }

    /// Seal only validated cosmetic bytes for this exact durable gameplay input and RNG.
    /// The validator is repeated here to reject a different entry or RNG. Runtime owner,
    /// run and instance fencing remains separate; cosmetic bytes are not authorization.
    pub(crate) fn seal_job(&self, host: &Ff7SceneHost, expected: &SceneState,
        presentation: &ValidatedTrainPresentation) -> Result<SceneBatchJob, IdleVisitError> {
        let mut execution = self.load(host, expected)?;
        if execution.input.presentation.is_some() { return Err(IdleVisitError::Result); }
        train_arrival_ready(&execution.play)?;
        let record = encode_train_arrival(&execution.play).map_err(|_| IdleVisitError::Result)?;
        let validated = validate_train_presentation(self.field, &record,
            execution.input.initial_shake_rng, presentation.bytes()).map_err(|_| IdleVisitError::Result)?;
        execution.input.presentation = Some(validated.bytes().to_vec());
        let mut checkpoint = expected.checkpoint.clone().ok_or(IdleVisitError::State)?;
        checkpoint.replay = execution.input.encode()?;
        batch(expected, SceneOperationKind::AdvanceArrival, checkpoint.instance, 3,
            Some(checkpoint), None, None)
    }

    /// No client-supplied outcome, ACK, entry bytes or replacement anchor is accepted here.
    /// The complete checked replay and sealed presentation must already be durable.
    pub(crate) fn complete(&self, host: &Ff7SceneHost, expected: &SceneState)
        -> Result<ArrivalCompletion, IdleVisitError> {
        let execution = self.load(host, expected)?;
        let ready = train_arrival_ready(&execution.play)?;
        let record = encode_train_arrival(&execution.play).map_err(|_| IdleVisitError::Result)?;
        let anchor = execution.input.presentation.as_ref().ok_or(IdleVisitError::Result)?;
        let visit = FieldVisitV1 { schema_version: 1, visit_id: execution.input.visit_id,
            revision: 0, field: "tin_1".into(), source_identity: self.idle.identity(),
            entry: FieldVisitEntryV1 { operation: execution.input.entry_operation,
                arrival: TrainPresentationEntry::new(&record, execution.input.initial_shake_rng)
                    .map_err(|_| IdleVisitError::Result)?.encode() },
            anchor: anchor.clone(), scratch_values: *ready.scratch().after().values(),
            scratch_known_masks: *ready.scratch().after().known_masks(), active_continuation: None };
        let instance = expected.checkpoint.as_ref().ok_or(IdleVisitError::State)?.instance;
        let job = batch(expected, SceneOperationKind::CompleteArrivalVisit, instance, 0,
            None, Some(visit), Some((ready.story().encode(), execution.gil)))?;
        Ok(ArrivalCompletion { job, pose: *ready.pose() })
    }

    fn load(&self, host: &Ff7SceneHost, expected: &SceneState) -> Result<Execution, IdleVisitError> {
        let character = expected.pair.character.as_ref().ok_or(IdleVisitError::State)?;
        let checkpoint = expected.checkpoint.as_ref().ok_or(IdleVisitError::State)?;
        if expected.field_visit.is_some() || expected.reservation.is_some()
            || checkpoint.scene_key != TRAIN_SCENE_ID || checkpoint.replay_version != REPLAY_VERSION
            || checkpoint.instance == [0; 32] || checkpoint.consumed_battles != 0
            || checkpoint.phase != (ScenePhase::Resume { segment: 0 })
            || checkpoint.original_story != expected.pair.story.blob || checkpoint.original_gil != character.gil {
            return Err(IdleVisitError::State);
        }
        let input = Input::decode(&checkpoint.replay)?;
        if input.account != expected.pair.story.account_id || input.idle_source != self.idle.identity()
            || input.visit_id != stable_id(b"visit", checkpoint.instance)
            || input.entry_operation != stable_id(b"entry-operation", checkpoint.instance)
            || input.cargo.instance == checkpoint.instance {
            return Err(IdleVisitError::SourceChanged);
        }
        let mut cargo = expected.clone();
        cargo.checkpoint = Some(input.cargo.clone());
        let story = accepted_cargo(host, &cargo)?;
        let mut play = TrainArrival::new(self.field, &story, checkpoint.seed,
            SceneResources { gil: Some(character.gil) }, TRAIN_ARRIVAL_LANDING)?.into_play();
        if play.replay_identity().fingerprint != input.fingerprint
            || play.train_entry().is_none_or(|entry| entry.source_identity() != input.entry_source)
            || binding(&input, checkpoint.instance, &checkpoint.original_story, checkpoint.original_gil,
                checkpoint.seed)? != checkpoint.source_hash { return Err(IdleVisitError::SourceChanged); }
        play.next_strict(None)?;
        audit(&play, &story, character.gil)?;
        for event in &input.events {
            accept(&mut play, event)?;
            audit(&play, &story, character.gil)?;
        }
        if let Some(bytes) = &input.presentation {
            let record = encode_train_arrival(&play).map_err(|_| IdleVisitError::Result)?;
            let validated = validate_train_presentation(self.field, &record, input.initial_shake_rng, bytes)
                .map_err(|_| IdleVisitError::Result)?;
            if validated.bytes() != bytes { return Err(IdleVisitError::Result); }
        }
        Ok(Execution { input, play, story, gil: character.gil })
    }
}

fn accepted_cargo(host: &Ff7SceneHost, state: &SceneState) -> Result<StoryState, IdleVisitError> {
    let checkpoint = state.checkpoint.as_ref().ok_or(IdleVisitError::State)?;
    let character = state.pair.character.as_ref().ok_or(IdleVisitError::State)?;
    if state.field_visit.is_some() || state.reservation.is_some()
        || state.pair.story.game != GameId::Ff7 || character.game != GameId::Ff7
        || character.account_id != state.pair.story.account_id || checkpoint.scene_key != CARGO_DOOR_SCENE_ID
        || checkpoint.instance == [0; 32] || checkpoint.consumed_battles != 0 {
        return Err(IdleVisitError::State);
    }
    let (replay, landing) = saved_landing_evidence(state, "cargoin").ok_or(IdleVisitError::State)?;
    if replay.origin.is_none() || !replay.events.is_empty() || replay.source != checkpoint.source_hash {
        return Err(IdleVisitError::SourceChanged);
    }
    let boundary = host.evaluate_continuation(CARGO_DOOR_SCENE_ID, &checkpoint.original_story,
        checkpoint.seed, checkpoint.original_gil, Some(&replay)).ok_or(IdleVisitError::SourceChanged)?;
    let SceneBoundaryKind::Complete(result) = boundary.at else { return Err(IdleVisitError::Result) };
    let travel = result.travel.ok_or(IdleVisitError::Result)?;
    let transaction = result.transaction.ok_or(IdleVisitError::Result)?;
    if boundary.source != checkpoint.source_hash || boundary.gil != character.gil
        || result.progress != state.pair.story.blob || transaction.expected_gil != checkpoint.original_gil
        || transaction.next_gil != character.gil || travel.target != landing.target
        || travel.arrival != landing.arrival || travel.arrival_facing != landing.arrival_facing
        || travel.arrival_hint != landing.arrival_hint || landing.target != "tin_1"
        || landing.arrival != fmv_core::Pos::new(-2.0, 608.0, 0.0) || landing.arrival_hint != Some(29)
        || landing.arrival_facing != Some(fmv_ff7::field_direction_to_facing(252)) {
        return Err(IdleVisitError::Result);
    }
    let story = StoryState::decode(&state.pair.story.blob).filter(StoryState::supports_extended)
        .ok_or(IdleVisitError::State)?;
    if story.encode() != state.pair.story.blob || story.moment != 48 { return Err(IdleVisitError::State); }
    Ok(story)
}

fn audit(play: &ScenePlay, story: &StoryState, gil: u32) -> Result<ArrivalBoundary, IdleVisitError> {
    let snapshot = play.snapshot().ok_or(IdleVisitError::Result)?;
    let boundary = match snapshot.boundary {
        SceneBoundary::Window(mark) if mark.choices.is_none() && (1..=2).contains(&mark.ordinal) =>
            ArrivalBoundary::Window { step: mark.ordinal - 1, identity: mark.identity_bytes() },
        SceneBoundary::End(End::Finished) => { train_arrival_ready(play)?; ArrivalBoundary::AwaitingPresentation }
        _ => return Err(IdleVisitError::Result),
    };
    let outcome = fmv_ff7::script::scene::Outcome {
        end: match snapshot.boundary { SceneBoundary::End(end) => end, _ => End::Battle },
        windows: snapshot.stats.windows, questions: 0, writes: snapshot.writes, jump: snapshot.jump,
        setup_stats: snapshot.setup_stats, unknown_persistent_reads: snapshot.unknown_persistent_reads,
        party: snapshot.party, resources: snapshot.resources, stats: snapshot.stats, scratch: snapshot.scratch,
    };
    checked_effect(&outcome, story, gil)?;
    Ok(boundary)
}

fn accept(play: &mut ScenePlay, event: &SceneReplayEvent) -> Result<(), IdleVisitError> {
    let (Some(SceneBoundary::Window(mark)), SceneReplayEvent::Window { step, identity, answer: None })
        = (play.boundary(), event) else { return Err(IdleVisitError::Result) };
    if mark.choices.is_some() || mark.ordinal.checked_sub(1) != Some(*step)
        || *step > 1 || mark.identity_bytes() != *identity { return Err(IdleVisitError::Result); }
    play.next_strict(Some(WindowAck { checkpoint: mark, answer: None }))?;
    Ok(())
}

fn stable_id(kind: &[u8], instance: [u8; 32]) -> [u8; 16] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"fmv-server/checked-train-arrival-id\0v1\0");
    hash.update(kind); hash.update(&instance);
    let mut result = [0; 16];
    for (out, byte) in result.iter_mut().zip(hash.finalize().as_bytes()) { *out = *byte; }
    result
}

fn binding(input: &Input, instance: [u8; 32], story: &[u8], gil: u32, seed: u32)
    -> Result<[u8; 32], IdleVisitError> {
    let mut original = input.clone();
    original.events.clear(); original.presentation = None;
    let mut hash = blake3::Hasher::new();
    hash.update(CHECKPOINT_DOMAIN); hash.update(&REPLAY_VERSION.to_le_bytes());
    hash.update(&instance); hash.update(&gil.to_le_bytes()); hash.update(&seed.to_le_bytes());
    let encoded = original.encode()?;
    for bytes in [story, encoded.as_slice()] {
        hash.update(&(bytes.len() as u64).to_le_bytes()); hash.update(bytes);
    }
    Ok(*hash.finalize().as_bytes())
}

fn batch(expected: &SceneState, kind: SceneOperationKind, instance: [u8; 32], ordinal: u32,
    checkpoint: Option<SceneCheckpoint>, next_visit: Option<FieldVisitV1>, result: Option<(Vec<u8>, u32)>)
    -> Result<SceneBatchJob, IdleVisitError> {
    let (next_story, next_gil) = result.map_or((None, None), |(story, gil)| (Some(story), Some(gil)));
    let job = SceneBatchJob { operation: SceneOperation { instance, ordinal, attempt: 0, kind },
        members: vec![SceneBatchMember { expected: expected.clone(), next_story, next_gil,
            checkpoint, reservation: None, battle: None, next_visit }] };
    job.validate()?;
    Ok(job)
}

#[cfg(test)]
mod tests;
