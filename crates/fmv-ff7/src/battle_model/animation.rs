//! `..da`: the animation pack of a battle model, and the decoder of its bit-packed clips.
//!
//! **Layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 391 `da` entries
//! of `battle/battle.lgp` (the ignored real-data tests of [`super`] assert the totals quoted
//! here). Measured facts:
//!
//! * A pack is a `u32` entry count followed by the entries back to back, each a 12-byte header
//!   `{ u32 bones, u32 frames, u32 chunk_size }` and `chunk_size` bytes. Walking the entries by
//!   their chunk sizes ends exactly at the end of the file in all 391 packs, and the entry count is
//!   exactly the skeleton's animation slots plus its weapon animation tracks
//!   ([`super::skeleton::BattleSkeleton::animations`] + `weapon_animations`) in all 391.
//! * An entry whose chunk is shorter than 11 bytes is not an animation (all 707 of them have a
//!   4-byte chunk: one frame, no data): the documents guess that they are key sets for scripted
//!   actions. They keep their slot (animation indices are slot numbers) and decode to nothing, see
//!   [`PackEntry::is_clip`].
//! * A clip's chunk is `{ u16 frames, u16 size, u8 key }` and then a bit stream (most significant
//!   bit first, every read sign-extended), the same format for a skeleton clip and a weapon track.
//!   `key` is the number of low bits the angles drop: 4 in 6 174 of the 7 130 clips, 0 in 953 and 2
//!   in 3. `chunk_size - size - 5` is 0 to 3 padding bytes in 7 129 of 7 130 clips (the exception
//!   is the 15th clip of the playable frog, which the documents also call corrupt).
//! * **Frame 0** is raw: three `i16` (the root translation, 16 bits each) and then `triplets` x 3
//!   angles of `12 - key` bits each, shifted left by `key`. **Every later frame** is a delta on the
//!   previous one: per translation axis a flag bit (0: a 7-bit signed delta, 1: a 16-bit one), per
//!   angle a flag bit (0: no change) and then a 3-bit code: 0 is -1, 7 an escape to the raw
//!   `12 - key` bit value, and 1 to 6 a signed value of that many bits with the sign bit's weight
//!   added back (`v + 2^(t-1)` for `v >= 0`, `v - 2^(t-1)` for `v < 0`); every angle delta is shifted
//!   left by `key`. Angles are accumulated in an `i16` and mean `raw * 360 / 4096` degrees.
//! * **Decoding the larger of the two frame counts ends on the declared stream size in 7 128 of the
//!   7 130 clips** (the whole stream is consumed to within the last byte): a wrong triplet count or
//!   a wrong code would desynchronise the stream and miss it, so this is the check that the decoder
//!   is right. The two others are the frog's corrupt clip and one clip that needs one more byte
//!   than it declares (the reader is bounded by the chunk, not by the declared size, so its last
//!   frame is still read). The two counts differ in 17 clips (see [`decode_clip`]); in the one
//!   where the header's 32-bit count is the larger the two extra frames are real (the stream ends
//!   28 bits after them, too few for another frame, and their angle steps are as small as their
//!   neighbours').
//! * **Triplets per frame** are `bones + 1` for a skeleton clip (the first is the **root
//!   rotation**: 46 of the multi-bone models' first clips start with a non-zero one and 1 379 clips
//!   have one in some frame, so it must not be dropped), 1 for a weapon track and 1 for a
//!   single-bone skeleton (13 enemies). The header's own `bones` word is unreliable (17 clips, all
//!   of single-bone skeletons, say 2), which is why the skeleton decides.
//!
//! * **The root translation and the slot numbers are right**: the MP's slot 2 (12 frames) carries its
//!   root from z = -169 to -1 139, slots 3 and 4 start at -1 145 and -1 149, and slot 5 (21 frames)
//!   runs from -1 113 back to -3, a run to the target, the attack there and the return, each clip
//!   ending where the next begins.
//!
//! # Sources
//!
//! * FFRTT "FF7/Battle/Battle Animation (PC)" <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Animation_(PC)>
//!   (container, mini header, bit order, frame 0, the delta codes and their rationale) and KimeraCS
//!   `FF7BattleAnimation.cs` <https://github.com/LaZar00/KimeraCS> (single-bone rule, the frame
//!   count rules, accumulators). Only format facts were taken; the decoder is written from them
//!   and checked against the owned files as above.
//!
//! # From a clip to a [`FieldAnimation`]
//!
//! A battle clip has exactly the shape of a field one (a root rotation, a root translation and one
//! rotation per bone per frame), and the field posing conventions pose it correctly (see
//! [`crate::anim`]): Euler order y, x, z with standard matrices, bones along `-z`, the root
//! translation's `y` a height above the ground. The one difference is the sign of the stored `y`:
//! a battle clip stores it already in the y-down native space (Cloud's idle clip stores -466, his
//! hip height, and with that value applied as stored the lowest vertex of every frame is within a
//! few units of `y = 0` for the five characters and enemies measured), whereas a field clip stores
//! a height. [`clip_to_animation`] therefore stores `-y`, so [`FieldAnimation::pose`], which negates
//! it again, puts the model where the game does.

