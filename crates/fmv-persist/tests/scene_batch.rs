//! Multi-account scene checkpoints and rewards through the public durable transaction API.
//! Every record used here is synthetic and lives in a temporary store.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_persist::{
    restore_snapshot, CharacterProgress, PersistError, Progression, SceneCheckpoint, SceneCommit,
    SceneOperation, SceneOperationKind, ScenePhase, SceneReservation, SceneState, Store,
};

const GAME: GameId = GameId::Ff7;
const INSTANCE: [u8; 32] = [7; 32];
const PREPARED: &[u8] = b"fixture prepare battle zero";
const RESOLVED: &[u8] = b"fixture battle zero won";
const FINISHED: &[u8] = b"fixture final landing";

fn accounts(store: &Store, count: usize) -> Vec<AccountId> {
    (0..count).map(|slot| {
        let (account, _) = store.create_account(&format!("Member {slot}"), NOW).unwrap();
        store.put_progression(&Progression::new(account, GAME, vec![1, slot as u8], NOW)).unwrap();
        let progress = CharacterProgress { exp: 100 + slot as u32, ap: slot as u32,
            gil: 240 + slot as u32, inventory: Some(vec![InventoryEntry::new(0, 3)]),
            ..CharacterProgress::new(account, GAME, slot as u8, 6, NOW) };
        store.put_character_progress(&progress).unwrap();
        account
    }).collect()
}

fn states(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> {
    accounts.iter().map(|&account| store.get_scene_state(account, GAME).unwrap()).collect()
}

fn operation(kind: SceneOperationKind, ordinal: u32) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal, attempt: 0, kind }
}

fn preparation(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    let owner = &before[0].pair;
    next[0].checkpoint = Some(SceneCheckpoint {
        instance: INSTANCE,
        scene_key: "fixture/interrupted-scene".into(),
        source_hash: [9; 32],
        replay_version: 1,
        original_story: owner.story.blob.clone(),
        original_gil: owner.character.as_ref().unwrap().gil,
        seed: 42,
        replay: Vec::new(),
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 12, battle_seed: u64::MAX - 9 },
        consumed_battles: 0,
    });
    for (slot, state) in next.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    next
}

fn resolution(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    let consumed = before[0].checkpoint.as_ref().unwrap().consumed_battles + 1;
    for (slot, state) in next.iter_mut().enumerate() {
        let progress = state.pair.character.as_mut().unwrap();
        progress.exp += 50 + slot as u32;
        progress.ap += 3;
        progress.gil += 10;
        progress.limit += 1;
        let inventory = progress.inventory.as_mut().unwrap();
        if let Some(stack) = inventory.iter_mut().find(|stack| stack.item == 2) { stack.count += 1; }
        else { inventory.push(InventoryEntry::new(2, 1)); }
        progress.updated_at = NOW + u64::from(consumed);
    }
    next[0].pair.story.blob.push(8);
    next[0].pair.story.updated_at = NOW + u64::from(consumed);
    let checkpoint = next[0].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::Resume { segment: consumed };
    checkpoint.consumed_battles = consumed;
    checkpoint.replay.push(1);
    next
}

fn finishing(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    for state in &mut next { state.reservation = None; }
    let checkpoint = next[0].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::Landing { destination: b"fixture/landing".to_vec() };
    checkpoint.replay.push(2);
    let time = NOW + u64::from(checkpoint.consumed_battles) + 1;
    next[0].pair.story.blob.push(9);
    next[0].pair.story.updated_at = time;
    next[0].pair.character.as_mut().unwrap().gil -= 1;
    next[0].pair.character.as_mut().unwrap().updated_at = time;
    next
}

fn comparable(mut state: SceneState) -> SceneState {
    // Receipt/checkpoint envelopes raise the on-disk schema, but do not change the requested
    // resource values. All other fields, including timestamps and opaque replay bytes, matter.
    if let Some(progress) = &mut state.pair.character { progress.schema_version = 1; }
    state
}

fn assert_states(actual: &[SceneState], requested: &[SceneState]) {
    assert_eq!(actual.iter().cloned().map(comparable).collect::<Vec<_>>(), requested.iter().cloned().map(comparable).collect::<Vec<_>>());
}

fn prepare(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> {
    let before = states(store, accounts);
    let next = preparation(&before);
    let committed = store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle, 0), PREPARED, &before, &next).unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    assert_states(&committed.current, &next);
    committed.current
}

