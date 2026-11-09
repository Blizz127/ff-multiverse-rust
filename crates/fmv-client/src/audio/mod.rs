//! The client's sound: the field's music, the battle music, the victory fanfare and the menu sound
//! effects, played through [`fmv_audio`] and decided from what the game is doing.
//!
//! ```text
//!   window / headless loop ──update(area, battle counters)──▶ GameAudio ──play_music / play_sfx──▶ AudioEngine
//!                                                               │  ▲                               (mixer thread,
//!                                                    Director ◀─┘  │ Done messages                 device or null sink)
//!                                                    (what should  └── Worker: reads a field's script, decodes
//!                                                     be playing)          a track, reads the menu sounds
//! ```
//!
//! * [`Director`] (pure, tested alone) says which track should be playing: the area's music, the
//!   battle theme, the fanfare, the field's music again.
//! * [`GameAudio`] watches the game each frame ([`GameAudio::update`]: two cheap comparisons and a
//!   queue poll, never a wait), asks the [`Worker`] for what it needs (decoding takes seconds, so the
//!   next fight's music and the fanfare are decoded in advance, and the old music plays on until a new
//!   track is ready) and hands the decoded track to the engine, which crossfades.
//! * **Nothing here can stop the game.** A library without audio, a missing file, a damaged track,
//!   no sound card and a thread that cannot start are each one line on stderr (starting `audio:`) and
//!   silence; `--mute`, `FMV_AUDIO=off` and headless runs build nothing at all.
//!
//! What the log says (it is the evidence of a headless run, which has no ears): one `audio: field
//! <area>:` line per area with the tracks its script names, `audio: decoded <track>` as each
//! finishes, and `audio: playing <track> (<why>)` at the moment a track really starts.

mod cues;
mod director;
pub mod ff7;
mod worker;

pub use cues::{battle_cue, menu_cue, Cue};
pub use director::BattleCounts;

use crate::battle::BattleClient;
use director::{Director, STANDARD_BATTLE_TRACK};
use worker::{Done, Tasks, ThreadWorker, Worker};
use crate::session::Session;
use fmv_audio::{AudioConfig, AudioEngine, DecodedMusic, Logger, Mode, PlayOptions, SfxClip, TrackId};
use fmv_core::GameId;
use std::collections::HashMap;
use std::time::Instant;

/// Decoded tracks kept at once (the playing one, the next fight's, the fanfare, and a little slack): about
/// 35 MB for a three-minute theme.
const CACHE_TRACKS: usize = 5;
/// Volume steps from silence to full: the keys move the master level by a tenth.
const VOLUME_STEPS: u8 = 10;
/// A track that had to be waited for this long gets the wait mentioned in the line that says it started.
const SLOW_START: std::time::Duration = std::time::Duration::from_millis(100);
/// The environment variable that names another track for the victory fanfare (see `main::start_audio`).
pub const FANFARE_ENV: &str = "FMV_AUDIO_FANFARE";

/// What this run does with sound: `--mute` is always off; a headless bot, which is a test tool, is silent
/// unless `FMV_AUDIO` (`env`) was set on purpose; otherwise `FMV_AUDIO` decides, and an unset one is
/// `auto` (the machine's output, or the null sink when there is none). The second value is a note about a
/// `FMV_AUDIO` that makes no sense.
pub fn run_mode(mute: bool, headless: bool, env: Option<&str>) -> (Mode, Option<String>) {
    fmv_audio::resolve_mode(mute || (headless && env.is_none()), env)
}

/// Whether `name` can be a track name (the spelling `music.idx` uses: lower-case letters, digits, `_` and `-`).
pub fn is_track_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

impl BattleCounts {
    /// The counters of the client's battle mirror.
    pub fn of(battle: &BattleClient) -> Self {
        Self {
            in_battle: battle.in_battle(),
            entered: battle.entered(),
            ended: battle.ended(),
            victories: battle.victories(),
            defeats: battle.defeats(),
            escapes: battle.escapes(),
        }
    }
}

/// Decoded tracks, least recently used first.
struct Cache {
    entries: Vec<(String, DecodedMusic)>,
}

impl Cache {
    fn get(&mut self, name: &str) -> Option<DecodedMusic> {
        let at = self.entries.iter().position(|(n, _)| n == name)?;
        let entry = self.entries.remove(at);
        let music = entry.1.clone();
        self.entries.push(entry);
        Some(music)
    }

    fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }

    fn insert(&mut self, name: String, music: DecodedMusic) {
        self.entries.retain(|(n, _)| *n != name);
        self.entries.push((name, music));
        while self.entries.len() > CACHE_TRACKS {
            self.entries.remove(0);
        }
    }
}

