//! Explicit owned-data acceptance. No media bytes are retained or printed.
use fmv_media::{Event, Limits, MovieWorker, Request, Tools};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires explicit FMV_OPENING_MOVIE, FMV_FFMPEG and FMV_FFPROBE owned local paths"]
fn opening_movie_decodes_its_actual_timestamps_audio_and_all_frames_before_eof() {
    let path = |name| {
        PathBuf::from(
            std::env::var_os(name)
                .unwrap_or_else(|| panic!("{name} must identify the explicit owned input/tool")),
        )
    };
    let mut worker = MovieWorker::start(Request {
        path: path("FMV_OPENING_MOVIE"),
        tools: Tools {
            ffmpeg: path("FMV_FFMPEG"),
            ffprobe: path("FMV_FFPROBE"),
        },
        limits: Limits::default(),
    })
    .expect("bounded worker");
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut prepared = None;
    let mut count = 0;
    let mut previous_pts = None;
    loop {
        assert!(
            Instant::now() < deadline,
            "no successful decoder EOF before timeout"
        );
        match worker.poll().expect("actual owned decode succeeds") {
            Some(Event::Prepared { metadata, audio }) => {
                let audio = audio.expect("the opening has audio");
                assert!(prepared.is_none());
                assert_eq!(
                    (
                        metadata.video.width,
                        metadata.video.height,
                        metadata.video.frames.len()
                    ),
                    (1280, 896, 1792)
                );
                assert_eq!(
                    (
                        metadata.video.time_base.numerator,
                        metadata.video.time_base.denominator
                    ),
                    (1, 1000)
                );
                assert_eq!(
                    metadata
                        .video
                        .frames
                        .iter()
                        .take(5)
                        .map(|f| f.pts)
                        .collect::<Vec<_>>(),
                    [0, 67, 133, 200, 267]
                );
                assert_eq!(audio.sample_rate, 22_050);
                assert_eq!(audio.frames(), 2_632_960);
                assert_eq!(audio.metadata.samples(), 2_632_960);
                assert_eq!(audio.pcm_le.len(), 10_531_840);
                assert_eq!(audio.metadata.frames().len(), 5299);
                assert_eq!(audio.metadata.stream_start_pts(), 0);
                assert_eq!(audio.metadata.initial_pts(), 11);
                assert_eq!(
                    audio.metadata.time_base(),
                    fmv_media::TimeBase {
                        numerator: 1,
                        denominator: 1000
                    }
                );
                let last = audio.metadata.frames().last().unwrap();
                assert_eq!(
                    (last.pts, last.samples, last.sample_offset),
                    (119401, 256, 2_632_704)
                );
                let mut samples = 0u64;
                for (ordinal, frame) in audio.metadata.frames().iter().enumerate() {
                    assert_eq!(frame.sample_offset, samples);
                    assert_eq!(
                        audio.chunk(ordinal).unwrap().1.len(),
                        frame.samples as usize * 4
                    );
                    samples += u64::from(frame.samples);
                }
                assert_eq!(samples, 2_632_960);
                // These are decoder7.0.2 observations, not a native audio-origin policy.
                // Matroska lace timestamps are synthesized with truncated packet durations.
                let timing = audio.metadata.continuity();
                assert_eq!(
                    (
                        timing.minimum_origin_numerator,
                        timing.maximum_origin_numerator
                    ),
                    (32_200, 242_550)
                );
                assert_eq!(
                    (timing.origin_denominator, timing.one_tick_numerator),
                    (22_050_000, 22_050)
                );
                assert!(!timing.is_compatible());
                assert!(
                    audio.require_contiguous().is_err(),
                    "successful chunked decode does not authorize a simple sample clock"
                );
                eprintln!(
                    "actual owned PCM frames={} rate={}; video frames={}; audio PTS continuity={timing:?}",
                    audio.frames(),
                    audio.sample_rate,
                    metadata.video.frames.len()
                );
                prepared = Some(metadata);
            }
            Some(Event::Frame(frame)) => {
                assert!(prepared.is_some());
                assert_eq!(frame.ordinal, count);
                assert_eq!(frame.rgba.len(), 1280 * 896 * 4);
                if let Some(previous) = previous_pts {
                    assert!(frame.timing.pts > previous);
                }
                previous_pts = Some(frame.timing.pts);
                count += 1;
            }
            Some(Event::DecodedEof { frames }) => {
                assert_eq!((count, frames), (1792, 1792));
                break;
            }
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}
