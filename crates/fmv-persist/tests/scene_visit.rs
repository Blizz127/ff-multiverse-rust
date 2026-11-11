//! Public storage contract for a scene attached to one account's field visit.
//! All data is synthetic and every store lives in a temporary directory.

mod common;

use std::path::Path;

use common::*;
use fmv_core::{GameId, InventoryEntry};
use fmv_persist::{
    CharacterProgress, FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind,
    FieldVisitV1, PersistError, Progression, SceneCheckpoint, SceneCommit, SceneOperation,
    SceneOperationKind, ScenePhase, SceneReservation, SceneState, Store,
};
use serde_json::json;

const GAME: GameId = GameId::Fixture;
const INSTANCE: [u8; 32] = [7; 32];
const ATTACH: &[u8] = b"fixture exact visit scene attachment";
const COMPLETE: &[u8] = b"fixture accepted answer and one gil debit";

type StateMutation = (&'static str, fn(&mut SceneState));

fn operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: 0, attempt: 0, kind }
}

fn seeded(store: &Store, name: &str) -> SceneState {
    let (account, _) = store.create_account(name, NOW).unwrap();
    store.put_progression(&Progression::new(account, GAME, vec![4, 8, 61, 27], NOW)).unwrap();
    store.put_character_progress(&CharacterProgress {
        exp: 1_234, ap: 8, gil: 240, limit: 17,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1)]),
        ..CharacterProgress::new(account, GAME, 2, 7, NOW)
    }).unwrap();
    store.get_scene_state(account, GAME).unwrap()
}

fn entry_request(before: &SceneState) -> (FieldVisitOperation, SceneState) {
    let id = before.pair.story.account_id.0 as u8;
    let op = FieldVisitOperation { id: [id; 16], kind: FieldVisitOperationKind::Enter };
    let mut next = before.clone();
    let mut visit = FieldVisitV1 {
        schema_version: 1, visit_id: [id + 10; 16], revision: 0,
        field: "fixture-room".into(), source_identity: [9; 32],
        entry: FieldVisitEntryV1 { operation: op.id, arrival: b"fixture arrival at the door".to_vec() },
        anchor: b"fixture ready profile and position".to_vec(),
        scratch_values: [0; 256], scratch_known_masks: [0; 256], active_continuation: None,
    };
    visit.scratch_values[6] = 1;
    visit.scratch_known_masks[6] = 3;
    visit.scratch_known_masks[31] = 1;
    next.field_visit = Some(visit);
    (op, next)
}

fn enter(store: &Store, before: &SceneState) -> SceneState {
    let (op, next) = entry_request(before);
    store.commit_field_visit(&op, before, &next).unwrap().current
}

