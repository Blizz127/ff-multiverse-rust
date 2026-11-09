//! The sounds a field's own scripts play: what a field plays by itself when it loads, and what its ladders, jumps and
//! lift play when a player uses them. Reads the scripts only (no audio, no files beyond the field).
//!
//! **Verified on the owner's FF7 Steam Edition (en) on 2026-11-09**; every other edition is unverified. The numbers
//! below are counts and ids, never game bytes; `tests/field_sound_real.rs` (ignored; needs `FMV_FF7_DATA` and
//! `FMV_FF7_AUDIO_DATA`) and `examples/field_sound_survey.rs` recompute every one of them.
//!
//! # Two opcodes play sound effects, on four channels
//!
//! * `SOUND` (`F1`): "uses effect channel #1", a sound id and a pan (63 is centre, 0 left, 127 right; "NO volume
//!   control with this operation"). Qhimm forum, "Audio in Field Script Explained"
//!   <https://forums.qhimm.com/index.php?topic=16937.0> (DLPB_'s opening post and recap, read through a summarising
//!   fetch). 4 387 of them in 446 of the 702 fields, every id and pan a literal.
//! * `AKAO` (`F2`) and `AKAO2` (`DA`) with operation **40, 41, 42, 43**: "play sound effect on channel 1, 2, 3, 4" (the
//!   same thread). The operands: `p1` is the pan and `p2` the sound id, `p3`, `p4` and `p5` are 0. **2 579 plays (2 575
//!   `AKAO`, 4 `AKAO2` operation 41) in 260 fields**, none with a banked operand, every `p3`, `p4` and `p5` is 0 and
//!   every `p2` is 0 or in `1..=750`. The thread says the id is in the *eighth* argument when the ninth is 0 and moves
//!   to the ninth, with the eighth becoming the pan, otherwise; **in the owned data it is always the second form**:
//!   the 525 plays with `p2` = 0 carry a pan-like `p1` (64 in 432 of them, 63 in 65, 127 in 10) where an id would be
//!   anything, and are stops, as `SOUND 0` is.
//!   Between them the two opcodes play in 566 fields. The other `AKAO` operations (music, volumes, pan sweeps: 160 to
//!   175) are not read.
//! * **Sound id 0 stops the channel**, like `SOUND 0` ([`crate::sound`], "Sound ids"). A play on a channel replaces
//!   what it was playing.
//! * **Ids are 1-based here too** (entry `id - 1` of `audio.fmt`): no id of either opcode lands on an empty slot read
//!   that way (`tests/field_sound_real.rs` repeats the check for the 2 054 non-zero `AKAO` ids that
//!   [`crate::sound`] had only checked for `SOUND`).
//!
//! # What loops
//!
//! A clip loops when `audio.fmt` says so (its loop flag: 90 of the 724 clips, [`crate::sound`]); FFNx, which plays this
//! very file pair, decides it from that flag. The forum also says an effect "will loop if placed in a 'main' part of
//! script, otherwise it will play once": that is consistent with it (a sound in a script that runs over and over
//! plays over and over, and a loop clip repeats by itself), and this module uses the flag, which the data hold,
//! not the placement. Measured: **every one of the 120 plays in an init part is a stop or a loop clip** (a field's
//! init part sets up what stays on, never a one-shot), which is what an ambience is.
//!
//! # What a field plays by itself ([`start_up`])
//!
//! Every entity's script 0 runs when the field loads: its init part (up to the first `RET`), and then its main part.
//! The ambient sounds are what those leave playing on the four channels. [`start_up`] runs them with [`run`]'s
//! bounded straight-line runner: conditions are judged against the story ([`StoryState`]), so a field that says
//! "reactor hum before the bomb (the moment is not 27), alarm after it" gives the hum or the alarm for the story the
//! player has; a condition the runner cannot judge (a variable the story does not carry, the party, the random
//! number) stops that run and is counted ([`StartUp::unjudged`]), never guessed. The result is a timeline of plays
//! and stops; [`StartUp::ambient`] turns it into the loops that are on at its end.
//!
//! * **A loop counts only when nothing could still change its channel**: its run ended cleanly (a return), or no
//!   sound opcode of that channel lies in the part of the script the run did not reach ([`RunInfo::open`]).
//! * A one-shot in the start-up timeline (a cut-scene's sound in a director's main part) is **not played**: the
//!   picture it goes with is not shown. It still replaces a loop on its channel.
//!
//! # What a link plays ([`link_events`])
//!
//! A ladder or a jump is a line whose trigger script asks the player's own entity to run one of ITS scripts
//! (`REQ` or `REQEW`, [`crate::links`]), and those scripts play the sounds: in `nmkin_3` the jump is
//! `REQEW cl 5` = `DFANM; WAIT 15; SOUND 62; JUMP ...` and the ladder `REQ cl 8` = `SOUND 21; MOVE ...; LADER ...`.
//! [`link_events`] runs each link's trigger script (with OK held for a link that waits for OK) and reports its
//! plays with their times: **up to the first opcode whose duration the script does not give** (a `MOVE`, a `JUMP`, a
//! `LADER`): the take-off sound of the jump comes half a second in, the ladder's first sound at once, and the sounds
//! after the climb are not known in time and are not played.
//!
//! # What a flipped story bit plays ([`flip_events`])
//!
//! The lift of `elevtr1` is not a link: the panel's script sets a bit of the story (`BITON Var[1][225] bit 0`, the
//! tracked [`crate::story::REACTOR_LIFT`]) and its next opcode is `SOUND 42`, a loop clip, stopped by a `SOUND 0` after
//! the ride's scrolls. [`flip_events`] finds the scripts that set or clear a tracked bit and runs what they do
//! right after it, so that a client that sees the bit change plays that. The stop is behind waits the data do not
//! give, so **the caller plays such a loop once through, not for a time nobody knows**.
//!
//! # Not attributed (skipped on purpose)
//!
//! * Doors that are gateways: a gateway is data, no script runs. Script exits (a line that runs a `MAPJUMP`) with a
//!   sound before the jump exist (see [`exit_events`]) but the server offers script exits in the reviewed fields only.
//! * Sounds a script plays inside a loop (periodic effects: a spark every 2 to 4 seconds in `md8_3`): their rhythm is a
//!   random number, a variable or an animation, none of which a run knows.
//! * Cut-scene one-shots, the sounds of a talk script and the sounds that follow a walk or a climb; the volume and pan
//!   envelopes of `AKAO` 160 to 175 (a sound plays at its start pan and the master volume).
//! * What runs concurrently with a wait on another script's variable (anything that reads a variable the story does
//!   not carry).
//!
//! # Untrusted data
//!
//! Like the rest of the crate: no indexing, no unchecked arithmetic, no panics on script bytes; every run is bounded
//! (see [`run`]).

