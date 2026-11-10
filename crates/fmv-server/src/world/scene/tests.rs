//! Tests of the story scenes in the world, over the FIXTURE rooms and a fake [`SceneHost`] (no game data).

use std::collections::BTreeSet;
use std::sync::Mutex;

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, AnimState, AvatarAppearance};
use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, YARD};
use fmv_party::PartyConfig;
use fmv_protocol::SyntheticRules;
use tokio::sync::{mpsc, watch};

use super::super::{BattleReply, MoveOutcome, NewPlayer, Outbound, PartyOutcome};
use super::*;
use crate::config::BattleSettings;
use crate::saver::{Msg, SaverHandle};

/// The story a previewing player is given by the fake host.
const MADE_UP: &[u8] = &[9];
const SCENE: &str = "test/yard-scene";
/// Where the fake trigger stands in the yard (its talk radius is 80).
const TRIGGER_AT: (f32, f32) = (200.0, -100.0);

/// One `finish` call: scene, story, seed, answers.
type Finish = (String, Vec<u8>, u32, Vec<u8>);

/// The story the fake host has an entry for (see [`FakeHost::entry`]).
const FAR_ALONG: &[u8] = &[7, 7];
/// The fake host's cutscene: of the hall's story region (the fixture's point 0, a new account to `STORY_ADVANCED`).
const POINT_SCENE: &str = "test/hall-cutscene";

#[derive(Default)]
struct FakeHost {
    finished: Mutex<Vec<Finish>>,
    travel: Mutex<Option<fmv_adapter_api::Exit>>,
    automatic: Mutex<bool>,
    /// Where a story of [`FAR_ALONG`] comes into the world, when the test sets one.
    entry: Mutex<Option<fmv_adapter_api::Exit>>,
    /// Whether the hall's story step has its cutscene ([`POINT_SCENE`]).
    cutscene: Mutex<bool>,
    victory: Mutex<bool>,
    transaction: Mutex<Option<SceneTransaction>>,
    resources: Mutex<Vec<u32>>,
    refuse: Mutex<bool>,
}

impl SceneHost for FakeHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![
            SceneInfo { id: SCENE.to_string(), field: YARD.to_string(), arrival: Pos::new(150.0, -100.0, 0.0), arrival_facing: 0.0, arrival_hint: None },
            SceneInfo { id: POINT_SCENE.to_string(), field: HALL.to_string(), arrival: Pos::new(200.0, 0.0, 0.0), arrival_facing: 0.0, arrival_hint: None },
        ]
    }
    fn point_scene(&self, field: &str, before: &[u8], after: &[u8]) -> Option<String> {
        (*self.cutscene.lock().unwrap() && field == HALL && before.is_empty() && after == STORY_ADVANCED).then(|| POINT_SCENE.to_string())
    }
    fn victory_scene(&self, field: &str, before: &[u8], after: &[u8], formation: u16) -> Option<String> {
        (*self.victory.lock().unwrap() && formation == fmv_fixture::rooms::BATTLE_FORMATION
            && field == HALL && before.is_empty() && after == STORY_ADVANCED).then(|| POINT_SCENE.to_string())
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool {
        scene == SCENE && (progress.is_empty() || progress == MADE_UP)
    }
    fn automatic(&self, scene: &str) -> bool { scene == SCENE && *self.automatic.lock().unwrap() }
    fn entry(&self, progress: &[u8]) -> Option<fmv_adapter_api::Exit> {
        if progress == FAR_ALONG { self.entry.lock().unwrap().clone() } else { None }
    }
    fn trigger(&self, scene: &str, _progress: &[u8]) -> Option<TriggerInfo> {
        (scene == SCENE).then(|| TriggerInfo {
            name: "giver".into(),
            model: "aagb".into(),
            idle: Some("abcd".into()),
            pos: Pos::new(TRIGGER_AT.0, TRIGGER_AT.1, 0.0),
            facing: 0.0,
            talk_radius: 80.0,
        })
    }
    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>> {
        (scene == SCENE).then(|| MADE_UP.to_vec())
    }
    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.finished.lock().unwrap().push((scene.to_string(), progress.to_vec(), seed, answers.to_vec()));
        if *self.refuse.lock().unwrap() { return None; }
        let after = if progress == MADE_UP { vec![9, 5] } else { vec![5] };
        Some(SceneResult { progress: after, changed: true, summary: "moment=1->5".into(), travel: self.travel.lock().unwrap().clone(), transaction: *self.transaction.lock().unwrap() })
    }
    fn finish_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], gil: u32) -> Option<SceneResult> {
        self.resources.lock().unwrap().push(gil);
        self.finish(scene, progress, seed, answers)
    }
    fn finish_preview_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], gil: u32) -> Option<SceneResult> {
        self.resources.lock().unwrap().push(gil);
        self.finish_preview(scene, progress, seed, answers)
    }
}

#[test]
fn automatic_scene_starts_once_on_entry_without_a_trigger_npc() {
    let mut now = Instant::now();
    let (mut w, _saved_rx, host) = world();
    *host.automatic.lock().unwrap() = true;
    let mut a = seat(&mut w, "A", &[], now);
    let msgs = into_the_yard(&mut w, &mut a, &mut now);
    assert_eq!(starts(&msgs).len(), 1);
    assert!(w.in_scene(a.entity));
    assert!(msgs.iter().all(|m| !matches!(m, ServerMsg::AreaSnapshot { npcs, .. } if npcs.iter().any(|n| n.id >= SCENE_NPC_ID_BASE))));
    w.automatic_scene_entered(a.entity, now);
    assert!(a.drain().is_empty(), "a held player cannot start the scene twice");
}

#[test]
fn automatic_scene_uses_and_saves_only_the_members_own_story() {
    let (mut w, mut saved_rx, host, a, mut b, now) = two_at_the_trigger();
    *host.automatic.lock().unwrap() = true;
    w.players.get_mut(&b.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
    w.players.get_mut(&a.entity).unwrap().progression = vec![7];
    b.drain();
    w.automatic_scene_entered(b.entity, now);
    let run = starts(&b.drain())[0].0;
    assert!(w.in_scene(b.entity));
    w.scene_message(b.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, [7]);
    assert_eq!(w.players.get(&b.entity).unwrap().progression, [5]);
    let Some(Msg::Story(job)) = saved_rx.try_recv().ok() else { panic!("the member's story was not saved") };
    assert_eq!((job.account, job.progress), (b.account, vec![5]));
    assert!(saved_rx.try_recv().is_err());
}

/// A world over the fixture rooms WITH their synthetic story (the hall's story region takes a new account to
/// `STORY_ADVANCED`), the fake host with its cutscene for that step on.
fn cutscene_world() -> (World, mpsc::UnboundedReceiver<Msg>, Arc<FakeHost>) {
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_story());
    let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter);
    let (handle, rx) = SaverHandle::channel();
    w.set_progress_saver(handle);
    let host = Arc::new(FakeHost::default());
    *host.cutscene.lock().unwrap() = true;
    w.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: false, debug_new_story: None }));
    (w, rx, host)
}

/// Walk `s` from the hall's spawn into its story region (x 150..250, y -50..50).
fn into_the_region(w: &mut World, s: &Seat, now: &mut Instant) {
    assert_eq!(go(w, s, 100.0, 0.0, now), MoveOutcome::Applied);
    assert_eq!(go(w, s, 200.0, 0.0, now), MoveOutcome::Applied);
}

/// A story point with a cutscene plays it FIRST: the player is held in a run of the cutscene, built against its story before
/// the step, and nothing of the story moves until the run ends; then the point's own step is applied, once (the saver gets
/// it once, after the `SceneEnded`). The cutscene writes nothing of its own: the host is never asked to run it.
#[test]
fn a_point_with_a_cutscene_plays_it_first_and_its_step_follows_when_it_ends() {
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = cutscene_world();
    let mut a = seat(&mut w, "A", &[], now);
    a.drain();
    into_the_region(&mut w, &a, &mut now);
    let msgs = a.drain();
    let started: Vec<_> = msgs.iter().filter_map(|m| match m { ServerMsg::SceneStart { run, scene, progress, .. } => Some((*run, scene.clone(), progress.clone())), _ => None }).collect();
    let [(run, scene, progress)] = &started[..] else { panic!("one cutscene run: {msgs:?}") };
    assert_eq!((scene.as_str(), progress.as_slice()), (POINT_SCENE, &[][..]), "built against the story before the step");
    assert!(w.in_scene(a.entity));
    assert!(w.players.get(&a.entity).unwrap().progression.is_empty(), "the step waits for the run");
    assert!(saved(&mut saved_rx).is_empty());
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { .. })));
    // held: a move is ignored, so the region is not reached again
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: *run, skipped: false }, now);
    let msgs = a.drain();
    let ended = msgs.iter().position(|m| matches!(m, ServerMsg::SceneEnded { .. })).expect("released");
    let story = msgs.iter().position(|m| matches!(m, ServerMsg::StoryUpdate { advance: Some(_), .. })).expect("then the step");
    assert!(ended < story);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED);
    assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()], "saved once");
    assert!(host.finished.lock().unwrap().is_empty(), "a cutscene's own run writes nothing");
    let log = logs_of(&mut w);
    assert!(log.iter().any(|l| l.contains(&format!("scene={POINT_SCENE} outcome=Finished answers=0 story=the point's step"))), "{log:?}");
    assert!(log.iter().any(|l| l.starts_with("story advanced account=") && l.contains("point=0")), "{log:?}");
    // walking on in the region reaches the point again, which no longer applies: no second run
    assert_eq!(go(&mut w, &a, 210.0, 0.0, &mut now), MoveOutcome::Applied);
    assert!(starts(&a.drain()).is_empty() && !w.in_scene(a.entity));
}

