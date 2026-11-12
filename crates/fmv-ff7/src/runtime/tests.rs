//! Runtime tests on synthetic scripts (never game data).

use super::*;
use crate::script::synth::{dialog, SynthScript};

/// `[init..., RET, main...]` as script 0.
fn script0(init: &[u8], main: &[u8]) -> (usize, Vec<u8>) {
    let mut v = init.to_vec();
    v.push(0x00);
    v.extend_from_slice(main);
    (0, v)
}

/// A main loop that only waits a frame forever: `WAIT1; JMPB 1`.
const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

fn runtime(s: SynthScript) -> FieldRuntime {
    let mut s = s;
    s.scale = 512;
    let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
    rt.init();
    rt
}

fn set_byte(bank: u8, addr: u8, value: u8) -> Vec<u8> {
    // SETBYTE: [0x80][dst<<4 | src][dst addr][value]
    vec![0x80, bank << 4, addr, value]
}

#[test]
fn init_runs_to_the_first_ret_and_main_runs_on_ticks() {
    let s = SynthScript::default().entity("dir", vec![script0(&set_byte(1, 0x10, 7), &[&set_byte(1, 0x11, 9)[..], IDLE].concat())]);
    let mut rt = runtime(s);
    assert_eq!(rt.mem.banks.get(0x10), 7);
    assert_eq!(rt.mem.banks.get(0x11), 0, "main has not run");
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x11), 9);
    assert!(rt.coverage().unsupported.is_empty(), "{:?}", rt.coverage().unsupported);
}

#[test]
fn a_backward_jump_yields_so_an_idle_loop_costs_one_visit_a_frame() {
    // INC Var[1][0x20] then JMPB back to it: one increment per frame, not eight.
    let main = [0x95, 0x02, 0x20, 0x12, 0x03];
    let s = SynthScript::default().entity("count", vec![script0(&[], &main)]);
    let mut rt = runtime(s);
    for _ in 0..5 {
        rt.tick(None);
    }
    assert_eq!(rt.mem.banks.get(0x20), 5);
}

#[test]
fn wait_holds_for_its_frames() {
    // WAIT 3 then SET flag.
    let main = [&[0x24, 3, 0][..], &set_byte(1, 0x30, 1), IDLE].concat();
    let s = SynthScript::default().entity("w", vec![script0(&[], &main)]);
    let mut rt = runtime(s);
    let mut frames = 0;
    while rt.mem.banks.get(0x30) == 0 && frames < 20 {
        rt.tick(None);
        frames += 1;
    }
    assert_eq!(frames, 4, "set on the visit after the third waiting frame");
}

#[test]
fn reqew_waits_for_the_requested_script_to_return() {
    // Entity 0 main: REQEW entity 1 script 2 at priority 2, then set 0x40 = 1.
    // Entity 1 script 2: WAIT 2, set 0x41 = 1, RET.
    let e0 = script0(&[], &[&[0x03, 1, (2 << 5) | 2][..], &set_byte(1, 0x40, 1), IDLE].concat());
    let e1_main = script0(&[], IDLE);
    let e1_s2 = (2, [&[0x24, 2, 0][..], &set_byte(1, 0x41, 1), &[0x00]].concat());
    let s = SynthScript::default().entity("a", vec![e0]).entity("b", vec![e1_main, e1_s2]);
    let mut rt = runtime(s);
    let mut done_at = None;
    for frame in 0..10 {
        rt.tick(None);
        if rt.mem.banks.get(0x41) == 1 && done_at.is_none() {
            done_at = Some(frame);
        }
        if rt.mem.banks.get(0x40) == 1 {
            assert!(done_at.is_some_and(|d| d < frame), "the requester waits until the request returned");
            break;
        }
    }
    assert_eq!(rt.mem.banks.get(0x41), 1);
    assert_eq!(rt.mem.banks.get(0x40), 1);
    assert_eq!(rt.entity_state(1).map(|(p, _)| p), Some(MAIN_PRIORITY), "b is back on its main loop");
}

#[test]
fn a_message_holds_its_script_until_the_window_is_closed_and_others_keep_running() {
    let talker = script0(&[], &[&[0x40, 0, 0][..], &set_byte(1, 0x50, 1), IDLE].concat());
    let counter = script0(&[], &[0x95, 0x02, 0x51, 0x12, 0x03]);
    let mut s = SynthScript::default().entity("talker", vec![talker]).entity("counter", vec![counter]);
    s.dialogs.push(dialog("Hello"));
    let mut rt = runtime(s);
    rt.tick(None);
    let events = rt.drain_events();
    assert!(matches!(events.as_slice(), [Event::Window { window: 0, dialog: 0, ask: None, .. }]), "{events:?}");
    for _ in 0..5 {
        rt.tick(None);
    }
    assert_eq!(rt.mem.banks.get(0x50), 0);
    assert_eq!(rt.mem.banks.get(0x51), 6, "the other entity ran every frame");
    assert!(rt.window_open());
    rt.close_window(0);
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x50), 1);
    assert!(!rt.window_open());
}

