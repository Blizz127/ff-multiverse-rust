//! Exercise the real replay, combat-plan, checkpoint, saver and wire types together. Fully
//! known story banks must survive three battles without trimming their knownness masks.

use super::*;
use fmv_core::{Fingerprint, MAX_INVENTORY_STACKS};
use fmv_ff7::story::{StoryState, MAX_BANK_BLOB_BYTES};
use fmv_persist::{CharacterProgress, SceneCommit, MAX_SCENE_CHECKPOINT_BYTES, MAX_SCENE_REPLAY_BYTES};
use fmv_protocol::{BattleMember, BattleReward, BattleSetup};
use fmv_protocol::battle::{BattleInitialState, StatusSet};
use crate::saver::{SceneBattleSettlement, SceneBatchJob, SceneBatchMember};

const INSTANCE: [u8; 32] = [251; 32];

fn known_story(value: u8) -> Vec<u8> {
    let mut story = StoryState::at(39).into_extended().unwrap();
    for bank in [1, 3, 7, 11, 13] {
        for address in 0..=255 {
            if bank != 1 || address > 1 { story.write_byte(bank, address, value).unwrap(); }
        }
    }
    let bytes = story.encode();
    assert_eq!(bytes.len(), MAX_BANK_BLOB_BYTES);
    assert_eq!(bytes.len(), 5117);
    assert_eq!(StoryState::decode(&bytes).unwrap().encode(), bytes);
    bytes
}

fn plan(accounts: &[AccountId]) -> SceneBattlePlan {
    let initial = BattleInitialState { hp: 9999, mp: 999, status: StatusSet::EMPTY, limit: 255 };
    SceneBattlePlan {
        setup: BattleSetup {
            formation_id: 300, seed: u64::MAX,
            party: accounts.iter().enumerate().map(|(slot, account)|
                BattleMember::player(EntityId(0), *account, "W".repeat(64), slot as u8, 99, 255)).collect(),
            initial_party: Some(vec![initial; accounts.len()]), escape_policy: EscapePolicy::Forbidden,
            preemptive: false, back_attack: false, battle_speed: 255, ticks_per_second: 1000, victory_fanfare: true,
            game_over_on_defeat: true,
        },
        fingerprint: Fingerprint([255; 32]),
        inventories: accounts.iter().map(|account| (*account, (0..MAX_INVENTORY_STACKS)
            .map(|id| InventoryEntry::new(id as u16, 99)).collect())).collect(),
    }
}

fn payload(accounts: &[AccountId]) -> Payload {
    let plan = plan(accounts);
    let carry = SceneCombatCarry { members: plan.setup.party.iter().cloned()
        .zip(plan.setup.initial_party.as_ref().unwrap().iter().copied()).collect() };
    Payload { replay: SceneReplay { source: SOURCE, origin: None, events: vec![] },
        field: HALL.into(), position: Pos::default(), facing: 0.0, plan: Some(plan),
        carry: Some(carry), landing: None, game_over: false }
}

fn windows(payload: &mut Payload, count: u32) {
    let first = payload.replay.events.iter().filter(|event| matches!(event, SceneReplayEvent::Window { .. })).count() as u32;
    payload.replay.events.extend((first..first + count).map(|step| SceneReplayEvent::Window {
        step, identity: vec![255; 64], answer: Some(255),
    }));
}

fn checkpoint(original: &[u8], payload: &Payload, phase: ScenePhase, consumed_battles: u32) -> SceneCheckpoint {
    SceneCheckpoint { instance: INSTANCE, scene_key: SCENE.into(), source_hash: SOURCE,
        replay_version: REPLAY_VERSION, original_story: original.to_vec(), original_gil: 240, seed: u32::MAX,
        replay: serde_json::to_vec(payload).unwrap(), phase, consumed_battles }
}

