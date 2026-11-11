//! Movies a live field's script plays (`ServerMsg::LiveMovie`) in the windowed client.
//!
//! [`MovieStage`] is the state machine between the server's request and the answer it is owed
//! (`ClientMsg::LiveMovieDone`): Idle, then Playing from the moment a [`MovieSource`] opens the movie, then Idle again when
//! it ends. The answer is the [`LiveClient`]'s own `movie_done`, which sends at most one per request, and **every** way out
//! sends it so that the script on the server never waits on this machine:
//!
//! * the id names no known file, the file is not here, the movie is not admitted, or the source fails to prepare it: one
//!   `live:` note, answered at once;
//! * it plays to its end, fails while playing, or outlasts its deadline (the movie's length plus a margin: a covered or
//!   minimised window never presents the last frame): answered;
//! * the player skips it (Enter, Space, Esc, Backspace or a pad's OK/Back): answered.
//!
//! The one way out that is *not* answered is the request going away on its own (a new area: `LiveClient::reset`): the
//! movie is cancelled and nothing is sent, since the server has already moved on. A repeated request for the movie that is
//! already playing does not restart it.
//!
//! [`LocalMovies`] is the source of the window: the field script's `PMVIE` id is named by `fmv_ff7::movies` (disc 1: this
//! client does not know the disc), the file is found in the owner's movies folder (`FMV_MOVIES_DIR`, else the one beside a
//! `flevel.lgp` read in place; the opening may also be named by `FMV_OPENING_MOVIE`), and the existing
//! [`MovieController`] decodes it with the external decoder (`FMV_FFMPEG` and `FMV_FFPROBE`, else the ones on `PATH`).
//! Only a movie the controller's [`Admission`] accepts plays; today that is the opening (`PMVIE 53`). The preflight hashes
//! the file (about 30 MB) when a movie opens, a stall of a few frames at the start of a movie that holds the field anyway.
//! The movie's sound goes through the run's audio engine, so `--mute` or `FMV_AUDIO=off` (no engine, no clock) skip movies;
//! `FMV_AUDIO=null` plays them silently on the null sink's clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use fmv_audio::AudioEngine;
use fmv_media::{Limits, Request, Tools};

use crate::audio::GameAudio;
use crate::live::LiveClient;
use crate::movie::{Admission, MovieController, Phase};
use crate::render::Renderer;

/// Environment: the folder of the owner's movie files (`<name>.avi`).
pub const MOVIES_DIR_ENV: &str = "FMV_MOVIES_DIR";
/// Environment: the opening movie's file, when it is kept apart from the others (a research capture).
pub const OPENING_ENV: &str = "FMV_OPENING_MOVIE";
/// Environment: the decoder tools (absolute paths); else `ffmpeg` and `ffprobe` are looked for on `PATH`.
pub const FFMPEG_ENV: &str = "FMV_FFMPEG";
pub const FFPROBE_ENV: &str = "FMV_FFPROBE";

/// The disc this client plays the field scripts of (it does not know the disc; disc 1 is what the live fields cover).
const DISC: u8 = 1;
/// How long a movie may take to prepare (the decoder lists every frame and decodes all the sound before the first frame:
/// about 13 s for the 120 s opening on the 48-core dev box) before it is given up; a slower machine gets margin.
const PREPARE_LIMIT: Duration = Duration::from_secs(60);
/// Margin on top of a movie's own length before it is cancelled and answered.
const SLACK: Duration = Duration::from_secs(10);

/// How a playing movie is getting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    /// Played to its end.
    Finished,
    /// Cannot go on (decoder, audio or GPU trouble): the reason, for one line of log.
    Failed(String),
}

/// One movie being played.
pub trait Playback {
    /// Advance by wall time since the movie opened (monotonic). Never blocks.
    fn poll(&mut self, elapsed: Duration) -> Status;
    /// The movie's length, once known (while it is still preparing: `None`).
    fn length(&self) -> Option<Duration>;
    /// Stop it now (decoder and sound).
    fn cancel(&mut self);
    /// Draw this frame of it into the window; an `Err` gives the movie up. The state machine's tests have nothing to draw.
    fn draw(&mut self, _renderer: &mut Renderer) -> Result<(), String> {
        Ok(())
    }
}

/// What opens a movie.
pub trait MovieSource {
    /// Open movie `movie` for playing (the work that must be done before the first frame: resolving, admitting, starting
    /// the decoder), or say in a few words why it cannot be played here.
    fn open(&mut self, movie: u16, audio: Option<Arc<AudioEngine>>) -> Result<Box<dyn Playback>, String>;
}

struct Active {
    movie: u16,
    playback: Box<dyn Playback>,
    opened: Instant,
}

