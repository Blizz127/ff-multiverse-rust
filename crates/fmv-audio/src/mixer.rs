//! The mixer: one music voice playing, a few fading out, the sound-effect voices and the three
//! gains, rendered into interleaved stereo `f32`.
//!
//! It runs on the audio thread, so it does no I/O, takes no locks and allocates nothing while
//! rendering (voices are built by the caller and arrive through [`Command`]; the only allocation
//! left is freeing a finished voice). Time is counted in frames, never in wall-clock time, so a
//! test renders exactly the frames it asks for.
//!
//! * **Resampling.** Every voice has its own rate; output is a linear interpolation between
//!   neighbouring source frames, stepped by `source rate / output rate`.
//! * **Looping** happens in the source's own frames, before the resampler, so a loop seam is
//!   sample-exact whatever the output rate is ([`LoopReader`]).
//! * **Crossfades** are equal power (`rise(p)^2 + fall(p)^2 == 1`) with a gentle start and end.
//! * **Gains** (master, music, effects, mute) move to their target over about 20 ms, so a volume
//!   change or a mute never clicks.
//! * The sum is hard limited to `-1.0..=1.0`, and anything that is not a finite number (a broken
//!   decoder) is written as silence.

use crate::source::{LoopPoints, MusicSource, Pull, SfxClip};
use crate::status::Shared;
use crate::types::{PlayOptions, TrackId};
use std::sync::Arc;
use std::time::Duration;

/// Frames per mixing block: fades and gains are stepped once per block and ramped across it.
pub(crate) const BLOCK_FRAMES: usize = 128;
/// Sound effects that can play at once; a new one cuts the oldest short.
pub(crate) const MAX_SFX_VOICES: usize = 16;
/// Tracks that can be fading out at once; a third cuts the oldest short.
pub(crate) const MAX_FADING_MUSIC: usize = 2;
/// Source frames a music voice reads at a time.
const CHUNK_FRAMES: usize = 1024;
/// How long a volume change or a mute takes to settle.
const GAIN_RAMP_SECONDS: f32 = 0.02;

/// What the engine asks of the mixer. Voices are built by the caller, on its own thread.
pub(crate) enum Command {
    PlayMusic { voice: Box<MusicVoice>, fade: Duration },
    StopMusic { fade: Duration },
    PlaySfx(SfxVoice),
    StopSfx,
}

// ---------------------------------------------------------------------------------------------
// Fades
// ---------------------------------------------------------------------------------------------

fn smooth(p: f32) -> f32 {
    p * p * (3.0 - 2.0 * p)
}

/// The gain of a track fading in, `p` of the way through: 0 to 1.
pub(crate) fn rise(p: f32) -> f32 {
    smooth(p.clamp(0.0, 1.0)).sqrt()
}

/// The gain of a track fading out, `p` of the way through: 1 to 0. Together with [`rise`] the
/// power stays constant: `rise(p)^2 + fall(p)^2 == 1`.
pub(crate) fn fall(p: f32) -> f32 {
    (1.0 - smooth(p.clamp(0.0, 1.0))).sqrt()
}

#[derive(Clone, Copy, Debug)]
enum Env {
    Steady,
    In { done: u64, total: u64 },
    /// Fading out from the level the voice had when the fade began.
    Out { from: f32, done: u64, total: u64 },
}

// ---------------------------------------------------------------------------------------------
// Music
// ---------------------------------------------------------------------------------------------

/// Presents a source as one endless (looping) or finite stream of frames, with the loop applied
/// in the source's own frames.
struct LoopReader {
    src: Box<dyn MusicSource>,
    looping: bool,
    start: u64,
    /// `None`: the end of the track.
    end: Option<u64>,
    /// The next source frame's index.
    pos: u64,
    ended: bool,
}

impl LoopReader {
    fn rewind(&mut self) -> bool {
        if self.src.seek(self.start) {
            self.pos = self.start;
            true
        } else {
            false
        }
    }

    /// Fill `out` (interleaved stereo) as far as the source allows, jumping from the loop end back
    /// to the loop start inside the same call so the seam has no gap.
    fn read(&mut self, out: &mut [f32]) -> Pull {
        let want = out.len() / 2;
        let mut got = 0usize;
        // Rewinds that produced no frame since the last frame: a loop with nothing in it must end,
        // not spin.
        let mut empty_rewinds = 0u8;
        while got < want && !self.ended {
            let mut n = want - got;
            if let (true, Some(end)) = (self.looping, self.end) {
                if self.pos >= end {
                    empty_rewinds += 1;
                    if empty_rewinds > 2 || !self.rewind() {
                        self.ended = true;
                        break;
                    }
                    continue;
                }
                n = n.min(usize::try_from(end - self.pos).unwrap_or(usize::MAX));
            }
            match self.src.read(&mut out[got * 2..(got + n) * 2]) {
                Pull::Frames(k) if k > 0 => {
                    let k = k.min(n);
                    self.pos += k as u64;
                    got += k;
                    empty_rewinds = 0;
                }
                Pull::Frames(_) | Pull::Pending => break,
                Pull::End => {
                    empty_rewinds += 1;
                    if !(self.looping && empty_rewinds <= 2 && self.rewind()) {
                        self.ended = true;
                    }
                }
            }
        }
        if got > 0 {
            Pull::Frames(got)
        } else if self.ended {
            Pull::End
        } else {
            Pull::Pending
        }
    }
}

