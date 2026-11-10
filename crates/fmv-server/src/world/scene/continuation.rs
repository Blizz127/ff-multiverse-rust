//! Durable scenes, including battles and required arrival scenes linked by saved landings.
//!
//! The host interprets game data. The world validates reported windows, retains participants,
//! and waits for the ordered saver before starting combat or resuming the script.

use super::SceneResult;

/// One authoritative boundary of a scene replay. Every boundary includes the exact current
/// persistent state and resources, so effects before a battle are committed with its checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneBoundary {
    pub source: [u8; 32],
    pub progress: Vec<u8>,
    pub gil: u32,
    pub at: SceneBoundaryKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneBoundaryKind {
    /// `identity` is the host's bounded, canonical identity of this exact window instruction.
    Window { step: u32, identity: Vec<u8>, choices: Option<[u8; 2]> },
    /// `identity` binds the script instruction and battle ordinal. Unknown field battle modes
    /// remain unknown; the world must not silently assume a mode when starting combat.
    Battle { ordinal: u32, identity: Vec<u8>, formation: u16, flags: Option<u16> },
    Complete(SceneResult),
}

use std::time::Instant;
use fmv_core::{AccountId, EntityId, Pos};
use fmv_adapter_api::{Exit, ExitKind};
use fmv_persist::{SceneBatchCommitted, SceneCheckpoint, SceneOperation, SceneOperationKind, ScenePhase, SceneReservation, SceneState};
use fmv_protocol::{BattleId, EscapePolicy, SceneOutcome, SceneParticipant, SceneReplay, SceneReplayEvent, SceneRole, SceneRunId, ServerMsg};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use crate::saver::{SceneBatchJob, SceneBatchMember, SceneSaveError};
use super::{SceneRun, World, SCENE_STEP_POINT};
use super::super::battle::{SceneBattleContext, SceneBattleFinished, SceneBattlePlan, SceneCombatCarry};

// Persisted execution contract, independent of the JSON envelope. Version 2 adds authored
// ambient tasks and field effects/motion; an older checkpoint must not be silently rerun
// under changed execution semantics. The adapter also binds its interpreter in source_hash.
const REPLAY_VERSION: u32 = 2;

/// Only validated scalar metadata is persisted. In particular, a final landing drops the battle
/// roster and contains no nested account ids, so the account can later be restored under a new id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Payload {
    pub(super) replay: SceneReplay,
    field: String,
    position: Pos,
    facing: f32,
    plan: Option<SceneBattlePlan>,
    carry: Option<SceneCombatCarry>,
    landing: Option<Landing>,
    /// Defeat is a durable hold. A process restart cannot silently retry the checkpoint.
    game_over: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Landing { field: String, position: Pos, facing: Option<f32>, hint: Option<u32> }

impl Landing {
    fn from_exit(exit: &Exit) -> Self {
        Self { field: exit.target.clone(), position: exit.arrival, facing: exit.arrival_facing, hint: exit.arrival_hint }
    }
    fn exit(&self) -> Exit {
        Exit { line: (Pos::default(), Pos::default()), target: self.field.clone(), arrival: self.position,
            arrival_facing: self.facing, arrival_hint: self.hint, kind: ExitKind::Gateway }
    }
}

pub(super) struct Continuation {
    owner: AccountId,
    instance: [u8; 32],
    pub(super) accounts: Vec<AccountId>,
    pub(super) payload: Payload,
    boundary: SceneBoundary,
    checkpoint: Option<SceneCheckpoint>,
    states: Vec<SceneState>,
    frozen: Vec<FrozenMirror>,
    phase: Phase,
}

/// Runtime mirrors captured before the ordered snapshot. Disconnect cannot erase the
/// preconditions of a read or write which the server has already queued for the frozen cohort.
struct FrozenMirror {
    account: AccountId,
    story: Vec<u8>,
    progress: Option<crate::progress::Progress>,
    inventory: Option<crate::inventory::Inventory>,
}

enum Phase {
    Playing,
    Snapshot { done: oneshot::Receiver<Result<Vec<SceneState>, SceneSaveError>>, action: Action },
    Commit { done: oneshot::Receiver<Result<SceneBatchCommitted, SceneSaveError>>, action: Action },
    Fighting { id: BattleId, ordinal: u32 },
    Dormant,
    GameOver { id: BattleId },
    Landing,
}

#[derive(Clone)]
enum Action {
    Prepare { plan: SceneBattlePlan, ordinal: u32 },
    Resolve(SceneBattleFinished),
    Finish(SceneResult),
    Continue { record: SceneRun, instance: [u8; 32], boundary: SceneBoundary, payload: Box<Payload> },
    Complete(SceneResult),
}

fn fresh_instance() -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes)
}

/// A saved execution prefix must also fit the bounded reconnect message. The storage envelope
/// has independent limits and includes combat setup, so accepting it alone is insufficient.
/// Reserve the largest supported cohort and numeric ids, rather than relying on today's small
/// entity numbers or a disconnected roster, to keep later announcements representable too.
fn checkpoint_can_be_announced(checkpoint: &SceneCheckpoint) -> bool {
    if checkpoint.replay.len() > fmv_persist::MAX_SCENE_REPLAY_BYTES { return false; }
    let Ok(payload) = serde_json::from_slice::<Payload>(&checkpoint.replay) else { return false; };
    replay_can_be_announced(&checkpoint.scene_key, &checkpoint.original_story, checkpoint.seed,
        checkpoint.original_gil, &payload.replay)
}

fn replay_can_be_announced(scene: &str, progress: &[u8], seed: u32, gil: u32, replay: &SceneReplay) -> bool {
    if replay.validate().is_err() { return false; }
    let participants = (0..fmv_persist::MAX_SCENE_BATCH_ACCOUNTS).map(|slot| SceneParticipant {
        entity: EntityId(u64::MAX - slot as u64), spot: slot as u8,
    }).collect();
    fmv_protocol::encode(&ServerMsg::SceneStart {
        run: SceneRunId(u64::MAX), scene: scene.to_owned(), seed, progress: progress.to_vec(), gil,
        replay: Some(replay.clone()), owner: EntityId(u64::MAX), participants, you: SceneRole::Watcher,
    }).is_ok()
}

impl Continuation {
    pub(super) fn destinations(&self) -> Vec<String> {
        let mut fields = vec![self.payload.field.clone()];
        if let Some(landing) = &self.payload.landing { fields.push(landing.field.clone()); }
        let action = match &self.phase { Phase::Snapshot { action, .. } | Phase::Commit { action, .. } => Some(action), _ => None };
        if let Some(Action::Finish(result)) = action {
            if let Some(exit) = &result.travel { fields.push(exit.target.clone()); }
        }
        fields
    }
    fn consumed(&self) -> u32 { self.checkpoint.as_ref().map_or(0, |c| c.consumed_battles) }
    fn checkpoint(&self, record: &SceneRun, phase: ScenePhase, consumed: u32, payload: &Payload) -> Option<SceneCheckpoint> {
        let replay = serde_json::to_vec(payload).ok()?;
        if replay.len() > fmv_persist::MAX_SCENE_REPLAY_BYTES { return None; }
        Some(SceneCheckpoint { instance: self.instance, scene_key: record.scene.clone(), source_hash: payload.replay.source,
            replay_version: REPLAY_VERSION, original_story: record.progress.clone(), original_gil: record.gil,
            seed: record.seed, replay, phase, consumed_battles: consumed })
    }
    fn context(&self, run: SceneRunId) -> Option<SceneBattleContext> {
        match self.checkpoint.as_ref()?.phase {
            ScenePhase::BeforeBattle { ordinal, attempt, .. } => Some(SceneBattleContext { run, instance: self.instance, ordinal: ordinal + 1, attempt }),
            _ => None,
        }
    }
}

