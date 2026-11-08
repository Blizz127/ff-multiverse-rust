//! Exact-event tests for every party operation. Each test asserts the precise event list, and
//! the scripted scenario at the bottom additionally runs under the full [`Harness`] checks.

mod common;

use common::*;
use fmv_core::AccountId;
use fmv_party::{
    DisbandReason, InviteCancelReason, LeaderChangeReason, LeaveReason, PartyConfig, PartyError,
    PartyEvent, PartyManager, Tick,
};

/// Cap 3 (FF7), invite ttl 10 ticks.
fn mgr() -> PartyManager {
    PartyManager::new(PartyConfig::new(3, 10).unwrap())
}

/// `from` invites `to`, who accepts immediately. Returns the accept events.
fn invite_accept(
    m: &mut PartyManager,
    from: AccountId,
    to: AccountId,
    now: Tick,
) -> Vec<PartyEvent> {
    let (id, _) = m.invite(from, to, now).unwrap();
    m.accept(id, to, now).unwrap()
}

/// Party 1 = [A (leader), B, C]; invites 1 and 2 consumed, so the next invite id is 3.
fn trio() -> PartyManager {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);
    invite_accept(&mut m, A, C, 0);
    m
}

// ----- happy paths --------------------------------------------------------------------------

#[test]
fn invite_then_accept_forms_a_party() {
    let mut m = mgr();
    let (inv, ev) = m.invite(A, B, 100).unwrap();
    assert_eq!(inv, iid(1));
    assert_eq!(
        ev,
        vec![PartyEvent::InviteSent {
            invite: iid(1),
            from: A,
            to: B,
            expires_at: 110
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B]);
    assert_eq!(m.invites_to(B).count(), 1);
    assert!(m.party_of(A).is_none() && m.party_of(B).is_none());

    let ev = m.accept(inv, B, 105).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::PartyFormed {
            party: pid(1),
            invite: iid(1),
            leader: A,
            members: vec![A, B]
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B]);
    let party = m.party_of(A).unwrap();
    assert_eq!(Some(party), m.party_of(B));
    assert_eq!(
        (party.id, party.leader, party.members.clone()),
        (pid(1), A, vec![A, B])
    );
    assert!(m.is_leader(A) && !m.is_leader(B));
    assert_eq!(m.party_id_of(B), Some(pid(1)));
    assert_eq!(m.invite_count(), 0);
    m.validate().unwrap();
}

#[test]
fn leader_invites_a_third_member() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);
    let (inv, ev) = m.invite(A, C, 1).unwrap();
    assert_eq!(inv, iid(2));
    assert_eq!(
        ev,
        vec![PartyEvent::InviteSent {
            invite: iid(2),
            from: A,
            to: C,
            expires_at: 11
        }]
    );
    let ev = m.accept(inv, C, 2).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::MemberJoined {
            party: pid(1),
            invite: iid(2),
            account: C,
            leader: A,
            members: vec![A, B, C],
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B, C]);
    assert_eq!(m.party_count(), 1);
}

#[test]
fn one_solo_inviter_can_have_two_invites_accepted() {
    // The invite resolves against the inviter's standing at accept time: the first accept
    // forms the party, the second joins it.
    let mut m = mgr();
    let (i1, _) = m.invite(A, B, 0).unwrap();
    let (i2, _) = m.invite(A, C, 0).unwrap();
    let ev1 = m.accept(i1, B, 1).unwrap();
    assert_eq!(
        ev1,
        vec![PartyEvent::PartyFormed {
            party: pid(1),
            invite: i1,
            leader: A,
            members: vec![A, B]
        }]
    );
    assert!(
        m.get_invite(i2).is_some(),
        "A's other invite stays valid once A leads a party"
    );
    let ev2 = m.accept(i2, C, 2).unwrap();
    assert_eq!(
        ev2,
        vec![PartyEvent::MemberJoined {
            party: pid(1),
            invite: i2,
            account: C,
            leader: A,
            members: vec![A, B, C],
        }]
    );
}

// ----- size cap ------------------------------------------------------------------------------

