//! Synthetic co-op checkpoints through the ordered saver and the real durable store.

mod visit;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmv_core::InventoryEntry;
use fmv_persist::{CharacterProgress, PersistError, Progression, ScenePhase};
use fmv_protocol::SyntheticRules;

use super::*;
use crate::saver::{persist, SaveJob, Saver};

const GAME: GameId = GameId::Fixture;
const NOW: u64 = 1_000;
const INSTANCE: [u8; 32] = [37; 32];

fn fixture(count: usize) -> (tempfile::TempDir, Arc<Store>, Vec<AccountId>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let accounts = (0..count).map(|slot| {
        let (account, _) = store.create_account(&format!("Member {slot}"), NOW).unwrap();
        store.put_progression(&Progression::new(account, GAME, vec![1, slot as u8, 0, 255], NOW)).unwrap();
        let record = CharacterProgress {
            exp: 40 + slot as u32,
            ap: 7 + slot as u32,
            gil: 240 + slot as u32,
            limit: 137 + slot as u16,
            inventory: Some(vec![item(0, 99), item(3, 2), item(7, 3)]),
            ..CharacterProgress::new(account, GAME, slot as u8, 5, NOW)
        };
        store.put_character_progress(&record).unwrap();
        account
    }).collect();
    (dir, store, accounts)
}

fn item(id: u16, count: u16) -> InventoryEntry { InventoryEntry::new(id, count) }

fn states(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> {
    store.get_scene_states(accounts, GAME).unwrap()
}

fn character(state: &SceneState) -> &CharacterProgress { state.pair.character.as_ref().unwrap() }

fn operation(kind: SceneOperationKind) -> SceneOperation {
    SceneOperation { instance: INSTANCE, ordinal: 0, attempt: 0, kind }
}

fn prepare_job(before: &[SceneState]) -> SceneBatchJob {
    let owner = &before[0];
    let checkpoint = SceneCheckpoint {
        instance: INSTANCE,
        scene_key: "fixture/bridge".into(),
        source_hash: [19; 32],
        replay_version: 1,
        original_story: owner.pair.story.blob.clone(),
        original_gil: character(owner).gil,
        seed: 123,
        replay: vec![0, 255, 0, 7],
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 1, battle_seed: u64::MAX - 123 },
        consumed_battles: 0,
    };
    SceneBatchJob {
        operation: operation(SceneOperationKind::PrepareBattle),
        members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember {
            next_visit: None,
            expected: expected.clone(),
            next_story: (slot == 0).then(|| vec![2, 0, 255, 0]),
            next_gil: (slot == 0).then(|| character(expected).gil - 1),
            checkpoint: (slot == 0).then(|| checkpoint.clone()),
            reservation: Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: slot as u16 }),
            battle: None,
        }).collect(),
    }
}

fn prepare(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> {
    let job = prepare_job(&states(store, accounts));
    let saved = persist_scene_batch(store, GAME, None, &job, NOW + 1).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, states(store, accounts));
    saved.current
}

/// A zero-effect result is still an explicit outcome for every reserved member.
fn resolve_job(before: &[SceneState]) -> SceneBatchJob {
    let mut checkpoint = before[0].checkpoint.clone().unwrap();
    checkpoint.phase = ScenePhase::Resume { segment: 1 };
    checkpoint.consumed_battles = 1;
    checkpoint.replay.extend_from_slice(&[1, 0, 255]);
    SceneBatchJob {
        operation: operation(SceneOperationKind::ResolveBattle),
        members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember {
            next_visit: None,
            expected: expected.clone(),
            next_story: (slot == 0).then(|| vec![3, 0, 255, 0, 9]),
            next_gil: None,
            checkpoint: (slot == 0).then(|| checkpoint.clone()),
            reservation: expected.reservation.clone(),
            battle: Some(SceneBattleSettlement {
                character: character(expected).character,
                consumed_items: Vec::new(),
                reward: BattleReward::new(expected.pair.story.account_id, 0, 0, 0),
                final_limit: None,
            }),
        }).collect(),
    }
}

fn applied(store: &Store, job: &SceneBatchJob) -> bool {
    store.scene_batch_applied(&job.operation, &job.selected_result().unwrap(), &job.accounts(), GAME).unwrap()
}

