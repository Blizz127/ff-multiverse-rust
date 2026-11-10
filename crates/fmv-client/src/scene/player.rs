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
use fmv_ff7::script::scene::{AnimHow, Look, SceneDef, Stage, Then, Timed, Window, FRAMES_PER_SECOND};
use fmv_protocol::{ClientMsg, SceneOutcome, SceneParticipant, SceneRefusal, SceneRole, SceneRunId, MAX_SCENE_STEPS};
use std::cell::Cell;
use std::collections::VecDeque;
use winit::keyboard::KeyCode;

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
}

impl SceneCmd {
    /// The message that carries this command.
    pub fn into_msg(self) -> ClientMsg {
        match self {
            SceneCmd::Step { run, step, answer } => ClientMsg::SceneStep { run, step, answer },
            SceneCmd::End { run, skipped } => ClientMsg::SceneEnd { run, skipped },
            SceneCmd::Leave { run } => ClientMsg::SceneLeave { run },
            SceneCmd::Preview { scene } => ClientMsg::SceneDebugPreview { scene },
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
    Play { id: u16, pan: u8 },
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
    pub pos: Pos,
    pub facing: f32,
    /// Walking now (the avatar's walk clip), standing otherwise.
    pub moving: bool,
    /// Default clip frame from scene time, frozen while a window holds the scene.
    pub frame: f32,
    /// Resolved authored loop or held frame, independent of the default clip's fallback clock.
    pub clip: Option<(&'a str, ClipFrame)>,
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

/// One run of a scene on this machine (see the module docs).
pub struct ScenePlayer {
    run: Run,
    def: SceneDef,
    source: Box<dyn SceneSource>,
    cast: Cast,
    /// Frames since the last window opened.
    clock: f32,
    /// Total advancing scene time across windows; render time never advances cast animation.
    elapsed_frames: f32,
    /// The stage events of the beat in hand that have not happened yet, in order.
    due: VecDeque<Timed<Stage>>,
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
    skipped: bool,
    /// A watcher's scene has played to its end.
    played_out: bool,
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
        if self.source.echo_voice() { Global.cue(VoiceCue::Stop); }
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
    pub fn new(run: Run, def: SceneDef, source: Box<dyn SceneSource>, instant: bool) -> Self {
        let mut cast = Cast::new(def.cast.iter().map(|m| m.entity));
        for member in &def.cast {
            if let Some((slot, clip)) = source.initial_default_animation_clip(member.entity) {
                cast.remember_default(member.entity, slot, clip);
            }
        }
        for event in source.initial() {
            Self::apply_stage(&mut cast, source.as_ref(), &event, 0.0);
        }
        cast.settle();
        let mut player = Self {
            run,
            def,
            source,
            cast,
            clock: 0.0,
            elapsed_frames: 0.0,
            due: VecDeque::new(),
            next: Next::Pull(None),
            panel: None,
            opened: 0,
            passed: 0,
            accepted: 0,
            early: VecDeque::new(),
            over: false,
            skipped: false,
            played_out: false,
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

    // ---- what it is ----------------------------------------------------------------------------------------------

    pub fn run_id(&self) -> SceneRunId {
        self.run.id
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

    pub fn cast_draws(&self) -> Vec<CastDraw<'_>> {
        self.cast
            .actors()
            .filter(|a| a.shown())
            .filter_map(|a| {
                let member = self.def.member(a.entity())?;
                let [x, y, z] = a.pos()?;
                Some(CastDraw { entity: a.entity(), look: &member.look, pos: Pos::new(x, y, z), facing: a.heading(), moving: a.moving(),
                    frame: if a.moving() { a.walked_frames() } else { self.elapsed_frames }, clip: a.animation() })
            })
            .collect()
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

    // ---- time ----------------------------------------------------------------------------------------------------

    /// Let `seconds` of real time go by: the stage moves on and what is due happens, unless a window is on show (it pauses
    /// the whole scene) or the scene is over.
    pub fn advance(&mut self, seconds: f32) {
        if self.over || self.played_out || self.panel.is_some() {
            return;
        }
        let mut left = seconds * FRAMES_PER_SECOND as f32;
        if left.partial_cmp(&0.0).is_none_or(|o| o.is_le()) {
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
            if self.over || self.played_out || self.panel.is_some() || left <= 0.0 {
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
        if let Stage::Sound { id, pan } = event.what {
            // A stalled frame must not dump a burst of old effects. Stops still cancel a loop.
            if id == 0 || self.instant || sound_late <= FRAMES_PER_SECOND as f32 / 2.0 {
                self.sound(SoundAction::Play { id, pan });
            }
        }
        Self::apply_stage(&mut self.cast, self.source.as_ref(), &event.what, motion_late);
    }

    fn apply_stage(cast: &mut Cast, source: &dyn SceneSource, event: &Stage, late: f32) {
        // Normal-rate presentation uses the script's own field model and loaded clip bounds:
        // a full default loop, full Hold, or explicit Hold/Once range. Other forms clear an old override.
        if let Stage::Anim { entity, animation, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 } = event {
            if let Some(clip) = source.animation_clip(*entity, *animation) {
                cast.apply_loop(*entity, *animation, clip, late);
                return;
            }
        }
        if let Stage::Anim { entity, animation, how: AnimHow::Hold, first: None, last: None, speed: 16, divisor: 1 } = event {
            if let Some(clip) = source.animation_clip(*entity, *animation) {
                cast.apply_hold_full(*entity, clip, late);
                return;
            }
        }
        if let Stage::Anim { entity, animation, how: AnimHow::Hold, first: Some(first), last: Some(last), speed: 16, divisor: 1 } = event {
            if first <= last {
                if let Some(clip) = source.animation_clip(*entity, *animation) {
                    if first == last { cast.apply_hold(*entity, clip, *first); }
                    else { cast.apply_hold_range(*entity, clip, *first, *last, late); }
                    return;
                }
            }
        }
        if let Stage::Anim { entity, animation, how: AnimHow::Once, first: Some(first), last: Some(last), speed: 16, divisor: 1 } = event {
            if first <= last {
                if let Some(clip) = source.animation_clip(*entity, *animation) {
                    if cast.apply_once(*entity, *animation, clip, *first, *last, late) { return; }
                }
            }
        }
        cast.apply(event, late, &|x, y, triangle| source.height_at(x, y, triangle));
        if let Stage::Move { entity, how: fmv_ff7::script::scene::MoveHow::Walk, animation: Some(slot @ (1 | 2)), .. } = event {
            if let Some(clip) = source.animation_clip(*entity, *slot) {
                cast.apply_move_animation(*entity, clip);
            }
        }
    }

    /// Go on as far as the clock allows: pull the next beat, play what is due, open the window or end the scene. Stops at
    /// an open window, at a beat whose time has not come and at the end. (A loop, bounded by the scene's own limit on
    /// windows: a source of nothing but empty windows ends instead of running away.)
    fn settle(&mut self) {
        loop {
            if self.over || self.panel.is_some() {
                return;
            }
            match std::mem::replace(&mut self.next, Next::Held) {
                Next::Pull(answer) => {
                    if self.passed >= MAX_SCENE_STEPS {
                        self.abort();
                        return;
                    }
                    let beat = self.source.next(answer);
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
                    // The scene's own end (a jump on to another field, or, for the cutscene of a story point that starts a
                    // battle first, the battle the server then starts) is playing it out; any other stop is an incomplete run.
                    let natural = matches!(how, End::Finished | End::MapJump { .. }) || (how == End::Battle && self.def.presents.is_some());
                    if natural { self.play_out(); } else { self.abort(); }
                    return;
                }
                Next::Held => return,
            }
        }
    }

    /// Open a window: on show (the owner moves past one of nothing at once).
    fn open(&mut self, window: Window) {
        self.opened += 1;
        self.epoch = self.epoch.wrapping_add(1);
        let panel = Panel::new(window);
        if self.is_owner() && panel.is_blank() {
            self.pass(panel, None);
            return;
        }
        if self.source.echo_voice() {
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
        let answer = panel.resolve(answer);
        let window = panel.window();
        let server_answer = if self.is_owner() {
            match self.source.server_answer(window, answer) { Ok(value) => value, Err(()) => { self.abort(); return; } }
        } else { answer };
        if self.source.echo_voice() { Global.cue(VoiceCue::Stop); }
        let step = self.passed;
        self.passed += 1;
        self.epoch = self.epoch.wrapping_add(1);
        self.events.push(SceneEvent::Line {
            index: step,
            opener: window.opener,
            speaker: window.speaker,
            dialog: dialog_of(&window.message.origin),
            asks: panel.asks(),
            answer,
        });
        if self.is_owner() {
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
        self.next = Next::Held;
        if self.is_owner() {
            self.finish();
            if !std::mem::replace(&mut self.sent_end, true) {
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
        if self.over {
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
        if self.over {
            return;
        }
        self.finish();
        self.skipped = true;
        if !std::mem::replace(&mut self.sent_end, true) {
            let run = self.run.id;
            self.outbox.push(if self.is_owner() { SceneCmd::End { run, skipped: true } } else { SceneCmd::Leave { run } });
        }
    }

    /// A failed or unsupported interpreter run must not be treated as the player's choice to skip.
    fn abort(&mut self) {
        if self.over { return; }
        self.finish();
        if !std::mem::replace(&mut self.sent_end, true) {
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

    /// The server says the run is over, however it ended: the scene is over for this player at once. Nothing is sent.
    pub fn on_ended(&mut self) {
        self.finish();
    }

    /// Over: no window, no actors, the field's camera.
    fn finish(&mut self) {
        if self.over { return; }
        self.sound(SoundAction::End);
        if self.source.echo_voice() { Global.cue(VoiceCue::Stop); }
        self.over = true;
        self.panel = None;
        self.early.clear();
        self.next = Next::Held;
        self.due.clear();
        self.cast.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn sound(&mut self, action: SoundAction) {
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
    }
}
