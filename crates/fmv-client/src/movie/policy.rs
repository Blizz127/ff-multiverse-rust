//! Explicit admission of an owned movie: the one inspected opening, pinned to its exact bytes and
//! timestamps, or any other owned movie under the general profile. This is a port policy, not
//! a general repair for discontinuous media or a claim of original-PC parity.

use super::ControllerError;
use fmv_media::{AudioRequirement, DecodedAudio, MediaMetadata, Request, TimeBase};
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

const OPENING_BYTES: u64 = 32_601_760;
const OPENING_BLAKE3: &str = "622ac618acec7651bf9f4516866816004f9636fab5c3e419f9d5d55c40e0d467";
// Independently inspected SHA256:
// 3ab9329e809f8b881698bd683629060ce7d91e8a30ea7d04775bc571f69a9e3d

/// No default: the caller must opt into the inspected FFNx-derived PCM origin.
#[derive(Debug, Clone)]
pub enum Admission {
    /// Concatenate the owned opening's decoded PCM and place sample zero at
    /// presentation zero. Retain every original audio PTS; require_contiguous()
    /// still refuses this asset. Do not add 11ms silence or drop 128 samples.
    ///
    /// Evidence: FFNx 661f92a36c3f1d6498317e04327c31dc24d06222,
    /// src/video/movies.cpp:885-982 appends PCM without using audio PTS. This
    /// controller's exact mixer clock and finite tail are our explicit port
    /// policy; FFNx itself uses a wall clock constrained by its audio stream.
    OpeningFfnxPcmZeroOrigin,
    /// Any other owned movie, which has no pinned bytes or timestamps. It is played like the
    /// opening: decoded PCM from sample zero on the controller's exact mixer clock, then the
    /// finite video tail. Nothing asset-specific is checked, but everything generic is: a
    /// regular file within the size limit, unchanged (size and modification time) from
    /// admission until the decoder has read all of it, PCM that matches its own timestamp
    /// inventory, the 600 second limit and the one-second video tail rule.
    ///
    /// The probed metadata decides the clock. A file with audio takes the PCM path above. A
    /// file that is one VP8 video stream and no audio is a silent movie: it has no audio, none
    /// is invented, and its clock is real time against the video frames' own timestamps up to
    /// the video endpoint.
    General,
    #[cfg(test)]
    Synthetic { digest: blake3::Hash },
}

impl Admission {
    /// Only the general profile may meet a file with no audio stream.
    pub(super) fn audio_requirement(&self) -> AudioRequirement {
        match self {
            Admission::General => AudioRequirement::Optional,
            Admission::OpeningFfnxPcmZeroOrigin => AudioRequirement::Required,
            #[cfg(test)]
            Admission::Synthetic { .. } => AudioRequirement::Required,
        }
    }
}

/// The identity of an admitted file that has no pinned hash: its canonical path, size and
/// modification time as admitted. [`FileStamp::verify`] says whether it is still the same file.
#[derive(Debug, Clone)]
pub(super) struct FileStamp {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    pub(super) fn verify(&self) -> Result<(), ControllerError> {
        let now = fs::metadata(&self.path).map_err(ControllerError::Io)?;
        if !now.is_file() || now.len() != self.len || now.modified().ok() != self.modified {
            return Err(ControllerError::Admission(
                "movie changed while it was being decoded",
            ));
        }
        Ok(())
    }
}

