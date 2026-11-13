//! The movie state machine against scripted playbacks (no GPU, no decoder, no sound device), the pure parts of the
//! window's source, and the controller-backed playback on the movie tests' fake decoder with a manual mixer.

use super::*;
use crate::live::LiveClient;
use fmv_protocol::ClientMsg;
use std::cell::RefCell;
use std::rc::Rc;

/// What a scripted playback did and what it will say.
#[derive(Default)]
struct Script {
    polls: Vec<Duration>,
    cancelled: bool,
    /// Said by successive polls; the last one repeats.
    says: Vec<Status>,
    length: Option<Duration>,
}

struct Scripted(Rc<RefCell<Script>>);
impl Playback for Scripted {
    fn poll(&mut self, elapsed: Duration) -> Status {
        let mut s = self.0.borrow_mut();
        s.polls.push(elapsed);
        let n = s.polls.len();
        s.says.get(n - 1).or_else(|| s.says.last()).cloned().unwrap_or(Status::Running)
    }
    fn length(&self) -> Option<Duration> {
        self.0.borrow().length
    }
    fn cancel(&mut self) {
        self.0.borrow_mut().cancelled = true;
    }
}

/// A source that opens what it is told to, and counts.
#[derive(Default)]
struct Source {
    opened: Vec<u16>,
    refuse: Option<String>,
    script: Rc<RefCell<Script>>,
}
impl MovieSource for Source {
    fn open(&mut self, movie: u16, _audio: Option<Arc<AudioEngine>>) -> Result<Box<dyn Playback>, String> {
        self.opened.push(movie);
        match &self.refuse {
            Some(why) => Err(why.clone()),
            None => Ok(Box::new(Scripted(Rc::clone(&self.script)))),
        }
    }
}

struct Rig {
    live: LiveClient,
    stage: MovieStage,
    source: Source,
    start: Instant,
}
impl Rig {
    fn new() -> Self {
        Self { live: LiveClient::default(), stage: MovieStage::default(), source: Source::default(), start: Instant::now() }
    }
    fn step(&mut self, seconds: u64, skip: bool) {
        self.stage.step_with(&mut self.live, &mut self.source, self.start + Duration::from_secs(seconds), skip, || None);
    }
    fn owed(&mut self) -> Vec<ClientMsg> {
        self.live.take_outbox()
    }
}

#[test]
fn a_movie_that_cannot_be_opened_is_answered_at_once_with_one_line() {
    let mut rig = Rig::new();
    rig.source.refuse = Some("no file is known for this id".into());
    rig.live.on_movie(9);
    rig.step(0, false);
    assert!(!rig.stage.active());
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 9 }]);
    let notes = rig.stage.take_notes();
    assert_eq!(notes, vec!["live: movie 9 is skipped (no file is known for this id)".to_string()]);
    // nothing more, however long we go on
    rig.step(5, false);
    assert!(rig.owed().is_empty());
}

#[test]
fn a_movie_plays_until_it_finishes_and_is_answered_exactly_once() {
    let mut rig = Rig::new();
    rig.source.script.borrow_mut().says = vec![Status::Running, Status::Running, Status::Finished];
    rig.live.on_movie(53);
    rig.step(0, false);
    assert!(rig.stage.active(), "opened on the first step");
    assert!(rig.owed().is_empty(), "the server is not answered while it plays");
    rig.step(1, false);
    rig.step(2, false);
    assert!(rig.stage.active());
    rig.step(3, false);
    assert!(!rig.stage.active());
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
    assert!(!rig.source.script.borrow().cancelled, "a movie that ends on its own is not cancelled");
    assert_eq!(rig.source.script.borrow().polls, vec![Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(3)]);
    rig.step(4, false);
    assert!(rig.owed().is_empty());
}

#[test]
fn skipping_cancels_the_movie_and_answers_once() {
    let mut rig = Rig::new();
    rig.live.on_movie(53);
    rig.step(0, false);
    rig.step(1, true);
    assert!(!rig.stage.active());
    assert!(rig.source.script.borrow().cancelled);
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
    rig.step(2, true);
    assert!(rig.owed().is_empty(), "a late skip answers nothing");
}

#[test]
fn a_failed_movie_is_answered_and_its_decoder_stopped() {
    let mut rig = Rig::new();
    rig.source.script.borrow_mut().says = vec![Status::Running, Status::Failed("decoder died".into())];
    rig.live.on_movie(53);
    rig.step(0, false);
    rig.step(1, false);
    rig.step(2, false);
    assert!(!rig.stage.active());
    assert!(rig.source.script.borrow().cancelled);
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
    assert!(rig.stage.take_notes().iter().any(|n| n == "live: movie 53 failed: decoder died"));
}

