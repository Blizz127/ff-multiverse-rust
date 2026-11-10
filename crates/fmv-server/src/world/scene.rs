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
//!   owner is gone. A completed durable scene stays held while its queued transaction is pending.
//! * **The story.** When a run ends as finished or explicitly skipped, the server runs the scene's script itself, headless,
//!   with the answers the owner reported ([`SceneHost::finish`]) and applies what that run wrote to the story ONCE: the
//!   mirror, a `StoryUpdate`, and a placement refresh. A resource transaction first commits the exact story and gil pair
//!   through the ordered saver; only its durable acknowledgment publishes the pair and releases participants. A failed or
//!   uncertain acknowledgment never publishes success. A run that is aborted before queuing (the
//!   owner disconnected, could not play it, or timed out) applies nothing: the scene can be played again. The run is removed first and every
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

mod continuation;
pub use continuation::{SceneBoundary, SceneBoundaryKind};

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
#[derive(Debug, Clone, PartialEq)]
pub struct SceneResult {
    /// The progression after the scene's writes (the game's own blob).
    pub progress: Vec<u8>,
    /// Whether the scene changed the story at all.
    pub changed: bool,
    /// What it changed, in the game's own `key=value` words.
    pub summary: String,
    /// A validated script destination, if this scene ends in a permitted field transition.
    pub travel: Option<fmv_adapter_api::Exit>,
    /// Resource/story changes that must be acknowledged by durable storage before releasing the scene.
    pub transaction: Option<SceneTransaction>,
}

/// Exact game-computed gil transition. Arithmetic, saturation and choice rules belong to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneTransaction {
    pub expected_gil: u32,
    pub next_gil: u32,
}

/// Where the world gets a game's scenes (see the module docs). Implementations read the game's own files; the world calls
/// them under its lock, so they must be quick (cache what is slow).
pub trait SceneHost: Send + Sync {
    /// The game's explicit starting story for a newly registered account, when this host
    /// supports storing it. Registration commits it with the identity and starting resources;
    /// token logins and imported saves never use it. `None` preserves the legacy empty seed.
    fn fresh_story(&self) -> Option<Vec<u8>> { None }
    /// Every scene, in the game's order: the trigger of the `n`-th is the scene NPC `SCENE_NPC_ID_BASE + n`.
    fn scenes(&self) -> Vec<SceneInfo>;
    /// Whether scene `scene` is on offer to a player whose instance plays the story `progress`.
    fn offered(&self, scene: &str, progress: &[u8]) -> bool;
    /// Whether the scene starts automatically on entering its field.
    fn automatic(&self, _scene: &str) -> bool { false }
    /// Whether this arrival must be linked to an acknowledged predecessor landing. Such a
    /// scene is entered by the continuation handoff, never by an unrelated field login.
    fn requires_landing(&self, _scene: &str) -> bool { false }
    /// Per-save arrival policy, allowing a new durable arrival to retain older presentation
    /// behavior for legacy saves. This describes a required predecessor, not executor support.
    fn requires_landing_for(&self, scene: &str, _progress: &[u8]) -> bool { self.requires_landing(scene) }
    /// Whether this scene uses strict, durable window and battle continuation instead of the
    /// presentation-only scene completion path.
    fn continuation(&self, _scene: &str) -> bool { false }
    /// Whether this stored story is eligible for the scene's continuation contract. This lets
    /// a host retain legacy presentation while admitting exact point scripts for full saves.
    fn continuation_for(&self, scene: &str, _progress: &[u8]) -> bool { self.continuation(scene) }
    /// This source has a reviewed noncombat restore which is redundant only when no combat
    /// plan or carried HP/MP exists. The runtime checks that precondition on replay and writes.
    fn noncombat_continuation(&self, _scene: &str) -> bool { false }
    /// Whether the source identity must capture the accepted trigger-crossing pose. Authored
    /// point scenes with their own cast placement do not inherit a mutable player position.
    fn requires_origin(&self, _scene: &str) -> bool { false }
    /// Reconstruct the exact accepted transcript to its next boundary. `None` starts a new
    /// replay and returns its source identity; an existing replay must match that identity.
    fn evaluate_continuation(&self, _scene: &str, _original_progress: &[u8], _seed: u32,
        _original_gil: u32, _replay: Option<&fmv_protocol::SceneReplay>) -> Option<SceneBoundary> { None }
    /// Capture an authored line trigger's initial cast placement before binding its source.
    fn initial_continuation(&self, scene: &str, progress: &[u8], seed: u32, gil: u32,
        _origin: Option<fmv_protocol::SceneOrigin>) -> Option<SceneBoundary> {
        self.evaluate_continuation(scene, progress, seed, gil, None)
    }
    /// The authored scene line crossed by an accepted move, if enabled for this story.
    fn line_scene(&self, _field: &str, _progress: &[u8], _from: Pos, _to: Pos) -> Option<String> { None }
    /// A source-verified exact point scene which owns its complete script result and travel.
    /// This is consulted before scalar point evaluation; an unsupported or unmatched source
    /// returns None and cannot bypass the area's normal step validation.
    fn point_continuation(&self, _field: &str, _before: &[u8], _point: u32) -> Option<String> { None }
    /// The CUTSCENE of a story point, if the game has one: the scene to play for a player whose step in area `field` takes its
    /// story from `before` to `after` (the point's own step, which the world applies when the scene ends: see
    /// `World::start_point_scene`). `None`: the step is applied at once, as it always was.
    fn point_scene(&self, _field: &str, _before: &[u8], _after: &[u8]) -> Option<String> { None }
    /// Presentation after a matching story battle wins. The victory step is still applied immediately and once;
    /// this scene neither saves nor starts a second fight.
    fn victory_scene(&self, _field: &str, _before: &[u8], _after: &[u8], _formation: u16) -> Option<String> { None }
    /// Where an account whose story is `progress` comes into the world, when that is not where everyone joins: a story that
    /// a scene took somewhere no walk leads back from stands there again on its next login (`World::join` sends the player
    /// on from the entry area through the ordinary area handshake). `None`: it joins like everyone.
    fn entry(&self, _progress: &[u8]) -> Option<fmv_adapter_api::Exit> { None }
    /// The trigger of scene `scene` for the story `progress` (`None`: it cannot be shown).
    fn trigger(&self, scene: &str, progress: &[u8]) -> Option<TriggerInfo>;
    /// The made-up story the debug preview puts a player in for scene `scene`.
    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>>;
    /// Run scene `scene` headless against the story `progress` with `seed` and the owner's `answers`, and say what it wrote.
    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult>;
    /// Interpret using the owner's resource snapshot captured when the scene began.
    fn finish_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], _gil: u32) -> Option<SceneResult> {
        self.finish(scene, progress, seed, answers)
    }
    /// Debug preview runs on made-up state which is never saved and never travels.
    fn finish_preview(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.finish(scene, progress, seed, answers)
    }
    /// A preview uses the same captured resource input as its clients, without a durable write.
    fn finish_preview_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], _gil: u32) -> Option<SceneResult> {
        self.finish_preview(scene, progress, seed, answers)
    }
}

