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
//! * It never guesses. Unsupported opcodes and effects such as palette changes and music are
//!   skipped and COUNTED by mnemonic ([`Stats::unknown`], [`Stats::ignored`]); an operand it
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
//! once ([`End`]); the world is not changed by any of them. A strict [`super::ScenePlay`] may
//! resume a BATTLE only after an exact checked checkpoint and authoritative victorious afterimage.
//! The suspended scheduler, scratch variables and RNG are retained across that boundary.
//!
//! # Requests and priorities
//!
//! `REQ` starts the requested script as a thread and goes on; `REQSW` does the same and lets the new thread run
//! first (the requester waits for it to *start*); `REQEW` blocks the requester until the thread has ended. A thread
//! runs on its entity's priority slot ([`ScriptRef`](super::ScriptRef)'s entity and the request's priority): a
//! request for a slot that is taken waits for it to be free (the engine's own rule for a taken slot is not
//! documented; waiting loses no line).

use super::play::{BattleCheckpoint, WindowCheckpoint, SceneBoundary, SceneError};
use super::data::{Given, ScriptRef, Step};
use super::fade::{FadeOp, FadeState};
use crate::field_direction_to_facing;
use crate::background::{BackgroundOp, BackgroundState, PaletteOp};
use crate::script::dialogue::{parse_message, Choice, End, Message, MessageOrigin, Piece, StoryVars, EFFECT_NOTE_LAYER};
use crate::script::{decode_at, field_wide_bytes, FieldScript, FieldScratch, FieldScratchError, FieldScratchOutcome, FieldScratchRun, Instruction};
use crate::story::{tracked_var, NativePartyOp, NativeStoryBuilder, NativeStoryJournal, PartyOp, StoryState, MOMENT_ADDRESS, MOMENT_BANK};
use std::collections::{BTreeMap, BTreeSet};

/// Field frames per second (the clock of [`Timed::at`]).
pub const FRAMES_PER_SECOND: u32 = 30;

/// Authoritative resources at the beginning of a scene. `None` is unknown, never an assumed
/// balance of zero. A preview may supply an explicitly synthetic balance, but cannot save it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneResources {
    pub gil: Option<u32>,
}

pub type Resources = SceneResources;

/// Economic instructions in execution order, including reads used by script branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GilOp {
    Add(u32),
    Subtract(u32),
    Read,
}

/// Resource observations and after-image. Nothing here changes shared state: the host must
/// compare the authoritative before-image and durably commit this together with the story.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceOutcome {
    pub gil_before: Option<u32>,
    pub gil_after: Option<u32>,
    pub gil_ops: Vec<GilOp>,
    /// An instruction needed gil or an amount without an authoritative input. Such a run cannot be committed.
    pub unknown_gil: bool,
}

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
/// Shortest estimated ladder traversal. JUMP instead uses its authored step count.
const MIN_LADDER_FRAMES: u32 = 15;
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
    /// `MOVE`, `CMOVE`, `MOVA`: walks (or runs: the frames say how long).
    Walk,
    /// `FMOVE`: walks while retaining the current animation instead of selecting a movement clip.
    WalkKeepAnimation,
    /// `JUMP`: `Stage::Move::frames` is the authored movement-step count plus two completion phases.
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
    /// Replace the model's world-space render offset (`OFST` type 0), independently of its
    /// logical position, floor, movement and facing. These are field units, not model units.
    Offset { entity: u8, x: i16, y: i16, z: i16 },
    /// OFST type 1: integer linear model-offset steps, independently of logical movement.
    OffsetMove { entity: u8, motion: super::OffsetMotion },
    /// Walk to a place over `frames`, from where the entity stood (`from`, when the scene knows it). `z` is the
    /// script's height (a ladder's), else the ground's at the place ([`super::ScenePlay`] fills it in).
    /// For JUMP, `frames` is N+2: N authored motion steps, the completion update and the script's
    /// next pass. Its `z` uses the authored triangle's infinite plane, including destinations
    /// outside that triangle; [`super::JumpMotion`] supplies the original Q12 trajectory.
    /// `triangle` preserves the authored landing floor of a jump or ladder when one is supplied.
    /// `animation` is ordinary MOVE's authored walk/run loader slot; other move opcodes leave it unknown.
    Move { entity: u8, from: Option<(i16, i16)>, x: i16, y: i16, z: Option<i16>, triangle: Option<u16>, frames: u32, how: MoveHow, animation: Option<u8> },
    /// Turn to face a heading (radians, counter-clockwise from the field's +x axis seen from above) over `frames`.
    Face { entity: u8, heading: f32, frames: u32 },
    /// Play a clip of the entity's model (`animation` is the index in the model-loader's list).
    /// `speed` is ASPED (default 16); `divisor` preserves the animation opcode's separate literal
    /// speed byte. Neither metadata field changes the interpreter's existing wait calculation.
    Anim { entity: u8, animation: u8, how: AnimHow, first: Option<u8>, last: Option<u8>, speed: u8, divisor: u8 },
    /// `ASPED` changed the actor's animation speed. Supported full standing loops keep their phase
    /// when switching between rates 16 and 32; other forms clear the presentation override.
    AnimSpeed { entity: u8, speed: u16 },
    /// Show or hide (`VISI`).
    Show { entity: u8, visible: bool },
    /// Play a literal `SOUND` (channel 1), or `AKAO`/`AKAO2` operation 40 or 41
    /// (channels 1 and 2); id 0 stops that channel. Pan is the script's byte.
    /// Higher scene channels remain unsupported; field ambience owns its existing loops.
    Sound { channel: u8, id: u16, pan: u8 },
    Camera(CameraOp),
    /// The field's one authored additive/subtractive fade, before dialogue and HUD.
    Fade(FadeOp),
    /// Change authored background groups or palette state; frames are bit ordinals, not tile-state bytes.
    Background(BackgroundOp),
    /// Nonblocking field-camera shake. Only enabled axes read their banked operands;
    /// disabled axes retain their previous parameters and settle back to zero.
    Shake { mask: u8, x_amplitude: u8, x_steps: u8, y_amplitude: u8, y_steps: u8 },
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
    /// Opcodes not presented (music, other sound commands, palettes, field logic): skipped.
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
    /// Explicit field-visit scratch. None preserves the historical zero/Given preview model.
    scratch: Option<FieldScratchRun>,
}

impl SceneVars {
    pub fn new(story: StoryState, given: &[Given]) -> Self {
        Self { story, given: given.to_vec(), unknown_persistent_reads: Default::default(), scratch: None }
    }

    /// Read an established visit image without inventing entry initialization or Given bytes.
    pub fn with_scratch(story: StoryState, given: &[Given], scratch: FieldScratch) -> Result<Self, SceneError> {
        if !story.supports_extended() { return Err(SceneError::ScratchStory); }
        if let Some(given) = given.iter().find(|given| matches!(given.bank, 5 | 6)) {
            return Err(SceneError::ScratchGiven { bank: given.bank, address: given.address });
        }
        Ok(Self { story, given: given.to_vec(), unknown_persistent_reads: Default::default(), scratch: Some(FieldScratchRun::new(&scratch)) })
    }

    pub fn unknown_persistent_reads(&self) -> std::collections::BTreeSet<(u8, u8)> {
        self.unknown_persistent_reads.borrow().clone()
    }

    /// Version 3 uses the game's shared byte/word banks; old replays keep their original interpretation.
    pub(super) fn key(&self, bank: u8, address: u8) -> (u8, u8) {
        if self.scratch.is_some() && matches!(bank, 5 | 6) { return (5, address); }
        let bank = if self.story.supports_extended() {
            // Temporary selectors 5/6 share bytes too, but never enter persistent story data.
            if bank == 6 { 5 } else { crate::story::canonical_persistent_bank(bank).unwrap_or(bank) }
        } else { bank };
        (bank, address)
    }

    /// A preview value without treating an overwrite or comparison for change as a script read.
    pub(super) fn baseline_byte(&self, bank: u8, address: u8) -> u8 {
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &self.scratch {
                return scratch.current().values().get(usize::from(address)).copied()
                    .unwrap_or_else(|| scratch.read_u8(bank, usize::from(address)).unwrap_or(0));
            }
        }
        if self.story.supports_extended() {
            let key = self.key(bank, address);
            let mask = self.story.known_mask(bank, address);
            let given = self.given.iter().find(|g| self.key(g.bank, g.address) == key).map_or(0, |g| g.value);
            return (self.story.byte(bank, address) & mask) | (given & !mask);
        }
        let moment = bank == MOMENT_BANK && address <= MOMENT_ADDRESS.wrapping_add(1);
        if !moment {
            if let Some(g) = self.given.iter().find(|g| g.bank == bank && g.address == address) {
                return match tracked_var(bank, address) {
                    Some(t) => (g.value & !t.bits) | (self.story.byte(bank, address) & t.bits),
                    None => g.value,
                };
            }
        }
        self.story.byte(bank, address)
    }

    fn audit_mask(&self, bank: u8, address: u8, mask: u8) {
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &self.scratch { let _ = scratch.read_masked_u8(bank, usize::from(address), mask); return; }
        }
        let persistent = if self.story.supports_extended() {
            crate::story::canonical_persistent_bank(bank).is_some()
        } else { (1..=4).contains(&bank) };
        if persistent && self.story.known_mask(bank, address) & mask != mask {
            self.unknown_persistent_reads.borrow_mut().insert(self.key(bank, address));
        }
    }
}

impl StoryVars for SceneVars {
    // The legacy dialogue walker has no checked visit outcome/fault contract. Only the
    // explicit SceneVM/ScenePlay path may execute these variables.
    fn execution_supported(&self) -> bool { self.scratch.is_none() && self.story.execution_supported() }

    fn byte(&self, bank: u8, address: u8) -> u8 {
        self.audit_mask(bank, address, u8::MAX);
        self.baseline_byte(bank, address)
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        self.story.party_member(slot)
    }
}

/// Dialogue substitutions read the same native main slots as GETPC, including authored
/// changes earlier in this segment. An unknown slot is an unresolved read, never Cloud or empty.
struct NativeMessageVars<'a> {
    story: &'a StoryState,
    unknown: std::cell::Cell<bool>,
}

