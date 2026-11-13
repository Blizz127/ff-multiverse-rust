//! Live fields (protocol 23): areas whose original scripts run on the server, frame by frame, for every instance.
//!
//! A [`LiveFieldHost`] (FF7: `crate::live_host`) starts one [`LiveField`] per instance of an area it can run, for the
//! story of the instance's GOVERNOR. On a live area an instance is keyed on the governor's account, not on the story,
//! because the story changes all the time there: [`World::live_phase`]. The governor is the player whose own phase
//! chose the instance (a party's leader, or a solo player).
//!
//! Every world tick runs the frames the 30 Hz field clock owes since the run started. The governor's avatar is the
//! field's player character: its position, facing and OK presses go in, and the scripts react to them (talk, lines,
//! pushes). What comes out is sent to the instance: the cast, when it changes; the windows, which only the governor
//! answers; control locks; where a script moved the player. A command the field cannot carry out itself is carried
//! out here: a map jump takes the whole instance to the next field, a battle starts with the field's party, a movie
//! is played by the clients, and a menu is answered for now (see `docs/FIELD-RUNTIME.md`).
//!
//! The story the field writes is saved as an exact story transaction (`StoryCasJob`), coalesced: at most one in
//! flight per run, at most one every [`SAVE_EVERY`], and once more before the run ends.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, Exit, ExitKind};
use fmv_core::{AccountId, EntityId, PhaseKey, Pos};
use fmv_party::InstanceId;
use fmv_protocol::{BattleId, LiveAsk, NpcSnapshot, ServerMsg};
use tokio::sync::oneshot;

use super::World;
use crate::saver::{SceneSaveError, SceneSaved, StoryCasJob};

/// Frames of the field clock per second.
pub const FIELD_FPS: u64 = 30;
/// Frames one tick may run at most (a stalled server catches up a second at a time, never a burst).
const MAX_FRAMES_PER_TICK: u64 = 30;
/// How often a running field's story is saved while it changes.
pub const SAVE_EVERY: Duration = Duration::from_secs(2);
/// How long a movie may take before the field goes on without the clients' answer.
const MOVIE_TIMEOUT: Duration = Duration::from_secs(180);
/// Frames an OK press stays held for the field's scripts (a fifth of a second).
const OK_HOLD_FRAMES: u8 = 6;
/// The phase-key prefix of a live instance.
const LIVE_PHASE: &[u8] = b"live";

/// The game's side: which areas run live and the running fields themselves.
pub trait LiveFieldHost: Send + Sync + std::fmt::Debug {
    /// Whether `area` can run live for a governor with story `progression` (a story the field could misread, one
    /// with unknown bits, never runs).
    fn runs(&self, area: &AreaKey, progression: &[u8]) -> bool;
    /// Start `area` for the story `progression`, with what the governor carries and the clocks it brings.
    fn start(&self, area: &AreaKey, progression: &[u8], seed: &LiveSeed) -> Option<Box<dyn LiveField>>;
}

/// What a field starts with besides the story: the governor's gil and items, the game clocks it carries from the field
/// before (play time and the story countdown, seconds) and the field it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveSeed {
    pub gil: Option<u32>,
    pub items: Vec<(u16, u16)>,
    pub play_time: Option<u32>,
    pub countdown: Option<u32>,
    pub previous_area: Option<AreaKey>,
}

/// How a battle a field started ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveBattleOutcome {
    Victory,
    Escaped,
    Defeat,
}

/// A live field's cast: everyone visible, which of them is the player's own script character, and the animation frame
/// of those a script plays once or holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LiveCastView {
    pub npcs: Vec<NpcSnapshot>,
    pub lead: Option<u32>,
    pub poses: Vec<fmv_protocol::LivePose>,
}

/// What the governor does this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveInput {
    pub pos: Pos,
    pub facing: f32,
    /// OK went down.
    pub ok: bool,
    /// OK is held (a press holds for a few frames, so a script that polls the pad sees it).
    pub ok_held: bool,
    /// The other pad buttons held (protocol 26 `LiveKeys`; never OK).
    pub keys: u16,
}

/// What a frame did that the world must carry out or tell.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    /// A window opened. `passive`: one the player cannot close (the countdown's clock, a standing notice).
    Window { window: u8, dialog: u16, ask: Option<(u8, u8)>, entity: u16, offset: u32, passive: bool },
    WindowClosed { window: u8 },
    /// A script sound effect on channel 1 or 2 (`id` 0 stops the channel).
    Sound { channel: u8, id: u16, pan: u8 },
    /// Go to another area (`target` is `None` when the field id names no area the adapter has).
    MapJump { target: Option<AreaKey>, field_id: u16, arrival: Pos, facing: f32, hint: Option<u32> },
    Battle { formation: u16, party: Vec<u8> },
    Movie { movie: u16 },
    Menu { kind: u8, param: u8 },
    GameOver,
    /// A script gave (positive) or took (negative) gil or items.
    Gil { delta: i64 },
    Item { item: u16, delta: i32 },
    /// Something worth a log line.
    Note(String),
}