fn attachment(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    let visit = next.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.active_continuation = Some(INSTANCE);
    next.checkpoint = Some(SceneCheckpoint {
        instance: INSTANCE, scene_key: "fixture-room/conversation".into(),
        // This identifies checked replay, not the visit's reconstruction source profile.
        // A successful attachment must not require these distinct identities to be equal.
        source_hash: [6; 32], replay_version: 1,
        original_story: before.pair.story.blob.clone(), original_gil: before.pair.character.as_ref().unwrap().gil,
        seed: 42, replay: b"fixture checked replay prefix".to_vec(),
        phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    next
}

fn completion(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    next.checkpoint = None;
    let visit = next.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.active_continuation = None;
    visit.scratch_values[31] = 3;
    visit.scratch_known_masks[31] = 3;
    next.pair.story.blob[1] = next.pair.story.blob[1].saturating_sub(3);
    next.pair.story.updated_at = NOW + 1;
    let character = next.pair.character.as_mut().unwrap();
    character.gil -= 1;
    character.inventory.as_mut().unwrap()[0].count -= 1;
    character.updated_at = NOW + 1;
    next
}

fn advancement(before: &SceneState, ordinal: u32) -> SceneState {
    let mut next = before.clone();
    next.field_visit.as_mut().unwrap().revision += 1;
    next.checkpoint.as_mut().unwrap().replay.extend_from_slice(&ordinal.to_le_bytes());
    next
}

fn advance_operation(ordinal: u32) -> SceneOperation {
    SceneOperation { ordinal, ..operation(SceneOperationKind::AdvanceVisit) }
}

fn attach(store: &Store, before: &SceneState) -> SceneState {
    let next = attachment(before);
    let saved = store.commit_scene_batch(&operation(SceneOperationKind::AttachVisit), ATTACH,
        std::slice::from_ref(before), std::slice::from_ref(&next)).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, vec![next.clone()]);
    next
}

fn assert_refused(store: &Store, root: &Path, op: SceneOperation, before: &[SceneState], next: &[SceneState], why: &str) {
    let files = file_map(root);
    assert!(store.commit_scene_batch(&op, b"invalid fixture transition", before, next).is_err(), "{why}");
    assert_eq!(file_map(root), files, "{why}: a refusal must leave no intent or partial afterimage");
}

#[test]
fn an_attached_visit_completes_atomically_and_both_receipts_return_current_state_after_reopen() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let initial = seeded(&store, "Visit owner");
    let idle = enter(&store, &initial);
    let account = idle.pair.story.account_id;
    let attached = attach(&store, &idle);
    assert_eq!(attached.pair, idle.pair, "attaching runs no prefix story or resource effect");
    let mut expected_visit = idle.field_visit.clone().unwrap();
    expected_visit.revision += 1;
    expected_visit.active_continuation = Some(INSTANCE);
    assert_eq!(attached.field_visit, Some(expected_visit));
    assert_ne!(attached.checkpoint.as_ref().unwrap().source_hash, attached.field_visit.as_ref().unwrap().source_identity);
    assert!(attached.reservation.is_none());
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![attached.clone()]);
    let files = file_map(&root);
    let duplicate = store.commit_scene_batch(&operation(SceneOperationKind::AttachVisit), ATTACH,
        std::slice::from_ref(&idle), std::slice::from_ref(&attached)).unwrap();
    assert_eq!((duplicate.commit, duplicate.current), (SceneCommit::AlreadyApplied, vec![attached.clone()]));
    assert_eq!(file_map(&root), files);

    let next = completion(&attached);
    let saved = store.commit_scene_batch(&operation(SceneOperationKind::CompleteVisit), COMPLETE,
        std::slice::from_ref(&attached), std::slice::from_ref(&next)).unwrap();
    assert_eq!((saved.commit, saved.current), (SceneCommit::Applied, vec![next.clone()]));
    assert_eq!(next.pair.story.blob, [4, 5, 61, 27]);
    assert_eq!(next.pair.character.as_ref().unwrap().gil, 239);
    assert_eq!(next.pair.character.as_ref().unwrap().inventory, Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(3, 1)]));
    assert_eq!(next.field_visit.as_ref().unwrap().scratch_values[6], 1, "unrelated scratch survives");
    assert_eq!(next.field_visit.as_ref().unwrap().scratch_values[31], 3);
    assert!(next.checkpoint.is_none() && next.reservation.is_none());
    assert_eq!(next.field_visit.as_ref().unwrap().active_continuation, None);
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![next.clone()]);
    store.update_character_progress(account, GAME, |old| {
        let mut character = old.unwrap();
        character.gil += 17;
        character.exp += 23;
        character.updated_at = NOW + 2;
        character
    }).unwrap();
    let current = store.get_scene_state(account, GAME).unwrap();
    let files = file_map(&root);
    for (op, result, before, after) in [
        (operation(SceneOperationKind::AttachVisit), ATTACH, &idle, &attached),
        (operation(SceneOperationKind::CompleteVisit), COMPLETE, &attached, &next),
    ] {
        assert!(store.scene_batch_applied(&op, result, &[account], GAME).unwrap());
        let duplicate = store.commit_scene_batch(&op, result, std::slice::from_ref(before), std::slice::from_ref(after)).unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, vec![current.clone()], "a retry cannot resurrect the checkpoint or undo later earnings");
        assert_eq!(file_map(&root), files);
        assert!(matches!(store.scene_batch_applied(&op, b"different selection", &[account], GAME),
            Err(PersistError::TransactionConflict(_))));
    }
    // The visit operation receipt predating the scene is retained too, not replaced by
    // either scene-batch receipt or by the ordinary character reward above.
    let (entry, entered) = entry_request(&initial);
    let duplicate = store.commit_field_visit(&entry, &initial, &entered).unwrap();
    assert_eq!((duplicate.commit, duplicate.current), (SceneCommit::AlreadyApplied, current));
    assert_eq!(file_map(&root), files);
}

