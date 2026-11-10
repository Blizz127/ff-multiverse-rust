//! What a field plays: its ambient loops, and the sounds of its ladders, jumps, lift and doors.
//!
//! The data side is [`fmv_ff7::field_sound`] (a bounded run of the field's own scripts, judged against the story); the
//! worker turns its answer into a [`FieldPlan`] (plain ids, channels and times) and reads the clips the plan names. This
//! module decides what to do with a plan, from what the game is doing: [`FieldState::follow`] is called every frame.
//!
//! * **Ambient loops.** The plan says which loop clips are on in this field for THIS story (the hum before the boss,
//!   the alarm after it). They fade in on the effect channel the script names (four channels, one sound each), stop when
//!   the player leaves the field, and **follow the story**: when the story the client holds changes (a `StoryUpdate`), a
//!   new plan is asked for, and when it arrives the set is changed to it, channel by channel (a loop that is still wanted
//!   is left alone; one that is not fades out; a new one fades in).
//! * **A battle takes over**: the loops fade out when a fight starts and come back, from their start, when it is over.
//! * **Events** the client can attribute with evidence: a ladder or jump the server put the player through
//!   (`MoveRejected` that lands the player where a link of the area leads: the sounds its trigger script and the player's
//!   scripts play, [`FieldPlan::links`]); a bit of the story a script sets and the sounds right after it (the lift of
//!   `elevtr1`, [`FieldPlan::flips`]); a door whose script plays before its `MAPJUMP` ([`FieldPlan::exits`]). A sound an
//!   event plays replaces what the channel was playing, as the game's channels do. A loop clip that an event starts plays
//!   once through: the script's stop is behind waits the data do not give.
//! * `FMV_FIELD_SOUNDS=off` asks for nothing and plays nothing (see [`FieldSounds`]).
//!
//! Nothing here can stop the game: a plan that cannot be read, a clip the library has no data for and a thread that is
//! gone are each one line and silence for that part.

use super::worker::{SoundsRead, Worker};
use fmv_audio::{AudioEngine, Logger, SfxClip, SfxHandle};
use fmv_core::Pos;
use fmv_ff7::story::StoryState;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

/// The environment variable that says which field sounds play (see [`FieldSounds`]).
pub const FIELD_SOUNDS_ENV: &str = "FMV_FIELD_SOUNDS";
/// How loud an ambient loop plays, `0.0..=1.0` before the master volume (the script says no volume: `SOUND` has none, and the
/// volume envelopes of `AKAO` 160 to 175 are not read; a loop sits under the music and the effects).
const AMBIENT_GAIN: f32 = 0.6;
/// How loud a field's ambience from the sound layer plays, before the master volume (as loud as the game's own ambient loops).
const AMBIENCE_GAIN: f32 = 0.6;
/// How loud a sound an event plays, `0.0..=1.0` before the master volume.
const EVENT_GAIN: f32 = 0.8;
/// An ambient loop fades in over this long.
const FADE_IN: Duration = Duration::from_millis(400);
/// A loop that goes (the field is left, a battle starts, a channel is replaced) fades out over this long.
const FADE_OUT: Duration = Duration::from_millis(300);
/// A loop that the story changes for another fades out over this long (the new one is fading in).
const FADE_STORY: Duration = Duration::from_millis(900);
/// The field module's frames per second (what the script times are counted in).
const FRAMES_PER_SECOND: f64 = 30.0;
/// A walker that comes to stand within this far (x and y) of where a link leads, from farther off, has used the link
/// (the rule `crate::route` uses: the server puts the player at the link's destination).
const LANDED: f32 = 3.0;
/// ...and within this far in height.
const LANDED_Z: f32 = 80.0;
/// A door's arrival (x and y) is the player's place after the change to within this far (the server snaps the arrival onto
/// the area's own surface).
const ARRIVAL: f32 = 24.0;
/// The player's last place in the field it left is within this far of the door's line (x and y) for it to be that door.
const DOOR_REACH: f32 = 160.0;
/// Clips kept at once (a field needs a handful).
const MAX_CLIPS: usize = 64;

/// Which field sounds are played (`FMV_FIELD_SOUNDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldSounds {
    /// None: nothing is asked of the worker and nothing plays.
    Off,
    /// What the game's own scripts say: the ambient loops of the field for the player's story, and the sounds of the links,
    /// the lift and the doors the player uses (the default).
    #[default]
    Data,
}