#[test]
fn ask_writes_the_answer_where_the_script_reads_it() {
    // ASK [banks: 0 / dst bank 1][window 1][dialog 0][first 1][last 3][var 0x60]
    let main = [&[0x48, 0x01, 1, 0, 1, 3, 0x60][..], IDLE].concat();
    let mut s = SynthScript::default().entity("q", vec![script0(&[], &main)]);
    s.dialogs.push(dialog("Q"));
    let mut rt = runtime(s);
    rt.tick(None);
    assert!(!rt.answer(1, 3), "outside the question's lines");
    assert!(rt.answer(1, 2));
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x60), 2);
}

#[test]
fn a_battle_waits_for_the_host_and_then_continues() {
    // BATTLE [banks 0][formation 300 u16], then set flag.
    let main = [&[0x70, 0x00, 0x2C, 0x01][..], &set_byte(1, 0x70, 1), IDLE].concat();
    let s = SynthScript::default().entity("b", vec![script0(&[], &main)]);
    let mut rt = runtime(s);
    rt.tick(None);
    assert_eq!(rt.command(), Some(&Command::Battle { formation: 300 }));
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x70), 0);
    rt.complete_command();
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x70), 1);
    assert_eq!(rt.command(), None);
}

#[test]
fn talking_to_a_model_runs_its_talk_script_at_priority_one() {
    // Entity "npc": CHAR 0, XYZI (100, 0, 0, triangle 0); talk script sets a flag.
    let init = [&[0xA1, 0x00][..], &[0xA5, 0x00, 0x00, 100, 0, 0, 0, 0, 0, 0, 0]].concat();
    let npc = vec![script0(&init, IDLE), (1, [&set_byte(1, 0x80, 1)[..], &[0x00]].concat())];
    let s = SynthScript::default().entity("npc", npc);
    let mut rt = runtime(s);
    rt.tick(None);
    // The player stands at (60, 0) facing +x (direction 64) with the model 40 units ahead.
    let player = PlayerInput { x: 60, y: 0, dir: 64, ok: true, ..Default::default() };
    rt.tick(Some(player));
    rt.tick(Some(PlayerInput { ok: false, ..player }));
    assert_eq!(rt.mem.banks.get(0x80), 1);
}

#[test]
fn crossing_a_line_runs_its_across_script() {
    // LINE from (0,-50,0) to (0,50,0); script 3 (across) sets a flag.
    let mut line = vec![0xD0];
    for v in [0i16, -50, 0, 0, 50, 0] {
        line.extend_from_slice(&v.to_le_bytes());
    }
    let ent = vec![script0(&line, IDLE), (3, [&set_byte(1, 0x90, 1)[..], &[0x00]].concat())];
    let s = SynthScript::default().entity("line", ent);
    let mut rt = runtime(s);
    for x in [-40, -20, -5, 5, 20, 40, 40] {
        rt.tick(Some(PlayerInput { x, y: 0, ..Default::default() }));
    }
    assert_eq!(rt.mem.banks.get(0x90), 1);
}

#[test]
fn talk_needs_the_model_in_front_and_within_reach() {
    let init = [&[0xA1, 0x00][..], &[0xA5, 0x00, 0x00, 100, 0, 0, 0, 0, 0, 0, 0]].concat();
    let npc = vec![script0(&init, IDLE), (1, [&set_byte(1, 0x80, 1)[..], &[0x00]].concat())];
    let mut rt = runtime(SynthScript::default().entity("npc", npc));
    rt.tick(None);
    // Facing away (direction 192, -x): nothing.
    rt.tick(Some(PlayerInput { x: 60, y: 0, dir: 192, ok: true, ..Default::default() }));
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x80), 0);
    // Too far: 80 + 34 = 114 is the reach.
    rt.tick(Some(PlayerInput { x: -20, y: 0, dir: 64, ok: true, ..Default::default() }));
    rt.tick(None);
    assert_eq!(rt.mem.banks.get(0x80), 0);
}