/// However the cutscene ends (the owner's client leaves it, or it goes quiet and times out), the step is applied: the story
/// never waits on a presentation. An owner that leaves the world takes nothing with it: no step, no save.
#[test]
fn a_cutscene_that_is_left_or_times_out_still_gives_the_step_and_one_whose_owner_left_gives_nothing() {
    let mut now = Instant::now();
    let (mut w, mut saved_rx, _host) = cutscene_world();
    let mut a = seat(&mut w, "A", &[], now);
    a.drain();
    into_the_region(&mut w, &a, &mut now);
    let run = starts(&a.drain())[0].0;
    w.scene_message(a.entity, ClientMsg::SceneLeave { run }, now);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED, "left: the step all the same");
    assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()]);

    let mut b = seat(&mut w, "B", &[], now);
    b.drain();
    into_the_region(&mut w, &b, &mut now);
    assert_eq!(starts(&b.drain()).len(), 1);
    w.advance_scenes(now + SCENE_IDLE_TIMEOUT + Duration::from_secs(1));
    assert_eq!(w.players.get(&b.entity).unwrap().progression, STORY_ADVANCED, "timed out: the step all the same");
    assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()]);

    let mut c = seat(&mut w, "C", &[], now);
    c.drain();
    into_the_region(&mut w, &c, &mut now);
    assert_eq!(starts(&c.drain()).len(), 1);
    assert!(w.leave(c.entity));
    assert!(saved(&mut saved_rx).is_empty(), "the owner left: nothing applied, nothing saved");
}

/// A party member in range watches the owner's cutscene: it is told the run as a watcher and held, both are released when it
/// ends, and the step goes to the OWNER only (the member's own story and the saver are untouched by it).
#[test]
fn a_party_member_in_range_watches_a_cutscene_and_the_step_is_the_owners_alone() {
    let mut now = Instant::now();
    let (mut w, mut saved_rx, _host) = cutscene_world();
    let mut a = seat(&mut w, "A", &[], now);
    let mut b = seat(&mut w, "B", &[], now);
    party(&mut w, &a, &b, now);
    a.drain();
    b.drain();
    // B stands near the region, A walks in.
    assert_eq!(go(&mut w, &b, 100.0, 60.0, &mut now), MoveOutcome::Applied);
    into_the_region(&mut w, &a, &mut now);
    let a_msgs = a.drain();
    let b_msgs = b.drain();
    let roles = |msgs: &[ServerMsg]| msgs.iter().filter_map(|m| match m { ServerMsg::SceneStart { you, scene, .. } => Some((*you, scene.clone())), _ => None }).collect::<Vec<_>>();
    assert_eq!(roles(&a_msgs), [(SceneRole::Owner, POINT_SCENE.to_string())]);
    assert_eq!(roles(&b_msgs), [(SceneRole::Watcher, POINT_SCENE.to_string())], "the member watches");
    assert!(w.in_scene(a.entity) && w.in_scene(b.entity));
    let run = starts(&a_msgs)[0].0;
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(!w.in_scene(a.entity) && !w.in_scene(b.entity), "both released");
    assert!(b.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { .. })));
    assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED, "the owner's step");
    assert!(w.players.get(&b.entity).unwrap().progression.is_empty(), "the member's own story is its own");
    assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()], "one save: the owner's");
}

/// A point that starts a battle first, with a cutscene: the cutscene plays first (no battle yet), and when it ends the battle
/// starts for the player; the step still waits for the win (the battle's own test pays it).
#[test]
fn a_battle_point_with_a_cutscene_plays_it_and_then_starts_its_battle() {
    use fmv_fixture::rooms::{BATTLE_FORMATION, BATTLE_REGION};
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
    let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter);
    let (handle, mut saved_rx) = SaverHandle::channel();
    w.set_progress_saver(handle);
    let host = Arc::new(FakeHost::default());
    *host.cutscene.lock().unwrap() = true;
    w.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: false, debug_new_story: None }));
    let settings = BattleSettings { battle_speed: 0, ticks_per_second: 1000, fixed_seed: Some(7), ..BattleSettings::new(Arc::new(SyntheticRules::default())) };
    w.configure_battle(Some(settings), false);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    a.drain();
    assert_eq!(BATTLE_REGION[0], (-300.0, 200.0));
    for (x, y) in [(-100.0, 100.0), (-200.0, 180.0), (-270.0, 220.0)] {
        assert_eq!(go(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
    }
    let run = starts(&a.drain()).first().map(|s| s.0).expect("the cutscene first");
    assert!(w.in_scene(a.entity) && !w.in_battle(a.entity), "no battle while it plays");
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(!w.in_scene(a.entity) && w.in_battle(a.entity), "then the point's battle");
    let log = logs_of(&mut w);
    assert!(log.iter().any(|l| l.starts_with("story battle started") && l.contains("point=2") && l.contains(&format!("formation={BATTLE_FORMATION}"))), "{log:?}");
    assert!(w.players.get(&a.entity).unwrap().progression.is_empty(), "the step waits for the win");
    assert!(saved(&mut saved_rx).is_empty());
}

/// Without a cutscene for the step (the host has none) the step is applied at once, exactly as before cutscenes.
#[test]
fn a_point_without_a_cutscene_applies_its_step_at_once() {
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = cutscene_world();
    *host.cutscene.lock().unwrap() = false;
    let mut a = seat(&mut w, "A", &[], now);
    a.drain();
    into_the_region(&mut w, &a, &mut now);
    let msgs = a.drain();
    assert!(starts(&msgs).is_empty());
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { advance: Some(_), .. })));
    assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()]);
}

fn logs_of(w: &mut World) -> Vec<String> {
    w.take_logs().iter().map(|l| l.text().to_string()).collect()
}

fn entry_to(target: &str) -> fmv_adapter_api::Exit {
    fmv_adapter_api::Exit { line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)), target: target.to_string(), arrival: Pos::new(150.0, -100.0, 0.0),
        arrival_facing: Some(0.0), arrival_hint: None, kind: fmv_adapter_api::ExitKind::Gateway }
}

/// A story the host has an entry for joins like everyone (the same first messages, in the entry area) and is then sent on to
/// where its story stands, through the ordinary area handshake; every other story stays where everyone joins.
#[test]
fn a_story_that_stands_elsewhere_is_sent_there_at_login_and_no_other_story_is() {
    let now = Instant::now();
    let (mut w, mut saved_rx, host) = world();
    *host.entry.lock().unwrap() = Some(entry_to(YARD));
    let mut new = seat(&mut w, "New", &[], now);
    assert_eq!(w.area_of(new.entity), Some((HALL.to_string(), false)), "a new account joins where everyone joins");
    assert!(new.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
    let mut other = seat(&mut w, "Other", &[5], now);
    assert_eq!(w.area_of(other.entity), Some((HALL.to_string(), false)), "a story the host has no entry for");
    assert!(other.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));

    let mut far = seat(&mut w, "Far", FAR_ALONG, now);
    let msgs = far.drain();
    let snapshot = msgs.iter().position(|m| matches!(m, ServerMsg::AreaSnapshot { .. })).expect("the entry area's snapshot first, as for everyone");
    let change = msgs.iter().position(|m| matches!(m, ServerMsg::AreaChange { .. })).expect("then the way on");
    assert!(snapshot < change);
    assert_eq!(w.transit_target(far.entity), Some(YARD.to_string()));
    assert!(logs_of(&mut w).iter().any(|l| l.starts_with("story entry account=") && l.contains("to=yard")), "the log says where and why");
    assert!(saved(&mut saved_rx).is_empty(), "coming in changes no story");
}

/// An entry that cannot be hosted leaves the player where everyone joins, and says so.
#[test]
fn an_entry_that_cannot_be_hosted_leaves_the_player_where_everyone_joins() {
    let now = Instant::now();
    let (mut w, _saved_rx, host) = world();
    *host.entry.lock().unwrap() = Some(entry_to("missing-field"));
    let mut far = seat(&mut w, "Far", FAR_ALONG, now);
    assert_eq!(w.area_of(far.entity), Some((HALL.to_string(), false)));
    assert!(far.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
    assert!(logs_of(&mut w).iter().any(|l| l.starts_with("story entry unavailable")));
    // ... and an entry that IS the entry area is no travel at all.
    *host.entry.lock().unwrap() = Some(entry_to(HALL));
    let mut home = seat(&mut w, "Home", FAR_ALONG, now);
    assert_eq!(w.area_of(home.entity), Some((HALL.to_string(), false)));
    assert!(home.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
}

#[test]
fn finished_scene_offers_validated_travel_once_after_release() {
    use fmv_adapter_api::{Exit, ExitKind};
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = world();
    *host.travel.lock().unwrap() = Some(Exit { line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)),
        target: HALL.to_string(), arrival: Pos::new(200.0, 0.0, 0.0), arrival_facing: Some(0.0),
        arrival_hint: None, kind: ExitKind::Gateway });
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    w.press(a.entity, Some(trigger_id()), now);
    let run = starts(&a.drain())[0].0;
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let msgs = a.drain();
    let ended = msgs.iter().position(|m| matches!(m, ServerMsg::SceneEnded { .. })).unwrap();
    let offer = msgs.iter().position(|m| matches!(m, ServerMsg::AreaChange { .. })).unwrap();
    assert!(ended < offer);
    assert_eq!(saved(&mut saved_rx), [vec![5u8]]);
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
}

