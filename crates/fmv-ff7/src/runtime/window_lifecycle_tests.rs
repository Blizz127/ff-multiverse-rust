//! Window lifecycle tests on synthetic scripts; no game data or timing sleeps.

use super::*;
use crate::script::synth::{dialog, SynthScript};

const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

fn script0(init: &[u8], main: &[u8]) -> (usize, Vec<u8>) {
    (0, [init, &[0], main].concat())
}

fn flag(address: u8) -> [u8; 4] {
    [0x80, 0x10, address, 1]
}

fn runtime(mut script: SynthScript) -> FieldRuntime {
    script.scale = 512;
    script.dialogs.push(dialog("Synthetic"));
    let mut rt = FieldRuntime::new(&script.build(), Banks::default()).unwrap();
    rt.init();
    rt
}

fn init_window(width: u16, height: u16, mode: u8, passive: bool) -> Vec<u8> {
    let mut init = vec![0x50, 0, 8, 0, 8, 0];
    init.extend_from_slice(&width.to_le_bytes());
    init.extend_from_slice(&height.to_le_bytes());
    init.extend_from_slice(&[0x52, 0, mode, u8::from(passive)]);
    init
}

fn message(width: u16, height: u16, mode: u8, passive: bool) -> FieldRuntime {
    let main = [&[0x40, 0, 0][..], &flag(0x50), IDLE].concat();
    runtime(SynthScript::default().entity("owner", vec![script0(&init_window(width, height, mode, passive), &main)]))
}

fn geometry(rt: &FieldRuntime) -> (WindowPhase, i32, i32) {
    let open = rt.windows()[0].open.as_ref().expect("window still occupied");
    (open.phase, open.current_width, open.current_height)
}

fn until_ready(rt: &mut FieldRuntime) {
    for _ in 0..16 {
        if rt.windows()[0].open.as_ref().is_some_and(|open| open.phase == WindowPhase::Ready) {
            return;
        }
        rt.tick(None);
    }
    panic!("window did not become ready");
}

#[test]
fn opening_uses_authored_quarters_then_a_distinct_text_poll_for_every_style() {
    // Neither non-multiples of four nor small dimensions may gain an invented minimum growth quantum.
    let cases: &[(u16, u16, &[(i32, i32)])] = &[
        (304, 73, &[(76, 18), (152, 36), (228, 54), (304, 72), (304, 73)]),
        (319, 73, &[(79, 18), (158, 36), (237, 54), (316, 72), (319, 73)]),
        (212, 57, &[(53, 14), (106, 28), (159, 42), (212, 56), (212, 57)]),
        (9, 9, &[(8, 8), (9, 9)]),
        (3, 3, &[(8, 8), (3, 3)]),
        (0, 0, &[(8, 8), (0, 0)]),
        (0, 73, &[(8, 18), (0, 36), (0, 54), (0, 72), (0, 73)]),
    ];
    for &(width, height, sizes) in cases {
        for mode in [0, 1, 2] {
            let mut rt = message(width, height, mode, false);
            for (index, &(x, y)) in sizes.iter().enumerate() {
                rt.tick(None);
                let phase = if index + 1 == sizes.len() { WindowPhase::Text } else { WindowPhase::Opening };
                assert_eq!(geometry(&rt), (phase, x, y), "{width}x{height}, mode {mode}, poll {}", index + 1);
                assert!(!rt.window_ready(0));
                assert!(rt.drain_events().is_empty(), "the host must not receive an answerable window yet");
            }
            rt.tick(None);
            assert_eq!(geometry(&rt), (WindowPhase::Ready, i32::from(width), i32::from(height)));
            assert!(rt.window_ready(0));
            assert!(matches!(rt.drain_events().as_slice(), [Event::Window { window: 0, ask: None, .. }]));
            rt.tick(None);
            assert!(rt.drain_events().is_empty(), "readiness is emitted once");
        }
    }
}

#[test]
fn early_wrong_kind_and_duplicate_message_inputs_do_not_complete_the_owner() {
    let mut rt = message(319, 73, 0, false);
    rt.tick(None);
    assert!(!rt.close_window(0));
    assert!(!rt.answer(0, 0));
    until_ready(&mut rt);
    assert_eq!(rt.mem.banks.get(0x50), 0, "early input was not queued");
    assert!(!rt.answer(0, 0), "a message is not a question");
    assert!(rt.close_window(0));
    assert!(!rt.close_window(0));
    assert!(!rt.window_ready(0));
    assert!(rt.window_waiting());
    assert!(!rt.windows()[0].open.as_ref().unwrap().closed);
    for expected in [(240, 55), (161, 37), (82, 19)] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Closing, expected.0, expected.1));
        assert!(rt.window_waiting());
        assert_eq!(rt.mem.banks.get(0x50), 0);
    }
    rt.tick(None);
    assert!(rt.windows()[0].open.is_none());
    assert!(!rt.window_waiting());
    assert_eq!(rt.mem.banks.get(0x50), 1);
    assert!(rt.drain_events().iter().any(|event| matches!(event, Event::WindowDone { window: 0 })));
}

