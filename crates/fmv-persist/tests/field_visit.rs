//! Durable visit state through the public storage API. Every payload is synthetic and
//! every store is temporary; game interpretation and runtime activation are tested elsewhere.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    restore_snapshot, CharacterProgress, FieldVisitEntryV1, FieldVisitOperation,
    FieldVisitOperationKind, FieldVisitV1, PersistError, Progression, SceneCheckpoint, SceneCommit,
    SceneOperation, SceneOperationKind, ScenePhase, SceneReservation, SceneState, Store,
    MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES,
};
use serde_json::json;

const GAME: GameId = GameId::Fixture;

fn operation(id: u8, kind: FieldVisitOperationKind) -> FieldVisitOperation {
    FieldVisitOperation { id: [id; 16], kind }
}

fn visit(id: u8, entry: u8, field: &str) -> FieldVisitV1 {
    FieldVisitV1 {
        schema_version: 1,
        visit_id: [id; 16],
        revision: 0,
        field: field.into(),
        source_identity: [9; 32],
        entry: FieldVisitEntryV1 {
            operation: [entry; 16],
            arrival: br#"{"arrival":[10,20,0],"facing":0,"triangle":2}"#.to_vec(),
        },
        anchor: br#"{"profile":"fixture-ready-v1","position":[12,20,0],"facing":0,"triangle":2}"#.to_vec(),
        scratch_values: [0; 256],
        // This synthetic entry explicitly ran a known zero initializer. An absent visit
        // is never used as evidence that a real field's initializer ran.
        scratch_known_masks: [u8::MAX; 256],
        active_continuation: None,
    }
}

fn seeded(store: &Store, name: &str, affection: u8) -> AccountId {
    let (account, _) = store.create_account(name, NOW).unwrap();
    store.put_progression(&Progression::new(account, GAME, vec![4, affection, 61, 27], NOW)).unwrap();
    store.put_character_progress(&CharacterProgress {
        exp: 1_234,
        ap: 8,
        gil: 240,
        limit: 17,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1)]),
        ..CharacterProgress::new(account, GAME, 2, 7, NOW)
    }).unwrap();
    account
}

fn enter(store: &Store, account: AccountId, id: u8, field: &str) -> SceneState {
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut next = before.clone();
    next.field_visit = Some(visit(id + 10, id, field));
    store.commit_field_visit(&operation(id, FieldVisitOperationKind::Enter), &before, &next).unwrap().current
}

fn next_revision(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    next.field_visit.as_mut().unwrap().revision += 1;
    next
}

fn comparable(mut state: SceneState) -> SceneState {
    // The private envelope stamps its schema when metadata is first added. It changes
    // no gameplay value, timestamp, visit field or opaque story byte.
    if let Some(character) = &mut state.pair.character { character.schema_version = 1; }
    state
}

fn character_path(root: &Path, account: AccountId) -> PathBuf {
    root.join("progression").join(account.0.to_string()).join("Fixture.character.json")
}

#[test]
fn legacy_absence_remains_absent_until_an_explicit_entry_and_reopen_keeps_its_exact_visit() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Legacy", 8);
    let before = store.get_scene_state(account, GAME).unwrap();
    assert!(before.field_visit.is_none());
    let files = file_map(&root);
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
    assert_eq!(file_map(&root), files, "opening cannot invent a zeroed visit or change its compatibility");
    assert_eq!(log_lines(&root)[0]["schema_version"], 2);

    let saved = enter(&store, account, 1, "tin_1");
    assert_eq!(saved.field_visit, Some(visit(11, 1, "tin_1")));
    assert_eq!(comparable(saved.clone()).pair, comparable(before).pair);
    assert_eq!(saved.pair.character.as_ref().unwrap().schema_version, 4);
    assert_eq!(log_lines(&root)[0]["schema_version"], 5);
    let encoded = read_json(&character_path(&root, account));
    assert_eq!(encoded["field_visit"]["scratch_values"], "00".repeat(256));
    assert_eq!(encoded["field_visit"]["scratch_known_masks"], "ff".repeat(256));
    let files = file_map(&root);
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), saved);
    assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![saved]);
    assert_eq!(file_map(&root), files);
}