fn frame(checkpoint: &SceneCheckpoint, payload: &Payload) -> Result<Vec<u8>, fmv_protocol::ProtoError> {
    fmv_protocol::encode(&ServerMsg::SceneStart { run: SceneRunId(u64::MAX), scene: checkpoint.scene_key.clone(),
        seed: checkpoint.seed, progress: checkpoint.original_story.clone(), gil: checkpoint.original_gil,
        replay: Some(payload.replay.clone()), owner: EntityId(u64::MAX), you: SceneRole::Watcher,
        participants: (0..fmv_persist::MAX_SCENE_BATCH_ACCOUNTS).map(|slot| SceneParticipant {
            entity: EntityId(u64::MAX - slot as u64), spot: slot as u8,
        }).collect() })
}

fn fits(checkpoint: &SceneCheckpoint, payload: &Payload) {
    payload.replay.validate().unwrap();
    assert!(checkpoint.replay.len() <= MAX_SCENE_REPLAY_BYTES);
    let encoded = serde_json::to_vec(checkpoint).unwrap();
    assert!(encoded.len() <= MAX_SCENE_CHECKPOINT_BYTES);
    let wire = frame(checkpoint, payload).unwrap();
    assert!(wire.len() <= fmv_protocol::MAX_FRAME as usize + 4);
    assert!(checkpoint_can_be_announced(checkpoint));
    let message: ServerMsg = fmv_protocol::decode(&wire[4..]).unwrap();
    assert!(matches!(message, ServerMsg::SceneStart { progress, replay: Some(replay), .. }
        if progress == checkpoint.original_story && replay == payload.replay));
}

