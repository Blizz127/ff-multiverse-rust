//! The controller is driven by real worker messages, manual mixer consumption and
//! explicit offscreen readback acknowledgements. No real audio device is opened.

#![cfg(unix)]

use super::*;
use crate::render::movie::{DisplayAspect, DisplayPolicy, MovieFit, MovieViewport};
use crate::ui::testkit::Offscreen;
use fmv_audio::{AudioConfig, AudioEngine, MovieAudioState, MoviePcm};
use fmv_media::{Limits, Request, Tools};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RATE: u32 = 22_050;

pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub(crate) request: Request,
    pub(crate) digest: blake3::Hash,
}

impl Fixture {
    pub(crate) fn new(audio_frames: u32, timings: &[(u32, u32)], video_body: Option<&str>) -> Option<Self> {
        Self::build(Some(audio_frames), timings, video_body)
    }

    /// A movie that is one video stream and no audio (the audio branches of the fake decoder fail loudly).
    pub(crate) fn silent(timings: &[(u32, u32)], video_body: Option<&str>) -> Option<Self> {
        Self::build(None, timings, video_body)
    }

    fn build(audio: Option<u32>, timings: &[(u32, u32)], video_body: Option<&str>) -> Option<Self> {
        let audio_frames = audio.unwrap_or(0);
        let python = ["/usr/bin/python3", "/bin/python3"].into_iter().find(|p| Path::new(p).is_file())?;
        let dir = tempfile::tempdir().expect("synthetic movie directory");
        let tool = dir.path().join("fixture-decoder");
        let path = dir.path().join("synthetic.owned");
        let bytes = b"synthetic movie controller fixture";
        std::fs::write(&path, bytes).unwrap();
        let mut streams = json!({"streams": [
            {"index":0,"codec_type":"video","codec_name":"vp8","width":1,"height":1,"time_base":"1/1000","start_pts":0},
            {"index":1,"codec_type":"audio","codec_name":"vorbis","sample_rate":"22050","channels":2,"time_base":"1/22050","start_pts":0}
        ]});
        if audio.is_none() {
            streams["streams"].as_array_mut().unwrap().pop();
        }
        let video = json!({"frames": timings.iter().map(|&(pts, duration)| {
            json!({"pts":pts,"duration":duration,"width":1,"height":1})
        }).collect::<Vec<_>>()});
        let audio_json = json!({"frames": [{"pts":0,"nb_samples":audio_frames,"stream_index":1,"channels":2}]});
        let default_body = format!("sys.stdout.buffer.write(bytes([20,40,60,255])*{})", timings.len());
        let body = video_body.unwrap_or(&default_body).lines().map(|line| format!(" {line}\n")).collect::<String>();
        let (audio_branch, pcm_branch) = if audio.is_some() {
            (format!("print(r'''{audio_json}''')"), format!("sys.stdout.buffer.write(bytes([0,32,0,32])*{audio_frames})"))
        } else {
            ("sys.exit(1)".to_string(), "sys.exit(1)".to_string())
        };
        let script = format!(
            "#!{python}\nimport sys,json,time,pathlib\nargs=sys.argv[1:]\n\
             if '-show_streams' in args:\n print(r'''{streams}''')\n\
             elif '-show_frames' in args and 'a:0' in args:\n {audio_branch}\n\
             elif '-show_frames' in args:\n print(r'''{video}''')\n\
             elif 's16le' in args:\n {pcm_branch}\n\
             else:\n{body}"
        );
        std::fs::write(&tool, script).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            path, tools: Tools { ffmpeg: tool.clone(), ffprobe: tool },
            limits: Limits { queued_frames: 1, lifetime: Duration::from_secs(10), ..Limits::default() },
        };
        Some(Self { _dir: dir, request, digest: blake3::hash(bytes) })
    }

    pub(crate) fn normal() -> Option<Self> { Self::new(8_820, &[(0, 100), (100, 150), (250, 100)], None) }

    pub(crate) fn controller(&self, stream: u64) -> MovieController {
        MovieController::prepare(stream, self.request.clone(), Admission::Synthetic { digest: self.digest }).unwrap()
    }

    /// The same movie under the general admission: no pinned hash, whatever its bytes.
    pub(crate) fn general(&self, stream: u64) -> MovieController {
        MovieController::prepare(stream, self.request.clone(), Admission::General).unwrap()
    }

    pub(crate) fn path(&self) -> &Path {
        &self.request.path
    }
}

fn engine() -> Arc<AudioEngine> {
    Arc::new(AudioEngine::start_manual(AudioConfig { sample_rate: RATE, ..AudioConfig::default() }))
}