#[test]
fn debug_preview_never_performs_a_scene_jump_or_saves_it() {
    use fmv_adapter_api::{Exit, ExitKind};
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = world();
    *host.travel.lock().unwrap() = Some(Exit { line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)),
        target: HALL.to_string(), arrival: Pos::new(200.0, 0.0, 0.0), arrival_facing: Some(0.0),
        arrival_hint: None, kind: ExitKind::Gateway });
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    w.players.get_mut(&a.entity).unwrap().preview = Some(Preview { phase: preview_phase(a.account, SCENE) });
    w.press(a.entity, Some(trigger_id()), now);
    let run = starts(&a.drain())[0].0;
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
    assert!(saved(&mut saved_rx).is_empty());
}

#[test]
fn unavailable_scene_destination_does_not_advance_story_or_move_player() {
    use fmv_adapter_api::{Exit, ExitKind};
    let mut now = Instant::now();
    let (mut w, mut saved_rx, host) = world();
    *host.travel.lock().unwrap() = Some(Exit { line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)),
        target: "missing-field".to_string(), arrival: Pos::new(0.0, 0.0, 0.0), arrival_facing: None,
        arrival_hint: None, kind: ExitKind::Gateway });
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    w.press(a.entity, Some(trigger_id()), now);
    let run = starts(&a.drain())[0].0;
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. } | ServerMsg::StoryUpdate { .. })));
    assert!(saved(&mut saved_rx).is_empty());
    assert!(w.players.get(&a.entity).unwrap().progression.is_empty());
    assert_eq!(w.area_of(a.entity), Some((YARD.to_string(), false)));
}

struct Seat {
    entity: EntityId,
    account: AccountId,
    rx: mpsc::Receiver<Outbound>,
}

impl Seat {
    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            out.push((*m).clone());
        }
        out
    }
}

fn world_with(preview: bool, battles: bool) -> (World, mpsc::UnboundedReceiver<Msg>, Arc<FakeHost>) {
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::new());
    let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter);
    let (handle, rx) = SaverHandle::channel();
    w.set_progress_saver(handle);
    let host = Arc::new(FakeHost::default());
    w.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: preview, debug_new_story: None }));
    if battles {
        let settings = BattleSettings { battle_speed: 0, ticks_per_second: 1000, fixed_seed: Some(7), ..BattleSettings::new(Arc::new(SyntheticRules::default())) };
        w.configure_battle(Some(settings), true);
    }
    (w, rx, host)
}

fn world() -> (World, mpsc::UnboundedReceiver<Msg>, Arc<FakeHost>) {
    world_with(false, false)
}

/// These rooms intentionally keep one phase for different stories, as a field can when
/// its walkable layout is unchanged. The ordinary rooms instead use each blob as its phase.
struct FixedPhaseRooms;
struct FixedPhaseRoom(Box<dyn fmv_adapter_api::AreaRuntime>);

impl GameAdapter for FixedPhaseRooms {
    fn game(&self) -> fmv_core::GameId { RoomsAdapter::new().game() }
    fn list_areas(&self) -> Vec<fmv_adapter_api::AreaInfo> { RoomsAdapter::new().list_areas() }
    fn load_area(&self, key: &str) -> Result<Box<dyn fmv_adapter_api::AreaRuntime>, fmv_adapter_api::AdapterError> {
        Ok(Box::new(FixedPhaseRoom(RoomsAdapter::new().load_area(key)?)))
    }
}

impl fmv_adapter_api::AreaRuntime for FixedPhaseRoom {
    fn info(&self) -> &fmv_adapter_api::AreaInfo { self.0.info() }
    fn spawn_points(&self) -> Vec<Pos> { self.0.spawn_points() }
    fn validate_move(&self, from: Pos, to: Pos) -> fmv_adapter_api::MoveResult { self.0.validate_move(from, to) }
    fn bounds(&self) -> fmv_adapter_api::Bounds { self.0.bounds() }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.0.walk_triangles() }
    fn max_speed(&self) -> f32 { self.0.max_speed() }
    fn exits(&self) -> Vec<fmv_adapter_api::Exit> { self.0.exits() }
    fn npcs(&self) -> Vec<fmv_adapter_api::NpcSpawn> { self.0.npcs() }
    fn phase_key(&self, _progress: &[u8]) -> Option<fmv_core::PhaseKey> { Some(fmv_core::PhaseKey::default()) }
}

fn seat(w: &mut World, name: &str, progression: &[u8], now: Instant) -> Seat {
    let (session, entity) = w.allocate_ids();
    let (tx, rx) = mpsc::channel(256);
    let close = Arc::new(watch::channel(false).0);
    let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
    let account = AccountId(entity.0);
    w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now).expect("joins");
    Seat { entity, account, rx }
}

fn go(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
    *now += Duration::from_millis(700);
    w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
}

/// Walk `s` through the hall's exit into the yard and let it in; returns what it was told on arriving.
fn into_the_yard(w: &mut World, s: &mut Seat, now: &mut Instant) -> Vec<ServerMsg> {
    assert_eq!(go(w, s, 100.0, 150.0, now), MoveOutcome::Applied);
    assert_eq!(go(w, s, 350.0, 150.0, now), MoveOutcome::Applied);
    let offer = s.drain().into_iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).expect("an offer");
    let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
    assert_eq!(w.join_area(s.entity, &area, &fingerprint, &provenance, *now), crate::world::TravelReply::Done);
    s.drain()
}

/// The seat walked up to the trigger (within 50 units of it).
fn up_to_the_trigger(w: &mut World, s: &mut Seat, now: &mut Instant) {
    into_the_yard(w, s, now);
    assert_eq!(go(w, s, 150.0, -100.0, now), MoveOutcome::Applied);
    s.drain();
}

fn trigger_id() -> u32 {
    SCENE_NPC_ID_BASE
}

type Started = (SceneRunId, SceneRole, Vec<(u64, u8)>);

fn starts(msgs: &[ServerMsg]) -> Vec<Started> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMsg::SceneStart { run, you, participants, .. } => Some((*run, *you, participants.iter().map(|p| (p.entity.0, p.spot)).collect())),
            _ => None,
        })
        .collect()
}

fn ends(msgs: &[ServerMsg]) -> Vec<SceneOutcome> {
    msgs.iter().filter_map(|m| if let ServerMsg::SceneEnded { outcome, .. } = m { Some(*outcome) } else { None }).collect()
}

fn saved(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let Msg::Story(job) = msg {
            out.push(job.progress);
        }
    }
    out
}

type SceneAcknowledgement = tokio::sync::oneshot::Sender<Result<crate::saver::SceneSaved, crate::saver::SceneSaveError>>;

fn pending_job(rx: &mut mpsc::UnboundedReceiver<Msg>) -> (crate::saver::SceneSaveJob, SceneAcknowledgement) {
    let Msg::Scene { job, done } = rx.try_recv().expect("one queued transaction") else { panic!("a scene transaction, not an ordinary save") };
    assert!(rx.try_recv().is_err(), "queued only once");
    (job, done)
}

fn scene_ack(w: &World, account: AccountId, story: &[u8], gil: u32) -> crate::saver::SceneSaved {
    let game = w.areas.get(&w.home).unwrap().id().game;
    let mut character = fmv_persist::CharacterProgress::new(account, game, 0, 5, 1000);
    character.gil = gil;
    character.exp = 52;
    character.ap = 7;
    character.limit = 99;
    character.inventory = Some(vec![fmv_core::InventoryEntry::new(2, 3)]);
    crate::saver::SceneSaved { commit: fmv_persist::SceneCommit::Applied, pair: fmv_persist::ProgressPair {
        story: fmv_persist::Progression::new(account, game, story.to_vec(), 1000), character: Some(character),
    } }
}

#[test]
fn durable_scene_holds_everyone_until_ack_then_publishes_before_release_and_travel() {
    let (mut w, mut queue, host) = world_with(false, true);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    let mut b = seat(&mut w, "B", &[], now);
    party(&mut w, &a, &b, now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    up_to_the_trigger(&mut w, &mut b, &mut now);
    w.players.get_mut(&a.entity).unwrap().progress.as_mut().unwrap().gil = 1;
    w.players.get_mut(&b.entity).unwrap().progress.as_mut().unwrap().gil = 9;
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 1, next_gil: 0 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now);
    for msgs in [a.drain(), b.drain()] {
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::SceneStart { gil: 1, .. })), "every participant gets the owner's resource snapshot");
    }
    let run = SceneRunId(1);
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let (job, done) = pending_job(&mut queue);
    assert_eq!((job.account, job.expected_gil, job.next_gil, job.expected_story, job.next_story), (a.account, 1, 0, vec![], vec![5]));
    assert_eq!(*host.resources.lock().unwrap(), [1]);
    assert!(w.in_scene(a.entity) && w.in_scene(b.entity));
    assert_eq!(go(&mut w, &a, 160.0, -100.0, &mut now), MoveOutcome::Frozen);
    assert_eq!(w.players.get(&a.entity).unwrap().progress.as_ref().unwrap().gil, 1);
    assert!(a.drain().is_empty() && b.drain().is_empty());
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: true }, now);
    w.scene_message(a.entity, ClientMsg::SceneLeave { run }, now);
    w.advance_scenes(now + SCENE_MAX_TIME);
    assert!(queue.try_recv().is_err() && w.scene_count() == 1, "duplicates, leave and timeouts cannot enqueue or cancel a commit");
    done.send(Ok(scene_ack(&w, a.account, &[5], 0))).unwrap();
    w.advance_scenes(now);
    assert_eq!(w.scene_count(), 0);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, [5]);
    let p = w.players.get(&a.entity).unwrap().progress.unwrap();
    assert_eq!((p.gil, p.exp, p.ap, p.limit), (0, 52, 7, 99));
    assert_eq!(w.players.get(&a.entity).unwrap().inventory.as_ref().unwrap().entries(), [fmv_core::InventoryEntry::new(2, 3)]);
    let msgs = a.drain();
    let pos = |test: fn(&ServerMsg) -> bool| msgs.iter().position(test).unwrap();
    let ended = pos(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. }));
    assert!(pos(|m| matches!(m, ServerMsg::StoryUpdate { .. })) < ended);
    assert!(pos(|m| matches!(m, ServerMsg::CharacterProgress { .. })) < ended);
    assert!(pos(|m| matches!(m, ServerMsg::Inventory { .. })) < ended);
    assert!(ended < pos(|m| matches!(m, ServerMsg::AreaChange { .. })));
    assert!(b.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
    assert!(queue.try_recv().is_err(), "publishing an acknowledged pair never queues another ordinary story save");
}

