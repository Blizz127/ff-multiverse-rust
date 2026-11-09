//! The local player's story progress (protocol `StoryUpdate`) and the bot that advances it.
//!
//! # The mirror
//!
//! The server keeps an account's story (one opaque blob per game, read only by the game's adapter) and
//! decides every step; [`StoryMirror`] only remembers the last blob it was told, says what changed as
//! one-line notes (`story:` on stderr in a headless run) and offers the blob to whatever needs the
//! story variables on the client (the dialogue lane reads [`StoryMirror::progress`] through the same
//! adapter that decoded it). A client that is never told anything is a new account: the blob is empty.
//!
//! # The bot (`--story-advance`)
//!
//! [`StoryBot`] walks to the story point of the area it stands in that would advance THIS player's
//! story (the same points and the same rule the server uses, from the client's own copy of the field:
//! `AreaRuntime::story_points` and `AreaRuntime::story_advance` on the progress it was told), and keeps
//! to that spot until the server has moved the story: a triangle or region it stands in, a line it walks
//! across (it aims a little past the middle of the line, so that the move really crosses it). The points
//! that fire when the player comes to stand in an area need no walking and are left to the server. It never
//! crosses an exit (that would take it to another area) and never walks off the walkable surface. The
//! server decides, as always: the bot only walks where the data says the story happens.
//!
//! Three more kinds of point need more than walking:
//!
//! * **A press** ([`StoryTrigger::Press`]): the elevator's panel. The bot walks into the region, stands still there and
//!   wants OK pressed ([`StoryBot::take_press`]: the caller sends `ClientMsg::Interact` when the server knows where the
//!   bot stands), and waits for the server's step; a press that brings no step is made again, a few times.
//! * **A talk** ([`StoryTrigger::Talk`]): a character whose talk moves the story. The point names the place the character
//!   stands; the bot finds the character of the area's snapshot that stands there ([`StoryBot::see_npcs`]), walks up to
//!   within a part of the point's radius of it and presses OK addressing it (`Interact { npc: Some(id) }`).
//! * **A battle first** (`AreaRuntime::story_battle`): the party fights before the step. The bot walks to the point like
//!   to any region, and last (the points that start no battle come first); a bot that leads a party stands before it until
//!   the others are in view ([`StoryBot::see_party`]), at most [`BATTLE_PARTY_WAIT_S`]. The server starts the battle when
//!   a member comes to the point from outside, the bots' battle code fights it, and the step comes when it is won: until
//!   then the bot stays (a lost battle is fought again by stepping out of the region and in).
//!
//! In an area with links (ladders, jumps: `AreaRuntime::links`) the bot walks with a [`crate::route::Navigator`], which
//! takes the links it needs to get to the point and keeps off the others.

use std::collections::VecDeque;
use std::sync::Arc;

use fmv_adapter_api::{AreaKey, AreaRuntime, Exit, ExitState, GameAdapter, Link, MoveResult, StoryPoint, StoryTrigger, SAME_SPOT};
use fmv_core::Pos;
use fmv_protocol::StoryAdvance;

use crate::bot::BotStep;
use crate::nav::plan_path_avoiding;
use crate::route::{goal_key, Nav, Navigator, Press};
use crate::state::NpcView;

/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 32;
/// Longest progression blob the mirror keeps (the server keeps at most 256 bytes of one).
pub const MAX_PROGRESS_BYTES: usize = 256;
/// Longest summary the notes repeat (characters).
const MAX_SUMMARY_CHARS: usize = 48;

/// What the server last said about our story.
#[derive(Debug, Default)]
pub struct StoryMirror {
    progress: Vec<u8>,
    told: bool,
    advances: u32,
    notes: VecDeque<String>,
}

impl StoryMirror {
    /// The server told us where our story stands (`advance` set when this follows a step).
    pub fn on_update(&mut self, progress: Vec<u8>, advance: Option<StoryAdvance>) {
        if progress.len() > MAX_PROGRESS_BYTES {
            self.note("ignored a story update that is far too long for any game's progression".to_string());
            return;
        }
        let hex: String = progress.iter().map(|b| format!("{b:02x}")).collect();
        match advance {
            Some(advance) => {
                self.advances += 1;
                let summary: String = crate::net::clean(&advance.summary).chars().take(MAX_SUMMARY_CHARS).collect();
                self.note(format!("advanced point={} {summary} (progress {hex})", advance.point));
            }
            None => self.note(format!("joined with progress {hex}")),
        }
        self.progress = progress;
        self.told = true;
    }

    /// The progression blob as last told (empty until told: a new account).
    pub fn progress(&self) -> &[u8] {
        &self.progress
    }

    /// The FF7 story counter (`Var[2][0]`, the "game moment") for whatever reads script variables on this
    /// side: a new account's is [`fmv_ff7::story::NEW_GAME_MOMENT`]. `None` when the blob is not FF7's
    /// (a progression of another build or game is never guessed at). The hook the dialogue lane feeds its
    /// script variables from; it changes only when the server sends a `StoryUpdate`.
    #[allow(dead_code)] // read by the dialogue lane, not by this crate yet
    pub fn moment(&self) -> Option<u16> {
        fmv_ff7::story::StoryState::decode(&self.progress).map(|s| s.moment)
    }

