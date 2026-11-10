//! Ordinary battle outcomes use the recoverable group transaction without inventing a
//! scene checkpoint. The operation identity, not a story value, deduplicates its rewards.

use super::*;

const BATTLE_INSTANCE: [u8; 32] = [71; 32];
const RESULT: &[u8] = b"ordinary battle: owner story and complete roster settlement";

fn operation() -> SceneOperation {
    SceneOperation { instance: BATTLE_INSTANCE, ordinal: 0, attempt: 0, kind: SceneOperationKind::BattleOutcome }
}

fn pairs(states: &[SceneState]) -> Vec<ProgressPair> { states.iter().map(|state| state.pair.clone()).collect() }

fn outcome(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    next[0].pair.story.blob.extend_from_slice(&[0, 255, 0, 19]);
    next[0].pair.story.updated_at = 8;
    for (slot, state) in next.iter_mut().enumerate() {
        let character = state.pair.character.as_mut().unwrap();
        character.gil += 10 + slot as u32;
        character.exp += 20 + slot as u32;
        character.ap += 3;
        character.limit = 30 + slot as u16;
        character.inventory = Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(8, 1 + slot as u16)]);
        character.updated_at = 8;
    }
    next
}

fn committed(store: &Store, before: &[SceneState], next: &[SceneState]) -> SceneBatchCommitted {
    store.commit_battle_outcome(BATTLE_INSTANCE, RESULT, &pairs(before), &pairs(next)).unwrap()
}

fn record_bytes(store: &Store, accounts: &[AccountId]) -> Vec<Vec<u8>> {
    accounts.iter().flat_map(|account| [store.progression_path(account.0, GAME).unwrap(),
        store.character_path(account.0, GAME).unwrap()]).map(|path| std::fs::read(path).unwrap()).collect()
}

#[test]
fn ordinary_outcome_commits_all_members_without_creating_a_scene_and_replays_only_its_receipt() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let next = outcome(&before);
    assert!(!store.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts, GAME).unwrap());
    let saved = committed(&store, &before, &next);
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(comparable(saved.current.clone()), comparable(next));
    assert!(store.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts, GAME).unwrap());
    for state in &saved.current {
        assert!(state.checkpoint.is_none() && state.reservation.is_none());
        assert_eq!(store.get_scene_group(state.pair.story.account_id, GAME).unwrap(), vec![state.clone()],
            "a receipt-only character has no synthetic battle recovery group");
    }
    assert_eq!(saved.current[1].pair.story, before[1].pair.story);
    assert_eq!(saved.current[2].pair.story, before[2].pair.story);

    // The later writer preserves receipts. An old delivery returns this current pair,
    // rather than reinstalling the first battle's after-image or paying it a second time.
    let mut later = saved.current[1].pair.character.clone().unwrap();
    later.gil += 100;
    store.put_character_progress(&later).unwrap();
    let current = store.get_scene_states(&accounts, GAME).unwrap();
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    let repeated = committed(&reopened, &before, &outcome(&before));
    assert_eq!(repeated.commit, SceneCommit::AlreadyApplied);
    assert_eq!(repeated.current, current);
    assert!(reopened.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts, GAME).unwrap());
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn ordinary_outcome_refuses_stale_records_changed_results_and_partial_cohorts_without_writing() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let next = outcome(&before);
    let original = record_bytes(&store, &accounts);
    for change_story in [true, false] {
        let mut stale = before.clone();
        if change_story { stale[0].pair.story.blob.push(9); }
        else { stale[2].pair.character.as_mut().unwrap().gil += 1; }
        assert!(matches!(store.commit_battle_outcome(BATTLE_INSTANCE, RESULT, &pairs(&stale), &pairs(&next)),
            Err(PersistError::TransactionConflict(_))));
        assert_eq!(record_bytes(&store, &accounts), original);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
    let saved = committed(&store, &before, &next);
    let bytes = record_bytes(&store, &accounts);
    assert!(matches!(store.commit_battle_outcome(BATTLE_INSTANCE, b"different outcome", &pairs(&before), &pairs(&next)),
        Err(PersistError::TransactionConflict(_))));
    assert!(matches!(store.battle_outcome_applied(BATTLE_INSTANCE, b"different outcome", &accounts, GAME),
        Err(PersistError::TransactionConflict(_))));
    assert!(matches!(store.commit_battle_outcome(BATTLE_INSTANCE, RESULT, &pairs(&before[..2]), &pairs(&next[..2])),
        Err(PersistError::TransactionConflict(_))));
    assert!(matches!(store.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts[..2], GAME),
        Err(PersistError::TransactionConflict(_))));
    assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), saved.current);
    assert_eq!(record_bytes(&store, &accounts), bytes);
}