fn finish_job(before: &[SceneState]) -> SceneBatchJob {
    let mut checkpoint = before[0].checkpoint.clone().unwrap();
    checkpoint.phase = ScenePhase::Landing { destination: vec![1, 0, 255] };
    SceneBatchJob {
        operation: SceneOperation { ordinal: checkpoint.consumed_battles, ..operation(SceneOperationKind::Finish) },
        members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember {
            next_visit: None,
            expected: expected.clone(),
            next_story: None,
            next_gil: None,
            checkpoint: (slot == 0).then(|| checkpoint.clone()),
            reservation: None,
            battle: None,
        }).collect(),
    }
}

/// A refused or repeated operation must not leave even a new receipt or partial record behind.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), out);
            } else if entry.file_type().unwrap().is_file() {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn reopen(dir: &tempfile::TempDir, store: Arc<Store>) -> Store {
    drop(store);
    Store::open(dir.path().join("state")).unwrap()
}

#[test]
fn absent_next_visit_keeps_the_exact_pre_visit_decision_encoding_and_hash() {
    // Recorded decision shape from before SceneBatchMember gained next_visit. This is a
    // complete singleton Resume operation, including zeroed receipt-only timestamps/schema.
    const OLD_JSON: &str = r#"{"operation":{"instance":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"ordinal":0,"attempt":0,"kind":"Complete"},"members":[{"expected":{"pair":{"story":{"schema_version":0,"account_id":7,"game":"Fixture","blob":"00ff","updated_at":0},"character":{"schema_version":0,"account_id":7,"game":"Fixture","character":0,"level":5,"exp":0,"ap":0,"gil":24,"updated_at":0}},"checkpoint":{"instance":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"scene_key":"fixture/old","source_hash":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3],"replay_version":1,"original_story":[0,255],"original_gil":24,"seed":9,"replay":[4,0,255],"phase":{"Resume":{"segment":0}},"consumed_battles":0},"reservation":null},"next_story":[1,255],"next_gil":23,"checkpoint":null,"reservation":null,"battle":null}]}"#;
    let account = AccountId(7);
    let expected = SceneState {
        pair: fmv_persist::ProgressPair {
            story: Progression { schema_version: 0, account_id: account, game: GAME, blob: vec![0, 255], updated_at: 0 },
            character: Some(CharacterProgress { schema_version: 0, gil: 24, ..CharacterProgress::new(account, GAME, 0, 5, 0) }),
        },
        checkpoint: Some(SceneCheckpoint { instance: [1; 32], scene_key: "fixture/old".into(), source_hash: [3; 32],
            replay_version: 1, original_story: vec![0, 255], original_gil: 24, seed: 9, replay: vec![4, 0, 255],
            phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0 }),
        reservation: None, field_visit: None,
    };
    let mut job = SceneBatchJob {
        operation: SceneOperation { instance: [1; 32], ordinal: 0, attempt: 0, kind: SceneOperationKind::Complete },
        members: vec![SceneBatchMember { expected, next_story: Some(vec![1, 255]), next_gil: Some(23),
            checkpoint: None, reservation: None, battle: None, next_visit: None }],
    };
    assert_eq!(serde_json::to_vec(&job).unwrap(), OLD_JSON.as_bytes(), "None must not serialize as null or reorder earlier fields");
    let mut legacy_digest = blake3::Hasher::new();
    legacy_digest.update(b"fmv-scene-batch-decision-v1\0");
    legacy_digest.update(OLD_JSON.as_bytes());
    let expected_hash = *legacy_digest.finalize().as_bytes();
    assert_eq!(job.selected_result().unwrap(), expected_hash);
    job.members[0].expected.pair.story.schema_version = 1;
    job.members[0].expected.pair.story.updated_at = NOW;
    let character = job.members[0].expected.pair.character.as_mut().unwrap();
    character.schema_version = 3;
    character.updated_at = NOW + 4;
    assert_eq!(job.selected_result().unwrap(), expected_hash, "legacy timestamp/schema normalization is unchanged");
}

#[test]
fn preparation_saves_exact_owner_story_and_gil_with_every_participants_reservation() {
    let (dir, store, accounts) = fixture(3);
    let before = states(&store, &accounts);
    let job = prepare_job(&before);
    let saved = persist_scene_batch(&store, GAME, None, &job, NOW + 1).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    let checkpoint = saved.current[0].checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.original_story, before[0].pair.story.blob);
    assert_eq!(checkpoint.original_gil, 240);
    assert_eq!(checkpoint.replay, [0, 255, 0, 7]);
    assert_eq!(checkpoint.phase, ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 1, battle_seed: u64::MAX - 123 });
    for (slot, state) in saved.current.iter().enumerate() {
        assert_eq!(state.reservation, Some(SceneReservation { instance: INSTANCE, ordinal: 0, attempt: 0, slot: slot as u16 }));
        assert_eq!(state.checkpoint.is_some(), slot == 0);
        let old = character(&before[slot]);
        let mut expected = old.clone();
        expected.schema_version = character(state).schema_version; // metadata raises the record schema
        if slot == 0 {
            expected.gil -= 1;
            expected.updated_at = NOW + 1;
            assert_eq!(state.pair.story.blob, [2, 0, 255, 0]);
        } else {
            assert_eq!(state.pair.story, before[slot].pair.story, "watchers keep their own story");
        }
        assert_eq!(character(state), &expected, "only authored owner gil changes resources");
    }
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), saved.current);
    assert!(applied(&reopened, &job), "the preparation receipt survives reopen");
}

