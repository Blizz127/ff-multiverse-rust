//! Passive, exact presentation of a completed checked train entry.
//!
//! The original input record is separate from this bounded presentation checkpoint. Replay
//! verifies the source, accepted windows and terminal actor structure; the checkpoint retains
//! the actual local clocks, including time spent reading dialogue. A checksum binds bytes,
//! not authority: the containing visit transaction must establish the record's provenance.
//! No admission path installs this object or turns it into a live ScenePlayer.
//! Retained-only items stay compiled while runtime admission is gated; their narrow
//! non-test dead-code allowances leave the shared live presentation view fully checked.

use super::*;
use crate::scene::stage::CastCheckpoint;
use fmv_ff7::background::BackgroundOp;
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::scene::{train_scene, SceneInput, TrainArrival};
use fmv_ff7::script::scene::train_arrival::codec::{replay_train_arrival_record, MAX_TRAIN_ARRIVAL_RECORD_BYTES};

#[cfg_attr(not(test), allow(dead_code))]
const MAGIC: &[u8; 8] = b"FMVTRVIS";
#[cfg_attr(not(test), allow(dead_code))]
const VERSION: u16 = 1;
#[cfg_attr(not(test), allow(dead_code))]
const DOMAIN: &[u8] = b"fmv-client/train-ready-presentation\0v1\0";
#[cfg_attr(not(test), allow(dead_code))]
const INPUT_DOMAIN: &[u8] = b"fmv-client/train-ready-original-input\0v1\0";
/// Exact conservative maximum; the complete original story is stored in entry_record once.
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_TRAIN_PRESENTATION_BYTES: usize = crate::scene::stage::MAX_CAST_CHECKPOINT_BYTES + 84;

/// Rendering borrows the same stage for live scenes and passive terminal pictures. This
/// view intentionally has no dialogue, participant visibility, key, command or sound API.
#[derive(Clone, Copy)]
pub struct PresentationRef<'a> {
    def: &'a SceneDef,
    cast: &'a Cast,
    elapsed_frames: f32,
    run: SceneRunId,
}

impl<'a> PresentationRef<'a> {
    pub fn def(self) -> &'a SceneDef { self.def }
    pub fn run_id(self) -> SceneRunId { self.run }
    pub fn camera_clock(self) -> crate::scene::stage::CameraClock { self.cast.camera_clock() }
    pub fn shake(self) -> [f32; 2] { self.cast.shake() }
    pub fn fade(self) -> Option<fmv_ff7::script::scene::FadeColor> { self.cast.fade() }
    pub fn cast_draws(self) -> Vec<CastDraw<'a>> {
        self.cast.actors().filter(|actor| actor.shown()).filter_map(|actor| {
            let member = self.def.member(actor.entity())?;
            let [x, y, z] = actor.pos()?;
            let [rx, ry, rz] = actor.render_pos()?;
            Some(CastDraw {
                entity: actor.entity(), look: &member.look,
                pos: Pos::new(x, y, z), render_pos: Pos::new(rx, ry, rz),
                facing: actor.heading(), moving: actor.moving(),
                frame: if actor.moving() { actor.walked_frames() } else { self.elapsed_frames },
                clip: actor.animation(), return_clip: actor.full_once_return(),
            })
        }).collect()
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct TrainReadyPresentation {
    def: SceneDef,
    cast: Cast,
    elapsed_frames: f32,
    initial_shake_rng: [u8; 2],
    entry_record: Vec<u8>,
}

impl ScenePlayer {
    pub fn presentation(&self) -> PresentationRef<'_> {
        PresentationRef { def: &self.def, cast: &self.cast, elapsed_frames: self.elapsed_frames, run: self.run.id }
    }

    /// Capture the actual current held terminal picture, including any dialogue and
    /// acknowledgement delay already displayed. Calling this does not drain or emit work.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn capture_train_ready(&self) -> Result<TrainReadyPresentation, String> {
        let unsupported = self.cast.counts();
        if self.def != train_scene() || !self.continuation || self.instant || !self.played_out
            || self.over || self.failed || self.skipped || self.panel.is_some() || self.battle.is_some()
            || !self.due.is_empty() || self.source.boundary() != Some(SceneBoundary::End(End::Finished))
            || unsupported.anims_skipped != 0 || unsupported.moves_skipped != 0 || unsupported.off_cast != 0 {
            return Err("the checked train arrival is not held at its complete ready boundary".into());
        }
        valid_elapsed(self.elapsed_frames)?;
        self.cast.checkpoint()?;
        let entry_record = self.source.train_arrival_record()?;
        Ok(TrainReadyPresentation {
            def: self.def.clone(), cast: self.cast.clone(), elapsed_frames: self.elapsed_frames,
            initial_shake_rng: self.initial_shake_rng, entry_record,
        })
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl TrainReadyPresentation {
    pub fn presentation(&self) -> PresentationRef<'_> {
        PresentationRef { def: &self.def, cast: &self.cast, elapsed_frames: self.elapsed_frames, run: SceneRunId(0) }
    }
    pub fn entry_record(&self) -> &[u8] { &self.entry_record }
    pub fn background_ops(&self) -> &[BackgroundOp] { self.cast.background_ops() }

