//! Durable scene continuation over a strict scripted source: no game data, GPU or network.

use super::client::{SceneBackground, SceneClient};
use super::player::{SceneCmd, SceneEvent, SoundAction};
use super::source::{Built, SceneBuilder, SceneSource};
use crate::state::ClientState;
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos};
use fmv_ff7::background::{BackgroundOp, BackgroundState};
use fmv_ff7::script::dialogue::{Choice, End, Message, MessageOrigin, Piece};
use fmv_ff7::script::scene::{
    BattleCheckpoint, Beat, CameraOp, FadeBlend, FadeColor, FadeOp, MoveHow, SceneBoundary, SceneResources, Stage, Then, Timed,
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
const BG_INITIAL: BackgroundOp = BackgroundOp::On { group: 7, frame: 1 };
const BG_STEP: BackgroundOp = BackgroundOp::ShiftLeft { group: 7 };
const BG_FINAL: BackgroundOp = BackgroundOp::ShiftRight { group: 7 };

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
            (Beat { stage: vec![timed(0, Stage::Sound { channel: 1, id: 42, pan: 64 })], then: Then::Window(window(0, 0)) }, SceneBoundary::Window(window_checkpoint(0))),
            (Beat { stage: vec![
                timed(20, Stage::Place { entity: 18, x: 100, y: 20, z: None, triangle: None }),
                timed(30, Stage::Camera(CameraOp::Follow { entity: 18 })),
                timed(30, Stage::Background(BG_STEP)),
            ], then: Then::End { at: 30, how: End::Battle } }, SceneBoundary::Battle(battle_checkpoint(0))),
            (Beat { stage: vec![timed(0, Stage::Move { entity: 18, from: None, x: 160, y: 20, z: None,
                triangle: None, frames: 30, how: MoveHow::Walk, animation: None })], then: Then::Window(second) }, SceneBoundary::Window(window_checkpoint(1))),
            (Beat { stage: vec![], then: Then::End { at: 0, how: End::Battle } }, SceneBoundary::Battle(battle_checkpoint(1))),
            (Beat { stage: vec![timed(0, Stage::Background(BG_FINAL))], then: Then::End { at: 0, how: End::Finished } }, SceneBoundary::End(End::Finished)),
        ];
        Self { beats: beats.into(), boundary: None, observed }
    }
}

