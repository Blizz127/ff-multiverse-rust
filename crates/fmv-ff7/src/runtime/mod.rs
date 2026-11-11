//! The live field runtime: every entity's original scripts, executed the way the field engine executes them.
//!
//! This is the generic engine the scene interpreter ([`crate::script::scene`]) is not. A scene runs a curated set of
//! scripts to a finite end; this runs a whole field forever, frame by frame, the way the original does:
//!
//! * **Initialisation.** On entry, each entity's script 0 runs from its first byte up to its first `RET`, without
//!   yielding (the original executes the init part to completion inside one call). The entity's main part (the
//!   bytes after that `RET`) then becomes its priority-7 script.
//! * **One frame** ([`FieldRuntime::tick`]): first the event requests raised since the last frame (talk, push, line
//!   across/touch) start the matching scripts at priority 1, then every entity in turn executes up to **eight**
//!   instructions of its active script, stopping early when an instruction yields (a wait, a backward jump, a window
//!   that is still open, a movement still under way, a pending global command, ...).
//! * **Priorities.** An entity runs one script at a time, the one at its highest priority (0 is highest, 7 the main
//!   loop). A request at a higher priority than the active one suspends the active script (its PC is kept per
//!   priority) and starts the requested one; `RET` resumes the next suspended lower-priority script. A request at a
//!   lower priority is queued in that priority's slot and starts when everything above it has returned. `REQSW`
//!   waits until the request has been placed, `REQEW` until the requested script has returned.
//! * **Global commands.** A map jump, battle, minigame, menu, movie, game over or disc change is a command the
//!   runtime cannot carry out itself. The instruction that issues it records it ([`FieldRuntime::command`]) and
//!   yields until the host says it is done ([`FieldRuntime::complete_command`]); a battle's instruction then
//!   continues, a map jump's field is replaced by the host.
//! * **Windows.** `MESSAGE` and `ASK` open a window and yield every frame until the host closes it
//!   ([`FieldRuntime::close_window`], [`FieldRuntime::answer`]). Other entities keep running meanwhile, as in the
//!   original.
//!
//! Behaviour is taken from the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor's opcode table) and
//! from reading the PSX decompilation for behaviour; no code is copied from either. Instructions this runtime does
//! not implement yet are counted in [`Coverage`] and skipped (the PC advances past them and the script continues
//! without yielding), so a field always keeps running and the counts say exactly what is still missing.
//!
//! Differences from the original that are deliberate are listed in `docs/FIELD-RUNTIME.md`.

pub mod memory;
mod ops;
pub use ops::{is_implemented, menu, AnimState, MotionState, PartyState, StageState, WindowsState};
#[cfg(test)]
mod owned_tests;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use crate::script::{FieldScript, ScriptError, SCRIPTS_PER_ENTITY};
pub use memory::{Banks, Memory, PERSISTENT_BYTES, SCRATCH_BYTES};

/// Instructions an entity may execute per frame.
pub const OPS_PER_ENTITY_PER_FRAME: u32 = 8;
/// Priority of an entity's main script.
pub const MAIN_PRIORITY: u8 = 7;
/// Priority event requests (talk, push, lines) run at.
pub const EVENT_PRIORITY: u8 = 1;
/// Instructions an entity's init part may execute before the runtime gives up on it (the original has no bound; a
/// real init script never comes near this).
pub const INIT_INSTRUCTION_LIMIT: u32 = 100_000;
/// Window slots.
pub const WINDOWS: usize = 4;
/// The flat-image index of the three field party slots (bank 3/4, addresses 9-11).
pub const PARTY_SLOTS: usize = 0x109;
/// "No character" in a party slot.
pub const NO_CHARACTER: u8 = 0xFF;

