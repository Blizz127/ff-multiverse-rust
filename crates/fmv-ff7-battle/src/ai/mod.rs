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
//!   Initialize script runs, if it has one: it sets up the enemy's variables and may change its stats (its
//!   defense ratings, attack and magic strength, evasion: Guard Scorpion changes its defenses).
//! * When its gauge is full ([`choose`]) its Main script runs against the battle as it is. The script may
//!   queue attacks (`ACT`, each on the mask of targets of that moment: a "double attack" is two of them),
//!   which the engine carries out one after the other in the order queued, or end without one: then the
//!   enemy **passes its turn** (1st Ray attacks every other turn this way). The variables the script wrote
//!   stay in the enemy ([`EnemyAi`]) for its next turn.
//! * When it is hit ([`react`]) its counter scripts run, see "Counters".
//! * **Anything the machine does not understand, and any answer the engine cannot carry out, makes the
//!   turn use today's behaviour** (a random living party member, one of the enemy's attacks): an unknown
//!   opcode, an address or an actor field that is not modelled, a budget overrun ([`vm::MAX_STEPS`]), an
//!   attack the scene does not define, a target mask that is empty or names no actor, queued attacks the
//!   enemy cannot pay for, a battle that does not fit the script slots. [`choose`] then returns `None`
//!   and changes nothing, and an enemy whose Initialize script fails stays on the plain behaviour for good.
//!   Nothing here panics or loops. (A one-target attack whose target has fallen by the time it comes up is
//!   not such a case: it fizzles, see [`crate::rules`] "Enemy turns".)
//!
//! # Counters
//!
//! The original runs an enemy's other scripts when it is hit. The Qhimm wiki's list of "the order of scripts
//! executed" (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes#AI_Data>) puts them after an
//! attack, for every target of it: "Death Counter (If script owner died, execution stops here)", then "General
//! Counter (Executed by all targets)", then "Physical Counter/Magical Counter (Executed by all targets
//! depending on damage type)". Any attack the script queues in answer goes into the same action queue
//! as every other (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Mechanics>, where "limits/counters" have
//! priority 0, ahead of the player's spells), so it is played at once, after the blow.
//!
//! The engine does the same, deterministically, from a helper that [`crate::rules::BattleState::submit_command`]
//! calls once a party member's action has been played on all its targets (nothing here runs inside an enemy's
//! own turn: enemies do not hit enemies):
//!
//! 1. every enemy among the targets that has scripts takes note of who acted on it ([`Memory::note_hit`]: the
//!    slot mask of the attacker, as "the last actor to attack it", "...physically" and "...with magic", the
//!    fields `0x40D0`, `0x40E0` and `0x40F0` that the counters and 34 of the Main scripts read), whether or not it
//!    has a counter to answer with; a healing action starts nothing, and nothing runs once the battle is over;
//! 2. if the blow killed it, its Death counter runs, and no other; else its General counter runs, then its
//!    Physical counter if the action was [physical](crate::action::ActionKind::Physical) or its Magical one
//!    if it was magical (limits and items count as the kind of their action);
//! 3. each counter is run with [`react`], like Main with [`choose`]: on a copy of the battle's random stream,
//!    kept only if the counter can be followed and carried out, and then what it queued is played as an enemy
//!    turn is played (announced by [`BattleEvent::Action`](crate::rules::BattleEvent::Action), the whole party
//!    at once for a whole-party attack) inside the batch of events of the command. A counter that cannot be
//!    followed is no counter: nothing is drawn, changed or announced ([`AiStats::counters_skipped`]).
//!
//! Because a counter is a pure function of the engine's state and its random stream, applied at one fixed point
//! of [`crate::rules::BattleState::submit_command`], a peer that replays the same commands at the same ticks gets the same events
//! (`counters_replay_exactly_from_the_commands_alone` replays a server's messages the way the protocol's
//! `BattleReplay` does). Not run: the Pre-action script (slot 7, which "occurs on all participants prior to any
//! actions") and the Battle-end and custom-event scripts.
//!
//! # Opcodes and addresses implemented
//!
//! Push value / address / constant, add, subtract, multiply, divide, remainder, AND, OR, bitwise NOT,
//! the six comparisons, logical AND / OR / NOT, the three jumps and `END`, `MASK`, random word, random
//! bit, bit count, greatest, least, MP cost, one-bit, store, drop, act (command `0x20`, and the text
//! pause `0x24`), and the text instruction. Not implemented (the run fails, see [`vm::implements`]): the
//! character-AI commands (`LINK`, copy, memory bank, elemental defense), the debug print, and `ACT` with
//! command `0x22` (run another script). The addresses are listed in [`vm`] and [`world`]; all other
//! globals and fields end the run.
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
//!   order; the original runs it "before combat" ("Players first, then enemies, then formation",
//!   <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes#AI_Data>). Many scripts draw random numbers
//!   there (to start a counter at a random value): they consume the battle's stream at that point, which is
//!   part of the draw order ([`crate::rules`], "RNG draw order").
//! * Only the Initialize, Main and counter scripts run. Pre-action, Battle-end, the custom events and the
//!   formation's own scripts are not run. A counter reads what the engine models of the blow: who struck
//!   (`0x40D0` ...), not the command or the action that struck (`0x2000`, `0x2008`, `0x2150`, `0x2120`), the
//!   flags of the blow (`0x4027`, "the attack lands") or the statuses it inflicts; a counter that reads them
//!   cannot be followed and is skipped.
//! * Statuses other than Death are not modelled by the engine, so the scripts read them as clear.
//! * An attack may be one the scene defines (its table); an attack the scene does not define falls back for
//!   that turn. (Attacks below `0x100` are the kernel's standard spells, which a scene that uses one carries
//!   its own copy of: Mono Drive's Fire plays.) An attack the scene defines but the rules cannot play (no
//!   damage, healing, a status) is announced and has no effect.
//! * A target mask of several actors is [`Target::AllEnemies`] / [`Target::AllAllies`] only when it is
//!   exactly the living side; any other mix falls back. The engine plays [`Target::AllEnemies`] (the whole
//!   party) as one attack announced once and played on each living member in turn by the whole-side rules
//!   of protocol v10 (`resolve_action`: each target with its own rolls, the 2/3 split of damage when the
//!   action splits). This engine treats the script's mask as authoritative (an engine choice, not checked
//!   against the original): the attack record's own
//!   targeting (`Aim`, read as the rule of the player's target menu, which `aim_at` checks a command against) is not
//!   consulted, so a one-target attack aimed at the whole party and a whole-side one aimed at a single member
//!   are played as the script says (measured on the owner's data: refusing what a command could not do would
//!   make five more enemies fall back to the plain behaviour). [`Target::AllAllies`] and a single enemy as the target
//!   of a damaging attack fall back (an attack the rules cannot play anyway is only announced, whatever its
//!   target). An attack whose record aims at a random target is played on one member.

