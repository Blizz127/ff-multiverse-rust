//! The scene interpreter: a small, bounded, read-only VM that runs a field's own scripts as the game does, side by
//! side, and says what the stage shows and which windows open, in order.
//!
//! # What it is
//!
//! The dialogue walker ([`crate::script::dialogue`]) runs ONE character's talk script far enough to collect its windows
//! and ignores everything that moves. A scene needs more: the conversation at the counter is a script of Tifa's that
//! asks Cloud's script to sit him down (`REQEW`), waits, asks him a question, walks Tifa away to make the drink,
//! has the camera follow and ends by starting two more scripts (`REQSW`) whose lines come after. So this VM runs
//! **threads** (every script a script requests is a thread of its own), each on its own **virtual clock** in field
//! frames (30 a second), and always runs the thread that is earliest, so that the order of windows and stage events
//! is the order the engine would give them, whatever the script's timing says (`WAIT`, a walk's length, an
//! animation's, a camera scroll's).
//!
//! # What it produces
//!
//! [`Beat`]s: the stage events up to the next window (placements, walks, turns, animations, visibility, sound, camera
//! moves, each stamped with the frame it happens at, counted from the previous window's opening), then the window
//! (a `MESSAGE` or an `ASK`, with the field's own text exactly as written) or the end of the scene. A window pauses
//! the whole scene until the caller resumes the VM (the engine pauses only the script that opened it; the others'
//! animations go on. Known gap).
//!
//! # What it never does
//!
//! * It never touches the world: the variables it writes are its own ([`Vm::writes`]), the caller decides what to
//!   keep ([`super::Writes`]).
//! * It never guesses. An opcode it does not know (or one that changes nothing a scene shows: palettes, music,
//!   background layers) is skipped and COUNTED by mnemonic ([`Stats::unknown`], [`Stats::ignored`]); an operand it
//!   cannot read is not made up.
//! * It never runs away: [`Limits`] bound instructions, windows, threads and virtual time, a script that cannot be
//!   decoded ends the scene ([`End::Broken`]), and a loop that waits for something no thread can change is
//!   *parked*, not run (below). Nothing indexes a slice, divides by a script's number or shifts by one unchecked.
//!
//! # How a scene ends
//!
//! When no thread can run any more: every thread has ended, or the ones left are parked. A thread is **parked**
//! when it meets an instruction a second time while nothing at all (a variable, a stage event, a thread, a window)
//! has changed since it met it the first time: an idle loop. It runs again if anything changes. So a background
//! script that loops for ever (a bartender's idle walk) does not hold a scene open, and a tail a script requested
//! just before it returned (`REQSW`) does run to its end. A `MAPJUMP`, `BATTLE`, `MINIGAME` or `GAMEOVER` ends it at
//! once ([`End`]); the world is not changed by any of them.
//!
//! # Requests and priorities
//!
//! `REQ` starts the requested script as a thread and goes on; `REQSW` does the same and lets the new thread run
//! first (the requester waits for it to *start*); `REQEW` blocks the requester until the thread has ended. A thread
//! runs on its entity's priority slot ([`ScriptRef`](super::ScriptRef)'s entity and the request's priority): a
//! request for a slot that is taken waits for it to be free (the engine's own rule for a taken slot is not
//! documented; waiting loses no line).

use super::data::{Given, ScriptRef, Step};
use crate::field_direction_to_facing;
use crate::script::dialogue::{parse_message, Choice, End, Message, MessageOrigin, Piece, StoryVars, EFFECT_NOTE_LAYER};
use crate::script::{decode_at, FieldScript, Instruction};
use crate::story::{tracked_var, PartyOp, StoryState, MOMENT_ADDRESS, MOMENT_BANK};
use std::collections::{BTreeMap, BTreeSet};

/// Field frames per second (the clock of [`Timed::at`]).
pub const FRAMES_PER_SECOND: u32 = 30;

/// Move speed a script has not set (`MSPED`): a walk.
const DEFAULT_MOVE_SPEED: i32 = 1024;
/// `MSPED` units per walkmesh unit per frame at field scale 512: 1024 walks at 2 units a frame and 4096 runs at 8,
/// the walk and run speeds of [`crate::area`] (UNVERIFIED hypotheses, like theirs: only pacing depends on it).
const MOVE_SPEED_PER_UNIT: f32 = 512.0;
/// The longest a walk is taken to last, in frames.
const MAX_MOVE_FRAMES: u32 = 900;
/// A clip's length when the script does not say (`CANIM` ranges do): the common one of the field models.
const DEFAULT_CLIP_FRAMES: u32 = 15;
/// Animation speed when the script has not set one (`ASPED`: 16 is one frame per frame).
const DEFAULT_ANIM_SPEED: u32 = 16;
/// How long a turn takes, in frames.
const TURN_FRAMES: u32 = 8;
/// How long a ladder or a jump takes, in frames.
const JUMP_FRAMES: u32 = 30;
/// Most threads a run ever starts, ended ones included (the table is bounded, whatever a script does).
const MAX_THREADS_EVER: usize = 4096;

/// How far one run of the scene may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Instructions run, in all threads together.
    pub steps: usize,
    /// Windows handed out.
    pub windows: usize,
    /// Threads alive at once.
    pub threads: usize,
    /// Virtual time, in frames, from the start of the scene (windows do not count: they wait for a person).
    pub frames: u32,
}

impl Default for Limits {
    fn default() -> Self {
        // Far above the proof scene (a few hundred instructions, a dozen windows, minutes of virtual time at most).
        Self { steps: 200_000, windows: 64, threads: 64, frames: FRAMES_PER_SECOND * 60 * 10 }
    }
}

/// A time-stamped thing: `at` is the frame it happens at, counted from the opening of the previous window (0 for the
/// first beat).
#[derive(Debug, Clone, PartialEq)]
pub struct Timed<T> {
    pub at: u32,
    pub what: T,
}

/// How a walk goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveHow {
    /// `MOVE`, `CMOVE`, `FMOVE`, `MOVA`: walks (or runs: the frames say how long).
    Walk,
    /// `JUMP`.
    Jump,
    /// `LADER`: up or down a ladder.
    Ladder,
}

/// How an animation plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimHow {
    /// `DFANM`: the clip the actor loops until another is set.
    Loop,
    /// Plays once and returns to standing (`ANIME1`, `ANIME2`, `CANIM1`, `CANIM2`).
    Once,
    /// Plays once and holds its last frame (`ANIM!1`, `ANIM!2`, `CANM!1`, `CANM!2`).
    Hold,
}

/// What a scene does to the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraOp {
    /// Scroll the view to (`x`, `y`) in the field's own screen units, over `frames` (0: at once).
    Scroll { x: i16, y: i16, frames: u32 },
    /// Follow an entity (`SCRLA`).
    Follow { entity: u8 },
    /// Give the camera back to the field (`SCRCC`).
    Reset,
}

/// One thing the stage shows. `entity` is the script's own entity number (the cast maps it to an actor).
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Stand at a place at once (`XYZI`, `XYI`, `XYZ`). `z` is the script's height, when it gives one (else the ground's).
    Place { entity: u8, x: i16, y: i16, z: Option<i16>, triangle: Option<u16> },
    /// Walk to a place over `frames`, from where the entity stood (`from`, when the scene knows it). `z` is the
    /// script's height (a ladder's), else the ground's at the place ([`super::ScenePlay`] fills it in).
    /// `triangle` preserves the authored landing floor of a jump or ladder when one is supplied.
    Move { entity: u8, from: Option<(i16, i16)>, x: i16, y: i16, z: Option<i16>, triangle: Option<u16>, frames: u32, how: MoveHow },
    /// Turn to face a heading (radians, counter-clockwise from the field's +x axis seen from above) over `frames`.
    Face { entity: u8, heading: f32, frames: u32 },
    /// Play a clip of the entity's model (`animation` is the index in the model-loader's list).
    Anim { entity: u8, animation: u8, how: AnimHow, first: Option<u8>, last: Option<u8>, speed: u8 },
    /// Show or hide (`VISI`).
    Show { entity: u8, visible: bool },
    /// Play a literal `SOUND` on effect channel 1; id 0 stops that channel. Pan is the script's byte.
    Sound { id: u16, pan: u8 },
    Camera(CameraOp),
}