#[test]
fn ordinary_outcome_cannot_replace_an_active_scene_or_its_reserved_guest() {
    let (dir, store, accounts) = rig();
    let initial = store.get_scene_states(&accounts, GAME).unwrap();
    let initial = committed(&store, &initial, &initial).current;
    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    let bytes = record_bytes(&store, &accounts);
    // The public ordinary API intentionally carries only pairs. Stripping metadata from
    // its arguments must not strip the actual durable checkpoint/reservations on disk.
    for cohort in [&prepared[..], &prepared[1..2]] {
        let mut next = pairs(cohort);
        next[0].character.as_mut().unwrap().gil += 1;
        assert!(matches!(store.commit_battle_outcome(BATTLE_INSTANCE, RESULT, &pairs(cohort), &next),
            Err(PersistError::TransactionConflict(_))));
        assert_eq!(record_bytes(&store, &accounts), bytes);
    }
    assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), prepared);
    for instance in [BATTLE_INSTANCE, [72; 32]] {
        assert!(matches!(store.battle_outcome_applied(instance, RESULT, &accounts, GAME),
            Err(PersistError::TransactionConflict(_))), "neither a duplicate nor a new outcome may bypass the active hold");
    }
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn ordinary_outcome_requires_stored_characters_and_a_nonzero_stable_instance() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let bytes = record_bytes(&store, &accounts);
    assert!(store.commit_battle_outcome([0; 32], RESULT, &pairs(&before), &pairs(&outcome(&before))).is_err());
    let (missing, _) = store.create_account("No progress yet", 2).unwrap();
    let empty = store.get_progress_pair(missing, GAME).unwrap();
    assert!(empty.character.is_none());
    assert!(store.commit_battle_outcome(BATTLE_INSTANCE, RESULT, std::slice::from_ref(&empty), std::slice::from_ref(&empty)).is_err());
    assert!(store.get_character_progress(missing, GAME).unwrap().is_none(), "a terminal outcome cannot invent character defaults");
    assert!(!store.progression_path(missing.0, GAME).unwrap().exists());
    assert_eq!(record_bytes(&store, &accounts), bytes);
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn ordinary_battle_readiness_returns_one_coherent_roster_and_refuses_missing_or_held_members() {
    let (dir, store, accounts) = rig();
    let initial = store.get_scene_states(&accounts, GAME).unwrap();
    let bytes = record_bytes(&store, &accounts);
    assert_eq!(store.get_battle_outcome_states(&accounts, GAME).unwrap(), initial);
    assert_eq!(record_bytes(&store, &accounts), bytes);

    let (missing, _) = store.create_account("No battle record", 2).unwrap();
    let mut mixed = accounts.clone();
    mixed.push(missing);
    assert!(matches!(store.get_battle_outcome_states(&mixed, GAME), Err(PersistError::TransactionConflict(_))));
    assert!(store.get_character_progress(missing, GAME).unwrap().is_none());
    assert!(!store.progression_path(missing.0, GAME).unwrap().exists());
    assert_eq!(record_bytes(&store, &accounts), bytes);

    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    let bytes = record_bytes(&store, &accounts);
    for cohort in [&accounts[..], &accounts[..1], &accounts[1..2]] {
        assert!(matches!(store.get_battle_outcome_states(cohort, GAME), Err(PersistError::TransactionConflict(_))),
            "both a checkpoint owner and its reserved guest must be refused");
        assert_eq!(record_bytes(&store, &accounts), bytes);
    }
    assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), prepared);
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn ordinary_battle_readiness_requires_a_free_combined_receipt_slot_in_every_member() {
    let (dir, store, accounts) = rig();
    let last_path = store.character_path(accounts[2].0, GAME).unwrap();
    let mut record = CharacterRecord::read(&last_path).unwrap().unwrap();
    // Construct a valid history directly: exercising readiness does not require thousands
    // of unrelated durable operations. The combined ordinary and batch capacity matters.
    record.scene_receipts = (0..1024).map(|n| (format!("{n:064x}"), "a".repeat(64))).collect();
    record.batch_receipts = (1024..2047).map(|n| (format!("{n:064x}"), BatchReceipt {
        result: "b".repeat(64), members: 1, slot: 0,
    })).collect();
    record.stamp();
    record.validate().unwrap();
    {
        let writer = store.writer().unwrap();
        let _progress = store.progress_write();
        store.ensure_scene_batch_format(&writer).unwrap();
        fsio::write_json(&last_path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    }
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let bytes = record_bytes(&store, &accounts);
    assert_eq!(store.get_battle_outcome_states(&accounts, GAME).unwrap(), before,
        "2,047 combined receipts leave room for this battle outcome");
    assert_eq!(record_bytes(&store, &accounts), bytes);

    record.batch_receipts.insert(format!("{:064x}", 2047), BatchReceipt {
        result: "b".repeat(64), members: 1, slot: 0,
    });
    record.validate().unwrap();
    assert_eq!(record.scene_receipts.len() + record.batch_receipts.len(), MAX_SCENE_RECEIPTS);
    fsio::write_json(&last_path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    let bytes = record_bytes(&store, &accounts);
    assert!(matches!(store.get_battle_outcome_states(&accounts, GAME), Err(PersistError::TransactionConflict(_))),
        "the last member's exhausted history refuses the whole roster");
    assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), before);
    assert_eq!(record_bytes(&store, &accounts), bytes);
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn every_ordinary_outcome_crash_boundary_recovers_the_whole_roster_and_once_only_receipt() {
    // Intent, each of three story/character replacements, and intent cleanup/sync.
    for crash_at in 0..=9 {
        let (dir, store, accounts) = rig();
        let before = store.get_scene_states(&accounts, GAME).unwrap();
        let next = outcome(&before);
        let result = store.commit_scene_batch_with_hook(&operation(), RESULT, &before, &next,
            |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) });
        assert!(result.is_err(), "fault at boundary {crash_at}");
        if crash_at == 0 {
            assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), before);
            assert!(!store.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts, GAME).unwrap());
        } else {
            assert!(matches!(store.get_scene_states(&accounts, GAME), Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.put_character_progress(before[2].pair.character.as_ref().unwrap()),
                Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.put_progression(&before[0].pair.story), Err(PersistError::TransactionRecoveryRequired)));
        }
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        let actual = reopened.get_scene_states(&accounts, GAME).unwrap();
        assert_eq!(comparable(actual), comparable(if crash_at == 0 { before.clone() } else { next.clone() }));
        let saved = committed(&reopened, &before, &next);
        assert_eq!(saved.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
        assert_eq!(comparable(saved.current.clone()), comparable(next));
        for state in &saved.current {
            assert_eq!(reopened.get_scene_group(state.pair.story.account_id, GAME).unwrap(), vec![state.clone()]);
        }
        assert!(reopened.battle_outcome_applied(BATTLE_INSTANCE, RESULT, &accounts, GAME).unwrap());
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

#[test]
fn ordinary_outcome_recovery_checks_the_final_member_before_installing_any_earlier_member() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    store.commit_scene_batch_with_hook(&operation(), RESULT, &before, &outcome(&before),
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    let first_paths = [store.progression_path(accounts[0].0, GAME).unwrap(), store.character_path(accounts[0].0, GAME).unwrap()];
    let first_bytes: Vec<_> = first_paths.iter().map(|path| std::fs::read(path).unwrap()).collect();
    let last_path = store.character_path(accounts[2].0, GAME).unwrap();
    drop(store);
    let mut edited = CharacterRecord::read(&last_path).unwrap().unwrap();
    edited.progress.gil += 1;
    fsio::write_json(&last_path, &edited, MAX_CHARACTER_RECORD_BYTES).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
    for (path, bytes) in first_paths.iter().zip(first_bytes) { assert_eq!(std::fs::read(path).unwrap(), bytes); }
    assert!(dir.path().join(INTENT_FILE).exists());
}
