//! Tests of the scene interpreter on synthetic scripts (invented, never real game data).

use super::*;
use crate::field::FieldFile;
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::script::FieldScript;
use crate::story::{StoryState, REACTOR_STATE};
use crate::testutil::{FieldSpec, LoaderModelSpec};

// ---- a tiny assembler ------------------------------------------------------------------------

const RET: u8 = 0x00;

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn message(window: u8, dialog: u8) -> Vec<u8> {
    vec![0x40, window, dialog]
}

/// `ASK window dialog first last var` with the answer variable `Var[bank][addr]`.
fn ask(dialog: u8, first: u8, last: u8, bank: u8, addr: u8) -> Vec<u8> {
    vec![0x48, bank, 0, dialog, first, last, addr]
}

fn setbyte(bank: u8, addr: u8, value: u8) -> Vec<u8> {
    vec![0x80, bank << 4, addr, value]
}

fn setword(bank: u8, addr: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0x81, bank << 4, addr, lo, hi]
}

fn biton(bank: u8, addr: u8, bit: u8) -> Vec<u8> {
    vec![0x82, bank << 4, addr, bit]
}

/// `IFUB Var[bank][addr] <op> right` guarding `body` (the jump skips exactly the body).
fn if_ub(bank: u8, addr: u8, op: u8, right: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x14, bank << 4, addr, right, op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}

fn req(op: u8, entity: u8, prio: u8, slot: u8) -> Vec<u8> {
    vec![op, entity, (prio << 5) | slot]
}

fn reqew(entity: u8, slot: u8) -> Vec<u8> {
    req(0x03, entity, 2, slot)
}

fn reqsw(entity: u8, slot: u8) -> Vec<u8> {
    req(0x02, entity, 2, slot)
}

fn wait(frames: u16) -> Vec<u8> {
    let [lo, hi] = frames.to_le_bytes();
    vec![0x24, lo, hi]
}

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn xyzi(x: i16, y: i16, z: i16, triangle: u16) -> Vec<u8> {
    cat(&[vec![0xA5, 0x00, 0x00], words(&[x, y, z]), triangle.to_le_bytes().to_vec()])
}

fn mov(x: i16, y: i16) -> Vec<u8> {
    cat(&[vec![0xA8, 0x00], words(&[x, y])])
}

fn dir(direction: u8) -> Vec<u8> {
    vec![0xB3, 0x00, direction]
}

fn dira(entity: u8) -> Vec<u8> {
    vec![0xB6, entity]
}

fn visi(v: u8) -> Vec<u8> {
    vec![0xA4, v]
}

fn mapjump(field: u16) -> Vec<u8> {
    cat(&[vec![0x60], field.to_le_bytes().to_vec(), words(&[0, 0, 0]), vec![0]])
}

/// A backward jump to its own first byte: the idle loop.
fn idle() -> Vec<u8> {
    vec![0x12, 0x00]
}

/// `RDMSD`: an opcode the VM does not know.
fn unknown_op() -> Vec<u8> {
    vec![0x7F, 0x00, 0x05]
}

/// One entity of a synthetic script: its name and, per slot, the instructions of that script.
type Entity<'a> = (&'a str, Vec<(usize, Vec<Vec<u8>>)>);

fn script(entities: Vec<Entity<'_>>, dialogs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut s = SynthScript { dialogs, ..Default::default() };
    for (name, slots) in entities {
        let slots = slots
            .into_iter()
            .map(|(slot, code)| {
                let mut c = cat(&code);
                c.push(RET);
                (slot, c)
            })
            .collect();
        s = s.entity(name, slots);
    }
    s.build()
}

fn field_of(section: Vec<u8>) -> FieldFile {
    let spec = FieldSpec { overrides: vec![(0, section)], ..FieldSpec::default() };
    FieldFile::parse(spec.build()).expect("a synthetic field parses")
}

#[test]
fn replacement_scene_entities_are_rebound_by_unique_name() {
    let original = script(vec![("dic", vec![]), ("cloud", vec![]), ("barret", vec![]), ("biggs", vec![]), ("jessie", vec![]), ("wedge", vec![])], vec![]);
    let replacement = script(vec![("Wedge", vec![]), ("Jessie", vec![]), ("Biggs", vec![]), ("Barret", vec![]), ("Cloud", vec![]), ("Dic", vec![])], vec![]);
    let original = FieldScript::parse(&original).unwrap();
    let replacement = FieldScript::parse(&replacement).unwrap();
    let mut def = station_scene();
    def.setup.push(ScriptRef::new(1, 3));
    def.root.extend([Step::Await(ScriptRef::new(2, 4)), Step::Start(ScriptRef::new(3, 5)), Step::SetBit { bank: 1, address: 2, bit: 3 }]);
    let rebound = def.rebind_entities(&original, &replacement).unwrap();
    assert_eq!(rebound.trigger.entity, 5);
    assert_eq!(rebound.setup, vec![ScriptRef::new(4, 3)]);
    assert_eq!(rebound.root, vec![Step::StartMain(5), Step::Await(ScriptRef::new(3, 4)), Step::Start(ScriptRef::new(2, 5)), Step::SetBit { bank: 1, address: 2, bit: 3 }]);
    assert_eq!(rebound.cast.iter().map(|m| m.entity).collect::<Vec<_>>(), vec![4, 3, 2, 1, 0]);
    assert_eq!(rebound.speakers.iter().map(|(_, e)| *e).collect::<Vec<_>>(), vec![4, 3, 2, 1, 0]);
    let missing = script(vec![("dic", vec![])], vec![]);
    assert!(def.rebind_entities(&original, &FieldScript::parse(&missing).unwrap()).is_none());
    let ambiguous = script(vec![("dic", vec![]), ("DIC", vec![])], vec![]);
    assert!(def.rebind_entities(&original, &FieldScript::parse(&ambiguous).unwrap()).is_none());
    assert!(def.rebind_entities(&FieldScript::parse(&ambiguous).unwrap(), &replacement).is_none());
}

// ---- running the VM --------------------------------------------------------------------------

fn vm_for(story: StoryState, given: &[Given]) -> Vm {
    Vm::new(SceneVars::new(story, given), 7, Limits::default())
}

/// Every beat of a scene that starts with `root`, answering each question from `answers`.
fn run_root(section: &[u8], root: Vec<Step>, answers: &[usize]) -> (Vec<Beat>, Vm) {
    run_with(section, root, answers, vm_for(StoryState::new_game(), &[]))
}

fn run_with(section: &[u8], root: Vec<Step>, answers: &[usize], mut vm: Vm) -> (Vec<Beat>, Vm) {
    let script = FieldScript::parse(section).unwrap();
    vm.begin(root);
    let (mut out, mut answers, mut given) = (Vec::new(), answers.iter().copied(), None);
    for _ in 0..200 {
        let beat = vm.resume(&script, given.take());
        let asked = matches!(&beat.then, Then::Window(w) if w.message.choice.is_some());
        let done = matches!(beat.then, Then::End { .. });
        out.push(beat);
        if done {
            return (out, vm);
        }
        if asked {
            given = answers.next();
        }
    }
    panic!("a scene never ended");
}

