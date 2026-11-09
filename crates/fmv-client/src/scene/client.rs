//! The scene client: what a client holds of story scenes, between the server's messages and the scene player.
//!
//! [`SceneClient`] lives in [`crate::state::ClientState`], like the battle and story mirrors. It turns the server's scene
//! messages into a [`ScenePlayer`] (built from this machine's own game through a [`SceneBuilder`]), hands the player the
//! keys and the time, and says what is owed to the server ([`SceneClient::take_commands`]) and what happened
//! ([`SceneClient::take_events`], [`SceneClient::take_notes`]). Whoever drives it (the window, the headless loop) sends the
//! commands and draws or prints the rest.
//!
//! # Control is never left locked
//!
//! The server holds the player until it says the run is over (`SceneEnded`), so the one thing this client must never do is
//! keep a player waiting for a scene it cannot show:
//!
//! * a scene this machine cannot build (no game data for its field, a story it cannot read, a scene of another field) is
//!   [`SceneEvent::Unavailable`] and the player releases itself on the spot: the owner tells the server the scene is
//!   skipped, a watcher that it leaves. Nothing else happens; the server's `SceneEnded` is what releases the player;
//! * a scene that ends here (played to its end by its owner, or skipped by a Back key) is dropped at once: no window, no
//!   actors, the field's camera and the field's keys; the server's later `SceneEnded` for it is reported
//!   ([`SceneEvent::Ended`]) and changes nothing;
//! * `SceneEnded` for a scene that is on show ends it at once, in either role; a repeated one, or one for another run, is
//!   ignored.
//!
//! The keys of a scene are the talk window's (see [`SceneKey::from_event`]); while a scene is on show EVERY key is the
//! scene's ([`SceneClient::key`] says so), so nothing reaches the field: no walking, no talking, no battle key, no `Interact`.

use super::player::{Run, SceneCmd, SceneEvent, SceneKey, ScenePlayer};
use super::source::{NoScenes, SceneBuilder};
use fmv_ff7::story::StoryState;
use fmv_protocol::{SceneOutcome, SceneParticipant, SceneRefusal, SceneRole, SceneRunId, ServerMsg, MAX_SCENE_STEPS};
use std::collections::VecDeque;
use winit::keyboard::KeyCode;

/// Events and notes kept until the loop takes them (a loop that never does, such as a window's, loses the oldest).
const MAX_PENDING: usize = 256;

/// The scenes of one client.
pub struct SceneClient {
    builder: Box<dyn SceneBuilder>,
    player: Option<ScenePlayer>,
    outbox: Vec<SceneCmd>,
    events: VecDeque<SceneEvent>,
    notes: VecDeque<String>,
    /// The run this client finished by itself (or could not play) and has not yet heard the server end, with the windows
    /// it moved past: its `SceneEnded` is still to be reported.
    finished: Option<(SceneRunId, u32)>,
    refusal: Option<SceneRefusal>,
    /// How many runs began.
    started: u64,
    /// No waiting: a bot has no picture to wait for.
    instant: bool,
    /// The arrow keys pressed (fresh) while the window has been open and not yet let go of: only they move a question's
    /// pointer and only they repeat (a direction held while walking up to the character goes on repeating when the window
    /// opens, and those repeats belong to the walk, not to a pointer that spins through the answers).
    armed: Vec<KeyCode>,
    armed_epoch: u32,
}

impl std::fmt::Debug for SceneClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneClient").field("player", &self.player).field("started", &self.started).finish_non_exhaustive()
    }
}

impl Default for SceneClient {
    fn default() -> Self {
        Self::new()
    }
}

/// An id from the network as printed: letters, digits and `_ - . /` only, at most 64 characters (a scene's id is
/// `<game>/<field>/<name>`), anything else becomes `?`.
pub fn clean_id(id: &str) -> String {
    id.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/') { c } else { '?' }).collect()
}

impl SceneClient {
    /// A client that cannot build any scene (give it [`SceneClient::set_builder`] to play them).
    pub fn new() -> Self {
        Self {
            builder: Box::new(NoScenes),
            player: None,
            outbox: Vec::new(),
            events: VecDeque::new(),
            notes: VecDeque::new(),
            finished: None,
            refusal: None,
            started: 0,
            instant: false,
            armed: Vec::new(),
            armed_epoch: 0,
        }
    }