fn until(controller: &mut MovieController, now: Duration, ready: impl Fn(&MovieController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !ready(controller) {
        assert!(Instant::now() < deadline, "controller did not reach expected state: {:?}", controller.phase());
        controller.poll(now).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn prepared(controller: &mut MovieController) {
    until(controller, Duration::ZERO, |c| c.metadata().is_some() && c.first_frame_available());
    assert_eq!(controller.phase(), Phase::Prepared);
}

fn time_is(controller: &MovieController, numerator: u128, denominator: u128) {
    let position = controller.position();
    assert_eq!(position.numerator * denominator, numerator * position.denominator, "{position:?}");
}

#[test]
fn admission_checks_the_selected_file_before_starting_the_decoder() {
    let Some(fixture) = Fixture::normal() else { return };
    assert!(MovieController::prepare(1, fixture.request.clone(), Admission::Synthetic { digest: blake3::hash(b"wrong") }).is_err());
    assert!(MovieController::prepare(1, fixture.request.clone(), Admission::OpeningFfnxPcmZeroOrigin).is_err());
    let first = fixture.controller(7);
    let second = fixture.controller(7);
    assert_eq!((first.id().stream, second.id().stream), (7, 7));
    assert_ne!(first.id().generation, second.id().generation, "reusing a caller stream cannot reuse a presentation identity");
}

#[test]
fn prepared_metadata_and_elapsed_wall_time_cannot_start_an_unavailable_first_frame() {
    let Some(fixture) = Fixture::new(8_820, &[(0, 100), (100, 150), (250, 100)], Some("time.sleep(30)")) else { return };
    let audio = engine();
    let mut controller = fixture.controller(1);
    until(&mut controller, Duration::ZERO, |c| c.metadata().is_some());
    assert!(!controller.first_frame_available());
    assert!(controller.play(&audio, Duration::ZERO, 1.0).is_err());
    assert!(controller.audio_handle().is_none());
    audio.pump(22_050);
    controller.poll(Duration::from_secs(300)).unwrap();
    time_is(&controller, 0, 1);
    controller.cancel();
    assert_eq!(controller.phase(), Phase::Cancelled);
    assert!(!matches!(controller.phase(), Phase::Completed));
}

#[test]
fn a_stalled_clock_bounds_decoded_frames_and_keeps_the_first_frame_available() {
    let timings = (0..40).map(|index| (index * 100, 100)).collect::<Vec<_>>();
    let Some(fixture) = Fixture::new(88_200, &timings, None) else { return };
    let mut controller = fixture.controller(2);
    prepared(&mut controller);
    until(&mut controller, Duration::ZERO, |c| c.pending_frames() == 2);
    for tick in 0..100 {
        controller.poll(Duration::from_secs(tick)).unwrap();
        assert!(controller.pending_frames() <= 2, "only the due frame and next future frame may be retained");
        assert!(controller.first_frame_available());
        assert_eq!(controller.phase(), Phase::Prepared);
        time_is(&controller, 0, 1);
    }
    controller.cancel();
    assert_eq!(controller.pending_frames(), 0);
}

#[test]
fn audio_off_refuses_play_without_consuming_the_prepared_movie() {
    let Some(fixture) = Fixture::normal() else { return };
    let audio = engine();
    let off = Arc::new(AudioEngine::start_manual(AudioConfig { mode: fmv_audio::Mode::Off, ..AudioConfig::default() }));
    let mut controller = fixture.controller(2);
    prepared(&mut controller);
    assert!(controller.play(&off, Duration::ZERO, 1.0).is_err());
    assert_eq!(controller.phase(), Phase::Prepared);
    assert!(controller.audio_handle().is_none());
    assert!(controller.first_frame_available());
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    audio.pump(2_205);
    controller.poll(Duration::ZERO).unwrap();
    time_is(&controller, 1, 10);
    controller.cancel();
}

#[test]
fn movie_position_uses_consumed_source_intervals_and_pause_never_invents_time() {
    let Some(fixture) = Fixture::normal() else { return };
    let audio = engine();
    let mut controller = fixture.controller(1);
    prepared(&mut controller);
    assert!(controller.play(&audio, Duration::ZERO, f32::NAN).is_err());
    assert!(controller.audio_handle().is_none());
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    controller.poll(Duration::from_secs(100)).unwrap();
    time_is(&controller, 0, 1);
    let samples = audio.pump(2_205);
    assert!(samples.iter().any(|sample| *sample > 0.0));
    controller.poll(Duration::from_secs(100)).unwrap();
    time_is(&controller, 1, 10);
    controller.set_paused(Duration::from_secs(100), true).unwrap();
    audio.pump(22_050);
    controller.poll(Duration::from_secs(200)).unwrap();
    assert_eq!(controller.phase(), Phase::Paused);
    time_is(&controller, 1, 10);
    controller.set_paused(Duration::from_secs(200), false).unwrap();
    audio.pump(2_205);
    controller.poll(Duration::from_secs(200)).unwrap();
    time_is(&controller, 1, 5);
    let before = (controller.phase(), controller.position());
    assert!(controller.poll(Duration::from_secs(199)).is_err());
    assert_eq!((controller.phase(), controller.position()), before, "an invalid wall-clock observation must not rewind playback");
    controller.cancel();
    audio.pump(1);
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Cancelled);
    assert_eq!(controller.phase(), Phase::Cancelled);
}

#[test]
fn foreign_and_impossible_audio_observations_cannot_mutate_this_playback() {
    let Some(fixture) = Fixture::normal() else { return };
    let audio = engine();
    let mut controller = fixture.controller(5);
    prepared(&mut controller);
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    audio.pump(2_205);
    controller.poll(Duration::ZERO).unwrap();
    let own = controller.audio_handle().unwrap().clone();
    let foreign_engine = engine();
    let foreign = foreign_engine.play_movie(MoviePcm::from_s16le(RATE, vec![0; 8_820 * 4]).unwrap(), 1.0).unwrap();
    foreign_engine.pump(2_205);
    let before = (controller.phase(), controller.position());
    let id = controller.id();
    assert!(!controller.accept_audio_status(PlaybackId { generation: id.generation + 1, ..id }, &own, own.status(), Duration::ZERO).unwrap());
    assert!(!controller.accept_audio_status(id, &foreign, foreign.status(), Duration::ZERO).unwrap());
    let valid = own.status();
    for bad in [
        fmv_audio::MovieAudioStatus { source_rate: RATE + 1, ..valid },
        fmv_audio::MovieAudioStatus { total_frames: valid.total_frames + 1, ..valid },
        fmv_audio::MovieAudioStatus { consumed_frames: valid.total_frames + 1, ..valid },
        fmv_audio::MovieAudioStatus { consumed_frames: valid.consumed_frames - 1, ..valid },
        fmv_audio::MovieAudioStatus { state: MovieAudioState::Ended, ..valid },
    ] {
        assert!(controller.accept_audio_status(id, &own, bad, Duration::ZERO).is_err());
        assert_eq!((controller.phase(), controller.position()), before);
    }
    controller.cancel();
}

#[test]
fn finite_video_tail_starts_only_after_audio_ended_and_pauses_separately() {
    let Some(fixture) = Fixture::new(2_205, &[(0, 100), (100, 150), (250, 100)], None) else { return };
    let audio = engine();
    let mut controller = fixture.controller(6);
    prepared(&mut controller);
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    controller.poll(Duration::from_secs(10)).unwrap();
    time_is(&controller, 0, 1);
    audio.pump(2_205);
    controller.poll(Duration::from_secs(10)).unwrap();
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Ended);
    assert_eq!(controller.phase(), Phase::VideoTail);
    time_is(&controller, 1, 10);
    controller.poll(Duration::from_millis(10_050)).unwrap();
    time_is(&controller, 3, 20);
    controller.set_paused(Duration::from_millis(10_050), true).unwrap();
    controller.poll(Duration::from_secs(20)).unwrap();
    time_is(&controller, 3, 20);
    controller.set_paused(Duration::from_secs(20), false).unwrap();
    controller.poll(Duration::from_millis(20_200)).unwrap();
    time_is(&controller, 7, 20);
    assert_ne!(controller.phase(), Phase::Completed, "elapsed endpoint and decoder EOF cannot replace final presentation");
    controller.cancel();
}

#[test]
fn synthetic_admission_still_enforces_the_finite_tail_limit() {
    let Some(fixture) = Fixture::new(2_205, &[(0, 1_101)], None) else { return };
    let mut controller = fixture.controller(8);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(Instant::now() < deadline);
        if controller.poll(Duration::ZERO).is_err() { break; }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted, "100 ms of audio plus 1001 ms of video tail is outside policy");
    assert!(controller.audio_handle().is_none());
}

