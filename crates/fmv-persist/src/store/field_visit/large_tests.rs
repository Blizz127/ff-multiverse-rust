//! The larger visit payload is an explicit envelope/index compatibility boundary. Small
//! visits retain their original V1 bytes; malformed input must not upgrade a legacy store.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use super::*;
use crate::index::INDEX_LOG_FILE;
use crate::store::scene::CharacterRecord;
use crate::{CharacterProgress, Progression, SceneCheckpoint, ScenePhase, MAX_SCENE_CHECKPOINT_BYTES,
    MAX_SCENE_RECEIPTS, MAX_SCENE_REPLAY_BYTES};
use fmv_core::{InventoryEntry, MAX_INVENTORY_STACKS};

const GAME: GameId = GameId::Fixture;
const OLD_PAYLOAD_LIMIT: usize = 1024;
const LARGE_PAYLOAD_LIMIT: usize = 8192;
const SMALL_CHARACTER_SCHEMA: u32 = 4;
const LARGE_CHARACTER_SCHEMA: u32 = 5;
const SMALL_INDEX: u32 = 5;
const LARGE_INDEX: u32 = 6;

fn fixture() -> (tempfile::TempDir, Store, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Large visit", 1).unwrap();
    store.put_progression(&Progression::new(account, GAME, vec![7, 0, 23, 255], 2)).unwrap();
    let mut character = CharacterProgress::new(account, GAME, 1, 6, 2);
    character.gil = 240;
    character.limit = 117;
    character.inventory = Some(vec![InventoryEntry::new(4, 2)]);
    store.put_character_progress(&character).unwrap();
    (dir, store, account)
}

fn operation(id: u8, kind: FieldVisitOperationKind) -> FieldVisitOperation {
    FieldVisitOperation { id: [id; 16], kind }
}

fn visit(id: u8, arrival: usize, anchor: usize) -> FieldVisitV1 {
    FieldVisitV1 {
        schema_version: 1, visit_id: [id; 16], revision: 0, field: "fixture/large".into(),
        source_identity: [7; 32], entry: FieldVisitEntryV1 { operation: [id; 16], arrival: vec![255; arrival] },
        anchor: vec![254; anchor], scratch_values: [0; 256], scratch_known_masks: [255; 256],
        active_continuation: None,
    }
}

fn entering(before: &SceneState, id: u8, arrival: usize, anchor: usize) -> SceneState {
    let mut next = before.clone();
    next.field_visit = Some(visit(id, arrival, anchor));
    next.pair.story.blob.push(id);
    next.pair.story.updated_at += 1;
    let character = next.pair.character.as_mut().unwrap();
    character.gil -= 1;
    character.updated_at += 1;
    next
}

fn commit_entry(store: &Store, account: AccountId, id: u8, arrival: usize, anchor: usize) -> SceneState {
    let before = store.get_scene_state(account, GAME).unwrap();
    store.commit_field_visit(&operation(id, FieldVisitOperationKind::Enter), &before,
        &entering(&before, id, arrival, anchor)).unwrap().current
}

fn stamped(mut state: SceneState, schema: u32) -> SceneState {
    state.pair.character.as_mut().unwrap().schema_version = schema;
    state
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() { collect(root, &path, out); }
            else if entry.file_name() != "LOCK" {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn interrupted(stage: u8) -> PersistError {
    PersistError::InvalidField { field: "injected_large_visit", reason: stage.to_string() }
}

#[test]
fn each_payload_crosses_the_old_envelope_boundary_and_stops_at_its_new_bound() {
    assert_eq!(MAX_FIELD_VISIT_ENTRY_BYTES, LARGE_PAYLOAD_LIMIT);
    assert_eq!(MAX_FIELD_VISIT_ANCHOR_BYTES, LARGE_PAYLOAD_LIMIT);
    for arrival_is_large in [false, true] {
        for size in [OLD_PAYLOAD_LIMIT, OLD_PAYLOAD_LIMIT + 1, LARGE_PAYLOAD_LIMIT, LARGE_PAYLOAD_LIMIT + 1] {
            let (dir, store, account) = fixture();
            let before = store.get_scene_state(account, GAME).unwrap();
            let (arrival, anchor) = if arrival_is_large { (size, 3) } else { (3, size) };
            let next = entering(&before, 11, arrival, anchor);
            let old_files = files(dir.path());
            let old_index = store.index().format_version();
            let result = store.commit_field_visit(&operation(11, FieldVisitOperationKind::Enter), &before, &next);
            if size > LARGE_PAYLOAD_LIMIT {
                assert!(result.is_err(), "arrival={arrival}, anchor={anchor}");
                assert_eq!(store.index().format_version(), old_index, "invalid input never selects a new writer format");
                assert_eq!(files(dir.path()), old_files, "oversize refusal writes no header, record or intent");
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
            } else {
                let large = size > OLD_PAYLOAD_LIMIT;
                let schema = if large { LARGE_CHARACTER_SCHEMA } else { SMALL_CHARACTER_SCHEMA };
                let header = if large { LARGE_INDEX } else { SMALL_INDEX };
                let saved = result.unwrap();
                assert_eq!(saved.current, stamped(next, schema));
                assert_eq!(saved.commit, SceneCommit::Applied);
                assert_eq!(store.index().format_version(), header);
                let path = store.character_path(account.0, GAME).unwrap();
                let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(raw["schema_version"], schema);
                assert_eq!(raw["field_visit"], serde_json::to_value(saved.current.field_visit.as_ref().unwrap()).unwrap());
                drop(store);
                let reopened = Store::open(dir.path()).unwrap();
                assert_eq!(reopened.index().format_version(), header);
                assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), saved.current);
            }
        }
    }
}

