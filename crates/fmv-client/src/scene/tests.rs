//! The scene state machine over a SCRIPTED source: no game data, no GPU, invented words only.
//!
//! The fake scene is a short exchange between two invented characters in an invented field: three plain windows and a
//! question, with a walk and a turn between them. What is checked is the machine: the order of the lines, what the owner
//! reports (steps 0, 1, 2 ... in order, the end exactly once), what a watcher does and does not do, what a skip and
//! the server's `SceneEnded` do, that the keys of a scene are the scene's alone while it is on show and the field's again
//! afterwards, and that a scene this machine cannot build releases the player on the spot.

use super::client::{clean_id, SceneClient};
use super::draw::{cast_players, cast_who, label_hidden, place_witnesses, visible_npcs, CAST_WHO_BASE};
use super::keys::{route_key, SceneRoute};
use super::player::{Run, SceneCmd, SceneEvent, SceneKey, ScenePlayer};
use super::source::{Built, SceneBuilder, SceneSource};
use super::stage::CameraState;
use crate::avatar::{AvatarPlayer, Who};
use crate::npc::npc_who;
use crate::state::{ClientState, NpcView};
use crate::ui::hud::Subject;
use fmv_core::{AccountId, AnimState, AreaId, EntityId, GameId, Pos};
use fmv_ff7::script::dialogue::{Choice, End, Message, MessageOrigin, Piece};
use fmv_ff7::script::scene::{Beat, CameraOp, CastMember, Look, MoveHow, SceneDef, Spot, Stage, Then, Timed, Trigger, Window};
use fmv_protocol::{SceneOutcome, SceneParticipant, SceneRefusal, SceneRole, SceneRunId, ServerMsg, MAX_SCENE_STEPS, SCENE_NPC_ID_BASE};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use winit::keyboard::KeyCode;

const AREA: &str = "arena";
const SCENE: &str = "test/arena/duel";
const RUN: SceneRunId = SceneRunId(7);
/// The proof scene's own moment, as the progression blob the server sends.
const PROGRESS: [u8; 3] = [1, 72, 0];

// ---- the fake scene ------------------------------------------------------------------------------------------------

pub(crate) fn def_in(field: &str) -> SceneDef {
    SceneDef {
        id: SCENE.to_string(),
        game: "test".to_string(),
        field: field.to_string(),
        trigger: Trigger { entity: 20, name: "beta".to_string(), slot: 1, talk_radius: 100, moments: 72..=72 },
        setup: vec![],
        given: vec![],
        root: vec![],
        cast: vec![
            CastMember { entity: 18, name: "alpha".to_string(), look: Look::Appearance("ff7/cloud".to_string()), start: None },
            CastMember { entity: 20, name: "beta".to_string(), look: Look::Appearance("ff7/tifa".to_string()), start: None },
        ],
        speakers: vec![("Alpha".to_string(), 18), ("Beta".to_string(), 20)],
        witness: vec![
            Spot { x: -100, y: -72, triangle: 24, direction: 0 },
            Spot { x: -100, y: -216, triangle: 40, direction: 128 },
            Spot { x: -30, y: -72, triangle: 25, direction: 226 },
        ],
        arrival: Spot { x: -125, y: -100, triangle: 27, direction: 192 },
    }
}

fn def() -> SceneDef {
    def_in(AREA)
}

fn message(dialog: usize, opener: u8, text: &str) -> Message {
    Message::new(MessageOrigin::Canon { entity: usize::from(opener), dialog }, vec![Piece::Text(text.to_string())])
}

fn window(at: u32, opener: u8, speaker: Option<u8>, message: Message) -> Window {
    Window { at, opener, speaker, id: 0, geometry: None, message }
}

fn say(at: u32, opener: u8, speaker: u8, dialog: usize) -> Beat {
    Beat { stage: vec![], then: Then::Window(window(at, opener, Some(speaker), message(dialog, opener, "words of the test"))) }
}

/// A question of two choices on lines 1 and 2, the pointer starting on line `cursor`.
fn ask(at: u32, opener: u8, speaker: u8, dialog: usize, cursor: usize) -> Beat {
    let mut m = Message::new(
        MessageOrigin::Canon { entity: usize::from(opener), dialog },
        vec![Piece::Text("a question?".into()), Piece::NewLine, Piece::Choice, Piece::Text("one".into()), Piece::NewLine, Piece::Choice, Piece::Text("two".into())],
    );
    m.choice = Some(Choice { first: 1, last: 2, cursor });
    Beat { stage: vec![], then: Then::Window(window(at, opener, Some(speaker), m)) }
}

fn blank(at: u32, opener: u8) -> Beat {
    let m = Message::new(MessageOrigin::Canon { entity: usize::from(opener), dialog: 9 }, vec![Piece::Text("  ".into())]);
    Beat { stage: vec![], then: Then::Window(window(at, opener, None, m)) }
}

fn end(at: u32) -> Beat {
    Beat { stage: vec![], then: Then::End { at, how: End::Finished } }
}

fn timed(at: u32, what: Stage) -> Timed<Stage> {
    Timed { at, what }
}

pub(crate) type Asked = Rc<RefCell<Vec<Option<usize>>>>;

/// A scripted source: the beats it gives in order (then an end), what it was answered, and a flat ground.
pub(crate) struct Scripted {
    pub(crate) initial: Vec<Stage>,
    pub(crate) beats: VecDeque<Beat>,
    pub(crate) asked: Asked,
}

impl SceneSource for Scripted {
    fn initial(&self) -> Vec<Stage> {
        self.initial.clone()
    }

    fn next(&mut self, answer: Option<usize>) -> Beat {
        self.asked.borrow_mut().push(answer);
        self.beats.pop_front().unwrap_or_else(|| end(0))
    }

    fn height_at(&self, _x: i16, _y: i16, _triangle: Option<u16>) -> Option<f32> {
        Some(11.0)
    }
}

#[test]
fn private_echo_sends_original_choice_and_voice_cues() {
    struct EchoFake(Scripted);
    impl SceneSource for EchoFake {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn echo_voice(&self) -> bool { true }
        fn server_answer(&self, w: &Window, answer: Option<usize>) -> Result<Option<usize>, ()> {
            if w.message.choice.is_some() { answer.filter(|a| *a <= 1).map(|a| Some(a + 1)).ok_or(()) } else { Ok(None) }
        }
    }
    use crate::mods::{take_voices, VoiceCue};
    let _ = take_voices();
    let mut question = ask(0, 18, 18, 3, 0);
    if let Then::Window(ref mut w) = question.then { w.message.choice = Some(Choice { first: 0, last: 1, cursor: 0 }); }
    let source = EchoFake(Scripted { initial: vec![], beats: vec![question, end(0)].into(), asked: Asked::default() });
    let run = Run { id: RUN, role: SceneRole::Owner, participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 }] };
    let mut p = ScenePlayer::new(run, def(), Box::new(source), true);
    assert_eq!(take_voices(), [VoiceCue::Say { field: AREA.into(), window: 0, dialog: 3, page: 0 }]);
    p.key(SceneKey::Down);
    assert_eq!(take_voices(), [VoiceCue::Choice { field: AREA.into(), dialog: 3, line: 1 }]);
    p.key(SceneKey::Confirm);
    assert_eq!(p.take_commands(), [step(0, Some(2)), SceneCmd::End { run: RUN, skipped: false }]);
    assert!(take_voices().contains(&VoiceCue::Stop));
}

