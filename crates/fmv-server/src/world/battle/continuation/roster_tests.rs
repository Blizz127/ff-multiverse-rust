//! Recovery checks for source-ordered companions and account-ordered carried resources.

use super::*;
use fmv_adapter_api::GameAdapter;
use fmv_core::GameId;
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_persist::{CharacterProgress, ProgressPair, Progression, SceneReservation, SceneState};
use fmv_protocol::{BattleRules, Status, StatusSet, SyntheticRules};
use crate::config::BattleSettings;

fn rules() -> SyntheticRules {
    SyntheticRules { player_limits: true, ..SyntheticRules::default() }
}

fn fixture() -> (World, SceneBattlePlan, SceneCombatCarry, Vec<SceneState>) {
    let rules = rules();
    let mut world = World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap();
    world.configure_battle(Some(BattleSettings::new(Arc::new(rules))), false);
    let party = vec![
        BattleMember::companion("Cloud", 0, 5, 0),
        BattleMember::companion("Barret", 1, 5, 0),
        // The same combat curve does not make either player the canon Cloud or each other.
        BattleMember::player(EntityId(0), AccountId(41), "One", 0, 5, 0),
        BattleMember::player(EntityId(0), AccountId(42), "Two", 0, 5, 0),
    ];
    let values = [
        BattleInitialState { hp: 31, mp: 11, status: StatusSet::EMPTY.with(Status::Sadness), limit: 10 },
        BattleInitialState { hp: 42, mp: 12, status: StatusSet::EMPTY.with(Status::Fury), limit: 20 },
        BattleInitialState { hp: 53, mp: 13, status: StatusSet::EMPTY, limit: 30 },
        BattleInitialState { hp: 64, mp: 14, status: StatusSet::EMPTY.with(Status::Sadness), limit: 40 },
    ];
    let carry = SceneCombatCarry { members: party.iter().cloned().zip(values).collect() };
    let inventories = vec![
        (AccountId(41), vec![InventoryEntry::new(0, 2)]),
        (AccountId(42), vec![InventoryEntry::new(0, 5)]),
    ];
    let states = inventories.iter().enumerate().map(|(slot, (account, inventory))| {
        let mut character = CharacterProgress::new(*account, GameId::Fixture, 0, 5, 0);
        character.exp = rules.starting_exp(0, 5);
        character.inventory = Some(inventory.clone());
        SceneState {
            pair: ProgressPair { story: Progression::empty(*account, GameId::Fixture), character: Some(character) },
            checkpoint: None,
            reservation: Some(SceneReservation { instance: [7; 32], ordinal: 0, attempt: 0, slot: slot as u16 }),
            field_visit: None,
        }
    }).collect();
    let plan = SceneBattlePlan {
        setup: BattleSetup {
            formation_id: 0,
            seed: 7,
            party,
            initial_party: None,
            escape_policy: EscapePolicy::Forbidden,
            victory_fanfare: false,
            game_over_on_defeat: true,
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 30,
        },
        fingerprint: rules.data_fingerprint(),
        inventories,
    };
    (world, plan, carry, states)
}

fn assert_resources(setup: &BattleSetup, expected: &[BattleInitialState]) {
    assert_eq!(setup.initial_party.as_deref(), Some(expected));
    let engine = rules().build(setup).expect("the real shared builder accepts the carried setup");
    assert_eq!(engine.party_ids().len(), expected.len());
    for (index, expected) in expected.iter().enumerate() {
        let actor = engine.actor(ActorId(index as u8)).unwrap();
        assert_eq!((actor.hp, actor.mp, actor.status.set(), actor.limit),
            (expected.hp, expected.mp, expected.status, expected.limit), "actor {index}");
    }
}

#[test]
fn canon_reordering_and_removal_preserve_each_actors_exact_resources() {
    let (_, plan, carry, _) = fixture();
    let value = |index: usize| carry.members[index].1;
    let players = plan.setup.party[2..].to_vec();
    let mut reordered = plan.setup.clone();
    reorder_canon(&mut reordered, &[1, 0]).unwrap();
    assert_eq!(&reordered.party[2..], players.as_slice(), "both player accounts keep their frozen order");
    apply_carry(&mut reordered, &carry, &rules()).unwrap();
    assert_resources(&reordered, &[value(1), value(0), value(2), value(3)]);

    let mut subset = plan.setup.clone();
    reorder_canon(&mut subset, &[0]).unwrap();
    apply_carry(&mut subset, &carry, &rules()).unwrap();
    assert_resources(&subset, &[value(0), value(2), value(3)]);

    let mut players_only = plan.setup.clone();
    reorder_canon(&mut players_only, &[]).unwrap();
    assert_eq!(players_only.party, players);
    apply_carry(&mut players_only, &carry, &rules()).unwrap();
    assert_resources(&players_only, &[value(2), value(3)]);
}