/// Script slots an event request starts.
pub mod event_script {
    pub const TALK: u8 = 1;
    pub const PUSH: u8 = 2;
    pub const ACROSS: u8 = 3;
    pub const TOUCH: u8 = 4;
    pub const TOUCH_ON: u8 = 5;
    pub const TOUCH_OFF: u8 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Sync {
    #[default]
    None,
    Waiting,
    Done,
}

/// One script group ("entity") of the field.
#[derive(Debug, Clone)]
struct Entity {
    pc: usize,
    priority: u8,
    /// Suspended or queued PC per priority (0 = nothing there, as no script starts at offset 0).
    saved_pc: [usize; 8],
    sync: [Sync; 8],
    sync_waiter: [u8; 8],
    wait: u16,
    model: Option<usize>,
    line: Option<usize>,
    /// The entry offsets of this entity's 32 scripts.
    entries: [u16; SCRIPTS_PER_ENTITY],
    /// The init part ran into trouble (an undecodable instruction or the init limit): its main script never runs.
    broken: bool,
}

/// How a model is moving under script control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoveMode {
    #[default]
    None,
    Walk,
    Jump,
    Ladder,
    Slide,
}

/// A field model (an entity with `CHAR`).
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub entity: usize,
    /// The model loader's index (`CHAR`'s operand).
    pub char_id: u8,
    /// Position in field units.
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub triangle: u16,
    /// Field direction (0-255).
    pub dir: u8,
    pub visible: bool,
    pub solid_off: bool,
    pub talk_off: bool,
    pub talk_range: u16,
    pub solid_range: u16,
    pub move_speed: u16,
    pub move_mode: MoveMode,
    /// Target of a scripted move.
    pub move_to: Option<(i32, i32)>,
    pub anim: u8,
    pub anim_speed: u16,
    /// The animation a script asked for and how it should play.
    pub anim_status: AnimStatus,
    /// Talk / push requested by the player since the last frame.
    request_talk: bool,
    request_push: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimStatus {
    #[default]
    DefaultLoop,
    ScriptedLoop,
    PlayOnce,
    PlayOnceDone,
    Hold,
    PlayOnceThenReset,
    PlayOnceThenHold,
}

/// A line entity (`LINE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub entity: usize,
    pub a: (i16, i16, i16),
    pub b: (i16, i16, i16),
    pub active: bool,
    /// Whether the player touched the line during the previous frame.
    touching: bool,
    request_talk: bool,
    request_push: bool,
    across: bool,
    touch: bool,
    touch_on: bool,
    touch_off: bool,
}

/// A global command an instruction has issued and is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MapJump { field: u16, x: i16, y: i16, triangle: i16, dir: u8 },
    Battle { formation: u16 },
    Minigame { field: u16, x: i16, y: i16, triangle: i16, dir: u8, game: u8, param: u8 },
    Menu { kind: u8, param: u8 },
    /// `PMVIE` chose `movie`; `MOVIE` plays the chosen one.
    Movie { movie: u16 },
    GameOver,
    DiscChange { disc: u8 },
}

impl Command {
    /// Whether the field stays loaded after the command (a battle, menu or movie returns to the same field).
    pub fn returns_to_field(&self) -> bool {
        matches!(self, Command::Battle { .. } | Command::Menu { .. } | Command::Movie { .. })
    }
}

/// A window's state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Window {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub open: Option<OpenWindow>,
    /// `WMODE`'s style and whether the player may close it.
    pub mode: u8,
    pub prevent_close: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub entity: usize,
    pub dialog: u8,
    pub ask: Option<Ask>,
    /// The host has closed it; the opening instruction completes on its next visit.
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask {
    pub first: u8,
    pub last: u8,
    /// Where the answer is written (bank selector, address).
    pub var: (u8, u8),
    pub answer: Option<u8>,
}

