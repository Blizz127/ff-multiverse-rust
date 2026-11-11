//! Ordinary story writes through the real ordered saver, with FF7's exact bank semantics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmv_core::InventoryEntry;
use fmv_ff7::story::{StoryState, MAX_BANK_BLOB_BYTES};
use fmv_persist::{CharacterProgress, PersistError, Progression, SceneCheckpoint, SceneOperation,
    SceneOperationKind, ScenePhase, SceneReservation, SceneState};
use fmv_protocol::{BattleReward, SyntheticRules};

use super::*;
use crate::saver::{persist, SaveJob, Saver, SaverHandle};

const GAME: GameId = GameId::Ff7;
const NOW: u64 = 1_000;
const INSTANCE: [u8; 32] = [71; 32];

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
fn rules() -> SyntheticRules { SyntheticRules::default() }
fn item(id: u16, count: u16) -> InventoryEntry { InventoryEntry::new(id, count) }
fn story(moment: u16) -> Vec<u8> { StoryState::at(moment).encode() }

fn fixture(count: usize) -> (tempfile::TempDir, Arc<Store>, Vec<AccountId>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let accounts = (0..count).map(|slot| {
        let (account, _) = store.create_account(&format!("Ordinary member {slot}"), NOW).unwrap();
        store.put_progression(&Progression::new(account, GAME, story(36 + slot as u16), NOW)).unwrap();
        let record = CharacterProgress {
            exp: 40 + slot as u32, ap: 7 + slot as u32, gil: 240 + slot as u32,
            limit: 137 + slot as u16, inventory: Some(vec![item(0, 3), item(3, 2), item(7, 1)]),
            ..CharacterProgress::new(account, GAME, slot as u8, 5, NOW)
        };
        store.put_character_progress(&record).unwrap();
        account
    }).collect();
    (dir, store, accounts)
}

fn states(store: &Store, accounts: &[AccountId]) -> Vec<SceneState> { store.get_scene_states(accounts, GAME).unwrap() }
fn character(state: &SceneState) -> &CharacterProgress { state.pair.character.as_ref().unwrap() }

fn character_path(dir: &tempfile::TempDir, account: AccountId) -> PathBuf {
    dir.path().join("state/progression").join(account.0.to_string()).join("Ff7.character.json")
}

/// Compare real persisted bytes as well as returned values on refused or duplicate writes.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { visit(root, &entry.path(), out); }
            else if entry.file_type().unwrap().is_file() {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn full_story() -> StoryState {
    let mut state = StoryState::at(36).into_extended().unwrap();
    for bank in [1, 3, 7, 11, 13] {
        for address in 0..=255u8 {
            if bank != 1 || address > 1 { state.write_byte(bank, address, 255).unwrap(); }
        }
    }
    let bytes = state.encode();
    assert_eq!(bytes.len(), 5117);
    assert_eq!(bytes.len(), MAX_BANK_BLOB_BYTES);
    assert_eq!(StoryState::decode(&bytes), Some(state.clone()));
    state
}

fn battle_job(before: &[SceneState]) -> StoryBattleJob {
    StoryBattleJob {
        instance: INSTANCE, owner: before[0].pair.story.account_id,
        expected_story: before[0].pair.story.blob.clone(), next_story: story(39),
        members: before.iter().enumerate().map(|(slot, state)| StoryBattleMember {
            expected: state.pair.clone(),
            battle: SceneBattleSettlement {
                character: character(state).character, consumed_items: Vec::new(),
                reward: BattleReward::new(state.pair.story.account_id, 12, 20, 3),
                final_limit: (slot != 1).then_some(42),
            },
        }).collect(),
    }
}

#[tokio::test]
async fn exact_story_cas_is_queued_after_rewards_and_legacy_story_writes() {
    let (_dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let before = store.get_progress_pair(account, GAME).unwrap().character.unwrap();
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(rules())), Arc::new(adapter()));
    let handle = saver.handle();
    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 20, 3))));
    assert!(handle.save_story(account, story(37)));
    let next = StoryState::at(38).into_extended().unwrap().encode();
    let saved = handle.save_story_cas(StoryCasJob { account, expected_story: story(37), next_story: next.clone() })
        .unwrap().await.unwrap().unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.pair.story.blob, next);
    let mut expected = before;
    expected.exp += 12;
    expected.gil += 20;
    expected.ap += 3;
    expected.updated_at = saved.pair.character.as_ref().unwrap().updated_at;
    assert_eq!(saved.pair.character, Some(expected), "the story acknowledgement includes the preceding reward");
    assert_eq!(store.get_progress_pair(account, GAME).unwrap(), saved.pair);
    saver.shutdown(Duration::from_secs(20)).await;
}

