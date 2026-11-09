//! Decoding a music file into memory, and playing it from there.
//!
//! A track is decoded **completely**, once, into interleaved stereo `i16` frames, and the mixer then
//! plays it through a [`MusicSource`] that only walks that buffer. Seeking is free, which is what a
//! sample-exact loop seam needs (the mixer calls `seek(loop_start)` the moment it reaches the loop
//! end), and nothing about playing it can block, wait for a disk or fall behind. The price is the
//! decode itself: it takes as long as the file is long divided by the decoder's speed, so
//! [`decode_music_file`] and [`decode_music_bytes`] are for a worker thread, never for the audio
//! callback or the game's frame loop.
//!
//! # What the files are
//!
//! The FF7 Steam Edition keeps its music as Ogg Vorbis in `music_ogg/` and four short pieces as
//! Microsoft ADPCM WAV in `music/`; both are 44.1 kHz stereo (measured on the owner's copy: 94 and 4
//! files). The Ogg files carry their loop as Vorbis comments, `LOOPSTART` and `LOOPLENGTH`, counted in
//! **samples of the file** (a frame, in this crate's words): the music loops from `LOOPSTART` to
//! `LOOPSTART + LOOPLENGTH` and the audio after that point (an ending) is never reached while it
//! repeats. [`loop_points_from_tags`] turns the tags into [`LoopPoints`] and the mixer does the rest
//! in the source's own frames, before resampling, so the seam stays exact whatever rate the device
//! runs at. A track without those tags (the short fanfare, the WAV files) loops as a whole or plays
//! once, as the caller asks.
//!
//! # Limits
//!
//! Only mono and stereo files are accepted (a mono file is played on both sides), one sample rate
//! per file, at most [`MAX_FRAMES`] frames (about 20 minutes: the real tracks are under five), and a
//! decode that panics inside the decoder is caught and reported as a damaged file. The error texts
//! never contain paths or bytes of the file.

use crate::source::{LoopPoints, MusicSource, Pull};
use std::fmt;
use std::fs::File;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// The most frames one decoded track may hold (20 minutes at 48 kHz, 230 MB as stereo `i16`). A longer
/// stream is refused rather than decoded, so a damaged or hostile file cannot eat the machine's memory.
pub const MAX_FRAMES: u64 = 20 * 60 * 48_000;

/// Why a music file could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The file could not be opened or read.
    Io(String),
    /// A container, codec or layout this build does not decode (more than two channels, a rate that
    /// changes inside the stream, an unknown format).
    Unsupported(String),
    /// The stream is damaged beyond what the decoder skips over, or the decoder itself panicked.
    Damaged(String),
    /// The file holds no audio.
    Empty,
    /// The stream is longer than [`MAX_FRAMES`] (the frames decoded when it was given up on).
    TooLong { frames: u64 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(why) => write!(f, "cannot read the file ({why})"),
            DecodeError::Unsupported(why) => write!(f, "not a music file this build can decode ({why})"),
            DecodeError::Damaged(why) => write!(f, "the music file is damaged ({why})"),
            DecodeError::Empty => f.write_str("the music file holds no audio"),
            DecodeError::TooLong { frames } => write!(f, "the music file is longer than {MAX_FRAMES} frames (gave up at {frames})"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A track held in memory as interleaved stereo `i16`, with the loop its tags name. Cloning shares
/// the samples, so it is cheap to keep one in a cache and hand a fresh [`MusicSource`] to the engine
/// each time the track plays.
#[derive(Clone)]
pub struct DecodedMusic {
    rate: u32,
    /// Interleaved stereo; an even number of samples.
    samples: Arc<Vec<i16>>,
    loop_points: Option<LoopPoints>,
    /// The frame count the container declared, when it did (an Ogg stream's last granule position).
    declared_frames: Option<u64>,
}

impl fmt::Debug for DecodedMusic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedMusic")
            .field("rate", &self.rate)
            .field("frames", &self.frames())
            .field("loop_points", &self.loop_points)
            .finish_non_exhaustive()
    }
}

impl DecodedMusic {
    /// A track from interleaved stereo samples at `rate` Hz (`None` for a zero rate, no frame, or an
    /// odd sample count). For tests and for callers that decoded the audio themselves.
    pub fn from_stereo(rate: u32, samples: Vec<i16>, loop_points: Option<LoopPoints>) -> Option<Self> {
        if rate == 0 || samples.is_empty() || !samples.len().is_multiple_of(2) {
            return None;
        }
        Some(Self { rate, samples: Arc::new(samples), loop_points, declared_frames: None })
    }