    /// Where scenes are built from (this machine's own game).
    pub fn set_builder(&mut self, builder: Box<dyn SceneBuilder>) {
        self.builder = builder;
    }

    /// Play without waiting for the picture (a headless bot): the first window of a scene is open as soon as it starts and
    /// each next one as soon as the one before is moved past. Applies to scenes that start from now on.
    pub fn set_instant(&mut self, instant: bool) {
        self.instant = instant;
    }

    /// Whether a scene is on show: every key is the scene's, the player is held, the trigger's character is hidden and the
    /// cast stands in its place.
    pub fn is_active(&self) -> bool {
        self.player.is_some()
    }

    pub fn player(&self) -> Option<&ScenePlayer> {
        self.player.as_ref()
    }

    /// How many runs began (a count the window watches to let go of the keys held when one starts).
    pub fn started(&self) -> u64 {
        self.started
    }

    /// What the scene told the camera: the field's own when no scene is on show.
    #[cfg(test)]
    pub fn camera(&self) -> super::stage::CameraState {
        self.player.as_ref().map_or(super::stage::CameraState::Default, ScenePlayer::camera)
    }

    /// How near a player has to stand to the trigger of scene number `index` in the game's list (see
    /// [`SceneBuilder::reach`]).
    pub fn reach(&mut self, index: usize, area: &str) -> Option<f32> {
        self.builder.reach(index, area)
    }

    // ---- the server ----------------------------------------------------------------------------------------------

