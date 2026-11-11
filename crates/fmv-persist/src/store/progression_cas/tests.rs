use super::*;
use crate::{CharacterProgress, Progression, SceneCheckpoint, SceneOperation, SceneOperationKind, ScenePhase, SceneReservation};
use fmv_core::InventoryEntry;

const GAME: GameId = GameId::Ff7;

fn fixture(character: bool) -> (tempfile::TempDir, Store, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Exact story", 1).unwrap();
    if character {
        let record = CharacterProgress { exp: 111, ap: 22, gil: 240, limit: 137,
            inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(20, 7)]),
            ..CharacterProgress::new(account, GAME, 0, 6, 1) };
        store.put_character_progress(&record).unwrap();
    }
    (dir, store, account)
}

/// Largest canonical fully-known FF7 bank blob, encoded here without introducing a
/// game adapter dependency into persistence. The store treats every byte as opaque.
fn full_banks(value: u8) -> Vec<u8> {
    let mut bytes = vec![3, 39, 0, 254, 4];
    for bank in [1u8, 3, 7, 11, 13] {
        for address in 0..=255u8 {
            if bank != 1 || address > 1 { bytes.extend([bank, address, value, 255]); }
        }
    }
    assert_eq!(bytes.len(), 5117);
    bytes
}

#[test]
fn exact_full_known_story_replacement_preserves_character_bytes_and_compatibility_fence() {
    let (dir, store, account) = fixture(true);
    let before = full_banks(255);
    let next = full_banks(0);
    store.put_progression(&Progression::new(account, GAME, before.clone(), 2)).unwrap();
    let character_path = store.character_path(account.0, GAME).unwrap();
    let character_bytes = std::fs::read(&character_path).unwrap();
    let old_character = store.get_character_progress(account, GAME).unwrap();
    let (commit, pair) = store.compare_exchange_progression(account, GAME, &before, &next, 3).unwrap();
    assert_eq!(commit, SceneCommit::Applied);
    assert_eq!(pair.story.blob, next);
    assert_eq!(pair.story.updated_at, 3);
    assert_eq!(pair.character, old_character);
    assert_eq!(std::fs::read(&character_path).unwrap(), character_bytes, "no schema/receipt/timestamp/resource rewrite");
    assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_progress_pair(account, GAME).unwrap(), pair);
    assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    assert_eq!(std::fs::read(character_path).unwrap(), character_bytes);
}

#[test]
fn an_already_present_extended_story_still_establishes_the_compatibility_fence() {
    let (dir, store, account) = fixture(true);
    let story = full_banks(0);
    store.put_progression(&Progression::new(account, GAME, story.clone(), 2)).unwrap();
    assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
    let paths = [store.progression_path(account.0, GAME).unwrap(), store.character_path(account.0, GAME).unwrap()];
    let bytes: Vec<_> = paths.iter().map(|path| std::fs::read(path).unwrap()).collect();
    let (commit, _) = store.compare_exchange_progression(account, GAME, b"old delivery", &story, 3).unwrap();
    assert_eq!(commit, SceneCommit::AlreadyApplied);
    assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    for (path, expected) in paths.iter().zip(bytes) { assert_eq!(std::fs::read(path).unwrap(), expected); }
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
    assert_eq!(reopened.get_progression(account, GAME).unwrap().blob, story);
}

#[test]
fn repeated_aba_changes_need_no_receipts_and_already_present_result_preserves_later_resources() {
    let (_dir, store, account) = fixture(true);
    let mut current = Vec::new();
    for turn in 0..20 {
        let next = vec![if turn % 2 == 0 { 1 } else { 2 }, 0, 255];
        assert_eq!(store.compare_exchange_progression(account, GAME, &current, &next, turn + 2).unwrap().0, SceneCommit::Applied);
        current = next;
    }
    let mut character = store.get_character_progress(account, GAME).unwrap().unwrap();
    character.gil += 100;
    store.put_character_progress(&character).unwrap();
    let before = store.get_progress_pair(account, GAME).unwrap();
    let story_path = store.progression_path(account.0, GAME).unwrap();
    let character_path = store.character_path(account.0, GAME).unwrap();
    let bytes = (std::fs::read(&story_path).unwrap(), std::fs::read(&character_path).unwrap());
    let (commit, repeated) = store.compare_exchange_progression(account, GAME, b"old delivery", &current, 999).unwrap();
    assert_eq!(commit, SceneCommit::AlreadyApplied);
    assert_eq!(repeated, before);
    assert_eq!((std::fs::read(story_path).unwrap(), std::fs::read(character_path).unwrap()), bytes);
    assert_eq!(repeated.character.unwrap().schema_version, 1, "ordinary changes never allocate durable receipt history");
}

