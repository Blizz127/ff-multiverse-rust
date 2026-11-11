//! Synthetic presentation-envelope fixtures. They exercise bounded parsing and replay-shape
//! validation without claiming that an arbitrary input blob is a checked field entry.

use super::*;
use crate::script::scene::{CameraOp, Stage};
use crate::script::scene::presentation::Cast;

struct Fixture {
    entry: Vec<u8>,
    cast: Cast,
    checkpoint: TrainPresentationCheckpoint,
}

fn fixture() -> Fixture {
    let mut cast = Cast::new([30, 36]);
    cast.seed_shake_rng([53, 201]);
    let ground = |_: i16, _: i16, _: Option<u16>| Some(-53.0);
    cast.apply(&Stage::Place { entity: 30, x: 2, y: 537, z: Some(-53), triangle: Some(28) }, 0.0, &ground);
    cast.apply(&Stage::Show { entity: 36, visible: false }, 0.0, &ground);
    cast.apply_loop(30, 0, "standing", 0.0, false);
    cast.apply(&Stage::Camera(CameraOp::Scroll { x: 18, y: 2, frames: 0 }), 0.0, &ground);
    cast.advance(37.25);
    let entry = (0..4096).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let checkpoint = TrainPresentationCheckpoint::new(&entry, [53, 201], f32::from_bits(0x4329_ffe8),
        cast.checkpoint().unwrap()).unwrap();
    Fixture { entry, cast, checkpoint }
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
    let bytes = retained.checkpoint.encode();
    assert!(bytes.len() <= MAX_TRAIN_PRESENTATION_BYTES);
    assert!(bytes.len() < retained.entry.len(), "the full original input is retained separately, not duplicated");
    let decoded = TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &bytes).unwrap();
    assert_eq!(decoded.initial_rng(), [53, 201]);
    assert_eq!(decoded.elapsed_frames().to_bits(), retained.checkpoint.elapsed_frames().to_bits());
    assert_eq!(decoded.checkpoint().encode(), retained.cast.checkpoint().unwrap().encode());
    assert_eq!(decoded.encode(), bytes);
    for at in [0, 1024, retained.entry.len() - 1] {
        let mut changed = retained.entry.clone();
        changed[at] ^= 1;
        assert!(TrainPresentationCheckpoint::decode_bound(&changed, [53, 201], &bytes).is_err(),
            "input byte {at} is part of the binding");
    }
    assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry[..1024], [53, 201], &bytes).is_err(),
        "truncating original input is not an upgrade path");
}

#[test]
fn ready_envelope_rejects_every_truncation_and_changed_byte() {
    let retained = fixture();
    let bytes = retained.checkpoint.encode();
    for end in 0..bytes.len() {
        assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &bytes[..end]).is_err(),
            "truncated at {end}");
    }
    for at in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[at] ^= 1;
        assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &changed).is_err(),
            "changed byte {at}");
    }
    assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201],
        &vec![0; MAX_TRAIN_PRESENTATION_BYTES + 1]).is_err());
}

#[test]
fn valid_checksums_do_not_admit_unknown_headers_invalid_clocks_lengths_or_trailing_data() {
    let retained = fixture();
    let bytes = retained.checkpoint.encode();
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
        assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &signed(body)).is_err());
    }
}

#[test]
fn initial_random_cursors_are_bound_to_immutable_entry_metadata_even_with_a_new_checksum() {
    let retained = fixture();
    let bytes = retained.checkpoint.encode();
    for different in [[0, 0], [53, 200], [54, 201]] {
        assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, different, &bytes).is_err());
    }
    let mut body = bytes[..bytes.len() - 32].to_vec();
    body[42] ^= 1;
    assert!(TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &signed(body)).is_err(),
        "a replacement checksum cannot replace the original RNG cursors");
}

#[test]
fn a_recomputed_envelope_checksum_cannot_replace_an_authored_terminal_actor_position() {
    let retained = fixture();
    let bytes = retained.checkpoint.encode();
    let mut body = bytes[..bytes.len() - 32].to_vec();
    // Outer header is 52 bytes; the inner v1 cast header is seven bytes, then the
    // ordered actor id and explicit position-presence byte precede its x/y/z values.
    assert_eq!(&body[52..56], b"FSTC");
    assert_eq!(&body[59..61], &[30, 1]);
    body[61..65].copy_from_slice(&99.0f32.to_bits().to_le_bytes());
    let changed = TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &signed(body)).unwrap();
    assert!(changed.checkpoint().restore_against(&retained.cast).is_err(),
        "the separately replayed source owns placement; retained bytes own only presentation phase");
}

#[test]
fn a_recomputed_envelope_checksum_cannot_admit_an_unreachable_shake_target() {
    let mut retained = fixture();
    let ground = |_: i16, _: i16, _: Option<u16>| Some(-53.0);
    retained.cast.apply(&Stage::Shake {
        mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2,
    }, 0.0, &ground);
    retained.cast.advance_effects(5.25);
    let checkpoint = TrainPresentationCheckpoint::new(&retained.entry, [53, 201],
        retained.checkpoint.elapsed_frames() + 5.25, retained.cast.checkpoint().unwrap()).unwrap();
    let bytes = checkpoint.encode();
    let original = TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &bytes).unwrap();
    assert!(original.checkpoint().restore_against(&retained.cast).is_ok(),
        "the real effect clock produces a phase accepted by source validation");

    let mut body = bytes[..bytes.len() - 32].to_vec();
    // The cast payload ends with the 34-byte SHAKE state: two 13-byte axes,
    // then the fractional frame. Each axis's target follows five byte fields and start:i16.
    let shake_start = body.len() - 34;
    let y_axis = shake_start + 13;
    assert_eq!(body[y_axis], 1, "the authored Y axis is enabled");
    assert_eq!(body[y_axis + 4], 5, "the authored amplitude is unchanged");
    body[y_axis + 7..y_axis + 9].copy_from_slice(&128i16.to_le_bytes());
    let changed = TrainPresentationCheckpoint::decode_bound(&retained.entry, [53, 201], &signed(body)).unwrap();
    assert!(changed.checkpoint().restore_against(&retained.cast).is_err(),
        "a safely represented target of 128 cannot be produced by the authored amplitude 5");
}