#[test]
fn a_small_window_closes_without_unsigned_wrap_or_an_extra_minimum_step() {
    for (width, height) in [(0, 0), (3, 3), (9, 9)] {
        let mut rt = message(width, height, 0, false);
        until_ready(&mut rt);
        assert!(rt.close_window(0));
        rt.tick(None);
        assert!(rt.windows()[0].open.is_none(), "{width}x{height}");
        assert_eq!(rt.mem.banks.get(0x50), 1);
    }
}

#[test]
fn a_zero_width_keeps_its_zero_quantum_while_normal_height_closes() {
    let mut rt = message(0, 73, 0, false);
    until_ready(&mut rt);
    assert!(rt.close_window(0));
    for height in [55, 37, 19] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Closing, 8, height));
        assert_eq!(rt.mem.banks.get(0x50), 0);
    }
    rt.tick(None);
    assert!(rt.windows()[0].open.is_none());
    assert_eq!(rt.mem.banks.get(0x50), 1);
}

#[test]
fn ask_keeps_control_and_defers_the_selected_value_until_closing_finishes() {
    let ask = [0x48, 0x01, 0, 0, 1, 3, 0x60];
    let main = [&ask[..], &flag(0x50), IDLE].concat();
    let mut rt = runtime(SynthScript::default().entity("owner", vec![script0(&init_window(212, 57, 0, false), &main)]));
    rt.tick(None);
    assert!(rt.question_open());
    assert!(rt.player_scripted());
    assert!(!rt.answer(0, 2));
    until_ready(&mut rt);
    assert!(!rt.close_window(0), "dismiss without a selection is the wrong kind");
    assert!(!rt.answer(0, 3), "choice index is outside the authored range");
    assert!(rt.answer(0, 2));
    assert!(!rt.answer(0, 1), "a duplicate cannot replace the accepted choice");
    for expected in [(159, 43), (106, 29), (53, 15)] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Closing, expected.0, expected.1));
        assert!(rt.question_open());
        assert!(rt.player_scripted());
        assert!(rt.window_waiting());
        assert_eq!(rt.mem.banks.get(0x60), 0);
    }
    rt.tick(None);
    assert!(!rt.question_open());
    assert!(!rt.player_scripted());
    assert_eq!(rt.mem.banks.get(0x60), 3, "choice 2 of lines 1..3 stores line 3");
    assert_eq!(rt.mem.banks.get(0x50), 1);
}

#[test]
fn passive_messages_reject_player_input_but_a_foreign_wclse_can_close_them() {
    let owner = script0(&init_window(304, 73, 0, true), &[&[0x40, 0, 0][..], &flag(0x50), IDLE].concat());
    // Wait until the owner is ready, then request its closure without polling the owner's geometry.
    let closer = script0(&[], &[&[0x24, 8, 0, 0x54, 0][..], &flag(0x51), IDLE].concat());
    let mut rt = runtime(SynthScript::default().entity("owner", vec![owner]).entity("closer", vec![closer]));
    until_ready(&mut rt);
    assert!(rt.window_passive(0));
    assert!(!rt.window_waiting());
    assert!(!rt.close_window(0));
    assert!(!rt.answer(0, 0));
    while rt.mem.banks.get(0x51) == 0 && rt.frame() < 16 { rt.tick(None); }
    assert_eq!(rt.mem.banks.get(0x51), 1);
    assert_eq!(geometry(&rt), (WindowPhase::Closing, 304, 73), "foreign WCLSE only admits closing");
    assert_eq!(rt.mem.banks.get(0x50), 0);
    for _ in 0..4 { rt.tick(None); }
    assert!(rt.windows()[0].open.is_none());
    assert_eq!(rt.mem.banks.get(0x50), 1);
}

