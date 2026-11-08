//! Instance selection (solo vs party) and the instance registry (create on demand, membership,
//! release when empty), plus an end-to-end run combining parties, selection and the registry.

mod common;

use common::*;
use fmv_core::{AccountId, AreaId, GameId, PhaseKey};
use fmv_party::{
    InstanceEvent, InstanceId, InstanceKey, InstanceRegistry, InstanceSelector, MemberPlacement,
    PartyConfig, PartyManager, PhaseMap, PhasingError, Placement,
};
use serde_json::json;

fn area(key: &str) -> AreaId {
    AreaId {
        game: GameId::Fixture,
        key: key.to_string(),
    }
}

fn ph(bytes: &[u8]) -> PhaseKey {
    PhaseKey(bytes.to_vec())
}

fn phases(entries: &[(AccountId, &[u8])]) -> PhaseMap {
    entries.iter().map(|(a, p)| (*a, ph(p))).collect()
}

fn key(area_key: &str, phase: &[u8]) -> InstanceKey {
    InstanceKey::new(area(area_key), ph(phase))
}

fn iid_(n: u64) -> InstanceId {
    InstanceId(n)
}

/// Party 1 led by A with B, C, D (cap 4), joined in that order.
fn quartet() -> PartyManager {
    let mut m = PartyManager::new(PartyConfig::new(4, 100).unwrap());
    for who in [B, C, D] {
        let (i, _) = m.invite(A, who, 0).unwrap();
        m.accept(i, who, 0).unwrap();
    }
    m
}

// ----- selector ------------------------------------------------------------------------------

#[test]
fn solo_player_gets_their_own_phase() {
    let p = InstanceSelector::select(&area("plaza"), A, None, &phases(&[(A, b"p2")])).unwrap();
    assert_eq!(
        p,
        Placement {
            account: A,
            key: key("plaza", b"p2"),
            party: None,
            phase_owner: A,
            story_advance_allowed: true,
        }
    );
}

#[test]
fn party_shares_the_leaders_phase_and_members_keep_their_own() {
    let m = quartet();
    let party = m.party_of(A).unwrap();
    // A (leader) is at "mid"; B is behind, C is ahead (core only sees "different"), D matches.
    let map = phases(&[(A, b"mid"), (B, b"early"), (C, b"late"), (D, b"mid")]);
    let map_before = map.clone();

    let shared = InstanceSelector::select_party(&area("x"), party, &map).unwrap();
    assert_eq!(shared.key, key("x", b"mid"));
    assert_eq!((shared.party, shared.leader), (pid(1), A));
    assert_eq!(
        shared.members,
        vec![
            MemberPlacement {
                account: A,
                story_advance_allowed: true
            },
            MemberPlacement {
                account: B,
                story_advance_allowed: false
            },
            MemberPlacement {
                account: C,
                story_advance_allowed: false
            },
            MemberPlacement {
                account: D,
                story_advance_allowed: true
            },
        ]
    );

    // Selecting for one player at a time agrees with the whole-party answer.
    for member in &shared.members {
        let p = InstanceSelector::select(&area("x"), member.account, Some(party), &map).unwrap();
        assert_eq!(p.key, shared.key);
        assert_eq!(p.party, Some(pid(1)));
        assert_eq!(p.phase_owner, A);
        assert_eq!(p.story_advance_allowed, member.story_advance_allowed);
    }

    // Selection never touches anyone's progression: the phases are only read.
    assert_eq!(map, map_before);

    // The same behind-the-leader player, solo, would land in their *own* instance.
    let solo = InstanceSelector::select(&area("x"), B, None, &map).unwrap();
    assert_eq!(solo.key, key("x", b"early"));
    assert!(solo.story_advance_allowed);
    assert_ne!(solo.key, shared.key);
}

#[test]
fn the_default_policy_hook_is_phase_equality() {
    assert!(InstanceSelector::story_advance_allowed(
        &ph(b"a"),
        &ph(b"a")
    ));
    assert!(!InstanceSelector::story_advance_allowed(
        &ph(b"a"),
        &ph(b"b")
    ));
    assert!(!InstanceSelector::story_advance_allowed(
        &ph(b"b"),
        &ph(b"a")
    ));
}