#[test]
fn size_cap_blocks_invites_into_a_full_party() {
    let mut m = trio();
    let before = m.clone();
    assert_eq!(
        m.invite(A, D, 5),
        Err(PartyError::PartyFull {
            party: pid(1),
            max_size: 3
        })
    );
    assert_eq!(m, before);
}

#[test]
fn size_cap_is_rechecked_at_accept_and_failed_accept_changes_nothing() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0); // [A, B], one free slot
    let (ic, _) = m.invite(A, C, 1).unwrap();
    let (id, _) = m.invite(A, D, 1).unwrap(); // two invites, one slot: first come first served
    m.accept(ic, C, 2).unwrap(); // now full

    let before = m.clone();
    assert_eq!(
        m.accept(id, D, 3),
        Err(PartyError::PartyFull {
            party: pid(1),
            max_size: 3
        })
    );
    assert_eq!(m, before, "a refused accept is atomic");
    assert!(m.get_invite(id).is_some(), "the invite stays pending");

    // A slot frees up; the still-pending invite works again.
    m.leave(B).unwrap();
    let ev = m.accept(id, D, 4).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::MemberJoined {
            party: pid(1),
            invite: id,
            account: D,
            leader: A,
            members: vec![A, C, D],
        }]
    );
}

#[test]
fn size_cap_is_configurable_per_game() {
    let mut m = PartyManager::new(PartyConfig::new(2, 10).unwrap());
    invite_accept(&mut m, A, B, 0); // full at 2
    assert_eq!(
        m.invite(A, C, 1),
        Err(PartyError::PartyFull {
            party: pid(1),
            max_size: 2
        })
    );
    // Leadership is checked before capacity.
    assert_eq!(m.invite(B, C, 1), Err(PartyError::NotLeader { account: B }));

    let mut four = PartyManager::new(PartyConfig::new(4, 10).unwrap());
    invite_accept(&mut four, A, B, 0);
    invite_accept(&mut four, A, C, 0);
    invite_accept(&mut four, A, D, 0);
    assert_eq!(four.party_of(A).unwrap().members, vec![A, B, C, D]);
    assert_eq!(
        four.invite(A, E, 1),
        Err(PartyError::PartyFull {
            party: pid(1),
            max_size: 4
        })
    );
}

// ----- decline / wrong actor -----------------------------------------------------------------

#[test]
fn decline_removes_the_invite_and_ids_are_never_reused() {
    let mut m = mgr();
    let (inv, _) = m.invite(A, B, 0).unwrap();
    let ev = m.decline(inv, B).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::InviteDeclined {
            invite: iid(1),
            from: A,
            to: B
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B]);
    assert_eq!(m.invite_count(), 0);
    assert_eq!(
        m.accept(inv, B, 1),
        Err(PartyError::UnknownInvite { invite: inv })
    );
    assert_eq!(
        m.decline(inv, B),
        Err(PartyError::UnknownInvite { invite: inv })
    );
    let (again, _) = m.invite(A, B, 2).unwrap();
    assert_eq!(again, iid(2));
}

#[test]
fn only_the_invitee_may_accept_or_decline() {
    let mut m = mgr();
    let (inv, _) = m.invite(A, B, 0).unwrap();
    let before = m.clone();
    assert_eq!(
        m.accept(inv, C, 1),
        Err(PartyError::NotInvitee {
            invite: inv,
            account: C
        })
    );
    assert_eq!(
        m.accept(inv, A, 1),
        Err(PartyError::NotInvitee {
            invite: inv,
            account: A
        })
    );
    assert_eq!(
        m.decline(inv, C),
        Err(PartyError::NotInvitee {
            invite: inv,
            account: C
        })
    );
    assert_eq!(
        m.accept(iid(99), B, 1),
        Err(PartyError::UnknownInvite { invite: iid(99) })
    );
    assert_eq!(m, before);
}

// ----- expiry --------------------------------------------------------------------------------