#[test]
fn each_prevalidated_jessie_answer_commits_affection_and_the_visit_latch_together() {
    // Persistence treats these bytes as opaque. This models the two adapter-validated
    // outcomes, including saturation, without pretending to execute the owned game script.
    for affection in [0u8, 2, 8] {
        for answer in [2u8, 3] {
            let (_dir, root) = fresh_root();
            let store = open(&root);
            let account = seeded(&store, "Jessie fixture", affection);
            let before = enter(&store, account, 1, "tin_1");
            let mut next = next_revision(&before);
            next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
            next.pair.story.blob[1] = if answer == 3 { affection.saturating_sub(3) } else { affection };
            next.pair.story.updated_at = NOW + 1;
            let op = operation(2, FieldVisitOperationKind::Advance);
            let saved = store.commit_field_visit(&op, &before, &next).unwrap();
            assert_eq!(saved.commit, SceneCommit::Applied);
            assert_eq!(saved.current, next);
            assert_eq!(saved.current.pair.character, before.pair.character, "a conversation does not normalize unrelated resources");
            assert_eq!(&saved.current.pair.story.blob[2..], &[61, 27], "unrelated opaque story/party bytes survive");
            drop(store);

            let store = open(&root);
            let current = store.get_scene_state(account, GAME).unwrap();
            assert_eq!(current, next, "both the affection result and latch survive restart");
            let bytes = file_map(&root);
            let duplicate = store.commit_field_visit(&op, &before, &next).unwrap();
            assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
            assert_eq!(duplicate.current, next);
            assert_eq!(file_map(&root), bytes, "duplicate completion applies no second deduction");

            // The adapter's one-window repeat outcome preserves the already set latch
            // and affection. A distinct accepted interaction advances only the visit revision.
            let repeat = next_revision(&current);
            let repeated = store.commit_field_visit(&operation(3, FieldVisitOperationKind::Advance), &current, &repeat).unwrap();
            assert_eq!(repeated.current.pair, current.pair);
            assert_eq!(repeated.current.field_visit.as_ref().unwrap().scratch_values[31], 1);
        }
    }
}

#[test]
fn entry_retry_uses_the_original_request_before_the_storage_envelope_was_upgraded() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Original entry request", 8);
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut next = before.clone();
    next.field_visit = Some(visit(11, 1, "tin_1"));
    let op = operation(1, FieldVisitOperationKind::Enter);
    let saved = store.commit_field_visit(&op, &before, &next).unwrap();
    assert_eq!(next.pair.character.as_ref().unwrap().schema_version, 1);
    assert_eq!(saved.current.pair.character.as_ref().unwrap().schema_version, 4);
    drop(store);

    let store = open(&root);
    let bytes = file_map(&root);
    let duplicate = store.commit_field_visit(&op, &before, &next).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, saved.current);
    let mut changed_request = next.clone();
    changed_request.pair.story.updated_at += 1;
    assert!(matches!(store.commit_field_visit(&op, &before, &changed_request), Err(PersistError::TransactionConflict(_))),
        "the operation id cannot acknowledge a different submitted request");
    assert_eq!(file_map(&root), bytes);
}

#[test]
fn stale_story_or_resource_predecessors_cannot_overwrite_an_intervening_save() {
    for change_story in [false, true] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let account = seeded(&store, "Intervening save", 8);
        let before = enter(&store, account, 1, "tin_1");
        let mut next = next_revision(&before);
        next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
        next.pair.story.blob[1] = 5;
        if change_story {
            let mut story = before.pair.story.clone();
            story.blob[2] = 62;
            store.put_progression(&story).unwrap();
        } else {
            store.update_character_progress(account, GAME, |old| {
                let mut character = old.unwrap();
                character.gil += 17;
                character
            }).unwrap();
        }
        let current = store.get_scene_state(account, GAME).unwrap();
        let bytes = file_map(&root);
        assert!(matches!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &next),
            Err(PersistError::TransactionConflict(_))));
        assert_eq!(store.get_scene_state(account, GAME).unwrap(), current);
        assert_eq!(current.field_visit.as_ref().unwrap().scratch_values[31], 0);
        assert_eq!(file_map(&root), bytes, "stale completion cannot roll back either record or consume the latch");
    }
}