#[test]
fn shared_resolution_spends_before_capped_drops_and_commits_rewards_gauges_and_resume_together() {
    let (dir, store, accounts) = fixture(3);
    let rules = SyntheticRules::default();
    let before = prepare(&store, &accounts);
    let mut job = resolve_job(&before);
    let owner = job.members[0].battle.as_mut().unwrap();
    owner.consumed_items = vec![0];
    owner.reward = BattleReward { items: vec![item(0, 5)], ..BattleReward::new(accounts[0], u32::MAX, u32::MAX, u32::MAX) };
    owner.final_limit = Some(u16::MAX);
    let guest = job.members[1].battle.as_mut().unwrap();
    guest.consumed_items = vec![3];
    guest.reward = BattleReward { items: vec![item(20, 2)], ..BattleReward::new(accounts[1], 12, 20, 2) };
    let previews = job.members.iter().map(|member| {
        preview_scene_settlement(character(&member.expected), &rules, member.battle.as_ref().unwrap()).unwrap()
    }).collect::<Vec<_>>();
    let saved = persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 2).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    for (mut predicted, state) in previews.into_iter().zip(&saved.current) {
        predicted.schema_version = character(state).schema_version;
        predicted.updated_at = character(state).updated_at;
        assert_eq!(&predicted, character(state), "the world must resume with the same resources the saver will commit");
    }
    let cap = rules.progress_limits();
    let owner = character(&saved.current[0]);
    assert_eq!((owner.level, owner.exp, owner.gil, owner.ap, owner.limit), (cap.max_level, cap.max_exp, cap.max_gil, cap.max_ap, cap.max_limit));
    assert_eq!(owner.inventory, Some(vec![item(0, 99), item(3, 2), item(7, 3)]), "cost before capped drop leaves 99, not 98");
    let guest = character(&saved.current[1]);
    assert_eq!((guest.character, guest.level, guest.exp, guest.gil, guest.ap), (1, 5, 53, 261, 10));
    assert_eq!(guest.limit, 138, "no gauge result preserves the player's existing gauge");
    assert_eq!(guest.inventory, Some(vec![item(0, 99), item(3, 1), item(7, 3), item(20, 2)]));
    assert_eq!(character(&saved.current[2]), character(&before[2]), "a non-earning member's resources are untouched");
    for (old, new) in before.iter().zip(&saved.current) {
        assert_eq!(new.reservation, old.reservation, "the complete party stays reserved through resumed scene dialogue");
    }
    assert_eq!(saved.current[0].pair.story.blob, [3, 0, 255, 0, 9]);
    assert_eq!(saved.current[1].pair.story, before[1].pair.story);
    assert_eq!(saved.current[2].pair.story, before[2].pair.story);
    let checkpoint = saved.current[0].checkpoint.as_ref().unwrap();
    assert_eq!((checkpoint.phase.clone(), checkpoint.consumed_battles), (ScenePhase::Resume { segment: 1 }, 1));
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), saved.current);
    assert!(applied(&reopened, &job));
}