    /// The rate of the audio, in Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Length in frames.
    pub fn frames(&self) -> u64 {
        (self.samples.len() / 2) as u64
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames() as f64 / f64::from(self.rate))
    }

    /// The loop the file's tags name (already checked against the length), if any.
    pub fn loop_points(&self) -> Option<LoopPoints> {
        self.loop_points
    }

    /// The frame count the container said the stream has (an Ogg file's last granule position), when
    /// it said: a decode that disagrees with it lost or gained audio.
    pub fn declared_frames(&self) -> Option<u64> {
        self.declared_frames
    }

    /// Memory the samples hold, in bytes.
    pub fn bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<i16>()
    }

    /// A fresh source over the samples, from the start, to hand to [`crate::AudioEngine::play_music`].
    pub fn source(&self) -> Box<dyn MusicSource> {
        Box::new(MemorySource { rate: self.rate, samples: Arc::clone(&self.samples), pos: 0, loop_points: self.loop_points })
    }
}

/// Plays a shared buffer of stereo frames: reading copies, seeking is an assignment.
struct MemorySource {
    rate: u32,
    samples: Arc<Vec<i16>>,
    /// The next frame to read.
    pos: usize,
    loop_points: Option<LoopPoints>,
}

impl MemorySource {
    fn frames(&self) -> usize {
        self.samples.len() / 2
    }
}

impl MusicSource for MemorySource {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn read(&mut self, out: &mut [f32]) -> Pull {
        let asked = out.len() / 2;
        if asked == 0 {
            return Pull::Pending;
        }
        let total = self.frames();
        if self.pos >= total {
            return Pull::End;
        }
        let n = asked.min(total - self.pos);
        let from = self.pos * 2;
        let Some(src) = self.samples.get(from..from + n * 2) else { return Pull::End };
        for (dst, &s) in out.iter_mut().zip(src) {
            *dst = f32::from(s) / 32_768.0;
        }
        self.pos += n;
        Pull::Frames(n)
    }

    fn seek(&mut self, frame: u64) -> bool {
        match usize::try_from(frame) {
            Ok(frame) if frame <= self.frames() => {
                self.pos = frame;
                true
            }
            _ => false,
        }
    }

    fn loop_points(&self) -> Option<LoopPoints> {
        self.loop_points
    }
}

/// The loop a file's tags name, checked against its length. `tags` are (key, value) pairs; keys are
/// matched without regard to case (Vorbis comment names are case-insensitive).
///
/// * `LOOPSTART` (frames) and `LOOPLENGTH` (frames): the loop runs from `start` to `start + length`.
///   An end past the last frame is cut back to it (the audio ends there whatever the tag says).
/// * `LOOPSTART` alone: the loop runs from `start` to the end of the track.
/// * No `LOOPSTART`, an unparsable value, a start at or past the end, or a zero length: no loop (the
///   caller then repeats the whole track, or not at all). `LOOPLENGTH` alone means nothing.
pub fn loop_points_from_tags<'a>(tags: impl IntoIterator<Item = (&'a str, &'a str)>, total_frames: u64) -> Option<LoopPoints> {
    let (mut start, mut length) = (None, None);
    for (key, value) in tags {
        let parsed = value.trim().parse::<u64>().ok();
        if key.eq_ignore_ascii_case("LOOPSTART") {
            start = parsed;
        } else if key.eq_ignore_ascii_case("LOOPLENGTH") {
            length = parsed;
        }
    }
    let start = start?;
    if start >= total_frames {
        return None;
    }
    match length {
        None => Some(LoopPoints { start, end: None }),
        Some(0) => None,
        Some(length) => {
            let end = start.saturating_add(length).min(total_frames);
            (end > start).then_some(LoopPoints { start, end: Some(end) })
        }
    }
}

/// How much of a file's start is looked at for a RIFF `fact` chunk (they sit right after `fmt `).
const WAV_HEAD_BYTES: usize = 4_096;

