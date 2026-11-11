//! The scene player: one run of a story scene on this machine, as a state machine with no GPU and no network.
//!
//! # What it holds
//!
//! * the **source** ([`SceneSource`]): the scene as the interpreter gives it, beat by beat (stage events, then a window or
//!   the end);
//! * the **scene clock**: field frames (30 a second) since the last window opened. The stage events of a beat happen when
//!   the clock reaches their time; the beat's window opens when it reaches the window's. A window **pauses** the clock and
//!   every walk and turn (the interpreter pauses the whole scene for a window); when the window is moved past the next beat
//!   is pulled and the clock starts again from 0;
//! * the **stage** ([`Cast`]): the cast's places, turns and visibility, and the camera as the scene told it;
//! * the **window** on show ([`Panel`]), with its page and its question's cursor;
//! * what is owed to the server ([`SceneCmd`]) and what happened that the headless bot reports ([`SceneEvent`]).
//!
//! # The two roles
//!
//! The **owner** drives. Each window it moves past is reported once, in order, with the line it answered if the window
//! asked ([`SceneCmd::Step`], `step` counting from 0). A window of nothing but codes is moved past at once (it is a step
//! like any other). When the scene plays to its end the owner reports it ([`SceneCmd::End`] with `skipped: false`) and is
//! finished. Its Back key skips the whole scene ([`SceneCmd::End`] with `skipped: true`) and finishes it.
//!
//! A **watcher** opens window 0 by itself (its clock runs from the start, like the owner's) and window `k + 1` only when
//! the owner's step `k` arrives ([`ScenePlayer::on_step`]): a step that is not the next one in order is ignored, and one
//! that comes before the window it moves past has opened here waits for it. A watcher never moves past a window by itself
//! (its keys turn the pages, nothing more); when its scene has played to the end it stands where the scene left everyone
//! until the server says the run is over. Its Back key leaves ([`SceneCmd::Leave`]) and finishes it.
//!
//! In both roles [`ScenePlayer::on_ended`] (the server says the run is over) finishes the player at once, and finishing is
//! idempotent: nothing is sent twice, and nothing is sent after the player is over.
//!
//! A durable continuation pauses at an exact authored battle boundary. Its cast, camera and
//! interpreter remain alive while combat owns input. Only the server's matching durable victory
//! acknowledgement resumes it, after the battle result panel closes. Defeat holds that boundary
//! for an explicit retry. Reconnect rebuilds accepted windows and victories silently from the
//! fingerprinted original script, without sending historical commands or replaying their audio.
//!
//! # The end
//!
//! A finished player has no window and no actors and the camera is the field's again; the caller drops it and the field's
//! keys are the field's again. The player never moved the local player: it is DRAWN at its witness spot while the scene
//! runs ([`ScenePlayer::witnesses`]) and stands where it always stood.

use super::panel::{Outcome, Panel, PanelKey, SpeakerSpot};
use super::source::SceneSource;
use super::stage::Cast;
use crate::avatar::anim::ClipFrame;
use crate::ui::dialogue::{dialogue_key, DialogueKey};
use crate::mods::{Global, ModSink, VoiceCue};
use fmv_core::{EntityId, Pos};
use fmv_ff7::field_direction_to_facing;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{BattleCheckpoint, Look, MoveHow, SceneBoundary, SceneDef, Stage, Then, Timed, Window, FRAMES_PER_SECOND};
use fmv_ff7::story::StoryState;
use fmv_protocol::{BattleId, ClientMsg, SceneOutcome, SceneParticipant, SceneRefusal, SceneReplay, SceneReplayEvent, SceneRole, SceneRunId, MAX_SCENE_STEPS};
use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use winit::keyboard::KeyCode;

mod ready;
pub use ready::PresentationRef;
#[cfg(test)]
pub use ready::TrainReadyPresentation;

/// A message for the server about a scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneCmd {
    /// The owner moved past window `step`, answering `answer` when it asked.
    Step { run: SceneRunId, step: u32, answer: Option<u8> },
    /// The owner played the scene to its end (or skipped it).
    End { run: SceneRunId, skipped: bool },
    /// A watcher stops watching.
    Leave { run: SceneRunId },
    /// Debug only: put this player in the field of scene `scene` on a made-up story.
    Preview { scene: String },
    /// The authored combat boundary has reached the screen; no scene advancement is allowed yet.
    BattleReady { run: SceneRunId, ordinal: u32, step: u32 },
    /// Explicitly retry a defeated durable battle from its saved start.
    Retry { run: SceneRunId },
}

impl SceneCmd {
    /// The message that carries this command.
    pub fn into_msg(self) -> ClientMsg {
        match self {
            SceneCmd::Step { run, step, answer } => ClientMsg::SceneStep { run, step, answer },
            SceneCmd::End { run, skipped } => ClientMsg::SceneEnd { run, skipped },
            SceneCmd::Leave { run } => ClientMsg::SceneLeave { run },
            SceneCmd::Preview { scene } => ClientMsg::SceneDebugPreview { scene },
            SceneCmd::BattleReady { run, ordinal, step } => ClientMsg::SceneBattleReady { run, ordinal, step },
            SceneCmd::Retry { run } => ClientMsg::SceneRetry { run },
        }
    }
}

/// Something that happened in a scene, for the headless bot to report (ids and counts only: never a word of the game).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneEvent {
    /// The server started a run and this client built it (or is about to say it cannot).
    Started { run: SceneRunId, scene: String, role: SceneRole, participants: usize },
    /// A window was moved past (by this player's own key, or by the owner's step).
    Line { index: u32, opener: u8, speaker: Option<u8>, dialog: Option<usize>, asks: bool, answer: Option<usize> },
    /// The server ended the run, after this player had moved past `lines` windows.
    Ended { run: SceneRunId, outcome: SceneOutcome, lines: u32 },
    /// The server refused a scene request.
    Refused(SceneRefusal),
    /// This machine cannot build the scene (no game data for its field, say); the player released itself.
    Unavailable,
}

