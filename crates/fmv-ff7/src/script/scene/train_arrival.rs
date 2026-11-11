//! Checked, explicit entry into the original passenger-train arrival (48 -> 51).
//!
//! This is a narrow execution profile, not permission to enter a live field. The host must
//! establish the incoming MAPJUMP and bind its operation to the saved predecessor. The
//! constructor never derives an entry from moment 51 or upgrades unknown persistent flags.
//! Quiet initialization is safe here only because every reviewed initializer is finite,
//! takes zero scene time and has no requested work, RNG, animation, camera, fade or shake.
//! Music/AKAO setup diagnostics remain visible; this module does not restore audible history
//! or the independent client effect clock during human-held dialogue.

use super::{train_scene, Limits, Look, ReplayIdentity, SceneBoundary, SceneDef, SceneError,
    SceneInput, SceneJump, ScenePlay, SceneResources, Stats, REPLAY_VERSION, SKIPPED_STATE_OPS};
use crate::field::FieldFile;
use crate::field_direction_to_facing;
use crate::script::dialogue::End;
use crate::script::{triangle_contains_xy, FieldScratch, FieldScratchOutcome, FieldScript};
use crate::story::StoryState;

pub mod codec;

const PROFILE: &[u8] = b"fmv-ff7/tin_1-arrival\0v1\0same-vm-zero-time-entry\0";

/// Exact authored incoming cargo-door MAPJUMP. It is not the final control position.
pub const TRAIN_ARRIVAL_LANDING: SceneJump = SceneJump {
    field: 139, x: -2, y: 608, triangle: 29, direction: 252,
};

/// Immutable inputs and actual initialized scratch, retained on the resulting ScenePlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainArrivalEntry {
    original_story: StoryState,
    resources: SceneResources,
    landing: SceneJump,
    source_identity: [u8; 32],
    initial_scratch: FieldScratch,
    initialized_scratch: FieldScratch,
}

impl TrainArrivalEntry {
    pub(super) fn new(original_story: StoryState, resources: SceneResources, landing: SceneJump,
        source_identity: [u8; 32], initial_scratch: FieldScratch, initialized_scratch: FieldScratch) -> Self {
        Self { original_story, resources, landing, source_identity, initial_scratch, initialized_scratch }
    }
    pub fn original_story(&self) -> &StoryState { &self.original_story }
    pub fn resources(&self) -> SceneResources { self.resources }
    pub fn landing(&self) -> SceneJump { self.landing }
    pub fn source_identity(&self) -> [u8; 32] { self.source_identity }
    pub fn initial_scratch(&self) -> &FieldScratch { &self.initial_scratch }
    pub fn initialized_scratch(&self) -> &FieldScratch { &self.initialized_scratch }
}

/// A validated new-entry player. Consume it to use the ordinary SceneSource interface.
#[derive(Debug)]
pub struct TrainArrival { play: ScenePlay }

impl TrainArrival {
    pub fn new(field: &FieldFile, story: &StoryState, seed: u32, resources: SceneResources,
        landing: SceneJump) -> Result<Self, SceneError> {
        if !story.supports_extended() || story.moment != 48 || resources.gil.is_none()
            || landing != TRAIN_ARRIVAL_LANDING {
            return Err(SceneError::TrainEntry);
        }
        let def = train_scene();
        let script = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        validate_initializers(&script, &def)?;
        validate_train_sites(&script)?;
        let mesh = field.walkmesh().map_err(|e| SceneError::Walkmesh(e.to_string()))?;
        if !mesh.triangles.get(usize::try_from(landing.triangle).map_err(|_| SceneError::TrainEntry)?)
            .is_some_and(|triangle| triangle_contains_xy(triangle, i32::from(landing.x), i32::from(landing.y))) {
            return Err(SceneError::TrainEntry);
        }
        let source = source_identity(field, &script, &def);
        let play = ScenePlay::with_train_entry(field, &def, story, seed, resources, landing, source)?;
        Ok(Self { play })
    }

    pub fn play(&self) -> &ScenePlay { &self.play }
    pub fn into_play(self) -> ScenePlay { self.play }
}

/// Authored terminal placement, only returned after all foreground arrival work finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainArrivalPose {
    pub x: i16, pub y: i16, pub z: i16, pub triangle: u16, pub direction: u8,
}

/// Execution evidence with private fields; callers cannot manufacture it from an Outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainArrivalReady {
    story: StoryState,
    scratch: FieldScratchOutcome,
    pose: TrainArrivalPose,
    transcript: Vec<SceneInput>,
    identity: ReplayIdentity,
}

impl TrainArrivalReady {
    pub fn story(&self) -> &StoryState { &self.story }
    pub fn scratch(&self) -> &FieldScratchOutcome { &self.scratch }
    pub fn pose(&self) -> &TrainArrivalPose { &self.pose }
    pub fn transcript(&self) -> &[SceneInput] { &self.transcript }
    pub fn identity(&self) -> ReplayIdentity { self.identity }
}

