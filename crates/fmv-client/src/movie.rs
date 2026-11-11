//! The movie controller. The windowed client registers it for a live field's movies (`crate::live_movie`); the
//! script VM and the headless client do not.
//!
//! Loading-stage `prepare` verifies an explicitly admitted immutable asset. Frame
//! polling never hashes a file or runs a decoder synchronously. Decode read-ahead
//! is bounded, as are retained frames and outstanding GPU presentation tickets.
//! `play` is separate and requires the first decoded frame to be available.
//!
//! During playback the clock is the exact finite voice's consumed source frames,
//! not wall time, decode speed or hardware-audible time. Only legitimate AudioEnded
//! can start the named, pause-aware video tail. EOF additionally needs decoder EOF,
//! an acknowledged final presentation and its complete authored duration.
//!
//! Drawing submits GPU work. A caller must separately acknowledge its actual
//! presentation boundary (offscreen tests use completed readback). Display policy,
//! field overlays, native camera alignment and script registration remain explicit
//! caller decisions. No original-PC parity is claimed by this module.

mod clock;
mod policy;
#[cfg(test)]
pub(crate) mod tests;

use crate::render::{
    movie::{DisplayPolicy, MovieFrameId, MovieSubmission, MovieViewport},
    Gpu,
};
pub use clock::PresentationTime;
use clock::VideoTailClock;
use fmv_audio::{AudioEngine, MovieAudioState, MovieAudioStatus, MovieHandle, MoviePcm};
use fmv_media::{
    DecodedFrame, Event, MediaMetadata, MovieWorker, PlaybackTimeline, Request, VideoMetadata,
};
pub use policy::Admission;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

