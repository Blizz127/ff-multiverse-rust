//! Durable scene continuation over a strict scripted source: no game data, GPU or network.

use super::client::SceneClient;
use super::player::{SceneCmd, SceneEvent, SoundAction};
use super::source::{Built, SceneBuilder, SceneSource};
use fmv_core::EntityId;
use fmv_ff7::script::dialogue::{Choice, End, Message, MessageOrigin, Piece};
use fmv_ff7::script::scene::{
    BattleCheckpoint, Beat, CameraOp, MoveHow, SceneBoundary, SceneResources, Stage, Then, Timed,
    Window, WindowCheckpoint,
};
use fmv_ff7::story::StoryState;
use fmv_protocol::{BattleId, SceneParticipant, SceneReplay, SceneReplayEvent, SceneRole, SceneRunId, ServerMsg};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use winit::keyboard::KeyCode;

const AREA: &str = "arena";
const RUN: SceneRunId = SceneRunId(7);
const SOURCE: [u8; 32] = [7; 32];
const BATTLE: BattleId = BattleId(31);

#[derive(Default)]
struct Observed {
    resumes: Vec<(BattleCheckpoint, u16, u32)>,
    private_builds: usize,
}

fn window_checkpoint(ordinal: u32) -> WindowCheckpoint {
    WindowCheckpoint {
        ordinal: ordinal + 1,
        entity: 18,
        instruction_offset: Some(100 + ordinal),
        window: 0,
        dialog: Some(ordinal as u16),
        choices: (ordinal == 0).then_some([1, 2]),
    }
}

fn battle_checkpoint(ordinal: u32) -> BattleCheckpoint {
    BattleCheckpoint { ordinal: ordinal + 1, entity: 18, instruction_offset: 200 + ordinal, formation: 328 + ordinal as u16, flags: Some(8) }
}

fn window(ordinal: u32, at: u32) -> Window {
    let mut message = Message::new(
        MessageOrigin::Canon { entity: 18, dialog: ordinal as usize },
        vec![Piece::Text("words from the test".into())],
    );
    if ordinal == 0 {
        message.pieces = vec![Piece::Text("test question".into()), Piece::NewLine, Piece::Choice,
            Piece::Text("first".into()), Piece::NewLine, Piece::Choice, Piece::Text("second".into())];
        message.choice = Some(Choice { first: 1, last: 2, cursor: 1 });
    }
    Window { at, opener: 18, speaker: Some(18), id: 0, geometry: None, message }
}

fn timed(at: u32, what: Stage) -> Timed<Stage> { Timed { at, what } }

struct Scripted {
    beats: VecDeque<(Beat, SceneBoundary)>,
    boundary: Option<SceneBoundary>,
    observed: Rc<RefCell<Observed>>,
}

impl Scripted {
    fn new(observed: Rc<RefCell<Observed>>, blank_second: bool) -> Self {
        let mut second = window(1, 30);
        if blank_second { second.message.pieces = vec![Piece::Text("  ".into())]; }
        let beats = vec![
            (Beat { stage: vec![timed(0, Stage::Sound { id: 42, pan: 64 })], then: Then::Window(window(0, 0)) }, SceneBoundary::Window(window_checkpoint(0))),
            (Beat { stage: vec![
                timed(20, Stage::Place { entity: 18, x: 100, y: 20, z: None, triangle: None }),
                timed(30, Stage::Camera(CameraOp::Follow { entity: 18 })),
            ], then: Then::End { at: 30, how: End::Battle } }, SceneBoundary::Battle(battle_checkpoint(0))),
            (Beat { stage: vec![timed(0, Stage::Move { entity: 18, from: None, x: 160, y: 20, z: None,
                triangle: None, frames: 30, how: MoveHow::Walk, animation: None })], then: Then::Window(second) }, SceneBoundary::Window(window_checkpoint(1))),
            (Beat { stage: vec![], then: Then::End { at: 0, how: End::Battle } }, SceneBoundary::Battle(battle_checkpoint(1))),
            (Beat { stage: vec![], then: Then::End { at: 0, how: End::Finished } }, SceneBoundary::End(End::Finished)),
        ];
        Self { beats: beats.into(), boundary: None, observed }
    }
}

