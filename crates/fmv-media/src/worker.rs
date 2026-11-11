use crate::{metadata, process, FrameTiming, Limits, MediaError, MediaMetadata};
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Tools {
    /// Explicit local executable paths. No download, PATH search or shell invocation.
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub tools: Tools,
    /// A local regular file, opened read-only by the decoder. URLs are not accepted.
    pub path: PathBuf,
    pub limits: Limits,
}

#[derive(Debug)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    /// Interleaved stereo signed 16-bit little-endian PCM, no synthetic loop region.
    /// Retaining bytes avoids an extra full-sized allocation/conversion on the worker.
    /// Audio frame PTS continuity is not yet validated: this payload alone must not
    /// be promoted to an authoritative movie playback clock.
    pub pcm_le: Vec<u8>,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.pcm_le.len() / 4
    }
}

#[derive(Debug)]
pub struct DecodedFrame {
    pub ordinal: usize,
    pub timing: FrameTiming,
    /// Exactly width * height * 4 bytes. This is decoded RGBA, not an encoded asset.
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub enum Event {
    Prepared {
        metadata: Arc<MediaMetadata>,
        audio: DecodedAudio,
    },
    Frame(DecodedFrame),
    /// Successful process exit AND exact decoded frame-count agreement. The consumer
    /// must separately finish presenting these frames/audio; this does not end MOVIE.
    DecodedEof {
        frames: usize,
    },
}

/// Dedicated movie worker; it cannot occupy the soundtrack's urgent decode queue.
/// Dropping it cancels/reaps only its own current child and joins its worker thread.
pub struct MovieWorker {
    events: Receiver<Event>,
    result: Receiver<Result<(), MediaError>>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    finished: bool,
}

impl MovieWorker {
    pub fn start(request: Request) -> Result<Self, MediaError> {
        request.limits.validate()?;
        for path in [&request.path, &request.tools.ffmpeg, &request.tools.ffprobe] {
            if !path.is_absolute() {
                return Err(MediaError::Invalid(
                    "movie and tool paths must be absolute".into(),
                ));
            }
        }
        let (send, events) = mpsc::sync_channel(request.limits.queued_frames);
        let (done, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("fmv-movie-decode".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    decode(request, &send, signal)
                }))
                .unwrap_or(Err(MediaError::WorkerPanic));
                // The result channel has one small message and cannot be held behind a full
                // frame queue. Failed decoding never emits a successful DecodedEof.
                let _ = done.send(result);
            })?;
        Ok(Self {
            events,
            result,
            cancel,
            thread: Some(thread),
            finished: false,
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// Nonblocking. A successful end is the explicit DecodedEof event; None also means
    /// the decoder has not produced another frame yet, so must not be treated as EOF.
    pub fn poll(&mut self) -> Result<Option<Event>, MediaError> {
        match self.events.try_recv() {
            Ok(event) => return Ok(Some(event)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        if !self.finished {
            match self.result.try_recv() {
                Ok(result) => {
                    self.finished = true;
                    result?;
                }
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    return Err(MediaError::WorkerPanic);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        Ok(None)
    }
}

impl Drop for MovieWorker {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn emit(
    send: &SyncSender<Event>,
    mut event: Event,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<(), MediaError> {
    loop {
        process::check(cancel, deadline)?;
        match send.try_send(event) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(MediaError::Cancelled),
            Err(TrySendError::Full(back)) => event = back,
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn words(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn decode(
    request: Request,
    send: &SyncSender<Event>,
    cancel: Arc<AtomicBool>,
) -> Result<(), MediaError> {
    let limits = &request.limits;
    let deadline = Instant::now()
        .checked_add(limits.lifetime)
        .ok_or(MediaError::Invalid("lifetime overflow".into()))?;
    process::check(&cancel, deadline)?;
    let file = std::fs::metadata(&request.path)?;
    if !file.is_file() || file.len() == 0 || file.len() > limits.max_media_bytes {
        return Err(MediaError::Invalid(
            "movie must be a bounded nonempty regular file".into(),
        ));
    }
    for tool in [&request.tools.ffmpeg, &request.tools.ffprobe] {
        if !std::fs::metadata(tool)?.is_file() {
            return Err(MediaError::Invalid(
                "decoder tool is not a regular file".into(),
            ));
        }
    }
    let path = request.path.canonicalize()?;
    let mut streams = words(&[
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_streams",
        "-show_entries",
        "stream=index,codec_name,codec_type,width,height,time_base,sample_rate,channels,start_pts",
        "-of",
        "json",
    ]);
    streams.push(path.as_os_str().into());
    let streams = process::capture(
        &request.tools.ffprobe,
        &streams,
        limits.max_metadata_bytes,
        limits.max_stderr_bytes,
        Arc::clone(&cancel),
        deadline,
    )?;
    metadata::validate_streams(&streams, limits)?;
    let mut frames = words(&[
        "-v",
        "error",
        "-threads",
        "2",
        "-protocol_whitelist",
        "file,pipe",
        "-select_streams",
        "v:0",
        "-show_frames",
        "-show_entries",
        "frame=pts,best_effort_timestamp,duration,pkt_duration,width,height",
        "-of",
        "json",
    ]);
    frames.push(path.as_os_str().into());
    let frames = process::capture(
        &request.tools.ffprobe,
        &frames,
        limits.max_metadata_bytes,
        limits.max_stderr_bytes,
        Arc::clone(&cancel),
        deadline,
    )?;
    let metadata = Arc::new(metadata::parse(&streams, &frames, limits)?);
    drop(streams);
    drop(frames);
    let mut audio = words(&[
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-err_detect",
        "explode",
        "-threads",
        "2",
        "-protocol_whitelist",
        "file,pipe",
        "-i",
    ]);
    audio.push(path.as_os_str().into());
    audio.extend(words(&[
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", "2", "-ar",
    ]));
    audio.push(metadata.audio.sample_rate.to_string().into());
    audio.extend(words(&["-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"]));
    let pcm_le = process::capture(
        &request.tools.ffmpeg,
        &audio,
        limits.max_audio_bytes,
        limits.max_stderr_bytes,
        Arc::clone(&cancel),
        deadline,
    )?;
    if pcm_le.is_empty() || !pcm_le.len().is_multiple_of(4) {
        return Err(MediaError::Invalid(
            "decoded stereo PCM is empty or has a partial sample frame".into(),
        ));
    }
    emit(
        send,
        Event::Prepared {
            metadata: Arc::clone(&metadata),
            audio: DecodedAudio {
                sample_rate: metadata.audio.sample_rate,
                pcm_le,
            },
        },
        &cancel,
        deadline,
    )?;
    let mut video = words(&[
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-err_detect",
        "explode",
        "-threads",
        "2",
        "-noautorotate",
        "-protocol_whitelist",
        "file,pipe",
        "-i",
    ]);
    video.push(path.as_os_str().into());
    video.extend(words(&[
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-noautoscale",
        "-fps_mode",
        "passthrough",
        "-threads",
        "2",
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "pipe:1",
    ]));
    let count = process::run(
        &request.tools.ffmpeg,
        &video,
        limits.max_stderr_bytes,
        Arc::clone(&cancel),
        deadline,
        |stdout| {
            read_frames(stdout, &metadata, |frame| {
                emit(send, Event::Frame(frame), &cancel, deadline)
            })
        },
    )?;
    emit(send, Event::DecodedEof { frames: count }, &cancel, deadline)
}

fn read_frames(
    input: &mut impl Read,
    metadata: &MediaMetadata,
    mut emit: impl FnMut(DecodedFrame) -> Result<(), MediaError>,
) -> Result<usize, MediaError> {
    let bytes = metadata.video.frame_bytes()?;
    for (ordinal, timing) in metadata.video.frames.iter().enumerate() {
        let mut rgba = vec![0; bytes];
        input.read_exact(&mut rgba).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => MediaError::Invalid(format!(
                "decoder stopped in frame {ordinal}; EOF is not complete"
            )),
            _ => e.into(),
        })?;
        emit(DecodedFrame {
            ordinal,
            timing: *timing,
            rgba,
        })?;
    }
    let mut extra = [0u8; 1];
    loop {
        match input.read(&mut extra) {
            Ok(0) => return Ok(metadata.video.frames.len()),
            Ok(_) => {
                return Err(MediaError::Invalid(
                    "decoder produced more pixels than the timestamp inventory".into(),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioMetadata, TimeBase, VideoMetadata};

    fn metadata() -> MediaMetadata {
        MediaMetadata {
            video: VideoMetadata {
                width: 1,
                height: 1,
                time_base: TimeBase {
                    numerator: 1,
                    denominator: 1000,
                },
                frames: vec![
                    FrameTiming {
                        pts: 0,
                        duration: 33,
                    },
                    FrameTiming {
                        pts: 37,
                        duration: 51,
                    },
                ],
            },
            audio: AudioMetadata {
                sample_rate: 22_050,
                channels: 2,
            },
        }
    }

    #[test]
    fn pixels_keep_exact_decoded_timestamps_and_partial_or_extra_frames_refuse() {
        let data = metadata();
        let mut seen = Vec::new();
        assert_eq!(
            read_frames(&mut &b"abcdefgh"[..], &data, |frame| {
                seen.push(frame);
                Ok(())
            }),
            Ok(2)
        );
        assert_eq!(
            seen.iter()
                .map(|f| (f.ordinal, f.timing.pts))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 37)]
        );
        assert!(read_frames(&mut &b"abcdefg"[..], &data, |_| Ok(())).is_err());
        assert!(read_frames(&mut &b"abcdefghi"[..], &data, |_| Ok(())).is_err());
    }

    #[test]
    fn cancellation_interrupts_a_full_frame_queue() {
        let (send, _receive) = mpsc::sync_channel(1);
        send.send(Event::DecodedEof { frames: 0 })
            .expect("fixture queue");
        let cancel = AtomicBool::new(true);
        assert_eq!(
            emit(
                &send,
                Event::DecodedEof { frames: 2 },
                &cancel,
                Instant::now() + Duration::from_secs(1)
            ),
            Err(MediaError::Cancelled)
        );
    }
}
