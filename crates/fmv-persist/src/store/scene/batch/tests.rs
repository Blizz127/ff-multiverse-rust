use super::*;
use crate::{CharacterProgress, Progression};
use fmv_core::InventoryEntry;

const GAME: GameId = GameId::Ff7;
const INSTANCE: [u8; 32] = [23; 32];

fn op(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: 0, attempt: 0, kind }
}

fn rig() -> (tempfile::TempDir, Store, Vec<AccountId>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut accounts = Vec::new();
    for n in 0..3 {
        let (account, _) = store.create_account(&format!("Batch {n}"), 1).unwrap();
        let mut progress = CharacterProgress::new(account, GAME, n, 6, 1);
        progress.gil = 240;
        progress.inventory = Some(vec![InventoryEntry::new(0, 3)]);
        store.put_character_progress(&progress).unwrap();
        store.put_progression(&Progression::new(account, GAME, vec![1, n], 1)).unwrap();
        accounts.push(account);
    }
    (dir, store, accounts)
}

fn prepare(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    for (slot, state) in next.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    next[0].checkpoint = Some(SceneCheckpoint { instance: INSTANCE, scene_key: "fixture/bridge".into(), source_hash: [9; 32],
        replay_version: 1, original_story: before[0].pair.story.blob.clone(), original_gil: 240, seed: 5, replay: vec![1],
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 303, battle_seed: u64::MAX }, consumed_battles: 0 });
    next[0].pair.character.as_mut().unwrap().gil -= 1;
    next[0].pair.story.blob.push(7);
    next
}

fn resolve(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    for state in &mut next {
        let c = state.pair.character.as_mut().unwrap();
        c.gil += 20;
        c.exp += 50;
        c.ap += 3;
        c.limit = 33;
        c.inventory = Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(8, 1)]);
    }
    let checkpoint = next[0].checkpoint.as_mut().unwrap();
    checkpoint.consumed_battles = 1;
    checkpoint.phase = ScenePhase::Resume { segment: 1 };
    checkpoint.replay.push(2);
    next
}

fn comparable(mut states: Vec<SceneState>) -> Vec<SceneState> {
    for state in &mut states { state.pair.character.as_mut().unwrap().schema_version = 1; }
    states
}

fn interrupted(stage: u32) -> PersistError {
    PersistError::InvalidField { field: "injected_checkpoint", reason: stage.to_string() }
}

