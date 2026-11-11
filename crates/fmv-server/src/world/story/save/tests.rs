//! Lifecycle tests for exact ordinary progression. The synthetic game uses opaque bytes:
//! the world must preserve a clear/zero assignment without interpreting or merging it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaInfo, AreaRuntime, Bounds, GameAdapter, MoveResult, StoryPoint, StoryStep, StoryTrigger};
use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, GameId, PhaseKey, Pos};
use fmv_fixture::rooms::{RoomsAdapter, HALL};
use fmv_party::{InviteId, PartyConfig, PartyEvent};
use fmv_persist::{ProgressPair, Progression, SceneCommit};
use fmv_protocol::{ClientMsg, CountdownState, PartyFailure, SceneOutcome, SceneReplay, ServerMsg};
use tokio::sync::{mpsc, oneshot, watch};

use crate::saver::{Msg, SaverHandle, SceneSaveError, SceneSaved, StoryCasJob};
use crate::world::{JoinError, MoveOutcome, NewPlayer, Outbound, PartyOutcome, SceneBoundary, SceneBoundaryKind, SceneHost, SceneInfo, SceneResult, SceneSettings, TriggerInfo, World};

const BEFORE: &[u8] = &[3, 7, 0, 1, 3, 255, 42];
const CLEARED: &[u8] = &[3, 7, 0, 1, 3, 0, 0];
const AFTER: &[u8] = &[3, 8, 0, 1, 3, 0, 0];
const AUTOMATIC: &str = "fixture/after-exact-entry";
const POINT_SCENE: &str = "fixture/exact-entry-point";

type Calls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

struct ExactRoom {
    room: Box<dyn AreaRuntime>,
    entry_points: bool,
    calls: Calls,
}

impl AreaRuntime for ExactRoom {
    fn info(&self) -> &AreaInfo { self.room.info() }
    fn spawn_points(&self) -> Vec<Pos> { self.room.spawn_points() }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult { self.room.validate_move(from, to) }
    fn bounds(&self) -> Bounds { self.room.bounds() }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.room.walk_triangles() }
    fn max_speed(&self) -> f32 { self.room.max_speed() }
    fn phase_key(&self, progress: &[u8]) -> Option<PhaseKey> { Some(PhaseKey(progress.to_vec())) }
    fn story_requires_transaction(&self, progress: &[u8]) -> bool { progress.first() == Some(&3) }
    fn story_points(&self) -> Vec<StoryPoint> {
        if !self.entry_points { return vec![]; }
        (0..2).map(|id| StoryPoint { id, trigger: StoryTrigger::Enter, label: format!("entry-{id}") }).collect()
    }
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        self.calls.lock().unwrap().push((point, progress.to_vec()));
        match (point, progress) {
            (0, BEFORE) => Some(step(CLEARED, None)),
            (1, CLEARED) => Some(step(AFTER, None)),
            _ => None,
        }
    }
    fn countdown_live(&self, started: &[u8], now: &[u8]) -> bool { started == now }
}

struct ArrivalHost;

impl SceneHost for ArrivalHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![SceneInfo { id: AUTOMATIC.into(), field: HALL.into(), arrival: Pos::default(), arrival_facing: 0.0, arrival_hint: None }]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { scene == AUTOMATIC && progress == AFTER }
    fn automatic(&self, scene: &str) -> bool { scene == AUTOMATIC }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { None }
}

struct PointHost;

impl SceneHost for PointHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![SceneInfo { id: POINT_SCENE.into(), field: HALL.into(), arrival: Pos::default(), arrival_facing: 0.0, arrival_hint: None }]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { scene == POINT_SCENE && progress == BEFORE }
    fn point_scene(&self, field: &str, before: &[u8], after: &[u8]) -> Option<String> {
        (field == HALL && before == BEFORE && after == CLEARED).then(|| POINT_SCENE.into())
    }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> {
        panic!("the point owns its story result; its presentation does not write another result")
    }
}

struct OrdinaryHost;

impl SceneHost for OrdinaryHost {
    fn scenes(&self) -> Vec<SceneInfo> { PointHost.scenes() }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { scene == POINT_SCENE && progress == BEFORE }
    fn automatic(&self, scene: &str) -> bool { scene == POINT_SCENE }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, progress: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> {
        Some(SceneResult { progress: progress.to_vec(), changed: false, summary: "presentation only".into(), travel: None, transaction: None })
    }
}

struct WritingHost;

impl SceneHost for WritingHost {
    fn scenes(&self) -> Vec<SceneInfo> { OrdinaryHost.scenes() }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { OrdinaryHost.offered(scene, progress) }
    fn automatic(&self, scene: &str) -> bool { OrdinaryHost.automatic(scene) }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> {
        Some(SceneResult { progress: CLEARED.to_vec(), changed: true, summary: "scene assignment".into(), travel: None, transaction: None })
    }
}

