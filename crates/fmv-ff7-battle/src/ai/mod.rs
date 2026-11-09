//! Enemy AI: the real FF7 battle scripts of `scene.bin`, run by a small interpreter.
//!
//! In the original every enemy has up to sixteen scripts (Initialize, Main, counters, ...) written in a
//! stack bytecode. Main runs when the enemy's gauge is full and decides what it does: which of its attacks,
//! on whom, whether to skip its turn. This module reads those scripts from the scene
//! ([`script::SceneAi`], kept by [`crate::scene::Scene::ai`]) and runs them
//! ([`vm`]) against the battle ([`world`]).
//!
//! | module | purpose |
//! |---|---|
//! | [`script`] | where the scripts are in a scene, the instruction decoder, reachability |
//! | [`vm`] | the bounded stack machine: opcodes, memory, the step budget, the errors |
//! | [`world`] | actor slots, the masks and the fields of each actor, from a [`BattleState`] |
//!
//! # What an enemy does with its script
//!
//! * At the start of the battle ([`initialize`], called by [`BattleState`] once the battle is built) its
//!   Initialize script runs, if it has one: it sets up the enemy's variables and may change its
//!   defense ratings (Guard Scorpion does).
//! * When its gauge is full ([`choose`]) its Main script runs against the battle as it is. The script may
//!   queue one attack (`ACT`) on a mask of targets, or end without one: then the enemy **passes its turn**
//!   (1st Ray attacks every other turn this way). The variables the script wrote stay in the enemy
//!   ([`EnemyAi`]) for its next turn.
//! * **Anything the machine does not understand, and any answer the engine cannot carry out, makes the
//!   turn use today's behaviour** (a random living party member, one of the enemy's attacks): an unknown
//!   opcode, an address or an actor field that is not modelled, a budget overrun ([`vm::MAX_STEPS`]), an
//!   attack the scene does not define, a target mask that is empty or names the dead, a battle that does
//!   not fit the script slots. [`choose`] then returns `None` and changes nothing, and an enemy whose
//!   Initialize script fails stays on the plain behaviour for good. Nothing here panics or loops.
//!
//! # Opcodes and addresses implemented
//!
//! Exactly the set the first reactor's enemies (MP, Guard Hound, Mono Drive, Grunt, 1st Ray, Sweeper, Guard
//! Scorpion) use, plus the arithmetic that is trivially the same: push value / address / constant,
//! add, subtract, multiply, divide, remainder, AND, OR, the six comparisons, logical AND / OR / NOT, the three
//! jumps and `END`, `MASK`, random word, random bit, greatest, least, MP cost, store, drop, act (command
//! `0x20`, and the text pause `0x24`), and the text instruction. Not implemented (the run fails, see
//! [`vm::implements`]): bitwise NOT, bit count, one-bit, the character-AI commands (`LINK`, copy,
//! memory bank, elemental defense), the debug print, and `ACT` with command `0x22` (run another script).
//! The addresses are listed in [`vm`] and [`world`]; all other globals and fields end the run.
//!
//! # Determinism
//!
//! A run reads only the battle ([`BattleState`], through [`world::BattleWorld`]), the enemy's own memory
//! and the battle's [`Rng`]: `RANDOM` is one `next_u16()` draw and `RANDOM_BIT` one `below(n)` draw when the
//! mask has two or more bits. Server and clients that replay the same battle therefore choose the same.
//! The draw order is part of the engine's contract: see "RNG draw order" in [`crate::rules`].
//!
//! # Approximations
//!
//! * The Initialize script runs when the battle is built, after the starting gauges are drawn, in enemy
//!   order; the original runs it "before combat". Scripts that draw random numbers there would consume the
//!   battle's stream at that point (the supported ones do not).
//! * Only the Main and Initialize scripts run. The counters (General, Death, Physical, Magical), Pre-action,
//!   Battle-end and the formation's own scripts are not run, so an enemy that reacts to being hit does not.
//! * Statuses other than Death are not modelled by the engine, so the scripts read them as clear.
//! * An attack may be one the scene defines (its table). Attacks below `0x100` are the kernel's standard
//!   spells, which the scene does not carry: a script that casts one (Mono Drive's Fire) falls back for that
//!   turn. An attack the scene defines but the rules cannot play (no damage, healing) is announced and has no
//!   effect.
//! * A target mask of several actors is [`Target::AllEnemies`] / [`Target::AllAllies`] only when it is
//!   exactly the living side; any other mix falls back.