    /// One of the server's scene messages (anything else is not ours). `area` is the key of the field this player stands in.
    pub fn on_message(&mut self, area: &str, msg: ServerMsg) {
        match msg {
            ServerMsg::SceneStart { run, scene, seed, progress, participants, you, .. } => {
                self.start(Start { run, scene, seed, progress, participants, you }, area);
            }
            ServerMsg::SceneStep { run, step, answer } => {
                if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
                    player.on_step(step, answer);
                    self.reap();
                }
            }
            ServerMsg::SceneEnded { run, outcome } => self.ended(run, outcome),
            ServerMsg::SceneRefused { reason } => {
                self.refusal = Some(reason);
                self.event(SceneEvent::Refused(reason));
            }
            _ => {}
        }
    }

    fn start(&mut self, start: Start, area: &str) {
        let Start { run, scene, seed, progress, participants, you } = start;
        // A run still on show is a stale one (the server holds a player in one run at a time): it is replaced.
        self.player = None;
        self.finished = None;
        self.armed.clear();
        self.started += 1;
        self.event(SceneEvent::Started { run, scene: clean_id(&scene), role: you, participants: participants.len() });
        let built = match StoryState::decode(&progress) {
            Some(story) => self.builder.build(&scene, &story, seed),
            None => Err("the story the scene plays against cannot be read here".to_string()),
        };
        match built {
            Ok(built) if built.def.field.eq_ignore_ascii_case(area) => {
                let run = Run { id: run, participants, role: you };
                self.player = Some(ScenePlayer::new(run, built.def, built.source, self.instant));
                self.reap();
            }
            Ok(built) => self.unavailable(run, you, &scene, &format!("the scene plays in another field ({}), this is {}", clean_id(&built.def.field), clean_id(area))),
            Err(why) => self.unavailable(run, you, &scene, &why),
        }
    }

    /// This machine cannot play the run: release the player now. The owner says the scene is skipped, a watcher leaves;
    /// the server's `SceneEnded` is what lets the player go.
    fn unavailable(&mut self, run: SceneRunId, role: SceneRole, scene: &str, why: &str) {
        self.note(format!("scene: cannot play {} here: {why}; releasing the player", clean_id(scene)));
        self.event(SceneEvent::Unavailable);
        self.outbox.push(match role {
            SceneRole::Owner => SceneCmd::End { run, skipped: true },
            SceneRole::Watcher => SceneCmd::Leave { run },
        });
        self.finished = Some((run, 0));
    }

    fn ended(&mut self, run: SceneRunId, outcome: SceneOutcome) {
        let lines = if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
            player.on_ended();
            let lines = player.lines();
            self.player = None;
            self.finished = None;
            lines
        } else if let Some((finished, lines)) = self.finished.filter(|(r, _)| *r == run) {
            let _ = finished;
            self.finished = None;
            lines
        } else {
            // Not a run this client has: a repeat of one it already heard end, or somebody else's.
            return;
        };
        self.event(SceneEvent::Ended { run, outcome, lines });
    }

    // ---- time and keys -------------------------------------------------------------------------------------------

    /// Let `seconds` go by: the stage moves on and what is due happens.
    pub fn advance(&mut self, seconds: f32) {
        if let Some(player) = self.player.as_mut() {
            player.advance(seconds);
            self.reap();
        }
    }

    /// A key event. Returns whether the scene took it: while one is on show it takes every key (the field gets none), and
    /// only the talk window's keys do anything.
    pub fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        let Some(player) = self.player.as_mut() else { return false };
        if player.epoch() != self.armed_epoch {
            self.armed.clear();
            self.armed_epoch = player.epoch();
        }
        if !pressed {
            // A release is never an action, but it ends an arrow key's claim.
            self.armed.retain(|k| *k != code);
            return true;
        }
        let Some(key) = SceneKey::from_event(code, pressed, repeat) else { return true };
        if matches!(key, SceneKey::Up | SceneKey::Down) {
            // Only a key pressed while the window is open moves the pointer, and only that one repeats.
            match (repeat, self.armed.contains(&code)) {
                (false, false) => self.armed.push(code),
                (true, false) => return true,
                _ => {}
            }
        }
        player.key(key);
        self.reap();
        true
    }

    /// A bot's way through a scene: as the owner it moves past every window on show at once, answering a question with
    /// `answer` (the pointer's line when `None`); a watcher's windows are moved past by the owner's steps. (Meant for a
    /// client set [`instant`](SceneClient::set_instant), where moving past a window opens the next at once.)
    pub fn autoplay(&mut self, answer: Option<usize>) {
        for _ in 0..=MAX_SCENE_STEPS {
            let Some(player) = self.player.as_mut() else { break };
            if !player.pass_window(answer) {
                break;
            }
        }
        self.reap();
    }

    /// The scene cannot go on here (the area changed under it): drop it without a word.
    pub fn end_local(&mut self) {
        if let Some(player) = self.player.take() {
            self.finished = Some((player.run_id(), player.lines()));
        }
    }

    // ---- what is owed and what happened ----------------------------------------------------------------------------

    /// Collect what the player has to say, and drop it when it is over.
    fn reap(&mut self) {
        let Some(player) = self.player.as_mut() else { return };
        self.outbox.extend(player.take_commands());
        let events: Vec<SceneEvent> = player.take_events();
        let over = player.is_over().then(|| (player.run_id(), player.lines()));
        for event in events {
            self.event(event);
        }
        if let Some(over) = over {
            self.player = None;
            self.finished = Some(over);
        }
    }

    /// What is owed to the server since the last call, oldest first.
    pub fn take_commands(&mut self) -> Vec<SceneCmd> {
        std::mem::take(&mut self.outbox)
    }

    /// Ask the server for a preview of scene `scene` (debug; the server refuses unless it runs with
    /// `--debug-scene-preview`). It goes out with the rest of the commands.
    pub fn request_preview(&mut self, scene: &str) {
        self.outbox.push(SceneCmd::Preview { scene: scene.to_string() });
    }

    /// What happened since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<SceneEvent> {
        self.events.drain(..).collect()
    }

    /// The words for stderr since the last call, oldest first (ids and counts only).
    pub fn take_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }

    /// The server's last refusal of a scene request, once.
    pub fn take_refusal(&mut self) -> Option<SceneRefusal> {
        self.refusal.take()
    }

    fn event(&mut self, event: SceneEvent) {
        if self.events.len() >= MAX_PENDING {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_PENDING {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }
}

/// The fields of a `SceneStart`.
struct Start {
    run: SceneRunId,
    scene: String,
    seed: u32,
    progress: Vec<u8>,
    participants: Vec<SceneParticipant>,
    you: SceneRole,
}
