//! Story scenes in the world: what happens when a player presses OK on a scene's trigger, while it runs, and when it ends.
//!
//! A scene is played by the CLIENTS, from their own copy of the game (`fmv_ff7::script::scene`; nothing about it but ids
//! and choices crosses the wire, see `fmv_protocol::scene`). The server's part is everything that must not be left to a
//! client: who is in a scene, that those players are held, that a scene always ends, and what the story becomes.
//!
//! # A scene run
//!
//! A [`SceneRun`] is held state, like a battle: the player that pressed OK is its OWNER, party members of the owner that
//! stand in the same instance and in range may watch (at most [`MAX_SCENE_WATCHERS`]), and every participant is HELD for
//! as long as it lasts: its moves are ignored (so no encounter is rolled), its OK presses reach nothing, and a battle
//! does not seat it. Players that are not part of it see nothing of it.
//!
//! * **Start.** `Interact { npc }` naming a scene NPC (an id from [`SCENE_NPC_ID_BASE`]): the scene must be on offer for
//!   the player's instance (the story of the account whose phase chose it), in this area, and the player within the
//!   trigger's reach. The run gets a seed and a number; every participant is told with `SceneStart`.
//! * **While it runs.** The owner's client reports each window it moves past (`SceneStep`, in order, once each, with the
//!   answer it chose when the window asked); the server records the answers and relays the step to the watchers.
//!   Nothing else is trusted: a step out of order, from a watcher or for another run is dropped.
//! * **End.** The owner's client reports `SceneEnd` (finished or skipped); the server ends the run itself when the owner
//!   says nothing for [`SCENE_IDLE_TIMEOUT`], when the run lasts longer than [`SCENE_MAX_TIME`], and aborts it when the
//!   owner is gone. Every participant is told with `SceneEnded` and released.
//! * **The story.** When a run ends as finished, skipped or timed out, the server runs the scene's script itself, headless,
//!   with the answers the owner reported ([`SceneHost::finish`]) and applies what that run wrote to the story ONCE: the
//!   mirror, the saver (which merges, forward only), a `StoryUpdate`, and a placement refresh. A run that is aborted (the
//!   owner disconnected or left its party) applies nothing: the scene can be played again. The run is removed first and every
//!   message about it afterwards finds nothing, so a second `SceneEnd`, a late step or a repeated timeout changes nothing.
//!   A party member in the leader's phase does not advance the story there (its own phase did not choose the instance), so
//!   a run it owns writes nothing.
//!
//! # The debug preview
//!
//! `ClientMsg::SceneDebugPreview` (refused unless the server runs with `--debug-scene-preview`) puts a player in the field
//! of a scene at the scene's own story moment, WITHOUT touching its real story:
//!
//! * the player's mirror of its progression is swapped for a made-up one ([`SceneHost::preview_progress`]) and its real one
//!   kept; **nothing derived from the made-up story is ever saved**: the one place the world saves a story
//!   (`story::apply_story_step`) returns early for a previewing player, and so does the scene's own write here;
//! * the player's instance is its own: the phase key of a previewing player is made from its account, so two previews never
//!   share a field (party members still follow their leader's phase, which is how a member watches);
//! * it travels there like through an exit (`AreaChange`, answered with `JoinArea`), arriving where the scene says;
//! * the reconnect gets the stored story back, because the stored story was never touched.
//!
//! The server reads the game through a [`SceneHost`], so the world stays game-neutral: FF7's implementation is
//! `crate::scene_host`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, Exit, ExitKind, StoryStep};
use fmv_core::{AccountId, EntityId, PhaseKey, Pos};
use fmv_protocol::{
    ClientMsg, NpcSnapshot, SceneOutcome, SceneParticipant, SceneRefusal, SceneRole, SceneRunId, ServerMsg, StoryAdvance, MAX_SCENE_ID_CHARS,
    MAX_SCENE_STEPS, MAX_SCENE_WATCHERS, NPC_NAME_CHARS, NPC_REF_CHARS, SCENE_NPC_ID_BASE,
};

use super::{is_full, normalize_facing, Player, World};

