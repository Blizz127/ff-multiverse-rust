//! Ordinary exact-story fights. A coherent queued read and the clients' ordered hold acknowledgments
//! precede combat. Accepted item writes retain the ordinary FIFO semantics; the terminal snapshot
//! follows those writes, then one acknowledged batch pays every fighter and advances the owner.
//! No in-flight checkpoint is persisted: a crash discards uncommitted combat/rewards, while already
//! accepted item debits remain saved. Account reservations prevent a replacement session racing it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::StoryStep;
use fmv_core::{AccountId, AnimState, EntityId};
use fmv_ff7_battle::rules::{ActorId, Outcome};
use fmv_persist::{ProgressPair, SceneBatchCommitted, SceneState};
use fmv_protocol::{BattleId, BattleMember, BattleReward, BattleSetup, ServerMsg};
use tokio::sync::oneshot;

use super::{Battle, Member, World};
use crate::inventory::Inventory;
use crate::progress::Progress;
use crate::saver::{SceneBattleSettlement, SceneSaveError, StoryBattleJob, StoryBattleMember};

type Snapshot = oneshot::Receiver<Result<Vec<SceneState>, SceneSaveError>>;
type Committed = oneshot::Receiver<Result<SceneBatchCommitted, SceneSaveError>>;

#[derive(Clone)]
struct Mirror {
    account: AccountId,
    story: Vec<u8>,
    progress: Progress,
    inventory: Inventory,
}

#[derive(Clone)]
struct Terminal {
    outcome: Outcome,
    rewards: Vec<BattleReward>,
    settlements: Vec<(AccountId, SceneBattleSettlement)>,
}

enum Phase {
    Starting(Snapshot),
    Ready,
    Refused,
    Fighting,
    Ending { terminal: Terminal, done: Snapshot },
    Abandoning(Snapshot),
    Committing { terminal: Terminal, job: StoryBattleJob, done: Committed },
    Busy,
}

pub(super) struct OrdinaryBattle {
    instance: [u8; 32],
    owner: AccountId,
    before: Vec<u8>,
    step: StoryStep,
    point: u32,
    area: String,
    setup: BattleSetup,
    mirrors: Vec<Mirror>,
    /// The coherent starting pairs, updated only by each accepted ordinary item debit. The terminal
    /// snapshot must match these logical values; storage timestamps alone may change in the FIFO.
    expected: Vec<ProgressPair>,
    holds: BTreeMap<AccountId, u64>,
    acknowledged: BTreeSet<AccountId>,
    held_at: Instant,
    phase: Phase,
}

/// A fully settled authored defeat. This is a session hold, not a new durable checkpoint;
/// leaving returns to the title, and a later login uses the already saved story and resources.
pub(super) struct GameOverHold {
    pub(super) battle: BattleId,
    pub(super) entity: EntityId,
    area: String,
}

impl OrdinaryBattle {
    pub(super) fn contains(&self, account: AccountId) -> bool { self.holds.contains_key(&account) }
    fn accounts(&self) -> Vec<AccountId> { self.setup.party.iter().filter_map(BattleMember::account).collect() }
}

fn equal_pair_values(a: &ProgressPair, b: &ProgressPair) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    a.story.updated_at = 0;
    b.story.updated_at = 0;
    if let Some(character) = &mut a.character { character.updated_at = 0; }
    if let Some(character) = &mut b.character { character.updated_at = 0; }
    a == b
}

impl World {
    pub(crate) fn ordinary_battle_account_held(&self, account: AccountId) -> bool {
        self.battle.as_ref().is_some_and(|bw| bw.game_over.contains_key(&account) || bw.ordinary.values().any(|battle| battle.contains(account)))
    }

    pub(in crate::world) fn ordinary_battle_destinations(&self) -> Vec<String> {
        self.battle.as_ref().map_or_else(Vec::new, |bw| bw.ordinary.values().map(|battle| battle.area.clone()).chain(bw.game_over.values().map(|hold| hold.area.clone())).collect())
    }