use std::collections::{BTreeMap, BTreeSet};

use fmv_core::Pos;

use crate::field::Walkmesh;
use crate::links::scan_links;
use crate::script::{ExitTrigger, FieldScript, Maplist, ScriptRef, INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use crate::story::{tracked_var, StoryState};

pub mod run;

pub use run::{open_for, Call, Env, Machine, Part, RunInfo, Source, Stop, Timed, Unjudged, MAX_CALL_DEPTH, MAX_FRAMES, MAX_STEPS};

/// The field module's frame rate: how many [`Timed::at`] frames make a second (the same constant the walking speeds use).
pub const FRAMES_PER_SECOND: u32 = 30;

/// Opcodes of the instructions that set or clear a bit of a variable.
const OP_BITON: u8 = 0x82;
const OP_BITOFF: u8 = 0x83;

/// How many frames `frames` is, in seconds.
pub fn seconds(frames: u32) -> f64 {
    f64::from(frames) / f64::from(FRAMES_PER_SECOND)
}

fn script_ref(entity: usize, slot: usize) -> Option<ScriptRef> {
    Some(ScriptRef { entity, slot: u8::try_from(slot).ok()? })
}

/// A loop that is on when a field's start-up is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ambient {
    pub channel: u8,
    pub id: u16,
    pub pan: u8,
    /// Frames after the field loaded that it starts (0 for what an init part starts).
    pub at: u32,
    /// The script that starts it and the opcode's offset.
    pub by: ScriptRef,
    pub offset: usize,
}

/// What a field's own scripts play when it loads, for one story: see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartUp {
    /// Every play and stop of the runs, in time order (ties in the order the runs met them).
    pub events: Vec<Timed>,
    /// The runs, one per entity part (and one per script they `REQ`).
    pub runs: Vec<RunInfo>,
    /// The conditions that stopped runs: skipped, never guessed.
    pub unjudged: Vec<Unjudged>,
    /// Sound opcodes whose id or pan names a variable (skipped).
    pub banked: u32,
}