/// How long a scene may go without the owner moving past a window before the server ends it.
pub const SCENE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest a scene may last in all.
pub const SCENE_MAX_TIME: Duration = Duration::from_secs(20 * 60);
/// How far from the owner (walkmesh units, x and y) a party member may stand and still watch.
pub const SCENE_WATCH_RANGE: f32 = 500.0;
/// How much farther than its talk radius a press may be from a trigger (a client's own idea of where it stands).
pub const SCENE_REACH_SLACK: f32 = 12.0;
/// Most answers the owner may report for one run.
pub const MAX_SCENE_ANSWERS: usize = 32;
/// The `StoryAdvance::point` of a step a scene made (no story point of the area is involved).
pub const SCENE_STEP_POINT: u32 = u32::MAX;
/// First byte of the phase key of a previewing player: longer than any verbatim key and different from the digest key's
/// and the FF7 keys' tags, so a made-up phase can never be a real one.
const PREVIEW_PHASE_TAG: u8 = 0xB7;

/// A scene the host knows (the world is game-neutral: it only sees where and how to offer one).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneInfo {
    /// The scene's stable id.
    pub id: String,
    /// The key of the area the scene plays in.
    pub field: String,
    /// Where a player put into the scene's field by the debug preview arrives (the area settles its height), the way it faces
    /// and the walkmesh triangle that holds it.
    pub arrival: Pos,
    pub arrival_facing: f32,
    pub arrival_hint: Option<u32>,
}

/// The character whose talk starts a scene, as the area stands him for a story.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerInfo {
    pub name: String,
    pub model: String,
    pub idle: Option<String>,
    pub pos: Pos,
    pub facing: f32,
    /// How near a player has to stand to talk to him.
    pub talk_radius: f32,
}

/// What a finished scene did to the story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneResult {
    /// The progression after the scene's writes (the game's own blob).
    pub progress: Vec<u8>,
    /// Whether the scene changed the story at all.
    pub changed: bool,
    /// What it changed, in the game's own `key=value` words.
    pub summary: String,
}

/// Where the world gets a game's scenes (see the module docs). Implementations read the game's own files; the world calls
/// them under its lock, so they must be quick (cache what is slow).
pub trait SceneHost: Send + Sync {
    /// Every scene, in the game's order: the trigger of the `n`-th is the scene NPC `SCENE_NPC_ID_BASE + n`.
    fn scenes(&self) -> Vec<SceneInfo>;
    /// Whether scene `scene` is on offer to a player whose instance plays the story `progress`.
    fn offered(&self, scene: &str, progress: &[u8]) -> bool;
    /// The trigger of scene `scene` for the story `progress` (`None`: it cannot be shown).
    fn trigger(&self, scene: &str, progress: &[u8]) -> Option<TriggerInfo>;
    /// The made-up story the debug preview puts a player in for scene `scene`.
    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>>;
    /// Run scene `scene` headless against the story `progress` with `seed` and the owner's `answers`, and say what it wrote.
    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult>;
}

/// What the server runs scenes with (`ServerConfig::scenes`).
#[derive(Clone)]
pub struct SceneSettings {
    pub host: Arc<dyn SceneHost>,
    /// Let players put themselves in a scene's field on a made-up story (`ClientMsg::SceneDebugPreview`). Off by default.
    pub debug_preview: bool,
}

impl std::fmt::Debug for SceneSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneSettings").field("debug_preview", &self.debug_preview).finish_non_exhaustive()
    }
}

/// What became of a scene message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneReply {
    /// Handled (what follows was queued).
    Done,
    /// Refused: tell the sender.
    Refused(SceneRefusal),
    /// The sender is no longer in the world.
    Gone,
}

/// A previewing player (see the module docs).
#[derive(Debug, Clone)]
pub(super) struct Preview {
    /// The made-up phase of its instances.
    phase: PhaseKey,
}

