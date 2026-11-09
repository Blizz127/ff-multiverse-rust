//! The interpreter: a bounded stack machine over one script.
//!
//! # The machine
//!
//! A script pushes typed entries on a stack and operators pop them. An entry is
//!
//! * a **number** with a size (0 bit, 1 byte, 2 word, 3 dword);
//! * an **address** to store to (`PushAddr`), possibly with the actors it applies to;
//! * a **field** of an actor (a `PushValue` of an address from `0x4000` up), waiting for the mask of actors
//!   to read it from; and
//! * a **set**: one value per actor (what `MASK` makes of a field and a mask of actors).
//!
//! Binary operators take the entry pushed first as their left side (`a - b` is "push a, push b, `SUB`").
//! A comparison of a set with a number (or of two sets) is a *mask of the actors for which it holds*, a
//! number of size 2; a comparison of two plain numbers is 1 or 0. A jump or a logical operator on a set
//! looks at its first value ("if a set is popped where any type is accepted only the first value is
//! considered", the wiki says).
//!
//! # Memory
//!
//! | addresses | what | here |
//! |---|---|---|
//! | `0x0000..=0x03FF` | the actor's own variables (they last through the battle) | [`Memory::local`], 128 bytes |
//! | `0x2000..=0x21DF` | the battle's globals | the masks of [`Masks`] and the target mask `0x2070`, which a script writes |
//! | `0x4000..=0x433F` | per-actor fields (statuses, row, level, HP, MP, ...) | whatever [`World::actor_field`] knows (among them who hit an enemy last: `0x40D0`, `0x40E0`, `0x40F0`) |
//!
//! Addresses count **bits**: the byte is `address / 8` and a bit-sized access uses bit `address % 8`.
//! Everything the machine does not model ends the run with an error ([`AiError`]), never a guess; the
//! caller then falls back to the plain behaviour. The step budget is [`MAX_STEPS`] instructions and the
//! stack holds at most [`MAX_STACK`] entries.
//!
//! # Randomness
//!
//! `RANDOM` is one `next_u16()` draw; `RANDOM_BIT` draws `below(n)` when the mask has `n >= 2` bits (none
//! for 0 or 1). Both use the battle's [`Rng`], the only source, so a replay agrees.
//!
//! # The action queue
//!
//! `ACT` with command `0x20` ("enemy attacks") queues one attack with the target mask (`0x2070`) of that
//! moment, and a script may do it several times (a "double attack" is two `ACT`s). The original keeps one
//! priority queue for players and enemies alike: "When a command is selected either by a player character or an
//! enemy the action is inserted into a priority queue. Up to 64 actions can be queued. On each update loop,
//! the main Queue function will pop off the next action with the lowest priority" and executes "in FIFO order
//! within priority bands" (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Mechanics>, "Action queue"), each
//! entry with its own "Action target mask". So the actions of one run are carried out one after the other, in
//! the order queued, each on the mask it was queued with ([`Outcome::actions`]; the engine's side of it is in
//! [`crate::ai`]). `0x24` (pause for a text) does nothing here; anything else (`0x22`, run another script) is
//! unsupported.
//!
//! # Known approximations
//!
//! * A division or remainder by zero ends the run with an error. Nothing documents it (the Qhimm wiki's
//!   Battle Script page, the source of the opcodes, never mentions it) and no script of the owner's data needs
//!   it: every divisor is a constant or a local that the enemy's Initialize script sets; the cases that did
//!   reach it ran Main on the zeroed memory of an Initialize that had failed, which the engine never does.
//! * Results are cut to the larger of their operands' sizes ("the highest pop's size is used").
//! * Storing a number to a bit address stores whether it is non-zero.
//! * A set with no value in it (a mask of no actor applied to a field) has no first value to test: popping it
//!   where a number is wanted ends the run with an error.

use std::collections::BTreeMap;

use crate::rng::Rng;

use super::script::{self, op, DecodeError, EnemyScripts, Insn};

/// Instructions one run may execute.
pub const MAX_STEPS: usize = 4096;
/// Enemy attacks one run may queue (the original's action queue holds 64 for the whole battle; measured on
/// the owner's `scene.bin`, no Main run of any enemy queues more than four).
pub const MAX_ACTIONS: usize = 16;
/// Entries the stack may hold.
pub const MAX_STACK: usize = 48;
/// Bytes of an actor's own variables (addresses `0x0000..=0x03FF` count bits).
pub const LOCAL_BYTES: usize = 128;
/// Address of the mask of the current action's targets.
pub const ADDR_TARGETS: u16 = 0x2070;
/// Address of the idle animation id of an actor (script-private here: the battle has no animations).
pub const ADDR_IDLE_ANIMATION: u16 = 0x4080;
/// Address of the damaged animation id of an actor (script-private here).
pub const ADDR_DAMAGED_ANIMATION: u16 = 0x4088;
/// Address of the "Immune to Death" flag of an actor (a bit). Every script of the owner's data only ever
/// *sets* it, for its own actor (an Initialize script makes its enemy immune; a Death counter shields the
/// dying one from being killed again): script-private here, the engine has no Death status for it to guard
/// against.
pub const ADDR_IMMUNE_DEATH: u16 = 0x402C;
/// Address of the physical defense rating.
pub const ADDR_DEFENSE: u16 = 0x4100;
/// Address of the magical defense rating.
pub const ADDR_MAGIC_DEFENSE: u16 = 0x4110;
/// Address of the physical attack strength (a byte).
pub const ADDR_ATTACK: u16 = 0x4068;
/// Address of the magical attack strength (a byte).
pub const ADDR_MAGIC_ATTACK: u16 = 0x4070;
/// Address of the physical evasion (a byte).
pub const ADDR_EVADE: u16 = 0x4078;
/// First actor-specific address.
pub const FIRST_FIELD: u16 = 0x4000;
/// The action command that performs an enemy attack.
pub const COMMAND_ENEMY_ATTACK: u32 = 0x20;
/// The action command that pauses the battle while a text is shown.
pub const COMMAND_PAUSE_FOR_TEXT: u32 = 0x24;

/// Everything that can end a run without an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AiError {
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
    #[error("the script has no such slot")]
    NoScript,
    #[error("step budget exhausted")]
    Budget,
    #[error("stack underflow")]
    StackUnderflow,
    #[error("stack overflow")]
    StackOverflow,
    #[error("operands of the wrong kind for opcode {0:#04x}")]
    Type(u8),
    #[error("opcode {0:#04x} is not supported")]
    Unsupported(u8),
    #[error("address {0:#06x} is not modelled")]
    Address(u16),
    #[error("jump target {0:#05x} is outside the block")]
    BadJump(u16),
    #[error("division by zero")]
    DivideByZero,
    #[error("opcode {0:#04x} popped an empty set")]
    EmptySet(u8),
    #[error("the script queues more than {MAX_ACTIONS} actions")]
    TooManyActions,
    #[error("attack {0:#06x} is not known to the battle")]
    Attack(u16),
}

/// A byte image of some memory with a "known" bit mask beside it: reading what is not known fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    value: Vec<u8>,
    known: Vec<u8>,
}

impl Image {
    pub fn new(len: usize) -> Self {
        Self {
            value: vec![0; len],
            known: vec![0; len],
        }
    }

    /// Mark the `bits` (a mask) of `byte` as known with their value from `value`.
    pub fn put_bits(&mut self, byte: usize, bits: u8, value: u8) {
        if let (Some(v), Some(k)) = (self.value.get_mut(byte), self.known.get_mut(byte)) {
            *v = (*v & !bits) | (value & bits);
            *k |= bits;
        }
    }

    /// Put whole bytes (little-endian number of `n` bytes) at `byte`, all known.
    pub fn put_le(&mut self, byte: usize, n: usize, value: u32) {
        for i in 0..n {
            self.put_bits(byte + i, 0xFF, (value >> (8 * i)) as u8);
        }
    }

