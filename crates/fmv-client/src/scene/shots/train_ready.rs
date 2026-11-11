//! Restore an actual checked train arrival's retained presentation, including the time the
//! owner spent reading its windows. This is a private offscreen fixture: it admits no live
//! visit, writes no save and treats the port's current clock behavior as its comparison.

use super::*;
use super::super::draw::{camera_shot_from, cast_players_from};
use super::super::player::{PresentationRef, Run, SceneCmd, SceneKey, ScenePlayer, TrainReadyPresentation};
use crate::camera::{CameraInput, FieldCamera};
use fmv_ff7::script::scene::{SceneResources, TrainArrival, TRAIN_ARRIVAL_LANDING};
use fmv_ff7::script::scene::train_arrival::presentation::validate_train_presentation;
use fmv_protocol::SceneReplay;

const CADENCE: [f32; 4] = [0.0173, 0.0117, 0.0391, 0.0256];

fn update_camera(presentation: PresentationRef<'_>, view: &mut FieldView, camera: &mut FieldCamera, dt: f32) {
    view.bounds = camera.update(&CameraInput {
        range: view.range,
        whole: view.whole,
        window: [SIZE.0 as f32, SIZE.1 as f32],
        margin: crate::render::MARGIN_PX,
        own: None,
        scene: Some(camera_shot_from(presentation, view)),
        shake: presentation.shake(),
        dt,
    });
}

/// Use the same model, camera and effects paths for both sides. Each image starts from a
/// fresh field baseline, so ordered background/palette mutations are applied exactly once.
fn picture(
    real: &Real,
    gpu: &mut Offscreen,
    presentation: PresentationRef<'_>,
    background: &[fmv_ff7::background::BackgroundOp],
    camera: &mut FieldCamera,
    name: &str,
) -> (Vec<u8>, [u32; 4]) {
    let mut view = FieldView::ff7_with(&real.adapter, "tin_1", true).expect("train field view");
    view.apply_background_events(background.iter().copied().map(|op| super::super::SceneBackground {
        field: "tin_1".into(), op,
    }));
    assert!(view.background_state().unwrap().visible(2, 3, 1), "the terminal doorway stays closed");
    update_camera(presentation, &mut view, camera, 0.0);
    let mut avatars = Avatars::new(AvatarLibrary::new(Box::new(LgpLoader::new(&real.char_lgp))));
    let players = cast_players_from(presentation);
    let stage = view.stage.as_ref().expect("the authored train camera remains available");
    avatars.frame(&view, stage, &players, 0.0);
    let built = avatars.frame(&view, stage, &players, 0.0);
    assert!(built.markers.is_empty(), "the terminal cast uses its owned models");
    assert!(built.anchors.contains_key(&cast_who(30)), "Cloud's authored terminal body is present");
    let scene = Scene {
        bounds: view.bounds, triangles: &[], players: &[], fixture_banner: false,
        backdrop: view.backdrop.as_ref(),
    };
    let rgba = gpu.render_field_effects(SIZE.0, SIZE.1, &scene, Some(&built.frame),
        &DrawList::new(), camera.clip_range(view.range), presentation.fade());
    save(name, SIZE.0, SIZE.1, &rgba);
    (rgba, [view.bounds.min.x, view.bounds.min.y, view.bounds.max.x, view.bounds.max.y].map(f32::to_bits))
}