#[test]
fn a_missing_actor_cannot_borrow_from_canon_or_another_player_on_the_same_curve() {
    let (_, plan, carry, _) = fixture();
    for missing in [0, 2, 3] {
        let mut incomplete = carry.clone();
        incomplete.members.remove(missing);
        assert!(apply_carry(&mut plan.setup.clone(), &incomplete, &rules()).is_err(), "missing actor {missing}");
    }
    let mut duplicate = carry.clone();
    duplicate.members.push(carry.members[2].clone());
    assert!(apply_carry(&mut plan.setup.clone(), &duplicate, &rules()).is_err());

    let mut changed_curve = carry.clone();
    changed_curve.members[2].0.character = 1;
    assert!(apply_carry(&mut plan.setup.clone(), &changed_curve, &rules()).is_err());

    let mut lost_player = plan.setup.clone();
    lost_player.party.pop();
    assert!(apply_carry(&mut lost_player, &carry, &rules()).is_err(), "canon removal never authorizes losing a frozen account");
}

#[test]
fn canon_changes_cannot_create_duplicate_new_or_rejoining_actors() {
    let (_, plan, _, _) = fixture();
    for canon in [&[2][..], &[0, 0][..], &[0, 1, 2][..]] {
        assert!(reorder_canon(&mut plan.setup.clone(), canon).is_err(), "unsupported prefix {canon:?}");
    }
    let mut removed = plan.setup.clone();
    reorder_canon(&mut removed, &[1]).unwrap();
    assert!(reorder_canon(&mut removed, &[0, 1]).is_err(), "an earlier fight's removed actor has no current resources");

    let mut duplicate = plan.setup.clone();
    duplicate.party.insert(1, duplicate.party[0].clone());
    assert!(reorder_canon(&mut duplicate, &[0]).is_err());
}

#[test]
fn carried_metadata_and_order_must_describe_the_exact_previous_plan() {
    let (_, plan, carry, _) = fixture();
    assert!(carry_matches_plan(&plan, &carry));
    for (first, second) in [(0, 1), (2, 3)] {
        let mut permuted = carry.clone();
        permuted.members.swap(first, second);
        assert!(!carry_matches_plan(&plan, &permuted));
    }
    let mut renamed = carry.clone();
    renamed.members[2].0.name = "Other".to_string();
    assert!(!carry_matches_plan(&plan, &renamed));
    let mut advanced = carry.clone();
    advanced.members[2].0.level += 1;
    assert!(!carry_matches_plan(&plan, &advanced), "new levels come from durable records, not changed prior metadata");

    let mut duplicate_plan = plan.clone();
    duplicate_plan.setup.party[3] = duplicate_plan.setup.party[2].clone();
    let mut duplicate_carry = carry.clone();
    duplicate_carry.members[3].0 = duplicate_carry.members[2].0.clone();
    assert!(!carry_matches_plan(&duplicate_plan, &duplicate_carry));
}

#[test]
fn canonical_actors_must_match_the_source_prefix_and_never_follow_a_player() {
    let (world, plan, carry, states) = fixture();
    assert!(canon_prefix_matches(&plan.setup.party, Some(&[0, 1])));
    assert!(!canon_prefix_matches(&plan.setup.party, Some(&[1, 0])));
    assert!(!canon_prefix_matches(&plan.setup.party, Some(&[0])));
    assert!(!canon_prefix_matches(&plan.setup.party, Some(&[])));
    assert!(world.validate_scene_battle_plan(&plan, &states, Some(&[0, 1])));
    assert!(world.validate_scene_battle_resume(&plan, &carry, &states, Some(&[0, 1])));
    assert!(!world.validate_scene_battle_plan(&plan, &states, Some(&[1, 0])));

    let mut trailing = plan.clone();
    trailing.setup.party.swap(1, 2);
    assert!(!canon_prefix_matches(&trailing.setup.party, None), "legacy lookup still enforces a canonical prefix");
    assert!(reorder_canon(&mut trailing.setup.clone(), &[0, 1]).is_err());
    assert!(!world.validate_scene_battle_plan(&trailing, &states, None));
}

#[test]
fn persisted_slots_reject_jointly_reordered_players_carry_and_inventory() {
    let (world, plan, carry, states) = fixture();
    assert!(world.validate_scene_battle_plan(&plan, &states, Some(&[0, 1])));
    let mut permuted = plan.clone();
    permuted.setup.party.swap(2, 3);
    permuted.inventories.swap(0, 1);
    let mut permuted_carry = carry.clone();
    permuted_carry.members.swap(2, 3);
    let mut permuted_states = states.clone();
    permuted_states.swap(0, 1);
    assert!(carry_matches_plan(&permuted, &permuted_carry), "a self-consistent forgery still contradicts the saved slots");
    assert!(!world.validate_scene_battle_plan(&permuted, &permuted_states, Some(&[0, 1])));
    assert!(!world.validate_scene_battle_resume(&permuted, &permuted_carry, &permuted_states, Some(&[0, 1])));

    let mut carry_only = carry.clone();
    carry_only.members.swap(2, 3);
    assert!(!world.validate_scene_battle_resume(&plan, &carry_only, &states, Some(&[0, 1])));
}

