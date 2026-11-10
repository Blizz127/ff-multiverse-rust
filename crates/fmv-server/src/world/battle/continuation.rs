//! Combat held behind a durable scene checkpoint. The world scene coordinator commits the frozen
//! plan before calling `start_scene_battle`, and commits every settlement before acknowledging the
//! result. This module never queues ordinary item, reward, or limit writes for these fights.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_core::{AccountId, EntityId, Fingerprint, InventoryEntry};
use fmv_ff7_battle::rules::{ActorId, Outcome};
use fmv_protocol::{BattleId, BattleInitialState, BattleMember, BattleRejection, BattleReward, BattleSetup,
    Controller, EscapePolicy, SceneRunId, ServerMsg, MAX_BATTLE_PARTY};
use serde::{Deserialize, Serialize};

use super::{Battle, Member, World};
use crate::saver::SceneBattleSettlement;

/// The persistent attempt identity plus its current presentation run. Runtime ids never identify
/// a durable operation; they only route the live presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SceneBattleContext {
    pub run: SceneRunId,
    pub instance: [u8; 32],
    pub ordinal: u32,
    pub attempt: u32,
}

/// A frozen battle input. Player entity ids are zero; accounts identify the saved roster. The
/// fingerprint refuses reconstruction against different rules data after a process restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SceneBattlePlan {
    pub setup: BattleSetup,
    pub fingerprint: Fingerprint,
    /// The inventory mirrors at the boundary. Accepted uses are staged against these on reconnect.
    pub inventories: Vec<(AccountId, Vec<InventoryEntry>)>,
}

impl SceneBattlePlan {
    pub fn accounts(&self) -> Vec<AccountId> {
        self.setup.party.iter().filter_map(BattleMember::account).collect()
    }
}

/// A completed fight's HP, MP, persistent statuses and limit gauges, paired with stable identities.
/// Member names/levels are descriptive; an account (or canon character) is the identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SceneCombatCarry {
    pub members: Vec<(BattleMember, BattleInitialState)>,
}

/// An engine outcome awaiting the scene coordinator's single durable settlement. In particular,
/// Defeat is not permission to continue the script or release the scene's holds.
#[derive(Debug, Clone)]
pub(crate) struct SceneBattleFinished {
    pub context: SceneBattleContext,
    pub id: BattleId,
    pub plan: SceneBattlePlan,
    pub outcome: Outcome,
    pub settlements: Vec<(AccountId, SceneBattleSettlement)>,
    /// Unboosted engine rewards, which clients independently verify.
    pub rewards: Vec<BattleReward>,
    pub carry: SceneCombatCarry,
}

/// Replay is an optimization for a still-running process, not a durable midfight checkpoint.
/// Once full, reattachment refuses rather than presenting an incomplete engine history.
const MAX_REPLAY_EVENTS: usize = 16_384;
const MAX_REPLAY_BYTES: usize = 16 * 1024 * 1024;
const REPLAY_BATCH: usize = 32;
const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);

struct ReplaySend {
    messages: VecDeque<Arc<ServerMsg>>,
    deadline: Instant,
}

pub(super) struct RunningSceneBattle {
    pub context: SceneBattleContext,
    pub plan: SceneBattlePlan,
    pub item_uses: BTreeMap<AccountId, Vec<u16>>,
    events: Vec<Arc<ServerMsg>>,
    replay_bytes: usize,
    replay_available: bool,
    replays: BTreeMap<EntityId, ReplaySend>,
}

fn canonical_member(mut member: BattleMember) -> BattleMember {
    if let Controller::Player { entity, .. } = &mut member.controller { *entity = EntityId(0); }
    member
}

fn same_actor(a: &BattleMember, b: &BattleMember) -> bool {
    match (a.account(), b.account()) {
        (Some(a), Some(b)) => a == b,
        (None, None) => a.character == b.character,
        _ => false,
    }
}