#[test]
fn maximum_known_story_clears_and_numeric_assignments_preserve_every_character_byte() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let before = full_story();
    let mut after = before.clone();
    after.write_masked(3, 66, 0, 1).unwrap();
    after.write_byte(1, 4, 30).unwrap();
    after.add_byte_checked(1, 4, -7, 0, 100).unwrap();
    after.write_word(13, 101, 0x1234).unwrap();
    after.moment = 39;
    assert_eq!(after.encode().len(), 5117);
    store.put_progression(&Progression::new(account, GAME, before.encode(), NOW)).unwrap();
    let wild = CharacterProgress { exp: u32::MAX, ap: u32::MAX - 1, gil: u32::MAX, limit: u16::MAX,
        inventory: Some(vec![item(8, 60_000)]), ..CharacterProgress::new(account, GAME, 9, 250, NOW) };
    store.put_character_progress(&wild).unwrap();
    let original = store.get_character_progress(account, GAME).unwrap();
    let path = character_path(&dir, account);
    let bytes = std::fs::read(&path).unwrap();
    let saved = persist_story_cas(&store, GAME, &adapter(), &StoryCasJob {
        account, expected_story: before.encode(), next_story: after.encode(),
    }, NOW + 1).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.pair.story.blob, after.encode());
    let decoded = StoryState::decode(&saved.pair.story.blob).unwrap();
    assert_eq!(decoded, after);
    assert_eq!(decoded.known_byte(1, 4), Some(23));
    assert_eq!(decoded.known_bit(3, 66, 0), Some(false));
    assert_eq!(decoded.known_word(13, 101), Some(0x1234));
    for bank in [1, 3, 7, 11, 13] {
        for address in 0..=255u8 { assert_eq!(decoded.known_mask(bank, address), before.known_mask(bank, address)); }
    }
    assert_eq!(saved.pair.character, original, "story-only writes never normalize resources");
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "no character timestamp, schema or receipt rewrite");
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    assert_eq!(reopened.get_progress_pair(account, GAME).unwrap(), saved.pair);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn exact_story_keeps_unknown_bits_unknown_and_does_not_create_a_missing_character() {
    let (dir, store, _) = fixture(0);
    let (account, _) = store.create_account("Story only", NOW).unwrap();
    let mut before = StoryState::at(36).into_extended().unwrap();
    before.write_masked(3, 66, 1, 1).unwrap();
    let mut after = before.clone();
    after.write_masked(3, 66, 0, 1).unwrap();
    store.put_progression(&Progression::new(account, GAME, before.encode(), NOW)).unwrap();
    let saved = persist_story_cas(&store, GAME, &adapter(), &StoryCasJob {
        account, expected_story: before.encode(), next_story: after.encode(),
    }, NOW + 1).unwrap();
    assert!(saved.pair.character.is_none());
    assert!(!character_path(&dir, account).exists());
    let decoded = StoryState::decode(&saved.pair.story.blob).unwrap();
    assert_eq!(decoded.known_bit(3, 66, 0), Some(false));
    assert_eq!(decoded.known_bit(3, 66, 1), None);
    assert_eq!(decoded.known_byte(1, 4), None);
    assert_eq!(decoded, after);
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    assert_eq!(reopened.get_progress_pair(account, GAME).unwrap(), saved.pair);
}