impl StoryVars for NativeMessageVars<'_> {
    fn byte(&self, bank: u8, address: u8) -> u8 { self.story.var(bank, address) }
    fn party_member(&self, slot: u8) -> Option<u8> {
        match self.story.native_party().and_then(|party| party.known_main_slot(slot).ok()).flatten() {
            Some(255) => None,
            Some(member) => Some(member),
            None => { self.unknown.set(true); None }
        }
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
    /// OFSTW waits for the current offset, including a replacement by another script.
    WaitOffset(u8),
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
    /// Authored background work and its descendants run during finite scene work only.
    ambient: bool,
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
    instruction_offset: u32,
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
    animation: Option<u8>,
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
    battle_checkpoint: Option<BattleCheckpoint>,
    battle_ordinal: u32,
    battle_flags: Option<u16>,
    window_checkpoint: Option<WindowCheckpoint>,
    window_ordinal: u32,
    implicit_choice: bool,
    invalid_message: bool,
    limits: Limits,
    vars: SceneVars,
    writes: BTreeMap<(u8, u8), u8>,
    /// Bits assigned by this run. A bit write does not initialize its unknown neighbours.
    written_masks: BTreeMap<(u8, u8), u8>,
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
    /// Presentation offsets survive placement and quiet setup without changing logical places.
    offsets: BTreeMap<u8, [i16; 3]>,
    offset_moves: BTreeMap<u8, (u32, super::OffsetMotion)>,
    headings: BTreeMap<u8, f32>,
    visible: BTreeMap<u8, bool>,
    speeds: BTreeMap<u8, i32>,
    anim_speeds: BTreeMap<u8, u32>,
    /// Last default loop, including those set during quiet setup. A later non-loop clears it.
    default_anims: BTreeMap<u8, Stage>,
    /// Standing defaults survive transient animations. Missing entries mean the engine's initial
    /// slot 0; an explicit `None` records a speed change whose presentation is unsupported.
    standing_defaults: BTreeMap<u8, Option<Stage>>,
    anim_ends: BTreeMap<u8, u32>,
    camera_end: u32,
    fade: FadeState,
    fade_at: u32,
    /// Background changes retained from quiet setup (ordinary playback emits timed events).
    initial_background: Vec<BackgroundOp>,
    /// Change detection for idle-loop parking; presentation still receives redundant commands
    /// because the field may have inherited state from a previous scene.
    background_state: BackgroundState,
    geometry: BTreeMap<u8, (i16, i16, u16, u16)>,
    /// What the scripts did to the party, in the order they did it (setup included): see [`Vm::party_ops`].
    party_ops: Vec<PartyOp>,
    /// Main-party slots read by GETPC, including holes; field-party order can differ.
    party_main: [u8; 3],
    /// Semantic field tuple for old previews and for replaying the published party journal.
    /// Version 3 operations read their authoritative raw inputs from bank3 instead.
    party_bank: [u8; 3],
    /// Source-validated MENU6 sites that retain the existing canonical names.
    default_name_sites: Vec<(usize, u8)>,
    /// Ordered persistent assignments and native party operations for this exact v4 segment.
    /// The original SceneVars story remains the immutable beforeimage; scratch stays in writes.
    native_story: Option<NativeStoryBuilder>,
    /// A malformed or representation-changing internal rebase may never resurrect execution.
    invalid_native_state: bool,
    /// Checked existing-visit setup may not change scratch, even temporarily. Also catches
    /// public SceneVars fields changed after its checked constructor validated them.
    invalid_scratch_setup: bool,
    /// A checked instruction whose native result is undefined/unrepresentable cannot later
    /// become a valid visit outcome merely because transient statistics were reset.
    invalid_scratch_execution: bool,
    /// Checked quiet setup changed persistent knowledge/value, party state or resources at
    /// some point. Restoring the original image and resetting counters never clear this audit.
    setup_state_changed: bool,
    resources: ResourceOutcome,
    stats: Stats,
}

impl Vm {
    pub fn new(vars: SceneVars, seed: u32, limits: Limits) -> Self {
        let invalid_scratch_setup = vars.scratch.is_some()
            && (!vars.story.supports_extended() || vars.given.iter().any(|given| matches!(given.bank, 5 | 6)));
        let native_story = vars.story.supports_native_party().then(|| NativeStoryBuilder::new(&vars.story)).transpose();
        let invalid_native_state = native_story.is_err();
        let native_story = native_story.ok().flatten();
        // These tuples are exclusively the legacy compatibility model. Native execution reads
        // the checked builder state and never falls back from unknown main slots to this tuple.
        let party = if vars.story.supports_native_party() { [255; 3] }
            else { [0, 1, 2].map(|slot| vars.party_member(slot).unwrap_or(255)) };
        Self {
            jump: None,
            battle: None,
            battle_checkpoint: None,
            battle_ordinal: 0,
            battle_flags: None,
            window_checkpoint: None,
            window_ordinal: 0,
            implicit_choice: false,
            invalid_message: false,
            limits,
            vars,
            writes: BTreeMap::new(),
            written_masks: BTreeMap::new(),
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
            offsets: BTreeMap::new(),
            offset_moves: BTreeMap::new(),
            headings: BTreeMap::new(),
            visible: BTreeMap::new(),
            speeds: BTreeMap::new(),
            anim_speeds: BTreeMap::new(),
            default_anims: BTreeMap::new(),
            standing_defaults: BTreeMap::new(),
            anim_ends: BTreeMap::new(),
            camera_end: 0,
            fade: FadeState::default(),
            fade_at: 0,
            initial_background: Vec::new(),
            background_state: BackgroundState::default(),
            geometry: BTreeMap::new(),
            party_ops: Vec::new(),
            party_main: party,
            party_bank: party,
            default_name_sites: Vec::new(),
            native_story,
            invalid_native_state,
            invalid_scratch_setup,
            invalid_scratch_execution: false,
            setup_state_changed: false,
            resources: ResourceOutcome::default(),
            stats: Stats::default(),
        }
    }

    pub fn with_resources(vars: SceneVars, seed: u32, limits: Limits, resources: SceneResources) -> Self {
        let mut vm = Self::new(vars, seed, limits);
        vm.resources.gil_before = resources.gil;
        vm.resources.gil_after = resources.gil;
        vm
    }

    pub fn resources(&self) -> &ResourceOutcome { &self.resources }

    pub fn checked_scratch(&self) -> bool { self.vars.scratch.is_some() }
    pub fn scratch_before(&self) -> Option<&FieldScratch> { self.vars.scratch.as_ref().map(FieldScratchRun::before) }
    pub fn scratch_current(&self) -> Option<&FieldScratch> { self.vars.scratch.as_ref().map(FieldScratchRun::current) }
    pub fn scratch_setup_changed(&self) -> bool { self.invalid_scratch_setup }
    /// Whether checked quiet setup left all persistent, party, resource and scratch state
    /// untouched throughout its execution. Unchanged assignments are allowed; a transient
    /// change followed by restoration is not neutral. This audit survives every VM reset.
    /// Unchecked constructors retain their previous behavior and do not perform this audit.
    pub fn setup_is_state_neutral(&self) -> bool { !self.setup_state_changed && !self.invalid_scratch_setup }
    fn checked_execution_refused(&self) -> bool {
        self.checked_scratch() && (self.stats.setup_incomplete || self.stats.unresolved != 0
            || self.stats.dropped_requests != 0 || !self.stats.unknown.is_empty()
            || matches!(self.done, Some(End::Broken | End::Budget)))
    }
    pub fn scratch_valid(&self) -> bool {
        !self.invalid_scratch_setup && !self.invalid_scratch_execution && !self.checked_execution_refused()
            && self.vars.scratch.as_ref().is_none_or(|scratch| scratch.fault().is_none()
                && self.vars.unknown_persistent_reads.borrow().is_empty())
    }
    /// Checked failure remains Some(Err), never the unchecked constructor's None.
    pub fn scratch_outcome(&self) -> Option<Result<FieldScratchOutcome, FieldScratchError>> {
        self.vars.scratch.as_ref().map(|scratch| {
            if let Some(error) = scratch.fault() { Err(error) }
            else if self.invalid_scratch_setup || self.invalid_scratch_execution || self.checked_execution_refused()
                || !self.vars.unknown_persistent_reads.borrow().is_empty() { Err(FieldScratchError::InvalidOutcome) }
            else { scratch.snapshot() }
        })
    }

    /// Native persistent state at the current instruction; no scratch or external resources.
    pub fn current_persistent_story(&self) -> Option<&StoryState> { self.native_story.as_ref().map(NativeStoryBuilder::state) }

    /// Ordered v4 assignments and native effects, including quiet setup and unchanged writes.
    pub fn native_journal(&self) -> Option<NativeStoryJournal> { self.native_story.as_ref().map(NativeStoryBuilder::snapshot) }

    /// Refuse a malformed or representation-changing internal battle rebase before executing.
    /// Clear unpublished output; the refusal survives begin and statistics resets.
    fn refuse_invalid_native_state(&mut self) -> bool {
        if !self.invalid_native_state { return false; }
        self.stats.setup_incomplete = true;
        self.stats.unresolved = self.stats.unresolved.max(1);
        self.done = Some(End::Broken);
        self.pending = None;
        self.queued_note = None;
        self.out.clear();
        self.writes.clear();
        self.written_masks.clear();
        self.party_ops.clear();
        self.native_story = None;
        self.resources.gil_ops.clear();
        self.resources.gil_after = self.resources.gil_before;
        self.jump = None;
        self.battle = None;
        self.battle_checkpoint = None;
        self.battle_flags = None;
        self.window_checkpoint = None;
        true
    }

    /// Installed by ScenePlay only after validating the original script and semantic scene.
    /// An empty list keeps every MENU unsupported; this is not a general name-editor capability.
    pub(super) fn allow_default_name_sites(&mut self, sites: Vec<(usize, u8)>) {
        self.default_name_sites = sites;
    }

    /// Evaluate a native operation on the same ordered state as direct persistent writes.
    fn change_native_party(&mut self, operation: NativePartyOp) -> bool {
        if !self.scratch_valid() { return false; }
        let Some(native) = &mut self.native_story else { return false };
        let effect = match native.party(operation) {
            Ok(effect) => effect,
            Err(_) => {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                return false;
            }
        };
        if effect.changed() { self.epoch = self.epoch.wrapping_add(1); }
        let assigned = effect.assignments().raw_slots;
        for (slot, (value, known)) in effect.after().raw_slots().into_iter().enumerate() {
            if assigned & (1 << slot) != 0 { self.record_assignment_map(3, 9 + slot as u8, value, known); }
        }
        true
    }

    /// Resolve an U8 operand before a native operation. An unknown persistent input is never
    /// converted to a preview/Given value and then used to mutate the native party.
    fn native_operand_u8(&mut self, ins: &Instruction, name: &str) -> Option<u8> {
        let operand = ins.operand(name)?;
        if operand.bank == 0 { return u8::try_from(operand.value).ok(); }
        let address = u8::try_from(operand.value).ok()?;
        if !matches!(operand.bank, 5 | 6) && crate::story::canonical_persistent_bank(operand.bank).is_none() {
            if self.checked_scratch() { let _ = self.var_byte(operand.bank, address); }
            return None;
        }
        if matches!(operand.bank, 5 | 6) {
            if let Some(scratch) = &self.vars.scratch { return scratch.read_u8(operand.bank, usize::from(address)).ok(); }
        }
        let value = self.var_byte(operand.bank, address);
        let known = matches!(operand.bank, 5 | 6)
            || self.native_story.as_ref()?.state().known_mask(operand.bank, address) == 255;
        if !known { self.stats.unresolved = self.stats.unresolved.saturating_add(1); }
        known.then_some(value)
    }

    fn default_name_menu(&mut self, ins: &Instruction, class: &mut Class) {
        if !self.vars.story.supports_extended() {
            *class = Class::Ignored;
            return;
        }
        let param = ins.literal("param").and_then(|value| u8::try_from(value).ok());
        if ins.stray_banks || ins.literal("type") != Some(6)
            || !param.is_some_and(|param| param <= 1 && self.default_name_sites.contains(&(ins.offset, param))) {
            *class = Class::Ignored;
            return;
        }
        if self.native_story.is_some() {
            if !self.change_native_party(NativePartyOp::FieldSync) { *class = Class::Unknown; }
            return;
        }
        // Original OpcodeFuncMenu's completion calls FIELD_Init (field_opcodes.c6664), also
        // called on native main/110B8.c471 menu return. It reconciles raw slots with the saved
        // main party without changing names, main membership or PHS availability.
        let bank = [9, 10, 11].map(|address| self.var_byte(3, address));
        if [9, 10, 11].into_iter().any(|address|
            (self.vars.story.known_mask(3, address) | self.written_mask(3, address)) != 255) {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            *class = Class::Unknown;
            return;
        }
        let Ok(change) = crate::script::party::sync_from_main(bank, self.party_main) else {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            *class = Class::Unknown;
            return;
        };
        // The journal's semantic raw tuple stays where the last authored PartyOp left it.
        // A later dependent PartyOp compares it to these actual bytes and resolves its journal.
        for ((address, value), assigned) in [9, 10, 11].into_iter().zip(change.bank).zip(change.assigned) {
            if assigned { self.record_assignment(3, address, value, 255); }
        }
    }