fn start(entity: u8, slot: u8) -> Vec<Step> {
    vec![Step::Start(ScriptRef::new(entity, slot))]
}

#[test]
fn a_director_main_starts_after_initialization_returns() {
    let section = script(vec![("director", vec![(0, vec![message(0, 0), vec![RET], message(0, 1)])])], texts(2));
    let (beats, vm) = run_root(&section, vec![Step::StartMain(0)], &[]);
    assert_eq!(dialogs_of(&beats), [1]);
    assert_eq!(vm.stats().dropped_requests, 0);
}

fn windows(beats: &[Beat]) -> Vec<&Window> {
    beats.iter().filter_map(|b| if let Then::Window(w) = &b.then { Some(w) } else { None }).collect()
}

fn dialogs_of(beats: &[Beat]) -> Vec<usize> {
    windows(beats)
        .iter()
        .map(|w| match w.message.origin {
            crate::script::dialogue::MessageOrigin::Canon { dialog, .. } => dialog,
            crate::script::dialogue::MessageOrigin::Added { .. } => usize::MAX,
        })
        .collect()
}

fn end_of(beats: &[Beat]) -> End {
    match beats.last().map(|b| &b.then) {
        Some(Then::End { how, .. }) => *how,
        other => panic!("the scene did not end: {other:?}"),
    }
}

fn stage(beats: &[Beat]) -> Vec<(u32, Stage)> {
    beats.iter().flat_map(|b| b.stage.iter().map(|t| (t.at, t.what.clone()))).collect()
}

fn texts(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|i| dialog(&format!("Line {i}"))).collect()
}

#[test]
fn getai_reads_known_triangles_and_refuses_unresolved_positions() {
    let getai = |entity, dest| vec![0xB9, 0x06, entity, dest];
    let section = script(vec![("actor", vec![(1, vec![xyzi(10, 20, 0, 42), getai(0, 4), mov(11, 22), getai(0, 6), getai(1, 8)])])], vec![]);
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(vm.writes().get(&(6, 4)), Some(&42));
    assert_eq!(vm.writes().get(&(6, 5)), Some(&0));
    assert!(!vm.writes().contains_key(&(6, 6)), "a move without a triangle cannot supply one");
    assert!(!vm.writes().contains_key(&(6, 8)), "an unknown actor cannot supply one");
    assert_eq!(vm.stats().unresolved, 2);
    assert!(vm.stats().unknown.is_empty());
}

// ---- windows and asks ------------------------------------------------------------------------

#[test]
fn a_script_hands_out_its_windows_in_order_and_the_scene_ends() {
    let section = script(vec![("a", vec![(1, vec![message(0, 2), message(0, 0), message(3, 1)])])], texts(3));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [2, 0, 1]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(windows(&beats).iter().map(|w| w.id).collect::<Vec<_>>(), [0, 0, 3]);
    assert!(windows(&beats).iter().all(|w| w.opener == 0), "the opener is the script's entity");
    assert_eq!(vm.stats().windows, 3);
    assert!(vm.stats().unknown.is_empty() && vm.stats().handled.contains_key("MESSAGE"));
}

#[test]
fn an_ask_waits_for_its_answer_and_the_script_branches_on_it() {
    // dialog 0 asks (lines 1 and 2 are the choices), 1 and 2 are the replies.
    let question = [dialog("Q").split_last().map(|(_, r)| r.to_vec()).unwrap(), vec![0xE7], vec![b'A' - 0x20], vec![0xE7], vec![b'B' - 0x20, 0xFF]].concat();
    let questions = vec![question, dialog("Reply A"), dialog("Reply B")];
    let body = vec![
        setbyte(5, 13, 0),
        ask(0, 1, 2, 5, 13),
        if_ub(5, 13, 0, 1, &message(0, 1)),
        if_ub(5, 13, 0, 2, &message(0, 2)),
    ];
    let section = script(vec![("a", vec![(1, body)])], questions);
    let (beats, _) = run_root(&section, start(0, 1), &[2]);
    assert_eq!(dialogs_of(&beats), [0, 2], "answering line 2 takes the second branch");
    let Then::Window(w) = &beats[0].then else { panic!() };
    let choice = w.message.choice.expect("a question");
    assert_eq!((choice.first, choice.last, choice.cursor), (1, 2, 1));
    let (beats, _) = run_root(&section, start(0, 1), &[1]);
    assert_eq!(dialogs_of(&beats), [0, 1]);
    // no answer at all takes the cursor's starting line
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [0, 1]);
}

// ---- threads, requests and time --------------------------------------------------------------

#[test]
fn reqew_runs_the_requested_script_to_its_end_before_the_requester_goes_on() {
    let a = vec![message(0, 0), reqew(1, 1), message(0, 3)];
    let b = vec![message(0, 1), wait(10), message(0, 2)];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], texts(4));
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [0, 1, 2, 3]);
}

#[test]
fn concurrent_scripts_open_their_windows_in_the_order_of_their_virtual_time() {
    // `a` waits 60 frames and speaks, `b` waits 20 and speaks: b first, though `a` started first.
    let a = vec![wait(60), message(0, 0)];
    let b = vec![wait(20), message(0, 1)];
    let root = vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], texts(2));
    let (beats, _) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [1, 0]);
    let at: Vec<u32> = windows(&beats).iter().map(|w| w.at).collect();
    assert_eq!(at[0], 20, "the first window opens 20 frames in");
    assert_eq!(at[1], 40, "the second 40 frames after the first one opened (60 - 20)");
}

#[test]
fn a_tail_a_script_requested_just_before_it_returned_still_runs() {
    // a requests b with REQSW and returns at once; b's lines come after a's.
    let a = vec![message(0, 0), reqsw(1, 1)];
    let b = vec![message(0, 1)];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], texts(2));
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [0, 1], "the orphaned tail ran");
    assert_eq!(end_of(&beats), End::Finished);
}

#[test]
fn a_background_idle_loop_does_not_hold_the_scene_open() {
    // `bg` loops for ever doing nothing; `a` speaks twice. The scene ends after a's second line.
    let bg = vec![idle()];
    let a = vec![message(0, 0), wait(5), message(0, 1)];
    let root = vec![Step::Start(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))];
    let section = script(vec![("a", vec![(1, a)]), ("bg", vec![(1, bg)])], texts(2));
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [0, 1]);
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.stats().parked >= 1, "the idle loop was parked, not run for ever");
}