#[test]
fn dropping_a_controller_cancels_its_voice_but_cannot_cancel_a_replacement_voice() {
    let Some(fixture) = Fixture::normal() else { return };
    let audio = engine();
    let mut controller = fixture.controller(12);
    prepared(&mut controller);
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    audio.pump(1);
    let owned = controller.audio_handle().unwrap().clone();
    drop(controller);
    audio.pump(1);
    assert_eq!(owned.status().state, MovieAudioState::Cancelled);

    for teardown in ["drop", "cancel", "poll"] {
        let mut controller = fixture.controller(12);
        prepared(&mut controller);
        controller.play(&audio, Duration::ZERO, 1.0).unwrap();
        audio.pump(1);
        let owned = controller.audio_handle().unwrap().clone();
        let replacement = audio.play_movie(MoviePcm::from_s16le(RATE, vec![0; 8_820 * 4]).unwrap(), 1.0).unwrap();
        audio.pump(1);
        assert_eq!(owned.status().state, MovieAudioState::Replaced);
        match teardown {
            "cancel" => controller.cancel(),
            "poll" => {
                assert!(controller.poll(Duration::ZERO).is_err());
                assert_eq!(controller.phase(), Phase::Faulted, "the owned voice was replaced externally");
            }
            _ => {},
        }
        drop(controller);
        audio.pump(1);
        assert_eq!(replacement.status().state, MovieAudioState::Playing);
        assert_eq!(replacement.status().consumed_frames, 2, "old cancellation targets its exact handle, even on the same engine");
    }
}