#[test]
fn two_and_three_member_batches_preserve_atomic_rewards_across_reopen_and_duplicate_operations() {
    for count in [2, 3] {
        let (_dir, root) = fresh_root();
        let store = open(&root);
        let accounts = accounts(&store, count);
        let original = states(&store, &accounts);
        let prepare_op = operation(SceneOperationKind::PrepareBattle, 0);
        assert!(!store.scene_batch_applied(&prepare_op, PREPARED, &accounts, GAME).unwrap());
        let prepared = prepare(&store, &accounts);
        for &account in &accounts {
            assert_eq!(store.get_scene_group(account, GAME).unwrap(), prepared,
                "any participant can recover the complete battle with its checkpoint owner first");
        }
        assert!(store.scene_batch_applied(&prepare_op, PREPARED, &accounts, GAME).unwrap());
        let duplicate = store.commit_scene_batch(&prepare_op, PREPARED, &original, &preparation(&original)).unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, prepared);
        drop(store);

        let store = open(&root);
        assert_eq!(states(&store, &accounts), prepared, "all reservations and owner replay survive restart");
        assert_eq!(store.get_scene_group(*accounts.last().unwrap(), GAME).unwrap(), prepared);
        let after = resolution(&prepared);
        let resolve_op = operation(SceneOperationKind::ResolveBattle, 0);
        let committed = store.commit_scene_batch(&resolve_op, RESOLVED, &prepared, &after).unwrap();
        assert_eq!(committed.commit, SceneCommit::Applied);
        assert_states(&committed.current, &after);
        assert_eq!(committed.current.iter().map(|state| &state.reservation).collect::<Vec<_>>(),
            prepared.iter().map(|state| &state.reservation).collect::<Vec<_>>(), "winning retains the complete group's completed boundary until Finish");
        assert!(committed.current[1..].iter().all(|state| state.checkpoint.is_none()));
        let owner = committed.current[0].checkpoint.as_ref().unwrap();
        assert_eq!(owner.original_story, original[0].pair.story.blob);
        assert_eq!(owner.original_gil, original[0].pair.character.as_ref().unwrap().gil);
        assert_eq!(owner.consumed_battles, 1);
        assert_eq!(owner.phase, ScenePhase::Resume { segment: 1 });
        assert_eq!(committed.current[1].pair.story, original[1].pair.story, "a follower keeps its own story");
        drop(store);

        let store = open(&root);
        assert!(store.scene_batch_applied(&resolve_op, RESOLVED, &accounts, GAME).unwrap());
        let settled = states(&store, &accounts);
        for &account in &accounts {
            assert_eq!(store.get_scene_group(account, GAME).unwrap(), settled,
                "resolved participants remain one frozen group while the owner resumes");
        }
        let bytes = file_map(&root);
        for (op, chosen, before, next) in [
            (&prepare_op, PREPARED, &original, preparation(&original)),
            (&resolve_op, RESOLVED, &prepared, after),
        ] {
            let duplicate = store.commit_scene_batch(op, chosen, before, &next).unwrap();
            assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
            assert_eq!(duplicate.current, settled, "a duplicate returns the current records without replaying its old metadata");
            assert_eq!(file_map(&root), bytes);
        }
    }
}

#[test]
fn a_recorded_operation_rejects_a_different_result_without_touching_any_member() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 3);
    let before = states(&store, &accounts);
    prepare(&store, &accounts);
    let op = operation(SceneOperationKind::PrepareBattle, 0);
    let bytes = file_map(&root);
    assert!(matches!(store.scene_batch_applied(&op, b"a different selection", &accounts, GAME), Err(PersistError::TransactionConflict(_))));
    assert!(matches!(store.commit_scene_batch(&op, b"a different selection", &before, &preparation(&before)), Err(PersistError::TransactionConflict(_))));
    assert_eq!(file_map(&root), bytes);
    assert!(store.scene_batch_applied(&op, PREPARED, &accounts, GAME).unwrap(), "the known receipt remains usable after refusal");
}

