//! Bounds use the real private envelopes and the same pretty JSON plus newline as disk.
//! These tests only serialize in memory; their paths are diagnostic labels, never files.

use super::*;
use crate::store::scene::{Intent, IntentFile, INTENT_VERSION, MAX_INTENT_BYTES};
use fmv_core::MAX_INVENTORY_STACKS;

const SIZE_GAME: GameId = GameId::Fixture;
const SIZE_INSTANCE: [u8; 32] = [u8::MAX; 32];

fn widest_checkpoint(phase: ScenePhase, consumed_battles: u32) -> SceneCheckpoint {
    SceneCheckpoint {
        instance: SIZE_INSTANCE,
        // Escaping doubles these 256 valid non-control bytes in the encoded checkpoint.
        scene_key: "\\".repeat(256),
        source_hash: [u8::MAX; 32],
        replay_version: u32::MAX,
        original_story: vec![u8::MAX; MAX_INPUT_BYTES],
        original_gil: u32::MAX,
        seed: u32::MAX,
        replay: vec![u8::MAX; MAX_SCENE_REPLAY_BYTES],
        phase,
        consumed_battles,
    }
}

fn widest_character(account: AccountId, slot: u16, receipts: usize) -> CharacterRecord {
    let progress = CharacterProgress {
        schema_version: 3,
        account_id: account,
        game: SIZE_GAME,
        character: u8::MAX,
        level: u8::MAX,
        exp: u32::MAX,
        ap: u32::MAX,
        gil: u32::MAX,
        updated_at: u64::MAX,
        inventory: Some((0..MAX_INVENTORY_STACKS).map(|i| {
            InventoryEntry::new(u16::MAX - u16::try_from(i).unwrap(), u16::MAX)
        }).collect()),
        limit: u16::MAX,
    };
    CharacterRecord {
        progress,
        scene_receipts: Default::default(),
        batch_receipts: (0..receipts).map(|n| (format!("{:048x}{n:016x}", account.0), BatchReceipt {
            result: "f".repeat(64), members: MAX_SCENE_BATCH_ACCOUNTS as u16, slot,
        })).collect(),
        checkpoint: None,
        reservation: None,
        scene_roster: None,
        field_visit: None,
        last_visit_commit: None,
    }
}

fn widest_story(account: AccountId) -> Progression {
    Progression::new(account, SIZE_GAME, vec![u8::MAX; MAX_BLOB_BYTES], u64::MAX)
}

#[test]
fn maximum_checkpoint_and_receipt_history_fit_the_pretty_character_file() {
    let checkpoint = widest_checkpoint(ScenePhase::Landing { destination: vec![u8::MAX; MAX_INPUT_BYTES] }, MAX_BATTLES);
    validate_checkpoint(&checkpoint).unwrap();
    let compact_size = serde_json::to_vec(&checkpoint).unwrap().len();
    assert!(compact_size < MAX_SCENE_CHECKPOINT_BYTES, "{compact_size} compact checkpoint bytes");

    let mut record = widest_character(AccountId(u64::MAX), (MAX_SCENE_BATCH_ACCOUNTS - 1) as u16, MAX_SCENE_RECEIPTS);
    record.checkpoint = Some(checkpoint);
    record.validate().unwrap();
    let bytes = encoded(Path::new("character-size-bound.json"), &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
    assert!((bytes.len() as u64) < MAX_CHARACTER_RECORD_BYTES, "{} character bytes", bytes.len());
    let decoded: CharacterRecord = serde_json::from_slice(&bytes).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, record, "byte-array encoding and source identity survive the unchanged disk format");
    assert!(matches!(encoded(Path::new("character-size-bound.json"), &record, bytes.len() as u64 - 1),
        Err(PersistError::TooLarge { .. })), "the actual encoded cap is still enforced before writes");
    eprintln!("widest checkpoint: {compact_size} compact bytes; widest character: {} pretty bytes", bytes.len());

    record.checkpoint.as_mut().unwrap().replay.push(u8::MAX);
    assert!(serde_json::to_vec(record.checkpoint.as_ref().unwrap()).unwrap().len() < MAX_SCENE_CHECKPOINT_BYTES);
    assert!(validate_checkpoint(record.checkpoint.as_ref().unwrap()).is_err(), "the raw replay bound is independent of the envelope bound");
    assert!(record.validate().is_err(), "an enclosing record cannot bypass the raw replay bound");
}