#[test]
fn truncated_decoder_output_faults_behind_a_future_frame_even_while_paused() {
    for paused in [false, true] {
        let Some(fixture) = Fixture::new(8_820, &[(0, 100), (100, 150), (250, 100)], Some(
            "sys.stdout.buffer.write(bytes([20,40,60,255])*2); sys.stdout.flush()\nwhile not pathlib.Path(args[args.index('-i')+1]).with_suffix('.release').exists(): time.sleep(0.002)\nsys.stdout.buffer.write(b'xx')"
        )) else { return };
        let audio = engine();
        let mut controller = fixture.controller(3);
        prepared(&mut controller);
        until(&mut controller, Duration::ZERO, |c| c.pending_frames() == 2);
        controller.play(&audio, Duration::ZERO, 1.0).unwrap();
        audio.pump(1);
        controller.poll(Duration::ZERO).unwrap();
        if paused {
            controller.set_paused(Duration::ZERO, true).unwrap();
            audio.pump(1);
        }
        let handle = controller.audio_handle().unwrap().clone();
        assert_eq!(controller.pending_frames(), 2, "a future frame is retained while the source clock stays near zero");
        std::fs::write(fixture.request.path.with_extension("release"), b"release synthetic decoder").unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            assert!(Instant::now() < deadline, "decoder failure stayed hidden behind a future frame; paused={paused}");
            if controller.poll(Duration::ZERO).is_err() { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(controller.phase(), Phase::Faulted);
        assert_eq!(handle.status().consumed_frames, 1, "surfacing the fault did not need clock advancement");
        audio.pump(1);
        assert_eq!(handle.status().state, MovieAudioState::Cancelled);
        assert_eq!(controller.pending_frames(), 0);
    }
}

fn display() -> DisplayPolicy {
    DisplayPolicy { aspect: DisplayAspect::new(1, 1).unwrap(), fit: MovieFit::Contain }
}

fn draw(controller: &mut MovieController, offscreen: &mut Offscreen, bind: bool) -> PresentationTicket {
    let mut ticket = None;
    offscreen.render_with(4, 4, |gpu, target, _| {
        if bind { controller.bind_gpu(gpu); }
        controller.upload_due(gpu).unwrap();
        ticket = Some(controller.draw_base(gpu, target, (4, 4), MovieViewport { x: 0, y: 0, width: 4, height: 4 }, display()).unwrap());
    });
    ticket.unwrap()
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_controller_requires_an_exact_acknowledgement_and_uses_drawn_pts_for_mvief() {
    let Some(fixture) = Fixture::normal() else { return };
    let Some(mut offscreen) = Offscreen::new() else { return };
    let audio = engine();
    let mut controller = fixture.controller(7);
    prepared(&mut controller);
    let mvief = MviefPolicy::FfnxDrawnPts { fps_numerator: 15, fps_denominator: 1 };
    assert_eq!(controller.mvief(mvief).unwrap(), None);
    until(&mut controller, Duration::ZERO, |c| c.pending_frames() == 2);
    let first = draw(&mut controller, &mut offscreen, true);
    assert_eq!((first.submission.frame.stream, first.submission.frame.ordinal), (controller.id().generation, 0));
    assert_eq!(controller.mvief(mvief).unwrap(), Some(1), "FFNx counts its first drawn frame as one; decoded ordinal remains zero");
    offscreen.render_with(4, 4, |gpu, target, _| {
        assert!(matches!(controller.upload_due(gpu), Err(ControllerError::PresentationBusy)));
        assert!(matches!(controller.draw_base(gpu, target, (4, 4), MovieViewport { x: 0, y: 0, width: 4, height: 4 }, display()), Err(ControllerError::PresentationBusy)));
        gpu.draw_movie_base(target, (4, 4), display()).unwrap();
    });
    for wrong in [
        PresentationTicket { serial: first.serial + 1, ..first },
        PresentationTicket { id: PlaybackId { generation: first.id.generation + 1, ..first.id }, ..first },
        PresentationTicket { submission: crate::render::movie::MovieSubmission {
            frame: crate::render::movie::MovieFrameId { ordinal: 1, ..first.submission.frame },
        }, ..first },
    ] { assert!(!controller.acknowledge_presented(wrong).unwrap()); }
    assert!(controller.acknowledge_presented(first).unwrap());
    assert!(!controller.acknowledge_presented(first).unwrap());
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    audio.pump(2_204);
    controller.poll(Duration::ZERO).unwrap();
    let almost = draw(&mut controller, &mut offscreen, false);
    assert_eq!(almost.submission.frame.ordinal, 0, "the 100 ms frame is not due at 2204/22050 seconds");
    assert!(controller.acknowledge_presented(almost).unwrap());
    audio.pump(1);
    controller.poll(Duration::ZERO).unwrap();
    let second = draw(&mut controller, &mut offscreen, false);
    assert_eq!(second.submission.frame.ordinal, 1);
    assert_eq!(controller.mvief(mvief).unwrap(), Some(3), "round(100 ms *15)+1 uses actual drawn PTS");
    assert!(controller.discard_submission(second));
    assert!(!controller.acknowledge_presented(second).unwrap());
    audio.pump(6_615);
    let deadline = Instant::now() + Duration::from_secs(6);
    let final_ticket = loop {
        assert!(Instant::now() < deadline);
        controller.poll(Duration::ZERO).unwrap();
        let ticket = draw(&mut controller, &mut offscreen, false);
        if ticket.submission.frame.ordinal == 2 { break ticket; }
        assert!(controller.acknowledge_presented(ticket).unwrap());
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(controller.mvief(mvief).unwrap(), Some(5), "round(250 ms *15)+1 is five; the decoded ordinal is two");
    controller.poll(Duration::ZERO).unwrap();
    assert_ne!(controller.phase(), Phase::Completed, "successful decode, audio end and GPU submission still lack presentation acknowledgement");
    assert!(controller.acknowledge_presented(final_ticket).unwrap());
    until(&mut controller, Duration::ZERO, |c| c.phase() == Phase::Completed);
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Ended);
    let held = draw(&mut controller, &mut offscreen, false);
    assert_eq!(held.submission.frame.ordinal, 2);
    assert!(controller.acknowledge_presented(held).unwrap(), "a completed final frame can still be redrawn and acknowledged");
    assert!(!controller.acknowledge_presented(held).unwrap());
    assert_eq!(controller.phase(), Phase::Completed);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_acknowledged_last_frame_waits_for_its_full_duration_and_audio_eof() {
    let Some(fixture) = Fixture::normal() else { return };
    let Some(mut offscreen) = Offscreen::new() else { return };
    let audio = engine();
    let mut controller = fixture.controller(8);
    prepared(&mut controller);
    let first = draw(&mut controller, &mut offscreen, true);
    assert!(controller.acknowledge_presented(first).unwrap());
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    // 300 ms: the last frame (PTS250 ms) is due, but ends only at350 ms.
    audio.pump(6_615);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(Instant::now() < deadline);
        controller.poll(Duration::ZERO).unwrap();
        let ticket = draw(&mut controller, &mut offscreen, false);
        let last = ticket.submission.frame.ordinal == 2;
        assert!(controller.acknowledge_presented(ticket).unwrap());
        if last { break; }
        std::thread::sleep(Duration::from_millis(2));
    }
    controller.poll(Duration::from_secs(1)).unwrap();
    assert_ne!(controller.phase(), Phase::Completed);
    time_is(&controller, 3, 10);
    // 380 ms: every video interval elapsed, but400 ms PCM has not ended.
    audio.pump(1_764);
    controller.poll(Duration::from_secs(1)).unwrap();
    time_is(&controller, 19, 50);
    assert_ne!(controller.phase(), Phase::Completed);
    audio.pump(441);
    until(&mut controller, Duration::from_secs(1), |c| c.phase() == Phase::Completed);
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Ended);
}

#[test]
#[ignore = "needs the pinned owned opening (FMV_OPENING_MOVIE), explicit FMV_FFMPEG/FMV_FFPROBE and a GPU; no asset output"]
fn offscreen_owned_opening_decodes_mixes_and_acknowledges_the_final_frame_before_completion() {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let (Some(path), Some(ffmpeg), Some(ffprobe)) = (var("FMV_OPENING_MOVIE"), var("FMV_FFMPEG"), var("FMV_FFPROBE")) else {
        eprintln!("skipped: explicit owned movie and tool paths are required");
        return;
    };
    let Some(mut offscreen) = Offscreen::new() else { return };
    let audio = engine();
    let request = Request { path, tools: Tools { ffmpeg, ffprobe }, limits: Limits::default() };
    let mut controller = MovieController::prepare(91, request, Admission::OpeningFfnxPcmZeroOrigin).unwrap();
    let deadline = Instant::now() + Duration::from_secs(295);
    while !controller.first_frame_available() {
        assert!(Instant::now() < deadline, "opening preparation deadline");
        controller.poll(Duration::ZERO).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    let metadata = controller.metadata().unwrap();
    assert_eq!((metadata.video.width, metadata.video.height, metadata.video.frames.len()), (1280, 896, 1792));
    assert!(metadata.audio.as_ref().expect("the opening has audio").require_contiguous().is_err(), "the explicit pinned policy has not rewritten the source audio evidence");
    let last = metadata.video.frames.len() - 1;
    let fit = DisplayPolicy { aspect: DisplayAspect::new(1280, 896).unwrap(), fit: MovieFit::Contain };
    let viewport = MovieViewport { x: 0, y: 0, width: 80, height: 56 };
    let mut first = None;
    offscreen.render_with(80, 56, |gpu, target, _| {
        controller.bind_gpu(gpu);
        controller.upload_due(gpu).unwrap().expect("first decoded frame uploads");
        first = Some(controller.draw_base(gpu, target, (80, 56), viewport, fit).unwrap());
    });
    let first = first.unwrap();
    assert_eq!(first.submission.frame.ordinal, 0);
    assert!(controller.acknowledge_presented(first).unwrap());
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    let mut now = Duration::ZERO;
    let mut maximum_pending = 0;
    let mut final_seen = false;
    let mut heard = false;
    while controller.phase() != Phase::Completed {
        assert!(Instant::now() < deadline, "opening acceptance deadline, phase {:?}", controller.phase());
        if controller.audio_handle().unwrap().status().state != MovieAudioState::Ended {
            heard |= audio.pump(22_050).iter().any(|s| s.abs() > 0.0001);
        }
        now += Duration::from_millis(10);
        controller.poll(now).unwrap();
        maximum_pending = maximum_pending.max(controller.pending_frames());
        let mut ticket = None;
        offscreen.render_with(80, 56, |gpu, target, _| {
            controller.upload_due(gpu).unwrap();
            ticket = Some(controller.draw_base(gpu, target, (80, 56), viewport, fit).unwrap());
            assert_eq!(gpu.movie_state().allocations, 1, "movie textures are reused across all inspected frames");
        });
        let ticket = ticket.unwrap();
        if ticket.submission.frame.ordinal == last {
            if !final_seen { assert_ne!(controller.phase(), Phase::Completed, "the final draw has not yet been acknowledged"); }
            final_seen = true;
        }
        assert!(controller.acknowledge_presented(ticket).unwrap());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(heard && final_seen);
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Ended);
    assert!(maximum_pending <= 2, "controller retention stays bounded: {maximum_pending}");
}

// ---------------------------------------------------------------------------------------------
// The general admission and silent movies
// ---------------------------------------------------------------------------------------------

/// Stand in for the renderer's upload, base draw and acknowledged present of the due frame, so the controller's completion
/// rule can be driven without a GPU (the offscreen tests below do the real draw). Returns the presented ordinal.
fn present_due(c: &mut MovieController) -> Option<usize> {
    let frame = c.due.take()?;
    let id = MovieFrameId { stream: c.id.generation, ordinal: frame.ordinal, timing: frame.timing, time_base: c.video.as_ref().unwrap().time_base };
    let ticket = PresentationTicket { id: c.id, submission: MovieSubmission { frame: id }, serial: 0 };
    c.pending_ticket = Some(ticket);
    assert!(c.acknowledge_presented(ticket).unwrap());
    Some(frame.ordinal)
}

fn run_until_completed(c: &mut MovieController, mut now: Duration, step: Duration, audio: Option<&AudioEngine>) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while c.phase() != Phase::Completed {
        assert!(Instant::now() < deadline, "did not complete: {:?} at {:?}", c.phase(), c.position());
        if let Some(audio) = audio {
            audio.pump(441);
        }
        c.poll(now).unwrap();
        present_due(c);
        now += step;
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn touch(path: &Path) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(3_600)).unwrap();
}

#[test]
fn the_general_admission_plays_a_movie_with_audio_on_the_mixer_clock_without_a_pinned_hash() {
    let Some(fixture) = Fixture::normal() else { return };
    // No digest, no pinned size or timestamps: any owned movie is admitted by what is generic about it.
    let audio = engine();
    let mut controller = fixture.general(1);
    prepared(&mut controller);
    assert!(!controller.is_silent());
    assert!(controller.metadata().unwrap().audio.is_some());
    assert!(controller.play_silent(Duration::ZERO).is_err(), "a movie with audio cannot start on the silent clock");
    assert_eq!(controller.phase(), Phase::Prepared);
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    controller.poll(Duration::from_secs(100)).unwrap();
    time_is(&controller, 0, 1);
    audio.pump(2_205);
    controller.poll(Duration::from_secs(100)).unwrap();
    time_is(&controller, 1, 10);
    run_until_completed(&mut controller, Duration::from_secs(100), Duration::ZERO, Some(&audio));
    assert_eq!(controller.audio_handle().unwrap().status().state, MovieAudioState::Ended);
}

#[test]
fn the_general_admission_keeps_the_generic_checks_that_are_not_asset_specific() {
    // The finite video tail rule: 100 ms of audio plus 1001 ms of video is outside policy.
    let Some(fixture) = Fixture::new(2_205, &[(0, 1_101)], None) else { return };
    let mut controller = fixture.general(1);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(Instant::now() < deadline);
        if controller.poll(Duration::ZERO).is_err() { break; }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted);
    assert!(controller.audio_handle().is_none());
    // A regular file within the size limit, named by an absolute path.
    let Some(fixture) = Fixture::normal() else { return };
    let general = |request: Request| MovieController::prepare(1, request, Admission::General).is_err();
    let mut small = fixture.request.clone();
    small.limits.max_media_bytes = 8;
    assert!(general(small), "the size limit still applies");
    let mut directory = fixture.request.clone();
    directory.path = fixture.path().parent().unwrap().to_path_buf();
    assert!(general(directory), "only a regular file");
    let mut relative = fixture.request.clone();
    relative.path = PathBuf::from("synthetic.owned");
    assert!(general(relative), "an absolute path is required");
    let mut empty = fixture.request.clone();
    empty.path = fixture.path().with_file_name("empty.owned");
    std::fs::write(&empty.path, b"").unwrap();
    assert!(general(empty), "an empty file is refused");
    // And the 600 second limit: 601 s of video.
    let Some(fixture) = Fixture::new(2_205, &[(0, 601_000)], None) else { return };
    let mut controller = fixture.general(2);
    let deadline = Instant::now() + Duration::from_secs(6);
    while controller.poll(Duration::ZERO).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted);
}

