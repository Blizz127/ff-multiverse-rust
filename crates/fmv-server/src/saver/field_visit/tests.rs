//! Idle field-visit decisions through the real FIFO and durable receipt path. The fixtures
//! supply already validated synthetic results; they do not enable any field runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmv_core::{AccountId, InventoryEntry, MAX_INVENTORY_STACKS};
use fmv_ff7::story::StoryState;
use fmv_persist::{CharacterProgress, FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind,
    FieldVisitV1, PersistError, Progression, SceneCheckpoint, SceneCommit, ScenePhase, SceneReservation, SceneState,
    MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES};
use fmv_protocol::{BattleReward, SyntheticRules, MAX_STORY_PROGRESS_BYTES};

use super::*;
use crate::saver::{SaveJob, Saver};

const GAME: GameId = GameId::Ff7;
const NOW: u64 = 1_000;

struct Adapter { game: GameId, accepts: bool }

impl GameAdapter for Adapter {
    fn game(&self) -> GameId { self.game }
    fn list_areas(&self) -> Vec<fmv_adapter_api::AreaInfo> { Vec::new() }
    fn load_area(&self, key: &str) -> Result<Box<dyn fmv_adapter_api::AreaRuntime>, fmv_adapter_api::AdapterError> {
        Err(fmv_adapter_api::AdapterError::AreaNotFound(key.to_string()))
    }
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        fmv_ff7::story::merge_progression(stored, advanced)
    }
    fn compare_exchange_progression(&self, stored: &[u8], expected: &[u8], next: &[u8]) -> Option<Vec<u8>> {
        self.accepts.then(|| fmv_ff7::story::compare_exchange_progression(stored, expected, next)).flatten()
    }
}

fn adapter() -> Adapter { Adapter { game: GAME, accepts: true } }
fn operation(id: u8, kind: FieldVisitOperationKind) -> FieldVisitOperation { FieldVisitOperation { id: [id; 16], kind } }
fn character(state: &SceneState) -> &CharacterProgress { state.pair.character.as_ref().unwrap() }

fn fixture() -> (tempfile::TempDir, Arc<Store>, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let (account, _) = store.create_account("Visit member", NOW).unwrap();
    let mut story = StoryState::at(51).into_extended().unwrap();
    story.write_byte(1, 4, 50).unwrap();
    story.write_masked(3, 66, 1, 1).unwrap();
    store.put_progression(&Progression::new(account, GAME, story.encode(), NOW)).unwrap();
    let progress = CharacterProgress {
        exp: 40, ap: 7, gil: 240, limit: 137,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 2)]),
        ..CharacterProgress::new(account, GAME, 0, 5, NOW)
    };
    store.put_character_progress(&progress).unwrap();
    (dir, store, account)
}

fn entry(before: &SceneState) -> FieldVisitSaveJob {
    let op = operation(11, FieldVisitOperationKind::Enter);
    let mut next = before.clone();
    let mut masks = [0; 256];
    masks[31] = 1;
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [31; 16], revision: 0, field: "fixture/train".into(),
        source_identity: [41; 32], entry: FieldVisitEntryV1 { operation: op.id, arrival: vec![1, 4, 2] },
        anchor: vec![2, 1, 8], scratch_values: [0; 256], scratch_known_masks: masks,
        active_continuation: None,
    });
    FieldVisitSaveJob::new(op, before.clone(), next, NOW + 1).unwrap()
}

fn entered(store: &Store, account: AccountId) -> SceneState {
    let job = entry(&store.get_scene_state(account, GAME).unwrap());
    persist_field_visit(store, GAME, &adapter(), &job).unwrap().current
}

