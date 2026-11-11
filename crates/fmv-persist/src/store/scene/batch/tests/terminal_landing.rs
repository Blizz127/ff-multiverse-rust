//! Retiring a terminal landing changes only its checkpoint and once-only receipt.

use super::*;

const RESULT: &[u8] = b"terminal landing independently validated";

fn complete() -> SceneOperation { SceneOperation { ordinal: 1, ..op(SceneOperationKind::Complete) } }

fn finished(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> {
    let initial = store.get_scene_states(accounts, GAME).unwrap();
    let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
    let resumed = store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"victory", &prepared, &resolve(&prepared)).unwrap().current;
    store.commit_scene_batch(&SceneOperation { ordinal: 1, ..op(SceneOperationKind::Finish) }, b"landing", &resumed, &landing(&resumed)).unwrap().current
}

fn cleared(before: &[SceneState]) -> Vec<SceneState> {
    let mut next = before.to_vec();
    next[0].checkpoint = None;
    next
}

fn paths(store: &Store, accounts: &[AccountId]) -> Vec<std::path::PathBuf> {
    accounts.iter().flat_map(|account| [store.progression_path(account.0, GAME).unwrap(),
        store.character_path(account.0, GAME).unwrap()]).collect()
}

fn bytes(paths: &[std::path::PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().map(|path| std::fs::read(path).unwrap()).collect()
}

#[test]
fn terminal_landing_retires_once_without_rewriting_progress_or_released_guests() {
    let (dir, store, accounts) = rig();
    let landed = finished(&store, &accounts);
    let before = vec![landed[0].clone()];
    let next = cleared(&before);
    let story_path = store.progression_path(accounts[0].0, GAME).unwrap();
    let character_path = store.character_path(accounts[0].0, GAME).unwrap();
    let previous_character = CharacterRecord::read(&character_path).unwrap().unwrap();
    let story_bytes = std::fs::read(&story_path).unwrap();
    let guest_paths = paths(&store, &accounts[1..]);
    let guest_bytes = bytes(&guest_paths);
    let saved = store.commit_scene_batch(&complete(), RESULT, &before, &next).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, next, "all story/resource fields and timestamps stay exact");
    let saved_character = CharacterRecord::read(&character_path).unwrap().unwrap();
    assert_eq!(saved_character.scene_receipts, previous_character.scene_receipts);
    assert_eq!(saved_character.batch_receipts.len(), previous_character.batch_receipts.len() + 1);
    for (key, receipt) in previous_character.batch_receipts {
        assert_eq!(saved_character.batch_receipts.get(&key), Some(&receipt));
    }
    assert_eq!(std::fs::read(&story_path).unwrap(), story_bytes);
    assert_eq!(bytes(&guest_paths), guest_bytes);
    assert_eq!(store.get_scene_group(accounts[0], GAME).unwrap(), next);
    assert_eq!(store.get_scene_states(&accounts[1..], GAME).unwrap(), landed[1..]);
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    let all_paths = paths(&reopened, &accounts);
    let committed_bytes = bytes(&all_paths);
    let duplicate = reopened.commit_scene_batch(&complete(), RESULT, &before, &next).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, next);
    assert!(reopened.scene_batch_applied(&complete(), RESULT, &accounts[..1], GAME).unwrap());
    assert_eq!(bytes(&all_paths), committed_bytes, "a durable duplicate does not rewrite any record");
    assert!(matches!(reopened.commit_scene_batch(&complete(), b"another result", &before, &next),
        Err(PersistError::TransactionConflict(_))));
    assert_eq!(bytes(&all_paths), committed_bytes);
    // A delayed duplicate after subsequent ordinary rewards reports their current pair;
    // it must not reinstall the old landing's resource snapshot.
    let mut earned = reopened.get_character_progress(accounts[0], GAME).unwrap().unwrap();
    earned.gil += 17;
    earned.updated_at += 1;
    reopened.put_character_progress(&earned).unwrap();
    let current = reopened.get_scene_states(&accounts[..1], GAME).unwrap();
    let earned_bytes = bytes(&all_paths);
    let duplicate = reopened.commit_scene_batch(&complete(), RESULT, &before, &next).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, current);
    assert_eq!(duplicate.current[0].pair.character.as_ref().unwrap().gil, earned.gil);
    assert_eq!(bytes(&all_paths), earned_bytes);
}