struct VerifiedPointHost { calls: Arc<Mutex<Vec<u32>>> }

impl SceneHost for VerifiedPointHost {
    fn scenes(&self) -> Vec<SceneInfo> { PointHost.scenes() }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool { scene == POINT_SCENE && progress == BEFORE }
    fn continuation(&self, scene: &str) -> bool { scene == POINT_SCENE }
    fn point_continuation(&self, field: &str, before: &[u8], point: u32) -> Option<String> {
        self.calls.lock().unwrap().push(point);
        (field == HALL && before == BEFORE && point == 9).then(|| POINT_SCENE.into())
    }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> {
        panic!("verified point continuations use the strict boundary evaluator")
    }
    fn evaluate_continuation(&self, scene: &str, progress: &[u8], _: u32, gil: u32, replay: Option<&SceneReplay>) -> Option<SceneBoundary> {
        if scene != POINT_SCENE || progress != BEFORE
            || replay.is_some_and(|replay| replay.source != [19; 32] || !replay.events.is_empty() || replay.validate().is_err()) { return None; }
        Some(SceneBoundary { source: [19; 32], progress: progress.to_vec(), gil,
            at: SceneBoundaryKind::Window { step: 0, identity: vec![9, 1], choices: None } })
    }
}

struct Seat {
    account: AccountId,
    entity: EntityId,
    rx: mpsc::Receiver<Outbound>,
}

impl Seat {
    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut messages = vec![];
        while let Ok(message) = self.rx.try_recv() { messages.push((*message).clone()); }
        messages
    }
}

fn world(entry_points: bool) -> (World, mpsc::UnboundedReceiver<Msg>, Calls) {
    let calls = Arc::new(Mutex::new(vec![]));
    let room = RoomsAdapter::new().load_area(HALL).unwrap();
    let mut world = World::with_party_config(Box::new(ExactRoom { room, entry_points, calls: calls.clone() }), PartyConfig::default()).unwrap();
    let (saver, queue) = SaverHandle::channel();
    world.set_progress_saver(saver);
    (world, queue, calls)
}

fn new_player(world: &mut World, account: Option<AccountId>, story: &[u8]) -> (NewPlayer, Seat) {
    let (session, entity) = world.allocate_ids();
    let account = account.unwrap_or(AccountId(entity.0));
    let (tx, rx) = mpsc::channel(256);
    let new = NewPlayer {
        session, entity, account,
        avatar: AvatarAppearance { display_name: format!("player-{}", entity.0), tint: [1, 2, 3], appearance_ref: None },
        progression: story.to_vec(), progress: Default::default(), scene_state: None, scene_group: vec![],
        tx, close: Arc::new(watch::channel(false).0),
    };
    (new, Seat { account, entity, rx })
}

fn seat(world: &mut World, story: &[u8], now: Instant) -> Seat {
    let (new, seat) = new_player(world, None, story);
    world.join(new, now).unwrap();
    seat
}

fn step(progress: &[u8], countdown: Option<u32>) -> StoryStep {
    StoryStep { progress: progress.to_vec(), summary: "exact-assignment".into(), countdown }
}

fn pending(queue: &mut mpsc::UnboundedReceiver<Msg>) -> (StoryCasJob, oneshot::Sender<Result<SceneSaved, SceneSaveError>>) {
    match queue.try_recv().expect("one exact story request") {
        Msg::StoryCas { job, done } => (job, done),
        _ => panic!("ordinary exact state must use an acknowledged CAS, not a merge save"),
    }
}

fn saved(account: AccountId, progress: &[u8]) -> SceneSaved {
    SceneSaved { commit: SceneCommit::Applied, pair: ProgressPair {
        story: Progression::new(account, GameId::Fixture, progress.to_vec(), 17), character: None,
    } }
}

