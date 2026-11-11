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
 {{'index':1,'codec_type':'audio','codec_name':'vorbis','sample_rate':'22050','channels':2,'time_base':'1/22050','start_pts':0}}]}}))
elif '-show_frames' in args and 'a:0' in args:
 print(json.dumps({{'frames':[{{'pts':0,'nb_samples':1,'stream_index':1,'channels':2}}]}}))
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
    if let Some(Event::Prepared { metadata, audio }) = events.first() {
        assert!(std::sync::Arc::ptr_eq(&metadata.audio, &audio.metadata));
        assert_eq!((audio.frames(), audio.metadata.samples()), (1, 1));
        assert_eq!(audio.chunk(0).unwrap().1, [0, 0, 1, 0]);
        assert!(audio.chunk(1).is_none());
        assert!(audio.require_contiguous().is_ok());
    }
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

fn replace_tool(request: &Request, before: &str, after: &str) {
    let original = std::fs::read_to_string(&request.tools.ffprobe).unwrap();
    assert!(original.contains(before), "fixture replacement exists");
    std::fs::write(&request.tools.ffprobe, original.replace(before, after)).unwrap();
}

#[test]
fn timestamped_discontinuous_audio_decodes_but_simple_clock_admission_refuses() {
    let Some((_dir, request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else {
        return;
    };
    replace_tool(&request, "'time_base':'1/22050'", "'time_base':'1/1000'");
    replace_tool(&request, "{'pts':0,'nb_samples':1,'stream_index':1,'channels':2}",
        "{'pts':11,'nb_samples':256,'stream_index':1,'channels':2},{'pts':27,'nb_samples':256,'stream_index':1,'channels':2}");
    replace_tool(&request, "bytes([0,0,1,0])", "bytes([0,0,1,0])*512");
    let mut worker = MovieWorker::start(request).unwrap();
    let (events, result) = drain(&mut worker);
    assert_eq!(
        result,
        Ok(()),
        "noncontiguity does not discard a successful decode"
    );
    let Some(Event::Prepared { audio, .. }) = events.first() else {
        panic!("prepared audio")
    };
    assert_eq!(audio.metadata.initial_pts(), 11);
    assert_eq!(audio.frames(), 512);
    let (first, a) = audio.chunk(0).unwrap();
    let (second, b) = audio.chunk(1).unwrap();
    assert_eq!((first.pts, first.sample_offset, a.len()), (11, 0, 1024));
    assert_eq!((second.pts, second.sample_offset, b.len()), (27, 256, 1024));
    assert!(audio.require_contiguous().is_err());
    assert!(matches!(
        events.last(),
        Some(Event::DecodedEof { frames: 2 })
    ));
}

#[test]
fn pcm_count_must_exactly_match_decoded_audio_inventory_before_preparation() {
    for output in ["bytes()", "bytes(3)", "bytes(8)"] {
        let Some((_dir, request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else {
            return;
        };
        replace_tool(&request, "bytes([0,0,1,0])", output);
        let mut worker = MovieWorker::start(request).unwrap();
        let (events, result) = drain(&mut worker);
        assert!(
            matches!(result, Err(MediaError::Invalid(_))),
            "{output}: {result:?}"
        );
        assert!(
            events.is_empty(),
            "{output} must fail before Prepared/decoded EOF"
        );
    }
    let Some((_dir, request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else {
        return;
    };
    replace_tool(&request, "'nb_samples':1", "'nb_samples':2");
    let mut worker = MovieWorker::start(request).unwrap();
    let (events, result) = drain(&mut worker);
    assert!(matches!(result, Err(MediaError::Invalid(_))));
    assert!(
        events.is_empty(),
        "a short whole-sample decode is not accepted either"
    );
}

#[test]
fn audio_frame_limits_and_malformed_timestamps_refuse_before_pcm_decode() {
    for frame in [
        "{'nb_samples':1,'stream_index':1,'channels':2}",
        "{'pts':0,'nb_samples':0,'stream_index':1,'channels':2}",
        "{'pts':0,'nb_samples':1,'stream_index':1,'channels':2},{'pts':1,'nb_samples':1,'stream_index':1,'channels':2}",
    ] {
        let Some((_dir, mut request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else { return };
        replace_tool(&request, "{'pts':0,'nb_samples':1,'stream_index':1,'channels':2}", frame);
        replace_tool(&request, "sys.stdout.buffer.write(bytes([0,0,1,0]))", "sys.stderr.write('PCM decoder must not run');sys.exit(9)");
        request.limits.max_audio_frames = 1;
        let mut worker = MovieWorker::start(request).unwrap();
        let (events, result) = drain(&mut worker);
        assert!(matches!(result, Err(MediaError::Invalid(_))), "{frame}: {result:?}");
        assert!(events.is_empty());
    }
    for max in [0, 100_001] {
        let Some((_dir, mut request)) = fixture("sys.stdout.buffer.write(b'abcdefgh')") else {
            return;
        };
        request.limits.max_audio_frames = max;
        assert!(matches!(
            MovieWorker::start(request),
            Err(MediaError::Invalid(_))
        ));
    }
}