/// A window the scene opens.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// The frame it opens at, counted from the previous window's opening.
    pub at: u32,
    /// The script entity whose script opened it (who SPEAKS is the first line's name: [`Window::speaker`]).
    pub opener: u8,
    /// The cast entity named on the message's first line (the game's own convention: a name, then the quotation), set
    /// by [`super::ScenePlay`] from the scene's speakers; the VM itself does not know the cast.
    pub speaker: Option<u8>,
    /// The window number the script asked for.
    pub id: u8,
    /// `WINDOW`'s place and size, when the script set them.
    pub geometry: Option<(i16, i16, u16, u16)>,
    /// The text, exactly as the field writes it, and the choices of an `ASK` (`message.choice`).
    pub message: Message,
}

/// What ends a beat.
#[derive(Debug, Clone, PartialEq)]
pub enum Then {
    /// A window opens: show it, and resume the VM with the answer when it asked.
    Window(Window),
    /// The scene is over.
    End { at: u32, how: End },
}

/// The stage events up to the next window (or the end), and that window (or the end).
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    pub stage: Vec<Timed<Stage>>,
    pub then: Then,
}

/// What the VM did, by opcode (the names are the opcode table's mnemonics).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub steps: usize,
    pub windows: usize,
    pub threads: usize,
    /// Opcodes the VM carries out.
    pub handled: BTreeMap<&'static str, u32>,
    /// Opcodes not presented (music, other sound commands, palettes, background layers, field logic): skipped.
    pub ignored: BTreeMap<&'static str, u32>,
    /// Opcodes it does not know: skipped, never guessed.
    pub unknown: BTreeMap<&'static str, u32>,
    /// Threads parked in an idle loop.
    pub parked: u32,
    /// Exact animation-only tails recognized after all other threads finished or parked in self-jumps.
    pub quiescent_tails: u32,
    /// Requests that waited for a taken priority slot.
    pub queued: u32,
    /// Requests that could not be made (no such script, too many threads).
    pub dropped_requests: u32,
    /// Reads the VM could not answer (a read-back of a place it does not know).
    pub unresolved: u32,
    /// Setup stopped at a budget or at an unexpected window/scene end while scripts were still running.
    pub setup_incomplete: bool,
}

/// The destination of the MAPJUMP that ended a scene, as authored in the field script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneJump {
    pub field: u16,
    pub x: i16,
    pub y: i16,
    pub triangle: i16,
    pub direction: u8,
}

impl Stats {
    fn note(&mut self, class: Class, mnemonic: &'static str) {
        let table = match class {
            Class::Handled => &mut self.handled,
            Class::Ignored => &mut self.ignored,
            Class::Unknown => &mut self.unknown,
        };
        let count = table.entry(mnemonic).or_insert(0);
        *count = count.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Handled,
    Ignored,
    Unknown,
}

/// The variables a scene reads when its threads have not written them: the moment and the tracked variables from the
/// player's story, everything else from what the scene was found with ([`Given`]), else 0.
#[derive(Debug, Clone)]
pub struct SceneVars {
    pub story: StoryState,
    pub given: Vec<Given>,
    unknown_persistent_reads: std::cell::RefCell<std::collections::BTreeSet<(u8, u8)>>,
}

impl SceneVars {
    pub fn new(story: StoryState, given: &[Given]) -> Self {
        Self { story, given: given.to_vec(), unknown_persistent_reads: Default::default() }
    }

    pub fn unknown_persistent_reads(&self) -> std::collections::BTreeSet<(u8, u8)> {
        self.unknown_persistent_reads.borrow().clone()
    }
}

impl StoryVars for SceneVars {
    fn byte(&self, bank: u8, address: u8) -> u8 {
        if (1..=4).contains(&bank) && self.story.known_byte(bank, address).is_none() {
            self.unknown_persistent_reads.borrow_mut().insert((bank, address));
        }
        let moment = bank == MOMENT_BANK && address <= MOMENT_ADDRESS.wrapping_add(1);
        if !moment {
            if let Some(g) = self.given.iter().find(|g| g.bank == bank && g.address == address) {
                // The bits the story model carries are the player's story's own; the rest are what the scene was found with.
                return match tracked_var(bank, address) {
                    Some(t) => (g.value & !t.bits) | (self.story.byte(bank, address) & t.bits),
                    None => g.value,
                };
            }
        }
        self.story.byte(bank, address)
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        self.story.party_member(slot)
    }
}

/// Where an entity stands, as far as the scene knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Where {
    pub x: i16,
    pub y: i16,
    pub z: Option<i16>,
    pub triangle: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Run,
    /// Blocked until thread `n` has ended (`REQEW`).
    WaitEnd(usize),
    /// Waiting for its entity's priority slot.
    WaitSlot,
    /// In an idle loop: runs again when anything changes (the epoch it parked at is the one stored).
    Parked(u64),
    Done,
}

#[derive(Debug, Clone)]
enum Code {
    Script { pc: usize },
    /// The scene's own start: the steps the field's trigger takes, in order.
    Root { steps: Vec<Step>, next: usize },
}

#[derive(Debug, Clone)]
struct Thread {
    entity: usize,
    prio: u8,
    code: Code,
    time: u32,
    state: State,
    seq: u64,
    seen: BTreeSet<usize>,
    seen_epoch: u64,
    ended_at: u32,
}

/// An `ASK` waiting for its answer.
#[derive(Debug, Clone, Copy)]
struct Pending {
    var: Option<(u8, u8)>,
    first: usize,
    last: usize,
    cursor: usize,
}

/// What one instruction did to its thread.
enum Flow {
    /// Run the next instruction of the same thread (the scheduler may still pick another).
    Next,
    Yield,
    Window(Opened),
    Scene(End),
}

struct Opened {
    opener: usize,
    id: u8,
    time: u32,
    message: Message,
    pending: Option<Pending>,
}

/// Where a walk goes.
struct Goal {
    x: i16,
    y: i16,
    z: Option<i16>,
    triangle: Option<u16>,
    how: MoveHow,
    /// How long it takes, when the script says (else the distance and the entity's speed do).
    frames: Option<u32>,
}

/// The width an operand is read at.
#[derive(Debug, Clone, Copy)]
enum Width {
    U8,
    S16,
    U16,
}

/// What the arithmetic opcodes do to their variable.
#[derive(Debug, Clone, Copy)]
enum Arith {
    Set,
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Inc,
    Dec,
}

/// `(operation, word, saturating)` of an arithmetic opcode (the same table as the dialogue walker's).
fn arith_of(opcode: u8) -> Option<(Arith, bool, bool)> {
    Some(match opcode {
        0x76 => (Arith::Plus, false, true),
        0x77 => (Arith::Plus, true, true),
        0x78 => (Arith::Minus, false, true),
        0x79 => (Arith::Minus, true, true),
        0x7A => (Arith::Inc, false, true),
        0x7B => (Arith::Inc, true, true),
        0x7C => (Arith::Dec, false, true),
        0x7D => (Arith::Dec, true, true),
        0x80 => (Arith::Set, false, false),
        0x81 => (Arith::Set, true, false),
        0x85 => (Arith::Plus, false, false),
        0x86 => (Arith::Plus, true, false),
        0x87 => (Arith::Minus, false, false),
        0x88 => (Arith::Minus, true, false),
        0x89 => (Arith::Mul, false, false),
        0x8A => (Arith::Mul, true, false),
        0x8B => (Arith::Div, false, false),
        0x8C => (Arith::Div, true, false),
        0x8D => (Arith::Mod, false, false),
        0x8E => (Arith::Mod, true, false),
        0x8F => (Arith::And, false, false),
        0x90 => (Arith::And, true, false),
        0x91 => (Arith::Or, false, false),
        0x92 => (Arith::Or, true, false),
        0x93 => (Arith::Xor, false, false),
        0x94 => (Arith::Xor, true, false),
        0x95 => (Arith::Inc, false, false),
        0x96 => (Arith::Inc, true, false),
        0x97 => (Arith::Dec, false, false),
        0x98 => (Arith::Dec, true, false),
        _ => return None,
    })
}

/// Whether bit `bit` of `value` is set (`false` for a bit the operand's width does not have).
fn bit_set(value: i32, bit: i32, bits: u32) -> bool {
    u32::try_from(bit).ok().filter(|&b| b < bits).and_then(|b| value.checked_shr(b)).is_some_and(|v| v & 1 == 1)
}

