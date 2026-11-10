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

fn sound(id: u16, pan: u8) -> Vec<u8> {
    let [lo, hi] = id.to_le_bytes();
    vec![0xF1, 0, lo, hi, pan]
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
fn authored_ambient_main_and_descendants_run_without_cutting_off_finite_actor_tails() {
    let camera = cat(&[vec![0x64, 0], words(&[18, 2])]);
    let section = script(vec![
        ("director", vec![(1, vec![reqsw(2, 1)])]),
        ("ambient", vec![(0, vec![vec![RET], camera, reqsw(3, 1), idle()])]),
        ("actor", vec![(1, vec![xyzi(0, 0, 0, 0), wait(25), mov(12, 0), message(0, 0), setword(2, 0, 51)])]),
        ("ambient-child", vec![(1, vec![sound(42, 64), wait(2), vec![0x12, 8]])]),
    ], texts(1));
    let (beats, vm) = run_root(&section, vec![Step::StartAmbientMain(1), Step::Start(ScriptRef::new(0, 1))], &[]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(dialogs_of(&beats), [0], "an asynchronous foreground actor still owns completion");
    assert!(windows(&beats)[0].at >= 25);
    assert_eq!(vm.writes().get(&(2, 0)), Some(&51));
    let events = stage(&beats);
    assert!(events.iter().any(|(_, s)| *s == Stage::Camera(CameraOp::Scroll { x: 18, y: 2, frames: 0 })));
    assert!(events.iter().filter(|(_, s)| matches!(s, Stage::Sound { id: 42, .. })).count() > 5,
        "the requested ambient loop ran on the same scene clock");
    assert!(events.iter().any(|(_, s)| matches!(s, Stage::Move { entity: 2, x: 12, .. })));
}

#[test]
fn foreground_polling_and_slot_waits_are_not_mistaken_for_ambient_completion() {
    let poll = vec![if_ub(5, 1, 0, 1, &[0x10, 3]), vec![0x12, 8], message(0, 0)];
    let section = script(vec![
        ("poll", vec![(1, poll)]),
        ("ambient", vec![(0, vec![vec![RET], wait(20), setbyte(5, 1, 1), sound(42, 64), wait(2), vec![0x12, 8]])]),
    ], texts(1));
    let (beats, _) = run_root(&section, vec![Step::Start(ScriptRef::new(0, 1)), Step::StartAmbientMain(1)], &[]);
    assert_eq!(dialogs_of(&beats), [0], "the parked condition must see the future ambient write");
    assert_eq!(windows(&beats)[0].at, 20);
    assert_eq!(end_of(&beats), End::Finished);

    // A foreground request queued at the ambient main's occupied priority cannot fake its RET.
    let section = script(vec![("same-slot", vec![(0, vec![vec![RET], idle()]), (1, vec![message(0, 0)])])], texts(1));
    let (beats, _) = run_root(&section, vec![Step::StartAmbientMain(0), Step::Await(ScriptRef::new(0, 1))], &[]);
    assert_eq!(end_of(&beats), End::Budget);
    assert!(windows(&beats).is_empty());

    let section = script(vec![
        ("poll", vec![(1, vec![if_ub(5, 1, 0, 1, &[0x10, 3]), vec![0x12, 8], message(0, 0)])]),
        ("ambient", vec![(0, vec![vec![RET], vec![RET]])]),
    ], texts(1));
    let (beats, _) = run_root(&section, vec![Step::Start(ScriptRef::new(0, 1)), Step::StartAmbientMain(1)], &[]);
    assert_eq!(end_of(&beats), End::Budget, "an unsatisfied conditional poll is not completion even without runnable work");
    assert!(windows(&beats).is_empty());
}

#[test]
fn ambient_work_keeps_unknown_read_and_execution_budgets() {
    let section = script(vec![
        ("finite", vec![(1, vec![wait(20)])]),
        ("ambient", vec![(0, vec![vec![RET], if_ub(1, 44, 0, 0, &sound(42, 64)), idle()])]),
    ], vec![]);
    let vm = Vm::new(SceneVars::new(StoryState::at(48).into_extended().unwrap(), &[]), 7, Limits::default());
    let (beats, vm) = run_with(&section, vec![Step::StartAmbientMain(1), Step::Start(ScriptRef::new(0, 1))], &[], vm);
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.vars().unknown_persistent_reads().contains(&(1, 44)));

    let section = script(vec![
        ("finite", vec![(1, vec![wait(20)])]),
        ("ambient", vec![(0, vec![vec![RET], sound(42, 64), vec![0x12, 5]])]),
    ], vec![]);
    let vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, Limits { steps: 40, ..Limits::default() });
    let (beats, _) = run_with(&section, vec![Step::StartAmbientMain(1), Step::Start(ScriptRef::new(0, 1))], &[], vm);
    assert_eq!(end_of(&beats), End::Budget, "a fast ambient effect loop is still bounded");
}

