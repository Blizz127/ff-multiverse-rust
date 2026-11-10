//! Unit tests of the decoder, the disassembler and the init-script analysis (synthetic bytes).

use super::synth::SynthScript;
use super::*;
use crate::field::{Vertex, Walkmesh};

fn xyzi(x: i16, y: i16, z: i16, tri: u16) -> Vec<u8> {
    let mut v = vec![0xA5, 0x00, 0x00];
    for w in [x as u16, y as u16, z as u16, tri] {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

#[test]
fn decodes_field_object_instructions() {
    let d = |bytes: &[u8]| decode_at(bytes, 0, 0).unwrap();
    let i = d(&[0xA1, 0x07]);
    assert_eq!((i.mnemonic, i.len, i.literal("char_id")), ("CHAR", 2, Some(7)));
    let i = d(&[0xA0, 0x00]);
    assert_eq!((i.mnemonic, i.literal("character")), ("PC", Some(0)));
    let i = d(&[0xA4, 0x01]);
    assert_eq!((i.mnemonic, i.literal("visible")), ("VISI", Some(1)));
    let i = d(&[0xC7, 0x01]);
    assert_eq!((i.mnemonic, i.literal("solid")), ("SOLID", Some(1)));
    let i = d(&[0xB3, 0x00, 0x80]);
    assert_eq!((i.mnemonic, i.len, i.literal("direction")), ("DIR", 3, Some(0x80)));
    let i = d(&[0xC5, 0x00, 0x30]);
    assert_eq!((i.mnemonic, i.literal("radius")), ("TALKR", Some(0x30)));
    let i = d(&xyzi(-300, 28_000, 400, 17));
    assert_eq!(i.mnemonic, "XYZI");
    assert_eq!(i.len, 11);
    assert_eq!((i.literal("x"), i.literal("y"), i.literal("z"), i.literal("triangle")), (Some(-300), Some(28_000), Some(400), Some(17)));
    assert!(!i.stray_banks);
    let i = d(&[0xA6, 0x00, 0x00, 0x10, 0x00, 0x20, 0x00, 0x03, 0x00]);
    assert_eq!((i.mnemonic, i.len, i.literal("x"), i.literal("y"), i.literal("triangle")), ("XYI", 9, Some(16), Some(32), Some(3)));
    assert!(i.operand("z").is_none());
    let i = d(&[0xA7, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0xFF, 0xFF]);
    assert_eq!((i.mnemonic, i.len, i.literal("z")), ("XYZ", 9, Some(-1)));
    let i = d(&[0x60, 0x75, 0x00, 0x10, 0x00, 0x20, 0x00, 0x05, 0x00, 0x80]);
    assert_eq!((i.mnemonic, i.len, i.literal("field"), i.literal("direction")), ("MAPJUMP", 10, Some(117), Some(0x80)));
    // REQ splits its priority/script byte
    let i = d(&[0x01, 0x04, 0x61]);
    assert_eq!((i.literal("entity"), i.literal("priority"), i.literal("script")), (Some(4), Some(3), Some(1)));
}

#[test]
fn tura_reads_steps_before_path_and_preserves_instruction_boundaries() {
    let code = [
        0xAB, 3, 10, 2, // Target at +1, steps at +2, path at +3.
        0xB4, 0, 28, 2, 10, 2,
        0xB5, 0, 64, 1, 9, 1, // B5 is six bytes; its target interpretation is outside this correction.
        0x00,
    ];
    let decoded = disassemble_slice(&code, 0);
    assert!(decoded.is_complete());
    let instructions = &decoded.instructions;
    assert_eq!(instructions.iter().map(|i| (i.offset, i.len, i.opcode)).collect::<Vec<_>>(),
        [(0, 4, 0xAB), (4, 6, 0xB4), (10, 6, 0xB5), (16, 1, 0x00)]);
    let tura = &instructions[0];
    assert_eq!((tura.mnemonic, tura.literal("entity"), tura.literal("speed"), tura.literal("direction")),
        ("TURA", Some(3), Some(10), Some(2)));
    assert!(tura.operands.iter().all(|operand| operand.bank == 0));
    let turngen = &instructions[1];
    assert_eq!((turngen.literal("direction"), turngen.literal("turns"), turngen.literal("speed"), turngen.literal("unknown")),
        (Some(28), Some(2), Some(10), Some(2)));
    assert!(!turngen.stray_banks);
}

#[test]
fn bank_nibbles_turn_operands_into_variables() {
    // XYZI with x in bank 1 and the triangle in bank 3: the stored numbers are variable addresses
    let mut bytes = xyzi(5, 6, 7, 9);
    bytes[1] = 0x10; // B1 = 1 (x), B2 = 0 (y)
    bytes[2] = 0x03; // B3 = 0 (z), B4 = 3 (triangle)
    let i = decode_at(&bytes, 0, 0).unwrap();
    assert_eq!(i.operand("x").map(|o| (o.bank, o.value)), Some((1, 5)));
    assert_eq!(i.literal("x"), None);
    assert_eq!(i.literal("y"), Some(6));
    assert_eq!(i.operand("triangle").map(|o| (o.bank, o.value)), Some((3, 9)));
    assert!(!i.stray_banks);
    // a bank nibble that no operand of the opcode uses is flagged (DIR only uses nibble 1)
    let stray = decode_at(&[0xB3, 0x70, 0x80], 0, 0).unwrap();
    assert!(stray.stray_banks);
    assert_eq!(stray.operand("direction").map(|o| o.bank), Some(0));
    let ok = decode_at(&[0xB3, 0x03, 0x80], 0, 0).unwrap();
    assert!(!ok.stray_banks);
    assert_eq!(ok.operand("direction").map(|o| o.bank), Some(3));
}

#[test]
fn jump_targets_use_the_documented_bases() {
    // forward jumps count from the jump operand byte: JMPF at 0 with distance 3 -> 1 + 3
    let i = decode_at(&[0x10, 0x03, 0x00, 0x00], 0, 0).unwrap();
    assert_eq!(i.jump_target, Some(4));
    // IFUB at 0x10: the jump operand is its 6th byte (offset 5), distance 6 -> 0x10 + 5 + 6
    let ifub = [0x14, 0x00, 0x05, 0x01, 0x00, 0x06];
    let i = decode_at(&ifub, 0, 0x10).unwrap();
    assert_eq!((i.len, i.offset, i.jump_target), (6, 0x10, Some(0x10 + 5 + 6)));
    // the long forms take a u16 distance
    let ifubl = [0x15, 0x00, 0x05, 0x01, 0x00, 0x00, 0x01];
    assert_eq!(decode_at(&ifubl, 0, 0).unwrap().jump_target, Some(5 + 0x100));
    let jmpfl = [0x11, 0x00, 0x02];
    assert_eq!(decode_at(&jmpfl, 0, 0).unwrap().jump_target, Some(1 + 0x200));
    // backward jumps count from the opcode byte: JMPB at 5 with distance 5 -> 0
    let code = [0xC7, 0x01, 0x24, 0x70, 0x00, 0x12, 0x05];
    let d = disassemble_slice(&code, 0);
    assert!(d.is_complete());
    assert_eq!(d.instructions.len(), 3);
    assert_eq!(d.instructions[2].jump_target, Some(0));
    let jmpbl = [0x13, 0x03, 0x00];
    assert_eq!(decode_at(&jmpbl, 0, 10).unwrap().jump_target, Some(10 - 3));
    // a backward jump before the section start has no target and is reported as a bad jump
    let d = disassemble_slice(&[0x12, 0x09, 0x00], 2);
    assert_eq!(d.instructions[0].jump_target, None);
    assert_eq!(d.bad_jumps(5, false).len(), 1);
}

#[test]
fn bad_jumps_are_those_that_miss_an_instruction_start() {
    // IFUB skipping the 2-byte VISI lands on RET
    let mut code = vec![0x14, 0x00, 0x01, 0x02, 0x00, 0x03]; // IFUB: target = 5 + 3 = 8
    code.extend_from_slice(&[0xA4, 0x01]); // VISI at 6..8
    code.push(0x00); // RET at 8
    let d = disassemble_slice(&code, 0);
    assert!(d.is_complete());
    assert_eq!(d.instructions[0].jump_target, Some(8));
    assert!(d.bad_jumps(code.len(), false).is_empty());
    code[5] = 0x02; // now the target is 7: the middle of VISI
    let d = disassemble_slice(&code, 0);
    assert_eq!(d.bad_jumps(code.len(), false), vec![(0, 7)]);
    // a target exactly at the end of the script is only accepted on request
    code[5] = 0x04; // 5 + 4 = 9 = end
    let d = disassemble_slice(&code, 0);
    assert_eq!(d.bad_jumps(code.len(), false), vec![(0, 9)]);
    assert!(d.bad_jumps(code.len(), true).is_empty());
}

#[test]
fn walking_stops_at_unknown_opcodes_and_truncated_instructions() {
    let d = disassemble_slice(&[0xA4, 0x01, 0x0C, 0x00], 0x100);
    assert_eq!(d.instructions.len(), 1);
    assert_eq!(d.error, Some(DecodeError::UnknownOpcode { offset: 0x102, opcode: 0x0C }));
    assert_eq!(d.end_offset(0x100), 0x102);
    // XYZI cut off after 5 bytes
    let d = disassemble_slice(&xyzi(1, 2, 3, 4)[..5], 0);
    assert!(d.instructions.is_empty());
    assert!(matches!(d.error, Some(DecodeError::Truncated { offset: 0, opcode: 0xA5, needed: 11, available: 5 })));
    assert_eq!(d.error.as_ref().map(DecodeError::offset), Some(0));
    // the strict API reports it with the entity and script
    let built = SynthScript::default().entity("e", vec![(0, vec![0xA4, 0x01, 0x0C])]).build();
    let s = FieldScript::parse(&built).unwrap();
    assert!(matches!(
        s.disassemble(0, 0),
        Err(ScriptError::Decode { entity: 0, script: 0, source: DecodeError::UnknownOpcode { opcode: 0x0C, .. } })
    ));
    let partial = s.disassemble_partial(0, 0).unwrap();
    assert_eq!(partial.instructions.len(), 1);
}

#[test]
fn kawai_length_byte_is_the_whole_instruction() {
    // KAWAI, L = 6, sub-opcode 1 (TRNSP), 3 argument bytes, then RET
    let code = [0x28, 0x06, 0x01, 0xAA, 0xBB, 0xCC, 0x00];
    let d = disassemble_slice(&code, 0);
    assert!(d.is_complete());
    assert_eq!(d.instructions.len(), 2);
    let k = &d.instructions[0];
    assert_eq!((k.mnemonic, k.len, k.sub, k.payload.as_slice()), ("KAWAI", 6, Some((1, "TRNSP")), &[0xAA, 0xBB, 0xCC][..]));
    assert_eq!(d.instructions[1].mnemonic, "RET");
    // smallest legal KAWAI has no arguments
    let d = disassemble_slice(&[0x28, 0x03, 0xFF], 0);
    assert!(d.is_complete());
    assert_eq!(d.instructions[0].sub, Some((0xFF, "RESET")));
    // impossible lengths
    assert!(matches!(disassemble_slice(&[0x28, 0x02, 0x00], 0).error, Some(DecodeError::BadLength { offset: 0, declared: 2 })));
    assert!(matches!(disassemble_slice(&[0x28, 0x00, 0x00], 0).error, Some(DecodeError::BadLength { .. })));
    // length running past the script
    assert!(matches!(disassemble_slice(&[0x28, 0x09, 0x00, 0x00], 0).error, Some(DecodeError::Truncated { needed: 9, available: 4, .. })));
    assert!(matches!(disassemble_slice(&[0x28], 0).error, Some(DecodeError::Truncated { .. })));
}

#[test]
fn special_sub_opcodes_select_the_layout() {
    let d = disassemble_slice(&[0x0F, 0xF5, 0x01, 0x0F, 0xF9, 0x0F, 0xF8, 0x10, 0x00], 0);
    assert!(d.is_complete());
    let lens: Vec<usize> = d.instructions.iter().map(|i| i.len).collect();
    assert_eq!(lens, [3, 2, 4]);
    assert_eq!(d.instructions[0].sub, Some((0xF5, "ARROW")));
    assert_eq!(d.instructions[1].sub, Some((0xF9, "FLMAT")));
    assert!(matches!(disassemble_slice(&[0x0F, 0x00], 0).error, Some(DecodeError::UnknownSubOpcode { sub: 0, .. })));
    assert!(matches!(disassemble_slice(&[0x0F], 0).error, Some(DecodeError::Truncated { .. })));
}

#[test]
fn display_lists_operands_and_targets() {
    let i = decode_at(&[0xA4, 0x01], 0, 0x20).unwrap();
    assert_eq!(i.to_string(), "0020: VISI visible=1");
    let mut bytes = xyzi(5, 6, 7, 9);
    bytes[1] = 0x10;
    assert_eq!(decode_at(&bytes, 0, 0).unwrap().to_string(), "0000: XYZI x=Var[1][5] y=6 z=7 triangle=9");
    assert_eq!(decode_at(&[0x10, 0x02, 0x00], 0, 0).unwrap().to_string(), "0000: JMPF jump=2 -> 0003");
}

#[test]
fn every_opcode_survives_every_truncation_and_filler() {
    for op in 0..=255u8 {
        for filler in [0x00u8, 0x01, 0x7F, 0x80, 0xFF] {
            for len in 0..24usize {
                let mut code = vec![filler; len];
                if let Some(first) = code.first_mut() {
                    *first = op;
                }
                match decode_at(&code, 0, 0) {
                    Ok(i) => assert!(i.len >= 1 && i.len <= len, "op {op:#04X} len {len} -> {}", i.len),
                    Err(e) => assert_eq!(e.offset(), 0),
                }
                let d = disassemble_slice(&code, 7);
                let consumed: usize = d.instructions.iter().map(|i| i.len).sum();
                assert!(consumed <= len);
                if d.error.is_none() {
                    assert_eq!(consumed, len);
                }
            }
        }
    }
}

#[test]
fn random_bytes_never_panic_and_instructions_tile_contiguously() {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u8
    };
    for round in 0..4000usize {
        let len = usize::from(next()) % 96;
        let code: Vec<u8> = (0..len).map(|_| next()).collect();
        let d = disassemble_slice(&code, round);
        let mut at = round;
        for i in &d.instructions {
            assert_eq!(i.offset, at);
            at += i.len;
            assert!(i.len >= 1);
            assert!(!i.to_string().is_empty());
        }
        assert!(at <= round + len);
        if d.is_complete() {
            assert_eq!(at, round + len);
        }
        let _ = d.bad_jumps(round + len, true);
        let _ = ScriptEnd::of(&d);
    }
}

#[test]
fn script_end_classification() {
    let end = |code: &[u8]| ScriptEnd::of(&disassemble_slice(code, 0));
    assert_eq!(end(&[]), ScriptEnd::Empty);
    assert_eq!(end(&[0xA4, 0x01, 0x00]), ScriptEnd::Ret);
    assert_eq!(end(&[0x07, 0x00]), ScriptEnd::Retto);
    assert_eq!(end(&[0xC7, 0x01, 0x12, 0x02]), ScriptEnd::Loop);
    assert_eq!(end(&[0x10, 0x01]), ScriptEnd::ForwardJump);
    assert_eq!(end(&[0xFF]), ScriptEnd::GameOver);
    assert_eq!(end(&[0xA4, 0x01]), ScriptEnd::Other(0xA4));
}

// ---------------------------------------------------------------------------------------
// Init-script analysis
// ---------------------------------------------------------------------------------------

#[test]
fn init_analysis_finds_models_placements_and_the_player() {
    let npc = concat(&[
        &[0xA1, 0x03],
        &xyzi(100, -200, 30, 4),
        &[0xB3, 0x00, 0x40],
        &[0xA4, 0x00],
        &[0xC7, 0x01],
        &[0xC5, 0x00, 0x20],
        &[0xC6, 0x00, 0x10],
        &[0x00],
    ]);
    let hero = concat(&[&[0xA1, 0x00], &[0xA0, 0x02], &[0x00]]);
    let door = vec![0x00]; // no CHAR: not a character
    let built = SynthScript::default().entity("door", vec![(0, door)]).entity("npc", vec![(0, npc)]).entity("hero", vec![(0, hero)]).build();
    let s = FieldScript::parse(&built).unwrap();
    let inits = s.entity_inits(None);
    assert_eq!(inits.len(), 3);
    assert!(!inits[0].is_character() && !inits[0].is_player());
    assert_eq!(inits[0].model_index, None);

    let npc = &inits[1];
    assert!(npc.is_character() && !npc.is_player());
    assert_eq!(npc.char_id.map(|t| t.value), Some(Arg::Literal(3)));
    // the model index is the order among CHAR entities, not the operand (3)
    assert_eq!(npc.model_index, Some(0));
    assert_eq!(npc.placements.len(), 1);
    let p = npc.placement().unwrap();
    assert_eq!(
        (p.kind, p.x, p.y, p.z, p.triangle, p.conditional),
        (PlacementKind::Xyzi, Arg::Literal(100), Arg::Literal(-200), Arg::Literal(30), Arg::Literal(4), false)
    );
    assert_eq!(p.xy(), Some((100, -200)));
    assert!(p.is_literal());
    assert_eq!(npc.direction.map(|t| t.value), Some(Arg::Literal(0x40)));
    assert_eq!(npc.initially_visible(), Some(false));
    assert_eq!(npc.solid.map(|t| t.value), Some(Arg::Literal(1)));
    assert_eq!(npc.talk_radius.map(|t| t.value), Some(Arg::Literal(0x20)));
    assert_eq!(npc.solid_radius.map(|t| t.value), Some(Arg::Literal(0x10)));

    let hero = &inits[2];
    assert!(hero.is_character() && hero.is_player());
    assert_eq!(hero.player.map(|t| t.value), Some(Arg::Literal(2)));
    assert_eq!(hero.model_index, Some(1));
    assert_eq!(hero.placement(), None);
    assert_eq!(s.player_entity(2), Some(2));
    assert_eq!(s.player_entity(0), None);
    assert_eq!(s.player_entities(), vec![(2, 2)]);
    assert_eq!(s.character_entities(None).len(), 2);
}

#[test]
fn model_indices_resolve_through_the_model_list() {
    let models = ModelList {
        models: vec![
            ModelRef { name: "a".into(), hrc: "AAAA.HRC".into(), animation_count: 1 },
            ModelRef { name: "b".into(), hrc: "BBBB.HRC".into(), animation_count: 2 },
        ],
        declared: 2,
        covered: 0,
    };
    let built = SynthScript::default()
        .entity("x", vec![(0, vec![0xA1, 0x63, 0x00])])
        .entity("y", vec![(0, vec![0x00])])
        .entity("z", vec![(0, vec![0xA1, 0x00, 0x00])])
        .entity("w", vec![(0, vec![0xA1, 0x00, 0x00])])
        .build();
    let s = FieldScript::parse(&built).unwrap();
    let inits = s.entity_inits(Some(&models));
    assert_eq!(inits[0].model_ref.as_ref().map(|m| m.hrc.as_str()), Some("AAAA.HRC"));
    assert_eq!(inits[2].model_ref.as_ref().map(|m| m.hrc.as_str()), Some("BBBB.HRC"));
    // a third CHAR entity has no record in a two-model list
    assert_eq!(inits[3].model_index, Some(2));
    assert_eq!(inits[3].model_ref, None);
}

#[test]
fn conditional_effects_are_flagged() {
    // IFUB skipping a VISI 0 and an XYZI: both lie between the jump and its target
    let skipped = concat(&[&[0xA4, 0x00], &xyzi(1, 2, 3, 4)]);
    // jump operand is the 6th byte of the IFUB (offset 5 from its start); the distance is
    // measured from there to the first byte after the skipped block: 1 + skipped.len()
    let distance = u8::try_from(1 + skipped.len()).unwrap();
    let init = concat(&[&[0xA1, 0x00], &[0xA4, 0x01], &[0x14, 0x00, 0x01, 0x00, 0x00, distance], &skipped, &xyzi(9, 9, 9, 0), &[0x00]]);
    let built = SynthScript::default().entity("n", vec![(0, init)]).build();
    let s = FieldScript::parse(&built).unwrap();
    let n = &s.entity_inits(None)[0];
    assert_eq!(n.decode_error, None);
    // visibility: the unconditional VISI 1 stands, the conditional VISI 0 may override it
    let v = n.visible.unwrap();
    assert_eq!((v.value, v.conditional, v.may_be_overridden), (Arg::Literal(1), false, true));
    assert_eq!(n.placements.len(), 2);
    assert!(n.placements[0].conditional);
    assert!(!n.placements[1].conditional);
    assert_eq!(n.placement().unwrap().x, Arg::Literal(9));
    // with only a conditional placement, that one is reported
    let placed = xyzi(7, 7, 7, 1);
    let distance = u8::try_from(1 + placed.len()).unwrap();
    let init = concat(&[&[0x14, 0x00, 0x01, 0x00, 0x00, distance], &placed, &[0x00]]);
    let built = SynthScript::default().entity("n", vec![(0, init)]).build();
    let n = &FieldScript::parse(&built).unwrap().entity_inits(None)[0];
    assert_eq!(n.placements.len(), 1);
    assert!(n.placement().unwrap().conditional);
}

#[test]
fn script_zero_is_an_init_part_and_a_main_part() {
    // CHAR; PC; RET | IFSW (skips placement); XYZI; VISI 0; JMPB 0 (idle loop)
    let placed = concat(&[&xyzi(10, 20, 30, 2), &[0xA4, 0x00]]);
    // IFSW: B1B2, A u16, V s16, C, E: the jump operand is the last byte (offset 7 of the instruction)
    let distance = u8::try_from(1 + placed.len()).unwrap();
    let ifsw = [0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, distance];
    let init = concat(&[&[0xA1, 0x00], &[0xA0, 0x03], &[0x00], &ifsw, &placed, &[0x12, 0x00]]);
    let built = SynthScript::default().entity("n", vec![(0, init)]).build();
    let s = FieldScript::parse(&built).unwrap();
    let n = &s.entity_inits(None)[0];
    assert_eq!(n.decode_error, None);
    // CHAR and PC run in the init part; the placement and VISI belong to the main part
    assert_eq!(n.char_id.map(|t| t.phase), Some(Phase::Init));
    assert_eq!(n.player.map(|t| (t.value, t.phase)), Some((Arg::Literal(3), Phase::Init)));
    assert_eq!(n.placements.len(), 1);
    assert_eq!(n.placements[0].phase, Phase::Main);
    assert!(n.placements[0].conditional);
    let v = n.visible.unwrap();
    assert_eq!((v.value, v.phase, v.conditional), (Arg::Literal(0), Phase::Main, true));
    // main starts at the byte after the first RET
    let start = s.span(0, 0).unwrap().start;
    assert_eq!(n.main_start, Some(start + 2 + 2 + 1));
    assert_eq!(n.instructions, 3 + 1 + 2 + 1); // CHAR PC RET IFSW XYZI VISI JMPB
}

#[test]
fn code_that_nothing_reaches_is_ignored() {
    // RET (end of init), RET (main part is just a return), then a PC that no path executes
    let init = concat(&[&[0xA1, 0x00], &[0x00], &[0x00], &[0xA0, 0x05]]);
    let built = SynthScript::default().entity("n", vec![(0, init)]).build();
    let n = &FieldScript::parse(&built).unwrap().entity_inits(None)[0];
    assert!(n.is_character());
    assert!(!n.is_player());
    assert_eq!(n.instructions, 3);
}

#[test]
fn map_jumps_are_listed_once_per_distinct_script() {
    let jump = |field: u16, x: i16, y: i16, tri: i16, dir: u8| {
        let mut v = vec![0x60];
        v.extend_from_slice(&field.to_le_bytes());
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v.extend_from_slice(&tri.to_le_bytes());
        v.push(dir);
        v
    };
    let init = concat(&[&[0xA4, 0x01], &[0x00]]);
    let go = concat(&[&jump(117, 1049, 400, 90, 0x80), &[0x00]]);
    // slot 1 holds the jump; slots 2.. share the lone RET the builder gave them
    let built = SynthScript::default().entity("door", vec![(0, init), (1, go)]).build();
    let s = FieldScript::parse(&built).unwrap();
    let jumps = s.map_jumps();
    assert_eq!(jumps.len(), 1);
    let j = &jumps[0];
    assert_eq!((j.entity, j.entity_name.as_str(), j.script, j.field_id), (0, "door", 1, 117));
    assert_eq!((j.x, j.y, j.triangle, j.direction, j.conditional), (1049, 400, 90, 0x80, false));
}

#[test]
fn variable_operands_are_reported_not_guessed() {
    let mut place = xyzi(5, 6, 7, 9);
    place[1] = 0x10; // x comes from bank 1
    let init = concat(&[&[0xA1, 0x00], &place, &[0x00]]);
    let built = SynthScript::default().entity("n", vec![(0, init)]).build();
    let n = &FieldScript::parse(&built).unwrap().entity_inits(None)[0];
    let p = n.placement().unwrap();
    assert_eq!(p.x, Arg::Variable { bank: 1, address: 5 });
    assert_eq!(p.xy(), None);
    assert!(!p.is_literal());
    let mesh = Walkmesh { triangles: vec![], access: vec![] };
    assert_eq!(check_placement(&mesh, p), None);
}

#[test]
fn a_broken_init_script_is_reported_with_what_was_decoded() {
    let built = SynthScript::default().entity("n", vec![(0, vec![0xA1, 0x00, 0x0C, 0x00])]).build();
    let n = &FieldScript::parse(&built).unwrap().entity_inits(None)[0];
    assert!(n.is_character());
    assert!(matches!(n.decode_error, Some(DecodeError::UnknownOpcode { opcode: 0x0C, .. })));
}

#[test]
fn placements_are_checked_against_the_walkmesh() {
    let v = |x: i16, y: i16, z: i16| Vertex { x, y, z };
    let mesh = Walkmesh {
        triangles: vec![[v(0, 0, 0), v(100, 0, 100), v(0, 100, 0)], [v(100, 0, 100), v(100, 100, 100), v(0, 100, 0)]],
        access: vec![[0xFFFF; 3]; 2],
    };
    let place = |x: i16, y: i16, z: i16, tri: u16| Placement {
        kind: PlacementKind::Xyzi,
        x: Arg::Literal(x),
        y: Arg::Literal(y),
        z: Arg::Literal(z),
        triangle: Arg::Literal(tri),
        offset: 0,
        phase: Phase::Init,
        conditional: false,
    };
    // inside triangle 0; its height at (10, 10) is 10 (z rises with x)
    match check_placement(&mesh, &place(10, 10, 13, 0)) {
        Some(WalkmeshFit::Inside { height_error: Some(e) }) => assert!((e - 3.0).abs() < 1e-9, "{e}"),
        other => panic!("{other:?}"),
    }
    // on the shared edge counts as inside either triangle
    assert!(matches!(check_placement(&mesh, &place(50, 50, 50, 0)), Some(WalkmeshFit::Inside { .. })));
    assert!(matches!(check_placement(&mesh, &place(50, 50, 50, 1)), Some(WalkmeshFit::Inside { .. })));
    // named triangle 1 but the point is in triangle 0
    match check_placement(&mesh, &place(10, 10, 10, 1)) {
        Some(WalkmeshFit::Outside { distance, containing: Some(0) }) => assert!(distance > 0.0),
        other => panic!("{other:?}"),
    }
    // far outside everything
    assert!(matches!(check_placement(&mesh, &place(500, 500, 0, 0)), Some(WalkmeshFit::Outside { containing: None, .. })));
    // triangle index past the end
    assert_eq!(check_placement(&mesh, &place(10, 10, 0, 9)), Some(WalkmeshFit::NoSuchTriangle));
    // XYZ names no triangle
    let mut p = place(10, 10, 0, 0);
    p.kind = PlacementKind::Xyz;
    p.triangle = Arg::Absent;
    assert_eq!(check_placement(&mesh, &p), Some(WalkmeshFit::Unnamed { found: Some(0) }));
    assert_eq!(locate_on_walkmesh(&mesh, 90, 90), Some(1));
    assert_eq!(locate_on_walkmesh(&mesh, -1, 0), None);
    // a degenerate (collinear) triangle has no height but does not panic
    let flat = [v(0, 0, 0), v(10, 10, 0), v(20, 20, 0)];
    assert_eq!(triangle_height_at(&flat, 5, 5), None);
    assert!(distance_to_triangle_xy(&flat, 30, 30) > 0.0);
}
