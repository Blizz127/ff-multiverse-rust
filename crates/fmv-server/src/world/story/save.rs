//! Ordered exact story writes. Storage confirmation and the client's ordered input barrier
//! must both arrive before the world publishes a step or resumes its dependent work.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use fmv_adapter_api::{AreaKey, StoryStep};
use fmv_core::{AccountId, AnimState, EntityId, Pos};
use fmv_protocol::{SceneRunId, ServerMsg};
use tokio::sync::oneshot;

use crate::saver::{SceneSaveError, SceneSaved, StoryCasJob};
use super::super::scene::SceneReply;
use super::super::{is_full, World};

/// Work suspended by a story write. A sweep stores the unvisited point ids rather than
/// starting over: numeric assignments and clears may make an earlier point applicable again.
#[derive(Debug, Clone)]
pub(in crate::world) enum StoryFollowup {
    Move { area: AreaKey, from: Pos, to: Pos, distance: f64 },
    Scene { run: SceneRunId },
    Automatic { area: AreaKey },
    Sweep { area: AreaKey, points: Vec<u32>, on_entry: bool },
    /// Entry work deliberately skipped while an acknowledged transfer still held its token.
    Entered,
    Victory { before: Vec<u8>, step: StoryStep, formation: u16 },
}

#[derive(Default)]
pub(in crate::world) struct StorySaves {
    next_id: u64,
    pending: BTreeMap<AccountId, Pending>,
    resuming: Option<Resuming>,
    scenes: BTreeMap<SceneRunId, VecDeque<StoryFollowup>>,
    battles: BTreeMap<AccountId, VecDeque<StoryFollowup>>,
}

#[derive(Clone, Copy)]
struct Resuming { account: AccountId, entity: EntityId, id: u64 }

struct Pending {
    id: u64,
    entity: EntityId,
    area: AreaKey,
    expected: Vec<u8>,
    point: u32,
    step: StoryStep,
    client_ack: bool,
    phase: Phase,
    followups: VecDeque<StoryFollowup>,
}

enum Phase {
    Saving(oneshot::Receiver<Result<SceneSaved, SceneSaveError>>),
    Saved(Result<SceneSaved, SceneSaveError>),
    /// The write was published, but its same token still covers the area handshake it caused.
    Published,
}

impl World {
    /// Tokens are shared with ordinary story battles and never wrap into an old barrier.
    pub(in crate::world) fn new_story_pending_id(&mut self) -> Option<u64> {
        let id = self.story_saves.next_id.checked_add(1)?;
        self.story_saves.next_id = id;
        Some(id)
    }

    pub(in crate::world) fn ordinary_story_account_held(&self, account: AccountId) -> bool {
        self.story_saves.pending.contains_key(&account)
            || self.story_saves.resuming.is_some_and(|resuming| resuming.account == account)
    }

    /// A disconnected writer still needs its source field until the selected step is settled.
    pub(in crate::world) fn ordinary_story_destinations(&self) -> Vec<AreaKey> {
        self.story_saves.pending.values().map(|pending| pending.area.clone()).collect()
    }

    pub(in crate::world) fn ordinary_story_held(&self, entity: EntityId) -> bool {
        self.players.get(&entity).is_some_and(|player| self.story_saves.pending.contains_key(&player.account))
    }

    /// Placement refreshes caused by the acknowledged publication itself are allowed. Party
    /// cleanup while an asynchronous write is pending must still leave its presentation alone.
    pub(in crate::world) fn ordinary_story_placement_held(&self, account: AccountId) -> bool {
        self.story_saves.pending.contains_key(&account)
    }

    pub(in crate::world) fn ordinary_story_resuming(&self, entity: EntityId) -> bool {
        self.story_saves.resuming.is_some_and(|resuming| resuming.entity == entity)
    }

    pub(in crate::world) fn ordinary_story_barrier_acked(&self, entity: EntityId, id: u64) -> bool {
        (id != 0 && self.story_saves.resuming.is_some_and(|r| r.entity == entity && r.id == id))
            || self.players.get(&entity).and_then(|p| self.story_saves.pending.get(&p.account))
                .is_some_and(|pending| pending.entity == entity && pending.id == id && pending.client_ack)
    }