/// Apply complete resources by stable identity. Rebuilding the default templates first validates
/// the preserved HP/MP against any newly earned level; it does not refill a carried actor.
fn apply_carry(setup: &mut BattleSetup, carry: &SceneCombatCarry, rules: &dyn fmv_protocol::BattleRules,
    require_all: bool) -> Result<(), BattleRejection> {
    setup.initial_party = None;
    let state = rules.build(setup).map_err(|_| BattleRejection::UnknownFormation)?;
    let mut initial = Vec::with_capacity(setup.party.len());
    for (i, member) in setup.party.iter_mut().enumerate() {
        let actor = state.actor(ActorId(i as u8)).ok_or(BattleRejection::Unsupported)?;
        let matches: Vec<_> = carry.members.iter().filter(|(old, _)| same_actor(old, member)).collect();
        if matches.len() > 1 || (require_all && matches.len() != 1) { return Err(BattleRejection::Unsupported); }
        let value = if let Some((old, value)) = matches.first() {
            if old.character != member.character { return Err(BattleRejection::Unsupported); }
            *value
        } else {
            BattleInitialState { hp: actor.hp, mp: actor.mp, status: actor.status.set(), limit: actor.limit }
        };
        member.status = value.status;
        member.limit = value.limit;
        initial.push(value);
    }
    setup.initial_party = Some(initial);
    rules.build(setup).map_err(|_| BattleRejection::UnknownFormation)?;
    Ok(())
}

fn start_message(id: BattleId, setup: &BattleSetup, fingerprint: Fingerprint) -> ServerMsg {
    ServerMsg::EncounterStart {
        battle_id: id,
        formation_id: setup.formation_id,
        seed: setup.seed,
        party: setup.party.clone(),
        preemptive: setup.preemptive,
        back_attack: setup.back_attack,
        battle_speed: setup.battle_speed,
        ticks_per_second: setup.ticks_per_second,
        escape_policy: setup.escape_policy,
        initial_party: setup.initial_party.clone(),
        data_fingerprint: fingerprint,
    }
}

impl Battle {
    pub(super) fn replaying_scene(&self) -> bool {
        self.continuation.as_ref().is_some_and(|c| !c.replays.is_empty())
    }

    pub(super) fn scene_member_gone(&mut self, entity: EntityId) {
        if let Some(scene) = self.continuation.as_mut() { scene.replays.remove(&entity); }
    }

    pub(super) fn record_scene_event(&mut self, event: &ServerMsg) {
        let Some(scene) = self.continuation.as_mut().filter(|c| c.replay_available) else { return };
        let size = serde_json::to_vec(event).map_or(MAX_REPLAY_BYTES + 1, |bytes| bytes.len());
        if scene.events.len() == MAX_REPLAY_EVENTS || scene.replay_bytes.saturating_add(size) > MAX_REPLAY_BYTES {
            scene.replay_available = false;
            scene.events.clear();
        } else {
            scene.events.push(Arc::new(event.clone()));
            scene.replay_bytes += size;
        }
    }
}

impl World {
    /// Select the same nearby co-op roster as an ordinary battle, allowing the named scene's hold.
    /// No engine starts and no message is sent until the caller has committed this plan.
    pub(crate) fn prepare_scene_battle(&mut self, requester: EntityId, run: SceneRunId, formation: u16,
        escape_policy: EscapePolicy, carry: Option<&SceneCombatCarry>) -> Result<SceneBattlePlan, BattleRejection> {
        let mut setup = self.prepare_battle_setup(requester, formation, Some(run))?;
        setup.escape_policy = escape_policy;
        let rules = self.battle.as_ref().ok_or(BattleRejection::Unsupported)?.settings.rules.as_ref();
        if let Some(carry) = carry { apply_carry(&mut setup, carry, rules, false)?; }
        rules.build(&setup).map_err(|_| BattleRejection::UnknownFormation)?;
        let inventories = setup.party.iter().filter_map(BattleMember::account).map(|account| {
            let inventory = self.by_account.get(&account).and_then(|entity| self.players.get(entity))
                .and_then(|p| p.inventory.as_ref()).ok_or(BattleRejection::Unsupported)?;
            Ok((account, inventory.entries().to_vec()))
        }).collect::<Result<Vec<_>, BattleRejection>>()?;
        setup.party = setup.party.into_iter().map(canonical_member).collect();
        Ok(SceneBattlePlan { setup, fingerprint: rules.data_fingerprint(), inventories })
    }