#[test]
fn ordinary_writes_preserve_a_maximum_visit_and_a_new_small_entry_does_not_lower_the_index() {
    let (dir, store, account) = fixture();
    let large = commit_entry(&store, account, 11, LARGE_PAYLOAD_LIMIT, LARGE_PAYLOAD_LIMIT);
    assert_eq!(large.pair.character.as_ref().unwrap().schema_version, LARGE_CHARACTER_SCHEMA);
    assert_eq!(store.index().format_version(), LARGE_INDEX);
    store.update_character_progress(account, GAME, |old| {
        let mut character = old.unwrap();
        character.gil += 17;
        character.updated_at += 1;
        character
    }).unwrap();
    let mut story = large.pair.story.clone();
    story.blob.push(99);
    story.updated_at += 1;
    store.put_progression(&story).unwrap();
    let earned = store.get_scene_state(account, GAME).unwrap();
    assert_eq!(earned.field_visit, large.field_visit);
    assert_eq!(earned.pair.character.as_ref().unwrap().schema_version, LARGE_CHARACTER_SCHEMA);
    assert_eq!(earned.pair.character.as_ref().unwrap().gil, 256);
    assert_eq!(earned.pair.character.as_ref().unwrap().inventory, large.pair.character.as_ref().unwrap().inventory);
    assert_eq!(store.index().format_version(), LARGE_INDEX);
    let next = entering(&earned, 12, 3, 4);
    let small = store.commit_field_visit(&operation(12, FieldVisitOperationKind::Enter), &earned, &next).unwrap().current;
    assert_eq!(small, stamped(next, SMALL_CHARACTER_SCHEMA));
    assert_eq!(store.index().format_version(), LARGE_INDEX, "a global compatibility fence never goes backwards");
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.index().format_version(), LARGE_INDEX);
    assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), small);
}

#[test]
fn small_visit_v1_json_keeps_its_original_field_names_order_and_byte_encodings() {
    let value = visit(1, 2, 3);
    let ones = std::iter::repeat_n("1", 16).collect::<Vec<_>>().join(",");
    let sevens = std::iter::repeat_n("7", 32).collect::<Vec<_>>().join(",");
    let golden = format!(concat!(
        "{{\"schema_version\":1,\"visit_id\":[{}],\"revision\":0,\"field\":\"fixture/large\",",
        "\"source_identity\":[{}],\"entry\":{{\"operation\":[{}],\"arrival\":[255,255]}},",
        "\"anchor\":[254,254,254],\"scratch_values\":\"{}\",\"scratch_known_masks\":\"{}\",\"active_continuation\":null}}"),
        ones, sevens, ones, "00".repeat(256), "ff".repeat(256));
    assert_eq!(serde_json::to_string(&value).unwrap(), golden);
    assert_eq!(serde_json::from_str::<FieldVisitV1>(&golden).unwrap(), value);
}