#[test]
fn durable_scene_refused_preconditions_never_publish_or_travel_and_release_watchers() {
    for error in [crate::saver::SceneSaveError::StaleStory, crate::saver::SceneSaveError::StaleGil,
        crate::saver::SceneSaveError::InvalidRequest,
        crate::saver::SceneSaveError::Store(fmv_persist::PersistError::TransactionConflict("different selected outcome".into()))] {
        let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
        *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
        *host.travel.lock().unwrap() = Some(entry_to(HALL));
        w.press(a.entity, Some(trigger_id()), now);
        a.drain(); b.drain();
        w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
        let (_, done) = pending_job(&mut queue);
        done.send(Err(error)).unwrap();
        w.advance_scenes(now);
        assert!(!w.players.contains_key(&a.entity), "the owner must reload authoritative state");
        assert!(!w.in_scene(b.entity));
        assert!(!w.scenes.storage_failed && w.players.contains_key(&b.entity), "known no-write refusals leave the store usable");
        let owner = a.drain();
        assert!(owner.iter().all(|m| !matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::AreaChange { .. } | ServerMsg::SceneEnded { .. })));
        assert!(owner.iter().any(|m| matches!(m, ServerMsg::Reject { code: fmv_protocol::RejectCode::Internal, .. })));
        let watcher = b.drain();
        assert!(watcher.iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Aborted, .. })));
        assert!(watcher.iter().all(|m| !matches!(m, ServerMsg::AreaChange { .. })));
        assert!(queue.try_recv().is_err());
    }
}

#[test]
fn durable_scene_without_a_saver_refuses_before_releasing_success() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    w.saver = None;
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert!(queue.try_recv().is_err());
    assert!(!w.players.contains_key(&a.entity) && !w.in_scene(b.entity));
    assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. } | ServerMsg::StoryUpdate { .. })));
}

#[test]
fn a_stopped_real_saver_stops_every_player_when_a_scene_cannot_be_enqueued() {
    let (mut w, queue, host, mut a, mut b, now) = two_at_the_trigger();
    drop(queue);
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert_eq!(w.player_count(), 0);
    assert!(w.scenes.storage_failed);
    assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. } | ServerMsg::StoryUpdate { .. })));
}

#[test]
fn an_oversized_scene_result_is_refused_without_stopping_the_healthy_saver() {
    let (mut w, mut queue, _host, mut a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    let run = SceneRunId(1);
    let record = w.scenes.runs.remove(&run).unwrap();
    let result = SceneResult { progress: vec![0; fmv_protocol::MAX_STORY_PROGRESS_BYTES + 1], changed: true,
        summary: "too large".into(), travel: None, transaction: Some(SceneTransaction { expected_gil: 0, next_gil: 0 }) };
    w.queue_scene_save(run, record, result, SceneOutcome::Finished);
    assert!(queue.try_recv().is_err());
    assert!(!w.scenes.storage_failed && w.players.contains_key(&b.entity));
    assert!(!w.players.contains_key(&a.entity) && !w.in_scene(b.entity));
}

#[test]
fn durable_scene_watcher_can_leave_while_its_owner_is_waiting_for_storage() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    let run = SceneRunId(1);
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    w.scene_message(b.entity, ClientMsg::SceneLeave { run }, now);
    assert!(!w.in_scene(b.entity) && w.in_scene(a.entity));
    assert_eq!(ends(&b.drain()), [SceneOutcome::Skipped]);
    done.send(Ok(scene_ack(&w, a.account, &[5], 0))).unwrap();
    w.advance_scenes(now);
    assert!(b.drain().iter().all(|m| !matches!(m, ServerMsg::SceneEnded { .. } | ServerMsg::AreaChange { .. })));
}

fn reconnect(w: &mut World, account: AccountId, story: &[u8]) -> (NewPlayer, mpsc::Receiver<Outbound>) {
    let (session, entity) = w.allocate_ids();
    let (tx, rx) = mpsc::channel(256);
    (NewPlayer { session, entity, account, avatar: AvatarAppearance { display_name: "reconnected".into(), tint: [1, 2, 3], appearance_ref: None },
        progression: story.to_vec(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close: Arc::new(watch::channel(false).0) }, rx)
}

#[test]
fn debug_new_story_queues_its_baseline_before_the_player_can_start_a_scene() {
    let (mut w, mut queue, _host) = world();
    w.scenes.debug_new_story = Some(MADE_UP.to_vec());
    let now = Instant::now();
    let a = seat(&mut w, "debug fresh", &[], now);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, MADE_UP);
    let Some(Msg::Story(job)) = queue.try_recv().ok() else { panic!("debug baseline was not queued before join returned") };
    assert_eq!((job.account, job.progress), (a.account, MADE_UP.to_vec()));
    let b = seat(&mut w, "existing story", &[5], now);
    assert_eq!(w.players.get(&b.entity).unwrap().progression, [5]);
    assert!(queue.try_recv().is_err(), "an existing story gets no debug initialization write");
}

#[test]
fn debug_new_story_does_not_initialize_an_empty_coherent_token_login() {
    let (mut w, mut queue, _host) = world();
    w.scenes.debug_new_story = Some(MADE_UP.to_vec());
    let account = AccountId(123);
    let (mut new, _rx) = reconnect(&mut w, account, &[]);
    let entity = new.entity;
    let state = fmv_persist::SceneState {
        pair: fmv_persist::ProgressPair {
            story: fmv_persist::Progression::new(account, w.home_game(), Vec::new(), 0),
            character: None,
        },
        checkpoint: None,
        reservation: None,
    };
    new.scene_state = Some(state.clone());
    new.scene_group = vec![state];
    w.join(new, Instant::now()).unwrap();
    assert!(w.players.get(&entity).unwrap().progression.is_empty());
    assert!(queue.try_recv().is_err(), "a token login never queues a new-account baseline");
}

#[test]
fn debug_new_story_refuses_admission_if_its_configured_saver_has_stopped() {
    let (mut w, queue, _host) = world();
    w.scenes.debug_new_story = Some(MADE_UP.to_vec());
    drop(queue);
    let account = AccountId(123);
    let (new, mut rx) = reconnect(&mut w, account, &[]);
    assert_eq!(w.join(new, Instant::now()), Err(crate::world::JoinError::StorageUnavailable));
    assert_eq!(w.player_count(), 0);
    assert!(!w.is_online(account));
    assert!(rx.try_recv().is_err(), "no area or story was published for the refused join");
}

#[test]
fn debug_new_story_still_works_in_a_fixture_without_any_saver() {
    let (mut w, queue, _host) = world();
    w.scenes.debug_new_story = Some(MADE_UP.to_vec());
    w.saver = None;
    drop(queue);
    let a = seat(&mut w, "ephemeral debug", &[], Instant::now());
    assert_eq!(w.players.get(&a.entity).unwrap().progression, MADE_UP);
}

#[test]
fn a_disconnected_pending_owner_is_reserved_and_stale_admission_is_fenced_even_after_ack() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    let before = w.scene_save_epoch();
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    let during = w.scene_save_epoch();
    assert_ne!(before, during);
    assert!(w.leave(a.entity));
    assert!(w.is_online(a.account), "queued persistence reserves the account after disconnect");
    assert!(!w.has_session(a.account), "reservation-only recovery is distinct from a connected duplicate");
    let (new, _rx) = reconnect(&mut w, a.account, &[]);
    assert_eq!(w.join(new, now), Err(crate::world::JoinError::AlreadyOnline));
    let acknowledged = scene_ack(&w, a.account, &[5], 0);
    done.send(Ok(acknowledged)).unwrap();
    w.advance_scenes(now);
    assert!(!w.is_online(a.account) && w.scene_count() == 0);
    assert_eq!(ends(&b.drain()), [SceneOutcome::Finished]);
    assert!(w.players.get(&b.entity).unwrap().transit.is_none(), "an absent owner cannot move its watcher to a stale destination");
    for snapshot in [before, during] {
        let (new, _rx) = reconnect(&mut w, a.account, &[]);
        assert_eq!(w.join_with_scene_epoch(new, now, snapshot), Err(crate::world::JoinError::ProgressChanged));
    }
    let fresh = w.scene_save_epoch();
    let (new, _rx) = reconnect(&mut w, a.account, &[5]);
    let entity = new.entity;
    w.join_with_scene_epoch(new, now, fresh).expect("freshly loaded committed story can join");
    assert_eq!(w.players.get(&entity).unwrap().progression, [5]);
    assert!(queue.try_recv().is_err());
}

