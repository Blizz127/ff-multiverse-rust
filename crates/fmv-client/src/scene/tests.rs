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
        arrival: Spot { x: -125, y: -100, triangle: 27, direction: 192 }, presents: None, resume_after_battle: None, presents_bit: None,
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
    /// The built scene is the cutscene of a story point that moves the story to this moment (`SceneDef::presents`).
    presents: Option<u16>,
}

impl SceneBuilder for Fake {
    fn build(&mut self, scene: &str, story: &fmv_ff7::story::StoryState, seed: u32) -> Result<Built, String> {
        assert_eq!((scene, story.moment, seed), (SCENE, 72, 5), "the scene, the story and the seed the server sent");
        *self.built.borrow_mut() += 1;
        if self.fail {
            return Err("test: no data".to_string());
        }
        Ok(Built { def: SceneDef { presents: self.presents, ..def() }, source: Box::new(Scripted { initial: self.initial.clone(), beats: self.beats.clone().into(), asked: Rc::clone(&self.asked) }) })
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
    client.set_builder(Box::new(Fake { initial: places(), beats, asked: Rc::clone(&asked), fail, built: Rc::clone(&built), presents: None }));
    Rig { client, asked, built }
}

/// [`rig_with`] whose scene is the cutscene of a story point (to moment 73).
fn cutscene_rig(beats: Vec<Beat>) -> Rig {
    let (asked, built) = (Asked::default(), Rc::new(RefCell::new(0)));
    let mut client = SceneClient::new();
    client.set_builder(Box::new(Fake { initial: places(), beats, asked: Rc::clone(&asked), fail: false, built: Rc::clone(&built), presents: Some(73) }));
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
    let walk = Stage::Move { entity: 20, from: None, x: 50, y: 110, z: None, triangle: None, frames: 30, how: MoveHow::Walk, animation: None };
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
    let frame = |rig: &Rig, entity| cast_players(rig.client.player().unwrap()).into_iter().find(|p| p.who == cast_who(entity)).unwrap().scene_frame.unwrap();
    assert_eq!(frame(&rig, 20), 10.0, "walk clip counts only time spent walking");
    assert_eq!(frame(&rig, 18), 20.0, "standing cast carries the scene's elapsed time");
    rig.client.advance(10.0);
    assert_eq!(at(&rig, 20), Some(paused), "a window on show pauses the stage: nothing moves while it is read");
    assert_eq!((frame(&rig, 20), frame(&rig, 18)), (10.0, 20.0), "both cast clip clocks pause with the window");
    // The window is moved past: the walk goes on from where it stopped, over its remaining frames.
    confirm(&mut rig);
    rig.client.advance(0.1); // 3 of the remaining 20 frames
    assert_eq!((frame(&rig, 20), frame(&rig, 18)), (13.0, 23.0), "resuming a new beat preserves the clip clocks");
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
fn replacing_moves_and_placing_actors_keeps_the_same_phase_across_tick_sizes() {
    let walk = |at, x| timed(at, Stage::Move { entity: 20, from: None, x, y: 10, z: None, triangle: None, frames: 100, how: MoveHow::Walk, animation: None });
    let beats = vec![Beat {
        stage: vec![
            walk(0, 150),
            walk(5, 250),
            timed(8, Stage::Face { entity: 20, heading: 1.0, frames: 20 }),
            timed(10, Stage::Place { entity: 20, x: 0, y: 10, z: None, triangle: None }),
            walk(12, 100),
            walk(15, 200),
            timed(18, Stage::Face { entity: 20, heading: 2.0, frames: 20 }),
        ],
        then: Then::Window(window(20, 18, Some(18), message(1, 18, "one"))),
    }];
    let mut whole = rig_with(beats.clone(), false);
    let mut split = rig_with(beats, false);
    begin(&mut whole, SceneRole::Owner);
    begin(&mut split, SceneRole::Owner);
    whole.client.advance(1.0); // all seven stage events, stopping at the window
    for _ in 0..4 { split.client.advance(5.0 / 30.0); }
    let a = whole.client.player().unwrap().actor(20).unwrap();
    let b = split.client.player().unwrap().actor(20).unwrap();
    assert_eq!(a, b, "stage and animation phase must not depend on tick size");
    assert_eq!(a.pos(), Some([105.0, 10.0, 11.0]));
    assert_eq!(a.walked_frames(), 18.0, "the placement cancels walking for two frames");
    assert!(whole.client.player().unwrap().has_window() && split.client.player().unwrap().has_window());
}

#[test]
fn authored_loops_start_at_events_pause_and_restart_without_changing_the_base_model() {
    use fmv_ff7::script::scene::AnimHow;
    struct Loops(Scripted);
    impl SceneSource for Loops {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 0 => Some("zero"), 3 => Some("three"), 4 => Some("four"), _ => None }
        }
    }
    let looping = |slot| Stage::Anim { entity: 20, animation: slot, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 };
    let mut unsupported = vec![
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: None, speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: Some(0), last: Some(1), speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 2 },
        Stage::AnimSpeed { entity: 20, speed: 32 },
    ];
    unsupported.push(looping(99));
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut initial = places();
        initial.push(looping(0));
        let mut beats = vec![
            Beat { stage: vec![timed(10, looping(3))], then: say(20, 20, 20, 1).then },
            Beat { stage: vec![timed(5, looping(4))], then: say(10, 20, 20, 2).then },
        ];
        beats.extend(unsupported.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, looping(0)), timed(2, event)], then: say(4, 20, 20, 3).then,
        }));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        let run = Run { id: RUN, participants: vec![], role };
        let mut player = ScenePlayer::new(run, definition, Box::new(Loops(Scripted { initial, beats: beats.into(), asked: Asked::default() })), false);
        let drawn = |p: &ScenePlayer| {
            let actors = cast_players(p);
            let actor = actors.iter().find(|p| p.who == cast_who(20)).unwrap();
            let npc = actor.npc.unwrap();
            assert_eq!((npc.model, npc.idle), ("model", Some("base")), "the base mesh/cache key stays the same");
            actor.scene_clip.map(|(clip, frame)| {
                let crate::avatar::anim::ClipFrame::Loop(frame) = frame else { panic!("expected loop") };
                (clip.to_string(), frame)
            })
        };
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        assert_eq!(drawn(&player), Some(("zero".into(), 0.0)), "setup's loop survives into scene playback");
        player.advance(0.5);
        assert_eq!(drawn(&player), Some(("three".into(), 5.0)));
        player.advance(1.0);
        assert_eq!(drawn(&player), Some(("three".into(), 10.0)));
        player.advance(10.0);
        assert_eq!(drawn(&player), Some(("three".into(), 10.0)), "dialogue freezes the authored pose");
        pass(&mut player, 0);
        player.advance(6.0 / 30.0);
        assert_eq!(drawn(&player), Some(("four".into(), 1.0)), "replacing a clip resets its own origin");
        player.advance(1.0);
        assert_eq!(drawn(&player), Some(("four".into(), 5.0)));
        for index in 0..unsupported.len() {
            pass(&mut player, 1 + index as u32);
            assert_eq!(drawn(&player), Some(("zero".into(), 0.0)));
            player.advance(4.0 / 30.0);
            assert_eq!(drawn(&player), None, "unsupported/unresolved event {index} clears the previous loop");
        }
        assert_eq!(player.counts().anims_skipped, unsupported.len() as u32);
    }
}