    /// Check a recovered frozen plan against the complete coherent group before restoring any
    /// inventory or starting an engine. The plan cannot invent a participant, change its combat
    /// character or level, or replace the durable bag. Carried combat-only resources are validated
    /// by the shared engine; the scene coordinator additionally binds them to its accepted replay.
    pub(crate) fn validate_scene_battle_plan(&self, plan: &SceneBattlePlan, states: &[fmv_persist::SceneState]) -> bool {
        let Some(bw) = self.battle.as_ref() else { return false; };
        let rules = bw.rules();
        let accounts = plan.accounts();
        let unique: BTreeSet<_> = accounts.iter().copied().collect();
        if plan.fingerprint != rules.data_fingerprint() || plan.setup.party.is_empty()
            || plan.setup.party.len() > MAX_BATTLE_PARTY || accounts.is_empty() || accounts.len() != unique.len()
            || states.len() != accounts.len() || plan.inventories.len() != accounts.len()
            || states.first().is_some_and(|first| states.iter().any(|s| s.pair.story.game != first.pair.story.game))
            || states.iter().map(|s| s.pair.story.account_id).collect::<BTreeSet<_>>() != unique
            || plan.inventories.iter().map(|(a, _)| *a).collect::<BTreeSet<_>>() != unique
            || plan.setup.party.iter().filter_map(BattleMember::entity).any(|entity| entity != EntityId(0))
            || !(1..=1000).contains(&plan.setup.ticks_per_second) {
            return false;
        }
        let Some(formation) = rules.formation(plan.setup.formation_id) else { return false; };
        if (plan.setup.preemptive, plan.setup.back_attack) != (formation.preemptive, formation.back_attack) { return false; }
        let mut companion_characters = BTreeSet::new();
        let cap = rules.progress_limits().max_limit;
        for member in &plan.setup.party {
            // This server has no equipment selection path yet: plans only freeze the default templates.
            if member.weapon.is_some() || member.armor.is_some() || member.commander.is_some_and(|a| !unique.contains(&a)) {
                return false;
            }
            let Some(account) = member.account() else {
                if !companion_characters.insert(member.character) { return false; }
                if plan.setup.initial_party.is_none() {
                    let expected_limit = if bw.settings.debug_limit_full { cap } else { 0 };
                    if member.limit != expected_limit || member.status != bw.settings.debug_status { return false; }
                }
                continue;
            };
            let Some(state) = states.iter().find(|s| s.pair.story.account_id == account) else { return false; };
            let Some(record) = state.pair.character.as_ref() else { return false; };
            if record.account_id != account || record.game != state.pair.story.game
                || member.character != record.character || member.level != bw.settings.level.unwrap_or(record.level)
                || member.commander.is_some() {
                return false;
            }
            let expected_inventory = crate::inventory::Inventory::resolve(rules, record.inventory.as_deref());
            if plan.inventories.iter().find(|(a, _)| *a == account).is_none_or(|(_, items)| items != expected_inventory.entries()) {
                return false;
            }
            if plan.setup.initial_party.is_none() {
                let expected_limit = if bw.settings.debug_limit_full { cap } else { record.limit.min(cap) };
                if member.limit != expected_limit || member.status != bw.settings.debug_status { return false; }
            }
        }
        rules.build(&plan.setup).is_ok()
    }

    /// Validate a recovered postbattle resume without consuming a seed or rewriting anything.
    /// The retained plan describes the fight before its rewards, so player levels and bags are
    /// refreshed from the acknowledged records before checking the carried combat resources.
    /// Every carry entry must identify exactly its original frozen actor, in the original order.
    pub(crate) fn validate_scene_battle_resume(&self, plan: &SceneBattlePlan, carry: &SceneCombatCarry,
        states: &[fmv_persist::SceneState]) -> bool {
        let Some(bw) = self.battle.as_ref() else { return false; };
        if carry.members.len() != plan.setup.party.len()
            || carry.members.iter().zip(&plan.setup.party).any(|((member, _), original)| member != original) {
            return false;
        }
        let mut resumed = plan.clone();
        resumed.inventories.clear();
        for member in &mut resumed.setup.party {
            let Some(account) = member.account() else { continue; };
            let Some(record) = states.iter().find(|s| s.pair.story.account_id == account).and_then(|s| s.pair.character.as_ref()) else {
                return false;
            };
            if record.account_id != account || record.character != member.character { return false; }
            member.level = bw.settings.level.unwrap_or(record.level);
            let inventory = crate::inventory::Inventory::resolve(bw.rules(), record.inventory.as_deref());
            resumed.inventories.push((account, inventory.entries().to_vec()));
        }
        if apply_carry(&mut resumed.setup, carry, bw.rules(), true).is_err() { return false; }
        self.validate_scene_battle_plan(&resumed, states)
    }