pub mod script;
pub mod vm;
pub mod world;

#[cfg(test)]
mod asm;

use std::sync::Arc;

use crate::rng::Rng;
use crate::rules::{ActorId, BattleState};

pub use script::{EnemyScripts, SceneAi};
pub use vm::{AiError, Effect, Memory, Phase};

use world::BattleWorld;

/// An attack the scene defines, as the scripts need to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiAttack {
    /// The attack id of the scene's table.
    pub id: u16,
    /// Its MP cost.
    pub mp_cost: u16,
}

/// Whom an enemy's attack is aimed at, from the point of view of the actor that uses it (an enemy's
/// "enemies" are the party members). This mirrors the whole-side targeting type of the engine's protocol
/// v8; until that lands on this branch it is a local copy and the engine only carries out `One`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    One(ActorId),
    AllEnemies,
    AllAllies,
}

/// The attack and the target a script chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnemyChoice {
    /// The attack id (the scene's attack table).
    pub attack: u16,
    pub target: Target,
}

/// A change a script made to the battle, for the engine to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Set the physical defense rating of `actor`.
    Defense { actor: ActorId, value: u32 },
    /// Set the magical defense rating of `actor`.
    MagicDefense { actor: ActorId, value: u32 },
}

/// What an enemy's turn came to when a script ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTurn {
    /// The attack and target; `None`: the script ended without one and the enemy passes its turn.
    pub choice: Option<EnemyChoice>,
    /// The enemy's memory after the run, to be stored back ([`EnemyAi::set_memory`]).
    pub memory: Memory,
    /// What the script changed in the battle.
    pub changes: Vec<Change>,
}

/// What an Initialize script came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub memory: Memory,
    pub changes: Vec<Change>,
}

/// An enemy's scripts and the memory they keep through the battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnemyAi {
    scripts: Arc<EnemyScripts>,
    attacks: Arc<[AiAttack]>,
    memory: Memory,
}

impl EnemyAi {
    /// An enemy with `scripts`, which knows the attacks of its scene.
    pub fn new(scripts: Arc<EnemyScripts>, attacks: Arc<[AiAttack]>) -> Self {
        Self {
            scripts,
            attacks,
            memory: Memory::new(),
        }
    }

    pub fn scripts(&self) -> &EnemyScripts {
        &self.scripts
    }