#[test]
fn maximum_visit_replay_inventory_and_receipts_together_fit_the_existing_character_cap() {
    let mut character = CharacterProgress::new(AccountId(u64::MAX), GAME, u8::MAX, u8::MAX, u64::MAX);
    character.exp = u32::MAX;
    character.ap = u32::MAX;
    character.gil = u32::MAX;
    character.limit = u16::MAX;
    character.inventory = Some((0..MAX_INVENTORY_STACKS).map(|n|
        InventoryEntry::new(u16::MAX - u16::try_from(n).unwrap(), u16::MAX)).collect());
    let mut value = serde_json::to_value(character).unwrap();
    let receipts: serde_json::Map<String, serde_json::Value> = (0..MAX_SCENE_RECEIPTS).map(|n|
        (format!("{n:064x}"), serde_json::json!({"result": "f".repeat(64), "members": 16, "slot": 15}))).collect();
    value["batch_receipts"] = serde_json::Value::Object(receipts);
    let checkpoint = SceneCheckpoint {
        instance: [255; 32], scene_key: "\\".repeat(256), source_hash: [255; 32], replay_version: u32::MAX,
        original_story: vec![255; 8 * 1024], original_gil: u32::MAX, seed: u32::MAX,
        replay: vec![255; MAX_SCENE_REPLAY_BYTES], phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    };
    assert!(serde_json::to_vec(&checkpoint).unwrap().len() <= MAX_SCENE_CHECKPOINT_BYTES);
    value["checkpoint"] = serde_json::to_value(checkpoint).unwrap();
    let mut record: CharacterRecord = serde_json::from_value(value).unwrap();
    let mut large = visit(255, LARGE_PAYLOAD_LIMIT, LARGE_PAYLOAD_LIMIT);
    large.field = "\\".repeat(MAX_FIELD_VISIT_FIELD_BYTES);
    large.revision = u64::MAX;
    large.source_identity = [255; 32];
    large.scratch_values = [255; 256];
    large.anchor.fill(255);
    large.active_continuation = Some([255; 32]);
    record.field_visit = Some(large);
    record.last_visit_commit = Some(VisitReceipt { operation: [255; 16], request: "f".repeat(64) });
    record.stamp();
    record.validate().unwrap();
    assert_eq!(record.progress.schema_version, LARGE_CHARACTER_SCHEMA);
    let bytes = encoded(Path::new("large-visit-character-bound.json"), &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    assert!(bytes.len() > 3 * 1024 * 1024, "the fixture exercises the large pretty-JSON replay, not a short stand-in");
    assert!((bytes.len() as u64) < MAX_CHARACTER_RECORD_BYTES);
    let decoded: CharacterRecord = serde_json::from_slice(&bytes).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, record);
    assert!(matches!(encoded(Path::new("large-visit-character-bound.json"), &record, bytes.len() as u64 - 1),
        Err(PersistError::TooLarge { .. })), "the unchanged per-record cap is enforced independently of payload validation");
    eprintln!("maximum combined large visit character: {} pretty JSON bytes", bytes.len());
}

#[test]
fn large_entry_and_anchor_promotion_recover_once_at_all_six_failure_boundaries() {
    for advancing in [false, true] {
        for crash_at in 0..=5 {
            let (dir, store, account) = fixture();
            let before = if advancing { commit_entry(&store, account, 11, 3, 4) }
                else { store.get_scene_state(account, GAME).unwrap() };
            let (op, next) = if advancing {
                let mut next = before.clone();
                let visit = next.field_visit.as_mut().unwrap();
                visit.revision += 1;
                visit.anchor = vec![254; LARGE_PAYLOAD_LIMIT];
                visit.scratch_values[31] = 1;
                next.pair.story.blob.push(33);
                next.pair.character.as_mut().unwrap().gil -= 3;
                (operation(12, FieldVisitOperationKind::Advance), next)
            } else {
                (operation(11, FieldVisitOperationKind::Enter), entering(&before, 11, LARGE_PAYLOAD_LIMIT, 4))
            };
            let prior_files = files(dir.path());
            let prior_index = store.index().format_version();
            let mut observed = Vec::new();
            let result = store.commit_field_visit_with_hook(&op, &before, &next, |stage| {
                observed.push(stage);
                if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }
            });
            assert!(matches!(result, Err(PersistError::InvalidField { field: "injected_large_visit", .. })));
            assert_eq!(observed, (0..=crash_at).collect::<Vec<_>>());
            if crash_at == 0 {
                assert_eq!(store.index().format_version(), prior_index);
                assert_eq!(files(dir.path()), prior_files);
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
            } else {
                assert_eq!(store.index().format_version(), LARGE_INDEX);
                assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.commit_field_visit(&op, &before, &next), Err(PersistError::TransactionRecoveryRequired)));
                assert_eq!(dir.path().join(INTENT_FILE).exists(), crash_at <= 3);
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            let expected = stamped(next.clone(), LARGE_CHARACTER_SCHEMA);
            assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), if crash_at == 0 { before.clone() } else { expected.clone() });
            let saved = reopened.commit_field_visit(&op, &before, &next).unwrap();
            assert_eq!(saved.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(saved.current, expected);
            assert_eq!(reopened.index().format_version(), LARGE_INDEX);
            let durable = files(dir.path());
            assert_eq!(reopened.commit_field_visit(&op, &before, &next).unwrap().commit, SceneCommit::AlreadyApplied);
            assert_eq!(files(dir.path()), durable);
        }
    }
}