#[test]
fn private_scene_builder_is_used_only_for_a_lone_owner() {
    struct Probe { private: Rc<RefCell<u32>>, regular: Rc<RefCell<u32>> }
    impl SceneBuilder for Probe {
        fn build(&mut self, _: &str, _: &fmv_ff7::story::StoryState, _: u32) -> Result<Built, String> {
            *self.regular.borrow_mut() += 1;
            Err("test".into())
        }
        fn build_private(&mut self, _: &str, _: &fmv_ff7::story::StoryState, _: u32) -> Result<Built, String> {
            *self.private.borrow_mut() += 1;
            Err("test".into())
        }
        fn reach(&mut self, _: usize, _: &str) -> Option<f32> { None }
    }
    let private = Rc::new(RefCell::new(0));
    let regular = Rc::new(RefCell::new(0));
    let mut c = SceneClient::new();
    c.set_builder(Box::new(Probe { private: private.clone(), regular: regular.clone() }));
    c.on_message(AREA, start(RUN, SceneRole::Owner, &[(1, 0)]));
    c.on_message(AREA, start(RUN, SceneRole::Owner, &[(1, 0), (2, 1)]));
    c.on_message(AREA, start(RUN, SceneRole::Watcher, &[(1, 0)]));
    assert_eq!((*private.borrow(), *regular.borrow()), (1, 2));
}

/// A builder that makes the scripted scene (or fails).
struct Fake {
    initial: Vec<Stage>,
    beats: Vec<Beat>,
    asked: Asked,
    fail: bool,
    built: Rc<RefCell<u32>>,
}

impl SceneBuilder for Fake {
    fn build(&mut self, scene: &str, story: &fmv_ff7::story::StoryState, seed: u32) -> Result<Built, String> {
        assert_eq!((scene, story.moment, seed), (SCENE, 72, 5), "the scene, the story and the seed the server sent");
        *self.built.borrow_mut() += 1;
        if self.fail {
            return Err("test: no data".to_string());
        }
        Ok(Built { def: def(), source: Box::new(Scripted { initial: self.initial.clone(), beats: self.beats.clone().into(), asked: Rc::clone(&self.asked) }) })
    }

    fn reach(&mut self, index: usize, area: &str) -> Option<f32> {
        (index == 0 && area == AREA).then_some(100.0)
    }
}

struct Rig {
    client: SceneClient,
    asked: Asked,
    built: Rc<RefCell<u32>>,
}

pub(crate) fn places() -> Vec<Stage> {
    vec![
        Stage::Place { entity: 18, x: 10, y: 10, z: None, triangle: Some(1) },
        Stage::Place { entity: 20, x: 50, y: 10, z: None, triangle: Some(2) },
    ]
}

fn rig_with(beats: Vec<Beat>, fail: bool) -> Rig {
    let (asked, built) = (Asked::default(), Rc::new(RefCell::new(0)));
    let mut client = SceneClient::new();
    client.set_builder(Box::new(Fake { initial: places(), beats, asked: Rc::clone(&asked), fail, built: Rc::clone(&built) }));
    Rig { client, asked, built }
}

/// Three plain windows and a question: alpha, beta, a question of alpha's, beta's reply.
pub(crate) fn talk() -> Vec<Beat> {
    vec![say(0, 18, 18, 1), say(15, 20, 20, 2), ask(0, 18, 18, 3, 1), say(0, 20, 20, 4), end(30)]
}

fn start(run: SceneRunId, you: SceneRole, spots: &[(u64, u8)]) -> ServerMsg {
    ServerMsg::SceneStart {
        run,
        scene: SCENE.to_string(),
        seed: 5,
        progress: PROGRESS.to_vec(),
        owner: EntityId(1),
        participants: spots.iter().map(|&(entity, spot)| SceneParticipant { entity: EntityId(entity), spot }).collect(),
        you,
    }
}

fn begin(rig: &mut Rig, you: SceneRole) {
    rig.client.on_message(AREA, start(RUN, you, &[(1, 0), (2, 1)]));
}

fn ended(rig: &mut Rig, run: SceneRunId, outcome: SceneOutcome) {
    rig.client.on_message(AREA, ServerMsg::SceneEnded { run, outcome });
}

fn confirm(rig: &mut Rig) {
    assert!(rig.client.key(KeyCode::Enter, true, false), "a scene on show takes the key");
}

fn lines(events: &[SceneEvent]) -> Vec<(u32, Option<usize>)> {
    events
        .iter()
        .filter_map(|e| match e {
            SceneEvent::Line { index, answer, .. } => Some((*index, *answer)),
            _ => None,
        })
        .collect()
}

fn step(step: u32, answer: Option<u8>) -> SceneCmd {
    SceneCmd::Step { run: RUN, step, answer }
}

// ---- the owner -----------------------------------------------------------------------------------------------------

#[test]
fn the_owner_plays_the_lines_in_order_and_reports_each_window_once_and_the_end_once() {
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Owner);
    assert!(rig.client.is_active());
    assert_eq!(*rig.built.borrow(), 1, "built once, from the server's own words");
    // Window 0 is open at once (its time is 0); nothing is owed yet.
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(0));
    assert!(rig.client.take_commands().is_empty());
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(0, None)]);
    // Window 1 is 15 frames (half a second) after the previous one opened: not open until then.
    assert!(!rig.client.player().unwrap().has_window());
    rig.client.advance(0.4);
    assert!(!rig.client.player().unwrap().has_window(), "12 frames");
    rig.client.advance(0.2);
    assert!(rig.client.player().unwrap().has_window(), "18 frames: open");
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(1, None)]);
    // The question opens at once; the pointer starts on line 1; Down moves it to line 2; Confirm answers it.
    assert!(rig.client.player().unwrap().has_window());
    assert!(rig.client.key(KeyCode::ArrowDown, true, false));
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(2, Some(2))], "the answer is the absolute line, and it is reported with the step");
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(3, None)]);
    // The scene plays out its last half second and ends: the owner reports it once and is finished.
    rig.client.advance(0.5);
    assert!(rig.client.is_active(), "15 of 30 frames");
    assert!(rig.client.take_commands().is_empty());
    rig.client.advance(0.6);
    assert!(!rig.client.is_active());
    assert_eq!(rig.client.take_commands(), [SceneCmd::End { run: RUN, skipped: false }]);
    // The source heard the answer to the question, and only that one, in the order of the windows.
    assert_eq!(*rig.asked.borrow(), [None, None, None, Some(2), None], "an answer goes to the beat after its question");
    // The events: started, four lines in order; the server's word at the end.
    let events = rig.client.take_events();
    assert!(matches!(&events[0], SceneEvent::Started { run, role: SceneRole::Owner, participants: 2, .. } if *run == RUN));
    assert_eq!(lines(&events), [(0, None), (1, None), (2, Some(2)), (3, None)]);
    ended(&mut rig, RUN, SceneOutcome::Finished);
    assert_eq!(rig.client.take_events(), [SceneEvent::Ended { run: RUN, outcome: SceneOutcome::Finished, lines: 4 }]);
    // A second SceneEnded changes and says nothing, and nothing more is owed.
    ended(&mut rig, RUN, SceneOutcome::Finished);
    assert!(rig.client.take_events().is_empty() && rig.client.take_commands().is_empty());
}