    /// The main-party slots read by GETPC, including the holes left by PRTYM.
    fn party_now(&self) -> [u8; 3] { self.party_main }

    /// IFPRTYQ reads the physical field tuple; GETPC independently reads the main tuple.
    /// Keep the original early return, so a match does not depend on later unexamined slots.
    fn field_party_contains(&mut self, character: u8) -> Option<bool> {
        if !self.vars.story.supports_extended() { return Some(self.party_bank.contains(&character)); }
        for address in 9..=11 {
            let member = self.var_byte(3, address);
            if (self.vars.story.known_mask(3, address) | self.written_mask(3, address)) != 255 {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                return None;
            }
            if member == character { return Some(true); }
        }
        Some(false)
    }

    /// Apply the original field/main tuple transition. No initial physical bytes are invented:
    /// only an authored assignment becomes a write, and a full PRTYE without 255 reads no old tuple.
    fn change_party(&mut self, op: PartyOp) -> bool {
        if self.native_story.is_some() {
            return self.change_native_party(match op {
                PartyOp::Add(character) => NativePartyOp::Add(character),
                PartyOp::Remove(character) => NativePartyOp::Remove(character),
                PartyOp::Set(slots) => NativePartyOp::Set(slots),
            });
        }
        use crate::script::party::transition;
        let extended = self.vars.story.supports_extended();
        let replaces_all = matches!(op, PartyOp::Set(slots) if !slots.contains(&255));
        let bank = if extended && !replaces_all {
            let raw = [9, 10, 11].map(|address| self.var_byte(3, address));
            if [9, 10, 11].into_iter().any(|address|
                (self.vars.story.known_mask(3, address) | self.written_mask(3, address)) != 255) {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                return false;
            }
            raw
        } else { self.party_bank };
        let Ok(change) = transition(bank, self.party_main, op) else { return false };
        if extended {
            let compact = change.bank.iter().skip_while(|&&member| member != 255).all(|&member| member == 255);
            if change.main != change.bank || !compact {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                return false;
            }
        }
        // A dependent operation may have consumed a different physical tuple from the journal's
        // canonical input. Publish resolved slots in that case so replay sees the exact same result.
        let replay = transition(self.party_bank, self.party_main, op);
        let journal = if replay.is_ok_and(|r| r.bank == change.bank && r.main == change.main) { op }
            else { PartyOp::Set(change.bank.map(|member| if member == 255 { 254 } else { member })) };
        if !transition(self.party_bank, self.party_main, journal)
            .is_ok_and(|r| r.bank == change.bank && r.main == change.main) { return false; }
        if self.party_bank != change.bank || self.party_main != change.main { self.epoch = self.epoch.wrapping_add(1); }
        self.party_bank = change.bank;
        self.party_main = change.main;
        self.party_ops.push(journal);
        if extended {
            for ((address, value), assigned) in [9, 10, 11].into_iter().zip(change.bank).zip(change.assigned) {
                if assigned { self.record_assignment(3, address, value, 255); }
            }
        }
        true
    }

    /// Authored party changes in order, with dependent inputs resolved when replay needs them.
    /// Version 3 also journals the actual bank3/4 assignments. The caller validates the resulting
    /// main party against the durable story table ([`crate::story::canon_party`]).
    pub fn party_ops(&self) -> &[PartyOp] {
        &self.party_ops
    }

    pub(crate) fn note_unresolved_motion(&mut self) {
        self.stats.unresolved = self.stats.unresolved.saturating_add(1);
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn destination(&self) -> Option<SceneJump> { self.jump }

    /// Authored group changes from quiet setup, replayed once in the initial presentation.
    pub fn initial_background_ops(&self) -> &[BackgroundOp] { &self.initial_background }

    /// The counters so far, which start again from nothing (the setup's are kept apart from the scene's).
    pub fn take_stats(&mut self) -> Stats {
        if self.checked_execution_refused() { self.invalid_scratch_execution = true; }
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

    /// The model's render offset, including quiet setup's result; a new field starts at zero.
    /// Logical position reads and movement continue to use [`Vm::place_of`].
    pub fn offset(&self, entity: u8) -> [i16; 3] {
        self.offset_at(entity, self.clock)
    }

    /// Scripts run before the model updater: a replacement at F+1 sees step 0,
    /// not the step 1 that will be drawn after that script pass.
    fn offset_at(&self, entity: u8, time: u32) -> [i16; 3] {
        self.offset_moves.get(&entity).map_or_else(
            || self.offsets.get(&entity).copied().unwrap_or([0; 3]),
            |(start, motion)| motion.at(time.saturating_sub(*start).saturating_sub(1)))
    }

    fn offsets_running_at(&self, time: u32) -> bool {
        self.offset_moves.values().any(|(start, motion)| time < start.saturating_add(motion.completion_frames()))
    }

    /// Original offsets continue while MES/ASK holds a script. The current scene
    /// timeline cannot reproduce an arbitrary dialogue dwell, so durable boundaries
    /// require the motion to have completed; legacy previews retain scene-time playback.
    fn unresolved_offset_boundary(&mut self, time: u32) -> bool {
        if self.vars.story.supports_extended() && self.offsets_running_at(time) {
            self.note_unresolved_motion();
            true
        } else { false }
    }

    /// The heading the scene knows `entity` has.
    pub fn heading_of(&self, entity: u8) -> Option<f32> {
        self.headings.get(&entity).copied()
    }

    /// Whether the scene has hidden `entity` (`Some(false)`), shown it (`Some(true)`) or said nothing.
    pub fn visible_of(&self, entity: u8) -> Option<bool> {
        self.visible.get(&entity).copied()
    }

    /// The last default loop still active for `entity`, including quiet setup's result.
    pub fn default_animation_of(&self, entity: u8) -> Option<&Stage> {
        self.default_anims.get(&entity)
    }

    /// The remembered standing default, independently of the currently displayed setup pose.
    pub(super) fn standing_default_animation_of(&self, entity: u8) -> Option<Stage> {
        self.standing_defaults.get(&entity).cloned().unwrap_or_else(|| Some(Stage::Anim {
            entity, animation: 0, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1,
        }))
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
        if field_wide_bytes(bank).is_none() {
            if let Some(scratch) = &self.vars.scratch { let _ = scratch.read_u8(bank, usize::from(address)); return 0; }
        }
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &self.vars.scratch { return scratch.read_u8(bank, usize::from(address)).unwrap_or(0); }
        }
        if !self.vars.story.supports_extended() {
            return self.writes.get(&(bank, address)).copied().unwrap_or_else(|| self.vars.byte(bank, address));
        }
        let key = self.vars.key(bank, address);
        let written = self.written_masks.get(&key).copied().unwrap_or(0);
        self.vars.audit_mask(bank, address, !written);
        self.peek_byte(bank, address)
    }

    fn peek_byte(&self, bank: u8, address: u8) -> u8 {
        self.writes.get(&self.vars.key(bank, address)).copied().unwrap_or_else(|| self.vars.baseline_byte(bank, address))
    }

    pub(super) fn written_mask(&self, bank: u8, address: u8) -> u8 {
        self.written_masks.get(&self.vars.key(bank, address)).copied().unwrap_or(0)
    }

    fn var_word(&self, bank: u8, address: u8) -> u16 {
        if let Some(scratch) = &self.vars.scratch {
            if matches!(bank, 5 | 6) { return scratch.read_u16(bank, usize::from(address)).unwrap_or(0); }
            match field_wide_bytes(bank) {
                Some(1) => return u16::from(self.var_byte(bank, address)),
                Some(2) if address == 255 => { let _ = scratch.read_masked_u16(6, 255, 0); return 0; }
                Some(2) => {},
                _ => { let _ = scratch.read_u8(bank, usize::from(address)); return 0; }
            }
        }
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &self.vars.scratch { return scratch.read_u16(bank, usize::from(address)).unwrap_or(0); }
        }
        if self.vars.story.supports_extended() && address == u8::MAX {
            // There is no high byte at this address. Do not let a guessed zero authorize an outcome.
            self.vars.unknown_persistent_reads.borrow_mut().insert(self.vars.key(bank, address));
            return 0;
        }
        let low = self.var_byte(bank, address);
        let high = address.checked_add(1).map_or(0, |a| self.var_byte(bank, a));
        u16::from_le_bytes([low, high])
    }

    fn set_byte(&mut self, bank: u8, address: u8, value: u8) {
        self.set_masked(bank, address, value, u8::MAX);
    }

    /// Version 3 cannot represent a direct raw/main divergence and refuses these writes.
    /// Version 4 journals arbitrary raw assignments independently from its native main tuple.
    fn party_mirror_write(&self, bank: u8, address: u8, wide: bool) -> bool {
        self.vars.story.supports_extended() && !self.vars.story.supports_native_party() && self.vars.key(bank, address).0 == 3
            && ((9..=11).contains(&address) || (wide && address.checked_add(1).is_some_and(|next| (9..=11).contains(&next))))
    }

    fn set_masked(&mut self, bank: u8, address: u8, value: u8, mask: u8) {
        if self.party_mirror_write(bank, address, false) {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return;
        }
        self.record_assignment(bank, address, value, mask);
    }