use crate::anim::{AnimFrame, FieldAnimation, MAX_ANIM_BONES, MAX_ANIM_FRAMES};
use crate::model::ModelError;
use crate::util::{slice_at, u16_at, u32_at, u8_at};

/// Bytes of an entry header (`bones`, `frames`, `chunk_size`).
pub const PACK_ENTRY_HEADER_LEN: usize = 12;
/// Bytes of a clip chunk's own header (`u16` frames, `u16` size, `u8` key).
pub const CLIP_HEADER_LEN: usize = 5;
/// A chunk shorter than this is not a clip (the real ones are 4 bytes; the smallest clip is 9+5).
pub const MIN_CLIP_CHUNK: usize = 11;
/// Largest `key` accepted (the real values are 0, 2 and 4).
pub const MAX_KEY: u8 = 4;
/// Most entries accepted in one pack (the owned maximum is 94).
pub const MAX_PACK_ENTRIES: usize = 1024;
/// Most padding bytes a clip chunk may have after its declared data size.
pub const MAX_CLIP_PADDING: usize = 3;

/// One entry of a pack: the header words and the chunk, borrowed from the pack's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackEntry<'a> {
    /// The header's bone count (bones + 1 for a skeleton clip, 1 for a weapon track; **unreliable**).
    pub bones: u32,
    /// The header's frame count (a minimum: see [`decode_clip`]).
    pub frames: u32,
    pub chunk: &'a [u8],
}

impl PackEntry<'_> {
    /// Whether this entry is a clip (and not one of the 4-byte special entries).
    pub fn is_clip(&self) -> bool {
        self.chunk.len() >= MIN_CLIP_CHUNK
    }
}

/// Split a pack into its entries. An error if the count or a chunk does not fit the file; trailing
/// bytes after the last entry are ignored.
pub fn split_pack(bytes: &[u8]) -> Result<Vec<PackEntry<'_>>, ModelError> {
    let count = u32_at(bytes, 0).ok_or(ModelError::Truncated { what: "animation pack", at: 0, needed: 4 })?;
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    if count > MAX_PACK_ENTRIES {
        return Err(ModelError::TooMany {
            what: "animation pack entries",
            count: count as u64,
            limit: MAX_PACK_ENTRIES as u64,
        });
    }
    // Every entry has a header: a count that cannot fit is refused before anything is allocated.
    let room = bytes.len().saturating_sub(4) / PACK_ENTRY_HEADER_LEN;
    if count > room {
        return Err(ModelError::Truncated {
            what: "animation pack entries",
            at: bytes.len(),
            needed: (count - room) * PACK_ENTRY_HEADER_LEN,
        });
    }
    let mut entries = Vec::with_capacity(count);
    let mut pos = 4usize;
    for _ in 0..count {
        let word = |k: usize| {
            u32_at(bytes, pos + 4 * k).ok_or(ModelError::Truncated {
                what: "animation pack entry header",
                at: pos + 4 * k,
                needed: 4,
            })
        };
        let (bones, frames, chunk_size) = (word(0)?, word(1)?, word(2)?);
        let chunk_len = usize::try_from(chunk_size).unwrap_or(usize::MAX);
        let start = pos + PACK_ENTRY_HEADER_LEN;
        let chunk = slice_at(bytes, start, chunk_len).ok_or(ModelError::Truncated {
            what: "animation pack chunk",
            at: start,
            needed: chunk_len.saturating_sub(bytes.len().saturating_sub(start)),
        })?;
        entries.push(PackEntry { bones, frames, chunk });
        pos = start + chunk_len;
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------------------
// The bit stream
// ---------------------------------------------------------------------------------------

/// A most-significant-bit-first reader over a byte slice; every read is bounds-checked and a read
/// past the end is `None` (and consumes nothing useful).
struct BitReader<'a> {
    data: &'a [u8],
    /// Bit position.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bits_left(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.pos)
    }

    /// `n` bits (at most 24) as an unsigned number.
    fn unsigned(&mut self, n: u32) -> Option<u32> {
        if n > 24 || (n as usize) > self.bits_left() {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(value)
    }

    /// `n` bits (1 to 24) as a two's-complement signed number.
    fn signed(&mut self, n: u32) -> Option<i32> {
        let value = i64::from(self.unsigned(n)?);
        let signed = if n >= 1 && value >= 1i64 << (n - 1) { value - (1i64 << n) } else { value };
        i32::try_from(signed).ok()
    }
}

