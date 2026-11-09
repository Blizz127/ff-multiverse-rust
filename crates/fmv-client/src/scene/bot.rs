//! The headless scene bot: `--scene-preview <ID>`, `--scene-watch`, `--scene-answer <LINE>`, `--scene-wait-secs <N>` and
//! `--scene-watchers <N>`.
//!
//! The bot runs a story scene through the SAME code the window uses ([`super::SceneClient`]: the player, the owner's steps,
//! the watcher's order), with no waiting for a picture and no keys: it moves past every window at once, answering a question
//! with the line it was given (`--scene-answer`) or the question's pointer. What it adds is the script around it:
//!
//! 1. **Preview** (`--scene-preview ID`): when its party allows (a bot that invites waits for a party of two, one that
//!    accepts waits to be in a party, any other goes at once) it asks the server for a debug preview of the scene, which
//!    moves it to the scene's field; the session answers the `AreaChange` as it always does.
//! 2. **Press** (the owner): when the scene's trigger is among the area's characters (a list that came after the preview was
//!    asked for) and the server knows where the bot stands, it presses OK next to it (`Interact` naming it), once `--scene-watchers N`
//!    other players are in view (or `--scene-wait-secs` after it could have pressed). A press that started nothing in five
//!    seconds is made again, at most three times.
//! 3. **Watch** (`--scene-watch`): it presses nothing; it waits for a `SceneStart` as a watcher. With `--scene-preview`
//!    too it previews first (that is how a party member ends up in its leader's preview instance).
//! 4. **Play**: the scene runs; the bot prints a line when a window is moved past (owner and watcher print the same lines),
//!    and a line when the server ends the run.
//! 5. **Control**: after the server's `SceneEnded` the bot sends ONE `MoveIntent` and says whether the server accepted it.
//!    The server answers a move it ignores (the player is still held) with nothing and an accepted one with the entity's
//!    new place in the next update, so `moved=true` is an update of our own entity at the place we asked for within
//!    [`PROBE_WAIT_S`] seconds, and anything else is `moved=false`.
//!
//! The bot stands still from the moment it asks for the preview (or from the start, when it only watches): it neither
//! walks its pattern into a place the press is not judged at, nor makes echoes of its own that would answer the probe.
//!
//! # Output
//!
//! One line per event on STDOUT, flushed, before the run's `SMOKE` line (the headless loop's one line on stdout is still the
//! last one; without a `--scene-*` flag stdout is unchanged). Ids and counts only, never a word of the game:
//!
//! ```text
//! scene preview requested scene=<id>
//! scene started run=<n> scene=<id> role=<Owner|Watcher> participants=<n>
//! scene line index=<i> opener=<entity> speaker=<entity|none> dialog=<n|-> asks=<true|false> answer=<line|none>
//! scene ended run=<n> outcome=<Finished|Skipped|TimedOut|Aborted> lines=<n>
//! scene control restored moved=<true|false>
//! scene refused reason=<Unavailable|UnknownScene|Busy|NotHere>
//! scene unavailable (cannot read this machine's field data)
//! ```
//!
//! `scene line` is printed when a window is MOVED PAST, by the owner and by a watcher alike, so that both print the same
//! lines (a watcher learns the answer only from the owner's step); `lines=` counts them. `dialog=-` is a line this project
//! added (a scene's lines are the game's own, so it is a number there).
//!
//! # Exit
//!
//! The bot is done when the control check is over, or when it failed; a run in which it was asked to play a scene and did not
//! see it finish (the scene was refused or unavailable here, did not start or end within `--scene-wait-secs`, ended in any way
//! but `Finished`, or control did not come back) exits non-zero, with one line on stderr that says why ([`SceneBot::failure`]).

use super::client::clean_id;
use super::player::SceneEvent;
use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::Pos;
use fmv_ff7::script::scene::scenes;
use fmv_protocol::{SceneOutcome, SceneRefusal, SceneRole, SCENE_NPC_ID_BASE};
use std::time::Duration;

