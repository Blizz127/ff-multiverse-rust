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
//! to that spot until the server has moved the story. It never crosses an exit (that would take it to
//! another area) and never walks off the walkable surface. The server decides, as always: the bot only
//! walks where the data says the story happens.

use std::collections::VecDeque;

use fmv_adapter_api::{AreaKey, AreaRuntime, Exit, MoveResult, StoryPoint, StoryTrigger};
use fmv_core::Pos;
use fmv_protocol::StoryAdvance;

use crate::bot::BotStep;
use crate::nav::plan_path;

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
/// Headings tried in a step, as degrees away from the straight line to the goal, best first.
const FANOUT_DEG: [f32; 11] = [0.0, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0, 60.0, -60.0, 90.0, -90.0];
/// Steps without headway after which the bot gives up (at 100 steps per second: 5 s).
const STUCK_LIMIT: u32 = 500;
/// Steps to wait before planning again after a plan failed.
const PLAN_COOLDOWN_STEPS: u32 = 100;

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
}

impl StoryBot {
    pub fn new(requested: bool, wait_for_invite: bool) -> Self {
        Self { requested, wait_for_invite, plan: None, plan_cooldown: 0, stuck: 0, gave_up: None, nudges: 0 }
    }

    /// `--story-advance` was given: the bot decides every step, even when there is nothing to do
    /// (it stands still then, instead of walking the pattern).
    pub fn is_requested(&self) -> bool {
        self.requested
    }

    pub fn gave_up(&self) -> Option<&str> {
        self.gave_up.as_deref()
    }

    /// May the bot walk? It holds still until its invite went out when it was told to wait for it
    /// (`invited`), but at most [`INVITE_WAIT_S`] (`now` is seconds on the session clock).
    pub fn may_walk(&self, now: f64, invited: bool) -> bool {
        self.requested && self.gave_up.is_none() && !(self.wait_for_invite && !invited && now < INVITE_WAIT_S)
    }

    /// The region point of `area` that would advance an account whose progress is `progress`, nearest
    /// to `from` first.
    fn target(area: &dyn AreaRuntime, progress: &[u8], from: Pos) -> Option<StoryPoint> {
        area.story_points()
            .into_iter()
            .filter(|p| matches!(p.trigger, StoryTrigger::Region(_)) && area.story_advance(progress, p.id).is_some())
            .min_by(|a, b| a.center().dist_xy(from).total_cmp(&b.center().dist_xy(from)))
    }

    /// Advance the bot by `dt` seconds from `cur` at `speed` units per second.
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, dt: f32, speed: f32, progress: &[u8]) -> BotStep {
        let still = BotStep { pos: cur, facing: None, moved: false };
        if !self.requested || self.gave_up.is_some() || !(dt.is_finite() && dt > 0.0 && speed.is_finite() && speed > 0.0) {
            return still;
        }
        // Nothing here would advance this story (it already did, or the area has no point for it).
        let Some(point) = Self::target(area, progress, cur) else { return still };
        let exits = area.exits();
        let len = speed * dt;
        let centre = point.center();

        // Standing in the region and the server has not advanced the story yet: touch it again by
        // stepping a little way and back (it judges every accepted move).
        if point.is_hit_by(cur, cur) && cur.dist_xy(centre) < NUDGE * 2.0 {
            self.nudges += 1;
            // Forty steps one way, forty the other, and so on (the lowest bit of the count of forties).
            let sign = if (self.nudges / 40) & 1 == 0 { 1.0 } else { -1.0 };
            let want = Pos::new(centre.x + sign * NUDGE, centre.y, cur.z);
            return self.step_toward(area, &exits, cur, want, len).unwrap_or(still);
        }

        if let Some(step) = self.follow_plan(area, &exits, &point, cur, centre, len) {
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
    fn follow_plan(&mut self, area: &dyn AreaRuntime, exits: &[Exit], point: &StoryPoint, cur: Pos, centre: Pos, len: f32) -> Option<BotStep> {
        let key = area.info().id.key.clone();
        if self.plan.as_ref().is_none_or(|p| p.area != key || p.point != point.id) {
            self.plan = None;
            if self.plan_cooldown > 0 {
                self.plan_cooldown -= 1;
                return None;
            }
            match plan_path(area, cur, centre) {
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