    /// The whole FF7 story state (the moment and the variables beyond it) for the dialogue lane, as the story model
    /// reads the progress the server told us; `None` when the blob is not FF7's.
    pub fn state(&self) -> Option<fmv_ff7::story::StoryState> {
        fmv_ff7::story::StoryState::decode(&self.progress)
    }

    /// Whether the server ever sent a `StoryUpdate` (a new account's session never gets one until it advances).
    #[cfg(test)]
    pub fn told(&self) -> bool {
        self.told
    }

    /// How many steps the server reported.
    pub fn advances(&self) -> u32 {
        self.advances
    }

    /// The notes since the last call, oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }
}

/// A bot that holds still until its own invite went out waits at most this long (seconds).
pub const INVITE_WAIT_S: f64 = 15.0;
/// How far from the middle of the region a bot that is standing in it steps to touch it again.
const NUDGE: f32 = 12.0;
/// How far past a line the bot aims, tried in this order until one of them is walkable.
const LINE_OVERSHOOTS: [f32; 4] = [24.0, 12.0, 6.0, 3.0];
/// Headings tried in a step, as degrees away from the straight line to the goal, best first.
const FANOUT_DEG: [f32; 11] = [0.0, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0, 60.0, -60.0, 90.0, -90.0];
/// Steps without headway after which the bot gives up (at 100 steps per second: 5 s).
const STUCK_LIMIT: u32 = 500;
/// Steps to wait before planning again after a plan failed.
const PLAN_COOLDOWN_STEPS: u32 = 100;
/// A bot that leads a party holds still before a point that starts a battle until the rest of the party is in view, but
/// at most this long (seconds).
pub const BATTLE_PARTY_WAIT_S: f32 = 20.0;
/// A press is made ONCE: a bot that has pressed OK waits for the server's step (the `StoryUpdate` that changes the
/// progress it was pressed on) and presses nothing else before it comes. A press that brings no step in this long
/// (seconds) is a failure the bot reports and stops on, not one it repeats: a second press on a story the server
/// has already moved (the lift's panel, which rides back up) would be a different step.
const PRESS_PATIENCE_S: f32 = 10.0;
/// A bot presses OK next to a character standing within this part of the point's talk radius of it (the server's own
/// judgement is made on where it last heard the player was).
const TALK_STAND: f32 = 0.7;

/// What kind of place a story point is for the bot.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// A region or a line to walk over: touching it is all it takes.
    Walk,
    /// OK pressed standing in a region.
    Press,
    /// OK pressed next to a character: its id (`NpcView::id`, the index in the area's list), where it stands and how
    /// near the point says one has to be.
    Talk { npc: u32, centre: Pos, radius: f32 },
}

/// The point the bot goes for.
#[derive(Debug, Clone)]
struct Target {
    point: StoryPoint,
    kind: Kind,
    /// The point starts a battle first (the step waits for the win): the party has to be there.
    battle: bool,
}

/// What the bot has done about pressing OK at the point it stands at.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pressing {
    /// Nothing yet.
    No,
    /// At the spot, still: the press is wanted ([`StoryBot::take_press`]).
    Wanted,
    /// Pressed at this time (the session clock of the bot's own steps): waiting for the server's step, which is the
    /// progress changing from [`StoryBot::sent_progress`]. Nothing is pressed again until then.
    Sent { since: f32 },
}

/// How the bot tells a step that goes FORWARD from one that does not: the game's own join of two progressions
/// (`GameAdapter::merge_progression`). A step is forward when the join of the story the player has and the step's is
/// something new (a further moment, bits turned on); a step the join adds nothing to (the lift's ride back, which turns
/// a bit off) is not, and the bot never pursues it.
#[derive(Clone)]
struct Judge(Arc<dyn GameAdapter>);

impl std::fmt::Debug for Judge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Judge").finish_non_exhaustive()
    }
}

/// The planned way to a story point.
#[derive(Debug, Clone)]
struct Plan {
    area: AreaKey,
    point: u32,
    waypoints: Vec<Pos>,
    next: usize,
}

