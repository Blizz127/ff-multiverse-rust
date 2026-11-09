//! The block of atomics the mixer publishes into and the engine reads from, so that neither the
//! audio callback nor the game's thread ever waits for the other.

use crate::types::{AudioStatus, Backend, TrackId, Volumes};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// `Shared::music` when nothing plays.
const NO_TRACK: u64 = u64::MAX;

/// State shared between the game's thread, the supervisor and the audio callback. Everything the
/// callback touches is an atomic; the one `Mutex` (the device name) is for the supervisor and
/// [`Shared::snapshot`] only.
#[derive(Debug)]
pub struct Shared {
    /// `f32` bits.
    master: AtomicU32,
    music_volume: AtomicU32,
    sfx_volume: AtomicU32,
    muted: AtomicBool,
    music: AtomicU64,
    music_ended: AtomicU64,
    sfx: AtomicU32,
    loops: AtomicU32,
    frames: AtomicU64,
    callbacks: AtomicU64,
    underruns: AtomicU64,
    faults: AtomicU32,
    rate: AtomicU32,
    backend: AtomicU8,
    lost: AtomicBool,
    device: Mutex<Option<String>>,
}

impl Shared {
    pub(crate) fn new(volumes: Volumes, muted: bool, rate: u32, backend: Backend) -> Arc<Self> {
        let v = volumes.clamped();
        Arc::new(Self {
            master: AtomicU32::new(v.master.to_bits()),
            music_volume: AtomicU32::new(v.music.to_bits()),
            sfx_volume: AtomicU32::new(v.sfx.to_bits()),
            muted: AtomicBool::new(muted),
            music: AtomicU64::new(NO_TRACK),
            music_ended: AtomicU64::new(0),
            sfx: AtomicU32::new(0),
            loops: AtomicU32::new(0),
            frames: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            faults: AtomicU32::new(0),
            rate: AtomicU32::new(rate),
            backend: AtomicU8::new(backend.to_u8()),
            lost: AtomicBool::new(false),
            device: Mutex::new(None),
        })
    }

    pub(crate) fn volumes(&self) -> Volumes {
        Volumes {
            master: f32::from_bits(self.master.load(Relaxed)),
            music: f32::from_bits(self.music_volume.load(Relaxed)),
            sfx: f32::from_bits(self.sfx_volume.load(Relaxed)),
        }
    }

    pub(crate) fn set_volumes(&self, v: Volumes) {
        let v = v.clamped();
        self.master.store(v.master.to_bits(), Relaxed);
        self.music_volume.store(v.music.to_bits(), Relaxed);
        self.sfx_volume.store(v.sfx.to_bits(), Relaxed);
    }

    pub(crate) fn muted(&self) -> bool {
        self.muted.load(Relaxed)
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Relaxed);
    }

    pub(crate) fn set_music(&self, track: Option<TrackId>) {
        self.music.store(track.map_or(NO_TRACK, |t| u64::from(t.0)), Relaxed);
    }

    pub(crate) fn count_music_end(&self) {
        self.music_ended.fetch_add(1, Relaxed);
    }

    pub(crate) fn set_sfx(&self, playing: usize) {
        self.sfx.store(u32::try_from(playing).unwrap_or(u32::MAX), Relaxed);
    }

    pub(crate) fn set_loops(&self, playing: usize) {
        self.loops.store(u32::try_from(playing).unwrap_or(u32::MAX), Relaxed);
    }

    pub(crate) fn count_frames(&self, frames: usize) {
        self.frames.fetch_add(frames as u64, Relaxed);
    }

    pub(crate) fn count_callback(&self) {
        self.callbacks.fetch_add(1, Relaxed);
    }

    /// Render passes so far: a device that stops calling back shows as a counter that stops moving.
    pub(crate) fn callbacks(&self) -> u64 {
        self.callbacks.load(Relaxed)
    }

    pub(crate) fn count_underrun(&self) {
        self.underruns.fetch_add(1, Relaxed);
    }

    pub(crate) fn count_fault(&self) {
        self.faults.fetch_add(1, Relaxed);
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate.load(Relaxed)
    }

    pub(crate) fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Relaxed);
    }

    pub(crate) fn backend(&self) -> Backend {
        Backend::from_u8(self.backend.load(Relaxed))
    }

    pub(crate) fn set_backend(&self, backend: Backend) {
        self.backend.store(backend.to_u8(), Relaxed);
    }

    /// A device backend calls this from its error callback when the stream is gone for good (see
    /// [`crate::DeviceLink::report_lost`]); the supervisor then moves the mixer to the null sink.
    pub(crate) fn report_lost(&self) {
        self.lost.store(true, Relaxed);
    }

    pub(crate) fn lost(&self) -> bool {
        self.lost.load(Relaxed)
    }

    pub(crate) fn set_device(&self, name: Option<String>) {
        if let Ok(mut device) = self.device.lock() {
            *device = name;
        }
    }

    pub(crate) fn snapshot(&self) -> AudioStatus {
        let music = self.music.load(Relaxed);
        AudioStatus {
            backend: self.backend(),
            device: self.device.lock().ok().and_then(|d| d.clone()),
            sample_rate: self.rate(),
            music: u32::try_from(music).ok().map(TrackId),
            music_ended: self.music_ended.load(Relaxed),
            sfx_playing: self.sfx.load(Relaxed),
            loops_playing: self.loops.load(Relaxed),
            volumes: self.volumes(),
            muted: self.muted(),
            frames_rendered: self.frames.load(Relaxed),
            underruns: self.underruns.load(Relaxed),
            faults: self.faults.load(Relaxed),
        }
    }
}