/// What the host should know about, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A window opened. `ask` is `Some` for a question.
    Window { window: u8, entity: usize, dialog: u8, ask: Option<(u8, u8)> },
    /// The window an instruction opened was dismissed and its instruction completed.
    WindowDone { window: u8 },
    /// A window was closed by a script (`WCLSE`).
    WindowClosedByScript { window: u8 },
    /// A command was issued (see [`FieldRuntime::command`]).
    Command(Command),
    /// Music, sound and other presentation the host may play: the opcode's mnemonic and its raw operand bytes.
    Presentation { entity: usize, mnemonic: &'static str, bytes: Vec<u8> },
    /// A script changed the party (`PRTYP`, `PRTYM`, `PRTYE`, `SPTYE`).
    PartyChanged { party: [u8; 3] },
    /// Gil changed by the signed amount.
    Gil { delta: i64 },
    /// An item was added (positive) or removed.
    Item { item: u16, delta: i32 },
    /// Materia was given.
    Materia { materia: u8, ap: u32 },
    /// The player's control was locked or released (`UC`).
    Control { locked: bool },
}

/// Per-opcode execution counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Instructions executed with full support.
    pub executed: BTreeMap<&'static str, u64>,
    /// Instructions skipped because the runtime does not implement them yet.
    pub unsupported: BTreeMap<&'static str, u64>,
    /// Instructions that could not be decoded (the entity stops).
    pub undecodable: u64,
    /// Operands the original would read from undefined memory (division by zero, an out-of-range table index...).
    pub anomalies: u64,
}

impl Coverage {
    pub fn merge(&mut self, other: &Coverage) {
        for (k, v) in &other.executed {
            *self.executed.entry(k).or_default() += v;
        }
        for (k, v) in &other.unsupported {
            *self.unsupported.entry(k).or_default() += v;
        }
        self.undecodable += other.undecodable;
        self.anomalies += other.anomalies;
    }
}

/// What the player is doing this frame (the leader's avatar, in field units).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerInput {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub triangle: u16,
    pub dir: u8,
    /// The OK button went down this frame.
    pub ok: bool,
    /// Buttons held (original pad bit layout), and those that went down this frame.
    pub keys_held: u16,
    pub keys_pressed: u16,
}