/// Validate the complete strict execution, including its exact two acknowledged windows.
/// Reaching Cloud's UC0 or writing moment 51 alone does not produce this ready boundary.
pub fn train_arrival_ready(play: &ScenePlay) -> Result<TrainArrivalReady, SceneError> {
    let entry = play.train_entry().ok_or(SceneError::TrainReady)?;
    if play.boundary() != Some(SceneBoundary::End(End::Finished)) || play.off_cast() != 0
        || !safe_stats(play.setup_stats()) || !safe_stats(play.stats()) {
        return Err(SceneError::TrainReady);
    }
    let snapshot = play.snapshot().ok_or(SceneError::TrainReady)?;
    if snapshot.jump.is_some() || !snapshot.party.is_empty() || !snapshot.unknown_persistent_reads.is_empty()
        || snapshot.resources.unknown_gil || !snapshot.resources.gil_ops.is_empty()
        || snapshot.resources.gil_before != entry.resources.gil || snapshot.resources.gil_after != entry.resources.gil {
        return Err(SceneError::TrainReady);
    }
    if play.transcript().len() != 2 { return Err(SceneError::TrainReady); }
    for (input, (ordinal, entity, offset, dialog)) in play.transcript().iter().zip([
        (1, 17, 0x1066, 32), (2, 37, 0x2874, 39),
    ]) {
        let SceneInput::Window(ack) = input else { return Err(SceneError::TrainReady) };
        let at = ack.checkpoint;
        if ack.answer.is_some() || at.choices.is_some() || at.ordinal != ordinal || at.entity != entity
            || at.instruction_offset != Some(offset) || at.dialog != Some(dialog) {
            return Err(SceneError::TrainReady);
        }
    }
    let story = play.current_story().ok_or(SceneError::TrainReady)?;
    let mut expected = entry.original_story.clone();
    expected.moment = 51;
    if story != expected { return Err(SceneError::TrainReady); }
    let scratch = play.scratch_outcome().ok_or(SceneError::TrainReady)??;
    if scratch.before() != &entry.initial_scratch { return Err(SceneError::TrainReady); }
    let (at, heading) = play.train_ready_pose().ok_or(SceneError::TrainReady)?;
    if (at.x, at.y, at.z, at.triangle) != (2, 537, Some(-53), Some(28))
        || heading != field_direction_to_facing(0) {
        return Err(SceneError::TrainReady);
    }
    Ok(TrainArrivalReady { story, scratch,
        pose: TrainArrivalPose { x: 2, y: 537, z: -53, triangle: 28, direction: 0 },
        transcript: play.transcript().to_vec(), identity: play.replay_identity() })
}

/// Reconstruct a complete ready boundary from original inputs and accepted ACKs only.
/// The client separately consumes a fresh player's beats to reconstruct presentation.
pub fn replay_train_arrival(field: &FieldFile, story: &StoryState, resources: SceneResources,
    landing: SceneJump, identity: ReplayIdentity, transcript: &[SceneInput]) -> Result<ScenePlay, SceneError> {
    let mut play = TrainArrival::new(field, story, identity.seed, resources, landing)?.into_play();
    if play.replay_identity() != identity { return Err(SceneError::ReplayIdentity); }
    if transcript.len() != 2 { return Err(SceneError::ReplayBoundary); }
    play.next_strict(None)?;
    for input in transcript {
        let SceneInput::Window(ack) = input else { return Err(SceneError::ReplayBoundary) };
        play.next_strict(Some(*ack))?;
    }
    train_arrival_ready(&play)?;
    Ok(play)
}

pub(super) fn safe_stats(stats: &Stats) -> bool {
    !stats.setup_incomplete && stats.unknown.is_empty() && stats.unresolved == 0 && stats.dropped_requests == 0
        && !stats.ignored.keys().any(|op| SKIPPED_STATE_OPS.contains(op))
}