    /// The next authored fight retains every frozen player and companion, including disconnected
    /// accounts. Levels and bags come from the preceding acknowledged transaction; HP/MP come from
    /// that fight's engine result. Canon companions keep the first fight's level throughout this
    /// chain: they have no independent experience record, and a different online party leader must
    /// not reinterpret the frozen group. This is a port policy, not original companion progression.
    /// No nearby roster selection or healing occurs here.
    pub(crate) fn prepare_next_scene_battle(&mut self, previous: &SceneBattlePlan, formation: u16,
        carry: &SceneCombatCarry, states: &[fmv_persist::SceneState]) -> Result<SceneBattlePlan, BattleRejection> {
        let bw = self.battle.as_mut().ok_or(BattleRejection::Unsupported)?;
        if previous.fingerprint != bw.rules().data_fingerprint() { return Err(BattleRejection::Unsupported); }
        let accounts = previous.accounts();
        let supplied: BTreeSet<_> = states.iter().map(|s| s.pair.story.account_id).collect();
        if supplied.len() != states.len() || supplied != accounts.iter().copied().collect() {
            return Err(BattleRejection::Unsupported);
        }
        let info = bw.rules().formation(formation).ok_or(BattleRejection::UnknownFormation)?;
        let mut setup = previous.setup.clone();
        setup.formation_id = formation;
        setup.seed = bw.next_seed();
        setup.preemptive = info.preemptive;
        setup.back_attack = info.back_attack;
        let mut inventories = Vec::new();
        for member in &mut setup.party {
            let Some(account) = member.account() else { continue };
            let record = states.iter().find(|s| s.pair.story.account_id == account).and_then(|s| s.pair.character.as_ref())
                .ok_or(BattleRejection::Unsupported)?;
            if record.account_id != account || record.character != member.character { return Err(BattleRejection::Unsupported); }
            member.level = bw.settings.level.unwrap_or(record.level);
            let inventory = crate::inventory::Inventory::resolve(bw.rules(), record.inventory.as_deref());
            inventories.push((account, inventory.entries().to_vec()));
        }
        if carry.members.len() != setup.party.len() { return Err(BattleRejection::Unsupported); }
        apply_carry(&mut setup, carry, bw.rules(), true)?;
        Ok(SceneBattlePlan { setup, fingerprint: previous.fingerprint, inventories })
    }

    /// Start exactly the saved input after its prepare acknowledgment. A disconnected saved member
    /// stays in the engine; only sessions already held by this run attach to it.
    pub(crate) fn start_scene_battle(&mut self, context: SceneBattleContext, plan: SceneBattlePlan, now: Instant)
        -> Result<BattleId, BattleRejection> {
        let bw = self.battle.as_mut().ok_or(BattleRejection::Unsupported)?;
        if plan.fingerprint != bw.rules().data_fingerprint() || plan.setup.party.is_empty() || plan.setup.party.len() > MAX_BATTLE_PARTY {
            return Err(BattleRejection::Unsupported);
        }
        let accounts = plan.accounts();
        let inventory_accounts: BTreeSet<_> = plan.inventories.iter().map(|(a, _)| *a).collect();
        if accounts.is_empty() || accounts.iter().copied().collect::<BTreeSet<_>>().len() != accounts.len()
            || inventory_accounts.len() != plan.inventories.len() || inventory_accounts != accounts.iter().copied().collect()
            || plan.setup.party.iter().filter_map(BattleMember::entity).any(|entity| entity != EntityId(0))
            || bw.battles.values().any(|b| b.members.iter().filter_map(|m| m.info.account()).any(|a| accounts.contains(&a))) {
            return Err(BattleRejection::Unsupported);
        }
        let mut bags = BTreeMap::new();
        for (account, entries) in &plan.inventories {
            let bag = crate::inventory::Inventory::from_entries(entries, bw.rules().max_item_stack());
            if bag.entries() != entries { return Err(BattleRejection::Unsupported); }
            bags.insert(*account, bag);
        }
        let mut setup = plan.setup.clone();
        let mut connected = Vec::new();
        for member in &mut setup.party {
            let Controller::Player { entity, account } = &mut member.controller else { continue };
            if let Some(current) = self.by_account.get(account).copied() {
                let p = self.players.get(&current).ok_or(BattleRejection::Unsupported)?;
                if p.scene != Some(context.run) || p.battle.is_some() || p.transit.is_some() {
                    return Err(BattleRejection::Unsupported);
                }
                *entity = current;
                connected.push(current);
            }
        }
        if connected.is_empty() { return Err(BattleRejection::NotInBattle); }
        let state = bw.rules().build(&setup).map_err(|_| BattleRejection::UnknownFormation)?;
        let id = BattleId(bw.next_id);
        bw.next_id = bw.next_id.checked_add(1).ok_or(BattleRejection::Unsupported)?;
        let message = start_message(id, &setup, plan.fingerprint);
        let members = setup.party.into_iter().map(|info| Member {
            connected: info.entity().is_some_and(|entity| connected.contains(&entity)), info,
        }).collect::<Vec<_>>();
        let count = members.len();
        bw.battles.insert(id, Battle {
            state, members, commands: vec![0; count], ready_since: vec![None; count], last_action: None,
            clock: now, formation: plan.setup.formation_id, seed: plan.setup.seed, status_counts: [0; 3],
            then: None, counters: 0,
            continuation: Some(RunningSceneBattle { context, plan, item_uses: BTreeMap::new(),
                events: Vec::new(), replay_bytes: 0, replay_available: true, replays: BTreeMap::new() }),
        });
        for entity in &connected {
            if let Some(p) = self.players.get_mut(entity) {
                p.battle = Some(id);
                // A retry reuses the durable prebattle bag, discarding the failed attempt's staged costs.
                p.inventory = bags.remove(&p.account);
            }
        }
        self.logs.info(format!("scene battle started battle={} run={} ordinal={} attempt={}", id.0, context.run.0, context.ordinal, context.attempt));
        self.fan_to(&connected, Arc::new(ServerMsg::SceneBattleStarted { run: context.run, ordinal: context.ordinal, battle_id: id }));
        self.fan_to(&connected, Arc::new(message));
        Ok(id)
    }