    /// Read `size` (0 bit, 1 byte, 2 word, 3 dword) at the bit address `bit` (relative to the image).
    /// `None` if any bit of it is not known or it is out of range.
    pub fn read(&self, bit: usize, size: u8) -> Option<u32> {
        let byte = bit >> 3;
        if size == 0 {
            let pos = bit & 7;
            let known = *self.known.get(byte)?;
            if known >> pos & 1 == 0 {
                return None;
            }
            return Some(u32::from(*self.value.get(byte)? >> pos & 1));
        }
        let n = bytes_of(size);
        let mut out = 0u32;
        for i in 0..n {
            if *self.known.get(byte + i)? != 0xFF {
                return None;
            }
            out |= u32::from(*self.value.get(byte + i)?) << (8 * i);
        }
        Some(out)
    }
}

/// Bytes a size class takes in memory.
fn bytes_of(size: u8) -> usize {
    match size {
        0 | 1 => 1,
        2 => 2,
        _ => 4,
    }
}

/// Cut `v` to what a size class can hold.
fn trunc(v: u32, size: u8) -> u32 {
    match size {
        0 => v & 1,
        1 => v & 0xFF,
        2 => v & 0xFFFF,
        _ => v,
    }
}

/// The actor masks behind the global addresses; bit `n` is actor slot `n`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Masks {
    /// `0x2050`: the actors that can act.
    pub active: u16,
    /// `0x2060`: the actor that owns the running script.
    pub me: u16,
    /// `0x2080`: the actors it counts as allies (itself included).
    pub allies: u16,
    /// `0x2090`: the active ones among them.
    pub active_allies: u16,
    /// `0x20A0`: the actors it counts as enemies.
    pub enemies: u16,
    /// `0x20B0`: the active ones among them.
    pub active_enemies: u16,
    /// `0x20C0`: the active player characters.
    pub players: u16,
    /// `0x20E0`: every actor of the battle.
    pub all: u16,
}

impl Masks {
    /// The globals as an image over addresses `0x2000..` (the target mask `0x2070` is not included).
    fn image(&self) -> Image {
        let mut img = Image::new(0x40);
        for (addr, value) in [
            (0x2050u16, self.active),
            (0x2060, self.me),
            (0x2080, self.allies),
            (0x2090, self.active_allies),
            (0x20A0, self.enemies),
            (0x20B0, self.active_enemies),
            (0x20C0, self.players),
            (0x20E0, self.all),
        ] {
            img.put_le(usize::from(addr - 0x2000) >> 3, 2, u32::from(value));
        }
        img
    }
}

/// What the machine asks the battle.
pub trait World {
    /// Slot of the actor that owns the script.
    fn me(&self) -> u8;
    /// The masks behind the global addresses.
    fn masks(&self) -> Masks;
    /// The value of `size` at actor-specific address `addr` of the actor in `slot`; `None` if the
    /// battle does not model it (or there is no such actor).
    fn actor_field(&self, slot: u8, addr: u16, size: u8) -> Option<u32>;
    /// The MP an attack costs; `None` if the attack is not known.
    fn attack_mp_cost(&self, attack: u16) -> Option<u16>;
}

/// Whether a state of the script's memory can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The Initialize script has not run.
    Fresh,
    /// Initialized; the scripts may run.
    Ready,
    /// A script could not be followed: this actor falls back to the plain behaviour for good.
    Disabled,
}

/// The memory of one actor's scripts, kept through the battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    local: [u8; LOCAL_BYTES],
    idle_animation: u32,
    damaged_animation: u32,
    immune_death: bool,
    /// The slot masks of the last actors that hit this one by any action, by a physical one and by a magical
    /// one (the fields `0x40D0`, `0x40E0` and `0x40F0` of the owner's counters and Main scripts); 0: nobody yet.
    last_attacker: u16,
    last_physical: u16,
    last_magical: u16,
    pub phase: Phase,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    pub fn new() -> Self {
        Self {
            local: [0; LOCAL_BYTES],
            idle_animation: 0,
            damaged_animation: 0,
            immune_death: false,
            last_attacker: 0,
            last_physical: 0,
            last_magical: 0,
            phase: Phase::Fresh,
        }
    }

    /// Whether the scripts have made their actor immune to Death ([`ADDR_IMMUNE_DEATH`]; nothing in the
    /// battle looks at it).
    pub fn immune_death(&self) -> bool {
        self.immune_death
    }

    /// The slot mask of the last actor that acted on this one (`0x40D0`); 0 if none has.
    pub fn last_attacker(&self) -> u16 {
        self.last_attacker
    }

    /// The slot mask of the last actor that hit this one with a physical action (`0x40E0`).
    pub fn last_physical_attacker(&self) -> u16 {
        self.last_physical
    }

    /// The slot mask of the last actor that hit this one with a magical action (`0x40F0`).
    pub fn last_magical_attacker(&self) -> u16 {
        self.last_magical
    }

    /// Take note that the actor whose slot mask is `attacker` acted on this one, with a physical action or a
    /// magical one. The engine does it before the owner's counters run, so that they can answer the blow.
    pub fn note_hit(&mut self, attacker: u16, physical: bool) {
        self.last_attacker = attacker;
        if physical {
            self.last_physical = attacker;
        } else {
            self.last_magical = attacker;
        }
    }

    /// The value of `size` at local address `addr` (a bit address), `None` out of range.
    pub fn local(&self, addr: u16, size: u8) -> Option<u32> {
        let bit = usize::from(addr);
        let byte = bit >> 3;
        if size == 0 {
            return Some(u32::from(*self.local.get(byte)? >> (bit & 7) & 1));
        }
        let cells = self.local.get(byte..byte + bytes_of(size))?;
        Some(
            cells
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (8 * i)),
        )
    }

    fn write_local(&mut self, addr: u16, size: u8, v: u32) -> Result<(), AiError> {
        let byte = usize::from(addr) >> 3;
        if addr > 0x3FF {
            return Err(AiError::Address(addr));
        }
        if size == 0 {
            let pos = usize::from(addr) & 7;
            let cell = self.local.get_mut(byte).ok_or(AiError::Address(addr))?;
            if v != 0 {
                *cell |= 1 << pos;
            } else {
                *cell &= !(1 << pos);
            }
            return Ok(());
        }
        let n = bytes_of(size);
        let cells = self
            .local
            .get_mut(byte..byte + n)
            .ok_or(AiError::Address(addr))?;
        for (i, cell) in cells.iter_mut().enumerate() {
            *cell = (v >> (8 * i)) as u8;
        }
        Ok(())
    }
}

/// One change a script made to the battle, for the caller to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Set the physical defense rating of the actor in `slot`.
    Defense { slot: u8, value: u32 },
    /// Set the magical defense rating of the actor in `slot`.
    MagicDefense { slot: u8, value: u32 },
    /// Set the physical evasion of the actor in `slot` (some enemies raise it while they fly).
    Evade { slot: u8, value: u32 },
    /// Set the physical attack strength of the actor in `slot` (an enemy may raise it as it winds up).
    Attack { slot: u8, value: u32 },
    /// Set the magical attack strength of the actor in `slot`.
    MagicAttack { slot: u8, value: u32 },
}

/// The attack a script queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// The attack id (scene attack table, or the kernel's below `0x100`).
    pub attack: u16,
    /// The mask of the actors it is aimed at, as the script set it.
    pub targets: u16,
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// The enemy attacks the script queued, in the order it queued them, each with the target mask at the
    /// moment of its `ACT` (empty: it ended without one: the enemy lets its turn pass).
    pub actions: Vec<Action>,
    pub effects: Vec<Effect>,
    pub steps: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Num { v: u32, size: u8 },
    Addr { addr: u16, size: u8, mask: Option<u16> },
    Field { addr: u16, size: u8 },
    Set { vals: Vec<(u8, u32)>, size: u8 },
}

enum Flow {
    Next,
    Jump(usize),
    End,
}

struct Vm<'a> {
    world: &'a dyn World,
    mem: &'a mut Memory,
    rng: &'a mut Rng,
    me: u8,
    present: u16,
    globals: Image,
    targets: u16,
    stack: Vec<Item>,
    out: Outcome,
    overrides: BTreeMap<(u8, u16), u32>,
}