#[test]
fn new_leader_moves_the_party_to_their_phase() {
    let mut m = quartet();
    let map = phases(&[(A, b"mid"), (B, b"early"), (C, b"late"), (D, b"mid")]);
    m.promote(A, B).unwrap();
    let shared = InstanceSelector::select_party(&area("x"), m.party_of(B).unwrap(), &map).unwrap();
    assert_eq!(shared.key, key("x", b"early"));
    assert_eq!(shared.leader, B);
    let flags: Vec<(AccountId, bool)> = shared
        .members
        .iter()
        .map(|p| (p.account, p.story_advance_allowed))
        .collect();
    assert_eq!(flags, vec![(A, false), (B, true), (C, false), (D, false)]);
}

#[test]
fn only_the_leader_and_the_player_need_a_phase_for_single_selection() {
    let m = quartet();
    let party = m.party_of(A).unwrap();
    let map = phases(&[(A, b"mid"), (C, b"late")]); // B and D omitted on purpose
    let p = InstanceSelector::select(&area("x"), C, Some(party), &map).unwrap();
    assert_eq!(p.key, key("x", b"mid"));
    assert!(!p.story_advance_allowed);
}

#[test]
fn selection_errors_are_values_not_panics() {
    let m = quartet();
    let party = m.party_of(A).unwrap();
    let full = phases(&[(A, b"x"), (B, b"x"), (C, b"x"), (D, b"x")]);

    // solo without a phase
    assert_eq!(
        InstanceSelector::select(&area("x"), E, None, &full),
        Err(PhasingError::MissingPhase { account: E })
    );
    // party member without a phase
    let no_c = phases(&[(A, b"x"), (B, b"x"), (D, b"x")]);
    assert_eq!(
        InstanceSelector::select(&area("x"), C, Some(party), &no_c),
        Err(PhasingError::MissingPhase { account: C })
    );
    // leader's phase missing
    let no_leader = phases(&[(B, b"x"), (C, b"x"), (D, b"x")]);
    assert_eq!(
        InstanceSelector::select(&area("x"), B, Some(party), &no_leader),
        Err(PhasingError::MissingPhase { account: A })
    );
    // whole-party selection needs everyone
    assert_eq!(
        InstanceSelector::select_party(&area("x"), party, &no_c),
        Err(PhasingError::MissingPhase { account: C })
    );
    // the named player is not in that party
    assert_eq!(
        InstanceSelector::select(&area("x"), E, Some(party), &full),
        Err(PhasingError::NotInParty {
            account: E,
            party: pid(1)
        })
    );
}

#[test]
fn different_areas_never_share_a_key() {
    let map = phases(&[(A, b"x")]);
    let a = InstanceSelector::select(&area("one"), A, None, &map).unwrap();
    let b = InstanceSelector::select(&area("two"), A, None, &map).unwrap();
    assert_ne!(a.key, b.key);
    let other_game = AreaId {
        game: GameId::Ff7,
        key: "one".to_string(),
    };
    let c = InstanceSelector::select(&other_game, A, None, &map).unwrap();
    assert_ne!(
        a.key, c.key,
        "same area key in another game is a different area"
    );
}

#[test]
fn m1_free_roam_single_phase_means_one_shared_instance_and_every_flag_true() {
    let m = quartet();
    let party = m.party_of(A).unwrap();
    let free: PhaseMap = [A, B, C, D]
        .into_iter()
        .map(|a| (a, PhaseKey::default()))
        .collect();
    let shared = InstanceSelector::select_party(&area("md1stin"), party, &free).unwrap();
    assert!(shared.members.iter().all(|p| p.story_advance_allowed));
    let solo = InstanceSelector::select(&area("md1stin"), E, None, &phases(&[(E, b"")])).unwrap();
    assert_eq!(
        solo.key, shared.key,
        "a solo player and a party share the one free-roam instance"
    );
}

// ----- registry ------------------------------------------------------------------------------

#[test]
fn instances_are_created_on_demand_and_shared() {
    let mut r = InstanceRegistry::new();
    let k = key("plaza", b"");
    assert!(r.is_empty() && r.find(&k).is_none());

    let ev = r.enter(A, &k).unwrap();
    assert_eq!(
        ev,
        vec![
            InstanceEvent::Created {
                instance: iid_(1),
                key: k.clone()
            },
            InstanceEvent::Entered {
                instance: iid_(1),
                account: A
            },
        ]
    );
    let ev = r.enter(B, &k).unwrap();
    assert_eq!(
        ev,
        vec![InstanceEvent::Entered {
            instance: iid_(1),
            account: B
        }]
    );

    assert_eq!(r.len(), 1);
    assert_eq!(r.find(&k), Some(iid_(1)));
    let inst = r.get(iid_(1)).unwrap();
    assert_eq!(inst.key(), &k);
    assert_eq!(inst.occupants().collect::<Vec<_>>(), vec![A, B]);
    assert_eq!(
        (inst.occupant_count(), inst.contains(A), inst.contains(C)),
        (2, true, false)
    );
    assert_eq!(
        (r.instance_of(A), r.instance_of(B), r.instance_of(C)),
        (Some(iid_(1)), Some(iid_(1)), None)
    );

    assert_eq!(
        r.enter(A, &k).unwrap(),
        vec![],
        "re-entering your own instance is a no-op"
    );
    r.validate().unwrap();
}

