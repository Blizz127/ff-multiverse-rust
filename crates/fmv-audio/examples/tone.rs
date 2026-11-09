//! Plays a test tone through the real output device, or says why it cannot: a way to check the audio
//! path on a machine that has a sound card (this crate's tests never touch one).
//!
//! ```text
//! cargo run -p fmv-audio --example tone                    # the default device
//! FMV_AUDIO=null cargo run -p fmv-audio --example tone     # the null sink: silent, same timeline
//! ```
//!
//! You should hear a 440 Hz tone, a crossfade into 660 Hz, a beep on the left and one on the right
//! over it, and a fade out. It reads no game file.

use fmv_audio::{AudioConfig, AudioEngine, Backend, PlayOptions, SfxClip, ToneSource, TrackId, Volumes};
use std::f32::consts::TAU;
use std::thread::sleep;
use std::time::{Duration, Instant};

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s)
}

fn beep(rate: u32, hz: f32) -> SfxClip {
    let frames = (rate / 4) as usize;
    let samples: Vec<f32> = (0..frames)
        .map(|i| 0.6 * (i as f32 * hz * TAU / rate as f32).sin() * (1.0 - i as f32 / frames as f32))
        .collect();
    SfxClip::from_mono(rate, &samples).expect("a beep clip")
}

fn main() {
    let engine = AudioEngine::start(AudioConfig {
        // A test tone should not startle anyone.
        volumes: Volumes { master: 0.3, ..Volumes::default() },
        ..AudioConfig::from_run(false)
    });
    // `start` returns at once; give the device a few seconds to open (or to be given up on).
    let deadline = Instant::now() + secs(8.0);
    while engine.status().backend == Backend::Opening && Instant::now() < deadline {
        sleep(Duration::from_millis(20));
    }
    let status = engine.status();
    println!("backend: {:?}, device: {:?}, {} Hz", status.backend, status.device, status.sample_rate);

    let fade = |s: f32| PlayOptions { fade: secs(s), ..PlayOptions::default() };
    engine.play_music(TrackId(1), Box::new(ToneSource::new(44_100, 440.0, 0.5, None)), fade(0.3));
    sleep(secs(2.0));
    println!("playing: {:?}", engine.status().music);
    engine.play_music(TrackId(2), Box::new(ToneSource::new(44_100, 660.0, 0.5, None)), fade(1.5));
    sleep(secs(1.0));
    engine.play_sfx(&beep(44_100, 880.0), 1.0, -0.9);
    sleep(secs(0.6));
    engine.play_sfx(&beep(44_100, 1_320.0), 1.0, 0.9);
    sleep(secs(1.5));
    engine.stop_music(secs(1.0));
    sleep(secs(1.3));
    println!("done: {:?}", engine.status());
}