impl StartUp {
    /// The loops that are on when the timeline ends, for a clip set where `is_loop(id)` says which ids loop (the
    /// clip's loop flag in `audio.fmt`): walk the events in order, a stop empties a channel, a loop clip fills it, any
    /// other clip empties it (a one-shot replaced what was there). A loop whose channel could still be changed by a part of
    /// its script the run did not reach is left out (`open`, see the module docs).
    pub fn ambient(&self, is_loop: impl Fn(u16) -> bool) -> Vec<Ambient> {
        let mut state: [Option<Timed>; 4] = [None; 4];
        for e in &self.events {
            let Some(slot) = usize::from(e.call.channel).checked_sub(1).and_then(|c| state.get_mut(c)) else { continue };
            *slot = (e.call.id != 0 && is_loop(e.call.id)).then_some(*e);
        }
        state
            .into_iter()
            .flatten()
            .filter(|e| open_for(&self.runs, e.run) & e.call.channel_bit() == 0)
            .map(|e| Ambient { channel: e.call.channel, id: e.call.id, pan: e.call.pan, at: e.at, by: e.by, offset: e.offset })
            .collect()
    }

    /// The loops [`StartUp::ambient`] leaves out only because their channel is still open (for the survey's count of
    /// what the rule costs).
    pub fn ambient_left_open(&self, is_loop: impl Fn(u16) -> bool) -> usize {
        let mut state: [Option<Timed>; 4] = [None; 4];
        for e in &self.events {
            let Some(slot) = usize::from(e.call.channel).checked_sub(1).and_then(|c| state.get_mut(c)) else { continue };
            *slot = (e.call.id != 0 && is_loop(e.call.id)).then_some(*e);
        }
        state.into_iter().flatten().filter(|e| open_for(&self.runs, e.run) & e.call.channel_bit() != 0).count()
    }

    /// How many runs ended each way, by [`Stop::label`] (the survey's table of why runs stop).
    pub fn stops(&self) -> BTreeMap<&'static str, usize> {
        let mut tally = BTreeMap::new();
        for run in &self.runs {
            *tally.entry(run.stop.label()).or_default() += 1;
        }
        tally
    }
}

/// Run the init part and the main part of every entity's script 0, for `story` (see the module docs).
pub fn start_up(script: &FieldScript<'_>, story: &StoryState) -> StartUp {
    let mut machine = Machine::new(script, Env { story, keys: 0 });
    for entity in 0..script.entity_count() {
        let Some(at) = script_ref(entity, INIT_SCRIPT) else { continue };
        if script.span(entity, INIT_SCRIPT).is_none() {
            continue;
        }
        let init = machine.run(at, Part::Init, None, 0);
        // The main part starts when the init part has returned.
        let after_init = machine.runs.get(init).and_then(|r| r.frames).unwrap_or(0);
        machine.run(at, Part::Main, None, after_init);
    }
    let mut events = machine.events;
    events.sort_by_key(|e| e.at);
    StartUp { events, runs: machine.runs, unjudged: machine.unjudged, banked: machine.banked }
}

/// The sounds one link of a field plays when a player uses it, with the link's identity as the area's list has it
/// (`line`, `to` and `to_hint` equal those of the `fmv_adapter_api::Link` the area offers).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSounds {
    pub entity: usize,
    pub slot: usize,
    pub line: (Pos, Pos),
    pub to: Pos,
    pub to_hint: Option<u32>,
    pub needs_ok: bool,
    /// What the trigger script plays, with times, up to where the run could go on.
    pub events: Vec<Timed>,
    /// Why the trigger script's own run stopped (a script that waits for one it started, `REQEW`, stops with that
    /// opcode as its reason when the other cannot be timed: the reason it could not is in [`LinkSounds::runs`]).
    pub stop: Stop,
    /// Every run of the link (the trigger script's first, then the scripts it requested).
    pub runs: Vec<RunInfo>,
    pub unjudged: Vec<Unjudged>,
}

/// The sounds of every link of a field ([`crate::links::scan_links`]: the ladders and jumps its scripts define), for `story`.
/// A link that plays nothing is left out.
pub fn link_events(script: &FieldScript<'_>, mesh: &Walkmesh, story: &StoryState) -> Vec<LinkSounds> {
    let mut out = Vec::new();
    for def in scan_links(script, mesh).found {
        let Some(at) = script_ref(def.entity, def.slot) else { continue };
        let keys = if def.needs_ok { crate::script::dialogue::KEY_OK } else { 0 };
        let mut machine = Machine::new(script, Env { story, keys });
        let run = machine.run(at, Part::Whole, None, 0);
        if machine.events.is_empty() {
            continue;
        }
        let stop = machine.runs.get(run).map_or(Stop::Broken, |r| r.stop);
        out.push(LinkSounds {
            entity: def.entity,
            slot: def.slot,
            line: def.line,
            to: def.to,
            to_hint: Some(def.triangle),
            needs_ok: def.needs_ok,
            events: machine.events,
            stop,
            runs: machine.runs,
            unjudged: machine.unjudged,
        });
    }
    out
}