#[test]
fn distinct_phases_and_areas_get_distinct_instances() {
    let mut r = InstanceRegistry::new();
    r.enter(A, &key("plaza", b"x")).unwrap();
    r.enter(B, &key("plaza", b"y")).unwrap(); // same area, other phase
    r.enter(C, &key("alley", b"x")).unwrap(); // other area, same phase
    assert_eq!(r.len(), 3);
    let ids = [r.instance_of(A), r.instance_of(B), r.instance_of(C)];
    assert_eq!(ids, [Some(iid_(1)), Some(iid_(2)), Some(iid_(3))]);
    assert_eq!(
        r.instances().map(|i| i.key().clone()).collect::<Vec<_>>(),
        vec![key("plaza", b"x"), key("plaza", b"y"), key("alley", b"x")]
    );
}

#[test]
fn an_instance_is_released_when_its_last_occupant_leaves() {
    let mut r = InstanceRegistry::new();
    let k = key("plaza", b"x");
    r.enter(A, &k).unwrap();
    r.enter(B, &k).unwrap();

    assert_eq!(
        r.leave(A).unwrap(),
        vec![InstanceEvent::Left {
            instance: iid_(1),
            account: A
        }]
    );
    assert_eq!(r.len(), 1, "still occupied");
    assert_eq!(
        r.leave(B).unwrap(),
        vec![
            InstanceEvent::Left {
                instance: iid_(1),
                account: B
            },
            InstanceEvent::Released {
                instance: iid_(1),
                key: k.clone()
            },
        ]
    );
    assert!(r.is_empty() && r.find(&k).is_none() && r.get(iid_(1)).is_none());
    assert_eq!(r.instance_of(B), None);

    // Ids are never reused.
    let ev = r.enter(A, &k).unwrap();
    assert_eq!(
        ev[0],
        InstanceEvent::Created {
            instance: iid_(2),
            key: k.clone()
        }
    );

    // Leaving without being placed is an error value.
    assert_eq!(r.leave(C), Err(PhasingError::NotInInstance { account: C }));
    r.validate().unwrap();
}

#[test]
fn moving_between_instances_leaves_then_enters() {
    let mut r = InstanceRegistry::new();
    let (k1, k2) = (key("plaza", b"x"), key("plaza", b"y"));
    r.enter(A, &k1).unwrap();
    // A is alone in k1: moving to k2 releases k1 and creates k2.
    assert_eq!(
        r.enter(A, &k2).unwrap(),
        vec![
            InstanceEvent::Left {
                instance: iid_(1),
                account: A
            },
            InstanceEvent::Released {
                instance: iid_(1),
                key: k1.clone()
            },
            InstanceEvent::Created {
                instance: iid_(2),
                key: k2.clone()
            },
            InstanceEvent::Entered {
                instance: iid_(2),
                account: A
            },
        ]
    );
    // B joins k2, then A moves back to k1: k2 stays (B), k1 is created anew with a fresh id.
    r.enter(B, &k2).unwrap();
    assert_eq!(
        r.enter(A, &k1).unwrap(),
        vec![
            InstanceEvent::Left {
                instance: iid_(2),
                account: A
            },
            InstanceEvent::Created {
                instance: iid_(3),
                key: k1.clone()
            },
            InstanceEvent::Entered {
                instance: iid_(3),
                account: A
            },
        ]
    );
    assert_eq!(r.len(), 2);
    assert_eq!(r.instance_of(B), Some(iid_(2)));
    // Moving into an existing instance creates nothing.
    assert_eq!(
        r.enter(B, &k1).unwrap(),
        vec![
            InstanceEvent::Left {
                instance: iid_(2),
                account: B
            },
            InstanceEvent::Released {
                instance: iid_(2),
                key: k2
            },
            InstanceEvent::Entered {
                instance: iid_(3),
                account: B
            },
        ]
    );
    r.validate().unwrap();
}

