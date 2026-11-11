#![cfg(unix)]

use fmv_media::{Event, Limits, MediaError, MovieWorker, Request, Tools};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fixture(video_tail: &str) -> Option<(tempfile::TempDir, Request)> {
    let python = ["/usr/bin/python3", "/bin/python3"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())?;
    let dir = tempfile::tempdir().expect("fixture directory");
    let tool = dir.path().join("fixture-decoder");
    let script = format!(
        r##"#!{python}
import json,sys
args=sys.argv[1:]
if '-show_streams' in args:
 print(json.dumps({{'streams':[
 {{'index':0,'codec_type':'video','codec_name':'vp8','width':1,'height':1,'time_base':'1/1000','start_pts':0}},
 {{'index':1,'codec_type':'audio','codec_name':'vorbis','sample_rate':'22050','channels':2,'start_pts':0}}]}}))
elif '-show_frames' in args:
 print(json.dumps({{'frames':[
 {{'pts':0,'duration':37,'width':1,'height':1}},{{'pts':37,'duration':51,'width':1,'height':1}}]}}))
elif 's16le' in args:
 sys.stdout.buffer.write(bytes([0,0,1,0]))
else:
 {video_tail}
"##
    );
    std::fs::write(&tool, script).expect("fixture tool");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700))
        .expect("fixture executable");
    let path = dir.path().join("input.owned");
    std::fs::write(&path, [1u8]).expect("fixture input");
    let request = Request {
        path,
        tools: Tools {
            ffmpeg: tool.clone(),
            ffprobe: tool,
        },
        limits: Limits {
            lifetime: Duration::from_secs(5),
            ..Limits::default()
        },
    };
    Some((dir, request))
}

fn drain(worker: &mut MovieWorker) -> (Vec<Event>, Result<(), MediaError>) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut events = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "worker stopped without an explicit result"
        );
        match worker.poll() {
            Ok(Some(event)) => {
                let eof = matches!(event, Event::DecodedEof { .. });
                events.push(event);
                if eof {
                    return (events, Ok(()));
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(error) => return (events, Err(error)),
        }
    }
}

#[test]
fn public_worker_preserves_pts_and_reports_only_verified_decoder_eof() {
    let Some((_dir, request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else {
        return;
    };
    let mut worker = MovieWorker::start(request).expect("worker");
    let (events, result) = drain(&mut worker);
    assert_eq!(result, Ok(()));
    assert!(matches!(events.first(), Some(Event::Prepared { .. })));
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                Event::Frame(frame) => Some((frame.ordinal, frame.timing.pts)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [(0, 0), (1, 37)]
    );
    assert!(matches!(
        events.last(),
        Some(Event::DecodedEof { frames: 2 })
    ));
}

#[test]
fn truncated_or_failed_or_error_diagnostic_decodes_never_emit_eof() {
    for tail in [
        "sys.stdout.buffer.write(b'abcdefg')",
        "sys.stdout.buffer.write(b'abcdefgh'); sys.exit(7)",
        "sys.stdout.buffer.write(b'abcdefgh'); sys.stderr.write('corrupt packet')",
    ] {
        let Some((_dir, request)) = fixture(tail) else {
            return;
        };
        let mut worker = MovieWorker::start(request).expect("worker");
        let (events, result) = drain(&mut worker);
        assert!(result.is_err(), "{tail}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::DecodedEof { .. })),
            "{tail}"
        );
    }
}

#[test]
fn cancellation_during_a_partial_video_frame_is_cancelled_not_success_or_truncation() {
    let Some((_dir, request)) =
        fixture("sys.stdout.buffer.write(b'ab'); sys.stdout.flush(); import time; time.sleep(30)")
    else {
        return;
    };
    let mut worker = MovieWorker::start(request).expect("worker");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        if matches!(
            worker.poll().expect("preparation"),
            Some(Event::Prepared { .. })
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(50));
    worker.cancel();
    let (events, result) = drain(&mut worker);
    assert_eq!(result, Err(MediaError::Cancelled));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Event::DecodedEof { .. })));
}

#[test]
fn relative_tool_paths_are_never_resolved_through_path() {
    let request = Request {
        path: PathBuf::from("/input"),
        tools: Tools {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
        },
        limits: Limits::default(),
    };
    assert!(matches!(
        MovieWorker::start(request),
        Err(MediaError::Invalid(_))
    ));
}