#[test]
fn a_missing_item_in_the_last_member_refuses_every_cost_reward_and_checkpoint() {
    let (dir, store, accounts) = fixture(3);
    let rules = SyntheticRules::default();
    let before = prepare(&store, &accounts);
    let mut job = resolve_job(&before);
    job.members[0].battle.as_mut().unwrap().consumed_items = vec![0];
    job.members[0].battle.as_mut().unwrap().reward.gil = 99;
    let last = job.members[2].battle.as_mut().unwrap();
    last.consumed_items = vec![20];
    last.reward.items = vec![item(20, 1)]; // a later drop cannot fund this command
    let bytes = files(&dir.path().join("state"));
    assert!(matches!(persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 2), Err(SceneSaveError::InsufficientItems)));
    assert_eq!(states(&store, &accounts), before);
    assert!(!applied(&store, &job));
    assert_eq!(files(&dir.path().join("state")), bytes);
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), before);
    assert!(!applied(&reopened, &job));
}

#[test]
fn unstored_inventory_uses_starting_stock_but_a_stored_empty_inventory_cannot_pay_a_cost() {
    let (dir, store, accounts) = fixture(2);
    for (slot, &account) in accounts.iter().enumerate() {
        let mut record = store.get_character_progress(account, GAME).unwrap().unwrap();
        record.inventory = if slot == 0 { None } else { Some(Vec::new()) };
        store.put_character_progress(&record).unwrap();
    }
    let rules = SyntheticRules::default();
    let before = prepare(&store, &accounts);
    let mut job = resolve_job(&before);
    for member in &mut job.members {
        member.battle.as_mut().unwrap().consumed_items = vec![0];
    }
    let bytes = files(&dir.path().join("state"));
    assert!(matches!(persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 2), Err(SceneSaveError::InsufficientItems)));
    assert_eq!(states(&store, &accounts), before, "the empty guest also prevents the owner's staged use");
    assert_eq!(files(&dir.path().join("state")), bytes);
    assert!(!applied(&store, &job));

    job.members[1].battle.as_mut().unwrap().consumed_items.clear();
    let saved = persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 3).unwrap();
    assert_eq!(character(&saved.current[0]).inventory, Some(vec![item(0, 2), item(3, 1), item(7, 1)]), "an accepted use materializes the remaining legacy stock");
    assert_eq!(character(&saved.current[1]).inventory, Some(Vec::new()), "stored empty is never replenished");
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), saved.current);
    assert!(applied(&reopened, &job));
}

#[test]
fn any_stale_participant_story_gil_or_inventory_refuses_the_whole_resolution() {
    let (dir, store, accounts) = fixture(2);
    let rules = SyntheticRules::default();
    let before = prepare(&store, &accounts);
    let bytes = files(&dir.path().join("state"));
    for changed in 0..3 {
        let mut job = resolve_job(&before);
        job.members[0].battle.as_mut().unwrap().reward.gil = 20;
        let guest = &mut job.members[1].expected;
        match changed {
            0 => guest.pair.story.blob.push(99),
            1 => guest.pair.character.as_mut().unwrap().gil += 1,
            _ => guest.pair.character.as_mut().unwrap().inventory.as_mut().unwrap()[0].count -= 1,
        }
        assert!(matches!(persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 2), Err(SceneSaveError::StaleState)), "stale component {changed}");
        assert_eq!(states(&store, &accounts), before);
        assert!(!applied(&store, &job));
        assert_eq!(files(&dir.path().join("state")), bytes);
    }
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), before);
}

