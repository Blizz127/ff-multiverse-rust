//! The render path: the mixer, the queue of commands in front of it, and the guard that keeps a
//! panic in either from reaching whoever owns the audio thread.
//!
//! One [`MixerHandle`] is shared by whatever is rendering: a device's callback, or the null sink's
//! clock. When a device dies the supervisor starts the null sink on the same handle, so the music
//! (its position, its fades, which track it is) carries on.

use crate::mixer::{Command, Mixer};
use crate::status::Shared;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

pub(crate) struct RenderHost {
    mixer: Mixer,
    commands: Receiver<Command>,
    shared: Arc<Shared>,
}

impl RenderHost {
    pub(crate) fn new(mixer: Mixer, commands: Receiver<Command>, shared: Arc<Shared>) -> Self {
        Self { mixer, commands, shared }
    }

    fn drain(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            self.mixer.apply(command);
        }
    }

    /// Run `work`; if it panics (a faulty source, or a bug here), count the fault, throw the mixer's
    /// state away for a fresh one at the same rate (the levels live in `shared`, so they survive)
    /// and say so with `None`.
    fn guarded<R>(&mut self, work: impl FnOnce(&mut Self) -> R) -> Option<R> {
        match catch_unwind(AssertUnwindSafe(|| work(&mut *self))) {
            Ok(result) => Some(result),
            Err(_) => {
                self.shared.count_fault();
                self.mixer = Mixer::new(self.mixer.rate(), self.shared.clone());
                None
            }
        }
    }

    fn render(&mut self, out: &mut [f32]) {
        self.shared.count_callback();
        let rendered = self
            .guarded(|host| {
                host.drain();
                host.mixer.render(out);
            })
            .is_some();
        if !rendered {
            out.fill(0.0);
        }
    }
}

/// What a device backend renders from: a cheap clone of a handle to the shared mixer.
#[derive(Clone)]
pub struct MixerHandle {
    inner: Arc<Mutex<RenderHost>>,
}

impl MixerHandle {
    pub(crate) fn new(host: RenderHost) -> Self {
        Self { inner: Arc::new(Mutex::new(host)) }
    }

    /// Render `out.len() / 2` interleaved stereo frames at the rate set with
    /// [`MixerHandle::set_output_rate`]. This is the audio callback's call: it never blocks (if
    /// something else holds the mixer for the moment it writes silence), never allocates, and a
    /// panic inside it is caught and costs one silent pass instead of the thread.
    pub fn render_stereo(&self, out: &mut [f32]) {
        match self.inner.try_lock() {
            Ok(mut host) => host.render(out),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().render(out),
            Err(TryLockError::WouldBlock) => out.fill(0.0),
        }
    }

    /// Render at `rate` Hz from now on. A device opener sets this to the device's rate before the
    /// stream starts.
    pub fn set_output_rate(&self, rate: u32) {
        let mut host = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        host.mixer.set_rate(rate);
    }

    /// Apply queued commands without rendering: how the supervisor keeps the queue from piling up
    /// while no backend is running yet (the device is still opening).
    pub(crate) fn drain_commands(&self) {
        let mut host = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = host.guarded(RenderHost::drain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MusicSource, Pull};
    use crate::types::{Backend, PlayOptions, TrackId, Volumes};
    use std::sync::mpsc;
    use std::time::Duration;

    struct Explodes;

    impl MusicSource for Explodes {
        fn sample_rate(&self) -> u32 {
            1_000
        }
        fn read(&mut self, _out: &mut [f32]) -> Pull {
            panic!("a decoder bug");
        }
        fn seek(&mut self, _frame: u64) -> bool {
            true
        }
    }

    struct Steady;

    impl MusicSource for Steady {
        fn sample_rate(&self) -> u32 {
            1_000
        }
        fn read(&mut self, out: &mut [f32]) -> Pull {
            out.fill(0.5);
            Pull::Frames(out.len() / 2)
        }
        fn seek(&mut self, _frame: u64) -> bool {
            true
        }
    }

    fn handle() -> (MixerHandle, mpsc::Sender<Command>, Arc<Shared>) {
        let shared = Shared::new(Volumes { master: 1.0, music: 1.0, sfx: 1.0 }, false, 1_000, Backend::Null);
        let (tx, rx) = mpsc::channel();
        let host = RenderHost::new(Mixer::new(1_000, shared.clone()), rx, shared.clone());
        (MixerHandle::new(host), tx, shared)
    }

    fn play(tx: &mpsc::Sender<Command>, track: u32, source: Box<dyn MusicSource>) {
        let opts = PlayOptions { fade: Duration::ZERO, repeat: true, gain: 1.0 };
        let voice = Box::new(crate::mixer::MusicVoice::new(TrackId(track), source, opts));
        tx.send(Command::PlayMusic { voice, fade: Duration::ZERO }).expect("the host is alive");
    }

    #[test]
    fn a_panicking_source_costs_one_silent_pass_and_the_next_track_plays() {
        let (handle, tx, shared) = handle();
        play(&tx, 1, Box::new(Explodes));
        let mut out = vec![1.0f32; 64];
        handle.render_stereo(&mut out);
        assert!(out.iter().all(|&v| v == 0.0), "the pass in which the source panicked is silent");
        let status = shared.snapshot();
        assert_eq!((status.faults, status.music), (1, None), "the fault is counted and the mixer was reset");

        play(&tx, 2, Box::new(Steady));
        handle.render_stereo(&mut out);
        assert!(out.iter().all(|&v| (v - 0.5).abs() < 1e-4), "audio carries on");
        let status = shared.snapshot();
        assert_eq!((status.faults, status.music), (1, Some(TrackId(2))));
    }

    #[test]
    fn rendering_never_blocks_on_a_held_mixer_and_survives_a_poisoned_one() {
        let (handle, tx, _shared) = handle();
        play(&tx, 1, Box::new(Steady));
        let mut out = vec![1.0f32; 16];
        {
            let _held = handle.inner.lock().expect("lock");
            handle.render_stereo(&mut out);
            assert!(out.iter().all(|&v| v == 0.0), "contended: silence, and no wait");
        }
        // Poison the mutex the way a panic outside the render guard would.
        let poisoner = handle.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.inner.lock().expect("lock");
            panic!("poison the mixer");
        })
        .join();
        assert!(handle.inner.is_poisoned());
        handle.render_stereo(&mut out);
        assert!(out.iter().all(|&v| (v - 0.5).abs() < 1e-4), "a poisoned mixer still renders");
    }

    #[test]
    fn draining_applies_commands_without_rendering() {
        let (handle, tx, shared) = handle();
        play(&tx, 9, Box::new(Steady));
        assert_eq!(shared.snapshot().music, None);
        handle.drain_commands();
        let status = shared.snapshot();
        assert_eq!((status.music, status.frames_rendered), (Some(TrackId(9)), 0));
        handle.set_output_rate(44_100);
        assert_eq!(shared.snapshot().sample_rate, 44_100);
    }
}