#[test]
fn exact_visit_cas_preserves_the_entire_large_opaque_story_without_trimming_knownness() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Large opaque story", 8);
    let mut story = (0..5_129usize).map(|n| ((n * 17 + n / 256) % 256) as u8).collect::<Vec<_>>();
    story[0] = 4;
    store.put_progression(&Progression::new(account, GAME, story.clone(), NOW)).unwrap();
    let before = enter(&store, account, 1, "tin_1");
    let mut next = next_revision(&before);
    next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
    next.pair.story.blob[400] = 0;
    story[400] = 0;
    let saved = store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &next).unwrap();
    assert_eq!(saved.current.pair.story.blob, story);
    drop(store);
    assert_eq!(open(&root).get_scene_state(account, GAME).unwrap().pair.story.blob, story);
}

#[test]
fn the_last_receipt_deduplicates_after_rewards_and_older_forgotten_operations_fail_stale() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Receipt", 8);
    let before = enter(&store, account, 1, "tin_1");
    let mut next = next_revision(&before);
    next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
    next.pair.story.blob[1] = 5;
    let op = operation(2, FieldVisitOperationKind::Advance);
    store.commit_field_visit(&op, &before, &next).unwrap();
    store.update_character_progress(account, GAME, |old| {
        let mut character = old.unwrap();
        character.gil += 17;
        character.updated_at += 1;
        character
    }).unwrap();
    let earned = store.get_scene_state(account, GAME).unwrap();
    let bytes = file_map(&root);
    let duplicate = store.commit_field_visit(&op, &before, &next).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, earned, "a late acknowledgement cannot reinstall pre-reward resources");
    assert_eq!(file_map(&root), bytes);

    let mut conflicting = next.clone();
    conflicting.pair.story.blob[1] = 4;
    assert!(matches!(store.commit_field_visit(&op, &before, &conflicting), Err(PersistError::TransactionConflict(_))));
    let mut conflicting_before = before.clone();
    conflicting_before.pair.character.as_mut().unwrap().gil += 1;
    assert!(matches!(store.commit_field_visit(&op, &conflicting_before, &next), Err(PersistError::TransactionConflict(_))),
        "the receipt binds the predecessor as well as the result");
    assert_eq!(file_map(&root), bytes);

    let advanced = next_revision(&earned);
    store.commit_field_visit(&operation(3, FieldVisitOperationKind::Advance), &earned, &advanced).unwrap();
    let bytes = file_map(&root);
    assert!(matches!(store.commit_field_visit(&op, &before, &next), Err(PersistError::TransactionConflict(_))),
        "forgetting an older operation does not make its old revision current again");
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), advanced);
    assert_eq!(file_map(&root), bytes);
}

#[test]
fn same_field_and_round_trip_entries_get_new_identities_without_resetting_persistent_story() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Travel", 8);
    let initial = enter(&store, account, 1, "tin_1");
    let mut talked = next_revision(&initial);
    talked.field_visit.as_mut().unwrap().scratch_values[31] = 1;
    talked.pair.story.blob[1] = 5;
    store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &initial, &talked).unwrap();
    let story = talked.pair.story.clone();
    for (id, field) in [(3, "tin_1"), (4, "rootmap"), (5, "tin_1")] {
        let old = store.get_scene_state(account, GAME).unwrap();
        let current = enter(&store, account, id, field);
        let visit = current.field_visit.as_ref().unwrap();
        assert_ne!(visit.visit_id, old.field_visit.as_ref().unwrap().visit_id);
        assert_eq!((visit.revision, visit.field.as_str(), visit.scratch_values[31]), (0, field, 0));
        assert_eq!(current.pair.story, story, "only visit scratch resets on an accepted entry");
        assert_eq!(current.pair.character, old.pair.character);
    }
}