/// What the server runs scenes with (`ServerConfig::scenes`).
#[derive(Clone)]
pub struct SceneSettings {
    pub host: Arc<dyn SceneHost>,
    /// Let players put themselves in a scene's field on a made-up story (`ClientMsg::SceneDebugPreview`). Off by default.
    pub debug_preview: bool,
    /// DEBUG: the story a NEW account starts with instead of the game's own new story (the game's blob). `None` on every
    /// real server. A smoke test's way to a later part of the story without playing through everything before it; the
    /// binary only allows it on a throw-away store (`--debug-start-moment`).
    pub debug_new_story: Option<Vec<u8>>,
}

impl std::fmt::Debug for SceneSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneSettings").field("debug_preview", &self.debug_preview).field("debug_new_story", &self.debug_new_story.is_some()).finish_non_exhaustive()
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
    /// The owner's gil when interpretation began, shared with its client and checked again by the saver.
    gil: u32,
    /// The answers the owner reported, in order.
    answers: Vec<u8>,
    /// The windows the owner has moved past.
    steps: u32,
    started: Instant,
    last_activity: Instant,
    /// Whether the owner may write the story here (its own phase chose the instance).
    writes_story: bool,
    preview: bool,
    /// A cutscene's story point, whose step is applied when the run ends (`World::start_point_scene`). `None` for every other run.
    then: Option<PointThen>,
}

/// What a cutscene run holds back until it ends: the story point whose step it presents.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PointThen {
    point: u32,
    /// The area the point is in (the step applies only to a player still standing there).
    area: AreaKey,
    /// Whether the point was reached by coming to stand in the area (`World::enter_story` stopped at it and judges the
    /// area's other entry points again after the step).
    on_entry: bool,
    /// The battle the point starts first (`AreaRuntime::story_battle`), if it does: when the run ends the battle starts, and
    /// the step is paid when it is won, as for a point without a cutscene.
    battle: Option<u16>,
}

struct PendingScene {
    record: SceneRun,
    account: AccountId,
    result: SceneResult,
    outcome: SceneOutcome,
    progress: Option<crate::progress::Progress>,
    inventory: Option<crate::inventory::Inventory>,
    done: tokio::sync::oneshot::Receiver<Result<crate::saver::SceneSaved, crate::saver::SceneSaveError>>,
}