impl SceneSource for Scripted {
    fn initial(&self) -> Vec<Stage> {
        let mut events = super::tests::places();
        events.push(Stage::Background(BG_INITIAL));
        events
    }
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
    ServerMsg::SceneStart { presentation_origin: None,
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
    let mut start = ServerMsg::EncounterStart {
        battle_id: BATTLE, formation_id: 3, seed: 5,
        party: vec![BattleMember::player(EntityId(1), AccountId(1), "test", 0, 5, 0)],
        preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000,
        escape_policy: EscapePolicy::Formation, victory_fanfare: true, game_over_on_defeat: true, initial_party: None, data_fingerprint: SyntheticRules::FINGERPRINT,
    };
    assert_eq!(state.apply(0.0, start.clone()), None);
    assert!(state.battle.in_battle());
    assert_eq!(route(&mut state, 0.1, KeyCode::Enter), SceneRoute::Field, "the existing battle handler receives confirm");
    assert_eq!(route(&mut state, 0.1, KeyCode::F6), SceneRoute::Field, "a battle key cannot queue a scene preview");
    state.apply(1.0, ServerMsg::BattleEnd { battle_id: BATTLE, outcome: fmv_ff7_battle::rules::Outcome::Defeat, rewards: vec![] });
    state.apply(1.0, ServerMsg::SceneGameOver { run: RUN, ordinal: 1, battle_id: BATTLE });
    assert_eq!(state.battle.game_over(), Some(BATTLE));
    assert!(!state.ordinary_game_over(), "the authored flag must not hijack the scene owner's retry controls");
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
    let retry = BattleId(BATTLE.0 + 1);
    begun(&mut state.scene, 1, retry);
    if let ServerMsg::EncounterStart { battle_id, .. } = &mut start { *battle_id = retry; }
    assert_eq!(state.apply(20.1, start), None, "a scene-authorized retry remains admissible");
    assert!(state.battle.in_battle());
    assert_eq!(state.battle.game_over(), None);
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

fn recently_moving_state() -> ClientState {
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::default(), AreaId { game: GameId::Ff7, key: AREA.into() });
    state.own.set_motion(Pos::new(10.0, 20.0, 30.0), 1.25, AnimState::Run);
    let sent = state.own.next_intent(10.0).expect("the predicted movement was sent before the scene started");
    assert_eq!(sent.seq, 1);
    state
}

fn own_pose_message(full_snapshot: bool, pos: Pos, facing: f32, anim: AnimState) -> ServerMsg {
    let entities = vec![EntitySnapshot { id: EntityId(1), pos, facing, anim, avatar: AvatarAppearance::new("test", [1, 2, 3], None) }];
    if full_snapshot {
        ServerMsg::AreaSnapshot { area: AreaId { game: GameId::Ff7, key: AREA.into() }, tick: 1, entities, npcs: vec![] }
    } else {
        ServerMsg::EntityUpdate { tick: 1, entities }
    }
}

fn start_held_scene(state: &mut ClientState, durable: bool, terminal: bool) {
    let events = if terminal {
        vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, None), victory(1, 123)]
    } else {
        vec![]
    };
    state.scene.set_builder(Box::new(Builder(Rc::new(RefCell::new(Observed::default())), false)));
    let mut message = start(SceneRole::Owner, SceneReplay { origin: None, source: SOURCE, events });
    let ServerMsg::SceneStart { replay, .. } = &mut message else { unreachable!("the fixture creates a scene start") };
    if !durable { *replay = None; }
    assert!(state.apply(10.015, message).is_none());
    assert!(state.scene.is_active());
    assert_eq!(state.scene.player().unwrap().played_out(), terminal);
    state.scene.take_commands();
}

#[test]
fn durable_scene_snapshots_adopt_the_exact_full_pose_without_echoing_or_restoring_deferred_prediction() {
    for full_snapshot in [false, true] {
        for terminal in [false, true] {
            for authored in [Pos::new(10.0, 20.0, 30.25), Pos::new(10.25, 20.125, 30.25)] {
                let mut state = recently_moving_state();
                let predicted = state.own.pos();
                // A rejected intent wants a resync. A later ordinary echo is also deferred while
                // prediction is fresh; neither may survive the scene's authoritative final pose.
                assert!(state.own.on_rejected(1, predicted));
                assert!(state.own.has_unsent());
                assert!(state.apply(10.01, own_pose_message(false, Pos::new(900.0, 800.0, 700.0), 2.0, AnimState::Walk)).is_none());
                assert_eq!(state.own.pos(), predicted);
                start_held_scene(&mut state, true, terminal);

                assert!(state.apply(10.02, own_pose_message(full_snapshot, authored, -0.75, AnimState::Idle)).is_none());
                let expected = (authored, -0.75, AnimState::Idle);
                assert_eq!((state.own.pos(), state.own.facing(), state.own.anim()), expected,
                    "the authored pose wins immediately even for z-only/subunit moves (full: {full_snapshot}, terminal: {terminal})");
                assert!(!state.own.has_unsent(), "the complete pose is already known to the server");
                assert!(state.apply(10.03, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished }).is_none());
                assert!(!state.scene.is_active());
                for now in [10.049, 10.05, 10.6, 12.0] {
                    assert!(state.own.next_intent(now).is_none(), "no resync or move echo at {now}");
                    assert_eq!((state.own.pos(), state.own.facing(), state.own.anim()), expected, "a deferred ordinary echo must not pull the player back after release");
                }
                let moved = Pos::new(authored.x + 5.0, authored.y, authored.z);
                state.own.set_motion(moved, 0.25, AnimState::Walk);
                let next = state.own.next_intent(12.1).expect("the player's next actual movement is sent");
                assert_eq!((next.seq, next.pos, next.facing, next.anim), (2, moved, 0.25, AnimState::Walk), "snapshot adoption preserves the intent sequence");
            }
        }
    }
}