fn token(messages: &[ServerMsg]) -> u64 {
    let ids = messages.iter().filter_map(|message| match message {
        ServerMsg::StoryPending { id, .. } => Some(*id), _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(ids.len(), 1, "one pending token: {messages:?}");
    ids[0]
}

fn no_publication(messages: &[ServerMsg]) {
    assert!(!messages.iter().any(|message| matches!(message,
        ServerMsg::StoryUpdate { advance: Some(_), .. } | ServerMsg::StoryCommitted { .. }
        | ServerMsg::Countdown { .. } | ServerMsg::SceneStart { .. })), "premature publication: {messages:?}");
}

fn invite(messages: &[ServerMsg]) -> InviteId {
    messages.iter().find_map(|message| match message {
        ServerMsg::PartyEvent { event: PartyEvent::InviteSent { invite, .. }, .. } => Some(*invite),
        _ => None,
    }).expect("an invitation was delivered")
}

#[test]
fn exact_clear_waits_for_both_acks_and_publishes_story_phase_and_full_countdown_once() {
    for storage_first in [false, true] {
        let now = Instant::now();
        let (mut world, mut queue, _) = world(false);
        let mut owner = seat(&mut world, BEFORE, now);
        owner.drain();
        world.take_logs();
        let phase = world.players[&owner.entity].placement.clone();
        let position = world.players[&owner.entity].snapshot.pos;
        assert!(world.apply_story_step(owner.entity, 9, step(CLEARED, Some(30))));
        let messages = owner.drain();
        let id = token(&messages);
        assert!(messages.iter().any(|message| matches!(message,
            ServerMsg::StoryPending { area, pos, facing, .. } if area.key == HALL && *pos == position && *facing == 0.0)));
        no_publication(&messages);
        let (job, done) = pending(&mut queue);
        assert_eq!((job.account, job.expected_story.as_slice(), job.next_story.as_slice()), (owner.account, BEFORE, CLEARED));
        assert!(world.ordinary_story_account_held(owner.account));
        assert_eq!(world.apply_move(owner.entity, position, 1.0, AnimState::Walk, now), MoveOutcome::Frozen);
        world.advance_countdowns(now + Duration::from_secs(60));
        assert_eq!(world.countdown_count(), 0, "waiting for disk does not spend the future countdown");
        assert_eq!(world.players[&owner.entity].progression, BEFORE);
        assert_eq!(world.players[&owner.entity].placement, phase);
        assert!(!world.take_logs().iter().any(|line| line.text().contains("story advanced")));
        if storage_first {
            done.send(Ok(saved(owner.account, CLEARED))).unwrap();
            world.advance_story_saves(now);
            no_publication(&owner.drain());
            assert_eq!(world.players[&owner.entity].progression, BEFORE);
            world.story_pending_ack(owner.entity, id.wrapping_add(1), now);
            world.advance_story_saves(now);
            no_publication(&owner.drain());
            world.story_pending_ack(owner.entity, id, now);
        } else {
            world.story_pending_ack(owner.entity, id, now);
            world.advance_story_saves(now);
            no_publication(&owner.drain());
            assert_eq!(world.players[&owner.entity].progression, BEFORE);
            done.send(Ok(saved(owner.account, CLEARED))).unwrap();
        }
        world.advance_story_saves(now);
        let messages = owner.drain();
        let updated = messages.iter().position(|message| matches!(message,
            ServerMsg::StoryUpdate { progress, advance: Some(advance) } if progress == CLEARED && advance.point == 9)).expect("confirmed exact clear");
        let released = messages.iter().position(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)).expect("matching release");
        assert!(updated < released, "confirmed state precedes control release: {messages:?}");
        assert!(messages.iter().any(|message| matches!(message, ServerMsg::Countdown {
            state: CountdownState::Running { remaining_ms: 30_000, total_ms: 30_000 }
        })), "the full countdown starts only after acknowledgment: {messages:?}");
        assert_eq!(world.players[&owner.entity].progression, CLEARED);
        assert_ne!(world.players[&owner.entity].placement, phase);
        assert!(!world.ordinary_story_account_held(owner.account));
        world.story_pending_ack(owner.entity, id, now);
        world.advance_story_saves(now);
        assert!(owner.drain().is_empty(), "duplicate acks do not publish or restart the timer");
        assert!(queue.try_recv().is_err());
    }
}

#[test]
fn disconnect_keeps_the_account_reserved_until_disk_finishes_then_fresh_login_sees_the_result() {
    let now = Instant::now();
    let (mut world, mut queue, _) = world(false);
    let mut owner = seat(&mut world, BEFORE, now);
    owner.drain();
    let before_epoch = world.scene_save_epoch();
    assert!(world.apply_story_step(owner.entity, 4, step(CLEARED, Some(30))));
    let _id = token(&owner.drain());
    let (_, done) = pending(&mut queue);
    let queued_epoch = world.scene_save_epoch();
    assert_ne!(before_epoch, queued_epoch);
    assert!(world.leave(owner.entity));
    assert!(world.is_online(owner.account), "pending persistence owns the account after disconnect");
    let (new, _) = new_player(&mut world, Some(owner.account), BEFORE);
    assert_eq!(world.join(new, now), Err(JoinError::AlreadyOnline));
    done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now);
    assert!(!world.is_online(owner.account));
    assert_eq!(world.countdown_count(), 0, "an offline ack must not create an orphan session timer");
    for old_epoch in [before_epoch, queued_epoch] {
        let (new, _) = new_player(&mut world, Some(owner.account), BEFORE);
        assert_eq!(world.join_with_scene_epoch(new, now, old_epoch), Err(JoinError::ProgressChanged));
    }
    let current_epoch = world.scene_save_epoch();
    let (new, mut returned) = new_player(&mut world, Some(owner.account), CLEARED);
    world.join_with_scene_epoch(new, now, current_epoch).unwrap();
    assert_eq!(world.players[&returned.entity].progression, CLEARED);
    assert!(!returned.drain().iter().any(|message| matches!(message, ServerMsg::StoryPending { .. } | ServerMsg::StoryCommitted { .. })));
    assert!(queue.try_recv().is_err());
}

