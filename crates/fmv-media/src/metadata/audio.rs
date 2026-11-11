//! Decoded audio timestamps are observations, not a license to retime the PCM.

use super::{invalid, object, positive_u32, required_integer, timestamp_alias, TimeBase};
use crate::{Limits, MediaError};
use serde_json::Value;

/// One decoded audio frame and its range in the concatenated stereo PCM. `samples`
/// and `sample_offset` count stereo sample frames, not individual channel samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFrameTiming {
    pub pts: i64,
    pub samples: u32,
    pub sample_offset: u64,
}

/// Exact common-origin observations in seconds with `origin_denominator` as denominator.
/// For frame i, origin = PTS_i * time_base - preceding_samples / sample_rate.
/// A span no wider than one source tick is *compatible* with one continuous clock
/// under bounded timestamp quantization. It does not prove sub-tick continuity or
/// choose a quantizer, an exact origin, a codec-delay correction, or native FF7 timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioContinuity {
    pub minimum_origin_numerator: i128,
    pub maximum_origin_numerator: i128,
    pub origin_denominator: u64,
    pub one_tick_numerator: i128,
    /// First frame whose cumulative origin span exceeds one source timebase tick.
    pub first_inconsistent_frame: Option<usize>,
}

impl AudioContinuity {
    pub fn is_compatible(&self) -> bool {
        self.first_inconsistent_frame.is_none()
    }
}

/// Validated, immutable decoded audio inventory. A discontinuous inventory is valid
/// decoded media; it cannot be admitted to the documented simple continuous-clock mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioMetadata {
    sample_rate: u32,
    time_base: TimeBase,
    frames: Vec<AudioFrameTiming>,
    samples: u64,
    initial_pts: i64,
    continuity: AudioContinuity,
}

impl AudioMetadata {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        2
    }
    pub fn time_base(&self) -> TimeBase {
        self.time_base
    }
    /// The current stream profile requires an explicit zero stream-header start.
    /// This is distinct from the first decoded audio frame's PTS.
    pub fn stream_start_pts(&self) -> i64 {
        0
    }
    pub fn initial_pts(&self) -> i64 {
        self.initial_pts
    }
    pub fn frames(&self) -> &[AudioFrameTiming] {
        &self.frames
    }
    pub fn samples(&self) -> u64 {
        self.samples
    }
    pub fn continuity(&self) -> AudioContinuity {
        self.continuity
    }

    /// Admit only the explicit simple-clock policy: frame PTS must be compatible
    /// with a common sample-clock origin within ONE source tick. Equality is included
    /// as a closed observation bound, not a claim about the decoder's rounding rule.
    /// Callers still need an explicit origin policy using `initial_pts`/`time_base`;
    /// this method neither changes timestamps nor pads, drops or resamples PCM.
    pub fn require_contiguous(&self) -> Result<AudioContinuity, MediaError> {
        if let Some(frame) = self.continuity.first_inconsistent_frame {
            return Err(invalid(format!(
                "decoded audio frame {frame} exceeds the one-tick common-origin bound; a timestamped audio policy is required"
            )));
        }
        Ok(self.continuity)
    }

    pub(crate) fn pcm_bytes(&self) -> Result<usize, MediaError> {
        self.samples
            .checked_mul(4)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| invalid("decoded audio byte count overflows"))
    }
}

pub(super) struct AudioStream {
    pub index: u32,
    pub sample_rate: u32,
    pub time_base: TimeBase,
}

pub(super) fn parse(
    stream: AudioStream,
    json: &[u8],
    limits: &Limits,
) -> Result<AudioMetadata, MediaError> {
    let value: Value =
        serde_json::from_slice(json).map_err(|_| invalid("invalid audio frame metadata JSON"))?;
    let frames = object(&value, "audio frame metadata")?
        .get("frames")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("audio frame metadata needs a frames array"))?;
    if frames.is_empty() || frames.len() > limits.max_audio_frames {
        return Err(invalid("audio frame count is empty or exceeds its limit"));
    }
    let mut inventory = Vec::with_capacity(frames.len());
    let mut samples = 0u64;
    let mut initial_pts = None;
    let tick = i128::from(stream.sample_rate) * i128::from(stream.time_base.numerator);
    let denominator = u64::from(stream.sample_rate) * u64::from(stream.time_base.denominator);
    let mut continuity = AudioContinuity {
        minimum_origin_numerator: i128::MAX,
        maximum_origin_numerator: i128::MIN,
        origin_denominator: denominator,
        one_tick_numerator: tick,
        first_inconsistent_frame: None,
    };
    for (ordinal, value) in frames.iter().enumerate() {
        let frame = object(value, "audio frame")?;
        if frame
            .get("media_type")
            .is_some_and(|kind| kind.as_str() != Some("audio"))
        {
            return Err(invalid("audio frame metadata contains a non-audio frame"));
        }
        if u32::try_from(required_integer(frame, "stream_index")?).ok() != Some(stream.index) {
            return Err(invalid("audio frame metadata refers to another stream"));
        }
        if required_integer(frame, "channels")? != 2 {
            return Err(invalid(
                "decoded audio channel count differs from the stream",
            ));
        }
        // ffprobe does not always export frame sample_rate. When present it must agree;
        // the stream's explicit rate remains required, and ffmpeg is asked for that rate.
        if frame.contains_key("sample_rate")
            && positive_u32(frame, "sample_rate")? != stream.sample_rate
        {
            return Err(invalid("decoded audio sample rate differs from the stream"));
        }
        let pts = timestamp_alias(frame, "pts", "best_effort_timestamp")?;
        // Preserve signed PTS, including overlaps or preroll. The separate cumulative
        // observation check decides simple-clock compatibility; decoding never repairs it.
        let count = positive_u32(frame, "nb_samples")?;
        let next_samples = samples
            .checked_add(u64::from(count))
            .ok_or_else(|| invalid("decoded audio sample count overflows"))?;
        if next_samples
            .checked_mul(4)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .is_none_or(|bytes| bytes > limits.max_audio_bytes)
        {
            return Err(invalid("decoded audio samples exceed the PCM byte limit"));
        }
        let origin = i128::from(pts)
            .checked_mul(tick)
            .and_then(|time| {
                i128::from(samples)
                    .checked_mul(i128::from(stream.time_base.denominator))
                    .and_then(|offset| time.checked_sub(offset))
            })
            .ok_or_else(|| invalid("decoded audio timestamp arithmetic overflows"))?;
        continuity.minimum_origin_numerator = continuity.minimum_origin_numerator.min(origin);
        continuity.maximum_origin_numerator = continuity.maximum_origin_numerator.max(origin);
        if continuity.maximum_origin_numerator - continuity.minimum_origin_numerator > tick
            && continuity.first_inconsistent_frame.is_none()
        {
            continuity.first_inconsistent_frame = Some(ordinal);
        }
        inventory.push(AudioFrameTiming {
            pts,
            samples: count,
            sample_offset: samples,
        });
        initial_pts.get_or_insert(pts);
        samples = next_samples;
    }
    Ok(AudioMetadata {
        sample_rate: stream.sample_rate,
        time_base: stream.time_base,
        frames: inventory,
        samples,
        initial_pts: initial_pts.ok_or_else(|| invalid("no decoded audio frames"))?,
        continuity,
    })
}

#[cfg(test)]
mod tests;