#[test]
fn authored_move_clips_reset_pause_and_return_to_standing_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct Moving(Scripted);
    impl SceneSource for Moving {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 0 => Some("idle"), 1 => Some("walk"), 2 => Some("run"), 3 => Some("other"), _ => None }
        }
    }
    let moving = |x, frames, animation| Stage::Move { entity: 20, from: None, x, y: 10, z: None, triangle: None,
        frames, how: MoveHow::Walk, animation };
    let cancel = [
        Stage::AnimSpeed { entity: 20, speed: 32 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut initial = places();
        initial.push(Stage::Anim { entity: 20, animation: 0, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 });
        let mut beats = vec![
            Beat { stage: vec![timed(10, moving(150, 30, Some(1)))], then: say(20, 20, 20, 1).then },
            Beat { stage: vec![timed(5, moving(250, 20, Some(2))), timed(10, moving(350, 10, Some(2)))], then: say(15, 20, 20, 2).then },
            say(10, 20, 20, 3),
        ];
        beats.extend([None, Some(99), Some(3)].map(|animation| Beat {
            stage: vec![timed(0, moving(450, 6, animation))], then: say(3, 20, 20, 4).then,
        }));
        beats.extend(cancel.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, moving(550, 10, Some(1))), timed(2, event)], then: say(4, 20, 20, 5).then,
        }));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        let mut player = ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(Moving(Scripted { initial, beats: beats.into(), asked: Asked::default() })), false);
        let clip = |p: &ScenePlayer| {
            let actors = cast_players(p);
            let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
            let npc = actor.npc.unwrap();
            assert_eq!((npc.model, npc.idle), ("model", Some("base")), "movement keeps the base mesh/cache identity");
            actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
        };
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        assert_eq!(clip(&player), Some(("idle".into(), ClipFrame::Loop(0.0))));
        player.advance(0.5);
        assert_eq!(clip(&player), Some(("walk".into(), ClipFrame::Loop(5.0))), "the move clock begins at its event");
        player.advance(1.0);
        assert!(player.has_window() && player.actor(20).unwrap().moving());
        assert_eq!(clip(&player), Some(("walk".into(), ClipFrame::Loop(10.0))));
        player.advance(10.0);
        assert_eq!(clip(&player), Some(("walk".into(), ClipFrame::Loop(10.0))), "dialogue freezes the move clip");
        pass(&mut player, 0);
        player.advance(6.0 / 30.0);
        assert_eq!(clip(&player), Some(("run".into(), ClipFrame::Loop(1.0))), "a new slot starts with the new Move");
        player.advance(5.0 / 30.0);
        assert_eq!(clip(&player), Some(("run".into(), ClipFrame::Loop(1.0))), "an overlapping replacement restarts even with the same slot");
        player.advance(4.0 / 30.0);
        assert_eq!(clip(&player), Some(("run".into(), ClipFrame::Loop(5.0))));
        pass(&mut player, 1);
        player.advance(5.0 / 30.0);
        assert!(!player.actor(20).unwrap().moving());
        assert_eq!(clip(&player), Some(("idle".into(), ClipFrame::Loop(0.0))), "movement completion removes its clip and restarts standing");
        player.advance(2.0 / 30.0);
        assert_eq!(clip(&player), Some(("idle".into(), ClipFrame::Loop(2.0))));
        player.advance(3.0 / 30.0);
        assert!(player.has_window());
        for index in 0..3 {
            pass(&mut player, 2 + index);
            player.advance(3.0 / 30.0);
            assert!(player.actor(20).unwrap().moving());
            assert_eq!(clip(&player), None, "missing or unsupported move metadata {index} uses the default walk");
        }
        for index in 0..cancel.len() {
            pass(&mut player, 5 + index as u32);
            assert_eq!(clip(&player), Some(("walk".into(), ClipFrame::Loop(0.0))));
            player.advance(4.0 / 30.0);
            assert!(player.actor(20).unwrap().moving());
            assert_eq!(clip(&player), None, "animation replacement {index} cancels the movement override");
        }
    }
}

mod movement_continuity {
    use super::*;
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;

    struct Moving(Scripted);
    impl SceneSource for Moving {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            // Two distinct authored slots can refer to the same file; that is not phase continuity.
            match slot { 0 => Some("idle"), 1 | 2 => Some("stride"), _ => None }
        }
    }

    fn movement(at: u32, from: Option<i16>, x: i16, frames: u32, animation: Option<u8>) -> Timed<Stage> {
        timed(at, Stage::Move { entity: 20, from: from.map(|x| (x, 10)), x, y: 10, z: None,
            triangle: None, frames, how: MoveHow::Walk, animation })
    }

    fn player(role: SceneRole, beats: Vec<Beat>) -> ScenePlayer {
        let mut initial = places();
        initial.push(Stage::Anim { entity: 20, animation: 0, how: AnimHow::Loop,
            first: None, last: None, speed: 16, divisor: 1 });
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(Moving(Scripted { initial, beats: beats.into(), asked: Asked::default() })), false)
    }

    fn clip(player: &ScenePlayer) -> (String, f32) {
        let actors = cast_players(player);
        let actor = actors.iter().find(|a| a.who == cast_who(20)).unwrap();
        let npc = actor.npc.unwrap();
        assert_eq!((npc.model, npc.idle), ("model", Some("base")));
        let (stem, ClipFrame::Loop(frame)) = actor.scene_clip.expect("resolved scene clip") else { panic!("expected loop") };
        (stem.to_string(), frame)
    }

    fn assert_phase(player: &ScenePlayer, stem: &str, expected: f32) {
        let (actual_stem, frame) = clip(player);
        assert_eq!(actual_stem, stem);
        assert!((frame - expected).abs() < 0.0001, "expected phase {expected}, got {frame}");
    }

    fn pass(player: &mut ScenePlayer, role: SceneRole) {
        if role == SceneRole::Owner { player.key(SceneKey::Confirm); } else { player.on_step(0, None); }
    }

    #[test]
    fn contiguous_moves_keep_phase_across_tick_partitions_for_both_roles() {
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for chunks in [vec![24.0], vec![3.0, 7.0, 1.0, 9.0, 4.0]] {
                let mut player = player(role, vec![Beat { stage: vec![
                    movement(2, Some(50), 100, 8, Some(1)),
                    timed(10, Stage::Place { entity: 18, x: 12, y: 10, z: None, triangle: None }),
                    movement(10, Some(100), 200, 10, Some(1)),
                    movement(20, Some(200), 300, 8, Some(1)),
                ], then: say(24, 20, 20, 1).then }, say(10, 20, 20, 2)]);
                for frames in chunks { player.advance(frames / 30.0); }
                assert!(player.has_window() && player.actor(20).unwrap().moving());
                assert_phase(&player, "stride", 22.0);
                assert!((player.actor(20).unwrap().pos().unwrap()[0] - 250.0).abs() < 0.0001);
                player.advance(20.0);
                assert_phase(&player, "stride", 22.0);
                pass(&mut player, role);
                player.advance(6.0 / 30.0);
                assert!(!player.actor(20).unwrap().moving());
                assert_phase(&player, "idle", 2.0);
            }
        }
    }

    #[test]
    fn interrupted_or_unproven_moves_start_a_new_phase() {
        let reset_events = [
            Stage::Place { entity: 20, x: 100, y: 10, z: None, triangle: None },
            Stage::Anim { entity: 20, animation: 0, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 },
            Stage::Anim { entity: 20, animation: 0, how: AnimHow::Hold, first: Some(0), last: Some(0), speed: 16, divisor: 1 },
            Stage::Anim { entity: 20, animation: 99, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 },
            Stage::AnimSpeed { entity: 20, speed: 32 },
        ];
        let first = movement(0, Some(50), 100, 10, Some(1));
        let second = movement(10, Some(100), 200, 10, Some(1));
        let mut cases = vec![
            vec![first.clone(), movement(11, Some(100), 200, 10, Some(1))], // idle gap
            vec![first.clone(), movement(9, Some(100), 200, 10, Some(1))], // interrupted move
            vec![first.clone(), movement(10, Some(101), 200, 10, Some(1))], // disconnected start
            vec![first.clone(), movement(10, None, 200, 10, Some(1))], // unknown start
            vec![first.clone(), movement(10, Some(100), 200, 10, Some(2))], // different slot, same file
            vec![movement(0, Some(50), 100, 0, Some(1)), movement(0, Some(100), 200, 10, Some(1))],
        ];
        cases.extend([None, Some(99), Some(3)].map(|slot| vec![movement(0, Some(50), 100, 10, slot), second.clone()]));
        cases.extend(reset_events.into_iter().map(|event| vec![first.clone(), timed(10, event), second.clone()]));
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for stage in &cases {
                let at = stage.last().unwrap().at + 3;
                let mut player = player(role, vec![Beat { stage: stage.clone(), then: say(at, 20, 20, 1).then }]);
                player.advance(at as f32 / 30.0);
                assert!(player.has_window());
                assert_phase(&player, "stride", 3.0);
            }
        }
    }

    #[test]
    fn normal_blank_and_early_watcher_windows_break_phase_continuity() {
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for at in [5, 10] { // a window during walking, and one precisely on arrival
                for is_blank in [false, true] {
                    let then = if is_blank { blank(at, 20).then } else { say(at, 20, 20, 1).then };
                    let mut player = player(role, vec![
                        Beat { stage: vec![movement(0, Some(50), 100, 10, Some(1))], then },
                        Beat { stage: vec![movement(10, Some(100), 200, 10, Some(1))], then: say(13, 20, 20, 2).then },
                    ]);
                    if is_blank && role == SceneRole::Watcher { player.on_step(0, None); }
                    player.advance(at as f32 / 30.0);
                    if !is_blank {
                        assert!(player.has_window());
                        let frozen = clip(&player);
                        player.advance(20.0);
                        assert_eq!(clip(&player), frozen, "a paused actor keeps its visible phase");
                        pass(&mut player, role);
                    }
                    player.advance(13.0 / 30.0);
                    assert!(player.has_window());
                    assert_phase(&player, "stride", 3.0);
                }
            }
        }
    }
}