#[test]
fn adopting_a_durable_scene_pose_preserves_the_original_movement_rate_clock() {
    for full_snapshot in [false, true] {
        let mut state = recently_moving_state();
        start_held_scene(&mut state, true, true);
        let authored = Pos::new(10.0, 20.0, 30.25);
        assert!(state.apply(10.02, own_pose_message(full_snapshot, authored, -0.75, AnimState::Idle)).is_none());
        assert!(state.apply(10.025, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished }).is_none());
        state.own.set_motion(Pos::new(11.0, 20.0, 30.25), 0.25, AnimState::Walk);
        assert!(state.own.next_intent(10.049).is_none(), "the snapshot must not reset the rate clock to an unset value");
        let next = state.own.next_intent(10.05).expect("the original send at 10.0, not snapshot receipt at 10.02, determines the next send time");
        assert_eq!((next.seq, next.pos, next.facing, next.anim), (2, Pos::new(11.0, 20.0, 30.25), 0.25, AnimState::Walk));
    }
}

#[test]
fn ordinary_field_and_legacy_scene_snapshots_keep_the_existing_prediction_rules() {
    for legacy_scene in [false, true] {
        for full_snapshot in [false, true] {
            for server_pos in [Pos::new(40.0, 50.0, 60.0), Pos::new(10.0, 20.0, 30.25), Pos::new(10.25, 20.125, 30.25)] {
                let mut state = recently_moving_state();
                let predicted = (state.own.pos(), state.own.facing(), state.own.anim());
                if legacy_scene { start_held_scene(&mut state, false, false); }
                assert!(state.apply(10.02, own_pose_message(full_snapshot, server_pos, -0.75, AnimState::Idle)).is_none());
                assert_eq!((state.own.pos(), state.own.facing(), state.own.anim()), predicted, "ordinary recent prediction is unchanged (legacy: {legacy_scene}, full: {full_snapshot})");
                assert!(state.own.next_intent(10.1).is_none());
                assert_eq!((state.own.pos(), state.own.facing(), state.own.anim()), predicted);
                assert!(state.own.next_intent(11.0).is_none());
                let quiet_pos = if predicted.0.dist_xy(server_pos) > 1.0 { server_pos } else { predicted.0 };
                assert_eq!((state.own.pos(), state.own.facing(), state.own.anim()), (quiet_pos, predicted.1, predicted.2), "ordinary quiet reconciliation keeps its existing position threshold and local facing/animation");
            }
        }
    }
}

fn background(op: BackgroundOp) -> SceneBackground { SceneBackground { field: AREA.into(), op } }

fn next_scene_start() -> ServerMsg {
    let mut message = start(SceneRole::Owner, SceneReplay { origin: None, source: SOURCE, events: vec![] });
    let ServerMsg::SceneStart { run, .. } = &mut message else { unreachable!("the fixture creates a scene start") };
    *run = SceneRunId(RUN.0 + 1);
    message
}

#[test]
fn a_final_background_instruction_survives_scene_ended_before_the_next_render() {
    let (mut client, _) = rig(SceneRole::Owner, vec![]);
    at_battle(&mut client);
    begun(&mut client, 1, BATTLE);
    resumed(&mut client, RUN, 1, BATTLE, 99);
    client.advance_visible(1.0, true);
    let earlier = client.drain_background();
    assert_eq!(earlier, [background(BG_INITIAL), background(BG_STEP)]);
    let mut field = BackgroundState::default();
    for event in earlier { field.apply(event.op); }
    assert!(field.visible(2, 7, 4) && !field.visible(2, 7, 2));

    client.autoplay(None);
    let last_battle = BattleId(BATTLE.0 + 1);
    begun(&mut client, 2, last_battle);
    resumed(&mut client, RUN, 2, last_battle, 123);
    client.advance_visible(0.0, true);
    assert!(client.player().unwrap().played_out());
    // The server's final acknowledgement arrives in the same frame, before a renderer can
    // read the final shift from the scene player that this message removes.
    client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
    assert!(!client.is_active());
    let final_events = client.drain_background();
    assert_eq!(final_events, [background(BG_FINAL)]);
    for event in final_events { field.apply(event.op); }
    assert!(field.visible(2, 7, 2) && !field.visible(2, 7, 4));
    assert!(client.drain_background().is_empty(), "a second rendered frame must not shift the mask again");
    client.advance_visible(1.0, true);
    assert!(client.drain_background().is_empty());
    assert!(field.visible(2, 7, 2));
}