#[test]
fn landing_retirement_refuses_every_changed_progress_field_including_timestamps() {
    let (dir, store, accounts) = rig();
    let landed = finished(&store, &accounts);
    let before = vec![landed[0].clone()];
    let record_paths = paths(&store, &accounts);
    let original = bytes(&record_paths);
    type Mutation = (&'static str, fn(&mut ProgressPair));
    let changes: &[Mutation] = &[
        ("story bytes", |pair| pair.story.blob.push(48)),
        ("story timestamp", |pair| pair.story.updated_at += 1),
        ("story schema", |pair| pair.story.schema_version += 1),
        ("character id", |pair| pair.character.as_mut().unwrap().character += 1),
        ("character level", |pair| pair.character.as_mut().unwrap().level += 1),
        ("experience", |pair| pair.character.as_mut().unwrap().exp += 1),
        ("ability points", |pair| pair.character.as_mut().unwrap().ap += 1),
        ("gil", |pair| pair.character.as_mut().unwrap().gil += 1),
        ("limit gauge", |pair| pair.character.as_mut().unwrap().limit += 1),
        ("inventory", |pair| pair.character.as_mut().unwrap().inventory = Some(vec![])),
        ("never-stored inventory", |pair| pair.character.as_mut().unwrap().inventory = None),
        ("character timestamp", |pair| pair.character.as_mut().unwrap().updated_at += 1),
        ("character schema", |pair| pair.character.as_mut().unwrap().schema_version -= 1),
        ("missing character", |pair| pair.character = None),
    ];
    for (name, change) in changes {
        let mut next = cleared(&before);
        change(&mut next[0].pair);
        assert!(store.commit_scene_batch(&complete(), RESULT, &before, &next).is_err(), "{name}");
        assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), landed, "{name}");
        assert_eq!(bytes(&record_paths), original, "{name}");
        assert!(!dir.path().join(INTENT_FILE).exists(), "{name} rejected before creating an intent");
    }
}

#[test]
fn landing_retirement_requires_exact_boundary_and_removes_instead_of_replacing_checkpoint() {
    let (dir, store, accounts) = rig();
    let landed = finished(&store, &accounts);
    let before = vec![landed[0].clone()];
    let next = cleared(&before);
    for operation in [SceneOperation { instance: [24; 32], ..complete() },
        SceneOperation { ordinal: 0, ..complete() }, SceneOperation { ordinal: 2, ..complete() },
        SceneOperation { attempt: 1, ..complete() }] {
        assert!(store.commit_scene_batch(&operation, RESULT, &before, &next).is_err());
    }
    assert!(store.commit_scene_batch(&complete(), RESULT, &before, &before).is_err(), "keeping the checkpoint is not retirement");
    let mut replaced = before.clone();
    replaced[0].checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 1 };
    assert!(store.commit_scene_batch(&complete(), RESULT, &before, &replaced).is_err(), "same-instance replacement is not retirement");
    replaced[0].checkpoint.as_mut().unwrap().instance = [24; 32];
    assert!(store.commit_scene_batch(&complete(), RESULT, &before, &replaced).is_err(), "a fresh checkpoint needs the linked-scene operation");
    let mut reserved_after = next.clone();
    reserved_after[0].reservation = Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: 0 });
    assert!(store.commit_scene_batch(&complete(), RESULT, &before, &reserved_after).is_err());
    assert!(store.commit_scene_batch(&complete(), RESULT, &landed, &cleared(&landed)).is_err(),
        "already released guests cannot be included in singleton retirement");
    assert_eq!(store.get_scene_states(&accounts, GAME).unwrap(), landed);
    assert!(!dir.path().join(INTENT_FILE).exists());
}