impl FieldSounds {
    /// The choice `FMV_FIELD_SOUNDS` (`value`) makes: `off` (or `0`, `no`, `false`, `none`) or `data` (or `on`, `1`, `yes`,
    /// `true`; also what an unset or empty one is). The second value is a note about one that makes no sense (which is `data`).
    pub fn from_env(value: Option<&str>) -> (Self, Option<String>) {
        let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else { return (Self::Data, None) };
        match raw.to_ascii_lowercase().as_str() {
            "off" | "0" | "no" | "false" | "none" => (Self::Off, None),
            "data" | "on" | "1" | "yes" | "true" => (Self::Data, None),
            _ => (Self::Data, Some(format!("{FIELD_SOUNDS_ENV}={raw:?} is not off or data: playing what the game's data names"))),
        }
    }
}

/// One sound play or stop of a plan, `at` frames (30 per second) after what set it going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedCall {
    pub at: u32,
    /// Effect channel, 1 to 4.
    pub channel: u8,
    /// The game's sound id; 0 stops the channel.
    pub id: u16,
    /// 0 left, 63 or 64 centre, 127 right.
    pub pan: u8,
}

/// A loop that is on in a field when it has started up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmbientLoop {
    pub channel: u8,
    pub id: u16,
    pub pan: u8,
    /// Frames after the field loaded that it starts.
    pub at: u32,
}

/// The sounds of a link (a ladder or a jump), with the identity of the area's own link.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkPlan {
    pub line: (Pos, Pos),
    pub to: Pos,
    pub needs_ok: bool,
    pub calls: Vec<PlannedCall>,
}

/// The sounds that follow a script's setting or clearing a bit of the story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlipPlan {
    pub bank: u8,
    pub address: u8,
    pub bit: u8,
    pub on: bool,
    pub calls: Vec<PlannedCall>,
}

/// The sounds on the way out of a door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitPlan {
    /// The field it leads to (as the archive spells it) and where it puts the player there.
    pub target: String,
    pub arrival: (i16, i16),
    pub line: Option<((i16, i16), (i16, i16))>,
    pub calls: Vec<PlannedCall>,
}

/// What a field's scripts play, for one story (the worker's answer, see the module docs).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldPlan {
    pub key: String,
    /// The progression blob the plan was made for.
    pub story: Vec<u8>,
    pub ambient: Vec<AmbientLoop>,
    pub links: Vec<LinkPlan>,
    pub flips: Vec<FlipPlan>,
    pub exits: Vec<ExitPlan>,
    /// Conditions the scripts make that could not be judged against the story (the plan leaves what lies behind them out).
    pub unjudged: usize,
    /// Loops left out because their channel could still be changed by a part of a script that was not reached.
    pub left_open: usize,
    /// Links that have a sound in their scripts and none that a time can be given for (it comes after an animation that is
    /// waited for, a walk, a condition nothing can judge): not played, counted.
    pub withheld_links: usize,
}

impl FieldPlan {
    /// The sound ids the plan can play (the clips to have).
    pub fn ids(&self) -> BTreeSet<u16> {
        let calls = |list: &[PlannedCall]| list.iter().map(|c| c.id).collect::<Vec<_>>();
        let mut ids: BTreeSet<u16> = self.ambient.iter().map(|a| a.id).collect();
        for plan in &self.links {
            ids.extend(calls(&plan.calls));
        }
        for plan in &self.flips {
            ids.extend(calls(&plan.calls));
        }
        for plan in &self.exits {
            ids.extend(calls(&plan.calls));
        }
        ids.remove(&0);
        ids
    }

    /// Whether the plan has anything to play (tests).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.ambient.is_empty() && self.links.is_empty() && self.flips.is_empty() && self.exits.is_empty()
    }
}

/// What the worker brings back for a field: the plan, the clips it names that the client did not have, and the field's ambience
/// from the sound layer (Cosmo Memory's), when it has one.
pub struct FieldRead {
    pub plan: FieldPlan,
    pub clips: SoundsRead,
    /// (Boxed: the worker's answers stay small whatever a clip weighs.)
    pub ambience: Option<Box<ModAmbience>>,
}