#[test]
fn a_same_field_scene_keeps_ordered_undrained_background_changes_from_its_predecessor() {
    for server_ended_first in [false, true] {
        let history = vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, None), victory(1, 123)];
        let (mut client, _) = rig(SceneRole::Owner, history);
        assert!(client.player().unwrap().played_out());
        if server_ended_first {
            client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
        }
        // Both the explicit ended path and direct replacement call sites must preserve the
        // prior field mutations until the renderer consumes them, then append the new scene's.
        client.on_message(AREA, next_scene_start());
        assert_eq!(client.drain_background(), [background(BG_INITIAL), background(BG_STEP), background(BG_FINAL), background(BG_INITIAL)]);
        assert!(client.drain_background().is_empty());
        client.advance_visible(1.0, true);
        assert!(client.drain_background().is_empty(), "reaping the new scene again must not replay its initial operation");
    }
}

#[test]
fn replay_reconstructs_historical_background_state_once_without_repeating_shifts() {
    let (mut client, _) = rig(SceneRole::Owner, vec![ack_window(0, Some(2)), victory(0, 99)]);
    let events = client.drain_background();
    assert_eq!(events, [background(BG_INITIAL), background(BG_STEP)], "historical stage operations still rebuild the field's current background");
    let mut field = BackgroundState::default();
    for event in events { field.apply(event.op); }
    assert!(field.visible(2, 7, 4) && !field.visible(2, 7, 2));
    assert!(client.drain_background().is_empty());
    client.advance_visible(1.0, true);
    assert!(client.drain_background().is_empty(), "the retained cast journal is consumed by suffix, not replayed each frame");
    assert!(field.visible(2, 7, 4) && !field.visible(2, 7, 8), "duplicating the left shift would display a different frame");
}

#[test]
fn changing_area_discards_old_visit_background_changes_even_when_reloading_the_same_field() {
    for target in [AREA, "another-field"] {
        let mut state = recently_moving_state();
        let history = vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, None), victory(1, 123)];
        state.scene = rig(SceneRole::Owner, history).0;
        assert!(state.scene.player().unwrap().played_out());
        state.change_area(AreaId { game: GameId::Ff7, key: target.into() }, Pos::default(), 0.0, true);
        assert!(!state.scene.is_active());
        assert!(state.scene.drain_background().is_empty(), "a new visit to {target} has its own field-entry mask baseline");
        if target == AREA {
            state.scene.on_message(AREA, next_scene_start());
            assert_eq!(state.scene.drain_background(), [background(BG_INITIAL)], "the reloaded visit receives only its new scene's operation");
            assert!(state.scene.drain_background().is_empty());
        }
    }
}

fn roster_refresh(events: Vec<SceneReplayEvent>) -> ServerMsg {
    let mut message = start(SceneRole::Watcher, SceneReplay { origin: None, source: SOURCE, events });
    let ServerMsg::SceneStart { owner, participants, .. } = &mut message else { unreachable!("the fixture creates a scene start") };
    *owner = EntityId(99);
    *participants = vec![SceneParticipant { entity: EntityId(99), spot: 0 }, SceneParticipant { entity: EntityId(2), spot: 1 }];
    message
}