#[test]
fn malformed_entries_refuse_before_writing_any_record_or_selecting_the_format() {
    type Mutation = (&'static str, fn(&mut FieldVisitV1));
    let invalid: &[Mutation] = &[
        ("zero visit id", |v| v.visit_id = [0; 16]),
        ("zero source", |v| v.source_identity = [0; 32]),
        ("zero entry id", |v| v.entry.operation = [0; 16]),
        ("wrong entry id", |v| v.entry.operation = [2; 16]),
        ("nonzero first revision", |v| v.revision = 1),
        ("empty field", |v| v.field.clear()),
        ("field too long", |v| v.field = "x".repeat(MAX_FIELD_VISIT_FIELD_BYTES + 1)),
        ("control character", |v| v.field.push('\n')),
        ("empty arrival", |v| v.entry.arrival.clear()),
        ("arrival too large", |v| v.entry.arrival = vec![1; MAX_FIELD_VISIT_ENTRY_BYTES + 1]),
        ("empty ready anchor", |v| v.anchor.clear()),
        ("ready anchor too large", |v| v.anchor = vec![1; MAX_FIELD_VISIT_ANCHOR_BYTES + 1]),
        ("unknown nonzero bits", |v| { v.scratch_values[31] = 1; v.scratch_known_masks[31] = 0; }),
        ("dangling continuation", |v| v.active_continuation = Some([8; 32])),
        ("zero schema", |v| v.schema_version = 0),
        ("future schema", |v| v.schema_version = 2),
    ];
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Invalid entry", 8);
    let before = store.get_scene_state(account, GAME).unwrap();
    let original = file_map(&root);
    for (name, mutate) in invalid {
        let mut next = before.clone();
        let mut value = visit(11, 1, "tin_1");
        mutate(&mut value);
        next.field_visit = Some(value);
        assert!(store.commit_field_visit(&operation(1, FieldVisitOperationKind::Enter), &before, &next).is_err(), "{name}");
        assert_eq!(file_map(&root), original, "{name}");
    }
    let mut next = before.clone();
    next.field_visit = Some(visit(11, 1, "tin_1"));
    assert!(store.commit_field_visit(&operation(0, FieldVisitOperationKind::Enter), &before, &next).is_err());
    assert_eq!(file_map(&root), original);
    assert_eq!(log_lines(&root)[0]["schema_version"], 2);
}

#[test]
fn visit_boundary_encodings_and_unknown_masks_round_trip_without_inventing_known_bits() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Bounds", 8);
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut next = before.clone();
    let mut value = visit(11, 1, &"x".repeat(MAX_FIELD_VISIT_FIELD_BYTES));
    value.entry.arrival = vec![255; MAX_FIELD_VISIT_ENTRY_BYTES];
    value.anchor = vec![255; MAX_FIELD_VISIT_ANCHOR_BYTES];
    value.scratch_known_masks = [0; 256];
    value.scratch_known_masks[31] = 0b0101;
    value.scratch_values[31] = 0b0100;
    next.field_visit = Some(value.clone());
    let saved = store.commit_field_visit(&operation(1, FieldVisitOperationKind::Enter), &before, &next).unwrap();
    assert_eq!(saved.current.field_visit, Some(value.clone()));
    let json = serde_json::to_value(&value).unwrap();
    assert_eq!(json["scratch_values"].as_str().unwrap().len(), 512);
    assert_eq!(json["scratch_known_masks"].as_str().unwrap().len(), 512);
    assert_eq!(serde_json::from_value::<FieldVisitV1>(json.clone()).unwrap(), value);
    for malformed in ["00".repeat(255), "00".repeat(257), "GG".repeat(256), "FF".repeat(256)] {
        let mut broken = json.clone();
        broken["scratch_values"] = malformed.into();
        assert!(serde_json::from_value::<FieldVisitV1>(broken).is_err(), "scratch encoding must be exact canonical lowercase hex");
    }
    drop(store);
    assert_eq!(open(&root).get_scene_state(account, GAME).unwrap().field_visit, Some(value));
}

#[test]
fn advance_cannot_change_visit_identity_or_omit_the_exact_next_revision() {
    type Mutation = (&'static str, fn(&mut FieldVisitV1));
    let invalid: &[Mutation] = &[
        ("another visit", |v| v.visit_id = [12; 16]),
        ("another field", |v| v.field = "rootmap".into()),
        ("another source", |v| v.source_identity = [10; 32]),
        ("another entry operation", |v| v.entry.operation = [3; 16]),
        ("another arrival", |v| v.entry.arrival.push(1)),
        ("no revision increment", |v| v.revision = 0),
        ("skipped revision", |v| v.revision = 2),
        ("dangling active scene", |v| v.active_continuation = Some([8; 32])),
    ];
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Advance", 8);
    let before = enter(&store, account, 1, "tin_1");
    let original = file_map(&root);
    for (name, mutate) in invalid {
        let mut next = next_revision(&before);
        mutate(next.field_visit.as_mut().unwrap());
        assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &next).is_err(), "{name}");
        assert_eq!(file_map(&root), original, "{name}");
    }
    let mut next = next_revision(&before);
    next.field_visit = None;
    assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &next).is_err());
    let mut entered = before.clone();
    entered.field_visit.as_mut().unwrap().entry.operation = [2; 16];
    assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Enter), &before, &entered).is_err(),
        "even a same-field MAPJUMP needs a fresh identity");
    let mut reserved = next_revision(&before);
    reserved.reservation = Some(SceneReservation { instance: [8; 32], ordinal: 0, attempt: 0, slot: 0 });
    assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &reserved).is_err());
    assert_eq!(file_map(&root), original);
}