#[test]
fn a_general_movie_whose_file_changes_is_refused_before_and_while_it_is_decoded() {
    let Some(fixture) = Fixture::normal() else { return };
    // Changed between admission and the decoder's first word: the controller never accepts its metadata.
    let mut controller = fixture.general(1);
    touch(fixture.path());
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(Instant::now() < deadline);
        if controller.poll(Duration::ZERO).is_err() { break; }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted);
    assert!(controller.metadata().is_none());
    // Changed after preparation, before the decoder has finished reading: refused before it can complete.
    let Some(fixture) = Fixture::normal() else { return };
    let audio = engine();
    let mut controller = fixture.general(2);
    prepared(&mut controller);
    controller.play(&audio, Duration::ZERO, 1.0).unwrap();
    touch(fixture.path());
    audio.pump(8_820);
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut now = Duration::ZERO;
    loop {
        assert!(Instant::now() < deadline);
        if controller.poll(now).is_err() { break; }
        present_due(&mut controller);
        assert_ne!(controller.phase(), Phase::Completed, "a movie that changed under the decoder never completes");
        now += Duration::from_millis(10);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(controller.phase(), Phase::Faulted);
    // A file that grew (its modification time moves with it) is refused too.
    let Some(fixture) = Fixture::normal() else { return };
    let mut controller = fixture.general(3);
    std::fs::OpenOptions::new().append(true).open(fixture.path()).unwrap().write_all(b"more").unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while controller.poll(Duration::ZERO).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted);
}