    pub(crate) fn story_pending_ack(&mut self, entity: EntityId, id: u64, now: Instant) -> SceneReply {
        if !self.players.contains_key(&entity) { return SceneReply::Gone; }
        if self.ordinary_battle_pending_ack(entity, id, now) { return SceneReply::Done; }
        let account = self.players[&entity].account;
        if let Some(pending) = self.story_saves.pending.get_mut(&account)
            .filter(|pending| pending.entity == entity && pending.id == id) {
            pending.client_ack = true;
        }
        self.advance_story_saves(now);
        SceneReply::Done
    }

    /// The caller has already selected an exact step from this player's current story.
    /// Returns true even if queueing fails: callers must stop the triggering action either way.
    pub(super) fn queue_story_cas(&mut self, entity: EntityId, point: u32, step: StoryStep) -> bool {
        let Some(player) = self.players.get(&entity) else { return true; };
        if player.preview.is_some() { return false; }
        let account = player.account;
        if self.story_saves.pending.contains_key(&account) { return true; }
        let Some(area) = self.areas.get(&player.area) else {
            self.fail_story_storage("the exact story step lost its field"); return true;
        };
        let (area_id, area_key, pos, facing, expected) =
            (area.id().clone(), player.area.clone(), player.snapshot.pos, player.snapshot.facing, player.progression.clone());
        let resumed = self.story_saves.resuming.filter(|r| r.entity == entity && r.account == account && r.id != 0);
        let id = match resumed.map(|r| r.id).or_else(|| self.new_story_pending_id()) {
            Some(id) => id,
            None => { self.fail_story_storage("story hold tokens are exhausted"); return true; }
        };
        let Some(player) = self.players.get_mut(&entity) else { return true; };
        player.story_pending = Some(id);
        player.snapshot.anim = AnimState::Idle;
        player.dirty = true;
        self.bump_story_save_epoch();
        let result = self.saver.as_ref().ok_or(SceneSaveError::Stopped).and_then(|saver|
            saver.save_story_cas(StoryCasJob { account, expected_story: expected.clone(), next_story: step.progress.clone() }));
        let done = match result {
            Ok(done) => done,
            Err(_) => { self.fail_story_storage("the exact story write could not be queued"); return true; }
        };
        self.story_saves.pending.insert(account, Pending { id, entity, area: area_key, expected, point, step,
            client_ack: resumed.is_some(), phase: Phase::Saving(done), followups: VecDeque::new() });
        if resumed.is_none() {
            self.send_story_barrier(entity, ServerMsg::StoryPending { id, area: area_id, pos, facing });
        }
        true
    }

    pub(in crate::world) fn defer_story_followup(&mut self, entity: EntityId, followup: StoryFollowup) -> bool {
        if let Some(pending) = self.story_saves.pending.values_mut().find(|pending| pending.entity == entity) {
            pending.followups.push_back(followup);
            return true;
        }
        let exact = self.players.get(&entity).is_some_and(|player| player.preview.is_none()
            && self.areas.get(&player.area).is_some_and(|area| area.runtime().story_requires_transaction(&player.progression)));
        if !exact { return false; }
        if let Some(run) = self.players.get(&entity).and_then(|player| player.scene) {
            self.defer_scene_story_followup(run, followup);
            return true;
        }
        if let Some(account) = self.players.get(&entity).map(|player| player.account).filter(|account| self.ordinary_battle_account_held(*account)) {
            self.story_saves.battles.entry(account).or_default().push_back(followup);
            return true;
        }
        false
    }

    pub(super) fn defer_scene_story_followup(&mut self, run: SceneRunId, followup: StoryFollowup) {
        self.story_saves.scenes.entry(run).or_default().push_back(followup);
    }

    pub(in crate::world) fn discard_scene_story_followups(&mut self, run: SceneRunId) {
        self.story_saves.scenes.remove(&run);
    }