#[test]
fn uncertain_scene_storage_disconnects_every_player_and_refuses_admission_until_recovery() {
    for store_error in [false, true] {
        let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
        let mut unrelated = seat(&mut w, "unrelated", &[], now);
        unrelated.drain();
        *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
        w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
        w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
        let (_, done) = pending_job(&mut queue);
        if store_error { done.send(Err(crate::saver::SceneSaveError::Store(fmv_persist::PersistError::TransactionRecoveryRequired))).unwrap(); }
        else { drop(done); }
        w.advance_scenes(now);
        assert_eq!(w.player_count(), 0, "every player must stop when the shared store cannot save");
        for msgs in [a.drain(), b.drain(), unrelated.drain()] {
            assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Reject { code: fmv_protocol::RejectCode::Internal, .. })));
            assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::AreaChange { .. } | ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
        }
        let epoch = w.scene_save_epoch();
        let (new, _rx) = reconnect(&mut w, a.account, &[5]);
        assert_eq!(w.join_with_scene_epoch(new, now, epoch), Err(crate::world::JoinError::StorageUnavailable));
    }
}

#[test]
fn a_refused_scene_interpretation_cannot_be_reported_as_success() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    *host.refuse.lock().unwrap() = true;
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert!(queue.try_recv().is_err());
    assert!(!w.players.contains_key(&a.entity) && !w.in_scene(b.entity));
    assert_eq!(ends(&b.drain()), [SceneOutcome::Aborted]);
    assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { .. } | ServerMsg::StoryUpdate { .. } | ServerMsg::AreaChange { .. })));
}

#[test]
fn skipping_before_a_scene_can_be_interpreted_aborts_without_saving_or_disconnecting() {
    let (mut w, mut queue, host) = world_with(false, true);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    w.players.get_mut(&a.entity).unwrap().progress.as_mut().unwrap().gil = 600;
    *host.refuse.lock().unwrap() = true;
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    let before = {
        let p = w.players.get(&a.entity).unwrap();
        (p.progression.clone(), p.progress, p.inventory.as_ref().unwrap().entries().to_vec(), p.snapshot.pos, p.area.clone())
    };
    w.press(a.entity, Some(trigger_id()), now);
    let run = starts(&a.drain())[0].0;
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: true }, now);
    assert!(queue.try_recv().is_err());
    assert!(w.is_online(a.account) && !w.in_scene(a.entity));
    assert!(!w.scenes.storage_failed);
    let p = w.players.get(&a.entity).unwrap();
    assert_eq!((p.progression.clone(), p.progress, p.inventory.as_ref().unwrap().entries().to_vec(), p.snapshot.pos, p.area.clone()), before);
    assert!(p.transit.is_none());
    let msgs = a.drain();
    assert_eq!(ends(&msgs), [SceneOutcome::Aborted]);
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::Reject { .. } | ServerMsg::StoryUpdate { .. } | ServerMsg::CharacterProgress { .. } | ServerMsg::Inventory { .. } | ServerMsg::AreaChange { .. })));
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert!(queue.try_recv().is_err() && a.drain().is_empty(), "a later completion cannot revive the abandoned run");
}

#[test]
fn an_acknowledged_duplicate_uses_current_progress_without_replaying_the_old_travel() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    let mut ack = scene_ack(&w, a.account, &[7], 20);
    ack.commit = fmv_persist::SceneCommit::AlreadyApplied;
    done.send(Ok(ack)).unwrap();
    w.advance_scenes(now);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, [7]);
    assert_eq!(w.players.get(&a.entity).unwrap().progress.unwrap().gil, 20);
    for msgs in [a.drain(), b.drain()] {
        assert_eq!(ends(&msgs), [SceneOutcome::Finished]);
        assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
    }
}

#[test]
fn a_pending_destination_is_kept_loaded_until_the_scene_can_travel() {
    let (mut w, mut queue, host, mut a, mut b, now) = two_at_the_trigger();
    // Both participants are in the yard: choose it as the protected entry area so the empty
    // hall is an unloadable destination in this fixture.
    w.home = YARD.into();
    w.set_unload_grace(Duration::ZERO);
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now); a.drain(); b.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    w.manage_areas(now + SCENE_MAX_TIME);
    assert!(w.areas.contains_key(HALL), "the validated destination stays pinned during storage latency");
    w.source = None; // A second load would fail; the prepared destination must be retained.
    done.send(Ok(scene_ack(&w, a.account, &[5], 0))).unwrap();
    w.advance_scenes(now + SCENE_MAX_TIME);
    assert!(a.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
}

#[test]
fn previews_use_the_resource_snapshot_but_never_queue_the_returned_transaction() {
    let (mut w, mut queue, host) = world_with(false, true);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    let player = w.players.get_mut(&a.entity).unwrap();
    player.preview = Some(Preview { phase: preview_phase(a.account, SCENE) });
    player.progress.as_mut().unwrap().gil = 3;
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 3, next_gil: 2 });
    *host.travel.lock().unwrap() = Some(entry_to(HALL));
    w.press(a.entity, Some(trigger_id()), now); a.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert_eq!(*host.resources.lock().unwrap(), [3]);
    assert!(queue.try_recv().is_err());
    assert_eq!(w.players.get(&a.entity).unwrap().progress.unwrap().gil, 3);
    assert_eq!(w.scene_count(), 0);
    assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::AreaChange { .. })));
}

#[test]
fn the_saved_mirror_keeps_the_active_character_slot_and_clamps_game_caps() {
    let (mut w, mut queue, host) = world_with(false, true);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    let slot = w.players.get(&a.entity).unwrap().progress.unwrap().character;
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    w.press(a.entity, Some(trigger_id()), now); a.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    let mut ack = scene_ack(&w, a.account, &[5], u32::MAX);
    let character = ack.pair.character.as_mut().unwrap();
    character.character = slot.wrapping_add(1);
    character.level = u8::MAX; character.exp = u32::MAX; character.ap = u32::MAX; character.limit = u16::MAX;
    done.send(Ok(ack)).unwrap();
    w.advance_scenes(now);
    let got = w.players.get(&a.entity).unwrap().progress.unwrap();
    let caps = w.battle.as_ref().unwrap().rules().progress_limits();
    assert_eq!((got.character, got.level, got.exp, got.ap, got.limit), (slot, caps.max_level, caps.max_exp, caps.max_ap, caps.max_limit));
    assert_eq!(got.gil, u32::MAX, "the durable authored gil result stays exact");
}

#[test]
fn a_newer_resource_mirror_is_never_overwritten_by_a_delayed_scene_acknowledgment() {
    let (mut w, mut queue, host) = world_with(false, true);
    let mut now = Instant::now();
    let mut a = seat(&mut w, "A", &[], now);
    up_to_the_trigger(&mut w, &mut a, &mut now);
    *host.transaction.lock().unwrap() = Some(SceneTransaction { expected_gil: 0, next_gil: 0 });
    w.press(a.entity, Some(trigger_id()), now); a.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let (_, done) = pending_job(&mut queue);
    w.players.get_mut(&a.entity).unwrap().progress.as_mut().unwrap().gil = 50;
    done.send(Ok(scene_ack(&w, a.account, &[5], 0))).unwrap();
    w.advance_scenes(now);
    assert!(!w.players.contains_key(&a.entity), "reload when any resource changed during the save");
    assert!(!a.drain().iter().any(|m| matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::CharacterProgress { .. } | ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
}

fn party(w: &mut World, leader: &Seat, member: &Seat, now: Instant) {
    assert!(matches!(w.party_invite(leader.entity, member.entity, now), PartyOutcome::Done));
    let invite = w.parties.invites().next().map(|i| i.id).expect("an invite");
    assert!(matches!(w.party_accept(member.entity, invite, now), PartyOutcome::Done));
}

fn log_text(w: &mut World) -> String {
    w.take_logs().iter().map(|l| l.text().to_string()).collect::<Vec<_>>().join("\n")
}

// ---- the trigger -----------------------------------------------------------------------------------------

#[test]
fn the_scene_npc_joins_the_cast_where_the_scene_is_on_offer_and_nowhere_else() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let hall = a.drain();
    assert!(hall.iter().all(|m| !matches!(m, ServerMsg::AreaSnapshot { npcs, .. } if !npcs.is_empty())), "no scene NPC in the hall");
    let mut now = t0;
    let arrived = into_the_yard(&mut w, &mut a, &mut now);
    let npcs: Vec<NpcSnapshot> = arrived.into_iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs) } else { None }).unwrap();
    // the yard's own character (id 0) is still there, and the scene's trigger joins it under an id of its own range
    assert_eq!(npcs.iter().filter(|n| n.id < SCENE_NPC_ID_BASE).count(), 1);
    let scene_npcs: Vec<&NpcSnapshot> = npcs.iter().filter(|n| n.id >= SCENE_NPC_ID_BASE).collect();
    assert_eq!(scene_npcs.len(), 1);
    let n = scene_npcs[0];
    assert_eq!((n.id, n.name.as_str(), n.model.as_str(), n.idle_animation.as_deref()), (trigger_id(), "giver", "aagb", Some("abcd")));
    assert_eq!((n.pos.x, n.pos.y), TRIGGER_AT);

    // an account whose story the scene is not on offer for is not shown it
    let mut b = seat(&mut w, "B", &[7], t0);
    let b_arrived = into_the_yard(&mut w, &mut b, &mut now);
    let b_npcs: Vec<NpcSnapshot> = b_arrived.into_iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs) } else { None }).unwrap();
    assert!(b_npcs.iter().all(|n| n.id < SCENE_NPC_ID_BASE), "story [7] is not offered the scene");
    assert_eq!(w.scene_npcs(a.entity, &YARD.to_string()).len(), 1);
    assert!(w.scene_npcs(a.entity, &HALL.to_string()).is_empty(), "the scene plays in the yard only");
}