#[test]
fn invite_is_acceptable_for_exactly_ttl_ticks() {
    let mut m = mgr();
    let (inv, _) = m.invite(A, B, 100).unwrap(); // expires_at 110
    assert_eq!(m.get_invite(inv).unwrap().expires_at, 110);
    assert_eq!(
        m.expire(109).unwrap(),
        vec![],
        "nothing expires before the deadline"
    );

    let before = m.clone();
    assert_eq!(
        m.accept(inv, B, 110),
        Err(PartyError::InviteExpired { invite: inv })
    );
    assert_eq!(
        m, before,
        "a late accept changes nothing; the invite waits for expire()"
    );

    let ev = m.expire(110).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::InviteExpired {
            invite: inv,
            from: A,
            to: B
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B]);
    assert_eq!(m.invite_count(), 0);
    assert_eq!(m.expire(110).unwrap(), vec![], "expire is idempotent");
    assert_eq!(
        m.accept(inv, B, 111),
        Err(PartyError::UnknownInvite { invite: inv })
    );

    let mut ok = mgr();
    let (inv, _) = ok.invite(A, B, 100).unwrap();
    assert!(
        ok.accept(inv, B, 109).is_ok(),
        "the last tick before the deadline still works"
    );
}

#[test]
fn expire_reaps_in_invite_id_order_and_keeps_live_invites() {
    let mut m = mgr();
    m.invite(A, B, 100).unwrap(); // id 1, expires 110
    m.invite(C, D, 103).unwrap(); // id 2, expires 113
    m.invite(E, F, 120).unwrap(); // id 3, expires 130
    let ev = m.expire(115).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::InviteExpired {
                invite: iid(1),
                from: A,
                to: B
            },
            PartyEvent::InviteExpired {
                invite: iid(2),
                from: C,
                to: D
            },
        ]
    );
    assert_eq!(m.invite_count(), 1);
    assert!(m.get_invite(iid(3)).is_some());
}

#[test]
fn reinviting_replaces_a_timed_out_invite_and_a_live_duplicate_is_refused() {
    let mut m = mgr();
    let (old, _) = m.invite(A, B, 100).unwrap(); // expires 110
    let before = m.clone();
    assert_eq!(
        m.invite(A, B, 105),
        Err(PartyError::DuplicateInvite { from: A, to: B })
    );
    assert_eq!(m, before);

    // Timed out but never reaped: replaced. The expiry is announced first, so callers must use
    // the returned id rather than assuming InviteSent is events[0].
    let (new, ev) = m.invite(A, B, 150).unwrap();
    assert_eq!(new, iid(2));
    assert_eq!(
        ev,
        vec![
            PartyEvent::InviteExpired {
                invite: old,
                from: A,
                to: B
            },
            PartyEvent::InviteSent {
                invite: new,
                from: A,
                to: B,
                expires_at: 160
            },
        ]
    );
    assert_eq!(m.invite_count(), 1);
    // The opposite direction is a different pair.
    assert!(m.invite(B, A, 150).is_ok());
}

#[test]
fn extreme_clock_values_never_panic() {
    let mut m = mgr();
    let (inv, ev) = m.invite(A, B, u64::MAX - 3).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::InviteSent {
            invite: inv,
            from: A,
            to: B,
            expires_at: u64::MAX
        }],
        "expires_at saturates instead of overflowing"
    );
    assert!(m.accept(inv, B, u64::MAX - 2).is_ok());
    let (inv2, _) = m.invite(A, C, u64::MAX).unwrap();
    assert_eq!(
        m.accept(inv2, C, u64::MAX),
        Err(PartyError::InviteExpired { invite: inv2 })
    );
    assert_eq!(m.expire(0).unwrap(), vec![]);
    assert_eq!(m.expire(u64::MAX).unwrap().len(), 1);
    m.validate().unwrap();
}

// ----- leave / leader succession / disband ---------------------------------------------------