/// Local audio commands; no sound bytes or new messages cross the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneSound {
    pub run: SceneRunId,
    pub action: SoundAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundAction {
    Begin,
    Prepare(u16),
    /// A literal effect on scene-owned channel 1 or 2; id 0 stops only that channel.
    Play { channel: u8, id: u16, pan: u8 },
    End,
}

/// What a key means to a scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneKey {
    Confirm,
    Back,
    Up,
    Down,
}

impl SceneKey {
    /// The scene key a keyboard event stands for: the talk window's own table (Enter, Space and the pad's A confirm;
    /// Backspace and Esc go back, which skips the scene; the arrows and W and S move a question's pointer).
    pub fn from_event(code: KeyCode, pressed: bool, repeat: bool) -> Option<Self> {
        Some(match dialogue_key(code, pressed, repeat)? {
            DialogueKey::Confirm => SceneKey::Confirm,
            DialogueKey::Back => SceneKey::Back,
            DialogueKey::Up => SceneKey::Up,
            DialogueKey::Down => SceneKey::Down,
        })
    }
}

/// What the server said about a run.
#[derive(Debug, Clone)]
pub struct Run {
    pub id: SceneRunId,
    /// Everyone who stands in it, each on its witness spot.
    pub participants: Vec<SceneParticipant>,
    /// What this player is.
    pub role: SceneRole,
}

/// Where somebody who watches the scene stands, and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Witness {
    pub entity: EntityId,
    /// Where the witness spot is; `z` is the ground's height there, when the source knows it.
    pub pos: Pos,
    pub height_known: bool,
    pub facing: f32,
}

/// One cast member to draw.
#[derive(Debug, Clone, Copy)]
pub struct CastDraw<'a> {
    /// The script entity it plays.
    pub entity: u8,
    pub look: &'a Look,
    /// Logical walkmesh position, used by movement and the native following camera.
    pub pos: Pos,
    /// Model position after its independent world-space OFST offset.
    pub render_pos: Pos,
    pub facing: f32,
    /// Walking now (the avatar's walk clip), standing otherwise.
    pub moving: bool,
    /// Default clip frame from scene time, frozen while a window holds the scene.
    pub frame: f32,
    /// Resolved authored animation, independent of the default clip's fallback clock.
    pub clip: Option<(&'a str, ClipFrame)>,
    /// Verified return clip for a full Once; its duration comes from the loaded animation.
    pub return_clip: Option<&'a str>,
}

/// What comes after the stage events that are due.
enum Next {
    /// Ask the source for the next beat (with the answer to the window just moved past).
    Pull(Option<usize>),
    /// A window, waiting for its time.
    Window(Window),
    /// The end of the scene, waiting for its time.
    End { at: u32, how: End },
    /// Nothing: the scene has played to its end (a watcher waits here for the server).
    Held,
}

/// A resolved ordinary MOVE may continue its clip only at this exact authored boundary.
/// Keeping integer event times here avoids inferring continuity from rounded render time.
struct MoveContinuation {
    slot: u8,
    clip: String,
    target: (i16, i16),
    end_at: u32,
    phase_at_end: u32,
}

struct PausedBattle {
    checkpoint: BattleCheckpoint,
    battle: Option<BattleId>,
    /// Acknowledged resources wait until the battle's presentation is no longer on screen.
    resume: Option<(StoryState, u32)>,
    game_over: bool,
    retry_sent: bool,
}

/// One run of a scene on this machine (see the module docs).
pub struct ScenePlayer {
    run: Run,
    def: SceneDef,
    source: Box<dyn SceneSource>,
    continuation: bool,
    /// Replay reconstructs stage state without audio, window reports, or other historical effects.
    silent: bool,
    battle: Option<PausedBattle>,
    battles_resumed: u32,
    /// Scenes without reviewed spectator placement reveal participants only at control handoff.
    participants_hidden: bool,
    cast: Cast,
    /// Frames since the last window opened.
    clock: f32,
    /// Total advancing scene time across windows; render time never advances cast animation.
    elapsed_frames: f32,
    /// The stage events of the beat in hand that have not happened yet, in order.
    due: VecDeque<Timed<Stage>>,
    /// Per-actor MOVE boundaries within this beat; a window severs even an active walk's chain.
    move_continuations: BTreeMap<u8, MoveContinuation>,
    next: Next,
    panel: Option<Panel>,
    /// Windows opened so far.
    opened: u32,
    /// Windows moved past so far.
    passed: u32,
    /// A watcher: steps accepted so far (applied, or waiting for their window to open here).
    accepted: u32,
    /// A watcher: the answers of accepted steps whose window has not opened here yet.
    early: VecDeque<Option<u8>>,
    /// The scene is over for this player (it played to its end, was skipped, or the server ended it).
    over: bool,
    failed: bool,
    failure_reason: Option<&'static str>,
    skipped: bool,
    /// A watcher's scene has played to its end.
    played_out: bool,
    terminal_travel: bool,
    terminal_fade: Option<fmv_ff7::script::scene::FadeColor>,
    initial_shake_rng: [u8; 2],
    sent_end: bool,
    /// No waiting: the clock never holds a beat back (a headless bot has no picture to wait for).
    instant: bool,
    outbox: Vec<SceneCmd>,
    events: Vec<SceneEvent>,
    sounds: VecDeque<SceneSound>,
    /// Changes whenever a window opens or closes.
    epoch: u32,
    /// The screen the window was last drawn for (keys lay the text out for the same one).
    size: Cell<(u32, u32)>,
}

