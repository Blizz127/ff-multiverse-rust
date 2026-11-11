//! Actual cargo source and checked train boundaries through the ordered saver. The
//! temporary stores below establish durable entry provenance; no live visit is admitted.
//! A complete presentation must come from a separately captured client, never an
//! interpreter-derived no-dwell picture manufactured by these server tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmv_core::{AccountId, GameId, InventoryEntry, Pos};
use fmv_ff7::script::scene::{cargo_door_scene, train_scene, train_arrival_ready, SceneInput,
    ScenePlay, Stage, Then, TrainArrival, CARGO_DOOR_SCENE_ID, TRAIN_SCENE_ID, FRAMES_PER_SECOND};
use fmv_ff7::script::scene::presentation::{self as reducer, Cast};
use fmv_ff7::script::scene::train_arrival::codec::{encode_train_arrival, replay_train_arrival_record};
use fmv_ff7::script::scene::train_arrival::presentation::{validate_train_presentation, TrainPresentationCheckpoint, TrainPresentationEntry};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::field::FieldFile;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use fmv_persist::{CharacterProgress, Progression, SceneCheckpoint, SceneCommit, SceneOperation,
    SceneOperationKind, ScenePhase, SceneState, Store};
use fmv_protocol::{BattleReward, SceneOrigin, SceneReplay, SceneReplayEvent, SyntheticRules};

use super::{ArrivalBoundary, CheckedTrainArrivalSource};
use crate::ff7::{open_ff7, Ff7Labels, Ff7Source, SharedFf7};
use crate::saver::{SaveJob, Saver, SaverHandle, SceneBatchJob, SceneBatchMember};
use crate::scene_host::Ff7SceneHost;
use crate::world::{SceneBoundaryKind, SceneHost};

const GAME: GameId = GameId::Ff7;
const NOW: u64 = 1_000;
const CARGO_INSTANCE: [u8; 32] = [41; 32];
const ARRIVAL_INSTANCE: [u8; 32] = [42; 32];
const SEED: u32 = 7;
const INITIAL_SHAKE_RNG: [u8; 2] = [17, 29];

fn owned_adapter() -> Arc<Ff7Adapter> {
    let path = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned FF7 data is required"))
            .join("field/flevel.lgp")
    });
    open_ff7(&Ff7Source::Flevel { path, labels: Ff7Labels::default() }).unwrap().adapter
}

fn host(adapter: &Arc<Ff7Adapter>) -> Ff7SceneHost {
    let mut host = Ff7SceneHost::with_extended_save(Arc::clone(adapter), true);
    host.defs.retain(|def| ![CARGO_DOOR_SCENE_ID, TRAIN_SCENE_ID].contains(&def.id.as_str()));
    host.defs.extend([cargo_door_scene(), train_scene()]);
    host
}

fn original_story() -> StoryState {
    let mut story = StoryState::fresh_first_control();
    story.moment = 48;
    story.write_masked(3, 66, 1, 1).unwrap();
    story.write_masked(13, 101, 128, 128).unwrap();
    story
}

fn fixture() -> (tempfile::TempDir, Arc<Store>, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let (account, _) = store.create_account("Checked train entry", NOW).unwrap();
    store.put_progression(&Progression::new(account, GAME, original_story().encode(), NOW)).unwrap();
    store.put_character_progress(&CharacterProgress { exp: 40, ap: 7, gil: 240, limit: 137,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 2)]),
        ..CharacterProgress::new(account, GAME, 0, 5, NOW) }).unwrap();
    (dir, store, account)
}

