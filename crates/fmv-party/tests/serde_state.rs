//! Persistence and replication: the state and the events are serde-serializable, the persisted
//! shape is stable, and corrupt persisted state is rejected instead of being loaded.

mod common;

use common::*;
use fmv_core::AccountId;
use fmv_party::{
    DisbandReason, InviteCancelReason, LeaderChangeReason, LeaveReason, PartyConfig, PartyError,
    PartyEvent, PartyManager,
};
use serde_json::{json, Value};

/// Two parties and one pending invite. Next party id 3, next invite id 5.
fn rich_manager() -> PartyManager {
    let mut m = PartyManager::new(PartyConfig::new(3, 10).unwrap());
    let (i, _) = m.invite(A, B, 0).unwrap();
    m.accept(i, B, 0).unwrap();
    let (i, _) = m.invite(A, C, 0).unwrap();
    m.accept(i, C, 0).unwrap();
    let (i, _) = m.invite(D, E, 0).unwrap();
    m.accept(i, E, 0).unwrap();
    m.invite(F, AccountId(7), 0).unwrap();
    m
}

fn base_json() -> Value {
    json!({
        "config": { "max_size": 3, "invite_ttl": 10 },
        "next_party": 3,
        "next_invite": 5,
        "parties": [
            { "id": 1, "leader": 1, "members": [1, 2, 3] },
            { "id": 2, "leader": 4, "members": [4, 5] }
        ],
        "invites": [
            { "id": 4, "from": 6, "to": 7, "created_at": 0, "expires_at": 10 }
        ]
    })
}

fn load(v: Value) -> Result<PartyManager, serde_json::Error> {
    serde_json::from_value(v)
}

#[test]
fn persisted_shape_is_plain_sequences_in_id_order() {
    // This literal is the persistence contract: sequences (no map keys), id order, counters.
    assert_eq!(serde_json::to_value(rich_manager()).unwrap(), base_json());
}

#[test]
fn manager_round_trips_and_then_behaves_identically() {
    let mut original = rich_manager();
    let mut restored: PartyManager =
        serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    restored.validate().unwrap();

    // Same follow-up operations produce the same events and the same state, including ids
    // allocated from the restored counters.
    for step in 0..3 {
        let (a, b) = match step {
            0 => (original.leave(A), restored.leave(A)),
            1 => (original.expire(100), restored.expire(100)),
            _ => (original.kick(D, E), restored.kick(D, E)),
        };
        assert_eq!(a, b);
    }
    let (ia, ea) = original.invite(D, AccountId(8), 200).unwrap();
    let (ib, eb) = restored.invite(D, AccountId(8), 200).unwrap();
    assert_eq!((ia, ea), (ib, eb));
    assert_eq!(restored, original);
}

