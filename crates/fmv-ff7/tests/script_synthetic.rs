//! The public field-script API on a small synthetic script section (no game data).

use fmv_ff7::field::{CameraRange, Gateway, Triggers, Vertex, Walkmesh};
use fmv_ff7::script::{
    check_placement, decode_dialog, disassemble_slice, resolve_gateways, Arg, DecodeError, FieldScript, Maplist, Phase, PlacementKind, ScriptError,
    WalkmeshFit, INIT_SCRIPT, SCRIPTS_PER_ENTITY,
};

/// Hand-assembled section 0: two entities ("npc" and "hero"), one dialog, no AKAO blocks.
fn section() -> Vec<u8> {
    // npc script 0: CHAR 0; XYZI(10, 20, 30, triangle 1); DIR 64; VISI 1; RET
    let mut npc = vec![0xA1, 0x00, 0xA5, 0x00, 0x00];
    for v in [10i16, 20, 30, 1] {
        npc.extend_from_slice(&v.to_le_bytes());
    }
    npc.extend_from_slice(&[0xB3, 0x00, 0x40, 0xA4, 0x01, 0x00]);
    // hero script 0: CHAR 1; PC 0; RET | main part: JMPB 0
    let hero = vec![0xA1, 0x01, 0xA0, 0x00, 0x00, 0x12, 0x00];
    let ret = vec![0x00];

    let names = ["npc", "hero"];
    let code_start = 0x20 + 8 * names.len() + 64 * names.len();
    let mut code = Vec::new();
    let mut entries: Vec<[u16; 32]> = Vec::new();
    for body in [&npc, &hero] {
        let mut row = [0u16; 32];
        for (slot, entry) in row.iter_mut().enumerate() {
            *entry = u16::try_from(code_start + code.len()).unwrap();
            code.extend_from_slice(if slot == 0 { body } else { &ret });
        }
        entries.push(row);
    }
    let dialog_at = code_start + code.len();
    let text: Vec<u8> = "Hello".bytes().map(|b| b - 0x20).chain([0xFF]).collect();

    let mut out = vec![0u8; 0x20];
    out[0] = 2;
    out[1] = 5;
    out[2] = 2; // entities
    out[3] = 2; // models
    out[4..6].copy_from_slice(&u16::try_from(dialog_at).unwrap().to_le_bytes());
    out[8..10].copy_from_slice(&512u16.to_le_bytes());
    out[0x10..0x15].copy_from_slice(b"maker");
    out[0x18..0x1D].copy_from_slice(b"synth");
    for n in names {
        let mut name = [0u8; 8];
        name[..n.len()].copy_from_slice(n.as_bytes());
        out.extend_from_slice(&name);
    }
    for row in &entries {
        for e in row {
            out.extend_from_slice(&e.to_le_bytes());
        }
    }
    out.extend_from_slice(&code);
    out.extend_from_slice(&1u16.to_le_bytes()); // one dialog
    out.extend_from_slice(&4u16.to_le_bytes()); // pointer: right after the table
    out.extend_from_slice(&text);
    out
}