#[test]
fn advance_preserves_known_scratch_bits_but_a_new_visit_can_initialize_different_masks() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Scratch knowledge", 8);
    let before = store.get_scene_state(account, GAME).unwrap();
    let mut entered = before.clone();
    let mut value = visit(11, 1, "tin_1");
    value.scratch_known_masks = [0; 256];
    value.scratch_known_masks[31] = 0b0011;
    value.scratch_values[31] = 0b0001;
    entered.field_visit = Some(value);
    let entered = store.commit_field_visit(&operation(1, FieldVisitOperationKind::Enter), &before, &entered).unwrap().current;
    let bytes = file_map(&root);
    for forgotten in [0b0001, 0b0010] {
        let mut next = next_revision(&entered);
        let visit = next.field_visit.as_mut().unwrap();
        visit.scratch_known_masks[31] &= !forgotten;
        visit.scratch_values[31] &= visit.scratch_known_masks[31];
        assert_eq!(visit.scratch_values[31] & !visit.scratch_known_masks[31], 0,
            "each refused after-image is still canonically encoded");
        assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &entered, &next).is_err(),
            "Advance cannot forget either a known one or a known zero");
        assert_eq!(store.get_scene_state(account, GAME).unwrap(), entered);
        assert_eq!(file_map(&root), bytes);
    }

    // Within a visit the interpreter may learn more bits, including known zeroes.
    let mut learned = next_revision(&entered);
    learned.field_visit.as_mut().unwrap().scratch_known_masks[31] |= 0b0100;
    let learned = store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &entered, &learned).unwrap().current;
    let mut arrived = learned.clone();
    let mut fresh = visit(12, 3, "tin_1");
    fresh.scratch_known_masks = [0; 256];
    arrived.field_visit = Some(fresh);
    let arrived = store.commit_field_visit(&operation(3, FieldVisitOperationKind::Enter), &learned, &arrived).unwrap().current;
    assert_eq!(arrived.field_visit.as_ref().unwrap().scratch_known_masks, [0; 256],
        "an accepted new entry owns a new scratch lifetime, even in the same field");
    assert_eq!(arrived.pair, learned.pair);
    drop(store);
    assert_eq!(open(&root).get_scene_state(account, GAME).unwrap(), arrived);
}

#[test]
fn ordinary_scene_batches_cannot_mutate_or_clear_the_visit_as_a_side_effect() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Batch preservation", 8);
    let before = enter(&store, account, 1, "tin_1");
    let op = SceneOperation { instance: [8; 32], ordinal: 0, attempt: 0, kind: SceneOperationKind::BattleOutcome };
    let bytes = file_map(&root);
    for change in 0..4 {
        let mut next = before.clone();
        next.pair.character.as_mut().unwrap().gil += 7;
        match change {
            0 => next.field_visit.as_mut().unwrap().scratch_values[31] = 1,
            1 => next.field_visit.as_mut().unwrap().revision += 1,
            2 => next.field_visit.as_mut().unwrap().anchor.push(1),
            3 => next.field_visit = None,
            _ => unreachable!(),
        }
        assert!(store.commit_scene_batch(&op, b"fixture battle reward", std::slice::from_ref(&before), &[next]).is_err());
        assert_eq!(file_map(&root), bytes, "refused metadata changes cannot leave the reward half committed");
    }
    let mut next = before.clone();
    next.pair.character.as_mut().unwrap().gil += 7;
    let saved = store.commit_scene_batch(&op, b"fixture battle reward", std::slice::from_ref(&before), &[next]).unwrap();
    assert_eq!(saved.current[0].field_visit, before.field_visit,
        "the same batch is valid when it preserves the account's visit");
}

