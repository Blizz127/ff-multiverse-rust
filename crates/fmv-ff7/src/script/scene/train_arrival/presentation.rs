//! Bounded actual presentation phases, bound to a complete checked train-entry record.
//!
//! A decoded checkpoint is untrusted cosmetic data. Only `validate_train_presentation`
//! replays the supplied server-owned entry record and validates every terminal descriptor
//! through the same pure reducer used for live playback. Recorded phases are retained;
//! this does not authenticate historical wall-clock time or derive gameplay from the picture.

use crate::field::FieldFile;
use crate::script::dialogue::End;
use super::{TrainArrival, train_arrival_ready};
use super::codec::{encode_train_arrival, replay_train_arrival_record, MAX_TRAIN_ARRIVAL_RECORD_BYTES};
use super::super::{SceneInput, ScenePlay, Stage, Then};
use super::super::presentation::{self as reducer, Cast, CastCheckpoint, MAX_CAST_CHECKPOINT_BYTES};

/// Bind this exact cosmetic validation profile into the authoritative entry operation.
/// Wire v1 is retained; validation profile v3 also requires immutable original RNG metadata
/// to survive the active checkpoint in the separately stored `TrainPresentationEntry`.
pub const TRAIN_PRESENTATION_PROFILE: &[u8] = b"fmv-ff7/train-ready-presentation-validation\0v3\0";

mod entry;
pub use entry::{TrainPresentationEntry, MAX_TRAIN_PRESENTATION_ENTRY_BYTES};

const MAGIC: &[u8; 8] = b"FMVTRVIS";
const VERSION: u16 = 1;
const DOMAIN: &[u8] = b"fmv-client/train-ready-presentation\0v1\0";
const INPUT_DOMAIN: &[u8] = b"fmv-client/train-ready-original-input\0v1\0";
/// Exact conservative maximum; the complete original story is stored in entry_record once.
pub const MAX_TRAIN_PRESENTATION_BYTES: usize = MAX_CAST_CHECKPOINT_BYTES + 84;

/// Parsed and bounded, but not source-validated. Constructing this never mints a ready visit.
#[derive(Debug, Clone)]
pub struct TrainPresentationCheckpoint {
    entry_record: Vec<u8>,
    initial_shake_rng: [u8; 2],
    elapsed_frames: f32,
    checkpoint: CastCheckpoint,
}

impl TrainPresentationCheckpoint {
    pub fn new(entry_record: &[u8], initial_shake_rng: [u8; 2], elapsed_frames: f32,
        checkpoint: CastCheckpoint) -> Result<Self, String> {
        valid_elapsed(elapsed_frames)?;
        if entry_record.is_empty() || entry_record.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES {
            return Err("train original input record exceeds its bound".into());
        }
        Ok(Self { entry_record: entry_record.to_vec(), initial_shake_rng, elapsed_frames, checkpoint })
    }
    pub fn initial_rng(&self) -> [u8; 2] { self.initial_shake_rng }
    pub fn elapsed_frames(&self) -> f32 { self.elapsed_frames }
    pub fn checkpoint(&self) -> &CastCheckpoint { &self.checkpoint }
    pub fn encode(&self) -> Vec<u8> {
        let cast = self.checkpoint.encode();
        let mut out = Vec::with_capacity(cast.len() + 84);
        out.extend(MAGIC); out.extend(VERSION.to_le_bytes());
        out.extend(hash(INPUT_DOMAIN, &self.entry_record));
        out.extend(self.initial_shake_rng); out.extend(self.elapsed_frames.to_bits().to_le_bytes());
        // CastCheckpoint's closed representation is bounded by 7,630 bytes.
        out.extend((cast.len() as u32).to_le_bytes());
        out.extend(cast);
        out.extend(hash(DOMAIN, &out));
        out
    }
    pub fn decode_bound(entry_record: &[u8], expected_rng: [u8; 2], bytes: &[u8]) -> Result<Self, String> {
        let (initial_shake_rng, elapsed_frames, checkpoint) = decode(entry_record, expected_rng, bytes)?;
        Self::new(entry_record, initial_shake_rng, elapsed_frames, checkpoint)
    }
}