#[test]
fn exact_story_rejects_stale_lossy_malformed_or_unsupported_transitions_without_writes() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let mut old = StoryState::at(36).into_extended().unwrap();
    old.write_masked(3, 66, 1, 1).unwrap();
    store.put_progression(&Progression::new(account, GAME, old.encode(), NOW)).unwrap();
    let mut next = old.clone();
    next.moment = 39;
    let valid = StoryCasJob { account, expected_story: old.encode(), next_story: next.encode() };
    let before = files(dir.path());
    let stale = StoryCasJob { expected_story: story(35), ..valid.clone() };
    assert!(matches!(persist_story_cas(&store, GAME, &adapter(), &stale, NOW + 1),
        Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    for bad in [story(39), StoryState::at(39).into_extended().unwrap().encode(), vec![3, 39, 0],
        vec![0; MAX_STORY_PROGRESS_BYTES + 1]] {
        let job = StoryCasJob { next_story: bad, ..valid.clone() };
        assert!(matches!(persist_story_cas(&store, GAME, &adapter(), &job, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    }
    for refused in [Adapter { game: GAME, accepts: false }, Adapter { game: GameId::Fixture, accepts: true }] {
        assert!(matches!(persist_story_cas(&store, GAME, &refused, &valid, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    }
    let (handle, mut receiver) = SaverHandle::channel();
    for job in [StoryCasJob { expected_story: vec![0; MAX_STORY_PROGRESS_BYTES + 1], ..valid.clone() },
        StoryCasJob { next_story: vec![0; MAX_STORY_PROGRESS_BYTES + 1], ..valid }] {
        assert!(matches!(handle.save_story_cas(job), Err(SceneSaveError::InvalidRequest)));
    }
    assert!(receiver.try_recv().is_err(), "oversized jobs never enter the queue");
    assert_eq!(files(dir.path()), before);
}

#[test]
fn repeating_a_to_b_to_a_to_b_spends_no_receipt_capacity_and_duplicate_returns_current_resources() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let mut a = StoryState::at(36).into_extended().unwrap();
    a.write_byte(3, 66, 1).unwrap();
    let mut b = a.clone();
    b.write_byte(3, 66, 0).unwrap();
    store.put_progression(&Progression::new(account, GAME, a.encode(), NOW)).unwrap();
    let path = character_path(&dir, account);
    let unchanged = std::fs::read(&path).unwrap();
    for turn in 0..12 {
        let (expected_story, next_story) = if turn % 2 == 0 { (a.encode(), b.encode()) } else { (b.encode(), a.encode()) };
        let job = StoryCasJob { account, expected_story, next_story };
        assert_eq!(persist_story_cas(&store, GAME, &adapter(), &job, NOW + turn + 1).unwrap().commit, SceneCommit::Applied);
        assert_eq!(std::fs::read(&path).unwrap(), unchanged, "no receipt is allocated for turn {turn}");
    }
    let job = StoryCasJob { account, expected_story: a.encode(), next_story: b.encode() };
    persist_story_cas(&store, GAME, &adapter(), &job, NOW + 20).unwrap();
    persist(&store, GAME, &rules(), &SaveJob::reward(account, 0, BattleReward::new(account, 0, 19, 0)), NOW + 21).unwrap();
    let current = store.get_progress_pair(account, GAME).unwrap();
    let before = files(dir.path());
    let duplicate = persist_story_cas(&store, GAME, &adapter(), &job, NOW + 22).unwrap();
    assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.pair, current);
    assert_eq!(files(dir.path()), before);
}

#[tokio::test]
async fn a_dropped_story_acknowledgement_still_commits_durably_and_the_queue_survives_refusal() {
    let (dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let saver = Saver::spawn(Arc::clone(&store), GAME, None, Arc::new(adapter()));
    let handle = saver.handle();
    let stale = StoryCasJob { account, expected_story: story(35), next_story: story(38) };
    assert!(matches!(handle.save_story_cas(stale).unwrap().await.unwrap(), Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    let job = StoryCasJob { account, expected_story: story(36), next_story: story(39) };
    drop(handle.save_story_cas(job.clone()).unwrap());
    saver.shutdown(Duration::from_secs(20)).await;
    let saved = store.get_progress_pair(account, GAME).unwrap();
    assert_eq!(saved.story.blob, story(39));
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    assert_eq!(reopened.get_progress_pair(account, GAME).unwrap(), saved);
    assert_eq!(persist_story_cas(&reopened, GAME, &adapter(), &job, NOW + 1).unwrap().commit, SceneCommit::AlreadyApplied);
    assert!(matches!(handle.save_story_cas(job), Err(SceneSaveError::Stopped)));
}

#[test]
fn one_battle_commits_owner_banks_all_rewards_and_optional_gauges_without_changing_guest_stories() {
    let (_dir, store, accounts) = fixture(3);
    let old = full_story();
    store.put_progression(&Progression::new(accounts[0], GAME, old.encode(), NOW)).unwrap();
    let before = states(&store, &accounts);
    let mut job = battle_job(&before);
    let mut next = old;
    next.write_masked(3, 66, 0, 1).unwrap();
    next.write_byte(1, 4, 29).unwrap();
    next.moment = 39;
    job.next_story = next.encode();
    job.members[0].battle.reward.items = vec![item(0, 2), item(9, 1)];
    let saved = persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 1).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current, states(&store, &accounts));
    for (slot, state) in saved.current.iter().enumerate() {
        let mut expected = character(&before[slot]).clone();
        expected.exp += 12;
        expected.gil += 20;
        expected.ap += 3;
        expected.updated_at = NOW + 1;
        expected.schema_version = character(state).schema_version;
        if slot != 1 { expected.limit = 42; }
        if slot == 0 { expected.inventory = Some(vec![item(0, 5), item(3, 2), item(7, 1), item(9, 1)]); }
        assert_eq!(character(state), &expected, "member {slot}");
        if slot == 0 {
            assert_eq!(state.pair.story.blob, next.encode());
            assert_eq!(StoryState::decode(&state.pair.story.blob), Some(next.clone()));
        } else { assert_eq!(state.pair.story, before[slot].pair.story, "guest story and its timestamp are untouched"); }
        assert!(state.checkpoint.is_none() && state.reservation.is_none());
    }
    let repeated = persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 2).unwrap();
    assert_eq!(repeated.commit, SceneCommit::AlreadyApplied);
    assert_eq!(repeated.current, saved.current);
    let mut another = battle_job(&saved.current);
    another.instance = [72; 32];
    another.next_story = job.next_story;
    let again = persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &another, NOW + 3).unwrap();
    assert_eq!(again.commit, SceneCommit::Applied, "a fresh battle instance can earn another reward at the same story");
    for (old, new) in saved.current.iter().zip(&again.current) { assert_eq!(character(new).gil, character(old).gil + 20); }
}

#[test]
fn an_actual_battle_reconciles_a_legacy_character_to_the_authoritative_slot() {
    let (_dir, store, accounts) = fixture(1);
    let account = accounts[0];
    let mut raw = store.get_character_progress(account, GAME).unwrap().unwrap();
    raw.character = 9;
    store.put_character_progress(&raw).unwrap();
    persist(&store, GAME, &rules(), &SaveJob::used_item(account, 2, 0), NOW + 1).unwrap();
    let before = states(&store, &accounts);
    assert_eq!(character(&before[0]).character, 9, "an item-only save preserves the raw slot");
    let mut job = battle_job(&before);
    job.members[0].battle.character = 2;
    let saved = persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 2).unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    let record = character(&saved.current[0]);
    assert_eq!((record.character, record.exp, record.gil, record.ap, record.limit), (2, 52, 260, 10, 42));
    assert_eq!(record.inventory, character(&before[0]).inventory, "the earlier item use is not replayed");
    assert_eq!(saved.current[0].pair.story.blob, job.next_story);
    let retry = persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 3).unwrap();
    assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
    assert_eq!(retry.current, saved.current, "the exact raw predecessor remains valid as a receipt identity");
}

#[test]
fn a_battle_receipt_survives_reopen_and_returns_later_pairs_before_rules_or_adapter_revalidation() {
    let (dir, store, accounts) = fixture(2);
    let job = battle_job(&states(&store, &accounts));
    persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 1).unwrap();
    for (slot, account) in accounts.iter().enumerate() {
        persist(&store, GAME, &rules(), &SaveJob::reward(*account, slot as u8, BattleReward::new(*account, 1, 31, 2)), NOW + 2).unwrap();
        let expected_story = store.get_progression(*account, GAME).unwrap().blob;
        persist_story_cas(&store, GAME, &adapter(), &StoryCasJob { account: *account, expected_story, next_story: story(45 + slot as u16) }, NOW + 3).unwrap();
    }
    let current = states(&store, &accounts);
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    let before = files(dir.path());
    let refusal = Adapter { game: GAME, accepts: false };
    let repeated = persist_story_battle(&reopened, GAME, None, &refusal, &job, NOW + 4).unwrap();
    assert_eq!(repeated.commit, SceneCommit::AlreadyApplied);
    assert_eq!(repeated.current, current, "later resources and owner/guest story changes survive retry");
    let mut reordered = job.clone();
    reordered.members.reverse();
    for member in &mut reordered.members {
        member.expected.story.updated_at = 999;
        member.expected.character.as_mut().unwrap().updated_at = 999;
    }
    let reversed = persist_story_battle(&reopened, GAME, None, &refusal, &reordered, NOW + 5).unwrap();
    assert_eq!(reversed.commit, SceneCommit::AlreadyApplied);
    assert_eq!(reversed.current, current.into_iter().rev().collect::<Vec<_>>());
    let mut conflict = job;
    conflict.members[0].battle.reward.gil += 1;
    assert!(matches!(persist_story_battle(&reopened, GAME, Some(&rules()), &adapter(), &conflict, NOW + 6),
        Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    assert_eq!(files(dir.path()), before, "duplicates and conflicting outcomes write no records or receipts");
}

#[tokio::test]
async fn saved_item_uses_precede_the_battle_and_a_dropped_battle_acknowledgement_does_not_charge_twice() {
    let (dir, store, accounts) = fixture(2);
    let saver = Saver::spawn(Arc::clone(&store), GAME, Some(Arc::new(rules())), Arc::new(adapter()));
    let handle = saver.handle();
    assert!(handle.save(SaveJob::used_item(accounts[0], 0, 0)));
    assert!(handle.save(SaveJob::reward(accounts[1], 1, BattleReward::new(accounts[1], 0, 5, 0))));
    let snapshot = handle.snapshot_scene_batch(accounts.clone()).unwrap().await.unwrap().unwrap();
    assert_eq!(character(&snapshot[0]).inventory.as_ref().unwrap()[0], item(0, 2));
    assert_eq!(character(&snapshot[1]).gil, 246);
    let job = battle_job(&snapshot);
    drop(handle.save_story_battle(job.clone()).unwrap());
    saver.shutdown(Duration::from_secs(20)).await;
    let saved = states(&store, &accounts);
    assert_eq!(character(&saved[0]).inventory.as_ref().unwrap()[0], item(0, 2), "the accepted item use was already paid");
    assert_eq!(character(&saved[1]).gil, 266);
    assert_eq!(saved[0].pair.story.blob, job.next_story);
    drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap();
    let retry = persist_story_battle(&reopened, GAME, Some(&rules()), &adapter(), &job, NOW + 1).unwrap();
    assert_eq!(retry.commit, SceneCommit::AlreadyApplied);
    assert_eq!(retry.current, saved);
    assert!(matches!(handle.save_story_battle(job), Err(SceneSaveError::Stopped)));
}

#[test]
fn stale_complete_pairs_missing_rules_and_refused_adapter_leave_every_member_unchanged() {
    let (dir, store, accounts) = fixture(2);
    let job = battle_job(&states(&store, &accounts));
    let before = files(dir.path());
    let mut stale_timestamp = job.clone();
    stale_timestamp.members[1].expected.character.as_mut().unwrap().updated_at += 1;
    let mut stale_inventory = job.clone();
    stale_inventory.members[1].expected.character.as_mut().unwrap().inventory = Some(vec![item(0, 2)]);
    let mut stale_guest_story = job.clone();
    stale_guest_story.members[1].expected.story.blob = story(38);
    for stale in [stale_timestamp, stale_inventory, stale_guest_story] {
        assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &stale, NOW + 1), Err(SceneSaveError::StaleState)));
    }
    assert!(matches!(persist_story_battle(&store, GAME, None, &adapter(), &job, NOW + 1), Err(SceneSaveError::MissingRules)));
    for refused in [Adapter { game: GAME, accepts: false }, Adapter { game: GameId::Fixture, accepts: true }] {
        assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &refused, &job, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    }
    let mut malformed = job;
    malformed.next_story = vec![3, 39, 0];
    assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &malformed, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    assert_eq!(files(dir.path()), before);
}