#[test]
fn refused_missing_or_mismatched_storage_confirmation_never_publishes_a_success() {
    for failure in 0..3 {
        let now = Instant::now();
        let (mut world, mut queue, _) = world(false);
        let mut owner = seat(&mut world, BEFORE, now);
        let mut unrelated = seat(&mut world, BEFORE, now);
        owner.drain(); unrelated.drain();
        assert!(world.apply_story_step(owner.entity, 3, step(CLEARED, Some(10))));
        let id = token(&owner.drain());
        let (_, done) = pending(&mut queue);
        world.story_pending_ack(owner.entity, id, now);
        match failure {
            0 => done.send(Err(SceneSaveError::StaleStory)).unwrap(),
            1 => drop(done),
            _ => done.send(Ok(saved(owner.account, AFTER))).unwrap(),
        }
        world.advance_story_saves(now);
        assert_eq!(world.player_count(), 0, "unconfirmed storage fences gameplay until recovery");
        for messages in [owner.drain(), unrelated.drain()] {
            no_publication(&messages);
            assert!(messages.iter().any(|message| matches!(message, ServerMsg::Reject { code: fmv_protocol::RejectCode::Internal, .. })));
        }
        assert_eq!(world.countdown_count(), 0);
        let (new, _) = new_player(&mut world, Some(owner.account), BEFORE);
        assert_eq!(world.join(new, now), Err(JoinError::StorageUnavailable));
        assert!(queue.try_recv().is_err());
    }
}

#[test]
fn sequential_entry_steps_use_acknowledged_predecessors_and_one_hold_before_the_automatic_scene() {
    let now = Instant::now();
    let (mut world, mut queue, calls) = world(true);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(ArrivalHost), debug_preview: false, debug_new_story: None }));
    let mut owner = seat(&mut world, BEFORE, now);
    let messages = owner.drain();
    let id = token(&messages);
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneStart { .. })));
    let (first, first_done) = pending(&mut queue);
    assert_eq!((first.expected_story.as_slice(), first.next_story.as_slice()), (BEFORE, CLEARED));
    assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec())]);
    world.story_pending_ack(owner.entity, id, now);
    first_done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress == CLEARED)));
    assert!(!messages.iter().any(|message| matches!(message, ServerMsg::StoryPending { .. } | ServerMsg::StoryCommitted { .. } | ServerMsg::SceneStart { .. })));
    assert_eq!(world.players[&owner.entity].story_pending, Some(id));
    let (second, second_done) = pending(&mut queue);
    assert_eq!((second.expected_story.as_slice(), second.next_story.as_slice()), (CLEARED, AFTER));
    assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec()), (1, CLEARED.to_vec())], "only the unprocessed point is revisited");
    second_done.send(Ok(saved(owner.account, AFTER))).unwrap();
    world.advance_story_saves(now);
    let messages = owner.drain();
    let update = messages.iter().position(|message| matches!(message, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress == AFTER)).expect("second step");
    let start = messages.iter().position(|message| matches!(message, ServerMsg::SceneStart { scene, progress, .. } if scene == AUTOMATIC && progress == AFTER)).expect("dependent automatic scene");
    let release = messages.iter().position(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)).expect("one final control release");
    assert!(update < start && start < release, "the confirmed chain is scheduled before release: {messages:?}");
    assert!(queue.try_recv().is_err());
    assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec()), (1, CLEARED.to_vec())]);
}