/// Source-validated cosmetic evidence. Gameplay state still comes exclusively from the
/// server's independent checked arrival, never from this cast or its serialized coordinates.
#[derive(Debug, Clone)]
pub struct ValidatedTrainPresentation {
    bytes: Vec<u8>,
    cast: Cast,
    initial_rng: [u8; 2],
    elapsed_frames: f32,
}

impl ValidatedTrainPresentation {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub fn cast(&self) -> &Cast { &self.cast }
    pub fn initial_rng(&self) -> [u8; 2] { self.initial_rng }
    pub fn elapsed_frames(&self) -> f32 { self.elapsed_frames }
}

/// Validate a client-captured picture against the complete entry record and initial effect
/// stream computed/retained by the host. The host must establish entry-record provenance.
/// The replay is a structural reference only: its no-dwell phases are never substituted for
/// the actual supplied phases. Sound/dialogue/state commands are not delivered by this path.
pub fn validate_train_presentation(field: &FieldFile, expected_entry_record: &[u8],
    expected_initial_rng: [u8; 2], bytes: &[u8]) -> Result<ValidatedTrainPresentation, String> {
    let saved = TrainPresentationCheckpoint::decode_bound(expected_entry_record, expected_initial_rng, bytes)?;
    let expected = replay_shape(field, expected_entry_record, expected_initial_rng)?;
    let cast = saved.checkpoint.restore_against(&expected)?;
    let canonical = saved.encode();
    if canonical != bytes { return Err("train presentation is not canonical".into()); }
    Ok(ValidatedTrainPresentation { bytes: canonical, cast,
        initial_rng: saved.initial_shake_rng, elapsed_frames: saved.elapsed_frames })
}

/// Execute the exact accepted inputs and stage events through the shared live reducer.
/// This derives source-fixed descriptors and background history, not historical display time.
fn replay_shape(field: &FieldFile, record: &[u8], initial_rng: [u8; 2]) -> Result<Cast, String> {
    let verified = replay_train_arrival_record(field, record).map_err(|e| e.to_string())?;
    let entry = verified.train_entry().ok_or("train entry metadata is missing")?;
    let mut play = TrainArrival::new(field, entry.original_story(), verified.replay_identity().seed,
        entry.resources(), entry.landing()).map_err(|e| e.to_string())?.into_play();
    let mut cast = Cast::new(play.def().cast.iter().map(|m| m.entity));
    cast.seed_shake_rng(initial_rng);
    for member in &play.def().cast {
        if let Some((slot, clip)) = play.initial_default_animation_clip(member.entity) {
            cast.remember_default(member.entity, slot, clip);
        }
    }
    let mut shake_config = [None, None];
    for event in play.initial_stage_events() {
        check_effect_profile(event, &mut shake_config)?;
        apply(&mut cast, &play, event);
    }
    if cast.busy() { return Err("train initializer left presentation motion".into()); }
    let mut answer = None;
    for step in 0..=2 {
        let beat = play.next_strict(answer).map_err(|e| e.to_string())?;
        let boundary_at = match &beat.then { Then::Window(w) => w.at, Then::End { at, .. } => *at };
        // Match ScenePlayer::due_at: a beat includes all its staged events before exposing
        // the boundary, even if a later event extends beyond the nominal window time.
        let end_at = boundary_at.max(beat.stage.last().map_or(0, |event| event.at));
        let mut clock = 0u32;
        for event in &beat.stage {
            if event.at < clock || event.at > end_at { return Err("train stage events are not ordered within their boundary".into()); }
            cast.advance((event.at - clock) as f32);
            clock = event.at;
            check_effect_profile(&event.what, &mut shake_config)?;
            apply(&mut cast, &play, &event.what);
        }
        cast.advance((end_at - clock) as f32);
        match beat.then {
            Then::Window(_) => {
                let Some(SceneInput::Window(ack)) = verified.transcript().get(step) else {
                    return Err("train presentation reached an unexpected window".into());
                };
                answer = Some(*ack);
            }
            Then::End { how: End::Finished, .. } if step == 2 => {
                train_arrival_ready(&play).map_err(|e| e.to_string())?;
                if encode_train_arrival(&play).map_err(|e| e.to_string())? != record {
                    return Err("train presentation replay changed the complete entry record".into());
                }
                let counts = cast.counts();
                if counts.anims_skipped != 0 || counts.moves_skipped != 0 || counts.off_cast != 0 || cast.busy() {
                    return Err("train presentation replay is unsupported or unfinished".into());
                }
                cast.checkpoint()?;
                return Ok(cast);
            }
            Then::End { .. } => return Err("train presentation reached an unsupported end".into()),
        }
    }
    Err("train presentation did not reach its ready boundary".into())
}

