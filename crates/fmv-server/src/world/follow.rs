//! A member of a party follows the way its leader left an area by.
//!
//! The exits a script decides (`ExitKind::Script`: FF7's lift of the first reactor, four exits on one line, the story
//! picks which one is open) are judged for a party member on its LEADER's story (`World::instance_progression`). That
//! story moves while the members are still on their way: the leader reaches the reactor's finale in `nrthmk` and the
//! story goes from moment 27 to 33, and a member that comes to the lift after that, a few seconds behind, is judged on
//! moment 33, where the lift leads to the second reactor, a field nobody has reviewed. The member was not making the
//! story, it was walking the way its leader had walked, and it must end up where the leader did.
//!
//! * **Remember.** When a party's leader leaves an area through an exit its story decided, the world keeps which one
//!   ([`World::note_way_out`], called when the transfer starts), on the leader: the party, the area and the exit. A leader
//!   remembers the last [`MAX_WAYS_PER_LEADER`] areas it left this way (it is bounded and it goes with the leader).
//!   Gateways are data and open for every story: nothing is kept for them.
//! * **Follow.** A member of that party that crosses the same exit line in the same area is taken through the exit the
//!   leader took, whatever the story now says ([`World::followed_exit`], asked by `open_exit` first; the story's own
//!   choice decides only when no way is remembered). Each member follows a way ONCE: the one that crossed has done what the
//!   leader's way was for, and is judged on the story as always the next time it comes by (it is not a standing order).
//! * **Forget.** A way is forgotten when the leader comes to stand in that area again ([`World::forget_ways_into`]: the
//!   party is together again and the story decides), when it is replaced by the leader's next way out of the area, when
//!   the party's leader changes or the party is disbanded ([`World::forget_ways_of`]), and with the leader's account
//!   leaving the world. A way never applies to a player that is not in the party it was made for.
//!
//! Nothing here changes anybody's story: only which exit a crossing member takes. The member's own progression is what it
//! was, and the target's instance is chosen as always (the leader's phase for a member).

use std::collections::BTreeSet;

use fmv_adapter_api::{AreaKey, Exit, ExitKind};
use fmv_core::{AccountId, EntityId};
use fmv_party::PartyId;

use super::World;

/// How many areas a leader remembers the way out of.
pub(super) const MAX_WAYS_PER_LEADER: usize = 8;

/// Where a party's leader went when it left `area` through an exit its story decided.
#[derive(Debug, Clone)]
pub(super) struct WayOut {
    /// The party the leader led (a way is for the members of that party only).
    party: PartyId,
    /// The area the leader left.
    area: AreaKey,
    /// The exit it took (an exit of that area: the members cross the same line).
    exit: Exit,
    /// The members that have followed it (each follows once).
    followed: BTreeSet<AccountId>,
}

impl World {
    /// `entity` has started to leave `area` through `exit`: when it leads a party and the exit is one its story decided,
    /// remember it for the members (module docs, "Remember").
    pub(super) fn note_way_out(&mut self, entity: EntityId, area: &AreaKey, exit: &Exit) {
        if !matches!(exit.kind, ExitKind::Script { .. }) {
            return;
        }
        let Some(account) = self.players.get(&entity).map(|p| p.account) else { return };
        let Some(party) = self.parties.party_of(account).filter(|p| p.leader == account).map(|p| p.id) else { return };
        let Some(player) = self.players.get_mut(&entity) else { return };
        player.ways.retain(|w| !(w.party == party && w.area == *area));
        player.ways.push(WayOut { party, area: area.clone(), exit: exit.clone(), followed: BTreeSet::new() });
        if player.ways.len() > MAX_WAYS_PER_LEADER {
            player.ways.remove(0);
        }
    }

    /// The exit of `crossed` (the exits `entity`'s move crosses, in `area`) that its party's leader left `area` by, when
    /// `entity` is a member that has not followed that way yet (module docs, "Follow"). Marks it followed.
    pub(super) fn followed_exit(&mut self, entity: EntityId, area: &AreaKey, crossed: &[Exit]) -> Option<Exit> {
        let account = self.players.get(&entity)?.account;
        let party = self.parties.party_of(account)?;
        if party.leader == account {
            return None;
        }
        let (party, leader) = (party.id, party.leader);
        let leader_entity = *self.by_account.get(&leader)?;
        let way = self.players.get_mut(&leader_entity)?.ways.iter_mut().find(|w| w.party == party && w.area == *area && !w.followed.contains(&account))?;
        let exit = crossed.iter().find(|e| **e == way.exit)?.clone();
        way.followed.insert(account);
        Some(exit)
    }

    /// `entity` has just come to stand in its area: the way it left that area by, if it remembered one, is over (module
    /// docs, "Forget").
    pub(super) fn forget_ways_into(&mut self, entity: EntityId) {
        if let Some(player) = self.players.get_mut(&entity) {
            let area = player.area.clone();
            player.ways.retain(|w| w.area != area);
        }
    }

    /// Everything these accounts remember of ways out is over (their party's leader changed, or it was disbanded).
    pub(super) fn forget_ways_of(&mut self, accounts: &[AccountId]) {
        for account in accounts {
            if let Some(player) = self.by_account.get(account).and_then(|e| self.players.get_mut(e)) {
                player.ways.clear();
            }
        }
    }

    /// The ways `entity` remembers (tests).
    #[cfg(test)]
    pub(super) fn ways_of(&self, entity: EntityId) -> usize {
        self.players.get(&entity).map_or(0, |p| p.ways.len())
    }
}

#[cfg(test)]
mod real_tests;