#[test]
fn pressing_ok_next_to_the_trigger_starts_the_scene_for_that_player_alone_and_holds_it() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut other = seat(&mut w, "Other", &[], t0);
    let mut now = t0;
    into_the_yard(&mut w, &mut a, &mut now);
    // far from the trigger (the yard arrival is 316 units from it): no scene
    w.press(a.entity, Some(trigger_id()), now);
    assert_eq!(w.scene_count(), 0);
    assert_eq!(go(&mut w, &a, 150.0, -100.0, &mut now), MoveOutcome::Applied);
    a.drain();
    // a press naming nothing the area or the scenes have is no scene
    w.press(a.entity, Some(trigger_id() + 5), now);
    assert_eq!(w.scene_count(), 0);
    // next to it, naming it
    w.press(a.entity, Some(trigger_id()), now);
    assert_eq!(w.scene_count(), 1);
    let msgs = a.drain();
    assert_eq!(starts(&msgs), [(SceneRunId(1), SceneRole::Owner, vec![(a.entity.0, 0)])]);
    // the player the scene is for is held: it moves nowhere, presses nothing and starts no second scene
    assert!(w.in_scene(a.entity));
    assert_eq!(go(&mut w, &a, 140.0, 100.0, &mut now), MoveOutcome::Frozen);
    w.press(a.entity, Some(trigger_id()), now);
    assert_eq!(w.scene_count(), 1);
    // nobody else was told anything
    assert!(other.drain().iter().all(|m| !matches!(m, ServerMsg::SceneStart { .. } | ServerMsg::SceneEnded { .. })));
    let log = log_text(&mut w);
    assert!(log.contains(&format!("scene started run=1 scene={SCENE} owner={} watchers=[] preview=false", a.entity.0)), "{log}");
}

#[test]
fn a_scene_that_is_not_on_offer_for_the_story_does_not_start() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world();
    let mut b = seat(&mut w, "B", &[7], t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut b, &mut now);
    w.press(b.entity, Some(trigger_id()), now);
    assert_eq!((w.scene_count(), w.in_scene(b.entity)), (0, false));
}

// ---- party members watch ---------------------------------------------------------------------------------

/// Two party members next to the trigger, A the leader.
fn two_at_the_trigger() -> (World, mpsc::UnboundedReceiver<Msg>, Arc<FakeHost>, Seat, Seat, Instant) {
    let t0 = Instant::now();
    let (mut w, saved, host) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    party(&mut w, &a, &b, t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut a, &mut now);
    up_to_the_trigger(&mut w, &mut b, &mut now);
    (w, saved, host, a, b, now)
}

#[test]
fn party_members_in_the_same_instance_and_in_range_watch_in_step_and_only_the_owner_drives() {
    let (mut w, _saved, _, mut a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    let run = SceneRunId(1);
    assert_eq!(starts(&a.drain()), [(run, SceneRole::Owner, vec![(a.entity.0, 0), (b.entity.0, 1)])]);
    assert_eq!(starts(&b.drain()), [(run, SceneRole::Watcher, vec![(a.entity.0, 0), (b.entity.0, 1)])]);
    assert!(w.in_scene(b.entity), "a watcher is held too");
    // the owner moves past window 0 (a question, answered 2): the watcher is told, the owner is not
    w.scene_message(a.entity, ClientMsg::SceneStep { run, step: 0, answer: Some(2) }, now);
    assert_eq!(b.drain(), [ServerMsg::SceneStep { run, step: 0, answer: Some(2) }]);
    assert!(a.drain().is_empty());
    // out of order, repeated, from a watcher, or for a run that does not exist: dropped
    for (who, step) in [(a.entity, 0u32), (a.entity, 5), (b.entity, 1)] {
        w.scene_message(who, ClientMsg::SceneStep { run, step, answer: None }, now);
    }
    w.scene_message(a.entity, ClientMsg::SceneStep { run: SceneRunId(99), step: 1, answer: None }, now);
    assert!(b.drain().is_empty(), "nothing out of order is relayed");
    w.scene_message(a.entity, ClientMsg::SceneStep { run, step: 1, answer: None }, now);
    assert_eq!(b.drain(), [ServerMsg::SceneStep { run, step: 1, answer: None }]);
    // a watcher may leave: it is released alone and the owner's scene goes on
    w.scene_message(b.entity, ClientMsg::SceneLeave { run }, now);
    assert_eq!(ends(&b.drain()), [SceneOutcome::Skipped]);
    assert!(!w.in_scene(b.entity) && w.in_scene(a.entity) && w.scene_count() == 1);
    // a watcher that is not the owner cannot end it
    w.scene_message(b.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    assert_eq!(w.scene_count(), 1);
}

#[test]
fn a_party_member_out_of_range_or_not_there_does_not_watch() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut far = seat(&mut w, "Far", &[], t0);
    let hall = seat(&mut w, "Hall", &[], t0);
    party(&mut w, &a, &far, t0);
    party(&mut w, &a, &hall, t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut a, &mut now);
    into_the_yard(&mut w, &mut far, &mut now);
    // `far` is out of range of `a`
    w.players.get_mut(&far.entity).unwrap().snapshot.pos = Pos::new(-400.0, 0.0, 0.0);
    w.press(a.entity, Some(trigger_id()), now);
    assert_eq!(starts(&a.drain()), [(SceneRunId(1), SceneRole::Owner, vec![(a.entity.0, 0)])], "nobody watches");
    assert!(!w.in_scene(far.entity) && !w.in_scene(hall.entity));
    assert!(starts(&far.drain()).is_empty());
}

// ---- the end ---------------------------------------------------------------------------------------------

#[test]
fn when_the_owner_finishes_everyone_is_released_and_the_story_write_is_applied_once_and_saved() {
    let (mut w, mut saved_rx, host, mut a, mut b, now) = two_at_the_trigger();
    let old_instance = w.instance_of(a.account);
    w.press(a.entity, Some(trigger_id()), now);
    a.drain();
    b.drain();
    let run = SceneRunId(1);
    w.scene_message(a.entity, ClientMsg::SceneStep { run, step: 0, answer: Some(2) }, now);
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    // released, told how it ended
    let a_msgs = a.drain();
    let b_msgs = b.drain();
    assert_eq!(ends(&a_msgs), [SceneOutcome::Finished]);
    assert_eq!(ends(&b_msgs), [SceneOutcome::Finished]);
    assert_ne!(w.instance_of(a.account), old_instance, "this fixture's changed blob changes its instance");
    assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
    for msgs in [&a_msgs, &b_msgs] {
        assert_eq!(msgs.iter().filter(|msg| matches!(msg, ServerMsg::AreaSnapshot { .. })).count(), 1,
            "the instance transition sends its own snapshot, without a story-refresh duplicate");
    }
    assert!(!w.in_scene(a.entity) && !w.in_scene(b.entity) && w.scene_count() == 0);
    // the owner's story moved once: the mirror, the saver, the log
    assert_eq!(saved(&mut saved_rx), [vec![5u8]], "saved once, as the host said");
    assert_eq!(w.players.get(&a.entity).unwrap().progression, [5]);
    let calls = host.finished.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].0.as_str(), calls[0].1.as_slice(), calls[0].3.as_slice()), (SCENE, &[][..], &[2u8][..]), "the host ran it with the owner's answers");
    let log = log_text(&mut w);
    assert!(log.contains("story advanced") && log.contains(&format!("moment=1->5 scene={SCENE}")), "{log}");
    assert!(log.contains("scene ended run=1") && log.contains("outcome=Finished") && log.contains("story=applied"), "{log}");
    // a second end, a late step and the passing of time change nothing
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
    w.scene_message(a.entity, ClientMsg::SceneStep { run, step: 1, answer: None }, now);
    w.advance_scenes(now + SCENE_MAX_TIME * 2);
    assert!(saved(&mut saved_rx).is_empty(), "nothing is saved twice");
    assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::StoryUpdate { .. } | ServerMsg::SceneEnded { .. })));
    assert_eq!(host.finished.lock().unwrap().len(), 1);
    // and the player moves again
    let mut t = now;
    assert!(matches!(go(&mut w, &a, 140.0, 100.0, &mut t), MoveOutcome::Applied));
}

#[test]
fn the_owner_is_told_the_step_with_the_ordinary_story_update() {
    let (mut w, _saved, _, mut a, _b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    a.drain();
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    let updates: Vec<(Vec<u8>, Option<u32>)> = a
        .drain()
        .into_iter()
        .filter_map(|m| if let ServerMsg::StoryUpdate { progress, advance } = m { Some((progress, advance.map(|x| x.point))) } else { None })
        .collect();
    assert_eq!(updates, [(vec![5u8], Some(SCENE_STEP_POINT))]);
}

#[test]
fn a_skip_ends_it_just_the_same() {
    let (mut w, mut saved_rx, _, mut a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    let run = SceneRunId(1);
    w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: true }, now);
    assert_eq!(ends(&b.drain()), [SceneOutcome::Skipped]);
    assert_eq!(saved(&mut saved_rx).len(), 1, "a skipped scene writes the story too (the script's own writes)");
    a.drain();
}