#[test]
fn corrupt_persisted_state_is_rejected() {
    assert!(
        load(base_json()).is_ok(),
        "the unmodified base state must load"
    );

    type Mutation = (&'static str, fn(&mut Value));
    let mutations: &[Mutation] = &[
        ("account in two parties", |v| {
            v["parties"][1]["members"] = json!([4, 5, 1])
        }),
        ("duplicate member inside a party", |v| {
            v["parties"][1]["members"] = json!([4, 4, 5])
        }),
        ("leader is not a member", |v| {
            v["parties"][0]["leader"] = json!(9)
        }),
        ("empty party", |v| v["parties"][1]["members"] = json!([])),
        ("party above max_size", |v| {
            v["parties"][0]["members"] = json!([1, 2, 3, 8])
        }),
        ("party id not below the counter", |v| {
            v["next_party"] = json!(2)
        }),
        ("duplicate party id", |v| v["parties"][1]["id"] = json!(1)),
        ("invite from an account to itself", |v| {
            v["invites"][0]["to"] = json!(6)
        }),
        ("invite to an account already in a party", |v| {
            v["invites"][0]["to"] = json!(1)
        }),
        ("invite sent by a non-leader member", |v| {
            v["invites"][0]["from"] = json!(2)
        }),
        ("invite id not below the counter", |v| {
            v["next_invite"] = json!(4)
        }),
        ("invite expires before it was created", |v| {
            v["invites"][0]["created_at"] = json!(5);
            v["invites"][0]["expires_at"] = json!(1);
        }),
        ("duplicate invite id", |v| {
            v["invites"] = json!([
                { "id": 4, "from": 6, "to": 7, "created_at": 0, "expires_at": 10 },
                { "id": 4, "from": 7, "to": 6, "created_at": 0, "expires_at": 10 }
            ]);
        }),
        ("two invites for one (from, to) pair", |v| {
            v["invites"] = json!([
                { "id": 3, "from": 6, "to": 7, "created_at": 0, "expires_at": 10 },
                { "id": 4, "from": 6, "to": 7, "created_at": 0, "expires_at": 10 }
            ]);
        }),
        ("max_size below the minimum", |v| {
            v["config"]["max_size"] = json!(1)
        }),
        ("zero invite ttl", |v| v["config"]["invite_ttl"] = json!(0)),
        ("missing field", |v| {
            v.as_object_mut().unwrap().remove("parties");
        }),
    ];
    for (name, mutate) in mutations {
        let mut v = base_json();
        mutate(&mut v);
        assert!(load(v).is_err(), "corrupt state was accepted: {name}");
    }
}

#[test]
fn id_space_exhaustion_is_an_error_not_a_panic() {
    // Invite counter at the end of the id space.
    let mut m = load(json!({
        "config": { "max_size": 3, "invite_ttl": 10 },
        "next_party": 1, "next_invite": u64::MAX, "parties": [], "invites": []
    }))
    .unwrap();
    let before = m.clone();
    assert_eq!(m.invite(A, B, 0), Err(PartyError::IdSpaceExhausted));
    assert_eq!(m, before);

    // Party counter at the end of the id space: forming fails atomically, the invite survives.
    let mut m = load(json!({
        "config": { "max_size": 3, "invite_ttl": 10 },
        "next_party": u64::MAX, "next_invite": 5, "parties": [],
        "invites": [{ "id": 1, "from": 1, "to": 2, "created_at": 0, "expires_at": 100 }]
    }))
    .unwrap();
    let before = m.clone();
    assert_eq!(
        m.accept(fmv_party::InviteId(1), B, 0),
        Err(PartyError::IdSpaceExhausted)
    );
    assert_eq!(m, before);
    assert!(m.get_invite(fmv_party::InviteId(1)).is_some());
}

#[test]
fn every_event_variant_round_trips_through_json() {
    use fmv_party::{InviteId as I, PartyId as P};
    let members = vec![A, B];
    let mut events = vec![
        PartyEvent::InviteSent {
            invite: I(1),
            from: A,
            to: B,
            expires_at: 9,
        },
        PartyEvent::InviteDeclined {
            invite: I(1),
            from: A,
            to: B,
        },
        PartyEvent::InviteExpired {
            invite: I(1),
            from: A,
            to: B,
        },
        PartyEvent::PartyFormed {
            party: P(1),
            invite: I(1),
            leader: A,
            members: members.clone(),
        },
        PartyEvent::MemberJoined {
            party: P(1),
            invite: I(2),
            account: C,
            leader: A,
            members: vec![A, B, C],
        },
        PartyEvent::Disbanded {
            party: P(1),
            members: vec![A],
            reason: DisbandReason::LastMemberLeft,
        },
    ];
    for reason in [
        InviteCancelReason::InviteeJoinedParty,
        InviteCancelReason::InviterLeftParty,
        InviteCancelReason::InviterCannotInvite,
    ] {
        events.push(PartyEvent::InviteCancelled {
            invite: I(3),
            from: A,
            to: B,
            reason,
        });
    }
    for reason in [LeaveReason::Left, LeaveReason::Kicked] {
        events.push(PartyEvent::MemberLeft {
            party: P(1),
            account: B,
            reason,
            members: vec![A],
        });
    }
    for reason in [LeaderChangeReason::Promoted, LeaderChangeReason::LeaderLeft] {
        events.push(PartyEvent::LeaderChanged {
            party: P(1),
            old_leader: A,
            new_leader: B,
            reason,
            members: members.clone(),
        });
    }
    for event in &events {
        let text = serde_json::to_string(event).unwrap();
        let back: PartyEvent = serde_json::from_str(&text).unwrap();
        assert_eq!(&back, event, "round trip changed {text}");
    }
    // Spot-check the wire shape clients will see.
    assert_eq!(
        serde_json::to_value(&events[1]).unwrap(),
        json!({ "InviteDeclined": { "invite": 1, "from": 1, "to": 2 } })
    );
}

#[test]
fn errors_and_config_round_trip_so_a_server_can_reply_with_them() {
    let errors = [
        PartyError::SelfInvite,
        PartyError::PartyFull {
            party: fmv_party::PartyId(2),
            max_size: 3,
        },
        PartyError::NotInvitee {
            invite: fmv_party::InviteId(4),
            account: A,
        },
        PartyError::InvalidConfig("x".into()),
        PartyError::IdSpaceExhausted,
    ];
    for e in errors {
        let back: PartyError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
    let cfg = PartyConfig::new(4, 123).unwrap();
    let back: PartyConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(back, cfg);
}