impl Drop for ScenePlayer {
    fn drop(&mut self) {
        if !self.silent && self.source.echo_voice() { Global.cue(VoiceCue::Stop); }
    }
}

/// Counts only: the scene is the game's and never goes to a log.
impl std::fmt::Debug for ScenePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScenePlayer")
            .field("run", &self.run.id)
            .field("role", &self.run.role)
            .field("opened", &self.opened)
            .field("passed", &self.passed)
            .field("over", &self.over)
            .finish_non_exhaustive()
    }
}

/// The table index of a canon window's text (`None` for a line this project added).
fn dialog_of(origin: &MessageOrigin) -> Option<usize> {
    match origin {
        MessageOrigin::Canon { dialog, .. } => Some(*dialog),
        MessageOrigin::Added { .. } => None,
    }
}

impl ScenePlayer {
    /// Start `run` of `def` read through `source`: the cast is put where the scene begins and the first beat is pulled.
    /// `instant` is for a bot: no clock, the first window is open on return.
    #[cfg(test)]
    pub fn new(run: Run, def: SceneDef, source: Box<dyn SceneSource>, instant: bool) -> Self {
        Self::new_seeded(run, def, source, instant, [0; 2])
    }

    pub fn new_seeded(run: Run, def: SceneDef, source: Box<dyn SceneSource>, instant: bool, shake_rng: [u8; 2]) -> Self {
        Self::new_mode(run, def, source, instant, false, false, shake_rng)
    }

    fn new_mode(run: Run, def: SceneDef, source: Box<dyn SceneSource>, instant: bool, continuation: bool, silent: bool, shake_rng: [u8; 2]) -> Self {
        let mut cast = Cast::new(def.cast.iter().map(|m| m.entity));
        cast.seed_shake_rng(shake_rng);
        for member in &def.cast {
            if let Some((slot, clip)) = source.initial_default_animation_clip(member.entity) {
                cast.remember_default(member.entity, slot, clip);
            }
        }
        for event in source.initial() {
            Self::apply_stage(&mut cast, source.as_ref(), &event, 0.0, 0.0);
        }
        cast.settle();
        let participants_hidden = def.hides_participants();
        let mut player = Self {
            run,
            def,
            source,
            continuation,
            silent,
            battle: None,
            battles_resumed: 0,
            participants_hidden,
            cast,
            clock: 0.0,
            elapsed_frames: 0.0,
            due: VecDeque::new(),
            move_continuations: BTreeMap::new(),
            next: Next::Pull(None),
            panel: None,
            opened: 0,
            passed: 0,
            accepted: 0,
            early: VecDeque::new(),
            over: false,
            failed: false,
            failure_reason: None,
            skipped: false,
            played_out: false,
            terminal_travel: false,
            terminal_fade: None,
            initial_shake_rng: shake_rng,
            sent_end: false,
            instant,
            outbox: Vec::new(),
            events: Vec::new(),
            sounds: VecDeque::new(),
            epoch: 0,
            size: Cell::new((960, 540)),
        };
        player.sound(SoundAction::Begin);
        player.settle();
        player
    }

    /// Rebuild the exact accepted prefix from original data. Historical beats update only the
    /// local stage; every boundary, choice, source hash and committed victory is checked first.
    pub fn with_replay_seeded(run: Run, def: SceneDef, source: Box<dyn SceneSource>, instant: bool, replay: &SceneReplay, shake_rng: [u8; 2]) -> Result<Self, String> {
        replay.validate().map_err(str::to_owned)?;
        if source.fingerprint() != Some(replay.source) {
            return Err("the scene replay source differs from the installed game".into());
        }
        if replay.events.is_empty() {
            return Ok(Self::new_mode(run, def, source, instant, true, false, shake_rng));
        }
        let mut player = Self::replay_silent(run, def, source, replay, shake_rng)?;
        player.silent = false;
        player.instant = instant;
        player.accepted = player.passed;
        if player.over || player.played_out {
            if player.is_owner() {
                player.outbox.push(SceneCmd::End { run: player.run.id, skipped: false });
            }
        } else if let Some(paused) = &player.battle {
            if player.is_owner() {
                player.outbox.push(SceneCmd::BattleReady { run: player.run.id, ordinal: paused.checkpoint.ordinal, step: player.passed });
            }
        } else {
            player.sound(SoundAction::Begin);
            // Only the historical prefix was silent; the current blank window is new work.
            if player.is_owner() && player.panel.as_ref().is_some_and(Panel::is_blank) {
                let panel = player.panel.take().expect("the blank current panel");
                player.pass(panel, None);
                player.settle();
            }
        }
        Ok(player)
    }

    /// Rebuild without ever activating a run. In particular an empty prefix is silent too,
    /// and reaching Finished does not queue End or play a historical sound/voice cue.
    fn replay_silent(run: Run, def: SceneDef, source: Box<dyn SceneSource>, replay: &SceneReplay, shake_rng: [u8; 2]) -> Result<Self, String> {
        replay.validate().map_err(str::to_owned)?;
        if source.fingerprint() != Some(replay.source) {
            return Err("the scene replay source differs from the installed game".into());
        }
        let mut player = Self::new_mode(run, def, source, false, true, true, shake_rng);
        for event in &replay.events {
            player.reach_boundary()?;
            match event {
                SceneReplayEvent::Window { step, identity, answer } => {
                    let Some(SceneBoundary::Window(checkpoint)) = player.source.boundary() else {
                        return Err("scene replay window does not match its boundary".into());
                    };
                    let valid_answer = match (checkpoint.choices, answer) {
                        (None, None) => true,
                        (Some([first, last]), Some(a)) => (first..=last).contains(a),
                        _ => false,
                    };
                    if *step != player.passed || checkpoint.identity_bytes() != *identity || !valid_answer {
                        return Err("scene replay window identity, order, or answer differs".into());
                    }
                    let panel = player.panel.take().ok_or("scene replay window has no local panel")?;
                    player.pass(panel, answer.map(usize::from));
                    player.settle();
                }
                SceneReplayEvent::Battle { ordinal, identity, formation, progress, gil } => {
                    let Some(paused) = player.battle.as_ref() else { return Err("scene replay victory has no pending battle".into()) };
                    let checkpoint = paused.checkpoint;
                    if checkpoint.ordinal != *ordinal || checkpoint.formation != *formation || checkpoint.identity_bytes() != *identity {
                        return Err("scene replay victory differs from its authored battle".into());
                    }
                    let story = StoryState::decode(progress).ok_or("scene replay victory has an unreadable story")?;
                    player.source.resume_battle(checkpoint, story, *gil)?;
                    player.battles_resumed = checkpoint.ordinal;
                    player.battle = None;
                    player.next = Next::Pull(None);
                    player.settle();
                }
            }
        }
        player.reach_boundary()?;
        Ok(player)
    }