    pub fn attacks(&self) -> &[AiAttack] {
        &self.attacks
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Store the memory a run left.
    pub fn set_memory(&mut self, memory: Memory) {
        self.memory = memory;
    }

    /// Give the scripts up for good: this enemy uses the plain behaviour from now on.
    pub fn disable(&mut self) {
        self.memory.phase = Phase::Disabled;
    }
}

fn changes_of(effects: &[Effect], world: &BattleWorld<'_>) -> Option<Vec<Change>> {
    effects
        .iter()
        .map(|effect| {
            let (slot, change): (u8, fn(ActorId, u32) -> Change) = match *effect {
                Effect::Defense { slot, .. } => (slot, |actor, value| Change::Defense { actor, value }),
                Effect::MagicDefense { slot, .. } => {
                    (slot, |actor, value| Change::MagicDefense { actor, value })
                }
            };
            let value = match *effect {
                Effect::Defense { value, .. } | Effect::MagicDefense { value, .. } => value,
            };
            let idx = world.slots().actor_of(slot)?;
            Some(change(ActorId(u8::try_from(idx).ok()?), value))
        })
        .collect()
}

/// Run the Initialize script of the enemy at position `idx` of the battle. `None` when the enemy has no
/// scripts, they cannot be followed, or the battle does not fit the script slots: the caller then
/// disables the enemy's scripts ([`EnemyAi::disable`]). An enemy without an Initialize script starts at
/// once.
pub fn initialize(state: &BattleState, idx: usize, rng: &mut Rng) -> Option<Started> {
    let ai = state.actors().get(idx)?.ai.as_ref()?;
    let world = BattleWorld::new(state, idx, ai.attacks())?;
    let mut memory = ai.memory().clone();
    let mut changes = Vec::new();
    if ai.scripts().entry(script::slot::INITIALIZE).is_some() {
        let out = vm::run(ai.scripts(), script::slot::INITIALIZE, &world, &mut memory, rng).ok()?;
        if out.action.is_some() {
            return None;
        }
        changes = changes_of(&out.effects, &world)?;
    }
    memory.phase = Phase::Ready;
    Some(Started { memory, changes })
}

/// Run the Main script of the enemy at position `idx` of the battle and say what it does.
///
/// `None`: the enemy has no scripts, or no Main script, they are not ready or were disabled, the run
/// failed, or its answer cannot be carried out; the caller uses the plain behaviour and nothing is
/// changed (`rng` was advanced only if the answer is returned: the caller passes a copy and keeps it on
/// `Some`).
pub fn choose(state: &BattleState, idx: usize, rng: &mut Rng) -> Option<AiTurn> {
    let ai = state.actors().get(idx)?.ai.as_ref()?;
    if ai.memory().phase != Phase::Ready {
        return None;
    }
    let world = BattleWorld::new(state, idx, ai.attacks())?;
    let mut memory = ai.memory().clone();
    let out = vm::run(ai.scripts(), script::slot::MAIN, &world, &mut memory, rng).ok()?;
    let choice = match out.action {
        None => None,
        Some(action) => Some(resolve(state, &world, ai, action)?),
    };
    let changes = changes_of(&out.effects, &world)?;
    Some(AiTurn {
        choice,
        memory,
        changes,
    })
}

/// Turn a queued action into an attack and a target the engine can carry out.
fn resolve(
    state: &BattleState,
    world: &BattleWorld<'_>,
    ai: &EnemyAi,
    action: vm::Action,
) -> Option<EnemyChoice> {
    if !ai.attacks().iter().any(|a| a.id == action.attack) {
        return None;
    }
    let slots = world.slots();
    let mut chosen: Vec<usize> = Vec::new();
    for slot in 0..16u8 {
        if action.targets >> slot & 1 == 1 {
            let idx = slots.actor_of(slot)?;
            if !state.actors().get(idx)?.is_alive() {
                return None;
            }
            chosen.push(idx);
        }
    }
    let target = match chosen.as_slice() {
        [] => return None,
        [only] => Target::One(ActorId(u8::try_from(*only).ok()?)),
        many => {
            let living = |range: std::ops::Range<usize>| -> Vec<usize> {
                range
                    .filter(|&i| state.actors().get(i).is_some_and(|c| c.is_alive()))
                    .collect()
            };
            let party = slots.party();
            let (party_alive, foes_alive) = (
                living(0..party),
                living(party..party + slots.enemies()),
            );
            let mine_is_party = world.me_idx() < party;
            let (opposing, own) = if mine_is_party {
                (&foes_alive, &party_alive)
            } else {
                (&party_alive, &foes_alive)
            };
            if many == opposing.as_slice() {
                Target::AllEnemies
            } else if many == own.as_slice() {
                Target::AllAllies
            } else {
                return None;
            }
        }
    };
    Some(EnemyChoice {
        attack: action.attack,
        target,
    })
}

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
