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
                assert!(audio.frames() > 2_600_000 && audio.frames() < 2_700_000);
                eprintln!(
                    "actual owned PCM frames={} rate={}; video frames={}",
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
