//! The client's sound: the field's music, the battle music, the victory fanfare, the menu sound
//! effects and the sounds of a battle's blows, played through [`fmv_audio`] and decided from what the game is doing.
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
//! * **Battle sounds.** Each performance of a battle plays the sound the game's own data names for it, at the
//!   instant it lands (`crate::battle::sounds`: a weapon's hit, critical hit and miss, an enemy attack's impact). When a
//!   battle starts [`GameAudio::follow_battle`] asks the [`Worker`] for the clips this battle can need (only those, by
//!   the game's sound id, in the urgent lane), and every frame it asks the battle for the sounds that came due and plays
//!   them with the effects' volume path: they share the master volume, the mute key and `--mute`/`FMV_AUDIO` with the rest.
//!   `FMV_BATTLE_SOUNDS` says which ones are played: `off` none, `data` (the default) what the data names, `all` also
//!   the two that only a hand-made list of the sound bank names (an enemy's death, an escape; see [`BattleSounds`]).
//! * **Scene sounds.** Literal SOUND commands follow the local scene clock on cancellable channel 1.
//!   Stops, replacement and scene end cancel pending decodes; other channels keep playing.
//! * **Field sounds.** What the field's own scripts play for the player's story ([`field`]): ambient loops (the hum of
//!   the reactor before the boss, its alarm after it), and the sounds of the ladders, jumps, lift and doors the player uses.
//!   When the player is in an FF7 field [`GameAudio::follow_field`] (every frame, from [`GameAudio::update_from`]) asks the
//!   [`Worker`] for the field's plan (what plays for the story the client holds, and only the clips that names), starts the
//!   loops on their channels, asks again when a `StoryUpdate` changes the story, pauses the loops for a battle and plays the
//!   event sounds when it sees their evidence. `FMV_FIELD_SOUNDS=off|data` ([`FieldSounds`]): `off` asks for nothing and
//!   plays nothing.
//!
//! What the log says (it is the evidence of a headless run, which has no ears): one `audio: field
//! <area>:` line per area with the tracks its script names, `audio: decoded <track>` as each
//! finishes, `audio: playing <track> (<why>)` at the moment a track really starts, `audio: effect sounds: N loaded`
//! when a fight's clips were read, `audio: battle sound <id> (<hit|critical|miss|impact|...>)` at the moment each one
//! plays and, when the run ends, `audio: battle sounds played: N (<id>x<times> ...)`. For the field's sounds: `audio: field
//! <area> sounds (moment <m>): ambient ch3:38; ...` when a plan is read, `audio: field loop start ch3 38` and `... stop ...
//! (<why>)`, `audio: field sound ch1 62 (link)` and, at the end, `audio: field sounds played: ...`.

mod cues;
mod director;
pub mod ff7;
pub mod field;
mod scene;
mod worker;

pub use cues::{battle_cue, menu_cue, Cue};
pub use director::BattleCounts;
pub use field::{FieldFrame, FieldSounds, LinkSpot, FIELD_SOUNDS_ENV};

use crate::battle::{BattleClient, BattleSound, Planned};
use director::{Director, STANDARD_BATTLE_TRACK};
use worker::{Done, Tasks, ThreadWorker, VoiceRequest, Worker};
use crate::mods::VoiceCue;
use crate::session::Session;
use fmv_audio::{AudioConfig, AudioEngine, DecodedMusic, Logger, Mode, PlayOptions, SfxClip, SfxHandle, TrackId};
use fmv_core::GameId;
use fmv_mods::echo::short_voice_name;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Decoded tracks kept at once (the playing one, the next fight's, the fanfare, and a little slack): about
/// 35 MB for a three-minute theme.
const CACHE_TRACKS: usize = 5;
/// Volume steps from silence to full: the keys move the master level by a tenth.
const VOLUME_STEPS: u8 = 10;
/// A track that had to be waited for this long gets the wait mentioned in the line that says it started.
const SLOW_START: std::time::Duration = std::time::Duration::from_millis(100);
/// The environment variable that names another track for the victory fanfare (see `main::start_audio`).
pub const FANFARE_ENV: &str = "FMV_AUDIO_FANFARE";
/// The environment variable that says which battle sounds play (see [`BattleSounds`]).
pub const BATTLE_SOUNDS_ENV: &str = "FMV_BATTLE_SOUNDS";
/// How loud a battle sound plays, `0.0..=1.0` (before the master volume, which it shares with everything else).
const BATTLE_GAIN: f32 = 0.8;
/// How loud a battle's ambience from the sound layer plays, before the master volume (as loud as a field's).
const BATTLE_AMBIENCE_GAIN: f32 = 0.6;
/// A battle sound that came due this long ago or more is not played: a window that stalled does not play a burst.
const STALE_SOUND_S: f64 = 0.5;
/// Clips of battle sounds kept at once (a fight needs a dozen or so; the oldest go first).
const BATTLE_CLIPS: usize = 64;
/// How loud a voice plays, `0.0..=1.0` (before the master volume, which it shares with everything else).
const VOICE_GAIN: f32 = 1.0;
/// A voice that is cut (the next window opens, the conversation ends) fades out over this.
const VOICE_CUT: Duration = Duration::from_millis(60);
/// A voice's loop handle is stopped this long after its clip ended (the clip's silent tail loops meanwhile).
const VOICE_GRACE: Duration = Duration::from_millis(250);

/// The voice layer's state: which ask is current, the voice that plays and what the run did.
/// A battle's ambience (see [`Live::on_battle_ambience`]): the formation of the fight it was asked for, and the loop playing it
/// with its section (for the log) and its fade out.
#[derive(Default)]
struct BattleAmbience {
    formation: Option<u16>,
    playing: Option<(fmv_audio::SfxHandle, String, std::time::Duration)>,
}

#[derive(Default)]
struct VoiceState {
    /// The ask in flight or playing: the answer to any other is thrown away (a newer window opened, or the conversation ended).
    seq: u64,
    /// The playing voice: the handle of the loop that carries it (the engine's one-shots cannot be stopped one by one) and when its
    /// clip is over.
    playing: Option<(SfxHandle, Instant)>,
    /// Voices that started, and asks that found no clip.
    played: u32,
    silent: u32,
}

/// Which battle sounds are played (`FMV_BATTLE_SOUNDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BattleSounds {
    /// None.
    Off,
    /// What the game's data names: a weapon's hit, critical hit and miss, an enemy attack's impact (the default).
    #[default]
    Data,
    /// Also the two sounds only a hand-made list of the sound bank names, an enemy's death and an escape (identified by
    /// ear, and nothing says when the game plays them: a boss has a death clip of its own).
    All,
}

impl BattleSounds {
    /// The choice `FMV_BATTLE_SOUNDS` (`value`) makes: `off` (or `0`, `no`, `false`, `none`), `data` (or `on`, `1`, `yes`,
    /// `true`; also what an unset or empty one is) or `all` (or `more`). The second value is a note about one that makes no
    /// sense (which is `data`).
    pub fn from_env(value: Option<&str>) -> (Self, Option<String>) {
        let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else { return (Self::Data, None) };
        match raw.to_ascii_lowercase().as_str() {
            "off" | "0" | "no" | "false" | "none" => (Self::Off, None),
            "data" | "on" | "1" | "yes" | "true" => (Self::Data, None),
            "all" | "more" => (Self::All, None),
            _ => (Self::Data, Some(format!("{BATTLE_SOUNDS_ENV}={raw:?} is not off, data or all: playing what the game's data names"))),
        }
    }

    /// Whether a sound plays (`by_ear`: named only by the hand-made list, see [`crate::battle::sounds::Why::by_ear`]).
    fn plays(self, by_ear: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Data => !by_ear,
            Self::All => true,
        }
    }
}

/// The clips of the battle sounds read so far, by the game's sound id; the oldest are let go past [`BATTLE_CLIPS`].
#[derive(Default)]
struct Clips {
    order: VecDeque<u16>,
    clips: HashMap<u16, SfxClip>,
}

impl Clips {
    fn get(&self, id: u16) -> Option<&SfxClip> {
        self.clips.get(&id)
    }

    /// Keep `clip`; the ids of the clips that had to go to make room.
    fn insert(&mut self, id: u16, clip: SfxClip) -> Vec<u16> {
        if self.clips.insert(id, clip).is_none() {
            self.order.push_back(id);
        }
        let mut gone = Vec::new();
        while self.order.len() > BATTLE_CLIPS {
            if let Some(old) = self.order.pop_front() {
                self.clips.remove(&old);
                gone.push(old);
            }
        }
        gone
    }
}

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
    /// Which battle sounds play.
    battle_sounds: BattleSounds,
    /// The clips of the battle sounds that were read.
    battle_clips: Clips,
    /// The sound ids asked of the worker (read, being read, or without a clip): never asked for again, until the clip is let go.
    sounds_asked: HashSet<u16>,
    /// Battles seen start (the battle counter at the last look): a new one asks for its clips.
    battles_seen: u64,
    /// The battle's ambience from the sound layer: the formation it was asked for (the fight on now) and the loop playing it.
    battle_ambience: BattleAmbience,
    /// The time the battle's due sounds were last asked for up to (the next look asks from here).
    polled_to: Option<f64>,
    /// How many times each battle sound id was played (the closing line of the run, and tests).
    battle_played: BTreeMap<u16, u32>,
    /// Ids that came due with no clip to play, said once each.
    unheard: HashSet<u16>,
    /// What the field plays (see [`field`]).
    field: field::FieldState,
    scene_audio: scene::SceneAudio,
    /// The area whose links the field state holds.
    links_for: Option<String>,
    /// The voice layer (see [`VoiceState`]).
    voice: VoiceState,
    /// Whether this soundtrack takes the conversations' voice cues from the process's queue ([`crate::mods::take_voices`]): the one
    /// [`GameAudio::start`] makes does, the ones the tests assemble do not (the queue is the process's, and tests run side by side).
    takes_global_voices: bool,
}

impl Live {
    /// Stop the voice that plays, if one does (fading out over [`VOICE_CUT`]).
    fn stop_voice(&mut self) {
        if let Some((handle, _)) = self.voice.playing.take() {
            self.engine.stop_sfx_loop(handle, VOICE_CUT);
        }
    }

    /// Let go of the loop of a voice whose clip is over (a one-shot cannot be stopped one by one, so a voice is a loop whose
    /// last part is silence; the engine has eight of those at once, which a voice must not keep).
    fn expire_voice(&mut self) {
        if self.voice.playing.is_some_and(|(_, until)| Instant::now() >= until) {
            if let Some((handle, _)) = self.voice.playing.take() {
                self.engine.stop_sfx_loop(handle, Duration::ZERO);
            }
        }
    }

