use super::*;
use crate::index::{FIELD_VISIT_INDEX_VERSION, INDEX_LOG_FILE};
use crate::model::MAX_BLOB_BYTES;
use crate::store::scene::{CharacterRecord, MAX_SCENE_RECEIPTS};
use crate::{CharacterProgress, Progression, SceneOperation, SceneOperationKind, ScenePhase};
use fmv_core::{InventoryEntry, MAX_INVENTORY_STACKS};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const GAME: GameId = GameId::Ff7;

fn interrupted(stage: u8) -> PersistError {
    PersistError::InvalidField { field: "injected_visit", reason: stage.to_string() }
}

fn operation(kind: FieldVisitOperationKind, id: u8) -> FieldVisitOperation {
    FieldVisitOperation { id: [id; 16], kind }
}

fn visit(id: u8) -> FieldVisitV1 {
    let mut scratch_values = [0; 256];
    let mut scratch_known_masks = [0; 256];
    scratch_values[19] = 0x81;
    scratch_known_masks[19] = 0x83;
    FieldVisitV1 {
        schema_version: 1, visit_id: [id; 16], revision: 0, field: "fixture/reactor".into(),
        source_identity: [7; 32], entry: FieldVisitEntryV1 { operation: [id; 16], arrival: vec![4, 255, 9] },
        anchor: vec![8, 0, 255, 2], scratch_values, scratch_known_masks, active_continuation: None,
    }
}

fn rig() -> (tempfile::TempDir, Store, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (account, _) = store.create_account("Visit", 1).unwrap();
    let mut character = CharacterProgress::new(account, GAME, 9, 7, 2);
    character.gil = 240;
    character.exp = 626;
    character.ap = 17;
    character.limit = 203;
    character.inventory = Some(vec![InventoryEntry::new(8, 3), InventoryEntry::new(1, 19)]);
    store.put_character_progress(&character).unwrap();
    // Storage must preserve opaque native party bytes, knownness, and every other byte
    // it does not interpret. Only the adapter is entitled to understand this payload.
    let blob = (0..1024).map(|n| (n % 256) as u8).collect();
    store.put_progression(&Progression::new(account, GAME, blob, 3)).unwrap();
    (dir, store, account)
}

fn entry(before: &SceneState, id: u8) -> SceneState {
    let mut next = before.clone();
    next.field_visit = Some(visit(id));
    next.pair.story.blob[0] = 199;
    next.pair.story.updated_at = 4;
    let character = next.pair.character.as_mut().unwrap();
    character.gil -= 7;
    character.updated_at = 4;
    next
}

fn advance(before: &SceneState) -> SceneState {
    let mut next = before.clone();
    let visit = next.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.scratch_values[19] = 2;
    visit.scratch_known_masks[19] = 0xff;
    visit.anchor.push(3);
    next.pair.story.blob[1] = 197;
    next.pair.story.updated_at = 5;
    next.pair.character.as_mut().unwrap().exp += 23;
    next.pair.character.as_mut().unwrap().updated_at = 5;
    next
}