#[test]
fn wclse_waits_through_opening_then_requests_close_without_polling_geometry() {
    let owner = script0(&init_window(304, 73, 0, false), &[&[0x40, 0, 0][..], &flag(0x50), IDLE].concat());
    let closer = script0(&[], &[&[0x54, 0][..], &flag(0x51), IDLE].concat());
    let mut rt = runtime(SynthScript::default().entity("owner", vec![owner]).entity("closer", vec![closer]));
    for expected in [(76, 18), (152, 36), (228, 54), (304, 72)] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Opening, expected.0, expected.1));
        assert_eq!(rt.mem.banks.get(0x51), 0, "WCLSE remains at its instruction while the owner opens");
    }
    rt.tick(None);
    assert_eq!(geometry(&rt), (WindowPhase::Closing, 304, 73));
    assert_eq!(rt.mem.banks.get(0x51), 1, "WCLSE advances on acceptance, before shrink");
    assert_eq!(rt.mem.banks.get(0x50), 0);
    assert!(!rt.drain_events().iter().any(|event| matches!(event, Event::Window { .. })), "the close request arrived during the distinct Text phase");
    rt.tick(None);
    assert_eq!(geometry(&rt), (WindowPhase::Closing, 228, 55));
}

#[test]
fn foreign_wcls_never_double_steps_the_owners_geometry() {
    let owner = script0(&init_window(304, 73, 0, false), &[&[0x40, 0, 0][..], &flag(0x50), IDLE].concat());
    let closer = script0(&[], &[&[0x2E, 0][..], &flag(0x51), IDLE].concat());
    let mut rt = runtime(SynthScript::default().entity("owner", vec![owner]).entity("closer", vec![closer]));
    for expected in [(76, 18), (152, 36), (228, 54), (304, 72)] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Opening, expected.0, expected.1));
    }
    rt.tick(None);
    assert_eq!(geometry(&rt), (WindowPhase::Closing, 304, 73));
    for expected in [(228, 55), (152, 37), (76, 19)] {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Closing, expected.0, expected.1));
        assert_eq!(rt.mem.banks.get(0x50), 0);
        assert_eq!(rt.mem.banks.get(0x51), 0);
    }
    rt.tick(None);
    assert!(rt.windows()[0].open.is_none());
    assert_eq!(rt.mem.banks.get(0x50), 1);
    assert_eq!(rt.mem.banks.get(0x51), 1, "owner ran first and completed MESSAGE before foreign WCLS found the slot vacant");
}

#[test]
fn owner_preemption_pauses_opening_while_other_entities_keep_ticking() {
    // The request happens on frame 2, suspending MESSAGE for four frames in the owner's higher-priority script.
    let requester = script0(&[], &[&[0x24, 1, 0, 0x03, 1, 0x21][..], IDLE].concat());
    let owner = script0(&init_window(304, 73, 0, false), &[0x40, 0, 0, 0x5F, 0x12, 1]);
    let interrupt = (1, vec![0x24, 4, 0, 0]);
    let counter = script0(&[], &[0x95, 0x02, 0x51, 0x12, 0x03]);
    let mut rt = runtime(SynthScript::default().entity("request", vec![requester]).entity("owner", vec![owner, interrupt]).entity("counter", vec![counter]));
    rt.tick(None);
    assert_eq!(geometry(&rt), (WindowPhase::Opening, 76, 18));
    for frame in 2..=5 {
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Opening, 76, 18));
        assert_eq!(rt.entity_state(1).unwrap().0, 1);
        assert_eq!(rt.mem.banks.get(0x51), frame);
    }
    rt.tick(None);
    assert_eq!(rt.entity_state(1).unwrap().0, MAIN_PRIORITY);
    assert_eq!(geometry(&rt), (WindowPhase::Opening, 152, 36));
}

#[test]
fn self_wcls_and_wsizw_advance_only_on_a_later_vacant_poll() {
    for instruction in [vec![0x2E, 0], vec![0x2F, 0, 8, 0, 8, 0, 100, 0, 50, 0]] {
        let resize = instruction[0] == 0x2F;
        let requester = script0(&[], &[&[0x24, 1, 0, 0x01, 1, 0x21][..], IDLE].concat());
        let owner = script0(&init_window(304, 73, 0, false), &[0x40, 0, 0, 0x5F, 0x12, 1]);
        // Stay here after closing: this case does not assume how a suspended MESSAGE resumes.
        let close = (1, [&instruction[..], &flag(0x50), IDLE].concat());
        let mut rt = runtime(SynthScript::default().entity("request", vec![requester]).entity("owner", vec![owner, close]));
        rt.tick(None);
        assert_eq!(geometry(&rt), (WindowPhase::Opening, 76, 18));
        for _ in 0..20 {
            rt.tick(None);
            if rt.windows()[0].open.is_none() { break; }
            assert_eq!(rt.mem.banks.get(0x50), 0);
        }
        assert!(rt.windows()[0].open.is_none());
        assert_eq!(rt.mem.banks.get(0x50), 0, "the poll which clears an occupied slot still yields");
        assert_eq!((rt.windows()[0].width, rt.windows()[0].height), (304, 73));
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x50), 1);
        if resize { assert_eq!((rt.windows()[0].width, rt.windows()[0].height), (100, 50)); }
    }
}