#[test]
fn leader_leaving_promotes_the_longest_standing_member() {
    let mut m = trio();
    let ev = m.leave(A).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::LeaderChanged {
                party: pid(1),
                old_leader: A,
                new_leader: B,
                reason: LeaderChangeReason::LeaderLeft,
                members: vec![A, B, C],
            },
            PartyEvent::MemberLeft {
                party: pid(1),
                account: A,
                reason: LeaveReason::Left,
                members: vec![B, C],
            },
        ]
    );
    assert_eq!(ev[0].audience(), vec![A, B, C]);
    assert_eq!(ev[1].audience(), vec![B, C, A], "the leaver is told too");
    let party = m.party_of(B).unwrap();
    assert_eq!((party.leader, party.members.clone()), (B, vec![B, C]));
    assert!(m.party_of(A).is_none());
}

#[test]
fn promotion_does_not_change_seniority() {
    let mut m = trio();
    let ev = m.promote(A, C).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::LeaderChanged {
            party: pid(1),
            old_leader: A,
            new_leader: C,
            reason: LeaderChangeReason::Promoted,
            members: vec![A, B, C],
        }]
    );
    // C leaves: the longest-standing remaining member is A (founder), not B.
    let ev = m.leave(C).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::LeaderChanged {
                party: pid(1),
                old_leader: C,
                new_leader: A,
                reason: LeaderChangeReason::LeaderLeft,
                members: vec![A, B, C],
            },
            PartyEvent::MemberLeft {
                party: pid(1),
                account: C,
                reason: LeaveReason::Left,
                members: vec![A, B],
            },
        ]
    );
    assert!(m.is_leader(A));
}

#[test]
fn a_regular_member_leaving_does_not_change_the_leader() {
    let mut m = trio();
    let ev = m.leave(B).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::MemberLeft {
            party: pid(1),
            account: B,
            reason: LeaveReason::Left,
            members: vec![A, C],
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, C, B]);
    assert!(m.is_leader(A));
    assert!(m.party_of(B).is_none());
    // B is free to be invited again.
    assert!(m.invite(A, B, 1).is_ok());
}

#[test]
fn last_member_leaving_disbands_and_ids_are_not_reused() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);

    // A party of one is legal; it only disbands when its last member leaves.
    let ev = m.leave(B).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::MemberLeft {
            party: pid(1),
            account: B,
            reason: LeaveReason::Left,
            members: vec![A],
        }]
    );
    assert_eq!(m.party_of(A).unwrap().members, vec![A]);
    assert!(m.is_leader(A));

    // The leader of a party of one inviting someone joins the existing party.
    let (inv, _) = m.invite(A, C, 5).unwrap();
    let ev = m.accept(inv, C, 6).unwrap();
    assert!(matches!(ev[0], PartyEvent::MemberJoined { party, .. } if party == pid(1)));
    m.leave(C).unwrap();

    // Disbanded replaces the final MemberLeft and still has an audience.
    let ev = m.leave(A).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::Disbanded {
            party: pid(1),
            members: vec![A],
            reason: DisbandReason::LastMemberLeft,
        }]
    );
    assert_eq!(ev[0].audience(), vec![A]);
    assert_eq!(m.party_count(), 0);
    assert!(m.party(pid(1)).is_none() && m.party_of(A).is_none());

    let ev = invite_accept(&mut m, A, B, 10);
    assert!(matches!(ev[0], PartyEvent::PartyFormed { party, .. } if party == pid(2)));
    assert_eq!(m.leave(D), Err(PartyError::NotInParty { account: D }));
}

// ----- kick ----------------------------------------------------------------------------------

#[test]
fn kick_removes_a_member() {
    let mut m = trio();
    let ev = m.kick(A, B).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::MemberLeft {
            party: pid(1),
            account: B,
            reason: LeaveReason::Kicked,
            members: vec![A, C],
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, C, B], "the kicked member is told");
    assert!(m.party_of(B).is_none());

    // A kicked player can be invited again and rejoins at the back of the seniority order.
    let (inv, _) = m.invite(A, B, 20).unwrap();
    m.accept(inv, B, 21).unwrap();
    assert_eq!(m.party_of(A).unwrap().members, vec![A, C, B]);
}