/// One running field.
pub trait LiveField: Send {
    /// Run one frame.
    fn advance(&mut self, input: Option<LiveInput>) -> Vec<LiveEvent>;
    /// The visible models other than the player's.
    fn cast(&self) -> LiveCastView;
    /// The stage: the background's group masks and the fade overlay (blend, colour).
    fn stage(&self) -> (Vec<u8>, Option<(u8, [u8; 3])>);
    /// Whether a script holds the player still.
    fn control_locked(&self) -> bool;
    /// Where the scripts put the player's character (while control is locked).
    fn player_place(&self) -> Option<(Pos, f32)>;
    /// The governor answered window `window`.
    fn answer(&mut self, window: u8, choice: Option<u8>) -> bool;
    /// The pending command was carried out.
    fn complete_command(&mut self);
    /// The battle the field started ended (the command is then complete).
    fn battle_over(&mut self, outcome: LiveBattleOutcome);
    /// Whether the field is between scenes: no script holds the player, no window is open and no command is pending.
    /// Only then is its story saved (a save in the middle of a scene could store a state the original never could).
    fn idle(&self) -> bool;
    /// The game clocks now (play time, story countdown; seconds).
    fn clocks(&self) -> (u32, u32);
    /// The seconds left on the story countdown while the field shows its clock (`None`: no countdown is shown).
    fn countdown(&self) -> Option<u32>;
    fn map_jumps_disabled(&self) -> bool;
    /// Whether a script closed the way from `from` to `to` (FF7: `to` lies on a walkmesh triangle a script locked and
    /// `from` does not, so a player already inside can still walk out).
    fn blocks(&self, from: Pos, to: Pos) -> bool;
    /// The governor's move from `from` to `to` was refused because [`LiveField::blocks`] said so: whoever stood in the
    /// way may react (FF7: their contact script).
    fn bumped(&mut self, _from: Pos, _to: Pos) {}
    fn battles_disabled(&self) -> bool;
    /// The progression to save over `saved` (which this field started from or last saved), if the field changed it.
    fn progression(&self, saved: &[u8]) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq)]
enum Waiting {
    Battle(BattleId),
    Movie { movie: u16, since: Instant },
}

struct LiveRun {
    area: AreaKey,
    governor: AccountId,
    field: Box<dyn LiveField>,
    started: Instant,
    frames: u64,
    cast: LiveCastView,
    stage: (Vec<u8>, Option<(u8, [u8; 3])>),
    locked: bool,
    place: Option<(Pos, f32)>,
    /// The progression last saved (or the one the run started from).
    saved: Vec<u8>,
    in_flight: Option<(Vec<u8>, oneshot::Receiver<Result<SceneSaved, SceneSaveError>>)>,
    last_save: Instant,
    waiting: Option<Waiting>,
    windows: BTreeSet<u8>,
    /// Frames the governor's last OK press is still held for.
    ok_hold: u8,
    /// The countdown last told to the instance: the seconds left, how long it was when it started, when it was told.
    countdown: Option<(u32, u32, Instant)>,
    /// The run cannot go on (its save failed): it stops advancing and the instance keeps its last state.
    halted: bool,
    /// The field ended the game (the story countdown ran out). The governor returns where it stands with the clocks
    /// forgotten, so the countdown starts full again: a game over must never be a state a character cannot leave.
    game_over: bool,
}

/// The pad's OK bit, which `LiveKeys` never carries (OK is `Interact`).
const LIVE_PAD_OK: u16 = 0x0020;

/// The world's live fields.
#[derive(Default)]
pub(super) struct LiveWorld {
    pub host: Option<Arc<dyn LiveFieldHost>>,
    runs: BTreeMap<InstanceId, LiveRun>,
    /// OK presses since the last frame, by account.
    ok: BTreeSet<AccountId>,
    /// The pad buttons each account holds (OK excluded), as its client last said.
    keys: BTreeMap<AccountId, u16>,
    answers: Vec<(AccountId, u8, Option<u8>)>,
    movies_done: BTreeSet<AccountId>,
    /// Battles a run is waiting on, and how they ended.
    battle_results: BTreeMap<BattleId, LiveBattleOutcome>,
    /// The clocks and the last area each governor left a live field with (carried into the next one).
    carried: BTreeMap<AccountId, (u32, u32, AreaKey)>,
}

/// The phase of a live instance governed by `account`.
pub(super) fn live_phase(account: AccountId) -> PhaseKey {
    let mut key = LIVE_PHASE.to_vec();
    key.extend_from_slice(&account.0.to_le_bytes());
    PhaseKey(key)
}

/// The governor of a live phase.
fn governor_of(phase: &PhaseKey) -> Option<AccountId> {
    let rest = phase.0.strip_prefix(LIVE_PHASE)?;
    let bytes: [u8; 8] = rest.try_into().ok()?;
    Some(AccountId(u64::from_le_bytes(bytes)))
}

impl World {
    /// Serve live fields with `host` (`--field-runtime`).
    pub fn set_live_host(&mut self, host: Arc<dyn LiveFieldHost>) {
        self.live.host = Some(host);
    }