/// A field's ambience from the sound layer (`fmv_mods::Mods::field_ambience`): one clip that loops, centred, for as long as the
/// player is in the field and not fighting, fading in and out as the layer's config says. It is the layer's own addition: the
/// game's scripts play their ambient loops beside it as ever.
pub struct ModAmbience {
    /// The config section (`field_116`), the clip's name in the archive, and which mod it comes from.
    pub section: String,
    pub name: String,
    pub source: &'static str,
    pub clip: SfxClip,
    pub fade_in: Duration,
    pub fade_out: Duration,
}

/// A link of the area the player stands in, with the place the server puts a player at when it fires (the area's own
/// placement of the link's destination).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSpot {
    pub line: (Pos, Pos),
    pub to: Pos,
    pub needs_ok: bool,
    pub spot: Pos,
}

/// What the game is doing, as this module looks at it every frame.
#[derive(Debug, Clone, Copy)]
pub struct FieldFrame<'a> {
    /// Seconds on the session's clock.
    pub now: f64,
    /// The FF7 field the player is in (`None`: not in an FF7 field).
    pub key: Option<&'a str>,
    /// The progression blob the client holds (what the server last told it of the story).
    pub story: &'a [u8],
    /// Where the player's character stands.
    pub pos: Option<Pos>,
    /// How many `MoveRejected` the client has had (every one counts).
    pub rejected: u64,
    /// A battle has the field: a fight is on, or its end panel is still up.
    pub in_battle: bool,
}

/// A loop that is playing on a channel.
struct ActiveLoop {
    id: u16,
    handle: SfxHandle,
}

/// A call that is due later.
struct Scheduled {
    due: f64,
    call: PlannedCall,
    why: &'static str,
}

/// The field's sounds: what the player's field plays, and what is playing of it.
pub struct FieldState {
    mode: FieldSounds,
    key: Option<String>,
    entered: f64,
    plan: Option<FieldPlan>,
    /// The story the client holds now.
    wanted: Vec<u8>,
    /// A request for a plan is on its way.
    requested: bool,
    /// The plan of the field that was left, kept until the next field's plan arrives (for the door that led out of it).
    left: Option<FieldPlan>,
    clips: HashMap<u16, SfxClip>,
    /// Ids that have no clip to play, said once each.
    unheard: HashSet<u16>,
    loops: [Option<ActiveLoop>; 4],
    /// Channels an event replaced the loop of (not started again until the field changes or a battle ends).
    replaced: [bool; 4],
    pending: Vec<Scheduled>,
    battle: bool,
    links: Vec<LinkSpot>,
    last_pos: Option<Pos>,
    seen_rejected: u64,
    last_story: Option<StoryState>,
    /// Loops started and sounds played by events, by sound id (the closing line of a run).
    started: BTreeMap<u16, u32>,
    played: BTreeMap<u16, u32>,
    /// The ambience of the field the player is in, from the sound layer ([`ModAmbience`]), and the loop playing it.
    ambience: Option<Box<ModAmbience>>,
    ambience_loop: Option<SfxHandle>,
    /// How many times an ambience was started (the closing line of a run).
    ambiences: u32,
}

impl FieldState {
    pub fn new(mode: FieldSounds) -> Self {
        Self {
            mode,
            key: None,
            entered: 0.0,
            plan: None,
            wanted: Vec::new(),
            requested: false,
            left: None,
            clips: HashMap::new(),
            unheard: HashSet::new(),
            loops: [None, None, None, None],
            replaced: [false; 4],
            pending: Vec::new(),
            battle: false,
            links: Vec::new(),
            last_pos: None,
            seen_rejected: 0,
            last_story: None,
            started: BTreeMap::new(),
            played: BTreeMap::new(),
            ambience: None,
            ambience_loop: None,
            ambiences: 0,
        }
    }

    pub fn mode(&self) -> FieldSounds {
        self.mode
    }

    /// The links of the area the player stands in (see [`LinkSpot`]); the caller sets them when the area changes.
    pub fn set_links(&mut self, links: Vec<LinkSpot>) {
        self.links = links;
    }