    fn reach_boundary(&mut self) -> Result<(), String> {
        for _ in 0..=MAX_SCENE_STEPS {
            if self.over {
                return if !self.failed && matches!(self.source.boundary(), Some(SceneBoundary::End(End::Finished | End::MapJump { .. }))) {
                    Ok(())
                } else { Err("scene replay reached an unsupported boundary".into()) };
            }
            if self.panel.is_some() || self.battle.is_some() || self.played_out { return Ok(()); }
            let at = self.due_at();
            if !at.is_finite() { return Err("scene replay has no advancing boundary".into()); }
            self.advance_stage_to(at.max(self.clock));
            self.settle();
        }
        Err("scene replay did not reach a boundary".into())
    }

    // ---- what it is ----------------------------------------------------------------------------------------------

    pub fn run_id(&self) -> SceneRunId {
        self.run.id
    }

    pub fn is_durable(&self) -> bool { self.continuation }

    pub fn continuation_failure(&self) -> Option<&str> {
        self.failed.then_some(self.failure_reason.unwrap_or("Saved scene continuation failed validation. Reconnect with matching game data."))
    }

    pub fn is_owner(&self) -> bool {
        self.run.role == SceneRole::Owner
    }

    /// Whether the scene is over for this player: the caller drops it.
    pub fn is_over(&self) -> bool {
        self.over
    }

    /// Whether a watcher's scene has played to its end and waits for the server.
    #[cfg(test)]
    pub fn played_out(&self) -> bool {
        self.played_out
    }

    /// Windows moved past so far.
    pub fn lines(&self) -> u32 {
        self.passed
    }

    /// Changes whenever a window opens or closes (what a key armed for one window must not carry to the next).
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The window on show.
    #[cfg(test)]
    pub fn panel(&self) -> Option<&Panel> {
        self.panel.as_ref()
    }

    /// Whether a window is on show.
    #[cfg(test)]
    pub fn has_window(&self) -> bool {
        self.panel.is_some()
    }

    /// The cast member whose script entity is `entity`, as the stage has it (None once the scene is over).
    #[cfg(test)]
    pub fn actor(&self, entity: u8) -> Option<&super::stage::Actor> {
        self.cast.actor(entity)
    }

    /// What the scene told the camera (the field's own once the scene is over).
    #[cfg(test)]
    pub fn camera(&self) -> super::stage::CameraState {
        self.cast.camera()
    }

    /// What the scene told the camera, with the stage's own clock of it (see [`super::stage::CameraClock`]): the
    /// window's field camera shows it (`crate::camera`).
    #[cfg(test)]
    pub fn camera_clock(&self) -> super::stage::CameraClock {
        self.cast.camera_clock()
    }

    pub fn background_ops(&self) -> &[fmv_ff7::background::BackgroundOp] { self.cast.background_ops() }

    pub fn fade(&self) -> Option<fmv_ff7::script::scene::FadeColor> { self.presentation().fade() }

    pub fn shake(&self) -> [f32; 2] { self.presentation().shake() }

    pub fn shake_rng_indices(&self) -> [u8; 2] { self.cast.shake_rng_indices() }

    pub fn initial_shake_rng_indices(&self) -> [u8; 2] { self.initial_shake_rng }

    /// A completed authored map jump keeps its last screen colour until the field actually changes.
    pub fn handoff_fade(&self) -> Option<fmv_ff7::script::scene::FadeColor> {
        if !self.terminal_travel { return None; }
        if self.over { self.terminal_fade } else { self.cast.fade() }
    }

    /// What the stage could not show.
    #[cfg(test)]
    pub fn counts(&self) -> super::stage::Counts {
        self.cast.counts()
    }

    /// The cast entity who speaks the window on show, if there is one and the script names it.
    pub fn speaker(&self) -> Option<u8> {
        self.panel.as_ref().and_then(|p| p.window().speaker)
    }

    // ---- the stage -----------------------------------------------------------------------------------------------

    /// The cast members to draw: placed, not hidden, with the look the scene gives them.
    /// The scene's data (its cast, witness spots and the rest).
    pub fn def(&self) -> &SceneDef {
        &self.def
    }