#[test]
fn battle_preflight_rejects_already_saved_item_costs_missing_characters_and_bad_rosters_before_enqueue() {
    let (dir, store, accounts) = fixture(2);
    let job = battle_job(&states(&store, &accounts));
    let before = files(dir.path());
    let (handle, mut receiver) = SaverHandle::channel();
    let mut used = job.clone();
    used.members[0].battle.consumed_items.push(0);
    let mut no_instance = job.clone();
    no_instance.instance = [0; 32];
    let mut duplicate = job.clone();
    duplicate.members.push(duplicate.members[0].clone());
    let mut no_owner = job.clone();
    no_owner.owner = AccountId(u64::MAX);
    let mut too_large = job.clone();
    too_large.next_story = vec![0; MAX_STORY_PROGRESS_BYTES + 1];
    let mut wrong_game = job.clone();
    wrong_game.members[1].expected.story.game = GameId::Fixture;
    for bad in [used, no_instance, duplicate, no_owner, too_large, wrong_game] {
        assert!(matches!(handle.save_story_battle(bad.clone()), Err(SceneSaveError::InvalidRequest)));
        assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &bad, NOW + 1), Err(SceneSaveError::InvalidRequest)));
    }
    let mut no_character = job;
    no_character.members[0].expected.character = None;
    assert!(matches!(handle.save_story_battle(no_character.clone()), Err(SceneSaveError::MissingCharacter)));
    assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &no_character, NOW + 1), Err(SceneSaveError::MissingCharacter)));
    assert!(receiver.try_recv().is_err());
    assert_eq!(files(dir.path()), before);
}