mod fmove_loop_phase {
    use super::*;
    use crate::avatar::anim::ClipFrame;
    use super::super::stage::Cast;
    use fmv_ff7::script::scene::AnimHow;

    struct Moving(Scripted);
    impl SceneSource for Moving {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 0 => Some("idle"), 1 => Some("stride"), 3 => Some("other"), _ => None }
        }
    }

    fn looping(slot: u8) -> Stage {
        Stage::Anim { entity: 20, animation: slot, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 }
    }

    fn movement(x: i16, y: i16, z: Option<i16>, frames: u32, how: MoveHow) -> Stage {
        Stage::Move { entity: 20, from: None, x, y, z, triangle: None, frames, how,
            animation: (how == MoveHow::Walk).then_some(1) }
    }

    fn player(role: SceneRole, beats: Vec<Beat>) -> ScenePlayer {
        let mut initial = places();
        initial.push(looping(0));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(Moving(Scripted { initial, beats: beats.into(), asked: Asked::default() })), false)
    }

    fn clip(player: &ScenePlayer) -> Option<(String, ClipFrame)> {
        let actors = cast_players(player);
        let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
        let npc = actor.npc.unwrap();
        assert_eq!((npc.model, npc.idle), ("model", Some("base")), "movement never changes the retained model or fallback idle");
        actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
    }

    fn phase(player: &ScenePlayer, stem: &str, expected: f32) {
        let Some((actual, ClipFrame::Loop(frame))) = clip(player) else { panic!("expected {stem} loop") };
        assert_eq!(actual, stem);
        assert!((frame - expected).abs() < 0.0001, "expected {expected}, got {frame}");
    }

    fn pass(player: &mut ScenePlayer, role: SceneRole, index: u32) {
        if role == SceneRole::Owner { player.key(SceneKey::Confirm); } else { player.on_step(index, None); }
    }

    #[test]
    fn normal_loop_survives_two_segments_pause_and_arrival_then_an_ordinary_move_resets_it() {
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            let mut player = player(role, vec![
                Beat { stage: vec![
                    timed(3, movement(80, 10, None, 6, MoveHow::WalkKeepAnimation)),
                    timed(9, movement(80, 90, Some(31), 8, MoveHow::WalkKeepAnimation)),
                ], then: say(13, 20, 20, 1).then },
                say(10, 20, 20, 2),
                Beat { stage: vec![timed(0, movement(100, 90, Some(31), 4, MoveHow::Walk))], then: say(6, 20, 20, 3).then },
            ]);
            player.advance(2.0 / 30.0);
            phase(&player, "idle", 2.0);
            assert!(!player.actor(20).unwrap().moving());
            player.advance(3.0 / 30.0);
            phase(&player, "idle", 5.0);
            let actor = player.actor(20).unwrap();
            assert!(actor.moving());
            assert_eq!(actor.pos(), Some([60.0, 10.0, 11.0]));
            assert_eq!((actor.heading(), actor.walked_frames()), (0.0, 2.0));
            // One tick crosses the exact segment boundary; the loop advances once, not once per event.
            player.advance(8.0 / 30.0);
            assert!(player.has_window());
            phase(&player, "idle", 13.0);
            let actor = player.actor(20).unwrap();
            assert_eq!(actor.pos(), Some([80.0, 50.0, 21.0]));
            assert!((actor.heading() - std::f32::consts::FRAC_PI_2).abs() < 0.0001);
            assert_eq!(actor.walked_frames(), 10.0);
            player.advance(20.0);
            phase(&player, "idle", 13.0);
            assert_eq!(player.actor(20).unwrap().pos(), Some([80.0, 50.0, 21.0]), "dialogue freezes geometry and phase together");
            pass(&mut player, role, 0);
            player.advance(4.0 / 30.0);
            phase(&player, "idle", 17.0);
            let actor = player.actor(20).unwrap();
            assert!(!actor.moving());
            assert_eq!((actor.pos(), actor.walked_frames()), (Some([80.0, 90.0, 31.0]), 14.0));
            player.advance(6.0 / 30.0);
            phase(&player, "idle", 23.0);
            assert!(player.has_window());
            pass(&mut player, role, 1);
            player.advance(2.0 / 30.0);
            phase(&player, "stride", 2.0);
            player.advance(2.0 / 30.0);
            phase(&player, "idle", 0.0);
            player.advance(2.0 / 30.0);
            phase(&player, "idle", 2.0);
            assert_eq!(player.actor(20).unwrap().walked_frames(), 18.0);
        }
    }

    #[test]
    fn a_single_tick_and_partitioned_ticks_keep_the_same_phase() {
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for chunks in [vec![23.0], vec![2.0, 3.0, 4.0, 8.0, 6.0]] {
                let mut player = player(role, vec![Beat { stage: vec![
                    timed(3, movement(80, 10, None, 6, MoveHow::WalkKeepAnimation)),
                    timed(9, movement(80, 90, Some(31), 8, MoveHow::WalkKeepAnimation)),
                ], then: say(23, 20, 20, 1).then }]);
                for frames in chunks { player.advance(frames / 30.0); }
                phase(&player, "idle", 23.0);
                assert_eq!((player.actor(20).unwrap().pos(), player.actor(20).unwrap().walked_frames()),
                    (Some([80.0, 90.0, 31.0]), 14.0));
            }
        }
    }

    #[test]
    fn replacements_cannot_inherit_the_preserved_clock() {
        let events = [
            (looping(3), Some(("other".to_string(), ClipFrame::Loop(2.0)))),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 16, divisor: 1 },
                Some(("other".to_string(), ClipFrame::HoldFull { elapsed: 2.0 }))),
            (Stage::AnimSpeed { entity: 20, speed: 32 }, None),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 }, None),
            (looping(99), None),
        ];
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for (replacement, after) in &events {
                let mut player = player(role, vec![Beat { stage: vec![
                    timed(2, movement(90, 10, None, 8, MoveHow::WalkKeepAnimation)),
                    timed(5, replacement.clone()),
                ], then: say(12, 20, 20, 1).then }]);
                player.advance(4.0 / 30.0);
                phase(&player, "idle", 4.0);
                player.advance(3.0 / 30.0);
                assert!(player.actor(20).unwrap().moving());
                assert_eq!(clip(&player), None, "replacement {replacement:?} uses the existing fallback while moving");
                player.advance(5.0 / 30.0);
                assert_eq!(&clip(&player), after, "only time after arrival advances replacement {replacement:?}");
            }
        }
    }

    #[test]
    fn holds_once_and_a_hidden_ordinary_move_default_do_not_qualify() {
        let events = [
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(0), speed: 16, divisor: 1 }, false),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(9), speed: 16, divisor: 1 }, false),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 32, divisor: 1 }, false),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: Some(19), speed: 16, divisor: 1 }, true),
            (Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 }, true),
            (movement(70, 10, None, 20, MoveHow::Walk), true),
        ];
        for role in [SceneRole::Owner, SceneRole::Watcher] {
            for (preceding, has_default) in &events {
                let mut player = player(role, vec![Beat { stage: vec![
                    timed(1, preceding.clone()),
                    timed(3, movement(90, 10, None, 5, MoveHow::WalkKeepAnimation)),
                ], then: say(10, 20, 20, 1).then }]);
                player.advance(6.0 / 30.0);
                assert!(player.actor(20).unwrap().moving());
                assert_eq!(clip(&player), None, "preceding {preceding:?} is not an active normal full loop");
                player.advance(4.0 / 30.0);
                assert_eq!(clip(&player), has_default.then(|| ("idle".to_string(), ClipFrame::Loop(2.0))),
                    "the previous cancellation/restoration behavior survives {preceding:?}");
            }
        }
    }

    #[test]
    fn late_motion_catchup_never_ticks_an_already_advanced_loop_again() {
        for (late, x, walked) in [(f32::NAN, 50.0, 0.0), (-3.0, 50.0, 0.0), (2.0, 60.0, 2.0), (f32::INFINITY, 70.0, 4.0)] {
            let mut cast = Cast::new([20]);
            cast.apply(&Stage::Place { entity: 20, x: 50, y: 10, z: Some(11), triangle: None }, 0.0, &|_, _, _| None);
            cast.apply_loop(20, 0, "idle", 7.0);
            cast.advance(3.0);
            cast.apply(&movement(70, 10, Some(11), 4, MoveHow::WalkKeepAnimation), late, &|_, _, _| None);
            let actor = cast.actor(20).unwrap();
            assert_eq!(actor.animation(), Some(("idle", ClipFrame::Loop(10.0))), "late={late}: loop already reached this scene time");
            assert_eq!((actor.pos(), actor.walked_frames()), (Some([x, 10.0, 11.0]), walked));
            cast.advance(3.0);
            assert_eq!(cast.actor(20).unwrap().animation(), Some(("idle", ClipFrame::Loop(13.0))));
            cast.settle();
            assert_eq!(cast.actor(20).unwrap().animation(), Some(("idle", ClipFrame::Loop(13.0))), "infinite settling changes geometry, not finite clip phase");
            assert_eq!(cast.actor(20).unwrap().pos(), Some([70.0, 10.0, 11.0]));
        }
    }
}

