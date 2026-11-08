//! The headless battle player behind `--battle-auto`.
//!
//! While a battle runs it acts whenever our own gauge is full (one command at a time: the mirror's
//! `pending` guard keeps it from repeating a command before the server answered). Its rule is
//! `BattleClient::bot_command`: when its character's HP is under 30% of the maximum and it knows a
//! restoring spell it can pay for, it casts the cheapest one on itself; otherwise, when its limit gauge is
//! full (protocol v7), it gives its limit break (a damaging one on the first living enemy, a healing one
//! on a wounded ally); otherwise it attacks the first living enemy. It never escapes. With
//! `--debug-encounters` it also asks the server for a debug encounter,
//! ONCE: when the session has been up for a moment and, with `--battle-wait-party N`, only when
//! our party has N members and we lead it (so one request starts one battle for everyone). A
//! refusal (a server without battle data, or without `--debug-encounters`) is logged by the
//! mirror and never retried: the bot just plays whatever battles come (random encounters), and
//! the SMOKE line says `battles=0` when none did.

use super::{BattleClient, BattleCmd};
use crate::party::PartyMirror;

/// The request is not sent before the session has been running for this long (seconds), so the
/// first snapshots and placements have arrived and a party that is about to form can form.
pub const REQUEST_DELAY_S: f64 = 1.0;

/// See the module docs.
#[derive(Debug, Clone)]
pub struct BattleBot {
    /// Play battles (attack, and heal itself when its HP is low).
    auto: bool,
    /// Ask the server for a debug encounter (once).
    request: bool,
    /// Request only once our party has at least this many members (and we lead it).
    wait_party: usize,
    requested: bool,
    first_seen: Option<f64>,
}

impl BattleBot {
    /// `auto`: play battles (`--battle-auto`). `request`: also ask for a debug encounter
    /// (`--debug-encounters`). `wait_party`: see the module docs (`1`: do not wait).
    pub fn new(auto: bool, request: bool, wait_party: usize) -> Self {
        Self { auto, request: auto && request, wait_party: wait_party.max(1), requested: false, first_seen: None }
    }

    /// Whether the bot plays battles at all.
    pub fn is_active(&self) -> bool {
        self.auto
    }

    /// Whether the one request was sent.
    #[cfg(test)]
    pub fn requested(&self) -> bool {
        self.requested
    }

    /// What to send now. `now` is seconds on the local monotonic clock.
    pub fn step(&mut self, now: f64, battles: &BattleClient, party: &PartyMirror) -> Vec<BattleCmd> {
        if !self.auto {
            return Vec::new();
        }
        let first = *self.first_seen.get_or_insert(now);
        if battles.in_battle() {
            return battles.bot_command(now).into_iter().collect();
        }
        if !self.request || self.requested || now - first < REQUEST_DELAY_S {
            return Vec::new();
        }
        if self.wait_party > 1 && !(party.party_size() >= self.wait_party && party.is_leader()) {
            return Vec::new();
        }
        self.requested = true;
        vec![BattleCmd::RequestEncounter { formation: None }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AccountId, EntityId};
    use fmv_protocol::{InviteId, PartyEvent, PartyId, PartyName};

    const ME: AccountId = AccountId(1);
    const PEER: AccountId = AccountId(2);

    fn party_of_two(leader: AccountId) -> PartyMirror {
        let mut p = PartyMirror::new(ME);
        let names = vec![
            PartyName { account: ME, name: "Me".into(), entity: Some(EntityId(1)) },
            PartyName { account: PEER, name: "Peer".into(), entity: Some(EntityId(2)) },
        ];
        p.on_event(1, PartyEvent::PartyFormed { party: PartyId(1), invite: InviteId(1), leader, members: vec![ME, PEER] }, names);
        p
    }

    #[test]
    fn a_bot_without_the_flags_never_sends_anything() {
        let battles = BattleClient::new(ME);
        let party = PartyMirror::new(ME);
        for (auto, request) in [(false, false), (false, true)] {
            let mut bot = BattleBot::new(auto, request, 1);
            assert!(!bot.is_active());
            for t in [0.0, 1.5, 50.0] {
                assert!(bot.step(t, &battles, &party).is_empty());
            }
        }
    }

    #[test]
    fn the_request_is_sent_once_after_the_delay_and_never_again() {
        let battles = BattleClient::new(ME);
        let party = PartyMirror::new(ME);
        let mut bot = BattleBot::new(true, true, 1);
        assert!(bot.step(10.0, &battles, &party).is_empty(), "the clock starts at the first call");
        assert!(bot.step(10.0 + REQUEST_DELAY_S - 0.01, &battles, &party).is_empty());
        assert_eq!(bot.step(10.0 + REQUEST_DELAY_S, &battles, &party), vec![BattleCmd::RequestEncounter { formation: None }]);
        assert!(bot.requested());
        for t in [12.0, 13.0, 100.0, 5_000.0] {
            assert!(bot.step(t, &battles, &party).is_empty(), "t {t}: one request only, whatever the answer was");
        }
    }

    #[test]
    fn attacking_alone_never_asks_for_a_battle() {
        let battles = BattleClient::new(ME);
        let party = PartyMirror::new(ME);
        let mut bot = BattleBot::new(true, false, 1);
        assert!(bot.is_active());
        for t in [0.0, 2.0, 60.0] {
            assert!(bot.step(t, &battles, &party).is_empty());
        }
        assert!(!bot.requested());
    }

    #[test]
    fn with_a_party_to_wait_for_only_its_leader_asks_once_it_is_big_enough() {
        let battles = BattleClient::new(ME);
        // Alone, or in no party at all: waiting.
        let mut bot = BattleBot::new(true, true, 2);
        assert!(bot.step(0.0, &battles, &PartyMirror::new(ME)).is_empty());
        assert!(bot.step(5.0, &battles, &PartyMirror::new(ME)).is_empty());
        // In a party of two that somebody else leads: still not our request to make.
        assert!(bot.step(6.0, &battles, &party_of_two(PEER)).is_empty());
        assert!(!bot.requested());
        // Leading a party of two: ask.
        assert_eq!(bot.step(7.0, &battles, &party_of_two(ME)), vec![BattleCmd::RequestEncounter { formation: None }]);
        assert!(bot.step(8.0, &battles, &party_of_two(ME)).is_empty());
        // A party of three is waited for by a bot that wants three.
        let mut greedy = BattleBot::new(true, true, 3);
        assert!(greedy.step(0.0, &battles, &party_of_two(ME)).is_empty());
        assert!(greedy.step(9.0, &battles, &party_of_two(ME)).is_empty());
    }

    #[test]
    fn a_wait_of_zero_means_do_not_wait() {
        let battles = BattleClient::new(ME);
        let mut bot = BattleBot::new(true, true, 0);
        bot.step(0.0, &battles, &PartyMirror::new(ME));
        assert_eq!(bot.step(2.0, &battles, &PartyMirror::new(ME)).len(), 1);
    }
}