    #[cfg(test)]
    pub fn cast_draws(&self) -> Vec<CastDraw<'_>> {
        self.presentation().cast_draws()
    }

    /// Where every participant stands while the scene runs: on the witness spot it was given, which way the spot faces. A
    /// participant whose spot the scene does not have is not listed (it keeps its real place).
    pub fn witnesses(&self) -> Vec<Witness> {
        if self.over {
            return Vec::new();
        }
        self.run
            .participants
            .iter()
            .filter_map(|p| {
                let spot = self.def.witness.get(usize::from(p.spot))?;
                let z = self.source.height_at(spot.x, spot.y, Some(spot.triangle));
                Some(Witness {
                    entity: p.entity,
                    pos: Pos::new(f32::from(spot.x), f32::from(spot.y), z.unwrap_or(0.0)),
                    height_known: z.is_some(),
                    facing: field_direction_to_facing(spot.direction),
                })
            })
            .collect()
    }

    /// A reconnect changes runtime entity ids, not the retained scene or its pending inputs.
    pub fn refresh_participants(&mut self, participants: Vec<fmv_protocol::SceneParticipant>) {
        self.run.participants = participants;
    }

    pub fn hides_participant(&self, entity: EntityId) -> bool {
        !self.over && self.participants_hidden && self.run.participants.iter().any(|p| p.entity == entity)
    }

    // ---- time ----------------------------------------------------------------------------------------------------

    /// Advance visible field time. Dialogue holds actors and script time, while fades and shake keep moving.
    /// A battle or its result panel freezes the field through `advance_visible`.
    pub fn advance(&mut self, seconds: f32) {
        if self.over || self.battle.is_some() { return; }
        let mut left = seconds * FRAMES_PER_SECOND as f32;
        if !left.is_finite() || left <= 0.0 { return; }
        if self.played_out || self.panel.is_some() {
            self.cast.advance_effects(left);
            return;
        }
        // Time is spent up to the moment the next window (or the end) is due and no further: a window pauses the stage at
        // its own moment, not at the end of the tick that carried the clock past it. (A few rounds at most: each ends at
        // a boundary or uses up the time.)
        for _ in 0..4 {
            let spend = left.min((self.due_at() - self.clock).max(0.0));
            self.advance_stage_to(self.clock + spend);
            left -= spend;
            self.settle();
            if self.over || self.battle.is_some() || left <= 0.0 { return; }
            if self.played_out || self.panel.is_some() {
                self.cast.advance_effects(left);
                return;
            }
        }
    }

    /// Advance through each event in order: a replacement move or placement must not credit
    /// the cancelled walk with time past that event. Audio still measures lateness at the tick's
    /// endpoint, so crossing several effects in one stalled frame does not replay stale cues.
    fn advance_stage_to(&mut self, target: f32) {
        while self.due.front().is_some_and(|event| event.at as f32 <= target) {
            let Some(event) = self.due.pop_front() else { break };
            self.advance_clock_to(self.clock.max(event.at as f32));
            let late = self.clock - event.at as f32;
            let sound_late = target - event.at as f32;
            self.apply_event(event, late, sound_late);
        }
        self.advance_clock_to(target);
    }

    fn advance_clock_to(&mut self, target: f32) {
        let frames = (target - self.clock).max(0.0);
        self.move_continuations.retain(|_, movement| target.is_finite() && target <= movement.end_at as f32);
        self.cast.advance(frames);
        if frames.is_finite() { self.elapsed_frames += frames; }
        self.clock = target;
    }

    /// The frame at which what is next is due: its window (or the end), and the last stage event before it.
    fn due_at(&self) -> f32 {
        let last = self.due.back().map_or(0.0, |t| t.at as f32);
        match &self.next {
            Next::Window(w) => (w.at as f32).max(last),
            Next::End { at, .. } => (*at as f32).max(last),
            Next::Pull(_) | Next::Held => f32::INFINITY,
        }
    }

    /// Apply the stage events whose time has come.
    fn apply_due(&mut self) {
        while let Some(front) = self.due.front() {
            if front.at as f32 > self.clock {
                break;
            }
            let Some(event) = self.due.pop_front() else { break };
            let late = self.clock - event.at as f32;
            self.apply_event(event, late, late);
        }
    }

    fn apply_event(&mut self, event: Timed<Stage>, motion_late: f32, sound_late: f32) {
        if let Stage::Sound { channel, id, pan } = event.what {
            // A stalled frame must not dump a burst of old effects. Stops still cancel a loop.
            if id == 0 || self.instant || sound_late <= FRAMES_PER_SECOND as f32 / 2.0 {
                self.sound(SoundAction::Play { channel, id, pan });
            }
        }
        let movement_phase = self.move_phase(&event, motion_late);
        Self::apply_stage(&mut self.cast, self.source.as_ref(), &event.what, motion_late, movement_phase);
    }

    /// Original MOVE keeps its phase when the selected slot stays the same. Support only
    /// positive, exactly adjacent ordinary MOVEs with matching authored endpoints and clips.
    fn move_phase(&mut self, event: &Timed<Stage>, late: f32) -> f32 {
        match &event.what {
            Stage::Move { entity, from, x, y, frames, how, animation, .. } => {
                // Taking the old record also invalidates overlapping/unsupported replacements.
                let previous = self.move_continuations.remove(entity);
                if *how != MoveHow::Walk || *frames == 0 || !late.is_finite() || late != 0.0 { return 0.0; }
                let Some(slot @ (1 | 2)) = *animation else { return 0.0 };
                let Some(clip) = self.source.animation_clip(*entity, slot) else { return 0.0 };
                let Some(end_at) = event.at.checked_add(*frames) else { return 0.0 };
                let phase = previous.filter(|movement| movement.end_at == event.at
                    && movement.slot == slot && movement.clip == clip && *from == Some(movement.target))
                    .map_or(0, |movement| movement.phase_at_end);
                let Some(phase_at_end) = phase.checked_add(*frames) else { return 0.0 };
                self.move_continuations.insert(*entity, MoveContinuation {
                    slot, clip: clip.to_string(), target: (*x, *y), end_at, phase_at_end,
                });
                phase as f32
            }
            // Invalidate before apply_stage's supported animation branches can return early.
            Stage::Place { entity, .. } | Stage::Anim { entity, .. } | Stage::AnimSpeed { entity, .. } => {
                self.move_continuations.remove(entity);
                0.0
            }
            _ => 0.0,
        }
    }

    fn apply_stage(cast: &mut Cast, source: &dyn SceneSource, event: &Stage, late: f32, movement_phase: f32) {
        let clip = fmv_ff7::script::scene::presentation::animation_slot(event)
            .and_then(|(entity, slot)| source.animation_clip(entity, slot));
        fmv_ff7::script::scene::presentation::apply_stage(cast, event, late, movement_phase,
            clip, &|x, y, triangle| source.height_at(x, y, triangle));
    }

    /// Go on as far as the clock allows: pull the next beat, play what is due, open the window or end the scene. Stops at
    /// an open window, at a beat whose time has not come and at the end. (A loop, bounded by the scene's own limit on
    /// windows: a source of nothing but empty windows ends instead of running away.)
    fn settle(&mut self) {
        loop {
            if self.over || self.panel.is_some() || self.battle.is_some() {
                return;
            }
            match std::mem::replace(&mut self.next, Next::Held) {
                Next::Pull(answer) => {
                    if self.passed >= MAX_SCENE_STEPS {
                        self.abort();
                        return;
                    }
                    self.move_continuations.clear();
                    let beat = if self.continuation {
                        match self.source.next_checked(answer) {
                            Ok(beat) => beat,
                            Err(_) => { self.abort(); return; }
                        }
                    } else { self.source.next(answer) };
                    self.clock = if self.instant { f32::INFINITY } else { 0.0 };
                    for event in &beat.stage {
                        if let Stage::Sound { id, .. } = event.what {
                            if id != 0 { self.sound(SoundAction::Prepare(id)); }
                        }
                    }
                    self.due = beat.stage.into();
                    self.next = match beat.then {
                        Then::Window(w) => Next::Window(w),
                        Then::End { at, how } => Next::End { at, how },
                    };
                }
                Next::Window(w) => {
                    self.apply_due();
                    if !self.due.is_empty() || w.at as f32 > self.clock {
                        self.next = Next::Window(w);
                        return;
                    }
                    self.open(w);
                }
                Next::End { at, how } => {
                    self.apply_due();
                    if !self.due.is_empty() || at as f32 > self.clock {
                        self.next = Next::End { at, how };
                        return;
                    }
                    if how == End::Battle && self.continuation && self.def.presents.is_none() {
                        let Some(SceneBoundary::Battle(checkpoint)) = self.source.boundary() else { self.abort(); return };
                        if checkpoint.ordinal != self.battles_resumed.saturating_add(1) { self.abort(); return; }
                        self.battle = Some(PausedBattle { checkpoint, battle: None, resume: None, game_over: false, retry_sent: false });
                        self.sound(SoundAction::End);
                        self.epoch = self.epoch.wrapping_add(1);
                        if self.is_owner() && !self.silent {
                            self.outbox.push(SceneCmd::BattleReady { run: self.run.id, ordinal: checkpoint.ordinal, step: self.passed });
                        }
                        return;
                    }
                    // The scene's own end (a jump on to another field, or, for the cutscene of a story point that starts a
                    // battle first, the battle the server then starts) is playing it out; any other stop is an incomplete run.
                    let natural = matches!(how, End::Finished | End::MapJump { .. }) || (how == End::Battle && self.def.presents.is_some());
                    if natural {
                        self.terminal_travel = matches!(how, End::MapJump { .. });
                        self.play_out();
                    } else { self.abort(); }
                    return;
                }
                Next::Held => return,
            }
        }
    }

    /// Open a window: on show (the owner moves past one of nothing at once).
    fn open(&mut self, window: Window) {
        self.move_continuations.clear();
        self.opened += 1;
        self.epoch = self.epoch.wrapping_add(1);
        let panel = Panel::new(window);
        if self.is_owner() && panel.is_blank() && !self.silent {
            self.pass(panel, None);
            return;
        }
        if self.source.echo_voice() && !self.silent {
            if let MessageOrigin::Canon { dialog, .. } = &panel.window().message.origin {
                Global.cue(VoiceCue::Say { field: self.def.field.clone(), window: panel.window().id, dialog: *dialog, page: 0 });
            }
        }
        self.panel = Some(panel);
        // A watcher whose owner moved past this window before it opened here moves past it too.
        if let Some(answer) = self.early.pop_front() {
            self.pass_watched(answer);
        }
    }

    /// Move past a window: count it, say so, report it if this player is the owner, and ask the source what comes next.
    fn pass(&mut self, panel: Panel, answer: Option<usize>) {
        if self.continuation && !self.is_owner() {
            let valid = match (panel.window().message.choice, answer) {
                (None, None) => true,
                (Some(c), Some(a)) => (c.first..=c.last).contains(&a),
                _ => false,
            };
            if !valid { self.abort(); return; }
        }
        let answer = panel.resolve(answer);
        let window = panel.window();
        let server_answer = if self.is_owner() {
            match self.source.server_answer(window, answer) { Ok(value) => value, Err(()) => { self.abort(); return; } }
        } else { answer };
        if self.source.echo_voice() && !self.silent { Global.cue(VoiceCue::Stop); }
        let step = self.passed;
        self.passed += 1;
        self.epoch = self.epoch.wrapping_add(1);
        if !self.silent { self.events.push(SceneEvent::Line {
            index: step,
            opener: window.opener,
            speaker: window.speaker,
            dialog: dialog_of(&window.message.origin),
            asks: panel.asks(),
            answer,
        }); }
        if self.is_owner() && !self.silent {
            let answer = server_answer.map(|line| u8::try_from(line).unwrap_or(u8::MAX));
            self.outbox.push(SceneCmd::Step { run: self.run.id, step, answer });
        }
        self.next = Next::Pull(answer);
    }

    /// A watcher moves past the window on show, with the answer the owner gave.
    fn pass_watched(&mut self, answer: Option<u8>) {
        if let Some(panel) = self.panel.take() {
            self.pass(panel, answer.map(usize::from));
        }
    }

    /// The scene has played to its end. The owner says so and is over; a watcher stands where the scene left everyone
    /// until the server says the run is over.
    fn play_out(&mut self) {
        self.move_continuations.clear();
        self.next = Next::Held;
        if self.is_owner() {
            if self.continuation {
                self.played_out = true;
                self.sound(SoundAction::End);
            } else { self.finish(); }
            if !std::mem::replace(&mut self.sent_end, true) && !self.silent {
                self.outbox.push(SceneCmd::End { run: self.run.id, skipped: false });
            }
        } else {
            self.played_out = true;
        }
    }

    // ---- keys ----------------------------------------------------------------------------------------------------

    /// Give the scene a key. Confirm turns a page, moves past the window or answers its question (the owner; a watcher
    /// only turns pages); the arrows move a question's pointer; Back skips the whole scene.
    pub fn key(&mut self, key: SceneKey) {
        if self.over || self.played_out {
            return;
        }
        let owner = self.is_owner();
        if let Some(paused) = &mut self.battle {
            if key == SceneKey::Back {
                self.skip();
            } else if owner && paused.game_over && !paused.retry_sent && key == SceneKey::Confirm {
                paused.retry_sent = true;
                self.outbox.push(SceneCmd::Retry { run: self.run.id });
            }
            return;
        }
        let panel_key = match key {
            SceneKey::Back => return self.skip(),
            SceneKey::Confirm => PanelKey::Confirm,
            SceneKey::Up => PanelKey::Up,
            SceneKey::Down => PanelKey::Down,
        };
        let (size, answers) = (self.size.get(), self.is_owner());
        let Some(panel) = self.panel.as_mut() else { return };
        let before = panel.voice_state(size);
        let outcome = panel.key(panel_key, size, answers);
        let after = panel.voice_state(size);
        if self.source.echo_voice() && outcome == Outcome::Stay {
            if let MessageOrigin::Canon { dialog, .. } = &panel.window().message.origin {
                if before.0 != after.0 {
                    Global.cue(VoiceCue::Say { field: self.def.field.clone(), window: panel.window().id, dialog: *dialog, page: after.0 });
                } else if before.1 != after.1 {
                    if let Some(line) = after.1 { Global.cue(VoiceCue::Choice { field: self.def.field.clone(), dialog: *dialog, line }); }
                }
            }
        }
        if let Outcome::Pass(answer) = outcome {
            if let Some(panel) = self.panel.take() {
                self.pass(panel, answer);
                self.settle();
            }
        }
    }

    /// Move past the window on show with `answer` (the pointer's line when `None`), as a Confirm on its last page would. A
    /// bot's way through a scene. `false` when no window is on show or this player is only watching.
    pub fn pass_window(&mut self, answer: Option<usize>) -> bool {
        if self.over || !self.is_owner() {
            return false;
        }
        let Some(panel) = self.panel.take() else { return false };
        let answer = self.source.bot_answer(panel.window(), answer);
        self.pass(panel, answer);
        self.settle();
        true
    }

    /// Skip the whole scene: the owner tells the server it is over (skipped), a watcher that it leaves. Once.
    pub fn skip(&mut self) {
        if self.over || self.played_out {
            return;
        }
        if self.continuation {
            // A checkpoint may already be reserved even with no battles consumed (cargo).
            // Back leaves the session; only SceneEnded can release its saved scene in place.
            self.failed = true;
            self.failure_reason = Some("Left the saved scene. Reconnect to continue from its checkpoint.");
            self.finish();
            return;
        }
        self.finish();
        self.skipped = true;
        if !std::mem::replace(&mut self.sent_end, true) && !self.silent {
            let run = self.run.id;
            self.outbox.push(if self.is_owner() { SceneCmd::End { run, skipped: true } } else { SceneCmd::Leave { run } });
        }
    }

    /// A failed or unsupported interpreter run must not be treated as the player's choice to skip.
    fn abort(&mut self) {
        if self.over { return; }
        self.failed = self.continuation;
        self.finish();
        if !std::mem::replace(&mut self.sent_end, true) && !self.silent {
            self.outbox.push(SceneCmd::Leave { run: self.run.id });
        }
    }

    // ---- the server -----------------------------------------------------------------------------------------------

    /// A watcher: the owner moved past window `step`, answering `answer` if it asked. The next step in order moves the
    /// window on show past (or waits for the window to open here); any other is ignored.
    pub fn on_step(&mut self, step: u32, answer: Option<u8>) {
        if self.over || self.is_owner() || step != self.accepted {
            return;
        }
        self.accepted += 1;
        if self.panel.is_some() {
            self.pass_watched(answer);
            self.settle();
        } else {
            self.early.push_back(answer);
        }
    }

    /// The scene continues to hold the field while its battle owns input and presentation.
    pub fn battle_input(&self) -> bool { self.battle.is_some() }

    /// Watchers may still be animating an acknowledged prefix. Catch up to its boundary without
    /// replaying sound; wire ordering guarantees the preceding window acknowledgements came first.
    pub fn on_battle_started(&mut self, ordinal: u32, battle_id: BattleId) {
        if !self.continuation || self.over { return; }
        // An owner can close its result panel before a watcher does. If its next battle has
        // already begun, the preceding durable victory and accepted windows are now historical
        // for this watcher. Reconstruct that prefix silently before attaching the new battle.
        if !self.is_owner() && ordinal == self.battles_resumed.saturating_add(2)
            && self.battle.as_ref().is_some_and(|b| b.resume.is_some()) {
            let silent = self.silent;
            self.silent = true;
            let caught_up = self.resume_pending().and_then(|_| self.reach_boundary());
            self.silent = silent;
            if caught_up.is_err() { self.abort(); return; }
        }
        if ordinal != self.battles_resumed.saturating_add(1) { return; }
        if self.battle.is_none() {
            let silent = self.silent;
            self.silent = true;
            let reached = self.reach_boundary();
            self.silent = silent;
            if reached.is_err() { self.abort(); return; }
        }
        let Some(paused) = &mut self.battle else { return };
        if paused.checkpoint.ordinal != ordinal { return; }
        if paused.battle == Some(battle_id) { return; }
        if paused.battle.is_some() && !paused.game_over { return; }
        paused.battle = Some(battle_id);
        paused.game_over = false;
        paused.retry_sent = false;
        paused.resume = None;
        self.sound(SoundAction::End);
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Only this durable acknowledgement, correlated to the active battle, permits a resume.
    pub fn on_battle_resumed(&mut self, ordinal: u32, battle_id: BattleId, progress: &[u8], gil: u32) -> bool {
        let Some(paused) = &mut self.battle else { return false };
        if paused.checkpoint.ordinal != ordinal || paused.battle != Some(battle_id) || paused.game_over || paused.resume.is_some() { return false; }
        let Some(story) = StoryState::decode(progress) else { self.abort(); return false };
        paused.resume = Some((story, gil));
        true
    }

    pub fn on_game_over(&mut self, ordinal: u32, battle_id: BattleId) {
        let Some(paused) = &mut self.battle else { return };
        if paused.checkpoint.ordinal != ordinal || paused.battle != Some(battle_id) || paused.resume.is_some() { return; }
        paused.game_over = true;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Apply an acknowledged victory only after the battle screen, including its result panel,
    /// has gone. The original stage and its clock survive the fight unchanged.
    pub fn advance_visible(&mut self, seconds: f32, visible: bool) {
        if !visible { return; }
        if self.battle.is_some() {
            match self.resume_pending() {
                Ok(true) => {},
                Ok(false) => return,
                Err(_) => { self.abort(); return; }
            }
        }
        self.advance(seconds);
    }

    fn resume_pending(&mut self) -> Result<bool, String> {
        let Some(paused) = &mut self.battle else { return Ok(false) };
        let Some((story, gil)) = paused.resume.take() else { return Ok(false) };
        let checkpoint = paused.checkpoint;
        self.source.resume_battle(checkpoint, story, gil)?;
        self.battles_resumed = checkpoint.ordinal;
        self.battle = None;
        self.next = Next::Pull(None);
        self.epoch = self.epoch.wrapping_add(1);
        self.sound(SoundAction::Begin);
        self.settle();
        Ok(true)
    }

    /// The server says the run is over, however it ended: the scene is over for this player at once. Nothing is sent.
    pub fn on_ended(&mut self) {
        self.finish();
    }

    /// Over: no window, no actors, the field's camera.
    fn finish(&mut self) {
        if self.over { return; }
        self.sound(SoundAction::End);
        if !self.silent && self.source.echo_voice() { Global.cue(VoiceCue::Stop); }
        self.terminal_fade = self.handoff_fade();
        self.over = true;
        self.battle = None;
        self.panel = None;
        self.early.clear();
        self.next = Next::Held;
        self.due.clear();
        self.move_continuations.clear();
        self.cast.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn sound(&mut self, action: SoundAction) {
        if self.silent { return; }
        if self.sounds.len() >= 256 { self.sounds.pop_front(); }
        self.sounds.push_back(SceneSound { run: self.run.id, action });
    }

    pub fn take_sounds(&mut self) -> Vec<SceneSound> {
        self.sounds.drain(..).collect()
    }

    /// What is owed to the server since the last call.
    pub fn take_commands(&mut self) -> Vec<SceneCmd> {
        std::mem::take(&mut self.outbox)
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<SceneEvent> {
        std::mem::take(&mut self.events)
    }

    // ---- drawing -------------------------------------------------------------------------------------------------

    /// Draw the window on show for a `vw` x `vh` screen at time `t`, near `spot` (where its speaker stands on the screen)
    /// when there is one.
    pub fn draw(&self, vw: u32, vh: u32, t: f64, spot: Option<&SpeakerSpot>, out: &mut crate::ui::draw::DrawList) {
        self.size.set((vw, vh));
        if let Some(panel) = &self.panel {
            panel.draw(vw, vh, t, spot, out);
        }
        if let Some(paused) = self.battle.as_ref().filter(|b| b.game_over) {
            use crate::ui::text::{ellipsize, text_height, text_width};
            use crate::ui::widgets::{BORDER, TEXT};
            let scale = crate::ui::hud::hud_scale(vw, vh);
            let hint = if paused.retry_sent { "Preparing retry... Back: disconnect" }
                else if self.is_owner() { "Confirm: retry  Back: disconnect" }
                else { "Waiting for owner. Back: disconnect" };
            let cells = vw.saturating_sub(32) / (8 * scale);
            let hint = ellipsize(hint, cells as usize);
            let width = text_width(&hint, scale).max(text_width("GAME OVER", scale));
            let height = text_height(scale);
            let x = ((vw as i32 - width) / 2).max(0);
            let y = ((vh as i32 - height * 4) / 2).max(0);
            out.fill_rect(x - 12, y - 12, width + 24, height * 3 + 24, [8, 12, 36, 235]);
            out.frame_rect(x - 12, y - 12, width + 24, height * 3 + 24, 1, BORDER);
            out.draw_text(x + (width - text_width("GAME OVER", scale)) / 2, y, scale, TEXT, "GAME OVER");
            out.draw_text(x, y + height * 2, scale, TEXT, &hint);
        }
    }
}