    /// A scene's SOUND owns channel 1 until its run ends. Other ambient channels keep playing.
    pub fn scene_channel(&mut self, engine: &AudioEngine, log: &Logger, active: bool) {
        if active {
            if let Some(old) = self.loops[0].take() {
                engine.stop_sfx_loop(old.handle, Duration::ZERO);
                log(&format!("field loop stop ch1 {} (a scene sound replaces it)", old.id));
            }
        }
        self.replaced[0] = active;
    }

    /// The loops playing now, as `(channel, sound id)` (tests).
    #[cfg(test)]
    pub fn playing(&self) -> Vec<(u8, u16)> {
        self.loops.iter().enumerate().filter_map(|(c, l)| l.as_ref().map(|l| (c as u8 + 1, l.id))).collect()
    }

    /// What a run's field sounds came to, `None` when there were none: `4 sounds (21x3 62x1), 2 loops (38x1 60x1)`.
    pub fn summary(&self) -> Option<String> {
        let list = |m: &BTreeMap<u16, u32>| m.iter().map(|(id, n)| format!("{id}x{n}")).collect::<Vec<_>>().join(" ");
        let (sounds, loops): (u32, u32) = (self.played.values().sum(), self.started.values().sum());
        let ambiences = if self.ambiences > 0 { format!(", {} ambiences", self.ambiences) } else { String::new() };
        (sounds + loops + self.ambiences > 0).then(|| format!("{sounds} sounds ({}), {loops} loops ({}){ambiences}", list(&self.played), list(&self.started)))
    }

    /// Follow the game for one frame: leave and enter fields, ask for plans, start, change and stop loops, detect the
    /// events the plan has sounds for and play what is due. Cheap, never blocks.
    pub fn follow(&mut self, engine: &AudioEngine, worker: &mut dyn Worker, log: &Logger, frame: &FieldFrame<'_>) {
        if self.mode == FieldSounds::Off {
            return;
        }
        let story_changed = frame.story != self.wanted.as_slice();
        // A new field (or none): what the old one played goes.
        if frame.key != self.key.as_deref() {
            self.change_field(engine, log, frame);
        } else if story_changed {
            self.on_story(frame);
        }
        if story_changed {
            self.wanted = frame.story.to_vec();
        }
        if let Some(key) = frame.key {
            if !self.requested && self.plan.as_ref().is_none_or(|p| p.story != self.wanted) {
                self.requested = true;
                let have: Vec<u16> = self.clips.keys().copied().collect();
                worker.scan_field(key, self.wanted.clone(), have);
            }
        }
        // A fight takes the field's sounds over, and gives them back.
        if frame.in_battle != self.battle {
            self.battle = frame.in_battle;
            if self.battle {
                self.pending.clear();
                self.stop_all(engine, log, "a battle", FADE_OUT);
                self.stop_ambience(engine, log, "a battle");
            } else {
                self.replaced = [false; 4];
            }
        }
        if !self.battle {
            self.detect_link(frame);
            self.play_due(engine, log, frame.now);
            self.apply_ambient(engine, log, frame.now);
            self.apply_ambience(engine, log);
        }
        self.last_pos = frame.pos;
        self.seen_rejected = frame.rejected;
    }

    /// The player is in another field (or none).
    fn change_field(&mut self, engine: &AudioEngine, log: &Logger, frame: &FieldFrame<'_>) {
        self.stop_all(engine, log, "the field was left", FADE_OUT);
        self.stop_ambience(engine, log, "the field was left");
        self.ambience = None;
        self.pending.clear();
        self.replaced = [false; 4];
        let before = self.key.take();
        self.left = self.plan.take().filter(|p| before.as_deref() == Some(p.key.as_str()));
        self.key = frame.key.map(str::to_string);
        self.entered = frame.now;
        self.requested = false;
        self.last_story = StoryState::decode(frame.story);
        // A door the player just came through: the sounds its script plays on the way out.
        if let (Some(arrived), Some(plan), Some(key)) = (frame.pos, self.left.as_ref(), frame.key) {
            let from = self.last_pos;
            let door = plan.exits.iter().find(|e| {
                e.target.eq_ignore_ascii_case(key)
                    && (f32::from(e.arrival.0) - arrived.x).hypot(f32::from(e.arrival.1) - arrived.y) <= ARRIVAL
                    && match (e.line, from) {
                        (Some((a, b)), Some(from)) => near_line(from, a, b) <= DOOR_REACH,
                        _ => true,
                    }
            });
            if let Some(door) = door.cloned() {
                self.schedule(frame.now, &door.calls, "door");
            }
        }
        // The links of the new area are set by the caller (before this frame's `follow`).
        self.last_pos = None;
        self.seen_rejected = frame.rejected;
    }