/// These prefixes are reviewed through their first RET, including both sides of each
/// initializer branch. Changed temporal/request operations refuse before any execution.
fn validate_initializers(script: &FieldScript<'_>, def: &SceneDef) -> Result<(), SceneError> {
    for reference in &def.setup {
        let code = script.disassemble(usize::from(reference.entity), usize::from(reference.slot))
            .map_err(|_| SceneError::TrainEntrySetup)?;
        let end = code.iter().position(|ins| ins.mnemonic == "RET").ok_or(SceneError::TrainEntrySetup)?;
        let prefix = code.get(..=end).ok_or(SceneError::TrainEntrySetup)?;
        if prefix.len() > 64 { return Err(SceneError::TrainEntrySetup); }
        for ins in prefix {
            if ins.stray_banks || !matches!(ins.mnemonic, "RET" | "BGON" | "SETBYTE" | "IFSW" | "JMPF"
                | "MPNAM" | "AKAO" | "MUSIC" | "CHAR" | "PC" | "SLIDR" | "TLKON" | "SOLID" | "VISI" | "XYZI" | "DIR") {
                return Err(SceneError::TrainEntrySetup);
            }
            if ins.is_jump() && ins.jump_target.is_none_or(|target|
                target <= ins.offset || !prefix.iter().any(|next| next.offset == target)) {
                return Err(SceneError::TrainEntrySetup);
            }
            // All reviewed initializer branches compare the moment with >=. Do not
            // allow the legacy comparator's fallback for unknown operator numbers.
            if ins.mnemonic == "IFSW" && ins.literal("op") != Some(4) {
                return Err(SceneError::TrainEntrySetup);
            }
            for operand in &ins.operands {
                let allowed = operand.bank == 0
                    || (ins.mnemonic == "IFSW" && operand.name == "left" && operand.bank == 2 && operand.value == 0)
                    || (ins.mnemonic == "SETBYTE" && operand.name == "var" && operand.bank == 5 && [0, 4, 6].contains(&operand.value));
                if !allowed { return Err(SceneError::TrainEntrySetup); }
            }
            if ins.mnemonic == "SETBYTE" {
                let address = ins.operand("var").map(|v| (v.bank, v.value));
                if !matches!(address, Some((5, 0 | 4 | 6)))
                    || ins.literal("value") != Some(if address == Some((5, 6)) { 10 } else { 0 }) {
                    return Err(SceneError::TrainEntrySetup);
                }
            }
            // Original init uses the global volume command, not an unreported one-shot.
            if ins.mnemonic == "AKAO" && (ins.literal("op") != Some(192) || ins.literal("p1") != Some(127)
                || ["p2", "p3", "p4", "p5"].iter().any(|name| ins.literal(name) != Some(0))) {
                return Err(SceneError::TrainEntrySetup);
            }
        }
    }
    Ok(())
}

fn validate_train_sites(script: &FieldScript<'_>) -> Result<(), SceneError> {
    for (entity, slot, offset, mnemonic, operands) in [
        (17, 0, 0x1066, "MESSAGE", vec![("dialog", 32)]),
        (37, 3, 0x2874, "MESSAGE", vec![("dialog", 39)]),
        (30, 3, 0x1512, "XYZI", vec![("x", 2), ("y", 537), ("z", -53), ("triangle", 28)]),
        (30, 3, 0x1537, "SETWORD", vec![("value", 51)]),
        (30, 3, 0x153C, "UC", vec![("disabled", 0)]),
    ] {
        let code = script.disassemble(entity, slot).map_err(|_| SceneError::TrainEntry)?;
        let ins = code.iter().find(|ins| ins.offset == offset).ok_or(SceneError::TrainEntry)?;
        if ins.mnemonic != mnemonic || operands.into_iter().any(|(name, value)| ins.literal(name) != Some(value)) {
            return Err(SceneError::TrainEntry);
        }
    }
    Ok(())
}

fn source_identity(field: &FieldFile, script: &FieldScript<'_>, def: &SceneDef) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    let mut bytes = |value: &[u8]| { hash.update(&(value.len() as u64).to_le_bytes()); hash.update(value); };
    bytes(PROFILE); bytes(&REPLAY_VERSION.to_le_bytes()); bytes(field.bytes());
    bytes(&super::replay_identity::fingerprint(script.bytes(), def));
    bytes(def.trigger.name.as_bytes());
    // Presentation is deliberately excluded from ordinary scene identity, but a ready
    // reconstruction profile must bind it as well as the original complete field bytes.
    bytes(&(def.cast.len() as u64).to_le_bytes());
    for member in &def.cast {
        bytes(&[member.entity]); bytes(member.name.as_bytes());
        match &member.look {
            Look::Appearance(id) => { bytes(&[0]); bytes(id.as_bytes()); }
            Look::Model { hrc, idle } => { bytes(&[1]); bytes(hrc.as_bytes()); bytes(idle.as_bytes()); }
        }
    }
    bytes(&(def.speakers.len() as u64).to_le_bytes());
    for (name, entity) in &def.speakers { bytes(name.as_bytes()); bytes(&[*entity]); }
    bytes(&(def.witness.len() as u64).to_le_bytes());
    for spot in def.witness.iter().chain(std::iter::once(&def.arrival)) {
        bytes(&spot.x.to_le_bytes()); bytes(&spot.y.to_le_bytes()); bytes(&spot.triangle.to_le_bytes()); bytes(&[spot.direction]);
    }
    bytes(&[u8::from(def.hides_participants())]);
    let limits = Limits::default();
    for limit in [limits.steps as u64, limits.windows as u64, limits.threads as u64, u64::from(limits.frames)] {
        bytes(&limit.to_le_bytes());
    }
    *hash.finalize().as_bytes()
}

#[cfg(test)]
mod tests;