impl SceneSource for Scripted {
    fn initial(&self) -> Vec<Stage> { super::tests::places() }
    fn height_at(&self, _: i16, _: i16, _: Option<u16>) -> Option<f32> { Some(11.0) }
    fn fingerprint(&self) -> Option<[u8; 32]> { Some(SOURCE) }
    fn boundary(&self) -> Option<SceneBoundary> { self.boundary }
    fn next(&mut self, answer: Option<usize>) -> Beat {
        self.next_checked(answer).expect("the strict source is advanced at a valid boundary")
    }
    fn next_checked(&mut self, answer: Option<usize>) -> Result<Beat, String> {
        match (self.boundary, answer) {
            (None, None) => {},
            (Some(SceneBoundary::Window(WindowCheckpoint { choices: None, .. })), None) => {},
            (Some(SceneBoundary::Window(WindowCheckpoint { choices: Some([first, last]), .. })), Some(answer))
                if (usize::from(first)..=usize::from(last)).contains(&answer) => {},
            _ => return Err("wrong strict window answer or boundary".into()),
        }
        let (beat, boundary) = self.beats.pop_front().ok_or("no more beats")?;
        self.boundary = Some(boundary);
        Ok(beat)
    }
    fn resume_battle(&mut self, checkpoint: BattleCheckpoint, story: StoryState, gil: u32) -> Result<(), String> {
        if self.boundary != Some(SceneBoundary::Battle(checkpoint)) { return Err("wrong battle checkpoint".into()); }
        self.observed.borrow_mut().resumes.push((checkpoint, story.moment, gil));
        self.boundary = None;
        Ok(())
    }
}

struct Builder(Rc<RefCell<Observed>>, bool);
impl SceneBuilder for Builder {
    fn build(&mut self, _: &str, story: &StoryState, seed: u32, resources: SceneResources) -> Result<Built, String> {
        assert_eq!((story.moment, seed, resources.gil), (39, 5, Some(7)), "always rebuild from original inputs");
        Ok(Built { def: super::tests::def_in(AREA), source: Box::new(Scripted::new(Rc::clone(&self.0), self.1)) })
    }
    fn build_private(&mut self, scene: &str, story: &StoryState, seed: u32, resources: SceneResources) -> Result<Built, String> {
        self.0.borrow_mut().private_builds += 1;
        self.build(scene, story, seed, resources)
    }
    fn reach(&mut self, _: usize, _: &str) -> Option<f32> { None }
}

fn start(role: SceneRole, replay: SceneReplay) -> ServerMsg {
    ServerMsg::SceneStart {
        run: RUN, scene: "test/arena/duel".into(), seed: 5, progress: StoryState::at(39).encode(), gil: 7,
        replay: Some(replay), owner: EntityId(1), participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 }], you: role,
    }
}

fn rig(role: SceneRole, events: Vec<SceneReplayEvent>) -> (SceneClient, Rc<RefCell<Observed>>) {
    rig_with_blank(role, events, false)
}

fn rig_with_blank(role: SceneRole, events: Vec<SceneReplayEvent>, blank_second: bool) -> (SceneClient, Rc<RefCell<Observed>>) {
    let observed = Rc::new(RefCell::new(Observed::default()));
    let mut client = SceneClient::new();
    client.set_builder(Box::new(Builder(Rc::clone(&observed), blank_second)));
    client.on_message(AREA, start(role, SceneReplay { origin: None, source: SOURCE, events }));
    (client, observed)
}

fn ack_window(step: u32, answer: Option<u8>) -> SceneReplayEvent {
    SceneReplayEvent::Window { step, identity: window_checkpoint(step).identity_bytes(), answer }
}

fn victory(ordinal: u32, gil: u32) -> SceneReplayEvent {
    let checkpoint = battle_checkpoint(ordinal);
    SceneReplayEvent::Battle { ordinal: checkpoint.ordinal, identity: checkpoint.identity_bytes(), formation: checkpoint.formation,
        progress: StoryState::at(39).encode(), gil }
}

fn ready() -> SceneCmd { SceneCmd::BattleReady { run: RUN, ordinal: 1, step: 1 } }

fn at_battle(client: &mut SceneClient) {
    client.autoplay(Some(2));
    client.advance_visible(1.0, true);
    assert!(client.is_active());
    assert!(!client.player().unwrap().has_window());
}

fn begun(client: &mut SceneClient, ordinal: u32, battle_id: BattleId) {
    client.on_message(AREA, ServerMsg::SceneBattleStarted { run: RUN, ordinal, battle_id });
}