#[test]
fn existing_checkpoint_operations_cannot_create_a_dangling_or_unreviewed_visit_link() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Checkpoint link", 8);
    let before = enter(&store, account, 1, "tin_1");
    let instance = [8; 32];
    let prepare = |state: &SceneState| {
        let mut next = state.clone();
        next.checkpoint = Some(SceneCheckpoint {
            instance, scene_key: "fixture/held-scene".into(), source_hash: [9; 32], replay_version: 1,
            original_story: state.pair.story.blob.clone(), original_gil: state.pair.character.as_ref().unwrap().gil,
            seed: 42, replay: Vec::new(), consumed_battles: 0,
            phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 12, battle_seed: 123 },
        });
        next.reservation = Some(SceneReservation { instance, ordinal: 0, attempt: 0, slot: 0 });
        next
    };
    let op = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::PrepareBattle };
    let bytes = file_map(&root);
    for active in [None, Some([7; 32]), Some(instance)] {
        let mut next = prepare(&before);
        next.field_visit.as_mut().unwrap().active_continuation = active;
        assert!(store.commit_scene_batch(&op, b"fixture prepare", std::slice::from_ref(&before), &[next]).is_err(),
            "a missing or wrong link is invalid; even a matching new link needs a separately reviewed lifecycle operation");
        assert_eq!(file_map(&root), bytes);
    }
    // The foundation has not changed existing checkpoint semantics for legacy accounts.
    let legacy = seeded(&store, "Legacy checkpoint", 8);
    let legacy = store.get_scene_state(legacy, GAME).unwrap();
    let next = prepare(&legacy);
    let saved = store.commit_scene_batch(&op, b"fixture prepare", std::slice::from_ref(&legacy), &[next]).unwrap();
    assert!(saved.current[0].field_visit.is_none());
    assert_eq!(saved.current[0].checkpoint.as_ref().unwrap().instance, instance);
}

#[test]
fn an_exhausted_revision_cannot_wrap_back_to_zero() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Exhausted revision", 8);
    enter(&store, account, 1, "tin_1");
    drop(store);
    let path = character_path(&root, account);
    let mut record = read_json(&path);
    record["field_visit"]["revision"] = json!(u64::MAX);
    write_json(&path, &record);
    let store = open(&root);
    let before = store.get_scene_state(account, GAME).unwrap();
    assert_eq!(before.field_visit.as_ref().unwrap().revision, u64::MAX);
    let mut wrapped = before.clone();
    wrapped.field_visit.as_mut().unwrap().revision = 0;
    let bytes = file_map(&root);
    assert!(store.commit_field_visit(&operation(2, FieldVisitOperationKind::Advance), &before, &wrapped).is_err());
    assert_eq!(file_map(&root), bytes);
}

#[test]
fn ordinary_story_character_and_battle_writes_preserve_each_accounts_idle_visit() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let a = seeded(&store, "Owner", 8);
    let b = seeded(&store, "Guest", 17);
    let a_visit = enter(&store, a, 1, "tin_1").field_visit;
    let b_visit = enter(&store, b, 2, "tin_1").field_visit;
    let before = store.get_progress_pair(a, GAME).unwrap();
    let mut next = before.clone();
    next.story.blob.push(1);
    next.character.as_mut().unwrap().gil += 10;
    store.commit_scene_progress("fixture/visit-independent", &before, &next).unwrap();
    let current = store.get_progress_pair(a, GAME).unwrap();
    let mut story = current.story.blob.clone();
    story.push(2);
    store.compare_exchange_progression(a, GAME, &current.story.blob, &story, NOW + 1).unwrap();
    store.update_character_progress(a, GAME, |old| {
        let mut record = old.unwrap();
        record.inventory.as_mut().unwrap()[0].count -= 1;
        record
    }).unwrap();
    let story = store.get_progression(a, GAME).unwrap();
    store.put_progression(&story).unwrap();
    let progress = store.get_character_progress(a, GAME).unwrap().unwrap();
    store.put_character_progress(&progress).unwrap();
    assert_eq!(store.get_scene_state(a, GAME).unwrap().field_visit, a_visit);
    assert_eq!(store.get_scene_state(b, GAME).unwrap().field_visit, b_visit);

    let before = [a, b].map(|account| store.get_progress_pair(account, GAME).unwrap());
    let mut after = before.clone();
    for pair in &mut after { pair.character.as_mut().unwrap().gil += 7; }
    after[0].story.blob.push(3);
    let saved = store.commit_battle_outcome([5; 32], b"fixture outcome", &before, &after).unwrap();
    assert_eq!(saved.current.iter().map(|state| state.field_visit.clone()).collect::<Vec<_>>(), [a_visit.clone(), b_visit.clone()]);
    assert_eq!(saved.current[1].pair.story, before[1].story, "the owner's outcome does not transfer its story to a guest");
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_scene_state(a, GAME).unwrap().field_visit, a_visit);
    assert_eq!(store.get_scene_state(b, GAME).unwrap().field_visit, b_visit);
}