/// One decoded frame: the root translation as stored and one raw `i16` angle triple per triplet
/// (the first is the root rotation of a skeleton clip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// `x`, `y`, `z` as stored (`y` is already in the y-down native space: negative is up).
    pub position: [i32; 3],
    /// Angles in 4096ths of a turn, accumulated in an `i16` (so they may be negative or past 4095;
    /// only their value modulo 4096 matters).
    pub rotations: Vec<[i16; 3]>,
}

/// A decoded clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawClip {
    pub key: u8,
    /// The chunk's own 16-bit frame count and the entry header's 32-bit one.
    pub stream_frames: u16,
    pub header_frames: u32,
    pub frames: Vec<RawFrame>,
    /// The stream ended before the declared number of frames (the frames before it are kept).
    pub ended_early: bool,
}

fn truncated(what: &'static str, at: usize) -> ModelError {
    ModelError::Truncated { what, at, needed: 1 }
}

fn invalid(why: impl Into<String>) -> ModelError {
    ModelError::Invalid { what: "battle animation", why: why.into() }
}

/// Decode one clip. `triplets` is the number of angle triples per frame: `bones + 1` for a skeleton
/// clip (the first is the root rotation), 1 for a weapon track or a single-bone skeleton.
///
/// The number of frames is the larger of the chunk's 16-bit count and the header's 32-bit one
/// (capped at [`MAX_ANIM_FRAMES`]): the documents call the header's a minimum, and in the owned
/// data they differ in 17 of 7 130 clips (the 16-bit one is smaller in one). The stream is bounded
/// by the chunk's bytes, not by the declared data size, and a stream that ends early keeps the
/// frames it has (`ended_early`); only a stream that cannot hold frame 0 is an error.
pub fn decode_clip(entry: &PackEntry<'_>, triplets: usize) -> Result<RawClip, ModelError> {
    let chunk = entry.chunk;
    if chunk.len() < CLIP_HEADER_LEN {
        return Err(ModelError::Truncated { what: "battle clip header", at: 0, needed: CLIP_HEADER_LEN - chunk.len() });
    }
    let stream_frames = u16_at(chunk, 0).ok_or_else(|| truncated("battle clip header", 0))?;
    let size = usize::from(u16_at(chunk, 2).ok_or_else(|| truncated("battle clip header", 2))?);
    let key = u8_at(chunk, 4).ok_or_else(|| truncated("battle clip header", 4))?;
    if key > MAX_KEY {
        return Err(invalid(format!("key {key} (expected 0 to {MAX_KEY})")));
    }
    // The declared data size and the padding after it must agree with the chunk (all real clips
    // but one do): this is what tells a corrupt clip from a good one.
    let padding = chunk.len().checked_sub(CLIP_HEADER_LEN + size);
    if !padding.is_some_and(|p| p <= MAX_CLIP_PADDING) {
        return Err(invalid(format!("data size {size} does not fit the {}-byte chunk", chunk.len())));
    }
    if triplets == 0 || triplets > MAX_ANIM_BONES + 1 {
        return Err(invalid(format!("{triplets} angle triples per frame")));
    }
    let data = chunk.get(CLIP_HEADER_LEN..).unwrap_or(&[]);
    let mut reader = BitReader::new(data);
    let width = 12 - u32::from(key);
    let scale = 1i32 << key;

    let frame0 = (|| {
        let position = [reader.signed(16)?, reader.signed(16)?, reader.signed(16)?];
        let mut rotations = Vec::with_capacity(triplets);
        for _ in 0..triplets {
            let mut triple = [0i16; 3];
            for angle in &mut triple {
                *angle = (reader.signed(width)? * scale) as i16;
            }
            rotations.push(triple);
        }
        Some(RawFrame { position, rotations })
    })()
    .ok_or(ModelError::Truncated { what: "battle clip frame 0", at: chunk.len(), needed: 1 })?;

    let wanted = usize::try_from(entry.frames).unwrap_or(usize::MAX).max(usize::from(stream_frames)).clamp(1, MAX_ANIM_FRAMES);
    let mut frames = Vec::with_capacity(wanted.min(256));
    frames.push(frame0);
    let mut ended_early = false;
    while frames.len() < wanted {
        let Some(previous) = frames.last() else { break };
        match delta_frame(&mut reader, previous, key) {
            Some(next) => frames.push(next),
            None => {
                ended_early = true;
                break;
            }
        }
    }
    Ok(RawClip { key, stream_frames, header_frames: entry.frames, frames, ended_early })
}

/// One angle delta (`key` low bits dropped), `None` if the stream ends.
fn angle_delta(reader: &mut BitReader<'_>, key: u8) -> Option<i32> {
    if reader.unsigned(1)? == 0 {
        return Some(0);
    }
    let width = 12 - u32::from(key);
    let code = reader.unsigned(3)?;
    let steps = match code {
        0 => -1,
        7 => reader.signed(width)?,
        t => {
            let v = reader.signed(t)?;
            let half = 1i32 << (t - 1);
            if v < 0 {
                v - half
            } else {
                v + half
            }
        }
    };
    Some(steps * (1i32 << key))
}