#[test]
fn a_movie_that_never_ends_is_cancelled_and_answered_after_its_deadline() {
    let mut rig = Rig::new();
    rig.source.script.borrow_mut().length = Some(Duration::from_secs(13));
    rig.live.on_movie(53);
    rig.step(0, false);
    let deadline = (PREPARE_LIMIT + Duration::from_secs(13) + SLACK).as_secs();
    rig.step(deadline, false);
    assert!(rig.stage.active(), "still inside its deadline");
    rig.step(deadline + 1, false);
    assert!(!rig.stage.active());
    assert!(rig.source.script.borrow().cancelled);
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
}

#[test]
fn a_movie_still_preparing_has_only_the_prepare_limit_and_slack() {
    let mut rig = Rig::new();
    rig.live.on_movie(53);
    rig.step(0, false);
    let deadline = (PREPARE_LIMIT + SLACK).as_secs();
    rig.step(deadline, false);
    assert!(rig.stage.active());
    rig.step(deadline + 1, false);
    assert!(!rig.stage.active());
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
}

#[test]
fn a_new_area_stops_the_movie_without_answering() {
    let mut rig = Rig::new();
    rig.live.on_movie(53);
    rig.step(0, false);
    assert!(rig.stage.active());
    rig.live.reset();
    rig.step(1, false);
    assert!(!rig.stage.active());
    assert!(rig.source.script.borrow().cancelled);
    assert!(rig.owed().is_empty(), "the server has moved on: nothing is owed");
    rig.step(2, false);
    assert!(rig.owed().is_empty());
}

#[test]
fn a_repeated_request_for_the_movie_that_plays_does_not_restart_it() {
    let mut rig = Rig::new();
    rig.live.on_movie(53);
    rig.step(0, false);
    rig.live.on_movie(53);
    rig.step(1, false);
    rig.step(2, false);
    assert_eq!(rig.source.opened, vec![53]);
    assert!(rig.stage.active());
    assert!(!rig.source.script.borrow().cancelled);
    rig.step(3, true);
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }], "one answer however often it was asked");
}

#[test]
fn another_movie_replaces_the_one_playing_and_only_the_new_one_is_answered() {
    let mut rig = Rig::new();
    rig.live.on_movie(23);
    rig.step(0, false);
    rig.live.on_movie(24);
    rig.step(1, false);
    assert_eq!(rig.source.opened, vec![23, 24]);
    assert!(rig.stage.active());
    rig.step(2, true);
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 24 }]);
}

#[test]
fn the_same_movie_asked_again_after_it_ended_plays_again() {
    let mut rig = Rig::new();
    rig.live.on_movie(24);
    rig.step(0, false);
    rig.step(1, true);
    assert_eq!(rig.owed().len(), 1);
    rig.live.on_movie(24);
    rig.step(2, false);
    assert_eq!(rig.source.opened, vec![24, 24]);
    assert!(rig.stage.active());
}

#[test]
fn a_movie_that_cannot_be_drawn_is_given_up_and_answered_at_the_next_step() {
    let mut rig = Rig::new();
    rig.live.on_movie(53);
    rig.step(0, false);
    rig.stage.fail_draw("surface lost");
    rig.step(1, false);
    assert!(!rig.stage.active());
    assert!(rig.source.script.borrow().cancelled, "the movie that could not be drawn is stopped");
    assert_eq!(rig.owed(), vec![ClientMsg::LiveMovieDone { movie: 53 }]);
    assert!(rig.stage.take_notes().iter().any(|n| n == "live: movie 53 failed: surface lost"));
}

#[test]
fn only_the_ok_and_back_keys_skip() {
    use winit::keyboard::KeyCode;
    for code in [KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Space, KeyCode::Escape, KeyCode::Backspace] {
        assert!(MovieStage::skips(code), "{code:?}");
    }
    for code in [KeyCode::KeyW, KeyCode::ArrowUp, KeyCode::F1, KeyCode::KeyM, KeyCode::ShiftLeft] {
        assert!(!MovieStage::skips(code), "{code:?}");
    }
}