    /// The phase `player` has in `area` when the area runs live for it: its own live phase.
    pub(super) fn live_phase_of(&self, area: &super::areas::AreaState, account: AccountId, progression: &[u8]) -> Option<PhaseKey> {
        let host = self.live.host.as_ref()?;
        host.runs(area.key(), progression).then(|| live_phase(account))
    }

    /// The phase `player` has in `area`: its live phase where the area runs live for it, else the usual one.
    pub(super) fn effective_phase(&self, area: &super::areas::AreaState, player: &super::Player) -> PhaseKey {
        if player.preview.is_none() {
            if let Some(phase) = self.live_phase_of(area, player.account, &player.progression) {
                return phase;
            }
        }
        Self::player_phase(area, player)
    }

    /// The cast of live instance `id` (`Some(empty)` before its first frame; `None` when it is not live).
    pub(super) fn live_cast(&self, id: InstanceId) -> Option<Vec<NpcSnapshot>> {
        let key = self.instances.get(id)?.key().clone();
        governor_of(&key.phase)?;
        // (a snapshot's characters are plain: the player's own script character comes with the next cast, which says
        // which one it is)
        Some(self.live.runs.get(&id).map(|r| r.cast.npcs.iter().filter(|n| Some(n.id) != r.cast.lead).cloned().collect()).unwrap_or_default())
    }

    /// The run of the instance `entity` stands in, if it is live.
    fn live_run_of(&self, entity: EntityId) -> Option<InstanceId> {
        let account = self.players.get(&entity)?.account;
        let id = self.instances.instance_of(account)?;
        self.live.runs.contains_key(&id).then_some(id)
    }

    /// Whether `entity` stands in a live field (its moves skip the static story points and script exits, and no curated
    /// scene or story point starts for it: the field's own scripts play the story). True from the moment it arrives,
    /// before the field's first frame.
    pub(super) fn in_live_field(&self, entity: EntityId) -> bool {
        let Some(player) = self.players.get(&entity) else { return false };
        if player.preview.is_some() {
            return false;
        }
        if player.placement.as_ref().is_some_and(|p| governor_of(&p.key.phase).is_some() && p.key.area.key == player.area) {
            return true;
        }
        self.areas.get(&player.area).is_some_and(|area| self.live_phase_of(area, player.account, &player.progression).is_some())
    }

    /// Whether a script holds `entity` still: only the governor, whose avatar is the field's player character.
    pub(super) fn live_locked(&self, entity: EntityId) -> bool {
        let Some(account) = self.players.get(&entity).map(|p| p.account) else { return false };
        self.live_run_of(entity)
            .and_then(|id| self.live.runs.get(&id))
            .is_some_and(|r| r.governor == account && (r.locked || r.waiting.is_some()))
    }

    /// Whether the live field `entity` stands in closes the way from `from` to `to`.
    pub(super) fn live_blocks(&self, entity: EntityId, from: Pos, to: Pos) -> bool {
        self.live_run_of(entity).and_then(|id| self.live.runs.get(&id)).is_some_and(|r| r.field.blocks(from, to))
    }

    /// The live field refused `entity`'s move from `from` to `to`. Only the governor's character is the field's player.
    pub(super) fn live_bumped(&mut self, entity: EntityId, from: Pos, to: Pos) {
        let Some(account) = self.players.get(&entity).map(|p| p.account) else { return };
        if let Some(run) = self.live_run_of(entity).and_then(|id| self.live.runs.get_mut(&id)).filter(|r| r.governor == account) {
            run.field.bumped(from, to);
        }
    }

    pub(super) fn live_map_jumps_disabled(&self, entity: EntityId) -> bool {
        self.live_run_of(entity).and_then(|id| self.live.runs.get(&id)).is_some_and(|r| r.field.map_jumps_disabled())
    }

    pub(super) fn live_battles_disabled(&self, entity: EntityId) -> bool {
        self.live_run_of(entity).and_then(|id| self.live.runs.get(&id)).is_some_and(|r| r.field.battles_disabled())
    }

    /// The pad buttons `entity` holds now (OK excluded): the frames from now on see them, while it is in a live field.
    pub fn live_keys(&mut self, entity: EntityId, held: u16) {
        let Some(account) = self.players.get(&entity).map(|p| p.account) else { return };
        if self.in_live_field(entity) {
            self.live.keys.insert(account, held & !LIVE_PAD_OK);
        } else {
            self.live.keys.remove(&account);
        }
    }

    /// An OK press of `entity` in a live field: the next frame sees it. Returns whether it was taken.
    pub(super) fn live_press(&mut self, entity: EntityId) -> bool {
        let Some(account) = self.players.get(&entity).map(|p| p.account) else { return false };
        if !self.in_live_field(entity) {
            return false;
        }
        self.live.ok.insert(account);
        true
    }

    /// `ClientMsg::LiveAnswer`.
    pub fn live_answer(&mut self, entity: EntityId, window: u8, choice: Option<u8>) {
        if let Some(account) = self.players.get(&entity).map(|p| p.account) {
            self.live.answers.push((account, window, choice));
        }
    }