/// What `--story-advance` does (see the module docs). A state machine over the area's own story
/// points, exits and `validate_move`; nothing in here touches the network or the clock.
#[derive(Debug)]
pub struct StoryBot {
    requested: bool,
    /// Hold still until an invite of ours went out (the bot also invites): see [`INVITE_WAIT_S`].
    wait_for_invite: bool,
    plan: Option<Plan>,
    plan_cooldown: u32,
    stuck: u32,
    gave_up: Option<String>,
    /// How many steps of nudging a bot that stands in the region has done (it touches the region again
    /// by stepping a little way and back).
    nudges: u32,
    /// How the bot gets about in an area with links (ladders, jumps): see [`crate::route`]. Unused in any other.
    nav: Navigator,
    /// The characters of the area as its snapshot named them (id and place): what a point that waits for a talk is
    /// matched with. Told by [`StoryBot::see_npcs`].
    npcs: Vec<(u32, Pos)>,
    /// The size of the party and the number of other players in view: what a bot that leads a party looks at before it
    /// steps onto a point that starts a battle. Told by [`StoryBot::see_party`].
    party: (usize, usize),
    /// Seconds of the bot's own steps (what a press is timed with).
    clock: f32,
    /// Seconds the bot has held still before a battle point for the rest of its party.
    party_wait: f32,
    pressing: Pressing,
    /// The character the press wanted now addresses.
    press_npc: Option<u32>,
    /// The point the press state is about.
    press_point: Option<u32>,
    /// The progress the press was wanted on: the press has its step when the progress is another.
    sent_progress: Vec<u8>,
    /// Tells forward steps from the others (none: every step the area offers is pursued).
    judge: Option<Judge>,
    /// The area the bot's route goes to next, when it travels as well (see [`StoryBot::set_wanted_exit`]).
    wanted_exit: Option<String>,
}

/// How the step of a story point bears on the exit a travelling bot is heading for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bearing {
    /// The exit is closed for the story the bot has and open for the one the step makes.
    Opens,
    /// The exit is open for the story the bot has and closed for the one the step makes.
    Closes,
    /// The step changes nothing about it (or there is no such exit).
    Neither,
}

impl StoryBot {
    pub fn new(requested: bool, wait_for_invite: bool) -> Self {
        Self {
            requested,
            wait_for_invite,
            plan: None,
            plan_cooldown: 0,
            stuck: 0,
            gave_up: None,
            nudges: 0,
            nav: Navigator::new(),
            npcs: Vec::new(),
            party: (0, 0),
            clock: 0.0,
            party_wait: 0.0,
            pressing: Pressing::No,
            press_npc: None,
            press_point: None,
            sent_progress: Vec::new(),
            judge: None,
            wanted_exit: None,
        }
    }

    /// The area the bot's route leads to next (`--travel`), when it travels as well as advances the story: the exit to
    /// it changes what the bot pursues. The lift of the first reactor is the case. Going UP, the exit to `nmkin_1` is
    /// closed while the lift is down, and the panel's ride back (a step that is not forward: the story only moves forward)
    /// opens it, so that press is pursued; once the lift is up, the ride DOWN is a step forward again (the lift bit on)
    /// but it would close the exit the bot is heading for, so it is not pursued, and the bot does not ride up and down for
    /// ever. Going down the same ride is forward AND opens the exit to `nmkin_2`. `None`: no route, and every step that
    /// goes forward is pursued, as ever.
    pub fn set_wanted_exit(&mut self, target: Option<&str>) {
        if self.wanted_exit.as_deref() != target {
            self.wanted_exit = target.map(str::to_string);
        }
    }

    /// How the step `step` (the progression a point would make of `progress`) bears on the exits of `area` that lead to the
    /// area the bot's route goes to next: whether it opens or closes them, or neither.
    fn bearing(&self, area: &dyn AreaRuntime, progress: &[u8], step: &[u8]) -> Bearing {
        let Some(want) = self.wanted_exit.as_deref() else { return Bearing::Neither };
        let exits: Vec<Exit> = area.exits().into_iter().filter(|e| e.target.eq_ignore_ascii_case(want)).collect();
        if exits.is_empty() {
            return Bearing::Neither;
        }
        let open = |story: &[u8]| exits.iter().any(|e| area.exit_state(e, story) == ExitState::Open);
        match (open(progress), open(step)) {
            (false, true) => Bearing::Opens,
            (true, false) => Bearing::Closes,
            _ => Bearing::Neither,
        }
    }

    /// The game's own content, to tell a step that goes forward from one that does not: the bot pursues and presses
    /// only the points whose step is forward of the story it was told (`merge_progression` of the story and the step
    /// is something new). Without it every step the area offers is pursued.
    pub fn with_adapter(mut self, adapter: Option<Arc<dyn GameAdapter>>) -> Self {
        self.judge = adapter.map(Judge);
        self
    }

    /// Is `step` (the progression a point would give) forward of `current` (the one the player has)?
    fn is_forward(&self, current: &[u8], step: &[u8]) -> bool {
        self.judge.as_ref().is_none_or(|judge| judge.0.merge_progression(current, step) != current)
    }

    /// `--story-advance` was given: the bot decides every step, even when there is nothing to do
    /// (it stands still then, instead of walking the pattern).
    pub fn is_requested(&self) -> bool {
        self.requested
    }

    /// The session entered another area: what the bot learned about the last one (a plan, how long it was stuck,
    /// that it gave up) is of no use in this one. A bot that travels meets a new area's story points with a clean
    /// slate; one stuck moment in a field must not turn the story off for the rest of the run.
    pub fn arrived(&mut self) {
        self.plan = None;
        self.plan_cooldown = 0;
        self.stuck = 0;
        self.gave_up = None;
        self.nudges = 0;
        self.nav.arrived();
        self.npcs.clear();
        self.pressing = Pressing::No;
        self.press_npc = None;
        self.press_point = None;
        self.party_wait = 0.0;
    }