fn resumed(client: &mut SceneClient, run: SceneRunId, ordinal: u32, battle_id: BattleId, gil: u32) {
    client.on_message(AREA, ServerMsg::SceneBattleResumed { run, ordinal, battle_id, progress: StoryState::at(39).encode(), gil });
}

#[test]
fn a_battle_holds_the_cast_and_camera_and_reports_ready_once() {
    let (mut client, observed) = rig(SceneRole::Owner, vec![]);
    assert_eq!(observed.borrow().private_builds, 0, "continuations always use the fingerprinted original source");
    at_battle(&mut client);
    assert_eq!(client.take_commands(), [SceneCmd::Step { run: RUN, step: 0, answer: Some(2) }, ready()]);
    let player = client.player().unwrap();
    let before = (player.actor(18).unwrap().pos(), player.camera_clock());
    assert_eq!(before.0, Some([100.0, 20.0, 11.0]));
    for _ in 0..3 { client.advance_visible(60.0, true); client.autoplay(None); }
    let player = client.player().unwrap();
    assert_eq!((player.actor(18).unwrap().pos(), player.camera_clock()), before, "scene time is frozen through the battle");
    assert!(client.take_commands().is_empty());
    assert!(observed.borrow().resumes.is_empty());
}

#[test]
fn only_the_correlated_durable_victory_resumes_and_waits_for_the_battle_picture_to_go() {
    let (mut client, observed) = rig(SceneRole::Owner, vec![]);
    at_battle(&mut client);
    client.take_commands();
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(1.0, true);
    assert!(observed.borrow().resumes.is_empty(), "a result before its battle started is not authority");
    begun(&mut client, 1, BATTLE);
    begun(&mut client, 1, BATTLE);
    for (run, ordinal, battle) in [(SceneRunId(9), 1, BATTLE), (RUN, 2, BATTLE), (RUN, 1, BattleId(99))] {
        resumed(&mut client, run, ordinal, battle, 99);
    }
    client.advance_visible(1.0, true);
    assert!(observed.borrow().resumes.is_empty());
    resumed(&mut client, RUN, 1, BATTLE, 99);
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(10.0, false);
    assert!(!client.player().unwrap().has_window(), "a victory panel keeps the scene frozen");
    assert!(client.take_commands().is_empty());
    client.advance_visible(1.0, true);
    assert!(client.player().unwrap().has_window());
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
    resumed(&mut client, RUN, 1, BATTLE, 1);
    client.advance_visible(1.0, true);
    assert_eq!(observed.borrow().resumes.len(), 1, "a replayed acknowledgement cannot reapply resources");
    client.autoplay(None);
    assert_eq!(client.take_commands(), [SceneCmd::Step { run: RUN, step: 1, answer: None }, SceneCmd::BattleReady { run: RUN, ordinal: 2, step: 2 }]);
}

#[test]
fn a_watcher_waits_at_the_same_battle_without_reporting_ready() {
    let (mut client, observed) = rig(SceneRole::Watcher, vec![]);
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
    client.advance_visible(1.0, true);
    assert!(client.is_active() && !client.player().unwrap().has_window());
    assert!(client.take_commands().is_empty());
    begun(&mut client, 1, BATTLE);
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(1.0, true);
    assert!(client.player().unwrap().has_window());
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
    assert!(client.take_commands().is_empty());
}

#[test]
fn a_watcher_catches_up_only_to_the_announced_battle_boundary() {
    let (mut client, observed) = rig(SceneRole::Watcher, vec![]);
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
    let before = client.player().unwrap().actor(18).unwrap().pos();
    begun(&mut client, 2, BATTLE);
    assert_eq!(client.player().unwrap().actor(18).unwrap().pos(), before, "an unrelated battle cannot fast-forward a scene");
    begun(&mut client, 1, BATTLE);
    assert_eq!(client.player().unwrap().actor(18).unwrap().pos(), Some([100.0, 20.0, 11.0]));
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(1.0, true);
    assert!(client.player().unwrap().has_window());
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
    assert!(client.take_commands().is_empty());
}