/// The live field's movie, from the server's request to the answer.
#[derive(Default)]
pub struct MovieStage {
    active: Option<Active>,
    /// Lines for the log, in order (`live: movie ...`).
    notes: Vec<String>,
}

impl MovieStage {
    /// A movie is on screen: the field is held, its input goes to [`MovieStage::skip_key`] only.
    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Whether `code` skips the movie that is playing: the keys that say OK or back (a pad's confirm and back are the same
    /// keys, see `PadKey::key_code`).
    pub fn skips(code: winit::keyboard::KeyCode) -> bool {
        use winit::keyboard::KeyCode;
        matches!(code, KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space | KeyCode::Escape | KeyCode::Backspace)
    }

    /// One step: start the movie `live` is asking for, move the one that plays, and answer when it is over. `skip` is a
    /// press of a skip key since the last step. `audio` is looked up only when a movie opens.
    pub fn step(&mut self, live: &mut LiveClient, source: &mut dyn MovieSource, now: Instant, skip: bool, audio: &GameAudio) {
        self.step_with(live, source, now, skip, || audio.movie_engine());
    }

    pub(crate) fn step_with(
        &mut self,
        live: &mut LiveClient,
        source: &mut dyn MovieSource,
        now: Instant,
        skip: bool,
        engine: impl FnOnce() -> Option<Arc<AudioEngine>>,
    ) {
        // The request went away on its own (a new area, a reset): nothing is owed, so nothing is sent.
        if self.active.as_ref().is_some_and(|a| live.movie != Some(a.movie)) {
            if let Some(mut gone) = self.active.take() {
                gone.playback.cancel();
                self.notes.push(format!("live: movie {} stopped: the field no longer asks for it", gone.movie));
            }
        }
        if self.active.is_none() {
            if let Some(movie) = live.movie {
                match source.open(movie, engine()) {
                    Ok(playback) => {
                        self.notes.push(format!("live: movie {movie} plays"));
                        self.active = Some(Active { movie, playback, opened: now });
                    }
                    Err(why) => {
                        self.notes.push(format!("live: movie {movie} is skipped ({why})"));
                        live.movie_done();
                    }
                }
            }
            return;
        }
        let Some(active) = self.active.as_mut() else { return };
        let elapsed = now.saturating_duration_since(active.opened);
        let verdict = if skip {
            Some("skipped by the player".to_string())
        } else {
            match active.playback.poll(elapsed) {
                Status::Finished => Some(String::new()),
                Status::Failed(why) => Some(format!("failed: {why}")),
                Status::Running => {
                    let deadline = PREPARE_LIMIT + active.playback.length().unwrap_or_default() + SLACK;
                    (elapsed > deadline).then(|| "gave up: it did not end in time".to_string())
                }
            }
        };
        if let Some(why) = verdict {
            let movie = active.movie;
            if let Some(mut done) = self.active.take() {
                // Ending on its own needs no cancel; every other way out stops the decoder and the sound.
                if !why.is_empty() {
                    done.playback.cancel();
                }
            }
            self.notes.push(if why.is_empty() { format!("live: movie {movie} ended") } else { format!("live: movie {movie} {why}") });
            live.movie_done();
        }
    }

    /// Draw the playing movie's frame into the window. A movie that cannot be drawn is given up at the next step (it is
    /// cancelled there and answered).
    pub fn draw(&mut self, renderer: &mut Renderer) -> Result<(), String> {
        match self.active.as_mut() {
            Some(a) => a.playback.draw(renderer),
            None => Ok(()),
        }
    }

    /// Give up the playing movie because it cannot be drawn: it ends (and is answered) at the next step.
    pub fn fail_draw(&mut self, why: &str) {
        if let Some(a) = self.active.as_mut() {
            a.playback.cancel();
            a.playback = Box::new(Dead(why.to_string()));
        }
    }
}

/// A playback that has already failed (its [`Status::Failed`] is its only word).
struct Dead(String);
impl Playback for Dead {
    fn poll(&mut self, _elapsed: Duration) -> Status {
        Status::Failed(self.0.clone())
    }
    fn length(&self) -> Option<Duration> {
        None
    }
    fn cancel(&mut self) {}
}

// ---------------------------------------------------------------------------------------------
// The window's source: the owner's files, the existing controller
// ---------------------------------------------------------------------------------------------

static CONFIGURED_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Remember where the install's movie folder is (found beside the `flevel.lgp` read in place; `None` when it is not).
/// The environment's `FMV_MOVIES_DIR` still wins.
pub fn configure(dir: Option<PathBuf>) {
    let _ = CONFIGURED_DIR.set(dir);
}

/// Where the decoder tools are: the environment, else `PATH`.
fn tools() -> Result<Tools, String> {
    let find = |env: &str, name: &str| -> Result<PathBuf, String> {
        if let Some(path) = std::env::var_os(env).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
            return Ok(path);
        }
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_absolute() && candidate.is_file())
            .ok_or_else(|| format!("no {name}: set {env} or put it on PATH"))
    };
    Ok(Tools { ffmpeg: find(FFMPEG_ENV, "ffmpeg")?, ffprobe: find(FFPROBE_ENV, "ffprobe")? })
}