pub mod script;
pub mod vm;
pub mod world;

#[cfg(test)]
mod asm;

use std::sync::Arc;

use crate::rng::Rng;
use crate::rules::{ActorId, BattleState};

pub use crate::rules::Target;
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

/// The attack and the target a script chose. The target is the engine's own [`Target`] (protocol v10), named
/// from the point of view of the actor that uses the attack: an enemy's "enemies" are the party members.
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
    /// Set the physical evasion of `actor`.
    Evade { actor: ActorId, value: u32 },
    /// Set the physical attack strength of `actor`.
    Attack { actor: ActorId, value: u32 },
    /// Set the magical attack strength of `actor`.
    MagicAttack { actor: ActorId, value: u32 },
}

/// What an enemy's turn came to when a script ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTurn {
    /// The attacks the script queued and their targets, in the order it queued them (the engine carries them
    /// out one after the other, see [`vm`], "The action queue"); empty: the script ended without one and the
    /// enemy passes its turn.
    pub actions: Vec<EnemyChoice>,
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

/// What an enemy's scripts came to over a battle, kept beside them for tests and measurements (no script can
/// read it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AiStats {
    /// Turns its Main script played.
    pub turns: u32,
    /// Turns it could not be followed on, which the plain behaviour played instead.
    pub fallbacks: u32,
    /// Counter scripts that played.
    pub counters: u32,
    /// Counter scripts that could not be followed (no counter came of the blow).
    pub counters_skipped: u32,
}

/// An enemy's scripts and the memory they keep through the battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnemyAi {
    scripts: Arc<EnemyScripts>,
    attacks: Arc<[AiAttack]>,
    memory: Memory,
    stats: AiStats,
}