#[test]
fn the_pointer_starts_where_the_script_says_and_an_answer_never_leaves_the_choices() {
    let mut rig = rig_with(vec![ask(0, 18, 18, 1, 2), end(0)], false);
    begin(&mut rig, SceneRole::Owner);
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(0, Some(2)), SceneCmd::End { run: RUN, skipped: false }], "the pointer's own line (2)");
    // A bot answers with a line it is given; one that is not a choice is clamped into them, as the script clamps it.
    let mut rig = rig_with(vec![ask(0, 18, 18, 1, 1), end(0)], false);
    rig.client.set_instant(true);
    begin(&mut rig, SceneRole::Owner);
    rig.client.autoplay(Some(200));
    assert_eq!(rig.client.take_commands(), [step(0, Some(2)), SceneCmd::End { run: RUN, skipped: false }]);
    assert_eq!(*rig.asked.borrow(), [None, Some(2)]);
}

#[test]
fn a_window_of_nothing_is_moved_past_at_once_but_is_a_step_like_any_other() {
    let mut rig = rig_with(vec![blank(0, 18), say(0, 20, 20, 2), end(0)], false);
    begin(&mut rig, SceneRole::Owner);
    assert_eq!(rig.client.take_commands(), [step(0, None)], "the empty window did not wait for a key");
    assert!(rig.client.player().unwrap().has_window(), "the next one is on show");
    confirm(&mut rig);
    assert_eq!(rig.client.take_commands(), [step(1, None), SceneCmd::End { run: RUN, skipped: false }]);
    assert_eq!(lines(&rig.client.take_events()), [(0, None), (1, None)]);
}

#[test]
fn a_multi_page_window_takes_a_confirm_per_page_and_reports_one_step() {
    let long = (0..40).map(|i| format!("line number {i}")).collect::<Vec<_>>().join("\n");
    let pieces: Vec<Piece> = long.split('\n').enumerate().flat_map(|(i, t)| if i == 0 { vec![Piece::Text(t.to_string())] } else { vec![Piece::NewLine, Piece::Text(t.to_string())] }).collect();
    let beat = Beat { stage: vec![], then: Then::Window(window(0, 18, Some(18), Message::new(MessageOrigin::Canon { entity: 18, dialog: 1 }, pieces))) };
    let mut rig = rig_with(vec![beat, end(0)], false);
    begin(&mut rig, SceneRole::Owner);
    let pages = rig.client.player().unwrap().panel().unwrap().position().0;
    assert_eq!(pages, 0);
    let mut presses = 0;
    while rig.client.is_active() {
        confirm(&mut rig);
        presses += 1;
        assert!(presses < 40, "it must end");
    }
    assert!(presses >= 3, "several pages: {presses}");
    assert_eq!(rig.client.take_commands(), [step(0, None), SceneCmd::End { run: RUN, skipped: false }], "one step for the whole window");
}

#[test]
fn skipping_tells_the_server_once_and_nothing_follows_it() {
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Owner);
    confirm(&mut rig);
    rig.client.take_commands();
    // Esc skips the whole scene.
    assert!(rig.client.key(KeyCode::Escape, true, false));
    assert!(!rig.client.is_active(), "finished locally at once");
    assert_eq!(rig.client.take_commands(), [SceneCmd::End { run: RUN, skipped: true }]);
    // Nothing a key, the clock or a repeated skip does can send anything now.
    for code in [KeyCode::Escape, KeyCode::Backspace, KeyCode::Enter] {
        assert!(!rig.client.key(code, true, false), "no scene, no key taken");
    }
    rig.client.advance(5.0);
    assert!(rig.client.take_commands().is_empty());
    // The server's word is reported, and changes nothing.
    ended(&mut rig, RUN, SceneOutcome::Skipped);
    assert_eq!(rig.client.take_events().last(), Some(&SceneEvent::Ended { run: RUN, outcome: SceneOutcome::Skipped, lines: 1 }));
    assert!(rig.client.take_commands().is_empty());
    // Backspace skips too (and a held Esc does not skip twice: only a fresh press counts).
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Owner);
    assert!(rig.client.key(KeyCode::Escape, true, true), "a repeat is swallowed");
    assert!(rig.client.is_active(), "and skips nothing");
    assert!(rig.client.key(KeyCode::Backspace, true, false));
    assert_eq!(rig.client.take_commands(), [SceneCmd::End { run: RUN, skipped: true }]);
}

#[test]
fn the_server_ending_the_run_ends_it_at_once_in_either_role_and_a_repeat_is_nothing() {
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        for outcome in [SceneOutcome::Finished, SceneOutcome::Skipped, SceneOutcome::TimedOut, SceneOutcome::Aborted] {
            let mut rig = rig_with(talk(), false);
            begin(&mut rig, role);
            rig.client.advance(0.1);
            rig.client.take_commands();
            rig.client.take_events();
            ended(&mut rig, RUN, outcome);
            assert!(!rig.client.is_active(), "{role:?} {outcome:?}");
            assert_eq!(rig.client.take_events(), [SceneEvent::Ended { run: RUN, outcome, lines: 0 }]);
            assert!(rig.client.take_commands().is_empty(), "the server's word is never answered");
            ended(&mut rig, RUN, outcome);
            assert!(rig.client.take_events().is_empty() && !rig.client.is_active(), "idempotent");
        }
    }
    // A run that is not this one's is not ours to end.
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Owner);
    ended(&mut rig, SceneRunId(99), SceneOutcome::Aborted);
    assert!(rig.client.is_active());
    // Nor is a step of another run a watcher's to take.
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: SceneRunId(99), step: 0, answer: None });
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(0));
}

// ---- the watcher ---------------------------------------------------------------------------------------------------

