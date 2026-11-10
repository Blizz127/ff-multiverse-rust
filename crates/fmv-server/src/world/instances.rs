//! Instances: which players share an area, decided by story phase and party.
//!
//! An AREA is the decoded data of a place (`areas::AreaState`: runtime, exits, rules); an INSTANCE is
//! one population of it: the players whose story phase for that area is the same, or whom a party
//! leader's phase puts there. The world keeps one `fmv_party::InstanceRegistry` (the single source of
//! truth for "who is in which instance") and every broadcast about an area reaches only the players of
//! one instance of it: `PlayerJoined`, `PlayerLeft`, `EntityUpdate` and the entities of an
//! `AreaSnapshot`. Instances are created when the first player enters and released when the last one
//! leaves; the area itself stays loaded as long as anybody stands in any instance of it.
//!
//! # The rule (see `fmv_party::phasing`)
//!
//! * Solo: the instance is `(area, own phase)`.
//! * Party: `(area, leader's phase)` for every member standing in that area; each member keeps its own
//!   progression, and [`Placement::story_advance_allowed`] says whether the story may advance for it
//!   there (its own phase chose the instance).
//! * The phase of an account in an area is the area's own reading of its progression
//!   (`AreaRuntime::phase_key`); an area that does not read it (`None`) uses one phase per distinct
//!   progression blob, the same for every area ([`crate::accounts::phase_key`]). The empty progression
//!   (every account that exists today) is the empty key in every area either way.
//!
//! # When a player changes instance
//!
//! [`World::settle`] is the one place: it computes the placement, and when it names another instance
//! than the one the player is in (or the player has just arrived in the area) it moves the player:
//! `PlayerLeft` to the instance it leaves, a fresh `AreaSnapshot` (this instance's entities and cast) to
//! the player and then its `InstancePlacement`, `PlayerJoined` to the instance it enters. A placement
//! that changes only its flag or its party is told alone. It runs on every entry to an area (join and
//! arrival), after every party change, and after the story advances for the player or for the leader of
//! its party.

use std::collections::BTreeSet;
use std::sync::Arc;

use fmv_core::{AccountId, EntityId, EntitySnapshot, PhaseKey};
use fmv_party::{InstanceEvent, InstanceId, InstanceSelector, PhaseMap, PhasingError, Placement};
use fmv_protocol::ServerMsg;

use super::areas::AreaState;
use super::{is_full, Outbound, World};