fn stamped(mut state: SceneState) -> SceneState {
    state.pair.character.as_mut().unwrap().schema_version = 4;
    state
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
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

fn pending(stage: u8) -> (tempfile::TempDir, VisitIntentFile) {
    let (dir, store, account) = rig();
    let before = store.get_scene_state(account, GAME).unwrap();
    store.commit_field_visit_with_hook(&operation(FieldVisitOperationKind::Enter, 11), &before, &entry(&before, 11),
        |at| if at == stage { Err(interrupted(at)) } else { Ok(()) }).unwrap_err();
    let file = serde_json::from_slice(&fs::read(dir.path().join(INTENT_FILE)).unwrap()).unwrap();
    drop(store);
    (dir, file)
}

fn write_intent(root: &Path, file: &VisitIntentFile) {
    fs::write(root.join(INTENT_FILE), serde_json::to_vec(file).unwrap()).unwrap();
}

fn refuses_without_repair(root: &Path) {
    let prior = files(root);
    assert!(Store::open(root).is_err());
    assert_eq!(files(root), prior, "a rejected recovery must not repair either record or remove its evidence");
    assert!(!root.join("LOCK").exists(), "failed open releases the lock");
}

#[test]
fn every_entry_and_advance_failure_boundary_recovers_exactly_once() {
    for advancing in [false, true] {
        for crash_at in 0..=5 {
            let (dir, store, account) = rig();
            let initial = store.get_scene_state(account, GAME).unwrap();
            let before = if advancing {
                store.commit_field_visit(&operation(FieldVisitOperationKind::Enter, 11), &initial, &entry(&initial, 11)).unwrap().current
            } else { initial };
            let (op, next) = if advancing {
                (operation(FieldVisitOperationKind::Advance, 12), advance(&before))
            } else { (operation(FieldVisitOperationKind::Enter, 11), entry(&before, 11)) };
            let original = files(dir.path());
            let prior_version = store.index().format_version();
            let mut observed = Vec::new();
            let result = store.commit_field_visit_with_hook(&op, &before, &next, |stage| {
                observed.push(stage);
                if stage == crash_at { Err(interrupted(stage)) } else { Ok(()) }
            });
            assert!(matches!(result, Err(PersistError::InvalidField { field: "injected_visit", .. })), "advance={advancing}, stage={crash_at}");
            assert_eq!(observed, (0..=crash_at).collect::<Vec<_>>());
            if crash_at == 0 {
                assert_eq!(store.index().format_version(), prior_version);
                assert_eq!(files(dir.path()), original, "pre-intent refusal changes no bytes");
                assert!(!store.transaction_poisoned.load(Ordering::Acquire));
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), before);
            } else {
                assert_eq!(store.index().format_version(), FIELD_VISIT_INDEX_VERSION);
                assert!(store.transaction_poisoned.load(Ordering::Acquire));
                assert!(matches!(store.get_scene_state(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.get_progress_pair(account, GAME), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_progression(&before.pair.story), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_character_progress(before.pair.character.as_ref().unwrap()), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.commit_field_visit(&op, &before, &next), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.snapshot(dir.path().join("blocked-snapshot")), Err(PersistError::TransactionRecoveryRequired)));
                assert_eq!(dir.path().join(INTENT_FILE).exists(), crash_at <= 3);
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            let expected = stamped(next.clone());
            assert_eq!(reopened.get_scene_state(account, GAME).unwrap(), if crash_at == 0 { before.clone() } else { expected.clone() });
            let saved = reopened.commit_field_visit(&op, &before, &next).unwrap();
            assert_eq!(saved.commit, if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(saved.current, expected);
            assert_eq!(&saved.current.pair.story.blob[2..], &before.pair.story.blob[2..], "opaque party/state bytes survive");
            assert_eq!(saved.current.pair.character.as_ref().unwrap().inventory, before.pair.character.as_ref().unwrap().inventory);
            assert_eq!(saved.current.pair.character.as_ref().unwrap().limit, 203);
            let durable = files(dir.path());
            assert_eq!(reopened.commit_field_visit(&op, &before, &next).unwrap().commit, SceneCommit::AlreadyApplied);
            assert_eq!(files(dir.path()), durable, "ack retry performs no record writes");
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }
}

#[test]
fn damaged_checksums_future_schemas_and_wrong_kinds_refuse_before_repair() {
    for corruption in ["checksum", "future", "kind"] {
        let (dir, mut file) = pending(2); // Story is new, character is old: real repair remains.
        match corruption {
            "checksum" => file.checksum = "0".repeat(64),
            "future" => file.schema_version = VISIT_INTENT_VERSION + 1,
            "kind" => file.kind = "not-a-field-visit".into(),
            _ => unreachable!(),
        }
        write_intent(dir.path(), &file);
        refuses_without_repair(dir.path());
    }
}

#[test]
fn an_unrelated_story_or_character_blocks_all_recovery_writes() {
    for changed_story in [false, true] {
        // Leave the other record at its before-image, so premature repair is observable.
        let (dir, file) = pending(if changed_story { 2 } else { 1 });
        let base = dir.path().join("progression").join(file.intent.account.0.to_string());
        if changed_story {
            let mut unrelated = file.intent.after.story.clone().unwrap();
            unrelated.blob.push(171);
            fsio::write_json(&base.join("Ff7.json"), &unrelated, MAX_RECORD_BYTES).unwrap();
        } else {
            let mut unrelated = file.intent.before.character.clone().unwrap();
            unrelated.progress.gil += 1;
            fsio::write_json(&base.join("Ff7.character.json"), &unrelated, MAX_CHARACTER_RECORD_BYTES).unwrap();
        }
        refuses_without_repair(dir.path());
    }
}

#[test]
fn a_correct_checksum_cannot_authorize_rewriting_prior_scene_receipts() {
    for mutation in ["remove", "replace", "add"] {
        let (dir, store, account) = rig();
        let original = store.get_progress_pair(account, GAME).unwrap();
        store.commit_scene_progress_with_result("fixture/already-paid", b"paid once", &original, &original).unwrap();
        let before = store.get_scene_state(account, GAME).unwrap();
        store.commit_field_visit_with_hook(&operation(FieldVisitOperationKind::Enter, 11), &before, &entry(&before, 11),
            |stage| if stage == 1 { Err(interrupted(stage)) } else { Ok(()) }).unwrap_err();
        drop(store);
        let path = dir.path().join(INTENT_FILE);
        let mut file: VisitIntentFile = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let mut changed = serde_json::to_value(file.intent.after.character.as_ref().unwrap()).unwrap();
        let receipts = changed["scene_receipts"].as_object_mut().unwrap();
        assert_eq!(receipts.len(), 1);
        let key = receipts.keys().next().unwrap().clone();
        match mutation {
            "remove" => { receipts.remove(&key); }
            "replace" => { receipts.insert(key, serde_json::json!("0".repeat(64))); }
            "add" => { receipts.insert("f".repeat(64), serde_json::json!("0".repeat(64))); }
            _ => unreachable!(),
        }
        let forged: CharacterRecord = serde_json::from_value(changed).unwrap();
        forged.validate().unwrap();
        file.intent.after.character = Some(forged);
        // SceneState omits private receipts, so both the public request digest and the
        // enclosing checksum remain valid. The metadata preservation check must catch this.
        let mut requested = file.intent.after.state(account, GAME);
        requested.pair.character.as_mut().unwrap().schema_version = file.intent.requested_character_schema;
        assert_eq!(request_digest(&file.intent.operation, &before, &requested).unwrap(), file.intent.request);
        file.checksum = digest(&file.intent).unwrap();
        write_intent(dir.path(), &file);
        refuses_without_repair(dir.path());
    }
}

#[test]
fn pending_visit_recovery_requires_index_five_before_installing_either_image() {
    for version in [2, 3, 4] {
        let (dir, _) = pending(2);
        let path = dir.path().join(INDEX_LOG_FILE);
        // Production compaction correctly forbids downgrades. Forge a valid older header
        // directly, retaining all operations and recomputing the header checksum.
        let bytes = fs::read(&path).unwrap();
        let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        let space = bytes[..end].iter().position(|byte| *byte == b' ').unwrap();
        let mut header: serde_json::Value = serde_json::from_slice(&bytes[space + 1..end]).unwrap();
        header["schema_version"] = version.into();
        let payload = serde_json::to_vec(&header).unwrap();
        let hash = blake3::hash(&payload).to_hex().to_string();
        let mut replaced = format!("{} ", &hash[..space]).into_bytes();
        replaced.extend_from_slice(&payload);
        replaced.extend_from_slice(&bytes[end..]);
        fs::write(path, replaced).unwrap();
        refuses_without_repair(dir.path());
    }
}

fn widest_visit() -> FieldVisitV1 {
    FieldVisitV1 {
        schema_version: 1, visit_id: [255; 16], revision: u64::MAX, field: "\\".repeat(MAX_FIELD_VISIT_FIELD_BYTES),
        source_identity: [255; 32], entry: FieldVisitEntryV1 { operation: [255; 16], arrival: vec![255; MAX_FIELD_VISIT_ENTRY_BYTES] },
        anchor: vec![255; MAX_FIELD_VISIT_ANCHOR_BYTES], scratch_values: [255; 256], scratch_known_masks: [255; 256],
        active_continuation: None,
    }
}

#[test]
fn largest_visit_and_full_receipt_images_fit_the_documented_independent_caps() {
    let (dir, store, account) = rig();
    let visit = widest_visit();
    visit.validate().unwrap();
    let mut linked = visit.clone();
    linked.active_continuation = Some([255; 32]);
    linked.validate().unwrap();
    let visit_bytes = serde_json::to_vec(&linked).unwrap();
    assert!(visit_bytes.len() > 10 * 1024, "fixture uses maximum-width opaque byte arrays");
    assert!(visit_bytes.len() <= MAX_FIELD_VISIT_BYTES);
    assert_eq!(serde_json::from_slice::<FieldVisitV1>(&visit_bytes).unwrap(), linked);

    let mut character = CharacterProgress::new(account, GAME, u8::MAX, u8::MAX, u64::MAX);
    character.exp = u32::MAX;
    character.ap = u32::MAX;
    character.gil = u32::MAX;
    character.limit = u16::MAX;
    character.inventory = Some((0..MAX_INVENTORY_STACKS).map(|n| InventoryEntry::new(u16::MAX - n as u16, u16::MAX)).collect());
    let mut value = serde_json::to_value(character).unwrap();
    // Full larger batch receipts bound a mixed scene/batch history. Visits replace their
    // own single receipt and therefore remain writable at the old receipt-history cap.
    let receipts: serde_json::Map<String, serde_json::Value> = (0..MAX_SCENE_RECEIPTS).map(|n| {
        (format!("{n:064x}"), serde_json::json!({"result": "f".repeat(64), "members": 16, "slot": 15}))
    }).collect();
    value["batch_receipts"] = serde_json::Value::Object(receipts);
    let mut record: CharacterRecord = serde_json::from_value(value).unwrap();
    let mut old_visit = visit.clone();
    old_visit.revision -= 1;
    record.field_visit = Some(old_visit);
    record.last_visit_commit = Some(VisitReceipt { operation: [1; 16], request: "f".repeat(64) });
    record.stamp();
    record.validate().unwrap();
    let story = Progression::new(account, GAME, vec![255; MAX_BLOB_BYTES], u64::MAX);
    let before = Images { story: Some(story.clone()), character: Some(record) };
    let mut after = before.clone();
    after.character.as_mut().unwrap().field_visit = Some(visit);
    let operation = operation(FieldVisitOperationKind::Advance, 254);
    let request = request_digest(&operation, &before.state(account, GAME), &after.state(account, GAME)).unwrap();
    after.character.as_mut().unwrap().last_visit_commit = Some(VisitReceipt { operation: operation.id, request: request.clone() });
    let requested_character_schema = after.character.as_ref().unwrap().progress.schema_version;
    let intent = VisitIntent { operation, account, game: GAME, requested_character_schema, request, before, after };
    store.validate_visit_intent(&intent).unwrap();
    for images in [&intent.before, &intent.after] {
        store.encode_visit_images(images, account, GAME).unwrap();
        let compact = serde_json::to_vec(images).unwrap();
        let pretty_character = encoded(Path::new("character-bound.json"), images.character.as_ref().unwrap(), MAX_CHARACTER_RECORD_BYTES).unwrap();
        let pretty_story = encoded(Path::new("story-bound.json"), images.story.as_ref().unwrap(), MAX_RECORD_BYTES).unwrap();
        assert!(compact.len() < pretty_character.len() + pretty_story.len());
        assert!(matches!(encoded(Path::new("small-bound.json"), images.character.as_ref().unwrap(), pretty_character.len() as u64 - 1), Err(PersistError::TooLarge { .. })));
    }
    let image_bytes = serde_json::to_vec(&intent.before).unwrap().len() + serde_json::to_vec(&intent.after).unwrap().len();
    let file = VisitIntentFile { schema_version: VISIT_INTENT_VERSION, kind: "fmv-field-visit-transaction".into(), checksum: digest(&intent).unwrap(), intent };
    let mut bytes = serde_json::to_vec(&file).unwrap();
    bytes.push(b'\n');
    assert!(bytes.len() - image_bytes < 2048, "intent/receipt framing fits its explicit allowance");
    let conservative_bound = 2 * (MAX_CHARACTER_RECORD_BYTES + MAX_RECORD_BYTES) + 2048;
    assert!(conservative_bound <= MAX_VISIT_INTENT_BYTES);
    assert!((bytes.len() as u64) < MAX_VISIT_INTENT_BYTES);
    let decoded: VisitIntentFile = serde_json::from_slice(&bytes).unwrap();
    store.validate_visit_intent(&decoded.intent).unwrap();
    assert_eq!(decoded.checksum, digest(&decoded.intent).unwrap());
    let prior = files(dir.path());
    let over = vec![b' '; MAX_VISIT_INTENT_BYTES as usize + 1];
    assert!(matches!(store.recover_field_visit(&dir.path().join(INTENT_FILE), &over), Err(PersistError::TooLarge { max: MAX_VISIT_INTENT_BYTES, .. })));
    assert_eq!(files(dir.path()), prior);
}

#[test]
fn raw_visit_limits_and_unknown_scratch_bits_refuse_before_opt_in() {
    let (dir, store, account) = rig();
    let before = store.get_scene_state(account, GAME).unwrap();
    let original = files(dir.path());
    for bad in ["field", "arrival", "anchor", "unknown", "revision", "identity", "story"] {
        let mut next = entry(&before, 11);
        let visit = next.field_visit.as_mut().unwrap();
        match bad {
            "field" => visit.field = "x".repeat(MAX_FIELD_VISIT_FIELD_BYTES + 1),
            "arrival" => visit.entry.arrival = vec![1; MAX_FIELD_VISIT_ENTRY_BYTES + 1],
            "anchor" => visit.anchor = vec![1; MAX_FIELD_VISIT_ANCHOR_BYTES + 1],
            "unknown" => visit.scratch_values[0] = 1,
            "revision" => visit.revision = 1,
            "identity" => visit.visit_id = [0; 16],
            "story" => next.pair.story.blob = vec![255; MAX_BLOB_BYTES + 1],
            _ => unreachable!(),
        }
        assert!(store.commit_field_visit(&operation(FieldVisitOperationKind::Enter, 11), &before, &next).is_err(), "{bad}");
        assert_eq!(files(dir.path()), original, "{bad}: no format fence or intent on invalid input");
        assert!(!store.transaction_poisoned.load(Ordering::Acquire));
    }
}

#[test]
fn checkpoint_links_are_exact_and_standalone_entries_reject_owner_and_guest_reservations() {
    let (dir, store, account) = rig();
    let (guest, _) = store.create_account("Guest", 2).unwrap();
    store.put_character_progress(&CharacterProgress::new(guest, GAME, 1, 6, 2)).unwrap();
    let before = store.get_scene_states(&[account, guest], GAME).unwrap();
    let instance = [33; 32];
    let checkpoint = SceneCheckpoint { instance, scene_key: "fixture/battle".into(), source_hash: [9; 32], replay_version: 1,
        original_story: before[0].pair.story.blob.clone(), original_gil: 240, seed: 1, replay: vec![1],
        phase: ScenePhase::BeforeBattle { ordinal: 0, attempt: 0, formation: 303, battle_seed: 99 }, consumed_battles: 0 };
    let mut prepared = before.clone();
    for (slot, state) in prepared.iter_mut().enumerate() {
        state.reservation = Some(SceneReservation { instance, ordinal: 0, attempt: 0, slot: slot as u16 });
    }
    prepared[0].checkpoint = Some(checkpoint.clone());
    let batch = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::PrepareBattle };
    let active = store.commit_scene_batch(&batch, b"prepared", &before, &prepared).unwrap().current;
    let original = files(dir.path());
    for state in &active {
        let mut next = state.clone();
        let mut visit = visit(11);
        visit.active_continuation = state.checkpoint.as_ref().map(|cp| cp.instance);
        next.field_visit = Some(visit);
        assert!(matches!(store.commit_field_visit(&operation(FieldVisitOperationKind::Enter, 11), state, &next), Err(PersistError::TransactionConflict(_))));
        assert_eq!(files(dir.path()), original);
    }
    let mut independent = visit(11);
    assert!(validate_link(Some(&independent), None, active[1].reservation.as_ref()).is_ok(), "a guest reservation is not its own checkpoint");
    assert!(validate_link(Some(&independent), Some(&checkpoint), None).is_err());
    independent.active_continuation = Some(instance);
    assert!(validate_link(Some(&independent), Some(&checkpoint), None).is_ok());
    assert!(validate_link(Some(&independent), None, None).is_err());
    independent.active_continuation = Some([34; 32]);
    assert!(validate_link(Some(&independent), Some(&checkpoint), None).is_err());
}