impl World {
    pub(super) fn initialize_continuation(&mut self, run: SceneRunId, record: &SceneRun) -> bool {
        if self.scenes.continuations.contains_key(&run) { return true; }
        let Some(host) = self.scenes.host.as_ref().filter(|host| host.continuation(&record.scene)) else { return true; };
        if record.preview { return true; }
        let Some(player) = self.players.get(&record.owner) else { return false; };
        if !record.writes_story || record.then.is_some() || player.preview.is_some() { return false; }
        let origin = (!host.automatic(&record.scene)).then_some(fmv_protocol::SceneOrigin {
            pos: player.snapshot.pos, facing: player.snapshot.facing, triangle: None,
        });
        let Some(boundary) = host.initial_continuation(&record.scene, &record.progress, record.seed, record.gil, origin) else { return false; };
        let Some(instance) = fresh_instance() else { return false; };
        let payload = Payload { replay: SceneReplay { source: boundary.source, origin, events: vec![] }, field: player.area.clone(),
            position: player.snapshot.pos, facing: player.snapshot.facing, plan: None, carry: None, landing: None, game_over: false };
        if !replay_can_be_announced(&record.scene, &record.progress, record.seed, record.gil, &payload.replay) { return false; }
        self.scenes.continuations.insert(run, Continuation { owner: player.account, instance, accounts: vec![player.account],
            payload, boundary, checkpoint: None, states: vec![], frozen: vec![], phase: Phase::Playing });
        true
    }

    /// Run only after the adapter accepted the movement. A line scene owns the remainder of this move.
    pub(in crate::world) fn check_scene_line(&mut self, entity: EntityId, from: Pos, to: Pos, now: Instant) -> bool {
        let Some(player) = self.players.get(&entity) else { return false; };
        if player.scene.is_some() || player.battle.is_some() || player.transit.is_some() || player.preview.is_some()
            || player.placement.as_ref().is_some_and(|p| !p.story_advance_allowed) { return false; }
        let Some(host) = self.scenes.host.as_ref() else { return false; };
        let Some(scene) = host.line_scene(&player.area, &player.progression, from, to) else { return false; };
        if !host.continuation(&scene) || !host.offered(&scene, &player.progression) { return false; }
        let watchers = self.watchers_for(entity, player.account, to);
        let run = SceneRunId(self.scenes.next_run.max(1));
        self.scenes.next_run = run.0.saturating_add(1);
        let record = SceneRun { scene, owner: entity, watchers, seed: run.0.wrapping_mul(0x9E37_79B9) as u32,
            progress: player.progression.clone(), gil: player.progress.map_or(0, |p| p.gil), answers: vec![], steps: 0,
            started: now, last_activity: now, writes_story: true, preview: false, then: None };
        self.start_run(run, record, &mut Vec::new());
        true
    }

    pub(super) fn continuation_step(&mut self, entity: EntityId, run: SceneRunId, step: u32, answer: Option<u8>, now: Instant) {
        let Some(record) = self.scenes.runs.get(&run).cloned().filter(|r| r.owner == entity) else { return; };
        let Some(mut c) = self.scenes.continuations.remove(&run) else { return; };
        if !matches!(c.phase, Phase::Playing) { self.scenes.continuations.insert(run, c); return; }
        let accepted = c.payload.replay.events.len();
        let valid = match &c.boundary.at {
            SceneBoundaryKind::Window { step: expected, identity, choices } if *expected == step && record.steps == step => {
                let valid_answer = match (choices, answer) {
                    (None, None) => true,
                    (Some([first, last]), Some(answer)) => (*first..=*last).contains(&answer),
                    _ => false,
                };
                if valid_answer {
                    c.payload.replay.events.push(SceneReplayEvent::Window { step, identity: identity.clone(), answer });
                }
                valid_answer
            }
            _ => false,
        };
        // A reconnect to this still-running process receives the accepted in-memory prefix,
        // even before its next durable boundary. Keep that prefix within the same wire bound.
        let valid = valid && replay_can_be_announced(&record.scene, &record.progress, record.seed, record.gil, &c.payload.replay);
        let boundary = valid.then(|| self.scenes.host.as_ref()?.evaluate_continuation(&record.scene, &record.progress,
            record.seed, record.gil, Some(&c.payload.replay))).flatten();
        let Some(boundary) = boundary else {
            c.payload.replay.events.truncate(accepted);
            self.scenes.continuations.insert(run, c);
            self.continuation_bad_input(entity, run, "invalid continuation window transcript");
            return;
        };
        c.boundary = boundary;
        self.scenes.continuations.insert(run, c);
        if let Some(record) = self.scenes.runs.get_mut(&run) { record.steps += 1; record.last_activity = now; }
        let mut failed = vec![];
        for watcher in record.watchers { self.send_to(watcher, ServerMsg::SceneStep { run, step, answer }, &mut failed); }
        self.drop_failed(failed);
    }

    pub(super) fn continuation_battle_ready(&mut self, entity: EntityId, run: SceneRunId, ordinal: u32, step: u32, _now: Instant) {
        let Some(record) = self.scenes.runs.get(&run).cloned().filter(|r| r.owner == entity && r.steps == step) else { return; };
        let Some(c) = self.scenes.continuations.get(&run) else { return; };
        if !matches!(c.phase, Phase::Playing) { return; }
        let SceneBoundaryKind::Battle { ordinal: expected, formation, flags, .. } = c.boundary.at else {
            self.continuation_bad_input(entity, run, "battle reported at a different script boundary"); return;
        };
        if ordinal != expected || ordinal != c.consumed() + 1 || flags != Some(8) {
            self.continuation_bad_input(entity, run, "unsupported or mismatched authored battle"); return;
        }
        if c.consumed() > 0 && (c.payload.plan.is_none() || c.payload.carry.is_none()) {
            self.continuation_failure(run, "a resumed scene lost its frozen battle roster or resources"); return;
        }
        let previous = c.payload.plan.clone();
        let carry = c.payload.carry.clone();
        let states = c.states.clone();
        let plan = if let (Some(previous), Some(carry)) = (previous, carry) {
            self.prepare_next_scene_battle(&previous, formation, &carry, &states)
        } else { self.prepare_scene_battle(record.owner, run, formation, EscapePolicy::Forbidden, None) };
        let Ok(plan) = plan else { self.continuation_failure(run, "the frozen battle roster cannot be prepared"); return; };
        self.snapshot_continuation(run, Action::Prepare { plan, ordinal });
    }

