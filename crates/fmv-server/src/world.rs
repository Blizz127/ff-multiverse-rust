//! Authoritative world state: the hosted areas and the players in them.
//!
//! The world hosts many areas (see the `areas` module): the entry area the server was started with
//! is always loaded, any other is loaded when the first player walks through an exit that leads
//! there and unloaded again some time after the last one left. Every area has its own players,
//! rules, exits and NPCs; broadcasts (`PlayerJoined`, `PlayerLeft`, `EntityUpdate`) reach only the
//! players of the area they are about. Parties, accounts and battles span the world: a party may
//! be spread over several areas (a battle takes the members that stand in one area).
//!
//! Everything in here is synchronous and never touches a socket. Outbound messages are
//! pushed with non-blocking `try_send` into each connection's bounded queue. A client whose
//! queue is full (or already closed) is removed from the world and told to close through its
//! close flag, so one slow client can never stall the others or the tick loop.
//!
//! All time-dependent logic takes an explicit `Instant`, which keeps it deterministic under
//! test without sleeping.
//!
//! # Movement rules
//!
//! A `MoveIntent` is a straight chord from the entity's authoritative position to the position
//! the client claims. It is applied only if it passes, in this order:
//!
//! 1. **Finite**: no NaN/infinite position or facing. Facing is normalized into `(-pi, pi]`
//!    before it is stored and echoed to the other players.
//! 2. **Speed**: every player owns a travel budget, a token bucket measured in horizontal
//!    distance units on *server* time. It refills at `max_speed * SPEED_SLACK` units per second,
//!    holds at most [`MAX_ELAPSED`] worth of that, and starts empty when the player spawns. An
//!    accepted move costs its distance; a move costing more than the budget is `TooFast`
//!    (refused moves cost nothing). Because the budget is spent rather than re-granted per
//!    message, intents that arrive bunched together (TCP coalescing, jitter, a scheduling stall
//!    on either side) are judged against the time that really passed, and sending many small
//!    messages cannot beat `max_speed * SPEED_SLACK` on average: the only slack beyond that is a
//!    one-off float tolerance, never a per-message allowance.
//! 3. **Walkable**: adapters only validate a destination (`AreaRuntime::validate_move`), so the
//!    chord is walked in sub-steps of at most `max_speed / SUBSTEPS_PER_SEC` units (one 60 fps
//!    frame of travel), each validated from the previous accepted step. The move happens only if
//!    every sub-step is accepted; a hole or wall inside the chord leaves the entity where it was
//!    (`Blocked`). Known gap: a chord that clips the very corner of a reflex bend can be refused
//!    although the client's real path (validated frame by frame on its side) did not, which costs
//!    that client one snap-back.
//!
//! The speed rule measures horizontal distance only. Height is never taken from the client: the
//! adapter is shown the entity's own height and decides the new one, so vertical travel is
//! whatever the walkable surface allows for the horizontal travel that was paid for.
//!
//! # Accounts, parties, placement
//!
//! Every player belongs to an authenticated account (one session per account at a time: a second
//! `join` for an account that is already in the world is refused, atomically). The world owns the
//! one [`PartyManager`] and routes its events: a request acts for the account of the REQUESTING
//! session (never an id from a message), every event goes to exactly the accounts in
//! `PartyEvent::audience`, and a player that leaves the world for any reason (disconnect,
//! shutdown, a slow client being dropped) leaves its party and has its pending invites withdrawn
//! in the same step, so no ghost member or ghost invite can outlive a connection. The party clock
//! is milliseconds of the `Instant`s the caller passes in. Placement
//! (`fmv_party::InstanceSelector`) is told to each player on join, whenever a party change
//! or the story alters it and whenever the player enters an area (it names the player's own
//! area and the phase that chose its instance: its own when solo, its leader's in a party).
//!
//! # Instances and the story
//!
//! A placement is not informational: the world keeps one INSTANCE of an area per story phase that
//! somebody is in, and every broadcast about an area reaches only the players of one instance. See
//! the `instances` module (who is where, and how a player changes instance) and the `story` module (what
//! reaching a story point does to a player's progression). Every account that exists today has the
//! empty progression, which is the empty phase in every area, so they all share one instance of each
//! area until one of them advances the story.
//!
//! # Areas and travel
//!
//! See the `areas` module: exits, the transfer between areas, lazy loading and unloading.
//!
//! # Battles
//!
//! A server with battle support runs shared battles in the world (see the `battle` module): a
//! player in a battle is frozen (its moves are ignored), the engine is stepped on the world tick
//! and every member is told what happened. The random-encounter table of the area a player walks
//! in, if it has one, is rolled as it walks; a battle takes the party members that stand in the
//! requester's area.
//!
//! # Character progress
//!
//! On such a server every player carries a mirror of its account's own character progress (the
//! character it fights as (its combat slot) and its level, experience, AP and gil; see `crate::progress`), loaded at
//! login and handed to [`World::join`] in [`NewPlayer::progress`], which also tells the player where
//! it stands (the third message after the snapshot and the placement). A battle fights each member
//! as its own character at its own level, and a victory pays it (see the `battle` module): the mirror
//! is updated under the world lock, in memory, and the same payment is queued for the saver
//! (`crate::saver`), which writes it to the store off the lock and off the async executor. Nothing in
//! the world does disk I/O. The player's inventory (`crate::inventory`) follows the same pattern: a
//! mirror per player, told to it as the fourth message of a join, changed by the items it uses and the
//! drops it is paid, every change queued for the saver.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, AreaRuntime, Exit, GameAdapter, LinkArming, MoveResult};
use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, Pos, SessionId};
use fmv_party::{InstanceRegistry, Invite, PartyConfig, PartyEvent, PartyManager, Placement, Tick};
use fmv_protocol::{BattleId, PartyName, ServerMsg};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};

use crate::deferred::{DeferredLogs, Line};
use crate::inventory::Inventory;
use crate::progress::{self, Progress, ProgressSeed};
use crate::saver::SaverHandle;
use crate::ServerError;

mod areas;
mod battle;
mod countdown;
mod follow;
mod instances;
mod party;
mod press;
mod scene;
mod story;
pub use areas::{TravelReply, AREA_UNLOAD_GRACE, TRAVEL_TIMEOUT};
pub use battle::BattleReply;
pub use party::PartyOutcome;
pub use press::PressOutcome;
pub use scene::{SceneBoundary, SceneBoundaryKind, SceneHost, SceneInfo, SceneReply, SceneResult, SceneSettings, SceneTransaction, TriggerInfo};

/// A message queued for one or more connections. Broadcasts share one allocation.
pub type Outbound = Arc<ServerMsg>;
pub type OutboundTx = mpsc::Sender<Outbound>;
/// Set to `true` to tell a connection task to stop (slow client, dead writer).
pub type CloseFlag = Arc<watch::Sender<bool>>;

/// A player may travel `SPEED_SLACK * max_speed` units per second on average.
pub const SPEED_SLACK: f64 = 1.5;
/// The travel budget never banks more than this much time worth of travel, however long the
/// client was silent (so a long pause cannot be turned into a teleport).
pub const MAX_ELAPSED: Duration = Duration::from_millis(500);
/// One-off tolerance for float noise, as a fraction of `max_speed` (in units). The budget may be
/// overdrawn by this much, but the debt is carried, never forgiven, so it does not add up.
pub const SPEED_TOLERANCE: f64 = 1e-5;
/// A chord is validated at least this many times per second of `max_speed` travel.
pub const SUBSTEPS_PER_SEC: f64 = 60.0;
/// Hard bound on adapter calls per `MoveIntent`. The budget cap already limits a chord to about
/// `SPEED_SLACK * MAX_ELAPSED * SUBSTEPS_PER_SEC = 45` sub-steps; this only guards the loop
/// against numeric surprises.
pub const MAX_SUBSTEPS: usize = 64;

/// Movement limits derived from the area's `max_speed`.
#[derive(Debug, Clone, Copy)]
struct MoveRules {
    /// Budget refill in distance units per second: `max_speed * SPEED_SLACK`.
    rate: f64,
    /// Most budget a player can hold: `rate * MAX_ELAPSED`.
    cap: f64,
    /// One-off float tolerance in distance units.
    tolerance: f64,
    /// Longest stretch of a chord handed to the adapter in one call.
    substep: f64,
}

impl MoveRules {
    fn new(max_speed: f32) -> Self {
        let v = f64::from(max_speed);
        let rate = v * SPEED_SLACK;
        Self { rate, cap: rate * MAX_ELAPSED.as_secs_f64(), tolerance: v * SPEED_TOLERANCE, substep: v / SUBSTEPS_PER_SEC }
    }
}

/// A player's travel budget (token bucket in distance units, on server time).
///
/// Invariant: `tokens >= -tolerance` (an accepted move never costs more than
/// `tokens + tolerance`) and `tokens <= cap` after a refill.
#[derive(Debug, Clone, Copy)]
struct MoveBudget {
    tokens: f64,
    /// Server time up to which the budget has been refilled.
    last: Instant,
}

impl MoveBudget {
    /// Empty at `now`: right after spawning, only the time since spawning has been earned.
    fn new(now: Instant) -> Self {
        Self { tokens: 0.0, last: now }
    }

    /// Full at `now`: what a player arrives with after crossing into another area. The client
    /// keeps walking through the transfer and sends its first move there before the server has
    /// had the time to earn it anything; an empty budget would turn that into a rejection (and a
    /// visible snap-back) at every crossing. A full one is worth at most half a second of travel.
    fn full(now: Instant, rules: &MoveRules) -> Self {
        Self { tokens: rules.cap, last: now }
    }

    fn refill(&mut self, now: Instant, rules: &MoveRules) {
        // `None` means `now` is not after `last` (callers pass monotonic times); nothing to add.
        if let Some(dt) = now.checked_duration_since(self.last) {
            self.tokens = (self.tokens + dt.as_secs_f64() * rules.rate).min(rules.cap);
            self.last = now;
        }
    }

    /// Whether a move covering `dist` units can be paid for. A NaN distance never can. Turning
    /// or changing animation on the spot covers no distance, costs nothing and is always
    /// allowed, even when float noise leaves the carried debt a hair past the tolerance.
    fn can_afford(&self, dist: f64, rules: &MoveRules) -> bool {
        dist == 0.0 || matches!(dist.partial_cmp(&(self.tokens + rules.tolerance)), Some(Ordering::Less | Ordering::Equal))
    }
}