struct Live {
    engine: AudioEngine,
    worker: Box<dyn Worker>,
    director: Director,
    log: Logger,
    cache: Cache,
    /// Tracks asked of the worker that are being decoded, or failed (a failed one is never asked for
    /// again); a decoded one lives in the cache instead. The value says whether it was asked for as
    /// urgent (something is waiting for it) or only in advance.
    asked: HashMap<String, bool>,
    /// The track the director wants that is not decoded yet, and since when.
    waiting: Option<(String, Instant)>,
    /// What was last handed to the engine.
    playing: Option<String>,
    /// Track names by engine id (`TrackId(n)` is `ids[n - 1]`).
    ids: Vec<String>,
    area: Option<String>,
    ended_seen: u64,
    cues: HashMap<Cue, SfxClip>,
    /// Every track handed to the engine, in order (the headless summary line, and tests).
    history: Vec<String>,
    volume_step: u8,
    muted: bool,
}

impl Live {
    fn track_id(&mut self, name: &str) -> TrackId {
        let at = match self.ids.iter().position(|n| n == name) {
            Some(at) => at,
            None => {
                self.ids.push(name.to_string());
                self.ids.len() - 1
            }
        };
        TrackId(at as u32 + 1)
    }

    fn say(&self, line: &str) {
        (self.log)(line);
    }

    /// Ask the worker for a track unless it is decoded already, being decoded, or failed (a track that
    /// failed is never asked for again). A track the cache let go of is asked for again when it is wanted.
    fn ask(&mut self, track: &str, urgent: bool) {
        if self.cache.contains(track) {
            return;
        }
        match self.asked.get(track).copied() {
            None => {
                self.asked.insert(track.to_string(), urgent);
                self.worker.decode(track, urgent);
            }
            // Asked for in advance and wanted now: tell the worker, which moves it to the front.
            Some(false) if urgent => {
                self.asked.insert(track.to_string(), true);
                self.worker.decode(track, true);
            }
            Some(_) => {}
        }
    }

    fn pump_worker(&mut self) {
        for done in self.worker.poll() {
            match done {
                Done::Area { key, music } => {
                    let line = format!(
                        "field {key}: music {}, battle {}, after the fight {}",
                        music.arrival.as_deref().unwrap_or("-"),
                        music.battle.as_deref().map_or_else(|| format!("{STANDARD_BATTLE_TRACK} (the standard theme)"), str::to_string),
                        music.after_battle.as_deref().unwrap_or("-")
                    );
                    if self.director.area_music(&key, music) {
                        self.say(&line);
                        // The next fight's music and the fanfare take seconds to decode: do it now, in
                        // the background, so that neither has to be waited for when it is needed.
                        for track in self.director.prefetch() {
                            self.ask(&track, false);
                        }
                    }
                }
                Done::Track { name, result, took } => match result {
                    Ok(music) => {
                        self.say(&format!("decoded {name}: {:.0} s of music in {} ms", music.duration().as_secs_f64(), took.as_millis()));
                        // No longer in flight: the cache holds it (and may let it go later, to be asked for again).
                        self.asked.remove(&name);
                        self.cache.insert(name, music);
                    }
                    Err(why) => {
                        self.say(&format!("cannot play the track {name} ({why}); going on without it"));
                        self.director.unavailable(&name);
                        // Never asked for again, urgent or not.
                        self.asked.insert(name.clone(), true);
                        if self.waiting.as_ref().is_some_and(|(t, _)| *t == name) {
                            self.waiting = None;
                        }
                    }
                },
                Done::Cues(Ok(clips)) => {
                    self.say(&format!("menu sounds: {} loaded", clips.len()));
                    self.cues = clips.into_iter().collect();
                }
                Done::Cues(Err(why)) => self.say(&format!("no menu sounds ({why})")),
            }
        }
    }

    fn apply_want(&mut self) {
        let Some(want) = self.director.want() else { return };
        if self.playing.as_deref() == Some(want.track.as_str()) {
            return;
        }
        let Some(music) = self.cache.get(&want.track) else {
            // Not decoded yet: ask for it now (it jumps the queue) and let the old music play on.
            self.ask(&want.track, true);
            if self.waiting.as_ref().is_none_or(|(t, _)| *t != want.track) {
                self.waiting = Some((want.track.clone(), Instant::now()));
            }
            return;
        };
        // How long the track had to be waited for, when that was long enough to be worth saying.
        let waited = self.waiting.take().filter(|(t, _)| *t == want.track).map(|(_, since)| since.elapsed()).filter(|d| *d >= SLOW_START);
        let id = self.track_id(&want.track);
        let fade = director::fade_into(want.why);
        self.engine.play_music(id, music.source(), PlayOptions { fade, repeat: want.repeat, gain: 1.0 });
        let note = waited.map_or_else(String::new, |d| format!(", waited {} ms for it", d.as_millis()));
        self.say(&format!("playing {} ({}{note})", want.track, want.why.text()));
        self.history.push(want.track.clone());
        self.playing = Some(want.track);
    }
}

