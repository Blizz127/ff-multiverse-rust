//! Background presentation from synthetic field scripts, including quiet setup and awaited helpers.

use super::{Beat, Limits, SceneVars, ScriptRef, Stage, Step, Then, Vm};
use crate::background::{BackgroundOp, BackgroundState};
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::script::FieldScript;
use crate::story::StoryState;

fn code(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = parts.concat();
    bytes.push(0x00); // RET
    bytes
}

fn section(scripts: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    SynthScript { dialogs: vec![dialog("First."), dialog("Second.")], ..Default::default() }
        .entity("director", scripts).build()
}

fn vm() -> Vm {
    Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, Limits::default())
}

fn start(slot: u8) -> Vec<Step> {
    vec![Step::Start(ScriptRef::new(0, slot))]
}

fn backgrounds(beat: &Beat) -> Vec<(u32, BackgroundOp)> {
    beat.stage.iter().filter_map(|event| match event.what {
        Stage::Background(op) => Some((event.at, op)),
        _ => None,
    }).collect()
}

#[test]
fn literal_background_changes_keep_instruction_order_and_window_relative_timing() {
    let bytes = section(vec![(1, code(&[
        &[0xE0, 0, 8, 2], // BGON group 8, bit 2
        &[0x24, 12, 0], // WAIT 12
        &[0xE1, 0, 8, 2], &[0xE2, 0, 8], &[0xE3, 0, 8], &[0xE4, 0, 8],
        &[0x40, 0, 0], // MESSAGE
        &[0x24, 7, 0], &[0xE0, 0, 8, 1], &[0x40, 0, 1],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let first = vm.resume(&script, None);
    assert_eq!(backgrounds(&first), [
        (0, BackgroundOp::On { group: 8, frame: 2 }),
        (12, BackgroundOp::Off { group: 8, frame: 2 }),
        (12, BackgroundOp::ShiftLeft { group: 8 }),
        (12, BackgroundOp::ShiftRight { group: 8 }),
        (12, BackgroundOp::Clear { group: 8 }),
    ]);
    assert!(matches!(first.then, Then::Window(ref w) if w.at == 12));
    let second = vm.resume(&script, None);
    assert_eq!(backgrounds(&second), [(7, BackgroundOp::On { group: 8, frame: 1 })]);
    assert!(matches!(second.then, Then::Window(ref w) if w.at == 7));
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    for (name, count) in [("BGON", 2), ("BGOFF", 1), ("BGROL", 1), ("BGROL2", 1), ("BGCLR", 1)] {
        assert_eq!(vm.stats().handled.get(name), Some(&count), "{name}");
    }
    assert!(vm.stats().ignored.is_empty() && vm.stats().unknown.is_empty());
    assert_eq!(vm.stats().unresolved, 0);
    assert!(vm.writes().is_empty(), "background presentation does not write the story");
}

#[test]
fn background_operands_resolve_their_high_and_low_banks_including_single_operand_low_nibbles() {
    // The same address has different values in the two banks, so swapping the nibbles is observable.
    let bytes = section(vec![(1, code(&[
        &[0x80, 0x30, 50, 8], // SETBYTE bank 3, address 50, value 8
        &[0x80, 0x50, 50, 2], // SETBYTE bank 5, address 50, value 2
        &[0xE0, 0x35, 50, 50],
        &[0x80, 0x50, 50, 1], // later reads see the new value, not the initial one
        &[0xE1, 0x35, 50, 50],
        &[0xE2, 0x03, 50], &[0xE3, 0x03, 50], &[0xE4, 0x03, 50],
        &[0xE0, 0x30, 50, 4], // banked group, literal frame
        &[0xE1, 0x05, 8, 50], // literal group, banked frame
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
    assert_eq!(backgrounds(&beat), [
        (0, BackgroundOp::On { group: 8, frame: 2 }),
        (0, BackgroundOp::Off { group: 8, frame: 1 }),
        (0, BackgroundOp::ShiftLeft { group: 8 }),
        (0, BackgroundOp::ShiftRight { group: 8 }),
        (0, BackgroundOp::Clear { group: 8 }),
        (0, BackgroundOp::On { group: 8, frame: 4 }),
        (0, BackgroundOp::Off { group: 8, frame: 1 }),
    ]);
    assert!(vm.stats().ignored.is_empty() && vm.stats().unknown.is_empty());
    assert_eq!(vm.stats().unresolved, 0);
}

#[test]
fn unsupported_background_groups_and_frames_are_counted_without_emitting_or_aliasing() {
    let bytes = section(vec![(1, code(&[
        &[0xE0, 0, 63, 7], // last valid group and frame
        &[0xE0, 0, 64, 0], &[0xE0, 0, 63, 8],
        &[0xE1, 0, 255, 0], &[0xE1, 0, 63, 255],
        &[0xE2, 0, 64], &[0xE3, 0, 255], &[0xE4, 0, 64],
        &[0x24, 4, 0], &[0xE1, 0, 63, 7],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
    assert_eq!(backgrounds(&beat), [
        (0, BackgroundOp::On { group: 63, frame: 7 }),
        (4, BackgroundOp::Off { group: 63, frame: 7 }),
    ]);
    for (name, count) in [("BGON", 2), ("BGOFF", 2), ("BGROL", 1), ("BGROL2", 1), ("BGCLR", 1)] {
        assert_eq!(vm.stats().ignored.get(name), Some(&count), "{name}");
    }
    assert!(vm.stats().unknown.is_empty());
    assert_eq!(vm.stats().unresolved, 0);
}

#[test]
fn quiet_setup_background_changes_survive_begin_once_without_reappearing_as_timed_events() {
    let bytes = section(vec![
        (3, code(&[
            &[0xE0, 0, 8, 2], &[0x24, 4, 0], &[0xE2, 0, 8],
            &[0xE1, 0, 8, 3], &[0xE0, 0, 9, 1],
        ])),
        (1, code(&[&[0xE0, 0, 8, 0], &[0x24, 6, 0], &[0xE1, 0, 9, 1], &[0x40, 0, 0]])),
    ]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    let initial = [
        BackgroundOp::On { group: 8, frame: 2 }, BackgroundOp::ShiftLeft { group: 8 },
        BackgroundOp::Off { group: 8, frame: 3 }, BackgroundOp::On { group: 9, frame: 1 },
    ];
    assert_eq!(vm.initial_background_ops(), initial);
    assert!(!vm.stats().setup_incomplete);
    let mut state = BackgroundState::default();
    for op in vm.initial_background_ops() { state.apply(*op); }
    assert!(!state.visible(2, 8, 255), "setup's on, shift and off must retain their order");
    assert!(state.visible(2, 9, 2));
    vm.begin(start(1));
    assert_eq!(vm.initial_background_ops(), initial, "begin retains the setup snapshot");
    let beat = vm.resume(&script, None);
    assert_eq!(backgrounds(&beat), [
        (0, BackgroundOp::On { group: 8, frame: 0 }),
        (6, BackgroundOp::Off { group: 9, frame: 1 }),
    ], "setup events and its four-frame wait are not replayed in the first beat");
    assert!(matches!(beat.then, Then::Window(ref w) if w.at == 6));
    assert_eq!(vm.initial_background_ops(), initial, "ordinary playback cannot extend the setup list");
    let end = vm.resume(&script, None);
    assert!(end.stage.is_empty());
    assert!(matches!(end.then, Then::End { how: End::Finished, .. }));
}

#[test]
fn seeking_a_legacy_battle_retains_setup_and_discarded_prelude_once_then_times_only_the_tail() {
    let bytes = section(vec![
        (3, code(&[&[0xE0, 0, 8, 1]])),
        (1, code(&[
            &[0x24, 2, 0], &[0xE2, 0, 8], &[0x40, 0, 0],
            &[0x24, 3, 0], &[0xE0, 0, 9, 2], &[0x70, 0, 0x2F, 0x01], // BATTLE 303
            &[0xE1, 0, 8, 2], &[0x24, 6, 0], &[0xE0, 0, 10, 1], &[0x40, 0, 1],
        ])),
    ]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    vm.begin(start(1));
    assert!(vm.seek_after_battle(&script, 303));
    let initial = [
        BackgroundOp::On { group: 8, frame: 1 },
        BackgroundOp::ShiftLeft { group: 8 },
        BackgroundOp::On { group: 9, frame: 2 },
    ];
    assert_eq!(vm.initial_background_ops(), initial, "setup and both discarded beats occur exactly once");
    let mut state = BackgroundState::default();
    for op in vm.initial_background_ops() { state.apply(*op); }
    assert!(state.visible(2, 8, 4) && !state.visible(2, 8, 8), "the prefix shift was applied once");
    assert!(state.visible(2, 9, 4));
    let tail = vm.resume(&script, None);
    assert_eq!(backgrounds(&tail), [
        (0, BackgroundOp::Off { group: 8, frame: 2 }),
        (6, BackgroundOp::On { group: 10, frame: 1 }),
    ], "prefix events and its elapsed five frames do not replay after the battle");
    assert!(matches!(tail.then, Then::Window(ref w) if w.at == 6));
    assert_eq!(vm.initial_background_ops(), initial, "the tail stays in timed playback");
    let end = vm.resume(&script, None);
    assert!(end.stage.is_empty());
    assert!(matches!(end.then, Then::End { how: End::Finished, .. }));
    assert_eq!(vm.initial_background_ops(), initial);
}

#[test]
fn an_awaited_background_helper_changes_the_layer_before_its_requester_continues() {
    let bytes = SynthScript { dialogs: vec![dialog("Door changed.")], ..Default::default() }
        .entity("cloud", vec![(8, code(&[
            &[0x03, 1, 0x41], // REQEW helper entity 1, priority 2, script 1
            &[0xE0, 0, 9, 0], &[0x40, 0, 0],
        ]))])
        .entity("door", vec![(1, code(&[&[0x24, 5, 0], &[0xE0, 0, 8, 2]]))])
        .build();
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(8));
    let beat = vm.resume(&script, None);
    assert_eq!(backgrounds(&beat), [
        (5, BackgroundOp::On { group: 8, frame: 2 }),
        (5, BackgroundOp::On { group: 9, frame: 0 }),
    ]);
    assert!(matches!(beat.then, Then::Window(ref w) if w.at == 5));
    assert_eq!(vm.stats().dropped_requests, 0);
}

#[test]
fn a_redundant_background_loop_parks_without_dropping_its_presentation_command() {
    let bytes = section(vec![(1, vec![0xE0, 0, 8, 2, 0x24, 1, 0, 0x12, 7])]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }), "a stable group is not forever-changing scene work: {:?}", beat.then);
    let ops = backgrounds(&beat);
    assert!(!ops.is_empty() && ops.len() <= 3);
    assert!(ops.iter().all(|(_, op)| *op == BackgroundOp::On { group: 8, frame: 2 }));
}