#[test]
fn animation_tail_cannot_cut_off_other_work_or_a_waiter() {
    let tail = vec![vec![0xBB, 1, 0, 0, 1], wait(1), vec![0xA3, 1, 1], wait(1), vec![0x12, 14]];
    let section = script(vec![("background", vec![(1, tail.clone())]), ("speaker", vec![(1, vec![wait(20), message(0, 0)])]), ("parked", vec![(1, vec![idle()])])], texts(1));
    let root = vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(2, 1))];
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [0]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(vm.stats().quiescent_tails, 1);
    let section = script(vec![("background", vec![(1, tail)])], vec![]);
    let limits = Limits { frames: 100, ..Limits::default() };
    let vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, limits);
    let (beats, vm) = run_with(&section, vec![Step::Await(ScriptRef::new(0, 1))], &[], vm);
    assert_eq!(end_of(&beats), End::Budget, "a waiter must not be released while its script still loops");
    assert_eq!(vm.stats().quiescent_tails, 0);
}

#[test]
fn a_polling_loop_is_woken_by_the_change_it_waits_for() {
    // `poll` waits for Var[5][1] == 1 (a busy loop), then speaks; `setter` waits 30 frames and sets it.
    // L: IFUB Var[5][1] == 1 (true: fall into JMPF, which skips the loop-back; false: land on the JMPB back to L)
    let poll = vec![if_ub(5, 1, 0, 1, &[0x10, 3]), vec![0x12, 8], message(0, 0)];
    let setter = vec![wait(30), setbyte(5, 1, 1)];
    let root = vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))];
    let section = script(vec![("poll", vec![(1, poll)]), ("setter", vec![(1, setter)])], texts(1));
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [0], "the poller spoke once the variable was set");
    assert_eq!(vm.writes().get(&(5, 1)), Some(&1));
}

#[test]
fn a_request_for_a_taken_priority_slot_waits_for_it() {
    // Two requests for the same entity at the same priority: the second starts when the first has ended.
    let first = vec![message(0, 0), wait(10), message(0, 1)];
    let second = vec![message(0, 2)];
    let root = vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(0, 2))];
    let section = script(vec![("a", vec![(1, first), (2, second)])], texts(3));
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [0, 1, 2], "slot 2 waited for slot 1's priority");
    assert_eq!(vm.stats().queued, 1);
}

#[test]
fn the_trigger_steps_run_in_order_the_bit_first_then_the_awaited_script_then_the_started_one() {
    let first = vec![if_ub(3, 9, 9, 2, &message(0, 0)) /* only if bit 2 of Var[3][9] is on */];
    let second = vec![message(0, 1)];
    let section = script(vec![("a", vec![(1, first)]), ("b", vec![(1, second)])], texts(2));
    let root = vec![Step::SetBit { bank: 3, address: 9, bit: 2 }, Step::Await(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))];
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(dialogs_of(&beats), [0, 1], "the awaited script saw the bit the trigger set before it");
    assert_eq!(vm.writes().get(&(3, 9)), Some(&0b100));
}

// ---- the stage -------------------------------------------------------------------------------

#[test]
fn places_walks_turns_and_visibility_come_out_as_stage_events_with_where_things_stood() {
    let a = vec![xyzi(10, 20, 5, 1), dir(64), mov(40, 20), visi(0), message(0, 0), visi(1)];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    let events = stage(&beats);
    assert!(matches!(events[0].1, Stage::Place { entity: 0, x: 10, y: 20, z: Some(5), triangle: Some(1) }));
    assert!(matches!(events[1].1, Stage::Face { entity: 0, frames: 0, .. }));
    let Stage::Move { from, x, y, frames, how, .. } = events[2].1 else { panic!("{events:?}") };
    assert_eq!((from, x, y, how), (Some((10, 20)), 40, 20, MoveHow::Walk));
    assert_eq!(frames, 15, "30 units at the default walk of 2 a frame");
    assert!(matches!(events[3].1, Stage::Show { entity: 0, visible: false }));
    assert_eq!(events[3].0, 15, "the script waited for the walk to end");
    assert_eq!(vm.place_of(0).map(|w| (w.x, w.y)), Some((40, 20)));
    assert_eq!(vm.visible_of(0), Some(true));
}

#[test]
fn a_script_turns_towards_another_entity_it_knows_the_place_of() {
    let a = vec![xyzi(0, 0, 0, 0), dira(1), message(0, 0)];
    let b = vec![xyzi(0, 100, 0, 0)];
    let root = vec![Step::Await(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], texts(1));
    let (beats, _) = run_root(&section, root, &[]);
    let faces: Vec<f32> = stage(&beats).into_iter().filter_map(|(_, s)| if let Stage::Face { entity: 0, heading, .. } = s { Some(heading) } else { None }).collect();
    assert_eq!(faces.len(), 1);
    assert!((faces[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "b is straight along +y: {faces:?}");
}

#[test]
fn animations_come_out_with_how_they_play_and_the_script_waits_where_the_engine_does() {
    // ANIM!1 (waits, holds) then ANIM!2 (does not wait), DFANM (loops).
    let a = vec![vec![0xAF, 3, 1], vec![0xBA, 4, 1], vec![0xA2, 0, 1], message(0, 0)];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    let anims: Vec<(u32, u8, AnimHow)> = stage(&beats).into_iter().filter_map(|(at, s)| if let Stage::Anim { animation, how, .. } = s { Some((at, animation, how)) } else { None }).collect();
    assert_eq!(anims, [(0, 3, AnimHow::Hold), (15, 4, AnimHow::Hold), (15, 0, AnimHow::Loop)]);
}

#[test]
fn camera_moves_are_recorded_and_the_script_can_wait_for_a_scroll() {
    let a = vec![cat(&[vec![0x66, 0x00, 0x00], words(&[60, -20]), 30u16.to_le_bytes().to_vec()]), vec![0x67], message(0, 0), vec![0x65]];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    let cam: Vec<(u32, CameraOp)> = stage(&beats).into_iter().filter_map(|(at, s)| if let Stage::Camera(c) = s { Some((at, c)) } else { None }).collect();
    assert_eq!(cam[0], (0, CameraOp::Scroll { x: 60, y: -20, frames: 30 }));
    assert_eq!(windows(&beats)[0].at, 30, "SCRLW waited for the scroll");
    assert_eq!(cam[1].1, CameraOp::Reset);
}

// ---- what the VM does not do ------------------------------------------------------------------

#[test]
fn an_opcode_it_does_not_know_is_skipped_and_counted_never_guessed() {
    let a = vec![unknown_op(), unknown_op(), vec![0xF0, 1] /* MUSIC: known to change nothing a scene shows */, message(0, 0)];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [0], "the script went on past what it did not know");
    assert_eq!(vm.stats().unknown.get("RDMSD"), Some(&2));
    assert_eq!(vm.stats().ignored.get("MUSIC"), Some(&1));
    assert!(!vm.stats().handled.contains_key("RDMSD"));
}

#[test]
fn a_map_jump_ends_the_scene_at_once_and_nothing_is_moved() {
    let a = vec![message(0, 0), mapjump(9), message(0, 1)];
    let section = script(vec![("a", vec![(1, a)])], texts(2));
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [0]);
    assert_eq!(end_of(&beats), End::MapJump { field: 9 });
}