#[test]
fn a_point_scene_releases_after_its_exact_save_then_resumes_only_the_remaining_entry_step() {
    for skipped in [false, true] {
        let now = Instant::now();
        let (mut world, mut queue, calls) = world(true);
        world.configure_scenes(Some(SceneSettings { host: Arc::new(PointHost), debug_preview: false, debug_new_story: None }));
        let mut owner = seat(&mut world, BEFORE, now);
        let messages = owner.drain();
        let run = messages.iter().find_map(|message| match message {
            ServerMsg::SceneStart { run, scene, progress, .. } if scene == POINT_SCENE && progress == BEFORE => Some(*run),
            _ => None,
        }).expect("the first entry point plays its presentation before saving");
        assert_eq!(world.players[&owner.entity].scene, Some(run));
        assert_eq!(world.players[&owner.entity].story_pending, None);
        assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec())]);
        assert!(queue.try_recv().is_err());

        world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped }, now);
        let messages = owner.drain();
        let id = token(&messages);
        no_publication(&messages);
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneEnded { .. })));
        assert_eq!(world.players[&owner.entity].scene, Some(run), "the terminal scene remains held during its CAS");
        assert_eq!(world.players[&owner.entity].progression, BEFORE);
        let (first, done) = pending(&mut queue);
        assert_eq!((first.expected_story.as_slice(), first.next_story.as_slice()), (BEFORE, CLEARED));
        assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec()), (0, BEFORE.to_vec())], "completion validates the still-held predecessor");

        world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped: !skipped }, now);
        assert!(owner.drain().is_empty(), "a repeated completion cannot replace the original outcome");
        assert!(queue.try_recv().is_err(), "one scene completion queues one save");
        world.story_pending_ack(owner.entity, id, now);
        assert!(owner.drain().is_empty());
        done.send(Ok(saved(owner.account, CLEARED))).unwrap();
        world.advance_story_saves(now);
        let messages = owner.drain();
        let updated = messages.iter().position(|message| matches!(message,
            ServerMsg::StoryUpdate { progress, advance: Some(advance) } if progress == CLEARED && advance.point == 0)).expect("the point's durable result");
        let ended = messages.iter().position(|message| matches!(message,
            ServerMsg::SceneEnded { run: actual, outcome } if *actual == run && *outcome == if skipped { SceneOutcome::Skipped } else { SceneOutcome::Finished }))
            .expect("the held point scene is released with its original outcome");
        assert!(updated < ended, "the result is visible before the presentation releases: {messages:?}");
        assert_eq!(messages.iter().filter(|message| matches!(message, ServerMsg::SceneEnded { .. })).count(), 1);
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::StoryPending { .. } | ServerMsg::StoryCommitted { .. } | ServerMsg::SceneStart { .. })));
        assert_eq!(world.players[&owner.entity].scene, None, "the release must not queue behind its own scene");
        assert_eq!(world.players[&owner.entity].story_pending, Some(id));
        assert_eq!(world.scene_count(), 0);

        let (second, done) = pending(&mut queue);
        assert_eq!((second.expected_story.as_slice(), second.next_story.as_slice()), (CLEARED, AFTER));
        assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec()), (0, BEFORE.to_vec()), (1, CLEARED.to_vec())],
            "the remaining entry point runs once, without rescanning the first");
        done.send(Ok(saved(owner.account, AFTER))).unwrap();
        world.advance_story_saves(now);
        let messages = owner.drain();
        let updated = messages.iter().position(|message| matches!(message,
            ServerMsg::StoryUpdate { progress, advance: Some(advance) } if progress == AFTER && advance.point == 1)).expect("the remaining point's result");
        let released = messages.iter().position(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)).expect("one final control release");
        assert!(updated < released);
        assert!(!messages.iter().any(|message| matches!(message, ServerMsg::SceneStart { .. } | ServerMsg::SceneEnded { .. })));
        assert_eq!(world.players[&owner.entity].scene, None);
        assert_eq!(world.players[&owner.entity].story_pending, None);
        assert!(!world.ordinary_story_account_held(owner.account));
        assert_eq!(world.players[&owner.entity].progression, AFTER);
        world.advance_story_saves(now);
        assert!(owner.drain().is_empty());
        assert!(queue.try_recv().is_err());
    }
}

#[test]
fn a_point_scene_that_loses_story_permission_releases_without_an_exact_write() {
    let now = Instant::now();
    let (mut world, mut queue, calls) = world(true);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(PointHost), debug_preview: false, debug_new_story: None }));
    let mut owner = seat(&mut world, BEFORE, now);
    let run = world.players[&owner.entity].scene.expect("the entry point's presentation");
    owner.drain();
    world.players.get_mut(&owner.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
    world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message,
        ServerMsg::SceneEnded { run: actual, outcome: SceneOutcome::Finished } if *actual == run)));
    no_publication(&messages);
    assert_eq!(world.players[&owner.entity].progression, BEFORE);
    assert_eq!(world.players[&owner.entity].scene, None);
    assert_eq!(world.players[&owner.entity].story_pending, None);
    assert!(!world.ordinary_story_account_held(owner.account));
    assert_eq!(world.scene_count(), 0);
    assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec())], "neither the old nor remaining point may write the leader's phase");
    assert!(queue.try_recv().is_err());
}

