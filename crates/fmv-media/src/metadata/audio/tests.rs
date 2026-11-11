use super::*;
use serde_json::json;

fn read(rate: u32, base: TimeBase, frames: &[(i64, u32)]) -> AudioMetadata {
    let value = json!({"frames": frames.iter().map(|(pts,samples)| json!({
        "pts":pts,"nb_samples":samples,"stream_index":1,"channels":2
    })).collect::<Vec<_>>()});
    parse(
        AudioStream {
            index: 1,
            sample_rate: rate,
            time_base: base,
        },
        &serde_json::to_vec(&value).unwrap(),
        &Limits::default(),
    )
    .unwrap()
}

const MS: TimeBase = TimeBase {
    numerator: 1,
    denominator: 1000,
};

#[test]
fn irregular_audio_chunks_keep_nonzero_start_and_exact_sample_ranges() {
    let audio = read(1000, MS, &[(11, 3), (14, 7), (21, 2)]);
    assert_eq!(audio.stream_start_pts(), 0);
    assert_eq!(audio.initial_pts(), 11);
    assert_eq!(
        audio.frames(),
        &[
            AudioFrameTiming {
                pts: 11,
                samples: 3,
                sample_offset: 0
            },
            AudioFrameTiming {
                pts: 14,
                samples: 7,
                sample_offset: 3
            },
            AudioFrameTiming {
                pts: 21,
                samples: 2,
                sample_offset: 10
            },
        ]
    );
    assert_eq!((audio.samples(), audio.pcm_bytes().unwrap()), (12, 48));
    let report = audio.require_contiguous().unwrap();
    assert_eq!(report.minimum_origin_numerator, 11_000);
    assert_eq!(report.maximum_origin_numerator, 11_000);
    assert_eq!(report.origin_denominator, 1_000_000);
}

#[test]
fn global_quantization_is_compatible_but_small_per_frame_drift_is_not() {
    let global = read(
        44_100,
        MS,
        &[
            (0, 1024),
            (23, 1024),
            (46, 1024),
            (69, 1024),
            (92, 1024),
            (116, 1024),
        ],
    );
    assert!(global.require_contiguous().is_ok());
    let drift = read(
        44_100,
        MS,
        &[
            (0, 1024),
            (23, 1024),
            (46, 1024),
            (69, 1024),
            (92, 1024),
            (115, 1024),
        ],
    );
    assert_eq!(drift.continuity().first_inconsistent_frame, Some(5));
    assert!(
        drift.require_contiguous().is_err(),
        "adjacent discrepancies under one tick cannot accumulate forever"
    );
}

#[test]
fn a_gap_overlap_backward_pts_and_zero_pts_duplicates_are_preserved_not_repaired() {
    for frames in [
        &[(0, 480), (12, 480)][..],
        &[(0, 480), (8, 480)],
        &[(10, 480), (0, 480)],
    ] {
        let audio = read(48_000, MS, frames);
        assert_eq!(
            audio
                .frames()
                .iter()
                .map(|f| (f.pts, f.samples))
                .collect::<Vec<_>>(),
            frames
        );
        assert_eq!(audio.continuity().first_inconsistent_frame, Some(1));
        assert!(audio.require_contiguous().is_err());
    }
    let short = read(48_000, MS, &[(0, 1), (0, 1), (0, 1)]);
    assert!(
        short.require_contiguous().is_ok(),
        "a coarse timebase can assign equal PTS to separate sample frames"
    );
    let negative = read(48_000, MS, &[(-10, 480), (0, 480)]);
    assert_eq!(
        negative.initial_pts(),
        -10,
        "decoded preroll is retained, not zeroed"
    );
    assert!(
        negative.require_contiguous().is_ok(),
        "origin policy remains the consumer's explicit responsibility"
    );
}