#[test]
fn snapshots_and_account_rekeys_preserve_the_visit_and_do_not_repeat_the_latched_effect() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let account = seeded(&store, "Snapshot", 8);
    let before = enter(&store, account, 1, "tin_1");
    let mut next = next_revision(&before);
    next.field_visit.as_mut().unwrap().scratch_values[31] = 1;
    next.pair.story.blob[1] = 5;
    let op = operation(2, FieldVisitOperationKind::Advance);
    store.commit_field_visit(&op, &before, &next).unwrap();
    let snapshot = dir.path().join("snapshot");
    store.snapshot(&snapshot).unwrap();
    let restored_root = dir.path().join("snapshot-copy");
    restore_snapshot(&snapshot, &restored_root).unwrap();
    let copy = open(&restored_root);
    assert_eq!(copy.get_scene_state(account, GAME).unwrap(), next);
    assert_eq!(copy.commit_field_visit(&op, &before, &next).unwrap().commit, SceneCommit::AlreadyApplied);

    let deleted = store.delete_account(account, NOW + 1).unwrap();
    let archive = root.join("trash").join(&deleted.entry);
    let archived_character = fs::read(archive.join(format!("progression/{}/Fixture.character.json", account.0))).unwrap();
    let restored = store.restore_account(&deleted.entry, NOW + 2).unwrap();
    assert_ne!(restored.account_id, account);
    let rekeyed = store.get_scene_state(restored.account_id, GAME).unwrap();
    assert_eq!(rekeyed.field_visit, next.field_visit);
    assert_eq!(rekeyed.pair.story.blob, next.pair.story.blob);
    assert_eq!(rekeyed.pair.character.as_ref().unwrap().gil, next.pair.character.as_ref().unwrap().gil);
    let sealed = root.join("trash").join(&restored.entry);
    assert_eq!(fs::read(sealed.join(format!("progression/{}/Fixture.character.json", account.0))).unwrap(), archived_character,
        "restore rekeys installed records without rewriting the archive's originals");
    let rekey_request = |state: &SceneState| {
        let mut state = state.clone();
        state.pair.story.account_id = restored.account_id;
        state.pair.character.as_mut().unwrap().account_id = restored.account_id;
        state
    };
    let bytes = file_map(&root);
    assert!(matches!(store.commit_field_visit(&op, &rekey_request(&before), &rekey_request(&next)),
        Err(PersistError::TransactionConflict(_))),
        "the retained receipt binds the old account identity, so a rekeyed request is not an exact retry");
    assert_eq!(store.get_scene_state(restored.account_id, GAME).unwrap(), rekeyed);
    assert_eq!(file_map(&root), bytes, "a refused replay cannot deduct affection or change the restored receipt");
    let repeat = next_revision(&rekeyed);
    let saved = store.commit_field_visit(&operation(3, FieldVisitOperationKind::Advance), &rekeyed, &repeat).unwrap();
    assert_eq!(saved.current.pair, rekeyed.pair);
    assert_eq!(saved.current.field_visit.as_ref().unwrap().scratch_values[31], 1);
}

#[test]
fn entry_requires_authoritative_character_progress_and_never_synthesizes_it() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let (account, _) = store.create_account("No resources", NOW).unwrap();
    let before = store.get_scene_state(account, GAME).unwrap();
    assert!(before.pair.character.is_none());
    let mut next = before.clone();
    next.field_visit = Some(visit(11, 1, "tin_1"));
    let bytes = file_map(&root);
    assert!(store.commit_field_visit(&operation(1, FieldVisitOperationKind::Enter), &before, &next).is_err());
    assert_eq!(file_map(&root), bytes);
}