#[test]
fn the_pinned_admissions_still_refuse_a_file_without_audio() {
    let Some(fixture) = Fixture::silent(&[(0, 100)], None) else { return };
    let mut controller = fixture.controller(1);
    let deadline = Instant::now() + Duration::from_secs(6);
    while controller.poll(Duration::ZERO).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted, "only the general admission may meet a movie with no audio stream");
    assert!(controller.metadata().is_none());
    assert!(MovieController::prepare(2, fixture.request.clone(), Admission::OpeningFfnxPcmZeroOrigin).is_err());
}

#[test]
fn a_silent_movie_plays_to_its_end_on_its_own_frame_clock_without_any_audio() {
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150), (250, 100)], None) else { return };
    let audio = engine();
    let mut controller = fixture.general(1);
    prepared(&mut controller);
    assert!(controller.is_silent());
    assert!(controller.metadata().unwrap().audio.is_none(), "no audio is invented");
    assert!(controller.play(&audio, Duration::ZERO, 1.0).is_err(), "there is nothing to play on the mixer");
    assert_eq!(controller.phase(), Phase::Prepared);
    assert!(controller.audio_handle().is_none());
    // Wall time before play is not movie time, and a silent movie never needs an engine.
    controller.poll(Duration::from_secs(50)).unwrap();
    time_is(&controller, 0, 1);
    controller.play_silent(Duration::from_secs(100)).unwrap();
    assert_eq!(controller.phase(), Phase::Playing);
    controller.poll(Duration::from_secs(100)).unwrap();
    time_is(&controller, 0, 1);
    controller.poll(Duration::from_millis(100_050)).unwrap();
    time_is(&controller, 1, 20);
    controller.poll(Duration::from_millis(100_200)).unwrap();
    time_is(&controller, 1, 5);
    // Pause stops the movie's time; resuming continues from where it stopped.
    controller.set_paused(Duration::from_millis(100_200), true).unwrap();
    controller.poll(Duration::from_secs(160)).unwrap();
    assert_eq!(controller.phase(), Phase::Paused);
    time_is(&controller, 1, 5);
    controller.set_paused(Duration::from_secs(160), false).unwrap();
    controller.poll(Duration::from_millis(160_100)).unwrap();
    time_is(&controller, 3, 10);
    assert!(controller.poll(Duration::from_secs(159)).is_err(), "the clock never runs backwards");
}