#[test]
fn a_watcher_can_receive_the_next_fight_while_the_previous_result_still_lingers() {
    let (mut client, observed) = rig(SceneRole::Watcher, vec![]);
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
    client.advance_visible(1.0, true);
    begun(&mut client, 1, BATTLE);
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(1.0, false);
    // The owner dismissed its victory screen first and has already reached the next battle.
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: None });
    let next_battle = BattleId(BATTLE.0 + 1);
    begun(&mut client, 2, next_battle);
    resumed(&mut client, RUN, 2, next_battle, 123);
    client.advance_visible(1.0, false);
    assert!(client.is_active());
    client.advance_visible(1.0, true);
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99), (battle_checkpoint(1), 39, 123)]);
    assert_eq!(client.player().unwrap().lines(), 2);
    assert!(client.player().unwrap().played_out(), "the watcher reached the completed script and awaits final server release");
    assert!(client.take_commands().is_empty());
    client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
    assert!(!client.is_active());
}

#[test]
fn game_over_keeps_the_checkpoint_and_owner_retry_is_sent_once() {
    let (mut client, observed) = rig(SceneRole::Owner, vec![]);
    at_battle(&mut client);
    client.take_commands();
    begun(&mut client, 1, BATTLE);
    client.on_message(AREA, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
    client.on_message(AREA, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
    begun(&mut client, 1, BATTLE);
    client.advance_visible(10.0, false);
    assert!(client.is_active() && !client.player().unwrap().has_window());
    assert!(observed.borrow().resumes.is_empty());
    client.key(KeyCode::Enter, true, false);
    client.key(KeyCode::Enter, false, false);
    assert!(client.take_commands().is_empty(), "dismissing a defeat panel is not a retry");
    client.advance_visible(0.0, true);
    client.key(KeyCode::Enter, true, false);
    client.key(KeyCode::Enter, true, true);
    client.key(KeyCode::Enter, false, false);
    client.key(KeyCode::Enter, true, false);
    assert_eq!(client.take_commands(), [SceneCmd::Retry { run: RUN }]);
    assert!(client.is_active(), "a retry never releases the scene into the field");
    let retry = BattleId(BATTLE.0 + 1);
    begun(&mut client, 1, retry);
    client.on_message(AREA, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
    resumed(&mut client, RUN, 1, BATTLE, 1);
    client.advance_visible(1.0, true);
    assert!(observed.borrow().resumes.is_empty(), "a result from the defeated attempt cannot resume its retry");
    resumed(&mut client, RUN, 1, retry, 99);
    client.advance_visible(1.0, true);
    assert!(client.player().unwrap().has_window());
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
}

#[test]
fn replay_rebuilds_the_original_cast_and_accepted_counters_without_historical_effects() {
    let (mut client, observed) = rig(SceneRole::Owner, vec![ack_window(0, Some(2)), victory(0, 99)]);
    assert!(client.is_active());
    assert_eq!(client.player().unwrap().lines(), 1);
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
    assert_eq!(observed.borrow().private_builds, 0);
    assert!(client.take_commands().is_empty(), "accepted history sends no old steps, ready or end");
    assert!(!client.take_events().iter().any(|e| matches!(e, SceneEvent::Line { .. })), "old lines do not advance a bot twice");
    assert!(!client.take_sounds().iter().any(|s| matches!(s.action, SoundAction::Play { id: 42, .. })), "historical sounds do not replay");
    client.advance_visible(1.0, true);
    assert_eq!(client.player().unwrap().actor(18).unwrap().pos(), Some([160.0, 20.0, 11.0]));
    client.autoplay(None);
    assert_eq!(client.take_commands(), [SceneCmd::Step { run: RUN, step: 1, answer: None }, SceneCmd::BattleReady { run: RUN, ordinal: 2, step: 2 }]);
}

#[test]
fn a_reconnected_watcher_accepts_the_next_step_without_reaccepting_history() {
    let (mut client, _) = rig(SceneRole::Watcher, vec![ack_window(0, Some(2)), victory(0, 99)]);
    client.take_events();
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
    assert_eq!(client.player().unwrap().lines(), 1);
    assert!(client.take_events().is_empty(), "repeated historical steps do not replay a line");
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: None });
    assert_eq!(client.player().unwrap().lines(), 2);
    assert!(client.player().unwrap().battle_input());
    assert!(client.take_commands().is_empty(), "watchers never report a combat boundary");
    assert!(matches!(client.take_events().as_slice(), [SceneEvent::Line { index: 1, .. }]));
}

#[test]
fn replay_rejects_source_identity_answers_and_boundary_order() {
    let mut wrong_window = ack_window(0, Some(2));
    if let SceneReplayEvent::Window { identity, .. } = &mut wrong_window { identity[0] ^= 1; }
    let mut wrong_battle = victory(0, 99);
    if let SceneReplayEvent::Battle { identity, .. } = &mut wrong_battle { identity[0] ^= 1; }
    let mut wrong_formation = victory(0, 99);
    if let SceneReplayEvent::Battle { formation, .. } = &mut wrong_formation { *formation += 1; }
    let mut wrong_progress = victory(0, 99);
    if let SceneReplayEvent::Battle { progress, .. } = &mut wrong_progress { *progress = vec![255]; }
    let bad = vec![
        SceneReplay { origin: None, source: [8; 32], events: vec![] },
        SceneReplay { origin: None, source: SOURCE, events: vec![wrong_window] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, None)] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(3))] },
        SceneReplay { origin: None, source: SOURCE, events: vec![victory(0, 99)] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(2)), wrong_battle] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(2)), wrong_formation] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(2)), wrong_progress] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(1, Some(2))] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(2)), ack_window(1, None)] },
        SceneReplay { origin: None, source: SOURCE, events: vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, Some(1))] },
    ];
    for (case, replay) in bad.into_iter().enumerate() {
        let mut client = SceneClient::new();
        client.set_builder(Box::new(Builder(Rc::new(RefCell::new(Observed::default())), false)));
        client.on_message(AREA, start(SceneRole::Owner, replay));
        assert!(client.is_active() && client.player().is_none() && client.failure().is_some(), "case {case} must hold the field and require disconnect instead of starting a guessed replay");
        assert_eq!(client.take_commands(), [SceneCmd::Leave { run: RUN }], "case {case}");
    }
}