fn decision(before: &SceneState, id: u8, now: u64) -> FieldVisitSaveJob {
    let mut next = before.clone();
    let mut story = StoryState::decode(&next.pair.story.blob).unwrap();
    story.add_byte_checked(1, 4, -3, 0, 100).unwrap();
    next.pair.story.blob = story.encode();
    next.pair.character.as_mut().unwrap().gil -= 1;
    let visit = next.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.scratch_values[31] = 1;
    visit.anchor = vec![2, 3, 8];
    FieldVisitSaveJob::new(operation(id, FieldVisitOperationKind::Advance), before.clone(), next, now).unwrap()
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { read(root, &entry.path(), out); }
            else if entry.file_type().unwrap().is_file() {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    read(root, root, &mut out);
    out
}

#[tokio::test]
async fn entry_returns_the_stamped_state_but_retries_keep_the_original_caller_schema() {
    let (dir, store, account) = fixture();
    let saver = Saver::spawn(Arc::clone(&store), GAME, None, Arc::new(adapter()));
    let handle = saver.handle();
    let before = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap().pop().unwrap();
    assert!(before.field_visit.is_none());
    let job = entry(&before);
    assert_eq!(character(job.next()).schema_version, 1);
    let entered = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(entered.commit, SceneCommit::Applied);
    assert_eq!(character(&entered.current).schema_version, 4);
    let mut expected = job.next().clone();
    expected.pair.character.as_mut().unwrap().schema_version = 4;
    assert_eq!(entered.current, expected);
    assert_eq!(character(job.next()).schema_version, 1, "storage cannot alter the frozen retry request");
    let durable = files(dir.path());
    let queued_retry = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(queued_retry.commit, SceneCommit::AlreadyApplied);
    assert_eq!(queued_retry.current, entered.current);
    assert_eq!(files(dir.path()), durable);
    saver.shutdown(Duration::from_secs(20)).await;
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    let retry = persist_field_visit(&reopened, GAME, &adapter(), &job).unwrap();
    assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
    assert_eq!(retry.current, entered.current);
    assert_eq!(files(dir.path()), durable);
    let next = decision(&retry.current, 12, NOW + 2);
    assert_eq!(persist_field_visit(&reopened, GAME, &adapter(), &next).unwrap().current, *next.next());
}

#[tokio::test]
async fn the_ordered_baseline_includes_rewards_and_the_acknowledgement_contains_one_complete_visit_result() {
    let (_dir, store, account) = fixture();
    let initial = entered(&store, account);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())), Arc::new(adapter()));
    let handle = saver.handle();
    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 20, 3))));
    let mut baseline = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap();
    let before = baseline.pop().unwrap();
    assert_eq!(before.field_visit, initial.field_visit);
    assert_eq!((character(&before).exp, character(&before).gil, character(&before).ap), (52, 260, 10));
    let job = decision(&before, 12, NOW + 2);
    assert_eq!(job.expected(), &before);
    assert_eq!(job.next().pair.story.updated_at, NOW + 2);
    assert_eq!(character(job.next()).updated_at, NOW + 2);
    let saved = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, *job.next());
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), saved.current);
    assert_eq!(StoryState::decode(&saved.current.pair.story.blob).unwrap().known_byte(1, 4), Some(47));
    assert_eq!((character(&saved.current).exp, character(&saved.current).gil, character(&saved.current).ap), (52, 259, 10));
    assert_eq!(character(&saved.current).inventory, character(&before).inventory);
    assert_eq!(character(&saved.current).limit, 137);
    let visit = saved.current.field_visit.as_ref().unwrap();
    assert_eq!((visit.revision, visit.scratch_values[31], visit.scratch_known_masks[31]), (1, 1, 1));
    assert_eq!(visit.scratch_known_masks[30], 0, "the bridge does not invent an adjacent known byte");
    assert_eq!((visit.visit_id, visit.source_identity, &visit.entry),
        (initial.field_visit.as_ref().unwrap().visit_id, initial.field_visit.as_ref().unwrap().source_identity, &initial.field_visit.as_ref().unwrap().entry));

    // A following result uses the acknowledged snapshot, including the stored schema and time.
    let mut neutral = saved.current.clone();
    neutral.field_visit.as_mut().unwrap().revision += 1;
    neutral.field_visit.as_mut().unwrap().anchor = vec![2, 4, 8];
    let next = FieldVisitSaveJob::new(operation(13, FieldVisitOperationKind::Advance), saved.current.clone(), neutral, NOW + 3).unwrap();
    let acknowledged = handle.save_field_visit(next.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(acknowledged.current, *next.next());
    assert_eq!(acknowledged.current.pair.story.blob, saved.current.pair.story.blob);
    assert_eq!(character(&acknowledged.current).gil, 259, "a neutral scratch result can still advance its durable anchor");
    saver.shutdown(Duration::from_secs(20)).await;
}

#[tokio::test]
async fn an_identical_retry_returns_later_rewards_and_never_repeats_the_latched_payment() {
    let (dir, store, account) = fixture();
    let before = entered(&store, account);
    let job = decision(&before, 12, NOW + 2);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(SyntheticRules::default())), Arc::new(adapter()));
    let handle = saver.handle();
    let committed = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(committed.commit, SceneCommit::Applied);
    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 20, 3))));
    let earned = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap().pop().unwrap();
    let durable = files(dir.path());
    let duplicate = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, earned);
    assert_eq!(character(&duplicate.current).gil, 259);
    assert_eq!(StoryState::decode(&duplicate.current.pair.story.blob).unwrap().known_byte(1, 4), Some(47));
    assert_eq!(files(dir.path()), durable, "the exact frozen retry does not rewrite a timestamp or receipt");

    // Rebuilding the request at another time is a different selected request, not a retry.
    let altered = FieldVisitSaveJob::new(job.operation(), job.expected().clone(), job.next().clone(), NOW + 3).unwrap();
    assert!(matches!(handle.save_field_visit(altered).unwrap().await.unwrap(), Err(SceneSaveError::StaleState)));
    assert_eq!(files(dir.path()), durable);
    // A known refusal does not stop the FIFO. Rebase a genuinely new decision on the
    // ordered current snapshot, rather than altering and replaying the old request.
    let fresh = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap().pop().unwrap();
    assert_eq!(fresh, earned);
    let mut next = fresh.clone();
    next.field_visit.as_mut().unwrap().revision += 1;
    next.field_visit.as_mut().unwrap().anchor = vec![2, 4, 8];
    let following = FieldVisitSaveJob::new(operation(13, FieldVisitOperationKind::Advance), fresh, next, NOW + 4).unwrap();
    let saved = handle.save_field_visit(following.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, *following.next());
    assert_eq!(character(&saved.current).gil, 259);
    saver.shutdown(Duration::from_secs(20)).await;
}