#[test]
fn single_frame_holds_stay_fixed_until_replaced_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct Holds(Scripted);
    impl SceneSource for Holds {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            (entity == 20 && slot == 3).then_some("pose")
        }
    }
    let hold = |frame| Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(frame), last: Some(frame), speed: 16, divisor: 1 };
    let replace = [
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(1), last: Some(0), speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: Some(0), speed: 16, divisor: 1 },
        Stage::AnimSpeed { entity: 20, speed: 32 },
        Stage::Anim { entity: 20, animation: 99, how: AnimHow::Hold, first: Some(0), last: Some(0), speed: 16, divisor: 1 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut beats = vec![
            Beat { stage: vec![timed(10, hold(1))], then: say(20, 20, 20, 1).then },
            Beat { stage: vec![timed(1, Stage::Move { entity: 20, from: None, x: 90, y: 10, z: None, triangle: None, frames: 3, how: MoveHow::Walk, animation: None })], then: say(6, 20, 20, 2).then },
            Beat { stage: vec![
                timed(0, Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 }),
                timed(3, hold(0)),
            ], then: say(8, 20, 20, 3).then },
        ];
        beats.extend(replace.iter().cloned().map(|event| Beat { stage: vec![timed(0, hold(1)), timed(2, event)], then: say(4, 20, 20, 4).then }));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        let mut player = ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(Holds(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() })), false);
        let frame = |p: &ScenePlayer| {
            let actors = cast_players(p);
            let actor = actors.iter().find(|a| a.who == cast_who(20)).unwrap();
            let npc = actor.npc.unwrap();
            assert_eq!((npc.model, npc.idle), ("model", Some("base")));
            actor.scene_clip.map(|(stem, frame)| { assert_eq!(stem, "pose"); frame })
        };
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        player.advance(0.5);
        assert_eq!(frame(&player), Some(ClipFrame::Hold(1)), "event overshoot must not advance a held frame");
        player.advance(1.0);
        player.advance(10.0);
        assert_eq!(frame(&player), Some(ClipFrame::Hold(1)), "the same held pose through an open window");
        pass(&mut player, 0);
        player.advance(2.0 / 30.0);
        assert_eq!(frame(&player), None, "movement replaces the held pose");
        player.advance(1.0);
        assert_eq!(frame(&player), None, "the stale hold does not resume after movement");
        pass(&mut player, 1);
        player.advance(2.0 / 30.0);
        assert_eq!(frame(&player), Some(ClipFrame::Loop(2.0)));
        player.advance(1.0);
        assert_eq!(frame(&player), Some(ClipFrame::Hold(0)), "a hold replaces an advancing loop");
        for index in 0..replace.len() {
            pass(&mut player, 2 + index as u32);
            assert_eq!(frame(&player), Some(ClipFrame::Hold(1)));
            player.advance(1.0);
            assert_eq!(frame(&player), None, "unsupported/unresolved replacement {index} clears the hold");
        }
    }
}