#[test]
fn parse_disassemble_and_analyse_through_the_public_api() {
    let bytes = section();
    let script = FieldScript::parse(&bytes).unwrap();
    assert_eq!(script.entity_count(), 2);
    assert_eq!((script.header().entity_count, script.header().scale), (2, 512));
    assert_eq!(script.entity_by_name("hero"), Some(1));
    assert_eq!(SCRIPTS_PER_ENTITY, 32);

    let listing = script.disassemble(0, INIT_SCRIPT).unwrap();
    let mnemonics: Vec<&str> = listing.iter().map(|i| i.mnemonic).collect();
    assert_eq!(mnemonics, ["CHAR", "XYZI", "DIR", "VISI", "RET"]);
    assert_eq!(listing[1].len, 11);
    assert!(listing[1].to_string().ends_with("XYZI x=10 y=20 z=30 triangle=1"));

    let inits = script.entity_inits(None);
    assert!(inits[0].is_character() && !inits[0].is_player());
    let p = inits[0].placement().unwrap();
    assert_eq!((p.kind, p.xy(), p.z, p.triangle), (PlacementKind::Xyzi, Some((10, 20)), Arg::Literal(30), Arg::Literal(1)));
    assert_eq!(inits[0].direction.map(|d| d.value), Some(Arg::Literal(0x40)));
    assert_eq!(inits[0].initially_visible(), Some(true));
    assert_eq!(inits[1].model_index, Some(1));
    assert_eq!(script.player_entities(), [(1, 0)]);
    // hero's `JMPB 0` is its main part
    assert!(inits[1].main_start.is_some());
    assert_eq!(inits[1].char_id.map(|c| c.phase), Some(Phase::Init));

    // the placement lies on triangle 1 of a two-triangle mesh
    let v = |x: i16, y: i16, z: i16| Vertex { x, y, z };
    let mesh = Walkmesh {
        triangles: vec![[v(0, 0, 0), v(5, 0, 0), v(0, 5, 0)], [v(0, 0, 30), v(50, 0, 30), v(0, 50, 30)]],
        access: vec![[0xFFFF; 3]; 2],
    };
    match check_placement(&mesh, p) {
        Some(WalkmeshFit::Inside { height_error: Some(e) }) => assert!(e.abs() < 1e-9, "{e}"),
        other => panic!("{other:?}"),
    }

    // dialogs
    assert_eq!(script.dialogs().len(), 1);
    assert_eq!(script.dialog_text(0).unwrap().text, "Hello");
    assert_eq!(decode_dialog(&[0x28, 0xFF, 0x28]).consumed, 2);
}

#[test]
fn malformed_input_is_an_error_or_a_partial_listing_never_a_panic() {
    let bytes = section();
    for cut in 0..bytes.len() {
        if let Ok(script) = FieldScript::parse(&bytes[..cut]) {
            for e in 0..script.entity_count() {
                for s in 0..SCRIPTS_PER_ENTITY {
                    let _ = script.disassemble_partial(e, s);
                    let _ = script.disassemble(e, s);
                }
            }
            let _ = script.entity_inits(None);
            let _ = script.map_jumps();
            let _ = script.player_entities();
        }
    }
    assert!(matches!(FieldScript::parse(&[]), Err(ScriptError::TooShort { len: 0 })));
    let d = disassemble_slice(&[0xA1, 0x00, 0x0C], 0);
    assert_eq!(d.instructions.len(), 1);
    assert!(matches!(d.error, Some(DecodeError::UnknownOpcode { opcode: 0x0C, offset: 2 })));
}

#[test]
fn gateways_join_with_the_maplist() {
    let mut list = 3u16.to_le_bytes().to_vec();
    for name in ["alpha", "beta", "gamma"] {
        let mut rec = [0u8; 32];
        rec[..name.len()].copy_from_slice(name.as_bytes());
        list.extend_from_slice(&rec);
    }
    let maplist = Maplist::parse(&list).unwrap();
    let gw = |target: u16| Gateway { exit_a: [0; 3], exit_b: [1; 3], destination: [100, -50, 7], target_field_id: target, unknown: [0; 4] };
    let triggers = Triggers {
        name: "x".into(),
        control_direction: 0,
        focus_height: 0,
        camera_range: CameraRange { left: 0, bottom: 0, right: 0, top: 0 },
        bg_animation_sizes: [0; 4],
        gateways: vec![gw(1), gw(Gateway::UNUSED_FIELD_ID), gw(99)],
        triggers: vec![],
    };
    let resolved = resolve_gateways(&triggers, &maplist);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].target_field.as_deref(), Some("beta"));
    assert_eq!((resolved[0].destination_x, resolved[0].destination_y, resolved[0].destination_triangle), (100, -50, Some(7)));
    assert_eq!(resolved[1].target_field, None);
}