/// Run the script in `slot` of `scripts` against `world`, with `mem` as the actor's memory and `rng` for
/// every random draw. On an error `mem` and `rng` may have been changed: run on copies and keep them only
/// on success.
pub fn run(
    scripts: &EnemyScripts,
    slot: usize,
    world: &dyn World,
    mem: &mut Memory,
    rng: &mut Rng,
) -> Result<Outcome, AiError> {
    let entry = scripts.entry(slot).ok_or(AiError::NoScript)?;
    let code = scripts.code();
    let mut vm = Vm {
        world,
        mem,
        rng,
        me: world.me(),
        present: world.masks().all,
        globals: world.masks().image(),
        targets: 0,
        stack: Vec::new(),
        out: Outcome::default(),
        overrides: BTreeMap::new(),
    };
    let mut pc = entry;
    for step in 0..MAX_STEPS {
        let (insn, len) = script::decode(code, pc)?;
        match vm.step(insn)? {
            Flow::Next => pc += len,
            Flow::Jump(target) => {
                let to = entry + target;
                if to >= code.len() {
                    return Err(AiError::BadJump(target as u16));
                }
                pc = to;
            }
            Flow::End => {
                vm.out.steps = step + 1;
                return Ok(vm.out);
            }
        }
    }
    Err(AiError::Budget)
}