#[test]
fn every_part_of_the_full_predecessor_must_still_match_before_a_visit_can_commit() {
    for changed in ["story", "story timestamp", "gil", "inventory", "limit", "scratch"] {
        let (dir, store, account) = fixture();
        let before = entered(&store, account);
        let pending = decision(&before, 12, NOW + 2);
        match changed {
            "story" | "story timestamp" => {
                let mut record = before.pair.story.clone();
                if changed == "story" {
                    let mut story = StoryState::decode(&record.blob).unwrap();
                    story.write_byte(1, 4, 49).unwrap();
                    record.blob = story.encode();
                } else { record.updated_at += 1; }
                store.put_progression(&record).unwrap();
            }
            "scratch" => {
                let mut next = before.clone();
                let visit = next.field_visit.as_mut().unwrap();
                visit.revision += 1;
                visit.scratch_known_masks[30] = 2;
                visit.scratch_values[30] = 2;
                let earlier = FieldVisitSaveJob::new(operation(13, FieldVisitOperationKind::Advance), before.clone(), next, NOW + 1).unwrap();
                persist_field_visit(&store, GAME, &adapter(), &earlier).unwrap();
            }
            _ => {
                let mut record = character(&before).clone();
                match changed {
                    "gil" => record.gil += 1,
                    "inventory" => record.inventory = Some(vec![InventoryEntry::new(8, 1)]),
                    "limit" => record.limit += 1,
                    _ => unreachable!(),
                }
                store.put_character_progress(&record).unwrap();
            }
        }
        let current = store.get_scene_state(account, GAME).unwrap();
        let durable = files(dir.path());
        assert!(matches!(persist_field_visit(&store, GAME, &adapter(), &pending), Err(SceneSaveError::StaleState)), "{changed}");
        assert_eq!(store.get_scene_state(account, GAME).unwrap(), current, "{changed}");
        assert_eq!(files(dir.path()), durable, "{changed}");
    }
}

#[tokio::test]
async fn dropping_an_acknowledgement_does_not_cancel_a_commit_and_stopped_queues_refuse() {
    let (dir, store, account) = fixture();
    let before = entered(&store, account);
    let job = decision(&before, 12, NOW + 2);
    let saver = Saver::spawn(Arc::clone(&store), GAME, None, Arc::new(adapter()));
    let handle = saver.handle();
    drop(handle.save_field_visit(job.clone()).unwrap());
    saver.shutdown(Duration::from_secs(20)).await;
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), *job.next());
    assert!(matches!(handle.save_field_visit(job.clone()), Err(SceneSaveError::Stopped)));
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    let saved = persist_field_visit(&reopened, GAME, &adapter(), &job).unwrap();
    assert_eq!(saved.commit, SceneCommit::AlreadyApplied);
    assert_eq!(saved.current, *job.next());
}

