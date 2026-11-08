//! Live area instances keyed by `(area, phase)`.
//!
//! [`InstanceRegistry`] is pure bookkeeping: it hands out [`InstanceId`]s, tracks which accounts
//! are inside which instance, and releases an instance the moment its last occupant leaves. It
//! never loads or owns area data; the server reacts to the returned [`InstanceEvent`]s (load a
//! runtime on `Created`, drop it on `Released`).
//!
//! Rules:
//! * Instances are created on demand by [`InstanceRegistry::enter`]; none exists empty.
//! * An account is in at most one instance. Entering another instance moves it: `Left` (and
//!   `Released` if that emptied the old one) come first, then `Created` (if new), then `Entered`.
//! * Entering the instance you are already in is a no-op returning no events.
//! * Ids are never reused, even after release.

use crate::ids::InstanceId;
use crate::phasing::{InstanceKey, PhasingError};
use fmv_core::AccountId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One live instance and who is inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    id: InstanceId,
    key: InstanceKey,
    occupants: BTreeSet<AccountId>,
}

impl Instance {
    pub fn id(&self) -> InstanceId {
        self.id
    }

    pub fn key(&self) -> &InstanceKey {
        &self.key
    }

    /// Occupants in ascending account-id order.
    pub fn occupants(&self) -> impl Iterator<Item = AccountId> + '_ {
        self.occupants.iter().copied()
    }

    pub fn occupant_count(&self) -> usize {
        self.occupants.len()
    }

    pub fn contains(&self, account: AccountId) -> bool {
        self.occupants.contains(&account)
    }
}

/// What changed in the registry. The server turns these into instance load/unload work and
/// `PlayerJoined` / `PlayerLeft` style notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceEvent {
    /// A new instance now exists for `key`; load its area runtime.
    Created {
        instance: InstanceId,
        key: InstanceKey,
    },
    Entered {
        instance: InstanceId,
        account: AccountId,
    },
    Left {
        instance: InstanceId,
        account: AccountId,
    },
    /// The instance emptied and is gone; unload its runtime.
    Released {
        instance: InstanceId,
        key: InstanceKey,
    },
}

/// Map `(AreaId, PhaseKey)` to live instances with membership tracking. See the
/// [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRegistry {
    next_instance: u64,
    instances: BTreeMap<InstanceId, Instance>,
    /// Derived lookup index (never iterated, so hashing order cannot affect outputs).
    by_key: HashMap<InstanceKey, InstanceId>,
    /// Derived: which instance each account is in.
    placement: BTreeMap<AccountId, InstanceId>,
}

impl Default for InstanceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InstanceRegistry {
    pub fn new() -> Self {
        Self {
            next_instance: 1,
            instances: BTreeMap::new(),
            by_key: HashMap::new(),
            placement: BTreeMap::new(),
        }
    }

    // ----- queries -------------------------------------------------------------------------

    pub fn get(&self, id: InstanceId) -> Option<&Instance> {
        self.instances.get(&id)
    }

    /// The live instance for `key`, if any (never creates one).
    pub fn find(&self, key: &InstanceKey) -> Option<InstanceId> {
        self.by_key.get(key).copied()
    }

    pub fn instance_of(&self, account: AccountId) -> Option<InstanceId> {
        self.placement.get(&account).copied()
    }

    /// All live instances in id order.
    pub fn instances(&self) -> impl Iterator<Item = &Instance> + '_ {
        self.instances.values()
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    // ----- operations ----------------------------------------------------------------------

    /// Put `account` into the instance for `key`, creating it on demand and leaving whichever
    /// instance they were in. Errors only with [`PhasingError::IdSpaceExhausted`], in which case
    /// nothing changes.
    pub fn enter(
        &mut self,
        account: AccountId,
        key: &InstanceKey,
    ) -> Result<Vec<InstanceEvent>, PhasingError> {
        // A dangling index entry is treated as "no such instance" and healed by re-creating.
        let existing = self
            .by_key
            .get(key)
            .copied()
            .filter(|id| self.instances.contains_key(id));
        if existing.is_some() && existing == self.placement.get(&account).copied() {
            return Ok(Vec::new());
        }
        let (target, creating) = match existing {
            Some(id) => (id, None),
            None => {
                let id = InstanceId(self.next_instance);
                let next = self
                    .next_instance
                    .checked_add(1)
                    .ok_or(PhasingError::IdSpaceExhausted)?;
                (id, Some(next))
            }
        };

        // Validated; nothing below can fail.
        let mut events = Vec::new();
        if let Some(old) = self.placement.remove(&account) {
            self.detach(old, account, &mut events);
        }
        if let Some(next) = creating {
            self.next_instance = next;
            self.by_key.insert(key.clone(), target);
            self.instances.insert(
                target,
                Instance {
                    id: target,
                    key: key.clone(),
                    occupants: BTreeSet::new(),
                },
            );
            events.push(InstanceEvent::Created {
                instance: target,
                key: key.clone(),
            });
        }
        if let Some(instance) = self.instances.get_mut(&target) {
            instance.occupants.insert(account);
        }
        self.placement.insert(account, target);
        events.push(InstanceEvent::Entered {
            instance: target,
            account,
        });
        Ok(events)
    }