#[test]
fn recovery_requires_index_six_when_either_image_contains_a_large_visit() {
    for large_before in [false, true] {
        let (dir, store, account) = fixture();
        let before = if large_before { commit_entry(&store, account, 11, LARGE_PAYLOAD_LIMIT, 4) }
            else { store.get_scene_state(account, GAME).unwrap() };
        let next = entering(&before, 12, if large_before { 3 } else { LARGE_PAYLOAD_LIMIT }, 4);
        store.commit_field_visit_with_hook(&operation(12, FieldVisitOperationKind::Enter), &before, &next,
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        assert_eq!(store.index().format_version(), LARGE_INDEX);
        let pending: VisitIntentFile = serde_json::from_slice(&fs::read(dir.path().join(INTENT_FILE)).unwrap()).unwrap();
        assert_eq!(pending.intent.before.character.as_ref().unwrap().progress.schema_version,
            if large_before { LARGE_CHARACTER_SCHEMA } else { 1 });
        assert_eq!(pending.intent.after.character.as_ref().unwrap().progress.schema_version,
            if large_before { SMALL_CHARACTER_SCHEMA } else { LARGE_CHARACTER_SCHEMA });
        drop(store);
        let index_path = dir.path().join(INDEX_LOG_FILE);
        // A supported compaction refuses to downgrade the writer fence. Deliberately
        // forge only the header, with a valid checksum, leaving every operation intact.
        let bytes = fs::read(&index_path).unwrap();
        let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        let line = bytes.get(..end).unwrap();
        let space = line.iter().position(|byte| *byte == b' ').unwrap();
        let mut header: serde_json::Value = serde_json::from_slice(line.get(space + 1..).unwrap()).unwrap();
        header["schema_version"] = SMALL_INDEX.into();
        let payload = serde_json::to_vec(&header).unwrap();
        let hash = blake3::hash(&payload).to_hex().to_string();
        let mut replaced = format!("{} ", hash.get(..space).unwrap()).into_bytes();
        replaced.extend_from_slice(&payload);
        replaced.extend_from_slice(bytes.get(end..).unwrap());
        fs::write(index_path, replaced).unwrap();
        let evidence = files(dir.path());
        assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })), "large before={large_before}");
        assert_eq!(files(dir.path()), evidence, "an old header cannot authorize any half of the large transaction");
    }
}

#[test]
fn a_small_character_schema_cannot_hide_a_large_visit_on_read_or_recovery() {
    // A committed record must advertise the reader/writer contract its payload needs.
    let (dir, store, account) = fixture();
    commit_entry(&store, account, 11, LARGE_PAYLOAD_LIMIT, 4);
    let path = store.character_path(account.0, GAME).unwrap();
    drop(store);
    let mut record: CharacterRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record.progress.schema_version = SMALL_CHARACTER_SCHEMA;
    fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let evidence = files(dir.path());
    assert!(matches!(CharacterRecord::read(&path), Err(PersistError::Corrupt { .. })));
    assert_eq!(files(dir.path()), evidence);

    // A valid checksum and unchanged submitted request do not make a downgraded intent valid.
    let (dir, store, account) = fixture();
    let before = store.get_scene_state(account, GAME).unwrap();
    let next = entering(&before, 11, LARGE_PAYLOAD_LIMIT, 4);
    let op = operation(11, FieldVisitOperationKind::Enter);
    store.commit_field_visit_with_hook(&op, &before, &next,
        |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
    drop(store);
    let path = dir.path().join(INTENT_FILE);
    let mut file: VisitIntentFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    file.intent.after.character.as_mut().unwrap().progress.schema_version = SMALL_CHARACTER_SCHEMA;
    assert_eq!(file.intent.request, request_digest(&op, &before, &next).unwrap(), "the original request hash remains valid");
    file.checksum = digest(&file.intent).unwrap();
    fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let evidence = files(dir.path());
    assert!(matches!(Store::open(dir.path()), Err(PersistError::Corrupt { .. })));
    assert_eq!(files(dir.path()), evidence);
}