#[test]
fn fully_known_story_survives_three_durable_battles_maximum_windows_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut store = Store::open(&path).unwrap();
    let original = known_story(255);
    let accounts = (0..3).map(|slot| {
        let (account, _) = store.create_account(&format!("Full banks {slot}"), 1).unwrap();
        store.put_progression(&Progression::new(account, GameId::Fixture, original.clone(), 1)).unwrap();
        let mut character = CharacterProgress::new(account, GameId::Fixture, slot, 5, 1);
        character.gil = 240;
        character.inventory = Some(vec![InventoryEntry::new(0, 3)]);
        store.put_character_progress(&character).unwrap();
        account
    }).collect::<Vec<_>>();
    let rules = SyntheticRules::default();
    let mut payload = payload(&accounts);
    let mut latest = original.clone();
    for ordinal in 0..3u32 {
        windows(&mut payload, if ordinal == 0 { 41 } else { 42 });
        latest = known_story(254 - ordinal as u8);
        let before = store.get_scene_states(&accounts, GameId::Fixture).unwrap();
        let prepared = checkpoint(&original, &payload, ScenePhase::BeforeBattle {
            ordinal, attempt: 0, formation: 300, battle_seed: u64::MAX,
        }, ordinal);
        fits(&prepared, &payload);
        let prepare = SceneBatchJob {
            operation: SceneOperation { instance: INSTANCE, ordinal, attempt: 0, kind: SceneOperationKind::PrepareBattle },
            members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember {
                expected: expected.clone(), next_story: (slot == 0).then(|| latest.clone()), next_gil: None,
                checkpoint: (slot == 0).then(|| prepared.clone()), battle: None,
                reservation: Some(SceneReservation { instance: INSTANCE, ordinal, attempt: 0, slot: slot as u16 }),
            }).collect(),
        };
        assert_eq!(persist_scene_batch(&store, GameId::Fixture, Some(&rules), &prepare, 10 + u64::from(ordinal) * 2).unwrap().commit, SceneCommit::Applied);
        drop(store);
        store = Store::open(&path).unwrap();
        let before = store.get_scene_states(&accounts, GameId::Fixture).unwrap();
        assert_eq!(before[0].checkpoint.as_ref(), Some(&prepared));
        assert_eq!(before[0].pair.story.blob, latest);
        assert_eq!(StoryState::decode(&before[0].pair.story.blob).unwrap().encode(), latest);
        let owner_gil = 250 + ordinal * 10;
        payload.replay.events.push(SceneReplayEvent::Battle { ordinal: ordinal + 1, identity: vec![255; 64],
            formation: 300, progress: latest.clone(), gil: owner_gil });
        let resumed = checkpoint(&original, &payload, ScenePhase::Resume { segment: ordinal + 1 }, ordinal + 1);
        fits(&resumed, &payload);
        let resolve = SceneBatchJob {
            operation: SceneOperation { kind: SceneOperationKind::ResolveBattle, ..prepare.operation },
            members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember {
                expected: expected.clone(), next_story: None, next_gil: None,
                checkpoint: (slot == 0).then(|| resumed.clone()), reservation: expected.reservation.clone(),
                battle: Some(SceneBattleSettlement { character: slot as u8, consumed_items: vec![], final_limit: None,
                    reward: BattleReward::new(accounts[slot], 0, 10, 0) }),
            }).collect(),
        };
        let saved = persist_scene_batch(&store, GameId::Fixture, Some(&rules), &resolve, 11 + u64::from(ordinal) * 2).unwrap();
        assert_eq!(saved.commit, SceneCommit::Applied);
        drop(store);
        store = Store::open(&path).unwrap();
        let duplicate = persist_scene_batch(&store, GameId::Fixture, Some(&rules), &resolve, 100).unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, saved.current, "receipt replay cannot award twice");
        for (slot, state) in saved.current.iter().enumerate() {
            assert_eq!(state.pair.character.as_ref().unwrap().gil, owner_gil);
            assert_eq!(state.pair.character.as_ref().unwrap().inventory, Some(vec![InventoryEntry::new(0, 3)]));
            assert_eq!(state.pair.story.blob, if slot == 0 { latest.clone() } else { original.clone() });
        }
        let decoded: Payload = serde_json::from_slice(&saved.current[0].checkpoint.as_ref().unwrap().replay).unwrap();
        assert_eq!(decoded.replay, payload.replay);
        for event in &decoded.replay.events {
            if let SceneReplayEvent::Battle { progress, .. } = event {
                assert_eq!(progress.len(), MAX_BANK_BLOB_BYTES);
                assert_eq!(StoryState::decode(progress).unwrap().encode(), *progress);
            }
        }
    }
    assert_eq!(payload.replay.events.len(), fmv_protocol::MAX_SCENE_REPLAY_EVENTS);
    let before = store.get_scene_states(&accounts, GameId::Fixture).unwrap();
    let largest = before[0].checkpoint.as_ref().unwrap();
    eprintln!("full-known three-battle/125-window checkpoint: payload={}, compact={}, frame={}",
        largest.replay.len(), serde_json::to_vec(largest).unwrap().len(), frame(largest, &payload).unwrap().len());
    assert!(largest.replay.len() > 32 * 1024 && serde_json::to_vec(largest).unwrap().len() > 64 * 1024,
        "this fixture must catch both former limits");
    let landing = Landing { field: YARD.into(), position: Pos::default(), facing: Some(0.0), hint: None };
    payload.plan = None;
    payload.carry = None;
    payload.landing = Some(landing.clone());
    let landed = checkpoint(&original, &payload, ScenePhase::Landing { destination: serde_json::to_vec(&landing).unwrap() }, 3);
    fits(&landed, &payload);
    let finish = SceneBatchJob { operation: SceneOperation { instance: INSTANCE, ordinal: 3, attempt: 0, kind: SceneOperationKind::Finish },
        members: before.iter().enumerate().map(|(slot, expected)| SceneBatchMember { expected: expected.clone(),
            next_story: None, next_gil: None, checkpoint: (slot == 0).then(|| landed.clone()), reservation: None, battle: None }).collect() };
    persist_scene_batch(&store, GameId::Fixture, None, &finish, 20).unwrap();
    drop(store);
    store = Store::open(&path).unwrap();
    let owner = store.get_scene_group(accounts[0], GameId::Fixture).unwrap();
    assert_eq!(owner.len(), 1);
    assert_eq!(owner[0].checkpoint.as_ref(), Some(&landed));
    assert_eq!(owner[0].pair.story.blob, latest);
    for account in &accounts[1..] {
        let state = store.get_scene_state(*account, GameId::Fixture).unwrap();
        assert!(state.reservation.is_none() && state.checkpoint.is_none());
    }
}