#[test]
fn construction_refuses_unbounded_noncanonical_or_nonidle_images_before_they_can_be_queued() {
    let (dir, store, account) = fixture();
    let before = entered(&store, account);
    let valid = decision(&before, 12, NOW + 2);
    let durable = files(dir.path());
    for bad in ["operation", "story", "expected story", "expected visit", "next visit", "field", "arrival", "anchor", "visit schema", "identity",
        "source", "unknown", "revision", "knowledge loss", "inventory", "character account", "character game",
        "reservation", "checkpoint", "active continuation"] {
        let mut op = valid.operation();
        let mut expected = valid.expected().clone();
        let mut next = valid.next().clone();
        match bad {
            "operation" => op.id = [0; 16],
            "story" => next.pair.story.blob = vec![0; MAX_STORY_PROGRESS_BYTES + 1],
            "expected story" => expected.pair.story.blob = vec![0; MAX_STORY_PROGRESS_BYTES + 1],
            "expected visit" => expected.field_visit = None,
            "next visit" => next.field_visit = None,
            "inventory" => next.pair.character.as_mut().unwrap().inventory = Some(vec![InventoryEntry::new(0, 1); MAX_INVENTORY_STACKS + 1]),
            "character account" => next.pair.character.as_mut().unwrap().account_id = AccountId(account.0 + 1),
            "character game" => next.pair.character.as_mut().unwrap().game = GameId::Fixture,
            "reservation" => next.reservation = Some(SceneReservation { instance: [7; 32], ordinal: 0, attempt: 0, slot: 0 }),
            "checkpoint" => next.checkpoint = Some(SceneCheckpoint {
                instance: [7; 32], scene_key: "fixture/held".into(), source_hash: [8; 32], replay_version: 1,
                original_story: expected.pair.story.blob.clone(), original_gil: character(&expected).gil,
                seed: 1, replay: Vec::new(), consumed_battles: 0,
                phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 12, battle_seed: 123 },
            }),
            _ => {
                let visit = next.field_visit.as_mut().unwrap();
                match bad {
                    "field" => visit.field = "x".repeat(MAX_FIELD_VISIT_FIELD_BYTES + 1),
                    "arrival" => visit.entry.arrival = vec![0; MAX_FIELD_VISIT_ENTRY_BYTES + 1],
                    "anchor" => visit.anchor = vec![0; MAX_FIELD_VISIT_ANCHOR_BYTES + 1],
                    "visit schema" => visit.schema_version = 2,
                    "identity" => visit.visit_id = [0; 16],
                    "source" => visit.source_identity = [0; 32],
                    "unknown" => visit.scratch_values[30] = 1,
                    "revision" => visit.revision += 1,
                    "knowledge loss" => { visit.scratch_values[31] = 0; visit.scratch_known_masks[31] = 0; }
                    "active continuation" => visit.active_continuation = Some([7; 32]),
                    _ => unreachable!(),
                }
            }
        }
        assert!(matches!(FieldVisitSaveJob::new(op, expected, next, NOW + 2), Err(SceneSaveError::InvalidRequest)), "{bad}");
    }
    for missing_before in [false, true] {
        let mut expected = valid.expected().clone();
        let mut next = valid.next().clone();
        if missing_before { expected.pair.character = None; } else { next.pair.character = None; }
        assert!(matches!(FieldVisitSaveJob::new(valid.operation(), expected, next, NOW + 2), Err(SceneSaveError::MissingCharacter)));
    }
    assert_eq!(files(dir.path()), durable);
}

#[test]
fn wrong_or_refusing_adapters_cannot_commit_an_otherwise_well_formed_job() {
    let (dir, store, account) = fixture();
    let before = entered(&store, account);
    let job = decision(&before, 12, NOW + 2);
    let durable = files(dir.path());
    for refused in [Adapter { game: GAME, accepts: false }, Adapter { game: GameId::Fixture, accepts: true }] {
        assert!(matches!(persist_field_visit(&store, GAME, &refused, &job), Err(SceneSaveError::InvalidRequest)));
    }
    assert!(matches!(persist_field_visit(&store, GameId::Fixture, &adapter(), &job), Err(SceneSaveError::InvalidRequest)));
    assert_eq!(files(dir.path()), durable);
}

#[tokio::test]
async fn an_uncertain_intent_write_is_returned_without_retry_and_fences_the_following_job() {
    let (dir, store, account) = fixture();
    let before = entered(&store, account);
    let job = decision(&before, 12, NOW + 2);
    // The intent's atomic rename cannot replace a directory. It fails after the store's
    // uncertainty latch was set, before either authoritative progress file was installed.
    let obstacle = dir.path().join("state/pending-scene.json");
    std::fs::create_dir(&obstacle).unwrap();
    let durable = files(dir.path());
    let saver = Saver::spawn(Arc::clone(&store), GAME, None, Arc::new(adapter()));
    let handle = saver.handle();
    let first = handle.save_field_visit(job.clone()).unwrap();
    let second = handle.save_field_visit(job.clone()).unwrap();
    assert!(matches!(first.await.unwrap(), Err(SceneSaveError::Store(PersistError::Io { .. }))),
        "the original I/O error reaches its caller; retrying would instead yield RecoveryRequired");
    assert!(matches!(second.await.unwrap(), Err(SceneSaveError::Store(PersistError::TransactionRecoveryRequired))));
    assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
    assert_eq!(files(dir.path()), durable, "neither request installed a partial story or latch");
    saver.shutdown(Duration::from_secs(20)).await;
    drop(store);
    std::fs::remove_dir(&obstacle).unwrap();
    let reopened = Store::open(dir.path().join("state")).unwrap();
    assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), before);
    assert_eq!(persist_field_visit(&reopened, GAME, &adapter(), &job).unwrap().commit, SceneCommit::Applied);
}