#[test]
fn accepted_window_prefixes_keep_resources_and_scratch_unchanged_and_old_retries_return_current() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let idle = enter(&store, &seeded(&store, "Window prefixes"));
    let attached = attach(&store, &idle);
    let account = idle.pair.story.account_id;
    let first = advancement(&attached, 1);
    let second = advancement(&first, 2);
    for (ordinal, before, next, result) in [
        (1, &attached, &first, b"fixture accepted window one".as_slice()),
        (2, &first, &second, b"fixture accepted window two".as_slice()),
    ] {
        let saved = store.commit_scene_batch(&advance_operation(ordinal), result,
            std::slice::from_ref(before), std::slice::from_ref(next)).unwrap();
        assert_eq!((saved.commit, saved.current), (SceneCommit::Applied, vec![next.clone()]));
        assert_eq!(next.pair, attached.pair, "accepting a window cannot publish the in-scene story or resources");
        let mut visit = attached.field_visit.clone().unwrap();
        visit.revision += u64::from(ordinal);
        assert_eq!(next.field_visit, Some(visit), "scratch and its knownness remain the durable entry state");
        let checkpoint = next.checkpoint.as_ref().unwrap();
        assert_eq!((checkpoint.phase.clone(), checkpoint.consumed_battles), (ScenePhase::Resume { segment: 0 }, 0));
        assert!(next.reservation.is_none());
    }
    drop(store);
    let store = open(&root);
    assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![second.clone()]);
    let files = file_map(&root);
    let replay_first = || store.commit_scene_batch(&advance_operation(1), b"fixture accepted window one",
        std::slice::from_ref(&attached), std::slice::from_ref(&first)).unwrap();
    let duplicate = replay_first();
    assert_eq!((duplicate.commit, duplicate.current), (SceneCommit::AlreadyApplied, vec![second.clone()]));
    assert_eq!(file_map(&root), files, "window one cannot roll back the second accepted prefix");
    let completed = completion(&second);
    store.commit_scene_batch(&operation(SceneOperationKind::CompleteVisit), COMPLETE,
        std::slice::from_ref(&second), std::slice::from_ref(&completed)).unwrap();
    let files = file_map(&root);
    let duplicate = replay_first();
    assert_eq!((duplicate.commit, duplicate.current), (SceneCommit::AlreadyApplied, vec![completed.clone()]));
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), completed);
    assert_eq!(file_map(&root), files, "a window retry after completion cannot resurrect its checkpoint");
}

#[test]
fn advancing_a_visit_only_updates_replay_and_one_revision_at_a_positive_window_ordinal() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let idle = enter(&store, &seeded(&store, "Advance refusals"));
    let attached = attach(&store, &idle);
    let valid = advancement(&attached, 1);
    let cases: &[StateMutation] = &[
        ("missing checkpoint", |s| s.checkpoint = None),
        ("missing link", |s| s.field_visit.as_mut().unwrap().active_continuation = None),
        ("different link", |s| s.field_visit.as_mut().unwrap().active_continuation = Some([8; 32])),
        ("different checkpoint instance", |s| s.checkpoint.as_mut().unwrap().instance = [8; 32]),
        ("changed checkpoint source", |s| s.checkpoint.as_mut().unwrap().source_hash = [8; 32]),
        ("changed scene key", |s| s.checkpoint.as_mut().unwrap().scene_key.push('x')),
        ("changed replay version", |s| s.checkpoint.as_mut().unwrap().replay_version += 1),
        ("unchanged replay", |s| s.checkpoint.as_mut().unwrap().replay = b"fixture checked replay prefix".to_vec()),
        ("changed seed", |s| s.checkpoint.as_mut().unwrap().seed += 1),
        ("changed original story", |s| s.checkpoint.as_mut().unwrap().original_story.push(99)),
        ("changed original gil", |s| s.checkpoint.as_mut().unwrap().original_gil += 1),
        ("consumed battle", |s| { let c = s.checkpoint.as_mut().unwrap(); c.consumed_battles = 1; c.phase = ScenePhase::Resume { segment: 1 }; }),
        ("changed phase", |s| s.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: vec![1] }),
        ("no revision advance", |s| s.field_visit.as_mut().unwrap().revision -= 1),
        ("skipped revision", |s| s.field_visit.as_mut().unwrap().revision += 1),
        ("changed identity", |s| s.field_visit.as_mut().unwrap().visit_id = [8; 16]),
        ("changed field", |s| s.field_visit.as_mut().unwrap().field.push('x')),
        ("changed source", |s| s.field_visit.as_mut().unwrap().source_identity = [8; 32]),
        ("changed entry", |s| s.field_visit.as_mut().unwrap().entry.arrival.push(1)),
        ("changed anchor", |s| s.field_visit.as_mut().unwrap().anchor.push(1)),
        ("changed scratch", |s| s.field_visit.as_mut().unwrap().scratch_values[6] = 2),
        ("new scratch knowledge", |s| s.field_visit.as_mut().unwrap().scratch_known_masks[31] = 3),
        ("changed story", |s| s.pair.story.blob.push(1)),
        ("changed story timestamp", |s| s.pair.story.updated_at += 1),
        ("changed gil", |s| s.pair.character.as_mut().unwrap().gil -= 1),
        ("changed character timestamp", |s| s.pair.character.as_mut().unwrap().updated_at += 1),
        ("new reservation", |s| s.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 })),
    ];
    for &(why, change) in cases {
        let mut next = valid.clone();
        change(&mut next);
        assert_refused(&store, &root, advance_operation(1), std::slice::from_ref(&attached), &[next], why);
    }
    for (ordinal, attempt) in [(0, 0), (1025, 0), (1, 1)] {
        let op = SceneOperation { attempt, ..advance_operation(ordinal) };
        assert_refused(&store, &root, op, std::slice::from_ref(&attached), std::slice::from_ref(&valid), "invalid accepted-window boundary");
    }
    assert_refused(&store, &root, advance_operation(1), &[], &[], "empty advancement");
    let other = seeded(&store, "Unrelated member");
    assert_refused(&store, &root, advance_operation(1), &[attached.clone(), other.clone()], &[valid, other], "non-singleton advancement");
    assert_eq!(store.get_scene_state(idle.pair.story.account_id, GAME).unwrap(), attached);
}