#[test]
fn an_ordinary_scene_followup_acquires_a_fresh_nonzero_barrier_before_its_first_exact_write() {
    let now = Instant::now();
    let (mut world, mut queue, calls) = world(false);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(OrdinaryHost), debug_preview: false, debug_new_story: None }));
    let mut owner = seat(&mut world, BEFORE, now);
    let run = world.players[&owner.entity].scene.expect("an ordinary scene has no existing story token");
    assert_eq!(world.players[&owner.entity].story_pending, None);
    owner.drain();
    // This models trigger work deferred behind an ordinary/victory presentation. The real
    // SceneEnd dispatcher must replace its internal no-token context with a new wire barrier.
    world.defer_scene_story_followup(run, super::StoryFollowup::Sweep { area: HALL.into(), points: vec![0], on_entry: false });
    world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let messages = owner.drain();
    let id = token(&messages);
    assert_ne!(id, 0, "the internal no-token sentinel is never sent or treated as acknowledged");
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::SceneEnded { run: actual, .. } if *actual == run)));
    no_publication(&messages);
    assert_eq!(world.players[&owner.entity].scene, None);
    assert_eq!(world.players[&owner.entity].story_pending, Some(id));
    let (job, done) = pending(&mut queue);
    assert_eq!((job.expected_story.as_slice(), job.next_story.as_slice()), (BEFORE, CLEARED));
    assert_eq!(*calls.lock().unwrap(), [(0, BEFORE.to_vec())]);
    done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now);
    assert!(owner.drain().is_empty(), "a durable result alone cannot release newly accepted field input");
    assert_eq!(world.players[&owner.entity].progression, BEFORE);
    world.story_pending_ack(owner.entity, 0, now);
    assert!(owner.drain().is_empty(), "the sentinel cannot acknowledge a real token");
    world.story_pending_ack(owner.entity, id, now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress == CLEARED)));
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)));
    assert_eq!(world.players[&owner.entity].story_pending, None);
    assert_eq!(world.players[&owner.entity].progression, CLEARED);
    assert!(queue.try_recv().is_err());
}

#[test]
fn a_scene_queued_exact_write_preserves_its_deferred_sweep_until_the_first_ack() {
    let now = Instant::now();
    let (mut world, mut queue, calls) = world(false);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(WritingHost), debug_preview: false, debug_new_story: None }));
    let mut owner = seat(&mut world, BEFORE, now);
    let run = world.players[&owner.entity].scene.expect("an ordinary automatic scene");
    owner.drain();
    world.defer_scene_story_followup(run, super::StoryFollowup::Sweep { area: HALL.into(), points: vec![1], on_entry: false });
    world.scene_message(owner.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let messages = owner.drain();
    let id = token(&messages);
    assert_ne!(id, 0);
    no_publication(&messages);
    assert_eq!(messages.iter().filter(|message| matches!(message, ServerMsg::SceneEnded { run: actual, .. } if *actual == run)).count(), 1);
    let (first, done) = pending(&mut queue);
    assert_eq!((first.expected_story.as_slice(), first.next_story.as_slice()), (BEFORE, CLEARED));
    assert!(calls.lock().unwrap().is_empty(), "the deferred point cannot inspect the uncommitted predecessor");
    assert_eq!(world.players[&owner.entity].progression, BEFORE);
    assert_eq!(world.players[&owner.entity].story_pending, Some(id));

    done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now);
    assert!(owner.drain().is_empty(), "the first result still waits for the ordered client barrier");
    assert!(calls.lock().unwrap().is_empty());
    assert!(queue.try_recv().is_err());
    world.story_pending_ack(owner.entity, id, now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress == CLEARED)));
    assert!(!messages.iter().any(|message| matches!(message,
        ServerMsg::StoryPending { .. } | ServerMsg::StoryCommitted { .. } | ServerMsg::SceneStart { .. } | ServerMsg::SceneEnded { .. })));
    assert_eq!(*calls.lock().unwrap(), [(1, CLEARED.to_vec())], "the pending handoff preserves and resumes the original sweep");
    let (second, done) = pending(&mut queue);
    assert_eq!((second.expected_story.as_slice(), second.next_story.as_slice()), (CLEARED, AFTER));
    assert_eq!(world.players[&owner.entity].story_pending, Some(id), "the existing acknowledged token spans both writes");

    done.send(Ok(saved(owner.account, AFTER))).unwrap();
    world.advance_story_saves(now);
    let messages = owner.drain();
    let updated = messages.iter().position(|message| matches!(message,
        ServerMsg::StoryUpdate { progress, advance: Some(advance) } if progress == AFTER && advance.point == 1)).expect("the preserved point's result");
    let released = messages.iter().position(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)).expect("one final release");
    assert!(updated < released);
    assert_eq!(messages.iter().filter(|message| matches!(message, ServerMsg::StoryCommitted { .. })).count(), 1);
    assert_eq!(world.players[&owner.entity].progression, AFTER);
    assert_eq!(world.players[&owner.entity].story_pending, None);
    assert_eq!(world.players[&owner.entity].scene, None);
    assert!(!world.ordinary_story_account_held(owner.account));
    world.advance_story_saves(now);
    assert!(owner.drain().is_empty());
    assert!(queue.try_recv().is_err());
    assert_eq!(*calls.lock().unwrap(), [(1, CLEARED.to_vec())]);
}