#[test]
#[ignore = "needs owned FF7 data and an offscreen GPU; actual retained train-arrival presentation"]
fn offscreen_real_ff7_train_ready_restores_actual_window_dwell_and_terminal_phase() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("tin_1").expect("owned train field");
    let field = area.field_file();
    // Unknown story flags remain unknown. Only the actual entry profile clears scratch.
    let story = StoryState::decode(&[3, 48, 0, 0, 0]).expect("sparse original entry story");
    let play = TrainArrival::new(field, &story, SEED, SceneResources { gil: Some(600) },
        TRAIN_ARRIVAL_LANDING).expect("reviewed train entry").into_play();
    let replay = SceneReplay { source: play.replay_identity().fingerprint, origin: None, events: vec![] };
    let mut live = ScenePlayer::with_replay_seeded(
        Run { id: SceneRunId(1), role: SceneRole::Owner,
            participants: (1..=3).map(|entity| SceneParticipant { entity: EntityId(entity), spot: (entity - 1) as u8 }).collect() },
        play.def().clone(), Box::new(play), false, &replay, [53, 201],
    ).expect("the uninterrupted checked arrival starts");
    assert!(live.capture_train_ready().is_err(), "entry is not completion");
    let mut live_view = FieldView::ff7_with(&real.adapter, "tin_1", true).unwrap();
    let mut live_camera = FieldCamera::default();

    let mut held = [0u32; 2];
    let mut reported = Vec::new();
    let mut end_reports = 0;
    let mut changing_shake = BTreeSet::new();
    for tick in 0..6000 {
        if live.played_out() { break; }
        let dt = CADENCE[tick % CADENCE.len()];
        update_camera(live.presentation(), &mut live_view, &mut live_camera, dt);
        if live.has_window() {
            let index = usize::try_from(live.lines()).unwrap();
            assert!(index < 2, "the real entry has exactly two windows");
            held[index] += 1;
            assert!(live.capture_train_ready().is_err(), "an unanswered window is not ready");
            if held[index] <= [113, 197][index] {
                let clock = live.camera_clock();
                live.advance(dt);
                assert_eq!(live.camera_clock(), clock, "reading holds script/camera time");
                changing_shake.insert(live.shake().map(|v| v as i32));
            } else {
                live.key(SceneKey::Confirm);
            }
        } else {
            live.advance(dt);
        }
        for command in live.take_commands() {
            match command {
                SceneCmd::Step { step, answer: None, .. } => reported.push(step),
                SceneCmd::End { skipped: false, .. } => end_reports += 1,
                other => panic!("unexpected train command: {other:?}"),
            }
        }
        live.take_events();
        live.take_sounds();
    }
    assert!(live.played_out(), "the whole foreground tail finishes");
    assert_eq!(reported, [0, 1]);
    assert_eq!(end_reports, 1);
    assert!(held[0] > 113 && held[1] > 197, "both windows include real elapsed reading time");
    assert!(changing_shake.len() > 1, "the retained effect history includes visible motion during dialogue");
    assert_eq!(live.counts().anims_skipped, 0);
    assert_eq!(live.counts().moves_skipped, 0);
    assert_eq!(live.camera_clock().state, super::super::stage::CameraState::Scroll { x: 18, y: 2, frames: 0 });
    assert_eq!(live.cast_draws().iter().map(|actor| actor.entity).collect::<BTreeSet<_>>(),
        BTreeSet::from([30, 31, 33, 34, 35]), "the departing passengers and employee stay hidden");
    let cloud = live.actor(30).unwrap();
    assert_eq!(cloud.pos(), Some([2.0, 537.0, -53.0]));
    assert_eq!(cloud.triangle(), Some(28));

    // Capture the actual delayed handoff, rather than substituting the first terminal tick.
    for dt in [0.137, 0.0517, 0.4023] {
        live.advance(dt);
        update_camera(live.presentation(), &mut live_view, &mut live_camera, dt);
    }
    assert!(live.take_commands().is_empty());
    assert!(live.take_sounds().is_empty());
    assert!(crate::mods::take_voices().is_empty(), "the original source has no Echo-S cues");
    let captured = live.capture_train_ready().expect("capture the actual held ready boundary");
    let entry = captured.entry_record().to_vec();
    let bytes = captured.encode().expect("lossless bounded presentation record");
    validate_train_presentation(field, &entry, [53, 201], &bytes)
        .expect("the shared server-side validator accepts this actual displayed phase");
    let immutable_entry = captured.encode_entry().expect("retain original entry and its initial effect stream");
    let mut restored = TrainReadyPresentation::reconstruct_entry(field, &immutable_entry, &bytes)
        .expect("restore the same checked entry after its active checkpoint is gone");
    assert_eq!(restored.encode().unwrap(), bytes, "every retained phase bit survives reconstruction");
    assert_eq!(restored.background_ops(), live.background_ops(), "the journal comes from exact source replay");
    assert!(live.take_commands().is_empty() && live.take_sounds().is_empty());
    assert!(crate::mods::take_voices().is_empty(), "reconstruction emits no historical voice cue or stop");
    eprintln!("train ready: entry {} bytes, presentation {} bytes, combined {} bytes; dialogue ticks {:?}",
        entry.len(), bytes.len(), entry.len() + bytes.len(), held);
    // Private cross-layer fixtures come from this actual displayed run. A server test can
    // validate the same input and clocks without inventing a canonical no-dwell snapshot.
    let output = crate::ui::testkit::shot_dir();
    std::fs::create_dir_all(&output).expect("private screenshot/fixture directory");
    std::fs::write(output.join("train-ready-entry.bin"), &entry).expect("retained original input fixture");
    std::fs::write(output.join("train-ready-immutable-entry.bin"), &immutable_entry)
        .expect("durable original input and initial shake metadata fixture");
    std::fs::write(output.join("train-ready-presentation.bin"), &bytes).expect("actual retained presentation fixture");
    std::fs::write(output.join("train-ready-initial-shake.bin"), [53, 201]).expect("immutable initial shake cursor fixture");
    std::fs::write(output.join("train-ready-fixture.txt"), format!(
        "private owned-source test fixture; no live visit admission\nentry_bytes={}\npresentation_bytes={}\ninitial_shake=53,201\nwindow_hold_updates={},{}\nentry_blake3={}\npresentation_blake3={}\n",
        entry.len(), bytes.len(), held[0], held[1], blake3::hash(&entry).to_hex(), blake3::hash(&bytes).to_hex(),
    )).expect("fixture receipt");
    eprintln!("retained train-ready fixtures in {}", output.display());

    let mut restored_camera = FieldCamera::default();
    let mut restored_view = FieldView::ff7_with(&real.adapter, "tin_1", true).unwrap();
    let uninterrupted = picture(&real, &mut gpu, live.presentation(), live.background_ops(), &mut live_camera, "train-ready-uninterrupted");
    let rebuilt = picture(&real, &mut gpu, restored.presentation(), restored.background_ops(), &mut restored_camera, "train-ready-restored");
    assert_eq!(uninterrupted, rebuilt, "the restored cast, backdrop, camera and effects produce the exact terminal image");
    for tick in 0..97 {
        let dt = CADENCE[(tick + 1) % CADENCE.len()];
        live.advance(dt);
        restored.advance_effects(dt);
        update_camera(live.presentation(), &mut live_view, &mut live_camera, dt);
        update_camera(restored.presentation(), &mut restored_view, &mut restored_camera, dt);
        assert_eq!(restored.encode().unwrap(), live.capture_train_ready().unwrap().encode().unwrap(),
            "future effect phase remains exact after fractional update {tick}");
    }
    let uninterrupted = picture(&real, &mut gpu, live.presentation(), live.background_ops(), &mut live_camera, "train-ready-uninterrupted-later");
    let rebuilt = picture(&real, &mut gpu, restored.presentation(), restored.background_ops(), &mut restored_camera, "train-ready-restored-later");
    assert_eq!(uninterrupted, rebuilt, "the next retained effect samples stay visually identical");
    std::fs::write(output.join("train-ready-presentation-later.bin"), restored.encode().unwrap())
        .expect("later displayed phase fixture");
    assert!(live.take_commands().is_empty() && live.take_sounds().is_empty());
    assert!(crate::mods::take_voices().is_empty());

    // These are rejected before they can become a passive picture. A changed source is
    // checked against the same complete retained entry, not merely a matching scene id.
    for length in [0, 1, 8, bytes.len() / 2, bytes.len() - 1] {
        assert!(TrainReadyPresentation::reconstruct(field, &entry, [53, 201], &bytes[..length]).is_err());
    }
    let mut changed = bytes.clone();
    let at = changed.len() / 2;
    changed[at] ^= 1;
    assert!(TrainReadyPresentation::reconstruct(field, &entry, [53, 201], &changed).is_err());
    assert!(TrainReadyPresentation::reconstruct(field, &entry, [53, 200], &bytes).is_err(),
        "the saved entry's initial shake cursors cannot be replaced");
    let cargo = real.adapter.load_field("cargoin").unwrap();
    assert!(TrainReadyPresentation::reconstruct(cargo.field_file(), &entry, [53, 201], &bytes).is_err());
    assert!(crate::mods::take_voices().is_empty(), "refusal is silent too");
}