struct Player {
    session: SessionId,
    /// The authenticated account of this session (never taken from a message).
    account: AccountId,
    /// The account's story progression for the hosted game: the blob only the adapter reads (empty
    /// for a new account), mirrored here and changed by `story`. The phase of each area comes from it.
    progression: Vec<u8>,
    snapshot: EntitySnapshot,
    /// Travel budget for the speed rule.
    budget: MoveBudget,
    /// Changed since the last tick broadcast.
    dirty: bool,
    tx: OutboundTx,
    close: CloseFlag,
    /// Party events delivered to this player so far (the `seq` of the last one).
    party_seq: u64,
    /// When this player's recent invites were created, oldest first (the invite rate limit).
    invites: VecDeque<Instant>,
    /// What the last `InstancePlacement` sent to this player said.
    placement: Option<Placement>,
    /// The battle this player is in, if any: it is frozen in the field until that battle ends.
    battle: Option<BattleId>,
    /// What this player's character has earned (the mirror of its stored character progress; see
    /// `progress`). `None` on a server that runs no battles.
    progress: Option<Progress>,
    /// What this player carries (the mirror of its stored inventory; see `inventory`). `None` on a server
    /// that runs no battles.
    inventory: Option<Inventory>,
    /// Horizontal distance walked since the last random-encounter roll.
    walked: f64,
    /// The area the player stands in (a key of `World::areas`). While the player is in transit it
    /// is the area it left, which stays loaded until the transfer is over.
    area: AreaKey,
    /// Set while the player is between two areas (see the `areas` module): it is in no area,
    /// nobody sees it and its moves are ignored.
    transit: Option<areas::Transit>,
    /// Which walk-on links of the area the player can fire (see the `press` module): placed anew at every entry and
    /// relocation.
    arming: LinkArming,
    /// Set after a link moved the player: until the client has taken the new position (an intent that agrees with it)
    /// every intent is answered with that position (see the `press` module).
    fence: Option<press::Fence>,
    /// The story scene this player is held in (as owner or watcher), if any (see the `scene` module).
    scene: Option<fmv_protocol::SceneRunId>,
    /// Set while the player is in a debug scene preview: its `progression` is made up and nothing of it is ever saved.
    preview: Option<scene::Preview>,
    /// The account whose countdown this player was last told is running (see the `countdown` module): `None` when it was
    /// told of none, or that it is over.
    countdown_told: Option<AccountId>,
    /// The ways out of an area that this player, as the leader of a party, took through an exit its story decided: what
    /// its members follow (see the `follow` module).
    ways: Vec<follow::WayOut>,
}

/// Everything the world needs to admit a player into the area.
pub struct NewPlayer {
    pub session: SessionId,
    pub entity: EntityId,
    /// The authenticated account. At most one session per account may be in the world.
    pub account: AccountId,
    pub avatar: AvatarAppearance,
    /// The account's story progression for the hosted game (`accounts::Login::progression`): empty for a
    /// new account. Whatever the area's adapter does not read keeps one phase per distinct blob.
    pub progression: Vec<u8>,
    /// The character it fights as (its combat slot) and what the store holds of its character progress (see
    /// `progress`; unrelated to `progression`).
    pub progress: ProgressSeed,
    /// The exact paired record loaded with this account's authentication snapshot.
    pub scene_state: Option<fmv_persist::SceneState>,
    /// The coherent durable continuation cohort (owner first), or the account's inactive singleton.
    pub scene_group: Vec<fmv_persist::SceneState>,
    pub tx: OutboundTx,
    pub close: CloseFlag,
}

/// Why a player could not be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// The account already has a session in the world.
    AlreadyOnline,
    /// A scene transaction crossed the storage snapshot used by this handshake; reload before joining.
    ProgressChanged,
    /// The shared store or save worker requires recovery before gameplay can resume.
    StorageUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// NaN or infinite position/facing.
    NonFinite,
    /// Costs more than the player's travel budget holds (see the module docs).
    TooFast,
    /// The adapter refused the destination or a point on the way (off the walkable surface).
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MoveOutcome {
    Applied,
    /// The move was refused. `corrected` is the authoritative position to send back.
    Rejected { corrected: Pos, reason: Rejection },
    /// The player was moved to `corrected` (by a link, see the `press` module), or its move came from a client that
    /// has not taken that yet. `corrected` is the authoritative position to send back, as for a refusal: but this is
    /// no refusal, and a connection does not count or log it as one.
    Relocated { corrected: Pos },
    /// The player is in a battle: the move is ignored (not applied, not answered).
    Frozen,
    /// The entity is no longer in the world (dropped or disconnected).
    Gone,
}

pub struct World {
    /// The loaded areas by key. The entry area (`home`) is always among them; the others come and
    /// go (see the `areas` module).
    areas: BTreeMap<AreaKey, areas::AreaState>,
    /// The key of the entry area: where every player joins, and the one area that is never unloaded.
    home: AreaKey,
    /// Where the other areas come from (`None`: a single-area world, which has no exits to follow).
    source: Option<Arc<dyn GameAdapter>>,
    /// How long a player may take to answer an `AreaChange`, and how long an empty area stays
    /// loaded.
    travel_timeout: Duration,
    unload_grace: Duration,
    next_entity: u64,
    next_session: u64,
    tick: u64,
    /// Ordered by entity id so snapshots are deterministic.
    players: BTreeMap<EntityId, Player>,
    /// The entity of each account's session.
    by_account: BTreeMap<AccountId, EntityId>,
    parties: PartyManager,
    /// Who stands in which instance of which area (area + story phase): the unit of visibility. See
    /// `instances`.
    instances: InstanceRegistry,
    /// Origin of the party clock: party `Tick`s are milliseconds since this instant.
    epoch: Instant,
    /// Battle support (`None`: this server cannot run battles). See `battle`.
    battle: Option<battle::BattleWorld>,
    /// Where what battles pay is queued for saving (`None`: nothing is saved, as in the unit tests of
    /// the world). See `saver`.
    saver: Option<SaverHandle>,
    /// Log lines recorded under the world lock, written after it is released (see `take_logs`).
    logs: DeferredLogs,
    /// Story scenes: the game's scenes, the runs in progress and the debug preview switch (see `scene`).
    scenes: scene::SceneWorld,
    /// The countdowns that story steps started, one per account (see `countdown`).
    countdowns: BTreeMap<AccountId, countdown::Countdown>,
    /// The `now` of the last [`World::advance_countdowns`]: what a countdown's time left is measured against.
    clock: Instant,
}

impl World {
    /// A world with the default party rules (three members, 60 s invites).
    #[cfg(test)]
    pub fn new(area: Box<dyn AreaRuntime>) -> Result<Self, ServerError> {
        Self::with_party_config(area, PartyConfig::default())
    }

    /// A world whose entry area is `area`. It has no other areas until
    /// [`World::set_area_source`] says where to get them.
    pub fn with_party_config(area: Box<dyn AreaRuntime>, party: PartyConfig) -> Result<Self, ServerError> {
        let state = areas::AreaState::new(area, None)?;
        let home = state.key().clone();
        let mut logs = DeferredLogs::default();
        // (the entry area is not loaded through `ensure_area`, so what its adapter left off is said here)
        if let Some(line) = state.withheld_line() {
            logs.info(line);
        }
        Ok(Self {
            areas: BTreeMap::from([(home.clone(), state)]),
            home,
            source: None,
            travel_timeout: TRAVEL_TIMEOUT,
            unload_grace: AREA_UNLOAD_GRACE,
            next_entity: 1,
            next_session: 1,
            tick: 0,
            players: BTreeMap::new(),
            by_account: BTreeMap::new(),
            parties: PartyManager::new(party),
            instances: InstanceRegistry::new(),
            epoch: Instant::now(),
            battle: None,
            saver: None,
            logs,
            scenes: scene::SceneWorld::default(),
            countdowns: BTreeMap::new(),
            clock: Instant::now(),
        })
    }

    /// Queue what battles pay into `saver` from now on (see `saver`). Without it payments still
    /// reach the players' mirrors and messages, but nothing is stored.
    pub(crate) fn set_progress_saver(&mut self, saver: SaverHandle) {
        self.saver = Some(saver);
    }

    /// The log lines recorded while the world lock was held since the last call. Code in the
    /// world never logs directly (a blocked log sink under the lock would stop every connection):
    /// it records through `self.logs` and `deferred::with_locked` writes the lines once the lock
    /// is released.
    pub(crate) fn take_logs(&mut self) -> Vec<Line> {
        self.logs.take()
    }

    #[cfg(test)]
    pub(crate) fn defer_warn(&mut self, text: &str) {
        self.logs.warn(text.to_string());
    }

    #[cfg(test)]
    pub(crate) fn defer_info(&mut self, text: &str) {
        self.logs.info(text.to_string());
    }

    #[cfg(test)]
    pub(crate) fn defer_debug(&mut self, text: &str) {
        self.logs.debug(text.to_string());
    }