#[test]
fn a_timeout_ends_what_nobody_ended_and_a_step_starts_the_idle_clock_again() {
    let t0 = Instant::now();
    let (mut w, mut saved_rx, _) = world();
    let mut c = seat(&mut w, "C", &[], t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut c, &mut now);
    w.press(c.entity, Some(trigger_id()), t0);
    c.drain();
    // the owner moves past a window at 100 s: the idle clock starts again from there
    w.scene_message(c.entity, ClientMsg::SceneStep { run: SceneRunId(1), step: 0, answer: None }, t0 + Duration::from_secs(100));
    w.advance_scenes(t0 + Duration::from_secs(219));
    assert_eq!(w.scene_count(), 1, "119 s after the last window is not yet too long");
    w.advance_scenes(t0 + Duration::from_secs(221));
    assert_eq!(w.scene_count(), 0);
    assert_eq!(ends(&c.drain()), [SceneOutcome::TimedOut]);
    assert!(saved(&mut saved_rx).is_empty(), "a timeout must not choose unanswered branches or advance story");
    assert!(!w.in_scene(c.entity));
}

#[test]
fn an_owner_that_cannot_play_aborts_without_saving() {
    let (mut w, mut saved_rx, _, a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    w.scene_message(a.entity, ClientMsg::SceneLeave { run: SceneRunId(1) }, now);
    assert_eq!(ends(&b.drain()), [SceneOutcome::Aborted]);
    assert!(saved(&mut saved_rx).is_empty());
    assert!(!w.in_scene(a.entity));
}

#[test]
fn the_whole_run_has_a_limit_however_busy_the_owner_is() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world();
    let mut d = seat(&mut w, "D", &[], t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut d, &mut now);
    w.press(d.entity, Some(trigger_id()), t0);
    for step in 0u32..19 {
        w.scene_message(d.entity, ClientMsg::SceneStep { run: SceneRunId(1), step, answer: None }, t0 + Duration::from_secs(u64::from(step) * 60));
    }
    w.advance_scenes(t0 + Duration::from_secs(19 * 60 + 30));
    assert_eq!(w.scene_count(), 1, "still within the twenty minutes");
    w.advance_scenes(t0 + SCENE_MAX_TIME + Duration::from_secs(1));
    assert_eq!(w.scene_count(), 0, "twenty minutes is the longest a scene lasts");
}

#[test]
fn a_scene_whose_owner_disconnects_is_dropped_without_writing_and_its_watchers_go_free() {
    let (mut w, mut saved_rx, host, a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    b.drain();
    assert!(w.leave(a.entity));
    assert_eq!(ends(&b.drain()), [SceneOutcome::Aborted]);
    assert!(!w.in_scene(b.entity) && w.scene_count() == 0);
    assert!(saved(&mut saved_rx).is_empty() && host.finished.lock().unwrap().is_empty(), "an abandoned scene writes nothing");
    assert!(log_text(&mut w).contains("outcome=Aborted"));
}

#[test]
fn a_watcher_that_disconnects_leaves_the_scene_to_the_owner() {
    let (mut w, mut saved_rx, _, mut a, b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    a.drain();
    assert!(w.leave(b.entity));
    assert_eq!((w.scene_count(), w.in_scene(a.entity)), (1, true));
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert_eq!(ends(&a.drain()), [SceneOutcome::Finished]);
    assert_eq!(saved(&mut saved_rx).len(), 1);
}

#[test]
fn a_watcher_that_leaves_the_party_stops_watching() {
    let (mut w, _saved, _, mut a, mut b, now) = two_at_the_trigger();
    w.press(a.entity, Some(trigger_id()), now);
    a.drain();
    b.drain();
    assert!(matches!(w.party_leave(b.entity), PartyOutcome::Done));
    assert_eq!(ends(&b.drain()), [SceneOutcome::Aborted]);
    assert!(!w.in_scene(b.entity) && w.in_scene(a.entity));
}

#[test]
fn a_member_in_its_leaders_phase_owns_a_scene_that_writes_nothing() {
    let (mut w, mut saved_rx, host, mut a, mut b, now) = two_at_the_trigger();
    // (as if its own phase were not the one that chose the instance)
    w.players.get_mut(&b.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
    w.press(b.entity, Some(trigger_id()), now);
    a.drain();
    b.drain();
    w.scene_message(b.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, now);
    assert!(saved(&mut saved_rx).is_empty() && host.finished.lock().unwrap().is_empty());
    let a_msgs = a.drain();
    let b_msgs = b.drain();
    assert_eq!(ends(&b_msgs), [SceneOutcome::Finished]);
    assert!(a_msgs.iter().chain(&b_msgs).all(|msg| !matches!(msg, ServerMsg::AreaSnapshot { .. })),
        "a read-only run has no changed story and does not refresh the cast");
}

#[test]
fn same_instance_story_changes_refresh_party_triggers_without_reentry_for_real_and_preview() {
    for is_preview in [false, true] {
        let mut now = Instant::now();
        let adapter: Arc<dyn GameAdapter> = Arc::new(FixedPhaseRooms);
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (handle, mut saved_rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        let host = Arc::new(FakeHost::default());
        w.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: is_preview, debug_new_story: None }));
        let mut a = seat(&mut w, "A", &[], now);
        let mut b = seat(&mut w, "B", &[3], now);
        let mut other = seat(&mut w, "Other", &[], now);
        party(&mut w, &a, &b, now);
        let mut ordinary = None;
        for s in [&mut a, &mut b, &mut other] {
            let arrived = if is_preview { preview(&mut w, s, now) } else { into_the_yard(&mut w, s, &mut now) };
            let cast = arrived.into_iter().find_map(|msg| match msg {
                ServerMsg::AreaSnapshot { npcs, .. } => Some(npcs), _ => None,
            }).expect("the arrival announced its cast");
            assert_eq!(cast.iter().filter(|npc| npc.id >= SCENE_NPC_ID_BASE).count(), 1);
            let base: Vec<_> = cast.into_iter().filter(|npc| npc.id < SCENE_NPC_ID_BASE).collect();
            assert_eq!(base.len(), 1, "the yard guard is an ordinary NPC");
            if let Some(expected) = &ordinary { assert_eq!(&base, expected); } else { ordinary = Some(base); }
            if !is_preview { assert_eq!(go(&mut w, s, 150.0, -100.0, &mut now), MoveOutcome::Applied); }
        }
        let ordinary = ordinary.unwrap();
        let instance = w.instance_of(a.account).unwrap();
        assert_eq!(w.instance_of(b.account), Some(instance));
        let roster: Vec<_> = [&a, &b, &other].into_iter().filter(|s| w.instance_of(s.account) == Some(instance))
            .map(|s| (s.entity, w.players[&s.entity].snapshot.pos)).collect();
        let member_progress = w.players[&b.entity].progression.clone();
        a.drain(); b.drain(); other.drain();
        w.press(a.entity, Some(trigger_id()), now);
        assert_eq!(starts(&a.drain())[0].1, SceneRole::Owner);
        assert_eq!(starts(&b.drain())[0].1, SceneRole::Watcher);
        let run = SceneRunId(1);
        w.scene_message(a.entity, ClientMsg::SceneEnd { run, skipped: false }, now);
        for msgs in [a.drain(), b.drain()] {
            assert_eq!(ends(&msgs), [SceneOutcome::Finished]);
            let snapshots: Vec<_> = msgs.iter().filter_map(|msg| match msg {
                ServerMsg::AreaSnapshot { area, entities, npcs, .. } => Some((area, entities, npcs)), _ => None,
            }).collect();
            assert_eq!(snapshots.len(), 1, "one fresh cast for an unchanged instance");
            let (area, entities, npcs) = snapshots[0];
            assert_eq!(area.key, YARD);
            assert_eq!(npcs, &ordinary, "only the completed trigger leaves; ordinary IDs, positions and models stay");
            assert_eq!(entities.iter().map(|entity| (entity.id, entity.pos)).collect::<Vec<_>>(), roster,
                "a full current roster preserves peers and their positions");
            assert!(msgs.iter().all(|msg| !matches!(msg, ServerMsg::PlayerJoined { .. } | ServerMsg::PlayerLeft { .. }
                | ServerMsg::AreaChange { .. } | ServerMsg::InstancePlacement { .. })), "no manufactured movement or membership event");
        }
        assert_eq!((w.instance_of(a.account), w.instance_of(b.account)), (Some(instance), Some(instance)));
        assert_eq!(w.players[&b.entity].progression, member_progress, "watching never advances the member's own story");
        assert_eq!(w.players[&a.entity].progression, if is_preview { vec![9, 5] } else { vec![5] });
        assert_eq!(saved(&mut saved_rx), if is_preview { vec![] } else { vec![vec![5u8]] }, "preview never queues a save; real story queues the owner once");
        assert_eq!(host.finished.lock().unwrap().len(), 1);
        assert!(other.drain().is_empty(), "an unrelated player gets no refresh or scene traffic");
        assert_eq!(w.scene_npcs(other.entity, &YARD.to_string()).len(), 1, "the unrelated player's trigger remains eligible");
        assert!(w.scene_npcs(b.entity, &YARD.to_string()).is_empty(), "the member sees its phase owner's completed story");
        let mut failed = Vec::new();
        w.refresh_placements(&BTreeSet::from([a.account, b.account, other.account]), &mut failed);
        assert!(failed.is_empty() && a.drain().is_empty() && b.drain().is_empty() && other.drain().is_empty(),
            "an ordinary unchanged placement refresh stays quiet");
    }
}

// ---- battles ---------------------------------------------------------------------------------------------

#[test]
fn a_player_in_a_scene_fights_nothing_and_is_not_seated_in_a_partys_battle() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world_with(false, true);
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    let c = seat(&mut w, "C", &[], t0);
    party(&mut w, &a, &b, t0);
    party(&mut w, &a, &c, t0);
    let mut now = t0;
    up_to_the_trigger(&mut w, &mut a, &mut now);
    up_to_the_trigger(&mut w, &mut b, &mut now);
    // C stays in the hall
    w.press(a.entity, Some(trigger_id()), now);
    a.drain();
    b.drain();
    assert!(w.in_scene(a.entity) && w.in_scene(b.entity));
    assert!(matches!(w.request_encounter(a.entity, None, now), BattleReply::Refused { .. }), "the owner cannot start a battle");
    // a battle C starts takes no member that is in a scene (they are in the yard anyway: the rule is the scene's, not the area's)
    w.players.get_mut(&c.entity).unwrap().area = YARD.to_string();
    assert!(matches!(w.request_encounter(c.entity, None, now), BattleReply::Done));
    let seated: Vec<bool> = [&a, &b, &c].iter().map(|s| w.players.get(&s.entity).unwrap().battle.is_some()).collect();
    assert_eq!(seated, [false, false, true], "only the player that was free fights");
}