#[test]
fn a_same_run_roster_refresh_preserves_the_stage_clock_and_pending_effects_without_replaying_them() {
    for phase in 0..4 {
        let prepare = || {
            let (mut client, observed) = rig(SceneRole::Watcher, vec![]);
            if phase > 0 {
                client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
                client.advance_visible(1.0, true);
            }
            if phase >= 2 {
                begun(&mut client, 1, BATTLE);
                resumed(&mut client, RUN, 1, BATTLE, 99);
                client.advance_visible(0.25, true);
            }
            if phase == 3 {
                client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: None });
            }
            (client, observed)
        };
        let history = match phase {
            0 => vec![],
            1 => vec![ack_window(0, Some(2))],
            2 => vec![ack_window(0, Some(2)), victory(0, 99)],
            _ => vec![ack_window(0, Some(2)), victory(0, 99), ack_window(1, None)],
        };
        let (mut reference, _) = prepare();
        let expected_background = reference.drain_background();
        let expected_sounds = reference.take_sounds();
        let expected_events = reference.take_events();
        let expected_commands = reference.take_commands();
        let (mut client, _) = prepare();
        let state = |client: &SceneClient| {
            let player = client.player().unwrap();
            (player.actor(18).unwrap().pos(), player.camera_clock(), player.lines(), player.epoch(), player.has_window(), player.battle_input())
        };
        let before = state(&client);
        client.on_message(AREA, roster_refresh(history.clone()));
        assert_eq!(state(&client), before, "a reconnect updates the roster, not stage time or the accepted-step counter (phase {phase})");
        assert_eq!(client.player().unwrap().witnesses().iter().map(|w| w.entity).collect::<Vec<_>>(), [EntityId(99), EntityId(2)]);
        assert_eq!(client.started(), 1);
        assert_eq!(client.drain_background(), expected_background, "undrained operations stay ordered and occur once");
        assert_eq!(client.take_sounds(), expected_sounds, "no Begin/End or historical sound is appended or discarded");
        assert_eq!(client.take_events(), expected_events, "no Started/Line event is appended or discarded");
        assert_eq!(client.take_commands(), expected_commands);

        client.on_message(AREA, roster_refresh(history.clone()));
        assert_eq!(state(&client), before);
        assert!(client.drain_background().is_empty() && client.take_sounds().is_empty() && client.take_events().is_empty() && client.take_commands().is_empty(), "a repeated roster refresh has no presentation or protocol effects");
        if phase == 3 {
            client.advance_visible(1.0, true);
            assert_eq!(client.player().unwrap().lines(), 2, "the early accepted answer survives refresh until its window arrives");
            assert!(client.player().unwrap().battle_input());
            assert!(client.take_commands().is_empty(), "a watcher never echoes the retained answer");
        }

        // A fresh connection has no retained stage: the same prefix must still reconstruct all
        // accepted background operations and battles, while suppressing historical commands.
        let (mut fresh, fresh_observed) = rig(SceneRole::Watcher, history);
        assert_eq!(fresh.player().unwrap().lines(), if phase == 3 { 2 } else { u32::from(phase > 0) });
        assert_eq!(fresh_observed.borrow().resumes.len(), usize::from(phase >= 2));
        let expected = if phase > 0 { vec![background(BG_INITIAL), background(BG_STEP)] } else { vec![background(BG_INITIAL)] };
        assert_eq!(fresh.drain_background(), expected);
        assert!(fresh.take_commands().is_empty());
    }
}

#[test]
fn a_same_run_refresh_with_changed_original_inputs_or_invalid_replay_holds_a_failure() {
    for changed in ["gil", "source", "seed", "replay shape", "replay downgrade"] {
        let (mut client, _) = rig(SceneRole::Watcher, vec![]);
        let mut message = roster_refresh(vec![]);
        let ServerMsg::SceneStart { gil, seed, replay, .. } = &mut message else { unreachable!("the fixture creates a scene start") };
        match changed {
            "gil" => *gil += 1,
            "source" => replay.as_mut().unwrap().source[0] ^= 1,
            "seed" => *seed += 1,
            "replay downgrade" => *replay = None,
            _ => replay.as_mut().unwrap().events.push(ack_window(1, None)),
        }
        client.on_message(AREA, message);
        assert!(client.is_active() && client.failure().is_some(), "changed {changed} cannot silently rebuild or release the retained scene");
        assert_eq!(client.started(), 1, "a bad refresh cannot replace the original scene");
    }
}