    /// `ClientMsg::LiveMovieDone`.
    pub fn live_movie_done(&mut self, entity: EntityId) {
        if let Some(account) = self.players.get(&entity).map(|p| p.account) {
            self.live.movies_done.insert(account);
        }
    }

    /// A battle ended: a run waiting on it goes on.
    pub(super) fn live_battle_finished(&mut self, id: BattleId, outcome: LiveBattleOutcome) {
        if self.live.runs.values().any(|r| r.waiting == Some(Waiting::Battle(id))) {
            self.live.battle_results.insert(id, outcome);
        }
    }

    /// Run every live instance's owed frames and carry out what they did. Called once per world tick.
    pub fn advance_live(&mut self, now: Instant) {
        let Some(host) = self.live.host.clone() else { return };
        // Start a run for every live instance without one; forget runs whose instance is gone.
        let live: Vec<(InstanceId, AreaKey, AccountId)> = self
            .instances
            .instances()
            .filter_map(|i| governor_of(&i.key().phase).map(|g| (i.id(), i.key().area.key.clone(), g)))
            .collect();
        let alive: BTreeSet<InstanceId> = live.iter().map(|(id, _, _)| *id).collect();
        let gone: Vec<InstanceId> = self.live.runs.keys().filter(|id| !alive.contains(id)).copied().collect();
        for id in gone {
            if let Some(run) = self.live.runs.remove(&id) {
                if run.game_over {
                    self.live.carried.remove(&run.governor);
                } else {
                    let (play, countdown) = run.field.clocks();
                    self.live.carried.insert(run.governor, (play, countdown, run.area.clone()));
                }
                self.save_live_story(&run, true);
                self.logs.info(format!("live field stopped area={} governor={} frames={}", run.area, run.governor.0, run.frames));
            }
        }
        for (id, area, governor) in live {
            if self.live.runs.contains_key(&id) {
                continue;
            }
            let Some(player) = self.by_account.get(&governor).and_then(|e| self.players.get(e)) else { continue };
            let progression = player.progression.clone();
            let carried = self.live.carried.get(&governor).cloned();
            let seed = LiveSeed {
                gil: player.progress.map(|p| p.gil),
                items: player.inventory.as_ref().map(|i| i.entries().iter().map(|e| (e.item, e.count)).collect()).unwrap_or_default(),
                play_time: carried.as_ref().map(|c| c.0),
                countdown: carried.as_ref().map(|c| c.1),
                previous_area: carried.map(|c| c.2),
            };
            match host.start(&area, &progression, &seed) {
                Some(field) => {
                    self.logs.info(format!("live field started area={area} governor={}", governor.0));
                    self.live.runs.insert(
                        id,
                        LiveRun {
                            area,
                            governor,
                            field,
                            started: now,
                            frames: 0,
                            cast: LiveCastView::default(),
                            stage: (Vec::new(), None),
                            locked: false,
                            place: None,
                            saved: progression,
                            in_flight: None,
                            last_save: now,
                            waiting: None,
                            windows: BTreeSet::new(),
                            ok_hold: 0,
                            countdown: None,
                            halted: false,
                            game_over: false,
                        },
                    );
                }
                None => self.logs.warn(format!("live field cannot start area={area} governor={}", governor.0)),
            }
        }
        let ids: Vec<InstanceId> = self.live.runs.keys().copied().collect();
        for id in ids {
            self.advance_run(id, now);
        }
        self.live.ok.clear();
        self.live.movies_done.clear();
        self.live.answers.clear();
    }