fn apply(cast: &mut Cast, play: &ScenePlay, event: &Stage) {
    let clip = reducer::animation_slot(event).and_then(|(entity, slot)| play.animation_clip(entity, slot));
    // Ordinary MOVE continuity changes only a transient movement phase; every movement must
    // finish before ready, and the shared stage discards its movement animation at arrival.
    reducer::apply_stage(cast, event, 0.0, 0.0, clip,
        &|x, y, triangle| play.ground().height(x, y, triangle, None));
}

fn check_effect_profile(event: &Stage, shake: &mut [Option<(u8, u8)>; 2]) -> Result<(), String> {
    match event {
        Stage::Fade(_) => return Err("train presentation profile does not support authored fades".into()),
        Stage::Shake { mask, x_amplitude, x_steps, y_amplitude, y_steps } => {
            for (index, (slot, config)) in shake.iter_mut().zip([(*x_amplitude, *x_steps), (*y_amplitude, *y_steps)]).enumerate() {
                if mask & (1 << index) == 0 { continue; }
                let (_, steps) = config;
                // The reviewed train keeps each enabled axis's amplitude and duration constant.
                // A changed source needs a new validator; endpoint bounds cannot assume this generally.
                if steps == 0 || slot.is_some_and(|old| old != config) {
                    return Err("train presentation changes its authored shake configuration".into());
                }
                *slot = Some(config);
            }
        }
        _ => {},
    }
    Ok(())
}

fn valid_elapsed(value: f32) -> Result<(), String> {
    // A terminal clock is nonnegative finite data, never a duration to execute. Retain exact
    // bits: fractional live updates need not sum to replay's event-sized additions.
    if !value.is_finite() || value.is_sign_negative() { Err("train presentation has an invalid elapsed clock".into()) } else { Ok(()) }
}

fn hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new(); hash.update(domain); hash.update(bytes); *hash.finalize().as_bytes()
}

fn decode(entry_record: &[u8], expected_rng: [u8; 2], bytes: &[u8]) -> Result<([u8; 2], f32, CastCheckpoint), String> {
    if entry_record.is_empty() || entry_record.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES {
        return Err("train original input record exceeds its bound".into());
    }
    if bytes.len() < 84 || bytes.len() > MAX_TRAIN_PRESENTATION_BYTES { return Err("invalid train presentation length".into()); }
    let (body, checksum) = bytes.split_at(bytes.len() - 32);
    if hash(DOMAIN, body) != checksum { return Err("train presentation checksum differs".into()); }
    let mut input = Input(body);
    if input.take(8)? != MAGIC || u16::from_le_bytes(input.array()?) != VERSION { return Err("unsupported train presentation encoding".into()); }
    if input.array::<32>()? != hash(INPUT_DOMAIN, entry_record) { return Err("train presentation belongs to another original input record".into()); }
    let initial_shake_rng = input.array()?;
    if initial_shake_rng != expected_rng { return Err("train presentation has another initial shake stream".into()); }
    let elapsed_frames = f32::from_bits(u32::from_le_bytes(input.array()?));
    valid_elapsed(elapsed_frames)?;
    let len = usize::try_from(u32::from_le_bytes(input.array()?)).map_err(|_| "train cast length exceeds this platform")?;
    let checkpoint = CastCheckpoint::decode(input.take(len)?)?;
    if !input.0.is_empty() { return Err("train presentation has trailing data".into()); }
    Ok((initial_shake_rng, elapsed_frames, checkpoint))
}

struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let (taken, rest) = self.0.split_at_checked(len).ok_or("truncated train presentation")?;
        self.0 = rest; Ok(taken)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?.try_into().map_err(|_| "invalid train presentation field".into())
    }
}


#[cfg(test)]
mod tests;
