//! Unit tests on synthetic scripts (no game data).

use super::synth::{dialog, SynthScript};
use super::*;

fn ret() -> Vec<u8> {
    vec![0x00]
}

#[test]
fn layout_round_trip() {
    let init0: Vec<u8> = vec![0xA1, 0x00, 0x00]; // CHAR 0, RET
    let init1: Vec<u8> = vec![0xA0, 0x00, 0x00]; // PC 0, RET
    let built = SynthScript { model_count: 2, scale: 512, dialogs: vec![dialog("Hi"), dialog("Yo!")], akao: vec![b"AKAOxxxx".to_vec(), b"TUTORIAL".to_vec()], ..Default::default() }
        .entity("npc", vec![(0, init0.clone())])
        .entity("hero", vec![(0, init1.clone()), (1, ret())])
        .build();
    let s = FieldScript::parse(&built).unwrap();
    let h = s.header();
    assert_eq!((h.data_version, h.event_version, h.entity_count, h.model_count, h.scale), (2, 5, 2, 2, 512));
    assert_eq!(h.creator, "synth");
    assert_eq!(h.name, "fixture");
    assert_eq!(s.entity_name(0), Some("npc"));
    assert_eq!(s.entity_by_name("hero"), Some(1));
    assert_eq!(s.entity_by_name("nobody"), None);
    // script 0 of entity 0 is the first script and runs for exactly its own bytes
    let span = s.span(0, 0).unwrap();
    assert_eq!(span.start, s.layout().code_start);
    assert_eq!(s.script_bytes(0, 0).unwrap(), &init0[..]);
    assert_eq!(s.script_bytes(1, 0).unwrap(), &init1[..]);
    // empty slots each got their own single RET
    assert_eq!(s.script_bytes(0, 5).unwrap(), &[0x00]);
    assert_eq!(s.span(0, 32), None);
    assert_eq!(s.span(2, 0), None);
    // the last script ends where the dialog table starts
    let last = s.span(1, 31).unwrap();
    assert_eq!(last.end, usize::from(h.string_offset));
    assert_eq!(s.layout().code_end, usize::from(h.string_offset));
    // dialogs
    assert_eq!(s.dialogs().len(), 2);
    assert_eq!(s.dialog_text(0).unwrap().text, "Hi");
    assert_eq!(s.dialog_text(1).unwrap().text, "Yo!");
    assert!(s.dialog_text(2).is_none());
    assert!(s.dialogs().iter().all(|d| d.terminated));
    assert!(s.dialog_error().is_none());
    // akao blocks
    let blocks = s.akao_blocks();
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].is_akao());
    assert!(!blocks[1].is_akao());
    assert_eq!(blocks[0].len, 8);
    assert_eq!(blocks[1].len, 8);
}

#[test]
fn scripts_that_share_an_entry_share_a_span() {
    // two slots pointing at the same address (real files do this for their unused slots)
    let mut built = SynthScript::default().entity("a", vec![(0, vec![0x00, 0x00])]).build();
    // entity 0 slot 1 -> same address as slot 0
    let table_at = 0x20 + 8;
    let e0 = u16::from_le_bytes([built[table_at], built[table_at + 1]]);
    built[table_at + 2..table_at + 4].copy_from_slice(&e0.to_le_bytes());
    let s = FieldScript::parse(&built).unwrap();
    assert_eq!(s.span(0, 0), s.span(0, 1));
    // the span runs to the next higher entry (slot 2's RET); slot 1's own RET is unreferenced
    assert_eq!(s.span(0, 0).unwrap().len(), 3);
}

#[test]
fn damaged_headers_are_errors_not_panics() {
    assert!(matches!(FieldScript::parse(&[]), Err(ScriptError::TooShort { len: 0 })));
    assert!(matches!(FieldScript::parse(&[0u8; 31]), Err(ScriptError::TooShort { len: 31 })));
    // header promises 255 entities but the data ends right after it
    let mut h = vec![0u8; 0x20];
    h[2] = 255;
    assert!(matches!(FieldScript::parse(&h), Err(ScriptError::Truncated { .. })));
    // every truncation of a valid section either parses or errors
    let built = SynthScript { dialogs: vec![dialog("x")], akao: vec![b"AKAO".to_vec()], ..Default::default() }.entity("e", vec![(0, vec![0x00])]).build();
    for cut in 0..built.len() {
        let _ = FieldScript::parse(&built[..cut]);
    }
}

#[test]
fn bad_string_offset_falls_back_to_the_section_end() {
    let mut built = SynthScript::default().entity("e", vec![(0, vec![0x00])]).build();
    built[4..6].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let s = FieldScript::parse(&built).unwrap();
    assert!(s.dialog_error().is_some());
    assert_eq!(s.layout().code_end, built.len());
    assert!(s.dialogs().is_empty());
    // the scripts still decode
    assert!(s.disassemble(0, 0).is_ok());
}

#[test]
fn dialog_pointers_out_of_range_do_not_panic() {
    let mut built = SynthScript { dialogs: vec![dialog("abc")], ..Default::default() }.entity("e", vec![(0, vec![0x00])]).build();
    let table = usize::from(u16::from_le_bytes([built[4], built[5]]));
    built[table + 2..table + 4].copy_from_slice(&0xFFF0u16.to_le_bytes());
    let s = FieldScript::parse(&built).unwrap();
    assert_eq!(s.dialogs().len(), 1);
    assert!(!s.dialogs().first().unwrap().terminated);
    assert_eq!(s.dialog_text(0), None);
}

#[test]
fn indexing_errors_for_unknown_entities_and_slots() {
    let built = SynthScript::default().entity("e", vec![(0, ret())]).build();
    let s = FieldScript::parse(&built).unwrap();
    assert!(matches!(s.disassemble(1, 0), Err(ScriptError::NoSuchEntity(1))));
    assert!(matches!(s.disassemble(0, 32), Err(ScriptError::NoSuchScript(32))));
    let listing = s.disassemble(0, 0).unwrap();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].mnemonic, "RET");
}