#[test]
fn sixteen_full_before_and_after_images_fit_the_batch_intent_limit() {
    let operation = SceneOperation {
        instance: SIZE_INSTANCE, ordinal: MAX_BATTLES, attempt: 0, kind: SceneOperationKind::Finish,
    };
    let receipt = operation_key(&operation).unwrap();
    let result = "f".repeat(64);
    let roster: Vec<_> = (0..MAX_SCENE_BATCH_ACCOUNTS).map(|slot| RosterMember {
        account: AccountId(u64::MAX - (MAX_SCENE_BATCH_ACCOUNTS - 1 - slot) as u64), slot: slot as u16,
    }).collect();
    let rows: Vec<_> = roster.iter().enumerate().map(|(index, member)| {
        // Historical cohorts may have used slot 15 for every current account. Only this
        // operation's new receipt uses the current cohort's distinct slot numbers.
        let mut before = widest_character(member.account, (MAX_SCENE_BATCH_ACCOUNTS - 1) as u16, MAX_SCENE_RECEIPTS - 1);
        before.reservation = Some(SceneReservation {
            instance: operation.instance, ordinal: MAX_BATTLES - 1, attempt: MAX_ATTEMPTS, slot: member.slot,
        });
        before.scene_roster = Some(roster.clone());
        if index == 0 {
            before.checkpoint = Some(widest_checkpoint(ScenePhase::Resume { segment: MAX_BATTLES }, MAX_BATTLES));
        }
        before.validate().unwrap();
        let mut after = before.clone();
        assert!(after.batch_receipts.insert(receipt.clone(), BatchReceipt {
            result: result.clone(), members: MAX_SCENE_BATCH_ACCOUNTS as u16, slot: member.slot,
        }).is_none());
        after.reservation = None;
        after.scene_roster = None;
        if let Some(checkpoint) = &mut after.checkpoint {
            // The large destination adds more bytes than the released roster removes:
            // finishing is larger than a 16-member BeforeBattle -> Resume settlement.
            checkpoint.phase = ScenePhase::Landing { destination: vec![u8::MAX; MAX_INPUT_BYTES] };
        }
        after.validate().unwrap();
        let story = widest_story(member.account);
        BatchRow {
            account: member.account,
            game: SIZE_GAME,
            before: Images { story: Some(story.clone()), character: Some(before) },
            after: Images { story: Some(story), character: Some(after) },
        }
    }).collect();
    let before: Vec<_> = rows.iter().map(|row| row.before.state(row.account, row.game)).collect();
    let after: Vec<_> = rows.iter().map(|row| row.after.state(row.account, row.game)).collect();
    assert_eq!(validate_transition(&operation, &before, &after).unwrap(), Some(0));
    let intent = BatchIntent { operation, receipt, result, rows };
    let file = BatchIntentFile {
        schema_version: BATCH_INTENT_VERSION, kind: "fmv-scene-batch-transaction".into(), checksum: digest(&intent).unwrap(), intent,
    };
    let size = encoded(Path::new("batch-size-bound.json"), &file, MAX_BATCH_INTENT_BYTES).unwrap().len();
    assert!((size as u64) < MAX_BATCH_INTENT_BYTES, "{size} batch intent bytes");
    eprintln!("widest 16-member batch intent: {size} pretty bytes");
}

#[test]
fn a_receipt_full_inactive_character_still_fits_the_legacy_two_record_intent() {
    let account = AccountId(u64::MAX);
    // A new ordinary transaction needs one free slot. Batch receipts are the larger form,
    // so filling all other slots with them bounds mixed receipt histories too.
    let before = widest_character(account, (MAX_SCENE_BATCH_ACCOUNTS - 1) as u16, MAX_SCENE_RECEIPTS - 1);
    before.validate().unwrap();
    let mut after = before.clone();
    let receipt = "e".repeat(64);
    let result = "f".repeat(64);
    after.scene_receipts.insert(receipt.clone(), result.clone());
    after.validate().unwrap();
    let story = widest_story(account);
    let intent = Intent {
        account, game: SIZE_GAME, receipt, result,
        before: Images { story: Some(story.clone()), character: Some(before) },
        after: Images { story: Some(story), character: Some(after) },
    };
    let file = IntentFile {
        schema_version: INTENT_VERSION, kind: "fmv-scene-transaction".into(), checksum: digest(&intent).unwrap(), intent,
    };
    let size = encoded(Path::new("legacy-size-bound.json"), &file, MAX_INTENT_BYTES).unwrap().len();
    assert!((size as u64) < MAX_INTENT_BYTES, "{size} legacy intent bytes");
    eprintln!("widest inactive legacy intent: {size} pretty bytes");
}