    fn advance_run(&mut self, id: InstanceId, now: Instant) {
        let Some(mut run) = self.live.runs.remove(&id) else { return };
        self.collect_live_save(&mut run);
        if run.halted {
            self.live.runs.insert(id, run);
            return;
        }
        let governor_entity = self.by_account.get(&run.governor).copied();
        // Answers and acknowledgements first.
        for (account, window, choice) in std::mem::take(&mut self.live.answers) {
            if account != run.governor {
                self.live.answers.push((account, window, choice));
            } else if run.field.answer(window, choice) {
                run.windows.remove(&window);
            }
            // An invalid/early answer for this governor is consumed, never offered to another run.
        }
        match run.waiting.clone() {
            Some(Waiting::Battle(battle)) => {
                if let Some(outcome) = self.live.battle_results.remove(&battle) {
                    run.waiting = None;
                    self.logs.info(format!("live battle over area={} battle={} outcome={outcome:?}", run.area, battle.0));
                    if outcome == LiveBattleOutcome::Defeat {
                        // Game over: the field stops; the story keeps its last save between scenes, where the governor comes
                        // back (MMO-style) the next time it logs in.
                        self.logs.warn(format!("live game over (battle lost) area={} governor={}", run.area, run.governor.0));
                        run.halted = true;
                    } else {
                        run.field.battle_over(outcome);
                    }
                }
            }
            Some(Waiting::Movie { movie, since }) => {
                if self.live.movies_done.contains(&run.governor) || now.saturating_duration_since(since) > MOVIE_TIMEOUT {
                    run.waiting = None;
                    run.field.complete_command();
                    self.logs.info(format!("live movie over area={} movie={movie}", run.area));
                }
            }
            None => {}
        }
        let mut ok = self.live.ok.contains(&run.governor);
        let owed = (now.saturating_duration_since(run.started).as_secs_f64() * FIELD_FPS as f64) as u64;
        let mut frames = owed.saturating_sub(run.frames).min(MAX_FRAMES_PER_TICK);
        if run.waiting.is_some() {
            // The field is suspended while its command is carried out (the original's field is not running then).
            run.frames = owed;
            frames = 0;
        }
        let mut events = Vec::new();
        // Where the scripts last put the governor's character during this tick's frames.
        let mut placed: Option<(Pos, f32)> = None;
        for _ in 0..frames {
            if ok {
                run.ok_hold = OK_HOLD_FRAMES;
            }
            let ok_held = run.ok_hold > 0;
            run.ok_hold = run.ok_hold.saturating_sub(1);
            let input = governor_entity.and_then(|e| self.players.get(&e)).filter(|p| p.transit.is_none() && p.battle.is_none()).map(|p| LiveInput {
                pos: p.snapshot.pos,
                facing: p.snapshot.facing,
                ok,
                ok_held,
                keys: self.live.keys.get(&run.governor).copied().unwrap_or(0),
            });
            ok = false;
            events.extend(run.field.advance(input));
            run.frames += 1;
            // While the scripts place the governor's character, its avatar is where they put it, frame by frame: the frame
            // after they let go must start from there, not from where the avatar stood before.
            if run.field.control_locked() {
                if let (Some((pos, facing)), Some(p)) = (run.field.player_place(), governor_entity.and_then(|e| self.players.get_mut(&e))) {
                    if p.snapshot.pos != pos || p.snapshot.facing != facing {
                        p.snapshot.pos = pos;
                        p.snapshot.facing = facing;
                        p.dirty = true;
                        placed = Some((pos, facing));
                    }
                }
            }
            if events.iter().any(|e| matches!(e, LiveEvent::MapJump { .. } | LiveEvent::Battle { .. } | LiveEvent::Movie { .. } | LiveEvent::GameOver)) {
                break;
            }
        }
        let area_id = self.areas.get(&run.area).map(|a| a.id().clone());
        let mut failed = Vec::new();
        let mut jump = None;
        for event in events {
            match event {
                LiveEvent::Window { passive: true, .. } => {}
                LiveEvent::Window { window, dialog, ask, entity: opener, offset, passive: false } => {
                    run.windows.insert(window);
                    if let Some(area) = &area_id {
                        for (entity, governor) in self.instance_members(id, run.governor) {
                            let msg = ServerMsg::LiveWindow { area: area.clone(), window, dialog, ask: ask.map(|(first, last)| LiveAsk { first, last }), answerable: governor, entity: opener, offset };
                            self.send_to(entity, msg, &mut failed);
                        }
                    }
                }
                LiveEvent::WindowClosed { window } => {
                    run.windows.remove(&window);
                    if let Some(area) = &area_id {
                        self.tell_instance(id, Arc::new(ServerMsg::LiveWindowClosed { area: area.clone(), window }), &mut failed);
                    }
                }
                LiveEvent::Sound { channel, id: sound, pan } => {
                    if let Some(area) = &area_id {
                        self.tell_instance(id, Arc::new(ServerMsg::LiveSound { area: area.clone(), channel, id: sound, pan }), &mut failed);
                    }
                }
                LiveEvent::Movie { movie } => {
                    run.waiting = Some(Waiting::Movie { movie, since: now });
                    self.tell_instance(id, Arc::new(ServerMsg::LiveMovie { movie }), &mut failed);
                    self.logs.info(format!("live movie area={} movie={movie}", run.area));
                }
                LiveEvent::Battle { formation, party } => match governor_entity.map(|e| self.begin_live_battle(e, formation, &party, now)) {
                    Some(Ok(battle)) => {
                        run.waiting = Some(Waiting::Battle(battle));
                        self.logs.info(format!("live battle area={} formation={formation} battle={}", run.area, battle.0));
                    }
                    other => {
                        // No battle can run (no battle support, the governor is away): the original's field would wait;
                        // here the fight is skipped as won so the story is never stuck, and the log says so.
                        self.logs.warn(format!("live battle skipped area={} formation={formation} reason={other:?}", run.area));
                        run.field.complete_command();
                    }
                },
                LiveEvent::Menu { kind, param } => {
                    self.logs.info(format!("live menu answered area={} kind={kind} param={param}", run.area));
                    run.field.complete_command();
                }
                LiveEvent::GameOver => {
                    self.logs.warn(format!("live game over area={} governor={}", run.area, run.governor.0));
                    run.halted = true;
                    run.game_over = true;
                }
                LiveEvent::MapJump { target, field_id, arrival, facing, hint } => {
                    jump = Some((target, field_id, arrival, facing, hint));
                }
                LiveEvent::Note(note) => self.logs.info(format!("live note area={} {note}", run.area)),
                LiveEvent::Gil { delta } => self.live_resources(run.governor, delta, None),
                LiveEvent::Item { item, delta } => self.live_resources(run.governor, 0, Some((item, delta))),
            }
        }
        // The cast, when it changed.
        let cast = run.field.cast();
        if cast != run.cast {
            if let Some(area) = &area_id {
                self.tell_instance(id, Arc::new(ServerMsg::LiveCast { area: area.clone(), tick: self.tick, npcs: cast.npcs.clone(), lead: cast.lead, poses: cast.poses.clone() }), &mut failed);
            }
            run.cast = cast;
        }
        // The stage, when it changed.
        let stage = run.field.stage();
        if stage != run.stage {
            if let Some(area) = &area_id {
                let fade = stage.1.map(|(blend, rgb)| fmv_protocol::LiveFade { blend, rgb });
                self.tell_instance(id, Arc::new(ServerMsg::LiveStage { area: area.clone(), masks: stage.0.clone(), fade }), &mut failed);
            }
            run.stage = stage;
        }
        // The story countdown, while the field shows its clock: told when it starts, every ten seconds, and when it ends.
        match (run.field.countdown(), run.countdown) {
            (Some(left), told) if told.is_none_or(|(_, _, at)| now.saturating_duration_since(at) >= Duration::from_secs(10)) => {
                let total = told.map_or(left, |(_, total, _)| total.max(left));
                run.countdown = Some((left, total, now));
                let state = fmv_protocol::CountdownState::Running { remaining_ms: left.saturating_mul(1000), total_ms: total.saturating_mul(1000) };
                self.tell_instance(id, Arc::new(ServerMsg::Countdown { state }), &mut failed);
            }
            (None, Some(_)) => {
                run.countdown = None;
                let state = fmv_protocol::CountdownState::Over { reason: fmv_protocol::CountdownEnd::Cleared };
                self.tell_instance(id, Arc::new(ServerMsg::Countdown { state }), &mut failed);
            }
            _ => {}
        }
        // Control and the scripted player position.
        let locked = run.field.control_locked();
        if locked != run.locked {
            run.locked = locked;
            self.tell_instance(id, Arc::new(ServerMsg::LiveControl { locked }), &mut failed);
        }
        // Tell the governor's client where the scripts put its character (the last place of this tick, also when the
        // scripts have let go since).
        if let (Some((pos, facing)), Some(entity)) = (placed, governor_entity) {
            self.send_to(entity, ServerMsg::LivePlace { pos, facing }, &mut failed);
        }
        run.place = placed.or(run.place.filter(|_| locked));
        // The story.
        let jumping = jump.is_some();
        if jumping || (run.field.idle() && run.waiting.is_none() && now.saturating_duration_since(run.last_save) >= SAVE_EVERY) {
            self.save_live_story_run(&mut run, now);
        }
        if jumping {
            let (play, countdown) = run.field.clocks();
            self.live.carried.insert(run.governor, (play, countdown, run.area.clone()));
        }
        self.live.runs.insert(id, run);
        self.drop_failed(failed);
        if let Some((target, field_id, arrival, facing, hint)) = jump {
            self.live_map_jump(id, target, field_id, arrival, facing, hint, now);
        }
    }