    /// The story the client holds changed while the player stands in the same field: a flipped tracked bit plays what the
    /// script plays right after it (a new plan for the new story is asked for by `follow`).
    fn on_story(&mut self, frame: &FieldFrame<'_>) {
        let now = StoryState::decode(frame.story);
        if let (Some(before), Some(after), Some(plan)) = (self.last_story.as_ref(), now.as_ref(), self.plan.as_ref()) {
            let calls: Vec<PlannedCall> = plan
                .flips
                .iter()
                .filter(|f| {
                    let (was, is) = (before.known_bit(f.bank, f.address, f.bit), after.known_bit(f.bank, f.address, f.bit));
                    was.is_some() && is.is_some() && was != is && is == Some(f.on)
                })
                .flat_map(|f| f.calls.iter().copied())
                .collect();
            if !calls.is_empty() {
                self.schedule(frame.now, &calls, "lift");
            }
        }
        if now.is_some() {
            self.last_story = now;
        }
    }

    /// A link of the area the server just put the player through (it was told with a `MoveRejected`, and the player now stands
    /// where the link leads, having been somewhere else a moment ago): its sounds.
    fn detect_link(&mut self, frame: &FieldFrame<'_>) {
        let (Some(pos), Some(last)) = (frame.pos, self.last_pos) else { return };
        if frame.rejected <= self.seen_rejected || pos.dist_xy(last) <= LANDED {
            return;
        }
        let Some(spot) = self.links.iter().find(|s| s.spot.dist_xy(pos) <= LANDED && (s.spot.z - pos.z).abs() <= LANDED_Z) else { return };
        let Some(plan) = self.plan.as_ref().and_then(|p| p.links.iter().find(|l| l.line == spot.line && l.to == spot.to && l.needs_ok == spot.needs_ok)) else {
            return;
        };
        let calls = plan.calls.clone();
        self.schedule(frame.now, &calls, "link");
    }

    fn schedule(&mut self, now: f64, calls: &[PlannedCall], why: &'static str) {
        for call in calls {
            self.pending.push(Scheduled { due: now + f64::from(call.at) / FRAMES_PER_SECOND, call: *call, why });
        }
    }

    /// Play the calls that are due.
    fn play_due(&mut self, engine: &AudioEngine, log: &Logger, now: f64) {
        if self.pending.is_empty() {
            return;
        }
        self.pending.sort_by(|a, b| a.due.total_cmp(&b.due));
        let due = self.pending.iter().take_while(|s| s.due <= now).count();
        let ready: Vec<Scheduled> = self.pending.drain(..due).collect();
        for s in ready {
            self.play_call(engine, log, s.call, s.why);
        }
    }

    /// One call of an event: it replaces what its channel was playing (a stop only does that).
    fn play_call(&mut self, engine: &AudioEngine, log: &Logger, call: PlannedCall, why: &'static str) {
        let Some(index) = usize::from(call.channel).checked_sub(1).filter(|c| *c < self.loops.len()) else { return };
        if let Some(old) = self.loops.get_mut(index).and_then(Option::take) {
            engine.stop_sfx_loop(old.handle, FADE_OUT);
            log(&format!("field loop stop ch{} {} (a {why} sound replaces it)", call.channel, old.id));
        }
        if let Some(flag) = self.replaced.get_mut(index) {
            *flag = true;
        }
        if call.id == 0 {
            return;
        }
        let Some(clip) = self.clips.get(&call.id) else {
            if self.unheard.insert(call.id) {
                log(&format!("field sound {} ({why}) has no clip to play", call.id));
            }
            return;
        };
        engine.play_sfx(clip, EVENT_GAIN, pan_of(call.pan));
        *self.played.entry(call.id).or_default() += 1;
        log(&format!("field sound ch{} {} ({why})", call.channel, call.id));
    }

