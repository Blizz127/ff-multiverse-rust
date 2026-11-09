//! The engine: the handle the game holds, and the supervisor thread behind it that opens the
//! output device, moves to the null sink when there is no device (or it goes away) and keeps the
//! null sink's clock.
//!
//! **The game's calls never block.** `play_music` and friends build their voice on the caller's
//! thread and push it onto a queue; levels are atomics; [`AudioEngine::status`] reads atomics. Opening
//! a device (which can hang on a wedged sound server) happens on a thread of its own, and the
//! supervisor gives up on it after [`AudioConfig::open_timeout`] without ever waiting for it.
//!
//! **A missing or dead device is silence, not an error.** The supervisor writes exactly one note,
//! and the very same mixer carries on against a clock: track ids, fades and track ends behave as
//! they would with a device, so the game logic that watches them does not care.

use crate::config::{AudioConfig, Logger, Mode};
use crate::host::{MixerHandle, RenderHost};
use crate::mixer::{Command, Mixer, MusicVoice, SfxVoice};
use crate::source::{MusicSource, SfxClip};
use crate::status::Shared;
use crate::types::{AudioStatus, Backend, PlayOptions, TrackId, Volumes};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often the supervisor looks at its state (and, with the null sink, renders what is due).
const TICK: Duration = Duration::from_millis(5);
/// After a stall of the supervisor itself, the null sink renders at most this much time at once.
const MAX_CATCH_UP: Duration = Duration::from_millis(250);
/// Frames the null sink renders per call.
const NULL_CHUNK_FRAMES: usize = 1024;
/// How long dropping the engine waits for its thread before leaving it to finish on its own.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1500);