    /// `true` means the token belongs to this ordinary battle, even when it is an already accepted
    /// duplicate. Other/stale tokens remain available to the ordinary field-story hold dispatcher.
    pub(in crate::world) fn ordinary_battle_pending_ack(&mut self, entity: EntityId, token: u64, now: Instant) -> bool {
        let Some(player) = self.players.get(&entity) else { return false; };
        let (account, id) = (player.account, player.battle);
        let Some((id, battle)) = self.battle.as_mut().and_then(|bw| id.and_then(|id| bw.ordinary.get_mut(&id).map(|b| (id, b)))) else {
            return false;
        };
        if battle.holds.get(&account) != Some(&token) { return false; }
        if player.story_pending == Some(token) && player.battle == Some(id) { battle.acknowledged.insert(account); }
        self.advance_ordinary_battles(now);
        true
    }

    pub(super) fn begin_ordinary_story_battle(&mut self, entity: EntityId, point: u32, formation: u16, now: Instant) -> bool {
        let Some(player) = self.players.get(&entity) else { return false; };
        if player.preview.is_some() || player.story_pending.is_some_and(|id| !self.ordinary_story_barrier_acked(entity, id)) { return false; }
        let owner = player.account;
        let area = player.area.clone();
        let before = player.progression.clone();
        let Some(runtime) = self.areas.get(&player.area).map(|area| area.runtime()) else { return false; };
        // One authored evaluation binds policy, the BATTLE-time roster and the eventual
        // victory together. Its prefix is only an engine input: `before` remains the
        // terminal transaction's exact predecessor and is never replaced by that prefix.
        let Some(action) = runtime.story_battle_action(&before, point).filter(|action| action.policy.formation == formation) else { return false; };
        let Ok(canon) = action.battle_progress.as_deref().map(|progress| runtime.try_story_party(progress)).transpose() else { return false; };
        let (policy, step) = (action.policy, action.victory);
        let Ok(mut setup) = self.prepare_battle_setup_with_canon(entity, formation, None, true, canon.as_deref()) else { return false; };
        setup.escape_policy = if policy.escape_forbidden { fmv_protocol::EscapePolicy::Forbidden } else { fmv_protocol::EscapePolicy::Formation };
        setup.victory_fanfare = policy.victory_fanfare;
        setup.game_over_on_defeat = policy.game_over_on_defeat;
        let Some(bw) = self.battle.as_ref() else { return false; };
        if bw.rules().build(&setup).is_err() { return false; }
        let accounts = setup.party.iter().filter_map(BattleMember::account).collect::<Vec<_>>();
        if accounts.is_empty() || !accounts.contains(&owner) || accounts.iter().copied().collect::<BTreeSet<_>>().len() != accounts.len() { return false; }
        let mut mirrors = Vec::new();
        let mut reused = BTreeMap::new();
        for account in &accounts {
            let Some(p) = self.by_account.get(account).and_then(|entity| self.players.get(entity)) else { return false; };
            if let Some(token) = p.story_pending {
                let Some(entity) = self.by_account.get(account).copied() else { return false; };
                if !self.ordinary_story_barrier_acked(entity, token) { return false; }
                reused.insert(*account, token);
            }
            let (Some(progress), Some(inventory)) = (p.progress, p.inventory.clone()) else { return false; };
            mirrors.push(Mirror { account: *account, story: p.progression.clone(), progress, inventory });
        }
        let mut holds = BTreeMap::new();
        for account in &accounts {
            let Some(token) = reused.get(account).copied().or_else(|| self.new_story_pending_id()) else { return false; };
            holds.insert(*account, token);
        }
        let mut instance = [0; 32];
        if getrandom::fill(&mut instance).is_err() || instance == [0; 32] { return false; }
        let Some(bw) = self.battle.as_mut() else { return false; };
        let id = BattleId(bw.next_id);
        let Some(next) = bw.next_id.checked_add(1) else { return false; };
        let done = match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.snapshot_story_battle(accounts)) {
            Ok(done) => done,
            Err(_) => { self.ordinary_battle_failure("the initial battle snapshot could not be queued"); return false; }
        };
        bw.next_id = next;
        bw.ordinary.insert(id, OrdinaryBattle { instance, owner, before, step, point, area, setup,
            mirrors, expected: Vec::new(), holds: holds.clone(), acknowledged: reused.keys().copied().collect(), held_at: now, phase: Phase::Starting(done) });
        self.bump_story_save_epoch();
        let mut messages = Vec::new();
        for (account, token) in holds {
            let Some(entity) = self.by_account.get(&account).copied() else { continue; };
            let Some(p) = self.players.get_mut(&entity) else { continue; };
            p.battle = Some(id);
            p.story_pending = Some(token);
            p.snapshot.anim = AnimState::Idle;
            p.dirty = true;
            let Some(area) = self.areas.get(&p.area) else { continue; };
            if !reused.contains_key(&account) {
                messages.push((entity, ServerMsg::StoryPending { id: token, area: area.id().clone(), pos: p.snapshot.pos, facing: p.snapshot.facing }));
            }
        }
        for (entity, message) in messages { self.fan_to(&[entity], Arc::new(message)); }
        true
    }

    fn ordinary_battle_failure(&mut self, reason: &str) {
        self.logs.warn(format!("ordinary story battle requires storage recovery: {reason}"));
        self.stop_for_scene_storage_recovery();
    }

    /// Poll only outside engine borrows. This also handles an owner who disconnected: the captured
    /// account and pair remain authoritative, and acknowledgments cannot turn into ordinary deltas.
    pub(crate) fn advance_ordinary_battles(&mut self, now: Instant) {
        enum Ready {
            Initial(Result<Vec<SceneState>, SceneSaveError>),
            Launch,
            Refused,
            Abandoned(Result<Vec<SceneState>, SceneSaveError>),
            Terminal(Terminal, Result<Vec<SceneState>, SceneSaveError>),
            Committed(Terminal, StoryBattleJob, Result<SceneBatchCommitted, SceneSaveError>),
        }
        let ids = self.battle.as_ref().map_or_else(Vec::new, |bw| bw.ordinary.keys().copied().collect::<Vec<_>>());
        for id in ids {
            if self.scenes.storage_failed { return; }
            let ready = {
                let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.get_mut(&id)) else { continue; };
                let ready = match &mut record.phase {
                    Phase::Starting(done) => match done.try_recv() {
                        Ok(result) => Some(Ready::Initial(result)),
                        Err(oneshot::error::TryRecvError::Empty) => None,
                        Err(_) => Some(Ready::Initial(Err(SceneSaveError::WorkerFailed))),
                    },
                    Phase::Ready => Some(Ready::Launch),
                    Phase::Refused => Some(Ready::Refused),
                    Phase::Abandoning(done) => match done.try_recv() {
                        Ok(result) => Some(Ready::Abandoned(result)),
                        Err(oneshot::error::TryRecvError::Empty) => None,
                        Err(_) => Some(Ready::Abandoned(Err(SceneSaveError::WorkerFailed))),
                    },
                    Phase::Ending { terminal, done } => match done.try_recv() {
                        Ok(result) => Some(Ready::Terminal(terminal.clone(), result)),
                        Err(oneshot::error::TryRecvError::Empty) => None,
                        Err(_) => Some(Ready::Terminal(terminal.clone(), Err(SceneSaveError::WorkerFailed))),
                    },
                    Phase::Committing { terminal, job, done } => match done.try_recv() {
                        Ok(result) => Some(Ready::Committed(terminal.clone(), job.clone(), result)),
                        Err(oneshot::error::TryRecvError::Empty) => None,
                        Err(_) => Some(Ready::Committed(terminal.clone(), job.clone(), Err(SceneSaveError::WorkerFailed))),
                    },
                    Phase::Fighting | Phase::Busy => None,
                };
                if ready.is_some() && !matches!(record.phase, Phase::Ready | Phase::Refused) { record.phase = Phase::Busy; }
                ready
            };
            match ready {
                None => {},
                Some(Ready::Initial(Ok(states))) => {
                    if !self.ordinary_initial_matches(id, &states) { self.ordinary_battle_failure("initial pairs differ from the frozen roster"); continue; }
                    if let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.get_mut(&id)) {
                        record.expected = states.into_iter().map(|s| s.pair).collect();
                        record.phase = Phase::Ready;
                    }
                    self.launch_ordinary_battle(id, now);
                }
                Some(Ready::Launch) => self.launch_ordinary_battle(id, now),
                Some(Ready::Refused) => self.release_unstarted_ordinary_battle(id, now),
                Some(Ready::Abandoned(Ok(states))) => {
                    if !self.ordinary_terminal_matches(id, &states) {
                        self.ordinary_battle_failure("an abandoned fight did not retain exactly its accepted item debits");
                        continue;
                    }
                    if let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.remove(&id)) {
                        self.discard_ordinary_story_battle_followups(record.owner);
                    }
                    self.bump_story_save_epoch();
                }
                Some(Ready::Terminal(terminal, Ok(states))) => self.commit_ordinary_battle(id, terminal, states),
                Some(Ready::Committed(terminal, job, Ok(saved))) => self.publish_ordinary_battle(id, terminal, job, saved, now),
                Some(Ready::Initial(Err(SceneSaveError::Store(fmv_persist::PersistError::TransactionConflict(_))))) => {
                    if let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.get_mut(&id)) {
                        record.phase = Phase::Refused;
                    }
                    self.release_unstarted_ordinary_battle(id, now);
                }
                Some(Ready::Initial(Err(_)) | Ready::Abandoned(Err(_)) | Ready::Terminal(_, Err(_)) | Ready::Committed(_, _, Err(_))) =>
                    self.ordinary_battle_failure("the battle's ordered storage operation was not acknowledged"),
            }
        }
    }

    /// A known preflight refusal changed nothing and launched no engine. Release the ordered hold
    /// so the player can leave the blocked story point; uncertain writes use the fatal path instead.
    fn release_unstarted_ordinary_battle(&mut self, id: BattleId, now: Instant) {
        let Some(record) = self.battle.as_ref().and_then(|bw| bw.ordinary.get(&id)) else { return; };
        let unacknowledged = record.holds.keys().filter(|account| !record.acknowledged.contains(account))
            .filter_map(|account| self.by_account.get(account).copied()).collect::<Vec<_>>();
        if !unacknowledged.is_empty() {
            if now.saturating_duration_since(record.held_at) >= Duration::from_secs(30) {
                self.drop_failed(unacknowledged.into_iter().map(|entity| (entity, false)).collect());
            }
            return;
        }
        let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.remove(&id)) else { return; };
        self.bump_story_save_epoch();
        self.logs.warn("ordinary story battle refused before combat: its durable receipt capacity is unavailable".into());
        self.discard_ordinary_story_battle_followups(record.owner);
        let mut failed = Vec::new();
        self.refresh_placements(&record.holds.keys().copied().collect(), &mut failed);
        self.drop_failed(failed);
        for (account, token) in record.holds {
            let Some(entity) = self.by_account.get(&account).copied() else { continue; };
            let Some(p) = self.players.get_mut(&entity).filter(|p| p.battle == Some(id) && p.story_pending == Some(token)) else { continue; };
            p.battle = None;
            p.story_pending = None;
            p.dirty = true;
            self.fan_to(&[entity], Arc::new(ServerMsg::StoryCommitted { id: token }));
        }
    }

    fn ordinary_initial_matches(&self, id: BattleId, states: &[SceneState]) -> bool {
        let Some(bw) = self.battle.as_ref() else { return false; };
        let Some(record) = bw.ordinary.get(&id) else { return false; };
        if states.len() != record.mirrors.len() { return false; }
        states.iter().zip(&record.mirrors).all(|(state, mirror)| {
            let Some(character) = state.pair.character.as_ref() else { return false; };
            state.checkpoint.is_none() && state.reservation.is_none()
                && state.pair.story.account_id == mirror.account && character.account_id == mirror.account
                && character.game == state.pair.story.game && state.pair.story.blob == mirror.story
                && crate::progress::resolve(bw.rules(), &crate::progress::ProgressSeed::of(
                    mirror.progress.character, Some(Progress::from_record(character)))) == mirror.progress
                && Inventory::resolve(bw.rules(), character.inventory.as_deref()) == mirror.inventory
        })
    }

    fn launch_ordinary_battle(&mut self, id: BattleId, now: Instant) {
        let Some(bw) = self.battle.as_mut() else { return; };
        let Some(record) = bw.ordinary.get_mut(&id) else { return; };
        if !matches!(record.phase, Phase::Ready) { return; }
        let mut connected = Vec::new();
        let mut unacknowledged = Vec::new();
        for member in &record.setup.party {
            let (Some(account), Some(entity)) = (member.account(), member.entity()) else { continue; };
            let Some(p) = self.players.get(&entity) else { continue; };
            if p.account != account || p.battle != Some(id) || p.story_pending != record.holds.get(&account).copied() {
                self.ordinary_battle_failure("a frozen battle participant changed before launch"); return;
            }
            connected.push(entity);
            if !record.acknowledged.contains(&account) { unacknowledged.push(entity); }
        }
        if !unacknowledged.is_empty() {
            // One connected peer cannot retain the whole co-op roster indefinitely. The ordered
            // snapshot already completed; after dropping silent peers, the next tick can launch
            // the remaining acknowledged players with the same frozen combatants.
            if now.saturating_duration_since(record.held_at) >= Duration::from_secs(30) {
                self.drop_failed(unacknowledged.into_iter().map(|entity| (entity, false)).collect());
            }
            return;
        }
        if connected.is_empty() {
            let owner = record.owner;
            bw.ordinary.remove(&id);
            self.discard_ordinary_story_battle_followups(owner);
            self.bump_story_save_epoch();
            return;
        }
        let setup = record.setup.clone();
        let state = match bw.settings.rules.build(&setup) {
            Ok(state) => state,
            Err(_) => { self.ordinary_battle_failure("the frozen battle setup no longer builds"); return; }
        };
        record.phase = Phase::Fighting;
        let count = setup.party.len();
        let start = ServerMsg::EncounterStart { battle_id: id, formation_id: setup.formation_id, seed: setup.seed,
            party: setup.party.clone(), preemptive: setup.preemptive, back_attack: setup.back_attack,
            battle_speed: setup.battle_speed, ticks_per_second: setup.ticks_per_second, escape_policy: setup.escape_policy,
            victory_fanfare: setup.victory_fanfare, game_over_on_defeat: setup.game_over_on_defeat, initial_party: setup.initial_party.clone(), data_fingerprint: bw.settings.rules.data_fingerprint() };
        bw.battles.insert(id, Battle { state,
            members: setup.party.iter().cloned().map(|info| Member { connected: info.entity().is_some_and(|e| connected.contains(&e)), info }).collect(),
            commands: vec![0; count], ready_since: vec![None; count], last_action: None, clock: now,
            formation: setup.formation_id, seed: setup.seed, status_counts: [0; 3], then: None, counters: 0, continuation: None });
        self.logs.info(format!("ordinary exact story battle started battle={} formation={} members={}", id.0, setup.formation_id, connected.len()));
        self.fan_to(&connected, Arc::new(start));
    }

    /// Forecast exactly the inventory-only FIFO debit. The worker snapshot will prove that every
    /// accepted item write reached storage; the terminal settlement never charges it a second time.
    pub(super) fn note_ordinary_item(&mut self, id: BattleId, account: AccountId, item: u16) {
        let Some(bw) = self.battle.as_mut() else { return; };
        let Some(record) = bw.ordinary.get_mut(&id) else { return; };
        let Some(character) = record.expected.iter_mut().find(|p| p.story.account_id == account).and_then(|p| p.character.as_mut()) else {
            self.ordinary_battle_failure("an item payer was absent from the frozen battle records"); return;
        };
        let mut inventory = Inventory::resolve(bw.settings.rules.as_ref(), character.inventory.as_deref());
        if !inventory.take_one(item) { self.ordinary_battle_failure("an accepted item was absent from the frozen inventory"); return; }
        character.inventory = Some(inventory.entries().to_vec());
    }

    pub(super) fn queue_ordinary_battle_outcome(&mut self, id: BattleId, battle: Battle, outcome: Outcome, rewards: Vec<BattleReward>) {
        let Some(bw) = self.battle.as_mut() else { return; };
        let Some(record) = bw.ordinary.get(&id) else { return; };
        if !matches!(record.phase, Phase::Fighting) { self.ordinary_battle_failure("the engine ended outside its frozen attempt"); return; }
        let accounts = record.accounts();
        let mut settlements = Vec::new();
        for (i, member) in battle.members.iter().enumerate() {
            let Some(account) = member.info.account() else { continue; };
            let mut reward = rewards.iter().find(|r| r.account == account).cloned().unwrap_or_else(|| BattleReward::new(account, 0, 0, 0));
            if !reward.is_nothing() { reward.exp = reward.exp.saturating_add(bw.settings.debug_bonus_exp); }
            let actor = ActorId(i as u8);
            let final_limit = if bw.settings.debug_limit_full || battle.state.limit_for(actor).is_none() { None } else {
                let end = battle.state.limit_gauge(actor).saturating_add(u16::from(bw.settings.debug_bonus_limit)).min(bw.rules().progress_limits().max_limit);
                (end != member.info.limit).then_some(end)
            };
            settlements.push((account, SceneBattleSettlement { character: member.info.character, consumed_items: Vec::new(), reward, final_limit }));
        }
        let done = match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.snapshot_story_battle(accounts)) {
            Ok(done) => done,
            Err(_) => { self.ordinary_battle_failure("the terminal battle snapshot could not be queued"); return; }
        };
        bw.ordinary.get_mut(&id).expect("present").phase = Phase::Ending { terminal: Terminal { outcome, rewards, settlements }, done };
    }

    /// All sessions left an unfinished ordinary battle. Keep account reservations until the
    /// FIFO has proved every accepted item debit durable, then discard the uncommitted fight.
    pub(super) fn abandon_ordinary_battle(&mut self, id: BattleId) {
        let Some(record) = self.battle.as_ref().and_then(|bw| bw.ordinary.get(&id)) else { return; };
        let accounts = record.accounts();
        let done = match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.snapshot_story_battle(accounts)) {
            Ok(done) => done,
            Err(_) => { self.ordinary_battle_failure("the abandoned fight's item fence could not be queued"); return; }
        };
        if let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.get_mut(&id)) {
            record.phase = Phase::Abandoning(done);
        }
    }

    fn ordinary_terminal_matches(&self, id: BattleId, states: &[SceneState]) -> bool {
        self.battle.as_ref().and_then(|bw| bw.ordinary.get(&id)).is_some_and(|record|
            states.len() == record.expected.len() && states.iter().zip(&record.expected).all(|(state, expected)| {
                state.checkpoint.is_none() && state.reservation.is_none() && equal_pair_values(&state.pair, expected)
            }))
    }

    fn commit_ordinary_battle(&mut self, id: BattleId, terminal: Terminal, states: Vec<SceneState>) {
        let Some(record) = self.battle.as_ref().and_then(|bw| bw.ordinary.get(&id)) else { return; };
        if !self.ordinary_terminal_matches(id, &states) { self.ordinary_battle_failure("the terminal snapshot does not include exactly the accepted item debits"); return; }
        let members = states.into_iter().map(|state| {
            let battle = terminal.settlements.iter().find(|(account, _)| *account == state.pair.story.account_id)?.1.clone();
            Some(StoryBattleMember { expected: state.pair, battle })
        }).collect::<Option<Vec<_>>>();
        let Some(members) = members else { self.ordinary_battle_failure("a terminal fighter is missing its settlement"); return; };
        let job = StoryBattleJob { instance: record.instance, owner: record.owner, expected_story: record.before.clone(),
            next_story: if terminal.outcome == Outcome::Victory { record.step.progress.clone() } else { record.before.clone() }, members };
        let done = match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.save_story_battle(job.clone())) {
            Ok(done) => done,
            Err(_) => { self.ordinary_battle_failure("the atomic battle outcome could not be queued"); return; }
        };
        self.bump_story_save_epoch();
        if let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.get_mut(&id)) {
            record.phase = Phase::Committing { terminal, job, done };
        }
    }

    fn publish_ordinary_battle(&mut self, id: BattleId, terminal: Terminal, job: StoryBattleJob, saved: SceneBatchCommitted, now: Instant) {
        let Some(bw) = self.battle.as_ref() else { return; };
        let valid = saved.current.len() == job.members.len() && saved.current.iter().zip(&job.members).all(|(state, member)| {
            let mut expected = member.expected.clone();
            let Some(character) = expected.character.as_ref() else { return false; };
            let Ok(character) = crate::saver::preview_scene_settlement(character, bw.rules(), &member.battle) else { return false; };
            // The durable outcome receipt upgrades the character envelope to schema 3.
            // Keep terminal snapshot schema exact; only this acknowledged stamp is expected.
            expected.character = Some(fmv_persist::CharacterProgress { schema_version: 3, ..character });
            if expected.story.account_id == job.owner { expected.story.blob = job.next_story.clone(); }
            state.checkpoint.is_none() && state.reservation.is_none() && equal_pair_values(&state.pair, &expected)
        });
        if !valid { self.ordinary_battle_failure("the acknowledged outcome differs from the selected battle result"); return; }
        let Some(record) = self.battle.as_mut().and_then(|bw| bw.ordinary.remove(&id)) else { return; };
        self.bump_story_save_epoch();
        let game_over = terminal.outcome == Outcome::Defeat && record.setup.game_over_on_defeat;
        let mut recipients = Vec::new();
        for account in record.accounts() {
            let Some(entity) = self.by_account.get(&account).copied() else { continue; };
            if let Some(p) = self.players.get_mut(&entity).filter(|p| p.battle == Some(id)) {
                if game_over {
                    if let Some(bw) = self.battle.as_mut() {
                        bw.game_over.insert(account, GameOverHold { battle: id, entity, area: p.area.clone() });
                    }
                } else { p.battle = None; }
                p.walked = 0.0; p.dirty = true;
                recipients.push(entity);
            }
        }
        self.fan_to(&recipients, Arc::new(ServerMsg::BattleEnd { battle_id: id, outcome: terminal.outcome, rewards: terminal.rewards }));
        let mut failed = Vec::new();
        for state in saved.current {
            let Some(entity) = self.by_account.get(&state.pair.story.account_id).copied() else { continue; };
            let Some(character) = state.pair.character else { continue; };
            let Some(bw) = self.battle.as_ref() else { continue; };
            let Some(p) = self.players.get_mut(&entity) else { continue; };
            let leveled_up = p.progress.is_some_and(|old| character.level > old.level);
            let slot = record.mirrors.iter().find(|mirror| mirror.account == state.pair.story.account_id)
                .map_or(character.character, |mirror| mirror.progress.character);
            p.progress = Some(crate::progress::resolve(bw.rules(), &crate::progress::ProgressSeed::of(slot, Some(Progress::from_record(&character)))));
            p.inventory = Some(Inventory::resolve(bw.rules(), character.inventory.as_deref()));
            self.tell_progress(entity, leveled_up, &mut failed);
            self.tell_inventory(entity, &mut failed);
        }
        self.refresh_placements(&record.holds.keys().copied().collect(), &mut failed);
        self.drop_failed(failed);
        if game_over {
            // BattleEnd is the authoritative signal; the required setup policy tells each
            // client to show Game Over after its result panel. No field control or story
            // follow-up is released. These settled holds end independently on disconnect.
            self.discard_ordinary_story_battle_followups(record.owner);
            return;
        }
        // The complete outcome is durable and all resource messages are queued. Release guest
        // field barriers now so the owner's existing nearby co-op victory scene can admit them;
        // the owner keeps its token through the callback and any chained exact story step.
        for (&account, &token) in &record.holds {
            if account != record.owner { self.release_ordinary_battle_token(account, token); }
        }
        if terminal.outcome == Outcome::Victory {
            self.ordinary_story_battle_committed(record.owner, record.before, record.step, record.setup.formation_id, record.point, now);
        } else {
            self.discard_ordinary_story_battle_followups(record.owner);
        }
        for (account, token) in record.holds { self.release_ordinary_battle_token(account, token); }
    }

    fn release_ordinary_battle_token(&mut self, account: AccountId, token: u64) {
        if self.ordinary_story_account_held(account) || self.ordinary_battle_account_held(account) { return; }
        let Some(entity) = self.by_account.get(&account).copied() else { return; };
        let Some(p) = self.players.get_mut(&entity).filter(|p| p.story_pending == Some(token)) else { return; };
        p.story_pending = None;
        self.fan_to(&[entity], Arc::new(ServerMsg::StoryCommitted { id: token }));
    }
}