    /// Make the loops that play the ones the plan has for the story the client holds: a loop still wanted is left alone, one that is
    /// not fades out, a new one fades in (once its time has come and its clip is here).
    fn apply_ambient(&mut self, engine: &AudioEngine, log: &Logger, now: f64) {
        // (a plan for an older story is not applied: a newer one is on its way)
        if !self.plan.as_ref().is_some_and(|p| p.story == self.wanted) {
            return;
        }
        let elapsed = now - self.entered;
        for channel in 1..=4u8 {
            let index = usize::from(channel - 1);
            let want: Option<AmbientLoop> = self
                .plan
                .as_ref()
                .and_then(|p| p.ambient.iter().find(|a| a.channel == channel).copied())
                .filter(|_| !self.replaced.get(index).copied().unwrap_or(false));
            let have = self.loops.get(index).and_then(|l| l.as_ref().map(|l| l.id));
            if have == want.map(|w| w.id) {
                continue;
            }
            if let Some(old) = self.loops.get_mut(index).and_then(Option::take) {
                engine.stop_sfx_loop(old.handle, FADE_STORY);
                log(&format!("field loop stop ch{channel} {} (the story moved)", old.id));
            }
            let Some(want) = want else { continue };
            if elapsed < f64::from(want.at) / FRAMES_PER_SECOND {
                continue;
            }
            let Some(clip) = self.clips.get(&want.id) else {
                if self.unheard.insert(want.id) {
                    log(&format!("field loop {} has no clip to play", want.id));
                }
                continue;
            };
            let handle = engine.play_sfx_loop(clip, AMBIENT_GAIN, pan_of(want.pan), FADE_IN);
            if let Some(slot) = self.loops.get_mut(index) {
                *slot = Some(ActiveLoop { id: want.id, handle });
            }
            *self.started.entry(want.id).or_default() += 1;
            log(&format!("field loop start ch{channel} {}", want.id));
        }
    }

    /// Start the field's ambience from the sound layer when it is here and not playing (the caller does not call this in a battle).
    fn apply_ambience(&mut self, engine: &AudioEngine, log: &Logger) {
        let Some(ambience) = self.ambience.as_ref().filter(|_| self.ambience_loop.is_none()) else { return };
        self.ambience_loop = Some(engine.play_sfx_loop(&ambience.clip, AMBIENCE_GAIN, 0.0, ambience.fade_in));
        self.ambiences += 1;
        log(&format!("field ambience start {}", ambience.section));
    }

    /// Stop the ambience's loop, with its own fade (it starts again by [`FieldState::apply_ambience`] if the field still has it).
    fn stop_ambience(&mut self, engine: &AudioEngine, log: &Logger, why: &str) {
        let Some(handle) = self.ambience_loop.take() else { return };
        let (section, fade) = self.ambience.as_ref().map_or(("?", FADE_OUT), |a| (a.section.as_str(), a.fade_out));
        engine.stop_sfx_loop(handle, fade);
        log(&format!("field ambience stop {section} ({why})"));
    }

    /// Stop every loop (the field was left, a battle starts).
    fn stop_all(&mut self, engine: &AudioEngine, log: &Logger, why: &str, fade: Duration) {
        for (index, slot) in self.loops.iter_mut().enumerate() {
            if let Some(old) = slot.take() {
                engine.stop_sfx_loop(old.handle, fade);
                log(&format!("field loop stop ch{} {} ({why})", index + 1, old.id));
            }
        }
    }