#[test]
fn a_silent_movie_ends_at_the_video_endpoint_and_completes_only_after_the_final_presentation() {
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150), (250, 100)], None) else { return };
    let mut controller = fixture.general(1);
    prepared(&mut controller);
    controller.play_silent(Duration::ZERO).unwrap();
    // A long stall is one late tick: the position stops at the endpoint (350 ms), never past it, and the phase is still Playing.
    controller.poll(Duration::from_secs(30)).unwrap();
    time_is(&controller, 7, 20);
    assert_eq!(controller.phase(), Phase::Playing, "a silent movie has no video tail phase");
    controller.poll(Duration::from_secs(300)).unwrap();
    time_is(&controller, 7, 20);
    // Frames before the last are not the end: the movie completes only when the last frame is presented after decoder EOF.
    let mut now = Duration::from_secs(300);
    let mut presented = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while controller.phase() != Phase::Completed {
        assert!(Instant::now() < deadline, "silent movie did not complete: {:?}", controller.phase());
        controller.poll(now).unwrap();
        if let Some(ordinal) = present_due(&mut controller) {
            presented.push(ordinal);
        }
        now += Duration::from_millis(5);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(presented.last(), Some(&2), "the last decoded frame is the last presented: {presented:?}");
    time_is(&controller, 7, 20);
    assert!(controller.audio_handle().is_none());
}