enum Fetch {
    Frame([f32; 2]),
    Pending,
    End,
}

/// What one block of a voice did.
struct BlockOutcome {
    underrun: bool,
    finished: bool,
}

/// One track being played: its source (looped), a resampler and a fade envelope.
pub(crate) struct MusicVoice {
    track: TrackId,
    reader: LoopReader,
    src_rate: u32,
    gain: f32,
    env: Env,
    /// The envelope's level at the end of the last block (what a fade out starts from).
    level: f32,
    /// The two source frames the output is interpolated between, and how far past `a` it is.
    a: [f32; 2],
    b: [f32; 2],
    t: f64,
    /// Silent frames shifted in after the source ended; two of them and the voice is done.
    padding: u8,
    exhausted: bool,
    chunk: Vec<f32>,
    /// Frames in `chunk` and the next one to take.
    chunk_len: usize,
    chunk_pos: usize,
}

impl MusicVoice {
    pub(crate) fn new(track: TrackId, source: Box<dyn MusicSource>, opts: PlayOptions) -> Self {
        let src_rate = source.sample_rate().max(1);
        let (start, end) = match source.loop_points() {
            // A loop that is empty or runs backwards is no loop at all.
            Some(LoopPoints { start, end }) if end.is_none_or(|e| e > start) => (start, end),
            _ => (0, None),
        };
        Self {
            track,
            reader: LoopReader { src: source, looping: opts.repeat, start, end, pos: 0, ended: false },
            src_rate,
            gain: if opts.gain.is_nan() { 1.0 } else { opts.gain.clamp(0.0, 1.0) },
            env: Env::Steady,
            level: 1.0,
            a: [0.0; 2],
            b: [0.0; 2],
            // Two frames have to come in before the first one can be played.
            t: 2.0,
            padding: 0,
            exhausted: false,
            chunk: vec![0.0; CHUNK_FRAMES * 2],
            chunk_len: 0,
            chunk_pos: 0,
        }
    }

    fn fade_in(&mut self, total: u64) {
        self.env = Env::In { done: 0, total };
        self.level = 0.0;
    }

    fn fade_out(&mut self, total: u64) {
        self.env = Env::Out { from: self.level, done: 0, total };
    }

    /// Step the envelope over `n` frames: the gain at the start and at the end of them, and whether
    /// a fade out is over.
    fn advance_env(&mut self, n: usize) -> (f32, f32, bool) {
        let g0 = self.level;
        let n = n as u64;
        let (env, g1, over) = match self.env {
            Env::Steady => (Env::Steady, 1.0, false),
            Env::In { done, total } => {
                let done = (done + n).min(total);
                if done >= total {
                    (Env::Steady, 1.0, false)
                } else {
                    (Env::In { done, total }, rise(done as f32 / total as f32), false)
                }
            }
            Env::Out { from, done, total } => {
                let done = (done + n).min(total);
                let g = from * fall(done as f32 / total as f32);
                (Env::Out { from, done, total }, g, done >= total)
            }
        };
        self.env = env;
        self.level = g1;
        (g0, g1, over)
    }

    fn fetch(&mut self) -> Fetch {
        if self.chunk_pos >= self.chunk_len {
            match self.reader.read(&mut self.chunk) {
                Pull::Frames(n) => {
                    self.chunk_len = n.min(CHUNK_FRAMES);
                    self.chunk_pos = 0;
                }
                Pull::Pending => return Fetch::Pending,
                Pull::End => return Fetch::End,
            }
        }
        let i = self.chunk_pos * 2;
        self.chunk_pos += 1;
        Fetch::Frame([self.chunk[i], self.chunk[i + 1]])
    }

    /// Add `n` frames of this track, at the output rate `out_rate`, into `acc` (interleaved stereo).
    fn render_block(&mut self, out_rate: u32, n: usize, acc: &mut [f32]) -> BlockOutcome {
        let (g0, g1, faded_out) = self.advance_env(n);
        if self.exhausted {
            return BlockOutcome { underrun: false, finished: true };
        }
        let step = (g1 - g0) / n as f32;
        let ratio = f64::from(self.src_rate) / f64::from(out_rate.max(1));
        let mut underrun = false;
        'frames: for i in 0..n {
            while self.t >= 1.0 {
                match self.fetch() {
                    Fetch::Frame(f) => {
                        self.t -= 1.0;
                        self.a = self.b;
                        self.b = f;
                    }
                    Fetch::Pending => {
                        underrun = true;
                        break 'frames;
                    }
                    Fetch::End => {
                        // Run out through one more source frame so the last one is played whole
                        // and fades into silence instead of stopping dead.
                        self.t -= 1.0;
                        self.a = self.b;
                        self.b = [0.0; 2];
                        self.padding += 1;
                        if self.padding >= 2 {
                            self.exhausted = true;
                            break 'frames;
                        }
                    }
                }
            }
            let t = self.t as f32;
            let g = (g0 + step * (i + 1) as f32) * self.gain;
            for ch in 0..2 {
                acc[i * 2 + ch] += (self.a[ch] + (self.b[ch] - self.a[ch]) * t) * g;
            }
            self.t += ratio;
        }
        BlockOutcome { underrun, finished: self.exhausted || faded_out }
    }
}