    /// A voice clip arrived for ask `seq`: play it if that is still the ask, else it is thrown away.
    /// The worker's answer for a battle's ambience: it plays if it is for the fight on now and none plays yet.
    fn on_battle_ambience(&mut self, formation: u16, ambience: Option<Box<field::ModAmbience>>) {
        if self.battle_ambience.formation != Some(formation) || self.battle_ambience.playing.is_some() {
            return;
        }
        let Some(a) = ambience else { return };
        let handle = self.engine.play_sfx_loop(&a.clip, BATTLE_AMBIENCE_GAIN, 0.0, a.fade_in);
        self.say(&format!("battle ambience start {} from {} ({})", a.section, a.source, a.name));
        self.battle_ambience.playing = Some((handle, a.section, a.fade_out));
    }

    /// The fight is over (or another began): its ambience fades out with its own fade.
    fn stop_battle_ambience(&mut self, why: &str) {
        self.battle_ambience.formation = None;
        if let Some((handle, section, fade)) = self.battle_ambience.playing.take() {
            self.engine.stop_sfx_loop(handle, fade);
            self.say(&format!("battle ambience stop {section} ({why})"));
        }
    }

    fn on_voice(&mut self, seq: u64, result: Result<Option<worker::VoiceClip>, String>) {
        if seq != self.voice.seq {
            return;
        }
        match result {
            Ok(Some(found)) => {
                self.stop_voice();
                let handle = self.engine.play_sfx_loop(&found.clip, VOICE_GAIN, 0.0, Duration::ZERO);
                self.voice.playing = Some((handle, Instant::now() + found.length + VOICE_GRACE));
                self.voice.played += 1;
                self.say(&format!("voice {} ({:.1} s)", short_voice_name(&found.name), found.length.as_secs_f64()));
            }
            Ok(None) => self.voice.silent += 1,
            Err(why) => {
                self.voice.silent += 1;
                self.say(&format!("voice: {why}; going on without it"));
            }
        }
    }

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
                Done::Sounds { read, took } => {
                    let loaded = read.clips.len();
                    for (id, clip) in read.clips {
                        // A clip let go to make room may be asked for again by a later battle.
                        for gone in self.battle_clips.insert(id, clip) {
                            self.sounds_asked.remove(&gone);
                        }
                    }
                    for (id, why) in &read.missing {
                        self.say(&format!("no clip for effect sound {id} ({why}); going on without it"));
                    }
                    self.say(&format!("effect sounds: {loaded} loaded in {} ms", took.as_millis()));
                }
                Done::Field { key, story, read, took } => self.field.on_read(&self.log, &key, &story, read, took),
                Done::Voice { seq, result } => self.on_voice(seq, result),
                Done::BattleAmbience { formation, ambience } => self.on_battle_ambience(formation, ambience),
            }
        }
    }

    /// What the battle sounds of this run came to, `None` when there were none: `86 (16x39 18x12 46x1 227x34)`, each sound id
    /// and how many times it played (the closing line of a run that heard any).
    fn battle_sounds_played(&self) -> Option<String> {
        let total: u32 = self.battle_played.values().sum();
        let ids = self.battle_played.iter().map(|(id, n)| format!("{id}x{n}")).collect::<Vec<_>>().join(" ");
        (total > 0).then(|| format!("{total} ({ids})"))
    }

    /// Ask the worker for the clips of the sounds in `plan` that this run plays and has not asked for yet.
    fn request_battle_sounds(&mut self, plan: &[Planned]) {
        let wanted: Vec<u16> = plan
            .iter()
            .filter(|p| self.battle_sounds.plays(p.by_ear))
            .map(|p| p.id)
            .filter(|id| !self.sounds_asked.contains(id))
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.sounds_asked.extend(wanted.iter().copied());
        self.worker.load_sounds(wanted);
    }

    /// Play a battle sound that came due at `at` (it is `now`), unless this run does not play that kind, it is long over or
    /// there is no clip for it (said once).
    fn play_battle_sound(&mut self, at: f64, now: f64, sound: BattleSound) {
        if !self.battle_sounds.plays(sound.why.by_ear()) || now - at > STALE_SOUND_S {
            return;
        }
        let Some(clip) = self.battle_clips.get(sound.id) else {
            if self.unheard.insert(sound.id) {
                self.say(&format!("battle sound {} ({}) has no clip to play", sound.id, sound.why.name()));
            }
            return;
        };
        self.engine.play_sfx(clip, BATTLE_GAIN, 0.0);
        *self.battle_played.entry(sound.id).or_default() += 1;
        self.say(&format!("battle sound {} ({})", sound.id, sound.why.name()));
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

impl Drop for Live {
    /// The end of a run: what its battles and its fields sounded like, in a line each (the evidence of a headless run, which has
    /// no ears).
    fn drop(&mut self) {
        if let Some(played) = self.battle_sounds_played() {
            self.say(&format!("battle sounds played: {played}"));
        }
        if let Some(played) = self.field.summary() {
            self.say(&format!("field sounds played: {played}"));
        }
        if self.voice.played + self.voice.silent > 0 {
            self.say(&format!("voices played: {} (asks without a clip: {})", self.voice.played, self.voice.silent));
        }
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
        let (battle_sounds, note) = BattleSounds::from_env(std::env::var(BATTLE_SOUNDS_ENV).ok().as_deref());
        if let Some(note) = note {
            log(&note);
        }
        let (field_sounds, note) = FieldSounds::from_env(std::env::var(FIELD_SOUNDS_ENV).ok().as_deref());
        if let Some(note) = note {
            log(&note);
        }
        let mut audio = Self::with_parts(engine, Box::new(ThreadWorker::start(tasks)), log).with_battle_sounds(battle_sounds).with_field_sounds(field_sounds);
        // The soundtrack of the running game is the one that hears the conversations.
        if let Some(live) = audio.live.as_mut() {
            live.takes_global_voices = true;
        }
        audio
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
                battle_sounds: BattleSounds::default(),
                battle_clips: Clips::default(),
                sounds_asked: HashSet::new(),
                battles_seen: 0,
                battle_ambience: BattleAmbience::default(),
                polled_to: None,
                battle_played: BTreeMap::new(),
                unheard: HashSet::new(),
                field: field::FieldState::new(FieldSounds::default()),
                scene_audio: scene::SceneAudio::default(),
                links_for: None,
                voice: VoiceState::default(),
                takes_global_voices: false,
            })),
        }
    }

    /// Play `which` field sounds (the default is what the data names, see [`FieldSounds`]); no effect on a soundtrack that is
    /// off.
    pub fn with_field_sounds(mut self, which: FieldSounds) -> Self {
        if let Some(live) = self.live.as_mut() {
            live.field = field::FieldState::new(which);
        }
        self
    }

    /// Play `which` battle sounds (the default is what the data names, see [`BattleSounds`]); no effect on a soundtrack that
    /// is off.
    pub fn with_battle_sounds(mut self, which: BattleSounds) -> Self {
        if let Some(live) = self.live.as_mut() {
            live.battle_sounds = which;
        }
        self
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
        // What the conversations asked of the voice layer since the last frame.
        if self.live.as_ref().is_some_and(|l| l.takes_global_voices) {
            for cue in crate::mods::take_voices() {
                self.voice(cue);
            }
        }
        let Some(live) = self.live.as_mut() else { return };
        live.pump_worker();
        live.expire_voice();
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
        self.follow_battle(&session.state.battle, session.now());
        self.follow_session_field(session);
    }

    /// [`GameAudio::follow_field`] for a joined session: the field the player is in, the story the client holds, where the
    /// player stands and how many `MoveRejected` it has had. The links of a new area (and the place the server puts a player at when
    /// each fires) are taken from the session's area when it changes.
    fn follow_session_field(&mut self, session: &Session) {
        let Some(live) = self.live.as_mut() else { return };
        if live.field.mode() == FieldSounds::Off {
            return;
        }
        let area = &session.info.area_id;
        let key = (area.game == GameId::Ff7).then_some(area.key.as_str());
        if live.links_for.as_deref() != key {
            live.links_for = key.map(str::to_string);
            let areas = session.area.as_ref();
            let spots = areas
                .links()
                .into_iter()
                .map(|link| LinkSpot { spot: areas.place_arrival(link.to, link.to_hint), line: link.line, to: link.to, needs_ok: link.needs_ok })
                .collect();
            live.field.set_links(spots);
        }
        // The field is the battle's until its end panel is gone too (the window freezes the field for as long): the loops come
        // back then, when the story the win moved has long been told.
        let now = session.now();
        let battle = &session.state.battle;
        let frame = FieldFrame {
            now,
            key,
            story: session.state.story.progress(),
            pos: Some(session.state.own.pos()),
            rejected: session.state.rejected(),
            in_battle: battle.in_battle() || battle.lingering(now),
        };
        self.follow_field(&frame);
    }

    /// Follow the field's own sounds for one frame (see [`field`]): leave and enter fields, ask for plans, start, change and stop
    /// ambient loops as the story moves, pause them for a battle, and play the sounds of the ladders, jumps, lift and doors the player
    /// uses. Cheap, never blocks; with `FMV_FIELD_SOUNDS=off` it returns at once.
    pub fn follow_field(&mut self, frame: &FieldFrame<'_>) {
        let Some(live) = self.live.as_mut() else { return };
        if live.field.mode() == FieldSounds::Off {
            return;
        }
        live.pump_worker();
        let Live { engine, worker, log, field, .. } = &mut **live;
        field.follow(engine, worker.as_mut(), log, frame);
    }

    /// Drain this client's local scene cues, including on silent runs, without blocking the scene.
    pub fn follow_scene(&mut self, scene: &mut crate::scene::SceneClient, now: f64) {
        let commands = scene.take_sounds();
        let Some(live) = self.live.as_mut() else { return };
        live.scene_sounds(commands, now);
    }

    /// The loops the field has playing, as `(channel, sound id)` (tests).
    #[cfg(test)]
    pub fn field_loops(&self) -> Vec<(u8, u16)> {
        self.live.as_ref().map_or_else(Vec::new, |l| l.field.playing())
    }

    /// Follow a battle's sounds at `now` (seconds on the clock its messages were stamped with, [`Session::now`]): a battle
    /// that has started asks the worker for the clips it can need (once), and the sounds that came due since the last look
    /// are played, each at the moment its performance lands. Cheap, never blocks; call it every frame. A sound due after the
    /// battle ended (the killing blow) is still played: only the clips being loaded gate it.
    pub fn follow_battle(&mut self, battle: &BattleClient, now: f64) {
        let Some(live) = self.live.as_mut() else { return };
        if live.battle_sounds == BattleSounds::Off {
            return;
        }
        live.pump_worker();
        if battle.entered() > live.battles_seen {
            live.battles_seen = battle.entered();
            live.request_battle_sounds(&battle.sound_plan());
            live.stop_battle_ambience("another battle began");
            if let Some(formation) = battle.formation() {
                live.battle_ambience.formation = Some(formation);
                live.worker.load_battle_ambience(formation);
            }
        }
        if !battle.in_battle() && live.battle_ambience.formation.is_some() {
            live.stop_battle_ambience("the battle ended");
        }
        let from = live.polled_to.replace(now).unwrap_or(now);
        for (at, sound) in battle.sounds_due(from, now) {
            live.play_battle_sound(at, now, sound);
        }
    }

    /// A conversation's voice cue (see [`VoiceCue`]): the voice that plays stops (a window moved on or closed), and a window that
    /// opened asks the worker for its clip, which plays when it arrives unless something newer came first. A conversation without
    /// a clip for the window is silent; a run without sound drops the cue.
    pub fn voice(&mut self, cue: VoiceCue) {
        let Some(live) = self.live.as_mut() else { return };
        live.stop_voice();
        live.voice.seq = live.voice.seq.wrapping_add(1);
        if !matches!(cue, VoiceCue::Stop) {
            live.worker.load_voice(VoiceRequest { seq: live.voice.seq, cue });
        }
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
    use super::field::{AmbientLoop, ExitPlan, FlipPlan, LinkPlan, PlannedCall};
    use super::*;
    use fmv_ff7::story::StoryState;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    const RATE: u32 = 48_000;

    /// A track of `frames` frames of a constant (never zero, so that it can be told from silence).
    fn track(frames: usize) -> DecodedMusic {
        DecodedMusic::from_stereo(RATE, vec![8_000; frames * 2], None).expect("a track")
    }

    /// What a field plays for a story blob.
    type PlanOf = Rc<dyn Fn(&[u8]) -> field::FieldPlan>;

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
        /// The sound ids the library has no clip for (an empty slot, say); every other id has one.
        no_clip: Vec<u16>,
        /// What each field plays, as a function of the story blob it is asked for; a field not listed plays nothing.
        fields: HashMap<String, PlanOf>,
        /// The ids whose clips loop (their loop region is the whole clip).
        loop_ids: Vec<u16>,
        /// The sound layer's ambience of a field, by key: its section and its fades in and out (seconds).
        ambience: HashMap<String, (String, f32, f32)>,
        /// Fields that cannot be read.
        broken: Vec<String>,
        /// The voice clips the mod layer has, by `<field>/<dialog>` (or `<field>/<dialog>_<line>`), with their length in frames.
        voices: HashMap<String, usize>,
        /// The battle ambiences the sound layer has, by formation: their sections.
        battle_ambience: HashMap<u16, String>,
    }

    /// A worker that answers on the spot (or when told to): no threads, no waiting.
    struct Fake(Rc<RefCell<State>>);

    impl Worker for Fake {
        fn load_battle_ambience(&mut self, formation: u16) {
            let mut s = self.0.borrow_mut();
            s.asked.push(format!("battle ambience {formation}"));
            let ambience = s.battle_ambience.get(&formation).map(|section| Box::new(field::ModAmbience {
                section: section.clone(),
                name: format!("Ambience\\Ambient\\{section}.ogg"),
                source: "cosmo",
                clip: SfxClip::from_mono(RATE, &vec![0.25; 4_800]).expect("a clip"),
                fade_in: std::time::Duration::from_millis(500),
                fade_out: std::time::Duration::from_millis(500),
            }));
            let done = Done::BattleAmbience { formation, ambience };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
        }

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

        fn load_sounds(&mut self, ids: Vec<u16>) {
            let mut s = self.0.borrow_mut();
            s.asked.push(format!("sounds {ids:?}"));
            let mut read = worker::SoundsRead::default();
            for id in ids {
                if s.no_clip.contains(&id) {
                    read.missing.push((id, "an empty slot".to_string()));
                } else {
                    // A clip of a tenth of a second at the rate of the engine, never silent.
                    read.clips.push((id, SfxClip::from_mono(RATE, &vec![0.5; 4_800]).expect("a clip")));
                }
            }
            let done = Done::Sounds { read, took: std::time::Duration::from_millis(3) };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
        }

        fn scan_field(&mut self, key: &str, story: Vec<u8>, have: Vec<u16>) {
            let mut s = self.0.borrow_mut();
            let moment = StoryState::decode(&story).map_or_else(|| "?".to_string(), |st| st.moment.to_string());
            s.asked.push(format!("field {key} moment {moment} have {have:?}"));
            let read = if s.broken.iter().any(|b| b == key) {
                Err("the field cannot be read".to_string())
            } else {
                let mut plan = s.fields.get(key).map(|f| f(&story)).unwrap_or_default();
                plan.key = key.to_string();
                plan.story = story.clone();
                let mut clips = worker::SoundsRead::default();
                for id in plan.ids().into_iter().filter(|id| !have.contains(id)) {
                    if s.no_clip.contains(&id) {
                        clips.missing.push((id, "an empty slot".to_string()));
                    } else {
                        let clip = SfxClip::from_mono(RATE, &vec![0.5; 4_800]).expect("a clip");
                        clips.clips.push((id, if s.loop_ids.contains(&id) { clip.with_loop(0, 4_800) } else { clip }));
                    }
                }
                let ambience = s.ambience.get(key).map(|(section, fade_in, fade_out)| Box::new(field::ModAmbience {
                    section: section.clone(),
                    name: format!("Ambience\\Ambient\\{section}.ogg"),
                    source: "cosmo",
                    clip: SfxClip::from_mono(RATE, &vec![0.25; 4_800]).expect("a clip"),
                    fade_in: std::time::Duration::from_secs_f32(*fade_in),
                    fade_out: std::time::Duration::from_secs_f32(*fade_out),
                }));
                Ok(field::FieldRead { plan, clips, ambience })
            };
            let done = Done::Field { key: key.to_string(), story, read, took: std::time::Duration::from_millis(4) };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
        }

        fn load_voice(&mut self, request: worker::VoiceRequest) {
            let mut s = self.0.borrow_mut();
            s.asked.push(format!("voice {:?}", request.cue));
            let key = match &request.cue {
                VoiceCue::Say { field, dialog, .. } => format!("{field}/{dialog}"),
                VoiceCue::Choice { field, dialog, line } => format!("{field}/{dialog}_{line}"),
                VoiceCue::Stop => String::new(),
            };
            let result = Ok(s.voices.get(&key).map(|&frames| {
                // The voice, then a silent tail the loop handle holds on to (the way the real task builds it).
                const TAIL: usize = 480;
                let mut samples = vec![0.5f32; frames];
                samples.extend(vec![0.0f32; TAIL]);
                let clip = SfxClip::from_mono(RATE, &samples).expect("a clip").with_loop(frames as u64, (frames + TAIL) as u64);
                worker::VoiceClip { name: format!("VO\\Voice\\{}.ogg", key.replace('/', "\\")), clip, length: Duration::from_secs_f64(frames as f64 / f64::from(RATE)) }
            }));
            let done = Done::Voice { seq: request.seq, result };
            if s.hold { s.held.push(done) } else { s.inbox.push(done) }
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
            sounds: Arc::new(|_| panic!("a muted run must not read the sound files")),
            field: Arc::new(|_, _, _| panic!("a muted run must not read a field's sounds")),
            voice: Arc::new(|_| panic!("a muted run must not read a voice")),
            battle_ambience: Some(Arc::new(|_| panic!("a muted run must not read an ambience"))),
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

    // ---------- battle sounds ----------

    use crate::battle::sounds::{SoundBook, ENEMY_DEATH_SOUND, ESCAPE_SOUND};
    use crate::battle::{BattleClient, NameBook};
    use fmv_core::{AccountId, EntityId};
    use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent};
    use fmv_protocol::{BattleId, BattleMember, ServerMsg, SyntheticRules};

    const ME: AccountId = AccountId(11);

    /// A battle of formation 301 (scene 75) with two players (characters 0 and 3: invented weapons 0 and 5) whose machine has a
    /// sound book of invented ids: weapon 0 is hit 18, critical 27, miss 5; scene 75's attack 272 has the impact 15. No rules, so
    /// nothing is replayed and the sounds are scheduled from the events alone.
    fn battle() -> BattleClient {
        let book = SoundBook::for_tests()
            .with_weapon(0, 18, 27, 5)
            .with_weapon(5, 292, 350, 5)
            .with_starting_weapon(0, 0)
            .with_starting_weapon(3, 5)
            .with_scene_attack(75, 272, 15)
            .with_scene_attack(76, 272, 99);
        let mut client = BattleClient::new(ME);
        client.set_rules(None);
        client.set_names(Some(Arc::new(NameBook::from_tables(&[], &[]).with_sounds(book))));
        client.on_message(0.0, start(1, 301));
        client
    }

    /// The message that starts battle `id` of formation `formation` with the two players.
    fn start(id: u64, formation: u16) -> ServerMsg {
        let party = vec![
            BattleMember::player(EntityId(1), ME, "A", 0, 6, 0),
            BattleMember::player(EntityId(2), AccountId(12), "B", 3, 6, 0),
        ];
        ServerMsg::EncounterStart {
            battle_id: BattleId(id),
            formation_id: formation,
            seed: 1,
            party,
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        }
    }

    fn send(client: &mut BattleClient, now: f64, events: Vec<BattleEvent>) {
        client.on_message(now, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 1, events, command: None });
    }

    fn blow(actor: u8, target: u8, critical: bool) -> Vec<BattleEvent> {
        vec![
            BattleEvent::Action { actor: ActorId(actor), action: ActionRef::Attack },
            BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount: 9, critical },
        ]
    }

    /// One frame of the audio at `now`: the worker's answers, the due sounds and a little time on the engine.
    fn look(rig: &mut Rig, client: &BattleClient, now: f64) {
        rig.audio.update(None, BattleCounts::of(client));
        rig.audio.follow_battle(client, now);
        rig.audio.pump(64);
    }

    fn played(rig: &Rig) -> Vec<String> {
        rig.lines().into_iter().filter(|l| l.starts_with("battle sound ") && !l.contains("loaded") && !l.contains("no clip")).collect()
    }

    #[test]
    fn a_battle_asks_for_its_clips_once_when_it_starts_and_plays_each_sound_when_its_blow_lands() {
        let mut rig = Rig::new(&[], &[]);
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        // The clips this fight can need, once: the weapons (5, 18, 27, 292, 350) and the scene's impact (15) (never the two
        // by-ear sounds: they are not played by default).
        let asked: Vec<String> = rig.state.borrow().asked.iter().filter(|a| a.starts_with("sounds")).cloned().collect();
        assert_eq!(asked, ["sounds [5, 15, 18, 27, 292, 350]"], "{asked:?}");
        look(&mut rig, &client, 0.1);
        assert_eq!(rig.state.borrow().asked.iter().filter(|a| a.starts_with("sounds")).count(), 1, "not asked for again");
        assert!(rig.lines().iter().any(|l| l == "effect sounds: 6 loaded in 3 ms"), "{:?}", rig.lines());
        // A blow of player A's arrives at 10.0; it lands half a second later and not before.
        send(&mut client, 10.0, blow(0, 2, false));
        look(&mut rig, &client, 10.0);
        look(&mut rig, &client, 10.2);
        look(&mut rig, &client, 10.49);
        assert!(played(&rig).is_empty(), "nothing at the arrival of the batch: the blow has not landed");
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0);
        look(&mut rig, &client, 10.51);
        assert_eq!(played(&rig), ["battle sound 18 (hit)"]);
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 1, "and the engine has it");
        // Looking again does not play it again.
        look(&mut rig, &client, 10.6);
        look(&mut rig, &client, 11.0);
        assert_eq!(played(&rig), ["battle sound 18 (hit)"]);
        // A critical hit and a miss of the second player, and an enemy's attack: each its own sound.
        send(&mut client, 20.0, blow(1, 2, true));
        send(&mut client, 21.0, vec![BattleEvent::Action { actor: ActorId(0), action: ActionRef::Attack }, BattleEvent::Miss { actor: ActorId(0), target: ActorId(2) }]);
        send(&mut client, 22.0, vec![BattleEvent::Action { actor: ActorId(2), action: ActionRef::EnemyAttack(272) }, BattleEvent::Damage { actor: ActorId(2), target: ActorId(0), amount: 4, critical: false }]);
        for step in 0..=300 {
            look(&mut rig, &client, 20.0 + f64::from(step) * 0.01);
        }
        assert_eq!(played(&rig), ["battle sound 18 (hit)", "battle sound 350 (critical)", "battle sound 5 (miss)", "battle sound 15 (impact)"]);
        // The summary of the tracks is what it was; the end of the run says what the battles sounded like.
        assert!(rig.audio.summary().expect("on").ends_with("tracks started: -"), "{:?}", rig.audio.summary());
        let lines = Arc::clone(&rig.lines);
        drop(rig);
        let said = lines.lock().expect("log").clone();
        assert!(said.iter().any(|l| l == "battle sounds played: 4 (5x1 15x1 18x1 350x1)"), "{said:?}");
    }

    #[test]
    fn a_run_that_heard_no_battle_sound_says_nothing_about_them_when_it_ends() {
        let rig = Rig::new(&[], &[]);
        let lines = Arc::clone(&rig.lines);
        drop(rig);
        assert!(!lines.lock().expect("log").iter().any(|l| l.contains("battle sounds played")));
    }

    #[test]
    fn a_counter_is_heard_after_the_blow_it_answers_and_the_killing_blow_is_heard_after_the_battle_ended() {
        let mut rig = Rig::new(&[], &[]);
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        // One batch: A's blow and the enemy's counter on A; the batch also ends the battle with a message of its own.
        let mut batch = blow(0, 2, false);
        batch.extend([BattleEvent::Action { actor: ActorId(2), action: ActionRef::EnemyAttack(272) }, BattleEvent::Damage { actor: ActorId(2), target: ActorId(0), amount: 99, critical: false }]);
        send(&mut client, 30.0, batch);
        client.on_message(30.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: fmv_protocol::battle::Outcome::Defeat, rewards: Vec::new() });
        assert!(!client.in_battle(), "the battle is over for the mirror");
        let blow_at = 30.0 + crate::battle::scene::cue::impact_delay(Some(ActionRef::Attack));
        let counter_at = 30.0 + crate::battle::scene::cue::length_of(Some(ActionRef::Attack)) + crate::battle::scene::cue::impact_delay(Some(ActionRef::EnemyAttack(272)));
        assert!(blow_at < counter_at);
        let heard_at = |rig: &mut Rig, until: f64| {
            let mut now = (30.0f64).max(until - 0.05);
            while now <= until {
                look(rig, &client, now);
                now += 0.01;
            }
            played(rig)
        };
        look(&mut rig, &client, 30.0);
        assert!(heard_at(&mut rig, blow_at - 0.02).is_empty(), "nothing before the blow lands");
        assert_eq!(heard_at(&mut rig, blow_at + 0.1), ["battle sound 18 (hit)"], "the blow");
        assert_eq!(heard_at(&mut rig, counter_at - 0.05), ["battle sound 18 (hit)"], "the counter has not landed yet");
        assert_eq!(heard_at(&mut rig, counter_at + 0.1), ["battle sound 18 (hit)", "battle sound 15 (impact)"], "the counter comes after the blow, after the end message too");
    }

    #[test]
    fn a_window_that_stalled_does_not_play_a_burst_of_old_sounds() {
        let mut rig = Rig::new(&[], &[]);
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        send(&mut client, 10.0, blow(0, 2, false));
        look(&mut rig, &client, 10.1);
        // The next look is two seconds later: the blow came due 1.5 s ago, which is too late to be heard.
        look(&mut rig, &client, 12.0);
        assert!(played(&rig).is_empty());
        // A fresh one still plays.
        send(&mut client, 12.0, blow(0, 2, false));
        look(&mut rig, &client, 12.6);
        assert_eq!(played(&rig).len(), 1);
    }

    #[test]
    fn the_flag_decides_which_battle_sounds_are_heard_and_asked_for() {
        let events = |client: &mut BattleClient| {
            // A blow that beats an enemy, then an escape: two by-ear sounds (the enemy's death, the escape) and one from the data.
            let mut batch = blow(0, 2, false);
            batch.extend([BattleEvent::Death { actor: ActorId(2) }, BattleEvent::Action { actor: ActorId(1), action: ActionRef::Escape }]);
            send(client, 10.0, batch);
        };
        let run = |which: BattleSounds| {
            let mut rig = Rig::new(&[], &[]);
            rig.audio = std::mem::take(&mut rig.audio).with_battle_sounds(which);
            let mut client = battle();
            look(&mut rig, &client, 0.0);
            events(&mut client);
            for step in 0..=400 {
                look(&mut rig, &client, 10.0 + f64::from(step) * 0.01);
            }
            let asked: Vec<String> = rig.state.borrow().asked.iter().filter(|a| a.starts_with("sounds")).cloned().collect();
            (played(&rig), asked)
        };
        let (heard, asked) = run(BattleSounds::Data);
        assert_eq!(heard, ["battle sound 18 (hit)"], "by default only what the data names");
        assert!(!asked[0].contains(&ESCAPE_SOUND.to_string()) && !asked[0].contains(&ENEMY_DEATH_SOUND.to_string()), "{asked:?}");
        let (heard, asked) = run(BattleSounds::All);
        assert_eq!(heard, ["battle sound 18 (hit)", "battle sound 22 (death)", "battle sound 26 (escape)"], "all: the by-ear ones too, in the order they land");
        assert!(asked[0].contains("22") && asked[0].contains("26"), "{asked:?}");
        let (heard, asked) = run(BattleSounds::Off);
        assert!(heard.is_empty() && asked.is_empty(), "off asks for nothing and plays nothing: {heard:?} {asked:?}");
    }

    #[test]
    fn an_id_without_a_clip_is_said_once_and_the_battle_goes_on_without_it() {
        let mut rig = Rig::new(&[], &[]);
        rig.state.borrow_mut().no_clip = vec![18];
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        look(&mut rig, &client, 0.1);
        assert!(rig.lines().iter().any(|l| l.starts_with("no clip for effect sound 18 (an empty slot)")), "{:?}", rig.lines());
        for round in 0..3 {
            send(&mut client, 10.0 + f64::from(round) * 3.0, blow(0, 2, false));
            for step in 0..=100 {
                look(&mut rig, &client, 10.0 + f64::from(round) * 3.0 + f64::from(step) * 0.01);
            }
        }
        assert!(played(&rig).is_empty());
        assert_eq!(rig.lines().iter().filter(|l| l.starts_with("battle sound 18 (hit) has no clip")).count(), 1, "said once");
        // Another weapon's sounds are untouched.
        send(&mut client, 30.0, blow(1, 2, false));
        for step in 0..=100 {
            look(&mut rig, &client, 30.0 + f64::from(step) * 0.01);
        }
        assert_eq!(played(&rig), ["battle sound 292 (hit)"]);
    }

    #[test]
    fn a_second_battle_asks_only_for_the_clips_it_does_not_have_and_a_machine_without_sound_files_plays_nothing() {
        let mut rig = Rig::new(&[], &[]);
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        let requests = |rig: &Rig| rig.state.borrow().asked.iter().filter(|a| a.starts_with("sounds")).cloned().collect::<Vec<_>>();
        assert_eq!(requests(&rig), ["sounds [5, 15, 18, 27, 292, 350]"]);
        // The same party and formation again: nothing new to read. Another formation (scene 76, whose attack 272 has another
        // impact): only that clip.
        client.on_message(5.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: fmv_protocol::battle::Outcome::Victory, rewards: Vec::new() });
        client.on_message(6.0, start(2, 301));
        look(&mut rig, &client, 6.0);
        assert_eq!(requests(&rig).len(), 1, "{:?}", requests(&rig));
        client.on_message(7.0, ServerMsg::BattleEnd { battle_id: BattleId(2), outcome: fmv_protocol::battle::Outcome::Victory, rewards: Vec::new() });
        client.on_message(8.0, start(3, 304));
        look(&mut rig, &client, 8.0);
        assert_eq!(requests(&rig), ["sounds [5, 15, 18, 27, 292, 350]", "sounds [99]"]);
        // No audio on this run at all: every call is a no-op.
        let mut off = GameAudio::off();
        off.follow_battle(&client, 1.0);
        assert_eq!(off.summary(), None);
    }

    #[test]
    fn battle_sounds_share_the_master_volume_and_the_mute_key() {
        let peak = |steps_down: usize, mute: bool| {
            let mut rig = Rig::new(&[], &[]);
            for _ in 0..steps_down {
                rig.audio.change_volume(false);
            }
            let mut client = battle();
            look(&mut rig, &client, 0.0);
            send(&mut client, 10.0, blow(0, 2, false));
            look(&mut rig, &client, 10.0);
            if mute {
                rig.audio.toggle_mute();
            }
            // Let the mixer's ramps (volume, mute) finish before the sound starts.
            rig.audio.pump(24_000);
            look(&mut rig, &client, 10.51);
            assert_eq!(played(&rig), ["battle sound 18 (hit)"]);
            rig.audio.pump(2_000).iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        let (loud, quiet, muted) = (peak(0, false), peak(4, false), peak(0, true));
        assert!(loud > 0.1, "the sound is in the mix ({loud})");
        assert!((quiet / loud - 0.5).abs() < 0.05, "master volume 0.4 against 0.8 halves it: {quiet} against {loud}");
        assert!(muted < 1e-6, "the M key silences it ({muted})");
    }

    #[test]
    fn the_flag_is_read_from_the_environment_the_way_the_other_flags_are() {
        assert_eq!(BattleSounds::from_env(None), (BattleSounds::Data, None));
        assert_eq!(BattleSounds::from_env(Some("")), (BattleSounds::Data, None));
        for off in ["off", "OFF", " 0 ", "no", "false", "none"] {
            assert_eq!(BattleSounds::from_env(Some(off)), (BattleSounds::Off, None), "{off:?}");
        }
        for data in ["data", "on", "1", "yes", "true", "Data"] {
            assert_eq!(BattleSounds::from_env(Some(data)), (BattleSounds::Data, None), "{data:?}");
        }
        for all in ["all", "ALL", "more"] {
            assert_eq!(BattleSounds::from_env(Some(all)), (BattleSounds::All, None), "{all:?}");
        }
        let (which, note) = BattleSounds::from_env(Some("loud"));
        assert_eq!(which, BattleSounds::Data);
        assert!(note.is_some_and(|n| n.contains("FMV_BATTLE_SOUNDS") && n.contains("loud")));
        assert!(!BattleSounds::Off.plays(false) && BattleSounds::Data.plays(false) && !BattleSounds::Data.plays(true) && BattleSounds::All.plays(true));
    }

    #[test]
    fn the_clip_store_lets_the_oldest_go_and_says_which() {
        let clip = || SfxClip::from_mono(RATE, &[0.5; 8]).expect("a clip");
        let mut clips = Clips::default();
        let mut gone = Vec::new();
        for id in 1..=(BATTLE_CLIPS as u16 + 3) {
            gone.extend(clips.insert(id, clip()));
        }
        assert_eq!(gone, [1, 2, 3]);
        assert!(clips.get(1).is_none() && clips.get(4).is_some() && clips.get(BATTLE_CLIPS as u16 + 3).is_some());
        // The same id again is one clip, not two.
        assert!(clips.insert(10, clip()).is_empty());
        assert_eq!(clips.clips.len(), BATTLE_CLIPS);
    }

    // ---------- field sounds ----------

    use fmv_core::Pos;

    /// The progression blob of a story at `moment`.
    fn blob(moment: u16) -> Vec<u8> {
        StoryState::at(moment).encode()
    }

    fn plan_of(ambient: Vec<AmbientLoop>) -> field::FieldPlan {
        field::FieldPlan { ambient, ..field::FieldPlan::default() }
    }

    fn ambient(channel: u8, id: u16) -> AmbientLoop {
        AmbientLoop { channel, id, pan: 64, at: 0 }
    }

    fn call(at: u32, channel: u8, id: u16) -> PlannedCall {
        PlannedCall { at, channel, id, pan: 64 }
    }

    /// A frame in field `key` at `now` seconds with the player at `pos`, `rejected` rejections so far, not in a battle.
    fn frame<'a>(key: &'a str, story: &'a [u8], now: f64) -> FieldFrame<'a> {
        FieldFrame { now, key: Some(key), story, pos: Some(Pos::new(0.0, 0.0, 5.0)), rejected: 0, in_battle: false }
    }

    /// One frame of the field's sounds, then a little time on the engine so that it applies what was asked.
    fn follow(rig: &mut Rig, f: &FieldFrame<'_>) {
        rig.audio.follow_field(f);
        rig.audio.pump(64);
    }

    /// The `field` lines of the log.
    fn field_lines(rig: &Rig) -> Vec<String> {
        rig.lines().into_iter().filter(|l| l.starts_with("field ")).collect()
    }

    fn field_asks(rig: &Rig) -> Vec<String> {
        rig.state.borrow().asked.iter().filter(|a| a.starts_with("field ")).cloned().collect()
    }

    /// A rig whose field `key` plays `plan` whatever the story, with the clips of `loops` looping.
    fn field_rig(key: &str, plan: field::FieldPlan, loops: &[u16]) -> Rig {
        let rig = Rig::new(&[], &[]);
        {
            let mut s = rig.state.borrow_mut();
            s.loop_ids = loops.to_vec();
            s.fields.insert(key.to_string(), Rc::new(move |_| plan.clone()));
        }
        rig
    }

    #[test]
    fn a_field_starts_its_ambient_loop_when_its_plan_and_clip_have_arrived_and_stops_it_when_the_player_leaves() {
        let mut rig = field_rig("nmkin_2", plan_of(vec![ambient(3, 38)]), &[38]);
        let story = blob(15);
        // the plan is asked for when the field is entered, with the story the client holds; nothing plays before it arrives
        follow(&mut rig, &frame("nmkin_2", &story, 0.0));
        assert_eq!(field_asks(&rig), ["field nmkin_2 moment 15 have []"]);
        assert!(rig.audio.field_loops().is_empty());
        follow(&mut rig, &frame("nmkin_2", &story, 0.05));
        assert_eq!(rig.audio.field_loops(), [(3, 38)]);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1);
        let lines = field_lines(&rig);
        assert!(lines.iter().any(|l| l.starts_with("field nmkin_2 sounds (moment 15): ambient ch3:38; links 0, lift 0, doors 0; unjudged 0, open 0; ")), "{lines:?}");
        assert!(lines.contains(&"field loop start ch3 38".to_string()), "{lines:?}");
        // a clip of a tenth of a second is still playing a second later: it is a loop
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1);
        assert_eq!(field_asks(&rig).len(), 1, "asked once, however long the player stays");
        // the next field has none: the loop fades out and is gone
        follow(&mut rig, &frame("nmkin_3", &story, 2.0));
        assert!(rig.audio.field_loops().is_empty());
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0);
        assert!(field_lines(&rig).contains(&"field loop stop ch3 38 (the field was left)".to_string()));
        // out of the FF7 fields altogether nothing is asked for
        let asked = field_asks(&rig).len();
        rig.audio.follow_field(&FieldFrame { key: None, ..frame("x", &story, 3.0) });
        assert_eq!(field_asks(&rig).len(), asked);
    }

    #[test]
    fn the_story_moving_from_15_to_27_while_the_player_stands_in_nmkin_4_turns_the_reactor_hum_into_the_alarm() {
        let mut rig = Rig::new(&[], &[]);
        {
            let mut s = rig.state.borrow_mut();
            s.loop_ids = vec![38, 60];
            // nmkin_4 as its script says: the hum on channel 3 until the moment is 27, then the alarm on channel 2
            s.fields.insert(
                "nmkin_4".to_string(),
                Rc::new(|story: &[u8]| plan_of(if StoryState::decode(story).is_some_and(|s| s.moment == 27) { vec![ambient(2, 60)] } else { vec![ambient(3, 38)] })),
            );
        }
        let (before, after) = (blob(15), blob(27));
        follow(&mut rig, &frame("nmkin_4", &before, 0.0));
        follow(&mut rig, &frame("nmkin_4", &before, 0.05));
        assert_eq!(rig.audio.field_loops(), [(3, 38)], "the hum before the boss");
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1);
        // the boss falls: the server moves the story to 27 and the client is told. The old hum goes on until the plan for 27 is read...
        follow(&mut rig, &frame("nmkin_4", &after, 2.0));
        assert_eq!(field_asks(&rig).last().map(String::as_str), Some("field nmkin_4 moment 27 have [38]"), "a new plan for the new story, asking only for the clips it does not hold");
        // ...and then the set changes to it, channel by channel
        follow(&mut rig, &frame("nmkin_4", &after, 2.05));
        assert_eq!(rig.audio.field_loops(), [(2, 60)], "the alarm is on and the hum is off");
        assert_eq!(rig.audio.status().expect("on").loops_playing, 2, "the hum is still fading out (a second, not a cut)");
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1, "and then only the alarm plays");
        let lines = field_lines(&rig);
        let at = |needle: &str| lines.iter().position(|l| l == needle).unwrap_or_else(|| panic!("{needle}: {lines:?}"));
        // (the plan for 27 is read first; the two channels are then changed in the same look, in channel order)
        let plan_27 = lines.iter().position(|l| l.starts_with("field nmkin_4 sounds (moment 27): ambient ch2:60;")).unwrap_or_else(|| panic!("{lines:?}"));
        assert!(at("field loop start ch3 38") < plan_27 && plan_27 < at("field loop start ch2 60") && plan_27 < at("field loop stop ch3 38 (the story moved)"), "{lines:?}");
        // a story that moves again without changing what the field plays leaves the alarm alone
        follow(&mut rig, &frame("nmkin_4", &blob(28), 3.0));
        follow(&mut rig, &frame("nmkin_4", &blob(28), 3.05));
        assert_eq!(rig.audio.field_loops(), [(3, 38)], "(the plan for 28 is the hum again: this fake field's own condition is == 27)");
    }

    #[test]
    fn a_new_plan_that_wants_the_same_loops_leaves_them_playing_untouched() {
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38)]), &[38]);
        follow(&mut rig, &frame("a", &blob(15), 0.0));
        follow(&mut rig, &frame("a", &blob(15), 0.05));
        // the story moves, the plan comes back the same
        follow(&mut rig, &frame("a", &blob(16), 1.0));
        follow(&mut rig, &frame("a", &blob(16), 1.05));
        assert_eq!(field_asks(&rig), ["field a moment 15 have []", "field a moment 16 have [38]"]);
        assert_eq!(rig.audio.field_loops(), [(3, 38)]);
        let starts = field_lines(&rig).iter().filter(|l| l.starts_with("field loop start")).count();
        let stops = field_lines(&rig).iter().filter(|l| l.starts_with("field loop stop")).count();
        assert_eq!((starts, stops), (1, 0), "{:?}", field_lines(&rig));
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1);
    }

    #[test]
    fn a_plan_for_a_story_the_client_has_left_is_dropped_and_another_is_asked_for() {
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38)]), &[38]);
        let (old, new) = (blob(15), blob(27));
        rig.state.borrow_mut().hold = true;
        follow(&mut rig, &frame("a", &old, 0.0));
        // the story moves before the answer comes
        follow(&mut rig, &frame("a", &new, 0.05));
        assert_eq!(field_asks(&rig).len(), 1, "one request in flight at a time");
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        {
            let mut s = rig.state.borrow_mut();
            s.hold = false;
            s.inbox.extend(held);
        }
        follow(&mut rig, &frame("a", &new, 0.1));
        assert!(rig.audio.field_loops().is_empty(), "the plan for the old story is not applied");
        assert_eq!(field_asks(&rig), ["field a moment 15 have []", "field a moment 27 have [38]"], "the clips it brought are kept, and the new request knows it");
        follow(&mut rig, &frame("a", &new, 0.15));
        assert_eq!(rig.audio.field_loops(), [(3, 38)]);
    }

    #[test]
    fn a_battle_takes_the_loops_over_and_gives_them_back_without_asking_again() {
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38), ambient(2, 60)]), &[38, 60]);
        let story = blob(15);
        follow(&mut rig, &frame("a", &story, 0.0));
        follow(&mut rig, &frame("a", &story, 0.05));
        assert_eq!(rig.audio.field_loops(), [(2, 60), (3, 38)]);
        let fight = |now| FieldFrame { in_battle: true, ..frame("a", &story, now) };
        follow(&mut rig, &fight(1.0));
        assert!(rig.audio.field_loops().is_empty(), "the field's loops stop for the fight");
        follow(&mut rig, &fight(1.5));
        assert!(rig.audio.field_loops().is_empty(), "and stay stopped for as long as it lasts");
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0);
        follow(&mut rig, &frame("a", &story, 5.0));
        assert_eq!(rig.audio.field_loops(), [(2, 60), (3, 38)], "back after it, from their start");
        assert_eq!(field_asks(&rig).len(), 1, "the plan did not have to be read again");
        let lines = field_lines(&rig);
        assert_eq!(lines.iter().filter(|l| l.starts_with("field loop start")).count(), 4);
        assert!(lines.contains(&"field loop stop ch3 38 (a battle)".to_string()), "{lines:?}");
    }

    #[test]
    fn a_battle_that_moves_the_story_gives_the_loops_back_for_the_new_story_and_never_for_the_old_one() {
        // the boss of nmkin_5, in shape: the hum (15) is stopped for the fight, the win moves the story to 27 while the end panel
        // is up (the session's frame keeps `in_battle` for as long), and the loops that come back are the ones for 27
        let mut rig = Rig::new(&[], &[]);
        {
            let mut s = rig.state.borrow_mut();
            s.loop_ids = vec![38, 60];
            s.fields.insert(
                "nmkin_5".to_string(),
                Rc::new(|story: &[u8]| plan_of(if StoryState::decode(story).is_some_and(|s| s.moment == 27) { vec![ambient(2, 60)] } else { vec![ambient(3, 38)] })),
            );
        }
        let (before, after) = (blob(15), blob(27));
        follow(&mut rig, &frame("nmkin_5", &before, 0.0));
        follow(&mut rig, &frame("nmkin_5", &before, 0.05));
        assert_eq!(rig.audio.field_loops(), [(3, 38)]);
        let fight = |story: &'static [u8], now: f64| FieldFrame { in_battle: true, ..frame("nmkin_5", story, now) };
        let (before, after): (&'static [u8], &'static [u8]) = (Box::leak(before.into_boxed_slice()), Box::leak(after.into_boxed_slice()));
        follow(&mut rig, &fight(before, 1.0));
        assert!(rig.audio.field_loops().is_empty());
        // the fight is won and the story moves, with the end panel still up
        follow(&mut rig, &fight(after, 2.0));
        follow(&mut rig, &fight(after, 2.05));
        assert!(rig.audio.field_loops().is_empty(), "nothing plays under the end panel");
        // the panel goes: the loops of the story the player has now
        follow(&mut rig, &frame("nmkin_5", after, 4.0));
        assert_eq!(rig.audio.field_loops(), [(2, 60)]);
        let lines = field_lines(&rig);
        assert_eq!(lines.iter().filter(|l| l.starts_with("field loop start ch3 38")).count(), 1, "the hum was started once, before the fight: {lines:?}");
        assert!(lines.contains(&"field loop start ch2 60".to_string()), "{lines:?}");
    }

    #[test]
    fn a_fields_ambience_from_the_sound_layer_fades_in_stops_for_a_battle_comes_back_and_goes_with_the_field() {
        let mut rig = field_rig("md1stin", plan_of(vec![]), &[]);
        rig.state.borrow_mut().ambience.insert("md1stin".to_string(), ("field_116".to_string(), 0.5, 0.25));
        let story = blob(8);
        follow(&mut rig, &frame("md1stin", &story, 0.0));
        follow(&mut rig, &frame("md1stin", &story, 0.05));
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1, "the ambience loops beside the field's own (none here)");
        assert!(rig.audio.field_loops().is_empty(), "it takes none of the four channels of the game's own sounds");
        let lines = field_lines(&rig);
        assert!(lines.contains(&"field md1stin: ambience field_116 from cosmo (Ambience\\Ambient\\field_116.ogg; fades in over 0.50 s, out over 0.25 s)".to_string()), "{lines:?}");
        assert!(lines.contains(&"field ambience start field_116".to_string()), "{lines:?}");
        // a fight takes it away, and its end gives it back (once)
        follow(&mut rig, &FieldFrame { in_battle: true, ..frame("md1stin", &story, 1.0) });
        follow(&mut rig, &frame("md1stin", &story, 2.0));
        follow(&mut rig, &frame("md1stin", &story, 2.05));
        // another field without one: it goes
        follow(&mut rig, &frame("md1_1", &story, 3.0));
        follow(&mut rig, &frame("md1_1", &story, 3.05));
        let lines = field_lines(&rig);
        let count = |want: &str| lines.iter().filter(|l| l.as_str() == want).count();
        assert_eq!(count("field ambience start field_116"), 2, "{lines:?}");
        assert_eq!(count("field ambience stop field_116 (a battle)"), 1, "{lines:?}");
        assert_eq!(count("field ambience stop field_116 (the field was left)"), 1, "{lines:?}");
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0, "faded out and gone");
    }

    #[test]
    fn a_battles_ambience_from_the_sound_layer_plays_for_the_fight_and_fades_out_at_its_end() {
        let mut rig = Rig::new(&[], &[]);
        rig.state.borrow_mut().battle_ambience.insert(301, "bat_301".to_string());
        let mut client = battle();
        look(&mut rig, &client, 0.0);
        look(&mut rig, &client, 0.05);
        let lines = rig.lines();
        assert!(rig.state.borrow().asked.contains(&"battle ambience 301".to_string()));
        assert!(lines.contains(&"battle ambience start bat_301 from cosmo (Ambience\\Ambient\\bat_301.ogg)".to_string()), "{lines:?}");
        assert_eq!(rig.audio.status().expect("on").loops_playing, 1);
        // the fight ends: the ambience fades out and is gone; a formation without one asks and plays nothing
        client.on_message(5.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: fmv_protocol::battle::Outcome::Victory, rewards: Vec::new() });
        look(&mut rig, &client, 5.0);
        assert!(rig.lines().contains(&"battle ambience stop bat_301 (the battle ended)".to_string()), "{:?}", rig.lines());
        rig.audio.pump(48_000);
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0);
        client.on_message(6.0, start(2, 302));
        look(&mut rig, &client, 6.0);
        look(&mut rig, &client, 6.05);
        assert!(rig.state.borrow().asked.contains(&"battle ambience 302".to_string()));
        assert_eq!(rig.lines().iter().filter(|l| l.starts_with("battle ambience start")).count(), 1);
    }

    #[test]
    fn with_field_sounds_off_nothing_is_asked_for_and_nothing_plays() {
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38)]), &[38]);
        rig.audio = std::mem::take(&mut rig.audio).with_field_sounds(FieldSounds::Off);
        let story = blob(15);
        for step in 0..6 {
            follow(&mut rig, &frame("a", &story, f64::from(step) * 0.05));
        }
        follow(&mut rig, &frame("b", &blob(27), 1.0));
        assert!(field_asks(&rig).is_empty(), "no field scan is requested: {:?}", rig.state.borrow().asked);
        assert!(rig.audio.field_loops().is_empty() && field_lines(&rig).is_empty());
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0);
        // the rest of the soundtrack does not care: the field's music is still decided
        let mut rig = Rig::new(&[("a", area(Some("ob"), None, None))], &[("ob", 4_800), ("bat", 4_800), ("fanfare", 2_400)]);
        rig.audio = std::mem::take(&mut rig.audio).with_field_sounds(FieldSounds::Off);
        for _ in 0..3 {
            rig.frame(Some("a"), NONE);
        }
        assert_eq!(rig.playing().as_deref(), Some("ob"));
    }

    #[test]
    fn the_flag_is_read_from_the_environment_like_the_battle_one() {
        assert_eq!(FieldSounds::from_env(None), (FieldSounds::Data, None));
        assert_eq!(FieldSounds::from_env(Some("  ")), (FieldSounds::Data, None));
        for off in ["off", "OFF", " 0 ", "no", "false", "none"] {
            assert_eq!(FieldSounds::from_env(Some(off)), (FieldSounds::Off, None), "{off:?}");
        }
        for data in ["data", "Data", "on", "1", "yes", "true"] {
            assert_eq!(FieldSounds::from_env(Some(data)), (FieldSounds::Data, None), "{data:?}");
        }
        let (which, note) = FieldSounds::from_env(Some("loud"));
        assert_eq!(which, FieldSounds::Data);
        assert!(note.is_some_and(|n| n.contains("FMV_FIELD_SOUNDS") && n.contains("loud")));
        assert_eq!(FieldSounds::default(), FieldSounds::Data);
    }

    /// The link `line`/`to` of the tests: a jump from the left of the field to a place 110 units on.
    fn jump_link() -> (LinkSpot, LinkPlan) {
        let line = (Pos::new(-10.0, 0.0, 7.0), Pos::new(-10.0, 50.0, 7.0));
        let to = Pos::new(110.0, 10.0, 0.0);
        (LinkSpot { line, to, needs_ok: false, spot: Pos::new(110.0, 10.0, 5.0) }, LinkPlan { line, to, needs_ok: false, calls: vec![call(15, 1, 62)] })
    }

    #[test]
    fn a_link_the_server_put_the_player_through_plays_its_sound_when_the_script_says() {
        let (spot, plan) = jump_link();
        let mut rig = field_rig("nmkin_3", field::FieldPlan { links: vec![plan], ..field::FieldPlan::default() }, &[]);
        rig.audio.live.as_mut().expect("on").field.set_links(vec![spot.clone()]);
        let story = blob(13);
        let at = |now: f64, x: f32, rejected: u64| FieldFrame { pos: Some(Pos::new(x, 10.0, 5.0)), rejected, ..frame("nmkin_3", &story, now) };
        follow(&mut rig, &at(0.0, -20.0, 0));
        follow(&mut rig, &at(0.05, -20.0, 0));
        // walking about, even with a rejection against a wall, plays nothing
        follow(&mut rig, &at(0.5, -15.0, 1));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0);
        // the server tells the client it moved it to where the link leads: the jump's sound is due half a second after
        follow(&mut rig, &at(1.0, 110.0, 2));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0, "the script waits 15 frames before the take-off");
        follow(&mut rig, &at(1.3, 110.0, 3));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0, "not yet");
        follow(&mut rig, &at(1.5, 110.0, 3));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 1);
        assert!(field_lines(&rig).contains(&"field sound ch1 62 (link)".to_string()), "{:?}", field_lines(&rig));
        // standing there, even with more rejections (the fence answers every old intent), does not play it again
        follow(&mut rig, &at(2.0, 110.0, 6));
        follow(&mut rig, &at(2.5, 110.0, 7));
        rig.audio.pump(48_000);
        follow(&mut rig, &at(3.0, 110.0, 8));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0);
        assert_eq!(field_lines(&rig).iter().filter(|l| l.starts_with("field sound")).count(), 1);
        // a place that is not where a link leads is no link, whatever the rejections say
        follow(&mut rig, &at(4.0, 300.0, 9));
        follow(&mut rig, &at(6.0, 300.0, 9));
        assert_eq!(field_lines(&rig).iter().filter(|l| l.starts_with("field sound")).count(), 1);
    }

    #[test]
    fn a_story_bit_that_flips_plays_what_the_script_plays_after_it_and_one_that_does_not_move_plays_nothing() {
        let flips = vec![
            FlipPlan { bank: 1, address: 225, bit: 0, on: true, calls: vec![call(0, 1, 42)] },
            FlipPlan { bank: 1, address: 225, bit: 0, on: false, calls: vec![call(0, 1, 43)] },
        ];
        let mut rig = field_rig("elevtr1", field::FieldPlan { flips, ..field::FieldPlan::default() }, &[42]);
        let state = |moment: u16, lift: bool| {
            let mut s = StoryState::at(moment);
            s.set_bit(1, 225, 0, lift);
            s.encode()
        };
        let (up, down) = (state(11, false), state(12, true));
        follow(&mut rig, &frame("elevtr1", &up, 0.0));
        follow(&mut rig, &frame("elevtr1", &up, 0.05));
        // the party rides down: the bit goes on and the story moves to 12
        follow(&mut rig, &frame("elevtr1", &down, 1.0));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 1, "the lift's sound, once through (a loop clip is not looped by an event)");
        assert_eq!(rig.audio.status().expect("on").loops_playing, 0);
        assert!(field_lines(&rig).contains(&"field sound ch1 42 (lift)".to_string()), "{:?}", field_lines(&rig));
        // the story moves again with the bit as it was: nothing
        follow(&mut rig, &frame("elevtr1", &state(13, true), 2.0));
        assert_eq!(field_lines(&rig).iter().filter(|l| l.starts_with("field sound")).count(), 1);
        // and the ride back plays the other flip's sound
        follow(&mut rig, &frame("elevtr1", &state(13, false), 3.0));
        assert!(field_lines(&rig).contains(&"field sound ch1 43 (lift)".to_string()), "{:?}", field_lines(&rig));
        // a field entered with the bit already on is no flip
        let mut rig = field_rig("nmkin_2", field::FieldPlan { flips: vec![FlipPlan { bank: 1, address: 225, bit: 0, on: true, calls: vec![call(0, 1, 42)] }], ..field::FieldPlan::default() }, &[]);
        follow(&mut rig, &frame("nmkin_2", &down, 0.0));
        follow(&mut rig, &frame("nmkin_2", &down, 0.05));
        assert!(field_lines(&rig).iter().all(|l| !l.starts_with("field sound")));
    }

    #[test]
    fn a_door_whose_script_plays_before_its_jump_plays_in_the_field_it_leads_to() {
        let exit = ExitPlan { target: "nmkin_2".to_string(), arrival: (-682, 308), line: Some(((-185, -68), (-185, 56))), calls: vec![call(0, 1, 55)] };
        let mut rig = field_rig("elevtr1", field::FieldPlan { exits: vec![exit], ..field::FieldPlan::default() }, &[]);
        let story = blob(12);
        let near_the_door = |now: f64| FieldFrame { pos: Some(Pos::new(-170.0, 0.0, 0.0)), ..frame("elevtr1", &story, now) };
        follow(&mut rig, &near_the_door(0.0));
        follow(&mut rig, &near_the_door(0.05));
        follow(&mut rig, &near_the_door(0.1));
        // the server moves the player to nmkin_2, to the place the door's jump says
        let arrived = |key: &'static str, x: f32, y: f32, now: f64| FieldFrame { pos: Some(Pos::new(x, y, 0.0)), ..frame(key, &story, now) };
        follow(&mut rig, &arrived("nmkin_2", -680.0, 300.0, 0.15));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 1);
        assert!(field_lines(&rig).contains(&"field sound ch1 55 (door)".to_string()), "{:?}", field_lines(&rig));
        // another field, or a place that is not where that door puts a player, is not that door
        let mut rig = field_rig("elevtr1", field::FieldPlan { exits: vec![ExitPlan { target: "nmkin_2".to_string(), arrival: (-682, 308), line: None, calls: vec![call(0, 1, 55)] }], ..field::FieldPlan::default() }, &[]);
        for now in [0.0, 0.05, 0.1] {
            follow(&mut rig, &near_the_door(now));
        }
        follow(&mut rig, &arrived("nmkin_2", 500.0, 300.0, 0.15));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0, "the arrival is not where the door leads");
        let mut rig = field_rig("elevtr1", field::FieldPlan { exits: vec![ExitPlan { target: "nmkin_2".to_string(), arrival: (-682, 308), line: None, calls: vec![call(0, 1, 55)] }], ..field::FieldPlan::default() }, &[]);
        for now in [0.0, 0.05, 0.1] {
            follow(&mut rig, &near_the_door(now));
        }
        follow(&mut rig, &arrived("nmkin_3", -680.0, 300.0, 0.15));
        assert_eq!(rig.audio.status().expect("on").sfx_playing, 0, "it does not lead to nmkin_3");
    }

    #[test]
    fn a_sound_an_event_plays_replaces_the_loop_of_its_channel_and_leaves_the_others_alone() {
        let (spot, mut plan) = jump_link();
        plan.calls = vec![call(0, 1, 62), call(0, 3, 21)];
        let field_plan = field::FieldPlan { ambient: vec![ambient(1, 95), ambient(2, 60)], links: vec![plan], ..field::FieldPlan::default() };
        let mut rig = field_rig("a", field_plan, &[95, 60]);
        rig.audio.live.as_mut().expect("on").field.set_links(vec![spot]);
        let story = blob(13);
        let at = |now: f64, x: f32, rejected: u64| FieldFrame { pos: Some(Pos::new(x, 10.0, 5.0)), rejected, ..frame("a", &story, now) };
        follow(&mut rig, &at(0.0, -20.0, 0));
        follow(&mut rig, &at(0.05, -20.0, 0));
        assert_eq!(rig.audio.field_loops(), [(1, 95), (2, 60)]);
        follow(&mut rig, &at(1.0, 110.0, 1));
        assert_eq!(rig.audio.field_loops(), [(2, 60)], "the sound on channel 1 replaced channel 1's loop; channel 2's goes on");
        assert!(field_lines(&rig).contains(&"field loop stop ch1 95 (a link sound replaces it)".to_string()), "{:?}", field_lines(&rig));
        // and it does not come back by itself while the player stays in the field
        follow(&mut rig, &at(2.0, 110.0, 1));
        follow(&mut rig, &at(3.0, 110.0, 1));
        assert_eq!(rig.audio.field_loops(), [(2, 60)]);
    }

    #[test]
    fn what_cannot_be_judged_is_counted_and_said_and_a_plan_that_cannot_be_read_is_said_once() {
        let plan = field::FieldPlan { ambient: vec![ambient(3, 38)], unjudged: 3, left_open: 1, withheld_links: 2, ..field::FieldPlan::default() };
        let mut rig = field_rig("a", plan, &[38]);
        let story = blob(15);
        follow(&mut rig, &frame("a", &story, 0.0));
        follow(&mut rig, &frame("a", &story, 0.05));
        assert!(
            field_lines(&rig).iter().any(|l| l.starts_with("field a sounds (moment 15): ambient ch3:38; links 0 (+2 untimed), lift 0, doors 0; unjudged 3, open 1; ")),
            "what could not be judged, what is left open and what has no time are all in the line: {:?}",
            field_lines(&rig)
        );
        // a field that cannot be read: one line, silence, and it is not asked for again for that story
        let mut rig = Rig::new(&[], &[]);
        rig.state.borrow_mut().broken = vec!["x".to_string()];
        for step in 0..6 {
            follow(&mut rig, &frame("x", &story, f64::from(step) * 0.05));
        }
        let cannot: Vec<String> = field_lines(&rig).into_iter().filter(|l| l.contains("cannot read its sounds")).collect();
        assert_eq!(cannot, ["field x: cannot read its sounds (the field cannot be read); going on without them"]);
        assert_eq!(field_asks(&rig).len(), 1);
        assert!(rig.audio.field_loops().is_empty());
    }

    #[test]
    fn an_id_without_a_clip_is_said_once_and_the_rest_of_the_field_still_plays() {
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38), ambient(2, 60)]), &[38, 60]);
        rig.state.borrow_mut().no_clip = vec![38];
        let story = blob(15);
        for step in 0..8 {
            follow(&mut rig, &frame("a", &story, f64::from(step) * 0.05));
        }
        assert_eq!(rig.audio.field_loops(), [(2, 60)]);
        let lines = rig.lines();
        let about_38 = lines.iter().filter(|l| l.contains("38") && l.contains("clip") && !l.contains("sounds (moment")).collect::<Vec<_>>();
        assert_eq!(about_38.len(), 1, "said once, however many frames follow: {lines:?}");
        assert!(about_38[0].starts_with("no clip for field sound 38 (an empty slot)"), "{about_38:?}");
    }

    #[test]
    fn a_loop_that_starts_after_a_wait_waits_for_its_time_in_the_field() {
        let late = AmbientLoop { channel: 1, id: 95, pan: 64, at: 30 };
        let mut rig = field_rig("a", plan_of(vec![late]), &[95]);
        let story = blob(15);
        follow(&mut rig, &frame("a", &story, 10.0));
        follow(&mut rig, &frame("a", &story, 10.1));
        assert!(rig.audio.field_loops().is_empty(), "a second after the field was entered: not yet");
        follow(&mut rig, &frame("a", &story, 10.9));
        assert!(rig.audio.field_loops().is_empty());
        follow(&mut rig, &frame("a", &story, 11.05));
        assert_eq!(rig.audio.field_loops(), [(1, 95)], "30 frames = a second after the entry");
    }

    #[test]
    fn a_plan_names_the_ids_it_needs_and_a_run_that_heard_field_sounds_says_so_when_it_ends() {
        let plan = field::FieldPlan {
            ambient: vec![ambient(3, 38)],
            links: vec![LinkPlan { line: (Pos::default(), Pos::default()), to: Pos::default(), needs_ok: true, calls: vec![call(0, 1, 21), call(30, 1, 0)] }],
            flips: vec![FlipPlan { bank: 1, address: 225, bit: 0, on: true, calls: vec![call(0, 1, 42)] }],
            exits: vec![ExitPlan { target: "x".to_string(), arrival: (0, 0), line: None, calls: vec![call(0, 2, 55)] }],
            ..field::FieldPlan::default()
        };
        assert_eq!(plan.ids().into_iter().collect::<Vec<_>>(), [21, 38, 42, 55], "the stop (id 0) names no clip");
        assert!(!plan.is_empty() && field::FieldPlan::default().is_empty());
        let mut rig = field_rig("a", plan_of(vec![ambient(3, 38)]), &[38]);
        let story = blob(15);
        follow(&mut rig, &frame("a", &story, 0.0));
        follow(&mut rig, &frame("a", &story, 0.05));
        let lines = Arc::clone(&rig.lines);
        drop(rig);
        let said = lines.lock().expect("log").clone();
        assert!(said.iter().any(|l| l == "field sounds played: 0 sounds (), 1 loops (38x1)"), "{said:?}");
        // a run that heard none says nothing about them
        let quiet = Rig::new(&[], &[]);
        let lines = Arc::clone(&quiet.lines);
        drop(quiet);
        assert!(!lines.lock().expect("log").iter().any(|l| l.contains("field sounds played")));
    }

    // ---- the voice layer ----------------------------------------------------------------------------

    fn say(field: &str, dialog: usize) -> VoiceCue {
        VoiceCue::Say { field: field.to_string(), window: 0, dialog, page: 0 }
    }

    fn with_voices(voices: &[(&str, usize)]) -> Rig {
        let rig = Rig::new(&[], &[]);
        rig.state.borrow_mut().voices = voices.iter().map(|(k, f)| (k.to_string(), *f)).collect();
        rig
    }

    /// The loops the engine is playing after it has applied what was asked of it.
    fn loops(rig: &Rig) -> u32 {
        rig.audio.pump(64);
        rig.audio.status().expect("on").loops_playing
    }

    #[test]
    fn a_window_that_opens_asks_for_its_voice_and_plays_it_when_it_arrives() {
        let mut rig = with_voices(&[("md1stin/3", 24_000)]);
        rig.audio.voice(say("md1stin", 3));
        assert_eq!(loops(&rig), 0, "nothing plays until the clip has been read");
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 1, "the voice plays as a loop its handle can stop");
        assert!(rig.lines().iter().any(|l| l == "voice md1stin/3 (0.5 s)"), "{:?}", rig.lines());
        assert_eq!(rig.state.borrow().asked.last().map(String::as_str), Some("voice Say { field: \"md1stin\", window: 0, dialog: 3, page: 0 }"));
    }

    #[test]
    fn the_next_window_replaces_the_voice_and_a_conversation_that_ends_stops_it() {
        let mut rig = with_voices(&[("a/1", 24_000), ("a/2", 24_000)]);
        rig.audio.voice(say("a", 1));
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 1);
        // The next window: the first voice goes (it fades out), the second plays.
        rig.audio.voice(say("a", 2));
        rig.frame(None, NONE);
        rig.audio.pump(4_800);
        assert_eq!(loops(&rig), 1, "one voice at a time");
        assert_eq!(rig.lines().iter().filter(|l| l.starts_with("voice a/")).count(), 2);
        // The conversation ends.
        rig.audio.voice(VoiceCue::Stop);
        rig.frame(None, NONE);
        rig.audio.pump(4_800);
        assert_eq!(loops(&rig), 0);
    }

    #[test]
    fn an_answer_for_a_window_that_has_moved_on_is_thrown_away() {
        let mut rig = with_voices(&[("a/1", 24_000), ("a/2", 24_000)]);
        rig.state.borrow_mut().hold = true;
        rig.audio.voice(say("a", 1));
        rig.audio.voice(say("a", 2));
        // Both answers come back late, in order: only the newest ask is played.
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        {
            let mut s = rig.state.borrow_mut();
            s.hold = false;
            s.inbox.extend(held);
        }
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 1);
        let said: Vec<String> = rig.lines().into_iter().filter(|l| l.starts_with("voice ")).collect();
        assert_eq!(said, ["voice a/2 (0.5 s)"]);
        // A conversation that ended while its clip was being read plays nothing at all.
        rig.audio.voice(VoiceCue::Stop);
        rig.state.borrow_mut().hold = true;
        rig.audio.voice(say("a", 1));
        rig.audio.voice(VoiceCue::Stop);
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        rig.state.borrow_mut().inbox.extend(held);
        rig.frame(None, NONE);
        rig.audio.pump(4_800);
        assert_eq!(loops(&rig), 0);
    }

    #[test]
    fn a_window_without_a_clip_is_silent_and_a_choice_has_its_own_clip() {
        let mut rig = with_voices(&[("a/7_1", 12_000)]);
        rig.audio.voice(say("a", 7));
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 0);
        assert!(!rig.lines().iter().any(|l| l.starts_with("voice ")), "a window with no voice says nothing");
        rig.audio.voice(VoiceCue::Choice { field: "a".to_string(), dialog: 7, line: 1 });
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 1);
        assert!(rig.lines().iter().any(|l| l == "voice a/7_1 (0.2 s)"), "{:?}", rig.lines());
        let lines = Arc::clone(&rig.lines);
        drop(rig);
        assert!(lines.lock().expect("log").iter().any(|l| l == "voices played: 1 (asks without a clip: 1)"));
    }

    #[test]
    fn a_voice_whose_clip_is_over_lets_go_of_its_loop() {
        let mut rig = with_voices(&[("a/1", 480)]);
        rig.audio.voice(say("a", 1));
        rig.frame(None, NONE);
        assert_eq!(loops(&rig), 1);
        // The clip is over (its end is the length plus a grace): the next frame after that frees the slot.
        let live = rig.audio.live.as_mut().expect("on");
        live.voice.playing = live.voice.playing.map(|(handle, _)| (handle, Instant::now()));
        rig.frame(None, NONE);
        rig.audio.pump(4_800);
        assert_eq!(loops(&rig), 0);
    }

    #[test]
    fn a_run_without_sound_drops_the_cues_it_is_given() {
        let mut off = GameAudio::off();
        off.voice(say("a", 1));
        off.voice(VoiceCue::Stop);
        assert!(!off.is_on());
    }


    use crate::scene::{SceneSound, SoundAction};
    use fmv_protocol::SceneRunId;
    #[test]
    fn scene_sound_replacement_stop_delayed_decode_and_off_are_bounded_to_the_run() {
        // Shared test rig supplies a manual sink and delayed worker; no sound device.
        let mut rig = Rig::new(&[], &[]);
        let call = |action| SceneSound { run: SceneRunId(7), action };
        let feed = |rig: &mut Rig, actions: Vec<SceneSound>, at| rig.audio.live.as_mut().unwrap().scene_sounds(actions, at);
        feed(&mut rig, vec![call(SoundAction::Begin), call(SoundAction::Prepare(61))], 0.0);
        feed(&mut rig, vec![call(SoundAction::Play { id: 61, pan: 63 })], 0.5);
        assert!(rig.audio.pump(64).iter().any(|s| s.abs() > 0.01));
        feed(&mut rig, vec![call(SoundAction::Play { id: 0, pan: 63 })], 0.6);
        rig.audio.pump(64); // stop ramp
        assert!(rig.audio.pump(64).iter().all(|s| s.abs() < 0.001));
        rig.state.borrow_mut().hold = true;
        feed(&mut rig, vec![call(SoundAction::Play { id: 59, pan: 0 })], 1.0);
        feed(&mut rig, vec![call(SoundAction::End)], 1.1);
        let held = std::mem::take(&mut rig.state.borrow_mut().held);
        rig.state.borrow_mut().inbox.extend(held);
        feed(&mut rig, vec![], 1.2);
        assert!(rig.audio.pump(64).iter().all(|s| s.abs() < 0.001), "late clip cannot restart an ended run");
        feed(&mut rig, vec![call(SoundAction::Play { id: 61, pan: 63 })], 1.3);
        assert!(rig.audio.pump(64).iter().all(|s| s.abs() < 0.001), "stale run ignored");
        rig.audio = rig.audio.with_field_sounds(FieldSounds::Off);
        let asks = rig.state.borrow().asked.len();
        feed(&mut rig, vec![call(SoundAction::Begin), call(SoundAction::Prepare(236)), call(SoundAction::Play { id: 236, pan: 63 })], 2.0);
        assert_eq!(rig.state.borrow().asked.len(), asks, "off neither loads nor plays");
    }
}