#[test]
fn attach_only_adds_the_matching_zero_battle_link_and_exactly_one_visit_revision() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let idle = enter(&store, &seeded(&store, "Attach refusals"));
    let valid = attachment(&idle);
    let cases: &[StateMutation] = &[
        ("missing checkpoint", |s| s.checkpoint = None),
        ("missing link", |s| s.field_visit.as_mut().unwrap().active_continuation = None),
        ("different link", |s| s.field_visit.as_mut().unwrap().active_continuation = Some([8; 32])),
        ("different checkpoint instance", |s| s.checkpoint.as_mut().unwrap().instance = [8; 32]),
        ("missing replay source", |s| s.checkpoint.as_mut().unwrap().source_hash = [0; 32]),
        ("already consumed battle", |s| { let c = s.checkpoint.as_mut().unwrap(); c.consumed_battles = 1; c.phase = ScenePhase::Resume { segment: 1 }; }),
        ("not resumed", |s| s.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: vec![1] }),
        ("wrong original story", |s| s.checkpoint.as_mut().unwrap().original_story.push(99)),
        ("wrong original gil", |s| s.checkpoint.as_mut().unwrap().original_gil += 1),
        ("no revision advance", |s| s.field_visit.as_mut().unwrap().revision -= 1),
        ("skipped revision", |s| s.field_visit.as_mut().unwrap().revision += 1),
        ("changed identity", |s| s.field_visit.as_mut().unwrap().visit_id = [8; 16]),
        ("changed field", |s| s.field_visit.as_mut().unwrap().field.push('x')),
        ("changed source", |s| s.field_visit.as_mut().unwrap().source_identity = [8; 32]),
        ("changed entry", |s| s.field_visit.as_mut().unwrap().entry.arrival.push(1)),
        ("changed anchor", |s| s.field_visit.as_mut().unwrap().anchor.push(1)),
        ("changed scratch", |s| s.field_visit.as_mut().unwrap().scratch_values[6] = 2),
        ("new scratch knowledge", |s| s.field_visit.as_mut().unwrap().scratch_known_masks[31] = 3),
        ("changed story", |s| s.pair.story.blob.push(1)),
        ("changed story timestamp", |s| s.pair.story.updated_at += 1),
        ("changed gil", |s| s.pair.character.as_mut().unwrap().gil -= 1),
        ("changed inventory", |s| s.pair.character.as_mut().unwrap().inventory.as_mut().unwrap()[0].count -= 1),
        ("changed character timestamp", |s| s.pair.character.as_mut().unwrap().updated_at += 1),
        ("new reservation", |s| s.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 })),
    ];
    for &(why, change) in cases {
        let mut next = valid.clone();
        change(&mut next);
        assert_refused(&store, &root, operation(SceneOperationKind::AttachVisit), std::slice::from_ref(&idle), &[next], why);
    }
    for (ordinal, attempt) in [(1, 0), (0, 1)] {
        let op = SceneOperation { ordinal, attempt, ..operation(SceneOperationKind::AttachVisit) };
        assert_refused(&store, &root, op, std::slice::from_ref(&idle), std::slice::from_ref(&valid), "nonzero operation boundary");
    }
    assert_eq!(store.get_scene_state(idle.pair.story.account_id, GAME).unwrap(), idle);
}