#[test]
fn a_watcher_opens_window_zero_by_itself_and_moves_on_only_when_the_owners_steps_arrive_in_order() {
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Watcher);
    assert!(rig.client.player().unwrap().has_window(), "window 0 opened by itself");
    // Its own keys never move it past a window, and never send anything.
    confirm(&mut rig);
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(0));
    // A step that is not the next one is ignored: 1 before 0, and 5.
    for wrong in [1, 5, 99] {
        rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: wrong, answer: None });
        assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(0), "step {wrong}");
    }
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: None });
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(1));
    // Window 1 is half a second away on this machine's clock; the owner moves past it before it has opened here: the step
    // waits for its window, and the window is moved past the moment it opens.
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: None });
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(1), "its window is not open yet");
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 1, answer: None });
    rig.client.advance(1.0);
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(2), "a repeat of the step is still not the next one");
    // The question: it shows the pointer on the script's line; the owner's answer arrives with its step.
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 2, answer: Some(1) });
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(3));
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 3, answer: None });
    rig.client.advance(2.0);
    // Its scene has played to its end: it stands there, finished only by the server.
    assert!(rig.client.is_active() && rig.client.player().unwrap().played_out());
    assert!(rig.client.take_commands().is_empty(), "a watcher never sends a step or an end");
    // The answer reached its own copy of the script: what it was asked, in order.
    assert_eq!(*rig.asked.borrow(), [None, None, None, Some(1), None]);
    assert_eq!(lines(&rig.client.take_events()), [(0, None), (1, None), (2, Some(1)), (3, None)], "the same lines the owner printed");
    ended(&mut rig, RUN, SceneOutcome::Finished);
    assert!(!rig.client.is_active());
}

#[test]
fn a_watcher_that_leaves_says_so_once_and_is_over() {
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Watcher);
    assert!(rig.client.key(KeyCode::Escape, true, false));
    assert!(!rig.client.is_active());
    assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: RUN }]);
    rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: None });
    rig.client.advance(3.0);
    assert!(rig.client.take_commands().is_empty());
}

#[test]
fn a_run_of_empty_windows_without_end_is_cut_off_by_the_scenes_own_limit() {
    let endless = Scripted { initial: places(), beats: (0..1000).map(|_| blank(0, 18)).collect(), asked: Asked::default() };
    let run = Run { id: RUN, participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 }], role: SceneRole::Owner };
    let mut player = ScenePlayer::new(run, def(), Box::new(endless), false);
    let commands = player.take_commands();
    assert!(player.is_over(), "it ended instead of running away");
    assert_eq!(commands.len(), usize::try_from(MAX_SCENE_STEPS).unwrap() + 1);
    assert_eq!(commands.last(), Some(&SceneCmd::Leave { run: RUN }), "a scene that will not end is aborted, once");
}

// ---- time and the stage --------------------------------------------------------------------------------------------

#[test]
fn the_stage_is_where_the_scene_puts_it_when_it_begins_and_moves_with_the_clock_and_a_window_pauses_it() {
    let walk = Stage::Move { entity: 20, from: None, x: 50, y: 110, z: None, frames: 30, how: MoveHow::Walk };
    let beats = vec![
        // Beat 0: she starts to walk at frame 10 (30 frames, 100 units); the first window opens at frame 20.
        Beat { stage: vec![timed(10, walk)], then: Then::Window(window(20, 18, Some(18), message(1, 18, "one"))) },
        // Beat 1: the next window is 5 frames after the first was moved past.
        say(5, 20, 20, 2),
        end(0),
    ];
    let mut rig = rig_with(beats, false);
    begin(&mut rig, SceneRole::Owner);
    let at = |rig: &Rig, entity: u8| rig.client.player().and_then(|p| p.actor(entity)).and_then(|a| a.pos());
    // Placed where the scene says, at the ground's height (the scripted ground is flat at 11).
    assert_eq!(at(&rig, 18), Some([10.0, 10.0, 11.0]));
    assert_eq!(at(&rig, 20), Some([50.0, 10.0, 11.0]));
    rig.client.advance(0.25); // frame 7.5: nothing yet
    assert_eq!(at(&rig, 20), Some([50.0, 10.0, 11.0]));
    rig.client.advance(0.25); // frame 15: five frames into the 30-frame walk
    let y = at(&rig, 20).unwrap()[1];
    assert!((y - (10.0 + 100.0 * 5.0 / 30.0)).abs() < 0.5, "{y}");
    assert!(rig.client.player().unwrap().actor(20).unwrap().moving(), "she walks while she walks");
    assert!(!rig.client.player().unwrap().has_window());
    // A long tick carries the clock past the window's frame 20: the window opens AT frame 20 and the stage stops there,
    // ten frames into the walk, not at the end of the tick.
    rig.client.advance(0.25);
    assert!(rig.client.player().unwrap().has_window());
    let paused = at(&rig, 20).unwrap();
    assert!((paused[1] - (10.0 + 100.0 * 10.0 / 30.0)).abs() < 0.5, "{paused:?}");
    rig.client.advance(10.0);
    assert_eq!(at(&rig, 20), Some(paused), "a window on show pauses the stage: nothing moves while it is read");
    // The window is moved past: the walk goes on from where it stopped, over its remaining frames.
    confirm(&mut rig);
    rig.client.advance(0.1); // 3 of the remaining 20 frames
    let y = at(&rig, 20).unwrap()[1];
    assert!((y - (10.0 + 100.0 * 13.0 / 30.0)).abs() < 0.5, "{y}");
    rig.client.advance(0.2); // the next window is due at frame 5 of this beat: the stage stops there too
    assert!(rig.client.player().unwrap().has_window());
    let y = at(&rig, 20).unwrap()[1];
    assert!((y - (10.0 + 100.0 * 15.0 / 30.0)).abs() < 0.5, "{y}: 5 frames into the beat, 15 into the walk");
    confirm(&mut rig);
    assert_eq!(at(&rig, 20), None, "the scene is over: nobody stands anywhere");
}

#[test]
fn what_the_stage_cannot_show_is_counted_and_the_camera_is_recorded_and_given_back_at_the_end() {
    let anim = Stage::Anim { entity: 20, animation: 2, how: fmv_ff7::script::scene::AnimHow::Once, first: None, last: None, speed: 16 };
    let beats = vec![
        Beat {
            stage: vec![timed(0, anim), timed(0, Stage::Camera(CameraOp::Follow { entity: 20 })), timed(0, Stage::Place { entity: 77, x: 0, y: 0, z: None, triangle: None })],
            then: Then::Window(window(0, 18, Some(18), message(1, 18, "one"))),
        },
        end(0),
    ];
    let mut rig = rig_with(beats, false);
    begin(&mut rig, SceneRole::Owner);
    let player = rig.client.player().unwrap();
    assert_eq!(rig.client.camera(), CameraState::Follow { entity: 20 }, "recorded");
    let counts = player.counts();
    assert_eq!((counts.anims_skipped, counts.camera_ops, counts.off_cast), (1, 1, 1), "counted, not guessed at");
    confirm(&mut rig);
    assert_eq!(rig.client.camera(), CameraState::Default, "the field's own camera again");
    assert!(rig.client.player().is_none());
}

#[test]
fn an_instant_player_has_no_clock_to_wait_for() {
    let mut rig = rig_with(talk(), false);
    rig.client.set_instant(true);
    begin(&mut rig, SceneRole::Owner);
    assert!(rig.client.player().unwrap().has_window(), "window 0 is open as the scene starts");
    rig.client.autoplay(None);
    assert!(!rig.client.is_active(), "every window moved past, at once, and the scene over");
    assert_eq!(
        rig.client.take_commands(),
        [step(0, None), step(1, None), step(2, Some(1)), step(3, None), SceneCmd::End { run: RUN, skipped: false }],
        "the same steps the keys would send, answering the question with the pointer's line"
    );
    // A watcher has nothing to auto-play: the owner's steps move it.
    let mut rig = rig_with(talk(), false);
    rig.client.set_instant(true);
    begin(&mut rig, SceneRole::Watcher);
    rig.client.autoplay(None);
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(0));
    for k in 0..4 {
        rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: k, answer: (k == 2).then_some(2) });
    }
    assert_eq!(rig.client.player().map(ScenePlayer::lines), Some(4));
    assert!(rig.client.take_commands().is_empty());
}