#[test]
fn the_next_fight_uses_acknowledged_levels_and_bags_without_healing_or_reselecting_players() {
    let (mut world, plan, carry, mut states) = fixture();
    // No online players exist: the frozen roster, not a fresh nearby selection, supplies both accounts.
    let record = states[0].pair.character.as_mut().unwrap();
    record.level = 6;
    record.exp = rules().starting_exp(0, 6);
    record.inventory = Some(vec![InventoryEntry::new(0, 1)]);
    let next = world.prepare_next_scene_battle(&plan, 3, &carry, &states, Some(&[1, 0])).unwrap();
    assert_eq!(next.accounts(), plan.accounts());
    assert_eq!(next.setup.party.iter().map(|member| member.character).collect::<Vec<_>>(), [1, 0, 0, 0]);
    assert_eq!(next.setup.party.iter().map(|member| member.level).collect::<Vec<_>>(), [5, 5, 6, 5]);
    assert_eq!(next.inventories, vec![(AccountId(41), vec![InventoryEntry::new(0, 1)]), plan.inventories[1].clone()]);
    assert!(next.setup.preemptive);
    assert_resources(&next.setup, &[carry.members[1].1, carry.members[0].1, carry.members[2].1, carry.members[3].1]);
    assert!(world.validate_scene_battle_plan(&next, &states, Some(&[1, 0])));

    let subset = world.prepare_next_scene_battle(&plan, 3, &carry, &states, Some(&[0])).unwrap();
    assert_eq!(subset.accounts(), plan.accounts());
    assert_resources(&subset.setup, &[carry.members[0].1, carry.members[2].1, carry.members[3].1]);
    assert!(world.validate_scene_battle_plan(&subset, &states, Some(&[0])));
    let subset_carry = SceneCombatCarry {
        members: subset.setup.party.iter().cloned().zip(subset.setup.initial_party.clone().unwrap()).collect(),
    };
    assert!(world.prepare_next_scene_battle(&subset, 0, &subset_carry, &states, Some(&[0, 1])).is_err());
}

#[test]
fn the_next_fight_refuses_missing_state_missing_carry_and_unknown_companions() {
    let (mut world, plan, carry, states) = fixture();
    assert!(world.prepare_next_scene_battle(&plan, 3, &carry, &states[..1], Some(&[0, 1])).is_err());
    let mut incomplete = carry.clone();
    incomplete.members.pop();
    assert!(world.prepare_next_scene_battle(&plan, 3, &incomplete, &states, Some(&[0, 1])).is_err());
    assert!(world.prepare_next_scene_battle(&plan, 3, &carry, &states, Some(&[0, 2])).is_err());
    assert!(world.prepare_next_scene_battle(&plan, 3, &carry, &states, Some(&[0, 0])).is_err());
}

#[test]
fn recovered_initial_resources_must_equal_the_carry_even_when_altered_values_are_legal() {
    let (mut world, plan, carry, states) = fixture();
    assert!(world.validate_scene_battle_initial_carry(&plan, None));
    assert!(!world.validate_scene_battle_initial_carry(&plan, Some(&carry)), "a carried start cannot silently become a fresh start");
    for canon in [&[1, 0][..], &[0][..], &[][..]] {
        let next = world.prepare_next_scene_battle(&plan, 3, &carry, &states, Some(canon)).unwrap();
        assert!(world.validate_scene_battle_initial_carry(&next, Some(&carry)), "valid prefix {canon:?}");
        assert!(!world.validate_scene_battle_initial_carry(&next, None), "saved initial values require their carry evidence");
        for resource in 0..3 {
            let mut altered = next.clone();
            let index = altered.setup.party.len() - 1;
            let value = &mut altered.setup.initial_party.as_mut().unwrap()[index];
            match resource {
                0 => value.hp += 1,
                1 => value.mp += 1,
                _ => {
                    value.limit += 1;
                    // Keep the engine's duplicate gauge metadata coherent too: this is
                    // a bounded but unauthorised resource change, not malformed setup.
                    altered.setup.party[index].limit = value.limit;
                }
            }
            assert!(rules().build(&altered.setup).is_ok());
            assert!(!world.validate_scene_battle_initial_carry(&altered, Some(&carry)), "resource {resource}, prefix {canon:?}");
        }
    }
}