#[test]
fn kick_validation() {
    let mut m = trio();
    invite_accept(&mut m, D, E, 0); // a second, unrelated party
    let before = m.clone();
    assert_eq!(m.kick(C, A), Err(PartyError::NotLeader { account: C }));
    assert_eq!(m.kick(F, A), Err(PartyError::NotInParty { account: F }));
    assert_eq!(
        m.kick(A, A),
        Err(PartyError::CannotTargetSelf { account: A })
    );
    assert_eq!(
        m.kick(A, F),
        Err(PartyError::TargetNotInParty { account: F })
    );
    assert_eq!(
        m.kick(A, D),
        Err(PartyError::TargetNotInParty { account: D }),
        "other party"
    );
    assert_eq!(m, before);
}

// ----- promote -------------------------------------------------------------------------------

#[test]
fn promote_transfers_leadership_and_privileges() {
    let mut m = trio();
    let before = m.clone();
    assert_eq!(
        m.promote(A, A),
        Err(PartyError::AlreadyLeader { account: A })
    );
    assert_eq!(m.promote(B, C), Err(PartyError::NotLeader { account: B }));
    assert_eq!(m.promote(D, A), Err(PartyError::NotInParty { account: D }));
    assert_eq!(
        m.promote(A, D),
        Err(PartyError::TargetNotInParty { account: D })
    );
    assert_eq!(m, before);

    let ev = m.promote(A, B).unwrap();
    assert_eq!(
        ev,
        vec![PartyEvent::LeaderChanged {
            party: pid(1),
            old_leader: A,
            new_leader: B,
            reason: LeaderChangeReason::Promoted,
            members: vec![A, B, C],
        }]
    );
    assert_eq!(ev[0].audience(), vec![A, B, C]);
    assert!(m.is_leader(B) && !m.is_leader(A));
    assert_eq!(
        m.party_of(B).unwrap().members,
        vec![A, B, C],
        "roster order is untouched"
    );
    assert_eq!(m.kick(A, C), Err(PartyError::NotLeader { account: A }));
    assert!(m.kick(B, C).is_ok());
}

// ----- invite validation ---------------------------------------------------------------------

#[test]
fn invite_validation() {
    let mut m = mgr();
    assert_eq!(m.invite(A, A, 0), Err(PartyError::SelfInvite));
    invite_accept(&mut m, A, B, 0);
    let (c_inv, _) = m.invite(A, C, 1).unwrap();

    let before = m.clone();
    assert_eq!(
        m.invite(D, A, 1),
        Err(PartyError::AlreadyInParty { account: A })
    );
    assert_eq!(
        m.invite(A, B, 1),
        Err(PartyError::AlreadyInParty { account: B })
    );
    assert_eq!(m.invite(B, C, 1), Err(PartyError::NotLeader { account: B }));
    assert_eq!(
        m.invite(A, C, 2),
        Err(PartyError::DuplicateInvite { from: A, to: C })
    );
    assert_eq!(m, before);
    assert!(m.get_invite(c_inv).is_some());
}

#[test]
fn accept_is_refused_if_the_acceptor_joined_a_party_some_other_way() {
    // C holds invites from two different solo players; after accepting one, the other is
    // cancelled, so a later accept finds nothing.
    let mut m = mgr();
    let (from_a, _) = m.invite(A, C, 0).unwrap();
    let (from_d, _) = m.invite(D, C, 0).unwrap();
    m.accept(from_a, C, 1).unwrap();
    assert_eq!(
        m.accept(from_d, C, 2),
        Err(PartyError::UnknownInvite { invite: from_d })
    );
}

// ----- invite cancellation side effects ------------------------------------------------------