// ---------------------------------------------------------------------------------------------
// Sound effects
// ---------------------------------------------------------------------------------------------

/// One sound effect being played, from memory.
pub(crate) struct SfxVoice {
    clip: SfxClip,
    /// Position in the clip, in clip frames.
    pos: f64,
    gain_left: f32,
    gain_right: f32,
}

impl SfxVoice {
    /// `gain` in `0.0..=1.0`; `pan` from -1 (left) to 1 (right) is a balance: the far side is turned
    /// down, the near side stays as it is.
    pub(crate) fn new(clip: SfxClip, gain: f32, pan: f32) -> Self {
        let gain = if gain.is_nan() { 1.0 } else { gain.clamp(0.0, 1.0) };
        let pan = if pan.is_nan() { 0.0 } else { pan.clamp(-1.0, 1.0) };
        Self { clip, pos: 0.0, gain_left: gain * (1.0 - pan).min(1.0), gain_right: gain * (1.0 + pan).min(1.0) }
    }

    /// Add `n` frames into `acc`; `true` once the clip is over.
    fn render_block(&mut self, out_rate: u32, n: usize, acc: &mut [f32]) -> bool {
        let step = f64::from(self.clip.rate()) / f64::from(out_rate.max(1));
        let samples = self.clip.samples();
        let frames = samples.len() / 2;
        for i in 0..n {
            let at = self.pos as usize;
            if at >= frames {
                return true;
            }
            let frac = (self.pos - at as f64) as f32;
            let (l0, r0) = (samples[at * 2], samples[at * 2 + 1]);
            let (l1, r1) = if at + 1 < frames { (samples[at * 2 + 2], samples[at * 2 + 3]) } else { (0.0, 0.0) };
            acc[i * 2] += (l0 + (l1 - l0) * frac) * self.gain_left;
            acc[i * 2 + 1] += (r0 + (r1 - r0) * frac) * self.gain_right;
            self.pos += step;
        }
        self.pos as usize >= frames
    }
}

// ---------------------------------------------------------------------------------------------
// The mixer
// ---------------------------------------------------------------------------------------------

/// A gain that walks to its target instead of jumping.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    current: f32,
    target: f32,
}

impl Ramp {
    fn at(level: f32) -> Self {
        Self { current: level, target: level }
    }

    /// Move toward the target by at most `max_step`; the level at the block's start and its end.
    fn advance(&mut self, max_step: f32) -> (f32, f32) {
        let from = self.current;
        self.current += (self.target - self.current).clamp(-max_step, max_step);
        (from, self.current)
    }
}

pub(crate) struct Mixer {
    rate: u32,
    shared: Arc<Shared>,
    music: Option<Box<MusicVoice>>,
    fading: Vec<MusicVoice>,
    sfx: Vec<SfxVoice>,
    master: Ramp,
    music_gain: Ramp,
    sfx_gain: Ramp,
}