impl EnemyAi {
    /// An enemy with `scripts`, which knows the attacks of its scene.
    pub fn new(scripts: Arc<EnemyScripts>, attacks: Arc<[AiAttack]>) -> Self {
        Self {
            scripts,
            attacks,
            memory: Memory::new(),
            stats: AiStats::default(),
        }
    }

    /// What the scripts came to so far.
    pub fn stats(&self) -> AiStats {
        self.stats
    }

    pub(crate) fn note_turn(&mut self) {
        self.stats.turns += 1;
    }

    pub(crate) fn note_fallback(&mut self) {
        self.stats.fallbacks += 1;
    }

    pub(crate) fn note_counter(&mut self) {
        self.stats.counters += 1;
    }

    pub(crate) fn note_counter_skipped(&mut self) {
        self.stats.counters_skipped += 1;
    }

    /// Take note that the actor with slot mask `attacker` acted on this enemy ([`Memory::note_hit`]).
    pub(crate) fn note_hit(&mut self, attacker: u16, physical: bool) {
        self.memory.note_hit(attacker, physical);
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
            let (slot, value, change): (u8, u32, fn(ActorId, u32) -> Change) = match *effect {
                Effect::Defense { slot, value } => (slot, value, |actor, value| Change::Defense { actor, value }),
                Effect::MagicDefense { slot, value } => (slot, value, |actor, value| Change::MagicDefense { actor, value }),
                Effect::Evade { slot, value } => (slot, value, |actor, value| Change::Evade { actor, value }),
                Effect::Attack { slot, value } => (slot, value, |actor, value| Change::Attack { actor, value }),
                Effect::MagicAttack { slot, value } => (slot, value, |actor, value| Change::MagicAttack { actor, value }),
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
        if !out.actions.is_empty() {
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
    run_slot(state, idx, script::slot::MAIN, rng)
}

/// Run the counter script in `slot` ([`script::slot::GENERAL_COUNTER`], [`script::slot::DEATH_COUNTER`],
/// [`script::slot::PHYSICAL_COUNTER`] or [`script::slot::MAGICAL_COUNTER`]) of the enemy at position `idx`,
/// which has just been hit, and say what it does: the attacks it queues are the enemy's answer to the blow.
/// Everything [`choose`] says holds, with "counter" for "turn": `None` is no counter (the enemy has no such
/// script, it cannot be followed, or its answer cannot be carried out) and changes nothing. The caller has
/// already told the enemy who hit it ([`Memory::note_hit`]), which is what these scripts read.
pub fn react(state: &BattleState, idx: usize, slot: usize, rng: &mut Rng) -> Option<AiTurn> {
    use script::slot;
    matches!(
        slot,
        slot::GENERAL_COUNTER | slot::DEATH_COUNTER | slot::PHYSICAL_COUNTER | slot::MAGICAL_COUNTER
    )
    .then(|| run_slot(state, idx, slot, rng))
    .flatten()
}

/// Run the script in `slot` of the enemy at `idx` ([`choose`] and [`react`]).
fn run_slot(state: &BattleState, idx: usize, slot: usize, rng: &mut Rng) -> Option<AiTurn> {
    let ai = state.actors().get(idx)?.ai.as_ref()?;
    if ai.memory().phase != Phase::Ready {
        return None;
    }
    let world = BattleWorld::new(state, idx, ai.attacks())?;
    let mut memory = ai.memory().clone();
    let out = vm::run(ai.scripts(), slot, &world, &mut memory, rng).ok()?;
    let actions = out
        .actions
        .iter()
        .map(|&action| resolve(state, &world, ai, action))
        .collect::<Option<Vec<_>>>()?;
    let changes = changes_of(&out.effects, &world)?;
    Some(AiTurn {
        actions,
        memory,
        changes,
    })
}

/// Turn a queued action into an attack and a target. `None` (the turn falls back) for an attack the scene
/// does not define, a mask that names no actor, a slot that holds none, or several actors that are not exactly
/// the living side of one team. **One actor is a [`Target::One`] whether it is alive or not**: whether the
/// attack can be played on it is the engine's business, which knows what the attack is (a script may keep a
/// mask in its memory from a turn ago, and the actor in it may have fallen since; an attack with no effect,
/// such as the Death counter's own animation, has the dead owner as its target).
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
            chosen.push(slots.actor_of(slot)?);
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
mod counter_tests;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod status_tests;
#[cfg(test)]
mod tests;
