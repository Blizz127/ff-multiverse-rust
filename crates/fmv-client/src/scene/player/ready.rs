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
use fmv_ff7::background::BackgroundOp;
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::scene::train_scene;
use fmv_ff7::script::scene::train_arrival::presentation::{
    TrainPresentationCheckpoint, TrainPresentationEntry, validate_train_presentation,
};

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
        if !self.elapsed_frames.is_finite() || self.elapsed_frames.is_sign_negative() {
            return Err("train presentation has an invalid elapsed clock".into());
        }
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

    /// Store the complete original entry and its initial effect stream together. This
    /// envelope remains input data; its containing durable transaction establishes provenance.
    pub fn encode_entry(&self) -> Result<Vec<u8>, String> {
        Ok(TrainPresentationEntry::new(&self.entry_record, self.initial_shake_rng)?.encode())
    }

    /// Restore from the immutable final entry after the active checkpoint has been cleared.
    /// Initial RNG comes from that retained entry, never from the replaceable phase envelope.
    pub fn reconstruct_entry(field: &FieldFile, entry_bytes: &[u8], phase_bytes: &[u8]) -> Result<Self, String> {
        let entry = TrainPresentationEntry::decode(entry_bytes)?;
        Self::reconstruct(field, entry.record(), entry.initial_rng(), phase_bytes)
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        Ok(TrainPresentationCheckpoint::new(&self.entry_record, self.initial_shake_rng,
            self.elapsed_frames, self.cast.checkpoint()?)?.encode())
    }

    /// Restore actual phases only after shared source and immutable-input validation. The
    /// same pure stage reducer is used by the server and live playback; no historical
    /// ScenePlayer is constructed, so no dialogue, commands, sounds or global voice cues run.
    pub fn reconstruct(field: &FieldFile, entry_record: &[u8], expected_initial_rng: [u8; 2],
        bytes: &[u8]) -> Result<Self, String> {
        let validated = validate_train_presentation(field, entry_record, expected_initial_rng, bytes)?;
        Ok(Self { def: train_scene(), cast: validated.cast().clone(),
            elapsed_frames: validated.elapsed_frames(), initial_shake_rng: validated.initial_rng(),
            entry_record: entry_record.to_vec() })
    }
}
