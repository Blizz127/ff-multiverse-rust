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
        let python = ["/usr/bin/python3", "/bin/python3"].into_iter().find(|p| Path::new(p).is_file())?;
        let dir = tempfile::tempdir().expect("synthetic movie directory");
        let tool = dir.path().join("fixture-decoder");
        let path = dir.path().join("synthetic.owned");
        let bytes = b"synthetic movie controller fixture";
        std::fs::write(&path, bytes).unwrap();
        let streams = json!({"streams": [
            {"index":0,"codec_type":"video","codec_name":"vp8","width":1,"height":1,"time_base":"1/1000","start_pts":0},
            {"index":1,"codec_type":"audio","codec_name":"vorbis","sample_rate":"22050","channels":2,"time_base":"1/22050","start_pts":0}
        ]});
        let video = json!({"frames": timings.iter().map(|&(pts, duration)| {
            json!({"pts":pts,"duration":duration,"width":1,"height":1})
        }).collect::<Vec<_>>()});
        let audio = json!({"frames": [{"pts":0,"nb_samples":audio_frames,"stream_index":1,"channels":2}]});
        let default_body = format!("sys.stdout.buffer.write(bytes([20,40,60,255])*{})", timings.len());
        let body = video_body.unwrap_or(&default_body).lines().map(|line| format!(" {line}\n")).collect::<String>();
        let script = format!(
            "#!{python}\nimport sys,json,time,pathlib\nargs=sys.argv[1:]\n\
             if '-show_streams' in args:\n print(r'''{streams}''')\n\
             elif '-show_frames' in args and 'a:0' in args:\n print(r'''{audio}''')\n\
             elif '-show_frames' in args:\n print(r'''{video}''')\n\
             elif 's16le' in args:\n sys.stdout.buffer.write(bytes([0,32,0,32])*{audio_frames})\n\
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
    assert!(metadata.audio.require_contiguous().is_err(), "the explicit pinned policy has not rewritten the source audio evidence");
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