#[test]
fn a_loop_that_keeps_changing_things_ends_in_the_budget_not_never() {
    // INC for ever: the variable changes every lap, so it is never "idle".
    let a = vec![vec![0x95, 0x05, 0x01], vec![0x12, 3]];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let limits = Limits { steps: 5_000, ..Limits::default() };
    let (beats, _) = run_with(&section, start(0, 1), &[], Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, limits));
    assert_eq!(end_of(&beats), End::Budget);
}

#[test]
fn too_many_windows_end_the_scene_in_the_budget() {
    let a = vec![message(0, 0), vec![0x12, 3]];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, _) = run_with(&section, start(0, 1), &[], Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, Limits { windows: 5, ..Limits::default() }));
    assert_eq!(windows(&beats).len(), 5);
    assert_eq!(end_of(&beats), End::Budget);
}

#[test]
fn a_script_that_starts_a_short_lived_thread_every_lap_cannot_grow_the_table_for_ever() {
    // `a` starts `b` (which ends at once) and loops: each lap is a change (a thread), so it is never parked and only
    // the limits end it. Neither the live-thread limit nor the step budget alone bounds the table of ended threads.
    let a = vec![req(0x01, 1, 2, 1), vec![0x12, 3]];
    let b = vec![wait(1)];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], texts(1));
    let (beats, vm) = run_with(&section, start(0, 1), &[], Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, Limits { steps: 100_000, ..Limits::default() }));
    assert!(matches!(end_of(&beats), End::Budget | End::Finished));
    assert!(vm.stats().threads <= 4096 + 1, "{} threads were started", vm.stats().threads);
    assert!(vm.stats().dropped_requests > 0, "the requests past the bound were dropped and counted");
}

#[test]
fn damaged_bytes_never_panic_and_end_the_scene() {
    // every opcode byte, then truncations: the VM ends the scene or skips, but never panics
    for op in 0..=255u8 {
        for tail in [vec![], vec![0xFF; 3], vec![0; 12], (0..30).collect::<Vec<u8>>()] {
            let mut code = vec![op];
            code.extend(tail);
            code.push(RET);
            let section = SynthScript { dialogs: texts(2), ..Default::default() }.entity("a", vec![(1, code)]).build();
            let parsed = FieldScript::parse(&section).unwrap();
            let mut vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, Limits { steps: 2_000, windows: 8, threads: 8, frames: 600 });
            vm.begin(start(0, 1));
            for _ in 0..40 {
                if matches!(vm.resume(&parsed, None).then, Then::End { .. }) {
                    break;
                }
            }
        }
    }
}

#[test]
fn the_same_seed_gives_the_same_scene() {
    let a = vec![vec![0x99, 0x05, 0x02] /* RANDOM Var[5][2] */, if_ub(5, 2, 2, 127, &message(0, 0)), message(0, 1)];
    let section = script(vec![("a", vec![(1, a)])], texts(2));
    let run = |seed: u32| {
        let (beats, _) = run_with(&section, start(0, 1), &[], Vm::new(SceneVars::new(StoryState::new_game(), &[]), seed, Limits::default()));
        dialogs_of(&beats)
    };
    assert_eq!(run(5), run(5));
    assert!((1..200).any(|s| run(s) != run(1)), "the seed does feed RANDOM");
}

// ---- variables, the story and what it writes ----------------------------------------------------

#[test]
fn the_moment_comes_from_the_story_and_the_scene_from_what_it_was_found_with() {
    let given = [Given { bank: 3, address: 5, value: 0b10 }];
    let vars = SceneVars::new(StoryState::at(72), &given);
    use crate::script::dialogue::StoryVars;
    assert_eq!(vars.byte(2, 0), 72, "the moment is the player's story's");
    assert_eq!(vars.byte(3, 5), 0b10, "a flag the story does not carry is the scene's");
    assert_eq!(vars.byte(3, 6), 0);
    // a given value never overrides the moment
    let sneaky = SceneVars::new(StoryState::at(72), &[Given { bank: 2, address: 0, value: 9 }]);
    assert_eq!(sneaky.byte(2, 0), 72);
}

#[test]
fn a_branch_on_a_given_flag_is_taken_as_the_scene_was_found() {
    let a = vec![if_ub(3, 5, 9, 1, &message(0, 0)), message(0, 1)];
    let section = script(vec![("a", vec![(1, a)])], texts(2));
    let with = vm_for(StoryState::new_game(), &[Given { bank: 3, address: 5, value: 0b10 }]);
    let (beats, _) = run_with(&section, start(0, 1), &[], with);
    assert_eq!(dialogs_of(&beats), [0, 1]);
    let (beats, _) = run_root(&section, start(0, 1), &[]);
    assert_eq!(dialogs_of(&beats), [1], "without the flag the first window is not opened");
}

#[test]
fn unknown_persistent_reads_are_reported_even_without_a_persistent_write() {
    let branch = if_ub(3, 5, 9, 1, &message(0, 0));
    let field = field_of(script(vec![("a", vec![(1, vec![branch])])], vec![dialog("Hello.")]));
    let mut def = demo_def(vec![member(0, "aya")], start(0, 1));
    def.given = vec![Given { bank: 3, address: 5, value: 2 }];
    let result = run_to_end(&field, &def, &StoryState::at(4), 1, &[]).unwrap();
    assert!(result.writes.untracked.is_empty());
    assert!(result.unknown_persistent_reads.contains(&(3, 5)));
}

#[test]
fn read_audit_accepts_the_moment_and_field_scratch_but_refuses_a_partial_story_byte() {
    use crate::script::dialogue::StoryVars;
    let vars = SceneVars::new(StoryState::at(33), &[]);
    let _ = (vars.byte(2, 0), vars.byte(2, 1), vars.byte(5, 4));
    assert!(vars.unknown_persistent_reads().is_empty());
    let _ = vars.byte(REACTOR_STATE.bank, REACTOR_STATE.address);
    assert!(vars.unknown_persistent_reads().contains(&(REACTOR_STATE.bank, REACTOR_STATE.address)));
}

