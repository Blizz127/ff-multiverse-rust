//! Model offsets are presentation state, independent of logical field placement.

use super::{Beat, CastMember, Limits, Look, OffsetMotion, SceneDef, ScenePlay, SceneVars, ScriptRef, Spot, Stage, Step, Then, Trigger, Vm, Where};
use crate::field::FieldFile;
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::script::{decode_at, FieldScript};
use crate::story::StoryState;
use crate::testutil::FieldSpec;

fn code(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = parts.concat();
    bytes.push(0); // RET
    bytes
}

fn section(scripts: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    SynthScript { dialogs: vec![dialog("Offset ready.")], ..Default::default() }
        .entity("actor", scripts).build()
}

fn offset(kind: u8, xyz: [i16; 3], speed: i16) -> Vec<u8> {
    let mut bytes = vec![0xC3, 0, 0, kind];
    for value in xyz.into_iter().chain([speed]) { bytes.extend(value.to_le_bytes()); }
    bytes
}

fn place(xyz: [i16; 3], triangle: u16) -> Vec<u8> {
    let mut bytes = vec![0xA5, 0, 0];
    for value in xyz { bytes.extend(value.to_le_bytes()); }
    bytes.extend(triangle.to_le_bytes());
    bytes
}

fn setword(bank: u8, address: u8, value: i16) -> Vec<u8> {
    let mut bytes = vec![0x81, bank << 4, address];
    bytes.extend(value.to_le_bytes());
    bytes
}

fn start(slot: u8) -> Vec<Step> { vec![Step::Start(ScriptRef::new(0, slot))] }

fn vm() -> Vm { Vm::new(SceneVars::new(StoryState::new_game(), &[]), 7, Limits::default()) }

fn strict_vm() -> Vm {
    Vm::new(SceneVars::new(StoryState::new_game().into_extended().unwrap(), &[]), 7, Limits::default())
}

fn motions(beat: &Beat) -> Vec<(u32, u8, OffsetMotion)> {
    beat.stage.iter().filter_map(|event| match event.what {
        Stage::OffsetMove { entity, motion } => Some((event.at, entity, motion)),
        _ => None,
    }).collect()
}

fn offsets(beat: &Beat) -> Vec<(u32, u8, [i16; 3])> {
    beat.stage.iter().filter_map(|event| match event.what {
        Stage::Offset { entity, x, y, z } => Some((event.at, entity, [x, y, z])),
        _ => None,
    }).collect()
}