/// How long the control check waits for the server's echo of its move, in seconds.
pub const PROBE_WAIT_S: f64 = 2.5;
/// A press that started nothing in this long is made again.
const REPRESS_AFTER_S: f64 = 5.0;
/// Presses at most.
const MAX_PRESSES: u32 = 3;
/// A scene's echo of the probe within this many units of where it was asked for counts as the probe's.
const ECHO_TOLERANCE: f32 = 1.5;

/// What `--scene-*` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneBotConfig {
    /// `--scene-preview`: the scene to ask a preview of.
    pub preview: Option<String>,
    /// `--scene-watch`: press nothing, wait for a scene to watch.
    pub watch: bool,
    /// `--scene-answer`: the line to answer questions with (the pointer's line when not given).
    pub answer: Option<usize>,
    /// `--scene-wait-secs`.
    pub wait: Duration,
    /// `--scene-watchers`: other players to see before the owner presses.
    pub watchers: usize,
    /// `--party-invite-first-peer`: the preview waits for a party of two.
    pub invite_first_peer: bool,
    /// `--party-accept`: the preview waits for a party.
    pub accept: bool,
}

/// What the bot looks at each tick.
#[derive(Debug, Clone, Copy)]
pub struct Senses<'a> {
    /// Seconds of the session's clock.
    pub now: f64,
    /// The area's characters.
    pub npcs: &'a [crate::state::NpcView],
    /// How many times the list of characters was replaced.
    pub npc_epoch: u64,
    /// Members of the party this player is in (0: none).
    pub party_size: usize,
    /// Other players in view.
    pub peers_in_view: usize,
    /// The server does not yet know how the player stands (an intent is owed).
    pub unsent: bool,
    /// How many updates of the player's own entity arrived, and the place the last one gave.
    pub echoes: u64,
    pub echo_at: Option<Pos>,
    /// How many `MoveRejected` arrived.
    pub rejected: u64,
}

/// What the bot wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Ask the server for a preview of this scene.
    Preview(String),
    /// Press OK on the character with this id (`Interact`).
    Press(u32),
    /// Send the one `MoveIntent` of the control check: the loop picks a place the area accepts and says which with
    /// [`SceneBot::probe_target`].
    Probe,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Not yet asked for the preview (waiting for the party it needs).
    Waiting,
    /// Asked for it; waiting for the area's characters to be replaced and the trigger to be among them.
    Previewed,
    /// The trigger is here; waiting to be known by the server and for the watchers to be in view.
    Ready { since: f64 },
    /// OK pressed (this many times, the last at `at`); waiting for the scene to start.
    Pressed { at: f64, presses: u32 },
    /// Only watching: waiting for a scene to start.
    Watching,
    /// The scene runs.
    Playing { since: f64 },
    /// The server ended the run; the move is not yet sent.
    Ended,
    /// The move was sent (or no place was found for it); waiting for the server's answer.
    Probing { since: f64, target: Option<Pos>, echoes: u64, rejected: u64 },
    Done,
}

/// The scene bot (see the module docs).
#[derive(Debug)]
pub struct SceneBot {
    cfg: SceneBotConfig,
    phase: Phase,
    /// When the bot first looked.
    began: Option<f64>,
    /// When it first pressed OK (the wait for the scene to start counts from it).
    first_press: Option<f64>,
    /// The index of the scene it asked a preview of in the game's list, when this machine knows it.
    wanted: Option<usize>,
    /// The NPC list epoch at the time the preview was asked for.
    epoch_at_preview: u64,
    lines: Vec<String>,
    outcome: Option<SceneOutcome>,
    moved: Option<bool>,
    failure: Option<String>,
}