/// The sample count a RIFF/WAVE file's `fact` chunk declares. ADPCM files pad their last block, so the
/// decoder returns up to one block (about 46 ms) more than the music has; the chunk says where the music
/// really ends, which matters when a short piece loops. `None` for anything that is not a WAVE with a
/// non-zero `fact` chunk in the part of the file given.
fn wav_fact_frames(head: &[u8]) -> Option<u64> {
    if head.get(0..4)? != b"RIFF" || head.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12usize;
    while let Some(header) = head.get(at..at.checked_add(8)?) {
        let size = usize::try_from(u32::from_le_bytes(header.get(4..8)?.try_into().ok()?)).ok()?;
        let body = at + 8;
        match header.get(0..4)? {
            b"fact" => {
                let count = u32::from_le_bytes(head.get(body..body.checked_add(4)?)?.try_into().ok()?);
                return (count > 0).then_some(u64::from(count));
            }
            // The audio comes first: no `fact` chunk we could see.
            b"data" => return None,
            _ => at = body.checked_add(size)?.checked_add(size & 1)?,
        }
    }
    None
}

/// Decode the music file at `path` (blocks for the length of the decode: call it from a worker
/// thread). The format is found from the content; the extension is only a hint.
pub fn decode_music_file(path: &Path) -> Result<DecodedMusic, DecodeError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path).map_err(|e| DecodeError::Io(e.kind().to_string()))?;
    let mut head = vec![0u8; WAV_HEAD_BYTES];
    let read = file.read(&mut head).map_err(|e| DecodeError::Io(e.kind().to_string()))?;
    head.truncate(read);
    file.seek(SeekFrom::Start(0)).map_err(|e| DecodeError::Io(e.kind().to_string()))?;
    let extension = path.extension().and_then(|e| e.to_str());
    decode_guarded(Box::new(file), extension, MAX_FRAMES, wav_fact_frames(&head))
}

/// Decode a music file held in memory (for tests and for callers that read the file themselves).
pub fn decode_music_bytes(bytes: Vec<u8>, extension: Option<&str>) -> Result<DecodedMusic, DecodeError> {
    let fact = wav_fact_frames(&bytes[..bytes.len().min(WAV_HEAD_BYTES)]);
    decode_guarded(Box::new(Cursor::new(bytes)), extension, MAX_FRAMES, fact)
}

/// [`decode_music_bytes`] with another frame limit (so a test can reach `TooLong` with a small file).
#[cfg(test)]
fn decode_music_bytes_limited(bytes: Vec<u8>, extension: Option<&str>, max_frames: u64) -> Result<DecodedMusic, DecodeError> {
    decode_guarded(Box::new(Cursor::new(bytes)), extension, max_frames, None)
}

/// The decode under `catch_unwind`: a panic in a decoder is a damaged file, not a dead worker.
fn decode_guarded(source: Box<dyn MediaSource>, extension: Option<&str>, max_frames: u64, fact: Option<u64>) -> Result<DecodedMusic, DecodeError> {
    catch_unwind(AssertUnwindSafe(|| decode(source, extension, max_frames, fact)))
        .unwrap_or_else(|_| Err(DecodeError::Damaged("the decoder panicked".into())))
}

fn unsupported(error: SymphoniaError) -> DecodeError {
    match error {
        SymphoniaError::Unsupported(why) | SymphoniaError::LimitError(why) => DecodeError::Unsupported(why.to_string()),
        SymphoniaError::IoError(e) => DecodeError::Io(e.kind().to_string()),
        other => DecodeError::Damaged(other.to_string()),
    }
}

