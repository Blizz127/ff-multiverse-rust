//! Weapon contacts must survive the same frame sampling as their owner's skeleton.

use super::*;
use fmv_ff7::anim::AnimFrame;
use fmv_ff7::battle_model::animation::animation_from_frames;
use fmv_ff7::model::transform_point;

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x - y).abs() < 0.001)
}

/// A rotating arm with a hand at its end, carrying a weapon whose authored world-space track
/// agrees exactly with that hand. Between frames the hand's arc and the weapon's chord disagree.
fn rotating_arm() -> BattleModelData {
    let body = box_character(&BoxSpec::for_party(0));
    let mut model = body.model.clone();
    model.bones = vec![
        ModelBone { name: "arm".into(), parent: None, length: 100.0 },
        ModelBone { name: "hand".into(), parent: Some(0), length: 0.0 },
    ];
    let mut clips = Vec::new();
    let mut tracks = Vec::new();
    for angles in [[0.0, 90.0], [90.0, 180.0], [180.0, 0.0]] {
        let frames: Vec<_> = angles.iter().enumerate().map(|(i, &angle)| AnimFrame {
            root_rotation: [0.0; 3],
            root_translation: [10.0 + i as f32 * 200.0, 40.0, 30.0 + i as f32 * 60.0],
            rotations: vec![[0.0, angle, 0.0], [0.0; 3]],
        }).collect();
        let clip = animation_from_frames(&frames, 2).unwrap();
        let weapon_frames: Vec<_> = angles.iter().enumerate().map(|(i, &angle)| {
            let hand = transform_point(&clip.pose(&model.bones, i).unwrap()[1], [0.0; 3]);
            AnimFrame {
                root_rotation: [0.0, angle, 0.0],
                root_translation: [hand[0], -hand[1], hand[2]],
                rotations: vec![[0.0; 3]],
            }
        }).collect();
        clips.push(clip);
        tracks.push(animation_from_frames(&weapon_frames, 1).unwrap());
    }
    BattleModelData::new("rotating-arm", model, clips)
        .with_attachment(Attachment::new("held-weapon", body.model, tracks))
}

#[test]
fn an_attached_weapon_keeps_contact_through_idle_action_return_and_loop_boundaries() {
    let data = rotating_arm();
    let a = &data.attachments[0];
    let interpolated_hand = data.clips[0].pose_at(&data.model.bones, 0.5).unwrap()[1];
    let interpolated_weapon = a.clips[0].pose_at(&a.model.bones, 0.5).unwrap()[0];
    let hand = transform_point(&interpolated_hand, [0.0; 3]);
    let weapon = transform_point(&interpolated_weapon, [0.0; 3]);
    assert!((hand[0] - weapon[0]).hypot(hand[2] - weapon[2]) > 25.0,
        "independently interpolated tracks reproduce the detached grip");

    let rig = Rigs::new(None).resolve_optional(&BattleActorModel::Battle(data)).unwrap();
    for (slot, frame) in [(0, 0.25), (0, 1.75), (0, 2.0), (1, 0.999), (1, 1.0), (1, 1.999), (2, 0.0), (2, 0.5), (2, 1.0)] {
        let (body, root) = rig.clip_pose(slot, frame).unwrap();
        let weapons = rig.attachment_clip_poses(slot, frame, root);
        assert!(near(transform_point(&body[1], [0.0; 3]), transform_point(&weapons[0].1[0], [0.0; 3])),
            "slot {slot} at {frame}: grip remains on the hand after the common root travel is removed");
    }
    for frame in [0.25, 0.5, 0.999, 1.0, 1.5, 1.999, 2.0] {
        let seconds = frame / rig.fps;
        let body = rig.idle_pose(seconds).unwrap();
        let weapons = rig.attachment_poses(seconds);
        assert!(near(transform_point(&body[1], [0.0; 3]), transform_point(&weapons[0].1[0], [0.0; 3])),
            "idle at {frame}: the same authored frame, including at the loop boundary");
    }
}

