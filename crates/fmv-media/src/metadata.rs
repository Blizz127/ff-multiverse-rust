//! Explicit ffprobe metadata for the bounded VP8-video, stereo-Vorbis-audio profile.
//! Frame timestamps and durations come from the source; frame rate, final duration,
//! sample aspect ratio and display aspect ratio are never inferred here.
//!
//! A file with no audio stream at all (one VP8 video stream) is accepted only when the
//! caller asks for [`AudioRequirement::Optional`]; every other entry point is strict.

use crate::{Limits, MediaError};
use serde_json::{Map, Value};
use std::sync::Arc;

mod audio;
pub use audio::{AudioContinuity, AudioFrameTiming, AudioMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBase {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    pub pts: i64,
    pub duration: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub time_base: TimeBase,
    pub frames: Vec<FrameTiming>,
}

impl VideoMetadata {
    /// Exact byte length of a tightly packed RGBA frame, checked before allocation.
    pub fn frame_bytes(&self) -> Result<usize, MediaError> {
        if self.width == 0 || self.height == 0 {
            return Err(invalid("video dimensions must be positive"));
        }
        usize::try_from(self.width)
            .ok()
            .zip(usize::try_from(self.height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| invalid("RGBA frame byte length overflows"))
    }

    /// Also used by the independent timeline, since callers can construct public metadata.
    pub(crate) fn validate_timing(&self) -> Result<(), MediaError> {
        if self.time_base.numerator == 0 || self.time_base.denominator == 0 {
            return Err(invalid("video time base must be positive"));
        }
        if self.frames.first().is_none_or(|frame| frame.pts != 0) {
            return Err(invalid(
                "video must have frames beginning at timestamp zero",
            ));
        }
        let mut previous = None;
        for frame in &self.frames {
            if frame.pts < 0 || previous.is_some_and(|pts| frame.pts <= pts) {
                return Err(invalid(
                    "video timestamps must be nonnegative and strictly increasing",
                ));
            }
            if frame.duration <= 0 || frame.pts.checked_add(frame.duration).is_none() {
                return Err(invalid(
                    "each frame needs a positive integer duration and bounded endpoint",
                ));
            }
            previous = Some(frame.pts);
        }
        Ok(())
    }
}

/// Whether a file must carry its stereo Vorbis audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioRequirement {
    /// One video and one audio stream (the original strict profile).
    #[default]
    Required,
    /// The strict profile, or a file that is exactly one VP8 video stream and nothing else
    /// (a silent movie). Audio is never invented for the latter: its metadata has no audio.
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaMetadata {
    pub video: VideoMetadata,
    /// Shared with the decoded PCM; decoded sample ranges retain their source PTS.
    /// `None` only for a silent movie admitted with [`AudioRequirement::Optional`].
    pub audio: Option<Arc<AudioMetadata>>,
}

fn invalid(message: impl Into<String>) -> MediaError {
    MediaError::Invalid(message.into())
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, MediaError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn integer(value: &Value, context: &str) -> Result<i64, MediaError> {
    let integer = match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse::<i64>().ok(),
        _ => None,
    };
    integer.ok_or_else(|| invalid(format!("{context} must be an explicit integer")))
}

fn required_integer(object: &Map<String, Value>, name: &str) -> Result<i64, MediaError> {
    integer(
        object
            .get(name)
            .ok_or_else(|| invalid(format!("missing {name}")))?,
        name,
    )
}

fn positive_u32(object: &Map<String, Value>, name: &str) -> Result<u32, MediaError> {
    u32::try_from(required_integer(object, name)?)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(format!("{name} must be a positive 32-bit integer")))
}

fn timestamp_alias(
    object: &Map<String, Value>,
    first: &str,
    second: &str,
) -> Result<i64, MediaError> {
    let a = object
        .get(first)
        .map(|value| integer(value, first))
        .transpose()?;
    let b = object
        .get(second)
        .map(|value| integer(value, second))
        .transpose()?;
    match (a, b) {
        (Some(a), Some(b)) if a != b => Err(invalid(format!("{first} and {second} disagree"))),
        (Some(value), _) | (_, Some(value)) => Ok(value),
        _ => Err(invalid(format!("missing {first} or {second}"))),
    }
}

fn time_base(value: Option<&Value>) -> Result<TimeBase, MediaError> {
    let (numerator, denominator) = value
        .and_then(Value::as_str)
        .and_then(|text| text.split_once('/'))
        .ok_or_else(|| invalid("time_base must be an explicit positive rational"))?;
    let numerator = numerator.parse::<u32>().ok().filter(|value| *value > 0);
    let denominator = denominator.parse::<u32>().ok().filter(|value| *value > 0);
    match (numerator, denominator) {
        (Some(numerator), Some(denominator)) => Ok(TimeBase {
            numerator,
            denominator,
        }),
        _ => Err(invalid("time_base must be an explicit positive rational")),
    }
}

/// Whether the ffprobe stream document lists an audio stream. Only a pre-check of which
/// decoder passes to run: [`validate_streams_with`] and [`parse_with`] still decide validity.
pub(crate) fn has_audio_stream(stream_json: &[u8]) -> bool {
    serde_json::from_slice::<Value>(stream_json)
        .ok()
        .and_then(|value| value.get("streams")?.as_array().cloned())
        .is_some_and(|streams| {
            streams
                .iter()
                .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
        })
}

/// Check the stream profile and frame allocation size before requesting decoded-frame
/// metadata. This bounds our buffers; it does not bound the decoder process's working memory.
#[cfg(test)]
pub fn validate_streams(stream_json: &[u8], limits: &Limits) -> Result<(), MediaError> {
    validate_streams_with(stream_json, limits, AudioRequirement::Required)
}

/// [`validate_streams`] with an explicit audio requirement.
pub fn validate_streams_with(
    stream_json: &[u8],
    limits: &Limits,
    requirement: AudioRequirement,
) -> Result<(), MediaError> {
    parse_streams(stream_json, limits, requirement).map(|_| ())
}

fn parse_streams(
    stream_json: &[u8],
    limits: &Limits,
    requirement: AudioRequirement,
) -> Result<(VideoMetadata, Option<audio::AudioStream>, u32), MediaError> {
    if stream_json.len() > limits.max_metadata_bytes {
        return Err(invalid("metadata exceeds its byte limit"));
    }
    let streams: Value =
        serde_json::from_slice(stream_json).map_err(|_| invalid("invalid stream metadata JSON"))?;
    let streams = object(&streams, "stream metadata")?
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("stream metadata needs a streams array"))?;
    let silent_allowed = requirement == AudioRequirement::Optional && streams.len() == 1;
    if streams.len() != 2 && !silent_allowed {
        return Err(invalid(
            "exactly one video and one audio stream are required",
        ));
    }
    let mut video = None;
    let mut audio = None;
    let mut video_index = None;
    let mut indices = Vec::new();
    for stream in streams {
        let stream = object(stream, "stream")?;
        let index = u32::try_from(required_integer(stream, "index")?)
            .map_err(|_| invalid("stream index must be a nonnegative 32-bit integer"))?;
        if indices.contains(&index) {
            return Err(invalid("stream indices must be distinct"));
        }
        indices.push(index);
        if required_integer(stream, "start_pts")? != 0 {
            return Err(invalid(
                "this media profile requires both streams to start at timestamp zero",
            ));
        }
        match stream.get("codec_type").and_then(Value::as_str) {
            Some("video") if video.is_none() => {
                if stream.get("codec_name").and_then(Value::as_str) != Some("vp8") {
                    return Err(invalid("this media profile requires VP8 video"));
                }
                let width = positive_u32(stream, "width")?;
                let height = positive_u32(stream, "height")?;
                if width > limits.max_width || height > limits.max_height {
                    return Err(invalid("video dimensions exceed their limits"));
                }
                let metadata = VideoMetadata {
                    width,
                    height,
                    time_base: time_base(stream.get("time_base"))?,
                    frames: Vec::new(),
                };
                if metadata.frame_bytes()? > limits.max_frame_bytes {
                    return Err(invalid("RGBA frame exceeds its byte limit"));
                }
                video = Some(metadata);
                video_index = Some(index);
            }
            Some("audio") if audio.is_none() => {
                if stream.get("codec_name").and_then(Value::as_str) != Some("vorbis") {
                    return Err(invalid("this media profile requires Vorbis audio"));
                }
                let sample_rate = positive_u32(stream, "sample_rate")?;
                if sample_rate > 192_000 {
                    return Err(invalid("audio sample rate exceeds 192000 Hz"));
                }
                if required_integer(stream, "channels")? != 2 {
                    return Err(invalid("audio must have two channels"));
                }
                audio = Some(audio::AudioStream {
                    index,
                    sample_rate,
                    time_base: time_base(stream.get("time_base"))?,
                });
            }
            _ => {
                return Err(invalid(
                    "exactly one video and one audio stream are required",
                ))
            }
        }
    }
    let video = video.ok_or_else(|| invalid("missing video stream"))?;
    if audio.is_none() && !silent_allowed {
        return Err(invalid("missing audio stream"));
    }
    let video_index = video_index.ok_or_else(|| invalid("missing video stream index"))?;
    Ok((video, audio, video_index))
}

