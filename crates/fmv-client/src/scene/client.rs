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
//!   [`SceneEvent::Unavailable`] and the player asks to leave on the spot; an owner aborts the run without a story write.
//!   The server's `SceneEnded` is what releases the player;
//! * a scene that ends here (played to its end by its owner, or skipped by a Back key) is dropped at once: no window, no
//!   actors, the field's camera and the field's keys; the server's later `SceneEnded` for it is reported
//!   ([`SceneEvent::Ended`]) and changes nothing;
//! * `SceneEnded` for a scene that is on show ends it at once, in either role; a repeated one, or one for another run, is
//!   ignored.
//!
//! The keys of a scene are the talk window's (see [`SceneKey::from_event`]). A scene holds field
//! movement throughout a durable battle, while [`super::keys::route_key`] gives combat and its
//! result panel their input. A defeated continuation stays held until its owner explicitly retries.

use super::player::{Run, SceneCmd, SceneEvent, SceneKey, ScenePlayer, SceneSound, SoundAction};
use super::source::{NoScenes, SceneBuilder};
use fmv_ff7::story::StoryState;
use fmv_ff7::script::scene::SceneResources;
use fmv_protocol::{SceneOrigin, SceneOutcome, SceneParticipant, SceneRefusal, SceneReplay, SceneRole, SceneRunId, ServerMsg, MAX_SCENE_STEPS};
use std::collections::VecDeque;
use winit::keyboard::KeyCode;

/// Events and notes kept until the loop takes them (a loop that never does, such as a window's, loses the oldest).
const MAX_PENDING: usize = 256;

/// A field mutation from the authored scene, delivered once even when its player ends between
/// rendered frames. FieldView owns the resulting mask state, across successive scenes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneBackground {
    pub field: String,
    pub op: fmv_ff7::background::BackgroundOp,
}

/// The scenes of one client.
pub struct SceneClient {
    builder: Box<dyn SceneBuilder>,
    player: Option<ScenePlayer>,
    /// Immutable original inputs of the retained durable run, for idempotent roster refreshes.
    binding: Option<StartBinding>,
    authoritative: Vec<ReplayInput>,
    outbox: Vec<SceneCmd>,
    events: VecDeque<SceneEvent>,
    notes: VecDeque<String>,
    sounds: VecDeque<SceneSound>,
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
    visible: bool,
    /// A reserved continuation cannot be released locally. The session closes on this error.
    failure: Option<String>,
    background: Vec<SceneBackground>,
    background_seen: usize,
    /// Presentation RNG belongs to the running client, independently of the VM gameplay RNG.
    shake_rng: [u8; 2],
    /// Only a terminal authored map jump may carry its screen colour into the field handoff.
    handoff_fade: Option<(String, fmv_ff7::script::scene::FadeColor)>,
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
            binding: None,
            authoritative: Vec::new(),
            outbox: Vec::new(),
            events: VecDeque::new(),
            notes: VecDeque::new(),
            sounds: VecDeque::new(),
            finished: None,
            refusal: None,
            started: 0,
            instant: false,
            armed: Vec::new(),
            armed_epoch: 0,
            visible: true,
            failure: None,
            background: Vec::new(),
            background_seen: 0,
            shake_rng: [0; 2],
            handoff_fade: None,
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

    /// Whether a scene holds the field, including a suspended battle or a defeated attempt.
    pub fn is_active(&self) -> bool {
        self.player.is_some() || self.failure.is_some()
    }

    pub fn failure(&self) -> Option<&str> { self.failure.as_deref() }

    pub fn fade(&self) -> Option<fmv_ff7::script::scene::FadeColor> {
        self.player.as_ref().and_then(ScenePlayer::fade).or_else(|| self.handoff_fade.as_ref()
            .filter(|(field, _)| self.player.as_ref().is_none_or(|p| p.def().field == *field))
            .map(|(_, colour)| *colour))
    }