#[test]
fn accepting_cancels_the_acceptors_other_invites_but_not_the_leaders() {
    let mut m = mgr();
    let (e_to_b, _) = m.invite(E, B, 0).unwrap(); // id 1: incoming to B
    let (b_to_d, _) = m.invite(B, D, 0).unwrap(); // id 2: outgoing from B
    let (a_to_b, _) = m.invite(A, B, 0).unwrap(); // id 3
    let (a_to_c, _) = m.invite(A, C, 0).unwrap(); // id 4: A's other invite
    let ev = m.accept(a_to_b, B, 1).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::PartyFormed {
                party: pid(1),
                invite: a_to_b,
                leader: A,
                members: vec![A, B]
            },
            PartyEvent::InviteCancelled {
                invite: e_to_b,
                from: E,
                to: B,
                reason: InviteCancelReason::InviteeJoinedParty,
            },
            PartyEvent::InviteCancelled {
                invite: b_to_d,
                from: B,
                to: D,
                reason: InviteCancelReason::InviterCannotInvite,
            },
        ]
    );
    assert!(
        m.get_invite(a_to_c).is_some(),
        "the leader keeps their other invites"
    );
    assert_eq!(m.invite_count(), 1);
}

#[test]
fn mutual_invites_cancel_exactly_once() {
    let mut m = mgr();
    let (a_to_b, _) = m.invite(A, B, 0).unwrap(); // id 1
    let (b_to_a, _) = m.invite(B, A, 0).unwrap(); // id 2: hit by two causes at once
    let ev = m.accept(a_to_b, B, 1).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::PartyFormed {
                party: pid(1),
                invite: a_to_b,
                leader: A,
                members: vec![A, B]
            },
            PartyEvent::InviteCancelled {
                invite: b_to_a,
                from: B,
                to: A,
                reason: InviteCancelReason::InviteeJoinedParty,
            },
        ]
    );
    assert_eq!(m.invite_count(), 0);
}

#[test]
fn accepting_another_partys_invite_cancels_my_outgoing_invites() {
    let mut m = mgr();
    let (a_to_c, _) = m.invite(A, C, 0).unwrap(); // id 1: A is solo and invites C
    let (d_to_a, _) = m.invite(D, A, 0).unwrap(); // id 2
    let ev = m.accept(d_to_a, A, 1).unwrap(); // A joins D's brand new party as a member
    assert_eq!(
        ev,
        vec![
            PartyEvent::PartyFormed {
                party: pid(1),
                invite: d_to_a,
                leader: D,
                members: vec![D, A]
            },
            PartyEvent::InviteCancelled {
                invite: a_to_c,
                from: A,
                to: C,
                reason: InviteCancelReason::InviterCannotInvite,
            },
        ]
    );
}

#[test]
fn a_leader_leaving_cancels_their_pending_invites() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);
    let (a_to_c, _) = m.invite(A, C, 5).unwrap(); // id 2
    let ev = m.leave(A).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::LeaderChanged {
                party: pid(1),
                old_leader: A,
                new_leader: B,
                reason: LeaderChangeReason::LeaderLeft,
                members: vec![A, B],
            },
            PartyEvent::MemberLeft {
                party: pid(1),
                account: A,
                reason: LeaveReason::Left,
                members: vec![B],
            },
            PartyEvent::InviteCancelled {
                invite: a_to_c,
                from: A,
                to: C,
                reason: InviteCancelReason::InviterLeftParty,
            },
        ]
    );
    assert_eq!(
        m.accept(a_to_c, C, 6),
        Err(PartyError::UnknownInvite { invite: a_to_c })
    );
}

#[test]
fn disbanding_cancels_the_last_leaders_pending_invites() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);
    m.leave(B).unwrap();
    let (a_to_c, _) = m.invite(A, C, 5).unwrap(); // id 2
    let ev = m.leave(A).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::Disbanded {
                party: pid(1),
                members: vec![A],
                reason: DisbandReason::LastMemberLeft
            },
            PartyEvent::InviteCancelled {
                invite: a_to_c,
                from: A,
                to: C,
                reason: InviteCancelReason::InviterLeftParty,
            },
        ]
    );
}

#[test]
fn promoting_cancels_the_old_leaders_pending_invites() {
    let mut m = mgr();
    invite_accept(&mut m, A, B, 0);
    let (a_to_c, _) = m.invite(A, C, 5).unwrap(); // id 2
    let ev = m.promote(A, B).unwrap();
    assert_eq!(
        ev,
        vec![
            PartyEvent::LeaderChanged {
                party: pid(1),
                old_leader: A,
                new_leader: B,
                reason: LeaderChangeReason::Promoted,
                members: vec![A, B],
            },
            PartyEvent::InviteCancelled {
                invite: a_to_c,
                from: A,
                to: C,
                reason: InviteCancelReason::InviterCannotInvite,
            },
        ]
    );
}