/// A place a few steps from `from` that `area` accepts a move to: where the control check's one move goes (`None`: boxed in).
pub fn pick_probe(area: &dyn AreaRuntime, from: Pos) -> Option<Pos> {
    const DISTANCES: [f32; 3] = [12.0, 8.0, 5.0];
    const DIRECTIONS: [(f32, f32); 8] = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (0.7, 0.7), (-0.7, 0.7), (-0.7, -0.7), (0.7, -0.7)];
    for distance in DISTANCES {
        for (dx, dy) in DIRECTIONS {
            let to = Pos::new(from.x + dx * distance, from.y + dy * distance, from.z);
            if let MoveResult::Accepted(place) = area.validate_move(from, to) {
                if place.is_finite() && place.dist_xy(from) > ECHO_TOLERANCE + 0.5 {
                    return Some(place);
                }
            }
        }
    }
    None
}

fn word(role: SceneRole) -> &'static str {
    match role {
        SceneRole::Owner => "Owner",
        SceneRole::Watcher => "Watcher",
    }
}

fn outcome_word(outcome: SceneOutcome) -> &'static str {
    match outcome {
        SceneOutcome::Finished => "Finished",
        SceneOutcome::Skipped => "Skipped",
        SceneOutcome::TimedOut => "TimedOut",
        SceneOutcome::Aborted => "Aborted",
    }
}

fn refusal_word(reason: SceneRefusal) -> &'static str {
    match reason {
        SceneRefusal::Unavailable => "Unavailable",
        SceneRefusal::UnknownScene => "UnknownScene",
        SceneRefusal::Busy => "Busy",
        SceneRefusal::NotHere => "NotHere",
    }
}

impl SceneBot {
    pub fn new(cfg: SceneBotConfig) -> Self {
        let wanted = cfg.preview.as_deref().and_then(|id| scenes().iter().position(|s| s.id == id));
        let phase = if cfg.preview.is_some() { Phase::Waiting } else { Phase::Watching };
        Self { cfg, phase, began: None, first_press: None, wanted, epoch_at_preview: 0, lines: Vec::new(), outcome: None, moved: None, failure: None }
    }

    /// Whether a `--scene-*` flag asked for a scene.
    pub fn is_active(&self) -> bool {
        self.cfg.preview.is_some() || self.cfg.watch
    }

    /// The line a question is answered with.
    pub fn answer(&self) -> Option<usize> {
        self.cfg.answer
    }

    /// Whether the bot is finished (the control check is over, or it failed).
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Whether the bot stands still: from the moment it asked for the preview (or from the start, when it only watches), to
    /// the end (the tick in which it finishes does not walk either). A run with no scene flag never stands still for it.
    pub fn holds_still(&self) -> bool {
        self.is_active() && !matches!(self.phase, Phase::Waiting)
    }

    /// Whether the bot is in the control check, where the one move it makes is sent.
    pub fn probing(&self) -> bool {
        matches!(self.phase, Phase::Probing { .. } | Phase::Ended)
    }

    /// Why the bot failed, once it has (the run then exits non-zero).
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The lines to print on stdout since the last call.
    pub fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    fn line(&mut self, text: String) {
        self.lines.push(text);
    }

    fn fail(&mut self, why: impl Into<String>) {
        if self.failure.is_none() {
            self.failure = Some(why.into());
        }
        self.phase = Phase::Done;
    }

    /// Whether the party the preview waits for is there.
    fn gate_open(&self, s: &Senses<'_>) -> bool {
        match (self.cfg.invite_first_peer, self.cfg.accept) {
            (true, _) => s.party_size >= 2,
            (false, true) => s.party_size >= 1,
            (false, false) => true,
        }
    }

    /// The trigger of the scene wanted among `npcs`: the one of the scene's own index when this machine knows the scene,
    /// else the first scene trigger there is.
    fn trigger_of(&self, npcs: &[crate::state::NpcView]) -> Option<u32> {
        let mut triggers = npcs.iter().map(|n| n.id).filter(|id| *id >= SCENE_NPC_ID_BASE);
        match self.wanted {
            Some(index) => {
                let id = SCENE_NPC_ID_BASE.saturating_add(u32::try_from(index).ok()?);
                triggers.find(|t| *t == id)
            }
            None => triggers.next(),
        }
    }