#[test]
fn completion_cannot_change_visit_provenance_forget_knowledge_or_leave_a_link() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let idle = enter(&store, &seeded(&store, "Complete refusals"));
    let attached = attach(&store, &idle);
    let valid = completion(&attached);
    let cases: &[StateMutation] = &[
        ("missing visit", |s| s.field_visit = None),
        ("remaining link", |s| s.field_visit.as_mut().unwrap().active_continuation = Some(INSTANCE)),
        ("no revision advance", |s| s.field_visit.as_mut().unwrap().revision -= 1),
        ("skipped revision", |s| s.field_visit.as_mut().unwrap().revision += 1),
        ("changed identity", |s| s.field_visit.as_mut().unwrap().visit_id = [8; 16]),
        ("changed field", |s| s.field_visit.as_mut().unwrap().field.push('x')),
        ("changed source", |s| s.field_visit.as_mut().unwrap().source_identity = [8; 32]),
        ("changed entry", |s| s.field_visit.as_mut().unwrap().entry.operation = [8; 16]),
        ("changed anchor", |s| s.field_visit.as_mut().unwrap().anchor.push(1)),
        ("forgotten known bit", |s| { let v = s.field_visit.as_mut().unwrap(); v.scratch_known_masks[6] = 1; }),
        ("noncanonical unknown value", |s| s.field_visit.as_mut().unwrap().scratch_values[5] = 1),
        ("new reservation", |s| s.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 })),
    ];
    for &(why, change) in cases {
        let mut next = valid.clone();
        change(&mut next);
        assert_refused(&store, &root, operation(SceneOperationKind::CompleteVisit), std::slice::from_ref(&attached), &[next], why);
    }
    let mut kept = valid.clone();
    kept.checkpoint = attached.checkpoint.clone();
    kept.field_visit.as_mut().unwrap().active_continuation = Some(INSTANCE);
    assert_refused(&store, &root, operation(SceneOperationKind::CompleteVisit), std::slice::from_ref(&attached), &[kept], "checkpoint kept");
    for (instance, ordinal, attempt) in [([8; 32], 0, 0), (INSTANCE, 1, 0), (INSTANCE, 0, 1)] {
        let op = SceneOperation { instance, ordinal, attempt, kind: SceneOperationKind::CompleteVisit };
        assert_refused(&store, &root, op, std::slice::from_ref(&attached), std::slice::from_ref(&valid), "wrong operation boundary");
    }
    assert_eq!(store.get_scene_state(idle.pair.story.account_id, GAME).unwrap(), attached);
}

#[test]
fn attach_and_complete_are_singleton_operations_and_do_not_take_reserved_guests() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let owner = seeded(&store, "Legacy battle owner");
    let idle = enter(&store, &seeded(&store, "Visited guest"));
    let other = enter(&store, &seeded(&store, "Other visited guest"));
    let attached = attachment(&idle);
    for guest in [&owner, &other] {
        assert_refused(&store, &root, operation(SceneOperationKind::AttachVisit), &[idle.clone(), guest.clone()],
            &[attached.clone(), guest.clone()], "mixed or multiaccount attachment");
    }
    assert_refused(&store, &root, operation(SceneOperationKind::AttachVisit), &[], &[], "empty attachment");
    let mut no_visit = owner.clone();
    no_visit.checkpoint = Some(SceneCheckpoint { original_story: owner.pair.story.blob.clone(),
        original_gil: owner.pair.character.as_ref().unwrap().gil, ..attached.checkpoint.clone().unwrap() });
    assert_refused(&store, &root, operation(SceneOperationKind::AttachVisit), std::slice::from_ref(&owner), &[no_visit], "no accepted visit");

    // A valid existing battle may reserve a guest's visit without owning that guest's
    // checkpoint. AttachVisit cannot silently steal the reserved guest from its group.
    let before = vec![owner.clone(), idle.clone()];
    let mut prepared = before.clone();
    prepared[0].checkpoint = Some(SceneCheckpoint {
        instance: [5; 32], scene_key: "fixture/battle".into(), source_hash: [6; 32], replay_version: 1,
        original_story: owner.pair.story.blob.clone(), original_gil: owner.pair.character.as_ref().unwrap().gil,
        seed: 1, replay: vec![], consumed_battles: 0,
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 12, battle_seed: 9 },
    });
    for (slot, state) in prepared.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance: [5; 32], ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    let op = SceneOperation { instance: [5; 32], ordinal: 0, attempt: 0, kind: SceneOperationKind::PrepareBattle };
    let reserved = store.commit_scene_batch(&op, b"fixture prepared group", &before, &prepared).unwrap().current;
    let mut steal = attachment(&reserved[1]);
    steal.reservation = None;
    assert_refused(&store, &root, operation(SceneOperationKind::AttachVisit), std::slice::from_ref(&reserved[1]), &[steal], "reserved guest");
    assert_eq!(store.get_scene_group(idle.pair.story.account_id, GAME).unwrap(), reserved);

    let attached = attach(&store, &other);
    let complete = completion(&attached);
    for guest in [&reserved[0], &reserved[1]] {
        assert_refused(&store, &root, operation(SceneOperationKind::CompleteVisit), &[attached.clone(), guest.clone()],
            &[complete.clone(), guest.clone()], "completion cannot include unrelated or reserved members");
    }
    assert_refused(&store, &root, operation(SceneOperationKind::CompleteVisit), &[], &[], "empty completion");
}