/// One translation delta: a flag bit, then 7 or 16 signed bits.
fn position_delta(reader: &mut BitReader<'_>) -> Option<i32> {
    let wide = reader.unsigned(1)? != 0;
    reader.signed(if wide { 16 } else { 7 })
}

fn delta_frame(reader: &mut BitReader<'_>, previous: &RawFrame, key: u8) -> Option<RawFrame> {
    let mut position = previous.position;
    for slot in &mut position {
        *slot = slot.saturating_add(position_delta(reader)?);
    }
    let mut rotations = Vec::with_capacity(previous.rotations.len());
    for triple in &previous.rotations {
        let mut next = *triple;
        for angle in &mut next {
            *angle = (i32::from(*angle) + angle_delta(reader, key)?) as i16;
        }
        rotations.push(next);
    }
    Some(RawFrame { position, rotations })
}

// ---------------------------------------------------------------------------------------
// To FieldAnimation
// ---------------------------------------------------------------------------------------

/// Degrees (`0..360`) of an angle in 4096ths of a turn (any `i16`; only its value modulo 4096
/// matters to a rotation).
pub fn raw_angle_degrees(raw: i16) -> f32 {
    i32::from(raw).rem_euclid(4096) as f32 * (360.0 / 4096.0)
}

/// Build a [`FieldAnimation`] from frames: through the `.a` layout the field loader parses, so the
/// result is exactly what loading such a file gives (and every limit of [`FieldAnimation::parse`]
/// applies).
pub fn animation_from_frames(frames: &[AnimFrame], bone_count: usize) -> Result<FieldAnimation, ModelError> {
    let mut out: Vec<u8> = Vec::with_capacity(36 + frames.len() * (24 + 12 * bone_count));
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(frames.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&u32::try_from(bone_count).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&[1, 0, 2, 0]); // rotation order y, x, z; unused byte
    out.extend_from_slice(&[0u8; 20]); // run-time data
    let zero = [0.0f32; 3];
    for frame in frames {
        for v in frame.root_rotation.iter().chain(frame.root_translation.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for b in 0..bone_count {
            for v in frame.rotations.get(b).unwrap_or(&zero) {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    FieldAnimation::parse(&out)
}

/// The animation of `clip` for a skeleton of `bones` bones (1 for a weapon track): a root rotation,
/// the root translation (`y` negated, see the module docs) and `bones` rotations per frame. A clip
/// whose triplets do not match (`bones + 1`, or 1 when `bones` is 1) is an error.
pub fn clip_to_animation(clip: &RawClip, bones: usize) -> Result<FieldAnimation, ModelError> {
    let single = bones <= 1;
    let expected = if single { 1 } else { bones + 1 };
    let mut frames = Vec::with_capacity(clip.frames.len());
    for frame in &clip.frames {
        if frame.rotations.len() != expected {
            return Err(invalid(format!("{} angle triples per frame for {bones} bones", frame.rotations.len())));
        }
        let degrees = |t: &[i16; 3]| t.map(raw_angle_degrees);
        let (root, rest) = frame.rotations.split_first().ok_or_else(|| invalid("a frame with no angles"))?;
        let rotations = if single { vec![[0.0; 3]; bones.max(1)] } else { rest.iter().map(degrees).collect() };
        frames.push(AnimFrame {
            root_rotation: degrees(root),
            root_translation: [frame.position[0] as f32, -(frame.position[1] as f32), frame.position[2] as f32],
            rotations,
        });
    }
    animation_from_frames(&frames, bones.max(1))
}

/// A one-frame animation of `bones` bones holding frame 0 of `source` (a copy), or the identity
/// pose (all angles and the translation zero) when there is no source: what an unusable slot holds
/// so that every slot can be posed and none shows a straight line of bones.
pub fn rest_animation(source: Option<&FieldAnimation>, bones: usize) -> Result<FieldAnimation, ModelError> {
    let frame = source.and_then(|a| a.frame(0)).cloned().unwrap_or(AnimFrame {
        root_rotation: [0.0; 3],
        root_translation: [0.0; 3],
        rotations: vec![[0.0; 3]; bones],
    });
    animation_from_frames(&[frame], bones)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A most-significant-bit-first bit writer (independent of the reader).
    #[derive(Default)]
    pub(crate) struct BitWriter {
        pub bytes: Vec<u8>,
        used: usize,
    }

    impl BitWriter {
        pub(crate) fn put(&mut self, value: i64, bits: u32) {
            for k in (0..bits).rev() {
                let bit = ((value >> k) & 1) as u8;
                if self.used % 8 == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= bit << (7 - (self.used % 8));
                self.used += 1;
            }
        }
    }

    /// The documented code of an angle delta of `steps` (already divided by `1 << key`).
    pub(crate) fn put_angle_delta(w: &mut BitWriter, steps: i32, key: u8) {
        if steps == 0 {
            w.put(0, 1);
            return;
        }
        w.put(1, 1);
        if steps == -1 {
            w.put(0, 3);
            return;
        }
        // the smallest t in 1..=6 whose sign-bit-weight form holds `steps`
        for t in 1..=6u32 {
            let half = 1i32 << (t - 1);
            let v = if steps > 0 { steps - half } else { steps + half };
            let fits = if steps > 0 { (0..half).contains(&v) } else { (-half..0).contains(&v) };
            if fits {
                w.put(i64::from(t), 3);
                w.put(i64::from(v), t);
                return;
            }
        }
        w.put(7, 3);
        w.put(i64::from(steps), 12 - u32::from(key));
    }

    pub(crate) fn put_position_delta(w: &mut BitWriter, delta: i32) {
        if (-64..64).contains(&delta) {
            w.put(0, 1);
            w.put(i64::from(delta), 7);
        } else {
            w.put(1, 1);
            w.put(i64::from(delta), 16);
        }
    }

    /// Encode frames (positions, then per triplet three raw angles that are multiples of `1 << key`
    /// whose deltas are multiples of it too) into a clip chunk in the documented layout.
    pub(crate) fn encode_clip(frames: &[RawFrame], key: u8, padding: usize) -> Vec<u8> {
        let mut w = BitWriter::default();
        let first = &frames[0];
        for p in first.position {
            w.put(i64::from(p), 16);
        }
        for t in &first.rotations {
            for &a in t {
                w.put(i64::from(i32::from(a) >> key), 12 - u32::from(key));
            }
        }
        for pair in frames.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            for k in 0..3 {
                put_position_delta(&mut w, next.position[k] - prev.position[k]);
            }
            for (a, b) in prev.rotations.iter().zip(&next.rotations) {
                for k in 0..3 {
                    let delta = i32::from(b[k]) - i32::from(a[k]);
                    assert_eq!(delta % (1 << key), 0, "a delta the key cannot hold");
                    put_angle_delta(&mut w, delta >> key, key);
                }
            }
        }
        let size = w.bytes.len();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(frames.len() as u16).to_le_bytes());
        chunk.extend_from_slice(&(size as u16).to_le_bytes());
        chunk.push(key);
        chunk.extend_from_slice(&w.bytes);
        chunk.extend(std::iter::repeat_n(0u8, padding));
        chunk
    }

    /// A pack in the documented layout: entries are `(bones, frames, chunk)`.
    pub(crate) fn build_pack(entries: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = (entries.len() as u32).to_le_bytes().to_vec();
        for (bones, frames, chunk) in entries {
            out.extend_from_slice(&bones.to_le_bytes());
            out.extend_from_slice(&frames.to_le_bytes());
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out
    }

    fn frame(position: [i32; 3], rotations: &[[i16; 3]]) -> RawFrame {
        RawFrame { position, rotations: rotations.to_vec() }
    }

    fn entry(chunk: &[u8], frames: u32) -> PackEntry<'_> {
        PackEntry { bones: 2, frames, chunk }
    }

    #[test]
    fn the_bit_reader_reads_msb_first_and_sign_extends() {
        let mut r = BitReader::new(&[0b1010_0000, 0b1111_1111, 0b0000_0001]);
        assert_eq!(r.unsigned(1), Some(1));
        assert_eq!(r.unsigned(2), Some(0b01));
        assert_eq!(r.signed(5), Some(0), "00000 is zero");
        assert_eq!(r.signed(4), Some(-1), "1111 is -1");
        assert_eq!(r.signed(8), Some(-16), "1111 0000 is -16");
        assert_eq!(r.bits_left(), 4);
        assert_eq!(r.unsigned(5), None, "a read past the end fails");
        assert_eq!(r.unsigned(4), Some(1));
        assert_eq!(r.unsigned(1), None);
        // a one-bit signed read is 0 or -1, wide reads work, and a 25-bit read is refused
        let mut r = BitReader::new(&[0x80, 0xFF, 0xFF, 0xFF]);
        assert_eq!(r.signed(1), Some(-1));
        // 7 zero bits, then 17 ones
        assert_eq!(r.signed(24), Some(0x1_FFFF));
        let mut r = BitReader::new(&[0xFF, 0xFF, 0xFF]);
        assert_eq!(r.signed(24), Some(-1));
        assert_eq!(BitReader::new(&[0; 8]).unsigned(25), None);
        assert_eq!(BitReader::new(&[]).unsigned(0), Some(0));
    }

    #[test]
    fn splits_a_pack_into_entries_and_tells_clips_from_special_entries() {
        let clip = encode_clip(&[frame([0, -100, 0], &[[0, 0, 0], [16, 32, 48]])], 0, 0);
        let pack = build_pack(&[(2, 1, clip.clone()), (1, 1, vec![9, 9, 9, 9]), (2, 1, clip.clone())]);
        let entries = split_pack(&pack).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_clip() && !entries[1].is_clip() && entries[2].is_clip());
        assert_eq!((entries[1].bones, entries[1].frames, entries[1].chunk), (1, 1, &[9u8, 9, 9, 9][..]));
        // trailing bytes are ignored
        let mut padded = pack.clone();
        padded.extend_from_slice(&[7; 9]);
        assert_eq!(split_pack(&padded).unwrap().len(), 3);
    }

    #[test]
    fn rejects_bad_packs_before_allocating() {
        assert!(matches!(split_pack(&[]), Err(ModelError::Truncated { .. })));
        assert!(matches!(split_pack(&u32::MAX.to_le_bytes()), Err(ModelError::TooMany { .. })));
        assert!(matches!(split_pack(&(MAX_PACK_ENTRIES as u32 + 1).to_le_bytes()), Err(ModelError::TooMany { .. })));
        // a count the file cannot hold even as bare headers
        assert!(matches!(split_pack(&1000u32.to_le_bytes()), Err(ModelError::Truncated { .. })));
        let pack = build_pack(&[(2, 1, vec![0; 20])]);
        // a chunk that runs past the end
        assert!(matches!(split_pack(pack.get(..pack.len() - 1).unwrap()), Err(ModelError::Truncated { .. })));
        let mut bad = pack.clone();
        bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(split_pack(&bad), Err(ModelError::Truncated { .. })));
        // a header cut short
        assert!(matches!(split_pack(pack.get(..10).unwrap()), Err(ModelError::Truncated { .. })));
    }

    #[test]
    fn decodes_frame_zero_and_every_kind_of_delta() {
        for key in [0u8, 2, 4] {
            let k = 1i16 << key;
            // translation: no change, small step, wide step, both signs; angles: no change, -1, every
            // small code, the escape, big jumps and a wrap past 4095
            let frames = vec![
                frame([0, -466, 7], &[[0, 0, 0], [1024 / k * k, -306 / k * k, 0], [100 / k * k, 4000 / k * k, -8 / k * k]]),
                frame([0, -466, 7], &[[0, 0, 0], [1024 / k * k, -306 / k * k, 0], [100 / k * k, 4000 / k * k, -8 / k * k]]),
                frame([5, -460, 6], &[[-k, 0, 3 * k], [1024 / k * k - k, -306 / k * k + 2 * k, 0], [100 / k * k, 4000 / k * k, -8 / k * k]]),
                frame([5, -460 + 200, -300], &[[40 * k, -33 * k, -64 * k], [0, 63 * k, -2 * k], [1500 / k * k, 4000 / k * k + 5 * k, -8 / k * k]]),
                frame([-20, 5, 9], &[[-1000 / k * k, 1000 / k * k, -k * 5], [3 * k, -3 * k, 4 * k], [-1000 / k * k, 20 * k, 1500 / k * k]]),
            ];
            let chunk = encode_clip(&frames, key, 2);
            let clip = decode_clip(&entry(&chunk, frames.len() as u32), 3).unwrap();
            assert_eq!(clip.key, key);
            assert_eq!(clip.frames.len(), frames.len(), "key {key}");
            assert!(!clip.ended_early);
            // (frame 0 stores signed 12-bit angles, so 4000 reads back as -96: the same angle)
            let modulo = |rotations: &[[i16; 3]]| -> Vec<[i32; 3]> {
                rotations.iter().map(|t| t.map(|a| i32::from(a).rem_euclid(4096))).collect()
            };
            for (i, (got, want)) in clip.frames.iter().zip(&frames).enumerate() {
                assert_eq!(got.position, want.position, "key {key} frame {i}");
                assert_eq!(modulo(&got.rotations), modulo(&want.rotations), "key {key} frame {i}");
            }
        }
    }

    #[test]
    fn every_delta_code_round_trips_at_every_key() {
        // all steps from -80 to 80 (the whole compact range and both sides of the escape)
        for key in [0u8, 2, 4] {
            let k = 1i16 << key;
            let mut frames = vec![frame([0, 0, 0], &[[0, 0, 0]])];
            let mut angle = 0i16;
            for steps in -80i16..=80 {
                angle = angle.wrapping_add(steps * k);
                frames.push(frame([i32::from(steps) * 3, 0, 0], &[[angle, 0, 0]]));
            }
            let chunk = encode_clip(&frames, key, 0);
            let clip = decode_clip(&entry(&chunk, frames.len() as u32), 1).unwrap();
            assert_eq!(clip.frames, frames, "key {key}");
        }
    }

    #[test]
    fn the_frame_count_is_the_larger_of_the_two_and_a_short_stream_keeps_its_frames() {
        let frames: Vec<RawFrame> = (0..6).map(|i| frame([i, 0, 0], &[[0, 0, 0], [(i * 4) as i16, 0, 0]])).collect();
        let chunk = encode_clip(&frames, 2, 3);
        // the header's count is the smaller: the chunk's own 16-bit count wins
        assert_eq!(decode_clip(&entry(&chunk, 2), 2).unwrap().frames.len(), 6);
        // the header's count is the larger: decode up to it; the stream ends first, and says so
        let longer = decode_clip(&entry(&chunk, 9), 2).unwrap();
        assert!(longer.frames.len() >= 6 && longer.frames.len() <= 9);
        assert_eq!(&longer.frames[..6], &frames[..]);
        assert!(longer.ended_early || longer.frames.len() == 9);
        // a count of billions is capped, never allocated
        let capped = decode_clip(&entry(&chunk, u32::MAX), 2).unwrap();
        assert!(capped.frames.len() <= MAX_ANIM_FRAMES);
        assert!(capped.ended_early);
    }

    #[test]
    fn the_reader_is_bounded_by_the_chunk_not_by_the_declared_size() {
        // A clip whose last frame needs one byte more than it declares (as the owned `am` clip 15
        // does): shrink the declared size by one and the frame must still decode from the padding.
        let frames: Vec<RawFrame> = (0..4).map(|i| frame([i * 1000, 0, 0], &[[0, 0, 0], [(i * 100) as i16, 0, 0]])).collect();
        let mut chunk = encode_clip(&frames, 0, 2);
        let size = u16::from_le_bytes([chunk[2], chunk[3]]);
        chunk[2..4].copy_from_slice(&(size - 1).to_le_bytes());
        let clip = decode_clip(&entry(&chunk, 4), 2).unwrap();
        assert_eq!(clip.frames, frames);
    }

    #[test]
    fn corrupt_clips_are_errors_not_panics() {
        let frames = vec![frame([0, 0, 0], &[[0, 0, 0], [8, 8, 8]]); 3];
        let good = encode_clip(&frames, 0, 0);
        assert!(decode_clip(&entry(&good, 3), 2).is_ok());
        // too short for a header, or for frame 0
        assert!(matches!(decode_clip(&entry(&[], 1), 2), Err(ModelError::Truncated { .. })));
        assert!(matches!(decode_clip(&entry(&good[..4], 1), 2), Err(ModelError::Truncated { .. })));
        // frame 0 of more triplets than the stream holds
        assert!(matches!(decode_clip(&entry(&good, 3), 40), Err(ModelError::Invalid { .. } | ModelError::Truncated { .. })));
        assert!(matches!(decode_clip(&entry(&good, 3), 0), Err(ModelError::Invalid { .. })));
        assert!(matches!(decode_clip(&entry(&good, 3), 10_000), Err(ModelError::Invalid { .. })));
        // a key the format does not have
        let mut bad_key = good.clone();
        bad_key[4] = 5;
        assert!(matches!(decode_clip(&entry(&bad_key, 3), 2), Err(ModelError::Invalid { .. })));
        // a declared size that disagrees with the chunk: the corrupt frog clip's signature
        let mut long_padding = good.clone();
        long_padding.extend(std::iter::repeat_n(0u8, MAX_CLIP_PADDING + 1));
        assert!(matches!(decode_clip(&entry(&long_padding, 3), 2), Err(ModelError::Invalid { .. })));
        let mut huge_size = good.clone();
        huge_size[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(decode_clip(&entry(&huge_size, 3), 2), Err(ModelError::Invalid { .. })));
    }

    #[test]
    fn damaged_clips_never_panic() {
        let frames: Vec<RawFrame> = (0..5).map(|i| frame([i * 40, -i, 0], &[[0, 4 * i as i16, 0], [8 * i as i16, 0, -4]])).collect();
        let good = encode_clip(&frames, 2, 1);
        for n in 0..good.len() {
            let _ = decode_clip(&entry(good.get(..n).unwrap_or(&[]), 5), 2);
        }
        for i in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                if let Ok(clip) = decode_clip(&entry(&b, 5), 2) {
                    let _ = clip_to_animation(&clip, 1);
                }
            }
        }
    }

    #[test]
    fn angles_are_degrees_modulo_a_turn() {
        assert_eq!(raw_angle_degrees(0), 0.0);
        assert_eq!(raw_angle_degrees(1024), 90.0);
        assert_eq!(raw_angle_degrees(-1024), 270.0);
        assert_eq!(raw_angle_degrees(4096), 0.0);
        assert_eq!(raw_angle_degrees(4096 + 512), 45.0);
        assert_eq!(raw_angle_degrees(-4096 - 512), 315.0);
        assert!(raw_angle_degrees(i16::MIN) >= 0.0 && raw_angle_degrees(i16::MAX) < 360.0);
    }

    #[test]
    fn a_clip_becomes_a_field_animation_with_a_root_rotation_and_a_height() {
        // two bones: three triplets per frame, the first is the root rotation
        let frames = vec![
            frame([10, -466, -3], &[[0, 512, 0], [-1024, 0, 0], [0, 0, 1024]]),
            frame([12, -460, -3], &[[0, 512, 0], [-1024, 0, 0], [0, 0, 1024]]),
        ];
        let clip = RawClip { key: 0, stream_frames: 2, header_frames: 2, frames, ended_early: false };
        let anim = clip_to_animation(&clip, 2).unwrap();
        assert_eq!((anim.frame_count(), anim.bone_count(), anim.rotation_order), (2, 2, [1, 0, 2]));
        let f0 = anim.frame(0).unwrap();
        assert_eq!(f0.root_rotation, [0.0, 45.0, 0.0]);
        // y is stored negated: the field pose negates it back, so the native y is the stored one
        assert_eq!(f0.root_translation, [10.0, 466.0, -3.0]);
        assert_eq!(f0.rotations, vec![[270.0, 0.0, 0.0], [0.0, 0.0, 90.0]]);
        let bones = vec![
            crate::model::ModelBone { name: "a".into(), parent: None, length: 10.0 },
            crate::model::ModelBone { name: "b".into(), parent: Some(0), length: 5.0 },
        ];
        let pose = anim.pose(&bones, 0).unwrap();
        let origin = crate::model::transform_point(&pose[0], [0.0; 3]);
        assert_eq!(origin, [10.0, -466.0, -3.0], "the root sits at the stored translation");
        // a wrong triplet count is an error
        assert!(clip_to_animation(&clip, 3).is_err());
        assert!(clip_to_animation(&clip, 1).is_err());
    }

    #[test]
    fn single_bone_skeletons_and_weapon_tracks_have_one_triplet() {
        let frames = vec![frame([1, -2, 3], &[[1024, 0, 0]]), frame([2, -2, 3], &[[2048, 0, 0]])];
        let clip = RawClip { key: 0, stream_frames: 2, header_frames: 2, frames, ended_early: false };
        let anim = clip_to_animation(&clip, 1).unwrap();
        assert_eq!((anim.frame_count(), anim.bone_count()), (2, 1));
        let f1 = anim.frame(1).unwrap();
        assert_eq!((f1.root_rotation, f1.root_translation), ([180.0, 0.0, 0.0], [2.0, 2.0, 3.0]));
        assert_eq!(f1.rotations, vec![[0.0; 3]], "the single bone follows the root");
        // and a skeleton of two bones refuses the same clip
        assert!(clip_to_animation(&clip, 2).is_err());
    }

    #[test]
    fn a_rest_animation_copies_frame_zero_or_is_the_identity() {
        let frames = vec![frame([0, -5, 0], &[[0, 0, 0], [1024, 0, 0]]), frame([0, -9, 0], &[[0, 0, 0], [2048, 0, 0]])];
        let clip = RawClip { key: 0, stream_frames: 2, header_frames: 2, frames, ended_early: false };
        let anim = clip_to_animation(&clip, 1);
        assert!(anim.is_err(), "one bone needs one triplet");
        let two = clip_to_animation(&clip, 1).err();
        assert!(two.is_some());
        let anim = clip_to_animation(&RawClip { frames: clip.frames.iter().map(|f| RawFrame { position: f.position, rotations: f.rotations.clone() }).collect(), ..clip.clone() }, 1);
        assert!(anim.is_err());
        // identity
        let id = rest_animation(None, 3).unwrap();
        assert_eq!((id.frame_count(), id.bone_count()), (1, 3));
        let f = id.frame(0).unwrap();
        assert_eq!((f.root_rotation, f.root_translation, f.rotations.clone()), ([0.0; 3], [0.0; 3], vec![[0.0; 3]; 3]));
        // a copy of frame 0 of a real animation
        let source = animation_from_frames(
            &[
                AnimFrame { root_rotation: [1.0, 2.0, 3.0], root_translation: [4.0, 5.0, 6.0], rotations: vec![[7.0, 8.0, 9.0]] },
                AnimFrame { root_rotation: [0.0; 3], root_translation: [0.0; 3], rotations: vec![[0.0; 3]] },
            ],
            1,
        )
        .unwrap();
        let copy = rest_animation(Some(&source), 1).unwrap();
        assert_eq!(copy.frame_count(), 1);
        assert_eq!(copy.frame(0), source.frame(0));
    }
}