#[test]
fn a_same_run_refresh_rejects_changed_accepted_answers_rewards_and_boundary_identities() {
    for changed in ["answer", "battle gil", "window identity", "battle identity"] {
        let (mut client, _) = rig(SceneRole::Watcher, vec![]);
        client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: Some(2) });
        client.advance_visible(1.0, true);
        begun(&mut client, 1, BATTLE);
        resumed(&mut client, RUN, 1, BATTLE, 99);
        client.advance_visible(0.25, true);
        client.drain_background();
        let mut history = vec![ack_window(0, Some(2)), victory(0, 99)];
        match changed {
            "answer" => {
                let SceneReplayEvent::Window { answer, .. } = &mut history[0] else { unreachable!() };
                *answer = Some(1); // Another valid option is still a different accepted choice.
            }
            "battle gil" => {
                let SceneReplayEvent::Battle { gil, .. } = &mut history[1] else { unreachable!() };
                *gil += 1;
            }
            "window identity" => {
                let SceneReplayEvent::Window { identity, .. } = &mut history[0] else { unreachable!() };
                identity[0] ^= 1;
            }
            _ => {
                let SceneReplayEvent::Battle { identity, .. } = &mut history[1] else { unreachable!() };
                identity[0] ^= 1;
            }
        }
        client.on_message(AREA, roster_refresh(history));
        assert!(client.is_active() && client.failure().is_some(), "changed {changed} cannot rewrite accepted history during a roster refresh");
        assert_eq!(client.started(), 1);
        assert!(client.drain_background().is_empty(), "a failed refresh probe cannot publish historical background operations");
    }
}

struct EffectsSource {
    script: Scripted,
    initial: Vec<Stage>,
}

impl SceneSource for EffectsSource {
    fn initial(&self) -> Vec<Stage> { self.initial.clone() }
    fn height_at(&self, _: i16, _: i16, _: Option<u16>) -> Option<f32> { Some(11.0) }
    fn fingerprint(&self) -> Option<[u8; 32]> { Some(SOURCE) }
    fn boundary(&self) -> Option<SceneBoundary> { self.script.boundary() }
    fn next(&mut self, answer: Option<usize>) -> Beat { self.script.next(answer) }
    fn next_checked(&mut self, answer: Option<usize>) -> Result<Beat, String> { self.script.next_checked(answer) }
}

struct EffectsBuilder {
    field: &'static str,
    x_steps: u8,
    end: End,
}

impl SceneBuilder for EffectsBuilder {
    fn build(&mut self, _: &str, _: &StoryState, _: u32, _: SceneResources) -> Result<Built, String> {
        let checkpoint = WindowCheckpoint { dialog: Some(1), choices: None, ..window_checkpoint(0) };
        let beats = vec![
            (Beat { stage: vec![], then: Then::Window(window(1, 0)) }, SceneBoundary::Window(checkpoint)),
            (Beat {
                stage: vec![timed(0, Stage::Fade(FadeOp::Nfade { kind: 12, rgb: [255; 3], frames: 6 }))],
                then: Then::End { at: 6, how: self.end },
            }, SceneBoundary::End(self.end)),
        ];
        let mut initial = super::tests::places();
        initial.push(Stage::Shake { mask: 3, x_amplitude: 20, x_steps: self.x_steps, y_amplitude: 5, y_steps: 2 });
        let script = Scripted { beats: beats.into(), boundary: None, observed: Rc::new(RefCell::new(Observed::default())) };
        Ok(Built { def: super::tests::def_in(self.field), source: Box::new(EffectsSource { script, initial }) })
    }
    fn reach(&mut self, _: usize, _: &str) -> Option<f32> { None }
}

fn effects_start(run: SceneRunId) -> ServerMsg {
    let mut message = start(SceneRole::Owner, SceneReplay { origin: None, source: SOURCE, events: vec![] });
    let ServerMsg::SceneStart { run: id, .. } = &mut message else { unreachable!("the fixture creates a scene start") };
    *id = run;
    message
}