    pub fn gave_up(&self) -> Option<&str> {
        self.gave_up.as_deref()
    }

    /// May the bot walk? It holds still until its invite went out when it was told to wait for it
    /// (`invited`), but at most [`INVITE_WAIT_S`] (`now` is seconds on the session clock).
    pub fn may_walk(&self, now: f64, invited: bool) -> bool {
        self.requested && self.gave_up.is_none() && !(self.wait_for_invite && !invited && now < INVITE_WAIT_S)
    }

    /// The characters the area's snapshot named: where each stands (tell it whenever the list changes).
    pub fn see_npcs(&mut self, npcs: &[NpcView]) {
        self.npcs = npcs.iter().map(|n| (n.id, n.pos)).collect();
    }

    /// The size of the party the bot is in (0: none) and how many other players it sees now: before a point that starts
    /// a battle a bot that leads a party waits for the others (at most [`BATTLE_PARTY_WAIT_S`]).
    pub fn see_party(&mut self, party_size: usize, in_view: usize) {
        self.party = (party_size, in_view);
    }

    /// Look at where the walker is (every tick, whoever moved it): an area with links keeps the arming of its walk-on
    /// links from what it sees (see [`crate::route::Navigator::observe`]).
    pub fn observe(&mut self, area: &dyn AreaRuntime, cur: Pos) {
        if self.nav.has_links(area) {
            self.nav.observe(cur);
        }
    }

    /// The point of `area` that the bot goes for: one that would advance an account whose progress is `progress`, and
    /// that the bot can reach the way the point asks: a region or a line it walks over, a region where it presses OK, a
    /// character that the area has announced and that it talks to. The points that start a battle come last (the party
    /// is better off with its story done first), the nearest to `from` first.
    fn target(&self, area: &dyn AreaRuntime, progress: &[u8], from: Pos) -> Option<Target> {
        area.story_points()
            .into_iter()
            .filter_map(|point| {
                let kind = match &point.trigger {
                    StoryTrigger::Region(_) | StoryTrigger::Line(..) => Kind::Walk,
                    StoryTrigger::Press(_) => Kind::Press,
                    StoryTrigger::Talk { center, radius } => {
                        let npc = self.npcs.iter().find(|(_, pos)| pos.dist_xy(*center) <= SAME_SPOT).map(|(id, _)| *id)?;
                        Kind::Talk { npc, centre: *center, radius: *radius }
                    }
                    // what fires when the player comes to stand in the area needs no walking
                    _ => return None,
                };
                // only a step that goes forward (the lift's ride back would take the story backwards), unless it opens the
                // way the bot is going (the ride back is what lets it go up), and never one that would close that way
                // (the ride down, once the lift is up: the bot would ride up and down for ever)
                let step = area.story_advance(progress, point.id)?;
                match self.bearing(area, progress, &step.progress) {
                    Bearing::Closes => return None,
                    Bearing::Opens => {}
                    Bearing::Neither => {
                        if !self.is_forward(progress, &step.progress) {
                            return None;
                        }
                    }
                }
                let battle = area.story_battle(point.id).is_some();
                Some(Target { point, kind, battle })
            })
            .min_by(|a, b| a.battle.cmp(&b.battle).then_with(|| a.point.center().dist_xy(from).total_cmp(&b.point.center().dist_xy(from))))
    }

    /// Whether there is a story point to walk to that would advance `progress` (and the bot has not given up):
    /// what a bot that also travels asks, to do the story of an area before it leaves it.
    pub fn wants_to_walk(&self, area: &dyn AreaRuntime, progress: &[u8], from: Pos) -> bool {
        self.requested && self.gave_up.is_none() && self.target(area, progress, from).is_some()
    }