#[test]
fn instant_offsets_read_signed_literals_replace_and_reset_without_waiting() {
    let encoded = offset(0, [590, -38, -1316], -1);
    let decoded = decode_at(&encoded, 0, 0).unwrap();
    assert_eq!(decoded.literal("speed"), Some(-1), "the native handler reads a signed word even for an instant offset");
    let bytes = section(vec![(1, code(&[
        vec![0xC4], // OFSTW before any offset is already complete.
        encoded, vec![0xC4], vec![0x24, 7, 0],
        offset(0, [-32768, 32767, -7], 32767), vec![0xC4],
        offset(0, [0; 3], 0), vec![0xC4], vec![0x40, 0, 0],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    assert_eq!(vm.offset(0), [0; 3]);
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert_eq!(offsets(&beat), [(0, 0, [590, -38, -1316]), (7, 0, [-32768, 32767, -7]), (7, 0, [0; 3])]);
    assert!(matches!(beat.then, Then::Window(ref window) if window.at == 7));
    assert_eq!(vm.offset(0), [0; 3]);
    assert!(vm.writes().is_empty());
    assert_eq!(vm.stats().handled.get("OFST"), Some(&3));
    assert_eq!(vm.stats().handled.get("OFSTW"), Some(&4));
    assert_eq!(vm.stats().unresolved, 0);
    assert!(vm.stats().ignored.is_empty() && vm.stats().unknown.is_empty());
}

#[test]
fn every_banked_offset_operand_is_read_as_a_signed_word_including_unused_speed() {
    let mut banked = offset(0, [50, 52, 54], 56);
    banked[1] = 0x66;
    banked[2] = 0x66;
    let bytes = section(vec![(1, code(&[
        setword(6, 50, -590), setword(6, 52, 308), setword(6, 54, -1204), setword(6, 56, -300),
        banked, vec![0xC4],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert_eq!(offsets(&beat), [(0, 0, [-590, 308, -1204])]);
    assert!(matches!(beat.then, Then::End { at: 0, how: End::Finished }));
    assert_eq!(vm.writes().len(), 8, "only the four explicit SETWORD instructions write variables");

    // Even an unused speed operand is a real script read: an unknown persistent word
    // cannot disappear from the durable replay audit merely because type 0 is instant.
    let mut unknown_speed = offset(0, [1, 2, 3], 200);
    unknown_speed[2] = 0x04;
    let bytes = section(vec![(1, code(&[unknown_speed]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let story = StoryState::new_game().into_extended().unwrap();
    let mut vm = Vm::new(SceneVars::new(story, &[]), 7, Limits::default());
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert_eq!(offsets(&beat), [(0, 0, [1, 2, 3])]);
    let unknown = vm.vars().unknown_persistent_reads();
    assert!(unknown.contains(&(3, 200)) && unknown.contains(&(3, 201)), "both bytes of the signed speed word are audited: {unknown:?}");
}

#[test]
fn offsets_leave_logical_positions_and_getai_unchanged_and_survive_placement() {
    let bytes = section(vec![(1, code(&[
        place([-18, -459, -56], 53), offset(0, [590, -38, -1316], 0), vec![0x40, 0, 0],
        place([7, 8, 9], 4), vec![0xB9, 0x06, 0, 90], // GETAI actor 0 -> scratch word 90.
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let first = vm.resume(&script, None);
    assert_eq!(offsets(&first), [(0, 0, [590, -38, -1316])]);
    assert_eq!(vm.place_of(0), Some(Where { x: -18, y: -459, z: Some(-56), triangle: Some(53) }));
    assert!(vm.writes().is_empty(), "the offset never mutates story or scratch state");
    let last = vm.resume(&script, None);
    assert!(matches!(last.then, Then::End { how: End::Finished, .. }));
    assert_eq!(vm.place_of(0), Some(Where { x: 7, y: 8, z: Some(9), triangle: Some(4) }));
    assert_eq!(vm.offset(0), [590, -38, -1316], "XYZI does not clear a model offset");
    assert_eq!(vm.writes().get(&(6, 90)), Some(&4));
    assert_eq!(vm.writes().get(&(6, 91)), Some(&0));
    assert_eq!(vm.writes().len(), 2);
}

#[test]
fn quiet_setup_retains_only_the_last_offset_in_initial_presentation() {
    let bytes = section(vec![
        (3, code(&[place([7, 8, 9], 0), offset(0, [590, -38, -1316], 0), vec![0x24, 9, 0], offset(0, [636, 308, -1204], 0)])),
        (1, code(&[vec![0xC4], vec![0x40, 0, 0]])),
    ]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    assert_eq!(vm.offset(0), [636, 308, -1204]);
    assert!(!vm.stats().setup_incomplete);
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert!(beat.stage.is_empty(), "setup events are not replayed as timed instructions");
    assert!(matches!(beat.then, Then::Window(ref window) if window.at == 0));

    let field = FieldFile::parse(FieldSpec { overrides: vec![(0, bytes)], ..FieldSpec::default() }.build()).unwrap();
    let def = SceneDef {
        id: "offset-fixture".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 0, moments: 0..=u16::MAX },
        setup: vec![ScriptRef::new(0, 3)], given: vec![], root: start(1),
        cast: vec![CastMember { entity: 0, name: "actor".into(), look: Look::Appearance("ff7/cloud".into()), start: None }],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, resume_after_battle: None, presents_bit: None,
    };
    let mut play = ScenePlay::new(&field, &def, &StoryState::new_game(), 7).unwrap();
    assert_eq!(play.initial_stage_events(), [
        Stage::Place { entity: 0, x: 7, y: 8, z: Some(9), triangle: Some(0) },
        Stage::Offset { entity: 0, x: 636, y: 308, z: -1204 },
    ]);
    assert!(play.next(None).stage.is_empty());
}

#[test]
fn a_repeated_instant_offset_parks_without_losing_its_presentation_event() {
    let mut loop_code = offset(0, [590, -38, -1316], 0);
    loop_code.extend([0xC4, 0x24, 1, 0]);
    let back = loop_code.len() as u8;
    loop_code.extend([0x12, back]);
    let bytes = section(vec![(1, loop_code)]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }), "a stable offset is not endlessly changing work: {:?}", beat.then);
    let events = offsets(&beat);
    assert!(!events.is_empty() && events.len() <= 3);
    assert!(events.iter().all(|(_, entity, xyz)| *entity == 0 && *xyz == [590, -38, -1316]));
    assert!(vm.stats().parked > 0);
}

#[test]
fn unsupported_offset_modes_stop_before_guessed_motion_or_wait_completion() {
    for kind in [2, 3, 255] {
        let bytes = section(vec![(1, code(&[
            offset(0, [1, 2, 3], 0), offset(kind, [590, -38, -1316], 30), vec![0xC4], vec![0x40, 0, 0],
        ]))]);
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = vm();
        vm.begin(start(1));
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::End { at: 0, how: End::Budget }), "mode {kind}");
        assert_eq!(offsets(&beat), [(0, 0, [1, 2, 3])]);
        assert_eq!(vm.offset(0), [1, 2, 3]);
        assert_eq!(vm.stats().ignored.get("OFST"), Some(&1));
        assert_eq!(vm.stats().unresolved, 1);
        assert!(!vm.stats().handled.contains_key("OFSTW"));
        assert!(vm.writes().is_empty());
    }
}

#[test]
fn a_linear_offset_starts_without_blocking_and_ofstw_waits_through_step_fifty() {
    let target = [-202, 184, -40];
    let bytes = section(vec![(1, code(&[
        place([7, 8, 9], 4), offset(1, target, 50),
        vec![0xA4, 0], // VISI after OFST, before OFSTW: still the issue frame.
        vec![0xC4], vec![0x40, 0, 0],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = strict_vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert_eq!(motions(&beat), [(0, 0, OffsetMotion::new([0; 3], target, 50).unwrap())]);
    assert!(beat.stage.iter().any(|event| event.at == 0 && event.what == Stage::Show { entity: 0, visible: false }),
        "OFST does not delay the next instruction");
    assert!(matches!(beat.then, Then::Window(ref window) if window.at == 51), "{:?}", beat.then);
    assert_eq!(vm.offset(0), target);
    assert_eq!(vm.place_of(0), Some(Where { x: 7, y: 8, z: Some(9), triangle: Some(4) }));
    assert!(offsets(&beat).is_empty(), "the endpoint is not an invented instant opcode");
    assert!(vm.writes().is_empty());
    assert_eq!(vm.stats().unresolved, 0);
    assert!(matches!(vm.resume(&script, None).then, Then::End { at: 0, how: End::Finished }));
}

#[test]
fn a_replacement_after_wait_ten_starts_at_the_preceding_model_update_step_nine() {
    let bytes = section(vec![(1, code(&[
        offset(1, [100, -100, 50], 50), vec![0x24, 10, 0],
        offset(1, [-20, 60, 0], 5), vec![0xC4], vec![0x40, 0, 0],
    ]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = strict_vm();
    vm.begin(start(1));
    let beat = vm.resume(&script, None);
    assert_eq!(motions(&beat), [
        (0, 0, OffsetMotion::new([0; 3], [100, -100, 50], 50).unwrap()),
        (10, 0, OffsetMotion::new([18, -18, 9], [-20, 60, 0], 5).unwrap()),
    ]);
    assert!(matches!(beat.then, Then::Window(ref window) if window.at == 16), "{:?}", beat.then);
    assert_eq!(vm.offset(0), [-20, 60, 0]);
    assert_eq!(vm.stats().unresolved, 0);
}

#[test]
fn strict_windows_and_terminals_refuse_an_unfinished_offset_but_legacy_previews_keep_it() {
    for window in [false, true] {
        for elapsed in [0u16, 50] {
            let bytes = section(vec![(1, code(&[
                offset(1, [-202, 0, 0], 50),
                vec![0x24, elapsed as u8, (elapsed >> 8) as u8],
                if window { vec![0x40, 0, 0] } else { vec![] },
            ]))]);
            let script = FieldScript::parse(&bytes).unwrap();
            let mut strict = strict_vm();
            strict.begin(start(1));
            let beat = strict.resume(&script, None);
            assert!(matches!(beat.then, Then::End { at, how: End::Budget } if at == u32::from(elapsed)),
                "window={window}, elapsed={elapsed}: {:?}", beat.then);
            assert_eq!(strict.stats().unresolved, 1);
            assert_eq!(motions(&beat).len(), 1);

            let mut legacy = vm();
            legacy.begin(start(1));
            let beat = legacy.resume(&script, None);
            if window {
                assert!(matches!(beat.then, Then::Window(ref window) if window.at == u32::from(elapsed)));
            } else {
                assert!(matches!(beat.then, Then::End { at, how: End::Finished } if at == u32::from(elapsed)));
            }
            assert_eq!(legacy.stats().unresolved, 0);
        }
    }
}

#[test]
fn a_completed_offset_allows_strict_boundaries_without_an_explicit_ofstw() {
    for window in [false, true] {
        let bytes = section(vec![(1, code(&[
            offset(1, [-202, 184, 0], 50), vec![0x24, 51, 0],
            if window { vec![0x40, 0, 0] } else { vec![] },
        ]))]);
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = strict_vm();
        vm.begin(start(1));
        let beat = vm.resume(&script, None);
        if window {
            assert!(matches!(beat.then, Then::Window(ref window) if window.at == 51));
        } else {
            assert!(matches!(beat.then, Then::End { at: 51, how: End::Finished }));
        }
        assert_eq!(vm.offset(0), [-202, 184, 0]);
        assert_eq!(vm.stats().unresolved, 0);
    }
}

#[test]
fn linear_offsets_accept_positive_signed_durations_and_reject_zero_or_negative_ones() {
    for duration in [1i16, 50, i16::MAX] {
        let bytes = section(vec![(1, code(&[offset(1, [1, -2, 3], duration), vec![0xC4], vec![0x40, 0, 0]]))]);
        let script = FieldScript::parse(&bytes).unwrap();
        // The longest supported motion exceeds the normal ten-minute scene budget.
        let mut vm = Vm::new(SceneVars::new(StoryState::new_game().into_extended().unwrap(), &[]), 7,
            Limits { frames: 40_000, ..Limits::default() });
        vm.begin(start(1));
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::Window(ref window) if window.at == duration as u32 + 1),
            "duration={duration}: {:?}", beat.then);
        assert_eq!(vm.offset(0), [1, -2, 3]);
        assert_eq!(vm.stats().unresolved, 0);
    }
    for duration in [i16::MIN, -1, 0] {
        let bytes = section(vec![(1, code(&[
            offset(0, [7, 8, 9], 0), offset(1, [1, -2, 3], duration), vec![0xC4], vec![0x40, 0, 0],
        ]))]);
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = strict_vm();
        vm.begin(start(1));
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::End { at: 0, how: End::Budget }), "duration={duration}");
        assert!(motions(&beat).is_empty());
        assert_eq!(vm.offset(0), [7, 8, 9]);
        assert_eq!(vm.stats().ignored.get("OFST"), Some(&1));
        assert_eq!(vm.stats().unresolved, 1);
        assert!(!vm.stats().handled.contains_key("OFSTW"));
    }
}

#[test]
fn quiet_setup_finishes_a_linear_offset_and_snapshots_its_fixed_result() {
    let target = [100, -100, 50];
    let bytes = section(vec![
        (3, code(&[place([7, 8, 9], 0), offset(1, target, 5), vec![0xC4]])),
        (1, code(&[vec![0x24, 100, 0], vec![0x40, 0, 0]])),
    ]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = strict_vm();
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    assert!(!vm.stats().setup_incomplete);
    assert_eq!(vm.offset(0), target);
    vm.begin(start(1));
    assert_eq!(vm.offset(0), target, "resetting scene time does not restart a completed setup motion");
    let beat = vm.resume(&script, None);
    assert!(beat.stage.is_empty());
    assert!(matches!(beat.then, Then::Window(ref window) if window.at == 100));
    assert_eq!(vm.offset(0), target);

    let field = FieldFile::parse(FieldSpec { overrides: vec![(0, bytes)], ..FieldSpec::default() }.build()).unwrap();
    let def = SceneDef {
        id: "offset-setup-fixture".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 0, moments: 0..=u16::MAX },
        setup: vec![ScriptRef::new(0, 3)], given: vec![], root: start(1),
        cast: vec![CastMember { entity: 0, name: "actor".into(), look: Look::Appearance("ff7/cloud".into()), start: None }],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, resume_after_battle: None, presents_bit: None,
    };
    let mut play = ScenePlay::new(&field, &def, &StoryState::new_game().into_extended().unwrap(), 7).unwrap();
    assert!(!play.setup_stats().setup_incomplete);
    assert_eq!(play.initial_stage_events(), [
        Stage::Place { entity: 0, x: 7, y: 8, z: Some(9), triangle: Some(0) },
        Stage::Offset { entity: 0, x: 100, y: -100, z: 50 },
    ]);
    assert!(play.next(None).stage.is_empty(), "quiet setup is not replayed as a new timed motion");
}

#[test]
fn quiet_setup_flags_a_motion_that_returns_before_its_model_updates_finish() {
    let bytes = section(vec![(3, code(&[offset(1, [100, -100, 50], 50), vec![0x24, 10, 0]]))]);
    let script = FieldScript::parse(&bytes).unwrap();
    let mut vm = strict_vm();
    vm.setup(&script, &[ScriptRef::new(0, 3)]);
    assert!(vm.stats().setup_incomplete, "RET cannot silently discard the rest of an animated offset");
    assert_eq!(vm.offset(0), [18, -18, 9], "the incomplete setup snapshot is the last native model update");
}

#[test]
fn an_ofstw_waiter_rechecks_motion_replaced_or_cancelled_by_another_script() {
    for (replacement, expected_at, target) in [
        (offset(0, [7, 8, 9], 0), 10, [7, 8, 9]),
        (offset(1, [-20, 30, 40], 80), 91, [-20, 30, 40]),
    ] {
        let bytes = SynthScript { dialogs: vec![dialog("Offset ready.")], ..Default::default() }
            .entity("actor", vec![
                (1, code(&[offset(1, [100, 0, 0], 50), vec![0xC4], vec![0x40, 0, 0]])),
                (2, code(&[replacement])),
            ])
            .entity("replacer", vec![(1, code(&[
                vec![0x24, 10, 0], vec![0x03, 0, 0x22], // REQEW actor, priority1, slot2; waiter occupies priority7.
            ]))]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = strict_vm();
        vm.begin(vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))]);
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::Window(ref window) if window.at == expected_at),
            "the waiter must use the replacement deadline, not the original frame51: {:?}", beat.then);
        assert_eq!(vm.offset(0), target);
        assert_eq!(vm.stats().queued, 0);
        assert_eq!(vm.stats().unresolved, 0);
        if expected_at == 10 {
            assert_eq!(offsets(&beat), [(10, 0, target)]);
        } else {
            assert_eq!(motions(&beat)[1], (10, 0, OffsetMotion::new([18, 0, 0], target, 80).unwrap()));
        }
    }
}

#[test]
fn a_higher_priority_cancellation_and_restart_cannot_release_the_offset_waiter_between_opcodes() {
    for (hold, expected_at) in [(0u8, 91), (100, 110)] {
        let target = [-20, 30, 40];
        let bytes = SynthScript { dialogs: vec![dialog("Offset ready.")], ..Default::default() }
            .entity("actor", vec![
                (1, code(&[offset(1, [100, 0, 0], 50), vec![0xC4], vec![0x40, 0, 0]])),
                (2, code(&[
                    offset(0, [7, 8, 9], 0), offset(1, target, 80), vec![0x24, hold, 0],
                ])),
            ])
            .entity("replacer", vec![(1, code(&[
                vec![0x24, 10, 0], vec![0x03, 0, 0x22], // REQEW actor priority1, slot2.
            ]))]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = strict_vm();
        vm.begin(vec![Step::Start(ScriptRef::new(0, 1)), Step::Start(ScriptRef::new(1, 1))]);
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::Window(ref window) if window.at == expected_at),
            "hold={hold}: the waiter needs both the replacement endpoint and its priority slot back: {:?}", beat.then);
        assert_eq!(offsets(&beat), [(10, 0, [7, 8, 9])]);
        assert_eq!(motions(&beat), [
            (0, 0, OffsetMotion::new([0; 3], [100, 0, 0], 50).unwrap()),
            (10, 0, OffsetMotion::new([7, 8, 9], target, 80).unwrap()),
        ]);
        assert_eq!(vm.offset(0), target);
        assert_eq!(vm.stats().unresolved, 0);
        assert_eq!(vm.stats().queued, 0);
    }
}

#[test]
fn rejecting_a_terminal_during_an_offset_clears_its_battle_or_transfer_metadata() {
    let battle = vec![0x72, 8, 0, 0x70, 0, 0x2C, 1]; // BTLMD8; BATTLE300.
    let transfer = vec![0x60, 139, 0, 39, 0, 0xBB, 0xFE, 49, 0, 28]; // MAPJUMP139,39,-325,49,28.
    for (name, terminal) in [("battle", battle), ("transfer", transfer)] {
        let bytes = section(vec![(1, code(&[offset(1, [100, 0, 0], 50), terminal]))]);
        let script = FieldScript::parse(&bytes).unwrap();
        let mut vm = strict_vm();
        vm.begin(start(1));
        let beat = vm.resume(&script, None);
        assert!(matches!(beat.then, Then::End { at: 0, how: End::Budget }), "{name}: {:?}", beat.then);
        assert_eq!(vm.boundary(), Some(super::SceneBoundary::End(End::Budget)),
            "{name}: a rejected terminal cannot retain an actionable battle checkpoint");
        assert_eq!(vm.destination(), None, "{name}: a rejected transfer cannot retain a landing");
        assert_eq!(vm.stats().unresolved, 1);
        assert!(vm.is_over());
        assert!(matches!(vm.resume(&script, None).then, Then::End { at: 0, how: End::Budget }));
        assert_eq!(vm.boundary(), Some(super::SceneBoundary::End(End::Budget)));
    }
}