#[test]
fn an_exact_point_uses_its_verified_continuation_before_the_ordinary_interpreter() {
    use super::super::Reached;
    let now = Instant::now();
    let (mut world, mut queue, calls) = world(false);
    let hook_calls = Arc::new(Mutex::new(vec![]));
    world.configure_scenes(Some(SceneSettings {
        host: Arc::new(VerifiedPointHost { calls: hook_calls.clone() }), debug_preview: false, debug_new_story: None,
    }));
    let mut owner = seat(&mut world, BEFORE, now);
    owner.drain();
    // ExactRoom cannot interpret point 9. Only the explicitly reviewed continuation may
    // admit it, without asking the ordinary interpreter to guess a result first.
    assert_eq!(world.reach_story_point(owner.entity, 9, None, now, false), Reached::Held);
    let messages = owner.drain();
    let run = messages.iter().find_map(|message| match message {
        ServerMsg::SceneStart { run, scene, progress, replay: Some(replay), .. }
            if scene == POINT_SCENE && progress == BEFORE && replay.source == [19; 32] && replay.events.is_empty() => Some(*run),
        _ => None,
    }).expect("the reviewed point starts with its verified empty replay");
    assert_eq!(world.players[&owner.entity].scene, Some(run));
    assert_eq!(world.players[&owner.entity].progression, BEFORE);
    assert_eq!(*hook_calls.lock().unwrap(), [9]);
    assert!(calls.lock().unwrap().is_empty(), "strict point interpretation must not run before the reviewed continuation");
    assert!(queue.try_recv().is_err(), "admission to the first window has no story result to save");
}

#[test]
fn verified_point_admission_requires_an_exact_story_matching_host_and_allowed_phase() {
    use super::super::Reached;
    for case in ["unmatched point", "missing host", "denied phase", "legacy story"] {
        let now = Instant::now();
        let (mut world, mut queue, calls) = world(false);
        let hook_calls = Arc::new(Mutex::new(vec![]));
        if case != "missing host" {
            world.configure_scenes(Some(SceneSettings {
                host: Arc::new(VerifiedPointHost { calls: hook_calls.clone() }), debug_preview: false, debug_new_story: None,
            }));
        }
        let story: &[u8] = if case == "legacy story" { &[1, 7] } else { BEFORE };
        let mut owner = seat(&mut world, story, now);
        owner.drain();
        if case == "denied phase" {
            world.players.get_mut(&owner.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
        }
        let point = if case == "unmatched point" { 10 } else { 9 };
        assert_eq!(world.reach_story_point(owner.entity, point, None, now, false), Reached::Nothing, "{case}");
        assert_eq!(world.players[&owner.entity].scene, None, "{case}");
        assert_eq!(world.players[&owner.entity].story_pending, None, "{case}");
        assert_eq!(world.players[&owner.entity].progression, story, "{case}");
        no_publication(&owner.drain());
        assert!(queue.try_recv().is_err(), "{case}");
        if case == "unmatched point" { assert_eq!(*hook_calls.lock().unwrap(), [10]); }
        else { assert!(hook_calls.lock().unwrap().is_empty(), "{case} must not invoke the verified hook"); }
        if case == "denied phase" { assert!(calls.lock().unwrap().is_empty()); }
        else { assert_eq!(*calls.lock().unwrap(), [(point, story.to_vec())], "{case} retains the ordinary fallthrough"); }
    }
}

#[test]
fn a_stale_ack_cannot_release_a_later_independent_story_save() {
    let now = Instant::now();
    let (mut world, mut queue, _) = world(false);
    let mut owner = seat(&mut world, BEFORE, now);
    owner.drain();
    world.apply_story_step(owner.entity, 0, step(CLEARED, None));
    let first = token(&owner.drain());
    let (_, done) = pending(&mut queue);
    world.story_pending_ack(owner.entity, first, now);
    done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now); owner.drain();
    world.apply_story_step(owner.entity, 1, step(AFTER, None));
    let second = token(&owner.drain());
    assert_ne!(first, second);
    let (_, done) = pending(&mut queue);
    done.send(Ok(saved(owner.account, AFTER))).unwrap();
    world.story_pending_ack(owner.entity, first, now);
    world.advance_story_saves(now);
    no_publication(&owner.drain());
    assert_eq!(world.players[&owner.entity].story_pending, Some(second));
    assert_eq!(world.players[&owner.entity].progression, CLEARED);
    world.story_pending_ack(owner.entity, second, now);
    world.advance_story_saves(now);
    assert!(owner.drain().iter().any(|message| matches!(message, ServerMsg::StoryCommitted { id } if *id == second)));
    assert_eq!(world.players[&owner.entity].progression, AFTER);
}