/// The scenes of a world.
#[derive(Default)]
pub(super) struct SceneWorld {
    host: Option<Arc<dyn SceneHost>>,
    preview: bool,
    /// DEBUG: the story a new account starts with (`SceneSettings::debug_new_story`).
    pub(super) debug_new_story: Option<Vec<u8>>,
    list: Vec<SceneInfo>,
    next_run: u64,
    runs: BTreeMap<SceneRunId, SceneRun>,
    pending: BTreeMap<SceneRunId, PendingScene>,
    /// A point scene whose ordinary exact story write still owns its presentation hold.
    ordinary_finishing: BTreeMap<SceneRunId, SceneOutcome>,
    continuations: BTreeMap<SceneRunId, continuation::Continuation>,
    /// Admission fence: a login that read storage across a scene save must load it again.
    save_epoch: u64,
    /// A failed worker or uncertain store write requires recovery before any player may continue.
    pub(super) storage_failed: bool,
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
            Some(s) => SceneWorld { list: s.host.scenes(), host: Some(s.host), preview: s.debug_preview, debug_new_story: s.debug_new_story, ..SceneWorld::default() },
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
        self.scenes.runs.len() + self.scenes.pending.len()
    }

    /// Authored login placement for an already saved story, before admission publishes its
    /// first area snapshot. The scene host remains private to this module.
    pub(super) fn saved_story_entry(&self, progress: &[u8]) -> Option<Exit> {
        self.scenes.host.as_ref()?.entry(progress)
    }

    pub(crate) fn scene_save_epoch(&self) -> u64 { self.scenes.save_epoch }

    pub(super) fn bump_story_save_epoch(&mut self) {
        self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
    }

    pub(super) fn fail_story_storage(&mut self, reason: &str) {
        self.logs.warn(format!("ordinary story storage requires recovery: {reason}"));
        self.stop_for_scene_storage_recovery();
    }

    /// Validated travel targets stay loaded until the transaction is acknowledged.
    pub(super) fn pending_scene_destinations(&self) -> impl Iterator<Item = AreaKey> + '_ {
        self.scenes.pending.values().filter_map(|p| p.result.travel.as_ref().map(|exit| exit.target.clone()))
            .chain(self.scenes.continuations.values().flat_map(continuation::Continuation::destinations))
            .chain(self.ordinary_story_destinations())
            .chain(self.ordinary_battle_destinations())
    }

    pub(super) fn scene_save_pending(&self, account: AccountId) -> bool {
        self.scenes.pending.values().any(|pending| pending.account == account)
            || self.scenes.continuations.values().any(|c| c.accounts.contains(&account))
            || self.ordinary_story_account_held(account) || self.ordinary_battle_account_held(account)
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
            if !info.field.eq_ignore_ascii_case(area) || host.automatic(&info.id) || !host.offered(&info.id, &progress) {
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

    pub(super) fn send_to(&self, entity: EntityId, msg: ServerMsg, failed: &mut Vec<(EntityId, bool)>) {
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
        if host.automatic(&info.id) { return; }
        let (pos, area, account) = {
            let Some(p) = self.players.get(&entity) else { return };
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() || p.story_pending.is_some() {
                return;
            }
            (p.snapshot.pos, p.area.clone(), p.account)
        };
        if self.account_in_battle(account) { return; }
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
            gil: self.players.get(&entity).and_then(|p| p.progress).map_or(0, |p| p.gil),
            answers: Vec::new(),
            steps: 0,
            started: now,
            last_activity: now,
            writes_story,
            preview,
            then: None,
        };
        self.start_run(run, record, &mut Vec::new());
    }

    /// A verified point script owns its full durable continuation, including any authored
    /// destination. It has no PointThen: the ordinary point must not apply the result again.
    pub(super) fn start_point_continuation(&mut self, entity: EntityId, point: u32, now: Instant) -> bool {
        let Some(player) = self.players.get(&entity) else { return false; };
        if player.preview.is_some() || player.scene.is_some() || player.battle.is_some() || player.transit.is_some()
            || self.account_in_battle(player.account)
            || player.placement.as_ref().is_some_and(|placement| !placement.story_advance_allowed)
            || (player.story_pending.is_some() && !self.ordinary_story_resuming(entity)) { return false; }
        let Some(host) = self.scenes.host.as_ref() else { return false; };
        let Some(scene) = host.point_continuation(&player.area, &player.progression, point) else { return false; };
        if !host.continuation_for(&scene, &player.progression) || !host.offered(&scene, &player.progression)
            || !self.scenes.list.iter().any(|info| info.id == scene && info.field == player.area) { return false; }
        let watchers = self.watchers_for(entity, player.account, player.snapshot.pos);
        let run = SceneRunId(self.scenes.next_run.max(1));
        self.scenes.next_run = run.0.saturating_add(1);
        let record = SceneRun { scene, owner: entity, watchers,
            seed: run.0.wrapping_mul(0x9E37_79B9) as u32, progress: player.progression.clone(),
            gil: player.progress.map_or(0, |progress| progress.gil), answers: Vec::new(), steps: 0,
            started: now, last_activity: now, writes_story: true, preview: false, then: None };
        self.start_run(run, record, &mut Vec::new());
        true
    }

    /// `entity` reached story point `point` of its area, whose step is `step`: if the game has a cutscene for that step
    /// (`SceneHost::point_scene`), play it first. The run is the player's (party members in range watch, as for any scene),
    /// built against the story it has BEFORE the step; it writes nothing of its own and sends nobody anywhere, and when it
    /// ends, however it ends, the point's own step is applied (judged again then: `point_step_after_scene`), unless the player
    /// has left the world. Returns whether a run was started (the step then waits for it); `false` leaves the step to the
    /// caller, as before cutscenes. A previewing player, or one that is held, in a battle or in transit, gets none.
    pub(super) fn start_point_scene(&mut self, entity: EntityId, point: u32, step: &StoryStep, now: Instant, on_entry: bool, battle: Option<u16>) -> bool {
        let Some(host) = self.scenes.host.as_ref() else { return false };
        let Some(p) = self.players.get(&entity) else { return false };
        let area = p.area.clone();
        let Some(scene) = host.point_scene(&area, &p.progression, &step.progress) else { return false };
        self.start_cutscene(entity, scene, p.progression.clone(), now, Some(PointThen { point, area, on_entry, battle }))
    }

    /// Called only after a winning story battle's step has been saved, with its pre-victory story retained for staging.
    pub(super) fn start_victory_scene(&mut self, entity: EntityId, before: Vec<u8>, step: &StoryStep, formation: u16, now: Instant) -> bool {
        let Some(host) = self.scenes.host.as_ref() else { return false };
        let Some(p) = self.players.get(&entity) else { return false };
        let Some(scene) = host.victory_scene(&p.area, &before, &step.progress, formation) else { return false };
        self.start_cutscene(entity, scene, before, now, None)
    }

    fn start_cutscene(&mut self, entity: EntityId, scene: String, before: Vec<u8>, now: Instant, then: Option<PointThen>) -> bool {
        let (account, pos) = {
            let Some(p) = self.players.get(&entity) else { return false };
            if p.preview.is_some() || p.scene.is_some() || p.battle.is_some() || p.transit.is_some()
                || (p.story_pending.is_some() && !self.ordinary_story_resuming(entity)) {
                return false;
            }
            (p.account, p.snapshot.pos)
        };
        if self.account_in_battle(account) { return false; }
        if !self.scenes.list.iter().any(|s| s.id == scene) {
            return false;
        }
        let watchers = self.watchers_for(entity, account, pos);
        let run = SceneRunId(self.scenes.next_run.max(1));
        self.scenes.next_run = run.0.saturating_add(1);
        let seed = u32::try_from(run.0.wrapping_mul(0x9E37_79B9) & 0xFFFF_FFFF).unwrap_or(1);
        let record = SceneRun {
            scene,
            owner: entity,
            watchers,
            seed,
            progress: before,
            gil: self.players.get(&entity).and_then(|p| p.progress).map_or(0, |p| p.gil),
            answers: Vec::new(),
            steps: 0,
            started: now,
            last_activity: now,
            writes_story: false,
            preview: false,
            then,
        };
        self.start_run(run, record, &mut Vec::new());
        true
    }

    /// A cutscene run has ended: apply its point's step to its owner, judged on the owner's story NOW (as a battle's step is
    /// after the win), if the owner is still in the world, in that area, free, and allowed to move the story there; then, for a
    /// point reached on entry, the area's other entry points the run held back. A point that starts a battle first starts it
    /// instead (the step then waits for the win, `story_after_battle`).
    fn point_step_after_scene(&mut self, owner: EntityId, then: PointThen, now: Instant) {
        if let Some(formation) = then.battle {
            // The point starts a battle first: start it now (the step waits for the win), if the point still applies to the
            // owner where it stands.
            let applies = self.players.get(&owner).is_some_and(|p| {
                p.area == then.area && p.battle.is_none() && p.transit.is_none() && p.scene.is_none()
                    && p.placement.as_ref().is_none_or(|pl| pl.story_advance_allowed)
                    && self.areas.get(&p.area).is_some_and(|a| a.runtime().story_advance(&p.progression, then.point).is_some())
            });
            if applies {
                self.start_story_battle(owner, then.point, formation, now);
            }
            return;
        }
        let step = {
            let Some(player) = self.players.get(&owner) else { return };
            if player.area != then.area || player.battle.is_some() || player.transit.is_some() || player.scene.is_some() {
                return;
            }
            if player.placement.as_ref().is_some_and(|p| !p.story_advance_allowed) {
                return;
            }
            let Some(area) = self.areas.get(&player.area) else { return };
            area.runtime().story_advance(&player.progression, then.point)
        };
        if let Some(step) = step {
            self.apply_story_step(owner, then.point, step);
        }
        if then.on_entry {
            self.enter_story(owner, now);
        }
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
                        && !self.account_in_battle(p.account)
                        && p.transit.is_none()
                        && p.scene.is_none()
                        && p.story_pending.is_none()
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
        if !self.initialize_continuation(run, &record) {
            self.fail_scene_save(run, record, "the continuation could not be initialized");
            return;
        }
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
                gil: record.gil,
                replay: self.scenes.continuations.get(&run).map(|c| c.payload.replay.clone()),
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
            ClientMsg::SceneBattleReady { run, ordinal, step } => {
                self.continuation_battle_ready(entity, run, ordinal, step, now);
                SceneReply::Done
            }
            ClientMsg::SceneRetry { run } => {
                self.continuation_retry(entity, run, now);
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
        if self.scenes.continuations.contains_key(&run) {
            self.continuation_step(entity, run, step, answer, now);
            return;
        }
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

    /// A participant asks to be let go: the owner abandons the run without applying it; a watcher just stops watching.
    fn scene_leave(&mut self, entity: EntityId, run: SceneRunId, now: Instant) {
        if self.scenes.continuations.contains_key(&run) {
            self.continuation_end(entity, run, SceneOutcome::Aborted, now);
            return;
        }
        if let Some(pending) = self.scenes.pending.get_mut(&run) {
            // A queued commit cannot be cancelled. A watcher may detach without affecting it.
            if pending.record.owner == entity { return; }
            if pending.record.watchers.contains(&entity) {
                pending.record.watchers.retain(|e| *e != entity);
                let mut failed = Vec::new();
                self.release(entity, run, SceneOutcome::Skipped, &mut failed);
                self.drop_failed(failed);
            }
            return;
        }
        let Some(r) = self.scenes.runs.get_mut(&run) else { return };
        if r.owner == entity {
            self.end_run(run, SceneOutcome::Aborted, now);
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

    fn queue_scene_save(&mut self, run: SceneRunId, record: SceneRun, result: SceneResult, outcome: SceneOutcome) {
        let Some(transaction) = result.transaction else { return };
        let owner = self.players.get(&record.owner).filter(|p| p.scene == Some(run) && p.preview.is_none());
        let Some((account, progress, inventory)) = owner.map(|p| (p.account, p.progress, p.inventory.clone())) else {
            self.fail_scene_save(run, record, "the scene owner is no longer held");
            return;
        };
        if record.then.is_some() || transaction.expected_gil != record.gil || self.account_in_battle(account) {
            self.fail_scene_save(run, record, "the scene result does not match its resource snapshot");
            return;
        }
        let job = crate::saver::SceneSaveJob {
            account, scene_key: record.scene.clone(), expected_story: record.progress.clone(), next_story: result.progress.clone(),
            expected_gil: transaction.expected_gil, next_gil: transaction.next_gil,
        };
        let done = self.saver.as_ref().ok_or(crate::saver::SceneSaveError::Stopped).and_then(|saver| saver.save_scene(job));
        match done {
            Ok(done) => {
                self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
                self.scenes.pending.insert(run, PendingScene { record, account, result, outcome, progress, inventory, done });
            }
            Err(crate::saver::SceneSaveError::Stopped) if self.saver.is_some() => self.stop_for_scene_storage_recovery(),
            Err(_) => self.fail_scene_save(run, record, "the scene saver is unavailable"),
        }
    }

    /// A failed or uncertain commit never publishes a success. The owner reloads authoritative
    /// state after recovery; watchers have no changed save and can safely leave the presentation.
    fn fail_scene_save(&mut self, run: SceneRunId, record: SceneRun, reason: &str) {
        self.logs.warn(format!("scene save not confirmed run={} scene={} reason={reason}", run.0, clip(&record.scene, MAX_SCENE_ID_CHARS)));
        let mut failed = Vec::new();
        for watcher in record.watchers {
            if self.players.get(&watcher).is_some_and(|p| p.scene == Some(run)) {
                self.release(watcher, run, SceneOutcome::Aborted, &mut failed);
            }
        }
        if self.players.get(&record.owner).is_some_and(|p| p.scene == Some(run)) {
            self.send_to(record.owner, ServerMsg::Reject {
                code: fmv_protocol::RejectCode::Internal,
                reason: "The scene save could not be confirmed. Reconnect after server storage recovery.".into(),
            }, &mut failed);
            if let Some(player) = self.remove_player(record.owner, &mut failed) { player.close.send_replace(true); }
        }
        self.drop_failed(failed);
    }

    fn finish_scene_save(&mut self, run: SceneRunId, pending: PendingScene, saved: crate::saver::SceneSaved, now: Instant) {
        let PendingScene { record, account, result, outcome, progress, inventory: prior_inventory, .. } = pending;
        let game = self.areas.get(&self.home).map(|area| area.id().game);
        let valid = saved.pair.story.account_id == account && Some(saved.pair.story.game) == game
            && saved.pair.story.blob.len() <= fmv_protocol::MAX_STORY_PROGRESS_BYTES
            && saved.pair.character.as_ref().is_some_and(|c| c.account_id == account && Some(c.game) == game);
        if !valid {
            self.fail_scene_save(run, record, "the saver returned a mismatched record");
            return;
        }
        let current_owner = self.players.get(&record.owner).is_some_and(|p| p.account == account && p.scene == Some(run)
            && p.preview.is_none() && p.progression == record.progress && p.progress == progress && p.inventory == prior_inventory);
        let at_result = saved.pair.story.blob == result.progress;
        if current_owner {
            let character = saved.pair.character.as_ref().expect("validated above");
            let fixed_character = self.players.get(&record.owner).and_then(|p| p.progress).map_or(character.character, |p| p.character);
            let stored = crate::progress::Progress::from_record(character);
            let mut progress = self.battle.as_ref().map_or(stored, |battle| crate::progress::resolve(battle.rules(),
                &crate::progress::ProgressSeed::of(fixed_character, Some(stored))));
            // The host already computed the exact authored economic result. Stat repair must
            // not turn the acknowledged gil after-image into a second, different transaction.
            progress.gil = stored.gil;
            let inventory = self.battle.as_ref().map(|battle| crate::inventory::Inventory::resolve(battle.rules(), character.inventory.as_deref()));
            let player = self.players.get_mut(&record.owner).expect("checked above");
            player.progress = Some(progress);
            player.inventory = inventory;
            let summary = if at_result { result.summary.clone() } else { "current saved story after scene completion".into() };
            self.apply_saved_story_step(record.owner, SCENE_STEP_POINT, StoryStep { progress: saved.pair.story.blob, summary, countdown: None });
        } else if self.players.contains_key(&record.owner) {
            self.fail_scene_save(run, record, "the owner's in-memory story changed while its commit was pending");
            return;
        }
        let mut failed = Vec::new();
        if current_owner {
            self.tell_progress(record.owner, false, &mut failed);
            self.tell_inventory(record.owner, &mut failed);
        }
        let participants: Vec<_> = std::iter::once(record.owner).chain(record.watchers.iter().copied())
            .filter(|entity| self.players.get(entity).is_some_and(|p| p.scene == Some(run))).collect();
        for &entity in &participants { self.release(entity, run, outcome, &mut failed); }
        // A duplicate can return later progress. Never send it to the old scene's destination.
        if current_owner && at_result {
            if let Some(exit) = &result.travel {
                for entity in participants {
                    if let Some((pos, facing)) = self.players.get(&entity).map(|p| (p.snapshot.pos, p.snapshot.facing)) {
                        self.start_travel(entity, exit, pos, facing, now);
                    }
                }
            }
        }
        self.logs.info(format!("scene durably ended run={} scene={} outcome={outcome:?} connected={current_owner}", run.0, record.scene));
        self.drop_failed(failed);
    }

    /// End the presentation once. Durable results retain the hold until the saver acknowledges;
    /// removing the active run first makes repeated ends and timeouts harmless.
    fn end_run(&mut self, run: SceneRunId, mut outcome: SceneOutcome, now: Instant) {
        if let Some(owner) = self.scenes.runs.get(&run).filter(|_| self.scenes.continuations.contains_key(&run)).map(|r| r.owner) {
            self.continuation_end(owner, run, outcome, now);
            return;
        }
        if self.scenes.ordinary_finishing.contains_key(&run) { return; }
        // A v3 point scene is presentation for the point's exact write. Preserve that scene's
        // hold until the ordinary CAS and ordered input barrier have both completed.
        let point_save = self.scenes.runs.get(&run).and_then(|record| {
            let then = record.then.as_ref().filter(|then| then.battle.is_none())?;
            let player = self.players.get(&record.owner)?;
            let area = self.areas.get(&then.area)?;
            (!record.preview && player.area == then.area && player.scene == Some(run)
                && player.placement.as_ref().is_none_or(|placement| placement.story_advance_allowed)
                && area.runtime().story_requires_transaction(&player.progression))
                .then(|| (record.owner, then.point, area.runtime().story_advance(&player.progression, then.point)))
        });
        if let Some((owner, point, step)) = point_save {
            let Some(step) = step else {
                if let Some(record) = self.scenes.runs.remove(&run) {
                    self.fail_scene_save(run, record, "the exact point-scene result could not be interpreted");
                }
                return;
            };
            self.scenes.ordinary_finishing.insert(run, outcome);
            self.apply_story_step(owner, point, step);
            self.defer_story_followup(owner, super::story::StoryFollowup::Scene { run });
            return;
        }
        let Some(record) = self.scenes.runs.remove(&run) else { return };
        let mut apply = matches!(outcome, SceneOutcome::Finished | SceneOutcome::Skipped);
        let mut result = if !apply || !record.writes_story { None } else {
            self.scenes.host.as_ref().and_then(|host| {
                if record.preview { host.finish_preview_with_resources(&record.scene, &record.progress, record.seed, &record.answers, record.gil) }
                else { host.finish_with_resources(&record.scene, &record.progress, record.seed, &record.answers, record.gil) }
            })
        };
        if apply && record.writes_story && !record.preview && record.then.is_none() && result.is_none() {
            if outcome == SceneOutcome::Skipped {
                // Leaving before an economic choice is answered abandons the presentation. It
                // cannot invent an answer or publish success, but it is not a storage failure.
                outcome = SceneOutcome::Aborted;
                apply = false;
            } else {
                self.fail_scene_save(run, record, "the scene result could not be interpreted");
                return;
            }
        }
        let durable = !record.preview && result.as_ref().is_some_and(|r| r.transaction.is_some());
        // A story write must not strand a player when its required destination cannot load or
        // place an arrival. The same loaded area and placement are used by start_travel below.
        if !record.preview {
            if let Some(exit) = result.as_mut().and_then(|result| result.travel.as_mut()) {
                let target = self.ensure_area(&exit.target).ok();
                let valid = target.as_ref().and_then(|key| self.areas.get(key))
                    .is_some_and(|area| area.runtime().place_arrival(exit.arrival, exit.arrival_hint).is_finite());
                if !valid {
                    self.logs.warn(format!("scene destination unavailable scene={} target={}", clip(&record.scene, MAX_SCENE_ID_CHARS), clip(&exit.target, 128)));
                    result = None;
                } else if let Some(target) = target {
                    exit.target = target;
                }
            }
        }
        if durable {
            match result {
                Some(result) => self.queue_scene_save(run, record, result, outcome),
                None => self.fail_scene_save(run, record, "the required destination could not be loaded"),
            }
            return;
        }
        let mut failed = Vec::new();
        let mut everyone = vec![record.owner];
        everyone.extend(record.watchers.iter().copied());
        for &e in &everyone { self.release(e, run, outcome, &mut failed); }
        let story = if record.then.is_some() { "the point's step" } else if !apply { "dropped" } else if !record.writes_story { "none (read-only run)" } else if let Some(result) = &result {
            self.apply_scene_story(&record, result)
        } else { "none (the scene could not be run)" };
        // A debug preview never changes the real world. A real scene's destination is resolved and
        // validated by its host; each participant travels through the normal area handshake.
        if !record.preview {
            if let Some(exit) = result.as_ref().and_then(|result| result.travel.as_ref()) {
                for entity in everyone {
                    if let Some((pos, facing)) = self.players.get(&entity).map(|p| (p.snapshot.pos, p.snapshot.facing)) {
                        self.start_travel(entity, exit, pos, facing, now);
                    }
                }
            }
        }
        self.logs.info(format!("scene ended run={} scene={} outcome={outcome:?} answers={} story={story}", run.0, record.scene, record.answers.len()));
        self.drop_failed(failed);
        // A cutscene's point: its step, now that the run is over (however it ended; nothing if the owner is gone).
        if let Some(then) = record.then {
            self.point_step_after_scene(record.owner, then, now);
        }
        self.resume_story_followups_after_scene(run, record.owner, now);
    }

    /// The point's ordinary exact write has been acknowledged. Remaining trigger work belongs
    /// to the saved story sweep and is resumed by its caller after this presentation is released.
    pub(super) fn finish_story_point_scene(&mut self, run: SceneRunId, _now: Instant) {
        let Some(outcome) = self.scenes.ordinary_finishing.remove(&run) else { return; };
        let Some(record) = self.scenes.runs.remove(&run) else { return; };
        let mut failed = Vec::new();
        for entity in std::iter::once(record.owner).chain(record.watchers) {
            if self.players.get(&entity).is_some_and(|player| player.scene == Some(run)) {
                self.release(entity, run, outcome, &mut failed);
            }
        }
        self.drop_failed(failed);
    }

    /// What the scene's script wrote, applied to the owner's story once (see the module docs). Returns a word for the log.
    fn apply_scene_story(&mut self, record: &SceneRun, result: &SceneResult) -> &'static str {
        if !record.writes_story {
            return "none (not the owner's phase)";
        }
        if !result.changed {
            return "unchanged";
        }
        // What changed comes first: a client shows only the head of a summary, and a scene's id is long.
        let summary = format!("{} scene={}", result.summary, clip(&record.scene, MAX_SCENE_ID_CHARS));
        let Some(p) = self.players.get_mut(&record.owner) else { return "none (the owner is gone)" };
        if p.preview.is_none() {
            // A real story: a scene's write is a story step like any other (the mirror, the log line, a countdown it ends, the
            // save, the player told and the placements refreshed: `story::apply_story_step`).
            self.apply_story_step(record.owner, SCENE_STEP_POINT, StoryStep { progress: result.progress.clone(), summary, countdown: None });
            return "applied";
        }
        // A previewing player's story is made up: it is told the step and keeps it in memory only, and nothing is saved
        // (`apply_story_step` would not apply it either).
        let account = p.account;
        p.progression = result.progress.clone();
        self.logs.info(format!("story advanced (preview: made up, not saved) account={} entity={} {summary}", account.0, record.owner.0));
        let mut failed = Vec::new();
        self.tell_story(record.owner, Some(StoryAdvance { point: SCENE_STEP_POINT, summary }), &mut failed);
        let mut accounts = std::collections::BTreeSet::from([account]);
        if let Some(party) = self.parties.party_of(account) {
            accounts.extend(party.members.iter().copied());
        }
        self.refresh_story_placements(&accounts, &mut failed);
        self.drop_failed(failed);
        "applied (preview: not saved)"
    }

    /// Housekeeping, once per tick: end the runs that have gone quiet or lasted too long.
    pub fn advance_scenes(&mut self, now: Instant) {
        self.advance_story_saves(now);
        self.advance_continuations(now);
        let late: Vec<SceneRunId> = self
            .scenes
            .runs
            .iter()
            .filter(|(id, r)| !self.scenes.continuations.contains_key(id)
                && !self.scenes.ordinary_finishing.contains_key(id)
                && (now.saturating_duration_since(r.last_activity) >= SCENE_IDLE_TIMEOUT || now.saturating_duration_since(r.started) >= SCENE_MAX_TIME))
            .map(|(id, _)| *id)
            .collect();
        for run in late {
            self.end_run(run, SceneOutcome::TimedOut, now);
        }
        let ready: Vec<_> = self.scenes.pending.iter_mut().filter_map(|(&run, pending)| {
            match pending.done.try_recv() {
                Ok(result) => Some((run, result)),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some((run, Err(crate::saver::SceneSaveError::WorkerFailed))),
            }
        }).collect();
        for (run, result) in ready {
            let Some(pending) = self.scenes.pending.remove(&run) else { continue };
            self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
            match result {
                Ok(saved) => self.finish_scene_save(run, pending, saved, now),
                Err(error) => {
                    let fatal = match error {
                        crate::saver::SceneSaveError::Store(fmv_persist::PersistError::TransactionConflict(_)) => false,
                        crate::saver::SceneSaveError::WorkerFailed | crate::saver::SceneSaveError::Stopped | crate::saver::SceneSaveError::Store(_) => true,
                        _ => false,
                    };
                    if fatal { self.stop_for_scene_storage_recovery(); }
                    else { self.fail_scene_save(run, pending.record, "the durable scene preconditions were refused"); }
                }
            }
        }
    }

    /// The store is shared by every player. Continuing after an uncertain write would publish
    /// rewards and story updates which its poisoned writer cannot persist.
    pub(super) fn stop_for_scene_storage_recovery(&mut self) {
        self.scenes.storage_failed = true;
        self.logs.warn("scene storage requires recovery; disconnecting all players and refusing admission".into());
        let mut failed = Vec::new();
        let entities: Vec<_> = self.players.keys().copied().collect();
        for entity in entities {
            self.send_to(entity, ServerMsg::Reject { code: fmv_protocol::RejectCode::Internal,
                reason: "Server storage requires recovery. Please reconnect after the server restarts.".into() }, &mut failed);
            if let Some(player) = self.remove_player(entity, &mut failed) { player.close.send_replace(true); }
        }
        self.drop_failed(failed);
    }

    /// Disconnect abandons a presentation, but cannot cancel a transaction already queued.
    /// A pending account remains reserved until its acknowledgment; watchers can leave independently.
    pub(super) fn scene_player_gone(&mut self, entity: EntityId, held: Option<SceneRunId>) {
        let Some(run) = held else { return };
        if self.scenes.ordinary_finishing.contains_key(&run) {
            if let Some(record) = self.scenes.runs.get_mut(&run) { record.watchers.retain(|watcher| *watcher != entity); }
            return;
        }
        if self.scenes.continuations.contains_key(&run) { self.continuation_player_gone(entity, run); return; }
        if let Some(pending) = self.scenes.pending.get_mut(&run) {
            pending.record.watchers.retain(|w| *w != entity);
            return; // An owner's disconnect cannot cancel a transaction already queued.
        }
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
        for (id, r) in self.scenes.runs.iter().chain(self.scenes.pending.iter().map(|(id, pending)| (id, &pending.record))) {
            if self.scenes.continuations.contains_key(id) || self.scenes.ordinary_finishing.contains_key(id) { continue; }
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
            if let Some(pending) = self.scenes.pending.get_mut(&id) { pending.record.watchers.retain(|x| *x != w); }
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
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() || p.story_pending.is_some() {
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

    /// `entity` has just joined (in the entry area, like everyone): if its story stands somewhere else (`SceneHost::entry`),
    /// send it there through the ordinary area handshake. A same-field authored entry was
    /// already placed before the initial snapshot in `join`; an unavailable field leaves the
    /// player where it joined.
    pub(super) fn enter_at_story(&mut self, entity: EntityId, now: Instant) {
        let Some(host) = self.scenes.host.clone() else { return };
        let Some(player) = self.players.get(&entity) else { return };
        if player.preview.is_some() || player.transit.is_some() || player.battle.is_some() || player.scene.is_some()
            || (player.story_pending.is_some() && !self.ordinary_story_resuming(entity)) { return; }
        let Some(exit) = host.entry(&player.progression) else { return };
        if exit.target.eq_ignore_ascii_case(&player.area) { return; }
        let (account, from, facing) = (player.account, player.snapshot.pos, player.snapshot.facing);
        if self.start_travel(entity, &exit, from, facing, now) {
            self.logs.info(format!("story entry account={} entity={} to={} (where its story stands)", account.0, entity.0, clip(&exit.target, 128)));
        } else {
            self.logs.warn(format!("story entry unavailable account={} entity={} to={} (the player stays where everyone joins)", account.0, entity.0, clip(&exit.target, 128)));
        }
    }

    /// Start an offered arrival scene for this player only, after the area's entry story has run.
    pub(super) fn automatic_scene_entered(&mut self, entity: EntityId, now: Instant) {
        let Some(host) = self.scenes.host.clone() else { return };
        let Some(player) = self.players.get(&entity) else { return };
        if player.preview.is_some() || player.transit.is_some() || player.battle.is_some() || player.scene.is_some()
            || (player.story_pending.is_some() && !self.ordinary_story_resuming(entity)) { return; }
        if self.account_in_battle(player.account) { return; }
        let area = player.area.clone();
        // Arrival scenes are private to this player, even when its shared field follows the
        // party leader's phase. Judge and replay only the player's own story, never the leader's.
        let progress = player.progression.clone();
        let Some(info) = self.scenes.list.iter().find(|info| info.field.eq_ignore_ascii_case(&area)
            && host.automatic(&info.id) && !host.requires_landing_for(&info.id, &progress) && host.offered(&info.id, &progress)).cloned() else { return };
        let run = SceneRunId(self.scenes.next_run.max(1));
        self.scenes.next_run = run.0.saturating_add(1);
        let seed = u32::try_from(run.0.wrapping_mul(0x9E37_79B9) & 0xFFFF_FFFF).unwrap_or(1);
        let record = SceneRun { scene: info.id, owner: entity, watchers: vec![], seed, progress,
            gil: player.progress.map_or(0, |p| p.gil), answers: vec![], steps: 0,
            started: now, last_activity: now, writes_story: true, preview: false, then: None };
        self.start_run(run, record, &mut Vec::new());
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