/// The sounds a script plays right after it sets or clears a bit the story tracks (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct FlipSounds {
    pub bank: u8,
    pub address: u8,
    pub bit: u8,
    /// The script turns the bit on (`BITON`) or off (`BITOFF`).
    pub on: bool,
    /// The script and the offset of the `BITON`/`BITOFF`.
    pub by: ScriptRef,
    pub offset: usize,
    /// What the script plays after it, with times from the flip, up to where the run could go on.
    pub events: Vec<Timed>,
    pub stop: Stop,
    pub runs: Vec<RunInfo>,
}

/// The sounds that follow every `BITON` and `BITOFF` of a tracked bit ([`crate::story::TRACKED_VARS`]) in the field's scripts,
/// run for `story` (the story AFTER the flip). A flip that is followed by no sound is left out.
pub fn flip_events(script: &FieldScript<'_>, story: &StoryState) -> Vec<FlipSounds> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) {
                continue;
            }
            let (Some(at), Ok(d)) = (script_ref(entity, slot), script.disassemble_partial(entity, slot)) else { continue };
            for (i, ins) in d.instructions.iter().enumerate().filter(|(_, ins)| ins.opcode == OP_BITON || ins.opcode == OP_BITOFF) {
                let Some(var) = ins.operand("var").filter(|o| o.bank != 0) else { continue };
                let Some(bit) = ins.literal("bit").and_then(|b| u8::try_from(b).ok()) else { continue };
                let (bank, address) = (var.bank, u8::try_from(var.value & 0xFF).unwrap_or(0));
                let carried = tracked_var(bank, address).is_some_and(|t| 1u8.checked_shl(u32::from(bit)).is_some_and(|m| t.bits & m != 0));
                let Some(next) = d.instructions.get(i + 1).filter(|_| carried) else { continue };
                let mut machine = Machine::new(script, Env { story, keys: 0 });
                let run = machine.run(at, Part::Whole, Some(next.offset), 0);
                if machine.events.is_empty() {
                    continue;
                }
                let stop = machine.runs.get(run).map_or(Stop::Broken, |r| r.stop);
                out.push(FlipSounds { bank, address, bit, on: ins.opcode == OP_BITON, by: at, offset: ins.offset, events: machine.events, stop, runs: machine.runs });
            }
        }
    }
    out
}

/// The sounds a script exit (a line whose script runs a `MAPJUMP`) plays on the way out, with the exit's identity.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitSounds {
    pub trigger: ExitTrigger,
    /// The field the exit leads to (the maplist's name) and where it puts the player there.
    pub target: String,
    pub arrival: (i16, i16),
    pub triangle: i16,
    /// The line that sets it off, when it is a line.
    pub line: Option<((i16, i16), (i16, i16))>,
    /// What the scripts on the way to the jump play before it (the run ends at the `MAPJUMP`).
    pub events: Vec<Timed>,
    pub stop: Stop,
    pub runs: Vec<RunInfo>,
}

/// The sounds on the way out of every script exit that is set off by a walk onto a line ([`ExitTrigger::is_walk_on`]),
/// for `story`; exits that play nothing before their jump are left out. The run is the exit's root script, and it ends
/// at the `MAPJUMP` ([`Stop::LeavesField`]).
pub fn exit_events(script: &FieldScript<'_>, maplist: &Maplist, story: &StoryState) -> Vec<ExitSounds> {
    let mut out = Vec::new();
    for exit in script.script_exits(maplist).into_iter().filter(|e| e.trigger.is_walk_on()) {
        let (Some(root), Some(target)) = (exit.path.first().copied(), exit.target.clone()) else { continue };
        let mut machine = Machine::new(script, Env { story, keys: 0 });
        let run = machine.run(root, Part::Whole, None, 0);
        // Only what happens before the jump this exit is: the run of a root that has several jumps ends at the first
        // one it reaches (the story decides which), which is this exit's only if it is the very same instruction.
        let reaches = machine.runs.get(run).is_some_and(|r| r.stop == Stop::LeavesField { at: exit.offset });
        if machine.events.is_empty() || !reaches {
            continue;
        }
        let stop = machine.runs.get(run).map_or(Stop::Broken, |r| r.stop);
        let line = exit.line.map(|l| ((l.a[0], l.a[1]), (l.b[0], l.b[1])));
        out.push(ExitSounds { trigger: exit.trigger, target, arrival: (exit.x, exit.y), triangle: exit.triangle, line, events: machine.events, stop, runs: machine.runs });
    }
    out
}

#[cfg(test)]
mod tests;