#[test]
fn a_duplicate_settlement_after_later_progress_returns_current_records_without_paying_twice() {
    let (dir, store, accounts) = fixture(2);
    let rules = SyntheticRules::default();
    let before = prepare(&store, &accounts);
    let mut job = resolve_job(&before);
    for member in &mut job.members {
        let battle = member.battle.as_mut().unwrap();
        battle.consumed_items = vec![3];
        battle.reward = BattleReward { items: vec![item(20, 2)], ..BattleReward::new(member.expected.pair.story.account_id, 12, 20, 2) };
    }
    let resolved = persist_scene_batch(&store, GAME, Some(&rules), &job, NOW + 2).unwrap();
    // Finish releases all guests together; the owner retains its recoverable landing.
    let finished_job = finish_job(&resolved.current);
    let finished = persist_scene_batch(&store, GAME, None, &finished_job, NOW + 3).unwrap();
    assert!(finished.current.iter().all(|state| state.reservation.is_none()));
    assert!(matches!(finished.current[0].checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    persist(&store, GAME, &rules, &SaveJob::reward(accounts[1], 1, BattleReward::new(accounts[1], 24, 40, 4)), NOW + 4).unwrap();
    store.put_progression(&Progression::new(accounts[1], GAME, vec![9, 0, 9], NOW + 4)).unwrap();
    let later = states(&store, &accounts);
    let reopened = reopen(&dir, store);
    let bytes = files(&dir.path().join("state"));
    assert!(applied(&reopened, &job));
    assert!(applied(&reopened, &finished_job));
    let duplicate = persist_scene_batch(&reopened, GAME, Some(&rules), &job, NOW + 4).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, later, "never mirror the first settlement over later progress");
    assert_eq!(character(&duplicate.current[0]).gil, 259);
    assert_eq!(character(&duplicate.current[1]).gil, 301);
    assert_eq!(character(&duplicate.current[0]).inventory, Some(vec![item(0, 99), item(3, 1), item(7, 3), item(20, 2)]));
    // Reconstructing the same roster from a different container does not create a new payment.
    let mut reversed = job.clone();
    reversed.members.reverse();
    let duplicate = persist_scene_batch(&reopened, GAME, Some(&rules), &reversed, NOW + 5).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, later.iter().rev().cloned().collect::<Vec<_>>());
    let mut different = job;
    different.members[0].battle.as_mut().unwrap().reward.gil += 1;
    assert!(matches!(persist_scene_batch(&reopened, GAME, Some(&rules), &different, NOW + 6), Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    assert_eq!(states(&reopened, &accounts), later);
    assert_eq!(files(&dir.path().join("state")), bytes);
}

#[test]
fn an_interrupted_battle_keeps_prepared_resources_and_the_same_replay_seed_on_reopen() {
    let (dir, store, accounts) = fixture(2);
    let before = prepare(&store, &accounts);
    // Accepted actions exist only in the running attempt until its terminal batch is submitted.
    let mut abandoned = resolve_job(&before);
    abandoned.members[0].battle.as_mut().unwrap().consumed_items = vec![0, 3];
    abandoned.members[1].battle.as_mut().unwrap().reward.gil = 50;
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), before, "neither unsubmitted costs nor rewards may survive");
    assert!(!applied(&reopened, &abandoned));
    assert_eq!(character(&states(&reopened, &accounts)[0]).inventory, Some(vec![item(0, 99), item(3, 2), item(7, 3)]));
    let checkpoint = before[0].checkpoint.as_ref().unwrap();
    assert_eq!((checkpoint.seed, checkpoint.consumed_battles), (123, 0));
    assert_eq!(checkpoint.phase, ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 1, battle_seed: u64::MAX - 123 });
}

#[test]
fn a_zero_effect_settlement_preserves_the_entire_character_record_without_normalizing_it() {
    let (dir, store, accounts) = fixture(2);
    let mut raw = store.get_character_progress(accounts[1], GAME).unwrap().unwrap();
    raw.exp = u32::MAX;
    raw.ap = u32::MAX;
    raw.gil = u32::MAX;
    raw.limit = u16::MAX;
    raw.inventory = None;
    store.put_character_progress(&raw).unwrap();
    let before = prepare(&store, &accounts);
    let job = resolve_job(&before);
    let saved = persist_scene_batch(&store, GAME, Some(&SyntheticRules::default()), &job, NOW + 2).unwrap();
    for (old, new) in before.iter().zip(&saved.current) {
        assert_eq!(character(new), character(old), "no item, reward or gauge result means no resource rewrite");
    }
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), saved.current);
}