/// Parse stream and separately selected video/audio decoded-frame results. The byte budget
/// covers all three documents. Audio sample ranges are checked against the PCM byte budget;
/// video timing is not evidence of the audio stream's length. Each video frame must report
/// the declared dimensions; this initial profile does not support changing frame geometry.
#[cfg(test)]
pub fn parse(
    stream_json: &[u8],
    frames_json: &[u8],
    audio_json: &[u8],
    limits: &Limits,
) -> Result<MediaMetadata, MediaError> {
    parse_with(stream_json, frames_json, audio_json, limits, AudioRequirement::Required)
}

/// [`parse`] with an explicit audio requirement. A file without an audio stream (accepted
/// only for [`AudioRequirement::Optional`]) ignores `audio_json`; a file with one is parsed
/// exactly as the strict profile.
pub fn parse_with(
    stream_json: &[u8],
    frames_json: &[u8],
    audio_json: &[u8],
    limits: &Limits,
    requirement: AudioRequirement,
) -> Result<MediaMetadata, MediaError> {
    if stream_json
        .len()
        .checked_add(frames_json.len())
        .and_then(|bytes| bytes.checked_add(audio_json.len()))
        .is_none_or(|bytes| bytes > limits.max_metadata_bytes)
    {
        return Err(invalid("metadata exceeds its byte limit"));
    }
    let (mut video, audio, video_index) = parse_streams(stream_json, limits, requirement)?;
    let frames: Value =
        serde_json::from_slice(frames_json).map_err(|_| invalid("invalid frame metadata JSON"))?;
    let frames = object(&frames, "frame metadata")?
        .get("frames")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("frame metadata needs a frames array"))?;
    if frames.is_empty() || frames.len() > limits.max_frames {
        return Err(invalid("video frame count is empty or exceeds its limit"));
    }
    video.frames.reserve(frames.len());
    for frame in frames {
        let frame = object(frame, "frame")?;
        if frame
            .get("media_type")
            .is_some_and(|kind| kind.as_str() != Some("video"))
        {
            return Err(invalid("frame metadata contains a non-video frame"));
        }
        if let Some(index) = frame.get("stream_index") {
            if u32::try_from(integer(index, "frame stream_index")?).ok() != Some(video_index) {
                return Err(invalid("frame metadata refers to another stream"));
            }
        }
        if positive_u32(frame, "width")? != video.width
            || positive_u32(frame, "height")? != video.height
        {
            return Err(invalid(
                "decoded frame dimensions differ from the video stream",
            ));
        }
        video.frames.push(FrameTiming {
            pts: timestamp_alias(frame, "pts", "best_effort_timestamp")?,
            duration: timestamp_alias(frame, "duration", "pkt_duration")?,
        });
    }
    video.validate_timing()?;
    let audio = audio
        .map(|stream| audio::parse(stream, audio_json, limits).map(Arc::new))
        .transpose()?;
    Ok(MediaMetadata { video, audio })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> Limits {
        Limits {
            max_width: 1280,
            max_height: 896,
            max_frames: 10,
            max_frame_bytes: 1280 * 896 * 4,
            max_audio_bytes: 1024,
            max_metadata_bytes: 16 * 1024,
            ..Limits::default()
        }
    }

    fn fixture() -> (Value, Value) {
        (
            json!({"streams": [
                {"index": 0, "codec_type": "video", "codec_name": "vp8", "width": 1280, "height": 896, "time_base": "1/90000", "start_pts": 0},
                {"index": 1, "codec_type": "audio", "codec_name": "vorbis", "sample_rate": "44100", "channels": 2, "time_base": "1/44100", "start_pts": 0}
            ]}),
            json!({"frames": [
                {"width": 1280, "height": 896, "pts": 0, "best_effort_timestamp": 0, "duration": 3000, "pkt_duration": 3000},
                {"width": 1280, "height": 896, "pts": "3000", "pkt_duration": "4500"},
                {"width": 1280, "height": 896, "best_effort_timestamp": 7500, "duration": 1500}
            ]}),
        )
    }

    fn read(streams: &Value, frames: &Value, limits: &Limits) -> Result<MediaMetadata, MediaError> {
        parse(
            &serde_json::to_vec(streams).unwrap(),
            &serde_json::to_vec(frames).unwrap(),
            &serde_json::to_vec(
                &json!({"frames":[{"pts":0,"nb_samples":2,"stream_index":1,"channels":2}]}),
            )
            .unwrap(),
            limits,
        )
    }

    #[test]
    fn irregular_explicit_timing_and_alternate_ffprobe_names_are_preserved() {
        let (streams, frames) = fixture();
        validate_streams(&serde_json::to_vec(&streams).unwrap(), &limits()).unwrap();
        let media = read(&streams, &frames, &limits()).unwrap();
        assert_eq!(
            media.video.time_base,
            TimeBase {
                numerator: 1,
                denominator: 90_000
            }
        );
        assert_eq!(
            media.video.frames,
            [
                FrameTiming {
                    pts: 0,
                    duration: 3000
                },
                FrameTiming {
                    pts: 3000,
                    duration: 4500
                },
                FrameTiming {
                    pts: 7500,
                    duration: 1500
                }
            ]
        );
        assert_eq!(media.video.frame_bytes().unwrap(), 4_587_520);
        let audio = media.audio.as_ref().expect("the strict profile has audio");
        assert_eq!((audio.sample_rate(), audio.channels()), (44_100, 2));
        assert_eq!(audio.samples(), 2);
        assert_eq!(audio.initial_pts(), 0);
    }

    #[test]
    fn streams_need_exact_types_indices_zero_starts_and_supported_sizes() {
        let (streams, frames) = fixture();
        for (index, name, value) in [
            (0, "start_pts", json!(1)),
            (1, "start_pts", json!(-1)),
            (0, "width", json!(0)),
            (0, "width", json!(1281)),
            (0, "height", json!(897)),
            (1, "channels", json!(1)),
            (1, "sample_rate", json!(0)),
            (1, "sample_rate", json!(192001)),
            (1, "sample_rate", json!(44100.5)),
            (1, "index", json!(0)),
            (0, "codec_type", json!("audio")),
            (1, "codec_type", json!("subtitle")),
            (0, "codec_name", json!("vp9")),
            (1, "codec_name", json!("opus")),
            (0, "time_base", json!("0/1")),
            (0, "time_base", json!("1/0")),
            (0, "time_base", json!("1.0/15")),
            (0, "time_base", json!("1/4294967296")),
            (1, "time_base", json!("0/1")),
            (1, "time_base", json!("1/0")),
        ] {
            let mut changed = streams.clone();
            changed["streams"][index][name] = value;
            assert!(
                read(&changed, &frames, &limits()).is_err(),
                "stream {index} {name}"
            );
            assert!(
                validate_streams(&serde_json::to_vec(&changed).unwrap(), &limits()).is_err(),
                "preflight stream {index} {name}"
            );
        }
        for index in [0, 1] {
            let mut changed = streams.clone();
            changed["streams"][index]
                .as_object_mut()
                .unwrap()
                .remove("start_pts");
            assert!(
                read(&changed, &frames, &limits()).is_err(),
                "unknown start is not zero"
            );
            let mut changed = streams.clone();
            changed["streams"][index]
                .as_object_mut()
                .unwrap()
                .remove("codec_name");
            assert!(
                validate_streams(&serde_json::to_vec(&changed).unwrap(), &limits()).is_err(),
                "the codec must be explicit before decoded-frame probing"
            );
        }
        let mut changed = streams.clone();
        changed["streams"].as_array_mut().unwrap().pop();
        assert!(read(&changed, &frames, &limits()).is_err());
        let mut changed = streams.clone();
        changed["streams"]
            .as_array_mut()
            .unwrap()
            .push(json!({"codec_type": "data"}));
        assert!(read(&changed, &frames, &limits()).is_err());
    }

    #[test]
    fn missing_conflicting_noninteger_and_nonmonotonic_frame_times_refuse() {
        let (streams, frames) = fixture();
        for (index, name, value) in [
            (0, "best_effort_timestamp", json!(1)),
            (0, "pkt_duration", json!(2999)),
            (1, "pts", json!(-1)),
            (1, "pts", json!(0)),
            (1, "pts", json!(9000)),
            (1, "pts", json!(3000.5)),
            (1, "pts", json!("N/A")),
            (1, "pkt_duration", json!(0)),
            (1, "pkt_duration", json!(-1)),
            (1, "pkt_duration", json!("1.5")),
            (1, "pkt_duration", Value::Null),
            (2, "best_effort_timestamp", json!(i64::MAX)),
            (2, "media_type", json!("audio")),
            (2, "stream_index", json!(1)),
            (2, "width", json!(1279)),
            (2, "height", json!(897)),
            (2, "height", Value::Null),
        ] {
            let mut changed = frames.clone();
            changed["frames"][index][name] = value;
            assert!(
                read(&streams, &changed, &limits()).is_err(),
                "frame {index} {name}"
            );
        }
        for names in [
            ["pts", "best_effort_timestamp"],
            ["duration", "pkt_duration"],
        ] {
            let mut changed = frames.clone();
            for name in names {
                changed["frames"][0].as_object_mut().unwrap().remove(name);
            }
            assert!(
                read(&streams, &changed, &limits()).is_err(),
                "missing timing cannot be inferred"
            );
        }
        let mut changed = frames.clone();
        changed["frames"][0]["pts"] = json!(1);
        changed["frames"][0]["best_effort_timestamp"] = json!(1);
        assert!(
            read(&streams, &changed, &limits()).is_err(),
            "first PTS must be zero"
        );
        let mut changed = frames.clone();
        changed["frames"][0]
            .as_object_mut()
            .unwrap()
            .remove("width");
        assert!(
            read(&streams, &changed, &limits()).is_err(),
            "frame dimensions cannot be assumed from the stream"
        );
    }

    #[test]
    fn document_frame_and_allocation_bounds_are_checked_before_use() {
        let (streams, frames) = fixture();
        let mut small = limits();
        small.max_frames = 2;
        assert!(read(&streams, &frames, &small).is_err());
        let mut small = limits();
        small.max_frame_bytes -= 1;
        assert!(read(&streams, &frames, &small).is_err());
        let mut small = limits();
        small.max_metadata_bytes = serde_json::to_vec(&streams).unwrap().len()
            + serde_json::to_vec(&frames).unwrap().len()
            - 1;
        assert!(read(&streams, &frames, &small).is_err());
        let mut small = limits();
        small.max_metadata_bytes = serde_json::to_vec(&streams).unwrap().len()
            + serde_json::to_vec(&frames).unwrap().len();
        assert!(
            read(&streams, &frames, &small).is_err(),
            "the audio document shares the aggregate metadata limit"
        );
        assert!(parse(b"{", b"{}", b"{}", &limits()).is_err());
        assert!(read(&streams, &json!({"frames": []}), &limits()).is_err());
        let huge = VideoMetadata {
            width: u32::MAX,
            height: u32::MAX,
            time_base: TimeBase {
                numerator: 1,
                denominator: 1,
            },
            frames: Vec::new(),
        };
        assert!(
            huge.frame_bytes().is_err(),
            "even dimensions representable individually can overflow RGBA storage"
        );
    }

    fn read_with(
        streams: &Value,
        frames: &Value,
        requirement: AudioRequirement,
    ) -> Result<MediaMetadata, MediaError> {
        parse_with(
            &serde_json::to_vec(streams).unwrap(),
            &serde_json::to_vec(frames).unwrap(),
            &serde_json::to_vec(
                &json!({"frames":[{"pts":0,"nb_samples":2,"stream_index":1,"channels":2}]}),
            )
            .unwrap(),
            &limits(),
            requirement,
        )
    }

    #[test]
    fn a_file_without_audio_needs_the_explicit_optional_requirement() {
        let (streams, frames) = fixture();
        let mut silent = streams.clone();
        silent["streams"].as_array_mut().unwrap().pop();
        // The strict entry points keep refusing it.
        assert!(read(&silent, &frames, &limits()).is_err());
        assert!(read_with(&silent, &frames, AudioRequirement::Required).is_err());
        assert!(validate_streams(&serde_json::to_vec(&silent).unwrap(), &limits()).is_err());
        // Optional: accepted, with no audio metadata and the source's own video timing.
        validate_streams_with(
            &serde_json::to_vec(&silent).unwrap(),
            &limits(),
            AudioRequirement::Optional,
        )
        .unwrap();
        let media = read_with(&silent, &frames, AudioRequirement::Optional).unwrap();
        assert!(media.audio.is_none());
        assert_eq!(media.video.frames.len(), 3);
        assert_eq!(media.video.frames[2].pts, 7500);
        // Nothing reads an audio document for it.
        parse_with(
            &serde_json::to_vec(&silent).unwrap(),
            &serde_json::to_vec(&frames).unwrap(),
            b"",
            &limits(),
            AudioRequirement::Optional,
        )
        .unwrap();
    }

    #[test]
    fn optional_audio_is_the_strict_profile_when_an_audio_stream_exists_and_nothing_looser() {
        let (streams, frames) = fixture();
        let strict = read(&streams, &frames, &limits()).unwrap();
        let optional = read_with(&streams, &frames, AudioRequirement::Optional).unwrap();
        assert_eq!(strict, optional);
        assert!(optional.audio.is_some());
        // Every strict refusal of the two-stream profile still refuses under Optional.
        for (index, name, value) in [
            (1, "start_pts", json!(1)),
            (1, "channels", json!(1)),
            (1, "codec_name", json!("opus")),
            (0, "codec_name", json!("vp9")),
            (0, "start_pts", json!(1)),
            (1, "index", json!(0)),
        ] {
            let mut changed = streams.clone();
            changed["streams"][index][name] = value;
            assert!(
                read_with(&changed, &frames, AudioRequirement::Optional).is_err(),
                "stream {index} {name}"
            );
        }
        // One stream is a silent movie only when it is the VP8 video; no other layout slips in.
        let mut audio_alone = streams.clone();
        audio_alone["streams"].as_array_mut().unwrap().remove(0);
        assert!(read_with(&audio_alone, &frames, AudioRequirement::Optional).is_err(), "audio alone");
        let mut vp9 = streams.clone();
        vp9["streams"].as_array_mut().unwrap().pop();
        vp9["streams"][0]["codec_name"] = json!("vp9");
        assert!(read_with(&vp9, &frames, AudioRequirement::Optional).is_err(), "silent but not VP8");
        let mut data = streams.clone();
        data["streams"][1] = json!({"index": 1, "codec_type": "data", "start_pts": 0});
        assert!(read_with(&data, &frames, AudioRequirement::Optional).is_err(), "video plus data");
        let mut lone_data = streams.clone();
        lone_data["streams"] = json!([{"index": 0, "codec_type": "data", "start_pts": 0}]);
        assert!(read_with(&lone_data, &frames, AudioRequirement::Optional).is_err());
        let mut three = streams.clone();
        three["streams"].as_array_mut().unwrap().push(json!({"index": 2, "codec_type": "data"}));
        assert!(read_with(&three, &frames, AudioRequirement::Optional).is_err());
        let mut none = streams.clone();
        none["streams"] = json!([]);
        assert!(read_with(&none, &frames, AudioRequirement::Optional).is_err());
    }
}
