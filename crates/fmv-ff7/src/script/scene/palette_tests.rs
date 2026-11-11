//! Palette instruction playback from synthetic scripts, including intermediate loop states.

use super::{Beat, Given, Limits, SceneVars, ScriptRef, Stage, Step, Then, Vm};
use crate::background::{BackgroundOp, PaletteOp, PaletteState};
use crate::field::Palette;
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::script::FieldScript;
use crate::story::StoryState;

fn code(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = parts.concat();
    bytes.push(0x00);
    bytes
}

fn section(scripts: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    SynthScript { dialogs: vec![dialog("First."), dialog("Second.")], ..Default::default() }
        .entity("director", scripts).build()
}

fn vm(given: &[Given]) -> Vm {
    Vm::new(SceneVars::new(StoryState::new_game(), given), 7, Limits::default())
}

fn start() -> Vec<Step> {
    vec![Step::Start(ScriptRef::new(0, 1))]
}

fn palettes(beat: &Beat) -> Vec<(u32, PaletteOp)> {
    beat.stage.iter().filter_map(|event| match event.what {
        Stage::Background(BackgroundOp::Palette(op)) => Some((event.at, op)),
        _ => None,
    }).collect()
}

fn live_palette(color: u16) -> Palette {
    let mut bytes = vec![0; 12];
    bytes[8..10].copy_from_slice(&256u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    for _ in 0..256 { bytes.extend_from_slice(&color.to_le_bytes()); }
    Palette::parse(&bytes).unwrap()
}

#[test]
fn literal_palette_operations_keep_resolved_counts_rgb_order_and_window_relative_times() {
    let bytes = section(vec![(1, code(&[
        &[0xE5, 0, 0, 0, 255], &[0x24, 2, 0], &[0xE7, 0, 0, 16, 0], &[0x40, 0, 0],
        &[0x24, 3, 0], &[0xEA, 0, 0, 0, 0, 16, 10, 20, 30, 255],
        &[0xE6, 0, 16, 1, 255], &[0x40, 0, 1],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm(&[]);
    vm.begin(start());
    let first = vm.resume(&script, None);
    assert_eq!(palettes(&first), [
        (0, PaletteOp::Store { palette: 0, position: 0, count: 256 }),
        (2, PaletteOp::Copy { source: 0, destination: 16, count: 1 }),
    ]);
    assert!(matches!(first.then, Then::Window(ref w) if w.at == 2));
    let second = vm.resume(&script, None);
    assert_eq!(palettes(&second), [
        (3, PaletteOp::Multiply { source: 0, destination: 16, red: 30, green: 20, blue: 10, count: 256 }),
        (3, PaletteOp::Load { position: 16, palette: 1, count: 256 }),
    ]);
    assert!(matches!(second.then, Then::Window(ref w) if w.at == 3));
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    assert!(vm.writes().is_empty(), "literal palette work cannot become durable story writes");
    assert!(vm.stats().ignored.is_empty() && vm.stats().unknown.is_empty());
    assert_eq!(vm.stats().unresolved, 0);
    for name in ["STPAL", "CPPAL", "MPPAL2", "LDPAL"] {
        assert_eq!(vm.stats().handled.get(name), Some(&1), "{name}");
    }
}

#[test]
fn mixed_palette_banks_resolve_each_operand_without_swapping_source_destination_or_rgb() {
    let given = [
        Given { bank: 3, address: 40, value: 2 }, Given { bank: 5, address: 40, value: 16 },
        Given { bank: 5, address: 41, value: 7 }, Given { bank: 3, address: 42, value: 200 },
    ];
    let bytes = section(vec![(1, code(&[
        &[0xE5, 0x35, 40, 40, 255], // palette bank 3 / scratch position bank 5
        &[0xE6, 0x53, 40, 40, 255], // scratch position bank 5 / palette bank 3
        &[0xE7, 0x50, 40, 32, 0], // source bank 5 / destination literal
        &[0xEA, 0x53, 0x50, 0x30, 40, 40, 41, 64, 42, 255],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm(&given);
    vm.begin(start());
    let beat = vm.resume(&script, None);
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
    assert_eq!(palettes(&beat), [
        (0, PaletteOp::Store { palette: 2, position: 16, count: 256 }),
        (0, PaletteOp::Load { position: 16, palette: 2, count: 256 }),
        (0, PaletteOp::Copy { source: 16, destination: 32, count: 1 }),
        (0, PaletteOp::Multiply { source: 16, destination: 2, red: 200, green: 64, blue: 7, count: 256 }),
    ]);
    assert!(vm.writes().is_empty());
    assert!(vm.stats().ignored.is_empty() && vm.stats().unknown.is_empty());
    assert_eq!(vm.stats().unresolved, 0);
}

#[test]
fn resolved_palette_ranges_remain_exact_for_the_palette_state_to_validate_atomically() {
    let bytes = section(vec![(1, code(&[
        &[0xE5, 0, 255, 0, 255], &[0xE6, 0, 255, 0, 255],
        &[0xE7, 0, 0, 63, 255], &[0xEA, 0, 0, 0, 255, 0, 64, 64, 64, 255],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm(&[]);
    vm.begin(start());
    let beat = vm.resume(&script, None);
    assert_eq!(palettes(&beat), [
        (0, PaletteOp::Store { palette: 255, position: 0, count: 256 }),
        (0, PaletteOp::Load { position: 255, palette: 0, count: 256 }),
        (0, PaletteOp::Copy { source: 0, destination: 63, count: 256 }),
        (0, PaletteOp::Multiply { source: 255, destination: 0, red: 64, green: 64, blue: 64, count: 256 }),
    ], "the VM must not clamp a bad source/destination to a valid palette address");
    let mut state = PaletteState::new(live_palette(0x1234));
    state.apply(PaletteOp::Store { palette: 0, position: 0, count: 256 }).unwrap();
    for (_, op) in palettes(&beat) {
        let before = state.clone();
        assert!(state.apply(op).is_err(), "{op:?}");
        assert_eq!(state, before);
    }
    assert!(vm.writes().is_empty());
}

#[test]
fn quiet_setup_and_a_legacy_battle_prefix_keep_palette_work_once_while_the_tail_stays_timed() {
    let bytes = section(vec![
        (3, code(&[&[0xE5, 0, 0, 0, 255]])),
        (1, code(&[
            &[0xE7, 0, 0, 16, 255], &[0x40, 0, 0], &[0x24, 2, 0],
            &[0xEA, 0, 0, 0, 0, 16, 32, 32, 32, 255], &[0xE6, 0, 16, 0, 255],
            &[0x70, 0, 0x2F, 0x01], // BATTLE 303
            &[0x24, 3, 0], &[0xEA, 0, 0, 0, 0, 16, 64, 64, 64, 255],
            &[0xE6, 0, 16, 0, 255], &[0x40, 0, 1],
        ])),
    ]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm(&[]);
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    let store = BackgroundOp::Palette(PaletteOp::Store { palette: 0, position: 0, count: 256 });
    assert_eq!(vm.initial_background_ops(), [store]);
    vm.begin(start());
    assert_eq!(vm.initial_background_ops(), [store], "begin retains quiet palette setup");
    assert!(vm.seek_after_battle(&script, 303));
    let initial = [
        store,
        BackgroundOp::Palette(PaletteOp::Copy { source: 0, destination: 16, count: 256 }),
        BackgroundOp::Palette(PaletteOp::Multiply { source: 0, destination: 16, red: 32, green: 32, blue: 32, count: 256 }),
        BackgroundOp::Palette(PaletteOp::Load { position: 16, palette: 0, count: 256 }),
    ];
    assert_eq!(vm.initial_background_ops(), initial, "setup and both skipped prefix beats appear once, in order");
    let tail = vm.resume(&script, None);
    assert_eq!(palettes(&tail), [
        (3, PaletteOp::Multiply { source: 0, destination: 16, red: 64, green: 64, blue: 64, count: 256 }),
        (3, PaletteOp::Load { position: 16, palette: 0, count: 256 }),
    ]);
    assert!(matches!(tail.then, Then::Window(ref w) if w.at == 3));
    assert_eq!(vm.initial_background_ops(), initial, "timed tail commands do not enter the retained prefix");
    let end = vm.resume(&script, None);
    assert!(end.stage.is_empty());
    assert!(matches!(end.then, Then::End { how: End::Finished, .. }));
    assert!(vm.writes().is_empty());
}

#[test]
fn a_finite_variable_brightness_loop_keeps_each_multiply_and_load_at_its_authored_wait() {
    let mut body = code(&[
        &[0x80, 0x50, 60, 0], // SETBYTE scratch variable 60 = 0
        &[0xE5, 0, 0, 0, 255],
    ]);
    body.pop(); // append the loop before the final RET
    let loop_start = body.len();
    body.extend(code(&[
        &[0xEA, 0x00, 0x55, 0x50, 0, 16, 60, 60, 60, 255], // all three scales read variable 60
        &[0xE6, 0, 16, 0, 255], &[0x24, 2, 0],
        &[0x85, 0x50, 60, 32], // PLUS 32
        &[0x14, 0x50, 60, 96, 3, 3], // IF variable < 96; otherwise skip the two-byte JMPB
    ]));
    body.pop();
    let backwards = u8::try_from(body.len() - loop_start).unwrap();
    body.extend([0x12, backwards]); // backward offsets are from the JMPB opcode
    body.extend([0x40, 0, 0, 0x00]);
    let bytes = section(vec![(1, body)]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm(&[]);
    vm.begin(start());
    let beat = vm.resume(&script, None);
    let Then::Window(ref window) = beat.then else { panic!("the finite brightness loop did not reach its window: {:?}", beat.then) };
    let mut expected = vec![PaletteOp::Store { palette: 0, position: 0, count: 256 }];
    for scale in [0, 32, 64] {
        expected.push(PaletteOp::Multiply { source: 0, destination: 16, red: scale, green: scale, blue: scale, count: 256 });
        expected.push(PaletteOp::Load { position: 16, palette: 0, count: 256 });
    }
    assert_eq!(palettes(&beat).iter().map(|(_, op)| *op).collect::<Vec<_>>(), expected,
        "the loop preserves every intermediate multiply/load pair in authored order");
    let mut state = PaletteState::new(live_palette(0x4210));
    let mut shown = Vec::new();
    for (at, op) in palettes(&beat) {
        state.apply(op).unwrap();
        if matches!(op, PaletteOp::Load { .. }) { shown.push((at, state.palette().raw(0, 1).unwrap())); }
    }
    assert_eq!(shown.iter().map(|(_, color)| *color).collect::<Vec<_>>(), [0x8000, 0x2108, 0x4210],
        "the intermediate load really displays half brightness");
    assert!(shown.windows(2).all(|pair| pair[1].0 >= pair[0].0 + 2),
        "each load follows at least the authored WAIT2; backward-jump scheduling may add time: {shown:?}");
    assert!(window.at >= shown.last().unwrap().0 + 2, "the final WAIT2 completes before the window opens");
    assert_eq!(vm.stats().handled.get("MPPAL2"), Some(&3));
    assert_eq!(vm.stats().handled.get("LDPAL"), Some(&3));
    assert_eq!(vm.stats().parked, 0, "a progressing finite loop must not be mistaken for idle work");
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
}