    /// Where to walk for `point`: the middle of a region, and for a line a point just past its middle on the side
    /// the bot is not on, so that the step that gets there crosses it (a step that ended exactly on the line, or
    /// short of it, would not).
    fn goal(area: &dyn AreaRuntime, point: &StoryPoint, from: Pos) -> Pos {
        let centre = point.center();
        let StoryTrigger::Line(a, b) = &point.trigger else { return centre };
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy);
        if !(len.is_finite() && len > 1e-3) {
            return centre;
        }
        // the unit normal of the line, and which side of it the bot stands on
        let (nx, ny) = (-dy / len, dx / len);
        let side = if (from.x - a.x) * nx + (from.y - a.y) * ny >= 0.0 { 1.0 } else { -1.0 };
        LINE_OVERSHOOTS
            .iter()
            .map(|d| Pos::new(centre.x - side * nx * d, centre.y - side * ny * d, centre.z))
            .find(|p| matches!(area.validate_move(*p, *p), MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(*p) < 0.75))
            .unwrap_or(centre)
    }

    /// Whether the bot stands where OK is pressed for `target` (never for a point that is walked over).
    fn at_spot(target: &Target, cur: Pos) -> bool {
        match target.kind {
            Kind::Walk => false,
            Kind::Press => target.point.is_pressed_at(cur, None),
            Kind::Talk { centre, radius, .. } => cur.dist_xy(centre) <= radius * TALK_STAND && (cur.z - centre.z).abs() <= Link::LEVEL,
        }
    }

    /// Advance the bot by `dt` seconds from `cur` at `speed` units per second.
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, dt: f32, speed: f32, progress: &[u8]) -> BotStep {
        let still = BotStep { pos: cur, facing: None, moved: false };
        if !self.requested || self.gave_up.is_some() || !(dt.is_finite() && dt > 0.0 && speed.is_finite() && speed > 0.0) {
            return still;
        }
        self.clock += dt;
        // The press had its step (the progress is another now): what is pressed next is pressed on the new story.
        if matches!(self.pressing, Pressing::Sent { .. }) && progress != self.sent_progress.as_slice() {
            self.pressing = Pressing::No;
        }
        // Nothing here would advance this story (it already did, or the area has no point for it).
        let Some(target) = self.target(area, progress, cur) else {
            if !matches!(self.pressing, Pressing::Sent { .. }) {
                self.pressing = Pressing::No;
            }
            self.press_point = None;
            return still;
        };
        if self.press_point != Some(target.point.id) {
            self.press_point = Some(target.point.id);
            // (a press that is waiting for its step is not forgotten by looking at another point)
            if !matches!(self.pressing, Pressing::Sent { .. }) {
                self.pressing = Pressing::No;
            }
            self.party_wait = 0.0;
        }
        let exits = area.exits();
        let len = speed * dt;
        let centre = match target.kind {
            Kind::Walk => Self::goal(area, &target.point, cur),
            Kind::Press => target.point.center(),
            Kind::Talk { centre, .. } => centre,
        };

        // At the spot where OK is pressed: stand there, press, and wait for the server's step.
        if Self::at_spot(&target, cur) {
            return self.press_here(&target, progress, still);
        }
        // Standing in the region and the server has not advanced the story yet: touch it again by
        // stepping a little way and back (it judges every accepted move).
        if target.kind == Kind::Walk && matches!(target.point.trigger, StoryTrigger::Region(_)) && target.point.is_hit_by(cur, cur) && cur.dist_xy(centre) < NUDGE * 2.0 {
            self.nudges += 1;
            // Forty steps one way, forty the other, and so on (the lowest bit of the count of forties).
            let sign = if (self.nudges / 40) & 1 == 0 { 1.0 } else { -1.0 };
            let want = Pos::new(centre.x + sign * NUDGE, centre.y, cur.z);
            let step = self.step_toward(area, &exits, cur, want, len).unwrap_or(still);
            return self.gate(&target, cur, step, dt, still);
        }
        let avoid = Self::battles_to_avoid(area, &target);
        let step = self.walk_to(area, (&exits, &avoid), &target, cur, centre, (dt, speed));
        self.gate(&target, cur, step, dt, still)
    }

    /// The regions of the points that start a battle, other than the one the bot goes for: stepping on one on the way
    /// brings the battle on before the party is ready and before the story ahead of it is done (the first reactor's
    /// last field has the talk of the materia to do before the battle's triangle, which lies near the way).
    fn battles_to_avoid(area: &dyn AreaRuntime, target: &Target) -> Vec<StoryPoint> {
        area.story_points()
            .into_iter()
            .filter(|p| p.id != target.point.id && matches!(p.trigger, StoryTrigger::Region(_) | StoryTrigger::Line(..)) && area.story_battle(p.id).is_some())
            .collect()
    }

    /// At the spot of a press point: want the press, once, and wait for the step (a press that brings none in
    /// [`PRESS_PATIENCE_S`] is reported, never repeated).
    fn press_here(&mut self, target: &Target, progress: &[u8], still: BotStep) -> BotStep {
        match self.pressing {
            Pressing::No => {
                self.press_npc = match target.kind {
                    Kind::Talk { npc, .. } => Some(npc),
                    _ => None,
                };
                self.sent_progress = progress.to_vec();
                self.pressing = Pressing::Wanted;
            }
            Pressing::Wanted => {}
            Pressing::Sent { since } if self.clock - since > PRESS_PATIENCE_S => {
                self.gave_up = Some(format!("pressed OK at story point {} and the story did not move in {PRESS_PATIENCE_S} s", target.point.id));
            }
            Pressing::Sent { .. } => {}
        }
        still
    }

    /// Whether the bot wants a press of OK sent now: at the spot of a press point (or a character it talks to), or at
    /// the foot of a ladder on its way.
    pub fn wants_press(&self) -> bool {
        self.pressing == Pressing::Wanted || self.nav.wants_press()
    }

    /// The press the bot wants, once: the caller sends it (`ClientMsg::Interact`) when the server knows where the
    /// bot stands ([`crate::state::OwnPlayer::has_unsent`] is false).
    pub fn take_press(&mut self) -> Option<Press> {
        if self.nav.wants_press() {
            self.nav.pressed();
            return Some(Press { npc: None });
        }
        if self.pressing == Pressing::Wanted {
            self.pressing = Pressing::Sent { since: self.clock };
            return Some(Press { npc: self.press_npc });
        }
        None
    }

    /// What the navigator wants said on stderr since the last call (the links it took, the routes it made).
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.nav.drain_notes()
    }

    /// Before a point that starts a battle a bot that leads a party stands still until the rest of it is in view (or it
    /// has waited [`BATTLE_PARTY_WAIT_S`]): a step that would touch the point is not taken until then.
    fn gate(&mut self, target: &Target, cur: Pos, step: BotStep, dt: f32, still: BotStep) -> BotStep {
        if !target.battle || !step.moved || !target.point.is_hit_by(cur, step.pos) {
            return step;
        }
        let (size, in_view) = self.party;
        if size <= 1 || in_view + 1 >= size || self.party_wait >= BATTLE_PARTY_WAIT_S {
            return step;
        }
        self.party_wait += dt;
        still
    }

    /// One step toward `centre`, for `target`: with the navigator in an area with links, else along the planned legs
    /// or greedily.
    fn walk_to(&mut self, area: &dyn AreaRuntime, (exits, avoid): (&[Exit], &[StoryPoint]), target: &Target, cur: Pos, centre: Pos, (dt, speed): (f32, f32)) -> BotStep {
        let still = BotStep { pos: cur, facing: None, moved: false };
        let len = speed * dt;
        if self.nav.has_links(area) {
            // This bot never crosses an exit (it would leave the area before the story was told), and does not step
            // on the region of a battle it is not going for.
            let any_exit = |a: Pos, b: Pos| exits.iter().any(|e| e.is_crossed_by(a, b)) || avoid.iter().any(|p| p.is_hit_by(a, b));
            let aim = |from: Pos| {
                let goal = match target.kind {
                    Kind::Walk => Self::goal(area, &target.point, from),
                    _ => centre,
                };
                (goal, goal)
            };
            let key = goal_key(&[&area.info().id.key, &target.point.id.to_string()]);
            match self.nav.step(area, cur, key, &aim, true, &any_exit, dt, speed) {
                Nav::Step(step) => {
                    if step.moved {
                        self.stuck = 0;
                    }
                    return step;
                }
                Nav::Press => return still,
                Nav::NoWay => {
                    self.stuck += 1;
                    if self.stuck >= STUCK_LIMIT {
                        self.gave_up = Some(format!("no way to the story point at ({:.0}, {:.0}) from ({:.0}, {:.0})", centre.x, centre.y, cur.x, cur.y));
                    }
                    return still;
                }
                // at the goal: what is left to do there is the plain code's
                Nav::Here => {}
            }
        }
        if let Some(step) = self.follow_plan(area, (exits, avoid), &target.point, cur, centre, len) {
            return step;
        }
        // Greedy: the heading nearest the goal's that is walkable and gets closer.
        let heading = (centre.y - cur.y).atan2(centre.x - cur.x);
        let mut best: Option<(f32, Pos, f32)> = None;
        for deg in FANOUT_DEG {
            let h = heading + deg.to_radians();
            let want = Pos::new(cur.x + h.cos() * len, cur.y + h.sin() * len, cur.z);
            if exits.iter().any(|e| e.is_crossed_by(cur, want)) {
                continue;
            }
            if let MoveResult::Accepted(p) = area.validate_move(cur, want) {
                if p.is_finite() {
                    let left = p.dist_xy(centre);
                    if best.is_none_or(|(b, ..)| left < b - 1e-3) {
                        best = Some((left, p, h));
                    }
                }
            }
        }
        match best {
            Some((left, pos, h)) if left < cur.dist_xy(centre) - 1e-3 => {
                self.stuck = 0;
                BotStep { pos, facing: Some(h), moved: true }
            }
            _ => {
                self.stuck += 1;
                if self.stuck >= STUCK_LIMIT {
                    self.gave_up = Some(format!("stuck on the way to a story point at ({:.0}, {:.0})", cur.x, cur.y));
                }
                still
            }
        }
    }

    /// One step from `cur` toward `want`, if it is walkable and crosses no exit.
    fn step_toward(&mut self, area: &dyn AreaRuntime, exits: &[Exit], cur: Pos, want: Pos, len: f32) -> Option<BotStep> {
        let dist = cur.dist_xy(want);
        if !(dist.is_finite() && dist > 1e-3) {
            return None;
        }
        let k = len.min(dist) / dist;
        let to = Pos::new(cur.x + (want.x - cur.x) * k, cur.y + (want.y - cur.y) * k, cur.z);
        if exits.iter().any(|e| e.is_crossed_by(cur, to)) {
            return None;
        }
        match area.validate_move(cur, to) {
            MoveResult::Accepted(p) if p.is_finite() => Some(BotStep { pos: p, facing: Some((to.y - cur.y).atan2(to.x - cur.x)), moved: true }),
            _ => None,
        }
    }

    /// One step along the planned legs (planning them first). `None`: no plan, or the next leg is
    /// blocked; the caller takes a greedy step instead and the plan is made again after a pause.
    fn follow_plan(&mut self, area: &dyn AreaRuntime, (exits, avoid): (&[Exit], &[StoryPoint]), point: &StoryPoint, cur: Pos, centre: Pos, len: f32) -> Option<BotStep> {
        let key = area.info().id.key.clone();
        if self.plan.as_ref().is_none_or(|p| p.area != key || p.point != point.id) {
            self.plan = None;
            if self.plan_cooldown > 0 {
                self.plan_cooldown -= 1;
                return None;
            }
            // This bot never crosses an exit (it would leave the area before the story was told): the planner is told so.
            let any_exit = |a: Pos, b: Pos| exits.iter().any(|e| e.is_crossed_by(a, b));
            // ...and it keeps off the region of a battle it is not going for, where there is a way that does (else it goes
            // over it: the point's own conditions then decide whether anything happens).
            let careful = |a: Pos, b: Pos| any_exit(a, b) || avoid.iter().any(|p| p.is_hit_by(a, b));
            match plan_path_avoiding(area, cur, centre, &careful).or_else(|| plan_path_avoiding(area, cur, centre, &any_exit)) {
                Some(waypoints) => self.plan = Some(Plan { area: key, point: point.id, waypoints, next: 1 }),
                None => {
                    self.plan_cooldown = PLAN_COOLDOWN_STEPS;
                    return None;
                }
            }
        }
        let plan = self.plan.as_mut()?;
        while plan.waypoints.get(plan.next).is_some_and(|wp| cur.dist_xy(*wp) < 1.0) {
            plan.next += 1;
        }
        let Some(wp) = plan.waypoints.get(plan.next).copied() else {
            self.plan = None;
            return None;
        };
        let step = self.step_toward(area, exits, cur, wp, len);
        match step {
            Some(step) => {
                self.stuck = 0;
                Some(step)
            }
            None => {
                self.plan = None;
                self.plan_cooldown = PLAN_COOLDOWN_STEPS;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, YARD};

    fn walk(bot: &mut StoryBot, area: &dyn AreaRuntime, mut pos: Pos, progress: &[u8], steps: usize) -> Pos {
        for _ in 0..steps {
            let step = bot.step(area, pos, 0.01, 600.0, progress);
            pos = step.pos;
        }
        pos
    }

    #[test]
    fn the_mirror_remembers_the_last_blob_counts_steps_and_says_what_changed() {
        let mut m = StoryMirror::default();
        assert!(m.progress().is_empty() && !m.told() && m.advances() == 0);
        assert_eq!(m.moment(), Some(fmv_ff7::story::NEW_GAME_MOMENT), "never told: a new account's moment");
        m.on_update(vec![1, 6, 0], Some(StoryAdvance { point: 0, summary: "moment=1->6\u{1b}[31m".into() }));
        assert_eq!((m.progress(), m.advances(), m.told()), (&[1u8, 6, 0][..], 1, true));
        assert_eq!(m.moment(), Some(6), "the story variable the dialogue lane reads");
        // A blob that is not FF7's is kept but never read as a moment.
        let mut other = StoryMirror::default();
        other.on_update(vec![7, 7], None);
        assert_eq!((other.progress(), other.moment()), (&[7u8, 7][..], None));
        m.on_update(vec![1, 6, 0], None);
        let notes = m.drain_notes();
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("advanced point=0 moment=1->6") && !notes[0].contains('\u{1b}'), "{notes:?}");
        assert!(notes[1].contains("joined with progress 010600"), "{notes:?}");
        assert!(m.drain_notes().is_empty(), "drained");
        // An absurdly long blob is not kept.
        m.on_update(vec![0; MAX_PROGRESS_BYTES + 1], None);
        assert_eq!(m.progress(), &[1, 6, 0]);
    }

    #[test]
    fn the_bot_walks_to_the_region_that_advances_its_story_and_stops_there() {
        let area = RoomsAdapter::with_story().load_area(HALL).unwrap();
        let mut bot = StoryBot::new(true, false);
        let start = Pos::new(-100.0, 0.0, 0.0);
        let point = area.story_points().remove(0);
        assert!(!point.is_hit_by(start, start));
        // A new account (empty progress): it gets there, and touches the region on the way.
        let mut pos = start;
        let mut touched = false;
        for _ in 0..600 {
            let step = bot.step(area.as_ref(), pos, 0.01, 600.0, &[]);
            touched |= point.is_hit_by(pos, step.pos);
            pos = step.pos;
        }
        assert!(touched, "it reached the region (ended at {pos:?})");
        assert!(bot.gave_up().is_none());
        // It never crossed the hall's exit (x = 300) on the way.
        assert!(area.exits().iter().all(|e| !e.is_crossed_by(start, pos)));
    }

    #[test]
    fn the_bot_crosses_a_story_line_by_aiming_past_its_middle_from_either_side() {
        let area = RoomsAdapter::with_gates().load_area(HALL).unwrap();
        let line = area.story_points().remove(1);
        assert!(matches!(line.trigger, StoryTrigger::Line(..)), "the hall's second point is a line");
        for start in [Pos::new(0.0, 100.0, 0.0), Pos::new(0.0, 260.0, 0.0)] {
            // the line is the nearest point to either start (the region is 100 to 250 units away), and it is not a place to stand
            assert!(StoryBot::new(true, false).target(area.as_ref(), &[], start).is_some_and(|t| t.point.id == 1), "{start:?}");
            assert!(!line.is_hit_by(start, start));
            let mut bot = StoryBot::new(true, false);
            let mut pos = start;
            let mut crossed = false;
            for _ in 0..600 {
                let step = bot.step(area.as_ref(), pos, 0.01, 600.0, &[]);
                crossed |= line.is_hit_by(pos, step.pos);
                if crossed {
                    break;
                }
                pos = step.pos;
            }
            assert!(crossed, "a step from {pos:?} crossed the line (started at {start:?})");
            assert!(bot.gave_up().is_none());
        }
        // the goal is just past the middle on the far side, and walkable
        let goal = StoryBot::goal(area.as_ref(), &line, Pos::new(0.0, 100.0, 0.0));
        assert!((goal.x, goal.y) == (0.0, 200.0 + 24.0), "{goal:?}");
        let goal = StoryBot::goal(area.as_ref(), &line, Pos::new(0.0, 260.0, 0.0));
        assert!((goal.x, goal.y) == (0.0, 200.0 - 24.0), "{goal:?}");
        // a point that fires on entry is nothing to walk to
        let yard = RoomsAdapter::with_gates().load_area(YARD).unwrap();
        assert!(yard.story_points().iter().all(|p| p.is_on_entry()));
        let bot = StoryBot::new(true, false);
        assert!(!bot.wants_to_walk(yard.as_ref(), &[], Pos::new(-200.0, 0.0, 0.0)), "the server moves the story on entry");
        assert!(bot.wants_to_walk(area.as_ref(), &[], Pos::new(0.0, 0.0, 0.0)));
        assert!(!bot.wants_to_walk(area.as_ref(), STORY_ADVANCED, Pos::new(0.0, 0.0, 0.0)), "nothing applies to an advanced story");
        assert!(!StoryBot::new(false, false).wants_to_walk(area.as_ref(), &[], Pos::new(0.0, 0.0, 0.0)), "a bot that was not asked to wants nothing");
    }

    #[test]
    fn a_new_area_is_a_clean_slate_for_a_bot_that_gave_up_in_the_last_one() {
        let area = RoomsAdapter::with_story().load_area(HALL).unwrap();
        let mut bot = StoryBot::new(true, false);
        let start = Pos::new(-100.0, 0.0, 0.0);
        // make it give up: a bot that makes no headway for STUCK_LIMIT steps (a wall between it and the region)
        bot.gave_up = Some("stuck on the way to a story point at (0, 0)".into());
        bot.stuck = 7;
        bot.nudges = 3;
        bot.plan_cooldown = 50;
        assert!(!bot.wants_to_walk(area.as_ref(), &[], start), "a bot that gave up wants nothing");
        assert_eq!(walk(&mut bot, area.as_ref(), start, &[], 20), start, "and stands still");
        bot.arrived();
        assert!(bot.gave_up().is_none() && bot.stuck == 0 && bot.nudges == 0 && bot.plan_cooldown == 0 && bot.plan.is_none());
        assert!(bot.wants_to_walk(area.as_ref(), &[], start), "in the next area it goes after the story again");
        assert_ne!(walk(&mut bot, area.as_ref(), start, &[], 20), start);
    }

    #[test]
    fn a_bot_whose_story_has_moved_on_has_nothing_to_do_and_stands_still() {
        let area = RoomsAdapter::with_story().load_area(HALL).unwrap();
        let mut bot = StoryBot::new(true, false);
        let start = Pos::new(-100.0, 0.0, 0.0);
        assert_eq!(walk(&mut bot, area.as_ref(), start, STORY_ADVANCED, 50), start, "no point applies to an advanced account");
        // Nor does an area without a story point.
        let yard = RoomsAdapter::with_story().load_area(YARD).unwrap();
        assert_eq!(walk(&mut bot, yard.as_ref(), Pos::new(-200.0, 0.0, 0.0), &[], 50), Pos::new(-200.0, 0.0, 0.0));
        // A bot that was not asked to does nothing at all.
        let mut idle = StoryBot::new(false, false);
        assert!(!idle.is_requested() && !idle.may_walk(100.0, true));
        assert_eq!(walk(&mut idle, area.as_ref(), start, &[], 50), start);
    }

    #[test]
    fn the_bot_that_also_invites_waits_for_its_invite_but_not_for_ever() {
        let bot = StoryBot::new(true, true);
        assert!(!bot.may_walk(1.0, false), "holds still until the invite went out");
        assert!(bot.may_walk(1.0, true));
        assert!(bot.may_walk(INVITE_WAIT_S + 0.1, false), "but not for ever");
        assert!(StoryBot::new(true, false).may_walk(0.0, false));
    }
}

#[cfg(test)]
mod press_tests;
