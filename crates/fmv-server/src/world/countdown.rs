//! The countdown a story step starts: the server owns the clock and tells the players.
//!
//! An adapter's story step can say that it starts a countdown (`StoryStep::countdown`: FF7's first reactor, whose Guard
//! Scorpion step sets a ten minute timer in the very block that moves the story). The world then keeps ONE countdown for
//! the account whose story it is (the one that took the step: the leader, in a party) and tells the players that story
//! governs, with [`fmv_protocol::ServerMsg::Countdown`]. Who is governed is the rule that already decides which exits are
//! open for a player: the account that chose the player's instance (`Placement::phase_owner`: itself when solo, its
//! party's leader for a member), so a member sees its leader's countdown and stops seeing it when it leaves the party,
//! and a countdown follows the party through every area.
//!
//! * **Start.** [`World::countdown_after_step`], when a step is applied: the step's countdown replaces any the account
//!   had. The deadline is set by the first [`World::advance_countdowns`] after it (a step knows no clock; the tick loop
//!   calls it every tick, so that is a few milliseconds later), the message says the whole length.
//! * **Tell.** [`World::sync_countdown`], at the end of every `settle` (a join, an arrival, a party change, a story
//!   step: whenever a player's placement may have changed), says to a player what it has not been told: a running
//!   countdown it does not know (`Running`, with what is left), or the end of one it was told of (`Over { Cleared }`).
//!   It is idempotent: a player that knows the state is told nothing.
//! * **End.** Three ways. The account's story moves past what the countdown was for (`AreaRuntime::countdown_live`
//!   says no for the new progression): `Over { Cleared }`. The account leaves the world: the same. Time runs out
//!   ([`World::advance_countdowns`]): `Over { TimeUp }`, one log line, **and nothing else**: no story step, no saved
//!   progress changed, wiped or reset, nobody moved. The game's own timer ends in a game over; this server does not punish
//!   a party for being late (a persistent account must never lose what it saved to a clock), and the story's own way out
//!   keeps working for a party that was late.
//! * **Not stored.** A countdown is a clock of this process: a server that restarts, and an account that logs in again,
//!   have none (and the story the account saved is as it was). A step that starts one is the only thing that does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaKey, StoryStep};
use fmv_core::{AccountId, EntityId};
use fmv_protocol::{CountdownEnd, CountdownState, ServerMsg};

use super::{is_full, World};

/// The longest countdown the world runs, whatever an adapter says (a day).
pub(super) const MAX_COUNTDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// A countdown the world runs for one account's story.
#[derive(Debug, Clone)]
pub(super) struct Countdown {
    /// The progression the step that started it left (`AreaRuntime::countdown_live` compares the account's later ones with it).
    started: Vec<u8>,
    /// How long it is.
    total: Duration,
    /// When it runs out: `None` until the first tick after the start sets it.
    deadline: Option<Instant>,
}

/// Milliseconds of `d`, as the wire carries them.
fn ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

impl World {
    /// A step of `account` was applied in `area` (`step.progress` is what the account's story is now): start the
    /// countdown the step carries, or end the one the account had when the story has moved past it (module docs).
    pub(super) fn countdown_after_step(&mut self, account: AccountId, area: &AreaKey, step: &StoryStep) {
        if let Some(seconds) = step.countdown.filter(|s| *s > 0) {
            let total = Duration::from_secs(u64::from(seconds)).min(MAX_COUNTDOWN);
            self.countdowns.insert(account, Countdown { started: step.progress.clone(), total, deadline: None });
            // whoever was told about the one that ran before is told about this one
            for player in self.players.values_mut().filter(|p| p.countdown_told == Some(account)) {
                player.countdown_told = None;
            }
            self.logs.info(format!("countdown started account={} seconds={}", account.0, total.as_secs()));
            return;
        }
        let moot = self.countdowns.get(&account).is_some_and(|c| {
            !self.areas.get(area).is_none_or(|a| a.runtime().countdown_live(&c.started, &step.progress))
        });
        if moot {
            self.countdowns.remove(&account);
            self.logs.info(format!("countdown over account={} reason=cleared (the story moved past it)", account.0));
        }
    }