#[test]
fn the_hosted_game_and_raw_checkpoint_limit_are_checked_before_any_write() {
    let (dir, store, accounts) = fixture(2);
    let before = states(&store, &accounts);
    let job = prepare_job(&before);
    let bytes = files(&dir.path().join("state"));
    assert!(matches!(persist_scene_batch(&store, GameId::Ff7, None, &job, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    let mut oversized = job.clone();
    let checkpoint = oversized.members[0].checkpoint.as_mut().unwrap();
    checkpoint.replay = vec![255; MAX_SCENE_REPLAY_BYTES + 1];
    assert!(serde_json::to_vec(checkpoint).unwrap().len() < MAX_SCENE_CHECKPOINT_BYTES,
        "the containing JSON allowance does not waive the raw transcript bound");
    let (handle, mut queue) = crate::saver::SaverHandle::channel();
    assert!(matches!(handle.save_scene_batch(oversized.clone()), Err(SceneSaveError::InvalidRequest)));
    assert!(matches!(queue.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)), "invalid data never enters the save queue");
    assert!(matches!(persist_scene_batch(&store, GAME, None, &oversized, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    assert!(!applied(&store, &job));
    assert_eq!(states(&store, &accounts), before);
    assert_eq!(files(&dir.path().join("state")), bytes);
}

#[test]
fn only_the_checkpoint_owner_can_change_story_or_script_gil() {
    let (dir, store, accounts) = fixture(2);
    let before = states(&store, &accounts);
    let job = prepare_job(&before);
    let bytes = files(&dir.path().join("state"));
    for story_change in [true, false] {
        let mut invalid = job.clone();
        if story_change {
            invalid.members[1].next_story = Some(vec![99]);
        } else {
            invalid.members[1].next_gil = Some(999);
        }
        assert!(matches!(persist_scene_batch(&store, GAME, None, &invalid, NOW + 1), Err(SceneSaveError::InvalidRequest)));
        assert_eq!(states(&store, &accounts), before);
        assert_eq!(files(&dir.path().join("state")), bytes);
    }
    // Explicit unchanged follower values are harmless when a caller provides a full after-image.
    let mut no_op = job;
    no_op.members[1].next_story = Some(before[1].pair.story.blob.clone());
    no_op.members[1].next_gil = Some(character(&before[1]).gil);
    let saved = persist_scene_batch(&store, GAME, None, &no_op, NOW + 1).unwrap();
    assert_eq!(saved.current[1].pair.story.blob, before[1].pair.story.blob);
    assert_eq!(character(&saved.current[1]).gil, character(&before[1]).gil);
    assert!(applied(&store, &no_op));
}

#[tokio::test]
async fn queued_snapshot_follows_rewards_and_story_and_dropping_the_receiver_does_not_cancel_payment() {
    let (dir, store, accounts) = fixture(2);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())), Arc::new(fmv_fixture::FixtureAdapter::new()));
    let handle = saver.handle();
    for (slot, &account) in accounts.iter().enumerate() {
        assert!(handle.save(SaveJob::reward(account, slot as u8, BattleReward::new(account, 12, 20, 2))));
    }
    let advanced = vec![4, 0, 254, 0];
    assert!(handle.save_story(accounts[0], advanced.clone()));
    let snapshot = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!((character(&snapshot[0]).gil, character(&snapshot[1]).gil), (260, 261));
    assert_eq!((character(&snapshot[0]).exp, character(&snapshot[1]).exp), (52, 53));
    assert_eq!(snapshot[0].pair.story.blob, advanced);
    let prepare = prepare_job(&snapshot);
    let prepared = handle.save_scene_batch(prepare.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(prepared.commit, SceneCommit::Applied);
    assert_eq!(prepared.current[0].checkpoint.as_ref().unwrap().original_gil, 260);
    assert_eq!(prepared.current[0].checkpoint.as_ref().unwrap().original_story, advanced);
    let mut finish = resolve_job(&prepared.current);
    for member in &mut finish.members {
        member.battle.as_mut().unwrap().consumed_items = vec![3];
        member.battle.as_mut().unwrap().reward.gil = 10;
    }
    drop(handle.save_scene_batch(finish.clone()).unwrap()); // owner connection closes after enqueue
    let after = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!((character(&after[0]).gil, character(&after[1]).gil), (269, 271));
    for (old, new) in prepared.current.iter().zip(&after) {
        assert_eq!(new.reservation, old.reservation, "the party remains held for scene resumption");
    }
    saver.shutdown(Duration::from_secs(20)).await;
    drop(handle);
    drop(saver);
    let reopened = reopen(&dir, store);
    assert_eq!(states(&reopened, &accounts), after);
    assert!(applied(&reopened, &prepare));
    assert!(applied(&reopened, &finish));
    let duplicate = persist_scene_batch(&reopened, GAME, Some(&SyntheticRules::default()), &finish, NOW + 9).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, after);
}