    /// Reattach an authenticated reserved account to the existing engine and replay all accepted
    /// input before it can command. A capped or mismatched history is refused, never approximated.
    pub(crate) fn reattach_scene_battle(&mut self, entity: EntityId, context: SceneBattleContext) -> Result<BattleId, BattleRejection> {
        let p = self.players.get(&entity).ok_or(BattleRejection::NotInBattle)?;
        if p.scene != Some(context.run) || p.battle.is_some() || p.transit.is_some() { return Err(BattleRejection::Unsupported); }
        let account = p.account;
        let bw = self.battle.as_mut().ok_or(BattleRejection::Unsupported)?;
        let (id, battle) = bw.battles.iter_mut().find(|(_, b)| b.continuation.as_ref().is_some_and(|c| c.context == context)
            && b.members.iter().any(|m| m.info.account() == Some(account))).ok_or(BattleRejection::NotInBattle)?;
        let id = *id;
        let scene = battle.continuation.as_mut().ok_or(BattleRejection::Unsupported)?;
        if !scene.replay_available { return Err(BattleRejection::Unsupported); }
        let index = battle.members.iter().position(|m| m.info.account() == Some(account)).ok_or(BattleRejection::NotInBattle)?;
        if battle.members[index].connected { return Err(BattleRejection::AlreadyInBattle); }
        // Validate everything before changing either authority or the live player's inventory.
        let mut inventory = crate::inventory::Inventory::from_entries(
            &scene.plan.inventories.iter().find(|(a, _)| *a == account).ok_or(BattleRejection::Unsupported)?.1,
            bw.settings.rules.max_item_stack());
        for item in scene.item_uses.get(&account).into_iter().flatten() {
            if !inventory.take_one(*item) { return Err(BattleRejection::Unsupported); }
        }
        let mut setup = scene.plan.setup.clone();
        setup.party = battle.members.iter().map(|m| m.info.clone()).collect();
        setup.party[index].controller = Controller::Player { entity, account };
        let mut messages = VecDeque::with_capacity(scene.events.len() + 3);
        messages.push_back(Arc::new(ServerMsg::SceneBattleStarted { run: context.run, ordinal: context.ordinal, battle_id: id }));
        messages.push_back(Arc::new(start_message(id, &setup, scene.plan.fingerprint)));
        messages.extend(scene.events.iter().cloned());
        messages.push_back(Arc::new(ServerMsg::BattleEvent { battle_id: id, tick: battle.state.tick(), events: Vec::new(), command: None }));
        messages.push_back(Arc::new(inventory.message()));
        battle.members[index].info.controller = Controller::Player { entity, account };
        battle.members[index].connected = true;
        scene.replays.insert(entity, ReplaySend { messages, deadline: Instant::now() + REPLAY_TIMEOUT });
        if let Some(p) = self.players.get_mut(&entity) { p.battle = Some(id); p.inventory = Some(inventory); }
        // Small histories can finish immediately. Larger ones use the ordinary world tick, without
        // overflowing the bounded network queue or advancing combat ahead of the joining replay.
        self.pump_scene_replay(id, Instant::now());
        Ok(id)
    }