/// Field-wide state the scripts switch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldState {
    /// `UC`: the player cannot move.
    pub character_lock: bool,
    pub menu_disabled: bool,
    pub battles_disabled: bool,
    pub map_jumps_disabled: bool,
    /// `BTLMD`'s battle mode flags.
    pub battle_mode: u16,
    /// `BTLTB`'s encounter table.
    pub encounter_table: u8,
    /// The model the player controls (`PC` of party slot 0, or `CC`).
    pub pc_model: Option<usize>,
    /// `PMVIE`'s chosen movie.
    pub movie: Option<u16>,
    /// `MPNAM`'s map name dialog.
    pub map_name: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandState {
    Idle,
    Pending(Command),
    Done(Command),
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Script(#[from] ScriptError),
    #[error("the field has no entities")]
    Empty,
}

/// One field, running.
#[derive(Debug, Clone)]
pub struct FieldRuntime {
    code: Vec<u8>,
    names: Vec<String>,
    scale: u16,
    pub mem: Memory,
    ents: Vec<Entity>,
    models: Vec<Model>,
    lines: Vec<Line>,
    /// `PC`'s character → entity.
    char_entity: [Option<usize>; 9],
    pub state: FieldState,
    windows: [Window; WINDOWS],
    events: Vec<Event>,
    command: CommandState,
    current: usize,
    frame: u64,
    rand_index: u8,
    rand_step: u8,
    coverage: Coverage,
    player: Option<PlayerInput>,
    previous_player: Option<PlayerInput>,
    /// The field's walkmesh, when the host supplied one (movement needs it; without it moves go straight).
    walkmesh: Option<std::sync::Arc<crate::field::Walkmesh>>,
    /// Instruction families' own state.
    pub(crate) motion: MotionState,
    pub(crate) anim: AnimState,
    pub(crate) stage: StageState,
    pub(crate) windows_ext: WindowsState,
    pub(crate) party_ext: PartyState,
    /// The save's gil, when the host knows it (`GOLDu`/`GOLDd` change it; `CHGLD` reads it).
    pub gil: Option<u32>,
}


impl FieldRuntime {
    /// A runtime over a field's script section with the given memory (persistent banks from the save; scratch is
    /// cleared, as on every field entry). Nothing runs until [`FieldRuntime::init`].
    pub fn new(section: &[u8], banks: Banks) -> Result<Self, RuntimeError> {
        let script = FieldScript::parse(section)?;
        let layout = script.layout();
        if layout.entities.is_empty() {
            return Err(RuntimeError::Empty);
        }
        let scale = layout.header.scale;
        let ents = layout
            .entities
            .iter()
            .map(|e| Entity {
                pc: usize::from(e.entries[0]),
                priority: MAIN_PRIORITY,
                saved_pc: [0; 8],
                sync: [Sync::None; 8],
                sync_waiter: [u8::MAX; 8],
                wait: 0,
                model: None,
                line: None,
                entries: e.entries,
                broken: false,
            })
            .collect();
        let mem = Memory { banks, scratch: [0; SCRATCH_BYTES] };
        Ok(FieldRuntime {
            code: section.to_vec(),
            names: layout.entities.iter().map(|e| e.name.clone()).collect(),
            scale,
            mem,
            ents,
            models: Vec::new(),
            lines: Vec::new(),
            char_entity: [None; 9],
            state: FieldState::default(),
            windows: Default::default(),
            events: Vec::new(),
            command: CommandState::Idle,
            current: 0,
            frame: 0,
            rand_index: 0,
            rand_step: 1,
            coverage: Coverage::default(),
            player: None,
            previous_player: None,
            walkmesh: None,
            motion: MotionState::default(),
            anim: AnimState::default(),
            stage: StageState::default(),
            windows_ext: WindowsState::default(),
            party_ext: PartyState::default(),
            gil: None,
        })
    }

    /// Run every entity's init part to its first `RET` (in entity order, as the original), leaving each entity at
    /// the start of its main part. Yields are ignored here, as in the original: the instruction is simply executed
    /// again until the init part reaches its `RET`.
    pub fn init(&mut self) {
        // The original locks the PHS and save menus on every field entry (bank 1, address 31, bits 0-1).
        let locks = self.mem.banks.get(31) | 3;
        self.mem.banks.set(31, locks);
        for e in 0..self.ents.len() {
            self.current = e;
            let mut budget = INIT_INSTRUCTION_LIMIT;
            loop {
                let pc = self.ents[e].pc;
                match self.code.get(pc) {
                    Some(0) => {
                        self.ents[e].pc = pc + 1;
                        break;
                    }
                    Some(_) if budget > 0 => {
                        budget -= 1;
                        if self.step(e) == Flow::Broken {
                            break;
                        }
                    }
                    _ => {
                        self.ents[e].broken = true;
                        break;
                    }
                }
            }
        }
        self.current = 0;
    }

    /// Advance one field frame (1/30 s). `player` is where the player's character is and what they pressed;
    /// `None` when no one controls it (a cut-scene with nobody connected, a headless survey).
    pub fn tick(&mut self, player: Option<PlayerInput>) {
        self.previous_player = self.player;
        self.player = player;
        self.frame += 1;
        self.detect_player_events();
        self.dispatch_event_requests();
        let count = self.ents.len();
        for _ in 0..count {
            if self.current >= count {
                self.current = 0;
            }
            let e = self.current;
            if !self.ents[e].broken {
                for _ in 0..OPS_PER_ENTITY_PER_FRAME {
                    if self.step(e) != Flow::Continue {
                        break;
                    }
                }
            }
            self.current += 1;
        }
        self.update_families();
    }

    /// Execute the instruction at entity `e`'s PC.
    fn step(&mut self, e: usize) -> Flow {
        self.current = e;
        let pc = self.ents[e].pc;
        let Some(&op) = self.code.get(pc) else {
            self.coverage.undecodable += 1;
            self.ents[e].broken = true;
            return Flow::Broken;
        };
        let flow = self.exec(op);
        if flow == Flow::Broken {
            self.ents[e].broken = true;
        }
        flow
    }

    // ---- host interface ------------------------------------------------------------------------------------

    /// Give the runtime the field's walkmesh (scripted movement follows it).
    pub fn set_walkmesh(&mut self, walkmesh: std::sync::Arc<crate::field::Walkmesh>) {
        self.walkmesh = Some(walkmesh);
    }

    pub fn walkmesh(&self) -> Option<&crate::field::Walkmesh> {
        self.walkmesh.as_deref()
    }

    /// The field's scale (512 = 1.0).
    pub fn scale(&self) -> u16 {
        self.scale
    }

    /// The frames run so far.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub fn entity_count(&self) -> usize {
        self.ents.len()
    }

    pub fn entity_name(&self, e: usize) -> Option<&str> {
        self.names.get(e).map(String::as_str)
    }

    /// Entity `e`'s active priority and PC.
    pub fn entity_state(&self, e: usize) -> Option<(u8, usize)> {
        self.ents.get(e).map(|x| (x.priority, x.pc))
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn windows(&self) -> &[Window; WINDOWS] {
        &self.windows
    }

    /// Whether any window is open (the player cannot move while one is).
    pub fn window_open(&self) -> bool {
        self.windows.iter().any(|w| w.open.as_ref().is_some_and(|o| !o.closed))
    }

    /// Events since the last call, in order.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The command an instruction is waiting on, if any.
    pub fn command(&self) -> Option<&Command> {
        match &self.command {
            CommandState::Pending(c) => Some(c),
            _ => None,
        }
    }

    /// The host carried out the pending command (a battle won, a menu closed, a movie finished). The issuing
    /// instruction completes on its next visit.
    pub fn complete_command(&mut self) {
        if let CommandState::Pending(c) = std::mem::replace(&mut self.command, CommandState::Idle) {
            self.command = CommandState::Done(c);
        }
    }

    /// The player dismissed window `window` (a message).
    pub fn close_window(&mut self, window: u8) {
        if let Some(open) = self.windows.get_mut(usize::from(window)).and_then(|w| w.open.as_mut()) {
            open.closed = true;
        }
    }

    /// The player answered the question in window `window` with line `choice` (counted from the question's first
    /// choice line, as the script reads it). An answer outside the question's lines is refused (`false`).
    pub fn answer(&mut self, window: u8, choice: u8) -> bool {
        let Some(open) = self.windows.get_mut(usize::from(window)).and_then(|w| w.open.as_mut()) else {
            return false;
        };
        let Some(ask) = open.ask.as_mut() else {
            return false;
        };
        if choice > ask.last.saturating_sub(ask.first) {
            return false;
        }
        ask.answer = Some(choice);
        open.closed = true;
        true
    }

    /// The player's position is only the host's to set while the player has control; a script moving the player's
    /// model moves it here, and the host should follow ([`FieldRuntime::pc_model`]).
    pub fn pc_model(&self) -> Option<&Model> {
        self.state.pc_model.and_then(|m| self.models.get(m))
    }

    /// The entity a model belongs to and its talk script, for a host that resolves talking itself.
    pub fn request_talk(&mut self, model: usize) {
        if let Some(m) = self.models.get_mut(model) {
            m.request_talk = true;
        }
    }

    pub fn request_push(&mut self, model: usize) {
        if let Some(m) = self.models.get_mut(model) {
            m.request_push = true;
        }
    }

    // ---- event requests ------------------------------------------------------------------------------------

    /// Find what the player touched, crossed or talked to this frame, by the original's rules:
    ///
    /// * **talk** (OK went down): among the models other than the player's whose talk is on, at a different x/y
    ///   and within 256 units of height, the one whose direction from the player differs least from the player's
    ///   facing, if that difference is under a quarter turn and its distance is under its talk range plus the
    ///   player's solid range. Visibility does not matter, as in the original.
    /// * **push** (the player moved): every solid model within 128 units of height whose centre is closer than
    ///   half the two solid ranges.
    /// * **lines**: a line is touched while the player's nearest point on it (projected inside the segment) is
    ///   closer than the player's solid range; crossing it (the side changes between the previous and the current
    ///   position) while touching is "across"; facing it within an eighth of a turn and pressing OK is talk.
    fn detect_player_events(&mut self) {
        let Some(p) = self.player else { return };
        let pc_model = self.state.pc_model;
        if !self.state.character_lock {
            if let Some(m) = pc_model.and_then(|m| self.models.get_mut(m)) {
                m.x = p.x;
                m.y = p.y;
                m.z = p.z;
                m.triangle = p.triangle;
                m.dir = p.dir;
            }
        }
        let solid = i64::from(self.player_solid_range());
        let prev = self.previous_player.unwrap_or(p);
        let moved = (prev.x, prev.y) != (p.x, p.y);
        let here = (i64::from(p.x), i64::from(p.y), i64::from(p.z));

        if p.ok && !self.state.character_lock {
            let mut best: Option<(i64, usize)> = None;
            for (i, m) in self.models.iter().enumerate() {
                if Some(i) == pc_model || m.talk_off {
                    continue;
                }
                let (dx, dy, dz) = (i64::from(m.x) - here.0, i64::from(m.y) - here.1, here.2 - i64::from(m.z));
                if (dx == 0 && dy == 0) || dz <= -256 || dz >= 256 {
                    continue;
                }
                let distance = ((dx * dx + dy * dy) as f64).sqrt() as i64;
                if distance >= i64::from(m.talk_range) + solid {
                    continue;
                }
                let mut diff = i64::from(p.dir.wrapping_sub(direction_of(dx, dy)));
                if diff > 128 {
                    diff = 256 - diff;
                }
                if diff < 64 && best.is_none_or(|(b, _)| diff < b) {
                    best = Some((diff, i));
                }
            }
            if let Some((_, i)) = best {
                self.models[i].request_talk = true;
            }
        }
        if moved {
            for (i, m) in self.models.iter_mut().enumerate() {
                if Some(i) == pc_model || m.solid_off {
                    continue;
                }
                let dz = i64::from(m.z) - here.2;
                if dz <= -127 || dz >= 128 {
                    continue;
                }
                let (dx, dy) = (i64::from(m.x) - here.0, i64::from(m.y) - here.1);
                let threshold = (solid + i64::from(m.solid_range)) / 2;
                if dx * dx + dy * dy < threshold * threshold {
                    m.request_push = true;
                }
            }
        }
        let from = (i64::from(prev.x), i64::from(prev.y), i64::from(prev.z));
        let to = here;
        let move_dir = if moved { direction_of(to.0 - from.0, to.1 - from.1) } else { p.dir };
        for l in &mut self.lines {
            if !l.active {
                continue;
            }
            let a = (i64::from(l.a.0), i64::from(l.a.1), i64::from(l.a.2));
            let b = (i64::from(l.b.0), i64::from(l.b.1), i64::from(l.b.2));
            let probe = if moved { from } else { to };
            match nearest_on_line(a, b, probe) {
                Some((d2, nearest)) if d2 < solid * solid => {
                    if !l.touching {
                        l.touch_on = true;
                    }
                    l.touching = true;
                    l.touch = true;
                    let on_line = (probe.0, probe.1) == (nearest.0, nearest.1);
                    let toward = if on_line { 0 } else { i64::from(direction_of(nearest.0 - probe.0, nearest.1 - probe.1)) };
                    if moved {
                        let side = |q: (i64, i64, i64)| (b.0 - a.0) * (q.1 - a.1) - (q.0 - a.0) * (b.1 - a.1);
                        let (sf, st) = (side(from), side(to));
                        if (sf >= 0 && st < 0) || (st >= 0 && sf < 0) || (sf > 0 && st <= 0) || (st > 0 && sf <= 0) {
                            l.across = true;
                        }
                        if on_line || (toward - i64::from(move_dir) + 64).rem_euclid(256) < 128 {
                            l.request_push = true;
                        }
                    } else if p.ok && !self.state.character_lock && (on_line || (toward - i64::from(p.dir) + 32).rem_euclid(256) < 64) {
                        l.request_talk = true;
                    }
                }
                _ => {
                    if l.touching {
                        l.touch_off = true;
                    }
                    l.touching = false;
                    l.touch = false;
                }
            }
        }
    }

    /// The player model's solid range: the original gives the player 34 units at scale 512.
    fn player_solid_range(&self) -> u16 {
        (34 * u32::from(self.scale) / 512) as u16
    }

    /// Start the scripts the requests raised since the last frame ask for.
    fn dispatch_event_requests(&mut self) {
        let mut talked = false;
        for i in 0..self.models.len() {
            let (entity, talk, push) = {
                let m = &mut self.models[i];
                let r = (m.entity, m.request_talk, m.request_push);
                m.request_talk = false;
                m.request_push = false;
                r
            };
            if talk && !self.state.character_lock && !talked {
                self.request_run(entity, EVENT_PRIORITY, event_script::TALK);
                talked = true;
            }
            if push {
                self.request_run(entity, EVENT_PRIORITY, event_script::PUSH);
            }
        }
        for i in 0..self.lines.len() {
            let l = self.lines[i].clone();
            let flags = &mut self.lines[i];
            flags.request_talk = false;
            flags.request_push = false;
            flags.across = false;
            flags.touch_on = false;
            flags.touch_off = false;
            if l.request_talk && !self.state.character_lock {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::TALK);
            }
            if l.request_push {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::PUSH);
            }
            if l.across {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::ACROSS);
            }
            if l.touch_on {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::TOUCH_ON);
            }
            if l.touch_off {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::TOUCH_OFF);
            }
            if l.touch {
                self.request_run(l.entity, EVENT_PRIORITY, event_script::TOUCH);
            }
        }
    }

    /// Start script `script` of `entity` at `priority` if the entity is running something of lower priority (the
    /// engine's own event request, not a script's `REQ`). Returns whether the request was taken.
    fn request_run(&mut self, entity: usize, priority: u8, script: u8) -> bool {
        let Some(ent) = self.ents.get(entity) else { return false };
        if ent.priority <= priority || ent.sync[usize::from(priority)] != Sync::None {
            return false;
        }
        let offset = usize::from(ent.entries[usize::from(script)]);
        if self.code.get(offset).copied().unwrap_or(0) == 0 {
            return true; // an empty script: nothing to run
        }
        let ent = &mut self.ents[entity];
        let active = usize::from(ent.priority);
        ent.saved_pc[active] = ent.pc;
        ent.pc = offset;
        ent.priority = priority;
        ent.wait = 0;
        if let Some(m) = ent.model {
            if let Some(model) = self.models.get_mut(m) {
                if model.move_mode == MoveMode::Walk {
                    model.anim = 0;
                }
                model.move_mode = MoveMode::None;
                model.move_to = None;
            }
        }
        true
    }

    // ---- operand access ------------------------------------------------------------------------------------

    fn pc(&self) -> usize {
        self.ents[self.current].pc
    }

    /// Byte `i` of the current instruction (0 is the opcode).
    fn p8(&self, i: usize) -> u8 {
        self.code.get(self.pc() + i).copied().unwrap_or(0)
    }

    fn p16(&self, i: usize) -> u16 {
        u16::from_le_bytes([self.p8(i), self.p8(i + 1)])
    }

    /// The bank selector nibble `n` (1 = high nibble of byte 1, 2 = low nibble of byte 1, 3 = high of byte 2...).
    fn nib(&self, n: usize) -> u8 {
        let byte = self.p8(n.div_ceil(2));
        if n % 2 == 1 { byte >> 4 } else { byte & 0xF }
    }

    /// The byte operand at instruction byte `at`, governed by nibble `n`: the literal, or the variable it names.
    fn rd8(&self, n: usize, at: usize) -> u8 {
        match self.nib(n) {
            0 => self.p8(at),
            s => self.mem.read8(s, self.p8(at)),
        }
    }

    fn wr8(&mut self, n: usize, at: usize, value: u8) {
        let s = self.nib(n);
        if s != 0 {
            let a = self.p8(at);
            self.mem.write8(s, a, value);
        }
    }

    /// The 16-bit operand at instruction byte `at` (two literal bytes, or one address byte), governed by nibble `n`.
    fn rd16(&self, n: usize, at: usize) -> i16 {
        match self.nib(n) {
            0 => self.p16(at) as i16,
            s => self.mem.read16(s, self.p8(at)),
        }
    }

    fn wr16(&mut self, n: usize, at: usize, value: i16) {
        let s = self.nib(n);
        if s != 0 {
            let a = self.p8(at);
            self.mem.write16(s, a, value);
        }
    }

    fn advance(&mut self, by: usize) {
        let e = self.current;
        self.ents[e].pc += by;
    }

    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }

    /// The model of the current entity.
    fn own_model(&mut self) -> Option<&mut Model> {
        let m = self.ents[self.current].model?;
        self.models.get_mut(m)
    }

    /// The model of entity `e`.
    fn model_of(&self, e: usize) -> Option<usize> {
        self.ents.get(e).and_then(|x| x.model)
    }

    /// The field party.
    pub fn party(&self) -> [u8; 3] {
        [self.mem.banks.get(PARTY_SLOTS), self.mem.banks.get(PARTY_SLOTS + 1), self.mem.banks.get(PARTY_SLOTS + 2)]
    }

    fn set_party(&mut self, party: [u8; 3]) {
        for (i, c) in party.iter().enumerate() {
            self.mem.banks.set(PARTY_SLOTS + i, *c);
        }
        self.emit(Event::PartyChanged { party });
    }
}