#[test]
fn read_audit_follows_persistent_inputs_copied_to_field_scratch() {
    // SETBYTE Var[5][7] <- Var[3][5], then test the scratch value.
    let copy = vec![0x80, 0x53, 7, 5];
    let field = field_of(script(vec![("a", vec![(1, vec![copy, if_ub(5, 7, 0, 1, &message(0, 0))])])], vec![dialog("Hello.")]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let result = run_to_end(&field, &def, &StoryState::at(4), 1, &[]).unwrap();
    assert!(result.unknown_persistent_reads.contains(&(3, 5)));
}

#[test]
fn read_audit_conservatively_flags_replacing_an_unknown_persistent_byte() {
    let field = field_of(script(vec![("a", vec![(1, vec![setbyte(3, 5, 1)])])], vec![]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let result = run_to_end(&field, &def, &StoryState::at(4), 1, &[]).unwrap();
    assert!(result.unknown_persistent_reads.contains(&(3, 5)));
}

// ---- the party ------------------------------------------------------------------------------------

/// `IFPRTYQ character`: run `body` only if the character is in the party (the jump skips it otherwise).
fn if_party(character: u8, body: &[u8]) -> Vec<u8> {
    [vec![0xCB, character, u8::try_from(body.len() + 1).unwrap()], body.to_vec()].concat()
}

/// What a script does to the party is RECORDED in its order, changes nothing by itself, and is what a later test of the
/// party in the same scene sees: Barret is in the party the story has at this moment, leaves, is asked for (no window),
/// comes back, and is asked for again (a window).
#[test]
fn party_changes_are_recorded_in_order_and_a_later_test_of_the_party_sees_them() {
    use crate::story::{canon_party, PartyOp, BARRET, CLOUD};
    let story = StoryState::at(4);
    assert_eq!(canon_party(&story), vec![CLOUD, BARRET]);
    let a = [
        if_party(BARRET, &message(0, 0)), // in the story's party: a window
        vec![0xC9, BARRET],               // PRTYM: he leaves
        if_party(BARRET, &message(0, 1)), // not any more: no window
        vec![0xC8, BARRET],               // PRTYP: he is back
        if_party(BARRET, &message(0, 2)), // a window again
        vec![0xCA, CLOUD, 254, 254],      // PRTYE: Cloud alone
        if_party(BARRET, &message(0, 1)), // no window
    ]
    .concat();
    let field = field_of(script(vec![("a", vec![(1, vec![a])])], vec![dialog("One."), dialog("Never."), dialog("Three.")]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let outcome = run_to_end(&field, &def, &story, 1, &[]).unwrap();
    assert_eq!(outcome.windows, 2, "the party as it is at each test, not as the story had it");
    assert_eq!(outcome.party, vec![PartyOp::Remove(BARRET), PartyOp::Add(BARRET), PartyOp::Set([CLOUD, 254, 254])]);
    for stats in [&outcome.setup_stats, &outcome.stats] {
        assert!(stats.unknown.is_empty(), "{:?}", stats.unknown);
        assert!(!["PRTYP", "PRTYM", "PRTYE"].iter().any(|op| stats.ignored.contains_key(op)), "recorded, not skipped: {:?}", stats.ignored);
    }
    assert_eq!((outcome.stats.handled.get("PRTYM"), outcome.stats.handled.get("PRTYP"), outcome.stats.handled.get("PRTYE")), (Some(&1), Some(&1), Some(&1)));
    // The story's party table has no row for "Cloud alone" at this moment: a caller that saves must refuse it.
    assert!(!outcome.party_is_the_storys(&story, &story));
    assert!(outcome.party_is_the_storys(&story, &StoryState::at(36)), "where the table has that party, it is the story's");
}

/// Two editions of a scene tell the same story when they end the same way with the same saved writes, party changes and
/// skipped state instructions, however many windows each takes; and not when one of those differs.
#[test]
fn two_editions_tell_the_same_story_only_when_what_is_kept_is_the_same() {
    let run = |body: Vec<u8>, dialogs: Vec<Vec<u8>>| {
        let field = field_of(script(vec![("a", vec![(1, vec![body])])], dialogs));
        run_to_end(&field, &demo_def(vec![member(0, "aya")], start(0, 1)), &StoryState::at(4), 1, &[]).unwrap()
    };
    let lines = || vec![dialog("One."), dialog("Two.")];
    let original = run([message(0, 0), setword(2, 0, 9), vec![0x3F]].concat(), lines());
    // another wording: one window more, the same write and the same full restore
    let reworded = run([message(0, 0), message(0, 1), setword(2, 0, 9), vec![0x3F]].concat(), lines());
    assert_ne!(original.windows, reworded.windows);
    assert!(original.same_story_as(&reworded) && reworded.same_story_as(&original));
    // a different moment
    assert!(!original.same_story_as(&run([message(0, 0), setword(2, 0, 10), vec![0x3F]].concat(), lines())));
    // no moment at all
    assert!(!original.same_story_as(&run([message(0, 0), vec![0x3F]].concat(), lines())));
    // a party change the other does not make
    assert!(!original.same_story_as(&run([message(0, 0), setword(2, 0, 9), vec![0x3F], vec![0xC9, 1]].concat(), lines())));
    // a full restore the other does not skip announces nothing: the same story
    assert!(original.same_story_as(&run([message(0, 0), setword(2, 0, 9)].concat(), lines())));
    // gil the other does not take (GOLDd 1) could be announced in a window: not the same story
    assert!(!original.same_story_as(&run([message(0, 0), setword(2, 0, 9), vec![0x3F], vec![0x3A, 0, 1, 0, 0, 0]].concat(), lines())));
    // a saved variable the other does not write (bank 3), while field scratch (bank 5) does not count
    assert!(!original.same_story_as(&run([message(0, 0), setword(2, 0, 9), vec![0x3F], setbyte(3, 40, 1)].concat(), lines())));
    assert!(original.same_story_as(&run([message(0, 0), setword(2, 0, 9), vec![0x3F], setbyte(5, 40, 1)].concat(), lines())));
    // a different end
    assert!(!original.same_story_as(&run([message(0, 0), setword(2, 0, 9), vec![0x3F], mapjump(7)].concat(), lines())));
    assert_eq!((original.questions, reworded.questions), (0, 0));
}

/// The list of skipped instructions that touch the player's state names real instructions, every one of which the
/// interpreter does skip (so the list cannot rot into names nothing produces), and the party's three are not in it.
#[test]
fn the_skipped_state_instructions_are_real_and_skipped() {
    let table = crate::script::opcodes::opcode_name_table();
    for name in SKIPPED_STATE_OPS {
        let op = table.iter().find(|(_, m)| m == name).unwrap_or_else(|| panic!("{name} is not an instruction of the table")).0;
        // one instruction of that kind, its operands zero (SPECIAL with a sub-opcode that takes none): the run counts it as skipped
        let width = crate::script::opcodes::info(op).map_or(0, |i| crate::script::opcodes::layout_width(i.fields));
        let mut body = vec![op];
        if *name == "SPECIAL" {
            body.push(0xF9);
        }
        body.resize(body.len() + width, 0);
        let field = field_of(script(vec![("a", vec![(1, vec![body])])], vec![]));
        let outcome = run_to_end(&field, &demo_def(vec![member(0, "aya")], start(0, 1)), &StoryState::at(4), 1, &[]).unwrap();
        assert_eq!(outcome.stats.ignored.get(name), Some(&1), "{name} (0x{op:02X}) is skipped: {:?} / unknown {:?}", outcome.stats.ignored, outcome.stats.unknown);
    }
    assert!(FULL_RESTORE_OPS.iter().all(|op| SKIPPED_STATE_OPS.contains(op)));
    assert!(ANNOUNCED_STATE_OPS.iter().all(|op| SKIPPED_STATE_OPS.contains(op)), "the announced ones are skipped ones");
    assert!(!ANNOUNCED_STATE_OPS.iter().any(|op| FULL_RESTORE_OPS.contains(op) || ["MENU", "SPECIAL", "STTIM", "CKITM"].contains(op)));
    assert!(!["PRTYP", "PRTYM", "PRTYE"].iter().any(|op| SKIPPED_STATE_OPS.contains(op)), "the party's are recorded, not skipped");
}

// ---- the play layer: cast, ground, speakers, writes --------------------------------------------

fn demo_def(cast: Vec<CastMember>, root: Vec<Step>) -> SceneDef {
    SceneDef {
        id: "test/demo".to_string(),
        game: "ff7".to_string(),
        field: "demo".to_string(),
        trigger: Trigger { entity: 0, name: "a".to_string(), slot: 1, talk_radius: 80, moments: 3..=5 },
        setup: Vec::new(),
        given: Vec::new(),
        root,
        cast,
        speakers: vec![("Aya".to_string(), 0), ("Bo".to_string(), 1)],
        witness: vec![Spot { x: 50, y: 10, triangle: 0, direction: 64 }],
        arrival: Spot { x: 50, y: 10, triangle: 0, direction: 64 }, presents: None, resume_after_battle: None, presents_bit: None,
    }
}

#[test]
fn replacement_section_plays_its_own_windows_on_rebound_entities() {
    let original = script(vec![("a", vec![(1, vec![message(0, 0)])]), ("b", vec![])], vec![dialog("Aya")]);
    let line = [dialog("Aya").split_last().unwrap().1.to_vec(), vec![0xE7], dialog("Hello.")].concat();
    let replacement = script(vec![("b", vec![]), ("a", vec![(1, vec![message(0, 1)])])], vec![dialog("unused"), line]);
    let field = field_of(original);
    let def = demo_def(vec![member(0, "aya"), member(1, "bo")], vec![Step::Start(ScriptRef::new(0, 1))]);
    let mut play = ScenePlay::with_script_section(&field, &def, &StoryState::at(4), 1, &replacement).unwrap();
    assert_eq!(play.def().trigger.entity, 1);
    let beat = play.next(None);
    let Then::Window(w) = beat.then else { panic!("expected replacement window") };
    assert_eq!((w.opener, w.speaker), (1, Some(1)));
    assert!(matches!(w.message.origin, crate::script::dialogue::MessageOrigin::Canon { entity: 1, dialog: 1 }));
    let missing = script(vec![("b", vec![])], vec![]);
    assert!(matches!(ScenePlay::with_script_section(&field, &def, &StoryState::at(4), 1, &missing), Err(SceneError::UnmatchedEntities)));
}

fn member(entity: u8, name: &str) -> CastMember {
    CastMember { entity, name: name.to_string(), look: Look::Appearance(format!("test/{name}")), start: None }
}

#[test]
fn the_cast_decides_whose_events_are_kept_and_the_ground_gives_every_place_its_height() {
    // entity 0 (cast) walks and speaks; entity 1 is not in the cast: its walk is dropped and counted.
    let a = vec![xyzi(50, 10, 77, 0), mov(60, 20), message(0, 0)];
    let b = vec![mov(1, 1)];
    let section = script(vec![("a", vec![(1, a)]), ("b", vec![(1, b)])], vec![dialog("Aya"), dialog("Bo")]);
    let field = field_of(section);
    let def = demo_def(vec![member(0, "aya")], vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))]);
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    let beat = play.next(None);
    let moves: Vec<_> = beat.stage.iter().filter(|t| matches!(t.what, Stage::Move { .. })).collect();
    assert_eq!(moves.len(), 1, "{:?}", beat.stage);
    let Stage::Move { z, .. } = moves[0].what else { unreachable!() };
    assert!(z.is_some(), "the walk's end got the ground's height");
    assert_eq!(play.off_cast(), 1);
    let Stage::Place { z, .. } = beat.stage.iter().find(|t| matches!(t.what, Stage::Place { .. })).unwrap().what else { unreachable!() };
    assert_eq!(z, Some(77), "the script's own height is kept");
}

#[test]
fn the_first_line_of_a_window_names_who_speaks() {
    let a = vec![message(0, 0), message(0, 1), message(0, 2)];
    let dialogs = vec![
        // a name, a newline (0xE7), then the quotation: the game's own convention
        [dialog("Aya").split_last().map(|(_, rest)| rest.to_vec()).unwrap(), vec![0xE7], dialog("Hello.")].concat(),
        [dialog("Bo").split_last().map(|(_, rest)| rest.to_vec()).unwrap(), vec![0xE7], dialog("Hi.")].concat(),
        dialog("Nobody speaks here."),
    ];
    let field = field_of(script(vec![("a", vec![(1, a)])], dialogs));
    let def = demo_def(vec![member(0, "aya"), member(1, "bo")], start(0, 1));
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    let mut speakers = Vec::new();
    while let Then::Window(w) = play.next(None).then {
        speakers.push((w.opener, w.speaker));
    }
    assert_eq!(speakers, [(0, Some(0)), (0, Some(1)), (0, None)], "the opener is the script's entity, the speaker the name's");
}

#[test]
fn the_stage_scripts_place_the_cast_and_their_loops_do_not_hold_the_scene() {
    // `setup` puts entity 0 at (10, 20) and turns it; a loop after it would never end.
    let setup = vec![xyzi(10, 20, 0, 0), dir(128), idle()];
    let a = vec![message(0, 0)];
    let section = script(vec![("a", vec![(1, a), (3, setup)])], vec![dialog("Hello.")]);
    let field = field_of(section);
    let mut def = demo_def(vec![member(0, "aya"), CastMember { start: Some(Spot { x: 90, y: 90, triangle: 0, direction: 0 }), ..member(1, "bo") }], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 3)];
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    let initial: Vec<Stage> = play.initial_stage_events().to_vec();
    assert!(initial.iter().any(|s| matches!(s, Stage::Place { entity: 0, x: 10, y: 20, .. })), "{initial:?}");
    assert!(initial.iter().any(|s| matches!(s, Stage::Place { entity: 1, x: 90, y: 90, .. })), "an actor the stage scripts do not place starts where the cast says");
    let beat = play.next(None);
    assert!(matches!(beat.then, Then::Window(_)));
    assert!(matches!(play.next(None).then, Then::End { how: End::Finished, .. }));
}

#[test]
fn headless_outcome_includes_setup_interpreter_observations() {
    let section = script(vec![("a", vec![(1, vec![message(0, 0)]), (3, vec![unknown_op()])])], vec![dialog("Hello.")]);
    let field = field_of(section);
    let mut def = demo_def(vec![member(0, "aya")], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 3)];
    let result = run_to_end(&field, &def, &StoryState::at(4), 1, &[]).unwrap();
    assert_eq!(result.end, End::Finished);
    assert!(result.setup_stats.unknown.contains_key("RDMSD"));
    assert!(result.stats.unknown.is_empty(), "setup and main observations remain distinct");
}

#[test]
fn route_survey_definitions_are_not_registered_for_play() {
    assert!(scene(EARLY_BAR_SCENE_ID).is_none());
    assert!(scene(SECTOR7_STATION_SCENE_ID).is_none());
    assert!(scene(SECTOR7_INTRO_SCENE_ID).is_none());
    assert!(scene(FLOWER_STATION_SCENE_ID).is_none());
    assert!(scene(SECTOR7_PILLAR_SCENE_ID).is_none());
    assert!(scene(CARGO_TRAIN_SCENE_ID).is_none());
    assert!(scene(REACTOR_OPENING_SCENE_ID).is_none());
}

#[test]
fn setup_reports_when_a_script_exceeds_its_budget() {
    let section = script(vec![("a", vec![(3, vec![xyzi(10, 20, 0, 0), dir(128)])])], vec![]);
    let parsed = FieldScript::parse(&section).unwrap();
    let mut vm = Vm::new(SceneVars::new(StoryState::at(4), &[]), 1, Limits { steps: 1, ..Limits::default() });
    vm.setup(&parsed, &[ScriptRef::new(0, 3)]);
    assert!(vm.stats().setup_incomplete);
}

#[test]
fn what_the_scene_writes_is_reported_by_kind_and_applied_forward_only() {
    // the script sets the moment to 80, a tracked bit (the reactor lift) and a field flag of its own
    let a = vec![setword(2, 0, 80), biton(1, 225, 0), biton(3, 213, 2), message(0, 0)];
    let section = script(vec![("a", vec![(1, a)])], vec![dialog("Hello.")]);
    let field = field_of(section);
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let story = StoryState::at(72);
    let outcome = run_to_end(&field, &def, &story, 1, &[]).unwrap();
    assert_eq!(outcome.end, End::Finished);
    assert_eq!(outcome.windows, 1);
    assert_eq!(outcome.writes.moment, Some(80));
    assert_eq!(outcome.writes.tracked, [((REACTOR_STATE.bank, REACTOR_STATE.address), 0b1)]);
    assert_eq!(outcome.writes.untracked.get(&(3, 213)), Some(&0b100), "a field's own flag is not the story's, and is said so");
    assert!(!outcome.writes.is_story_neutral());

    let mut state = story.clone();
    assert!(outcome.writes.apply_to(&mut state));
    assert_eq!(state.moment, 80);
    assert_eq!(state.var(REACTOR_STATE.bank, REACTOR_STATE.address), 1);
    // applied again, it changes nothing (once)
    assert!(!outcome.writes.apply_to(&mut state));
    // and never takes a story back
    let mut later = StoryState::at(90);
    assert!(outcome.writes.apply_to(&mut later), "the tracked bit still goes on");
    assert_eq!(later.moment, 90, "the moment does not go back to 80");
}

#[test]
fn a_scene_that_writes_nothing_the_story_carries_is_story_neutral() {
    let a = vec![biton(3, 213, 2), message(0, 0)];
    let field = field_of(script(vec![("a", vec![(1, a)])], vec![dialog("Hello.")]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let outcome = run_to_end(&field, &def, &StoryState::at(72), 1, &[]).unwrap();
    assert!(outcome.writes.is_story_neutral());
    assert_eq!(outcome.writes.untracked.len(), 1);
    let mut s = StoryState::at(72);
    assert!(!outcome.writes.apply_to(&mut s));
    assert_eq!(s, StoryState::at(72));
}

#[test]
fn untracked_bits_in_a_partly_tracked_byte_are_reported() {
    let a = vec![biton(1, 225, 0), biton(1, 225, 1), message(0, 0)];
    let field = field_of(script(vec![("a", vec![(1, a)])], vec![dialog("Hello.")]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let result = run_to_end(&field, &def, &StoryState::at(72), 1, &[]).unwrap();
    assert_eq!(result.writes.tracked, [((1, 225), 3)]);
    assert_eq!(result.writes.untracked.get(&(1, 225)), Some(&3));
}

#[test]
fn the_answers_a_player_gave_decide_what_the_headless_run_writes() {
    // answering line 2 sets the moment to 90, line 1 leaves it
    let question = [dialog("Q").split_last().map(|(_, r)| r.to_vec()).unwrap(), vec![0xE7], vec![b'A' - 0x20], vec![0xE7], vec![b'B' - 0x20, 0xFF]].concat();
    let body = vec![setbyte(5, 13, 0), ask(0, 1, 2, 5, 13), if_ub(5, 13, 0, 2, &setword(2, 0, 90))];
    let field = field_of(script(vec![("a", vec![(1, body)])], vec![question]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let story = StoryState::at(72);
    assert_eq!(run_to_end(&field, &def, &story, 1, &[2]).unwrap().writes.moment, Some(90));
    assert_eq!(run_to_end(&field, &def, &story, 1, &[1]).unwrap().writes.moment, None);
    assert_eq!(run_to_end(&field, &def, &story, 1, &[]).unwrap().writes.moment, None, "no answer takes the cursor's line (the first)");
}

#[test]
fn a_scene_is_data_and_can_be_played_with_another_cast() {
    let def = proof_scene();
    assert_eq!(def.id, PROOF_SCENE_ID);
    assert!(def.applies_at(72) && !def.applies_at(71) && !def.applies_at(73));
    assert_eq!(def.speaker_entity(" tifa "), Some(proof::TIFA), "a name matches whatever its case");
    assert_eq!(def.speaker_entity("Marlene"), None);
    assert_eq!(scene(PROOF_SCENE_ID).map(|s| s.id), Some(PROOF_SCENE_ID.to_string()));
    assert_eq!(scenes_in_field("MDS7PB_1").len(), 1);
    assert!(scenes_in_field("md1stin").is_empty());
    // the witness has a spot for the owner and for each of two watchers, all different
    assert_eq!(def.witness.len(), 3);
    // another cast keeps the script's entities and changes the actors that play them
    let cosplay = def.clone().with_cast(vec![
        CastMember { entity: proof::CLOUD, name: "cloud".into(), look: Look::Appearance("ff7/barret".into()), start: def.member(proof::CLOUD).and_then(|m| m.start) },
        CastMember { entity: proof::TIFA, name: "tifa".into(), look: Look::Model { hrc: "aaaa".into(), idle: "acfe".into() }, start: None },
    ]);
    assert_eq!(cosplay.trigger, def.trigger);
    assert_eq!(cosplay.root, def.root);
    assert_ne!(cosplay.cast, def.cast);
    assert_eq!(cosplay.member(proof::TIFA).map(|m| m.look.clone()), Some(Look::Model { hrc: "aaaa".into(), idle: "acfe".into() }));
}

#[test]
fn the_trigger_npc_is_found_from_the_stage_script_and_the_models_record() {
    // entity 0's stage script places it; the loader record for its CHAR names the model and the first clip.
    let init = vec![vec![0xA1, 0x00] /* CHAR 0 */, vec![RET]];
    let stage_script = vec![xyzi(30, 40, 9, 0), dir(64)];
    let section = SynthScript { dialogs: texts(1), model_count: 1, scale: 512, ..Default::default() }
        .entity("npc", vec![(0, cat(&init)), (3, cat(&[cat(&stage_script), vec![RET]]))])
        .build();
    let spec = FieldSpec { overrides: vec![(0, section)], models: vec![LoaderModelSpec::new("npc.char", "ABCD.HRC", &["AAFE.aki", "BBBB.aki"])], ..FieldSpec::default() };
    let field = FieldFile::parse(spec.build()).unwrap();
    let mut def = demo_def(vec![member(0, "npc")], Vec::new());
    def.setup = vec![ScriptRef::new(0, 3)];
    let npc = trigger_npc(&field, &def, &StoryState::at(4)).expect("found");
    assert_eq!((npc.entity, npc.name.as_str(), npc.model.as_str(), npc.idle.as_deref()), (0, "a", "abcd", Some("aafe")));
    assert_eq!((npc.x, npc.y), (30, 40));
    assert!((npc.talk_radius - 80.0).abs() < 1e-3, "TALKR 80 at scale 512");
    assert!((npc.heading - 0.0).abs() < 1e-5, "DIR 64 faces +x");
}

// ---- a scene plays once per story, as far as the story carries its mark ------------------------------

#[test]
fn the_proof_scene_is_offered_at_its_moment_until_the_story_carries_its_mark() {
    let def = proof_scene();
    let fresh = StoryState::at(72);
    assert!(def.offered_to(&fresh));
    assert!(!def.offered_to(&StoryState::at(71)) && !def.offered_to(&StoryState::at(73)), "only at its moment");
    // the scene's own first act is to turn on the bar's mark; once the story carries it the scene is begun and not offered
    let mut begun = fresh.clone();
    assert!(begun.set_bit(3, 212, 2, true), "the story model carries the bar's mark");
    assert!(def.begun_in(&begun) && !def.offered_to(&begun));
    // it survives the blob the server stores
    let stored = StoryState::decode(&begun.encode()).unwrap();
    assert!(!def.offered_to(&stored));
    // another bit of the same byte is not the mark
    let mut other_bit = fresh.clone();
    assert!(!other_bit.set_bit(3, 212, 1, true), "the model carries only the one bit");
    assert!(def.offered_to(&other_bit));
}

#[test]
fn a_scene_that_marks_nothing_or_marks_what_the_story_does_not_carry_is_always_offered() {
    let mut begun = StoryState::at(72);
    assert!(begun.set_bit(3, 212, 2, true));
    let mut unmarked = proof_scene();
    unmarked.root = vec![Step::Await(ScriptRef::new(18, 9))];
    assert!(!unmarked.begun_in(&begun) && unmarked.offered_to(&begun));
    let mut not_carried = proof_scene();
    not_carried.root = vec![Step::SetBit { bank: 3, address: 213, bit: 1 }];
    assert!(!not_carried.begun_in(&begun) && not_carried.offered_to(&begun));
    // every mark must be on, not one of them
    let mut two = proof_scene();
    two.root = vec![Step::SetBit { bank: 3, address: 212, bit: 2 }, Step::SetBit { bank: 1, address: 225, bit: 0 }];
    assert!(!two.begun_in(&begun));
    assert!(begun.set_bit(1, 225, 0, true));
    assert!(two.begun_in(&begun));
}

#[test]
fn a_scene_reads_the_bits_the_story_carries_from_the_story_and_the_rest_from_what_it_was_found_with() {
    use crate::script::dialogue::StoryVars;
    let given = [Given { bank: 3, address: 212, value: 0b0000_0010 }, Given { bank: 3, address: 213, value: 0b0000_0001 }];
    let fresh = SceneVars::new(StoryState::at(72), &given);
    assert_eq!(fresh.byte(3, 212), 0b010, "found with bit 1; the story has not marked bit 2");
    let mut story = StoryState::at(72);
    assert!(story.set_bit(3, 212, 2, true));
    let begun = SceneVars::new(story, &given);
    assert_eq!(begun.byte(3, 212), 0b110, "the story's own bit joins what the scene was found with");
    assert_eq!(begun.byte(3, 213), 0b1, "a flag the story does not carry is what the scene was found with");
    assert_eq!(begun.byte(3, 214), 0, "and one it knows nothing of is 0");
}

#[test]
fn victory_continuation_resumes_the_waiting_parent_after_the_matching_battle() {
    let bytes = script(vec![
        ("parent", vec![(1, vec![message(0, 0), reqew(1, 1), message(0, 3)])]),
        ("child", vec![(1, vec![message(0, 1), vec![0x70, 0, 0x44, 1], message(0, 2)])]),
    ], texts(4));
    let field = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm_for(StoryState::new_game(), &[]);
    vm.begin(start(0, 1));
    assert!(vm.seek_after_battle(&field, 324));
    let beats = vec![vm.resume(&field, None), vm.resume(&field, None), vm.resume(&field, None)];
    assert_eq!(dialogs_of(&beats), [2, 3]);
    assert!(matches!(beats[2].then, Then::End { how: End::Finished, .. }));
    assert_eq!(vm.stats().handled.get("BATTLE"), Some(&1));
}

#[test]
fn victory_continuation_refuses_a_wrong_battle_or_a_choice_in_the_prelude() {
    for first in [message(0, 0), ask(0, 0, 1, 5, 1)] {
        let bytes = script(vec![("root", vec![(1, vec![first.clone(), vec![0x70, 0, 0x44, 1], message(0, 1)])])], texts(2));
        let field = FieldScript::parse(&bytes).unwrap();
        for formation in [323, 324] {
            let mut vm = vm_for(StoryState::new_game(), &[]);
            vm.begin(start(0, 1));
            assert_eq!(vm.seek_after_battle(&field, formation), formation == 324 && first[0] == 0x40);
        }
    }
}
