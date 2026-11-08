//! Randomized operation sequences over a small account universe, including invalid ids, unknown
//! accounts, wrong actors, backwards time and `now == u64::MAX`.
//!
//! This is the evidence for "no panics on any API input" and for "event lists are exact":
//! after *every* call the [`Harness`] checks atomic failure, `validate()`, replica equality and
//! audience exactness. Runs are fully deterministic (own LCG, no wall clock).

mod common;

use common::*;
use fmv_core::AccountId;
use fmv_party::{Invite, InviteId, PartyConfig, PartyManager};
use std::collections::BTreeSet;

const GHOST: AccountId = AccountId(99);

fn pick(rng: &mut Lcg, accounts: &[AccountId]) -> AccountId {
    if rng.below(12) == 0 {
        GHOST
    } else {
        accounts[rng.below(accounts.len() as u64) as usize]
    }
}

/// Mostly a real pending invite with its real invitee; sometimes a wrong actor or a random id.
fn pick_invite(rng: &mut Lcg, pending: &[Invite], accounts: &[AccountId]) -> (InviteId, AccountId) {
    if !pending.is_empty() && rng.below(10) < 8 {
        let inv = pending[rng.below(pending.len() as u64) as usize];
        let actor = if rng.below(4) != 0 {
            inv.to
        } else {
            pick(rng, accounts)
        };
        (inv.id, actor)
    } else {
        (InviteId(rng.below(60)), pick(rng, accounts))
    }
}

fn run_seed(seed: u64, steps: usize) -> Harness {
    let mut rng = Lcg(seed);
    let max_size = 2 + rng.below(3) as usize; // 2..=4
    let ttl = 1 + rng.below(40);
    let accounts: Vec<AccountId> = (1..=6).map(AccountId).collect();
    let mut universe = accounts.clone();
    universe.push(GHOST);
    let mut h = Harness::new(
        PartyManager::new(PartyConfig::new(max_size, ttl).unwrap()),
        universe,
    );

    let mut base: u64 = 0;
    for step in 0..steps {
        base += rng.below(15);
        let now = match rng.below(25) {
            0 => u64::MAX,
            1 => base.saturating_sub(rng.below(30)),
            _ => base,
        };
        let pending: Vec<Invite> = h.m.invites().copied().collect();
        match rng.below(100) {
            0..=29 => {
                let (from, to) = (pick(&mut rng, &accounts), pick(&mut rng, &accounts));
                let _ = h.invite(from, to, now);
            }
            30..=49 => {
                let (invite, by) = pick_invite(&mut rng, &pending, &accounts);
                let _ = h.accept(invite, by, now);
            }
            50..=54 => {
                let (invite, by) = pick_invite(&mut rng, &pending, &accounts);
                let _ = h.decline(invite, by);
            }
            55..=64 => {
                let who = pick(&mut rng, &accounts);
                let _ = h.leave(who);
            }
            65..=72 => {
                let (leader, member) = (pick(&mut rng, &accounts), pick(&mut rng, &accounts));
                let _ = h.kick(leader, member);
            }
            73..=79 => {
                let (leader, member) = (pick(&mut rng, &accounts), pick(&mut rng, &accounts));
                let _ = h.promote(leader, member);
            }
            _ => {
                let _ = h.expire(now);
            }
        }
        if step % 25 == 0 {
            let json = serde_json::to_string(&h.m).unwrap();
            let back: PartyManager = serde_json::from_str(&json).unwrap();
            assert_eq!(
                back, h.m,
                "serde round trip changed the manager (seed {seed}, step {step})"
            );
        }
    }
    h
}

#[test]
fn random_sequences_hold_every_invariant_and_cover_every_event_kind() {
    let mut events: BTreeSet<String> = BTreeSet::new();
    let mut errors: BTreeSet<String> = BTreeSet::new();
    for seed in 0..60 {
        let h = run_seed(seed, 500);
        events.extend(h.events_seen);
        errors.extend(h.errors_seen);
    }
    for kind in [
        "InviteSent",
        "InviteDeclined",
        "InviteExpired",
        "InviteCancelled:InviteeJoinedParty",
        "InviteCancelled:InviterLeftParty",
        "InviteCancelled:InviterCannotInvite",
        "PartyFormed",
        "MemberJoined",
        "MemberLeft:Left",
        "MemberLeft:Kicked",
        "LeaderChanged:Promoted",
        "LeaderChanged:LeaderLeft",
        "Disbanded",
    ] {
        assert!(
            events.contains(kind),
            "random runs never produced {kind}: {events:?}"
        );
    }
    for name in [
        "SelfInvite",
        "AlreadyInParty",
        "NotLeader",
        "NotInParty",
        "TargetNotInParty",
        "DuplicateInvite",
        "PartyFull",
        "UnknownInvite",
        "InviteExpired",
        "NotInvitee",
        "CannotTargetSelf",
        "AlreadyLeader",
    ] {
        assert!(
            errors.contains(name),
            "random runs never hit error {name}: {errors:?}"
        );
    }
}

#[test]
fn identical_inputs_give_identical_events_and_state() {
    for seed in [3, 17, 41] {
        let a = run_seed(seed, 400);
        let b = run_seed(seed, 400);
        assert_eq!(
            a.log, b.log,
            "event stream is not deterministic (seed {seed})"
        );
        assert_eq!(a.m, b.m);
        assert_eq!(
            serde_json::to_string(&a.m).unwrap(),
            serde_json::to_string(&b.m).unwrap(),
            "serialized form is not deterministic"
        );
    }
}
