//! Ordinary item/reward writes share the atomic replacement path and recovery fence used
//! by exact story CAS. A lost acknowledgment cannot turn an internal retry into two deltas.

use super::*;
use fmv_core::InventoryEntry;
use std::cell::Cell;

fn fixture() -> (tempfile::TempDir, Store, AccountId, AccountId) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let (account, _) = store.create_account("Item owner", 1).unwrap();
    let (other, _) = store.create_account("Unrelated account", 1).unwrap();
    let record = CharacterProgress { gil: 240, exp: 111, ap: 7, limit: 137,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(20, 7)]),
        ..CharacterProgress::new(account, GameId::Ff7, 0, 6, 1) };
    store.put_character_progress(&record).unwrap();
    store.put_progression(&Progression::new(account, GameId::Ff7, vec![1, 39, 0], 1)).unwrap();
    // Include an existing durable receipt: ordinary replacements must preserve its private
    // envelope in both the old and newly installed record.
    let before = store.get_progress_pair(account, GameId::Ff7).unwrap();
    let mut next = before.clone();
    next.story.blob = vec![1, 40, 0];
    store.commit_scene_progress("fixture/earlier-scene", &before, &next).unwrap();
    (directory, store, account, other)
}

fn use_item(mut record: CharacterProgress) -> CharacterProgress {
    record.inventory.as_mut().unwrap()[0].count -= 1;
    record.updated_at += 1;
    record
}

#[test]
fn uncertain_ordinary_item_writes_fence_retries_and_reopen_with_at_most_one_debit() {
    for stage in 0..=3 {
        let (directory, store, account, other) = fixture();
        let path = store.character_path(account.0, GameId::Ff7).unwrap();
        let before = CharacterRecord::read(&path).unwrap().unwrap();
        let after = use_item(before.progress.clone());
        let story = store.get_progression(account, GameId::Ff7).unwrap();
        let result = {
            // The same locks held by ensure/put/update_character_progress.
            let _writer = store.writer().unwrap();
            let _progress = store.progress_write();
            store.write_character_record_with_writer(&path, after.clone(), |path, bytes| {
                fsio::atomic_write_with_checkpoints(path, bytes, |at| {
                    if at == stage { Err(std::io::Error::other("injected item save interruption")) } else { Ok(()) }
                })
            })
        };
        assert!(matches!(result, Err(PersistError::Io { .. })), "atomic stage {stage}: {result:?}");
        let retry_ran = Cell::new(false);
        let retry = store.update_character_progress(account, GameId::Ff7, |current| {
            retry_ran.set(true);
            use_item(current.unwrap())
        });
        assert!(matches!(retry, Err(PersistError::TransactionRecoveryRequired)));
        assert!(!retry_ran.get(), "the same accepted item command cannot be charged again");
        assert!(matches!(store.get_progress_pair(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_progression(other, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.put_progression(&Progression::new(other, GameId::Ff7, vec![2], 2)), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.put_character_progress(&after), Err(PersistError::TransactionRecoveryRequired)));
        drop(store);

        let reopened = Store::open(directory.path()).unwrap();
        let actual = CharacterRecord::read(&path).unwrap().unwrap();
        let expected = if stage < 2 { before.progress.clone() } else { after };
        assert_eq!(actual.progress, expected);
        assert_eq!(actual.scene_receipts, before.scene_receipts);
        assert_eq!(actual.batch_receipts, before.batch_receipts);
        assert_eq!(actual.progress.inventory.as_ref().unwrap()[0].count, if stage < 2 { 3 } else { 2 });
        assert_ne!(actual.progress.inventory.as_ref().unwrap()[0].count, 1, "never a duplicate charge");
        assert_eq!(reopened.get_progression(account, GameId::Ff7).unwrap(), story);
        assert!(!directory.path().join(INTENT_FILE).exists(), "an ordinary one-record write has no scene intent");
    }
}

#[test]
fn invalid_ordinary_character_shape_refuses_before_the_fence_or_writer() {
    let (_directory, store, account, _) = fixture();
    let path = store.character_path(account.0, GameId::Ff7).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut invalid = store.get_character_progress(account, GameId::Ff7).unwrap().unwrap();
    invalid.level = 0;
    let wrote = Cell::new(false);
    let result = {
        let _writer = store.writer().unwrap();
        let _progress = store.progress_write();
        store.write_character_record_with_writer(&path, invalid, |_, _| {
            wrote.set(true);
            panic!("invalid records must be refused before any write")
        })
    };
    assert!(result.is_err());
    assert!(!wrote.get());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(store.get_progress_pair(account, GameId::Ff7).is_ok(), "a validation refusal must not poison the store");
    assert!(store.create_account("Still writable", 3).is_ok());
}
