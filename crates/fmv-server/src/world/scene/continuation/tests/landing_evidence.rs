//! The metadata reader preserves the original carrier; it never blesses game-script effects.
use super::*;

fn landed() -> (Rig, SceneState) {
    let mut rig = door_rig(YARD);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (job, done) = rig.pending_batch();
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    let state = rig.stored();
    assert!(matches!(state.checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    (rig, state)
}

fn change_payload(state: &mut SceneState, change: impl FnOnce(&mut Payload)) {
    let checkpoint = state.checkpoint.as_mut().unwrap();
    let mut payload: Payload = serde_json::from_slice(&checkpoint.replay).unwrap();
    change(&mut payload);
    checkpoint.replay = serde_json::to_vec(&payload).unwrap();
}

#[test]
fn saved_landing_evidence_preserves_original_origin_replay_and_exact_destination() {
    let (rig, mut state) = landed();
    // This metadata-only fixture records an explicit original crossing. The accessor
    // returns it unchanged; deciding whether it was accepted is the host's next check.
    let origin = fmv_protocol::SceneOrigin { pos: Pos::new(17.0, -21.0, 0.0), facing: 0.25, triangle: Some(4) };
    change_payload(&mut state, |payload| payload.replay.origin = Some(origin));
    let before = state.clone();
    let stored = rig.stored();
    let (replay, exit) = crate::world::saved_landing_evidence(&state, HALL).unwrap();
    assert_eq!(replay.source, DOOR_SOURCE);
    assert_eq!(replay.origin, Some(origin));
    assert!(replay.events.is_empty(), "the zero-window door acquired no invented ACK");
    assert_eq!(exit, DoorHost::landing());
    assert_eq!(state, before);
    assert_eq!(rig.stored(), stored, "reading evidence never changes the store or runtime");
    assert!(saved_landing_evidence(&state, YARD).is_none(), "source field is distinct from destination");
    assert!(saved_landing_evidence(&state, "").is_none());
}

#[test]
fn saved_landing_evidence_refuses_inconsistent_or_unsupported_metadata() {
    let (_rig, state) = landed();
    let rejected = |label: &str, changed: SceneState| {
        assert!(saved_landing_evidence(&changed, HALL).is_none(), "{label}");
    };
    let mut changed = state.clone(); changed.checkpoint = None; rejected("missing checkpoint", changed);
    let mut changed = state.clone(); changed.checkpoint.as_mut().unwrap().phase = ScenePhase::Resume { segment: 0 };
    rejected("not a landing", changed);
    for version in [0, 1, 2, 4] {
        let mut changed = state.clone(); changed.checkpoint.as_mut().unwrap().replay_version = version;
        rejected("another execution contract", changed);
    }
    for mutate in [
        (|checkpoint: &mut SceneCheckpoint| checkpoint.instance = [0; 32]) as fn(&mut SceneCheckpoint),
        |checkpoint| checkpoint.source_hash = [0; 32],
        |checkpoint| checkpoint.source_hash = [90; 32],
        |checkpoint| checkpoint.consumed_battles = 1,
        |checkpoint| checkpoint.scene_key.clear(),
        |checkpoint| checkpoint.scene_key.push('\n'),
        |checkpoint| checkpoint.original_story = vec![0; fmv_protocol::MAX_STORY_PROGRESS_BYTES + 1],
        |checkpoint| checkpoint.replay = b"{}".to_vec(),
        |checkpoint| checkpoint.replay = vec![0; fmv_persist::MAX_SCENE_REPLAY_BYTES + 1],
    ] {
        let mut changed = state.clone(); mutate(changed.checkpoint.as_mut().unwrap());
        rejected("invalid checkpoint metadata", changed);
    }
    for mutate in [
        (|payload: &mut Payload| payload.field = YARD.into()) as fn(&mut Payload),
        |payload| payload.field.push('\n'),
        |payload| payload.game_over = true,
        |payload| payload.carry = Some(SceneCombatCarry::default()),
        |payload| payload.landing = None,
        |payload| payload.landing.as_mut().unwrap().position.x += 1.0,
        |payload| payload.landing.as_mut().unwrap().facing = Some(0.5),
        |payload| payload.landing.as_mut().unwrap().hint = Some(28),
        |payload| payload.landing.as_mut().unwrap().field.clear(),
        |payload| payload.replay.source = [91; 32],
        |payload| payload.replay.events.push(SceneReplayEvent::Window { step: 1, identity: vec![1], answer: None }),
    ] {
        let mut changed = state.clone(); change_payload(&mut changed, mutate);
        rejected("inconsistent payload", changed);
    }
    let mut battle = Rig::new(3);
    battle.request_battle(); battle.snapshot();
    let (job, _done) = battle.pending_batch();
    let battle_payload: Payload = serde_json::from_slice(&job.members[0].checkpoint.as_ref().unwrap().replay).unwrap();
    assert!(battle_payload.plan.is_some());
    let mut changed = state.clone(); change_payload(&mut changed, |payload| payload.plan = battle_payload.plan);
    rejected("retained battle plan", changed);
    let mut changed = state.clone();
    changed.reservation = Some(SceneReservation { instance: [1; 32], ordinal: 0, attempt: 0, slot: 0 });
    rejected("reserved participant", changed);
    let mut changed = state.clone();
    changed.field_visit = Some(fmv_persist::FieldVisitV1 {
        schema_version: 1, visit_id: [1; 16], revision: 0, field: HALL.into(), source_identity: [2; 32],
        entry: fmv_persist::FieldVisitEntryV1 { operation: [3; 16], arrival: vec![1] }, anchor: vec![2],
        scratch_values: [0; 256], scratch_known_masks: [255; 256], active_continuation: None,
    });
    rejected("visit-bearing state", changed);
    let mut changed = state.clone();
    changed.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: b"{}".to_vec() };
    rejected("malformed destination", changed);

    // Values finite as f64 can still overflow the stored f32 coordinates. Test both
    // source and matching destination copies so equality alone cannot validate them.
    for (key, member) in [("position", Some("x")), ("facing", None)] {
        let mut changed = state.clone();
        let checkpoint = changed.checkpoint.as_mut().unwrap();
        let mut payload: serde_json::Value = serde_json::from_slice(&checkpoint.replay).unwrap();
        if let Some(member) = member { payload[key][member] = serde_json::json!(1e100); }
        else { payload[key] = serde_json::json!(1e100); }
        checkpoint.replay = serde_json::to_vec(&payload).unwrap();
        rejected("nonfinite source pose", changed);
    }
    let mut changed = state.clone();
    let checkpoint = changed.checkpoint.as_mut().unwrap();
    let mut payload: serde_json::Value = serde_json::from_slice(&checkpoint.replay).unwrap();
    payload["landing"]["position"]["x"] = serde_json::json!(1e100);
    checkpoint.phase = ScenePhase::Landing { destination: serde_json::to_vec(&payload["landing"]).unwrap() };
    checkpoint.replay = serde_json::to_vec(&payload).unwrap();
    rejected("nonfinite matching destination", changed);
}