impl Vm<'_> {
    fn push(&mut self, item: Item) -> Result<(), AiError> {
        if self.stack.len() >= MAX_STACK {
            return Err(AiError::StackOverflow);
        }
        self.stack.push(item);
        Ok(())
    }

    fn pop(&mut self) -> Result<Item, AiError> {
        self.stack.pop().ok_or(AiError::StackUnderflow)
    }

    /// The number an entry stands for where any kind is accepted: a number, or the first value of a set.
    fn scalar(item: &Item, opcode: u8) -> Result<u32, AiError> {
        match item {
            Item::Num { v, .. } => Ok(*v),
            Item::Set { vals, .. } => vals
                .first()
                .map(|&(_, v)| v)
                .ok_or(AiError::EmptySet(opcode)),
            _ => Err(AiError::Type(opcode)),
        }
    }

    fn pop_scalar(&mut self, opcode: u8) -> Result<u32, AiError> {
        let item = self.pop()?;
        Self::scalar(&item, opcode)
    }

    /// Read a value from a local or global address.
    fn read_value(&self, addr: u16, size: u8) -> Result<u32, AiError> {
        match addr {
            0..=0x03FF => self.mem.local(addr, size).ok_or(AiError::Address(addr)),
            0x2000..=0x21FF => {
                let rel = usize::from(addr - 0x2000);
                // The target mask is the machine's own state; it overlays the image.
                let target_bits = usize::from(ADDR_TARGETS - 0x2000);
                if (target_bits..target_bits + 16).contains(&rel) {
                    let mut img = Image::new(2);
                    img.put_le(0, 2, u32::from(self.targets));
                    return img
                        .read(rel - target_bits, size)
                        .ok_or(AiError::Address(addr));
                }
                self.globals.read(rel, size).ok_or(AiError::Address(addr))
            }
            _ => Err(AiError::Address(addr)),
        }
    }

    /// The value of an actor field for the actor in `slot`.
    fn field_value(&self, slot: u8, addr: u16, size: u8) -> Result<u32, AiError> {
        if slot == self.me {
            match addr {
                ADDR_IDLE_ANIMATION => return Ok(trunc(self.mem.idle_animation, size)),
                ADDR_DAMAGED_ANIMATION => return Ok(trunc(self.mem.damaged_animation, size)),
                ADDR_IMMUNE_DEATH => return Ok(trunc(u32::from(self.mem.immune_death), size)),
                _ => {}
            }
        }
        if let Some(v) = self.overrides.get(&(slot, addr)) {
            return Ok(trunc(*v, size));
        }
        self.world
            .actor_field(slot, addr, size)
            .ok_or(AiError::Address(addr))
    }

    fn write_field(&mut self, slot: u8, addr: u16, size: u8, v: u32) -> Result<(), AiError> {
        if slot != self.me {
            return Err(AiError::Address(addr));
        }
        match addr {
            ADDR_IDLE_ANIMATION => self.mem.idle_animation = trunc(v, size),
            ADDR_DAMAGED_ANIMATION => self.mem.damaged_animation = trunc(v, size),
            ADDR_IMMUNE_DEATH => self.mem.immune_death = trunc(v, size) != 0,
            ADDR_DEFENSE => {
                let v = trunc(v, size);
                self.overrides.insert((slot, addr), v);
                self.out.effects.push(Effect::Defense { slot, value: v });
            }
            ADDR_MAGIC_DEFENSE => {
                let v = trunc(v, size);
                self.overrides.insert((slot, addr), v);
                self.out.effects.push(Effect::MagicDefense { slot, value: v });
            }
            ADDR_EVADE | ADDR_ATTACK | ADDR_MAGIC_ATTACK => {
                let v = trunc(v, size);
                self.overrides.insert((slot, addr), v);
                self.out.effects.push(match addr {
                    ADDR_EVADE => Effect::Evade { slot, value: v },
                    ADDR_ATTACK => Effect::Attack { slot, value: v },
                    _ => Effect::MagicAttack { slot, value: v },
                });
            }
            _ => return Err(AiError::Address(addr)),
        }
        Ok(())
    }

    /// The mask a `MASK` operand stands for: a number, or the value at a global or local address.
    fn mask_of(&self, item: &Item) -> Result<u16, AiError> {
        match *item {
            Item::Num { v, .. } => Ok((v & 0xFFFF) as u16),
            Item::Addr {
                addr,
                size,
                mask: None,
            } if addr < FIRST_FIELD => Ok((self.read_value(addr, size)? & 0xFFFF) as u16),
            _ => Err(AiError::Type(op::MASK)),
        }
    }

    fn step(&mut self, insn: Insn) -> Result<Flow, AiError> {
        match insn {
            Insn::PushValue { size, addr } => {
                if addr >= FIRST_FIELD {
                    self.push(Item::Field { addr, size })?;
                } else {
                    let v = self.read_value(addr, size)?;
                    self.push(Item::Num { v, size })?;
                }
            }
            Insn::PushAddr { size, addr } => {
                self.push(Item::Addr {
                    addr,
                    size,
                    mask: None,
                })?;
            }
            Insn::Const { size, value } => self.push(Item::Num { v: value, size })?,
            Insn::Jump { op: opcode, target } => {
                let target = usize::from(target);
                match opcode {
                    op::JUMP => return Ok(Flow::Jump(target)),
                    op::JUMP_IF_ZERO => {
                        if self.pop_scalar(opcode)? == 0 {
                            return Ok(Flow::Jump(target));
                        }
                    }
                    _ => {
                        // Jump if the constant just popped differs from the entry below it, which stays.
                        let case = self.pop_scalar(opcode)?;
                        let top = self.stack.last().ok_or(AiError::StackUnderflow)?;
                        if case != Self::scalar(top, opcode)? {
                            return Ok(Flow::Jump(target));
                        }
                    }
                }
            }
            Insn::Text { .. } => {}
            Insn::Debug { .. } => return Err(AiError::Unsupported(op::DEBUG)),
            Insn::Simple(opcode) => return self.simple(opcode),
        }
        Ok(Flow::Next)
    }

    fn simple(&mut self, opcode: u8) -> Result<Flow, AiError> {
        match opcode {
            op::ADD | op::SUB | op::MUL | op::DIV | op::MOD | op::AND | op::OR => {
                self.arithmetic(opcode)?
            }
            op::EQ | op::NE | op::GE | op::LE | op::GT | op::LT => self.compare(opcode)?,
            op::LAND | op::LOR => {
                let b = self.pop_scalar(opcode)? != 0;
                let a = self.pop_scalar(opcode)? != 0;
                let v = if opcode == op::LAND { a && b } else { a || b };
                self.push(Item::Num {
                    v: u32::from(v),
                    size: 0,
                })?;
            }
            op::LNOT => {
                let a = self.pop_scalar(opcode)?;
                self.push(Item::Num {
                    v: u32::from(a == 0),
                    size: 0,
                })?;
            }
            // Bitwise NOT ("Bit-wise NOT of pop"), cut to the size of what was popped; a set is inverted
            // value by value.
            op::NOT => {
                let inverted = match self.pop()? {
                    Item::Num { v, size } => Item::Num {
                        v: trunc(!v, size),
                        size,
                    },
                    Item::Set { vals, size } => Item::Set {
                        vals: vals.into_iter().map(|(slot, v)| (slot, trunc(!v, size))).collect(),
                        size,
                    },
                    _ => return Err(AiError::Type(opcode)),
                };
                self.push(inverted)?;
            }
            // "Bit count": the number of bits set in what was popped (the first value of a set).
            op::BIT_COUNT => {
                let (v, size) = match self.pop()? {
                    Item::Num { v, size } => (v, size),
                    Item::Set { vals, size } => (vals.first().map(|&(_, v)| v).ok_or(AiError::EmptySet(opcode))?, size),
                    _ => return Err(AiError::Type(opcode)),
                };
                self.push(Item::Num {
                    v: v.count_ones(),
                    size: size.max(1),
                })?;
            }
            // "Type 02 with only bit in pop turned on (1 << [pop])": the mask of one actor from its slot.
            op::BIT => {
                let n = self.pop_scalar(opcode)?;
                let v = if n < 16 { 1u32 << n } else { 0 };
                self.push(Item::Num { v, size: 2 })?;
            }
            op::END => return Ok(Flow::End),
            op::DROP => {
                self.pop()?;
            }
            op::MASK => self.mask()?,
            op::RANDOM => {
                let v = u32::from(self.rng.next_u16());
                self.push(Item::Num { v, size: 2 })?;
            }
            op::RANDOM_BIT => {
                let mask = match self.pop()? {
                    Item::Num { v, .. } => (v & 0xFFFF) as u16,
                    _ => return Err(AiError::Type(opcode)),
                };
                let n = mask.count_ones();
                let picked = if n <= 1 {
                    mask
                } else {
                    nth_set_bit(mask, self.rng.below(n))
                };
                self.push(Item::Num {
                    v: u32::from(picked),
                    size: 2,
                })?;
            }
            op::GREATEST | op::LEAST => {
                let Item::Set { vals, .. } = self.pop()? else {
                    return Err(AiError::Type(opcode));
                };
                let best = if opcode == op::GREATEST {
                    vals.iter().map(|&(_, v)| v).max()
                } else {
                    vals.iter().map(|&(_, v)| v).min()
                };
                let mask = best.map_or(0, |best| {
                    vals.iter()
                        .filter(|&&(_, v)| v == best)
                        .fold(0u32, |m, &(slot, _)| m | 1u32 << slot)
                });
                self.push(Item::Num { v: mask, size: 2 })?;
            }
            op::MP_COST => {
                let id = self.pop_scalar(opcode)?;
                let id = u16::try_from(id).map_err(|_| AiError::Attack(0xFFFF))?;
                let cost = self.world.attack_mp_cost(id).ok_or(AiError::Attack(id))?;
                self.push(Item::Num {
                    v: u32::from(cost),
                    size: 2,
                })?;
            }
            op::STORE => self.store()?,
            op::ACT => {
                let attack = self.pop_scalar(opcode)?;
                let command = self.pop_scalar(opcode)?;
                match command {
                    COMMAND_ENEMY_ATTACK => {
                        if self.out.actions.len() >= MAX_ACTIONS {
                            return Err(AiError::TooManyActions);
                        }
                        let attack = u16::try_from(attack).map_err(|_| AiError::Attack(0xFFFF))?;
                        self.out.actions.push(Action {
                            attack,
                            targets: self.targets,
                        });
                    }
                    COMMAND_PAUSE_FOR_TEXT => {}
                    _ => return Err(AiError::Unsupported(opcode)),
                }
            }
            other => return Err(AiError::Unsupported(other)),
        }
        Ok(Flow::Next)
    }

    fn arithmetic(&mut self, opcode: u8) -> Result<(), AiError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let calc = |x: u32, y: u32| -> Result<u32, AiError> {
            Ok(match opcode {
                op::ADD => x.wrapping_add(y),
                op::SUB => x.wrapping_sub(y),
                op::MUL => x.wrapping_mul(y),
                op::DIV => x.checked_div(y).ok_or(AiError::DivideByZero)?,
                op::MOD => x.checked_rem(y).ok_or(AiError::DivideByZero)?,
                op::AND => x & y,
                _ => x | y,
            })
        };
        let result = match (a, b) {
            (Item::Num { v: x, size: sx }, Item::Num { v: y, size: sy }) => {
                let size = sx.max(sy);
                Item::Num {
                    v: trunc(calc(x, y)?, size),
                    size,
                }
            }
            (Item::Set { vals, size: sx }, Item::Num { v: y, size: sy }) => {
                let size = sx.max(sy);
                let mut out = Vec::with_capacity(vals.len());
                for (slot, x) in vals {
                    out.push((slot, trunc(calc(x, y)?, size)));
                }
                Item::Set { vals: out, size }
            }
            (Item::Num { v: x, size: sx }, Item::Set { vals, size: sy }) => {
                let size = sx.max(sy);
                let mut out = Vec::with_capacity(vals.len());
                for (slot, y) in vals {
                    out.push((slot, trunc(calc(x, y)?, size)));
                }
                Item::Set { vals: out, size }
            }
            (Item::Set { vals: xs, size: sx }, Item::Set { vals: ys, size: sy }) => {
                let size = sx.max(sy);
                let mut out = Vec::new();
                for (slot, x) in xs {
                    if let Some(&(_, y)) = ys.iter().find(|&&(s, _)| s == slot) {
                        out.push((slot, trunc(calc(x, y)?, size)));
                    }
                }
                Item::Set { vals: out, size }
            }
            _ => return Err(AiError::Type(opcode)),
        };
        self.push(result)
    }

    fn compare(&mut self, opcode: u8) -> Result<(), AiError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let holds = |x: u32, y: u32| match opcode {
            op::EQ => x == y,
            op::NE => x != y,
            op::GE => x >= y,
            op::LE => x <= y,
            op::GT => x > y,
            _ => x < y,
        };
        let to_mask = |pairs: Vec<(u8, bool)>| {
            pairs
                .into_iter()
                .filter(|&(_, ok)| ok)
                .fold(0u32, |m, (slot, _)| m | 1u32 << slot)
        };
        let result = match (a, b) {
            (Item::Num { v: x, .. }, Item::Num { v: y, .. }) => Item::Num {
                v: u32::from(holds(x, y)),
                size: 0,
            },
            (Item::Set { vals, .. }, Item::Num { v: y, .. }) => Item::Num {
                v: to_mask(vals.iter().map(|&(s, x)| (s, holds(x, y))).collect()),
                size: 2,
            },
            (Item::Num { v: x, .. }, Item::Set { vals, .. }) => Item::Num {
                v: to_mask(vals.iter().map(|&(s, y)| (s, holds(x, y))).collect()),
                size: 2,
            },
            (Item::Set { vals: xs, .. }, Item::Set { vals: ys, .. }) => Item::Num {
                v: to_mask(
                    xs.iter()
                        .filter_map(|&(s, x)| {
                            ys.iter()
                                .find(|&&(t, _)| t == s)
                                .map(|&(_, y)| (s, holds(x, y)))
                        })
                        .collect(),
                ),
                size: 2,
            },
            _ => return Err(AiError::Type(opcode)),
        };
        self.push(result)
    }

    /// `MASK`: the actors in a mask applied to an actor field (a set of their values) or to an address
    /// (a place to store to for those actors).
    fn mask(&mut self) -> Result<(), AiError> {
        let top = self.pop()?;
        let below = self.pop()?;
        let mask = self.mask_of(&below)?;
        let present = self.present;
        let result = match top {
            Item::Field { addr, size } => {
                let mut vals = Vec::new();
                for slot in 0..16u8 {
                    if mask >> slot & 1 == 1 && present >> slot & 1 == 1 {
                        vals.push((slot, self.field_value(slot, addr, size)?));
                    }
                }
                Item::Set { vals, size }
            }
            Item::Addr {
                addr,
                size,
                mask: None,
            } if addr >= FIRST_FIELD => Item::Addr {
                addr,
                size,
                mask: Some(mask),
            },
            _ => return Err(AiError::Type(op::MASK)),
        };
        self.push(result)
    }

    fn store(&mut self) -> Result<(), AiError> {
        let value = self.pop()?;
        let dest = self.pop()?;
        let v = Self::scalar(&value, op::STORE)?;
        match dest {
            Item::Addr {
                addr,
                size,
                mask: None,
            } if addr < 0x400 => self.mem.write_local(addr, size, v)?,
            Item::Addr {
                addr: ADDR_TARGETS,
                size: 2,
                mask: None,
            } => self.targets = (v & 0xFFFF) as u16,
            Item::Addr {
                addr,
                size,
                mask: Some(mask),
            } => {
                let present = self.present;
                for slot in 0..16u8 {
                    if mask >> slot & 1 == 1 && present >> slot & 1 == 1 {
                        self.write_field(slot, addr, size, v)?;
                    }
                }
            }
            Item::Addr { addr, .. } => return Err(AiError::Address(addr)),
            _ => return Err(AiError::Type(op::STORE)),
        }
        Ok(())
    }
}

