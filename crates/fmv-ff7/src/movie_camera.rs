//! Raw PC movie-camera (`.cam`) records from `cd/moviecam.lgp`.
//!
//! The owned English Steam app 3837340/build 21140793 program loads the complete CAM
//! member and selects `base + 40 * movie_frame`, without a header or a +/-1 adjustment
//! (loader function `0x40ADB0`, selection instruction `0x40AD49` in `0x40AC9A`).
//! Its camera consumers read the signed basis, translation,
//! pan and zoom below. The words at `0x12` and `0x26` remain opaque. Evidence and its
//! limits are recorded in `docs/FIELD-TIMELINE-DESIGN.md`, "PC movie contract"; the
//! executable SHA-256 is `26998200234f7a49a700bdc1ae5d0a43fd001cc909871a366cf82bdffaa351c9`.
//!
//! This parser preserves integers exactly, including zero matrices and zero zoom. It
//! does not create a projection, interpret either opaque word, choose the first video
//! frame, or infer playback completion from the number of records. Those are separate
//! runtime concerns. The input is borrowed, with no allocation or unbounded scan.

use crate::util::{slice_at, u16_at, u32_at};
use thiserror::Error;

/// Bytes per headerless PC movie-camera record.
pub const MOVIE_CAMERA_RECORD_BYTES: usize = 40;
/// Parser resource ceiling, not a native format limit, clock period or movie duration.
/// All CAM members in the inspected owned archive together contain only 56,443 records.
pub const MAX_MOVIE_CAMERA_FRAMES: usize = 65_536;
/// Maximum accepted member length (2.5 MiB), checked before decoding any record.
pub const MAX_MOVIE_CAMERA_BYTES: usize = MAX_MOVIE_CAMERA_FRAMES * MOVIE_CAMERA_RECORD_BYTES;

/// A malformed member or a requested record outside that member. Errors contain only
/// lengths and indexes, never game data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MovieCameraError {
    #[error("movie camera member contains no records")]
    Empty,
    #[error("movie camera member is {bytes} bytes, over the limit of {limit}")]
    TooLarge { bytes: usize, limit: usize },
    #[error("movie camera member is {bytes} bytes: the final {MOVIE_CAMERA_RECORD_BYTES}-byte record is incomplete")]
    TruncatedRecord { bytes: usize },
    #[error("movie camera frame {index} is outside the member's {frames} records")]
    FrameOutOfRange { index: usize, frames: usize },
}

/// One record's raw little-endian integers, with no coordinate conversion or repair.
/// `Debug` omits the decoded data.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MovieCameraRecord {
    /// Nine signed basis entries in stored row order, at offsets `0x00..=0x10`.
    pub basis: [[i16; 3]; 3],
    /// Raw word at `0x12`, excluded from the nine-entry basis. Its meaning is not assumed.
    pub word_12: u16,
    /// Signed translation at `0x14`, `0x18`, `0x1c`.
    pub translation: [i32; 3],
    /// Signed horizontal and vertical pan at `0x20`, `0x22`.
    pub pan: [i16; 2],
    /// Signed zoom at `0x24`, including zero or negative values as stored.
    pub zoom: i16,
    /// Raw final word at `0x26`. The PSX RGB24 interpretation is not established on PC.
    pub word_26: u16,
}

impl std::fmt::Debug for MovieCameraRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MovieCameraRecord").finish_non_exhaustive()
    }
}

/// A nonempty, length-validated CAM member, borrowing the original bytes.
/// Record count is a data bound; it does not indicate whether a movie has ended.
#[derive(Clone, Copy)]
pub struct MovieCameraTrack<'a> {
    bytes: &'a [u8],
}

/// `Debug` reports the record count, not the borrowed game data.
impl std::fmt::Debug for MovieCameraTrack<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MovieCameraTrack").field("frames", &self.frame_count()).finish()
    }
}