    /// A durable scene keeps the field held through its final authoritative placement.
    pub fn holds_authoritative_pose(&self) -> bool {
        self.player.as_ref().is_some_and(ScenePlayer::is_durable)
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
    #[cfg(test)]
    pub fn on_message(&mut self, area: &str, msg: ServerMsg) {
        let _ = self.on_message_with_presentation_origin(area, msg);
    }

    /// Apply scene traffic and return the accepted ordinary owner's trigger pose only after its
    /// source and player were built. The state adopts it before the next message or rendered frame;
    /// a watcher, refused source or durable replay never places the local player through this path.
    /// The result also survives a successful instant scene finishing during construction.
    pub fn on_message_with_presentation_origin(&mut self, area: &str, msg: ServerMsg) -> Option<SceneOrigin> {
        match msg {
            ServerMsg::SceneStart { run, scene, seed, progress, gil, replay, presentation_origin, participants, you, .. } => {
                return self.start(Start { run, scene, seed, progress, gil, replay, presentation_origin, participants, you }, area);
            }
            ServerMsg::SceneStep { run, step, answer } => {
                if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
                    if player.is_durable() && !player.is_owner()
                        && step as usize == self.authoritative.iter().filter(|e| matches!(e, ReplayInput::Window { .. })).count() {
                        self.authoritative.push(ReplayInput::Window { step, answer });
                    }
                    player.on_step(step, answer);
                    self.reap();
                }
            }
            ServerMsg::SceneEnded { run, outcome } => self.ended(run, outcome),
            ServerMsg::SceneBattleStarted { run, ordinal, battle_id } => {
                if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
                    player.on_battle_started(ordinal, battle_id);
                    self.armed.clear();
                    self.reap();
                }
            }
            ServerMsg::SceneBattleResumed { run, ordinal, battle_id, progress, gil } => {
                if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
                    if player.on_battle_resumed(ordinal, battle_id, &progress, gil) {
                        self.authoritative.push(ReplayInput::Battle { ordinal, progress, gil });
                    }
                    self.reap();
                }
            }
            ServerMsg::SceneGameOver { run, ordinal, battle_id } => {
                if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
                    player.on_game_over(ordinal, battle_id);
                    self.reap();
                }
            }
            ServerMsg::SceneRefused { reason } => {
                self.refusal = Some(reason);
                self.event(SceneEvent::Refused(reason));
            }
            _ => {}
        }
        None
    }

    fn start(&mut self, start: Start, area: &str) -> Option<SceneOrigin> {
        let Start { run, scene, seed, progress, gil, replay, presentation_origin, participants, you } = start;
        let durable = replay.is_some();
        if durable && presentation_origin.is_some() {
            self.unavailable(run, you, &scene, "a scene has both ordinary and durable origins", true);
            return None;
        }
        let needs_origin = fmv_ff7::script::scene::scene(&scene).is_some_and(|def| def.requires_presentation_origin());
        let origin_error = presentation_origin.as_ref().and_then(|origin| origin.validate().err())
            .or_else(|| (!durable && needs_origin && presentation_origin.is_none()).then_some("the accepted presentation origin is missing"));
        if let Some(why) = origin_error {
            let reserved = self.player.as_ref().is_some_and(ScenePlayer::is_durable);
            if !reserved { self.end_local(); }
            self.unavailable(run, you, &scene, why, reserved);
            return None;
        }
        let binding = replay.as_ref().map(|r| StartBinding {
            scene: scene.clone(), seed, progress: progress.clone(), gil, role: you,
            source: r.source, origin: r.origin,
        });
        if self.player.as_ref().is_some_and(|p| p.run_id() == run && p.is_durable()) {
            // A returning owner gets a new entity id. Existing guests have already received the
            // ordered step stream, including answers queued ahead of their visible animation.
            let Some(replay) = replay.as_ref() else {
                self.unavailable(run, you, &scene, "a retained durable scene lost its replay binding", true);
                return None;
            };
            let matches = binding == self.binding && replay.validate().is_ok()
                && replay.events.iter().map(ReplayInput::from).eq(self.authoritative.iter().cloned())
                && self.player.as_ref().is_some_and(|p| p.def().field.eq_ignore_ascii_case(area));
            // Compare the authoritative history above, then validate its opaque boundary identities
            // through a fresh strict source. Do not replace the live player or drain this probe's queues.
            let initial_rng = self.player.as_ref().map_or([0; 2], ScenePlayer::initial_shake_rng_indices);
            let valid = matches && StoryState::decode(&progress).is_some_and(|story| {
                self.builder.build_with_origin(&scene, &story, seed, SceneResources { gil: Some(gil) }, replay.origin.as_ref())
                    .and_then(|built| ScenePlayer::with_replay_seeded(
                        Run { id: run, participants: participants.clone(), role: you }, built.def, built.source, false, replay, initial_rng))
                    .is_ok()
            });
            if !valid {
                self.unavailable(run, you, &scene, "a retained scene's original inputs or accepted history changed", true);
            } else if let Some(player) = self.player.as_mut() {
                player.refresh_participants(participants);
            }
            return None;
        }
        // A run still on show is a stale one (the server holds a player in one run at a time): it is replaced.
        self.end_local();
        self.binding = binding;
        self.authoritative = replay.as_ref().map(|r| r.events.iter().map(ReplayInput::from).collect()).unwrap_or_default();
        self.finished = None;
        self.failure = None;
        self.armed.clear();
        self.visible = true;
        self.started += 1;
        self.event(SceneEvent::Started { run, scene: clean_id(&scene), role: you, participants: participants.len() });
        if scene == fmv_ff7::script::scene::PROOF_SCENE_ID && participants.len() > 1 && crate::mods::script_source().is_some() {
            self.note("scene: Echo-S bar dialogue is unavailable with watchers; using the original script".into());
        }
        // The scene belongs to its owner. A watcher's local wallet may be different, and a
        // missing snapshot must never be reconstructed as zero (the protocol requires the field).
        let resources = SceneResources { gil: Some(gil) };
        let built = match StoryState::decode(&progress) {
            Some(story) if durable => self.builder.build_with_origin(&scene, &story, seed, resources, replay.as_ref().and_then(|r| r.origin.as_ref())),
            Some(story) if presentation_origin.is_some() => self.builder.build_with_origin(&scene, &story, seed, resources, presentation_origin.as_ref()),
            Some(story) if replay.is_none() && you == SceneRole::Owner && participants.len() == 1 => self.builder.build_private(&scene, &story, seed, resources),
            Some(story) => self.builder.build(&scene, &story, seed, resources),
            None => Err("the story the scene plays against cannot be read here".to_string()),
        };
        match built {
            Ok(built) if built.def.field.eq_ignore_ascii_case(area) => {
                // One line for the log (ids and words of ours only): which scene, in which role, and whose script it is shown
                // from, so a log of a play session says what was on show without a word of the game.
                self.note(format!(
                    "scene: started run={} {} as {} with {} participant(s), script: {}",
                    run.0,
                    clean_id(&scene),
                    if you == SceneRole::Owner { "owner" } else { "watcher" },
                    participants.len(),
                    if built.source.echo_voice() { "echo-s" } else { "original" }
                ));
                let identity = Run { id: run, participants, role: you };
                let player = match replay {
                    Some(replay) => ScenePlayer::with_replay_seeded(identity, built.def, built.source, self.instant, &replay, self.shake_rng),
                    None => Ok(ScenePlayer::new_seeded(identity, built.def, built.source, self.instant, self.shake_rng)),
                };
                match player {
                    Ok(player) => {
                        self.player = Some(player);
                        self.reap();
                        return if !durable && you == SceneRole::Owner { presentation_origin } else { None };
                    }
                    Err(why) => self.unavailable(run, you, &scene, &why, durable),
                }
            }
            Ok(built) => self.unavailable(run, you, &scene, &format!("the scene plays in another field ({}), this is {}", clean_id(&built.def.field), clean_id(area)), durable),
            Err(why) => self.unavailable(run, you, &scene, &why, durable),
        }
        None
    }

    /// An ordinary scene can be left without advancing the owner's story. A reserved durable
    /// scene instead closes the session and stays locally held until that close takes effect.
    fn unavailable(&mut self, run: SceneRunId, _role: SceneRole, scene: &str, why: &str, durable: bool) {
        if durable {
            self.failure = Some(format!("Saved scene cannot be resumed safely: {why}. Reconnect with matching game data."));
        }
        let action = if durable { "closing the session" } else { "releasing the player" };
        self.note(format!("scene: cannot play {} here: {why}; {action}", clean_id(scene)));
        self.event(SceneEvent::Unavailable);
        self.outbox.push(SceneCmd::Leave { run });
        self.finished = Some((run, 0));
    }

    fn ended(&mut self, run: SceneRunId, outcome: SceneOutcome) {
        self.collect_background();
        let lines = if let Some(player) = self.player.as_mut().filter(|p| p.run_id() == run) {
            if outcome == SceneOutcome::Finished {
                if let Some(colour) = player.handoff_fade() { self.handoff_fade = Some((player.def().field.clone(), colour)); }
            } else { self.handoff_fade = None; }
            player.on_ended();
            self.shake_rng = player.shake_rng_indices();
            let lines = player.lines();
            self.sound(SceneSound { run, action: SoundAction::End });
            self.player = None;
            self.finished = None;
            lines
        } else if let Some((finished, lines)) = self.finished.filter(|(r, _)| *r == run) {
            if outcome != SceneOutcome::Finished { self.handoff_fade = None; }
            let _ = finished;
            self.finished = None;
            lines
        } else {
            // Not a run this client has: a repeat of one it already heard end, or somebody else's.
            return;
        };
        self.note(format!("scene: ended run={} outcome={outcome:?} windows={lines}", run.0));
        self.event(SceneEvent::Ended { run, outcome, lines });
    }

    // ---- time and keys -------------------------------------------------------------------------------------------

    /// Let `seconds` go by: the stage moves on and what is due happens.
    #[cfg(test)]
    pub fn advance(&mut self, seconds: f32) {
        self.advance_visible(seconds, true);
    }

    /// Battle rendering includes its lingering result panel. Neither script time nor a durable
    /// resume acknowledgement advances the scene until that presentation has left the screen.
    pub fn advance_visible(&mut self, seconds: f32, visible: bool) {
        self.visible = visible;
        if let Some(player) = self.player.as_mut() {
            player.advance_visible(seconds, visible);
            self.reap();
        }
    }

    pub fn battle_input(&self) -> bool { self.player.as_ref().is_some_and(ScenePlayer::battle_input) }

    /// A key event. Returns whether the scene took it: while one is on show it takes every key (the field gets none), and
    /// only the talk window's keys do anything.
    pub fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        if self.failure.is_some() { return true; }
        let Some(player) = self.player.as_mut() else { return false };
        if !self.visible { return true; }
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
        if !self.visible { return; }
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
        self.collect_background();
        if let Some(player) = self.player.take() {
            self.shake_rng = player.shake_rng_indices();
            if let Some(colour) = player.handoff_fade() { self.handoff_fade = Some((player.def().field.clone(), colour)); }
            self.sound(SceneSound { run: player.run_id(), action: SoundAction::End });
            self.finished = Some((player.run_id(), player.lines()));
        }
        self.background_seen = 0;
        self.binding = None;
        self.authoritative.clear();
    }

    fn collect_background(&mut self) {
        let Some(player) = &self.player else { return };
        let ops = player.background_ops();
        for &op in ops.get(self.background_seen..).unwrap_or_default() {
            self.background.push(SceneBackground { field: player.def().field.clone(), op });
        }
        self.background_seen = ops.len();
    }

    pub fn drain_background(&mut self) -> Vec<SceneBackground> { std::mem::take(&mut self.background) }

    /// A new field visit creates a new background baseline; old-visit commands cannot leak in.
    pub fn discard_background(&mut self) { self.background.clear(); }

    /// A new field's initialization resets active effects, but process-local shake indices survive.
    pub fn discard_field_effects(&mut self) { self.handoff_fade = None; }

    // ---- what is owed and what happened ----------------------------------------------------------------------------

    /// Collect what the player has to say, and drop it when it is over.
    fn reap(&mut self) {
        self.collect_background();
        let Some(player) = self.player.as_mut() else { return };
        if let Some(reason) = player.continuation_failure() {
            self.failure = Some(reason.to_string());
        }
        let commands = player.take_commands();
        if player.is_durable() && player.is_owner() {
            for command in &commands {
                if let SceneCmd::Step { step, answer, .. } = command {
                    self.authoritative.push(ReplayInput::Window { step: *step, answer: *answer });
                }
            }
        }
        self.outbox.extend(commands);
        let events: Vec<SceneEvent> = player.take_events();
        let sounds = player.take_sounds();
        let over = player.is_over().then(|| (player.run_id(), player.lines()));
        for sound in sounds { self.sound(sound); }
        for event in events {
            self.event(event);
        }
        if let Some(over) = over {
            if let Some(player) = &self.player {
                self.shake_rng = player.shake_rng_indices();
                if let Some(colour) = player.handoff_fade() { self.handoff_fade = Some((player.def().field.clone(), colour)); }
            }
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

    fn sound(&mut self, sound: SceneSound) {
        if self.sounds.len() >= MAX_PENDING { self.sounds.pop_front(); }
        self.sounds.push_back(sound);
    }

    pub fn take_sounds(&mut self) -> Vec<SceneSound> {
        self.sounds.drain(..).collect()
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

/// Accepted wire inputs, independent of how far the visible stage has animated.
#[derive(Clone, PartialEq)]
enum ReplayInput {
    Window { step: u32, answer: Option<u8> },
    Battle { ordinal: u32, progress: Vec<u8>, gil: u32 },
}

impl From<&fmv_protocol::SceneReplayEvent> for ReplayInput {
    fn from(event: &fmv_protocol::SceneReplayEvent) -> Self {
        match event {
            fmv_protocol::SceneReplayEvent::Window { step, answer, .. } => Self::Window { step: *step, answer: *answer },
            fmv_protocol::SceneReplayEvent::Battle { ordinal, progress, gil, .. } => Self::Battle { ordinal: *ordinal, progress: progress.clone(), gil: *gil },
        }
    }
}

#[derive(PartialEq)]
struct StartBinding {
    scene: String,
    seed: u32,
    progress: Vec<u8>,
    gil: u32,
    role: SceneRole,
    source: [u8; 32],
    origin: Option<fmv_protocol::SceneOrigin>,
}

/// The fields of a `SceneStart`.
struct Start {
    run: SceneRunId,
    scene: String,
    seed: u32,
    progress: Vec<u8>,
    gil: u32,
    replay: Option<SceneReplay>,
    presentation_origin: Option<fmv_protocol::SceneOrigin>,
    participants: Vec<SceneParticipant>,
    you: SceneRole,
}