// ---- the debug preview -----------------------------------------------------------------------------------

#[test]
fn a_preview_is_refused_when_the_server_does_not_run_them_and_for_a_scene_it_does_not_know() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world_with(false, false);
    let a = seat(&mut w, "A", &[], t0);
    assert_eq!(w.scene_message(a.entity, ClientMsg::SceneDebugPreview { scene: SCENE.into() }, t0), SceneReply::Refused(SceneRefusal::Unavailable), "off by default");
    let (mut w, _saved, _) = world_with(true, false);
    let a = seat(&mut w, "A", &[], t0);
    assert_eq!(w.scene_message(a.entity, ClientMsg::SceneDebugPreview { scene: "nope".into() }, t0), SceneReply::Refused(SceneRefusal::UnknownScene));
    assert_eq!(w.scene_message(a.entity, ClientMsg::SceneDebugPreview { scene: "x".repeat(200) }, t0), SceneReply::Refused(SceneRefusal::UnknownScene));
    assert_eq!(w.scene_message(EntityId(404), ClientMsg::SceneDebugPreview { scene: SCENE.into() }, t0), SceneReply::Gone);
    // no host at all: nothing is offered
    let mut bare = World::new(RoomsAdapter::new().load_area(HALL).unwrap()).unwrap();
    let b = seat(&mut bare, "B", &[], t0);
    assert_eq!(bare.scene_message(b.entity, ClientMsg::SceneDebugPreview { scene: SCENE.into() }, t0), SceneReply::Refused(SceneRefusal::Unavailable));
}

/// Preview the scene for `s` and let it in; returns what it was told on arriving.
fn preview(w: &mut World, s: &mut Seat, now: Instant) -> Vec<ServerMsg> {
    assert_eq!(w.scene_message(s.entity, ClientMsg::SceneDebugPreview { scene: SCENE.into() }, now), SceneReply::Done);
    let offer = s.drain().into_iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).expect("sent to the scene's field");
    let ServerMsg::AreaChange { area, fingerprint, provenance, arrival, .. } = offer else { unreachable!() };
    assert_eq!(area.key, YARD);
    assert_eq!((arrival.x, arrival.y), (150.0, -100.0), "where the scene says");
    assert_eq!(w.join_area(s.entity, &area, &fingerprint, &provenance, now), crate::world::TravelReply::Done);
    s.drain()
}

#[test]
fn a_preview_puts_the_player_in_the_scenes_field_on_a_made_up_story_and_never_saves_it() {
    let t0 = Instant::now();
    let (mut w, mut saved_rx, host) = world_with(true, false);
    // an account with a real story of its own
    let mut a = seat(&mut w, "A", &[1, 3, 0], t0);
    a.drain();
    let msgs = preview(&mut w, &mut a, t0);
    let snapshot_npcs = msgs.iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs.iter().filter(|n| n.id >= SCENE_NPC_ID_BASE).count()) } else { None });
    assert_eq!(snapshot_npcs, Some(1), "the scene's trigger is there");
    let told: Vec<&Vec<u8>> = msgs.iter().filter_map(|m| if let ServerMsg::StoryUpdate { progress, advance: None } = m { Some(progress) } else { None }).collect();
    assert_eq!(told, [&MADE_UP.to_vec()], "the client is told the story the scene plays on");
    assert_eq!(w.players.get(&a.entity).unwrap().progression, MADE_UP);
    // the scene starts from the trigger and finishes: the made-up story moves, the store is not written
    w.press(a.entity, Some(trigger_id()), t0);
    assert_eq!(w.scene_count(), 1);
    assert!(host.finished.lock().unwrap().is_empty());
    w.scene_message(a.entity, ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false }, t0);
    assert_eq!(host.finished.lock().unwrap().len(), 1);
    assert_eq!(host.finished.lock().unwrap()[0].1, MADE_UP, "it ran on the made-up story");
    assert_eq!(w.players.get(&a.entity).unwrap().progression, [9, 5], "in memory the made-up story moved");
    assert!(saved(&mut saved_rx).is_empty(), "NOTHING of a preview is saved, whatever the scene wrote");
    let log = log_text(&mut w);
    assert!(log.contains("preview=true") && log.contains("story=applied (preview: not saved)"), "{log}");
}

#[test]
fn a_story_step_of_a_previewing_player_is_never_kept_saved_or_told() {
    let t0 = Instant::now();
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_story());
    let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter);
    let (handle, mut saved_rx) = SaverHandle::channel();
    w.set_progress_saver(handle);
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    a.drain();
    b.drain();
    // (as the preview leaves a player: its own phase, and a story that is made up)
    w.players.get_mut(&a.entity).unwrap().preview = Some(Preview { phase: preview_phase(a.account, SCENE) });
    // both walk into the story region: B's step is the story's, A's is a preview's
    let mut now = t0;
    for (x, y) in [(100.0, 0.0), (200.0, 0.0)] {
        go(&mut w, &a, x, y, &mut now);
        go(&mut w, &b, x, y, &mut now);
    }
    assert!(w.players.get(&a.entity).unwrap().progression.is_empty(), "the previewing player's story did not move");
    assert_eq!(w.players.get(&b.entity).unwrap().progression, fmv_fixture::rooms::STORY_ADVANCED, "the ordinary player's did");
    assert_eq!(saved(&mut saved_rx).len(), 1, "one save: B's");
    assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::StoryUpdate { .. })));
}

#[test]
fn two_previews_are_two_instances_and_a_party_member_follows_its_leader_into_the_leaders() {
    let t0 = Instant::now();
    let (mut w, _saved, _) = world_with(true, false);
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    a.drain();
    b.drain();
    preview(&mut w, &mut a, t0);
    let b_msgs = preview(&mut w, &mut b, t0);
    assert_ne!(w.instance_of(a.account), w.instance_of(b.account), "each preview is its own player's instance");
    let seen_by_b: Vec<EntityId> = b_msgs
        .into_iter()
        .filter_map(|m| if let ServerMsg::AreaSnapshot { entities, .. } = m { Some(entities.iter().map(|e| e.id).collect::<Vec<_>>()) } else { None })
        .next_back()
        .unwrap();
    assert_eq!(seen_by_b, [b.entity], "B's preview shows B alone");
    // the phase keys are made-up ones, longer than every verbatim key and carrying their own tag
    let key = preview_phase(a.account, SCENE);
    assert_eq!((key.0.first().copied(), key.0.len()), (Some(PREVIEW_PHASE_TAG), 34));
    assert_ne!(preview_phase(a.account, SCENE), preview_phase(b.account, SCENE));
    // B joins A's party: it is placed in A's instance, which is how it can watch
    party(&mut w, &a, &b, t0);
    assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
    // and both are at the trigger: A's scene has B as its watcher
    for s in [&a, &b] {
        w.players.get_mut(&s.entity).unwrap().snapshot.pos = Pos::new(150.0, -100.0, 0.0);
    }
    a.drain();
    b.drain();
    w.press(a.entity, Some(trigger_id()), t0);
    assert_eq!(starts(&b.drain()).first().map(|s| s.1), Some(SceneRole::Watcher));
}

#[test]
fn victory_scene_cannot_delay_repeat_or_undo_the_saved_battle_step() {
    use super::super::story::StoryThen;
    use fmv_fixture::rooms::BATTLE_FORMATION;
    for skipped in [false, true] {
        let (mut w, mut saved_rx, host) = cutscene_world();
        *host.victory.lock().unwrap() = true;
        let now = Instant::now();
        let mut a = seat(&mut w, "A", &[], now);
        a.drain();
        // The battle-end path has already released the player; only a victory calls this method.
        let then = StoryThen { entity: a.entity, area: HALL.to_string(), point: 0 };
        w.story_after_battle(then.clone(), BATTLE_FORMATION, now);
        let msgs = a.drain();
        let started = starts(&msgs);
        let [(run, _, _)] = &started[..] else { panic!("one victory scene: {msgs:?}") };
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::SceneStart { progress, .. } if progress.is_empty())),
            "staged against the story before victory");
        assert!(w.in_scene(a.entity));
        assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED);
        assert_eq!(saved(&mut saved_rx), [STORY_ADVANCED.to_vec()]);
        w.story_after_battle(then, BATTLE_FORMATION, now);
        assert!(starts(&a.drain()).is_empty(), "no repeat once the step is paid");
        w.scene_message(a.entity, ClientMsg::SceneEnd { run: *run, skipped }, now);
        assert!(!w.in_scene(a.entity) && !w.in_battle(a.entity));
        assert!(saved(&mut saved_rx).is_empty());
        assert!(host.finished.lock().unwrap().is_empty(), "the presentation has no story writes");
        assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED);
    }
}