#[test]
fn cropped_holds_follow_scene_time_and_cancel_cleanly_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct Ranges(Scripted);
    impl SceneSource for Ranges {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 3 => Some("three"), 4 => Some("four"), _ => None }
        }
    }
    let hold = |animation, first, last| Stage::Anim { entity: 20, animation, how: AnimHow::Hold,
        first: Some(first), last: Some(last), speed: 16, divisor: 1 };
    let unsupported = [
        hold(3, 12, 2),
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: None, speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: Some(2), speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(2), speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(2), speed: 16, divisor: 2 },
        hold(99, 0, 2),
        Stage::AnimSpeed { entity: 20, speed: 32 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut beats = vec![
            Beat { stage: vec![timed(10, hold(3, 2, 12))], then: say(20, 20, 20, 1).then },
            Beat { stage: vec![timed(5, hold(4, 1, 7)), timed(8, hold(4, 1, 7))], then: say(12, 20, 20, 2).then },
            Beat { stage: vec![timed(0, hold(3, 0, 9)), timed(2, Stage::Move { entity: 20, from: None, x: 90, y: 10,
                z: None, triangle: None, frames: 3, how: MoveHow::Walk, animation: None })], then: say(8, 20, 20, 3).then },
            Beat { stage: vec![timed(0, hold(4, 1, 7)), timed(2, hold(3, 4, 4))], then: say(5, 20, 20, 4).then },
        ];
        beats.extend(unsupported.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, hold(3, 2, 12)), timed(2, event)], then: say(5, 20, 20, 5).then,
        }));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        let mut player = ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(Ranges(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() })), false);
        let clip = |p: &ScenePlayer| {
            let actors = cast_players(p);
            let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
            let npc = actor.npc.unwrap();
            assert_eq!((npc.model, npc.idle), ("model", Some("base")), "cropped playback keeps the base mesh/cache identity");
            actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
        };
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        assert_eq!(clip(&player), None);
        player.advance(0.5);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 12, elapsed: 5.0 })), "late ticks count from the event");
        player.advance(1.0);
        assert!(player.has_window());
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 12, elapsed: 10.0 })));
        player.advance(10.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 12, elapsed: 10.0 })), "dialogue freezes the range clock");
        pass(&mut player, 0);
        player.advance(3.0 / 30.0);
        let resumed = clip(&player).unwrap();
        assert_eq!(resumed, ("three".into(), ClipFrame::HoldRange { first: 2, last: 12, elapsed: 13.0 }));
        assert_eq!(resumed.1.in_clip(20), Some(12.0), "the avatar holds the requested last frame without wrapping");
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::HoldRange { first: 1, last: 7, elapsed: 1.0 })), "replacement starts a fresh range clock");
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::HoldRange { first: 1, last: 7, elapsed: 1.0 })), "repeating the same range restarts it too");
        player.advance(3.0 / 30.0);
        assert!(player.has_window());
        pass(&mut player, 1);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 0, last: 9, elapsed: 0.0 })));
        player.advance(3.0 / 30.0);
        assert!(player.actor(20).unwrap().moving());
        assert_eq!(clip(&player), None, "movement replaces the active cropped hold");
        player.advance(1.0);
        assert!(!player.actor(20).unwrap().moving() && player.has_window());
        assert_eq!(clip(&player), None, "the stale range does not resume after movement");
        pass(&mut player, 2);
        player.advance(1.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::Hold(4))), "a single-frame replacement stays the exact Hold variant");
        for index in 0..unsupported.len() {
            pass(&mut player, 3 + index as u32);
            assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 12, elapsed: 0.0 })));
            player.advance(1.0);
            assert_eq!(clip(&player), None, "unsupported/unresolved replacement {index} clears the range");
        }
        assert_eq!(player.counts().anims_skipped, unsupported.len() as u32);
    }
}

#[test]
fn full_holds_follow_scene_time_and_cancel_cleanly_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct FullHolds(Scripted);
    impl SceneSource for FullHolds {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 3 => Some("three"), 4 => Some("four"), _ => None }
        }
    }
    let hold = |animation| Stage::Anim { entity: 20, animation, how: AnimHow::Hold,
        first: None, last: None, speed: 16, divisor: 1 };
    let unsupported = [
        hold(99),
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: None, speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: Some(2), speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 48, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 16, divisor: 2 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 },
        Stage::AnimSpeed { entity: 20, speed: 32 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut beats = vec![
            Beat { stage: vec![timed(10, hold(3))], then: say(20, 20, 20, 1).then },
            Beat { stage: vec![timed(5, hold(4)), timed(8, hold(4))], then: say(12, 20, 20, 2).then },
            Beat { stage: vec![timed(0, hold(3)), timed(2, Stage::Move { entity: 20, from: None, x: 90, y: 10,
                z: None, triangle: None, frames: 3, how: MoveHow::Walk, animation: None })], then: say(8, 20, 20, 3).then },
        ];
        beats.extend(unsupported.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, hold(3)), timed(2, event)], then: say(5, 20, 20, 4).then,
        }));
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        let mut player = ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(FullHolds(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() })), false);
        let clip = |p: &ScenePlayer| {
            let actors = cast_players(p);
            let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
            let npc = actor.npc.unwrap();
            assert_eq!((npc.model, npc.idle), ("model", Some("base")), "full Hold keeps the base mesh/cache identity");
            actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
        };
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        assert_eq!(clip(&player), None);
        player.advance(0.5);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 5.0 })), "elapsed time begins at the event");
        player.advance(1.0);
        assert!(player.has_window());
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 10.0 })));
        player.advance(10.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 10.0 })), "dialogue freezes full Hold playback");
        pass(&mut player, 0);
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 13.0 })), "resuming preserves its elapsed scene time");
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::HoldFull { elapsed: 1.0 })), "a replacement clip starts at its event");
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::HoldFull { elapsed: 1.0 })), "repeating the same full Hold restarts it");
        player.advance(3.0 / 30.0);
        assert!(player.has_window());
        pass(&mut player, 1);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 0.0 })));
        player.advance(3.0 / 30.0);
        assert!(player.actor(20).unwrap().moving());
        assert_eq!(clip(&player), None, "movement cancels the active full Hold");
        player.advance(1.0);
        assert!(!player.actor(20).unwrap().moving() && player.has_window());
        assert_eq!(clip(&player), None, "the canceled full Hold does not resume at arrival");
        assert_eq!(player.counts().anims_skipped, 0, "all resolved normal-rate Holds were supported");
        for index in 0..unsupported.len() {
            pass(&mut player, 2 + index as u32);
            assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldFull { elapsed: 0.0 })));
            player.advance(1.0);
            assert_eq!(clip(&player), None, "unsupported/unresolved replacement {index} clears the full Hold");
            assert_eq!(player.counts().anims_skipped, index as u32 + 1);
        }
    }
}

#[test]
fn doubled_full_holds_pause_replace_and_cancel_for_both_roles() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct Holds(Scripted);
    impl SceneSource for Holds {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 3 => Some("fast"), 4 => Some("normal"), _ => None }
        }
    }
    let hold = |animation, speed| Stage::Anim { entity: 20, animation, how: AnimHow::Hold,
        first: None, last: None, speed, divisor: 1 };
    let cancel = [
        Stage::AnimSpeed { entity: 20, speed: 32 },
        Stage::Move { entity: 20, from: None, x: 90, y: 10, z: None, triangle: None,
            frames: 3, how: MoveHow::Walk, animation: None },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: None, last: None, speed: 32, divisor: 2 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(3), speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 32, divisor: 1 },
        hold(99, 32),
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        for chunks in [vec![11.0], vec![3.0, 4.0, 4.0]] {
            let mut beats = vec![
                Beat { stage: vec![timed(6, hold(3, 32))], then: say(11, 20, 20, 1).then },
                Beat { stage: vec![timed(2, hold(4, 16))], then: say(5, 20, 20, 2).then },
                Beat { stage: vec![timed(0, hold(3, 32)), timed(3, hold(3, 32))], then: say(5, 20, 20, 3).then },
            ];
            beats.extend(cancel.iter().cloned().map(|event| Beat {
                stage: vec![timed(0, hold(3, 32)), timed(2, event)], then: say(6, 20, 20, 4).then,
            }));
            let mut definition = def();
            definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
            let mut player = ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
                Box::new(Holds(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() })), false);
            let clip = |p: &ScenePlayer| {
                let actors = cast_players(p);
                let actor = actors.iter().find(|a| a.who == cast_who(20)).unwrap();
                let npc = actor.npc.unwrap();
                assert_eq!((npc.model, npc.idle), ("model", Some("base")), "the original mesh/cache identity is retained");
                actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
            };
            let pass = |p: &mut ScenePlayer, step| {
                if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
            };
            for frames in chunks { player.advance(frames / 30.0); }
            assert!(player.has_window());
            assert_eq!(clip(&player), Some(("fast".into(), ClipFrame::HoldFull { elapsed: 10.0 })));
            assert_eq!(clip(&player).unwrap().1.in_clip(10), Some(9.0), "a fast full Hold clamps to the actual endpoint");
            player.advance(20.0);
            assert_eq!(clip(&player), Some(("fast".into(), ClipFrame::HoldFull { elapsed: 10.0 })), "dialogue freezes clip time");
            pass(&mut player, 0);
            player.advance(1.0 / 30.0);
            assert_eq!(clip(&player), Some(("fast".into(), ClipFrame::HoldFull { elapsed: 12.0 })));
            player.advance(2.0 / 30.0);
            assert_eq!(clip(&player), Some(("normal".into(), ClipFrame::HoldFull { elapsed: 1.0 })), "replacement resets the rate and origin");
            player.advance(2.0 / 30.0);
            assert_eq!(clip(&player), Some(("normal".into(), ClipFrame::HoldFull { elapsed: 3.0 })));
            pass(&mut player, 1);
            player.advance(5.0 / 30.0);
            assert_eq!(clip(&player), Some(("fast".into(), ClipFrame::HoldFull { elapsed: 4.0 })), "same-clip replacement also restarts");
            assert_eq!(player.counts().anims_skipped, 0);
            for (index, _) in cancel.iter().enumerate() {
                pass(&mut player, 2 + index as u32);
                assert_eq!(clip(&player), Some(("fast".into(), ClipFrame::HoldFull { elapsed: 0.0 })));
                player.advance(6.0 / 30.0);
                assert_eq!(clip(&player), None, "replacement {index} clears the doubled Hold");
                assert!(player.has_window() && !player.actor(20).unwrap().moving());
            }
        }
    }
}

