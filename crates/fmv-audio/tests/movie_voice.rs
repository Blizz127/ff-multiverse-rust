//! The movie voice through the public manual engine: no device, wall clock or decoder.

use fmv_audio::{
    AudioConfig, AudioEngine, Mode, MovieAudioError, MovieAudioState, MovieHandle, MoviePcm,
    MusicSource, PlayOptions, Pull, TrackId, Volumes,
};
use std::time::Duration;

fn full() -> Volumes { Volumes { master: 1.0, music: 1.0, sfx: 1.0 } }

fn engine(rate: u32) -> AudioEngine {
    AudioEngine::start_manual(AudioConfig { mode: Mode::Null, sample_rate: rate, volumes: full(), ..AudioConfig::default() })
}

fn pcm(rate: u32, frames: &[[i16; 2]]) -> MoviePcm {
    let bytes = frames.iter().flat_map(|frame| frame.iter().flat_map(|sample| sample.to_le_bytes())).collect();
    MoviePcm::from_s16le(rate, bytes).expect("finite interleaved stereo PCM")
}

fn constant(rate: u32, frames: usize, value: i16) -> MoviePcm {
    pcm(rate, &vec![[value, value]; frames])
}

fn assert_samples(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        assert!((actual - expected).abs() < 1e-6, "sample {index}: {actual} versus {expected}");
    }
}

fn assert_position(handle: &MovieHandle, state: MovieAudioState, rate: u32, total: u64, consumed: u64) {
    let status = handle.status();
    assert_eq!((status.state, status.source_rate, status.total_frames, status.consumed_frames), (state, rate, total, consumed));
}

/// A normal music source whose signal and lifetime can be distinguished from the movie.
struct Music { remaining: Option<usize> }

impl MusicSource for Music {
    fn sample_rate(&self) -> u32 { 1_000 }
    fn read(&mut self, out: &mut [f32]) -> Pull {
        let asked = out.len() / 2;
        if asked == 0 { return Pull::Pending; }
        let count = self.remaining.map_or(asked, |remaining| asked.min(remaining));
        if count == 0 { return Pull::End; }
        out[..count * 2].fill(0.125);
        if let Some(remaining) = &mut self.remaining { *remaining -= count; }
        Pull::Frames(count)
    }
    fn seek(&mut self, _frame: u64) -> bool { false }
}

fn play_music(engine: &AudioEngine, id: u32, frames: Option<usize>) {
    engine.play_music(TrackId(id), Box::new(Music { remaining: frames }),
        PlayOptions { fade: Duration::ZERO, repeat: false, gain: 1.0 });
}

#[test]
fn queued_pcm_advances_only_when_rendered_and_ends_at_its_exact_finite_length() {
    let engine = engine(1_000);
    let movie = engine.play_movie(pcm(1_000, &[[16_384, -8_192], [8_192, -16_384], [4_096, -4_096]]), 0.5).unwrap();
    assert_position(&movie, MovieAudioState::Queued, 1_000, 3, 0);
    assert!(movie.clone() == movie, "clones refer to the same playback");
    assert_samples(&engine.pump(1), &[0.25, -0.125]);
    assert_position(&movie, MovieAudioState::Playing, 1_000, 3, 1);
    assert_samples(&engine.pump(2), &[0.125, -0.25, 0.0625, -0.0625]);
    assert_position(&movie, MovieAudioState::Ended, 1_000, 3, 3);
    assert_samples(&engine.pump(3), &[0.0; 6]);
    assert_position(&movie, MovieAudioState::Ended, 1_000, 3, 3);
    assert_eq!(engine.status().music_ended, 0, "a movie is not a one-shot music track");
}

#[test]
fn upsampling_preserves_fractional_progress_and_holds_the_last_sample_without_an_extra_tail() {
    let engine = engine(1_000);
    let movie = engine.play_movie(pcm(500, &[[0, 0], [16_384, -16_384], [8_192, -8_192]]), 1.0).unwrap();
    assert_samples(&engine.pump(1), &[0.0, 0.0]);
    assert_position(&movie, MovieAudioState::Playing, 500, 3, 0);
    assert_samples(&engine.pump(1), &[0.25, -0.25]);
    assert_position(&movie, MovieAudioState::Playing, 500, 3, 1);
    assert_samples(&engine.pump(4), &[0.5, -0.5, 0.375, -0.375, 0.25, -0.25, 0.25, -0.25]);
    assert_position(&movie, MovieAudioState::Ended, 500, 3, 3);
    assert_samples(&engine.pump(1), &[0.0, 0.0]);
}