    /// The account that governs `entity`'s story: whose countdown it sees (module docs).
    fn governing_account(&self, entity: EntityId) -> Option<AccountId> {
        self.players.get(&entity).map(|p| p.placement.as_ref().map_or(p.account, |placement| placement.phase_owner))
    }

    /// Tell `entity` what it has not been told of the countdown that governs it (module docs, "Tell").
    pub(super) fn sync_countdown(&mut self, entity: EntityId, failed: &mut Vec<(EntityId, bool)>) {
        let Some(governing) = self.governing_account(entity) else { return };
        let state = match (self.players.get(&entity).and_then(|p| p.countdown_told), self.countdowns.get(&governing)) {
            (Some(told), Some(_)) if told == governing => return,
            (_, Some(c)) => {
                let left = c.deadline.map_or(c.total, |d| d.saturating_duration_since(self.clock));
                CountdownState::Running { remaining_ms: ms(left), total_ms: ms(c.total) }
            }
            (Some(_), None) => CountdownState::Over { reason: CountdownEnd::Cleared },
            (None, None) => return,
        };
        let running = matches!(state, CountdownState::Running { .. });
        let Some(player) = self.players.get_mut(&entity) else { return };
        player.countdown_told = running.then_some(governing);
        if let Err(e) = player.tx.try_send(Arc::new(ServerMsg::Countdown { state })) {
            failed.push((entity, is_full(&e)));
        }
    }

    /// Settle what every player is told of countdowns (after one was ended or its account left).
    pub(super) fn sync_all_countdowns(&mut self, failed: &mut Vec<(EntityId, bool)>) {
        let entities: Vec<EntityId> = self.players.keys().copied().collect();
        for entity in entities {
            self.sync_countdown(entity, failed);
        }
    }

    /// The account left the world: its countdown goes with it (and whoever it governed is told it is over).
    pub(super) fn countdown_account_left(&mut self, account: AccountId, failed: &mut Vec<(EntityId, bool)>) {
        if self.countdowns.remove(&account).is_some() {
            self.logs.info(format!("countdown over account={} reason=cleared (its account left the world)", account.0));
            self.sync_all_countdowns(failed);
        }
    }

    /// The world's clock for the countdowns: set the deadlines of the ones just started and end the ones that ran out
    /// (module docs, "End"). Call it every tick.
    pub fn advance_countdowns(&mut self, now: Instant) {
        self.clock = now;
        if self.countdowns.is_empty() {
            return;
        }
        let mut expired = Vec::new();
        for (&account, countdown) in &mut self.countdowns {
            match countdown.deadline {
                None => countdown.deadline = Some(now + countdown.total),
                Some(deadline) if now >= deadline => expired.push(account),
                Some(_) => {}
            }
        }
        if expired.is_empty() {
            return;
        }
        let mut failed = Vec::new();
        for account in expired {
            self.countdowns.remove(&account);
            self.logs.info(format!(
                "countdown over account={} reason=time-up (nothing else happens: the story and the saved progress are untouched)",
                account.0
            ));
            for (&entity, player) in self.players.iter_mut().filter(|(_, p)| p.countdown_told == Some(account)) {
                player.countdown_told = None;
                let msg = ServerMsg::Countdown { state: CountdownState::Over { reason: CountdownEnd::TimeUp } };
                if let Err(e) = player.tx.try_send(Arc::new(msg)) {
                    failed.push((entity, is_full(&e)));
                }
            }
        }
        self.drop_failed(failed);
    }

    /// How many countdowns the world runs (tests).
    #[cfg(test)]
    pub(crate) fn countdown_count(&self) -> usize {
        self.countdowns.len()
    }
}

#[cfg(test)]
mod tests;