/// The soundtrack of one run. [`GameAudio::off`] (also the default) plays nothing and costs nothing.
#[derive(Default)]
pub struct GameAudio {
    live: Option<Box<Live>>,
}

impl GameAudio {
    /// No sound at all: nothing is built, every call returns at once.
    pub fn off() -> Self {
        Self { live: None }
    }

    /// Start the sound with the machine's own output (or the null sink; see [`AudioConfig`]) and the
    /// real worker over `tasks`. `config.mode == Off` builds nothing.
    pub fn start(config: AudioConfig, tasks: Tasks) -> Self {
        if config.mode == Mode::Off {
            return Self::off();
        }
        let log = config.logger.clone();
        let engine = AudioEngine::start(config);
        Self::with_parts(engine, Box::new(ThreadWorker::start(tasks)), log)
    }

    /// The sound over an engine and a worker the caller made (tests use a manual engine and a fake worker).
    pub fn with_parts(engine: AudioEngine, mut worker: Box<dyn Worker>, log: Logger) -> Self {
        let master = engine.status().volumes.master;
        // The menu sounds are small: read them at once, in the background.
        worker.load_cues();
        Self {
            live: Some(Box::new(Live {
                engine,
                worker,
                director: Director::new(),
                log,
                cache: Cache { entries: Vec::new() },
                asked: HashMap::new(),
                waiting: None,
                playing: None,
                ids: Vec::new(),
                area: None,
                ended_seen: 0,
                cues: HashMap::new(),
                history: Vec::new(),
                volume_step: (master.clamp(0.0, 1.0) * f32::from(VOLUME_STEPS)).round() as u8,
                muted: false,
            })),
        }
    }

    /// Play `track` as the victory fanfare instead of the standard one (see
    /// [`Director::with_victory_track`]); no effect on a soundtrack that is off.
    pub fn with_victory_track(mut self, track: &str) -> Self {
        if let Some(live) = self.live.as_mut() {
            live.director = std::mem::take(&mut live.director).with_victory_track(track);
        }
        self
    }

    /// Is there a soundtrack at all (not [`GameAudio::off`])? (tests)
    #[cfg(test)]
    pub fn is_on(&self) -> bool {
        self.live.is_some()
    }

    /// Follow the game: the area the client is in (`None` outside FF7 fields, which have no music) and
    /// its battle counters. Cheap, never blocks; call it every frame, whatever is on screen.
    pub fn update(&mut self, area: Option<&str>, battle: BattleCounts) {
        let Some(live) = self.live.as_mut() else { return };
        live.pump_worker();
        if let Some(key) = area {
            if live.area.as_deref() != Some(key) {
                live.area = Some(key.to_string());
                live.director.area(key);
                live.worker.analyze(key);
            }
        }
        live.director.battle(battle);
        let ended = live.engine.status().music_ended;
        if ended > live.ended_seen {
            // A one-shot (the fanfare) ran to its end: nothing is playing any more, whatever it was.
            live.ended_seen = ended;
            live.playing = None;
            live.director.track_ended();
        }
        live.apply_want();
    }

    /// [`GameAudio::update`] from a joined session.
    pub fn update_from(&mut self, session: &Session) {
        if self.live.is_none() {
            return;
        }
        let area = &session.info.area_id;
        let key = (area.game == GameId::Ff7).then_some(area.key.as_str());
        self.update(key, BattleCounts::of(&session.state.battle));
    }

    /// Play a menu sound effect (silently nothing while the effects are not loaded, or when there are none).
    pub fn cue(&mut self, cue: Cue) {
        let Some(live) = self.live.as_mut() else { return };
        if let Some(clip) = live.cues.get(&cue) {
            live.engine.play_sfx(clip, cue.gain(), 0.0);
        }
    }

    /// Mute or unmute (the music keeps moving while muted). `None` when there is no sound.
    pub fn toggle_mute(&mut self) -> Option<bool> {
        let live = self.live.as_mut()?;
        live.muted = !live.muted;
        live.engine.set_muted(live.muted);
        live.say(if live.muted { "muted" } else { "unmuted" });
        Some(live.muted)
    }

    /// Raise or lower the master volume by a tenth; returns the new level in percent (`None` when there
    /// is no sound).
    pub fn change_volume(&mut self, up: bool) -> Option<u8> {
        let live = self.live.as_mut()?;
        live.volume_step = if up { (live.volume_step + 1).min(VOLUME_STEPS) } else { live.volume_step.saturating_sub(1) };
        live.engine.set_master_volume(f32::from(live.volume_step) / f32::from(VOLUME_STEPS));
        let percent = u32::from(live.volume_step) * 100 / u32::from(VOLUME_STEPS);
        live.say(&format!("volume {percent}%"));
        Some(percent as u8)
    }