#[test]
fn directions_follow_the_original() {
    assert_eq!(direction_of(10, 0), 64);
    assert_eq!(direction_of(0, 10), 128);
    assert_eq!(direction_of(-10, 0), 192);
    assert_eq!(direction_of(0, -10), 0);
    for d in [0u8, 37, 64, 200] {
        let facing = crate::field_direction_to_facing(d);
        let (dx, dy) = ((f64::from(facing).cos() * 1000.0) as i64, (f64::from(facing).sin() * 1000.0) as i64);
        assert_eq!(direction_of(dx, dy), d, "a model facing direction {d} faces along the vector direction_of reads as {d}");
    }
}

/// Opcodes whose next PC is a jump target or a script switch rather than the instruction's end.
fn transfers_control(op: u8) -> bool {
    matches!(op, 0x00..=0x07 | 0x10..=0x19 | 0x30..=0x32 | 0xCB | 0xCC)
}

#[test]
fn every_implemented_opcode_advances_exactly_its_decoded_length_or_waits() {
    for op in 0u8..=255 {
        if crate::script::mnemonic(op).is_none() || !is_implemented(op) || transfers_control(op) || matches!(op, 0x28 | 0x0F) {
            continue;
        }
        let zeros = [0u8; 24];
        let mut probe = vec![op];
        probe.extend_from_slice(&zeros);
        let Ok(len) = crate::script::decode_at(&probe, 0, 0).map(|i| i.len) else { continue };
        let mut code = probe[..len].to_vec();
        code.extend_from_slice(IDLE);
        let s = SynthScript::default().entity("x", vec![script0(&[], &code)]);
        let mut rt = runtime(s);
        let (_, start) = rt.entity_state(0).unwrap();
        // One visit of the instruction alone: bound the entity to that single instruction by inspecting after
        // the first frame (an instruction that waits leaves the PC where it was).
        rt.tick(None);
        let (_, pc) = rt.entity_state(0).unwrap();
        let name = crate::script::mnemonic(op).unwrap();
        assert!(pc == start || pc >= start + len, "{name} (0x{op:02X}) moved the PC by {} but is {len} bytes long", pc - start);
        if pc != start {
            // It completed: the PC must have landed on the instruction after it (the idle loop's WAIT1 or JMPB).
            assert!(pc == start + len || pc == start + len + 1, "{name} (0x{op:02X}) moved the PC by {} but is {len} bytes long", pc - start);
        }
    }
}

#[test]
fn every_implemented_opcode_is_counted_as_executed() {
    // An unimplemented opcode is skipped and counted; an implemented one is not counted as unsupported.
    for op in 0u8..=255 {
        let Some(name) = crate::script::mnemonic(op) else { continue };
        if !is_implemented(op) || matches!(op, 0x00 | 0x28 | 0x0F) {
            continue;
        }
        // Zero operands of the instruction's documented length.
        let len = crate::script::decode_at(&[op, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, 0).map(|i| i.len).unwrap_or(1);
        let mut code = vec![op];
        code.resize(len, 0);
        let s = SynthScript::default().entity("x", vec![script0(&[], &[&code[..], IDLE].concat())]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert!(!rt.coverage().unsupported.contains_key(name), "{name} is implemented but was skipped");
    }
}

#[test]
fn a_background_trigger_opens_while_the_player_is_at_its_line_and_closes_when_it_leaves() {
    // One model entity that is the player's character (PC 0 with Cloud leading the party), so the player moves it.
    let init = [&[0xA1, 0x00][..], &[0xA0, 0x00]].concat();
    let s = SynthScript::default().entity("cl", vec![script0(&init, IDLE)]);
    let mut section = s;
    section.scale = 512;
    let mut banks = Banks::default();
    banks.set(PARTY_SLOTS, 0);
    let mut rt = FieldRuntime::new(&section.build(), banks).expect("runtime");
    rt.set_bg_triggers(vec![BgTrigger { a: (0, -50, 0), b: (0, 50, 0), group: 3, frame: 1, behavior: 2, sound: 1 }]);
    rt.init();
    let shown = |rt: &FieldRuntime| rt.stage_background().masks()[3] & 0b10 != 0;
    assert!(!shown(&rt), "a door its trigger opens starts closed");
    for x in [-200, -150, -100, -60, -30, -20] {
        rt.tick(Some(PlayerInput { x, y: 0, dir: 64, ..Default::default() }));
    }
    assert!(shown(&rt), "walking up to the line opens it");
    for x in [-60, -100, -150, -200] {
        rt.tick(Some(PlayerInput { x, y: 0, dir: 192, ..Default::default() }));
    }
    assert!(!shown(&rt), "walking away closes it again");
}