impl Mixer {
    /// A mixer rendering at `rate` Hz that reads its levels from `shared` and reports into it.
    pub(crate) fn new(rate: u32, shared: Arc<Shared>) -> Self {
        let rate = rate.max(1);
        shared.set_rate(rate);
        shared.set_music(None);
        shared.set_sfx(0);
        let v = shared.volumes();
        let master = if shared.muted() { 0.0 } else { v.master };
        Self {
            rate,
            music: None,
            fading: Vec::with_capacity(MAX_FADING_MUSIC),
            sfx: Vec::with_capacity(MAX_SFX_VOICES),
            master: Ramp::at(master),
            music_gain: Ramp::at(v.music),
            sfx_gain: Ramp::at(v.sfx),
            shared,
        }
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    /// Render at another rate from now on (the device changed): every voice follows, because each
    /// works out its own step per block.
    pub(crate) fn set_rate(&mut self, rate: u32) {
        self.rate = rate.max(1);
        self.shared.set_rate(self.rate);
    }

    fn fade_frames(&self, fade: Duration) -> u64 {
        (fade.as_secs_f64() * f64::from(self.rate)).round() as u64
    }

    /// The playing track leaves: it fades out over `frames`, or is cut at once when that is zero.
    fn retire_music(&mut self, frames: u64) {
        if let Some(mut old) = self.music.take() {
            if frames > 0 {
                old.fade_out(frames);
                if self.fading.len() >= MAX_FADING_MUSIC {
                    self.fading.remove(0);
                }
                self.fading.push(*old);
            }
        }
    }

    pub(crate) fn apply(&mut self, command: Command) {
        match command {
            Command::PlayMusic { mut voice, fade } => {
                let frames = self.fade_frames(fade);
                self.retire_music(frames);
                if frames > 0 {
                    voice.fade_in(frames);
                }
                self.shared.set_music(Some(voice.track));
                self.music = Some(voice);
            }
            Command::StopMusic { fade } => {
                let frames = self.fade_frames(fade);
                self.retire_music(frames);
                self.shared.set_music(None);
            }
            Command::PlaySfx(voice) => {
                if self.sfx.len() >= MAX_SFX_VOICES {
                    self.sfx.remove(0);
                }
                self.sfx.push(voice);
                self.shared.set_sfx(self.sfx.len());
            }
            Command::StopSfx => {
                self.sfx.clear();
                self.shared.set_sfx(0);
            }
        }
    }

    /// Fill `out` with interleaved stereo frames (an odd trailing sample is zeroed).
    pub(crate) fn render(&mut self, out: &mut [f32]) {
        let levels = self.shared.volumes();
        self.master.target = if self.shared.muted() { 0.0 } else { levels.master };
        self.music_gain.target = levels.music;
        self.sfx_gain.target = levels.sfx;
        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(BLOCK_FRAMES);
            self.render_block(&mut out[done * 2..(done + n) * 2], n);
            done += n;
        }
        out[frames * 2..].fill(0.0);
        self.shared.count_frames(frames);
    }