    /// The worker's answer for the plan of `key` for `story`: keep the clips, and use the plan if it is for the field and the story
    /// the client is in now (a plan for an older story is dropped: another is asked for).
    pub fn on_read(&mut self, log: &Logger, key: &str, story: &[u8], read: Result<FieldRead, String>, took: Duration) {
        if self.mode == FieldSounds::Off || self.key.as_deref() != Some(key) {
            return;
        }
        self.requested = false;
        let read = match read {
            Ok(read) => read,
            Err(why) => {
                log(&format!("field {key}: cannot read its sounds ({why}); going on without them"));
                // Never asked for again for this story: an empty plan holds the place.
                self.plan = Some(FieldPlan { key: key.to_string(), story: story.to_vec(), ..FieldPlan::default() });
                return;
            }
        };
        for (id, clip) in read.clips.clips {
            self.clips.insert(id, clip);
        }
        // The ambience does not depend on the story: the first read of the field brings it.
        if let (None, Some(a)) = (self.ambience.as_ref(), read.ambience) {
            log(&format!(
                "field {key}: ambience {} from {} ({}; fades in over {:.2} s, out over {:.2} s)",
                a.section, a.source, a.name, a.fade_in.as_secs_f32(), a.fade_out.as_secs_f32()
            ));
            self.ambience = Some(a);
        }
        for (id, why) in &read.clips.missing {
            log(&format!("no clip for field sound {id} ({why}); going on without it"));
            self.unheard.insert(*id);
        }
        // Keep what the plan needs, and drop clips it does not need (in no particular order) until the limit holds.
        if self.clips.len() > MAX_CLIPS {
            let keep = read.plan.ids();
            let drop: Vec<u16> = self.clips.keys().copied().filter(|id| !keep.contains(id)).take(self.clips.len() - MAX_CLIPS).collect();
            for id in drop {
                self.clips.remove(&id);
            }
        }
        if story != self.wanted.as_slice() {
            // The story moved again while this was being read: a plan for it is asked for by the next `follow`.
            return;
        }
        let plan = read.plan;
        let moment = StoryState::decode(&plan.story).map_or_else(|| "?".to_string(), |s| s.moment.to_string());
        let ambient = if plan.ambient.is_empty() {
            "none".to_string()
        } else {
            plan.ambient.iter().map(|a| if a.at == 0 { format!("ch{}:{}", a.channel, a.id) } else { format!("ch{}:{}@{}f", a.channel, a.id, a.at) }).collect::<Vec<_>>().join(" ")
        };
        let withheld = if plan.withheld_links > 0 { format!(" (+{} untimed)", plan.withheld_links) } else { String::new() };
        log(&format!(
            "field {key} sounds (moment {moment}): ambient {ambient}; links {}{withheld}, lift {}, doors {}; unjudged {}, open {}; {} ms",
            plan.links.len(),
            plan.flips.len(),
            plan.exits.len(),
            plan.unjudged,
            plan.left_open,
            took.as_millis()
        ));
        self.plan = Some(plan);
    }
}

/// The balance a script's pan byte (0 left, 63 or 64 centre, 127 right) asks for, `-1.0..=1.0`.
fn pan_of(pan: u8) -> f32 {
    ((f32::from(pan.min(127)) - 63.5) / 63.5).clamp(-1.0, 1.0)
}

/// The distance on x and y from `p` to the segment `a`-`b`.
fn near_line(p: Pos, a: (i16, i16), b: (i16, i16)) -> f32 {
    let (px, py) = (f64::from(p.x), f64::from(p.y));
    let (ax, ay, bx, by) = (f64::from(a.0), f64::from(a.1), f64::from(b.0), f64::from(b.1));
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (px - (ax + t * dx)).hypot(py - (ay + t * dy)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripts_pan_byte_is_a_balance_with_63_and_64_in_the_middle() {
        assert!(pan_of(63).abs() < 0.01 && pan_of(64).abs() < 0.01, "centre: {} {}", pan_of(63), pan_of(64));
        assert_eq!((pan_of(0), pan_of(127)), (-1.0, 1.0), "left only and right only");
        assert_eq!(pan_of(255), 1.0, "a byte past the documented range is as far right as it goes");
        assert!(pan_of(32) < 0.0 && pan_of(95) > 0.0);
    }

    #[test]
    fn the_distance_to_a_line_is_the_distance_to_its_segment() {
        let p = |x: f32, y: f32| Pos::new(x, y, 0.0);
        assert_eq!(near_line(p(-170.0, 0.0), (-185, -68), (-185, 56)), 15.0, "beside the line");
        assert_eq!(near_line(p(-170.0, 100.0), (-185, -68), (-185, 56)), 15.0f32.hypot(44.0), "past its end: to the end point");
        assert_eq!(near_line(p(3.0, 4.0), (0, 0), (0, 0)), 5.0, "a line of no length is a point");
    }

    #[test]
    fn a_plan_with_nothing_in_it_names_no_clip() {
        let plan = FieldPlan::default();
        assert!(plan.is_empty() && plan.ids().is_empty());
    }
}