#[test]
fn downsampling_clamps_the_final_partial_interval_and_does_not_read_past_pcm() {
    let engine = engine(1_000);
    let movie = engine.play_movie(pcm(1_500, &[[0, 0], [4_096, 4_096], [8_192, 8_192], [12_288, 12_288], [16_384, 16_384]]), 1.0).unwrap();
    assert_samples(&engine.pump(3), &[0.0, 0.0, 0.1875, 0.1875, 0.375, 0.375]);
    assert_position(&movie, MovieAudioState::Playing, 1_500, 5, 4);
    assert_samples(&engine.pump(3), &[0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
    assert_position(&movie, MovieAudioState::Ended, 1_500, 5, 5);
}

#[test]
fn a_common_fractional_sample_rate_has_exact_whole_frame_progress_across_blocks() {
    let engine = engine(48_000);
    let movie = engine.play_movie(constant(44_100, 5_000, 8_192), 1.0).unwrap();
    assert_samples(&engine.pump(480), &vec![0.25; 960]);
    assert_position(&movie, MovieAudioState::Playing, 44_100, 5_000, 441);
    for _ in 0..480 { engine.pump(1); }
    assert_position(&movie, MovieAudioState::Playing, 44_100, 5_000, 882);
}

#[test]
fn pause_freezes_only_the_movie_and_cancellation_is_not_natural_completion() {
    let engine = engine(1_000);
    play_music(&engine, 7, None);
    let movie = engine.play_movie(constant(1_000, 100, 8_192), 1.0).unwrap();
    assert_samples(&engine.pump(5), &[0.375; 10]);
    engine.pause_movie(&movie, true);
    assert_samples(&engine.pump(100), &[0.125; 200]);
    assert_position(&movie, MovieAudioState::Paused, 1_000, 100, 5);
    assert_eq!(engine.status().music, Some(TrackId(7)));
    engine.pause_movie(&movie, false);
    assert_samples(&engine.pump(2), &[0.375; 4]);
    assert_position(&movie, MovieAudioState::Playing, 1_000, 100, 7);
    engine.cancel_movie(&movie);
    assert_samples(&engine.pump(1), &[0.125; 2]);
    assert_position(&movie, MovieAudioState::Cancelled, 1_000, 100, 7);
    engine.pause_movie(&movie, false);
    engine.cancel_movie(&movie);
    engine.pump(20);
    assert_position(&movie, MovieAudioState::Cancelled, 1_000, 100, 7);
    assert_eq!(engine.status().music_ended, 0);
}

#[test]
fn a_pause_queued_before_the_first_render_consumes_no_source_interval() {
    let engine = engine(1_000);
    let movie = engine.play_movie(constant(1_000, 4, 8_192), 1.0).unwrap();
    engine.pause_movie(&movie, true);
    assert_samples(&engine.pump(50), &[0.0; 100]);
    assert_position(&movie, MovieAudioState::Paused, 1_000, 4, 0);
    engine.pause_movie(&movie, false);
    assert_samples(&engine.pump(4), &[0.25; 8]);
    assert_position(&movie, MovieAudioState::Ended, 1_000, 4, 4);
}

#[test]
fn movie_gain_is_independent_of_music_and_effect_levels_but_obeys_master_and_mute() {
    let config = |master, muted| AudioConfig { mode: Mode::Null, sample_rate: 1_000, muted,
        volumes: Volumes { master, music: 0.0, sfx: 0.0 }, ..AudioConfig::default() };
    let audible = AudioEngine::start_manual(config(1.0, false));
    let movie = audible.play_movie(constant(1_000, 4, 16_384), 0.5).unwrap();
    assert_samples(&audible.pump(4), &[0.25; 8]);
    assert_position(&movie, MovieAudioState::Ended, 1_000, 4, 4);
    for (master, muted, gain) in [(0.0, false, 1.0), (1.0, true, 1.0), (1.0, false, 0.0)] {
        let silent = AudioEngine::start_manual(config(master, muted));
        let movie = silent.play_movie(constant(1_000, 4, 16_384), gain).unwrap();
        assert_samples(&silent.pump(4), &[0.0; 8]);
        assert_position(&movie, MovieAudioState::Ended, 1_000, 4, 4);
    }
}

#[test]
fn replacement_makes_old_handles_stale_without_resetting_the_new_playback() {
    let engine = engine(1_000);
    let old = engine.play_movie(constant(1_000, 20, 4_096), 1.0).unwrap();
    engine.pump(3);
    let new = engine.play_movie(constant(1_000, 10, 8_192), 1.0).unwrap();
    assert!(old != new);
    engine.pause_movie(&old, true);
    engine.cancel_movie(&old);
    assert_samples(&engine.pump(2), &[0.25; 4]);
    assert_position(&old, MovieAudioState::Replaced, 1_000, 20, 3);
    assert_position(&new, MovieAudioState::Playing, 1_000, 10, 2);
    engine.pause_movie(&old, false);
    engine.pump(8);
    assert_position(&new, MovieAudioState::Ended, 1_000, 10, 10);
    engine.cancel_movie(&new);
    engine.pause_movie(&new, true);
    engine.pump(1);
    assert_position(&new, MovieAudioState::Ended, 1_000, 10, 10);
}

#[test]
fn handles_from_another_engine_cannot_pause_or_cancel_a_local_movie() {
    let a = engine(1_000);
    let b = engine(1_000);
    let first = a.play_movie(constant(1_000, 20, 4_096), 1.0).unwrap();
    let second = b.play_movie(constant(1_000, 20, 8_192), 1.0).unwrap();
    assert!(first != second, "even each engine's first playback has a distinct identity");
    a.pump(1); b.pump(1);
    b.pause_movie(&first, true); b.cancel_movie(&first);
    a.pause_movie(&second, true); a.cancel_movie(&second);
    assert_samples(&a.pump(2), &[0.125; 4]);
    assert_samples(&b.pump(2), &[0.25; 4]);
    assert_position(&first, MovieAudioState::Playing, 1_000, 20, 3);
    assert_position(&second, MovieAudioState::Playing, 1_000, 20, 3);
}

#[test]
fn music_and_effect_stops_leave_movie_audio_and_its_clock_alone() {
    let engine = engine(1_000);
    play_music(&engine, 7, None);
    let movie = engine.play_movie(constant(1_000, 20, 8_192), 1.0).unwrap();
    assert_samples(&engine.pump(1), &[0.375; 2]);
    engine.stop_music(Duration::ZERO);
    engine.stop_sfx();
    assert_samples(&engine.pump(2), &[0.25; 4]);
    assert_position(&movie, MovieAudioState::Playing, 1_000, 20, 3);
    play_music(&engine, 8, Some(2));
    engine.pump(4);
    assert_eq!((engine.status().music, engine.status().music_ended), (None, 1));
    assert_position(&movie, MovieAudioState::Playing, 1_000, 20, 7);
    engine.pump(13);
    assert_position(&movie, MovieAudioState::Ended, 1_000, 20, 20);
    assert_eq!(engine.status().music_ended, 1, "only the actual music voice contributes its end event");
}

#[test]
fn invalid_pcm_is_refused_and_disabled_audio_has_no_false_playback_handle() {
    assert!(MoviePcm::from_s16le(0, vec![0; 4]).is_err());
    for bytes in [vec![], vec![0], vec![0; 2], vec![0; 6]] {
        assert!(MoviePcm::from_s16le(1_000, bytes).is_err());
    }
    assert!(MoviePcm::from_s16le(1_000, vec![0; 128 * 1024 * 1024 + 4]).is_err(), "oversized PCM is rejected before conversion");
    let off = AudioEngine::start_manual(AudioConfig { mode: Mode::Off, ..AudioConfig::default() });
    assert!(matches!(off.play_movie(constant(1_000, 4, 8_192), 1.0), Err(MovieAudioError::Disabled)));
    assert!(off.pump(4).is_empty());
    assert_eq!(off.status().frames_rendered, 0);
}

#[test]
fn a_later_source_panic_invalidates_movie_eof_from_the_same_discarded_render_call() {
    struct PanicsAfterReadAhead { read_once: bool }
    impl MusicSource for PanicsAfterReadAhead {
        fn sample_rate(&self) -> u32 { 1_000 }
        fn read(&mut self, out: &mut [f32]) -> Pull {
            assert!(!self.read_once, "a later music read fails after the movie has rendered");
            self.read_once = true;
            out.fill(0.125);
            Pull::Frames(out.len() / 2)
        }
        fn seek(&mut self, _frame: u64) -> bool { false }
    }
    let engine = engine(1_000);
    engine.play_music(TrackId(9), Box::new(PanicsAfterReadAhead { read_once: false }),
        PlayOptions { fade: Duration::ZERO, repeat: false, gain: 1.0 });
    let movie = engine.play_movie(constant(1_000, 4, 8_192), 1.0).unwrap();
    assert_samples(&engine.pump(2_048), &vec![0.0; 4_096]);
    assert_eq!(engine.status().faults, 1);
    assert_eq!(movie.status().state, MovieAudioState::Faulted,
        "the render guard discarded even the earlier block containing the movie's last sample");
    assert_samples(&engine.pump(8), &[0.0; 16]);
    assert_eq!(movie.status().state, MovieAudioState::Faulted);
}