#[test]
fn a_rig_without_weapon_tracks_keeps_its_fractional_poses() {
    let mut data = rotating_arm();
    data.attachments.clear();
    let expected = data.clips[0].pose_at(&data.model.bones, 0.5).unwrap();
    assert_ne!(expected, data.clips[0].pose(&data.model.bones, 0).unwrap());
    let rig = Rigs::new(None).resolve_optional(&BattleActorModel::Battle(data)).unwrap();
    assert_eq!(rig.idle_pose(0.5 / rig.fps).unwrap(), expected);
    let (action, root) = rig.clip_pose(0, 0.5).unwrap();
    assert_eq!(root, [110.0, 60.0]);
    let mut expected_action = expected;
    for bone in &mut expected_action {
        bone[12] -= 110.0;
        bone[14] -= 60.0;
    }
    assert_eq!(action, expected_action);
}

fn same_pose(got: &[[f32; 16]], expected: &[[f32; 16]], context: &str) {
    assert_eq!(got.len(), expected.len(), "{context}");
    for (a, b) in got.iter().zip(expected) {
        assert!(a.iter().zip(b).all(|(x, y)| (*x - *y).abs() < 0.001), "{context}: {a:?} != {b:?}");
    }
}

/// Source-frame regression over the first mission's armed companions and MP. This deliberately
/// compares to the original archive's integer poses, not to a guessed attachment bone.
#[test]
#[ignore = "needs the owner's original battle.lgp (FMV_FF7_DATA)"]
fn owned_armed_battle_rigs_keep_body_and_weapon_on_the_same_authored_frame() {
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is needed; skipping");
        return;
    };
    let mut archive = BattleModels::open(Path::new(&data).join("battle/battle.lgp")).unwrap();
    let models = [
        archive.load_character(BattleCharacter::Cloud).unwrap(),
        archive.load_character(BattleCharacter::Barret).unwrap(),
        archive.load_enemy(16).unwrap(),
    ];
    let mut checked = 0usize;
    for original in models {
        let rig = Rigs::new(None).resolve_optional(&BattleActorModel::Battle(
            ArchiveModels::data_of(original.clone(), ModelRole::Plain),
        )).unwrap();
        assert_eq!(rig.attachments.len(), 1);
        for (slot, track) in original.weapon_tracks.iter().enumerate() {
            if !original.weapon_track_ok[slot] || !original.slot_ok[slot] { continue; }
            let clip = &original.animations[slot];
            assert_eq!(clip.frame_count(), track.frame_count());
            for frame in 0..=clip.frame_count() {
                let source_frame = frame % clip.frame_count();
                let root = clip.frame(source_frame).unwrap().root_translation;
                let mut expected_body = original.pose(slot, source_frame).unwrap();
                let mut expected_weapon = original.weapon_pose(0, slot, source_frame).unwrap();
                for bone in &mut expected_body {
                    bone[12] -= root[0];
                    bone[14] -= root[2];
                }
                expected_weapon[12] -= root[0];
                expected_weapon[14] -= root[2];
                for fraction in [0.0, 0.25, 0.5, 0.75, 0.999] {
                    let at = frame as f32 + fraction;
                    let (body, sampled_root) = rig.clip_pose(slot as Slot, at).unwrap();
                    let attachments = rig.attachment_clip_poses(slot as Slot, at, sampled_root);
                    let context = format!("{} slot {slot} at {at}", original.prefix);
                    assert_eq!(sampled_root, [root[0], root[2]], "{context}");
                    same_pose(&body, &expected_body, &context);
                    same_pose(&attachments[0].1, &[expected_weapon], &context);
                    checked += 1;
                }
            }
        }
        for frame in [0.25, 0.5, 0.75, 1.0, original.animations[0].frame_count() as f32 + 0.5] {
            let source_frame = frame.floor() as usize;
            let body = rig.idle_pose(frame / rig.fps).unwrap();
            let weapons = rig.attachment_poses(frame / rig.fps);
            same_pose(&body, &original.pose(0, source_frame).unwrap(), &original.prefix);
            same_pose(&weapons[0].1, &[original.weapon_pose(0, 0, source_frame).unwrap()], &original.prefix);
        }
    }
    assert!(checked > 5_000, "a broad sweep of owned clips and fractional positions: {checked}");
    eprintln!("{checked} authored body/weapon frame pairs checked");
}