#[test]
fn recovering_from_a_guest_returns_the_checkpoint_owner_first_even_when_its_slot_is_last() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 3);
    let before = states(&store, &accounts);
    let mut next = preparation(&before);
    for (state, slot) in next.iter_mut().zip([2, 0, 1]) {
        state.reservation.as_mut().unwrap().slot = slot;
    }
    let committed = store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle, 0), PREPARED, &before, &next).unwrap();
    for &guest in &accounts[1..] {
        let group = store.get_scene_group(guest, GAME).unwrap();
        assert_eq!(group, committed.current);
        assert!(group[0].checkpoint.is_some());
        assert_eq!(group.iter().map(|state| state.reservation.as_ref().unwrap().slot).collect::<Vec<_>>(), [2, 0, 1]);
    }
}

#[test]
fn a_stale_checkpoint_is_a_cas_conflict_even_when_the_story_and_money_are_current() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let prepared = prepare(&store, &accounts);
    let mut stale = prepared.clone();
    stale[0].checkpoint.as_mut().unwrap().replay.push(99);
    let next = resolution(&stale);
    let bytes = file_map(&root);
    assert!(matches!(store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED, &stale, &next), Err(PersistError::TransactionConflict(_))));
    assert_eq!(states(&store, &accounts), prepared);
    assert_eq!(file_map(&root), bytes);
}

#[test]
fn malformed_reservations_cannot_commit_a_partial_or_ambiguous_roster() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 3);
    let before = states(&store, &accounts);
    let valid = preparation(&before);
    let bytes = file_map(&root);
    for malformed in 0..4 {
        let mut next = valid.clone();
        match malformed {
            0 => next[1].reservation.as_mut().unwrap().slot = 0,
            1 => next[1].reservation.as_mut().unwrap().ordinal = 1,
            2 => next[1].reservation.as_mut().unwrap().attempt = 1,
            3 => next[0].reservation = None,
            _ => unreachable!(),
        }
        assert!(store.commit_scene_batch(&operation(SceneOperationKind::PrepareBattle, 0), PREPARED, &before, &next).is_err(), "malformed case {malformed}");
        assert_eq!(file_map(&root), bytes);
    }
    assert_eq!(states(&store, &accounts), before);
}

#[test]
fn resolving_a_battle_requires_its_complete_original_roster() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 4);
    let prepared = prepare(&store, &accounts[..3]);
    let after = resolution(&prepared);
    let op = operation(SceneOperationKind::ResolveBattle, 0);
    let bytes = file_map(&root);
    assert!(store.commit_scene_batch(&op, RESOLVED, &prepared[..2], &after[..2]).is_err(), "a missing third member cannot silently lose its reservation or reward");
    assert_eq!(file_map(&root), bytes);
    let mut substituted = prepared.clone();
    substituted[2] = store.get_scene_state(accounts[3], GAME).unwrap();
    let mut substitute_after = after;
    substitute_after[2] = substituted[2].clone();
    assert!(store.commit_scene_batch(&op, RESOLVED, &substituted, &substitute_after).is_err(), "an unrelated account cannot replace a reserved participant");
    assert_eq!(file_map(&root), bytes);
    assert_eq!(states(&store, &accounts[..3]), prepared);
}

#[test]
fn active_checkpoints_and_reservations_fence_ordinary_writers_and_old_scene_transactions() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let prepared = prepare(&store, &accounts);
    let bytes = file_map(&root);
    for state in &prepared {
        let account = state.pair.story.account_id;
        assert!(matches!(store.put_progression(&state.pair.story), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.update_progression(account, GAME, |_| panic!("an active scene cannot invoke an ordinary story writer")), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.put_character_progress(state.pair.character.as_ref().unwrap()), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.update_character_progress(account, GAME, |_| panic!("an active scene cannot invoke an ordinary resource writer")), Err(PersistError::TransactionConflict(_))));
        let mut next = state.pair.clone();
        next.character.as_mut().unwrap().gil -= 1;
        assert!(matches!(store.commit_scene_progress("fixture/old-scene", &state.pair, &next), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.delete_account(account, NOW + 10), Err(PersistError::TransactionConflict(_))));
        assert_eq!(file_map(&root), bytes, "refusal changes no participant or durable metadata");
    }
    let result = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED, &prepared, &resolution(&prepared)).unwrap();
    let resumed_bytes = file_map(&root);
    for state in &result.current {
        let account = state.pair.story.account_id;
        assert!(matches!(store.put_progression(&state.pair.story), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.update_character_progress(account, GAME, |_| panic!("guests remain frozen after the reward until the scene finishes")), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(store.delete_account(account, NOW + 10), Err(PersistError::TransactionConflict(_))));
        assert_eq!(file_map(&root), resumed_bytes);
    }
    let finished = store.commit_scene_batch(&operation(SceneOperationKind::Finish, 1), FINISHED,
        &result.current, &finishing(&result.current)).unwrap();
    let follower = finished.current[1].pair.character.as_ref().unwrap();
    store.update_character_progress(accounts[1], GAME, |old| {
        let mut record = old.unwrap();
        record.gil += 17;
        record
    }).expect("Finish releases the follower for later ordinary rewards");
    assert_eq!(store.get_character_progress(accounts[1], GAME).unwrap().unwrap().gil, follower.gil + 17);
    let duplicate = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED, &prepared, &resolution(&prepared)).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current[1].pair.character.as_ref().unwrap().gil, follower.gil + 17, "a repeated battle result preserves later rewards");
    assert!(matches!(store.put_progression(&finished.current[0].pair.story), Err(PersistError::TransactionConflict(_))), "the owner remains protected by its durable landing");
}