    /// A script gave or took gil or an item. Gains are paid like a battle's reward (saved through the same path, so a
    /// gift is never lost); losses are not saved yet and are logged (the original takes them in shops, inns and a few
    /// scripted payments, all still to be carried by the saver).
    fn live_resources(&mut self, governor: AccountId, gil: i64, item: Option<(u16, i32)>) {
        let gil_gain = u32::try_from(gil.max(0)).unwrap_or(u32::MAX);
        let items: Vec<fmv_core::InventoryEntry> = match item {
            Some((item, delta)) if delta > 0 && item < 128 => vec![fmv_core::InventoryEntry { item, count: u16::try_from(delta).unwrap_or(u16::MAX) }],
            _ => Vec::new(),
        };
        if gil < 0 || item.is_some_and(|(id, d)| d < 0 || id >= 128) {
            self.logs.warn(format!("live resource change not saved yet account={} gil={gil} item={item:?}", governor.0));
        }
        if gil_gain == 0 && items.is_empty() {
            return;
        }
        let mut reward = fmv_protocol::BattleReward::new(governor, 0, gil_gain, 0);
        reward.items = items;
        self.pay_live_reward(governor, reward);
    }

    /// The entities of instance `id` and whether each is its governor.
    fn instance_members(&self, id: InstanceId, governor: AccountId) -> Vec<(EntityId, bool)> {
        let Some(instance) = self.instances.get(id) else { return Vec::new() };
        instance.occupants().filter_map(|a| self.by_account.get(&a).map(|e| (*e, a == governor))).collect()
    }