fn saver(store: &Arc<Store>, adapter: &Arc<Ff7Adapter>) -> Saver {
    Saver::spawn(Arc::clone(store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(SharedFf7(Arc::clone(adapter))))
}

async fn snapshot(handle: &SaverHandle, account: AccountId) -> SceneState {
    let mut states = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap();
    assert_eq!(states.len(), 1);
    states.pop().unwrap()
}

async fn save(handle: &SaverHandle, job: SceneBatchJob) -> SceneState {
    let saved = handle.save_scene_batch(job).unwrap().await.unwrap().unwrap();
    assert_eq!(saved.commit, SceneCommit::Applied);
    assert_eq!(saved.current.len(), 1);
    saved.current.into_iter().next().unwrap()
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

/// The existing runtime's legacy checkpoint encoding is only a fixture input here. Its
/// source, crossing and MAPJUMP are obtained by executing the real cargo door, not selected
/// to bypass the new arrival constructor. The production constructor reads it through the
/// runtime-owned evidence accessor and independently replays its source.
fn cargo_finish(host: &Ff7SceneHost, before: &SceneState) -> SceneBatchJob {
    let progress = &before.pair.story.blob;
    let gil = before.pair.character.as_ref().unwrap().gil;
    let origin = SceneOrigin { pos: Pos::new(9.0, -105.0, 0.0),
        facing: fmv_ff7::field_direction_to_facing(0), triangle: None };
    assert_eq!(host.line_scene("cargoin", progress, Pos::new(9.0, -103.0, 0.0), origin.pos),
        Some(CARGO_DOOR_SCENE_ID.to_string()));
    let initial = host.initial_continuation(CARGO_DOOR_SCENE_ID, progress, SEED, gil, Some(origin)).unwrap();
    let replay = SceneReplay { source: initial.source, origin: Some(origin), events: vec![] };
    let boundary = host.evaluate_continuation(CARGO_DOOR_SCENE_ID, progress, SEED, gil, Some(&replay)).unwrap();
    let SceneBoundaryKind::Complete(result) = boundary.at else { panic!("the cargo door has no dialogue or combat") };
    let exit = result.travel.unwrap();
    assert_eq!(exit.target, "tin_1");
    assert_eq!(exit.arrival, Pos::new(-2.0, 608.0, 0.0));
    assert_eq!(exit.arrival_hint, Some(29));
    assert_eq!(exit.arrival_facing, Some(fmv_ff7::field_direction_to_facing(252)));
    let landing = serde_json::json!({"field": exit.target, "position": exit.arrival,
        "facing": exit.arrival_facing, "hint": exit.arrival_hint});
    let payload = serde_json::json!({"replay": replay, "field": "cargoin", "position": origin.pos,
        "facing": origin.facing, "plan": null, "carry": null, "landing": landing, "game_over": false});
    SceneBatchJob {
        operation: SceneOperation { instance: CARGO_INSTANCE, ordinal: 0, attempt: 0, kind: SceneOperationKind::Finish },
        members: vec![SceneBatchMember {
            expected: before.clone(), next_story: Some(result.progress), next_gil: Some(boundary.gil),
            checkpoint: Some(SceneCheckpoint { instance: CARGO_INSTANCE, scene_key: CARGO_DOOR_SCENE_ID.into(),
                source_hash: initial.source, replay_version: 3, original_story: progress.clone(), original_gil: gil,
                seed: SEED, replay: serde_json::to_vec(&payload).unwrap(),
                phase: ScenePhase::Landing { destination: serde_json::to_vec(&landing).unwrap() }, consumed_battles: 0 }),
            reservation: None, battle: None, next_visit: None,
        }],
    }
}

fn acknowledgment(source: &CheckedTrainArrivalSource<'_>, host: &Ff7SceneHost, state: &SceneState,
    expected_step: u32) -> SceneReplayEvent {
    let ArrivalBoundary::Window { step, identity } = source.boundary(host, state).unwrap()
        else { panic!("the next train window must await its durable ACK") };
    assert_eq!(step, expected_step);
    assert!(!identity.is_empty());
    SceneReplayEvent::Window { step, identity, answer: None }
}

fn unchanged_until_complete(landing: &SceneState, current: &SceneState) {
    assert_eq!(current.pair, landing.pair, "accepted ACKs cannot publish moment 51 or replace character resources");
    assert!(current.field_visit.is_none() && current.reservation.is_none());
    let checkpoint = current.checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.instance, ARRIVAL_INSTANCE);
    assert_eq!(checkpoint.scene_key, TRAIN_SCENE_ID);
    assert_eq!(checkpoint.original_story, landing.pair.story.blob);
    assert_eq!(checkpoint.original_gil, 240);
    assert_eq!(checkpoint.seed, SEED);
    assert_eq!(checkpoint.phase, ScenePhase::Resume { segment: 0 });
    assert_eq!(checkpoint.consumed_battles, 0);
}

/// Capture an explicit simulated playback, using the same reducer as the client. Actor
/// time advances through the actual source events; while each window is held only effects
/// advance. These measured reducer phases are serialized, never inferred from window count
/// by the completion path or substituted for a missing historical anchor.
fn captured_presentation(field: &FieldFile, entry_record: &[u8], initial_rng: [u8; 2]) -> Vec<u8> {
    let verified = replay_train_arrival_record(field, entry_record).unwrap();
    let entry = verified.train_entry().unwrap();
    let mut play = TrainArrival::new(field, entry.original_story(), verified.replay_identity().seed,
        entry.resources(), entry.landing()).unwrap().into_play();
    let mut cast = Cast::new(play.def().cast.iter().map(|member| member.entity));
    cast.seed_shake_rng(initial_rng);
    for member in &play.def().cast {
        if let Some((slot, clip)) = play.initial_default_animation_clip(member.entity) {
            cast.remember_default(member.entity, slot, clip);
        }
    }
    let apply = |cast: &mut Cast, play: &ScenePlay, event: &Stage| {
        let clip = reducer::animation_slot(event).and_then(|(entity, slot)| play.animation_clip(entity, slot));
        reducer::apply_stage(cast, event, 0.0, 0.0, clip,
            &|x, y, triangle| play.ground().height(x, y, triangle, None));
    };
    for event in play.initial_stage_events() { apply(&mut cast, &play, event); }
    assert!(!cast.busy());
    let mut answer = None;
    let mut elapsed_frames = 0.0;
    let mut effects_seen = BTreeSet::new();
    for step in 0..=2 {
        let beat = play.next_strict(answer).unwrap();
        let end_at = match &beat.then { Then::Window(window) => window.at, Then::End { at, .. } => *at };
        let mut clock = 0;
        for event in &beat.stage {
            assert!(event.at >= clock && event.at <= end_at);
            let delta = (event.at - clock) as f32;
            cast.advance(delta);
            elapsed_frames += delta;
            clock = event.at;
            apply(&mut cast, &play, &event.what);
        }
        let delta = (end_at - clock) as f32;
        cast.advance(delta);
        elapsed_frames += delta;
        match beat.then {
            Then::Window(_) => {
                let SceneInput::Window(ack) = &verified.transcript()[step] else { panic!("two real windows only") };
                let before_clock = cast.camera_clock();
                for tick in 0..[113, 197][step] {
                    let dt = [0.0173, 0.0117, 0.0391, 0.0256][tick % 4];
                    cast.advance_effects(dt * FRAMES_PER_SECOND as f32);
                    effects_seen.insert(cast.shake().map(f32::to_bits));
                }
                assert_eq!(cast.camera_clock(), before_clock, "dialogue dwell holds the actor/camera clock");
                answer = Some(*ack);
            }
            Then::End { how: End::Finished, .. } if step == 2 => {},
            other => panic!("unexpected train boundary: {other:?}"),
        }
    }
    assert!(effects_seen.len() > 1, "the capture includes actual effect evolution during both holds");
    train_arrival_ready(&play).unwrap();
    assert_eq!(encode_train_arrival(&play).unwrap(), entry_record);
    for dt in [0.137, 0.0517, 0.4023] { cast.advance_effects(dt * FRAMES_PER_SECOND as f32); }
    assert!(!cast.busy());
    let counts = cast.counts();
    assert_eq!((counts.anims_skipped, counts.moves_skipped, counts.off_cast), (0, 0, 0));
    let encoded = TrainPresentationCheckpoint::new(entry_record, initial_rng, elapsed_frames, cast.checkpoint().unwrap())
        .unwrap().encode();
    let validated = validate_train_presentation(field, entry_record, initial_rng, &encoded).unwrap();
    assert_eq!(validated.bytes(), encoded);
    encoded
}

#[tokio::test]
#[ignore = "needs original FF7 data; checked train checkpoints are tested only in throwaway stores"]
async fn owned_train_arrival_retains_each_exact_ack_across_store_restarts_before_any_effect_is_published() {
    let adapter = owned_adapter();
    let host = host(&adapter);
    let area = adapter.load_field("tin_1").unwrap();
    let source = CheckedTrainArrivalSource::new(area.field_file()).unwrap();
    let (dir, store, account) = fixture();
    let queue = saver(&store, &adapter);
    let handle = queue.handle();
    let before = snapshot(&handle, account).await;
    let landing = save(&handle, cargo_finish(&host, &before)).await;
    let durable = files(dir.path());
    for kind in ["missing-checkpoint", "scene", "version", "source", "destination", "original-story", "original-gil",
        "current-story", "current-gil", "missing-character", "account"] {
        let mut altered = landing.clone();
        match kind {
            "missing-checkpoint" => altered.checkpoint = None,
            "scene" => altered.checkpoint.as_mut().unwrap().scene_key = TRAIN_SCENE_ID.into(),
            "version" => altered.checkpoint.as_mut().unwrap().replay_version += 1,
            "source" => altered.checkpoint.as_mut().unwrap().source_hash[0] ^= 1,
            "destination" => altered.checkpoint.as_mut().unwrap().phase = ScenePhase::Landing { destination: vec![1, 2] },
            "original-story" => altered.checkpoint.as_mut().unwrap().original_story = StoryState::at(47).into_extended().unwrap().encode(),
            "original-gil" => altered.checkpoint.as_mut().unwrap().original_gil += 1,
            "current-story" => altered.pair.story.blob = StoryState::at(51).into_extended().unwrap().encode(),
            "current-gil" => altered.pair.character.as_mut().unwrap().gil += 1,
            "missing-character" => altered.pair.character = None,
            "account" => altered.pair.character.as_mut().unwrap().account_id = AccountId(account.0 + 1),
            _ => unreachable!(),
        }
        assert!(source.begin_from_landing(&host, &altered, ARRIVAL_INSTANCE, SEED, INITIAL_SHAKE_RNG).is_err(), "changed {kind}");
    }
    for invalid_instance in [[0; 32], CARGO_INSTANCE] {
        assert!(source.begin_from_landing(&host, &landing, invalid_instance, SEED, INITIAL_SHAKE_RNG).is_err());
    }
    let mut wrong_origin = landing.clone();
    let checkpoint = wrong_origin.checkpoint.as_mut().unwrap();
    let mut payload: serde_json::Value = serde_json::from_slice(&checkpoint.replay).unwrap();
    payload["replay"]["origin"]["facing"] = serde_json::json!(0.5);
    checkpoint.replay = serde_json::to_vec(&payload).unwrap();
    assert!(source.begin_from_landing(&host, &wrong_origin, ARRIVAL_INSTANCE, SEED, INITIAL_SHAKE_RNG).is_err(),
        "the saved origin is part of the executed cargo source");
    assert_eq!(files(dir.path()), durable, "invalid provenance cannot enqueue a decision");
    let begin = source.begin_from_landing(&host, &landing, ARRIVAL_INSTANCE, SEED, INITIAL_SHAKE_RNG).unwrap();
    assert_eq!(begin.operation.kind, SceneOperationKind::BeginArrival);
    assert_eq!(begin.operation.instance, CARGO_INSTANCE);
    assert_eq!(source.begin_from_landing(&host, &landing, ARRIVAL_INSTANCE, SEED, INITIAL_SHAKE_RNG).unwrap(), begin,
        "same durable landing and new identity select the same immutable decision");
    let mut retry_jobs = vec![begin.clone()];
    let attached = save(&handle, begin).await;
    unchanged_until_complete(&landing, &attached);
    for kind in ["seed", "instance", "source", "version", "legacy-replay", "original-story", "original-gil", "current-story", "current-gil"] {
        let mut altered = attached.clone();
        match kind {
            "seed" => altered.checkpoint.as_mut().unwrap().seed ^= 1,
            "instance" => altered.checkpoint.as_mut().unwrap().instance[0] ^= 1,
            "source" => altered.checkpoint.as_mut().unwrap().source_hash[0] ^= 1,
            "version" => altered.checkpoint.as_mut().unwrap().replay_version += 1,
            "legacy-replay" => altered.checkpoint.as_mut().unwrap().replay = landing.checkpoint.as_ref().unwrap().replay.clone(),
            "original-story" => altered.checkpoint.as_mut().unwrap().original_story = StoryState::at(51).into_extended().unwrap().encode(),
            "original-gil" => altered.checkpoint.as_mut().unwrap().original_gil += 1,
            "current-story" => altered.pair.story.blob = StoryState::at(51).into_extended().unwrap().encode(),
            "current-gil" => altered.pair.character.as_mut().unwrap().gil += 1,
            _ => unreachable!(),
        }
        assert!(source.boundary(&host, &altered).is_err(), "changed {kind} cannot reconstruct checked input");
    }
    assert!(source.complete(&host, &attached).is_err(), "no completion before either ACK");
    queue.shutdown(Duration::from_secs(20)).await;
    drop(handle); drop(queue); drop(store);

    let mut previous = attached;
    let mut first_ack = None;
    for step in 0..2 {
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        assert_eq!(store.get_scene_state(account, GAME).unwrap(), previous);
        let queue = saver(&store, &adapter);
        let handle = queue.handle();
        let current = snapshot(&handle, account).await;
        unchanged_until_complete(&landing, &current);
        let ack = acknowledgment(&source, &host, &current, step);
        if step == 0 { first_ack = Some(ack.clone()); }
        else {
            assert!(source.acknowledge(&host, &current, first_ack.clone().unwrap()).is_err(),
                "a previously persisted ACK cannot consume the next window");
        }
        let mut wrong = ack.clone();
        if let SceneReplayEvent::Window { identity, .. } = &mut wrong { identity.push(0); }
        let mut answered = ack.clone();
        if let SceneReplayEvent::Window { answer, .. } = &mut answered { *answer = Some(0); }
        let durable = files(dir.path());
        for invalid in [wrong, answered] { assert!(source.acknowledge(&host, &current, invalid).is_err()); }
        assert_eq!(files(dir.path()), durable);
        let job = source.acknowledge(&host, &current, ack).unwrap();
        assert_eq!(job.operation.kind, SceneOperationKind::AdvanceArrival);
        assert_eq!((job.operation.ordinal, job.operation.attempt), (step + 1, 0));
        assert!(job.members.iter().all(|member| member.next_story.is_none() && member.next_gil.is_none()
            && member.next_visit.is_none()));
        retry_jobs.push(job.clone());
        previous = save(&handle, job).await;
        unchanged_until_complete(&landing, &previous);
        queue.shutdown(Duration::from_secs(20)).await;
        drop(handle); drop(queue); drop(store);
    }

    // Restart after the final durable ACK. Finished is reconstructed without resending it;
    // the pair still contains 48 until a separately validated presentation can complete it.
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let finished = store.get_scene_state(account, GAME).unwrap();
    assert_eq!(finished, previous);
    unchanged_until_complete(&landing, &finished);
    assert_eq!(source.boundary(&host, &finished).unwrap(), ArrivalBoundary::AwaitingPresentation);
    assert!(source.acknowledge(&host, &finished, first_ack.unwrap()).is_err());
    let durable = files(dir.path());
    assert!(source.complete(&host, &finished).is_err(),
        "a complete gameplay prefix cannot manufacture retained client clocks");
    let (entry_record, rng) = source.terminal_entry(&host, &finished).unwrap();
    assert_eq!(rng, INITIAL_SHAKE_RNG);
    for anchor in [&[][..], &[1, 2, 3][..]] {
        assert!(validate_train_presentation(area.field_file(), &entry_record, rng, anchor).is_err());
    }
    assert_eq!(files(dir.path()), durable);

    let anchor = captured_presentation(area.field_file(), &entry_record, rng);
    let validated = validate_train_presentation(area.field_file(), &entry_record, rng, &anchor).unwrap();
    let queue = saver(&store, &adapter);
    let handle = queue.handle();
    let seal = source.seal_job(&host, &finished, &validated).unwrap();
    assert_eq!((seal.operation.kind, seal.operation.ordinal), (SceneOperationKind::AdvanceArrival, 3));
    assert_eq!(source.seal_job(&host, &finished, &validated).unwrap(), seal);
    retry_jobs.push(seal.clone());
    let sealed = save(&handle, seal).await;
    unchanged_until_complete(&landing, &sealed);
    assert_eq!(source.boundary(&host, &sealed).unwrap(), ArrivalBoundary::Complete);
    assert!(source.seal_job(&host, &sealed, &validated).is_err(), "a sealed anchor cannot be replaced");
    queue.shutdown(Duration::from_secs(20)).await;
    drop(handle); drop(queue); drop(store);

    // The presentation seal survives its own interruption boundary. Complete has no new
    // input parameter and therefore cannot choose fresh clocks or an alternative transcript.
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    assert_eq!(store.get_scene_state(account, GAME).unwrap(), sealed);
    let queue = saver(&store, &adapter);
    let handle = queue.handle();
    let restored = snapshot(&handle, account).await;
    assert_eq!(source.boundary(&host, &restored).unwrap(), ArrivalBoundary::Complete);
    let completion = source.complete(&host, &restored).unwrap();
    assert_eq!((completion.pose.x, completion.pose.y, completion.pose.z, completion.pose.triangle, completion.pose.direction),
        (2, 537, -53, 28, 0), "the incoming MAPJUMP is not the final control position");
    assert_eq!(completion.job.operation.kind, SceneOperationKind::CompleteArrivalVisit);
    assert_eq!(source.complete(&host, &restored).unwrap().job, completion.job);
    retry_jobs.push(completion.job.clone());
    let completed = save(&handle, completion.job).await;
    assert!(completed.checkpoint.is_none() && completed.reservation.is_none());
    let mut after_story = original_story();
    after_story.moment = 51;
    assert_eq!(completed.pair.story.blob, after_story.encode());
    let mut expected_character = landing.pair.character.clone().unwrap();
    expected_character.updated_at = completed.pair.character.as_ref().unwrap().updated_at;
    expected_character.schema_version = completed.pair.character.as_ref().unwrap().schema_version;
    assert_eq!(completed.pair.character, Some(expected_character), "arrival changes no inventory, gil, limits or experience");
    let visit = completed.field_visit.as_ref().unwrap();
    assert_eq!(visit.revision, 0);
    assert_eq!(visit.field, "tin_1");
    assert_eq!(visit.source_identity, source.idle_source().identity());
    let retained_entry = TrainPresentationEntry::decode(&visit.entry.arrival).unwrap();
    assert_eq!(retained_entry.record(), entry_record);
    assert_eq!(retained_entry.initial_rng(), INITIAL_SHAKE_RNG);
    assert_eq!(retained_entry.encode(), visit.entry.arrival);
    let wrong_rng = [INITIAL_SHAKE_RNG[0] ^ 1, INITIAL_SHAKE_RNG[1]];
    let changed_envelope = TrainPresentationEntry::new(retained_entry.record(), wrong_rng).unwrap().encode();
    let changed_entry = TrainPresentationEntry::decode(&changed_envelope).unwrap();
    assert_eq!(changed_entry.record(), retained_entry.record());
    assert!(validate_train_presentation(area.field_file(), changed_entry.record(),
        changed_entry.initial_rng(), &visit.anchor).is_err(),
        "a valid entry envelope with another RNG cannot validate the unchanged captured anchor");
    assert!(validate_train_presentation(area.field_file(), retained_entry.record(),
        retained_entry.initial_rng(), &visit.anchor).is_ok(),
        "the immutable entry envelope supplies the original cosmetic RNG after checkpoint removal");
    assert_eq!(visit.anchor, anchor);
    assert!(visit.active_continuation.is_none());
    assert_ne!(visit.visit_id, [0; 16]);
    assert_ne!(visit.entry.operation, [0; 16]);
    assert_ne!(visit.visit_id, visit.entry.operation, "distinct operation/visit domains");
    let verified = replay_train_arrival_record(area.field_file(), &entry_record).unwrap();
    let ready = train_arrival_ready(&verified).unwrap();
    assert_eq!(visit.scratch_values, *ready.scratch().after().values());
    assert_eq!(visit.scratch_known_masks, *ready.scratch().after().known_masks());
    assert_eq!(visit.scratch_values[31], 0, "the actual explicit entry retains a fresh Jessie visit latch");
    assert!(source.complete(&host, &completed).is_err());

    assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 17, 3))));
    let earned = snapshot(&handle, account).await;
    let character = earned.pair.character.as_ref().unwrap();
    assert_eq!((character.exp, character.gil, character.ap, character.limit), (52, 257, 10, 137));
    assert_eq!(earned.pair.story, completed.pair.story);
    assert_eq!(earned.field_visit, completed.field_visit);
    let durable = files(dir.path());
    for job in &retry_jobs {
        let duplicate = handle.save_scene_batch(job.clone()).unwrap().await.unwrap().unwrap();
        assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
        assert_eq!(duplicate.current, vec![earned.clone()], "an old prefix or completion returns current rewarded state");
    }
    assert_eq!(files(dir.path()), durable);
    queue.shutdown(Duration::from_secs(20)).await;
    drop(handle); drop(queue); drop(store);
    let reopened = Store::open(dir.path().join("state")).unwrap().get_scene_state(account, GAME).unwrap();
    assert_eq!(reopened, earned);
    let reopened_visit = reopened.field_visit.as_ref().unwrap();
    assert_eq!(reopened_visit.entry.arrival, completed.field_visit.as_ref().unwrap().entry.arrival);
    let reopened_entry = TrainPresentationEntry::decode(&reopened_visit.entry.arrival).unwrap();
    assert_eq!(reopened_entry.record(), entry_record);
    assert_eq!(reopened_entry.initial_rng(), INITIAL_SHAKE_RNG);
}