/// The admission the controller has for the movie `basename`, if any. Only the inspected opening has a profile.
fn admission(basename: &str) -> Option<Admission> {
    (basename == "opening").then_some(Admission::OpeningFfnxPcmZeroOrigin)
}

/// Where the file of `basename` is.
fn file_of(basename: &str, movies_dir: Option<&Path>, opening: Option<&Path>) -> Option<PathBuf> {
    if basename == "opening" {
        if let Some(explicit) = opening {
            return Some(explicit.to_path_buf());
        }
    }
    crate::ff7::movie_file(movies_dir?, basename)
}

/// The source of the window: the owner's own movie files.
#[derive(Default)]
pub struct LocalMovies {
    next_stream: u64,
}

impl LocalMovies {
    fn dir() -> Option<PathBuf> {
        std::env::var_os(MOVIES_DIR_ENV)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| CONFIGURED_DIR.get().cloned().flatten())
    }
}

impl MovieSource for LocalMovies {
    fn open(&mut self, movie: u16, audio: Option<Arc<AudioEngine>>) -> Result<Box<dyn Playback>, String> {
        self.prepare(movie, audio).map(|p| Box::new(p) as Box<dyn Playback>)
    }
}

impl LocalMovies {
    fn prepare(&mut self, movie: u16, audio: Option<Arc<AudioEngine>>) -> Result<ControllerPlayback, String> {
        let basename = fmv_ff7::movies::video_basename(DISC, movie).ok_or_else(|| "no file is known for this id".to_string())?;
        let admission = admission(basename).ok_or_else(|| format!("{basename} has no admission profile in this client"))?;
        let opening = std::env::var_os(OPENING_ENV).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
        let path = file_of(basename, Self::dir().as_deref(), opening.as_deref())
            .ok_or_else(|| format!("{basename}.avi is not in the movies folder ({MOVIES_DIR_ENV})"))?;
        let engine = audio.ok_or_else(|| "sound is off, so there is no movie clock".to_string())?;
        // Absolute, as the decoder worker requires; the preflight canonicalises it.
        let path = std::path::absolute(&path).map_err(|e| format!("{basename}.avi: {e}"))?;
        let request = Request { path, tools: tools()?, limits: Limits::default() };
        self.next_stream += 1;
        let controller = MovieController::prepare(self.next_stream, request, admission).map_err(|e| format!("{basename}.avi: {e}"))?;
        Ok(ControllerPlayback { controller, engine, played: false, length: None })
    }
}

/// A [`MovieController`] playing on the run's audio engine, drawn by [`Renderer::render_movie`].
struct ControllerPlayback {
    controller: MovieController,
    engine: Arc<AudioEngine>,
    played: bool,
    length: Option<Duration>,
}

impl Playback for ControllerPlayback {
    fn poll(&mut self, elapsed: Duration) -> Status {
        if let Err(e) = self.controller.poll(elapsed) {
            return Status::Failed(e.to_string());
        }
        if self.length.is_none() {
            self.length = self.controller.metadata().map(|m| {
                let audio = Duration::from_secs_f64(m.audio.samples() as f64 / f64::from(m.audio.sample_rate().max(1)));
                let video = m.video.frames.last().map_or(Duration::ZERO, |f| {
                    let ticks = (f.pts + f.duration as i64).max(0) as f64;
                    Duration::from_secs_f64(ticks * f64::from(m.video.time_base.numerator) / f64::from(m.video.time_base.denominator.max(1)))
                });
                audio.max(video)
            });
        }
        match self.controller.phase() {
            Phase::Completed => Status::Finished,
            Phase::Cancelled | Phase::Faulted => Status::Failed("the controller stopped".to_string()),
            Phase::Prepared if !self.played => match self.controller.play(&self.engine, elapsed, 1.0) {
                Ok(()) => {
                    self.played = true;
                    Status::Running
                }
                Err(e) => Status::Failed(e.to_string()),
            },
            _ => Status::Running,
        }
    }

    fn length(&self) -> Option<Duration> {
        self.length
    }

    fn cancel(&mut self) {
        self.controller.cancel();
    }

    fn draw(&mut self, renderer: &mut Renderer) -> Result<(), String> {
        // The window shows the movie's frames from the moment they exist; only a movie that is playing has presentable ones.
        match renderer.render_movie(&mut self.controller)? {
            crate::render::FrameResult::Fatal(why) => Err(why),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