    /// Returns true when this tick was spent catching a session up. Wall-clock replay time never
    /// advances the engine; a stalled writer cannot hold other players beyond the bounded timeout.
    pub(super) fn pump_scene_replay(&mut self, id: BattleId, now: Instant) -> bool {
        let Some(battle) = self.battle.as_mut().and_then(|bw| bw.battles.get_mut(&id)) else { return false };
        let Some(scene) = battle.continuation.as_mut().filter(|c| !c.replays.is_empty()) else { return false };
        battle.clock = now;
        let mut failed = Vec::new();
        let mut complete = Vec::new();
        for (entity, replay) in &mut scene.replays {
            let Some(player) = self.players.get(entity) else { complete.push(*entity); continue };
            if now >= replay.deadline { failed.push((*entity, true)); continue; }
            for _ in 0..REPLAY_BATCH {
                if player.tx.capacity() <= (player.tx.max_capacity() / 4).min(8) { break; }
                let Some(message) = replay.messages.pop_front() else { break };
                match player.tx.try_send(message) {
                    Ok(()) => {},
                    Err(tokio::sync::mpsc::error::TrySendError::Full(message)) => { replay.messages.push_front(message); break; },
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => { failed.push((*entity, false)); break; },
                }
            }
            if replay.messages.is_empty() { complete.push(*entity); }
        }
        for entity in complete { scene.replays.remove(&entity); }
        self.drop_failed(failed);
        true
    }

    pub(crate) fn take_scene_battle_finished(&mut self) -> Vec<SceneBattleFinished> {
        self.battle.as_mut().map_or_else(Vec::new, |bw| std::mem::take(&mut bw.scene_finished))
    }

    pub(crate) fn take_scene_battle_dormant(&mut self) -> Vec<(SceneBattleContext, SceneBattlePlan)> {
        self.battle.as_mut().map_or_else(Vec::new, |bw| std::mem::take(&mut bw.scene_dormant))
    }

    /// Publish the engine outcome only after the caller has committed its complete settlement (or
    /// retained the prebattle checkpoint for explicit game-over/retry). Scene holds remain in force.
    pub(crate) fn acknowledge_scene_battle_finished(&mut self, finished: &SceneBattleFinished) {
        let accounts = finished.plan.accounts();
        let mut recipients = Vec::new();
        for account in accounts {
            if let Some(entity) = self.by_account.get(&account).copied() {
                if let Some(p) = self.players.get_mut(&entity).filter(|p| p.battle == Some(finished.id)) {
                    p.battle = None;
                    p.walked = 0.0;
                    recipients.push(entity);
                }
            }
        }
        self.fan_to(&recipients, Arc::new(ServerMsg::BattleEnd { battle_id: finished.id,
            outcome: finished.outcome, rewards: finished.rewards.clone() }));
    }

    pub(super) fn queue_scene_battle_finished(&mut self, id: BattleId, mut battle: Battle, outcome: Outcome, rewards: Vec<BattleReward>) {
        let Some(scene) = battle.continuation.take() else { return };
        let Some(bw) = self.battle.as_mut() else { return };
        let cap = bw.rules().progress_limits().max_limit;
        let mut settlements = Vec::new();
        for (i, member) in battle.members.iter().enumerate() {
            let Some(account) = member.info.account() else { continue };
            let mut reward = rewards.iter().find(|r| r.account == account).cloned().unwrap_or_else(|| BattleReward::new(account, 0, 0, 0));
            if !reward.is_nothing() { reward.exp = reward.exp.saturating_add(bw.settings.debug_bonus_exp); }
            let actor = ActorId(i as u8);
            let final_limit = if bw.settings.debug_limit_full || battle.state.limit_for(actor).is_none() { None } else {
                let end = battle.state.limit_gauge(actor).saturating_add(u16::from(bw.settings.debug_bonus_limit)).min(cap);
                (end != member.info.limit).then_some(end)
            };
            settlements.push((account, SceneBattleSettlement { character: member.info.character,
                consumed_items: scene.item_uses.get(&account).cloned().unwrap_or_default(), reward, final_limit }));
        }
        let carry = SceneCombatCarry { members: battle.members.iter().map(|m| canonical_member(m.info.clone()))
            .zip(fmv_protocol::battle::battle_party_snapshot(&battle.state)).collect() };
        bw.scene_finished.push(SceneBattleFinished { context: scene.context, id, plan: scene.plan,
            outcome, settlements, rewards, carry });
    }
}