#[test]
fn terminal_retirement_cannot_drop_an_unresolved_or_retained_battle_group() {
    for resolved in [false, true] {
        let (dir, store, accounts) = rig();
        let initial = store.get_scene_states(&accounts, GAME).unwrap();
        let prepared = store.commit_scene_batch(&op(SceneOperationKind::PrepareBattle), b"prepare", &initial, &prepare(&initial)).unwrap().current;
        let before = if resolved {
            store.commit_scene_batch(&op(SceneOperationKind::ResolveBattle), b"victory", &prepared, &resolve(&prepared)).unwrap().current
        } else { prepared };
        let operation = SceneOperation { ordinal: u32::from(resolved), ..complete() };
        let record_paths = paths(&store, &accounts);
        let original = bytes(&record_paths);
        for cohort in [before.clone(), vec![before[0].clone()]] {
            let mut next = cleared(&cohort);
            for state in &mut next { state.reservation = None; }
            assert!(store.commit_scene_batch(&operation, RESULT, &cohort, &next).is_err());
        }
        // Removing the visible reservation from a forged predecessor cannot hide the
        // authoritative private roster that still holds the other participants.
        let mut forged = vec![before[0].clone()];
        forged[0].reservation = None;
        forged[0].checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: b"fixture/cargo".to_vec() };
        assert!(store.commit_scene_batch(&operation, RESULT, &forged, &cleared(&forged)).is_err());
        assert_eq!(store.get_scene_group(accounts[2], GAME).unwrap(), before);
        assert_eq!(bytes(&record_paths), original);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

#[test]
fn every_terminal_retirement_failure_recovers_the_checkpoint_and_receipt_once() {
    for crash_at in 0..=5 {
        let (dir, store, accounts) = rig();
        let landed = finished(&store, &accounts);
        let before = vec![landed[0].clone()];
        let next = cleared(&before);
        let guest_paths = paths(&store, &accounts[1..]);
        let guest_bytes = bytes(&guest_paths);
        assert!(store.commit_scene_batch_with_hook(&complete(), RESULT, &before, &next,
            |stage| if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }).is_err());
        if crash_at == 0 {
            assert_eq!(store.get_scene_states(&accounts[..1], GAME).unwrap(), before);
        } else {
            assert!(matches!(store.get_scene_state(accounts[0], GAME), Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.put_character_progress(before[0].pair.character.as_ref().unwrap()),
                Err(PersistError::TransactionRecoveryRequired)));
        }
        assert_eq!(bytes(&guest_paths), guest_bytes);
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_states(&accounts[..1], GAME).unwrap(), if crash_at == 0 { before.clone() } else { next.clone() });
        let retry = reopened.commit_scene_batch(&complete(), RESULT, &before, &next).unwrap();
        assert_eq!(retry.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
        assert_eq!(retry.current, next);
        assert_eq!(retry.current[0].pair, before[0].pair, "failure boundary {crash_at} preserves the complete pair");
        assert_eq!(bytes(&guest_paths), guest_bytes);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}

#[test]
fn a_checksummed_retirement_intent_cannot_change_story_resources_or_timestamps_during_recovery() {
    for mutation in 0..4 {
        let (dir, store, accounts) = rig();
        let landed = finished(&store, &accounts);
        let before = vec![landed[0].clone()];
        let next = cleared(&before);
        let record_paths = paths(&store, &accounts);
        let original_records = bytes(&record_paths);
        store.commit_scene_batch_with_hook(&complete(), RESULT, &before, &next,
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        let path = dir.path().join(INTENT_FILE);
        let original_intent = std::fs::read(&path).unwrap();
        drop(store);
        let mut file: BatchIntentFile = serde_json::from_slice(&original_intent).unwrap();
        let after = &mut file.intent.rows[0].after;
        match mutation {
            0 => after.story.as_mut().unwrap().blob.push(48),
            1 => after.character.as_mut().unwrap().progress.gil += 1,
            2 => after.story.as_mut().unwrap().updated_at += 1,
            _ => after.character.as_mut().unwrap().progress.updated_at += 1,
        }
        file.checksum = digest(&file.intent).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
        assert_eq!(bytes(&record_paths), original_records, "mutation {mutation} is rejected before installing any record");
        assert!(path.exists());
        std::fs::write(&path, original_intent).unwrap();
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_scene_states(&accounts[..1], GAME).unwrap(), next);
    }
}

fn set_header_version(root: &Path, version: u32) {
    // A synthetic old-store fixture only; the production compactor never downgrades.
    let path = root.join(crate::index::INDEX_LOG_FILE);
    let original = std::fs::read(&path).unwrap();
    let end = original.iter().position(|byte| *byte == b'\n').unwrap();
    let space = original[..end].iter().position(|byte| *byte == b' ').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(&original[space + 1..end]).unwrap();
    header["schema_version"] = version.into();
    let payload = serde_json::to_vec(&header).unwrap();
    let digest = blake3::hash(&payload).to_hex().to_string();
    let mut replacement = format!("{} ", &digest[..space]).into_bytes();
    replacement.extend_from_slice(&payload);
    replacement.extend_from_slice(&original[end..]);
    std::fs::write(path, replacement).unwrap();
}

#[test]
fn landing_completion_recovery_requires_index_four_while_existing_resume_completion_accepts_three() {
    for retires_landing in [true, false] {
        let (dir, store, accounts) = rig();
        let landed = finished(&store, &accounts);
        let owner = vec![landed[0].clone()];
        let (operation, before, next) = if retires_landing {
            (complete(), owner.clone(), cleared(&owner))
        } else {
            let resumed = store.commit_scene_batch(&SceneOperation { ordinal: 1, ..op(SceneOperationKind::ContinueLanding) },
                b"linked scene", &owner, &continuation(&owner)).unwrap().current;
            let mut next = cleared(&resumed);
            next[0].pair.story.blob.push(48);
            (SceneOperation { instance: [24; 32], ..op(SceneOperationKind::Complete) }, resumed, next)
        };
        let record_paths = paths(&store, &accounts);
        let original_records = bytes(&record_paths);
        store.commit_scene_batch_with_hook(&operation, RESULT, &before, &next,
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        set_header_version(dir.path(), crate::index::SCENE_BATCH_INDEX_VERSION);
        if retires_landing {
            assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
            assert_eq!(bytes(&record_paths), original_records, "old-header refusal precedes installation");
            assert!(dir.path().join(INTENT_FILE).exists());
            set_header_version(dir.path(), crate::index::DURABLE_STORY_INDEX_VERSION);
        }
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), if retires_landing {
            crate::index::DURABLE_STORY_INDEX_VERSION
        } else { crate::index::SCENE_BATCH_INDEX_VERSION });
        assert_eq!(reopened.get_scene_states(&accounts[..1], GAME).unwrap(), next);
        assert_eq!(reopened.get_scene_states(&accounts[1..], GAME).unwrap(), landed[1..]);
        assert!(!dir.path().join(INTENT_FILE).exists());
    }
}