/// The `k`-th (from 0) set bit of `mask`, counting from the lowest.
fn nth_set_bit(mask: u16, k: u32) -> u16 {
    let mut seen = 0;
    for bit in 0..16 {
        if mask >> bit & 1 == 1 {
            if seen == k {
                return 1 << bit;
            }
            seen += 1;
        }
    }
    0
}

/// Whether `insn` is something this machine implements (the address it names is checked by
/// [`unsupported_address`]).
pub fn implements(insn: &Insn) -> bool {
    match *insn {
        Insn::PushValue { .. } | Insn::PushAddr { .. } | Insn::Const { .. } => true,
        Insn::Jump { .. } | Insn::Text { .. } => true,
        Insn::Debug { .. } => false,
        Insn::Simple(opcode) => matches!(
            opcode,
            op::ADD
                | op::SUB
                | op::MUL
                | op::DIV
                | op::MOD
                | op::AND
                | op::OR
                | op::EQ
                | op::NE
                | op::GE
                | op::LE
                | op::GT
                | op::LT
                | op::LAND
                | op::LOR
                | op::LNOT
                | op::NOT
                | op::END
                | op::DROP
                | op::MASK
                | op::RANDOM
                | op::RANDOM_BIT
                | op::BIT_COUNT
                | op::GREATEST
                | op::LEAST
                | op::MP_COST
                | op::BIT
                | op::STORE
                | op::ACT
        ),
    }
}

/// The address a push names, if the machine does not model it: `field_known` says whether the battle
/// models an actor field (see [`super::world::field_known`]).
pub fn unsupported_address(insn: &Insn, field_known: impl Fn(u16, u8) -> bool) -> Option<u16> {
    let (addr, size, is_value) = match *insn {
        Insn::PushValue { size, addr } => (addr, size, true),
        Insn::PushAddr { size, addr } => (addr, size, false),
        _ => return None,
    };
    let ok = match addr {
        0..=0x03FF => usize::from(addr >> 3) + bytes_of(size) <= LOCAL_BYTES,
        0x2000..=0x21FF => {
            let rel = usize::from(addr - 0x2000);
            let targets = usize::from(ADDR_TARGETS - 0x2000);
            (targets..targets + 16).contains(&rel)
                || Masks {
                    active: 1,
                    me: 1,
                    allies: 1,
                    active_allies: 1,
                    enemies: 1,
                    active_enemies: 1,
                    players: 1,
                    all: 1,
                }
                .image()
                .read(rel, size)
                .is_some()
        }
        FIRST_FIELD.. => {
            // A value read needs the field to be modelled; an address push is a store target.
            if is_value {
                field_known(addr, size)
                    || matches!(addr, ADDR_IDLE_ANIMATION | ADDR_DAMAGED_ANIMATION)
            } else {
                matches!(
                    addr,
                    ADDR_IDLE_ANIMATION
                        | ADDR_DAMAGED_ANIMATION
                        | ADDR_IMMUNE_DEATH
                        | ADDR_DEFENSE
                        | ADDR_MAGIC_DEFENSE
                        | ADDR_EVADE
                        | ADDR_ATTACK
                        | ADDR_MAGIC_ATTACK
                )
            }
        }
        _ => false,
    };
    (!ok).then_some(addr)
}


#[cfg(test)]
mod tests {
    use super::super::asm::Asm;
    use super::*;

    /// A world with a fixed cast: slots 0..=2 players, 4..=5 enemies, HP per slot.
    struct Fixed {
        me: u8,
        hp: [u32; 10],
        back: u16,
    }

    impl Fixed {
        fn new(me: u8) -> Self {
            let mut hp = [0; 10];
            hp[0] = 300;
            hp[1] = 120;
            hp[2] = 500;
            hp[4] = 80;
            hp[5] = 90;
            Self {
                me,
                hp,
                back: 0b100,
            }
        }
    }

    impl World for Fixed {
        fn me(&self) -> u8 {
            self.me
        }
        fn masks(&self) -> Masks {
            let alive: u16 = (0..10u16)
                .filter(|&s| self.hp[usize::from(s)] > 0)
                .fold(0, |m, s| m | 1 << s);
            let players = alive & 0b111;
            let foes = alive & 0b11_0000;
            let (allies, enemies) = if self.me < 3 {
                (players, foes)
            } else {
                (foes, players)
            };
            Masks {
                active: alive,
                me: 1 << self.me,
                allies,
                active_allies: allies,
                enemies,
                active_enemies: enemies,
                players,
                all: 0b11_0111,
            }
        }
        fn actor_field(&self, slot: u8, addr: u16, size: u8) -> Option<u32> {
            let hp = *self.hp.get(usize::from(slot))?;
            match (addr, size) {
                (0x4160, 3) => Some(hp),
                (0x4026, 0) => Some(u32::from(self.back >> slot & 1)),
                (0x4100, 2) => Some(30 + u32::from(slot)),
                (0x4078, 1) => Some(25),
                (0x4068, 1) => Some(40),
                (0x4070, 1) => Some(9),
                _ => None,
            }
        }
        fn attack_mp_cost(&self, attack: u16) -> Option<u16> {
            (attack == 0x120).then_some(7)
        }
    }

    fn run_asm(
        asm: &Asm,
        world: &dyn World,
        mem: &mut Memory,
        rng: &mut Rng,
    ) -> Result<Outcome, AiError> {
        run(&asm.block(), script::slot::MAIN, world, mem, rng)
    }

    fn go(asm: &Asm) -> Result<Outcome, AiError> {
        run_asm(asm, &Fixed::new(4), &mut Memory::new(), &mut Rng::new(1))
    }

    fn targets_of(asm: &Asm) -> u16 {
        go(asm).unwrap().actions[0].targets
    }

    #[test]
    fn a_script_that_ends_at_once_passes_its_turn() {
        let out = go(&Asm::default().op(op::END)).unwrap();
        assert_eq!(out.actions, vec![]);
        assert_eq!(out.steps, 1);
    }

    #[test]
    fn act_queues_an_attack_with_the_target_mask_at_that_moment() {
        let out = go(&Asm::act_on(|a| a.k1(0b10), 0x0110)).unwrap();
        assert_eq!(
            out.actions,
            vec![Action {
                attack: 0x0110,
                targets: 0b10
            }]
        );
    }

    #[test]
    fn every_act_is_queued_in_order_each_with_the_mask_of_its_moment() {
        // The double attack of the real scripts: act on one target, then on another (the mask is stored
        // again between them), then a third time on the first mask without storing it again.
        let asm = Asm::default()
            .addr(2, 0x2070)
            .k1(0b001)
            .op(op::STORE)
            .k1(0x20)
            .k2(0x0110)
            .op(op::ACT)
            .addr(2, 0x2070)
            .k1(0b100)
            .op(op::STORE)
            .k1(0x20)
            .k2(0x0111)
            .op(op::ACT)
            .k1(0x20)
            .k2(0x0112)
            .op(op::ACT)
            .op(op::END);
        let out = go(&asm).unwrap();
        assert_eq!(
            out.actions,
            vec![
                Action { attack: 0x0110, targets: 0b001 },
                Action { attack: 0x0111, targets: 0b100 },
                Action { attack: 0x0112, targets: 0b100 },
            ]
        );
        // The text pause between two attacks is no action; the order is the order of the `ACT`s.
        let with_text = Asm::default()
            .k1(0x20)
            .k2(7)
            .op(op::ACT)
            .k1(0x24)
            .k1(0)
            .op(op::ACT)
            .k1(0x20)
            .k2(8)
            .op(op::ACT)
            .op(op::END);
        let attacks: Vec<u16> = go(&with_text).unwrap().actions.iter().map(|a| a.attack).collect();
        assert_eq!(attacks, vec![7, 8]);
    }