#[test]
fn tolerance_is_one_source_tick_as_a_closed_observation_bound() {
    assert!(read(48_000, MS, &[(0, 48_000), (1001, 1)])
        .require_contiguous()
        .is_ok());
    assert!(read(48_000, MS, &[(0, 48_000), (1002, 1)])
        .require_contiguous()
        .is_err());
    let sample_clock = TimeBase {
        numerator: 1,
        denominator: 48_000,
    };
    assert!(read(48_000, sample_clock, &[(0, 48_000), (48_001, 1)])
        .require_contiguous()
        .is_ok());
    assert!(read(48_000, sample_clock, &[(0, 48_000), (48_002, 1)])
        .require_contiguous()
        .is_err());
    let coarse = read(
        48_000,
        TimeBase {
            numerator: 2,
            denominator: 1000,
        },
        &[(0, 480), (6, 480)],
    );
    assert_eq!(
        coarse.require_contiguous().unwrap().one_tick_numerator,
        96_000
    );
}

#[test]
fn missing_conflicting_noninteger_or_changing_audio_metadata_refuses() {
    let frame = json!({"pts":0,"best_effort_timestamp":0,"nb_samples":2,"stream_index":1,"channels":2,"sample_rate":22050});
    let run = |frame: Value| {
        parse(
            AudioStream {
                index: 1,
                sample_rate: 22050,
                time_base: MS,
            },
            &serde_json::to_vec(&json!({"frames":[frame]})).unwrap(),
            &Limits::default(),
        )
    };
    for (key, value) in [
        ("pts", json!(0.5)),
        ("pts", json!("N/A")),
        ("best_effort_timestamp", json!(1)),
        ("nb_samples", json!(0)),
        ("nb_samples", json!(-1)),
        ("nb_samples", json!(1.5)),
        ("nb_samples", json!(4294967296u64)),
        ("stream_index", json!(0)),
        ("channels", json!(1)),
        ("sample_rate", json!(44100)),
        ("media_type", json!("video")),
    ] {
        let mut changed = frame.clone();
        changed[key] = value;
        assert!(run(changed).is_err(), "{key}");
    }
    for key in ["nb_samples", "stream_index", "channels"] {
        let mut changed = frame.clone();
        changed.as_object_mut().unwrap().remove(key);
        assert!(run(changed).is_err(), "missing {key}");
    }
    let mut changed = frame;
    changed.as_object_mut().unwrap().remove("pts");
    changed
        .as_object_mut()
        .unwrap()
        .remove("best_effort_timestamp");
    assert!(run(changed).is_err());
}

#[test]
fn audio_inventory_bounds_precede_pcm_allocation_and_arithmetic_is_wide() {
    let json = serde_json::to_vec(&json!({"frames":[
        {"pts":0,"nb_samples":2,"stream_index":1,"channels":2},
        {"pts":2,"nb_samples":2,"stream_index":1,"channels":2}
    ]}))
    .unwrap();
    let run = |limits: &Limits| {
        parse(
            AudioStream {
                index: 1,
                sample_rate: 1000,
                time_base: MS,
            },
            &json,
            limits,
        )
    };
    assert!(run(&Limits {
        max_audio_frames: 1,
        ..Limits::default()
    })
    .is_err());
    assert!(run(&Limits {
        max_audio_bytes: 15,
        ..Limits::default()
    })
    .is_err());
    assert_eq!(
        run(&Limits {
            max_audio_bytes: 16,
            ..Limits::default()
        })
        .unwrap()
        .samples(),
        4
    );
    let extremes = read(
        192_000,
        TimeBase {
            numerator: u32::MAX,
            denominator: u32::MAX,
        },
        &[(i64::MIN, 1), (i64::MAX, 1)],
    );
    assert!(!extremes.continuity().is_compatible());
    assert_eq!(extremes.frames().first().unwrap().pts, i64::MIN);
    assert_eq!(extremes.frames().last().unwrap().pts, i64::MAX);
    assert!(parse(
        AudioStream {
            index: 1,
            sample_rate: 1000,
            time_base: MS
        },
        br#"{"frames":[]}"#,
        &Limits::default()
    )
    .is_err());
}