#[test]
fn doubled_full_hold_scales_late_start_once_and_sanitizes_it() {
    use super::stage::Cast;
    use crate::avatar::anim::ClipFrame;
    let mut cast = Cast::new([20]);
    cast.apply_hold_full(20, "fast", 1.25, true);
    assert_eq!(cast.actor(20).unwrap().animation(), Some(("fast", ClipFrame::HoldFull { elapsed: 2.5 })));
    cast.advance(0.75);
    assert_eq!(cast.actor(20).unwrap().animation(), Some(("fast", ClipFrame::HoldFull { elapsed: 4.0 })));
    cast.apply_hold_full(20, "normal", 1.25, false);
    cast.advance(0.75);
    assert_eq!(cast.actor(20).unwrap().animation(), Some(("normal", ClipFrame::HoldFull { elapsed: 2.0 })));
    for overshoot in [-1.0, f32::NAN, f32::INFINITY] {
        cast.apply_hold_full(20, "fast", overshoot, true);
        assert_eq!(cast.actor(20).unwrap().animation(), Some(("fast", ClipFrame::HoldFull { elapsed: 0.0 })));
    }
}

#[test]
fn bounded_once_restores_only_known_defaults_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct OnceSource(Scripted, bool);
    impl SceneSource for OnceSource {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 0 => Some("base"), 3 => Some("three"), 4 => Some("four"), _ => None }
        }
        fn initial_default_animation_clip(&self, entity: u8) -> Option<(u8, &str)> {
            (self.1 && entity == 20).then_some((0, "base"))
        }
    }
    let once = |animation, first, last| Stage::Anim { entity: 20, animation, how: AnimHow::Once,
        first: Some(first), last: Some(last), speed: 16, divisor: 1 };
    let hold = |animation, frame| Stage::Anim { entity: 20, animation, how: AnimHow::Hold,
        first: Some(frame), last: Some(frame), speed: 16, divisor: 1 };
    let looping = |animation| Stage::Anim { entity: 20, animation, how: AnimHow::Loop,
        first: None, last: None, speed: 16, divisor: 1 };
    let make = |role, beats: Vec<Beat>, known_default| {
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(OnceSource(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() }, known_default)), false)
    };
    let clip = |p: &ScenePlayer| {
        let actors = cast_players(p);
        let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
        let npc = actor.npc.unwrap();
        assert_eq!((npc.model, npc.idle), ("model", Some("base")), "restoration never changes the base mesh/cache identity");
        actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame))
    };
    let unsupported = [
        once(0, 0, 2), // Same slot as the known default is deliberately unsupported.
        once(99, 0, 2),
        once(3, 2, 0),
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: Some(2), speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: Some(2), speed: 16, divisor: 2 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: None, speed: 16, divisor: 1 },
    ];
    let invalidators = [
        looping(99),
        Stage::Anim { entity: 20, animation: 4, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 4, how: AnimHow::Loop, first: Some(0), last: Some(1), speed: 16, divisor: 1 },
        Stage::AnimSpeed { entity: 20, speed: 32 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        let mut beats = vec![
            Beat { stage: vec![timed(10, once(3, 2, 5))], then: say(12, 20, 20, 1).then },
            say(5, 20, 20, 2),
            Beat { stage: vec![timed(0, hold(4, 1)), timed(2, once(3, 1, 3))], then: say(10, 20, 20, 3).then },
            Beat { stage: vec![timed(0, once(3, 0, 9)), timed(2, Stage::Move { entity: 20, from: None, x: 90, y: 10,
                z: None, triangle: None, frames: 3, how: MoveHow::Walk, animation: None })], then: say(8, 20, 20, 4).then },
            Beat { stage: vec![timed(0, looping(4)), timed(2, once(3, 2, 2))], then: say(6, 20, 20, 5).then },
            Beat { stage: vec![timed(0, looping(0)), timed(1, once(3, 0, 9)), timed(3, hold(4, 7))], then: say(15, 20, 20, 6).then },
        ];
        beats.extend(unsupported.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, looping(0)), timed(2, event)], then: say(8, 20, 20, 7).then,
        }));
        beats.extend(invalidators.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, looping(0)), timed(1, once(3, 0, 9)), timed(2, event), timed(3, once(3, 0, 2))], then: say(8, 20, 20, 8).then,
        }));
        let mut player = make(role, beats, true);
        assert_eq!(clip(&player), None, "initial default metadata does not create a display override");
        player.advance(11.0 / 30.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 5, elapsed: 1.0 })));
        player.advance(1.0 / 30.0);
        assert!(player.has_window());
        player.advance(10.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 5, elapsed: 2.0 })), "dialogue freezes a pending return");
        pass(&mut player, 0);
        player.advance(2.0 / 30.0);
        assert_eq!(clip(&player), Some(("base".into(), ClipFrame::Loop(0.0))), "inclusive range finishes after four frames and restarts the default");
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("base".into(), ClipFrame::Loop(3.0))));
        pass(&mut player, 1);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::Hold(1))));
        player.advance(3.0 / 30.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 1, last: 3, elapsed: 1.0 })), "Hold replacement does not erase the known default");
        player.advance(1.0);
        assert_eq!(clip(&player), Some(("base".into(), ClipFrame::Loop(5.0))), "only the five frames after completion advance the restarted default");
        pass(&mut player, 2);
        player.advance(3.0 / 30.0);
        assert!(player.actor(20).unwrap().moving());
        assert_eq!(clip(&player), None, "movement cancels Once");
        player.advance(1.0);
        assert_eq!(clip(&player), Some(("base".into(), ClipFrame::Loop(3.0))), "arrival restores the known default without resuming Once");
        pass(&mut player, 3);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::Loop(0.0))));
        player.advance(2.0 / 30.0);
        assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 2, last: 2, elapsed: 0.0 })), "a bounded single-frame Once still has a one-frame duration");
        player.advance(1.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::Loop(3.0))), "runtime DFANM is the new restoration target");
        pass(&mut player, 4);
        player.advance(1.0);
        assert_eq!(clip(&player), Some(("four".into(), ClipFrame::Hold(7))), "a later Hold cancels the pending return even past its old deadline");
        for index in 0..unsupported.len() {
            pass(&mut player, 5 + index as u32);
            player.advance(1.0);
            assert_eq!(clip(&player), None, "unsupported Once {index} falls back without a delayed return");
        }
        for index in 0..invalidators.len() {
            pass(&mut player, 5 + unsupported.len() as u32 + index as u32);
            player.advance(1.0 / 30.0);
            assert_eq!(clip(&player), Some(("three".into(), ClipFrame::HoldRange { first: 0, last: 9, elapsed: 0.0 })));
            player.advance(1.0);
            assert_eq!(clip(&player), None, "invalidation {index} cancels restoration and makes the next Once unsupported");
        }
        assert_eq!(player.counts().anims_skipped, (unsupported.len() + 2 * invalidators.len()) as u32);
        let mut unknown = make(role, vec![Beat {
            stage: vec![timed(0, hold(4, 1)), timed(2, once(3, 0, 2))], then: say(8, 20, 20, 9).then,
        }], false);
        assert_eq!(clip(&unknown), Some(("four".into(), ClipFrame::Hold(1))));
        unknown.advance(1.0);
        assert_eq!(clip(&unknown), None, "a prior Hold cannot stand in for an unknown default");
        assert_eq!(unknown.counts().anims_skipped, 1);
    }
}