#[test]
fn finishing_after_the_battle_keeps_the_landing_checkpoint_and_commits_the_final_pair_once() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let prepared = prepare(&store, &accounts);
    let resumed = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED, &prepared, &resolution(&prepared)).unwrap().current;
    let before = resumed;
    let next = finishing(&before);
    let op = operation(SceneOperationKind::Finish, 1);
    let bytes_before = file_map(&root);
    assert!(store.commit_scene_batch(&op, FINISHED, &before[..1], &next[..1]).is_err(), "Finish cannot omit a guest's reservation");
    assert_eq!(file_map(&root), bytes_before);
    let result = store.commit_scene_batch(&op, FINISHED, &before, &next).unwrap();
    assert_eq!(result.commit, SceneCommit::Applied);
    assert_states(&result.current, &next);
    assert!(matches!(result.current[0].checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    assert!(result.current.iter().all(|state| state.reservation.is_none()));
    drop(store);
    let store = open(&root);
    assert!(store.scene_batch_applied(&op, FINISHED, &accounts, GAME).unwrap());
    for (index, &account) in accounts.iter().enumerate() {
        assert_eq!(store.get_scene_group(account, GAME).unwrap(), vec![result.current[index].clone()]);
    }
    let bytes = file_map(&root);
    let duplicate = store.commit_scene_batch(&op, FINISHED, &before, &next).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, result.current);
    assert!(matches!(store.put_progression(&next[0].pair.story), Err(PersistError::TransactionConflict(_))), "Landing remains protected until the separate lifecycle acknowledgment");
    assert_eq!(file_map(&root), bytes);
}

#[test]
fn the_next_battle_moves_the_complete_frozen_group_to_its_new_boundary() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 4);
    let prepared = prepare(&store, &accounts[..3]);
    let resumed = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED,
        &prepared, &resolution(&prepared)).unwrap().current;
    let mut next = resumed.clone();
    let checkpoint = next[0].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::BeforeBattle { ordinal: 1, attempt: 0, formation: 13, battle_seed: 73 };
    checkpoint.replay.push(2);
    for state in &mut next { state.reservation.as_mut().unwrap().ordinal = 1; }
    let op = operation(SceneOperationKind::PrepareBattle, 1);
    let chosen = b"fixture prepare battle one";
    let bytes = file_map(&root);
    assert!(store.commit_scene_batch(&op, chosen, &resumed[..2], &next[..2]).is_err(), "the next battle cannot drop a reserved guest");
    let mut substituted = resumed.clone();
    substituted[2] = store.get_scene_state(accounts[3], GAME).unwrap();
    let mut substituted_next = next.clone();
    substituted_next[2].pair = substituted[2].pair.clone();
    assert!(store.commit_scene_batch(&op, chosen, &substituted, &substituted_next).is_err(), "the next battle cannot recruit a replacement for a frozen guest");
    assert_eq!(file_map(&root), bytes);
    let committed = store.commit_scene_batch(&op, chosen, &resumed, &next).unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    assert_states(&committed.current, &next);
    assert!(committed.current.iter().all(|state| state.reservation.as_ref().is_some_and(|r| r.ordinal == 1 && r.attempt == 0)));
    drop(store);

    let store = open(&root);
    for &account in &accounts[..3] {
        assert_eq!(store.get_scene_group(account, GAME).unwrap(), committed.current);
    }
    let resolved = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 1), b"fixture battle one won",
        &committed.current, &resolution(&committed.current)).unwrap();
    assert_eq!(resolved.current[0].checkpoint.as_ref().unwrap().phase, ScenePhase::Resume { segment: 2 });
    for &account in &accounts[..3] {
        assert_eq!(store.get_scene_group(account, GAME).unwrap(), resolved.current);
        assert!(matches!(store.update_character_progress(account, GAME, |_| panic!("the second reward also retains the group")), Err(PersistError::TransactionConflict(_))));
    }
    let finished = store.commit_scene_batch(&operation(SceneOperationKind::Finish, 2), FINISHED,
        &resolved.current, &finishing(&resolved.current)).unwrap();
    assert!(finished.current.iter().all(|state| state.reservation.is_none()));
    assert!(store.get_scene_state(accounts[3], GAME).unwrap().checkpoint.is_none(), "the unrelated account never joined this continuation");
}