#[test]
fn three_maximum_protocol_snapshots_and_125_windows_fit_the_existing_wire_frame() {
    let original = vec![255; fmv_protocol::MAX_STORY_PROGRESS_BYTES];
    let mut payload = payload(&[AccountId(u64::MAX - 2), AccountId(u64::MAX - 1), AccountId(u64::MAX)]);
    windows(&mut payload, 125);
    for ordinal in 1..=3 {
        payload.replay.events.push(SceneReplayEvent::Battle { ordinal, identity: vec![255; 64], formation: u16::MAX,
            progress: original.clone(), gil: u32::MAX });
    }
    let checkpoint = checkpoint(&original, &payload, ScenePhase::Resume { segment: 3 }, 3);
    fits(&checkpoint, &payload);
    eprintln!("protocol upper-bound three-battle fixture: payload={}, compact={}, frame={}",
        checkpoint.replay.len(), serde_json::to_vec(&checkpoint).unwrap().len(), frame(&checkpoint, &payload).unwrap().len());
}

#[test]
fn a_raw_payload_that_fits_storage_but_not_the_wire_cannot_be_queued_or_admitted() {
    let mut rig = Rig::new(1);
    let mut oversized = payload(&[rig.owner.account]);
    let original = vec![255; fmv_protocol::MAX_STORY_PROGRESS_BYTES];
    for ordinal in 1..=7 {
        oversized.replay.events.push(SceneReplayEvent::Battle { ordinal, identity: vec![255; 64], formation: 1,
            progress: original.clone(), gil: 240 });
    }
    let bad = checkpoint(&original, &oversized, ScenePhase::Resume { segment: 7 }, 7);
    oversized.replay.validate().unwrap();
    assert!(bad.replay.len() < MAX_SCENE_REPLAY_BYTES);
    assert!(serde_json::to_vec(&bad).unwrap().len() < MAX_SCENE_CHECKPOINT_BYTES);
    assert!(frame(&bad, &oversized).is_err());
    assert!(!checkpoint_can_be_announced(&bad));

    let mut c = rig.world.scenes.continuations.remove(&rig.run).unwrap();
    c.states = vec![rig.stored()];
    c.payload = oversized;
    let mut record = rig.world.scenes.runs[&rig.run].clone();
    record.progress = original;
    let action = Action::Prepare { plan: plan(&[rig.owner.account]), ordinal: 1 };
    assert!(rig.world.continuation_job(&c, &record, &action).is_none(), "unannouncable input never becomes a saver job");
    assert!(rig.saves.try_recv().is_err());
    assert!(rig.stored().checkpoint.is_none());
    let mut saved = rig.stored();
    saved.checkpoint = Some(bad);
    struct NoReplayBeforeWireCheck;
    impl SceneHost for NoReplayBeforeWireCheck {
        fn scenes(&self) -> Vec<SceneInfo> { Host { formation: 1, automatic: false }.scenes() }
        fn offered(&self, _: &str, _: &[u8]) -> bool { true }
        fn continuation(&self, _: &str) -> bool { true }
        fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
        fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
        fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { None }
        fn evaluate_continuation(&self, _: &str, _: &[u8], _: u32, _: u32, _: Option<&SceneReplay>) -> Option<SceneBoundary> {
            panic!("an oversized reconnect frame must be rejected before source replay");
        }
    }
    rig.world.scenes.host = Some(Arc::new(NoReplayBeforeWireCheck));
    assert!(matches!(rig.world.restore_continuation(rig.owner.entity, vec![saved], rig.now),
        Err(crate::world::JoinError::StorageUnavailable)));
    assert!(!rig.world.scenes.continuations.contains_key(&rig.run));
}