    /// Explicit displayed time only. Offline time is never implicitly applied, and this
    /// passive picture has no script, actor movement or new background events to execute.
    pub fn advance_effects(&mut self, seconds: f32) {
        let frames = seconds * FRAMES_PER_SECOND as f32;
        if frames.is_finite() && frames > 0.0 { self.cast.advance_effects(frames); }
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        valid_elapsed(self.elapsed_frames)?;
        if self.entry_record.is_empty() || self.entry_record.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES {
            return Err("train original input record exceeds its bound".into());
        }
        let cast = self.cast.checkpoint()?.encode();
        let mut out = Vec::with_capacity(cast.len() + 84);
        out.extend(MAGIC); out.extend(VERSION.to_le_bytes());
        out.extend(hash(INPUT_DOMAIN, &self.entry_record));
        out.extend(self.initial_shake_rng); out.extend(self.elapsed_frames.to_bits().to_le_bytes());
        out.extend(u32::try_from(cast.len()).map_err(|_| "train presentation exceeds its length encoding")?.to_le_bytes());
        out.extend(cast);
        out.extend(hash(DOMAIN, &out));
        if out.len() > MAX_TRAIN_PRESENTATION_BYTES { return Err("train presentation exceeds its bound".into()); }
        Ok(out)
    }

    /// Validate the complete original entry and replay all timed stage events silently, then
    /// restore the actual recorded phases. Restored bytes never provide gameplay outcomes,
    /// actor placement, background history or a replacement for missing legacy timing.
    pub fn reconstruct(field: &FieldFile, entry_record: &[u8], bytes: &[u8]) -> Result<Self, String> {
        let (initial_shake_rng, elapsed_frames, checkpoint) = decode(entry_record, bytes)?;
        let verified = replay_train_arrival_record(field, entry_record).map_err(|e| e.to_string())?;
        let entry = verified.train_entry().ok_or("train entry metadata is missing")?;
        let fresh = TrainArrival::new(field, entry.original_story(), verified.replay_identity().seed,
            entry.resources(), entry.landing()).map_err(|e| e.to_string())?.into_play();
        let def = fresh.def().clone();
        let events = verified.transcript().iter().enumerate().map(|(step, input)| {
            let SceneInput::Window(ack) = input else { return Err("train transcript contains a non-window input".to_string()) };
            Ok(SceneReplayEvent::Window {
                step: u32::try_from(step).map_err(|_| "train transcript exceeds step encoding")?,
                identity: ack.checkpoint.identity_bytes(), answer: ack.answer,
            })
        }).collect::<Result<Vec<_>, String>>()?;
        let replay = SceneReplay { source: verified.replay_identity().fingerprint, origin: None, events };
        let player = ScenePlayer::replay_silent(
            Run { id: SceneRunId(0), participants: Vec::new(), role: SceneRole::Owner },
            def, Box::new(fresh), &replay, initial_shake_rng)?;
        let replayed = player.capture_train_ready()?;
        if replayed.entry_record != entry_record || !player.outbox.is_empty() || !player.events.is_empty() || !player.sounds.is_empty() {
            return Err("train reconstruction changed its source record or emitted live work".into());
        }
        let cast = checkpoint.restore_against(&replayed.cast)?;
        Ok(Self { def: replayed.def, cast, elapsed_frames, initial_shake_rng, entry_record: entry_record.to_vec() })
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn valid_elapsed(value: f32) -> Result<(), String> {
    // A terminal clock is nonnegative finite data, never a duration to execute. Retain exact
    // bits: fractional live updates need not sum to replay's event-sized additions.
    if !value.is_finite() || value.is_sign_negative() { Err("train presentation has an invalid elapsed clock".into()) } else { Ok(()) }
}

#[cfg_attr(not(test), allow(dead_code))]
fn hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new(); hash.update(domain); hash.update(bytes); *hash.finalize().as_bytes()
}

#[cfg_attr(not(test), allow(dead_code))]
fn decode(entry_record: &[u8], bytes: &[u8]) -> Result<([u8; 2], f32, CastCheckpoint), String> {
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
    let elapsed_frames = f32::from_bits(u32::from_le_bytes(input.array()?));
    valid_elapsed(elapsed_frames)?;
    let len = usize::try_from(u32::from_le_bytes(input.array()?)).map_err(|_| "train cast length exceeds this platform")?;
    let checkpoint = CastCheckpoint::decode(input.take(len)?)?;
    if !input.0.is_empty() { return Err("train presentation has trailing data".into()); }
    Ok((initial_shake_rng, elapsed_frames, checkpoint))
}

#[cfg_attr(not(test), allow(dead_code))]
struct Input<'a>(&'a [u8]);
#[cfg_attr(not(test), allow(dead_code))]
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
#[path = "../train_ready_tests.rs"]
mod tests;