#[test]
fn every_prepare_and_resolve_crash_boundary_recovers_the_whole_coop_batch_once() {
    for resolving in [false, true] {
        for crash_at in 0..=9 {
            let (dir, store, accounts) = rig();
            let initial = store.get_scene_states(&accounts, GAME).unwrap();
            let before = if resolving {
                store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current
            } else { initial };
            let next = if resolving { resolve(&before) } else { prepare(&before) };
            let operation = op(if resolving { SceneOperationKind::ResolveBattle } else { SceneOperationKind::PrepareBattle });
            let result = if resolving { b"victory".as_slice() } else { b"prepare".as_slice() };
            let error = store.commit_scene_batch_with_hook(&operation, result, &before, &next,
                |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) });
            assert!(error.is_err(), "resolution={resolving}, checkpoint={crash_at}");
            if crash_at == 0 {
                assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), before);
            } else {
                assert!(matches!(store.get_scene_states(&accounts, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.get_scene_group(accounts[2], GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_character_progress(before[2].pair.character.as_ref().unwrap()), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.snapshot(dir.path().join("blocked")), Err(PersistError::TransactionRecoveryRequired)));
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            let actual = reopened.get_scene_states(&accounts, GAME).unwrap();
            assert_eq!(comparable(actual), comparable(if crash_at == 0 { before.clone() } else { next.clone() }));
            let retry = reopened.commit_scene_batch(&operation, result, &before, &next).unwrap();
            assert_eq!(retry.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(comparable(retry.current), comparable(next));
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }
}

#[test]
fn a_conflict_in_the_final_member_prevents_recovery_writing_any_earlier_member() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    store.commit_scene_batch_with_hook(&op(SceneOperationKind::PrepareBattle), b"prepare", &before, &prepare(&before),
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    let first_path = store.character_path(accounts[0].0, GAME).unwrap();
    let first_story = store.progression_path(accounts[0].0, GAME).unwrap();
    let last_path = store.character_path(accounts[2].0, GAME).unwrap();
    let original = (std::fs::read(&first_path).unwrap(), std::fs::read(&first_story).unwrap());
    drop(store);
    let mut changed = CharacterRecord::read(&last_path).unwrap().unwrap();
    changed.progress.gil += 1;
    fsio::write_json(&last_path, &changed, MAX_CHARACTER_RECORD_BYTES).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
    assert_eq!(std::fs::read(&first_path).unwrap(), original.0);
    assert_eq!(std::fs::read(&first_story).unwrap(), original.1);
    assert!(dir.path().join(INTENT_FILE).exists());
}

#[test]
fn a_checksummed_intent_that_omits_a_prepared_member_is_refused_before_any_install() {
    let (dir, store, accounts) = rig();
    let initial = store.get_scene_states(&accounts, GAME).unwrap();
    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    store.commit_scene_batch_with_hook(&op(SceneOperationKind::ResolveBattle), b"victory", &prepared, &resolve(&prepared),
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    let paths: Vec<_> = accounts.iter().map(|a| store.character_path(a.0, GAME).unwrap()).collect();
    let original: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let path = dir.path().join(INTENT_FILE);
    drop(store);
    let mut file: BatchIntentFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file.intent.rows.pop();
    file.checksum = digest(&file.intent).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
    for (path, expected) in paths.iter().zip(original) { assert_eq!(std::fs::read(path).unwrap(), expected); }
}

#[test]
fn authored_retry_uses_another_attempt_and_late_first_result_never_pays_twice() {
    let (_dir, store, accounts) = rig();
    let initial = store.get_scene_states(&accounts, GAME).unwrap();
    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    let mut retry = prepared.clone();
    for state in &mut retry {
        state.reservation.as_mut().unwrap().attempt = 1;
        state.pair.character.as_mut().unwrap().inventory = Some(vec![InventoryEntry::new(0, 2)]);
    }
    let checkpoint = retry[0].checkpoint.as_mut().unwrap();
    checkpoint.phase = ScenePhase::BeforeBattle { ordinal: 0, attempt: 1, formation: 303, battle_seed: 3 };
    checkpoint.replay.push(0);
    let retried = store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"retry after loss", &prepared, &retry).unwrap().current;
    let mut won = resolve(&retried);
    won[0].checkpoint.as_mut().unwrap().replay.push(1);
    let victory = SceneOperation { attempt: 1, ..op(SceneOperationKind::ResolveBattle) };
    let saved = store.commit_scene_batch(&victory, b"second attempt won", &retried, &won).unwrap();
    let late = store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"retry after loss", &prepared, &retry).unwrap();
    assert_eq!(late.commit, SceneCommit::AlreadyApplied);
    assert_eq!(late.current, saved.current);
    assert_eq!(late.current[0].pair.character.as_ref().unwrap().gil, 259);
    assert_eq!(late.current[0].checkpoint.as_ref().unwrap().consumed_battles, 1);
}

#[test]
fn ordinary_scene_receipts_cannot_bypass_an_active_checkpoint_and_remain_compatible_after_guest_release() {
    let (_dir, store, accounts) = rig();
    let before = store.get_progress_pair(accounts[1], GAME).unwrap();
    let mut next = before.clone();
    next.character.as_mut().unwrap().gil -= 1;
    store.commit_scene_progress_with_result("old scene", b"one debit", &before, &next).unwrap();
    let initial = store.get_scene_states(&accounts, GAME).unwrap();
    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    assert!(matches!(store.scene_progress_applied("old scene", &before.story, b"one debit"), Err(PersistError::TransactionConflict(_))));
    let resolved = store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"victory", &prepared, &resolve(&prepared)).unwrap().current;
    assert!(matches!(store.scene_progress_applied("old scene", &before.story, b"one debit"), Err(PersistError::TransactionConflict(_))));
    let mut landed = resolved.clone();
    for state in &mut landed { state.reservation = None; }
    landed[0].checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: b"fixture/landing".to_vec() };
    store.commit_scene_batch(&SceneOperation { ordinal: 1, ..op(SceneOperationKind::Finish) }, b"landing", &resolved, &landed).unwrap();
    assert!(store.scene_progress_applied("old scene", &before.story, b"one debit").unwrap());
    let before = store.get_progress_pair(accounts[1], GAME).unwrap();
    let mut next = before.clone();
    next.character.as_mut().unwrap().gil -= 2;
    store.commit_scene_progress_with_result("new old scene", b"two debit", &before, &next).unwrap();
    assert!(store.scene_batch_applied(&op(SceneOperationKind::ResolveBattle), b"victory", &accounts, GAME).unwrap());
}