#[test]
fn full_once_keeps_scene_time_and_a_verified_return_for_owner_and_watcher() {
    use crate::avatar::anim::ClipFrame;
    use fmv_ff7::script::scene::AnimHow;
    struct FullOnce(Scripted, bool);
    impl SceneSource for FullOnce {
        fn initial(&self) -> Vec<Stage> { self.0.initial() }
        fn next(&mut self, answer: Option<usize>) -> Beat { self.0.next(answer) }
        fn height_at(&self, x: i16, y: i16, triangle: Option<u16>) -> Option<f32> { self.0.height_at(x, y, triangle) }
        fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
            if entity != 20 { return None; }
            match slot { 0 => Some("base"), 3 => Some("three"), 4 => Some("four"), _ => None }
        }
        fn initial_default_animation_clip(&self, entity: u8) -> Option<(u8, &str)> {
            (self.1 && entity == 20).then_some((0, "base"))
        }
    }
    let once = |animation| Stage::Anim { entity: 20, animation, how: AnimHow::Once,
        first: None, last: None, speed: 16, divisor: 1 };
    let hold = |animation, frame| Stage::Anim { entity: 20, animation, how: AnimHow::Hold,
        first: Some(frame), last: Some(frame), speed: 16, divisor: 1 };
    let looping = |animation| Stage::Anim { entity: 20, animation, how: AnimHow::Loop,
        first: None, last: None, speed: 16, divisor: 1 };
    let make = |role, beats: Vec<Beat>, known_default| {
        let mut definition = def();
        definition.cast[1].look = Look::Model { hrc: "model".into(), idle: "base".into() };
        ScenePlayer::new(Run { id: RUN, participants: vec![], role }, definition,
            Box::new(FullOnce(Scripted { initial: places(), beats: beats.into(), asked: Asked::default() }, known_default)), false)
    };
    let drawn = |p: &ScenePlayer| {
        let actors = cast_players(p);
        let actor = actors.iter().find(|actor| actor.who == cast_who(20)).unwrap();
        let npc = actor.npc.unwrap();
        assert_eq!((npc.model, npc.idle), ("model", Some("base")), "full Once retains the original model/cache identity");
        (actor.scene_clip.map(|(stem, frame)| (stem.to_string(), frame)), actor.scene_return.map(str::to_string))
    };
    let active = |clip: &str, elapsed, default: &str| (
        Some((clip.to_string(), ClipFrame::OnceFull { elapsed })), Some(default.to_string()),
    );
    let unsupported = [
        once(0), // The incoming slot must differ from the verified default.
        once(99),
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: Some(0), last: None, speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: Some(2), speed: 16, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 32, divisor: 1 },
        Stage::Anim { entity: 20, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 2 },
    ];
    let invalidators = [
        looping(99),
        Stage::Anim { entity: 20, animation: 4, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let pass = |p: &mut ScenePlayer, step| {
            if role == SceneRole::Owner { p.key(SceneKey::Confirm); } else { p.on_step(step, None); }
        };
        let mut beats = vec![
            Beat { stage: vec![timed(10, once(3))], then: say(40, 20, 20, 1).then },
            Beat { stage: vec![timed(5, once(4)), timed(8, once(4))], then: say(10, 20, 20, 2).then },
            Beat { stage: vec![timed(0, hold(4, 1)), timed(2, once(3))], then: say(8, 20, 20, 3).then },
            Beat { stage: vec![timed(0, once(3)), timed(2, Stage::Move { entity: 20, from: None, x: 90, y: 10,
                z: None, triangle: None, frames: 3, how: MoveHow::Walk, animation: None })], then: say(8, 20, 20, 4).then },
            Beat { stage: vec![timed(0, looping(4)), timed(2, once(3))], then: say(20, 20, 20, 5).then },
            Beat { stage: vec![timed(0, once(3)), timed(3, hold(4, 7))], then: say(20, 20, 20, 6).then },
            Beat { stage: vec![timed(0, looping(0)), timed(1, once(3)), timed(3, Stage::AnimSpeed { entity: 20, speed: 32 }),
                timed(4, once(3))], then: say(8, 20, 20, 7).then },
        ];
        beats.extend(unsupported.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, looping(0)), timed(1, once(3)), timed(2, event)], then: say(8, 20, 20, 8).then,
        }));
        beats.extend(invalidators.iter().cloned().map(|event| Beat {
            stage: vec![timed(0, looping(0)), timed(1, once(3)), timed(2, event), timed(3, once(3))], then: say(8, 20, 20, 9).then,
        }));
        let mut player = make(role, beats, true);
        assert_eq!(drawn(&player), (None, None), "initial default metadata creates neither a display override nor a return");
        player.advance(0.5);
        assert_eq!(drawn(&player), active("three", 5.0, "base"), "elapsed begins at the event, not the render or beat origin");
        player.advance(1.0);
        assert!(player.has_window());
        assert_eq!(drawn(&player), active("three", 30.0, "base"), "stage must not guess completion at the VM's 15-frame estimate");
        player.advance(10.0);
        assert_eq!(drawn(&player), active("three", 30.0, "base"), "dialogue freezes both clip playback and eventual return phase");
        pass(&mut player, 0);
        player.advance(3.0 / 30.0);
        assert_eq!(drawn(&player), active("three", 33.0, "base"));
        player.advance(3.0 / 30.0);
        assert_eq!(drawn(&player), active("four", 1.0, "base"), "a new clip resets elapsed");
        player.advance(3.0 / 30.0);
        assert_eq!(drawn(&player), active("four", 1.0, "base"), "repeating a full Once also resets elapsed");
        player.advance(1.0 / 30.0);
        pass(&mut player, 1);
        assert_eq!(drawn(&player), (Some(("four".into(), ClipFrame::Hold(1))), None), "a Hold cancels the full Once return");
        player.advance(3.0 / 30.0);
        assert_eq!(drawn(&player), active("three", 1.0, "base"), "a transient Hold does not become the return default");
        player.advance(1.0);
        pass(&mut player, 2);
        assert_eq!(drawn(&player), active("three", 0.0, "base"));
        player.advance(3.0 / 30.0);
        assert!(player.actor(20).unwrap().moving());
        assert_eq!(drawn(&player), (None, None), "movement cancels both the full Once and its return descriptor");
        player.advance(1.0);
        assert_eq!(drawn(&player), (Some(("base".into(), ClipFrame::Loop(3.0))), None), "arrival restarts the known default without resuming Once");
        pass(&mut player, 3);
        assert_eq!(drawn(&player), (Some(("four".into(), ClipFrame::Loop(0.0))), None));
        player.advance(3.0 / 30.0);
        assert_eq!(drawn(&player), active("three", 1.0, "four"), "runtime DFANM updates the separately verified return");
        player.advance(1.0);
        assert_eq!(drawn(&player), active("three", 18.0, "four"));
        pass(&mut player, 4);
        player.advance(1.0);
        assert_eq!(drawn(&player), (Some(("four".into(), ClipFrame::Hold(7))), None), "replacement prevents a stale return even beyond the old estimate");
        assert_eq!(player.counts().anims_skipped, 0);
        pass(&mut player, 5);
        player.advance(2.0 / 30.0);
        assert_eq!(drawn(&player), active("three", 1.0, "base"));
        player.advance(1.0 / 30.0);
        assert_eq!(drawn(&player), (None, None), "ASPED cancels the active return descriptor");
        player.advance(1.0);
        assert_eq!(drawn(&player), (None, None), "ASPED also invalidates the default for a subsequent full Once");
        assert_eq!(player.counts().anims_skipped, 2);
        for index in 0..unsupported.len() {
            pass(&mut player, 6 + index as u32);
            player.advance(1.0 / 30.0);
            assert_eq!(drawn(&player), active("three", 0.0, "base"));
            player.advance(1.0);
            assert_eq!(drawn(&player), (None, None), "unsupported full Once {index} clears both descriptors");
        }
        for index in 0..invalidators.len() {
            pass(&mut player, 6 + unsupported.len() as u32 + index as u32);
            player.advance(1.0);
            assert_eq!(drawn(&player), (None, None), "invalid default {index} cannot provide a later return");
        }
        assert_eq!(player.counts().anims_skipped, (2 + unsupported.len() + 2 * invalidators.len()) as u32);
        let mut unknown = make(role, vec![Beat {
            stage: vec![timed(0, hold(4, 1)), timed(2, once(3))], then: say(8, 20, 20, 10).then,
        }], false);
        unknown.advance(1.0);
        assert_eq!(drawn(&unknown), (None, None), "an earlier Hold cannot supply an unknown default");
        assert_eq!(unknown.counts().anims_skipped, 1);
    }
}