#[test]
fn legacy_and_already_saved_steps_keep_their_existing_publication_paths() {
    let now = Instant::now();
    let (mut world, mut queue, _) = world(false);
    let mut legacy = seat(&mut world, &[1, 7], now);
    legacy.drain();
    assert!(!world.apply_story_step(legacy.entity, 2, step(&[1, 8], None)));
    match queue.try_recv().unwrap() {
        Msg::Story(job) => assert_eq!((job.account, job.progress), (legacy.account, vec![1, 8])),
        _ => panic!("legacy saves retain their merge path"),
    }
    assert_eq!(world.players[&legacy.entity].progression, [1, 8]);
    assert!(!legacy.drain().iter().any(|message| matches!(message, ServerMsg::StoryPending { .. })));

    let mut exact = seat(&mut world, BEFORE, now);
    exact.drain();
    world.apply_saved_story_step(exact.entity, 5, step(CLEARED, None));
    assert_eq!(world.players[&exact.entity].progression, CLEARED);
    assert!(exact.drain().iter().any(|message| matches!(message, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress == CLEARED)));
    assert!(queue.try_recv().is_err(), "an acknowledged scene/battle result is not saved twice");
}

#[test]
fn an_exact_step_without_a_configured_saver_cannot_be_published_as_persistent_progress() {
    let now = Instant::now();
    let (mut world, queue, _) = world(false);
    let mut owner = seat(&mut world, BEFORE, now);
    owner.drain();
    world.saver = None;
    drop(queue);
    world.apply_story_step(owner.entity, 4, step(CLEARED, Some(20)));
    no_publication(&owner.drain());
    assert_eq!(world.player_count(), 0);
    assert_eq!(world.countdown_count(), 0);
    let (new, _) = new_player(&mut world, Some(owner.account), BEFORE);
    assert_eq!(world.join(new, now), Err(JoinError::StorageUnavailable));
}

#[test]
fn a_pending_owner_holds_party_membership_and_placement_until_the_acknowledged_phase_change() {
    let now = Instant::now();
    let (mut world, mut queue, _) = world(false);
    let mut owner = seat(&mut world, BEFORE, now);
    let mut follower = seat(&mut world, BEFORE, now);
    let mut candidate = seat(&mut world, BEFORE, now);
    owner.drain(); follower.drain(); candidate.drain();
    assert_eq!(world.party_invite(owner.entity, follower.entity, now), PartyOutcome::Done);
    let accepted = invite(&follower.drain());
    assert_eq!(world.party_accept(follower.entity, accepted, now), PartyOutcome::Done);
    assert_eq!(world.party_invite(owner.entity, candidate.entity, now), PartyOutcome::Done);
    let waiting = invite(&candidate.drain());
    owner.drain(); follower.drain();
    let owner_before = world.players[&owner.entity].placement.clone();
    let follower_before = world.players[&follower.entity].placement.clone();

    assert!(world.apply_story_step(owner.entity, 6, step(CLEARED, None)));
    let id = token(&owner.drain());
    let (_, done) = pending(&mut queue);
    let busy = PartyOutcome::Refused(PartyFailure::Busy);
    assert_eq!(world.party_leave(owner.entity), busy);
    assert_eq!(world.party_leave(follower.entity), busy, "the affected cohort cannot dissolve its phase owner");
    assert_eq!(world.party_invite(candidate.entity, follower.entity, now), busy);
    assert_eq!(world.party_accept(candidate.entity, waiting, now), busy, "an earlier invite cannot bypass the held inviter");
    assert_eq!(world.party_promote(owner.entity, follower.entity), busy);
    assert_eq!(world.party_kick(owner.entity, follower.entity), busy);

    let mut failed = vec![];
    let held = BTreeSet::from([owner.account]);
    world.refresh_placements(&held, &mut failed);
    world.refresh_story_placements(&held, &mut failed);
    assert!(failed.is_empty());
    assert!(owner.drain().is_empty(), "unrelated refreshes must not send a fresh held snapshot");
    assert_eq!(world.players[&owner.entity].placement, owner_before);
    assert_eq!(world.players[&follower.entity].placement, follower_before);

    world.story_pending_ack(owner.entity, id, now);
    done.send(Ok(saved(owner.account, CLEARED))).unwrap();
    world.advance_story_saves(now);
    let messages = owner.drain();
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::AreaSnapshot { .. })), "publication refreshes the owner's phase");
    assert!(messages.iter().any(|message| matches!(message, ServerMsg::StoryCommitted { id: actual } if *actual == id)));
    assert_ne!(world.players[&owner.entity].placement, owner_before);
    assert_ne!(world.players[&follower.entity].placement, follower_before);
    assert_eq!(world.players[&follower.entity].progression, BEFORE, "the follower's own story is unchanged");
    assert_eq!(world.party_accept(candidate.entity, waiting, now), PartyOutcome::Done, "the invitation still works once the hold is released");
    assert!(queue.try_recv().is_err());
}