fn landing(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    for state in &mut next { state.reservation = None; }
    next[0].checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: b"fixture/cargo".to_vec() };
    next
}

fn continuation(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    let checkpoint = next[0].checkpoint.as_mut().unwrap();
    checkpoint.instance = [24; 32];
    checkpoint.scene_key = "fixture/cargo".into();
    checkpoint.source_hash = [10; 32];
    checkpoint.original_story = before[0].pair.story.blob.clone();
    checkpoint.original_gil = before[0].pair.character.as_ref().unwrap().gil;
    checkpoint.seed = 6;
    checkpoint.phase = ScenePhase::Resume { segment: 0 };
    checkpoint.consumed_battles = 0;
    checkpoint.replay.clear();
    next
}

#[test]
fn every_finish_continue_and_complete_crash_boundary_recovers_its_receipt_and_landing() {
    for kind in [SceneOperationKind::Finish, SceneOperationKind::ContinueLanding, SceneOperationKind::Complete] {
        let last_checkpoint = if kind == SceneOperationKind::Finish { 9 } else { 5 };
        for crash_at in 0..=last_checkpoint {
            let (dir, store, accounts) = rig();
            let initial = store.get_scene_states(&accounts, GAME).unwrap();
            let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
            let resolved = store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"victory", &prepared, &resolve(&prepared)).unwrap().current;
            let finish = SceneOperation { ordinal: 1, ..op(SceneOperationKind::Finish) };
            let (operation, before, next) = match kind {
                SceneOperationKind::Finish => (finish, resolved.clone(), landing(&resolved)),
                SceneOperationKind::ContinueLanding | SceneOperationKind::Complete => {
                    let landed = store.commit_scene_batch(&finish, b"landing", &resolved, &landing(&resolved)).unwrap().current;
                    let owner = vec![landed[0].clone()];
                    let continuing = SceneOperation { ordinal: 1, ..op(SceneOperationKind::ContinueLanding) };
                    if kind == SceneOperationKind::ContinueLanding {
                        (continuing, owner.clone(), continuation(&owner))
                    } else {
                        let resumed = store.commit_scene_batch(&continuing, b"new scene", &owner, &continuation(&owner)).unwrap().current;
                        let mut done = resumed.clone();
                        done[0].checkpoint = None;
                        done[0].pair.story.blob.push(48);
                        (SceneOperation { instance: [24; 32], ..op(SceneOperationKind::Complete) }, resumed, done)
                    }
                }
                _ => unreachable!(),
            };
            let result = b"the interrupted operation";
            store.commit_scene_batch_with_hook(&operation, result, &before, &next,
                |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            let accounts: Vec<_> = before.iter().map(|s| s.pair.story.account_id).collect();
            assert_eq!(comparable(reopened.get_scene_states(&accounts, GAME).unwrap()),
                comparable(if crash_at == 0 { before.clone() } else { next.clone() }), "{kind:?} at {crash_at}");
            let applied = reopened.commit_scene_batch(&operation, result, &before, &next).unwrap();
            assert_eq!(applied.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(comparable(applied.current), comparable(next));
        }
    }
}

#[test]
fn batch_recovery_requires_the_prior_global_index_fence() {
    let (dir, store, accounts) = rig();
    let before = store.get_scene_states(&accounts, GAME).unwrap();
    store.commit_scene_batch_with_hook(&op(SceneOperationKind::PrepareBattle), b"prepare", &before, &prepare(&before),
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    drop(store);
    let path = dir.path().join(crate::index::INDEX_LOG_FILE);
    let bytes = std::fs::read(&path).unwrap();
    let end = bytes.iter().position(|b| *b == b'\n').unwrap();
    // Preserve the exact recognized kind from the actual header while reducing its version.
    let first = &bytes[..end];
    let space = first.iter().position(|b| *b == b' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(&first[space + 1..]).unwrap();
    header["schema_version"] = 2.into();
    let payload = serde_json::to_vec(&header).unwrap();
    let sum = blake3::hash(&payload).to_hex().to_string();
    let mut lowered = format!("{} ", &sum[..space]).into_bytes();
    lowered.extend_from_slice(&payload);
    lowered.extend_from_slice(&bytes[end..]);
    std::fs::write(&path, lowered).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
    assert!(dir.path().join(INTENT_FILE).exists());
}
