//! The real output device, through cpal (ALSA on Linux; PipeWire and PulseAudio serve ALSA's
//! `default` device through their ALSA plugins, so this reaches them too).
//!
//! Kept thin on purpose: what can be tested without a sound card (the sample formats, the channel
//! mapping, chunked rendering) lives in [`crate::fill_device`], and what a card must be there for
//! cannot be tested on a machine without one. Nothing in the test suite ever opens this: tests use
//! fake openers, because on a desktop this would open the real device.
//!
//! cpal calls panic in a few places on resource exhaustion (a failed `pipe`, a thread that will not
//! start), so opening runs under `catch_unwind` and a panic is just another way to fail to get a
//! device.

use crate::convert::{fill_device, DeviceSample};
use crate::engine::{DeviceError, DeviceLink, DeviceOpener, LostReporter, OpenedDevice};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

/// Frames of scratch the data callback renders into at a time; a bigger callback is filled in
/// several chunks, so the callback never allocates.
const SCRATCH_FRAMES: usize = 4096;

/// Opens the default output device of the default cpal host.
pub struct CpalOpener {
    timeout: Duration,
}

impl CpalOpener {
    pub fn new() -> Self {
        Self { timeout: Duration::from_secs(3) }
    }
}

impl Default for CpalOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOpener for CpalOpener {
    fn open(self: Box<Self>, link: DeviceLink) -> Result<OpenedDevice, DeviceError> {
        let timeout = self.timeout;
        catch_unwind(AssertUnwindSafe(|| open_default(&link, timeout)))
            .unwrap_or_else(|_| Err(DeviceError::Backend("the audio backend panicked while opening the device".into())))
    }
}

/// Whether an error reported by a running stream means the stream is finished. A glitch (`Xrun`),
/// a re-route to another default device, a refusal of real-time priority and a busy device are
/// not; everything else (and any kind cpal adds later) is treated as the end of the stream, which
/// costs a fall back to silence at worst.
pub(crate) fn is_fatal(kind: ErrorKind) -> bool {
    !matches!(kind, ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied | ErrorKind::DeviceBusy)
}

fn from_cpal(error: cpal::Error) -> DeviceError {
    match error.kind() {
        ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => DeviceError::NoDevice,
        ErrorKind::UnsupportedConfig | ErrorKind::UnsupportedOperation => DeviceError::Unsupported(error.to_string()),
        _ => DeviceError::Backend(error.to_string()),
    }
}

/// The sample formats [`build`] can write.
fn writable(format: SampleFormat) -> bool {
    matches!(format, SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16 | SampleFormat::I32)
}

/// The device's default configuration, or (when its default format is one we do not write, such
/// as 24-bit) the first configuration in a format we do at the same rate.
fn choose(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, DeviceError> {
    let default = device.default_output_config().map_err(from_cpal)?;
    if writable(default.sample_format()) {
        return Ok(default);
    }
    let rate = default.sample_rate();
    device
        .supported_output_configs()
        .map_err(from_cpal)?
        .filter(|range| writable(range.sample_format()))
        .find_map(|range| range.try_with_sample_rate(rate))
        .ok_or_else(|| DeviceError::Unsupported(format!("sample format {:?}", default.sample_format())))
}

/// Keep libasound's own complaints off the terminal. By default it writes them to stderr itself, and
/// on a machine without a sound card it prints eight lines (`cannot find card '0'`, `Unknown PCM
/// default`, ...) before cpal reports that there is no device, which would bury the engine's one note
/// about going silent. The handler is per thread: it covers the thread that opens the device (the
/// calls that fail without a card all run there) and goes away with it. What it catches is dropped:
/// the note says what went wrong, and a real device that fails says so through cpal's own error.
#[cfg(target_os = "linux")]
fn quiet_alsa() {
    let _ = alsa::Output::local_error_handler();
}

#[cfg(not(target_os = "linux"))]
fn quiet_alsa() {}

fn open_default(link: &DeviceLink, timeout: Duration) -> Result<OpenedDevice, DeviceError> {
    quiet_alsa();
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(DeviceError::NoDevice)?;
    let supported = choose(&device)?;
    let (rate, channels) = (supported.sample_rate(), supported.channels());
    let config = supported.config();
    // The mixer has to know the rate before the first callback.
    link.mixer().set_output_rate(rate);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, link, timeout)?,
        SampleFormat::I16 => build::<i16>(&device, config, link, timeout)?,
        SampleFormat::U16 => build::<u16>(&device, config, link, timeout)?,
        SampleFormat::I32 => build::<i32>(&device, config, link, timeout)?,
        other => return Err(DeviceError::Unsupported(format!("sample format {other:?}"))),
    };
    stream.play().map_err(from_cpal)?;
    Ok(OpenedDevice { name: device.to_string(), sample_rate: rate, channels, guard: Box::new(stream) })
}

fn build<T: cpal::SizedSample + DeviceSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    link: &DeviceLink,
    timeout: Duration,
) -> Result<cpal::Stream, DeviceError> {
    let channels = usize::from(config.channels);
    let mixer = link.mixer();
    let lost: LostReporter = link.lost_reporter();
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * 2];
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                fill_device(data, channels, &mut scratch, |stereo| mixer.render_stereo(stereo));
            },
            move |error: cpal::Error| {
                if is_fatal(error.kind()) {
                    lost.report();
                }
            },
            Some(timeout),
        )
        .map_err(from_cpal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_end_of_a_stream_is_fatal() {
        for harmless in [ErrorKind::Xrun, ErrorKind::DeviceChanged, ErrorKind::RealtimeDenied, ErrorKind::DeviceBusy] {
            assert!(!is_fatal(harmless), "{harmless:?}");
        }
        for fatal in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::HostUnavailable,
            ErrorKind::BackendError,
            ErrorKind::PermissionDenied,
            ErrorKind::Other,
        ] {
            assert!(is_fatal(fatal), "{fatal:?}");
        }
    }

    #[test]
    fn cpal_errors_map_onto_the_engines_own() {
        let no_device = from_cpal(cpal::Error::new(ErrorKind::DeviceNotAvailable));
        assert_eq!(no_device, DeviceError::NoDevice);
        assert_eq!(from_cpal(cpal::Error::new(ErrorKind::HostUnavailable)), DeviceError::NoDevice);
        assert!(matches!(from_cpal(cpal::Error::new(ErrorKind::UnsupportedConfig)), DeviceError::Unsupported(_)));
        let other = from_cpal(cpal::Error::with_message(ErrorKind::BackendError, "alsa said no"));
        assert_eq!(other, DeviceError::Backend("alsa said no".into()));
    }

    #[test]
    fn the_formats_written_are_the_ones_a_desktop_card_offers() {
        for format in [SampleFormat::F32, SampleFormat::I16, SampleFormat::U16, SampleFormat::I32] {
            assert!(writable(format));
        }
        for format in [SampleFormat::F64, SampleFormat::I24, SampleFormat::U8, SampleFormat::I8] {
            assert!(!writable(format));
        }
    }
}