fn decode(source: Box<dyn MediaSource>, extension: Option<&str>, max_frames: u64, fact: Option<u64>) -> Result<DecodedMusic, DecodeError> {
    let stream = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = extension {
        hint.with_extension(extension);
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(unsupported)?;

    // Tags first: the Vorbis comment header is read while probing. A container may also hand them over
    // as side data of the probe, so both places are looked at.
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut collect = |revision: &symphonia::core::meta::MetadataRevision| {
        tags.extend(revision.tags().iter().map(|t| (t.key.clone(), t.value.to_string())));
    };
    if let Some(revision) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        collect(revision);
    }
    if let Some(revision) = probed.format.metadata().current() {
        collect(revision);
    }

    let track = probed
        .format
        .default_track()
        .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .or_else(|| probed.format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL))
        .ok_or_else(|| DecodeError::Unsupported("no audio track".into()))?;
    let track_id = track.id;
    let declared_frames = track.codec_params.n_frames;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(unsupported)?;

    let reserve = declared_frames.unwrap_or(0).min(max_frames);
    let mut samples: Vec<i16> = Vec::with_capacity(usize::try_from(reserve).unwrap_or(0).saturating_mul(2));
    let mut rate = 0u32;
    let mut scratch: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            // The end of the stream (and of the last chained stream) comes as an end-of-file error.
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(other) => return Err(unsupported(other)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A packet the decoder cannot make sense of costs that packet, not the track.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(other) => return Err(unsupported(other)),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        if channels == 0 || channels > 2 {
            return Err(DecodeError::Unsupported(format!("{channels} channels")));
        }
        if rate == 0 {
            rate = spec.rate;
        } else if rate != spec.rate {
            return Err(DecodeError::Unsupported("the sample rate changes inside the stream".into()));
        }
        let needed = decoded.frames() * channels;
        if needed == 0 {
            continue;
        }
        if scratch.as_ref().is_none_or(|s| s.capacity() < needed) {
            scratch = Some(SampleBuffer::<i16>::new(decoded.capacity() as u64, spec));
        }
        let Some(buffer) = scratch.as_mut() else { continue };
        buffer.copy_interleaved_ref(decoded);
        if channels == 1 {
            samples.extend(buffer.samples().iter().flat_map(|&s| [s, s]));
        } else {
            samples.extend_from_slice(buffer.samples());
        }
        let frames = (samples.len() / 2) as u64;
        if frames > max_frames {
            return Err(DecodeError::TooLong { frames });
        }
    }
    if samples.is_empty() || rate == 0 {
        return Err(DecodeError::Empty);
    }
    // The music ends where the file's `fact` chunk says (the last ADPCM block is padded past it).
    if let Some(fact) = fact.and_then(|f| usize::try_from(f).ok()) {
        samples.truncate(fact.saturating_mul(2));
    }
    let total = (samples.len() / 2) as u64;
    let loop_points = loop_points_from_tags(tags.iter().map(|(k, v)| (k.as_str(), v.as_str())), total);
    Ok(DecodedMusic { rate, samples: Arc::new(samples), loop_points, declared_frames })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioConfig, AudioEngine, PlayOptions, TrackId, Volumes};

    /// A 16-bit PCM WAV file in memory (a synthetic test file, no game data).
    fn wav(rate: u32, channels: u16, interleaved: &[i16]) -> Vec<u8> {
        let data_len = (interleaved.len() * 2) as u32;
        let block = channels * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in interleaved {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    /// Frame `i` of the test ramp: left rises, right falls, so a swap or an offset shows.
    fn ramp(frames: usize) -> Vec<i16> {
        (0..frames).flat_map(|i| [i as i16, -(i as i16)]).collect()
    }

    /// [`wav`] with a `fact` chunk (as ADPCM files have) between `fmt ` and `data`.
    fn wav_with_fact(rate: u32, channels: u16, interleaved: &[i16], fact: u32) -> Vec<u8> {
        let plain = wav(rate, channels, interleaved);
        // 12 bytes of RIFF header and the 24-byte `fmt ` chunk, then the new chunk, then `data`.
        let mut out = plain[..36].to_vec();
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&fact.to_le_bytes());
        out.extend_from_slice(&plain[36..]);
        let riff_len = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff_len.to_le_bytes());
        out
    }

    #[test]
    fn a_fact_chunk_trims_the_padding_of_the_last_block() {
        let samples = ramp(1_000);
        // The audio is 1000 frames long but the file says the music is 700: the rest is padding.
        let trimmed = decode_music_bytes(wav_with_fact(22_050, 2, &samples, 700), Some("wav")).expect("a wav with a fact chunk");
        assert_eq!(trimmed.frames(), 700);
        let mut out = vec![0.0f32; 2_000];
        assert_eq!(trimmed.source().read(&mut out), Pull::Frames(700));
        assert_eq!(out[2 * 699], 699.0 / 32_768.0);
        // A fact count past the audio, or zero, changes nothing.
        assert_eq!(decode_music_bytes(wav_with_fact(22_050, 2, &samples, 5_000), Some("wav")).expect("wav").frames(), 1_000);
        assert_eq!(decode_music_bytes(wav_with_fact(22_050, 2, &samples, 0), Some("wav")).expect("wav").frames(), 1_000);
        // The chunk is found only in a well-formed header, and a lying one never panics.
        let good = wav_with_fact(22_050, 2, &samples, 700);
        assert_eq!(wav_fact_frames(&good), Some(700));
        assert_eq!(wav_fact_frames(&wav(22_050, 2, &samples)), None, "a plain wav has none");
        assert_eq!(wav_fact_frames(&[]), None);
        assert_eq!(wav_fact_frames(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(wav_fact_frames(b"RIFX\0\0\0\0WAVEfact\x04\0\0\0\x05\0\0\0"), None, "not RIFF");
        assert_eq!(wav_fact_frames(b"RIFF\0\0\0\0WAVEfact\x04\0\0\0\x05\0"), None, "cut off in the count");
        let mut lying = good.clone();
        lying[16..20].copy_from_slice(&u32::MAX.to_le_bytes()); // the fmt chunk claims to be 4 GB long
        assert_eq!(wav_fact_frames(&lying), None);
        for len in 0..good.len() {
            let _ = wav_fact_frames(&good[..len]);
        }
    }

    #[test]
    fn a_stereo_wav_decodes_exactly() {
        let samples = ramp(1_000);
        let music = decode_music_bytes(wav(22_050, 2, &samples), Some("wav")).expect("a stereo wav");
        assert_eq!((music.rate(), music.frames()), (22_050, 1_000));
        assert_eq!(music.loop_points(), None);
        assert_eq!(music.bytes(), 4_000);
        assert!((music.duration().as_secs_f64() - 1_000.0 / 22_050.0).abs() < 1e-9);
        // The format is found from the content, whatever the extension says.
        let again = decode_music_bytes(wav(22_050, 2, &samples), None).expect("no extension");
        assert_eq!(again.frames(), 1_000);
        let mut source = music.source();
        assert_eq!(source.sample_rate(), 22_050);
        let mut out = vec![0.0f32; 2_000];
        assert_eq!(source.read(&mut out), Pull::Frames(1_000));
        assert_eq!(out[0], 0.0);
        assert_eq!(out[2], 1.0 / 32_768.0);
        assert_eq!(out[3], -1.0 / 32_768.0);
        assert_eq!(out[2 * 999], 999.0 / 32_768.0);
        assert_eq!(source.read(&mut out), Pull::End);
    }

    #[test]
    fn a_mono_wav_plays_on_both_sides() {
        let mono: Vec<i16> = (0..100).map(|i| i * 100 - 5_000).collect();
        let music = decode_music_bytes(wav(8_000, 1, &mono), Some("wav")).expect("a mono wav");
        assert_eq!(music.frames(), 100);
        let mut source = music.source();
        let mut out = vec![0.0f32; 200];
        assert_eq!(source.read(&mut out), Pull::Frames(100));
        for (i, frame) in out.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(frame[0], frame[1], "frame {i}");
            assert_eq!(frame[0], f32::from(mono[i]) / 32_768.0);
        }
    }

    #[test]
    fn bad_files_are_errors_and_never_panics() {
        assert!(matches!(decode_music_bytes(Vec::new(), Some("ogg")), Err(DecodeError::Unsupported(_) | DecodeError::Io(_))));
        assert!(matches!(decode_music_bytes(b"this is not audio at all".to_vec(), None), Err(DecodeError::Unsupported(_))));
        // A WAV with a header and no audio.
        assert_eq!(decode_music_bytes(wav(44_100, 2, &[]), Some("wav")).unwrap_err(), DecodeError::Empty);
        // A WAV cut off in the middle of its audio decodes what is there (a damaged tail costs the tail).
        let mut cut = wav(44_100, 2, &ramp(1_000));
        cut.truncate(cut.len() - 1_001);
        let music = decode_music_bytes(cut, Some("wav")).expect("a truncated wav");
        assert!(music.frames() >= 749 && music.frames() < 1_000, "{}", music.frames());
        // Garbage of every length never panics.
        let garbage: Vec<u8> = (0..4_096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for len in [1, 3, 11, 44, 100, 1_000, 4_096] {
            let _ = decode_music_bytes(garbage[..len].to_vec(), Some("ogg"));
            let _ = decode_music_bytes(garbage[..len].to_vec(), Some("wav"));
        }
        // A file that would decode to more than the limit is refused.
        let err = decode_music_bytes_limited(wav(44_100, 2, &ramp(1_000)), Some("wav"), 100).unwrap_err();
        assert!(matches!(err, DecodeError::TooLong { frames } if frames > 100), "{err}");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn the_errors_say_what_is_wrong_without_naming_a_path() {
        for error in [
            DecodeError::Io("not found".into()),
            DecodeError::Unsupported("3 channels".into()),
            DecodeError::Damaged("bad packet".into()),
            DecodeError::Empty,
            DecodeError::TooLong { frames: 5 },
        ] {
            let text = error.to_string();
            assert!(!text.is_empty() && !text.contains('/'), "{text}");
        }
        let missing = decode_music_file(Path::new("/definitely/not/here.ogg")).unwrap_err();
        assert!(matches!(missing, DecodeError::Io(_)), "{missing}");
        assert!(!missing.to_string().contains("definitely"), "the path stays out of the message");
    }

    #[test]
    fn loop_tags_become_loop_points_checked_against_the_length() {
        let lp = |tags: &[(&str, &str)], total| loop_points_from_tags(tags.iter().copied(), total);
        // Start and length: the loop is start .. start + length.
        assert_eq!(lp(&[("LOOPSTART", "192767"), ("LOOPLENGTH", "3087428")], 7_350_000), Some(LoopPoints { start: 192_767, end: Some(3_280_195) }));
        // Keys are case-insensitive, values may carry spaces, order does not matter.
        assert_eq!(lp(&[("looplength", " 40 "), ("LoopStart", "10")], 100), Some(LoopPoints { start: 10, end: Some(50) }));
        // A start alone loops to the end of the track.
        assert_eq!(lp(&[("LOOPSTART", "10")], 100), Some(LoopPoints { start: 10, end: None }));
        // An end past the last frame is cut back to it.
        assert_eq!(lp(&[("LOOPSTART", "10"), ("LOOPLENGTH", "1000")], 100), Some(LoopPoints { start: 10, end: Some(100) }));
        // Nothing usable: no loop (a length alone means nothing; a start at or past the end, a zero
        // length and unparsable values are all ignored).
        assert_eq!(lp(&[], 100), None);
        assert_eq!(lp(&[("LOOPLENGTH", "50")], 100), None);
        assert_eq!(lp(&[("LOOPSTART", "100")], 100), None);
        assert_eq!(lp(&[("LOOPSTART", "101"), ("LOOPLENGTH", "5")], 100), None);
        assert_eq!(lp(&[("LOOPSTART", "10"), ("LOOPLENGTH", "0")], 100), None);
        assert_eq!(lp(&[("LOOPSTART", "ten")], 100), None);
        assert_eq!(lp(&[("LOOPSTART", "-5")], 100), None);
        assert_eq!(lp(&[("TITLE", "x"), ("ARTIST", "y")], 100), None);
        // A huge length cannot overflow.
        assert_eq!(lp(&[("LOOPSTART", "10"), ("LOOPLENGTH", "18446744073709551615")], 100), Some(LoopPoints { start: 10, end: Some(100) }));
    }

    #[test]
    fn a_source_reads_in_pieces_seeks_and_refuses_to_seek_past_the_end() {
        let music = DecodedMusic::from_stereo(1_000, ramp(10), None).expect("ten frames");
        let mut source = music.source();
        let mut out = vec![0.0f32; 8];
        assert_eq!(source.read(&mut out), Pull::Frames(4));
        assert_eq!(source.read(&mut out), Pull::Frames(4));
        assert_eq!(source.read(&mut out), Pull::Frames(2), "the last two");
        assert_eq!(source.read(&mut out), Pull::End);
        assert!(source.seek(8));
        assert_eq!(source.read(&mut out), Pull::Frames(2));
        assert_eq!(out[0], 8.0 / 32_768.0);
        assert!(source.seek(10), "the very end is a valid place to be");
        assert_eq!(source.read(&mut out), Pull::End);
        assert!(!source.seek(11));
        assert!(source.seek(0));
        assert_eq!(source.read(&mut out[..1]), Pull::Pending, "no whole frame asked for is not an end");
        // Two sources over one track are independent.
        let mut a = music.source();
        let mut b = music.source();
        let mut buf = vec![0.0f32; 4];
        assert_eq!(a.read(&mut buf), Pull::Frames(2));
        assert_eq!(b.read(&mut buf), Pull::Frames(2));
        assert_eq!(buf[2], 1.0 / 32_768.0);
        // A track needs a rate, a frame and whole frames.
        assert!(DecodedMusic::from_stereo(0, ramp(4), None).is_none());
        assert!(DecodedMusic::from_stereo(1_000, Vec::new(), None).is_none());
        assert!(DecodedMusic::from_stereo(1_000, vec![0; 3], None).is_none());
    }

    #[test]
    fn a_decoded_loop_repeats_sample_exactly_through_the_engine() {
        // 20 frames whose value is their index; loop frames 5..15. Rendered at the file's own rate, the
        // output must be 0..15 and then 5..15 again, with no gap and no repeated frame at the seam.
        let samples: Vec<i16> = (0..20).flat_map(|i| [i * 1_000, i * 1_000]).collect();
        let music = DecodedMusic::from_stereo(48_000, samples, Some(LoopPoints { start: 5, end: Some(15) })).expect("a track");
        // Unity levels from the start, so that no gain ramp colours the values.
        let unity = Volumes { master: 1.0, music: 1.0, sfx: 1.0 };
        let engine = AudioEngine::start_manual(AudioConfig { sample_rate: 48_000, volumes: unity, ..AudioConfig::default() });
        engine.play_music(TrackId(1), music.source(), PlayOptions { fade: Duration::ZERO, repeat: true, gain: 1.0 });
        let out = engine.pump(600);
        let values: Vec<i32> = out.as_chunks::<2>().0.iter().map(|f| (f[0] * 32_768.0).round() as i32).collect();
        // Past the first pass, find a loop start (value 5_000) and check the cycle of ten after it.
        let tail = &values[100..];
        let first = tail.iter().position(|&v| v == 5_000).expect("a loop start in the tail");
        for (i, &v) in tail[first..].iter().enumerate() {
            let expected = 5_000 + 1_000 * (i as i32 % 10);
            assert_eq!(v, expected, "frame {i} after the first loop start");
        }
        assert!(tail.len() - first > 400, "the check covers dozens of seams");
        assert_eq!(engine.status().music, Some(TrackId(1)));
        assert_eq!(engine.status().music_ended, 0);
    }

    /// `ffmpeg` can make a real Ogg Vorbis file out of a sine, which is the one way to test the Ogg path
    /// without owned audio. Skipped (with a note) on a machine without ffmpeg or its Vorbis encoder.
    fn synthetic_ogg(dir: &Path, name: &str, tags: &[(&str, &str)], seconds: f32) -> Option<std::path::PathBuf> {
        let path = dir.join(name);
        let mut command = std::process::Command::new("ffmpeg");
        command
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=44100:duration={seconds}"))
            .args(["-ac", "2", "-c:a", "libvorbis"]);
        for (key, value) in tags {
            command.arg("-metadata").arg(format!("{key}={value}"));
        }
        command.arg(&path);
        match command.output() {
            Ok(done) if done.status.success() && path.exists() => Some(path),
            _ => {
                eprintln!("note: no ffmpeg with a Vorbis encoder here; the Ogg test is skipped");
                None
            }
        }
    }

    #[test]
    fn an_ogg_vorbis_file_decodes_with_its_loop_tags() {
        let dir = std::env::temp_dir().join(format!("fmv-audio-ogg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let found = synthetic_ogg(&dir, "tone.ogg", &[("LOOPSTART", "10000"), ("LOOPLENGTH", "20000")], 2.0);
        if let Some(path) = found {
            let music = decode_music_file(&path).expect("an ogg file");
            assert_eq!(music.rate(), 44_100);
            assert_eq!(music.loop_points(), Some(LoopPoints { start: 10_000, end: Some(30_000) }));
            // The decode is as long as the container says the stream is: no frame lost or gained.
            assert_eq!(Some(music.frames()), music.declared_frames());
            assert!(music.frames().abs_diff(88_200) < 2_048, "{}", music.frames());
            // It is the sine that went in: ffmpeg's `sine` source peaks at about 0.09 (ffmpeg's own
            // decode of such a file measures 0.0902, to the digit what this decoder gives), on both sides.
            let mut source = music.source();
            let mut out = vec![0.0f32; music.frames() as usize * 2];
            assert_eq!(source.read(&mut out), Pull::Frames(music.frames() as usize));
            for side in 0..2 {
                let peak = out.iter().skip(side).step_by(2).fold(0.0f32, |m, v| m.max(v.abs()));
                assert!((0.08..0.10).contains(&peak), "side {side} peaks at {peak}");
            }
            // The same file without tags has no loop.
            let plain = synthetic_ogg(&dir, "plain.ogg", &[], 0.5).expect("ffmpeg worked a moment ago");
            assert_eq!(decode_music_file(&plain).expect("plain ogg").loop_points(), None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