impl<'a> MovieCameraTrack<'a> {
    /// Validate a complete headerless member. Empty input, an incomplete final record,
    /// and input over [`MAX_MOVIE_CAMERA_BYTES`] are errors; no trailing bytes are ignored.
    /// Record contents are deliberately not validated as usable camera poses.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MovieCameraError> {
        if bytes.is_empty() {
            return Err(MovieCameraError::Empty);
        }
        if bytes.len() > MAX_MOVIE_CAMERA_BYTES {
            return Err(MovieCameraError::TooLarge { bytes: bytes.len(), limit: MAX_MOVIE_CAMERA_BYTES });
        }
        if !bytes.len().is_multiple_of(MOVIE_CAMERA_RECORD_BYTES) {
            return Err(MovieCameraError::TruncatedRecord { bytes: bytes.len() });
        }
        Ok(Self { bytes })
    }

    /// Number of stored records, with no implication about playback time or completion.
    pub fn frame_count(&self) -> usize {
        self.bytes.len() / MOVIE_CAMERA_RECORD_BYTES
    }

    /// Decode exactly the requested zero-based record. The supplied index is not shifted,
    /// wrapped or clamped. An out-of-range index is an error, not an end-of-stream signal.
    pub fn frame(&self, index: usize) -> Result<MovieCameraRecord, MovieCameraError> {
        let outside = MovieCameraError::FrameOutOfRange { index, frames: self.frame_count() };
        if index >= self.frame_count() {
            return Err(outside);
        }
        let offset = index.checked_mul(MOVIE_CAMERA_RECORD_BYTES).ok_or(outside)?;
        let bytes = slice_at(self.bytes, offset, MOVIE_CAMERA_RECORD_BYTES).ok_or(outside)?;
        // parse() and the checked slice establish a complete record. The readers still
        // return Option so all offset arithmetic stays total even if this helper changes.
        decode_record(bytes).ok_or(outside)
    }
}

