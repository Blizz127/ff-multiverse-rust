use super::*;

fn voice(rate: u32, frames: usize) -> (MovieVoice, MovieHandle) {
    let bytes = [8192i16, -8192]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .cycle()
        .take(frames * 4)
        .collect();
    let (mut voice, handle) =
        MovieVoice::new(MoviePcm::from_s16le(rate, bytes).unwrap(), 1.0).unwrap();
    voice.start();
    (voice, handle)
}

#[test]
fn fractional_source_intervals_survive_rate_changes_and_pause() {
    let (mut voice, handle) = voice(3, 10);
    assert!(!voice.render(2, &mut [0.0; 2]));
    assert_eq!(handle.status().consumed_frames, 1); // 1 + 1/2
    voice.pause(true);
    let mut silent = [0.0; 20];
    assert!(!voice.render(4, &mut silent));
    assert_eq!(silent, [0.0; 20]);
    assert_eq!(handle.status().consumed_frames, 1);
    voice.pause(false);
    assert!(!voice.render(4, &mut [0.0; 4]));
    assert_eq!(handle.status().consumed_frames, 3); // 1.5 + 2 * 3/4
}

#[test]
fn clock_is_independent_of_callback_chunking_and_preserves_fraction_at_common_rates() {
    let (mut once, a) = voice(44_100, 10_000);
    let (mut pieces, b) = voice(44_100, 10_000);
    assert!(!once.render(48_000, &mut vec![0.0; 960]));
    for frames in [1, 7, 31, 97, 128, 216] {
        assert!(!pieces.render(48_000, &mut vec![0.0; frames * 2]));
    }
    assert_eq!(a.status().consumed_frames, 441);
    assert_eq!(a.status(), b.status());
    let (mut changed, c) = voice(44_100, 10_000);
    changed.render(48_000, &mut [0.0; 200]); // 91.875 source frames
    changed.render(96_000, &mut [0.0; 400]); // +91.875
    assert_eq!(c.status().consumed_frames, 183);
    changed.render(48_000, &mut [0.0; 200]); // +91.875
    assert_eq!(c.status().consumed_frames, 275);
}

#[test]
fn unsupported_rational_growth_faults_without_rounding_or_end_success() {
    let (mut voice, handle) = voice(1, 10);
    voice.position = Position {
        whole: 1,
        numerator: 1,
        denominator: u64::MAX,
    };
    voice.handle.publish(MovieAudioState::Playing, 1);
    let mut out = [0.0; 2];
    assert!(voice.render(2, &mut out));
    assert_eq!(out, [0.0; 2]);
    assert_eq!(
        (handle.status().state, handle.status().consumed_frames),
        (MovieAudioState::Faulted, 1)
    );
}

#[test]
fn an_abandoned_voice_is_cancelled_without_claiming_finite_end() {
    let (voice, handle) = voice(1000, 100);
    drop(voice);
    assert_eq!(
        (handle.status().state, handle.status().consumed_frames),
        (MovieAudioState::Cancelled, 0)
    );
}

#[test]
fn final_interval_is_not_published_as_ended_until_the_enclosing_render_succeeds() {
    let (mut initial, handle) = voice(1000, 1);
    assert!(initial.render(1000, &mut [0.0; 2]));
    assert_eq!(
        (handle.status().state, handle.status().consumed_frames),
        (MovieAudioState::Playing, 1)
    );
    drop(initial); // PCM can be released while the callback is still being mixed.
    assert_eq!(handle.status().state, MovieAudioState::Playing);
    handle.complete_rendered_callback();
    assert_eq!(handle.status().state, MovieAudioState::Ended);

    let (mut other, failed) = voice(1000, 1);
    assert!(other.render(1000, &mut [0.0; 2]));
    drop(other);
    failed.fault_rendered_callback();
    failed.complete_rendered_callback();
    assert_eq!(
        failed.status().state,
        MovieAudioState::Faulted,
        "a discarded callback cannot later become EOF"
    );
}