// ----- whole-system scripted scenario under the full harness ---------------------------------

/// Drives every event kind through the [`Harness`], which checks atomic failure, replica
/// equality, audience exactness and `validate()` after every single call.
#[test]
fn scripted_scenario_exercises_every_event_kind_under_full_checks() {
    let universe = vec![A, B, C, D, E, F];
    let mut h = Harness::new(mgr(), universe);

    // invite / decline
    let (i1, _) = h.invite(A, B, 0).unwrap();
    h.decline(i1, B).unwrap();
    // invite / form / join / cap
    let (i2, _) = h.invite(A, B, 1).unwrap();
    let (i3, _) = h.invite(A, C, 1).unwrap();
    let (i4, _) = h.invite(A, D, 1).unwrap();
    h.accept(i2, B, 2).unwrap(); // PartyFormed
    h.accept(i3, C, 2).unwrap(); // MemberJoined, party now full
    assert!(h.accept(i4, D, 3).is_err()); // PartyFull
                                          // kick, re-invite, promote, leader leaves
    h.kick(A, C).unwrap();
    let (i5, _) = h.invite(A, C, 4).unwrap();
    h.accept(i5, C, 5).unwrap();
    h.promote(A, B).unwrap(); // cancels A's pending invite to D
    h.leave(B).unwrap(); // leader leaves: LeaderChanged + MemberLeft
                         // A leads [A, C] now. expiry:
    let (_i6, _) = h.invite(A, E, 10).unwrap();
    h.expire(20).unwrap(); // InviteExpired
                           // cancellation on accept, then disband
    let (i7, _) = h.invite(A, F, 21).unwrap();
    let (i8, _) = h.invite(E, F, 21).unwrap();
    h.accept(i7, F, 22).unwrap();
    assert!(h.accept(i8, F, 23).is_err()); // cancelled when F joined A's party
    h.leave(A).unwrap();
    h.leave(C).unwrap();
    h.leave(F).unwrap(); // Disbanded

    let expected = [
        "InviteSent",
        "InviteDeclined",
        "InviteExpired",
        "InviteCancelled:InviteeJoinedParty",
        "InviteCancelled:InviterCannotInvite",
        "PartyFormed",
        "MemberJoined",
        "MemberLeft:Left",
        "MemberLeft:Kicked",
        "LeaderChanged:Promoted",
        "LeaderChanged:LeaderLeft",
        "Disbanded",
    ];
    for kind in expected {
        assert!(
            h.events_seen.contains(kind),
            "scenario never produced {kind}: {:?}",
            h.events_seen
        );
    }
    assert_eq!(h.m.party_count(), 0);
}

#[test]
fn every_error_variant_leaves_state_untouched() {
    // Walk a manager through many refused calls and make sure the harness' atomicity check
    // sees none of them change anything.
    let mut h = Harness::new(mgr(), vec![A, B, C, D, E, F]);
    let (i, _) = h.invite(A, B, 0).unwrap();
    for result in [
        h.invite(A, A, 1).map(|_| ()),
        h.invite(A, B, 1).map(|_| ()),
        h.accept(i, C, 1).map(|_| ()),
        h.accept(iid(77), B, 1).map(|_| ()),
        h.accept(i, B, 10).map(|_| ()),
        h.decline(i, C).map(|_| ()),
        h.leave(F).map(|_| ()),
        h.kick(A, B).map(|_| ()),
        h.promote(A, B).map(|_| ()),
    ] {
        assert!(result.is_err());
    }
    for name in [
        "SelfInvite",
        "DuplicateInvite",
        "NotInvitee",
        "UnknownInvite",
        "InviteExpired",
        "NotInParty",
    ] {
        assert!(
            h.errors_seen.contains(name),
            "missing {name}: {:?}",
            h.errors_seen
        );
    }
}