    pub fn current_tick(&self) -> u64 {
        self.tick
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Reserve the session and entity ids reported in `Welcome`. The entity only appears in the
    /// world once the client joins the area.
    pub fn allocate_ids(&mut self) -> (SessionId, EntityId) {
        let ids = (SessionId(self.next_session), EntityId(self.next_entity));
        self.next_session += 1;
        self.next_entity += 1;
        ids
    }

    /// Whether `account` has a session or a scene save awaiting acknowledgment in the world.
    pub fn is_online(&self, account: AccountId) -> bool {
        self.by_account.contains_key(&account) || self.scene_save_pending(account)
    }

    /// Whether this account has a connected player. A durable reservation alone is recoverable
    /// and must reach the final admission checks instead of being mistaken for a second session.
    pub fn has_session(&self, account: AccountId) -> bool {
        self.by_account.contains_key(&account)
    }

    /// Spawn the player (round-robin over the spawn points), queue the full `AreaSnapshot` and
    /// then the `InstancePlacement` to the joiner (always the first things in its queue after the
    /// handshake replies) and `PlayerJoined` to everyone else. The movement budget starts empty.
    /// Returns the spawn position, or refuses a second session for the same account.
    /// Atomically fence admission against scene saves that began or ended after authentication
    /// started reading storage. The bounded global fence can require an unrelated login to retry.
    pub(crate) fn join_with_scene_epoch(&mut self, new: NewPlayer, now: Instant, read_epoch: u64) -> Result<Pos, JoinError> {
        if read_epoch != self.scene_save_epoch() { return Err(JoinError::ProgressChanged); }
        self.join(new, now)
    }

    pub fn join(&mut self, new: NewPlayer, now: Instant) -> Result<Pos, JoinError> {
        if self.scenes.storage_failed { return Err(JoinError::StorageUnavailable); }
        // Checked here, under the world lock, because two handshakes with the same token can both
        // pass authentication.
        let recovering = new.scene_state.as_ref().is_some_and(|state| state.checkpoint.is_some() || state.reservation.is_some());
        if self.has_session(new.account) || (!recovering && self.scene_save_pending(new.account)) {
            return Err(JoinError::AlreadyOnline);
        }
        // Authentication supplies one coherent pair and its cohort. Do not admit a mixture of
        // that pair and a separately loaded story/resource seed, or a detached reservation.
        match &new.scene_state {
            Some(state) if state.pair.story.account_id == new.account
                && state.pair.story.game == self.home_game()
                && (if recovering { state.pair.story.blob.clone() }
                    else { crate::accounts::kept_progression(state.pair.story.blob.clone()) }) == new.progression
                && state.pair.character.as_ref().map(Progress::from_record) == new.progress.stored
                && state.pair.character.as_ref().and_then(|record| record.inventory.clone()) == new.progress.inventory
                && new.scene_group.iter().filter(|member| member.pair.story.account_id == new.account).eq(std::iter::once(state)) => {}
            None if new.scene_group.is_empty() => {}
            _ => return Err(JoinError::ProgressChanged),
        }
        // DEBUG (`SceneSettings::debug_new_story`, off on every real server): an account with no story yet starts with the
        // one the server was told to give new accounts. Queue that baseline before admission so every later scene
        // transaction checks the same predecessor after this FIFO write. This is queued, not acknowledged durable at
        // join; if it cannot be saved, the later exact scene precondition fails without publishing its result.
        let mut new = new;
        if !recovering && new.progression.is_empty() {
            if let Some(story) = self.scenes.debug_new_story.clone() {
                if self.saver.as_ref().is_some_and(|saver| !saver.save_story(new.account, story.clone())) {
                    return Err(JoinError::StorageUnavailable);
                }
                self.logs.warn(format!("DEBUG: account={} starts with the debug story, not the game's new one (--debug-start-moment)", new.account.0));
                new.progression = story;
            }
        }
        // Everyone joins the entry area, which is always loaded.
        let home = self.home.clone();
        let pos = self.areas.get_mut(&home).map_or_else(Pos::default, areas::AreaState::next_spawn_point);
        let entity = new.entity;
        let account = new.account;
        let recovery_group = new.scene_group;
        // Only a server that runs battles has characters to level and items to carry (see `progress`,
        // `inventory`).
        let progress = self.battle.as_ref().map(|bw| progress::resolve(bw.rules(), &new.progress));
        let inventory = self.battle.as_ref().map(|bw| Inventory::resolve(bw.rules(), new.progress.inventory.as_deref()));
        let snapshot = EntitySnapshot { id: entity, pos, facing: 0.0, anim: AnimState::Idle, avatar: new.avatar };
        self.players.insert(
            entity,
            Player {
                session: new.session,
                account,
                progression: new.progression,
                snapshot,
                budget: MoveBudget::new(now),
                dirty: false,
                tx: new.tx,
                close: new.close,
                party_seq: 0,
                invites: VecDeque::new(),
                placement: None,
                battle: None,
                progress,
                inventory,
                walked: 0.0,
                area: home,
                transit: None,
                arming: LinkArming::default(),
                fence: None,
                scene: None,
                preview: None,
                countdown_told: None,
                ways: Vec::new(),
            },
        );
        self.by_account.insert(account, entity);

        // Validate and hold a restored continuation before any entry script can mutate its
        // durable predecessor. Replay/travel messages wait until the initial area snapshot.
        if recovering {
            if let Err(error) = self.restore_continuation(entity, recovery_group, now) {
                let mut failed = Vec::new();
                self.remove_player(entity, &mut failed);
                self.drop_failed(failed);
                return Err(error);
            }
        }

        // The snapshot to the joiner (its instance's entities), `PlayerJoined` to the others of that
        // instance, then the joiner's first placement (a solo player is placed by its own phase),
        // third, where its character stands and, fourth, what it carries. An account whose story is not a
        // new account's (it advanced it before) is then told where its story stands; a new account gets
        // exactly the messages it always got.
        let mut failed = Vec::new();
        self.announce_entry(entity, now, &mut failed);
        self.tell_progress(entity, false, &mut failed);
        self.tell_inventory(entity, &mut failed);
        if self.players.get(&entity).is_some_and(|p| !p.progression.is_empty()) {
            self.tell_story(entity, None, &mut failed);
        }
        self.drop_failed(failed);
        // A story that a scene took somewhere no walk leads back from stands there again (`scene::SceneHost::entry`).
        if !recovering { self.enter_at_story(entity, now); }
        Ok(pos)
    }

    /// Remove a player and tell the others. Returns `false` if it was already gone. The player
    /// also leaves its party and its pending invites are withdrawn (see the module docs).
    pub fn leave(&mut self, entity: EntityId) -> bool {
        let mut failed = Vec::new();
        let removed = self.remove_player(entity, &mut failed).is_some();
        self.drop_failed(failed);
        removed
    }

    /// THE removal path, for a voluntary leave and for a dropped client alike: take the player out
    /// of the world and the account map, take it out of its party (announcing that to the
    /// others), then tell everyone else it left. Players whose queues fail on the way are
    /// appended to `failed` for the caller to drop.
    fn remove_player(&mut self, entity: EntityId, failed: &mut Vec<(EntityId, bool)>) -> Option<Player> {
        let player = self.players.remove(&entity)?;
        self.by_account.remove(&player.account);
        // A presentation it owned is dropped; a queued durable scene continues to its acknowledgment.
        self.scene_player_gone(entity, player.scene);
        // A player in transit is in no instance: the one it left was told already.
        let left = self.leave_instance(player.account);
        if let Some(battle) = player.battle {
            self.member_gone(battle, entity);
        }
        self.leave_party_state(&player, failed);
        if let Some(instance) = left {
            self.tell_instance(instance, Arc::new(ServerMsg::PlayerLeft { entity }), failed);
        }
        // Its countdown (a clock of the story it held) goes with it, and so does what its party was told of it.
        self.countdown_account_left(player.account, failed);
        Some(player)
    }

    /// Validate and apply one `MoveIntent` (see the module docs for the rules). Order: finite
    /// check, speed budget, walking the chord through the adapter. Only an accepted move spends
    /// budget, and facing and anim are tracked only from accepted moves.
    pub fn apply_move(&mut self, entity: EntityId, to: Pos, facing: f32, anim: AnimState, now: Instant) -> MoveOutcome {
        // The rules are the rules of the area the player stands in (areas differ in speed).
        let (from, chord, crossed) = {
            let Some(p) = self.players.get_mut(&entity) else {
                return MoveOutcome::Gone;
            };
            // A player in a battle, or between two areas, is frozen: its moves are ignored.
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() {
                return MoveOutcome::Frozen;
            }
            let Some(area) = self.areas.get(&p.area) else {
                return MoveOutcome::Gone;
            };
            let from = p.snapshot.pos;
            if !to.is_finite() || !facing.is_finite() {
                return MoveOutcome::Rejected { corrected: from, reason: Rejection::NonFinite };
            }
            // A client that has not yet taken the position a link gave the player is answered with it (no budget is
            // spent): its intents are judged from the old place.
            if let Some(corrected) = press::fenced(&mut p.fence, to, now) {
                return MoveOutcome::Relocated { corrected };
            }

            p.budget.refill(now, area.rules());
            let dist = dist_xy(from, to);
            if !p.budget.can_afford(dist, area.rules()) {
                return MoveOutcome::Rejected { corrected: from, reason: Rejection::TooFast };
            }
            let chord = walk_chord(area.runtime(), from, to, dist, area.rules());
            // Crossing an exit line leaves the area, when one of the exits there is open for the story that governs
            // the player. Judged on the move as the adapter accepted it, and only where there is somewhere to go
            // (a single-area world has no exits).
            let crossed: Vec<Exit> = match &chord {
                Chord::Clear(accepted) if self.source.is_some() => area.crossed_exits(from, *accepted).into_iter().cloned().collect(),
                _ => Vec::new(),
            };
            (from, chord, crossed)
        };
        let facing = normalize_facing(facing);
        let dist = dist_xy(from, to);

        match chord {
            Chord::Clear(accepted) => {
                let Some(p) = self.players.get_mut(&entity) else {
                    return MoveOutcome::Gone;
                };
                let from_facing = p.snapshot.facing;
                let changed = accepted != from || facing != p.snapshot.facing || anim != p.snapshot.anim;
                p.snapshot.pos = accepted;
                p.snapshot.facing = facing;
                p.snapshot.anim = anim;
                p.budget.tokens -= dist;
                p.dirty |= changed;
                // An original scripted line owns its accepted crossing before exits, story
                // points, local links or random encounters can act on the same move.
                if self.check_scene_line(entity, from, accepted, now) {
                    return MoveOutcome::Applied;
                }
                if let Some(exit) = self.open_exit(entity, crossed) {
                    // A refused transfer returns to the near side of the line, with the
                    // facing from before this move, just as it did before scene line handling.
                    if self.start_travel(entity, &exit, from, facing, now) {
                        if let Some(transit) = self.players.get_mut(&entity).and_then(|p| p.transit.as_mut()) {
                            transit.from_facing = from_facing;
                        }
                        return MoveOutcome::Applied;
                    }
                    // The target is unusable and its exit is now closed: the move is ordinary.
                }
                // Reaching a story point is what advances the story (a no-op in an area without any).
                self.check_story(entity, from, accepted, now);
                // Walking up to a line that no key fires may put the player somewhere else in the area.
                if let Some(corrected) = self.walk_links(entity, from, accepted, now) {
                    return MoveOutcome::Relocated { corrected };
                }
                // Walking is what rolls random encounters (a no-op without an encounter table).
                self.roll_encounter(entity, dist, now);
                MoveOutcome::Applied
            }
            Chord::Blocked { place } => {
                let Some(p) = self.players.get_mut(&entity) else {
                    return MoveOutcome::Gone;
                };
                // `place` is where the entity already is, unless the adapter relocated it
                // (for example back onto the walkable surface).
                if place != from {
                    p.snapshot.pos = place;
                    p.dirty = true;
                }
                MoveOutcome::Rejected { corrected: place, reason: Rejection::Blocked }
            }
        }
    }

    /// Advance the tick counter and broadcast, per instance, one `EntityUpdate` with every entity of
    /// that instance that changed since the previous tick (nothing is sent to an instance where nothing
    /// changed). Returns the new tick.
    pub fn advance_tick(&mut self) -> u64 {
        self.tick += 1;
        let mut changed: BTreeMap<fmv_party::InstanceId, Vec<EntitySnapshot>> = BTreeMap::new();
        for p in self.players.values_mut().filter(|p| p.dirty) {
            p.dirty = false;
            // A player in transit is in no instance: nobody is told about it moving.
            if p.transit.is_none() {
                if let Some(instance) = self.instances.instance_of(p.account) {
                    changed.entry(instance).or_default().push(p.snapshot.clone());
                }
            }
        }
        for (instance, entities) in changed {
            let mut failed = Vec::new();
            self.tell_instance(instance, Arc::new(ServerMsg::EntityUpdate { tick: self.tick, entities }), &mut failed);
            self.drop_failed(failed);
        }
        self.tick
    }

    /// Remove players whose queue failed (through the one removal path: party and invites
    /// included) and tell their connections to close. Removing can expose further failures, so
    /// keep going until the set is stable.
    fn drop_failed(&mut self, mut failed: Vec<(EntityId, bool)>) {
        while let Some((id, queue_full)) = failed.pop() {
            let Some(p) = self.remove_player(id, &mut failed) else { continue };
            if queue_full {
                self.logs.warn(format!("outbound queue full; disconnecting slow client entity={} session={}", id.0, p.session.0));
            } else {
                self.logs.debug(format!("outbound queue closed; removing player entity={} session={}", id.0, p.session.0));
            }
            p.close.send_replace(true);
        }
    }

    // ---- parties: the pieces shared by every party operation (the operations are in `party`) ----

    /// The party clock: milliseconds of server monotonic time.
    fn party_clock(&self, now: Instant) -> Tick {
        u64::try_from(now.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    /// Who the accounts in a party message are: display name and entity for every account that is
    /// online, the name from `gone` for one that just left (its player is no longer here).
    fn names_for(&self, accounts: &[AccountId], gone: &[(AccountId, String)]) -> Vec<PartyName> {
        accounts
            .iter()
            .filter_map(|&account| {
                if let Some(&entity) = self.by_account.get(&account) {
                    let name = self.players.get(&entity)?.snapshot.avatar.display_name.clone();
                    return Some(PartyName { account, name, entity: Some(entity) });
                }
                let (_, name) = gone.iter().find(|(a, _)| *a == account)?;
                Some(PartyName { account, name: name.clone(), entity: None })
            })
            .collect()
    }

    /// Send every event to exactly the accounts of its audience (each gets its own `seq`), then
    /// refresh the placement of everyone a membership change touched. `gone` names accounts that
    /// are no longer online. Failing queues are appended to `failed`.
    fn route_party_events(
        &mut self,
        events: Vec<PartyEvent>,
        gone: &[(AccountId, String)],
        failed: &mut Vec<(EntityId, bool)>,
    ) {
        let mut moved: BTreeSet<AccountId> = BTreeSet::new();
        for event in events {
            // A party that has a new leader, or is gone, has no way out left to follow (the `follow` module).
            match &event {
                PartyEvent::LeaderChanged { old_leader, new_leader, .. } => self.forget_ways_of(&[*old_leader, *new_leader]),
                PartyEvent::Disbanded { members, .. } => self.forget_ways_of(members),
                _ => {}
            }
            let mut audience = event.audience();
            audience.sort();
            audience.dedup();
            let names = self.names_for(&audience, gone);
            for account in &audience {
                let Some(&entity) = self.by_account.get(account) else { continue };
                let Some(player) = self.players.get_mut(&entity) else { continue };
                player.party_seq += 1;
                let msg = ServerMsg::PartyEvent { seq: player.party_seq, event: event.clone(), names: names.clone() };
                if let Err(e) = player.tx.try_send(Arc::new(msg)) {
                    failed.push((entity, is_full(&e)));
                }
            }
            // Joining, leaving, a new leader or a disbanded party can all change where (and
            // whose phase decides where) the members are placed.
            if !matches!(
                event,
                PartyEvent::InviteSent { .. }
                    | PartyEvent::InviteDeclined { .. }
                    | PartyEvent::InviteExpired { .. }
                    | PartyEvent::InviteCancelled { .. }
            ) {
                moved.extend(audience);
            }
        }
        self.refresh_placements(&moved, failed);
        // A watcher that is no longer in its owner's party stops watching.
        self.scene_check_parties();
    }

    /// A player is leaving the world: leave its party (the others hear `LeaderChanged` /
    /// `MemberLeft` / `Disbanded`) and withdraw its pending invites. `fmv-party` has no
    /// "remove this account" operation, so invites to the leaver are declined for it and invites
    /// from a leaver that was solo are declined on the invitee's behalf; either way nobody can
    /// later accept an invite from, or into, a player who is not there.
    fn leave_party_state(&mut self, gone: &Player, failed: &mut Vec<(EntityId, bool)>) {
        let account = gone.account;
        let pending: Vec<Invite> =
            self.parties.invites().filter(|i| i.from == account || i.to == account).copied().collect();
        let mut events = Vec::new();
        if self.parties.party_of(account).is_some() {
            match self.parties.leave(account) {
                Ok(e) => events.extend(e),
                Err(e) => self.logs.warn(format!("party leave failed while removing a player account={} error={e}", account.0)),
            }
        }
        for invite in pending {
            // Leaving a party already cancelled the invites the leaver sent as a member.
            if self.parties.get_invite(invite.id).is_none() {
                continue;
            }
            let acting_as = if invite.to == account { account } else { invite.to };
            match self.parties.decline(invite.id, acting_as) {
                Ok(e) => events.extend(e),
                Err(e) => self.logs.warn(format!("cannot withdraw an invite while removing a player invite={} error={e}", invite.id.0)),
            }
        }
        let name = gone.snapshot.avatar.display_name.clone();
        self.route_party_events(events, &[(account, name)], failed);
    }

    /// Expire timed-out invites (`InviteExpired` to both sides). Call it every tick.
    pub fn expire_parties(&mut self, now: Instant) {
        let clock = self.party_clock(now);
        let events = match self.parties.expire(clock) {
            Ok(events) => events,
            Err(e) => {
                self.logs.warn(format!("party expiry failed error={e}"));
                return;
            }
        };
        if events.is_empty() {
            return;
        }
        let mut failed = Vec::new();
        self.route_party_events(events, &[], &mut failed);
        self.drop_failed(failed);
    }

    /// How many players are in `account`'s party (0 when it is solo).
    #[cfg(test)]
    pub fn party_size_of(&self, account: AccountId) -> usize {
        self.parties.party_of(account).map_or(0, |p| p.size())
    }

    #[cfg(test)]
    fn snapshot_of(&self, entity: EntityId) -> Option<EntitySnapshot> {
        self.players.get(&entity).map(|p| p.snapshot.clone())
    }
}

fn is_full<T>(e: &TrySendError<T>) -> bool {
    matches!(e, TrySendError::Full(_))
}

/// How walking one chord through the adapter ended.
enum Chord {
    /// Every sub-step was accepted; the entity ends here (height snapped by the adapter).
    Clear(Pos),
    /// A sub-step was refused, so the move does not happen. The entity must be at `place`: where
    /// it was or, when the adapter relocates it on the very first step, where the adapter says.
    Blocked { place: Pos },
}

/// Walk the straight chord `from -> to` (horizontal length `dist`) through `validate_move` in
/// sub-steps of at most `rules.substep` units, each starting where the previous one was accepted.
/// The last sub-step ends exactly on `to`'s x/y. The client's z is never forwarded: the adapter
/// is always shown the entity's own height.
fn walk_chord(area: &dyn AreaRuntime, from: Pos, to: Pos, dist: f64, rules: &MoveRules) -> Chord {
    // `as usize` saturates and maps NaN to 0, so the clamp alone bounds the loop.
    let steps = ((dist / rules.substep).ceil() as usize).clamp(1, MAX_SUBSTEPS);
    let (fx, fy) = (f64::from(from.x), f64::from(from.y));
    let (dx, dy) = (f64::from(to.x) - fx, f64::from(to.y) - fy);
    let mut cur = from;
    for i in 1..=steps {
        let target = if i == steps {
            Pos::new(to.x, to.y, cur.z)
        } else {
            let t = i as f64 / steps as f64;
            Pos::new((fx + dx * t) as f32, (fy + dy * t) as f32, cur.z)
        };
        match area.validate_move(cur, target) {
            MoveResult::Accepted(next) if next.is_finite() => cur = next,
            MoveResult::Corrected(fixed) if i == 1 && fixed.is_finite() => return Chord::Blocked { place: fixed },
            // Refused further along (a hole or wall between the endpoints), or a misbehaving
            // adapter returned a non-finite position: keep the entity where it is.
            _ => return Chord::Blocked { place: from },
        }
    }
    Chord::Clear(cur)
}

/// Facing is echoed to every player and interpolated by them, so keep it a plain angle in
/// `(-pi, pi]` instead of whatever finite `f32` the client sent (an angle near `f32::MAX` would
/// turn into NaN in a peer's interpolation). Angles already in range are returned unchanged.
fn normalize_facing(facing: f32) -> f32 {
    use std::f32::consts::PI;
    if facing > -PI && facing <= PI {
        return facing;
    }
    let tau = std::f64::consts::TAU;
    let mut a = f64::from(facing).rem_euclid(tau); // [0, tau)
    if a > std::f64::consts::PI {
        a -= tau;
    }
    // Rounding to f32 can land exactly on -pi, which belongs to the other end of the interval.
    let wrapped = (a as f32).clamp(-PI, PI);
    if wrapped <= -PI {
        PI
    } else {
        wrapped
    }
}

/// Horizontal distance in f64 so even absurd finite inputs cannot overflow into NaN.
fn dist_xy(a: Pos, b: Pos) -> f64 {
    let dx = f64::from(a.x) - f64::from(b.x);
    let dy = f64::from(a.y) - f64::from(b.y);
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter};
    use fmv_core::PhaseKey;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use std::sync::Mutex;

    struct Seat {
        entity: EntityId,
        rx: mpsc::Receiver<Outbound>,
        close: watch::Receiver<bool>,
    }

    impl Seat {
        /// Everything queued so far.
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            out
        }
    }

    fn world() -> World {
        World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap()
    }

    fn seat(w: &mut World, name: &str, queue: usize, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(queue);
        let close = Arc::new(watch::channel(false).0);
        let close_rx = close.subscribe();
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: Vec::new(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now)
            .expect("a fresh account joins");
        Seat { entity, rx, close: close_rx }
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    fn walk(w: &mut World, s: &Seat, x: f32, y: f32, now: Instant) -> MoveOutcome {
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.5, AnimState::Walk, now)
    }

    /// Every call the adapter received: `(from, to)`.
    type Calls = Arc<Mutex<Vec<(Pos, Pos)>>>;

    /// Area stub without walls: accepts the first `accept_first` calls (snapping the height to
    /// 5), answers every later call with `Corrected(fixed)`, and records all calls. The only
    /// spawn point is the origin.
    struct Scripted {
        info: AreaInfo,
        speed: f32,
        accept_first: usize,
        fixed: Pos,
        calls: Calls,
    }

    impl AreaRuntime for Scripted {
        fn info(&self) -> &AreaInfo {
            &self.info
        }
        fn spawn_points(&self) -> Vec<Pos> {
            vec![Pos::default()]
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            let mut calls = self.calls.lock().unwrap();
            calls.push((from, to));
            if calls.len() <= self.accept_first {
                MoveResult::Accepted(Pos::new(to.x, to.y, 5.0))
            } else {
                MoveResult::Corrected(self.fixed)
            }
        }
        fn bounds(&self) -> Bounds {
            Bounds { min: Pos::default(), max: Pos::default() }
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            Vec::new()
        }
        fn max_speed(&self) -> f32 {
            self.speed
        }
    }

    fn scripted_world(speed: f32, accept_first: usize, fixed: Pos) -> (World, Calls) {
        let calls = Calls::default();
        let info = FixtureAdapter::new().list_areas().remove(0);
        let area = Scripted { info, speed, accept_first, fixed, calls: Arc::clone(&calls) };
        (World::new(Box::new(area)).unwrap(), calls)
    }

    /// Hop 700 ms after the previous call, so the budget is full again.
    fn hop(w: &mut World, s: &Seat, now: &mut Instant, x: f32, y: f32) -> MoveOutcome {
        *now += Duration::from_millis(700);
        walk(w, s, x, y, *now)
    }

    #[test]
    fn join_sends_snapshot_to_joiner_and_joined_to_others_with_round_robin_spawns() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        // The snapshot comes first, then where the player is placed (solo: by its own phase).
        match &a.drain()[..] {
            [ServerMsg::AreaSnapshot { entities, area, .. }, ServerMsg::InstancePlacement { placement }] => {
                assert_eq!(area.key, FIXTURE_AREA_KEY);
                assert_eq!(entities.len(), 1);
                assert_eq!(entities[0].id, a.entity);
                assert_eq!(entities[0].pos, Pos::new(-100.0, 0.0, 0.0));
                assert_eq!(placement.account, AccountId(a.entity.0));
                assert_eq!(placement.key.area.key, FIXTURE_AREA_KEY);
                assert_eq!((placement.party, placement.phase_owner, placement.story_advance_allowed), (None, AccountId(a.entity.0), true));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let mut b = seat(&mut w, "B", 16, t0);
        match &b.drain()[..] {
            [ServerMsg::AreaSnapshot { entities, .. }, ServerMsg::InstancePlacement { .. }] => {
                assert_eq!(entities.len(), 2);
                assert_eq!(w.snapshot_of(b.entity).unwrap().pos, Pos::new(100.0, 0.0, 0.0));
            }
            other => panic!("unexpected: {other:?}"),
        }
        match &a.drain()[..] {
            [ServerMsg::PlayerJoined { snapshot }] => {
                assert_eq!(snapshot.id, b.entity);
                assert_eq!(snapshot.avatar.display_name, "B");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // Spawn points wrap around (the fixture has four).
        let _c = seat(&mut w, "C", 16, t0);
        let _d = seat(&mut w, "D", 16, t0);
        let e = seat(&mut w, "E", 16, t0);
        assert_eq!(w.snapshot_of(e.entity).unwrap().pos, Pos::new(-100.0, 0.0, 0.0));
        assert_eq!(w.player_count(), 5);
    }

    #[test]
    fn accepted_move_is_broadcast_once_and_idle_ticks_send_nothing() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        a.drain();
        b.drain();

        // No change yet: ticking sends nothing.
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());

        // 100 ms after spawn the budget holds 600 * 1.5 * 0.1 = 90 units; moving 80 is fine.
        // The z value the client sent is replaced by the adapter's snapped height.
        let out = w.apply_move(a.entity, Pos::new(-20.0, 0.0, 9.0), 1.25, AnimState::Walk, ms(t0, 100));
        assert_eq!(out, MoveOutcome::Applied);
        assert_eq!(w.advance_tick(), 2);
        let mover = a.entity;
        // The update goes to every joined client, the mover included.
        for seat in [&mut a, &mut b] {
            match &seat.drain()[..] {
                [ServerMsg::EntityUpdate { tick, entities }] => {
                    assert_eq!(*tick, 2);
                    assert_eq!(entities.len(), 1);
                    assert_eq!(entities[0].id, mover);
                    assert_eq!(entities[0].pos, Pos::new(-20.0, 0.0, 0.0));
                    assert_eq!(entities[0].facing, 1.25);
                    assert_eq!(entities[0].anim, AnimState::Walk);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }

        // Re-sending identical state is accepted but is not a change: nothing is broadcast.
        assert_eq!(w.apply_move(a.entity, Pos::new(-20.0, 0.0, 0.0), 1.25, AnimState::Walk, ms(t0, 200)), MoveOutcome::Applied);
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());
    }

    #[test]
    fn speed_budget_accrues_on_server_time_and_is_capped_at_half_a_second() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);

        // The budget starts empty: right after spawning nothing has been earned yet.
        let out = walk(&mut w, &a, -50.0, 0.0, ms(t0, 0));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-100.0, 0.0, 0.0), reason: Rejection::TooFast });
        // 100 ms earn 600 * 1.5 * 0.1 = 90 units. Moving 80 spends them and leaves 10.
        assert_eq!(walk(&mut w, &a, -20.0, 0.0, ms(t0, 100)), MoveOutcome::Applied);
        // 10 ms later 10 + 9 = 19 units are available, so a 250-unit jump is refused and the last
        // accepted position is reported.
        let out = walk(&mut w, &a, 230.0, 0.0, ms(t0, 110));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-20.0, 0.0, 0.0), reason: Rejection::TooFast });
        // The refusal spent nothing: at 500 ms the budget is 19 + 900 * 0.39 = 370, so 320 fit.
        assert_eq!(walk(&mut w, &a, 300.0, 0.0, ms(t0, 500)), MoveOutcome::Applied);
        // However long the client was silent, the budget holds at most half a second of travel
        // at 1.5x max speed: 450 units.
        let far = ms(t0, 60_000);
        let out = walk(&mut w, &a, -156.0, 0.0, far); // distance 456
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(300.0, 0.0, 0.0), reason: Rejection::TooFast });
        assert_eq!(walk(&mut w, &a, -150.0, 0.0, far), MoveOutcome::Applied); // distance 450
        // ...and that spent all of it: nothing more in the same instant...
        let out = walk(&mut w, &a, -140.0, 0.0, far);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
        // ...except turning on the spot, which covers no distance and so costs nothing.
        let out = w.apply_move(a.entity, Pos::new(-150.0, 0.0, 0.0), 2.5, AnimState::Idle, far);
        assert_eq!(out, MoveOutcome::Applied);
        assert_eq!(w.snapshot_of(a.entity).unwrap().facing, 2.5);
    }

    #[test]
    fn affordability_is_nan_safe_and_turning_on_the_spot_is_free() {
        let rules = MoveRules::new(600.0);
        let mut budget = MoveBudget::new(Instant::now());
        // Float noise left the carried debt a hair past the tolerance: a move that covers no
        // distance (a facing or animation change) must still go through, anything else must not.
        budget.tokens = -rules.tolerance - 1e-12;
        assert!(budget.can_afford(0.0, &rules));
        assert!(!budget.can_afford(1e-6, &rules));
        // A NaN distance can never be paid for.
        assert!(!budget.can_afford(f64::NAN, &rules));
        // Exactly the budget plus the one-off tolerance is affordable; a hair more is not.
        budget.tokens = 10.0;
        assert!(budget.can_afford(10.0, &rules));
        assert!(budget.can_afford(10.0 + rules.tolerance, &rules));
        assert!(!budget.can_afford(10.0 + rules.tolerance * 2.0, &rules));
    }

    #[test]
    fn bunched_intents_are_judged_against_the_time_that_really_passed() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        // After a pause the budget is full (450 units). A client that kept walking at 18 units per
        // 50 ms (the headless bot's pace) through a stall of the network or of the server has all
        // those intents handled in the same instant. Each is judged against the budget it earned
        // over the stall, not against the ~1 unit that "time since the previous intent" would give.
        let at = ms(t0, 1_000);
        for i in 1..=25 {
            let x = -100.0 + 18.0 * i as f32;
            assert_eq!(walk(&mut w, &a, x, 0.0, at), MoveOutcome::Applied, "step {i}");
        }
        // 25 steps cost exactly the 450 units the budget can hold: the 26th is too much...
        let out = walk(&mut w, &a, -100.0 + 18.0 * 26.0, 0.0, at);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
        // ...and only the time that passes afterwards pays for more: 50 ms earn 45 units, which
        // covers two more 18-unit steps but not a third.
        let later = ms(t0, 1_050);
        assert_eq!(walk(&mut w, &a, 368.0, 0.0, later), MoveOutcome::Applied);
        assert_eq!(walk(&mut w, &a, 386.0, 0.0, later), MoveOutcome::Applied);
        let out = walk(&mut w, &a, 404.0, 0.0, later);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
    }

    #[test]
    fn overspeed_does_not_grow_with_the_message_rate() {
        // A client at the 120 msgs/s rate limit asks for slightly more than the budget refills
        // per message. With a per-message epsilon its speed would be 1.5 * max_speed + 120 u/s
        // (1.7x at 600 u/s, 3.5x at 60 u/s). With a spent budget it is capped at 1.5 * max_speed
        // whatever the message rate or the adapter's scale.
        for max_speed in [600.0_f32, 60.0] {
            let t0 = Instant::now();
            let (mut w, _calls) = scripted_world(max_speed, usize::MAX, Pos::default());
            let a = seat(&mut w, "A", 16, t0);
            let mut x = 0.0_f32;
            let mut elapsed = Duration::ZERO;
            for _ in 0..1200 {
                elapsed += Duration::from_nanos(8_333_333); // 120 messages per second for 10 s
                let target = x + 8.5;
                if walk(&mut w, &a, target, 0.0, t0 + elapsed) == MoveOutcome::Applied {
                    x = target;
                }
            }
            let allowed = f64::from(max_speed) * SPEED_SLACK * elapsed.as_secs_f64();
            let travelled = f64::from(x);
            assert!(travelled <= allowed + f64::from(max_speed) * SPEED_TOLERANCE, "{max_speed}: travelled {travelled} of {allowed} allowed");
            assert!(travelled >= allowed * 0.5, "{max_speed}: only {travelled} of {allowed}; the test would prove nothing");
        }
    }

    #[test]
    fn a_chord_across_a_hole_is_refused_even_when_both_ends_are_walkable() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut now = t0;
        // Legal hops next to the notch (x > 400, y > 0 is not walkable).
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, 0.0), MoveOutcome::Applied);
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, 250.0), MoveOutcome::Applied);
        w.advance_tick();
        a.drain();
        b.drain();

        // (500, -100) is walkable and only 403 units away, well inside the speed budget, but the
        // straight line there crosses the notch (for example at (420, 40)). Checking only the
        // destination would let a client cut across any hole narrower than the budget.
        let out = hop(&mut w, &a, &mut now, 500.0, -100.0);
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(300.0, 250.0, 0.0), reason: Rejection::Blocked });
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty(), "a refused chord changes and broadcasts nothing");
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(300.0, 250.0, 0.0));

        // Walking around the notch is fine.
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, -100.0), MoveOutcome::Applied);
        assert_eq!(hop(&mut w, &a, &mut now, 500.0, -100.0), MoveOutcome::Applied);
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(500.0, -100.0, 0.0));
    }

    #[test]
    fn a_chord_is_validated_in_short_steps_each_starting_where_the_last_was_accepted() {
        let t0 = Instant::now();
        let (mut w, calls) = scripted_world(600.0, usize::MAX, Pos::default());
        let a = seat(&mut w, "A", 16, t0);
        // 300 units after a full budget; the client claims a height of 99, which must never reach
        // the adapter (the adapter snaps every accepted step to a height of 5).
        let out = w.apply_move(a.entity, Pos::new(240.0, 180.0, 99.0), 0.0, AnimState::Walk, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Applied);
        let calls = calls.lock().unwrap();
        // max_speed / 60 = 10 units per step.
        assert_eq!(calls.len(), 30);
        let mut expected_from = Pos::default();
        for (i, (from, to)) in calls.iter().enumerate() {
            assert_eq!(*from, expected_from, "step {i} starts where the previous one was accepted");
            assert!(from.dist_xy(*to) <= 10.0 + 1e-3, "step {i} is {} units long", from.dist_xy(*to));
            assert_eq!(to.z, from.z, "the client's height is never forwarded");
            expected_from = Pos::new(to.x, to.y, 5.0);
        }
        assert_eq!(calls.last().unwrap().1.x, 240.0, "the last step ends exactly on the requested x");
        assert_eq!(calls.last().unwrap().1.y, 180.0, "the last step ends exactly on the requested y");
        drop(calls);
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(240.0, 180.0, 5.0));
    }

    #[test]
    fn a_refusal_later_in_the_chord_leaves_the_entity_and_its_budget_alone() {
        let t0 = Instant::now();
        // The adapter accepts three steps, then claims to want the entity at (7, 7).
        let (mut w, calls) = scripted_world(600.0, 3, Pos::new(7.0, 7.0, 0.0));
        let mut a = seat(&mut w, "A", 16, t0);
        a.drain();
        let at = ms(t0, 1_000);
        let out = walk(&mut w, &a, 100.0, 0.0, at);
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::default(), reason: Rejection::Blocked });
        assert_eq!(calls.lock().unwrap().len(), 4, "the walk stops at the first refusal");
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::default(), "the adapter's correction is ignored mid-chord");
        // The budget refilled to its 450-unit cap while the client was quiet and none was spent.
        assert_eq!(w.players[&a.entity].budget.tokens, 450.0, "a refused move costs nothing");
        w.advance_tick();
        assert!(a.drain().is_empty());
    }

    #[test]
    fn the_adapter_may_relocate_the_entity_when_it_refuses_the_first_step() {
        let t0 = Instant::now();
        // For example the entity no longer stands on the walkable surface.
        let (mut w, _calls) = scripted_world(600.0, 0, Pos::new(7.0, 7.0, 0.0));
        let mut a = seat(&mut w, "A", 16, t0);
        a.drain();
        let out = walk(&mut w, &a, 50.0, 0.0, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(7.0, 7.0, 0.0), reason: Rejection::Blocked });
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(7.0, 7.0, 0.0));
        w.advance_tick();
        assert!(matches!(&a.drain()[..], [ServerMsg::EntityUpdate { entities, .. }] if entities[0].pos == Pos::new(7.0, 7.0, 0.0)));
    }

    #[test]
    fn facing_is_normalized_into_one_turn_and_in_range_values_are_untouched() {
        use std::f32::consts::{PI, TAU};
        // In range: bit for bit (the integration tests compare 1.25 and 2.0 exactly).
        for f in [0.0_f32, -0.0, 1.25, 2.0, -3.0, PI, PI - 1e-6, -(PI - 1e-6)] {
            assert_eq!(normalize_facing(f).to_bits(), f.to_bits(), "{f}");
        }
        // The interval is (-pi, pi]: -pi is the same direction as +pi, so it moves to the other end.
        // (`f32` PI is a hair above the real pi, so its mirror image lands a hair below it.)
        let flipped = normalize_facing(-PI);
        assert!(flipped > 3.0 && flipped <= PI, "{flipped}");
        // Out of range: the same direction, inside (-pi, pi].
        for f in [4.0_f32, -4.0, 7.0, -7.0, 100.0, -100.0, 1.0e6, -1.0e6, 3.0e38, -3.0e38, f32::MAX, f32::MIN, -3.0 * PI, 3.0 * PI] {
            let n = normalize_facing(f);
            assert!(n.is_finite() && n > -PI && n <= PI, "{f} -> {n}");
            if f.abs() <= 1.0e6 {
                let turns = (f64::from(f) - f64::from(n)) / std::f64::consts::TAU;
                assert!((turns - turns.round()).abs() < 1e-5, "{f} -> {n} is a different direction");
            }
        }
        assert!((normalize_facing(7.0) - (7.0 - TAU)).abs() < 1e-5);
        assert!((normalize_facing(-7.0) - (-7.0 + TAU)).abs() < 1e-5);

        // What the other players are told is the normalized value.
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        for f in [3.0e38_f32, -3.0e38] {
            let out = w.apply_move(a.entity, Pos::new(-100.0, 0.0, 0.0), f, AnimState::Walk, ms(t0, 1_000));
            assert_eq!(out, MoveOutcome::Applied);
            let stored = w.snapshot_of(a.entity).unwrap().facing;
            assert!(stored.is_finite() && stored > -PI && stored <= PI, "{f} -> {stored}");
        }
    }

    #[test]
    fn off_mesh_move_is_corrected_without_any_broadcast() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        a.drain();
        b.drain();
        // Well within the speed budget, but y = 350 is outside the plaza (max 300).
        let out = walk(&mut w, &a, -100.0, 350.0, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-100.0, 0.0, 0.0), reason: Rejection::Blocked });
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(-100.0, 0.0, 0.0));
        assert_eq!(w.snapshot_of(a.entity).unwrap().anim, AnimState::Idle, "rejected moves do not change anim");
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let spawn = Pos::new(-100.0, 0.0, 0.0);
        let at = ms(t0, 1_000);
        for (pos, facing) in [
            (Pos::new(f32::NAN, 0.0, 0.0), 0.0),
            (Pos::new(0.0, f32::INFINITY, 0.0), 0.0),
            (Pos::new(0.0, 0.0, f32::NEG_INFINITY), 0.0),
            (Pos::new(-100.0, 0.0, 0.0), f32::NAN),
            (Pos::new(-100.0, 0.0, 0.0), f32::INFINITY),
        ] {
            assert_eq!(
                w.apply_move(a.entity, pos, facing, AnimState::Idle, at),
                MoveOutcome::Rejected { corrected: spawn, reason: Rejection::NonFinite }
            );
        }
        // Finite but absurd values must not overflow into a bogus "accepted".
        let out = walk(&mut w, &a, f32::MAX, -f32::MAX, at);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
    }

    #[test]
    fn moves_of_unknown_entities_are_reported_gone() {
        let mut w = world();
        let out = w.apply_move(EntityId(99), Pos::default(), 0.0, AnimState::Idle, Instant::now());
        assert_eq!(out, MoveOutcome::Gone);
    }

    #[test]
    fn leave_announces_once() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        b.drain();
        assert!(w.leave(a.entity));
        assert!(!w.leave(a.entity), "second leave is a no-op");
        assert!(matches!(&b.drain()[..], [ServerMsg::PlayerLeft { entity }] if *entity == a.entity));
        assert_eq!(w.player_count(), 1);
    }

    #[test]
    fn slow_client_is_dropped_without_affecting_others() {
        let t0 = Instant::now();
        let mut w = world();
        // `slow` never drains its tiny queue; `fast` drains after every step.
        let slow = seat(&mut w, "slow", 3, t0);
        let mut fast = seat(&mut w, "fast", 64, t0);
        fast.drain();
        // slow's queue holds: AreaSnapshot, PlayerJoined(fast) -> one more slot.
        // `fast` is the second joiner, so it spawns at the second spawn point, (100, 0).
        let mut x = 100.0;
        let mut now = ms(t0, 0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            now += Duration::from_millis(100);
            x += 10.0;
            assert_eq!(walk(&mut w, &fast, x, 0.0, now), MoveOutcome::Applied);
            w.advance_tick();
            seen.extend(fast.drain());
        }
        assert!(*slow.close.borrow(), "slow client must be told to close");
        assert_eq!(w.player_count(), 1, "slow client is removed from the world");
        // The fast client kept receiving, and was told the slow one left.
        assert!(seen.iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == slow.entity)));
        assert_eq!(seen.iter().filter(|m| matches!(m, ServerMsg::EntityUpdate { .. })).count(), 4);
        assert!(!*fast.close.borrow());
    }

    #[test]
    fn closed_queue_removes_the_player() {
        let t0 = Instant::now();
        let mut w = world();
        let gone = seat(&mut w, "gone", 8, t0);
        let mut stay = seat(&mut w, "stay", 8, t0);
        stay.drain();
        drop(gone.rx); // the writer task died
        let mover_out = walk(&mut w, &stay, 0.0, 0.0, ms(t0, 1_000));
        assert_eq!(mover_out, MoveOutcome::Applied);
        w.advance_tick();
        assert_eq!(w.player_count(), 1);
        assert!(*gone.close.borrow());
        assert!(stay.drain().iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == gone.entity)));
    }

    // ---------------------------------------------------------------------------------------------
    // Accounts, parties, placement
    // ---------------------------------------------------------------------------------------------

    use fmv_party::{DisbandReason, InviteId, LeaderChangeReason, LeaveReason, PartyError};
    use fmv_protocol::PartyFailure;

    /// A seat with an explicit story phase (the account is still the entity's number).
    fn seat_phase(w: &mut World, name: &str, now: Instant, phase: &[u8]) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(64);
        let close = Arc::new(watch::channel(false).0);
        let close_rx = close.subscribe();
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let new = NewPlayer {
            session,
            entity,
            account: AccountId(entity.0),
            avatar,
            progression: phase.to_vec(),
            progress: Default::default(),
            scene_state: None,
            scene_group: Vec::new(),
            tx,
            close,
        };
        w.join(new, now).expect("join");
        Seat { entity, rx, close: close_rx }
    }

    fn acct(s: &Seat) -> AccountId {
        AccountId(s.entity.0)
    }

    /// The party events in `msgs`, in order, with their per-recipient sequence numbers.
    fn party_events(msgs: &[ServerMsg]) -> Vec<(u64, PartyEvent)> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::PartyEvent { seq, event, .. } => Some((*seq, event.clone())),
                _ => None,
            })
            .collect()
    }

    fn placements(msgs: &[ServerMsg]) -> Vec<Placement> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::InstancePlacement { placement } => Some(placement.clone()),
                _ => None,
            })
            .collect()
    }

    /// Everything but party events and placements (joins, leaves, updates).
    fn world_traffic(msgs: &[ServerMsg]) -> Vec<&ServerMsg> {
        msgs.iter()
            .filter(|m| !matches!(m, ServerMsg::PartyEvent { .. } | ServerMsg::InstancePlacement { .. }))
            .collect()
    }

    /// The invite id announced by the last `InviteSent` in `msgs`.
    fn last_invite(msgs: &[ServerMsg]) -> InviteId {
        party_events(msgs)
            .into_iter()
            .rev()
            .find_map(|(_, e)| match e {
                PartyEvent::InviteSent { invite, .. } => Some(invite),
                _ => None,
            })
            .expect("an InviteSent")
    }

    /// `a` invites `b`, `b` accepts; returns nothing, but the parties are formed.
    fn form(w: &mut World, a: &mut Seat, b: &mut Seat, now: Instant) {
        assert_eq!(w.party_invite(a.entity, b.entity, now), PartyOutcome::Done);
        let invite = last_invite(&b.drain());
        assert_eq!(w.party_accept(b.entity, invite, now), PartyOutcome::Done);
        a.drain();
        b.drain();
    }

    #[test]
    fn a_second_session_for_an_account_is_refused_and_leaves_no_trace() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let (session, entity) = w.allocate_ids();
        let (tx, _rx) = mpsc::channel(16);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: "A again".into(), tint: [0; 3], appearance_ref: None };
        let dup = NewPlayer { session, entity, account: acct(&a), avatar, progression: Vec::new(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close };
        assert_eq!(w.join(dup, t0).unwrap_err(), JoinError::AlreadyOnline);
        assert_eq!(w.player_count(), 1);
        assert!(w.snapshot_of(entity).is_none());
        assert!(w.is_online(acct(&a)));
        // Once the first session ends, the account can come back.
        assert!(w.leave(a.entity));
        assert!(!w.is_online(acct(&a)));
        let (session, entity) = w.allocate_ids();
        let (tx, _rx2) = mpsc::channel(16);
        let close = Arc::new(watch::channel(false).0);
        let again = NewPlayer {
            session,
            entity,
            account: acct(&a),
            avatar: AvatarAppearance { display_name: "A".into(), tint: [0; 3], appearance_ref: None },
            progression: Vec::new(),
            progress: Default::default(),
            scene_state: None,
            scene_group: Vec::new(),
            tx,
            close,
        };
        assert!(w.join(again, t0).is_ok());
    }

    #[test]
    fn admission_rejects_a_detached_or_mixed_storage_snapshot_before_publishing() {
        for malformed in 0..5 {
            let mut w = world();
            let now = Instant::now();
            let (session, entity) = w.allocate_ids();
            let account = AccountId(entity.0);
            let state = fmv_persist::SceneState {
                pair: fmv_persist::ProgressPair {
                    story: fmv_persist::Progression::empty(account, w.home_game()), character: None,
                },
                checkpoint: None, reservation: None,
            };
            let (tx, mut rx) = mpsc::channel(32);
            let mut new = NewPlayer {
                session, entity, account,
                avatar: AvatarAppearance { display_name: "snapshot".into(), tint: [0; 3], appearance_ref: None },
                progression: Vec::new(), progress: Default::default(),
                scene_state: Some(state.clone()), scene_group: vec![state],
                tx, close: Arc::new(watch::channel(false).0),
            };
            match malformed {
                0 => new.scene_group.clear(),
                1 => new.scene_group.push(new.scene_group[0].clone()),
                2 => new.progression.push(1),
                3 => new.progress.inventory = Some(vec![]),
                _ => new.scene_state = None,
            }
            assert_eq!(w.join(new, now), Err(JoinError::ProgressChanged));
            assert_eq!(w.player_count(), 0);
            assert!(!w.has_session(account));
            assert!(rx.try_recv().is_err(), "no area state may escape before snapshot validation");
        }
    }

    #[test]
    fn a_coherent_inactive_storage_snapshot_uses_normal_admission() {
        for stored_story in [Vec::new(), vec![7; crate::accounts::MAX_PROGRESSION_BYTES + 1]] {
            let mut w = world();
            let (session, entity) = w.allocate_ids();
            let account = AccountId(entity.0);
            let state = fmv_persist::SceneState {
                pair: fmv_persist::ProgressPair {
                    story: fmv_persist::Progression::new(account, w.home_game(), stored_story.clone(), 0), character: None,
                },
                checkpoint: None, reservation: None,
            };
            let (tx, mut rx) = mpsc::channel(32);
            let new = NewPlayer {
                session, entity, account,
                avatar: AvatarAppearance { display_name: "snapshot".into(), tint: [0; 3], appearance_ref: None },
                progression: crate::accounts::kept_progression(stored_story), progress: Default::default(),
                scene_state: Some(state.clone()), scene_group: vec![state],
                tx, close: Arc::new(watch::channel(false).0),
            };
            assert!(w.join(new, Instant::now()).is_ok());
            assert!(w.has_session(account));
            assert!(matches!(&*rx.try_recv().unwrap(), ServerMsg::AreaSnapshot { .. }));
        }
    }

    #[test]
    fn an_invite_and_its_acceptance_reach_exactly_their_audience() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        for s in [&mut a, &mut b, &mut c] {
            s.drain();
        }

        assert_eq!(w.party_invite(a.entity, b.entity, ms(t0, 10)), PartyOutcome::Done);
        let (a_msgs, b_msgs, c_msgs) = (a.drain(), b.drain(), c.drain());
        assert!(c_msgs.is_empty(), "a bystander hears nothing: {c_msgs:?}");
        for msgs in [&a_msgs, &b_msgs] {
            match &msgs[..] {
                [ServerMsg::PartyEvent { seq: 1, event: PartyEvent::InviteSent { from, to, .. }, names }] => {
                    assert_eq!((*from, *to), (acct(&a), acct(&b)));
                    // The names say who these accounts are, entities included, so a client can mark them.
                    assert_eq!(names.len(), 2);
                    let n = |acc: AccountId| names.iter().find(|n| n.account == acc).unwrap();
                    assert_eq!((n(acct(&a)).name.as_str(), n(acct(&a)).entity), ("A", Some(a.entity)));
                    assert_eq!((n(acct(&b)).name.as_str(), n(acct(&b)).entity), ("B", Some(b.entity)));
                }
                other => panic!("unexpected: {other:?}"),
            }
        }

        let invite = last_invite(&b_msgs);
        assert_eq!(w.party_accept(b.entity, invite, ms(t0, 20)), PartyOutcome::Done);
        let (a_msgs, b_msgs) = (a.drain(), b.drain());
        assert!(c.drain().is_empty());
        for (msgs, seat_name) in [(&a_msgs, "A"), (&b_msgs, "B")] {
            let events = party_events(msgs);
            assert_eq!(events.len(), 1, "{seat_name}: {msgs:?}");
            assert_eq!(events[0].0, 2, "{seat_name}'s second party event");
            assert!(matches!(&events[0].1, PartyEvent::PartyFormed { leader, members, .. }
                if *leader == acct(&a) && members == &vec![acct(&a), acct(&b)]));
            // Being in a party changes the placement: the instance now follows the leader's phase.
            let p = placements(msgs);
            assert_eq!(p.len(), 1, "{seat_name}: {msgs:?}");
            assert!(p[0].party.is_some() && p[0].phase_owner == acct(&a), "{seat_name}: {:?}", p[0]);
        }
        assert_eq!(w.party_size_of(acct(&a)), 2);
        assert_eq!(w.party_size_of(acct(&c)), 0);
    }

    #[test]
    fn the_party_holds_three_and_refuses_a_fourth() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        let mut d = seat(&mut w, "D", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        assert_eq!(w.party_invite(a.entity, c.entity, ms(t0, 2)), PartyOutcome::Done);
        let invite = last_invite(&c.drain());
        assert_eq!(w.party_accept(c.entity, invite, ms(t0, 3)), PartyOutcome::Done);
        // Everyone in the party, including the first member, hears about the third.
        let id_c = acct(&c);
        for s in [&mut a, &mut b, &mut c] {
            let events = party_events(&s.drain());
            assert!(
                events.iter().any(|(_, e)| matches!(e, PartyEvent::MemberJoined { account, members, .. }
                    if *account == id_c && members.len() == 3)),
                "{events:?}"
            );
        }
        assert_eq!(w.party_size_of(acct(&a)), 3);
        // The fourth cannot even be invited.
        let out = w.party_invite(a.entity, d.entity, ms(t0, 4));
        assert!(matches!(out, PartyOutcome::Refused(PartyFailure::Rules(PartyError::PartyFull { max_size: 3, .. }))), "{out:?}");
        assert!(party_events(&d.drain()).is_empty(), "the refused invitee hears nothing about it");
    }

    #[test]
    fn refusals_go_to_the_requester_only_and_change_nothing() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        c.drain();

        // A member that is not the leader may not invite.
        let out = w.party_invite(b.entity, c.entity, ms(t0, 2));
        assert!(matches!(out, PartyOutcome::Refused(PartyFailure::Rules(PartyError::NotLeader { .. }))), "{out:?}");
        // Nobody to invite / yourself / somebody already in a party.
        assert_eq!(w.party_invite(a.entity, EntityId(999), ms(t0, 3)), PartyOutcome::Refused(PartyFailure::UnknownPlayer));
        assert!(matches!(
            w.party_invite(a.entity, a.entity, ms(t0, 3)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::SelfInvite))
        ));
        assert!(matches!(
            w.party_invite(c.entity, b.entity, ms(t0, 3)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::AlreadyInParty { .. }))
        ));
        // Accepting somebody else's invite, or one that does not exist.
        assert_eq!(w.party_invite(a.entity, c.entity, ms(t0, 4)), PartyOutcome::Done);
        let invite = last_invite(&c.drain());
        assert!(matches!(
            w.party_accept(b.entity, invite, ms(t0, 5)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::NotInvitee { .. }))
        ));
        assert!(matches!(
            w.party_accept(c.entity, InviteId(4242), ms(t0, 5)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::UnknownInvite { .. }))
        ));
        // Kicking and promoting need a party, leadership and a member.
        assert!(matches!(
            w.party_kick(c.entity, a.entity),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::NotInParty { .. }))
        ));
        assert!(matches!(
            w.party_kick(b.entity, a.entity),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::NotLeader { .. }))
        ));
        assert!(matches!(
            w.party_promote(a.entity, c.entity),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::TargetNotInParty { .. }))
        ));
        assert_eq!(w.party_kick(a.entity, EntityId(999)), PartyOutcome::Refused(PartyFailure::UnknownPlayer));
        // Only the invite sent at ms 4 produced traffic (to A and C); the refusals produced none.
        assert_eq!(party_events(&a.drain()).len(), 1);
        assert_eq!(party_events(&b.drain()).len(), 0);
        assert_eq!(w.party_size_of(acct(&a)), 2);
    }

    #[test]
    fn invites_are_rate_limited_per_player_and_the_window_slides() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 256, t0);
        let others: Vec<Seat> = (0..7).map(|i| seat(&mut w, &format!("P{i}"), 64, t0)).collect();
        for (i, other) in others.iter().take(super::party::MAX_INVITES_PER_WINDOW).enumerate() {
            assert_eq!(w.party_invite(a.entity, other.entity, ms(t0, i as u64)), PartyOutcome::Done, "invite {i}");
        }
        let sixth = &others[super::party::MAX_INVITES_PER_WINDOW];
        assert_eq!(w.party_invite(a.entity, sixth.entity, ms(t0, 100)), PartyOutcome::Refused(PartyFailure::RateLimited));
        // Another player's allowance is its own.
        assert_eq!(w.party_invite(others[0].entity, sixth.entity, ms(t0, 100)), PartyOutcome::Done);
        // Once the oldest invites fall out of the window, A may invite again.
        let later = ms(t0, 10_500);
        assert_eq!(w.party_invite(a.entity, sixth.entity, later), PartyOutcome::Done);
    }

    #[test]
    fn a_member_who_disconnects_leaves_the_party_and_the_leader_role_moves() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        assert_eq!(w.party_invite(a.entity, c.entity, ms(t0, 2)), PartyOutcome::Done);
        let invite = last_invite(&c.drain());
        assert_eq!(w.party_accept(c.entity, invite, ms(t0, 3)), PartyOutcome::Done);
        for s in [&mut a, &mut b, &mut c] {
            s.drain();
        }

        // B disconnects: the others hear MemberLeft with the roster after it, naming B even though
        // B's player is already gone (entity None).
        let (id_a, id_b, id_c) = (acct(&a), acct(&b), acct(&c));
        assert!(w.leave(b.entity));
        for s in [&mut a, &mut c] {
            let msgs = s.drain();
            let ev: Vec<_> = msgs
                .iter()
                .filter_map(|m| match m {
                    ServerMsg::PartyEvent { event, names, .. } => Some((event.clone(), names.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(ev.len(), 1, "{msgs:?}");
            assert!(matches!(&ev[0].0, PartyEvent::MemberLeft { account, reason: LeaveReason::Left, members, .. }
                if *account == id_b && members == &vec![id_a, id_c]));
            let leaver = ev[0].1.iter().find(|n| n.account == id_b).expect("the leaver is named");
            assert_eq!((leaver.name.as_str(), leaver.entity), ("B", None));
        }
        assert_eq!(w.party_size_of(acct(&a)), 2);

        // The leader disconnects: leadership passes to the longest-standing remaining member
        // BEFORE the leaver is removed (every intermediate state is valid).
        assert!(w.leave(a.entity));
        let events = party_events(&c.drain());
        assert!(matches!(&events[0].1, PartyEvent::LeaderChanged { new_leader, reason: LeaderChangeReason::LeaderLeft, .. }
            if *new_leader == acct(&c)));
        assert!(matches!(&events[1].1, PartyEvent::MemberLeft { account, .. } if *account == acct(&a)));
        // A party of one is a party; the last one out disbands it.
        assert_eq!(w.party_size_of(acct(&c)), 1);
        assert_eq!(w.party_leave(c.entity), PartyOutcome::Done);
        let events = party_events(&c.drain());
        assert!(matches!(&events[0].1, PartyEvent::Disbanded { reason: DisbandReason::LastMemberLeft, .. }));
        assert_eq!(w.party_size_of(acct(&c)), 0);
    }

    #[test]
    fn a_dropped_slow_client_leaves_its_party_through_the_same_path() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        // B's queue holds the snapshot and placement and one more message: the party formation
        // overflows it, the world drops B, and A must still be told that B left the party.
        let b = seat(&mut w, "B", 3, t0);
        a.drain();
        assert_eq!(w.party_invite(a.entity, b.entity, ms(t0, 1)), PartyOutcome::Done); // B's 3rd message
        let invite = last_invite(&[ServerMsg::PartyEvent {
            seq: 0,
            event: PartyEvent::InviteSent { invite: InviteId(1), from: acct(&a), to: AccountId(b.entity.0), expires_at: 0 },
            names: vec![],
        }]);
        assert_eq!(w.party_accept(b.entity, invite, ms(t0, 2)), PartyOutcome::Done);
        assert!(*b.close.borrow(), "the slow client is told to close");
        assert_eq!(w.player_count(), 1);
        assert!(!w.is_online(AccountId(b.entity.0)));
        let msgs = a.drain();
        let events = party_events(&msgs);
        assert!(events.iter().any(|(_, e)| matches!(e, PartyEvent::PartyFormed { .. })), "{events:?}");
        assert!(
            events.iter().any(|(_, e)| matches!(e, PartyEvent::MemberLeft { account, .. } if *account == AccountId(b.entity.0))),
            "A must hear that B left: {events:?}"
        );
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == b.entity)));
        assert_eq!(w.party_size_of(acct(&a)), 1, "no ghost member");
    }

    #[test]
    fn pending_invites_are_withdrawn_when_either_side_leaves() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        for s in [&mut a, &mut b, &mut c] {
            s.drain();
        }
        // A (solo) invites B and then disconnects: B must not be able to accept an invite from a
        // player who is not there (that would form a party with a ghost).
        assert_eq!(w.party_invite(a.entity, b.entity, ms(t0, 1)), PartyOutcome::Done);
        let invite = last_invite(&b.drain());
        assert!(w.leave(a.entity));
        let events = party_events(&b.drain());
        assert!(
            events.iter().any(|(_, e)| matches!(e, PartyEvent::InviteDeclined { invite: i, .. } if *i == invite)),
            "B hears the invite is over: {events:?}"
        );
        assert!(matches!(
            w.party_accept(b.entity, invite, ms(t0, 2)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::UnknownInvite { .. }))
        ));
        assert_eq!(w.party_size_of(acct(&b)), 0);

        // The invitee disconnecting withdraws the invite from the inviter's side as well.
        assert_eq!(w.party_invite(b.entity, c.entity, ms(t0, 3)), PartyOutcome::Done);
        let invite = last_invite(&c.drain());
        b.drain();
        assert!(w.leave(c.entity));
        let events = party_events(&b.drain());
        assert!(events.iter().any(|(_, e)| matches!(e, PartyEvent::InviteDeclined { invite: i, .. } if *i == invite)), "{events:?}");
    }

    #[test]
    fn invites_expire_on_the_server_clock() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        a.drain();
        b.drain();
        assert_eq!(w.party_invite(a.entity, b.entity, ms(t0, 1_000)), PartyOutcome::Done);
        let invite = last_invite(&b.drain());
        a.drain();

        w.expire_parties(ms(t0, 30_000));
        assert!(party_events(&a.drain()).is_empty(), "still alive after 30 s");
        w.expire_parties(ms(t0, 62_000));
        for s in [&mut a, &mut b] {
            let events = party_events(&s.drain());
            assert!(matches!(&events[..], [(_, PartyEvent::InviteExpired { invite: i, .. })] if *i == invite), "{events:?}");
        }
        assert!(matches!(
            w.party_accept(b.entity, invite, ms(t0, 63_000)),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::UnknownInvite { .. }))
        ));
        // Expiring again announces nothing.
        w.expire_parties(ms(t0, 70_000));
        assert!(party_events(&a.drain()).is_empty());
    }

    #[test]
    fn kick_promote_and_leave_tell_everyone_concerned() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        assert_eq!(w.party_invite(a.entity, c.entity, ms(t0, 2)), PartyOutcome::Done);
        let invite = last_invite(&c.drain());
        assert_eq!(w.party_accept(c.entity, invite, ms(t0, 3)), PartyOutcome::Done);
        for s in [&mut a, &mut b, &mut c] {
            s.drain();
        }

        // Promote B: everyone is told, and the placement's phase owner follows the leader.
        let id_b = acct(&b);
        assert_eq!(w.party_promote(a.entity, b.entity), PartyOutcome::Done);
        for s in [&mut a, &mut b, &mut c] {
            let msgs = s.drain();
            let events = party_events(&msgs);
            assert!(matches!(&events[..], [(_, PartyEvent::LeaderChanged { new_leader, reason: LeaderChangeReason::Promoted, .. })]
                if *new_leader == id_b), "{events:?}");
            assert!(placements(&msgs).iter().all(|p| p.phase_owner == id_b));
        }
        // The old leader can no longer kick; the new one can. The kicked player hears it too and
        // is placed on its own again.
        assert!(matches!(
            w.party_kick(a.entity, c.entity),
            PartyOutcome::Refused(PartyFailure::Rules(PartyError::NotLeader { .. }))
        ));
        assert_eq!(w.party_kick(b.entity, c.entity), PartyOutcome::Done);
        let msgs = c.drain();
        let events = party_events(&msgs);
        assert!(matches!(&events[..], [(_, PartyEvent::MemberLeft { account, reason: LeaveReason::Kicked, members, .. })]
            if *account == acct(&c) && members == &vec![acct(&a), acct(&b)]), "{events:?}");
        assert!(matches!(&placements(&msgs)[..], [p] if p.party.is_none() && p.phase_owner == acct(&c)));
        assert_eq!(w.party_size_of(acct(&c)), 0);
        assert_eq!(w.party_size_of(acct(&a)), 2);
        // A member leaves on its own.
        a.drain();
        b.drain();
        assert_eq!(w.party_leave(a.entity), PartyOutcome::Done);
        let events = party_events(&b.drain());
        assert!(matches!(&events[..], [(_, PartyEvent::MemberLeft { account, reason: LeaveReason::Left, .. })] if *account == acct(&a)), "{events:?}");
    }

    #[test]
    fn sync_answers_with_the_whole_situation_and_the_sequence_to_continue_from() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut c = seat(&mut w, "C", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        assert_eq!(w.party_invite(a.entity, c.entity, ms(t0, 2)), PartyOutcome::Done);
        a.drain();
        c.drain();

        assert_eq!(w.party_sync(a.entity, ms(t0, 3)), PartyOutcome::Done);
        match &a.drain()[..] {
            [ServerMsg::PartyState { seq, party: Some(party), invites, names }] => {
                // A saw: InviteSent (1), PartyFormed (2), InviteSent (3).
                assert_eq!(*seq, 3);
                assert_eq!((party.leader, &party.members), (acct(&a), &vec![acct(&a), acct(&b)]));
                assert_eq!(invites.len(), 1);
                assert_eq!((invites[0].from, invites[0].to), (acct(&a), acct(&c)));
                let who: Vec<&str> = names.iter().map(|n| n.name.as_str()).collect();
                assert_eq!(who, ["A", "B", "C"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // The invitee sees the same invite and no party.
        assert_eq!(w.party_sync(c.entity, ms(t0, 3)), PartyOutcome::Done);
        match &c.drain()[..] {
            [ServerMsg::PartyState { seq: 1, party: None, invites, .. }] => assert_eq!(invites.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }
        // A timed-out invite that was not reaped yet is not reported.
        assert_eq!(w.party_sync(c.entity, ms(t0, 120_000)), PartyOutcome::Done);
        assert!(matches!(&c.drain()[..], [ServerMsg::PartyState { invites, .. }] if invites.is_empty()));
        // Sequence numbers continue from the state: the next event is seq 4 for A.
        assert_eq!(w.party_kick(a.entity, b.entity), PartyOutcome::Done);
        assert_eq!(party_events(&a.drain())[0].0, 4);
    }

    #[test]
    fn story_phases_decide_the_instance_and_whether_the_story_may_advance() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat_phase(&mut w, "A", t0, &[1]);
        let mut b = seat_phase(&mut w, "B", t0, &[2]);
        // Solo, everyone is placed by their own phase.
        let a_join = placements(&a.drain());
        let b_join = placements(&b.drain());
        assert_eq!(a_join[0].key.phase, PhaseKey(vec![1]));
        assert_eq!(b_join[0].key.phase, PhaseKey(vec![2]));
        assert!(a_join[0].story_advance_allowed && b_join[0].story_advance_allowed);

        // In A's party both are placed by the LEADER's phase; B keeps its own progression but
        // may not advance A's story.
        assert_eq!(w.party_invite(a.entity, b.entity, ms(t0, 1)), PartyOutcome::Done);
        let invite = last_invite(&b.drain());
        assert_eq!(w.party_accept(b.entity, invite, ms(t0, 2)), PartyOutcome::Done);
        let b_party = placements(&b.drain());
        assert_eq!(b_party.len(), 1);
        assert_eq!(b_party[0].key.phase, PhaseKey(vec![1]));
        assert_eq!(b_party[0].phase_owner, acct(&a));
        assert!(!b_party[0].story_advance_allowed);
        // A's own placement did not change (same key, same flag): it is not told again.
        assert!(placements(&a.drain()).iter().all(|p| p.party.is_some()));

        // Leaving puts B back on its own phase.
        assert_eq!(w.party_leave(b.entity), PartyOutcome::Done);
        let back = placements(&b.drain());
        assert_eq!(back.last().unwrap().key.phase, PhaseKey(vec![2]));
        assert!(back.last().unwrap().party.is_none() && back.last().unwrap().story_advance_allowed);
    }

    #[test]
    fn requests_from_players_who_are_gone_report_gone() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let ghost = EntityId(777);
        assert_eq!(w.party_invite(ghost, a.entity, t0), PartyOutcome::Gone);
        assert_eq!(w.party_accept(ghost, InviteId(1), t0), PartyOutcome::Gone);
        assert_eq!(w.party_decline(ghost, InviteId(1)), PartyOutcome::Gone);
        assert_eq!(w.party_leave(ghost), PartyOutcome::Gone);
        assert_eq!(w.party_kick(ghost, a.entity), PartyOutcome::Gone);
        assert_eq!(w.party_promote(ghost, a.entity), PartyOutcome::Gone);
        assert_eq!(w.party_sync(ghost, t0), PartyOutcome::Gone);
    }

    #[test]
    fn party_traffic_does_not_disturb_world_traffic() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        form(&mut w, &mut a, &mut b, ms(t0, 1));
        // Walking still broadcasts exactly one update per tick, and parties add nothing to it.
        assert_eq!(walk(&mut w, &a, -80.0, 0.0, ms(t0, 200)), MoveOutcome::Applied);
        w.advance_tick();
        for s in [&mut a, &mut b] {
            let msgs = s.drain();
            assert_eq!(world_traffic(&msgs).len(), 1, "{msgs:?}");
            assert!(party_events(&msgs).is_empty());
        }
    }

    #[test]
    fn world_rejects_unusable_areas() {
        use fmv_adapter_api::{AreaInfo, Bounds};

        struct Bare {
            info: AreaInfo,
            spawns: Vec<Pos>,
            speed: f32,
        }
        impl AreaRuntime for Bare {
            fn info(&self) -> &AreaInfo {
                &self.info
            }
            fn spawn_points(&self) -> Vec<Pos> {
                self.spawns.clone()
            }
            fn validate_move(&self, _from: Pos, to: Pos) -> MoveResult {
                MoveResult::Accepted(to)
            }
            fn bounds(&self) -> Bounds {
                Bounds { min: Pos::default(), max: Pos::default() }
            }
            fn walk_triangles(&self) -> Vec<[Pos; 3]> {
                Vec::new()
            }
            fn max_speed(&self) -> f32 {
                self.speed
            }
        }
        let info = FixtureAdapter::new().list_areas().remove(0);
        let mk = |spawns: Vec<Pos>, speed: f32| -> Box<dyn AreaRuntime> { Box::new(Bare { info: info.clone(), spawns, speed }) };
        assert!(World::new(mk(vec![Pos::default()], 10.0)).is_ok());
        assert!(World::new(mk(vec![], 10.0)).is_err());
        assert!(World::new(mk(vec![Pos::new(f32::NAN, 0.0, 0.0)], 10.0)).is_err());
        assert!(World::new(mk(vec![Pos::default()], 0.0)).is_err());
        assert!(World::new(mk(vec![Pos::default()], f32::NAN)).is_err());
    }
}