#[test]
fn landing_continues_as_a_fresh_instance_and_completion_releases_writes_without_losing_receipts() {
    const NEXT_INSTANCE: [u8; 32] = [8; 32];
    const CONTINUED: &[u8] = b"fixture landing continues into instance eight";
    const COMPLETED: &[u8] = b"fixture instance eight completed";
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let prepared = prepare(&store, &accounts);
    let resumed = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED,
        &prepared, &resolution(&prepared)).unwrap().current;
    let finished = store.commit_scene_batch(&operation(SceneOperationKind::Finish, 1), FINISHED,
        &resumed, &finishing(&resumed)).unwrap().current;
    let before = vec![finished[0].clone()];
    let mut next = before.clone();
    next[0].checkpoint = Some(SceneCheckpoint {
        instance: NEXT_INSTANCE,
        scene_key: "fixture/landing-scene".into(),
        source_hash: [10; 32],
        replay_version: 2,
        original_story: before[0].pair.story.blob.clone(),
        original_gil: before[0].pair.character.as_ref().unwrap().gil,
        seed: 43,
        replay: Vec::new(),
        phase: ScenePhase::Resume { segment: 0 },
        consumed_battles: 0,
    });
    let continue_op = operation(SceneOperationKind::ContinueLanding, 1);
    let bytes = file_map(&root);
    for malformed in 0..4 {
        let mut wrong = next.clone();
        let checkpoint = wrong[0].checkpoint.as_mut().unwrap();
        match malformed {
            0 => checkpoint.instance = INSTANCE,
            1 => checkpoint.original_story.push(99),
            2 => checkpoint.original_gil += 1,
            3 => { checkpoint.phase = ScenePhase::Resume { segment: 1 }; checkpoint.consumed_battles = 1; }
            _ => unreachable!(),
        }
        assert!(store.commit_scene_batch(&continue_op, CONTINUED, &before, &wrong).is_err(), "continuation case {malformed} cannot replace the exact landing with a different original input");
    }
    let wrong_op = SceneOperation { instance: NEXT_INSTANCE, ..continue_op };
    assert!(store.commit_scene_batch(&wrong_op, CONTINUED, &before, &next).is_err(), "the continuation operation belongs to the previous landed instance");
    assert_eq!(file_map(&root), bytes);
    let continued = store.commit_scene_batch(&continue_op, CONTINUED, &before, &next).unwrap();
    assert_eq!(continued.commit, SceneCommit::Applied);
    assert_states(&continued.current, &next);
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_scene_group(accounts[0], GAME).unwrap(), continued.current);
    let replayed = store.commit_scene_batch(&continue_op, CONTINUED, &before, &next).unwrap();
    assert_eq!(replayed.commit, SceneCommit::AlreadyApplied);
    assert_eq!(replayed.current[0].checkpoint.as_ref().unwrap().instance, NEXT_INSTANCE);
    assert!(matches!(store.commit_scene_batch(&continue_op, b"another destination scene", &before, &next), Err(PersistError::TransactionConflict(_))));
    assert!(matches!(store.update_character_progress(accounts[0], GAME, |_| panic!("the new scene still owns the resources")), Err(PersistError::TransactionConflict(_))));

    let complete_before = continued.current;
    let mut complete_next = complete_before.clone();
    complete_next[0].checkpoint = None;
    complete_next[0].pair.story.blob.push(10);
    complete_next[0].pair.story.updated_at = NOW + 3;
    complete_next[0].pair.character.as_mut().unwrap().gil -= 2;
    complete_next[0].pair.character.as_mut().unwrap().updated_at = NOW + 3;
    let complete_op = SceneOperation { instance: NEXT_INSTANCE, ordinal: 0, attempt: 0, kind: SceneOperationKind::Complete };
    let bytes = file_map(&root);
    assert!(store.commit_scene_batch(&operation(SceneOperationKind::Complete, 0), COMPLETED, &complete_before, &complete_next).is_err(), "the old scene instance cannot complete its successor");
    assert_eq!(file_map(&root), bytes);
    let completed = store.commit_scene_batch(&complete_op, COMPLETED, &complete_before, &complete_next).unwrap();
    assert_eq!(completed.commit, SceneCommit::Applied);
    assert_states(&completed.current, &complete_next);
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_scene_group(accounts[0], GAME).unwrap(), completed.current);
    assert!(completed.current[0].checkpoint.is_none() && completed.current[0].reservation.is_none());
    store.update_character_progress(accounts[0], GAME, |old| {
        let mut record = old.unwrap(); record.gil += 17; record
    }).expect("Complete releases ordinary rewards");
    store.update_progression(accounts[0], GAME, |old| {
        let mut record = old; record.blob.push(11); record
    }).expect("Complete releases ordinary story writes");
    let latest = states(&store, &accounts[..1]);
    let bytes = file_map(&root);
    for (op, selected, old, new) in [
        (&continue_op, CONTINUED, &before, &next),
        (&complete_op, COMPLETED, &complete_before, &complete_next),
    ] {
        assert!(store.scene_batch_applied(op, selected, &accounts[..1], GAME).unwrap(), "ordinary writes retain both lifecycle receipts");
        let duplicate = store.commit_scene_batch(op, selected, old, new).unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, latest, "an old completion cannot rewind the resources or resurrect its checkpoint");
        assert_eq!(file_map(&root), bytes);
    }
    assert!(store.scene_batch_applied(&operation(SceneOperationKind::Finish, 1), FINISHED, &accounts, GAME).unwrap(), "the earlier full-group receipt also survives");
}