fn decode_record(bytes: &[u8]) -> Option<MovieCameraRecord> {
    let short = |offset| u16_at(bytes, offset).map(|v| v as i16);
    let long = |offset| u32_at(bytes, offset).map(|v| v as i32);
    Some(MovieCameraRecord {
        basis: [
            [short(0x00)?, short(0x02)?, short(0x04)?],
            [short(0x06)?, short(0x08)?, short(0x0a)?],
            [short(0x0c)?, short(0x0e)?, short(0x10)?],
        ],
        word_12: u16_at(bytes, 0x12)?,
        translation: [long(0x14)?, long(0x18)?, long(0x1c)?],
        pan: [short(0x20)?, short(0x22)?],
        zoom: short(0x24)?,
        word_26: u16_at(bytes, 0x26)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-written bytes, independent of the decoder: every field boundary is distinct,
    // both signed widths exercise their extrema, and word_12 differs from the last basis entry.
    const FIRST: [u8; 40] = [
        0x34, 0x12, 0x00, 0x80, 0xff, 0x7f,
        0xff, 0xff, 0x02, 0x00, 0xfe, 0xff,
        0xcd, 0xab, 0x00, 0x00, 0x67, 0x45,
        0xef, 0xbe,
        0x00, 0x00, 0x00, 0x80,
        0xff, 0xff, 0xff, 0x7f,
        0x78, 0x56, 0x34, 0x12,
        0x35, 0xff, 0x2b, 0x01,
        0x4f, 0xff, 0xff, 0xff,
    ];

    #[test]
    fn every_field_keeps_its_offset_sign_and_opaque_words() {
        let track = MovieCameraTrack::parse(&FIRST).unwrap();
        let record = track.frame(0).unwrap();
        assert_eq!(track.frame_count(), 1);
        assert_eq!(record.basis, [[4660, i16::MIN, i16::MAX], [-1, 2, -2], [-21555, 0, 17767]]);
        assert_eq!(record.word_12, 0xbeef);
        assert_eq!(record.translation, [i32::MIN, i32::MAX, 0x1234_5678]);
        assert_eq!(record.pan, [-203, 299]);
        assert_eq!(record.zoom, -177);
        assert_eq!(record.word_26, 0xffff);
    }

    #[test]
    fn direct_record_index_has_no_header_shift_wrap_or_clamp() {
        let mut bytes = FIRST.to_vec();
        bytes.extend_from_slice(&[0xff; 40]);
        let track = MovieCameraTrack::parse(&bytes).unwrap();
        assert_eq!(track.frame_count(), 2);
        assert_eq!(track.frame(0).unwrap(), MovieCameraTrack::parse(&FIRST).unwrap().frame(0).unwrap());
        let last = track.frame(1).unwrap();
        assert_eq!(last.basis, [[-1; 3]; 3]);
        assert_eq!(last.translation, [-1; 3]);
        assert_eq!(last.pan, [-1; 2]);
        assert_eq!(last.zoom, -1);
        assert_eq!((last.word_12, last.word_26), (u16::MAX, u16::MAX));
        for index in [2, 3, usize::MAX / 40, usize::MAX] {
            assert_eq!(track.frame(index), Err(MovieCameraError::FrameOutOfRange { index, frames: 2 }));
        }
    }

    #[test]
    fn zero_matrix_and_zoom_remain_zero_with_arbitrary_opaque_words() {
        let mut bytes = [0; 40];
        bytes[18..20].copy_from_slice(&0x1234u16.to_le_bytes());
        bytes[38..40].copy_from_slice(&0x5678u16.to_le_bytes());
        let record = MovieCameraTrack::parse(&bytes).unwrap().frame(0).unwrap();
        assert_eq!(record.basis, [[0; 3]; 3]);
        assert_eq!(record.translation, [0; 3]);
        assert_eq!(record.pan, [0; 2]);
        assert_eq!(record.zoom, 0);
        assert_eq!((record.word_12, record.word_26), (0x1234, 0x5678));
        let zero = MovieCameraTrack::parse(&[0; 40]).unwrap().frame(0).unwrap();
        assert_eq!((zero.word_12, zero.word_26), (0, 0));
    }

    #[test]
    fn empty_and_every_partial_record_are_rejected_without_dropping_a_tail() {
        assert_eq!(MovieCameraTrack::parse(&[]).unwrap_err(), MovieCameraError::Empty);
        for prefix in [0, 40, 80] {
            for tail in 1..40 {
                let bytes = vec![0; prefix + tail];
                assert_eq!(
                    MovieCameraTrack::parse(&bytes).unwrap_err(),
                    MovieCameraError::TruncatedRecord { bytes: prefix + tail },
                );
            }
        }
    }

    #[test]
    fn the_resource_bound_accepts_its_last_record_and_refuses_larger_inputs() {
        let mut bytes = vec![0; MAX_MOVIE_CAMERA_BYTES];
        let track = MovieCameraTrack::parse(&bytes).unwrap();
        assert_eq!(track.frame_count(), MAX_MOVIE_CAMERA_FRAMES);
        assert_eq!(track.frame(MAX_MOVIE_CAMERA_FRAMES - 1).unwrap().zoom, 0);
        assert_eq!(
            track.frame(MAX_MOVIE_CAMERA_FRAMES),
            Err(MovieCameraError::FrameOutOfRange { index: MAX_MOVIE_CAMERA_FRAMES, frames: MAX_MOVIE_CAMERA_FRAMES }),
        );
        // Reject oversize input before its incomplete tail, and reject a whole excess record too.
        for extra in [1, 40] {
            bytes.resize(MAX_MOVIE_CAMERA_BYTES + extra, 0);
            assert_eq!(
                MovieCameraTrack::parse(&bytes).unwrap_err(),
                MovieCameraError::TooLarge { bytes: bytes.len(), limit: MAX_MOVIE_CAMERA_BYTES },
            );
        }
    }
}