/// Bounded per-frame work, independent of how far the decoder has read ahead.
const EVENTS_PER_POLL: usize = 8;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum ControllerError {
    Admission(&'static str),
    Clock(&'static str),
    State(&'static str),
    Io(std::io::Error),
    Media(fmv_media::MediaError),
    Audio(fmv_audio::MovieAudioError),
    Gpu(crate::render::movie::MovieError),
    PresentationBusy,
}
impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "movie controller: {self:?}")
    }
}
impl std::error::Error for ControllerError {}
impl From<fmv_media::MediaError> for ControllerError {
    fn from(e: fmv_media::MediaError) -> Self {
        Self::Media(e)
    }
}
impl From<fmv_audio::MovieAudioError> for ControllerError {
    fn from(e: fmv_audio::MovieAudioError) -> Self {
        Self::Audio(e)
    }
}
impl From<crate::render::movie::MovieError> for ControllerError {
    fn from(e: crate::render::movie::MovieError) -> Self {
        Self::Gpu(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackId {
    pub stream: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Prepared,
    Playing,
    Paused,
    VideoTail,
    Completed,
    Cancelled,
    Faulted,
}

/// Only an explicit FFNx-compatible draw observable is available. The original
/// PC graph counter is a different contract and is not silently emulated here.
#[derive(Debug, Clone, Copy)]
pub enum MviefPolicy {
    FfnxDrawnPts {
        fps_numerator: u32,
        fps_denominator: u32,
    },
}

/// A bounded pending draw receipt. It is not proof of display until the caller
/// returns it through acknowledge_presented after its own presentation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTicket {
    pub id: PlaybackId,
    pub submission: MovieSubmission,
    serial: u64,
}

pub struct MovieController {
    id: PlaybackId,
    admission: Admission,
    worker: MovieWorker,
    phase: Phase,
    paused: bool,
    metadata: Option<Arc<MediaMetadata>>,
    video: Option<Arc<VideoMetadata>>,
    timeline: Option<PlaybackTimeline>,
    pcm: Option<MoviePcm>,
    handle: Option<MovieHandle>,
    /// Own the exact cancellation route, including on drop or caller replacement.
    engine: Option<Arc<AudioEngine>>,
    audio_frames: u64,
    audio_rate: u32,
    consumed: u64,
    audio_ended: bool,
    endpoint: PresentationTime,
    position: PresentationTime,
    tail: Option<VideoTailClock>,
    last_now: Option<Duration>,
    next_decoded: usize,
    decoder_eof: bool,
    first_frame: bool,
    /// Latest decoded frame due now and first future frame. Never more than two.
    due: Option<DecodedFrame>,
    ahead: Option<DecodedFrame>,
    uploaded: Option<MovieFrameId>,
    last_drawn: Option<MovieFrameId>,
    pending_ticket: Option<PresentationTicket>,
    next_ticket: u64,
}

impl MovieController {
    /// Synchronous bounded asset preflight followed by asynchronous decode. Call
    /// from a loading stage/worker, not the normal frame path. Caller-provided
    /// paths identify both tools and immutable owned input; nothing is installed.
    pub fn prepare(
        stream: u64,
        mut request: Request,
        admission: Admission,
    ) -> Result<Self, ControllerError> {
        policy::preflight(&mut request, &admission)?;
        let generation = NEXT_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| ControllerError::State("movie generations exhausted"))?;
        let worker = MovieWorker::start(request)?;
        Ok(Self {
            id: PlaybackId { stream, generation },
            admission,
            worker,
            phase: Phase::Preparing,
            paused: false,
            metadata: None,
            video: None,
            timeline: None,
            pcm: None,
            handle: None,
            engine: None,
            audio_frames: 0,
            audio_rate: 0,
            consumed: 0,
            audio_ended: false,
            endpoint: PresentationTime::ZERO,
            position: PresentationTime::ZERO,
            tail: None,
            last_now: None,
            next_decoded: 0,
            decoder_eof: false,
            first_frame: false,
            due: None,
            ahead: None,
            uploaded: None,
            last_drawn: None,
            pending_ticket: None,
            next_ticket: 1,
        })
    }

    pub fn id(&self) -> PlaybackId {
        self.id
    }
    pub fn phase(&self) -> Phase {
        if self.paused && matches!(self.phase, Phase::Playing | Phase::VideoTail) {
            Phase::Paused
        } else {
            self.phase
        }
    }
    pub fn position(&self) -> PresentationTime {
        self.position
    }
    pub fn metadata(&self) -> Option<&MediaMetadata> {
        self.metadata.as_deref()
    }
    pub fn audio_handle(&self) -> Option<&MovieHandle> {
        self.handle.as_ref()
    }
    pub fn pending_frames(&self) -> usize {
        usize::from(self.due.is_some()) + usize::from(self.ahead.is_some())
    }
    pub fn first_frame_available(&self) -> bool {
        self.first_frame
    }

    fn check_now(&self, now: Duration) -> Result<(), ControllerError> {
        if self.last_now.is_some_and(|previous| now < previous) {
            return Err(ControllerError::Clock("monotonic clock moved backwards"));
        }
        Ok(())
    }
    fn terminal(&self) -> bool {
        matches!(
            self.phase,
            Phase::Completed | Phase::Cancelled | Phase::Faulted
        )
    }

    /// Nonblocking, bounded source polling. A stalled mixer freezes video. The
    /// decoder's bounded queue then applies backpressure rather than growing RAM.
    pub fn poll(&mut self, now: Duration) -> Result<(), ControllerError> {
        self.check_now(now)?;
        if self.terminal() {
            return Ok(());
        }
        let result = self.poll_inner(now);
        if result.is_err() {
            self.fail();
        }
        result
    }

    fn poll_inner(&mut self, now: Duration) -> Result<(), ControllerError> {
        // A full/future frame queue must not hide failure on the worker's
        // independent result channel. Successful status is never decoded EOF.
        self.worker.poll_status()?;
        if let Some(handle) = self.handle.clone() {
            self.accept_audio_status(self.id, &handle, handle.status(), now)?;
        }
        self.last_now = Some(now);
        let target = self.target()?;
        if self
            .ahead
            .as_ref()
            .is_some_and(|frame| frame.ordinal <= target)
        {
            self.due = self.ahead.take();
        }
        for _ in 0..EVENTS_PER_POLL {
            if self.ahead.is_some() || self.decoder_eof {
                break;
            }
            let Some(event) = self.worker.poll()? else {
                break;
            };
            self.accept_event(event)?;
        }
        self.update_completion()?;
        Ok(())
    }

    fn target(&self) -> Result<usize, ControllerError> {
        self.timeline.as_ref().map_or(Ok(0), |timeline| {
            timeline
                .target_ordinal_ratio(self.position.numerator, self.position.denominator)
                .map_err(Into::into)
        })
    }

    fn accept_event(&mut self, event: Event) -> Result<(), ControllerError> {
        match event {
            Event::Prepared { metadata, audio } => {
                if self.metadata.is_some() {
                    return Err(ControllerError::State("duplicate prepared event"));
                }
                policy::admit(&self.admission, &metadata, &audio)?;
                let video = Arc::new(metadata.video.clone());
                let timeline = PlaybackTimeline::new(video.clone())?;
                let endpoint = PresentationTime::endpoint(&video)?;
                let pcm = MoviePcm::from_s16le(audio.sample_rate, audio.pcm_le)?;
                let audio_end = PresentationTime::samples(pcm.frames(), pcm.sample_rate());
                if audio_end.numerator > 600 * audio_end.denominator {
                    return Err(ControllerError::Clock(
                        "audio exceeds the 600 second controller policy",
                    ));
                }
                // No broad wall-clock fallback: even an admitted source may have
                // at most one second of explicit video-only tail.
                let maximum = PresentationTime {
                    numerator: audio_end.numerator + audio_end.denominator,
                    denominator: audio_end.denominator,
                };
                if !maximum.at_least(endpoint) {
                    return Err(ControllerError::Clock("video tail exceeds one second"));
                }
                self.audio_frames = pcm.frames();
                self.audio_rate = pcm.sample_rate();
                self.endpoint = endpoint;
                self.pcm = Some(pcm);
                self.video = Some(video);
                self.timeline = Some(timeline);
                self.metadata = Some(metadata);
            }
            Event::Frame(frame) => {
                let video = self
                    .video
                    .as_ref()
                    .ok_or(ControllerError::State("frame before metadata"))?;
                if frame.ordinal != self.next_decoded
                    || video.frames.get(frame.ordinal) != Some(&frame.timing)
                    || frame.rgba.len() != video.frame_bytes()?
                    || self.decoder_eof
                {
                    return Err(ControllerError::State(
                        "decoder frame identity or layout changed",
                    ));
                }
                let target = self.target()?;
                if frame.ordinal <= target {
                    self.due = Some(frame);
                } else {
                    if self.ahead.is_some() {
                        return Err(ControllerError::State(
                            "decoder exceeded pending-frame bound",
                        ));
                    }
                    self.ahead = Some(frame);
                }
                self.next_decoded += 1;
                if self.next_decoded == 1 {
                    self.first_frame = true;
                    self.phase = Phase::Prepared;
                }
            }
            Event::DecodedEof { frames } => {
                if self.decoder_eof || frames != self.next_decoded {
                    return Err(ControllerError::State(
                        "decoder EOF before its exact frame inventory",
                    ));
                }
                self.timeline
                    .as_mut()
                    .ok_or(ControllerError::State("EOF before metadata"))?
                    .mark_decoded_eof(frames)?;
                self.decoder_eof = true;
            }
        }
        Ok(())
    }

    /// Start only after preparation has supplied frame zero. A caller may first
    /// upload/draw/acknowledge that frame if its chosen startup policy requires it.
    /// Audio Off refuses and leaves the prepared controller intact.
    pub fn play(
        &mut self,
        engine: &Arc<AudioEngine>,
        now: Duration,
        gain: f32,
    ) -> Result<(), ControllerError> {
        self.check_now(now)?;
        if self.phase != Phase::Prepared || !self.first_frame {
            return Err(ControllerError::State("first frame is not prepared"));
        }
        if let Err(error) = self.worker.poll_status() {
            self.fail();
            return Err(error.into());
        }
        let pcm = self
            .pcm
            .as_ref()
            .ok_or(ControllerError::State("prepared PCM missing"))?;
        let handle = engine.play_movie(pcm.clone(), gain)?;
        self.pcm = None;
        self.handle = Some(handle);
        self.engine = Some(Arc::clone(engine));
        self.last_now = Some(now);
        self.phase = Phase::Playing;
        Ok(())
    }

    /// Validate identity and the complete coherent audio status before mutation.
    /// The public path only reads its own handle; this boundary also documents why
    /// a stale asynchronous response must not retime a replacement movie.
    fn accept_audio_status(
        &mut self,
        id: PlaybackId,
        handle: &MovieHandle,
        status: MovieAudioStatus,
        now: Duration,
    ) -> Result<bool, ControllerError> {
        if id != self.id || self.handle.as_ref() != Some(handle) || self.terminal() {
            return Ok(false);
        }
        self.check_now(now)?;
        if status.source_rate != self.audio_rate
            || status.total_frames != self.audio_frames
            || status.consumed_frames > self.audio_frames
            || status.consumed_frames < self.consumed
            || (status.state == MovieAudioState::Queued && status.consumed_frames != 0)
            || (status.state == MovieAudioState::Ended
                && status.consumed_frames != self.audio_frames)
            || (self.audio_ended && status.state != MovieAudioState::Ended)
        {
            return Err(ControllerError::Clock(
                "audio status does not match the exact finite voice",
            ));
        }
        if matches!(
            status.state,
            MovieAudioState::Cancelled | MovieAudioState::Replaced | MovieAudioState::Faulted
        ) {
            return Err(ControllerError::Clock(
                "movie voice terminated without natural EOF",
            ));
        }
        self.consumed = status.consumed_frames;
        if status.state == MovieAudioState::Ended {
            self.audio_ended = true;
        }
        if let Some(tail) = self.tail.as_mut() {
            tail.advance(now, self.paused);
            self.position = tail.position(self.audio_frames, self.audio_rate, self.endpoint);
        } else if !self.paused {
            self.position = PresentationTime::samples(status.consumed_frames, self.audio_rate);
            if self.audio_ended && !self.position.at_least(self.endpoint) {
                self.tail = Some(VideoTailClock {
                    elapsed: Duration::ZERO,
                    observed_at: now,
                });
                self.phase = Phase::VideoTail;
            }
        }
        self.last_now = Some(now);
        Ok(true)
    }

    pub fn set_paused(&mut self, now: Duration, paused: bool) -> Result<(), ControllerError> {
        if !matches!(self.phase, Phase::Playing | Phase::VideoTail) {
            return Err(ControllerError::State("movie is not playing"));
        }
        // Account for the interval before changing the pause state. While paused,
        // poll keeps moving only the tail's wall-clock anchor, never its elapsed time.
        self.poll(now)?;
        if self.terminal() {
            return Ok(());
        }
        self.paused = paused;
        if let (Some(handle), Some(engine)) = (&self.handle, &self.engine) {
            engine.pause_movie(handle, paused);
        }
        Ok(())
    }

    pub fn cancel(&mut self) {
        if self.terminal() {
            return;
        }
        self.worker.cancel();
        if let (Some(handle), Some(engine)) = (&self.handle, &self.engine) {
            engine.cancel_movie(handle);
        }
        self.phase = Phase::Cancelled;
        self.release_pending();
    }
    fn fail(&mut self) {
        self.worker.cancel();
        if let (Some(handle), Some(engine)) = (&self.handle, &self.engine) {
            engine.cancel_movie(handle);
        }
        self.phase = Phase::Faulted;
        self.release_pending();
    }
    fn release_pending(&mut self) {
        self.due = None;
        self.ahead = None;
        self.pcm = None;
        self.pending_ticket = None;
    }

    /// Explicit GPU replacement. Generation is globally unique; the caller's
    /// stream identifier may be reused without admitting old frame receipts.
    pub fn bind_gpu(&self, gpu: &mut Gpu) {
        gpu.replace_movie_stream(self.id.generation);
    }

    /// Upload the latest due frame. GPU UploadBusy leaves the frame pending and
    /// must be retried; it is not a controller fault. No future frame is uploaded.
    pub fn upload_due(&mut self, gpu: &mut Gpu) -> Result<Option<MovieFrameId>, ControllerError> {
        if matches!(self.phase, Phase::Cancelled | Phase::Faulted) {
            return Err(ControllerError::State("movie terminated"));
        }
        if self.pending_ticket.is_some() {
            return Err(ControllerError::PresentationBusy);
        }
        let Some(frame) = &self.due else {
            return Ok(None);
        };
        let video = self
            .video
            .as_ref()
            .ok_or(ControllerError::State("no video metadata"))?;
        let id = gpu.upload_movie_frame(self.id.generation, video, frame)?;
        self.due = None;
        self.uploaded = Some(id);
        Ok(Some(id))
    }

    /// Submit the explicit movie base pass. Draw field overlays/UI afterwards.
    /// There is one outstanding receipt; acknowledge or discard before another.
    pub fn draw_base(
        &mut self,
        gpu: &mut Gpu,
        target: &wgpu::TextureView,
        size: (u32, u32),
        viewport: MovieViewport,
        policy: DisplayPolicy,
    ) -> Result<PresentationTicket, ControllerError> {
        if matches!(self.phase, Phase::Cancelled | Phase::Faulted) {
            return Err(ControllerError::State("movie terminated"));
        }
        if self.pending_ticket.is_some() {
            return Err(ControllerError::PresentationBusy);
        }
        let uploaded = self
            .uploaded
            .ok_or(ControllerError::State("no movie frame uploaded"))?;
        if gpu.movie_state().frame != Some(uploaded) {
            return Err(ControllerError::State("GPU belongs to another movie/frame"));
        }
        let next = self
            .next_ticket
            .checked_add(1)
            .ok_or(ControllerError::State("presentation tickets exhausted"))?;
        let submission = gpu.draw_movie_base_in(target, size, viewport, policy)?;
        let ticket = PresentationTicket {
            id: self.id,
            submission,
            serial: self.next_ticket,
        };
        self.next_ticket = next;
        self.last_drawn = Some(submission.frame);
        self.pending_ticket = Some(ticket);
        Ok(ticket)
    }

    pub fn acknowledge_presented(
        &mut self,
        ticket: PresentationTicket,
    ) -> Result<bool, ControllerError> {
        if matches!(self.phase, Phase::Cancelled | Phase::Faulted)
            || ticket.id != self.id
            || self.pending_ticket != Some(ticket)
        {
            return Ok(false);
        }
        self.timeline
            .as_mut()
            .ok_or(ControllerError::State("no timeline"))?
            .mark_presented(ticket.submission.frame.ordinal)?;
        self.pending_ticket = None;
        self.update_completion()?;
        Ok(true)
    }
    /// A dropped/occluded surface draw is not a presentation acknowledgment.
    pub fn discard_submission(&mut self, ticket: PresentationTicket) -> bool {
        if ticket.id != self.id || self.pending_ticket != Some(ticket) {
            return false;
        }
        self.pending_ticket = None;
        true
    }
    fn update_completion(&mut self) -> Result<(), ControllerError> {
        if self.audio_ended && self.decoder_eof {
            if let Some(timeline) = &self.timeline {
                if timeline
                    .presented_eof_ratio(self.position.numerator, self.position.denominator)?
                {
                    self.phase = Phase::Completed;
                }
            }
        }
        Ok(())
    }

    /// Pinned FFNx software path: round(last drawn PTS * FPS) + 1. This uses
    /// GPU draw submission, not decoded ordinal, hardware display or the original
    /// PC graph's frame counter. No counter exists before a successful draw.
    pub fn mvief(&self, policy: MviefPolicy) -> Result<Option<u64>, ControllerError> {
        let MviefPolicy::FfnxDrawnPts {
            fps_numerator,
            fps_denominator,
        } = policy;
        if fps_numerator == 0 || fps_denominator == 0 {
            return Err(ControllerError::Clock("invalid explicit MVIEF frame rate"));
        }
        let Some(frame) = self.last_drawn else {
            return Ok(None);
        };
        let numerator = (frame.timing.pts as u128)
            .checked_mul(u128::from(frame.time_base.numerator))
            .and_then(|n| n.checked_mul(u128::from(fps_numerator)))
            .ok_or(ControllerError::Clock("MVIEF numerator overflow"))?;
        let denominator = u128::from(frame.time_base.denominator) * u128::from(fps_denominator);
        let rounded = numerator / denominator
            + u128::from(numerator % denominator >= denominator.div_ceil(2));
        let counter = rounded
            .checked_add(1)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(ControllerError::Clock("MVIEF counter overflow"))?;
        Ok(Some(counter))
    }
}

// Explicit cancel and Drop share the exact retained voice identity. Engine commands
// are nonblocking; MovieWorker's subsequent Drop cancels and joins its own decoder.
// Teardown therefore belongs to the owning lifecycle/loading stage, not a GPU pass.
impl Drop for MovieController {
    fn drop(&mut self) {
        self.worker.cancel();
        if let (Some(handle), Some(engine)) = (&self.handle, &self.engine) {
            engine.cancel_movie(handle);
        }
    }
}
