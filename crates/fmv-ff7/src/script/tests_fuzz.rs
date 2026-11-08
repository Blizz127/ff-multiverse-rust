//! Deterministic fuzzing of the whole script pipeline (parse, disassemble, analyse, text, maplist,
//! model loader) with random and mutated sections. The dev profile keeps overflow checks on, so
//! an unchecked addition or a bad slice would panic here.

use super::synth::{dialog, SynthScript};
use super::*;

/// Iterations per test; `FMV_SCRIPT_FUZZ=200000` runs a much longer campaign.
fn iterations(default: usize) -> usize {
    std::env::var("FMV_SCRIPT_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 24
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.byte()).collect()
    }
}

/// Push a section through every public entry point.
fn exercise(bytes: &[u8]) {
    let _ = Maplist::parse(bytes);
    let _ = parse_model_list(bytes);
    let _ = decode_dialog(bytes);
    let _ = disassemble_slice(bytes, 0x10);
    let Ok(script) = FieldScript::parse(bytes) else { return };
    let layout = script.layout();
    assert!(layout.code_start <= bytes.len());
    assert!(layout.code_end <= bytes.len());
    for e in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            if let Some(span) = script.span(e, slot) {
                assert!(layout.code_start <= span.start && span.start < span.end && span.end <= bytes.len());
                assert_eq!(script.script_bytes(e, slot).map(<[u8]>::len), Some(span.len()));
            }
            if let Ok(d) = script.disassemble_partial(e, slot) {
                let mut at = script.span(e, slot).map_or(0, |s| s.start);
                for i in &d.instructions {
                    assert_eq!(i.offset, at);
                    at += i.len;
                    let _ = i.to_string();
                }
                let end = script.span(e, slot).map_or(0, |s| s.end);
                let _ = d.bad_jumps(end, true);
                let _ = ScriptEnd::of(&d);
            }
            let _ = script.disassemble(e, slot);
        }
    }
    for init in script.entity_inits(None) {
        let _ = init.placement();
        let _ = init.initially_visible();
        let _ = init.initially_solid();
    }
    let _ = script.character_entities(None);
    let _ = script.player_entities();
    let _ = script.player_entity(0);
    let jumps = script.map_jumps();
    let _ = resolve_map_jumps(&jumps, &Maplist::default());
    for d in script.dialogs() {
        if d.offset <= bytes.len() {
            assert!(d.offset + d.len <= bytes.len());
            assert!(script.dialog_text(d.index).is_some());
        } else {
            // a pointer past the end keeps its slot but has no text
            assert_eq!(d.len, 0);
            assert!(script.dialog_text(d.index).is_none());
        }
    }
    let _ = script.dialog_text(usize::MAX);
    for b in script.akao_blocks() {
        assert!(usize::try_from(b.offset).unwrap_or(usize::MAX) < bytes.len());
    }
}

fn random_script(rng: &mut Rng) -> SynthScript {
    let mut s = SynthScript { model_count: rng.byte(), scale: u16::from(rng.byte()) << 2, ..Default::default() };
    for e in 0..rng.below(7) {
        let mut scripts = Vec::new();
        for _ in 0..rng.below(4) {
            let slot = rng.below(SCRIPTS_PER_ENTITY);
            // half the time bias the bytes towards real opcodes so the decoder gets deep
            let code: Vec<u8> = if rng.below(2) == 0 {
                rng.bytes(60)
            } else {
                let mut v = Vec::new();
                for _ in 0..rng.below(14) {
                    v.push([0xA1, 0xA0, 0xA5, 0xA4, 0xB3, 0xC7, 0x14, 0x10, 0x12, 0x60, 0x28, 0x0F, 0x00, 0x40][rng.below(14)]);
                    v.extend(rng.bytes(12));
                }
                v
            };
            scripts.push((slot, code));
        }
        s = s.entity(&format!("e{e}"), scripts);
    }
    for _ in 0..rng.below(4) {
        let text: String = (0..rng.below(8)).map(|_| char::from(b'a' + (rng.byte() % 26))).collect();
        let mut d = dialog(&text);
        if rng.below(3) == 0 {
            d.pop(); // unterminated
        }
        if rng.below(4) == 0 {
            d.extend(rng.bytes(4));
        }
        s.dialogs.push(d);
    }
    for _ in 0..rng.below(3) {
        let mut b = b"AKAO".to_vec();
        b.extend(rng.bytes(10));
        s.akao.push(b);
    }
    s
}

#[test]
fn mutated_valid_sections_never_panic() {
    let mut rng = Rng(0x00C0_FFEE);
    for _ in 0..iterations(3000) {
        let mut bytes = random_script(&mut rng).build();
        for _ in 0..rng.below(10) {
            let n = bytes.len().max(1);
            match rng.below(5) {
                0 => {
                    let i = rng.below(n);
                    if let Some(b) = bytes.get_mut(i) {
                        *b = rng.byte();
                    }
                }
                1 => bytes.truncate(rng.below(n)),
                2 => bytes.extend(rng.bytes(8)),
                3 => {
                    // poke a header count or offset
                    let i = rng.below(0x20);
                    if let Some(b) = bytes.get_mut(i) {
                        *b = rng.byte();
                    }
                }
                _ => {
                    let i = rng.below(n);
                    if let Some(b) = bytes.get_mut(i) {
                        *b = 0xFF;
                    }
                }
            }
        }
        exercise(&bytes);
    }
}

#[test]
fn unmutated_random_sections_decode_cleanly_through_the_pipeline() {
    let mut rng = Rng(0xBAD_5EED);
    for _ in 0..iterations(1500) {
        let built = random_script(&mut rng).build();
        let script = FieldScript::parse(&built).expect("a synthetic section always parses");
        assert!(script.dialog_error().is_none());
        exercise(&built);
    }
}

#[test]
fn random_bytes_behind_a_plausible_header_never_panic() {
    let mut rng = Rng(0xDEAD_BEEF);
    for _ in 0..iterations(3000) {
        let mut bytes = vec![0u8; 0x20];
        bytes[0] = 2;
        bytes[1] = 5;
        bytes[2] = u8::try_from(rng.below(6)).unwrap_or(0); // entities
        bytes[3] = rng.byte();
        let total = 0x20 + usize::from(bytes[2]) * 72 + rng.below(400);
        bytes.extend((0..total).map(|_| if rng.below(3) == 0 { 0 } else { rng.byte() }));
        // string offset somewhere in or just outside the section
        let off = u16::try_from(rng.below(bytes.len() + 40)).unwrap_or(0);
        bytes[4..6].copy_from_slice(&off.to_le_bytes());
        bytes[6] = u8::try_from(rng.below(4)).unwrap_or(0); // AKAO count
        bytes[7] = 0;
        exercise(&bytes);
    }
}

#[test]
fn random_maplists_model_lists_and_dialogs_never_panic() {
    let mut rng = Rng(0x5EED_CAFE);
    for _ in 0..iterations(4000) {
        let mut bytes = rng.bytes(300);
        if rng.below(2) == 0 {
            // a plausible count word
            let n = u16::try_from(rng.below(12)).unwrap_or(0);
            if bytes.len() >= 2 {
                bytes[..2].copy_from_slice(&n.to_le_bytes());
            }
        }
        let _ = Maplist::parse(&bytes);
        let list = parse_model_list(&bytes);
        if let Ok(l) = list {
            assert!(l.covered <= bytes.len().max(6));
            assert!(l.models.len() <= usize::from(l.declared));
        }
        let d = decode_dialog(&bytes);
        assert!(d.consumed <= bytes.len());
    }
}