    /// Queue the run's story if it changed and nothing is in flight; mirror it on the governor right away (the
    /// field is the authority for its own story while it runs).
    fn save_live_story_run(&mut self, run: &mut LiveRun, now: Instant) {
        if run.in_flight.is_some() {
            return;
        }
        let Some(next) = run.field.progression(&run.saved) else { return };
        run.last_save = now;
        if let Some(entity) = self.by_account.get(&run.governor).copied() {
            if let Some(p) = self.players.get_mut(&entity) {
                p.progression = next.clone();
            }
        }
        match self.saver.as_ref().map(|s| s.save_story_cas(StoryCasJob { account: run.governor, expected_story: run.saved.clone(), next_story: next.clone() })) {
            Some(Ok(rx)) => run.in_flight = Some((next, rx)),
            Some(Err(e)) => {
                self.logs.warn(format!("live story save refused area={} governor={} error={e:?}; the field stops", run.area, run.governor.0));
                run.halted = true;
            }
            None => run.saved = next, // a world without a saver (unit tests) keeps it in memory
        }
    }

    /// A stopping run's last save (its acknowledgement is not awaited: the saver keeps the order).
    fn save_live_story(&mut self, run: &LiveRun, _final: bool) {
        let base = run.in_flight.as_ref().map_or(&run.saved, |(next, _)| next);
        if let Some(next) = run.field.progression(base) {
            if let Some(saver) = self.saver.as_ref() {
                if saver.save_story_cas(StoryCasJob { account: run.governor, expected_story: base.clone(), next_story: next.clone() }).is_err() {
                    self.logs.warn(format!("live story final save refused area={} governor={}", run.area, run.governor.0));
                }
            }
            if let Some(p) = self.by_account.get(&run.governor).and_then(|e| self.players.get_mut(e)) {
                p.progression = next;
            }
        }
    }

    fn collect_live_save(&mut self, run: &mut LiveRun) {
        let Some((next, rx)) = run.in_flight.as_mut() else { return };
        match rx.try_recv() {
            Ok(Ok(_)) => {
                run.saved = std::mem::take(next);
                run.in_flight = None;
            }
            Ok(Err(e)) => {
                self.logs.warn(format!("live story save failed area={} governor={} error={e:?}; the field stops", run.area, run.governor.0));
                run.in_flight = None;
                run.halted = true;
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.logs.warn(format!("live story save unacknowledged area={} governor={}; the field stops", run.area, run.governor.0));
                run.in_flight = None;
                run.halted = true;
            }
        }
    }