    /// Remove `account` from whichever instance they are in, releasing it if that was the last
    /// occupant. Errors with [`PhasingError::NotInInstance`] if they are not placed.
    pub fn leave(&mut self, account: AccountId) -> Result<Vec<InstanceEvent>, PhasingError> {
        let old = self
            .placement
            .remove(&account)
            .ok_or(PhasingError::NotInInstance { account })?;
        let mut events = Vec::new();
        self.detach(old, account, &mut events);
        Ok(events)
    }

    // ----- integrity -----------------------------------------------------------------------

    /// Checks every internal invariant: no empty instance, ids below the counter, and the key
    /// and placement indexes agree with the instances. Deserialization runs it automatically.
    pub fn validate(&self) -> Result<(), PhasingError> {
        let mut occupant_total = 0usize;
        for (id, instance) in &self.instances {
            if *id != instance.id {
                return corrupt("instance stored under a different id");
            }
            if id.0 >= self.next_instance {
                return corrupt("instance id is not below the counter");
            }
            if instance.occupants.is_empty() {
                return corrupt("empty instance was not released");
            }
            if self.by_key.get(&instance.key) != Some(id) {
                return corrupt("key index does not match the instances");
            }
            for account in &instance.occupants {
                if self.placement.get(account) != Some(id) {
                    return corrupt("placement index does not match the occupants");
                }
            }
            occupant_total = occupant_total.saturating_add(instance.occupants.len());
        }
        if self.by_key.len() != self.instances.len() {
            return corrupt("key index has stale entries");
        }
        if self.placement.len() != occupant_total {
            return corrupt("placement index has stale entries");
        }
        Ok(())
    }

    // ----- helpers -------------------------------------------------------------------------

    /// Remove `account` from `instance` (the caller already cleared `placement`), emitting
    /// `Left` and, if that emptied the instance, `Released`.
    fn detach(
        &mut self,
        instance: InstanceId,
        account: AccountId,
        events: &mut Vec<InstanceEvent>,
    ) {
        let Some(inst) = self.instances.get_mut(&instance) else {
            return;
        };
        inst.occupants.remove(&account);
        events.push(InstanceEvent::Left { instance, account });
        if inst.occupants.is_empty() {
            let key = inst.key.clone();
            self.instances.remove(&instance);
            self.by_key.remove(&key);
            events.push(InstanceEvent::Released { instance, key });
        }
    }

    fn from_state(state: RegistryState) -> Result<Self, PhasingError> {
        let mut instances = BTreeMap::new();
        let mut by_key = HashMap::new();
        let mut placement = BTreeMap::new();
        for record in state.instances {
            let mut occupants = BTreeSet::new();
            for account in record.occupants {
                if !occupants.insert(account) {
                    return corrupt("duplicate occupant in an instance");
                }
                if placement.insert(account, record.id).is_some() {
                    return corrupt("account placed in more than one instance");
                }
            }
            if by_key.insert(record.key.clone(), record.id).is_some() {
                return corrupt("two instances share a key");
            }
            let instance = Instance {
                id: record.id,
                key: record.key,
                occupants,
            };
            if instances.insert(record.id, instance).is_some() {
                return corrupt("duplicate instance id");
            }
        }
        let registry = Self {
            next_instance: state.next_instance,
            instances,
            by_key,
            placement,
        };
        registry.validate()?;
        Ok(registry)
    }
}

fn corrupt<T>(what: &str) -> Result<T, PhasingError> {
    Err(PhasingError::CorruptState(what.to_string()))
}

/// Persisted shape: plain sequences in id order. Serialize and deserialize views must keep
/// identical field names.
#[derive(Serialize)]
struct RegistryStateRef<'a> {
    next_instance: u64,
    instances: Vec<InstanceRecordRef<'a>>,
}

#[derive(Serialize)]
struct InstanceRecordRef<'a> {
    id: InstanceId,
    key: &'a InstanceKey,
    occupants: Vec<AccountId>,
}

#[derive(Deserialize)]
struct RegistryState {
    next_instance: u64,
    instances: Vec<InstanceRecord>,
}

#[derive(Deserialize)]
struct InstanceRecord {
    id: InstanceId,
    key: InstanceKey,
    occupants: Vec<AccountId>,
}

impl Serialize for InstanceRegistry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RegistryStateRef {
            next_instance: self.next_instance,
            instances: self
                .instances
                .values()
                .map(|i| InstanceRecordRef {
                    id: i.id,
                    key: &i.key,
                    occupants: i.occupants.iter().copied().collect(),
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InstanceRegistry {
    /// Rebuilds the derived indexes and rejects state that violates any invariant.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = RegistryState::deserialize(deserializer)?;
        InstanceRegistry::from_state(state).map_err(serde::de::Error::custom)
    }
}