/// Why a device could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// No output device (or the sound server is not running).
    NoDevice,
    /// A device exists but offers nothing this engine can use.
    Unsupported(String),
    /// The backend failed to open or start the stream.
    Backend(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::NoDevice => f.write_str("no output device"),
            DeviceError::Unsupported(why) => write!(f, "unsupported: {why}"),
            DeviceError::Backend(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for DeviceError {}

/// A device backend's way to say its stream is gone for good (from an error callback, say). Cheap
/// to clone and safe to call from any thread.
#[derive(Clone)]
pub struct LostReporter(Arc<Shared>);

impl LostReporter {
    /// The supervisor then drops the stream and moves the mixer to the null sink.
    pub fn report(&self) {
        self.0.report_lost();
    }
}

/// What an opener gets to work with.
pub struct DeviceLink {
    mixer: MixerHandle,
    shared: Arc<Shared>,
}

impl DeviceLink {
    /// The mixer to render from: call [`MixerHandle::render_stereo`] from the device's callback,
    /// after [`MixerHandle::set_output_rate`] with the device's rate.
    pub fn mixer(&self) -> MixerHandle {
        self.mixer.clone()
    }

    pub fn lost_reporter(&self) -> LostReporter {
        LostReporter(self.shared.clone())
    }
}

/// An open, running output stream.
pub struct OpenedDevice {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Keeps the stream alive; dropping it stops the stream.
    pub guard: Box<dyn Send>,
}

/// Opens an output device. The real one is [`crate::device::CpalOpener`]; tests use fakes, so
/// that no test ever probes (let alone opens) the machine's real audio.
pub trait DeviceOpener: Send + 'static {
    /// Open the default output device and start it rendering from `link`. It may block: it runs on
    /// a thread of its own, and the engine goes silent if it takes longer than
    /// [`AudioConfig::open_timeout`]. A panic here is caught and counts as a failure.
    fn open(self: Box<Self>, link: DeviceLink) -> Result<OpenedDevice, DeviceError>;
}

#[cfg(feature = "device")]
fn default_opener() -> Option<Box<dyn DeviceOpener>> {
    Some(Box::new(crate::device::CpalOpener::new()))
}

#[cfg(not(feature = "device"))]
fn default_opener() -> Option<Box<dyn DeviceOpener>> {
    None
}

/// The audio engine. See the module docs; `start` returns at once whatever the machine has.
pub struct AudioEngine {
    shared: Arc<Shared>,
    /// `None` when audio is off: nothing is built, queued or kept.
    commands: Option<Sender<Command>>,
    /// The mixer, for an engine the caller drives with [`AudioEngine::pump`].
    manual: Option<MixerHandle>,
    stop: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<()>>,
}

impl AudioEngine {
    /// Start with the machine's real output device (when `config.mode` is [`Mode::Auto`]).
    pub fn start(config: AudioConfig) -> Self {
        Self::start_with(config, default_opener())
    }

    /// Start with `opener` as the way to a device (`None`: this build has none).
    pub fn start_with(config: AudioConfig, opener: Option<Box<dyn DeviceOpener>>) -> Self {
        if config.mode == Mode::Off {
            return Self::off(&config);
        }
        let (shared, commands, mixer) = Self::parts(&config, Backend::Opening);
        let stop = Arc::new(AtomicBool::new(false));
        let supervisor = Supervisor {
            mixer,
            shared: shared.clone(),
            stop: stop.clone(),
            log: config.logger.clone(),
            null_rate: config.sample_rate,
            open_timeout: config.open_timeout,
            stall_timeout: config.stall_timeout,
            degraded: false,
        };
        let mode = config.mode;
        match thread::Builder::new().name("fmv-audio".into()).spawn(move || supervisor.run(mode, opener)) {
            Ok(thread) => Self { shared, commands: Some(commands), manual: None, stop, supervisor: Some(thread) },
            Err(error) => {
                (config.logger)(&format!("could not start the audio thread ({error}); continuing silently"));
                Self::off(&config)
            }
        }
    }

    /// An engine on the null sink that does not run by itself: the caller renders time with
    /// [`AudioEngine::pump`]. For tests and offline rendering; no thread, no device, no clock.
    pub fn start_manual(config: AudioConfig) -> Self {
        if config.mode == Mode::Off {
            return Self::off(&config);
        }
        let (shared, commands, mixer) = Self::parts(&config, Backend::Null);
        Self { shared, commands: Some(commands), manual: Some(mixer), stop: Arc::new(AtomicBool::new(false)), supervisor: None }
    }

    fn parts(config: &AudioConfig, backend: Backend) -> (Arc<Shared>, Sender<Command>, MixerHandle) {
        let shared = Shared::new(config.volumes, config.muted, config.sample_rate, backend);
        let (commands, queue) = mpsc::channel();
        let mixer = MixerHandle::new(RenderHost::new(Mixer::new(config.sample_rate, shared.clone()), queue, shared.clone()));
        (shared, commands, mixer)
    }

    fn off(config: &AudioConfig) -> Self {
        let shared = Shared::new(config.volumes, config.muted, config.sample_rate, Backend::Off);
        Self { shared, commands: None, manual: None, stop: Arc::new(AtomicBool::new(false)), supervisor: None }
    }

    /// Start `source` as the music, crossfading from whatever plays (see [`PlayOptions`]). Asking
    /// for the track that is already playing starts it again: the caller decides whether that is
    /// wanted (check [`AudioStatus::music`] first). Building the voice allocates, so this is for the
    /// game's thread, never the audio callback.
    pub fn play_music(&self, track: TrackId, source: Box<dyn MusicSource>, options: PlayOptions) {
        let Some(commands) = &self.commands else { return };
        let voice = Box::new(MusicVoice::new(track, source, options));
        let _ = commands.send(Command::PlayMusic { voice, fade: options.fade });
    }

    /// Fade the music out over `fade` (zero: cut it). The status shows no music from the moment the
    /// mixer applies this.
    pub fn stop_music(&self, fade: Duration) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::StopMusic { fade });
        }
    }

    /// Play a sound effect: `gain` in `0.0..=1.0`, `pan` from -1 (left) to 1 (right). Sixteen can
    /// play at once; a seventeenth cuts the oldest short.
    pub fn play_sfx(&self, clip: &SfxClip, gain: f32, pan: f32) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::PlaySfx(SfxVoice::new(clip.clone(), gain, pan)));
        }
    }

    pub fn stop_sfx(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::StopSfx);
        }
    }

    /// Set all three levels (each clamped into `0.0..=1.0`). They take effect within about 20 ms,
    /// without a click.
    pub fn set_volumes(&self, volumes: Volumes) {
        self.shared.set_volumes(volumes);
    }

    pub fn set_master_volume(&self, level: f32) {
        self.set_volumes(Volumes { master: level, ..self.shared.volumes() });
    }

    pub fn set_music_volume(&self, level: f32) {
        self.set_volumes(Volumes { music: level, ..self.shared.volumes() });
    }

    pub fn set_sfx_volume(&self, level: f32) {
        self.set_volumes(Volumes { sfx: level, ..self.shared.volumes() });
    }

    /// Mute or unmute: the music keeps moving while muted, so unmuting joins it where it is.
    pub fn set_muted(&self, muted: bool) {
        self.shared.set_muted(muted);
    }

    pub fn status(&self) -> AudioStatus {
        self.shared.snapshot()
    }

    /// Render `frames` stereo frames of an engine made with [`AudioEngine::start_manual`] and return
    /// them (interleaved); anything else returns nothing, because there the clock or the device
    /// does the rendering.
    pub fn pump(&self, frames: usize) -> Vec<f32> {
        let Some(mixer) = &self.manual else { return Vec::new() };
        let mut out = vec![0.0; frames * 2];
        mixer.render_stereo(&mut out);
        out
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(thread) = self.supervisor.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            // A thread stuck in a wedged sound server is left to finish on its own.
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

enum State {
    /// The opener thread is working; its answer comes on `result`.
    Opening { result: Receiver<Result<OpenedDevice, DeviceError>>, since: Instant },
    /// A device is rendering; `beat` watches its callbacks.
    Device { guard: Box<dyn Send>, beat: Heartbeat },
    /// No device: render what the clock says is due. `carry` is the fraction of a frame left over.
    Null { last: Instant, carry: f64 },
}

/// Watches a device's callback counter for one that stops moving.
struct Heartbeat {
    seen: u64,
    changed_at: Instant,
}

impl Heartbeat {
    fn new(calls: u64, now: Instant) -> Self {
        Self { seen: calls, changed_at: now }
    }

    /// Note the counter's value at `now`: how long it has been since it last changed.
    fn quiet_for(&mut self, calls: u64, now: Instant) -> Duration {
        if calls != self.seen {
            self.seen = calls;
            self.changed_at = now;
        }
        now.saturating_duration_since(self.changed_at)
    }
}

struct Supervisor {
    mixer: MixerHandle,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    log: Logger,
    null_rate: u32,
    open_timeout: Duration,
    stall_timeout: Duration,
    /// The one note about going silent has been written.
    degraded: bool,
}

impl Supervisor {
    fn run(mut self, mode: Mode, opener: Option<Box<dyn DeviceOpener>>) {
        let mut scratch = vec![0.0f32; NULL_CHUNK_FRAMES * 2];
        let mut state = match (mode, opener) {
            (Mode::Auto, Some(opener)) => self.begin_open(opener),
            (Mode::Auto, None) => {
                self.degrade("this build has no audio output support");
                self.enter_null()
            }
            _ => self.enter_null(),
        };
        while !self.stop.load(Relaxed) {
            state = self.step(state, &mut scratch);
            thread::sleep(TICK);
        }
        // Closes the device, if there is one.
        drop(state);
    }

    /// The one note about going silent; later reasons are not repeated.
    fn degrade(&mut self, why: &str) {
        if !self.degraded {
            self.degraded = true;
            (self.log)(&format!("{why}; continuing silently"));
        }
    }

    fn begin_open(&mut self, opener: Box<dyn DeviceOpener>) -> State {
        let (answer, result) = mpsc::channel();
        let link = DeviceLink { mixer: self.mixer.clone(), shared: self.shared.clone() };
        let spawned = thread::Builder::new().name("fmv-audio-open".into()).spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| opener.open(link)))
                .unwrap_or_else(|_| Err(DeviceError::Backend("the audio backend panicked while opening the device".into())));
            // The supervisor may have given up on us: then the stream just drops here.
            let _ = answer.send(outcome);
        });
        match spawned {
            Ok(_) => {
                self.shared.set_backend(Backend::Opening);
                State::Opening { result, since: Instant::now() }
            }
            Err(error) => {
                self.degrade(&format!("could not start the device opener ({error})"));
                self.enter_null()
            }
        }
    }

    fn enter_device(&mut self, device: OpenedDevice) -> State {
        self.shared.set_device(Some(device.name.clone()));
        self.shared.set_backend(Backend::Device);
        (self.log)(&format!("playing through \"{}\" ({} Hz, {} channels)", device.name, device.sample_rate, device.channels));
        State::Device { guard: device.guard, beat: Heartbeat::new(self.shared.callbacks(), Instant::now()) }
    }

    fn enter_null(&mut self) -> State {
        self.mixer.set_output_rate(self.null_rate);
        self.shared.set_device(None);
        self.shared.set_backend(Backend::Null);
        State::Null { last: Instant::now(), carry: 0.0 }
    }

    fn step(&mut self, state: State, scratch: &mut [f32]) -> State {
        match state {
            State::Opening { result, since } => {
                // Nothing renders yet: apply the queue so it cannot pile up behind a slow device.
                self.mixer.drain_commands();
                match result.try_recv() {
                    Ok(Ok(device)) => self.enter_device(device),
                    Ok(Err(DeviceError::NoDevice)) => {
                        self.degrade("no audio output device");
                        self.enter_null()
                    }
                    Ok(Err(error)) => {
                        self.degrade(&format!("cannot use the audio output device ({error})"));
                        self.enter_null()
                    }
                    Err(TryRecvError::Empty) if since.elapsed() < self.open_timeout => State::Opening { result, since },
                    Err(TryRecvError::Empty) => {
                        self.degrade("the output device did not answer in time");
                        self.enter_null()
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.degrade("the device opener stopped without an answer");
                        self.enter_null()
                    }
                }
            }
            State::Device { guard, mut beat } => {
                let quiet = beat.quiet_for(self.shared.callbacks(), Instant::now());
                let reason = if self.shared.lost() {
                    Some("the output device went away")
                } else if quiet > self.stall_timeout {
                    Some("the output device stopped asking for audio")
                } else {
                    None
                };
                match reason {
                    None => State::Device { guard, beat },
                    Some(reason) => {
                        self.degrade(reason);
                        // Stopping a stream can wait on the very thing that failed: never here.
                        let _ = thread::Builder::new().name("fmv-audio-reap".into()).spawn(move || drop(guard));
                        self.enter_null()
                    }
                }
            }
            State::Null { last, mut carry } => {
                let now = Instant::now();
                let elapsed = now.duration_since(last).min(MAX_CATCH_UP);
                carry += elapsed.as_secs_f64() * f64::from(self.shared.rate());
                let whole = carry.floor();
                carry -= whole;
                let mut due = whole as usize;
                while due > 0 {
                    let frames = due.min(NULL_CHUNK_FRAMES);
                    self.mixer.render_stereo(&mut scratch[..frames * 2]);
                    due -= frames;
                }
                State::Null { last: now, carry }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ToneSource;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// Long enough for a loaded machine, short enough to fail a hang.
    const PATIENCE: Duration = Duration::from_secs(20);

    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn lines() -> (Logger, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let logger: Logger = Arc::new(move |line| sink.lock().expect("log").push(line.to_string()));
        (logger, seen)
    }

    fn config(logger: Logger) -> AudioConfig {
        AudioConfig { logger, ..AudioConfig::default() }
    }

    fn tone() -> Box<dyn MusicSource> {
        Box::new(ToneSource::new(44_100, 440.0, 0.5, None))
    }

    fn hard(repeat: bool) -> PlayOptions {
        PlayOptions { fade: Duration::ZERO, repeat, gain: 1.0 }
    }

    /// Opens nothing: there is no device.
    struct NoDevice;

    impl DeviceOpener for NoDevice {
        fn open(self: Box<Self>, _link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
            Err(DeviceError::NoDevice)
        }
    }

    /// Must never be asked.
    struct MustNotOpen;

    impl DeviceOpener for MustNotOpen {
        fn open(self: Box<Self>, _link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
            panic!("a device was opened where none may be");
        }
    }

    struct Panics;

    impl DeviceOpener for Panics {
        fn open(self: Box<Self>, _link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
            panic!("the backend blew up");
        }
    }

    /// Flags when it is dropped, i.e. when the "stream" is stopped.
    struct Stopped(Arc<AtomicBool>);

    impl Drop for Stopped {
        fn drop(&mut self) {
            self.0.store(true, Relaxed);
        }
    }

    /// A device that opens at once; the test plays the part of its callback through `handle`.
    #[derive(Default, Clone)]
    struct Fake {
        handle: Arc<Mutex<Option<MixerHandle>>>,
        reporter: Arc<Mutex<Option<LostReporter>>>,
        stopped: Arc<AtomicBool>,
        opened: Arc<AtomicUsize>,
    }

    impl DeviceOpener for Fake {
        fn open(self: Box<Self>, link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
            self.opened.fetch_add(1, Relaxed);
            link.mixer().set_output_rate(44_100);
            *self.handle.lock().expect("handle") = Some(link.mixer());
            *self.reporter.lock().expect("reporter") = Some(link.lost_reporter());
            Ok(OpenedDevice { name: "fake output".into(), sample_rate: 44_100, channels: 2, guard: Box::new(Stopped(self.stopped.clone())) })
        }
    }

    impl Fake {
        /// One device callback's worth of audio.
        fn callback(&self, frames: usize) -> Vec<f32> {
            let mut out = vec![0.0; frames * 2];
            if let Some(handle) = self.handle.lock().expect("handle").as_ref() {
                handle.render_stereo(&mut out);
            }
            out
        }
    }

    /// Blocks until released, then opens like [`Fake`].
    struct Gate {
        release: Receiver<()>,
        inner: Fake,
    }

    impl DeviceOpener for Gate {
        fn open(self: Box<Self>, link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
            let _ = self.release.recv();
            Box::new(self.inner).open(link)
        }
    }

    #[test]
    fn the_engine_is_shareable_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AudioEngine>();
        assert_send_sync::<MixerHandle>();
    }

    #[test]
    fn off_builds_and_opens_nothing_and_says_nothing() {
        let (logger, seen) = lines();
        let engine = AudioEngine::start_with(AudioConfig { mode: Mode::Off, muted: true, ..config(logger) }, Some(Box::new(MustNotOpen)));
        engine.play_music(TrackId(1), tone(), hard(true));
        engine.play_sfx(&SfxClip::from_mono(8_000, &[0.5; 10]).expect("clip"), 1.0, 0.0);
        engine.stop_music(Duration::ZERO);
        engine.set_master_volume(0.25);
        engine.set_muted(false);
        let status = engine.status();
        assert_eq!((status.backend, status.music, status.sfx_playing, status.frames_rendered), (Backend::Off, None, 0, 0));
        assert_eq!(status.volumes.master, 0.25, "levels are still kept, for a settings screen");
        assert!(engine.pump(100).is_empty());
        assert!(seen.lock().expect("log").is_empty(), "off is silent in the log too");
        drop(engine);
        let manual_off = AudioEngine::start_manual(AudioConfig { mode: Mode::Off, ..AudioConfig::default() });
        assert_eq!(manual_off.status().backend, Backend::Off);
    }

    #[test]
    fn the_null_mode_never_asks_for_a_device() {
        let (logger, seen) = lines();
        let engine = AudioEngine::start_with(AudioConfig { mode: Mode::Null, ..config(logger) }, Some(Box::new(MustNotOpen)));
        wait_for("the null sink", || engine.status().backend == Backend::Null);
        engine.play_music(TrackId(3), tone(), hard(true));
        wait_for("the track", || engine.status().music == Some(TrackId(3)));
        wait_for("time to pass", || engine.status().frames_rendered > 0);
        assert!(seen.lock().expect("log").is_empty());
    }

    #[test]
    fn a_missing_device_degrades_to_silence_with_exactly_one_log_line() {
        let (logger, seen) = lines();
        let engine = AudioEngine::start_with(config(logger), Some(Box::new(NoDevice)));
        wait_for("the fallback", || engine.status().backend == Backend::Null);
        assert_eq!(engine.status().device, None);

        // Everything still works against the null sink, and nothing more is written.
        engine.play_music(TrackId(1), tone(), hard(true));
        wait_for("track 1", || engine.status().music == Some(TrackId(1)));
        engine.play_music(TrackId(2), tone(), PlayOptions { fade: Duration::from_millis(50), ..hard(true) });
        wait_for("track 2", || engine.status().music == Some(TrackId(2)));
        // Two seconds of effect: long enough that a slow machine cannot miss it playing.
        engine.play_sfx(&SfxClip::from_mono(8_000, &[0.5; 16_000]).expect("clip"), 1.0, 0.0);
        wait_for("the effect to start", || engine.status().sfx_playing == 1);
        wait_for("the effect to end", || engine.status().sfx_playing == 0);
        engine.stop_music(Duration::ZERO);
        wait_for("the stop", || engine.status().music.is_none());

        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 1, "one line, however much happens afterwards: {seen:?}");
        assert_eq!(seen[0], "no audio output device; continuing silently");
    }

    #[test]
    fn a_build_without_device_support_is_silent_with_one_line() {
        let (logger, seen) = lines();
        let engine = AudioEngine::start_with(config(logger), None);
        wait_for("the fallback", || engine.status().backend == Backend::Null);
        assert_eq!(seen.lock().expect("log").as_slice(), ["this build has no audio output support; continuing silently"]);
    }

    #[test]
    fn a_panicking_backend_is_contained_and_is_one_line() {
        let (logger, seen) = lines();
        let engine = AudioEngine::start_with(config(logger), Some(Box::new(Panics)));
        wait_for("the fallback", || engine.status().backend == Backend::Null);
        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("cannot use the audio output device (") && seen[0].contains("panicked while opening"), "{seen:?}");
    }

    #[test]
    fn an_opener_that_never_answers_goes_silent_after_the_timeout_without_blocking_anyone() {
        let (logger, seen) = lines();
        let (_hold, release) = mpsc::channel::<()>();
        let gate = Gate { release, inner: Fake::default() };
        let engine = AudioEngine::start_with(
            AudioConfig { open_timeout: Duration::from_millis(150), ..config(logger) },
            Some(Box::new(gate)),
        );
        // `start` returned at once; the status says it is still trying.
        let first = engine.status().backend;
        assert!(matches!(first, Backend::Opening | Backend::Null), "{first:?}");
        wait_for("the timeout", || engine.status().backend == Backend::Null);
        let seen = seen.lock().expect("log");
        assert_eq!(seen.as_slice(), ["the output device did not answer in time; continuing silently"]);
        drop(seen);
        // Dropping the engine must not wait for the opener thread that is still stuck.
        let started = Instant::now();
        drop(engine);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn commands_sent_while_the_device_is_still_opening_are_applied_not_piled_up() {
        let (logger, _seen) = lines();
        let (release_tx, release) = mpsc::channel::<()>();
        let fake = Fake::default();
        let gate = Gate { release, inner: fake.clone() };
        let engine = AudioEngine::start_with(
            AudioConfig { open_timeout: Duration::from_secs(600), ..config(logger) },
            Some(Box::new(gate)),
        );
        engine.play_music(TrackId(5), tone(), hard(true));
        wait_for("the track to be applied", || engine.status().music == Some(TrackId(5)));
        let status = engine.status();
        assert_eq!((status.backend, status.frames_rendered), (Backend::Opening, 0), "applied, but nothing rendered yet");

        release_tx.send(()).expect("release the opener");
        wait_for("the device", || engine.status().backend == Backend::Device);
        let status = engine.status();
        assert_eq!((status.device.as_deref(), status.sample_rate, status.music), (Some("fake output"), 44_100, Some(TrackId(5))));
        let heard = fake.callback(256);
        assert!(heard.iter().any(|v| v.abs() > 0.1), "the device callback now gets the music");
    }

    #[test]
    fn a_device_that_dies_hands_the_same_music_to_the_null_sink_with_one_line() {
        let (logger, seen) = lines();
        let fake = Fake::default();
        let engine = AudioEngine::start_with(config(logger), Some(Box::new(fake.clone())));
        wait_for("the device", || engine.status().backend == Backend::Device);
        engine.play_music(TrackId(8), tone(), hard(true));
        fake.callback(64);
        assert_eq!(engine.status().music, Some(TrackId(8)));

        let reporter = fake.reporter.lock().expect("reporter").clone().expect("the opener ran");
        reporter.report();
        wait_for("the fallback", || engine.status().backend == Backend::Null);
        wait_for("the dead stream to be stopped", || fake.stopped.load(Relaxed));
        assert_eq!(engine.status().music, Some(TrackId(8)), "the same music, still going");
        let before = engine.status().frames_rendered;
        wait_for("the null sink to keep time", || engine.status().frames_rendered > before + 1_000);
        assert_eq!(engine.status().sample_rate, 48_000, "the null sink's own rate");

        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 2, "the device line, then exactly one about losing it: {seen:?}");
        assert_eq!(seen[0], "playing through \"fake output\" (44100 Hz, 2 channels)");
        assert_eq!(seen[1], "the output device went away; continuing silently");
    }

    #[test]
    fn the_heartbeat_restarts_its_clock_whenever_the_count_moves() {
        let start = Instant::now();
        let mut beat = Heartbeat::new(5, start);
        assert_eq!(beat.quiet_for(5, start + Duration::from_secs(2)), Duration::from_secs(2));
        assert_eq!(beat.quiet_for(6, start + Duration::from_secs(3)), Duration::ZERO, "a callback ran: the clock restarts");
        assert_eq!(beat.quiet_for(6, start + Duration::from_secs(4)), Duration::from_secs(1));
        assert_eq!(beat.quiet_for(6, start), Duration::ZERO, "and it never runs backwards");
    }

    #[test]
    fn a_device_that_never_calls_back_is_treated_as_gone() {
        // Whether a device that keeps calling back stays a device is the heartbeat's rule, tested
        // above without a clock; this is the end to end case, which can only fire late, never early.
        let (logger, seen) = lines();
        let fake = Fake::default();
        let engine = AudioEngine::start_with(
            AudioConfig { stall_timeout: Duration::from_millis(150), ..config(logger) },
            Some(Box::new(fake.clone())),
        );
        wait_for("the device", || engine.status().backend == Backend::Device);
        // The stream's thread "dies": it never calls back.
        wait_for("the stall to be noticed", || engine.status().backend == Backend::Null);
        let seen = seen.lock().expect("log");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1], "the output device stopped asking for audio; continuing silently");
        drop(seen);
        wait_for("the dead stream to be stopped", || fake.stopped.load(Relaxed));
        assert_eq!(fake.opened.load(Relaxed), 1, "no second attempt");
    }

    #[test]
    fn dropping_a_running_engine_stops_its_thread_and_closes_the_device() {
        let (logger, _seen) = lines();
        let fake = Fake::default();
        let engine = AudioEngine::start_with(config(logger), Some(Box::new(fake.clone())));
        wait_for("the device", || engine.status().backend == Backend::Device);
        let started = Instant::now();
        drop(engine);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(fake.stopped.load(Relaxed), "the stream was dropped with the engine");
    }

    #[test]
    fn a_manual_engine_switches_the_track_id_crossfades_and_stops_on_demand() {
        let engine = AudioEngine::start_manual(AudioConfig { sample_rate: 8_000, ..AudioConfig::default() });
        assert_eq!(engine.status().backend, Backend::Null);
        assert_eq!(engine.status().music, None);

        engine.play_music(TrackId(1), tone(), hard(true));
        assert_eq!(engine.status().music, None, "applied by the next pass, not by the call");
        let heard = engine.pump(256);
        assert_eq!(engine.status().music, Some(TrackId(1)));
        assert!(heard.iter().any(|v| v.abs() > 0.05));

        engine.play_music(TrackId(2), tone(), PlayOptions { fade: Duration::from_millis(100), ..hard(true) });
        engine.pump(64);
        assert_eq!(engine.status().music, Some(TrackId(2)), "the new track is the status's at once, while the old one fades");
        engine.pump(2_000);

        engine.stop_music(Duration::from_millis(10));
        engine.pump(8);
        assert_eq!(engine.status().music, None);
        let tail = engine.pump(2_000);
        assert!(tail[400..].iter().all(|&v| v == 0.0), "and it is silent once the fade is over");
        assert_eq!(engine.status().frames_rendered, 256 + 64 + 2_000 + 8 + 2_000);
    }

    #[test]
    fn a_one_shot_track_reports_its_end() {
        let engine = AudioEngine::start_manual(AudioConfig { sample_rate: 1_000, ..AudioConfig::default() });
        let jingle = Box::new(ToneSource::new(1_000, 50.0, 0.5, Some(Duration::from_millis(300))));
        engine.play_music(TrackId(46), jingle, hard(false));
        engine.pump(200);
        assert_eq!((engine.status().music, engine.status().music_ended), (Some(TrackId(46)), 0));
        engine.pump(400);
        assert_eq!((engine.status().music, engine.status().music_ended), (None, 1));
    }

    #[test]
    fn effects_volumes_and_mute_go_through_the_engine() {
        let engine = AudioEngine::start_manual(AudioConfig {
            sample_rate: 48_000,
            volumes: Volumes { master: 1.0, music: 1.0, sfx: 1.0 },
            ..AudioConfig::default()
        });
        let clip = SfxClip::from_mono(48_000, &[0.5; 40_000]).expect("clip");
        engine.play_sfx(&clip, 1.0, 0.0);
        let heard = engine.pump(200);
        assert_eq!(engine.status().sfx_playing, 1);
        assert!((heard[100] - 0.5).abs() < 1e-4);

        engine.set_sfx_volume(0.5);
        engine.set_master_volume(0.5);
        engine.pump(2_000);
        let quieter = engine.pump(100);
        assert!((quieter[50] - 0.125).abs() < 1e-3, "half times half of 0.5: {}", quieter[50]);
        assert_eq!(engine.status().volumes, Volumes { master: 0.5, music: 1.0, sfx: 0.5 });

        engine.set_muted(true);
        assert!(engine.status().muted);
        engine.pump(2_000);
        assert!(engine.pump(100).iter().all(|&v| v == 0.0));
        engine.stop_sfx();
        engine.pump(8);
        assert_eq!(engine.status().sfx_playing, 0);
    }

    #[test]
    fn an_engine_that_starts_muted_stays_silent_but_keeps_the_music_moving() {
        let engine = AudioEngine::start_manual(AudioConfig { sample_rate: 8_000, muted: true, ..AudioConfig::default() });
        engine.play_music(TrackId(2), tone(), hard(true));
        let heard = engine.pump(1_000);
        assert!(heard.iter().all(|&v| v == 0.0));
        assert_eq!(engine.status().music, Some(TrackId(2)));
        engine.set_muted(false);
        engine.pump(2_000);
        assert!(engine.pump(500).iter().any(|v| v.abs() > 0.05));
    }
}