    pub(in crate::world) fn resume_story_followups_after_scene(&mut self, run: SceneRunId, entity: EntityId, now: Instant) {
        let followups = self.story_saves.scenes.remove(&run).unwrap_or_default();
        let Some(player) = self.players.get(&entity) else { return; };
        let previous = self.story_saves.resuming.replace(Resuming { account: player.account, entity,
            id: player.story_pending.unwrap_or(0) });
        self.run_story_followups(entity, followups, now);
        self.story_saves.resuming = previous;
    }

    pub(in crate::world) fn discard_ordinary_story_battle_followups(&mut self, account: AccountId) {
        self.story_saves.battles.remove(&account);
    }

    /// Battle storage has already acknowledged this step and all resources. Its ordered token
    /// can cover any next exact write; a victory cutscene keeps the remaining work by run id.
    pub(super) fn resume_saved_story_battle(&mut self, entity: EntityId, before: Vec<u8>, step: StoryStep,
        formation: u16, point: u32, now: Instant) {
        let Some(player) = self.players.get(&entity) else { return; };
        let account = player.account;
        let previous = self.story_saves.resuming.replace(Resuming { account, entity, id: player.story_pending.unwrap_or(0) });
        self.apply_saved_story_step(entity, point, step.clone());
        self.start_victory_scene(entity, before, &step, formation, now);
        let followups = self.story_saves.battles.remove(&account).unwrap_or_default();
        self.run_story_followups(entity, followups, now);
        self.story_saves.resuming = previous;
    }

    /// Polling never awaits with the world locked. A disconnected owner retains its account
    /// reservation until storage answers; there is then no old input stream left to drain.
    pub(in crate::world) fn advance_story_saves(&mut self, now: Instant) {
        let accounts = self.story_saves.pending.keys().copied().collect::<Vec<_>>();
        for account in accounts {
            if self.scenes.storage_failed { return; }
            let Some(pending) = self.story_saves.pending.get_mut(&account) else { continue; };
            if let Phase::Saving(done) = &mut pending.phase {
                let result = match done.try_recv() {
                    Ok(result) => Some(result),
                    Err(oneshot::error::TryRecvError::Empty) => None,
                    Err(oneshot::error::TryRecvError::Closed) => Some(Err(SceneSaveError::WorkerFailed)),
                };
                if let Some(result) = result { pending.phase = Phase::Saved(result); }
            }
            let connected = self.players.get(&pending.entity).filter(|p| p.account == account);
            let ready = match &pending.phase {
                Phase::Saving(_) => false,
                Phase::Saved(Err(_)) => true,
                Phase::Saved(Ok(_)) => pending.client_ack || connected.is_none(),
                Phase::Published => connected.is_none_or(|p| p.transit.is_none()),
            };
            if !ready { continue; }
            let Some(pending) = self.story_saves.pending.remove(&account) else { continue; };
            self.bump_story_save_epoch();
            self.finish_story_cas(account, pending, now);
        }
    }

    fn finish_story_cas(&mut self, account: AccountId, mut pending: Pending, now: Instant) {
        if let Phase::Saved(result) = std::mem::replace(&mut pending.phase, Phase::Published) {
            let saved = match result {
                Ok(saved) => saved,
                Err(_) => { self.fail_story_storage("the exact story write was not confirmed"); return; }
            };
            if saved.pair.story.account_id != account || saved.pair.story.game != self.home_game()
                || saved.pair.story.blob != pending.step.progress {
                self.fail_story_storage("the exact story acknowledgment disagreed with its selected result"); return;
            }
            if let Some(player) = self.players.get(&pending.entity).filter(|p| p.account == account) {
                if player.progression != pending.expected || player.area != pending.area
                    || player.transit.is_some() || player.story_pending != Some(pending.id) {
                    self.fail_story_storage("the held story predecessor changed before acknowledgment"); return;
                }
            }
            if self.players.get(&pending.entity).is_some_and(|p| p.account == account) {
                self.story_saves.resuming = Some(Resuming { account, entity: pending.entity, id: pending.id });
                self.apply_saved_story_step(pending.entity, pending.point, pending.step.clone());
            }
        }
        if self.players.get(&pending.entity).is_none_or(|p| p.account != account) {
            // A queued scene's watchers still need their release, but an absent owner must not
            // restart timers, move fields, run entry points, or launch victory presentation.
            for followup in pending.followups {
                if let StoryFollowup::Scene { run } = followup {
                    self.finish_story_point_scene(run, now);
                    self.story_saves.scenes.remove(&run);
                }
            }
            self.story_saves.resuming = None;
            return;
        }
        self.story_saves.resuming = Some(Resuming { account, entity: pending.entity, id: pending.id });
        self.run_story_followups(pending.entity, std::mem::take(&mut pending.followups), now);
        self.story_saves.resuming = None;
        if self.story_saves.pending.contains_key(&account) || self.ordinary_battle_account_held(account) { return; }
        if self.players.get(&pending.entity).is_some_and(|p| p.account == account && p.transit.is_some()) {
            pending.followups.push_back(StoryFollowup::Entered);
            self.story_saves.pending.insert(account, pending);
            return;
        }
        if let Some(player) = self.players.get_mut(&pending.entity).filter(|p| p.account == account && p.story_pending == Some(pending.id)) {
            player.story_pending = None;
            self.send_story_barrier(pending.entity, ServerMsg::StoryCommitted { id: pending.id });
        }
    }