    /// The engine's state (tests).
    #[cfg(test)]
    pub fn status(&self) -> Option<fmv_audio::AudioStatus> {
        self.live.as_ref().map(|l| l.engine.status())
    }

    /// Every track handed to the engine so far, in order (tests).
    #[cfg(test)]
    pub fn history(&self) -> &[String] {
        self.live.as_ref().map_or(&[], |l| l.history.as_slice())
    }

    /// The track handed to the engine last (tests).
    #[cfg(test)]
    pub fn now_playing(&self) -> Option<&str> {
        self.live.as_ref().and_then(|l| l.playing.as_deref())
    }

    /// One line for the end of a headless run: where the sound went and which tracks it started, in
    /// order (`None` for a run without sound).
    pub fn summary(&self) -> Option<String> {
        let live = self.live.as_ref()?;
        let status = live.engine.status();
        let played = if live.history.is_empty() { "-".to_string() } else { live.history.join(" > ") };
        Some(format!("sound {:?}, tracks started: {played}", status.backend))
    }

    /// The engine id a track name was given (tests compare it with [`AudioStatus::music`]).
    #[cfg(test)]
    fn id_of(&self, name: &str) -> Option<TrackId> {
        let live = self.live.as_ref()?;
        live.ids.iter().position(|n| n == name).map(|at| TrackId(at as u32 + 1))
    }

    /// Render `frames` of a manual engine (tests).
    #[cfg(test)]
    fn pump(&self, frames: usize) -> Vec<f32> {
        self.live.as_ref().map_or_else(Vec::new, |l| l.engine.pump(frames))
    }
}

#[cfg(test)]
mod tests {
    use super::director::AreaMusic;
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    const RATE: u32 = 48_000;

    /// A track of `frames` frames of a constant (never zero, so that it can be told from silence).
    fn track(frames: usize) -> DecodedMusic {
        DecodedMusic::from_stereo(RATE, vec![8_000; frames * 2], None).expect("a track")
    }

    #[derive(Default)]
    struct State {
        /// What each field says; a field not listed says nothing.
        areas: HashMap<String, AreaMusic>,
        /// Tracks that decode, with their length in frames; any other track has no file.
        tracks: HashMap<String, usize>,
        /// Answers that came back and wait for the next poll.
        inbox: Vec<Done>,
        /// Every request, in order.
        asked: Vec<String>,
        /// Hold the answers back until the test releases them.
        hold: bool,
        held: Vec<Done>,
        cues: bool,
    }

    /// A worker that answers on the spot (or when told to): no threads, no waiting.
    struct Fake(Rc<RefCell<State>>);

    impl Worker for Fake {
        fn analyze(&mut self, key: &str) {
            let mut s = self.0.borrow_mut();
            s.asked.push(format!("analyze {key}"));
            let music = s.areas.get(key).cloned().unwrap_or_default();
            let done = Done::Area { key: key.to_string(), music };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
        }

        fn decode(&mut self, name: &str, urgent: bool) {
            let mut s = self.0.borrow_mut();
            s.asked.push(format!("decode {name}{}", if urgent { " (urgent)" } else { "" }));
            let result = s.tracks.get(name).map(|&frames| track(frames)).ok_or_else(|| "no file for it".to_string());
            let done = Done::Track { name: name.to_string(), result, took: std::time::Duration::from_millis(7) };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
        }

        fn load_cues(&mut self) {
            let mut s = self.0.borrow_mut();
            let clips = if s.cues {
                Cue::ALL.iter().map(|&c| (c, SfxClip::from_mono(RATE, &vec![0.5; 480]).expect("a clip"))).collect()
            } else {
                Vec::new()
            };
            let done = Done::Cues(if s.cues { Ok(clips) } else { Err("no sound files".to_string()) });
            s.inbox.push(done);
        }

        fn poll(&mut self) -> Vec<Done> {
            std::mem::take(&mut self.0.borrow_mut().inbox)
        }
    }

    fn area(arrival: Option<&str>, battle: Option<&str>, after: Option<&str>) -> AreaMusic {
        AreaMusic { arrival: arrival.map(str::to_string), battle: battle.map(str::to_string), after_battle: after.map(str::to_string) }
    }

    struct Rig {
        audio: GameAudio,
        state: Rc<RefCell<State>>,
        lines: Arc<Mutex<Vec<String>>>,
    }

    impl Rig {
        fn new(areas: &[(&str, AreaMusic)], tracks: &[(&str, usize)]) -> Rig {
            Rig::with_cues(areas, tracks, true)
        }