    fn snapshot_continuation(&mut self, run: SceneRunId, action: Action) {
        let Some(c) = self.scenes.continuations.get_mut(&run) else { return; };
        let mut accounts = match &action { Action::Prepare { plan, .. } => plan.accounts(), _ => c.accounts.clone() };
        if let Some(owner) = accounts.iter().position(|a| *a == c.owner) { accounts.swap(0, owner); } else {
            self.continuation_failure(run, "the roster omitted its scene owner"); return;
        }
        c.accounts = accounts.clone(); // Freeze every account before ordering the read behind pending saves.
        if c.states.is_empty() {
            c.frozen = accounts.iter().filter_map(|account| self.by_account.get(account).and_then(|entity| self.players.get(entity)))
                .map(|p| FrozenMirror { account: p.account, story: p.progression.clone(), progress: p.progress, inventory: p.inventory.clone() }).collect();
        }
        self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
        match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.snapshot_scene_batch(accounts)) {
            Ok(done) => c.phase = Phase::Snapshot { done, action },
            Err(SceneSaveError::Stopped) if self.saver.is_some() => self.stop_for_scene_storage_recovery(),
            Err(_) => self.continuation_failure(run, "the continuation snapshot could not be queued"),
        }
    }

    pub(super) fn continuation_end(&mut self, entity: EntityId, run: SceneRunId, outcome: SceneOutcome, _now: Instant) {
        let Some(record) = self.scenes.runs.get(&run).filter(|r| r.owner == entity) else { return; };
        let Some(c) = self.scenes.continuations.get(&run) else { return; };
        if !matches!(c.phase, Phase::Playing) { return; }
        if outcome != SceneOutcome::Finished {
            if c.checkpoint.is_none() {
                let record = record.clone();
                self.scenes.runs.remove(&run);
                self.scenes.continuations.remove(&run);
                let mut failed = vec![];
                let released = std::iter::once(record.owner).chain(record.watchers).collect::<Vec<_>>();
                for &entity in &released { self.release(entity, run, SceneOutcome::Aborted, &mut failed); }
                self.refresh_continuation_release(&released, &mut failed);
                self.drop_failed(failed);
            }
            return; // A durable script cannot abandon its checkpoint via a client presentation message.
        }
        let SceneBoundaryKind::Complete(result) = &c.boundary.at else {
            self.continuation_bad_input(entity, run, "scene completion preceded its required windows or battles"); return;
        };
        let mut result = result.clone();
        if let Some(exit) = result.travel.as_mut() {
            let Ok(target) = self.ensure_area(&exit.target) else {
                self.continuation_failure(run, "required landing field could not be loaded"); return;
            };
            exit.target = target;
        }
        let action = if result.travel.is_some() { Action::Finish(result) } else { Action::Complete(result) };
        self.snapshot_continuation(run, action);
    }

    pub(super) fn continuation_retry(&mut self, entity: EntityId, run: SceneRunId, now: Instant) {
        if !self.scenes.runs.get(&run).is_some_and(|r| r.owner == entity) { return; }
        if !self.scenes.continuations.get(&run).is_some_and(|c| matches!(c.phase, Phase::GameOver { .. })) { return; }
        let Some(c) = self.scenes.continuations.get(&run) else { return; };
        let (Some(context), Some(plan)) = (c.context(run), c.payload.plan.clone()) else { return; };
        let _ = now;
        self.snapshot_continuation(run, Action::Prepare { plan, ordinal: context.ordinal });
    }

    fn continuation_bad_input(&mut self, entity: EntityId, run: SceneRunId, reason: &str) {
        self.logs.warn(format!("invalid continuation input run={} entity={} reason={reason}", run.0, entity.0));
        let mut failed = vec![];
        self.send_to(entity, ServerMsg::Reject { code: fmv_protocol::RejectCode::Internal,
            reason: "The scene transcript was refused. Reconnect to its saved checkpoint.".into() }, &mut failed);
        if let Some(player) = self.remove_player(entity, &mut failed) { player.close.send_replace(true); }
        self.drop_failed(failed);
    }

    fn continuation_failure(&mut self, run: SceneRunId, reason: &str) {
        self.logs.warn(format!("continuation refused run={} reason={reason}", run.0));
        // Once a group is reserved, any uncertain/mismatched state requires store recovery; do not
        // let a watcher leave and spend against an unacknowledged shared settlement.
        if self.scenes.continuations.get(&run).is_some_and(|c| c.checkpoint.is_some() || !matches!(c.phase, Phase::Playing)) {
            self.stop_for_scene_storage_recovery();
        } else if let Some(record) = self.scenes.runs.remove(&run) {
            self.scenes.continuations.remove(&run);
            self.fail_scene_save(run, record, reason);
        }
    }

    pub(super) fn advance_continuations(&mut self, now: Instant) {
        for (context, _) in self.take_scene_battle_dormant() {
            if let Some(c) = self.scenes.continuations.get_mut(&context.run).filter(|c| c.instance == context.instance) { c.phase = Phase::Dormant; }
        }
        for finished in self.take_scene_battle_finished() {
            let run = finished.context.run;
            let Some(c) = self.scenes.continuations.get_mut(&run).filter(|c| c.instance == finished.context.instance) else {
                self.stop_for_scene_storage_recovery(); continue;
            };
            if !matches!(c.phase, Phase::Fighting { id, ordinal } if id == finished.id && ordinal == finished.context.ordinal) {
                self.continuation_failure(run, "battle result did not match the active checkpoint"); continue;
            }
            // Even defeat is acknowledged durably: the next attempt retains all starting
            // resources and requires an explicit retry after either a reconnect or restart.
            self.queue_continuation_action(run, Action::Resolve(finished));
        }
        let runs = self.scenes.continuations.keys().copied().collect::<Vec<_>>();
        for run in runs {
            let Some(c) = self.scenes.continuations.get_mut(&run) else { continue; };
            enum Ready { Snapshot(Action, Result<Vec<SceneState>, SceneSaveError>), Commit(Action, Result<SceneBatchCommitted, SceneSaveError>) }
            let ready = match &mut c.phase {
                Phase::Snapshot { done, action } => match done.try_recv() {
                    Ok(value) => Some(Ready::Snapshot(action.clone(), value)),
                    Err(oneshot::error::TryRecvError::Empty) => None,
                    Err(_) => Some(Ready::Snapshot(action.clone(), Err(SceneSaveError::WorkerFailed))),
                },
                Phase::Commit { done, action } => match done.try_recv() {
                    Ok(value) => Some(Ready::Commit(action.clone(), value)),
                    Err(oneshot::error::TryRecvError::Empty) => None,
                    Err(_) => Some(Ready::Commit(action.clone(), Err(SceneSaveError::WorkerFailed))),
                },
                _ => None,
            };
            match ready {
                Some(Ready::Snapshot(action, Ok(states))) => {
                    if !self.continuation_snapshot_matches(run, &states) { self.continuation_failure(run, "the ordered snapshot disagreed with the held players"); continue; }
                    self.scenes.continuations.get_mut(&run).expect("present").states = states;
                    self.queue_continuation_action(run, action);
                }
                Some(Ready::Commit(action, Ok(saved))) => self.continuation_committed(run, action, saved, now),
                Some(_) => self.continuation_failure(run, "the durable continuation operation was not confirmed"),
                None => {
                    let ready = self.scenes.continuations.get(&run).is_some_and(|c| matches!(c.phase, Phase::Dormant)
                        && c.accounts.iter().any(|a| self.by_account.contains_key(a))
                        && c.accounts.iter().filter_map(|a| self.by_account.get(a))
                            .all(|e| self.players.get(e).is_some_and(|p| p.transit.is_none())));
                    if ready { self.launch_continuation_battle(run, now); }
                    // A follower's handshake or disconnect can be the last thing holding up
                    // a linked arrival. Only the owner's saved landing supplies durable input.
                    let landing_owner = self.scenes.continuations.get(&run).filter(|c| matches!(c.phase, Phase::Landing))
                        .and_then(|c| self.by_account.get(&c.owner)).copied();
                    if let Some(owner) = landing_owner { self.continuation_entered(owner, now); }
                }
            }
        }
    }

    fn continuation_snapshot_matches(&self, run: SceneRunId, states: &[SceneState]) -> bool {
        let Some(c) = self.scenes.continuations.get(&run) else { return false; };
        if states.len() != c.accounts.len() || states.iter().zip(&c.accounts).any(|(s,a)| s.pair.story.account_id != *a || s.pair.character.is_none()) { return false; }
        if !c.states.is_empty() { return states == c.states; }
        states.iter().all(|s| {
            if s.checkpoint.is_some() || s.reservation.is_some() { return false; }
            let Some(p) = c.frozen.iter().find(|p| p.account == s.pair.story.account_id) else { return false; };
            let Some(character) = s.pair.character.as_ref() else { return false; };
            p.story == s.pair.story.blob && p.progress == Some(crate::progress::Progress::from_record(character))
                && self.battle.as_ref().is_some_and(|b| p.inventory == Some(crate::inventory::Inventory::resolve(b.rules(), character.inventory.as_deref())))
        })
    }

    fn queue_continuation_action(&mut self, run: SceneRunId, action: Action) {
        let Some(c) = self.scenes.continuations.get(&run) else { return; };
        let Some(record) = self.scenes.runs.get(&run) else { return; };
        let built = self.continuation_job(c, record, &action);
        let Some(job) = built else { self.continuation_failure(run, "the continuation transition failed validation"); return; };
        self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
        match self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|s| s.save_scene_batch(job)) {
            Ok(done) => self.scenes.continuations.get_mut(&run).expect("present").phase = Phase::Commit { done, action },
            Err(_) => self.continuation_failure(run, "the continuation write could not be queued"),
        }
    }

    fn continuation_job(&self, c: &Continuation, record: &SceneRun, action: &Action) -> Option<SceneBatchJob> {
        let owner_index = c.states.iter().position(|s| s.pair.story.account_id == c.owner)?;
        let mut members = c.states.iter().map(|state| SceneBatchMember { expected: state.clone(), next_story: None,
            next_gil: None, checkpoint: state.checkpoint.clone(), reservation: state.reservation.clone(), battle: None }).collect::<Vec<_>>();
        let mut operation = SceneOperation { instance: c.instance, ordinal: c.consumed(), attempt: 0, kind: SceneOperationKind::Finish };
        match action {
            Action::Prepare { plan, ordinal } => {
                if *ordinal != c.consumed() + 1 || plan.setup.escape_policy != EscapePolicy::Forbidden { return None; }
                let mut payload = c.payload.clone();
                payload.plan = Some(plan.clone());
                payload.game_over = false;
                let attempt = c.checkpoint.as_ref().and_then(|cp| match cp.phase {
                    ScenePhase::BeforeBattle { ordinal: previous, attempt, .. } if previous + 1 == *ordinal => Some(attempt),
                    _ => None,
                }).unwrap_or(0);
                let checkpoint = c.checkpoint(record, ScenePhase::BeforeBattle { ordinal: ordinal - 1, attempt,
                    formation: plan.setup.formation_id, battle_seed: plan.setup.seed }, c.consumed(), &payload)?;
                operation.kind = SceneOperationKind::PrepareBattle;
                operation.ordinal = ordinal - 1;
                operation.attempt = attempt;
                let accounts = plan.accounts();
                for member in &mut members {
                    let slot = accounts.iter().position(|a| *a == member.expected.pair.story.account_id)?;
                    member.reservation = Some(SceneReservation { instance: c.instance, ordinal: ordinal - 1, attempt, slot: slot as u16 });
                }
                members[owner_index].checkpoint = Some(checkpoint);
                members[owner_index].next_story = Some(c.boundary.progress.clone());
                members[owner_index].next_gil = Some(c.boundary.gil);
            }
            Action::Resolve(finished) => {
                let context = c.context(finished.context.run)?;
                if context != finished.context { return None; }
                let won = finished.outcome == fmv_ff7_battle::rules::Outcome::Victory;
                let SceneBoundaryKind::Battle { identity, formation, ordinal, .. } = &c.boundary.at else { return None; };
                if *ordinal != context.ordinal || *formation != finished.plan.setup.formation_id { return None; }
                let rules = self.battle.as_ref()?.rules();
                let mut owner_gil = None;
                for member in &mut members {
                    let mut settlement = finished.settlements.iter().find(|(a,_)| *a == member.expected.pair.story.account_id)?.1.clone();
                    if !won {
                        settlement.consumed_items.clear();
                        settlement.reward = fmv_protocol::BattleReward::new(member.expected.pair.story.account_id, 0, 0, 0);
                        settlement.final_limit = None;
                    }
                    let after = crate::saver::preview_scene_settlement(member.expected.pair.character.as_ref()?, rules, &settlement).ok()?;
                    if after.account_id == c.owner { owner_gil = Some(after.gil); }
                    member.battle = Some(settlement);
                }
                let mut payload = c.payload.clone();
                if won {
                    payload.carry = Some(finished.carry.clone());
                    payload.game_over = false;
                    payload.replay.events.push(SceneReplayEvent::Battle { ordinal: *ordinal, identity: identity.clone(),
                        formation: *formation, progress: c.boundary.progress.clone(), gil: owner_gil? });
                    let next = self.scenes.host.as_ref()?.evaluate_continuation(&record.scene, &record.progress, record.seed,
                        record.gil, Some(&payload.replay))?;
                    if next.source != c.boundary.source { return None; }
                    members[owner_index].checkpoint = Some(c.checkpoint(record, ScenePhase::Resume { segment: *ordinal }, *ordinal, &payload)?);
                } else {
                    payload.game_over = true;
                    let attempt = context.attempt.checked_add(1)?;
                    members[owner_index].checkpoint = Some(c.checkpoint(record, ScenePhase::BeforeBattle {
                        ordinal: ordinal - 1, attempt, formation: *formation, battle_seed: finished.plan.setup.seed,
                    }, c.consumed(), &payload)?);
                    for member in &mut members { member.reservation.as_mut()?.attempt = attempt; }
                }
                operation.kind = SceneOperationKind::ResolveBattle;
                operation.ordinal = ordinal - 1;
                operation.attempt = context.attempt;
            }
            Action::Finish(result) => {
                let exit = result.travel.as_ref()?;
                let landing = Landing::from_exit(exit);
                if !self.valid_continuation_destination(&landing) { return None; }
                let mut payload = c.payload.clone();
                payload.plan = None;
                payload.carry = None;
                payload.landing = Some(landing.clone());
                let destination = serde_json::to_vec(&landing).ok()?;
                members[owner_index].checkpoint = Some(c.checkpoint(record, ScenePhase::Landing { destination }, c.consumed(), &payload)?);
                members[owner_index].next_story = Some(result.progress.clone());
                members[owner_index].next_gil = Some(c.boundary.gil);
                for member in &mut members { member.reservation = None; }
            }
            Action::Continue { record: next_record, instance, boundary, payload } => {
                if members.len() != 1 || !matches!(c.checkpoint.as_ref()?.phase, ScenePhase::Landing { .. }) { return None; }
                let encoded = serde_json::to_vec(payload).ok()?;
                members[owner_index].checkpoint = Some(SceneCheckpoint { instance: *instance, scene_key: next_record.scene.clone(),
                    source_hash: boundary.source, replay_version: REPLAY_VERSION, original_story: next_record.progress.clone(),
                    original_gil: next_record.gil, seed: next_record.seed, replay: encoded,
                    phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0 });
                members[owner_index].reservation = None;
                operation.kind = SceneOperationKind::ContinueLanding;
            }
            Action::Complete(result) => {
                if members.len() != 1 || c.checkpoint.is_none() || result.travel.is_some() { return None; }
                let landing = c.payload.landing.as_ref()?;
                let host = self.scenes.host.as_ref()?;
                let entry = Landing::from_exit(&host.entry(&result.progress)?);
                // The arrival script may walk the lead away from its MAPJUMP landing before
                // returning control. The final story must resolve to a valid control position
                // in this same field; another field requires an explicit saved travel instead.
                if entry.field != landing.field || entry.field != c.payload.field
                    || host.offered(&record.scene, &result.progress) || !self.valid_continuation_destination(&entry) { return None; }
                members[owner_index].checkpoint = None;
                members[owner_index].reservation = None;
                members[owner_index].next_story = Some(result.progress.clone());
                members[owner_index].next_gil = Some(c.boundary.gil);
                operation.kind = SceneOperationKind::Complete;
            }
        }
        if members.iter().filter_map(|member| member.checkpoint.as_ref()).any(|checkpoint| !checkpoint_can_be_announced(checkpoint)) {
            return None;
        }
        Some(SceneBatchJob { operation, members })
    }

    fn valid_continuation_destination(&self, landing: &Landing) -> bool {
        landing.position.is_finite() && landing.facing.is_none_or(f32::is_finite)
            && self.areas.get(&landing.field).is_some_and(|a| a.runtime().place_arrival(landing.position, landing.hint).is_finite())
    }

    fn continuation_committed(&mut self, run: SceneRunId, action: Action, saved: SceneBatchCommitted, now: Instant) {
        self.scenes.save_epoch = self.scenes.save_epoch.wrapping_add(1);
        let Some(mut c) = self.scenes.continuations.remove(&run) else { return; };
        // A repeated worker answer must not re-publish a past fight after a later transition.
        let expected = self.scenes.runs.get(&run).and_then(|r| self.continuation_job(&c, r, &action));
        let valid = expected.as_ref().is_some_and(|job| saved.current.len() == job.members.len()
            && saved.current.iter().zip(&job.members).all(|(state, member)| state.pair.story.account_id == member.expected.pair.story.account_id
                && state.checkpoint == member.checkpoint && state.reservation == member.reservation
                && state.pair.story.game == member.expected.pair.story.game
                && state.pair.story.blob == member.next_story.as_ref().unwrap_or(&member.expected.pair.story.blob).as_slice()
                && self.continuation_character_matches(state, member)));
        if !valid { self.scenes.continuations.insert(run,c); self.continuation_failure(run,"saver acknowledgment did not match the selected checkpoint"); return; }
        c.states = saved.current;
        c.checkpoint = c.states.iter().find(|s| s.pair.story.account_id == c.owner).and_then(|s| s.checkpoint.clone());
        if let Some(checkpoint) = &c.checkpoint {
            let Ok(payload) = serde_json::from_slice::<Payload>(&checkpoint.replay) else {
                self.scenes.continuations.insert(run,c); self.continuation_failure(run,"saved replay could not be decoded"); return;
            };
            c.payload = payload;
        }
        let states = c.states.clone();
        self.scenes.continuations.insert(run,c);
        self.mirror_scene_states(states);
        match action {
            Action::Prepare { .. } => self.launch_continuation_battle(run, now),
            Action::Resolve(finished) => {
                self.acknowledge_scene_battle_finished(&finished);
                if finished.outcome != fmv_ff7_battle::rules::Outcome::Victory {
                    self.scenes.continuations.get_mut(&run).expect("present").phase = Phase::GameOver { id: finished.id };
                    self.continuation_fan(run, ServerMsg::SceneGameOver { run, ordinal: finished.context.ordinal, battle_id: finished.id });
                    return;
                }
                let c = self.scenes.continuations.get_mut(&run).expect("present");
                let record = self.scenes.runs.get(&run).expect("present");
                let boundary = self.scenes.host.as_ref().and_then(|h| h.evaluate_continuation(&record.scene, &record.progress,
                    record.seed,record.gil,Some(&c.payload.replay)));
                let Some(boundary) = boundary else { self.continuation_failure(run,"saved victory could not be resumed"); return; };
                c.boundary = boundary;
                c.phase = Phase::Playing;
                let owner = c.states.iter().find(|s| s.pair.story.account_id == c.owner).expect("validated owner");
                let progress = owner.pair.story.blob.clone();
                let gil = owner.pair.character.as_ref().expect("validated character").gil;
                self.continuation_fan(run, ServerMsg::SceneBattleResumed { run, ordinal: finished.context.ordinal,
                    battle_id: finished.id, progress, gil });
            }
            Action::Finish(result) => {
                let mut record = self.scenes.runs.get(&run).cloned().expect("present");
                record.watchers.retain(|entity| self.players.get(entity).is_some_and(|p| p.scene == Some(run)));
                self.scenes.runs.get_mut(&run).expect("present").watchers = record.watchers.clone();
                let c = self.scenes.continuations.get_mut(&run).expect("present");
                c.accounts = vec![c.owner];
                c.states.retain(|s| s.pair.story.account_id == c.owner);
                c.phase = Phase::Landing;
                let mut failed = vec![];
                // Guests' durable reservations end here, but connected followers remain held
                // through the linked arrival. Their own story must not start an independent
                // automatic scene between the old SceneEnded and the owner's next SceneStart.
                for entity in std::iter::once(record.owner).chain(record.watchers.iter().copied()) {
                    self.send_to(entity, ServerMsg::SceneEnded { run, outcome: SceneOutcome::Finished }, &mut failed);
                }
                self.drop_failed(failed);
                if let Some(exit) = result.travel {
                    for entity in std::iter::once(record.owner).chain(record.watchers) {
                        if let Some((pos,facing)) = self.players.get(&entity).map(|p|(p.snapshot.pos,p.snapshot.facing)) {
                            if !self.start_travel(entity,&exit,pos,facing,now) { self.continuation_failure(run,"the acknowledged landing could not be entered"); return; }
                        }
                    }
                }
            }
            Action::Continue { mut record, instance, boundary, payload } => {
                // A disconnect while ContinueLanding was queued cannot put an expired entity
                // back into the arrival's presentation cohort when its acknowledgment arrives.
                record.watchers.retain(|entity| self.players.get(entity).is_some_and(|p| p.scene == Some(run)));
                let c = self.scenes.continuations.get_mut(&run).expect("present");
                c.instance = instance;
                c.boundary = boundary;
                c.payload = *payload;
                c.phase = Phase::Playing;
                self.scenes.runs.insert(run, record);
                self.announce_continuation(run, None);
            }
            Action::Complete(result) => {
                let Some(entry) = self.scenes.host.as_ref().and_then(|host| host.entry(&result.progress)) else {
                    self.continuation_failure(run,"the completed arrival has no control position"); return;
                };
                let Some(record) = self.scenes.runs.get(&run) else { return; };
                let everyone = std::iter::once(record.owner).chain(record.watchers.iter().copied())
                    .filter(|entity| self.players.get(entity).is_some_and(|p| p.scene == Some(run))).collect::<Vec<_>>();
                for &entity in &everyone {
                    if !self.place_continuation_control(entity, &entry, now) {
                        self.continuation_failure(run,"the completed arrival control position could not be placed"); return;
                    }
                }
                self.scenes.continuations.remove(&run);
                self.scenes.runs.remove(&run);
                let mut failed = vec![];
                // Replace the held view with the final phase and authoritative pose before
                // SceneEnded reveals the player's own character and permits local movement.
                let accounts = everyone.iter().filter_map(|entity| self.players.get(entity).map(|p| p.account)).collect();
                self.refresh_story_placements(&accounts, &mut failed);
                for entity in everyone { self.release(entity,run,SceneOutcome::Finished,&mut failed); }
                self.drop_failed(failed);
            }
        }
    }

    /// Apply a durably completed arrival's own control position without running entry scripts
    /// again. A disconnected owner recovers the same position from the committed story on login.
    fn place_continuation_control(&mut self, entity: EntityId, entry: &Exit, now: Instant) -> bool {
        let Some(player) = self.players.get(&entity) else { return true; };
        if player.area != entry.target || player.transit.is_some() { return false; }
        let Some(area) = self.areas.get(&entry.target) else { return false; };
        let position = area.runtime().place_arrival(entry.arrival, entry.arrival_hint);
        if !position.is_finite() || entry.arrival_facing.is_some_and(|facing| !facing.is_finite()) { return false; }
        let budget = super::super::MoveBudget::full(now, area.rules());
        let player = self.players.get_mut(&entity).expect("checked player");
        player.snapshot.pos = position;
        if let Some(facing) = entry.arrival_facing { player.snapshot.facing = super::normalize_facing(facing); }
        player.snapshot.anim = fmv_core::AnimState::Idle;
        player.budget = budget;
        player.walked = 0.0;
        player.dirty = true;
        self.place_links(entity);
        true
    }

    fn refresh_continuation_release(&mut self, entities: &[EntityId], failed: &mut Vec<(EntityId, bool)>) {
        let accounts = entities.iter().filter_map(|e| self.players.get(e).map(|p| p.account)).collect();
        self.refresh_placements(&accounts, failed);
    }

    fn continuation_character_matches(&self, state: &SceneState, member: &SceneBatchMember) -> bool {
        let (Some(before), Some(actual)) = (&member.expected.pair.character, &state.pair.character) else { return false; };
        let expected = if let Some(settlement) = &member.battle {
            self.battle.as_ref().and_then(|b| crate::saver::preview_scene_settlement(before, b.rules(), settlement).ok())
        } else { Some(before.clone()) };
        let Some(mut expected) = expected else { return false; };
        if let Some(gil) = member.next_gil { expected.gil = gil; }
        let mut actual = actual.clone();
        expected.updated_at = 0;
        actual.updated_at = 0;
        expected.schema_version = 0;
        actual.schema_version = 0;
        expected == actual
    }

    fn mirror_scene_states(&mut self, states: Vec<SceneState>) {
        let mut failed = vec![];
        for state in states {
            let Some(entity) = self.by_account.get(&state.pair.story.account_id).copied() else { continue; };
            let Some(record) = state.pair.character else { continue; };
            let inventory = self.battle.as_ref().map(|b|crate::inventory::Inventory::resolve(b.rules(),record.inventory.as_deref()));
            let Some(player) = self.players.get_mut(&entity) else { continue; };
            let character = player.progress.map_or(record.character,|p|p.character);
            let mut progress = crate::progress::Progress::from_record(&record);
            progress.character = character;
            player.progress = Some(progress);
            player.inventory = inventory;
            // No queue: this is the already acknowledged image, also used to roll a failed attempt
            // back to its durable checkpoint before an explicit retry.
            self.apply_saved_story_step(entity,SCENE_STEP_POINT,fmv_adapter_api::StoryStep {
                progress:state.pair.story.blob,summary:"saved scene checkpoint".into(),countdown:None });
            self.tell_progress(entity,false,&mut failed);
            self.tell_inventory(entity,&mut failed);
        }
        self.drop_failed(failed);
    }

    fn launch_continuation_battle(&mut self, run: SceneRunId, now: Instant) {
        let Some(c) = self.scenes.continuations.get(&run) else { return; };
        let Some(context) = c.context(run) else { self.continuation_failure(run,"prepared battle checkpoint missing"); return; };
        let Some(plan) = c.payload.plan.clone() else { self.continuation_failure(run,"prepared battle setup missing"); return; };
        if !self.validate_scene_battle_plan(&plan, &c.states) {
            self.continuation_failure(run, "the saved battle plan disagreed with its coherent roster"); return;
        }
        if !c.accounts.iter().any(|a| self.by_account.contains_key(a))
            || c.accounts.iter().filter_map(|a| self.by_account.get(a)).any(|e| self.players.get(e).is_some_and(|p| p.transit.is_some())) {
            self.scenes.continuations.get_mut(&run).expect("present").phase = Phase::Dormant;
            return;
        }
        // The battle helper emits Started immediately before EncounterStart to every connected
        // member so the client's scene binds this battle before the engine begins.
        match self.start_scene_battle(context, plan, now) {
            Ok(id) => self.scenes.continuations.get_mut(&run).expect("present").phase = Phase::Fighting { id,ordinal:context.ordinal },
            Err(_) => self.continuation_failure(run,"prepared battle could not be reconstructed"),
        }
    }

    fn continuation_fan(&mut self,run:SceneRunId,msg:ServerMsg) {
        let Some(record)=self.scenes.runs.get(&run) else{return;};
        let recipients=std::iter::once(record.owner).chain(record.watchers.iter().copied()).collect::<Vec<_>>();
        let mut failed=vec![];
        for entity in recipients { if self.players.get(&entity).is_some_and(|p|p.scene==Some(run)) { self.send_to(entity,msg.clone(),&mut failed); } }
        self.drop_failed(failed);
    }

    fn announce_continuation(&mut self, run:SceneRunId, only:Option<EntityId>) {
        let (Some(c),Some(record))=(self.scenes.continuations.get(&run),self.scenes.runs.get(&run)) else{return;};
        let participants=std::iter::once(record.owner).chain(record.watchers.iter().copied()).enumerate()
            .filter(|(_, entity)| self.players.get(entity).is_some_and(|p| p.scene == Some(run)
                && p.transit.is_none() && p.area.eq_ignore_ascii_case(&c.payload.field)))
            .map(|(spot,entity)|SceneParticipant{entity,spot:spot as u8}).collect::<Vec<_>>();
        let mut failed=vec![];
        for p in &participants {
            if only.is_some_and(|e|e!=p.entity) {continue;}
            self.send_to(p.entity,ServerMsg::SceneStart{run,scene:record.scene.clone(),seed:record.seed,
                progress:record.progress.clone(),gil:record.gil,replay:Some(c.payload.replay.clone()),owner:record.owner,
                participants:participants.clone(),you:if p.entity==record.owner{SceneRole::Owner}else{SceneRole::Watcher}},&mut failed);
        }
        self.drop_failed(failed);
    }

    /// Register a coherent authenticated group before any entry scripts or gameplay may run.
    /// Replaying the source is mandatory even when another session already owns the live group.
    pub(in crate::world) fn restore_continuation(&mut self, entity: EntityId, group: Vec<SceneState>, now: Instant)
        -> Result<(), super::super::JoinError> {
        use super::super::JoinError;
        let Some(owner_state) = group.iter().find(|s| s.checkpoint.is_some()) else {
            return if group.iter().any(|s|s.reservation.is_some()) { Err(JoinError::StorageUnavailable) } else { Ok(()) };
        };
        let checkpoint = owner_state.checkpoint.clone().ok_or(JoinError::StorageUnavailable)?;
        let owner = owner_state.pair.story.account_id;
        let player = self.players.get(&entity).ok_or(JoinError::StorageUnavailable)?;
        // ContinueLanding changes the durable instance before its acknowledgment reaches this
        // process. Fence by reserved account as well as instance so that new id cannot create a
        // second runtime while the old runtime is still committing the handoff.
        if self.scenes.continuations.values().any(|c| c.accounts.contains(&player.account)
            && (matches!(c.phase, Phase::Snapshot { .. } | Phase::Commit { .. })
                // Terminal combat and last-player disconnect queue a handoff for the scene
                // tick. Admission must not try to attach to the engine removed in that gap.
                || (matches!(c.phase, Phase::Fighting { .. }) && !self.account_in_battle(player.account)))) {
            return Err(JoinError::ProgressChanged);
        }
        if !group.iter().any(|s|s.pair.story.account_id==player.account) { return Err(JoinError::StorageUnavailable); }
        let host = self.scenes.host.as_ref().ok_or(JoinError::StorageUnavailable)?;
        if checkpoint.replay_version != REPLAY_VERSION || !host.continuation(&checkpoint.scene_key)
            || !checkpoint_can_be_announced(&checkpoint) {
            return Err(JoinError::StorageUnavailable);
        }
        let payload: Payload = serde_json::from_slice(&checkpoint.replay).map_err(|_|JoinError::StorageUnavailable)?;
        if payload.replay.source != checkpoint.source_hash || !payload.position.is_finite() || !payload.facing.is_finite()
            || !self.scenes.list.iter().any(|info| info.id == checkpoint.scene_key && info.field.eq_ignore_ascii_case(&payload.field))
            || payload.field.is_empty() || payload.field.len()>128 { return Err(JoinError::StorageUnavailable); }
        let boundary = host.evaluate_continuation(&checkpoint.scene_key,&checkpoint.original_story,checkpoint.seed,
            checkpoint.original_gil,Some(&payload.replay)).ok_or(JoinError::StorageUnavailable)?;
        if boundary.source != checkpoint.source_hash { return Err(JoinError::StorageUnavailable); }
        let valid = match &checkpoint.phase {
            ScenePhase::BeforeBattle{ordinal,formation,battle_seed,..} => {
                matches!(&boundary.at,SceneBoundaryKind::Battle{ordinal: actual,formation:actual_formation,flags:Some(8),..}
                    if *actual==ordinal+1 && actual_formation==formation)
                    && boundary.progress==owner_state.pair.story.blob
                    && owner_state.pair.character.as_ref().is_some_and(|r|r.gil==boundary.gil)
                    && payload.plan.as_ref().is_some_and(|p|p.setup.formation_id==*formation && p.setup.seed==*battle_seed
                        && p.setup.escape_policy==EscapePolicy::Forbidden && self.validate_scene_battle_plan(p, &group)
                        && p.accounts().len()==group.len() && p.accounts().iter().all(|a|group.iter().any(|s|s.pair.story.account_id==*a)))
            }
            ScenePhase::Resume{segment} => {
                let cohort = if checkpoint.consumed_battles == 0 {
                    payload.plan.is_none() && payload.carry.is_none() && group.len() == 1
                        && group[0].reservation.is_none() && host.automatic(&checkpoint.scene_key)
                        && payload.landing.as_ref().is_some_and(|l| l.field.eq_ignore_ascii_case(&payload.field))
                } else {
                    match (&payload.plan, &payload.carry) {
                        (Some(plan), Some(carry)) => self.validate_scene_battle_resume(plan, carry, &group),
                        _ => false,
                    }
                };
                !payload.game_over && cohort && *segment==checkpoint.consumed_battles && match payload.replay.events.iter().rev().find_map(|e|match e {
                    SceneReplayEvent::Battle{ordinal,progress,gil,..}=>Some((*ordinal,progress,*gil)),_=>None}) {
                    Some((ordinal,progress,gil)) => ordinal==checkpoint.consumed_battles && *progress==owner_state.pair.story.blob
                        && owner_state.pair.character.as_ref().is_some_and(|r|r.gil==gil),
                    None=>checkpoint.consumed_battles==0 && checkpoint.original_story==owner_state.pair.story.blob
                        && owner_state.pair.character.as_ref().is_some_and(|r|r.gil==checkpoint.original_gil),
                }
            }
            ScenePhase::Landing{destination} => {
                !payload.game_over && payload.plan.is_none() && payload.carry.is_none() && group.len()==1 && group[0].reservation.is_none()
                    && payload.landing.as_ref().is_some_and(|l|serde_json::from_slice::<Landing>(destination).ok().as_ref()==Some(l))
                    && matches!(&boundary.at,SceneBoundaryKind::Complete(result) if result.progress==owner_state.pair.story.blob
                        && result.travel.as_ref().map(Landing::from_exit)==payload.landing)
                    && owner_state.pair.character.as_ref().is_some_and(|r|r.gil==boundary.gil)
            }
        };
        if !valid { return Err(JoinError::StorageUnavailable); }
        // The configured entry field may be this checkpoint's field. Restore the saved position
        // before its first snapshot instead of treating a matching field name as a placed player.
        let desired = if matches!(checkpoint.phase, ScenePhase::Landing { .. }) { payload.landing.clone() }
            else { Some(Landing { field: payload.field.clone(), position: payload.position, facing: Some(payload.facing),
                hint: payload.replay.origin.as_ref().and_then(|origin| origin.triangle) }) };
        let placement = desired.as_ref().filter(|d| d.field.eq_ignore_ascii_case(&player.area)).map(|d| {
            let area = self.areas.get(&player.area).ok_or(JoinError::StorageUnavailable)?;
            let pos = area.runtime().place_arrival(d.position, d.hint);
            if !pos.is_finite() { return Err(JoinError::StorageUnavailable); }
            Ok((pos, d.facing.unwrap_or(player.snapshot.facing)))
        }).transpose()?;
        let existing=self.scenes.continuations.iter().find(|(_,c)|c.instance==checkpoint.instance).map(|(id,_)|*id);
        let run=if let Some(run)=existing {
            let c=self.scenes.continuations.get(&run).ok_or(JoinError::StorageUnavailable)?;
            // A pending acknowledgment can change both the prefix and combat binding. Retry
            // admission after it resolves rather than publish a player without its SceneStart.
            if matches!(c.phase, Phase::Snapshot { .. } | Phase::Commit { .. }) { return Err(JoinError::ProgressChanged); }
            if c.owner!=owner || c.states.len()!=group.len() || c.states.iter().any(|s|!group.contains(s)) {
                return Err(JoinError::ProgressChanged);
            }
            run
        } else {
            let run=SceneRunId(self.scenes.next_run.max(1));
            self.scenes.next_run=run.0.saturating_add(1);
            let owner_entity=self.by_account.get(&owner).copied().unwrap_or(EntityId(0));
            let accounts=group.iter().map(|s|s.pair.story.account_id).collect::<Vec<_>>();
            let watchers=accounts.iter().filter(|a|**a!=owner).filter_map(|a|self.by_account.get(a).copied()).collect();
            let steps=payload.replay.events.iter().filter(|e|matches!(e,SceneReplayEvent::Window{..})).count() as u32;
            let phase=match checkpoint.phase {ScenePhase::BeforeBattle{..} if payload.game_over => Phase::GameOver { id: BattleId(0) },
                ScenePhase::BeforeBattle{..}=>Phase::Dormant,ScenePhase::Resume{..}=>Phase::Playing,ScenePhase::Landing{..}=>Phase::Landing};
            self.scenes.runs.insert(run,SceneRun{scene:checkpoint.scene_key.clone(),owner:owner_entity,watchers,
                seed:checkpoint.seed,progress:checkpoint.original_story.clone(),gil:checkpoint.original_gil,answers:vec![],steps,
                started:now,last_activity:now,writes_story:true,preview:false,then:None});
            self.scenes.continuations.insert(run,Continuation{owner,instance:checkpoint.instance,accounts,payload,boundary,
                checkpoint:Some(checkpoint),states:group,frozen:vec![],phase});
            run
        };
        let player=self.players.get_mut(&entity).ok_or(JoinError::StorageUnavailable)?;
        player.scene=Some(run);
        if let Some((pos, facing)) = placement {
            player.snapshot.pos = pos;
            player.snapshot.facing = super::super::normalize_facing(facing);
        }
        let record=self.scenes.runs.get_mut(&run).ok_or(JoinError::StorageUnavailable)?;
        if player.account==owner {record.owner=entity;} else if !record.watchers.contains(&entity) {record.watchers.push(entity);}
        Ok(())
    }

    pub(super) fn continuation_player_gone(&mut self, entity: EntityId, run: SceneRunId) {
        let abandon = self.scenes.continuations.get(&run).is_some_and(|c| c.checkpoint.is_none() && matches!(c.phase, Phase::Playing))
            && self.scenes.runs.get(&run).is_some_and(|r| r.owner == entity);
        if abandon { self.continuation_end(entity, run, SceneOutcome::Aborted, Instant::now()); }
        else if let Some(record) = self.scenes.runs.get_mut(&run) { record.watchers.retain(|e| *e != entity); }
    }

    pub(in crate::world) fn continuation_account_held(&self, account: AccountId) -> bool {
        self.scenes.continuations.values().any(|c| c.accounts.contains(&account))
            || self.by_account.get(&account).is_some_and(|entity| self.continuation_held(*entity))
    }

    pub(in crate::world) fn continuation_held(&self,entity:EntityId)->bool {
        self.players.get(&entity).and_then(|p|p.scene).is_some_and(|run|self.scenes.continuations.contains_key(&run))
    }

    pub(in crate::world) fn continuation_failed_entry(&mut self,entity:EntityId) {
        if let Some(run)=self.players.get(&entity).and_then(|p|p.scene) {
            self.continuation_bad_input(entity, run, "the client could not enter its required continuation field");
        }
    }

    /// Called after the area's snapshot and placement, before ordinary entry scripts. It can
    /// redirect through the normal area handshake, replay a held source, or atomically begin its required arrival.
    pub(in crate::world) fn continuation_entered(&mut self,entity:EntityId,now:Instant)->bool {
        let Some((run,area,pos,facing,transit))=self.players.get(&entity).and_then(|p|p.scene.map(|run|
            (run,p.area.clone(),p.snapshot.pos,p.snapshot.facing,p.transit.is_some()))) else {return false;};
        let Some(c)=self.scenes.continuations.get(&run) else {return false;};
        if transit || matches!(c.phase, Phase::Snapshot { .. } | Phase::Commit { .. }) { return true; }
        let landing=matches!(c.phase,Phase::Landing);
        let destination=if landing {c.payload.landing.clone()} else {
            Some(Landing{field:c.payload.field.clone(),position:c.payload.position,facing:Some(c.payload.facing),hint:None})
        };
        let Some(destination)=destination else {self.continuation_failure(run,"required continuation placement missing");return true;};
        if !destination.field.eq_ignore_ascii_case(&area) {
            if self.ensure_area(&destination.field).is_err() || !self.start_travel(entity,&destination.exit(),pos,facing,now) {
                self.continuation_failure(run,"required continuation field unavailable");
            }
            return true;
        }
        if landing {
            let Some(previous) = self.scenes.runs.get(&run) else { return true; };
            let Some(owner) = self.players.get(&previous.owner).filter(|p| p.account == c.owner
                && p.scene == Some(run) && p.transit.is_none() && p.area.eq_ignore_ascii_case(&destination.field)) else { return true; };
            let watchers = previous.watchers.iter().copied().filter(|entity|
                self.players.get(entity).is_some_and(|p| p.scene == Some(run))).collect::<Vec<_>>();
            // A linked arrival is a single presentation for the online cohort. Wait for every
            // retained follower's destination handshake; disconnected followers no longer block it.
            if watchers.iter().any(|entity| self.players.get(entity).is_some_and(|p|
                p.transit.is_some() || !p.area.eq_ignore_ascii_case(&destination.field))) { return true; }
            let owner_entity = previous.owner;
            let (area, pos, facing) = (owner.area.clone(), owner.snapshot.pos, owner.snapshot.facing);
            let Some(owner_state)=c.states.iter().find(|s|s.pair.story.account_id==c.owner) else{return true;};
            let Some(host)=self.scenes.host.as_ref() else{return true;};
            let Some(info)=self.scenes.list.iter().find(|info|info.field.eq_ignore_ascii_case(&area)
                && host.automatic(&info.id) && host.continuation(&info.id) && host.offered(&info.id,&owner_state.pair.story.blob)).cloned() else {
                self.continuation_failure(run,"required landing arrival scene is unavailable");return true;
            };
            let progress=owner_state.pair.story.blob.clone();
            let gil=owner_state.pair.character.as_ref().map_or(0,|r|r.gil);
            let seed=run.0.wrapping_mul(0x85EB_CA6B) as u32;
            let Some(boundary)=host.evaluate_continuation(&info.id,&progress,seed,gil,None) else {
                self.continuation_failure(run,"required arrival could not be interpreted");return true;
            };
            let Some(instance)=fresh_instance() else {self.continuation_failure(run,"continuation instance could not be allocated");return true;};
            let payload=Payload{replay:SceneReplay{source:boundary.source,origin:None,events:vec![]},field:area,position:pos,facing,
                plan:None,carry:None,landing:Some(destination),game_over:false};
            let record=SceneRun{scene:info.id,owner:owner_entity,watchers,seed,progress,gil,answers:vec![],steps:0,
                started:now,last_activity:now,writes_story:true,preview:false,then:None};
            self.queue_continuation_action(run,Action::Continue{record,instance,boundary,payload:Box::new(payload)});
            return true;
        }
        let (fighting,dormant,game_over,context)=match &c.phase {
            Phase::Fighting{id,..}=>(Some(*id),false,false,c.context(run)),
            Phase::Dormant=>(None,true,false,c.context(run)),
            Phase::GameOver{id}=>(Some(*id),false,true,c.context(run)),
            Phase::Playing=>(None,false,false,None),
            _=>return true,
        };
        // A reconnected owner has a new entity id. Retained arrival watchers must receive the
        // same corrected participant list, otherwise they show that new body beside the cast.
        // Only a noncombat linked arrival is rebuilt here; active battle mirrors stay bound.
        let refresh_arrival = matches!(c.phase, Phase::Playing) && c.payload.landing.is_some()
            && self.scenes.runs.get(&run).is_some_and(|record| record.owner == entity);
        self.announce_continuation(run,if refresh_arrival { None } else { Some(entity) });
        if fighting.is_some() && !game_over {
            if context.is_none_or(|context|self.reattach_scene_battle(entity,context).is_err()) {
                self.continuation_failure(run,"reserved player could not reattach to the active fight");
            }
        } else if dormant {self.launch_continuation_battle(run,now);}
        else if game_over {
            // Restart is still explicit; there is no active engine whose outcome could be replayed.
            if let (Some(context), Some(id))=(context,fighting) {
                self.continuation_fan(run,ServerMsg::SceneBattleStarted{run,ordinal:context.ordinal,battle_id:id});
                self.continuation_fan(run,ServerMsg::SceneGameOver{run,ordinal:context.ordinal,battle_id:id});
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