    /// Take everyone in instance `id` to the next field.
    #[allow(clippy::too_many_arguments)]
    fn live_map_jump(&mut self, id: InstanceId, target: Option<AreaKey>, field_id: u16, arrival: Pos, facing: f32, hint: Option<u32>, now: Instant) {
        let Some(run) = self.live.runs.get(&id) else { return };
        let area = run.area.clone();
        let Some(target) = target else {
            self.logs.warn(format!("live map jump to an unknown field area={area} field_id={field_id}"));
            if let Some(run) = self.live.runs.get_mut(&id) {
                run.halted = true;
            }
            return;
        };
        let exit = Exit { line: (arrival, arrival), target: target.clone(), arrival, arrival_facing: Some(facing), arrival_hint: hint, kind: ExitKind::Script { id: u32::MAX } };
        let members: Vec<EntityId> = self.instance_members(id, run.governor).into_iter().map(|(e, _)| e).collect();
        self.logs.info(format!("live map jump area={area} to={target} members={}", members.len()));
        for entity in members {
            let Some((pos, f)) = self.players.get(&entity).map(|p| (p.snapshot.pos, p.snapshot.facing)) else { continue };
            self.start_travel(entity, &exit, pos, f, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, Pos};
    use fmv_fixture::rooms::{RoomsAdapter, HALL, YARD};
    use fmv_party::PartyConfig;
    use fmv_protocol::ServerMsg;
    use tokio::sync::{mpsc, watch};

    use super::super::{NewPlayer, Outbound, World};
    use super::*;
    use crate::saver::{Msg, SaverHandle};

    /// A field that opens one window on its first frame, and on an OK press jumps to the yard after writing its story.
    struct FakeField {
        frame: u64,
        open: bool,
        story: Vec<u8>,
        locked: bool,
    }

    impl LiveField for FakeField {
        fn advance(&mut self, input: Option<LiveInput>) -> Vec<LiveEvent> {
            self.frame += 1;
            let mut out = Vec::new();
            if self.frame == 1 {
                self.open = true;
                out.push(LiveEvent::Window { window: 0, dialog: 5, ask: None, entity: 2, offset: 77, passive: false });
            }
            if input.is_some_and(|i| i.ok) {
                self.story = vec![9];
                out.push(LiveEvent::MapJump { target: Some(YARD.to_string()), field_id: 1, arrival: Pos::new(150.0, -100.0, 0.0), facing: 0.0, hint: None });
            }
            out
        }
        fn cast(&self) -> LiveCastView {
            let npcs = vec![NpcSnapshot { id: 0, name: "walker".into(), model: "m".into(), idle_animation: None, pos: Pos::new(self.frame as f32, 0.0, 0.0), facing: 0.0 }];
            LiveCastView { npcs, lead: None, poses: Vec::new() }
        }
        fn stage(&self) -> (Vec<u8>, Option<(u8, [u8; 3])>) {
            (vec![0; 64], None)
        }
        fn control_locked(&self) -> bool {
            self.locked
        }
        fn player_place(&self) -> Option<(Pos, f32)> {
            None
        }
        fn answer(&mut self, _window: u8, _choice: Option<u8>) -> bool {
            std::mem::take(&mut self.open)
        }
        fn complete_command(&mut self) {}
        fn battle_over(&mut self, _outcome: LiveBattleOutcome) {}
        fn idle(&self) -> bool {
            !self.open
        }
        fn clocks(&self) -> (u32, u32) {
            (0, 0)
        }
        fn countdown(&self) -> Option<u32> {
            None
        }
        fn blocks(&self, _from: Pos, to: Pos) -> bool {
            to.x > 250.0 && to.y < 120.0
        }
        fn map_jumps_disabled(&self) -> bool {
            false
        }
        fn battles_disabled(&self) -> bool {
            true
        }
        fn progression(&self, saved: &[u8]) -> Option<Vec<u8>> {
            (self.story != saved).then(|| self.story.clone())
        }
    }

    #[derive(Debug)]
    struct FakeHost(Mutex<Vec<String>>);

    impl LiveFieldHost for FakeHost {
        fn runs(&self, area: &AreaKey, _progression: &[u8]) -> bool {
            area == HALL
        }
        fn start(&self, area: &AreaKey, progression: &[u8], _seed: &LiveSeed) -> Option<Box<dyn LiveField>> {
            self.0.lock().unwrap().push(area.clone());
            Some(Box::new(FakeField { frame: 0, open: false, story: progression.to_vec(), locked: false }))
        }
    }

    struct Seat {
        entity: EntityId,
        rx: mpsc::Receiver<Outbound>,
    }

    impl Seat {
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            out
        }
    }

    fn world() -> (World, mpsc::UnboundedReceiver<Msg>, Arc<FakeHost>) {
        let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (handle, rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        let host = Arc::new(FakeHost(Mutex::new(Vec::new())));
        w.set_live_host(host.clone());
        (w, rx, host)
    }

    fn seat(w: &mut World, name: &str, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(256);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: Vec::new(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now).expect("joins");
        Seat { entity, rx }
    }

    #[test]
    fn a_live_field_runs_for_its_governor_shows_its_cast_and_window_and_takes_only_the_governors_answer() {
        let t0 = Instant::now();
        let (mut w, _saved, host) = world();
        let mut a = seat(&mut w, "A", t0);
        assert!(w.in_live_field(a.entity), "the hall runs live from the moment the player stands in it");
        w.advance_live(t0 + Duration::from_millis(100));
        w.advance_live(t0 + Duration::from_millis(200));
        assert_eq!(*host.0.lock().unwrap(), [HALL.to_string()], "one field for the one instance");
        let msgs = a.drain();
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::LiveCast { npcs, .. } if npcs.len() == 1)), "{msgs:?}");
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::LiveWindow { window: 0, dialog: 5, answerable: true, entity: 2, offset: 77, .. })), "{msgs:?}");
        // The governor dismisses it.
        w.live_answer(a.entity, 0, None);
        w.advance_live(t0 + Duration::from_millis(300));
        assert!(!w.live_locked(a.entity));
    }

    #[test]
    fn a_script_locked_way_is_refused_and_ok_goes_to_the_field_which_can_take_the_instance_elsewhere_and_saves_its_story() {
        let t0 = Instant::now();
        let (mut w, mut saved, _host) = world();
        let mut a = seat(&mut w, "A", t0);
        w.advance_live(t0 + Duration::from_millis(100));
        w.live_answer(a.entity, 0, None);
        // The field closed part of the hall.
        let from = w.players.get(&a.entity).unwrap().snapshot.pos;
        assert!(w.live_blocks(a.entity, from, Pos::new(260.0, 100.0, 0.0)));
        assert!(matches!(
            w.apply_move(a.entity, Pos::new(260.0, 100.0, 0.0), 0.0, AnimState::Walk, t0 + Duration::from_millis(900)),
            crate::world::MoveOutcome::Rejected { .. }
        ));
        a.drain();
        // OK is the field's: it jumps to the yard.
        assert_eq!(w.press(a.entity, None, t0 + Duration::from_secs(1)), crate::world::PressOutcome::Nothing);
        w.advance_live(t0 + Duration::from_millis(1100));
        let msgs = a.drain();
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::AreaChange { area, .. } if area.key == YARD)), "{msgs:?}");
        let mut stories = Vec::new();
        while let Ok(msg) = saved.try_recv() {
            if let Msg::StoryCas { job, .. } = msg {
                stories.push((job.expected_story.clone(), job.next_story.clone()));
            }
        }
        assert_eq!(stories, [(Vec::new(), vec![9])], "the story the field wrote is saved as an exact step when it leaves");
    }
}