#[test]
fn the_window_source_pins_the_opening_and_gives_every_other_named_movie_the_general_profile() {
    assert!(matches!(admission("opening"), Some(Admission::OpeningFfnxPcmZeroOrigin)));
    // With sound and without (mkup and funeral have no audio stream on the owner's disc).
    for other in ["ontrain", "mainplr", "mkup", "funeral", "junon"] {
        assert!(matches!(admission(other), Some(Admission::General)), "{other} has the general profile");
    }
    // Every id of the disc-1 table is covered, and each name other than the opening is general.
    for (id, name) in fmv_ff7::movies::disc_table(DISC) {
        assert!(admission(name).is_some(), "movie {id} ({name})");
        assert_eq!(matches!(admission(name), Some(Admission::General)), name != "opening", "{name}");
    }
    // A name the table does not know, however it is spelled, is not admitted (and the opening is not matched loosely).
    for unknown in ["nosuchmovie", "", "Opening", "opening.avi", "../opening"] {
        assert!(admission(unknown).is_none(), "{unknown:?}");
    }
    let mut source = LocalMovies::default();
    let why = |movie: u16, source: &mut LocalMovies| source.open(movie, None).err().expect("cannot be opened here");
    assert!(why(999, &mut source).contains("no file is known"));
    assert!(why(0xFFFF, &mut source).contains("no file is known"));
    // Admitted, so the answer comes from the missing file, not from a missing profile.
    assert!(!why(24, &mut source).contains("admission profile"));
    assert!(!why(20, &mut source).contains("admission profile"));
    assert!(why(20, &mut source).contains("mkup.avi"), "movie 20 is mkup");
    // The opening is admitted, so the answer comes from the file or the sound: still never a playback without both.
    assert!(source.open(53, None).is_err());
}

#[test]
fn files_are_found_in_the_movies_folder_and_the_opening_can_be_named_apart() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Opening.avi"), b"x").unwrap();
    std::fs::write(other.path().join("elsewhere.avi"), b"y").unwrap();
    let named = other.path().join("elsewhere.avi");
    assert_eq!(file_of("opening", Some(dir.path()), None), Some(dir.path().join("Opening.avi")));
    assert_eq!(file_of("opening", Some(dir.path()), Some(&named)), Some(named.clone()), "an explicit opening wins");
    assert_eq!(file_of("opening", None, Some(&named)), Some(named.clone()));
    assert_eq!(file_of("opening", None, None), None);
    assert_eq!(file_of("mainplr", Some(dir.path()), Some(&named)), None, "the opening's own path is for the opening only");
    assert_eq!(file_of("mainplr", None, None), None);
}

#[test]
fn the_controller_playback_prepares_plays_and_stops_on_the_fake_decoder() {
    use crate::movie::tests::Fixture;
    use fmv_audio::{AudioConfig, AudioEngine};
    let Some(fixture) = Fixture::normal() else { return };
    // A manual mixer: no device, and nothing audible whatever FMV_AUDIO says.
    let engine = Arc::new(AudioEngine::start_manual(AudioConfig { sample_rate: 22_050, ..AudioConfig::default() }));
    let mut playback = ControllerPlayback { controller: fixture.controller(1), engine: Some(Arc::clone(&engine)), played: false, length: None };
    let started = Instant::now();
    while !playback.played {
        assert!(started.elapsed() < Duration::from_secs(6), "the fake decoder never delivered a first frame");
        assert_eq!(playback.poll(started.elapsed()), Status::Running);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(playback.controller.audio_handle().is_some(), "playing started the movie voice");
    // 8,820 audio frames at 22,050 Hz is 0.4 s; the last video frame ends at 0.35 s.
    let length = playback.length().expect("known once prepared");
    assert!((length.as_secs_f64() - 0.4).abs() < 0.01, "{length:?}");
    // Without a presented last frame it never finishes by itself: the stage's deadline covers that.
    engine.pump(22_050);
    assert_eq!(playback.poll(started.elapsed()), Status::Running);
    playback.cancel();
    assert!(matches!(playback.poll(started.elapsed()), Status::Failed(_)), "a cancelled movie ends the stage's wait");
}

#[test]
fn a_silent_movie_prepares_and_plays_with_no_audio_engine_at_all() {
    use crate::movie::tests::Fixture;
    let Some(fixture) = Fixture::silent(&[(0, 100), (100, 150), (250, 100)], None) else { return };
    // Sound is off (no engine, as with --mute): a movie with no audio does not need one.
    let mut playback = ControllerPlayback { controller: fixture.general(1), engine: None, played: false, length: None };
    let started = Instant::now();
    while !playback.played {
        assert!(started.elapsed() < Duration::from_secs(6), "the fake decoder never delivered a first frame");
        assert_eq!(playback.poll(started.elapsed()), Status::Running);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(playback.controller.is_silent() && playback.controller.audio_handle().is_none());
    // The length is the video's own: the last frame ends at 0.35 s.
    let length = playback.length().expect("known once prepared");
    assert!((length.as_secs_f64() - 0.35).abs() < 0.001, "{length:?}");
    // It never finishes by wall time alone (the last frame must be presented, which only a drawn frame does).
    assert_eq!(playback.poll(started.elapsed() + Duration::from_secs(60)), Status::Running);
    playback.cancel();
    assert!(matches!(playback.poll(started.elapsed() + Duration::from_secs(61)), Status::Failed(_)));
}

#[test]
fn a_movie_with_sound_and_no_audio_engine_is_refused_as_soon_as_its_metadata_says_so() {
    use crate::movie::tests::Fixture;
    let Some(fixture) = Fixture::normal() else { return };
    let mut playback = ControllerPlayback { controller: fixture.general(1), engine: None, played: false, length: None };
    let started = Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_secs(6), "never refused");
        match playback.poll(started.elapsed()) {
            Status::Running => std::thread::sleep(Duration::from_millis(2)),
            Status::Failed(why) => {
                assert!(why.contains("sound is off"), "{why}");
                break;
            }
            Status::Finished => panic!("a movie with sound cannot finish without a clock"),
        }
    }
    assert!(!playback.played);
    playback.cancel();
}