/// One scene being played.
#[derive(Debug, Clone)]
struct SceneRun {
    scene: String,
    owner: EntityId,
    watchers: Vec<EntityId>,
    seed: u32,
    /// The story the scene plays against (the owner's instance's).
    progress: Vec<u8>,
    /// The answers the owner reported, in order.
    answers: Vec<u8>,
    /// The windows the owner has moved past.
    steps: u32,
    started: Instant,
    last_activity: Instant,
    /// Whether the owner may write the story here (its own phase chose the instance).
    writes_story: bool,
    preview: bool,
}

/// The scenes of a world.
#[derive(Default)]
pub(super) struct SceneWorld {
    host: Option<Arc<dyn SceneHost>>,
    preview: bool,
    list: Vec<SceneInfo>,
    next_run: u64,
    runs: BTreeMap<SceneRunId, SceneRun>,
    /// The trigger NPCs asked of the host, by `(scene index, progression)`: the host decodes game files to answer.
    triggers: BTreeMap<(usize, Vec<u8>), Option<TriggerInfo>>,
}

/// The cache of trigger NPCs holds at most this many entries (a progression is a handful of bytes, but a client chooses them).
const MAX_TRIGGER_CACHE: usize = 256;

fn clip(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The index of the scene a scene NPC id stands for.
pub(super) fn scene_index(npc: u32) -> Option<usize> {
    npc.checked_sub(SCENE_NPC_ID_BASE).and_then(|n| usize::try_from(n).ok())
}

impl World {
    /// Give this world a game's scenes (and say whether the debug preview is on). Without a host there are no scenes: no
    /// scene NPC is announced and every scene message is refused.
    pub fn configure_scenes(&mut self, settings: Option<SceneSettings>) {
        self.scenes = match settings {
            Some(s) => SceneWorld { list: s.host.scenes(), host: Some(s.host), preview: s.debug_preview, ..SceneWorld::default() },
            None => SceneWorld::default(),
        };
    }

    /// Is `entity` held in a scene (as owner or watcher)?
    #[cfg(test)]
    pub fn in_scene(&self, entity: EntityId) -> bool {
        self.players.get(&entity).is_some_and(|p| p.scene.is_some())
    }

    /// How many scenes are running.
    #[cfg(test)]
    pub fn scene_count(&self) -> usize {
        self.scenes.runs.len()
    }

    /// The phase of `player` in `area`: the made-up one of a previewing player, else what the area reads of its progression.
    pub(super) fn player_phase(area: &super::areas::AreaState, player: &Player) -> PhaseKey {
        match &player.preview {
            Some(preview) => preview.phase.clone(),
            None => Self::phase_in(area, &player.progression),
        }
    }

    /// The scene NPCs to add to the cast `entity` is shown in `area`: the trigger of every scene on offer there for the story
    /// of its instance. Ids are `SCENE_NPC_ID_BASE + the scene's index`.
    pub(super) fn scene_npcs(&mut self, entity: EntityId, area: &AreaKey) -> Vec<NpcSnapshot> {
        let Some(host) = self.scenes.host.clone() else { return Vec::new() };
        let progress = self.instance_progression(entity);
        let mut out = Vec::new();
        for index in 0..self.scenes.list.len() {
            let Some(info) = self.scenes.list.get(index) else { continue };
            if !info.field.eq_ignore_ascii_case(area) || !host.offered(&info.id, &progress) {
                continue;
            }
            let id = info.id.clone();
            let Some(t) = self.trigger_of(&host, index, &id, &progress) else { continue };
            let Ok(npc_id) = u32::try_from(index).map(|i| SCENE_NPC_ID_BASE.saturating_add(i)) else { continue };
            out.push(NpcSnapshot {
                id: npc_id,
                name: clip(&t.name, NPC_NAME_CHARS),
                model: clip(&t.model, NPC_REF_CHARS),
                idle_animation: t.idle.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
                pos: t.pos,
                facing: normalize_facing(t.facing),
            });
        }
        out
    }

    fn trigger_of(&mut self, host: &Arc<dyn SceneHost>, index: usize, scene: &str, progress: &[u8]) -> Option<TriggerInfo> {
        let key = (index, progress.to_vec());
        if let Some(cached) = self.scenes.triggers.get(&key) {
            return cached.clone();
        }
        let found = host.trigger(scene, progress).filter(|t| t.pos.is_finite() && t.talk_radius.is_finite());
        if self.scenes.triggers.len() >= MAX_TRIGGER_CACHE {
            self.scenes.triggers.clear();
        }
        self.scenes.triggers.insert(key, found.clone());
        found
    }

    fn send_to(&self, entity: EntityId, msg: ServerMsg, failed: &mut Vec<(EntityId, bool)>) {
        let Some(p) = self.players.get(&entity) else { return };
        if let Err(e) = p.tx.try_send(Arc::new(msg)) {
            failed.push((entity, is_full(&e)));
        }
    }

    // ---- start ------------------------------------------------------------------------------------------------

    /// `Interact` naming scene NPC `npc`: start its scene for `entity` if it is on offer and in reach (module docs). Anything
    /// else is no press at all, as for a character the area does not have.
    pub(super) fn press_scene(&mut self, entity: EntityId, npc: u32, now: Instant) {
        let (Some(host), Some(index)) = (self.scenes.host.clone(), scene_index(npc)) else { return };
        let Some(info) = self.scenes.list.get(index).cloned() else { return };
        let (pos, area, account) = {
            let Some(p) = self.players.get(&entity) else { return };
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() {
                return;
            }
            (p.snapshot.pos, p.area.clone(), p.account)
        };
        if !info.field.eq_ignore_ascii_case(&area) {
            return;
        }
        let progress = self.instance_progression(entity);
        if !host.offered(&info.id, &progress) {
            return;
        }
        let Some(trigger) = self.trigger_of(&host, index, &info.id, &progress) else { return };
        if pos.dist_xy(trigger.pos) > trigger.talk_radius + SCENE_REACH_SLACK {
            return;
        }
        let (writes_story, preview) = self.players.get(&entity).map_or((false, false), |p| {
            (p.placement.as_ref().is_none_or(|pl| pl.story_advance_allowed), p.preview.is_some())
        });
        let watchers = self.watchers_for(entity, account, pos);
        let run = SceneRunId(self.scenes.next_run.max(1));
        self.scenes.next_run = run.0.saturating_add(1);
        let seed = u32::try_from(run.0.wrapping_mul(0x9E37_79B9) & 0xFFFF_FFFF).unwrap_or(1);
        let record = SceneRun {
            scene: info.id.clone(),
            owner: entity,
            watchers,
            seed,
            progress,
            answers: Vec::new(),
            steps: 0,
            started: now,
            last_activity: now,
            writes_story,
            preview,
        };
        self.start_run(run, record, &mut Vec::new());
    }

    /// The party members of `owner` that may watch: same instance, same area, in range, free (not in a battle, in transit or in a
    /// scene), in the party's own order, at most [`MAX_SCENE_WATCHERS`].
    fn watchers_for(&self, owner: EntityId, account: AccountId, pos: Pos) -> Vec<EntityId> {
        let Some(party) = self.parties.party_of(account) else { return Vec::new() };
        let instance = self.instances.instance_of(account);
        let area = self.players.get(&owner).map(|p| p.area.clone());
        party
            .members
            .iter()
            .filter(|&&m| m != account)
            .filter_map(|m| self.by_account.get(m).copied())
            .filter(|e| {
                self.players.get(e).is_some_and(|p| {
                    p.battle.is_none()
                        && p.transit.is_none()
                        && p.scene.is_none()
                        && Some(&p.area) == area.as_ref()
                        && self.instances.instance_of(p.account) == instance
                        && p.snapshot.pos.dist_xy(pos) <= SCENE_WATCH_RANGE
                })
            })
            .take(MAX_SCENE_WATCHERS)
            .collect()
    }

    /// Hold every participant of `record`, tell each with `SceneStart` and keep the run.
    fn start_run(&mut self, run: SceneRunId, record: SceneRun, failed: &mut Vec<(EntityId, bool)>) {
        let mut participants = vec![SceneParticipant { entity: record.owner, spot: 0 }];
        participants.extend(record.watchers.iter().zip(1u8..).map(|(&entity, spot)| SceneParticipant { entity, spot }));
        for p in &participants {
            if let Some(player) = self.players.get_mut(&p.entity) {
                player.scene = Some(run);
            }
            let role = if p.entity == record.owner { SceneRole::Owner } else { SceneRole::Watcher };
            let msg = ServerMsg::SceneStart {
                run,
                scene: clip(&record.scene, MAX_SCENE_ID_CHARS),
                seed: record.seed,
                progress: record.progress.clone(),
                owner: record.owner,
                participants: participants.clone(),
                you: role,
            };
            self.send_to(p.entity, msg, failed);
        }
        self.logs.info(format!(
            "scene started run={} scene={} owner={} watchers={:?} preview={} writes_story={}",
            run.0,
            record.scene,
            record.owner.0,
            record.watchers.iter().map(|e| e.0).collect::<Vec<_>>(),
            record.preview,
            record.writes_story
        ));
        self.scenes.runs.insert(run, record);
        let failed = std::mem::take(failed);
        self.drop_failed(failed);
    }

    // ---- while it runs --------------------------------------------------------------------------------------

    /// A scene message from `entity` (module docs).
    pub fn scene_message(&mut self, entity: EntityId, msg: ClientMsg, now: Instant) -> SceneReply {
        if !self.players.contains_key(&entity) {
            return SceneReply::Gone;
        }
        match msg {
            ClientMsg::SceneStep { run, step, answer } => {
                self.scene_step(entity, run, step, answer, now);
                SceneReply::Done
            }
            ClientMsg::SceneEnd { run, skipped } => {
                if self.scenes.runs.get(&run).is_some_and(|r| r.owner == entity) {
                    self.end_run(run, if skipped { SceneOutcome::Skipped } else { SceneOutcome::Finished }, now);
                }
                SceneReply::Done
            }
            ClientMsg::SceneLeave { run } => {
                self.scene_leave(entity, run, now);
                SceneReply::Done
            }
            ClientMsg::SceneDebugPreview { scene } => match self.debug_preview(entity, &scene, now) {
                Ok(()) => SceneReply::Done,
                Err(reason) => SceneReply::Refused(reason),
            },
            _ => SceneReply::Done,
        }
    }

    fn scene_step(&mut self, entity: EntityId, run: SceneRunId, step: u32, answer: Option<u8>, now: Instant) {
        let watchers = {
            let Some(r) = self.scenes.runs.get_mut(&run) else { return };
            // Only the owner drives, one window at a time, in order, within bounds.
            if r.owner != entity || step != r.steps || step >= MAX_SCENE_STEPS {
                return;
            }
            if let Some(a) = answer {
                if r.answers.len() >= MAX_SCENE_ANSWERS {
                    return;
                }
                r.answers.push(a);
            }
            r.steps = r.steps.saturating_add(1);
            r.last_activity = now;
            r.watchers.clone()
        };
        let mut failed = Vec::new();
        for w in watchers {
            self.send_to(w, ServerMsg::SceneStep { run, step, answer }, &mut failed);
        }
        self.drop_failed(failed);
    }

    /// A participant asks to be let go: the owner abandons the run (a skip), a watcher just stops watching.
    fn scene_leave(&mut self, entity: EntityId, run: SceneRunId, now: Instant) {
        let Some(r) = self.scenes.runs.get_mut(&run) else { return };
        if r.owner == entity {
            self.end_run(run, SceneOutcome::Skipped, now);
            return;
        }
        if !r.watchers.contains(&entity) {
            return;
        }
        r.watchers.retain(|w| *w != entity);
        let mut failed = Vec::new();
        self.release(entity, run, SceneOutcome::Skipped, &mut failed);
        self.drop_failed(failed);
    }

    /// Release `entity` from `run` and tell it so.
    fn release(&mut self, entity: EntityId, run: SceneRunId, outcome: SceneOutcome, failed: &mut Vec<(EntityId, bool)>) {
        if let Some(p) = self.players.get_mut(&entity) {
            if p.scene == Some(run) {
                p.scene = None;
            }
        }
        self.send_to(entity, ServerMsg::SceneEnded { run, outcome }, failed);
    }

    // ---- the end --------------------------------------------------------------------------------------------

    /// End `run` as `outcome`: release everyone and, unless it was aborted, apply what the scene's script wrote, once. The run
    /// is taken out first, so nothing that arrives for it afterwards finds it.
    fn end_run(&mut self, run: SceneRunId, outcome: SceneOutcome, _now: Instant) {
        let Some(record) = self.scenes.runs.remove(&run) else { return };
        let mut failed = Vec::new();
        let mut everyone = vec![record.owner];
        everyone.extend(record.watchers.iter().copied());
        for e in everyone {
            self.release(e, run, outcome, &mut failed);
        }
        let story = if outcome == SceneOutcome::Aborted { "dropped" } else { self.apply_scene_story(&record) };
        self.logs.info(format!("scene ended run={} scene={} outcome={outcome:?} answers={} story={story}", run.0, record.scene, record.answers.len()));
        self.drop_failed(failed);
    }

    /// What the scene's script wrote, applied to the owner's story once (see the module docs). Returns a word for the log.
    fn apply_scene_story(&mut self, record: &SceneRun) -> &'static str {
        if !record.writes_story {
            return "none (not the owner's phase)";
        }
        let Some(host) = self.scenes.host.clone() else { return "none (no host)" };
        let Some(result) = host.finish(&record.scene, &record.progress, record.seed, &record.answers) else { return "none (the scene could not be run)" };
        if !result.changed {
            return "unchanged";
        }
        let summary = format!("scene={} {}", clip(&record.scene, MAX_SCENE_ID_CHARS), result.summary);
        let Some(p) = self.players.get_mut(&record.owner) else { return "none (the owner is gone)" };
        if p.preview.is_none() {
            // A real story: a scene's write is a story step like any other (the mirror, the log line, a countdown it ends, the
            // save, the player told and the placements refreshed: `story::apply_story_step`).
            self.apply_story_step(record.owner, SCENE_STEP_POINT, StoryStep { progress: result.progress, summary, countdown: None });
            return "applied";
        }
        // A previewing player's story is made up: it is told the step and keeps it in memory only, and nothing is saved
        // (`apply_story_step` would not apply it either).
        let account = p.account;
        p.progression = result.progress;
        self.logs.info(format!("story advanced (preview: made up, not saved) account={} entity={} {summary}", account.0, record.owner.0));
        let mut failed = Vec::new();
        self.tell_story(record.owner, Some(StoryAdvance { point: SCENE_STEP_POINT, summary }), &mut failed);
        let mut accounts = std::collections::BTreeSet::from([account]);
        if let Some(party) = self.parties.party_of(account) {
            accounts.extend(party.members.iter().copied());
        }
        self.refresh_placements(&accounts, &mut failed);
        self.drop_failed(failed);
        "applied (preview: not saved)"
    }

    /// Housekeeping, once per tick: end the runs that have gone quiet or lasted too long.
    pub fn advance_scenes(&mut self, now: Instant) {
        let late: Vec<SceneRunId> = self
            .scenes
            .runs
            .iter()
            .filter(|(_, r)| now.saturating_duration_since(r.last_activity) >= SCENE_IDLE_TIMEOUT || now.saturating_duration_since(r.started) >= SCENE_MAX_TIME)
            .map(|(id, _)| *id)
            .collect();
        for run in late {
            self.end_run(run, SceneOutcome::TimedOut, now);
        }
    }

    /// `entity` left the world (disconnected, dropped): its run is abandoned if it owned one (nothing is written), or it simply
    /// stops watching one. `held` is the run its player was in.
    pub(super) fn scene_player_gone(&mut self, entity: EntityId, held: Option<SceneRunId>) {
        let Some(run) = held else { return };
        let Some(r) = self.scenes.runs.get_mut(&run) else { return };
        if r.owner == entity {
            self.end_run(run, SceneOutcome::Aborted, Instant::now());
        } else {
            r.watchers.retain(|w| *w != entity);
        }
    }

    /// The party changed: a watcher that is no longer in its owner's party stops watching.
    pub(super) fn scene_check_parties(&mut self) {
        let mut dropped: Vec<(SceneRunId, EntityId)> = Vec::new();
        for (id, r) in &self.scenes.runs {
            let Some(owner) = self.players.get(&r.owner) else { continue };
            let party = self.parties.party_of(owner.account);
            for &w in &r.watchers {
                let still = self.players.get(&w).is_some_and(|p| party.is_some_and(|party| party.contains(p.account)));
                if !still {
                    dropped.push((*id, w));
                }
            }
        }
        let mut failed = Vec::new();
        for (id, w) in dropped {
            if let Some(r) = self.scenes.runs.get_mut(&id) {
                r.watchers.retain(|x| *x != w);
            }
            self.release(w, id, SceneOutcome::Aborted, &mut failed);
        }
        self.drop_failed(failed);
    }

    // ---- the debug preview ----------------------------------------------------------------------------------

    /// `SceneDebugPreview`: put `entity` in the field of scene `scene` on a made-up story (module docs).
    fn debug_preview(&mut self, entity: EntityId, scene: &str, now: Instant) -> Result<(), SceneRefusal> {
        let (Some(host), true) = (self.scenes.host.clone(), self.scenes.preview) else { return Err(SceneRefusal::Unavailable) };
        if scene.chars().count() > MAX_SCENE_ID_CHARS {
            return Err(SceneRefusal::UnknownScene);
        }
        let Some(info) = self.scenes.list.iter().find(|s| s.id == scene).cloned() else { return Err(SceneRefusal::UnknownScene) };
        let Some(progress) = host.preview_progress(scene) else { return Err(SceneRefusal::UnknownScene) };
        let (from, facing, account) = {
            let Some(p) = self.players.get(&entity) else { return Err(SceneRefusal::Busy) };
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() {
                return Err(SceneRefusal::Busy);
            }
            (p.snapshot.pos, p.snapshot.facing, p.account)
        };
        // The made-up story and phase go in first: they are what the placement on arrival reads.
        let real = {
            let Some(p) = self.players.get_mut(&entity) else { return Err(SceneRefusal::Busy) };
            let real = (p.progression.clone(), p.preview.clone());
            p.progression = progress;
            p.preview = Some(Preview { phase: preview_phase(account, &info.id) });
            real
        };
        let exit = Exit {
            line: (Pos::default(), Pos::default()),
            target: info.field.clone(),
            arrival: info.arrival,
            arrival_facing: Some(info.arrival_facing),
            arrival_hint: info.arrival_hint,
            kind: ExitKind::Gateway,
        };
        if !self.start_travel(entity, &exit, from, facing, now) {
            // The field cannot be hosted: nothing of the preview stays.
            if let Some(p) = self.players.get_mut(&entity) {
                (p.progression, p.preview) = real;
            }
            return Err(SceneRefusal::Unavailable);
        }
        self.logs.info(format!("scene preview entity={} account={} scene={} (a made-up story: nothing of it is saved)", entity.0, account.0, info.id));
        Ok(())
    }

    /// `entity` has just come to stand in an area: a previewing player is told the made-up story its client reads its talks from.
    pub(super) fn scene_entered(&mut self, entity: EntityId, failed: &mut Vec<(EntityId, bool)>) {
        if self.players.get(&entity).is_some_and(|p| p.preview.is_some()) {
            self.tell_story(entity, None, failed);
        }
    }
}

/// The phase key of a previewing player: its account and the scene, behind a tag no real key carries (and longer than every
/// verbatim key, so it equals none of them).
fn preview_phase(account: AccountId, scene: &str) -> PhaseKey {
    let mut key = vec![PREVIEW_PHASE_TAG];
    key.extend_from_slice(&account.0.to_le_bytes());
    key.extend_from_slice(&blake3::hash(scene.as_bytes()).as_bytes()[..25]);
    PhaseKey(key)
}

#[cfg(test)]
mod tests;