#[test]
fn a_completed_replay_reports_only_its_unacknowledged_end() {
    let history = vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, None), victory(1, 123)];
    let (mut client, observed) = rig(SceneRole::Owner, history.clone());
    assert!(client.is_active() && client.player().unwrap().played_out(), "terminal replay stays held until the server acknowledges completion");
    assert_eq!(client.take_commands(), [SceneCmd::End { run: RUN, skipped: false }]);
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99), (battle_checkpoint(1), 39, 123)]);
    assert!(!client.take_events().iter().any(|e| matches!(e, SceneEvent::Line { .. })));
    client.advance_visible(1.0, true);
    client.autoplay(None);
    assert!(client.take_commands().is_empty());
    assert!(client.is_active());
    client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
    assert!(!client.is_active());

    let mut extra = history;
    extra.push(ack_window(2, None));
    let (mut client, _) = rig(SceneRole::Owner, extra);
    assert!(client.is_active() && client.player().is_none() && client.failure().is_some());
    assert_eq!(client.take_commands(), [SceneCmd::Leave { run: RUN }], "a transcript cannot continue after its terminal boundary");
}

#[test]
fn a_blank_current_window_after_replay_is_acknowledged_once() {
    let (mut client, observed) = rig_with_blank(SceneRole::Owner, vec![ack_window(0, Some(2)), victory(0, 99)], true);
    assert!(client.is_active());
    assert_eq!(client.player().unwrap().lines(), 2);
    assert_eq!(observed.borrow().resumes, [(battle_checkpoint(0), 39, 99)]);
    assert_eq!(client.take_commands(), [SceneCmd::Step { run: RUN, step: 1, answer: None }, SceneCmd::BattleReady { run: RUN, ordinal: 2, step: 2 }]);
    client.advance_visible(1.0, true);
    client.autoplay(None);
    assert!(client.take_commands().is_empty());
}