/// Loading-stage work only. Hashing is bounded but synchronous; do this before
/// entering normal frame polling, on a loading worker if the UI must stay live.
/// The caller must keep the selected owned asset immutable during playback.
pub(super) fn preflight(
    request: &mut Request,
    admission: &Admission,
) -> Result<Option<FileStamp>, ControllerError> {
    if !request.path.is_absolute() {
        return Err(ControllerError::Admission("absolute movie path required"));
    }
    let path = fs::canonicalize(&request.path).map_err(ControllerError::Io)?;
    // Reject devices/FIFOs before opening: opening a named pipe for read can
    // block waiting for a writer. The caller owns the immutable-path contract;
    // the opened descriptor is independently checked again below.
    let named = fs::metadata(&path).map_err(ControllerError::Io)?;
    if !named.is_file() || named.len() == 0 || named.len() > request.limits.max_media_bytes {
        return Err(ControllerError::Admission(
            "movie exceeds the regular-file size policy",
        ));
    }
    let mut file = File::open(&path).map_err(ControllerError::Io)?;
    let before = file.metadata().map_err(ControllerError::Io)?;
    if !before.is_file() || before.len() == 0 || before.len() > request.limits.max_media_bytes {
        return Err(ControllerError::Admission(
            "movie exceeds the regular-file size policy",
        ));
    }
    let expected = match admission {
        Admission::OpeningFfnxPcmZeroOrigin => {
            if before.len() != OPENING_BYTES {
                return Err(ControllerError::Admission("uninspected opening size"));
            }
            blake3::Hash::from_hex(OPENING_BLAKE3).expect("literal hash")
        }
        Admission::General => {
            // Nothing to hash against: the file's identity is its size and modification time,
            // which must be the same on the opened descriptor and on the path now, and are
            // checked again by the controller when the decoder has read the whole file.
            let named_again = fs::metadata(&path).map_err(ControllerError::Io)?;
            if named_again.len() != before.len()
                || named_again.modified().ok() != before.modified().ok()
            {
                return Err(ControllerError::Admission(
                    "movie changed while it was being opened",
                ));
            }
            let stamp = FileStamp {
                path: path.clone(),
                len: before.len(),
                modified: before.modified().ok(),
            };
            request.path = path;
            return Ok(Some(stamp));
        }
        #[cfg(test)]
        Admission::Synthetic { digest } => *digest,
    };
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut remaining = before.len();
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded chunk");
        let count = file
            .read(&mut buffer[..limit])
            .map_err(ControllerError::Io)?;
        if count == 0 {
            return Err(ControllerError::Admission("movie changed while hashing"));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let after = file.metadata().map_err(ControllerError::Io)?;
    if after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
        || hasher.finalize() != expected
    {
        return Err(ControllerError::Admission(
            "movie identity does not match the inspected asset",
        ));
    }
    request.path = path;
    Ok(None)
}

/// Check what the decoder prepared against the admission. A movie without audio (`audio` and
/// `metadata.audio` both `None`) passes only the general profile; one with audio must have PCM
/// that matches its timestamp inventory.
pub(super) fn admit(
    admission: &Admission,
    metadata: &MediaMetadata,
    audio: Option<&DecodedAudio>,
) -> Result<(), ControllerError> {
    let (Some(audio), Some(inventory)) = (audio, metadata.audio.as_ref()) else {
        // Both absent: a silent movie. One without the other is a decoder contradicting
        // itself, and the pinned profiles never accept silence.
        return if audio.is_none()
            && metadata.audio.is_none()
            && matches!(admission, Admission::General)
        {
            Ok(())
        } else {
            Err(ControllerError::Admission(
                "audio presence differs from the admitted profile",
            ))
        };
    };
    if audio.metadata.as_ref() != inventory.as_ref()
        || audio.sample_rate != inventory.sample_rate()
        || audio.frames() as u64 != inventory.samples()
        || !audio.pcm_le.len().is_multiple_of(4)
    {
        return Err(ControllerError::Admission(
            "PCM does not match its timestamp inventory",
        ));
    }
    if matches!(admission, Admission::OpeningFfnxPcmZeroOrigin) {
        let video = &metadata.video;
        let observed = inventory.as_ref();
        let time_base = TimeBase {
            numerator: 1,
            denominator: 1000,
        };
        let continuity = observed.continuity();
        let profile = video.width == 1280
            && video.height == 896
            && video.time_base == time_base
            && video.frames.len() == 1792
            && video.frames.iter().enumerate().all(|(i, frame)| {
                // This pinned asset's actual inspected integer millisecond PTS,
                // not a general inference from an advertised frame rate.
                frame.pts == ((i as i64 * 1000 + 7) / 15) && frame.duration == 66
            })
            && audio.sample_rate == 22_050
            && audio.frames() == 2_632_960
            && observed.time_base() == time_base
            && observed.initial_pts() == 11
            && observed.frames().len() == 5299
            && observed.frames().last().is_some_and(|last| {
                (last.pts, last.samples, last.sample_offset) == (119401, 256, 2_632_704)
            })
            && (
                continuity.minimum_origin_numerator,
                continuity.maximum_origin_numerator,
                continuity.origin_denominator,
                continuity.one_tick_numerator,
            ) == (32_200, 242_550, 22_050_000, 22_050)
            && audio.require_contiguous().is_err();
        if !profile {
            return Err(ControllerError::Admission(
                "opening decoder profile differs from inspected timestamps",
            ));
        }
    }
    Ok(())
}