    /// What happened in the scene client: lines to print, and where the bot goes next.
    pub fn on_event(&mut self, event: &SceneEvent, now: f64) {
        match event {
            SceneEvent::Started { run, scene, role, participants } => {
                self.line(format!("scene started run={} scene={} role={} participants={participants}", run.0, clean_id(scene), word(*role)));
                if !matches!(self.phase, Phase::Done) {
                    self.phase = Phase::Playing { since: now };
                }
            }
            SceneEvent::Line { index, opener, speaker, dialog, asks, answer } => {
                self.line(format!(
                    "scene line index={index} opener={opener} speaker={} dialog={} asks={asks} answer={}",
                    speaker.map_or_else(|| "none".to_string(), |s| s.to_string()),
                    dialog.map_or_else(|| "-".to_string(), |d| d.to_string()),
                    answer.map_or_else(|| "none".to_string(), |a| a.to_string()),
                ));
            }
            SceneEvent::Ended { run, outcome, lines } => {
                self.line(format!("scene ended run={} outcome={} lines={lines}", run.0, outcome_word(*outcome)));
                self.outcome = Some(*outcome);
                if !matches!(self.phase, Phase::Done) {
                    self.phase = Phase::Ended;
                }
            }
            SceneEvent::Refused(reason) => {
                self.line(format!("scene refused reason={}", refusal_word(*reason)));
                self.fail(format!("the server refused the scene ({})", refusal_word(*reason)));
            }
            SceneEvent::Unavailable => {
                self.line("scene unavailable (cannot read this machine's field data)".to_string());
                self.fail("this machine cannot play the scene (no readable field data for it)");
            }
        }
    }

    /// The loop picked the place of the control check (`None`: the area accepts no step from where the bot stands).
    pub fn probe_target(&mut self, target: Option<Pos>, s: &Senses<'_>) {
        if matches!(self.phase, Phase::Ended) {
            self.phase = Phase::Probing { since: s.now, target, echoes: s.echoes, rejected: s.rejected };
        }
    }