// ---- keys and the field --------------------------------------------------------------------------------------------

fn state() -> ClientState {
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), AreaId { game: GameId::Ff7, key: AREA.to_string() });
    state.scene.set_builder(Box::new(Fake { initial: places(), beats: talk(), asked: Asked::default(), fail: false, built: Rc::default() }));
    state
}

fn trigger(id: u32, x: f32) -> NpcView {
    NpcView { id, name: "beta".into(), model: "aagb".into(), idle_animation: None, pos: Pos::new(x, 0.0, 0.0), facing: 0.0 }
}

fn press(state: &mut ClientState, code: KeyCode) -> SceneRoute {
    route_key(state, AREA, 1.0, false, code, true, false)
}

#[test]
fn while_a_scene_is_on_show_every_key_is_the_scenes_and_nothing_reaches_the_field() {
    let mut state = state();
    assert_eq!(press(&mut state, KeyCode::KeyW), SceneRoute::Field, "no scene: the walk's");
    state.apply(0.0, start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert!(state.scene.is_active());
    for code in [KeyCode::KeyW, KeyCode::KeyA, KeyCode::ArrowLeft, KeyCode::ShiftLeft, KeyCode::KeyB, KeyCode::KeyP, KeyCode::KeyY, KeyCode::KeyM, KeyCode::F1, KeyCode::F6, KeyCode::Tab] {
        assert_eq!(press(&mut state, code), SceneRoute::Scene, "{code:?} is swallowed");
        assert_eq!(route_key(&mut state, AREA, 1.0, false, code, true, true), SceneRoute::Scene, "{code:?} repeating");
        assert_eq!(route_key(&mut state, AREA, 1.0, false, code, false, false), SceneRoute::Scene, "{code:?} let go of");
    }
    assert!(state.scene.is_active(), "none of them ended it");
    // OK is the scene's too: it moves the window, it is no talk and no Interact (no `Press`).
    assert_eq!(press(&mut state, KeyCode::Enter), SceneRoute::Scene);
    assert_eq!(press(&mut state, KeyCode::Space), SceneRoute::Scene);
    assert_eq!(press(&mut state, KeyCode::NumpadEnter), SceneRoute::Scene);
    // The scene sent steps for the windows those keys moved past, and nothing else (no preview from F6).
    let commands = state.scene.take_commands();
    assert!(commands.iter().all(|c| matches!(c, SceneCmd::Step { .. })), "{commands:?}");
    // Esc skips: the keys are the field's from then on.
    state.apply(0.0, start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert_eq!(press(&mut state, KeyCode::Escape), SceneRoute::Scene);
    assert!(!state.scene.is_active());
    assert_eq!(press(&mut state, KeyCode::KeyW), SceneRoute::Field);
    assert_eq!(press(&mut state, KeyCode::Enter), SceneRoute::Field, "no trigger, so the field's own OK");
    assert_eq!(press(&mut state, KeyCode::Escape), SceneRoute::Field, "and the field's own Esc");
}

#[test]
fn an_arrow_held_while_walking_up_does_not_spin_the_pointer_but_one_pressed_in_the_window_does() {
    let mut rig = rig_with(vec![ask(0, 18, 18, 1, 1), end(0)], false);
    begin(&mut rig, SceneRole::Owner);
    let cursor = |rig: &Rig| rig.client.player().and_then(|p| p.panel()).map(|p| p.position().1);
    // The player was holding Down when the scene started: its repeats (never pressed in the window) are not the pointer's.
    assert!(rig.client.key(KeyCode::ArrowDown, true, true));
    assert_eq!(cursor(&rig), Some(0));
    // A fresh press moves it, and then it may repeat.
    assert!(rig.client.key(KeyCode::ArrowDown, true, false));
    assert_eq!(cursor(&rig), Some(1));
    assert!(rig.client.key(KeyCode::ArrowDown, true, true));
    assert_eq!(cursor(&rig), Some(0), "wrapped: it repeats once it was pressed in the window");
    // Let go, and the repeats of a key that is down again but was not pressed fresh are ignored again.
    assert!(rig.client.key(KeyCode::ArrowDown, false, false));
    assert!(rig.client.key(KeyCode::ArrowDown, true, true));
    assert_eq!(cursor(&rig), Some(0));
}

#[test]
fn the_trigger_is_hidden_while_its_scene_runs_and_is_back_when_it_is_over() {
    let npcs = vec![trigger(3, 0.0), trigger(SCENE_NPC_ID_BASE, 10.0), trigger(SCENE_NPC_ID_BASE + 1, 20.0)];
    let mut rig = rig_with(talk(), false);
    assert_eq!(visible_npcs(&npcs, rig.client.player()).len(), 3, "no scene: everybody is drawn");
    assert!(!label_hidden(rig.client.player(), Subject::Npc(SCENE_NPC_ID_BASE)));
    begin(&mut rig, SceneRole::Owner);
    let shown: Vec<u32> = visible_npcs(&npcs, rig.client.player()).iter().map(|n| n.id).collect();
    assert_eq!(shown, [3], "the area's own character stays, the triggers go");
    assert!(label_hidden(rig.client.player(), Subject::Npc(SCENE_NPC_ID_BASE)), "and so does its name");
    assert!(!label_hidden(rig.client.player(), Subject::Npc(3)) && !label_hidden(rig.client.player(), Subject::Own));
    ended(&mut rig, RUN, SceneOutcome::Finished);
    assert_eq!(visible_npcs(&npcs, rig.client.player()).len(), 3, "back again");
    assert!(!label_hidden(rig.client.player(), Subject::Npc(SCENE_NPC_ID_BASE)));
}

#[test]
fn a_fresh_ok_beside_a_trigger_in_reach_and_in_front_starts_its_scene_and_other_presses_do_not() {
    let mut state = state();
    let area = AreaId { game: GameId::Ff7, key: AREA.to_string() };
    let npcs = |list: Vec<fmv_protocol::NpcSnapshot>| ServerMsg::AreaSnapshot { area: area.clone(), tick: 1, entities: vec![], npcs: list };
    let snapshot = |id: u32, x: f32| fmv_protocol::NpcSnapshot { id, name: "beta".into(), model: "aagb".into(), idle_animation: None, pos: Pos::new(x, 0.0, 0.0), facing: 0.0 };
    // A trigger 60 units ahead (to the +x the player faces); its reach is 100.
    assert!(state.apply(0.0, npcs(vec![snapshot(SCENE_NPC_ID_BASE, 60.0), snapshot(4, 5.0)])).is_none());
    assert_eq!(state.own.facing(), 0.0);
    assert_eq!(press(&mut state, KeyCode::Enter), SceneRoute::Press(SCENE_NPC_ID_BASE), "OK in reach and in front");
    assert_eq!(route_key(&mut state, AREA, 1.0, false, KeyCode::Space, true, false), SceneRoute::Press(SCENE_NPC_ID_BASE));
    // A repeat of the key (held) and a release are not a press; neither is any other key.
    assert_eq!(route_key(&mut state, AREA, 1.0, false, KeyCode::Enter, true, true), SceneRoute::Field);
    assert_eq!(route_key(&mut state, AREA, 1.0, false, KeyCode::Enter, false, false), SceneRoute::Field);
    assert_eq!(press(&mut state, KeyCode::KeyE), SceneRoute::Field);
    // A dialogue that is open owns the key.
    assert_eq!(route_key(&mut state, AREA, 1.0, true, KeyCode::Enter, true, false), SceneRoute::Field);
    // Out of reach: the field's own OK (an ordinary talk), not a scene.
    assert!(state.apply(0.0, npcs(vec![snapshot(SCENE_NPC_ID_BASE, 400.0)])).is_none());
    assert_eq!(press(&mut state, KeyCode::Enter), SceneRoute::Field);
    // Behind the player (and not very near): not in front.
    assert!(state.apply(0.0, npcs(vec![snapshot(SCENE_NPC_ID_BASE, -80.0)])).is_none());
    assert_eq!(press(&mut state, KeyCode::Enter), SceneRoute::Field);
    // A client that cannot tell the reach (no builder) leaves the press to the field.
    let mut bare = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), area.clone());
    assert!(bare.apply(0.0, npcs(vec![snapshot(SCENE_NPC_ID_BASE, 60.0)])).is_none());
    assert_eq!(press(&mut bare, KeyCode::Enter), SceneRoute::Field);
}