/// The eleven operators of the `IF*` opcodes: `==`, `!=`, `>`, `<`, `>=`, `<=`, the three bitwise ones (true when the
/// result is not 0), "bit on" and "bit off". Anything else is false.
fn compare(op: i32, left: i32, right: i32, bits: u32) -> bool {
    match op {
        0 => left == right,
        1 => left != right,
        2 => left > right,
        3 => left < right,
        4 => left >= right,
        5 => left <= right,
        6 => left & right != 0,
        7 => left ^ right != 0,
        8 => left | right != 0,
        9 => bit_set(left, right, bits),
        10 => !bit_set(left, right, bits),
        _ => false,
    }
}

/// Number of lines on the last page of `pieces`: the count an `ASK`'s line numbers refer to.
fn last_page_lines(pieces: &[Piece]) -> usize {
    let mut lines = 1usize;
    for piece in pieces {
        match piece {
            Piece::NewPage => lines = 1,
            Piece::NewLine => lines = lines.saturating_add(1),
            _ => {}
        }
    }
    lines
}

/// The note shown after a canon line that announced a gift the scene does not give (the walker's own wording).
fn gift_note() -> Message {
    Message::new(
        MessageOrigin::Added { layer: EFFECT_NOTE_LAYER.to_string() },
        vec![Piece::Text("(Nothing was given: picking up items from".to_string()), Piece::NewLine, Piece::Text("characters is not in the game yet.)".to_string())],
    )
}