    /// One tick: what the bot wants done now.
    pub fn step(&mut self, s: &Senses<'_>) -> Vec<Act> {
        let began = *self.began.get_or_insert(s.now);
        let wait = self.cfg.wait.as_secs_f64();
        let mut acts = Vec::new();
        match self.phase {
            Phase::Waiting => {
                if let (true, Some(id)) = (self.gate_open(s), self.cfg.preview.clone()) {
                    self.line(format!("scene preview requested scene={}", clean_id(&id)));
                    acts.push(Act::Preview(id));
                    self.epoch_at_preview = s.npc_epoch;
                    self.phase = if self.cfg.watch { Phase::Watching } else { Phase::Previewed };
                }
            }
            Phase::Previewed => {
                if s.npc_epoch != self.epoch_at_preview && self.trigger_of(s.npcs).is_some() {
                    self.phase = Phase::Ready { since: s.now };
                }
            }
            Phase::Ready { since } => {
                let others = s.peers_in_view >= self.cfg.watchers || s.now - since >= wait;
                if let (true, true, Some(id)) = (!s.unsent, others, self.trigger_of(s.npcs)) {
                    acts.push(Act::Press(id));
                    self.first_press = Some(s.now);
                    self.phase = Phase::Pressed { at: s.now, presses: 1 };
                }
            }
            Phase::Pressed { at, presses } => {
                if s.now - at >= REPRESS_AFTER_S && presses < MAX_PRESSES && !s.unsent {
                    if let Some(id) = self.trigger_of(s.npcs) {
                        acts.push(Act::Press(id));
                        self.phase = Phase::Pressed { at: s.now, presses: presses + 1 };
                    }
                }
            }
            Phase::Ended => acts.push(Act::Probe),
            Phase::Probing { since, target, echoes, rejected } => {
                let answered = target.is_some_and(|t| s.echoes > echoes && s.echo_at.is_some_and(|at| at.dist_xy(t) <= ECHO_TOLERANCE));
                let refused = s.rejected > rejected;
                if answered || refused || target.is_none() || s.now - since >= PROBE_WAIT_S {
                    self.moved = Some(answered);
                    self.line(format!("scene control restored moved={answered}"));
                    match (self.outcome, answered) {
                        (Some(SceneOutcome::Finished), true) => self.phase = Phase::Done,
                        (Some(SceneOutcome::Finished), false) => self.fail("control did not come back after the scene (the server ignored the move)"),
                        (Some(other), _) => self.fail(format!("the scene ended {} and not Finished", outcome_word(other))),
                        (None, _) => self.fail("no scene ended"),
                    }
                }
            }
            Phase::Watching | Phase::Playing { .. } | Phase::Done => {}
        }
        // The deadlines, each `--scene-wait-secs`: to get to the trigger (the party, the preview, the arrival) or, for a watcher, to be
        // given a scene; from the first press to the scene starting; from its start to its end. (The wait for the owner's watchers
        // is its own: it ends with a press, never with a failure.)
        match self.phase {
            Phase::Waiting | Phase::Previewed | Phase::Watching if s.now - began > wait => self.fail(format!("no scene started within {wait:.0} s")),
            Phase::Pressed { .. } if s.now - self.first_press.unwrap_or(began) > wait => self.fail(format!("no scene started within {wait:.0} s of the press")),
            Phase::Playing { since } if s.now - since > wait => self.fail(format!("the scene did not end within {wait:.0} s")),
            _ => {}
        }
        acts
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::state::NpcView;
    use fmv_protocol::SceneRunId;

    pub fn config() -> SceneBotConfig {
        SceneBotConfig { preview: Some("ff7/mds7pb_1/tifa-bar".into()), watch: false, answer: None, wait: Duration::from_secs(60), watchers: 0, invite_first_peer: false, accept: false }
    }

    pub fn npc(id: u32) -> NpcView {
        NpcView { id, name: "tifa".into(), model: "aagb".into(), idle_animation: None, pos: Pos::new(0.0, 0.0, 0.0), facing: 0.0 }
    }

    fn senses<'a>(now: f64, npcs: &'a [NpcView]) -> Senses<'a> {
        Senses { now, npcs, npc_epoch: 1, party_size: 0, peers_in_view: 0, unsent: false, echoes: 0, echo_at: None, rejected: 0 }
    }

    #[test]
    fn the_party_flags_decide_when_the_preview_is_asked() {
        let mut alone = SceneBot::new(config());
        assert_eq!(alone.step(&senses(0.0, &[])), [Act::Preview("ff7/mds7pb_1/tifa-bar".into())], "no party flag: at once");
        assert!(alone.holds_still());
        // A bot that invites waits for a party of two.
        let mut leader = SceneBot::new(SceneBotConfig { invite_first_peer: true, ..config() });
        let mut s = senses(0.0, &[]);
        assert!(leader.step(&s).is_empty() && !leader.holds_still(), "no party yet");
        s.party_size = 1;
        assert!(leader.step(&s).is_empty(), "a party of one is not enough to invite into");
        s.party_size = 2;
        assert_eq!(leader.step(&s).len(), 1);
        // A bot that accepts waits to be in a party.
        let mut member = SceneBot::new(SceneBotConfig { accept: true, ..config() });
        let mut s = senses(0.0, &[]);
        assert!(member.step(&s).is_empty());
        s.party_size = 1;
        assert_eq!(member.step(&s).len(), 1);
        assert_eq!(member.take_lines(), ["scene preview requested scene=ff7/mds7pb_1/tifa-bar"]);
    }

    #[test]
    fn the_owner_presses_once_the_trigger_is_in_a_list_that_came_after_the_preview_and_the_server_knows_where_it_stands() {
        let mut bot = SceneBot::new(config());
        let trigger = [npc(SCENE_NPC_ID_BASE)];
        let mut s = senses(0.0, &trigger);
        s.npc_epoch = 4;
        bot.step(&s); // preview asked at epoch 4
        // The same list (an old snapshot that merely already holds the trigger) is not the preview's arrival.
        assert!(bot.step(&s).is_empty());
        s.npc_epoch = 5;
        assert!(bot.step(&s).is_empty(), "now it is ready, but has not pressed yet");
        s.unsent = true;
        assert!(bot.step(&s).is_empty(), "the server does not know where it stands yet");
        s.unsent = false;
        assert_eq!(bot.step(&s), [Act::Press(SCENE_NPC_ID_BASE)]);
        assert!(bot.step(&s).is_empty(), "once");
    }

    #[test]
    fn the_owner_waits_for_its_watchers_but_only_so_long() {
        let mut bot = SceneBot::new(SceneBotConfig { watchers: 2, wait: Duration::from_secs(30), ..config() });
        let trigger = [npc(SCENE_NPC_ID_BASE)];
        let mut s = senses(0.0, &trigger);
        s.npc_epoch = 1;
        bot.step(&s);
        s.npc_epoch = 2;
        bot.step(&s); // ready since 0.0
        s.peers_in_view = 1;
        s.now = 10.0;
        assert!(bot.step(&s).is_empty(), "one of two is in view");
        s.peers_in_view = 2;
        assert_eq!(bot.step(&s), [Act::Press(SCENE_NPC_ID_BASE)], "both are in view");
        // Nobody comes: it presses anyway when the wait is up (the run will show the shortfall).
        let mut bot = SceneBot::new(SceneBotConfig { watchers: 2, wait: Duration::from_secs(30), ..config() });
        let mut s = senses(0.0, &trigger);
        s.npc_epoch = 1;
        bot.step(&s);
        s.npc_epoch = 2;
        s.now = 1.0;
        bot.step(&s);
        s.now = 20.0;
        assert!(bot.step(&s).is_empty());
        s.now = 31.0;
        assert_eq!(bot.step(&s), [Act::Press(SCENE_NPC_ID_BASE)]);
        assert!(!bot.is_done() && bot.failure().is_none(), "the wait for watchers ends with a press, never with a failure");
        // The press has its own wait for the scene to start: from the press, not from the start of the run.
        s.now = 31.0 + 29.0;
        assert!(bot.step(&s).iter().all(|a| !matches!(a, Act::Probe)) && !bot.is_done());
        s.now = 31.0 + 31.0;
        bot.step(&s);
        assert!(bot.is_done() && bot.failure().is_some_and(|f| f.contains("of the press")));
    }

    #[test]
    fn a_press_that_starts_nothing_is_made_again_at_most_three_times() {
        let mut bot = SceneBot::new(config());
        let trigger = [npc(SCENE_NPC_ID_BASE)];
        let mut s = senses(0.0, &trigger);
        s.npc_epoch = 1;
        bot.step(&s);
        s.npc_epoch = 2;
        bot.step(&s);
        assert_eq!(bot.step(&s).len(), 1);
        s.now = 4.0;
        assert!(bot.step(&s).is_empty());
        s.now = 5.5;
        assert_eq!(bot.step(&s).len(), 1, "the second");
        s.now = 11.0;
        assert_eq!(bot.step(&s).len(), 1, "the third");
        s.now = 17.0;
        assert!(bot.step(&s).is_empty(), "no more than three");
    }

    #[test]
    fn a_watcher_presses_nothing_and_waits_for_a_scene_with_or_without_a_preview() {
        let mut only = SceneBot::new(SceneBotConfig { preview: None, watch: true, ..config() });
        assert!(only.is_active() && only.holds_still());
        let trigger = [npc(SCENE_NPC_ID_BASE)];
        let mut s = senses(0.0, &trigger);
        assert!(only.step(&s).is_empty());
        s.npc_epoch = 9;
        assert!(only.step(&s).is_empty(), "it never presses");
        // With a preview it asks first (when its party allows), then watches.
        let mut previewing = SceneBot::new(SceneBotConfig { watch: true, accept: true, ..config() });
        let mut s = senses(0.0, &trigger);
        assert!(previewing.step(&s).is_empty());
        s.party_size = 1;
        assert_eq!(previewing.step(&s), [Act::Preview("ff7/mds7pb_1/tifa-bar".into())]);
        s.npc_epoch = 7;
        assert!(previewing.step(&s).is_empty(), "and never presses");
    }

    #[test]
    fn the_lines_have_the_documented_format_and_say_ids_and_counts_only() {
        let mut bot = SceneBot::new(config());
        bot.on_event(&SceneEvent::Started { run: SceneRunId(7), scene: "ff7/mds7pb_1/tifa-bar".into(), role: SceneRole::Owner, participants: 2 }, 1.0);
        bot.on_event(&SceneEvent::Line { index: 0, opener: 20, speaker: Some(20), dialog: Some(12), asks: false, answer: None }, 1.0);
        bot.on_event(&SceneEvent::Line { index: 1, opener: 18, speaker: None, dialog: None, asks: true, answer: Some(3) }, 1.0);
        bot.on_event(&SceneEvent::Ended { run: SceneRunId(7), outcome: SceneOutcome::Finished, lines: 2 }, 2.0);
        assert_eq!(
            bot.take_lines(),
            [
                "scene started run=7 scene=ff7/mds7pb_1/tifa-bar role=Owner participants=2",
                "scene line index=0 opener=20 speaker=20 dialog=12 asks=false answer=none",
                "scene line index=1 opener=18 speaker=none dialog=- asks=true answer=3",
                "scene ended run=7 outcome=Finished lines=2",
            ]
        );
        let mut bot = SceneBot::new(config());
        bot.on_event(&SceneEvent::Refused(SceneRefusal::NotHere), 0.0);
        assert_eq!(bot.take_lines(), ["scene refused reason=NotHere"]);
        assert!(bot.is_done() && bot.failure().is_some());
        let mut bot = SceneBot::new(config());
        bot.on_event(&SceneEvent::Unavailable, 0.0);
        assert_eq!(bot.take_lines(), ["scene unavailable (cannot read this machine's field data)"]);
        assert!(bot.is_done() && bot.failure().is_some());
    }

    #[test]
    fn a_scene_id_from_the_network_is_printed_clean() {
        let mut bot = SceneBot::new(config());
        bot.on_event(&SceneEvent::Started { run: SceneRunId(1), scene: "a b\u{1b}[31m".into(), role: SceneRole::Watcher, participants: 1 }, 0.0);
        assert_eq!(bot.take_lines(), ["scene started run=1 scene=a?b??31m role=Watcher participants=1"]);
    }

    /// A bot whose scene is over, ready to probe.
    fn ended(outcome: SceneOutcome) -> SceneBot {
        let mut bot = SceneBot::new(config());
        bot.step(&senses(0.0, &[]));
        bot.on_event(&SceneEvent::Started { run: SceneRunId(1), scene: "s".into(), role: SceneRole::Owner, participants: 1 }, 0.0);
        bot.on_event(&SceneEvent::Ended { run: SceneRunId(1), outcome, lines: 0 }, 1.0);
        bot.take_lines();
        bot
    }

    #[test]
    fn after_the_end_it_probes_once_and_reports_the_servers_echo() {
        let mut bot = ended(SceneOutcome::Finished);
        assert!(bot.probing());
        let mut s = senses(2.0, &[]);
        assert_eq!(bot.step(&s), [Act::Probe]);
        bot.probe_target(Some(Pos::new(5.0, 5.0, 0.0)), &s);
        assert!(bot.step(&s).is_empty(), "the probe is asked once");
        // An echo of somebody else's place is not the probe's.
        s.echoes = 1;
        s.echo_at = Some(Pos::new(90.0, 90.0, 0.0));
        s.now = 3.0;
        assert!(bot.step(&s).is_empty() && !bot.is_done());
        s.echoes = 2;
        s.echo_at = Some(Pos::new(5.2, 5.1, 0.0));
        bot.step(&s);
        assert_eq!(bot.take_lines(), ["scene control restored moved=true"]);
        assert!(bot.is_done() && bot.failure().is_none());
    }

    #[test]
    fn silence_or_a_rejection_after_the_probe_is_control_not_restored_and_fails_the_run() {
        for rejected in [false, true] {
            let mut bot = ended(SceneOutcome::Finished);
            let mut s = senses(2.0, &[]);
            bot.step(&s);
            bot.probe_target(Some(Pos::new(5.0, 5.0, 0.0)), &s);
            if rejected {
                s.rejected = 1;
                bot.step(&s);
            } else {
                s.now = 2.0 + PROBE_WAIT_S + 0.1;
                bot.step(&s);
            }
            assert_eq!(bot.take_lines(), ["scene control restored moved=false"], "rejected: {rejected}");
            assert!(bot.is_done() && bot.failure().is_some_and(|f| f.contains("control")));
        }
        // No place to step to is no proof of control either.
        let mut bot = ended(SceneOutcome::Finished);
        let s = senses(2.0, &[]);
        bot.step(&s);
        bot.probe_target(None, &s);
        bot.step(&s);
        assert_eq!(bot.take_lines(), ["scene control restored moved=false"]);
        assert!(bot.failure().is_some());
    }

    #[test]
    fn a_scene_that_did_not_finish_fails_the_run_even_when_control_came_back() {
        for outcome in [SceneOutcome::Skipped, SceneOutcome::TimedOut, SceneOutcome::Aborted] {
            let mut bot = ended(outcome);
            let mut s = senses(2.0, &[]);
            bot.step(&s);
            bot.probe_target(Some(Pos::new(5.0, 5.0, 0.0)), &s);
            s.echoes = 1;
            s.echo_at = Some(Pos::new(5.0, 5.0, 0.0));
            bot.step(&s);
            assert!(bot.is_done(), "{outcome:?}");
            assert!(bot.failure().is_some_and(|f| f.contains("not Finished")), "{outcome:?}");
        }
    }

    #[test]
    fn a_scene_that_never_starts_or_never_ends_fails_after_the_wait() {
        let mut bot = SceneBot::new(SceneBotConfig { wait: Duration::from_secs(10), ..config() });
        let mut s = senses(0.0, &[]);
        bot.step(&s);
        s.now = 11.0;
        bot.step(&s);
        assert!(bot.is_done() && bot.failure().is_some_and(|f| f.contains("no scene started")));
        let mut bot = SceneBot::new(SceneBotConfig { preview: None, watch: true, wait: Duration::from_secs(10), ..config() });
        let mut s = senses(0.0, &[]);
        bot.step(&s);
        bot.on_event(&SceneEvent::Started { run: SceneRunId(1), scene: "s".into(), role: SceneRole::Watcher, participants: 1 }, 5.0);
        s.now = 14.0;
        bot.step(&s);
        assert!(!bot.is_done(), "the wait for the end counts from the start");
        s.now = 16.0;
        bot.step(&s);
        assert!(bot.is_done() && bot.failure().is_some_and(|f| f.contains("did not end")));
    }

    #[test]
    fn it_stands_still_from_the_preview_on_to_the_very_end_and_not_before_and_never_without_a_flag() {
        let mut bot = SceneBot::new(SceneBotConfig { accept: true, ..config() });
        assert!(!bot.holds_still());
        let mut s = senses(0.0, &[]);
        s.party_size = 1;
        bot.step(&s);
        assert!(bot.holds_still());
        bot.on_event(&SceneEvent::Unavailable, 1.0);
        assert!(bot.is_done() && bot.holds_still(), "the tick that finishes it does not walk either");
        assert!(SceneBot::new(SceneBotConfig { preview: None, watch: true, ..config() }).holds_still());
        // No scene flag: a bot that is not asked for a scene does not stand still for one (every other bot walks as it did).
        let idle = SceneBot::new(SceneBotConfig { preview: None, watch: false, ..config() });
        assert!(!idle.is_active() && !idle.holds_still());
    }
}