        /// `cues`: whether the library has the menu sound files.
        fn with_cues(areas: &[(&str, AreaMusic)], tracks: &[(&str, usize)], cues: bool) -> Rig {
            let state = Rc::new(RefCell::new(State {
                areas: areas.iter().map(|(k, m)| (k.to_string(), m.clone())).collect(),
                tracks: tracks.iter().map(|(n, f)| (n.to_string(), *f)).collect(),
                cues,
                ..State::default()
            }));
            let lines = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&lines);
            let log: Logger = Arc::new(move |line| sink.lock().expect("log").push(line.to_string()));
            let config = AudioConfig { sample_rate: RATE, logger: Arc::clone(&log), ..AudioConfig::default() };
            let audio = GameAudio::with_parts(AudioEngine::start_manual(config), Box::new(Fake(Rc::clone(&state))), log);
            Rig { audio, state, lines }
        }

        fn lines(&self) -> Vec<String> {
            self.lines.lock().expect("log").clone()
        }

        fn playing(&self) -> Option<String> {
            self.audio.pump(1);
            let status = self.audio.status()?;
            let id = status.music?;
            self.audio.live.as_ref()?.ids.get(id.0 as usize - 1).cloned()
        }

        /// One frame of the game, then a little time on the engine so that it applies what was asked.
        fn frame(&mut self, area: Option<&str>, battle: BattleCounts) {
            self.audio.update(area, battle);
            self.audio.pump(64);
        }
    }

    fn counts(in_battle: bool, entered: u64, ended: u64, victories: u64) -> BattleCounts {
        BattleCounts { in_battle, entered, ended, victories, defeats: 0, escapes: 0 }
    }

    const NONE: BattleCounts = BattleCounts { in_battle: false, entered: 0, ended: 0, victories: 0, defeats: 0, escapes: 0 };

    #[test]
    fn a_field_plays_its_music_then_the_fight_its_theme_then_the_fanfare_then_the_field_again() {
        let mut rig = Rig::new(
            &[("md1stin", area(Some("ob"), None, Some("ob")))],
            &[("ob", 4_800), ("bat", 4_800), ("fanfare", 2_400)],
        );
        // Entering the field: the script is read, the music decoded, and it plays.
        rig.frame(Some("md1stin"), NONE);
        assert_eq!(rig.audio.now_playing(), None, "nothing is known of the field's music until its script is read");
        rig.frame(Some("md1stin"), NONE);
        rig.frame(Some("md1stin"), NONE);
        assert_eq!(rig.playing().as_deref(), Some("ob"));
        assert_eq!(rig.audio.history(), ["ob"]);
        // The next fight's music and the fanfare were decoded in advance, in the background.
        let asked = rig.state.borrow().asked.clone();
        assert!(asked.contains(&"decode bat".to_string()) && asked.contains(&"decode fanfare".to_string()), "{asked:?}");
        // A fight starts: its theme plays at once (it was decoded already).
        rig.frame(Some("md1stin"), counts(true, 1, 0, 0));
        assert_eq!(rig.playing().as_deref(), Some("bat"));
        // The fight is won: the fanfare, once.
        rig.frame(Some("md1stin"), counts(false, 1, 1, 1));
        assert_eq!(rig.playing().as_deref(), Some("fanfare"));
        // Its end brings the field's music back (2,400 frames plus the fade and the resampler's tail).
        for _ in 0..8 {
            rig.audio.pump(1_000);
            rig.audio.update(Some("md1stin"), counts(false, 1, 1, 1));
        }
        rig.audio.pump(64);
        assert_eq!(rig.playing().as_deref(), Some("ob"));
        assert_eq!(rig.audio.history(), ["ob", "bat", "fanfare", "ob"]);
        // The log tells the same story, in order, with the track starts after the decodes.
        let lines = rig.lines();
        let started: Vec<&str> = lines.iter().filter(|l| l.starts_with("playing ")).map(String::as_str).collect();
        assert_eq!(
            started,
            [
                "playing ob (field)",
                "playing bat (battle)",
                "playing fanfare (victory)",
                "playing ob (back in the field after the fight)"
            ]
        );
        assert!(lines.iter().any(|l| l.starts_with("field md1stin: music ob, battle bat (the standard theme), after the fight ob")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("decoded bat:")), "{lines:?}");
    }

    #[test]
    fn running_away_and_losing_go_back_to_the_field_music_without_a_fanfare() {
        for (escapes, defeats) in [(1, 0), (0, 1)] {
            let mut rig = Rig::new(&[("a", area(Some("ob"), None, None))], &[("ob", 4_800), ("bat", 4_800), ("fanfare", 2_400)]);
            for _ in 0..3 {
                rig.frame(Some("a"), NONE);
            }
            rig.frame(Some("a"), counts(true, 1, 0, 0));
            assert_eq!(rig.playing().as_deref(), Some("bat"));
            rig.frame(Some("a"), BattleCounts { in_battle: false, entered: 1, ended: 1, victories: 0, defeats, escapes });
            assert_eq!(rig.playing().as_deref(), Some("ob"), "escapes {escapes} defeats {defeats}");
            assert_eq!(rig.audio.history(), ["ob", "bat", "ob"]);
        }
    }

    #[test]
    fn the_music_goes_on_across_an_area_that_names_none_and_is_not_restarted() {
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None)), ("b", area(None, None, None)), ("c", area(Some("ob"), None, None))], &[("ob", 48_000)]);
        for _ in 0..3 {
            rig.frame(Some("a"), NONE);
        }
        assert_eq!(rig.playing().as_deref(), Some("ob"));
        for key in ["b", "b", "b", "c", "c", "c"] {
            rig.frame(Some(key), NONE);
        }
        assert_eq!(rig.audio.history(), ["ob"], "the same track in the next area does not start again");
        assert_eq!(rig.playing().as_deref(), Some("ob"));
    }

    #[test]
    fn a_new_areas_track_crossfades_in_when_it_is_ready_and_the_old_one_plays_until_then() {
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None)), ("b", area(Some("mati"), None, None))], &[("ob", 48_000), ("mati", 48_000)]);
        for _ in 0..3 {
            rig.frame(Some("a"), NONE);
        }
        assert_eq!(rig.playing().as_deref(), Some("ob"));
        // The answers are held back, as a slow decode would hold them.
        rig.state.borrow_mut().hold = true;
        rig.frame(Some("b"), NONE);
        rig.frame(Some("b"), NONE);
        assert_eq!(rig.playing().as_deref(), Some("ob"), "the old music goes on while the new track is being prepared");
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        {
            let mut s = rig.state.borrow_mut();
            s.hold = false;
            s.inbox.extend(held);
        }
        rig.frame(Some("b"), NONE);
        rig.frame(Some("b"), NONE);
        assert_eq!(rig.playing().as_deref(), Some("mati"));
        assert_eq!(rig.audio.history(), ["ob", "mati"]);
    }

    #[test]
    fn a_slow_start_says_how_long_the_track_was_waited_for() {
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None))], &[("ob", 48_000)]);
        // The script is read at once; the decode of the track it names is the slow part.
        rig.frame(Some("a"), NONE);
        rig.state.borrow_mut().hold = true;
        rig.frame(Some("a"), NONE);
        std::thread::sleep(SLOW_START + std::time::Duration::from_millis(50));
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        {
            let mut s = rig.state.borrow_mut();
            s.hold = false;
            s.inbox.extend(held);
        }
        rig.frame(Some("a"), NONE);
        assert!(rig.lines().iter().any(|l| l.starts_with("playing ob (field, waited ")), "{:?}", rig.lines());
    }

    #[test]
    fn a_missing_track_is_one_line_and_silence_and_the_rest_carries_on() {
        // No file for the field's track, none for the fanfare.
        let mut rig = Rig::new(&[("a", area(Some("ghost"), None, None))], &[("bat", 4_800)]);
        for _ in 0..4 {
            rig.frame(Some("a"), NONE);
        }
        assert_eq!(rig.playing(), None);
        let missing = |rig: &Rig, name: &str| rig.lines().iter().filter(|l| l.contains(&format!("cannot play the track {name} "))).count();
        assert_eq!(missing(&rig, "ghost"), 1);
        // The fight still has its music, and a win without a fanfare file just goes on.
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        assert_eq!(rig.playing().as_deref(), Some("bat"));
        rig.frame(Some("a"), counts(false, 1, 1, 1));
        rig.frame(Some("a"), counts(false, 1, 1, 1));
        assert_eq!(missing(&rig, "fanfare"), 1);
        assert_eq!(rig.playing().as_deref(), Some("bat"), "no field track and no fanfare: the battle music is left alone");
        // Asking again does not ask the worker again.
        let decodes = rig.state.borrow().asked.iter().filter(|a| a.starts_with("decode ghost")).count();
        assert_eq!(decodes, 1);
    }

    #[test]
    fn a_track_the_cache_let_go_of_is_decoded_again_when_it_is_wanted_again() {
        // Six fields, six tracks, and every area also prefetches the fight's theme and the fanfare: more
        // than the cache holds, so the first field's track is gone by the time we come back to it.
        let names = ["t_a", "t_b", "t_c", "t_d", "t_e", "t_f"];
        let areas: Vec<(&str, AreaMusic)> = ["a", "b", "c", "d", "e", "f"].iter().zip(names).map(|(k, t)| (*k, area(Some(t), None, None))).collect();
        let tracks: Vec<(&str, usize)> = names.iter().map(|t| (*t, 4_800)).chain([("bat", 4_800), ("fanfare", 2_400)]).collect();
        let mut rig = Rig::new(&areas, &tracks);
        for key in ["a", "b", "c", "d", "e", "f"] {
            for _ in 0..3 {
                rig.frame(Some(key), NONE);
            }
        }
        assert_eq!(rig.playing().as_deref(), Some("t_f"));
        assert!(!rig.audio.live.as_ref().expect("on").cache.contains("t_a"), "the cache holds {CACHE_TRACKS} tracks and t_a is the oldest");
        for _ in 0..4 {
            rig.frame(Some("a"), NONE);
        }
        assert_eq!(rig.playing().as_deref(), Some("t_a"), "back in the first field its music plays again");
        let decodes = rig.state.borrow().asked.iter().filter(|a| a.starts_with("decode t_a")).count();
        assert_eq!(decodes, 2, "decoded once on the first visit and once more on the return");
    }

    #[test]
    fn the_fanfare_plays_again_after_every_win_even_when_no_field_music_comes_back() {
        // A field that names no music: after the fanfare there is nothing to go back to, and the next win
        // must still be heard.
        let mut rig = Rig::new(&[("a", area(None, None, None))], &[("bat", 4_800), ("fanfare", 2_400)]);
        for _ in 0..3 {
            rig.frame(Some("a"), NONE);
        }
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        rig.frame(Some("a"), counts(false, 1, 1, 1));
        assert_eq!(rig.playing().as_deref(), Some("fanfare"));
        for _ in 0..8 {
            rig.audio.pump(1_000);
            rig.audio.update(Some("a"), counts(false, 1, 1, 1));
        }
        rig.audio.pump(64);
        assert_eq!(rig.playing(), None, "the jingle is over and the field has no music");
        rig.frame(Some("a"), counts(true, 2, 1, 1));
        rig.frame(Some("a"), counts(false, 2, 2, 2));
        assert_eq!(rig.playing().as_deref(), Some("fanfare"), "the second win is heard too");
        assert_eq!(rig.audio.history(), ["bat", "fanfare", "bat", "fanfare"]);
    }

    #[test]
    fn a_track_asked_for_in_advance_is_asked_for_again_as_urgent_when_it_is_wanted_now() {
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None))], &[("ob", 4_800), ("bat", 4_800), ("fanfare", 2_400)]);
        // The answers are held back, as slow decodes would hold them: `bat` and `fanfare` are asked for in
        // advance when the script is read.
        rig.frame(Some("a"), NONE);
        rig.state.borrow_mut().hold = true;
        rig.frame(Some("a"), NONE);
        let asked = rig.state.borrow().asked.clone();
        assert!(asked.contains(&"decode bat".to_string()), "{asked:?}");
        // A fight starts before they arrive: the theme is wanted now, so the worker is told it is urgent...
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        let asked = rig.state.borrow().asked.clone();
        assert!(asked.contains(&"decode bat (urgent)".to_string()), "{asked:?}");
        // ...once, however many frames the fight lasts.
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        let urgent = rig.state.borrow().asked.iter().filter(|a| *a == "decode bat (urgent)").count();
        assert_eq!(urgent, 1);
    }

    #[test]
    fn a_fight_before_the_fields_script_was_read_still_has_its_theme() {
        let mut rig = Rig::new(&[], &[("bat", 4_800), ("fanfare", 2_400)]);
        rig.frame(None, counts(true, 1, 0, 0));
        rig.frame(None, counts(true, 1, 0, 0));
        assert_eq!(rig.playing().as_deref(), Some("bat"));
    }

    #[test]
    fn menu_sounds_play_once_loaded_and_never_before() {
        let mut rig = Rig::new(&[], &[]);
        // The cues come back with the first poll.
        rig.audio.cue(Cue::Cursor);
        rig.audio.pump(10);
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0, "nothing is loaded before the first update");
        rig.frame(None, NONE);
        rig.audio.cue(Cue::Confirm);
        rig.audio.pump(10);
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 1);
        assert!(rig.lines().iter().any(|l| l == "menu sounds: 4 loaded"), "{:?}", rig.lines());
        // A machine without the sound files: one line, and cues are silent.
        let mut quiet = Rig::with_cues(&[], &[], false);
        quiet.frame(None, NONE);
        quiet.audio.cue(Cue::Cursor);
        quiet.audio.pump(10);
        assert_eq!(quiet.audio.status().expect("on").sfx_playing, 0);
        assert_eq!(quiet.lines().iter().filter(|l| l.starts_with("no menu sounds")).count(), 1);
    }

    #[test]
    fn mute_and_volume_change_the_engine_and_say_so() {
        let mut rig = Rig::new(&[], &[]);
        assert_eq!(rig.audio.status().expect("on").volumes.master, 0.8);
        assert_eq!(rig.audio.toggle_mute(), Some(true));
        assert!(rig.audio.status().expect("on").muted);
        assert_eq!(rig.audio.toggle_mute(), Some(false));
        assert_eq!(rig.audio.change_volume(true), Some(90));
        assert_eq!(rig.audio.change_volume(true), Some(100));
        assert_eq!(rig.audio.change_volume(true), Some(100), "no gain above unity");
        assert!((rig.audio.status().expect("on").volumes.master - 1.0).abs() < 1e-6);
        for _ in 0..12 {
            rig.audio.change_volume(false);
        }
        assert_eq!(rig.audio.status().expect("on").volumes.master, 0.0);
        assert_eq!(rig.audio.change_volume(false), Some(0));
        assert!(rig.lines().contains(&"muted".to_string()) && rig.lines().contains(&"volume 90%".to_string()));
    }

    #[test]
    fn a_soundtrack_that_is_off_does_nothing_and_a_muted_start_builds_nothing() {
        let mut off = GameAudio::off();
        assert!(!off.is_on());
        off.update(Some("md1stin"), NONE);
        off.cue(Cue::Cursor);
        assert_eq!(off.toggle_mute(), None);
        assert_eq!(off.change_volume(true), None);
        assert_eq!(off.status(), None);
        assert!(off.history().is_empty());
        assert_eq!(off.now_playing(), None);
        // `--mute` and `FMV_AUDIO=off` are Mode::Off: no engine, no worker threads.
        let tasks = Tasks {
            analyze: Arc::new(|_| panic!("a muted run must not read a script")),
            decode: Arc::new(|_| panic!("a muted run must not decode")),
            cues: Arc::new(|| panic!("a muted run must not read the sound files")),
        };
        let muted = GameAudio::start(AudioConfig { mode: Mode::Off, ..AudioConfig::default() }, tasks);
        assert!(!muted.is_on());
        assert!(GameAudio::default().live.is_none());
    }

    #[test]
    fn the_run_decides_whether_it_has_sound() {
        // A window: automatic unless told otherwise.
        assert_eq!(run_mode(false, false, None), (Mode::Auto, None));
        assert_eq!(run_mode(false, false, Some("null")), (Mode::Null, None));
        assert_eq!(run_mode(false, false, Some("off")), (Mode::Off, None));
        // `--mute` wins over everything.
        for env in [None, Some("auto"), Some("null")] {
            assert_eq!(run_mode(true, false, env), (Mode::Off, None), "{env:?}");
            assert_eq!(run_mode(true, true, env), (Mode::Off, None), "{env:?}");
        }
        // A headless bot is silent unless FMV_AUDIO was set on purpose.
        assert_eq!(run_mode(false, true, None), (Mode::Off, None));
        assert_eq!(run_mode(false, true, Some("null")), (Mode::Null, None));
        assert_eq!(run_mode(false, true, Some("auto")), (Mode::Auto, None));
        assert_eq!(run_mode(false, true, Some("off")), (Mode::Off, None));
        // Nonsense is automatic, with a note to show.
        let (mode, note) = run_mode(false, false, Some("loud"));
        assert_eq!(mode, Mode::Auto);
        assert!(note.is_some_and(|n| n.contains("loud")));
    }

    #[test]
    fn track_names_are_the_music_idx_spelling() {
        for ok in ["bat", "fan2", "kurai", "a_b-c", "0"] {
            assert!(is_track_name(ok), "{ok}");
        }
        for bad in ["", "Bat", "a b", "../x", "x.ogg", &"x".repeat(65)] {
            assert!(!is_track_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_cache_keeps_the_recent_tracks_and_drops_the_oldest() {
        let mut cache = Cache { entries: Vec::new() };
        for n in 0..CACHE_TRACKS {
            cache.insert(format!("t{n}"), track(10));
        }
        assert!(cache.get("t0").is_some(), "touching t0 makes it the newest");
        cache.insert("new".into(), track(10));
        assert!(cache.contains("t0") && cache.contains("new"));
        assert!(!cache.contains("t1"), "the least recently used is the one dropped");
        assert_eq!(cache.entries.len(), CACHE_TRACKS);
        cache.insert("new".into(), track(20));
        assert_eq!(cache.entries.len(), CACHE_TRACKS, "a track is held once");
        assert_eq!(cache.get("new").map(|m| m.frames()), Some(20));
        assert!(cache.get("nobody").is_none());
    }

    #[test]
    fn engine_ids_are_stable_per_track_name() {
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None))], &[("ob", 4_800), ("bat", 4_800), ("fanfare", 2_400)]);
        for _ in 0..3 {
            rig.frame(Some("a"), NONE);
        }
        rig.frame(Some("a"), counts(true, 1, 0, 0));
        rig.frame(Some("a"), counts(false, 1, 1, 0));
        // ob, bat, ob: the third play is the first one's id again.
        assert_eq!(rig.audio.id_of("ob"), Some(TrackId(1)));
        assert_eq!(rig.audio.id_of("bat"), Some(TrackId(2)));
        assert_eq!(rig.audio.id_of("nobody"), None);
    }
}