    fn run_story_followups(&mut self, entity: EntityId, mut followups: VecDeque<StoryFollowup>, now: Instant) {
        if let Some(player) = self.players.get(&entity) {
            if let Some(pending) = self.story_saves.pending.get_mut(&player.account) {
                pending.followups.append(&mut followups);
                return;
            }
            if let Some(run) = player.scene.filter(|run| !matches!(followups.front(), Some(StoryFollowup::Scene { run: finishing }) if finishing == run)) {
                if !followups.is_empty() { self.story_saves.scenes.entry(run).or_default().append(&mut followups); }
                return;
            }
            if self.ordinary_battle_account_held(player.account) {
                if !followups.is_empty() { self.story_saves.battles.entry(player.account).or_default().append(&mut followups); }
                return;
            }
            if player.transit.is_some() { return; }
        }
        while let Some(followup) = followups.pop_front() {
            let Some(account) = self.players.get(&entity).map(|p| p.account) else { return; };
            match followup {
                StoryFollowup::Move { area, from, to, distance } => self.resume_after_story_move(entity, area, from, to, distance, now),
                StoryFollowup::Scene { run } => {
                    self.finish_story_point_scene(run, now);
                    let mut scene = self.story_saves.scenes.remove(&run).unwrap_or_default();
                    scene.append(&mut followups);
                    followups = scene;
                }
                StoryFollowup::Automatic { area } => {
                    if self.players.get(&entity).is_some_and(|p| p.area == area && p.transit.is_none()) {
                        self.automatic_scene_entered(entity, now);
                    }
                }
                StoryFollowup::Sweep { area, points, on_entry } => self.run_story_points(entity, area, points, on_entry, now),
                StoryFollowup::Entered => {
                    self.enter_story(entity, now);
                    if !self.ordinary_story_held(entity) { self.automatic_scene_entered(entity, now); }
                }
                StoryFollowup::Victory { before, step, formation } => { self.start_victory_scene(entity, before, &step, formation, now); }
            }
            if let Some(pending) = self.story_saves.pending.get_mut(&account) {
                pending.followups.append(&mut followups);
                return;
            }
            if let Some(run) = self.players.get(&entity).and_then(|p| p.scene) {
                if !followups.is_empty() { self.story_saves.scenes.entry(run).or_default().append(&mut followups); }
                return;
            }
            if self.ordinary_battle_account_held(account) {
                if !followups.is_empty() { self.story_saves.battles.entry(account).or_default().append(&mut followups); }
                return;
            }
            if self.players.get(&entity).is_some_and(|p| p.battle.is_some() || p.transit.is_some()) { return; }
        }
    }

    fn send_story_barrier(&mut self, entity: EntityId, message: ServerMsg) {
        let mut failed = Vec::new();
        if let Some(player) = self.players.get(&entity) {
            if let Err(error) = player.tx.try_send(Arc::new(message)) { failed.push((entity, is_full(&error))); }
        }
        self.drop_failed(failed);
    }
}

#[cfg(test)]
mod tests;