#[test]
fn what_the_stage_cannot_show_is_counted_and_the_camera_is_recorded_and_given_back_at_the_end() {
    let anim = Stage::Anim { entity: 20, animation: 2, how: fmv_ff7::script::scene::AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 };
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
    state.scene.set_builder(Box::new(Fake { initial: places(), beats: talk(), asked: Asked::default(), fail: false, built: Rc::default(), presents: None }));
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
    // (the area's own character has a name of its own: one the cast plays is hidden too, see the next test)
    let npcs = vec![NpcView { name: "gamma".into(), ..trigger(3, 0.0) }, trigger(SCENE_NPC_ID_BASE, 10.0), trigger(SCENE_NPC_ID_BASE + 1, 20.0)];
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

/// An area character the scene's cast plays (the same script entity name, any case) is not drawn while the scene is on
/// show: the cast member stands for it where the scene puts it. Characters the cast does not play stay.
#[test]
fn an_area_character_the_cast_plays_is_drawn_once_while_the_scene_runs() {
    let named = |id: u32, name: &str| NpcView { name: name.to_string(), ..trigger(id, 0.0) };
    let npcs = vec![named(1, "ALPHA"), named(2, "gamma"), named(3, "")];
    let mut rig = rig_with(talk(), false);
    assert_eq!(visible_npcs(&npcs, rig.client.player()).len(), 3);
    begin(&mut rig, SceneRole::Owner);
    let shown: Vec<u32> = visible_npcs(&npcs, rig.client.player()).iter().map(|n| n.id).collect();
    assert_eq!(shown, [2, 3], "alpha is the cast's (entity 18); gamma and the unnamed one are not");
    ended(&mut rig, RUN, SceneOutcome::Finished);
    assert_eq!(visible_npcs(&npcs, rig.client.player()).len(), 3, "back again when it is over");
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

/// The cutscene of a story point that starts a battle first ends at that battle (the server then starts it): that is the scene
/// played out, not an incomplete run. The other stops still are.
#[test]
fn a_cutscene_that_ends_at_its_battle_is_played_out() {
    let mut rig = cutscene_rig(vec![Beat { stage: vec![], then: Then::End { at: 0, how: End::Battle } }]);
    rig.client.set_instant(true);
    begin(&mut rig, SceneRole::Owner);
    assert!(!rig.client.is_active());
    assert_eq!(rig.client.take_commands(), [SceneCmd::End { run: RUN, skipped: false }]);
    for how in [End::Broken, End::Budget, End::MiniGame, End::GameOver] {
        let mut rig = cutscene_rig(vec![Beat { stage: vec![], then: Then::End { at: 0, how } }]);
        rig.client.set_instant(true);
        begin(&mut rig, SceneRole::Owner);
        assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: RUN }], "{how:?}");
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
    AvatarPlayer { who: Who(id), pos: Pos::new(x, 0.0, 5.0), facing: 1.0, anim: AnimState::Walk, scene_frame: None, scene_clip: None, scene_return: None, appearance: Some(appearance), npc: None, tint: [9, 9, 9], own, party: false }
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
    let walk = Stage::Move { entity: 20, from: None, x: 50, y: 110, z: None, triangle: None, frames: 60, how: MoveHow::Walk, animation: None };
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

#[test]
fn scene_sound_late_tick_drops_stale_cues_but_keeps_stops_for_both_roles() {
    use super::SoundAction;
    let beats = vec![
        Beat {
            stage: vec![timed(10, Stage::Sound { id: 61, pan: 63 }), timed(40, Stage::Sound { id: 0, pan: 63 })],
            then: say(60, 18, 18, 1).then,
        },
        end(0),
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut rig = rig_with(beats.clone(), false);
        begin(&mut rig, role);
        let actions = |client: &mut SceneClient| client.take_sounds().into_iter().map(|c| c.action).collect::<Vec<_>>();
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Begin, SoundAction::Prepare(61)]);
        rig.client.advance(2.0);
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Play { id: 0, pan: 63 }], "event substeps must not replay a cue that is 50 frames late");
        assert!(rig.client.player().unwrap().has_window());
        rig.client.advance(10.0);
        assert!(actions(&mut rig.client).is_empty(), "the window pauses the remaining scene");
    }
}

#[test]
fn scene_sound_clock_prefetch_pause_stop_and_cancel_follow_both_roles() {
    use super::SoundAction;
    let beats = vec![
        Beat { stage: vec![timed(15, Stage::Sound { id: 61, pan: 63 })], then: say(30, 18, 18, 1).then },
        Beat { stage: vec![timed(15, Stage::Sound { id: 0, pan: 63 }), timed(30, Stage::Sound { id: 59, pan: 127 })], then: end(60).then },
    ];
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let mut rig = rig_with(beats.clone(), false);
        begin(&mut rig, role);
        let actions = |client: &mut SceneClient| client.take_sounds().into_iter().map(|c| c.action).collect::<Vec<_>>();
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Begin, SoundAction::Prepare(61)]);
        rig.client.advance(0.25);
        assert!(actions(&mut rig.client).is_empty());
        rig.client.advance(0.25);
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Play { id: 61, pan: 63 }]);
        rig.client.advance(0.5);
        rig.client.advance(10.0); // the open window pauses the clock, including the sound stop
        assert!(actions(&mut rig.client).is_empty());
        if role == SceneRole::Owner { rig.client.autoplay(None); }
        else { rig.client.on_message(AREA, ServerMsg::SceneStep { run: RUN, step: 0, answer: None }); }
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Prepare(59)]);
        rig.client.advance(0.5);
        assert_eq!(actions(&mut rig.client), vec![SoundAction::Play { id: 0, pan: 63 }]);
        rig.client.end_local(); // area change/disconnect cancels the unplayed tail
        assert_eq!(actions(&mut rig.client), vec![SoundAction::End]);
        rig.client.advance(60.0);
        assert!(actions(&mut rig.client).is_empty());
    }
    for outcome in [SceneOutcome::Skipped, SceneOutcome::TimedOut] {
        let mut rig = rig_with(beats.clone(), false);
        begin(&mut rig, SceneRole::Watcher);
        rig.client.take_sounds();
        rig.client.on_message(AREA, ServerMsg::SceneEnded { run: RUN, outcome });
        assert_eq!(rig.client.take_sounds().iter().map(|c| c.action).collect::<Vec<_>>(), [SoundAction::End]);
    }
}