/// The one movie this client admits, end to end up to its first played frame: the real preflight (size and blake3), the
/// real decoder profile, a manual mixer. Prints how long preparation took (the stage allows `PREPARE_LIMIT` + `SLACK`
/// while the length is unknown). Run with `FMV_OPENING_MOVIE`, `FMV_FFMPEG` and `FMV_FFPROBE` set:
/// `cargo test -p fmv-client the_real_opening -- --ignored --nocapture`.
#[test]
#[ignore = "needs the owned opening (FMV_OPENING_MOVIE) and the decoder tools (FMV_FFMPEG, FMV_FFPROBE)"]
fn the_real_opening_prepares_and_starts_playing_through_the_window_source() {
    use fmv_audio::{AudioConfig, AudioEngine};
    let engine = Arc::new(AudioEngine::start_manual(AudioConfig { sample_rate: 22_050, ..AudioConfig::default() }));
    let opened = Instant::now();
    let mut playback = LocalMovies::default().prepare(fmv_ff7::movies::OPENING, Some(Arc::clone(&engine))).expect("the opening is admitted");
    let admitted_in = opened.elapsed();
    let mut known = None;
    while !playback.played {
        let elapsed = opened.elapsed();
        assert!(elapsed < PREPARE_LIMIT + SLACK, "not playing {elapsed:?} after it opened: the stage would have given up");
        assert_eq!(playback.poll(elapsed), Status::Running);
        if known.is_none() && playback.length().is_some() {
            known = Some(elapsed);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let length = playback.length().expect("known by the time it plays");
    eprintln!("opening: preflight {admitted_in:?}, length known after {known:?}, playing after {:?}, length {length:?}", opened.elapsed());
    assert!((length.as_secs_f64() - 119.467).abs() < 0.5, "{length:?}");
    assert!(playback.controller.audio_handle().is_some());
    playback.cancel();
}

#[test]
fn the_steam_editions_movies_are_found_in_the_default_library_or_one_the_library_list_names() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(steam_movies_dir_in(home.path()), None, "no Steam, no folder");
    let steam = home.path().join(".local/share/Steam");
    std::fs::create_dir_all(steam.join("steamapps")).unwrap();
    let other = home.path().join("games/SteamLibrary");
    let movies = other.join("steamapps/common/FINAL FANTASY VII Steam Edition/ff7/workingdir/data/movies");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::write(steam.join("steamapps/libraryfolders.vdf"), format!("\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n", other.display())).unwrap();
    assert_eq!(steam_movies_dir_in(home.path()), Some(movies), "a library the list names");
    let default = steam.join("steamapps/common/FINAL FANTASY VII/data/movies");
    std::fs::create_dir_all(&default).unwrap();
    assert_eq!(steam_movies_dir_in(home.path()), Some(default), "the default library comes first");
}