    #[test]
    fn a_script_may_queue_a_bounded_number_of_actions() {
        let queue = |n: usize| {
            let mut asm = Asm::default();
            for i in 0..n {
                asm = asm.k1(0x20).k2(i as u16).op(op::ACT);
            }
            asm.op(op::END)
        };
        assert_eq!(go(&queue(MAX_ACTIONS)).unwrap().actions.len(), MAX_ACTIONS);
        assert_eq!(go(&queue(MAX_ACTIONS + 1)), Err(AiError::TooManyActions));
    }

    #[test]
    fn unknown_action_commands_are_refused_and_the_text_pause_is_nothing() {
        let run_script = Asm::default().k1(0x22).k1(8).op(op::ACT).op(op::END);
        assert_eq!(go(&run_script), Err(AiError::Unsupported(op::ACT)));
        // The text pause is a no-op.
        let pause = Asm::default().k1(0x24).k1(0).op(op::ACT).op(op::END);
        assert_eq!(go(&pause).unwrap().actions, vec![]);
    }

    #[test]
    fn locals_keep_their_size_and_persist_in_memory() {
        // LV0020 (byte) = 5; LV0040 (word) = LV0020 + 300; LV0060 (bit) = 1
        let asm = Asm::default()
            .addr(1, 0x20)
            .k1(5)
            .op(op::STORE)
            .addr(2, 0x40)
            .val(1, 0x20)
            .k2(300)
            .op(op::ADD)
            .op(op::STORE)
            .addr(0, 0x60)
            .k1(1)
            .op(op::STORE)
            .op(op::END);
        let mut mem = Memory::new();
        run_asm(&asm, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert_eq!(mem.local(0x20, 1), Some(5));
        assert_eq!(mem.local(0x40, 2), Some(305));
        assert_eq!(mem.local(0x60, 0), Some(1));
        assert_eq!(mem.local(0x61, 0), Some(0));
        assert_eq!(mem.local(0x400, 1), None);
        // A second run starts from what the first left.
        let read_back = Asm::act_on(|a| a.val(2, 0x40), 9);
        let out = run_asm(&read_back, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert_eq!(out.actions[0].targets, 305);
    }

    #[test]
    fn operators_take_the_entry_pushed_first_as_their_left_side() {
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(10).k1(3).op(op::SUB), 1)), 7);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(9).k1(2).op(op::DIV), 1)), 4);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(9).k1(4).op(op::MOD), 1)), 1);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(7).k1(9).op(op::LT), 1)), 1);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(7).k1(9).op(op::GE), 1)), 0);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(6).k1(3).op(op::AND), 1)), 2);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(6).k1(3).op(op::OR), 1)), 7);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(0).op(op::LNOT), 1)), 1);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(3).k1(0).op(op::LAND), 1)), 0);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(3).k1(0).op(op::LOR), 1)), 1);
        let div0 = Asm::act_on(|a| a.k1(7).k1(0).op(op::DIV), 1);
        assert_eq!(go(&div0), Err(AiError::DivideByZero));
        // A byte result wraps at its size: 250 + 10 in bytes is 4.
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(250).k1(10).op(op::ADD), 1)), 4);
    }

    #[test]
    fn jump_if_zero_and_the_switch_form() {
        // if (0) { targets = 1 } else { targets = 2 }
        let mut asm = Asm::default().k1(0);
        let jz_at = asm.here();
        asm = asm.jump(op::JUMP_IF_ZERO, 0);
        asm = asm.addr(2, 0x2070).k1(1).op(op::STORE);
        let jmp_at = asm.here();
        asm = asm.jump(op::JUMP, 0);
        let else_at = asm.here();
        asm = asm.addr(2, 0x2070).k1(2).op(op::STORE);
        let end_at = asm.here();
        asm = asm.k1(0x20).k2(3).op(op::ACT).op(op::END);
        asm.patch(jz_at, else_at);
        asm.patch(jmp_at, end_at);
        assert_eq!(targets_of(&asm), 2);

        // switch (5) { case 4: targets = 1; case 5: targets = 2 }: JNE keeps the value, DROP ends it.
        let mut asm = Asm::default().k1(5).k1(4);
        let ne1 = asm.here();
        asm = asm
            .jump(op::JUMP_IF_NE, 0)
            .addr(2, 0x2070)
            .k1(1)
            .op(op::STORE);
        let j1 = asm.here();
        asm = asm.jump(op::JUMP, 0);
        let case5 = asm.here();
        asm = asm.k1(5);
        let ne2 = asm.here();
        asm = asm
            .jump(op::JUMP_IF_NE, 0)
            .addr(2, 0x2070)
            .k1(2)
            .op(op::STORE);
        let end = asm.here();
        asm = asm.op(op::DROP).k1(0x20).k2(3).op(op::ACT).op(op::END);
        asm.patch(ne1, case5);
        asm.patch(j1, end);
        asm.patch(ne2, end);
        assert_eq!(targets_of(&asm), 2);
    }

    #[test]
    fn masks_read_actor_fields_into_sets_and_comparisons_give_masks() {
        // The enemies (of slot 4) are the players 0, 1, 2: HP 300, 120, 500.
        // targets = enemies whose HP >= 300  -> slots 0 and 2
        let hp_ge = Asm::act_on(
            |a| {
                a.val(2, 0x20A0)
                    .val(3, 0x4160)
                    .op(op::MASK)
                    .k2(300)
                    .op(op::GE)
            },
            1,
        );
        assert_eq!(targets_of(&hp_ge), 0b101);
        // the enemy with the least HP -> slot 1; the greatest -> slot 2
        let least = Asm::act_on(
            |a| a.val(2, 0x20A0).val(3, 0x4160).op(op::MASK).op(op::LEAST),
            1,
        );
        assert_eq!(targets_of(&least), 0b010);
        let greatest = Asm::act_on(
            |a| {
                a.val(2, 0x20A0)
                    .val(3, 0x4160)
                    .op(op::MASK)
                    .op(op::GREATEST)
            },
            1,
        );
        assert_eq!(targets_of(&greatest), 0b100);
        // Back row bit: slot 2 is in the back row.
        let back = Asm::act_on(
            |a| a.val(2, 0x20A0).val(0, 0x4026).op(op::MASK).k1(1).op(op::EQ),
            1,
        );
        assert_eq!(targets_of(&back), 0b100);
        // Arithmetic on a set: HP / 2 < 100 -> slot 1 (60)
        let half = Asm::act_on(
            |a| {
                a.val(2, 0x20A0)
                    .val(3, 0x4160)
                    .op(op::MASK)
                    .k1(2)
                    .op(op::DIV)
                    .k1(100)
                    .op(op::LT)
            },
            1,
        );
        assert_eq!(targets_of(&half), 0b010);
        // The address 0x2060 as a mask operand stands for the value there (me = slot 4).
        let me = Asm::act_on(
            |a| {
                a.addr(2, 0x2060)
                    .val(3, 0x4160)
                    .op(op::MASK)
                    .k1(80)
                    .op(op::EQ)
            },
            1,
        );
        assert_eq!(targets_of(&me), 0b1_0000);
    }

    #[test]
    fn a_random_bit_picks_one_bit_of_the_mask_with_the_battle_rng_only() {
        let asm = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), 1);
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..200 {
            let a = run_asm(&asm, &Fixed::new(4), &mut Memory::new(), &mut Rng::new(seed)).unwrap();
            let b = run_asm(&asm, &Fixed::new(4), &mut Memory::new(), &mut Rng::new(seed)).unwrap();
            assert_eq!(a, b, "same seed, same pick");
            let t = a.actions[0].targets;
            assert_eq!(t.count_ones(), 1);
            assert!(t & 0b111 == t, "only an enemy of slot 4");
            seen.insert(t);
        }
        assert_eq!(seen.len(), 3, "every candidate is picked sometimes");
        // One candidate or none: no draw is made, so the stream is untouched.
        let one = Asm::act_on(|a| a.k1(0b100).op(op::RANDOM_BIT), 1);
        let mut rng = Rng::new(9);
        run_asm(&one, &Fixed::new(4), &mut Memory::new(), &mut rng).unwrap();
        assert_eq!(rng, Rng::new(9));
        let none = Asm::act_on(|a| a.k1(0).op(op::RANDOM_BIT), 1);
        let out = run_asm(&none, &Fixed::new(4), &mut Memory::new(), &mut Rng::new(9)).unwrap();
        assert_eq!(out.actions[0].targets, 0);
    }

    #[test]
    fn random_is_one_word_draw() {
        let got = targets_of(&Asm::act_on(|a| a.op(op::RANDOM), 1));
        assert_eq!(got, Rng::new(1).next_u16());
    }

    #[test]
    fn mp_cost_asks_the_world() {
        assert_eq!(targets_of(&Asm::act_on(|a| a.k2(0x120).op(op::MP_COST), 1)), 7);
        let unknown = Asm::act_on(|a| a.k2(0x999).op(op::MP_COST), 1);
        assert_eq!(go(&unknown), Err(AiError::Attack(0x999)));
    }

    #[test]
    fn writes_to_actor_fields_are_effects_visible_to_the_rest_of_the_run() {
        // me.defense = 200; targets = me.defense  (the value read back is the new one)
        let asm = Asm::default()
            .addr(2, 0x2060)
            .addr(2, 0x4100)
            .op(op::MASK)
            .k2(200)
            .op(op::STORE)
            .addr(2, 0x2070)
            .val(2, 0x2060)
            .val(2, 0x4100)
            .op(op::MASK)
            .op(op::STORE)
            .k1(0x20)
            .k2(1)
            .op(op::ACT)
            .op(op::END);
        let out = go(&asm).unwrap();
        assert_eq!(out.effects, vec![Effect::Defense { slot: 4, value: 200 }]);
        assert_eq!(out.actions[0].targets, 200, "read back the written value");
        // Another actor's field cannot be written, nor a field that is not modelled.
        let other = Asm::default()
            .k1(1)
            .addr(2, 0x4100)
            .op(op::MASK)
            .k2(5)
            .op(op::STORE)
            .op(op::END);
        assert_eq!(go(&other), Err(AiError::Address(0x4100)));
        let unknown = Asm::default()
            .addr(2, 0x2060)
            .addr(2, 0x4160)
            .op(op::MASK)
            .k2(5)
            .op(op::STORE)
            .op(op::END);
        assert_eq!(go(&unknown), Err(AiError::Address(0x4160)));
    }

    #[test]
    fn an_actors_attack_magic_and_evasion_are_effects_its_script_can_write_and_read_back() {
        // me.evade = me.evade + 10 (25 + 10); me.attack = 90; me.magic = 5; then targets = me.evade.
        let asm = Asm::default()
            .addr(2, 0x2060)
            .addr(1, 0x4078)
            .op(op::MASK)
            .val(2, 0x2060)
            .val(1, 0x4078)
            .op(op::MASK)
            .k1(10)
            .op(op::ADD)
            .op(op::STORE)
            .addr(2, 0x2060)
            .addr(1, 0x4068)
            .op(op::MASK)
            .k1(90)
            .op(op::STORE)
            .addr(2, 0x2060)
            .addr(1, 0x4070)
            .op(op::MASK)
            .k1(5)
            .op(op::STORE)
            .addr(2, 0x2070)
            .val(2, 0x2060)
            .val(1, 0x4078)
            .op(op::MASK)
            .op(op::STORE)
            .k1(0x20)
            .k2(1)
            .op(op::ACT)
            .op(op::END);
        let out = go(&asm).unwrap();
        assert_eq!(
            out.effects,
            vec![
                Effect::Evade { slot: 4, value: 35 },
                Effect::Attack { slot: 4, value: 90 },
                Effect::MagicAttack { slot: 4, value: 5 },
            ]
        );
        assert_eq!(out.actions[0].targets, 35, "the value read back is the written one");
        // Only the owner's own stats can be written.
        for addr in [0x4078u16, 0x4068, 0x4070] {
            let other = Asm::default().k1(1).addr(1, addr).op(op::MASK).k1(1).op(op::STORE).op(op::END);
            assert_eq!(go(&other), Err(AiError::Address(addr)), "{addr:#06x}");
        }
    }

    #[test]
    fn animation_ids_are_private_to_the_script_and_persist() {
        let set = Asm::default()
            .addr(2, 0x2060)
            .addr(1, 0x4080)
            .op(op::MASK)
            .k1(7)
            .op(op::STORE)
            .op(op::END);
        let mut mem = Memory::new();
        run_asm(&set, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        let read = Asm::act_on(|a| a.val(2, 0x2060).val(1, 0x4080).op(op::MASK), 1);
        let out = run_asm(&read, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert_eq!(out.actions[0].targets, 7);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn bitwise_not_keeps_the_size_of_what_it_inverts() {
        // A byte 5 inverts to 0xFA, a word 0x0010 to 0xFFEF.
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(5).op(op::NOT), 1)), 0xFA);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k2(0x0010).op(op::NOT), 1)), 0xFFEF);
        // The usual use: a mask with some actors taken out ("the enemies, but not me" is `enemies & !me`).
        let others = Asm::act_on(|a| a.val(2, 0x20A0).val(2, 0x2060).op(op::NOT).op(op::AND), 1);
        assert_eq!(targets_of(&others), 0b111, "the party; slot 4 is not among them anyway");
        let not_slot_one = Asm::act_on(|a| a.val(2, 0x20A0).k1(0b010).op(op::NOT).op(op::AND), 1);
        assert_eq!(targets_of(&not_slot_one), 0b101);
        // A set is inverted value by value: the back-row bit of the three players is (0, 0, 1).
        let inverted = Asm::act_on(|a| a.val(2, 0x20A0).val(0, 0x4026).op(op::MASK).op(op::NOT).k1(1).op(op::EQ), 1);
        assert_eq!(targets_of(&inverted), 0b011);
        // An address cannot be inverted.
        assert_eq!(go(&Asm::default().addr(2, 0x0000).op(op::NOT).op(op::END)), Err(AiError::Type(op::NOT)));
        assert_eq!(go(&Asm::default().op(op::NOT)), Err(AiError::StackUnderflow));
    }

    #[test]
    fn bit_count_counts_the_bits_of_a_mask_and_bit_makes_the_mask_of_a_slot() {
        // The enemies of slot 4 are the three players; a plain number; nothing.
        assert_eq!(targets_of(&Asm::act_on(|a| a.val(2, 0x20A0).op(op::BIT_COUNT), 1)), 3);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k2(0b1011_0000_0000_0001).op(op::BIT_COUNT), 1)), 4);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(0).op(op::BIT_COUNT), 1)), 0);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(0xFF).op(op::BIT_COUNT), 1)), 8);
        // Of a comparison's mask: how many of the players have 200 HP or more (slots 0 and 2).
        let strong = Asm::act_on(|a| a.val(2, 0x20A0).val(3, 0x4160).op(op::MASK).k2(200).op(op::GE).op(op::BIT_COUNT), 1);
        assert_eq!(targets_of(&strong), 2);
        // Of a set only its first value is looked at (HP 300 = 0b1_0010_1100: four bits).
        let first = Asm::act_on(|a| a.val(2, 0x20A0).val(3, 0x4160).op(op::MASK).op(op::BIT_COUNT), 1);
        assert_eq!(targets_of(&first), 4);
        // The usual test: "am I the only one of my side left?" (the allies of slot 4 are the living foes: 4 and 5).
        let alone = Asm::act_on(|a| a.val(2, 0x2080).op(op::BIT_COUNT).k1(1).op(op::EQ), 1);
        assert_eq!(targets_of(&alone), 0, "slot 5 lives too");
        assert_eq!(go(&Asm::default().op(op::BIT_COUNT)), Err(AiError::StackUnderflow));
        assert_eq!(go(&Asm::default().addr(1, 0x0000).op(op::BIT_COUNT).op(op::END)), Err(AiError::Type(op::BIT_COUNT)));

        // BIT is 1 << n as a word: a slot's mask; out of the word's reach it is nothing.
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(0).op(op::BIT), 1)), 1);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(3).op(op::BIT), 1)), 8);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(15).op(op::BIT), 1)), 0x8000);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k1(16).op(op::BIT), 1)), 0);
        assert_eq!(targets_of(&Asm::act_on(|a| a.k2(300).op(op::BIT), 1)), 0);
        // "Aim at the actor whose index I computed": the mask of slot 2 + 1.
        let computed = Asm::act_on(|a| a.k1(2).k1(1).op(op::ADD).op(op::BIT), 1);
        assert_eq!(targets_of(&computed), 0b1000);
        // Its pop is any value: the first value of a set, and BIT of BIT_COUNT chains.
        let chained = Asm::act_on(|a| a.val(2, 0x20A0).op(op::BIT_COUNT).op(op::BIT), 1);
        assert_eq!(targets_of(&chained), 1 << 3);
        assert_eq!(go(&Asm::default().op(op::BIT)), Err(AiError::StackUnderflow));
    }

    #[test]
    fn the_immune_to_death_flag_is_the_owners_to_set_and_nobody_elses() {
        // me.immune_death = 1, as the Initialize scripts of the real enemies do.
        let set = Asm::default().addr(2, 0x2060).addr(0, 0x402C).op(op::MASK).k1(1).op(op::STORE).op(op::END);
        let mut mem = Memory::new();
        assert!(!mem.immune_death());
        run_asm(&set, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert!(mem.immune_death());
        // It reads back (the world is not asked for the owner's), and survives to the next run.
        let read = Asm::act_on(|a| a.val(2, 0x2060).val(0, 0x402C).op(op::MASK), 1);
        let out = run_asm(&read, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert_eq!(out.actions[0].targets, 1);
        // Storing 0 clears it.
        let clear = Asm::default().addr(2, 0x2060).addr(0, 0x402C).op(op::MASK).k1(0).op(op::STORE).op(op::END);
        run_asm(&clear, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap();
        assert!(!mem.immune_death());
        // It is no change to the battle, and another actor's flag cannot be set.
        assert!(run_asm(&set, &Fixed::new(4), &mut mem, &mut Rng::new(1)).unwrap().effects.is_empty());
        let other = Asm::default().k1(1).addr(0, 0x402C).op(op::MASK).k1(1).op(op::STORE).op(op::END);
        assert_eq!(go(&other), Err(AiError::Address(0x402C)));
        // A fresh memory is not immune.
        assert!(!Memory::new().immune_death());
    }

    #[test]
    fn what_the_machine_does_not_model_is_an_error_not_a_guess() {
        // Reading an unmodelled field, an unmodelled global, an unsupported opcode, a bad stack.
        let field = Asm::act_on(|a| a.val(2, 0x20A0).val(2, 0x4999).op(op::MASK), 1);
        assert_eq!(go(&field), Err(AiError::Address(0x4999)));
        let global = Asm::act_on(|a| a.val(2, 0x2110), 1);
        assert_eq!(go(&global), Err(AiError::Address(0x2110)));
        let local = Asm::act_on(|a| a.val(2, 0x0800), 1);
        assert_eq!(go(&local), Err(AiError::Address(0x0800)));
        assert_eq!(
            go(&Asm::default().op(op::LINK).op(op::END)),
            Err(AiError::Unsupported(op::LINK))
        );
        assert_eq!(go(&Asm::default().op(op::ADD)), Err(AiError::StackUnderflow));
        assert_eq!(
            go(&Asm::default().k1(1).op(op::MASK)),
            Err(AiError::StackUnderflow)
        );
        assert_eq!(
            go(&Asm::default().k1(1).k1(2).op(op::MASK).op(op::END)),
            Err(AiError::Type(op::MASK))
        );
        // An empty set cannot be tested (the original's behaviour is unknown).
        let empty = Asm::default()
            .k1(0)
            .val(3, 0x4160)
            .op(op::MASK)
            .op(op::LNOT)
            .op(op::END);
        assert_eq!(go(&empty), Err(AiError::EmptySet(op::LNOT)));
        // Running off the end of the block, and a jump out of it.
        assert!(matches!(
            go(&Asm::default().k1(1)),
            Err(AiError::Decode(DecodeError::Truncated))
        ));
        assert_eq!(
            go(&Asm::default().jump(op::JUMP, 0x400)),
            Err(AiError::BadJump(0x400))
        );
        // No such script.
        let scripts = Asm::default().op(op::END).block();
        assert_eq!(
            run(
                &scripts,
                script::slot::GENERAL_COUNTER,
                &Fixed::new(4),
                &mut Memory::new(),
                &mut Rng::new(1)
            ),
            Err(AiError::NoScript)
        );
    }

    #[test]
    fn an_endless_loop_and_a_deep_stack_hit_their_limits() {
        assert_eq!(go(&Asm::default().jump(op::JUMP, 0)), Err(AiError::Budget));
        let mut deep = Asm::default();
        for _ in 0..=MAX_STACK {
            deep = deep.k1(1);
        }
        assert_eq!(go(&deep.op(op::END)), Err(AiError::StackOverflow));
    }

    #[test]
    fn random_bytes_never_panic_the_decoder_or_the_machine() {
        // A fixed xorshift stream: random blocks of random code, run with a small cast.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        const LIKELY: [u8; 15] = [
            0x01, 0x02, 0x12, 0x60, 0x61, 0x80, 0x82, 0x90, 0x92, 0x70, 0x72, 0x73, 0x40, 0x34,
            0x52,
        ];
        let mut finished = 0;
        for _ in 0..3000 {
            let len = (next() % 160) as usize;
            let mut code: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // Bias toward real opcodes so that scripts get further than their first byte.
            for b in &mut code {
                if next() % 3 != 0 {
                    *b = LIKELY[(next() % 15) as usize];
                }
            }
            let mut block = vec![0xFF; script::HEADER_BYTES];
            block[2..4].copy_from_slice(&(script::HEADER_BYTES as u16).to_le_bytes());
            block.extend_from_slice(&code);
            let scripts = EnemyScripts::parse_block(&block);
            let _ = scripts.reachable(script::slot::MAIN);
            let mut mem = Memory::new();
            let mut rng = Rng::new(next());
            if run(&scripts, script::slot::MAIN, &Fixed::new(4), &mut mem, &mut rng).is_ok() {
                finished += 1;
            }
        }
        assert!(finished > 0, "some random scripts do run to an END");
    }

    #[test]
    fn what_is_implemented_is_what_decodes_to_something_the_machine_runs() {
        assert!(implements(&Insn::Simple(op::STORE)));
        for opcode in [op::NOT, op::BIT_COUNT, op::BIT] {
            assert!(implements(&Insn::Simple(opcode)), "{opcode:#04x}");
        }
        assert!(!implements(&Insn::Simple(op::LINK)));
        assert!(!implements(&Insn::Debug { len: 3 }));
        let known = |addr: u16, size: u8| addr == 0x4160 && size == 3;
        let value = |size, addr| Insn::PushValue { size, addr };
        let place = |size, addr| Insn::PushAddr { size, addr };
        assert_eq!(unsupported_address(&value(3, 0x4160), known), None);
        assert_eq!(unsupported_address(&value(3, 0x4161), known), Some(0x4161));
        assert_eq!(unsupported_address(&place(2, 0x4100), known), None);
        assert_eq!(unsupported_address(&place(0, 0x402C), known), None, "the owner's immunity flag is a store target");
        assert_eq!(unsupported_address(&place(0, 0x402D), known), Some(0x402D), "the dead flag is not");
        assert_eq!(unsupported_address(&place(2, 0x4160), known), Some(0x4160));
        assert_eq!(unsupported_address(&value(2, 0x2060), known), None);
        assert_eq!(unsupported_address(&value(2, 0x2070), known), None);
        assert_eq!(unsupported_address(&value(2, 0x2110), known), Some(0x2110));
        assert_eq!(unsupported_address(&value(1, 0x00E0), known), None);
        assert_eq!(unsupported_address(&value(1, 0x0400), known), Some(0x0400));
        assert_eq!(unsupported_address(&Insn::Simple(op::END), known), None);
    }
}