#[test]
fn suspended_scene_input_goes_to_combat_and_its_result_but_never_to_the_waiting_field() {
    use super::keys::{route_key, SceneRoute};
    use crate::state::ClientState;
    use fmv_core::{AccountId, AreaId, GameId, Pos};
    use fmv_protocol::{BattleMember, EscapePolicy, SyntheticRules};

    let (mut scene, _) = rig(SceneRole::Owner, vec![]);
    at_battle(&mut scene);
    scene.take_commands();
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::default(), AreaId { game: GameId::Ff7, key: AREA.into() });
    state.scene = scene;
    let route = |state: &mut ClientState, now, key| route_key(state, AREA, now, false, key, true, false);
    assert_eq!(route(&mut state, 0.0, KeyCode::Enter), SceneRoute::Scene, "waiting for durable preparation still owns all input");
    assert_eq!(route(&mut state, 0.0, KeyCode::KeyW), SceneRoute::Scene);
    begun(&mut state.scene, 1, BATTLE);
    // The real battle mirror holds input even when game data is unavailable on this machine.
    state.apply(0.0, ServerMsg::EncounterStart {
        battle_id: BATTLE, formation_id: 3, seed: 5,
        party: vec![BattleMember::player(EntityId(1), AccountId(1), "test", 0, 5, 0)],
        preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000,
        escape_policy: EscapePolicy::Formation, initial_party: None, data_fingerprint: SyntheticRules::FINGERPRINT,
    });
    assert!(state.battle.in_battle());
    assert_eq!(route(&mut state, 0.1, KeyCode::Enter), SceneRoute::Field, "the existing battle handler receives confirm");
    assert_eq!(route(&mut state, 0.1, KeyCode::F6), SceneRoute::Field, "a battle key cannot queue a scene preview");
    state.apply(1.0, ServerMsg::BattleEnd { battle_id: BATTLE, outcome: fmv_ff7_battle::rules::Outcome::Defeat, rewards: vec![] });
    state.apply(1.0, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
    state.scene.advance_visible(0.0, false);
    assert!(state.battle.lingering(1.1));
    assert_eq!(route(&mut state, 1.1, KeyCode::Enter), SceneRoute::Field, "the result panel receives confirm before retry can");
    assert!(state.scene.take_commands().is_empty());
    state.scene.advance_visible(0.0, true);
    assert!(!state.battle.lingering(20.0));
    assert_eq!(route(&mut state, 20.0, KeyCode::KeyW), SceneRoute::Scene, "game over holds field movement after the panel closes");
    assert_eq!(route(&mut state, 20.0, KeyCode::F6), SceneRoute::Scene);
    assert_eq!(route(&mut state, 20.0, KeyCode::Enter), SceneRoute::Scene);
    assert_eq!(state.scene.take_commands(), [SceneCmd::Retry { run: RUN }]);
}

#[test]
fn watcher_choices_are_validated_before_panel_clamping() {
    for answer in [None, Some(0), Some(3)] {
        let (mut client, observed) = rig(SceneRole::Watcher, vec![]);
        client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer });
        assert!(client.is_active() && client.player().is_none() && client.failure().is_some(), "invalid raw answer {answer:?} is not repaired by the displayed choice cursor or released into the field");
        assert_eq!(client.take_commands(), [SceneCmd::Leave { run: RUN }]);
        assert!(observed.borrow().resumes.is_empty());
    }
    let (mut client, _) = rig(SceneRole::Watcher, vec![ack_window(0, Some(2)), victory(0, 99)]);
    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: Some(1) });
    assert!(client.is_active() && client.player().is_none() && client.failure().is_some(), "a plain window accepts no answer and a failure never releases field control");
    assert_eq!(client.take_commands(), [SceneCmd::Leave { run: RUN }]);
}

#[test]
fn backing_out_of_a_durable_scene_holds_control_until_disconnect_without_claiming_completion() {
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        for after_battle in [false, true] {
            let (mut client, _) = rig(role, vec![]);
            if after_battle {
                if role == SceneRole::Owner {
                    at_battle(&mut client);
                } else {
                    client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
                    client.advance_visible(1.0, true);
                }
                client.take_commands();
                begun(&mut client, 1, BATTLE);
                resumed(&mut client, RUN, 1, BATTLE, 99);
                client.advance_visible(1.0, true);
            }
            assert!(client.player().unwrap().has_window());
            assert!(client.key(KeyCode::Escape, true, false));
            assert!(client.is_active() && client.player().is_none() && client.failure().is_some(), "{role:?}, after battle {after_battle}: disconnect must keep the field held");
            assert!(client.take_commands().is_empty(), "{role:?}, after battle {after_battle}: backing out neither commits nor requests a scene release");
        }
    }
}

#[test]
fn game_over_back_disconnects_even_after_a_retry_was_requested() {
    for retry_requested in [false, true] {
        let (mut client, _) = rig(SceneRole::Owner, vec![]);
        at_battle(&mut client);
        client.take_commands();
        begun(&mut client, 1, BATTLE);
        client.on_message(AREA, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
        if retry_requested {
            client.key(KeyCode::Enter, true, false);
            assert_eq!(client.take_commands(), [SceneCmd::Retry { run: RUN }]);
        }
        client.key(KeyCode::Escape, true, false);
        assert!(client.is_active() && client.player().is_none() && client.failure().is_some());
        assert!(client.take_commands().is_empty(), "Back disconnects instead of retrying or releasing the saved scene (retry pending: {retry_requested})");
    }
}