#[test]
fn a_story_only_account_does_not_acquire_fabricated_character_defaults() {
    let (dir, store, account) = fixture(false);
    let next = full_banks(0);
    let (commit, pair) = store.compare_exchange_progression(account, GAME, &[], &next, 2).unwrap();
    assert_eq!(commit, SceneCommit::Applied);
    assert!(pair.character.is_none());
    assert!(!store.character_path(account.0, GAME).unwrap().exists());
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_progress_pair(account, GAME).unwrap(), pair);
    assert!(reopened.get_character_progress(account, GAME).unwrap().is_none());
}

#[test]
fn stale_oversized_unknown_and_corrupt_inputs_do_not_write_or_raise_the_format() {
    let (_dir, store, account) = fixture(true);
    store.put_progression(&Progression::new(account, GAME, vec![1, 0, 255], 2)).unwrap();
    let path = store.progression_path(account.0, GAME).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(matches!(store.compare_exchange_progression(account, GAME, &[0], &[2], 3), Err(PersistError::TransactionConflict(_))));
    for (before, next) in [(vec![0; MAX_BLOB_BYTES + 1], vec![2]), (vec![1, 0, 255], vec![0; MAX_BLOB_BYTES + 1])] {
        assert!(matches!(store.compare_exchange_progression(account, GAME, &before, &next, 3), Err(PersistError::BlobTooLarge { .. })));
    }
    assert!(matches!(store.compare_exchange_progression(AccountId(999), GAME, &[], &[2], 3), Err(PersistError::UnknownAccount(_))));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
    std::fs::write(&path, b"not JSON").unwrap();
    assert!(matches!(store.compare_exchange_progression(account, GAME, &[], &[2], 3), Err(PersistError::Corrupt { .. })));
    assert_eq!(std::fs::read(path).unwrap(), b"not JSON");
    assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
}

#[test]
fn exact_story_cas_refuses_scene_owners_and_reserved_guests_even_for_an_already_present_result() {
    let (_dir, store, owner) = fixture(true);
    let (guest, _) = store.create_account("Reserved guest", 1).unwrap();
    store.put_character_progress(&CharacterProgress::new(guest, GAME, 1, 5, 1)).unwrap();
    let before = store.get_scene_states(&[owner, guest], GAME).unwrap();
    let mut next = before.clone();
    let instance = [42; 32];
    for (slot, state) in next.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance, ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    next[0].checkpoint = Some(SceneCheckpoint { instance, scene_key: "fixture/scene".into(), source_hash: [3; 32], replay_version: 1,
        original_story: Vec::new(), original_gil: 240, seed: 1, replay: vec![1],
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 1, battle_seed: 9 }, consumed_battles: 0 });
    let operation = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::PrepareBattle };
    let saved = store.commit_scene_batch(&operation, b"prepare", &before, &next).unwrap();
    for account in [owner, guest] {
        for desired in [Vec::new(), vec![7]] {
            assert!(matches!(store.compare_exchange_progression(account, GAME, &[], &desired, 3), Err(PersistError::TransactionConflict(_))));
        }
    }
    assert_eq!(store.get_scene_states(&[owner, guest], GAME).unwrap(), saved.current);
}

#[test]
fn actual_atomic_write_interruptions_fence_the_store_and_reopen_as_whole_old_or_new_story() {
    for stage in 0..=3 {
        let (dir, store, account) = fixture(true);
        let before = full_banks(255);
        let next = full_banks(0);
        store.put_progression(&Progression::new(account, GAME, before.clone(), 2)).unwrap();
        let character_path = store.character_path(account.0, GAME).unwrap();
        let character_bytes = std::fs::read(&character_path).unwrap();
        let result = store.compare_exchange_progression_with_writer(account, GAME, &before, &next, 3, |path, bytes| {
            fsio::atomic_write_with_checkpoints(path, bytes, |at| {
                if at == stage { Err(std::io::Error::other("injected exact story interruption")) } else { Ok(()) }
            })
        });
        assert!(result.is_err(), "fault stage {stage}");
        assert!(matches!(store.get_progress_pair(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.put_progression(&Progression::new(account, GAME, vec![9], 4)), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.create_account("Blocked writer", 4), Err(PersistError::TransactionRecoveryRequired)));
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        let current = reopened.get_progress_pair(account, GAME).unwrap();
        assert_eq!(current.story.blob, if stage < 2 { before.clone() } else { next.clone() });
        assert_eq!(std::fs::read(&character_path).unwrap(), character_bytes);
        let (commit, result) = reopened.compare_exchange_progression(account, GAME, &before, &next, 5).unwrap();
        assert_eq!(commit, if stage < 2 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
        assert_eq!(result.story.blob, next);
        assert_eq!(std::fs::read(character_path).unwrap(), character_bytes);
        assert!(!dir.path().join(super::super::scene::INTENT_FILE).exists(), "one-file CAS needs no scene intent");
    }
}