    fn render_block(&mut self, out: &mut [f32], n: usize) {
        let mut music = [0.0f32; BLOCK_FRAMES * 2];
        let mut sfx = [0.0f32; BLOCK_FRAMES * 2];
        let (music_acc, sfx_acc) = (&mut music[..n * 2], &mut sfx[..n * 2]);
        let rate = self.rate;
        let mut underrun = false;

        if let Some(voice) = self.music.as_mut() {
            let outcome = voice.render_block(rate, n, music_acc);
            underrun |= outcome.underrun;
            if outcome.finished {
                // The track ran out (a one-shot, or a loop that cannot seek).
                self.music = None;
                self.shared.set_music(None);
                self.shared.count_music_end();
            }
        }
        self.fading.retain_mut(|voice| {
            let outcome = voice.render_block(rate, n, music_acc);
            underrun |= outcome.underrun;
            !outcome.finished
        });
        if underrun {
            self.shared.count_underrun();
        }
        let before = self.sfx.len();
        self.sfx.retain_mut(|voice| !voice.render_block(rate, n, sfx_acc));
        if self.sfx.len() != before {
            self.shared.set_sfx(self.sfx.len());
        }

        let max_step = n as f32 / (GAIN_RAMP_SECONDS * rate as f32);
        let (m0, m1) = self.music_gain.advance(max_step);
        let (s0, s1) = self.sfx_gain.advance(max_step);
        let (g0, g1) = self.master.advance(max_step);
        let inv = 1.0 / n as f32;
        for i in 0..n {
            let k = (i + 1) as f32 * inv;
            let music_gain = m0 + (m1 - m0) * k;
            let sfx_gain = s0 + (s1 - s0) * k;
            let master = g0 + (g1 - g0) * k;
            for ch in 0..2 {
                let v = (music_acc[i * 2 + ch] * music_gain + sfx_acc[i * 2 + ch] * sfx_gain) * master;
                out[i * 2 + ch] = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn fading_count(&self) -> usize {
        self.fading.len()
    }

    #[cfg(test)]
    pub(crate) fn sfx_count(&self) -> usize {
        self.sfx.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Backend, Volumes};
    use std::collections::VecDeque;

    const RATE: u32 = 1_000;

    fn full() -> Volumes {
        Volumes { master: 1.0, music: 1.0, sfx: 1.0 }
    }

    fn mixer_with(volumes: Volumes) -> (Mixer, Arc<Shared>) {
        let shared = Shared::new(volumes, false, RATE, Backend::Null);
        (Mixer::new(RATE, shared.clone()), shared)
    }

    fn mixer() -> (Mixer, Arc<Shared>) {
        mixer_with(full())
    }

    /// The same value on both sides, forever or for `frames` frames.
    struct Constant {
        value: f32,
        rate: u32,
        frames: Option<u64>,
        pos: u64,
    }

    impl Constant {
        fn boxed(value: f32) -> Box<dyn MusicSource> {
            Box::new(Constant { value, rate: RATE, frames: None, pos: 0 })
        }
    }

    impl MusicSource for Constant {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn read(&mut self, out: &mut [f32]) -> Pull {
            let asked = out.len() / 2;
            let left = self.frames.map_or(u64::MAX, |f| f.saturating_sub(self.pos));
            if left == 0 {
                return Pull::End;
            }
            let n = usize::try_from(left).map_or(asked, |l| l.min(asked));
            out[..n * 2].fill(self.value);
            self.pos += n as u64;
            Pull::Frames(n)
        }
        fn seek(&mut self, frame: u64) -> bool {
            self.pos = frame;
            true
        }
    }

    /// Frame `i` is `i / 1024` (exact in `f32`), so the output says which source frame it is.
    struct Ramp1024 {
        total: u64,
        rate: u32,
        pos: u64,
        loop_points: Option<LoopPoints>,
    }

    impl Ramp1024 {
        fn boxed(total: u64, rate: u32, loop_points: Option<LoopPoints>) -> Box<dyn MusicSource> {
            Box::new(Ramp1024 { total, rate, pos: 0, loop_points })
        }
    }

    impl MusicSource for Ramp1024 {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn read(&mut self, out: &mut [f32]) -> Pull {
            if self.pos >= self.total {
                return Pull::End;
            }
            let n = usize::try_from(self.total - self.pos).map_or(out.len() / 2, |l| l.min(out.len() / 2));
            for (k, frame) in out.as_chunks_mut::<2>().0.iter_mut().take(n).enumerate() {
                frame.fill((self.pos + k as u64) as f32 / 1024.0);
            }
            self.pos += n as u64;
            Pull::Frames(n)
        }
        fn seek(&mut self, frame: u64) -> bool {
            self.pos = frame;
            true
        }
        fn loop_points(&self) -> Option<LoopPoints> {
            self.loop_points
        }
    }

    /// Plays a script of outcomes, then ends.
    struct Scripted(VecDeque<Pull>);

    impl MusicSource for Scripted {
        fn sample_rate(&self) -> u32 {
            RATE
        }
        fn read(&mut self, out: &mut [f32]) -> Pull {
            match self.0.pop_front() {
                Some(Pull::Frames(n)) => {
                    let n = n.min(out.len() / 2);
                    out[..n * 2].fill(0.5);
                    Pull::Frames(n)
                }
                Some(other) => other,
                None => Pull::End,
            }
        }
        fn seek(&mut self, _frame: u64) -> bool {
            false
        }
    }

    fn voice(track: u32, source: Box<dyn MusicSource>, opts: PlayOptions) -> Box<MusicVoice> {
        Box::new(MusicVoice::new(TrackId(track), source, opts))
    }

    fn play(m: &mut Mixer, track: u32, source: Box<dyn MusicSource>, opts: PlayOptions) {
        m.apply(Command::PlayMusic { voice: voice(track, source, opts), fade: opts.fade });
    }

    /// Hard cut, no fade.
    fn cut(repeat: bool) -> PlayOptions {
        PlayOptions { fade: Duration::ZERO, repeat, gain: 1.0 }
    }

    fn render(m: &mut Mixer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        m.render(&mut out);
        out
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn nothing_playing_renders_exact_silence_and_counts_frames() {
        let (mut m, shared) = mixer();
        let out = render(&mut m, 300);
        assert!(out.iter().all(|&v| v == 0.0));
        assert_eq!(shared.snapshot().frames_rendered, 300);
        // An odd buffer: the half frame at the end is zeroed, not left as it was.
        let mut odd = vec![9.0f32; 5];
        m.render(&mut odd);
        assert_eq!(odd, vec![0.0; 5]);
    }

    #[test]
    fn a_track_plays_at_its_level_scaled_by_the_music_and_master_volumes() {
        let (mut m, shared) = mixer_with(Volumes { master: 0.5, music: 0.5, sfx: 1.0 });
        play(&mut m, 7, Constant::boxed(0.8), cut(true));
        assert_eq!(shared.snapshot().music, Some(TrackId(7)));
        let out = render(&mut m, 400);
        assert!(out.iter().all(|&v| approx(v, 0.8 * 0.25)), "master times music times the track");
    }

    #[test]
    fn the_per_track_gain_applies_and_a_nan_gain_is_unity() {
        let (mut m, _) = mixer();
        play(&mut m, 1, Constant::boxed(0.8), PlayOptions { gain: 0.5, ..cut(true) });
        assert!(render(&mut m, 200).iter().all(|&v| approx(v, 0.4)));
        play(&mut m, 2, Constant::boxed(0.8), PlayOptions { gain: f32::NAN, ..cut(true) });
        assert!(render(&mut m, 200).iter().all(|&v| approx(v, 0.8)));
    }

    #[test]
    fn mute_ramps_to_exact_silence_and_back_without_a_jump() {
        // At a real output rate, so that the 20 ms ramp is several blocks long.
        let shared = Shared::new(full(), false, 48_000, Backend::Null);
        let mut m = Mixer::new(48_000, shared.clone());
        let steady = Box::new(Constant { value: 1.0, rate: 48_000, frames: None, pos: 0 });
        play(&mut m, 1, steady, cut(true));
        render(&mut m, 2_000);
        shared.set_muted(true);
        let down = left(&render(&mut m, 1_500));
        assert!(down.windows(2).all(|w| w[0] - w[1] <= 0.002), "no step bigger than a ramp step");
        assert!(down[..10].iter().all(|&v| v > 0.98), "it does not drop at once");
        assert!(down[1_100..].iter().all(|&v| v == 0.0), "and it ends in exact silence");
        shared.set_muted(false);
        let up = left(&render(&mut m, 1_500));
        assert!(up[0] < 0.01 && approx(up[1_499], 1.0), "back to full: {} .. {}", up[0], up[1_499]);
    }

    #[test]
    fn volume_changes_settle_within_the_ramp_and_clamp() {
        let (mut m, shared) = mixer();
        play(&mut m, 1, Constant::boxed(1.0), cut(true));
        render(&mut m, 64);
        shared.set_volumes(Volumes { master: 2.0, music: f32::NAN, sfx: -1.0 });
        assert_eq!(shared.volumes(), Volumes { master: 1.0, music: 0.0, sfx: 0.0 });
        let out = left(&render(&mut m, 100));
        assert_eq!(*out.last().unwrap_or(&1.0), 0.0, "music volume NaN is silence once settled");
    }

    #[test]
    fn crossfade_curves_keep_constant_power_and_start_and_end_gently() {
        for k in 0..=100 {
            let p = k as f32 / 100.0;
            assert!(approx(rise(p).powi(2) + fall(p).powi(2), 1.0), "p = {p}");
        }
        assert_eq!((rise(0.0), rise(1.0), fall(0.0), fall(1.0)), (0.0, 1.0, 1.0, 0.0));
        assert!(approx(rise(0.5), fall(0.5)));
        // Gentle: the first percent of the fade moves the gain by well under a percent of full scale.
        assert!(rise(0.01) < 0.03 && fall(0.99) < 0.03);
        // Clamped outside the fade.
        assert_eq!((rise(-1.0), rise(2.0), fall(-1.0), fall(2.0)), (0.0, 1.0, 1.0, 0.0));
    }

    #[test]
    fn a_crossfade_blends_the_two_tracks_and_leaves_only_the_new_one() {
        let (mut m, shared) = mixer();
        let fade = PlayOptions { fade: Duration::from_secs(1), repeat: true, gain: 1.0 };
        play(&mut m, 1, Constant::boxed(0.5), cut(true));
        render(&mut m, 100);
        play(&mut m, 2, Constant::boxed(0.5), fade);
        assert_eq!(shared.snapshot().music, Some(TrackId(2)), "the status names the new track at once");
        assert_eq!(m.fading_count(), 1);
        let first_half = left(&render(&mut m, 500));
        // Halfway, the two equal tracks add up to 0.5 * (rise + fall) = 0.5 * sqrt(2): louder than
        // either alone, which is what equal power (as opposed to equal gain) means for two
        // different pieces of music.
        let mid = first_half[499];
        assert!(approx(mid, 0.5 * (rise(0.5) + fall(0.5))), "mid = {mid}");
        assert!(first_half[0] < 0.51 && first_half[0] > 0.49, "it starts from the old track alone: {}", first_half[0]);
        let rest = left(&render(&mut m, 700));
        assert_eq!(m.fading_count(), 0, "the old track is gone once its fade is over");
        assert!(approx(rest[699], 0.5), "only the new track remains: {}", rest[699]);
    }

    #[test]
    fn a_hard_cut_replaces_the_track_at_once() {
        let (mut m, _) = mixer();
        play(&mut m, 1, Constant::boxed(0.25), cut(true));
        render(&mut m, 50);
        play(&mut m, 2, Constant::boxed(0.75), cut(true));
        assert_eq!(m.fading_count(), 0);
        let out = left(&render(&mut m, 10));
        assert!(out.iter().all(|&v| approx(v, 0.75)));
    }

    #[test]
    fn stop_music_fades_out_clears_the_status_and_is_not_a_track_end() {
        let (mut m, shared) = mixer();
        play(&mut m, 4, Constant::boxed(1.0), cut(true));
        render(&mut m, 100);
        m.apply(Command::StopMusic { fade: Duration::from_millis(400) });
        assert_eq!(shared.snapshot().music, None);
        let out = left(&render(&mut m, 600));
        // The fade runs in whole blocks, so it is over by the end of the block that holds its end.
        assert!(out[0] > 0.9 && out[300] < 0.5, "it fades: {} .. {}", out[0], out[300]);
        assert!(out[512..].iter().all(|&v| v == 0.0), "and the voice is gone after that");
        assert_eq!(m.fading_count(), 0);
        assert_eq!(shared.snapshot().music_ended, 0);
    }

    #[test]
    fn a_third_fade_cuts_the_oldest_short() {
        let (mut m, _) = mixer();
        let fade = PlayOptions { fade: Duration::from_secs(10), repeat: true, gain: 1.0 };
        for track in 0..5 {
            play(&mut m, track, Constant::boxed(0.1), fade);
            render(&mut m, 10);
        }
        assert_eq!(m.fading_count(), MAX_FADING_MUSIC);
    }

    #[test]
    fn loop_points_wrap_sample_exactly_with_an_intro_before_them() {
        let (mut m, shared) = mixer();
        let points = LoopPoints { start: 20, end: Some(80) };
        play(&mut m, 1, Ramp1024::boxed(100, RATE, Some(points)), cut(true));
        let out = left(&render(&mut m, 200));
        let expect: Vec<f32> = (0..80u32).chain((20..80).cycle()).take(200).map(|i| i as f32 / 1024.0).collect();
        assert_eq!(out, expect, "0..80 once, then 20..80 forever, with no gap at the seam");
        assert_eq!(shared.snapshot().music, Some(TrackId(1)));
        assert_eq!(shared.snapshot().music_ended, 0);
    }

    #[test]
    fn a_loop_without_an_end_wraps_at_the_end_of_the_track() {
        let (mut m, _) = mixer();
        let points = LoopPoints { start: 10, end: None };
        play(&mut m, 1, Ramp1024::boxed(30, RATE, Some(points)), cut(true));
        let out = left(&render(&mut m, 70));
        let expect: Vec<f32> = (0..30u32).chain((10..30).cycle()).take(70).map(|i| i as f32 / 1024.0).collect();
        assert_eq!(out, expect);
    }

    #[test]
    fn a_track_without_loop_points_loops_whole_or_plays_once_by_the_option() {
        let (mut m, shared) = mixer();
        play(&mut m, 1, Ramp1024::boxed(16, RATE, None), cut(true));
        let looped = left(&render(&mut m, 40));
        let expect: Vec<f32> = (0..16u32).cycle().take(40).map(|i| i as f32 / 1024.0).collect();
        assert_eq!(looped, expect);

        play(&mut m, 2, Ramp1024::boxed(16, RATE, None), cut(false));
        let once = left(&render(&mut m, 40));
        let expect: Vec<f32> = (0..16u32).map(|i| i as f32 / 1024.0).collect();
        assert_eq!(&once[..16], &expect[..], "the whole track, once");
        assert!(once[16..].iter().all(|&v| v == 0.0), "then silence");
        let status = shared.snapshot();
        assert_eq!((status.music, status.music_ended), (None, 1), "a one-shot that ran out is a track end");
    }

    #[test]
    fn a_loop_that_is_empty_backwards_or_cannot_seek_never_spins() {
        let (mut m, shared) = mixer();
        // Backwards loop points are ignored: the whole track repeats.
        play(&mut m, 1, Ramp1024::boxed(8, RATE, Some(LoopPoints { start: 5, end: Some(5) })), cut(true));
        let out = left(&render(&mut m, 12));
        let expect: Vec<f32> = (0..8u32).cycle().take(12).map(|i| i as f32 / 1024.0).collect();
        assert_eq!(out, expect);
        // A source that cannot seek cannot loop: it plays once and ends.
        let script = VecDeque::from([Pull::Frames(4), Pull::End]);
        play(&mut m, 2, Box::new(Scripted(script)), cut(true));
        render(&mut m, 40);
        assert_eq!((shared.snapshot().music, shared.snapshot().music_ended), (None, 1));
        // An empty source that "loops" ends instead of spinning.
        play(&mut m, 3, Ramp1024::boxed(0, RATE, None), cut(true));
        render(&mut m, 40);
        assert_eq!((shared.snapshot().music, shared.snapshot().music_ended), (None, 2));
    }

    #[test]
    fn a_source_at_another_rate_is_resampled_linearly_and_loops_stay_exact() {
        let (mut m, _) = mixer();
        // 500 Hz into a 1 kHz mixer: every source frame lasts two output frames, and the one in
        // between is the average of its neighbours. The last frame fades out through a silent one.
        play(&mut m, 1, Ramp1024::boxed(6, 500, None), cut(false));
        let out = left(&render(&mut m, 14));
        let at = |i: f32| i / 1024.0;
        let expect = [
            at(0.0),
            at(0.5),
            at(1.0),
            at(1.5),
            at(2.0),
            at(2.5),
            at(3.0),
            at(3.5),
            at(4.0),
            at(4.5),
            at(5.0),
            at(2.5),
            0.0,
            0.0,
        ];
        for (got, want) in out.iter().zip(expect.iter()) {
            assert!(approx(*got, *want), "{got} vs {want} in {out:?}");
        }
        // Loop points are in source frames: a 500 Hz source looping frames 2..4 into a 1 kHz mixer
        // plays 0, 1 once and then 2, 3 forever; frame 4 is never played.
        let (mut m, _) = mixer();
        play(&mut m, 1, Ramp1024::boxed(6, 500, Some(LoopPoints { start: 2, end: Some(4) })), cut(true));
        let out = left(&render(&mut m, 40));
        let max = out.iter().copied().fold(0.0f32, f32::max);
        assert!(approx(max, 3.0 / 1024.0), "nothing past the loop end is ever played: {max}");
    }

    #[test]
    fn the_output_rate_can_change_under_a_playing_voice() {
        let (mut m, shared) = mixer();
        play(&mut m, 1, Constant::boxed(0.5), cut(true));
        render(&mut m, 100);
        m.set_rate(48_000);
        assert_eq!(shared.snapshot().sample_rate, 48_000);
        assert!(render(&mut m, 500).iter().all(|&v| approx(v, 0.5)));
    }

    #[test]
    fn a_source_with_nothing_ready_plays_silence_and_resumes() {
        let (mut m, shared) = mixer();
        // The first read takes 10 frames and meets the first Pending; the next read meets the
        // second one before any frame, which is an underrun.
        let script = VecDeque::from([Pull::Frames(10), Pull::Pending, Pull::Pending, Pull::Frames(10), Pull::Frames(400)]);
        play(&mut m, 1, Box::new(Scripted(script)), cut(false));
        let out = left(&render(&mut m, 300));
        assert!(out[..9].iter().all(|&v| approx(v, 0.5)), "what was ready plays (the tenth frame waits for its successor)");
        assert!(out[9..128].iter().all(|&v| v == 0.0), "then the rest of the block is silent");
        assert!(out[128..].iter().all(|&v| approx(v, 0.5)), "and it picks up where it was in the next block");
        assert_eq!(shared.snapshot().underruns, 1);
        render(&mut m, 400);
        let status = shared.snapshot();
        assert_eq!((status.music, status.music_ended, status.faults), (None, 1, 0));
    }

    #[test]
    fn broken_samples_are_written_as_silence_and_the_sum_is_limited() {
        let (mut m, _) = mixer();
        play(&mut m, 1, Constant::boxed(f32::NAN), cut(true));
        assert!(render(&mut m, 50).iter().all(|&v| v == 0.0));
        play(&mut m, 2, Constant::boxed(1.0), cut(true));
        let clip = SfxClip::from_mono(RATE, &[1.0; 100]).expect("clip");
        m.apply(Command::PlaySfx(SfxVoice::new(clip, 1.0, 0.0)));
        assert!(render(&mut m, 50).iter().all(|&v| v == 1.0), "1.0 + 1.0 is limited to 1.0");
    }

    #[test]
    fn effects_play_through_once_pan_as_a_balance_and_free_their_voice() {
        let (mut m, shared) = mixer();
        let clip = SfxClip::from_mono(RATE, &[0.5; 10]).expect("clip");
        m.apply(Command::PlaySfx(SfxVoice::new(clip.clone(), 1.0, 1.0)));
        assert_eq!(shared.snapshot().sfx_playing, 1);
        let out = render(&mut m, 20);
        assert!(approx(out[0], 0.0) && approx(out[1], 0.5), "panned right: the left side is turned down");
        assert!(out[2 * 12..].iter().all(|&v| v == 0.0), "and it is over after its ten frames");
        assert_eq!(shared.snapshot().sfx_playing, 0);
        assert_eq!(m.sfx_count(), 0);
        m.apply(Command::PlaySfx(SfxVoice::new(clip, f32::NAN, f32::NAN)));
        let out = render(&mut m, 4);
        assert!(approx(out[0], 0.5) && approx(out[1], 0.5), "NaN gain and pan mean full and centred");
    }

    #[test]
    fn the_effect_volume_is_separate_from_the_music_volume() {
        let (mut m, _) = mixer_with(Volumes { master: 1.0, music: 0.0, sfx: 0.5 });
        play(&mut m, 1, Constant::boxed(1.0), cut(true));
        let clip = SfxClip::from_mono(RATE, &[0.8; 400]).expect("clip");
        m.apply(Command::PlaySfx(SfxVoice::new(clip, 1.0, 0.0)));
        let out = render(&mut m, 200);
        assert!(out.iter().all(|&v| approx(v, 0.4)), "only the effect is heard, at half level");
    }

    #[test]
    fn a_clip_at_another_rate_is_stretched_and_a_new_effect_cuts_the_oldest_when_full() {
        let (mut m, shared) = mixer();
        let slow = SfxClip::from_mono(500, &[1.0; 10]).expect("clip");
        m.apply(Command::PlaySfx(SfxVoice::new(slow, 1.0, 0.0)));
        let out = left(&render(&mut m, 30));
        let audible = out.iter().filter(|&&v| v > 0.0).count();
        assert!((19..=21).contains(&audible), "ten frames at half rate last about twenty: {audible}");

        let long = SfxClip::from_mono(RATE, &[0.1; 10_000]).expect("clip");
        for _ in 0..MAX_SFX_VOICES + 4 {
            m.apply(Command::PlaySfx(SfxVoice::new(long.clone(), 1.0, 0.0)));
        }
        assert_eq!(m.sfx_count(), MAX_SFX_VOICES);
        assert_eq!(shared.snapshot().sfx_playing as usize, MAX_SFX_VOICES);
        m.apply(Command::StopSfx);
        assert_eq!(shared.snapshot().sfx_playing, 0);
    }
}