fn clamp_i16(v: i32) -> i16 {
    i16::try_from(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
}

fn clamp_u8(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

/// The VM (see the module docs).
#[derive(Debug, Clone)]
pub struct Vm {
    jump: Option<SceneJump>,
    battle: Option<u16>,
    limits: Limits,
    vars: SceneVars,
    writes: BTreeMap<(u8, u8), u8>,
    threads: Vec<Thread>,
    slots: BTreeMap<(usize, u8), usize>,
    seq: u64,
    epoch: u64,
    steps: usize,
    windows: usize,
    rng: u32,
    /// The frame the previous window opened at: stage events and windows are stamped relative to it.
    origin: u32,
    /// The latest time any thread has run at.
    clock: u32,
    out: Vec<Timed<Stage>>,
    pending: Option<Pending>,
    queued_note: Option<(usize, Message)>,
    gift_not_given: bool,
    done: Option<End>,
    quiet: bool,
    places: BTreeMap<u8, Where>,
    headings: BTreeMap<u8, f32>,
    visible: BTreeMap<u8, bool>,
    speeds: BTreeMap<u8, i32>,
    anim_speeds: BTreeMap<u8, u32>,
    anim_ends: BTreeMap<u8, u32>,
    camera_end: u32,
    geometry: BTreeMap<u8, (i16, i16, u16, u16)>,
    /// What the scripts did to the party, in the order they did it (setup included): see [`Vm::party_ops`].
    party_ops: Vec<PartyOp>,
    stats: Stats,
}

impl Vm {
    pub fn new(vars: SceneVars, seed: u32, limits: Limits) -> Self {
        Self {
            jump: None,
            battle: None,
            limits,
            vars,
            writes: BTreeMap::new(),
            threads: Vec::new(),
            slots: BTreeMap::new(),
            seq: 0,
            epoch: 0,
            steps: 0,
            windows: 0,
            // Spread the seed over the whole word (a small one would give a run of small numbers).
            rng: seed.wrapping_mul(0x9E37_79B9) | 1,
            origin: 0,
            clock: 0,
            out: Vec::new(),
            pending: None,
            queued_note: None,
            gift_not_given: false,
            done: None,
            quiet: false,
            places: BTreeMap::new(),
            headings: BTreeMap::new(),
            visible: BTreeMap::new(),
            speeds: BTreeMap::new(),
            anim_speeds: BTreeMap::new(),
            anim_ends: BTreeMap::new(),
            camera_end: 0,
            geometry: BTreeMap::new(),
            party_ops: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// The party as the scene's scripts see it: the story's own, with what they have done to it so far.
    fn party_now(&self) -> Vec<u8> {
        let story: Vec<u8> = (0..3).filter_map(|slot| self.vars.party_member(slot)).collect();
        crate::story::party_after(&story, &self.party_ops)
    }

    /// What the scripts did to the story's party (`PRTYP`, `PRTYM`, `PRTYE`), in order, the stage scripts' part first.
    /// The VM changes no party: it only says what was asked, so that the caller can hold it against the party the story
    /// has after the scene ([`crate::story::party_after`], [`crate::story::canon_party`]) and refuse a scene whose party
    /// the story would not carry.
    pub fn party_ops(&self) -> &[PartyOp] {
        &self.party_ops
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn destination(&self) -> Option<SceneJump> { self.jump }

    /// The counters so far, which start again from nothing (the setup's are kept apart from the scene's).
    pub fn take_stats(&mut self) -> Stats {
        std::mem::take(&mut self.stats)
    }

    /// The variables the scene wrote, as `(bank, address)` to byte.
    pub fn writes(&self) -> &BTreeMap<(u8, u8), u8> {
        &self.writes
    }

    /// The variable base the scene reads (the player's story and what the scene was found with).
    pub fn vars(&self) -> &SceneVars {
        &self.vars
    }

    /// Tell the scene where `entity` stands (and which way it faces) when the stage scripts did not say: what a
    /// cast member's own start is.
    pub fn seed(&mut self, entity: u8, at: Where, heading: Option<f32>) {
        self.places.entry(entity).or_insert(at);
        if let Some(h) = heading {
            self.headings.entry(entity).or_insert(h);
        }
    }

    /// Where the scene knows `entity` stands.
    pub fn place_of(&self, entity: u8) -> Option<Where> {
        self.places.get(&entity).copied()
    }

    /// The heading the scene knows `entity` has.
    pub fn heading_of(&self, entity: u8) -> Option<f32> {
        self.headings.get(&entity).copied()
    }

    /// Whether the scene has hidden `entity` (`Some(false)`), shown it (`Some(true)`) or said nothing.
    pub fn visible_of(&self, entity: u8) -> Option<bool> {
        self.visible.get(&entity).copied()
    }

    /// Every place the scene knows, by entity.
    pub fn places(&self) -> &BTreeMap<u8, Where> {
        &self.places
    }

    /// Whether the scene has ended.
    pub fn is_over(&self) -> bool {
        self.done.is_some()
    }

    // ---- variables --------------------------------------------------------------------------------------------

    fn var_byte(&self, bank: u8, address: u8) -> u8 {
        self.writes.get(&(bank, address)).copied().unwrap_or_else(|| self.vars.byte(bank, address))
    }

    fn var_word(&self, bank: u8, address: u8) -> u16 {
        let low = self.var_byte(bank, address);
        let high = address.checked_add(1).map_or(0, |a| self.var_byte(bank, a));
        u16::from_le_bytes([low, high])
    }

    fn set_byte(&mut self, bank: u8, address: u8, value: u8) {
        if self.var_byte(bank, address) != value {
            self.epoch = self.epoch.wrapping_add(1);
        }
        self.writes.insert((bank, address), value);
    }

    fn set_word(&mut self, bank: u8, address: u8, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.set_byte(bank, address, low);
        if let Some(next) = address.checked_add(1) {
            self.set_byte(bank, next, high);
        }
    }

    /// The value of operand `name`: the literal, or the variable it names read at `width`; 0 when the instruction has
    /// no such operand.
    fn num(&self, ins: &Instruction, name: &str, width: Width) -> i32 {
        let Some(op) = ins.operand(name) else { return 0 };
        if op.bank == 0 {
            return op.value;
        }
        let address = u8::try_from(op.value & 0xFF).unwrap_or(0);
        match width {
            Width::U8 => i32::from(self.var_byte(op.bank, address)),
            Width::U16 => i32::from(self.var_word(op.bank, address)),
            Width::S16 => i32::from(i16::from_le_bytes(self.var_word(op.bank, address).to_le_bytes())),
        }
    }

    /// A byte condition can read a carried bit without depending on the unknown remainder of
    /// its saved byte. Unknown bits still use the normal audited read; local writes take priority.
    fn known_operand_bit(&self, ins: &Instruction, name: &str, bit: i32) -> Option<bool> {
        let bit = u8::try_from(bit).ok().filter(|bit| *bit < 8)?;
        let (bank, address) = Self::dest(ins, name)?;
        self.writes.get(&(bank, address)).map(|value| value & (1 << bit) != 0)
            .or_else(|| self.vars.story.known_bit(bank, address, bit))
    }

    /// `(bank, address)` of the variable operand `name` (`None` for a literal: nothing to write).
    fn dest(ins: &Instruction, name: &str) -> Option<(u8, u8)> {
        let op = ins.operand(name)?;
        (op.bank != 0).then(|| (op.bank, u8::try_from(op.value & 0xFF).unwrap_or(0)))
    }

    fn store(&mut self, (bank, address): (u8, u8), wide: bool, value: i64, saturate: bool) {
        if wide {
            let v = if saturate { value.clamp(0, 0xFFFF) } else { value.rem_euclid(0x1_0000) };
            self.set_word(bank, address, u16::try_from(v).unwrap_or(0));
        } else {
            let v = if saturate { value.clamp(0, 0xFF) } else { value.rem_euclid(0x100) };
            self.set_byte(bank, address, u8::try_from(v).unwrap_or(0));
        }
    }

    fn arithmetic(&mut self, ins: &Instruction, op: Arith, wide: bool, saturate: bool) {
        let Some(var) = Self::dest(ins, "var") else { return };
        let cur = if matches!(op, Arith::Set) {
            0
        } else if wide {
            i64::from(self.var_word(var.0, var.1))
        } else {
            i64::from(self.var_byte(var.0, var.1))
        };
        let value = i64::from(self.num(ins, "value", if wide { Width::S16 } else { Width::U8 }));
        let new = match op {
            Arith::Set => value,
            Arith::Plus => cur.saturating_add(value),
            Arith::Minus => cur.saturating_sub(value),
            Arith::Mul => cur.saturating_mul(value),
            Arith::Div => match cur.checked_div(value) {
                Some(q) => q,
                None => return,
            },
            Arith::Mod => match cur.checked_rem(value) {
                Some(r) => r,
                None => return,
            },
            Arith::And => cur & value,
            Arith::Or => cur | value,
            Arith::Xor => cur ^ value,
            Arith::Inc => cur.saturating_add(1),
            Arith::Dec => cur.saturating_sub(1),
        };
        self.store(var, wide, new, saturate);
    }

    /// `BITON`/`BITOFF`/`BITXOR`: the variable is a row of bits, bit `n` being bit `n % 8` of the byte `n / 8` after its
    /// address.
    fn bit_op(&mut self, ins: &Instruction, op: u8) {
        let Some((bank, address)) = Self::dest(ins, "var") else { return };
        let bit = self.num(ins, "bit", Width::U8).rem_euclid(256);
        self.flip_bit(bank, address, bit, op);
    }

    fn flip_bit(&mut self, bank: u8, address: u8, bit: i32, op: u8) {
        let byte_at = address.checked_add(u8::try_from(bit / 8).unwrap_or(0));
        let (Some(byte_at), Some(mask)) = (byte_at, 1u8.checked_shl(u32::try_from(bit % 8).unwrap_or(0))) else { return };
        let cur = self.var_byte(bank, byte_at);
        let new = match op {
            0x82 => cur | mask,
            0x83 => cur & !mask,
            _ => cur ^ mask,
        };
        self.set_byte(bank, byte_at, new);
    }

    fn random(&mut self) -> u8 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        u8::try_from(x >> 24).unwrap_or(0)
    }

    // ---- threads ----------------------------------------------------------------------------------------------

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn live_threads(&self) -> usize {
        self.threads.iter().filter(|t| t.state != State::Done).count()
    }

    fn thread_time(&self, idx: usize) -> u32 {
        self.threads.get(idx).map_or(0, |t| t.time)
    }

    fn add_time(&mut self, idx: usize, frames: u32) {
        if let Some(t) = self.threads.get_mut(idx) {
            t.time = t.time.saturating_add(frames);
        }
    }

    fn wait_until(&mut self, idx: usize, time: u32) {
        if let Some(t) = self.threads.get_mut(idx) {
            t.time = t.time.max(time);
        }
    }

    /// Start `entity`'s script `slot` as a thread at `time`, on priority slot `prio`. `None` when it cannot be started.
    fn spawn(&mut self, script: &FieldScript<'_>, entity: usize, slot: usize, prio: u8, time: u32) -> Option<usize> {
        // (the ended threads stay in the table, so a script that starts a short-lived one every lap is bounded too)
        if self.live_threads() >= self.limits.threads || self.threads.len() >= MAX_THREADS_EVER {
            self.stats.dropped_requests = self.stats.dropped_requests.saturating_add(1);
            return None;
        }
        let Some(span) = script.span(entity, slot) else {
            self.stats.dropped_requests = self.stats.dropped_requests.saturating_add(1);
            return None;
        };
        let idx = self.threads.len();
        let busy = self.slots.get(&(entity, prio)).copied().filter(|&i| self.threads.get(i).is_some_and(|t| t.state != State::Done));
        let state = if busy.is_some() {
            self.stats.queued = self.stats.queued.saturating_add(1);
            State::WaitSlot
        } else {
            self.slots.insert((entity, prio), idx);
            State::Run
        };
        let seq = self.next_seq();
        self.threads.push(Thread {
            entity,
            prio,
            code: Code::Script { pc: span.start },
            time,
            state,
            seq,
            seen: BTreeSet::new(),
            seen_epoch: self.epoch,
            ended_at: 0,
        });
        self.stats.threads = self.stats.threads.saturating_add(1);
        self.epoch = self.epoch.wrapping_add(1);
        Some(idx)
    }

    /// Thread `idx` has ended: free its slot (the next thread waiting for it starts), wake whoever waits for its end.
    fn finish_thread(&mut self, idx: usize) {
        let Some(t) = self.threads.get_mut(idx) else { return };
        if t.state == State::Done {
            return;
        }
        t.state = State::Done;
        t.ended_at = t.time;
        let (key, ended) = ((t.entity, t.prio), t.time);
        self.epoch = self.epoch.wrapping_add(1);
        if self.slots.get(&key) == Some(&idx) {
            self.slots.remove(&key);
            let next = self
                .threads
                .iter()
                .enumerate()
                .filter(|(_, t)| t.state == State::WaitSlot && (t.entity, t.prio) == key)
                .min_by_key(|(_, t)| t.seq)
                .map(|(i, _)| i);
            if let Some(i) = next {
                self.slots.insert(key, i);
                if let Some(t) = self.threads.get_mut(i) {
                    t.state = State::Run;
                    t.time = t.time.max(ended);
                }
            }
        }
        for t in &mut self.threads {
            if t.state == State::WaitEnd(idx) {
                t.state = State::Run;
                t.time = t.time.max(ended);
            }
        }
    }

    /// The runnable thread that is earliest (ties: the one that has waited longest in the queue).
    fn pick(&mut self) -> Option<usize> {
        let (epoch, clock) = (self.epoch, self.clock);
        for t in &mut self.threads {
            if let State::Parked(at) = t.state {
                if at != epoch {
                    // Something changed: the loop looks again, from the moment of the change.
                    t.state = State::Run;
                    t.seen.clear();
                    t.seen_epoch = epoch;
                    t.time = t.time.max(clock);
                }
            }
        }
        self.threads.iter().enumerate().filter(|(_, t)| t.state == State::Run).min_by_key(|(_, t)| (t.time, t.seq)).map(|(i, _)| i)
    }

    /// A narrow, verified animation tail: CANIM2, WAIT, ANIME1, WAIT, JMPB to the first
    /// instruction. Only finish when every other thread has ended or is parked at an
    /// unconditional self-jump. In particular, a waiter or runnable thread can never be
    /// skipped. This tail cannot change variables, request work or open a window.
    fn quiescent_animation_tail(&self, script: &FieldScript<'_>, idx: usize) -> bool {
        let Some(t) = self.threads.get(idx) else { return false };
        let Code::Script { pc: start } = t.code else { return false };
        let Some(code) = script.bytes().get(..script.layout().code_end) else { return false };
        let mut pc = start;
        for expected in [0xBB, 0x24, 0xA3, 0x24, 0x12] {
            let Ok(ins) = decode_at(code, pc, 0) else { return false };
            if ins.opcode != expected || (expected == 0x12 && ins.jump_target != Some(start)) { return false; }
            pc = ins.end();
        }
        self.threads.iter().enumerate().all(|(i, t)| {
            if i == idx || t.state == State::Done { return true; }
            let State::Parked(_) = t.state else { return false };
            let Code::Script { pc } = t.code else { return false };
            decode_at(code, pc, 0).is_ok_and(|ins| ins.opcode == 0x12 && ins.jump_target == Some(pc))
        })
    }

    // ---- stage ------------------------------------------------------------------------------------------------

    fn emit(&mut self, time: u32, what: Stage) {
        self.epoch = self.epoch.wrapping_add(1);
        self.out.push(Timed { at: time.saturating_sub(self.origin), what });
    }

    fn move_frames(&self, entity: u8, distance: f32) -> u32 {
        let speed = self.speeds.get(&entity).copied().unwrap_or(DEFAULT_MOVE_SPEED).max(128) as f32;
        let per_frame = (speed / MOVE_SPEED_PER_UNIT).max(0.25);
        let frames = (distance / per_frame).ceil();
        if frames.is_finite() {
            (frames as u32).clamp(1, MAX_MOVE_FRAMES)
        } else {
            1
        }
    }

    fn do_place(&mut self, idx: usize, entity: u8, ins: &Instruction) {
        let time = self.thread_time(idx);
        let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
        let z = ins.operand("z").map(|_| clamp_i16(self.num(ins, "z", Width::S16)));
        let triangle = ins.operand("triangle").and_then(|_| u16::try_from(self.num(ins, "triangle", Width::U16)).ok());
        self.places.insert(entity, Where { x, y, z, triangle });
        self.emit(time, Stage::Place { entity, x, y, z, triangle });
    }

    fn do_move(&mut self, idx: usize, entity: u8, goal: Goal) {
        let Goal { x, y, z, triangle, how, frames } = goal;
        let time = self.thread_time(idx);
        let from = self.places.get(&entity).map(|w| (w.x, w.y));
        let frames = frames.unwrap_or_else(|| {
            let distance = from.map_or(0.0, |(fx, fy)| (f32::from(x) - f32::from(fx)).hypot(f32::from(y) - f32::from(fy)));
            self.move_frames(entity, distance)
        });
        self.places.insert(entity, Where { x, y, z, triangle });
        self.emit(time, Stage::Move { entity, from, x, y, z, triangle, frames, how });
        // The script waits until the walk is over.
        self.add_time(idx, frames);
    }

    fn do_face(&mut self, idx: usize, entity: u8, heading: f32, frames: u32) {
        let time = self.thread_time(idx);
        self.headings.insert(entity, heading);
        self.emit(time, Stage::Face { entity, heading, frames });
        if frames > 0 {
            self.add_time(idx, frames);
        }
    }

    /// Turn `entity` towards `target` (`DIRA`, `TURA`): where the scene knows them both.
    fn face_entity(&mut self, idx: usize, entity: u8, target: u8, frames: u32) {
        let (Some(a), Some(b)) = (self.places.get(&entity).copied(), self.places.get(&target).copied()) else {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return;
        };
        let (dx, dy) = (f32::from(b.x) - f32::from(a.x), f32::from(b.y) - f32::from(a.y));
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.do_face(idx, entity, dy.atan2(dx), frames);
    }

    fn anim_frames(&self, entity: u8, first: Option<u8>, last: Option<u8>) -> u32 {
        let speed = self.anim_speeds.get(&entity).copied().unwrap_or(DEFAULT_ANIM_SPEED).max(1);
        let clip = match (first, last) {
            (Some(f), Some(l)) => u32::from(l.max(f).saturating_sub(l.min(f))).saturating_add(1),
            _ => DEFAULT_CLIP_FRAMES,
        };
        (clip.saturating_mul(DEFAULT_ANIM_SPEED)).div_ceil(speed).max(1)
    }

    fn do_anim(&mut self, idx: usize, entity: u8, ins: &Instruction, how: AnimHow, wait: bool) {
        let time = self.thread_time(idx);
        let animation = clamp_u8(self.num(ins, "animation", Width::U8));
        let (first, last) = (ins.literal("first").map(clamp_u8), ins.literal("last").map(clamp_u8));
        let speed = self.anim_speeds.get(&entity).copied().unwrap_or(DEFAULT_ANIM_SPEED);
        self.emit(time, Stage::Anim { entity, animation, how, first, last, speed: u8::try_from(speed.min(255)).unwrap_or(255) });
        if how != AnimHow::Loop {
            let end = time.saturating_add(self.anim_frames(entity, first, last));
            self.anim_ends.insert(entity, end);
            if wait {
                self.wait_until(idx, end);
            }
        }
    }

    // ---- one instruction --------------------------------------------------------------------------------------

    fn message(&self, script: &FieldScript<'_>, dialog: i32) -> Option<Message> {
        let index = usize::try_from(dialog).ok()?;
        let d = script.dialogs().get(index)?;
        let raw = script.bytes().get(d.offset..d.offset.checked_add(d.len)?)?;
        // The speaker's own entity is not known here: the entity is the opener's, set by the caller.
        Some(Message::new(MessageOrigin::Canon { entity: 0, dialog: index }, parse_message(raw, &self.vars)))
    }

    fn open(&mut self, idx: usize, entity: usize, window: i32, mut message: Message, pending: Option<Pending>) -> Flow {
        if let MessageOrigin::Canon { entity: e, .. } = &mut message.origin {
            *e = entity;
        }
        message.window = u8::try_from(window).unwrap_or(0);
        if self.windows >= self.limits.windows {
            return Flow::Scene(End::Budget);
        }
        self.windows = self.windows.saturating_add(1);
        self.stats.windows = self.windows;
        self.epoch = self.epoch.wrapping_add(1);
        Flow::Window(Opened { opener: entity, id: u8::try_from(window).unwrap_or(0), time: self.thread_time(idx), message, pending })
    }

    fn ask(&mut self, script: &FieldScript<'_>, idx: usize, entity: usize, ins: &Instruction) -> Flow {
        let (Some(window), Some(dialog), Some(first), Some(last)) =
            (ins.literal("window"), ins.literal("dialog"), ins.literal("first"), ins.literal("last"))
        else {
            return Flow::Next;
        };
        let Some(mut message) = self.message(script, dialog) else { return Flow::Next };
        let var = Self::dest(ins, "var");
        let first = usize::try_from(first).unwrap_or(0);
        // A last line the text does not have is the text's last line.
        let lines = last_page_lines(&message.pieces);
        let last = usize::try_from(last).unwrap_or(0).max(first).min(lines.saturating_sub(1));
        let held = var.map(|(bank, address)| usize::from(self.var_byte(bank, address)));
        let cursor = held.filter(|c| (first..=last).contains(c)).unwrap_or(first);
        if let Some((bank, address)) = var {
            // The variable always ends up holding an answer, even when nobody was asked.
            if self.quiet || first > last {
                self.set_byte(bank, address, u8::try_from(cursor).unwrap_or(u8::MAX));
            }
        }
        if self.quiet {
            return Flow::Next;
        }
        if first > last {
            return self.open(idx, entity, window, message, None);
        }
        message.choice = Some(Choice { first, last, cursor });
        self.open(idx, entity, window, message, Some(Pending { var, first, last, cursor }))
    }

    /// Continue after a conditional: into its body when `holds`, else past it.
    fn branch(&mut self, idx: usize, ins: &Instruction, holds: bool) -> Flow {
        if holds {
            Flow::Next
        } else {
            self.jump(idx, ins)
        }
    }

    fn set_pc(&mut self, idx: usize, pc: usize) {
        if let Some(Thread { code: Code::Script { pc: at }, .. }) = self.threads.get_mut(idx) {
            *at = pc;
        }
    }

    fn jump(&mut self, idx: usize, ins: &Instruction) -> Flow {
        match ins.jump_target {
            Some(target) => {
                self.set_pc(idx, target);
                Flow::Next
            }
            None => Flow::Scene(End::Broken),
        }
    }

    /// Run one instruction of script thread `idx`.
    fn step_script(&mut self, script: &FieldScript<'_>, idx: usize) -> Flow {
        let Some(thread) = self.threads.get_mut(idx) else { return Flow::Yield };
        let Code::Script { pc } = thread.code else { return Flow::Yield };
        let entity = thread.entity;
        // An idle loop: the same instruction again with nothing changed anywhere since this thread met it.
        if thread.seen_epoch != self.epoch {
            thread.seen.clear();
            thread.seen_epoch = self.epoch;
        }
        if !thread.seen.insert(pc) {
            thread.state = State::Parked(self.epoch);
            self.stats.parked = self.stats.parked.saturating_add(1);
            return Flow::Yield;
        }
        let code_end = script.layout().code_end;
        if pc >= code_end {
            // Ran off the end of the code: as good as a RET.
            self.finish_thread(idx);
            return Flow::Yield;
        }
        let Some(code) = script.bytes().get(..code_end) else { return Flow::Scene(End::Broken) };
        let Ok(ins) = decode_at(code, pc, 0) else { return Flow::Scene(End::Broken) };
        self.set_pc(idx, ins.end());
        let mut class = Class::Handled;
        let flow = self.exec(script, idx, entity, &ins, &mut class);
        self.stats.note(class, ins.mnemonic);
        flow
    }

    /// The entity a script's own instruction addresses, as the cast knows it.
    fn own(entity: usize) -> u8 {
        u8::try_from(entity).unwrap_or(u8::MAX)
    }

    /// `REQ`, `REQSW`, `REQEW`: start a script of an entity as a thread.
    fn request(&mut self, script: &FieldScript<'_>, idx: usize, ins: &Instruction) -> Flow {
        let time = self.thread_time(idx);
        let target = ins.literal("entity").zip(ins.literal("script")).and_then(|(e, s)| Some((usize::try_from(e).ok()?, usize::try_from(s).ok()?)));
        let prio = u8::try_from(ins.literal("priority").unwrap_or(7)).unwrap_or(7);
        let Some((entity, slot)) = target else {
            self.stats.dropped_requests = self.stats.dropped_requests.saturating_add(1);
            return Flow::Next;
        };
        let Some(child) = self.spawn(script, entity, slot, prio, time) else { return Flow::Next };
        match ins.opcode {
            // REQEW: the requester waits for the end of the script it asked for.
            0x03 => {
                let ended = self.threads.get(child).filter(|t| t.state == State::Done).map(|t| t.ended_at);
                if let Some(ended) = ended {
                    self.wait_until(idx, ended);
                } else if let Some(t) = self.threads.get_mut(idx) {
                    t.state = State::WaitEnd(child);
                }
                Flow::Yield
            }
            // REQSW: the new thread runs first (the requester waits for it to start).
            0x02 => {
                let seq = self.next_seq();
                if let Some(t) = self.threads.get_mut(idx) {
                    t.seq = seq;
                }
                Flow::Yield
            }
            _ => Flow::Next,
        }
    }

    fn exec(&mut self, script: &FieldScript<'_>, idx: usize, entity: usize, ins: &Instruction, class: &mut Class) -> Flow {
        let me = Self::own(entity);
        let op = ins.opcode;
        match op {
            0x00 => {
                self.finish_thread(idx);
                Flow::Yield
            }
            0x01..=0x03 => self.request(script, idx, ins),
            // RETTO: carry on in another script of the same entity, in place of this one.
            0x07 => {
                let slot = ins.literal("script").and_then(|s| usize::try_from(s).ok());
                match slot.and_then(|s| script.span(entity, s)) {
                    Some(span) => {
                        self.set_pc(idx, span.start);
                        Flow::Next
                    }
                    None => {
                        self.finish_thread(idx);
                        Flow::Yield
                    }
                }
            }
            0x10 | 0x11 => self.jump(idx, ins),
            0x12 | 0x13 => {
                // A backward jump is the end of the script's turn: a frame passes.
                self.add_time(idx, 1);
                self.jump(idx, ins)
            }
            0x14..=0x19 => {
                let (width, bits) = match op {
                    0x14 | 0x15 => (Width::U8, 8),
                    0x16 | 0x17 => (Width::S16, 16),
                    _ => (Width::U16, 16),
                };
                let operation = ins.literal("op").unwrap_or(-1);
                // Read a banked bit index normally so unknown inputs remain audited.
                let right = self.num(ins, "right", width);
                let known = if matches!(op, 0x14 | 0x15) && matches!(operation, 9 | 10) {
                    self.known_operand_bit(ins, "left", right)
                } else { None };
                let holds = known.map_or_else(
                    || compare(operation, self.num(ins, "left", width), right, bits),
                    |set| if operation == 9 { set } else { !set },
                );
                self.branch(idx, ins, holds)
            }
            // The keys: nobody presses any during a scene.
            0x30..=0x32 => self.branch(idx, ins, false),
            0x20 => Flow::Scene(End::MiniGame),
            0x24 => {
                self.add_time(idx, u32::from(u16::try_from(self.num(ins, "frames", Width::U16)).unwrap_or(0)));
                Flow::Yield
            }
            0x40 => {
                let (Some(window), Some(dialog)) = (ins.literal("window"), ins.literal("dialog")) else { return Flow::Next };
                let Some(message) = self.message(script, dialog) else { return Flow::Next };
                if self.quiet {
                    return Flow::Next;
                }
                self.open(idx, entity, window, message, None)
            }
            0x48 => self.ask(script, idx, entity, ins),
            0x50 => {
                if let Some(window) = ins.literal("window") {
                    let g = (
                        clamp_i16(self.num(ins, "x", Width::S16)),
                        clamp_i16(self.num(ins, "y", Width::S16)),
                        u16::try_from(self.num(ins, "width", Width::U16)).unwrap_or(0),
                        u16::try_from(self.num(ins, "height", Width::U16)).unwrap_or(0),
                    );
                    self.geometry.insert(clamp_u8(window), g);
                }
                Flow::Next
            }
            // Gifts are not given here (the server owns the inventory): the announcement that follows gets a note.
            0x58 | 0x5B | 0x39 => {
                *class = Class::Ignored;
                self.gift_not_given = true;
                Flow::Next
            }
            0x60 => {
                let jump = (|| Some(SceneJump {
                    field: u16::try_from(ins.literal("field")?).ok()?,
                    x: i16::try_from(ins.literal("x")?).ok()?,
                    y: i16::try_from(ins.literal("y")?).ok()?,
                    triangle: i16::try_from(ins.literal("triangle")?).ok()?,
                    direction: u8::try_from(ins.literal("direction")?).ok()?,
                }))();
                self.jump = jump;
                Flow::Scene(End::MapJump { field: jump.map_or(0, |j| j.field) })
            },
            0x70 => {
                self.battle = ins.literal("battle").and_then(|n| u16::try_from(n).ok());
                Flow::Scene(End::Battle)
            },
            0xFF => Flow::Scene(End::GameOver),
            0x74 => {
                // GETPC: the character in a party slot (0xFF for none).
                let slot = ins.literal("party").and_then(|s| u8::try_from(s).ok()).unwrap_or(u8::MAX);
                if let Some((bank, address)) = Self::dest(ins, "var_character") {
                    let who = self.party_now().get(usize::from(slot)).copied().unwrap_or(u8::MAX);
                    self.set_byte(bank, address, who);
                }
                Flow::Next
            }
            0x82..=0x84 => {
                self.bit_op(ins, op);
                Flow::Next
            }
            0x99 => {
                if let Some((bank, address)) = Self::dest(ins, "var") {
                    let value = self.random();
                    self.set_byte(bank, address, value);
                }
                Flow::Next
            }
            // LBYTE and HBYTE set the low and the high byte of a word variable.
            0x9A | 0x9B => {
                if let Some((bank, address)) = Self::dest(ins, "var") {
                    let value = u8::try_from(self.num(ins, "value", Width::U16) & 0xFF).unwrap_or(0);
                    let at = if op == 0x9A { Some(address) } else { address.checked_add(1) };
                    if let Some(at) = at {
                        self.set_byte(bank, at, value);
                    }
                }
                Flow::Next
            }
            0x9C => {
                if let Some((bank, address)) = Self::dest(ins, "var") {
                    let low = u8::try_from(self.num(ins, "low", Width::U8) & 0xFF).unwrap_or(0);
                    let high = u8::try_from(self.num(ins, "high", Width::U8) & 0xFF).unwrap_or(0);
                    self.set_word(bank, address, u16::from_le_bytes([low, high]));
                }
                Flow::Next
            }
            0xCB | 0xCC => {
                // IFPRTYQ asks who is in the party NOW: what the story says, and what this scene's scripts have done to it
                // since. IFMEMBQ asks whether the character belongs to the team at all, which a party change does not decide.
                let holds = ins.literal("character").and_then(|c| u8::try_from(c).ok()).is_some_and(|c| {
                    if op == 0xCB { self.party_now().contains(&c) } else { self.vars.is_member(c) }
                });
                self.branch(idx, ins, holds)
            }
            // GETAI reads an entity's current walkmesh triangle. A MOVE without an explicit
            // triangle does not establish one; never invent it from a stale position.
            0xB9 => {
                if let Some((bank, address)) = Self::dest(ins, "var_triangle") {
                    let triangle = ins.literal("entity").and_then(|e| u8::try_from(e).ok())
                        .and_then(|e| self.places.get(&e)).and_then(|place| place.triangle);
                    if let Some(triangle) = triangle {
                        self.set_word(bank, address, triangle);
                    } else {
                        self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    }
                }
                Flow::Next
            }
            // ---- the stage -----------------------------------------------------------------------------------
            0xA5..=0xA7 => {
                self.do_place(idx, me, ins);
                Flow::Next
            }
            0xA8 | 0xA9 | 0xAD => {
                let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
                self.do_move(idx, me, Goal { x, y, z: None, triangle: None, how: MoveHow::Walk, frames: None });
                Flow::Yield
            }
            0xAA => {
                // MOVA: walk to where another entity stands.
                let target = ins.literal("entity").map(clamp_u8).and_then(|e| self.places.get(&e).copied());
                match target {
                    Some(w) => {
                        self.do_move(idx, me, Goal { x: w.x, y: w.y, z: None, triangle: None, how: MoveHow::Walk, frames: None });
                        Flow::Yield
                    }
                    None => {
                        self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                        Flow::Next
                    }
                }
            }
            0xC0 => {
                let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
                let triangle = u16::try_from(self.num(ins, "triangle", Width::U16)).ok();
                self.do_move(idx, me, Goal { x, y, z: None, triangle, how: MoveHow::Jump, frames: Some(JUMP_FRAMES) });
                Flow::Yield
            }
            0xC2 => {
                let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
                let z = clamp_i16(self.num(ins, "z", Width::S16));
                let triangle = u16::try_from(self.num(ins, "triangle", Width::U16)).ok();
                let from_z = self.places.get(&me).and_then(|w| w.z).unwrap_or(z);
                let frames = (u32::from(from_z.abs_diff(z)) / 4).clamp(JUMP_FRAMES / 2, MAX_MOVE_FRAMES);
                self.do_move(idx, me, Goal { x, y, z: Some(z), triangle, how: MoveHow::Ladder, frames: Some(frames) });
                Flow::Yield
            }
            0xB2 => {
                let speed = self.num(ins, "speed", Width::U16);
                self.speeds.insert(me, speed);
                Flow::Next
            }
            0xB3 => {
                let direction = clamp_u8(self.num(ins, "direction", Width::U8));
                self.do_face(idx, me, field_direction_to_facing(direction), 0);
                Flow::Next
            }
            0xB4 | 0xB5 => {
                let direction = clamp_u8(self.num(ins, "direction", Width::U8));
                self.do_face(idx, me, field_direction_to_facing(direction), TURN_FRAMES);
                Flow::Yield
            }
            0xB6 => {
                if let Some(target) = ins.literal("entity").map(clamp_u8) {
                    self.face_entity(idx, me, target, 0);
                }
                Flow::Next
            }
            0xAB => {
                if let Some(target) = ins.literal("entity").map(clamp_u8) {
                    self.face_entity(idx, me, target, TURN_FRAMES);
                }
                Flow::Yield
            }
            0xA4 => {
                if let Some(v) = ins.literal("visible") {
                    let time = self.thread_time(idx);
                    self.visible.insert(me, v != 0);
                    self.emit(time, Stage::Show { entity: me, visible: v != 0 });
                }
                Flow::Next
            }
            0xA2 => {
                self.do_anim(idx, me, ins, AnimHow::Loop, false);
                Flow::Next
            }
            0xA3 | 0xB0 => {
                self.do_anim(idx, me, ins, AnimHow::Once, true);
                Flow::Yield
            }
            0xAE | 0xBB => {
                self.do_anim(idx, me, ins, AnimHow::Once, false);
                Flow::Next
            }
            0xAF | 0xB1 => {
                self.do_anim(idx, me, ins, AnimHow::Hold, true);
                Flow::Yield
            }
            0xBA | 0xBC => {
                self.do_anim(idx, me, ins, AnimHow::Hold, false);
                Flow::Next
            }
            0xBD => {
                let speed = u32::try_from(self.num(ins, "speed", Width::U16)).unwrap_or(DEFAULT_ANIM_SPEED).max(1);
                self.anim_speeds.insert(me, speed);
                Flow::Next
            }
            0xAC => {
                if let Some(end) = self.anim_ends.get(&me).copied() {
                    self.wait_until(idx, end);
                }
                Flow::Yield
            }
            // ---- the camera ----------------------------------------------------------------------------------
            0x64 | 0x66 | 0x68 => {
                let time = self.thread_time(idx);
                let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
                let frames = if op == 0x64 { 0 } else { u32::from(u16::try_from(self.num(ins, "speed", Width::U16)).unwrap_or(0)) };
                self.camera_end = time.saturating_add(frames);
                self.emit(time, Stage::Camera(CameraOp::Scroll { x, y, frames }));
                Flow::Next
            }
            0x63 => {
                let time = self.thread_time(idx);
                let entity = clamp_u8(self.num(ins, "entity", Width::U8));
                self.emit(time, Stage::Camera(CameraOp::Follow { entity }));
                Flow::Next
            }
            0x65 => {
                let time = self.thread_time(idx);
                self.emit(time, Stage::Camera(CameraOp::Reset));
                Flow::Next
            }
            0x67 => {
                self.wait_until(idx, self.camera_end);
                Flow::Yield
            }
            0xF1 => {
                match crate::field_sound::Call::of(ins) {
                    Ok(Some(call)) => self.emit(self.thread_time(idx), Stage::Sound { id: call.id, pan: call.pan }),
                    _ => {
                        // This presentation slice supports literal SOUND operands only. Do not turn an
                        // unreviewed variable read into a guessed sound or a stop of the current channel.
                        *class = Class::Ignored;
                    }
                }
                Flow::Next
            }
            // ---- the party: nothing changes here, what was asked is kept for the caller -----------------------
            0xC8 | 0xC9 => {
                // (an operand that is not a literal is not made up: the op stays unknown)
                match ins.literal("character") {
                    Some(character) => self.party_ops.push(if op == 0xC8 { PartyOp::Add(clamp_u8(character)) } else { PartyOp::Remove(clamp_u8(character)) }),
                    None => *class = Class::Unknown,
                }
                Flow::Next
            }
            0xCA => {
                match (ins.literal("character1"), ins.literal("character2"), ins.literal("character3")) {
                    (Some(a), Some(b), Some(c)) => self.party_ops.push(PartyOp::Set([clamp_u8(a), clamp_u8(b), clamp_u8(c)])),
                    _ => *class = Class::Unknown,
                }
                Flow::Next
            }
            // ---- known, and no change to what a scene shows ---------------------------------------------------
            _ => {
                if let Some((arith, wide, saturate)) = arith_of(op) {
                    self.arithmetic(ins, arith, wide, saturate);
                    return Flow::Next;
                }
                *class = if is_ignored(op) { Class::Ignored } else { Class::Unknown };
                Flow::Next
            }
        }
    }

    // ---- the scheduler ----------------------------------------------------------------------------------------

    /// Run the steps of the scene's own start thread `idx`.
    fn step_root(&mut self, script: &FieldScript<'_>, idx: usize) -> Flow {
        loop {
            let Some(Thread { code: Code::Root { steps, next }, time, .. }) = self.threads.get(idx) else { return Flow::Yield };
            let (time, step) = (*time, steps.get(*next).copied());
            let Some(step) = step else {
                self.finish_thread(idx);
                return Flow::Yield;
            };
            if let Some(Thread { code: Code::Root { next, .. }, .. }) = self.threads.get_mut(idx) {
                *next = next.saturating_add(1);
            }
            match step {
                Step::SetBit { bank, address, bit } => self.flip_bit(bank, address, i32::from(bit), 0x82),
                Step::StartMain(entity) => {
                    let main = script.entity_inits(None).into_iter().find(|init| init.entity == usize::from(entity)).and_then(|init| init.main_start);
                    if let Some(main) = main {
                        if let Some(child) = self.spawn(script, usize::from(entity), 0, 7, time) {
                            if let Some(thread) = self.threads.get_mut(child) { thread.code = Code::Script { pc: main }; }
                        }
                    } else {
                        self.stats.dropped_requests = self.stats.dropped_requests.saturating_add(1);
                    }
                }
                Step::Await(ScriptRef { entity, slot }) | Step::Start(ScriptRef { entity, slot }) => {
                    let wait = matches!(step, Step::Await(_));
                    let Some(child) = self.spawn(script, usize::from(entity), usize::from(slot), 7, time) else { continue };
                    if wait {
                        if let Some(t) = self.threads.get_mut(idx) {
                            t.state = State::WaitEnd(child);
                        }
                        return Flow::Yield;
                    }
                }
            }
        }
    }

    /// Start the scene: the root thread, which takes the steps the field's own trigger takes.
    pub fn begin(&mut self, root: Vec<Step>) {
        self.jump = None;
        self.battle = None;
        self.quiet = false;
        self.threads.clear();
        self.slots.clear();
        let seq = self.next_seq();
        self.threads.push(Thread {
            entity: usize::MAX,
            prio: 7,
            code: Code::Root { steps: root, next: 0 },
            time: 0,
            state: State::Run,
            seq,
            seen: BTreeSet::new(),
            seen_epoch: self.epoch,
            ended_at: 0,
        });
        self.origin = 0;
        self.clock = 0;
        self.out.clear();
        self.done = None;
    }

    /// Reconstruct a presentation-only continuation. Never guess an answer, skip another kind of termination,
    /// or pass a different formation. Existing budgets cover both the prelude and the tail.
    pub(super) fn seek_after_battle(&mut self, script: &FieldScript<'_>, formation: u16) -> bool {
        loop {
            match self.resume(script, None).then {
                Then::Window(w) if w.message.choice.is_none() => {},
                Then::End { how: End::Battle, .. } if self.battle == Some(formation) => {
                    self.done = None;
                    self.battle = None;
                    self.origin = self.clock;
                    self.out.clear();
                    return true;
                },
                _ => return false,
            }
        }
    }

    /// Run the stage scripts quietly (no window opens, nothing is handed out): when it returns, [`Vm::places`] and the
    /// other tables hold where everyone stands, and the variables hold what those scripts wrote. The threads are
    /// dropped, loops included.
    pub fn setup(&mut self, script: &FieldScript<'_>, refs: &[ScriptRef]) {
        self.quiet = true;
        self.threads.clear();
        self.slots.clear();
        for r in refs {
            let _ = self.spawn(script, usize::from(r.entity), usize::from(r.slot), 7, 0);
        }
        while let Some(idx) = self.pick() {
            if self.steps >= self.limits.steps.min(20_000) || self.thread_time(idx) > self.limits.frames {
                self.stats.setup_incomplete = true;
                break;
            }
            self.steps = self.steps.saturating_add(1);
            match self.step_script(script, idx) {
                Flow::Next | Flow::Yield => {}
                Flow::Window(_) | Flow::Scene(_) => {
                    self.stats.setup_incomplete = true;
                    break;
                }
            }
        }
        self.threads.clear();
        self.slots.clear();
        self.out.clear();
        self.quiet = false;
    }

    fn finish(&mut self, end: End) -> Beat {
        self.done = Some(end);
        let at = self.clock.saturating_sub(self.origin);
        Beat { stage: std::mem::take(&mut self.out), then: Then::End { at, how: end } }
    }

    /// Run on to the next window or the end of the scene. `answer` is the player's answer to the window handed out
    /// last, if that asked (the absolute line number of the choice, as a script stores it; anything else is clamped
    /// into the choices, `None` takes the cursor's starting line).
    pub fn resume(&mut self, script: &FieldScript<'_>, answer: Option<usize>) -> Beat {
        if let Some((opener, note)) = self.queued_note.take() {
            let at = self.clock.saturating_sub(self.origin);
            let window = Window { at, opener: Self::own(opener), speaker: None, id: 0, geometry: None, message: note };
            self.origin = self.clock;
            return Beat { stage: std::mem::take(&mut self.out), then: Then::Window(window) };
        }
        if let Some(end) = self.done {
            return Beat { stage: std::mem::take(&mut self.out), then: Then::End { at: self.clock.saturating_sub(self.origin), how: end } };
        }
        if let Some(p) = self.pending.take() {
            let chosen = answer.map_or(p.cursor, |a| a.clamp(p.first, p.last.max(p.first)));
            if let Some((bank, address)) = p.var {
                self.set_byte(bank, address, u8::try_from(chosen).unwrap_or(u8::MAX));
            }
        }
        loop {
            let Some(idx) = self.pick() else { return self.finish(End::Finished) };
            if self.quiescent_animation_tail(script, idx) {
                self.stats.quiescent_tails = self.stats.quiescent_tails.saturating_add(1);
                return self.finish(End::Finished);
            }
            if self.steps >= self.limits.steps {
                return self.finish(End::Budget);
            }
            self.steps = self.steps.saturating_add(1);
            self.stats.steps = self.steps;
            let time = self.thread_time(idx);
            if time > self.limits.frames {
                return self.finish(End::Budget);
            }
            self.clock = self.clock.max(time);
            let is_root = self.threads.get(idx).is_some_and(|t| matches!(t.code, Code::Root { .. }));
            let flow = if is_root { self.step_root(script, idx) } else { self.step_script(script, idx) };
            match flow {
                Flow::Next | Flow::Yield => {}
                Flow::Scene(end) => return self.finish(end),
                Flow::Window(opened) => {
                    let at = opened.time.saturating_sub(self.origin);
                    let geometry = self.geometry.get(&opened.id).copied();
                    let window = Window { at, opener: Self::own(opened.opener), speaker: None, id: opened.id, geometry, message: opened.message };
                    self.pending = opened.pending;
                    if std::mem::take(&mut self.gift_not_given) && !self.quiet {
                        self.queued_note = Some((opened.opener, gift_note()));
                    }
                    self.origin = opened.time;
                    self.clock = self.clock.max(opened.time);
                    return Beat { stage: std::mem::take(&mut self.out), then: Then::Window(window) };
                }
            }
        }
    }
}

/// Opcodes a scene can skip without changing a line, a place, a turn, an animation, a visibility or the camera: the
/// other sound commands and music, the picture's own effects (palettes, fades, background layers, shakes), and the field's logic
/// that is about the player (controls, menus, party, talk and solid radii, triggers).
fn is_ignored(op: u8) -> bool {
    matches!(
        op,
        // controls, menus, party and field logic
        0x04..=0x06 | 0x08..=0x0B | 0x21..=0x23 | 0x26 | 0x27 | 0x2A | 0x2B | 0x33 | 0x38 | 0x3B | 0x49 | 0x4A | 0x4B | 0x6D | 0x6E | 0x71..=0x73
        | 0x7E | 0xBF | 0xC3 | 0xC4 | 0xC5..=0xC7 | 0xCD..=0xCF | 0xD0..=0xD3 | 0xD6 | 0xD7 | 0xD8 | 0xD9 | 0xDB | 0xDD | 0xDE | 0xDC
        // windows other than the one that is shown
        | 0x2E | 0x2F | 0x36 | 0x37 | 0x41..=0x43 | 0x51..=0x55 | 0x56 | 0x57 | 0x45 | 0x47 | 0x4D | 0x4F
        // the picture: fades, shakes, background layers, palettes, scrolls of a layer
        | 0x25 | 0x2C | 0x2D | 0x5E | 0x69 | 0x6A | 0x6B | 0x6C | 0x61 | 0x62 | 0x6F | 0xDF | 0xE0..=0xEF
        // sound commands other than SOUND, and music
        | 0xDA | 0xF0 | 0xF2..=0xF7 | 0xF8..=0xFE
        // items and materia: not given here
        | 0x59 | 0x5A | 0x5C | 0x5D | 0x3A | 0x3C..=0x3F
        // SPECIAL and KAWAI families
        | 0x0F | 0x28 | 0x29
        // an entity's declarations: its model (CHAR) and the playable character it is bound to (PC). A scene's cast names
        // the models it draws, and nothing the VM does reads the binding (`GETAI` and the rest name their entity).
        | 0xA0 | 0xA1
    )
}