/// Lower-case hex of `bytes` (a phase key in a log line).
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl World {
    /// The phase of an area for an account with progression `progression`: what the area reads from
    /// it, else one phase per distinct blob.
    pub(super) fn phase_in(area: &AreaState, progression: &[u8]) -> PhaseKey {
        area.runtime().phase_key(progression).unwrap_or_else(|| crate::accounts::phase_key(progression.to_vec()))
    }

    /// Where `entity` belongs now: `Ok(None)` when it is in no area (gone, or in transit).
    pub(super) fn placement_of(&self, entity: EntityId) -> Result<Option<Placement>, PhasingError> {
        let Some(player) = self.players.get(&entity).filter(|p| p.transit.is_none()) else { return Ok(None) };
        let Some(area) = self.areas.get(&player.area) else { return Ok(None) };
        let party = self.parties.party_of(player.account).cloned();
        let mut phases = PhaseMap::new();
        // (a previewing player's phase is made up and its own: see `scene`)
        phases.insert(player.account, Self::player_phase(area, player));
        if let Some(party) = &party {
            // The leader's phase for THIS area, wherever the leader stands.
            if let Some(leader) = self.by_account.get(&party.leader).and_then(|e| self.players.get(e)) {
                phases.insert(party.leader, Self::player_phase(area, leader));
            }
        }
        InstanceSelector::select(area.id(), player.account, party.as_ref(), &phases).map(Some)
    }

    /// The instance `account` is in, if any.
    #[cfg(test)]
    pub(super) fn instance_of(&self, account: AccountId) -> Option<InstanceId> {
        self.instances.instance_of(account)
    }

    /// Queue `msg` for every player in instance `id`. Players whose queue is full or closed are
    /// appended to `failed` for the caller to drop. An instance that does not exist has nobody.
    pub(super) fn tell_instance(&self, id: InstanceId, msg: Outbound, failed: &mut Vec<(EntityId, bool)>) {
        let Some(instance) = self.instances.get(id) else { return };
        for account in instance.occupants() {
            let Some(&entity) = self.by_account.get(&account) else { continue };
            let Some(player) = self.players.get(&entity) else { continue };
            if let Err(e) = player.tx.try_send(Arc::clone(&msg)) {
                failed.push((entity, is_full(&e)));
            }
        }
    }

    /// The snapshots of everyone in instance `id`, in entity order.
    fn instance_entities(&self, id: InstanceId) -> Vec<EntitySnapshot> {
        let Some(instance) = self.instances.get(id) else { return Vec::new() };
        let mut entities: Vec<EntitySnapshot> = instance
            .occupants()
            .filter_map(|account| self.by_account.get(&account).and_then(|e| self.players.get(e)))
            .map(|p| p.snapshot.clone())
            .collect();
        entities.sort_by_key(|s| s.id);
        entities
    }

    /// Take `account` out of its instance (it leaves the world or goes into transit). Returns the
    /// instance it was in, so that the caller can tell the ones left behind.
    pub(super) fn leave_instance(&mut self, account: AccountId) -> Option<InstanceId> {
        let id = self.instances.instance_of(account)?;
        match self.instances.leave(account) {
            Ok(events) => self.log_instance_events(&events),
            Err(e) => self.logs.warn(format!("cannot take an account out of its instance account={} error={e}", account.0)),
        }
        Some(id)
    }

    fn log_instance_events(&mut self, events: &[InstanceEvent]) {
        for event in events {
            match event {
                InstanceEvent::Created { instance, key } => self.logs.info(format!(
                    "instance created instance={} area={} phase={}",
                    instance.0,
                    key.area,
                    if key.phase.0.is_empty() { "default".to_string() } else { hex(&key.phase.0) }
                )),
                InstanceEvent::Released { instance, key } => {
                    self.logs.info(format!("instance released instance={} area={}", instance.0, key.area))
                }
                InstanceEvent::Entered { .. } | InstanceEvent::Left { .. } => {}
            }
        }
    }

    /// Put `entity` where its placement says (module docs, "When a player changes instance").
    /// `entering`: it has just come to stand in its area (it joined, or finished a transfer): it is
    /// announced and sent the snapshot and its placement whatever it was told before. Otherwise this is a
    /// refresh after something changed (a party, the story): nothing is sent unless the instance or the
    /// placement differs.
    pub(super) fn settle(&mut self, entity: EntityId, entering: bool, failed: &mut Vec<(EntityId, bool)>) {
        self.settle_placement(entity, entering, failed);
        // Whoever governs the player's story now may have a countdown the player has not been told of (or none, after
        // one it was told of): the placement is what says who governs (see the `countdown` module).
        self.sync_countdown(entity, failed);
    }

    /// The placement half of [`World::settle`].
    fn settle_placement(&mut self, entity: EntityId, entering: bool, failed: &mut Vec<(EntityId, bool)>) {
        let placement = match self.placement_of(entity) {
            Ok(Some(placement)) => placement,
            Ok(None) => return,
            Err(e) => {
                self.logs.warn(format!("cannot compute a placement entity={} error={e}", entity.0));
                return;
            }
        };
        let Some(player) = self.players.get(&entity) else { return };
        let account = player.account;
        let current = self.instances.instance_of(account).and_then(|id| self.instances.get(id)).map(|i| i.key().clone());
        if !entering && current.as_ref() == Some(&placement.key) {
            // Same instance: only the party or the flag can differ.
            let Some(player) = self.players.get_mut(&entity) else { return };
            if player.placement.as_ref() == Some(&placement) {
                return;
            }
            player.placement = Some(placement.clone());
            if let Err(e) = player.tx.try_send(Arc::new(ServerMsg::InstancePlacement { placement })) {
                failed.push((entity, is_full(&e)));
            }
            return;
        }

        // Moving: leave the old instance (if any) and enter the one the placement names.
        let old = self.instances.instance_of(account);
        let events = match self.instances.enter(account, &placement.key) {
            Ok(events) => events,
            Err(e) => {
                self.logs.warn(format!("cannot place a player in an instance entity={} error={e}", entity.0));
                return;
            }
        };
        self.log_instance_events(&events);
        let Some(new) = self.instances.instance_of(account) else { return };
        if let Some(old) = old.filter(|old| *old != new) {
            self.tell_instance(old, Arc::new(ServerMsg::PlayerLeft { entity }), failed);
            self.logs.info(format!(
                "player moved to another instance of its area entity={} area={} from={} to={} phase={}",
                entity.0,
                placement.key.area,
                old.0,
                new.0,
                if placement.key.phase.0.is_empty() { "default".to_string() } else { hex(&placement.key.phase.0) }
            ));
        }

        // The triggers of the story scenes on offer here join the cast (see `scene`).
        let scene_npcs = self.scene_npcs(entity, &placement.key.area.key);
        let Some(area) = self.areas.get(&placement.key.area.key) else { return };
        let mut npcs = area.cast(&placement.key.phase);
        npcs.extend(scene_npcs);
        let snapshot = Arc::new(ServerMsg::AreaSnapshot { area: area.id().clone(), tick: self.tick, entities: self.instance_entities(new), npcs });
        let Some(me) = self.players.get(&entity) else { return };
        let joined = Arc::new(ServerMsg::PlayerJoined { snapshot: me.snapshot.clone() });
        if let Some(instance) = self.instances.get(new) {
            let others: BTreeSet<AccountId> = instance.occupants().collect();
            for other in others {
                let Some(&other_entity) = self.by_account.get(&other) else { continue };
                let Some(p) = self.players.get(&other_entity) else { continue };
                let msg = if other_entity == entity { &snapshot } else { &joined };
                if let Err(e) = p.tx.try_send(Arc::clone(msg)) {
                    failed.push((other_entity, is_full(&e)));
                }
            }
        }
        let Some(me) = self.players.get_mut(&entity) else { return };
        me.placement = Some(placement.clone());
        if let Err(e) = me.tx.try_send(Arc::new(ServerMsg::InstancePlacement { placement })) {
            failed.push((entity, is_full(&e)));
        }
    }

    /// Settle every online account in `accounts` that is standing in an area (see [`World::settle`]).
    pub(super) fn refresh_placements(&mut self, accounts: &BTreeSet<AccountId>, failed: &mut Vec<(EntityId, bool)>) {
        for &account in accounts {
            if let Some(&entity) = self.by_account.get(&account) {
                self.settle(entity, false, failed);
            }
        }
    }

    /// A changed story may remove or add scene triggers without changing the area's phase.
    /// Settle the affected accounts first, then refresh those that stayed in their instance.
    /// Players that moved already received a snapshot from `settle`; party-only refreshes use
    /// `refresh_placements` and do not generate this additional traffic.
    pub(super) fn refresh_story_placements(&mut self, accounts: &BTreeSet<AccountId>, failed: &mut Vec<(EntityId, bool)>) {
        let before: Vec<_> = accounts.iter().filter_map(|account| {
            let entity = *self.by_account.get(account)?;
            self.players.get(&entity).filter(|player| player.transit.is_none())?;
            Some((entity, *account, self.instances.instance_of(*account)?))
        }).collect();
        self.refresh_placements(accounts, failed);
        for (entity, account, instance) in before {
            if self.instances.instance_of(account) != Some(instance) { continue; }
            let Some(key) = self.instances.get(instance).map(|instance| instance.key().clone()) else { continue };
            let Some(player) = self.players.get(&entity).filter(|player| player.transit.is_none() && player.area == key.area.key) else { continue };
            if player.placement.as_ref().is_none_or(|placement| placement.key != key) { continue; }
            // Eligibility follows this recipient's phase owner, not necessarily its own story.
            let scene_npcs = self.scene_npcs(entity, &key.area.key);
            let Some(area) = self.areas.get(&key.area.key) else { continue };
            let mut npcs = area.cast(&key.phase);
            npcs.extend(scene_npcs);
            let snapshot = Arc::new(ServerMsg::AreaSnapshot {
                area: area.id().clone(), tick: self.tick, entities: self.instance_entities(instance), npcs,
            });
            let Some(player) = self.players.get(&entity) else { continue };
            if let Err(error) = player.tx.try_send(snapshot) {
                failed.push((entity, is_full(&error)));
            }
        }
    }
}