#[test]
fn f6_asks_for_the_first_scenes_preview_unless_a_scene_or_a_battle_is_on() {
    let mut state = state();
    assert_eq!(press(&mut state, KeyCode::F6), SceneRoute::Preview);
    assert_eq!(state.scene.take_commands(), [SceneCmd::Preview { scene: "ff7/mds7pb_1/tifa-bar".to_string() }]);
    // Held (a repeat) it asks nothing, and with a dialogue open it is not the scene's.
    assert_eq!(route_key(&mut state, AREA, 1.0, false, KeyCode::F6, true, true), SceneRoute::Field);
    assert_eq!(route_key(&mut state, AREA, 1.0, true, KeyCode::F6, true, false), SceneRoute::Field);
    assert!(state.scene.take_commands().is_empty());
}

// ---- a scene this machine cannot play -----------------------------------------------------------------------------------

#[test]
fn a_scene_that_cannot_be_built_releases_the_player_on_the_spot_and_never_locks_the_keys() {
    for (role, release) in [(SceneRole::Owner, SceneCmd::Leave { run: RUN }), (SceneRole::Watcher, SceneCmd::Leave { run: RUN })] {
        let mut rig = rig_with(talk(), true);
        begin(&mut rig, role);
        assert!(!rig.client.is_active(), "{role:?}: no player was made, nothing holds the keys");
        assert_eq!(rig.client.take_commands(), [release], "{role:?}: the right release, once");
        let events = rig.client.take_events();
        assert!(matches!(&events[0], SceneEvent::Started { .. }) && events[1] == SceneEvent::Unavailable, "{events:?}");
        let notes = rig.client.take_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains(SCENE) && notes[0].contains("releasing"), "{notes:?}");
        // The server's word ends it: reported, with no windows, and nothing more is sent.
        ended(&mut rig, RUN, SceneOutcome::Skipped);
        assert_eq!(rig.client.take_events(), [SceneEvent::Ended { run: RUN, outcome: SceneOutcome::Skipped, lines: 0 }]);
        assert!(rig.client.take_commands().is_empty());
    }
    // A client with no builder at all (no game data) does the same.
    let mut client = SceneClient::new();
    client.on_message(AREA, start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert_eq!((client.is_active(), client.take_commands()), (false, vec![SceneCmd::Leave { run: RUN }]));
    // Through the state, the keys stay the field's.
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), AreaId { game: GameId::Ff7, key: AREA.to_string() });
    state.apply(0.0, start(RUN, SceneRole::Watcher, &[(1, 0), (2, 1)]));
    assert_eq!(press(&mut state, KeyCode::KeyW), SceneRoute::Field);
    assert_eq!(state.scene.take_commands(), [SceneCmd::Leave { run: RUN }]);
}

#[test]
fn an_incomplete_interpreter_run_leaves_without_claiming_completion() {
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        for how in [End::Broken, End::Budget, End::Battle, End::MiniGame, End::GameOver] {
            let mut rig = rig_with(vec![Beat { stage: vec![], then: Then::End { at: 0, how } }], false);
            rig.client.set_instant(true);
            begin(&mut rig, role);
            assert!(!rig.client.is_active());
            assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: RUN }], "{role:?}, {how:?}");
        }
    }
}

#[test]
fn a_scene_of_another_field_or_a_story_that_cannot_be_read_is_not_played_either() {
    // The scene plays in `arena`; this player is somewhere else.
    let mut rig = rig_with(talk(), false);
    rig.client.on_message("elsewhere", start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert!(!rig.client.is_active());
    assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: RUN }]);
    // A progression this client cannot read.
    let mut rig = rig_with(talk(), false);
    rig.client.on_message(
        AREA,
        ServerMsg::SceneStart { run: RUN, scene: SCENE.into(), seed: 5, progress: vec![9, 9, 9, 9], owner: EntityId(1), participants: vec![], you: SceneRole::Watcher },
    );
    assert!(!rig.client.is_active());
    assert_eq!(*rig.built.borrow(), 0, "nothing was built from a story that is not readable");
    assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: RUN }]);
}

#[test]
fn a_refusal_is_noted_for_the_window_once() {
    let mut client = SceneClient::new();
    client.on_message(AREA, ServerMsg::SceneRefused { reason: SceneRefusal::Unavailable });
    assert_eq!(client.take_events(), [SceneEvent::Refused(SceneRefusal::Unavailable)]);
    assert_eq!(client.take_refusal(), Some(SceneRefusal::Unavailable));
    assert_eq!(client.take_refusal(), None);
    assert!(!client.is_active() && client.take_commands().is_empty());
}