#[test]
fn a_snapshot_preserves_every_reservation_and_can_resume_without_changing_its_source() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 3);
    let prepared = prepare(&store, &accounts);
    let snapshot = dir.path().join("snapshot");
    store.snapshot(&snapshot).unwrap();
    let copy = dir.path().join("restored");
    restore_snapshot(&snapshot, &copy).unwrap();
    let restored = open(&copy);
    assert_eq!(states(&restored, &accounts), prepared);
    let source = file_map(&root);
    let committed = restored.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED, &prepared, &resolution(&prepared)).unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    assert_states(&committed.current, &resolution(&prepared));
    assert_eq!(states(&store, &accounts), prepared);
    assert_eq!(file_map(&root), source);
}

fn character_file(root: &Path, account: AccountId) -> std::path::PathBuf {
    root.join("progression").join(account.0.to_string()).join("Ff7.character.json")
}

#[test]
fn a_finished_guest_can_be_restored_under_a_new_account_without_losing_rewards_or_batch_receipts() {
    let (_dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let prepared = prepare(&store, &accounts);
    let resumed = store.commit_scene_batch(&operation(SceneOperationKind::ResolveBattle, 0), RESOLVED,
        &prepared, &resolution(&prepared)).unwrap().current;
    let finished = store.commit_scene_batch(&operation(SceneOperationKind::Finish, 1), FINISHED,
        &resumed, &finishing(&resumed)).unwrap().current;
    let earned = finished[1].clone();
    assert!(earned.checkpoint.is_none() && earned.reservation.is_none(), "Finish releases the guest for deletion");
    assert_eq!(earned.pair.character.as_ref().unwrap().inventory, Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(2, 1)]));
    let original_envelope = read_json(&character_file(&root, accounts[1]));
    assert_eq!(original_envelope["schema_version"], 3);
    assert_eq!(original_envelope["batch_receipts"].as_object().unwrap().len(), 3);
    let owner_bytes = fs::read(character_file(&root, accounts[0])).unwrap();

    let deleted = store.delete_account(accounts[1], NOW + 10).unwrap();
    // A completed archive may also be moved into a separate fresh store. Installing its
    // v4 receipts must first upgrade that store's format fence, even without its old party.
    let (_copy_dir, copy_root) = fresh_root();
    let copy = open(&copy_root);
    assert_eq!(log_lines(&copy_root)[0]["schema_version"], 2);
    let archive = root.join("trash").join(&deleted.entry);
    let copy_archive = copy_root.join("trash").join(&deleted.entry);
    for (relative, bytes) in file_map(&archive) {
        let target = copy_archive.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    let copied = copy.restore_account(&deleted.entry, NOW + 11).unwrap();
    assert!(copied.unfinished.is_empty());
    assert_eq!(log_lines(&copy_root)[0]["schema_version"], 4);
    assert_eq!(read_json(&copy_root.join("index.json"))["schema_version"], 2, "the legacy marker stays at v2; index.log carries the v4 fence");
    let mut copy_expected = earned.clone();
    copy_expected.pair.story.account_id = copied.account_id;
    copy_expected.pair.character.as_mut().unwrap().account_id = copied.account_id;
    drop(copy);
    let copy = open(&copy_root);
    assert_eq!(copy.get_scene_state(copied.account_id, GAME).unwrap(), copy_expected);
    assert_eq!(read_json(&character_file(&copy_root, copied.account_id))["batch_receipts"], original_envelope["batch_receipts"]);

    let restored = store.restore_account(&deleted.entry, NOW + 11).unwrap();
    assert!(restored.unfinished.is_empty());
    assert_ne!(restored.account_id, accounts[1]);
    let mut expected = earned;
    expected.pair.story.account_id = restored.account_id;
    expected.pair.character.as_mut().unwrap().account_id = restored.account_id;
    assert_eq!(store.get_scene_state(restored.account_id, GAME).unwrap(), expected,
        "restore changes identity only, preserving exact resources, inventory, story and timestamps");
    let restored_envelope = read_json(&character_file(&root, restored.account_id));
    assert_eq!(restored_envelope["schema_version"], 3);
    assert_eq!(restored_envelope["batch_receipts"], original_envelope["batch_receipts"]);
    assert_eq!(fs::read(character_file(&root, accounts[0])).unwrap(), owner_bytes, "the owner's pending landing remains intact");
    let rekeyed_roster = [accounts[0], restored.account_id];
    drop(store);

    let store = open(&root);
    assert_eq!(store.get_scene_group(restored.account_id, GAME).unwrap(), vec![expected]);
    for (op, selected) in [
        (operation(SceneOperationKind::PrepareBattle, 0), PREPARED),
        (operation(SceneOperationKind::ResolveBattle, 0), RESOLVED),
        (operation(SceneOperationKind::Finish, 1), FINISHED),
    ] {
        assert!(store.scene_batch_applied(&op, selected, &rekeyed_roster, GAME).unwrap(),
            "the completed receipt's cohort count and slot survive account rekeying");
    }
}

#[test]
fn restoration_refuses_an_archived_checkpoint_or_reservation_without_rekeying_it() {
    let (dir, root) = fresh_root();
    let store = open(&root);
    let accounts = accounts(&store, 2);
    let historical = dir.path().join("historical");
    store.snapshot(&historical).unwrap();
    let historical_store = open(&historical);
    let deleted: Vec<_> = accounts.iter().map(|&account| historical_store.delete_account(account, NOW + 10).unwrap()).collect();
    prepare(&store, &accounts);
    for (&account, deleted) in accounts.iter().zip(&deleted) {
        // Simulate an old/manual archive containing active metadata. Public deletion itself
        // refuses this state, and restore must also reject it rather than changing account ids.
        let archived = character_file(&historical.join("trash").join(&deleted.entry), account);
        fs::copy(character_file(&root, account), archived).unwrap();
        let bytes = file_map(&historical);
        assert!(matches!(historical_store.restore_check(&deleted.entry), Err(PersistError::TransactionConflict(_))));
        assert!(matches!(historical_store.restore_account(&deleted.entry, NOW + 11), Err(PersistError::TransactionConflict(_))));
        assert_eq!(file_map(&historical), bytes);
    }
}