    /// Record a validated generic assignment, or an authored party opcode's explicit physical write.
    fn record_assignment(&mut self, bank: u8, address: u8, value: u8, mask: u8) {
        if !self.scratch_valid() { return; }
        if field_wide_bytes(bank).is_none() {
            if let Some(scratch) = &self.vars.scratch { let _ = scratch.read_u8(bank, usize::from(address)); return; }
        }
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &mut self.vars.scratch {
                if scratch.write_masked_u8(bank, usize::from(address), value, mask).is_err() { return; }
            }
        }
        if crate::story::canonical_persistent_bank(bank).is_some() {
            if let Some(native) = &mut self.native_story {
                if native.write_masked(bank, address, value, mask).is_err() {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return;
                }
            }
        }
        self.record_assignment_map(bank, address, value, mask);
    }

    /// Diagnostic overlay update after a native effect has already journaled its assignments.
    fn record_assignment_map(&mut self, bank: u8, address: u8, value: u8, mask: u8) {
        let key = self.vars.key(bank, address);
        let old = if self.vars.story.supports_extended() || self.checked_scratch() { self.peek_byte(bank, address) } else { self.var_byte(bank, address) };
        let old_mask = self.written_masks.get(&key).copied().unwrap_or(0);
        if self.quiet && self.checked_scratch() && self.native_story.is_none()
            && crate::story::canonical_persistent_bank(bank).is_some() {
            let known = self.vars.story.known_mask(bank, address) | old_mask;
            self.setup_state_changed |= (old ^ value) & mask != 0 || known & mask != mask;
        }
        if old != value || (self.vars.story.supports_extended() && old_mask & mask != mask) {
            self.epoch = self.epoch.wrapping_add(1);
        }
        self.writes.insert(key, value);
        self.written_masks.insert(key, old_mask | mask);
    }

    fn set_word(&mut self, bank: u8, address: u8, value: u16) {
        if !self.scratch_valid() { return; }
        if self.checked_scratch() && !matches!(bank, 5 | 6) {
            match field_wide_bytes(bank) {
                Some(1) => { self.set_byte(bank, address, value as u8); return; }
                Some(2) if address < 255 => {},
                _ => {
                    if let Some(scratch) = &self.vars.scratch {
                        if field_wide_bytes(bank) == Some(2) { let _ = scratch.read_masked_u16(6, usize::from(address), 0); }
                        else { let _ = scratch.read_u8(bank, usize::from(address)); }
                    }
                    return;
                }
            }
        }
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &mut self.vars.scratch {
                if scratch.write_u16(bank, usize::from(address), value).is_err() { return; }
                let [low, high] = value.to_le_bytes();
                self.record_assignment_map(bank, address, low, 255);
                if bank == 6 { self.record_assignment_map(bank, address + 1, high, 255); }
                return;
            }
        }
        if (self.vars.story.supports_extended() && address == u8::MAX) || self.party_mirror_write(bank, address, true) {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return; // Refuse the whole assignment, including an otherwise valid low byte.
        }
        let [low, high] = value.to_le_bytes();
        if crate::story::canonical_persistent_bank(bank).is_some() {
            if let Some(native) = &mut self.native_story {
                if native.write_word(bank, address, value).is_err() {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return;
                }
                self.record_assignment_map(bank, address, low, 255);
                self.record_assignment_map(bank, address + 1, high, 255);
                return;
            }
        }
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
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &self.vars.scratch {
                return scratch.read_masked_u8(bank, usize::from(address), 1 << bit).ok().map(|value| value != 0);
            }
        }
        self.writes.get(&self.vars.key(bank, address))
            .filter(|_| !self.vars.story.supports_extended() || self.written_mask(bank, address) & (1 << bit) != 0)
            .map(|value| value & (1 << bit) != 0)
            .or_else(|| self.vars.story.known_bit(bank, address, bit))
    }

    /// `(bank, address)` of the variable operand `name` (`None` for a literal: nothing to write).
    fn dest(ins: &Instruction, name: &str) -> Option<(u8, u8)> {
        let op = ins.operand(name)?;
        (op.bank != 0).then(|| (op.bank, u8::try_from(op.value & 0xFF).unwrap_or(0)))
    }

    // Original PSX handlers 800D3BB4/800D3C44 and gil helpers 80025B10/80025B48:
    // https://github.com/q-gears/q-gears-reversing-data/tree/2b155a8c5455f5fc8addd2eda9e5cd4c226abe34/reversing/ffvii
    // Amounts consist of two independently banked halves. A selector determines byte/word
    // access even though the immediate operand occupies two bytes. No code is copied here.
    // Arithmetic independently agrees with the reconstructed PC FF7MENU.lib helpers
    // C_006CBC7C/C_006CBCB9 in ergonomy-joe/ff7-worldmap@bc7576e68b118e776ccefecfbc982a702a7f9e0f
    // (static disassembly; not a byte-match against the owner's original executable).
    fn resource_half(&self, ins: &Instruction, name: &str) -> Option<u16> {
        let op = ins.operand(name)?;
        if op.bank == 0 { return Some(op.value as u16); }
        let address = (op.value & 255) as u8;
        if self.checked_scratch() {
            let value = self.var_word(op.bank, address);
            return self.scratch_valid().then_some(value);
        }
        if matches!(op.bank, 5 | 6) {
            if let Some(scratch) = &self.vars.scratch { return scratch.read_u16(op.bank, usize::from(address)).ok(); }
        }
        let wide = match op.bank {
            1 | 3 | 5 | 11 | 13 | 15 => false,
            2 | 4 | 6 | 7 | 12 | 14 => true,
            _ => return None,
        };
        let read = |address| {
            let value = self.var_byte(op.bank, address);
            let known = self.vars.story.known_mask(op.bank, address) | self.written_mask(op.bank, address);
            // Temporary variables have the field's initial zero value; a persistent missing
            // byte is unknown even if a preview's Given happened to supply a convenient value.
            (matches!(op.bank, 5 | 6) || known == 255).then_some(value)
        };
        let low = read(address)?;
        Some(if wide { u16::from_le_bytes([low, read(address.checked_add(1)?)?]) } else { u16::from(low) })
    }

    fn change_gil(&mut self, ins: &Instruction, class: &mut Class) {
        let amount = self.resource_half(ins, "amount_lo").zip(self.resource_half(ins, "amount_hi"))
            .map(|(lo, hi)| u32::from(lo) | (u32::from(hi) << 16));
        let Some(amount) = amount else {
            self.resources.unknown_gil = true;
            self.resources.gil_after = None;
            *class = Class::Unknown;
            return;
        };
        let operation = if ins.opcode == 0x39 { GilOp::Add(amount) } else { GilOp::Subtract(amount) };
        self.resources.gil_ops.push(operation);
        let Some(before) = self.resources.gil_after else {
            self.resources.unknown_gil = true;
            *class = Class::Ignored;
            if ins.opcode == 0x39 { self.gift_not_given = true; }
            return;
        };
        // The original helpers saturate at the storage limits, not the displayed 99,999,999.
        // GOLDd ignores the helper's return, so an insufficient debit reaches zero and continues.
        let after = if ins.opcode == 0x39 { before.saturating_add(amount) } else { before.saturating_sub(amount) };
        self.resources.gil_after = Some(after);
        if after != before { self.epoch = self.epoch.wrapping_add(1); }
    }

    fn read_gil(&mut self, ins: &Instruction, class: &mut Class) {
        self.resources.gil_ops.push(GilOp::Read);
        let Some(gil) = self.resources.gil_after else {
            self.resources.unknown_gil = true;
            *class = Class::Ignored;
            return;
        };
        // Overflow during preflight must fault before either CHGLD output is published.
        if let Some(scratch) = &self.vars.scratch {
            for name in ["var1", "var2"] {
                if let Some((bank, address)) = Self::dest(ins, name) {
                    if field_wide_bytes(bank).is_none() || (field_wide_bytes(bank) == Some(2) && address == 255) {
                        if field_wide_bytes(bank) == Some(2) { let _ = scratch.read_masked_u16(6, 255, 0); }
                        else { let _ = scratch.read_u8(bank, usize::from(address)); }
                        *class = Class::Unknown;
                        return;
                    }
                } else {
                    self.invalid_scratch_execution = true;
                    *class = Class::Unknown;
                    return;
                }
            }
        }
        let destination = |name| {
            let (bank, address) = Self::dest(ins, name)?;
            let wide = match bank {
                1 | 3 | 5 | 11 | 13 | 15 => false,
                2 | 4 | 6 | 7 | 12 | 14 => true,
                _ => return None,
            };
            (!wide || address < 255).then_some((bank, address, wide))
        };
        let (Some(low), Some(high)) = (destination("var1"), destination("var2")) else {
            // The original reports invalid store selectors. Do not publish an incomplete read.
            *class = Class::Unknown;
            return;
        };
        // Preflight both destinations: refusing the high output must not leave a low-half write.
        if [low, high].into_iter().any(|(bank, address, wide)| self.party_mirror_write(bank, address, wide)) {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            *class = Class::Unknown;
            return;
        }
        // CHGLD writes the low half first, then the high half; overlapping destinations matter.
        for ((bank, address, wide), value) in [(low, gil as u16), (high, (gil >> 16) as u16)] {
            if wide { self.set_word(bank, address, value); } else { self.set_byte(bank, address, value as u8); }
        }
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
        let Some(var) = Self::dest(ins, "var") else {
            if self.checked_scratch() { self.invalid_scratch_execution = true; }
            return;
        };
        let cur = if matches!(op, Arith::Set) {
            0
        } else if wide {
            let value = self.var_word(var.0, var.1);
            if self.checked_scratch() { i64::from(value as i16) } else { i64::from(value) }
        } else {
            i64::from(self.var_byte(var.0, var.1))
        };
        let value = i64::from(self.num(ins, "value", if wide { Width::S16 } else { Width::U8 }));
        if !self.scratch_valid() { return; }
        let new = match op {
            Arith::Set => value,
            Arith::Plus => cur.saturating_add(value),
            Arith::Minus => cur.saturating_sub(value),
            Arith::Mul => cur.saturating_mul(value),
            Arith::Div => match cur.checked_div(value) {
                Some(q) => q,
                None => {
                    if self.checked_scratch() { self.invalid_scratch_execution = true; self.stats.unresolved = self.stats.unresolved.saturating_add(1); }
                    return;
                }
            },
            Arith::Mod => match cur.checked_rem(value) {
                Some(r) => r,
                None => {
                    if self.checked_scratch() { self.invalid_scratch_execution = true; self.stats.unresolved = self.stats.unresolved.saturating_add(1); }
                    return;
                }
            },
            Arith::And => cur & value,
            Arith::Or => cur | value,
            Arith::Xor => cur ^ value,
            Arith::Inc => cur.saturating_add(1),
            Arith::Dec => cur.saturating_sub(1),
        };
        if self.checked_scratch() && wide && saturate {
            // Verified PC handlers: PLS2!/INC2! cap above32767, while MNS2!/DEC2!
            // compare against positive32768 and preserve low16bits of that maximum.
            let value = if matches!(op, Arith::Plus | Arith::Inc) { new.min(32767) } else { new.max(32768) };
            self.store(var, true, value, false);
        } else { self.store(var, wide, new, saturate); }
    }

    /// Checked `BITON`/`BITOFF`/`BITXOR` use the addressed byte; only bit indices 0..7
    /// have reviewed native semantics. Legacy previews retain their historical row-of-bits behavior.
    fn bit_op(&mut self, ins: &Instruction, op: u8) {
        let Some((bank, address)) = Self::dest(ins, "var") else {
            if self.checked_scratch() { self.invalid_scratch_execution = true; }
            return;
        };
        let bit = self.num(ins, "bit", Width::U8).rem_euclid(256);
        if self.checked_scratch() && bit >= 8 {
            self.invalid_scratch_execution = true;
            return;
        }
        self.flip_bit(bank, address, bit, op);
    }

    fn flip_bit(&mut self, bank: u8, address: u8, bit: i32, op: u8) {
        if !self.scratch_valid() { return; }
        if matches!(bank, 5 | 6) {
            if let Some(scratch) = &mut self.vars.scratch {
                let Ok(bit) = usize::try_from(bit) else { return };
                let at = usize::from(address).saturating_add(bit / 8);
                let mask = 1u8 << (bit % 8);
                let value = match op {
                    0x82 => mask,
                    0x83 => 0,
                    _ => match scratch.read_masked_u8(bank, at, mask) { Ok(value) => value ^ mask, Err(_) => return },
                };
                if scratch.write_masked_u8(bank, at, value, mask).is_err() { return; }
                let Some(value) = scratch.current().values().get(at).copied() else {
                    self.invalid_scratch_execution = true;
                    return;
                };
                self.record_assignment_map(bank, at as u8, value, mask);
                return;
            }
        }
        let byte_at = address.checked_add(u8::try_from(bit / 8).unwrap_or(0));
        if byte_at.is_none() {
            if let Some(scratch) = &self.vars.scratch {
                let at = usize::from(address).saturating_add(usize::try_from(bit / 8).unwrap_or(0));
                let _ = scratch.read_masked_u8(5, at, 0);
            }
        }
        let (Some(byte_at), Some(mask)) = (byte_at, 1u8.checked_shl(u32::try_from(bit % 8).unwrap_or(0))) else { return };
        let extended = self.vars.story.supports_extended();
        let cur = if extended {
            if op != 0x82 && op != 0x83 {
                // XOR needs the old selected bit; setting or clearing does not.
                self.vars.audit_mask(bank, byte_at, mask & !self.written_mask(bank, byte_at));
            }
            self.peek_byte(bank, byte_at)
        } else { self.var_byte(bank, byte_at) };
        let new = match op {
            0x82 => cur | mask,
            0x83 => cur & !mask,
            _ => cur ^ mask,
        };
        self.set_masked(bank, byte_at, new, if extended { mask } else { u8::MAX });
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
            ambient: false,
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
        let (idx, time, _) = self.threads.iter().enumerate().filter_map(|(i, t)| {
            let time = match t.state {
                State::Run => t.time,
                State::WaitOffset(entity) => {
                    // A native higher-priority script owns the entity until RET,
                    // including its WAIT/REQEW/OFSTW or parked state. Do not let an
                    // older waiter observe an intermediate cancellation between two
                    // instructions of that owner. WaitSlot has not acquired ownership.
                    if self.threads.iter().any(|owner| owner.entity == t.entity && owner.prio < t.prio
                        && !matches!(owner.state, State::Done | State::WaitSlot)) { return None; }
                    let end = self.offset_moves.get(&entity).map_or(self.clock,
                        |(start, motion)| start.saturating_add(motion.completion_frames()));
                    t.time.max(end).max(self.clock)
                },
                _ => return None,
            };
            Some((i, time, t.seq))
        }).min_by_key(|(_, time, seq)| (*time, *seq))?;
        // Do not move a waiter's stored time forward until it wins: another script
        // may replace or cancel the motion before this projected completion frame.
        let selected = self.threads.get_mut(idx)?;
        selected.time = time;
        if matches!(selected.state, State::WaitOffset(_)) { selected.state = State::Run; }
        Some(idx)
    }

    /// A narrow, verified animation tail: CANIM2, WAIT, ANIME1, WAIT, JMPB to the first
    /// instruction. Only finish when every other foreground thread has ended or reached
    /// a due unconditional self-jump. A waiter or any other runnable instruction cannot be
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
        let at = t.time;
        self.threads.iter().enumerate().all(|(i, t)| {
            i == idx || t.ambient || t.state == State::Done || Self::quiet_self_jump(script, t, at)
                || Self::quiet_parked_presentation_loop(script, t, at)
        })
    }

    /// An ambient write may wake an idle self-jump on every epoch. Its exact instruction
    /// is still provably inert even before the scheduler parks it again. Respect the due
    /// time so reaching such a jump after a finite WAIT cannot erase that wait.
    fn quiet_self_jump(script: &FieldScript<'_>, thread: &Thread, at: u32) -> bool {
        if thread.time > at || !matches!(thread.state, State::Run | State::Parked(_)) { return false; }
        let Code::Script { pc } = thread.code else { return false };
        decode_at(script.bytes(), pc, 0).is_ok_and(|ins| ins.opcode == 0x12 && ins.jump_target == Some(pc))
    }

    /// Preserve the idle behavior of constant ASPED/background/instant-offset metadata loops.
    /// Parking proves the last lap produced no epoch-changing work. Additionally require
    /// a linear cycle containing only these presentation commands and WAIT: no condition, request,
    /// actor movement, persistent write, resource operation or window may be hidden here.
    fn quiet_parked_presentation_loop(script: &FieldScript<'_>, thread: &Thread, at: u32) -> bool {
        if thread.time > at || !matches!(thread.state, State::Parked(_)) { return false; }
        let Code::Script { pc: start } = thread.code else { return false };
        let mut pc = start;
        let mut visited = BTreeSet::new();
        for _ in 0..128 {
            if !visited.insert(pc) { return pc == start; }
            let Ok(ins) = decode_at(script.bytes(), pc, 0) else { return false };
            match ins.opcode {
                0x12 => match ins.jump_target.filter(|target| *target <= pc) {
                    Some(target) => pc = target,
                    None => return false,
                },
                0xC3 if ins.literal("move_type") == Some(0) => pc = ins.end(),
                0x24 | 0xBD | 0xC4 | 0xE0..=0xE4 => pc = ins.end(),
                _ => return false,
            }
        }
        false
    }

    /// Background field mains do not extend a finite scene. A conditional foreground poll
    /// is not quiet: an ambient write may still wake it. Slot/end waiters and asynchronously
    /// requested actor tails likewise keep ownership of completion until they actually end.
    fn foreground_finished(&self, script: &FieldScript<'_>) -> bool {
        self.threads.iter().filter(|t| !t.ambient).all(|t| {
            t.state == State::Done || Self::quiet_self_jump(script, t, self.clock)
                || Self::quiet_parked_presentation_loop(script, t, self.clock)
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
        let Goal { x, y, z, triangle, how, animation, frames } = goal;
        let time = self.thread_time(idx);
        let from = self.places.get(&entity).map(|w| (w.x, w.y));
        let frames = frames.unwrap_or_else(|| {
            let distance = from.map_or(0.0, |(fx, fy)| (f32::from(x) - f32::from(fx)).hypot(f32::from(y) - f32::from(fy)));
            self.move_frames(entity, distance)
        });
        self.places.insert(entity, Where { x, y, z, triangle });
        self.emit(time, Stage::Move { entity, from, x, y, z, triangle, frames, how, animation });
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
    /// Return whether a turn was emitted, so an unresolved/coincident target adds no wait.
    fn face_entity(&mut self, idx: usize, entity: u8, target: u8, frames: u32) -> bool {
        let (Some(a), Some(b)) = (self.places.get(&entity).copied(), self.places.get(&target).copied()) else {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return false;
        };
        let (dx, dy) = (f32::from(b.x) - f32::from(a.x), f32::from(b.y) - f32::from(a.y));
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        self.do_face(idx, entity, dy.atan2(dx), frames);
        true
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
        let divisor = ins.literal("speed").map(clamp_u8).unwrap_or(0);
        let event = Stage::Anim { entity, animation, how, first, last, speed: u8::try_from(speed.min(255)).unwrap_or(255), divisor };
        if how == AnimHow::Loop {
            self.default_anims.insert(entity, event.clone());
            self.standing_defaults.insert(entity, Some(event.clone()));
        } else {
            self.default_anims.remove(&entity);
        }
        self.emit(time, event);
        if how != AnimHow::Loop {
            let end = time.saturating_add(self.anim_frames(entity, first, last));
            self.anim_ends.insert(entity, end);
            if wait {
                self.wait_until(idx, end);
            }
        }
    }

    // ---- one instruction --------------------------------------------------------------------------------------

    fn message(&mut self, script: &FieldScript<'_>, dialog: i32) -> Option<Message> {
        let index = usize::try_from(dialog).ok()?;
        let d = script.dialogs().get(index)?;
        let raw = script.bytes().get(d.offset..d.offset.checked_add(d.len)?)?;
        let pieces = if let Some(native) = &self.native_story {
            let vars = NativeMessageVars { story: native.state(), unknown: std::cell::Cell::new(false) };
            let pieces = parse_message(raw, &vars);
            if vars.unknown.get() {
                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                self.invalid_message = true;
            }
            pieces
        } else { parse_message(raw, &self.vars) };
        // The speaker's own entity is not known here: the entity is the opener's, set by the caller.
        Some(Message::new(MessageOrigin::Canon { entity: 0, dialog: index }, pieces))
    }

    fn open(&mut self, idx: usize, entity: usize, instruction_offset: usize, window: i32, mut message: Message, pending: Option<Pending>) -> Flow {
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
        Flow::Window(Opened { instruction_offset: instruction_offset as u32, opener: entity, id: u8::try_from(window).unwrap_or(0), time: self.thread_time(idx), message, pending })
    }

    fn ask(&mut self, script: &FieldScript<'_>, idx: usize, entity: usize, ins: &Instruction) -> Flow {
        let (Some(window), Some(dialog), Some(first), Some(last)) =
            (ins.literal("window"), ins.literal("dialog"), ins.literal("first"), ins.literal("last"))
        else {
            self.implicit_choice = true;
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return Flow::Next;
        };
        let Some(mut message) = self.message(script, dialog) else {
            self.implicit_choice = true;
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return Flow::Next;
        };
        let var = Self::dest(ins, "var");
        let reversed = last < first;
        let first = usize::try_from(first).unwrap_or(0);
        // A last line the text does not have is the text's last line.
        let lines = last_page_lines(&message.pieces);
        let last = usize::try_from(last).unwrap_or(0).max(first).min(lines.saturating_sub(1));
        let held = var.map(|(bank, address)| usize::from(self.var_byte(bank, address)));
        let cursor = held.filter(|c| (first..=last).contains(c)).unwrap_or(first);
        if self.quiet || first > last || reversed || var.is_none() {
            // Preview setup retains its historical cursor behavior, but a durable replay cannot
            // pretend this was a player's accepted answer (nor an ordinary MESSAGE window).
            self.implicit_choice = true;
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
        }
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
            return self.open(idx, entity, ins.offset, window, message, None);
        }
        message.choice = Some(Choice { first, last, cursor });
        self.open(idx, entity, ins.offset, window, message, Some(Pending { var, first, last, cursor }))
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
        let setup_before = (self.quiet && self.checked_scratch()).then(|| (
            self.native_story.as_ref().map_or(0, |native| native.events().len()),
            self.party_main, self.party_bank, self.resources.gil_after,
        ));
        let flow = self.exec(script, idx, entity, &ins, &mut class);
        if let Some((events, main, bank, gil)) = setup_before {
            self.setup_state_changed |= main != self.party_main || bank != self.party_bank
                || gil != self.resources.gil_after
                || self.native_story.as_ref().is_some_and(|native|
                    native.events().iter().skip(events).any(|event| event.changed()));
        }
        self.stats.note(class, ins.mnemonic);
        if !self.scratch_valid() {
            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
            return Flow::Scene(End::Broken);
        }
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
        let ambient = self.threads.get(idx).is_some_and(|thread| thread.ambient);
        let Some(child) = self.spawn(script, entity, slot, prio, time) else { return Flow::Next };
        if let Some(thread) = self.threads.get_mut(child) { thread.ambient = ambient; }
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
            0x0A if self.native_story.is_some() => {
                // SPTYE snapshots all three operands before PartyReplace mutates any source.
                let slots = ["character1", "character2", "character3"].map(|name| self.native_operand_u8(ins, name));
                match slots {
                    [Some(a), Some(b), Some(c)] if self.change_native_party(NativePartyOp::Set([a, b, c])) => {},
                    _ => *class = Class::Unknown,
                }
                Flow::Next
            }
            0x0B if self.native_story.is_some() => {
                // GTPYE reads each raw slot immediately before its U8 store. Stage the
                // instruction so an unknown later read cannot publish a partial assignment.
                let Some(mut staged) = self.native_story.clone() else {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    *class = Class::Unknown;
                    return Flow::Next;
                };
                let mut staged_scratch = self.vars.scratch.clone();
                let mut assigned = Vec::with_capacity(3);
                let mut valid = true;
                for (slot, name) in ["var1", "var2", "var3"].into_iter().enumerate() {
                    let address = 9 + slot as u8;
                    let Some(value) = staged.state().known_byte(3, address) else {
                        self.vars.audit_mask(3, address, !self.written_mask(3, address));
                        valid = false;
                        break;
                    };
                    let Some((bank, destination)) = Self::dest(ins, name) else { valid = false; break };
                    if crate::story::canonical_persistent_bank(bank).is_some() {
                        if staged.write_byte(bank, destination, value).is_err() { valid = false; break; }
                    } else if matches!(bank, 5 | 6) {
                        if let Some(scratch) = &mut staged_scratch {
                            if scratch.write_u8(bank, usize::from(destination), value).is_err() { valid = false; break; }
                        }
                    } else { valid = false; break; }
                    assigned.push((bank, destination, value));
                }
                if valid {
                    self.native_story = Some(staged);
                    self.vars.scratch = staged_scratch;
                    for (bank, address, value) in assigned { self.record_assignment_map(bank, address, value, 255); }
                } else {
                    if self.checked_scratch() { self.invalid_scratch_execution = true; }
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    *class = Class::Unknown;
                }
                Flow::Next
            }
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
                } else if self.checked_scratch() && matches!(operation, 9 | 10) && (0..16).contains(&right) {
                    Self::dest(ins, "left").and_then(|(bank, address)| {
                        if matches!(bank, 5 | 6) {
                            return self.vars.scratch.as_ref().and_then(|scratch| scratch.read_masked_u16(bank, usize::from(address), 1u16 << right)
                                .ok().map(|value| value != 0));
                        }
                        match field_wide_bytes(bank) {
                            Some(1) if right >= 8 => Some(false),
                            Some(2) if address == 255 => {
                                if let Some(scratch) = &self.vars.scratch { let _ = scratch.read_masked_u16(6, 255, 0); }
                                None
                            }
                            Some(1 | 2) => {
                                let Some(at) = address.checked_add((right / 8) as u8) else {
                                    if let Some(scratch) = &self.vars.scratch { let _ = scratch.read_masked_u16(6, 255, 0); }
                                    return None;
                                };
                                let mask = 1u8 << (right % 8);
                                self.vars.audit_mask(bank, at, mask & !self.written_mask(bank, at));
                                Some(self.peek_byte(bank, at) & mask != 0)
                            }
                            _ => { let _ = self.var_byte(bank, address); None }
                        }
                    })
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
                let (Some(window), Some(dialog)) = (ins.literal("window"), ins.literal("dialog")) else {
                    self.invalid_message = true;
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return Flow::Next;
                };
                let Some(message) = self.message(script, dialog) else {
                    self.invalid_message = true;
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return Flow::Next;
                };
                if self.quiet {
                    return Flow::Next;
                }
                self.open(idx, entity, ins.offset, window, message, None)
            }
            0x48 => self.ask(script, idx, entity, ins),
            0x49 => {
                self.default_name_menu(ins, class);
                Flow::Next
            }
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
            0x58 | 0x5B => {
                *class = Class::Ignored;
                self.gift_not_given = true;
                Flow::Next
            }
            0x39 | 0x3A => {
                self.change_gil(ins, class);
                Flow::Next
            }
            0x3B => {
                self.read_gil(ins, class);
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
                self.battle_ordinal = self.battle_ordinal.saturating_add(1);
                self.battle_checkpoint = self.battle.map(|formation| BattleCheckpoint {
                    ordinal: self.battle_ordinal, entity: Self::own(entity), instruction_offset: ins.offset as u32,
                    formation, flags: self.battle_flags,
                });
                Flow::Scene(End::Battle)
            },
            0x72 => {
                self.battle_flags = ins.literal("flags").and_then(|n| u16::try_from(n).ok());
                Flow::Next
            },
            0xFF => Flow::Scene(End::GameOver),
            0x74 => {
                // GETPC: the character in a party slot (0xFF for none).
                let slot = ins.literal("party").and_then(|s| u8::try_from(s).ok()).unwrap_or(u8::MAX);
                if self.native_story.is_some() {
                    // The original does nothing for an out-of-range slot. Known empty is
                    // 255; an unknown valid slot cannot authorize an assignment.
                    if slot < 3 {
                        let Some((bank, address)) = Self::dest(ins, "var_character").filter(|(bank, _)|
                            matches!(*bank, 5 | 6) || crate::story::canonical_persistent_bank(*bank).is_some()) else {
                            if self.checked_scratch() { self.invalid_scratch_execution = true; }
                            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                            *class = Class::Unknown;
                            return Flow::Next;
                        };
                        match self.native_story.as_ref().and_then(|native| native.state().native_party())
                            .and_then(|party| party.known_main_slot(slot).ok()).flatten() {
                            Some(who) => self.set_byte(bank, address, who),
                            None => {
                                if self.checked_scratch() { self.invalid_scratch_execution = true; }
                                self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                                *class = Class::Unknown;
                            }
                        }
                    }
                } else if let Some((bank, address)) = Self::dest(ins, "var_character") {
                    let who = self.party_now().get(usize::from(slot)).copied().unwrap_or(u8::MAX);
                    self.set_byte(bank, address, who);
                } else if self.checked_scratch() {
                    self.invalid_scratch_execution = true;
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
            // LBYTE/HBYTE extract a source byte into the stated destination byte. Original
            // OpcodeFuncHbyte shifts the source word; it never increments the destination.
            0x9A | 0x9B => {
                if let Some((bank, address)) = Self::dest(ins, "var") {
                    let value = if op == 0x9A {
                        self.num(ins, "value", Width::U8) as u8
                    } else if let Some(source) = ins.operand("value") {
                        if source.bank == 0 { ((source.value >> 8) & 255) as u8 }
                        else if let Some(scratch) = &self.vars.scratch {
                            let address = (source.value & 255) as u8;
                            // Preserve the selector's width before canonicalizing aliases.
                            // Byte-wide selectors have no high byte, even when it is unknown.
                            match field_wide_bytes(source.bank) {
                                Some(1) => 0,
                                Some(2) if matches!(source.bank, 5 | 6) => scratch.read_masked_u16(source.bank, usize::from(address), 0xFF00)
                                    .map(|value| (value >> 8) as u8).unwrap_or(0),
                                Some(2) if address < 255 => self.var_byte(source.bank, address + 1),
                                Some(2) => { let _ = scratch.read_masked_u16(6, 255, 0); 0 }
                                _ => { let _ = scratch.read_u8(source.bank, usize::from(address)); 0 }
                            }
                        }
                        else if let Some(high) = ((source.value & 255) as u8).checked_add(1) {
                            // The discarded low byte cannot affect this assignment. Audit only
                            // the selected high byte, preserving unknown low-byte knowledge.
                            self.var_byte(source.bank, high)
                        } else {
                            self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                            *class = Class::Unknown;
                            return Flow::Next;
                        }
                    } else {
                        *class = Class::Unknown;
                        return Flow::Next;
                    };
                    self.set_byte(bank, address, value);
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
                // Team/PHS availability is not active membership: Barret remains available after
                // leaving the party at moment36. V3 has no authoritative input for this query yet.
                if op == 0xCC && self.vars.story.supports_extended() && self.native_story.is_none() {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    *class = Class::Unknown;
                    return Flow::Next;
                }
                // IFPRTYQ reads the physical field tuple; legacy IFMEMBQ retains preview behavior.
                let Some(character) = ins.literal("character").and_then(|c| u8::try_from(c).ok()) else {
                    *class = Class::Unknown;
                    return Flow::Next;
                };
                let holds = if op == 0xCB { self.field_party_contains(character) }
                    else if let Some(native) = &self.native_story {
                        let known = native.state().native_party().and_then(|party| party.known_available(character).ok()).flatten();
                        if known.is_none() { self.stats.unresolved = self.stats.unresolved.saturating_add(1); }
                        known
                    } else { Some(self.vars.is_member(character)) };
                match holds {
                    Some(holds) => self.branch(idx, ins, holds),
                    None => { *class = Class::Unknown; Flow::Next }
                }
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
                // Original MOVE selects walk/run from the source MSPED and field scale, not the
                // presentation's distance or bounded duration. FMOVE retains the current animation;
                // CMOVE clip selection remains unresolved here.
                let animation = (op == 0xA8).then(|| {
                    let speed = self.speeds.get(&me).copied().unwrap_or(DEFAULT_MOVE_SPEED);
                    if speed <= 3 * i32::from(script.header().scale) { 1 } else { 2 }
                });
                let how = if op == 0xAD { MoveHow::WalkKeepAnimation } else { MoveHow::Walk };
                self.do_move(idx, me, Goal { x, y, z: None, triangle: None, how, animation, frames: None });
                Flow::Yield
            }
            0xAA => {
                // MOVA: walk to where another entity stands.
                let target = ins.literal("entity").map(clamp_u8).and_then(|e| self.places.get(&e).copied());
                match target {
                    Some(w) => {
                        self.do_move(idx, me, Goal { x: w.x, y: w.y, z: None, triangle: None, how: MoveHow::Walk, animation: None, frames: None });
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
                let steps = clamp_i16(self.num(ins, "steps", Width::S16));
                let Some(frames) = super::jump::jump_frames(steps) else {
                    self.note_unresolved_motion();
                    return Flow::Scene(End::Budget);
                };
                self.do_move(idx, me, Goal { x, y, z: None, triangle, how: MoveHow::Jump, animation: None, frames: Some(frames) });
                Flow::Yield
            }
            0xC2 => {
                let (x, y) = (clamp_i16(self.num(ins, "x", Width::S16)), clamp_i16(self.num(ins, "y", Width::S16)));
                let z = clamp_i16(self.num(ins, "z", Width::S16));
                let triangle = u16::try_from(self.num(ins, "triangle", Width::U16)).ok();
                let from_z = self.places.get(&me).and_then(|w| w.z).unwrap_or(z);
                let frames = (u32::from(from_z.abs_diff(z)) / 4).clamp(MIN_LADDER_FRAMES, MAX_MOVE_FRAMES);
                // LADER sets its literal facing directly, independently of the climb's path.
                if let Some(direction) = ins.literal("direction").map(clamp_u8) {
                    self.do_face(idx, me, field_direction_to_facing(direction), 0);
                }
                self.do_move(idx, me, Goal { x, y, z: Some(z), triangle, how: MoveHow::Ladder, animation: None, frames: Some(frames) });
                Flow::Yield
            }
            0xC3 => {
                // ff7-decomp d4db3de3, field_opcodes.c, OpcodeFuncOfst/OpcodeFuncOfstw:
                // original OFST reads signed words, including its otherwise unused duration
                // for type 0. Keep those reads in the unknown-state audit. The offset is an
                // absolute world-space translation added only when drawing the model.
                let x = clamp_i16(self.num(ins, "x", Width::S16));
                let y = clamp_i16(self.num(ins, "y", Width::S16));
                let z = clamp_i16(self.num(ins, "z", Width::S16));
                let speed = clamp_i16(self.num(ins, "speed", Width::S16));
                let time = self.thread_time(idx);
                let next = [x, y, z];
                if ins.literal("move_type") == Some(1) {
                    let Some(motion) = super::OffsetMotion::new(self.offset_at(me, time), next, speed) else {
                        *class = Class::Ignored;
                        self.note_unresolved_motion();
                        return Flow::Scene(End::Budget);
                    };
                    self.offset_moves.insert(me, (time, motion));
                    self.emit(time, Stage::OffsetMove { entity: me, motion });
                    return Flow::Next;
                }
                if ins.literal("move_type") != Some(0) {
                    // Type 2 needs the original cosine table; zero/negative-duration animated
                    // offsets are outside the bounded arithmetic above. Never guess either.
                    *class = Class::Ignored;
                    self.note_unresolved_motion();
                    return Flow::Scene(End::Budget);
                }
                if self.offset_at(me, time) != next || self.offset_moves.contains_key(&me) { self.epoch = self.epoch.wrapping_add(1); }
                self.offset_moves.remove(&me);
                self.offsets.insert(me, next);
                // Redundant instructions still reach the stage, but must not keep an idle
                // OFST/JMP loop awake by incrementing the scheduler's epoch.
                self.out.push(Timed {
                    at: time.saturating_sub(self.origin),
                    what: Stage::Offset { entity: me, x, y, z },
                });
                Flow::Next
            }
            0xC4 => {
                if self.offset_moves.get(&me).is_some_and(|(start, motion)|
                    self.thread_time(idx) < start.saturating_add(motion.completion_frames())) {
                    if let Some(thread) = self.threads.get_mut(idx) { thread.state = State::WaitOffset(me); }
                    Flow::Yield
                } else { Flow::Next }
            },
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
            0xB4 => {
                let direction = clamp_u8(self.num(ins, "direction", Width::U8));
                let steps = match (ins.literal("turns"), ins.literal("unknown"), ins.literal("speed")) {
                    (Some(0..=2), Some(1 | 2), Some(steps @ 1..=255)) => Some(steps as u32),
                    _ => None,
                };
                self.do_face(idx, me, field_direction_to_facing(direction), steps.unwrap_or(TURN_FRAMES));
                // PSX field scripts run before the turn update: step 0 is sampled on the start
                // frame, and completion at `steps` is observed on the following script tick.
                // This corrects timing only; the client still approximates the path/curve.
                if steps.is_some() {
                    self.add_time(idx, 1);
                }
                Flow::Yield
            }
            0xB5 => {
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
                    let steps = match (ins.literal("speed"), ins.literal("direction")) {
                        (Some(steps @ 1..=255), Some(0..=2)) => Some(steps as u32),
                        _ => None,
                    };
                    if self.face_entity(idx, me, target, steps.unwrap_or(TURN_FRAMES)) && steps.is_some() {
                        // TURA uses the same PSX turn update/completion ordering as TURNGEN.
                        // Only the timing is supported here; rendering still approximates its path/curve.
                        self.add_time(idx, 1);
                    }
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
            0xA3 => {
                self.do_anim(idx, me, ins, AnimHow::Once, true);
                Flow::Yield
            }
            0xAE => {
                self.do_anim(idx, me, ins, AnimHow::Once, false);
                Flow::Next
            }
            // CANIM1 starts asynchronously; CANIM2 waits for the existing clip deadline.
            0xB0 => {
                self.do_anim(idx, me, ins, AnimHow::Once, false);
                Flow::Next
            }
            0xBB => {
                self.do_anim(idx, me, ins, AnimHow::Once, true);
                Flow::Yield
            }
            // ANIM!1 starts asynchronously; ANIM!2 waits for the existing clip deadline.
            0xAF => {
                self.do_anim(idx, me, ins, AnimHow::Hold, false);
                Flow::Next
            }
            0xBA => {
                self.do_anim(idx, me, ins, AnimHow::Hold, true);
                Flow::Yield
            }
            // CANM!1 starts asynchronously; CANM!2 waits for the existing clip deadline.
            0xB1 => {
                self.do_anim(idx, me, ins, AnimHow::Hold, false);
                Flow::Next
            }
            0xBC => {
                self.do_anim(idx, me, ins, AnimHow::Hold, true);
                Flow::Yield
            }
            0xBD => {
                let speed = u32::try_from(self.num(ins, "speed", Width::U16)).unwrap_or(DEFAULT_ANIM_SPEED);
                self.anim_speeds.insert(me, speed.max(1));
                self.default_anims.remove(&me);
                self.standing_defaults.insert(me, None);
                // ASPED never advanced the scheduler epoch. Presentation invalidation must not
                // turn a constant ASPED/JMP loop into active work that evades idle parking.
                self.out.push(Timed {
                    at: self.thread_time(idx).saturating_sub(self.origin),
                    what: Stage::AnimSpeed { entity: me, speed: u16::try_from(speed).unwrap_or(u16::MAX) },
                });
                Flow::Next
            }
            0xAC => {
                if let Some(end) = self.anim_ends.get(&me).copied() {
                    self.wait_until(idx, end);
                }
                Flow::Yield
            }
            // ---- authored fades ---------------------------------------------------------------------------
            0x25 | 0x6B => {
                let time = self.thread_time(idx);
                self.fade.advance(time.saturating_sub(self.fade_at) as f32);
                self.fade_at = time;
                let kind = clamp_u8(ins.literal("type").unwrap_or(0));
                let rgb = ["r", "g", "b"].map(|name| clamp_u8(self.num(ins, name, Width::U8)));
                let fade = if op == 0x25 {
                    FadeOp::Nfade { kind, rgb, frames: self.num(ins, "speed", Width::S16) as i16 }
                } else {
                    FadeOp::Fade { kind, rgb, speed: clamp_u8(ins.literal("speed").unwrap_or(0)), adjust: clamp_u8(ins.literal("adjust").unwrap_or(0)) }
                };
                if self.fade.apply(fade) { self.emit(time, Stage::Fade(fade)); }
                else { *class = Class::Ignored; self.stats.unresolved = self.stats.unresolved.saturating_add(1); }
                Flow::Next
            }
            0x6C => {
                let time = self.thread_time(idx);
                self.fade.advance(time.saturating_sub(self.fade_at) as f32);
                self.fade_at = time;
                if self.fade.complete() { Flow::Next } else {
                    // Recheck each field tick: another script can replace the global fade.
                    // A zero-speed hold remains bounded by the VM's ordinary time/step limits.
                    self.set_pc(idx, ins.offset);
                    self.add_time(idx, 1);
                    if let Some(thread) = self.threads.get_mut(idx) { thread.seen.clear(); }
                    Flow::Yield
                }
            }
            // ---- background visibility ----------------------------------------------------------------------
            0xE0..=0xE4 => {
                let group = clamp_u8(self.num(ins, "group", Width::U8));
                let frame = if op <= 0xE1 { clamp_u8(self.num(ins, "frame", Width::U8)) } else { 0 };
                if group >= 64 || frame >= 8 {
                    *class = Class::Ignored;
                } else {
                    let change = match op {
                        0xE0 => BackgroundOp::On { group, frame },
                        0xE1 => BackgroundOp::Off { group, frame },
                        0xE2 => BackgroundOp::ShiftLeft { group },
                        0xE3 => BackgroundOp::ShiftRight { group },
                        _ => BackgroundOp::Clear { group },
                    };
                    if self.quiet { self.initial_background.push(change); }
                    if self.background_state.apply(change) { self.epoch = self.epoch.wrapping_add(1); }
                    self.out.push(Timed { at: self.thread_time(idx).saturating_sub(self.origin), what: Stage::Background(change) });
                }
                Flow::Next
            }
            // Palette positions address 16-colour blocks of the field's shared scratch buffer.
            // The count operand is inclusive; resolving it here preserves 255 as 256 entries.
            0xE5 | 0xE6 | 0xE7 | 0xEA => {
                let count = u16::from(clamp_u8(self.num(ins, "count", Width::U8))) + 1;
                let byte = |name| clamp_u8(self.num(ins, name, Width::U8));
                let palette = match op {
                    0xE5 => PaletteOp::Store { palette: byte("palette"), position: byte("position"), count },
                    0xE6 => PaletteOp::Load { position: byte("position"), palette: byte("palette"), count },
                    0xE7 => PaletteOp::Copy { source: byte("pos_src"), destination: byte("pos_dst"), count },
                    _ => PaletteOp::Multiply { source: byte("pos_src"), destination: byte("pos_dst"),
                        red: byte("r"), green: byte("g"), blue: byte("b"), count },
                };
                let change = BackgroundOp::Palette(palette);
                if self.quiet { self.initial_background.push(change); }
                self.emit(self.thread_time(idx), Stage::Background(change));
                Flow::Next
            }
            // ---- the camera ----------------------------------------------------------------------------------
            0x5E => {
                // Original field_opcodes.c opcode_SHAKE: low mask bits enable axes;
                // disabled axes do not read their operands or replace saved parameters.
                let Some(mask) = ins.literal("type").and_then(|v| u8::try_from(v).ok()) else {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return Flow::Next;
                };
                let (x_amplitude, x_steps) = if mask & 1 != 0 {
                    (clamp_u8(self.num(ins, "x_amplitude", Width::U8)), clamp_u8(self.num(ins, "x_frames", Width::U8)))
                } else { (0, 0) };
                let (y_amplitude, y_steps) = if mask & 2 != 0 {
                    (clamp_u8(self.num(ins, "y_amplitude", Width::U8)), clamp_u8(self.num(ins, "y_frames", Width::U8)))
                } else { (0, 0) };
                if (mask & 1 != 0 && x_steps == 0) || (mask & 2 != 0 && y_steps == 0) {
                    self.stats.unresolved = self.stats.unresolved.saturating_add(1);
                    return Flow::Next;
                }
                self.emit(self.thread_time(idx), Stage::Shake { mask: mask & 3, x_amplitude, x_steps, y_amplitude, y_steps });
                Flow::Next
            }
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
            0xF1 | 0xF2 | 0xDA => {
                match crate::field_sound::Call::of(ins) {
                    Ok(Some(call)) if call.channel <= 2 => self.emit(self.thread_time(idx),
                        Stage::Sound { channel: call.channel, id: call.id, pan: call.pan }),
                    _ => {
                        // Only reviewed literal effects on channels 1 and 2 are presented. Higher
                        // channels may already belong to field ambience (tin_1's train loop is ch3).
                        // Banked operands and music/volume operations remain skipped, never guessed.
                        *class = Class::Ignored;
                    }
                }
                Flow::Next
            }
            // ---- party slots and their authored physical assignments ----------------------------------------
            0xC8 | 0xC9 => {
                // (an operand that is not a literal is not made up: the op stays unknown)
                match ins.literal("character") {
                    Some(character) => {
                        if !self.change_party(if op == 0xC8 { PartyOp::Add(clamp_u8(character)) } else { PartyOp::Remove(clamp_u8(character)) }) {
                            *class = Class::Unknown;
                        }
                    },
                    None => *class = Class::Unknown,
                }
                Flow::Next
            }
            0xCA => {
                match (ins.literal("character1"), ins.literal("character2"), ins.literal("character3")) {
                    (Some(a), Some(b), Some(c)) => {
                        if !self.change_party(PartyOp::Set([clamp_u8(a), clamp_u8(b), clamp_u8(c)])) { *class = Class::Unknown; }
                    },
                    _ => *class = Class::Unknown,
                }
                Flow::Next
            }
            0xCD..=0xCF if self.native_story.is_some() => {
                let character = ins.literal("character").and_then(|value| u8::try_from(value).ok());
                let operation = character.and_then(|character| match op {
                    0xCD => ins.literal("exists").map(|value| NativePartyOp::Available { character, available: value != 0 }),
                    0xCE => Some(NativePartyOp::Lock { character, locked: true }),
                    0xCF => Some(NativePartyOp::Lock { character, locked: false }),
                    _ => None,
                });
                if !operation.is_some_and(|operation| self.change_native_party(operation)) { *class = Class::Unknown; }
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
                Step::StartMain(entity) | Step::StartAmbientMain(entity) => {
                    let main = script.entity_inits(None).into_iter().find(|init| init.entity == usize::from(entity)).and_then(|init| init.main_start);
                    if let Some(main) = main {
                        if let Some(child) = self.spawn(script, usize::from(entity), 0, 7, time) {
                            if let Some(thread) = self.threads.get_mut(child) {
                                thread.code = Code::Script { pc: main };
                                thread.ambient = matches!(step, Step::StartAmbientMain(_));
                            }
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
        if self.checked_execution_refused() { self.invalid_scratch_execution = true; }
        self.jump = None;
        self.battle = None;
        self.battle_checkpoint = None;
        self.battle_ordinal = 0;
        self.window_checkpoint = None;
        self.window_ordinal = 0;
        self.quiet = false;
        self.threads.clear();
        self.slots.clear();
        let seq = self.next_seq();
        self.threads.push(Thread {
            ambient: false,
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
        self.fade_at = 0;
        self.out.clear();
        self.done = None;
    }

    /// Reconstruct a presentation-only continuation. Never guess an answer, skip another kind of termination,
    /// or pass a different formation. Existing budgets cover both the prelude and the tail.
    pub(super) fn seek_after_battle(&mut self, script: &FieldScript<'_>, formation: u16) -> bool {
        loop {
            let beat = self.resume(script, None);
            // A legacy victory presentation starts after this prefix, but the field still has
            // every group change the prefix made. Hand them to the initial stage once.
            self.initial_background.extend(beat.stage.iter().filter_map(|event| match event.what {
                Stage::Background(op) => Some(op), _ => None,
            }));
            match beat.then {
                Then::Window(w) if w.message.choice.is_none() => {},
                Then::End { how: End::Battle, .. } if self.battle == Some(formation) => {
                    self.done = None;
                    self.battle = None;
                    self.battle_checkpoint = None;
                    self.window_checkpoint = None;
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
        if !self.scratch_valid() { self.stats.setup_incomplete = true; return; }
        if self.refuse_invalid_native_state() { return; }
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
            self.clock = self.clock.max(self.thread_time(idx));
            match self.step_script(script, idx) {
                Flow::Next | Flow::Yield => {}
                Flow::Window(_) | Flow::Scene(_) => {
                    self.stats.setup_incomplete = true;
                    break;
                }
            }
        }
        // Quiet setup cannot carry an unfinished native updater into a reset scene
        // clock. Completed offsets become its initial fixed presentation.
        if self.offsets_running_at(self.clock) { self.stats.setup_incomplete = true; }
        for (&entity, &(start, motion)) in &self.offset_moves {
            self.offsets.insert(entity, motion.at(self.clock.saturating_sub(start).saturating_sub(1)));
        }
        self.offset_moves.clear();
        self.threads.clear();
        self.slots.clear();
        self.out.clear();
        self.quiet = false;
        if self.vars.scratch.as_ref().is_some_and(FieldScratchRun::any_change) {
            self.invalid_scratch_setup = true;
            self.stats.setup_incomplete = true;
        }
    }

    pub(super) fn has_invalid_message(&self) -> bool { self.invalid_message }

    pub(super) fn has_implicit_choice(&self) -> bool { self.implicit_choice }

    pub(super) fn boundary(&self) -> Option<SceneBoundary> {
        if let Some(window) = self.window_checkpoint { return Some(SceneBoundary::Window(window)); }
        if let Some(battle) = self.battle_checkpoint { return Some(SceneBoundary::Battle(battle)); }
        self.done.map(SceneBoundary::End)
    }

    pub(super) fn accept_window(&mut self, answer: Option<u8>) {
        if let Some(p) = self.pending.take() {
            if let (Some((bank, address)), Some(answer)) = (p.var, answer) { self.set_byte(bank, address, answer); }
        }
        self.window_checkpoint = None;
    }

    pub(super) fn resume_battle(&mut self, story: &StoryState, resources: SceneResources) {
        if !self.scratch_valid() { return; }
        self.invalid_native_state |= self.vars.story.supports_native_party() != story.supports_native_party();
        let native = story.supports_native_party().then(|| NativeStoryBuilder::new(story)).transpose();
        self.invalid_native_state |= native.is_err();
        if self.refuse_invalid_native_state() { return; }
        self.native_story = native.ok().flatten();
        // Acknowledged persistent assignments now live in the authoritative afterimage. Scratch
        // selectors (5/6) and every scheduler/actor/RNG detail stay exactly where combat paused.
        self.writes.retain(|(bank, _), _| crate::story::canonical_persistent_bank(*bank).is_none());
        self.written_masks.retain(|(bank, _), _| crate::story::canonical_persistent_bank(*bank).is_none());
        self.vars.story = story.clone();
        // Combat is external progress. Revisiting the same instruction is now meaningful, and
        // parked peers must get a chance to observe authoritative post-battle values.
        self.epoch = self.epoch.wrapping_add(1);
        self.resources.gil_before = resources.gil;
        self.resources.gil_after = resources.gil;
        self.resources.gil_ops.clear();
        self.party_ops.clear();
        if !story.supports_native_party() {
            self.party_main = [0, 1, 2].map(|slot| self.vars.party_member(slot).unwrap_or(255));
            self.party_bank = self.party_main;
        }
        self.done = None;
        self.battle = None;
        self.battle_checkpoint = None;
        self.origin = self.clock;
    }

    fn record_window(&mut self, window: &Window, offset: Option<u32>) {
        self.window_ordinal = self.window_ordinal.saturating_add(1);
        let dialog = match window.message.origin { MessageOrigin::Canon { dialog, .. } => u16::try_from(dialog).ok(), _ => None };
        self.window_checkpoint = Some(WindowCheckpoint {
            ordinal: self.window_ordinal, entity: window.opener, instruction_offset: offset, window: window.id,
            dialog, choices: window.message.choice.as_ref().map(|c| [c.first as u8, c.last as u8]),
        });
    }

    fn finish(&mut self, end: End) -> Beat {
        let end = if !self.scratch_valid() {
            self.battle = None;
            self.battle_checkpoint = None;
            self.battle_flags = None;
            self.jump = None;
            self.window_checkpoint = None;
            self.pending = None;
            self.queued_note = None;
            End::Broken
        } else if end != End::Budget && self.unresolved_offset_boundary(self.clock) {
            // The terminal opcode may have already prepared a battle or transfer.
            // Refusal must expose only Budget, never a resumable battle checkpoint
            // or a destination from a presentation whose outcome was not resolved.
            self.battle = None;
            self.battle_checkpoint = None;
            self.battle_flags = None;
            self.jump = None;
            self.window_checkpoint = None;
            End::Budget
        } else { end };
        self.done = Some(end);
        let at = self.clock.saturating_sub(self.origin);
        Beat { stage: std::mem::take(&mut self.out), then: Then::End { at, how: end } }
    }

    /// Run on to the next window or the end of the scene. `answer` is the player's answer to the window handed out
    /// last, if that asked (the absolute line number of the choice, as a script stores it; anything else is clamped
    /// into the choices, `None` takes the cursor's starting line).
    pub fn resume(&mut self, script: &FieldScript<'_>, answer: Option<usize>) -> Beat {
        if !self.scratch_valid() { return self.finish(End::Broken); }
        if self.refuse_invalid_native_state() {
            return Beat { stage: Vec::new(), then: Then::End { at: self.clock.saturating_sub(self.origin), how: End::Broken } };
        }
        self.window_checkpoint = None;
        if let Some((opener, note)) = self.queued_note.take() {
            let at = self.clock.saturating_sub(self.origin);
            let window = Window { at, opener: Self::own(opener), speaker: None, id: 0, geometry: None, message: note };
            self.origin = self.clock;
            self.record_window(&window, None);
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
            let Some(idx) = self.pick() else {
                // A parked condition or slot owner cannot manufacture a RET. No runnable
                // work is not completion unless all foreground work is demonstrably quiet.
                return self.finish(if self.foreground_finished(script) { End::Finished } else { End::Budget });
            };
            if self.foreground_finished(script) { return self.finish(End::Finished); }
            // A background event may be next in time while the only foreground work left
            // is a proven standing-animation tail. Run background events up to that tail's
            // time, then finish there; no later actor work or waiter may be cut short.
            if let Some(time) = self.threads.iter().enumerate()
                .filter(|(_, t)| !t.ambient && t.state == State::Run && t.time <= self.thread_time(idx))
                .filter(|(i, _)| self.quiescent_animation_tail(script, *i))
                .map(|(_, t)| t.time).min() {
                if time > self.limits.frames || self.steps >= self.limits.steps {
                    return self.finish(End::Budget);
                }
                self.clock = self.clock.max(time);
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
            if !self.scratch_valid() { return self.finish(End::Broken); }
            match flow {
                Flow::Next | Flow::Yield => {}
                Flow::Scene(end) => return self.finish(end),
                Flow::Window(opened) => {
                    if self.unresolved_offset_boundary(opened.time) { return self.finish(End::Budget); }
                    let at = opened.time.saturating_sub(self.origin);
                    let geometry = self.geometry.get(&opened.id).copied();
                    let window = Window { at, opener: Self::own(opened.opener), speaker: None, id: opened.id, geometry, message: opened.message };
                    self.record_window(&window, Some(opened.instruction_offset));
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
/// other sound commands and music, unsupported picture effects, and the field's logic
/// that is about the player (controls, menus, party, talk and solid radii, triggers).
fn is_ignored(op: u8) -> bool {
    matches!(
        op,
        // controls, menus, party and field logic
        0x04..=0x06 | 0x08..=0x0B | 0x21..=0x23 | 0x26 | 0x27 | 0x2A | 0x2B | 0x33 | 0x38 | 0x3B | 0x49 | 0x4A | 0x4B | 0x6D | 0x6E | 0x71..=0x73
        | 0x7E | 0xBF | 0xC5..=0xC7 | 0xCD..=0xCF | 0xD0..=0xD3 | 0xD6 | 0xD7 | 0xD8 | 0xD9 | 0xDB | 0xDD | 0xDE | 0xDC
        // windows other than the one that is shown
        | 0x2E | 0x2F | 0x36 | 0x37 | 0x41..=0x43 | 0x51..=0x55 | 0x56 | 0x57 | 0x45 | 0x47 | 0x4D | 0x4F
        // unsupported picture effects (palette changes and layer scrolls among them)
        | 0x2C | 0x2D | 0x69 | 0x6A | 0x61 | 0x62 | 0x6F | 0xDF | 0xE8 | 0xE9 | 0xEB..=0xEF
        // sound commands other than the supported literal channel plays, and music
        | 0xF0 | 0xF3..=0xF7 | 0xF8..=0xFE
        // items and materia: not given here
        | 0x59 | 0x5A | 0x5C | 0x5D | 0x3A | 0x3C..=0x3F
        // SPECIAL and KAWAI families
        | 0x0F | 0x28 | 0x29
        // an entity's declarations: its model (CHAR) and the playable character it is bound to (PC). A scene's cast names
        // the models it draws, and nothing the VM does reads the binding (`GETAI` and the rest name their entity).
        | 0xA0 | 0xA1
    )
}