#[test]
fn a_new_scene_replaces_a_stale_one_and_the_area_changing_ends_it_quietly() {
    let mut rig = rig_with(talk(), false);
    begin(&mut rig, SceneRole::Owner);
    rig.client.on_message(AREA, start(SceneRunId(8), SceneRole::Watcher, &[(1, 0), (2, 1)]));
    assert_eq!(rig.client.player().map(ScenePlayer::run_id), Some(SceneRunId(8)));
    assert_eq!(rig.client.started(), 2);
    assert!(rig.client.take_commands().is_empty(), "the stale run is dropped without a word: the server already moved on");
    let mut state = state();
    state.apply(0.0, start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert!(state.scene.is_active());
    state.change_area(AreaId { game: GameId::Ff7, key: "elsewhere".into() }, Pos::new(1.0, 1.0, 0.0), 0.0, true);
    assert!(!state.scene.is_active());
    assert!(state.scene.take_commands().is_empty());
}

#[test]
fn the_server_echoing_our_own_entity_is_counted_for_the_bots_control_check() {
    let mut state = state();
    assert_eq!(state.own_echo(), (0, None));
    let me = |x: f32| fmv_core::EntitySnapshot {
        id: EntityId(1),
        pos: Pos::new(x, 2.0, 0.0),
        facing: 0.0,
        anim: AnimState::Idle,
        avatar: fmv_core::AvatarAppearance::new("Me", [1, 2, 3], None),
    };
    let other = fmv_core::EntitySnapshot { id: EntityId(5), ..me(9.0) };
    state.apply(0.0, ServerMsg::EntityUpdate { tick: 1, entities: vec![other] });
    assert_eq!(state.own_echo().0, 0, "somebody else's move is not ours");
    state.apply(0.1, ServerMsg::EntityUpdate { tick: 2, entities: vec![me(4.0)] });
    assert_eq!(state.own_echo(), (1, Some(Pos::new(4.0, 2.0, 0.0))));
}

// ---- what is drawn ---------------------------------------------------------------------------------------------------

fn avatar(id: u64, x: f32, appearance: &'static str, own: bool) -> AvatarPlayer<'static> {
    AvatarPlayer { who: Who(id), pos: Pos::new(x, 0.0, 5.0), facing: 1.0, anim: AnimState::Walk, appearance: Some(appearance), npc: None, tint: [9, 9, 9], own, party: false }
}

#[test]
fn every_participant_is_drawn_on_its_witness_spot_and_nobody_else_is_moved() {
    let mut rig = rig_with(talk(), false);
    rig.client.on_message(AREA, start(RUN, SceneRole::Owner, &[(1, 0), (2, 1)]));
    let scene = rig.client.player().unwrap();
    let npc = AvatarPlayer { who: npc_who(0), npc: Some(crate::avatar::frame::NpcRef { model: "aagb", idle: None }), ..avatar(0, 300.0, "x", false) };
    let mut players = vec![npc, avatar(2, 40.0, "ff7/body/slim", false), avatar(3, 70.0, "ff7/body/tall", false), avatar(1, 20.0, "ff7/body/sturdy", true)];
    place_witnesses(&mut players, scene);
    let by = |id: u64| players.iter().find(|p| p.who == Who(id)).unwrap();
    // The local player (spot 0) and the other participant (spot 1) stand on their spots, upright, facing the way the spot faces,
    // at the ground's height there.
    let (me, other) = (by(1), by(2));
    assert_eq!((me.pos.x, me.pos.y, me.pos.z), (-100.0, -72.0, 11.0));
    assert_eq!((other.pos.x, other.pos.y, other.pos.z), (-100.0, -216.0, 11.0));
    assert_eq!(me.facing, fmv_ff7::field_direction_to_facing(0));
    assert_eq!(other.facing, fmv_ff7::field_direction_to_facing(128));
    assert!(me.anim == AnimState::Idle && other.anim == AnimState::Idle, "standing, not walking in place");
    assert!(me.own && me.appearance == Some("ff7/body/sturdy"), "its own look, still the local player");
    // A player that is not in the scene, and an NPC, are exactly where they were.
    assert_eq!((by(3).pos, by(3).anim), (Pos::new(70.0, 0.0, 5.0), AnimState::Walk));
    assert_eq!(by(npc_who(0).0).pos, Pos::new(300.0, 0.0, 5.0));
    // An entity key that equals an NPC's is never the NPC (they are far apart by construction).
    assert!(CAST_WHO_BASE < npc_who(0).0 && CAST_WHO_BASE > u64::from(u32::MAX));
}

#[test]
fn the_cast_is_drawn_as_avatars_under_keys_that_meet_nobody_walking_only_while_a_move_is_on() {
    let walk = Stage::Move { entity: 20, from: None, x: 50, y: 110, z: None, frames: 60, how: MoveHow::Walk };
    let beats = vec![
        Beat { stage: vec![timed(0, walk), timed(0, Stage::Show { entity: 18, visible: false })], then: Then::Window(window(30, 20, Some(20), message(1, 20, "x"))) },
        say(60, 20, 20, 2),
        end(0),
    ];
    let mut rig = rig_with(beats, false);
    begin(&mut rig, SceneRole::Owner);
    let scene = rig.client.player().unwrap();
    // The first one (entity 18) is hidden by the scene; the second (20) is on its way.
    let cast = cast_players(scene);
    assert_eq!(cast.len(), 1, "a hidden actor is not drawn");
    let tifa = &cast[0];
    assert_eq!(tifa.who, cast_who(20));
    assert_eq!((tifa.appearance, tifa.npc.is_none(), tifa.own, tifa.party), (Some("ff7/tifa"), true, false, false));
    assert_eq!(tifa.anim, AnimState::Walk, "walking while the move is on");
    assert!(tifa.who.0 >= CAST_WHO_BASE && tifa.who != Who(1) && tifa.who != npc_who(20) && tifa.who != cast_who(18));
    // The window opens at frame 30 and pauses her half way; read, the walk goes on and ends, and she stands.
    rig.client.advance(2.0);
    assert!(rig.client.player().unwrap().has_window());
    assert_eq!(cast_players(rig.client.player().unwrap())[0].anim, AnimState::Walk, "paused mid-walk, still a walk");
    confirm(&mut rig);
    rig.client.advance(1.0);
    assert_eq!(cast_players(rig.client.player().unwrap())[0].anim, AnimState::Idle, "arrived: standing");
    // A model look is an NPC's way of being drawn.
    let mut modelled = def();
    modelled.cast[1].look = Look::Model { hrc: "aagb".into(), idle: "acfe".into() };
    let source = Scripted { initial: places(), beats: VecDeque::from(vec![say(0, 20, 20, 1)]), asked: Asked::default() };
    let run = Run { id: RUN, participants: vec![], role: SceneRole::Watcher };
    let player = ScenePlayer::new(run, modelled, Box::new(source), false);
    let drawn = cast_players(&player);
    let model = drawn.iter().find(|p| p.who == cast_who(20)).unwrap();
    assert_eq!(model.npc, Some(crate::avatar::frame::NpcRef { model: "aagb", idle: Some("acfe") }));
    assert!(model.appearance.is_none());
}

#[test]
fn the_cast_before_anything_places_it_is_not_drawn_and_a_participant_without_a_spot_keeps_its_place() {
    let source = Scripted { initial: vec![], beats: VecDeque::from(vec![say(0, 20, 20, 1)]), asked: Asked::default() };
    let run = Run { id: RUN, participants: vec![SceneParticipant { entity: EntityId(1), spot: 9 }], role: SceneRole::Owner };
    let player = ScenePlayer::new(run, def(), Box::new(source), false);
    assert!(cast_players(&player).is_empty(), "nobody was placed");
    let mut players = vec![avatar(1, 20.0, "ff7/body/sturdy", true)];
    place_witnesses(&mut players, &player);
    assert_eq!(players[0].pos, Pos::new(20.0, 0.0, 5.0), "no spot 9: it stays where it is");
}

#[test]
fn what_the_server_says_survives_the_trip_and_a_scene_id_is_printed_clean() {
    assert_eq!(SceneCmd::Step { run: RUN, step: 3, answer: Some(2) }.into_msg(), fmv_protocol::ClientMsg::SceneStep { run: RUN, step: 3, answer: Some(2) });
    assert_eq!(SceneCmd::End { run: RUN, skipped: true }.into_msg(), fmv_protocol::ClientMsg::SceneEnd { run: RUN, skipped: true });
    assert_eq!(SceneCmd::Leave { run: RUN }.into_msg(), fmv_protocol::ClientMsg::SceneLeave { run: RUN });
    assert_eq!(SceneCmd::Preview { scene: "a/b/c".into() }.into_msg(), fmv_protocol::ClientMsg::SceneDebugPreview { scene: "a/b/c".into() });
    assert_eq!(clean_id("ff7/mds7pb_1/tifa-bar"), "ff7/mds7pb_1/tifa-bar");
    assert_eq!(clean_id("a b\u{1b}[31m\n"), "a?b??31m?");
    assert_eq!(clean_id(&"x".repeat(100)).len(), 64);
}

#[test]
fn the_scene_keys_are_the_talk_windows() {
    assert_eq!(SceneKey::from_event(KeyCode::Enter, true, false), Some(SceneKey::Confirm));
    assert_eq!(SceneKey::from_event(KeyCode::Space, true, false), Some(SceneKey::Confirm));
    assert_eq!(SceneKey::from_event(KeyCode::NumpadEnter, true, false), Some(SceneKey::Confirm));
    assert_eq!(SceneKey::from_event(KeyCode::Enter, true, true), None, "a held key does not page through a scene");
    assert_eq!(SceneKey::from_event(KeyCode::Escape, true, false), Some(SceneKey::Back));
    assert_eq!(SceneKey::from_event(KeyCode::Backspace, true, false), Some(SceneKey::Back));
    assert_eq!(SceneKey::from_event(KeyCode::KeyW, true, true), Some(SceneKey::Up));
    assert_eq!(SceneKey::from_event(KeyCode::ArrowDown, true, false), Some(SceneKey::Down));
    assert_eq!(SceneKey::from_event(KeyCode::Enter, false, false), None);
}

// ---- where the window goes ---------------------------------------------------------------------------------------------

/// A window whose first line names nobody the scene knows (no speaker).
fn nobody_speaks() -> Beat {
    Beat { stage: vec![], then: Then::Window(window(0, 18, None, message(7, 18, "a note"))) }
}

#[test]
fn the_window_is_anchored_to_its_speakers_head_and_feet_and_knows_who_else_is_in_the_way() {
    use super::draw::speaker_spot;
    use crate::avatar::frame::{tests::view, AvatarFrame, Built};
    use crate::render::view_to_window;
    use crate::ui::draw::DrawList;
    use std::collections::HashMap;
    let view = view();
    let mut rig = rig_with(vec![say(0, 18, 18, 1), end(0)], false);
    rig.client.on_message(AREA, start(RUN, SceneRole::Watcher, &[(1, 0), (2, 1)]));
    let scene = rig.client.player().unwrap();
    assert_eq!(scene.speaker(), Some(18));
    let empty = || AvatarFrame { models: vec![], tints: vec![], identities: vec![], zoom: 400.0, overlay_bias: 0.0, ground: vec![], scenery: None };
    let built = |anchors: HashMap<Who, Pos>| Built { frame: empty(), markers: vec![], anchors };
    let head = Pos::new(12.0, 34.0, 0.0);
    let (vw, vh) = (1100, 680);
    // Only the speaker is drawn as a model: its head is the anchor the frame computed for it, its feet are where the stage
    // stands it, and nobody else is in the way.
    let alone = built(HashMap::from([(cast_who(18), head)]));
    let spot = speaker_spot(scene, Some(&alone), &view, vw, vh).expect("a speaker that is drawn has a place");
    let want_head = view_to_window(&view.bounds, vw, vh, head.x, head.y);
    assert_eq!(spot.head, (want_head[0], want_head[1]));
    let feet = view.to_view(Pos::new(10.0, 10.0, 11.0)).expect("the actor's place is on the screen");
    let want_feet = view_to_window(&view.bounds, vw, vh, feet.x, feet.y);
    assert_eq!(spot.feet, (want_feet[0], want_feet[1]));
    assert!(spot.others.is_empty(), "nobody else is drawn in this frame");
    // The other cast member and both participants (they stand on witness spots) are drawn too: each is a rectangle the window
    // must not hide, around the line from its head to its feet.
    let crowd = built(HashMap::from([
        (cast_who(18), head),
        (cast_who(20), Pos::new(40.0, 50.0, 0.0)),
        (Who(1), Pos::new(-20.0, 30.0, 0.0)),
        (Who(2), Pos::new(0.0, 60.0, 0.0)),
    ]));
    let spot = speaker_spot(scene, Some(&crowd), &view, vw, vh).expect("the speaker is still drawn");
    assert_eq!(spot.others.len(), 3, "the other cast member and both witnesses");
    for r in &spot.others {
        assert!(r.0 < r.2 && r.1 < r.3, "a rectangle with an area: {r:?}");
        assert!(![r.0, r.1, r.2, r.3].iter().any(|v| v.is_nan()));
    }
    // Nobody is anchored when the frame drew no model for the speaker (its model did not load), or there is no frame.
    assert!(speaker_spot(scene, Some(&built(HashMap::new())), &view, vw, vh).is_none());
    assert!(speaker_spot(scene, None, &view, vw, vh).is_none());
    // A window with nobody to speak it is not anchored either.
    let mut quiet = rig_with(vec![nobody_speaks(), end(0)], false);
    quiet.client.on_message(AREA, start(RUN, SceneRole::Watcher, &[(1, 0)]));
    assert_eq!(quiet.client.player().unwrap().speaker(), None);
    assert!(speaker_spot(quiet.client.player().unwrap(), Some(&crowd), &view, vw, vh).is_none());
    // Drawn with a spot the window is above the speaker's head and on the screen; without one it is at the bottom.
    let (mut near, mut bottom) = (DrawList::new(), DrawList::new());
    scene.draw(vw, vh, 0.0, Some(&speaker_spot(scene, Some(&alone), &view, vw, vh).unwrap()), &mut near);
    scene.draw(vw, vh, 0.0, None, &mut bottom);
    let (a, b) = (near.quads()[0], bottom.quads()[0]);
    assert!(a.y + a.h <= want_head[1] as i32, "the window stands above the head ({} <= {})", a.y + a.h, want_head[1]);
    assert!(b.y > a.y, "the one without a speaker is lower, at the bottom of the screen");
}