#[test]
fn shake_random_indices_survive_scene_and_field_handoffs_while_the_visible_offset_resets() {
    for handoff in ["next scene", "scene ended", "same field reload", "different field"] {
        let mut state = recently_moving_state();
        state.scene.set_builder(Box::new(EffectsBuilder { field: AREA, x_steps: 8, end: End::Finished }));
        state.scene.on_message(AREA, effects_start(RUN));
        assert_eq!(state.scene.player().unwrap().shake_rng_indices(), [0, 0], "a new process begins at the first sample of each axis");
        state.scene.advance_visible(0.25, true);
        let previous = state.scene.player().unwrap();
        assert_eq!(previous.shake_rng_indices(), [1, 2], "7.5 frames advance the differently paced axes independently");
        assert_ne!(previous.shake(), [0.0, 0.0]);

        let next_field = if handoff == "different field" { "another-field" } else { AREA };
        match handoff {
            "scene ended" => state.scene.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Aborted }),
            "same field reload" | "different field" => state.change_area(AreaId { game: GameId::Ff7, key: next_field.into() }, Pos::default(), 0.0, true),
            _ => {},
        }
        state.scene.set_builder(Box::new(EffectsBuilder { field: next_field, x_steps: 2, end: End::Finished }));
        state.scene.on_message(next_field, effects_start(SceneRunId(RUN.0 + 1)));
        let next = state.scene.player().unwrap();
        assert_eq!(next.shake_rng_indices(), [1, 2], "the next scene inherits each axis's random cursor across {handoff}");
        assert_eq!(next.shake(), [0.0, 0.0], "the previous visual phase does not leak across {handoff}");
        state.scene.advance_visible(0.1, true);
        let next = state.scene.player().unwrap();
        assert_eq!(next.shake_rng_indices(), [2, 3]);
        assert_eq!(next.shake(), [15.0, 4.0], "the next targets use samples 202 and 238; restarting at zero would produce [13, 3]");
    }
}

#[test]
fn a_completed_map_jump_keeps_its_fade_until_the_destination_field_is_loaded() {
    for next_field in [AREA, "another-field"] {
        let mut state = recently_moving_state();
        state.scene.set_builder(Box::new(EffectsBuilder { field: AREA, x_steps: 8, end: End::MapJump { field: 138 } }));
        state.scene.on_message(AREA, effects_start(RUN));
        state.scene.autoplay(None);
        state.scene.advance_visible(0.2, true);
        let expected = Some(FadeColor { blend: FadeBlend::Subtract, rgb: [255; 3] });
        assert!(state.scene.player().unwrap().played_out());
        assert_eq!(state.scene.fade(), expected);
        assert_eq!(state.scene.take_commands(), [SceneCmd::Step { run: RUN, step: 0, answer: None }, SceneCmd::End { run: RUN, skipped: false }]);

        state.scene.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
        assert!(state.scene.player().is_none());
        assert_eq!(state.scene.fade(), expected, "the acknowledgement must not flash the old field while travel is pending");
        state.scene.advance_visible(1.0, true);
        state.scene.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome: fmv_protocol::SceneOutcome::Finished });
        assert_eq!(state.scene.fade(), expected, "idle frames and duplicate acknowledgements retain the terminal picture");
        state.change_area(AreaId { game: GameId::Ff7, key: next_field.into() }, Pos::default(), 0.0, true);
        assert_eq!(state.scene.fade(), None, "the destination, including a new visit to the same field, establishes its own fade state");
    }
}

#[test]
fn only_a_successfully_completed_map_jump_retains_its_terminal_fade() {
    use fmv_protocol::SceneOutcome;
    for (end, outcome) in [
        (End::MapJump { field: 138 }, SceneOutcome::Aborted),
        (End::MapJump { field: 138 }, SceneOutcome::TimedOut),
        (End::MapJump { field: 138 }, SceneOutcome::Skipped),
        (End::Finished, SceneOutcome::Finished),
    ] {
        let mut client = SceneClient::new();
        client.set_builder(Box::new(EffectsBuilder { field: AREA, x_steps: 8, end }));
        client.on_message(AREA, effects_start(RUN));
        client.autoplay(None);
        client.advance_visible(0.2, true);
        assert!(client.player().unwrap().played_out());
        assert_eq!(client.fade(), Some(FadeColor { blend: FadeBlend::Subtract, rgb: [255; 3] }));
        client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome });
        assert!(client.player().is_none());
        assert_eq!(client.fade(), None, "{end:?} acknowledged as {outcome:?} must release its black screen");
        client.advance_visible(1.0, true);
        assert_eq!(client.fade(), None);
    }
}
