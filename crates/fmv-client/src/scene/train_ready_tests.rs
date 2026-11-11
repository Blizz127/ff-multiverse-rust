//! Synthetic tests of the private presentation envelope only. These fixtures do not mint a
//! checked train entry: public reconstruction separately executes and validates owned source.

use super::*;
use fmv_ff7::script::scene::{CameraOp, Stage};

fn fixture() -> TrainReadyPresentation {
    let mut cast = Cast::new([30, 36]);
    cast.seed_shake_rng([53, 201]);
    let ground = |_: i16, _: i16, _: Option<u16>| Some(-53.0);
    cast.apply(&Stage::Place { entity: 30, x: 2, y: 537, z: Some(-53), triangle: Some(28) }, 0.0, &ground);
    cast.apply(&Stage::Show { entity: 36, visible: false }, 0.0, &ground);
    cast.apply_loop(30, 0, "standing", 0.0, false);
    cast.apply(&Stage::Camera(CameraOp::Scroll { x: 18, y: 2, frames: 0 }), 0.0, &ground);
    cast.advance(37.25);
    TrainReadyPresentation {
        def: fmv_ff7::script::scene::train_scene(), cast,
        elapsed_frames: f32::from_bits(0x4329_ffe8),
        initial_shake_rng: [53, 201],
        entry_record: (0..4096).map(|n| (n % 251) as u8).collect(),
    }
}

fn signed(body: Vec<u8>) -> Vec<u8> {
    let checksum = hash(DOMAIN, &body);
    let mut bytes = body;
    bytes.extend(checksum);
    bytes
}

#[test]
fn ready_envelope_retains_exact_elapsed_bits_and_binds_the_complete_separate_input() {
    let retained = fixture();
    let bytes = retained.encode().unwrap();
    assert!(bytes.len() <= MAX_TRAIN_PRESENTATION_BYTES);
    assert!(bytes.len() < retained.entry_record.len(), "the full original input is retained separately, not duplicated");
    let (rng, elapsed, cast) = decode(&retained.entry_record, &bytes).unwrap();
    assert_eq!(rng, retained.initial_shake_rng);
    assert_eq!(elapsed.to_bits(), retained.elapsed_frames.to_bits());
    assert_eq!(cast.encode(), retained.cast.checkpoint().unwrap().encode());
    for at in [0, 1024, retained.entry_record.len() - 1] {
        let mut changed = retained.entry_record.clone();
        changed[at] ^= 1;
        assert!(decode(&changed, &bytes).is_err(), "input byte {at} is part of the binding");
    }
    assert!(decode(&retained.entry_record[..1024], &bytes).is_err(), "truncating original input is not an upgrade path");
}

#[test]
fn ready_envelope_rejects_every_truncation_and_changed_byte() {
    let retained = fixture();
    let bytes = retained.encode().unwrap();
    for end in 0..bytes.len() {
        assert!(decode(&retained.entry_record, &bytes[..end]).is_err(), "truncated at {end}");
    }
    for at in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[at] ^= 1;
        assert!(decode(&retained.entry_record, &changed).is_err(), "changed byte {at}");
    }
    assert!(decode(&retained.entry_record, &vec![0; MAX_TRAIN_PRESENTATION_BYTES + 1]).is_err());
}

#[test]
fn valid_checksums_do_not_admit_unknown_headers_invalid_clocks_lengths_or_trailing_data() {
    let retained = fixture();
    let bytes = retained.encode().unwrap();
    let body = &bytes[..bytes.len() - 32];
    let mut cases = Vec::new();
    let mut magic = body.to_vec(); magic[0] ^= 1; cases.push(magic);
    let mut version = body.to_vec(); version[8..10].copy_from_slice(&2u16.to_le_bytes()); cases.push(version);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, -0.0] {
        let mut clock = body.to_vec();
        clock[44..48].copy_from_slice(&value.to_bits().to_le_bytes());
        cases.push(clock);
    }
    let mut length = body.to_vec(); length[48..52].copy_from_slice(&u32::MAX.to_le_bytes()); cases.push(length);
    let mut trailing = body.to_vec(); trailing.push(0); cases.push(trailing);
    for body in cases {
        assert!(decode(&retained.entry_record, &signed(body)).is_err());
    }
}