/// What an instruction did to its entity's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Go on with the next instruction this frame.
    Continue,
    /// Stop this entity until the next frame.
    Yield,
    /// The entity cannot go on (an undecodable instruction).
    Broken,
}

/// The field direction (0-255) of the vector `(dx, dy)`, as the original measures it for talk and lines: 64 along
/// +x, 128 along +y, 192 along -x, 0 along -y (the original's angle plus a quarter turn). This is the same convention
/// as [`crate::field_direction_to_facing`]'s, where direction 64 faces heading 0 (+x).
pub fn direction_of(dx: i64, dy: i64) -> u8 {
    if dx == 0 && dy == 0 {
        return 0;
    }
    let turns = (dy as f64).atan2(dx as f64) / std::f64::consts::TAU;
    ((turns * 256.0).round() as i64 + 64).rem_euclid(256) as u8
}

/// The squared distance from `p` to its nearest point on line `a`-`b` and that point, when the projection falls
/// within the segment's extent in both x and y (the original's test); `None` otherwise.
fn nearest_on_line(a: (i64, i64, i64), b: (i64, i64, i64), p: (i64, i64, i64)) -> Option<(i64, (i64, i64, i64))> {
    let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
    let len2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
    if len2 == 0 {
        return None;
    }
    // Position along the line in 1/256ths, as the original computes it.
    let t = -256 * ((a.0 - p.0) * d.0 + (a.1 - p.1) * d.1 + (a.2 - p.2) * d.2) / len2;
    let n = (((t * d.0) >> 8) + a.0, ((t * d.1) >> 8) + a.1, ((t * d.2) >> 8) + a.2);
    let within = |lo: i64, hi: i64, v: i64| (lo - v >= 0 && hi - v <= 0) || (lo - v <= 0 && hi - v >= 0);
    if within(a.0, b.0, n.0) && within(a.1, b.1, n.1) {
        let (ex, ey, ez) = (n.0 - p.0, n.1 - p.1, n.2 - p.2);
        Some((ex * ex + ey * ey + ez * ez, n))
    } else {
        None
    }
}