#[test]
fn ambient_random_epoch_churn_cannot_keep_a_proven_foreground_tail_alive() {
    let tail = vec![vec![0xBB, 1, 0, 0, 1], wait(1), vec![0xA3, 1, 1], wait(1), vec![0x12, 14]];
    let section = script(vec![
        ("ambient", vec![(0, vec![vec![RET], vec![0x99, 5, 18], vec![0x12, 3]])]),
        ("finite", vec![(1, vec![wait(20), message(0, 0), reqsw(2, 1)])]),
        ("tail", vec![(1, tail)]),
        ("idle", vec![(1, vec![wait(40), idle()])]),
    ], texts(1));
    let root = vec![Step::StartAmbientMain(0), Step::Start(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(3, 1))];
    let (beats, vm) = run_root(&section, root, &[]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(dialogs_of(&beats), [0]);
    let elapsed: u32 = beats.iter().map(|beat| match beat.then { Then::Window(ref w) => w.at, Then::End { at, .. } => at }).sum();
    assert!(elapsed >= 40, "the foreground WAIT before its inert self-jump remains binding: {elapsed}");
    assert_eq!(vm.stats().quiescent_tails, 1);
}

#[test]
fn shake_reads_only_enabled_axes_and_never_blocks_the_script() {
    // Disabled X has unknown persistent operands. Only Y (literal amplitude5/steps2) is read.
    let section = script(vec![("camera", vec![(1, vec![
        vec![0x5E, 0x11, 0, 2, 44, 45, 5, 2],
        vec![0x5E, 0x11, 0x11, 0, 44, 45, 46, 47], message(0, 0),
    ])])], texts(1));
    let vm = Vm::new(SceneVars::new(StoryState::at(48).into_extended().unwrap(), &[]), 7, Limits::default());
    let (beats, vm) = run_with(&section, start(0, 1), &[], vm);
    assert_eq!(windows(&beats)[0].at, 0);
    assert_eq!(stage(&beats), vec![
        (0, Stage::Shake { mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2 }),
        (0, Stage::Shake { mask: 0, x_amplitude: 0, x_steps: 0, y_amplitude: 0, y_steps: 0 }),
    ]);
    assert!(vm.vars().unknown_persistent_reads().is_empty());
    assert_eq!(vm.stats().unresolved, 0);
    let section = script(vec![("camera", vec![(1, vec![vec![0x5E, 0, 0, 2, 0, 0, 5, 0]])])], vec![]);
    let (_, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(vm.stats().unresolved, 1, "enabled zero steps cannot be claimed as supported");
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

    let tail = vec![wait(101), vec![0xBB, 1, 0, 0, 1], wait(1), vec![0xA3, 1, 1], wait(1), vec![0x12, 14]];
    let section = script(vec![("tail", vec![(1, tail)])], vec![]);
    for limits in [Limits { frames: 100, ..Limits::default() }, Limits { steps: 2, ..Limits::default() }] {
        let vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, limits);
        let (beats, _) = run_with(&section, start(0, 1), &[], vm);
        assert_eq!(end_of(&beats), End::Budget, "proving a quiet tail cannot bypass frame or instruction limits");
    }
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
fn ordinary_move_selects_its_clip_from_source_speed_and_field_scale() {
    for (scale, speed, distance, animation, frames) in [
        (512u16, None, 100i16, 1, 50),
        (256, None, 100, 2, 50), // Same default MSPED/duration; a different field scale.
        (512, Some(1536u16), 99, 1, 33),
        (512, Some(1537), 99, 2, 33), // Exact threshold, despite equal rounded durations.
        (0, Some(0), 0, 1, 1),
        (0, Some(1), 0, 2, 1), // Use source speed, not the movement calculator's minimum of 128.
        (u16::MAX, Some(u16::MAX), 100, 1, 1), // Three times scale must not overflow a word.
        (512, Some(3840), 30000, 2, 900),
        (512, Some(128), 30000, 1, 900), // Duration clamping cannot select the clip.
        (512, Some(3840), 0, 2, 1),
    ] {
        let mut code = vec![xyzi(0, 0, 17, 2)];
        if let Some(speed) = speed {
            code.push(cat(&[vec![0xB2, 0], speed.to_le_bytes().to_vec()]));
        }
        code.extend([mov(distance, 0), message(0, 0), vec![RET]]);
        let section = SynthScript { scale, dialogs: texts(1), ..Default::default() }.entity("a", vec![(1, cat(&code))]).build();
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        let moves: Vec<_> = stage(&beats).into_iter().filter(|(_, event)| matches!(event, Stage::Move { .. })).collect();
        assert_eq!(moves, [(0, Stage::Move { entity: 0, from: Some((0, 0)), x: distance, y: 0, z: None, triangle: None,
            frames, how: MoveHow::Walk, animation: Some(animation) })], "scale={scale}, MSPED={speed:?}");
        assert_eq!(windows(&beats)[0].at, frames);
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn only_ordinary_move_selects_a_walk_or_run_slot() {
    let a = vec![
        xyzi(0, 0, 0, 0), cat(&[vec![0xB2, 0], 3840u16.to_le_bytes().to_vec()]), mov(10, 0),
        cat(&[vec![0xA9, 0], words(&[20, 0])]), // CMOVE
        cat(&[vec![0xAD, 0], words(&[30, 0])]), // FMOVE
        vec![0xAA, 1], // MOVA
        cat(&[vec![0xC0, 0, 0], words(&[50, 0, 0, 20])]), // JUMP
        cat(&[vec![0xC2, 0, 0], words(&[60, 0, 40, 0]), vec![0, 1, 0, 1]]), // LADER
        cat(&[vec![0xB2, 0], 1024u16.to_le_bytes().to_vec()]), mov(70, 0), message(0, 0), vec![RET],
    ];
    let section = SynthScript { scale: 512, dialogs: texts(1), ..Default::default() }
        .entity("a", vec![(1, cat(&a))])
        .entity("target", vec![(1, cat(&[xyzi(40, 0, 0, 0), vec![RET]]))]).build();
    let (beats, vm) = run_root(&section, vec![Step::Await(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))], &[]);
    let moves: Vec<_> = stage(&beats).into_iter().filter_map(|(at, event)| match event {
        Stage::Move { from, x, y, frames, how, animation, .. } => Some((at, from, (x, y), frames, how, animation)), _ => None,
    }).collect();
    assert_eq!(moves, [
        (0, Some((0, 0)), (10, 0), 2, MoveHow::Walk, Some(2)),
        (2, Some((10, 0)), (20, 0), 2, MoveHow::Walk, None),
        (4, Some((20, 0)), (30, 0), 2, MoveHow::WalkKeepAnimation, None),
        (6, Some((30, 0)), (40, 0), 2, MoveHow::Walk, None),
        (8, Some((40, 0)), (50, 0), 22, MoveHow::Jump, None),
        (30, Some((50, 0)), (60, 0), 15, MoveHow::Ladder, None),
        (45, Some((60, 0)), (70, 0), 5, MoveHow::Walk, Some(1)),
    ]);
    assert_eq!(windows(&beats)[0].at, 50, "JUMP uses its twenty steps and two completion frames");
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.writes().is_empty());
}

#[test]
fn jump_reads_signed_steps_and_blocks_for_setup_motion_and_completion() {
    for banked in [false, true] {
        for steps in [1i16, 12, 0, -1] {
            let mut code = vec![xyzi(9, -105, 0, 0), wait(15)];
            if banked { code.push(setword(5, 22, steps as u16)); }
            code.push(cat(&[vec![0xC0, 0, if banked { 5 } else { 0 }], words(&[9, -247, 0, if banked { 22 } else { steps }])]));
            code.extend([visi(0), vec![RET]]);
            let section = SynthScript::default().entity("jump", vec![(1, cat(&code))]).build();
            let (beats, vm) = run_root(&section, start(0, 1), &[]);
            let events = stage(&beats);
            if steps > 0 {
                let frames = steps as u32 + 2;
                assert!(events.iter().any(|(at, what)| *at == 15 && matches!(what,
                    Stage::Move { frames: actual, how: MoveHow::Jump, .. } if *actual == frames)), "{events:?}");
                assert!(events.iter().any(|(at, what)| *at == 15 + frames && matches!(what,
                    Stage::Show { visible: false, .. })), "{events:?}");
                assert_eq!(end_of(&beats), End::Finished);
                assert_eq!(vm.stats().unresolved, 0);
            } else {
                assert!(!events.iter().any(|(_, what)| matches!(what, Stage::Move { .. } | Stage::Show { .. })));
                assert_eq!(end_of(&beats), End::Budget);
                assert_eq!(vm.stats().unresolved, 1);
            }
        }
    }
}

#[test]
fn ladder_sets_authored_facing_before_movement_without_adding_time() {
    for direction in [0, 64, 128, 192, 255] {
        let climb = vec![
            xyzi(10, 20, 10, 0), dir(128), wait(3),
            cat(&[vec![0xC2, 0, 0], words(&[40, 50, 90, 2]), vec![0, 1, direction, 1]]),
        ];
        let mut visible = climb.clone();
        visible.push(message(0, 0));
        let section = script(vec![("actor", vec![(1, visible), (2, vec![message(0, 0)]), (3, climb)])], texts(1));
        let heading = crate::field_direction_to_facing(direction);
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(stage(&beats), [
            (0, Stage::Place { entity: 0, x: 10, y: 20, z: Some(10), triangle: Some(0) }),
            (0, Stage::Face { entity: 0, heading: crate::field_direction_to_facing(128), frames: 0 }),
            (3, Stage::Face { entity: 0, heading, frames: 0 }),
            (3, Stage::Move { entity: 0, from: Some((10, 20)), x: 40, y: 50, z: Some(90), triangle: Some(2), frames: 20, how: MoveHow::Ladder, animation: None }),
        ], "direction={direction}");
        assert_eq!(windows(&beats)[0].at, 23, "facing does not extend the existing climb duration");
        assert_eq!(vm.heading_of(0), Some(heading));
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());

        let field = field_of(section);
        let mut def = demo_def(vec![member(0, "actor")], start(0, 2));
        def.setup = vec![ScriptRef::new(0, 3)];
        let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
        assert_eq!(play.initial_stage_events(), [
            Stage::Place { entity: 0, x: 40, y: 50, z: Some(90), triangle: Some(2) },
            Stage::Face { entity: 0, heading, frames: 0 },
        ], "quiet setup retains the ladder's facing: {direction}");
        let beat = play.next(None);
        assert!(beat.stage.is_empty(), "setup does not replay the climb or its facing");
        assert!(matches!(beat.then, Then::Window(Window { at: 0, .. })));
    }
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
fn turngen_uses_authored_steps_and_waits_for_the_completion_tick() {
    for steps in [1u8, 10, 255] {
        for path in [0u8, 1, 2] {
            for interpolation in [1u8, 2] {
                let section = script(vec![("actor", vec![(1, vec![vec![0xB4, 0, 28, path, steps, interpolation], message(0, 0)])])], texts(1));
                let (beats, vm) = run_root(&section, start(0, 1), &[]);
                assert_eq!(stage(&beats), [(0, Stage::Face { entity: 0, heading: crate::field_direction_to_facing(28), frames: u32::from(steps) })]);
                assert_eq!(windows(&beats)[0].at, u32::from(steps) + 1, "steps={steps}, path={path}, interpolation={interpolation}: the script observes completion on the next tick");
                assert_eq!(end_of(&beats), End::Finished);
                assert!(vm.writes().is_empty());
            }
        }
    }
}

#[test]
fn turngen_completion_precedes_the_explicit_wait_and_cropped_hold() {
    let section = script(vec![("actor", vec![(1, vec![
        vec![0xB4, 0, 28, 2, 10, 2], wait(1), vec![0xBC, 3, 0, 9, 1], message(0, 0),
    ])])], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(stage(&beats), [
        (0, Stage::Face { entity: 0, heading: crate::field_direction_to_facing(28), frames: 10 }),
        (12, Stage::Anim { entity: 0, animation: 3, how: AnimHow::Hold, first: Some(0), last: Some(9), speed: 16, divisor: 1 }),
    ]);
    assert_eq!(windows(&beats)[0].at, 22, "eleven turn ticks, WAIT1, then the existing ten-frame BC wait");
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.writes().is_empty());
}

#[test]
fn reqew_waits_for_the_turngen_childs_completion_tick() {
    let section = script(vec![
        ("director", vec![(1, vec![reqew(1, 1), wait(2), message(0, 0)])]),
        ("actor", vec![(1, vec![vec![0xB4, 0, 64, 2, 10, 2]])]),
    ], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(stage(&beats), [(0, Stage::Face { entity: 1, heading: 0.0, frames: 10 })]);
    assert_eq!(windows(&beats)[0].at, 13, "REQEW joins the child's eleven-tick turn before the director's WAIT2");
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.writes().is_empty());
}

#[test]
fn tura_uses_authored_steps_before_observing_completion() {
    for steps in [1u8, 10, 255] {
        for path in [0u8, 1, 2] {
            let section = script(vec![
                ("actor", vec![(1, vec![xyzi(0, 0, 0, 0), vec![0xAB, 1, steps, path], message(0, 0)])]),
                ("target", vec![(1, vec![xyzi(100, 100, 0, 0)])]),
            ], texts(1));
            let (beats, vm) = run_root(&section, vec![Step::Await(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))], &[]);
            let faces: Vec<_> = stage(&beats).into_iter().filter(|(_, event)| matches!(event, Stage::Face { .. })).collect();
            assert_eq!(faces, [(0, Stage::Face { entity: 0, heading: std::f32::consts::FRAC_PI_4, frames: u32::from(steps) })]);
            assert_eq!(windows(&beats)[0].at, u32::from(steps) + 1, "steps={steps}, path={path}");
            assert_eq!(vm.stats().unresolved, 0);
            assert_eq!(end_of(&beats), End::Finished);
            assert!(vm.writes().is_empty());
        }
    }
}

#[test]
fn reqew_joins_tura_before_the_following_wait() {
    let section = script(vec![
        ("director", vec![(1, vec![xyzi(100, 0, 0, 0), reqew(1, 1), wait(4), message(0, 0)])]),
        ("actor", vec![(1, vec![xyzi(0, 0, 0, 0), vec![0xAB, 0, 10, 2]])]),
    ], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    let faces: Vec<_> = stage(&beats).into_iter().filter(|(_, event)| matches!(event, Stage::Face { .. })).collect();
    assert_eq!(faces, [(0, Stage::Face { entity: 1, heading: 0.0, frames: 10 })]);
    assert_eq!(windows(&beats)[0].at, 15, "eleven ticks for TURA completion, followed by WAIT4");
    assert_eq!(end_of(&beats), End::Finished);
    assert!(vm.writes().is_empty());
}

#[test]
fn tura_fallbacks_missing_targets_and_immediate_dira_keep_their_timing() {
    let cases = [
        (vec![0xAB, 1, 0, 2], Some((100, 0)), Some(8), 0),
        (vec![0xAB, 1, 10, 3], Some((100, 0)), Some(8), 0),
        (vec![0xAB, 1, 10, 255], Some((100, 0)), Some(8), 0),
        (vec![0xAB, 1, 10, 2], None, None, 1),
        (vec![0xAB, 1, 10, 2], Some((0, 0)), None, 0),
        (dira(1), Some((100, 0)), Some(0), 0),
    ];
    for (instruction, target, duration, unresolved) in cases {
        let target_code = target.map(|(x, y)| vec![xyzi(x, y, 0, 0)]).unwrap_or_default();
        let section = script(vec![
            ("actor", vec![(1, vec![xyzi(0, 0, 0, 0), instruction.clone(), message(0, 0)])]),
            ("target", vec![(1, target_code)]),
        ], texts(1));
        let (beats, vm) = run_root(&section, vec![Step::Await(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))], &[]);
        let faces: Vec<_> = stage(&beats).into_iter().filter(|(_, event)| matches!(event, Stage::Face { .. })).collect();
        let expected: Vec<_> = duration.into_iter().map(|frames| (0, Stage::Face { entity: 0, heading: 0.0, frames })).collect();
        assert_eq!(faces, expected, "{instruction:?}, target={target:?}");
        assert_eq!(windows(&beats)[0].at, duration.unwrap_or(0), "no extra completion tick for fallback/skipped turns");
        assert_eq!(vm.stats().unresolved, unresolved);
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn unsupported_turngen_operands_and_other_turns_keep_the_existing_timing() {
    for instruction in [
        vec![0xB4, 0, 64, 2, 0, 2], // Zero steps.
        vec![0xB4, 0, 64, 2, 10, 0], // Unsupported interpolation.
        vec![0xB4, 0, 64, 2, 10, 3],
        vec![0xB4, 0, 64, 3, 10, 2], // Unsupported path.
        vec![0xB5, 0, 64, 2, 10, 2], // TURN remains outside this timing slice.
        vec![0xAB, 1, 2, 10], // TURA's path 10 remains unsupported (steps are byte 2).
    ] {
        let section = script(vec![
            ("actor", vec![(1, vec![xyzi(0, 0, 0, 0), instruction.clone(), message(0, 0)])]),
            ("target", vec![(1, vec![xyzi(100, 0, 0, 0)])]),
        ], texts(1));
        let root = vec![Step::Await(ScriptRef::new(1, 1)), Step::Start(ScriptRef::new(0, 1))];
        let (beats, vm) = run_root(&section, root, &[]);
        let faces: Vec<_> = stage(&beats).into_iter().filter(|(_, event)| matches!(event, Stage::Face { .. })).collect();
        assert_eq!(faces, [(0, Stage::Face { entity: 0, heading: 0.0, frames: 8 })], "{instruction:?}");
        assert_eq!(windows(&beats)[0].at, 8, "the existing fallback has no added completion tick: {instruction:?}");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn full_holds_dispatch_async_af_and_sync_ba_before_explicit_waits() {
    // Isolated clips exercise dispatch without assuming how overlapping animations are queued.
    // Full clips retain the VM's existing fifteen-frame estimate.
    for (opcode, expected_at) in [(0xAF, 6), (0xBA, 21)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 4, 1], wait(6), message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(stage(&beats), [(0, Stage::Anim { entity: 0, animation: 4, how: AnimHow::Hold,
            first: None, last: None, speed: 16, divisor: 1 })]);
        assert_eq!(windows(&beats)[0].at, expected_at, "only BA adds the existing clip wait before WAIT6");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn animw_joins_the_existing_full_hold_deadline() {
    for (opcode, expected_at) in [(0xAF, 15), (0xBA, 21)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 4, 1], wait(6), vec![0xAC], message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(windows(&beats)[0].at, expected_at, "ANIMW joins the existing deadline without waiting for the full clip twice");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn cropped_holds_dispatch_async_b1_and_sync_bc_before_explicit_waits() {
    for (opcode, last, expected_at) in [(0xB1, 39, 15), (0xBC, 39, 55), (0xB1, 0, 15), (0xBC, 0, 16)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 5, 0, last, 1], wait(15), message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(stage(&beats), [(0, Stage::Anim { entity: 0, animation: 5, how: AnimHow::Hold,
            first: Some(0), last: Some(last), speed: 16, divisor: 1 })]);
        assert_eq!(windows(&beats)[0].at, expected_at, "opcode {opcode:#x}, last frame {last}: only BC adds the existing clip wait");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn animw_joins_the_existing_cropped_hold_deadline() {
    for (opcode, expected_at) in [(0xB1, 40), (0xBC, 55)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 5, 0, 39, 1], wait(15), vec![0xAC], message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(windows(&beats)[0].at, expected_at, "ANIMW joins the original deadline; it does not start another animation wait");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn cropped_once_dispatches_async_b0_and_sync_bb_before_explicit_waits() {
    for (opcode, expected_at) in [(0xB0, 15), (0xBB, 55)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 3, 0, 39, 1], wait(15), message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(stage(&beats), [(0, Stage::Anim { entity: 0, animation: 3, how: AnimHow::Once,
            first: Some(0), last: Some(39), speed: 16, divisor: 1 })]);
        assert_eq!(windows(&beats)[0].at, expected_at, "only BB adds the existing clip wait before WAIT15");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn animw_joins_the_existing_cropped_once_deadline() {
    for (opcode, expected_at) in [(0xB0, 40), (0xBB, 55)] {
        let section = script(vec![("a", vec![(1, vec![vec![opcode, 3, 0, 39, 1], wait(15), vec![0xAC], message(0, 0)])])], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(windows(&beats)[0].at, expected_at, "ANIMW joins the original deadline without waiting for the clip twice");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn reqew_observes_the_cropped_once_childs_dispatch() {
    for (opcode, expected_at) in [(0xB0, 6), (0xBB, 16)] {
        let section = script(vec![
            ("director", vec![(1, vec![reqew(1, 1), wait(6), message(0, 0)])]),
            ("actor", vec![(1, vec![vec![opcode, 3, 10, 19, 1]])]),
        ], texts(1));
        let (beats, vm) = run_root(&section, start(0, 1), &[]);
        assert_eq!(stage(&beats), [(0, Stage::Anim { entity: 1, animation: 3, how: AnimHow::Once,
            first: Some(10), last: Some(19), speed: 16, divisor: 1 })]);
        assert_eq!(windows(&beats)[0].at, expected_at, "REQEW waits for the child script; BB waits ten frames before returning");
        assert_eq!(end_of(&beats), End::Finished);
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn animation_literal_speed_is_preserved_separately_with_an_explicit_animw() {
    let a = vec![vec![0xA2, 3, 2], vec![0xBD, 0, 32, 0], vec![0xAF, 4, 3], vec![0xAC], vec![0xA2, 5, 0], message(0, 0)];
    let section = script(vec![("a", vec![(1, a)])], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    let anims: Vec<_> = stage(&beats).into_iter().filter_map(|(at, event)| match event {
        Stage::Anim { animation, speed, divisor, .. } => Some((at, animation, speed, divisor)), _ => None,
    }).collect();
    assert_eq!(anims, [(0, 3, 16, 2), (0, 4, 32, 3), (8, 5, 32, 0)]);
    assert_eq!(windows(&beats)[0].at, 8, "ANIMW explicitly joins AF's existing ASPED-based deadline; literal metadata does not change it");
    assert!(matches!(vm.default_animation_of(0), Some(Stage::Anim { animation: 5, speed: 32, divisor: 0, .. })));
    assert!(vm.writes().is_empty());
}

#[test]
fn asped_invalidates_setup_and_runtime_loops_until_a_fresh_animation() {
    for speed in [0u16, 16, 300, u16::MAX] {
        let asped = cat(&[vec![0xBD, 0], speed.to_le_bytes().to_vec()]);
        let section = script(vec![("a", vec![
            (3, vec![vec![0xA2, 3, 1], wait(7), asped.clone()]),
            (1, vec![vec![0xA2, 4, 1], wait(11), asped, message(0, 0), vec![0xA2, 5, 1]]),
        ])], texts(1));
        let parsed = FieldScript::parse(&section).unwrap();
        let mut vm = Vm::new(SceneVars::new(StoryState::at(4), &[]), 1, Limits::default());
        vm.setup(&parsed, &[ScriptRef::new(0, 3)]);
        assert_eq!(vm.default_animation_of(0), None, "setup ASPED clears the old default even at normal speed");
        vm.begin(start(0, 1));
        let beat = vm.resume(&parsed, None);
        assert!(matches!(beat.then, Then::Window(Window { at: 11, .. })));
        assert!(matches!(beat.stage.first(), Some(Timed { at: 0, what: Stage::Anim { animation: 4, .. } })));
        assert_eq!(beat.stage.last(), Some(&Timed { at: 11, what: Stage::AnimSpeed { entity: 0, speed } }), "metadata retains the full word, including zero");
        assert_eq!(vm.default_animation_of(0), None, "runtime ASPED also clears the active default");
        assert!(matches!(vm.resume(&parsed, None).then, Then::End { at: 0, how: End::Finished }));
        assert!(matches!(vm.default_animation_of(0), Some(Stage::Anim { animation: 5, how: AnimHow::Loop, .. })), "a fresh DFANM can establish a new default");
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn constant_asped_loop_parks_without_holding_the_scene_open() {
    let background = vec![vec![0xBD, 0, 16, 0], vec![0x12, 4]];
    let speaker = vec![wait(5), message(0, 0)];
    let section = script(vec![("background", vec![(1, background)]), ("speaker", vec![(1, speaker)])], texts(1));
    let root = vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))];
    let vm = Vm::new(SceneVars::new(StoryState::at(4), &[]), 1, Limits { steps: 128, ..Limits::default() });
    let (beats, vm) = run_with(&section, root, &[], vm);
    assert_eq!(dialogs_of(&beats), [0]);
    assert_eq!(windows(&beats)[0].at, 5);
    assert_eq!(end_of(&beats), End::Finished, "constant ASPED metadata must not exhaust the instruction budget");
    assert!(vm.stats().parked >= 1, "the unchanged background loop still parks");
    assert!(stage(&beats).iter().any(|(_, event)| *event == Stage::AnimSpeed { entity: 0, speed: 16 }));
    assert!(vm.writes().is_empty());
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

#[test]
fn literal_sounds_and_stops_follow_waits_and_window_boundaries() {
    let section = script(vec![("a", vec![(1, vec![
        sound(61, 0), wait(12), sound(59, 127), message(0, 0), wait(7), sound(0, 64), message(0, 1),
    ])])], texts(2));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(stage(&beats), [
        (0, Stage::Sound { channel: 1, id: 61, pan: 0 }),
        (12, Stage::Sound { channel: 1, id: 59, pan: 127 }),
        (7, Stage::Sound { channel: 1, id: 0, pan: 64 }),
    ]);
    assert_eq!(windows(&beats).iter().map(|w| w.at).collect::<Vec<_>>(), [12, 7]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!(vm.stats().handled.get("SOUND"), Some(&3));
    assert!(!vm.stats().ignored.contains_key("SOUND"));
    assert_eq!(vm.stats().unresolved, 0);
    assert!(vm.writes().is_empty());
}

#[test]
fn literal_akao_channels_one_and_two_keep_their_channels_stops_and_window_clock() {
    let akao = |op: u8, pan: u8, id: u16| {
        let [lo, hi] = id.to_le_bytes();
        vec![0xF2, 0, 0, 0, op, pan, lo, hi, 0, 0, 0, 0, 0, 0]
    };
    let akao2 = |op: u8, pan: u16, id: u16| {
        let mut bytes = vec![0xDA, 0, 0, 0, op];
        for word in [pan, id, 0, 0, 0] { bytes.extend(word.to_le_bytes()); }
        bytes
    };
    let mut banked = akao(40, 7, 59);
    banked[1] = 0x10;
    let mut banked2 = akao2(41, 64, 7);
    banked2[1] = 0x01;
    let section = script(vec![("a", vec![(1, vec![
        akao(40, 64, 59), wait(12), akao(41, 0, 233), message(0, 0),
        wait(7), akao2(41, 127, 0), akao2(40, 64, 167),
        akao(42, 64, 154), akao(43, 64, 59), akao(162, 64, 0), banked, banked2,
        message(0, 1),
    ])])], texts(2));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(stage(&beats), [
        (0, Stage::Sound { channel: 1, id: 59, pan: 64 }),
        (12, Stage::Sound { channel: 2, id: 233, pan: 0 }),
        (7, Stage::Sound { channel: 2, id: 0, pan: 127 }),
        (7, Stage::Sound { channel: 1, id: 167, pan: 64 }),
    ]);
    assert_eq!(windows(&beats).iter().map(|w| w.at).collect::<Vec<_>>(), [12, 7]);
    assert_eq!(end_of(&beats), End::Finished);
    assert_eq!((vm.stats().handled.get("AKAO"), vm.stats().handled.get("AKAO2")), (Some(&2), Some(&2)));
    assert_eq!((vm.stats().ignored.get("AKAO"), vm.stats().ignored.get("AKAO2")), (Some(&4), Some(&1)),
        "higher channels, volume operations and banked operands remain explicitly skipped");
    assert_eq!(vm.stats().unresolved, 0);
    assert!(vm.writes().is_empty());
}

#[test]
fn banked_sound_operands_are_reported_and_never_guessed() {
    let section = script(vec![("a", vec![(1, vec![
        vec![0xF1, 0x10, 7, 0, 64], // id is Var[1][7]
        vec![0xF1, 0x01, 61, 0, 7], // pan is Var[1][7]
        sound(59, 64), message(0, 0),
    ])])], texts(1));
    let (beats, vm) = run_root(&section, start(0, 1), &[]);
    assert_eq!(stage(&beats), [(0, Stage::Sound { channel: 1, id: 59, pan: 64 })]);
    assert_eq!(dialogs_of(&beats), [0]);
    assert_eq!(vm.stats().ignored.get("SOUND"), Some(&2));
    assert_eq!(vm.stats().handled.get("SOUND"), Some(&1));
    assert_eq!(vm.stats().unresolved, 0, "optional unsupported audio does not make the story incomplete");
    assert!(vm.stats().unknown.is_empty());
    let outcome = run_to_end(&field_of(section), &demo_def(vec![], start(0, 1)), &StoryState::new_game(), 1, &[]).unwrap();
    assert!(outcome.is_complete(), "banked SOUND remains a skipped presentation command");
}

#[test]
fn setup_sounds_do_not_replay_and_sounds_need_no_cast_member() {
    let field = field_of(script(vec![("a", vec![
        (0, vec![sound(236, 64)]),
        (1, vec![sound(61, 64), message(0, 0)]),
    ])], texts(1)));
    let mut def = demo_def(vec![], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 0)];
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    assert!(play.initial_stage_events().is_empty());
    let beat = play.next(None);
    assert_eq!(stage(&[beat]), [(0, Stage::Sound { channel: 1, id: 61, pan: 64 })]);
    assert_eq!(play.off_cast(), 0);
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
fn byte_bit_conditions_audit_only_the_selected_story_bit() {
    fn if_ubl(bank: u8, addr: u8, op: u8, right: u8, body: &[u8]) -> Vec<u8> {
        cat(&[vec![0x15, bank << 4, addr, right, op], u16::try_from(body.len() + 2).unwrap().to_le_bytes().to_vec(), body.to_vec()])
    }
    for long in [false, true] {
        for bit in [0u8, 5, 7, 3, 4, 6] {
            let known = [0, 5, 7].contains(&bit);
            for set in [false, true] {
                // Decode an existing-value fixture to cover bit7 set as well: this
                // does not ask the story writer to set the never-set timeout bit.
                let story = if known && set {
                    StoryState::decode(&[2, 33, 0, 1, 1, 225, 1u8 << bit]).unwrap()
                } else {
                    StoryState::at(33)
                };
                let given = if known { vec![] } else { vec![Given { bank: 1, address: 225, value: if set { 1u8 << bit } else { 0 } }] };
                for op in [9, 10] {
                    let branch = if long { if_ubl(1, 225, op, bit, &message(0, 0)) } else { if_ub(1, 225, op, bit, &message(0, 0)) };
                    let section = script(vec![("a", vec![(1, vec![branch, message(0, 1)])])], texts(2));
                    let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(story.clone(), &given));
                    let taken = if op == 9 { set } else { !set };
                    assert_eq!(dialogs_of(&beats), if taken { vec![0, 1] } else { vec![1] }, "long={long} bit={bit} set={set} op={op}");
                    let audited = vm.vars().unknown_persistent_reads().into_iter().collect::<Vec<_>>();
                    assert_eq!(audited, if known { vec![] } else { vec![(1, 225)] }, "long={long} bit={bit} set={set} op={op}");
                    assert!(vm.writes().is_empty());
                    assert_eq!(vm.vars().story.encode(), story.encode(), "a bit read changes no saved bytes");
                }
            }
        }
    }
}

#[test]
fn byte_bit_read_shortcut_preserves_other_audits_and_local_writes() {
    // Equality still needs the whole partial byte. Invalid bit indices retain
    // the old full-read audit and the existing bit-on=false / bit-off=true rule.
    for (op, right, taken) in [(0, 0, true), (9, 8, false), (10, 8, true), (9, 255, false), (10, 255, true)] {
        let section = script(vec![("a", vec![(1, vec![if_ub(1, 225, op, right, &message(0, 0)), message(0, 1)])])], texts(2));
        let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(StoryState::at(33), &[]));
        assert_eq!(dialogs_of(&beats), if taken { vec![0, 1] } else { vec![1] }, "op={op} right={right}");
        assert_eq!(vm.vars().unknown_persistent_reads().into_iter().collect::<Vec<_>>(), [(1, 225)], "op={op} right={right}");
        assert!(vm.writes().is_empty());
    }

    let code = vec![biton(1, 225, 0), if_ub(1, 225, 9, 0, &message(0, 0)), if_ub(1, 225, 10, 0, &message(0, 1)), message(0, 2)];
    let section = script(vec![("a", vec![(1, code)])], texts(3));
    let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(StoryState::at(33), &[]));
    assert_eq!(dialogs_of(&beats), [0, 2], "bit conditions read the VM write, not the original clear story bit");
    assert_eq!(vm.writes().get(&(1, 225)), Some(&1));
    assert_eq!(vm.vars().unknown_persistent_reads().into_iter().collect::<Vec<_>>(), [(1, 225)], "BITON still conservatively audits its read-modify-write");
    assert_eq!(vm.vars().story.encode(), [1, 33, 0], "VM writes have not changed the saved story");

    for op in [9, 10] {
        let mut branch = if_ub(1, 225, op, 5, &message(0, 0));
        branch[1] = 0x13; // Left Var[1][225], right bit index Var[3][5].
        let section = script(vec![("a", vec![(1, vec![branch, message(0, 1)])])], texts(2));
        let given = [Given { bank: 3, address: 5, value: 7 }];
        let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(StoryState::at(33), &given));
        assert_eq!(dialogs_of(&beats), if op == 10 { vec![0, 1] } else { vec![1] }, "the selected timeout bit is clear");
        assert!(vm.vars().unknown_persistent_reads().contains(&(3, 5)), "Given does not make the persistent bit index authoritative");
        assert!(vm.writes().is_empty());
    }
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

#[test]
fn extended_assignments_preserve_known_zeroes_aliases_clears_and_numeric_order() {
    let story = StoryState::at(36).into_extended().unwrap();
    let code = vec![
        setbyte(1, 4, 30), // authored affection initialization
        vec![0x85, 0x10, 4, 5], // PLUS 5
        vec![0x87, 0x10, 4, 2], // MINUS 2
        setbyte(1, 6, 0), // an explicit zero is known, not an absent guess
        setword(4, 20, 0x1234), // same bytes as bank 3
        biton(3, 66, 0),
        vec![0x83, 0x30, 66, 0], // BITOFF, preserves the explicit clear
        setword(2, 0, 39),
    ];
    let field = field_of(script(vec![("a", vec![(1, code)])], vec![]));
    let def = demo_def(vec![member(0, "aya")], start(0, 1));
    let result = run_to_end(&field, &def, &story, 1, &[]).unwrap();
    assert!(result.unknown_persistent_reads.is_empty(), "{:?}", result.unknown_persistent_reads);
    assert_eq!(result.writes.persistent.get(&(1, 6)), Some(&(0, 255)));
    assert_eq!(result.writes.persistent.get(&(3, 66)), Some(&(0, 1)));
    let mut after = story.clone();
    assert_eq!(result.writes.apply_extended_to(&mut after, &story), Some(true));
    assert_eq!(after.known_byte(1, 4), Some(33));
    assert_eq!(after.known_byte(1, 6), Some(0));
    assert_eq!(after.known_word(4, 20), Some(0x1234));
    assert_eq!(after.known_byte(3, 20), Some(0x34));
    assert_eq!(after.known_bit(3, 66, 0), Some(false));
    assert_eq!(after.known_bit(3, 66, 1), None);
    assert_eq!(after.moment, 39);
    let saved = after.clone();
    assert_eq!(result.writes.apply_extended_to(&mut after, &story), None, "stale replay cannot overwrite later state");
    assert_eq!(after, saved);
    assert_eq!(StoryState::decode(&after.encode()), Some(after));
}

#[test]
fn extended_bit_writes_do_not_make_unknown_neighbours_authoritative() {
    for bank in [3, 4, 7, 15, 11, 12, 13, 14] {
        let story = StoryState::at(36).into_extended().unwrap();
        let selected = vec![biton(bank, 66, 0), if_ub(bank, 66, 9, 0, &message(0, 0))];
        let section = script(vec![("a", vec![(1, selected)])], texts(1));
        let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(story.clone(), &[]));
        assert_eq!(dialogs_of(&beats), [0]);
        assert!(vm.vars().unknown_persistent_reads().is_empty(), "bank {bank}");
        for read in [if_ub(bank, 66, 9, 1, &message(0, 0)), if_ub(bank, 66, 0, 1, &message(0, 0))] {
            let section = script(vec![("a", vec![(1, vec![biton(bank, 66, 0), read])])], texts(1));
            let (_, vm) = run_with(&section, start(0, 1), &[], vm_for(story.clone(), &[]));
            assert!(vm.vars().unknown_persistent_reads().contains(&(crate::story::canonical_persistent_bank(bank).unwrap(), 66)));
        }
    }
}

#[test]
fn extended_given_values_cannot_override_saved_bits_or_hide_unknown_numeric_inputs() {
    use crate::script::dialogue::StoryVars;
    let mut story = StoryState::at(36).into_extended().unwrap();
    story.write_masked(3, 66, 0, 1).unwrap();
    let vars = SceneVars::new(story.clone(), &[Given { bank: 4, address: 66, value: 255 }]);
    assert_eq!(vars.byte(3, 66), 254);
    assert!(vars.unknown_persistent_reads().contains(&(3, 66)));
    for code in [vec![0x85, 0x10, 4, 5], vec![0x84, 0x30, 66, 1], vec![0x80, 0x53, 7, 66]] {
        let field = field_of(script(vec![("a", vec![(1, vec![code])])], vec![]));
        let def = demo_def(vec![member(0, "aya")], start(0, 1));
        let result = run_to_end(&field, &def, &story, 1, &[]).unwrap();
        assert!(!result.unknown_persistent_reads.is_empty(), "arithmetic, XOR and copies must retain unknown-input audits");
    }
}

#[test]
fn extended_word_access_cannot_read_or_write_half_a_word_at_the_bank_end() {
    let mut story = StoryState::at(36).into_extended().unwrap();
    story.write_masked(3, 255, 7, 255).unwrap();
    for (code, writing) in [(setword(4, 255, 0x1234), true), (vec![0x81, 0x54, 10, 255, 0], false)] {
        let field = field_of(script(vec![("a", vec![(1, vec![code])])], vec![]));
        let def = demo_def(vec![member(0, "aya")], start(0, 1));
        let result = run_to_end(&field, &def, &story, 1, &[]).unwrap();
        assert!(result.writes.persistent.is_empty(), "never commit only a low byte");
        if writing { assert!(result.stats.unresolved > 0); }
        else { assert!(result.unknown_persistent_reads.contains(&(3, 255))); }
    }
}

#[test]
fn extended_temporary_byte_and_word_selectors_share_storage_without_persisting_it() {
    let story = StoryState::at(36).into_extended().unwrap();
    let code = vec![setword(6, 20, 0x1234), if_ub(5, 20, 0, 0x34, &message(0, 0)), if_ub(5, 21, 0, 0x12, &message(0, 1))];
    let section = script(vec![("a", vec![(1, code.clone())])], texts(2));
    let (beats, vm) = run_with(&section, start(0, 1), &[], vm_for(story.clone(), &[]));
    assert_eq!(dialogs_of(&beats), [0, 1]);
    assert!(vm.vars().unknown_persistent_reads().is_empty());
    let field = field_of(script(vec![("a", vec![(1, code)])], texts(2)));
    let result = run_to_end(&field, &demo_def(vec![member(0, "aya")], start(0, 1)), &story, 1, &[]).unwrap();
    assert!(result.writes.is_story_neutral());
    assert!(result.writes.persistent.is_empty());
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
    // Sound presentation differs without changing the story or the provenance comparison.
    assert!(original.same_story_as(&run([sound(61, 64), message(0, 0), setword(2, 0, 9), vec![0x3F]].concat(), lines())));
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
    for bank in [7, 11, 13] {
        let changed = run([message(0, 0), setword(2, 0, 9), vec![0x3F], setbyte(bank, 40, 1)].concat(), lines());
        assert!(!original.same_story_as(&changed), "persistent bank {bank} matters even when the legacy save cannot carry it");
    }
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
fn walks_keep_the_actors_floor_across_windows_and_explicit_moves_reset_it() {
    for (opcode, how) in [(0xA8, MoveHow::Walk), (0xAD, MoveHow::WalkKeepAnimation)] {
        let walk = |x, y| cat(&[vec![opcode, 0], words(&[x, y])]);
        let jump = cat(&[vec![0xC0, 0, 0], words(&[40, 20, 0, 20])]);
        let section = script(vec![("a", vec![
            (3, vec![xyzi(10, 10, 100, 1)]),
            (1, vec![walk(20, 10), message(0, 0), walk(30, 10), xyzi(10, 10, 0, 0),
                     walk(20, 20), xyzi(10, 10, 100, 1), jump, walk(30, 20)]),
        ])], vec![dialog("Aya")]);
        let mut spec = FieldSpec::with_mesh(vec![
            [(0, 0, 0), (100, 0, 0), (0, 100, 0)],
            [(0, 0, 100), (100, 0, 100), (0, 100, 100)],
        ], vec![[u16::MAX; 3]; 2]);
        spec.overrides.push((0, section));
        let field = FieldFile::parse(spec.build()).unwrap();
        let mut def = demo_def(vec![member(0, "aya")], start(0, 1));
        def.setup = vec![ScriptRef::new(0, 3)];
        let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
        let moves = |beat: &Beat| beat.stage.iter().filter_map(|event| match event.what {
            Stage::Move { z, how, .. } => Some((z, how)), _ => None,
        }).collect::<Vec<_>>();
        let first = play.next(None);
        assert!(matches!(first.then, Then::Window(_)));
        assert_eq!(moves(&first), [(Some(100), how)], "setup height selects the upper floor: {how:?}");
        let second = play.next(None);
        assert!(matches!(second.then, Then::End { how: End::Finished, .. }));
        assert_eq!(moves(&second), [
            (Some(100), how), // The previous window did not lose the floor.
            (Some(0), how),   // Explicit placement resets the floor.
            (Some(0), MoveHow::Jump),   // The authored lower-floor jump resets the walking height.
            (Some(0), how),   // The resolved landing resets subsequent walking.
        ], "{how:?}");
    }
}

#[test]
fn jumps_land_on_the_authored_floor_and_following_walks_keep_it() {
    let jump = |x, y, triangle| cat(&[vec![0xC0, 0, 0], words(&[x, y, triangle, 20])]);
    let section = script(vec![("a", vec![
        (3, vec![xyzi(10, 10, 0, 0)]),
        (1, vec![jump(20, 10, 1), message(0, 0), mov(30, 10), jump(40, 20, 0),
                 mov(30, 20), jump(20, 20, 1), jump(10, 10, 99), jump(200, 200, 1)]),
    ])], vec![dialog("Aya")]);
    let mut spec = FieldSpec::with_mesh(vec![
        [(0, 0, 0), (100, 0, 0), (0, 100, 0)],
        [(0, 0, 100), (100, 0, 100), (0, 100, 100)],
    ], vec![[u16::MAX; 3]; 2]);
    spec.overrides.push((0, section));
    let field = FieldFile::parse(spec.build()).unwrap();
    let mut def = demo_def(vec![member(0, "aya")], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 3)];
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    let moves = |beat: &Beat| beat.stage.iter().filter_map(|event| match event.what {
        Stage::Move { z, triangle, how, .. } => Some((z, triangle, how)), _ => None,
    }).collect::<Vec<_>>();
    let first = play.next(None);
    assert!(matches!(first.then, Then::Window(_)));
    assert_eq!(moves(&first), [(Some(100), Some(1), MoveHow::Jump)], "landing on the upper floor overrides the lower takeoff height");
    let second = play.next(None);
    assert!(matches!(second.then, Then::End { how: End::Finished, .. }));
    assert_eq!(moves(&second), [
        (Some(100), None, MoveHow::Walk),
        (Some(0), Some(0), MoveHow::Jump),
        (Some(0), None, MoveHow::Walk),
        (Some(100), Some(1), MoveHow::Jump),
        (None, Some(99), MoveHow::Jump), // Invalid authored triangle cannot silently pick another floor.
        (Some(100), Some(1), MoveHow::Jump), // JUMP uses the infinite plane even beyond its edges.
    ]);
    assert_eq!(play.stats().unresolved, 1, "the invalid authored triangle prevents strict settlement");
    assert!(play.writes().is_story_neutral());
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
fn initial_stage_retains_only_the_last_loop_not_later_once_or_hold_animations() {
    let section = script(vec![
        ("a", vec![(3, vec![vec![0xA2, 3, 1], vec![0xBD, 0, 32, 0], vec![0xA2, 4, 2]])]),
        ("b", vec![(3, vec![vec![0xA2, 3, 1], vec![0xAE, 4, 1]])]),
        ("c", vec![(3, vec![vec![0xA2, 3, 1], vec![0xBA, 4, 1]])]),
        ("offcast", vec![(3, vec![vec![0xA2, 7, 1]])]),
    ], vec![]);
    let field = field_of(section);
    let mut def = demo_def(vec![member(0, "aya"), member(1, "bo"), member(2, "cy")], Vec::new());
    def.setup = (0..4).map(|entity| ScriptRef::new(entity, 3)).collect();
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    let initial: Vec<_> = play.initial_stage_events().iter().filter(|event| matches!(event, Stage::Anim { .. })).cloned().collect();
    assert_eq!(initial, [Stage::Anim { entity: 0, animation: 4, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 2 }]);
    let beat = play.next(None);
    assert!(beat.stage.is_empty(), "quiet setup animations are a snapshot, not replayed events");
    assert!(matches!(beat.then, Then::End { at: 0, how: End::Finished }));
    assert!(play.writes().is_story_neutral());
}

fn default_animation_play(setup: Vec<Vec<u8>>, runtime: Vec<Vec<u8>>) -> ScenePlay {
    let section = script(vec![("a", vec![(0, vec![vec![0xA1, 99]]), (3, setup), (1, runtime)])], texts(3));
    let spec = FieldSpec { overrides: vec![(0, section)], models: vec![
        LoaderModelSpec::new("npc.char", "ABCD.HRC", &["AAFE.aki", "BBBB.yos", "CCCC.aki"]),
    ], ..FieldSpec::default() };
    let field = FieldFile::parse(spec.build()).unwrap();
    let cast = CastMember { look: Look::Model { hrc: "abcd".into(), idle: "aafe".into() }, ..member(0, "a") };
    let mut def = demo_def(vec![cast], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 3)];
    ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap()
}

#[test]
fn standing_defaults_survive_setup_once_and_hold_without_replaying_them() {
    let implicit = default_animation_play(vec![], vec![]);
    assert_eq!(implicit.initial_default_animation_clip(0), Some((0, "aafe")), "the engine starts with slot 0");
    assert!(implicit.initial_stage_events().is_empty(), "remembering the implicit default does not add a displayed setup event");
    for opcode in [0xB0, 0xB1] {
        let mut play = default_animation_play(vec![vec![0xA2, 1, 1], vec![opcode, 2, 0, 9, 1]], vec![]);
        assert_eq!(play.initial_default_animation_clip(0), Some((1, "bbbb")), "transient opcode {opcode:#x} keeps DFANM's default");
        assert!(play.initial_stage_events().is_empty(), "the existing setup snapshot does not display a loop replaced by Once/Hold");
        let beat = play.next(None);
        assert!(beat.stage.is_empty(), "quiet setup poses do not replay");
        assert!(matches!(beat.then, Then::End { at: 0, how: End::Finished }));
        assert!(play.writes().is_story_neutral());
    }
}

#[test]
fn standing_default_snapshot_requires_a_supported_rate_and_fresh_dfanm_after_asped() {
    let asped = |speed: u16| cat(&[vec![0xBD, 0], speed.to_le_bytes().to_vec()]);
    for setup in [
        vec![vec![0xA2, 1, 0]],
        vec![vec![0xA2, 1, 2]],
        vec![asped(32), vec![0xA2, 1, 1]],
        vec![vec![0xA2, 1, 1], asped(16)],
        vec![asped(32), asped(16)],
        vec![vec![0xA2, 99, 1]],
    ] {
        let play = default_animation_play(setup, vec![]);
        assert_eq!(play.initial_default_animation_clip(0), None, "unsupported or invalidated defaults must not turn into guessed slot 0");
    }
    let play = default_animation_play(vec![vec![0xA2, 1, 1], asped(32), asped(16), vec![0xA2, 2, 1]], vec![]);
    assert_eq!(play.initial_default_animation_clip(0), Some((2, "cccc")), "a fresh normal-speed DFANM restores a known default");
}

#[test]
fn runtime_default_changes_cannot_mutate_the_initial_default_snapshot() {
    let mut play = default_animation_play(vec![vec![0xA2, 1, 1], vec![0xB1, 2, 0, 9, 1]], vec![
        vec![0xA2, 2, 1], message(0, 0), vec![0xBD, 0, 32, 0], message(0, 1), vec![0xA2, 0, 1], message(0, 2),
    ]);
    let initial = play.initial_stage_events().to_vec();
    for expected in [
        Stage::Anim { entity: 0, animation: 2, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 },
        Stage::AnimSpeed { entity: 0, speed: 32 },
        Stage::Anim { entity: 0, animation: 0, how: AnimHow::Loop, first: None, last: None, speed: 32, divisor: 1 },
    ] {
        let beat = play.next(None);
        assert!(matches!(beat.then, Then::Window(_)));
        assert_eq!(beat.stage.iter().map(|event| &event.what).collect::<Vec<_>>(), [&expected]);
        assert_eq!(play.initial_default_animation_clip(0), Some((1, "bbbb")));
        assert_eq!(play.initial_stage_events(), initial.as_slice(), "runtime metadata does not rewrite setup's displayed snapshot");
        assert!(play.writes().is_story_neutral());
    }
}

#[test]
fn resumed_scene_captures_the_default_after_the_battle_prelude_without_replaying_it() {
    let section = script(vec![("a", vec![
        (0, vec![vec![0xA1, 99]]),
        (1, vec![vec![0xA2, 1, 1], vec![0xB1, 2, 0, 9, 1], wait(10), message(0, 0),
            vec![0x70, 0, 0x44, 1], vec![0xA2, 2, 1], message(0, 1)]),
    ])], texts(2));
    let spec = FieldSpec { overrides: vec![(0, section)], models: vec![
        LoaderModelSpec::new("npc.char", "ABCD.HRC", &["AAFE.aki", "BBBB.yos", "CCCC.aki"]),
    ], ..FieldSpec::default() };
    let field = FieldFile::parse(spec.build()).unwrap();
    let cast = CastMember { look: Look::Model { hrc: "abcd".into(), idle: "aafe".into() }, ..member(0, "a") };
    let mut def = demo_def(vec![cast], start(0, 1));
    def.resume_after_battle = Some(324);
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    assert_eq!(play.initial_default_animation_clip(0), Some((1, "bbbb")), "the prebattle DFANM survives the transient Hold");
    assert!(play.initial_stage_events().is_empty(), "the Hold still suppresses the initial displayed loop");
    assert!(play.writes().is_story_neutral());
    let beat = play.next(None);
    let Then::Window(window) = &beat.then else { panic!("expected the continuation's window") };
    assert!(matches!(window.message.origin, crate::script::dialogue::MessageOrigin::Canon { dialog: 1, .. }), "the prelude's window does not replay");
    assert_eq!(stage(&[beat]), [(0, Stage::Anim { entity: 0, animation: 2, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 })], "only the continuation's animation is emitted");
    assert_eq!(play.initial_default_animation_clip(0), Some((1, "bbbb")), "later visible playback cannot mutate the snapshot");
    assert!(play.initial_stage_events().is_empty());
    assert!(play.writes().is_story_neutral());
}

#[test]
fn animation_slots_use_the_actual_script_binding_and_only_the_matching_cast_model() {
    let section = script(vec![
        ("director", vec![]), ("npc", vec![(0, vec![vec![0xA1, 99]])]),
        ("canon", vec![(0, vec![vec![0xA1, 0]])]), ("wrong", vec![(0, vec![vec![0xA1, 0]])]),
        ("plain", vec![]), ("missing", vec![(0, vec![vec![0xA1, 0]])]),
    ], vec![]);
    let spec = FieldSpec { overrides: vec![(0, section)], models: vec![
        LoaderModelSpec::new("npc.char", "ABCD.HRC", &["AAFE.aki", "BBBB.yos"]),
        LoaderModelSpec::new("canon.char", "EFGH.HRC", &["EEEE.aki"]),
        LoaderModelSpec::new("wrong.char", "IJKL.HRC", &["IIII.aki"]),
    ], ..FieldSpec::default() };
    let field = FieldFile::parse(spec.build()).unwrap();
    let model = |entity, hrc: &str| CastMember { look: Look::Model { hrc: hrc.into(), idle: "aafe".into() }, ..member(entity, "actor") };
    let def = demo_def(vec![model(1, "AbCd"), member(2, "canon"), model(3, "zzzz"), model(4, "abcd"), model(5, "ijkl")], Vec::new());
    let play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    assert_eq!(play.animation_clip(1, 1), Some("bbbb"), "CHAR 99 still receives loader record 0");
    assert_eq!(play.initial_default_animation_clip(1), Some((0, "aafe")));
    assert_eq!(play.animation_clip(1, 2), None, "out-of-range slot");
    for entity in [0, 2, 3, 4, 5, u8::MAX] {
        assert_eq!(play.animation_clip(entity, 0), None, "off-cast, appearance, wrong skeleton, absent binding or absent record: {entity}");
        assert_eq!(play.initial_default_animation_clip(entity), None);
    }
    // The replacement removes npc's CHAR, moving the remaining records up one. Lookup must use
    // that script's bindings, not the original entity's old record or its CHAR operand.
    let replacement = script(vec![
        ("director", vec![]), ("npc", vec![]), ("canon", vec![(0, vec![vec![0xA1, 99]])]),
        ("wrong", vec![(0, vec![vec![0xA1, 0]])]), ("plain", vec![]), ("missing", vec![(0, vec![vec![0xA1, 0]])]),
    ], vec![]);
    let replacement = ScenePlay::with_script_section(&field, &def, &StoryState::at(4), 1, &replacement).unwrap();
    assert_eq!(replacement.animation_clip(1, 0), None);
    assert_eq!(replacement.animation_clip(5, 0), Some("iiii"));
    assert_eq!(replacement.initial_default_animation_clip(1), None);
    assert_eq!(replacement.initial_default_animation_clip(5), Some((0, "iiii")));
    assert!(replacement.writes().is_story_neutral());
    let uncertain = script(vec![
        ("director", vec![]), ("npc", vec![(0, vec![if_ub(3, 9, 0, 0, &[0xA1, 99])])]),
        ("canon", vec![(0, vec![vec![0xA1, 0]])]), ("wrong", vec![(0, vec![vec![0xA1, 0]])]),
        ("plain", vec![]), ("missing", vec![(0, vec![vec![0xA1, 0]])]),
    ], vec![]);
    let mut uncertain_def = def.clone();
    uncertain_def.cast[2] = model(3, "ijkl");
    let uncertain = ScenePlay::with_script_section(&field, &uncertain_def, &StoryState::at(4), 1, &uncertain).unwrap();
    assert_eq!(uncertain.animation_clip(1, 0), None, "conditional CHAR has no certain loader binding");
    assert_eq!(uncertain.animation_clip(3, 0), None, "an uncertain earlier declaration also makes later loader indices uncertain");
    assert_eq!(uncertain.initial_default_animation_clip(1), None);
    assert_eq!(uncertain.initial_default_animation_clip(3), None);
}

#[test]
fn canonical_animation_slots_require_a_known_matching_source_model() {
    let section = script(vec![
        ("cloud", vec![(0, vec![vec![0xA1, 99]]), (1, vec![
            vec![0xA2, 1, 1], message(0, 0), vec![0xBD, 0, 32, 0], message(0, 1),
        ])]),
        ("tifa", vec![(0, vec![vec![0xA1, 0]])]),
        ("unbound", vec![]), ("missing", vec![(0, vec![vec![0xA1, 0]])]),
    ], texts(2));
    let field_with_cloud = |hrc: &str| FieldFile::parse(FieldSpec {
        overrides: vec![(0, section.clone())],
        models: vec![
            LoaderModelSpec::new("cloud.char", hrc, &["ACFE.aki", "CLOD.aki"]),
            LoaderModelSpec::new("tifa.char", "AAGB.HRC", &["ABCD.aki", "TIFA.yos"]),
        ], ..FieldSpec::default()
    }.build()).unwrap();
    let field = field_with_cloud("AAAA.HRC");
    let canonical = |entity, name, id: &str| CastMember {
        look: Look::Appearance(id.into()), ..member(entity, name)
    };
    let def = demo_def(vec![
        canonical(0, "cloud", "ff7/cloud"), canonical(1, "tifa", "FF7/TIFA"),
        canonical(2, "unbound", "ff7/cloud"), canonical(3, "missing", "ff7/cloud"),
    ], start(0, 1));
    let mut play = ScenePlay::new(&field, &def, &StoryState::at(4), 1).unwrap();
    assert_eq!(play.animation_clip(0, 1), Some("clod"), "CHAR 99 uses original loader record 0, whose HRC is Cloud's");
    assert_eq!(play.animation_clip(1, 1), Some("tifa"), "the canonical appearance resolves case-insensitively to Tifa's matching HRC");
    assert_eq!(play.initial_default_animation_clip(0), Some((0, "acfe")));
    assert_eq!(play.initial_default_animation_clip(1), Some((0, "abcd")));
    assert_eq!(play.animation_clip(0, 2), None, "slots still require an actual loader animation");
    for entity in [2, 3, u8::MAX] {
        assert_eq!(play.animation_clip(entity, 0), None, "absent declaration, absent record or off-cast entity: {entity}");
        assert_eq!(play.initial_default_animation_clip(entity), None);
    }
    assert_eq!(play.def().cast, def.cast, "binding clips does not replace the cast's canonical appearances");
    let initial = play.initial_stage_events().to_vec();
    for expected in [
        Stage::Anim { entity: 0, animation: 1, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1 },
        Stage::AnimSpeed { entity: 0, speed: 32 },
    ] {
        let beat = play.next(None);
        assert!(matches!(beat.then, Then::Window(_)));
        assert_eq!(beat.stage.iter().map(|event| &event.what).collect::<Vec<_>>(), [&expected]);
        assert_eq!(play.initial_default_animation_clip(0), Some((0, "acfe")), "runtime changes cannot mutate the canonical default snapshot");
        assert_eq!(play.initial_stage_events(), initial.as_slice());
        assert!(play.writes().is_story_neutral());
    }
    assert!(matches!(play.next(None).then, Then::End { how: End::Finished, .. }));

    for (appearance, hrc) in [
        ("ff7/not-a-character", "AAAA.HRC"), // A default-Cloud fallback would incorrectly match.
        ("ff7/body/lean", "AWCB.HRC"), // Even a matching body rig is not a canonical appearance.
        ("ff7/cloud", "AAGB.HRC"), // Recognized character, wrong original field rig.
    ] {
        let mut rejected_def = def.clone();
        rejected_def.cast[0].look = Look::Appearance(appearance.into());
        let rejected = ScenePlay::new(&field_with_cloud(hrc), &rejected_def, &StoryState::at(4), 1).unwrap();
        assert_eq!(rejected.animation_clip(0, 0), None, "{appearance} must not claim {hrc}'s clips");
        assert_eq!(rejected.initial_default_animation_clip(0), None);
        assert_eq!(rejected.def().cast, rejected_def.cast);
        assert!(rejected.writes().is_story_neutral());
    }

    let uncertain_section = script(vec![
        ("cloud", vec![(0, vec![if_ub(3, 9, 0, 0, &[0xA1, 99])])]),
        ("tifa", vec![(0, vec![vec![0xA1, 0]])]),
        ("unbound", vec![]), ("missing", vec![(0, vec![vec![0xA1, 0]])]),
    ], vec![]);
    let uncertain = ScenePlay::with_script_section(&field, &def, &StoryState::at(4), 1, &uncertain_section).unwrap();
    for entity in [0, 1] {
        assert_eq!(uncertain.animation_clip(entity, 0), None, "an uncertain declaration also invalidates later canonical loader bindings");
        assert_eq!(uncertain.initial_default_animation_clip(entity), None);
    }
    assert!(uncertain.writes().is_story_neutral());
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
    assert!(scene(SECTOR7_PILLAR_SCENE_ID).is_none());
    assert!(scene(REACTOR_OPENING_SCENE_ID).is_none());
}

#[test]
fn flower_registration_promotes_only_its_local_playback_snapshot() {
    let def = scene(FLOWER_STATION_SCENE_ID).expect("the reviewed flower scene is registered");
    assert_eq!(def.id, "ff7/md8_2/flower");
    assert_eq!(scenes().get(13), Some(&def), "the flower keeps its existing scene number");
    assert!(def.requires_exact_transaction() && def.starts_on_arrival());
    assert!(def.offered_to(&StoryState::at(36)));
    assert!(!def.offered_to(&StoryState::at(39)));
    assert!(!crate::REVIEWED_FIELDS.contains(&"md8_2"), "an eager point must not bypass the choices");
    let original = StoryState::at(36);
    let stored = original.encode();
    let prepared = def.prepare_story(&original).unwrap();
    assert!(prepared.supports_extended());
    assert!(def.offered_to(&prepared));
    assert_eq!(prepared.moment, original.moment);
    assert_eq!(prepared.known_bit(3, 66, 0), None);
    assert_eq!(prepared.known_bit(3, 111, 0), None);
    assert_eq!(original.encode(), stored, "preparing playback does not migrate the predecessor");
    assert_eq!(def.prepare_story(&prepared), Ok(prepared));
    let mut tracked = original.clone();
    tracked.set_bit(1, 225, 0, true);
    assert_eq!(tracked.encode()[0], 2);
    assert_eq!(def.prepare_story(&tracked).unwrap().known_bit(1, 225, 0), Some(true));
    assert!(!tracked.supports_extended(), "legacy tracked bits are preserved without migrating their source");
    for other in scenes().into_iter().filter(|s| s.id != def.id) {
        if [BRIDGE_SCENE_ID, CARGO_TRAIN_SCENE_ID, CARGO_DOOR_SCENE_ID, TRAIN_SCENE_ID].contains(&other.id.as_str()) {
            assert!(other.requires_exact_transaction());
            assert_eq!(other.prepare_story(&original), original.clone().into_extended());
        } else {
            assert!(!other.requires_exact_transaction());
            assert_eq!(other.prepare_story(&original), Ok(original.clone()));
        }
    }
    let bad = StoryState::decode(&[2, 36, 0, 1, 5, 10, 1]).unwrap();
    assert!(def.prepare_story(&bad).is_err(), "unsupported legacy state must never be discarded");
    let conflicting = StoryState::decode(&[2, 36, 0, 1, 2, 225, 1]).unwrap();
    assert!(matches!(def.prepare_story(&conflicting), Err(crate::story::StoryWriteError::ConflictingLegacyAlias { .. })),
        "a word-bank alias must not overwrite the tracked byte's known-zero bit");
}

#[test]
fn flower_original_and_replacement_play_use_the_same_promoted_snapshot() {
    let section = script(vec![("a", vec![(1, vec![biton(3, 66, 0), gold(0x3A, 1), setword(2, 0, 39)])]), ("b", vec![])], vec![]);
    let field = field_of(section.clone());
    let mut def = demo_def(vec![], start(0, 1));
    def.id = FLOWER_STATION_SCENE_ID.into();
    let story = StoryState::at(36);
    let resources = Resources { gil: Some(1) };
    let original = run_to_end_with_resources(&field, &def, &story, 1, resources, &[]).unwrap();
    let replacement = run_section_to_end_with_resources(&field, &def, &story, 1, &section, resources, &[]).unwrap();
    let prepared = def.prepare_story(&story).unwrap();
    let explicit = run_to_end_with_resources(&field, &def, &prepared, 1, resources, &[]).unwrap();
    assert!(original.same_story_as(&replacement) && original.same_story_as(&explicit));
    assert!(original.unknown_persistent_reads.is_empty());
    assert_eq!(original.writes.persistent.get(&(3, 66)), Some(&(1, 1)));
    assert!(original.writes.tracked.is_empty());
    assert_eq!(original.resources.gil_after, Some(0));
    assert!(!story.supports_extended() && story.moment == 36, "the authoritative predecessor is untouched");
    let bad = StoryState::decode(&[2, 36, 0, 1, 5, 10, 1]).unwrap();
    assert!(matches!(ScenePlay::with_resources(&field, &def, &bad, 1, resources), Err(SceneError::Story(_))));
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
fn durable_scene_answers_must_match_every_choice_on_the_selected_path_exactly() {
    let question = [dialog("Q").split_last().map(|(_, r)| r.to_vec()).unwrap(), vec![0xE7], vec![b'A' - 0x20], vec![0xE7], vec![b'B' - 0x20, 0xFF]].concat();
    // The second choice exists only after choosing line 2. Its line 1 spends one gil.
    let purchase = cat(&[ask(0, 1, 2, 5, 14), if_ub(5, 14, 0, 1, &gold(0x3A, 1))]);
    let body = vec![message(0, 1), ask(0, 1, 2, 5, 13), if_ub(5, 13, 0, 2, &purchase), setword(2, 0, 39)];
    let field = field_of(script(vec![("a", vec![(1, body)])], vec![question, dialog("Before the question.")]));
    let def = demo_def(vec![], start(0, 1));
    let story = StoryState::at(36).into_extended().unwrap();
    let resources = SceneResources { gil: Some(600) };
    let strict = |answers: &[usize]| run_to_end_with_resources_strict(&field, &def, &story, 7, resources, answers);
    for (answers, gil) in [(&[1][..], 600), (&[2, 1][..], 599), (&[2, 2][..], 600)] {
        let result = strict(answers).expect("one explicit answer per question on this path");
        assert_eq!(result.resources.gil_after, Some(gil));
        assert_eq!(result.writes.moment, Some(39));
        assert_eq!(result, run_to_end_with_resources(&field, &def, &story, 7, resources, answers).unwrap());
    }
    assert_eq!(strict(&[]), Err(SceneError::MissingAnswer { question: 1 }));
    assert_eq!(strict(&[2]), Err(SceneError::MissingAnswer { question: 2 }));
    for (answers, question, answer) in [(&[0][..], 1, 0), (&[3][..], 1, 3), (&[2, 0][..], 2, 0), (&[2, 255][..], 2, 255)] {
        assert_eq!(strict(answers), Err(SceneError::InvalidAnswer { question, answer, first: 1, last: 2 }));
    }
    assert_eq!(strict(&[1, 1]), Err(SceneError::TrailingAnswers { remaining: 1 }), "no invented second question on the short branch");
    assert_eq!(strict(&[2, 1, 1, 1]), Err(SceneError::TrailingAnswers { remaining: 2 }));
    let survey = |answers: &[usize]| run_to_end_with_resources(&field, &def, &story, 7, resources, answers).unwrap();
    assert_eq!(survey(&[2]).resources.gil_after, Some(599), "surveys retain their cursor-default behavior");
    assert_eq!(survey(&[2, 0, 255]), survey(&[2, 1]), "lenient previews retain clamping and ignore unused answers");
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
        ("parent", vec![(1, vec![sound(236, 64), message(0, 0), reqew(1, 1), message(0, 3)])]),
        ("child", vec![(1, vec![message(0, 1), sound(0, 64), vec![0x70, 0, 0x44, 1], sound(61, 64), message(0, 2)])]),
    ], texts(4));
    let field = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm_for(StoryState::new_game(), &[]);
    vm.begin(start(0, 1));
    assert!(vm.seek_after_battle(&field, 324));
    let beats = vec![vm.resume(&field, None), vm.resume(&field, None), vm.resume(&field, None)];
    assert_eq!(dialogs_of(&beats), [2, 3]);
    assert_eq!(stage(&beats), [(0, Stage::Sound { channel: 1, id: 61, pan: 64 })], "seeking past the battle never replays prelude sounds");
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

// ---- authoritative scene resources -----------------------------------------------------------

fn gold(op: u8, amount: u32) -> Vec<u8> {
    [vec![op, 0], amount.to_le_bytes().to_vec()].concat()
}

fn resource_run(body: Vec<Vec<u8>>, gil: Option<u32>) -> Outcome {
    let field = field_of(script(vec![("a", vec![(1, body)])], vec![]));
    let story = StoryState::at(36).into_extended().unwrap();
    run_to_end_with_resources(&field, &demo_def(vec![], start(0, 1)), &story, 1, SceneResources { gil }, &[]).unwrap()
}

#[test]
fn resource_gil_operations_saturate_at_storage_limits_and_keep_their_order() {
    let result = resource_run(vec![gold(0x3A, 1), gold(0x39, u32::MAX), gold(0x39, 1), gold(0x3A, 2)], Some(0));
    assert_eq!(result.end, End::Finished);
    assert!(result.is_complete());
    assert_eq!(result.resources, ResourceOutcome {
        gil_before: Some(0), gil_after: Some(u32::MAX - 2),
        gil_ops: vec![GilOp::Subtract(1), GilOp::Add(u32::MAX), GilOp::Add(1), GilOp::Subtract(2)], unknown_gil: false,
    });
    assert!(!result.stats.ignored.contains_key("GOLDu") && !result.stats.ignored.contains_key("GOLDd"));
    assert_eq!(resource_run(vec![gold(0x3A, 1)], Some(1)).resources.gil_after, Some(0));
    assert_eq!(resource_run(vec![gold(0x3A, 1)], Some(600)).resources.gil_after, Some(599));
    assert_eq!(resource_run(vec![gold(0x39, 1)], Some(99_999_999)).resources.gil_after, Some(100_000_000), "no invented display cap");
}

#[test]
fn resource_gil_unknown_is_not_zero_and_cannot_produce_a_complete_outcome() {
    for body in [vec![gold(0x39, 1)], vec![gold(0x3A, 1)], vec![vec![0x3B, 0x66, 10, 12]]] {
        let result = resource_run(body, None);
        assert!(result.resources.unknown_gil);
        assert_eq!((result.resources.gil_before, result.resources.gil_after), (None, None));
        assert!(!result.is_complete());
        assert!(result.writes.is_story_neutral());
    }
    assert!(resource_run(vec![], None).is_complete(), "a scene that never needs gil needs no supplied balance");
    let explicit_zero = resource_run(vec![vec![0x3B, 0x66, 10, 12]], Some(0));
    assert!(explicit_zero.is_complete());
    assert_eq!(explicit_zero.writes.untracked.get(&(5, 10)), None, "temporary zeros do not become story data");
}

#[test]
fn resource_gil_amount_halves_obey_each_selectors_byte_or_word_width() {
    let result = resource_run(vec![
        setword(6, 10, 0x02FF), setbyte(5, 20, 1),
        // low byte from bank5[10], high word from bank6[20].
        vec![0x39, 0x56, 10, 0, 20, 0],
        vec![0x3B, 0x66, 30, 32],
    ], Some(0));
    assert!(result.is_complete());
    assert_eq!(result.resources.gil_ops, vec![GilOp::Add(0x0001_00FF), GilOp::Read]);
    assert_eq!(result.resources.gil_after, Some(0x0001_00FF));
    assert_eq!(result.writes.untracked.get(&(5, 30)), Some(&255));
    assert_eq!(result.writes.untracked.get(&(5, 32)), Some(&1));
    let aliased = resource_run(vec![setword(7, 10, 0x02FF), vec![0x39, 0x7F, 10, 0, 10, 0]], Some(0));
    assert!(aliased.is_complete());
    assert_eq!(aliased.resources.gil_after, Some(0x00FF_02FF), "bank7 is the word selector and bank15 its byte alias");
    let unknown_amount = resource_run(vec![vec![0x3A, 0x10, 40, 0, 0, 0]], Some(600));
    assert!(!unknown_amount.is_complete());
    assert!(unknown_amount.unknown_persistent_reads.contains(&(1, 40)));
    assert_eq!(unknown_amount.resources.gil_after, None, "a guessed amount never yields an authoritative after-image");
}

#[test]
fn resource_chgld_writes_low_then_high_halves_and_respects_byte_selectors() {
    let result = resource_run(vec![vec![0x3B, 0x66, 10, 11]], Some(0x1234_5678));
    assert!(result.is_complete());
    assert_eq!(result.resources.gil_ops, vec![GilOp::Read]);
    for (address, value) in [(10, 0x78), (11, 0x34), (12, 0x12)] {
        assert_eq!(result.writes.untracked.get(&(5, address)), Some(&value), "overlapping destination {address}");
    }
    let byte_dest = resource_run(vec![vec![0x3B, 0x5F, 10, 11]], Some(0x1234_5678));
    assert!(byte_dest.is_complete());
    assert_eq!(byte_dest.writes.untracked.get(&(5, 10)), Some(&0x78));
    assert_eq!(byte_dest.writes.persistent.get(&(7, 11)), Some(&(0x34, 255)));
    assert!(!byte_dest.writes.persistent.contains_key(&(7, 12)));
    assert!(!resource_run(vec![vec![0x3B, 0x06, 10, 12]], Some(600)).is_complete(), "invalid destination never silently succeeds");
    assert!(!resource_run(vec![vec![0x3B, 0x66, 255, 12]], Some(600)).is_complete(), "a word cannot wrap a bank");
}

#[test]
fn resource_equivalence_compares_amounts_and_order_even_when_final_balances_match() {
    let one = resource_run(vec![gold(0x3A, 1)], Some(0));
    let two = resource_run(vec![gold(0x3A, 2)], Some(0));
    assert_eq!(one.resources.gil_after, two.resources.gil_after);
    assert!(!one.same_story_as(&two));
    assert!(one.same_story_as(&one));
    let a = resource_run(vec![gold(0x39, 1), gold(0x3A, 1)], Some(10));
    let b = resource_run(vec![gold(0x3A, 1), gold(0x39, 1)], Some(10));
    assert_eq!(a.resources.gil_after, b.resources.gil_after);
    assert!(!a.same_story_as(&b));
    assert!(!one.same_story_as(&resource_run(vec![gold(0x3A, 1)], None)));
}

#[test]
fn resource_setup_and_main_share_one_balance_and_replacement_resources() {
    let section = script(vec![("a", vec![(0, vec![gold(0x39, 10)]), (1, vec![gold(0x3A, 1)])]), ("b", vec![])], vec![]);
    let field = field_of(section.clone());
    let mut def = demo_def(vec![], start(0, 1));
    def.setup = vec![ScriptRef::new(0, 0)];
    let story = StoryState::at(36).into_extended().unwrap();
    let resources = SceneResources { gil: Some(50) };
    let original = run_to_end_with_resources(&field, &def, &story, 1, resources, &[]).unwrap();
    assert_eq!(original.resources.gil_after, Some(59));
    assert_eq!(original.resources.gil_ops, vec![GilOp::Add(10), GilOp::Subtract(1)]);
    let replacement = run_section_to_end_with_resources(&field, &def, &story, 1, &section, resources, &[]).unwrap();
    assert!(original.same_story_as(&replacement));
    let mut play = ScenePlay::with_script_section_and_resources(&field, &def, &story, 1, &section, resources).unwrap();
    assert_eq!(play.resources().gil_after, Some(60), "setup runs exactly once");
    while !matches!(play.next(None).then, Then::End { .. }) {}
    assert_eq!(play.resources(), &original.resources);
}

#[test]
fn resource_gold_decoder_keeps_independent_bank_halves_and_only_true_literal_amount_aliases() {
    use crate::script::decode_at;
    for op in [0x39, 0x3A] {
        let literal = decode_at(&gold(op, 0x8765_4321), 0, 0).unwrap();
        assert_eq!(literal.literal("amount_lo"), Some(0x4321));
        assert_eq!(literal.literal("amount_hi"), Some(0x8765));
        assert_eq!(literal.literal("amount").map(|n| n as u32), Some(0x8765_4321));
        let variable = decode_at(&[op, 0x65, 10, 0, 20, 0], 0, 0).unwrap();
        assert_eq!(variable.operand("amount_lo").unwrap().bank, 6);
        assert_eq!(variable.operand("amount_hi").unwrap().bank, 5);
        assert_eq!(variable.operand("amount"), None);
        assert!(!variable.stray_banks);
    }
}

#[test]
fn strict_replay_acknowledges_every_exact_window_and_never_invents_a_choice() {
    let question = [dialog("Q")[..1].to_vec(), vec![0xE7, 0x21, 0xE7, 0x22, 0xFF]].concat();
    let field = field_of(script(vec![("a", vec![(1, vec![message(0, 1), ask(0, 1, 2, 5, 9), message(0, 1)])])], vec![question, dialog("Hello")]));
    let def = demo_def(vec![], start(0, 1));
    let story = StoryState::at(4).into_extended().unwrap();
    let resources = Resources { gil: Some(20) };
    let mut play = ScenePlay::with_resources(&field, &def, &story, 77, resources).unwrap();
    let identity = play.replay_identity();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(first)) = play.boundary() else { panic!() };
    assert_eq!(first.ordinal, 1);
    assert_eq!(play.next_strict(None), Err(SceneError::WindowBoundary));
    assert_eq!(play.next_strict(Some(WindowAck { checkpoint: first, answer: Some(1) })), Err(SceneError::WindowBoundary));
    let wrong = WindowCheckpoint { instruction_offset: first.instruction_offset.map(|o| o + 1), ..first };
    assert_eq!(play.next_strict(Some(WindowAck { checkpoint: wrong, answer: None })), Err(SceneError::WindowBoundary));
    assert!(play.transcript().is_empty());
    play.next_strict(Some(WindowAck { checkpoint: first, answer: None })).unwrap();
    let Some(SceneBoundary::Window(second)) = play.boundary() else { panic!() };
    assert_eq!((second.ordinal, second.choices), (2, Some([1, 2])));
    for answer in [None, Some(0), Some(3)] {
        assert_eq!(play.next_strict(Some(WindowAck { checkpoint: second, answer })), Err(SceneError::WindowBoundary));
    }
    play.next_strict(Some(WindowAck { checkpoint: second, answer: Some(2) })).unwrap();
    let Some(SceneBoundary::Window(third)) = play.boundary() else { panic!() };
    assert_eq!(third.ordinal, 3);
    let replay = replay_scene_to(&field, &def, &story, resources, identity, play.transcript(), SceneBoundary::Window(third)).unwrap();
    assert_eq!(replay.snapshot(), play.snapshot());
    assert!(matches!(replay_scene_to(&field, &def, &story, resources, identity, &play.transcript()[..1], SceneBoundary::Window(third)), Err(SceneError::ReplayBoundary)));
    play.next_strict(Some(WindowAck { checkpoint: third, answer: None })).unwrap();
    assert_eq!(play.boundary(), Some(SceneBoundary::End(End::Finished)));
    assert_eq!(play.next_strict(Some(WindowAck { checkpoint: third, answer: None })), Err(SceneError::WindowBoundary));
    let mut extra = play.transcript().to_vec();
    extra.push(SceneInput::Window(WindowAck { checkpoint: third, answer: None }));
    assert!(matches!(replay_scene(&field, &def, &story, resources, identity, &extra), Err(SceneError::WindowBoundary)));
    let mut wrong_identity = identity;
    wrong_identity.version += 1;
    assert!(matches!(replay_scene(&field, &def, &story, resources, wrong_identity, &[]), Err(SceneError::ReplayIdentity)));
    let old_identity = ReplayIdentity { version: 1, ..identity };
    assert!(matches!(replay_scene(&field, &def, &story, resources, old_identity, &[]), Err(SceneError::ReplayIdentity)),
        "version 1 checkpoints must not silently adopt ambient, background, fade, shake or jump semantics");
    let mut preview = ScenePlay::with_resources(&field, &def, &story, 77, resources).unwrap();
    preview.next(None);
    assert_eq!(preview.next_strict(None), Err(SceneError::UncheckedReplay));
}

#[test]
fn checked_battle_resume_is_once_per_instruction_execution_and_rebases_rewards_without_resetting_scratch() {
    let body = vec![vec![0x72, 8, 0], setbyte(5, 9, 77), vec![0x99, 5, 2], setbyte(1, 40, 12), gold(0x3A, 5),
        vec![0x70, 0, 72, 1], vec![0x99, 5, 3], if_ub(5, 9, 0, 77, &message(0, 0)), gold(0x39, 2)];
    let control = field_of(script(vec![("a", vec![(1, body.iter().filter(|op| op.first() != Some(&0x70)).cloned().collect())])], texts(1)));
    let field = field_of(script(vec![("a", vec![(1, body)])], texts(1)));
    let def = demo_def(vec![], vec![Step::Await(ScriptRef::new(0, 1)), Step::Await(ScriptRef::new(0, 1))]);
    let story = StoryState::at(4).into_extended().unwrap();
    let resources = Resources { gil: Some(100) };
    let mut play = ScenePlay::with_resources(&field, &def, &story, 123, resources).unwrap();
    let identity = play.replay_identity();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(first)) = play.boundary() else { panic!("{:?}", play.boundary()) };
    assert_eq!((first.ordinal, first.formation, first.flags), (1, 328, Some(8)));
    assert_eq!(play.resources().gil_after, Some(95));
    let mut committed = story.clone();
    play.writes().apply_extended_to(&mut committed, &story).unwrap();
    let resume = BattleResume { checkpoint: first, resolution: BattleResolution::Victory, story: committed.clone(), resources: Resources { gil: Some(125) } };
    for checkpoint in [BattleCheckpoint { ordinal: 2, ..first }, BattleCheckpoint { instruction_offset: first.instruction_offset + 1, ..first }, BattleCheckpoint { formation: 329, ..first }] {
        assert_eq!(play.resume_battle_checked(BattleResume { checkpoint, ..resume.clone() }), Err(SceneError::BattleBoundary));
    }
    assert_eq!(play.resume_battle_checked(BattleResume { resources: Resources { gil: None }, ..resume.clone() }), Err(SceneError::BattleResources));
    play.resume_battle_checked(resume.clone()).unwrap();
    assert_eq!(play.resume_battle_checked(resume.clone()), Err(SceneError::BattleBoundary));
    assert!(play.writes().persistent.is_empty(), "committed writes do not get republished");
    assert_eq!((play.resources().gil_before, play.resources().gil_after, play.resources().gil_ops.len()), (Some(125), Some(125), 0));
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(window)) = play.boundary() else { panic!("the preserved scratch byte opens this window") };
    play.next_strict(Some(WindowAck { checkpoint: window, answer: None })).unwrap();
    let Some(SceneBoundary::Battle(second)) = play.boundary() else { panic!() };
    assert_eq!((second.ordinal, second.instruction_offset, second.formation), (2, first.instruction_offset, first.formation));
    assert_ne!(first.identity_bytes(), second.identity_bytes());
    assert_eq!(play.resources().gil_after, Some(122), "125 reward afterimage +2, then next fight's -5");
    assert_eq!(play.resume_battle_checked(resume), Err(SceneError::BattleBoundary), "old victory cannot unlock a repeated formation");
    play.resume_battle_checked(BattleResume { checkpoint: second, resolution: BattleResolution::Victory, story: committed, resources: Resources { gil: Some(152) } }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(window)) = play.boundary() else { panic!() };
    play.next_strict(Some(WindowAck { checkpoint: window, answer: None })).unwrap();
    assert_eq!(play.resources().gil_after, Some(154));
    let uninterrupted = run_to_end_with_resources(&control, &def, &story, 123, resources, &[]).unwrap();
    for key in [(5, 2), (5, 3), (5, 9)] {
        assert_eq!(play.writes().untracked.get(&key), uninterrupted.writes.untracked.get(&key), "the same scratch/RNG state as an uninterrupted script");
    }
    let replayed = replay_scene_to(&field, &def, &story, resources, identity, play.transcript(), SceneBoundary::End(End::Finished)).unwrap();
    assert_eq!(replayed.snapshot(), play.snapshot(), "scratch, RNG draws, scheduling and effect accounting replay exactly");
}

#[test]
fn checked_resume_preserves_unsafe_prefix_observations_and_refuses_unreviewed_modes() {
    for flags in [0u16, 8, 9] {
        let body = vec![cat(&[vec![0x72], flags.to_le_bytes().to_vec()]), unknown_op(), if_ub(1, 60, 0, 0, &setbyte(5, 9, 1)), vec![0x70, 0, 72, 1]];
        let field = field_of(script(vec![("a", vec![(1, body)])], vec![]));
        let def = demo_def(vec![], start(0, 1));
        let story = StoryState::at(4).into_extended().unwrap();
        let mut play = ScenePlay::with_resources(&field, &def, &story, 1, Resources { gil: Some(10) }).unwrap();
        play.next_strict(None).unwrap();
        let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!() };
        let before = play.snapshot().unwrap();
        let result = play.resume_battle_checked(BattleResume { checkpoint, resolution: BattleResolution::Victory, story, resources: Resources { gil: Some(20) } });
        if flags != 8 { assert_eq!(result, Err(SceneError::BattleMode)); continue; }
        result.unwrap(); play.next_strict(None).unwrap();
        let after = play.snapshot().unwrap();
        assert!(!after.stats.unknown.is_empty());
        assert_eq!(after.stats.unknown, before.stats.unknown);
        assert_eq!(after.unknown_persistent_reads, before.unknown_persistent_reads);
        assert!(after.unknown_persistent_reads.contains(&(1, 60)));
    }
}

#[test]
fn the_bridge_is_registered_after_flower_as_an_explicit_line_trigger_never_an_arrival() {
    let bridge = bridge_scene();
    assert!(bridge.requires_exact_transaction());
    assert!(!bridge.starts_on_arrival());
    assert_eq!(scene(BRIDGE_SCENE_ID), Some(bridge.clone()));
    let registered = scenes();
    assert_eq!(registered.get(14).map(|s| s.id.as_str()), Some(BRIDGE_SCENE_ID));
    assert_eq!(registered.get(15).map(|s| s.id.as_str()), Some(CARGO_TRAIN_SCENE_ID));
    assert!(cargo_train_scene().starts_on_arrival() && cargo_train_scene().requires_exact_transaction());
    assert_eq!(bridge.line_trigger(), Some(SceneLineTrigger { a: [-614, 835, 518], b: [-238, 835, 518] }));
    assert!(bridge.prepare_story(&StoryState::at(39)).unwrap().supports_extended());
}


#[test]
fn a_battle_resume_is_external_progress_for_the_same_thread() {
    // The same BATTLE instruction loops without any intervening write or window. Its next
    // execution must be a fresh checkpoint, not mistaken for an idle script loop.
    let body = vec![vec![0x72, 8, 0], vec![0x70, 0, 72, 1], vec![0x12, 4]];
    let field = field_of(script(vec![("a", vec![(1, body)])], vec![]));
    let def = demo_def(vec![], start(0, 1));
    let story = StoryState::at(4).into_extended().unwrap();
    let mut play = ScenePlay::with_resources(&field, &def, &story, 1, Resources { gil: Some(10) }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(first)) = play.boundary() else { panic!() };
    play.resume_battle_checked(BattleResume { checkpoint: first, resolution: BattleResolution::Victory, story, resources: Resources { gil: Some(20) } }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(second)) = play.boundary() else { panic!("{:?}", play.boundary()) };
    assert_eq!(second, BattleCheckpoint { ordinal: 2, ..first });
}


#[test]
fn a_battle_resume_wakes_a_parked_peer_to_observe_authoritative_state() {
    let conditional = if_ub(1, 40, 0, 1, &message(0, 0));
    let back = vec![0x12, conditional.len() as u8];
    let field = field_of(script(vec![
        ("director", vec![(1, vec![wait(1), vec![0x72, 8, 0], vec![0x70, 0, 72, 1]])]),
        ("reader", vec![(1, vec![conditional, back])]),
    ], texts(1)));
    let def = demo_def(vec![], vec![Step::Start(ScriptRef::new(1, 1)), Step::Await(ScriptRef::new(0, 1))]);
    let mut story = StoryState::at(4).into_extended().unwrap();
    story.write_masked(1, 40, 0, 255).unwrap();
    let mut play = ScenePlay::with_resources(&field, &def, &story, 1, Resources { gil: Some(10) }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!() };
    assert!(play.stats().parked > 0);
    story.write_masked(1, 40, 1, 255).unwrap();
    play.resume_battle_checked(BattleResume { checkpoint, resolution: BattleResolution::Victory, story, resources: Resources { gil: Some(20) } }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(window)) = play.boundary() else { panic!("a parked peer must see the new afterimage") };
    assert_eq!(window.entity, 1);
}


#[test]
fn durable_replay_refuses_a_setup_question_or_a_malformed_question_default() {
    let question = vec![0x31, 0xE7, 0x21, 0xE7, 0x22, 0xFF];
    for setup in [false, true] {
        let body = vec![ask(0, if setup { 1 } else { 9 }, 10, 5, 9), gold(0x3A, 1)];
        let field = field_of(script(vec![("a", vec![(if setup { 0 } else { 1 }, body)])], vec![question.clone()]));
        let mut def = demo_def(vec![], start(0, 1));
        if setup { def.setup = vec![ScriptRef::new(0, 0)]; }
        let story = StoryState::at(4).into_extended().unwrap();
        let resources = Resources { gil: Some(10) };
        let mut play = ScenePlay::with_resources(&field, &def, &story, 1, resources).unwrap();
        assert_eq!(play.next_strict(None), Err(SceneError::ImplicitAnswer));
        assert_eq!(run_to_end_with_resources_strict(&field, &def, &story, 1, resources, &[]), Err(SceneError::ImplicitAnswer));
    }
}


#[test]
fn strict_replay_rejects_missing_question_text_and_reversed_authored_choices() {
    for instruction in [ask(99, 0, 0, 5, 9), ask(0, 2, 1, 5, 9)] {
        let field = field_of(script(vec![("a", vec![(1, vec![instruction])])], vec![vec![0x31, 0xE7, 0x21, 0xE7, 0x22, 0xFF]]));
        let def = demo_def(vec![], start(0, 1));
        let mut play = ScenePlay::with_resources(&field, &def, &StoryState::at(4).into_extended().unwrap(), 1, Resources { gil: Some(0) }).unwrap();
        assert_eq!(play.next_strict(None), Err(SceneError::ImplicitAnswer));
    }
}


#[test]
fn a_missing_message_cannot_be_omitted_from_a_durable_transcript() {
    let field = field_of(script(vec![("a", vec![(1, vec![message(0, 99), setword(2, 0, 48)])])], texts(1)));
    let def = demo_def(vec![], start(0, 1));
    let story = StoryState::at(39).into_extended().unwrap();
    let resources = Resources { gil: Some(10) };
    let mut play = ScenePlay::with_resources(&field, &def, &story, 1, resources).unwrap();
    assert_eq!(play.next_strict(None), Err(SceneError::WindowData));
    assert_eq!(play.stats().unresolved, 1);
    assert_eq!(run_to_end_with_resources_strict(&field, &def, &story, 1, resources, &[]), Err(SceneError::WindowData));
    assert!(!run_to_end_with_resources(&field, &def, &story, 1, resources, &[]).unwrap().is_complete());
}

#[test]
fn bridge_origin_is_quantized_validated_and_bound_into_the_replay_source() {
    use fmv_core::Pos;
    let field = field_of(script(vec![("a", vec![])], vec![]));
    let def = bridge_scene();
    let facing = crate::field_direction_to_facing(73);
    let bound = with_trigger_position(&field, &def, Pos::new(25.4, 10.2, 0.0), facing, Some(0)).unwrap();
    assert_eq!(bound.cast.iter().find(|m| m.entity == 1).unwrap().start, Some(Spot { x: 25, y: 10, triangle: 0, direction: 73 }));
    let looked_up = with_trigger_position(&field, &def, Pos::new(25.4, 10.2, 0.0), facing, None).unwrap();
    assert_eq!(looked_up, bound);
    assert!(with_trigger_position(&field, &def, Pos::new(25.0, 10.0, 0.0), f32::MAX, Some(0)).is_ok(), "finite huge headings are normalized before integer conversion");
    let changed = with_trigger_position(&field, &def, Pos::new(26.4, 10.2, 0.0), facing, Some(0)).unwrap();
    assert_ne!(super::replay_identity::fingerprint(&[], &bound), super::replay_identity::fingerprint(&[], &changed));
    for (pos, angle, hint) in [(Pos::new(f32::NAN, 0.0, 0.0), 0.0, None), (Pos::new(40_000.0, 0.0, 0.0), 0.0, None),
        (Pos::new(25.0, 10.0, 0.0), f32::INFINITY, None), (Pos::new(25.0, 10.0, 0.0), 0.0, Some(u32::MAX)),
        (Pos::new(-30_000.0, -30_000.0, 0.0), 0.0, None)] {
        assert_eq!(with_trigger_position(&field, &def, pos, angle, hint), Err(SceneError::SceneOrigin));
    }
    assert_eq!(with_trigger_position(&field, &flower_station_scene(), Pos::new(25.0, 10.0, 0.0), 0.0, None), Err(SceneError::SceneOrigin));
}