#[test]
fn a_silent_movie_needs_its_first_frame_and_wall_time_alone_never_completes_it() {
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150), (250, 100)], Some("time.sleep(30)")) else { return };
    let mut controller = fixture.general(1);
    until(&mut controller, Duration::ZERO, |c| c.metadata().is_some());
    assert!(!controller.first_frame_available());
    assert!(controller.play_silent(Duration::ZERO).is_err());
    controller.poll(Duration::from_secs(1_000)).unwrap();
    time_is(&controller, 0, 1);
    assert_eq!(controller.phase(), Phase::Preparing);
    controller.cancel();
    assert_eq!(controller.phase(), Phase::Cancelled);
}

#[test]
fn a_silent_movie_that_outlasts_the_policy_is_refused_and_a_decoder_failure_faults_it() {
    let Some(fixture) = Fixture::silent(&[(0, 601_000)], None) else { return };
    let mut controller = fixture.general(1);
    let deadline = Instant::now() + Duration::from_secs(6);
    while controller.poll(Duration::ZERO).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(controller.phase(), Phase::Faulted, "the 600 second limit applies to silent movies too");
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150)], Some("sys.stdout.buffer.write(bytes([1,2,3,255])); sys.exit(3)")) else { return };
    let mut controller = fixture.general(2);
    prepared_or_fault(&mut controller);
    assert_eq!(controller.phase(), Phase::Faulted, "a decoder that stops short never completes a movie");
}

fn prepared_or_fault(controller: &mut MovieController) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut now = Duration::ZERO;
    while controller.poll(now).is_ok() {
        assert!(Instant::now() < deadline);
        if controller.first_frame_available() && controller.phase() == Phase::Prepared {
            controller.play_silent(now).unwrap();
        }
        now += Duration::from_secs(1);
        present_due(controller);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_silent_movie_draws_its_frames_and_completes_on_real_time_after_the_acknowledged_last_frame() {
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150), (250, 100)], None) else { return };
    let Some(mut offscreen) = Offscreen::new() else { return };
    let mut controller = fixture.general(9);
    prepared(&mut controller);
    let first = draw(&mut controller, &mut offscreen, true);
    assert_eq!(first.submission.frame.ordinal, 0);
    assert!(controller.acknowledge_presented(first).unwrap());
    let started = Instant::now();
    controller.play_silent(Duration::ZERO).unwrap();
    let deadline = started + Duration::from_secs(6);
    let mut last_seen = false;
    while controller.phase() != Phase::Completed {
        assert!(Instant::now() < deadline, "silent movie deadline: {:?}", controller.phase());
        controller.poll(started.elapsed()).unwrap();
        let ticket = draw(&mut controller, &mut offscreen, false);
        last_seen |= ticket.submission.frame.ordinal == 2;
        assert!(controller.acknowledge_presented(ticket).unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(last_seen);
    assert!(started.elapsed() >= Duration::from_millis(350), "a silent movie lasts its video's own length: {:?}", started.elapsed());
    assert!(controller.audio_handle().is_none());
}