#[test]
fn ordinary_story_and_battle_writes_refuse_both_checkpoint_owner_and_reserved_guest() {
    let (dir, store, accounts) = fixture(2);
    let before = states(&store, &accounts);
    let job = battle_job(&before);
    let instance = [73; 32];
    let mut held = before.clone();
    for (slot, state) in held.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance, ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    held[0].checkpoint = Some(SceneCheckpoint {
        instance, scene_key: "ff7/synthetic/held".into(), source_hash: [19; 32], replay_version: 1,
        original_story: before[0].pair.story.blob.clone(), original_gil: character(&before[0]).gil,
        seed: 1, replay: vec![1], phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 1, battle_seed: 9 },
        consumed_battles: 0,
    });
    let operation = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::PrepareBattle };
    store.commit_scene_batch(&operation, b"ordinary-test-hold", &before, &held).unwrap();
    let saved = states(&store, &accounts);
    let bytes = files(dir.path());
    assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &job, NOW + 1),
        Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    for state in &saved {
        let account = state.pair.story.account_id;
        for next_story in [state.pair.story.blob.clone(), story(39)] {
            let cas = StoryCasJob { account, expected_story: state.pair.story.blob.clone(), next_story };
            assert!(matches!(persist_story_cas(&store, GAME, &adapter(), &cas, NOW + 1),
                Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
        }
        let solo = battle_job(std::slice::from_ref(state));
        assert!(matches!(persist_story_battle(&store, GAME, Some(&rules()), &adapter(), &solo, NOW + 1),
            Err(SceneSaveError::Store(PersistError::TransactionConflict(_)))));
    }
    assert_eq!(states(&store, &accounts), saved);
    assert_eq!(files(dir.path()), bytes);
}