#[test]
fn registry_round_trips_and_rejects_corrupt_state() {
    let mut r = InstanceRegistry::new();
    r.enter(A, &key("plaza", b"x")).unwrap();
    r.enter(B, &key("plaza", b"x")).unwrap();
    r.enter(C, &key("alley", b"y")).unwrap();
    r.leave(C).unwrap(); // releases instance 2 so the counter is ahead of the live ids

    let text = serde_json::to_string(&r).unwrap();
    let mut back: InstanceRegistry = serde_json::from_str(&text).unwrap();
    assert_eq!(back, r);
    back.validate().unwrap();
    // The restored counter continues where it left off.
    assert_eq!(
        back.enter(D, &key("alley", b"y")).unwrap()[0],
        InstanceEvent::Created {
            instance: iid_(3),
            key: key("alley", b"y")
        }
    );

    let good = serde_json::to_value(&r).unwrap();
    let area_json = serde_json::to_value(area("plaza")).unwrap();
    let k = json!({ "area": area_json, "phase": [120] });
    let corrupt = [
        ("empty instance", {
            let mut v = good.clone();
            v["instances"][0]["occupants"] = json!([]);
            v
        }),
        ("account in two instances", {
            let mut v = good.clone();
            v["instances"] = json!([
                { "id": 1, "key": k, "occupants": [1, 2] },
                { "id": 2, "key": { "area": area_json, "phase": [121] }, "occupants": [1] }
            ]);
            v["next_instance"] = json!(3);
            v
        }),
        ("duplicate occupant", {
            let mut v = good.clone();
            v["instances"][0]["occupants"] = json!([1, 1]);
            v
        }),
        ("two instances share a key", {
            let mut v = good.clone();
            v["instances"] = json!([
                { "id": 1, "key": k, "occupants": [1] },
                { "id": 2, "key": k, "occupants": [2] }
            ]);
            v["next_instance"] = json!(3);
            v
        }),
        ("instance id not below counter", {
            let mut v = good.clone();
            v["next_instance"] = json!(1);
            v
        }),
    ];
    for (name, v) in corrupt {
        assert!(
            serde_json::from_value::<InstanceRegistry>(v).is_err(),
            "accepted: {name}"
        );
    }
}

#[test]
fn registry_id_exhaustion_is_an_error() {
    let mut r: InstanceRegistry = serde_json::from_value(json!({
        "next_instance": u64::MAX, "instances": []
    }))
    .unwrap();
    let before = r.clone();
    assert_eq!(
        r.enter(A, &key("plaza", b"x")),
        Err(PhasingError::IdSpaceExhausted)
    );
    assert_eq!(r, before);
}

// ----- parties + selection + registry together -----------------------------------------------

#[test]
fn party_members_land_in_the_leaders_instance_end_to_end() {
    let mut parties = PartyManager::new(PartyConfig::new(3, 100).unwrap());
    let (i, _) = parties.invite(A, B, 0).unwrap();
    parties.accept(i, B, 0).unwrap(); // A leads [A, B]

    // A is at phase "x"; B and a solo stranger C are both at phase "y".
    let map = phases(&[(A, b"x"), (B, b"y"), (C, b"y")]);
    let plaza = area("plaza");
    let mut registry = InstanceRegistry::new();

    let place = |registry: &mut InstanceRegistry, parties: &PartyManager, who| {
        let p = InstanceSelector::select(&plaza, who, parties.party_of(who), &map).unwrap();
        registry.enter(who, &p.key).unwrap();
        p
    };
    let pa = place(&mut registry, &parties, A);
    let pb = place(&mut registry, &parties, B);
    let pc = place(&mut registry, &parties, C);

    // B is "ahead/behind" the leader but still shares the leader's instance and keeps flag false.
    assert_eq!(registry.instance_of(A), registry.instance_of(B));
    assert!(pa.story_advance_allowed && !pb.story_advance_allowed);
    // The solo stranger at B's own phase is in a different instance.
    assert_ne!(registry.instance_of(C), registry.instance_of(A));
    assert!(pc.story_advance_allowed);
    assert_eq!(registry.len(), 2);

    // B leaves the party and re-enters as a solo player: now they meet C at phase "y".
    parties.leave(B).unwrap();
    place(&mut registry, &parties, B);
    assert_eq!(registry.instance_of(B), registry.instance_of(C));
    assert_ne!(registry.instance_of(B), registry.instance_of(A));
    // A is alone in the "x" instance; when A leaves, that instance is released.
    let events = registry.leave(A).unwrap();
    assert!(matches!(
        events.last(),
        Some(InstanceEvent::Released { .. })
    ));
    assert_eq!(registry.len(), 1);
    registry.validate().unwrap();
}