#[test]
fn a_visit_revision_at_the_limit_cannot_wrap_during_any_scene_visit_operation() {
    for kind in [SceneOperationKind::AttachVisit, SceneOperationKind::AdvanceVisit, SceneOperationKind::CompleteVisit] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let idle = enter(&store, &seeded(&store, "Exhausted visit"));
        let current = if kind == SceneOperationKind::AttachVisit { idle } else { attach(&store, &idle) };
        let account = current.pair.story.account_id;
        drop(store);
        // Boundary fixture only: reaching this revision by billions of API calls would
        // not exercise different behavior. All I/O remains inside the temporary store.
        let path = root.join("progression").join(account.0.to_string()).join("Fixture.character.json");
        let mut record = read_json(&path);
        record["field_visit"]["revision"] = json!(u64::MAX);
        write_json(&path, &record);
        let store = open(&root);
        let before = store.get_scene_state(account, GAME).unwrap();
        assert_eq!(before.field_visit.as_ref().unwrap().revision, u64::MAX);
        let mut next = match kind {
            SceneOperationKind::AttachVisit => attachment(&current),
            SceneOperationKind::AdvanceVisit => advancement(&current, 1),
            SceneOperationKind::CompleteVisit => completion(&current),
            _ => unreachable!(),
        };
        next.field_visit.as_mut().unwrap().revision = 0;
        let op = if kind == SceneOperationKind::AdvanceVisit { advance_operation(1) } else { operation(kind) };
        assert_refused(&store, &root, op,
            std::slice::from_ref(&before), &[next], "revision overflow");
    }
}

#[test]
fn legacy_operations_preserve_an_idle_visit_and_cannot_complete_its_attached_scene() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let idle = enter(&store, &seeded(&store, "Legacy operations"));
    let mut reward = idle.clone();
    reward.pair.character.as_mut().unwrap().gil += 7;
    let saved = store.commit_scene_batch(&operation(SceneOperationKind::BattleOutcome), b"fixture battle reward",
        std::slice::from_ref(&idle), std::slice::from_ref(&reward)).unwrap();
    assert_eq!(saved.current, vec![reward.clone()]);
    assert_eq!(saved.current[0].field_visit, idle.field_visit);
    let mut dropped = reward.clone();
    dropped.field_visit = None;
    assert_refused(&store, &root, SceneOperation { instance: [8; 32], ..operation(SceneOperationKind::BattleOutcome) },
        std::slice::from_ref(&reward), &[dropped], "legacy outcome cannot discard a visit");
    let attached = attach(&store, &reward);
    let complete = completion(&attached);
    assert_refused(&store, &root, operation(SceneOperationKind::Complete),
        std::slice::from_ref(&attached), std::slice::from_ref(&complete), "legacy completion cannot change visit metadata");
    let mut unchanged_visit = complete;
    unchanged_visit.field_visit = attached.field_visit.clone();
    assert_refused(&store, &root, operation(SceneOperationKind::Complete),
        std::slice::from_ref(&attached), &[unchanged_visit], "legacy completion cannot leave a dangling link");
}
