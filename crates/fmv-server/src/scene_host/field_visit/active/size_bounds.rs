//! Encoding capacity of the real private input. The opaque entry, anchor and window
//! identities below are deliberately not an authored gameplay or restoration proof.

use super::*;
use fmv_core::Pos;
use fmv_ff7::story::{NativePartyState, MAX_NATIVE_PARTY_BLOB_BYTES};
use fmv_persist::{FieldVisitEntryV1, MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_BYTES,
    MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES, MAX_SCENE_CHECKPOINT_BYTES};
use fmv_protocol::{MAX_SCENE_IDENTITY_BYTES, MAX_STORY_PROGRESS_BYTES};

#[test]
fn large_visit_and_all_window_identities_fit_the_actual_private_input_and_checkpoint_encodings() {
    let visit = FieldVisitV1 {
        schema_version: 1,
        visit_id: [255; 16],
        revision: u64::MAX - 130,
        field: "\"".repeat(MAX_FIELD_VISIT_FIELD_BYTES),
        source_identity: [255; 32],
        entry: FieldVisitEntryV1 { operation: [255; 16], arrival: vec![255; MAX_FIELD_VISIT_ENTRY_BYTES] },
        anchor: vec![255; MAX_FIELD_VISIT_ANCHOR_BYTES],
        scratch_values: [255; 256],
        scratch_known_masks: [255; 256],
        active_continuation: None,
    };
    assert_eq!((visit.entry.arrival.len(), visit.anchor.len()), (8192, 8192));
    validate_field_visit(&visit).unwrap();
    let visit_bytes = serde_json::to_vec(&visit).unwrap();
    assert!(visit_bytes.len() > 16 * 1024, "this fixture exceeds the former aggregate limit");
    assert!(visit_bytes.len() <= MAX_FIELD_VISIT_BYTES);

    let origin = SceneOrigin { pos: Pos::new(-f32::MAX, f32::MIN_POSITIVE, f32::MAX),
        facing: -f32::MAX, triangle: Some(u32::MAX) };
    origin.validate().unwrap(); // Finite encoding is not a claim of a valid field placement.
    let input = Input {
        version: 1,
        account: AccountId(u64::MAX),
        visit,
        origin: Some(origin),
        events: (0..MAX_SCENE_REPLAY_EVENTS).map(|step| SceneReplayEvent::Window {
            step: step as u32, identity: vec![255; MAX_SCENE_IDENTITY_BYTES], answer: Some(255),
        }).collect(),
    };
    assert_eq!(input.events.len(), 128);
    let replay = input.encode().unwrap();
    assert!(replay.len() <= MAX_SCENE_REPLAY_BYTES);
    assert_eq!(Input::decode(&replay).unwrap(), input);

    let mut native = StoryState::at(u16::MAX).into_native_party().unwrap();
    for bank in [1, 3, 7, 11, 13] {
        for address in 0..=u8::MAX { native.write_byte(bank, address, 255).unwrap(); }
    }
    *native.native_party_mut().unwrap() = NativePartyState::from_parts(
        [6, 7, 8], 7, (u16::MAX, u16::MAX), (u16::MAX, u16::MAX),
    ).unwrap();
    let canonical = native.encode();
    assert_eq!(canonical.len(), MAX_NATIVE_PARTY_BLOB_BYTES);
    assert_eq!(StoryState::decode(&canonical).unwrap(), native);
    assert!(canonical.len() <= MAX_STORY_PROGRESS_BYTES);
    // Also exercise the generic protocol's larger opaque story allowance, without
    // pretending those arbitrary bytes are a canonical FF7 story.
    for original_story in [canonical, vec![255; MAX_STORY_PROGRESS_BYTES]] {
        let checkpoint = SceneCheckpoint {
            instance: [255; 32], scene_key: "\"".repeat(256), source_hash: [255; 32],
            replay_version: REPLAY_VERSION, original_story, original_gil: u32::MAX,
            seed: u32::MAX, replay: replay.clone(), phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
        };
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        assert!(bytes.len() <= MAX_SCENE_CHECKPOINT_BYTES,
            "nested JSON must retain all original visit bytes and acknowledgments: {}", bytes.len());
        assert_eq!(serde_json::from_slice::<SceneCheckpoint>(&bytes).unwrap(), checkpoint);
    }
}
