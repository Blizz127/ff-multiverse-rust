//! Battle on the wire (protocol v3) and the deterministic setup both peers share.
//!
//! # What travels
//!
//! A shared battle is server-authoritative and client-replayed. The server starts a
//! [`fmv_ff7_battle::rules::BattleState`] and announces it with `ServerMsg::EncounterStart`: the
//! formation id (`scene * 4 + slot`), the RNG seed, who fights as which character at which level,
//! the start flags, the engine pace and a [`Fingerprint`] of the battle data the server built it
//! from. **No stat, enemy record or other game data is ever sent.** Every peer rebuilds the same
//! combatants from its OWN `scene.bin` and `kernel.bin` through [`BattleRules::build`] (for FF7,
//! [`Ff7BattleData`]); the engine is a pure function of those inputs, so a peer that owns the same
//! edition computes the same numbers. The wire carries the engine's own serde types
//! ([`BattleEvent`], [`Command`], [`Outcome`], [`ActorId`]), pinned by golden tests in this crate.
//!
//! # The input log
//!
//! The engine only changes through two operations: advance by whole ticks, and submit one party
//! command. `ServerMsg::BattleEvent { tick, events, command }` is one such operation together with
//! its result, and `tick` is the engine's tick counter AFTER it:
//!
//! * `command: None`: the engine advanced to `tick`; `events` is what that produced (the server
//!   only sends such a message when it produced something, so ticks that were silent are folded
//!   into the next message).
//! * `command: Some(c)`: the engine first advanced (silently) to `tick`, then `c` was applied at
//!   that tick; `events` is what the command produced (including any start events that had not
//!   been delivered yet).
//!
//! Replaying that log against a fresh engine reproduces the server's events exactly; a client
//! that sees a difference has diverged and must say so loudly. `ServerMsg::BattleEnd` always
//! follows the message that carried `Victory` or `Defeat`.
//!
//! # Commands (v6)
//!
//! A party member whose gauge is full gives one [`Command`]: `Attack`, `Magic { spell, target }` (a
//! spell it knows, which costs MP), `Item { item, target }` or `Defend` (the `target` of a spell, an item or
//! a limit is a [`Target`] since v10: one actor or a whole side, see "Whole-side targeting"). The server checks it with the
//! engine (`BattleState::check_command`: gauge, target, MP, spell and item known) and, for an item, that
//! the account owns one: the **engine knows no inventory**, so the server checks the count before it
//! submits and takes the item only when the engine accepted the command. A client replays the very same
//! commands from its own data and never needs a count (it learns its own from `ServerMsg::Inventory`).
//! Every action is announced by a [`BattleEvent::Action`] before its effects.
//!
//! # Limit breaks and escape (v7)
//!
//! * **Limit.** Each character has a limit gauge `0..=255` that enemy blows fill and a knockout or a limit
//!   empties (`fmv_ff7_battle::limit` has the rule). It is engine state, a pure function of the damage
//!   dealt, so every peer's replay has the same gauge; its **starting value is an input** like the level:
//!   [`BattleMember::limit`] carries the account's own, which the server keeps (in `fmv-persist`'s
//!   `CharacterProgress`) and saves at the end of every battle, whatever the outcome (a server run with
//!   the debug flag `--debug-limit-full` instead starts everyone full and saves no gauge). A value above 255
//!   is refused when the battle is built ([`BattleBuildError::InvalidLimit`]). With the gauge and the
//!   ATB gauge full a character gives [`Command::Limit`]: the engine announces
//!   `Action { Limit(id) }`, `LimitGauge { actor, value: 0 }` and then the effects of one action on
//!   one target (an enemy for a damaging limit, an ally for a healing one: `BattleState::limit_for`
//!   says which), so a limit that ends the battle still ends on its `Victory`.
//! * **Escape.** [`Command::Escape`] spends the actor's turn running. Whether and how hard a battle is to
//!   run from comes from the peer's own `scene.bin` ([`BattleRules::build`] gives the engine its
//!   `EscapeRule`; nothing travels). When the party gets away the battle ends with
//!   [`Outcome::Escaped`]: the message that carries the command has `Action { Escape }` and no
//!   terminal event, and `BattleEnd` follows it with no rewards; a failed attempt adds
//!   `EscapeFailed { actor }` and the battle goes on. The server saves the limit gauges of an escaped
//!   battle like any other (none under `--debug-limit-full`), and pays nothing.
//! * **Refusals.** `NoLimit` (the character has none), `LimitNotReady` (its gauge is not full) and
//!   `CannotEscape` (the formation cannot be run from) join the v6 reasons.
//!
//! # The party: canon companions, then the players (v9)
//!
//! `EncounterStart::party` lists every fighter of the player side, `party[i]` fighting as `ActorId(i)`: first
//! the story's CANON party ([`Controller::Companion`]: Cloud and Barret in the first reactor mission), then
//! every player's own character ([`Controller::Player`]) in party order. A player's character is ADDED to the
//! canon party; nobody is put in a canon member's place. A player commands only its own actor. A companion is
//! commanded by the SERVER: when its ATB gauge is full the server picks one ordinary [`Command`] with a fixed
//! rule over the engine's own state and applies it exactly like a player's, so it travels as a `BattleEvent`
//! with the command attached and every client replays and checks it like any other; the engine has no AI for
//! it and a client needs no rule of the server's. The commands a companion gave are therefore part of the log
//! (`BattleEvent::command`), and the two kinds of member can be told apart only by the roster.
//!
//! **Players have no limit break** ([`PLAYER_LIMIT_BREAKS`]): [`BattleRules::build`] gives a player's combatant
//! none, so `BattleState::limit_for` is `None` for it and a peer shows no limit bar. The companions keep theirs
//! and the server's rule uses them.
//!
//! # Whole-side targeting (v10)
//!
//! `Command::Magic`, `Command::Item` and `Command::Limit` carry a [`Target`] instead of an actor:
//! `One(actor)`, `AllEnemies` or `AllAllies` (`Command::Attack` keeps its one `target`). The sides are named
//! from the **commanding actor's** point of view, so a party member's `AllEnemies` is the enemy side and its
//! `AllAllies` the party; an enemy's action (the AI of an enemy asks the same way) reads the other way round.
//! On the wire it is `{"One":3}`, `"AllEnemies"` or `"AllAllies"`, and a bare number is the v9 shape, which a
//! v10 peer does not read. What it takes to aim a command that way is the engine's, from the peer's own data
//! (`fmv_ff7_battle::rules`, "Whole-side targeting"): the record's targeting bits say whether an action hits
//! one target, a whole side or either (Cure, Fire: either; Ultima, Megalixir, Healing Wind: a whole side
//! only), a spell that can do either needs an All materia use of its caster's (a count the engine keeps from
//! the character's data, one per battle at level 1, so it is replayed and never sent), and an action that
//! hits several targets splits its damage by 2/3.
//!
//! * **The party side is the whole roster.** With the canon party in front (see "The party" above) a party
//!   member's `AllAllies` is every fighter of the roster, the canon companions and the players alike (`party[i]`
//!   fights as `ActorId(i)`, so the effects come in roster order), and a player can aim a one-target spell or item at
//!   a companion with `One`. The commands a companion gives (the server's rule: a limit that heals the whole party
//!   is `AllAllies`, a damaging one at one enemy, or `AllEnemies` when that is the only way it is given) have the
//!   same shapes and travel as ordinary `BattleEvent`s with the command attached.
//! * **No new events.** A command on a whole side is one `Action` (and one `MpSpent`), followed by the
//!   effects on each target in ascending actor order: one `Damage`, `Miss`, `Heal`, `MpRestored` or `Revive`
//!   per target (and the `Death` and `LimitGauge` that follow its blow), then `Victory` or `Defeat` when the
//!   last of them ended the battle. The replay checks every one of them like any other event, whole sides
//!   included.
//! * **Refusals.** A way of aiming the action does not allow, a side that is not the one it aims at and a
//!   caster with no All use left are all `InvalidTarget`; a revival of a whole side with nobody fallen is
//!   `TargetNotDead`. The menu offers none of them (it asks the engine), so they only answer a stale or a
//!   hostile command.
//! * An item command still spends one item, however many targets it hits (the server checks the count as it
//!   always did).
//!
//! # Statuses
//!
//! The engine plays Poison, Regen, Sleep, Stop, Paralysis, Silence, Haste, Slow, Barrier, MBarrier, Sadness, Fury and
//! Darkness (`fmv_ff7_battle::status` has the model, the clocks and their sources); an action that inflicts or cures
//! one gets it from the peer's own `kernel.bin` and `scene.bin`, so nothing about a status is ever sent except what
//! the engine itself says happened:
//!
//! * **Three new events.** `StatusInflicted { target, status }` (an action inflicted it, or the battle started with it),
//!   `StatusEnded { target, status }` (a cure, a timer that ran out, or a sleeper woken by a physical blow) and
//!   `StatusTick { target, status, amount }` (a turn of Poison, which took `amount` HP, or of Regen, which gave it). A
//!   `status` travels as its name (`"Poison"`, `"MBarrier"`), a status set as its bit mask (a number: bit `n` is the
//!   `n`th status of `Status::ALL`, as in the game's own records). A v11 peer cannot decode them, so they came with
//!   protocol v12.
//! * **A held turn.** A member that is asleep, stopped or paralysed keeps its full gauge and gives no command: the
//!   engine's `ready_actors()` leaves it out, a command from it is refused (`CannotAct`), and a silenced member's
//!   Magic is (`Silenced`). Both are new [`BattleRejection`]s; the server's own rule for a companion never asks for
//!   either (it checks every candidate command with the engine first).
//! * **Starting statuses are an input**, like the level and the limit gauge: [`BattleMember::status`] is what the
//!   member begins the battle with (the engine keeps the modelled ones, announces them with the first step and gives
//!   the timed ones their whole duration). Today only the server's debug flag `--debug-status` sets it; a real battle
//!   starts clean. A set the engine does not play, or one that holds Haste with Slow or Sadness with Fury (the engine
//!   never has both), is refused when the battle is built ([`BattleBuildError::InvalidStatus`]).
//!
//! # Rewards
//!
//! [`split_rewards`] is a pure function of the engine's total reward and the ROSTER ([`reward_roster`]: every
//! PLAYER of `EncounterStart::party`, with whether the engine says it is alive; the companions are never paid),
//! never of who is still connected, so a client recomputes the very list the server sent.
//!
//! **Item drops are the exception.** The server rolls them AFTER the outcome ([`BattleRules::roll_drops`],
//! from the battle's seed on a stream of its own, logged) and hands them to the living members
//! ([`assign_drops`]); they travel in [`BattleReward::items`]. They are not part of the replayed event
//! stream, so a client cannot recompute them and [`BattleReplay::check_end`] compares everything of a
//! reward but its items.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use fmv_core::{AccountId, EntityId, Fingerprint, InventoryEntry};
use fmv_ff7_battle::action::{ActionSpec, Aim, ItemData, ItemTable, Spell};
use fmv_ff7_battle::kernel::{
    CharacterId, Kernel, KernelError, ITEM_COUNT, MAX_AP, MAX_EXP, MAX_GIL, MAX_ITEM_STACK, MAX_KERNEL_BYTES, MAX_LEVEL,
};
use fmv_ff7_battle::rng::Rng;
use fmv_ff7_battle::limit::LimitBreak;
use fmv_ff7_battle::rules::{
    element, enemies_for_formation, BattleConfig, BattleState, Combatant, EscapeRule, Reward, StartMode, LIMIT_MAX, MAX_PER_SIDE,
};
use fmv_ff7_battle::scene::{BattleLayout, SceneBin, SceneError, MAX_SCENE_BIN_BYTES};
use fmv_ff7_battle::status::StatusState;
use serde::{Deserialize, Serialize};

/// The engine types that travel in battle messages, re-exported so a peer can match on them
/// without a direct dependency.
pub use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent, Command, Outcome, Target};
pub use fmv_ff7_battle::status::{Status, StatusSet};

/// The random stream item drops are rolled on (the engine's own is [`fmv_ff7_battle::rng::DEFAULT_STREAM`]),
/// so drops never depend on, or disturb, the replayed battle.
const DROP_STREAM: u64 = 0x4452_4F50;
/// A drop rate is out of this many (an enemy record's rate byte is `0..=63`).
const DROP_DIE: u32 = 64;

/// Most characters a battle party can have (a protocol bound; the server's party size is smaller).
pub const MAX_BATTLE_PARTY: usize = 8;

/// Identifies one battle on a server (never reused while the server runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BattleId(pub u64);

impl fmt::Display for BattleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "battle#{}", self.0)
    }
}

/// Who gives a battle member's commands (protocol v9). A battle's party is the story's CANON party, commanded
/// by the server, followed by the players' own characters, each commanded by its player: nobody takes a canon
/// member's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Controller {
    /// A player's own original character: only that account's session may command it, and its entity is frozen
    /// in the field while the battle runs. The entity is what a peer finds the player's look by (the snapshot of
    /// the area says how every entity looks).
    Player { entity: EntityId, account: AccountId },
    /// A member of the story's canon party, commanded by the SERVER: when its gauge is full the server chooses
    /// the command with a fixed rule over the engine's own state and applies it like a player's, announcing it in
    /// a `BattleEvent` with the command attached, so every peer replays and checks it (the engine has no AI for
    /// it). It has no account and no entity: it is never paid, never saved and nobody may command it.
    Companion,
}

impl Controller {
    /// The controlling account, `None` for a companion.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Controller::Player { account, .. } => Some(*account),
            Controller::Companion => None,
        }
    }

    /// The player's entity in the field, `None` for a companion.
    pub fn entity(&self) -> Option<EntityId> {
        match self {
            Controller::Player { entity, .. } => Some(*entity),
            Controller::Companion => None,
        }
    }

    pub fn is_player(&self) -> bool {
        matches!(self, Controller::Player { .. })
    }

    pub fn is_companion(&self) -> bool {
        matches!(self, Controller::Companion)
    }
}

/// One fighter of a battle's party: a player's character or a canon companion. Ids, numbers and a name: the
/// stats follow from the peer's own kernel data. `party[i]` of `EncounterStart` fights as `ActorId(i)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleMember {
    /// Who commands this fighter (protocol v9; v8 had the player's `entity` and `account` here).
    pub controller: Controller,
    /// What the party panel, the HP list, the banners and the log of every peer call this fighter (protocol
    /// v8): a player's OWN name, a companion's canon name (Cloud, Barret), instead of the name of the combat
    /// template the engine uses. Display only: the engine never reads it, so it is not part of what a replay
    /// depends on. Clipped by the sender to [`crate::MAX_CHARACTER_NAME_CHARS`] characters, without control
    /// characters.
    pub name: String,
    /// The canon character whose combat template the engine fights this member with: FF7 character ids Cloud 0,
    /// Barret 1, Tifa 2, Aerith 3, Red XIII 4, Yuffie 5, Cait Sith 6, Vincent 7, Cid 8
    /// (`fmv_ff7_battle::kernel::CharacterId::index`). For a companion it is who they are. For a player it is
    /// the character's STAT CURVE ("fighting style"; protocol v8: the combat slot): fixed at creation, it
    /// decides the base stats, the experience curve and the spells, and says nothing about who the player is
    /// (two players may hold the same one, their [`BattleMember::name`]s tell them apart, and a player on
    /// Cloud's curve fights beside the canon Cloud).
    pub character: u8,
    pub level: u8,
    /// Equipment override ids; `None` is the character's starting equipment.
    pub weapon: Option<u8>,
    pub armor: Option<u8>,
    /// The character's limit gauge when the battle starts (protocol v7), `0..=LIMIT_MAX` (255; a
    /// larger number is refused by [`BattleRules::build`]). An input of the engine like the level, so every peer
    /// replays the gauge the server had. Players have no limit break (protocol v9, see [`PLAYER_LIMIT_BREAKS`]),
    /// so a player's value does not matter; a companion starts empty (full under the debug flag
    /// `--debug-limit-full`).
    pub limit: u16,
    /// The statuses the member begins the battle with (see the module docs, "Statuses"): an input of the engine like
    /// the level and the limit gauge, so every peer replays what the server had. Empty in a real battle; the server's
    /// debug flag `--debug-status` sets it. Only statuses the engine plays are valid and never both of a pair that
    /// cancel each other ([`BattleBuildError::InvalidStatus`]). Absent from the JSON when empty.
    #[serde(default, skip_serializing_if = "no_status")]
    pub status: StatusSet,
}

/// `skip_serializing_if` of [`BattleMember::status`] (serde passes a reference).
fn no_status(set: &StatusSet) -> bool {
    set.is_empty()
}

impl BattleMember {
    /// A player's character in a battle (see [`Controller::Player`]).
    pub fn player(entity: EntityId, account: AccountId, name: impl Into<String>, character: u8, level: u8, limit: u16) -> Self {
        Self {
            controller: Controller::Player { entity, account },
            name: name.into(),
            character,
            level,
            weapon: None,
            armor: None,
            limit,
            status: StatusSet::EMPTY,
        }
    }

    /// A canon companion of the story's party in a battle (see [`Controller::Companion`]).
    pub fn companion(name: impl Into<String>, character: u8, level: u8, limit: u16) -> Self {
        Self { controller: Controller::Companion, name: name.into(), character, level, weapon: None, armor: None, limit, status: StatusSet::EMPTY }
    }

    /// The FF7 character, if `character` names one.
    pub fn character_id(&self) -> Option<CharacterId> {
        CharacterId::from_index(usize::from(self.character))
    }

    /// The controlling account, `None` for a companion.
    pub fn account(&self) -> Option<AccountId> {
        self.controller.account()
    }

    /// The player's entity in the field, `None` for a companion.
    pub fn entity(&self) -> Option<EntityId> {
        self.controller.entity()
    }

    pub fn is_player(&self) -> bool {
        self.controller.is_player()
    }

    pub fn is_companion(&self) -> bool {
        self.controller.is_companion()
    }
}

/// Whether a player's character has a limit break (protocol v9: no, for now). The canon companions keep theirs
/// (Braver for the canon Cloud and so on); a player on Cloud's curve would otherwise also give Braver beside
/// him. [`BattleRules::build`] gives a player's combatant none, so a peer's engine has no limit gauge for it and
/// `BattleState::limit_for` is `None`; the stored gauge of an account is left exactly as it is (nothing writes it
/// for a character with no limit). A later version that gives players limits of their own changes this one
/// constant and the rules' use of it.
pub const PLAYER_LIMIT_BREAKS: bool = false;

/// The roster a victory's rewards and drops are split over: every PLAYER of `party`, in party order, with
/// whether `state` says its actor (`ActorId(i)` for `party[i]`) is alive. Companions are never in it: they are not
/// paid. The ONE definition the server (`finish_battle`) and every peer's replay ([`BattleReplay::roster`]) use, so
/// that both split the same list and a client can recompute the very rewards the server sent.
pub fn reward_roster<'a>(party: impl IntoIterator<Item = &'a BattleMember>, state: &BattleState) -> Vec<(AccountId, bool)> {
    party
        .into_iter()
        .enumerate()
        .filter_map(|(i, member)| {
            let account = member.account()?;
            let alive = u8::try_from(i).ok().and_then(|i| state.actor(ActorId(i))).is_some_and(Combatant::is_alive);
            Some((account, alive))
        })
        .collect()
}

/// What one account earned from a won battle (see [`split_rewards`] and [`assign_drops`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleReward {
    pub account: AccountId,
    pub exp: u32,
    pub gil: u32,
    pub ap: u32,
    /// The items dropped to this account: rolled by the server after the battle (not replayable, see
    /// the module docs), at most one entry per item, ascending by item. Absent from the JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<InventoryEntry>,
}

impl BattleReward {
    /// A reward of experience, gil and AP and no items.
    pub fn new(account: AccountId, exp: u32, gil: u32, ap: u32) -> Self {
        Self { account, exp, gil, ap, items: Vec::new() }
    }

    /// Nothing at all: no experience, gil, AP or items.
    pub fn is_nothing(&self) -> bool {
        self.exp == 0 && self.gil == 0 && self.ap == 0 && self.items.is_empty()
    }

    /// The same reward without its items: what a replay of the battle can reproduce.
    pub fn without_items(&self) -> Self {
        Self { items: Vec::new(), ..self.clone() }
    }
}

/// A party command as the server applied it: who, and what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedCommand {
    pub actor: ActorId,
    pub command: Command,
}

/// Why the server refused a battle request or command (`ServerMsg::BattleRejected`). A refusal is
/// routine (a stale command, a debug request on a normal server) and never ends the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattleRejection {
    /// This server cannot run battles: it has no owned battle data (the FIXTURE server never does).
    Unsupported,
    /// `RequestEncounter` while the server runs without `--debug-encounters`.
    DebugDisabled,
    /// You are already in a battle.
    AlreadyInBattle,
    /// The requested formation does not exist (or has no enemies).
    UnknownFormation,
    /// The battle named by a command is not one you are in (unknown, finished, or someone else's).
    NotInBattle,
    /// The actor of a command is not your character.
    NotYourActor,
    /// The actor's ATB gauge is not full.
    GaugeNotFull,
    /// The actor is dead.
    ActorDead,
    /// The target does not exist, or is on the wrong side for the action (damage goes to an enemy,
    /// restoring to an ally), or the action cannot be aimed that way (a whole side where it hits one target, one
    /// target where it hits a whole side), or the actor has no All use left for a whole-side spell (protocol v10).
    InvalidTarget,
    /// The target is already dead.
    TargetDead,
    /// The battle is over.
    BattleOver,
    /// The target is alive, and the action (Life, Phoenix Down) only revives the dead.
    TargetNotDead,
    /// The actor does not know that spell.
    UnknownSpell,
    /// The actor has not the MP for that spell.
    NotEnoughMp,
    /// That is not an item that can be used in battle.
    UnknownItem,
    /// The account has none of that item.
    OutOfItems,
    /// The actor has no limit break (protocol v7).
    NoLimit,
    /// The actor's limit gauge is not full (protocol v7).
    LimitNotReady,
    /// The party cannot run from this battle (protocol v7).
    CannotEscape,
    /// The actor is asleep, stopped or paralysed: it takes no turn until that is over (statuses).
    CannotAct,
    /// The actor is silenced: it cannot cast a spell (statuses).
    Silenced,
}

impl fmt::Display for BattleRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BattleRejection::Unsupported => "this server cannot run battles (it has no owned battle data)",
            BattleRejection::DebugDisabled => "debug encounters are disabled on this server (start it with --debug-encounters)",
            BattleRejection::AlreadyInBattle => "you are already in a battle",
            BattleRejection::UnknownFormation => "no such enemy formation",
            BattleRejection::NotInBattle => "you are not in that battle",
            BattleRejection::NotYourActor => "that character is not yours",
            BattleRejection::GaugeNotFull => "that character's ATB gauge is not full",
            BattleRejection::ActorDead => "that character is dead",
            BattleRejection::InvalidTarget => "that is not a valid target",
            BattleRejection::TargetDead => "that target is already dead",
            BattleRejection::BattleOver => "the battle is over",
            BattleRejection::TargetNotDead => "that target is alive (only the dead can be revived)",
            BattleRejection::UnknownSpell => "that character does not know that spell",
            BattleRejection::NotEnoughMp => "that character has not the MP for that spell",
            BattleRejection::UnknownItem => "that item cannot be used in battle",
            BattleRejection::OutOfItems => "you have none of that item",
            BattleRejection::NoLimit => "that character has no limit break",
            BattleRejection::LimitNotReady => "that character's limit gauge is not full",
            BattleRejection::CannotEscape => "you cannot escape from this battle",
            BattleRejection::CannotAct => "that character cannot act (asleep, stopped or paralysed)",
            BattleRejection::Silenced => "that character is silenced and cannot cast",
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The shared setup
// ---------------------------------------------------------------------------------------------

/// Everything a peer needs to build the engine of a battle: the content of `EncounterStart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleSetup {
    /// Formation id, `scene * 4 + slot`.
    pub formation_id: u16,
    pub seed: u64,
    pub party: Vec<BattleMember>,
    /// The party gets the first move (full gauges); the enemies start empty.
    pub preemptive: bool,
    /// The enemies get the first move; the party starts empty.
    pub back_attack: bool,
    /// FF7 "Battle Speed" configuration (0 fastest, 255 slowest).
    pub battle_speed: u8,
    /// Engine ticks per second of simulated time (the engine clamps it to `1..=1000`).
    pub ticks_per_second: u32,
}

impl BattleSetup {
    /// The gauge start the flags ask for. Both flags set is contradictory and counts as a normal
    /// start (the server never sends it).
    pub fn start_mode(&self) -> StartMode {
        match (self.preemptive, self.back_attack) {
            (true, false) => StartMode::Preemptive,
            (false, true) => StartMode::BackAttack,
            _ => StartMode::Normal,
        }
    }

    /// The engine configuration of this battle.
    pub fn config(&self) -> BattleConfig {
        BattleConfig {
            battle_speed: self.battle_speed,
            ticks_per_second: self.ticks_per_second,
            start: self.start_mode(),
        }
    }
}

/// Why a [`BattleSetup`] cannot be built from the local game data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BattleBuildError {
    #[error("no enemy formation {0}")]
    UnknownFormation(u16),
    #[error("enemy formation {0} has no enemies")]
    NoEnemies(u16),
    #[error("the battle party is empty")]
    EmptyParty,
    #[error("a battle party of {0} is too large (at most {MAX_BATTLE_PARTY})")]
    PartyTooLarge(usize),
    #[error("unknown character id {0}")]
    UnknownCharacter(u8),
    #[error("the local game data has no stats for character id {0}")]
    NoCharacterData(u8),
    /// A starting limit gauge above the most there is (a hostile or broken server; the real one never sends it).
    #[error("character id {character} starts with a limit gauge of {limit}, the most there is is {max}")]
    InvalidLimit { character: u8, limit: u16, max: u16 },
    /// A starting status set the engine does not play, or one that has both Haste and Slow or both Sadness and Fury.
    #[error("character id {character} starts with the status set {bits:#x}, which is not one the battle plays")]
    InvalidStatus { character: u8, bits: u32 },
}

/// The statuses a member begins a battle with, checked: only ones the engine plays, and never both of a pair that
/// cancel each other ([`StatusSet::is_valid_start`]). One definition for every [`BattleRules`].
pub fn start_status(member: &BattleMember) -> Result<StatusState, BattleBuildError> {
    let set = member.status;
    if !set.is_valid_start() {
        return Err(BattleBuildError::InvalidStatus { character: member.character, bits: set.bits() });
    }
    Ok(StatusState::starting(set))
}

/// What a formation looks like to the server (for choosing flags and for logs). Names and stats
/// are deliberately not part of it: they stay in the peer's own files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormationInfo {
    /// The scene's arrival layout says the party gets the first move.
    pub preemptive: bool,
    /// The scene's arrival layout says the enemies get the first move.
    pub back_attack: bool,
    /// How many enemies the formation places.
    pub enemies: usize,
}

/// The most a character's progress can hold in one game: the numbers battle rewards saturate at.
/// The store checks none of them (it is game-agnostic); the server applies them when it adds a
/// reward, so a character can never carry a number its game has no room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressLimits {
    /// The highest level.
    pub max_level: u8,
    /// Most total experience.
    pub max_exp: u32,
    /// Most gil.
    pub max_gil: u32,
    /// Most AP.
    pub max_ap: u32,
    /// The fullest limit gauge (protocol v7): a battle refuses a starting gauge above it.
    pub max_limit: u16,
}

impl ProgressLimits {
    /// FF7's own caps (`fmv_ff7_battle::kernel::MAX_LEVEL`, `MAX_EXP`, `MAX_GIL`, `MAX_AP` and
    /// `fmv_ff7_battle::limit::LIMIT_MAX`).
    pub const FF7: ProgressLimits =
        ProgressLimits { max_level: MAX_LEVEL, max_exp: MAX_EXP, max_gil: MAX_GIL, max_ap: MAX_AP, max_limit: LIMIT_MAX };
}

/// The game's battle rules as one peer's own data provides them. The server and every client call
/// [`BattleRules::build`] with the same [`BattleSetup`] and must get identical engines, which is
/// why this is one trait with one implementation per game rather than code in each binary.
pub trait BattleRules: Send + Sync {
    /// Build the engine of `setup` from the local game data. Pure: the same setup and the same
    /// data always give the same `BattleState`.
    fn build(&self, setup: &BattleSetup) -> Result<BattleState, BattleBuildError>;

    /// Fingerprint of the local battle data (compare with `EncounterStart::data_fingerprint`:
    /// peers with different data cannot replay each other's battles).
    fn data_fingerprint(&self) -> Fingerprint;

    // ---- used by the server when it chooses a battle ----

    /// The formation `id`, or `None` if it does not exist or has no enemies.
    fn formation(&self, id: u16) -> Option<FormationInfo>;

    /// The level `character` fights at when the server does not choose one.
    fn starting_level(&self, character: u8) -> u8;

    /// The formation a debug encounter uses when none is named and the area has no table.
    fn default_formation(&self) -> Option<u16>;

    /// The canon name of character `character` (FF7: Cloud, Barret, ...): what a battle's roster calls a canon
    /// companion (protocol v9), which has no player to name it. `None` for an id that is nobody's, and the default for
    /// a rule set that names nobody: the story's canon party then has no member to put in the battle.
    fn character_name(&self, _character: u8) -> Option<String> {
        None
    }

    // ---- used by the server to keep each player's progress (defaults keep older rule sets valid) ----

    /// Total experience at which `character` reaches `level` (level 1 is 0), or `None` if this rule
    /// set has no experience curve for it: such a character gains experience but never a level.
    fn exp_for_level(&self, _character: u8, _level: u8) -> Option<u32> {
        None
    }

    /// The experience a new account's `character` starts with when it starts at `level`: by default
    /// the least that level takes ([`BattleRules::exp_for_level`]).
    fn starting_exp(&self, character: u8, level: u8) -> u32 {
        self.exp_for_level(character, level).unwrap_or(0)
    }

    /// The caps battle rewards saturate at (FF7's by default).
    fn progress_limits(&self) -> ProgressLimits {
        ProgressLimits::FF7
    }

    // ---- used by the server to keep each player's inventory (defaults: no items) ----

    /// The items a new account starts with, ascending by item (FF7: the kernel's starting stock). An
    /// account whose store holds no inventory yet starts from this, once.
    fn starting_inventory(&self) -> Vec<InventoryEntry> {
        Vec::new()
    }

    /// The most of one item a stack holds (FF7: 99). A drop or a starting stock never takes a stack past it.
    fn max_item_stack(&self) -> u16 {
        MAX_ITEM_STACK
    }

    /// The items a won battle against `formation_id` drops, rolled from the battle's `seed` AFTER the
    /// outcome on a random stream of its own (never part of the replayed battle), ascending by item. The
    /// server logs what it rolled and hands it out with [`assign_drops`].
    fn roll_drops(&self, _formation_id: u16, _seed: u64) -> Vec<InventoryEntry> {
        Vec::new()
    }

    // ---- used by the client to DRAW a battle (the default keeps older rule sets valid) ----

    /// Where the enemies of formation `id` stand and where the battle camera looks from, in the
    /// game's battle units, or `None` if this rule set has no such data (a client then draws a
    /// default view and layout). Placement only, never sent over the wire: it is read from the
    /// peer's own files like everything else.
    fn formation_scene(&self, _id: u16) -> Option<FormationScene> {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Where a formation stands (what a client needs to draw it)
// ---------------------------------------------------------------------------------------------

/// One enemy's place in a formation, in the game's battle units. For FF7: y points DOWN with the
/// ground at y = 0 (a flying enemy has a negative y), and in a normal battle the enemies stand at
/// negative z and the party at positive z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnemyPlacement {
    /// The scene's enemy id (which of the scene's enemy records this entry is).
    pub enemy_id: u16,
    pub position: [i16; 3],
    /// Row (higher is further back).
    pub row: u16,
}

/// A camera in battle units: where it stands and the point it looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraPose {
    pub position: [i16; 3],
    pub target: [i16; 3],
}

/// What a peer needs to draw a formation (see [`BattleRules::formation_scene`]): placement and
/// camera only. Names, stats and models are the peer's own business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormationScene {
    /// The battle location (arena) the data names.
    pub location: u16,
    /// The arrival layout (normal, back attack, pincer, ...).
    pub layout: BattleLayout,
    /// The formation's idle camera.
    pub camera: CameraPose,
    /// One entry per enemy of the engine, in the engine's enemy order (`BattleState::enemy_ids`):
    /// the placement of the k-th enemy of a built battle is `enemies[k]`.
    pub enemies: Vec<EnemyPlacement>,
}

impl FormationScene {
    /// The scene of a formation of a decoded `scene.bin`. Its enemies are the ones
    /// `enemies_for_formation` builds, in the same order.
    pub fn of(formation: &fmv_ff7_battle::scene::FormationRef<'_>) -> Self {
        let camera = formation.camera().cameras[0];
        Self {
            location: formation.setup().location,
            layout: formation.setup().layout,
            camera: CameraPose { position: camera.position, target: camera.direction },
            enemies: formation
                .enemies()
                .iter()
                .map(|e| EnemyPlacement { enemy_id: e.entry.enemy_id, position: e.entry.position, row: e.entry.row })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Replay: the client side of the input log
// ---------------------------------------------------------------------------------------------

/// How a replayed battle disagrees with what the server announced. Any of these means the peer's
/// engine is no longer the server's: its game data differs, the engines differ, or the server's
/// log is not one a valid engine could have produced. A peer must say so loudly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Divergence {
    #[error("the server's engine tick went backwards: ours is {local}, the message says {server}")]
    TickBackwards { local: u64, server: u64 },
    #[error("after advancing, our engine is at tick {local} but the server's message is for tick {server}")]
    TickMismatch { local: u64, server: u64 },
    #[error("ticks that the server reported as silent produced events here (first: {first:?})")]
    SilentTicksNotSilent { first: BattleEvent },
    #[error("the server applied a command that our engine refuses: {0}")]
    CommandRefused(String),
    #[error("events differ at tick {tick}, event {index}: ours {local:?}, the server's {server:?} ({local_len} vs {server_len} events)")]
    EventsDiffer {
        tick: u64,
        index: usize,
        local: Option<BattleEvent>,
        server: Option<BattleEvent>,
        local_len: usize,
        server_len: usize,
    },
    #[error("the server ended the battle with {server:?} but our engine says {local:?}")]
    OutcomeDiffers { local: Option<Outcome>, server: Outcome },
    #[error("the rewards differ: ours {local:?}, the server's {server:?}")]
    RewardsDiffer { local: Vec<BattleReward>, server: Vec<BattleReward> },
}

/// A peer's replay of a battle: the engine built from its own game data, advanced only by what the
/// server announces (see the module docs, "The input log"). One definition of that rule, used by
/// the client and by the server's own tests.
#[derive(Debug, Clone)]
pub struct BattleReplay {
    state: BattleState,
    party: Vec<BattleMember>,
}

impl BattleReplay {
    /// Build the engine of `setup` from this peer's own data.
    pub fn start(rules: &dyn BattleRules, setup: &BattleSetup) -> Result<Self, BattleBuildError> {
        Ok(Self { state: rules.build(setup)?, party: setup.party.clone() })
    }

    /// The replayed engine (read-only: it only moves through [`BattleReplay::apply`]).
    pub fn state(&self) -> &BattleState {
        &self.state
    }

    /// The party, `party[i]` being `ActorId(i)`.
    pub fn party(&self) -> &[BattleMember] {
        &self.party
    }

    /// Apply one `BattleEvent` message and check that the engine reproduces its `events`.
    pub fn apply(&mut self, tick: u64, events: &[BattleEvent], command: Option<&AppliedCommand>) -> Result<(), Divergence> {
        let here = self.state.tick();
        let ahead = tick.checked_sub(here).ok_or(Divergence::TickBackwards { local: here, server: tick })?;
        match command {
            None => {
                let got = self.state.step_ticks(ahead);
                self.check_tick(tick)?;
                compare(tick, &got, events)
            }
            Some(applied) => {
                if ahead > 0 {
                    let silent = self.state.step_ticks(ahead);
                    if let Some(first) = silent.first() {
                        return Err(Divergence::SilentTicksNotSilent { first: *first });
                    }
                }
                self.check_tick(tick)?;
                let got = self
                    .state
                    .submit_command(applied.actor, applied.command)
                    .map_err(|e| Divergence::CommandRefused(e.to_string()))?;
                compare(tick, &got, events)
            }
        }
    }

    fn check_tick(&self, server: u64) -> Result<(), Divergence> {
        let local = self.state.tick();
        if local == server {
            Ok(())
        } else {
            Err(Divergence::TickMismatch { local, server })
        }
    }

    /// The roster the rewards are split over: every PLAYER of the party and whether the engine says it is alive
    /// (see [`reward_roster`] and [`split_rewards`]); the companions are not paid, so they are not in it.
    pub fn roster(&self) -> Vec<(AccountId, bool)> {
        reward_roster(&self.party, &self.state)
    }

    /// The rewards the engine says the battle earned (empty unless it was won).
    pub fn expected_rewards(&self) -> Vec<BattleReward> {
        match self.state.outcome() {
            Some(Outcome::Victory) => split_rewards(self.state.rewards(), &self.roster()),
            _ => Vec::new(),
        }
    }

    /// Check a `BattleEnd` against the engine: it must have ended the way the server says, and the
    /// rewards must be the ones the engine's totals split into. The dropped items are not compared:
    /// the server rolled them after the battle and a replay cannot reproduce them (see the module docs).
    pub fn check_end(&self, outcome: Outcome, rewards: &[BattleReward]) -> Result<(), Divergence> {
        let local = self.state.outcome();
        if local != Some(outcome) {
            return Err(Divergence::OutcomeDiffers { local, server: outcome });
        }
        let expected = self.expected_rewards();
        let announced: Vec<BattleReward> = rewards.iter().map(BattleReward::without_items).collect();
        if expected != announced {
            return Err(Divergence::RewardsDiffer { local: expected, server: rewards.to_vec() });
        }
        Ok(())
    }
}

/// `Ok` when `local == server`, else the first difference.
fn compare(tick: u64, local: &[BattleEvent], server: &[BattleEvent]) -> Result<(), Divergence> {
    if local == server {
        return Ok(());
    }
    let index = local.iter().zip(server).position(|(a, b)| a != b).unwrap_or(local.len().min(server.len()));
    Err(Divergence::EventsDiffer {
        tick,
        index,
        local: local.get(index).copied(),
        server: server.get(index).copied(),
        local_len: local.len(),
        server_len: server.len(),
    })
}

// ---------------------------------------------------------------------------------------------
// Rewards
// ---------------------------------------------------------------------------------------------

/// Divide the reward of a won battle among the roster: every member alive at the end gets
/// `exp / alive` and `gil / alive` (rounded down, the remainder is dropped) and the full `ap`;
/// the dead get nothing. The list has one entry per roster member, in roster order, so it does not
/// depend on who is still connected.
///
/// APPROXIMATION: FF7 pools gil for the whole party and splits experience among the living;
/// accounts here have separate wallets, so gil is split like experience.
pub fn split_rewards(total: Reward, roster: &[(AccountId, bool)]) -> Vec<BattleReward> {
    let alive = roster.iter().filter(|(_, alive)| *alive).count().max(1) as u32;
    roster
        .iter()
        .map(|&(account, is_alive)| {
            if is_alive {
                BattleReward::new(account, total.exp / alive, total.gil / alive, total.ap)
            } else {
                BattleReward::new(account, 0, 0, 0)
            }
        })
        .collect()
}

/// Hand the dropped items to the living members of the roster: one unit at a time, in the order of
/// `drops` (item by item, each as many times as its count), round-robin over the living members in
/// roster order, starting with the first. `rewards` is the list [`split_rewards`] gave for the same
/// `roster` and gets the items; each member's list ends up with one entry per item, ascending by item,
/// and a stack never passes `max_stack`. The dead get nothing, and when nobody is alive nothing is
/// handed out.
///
/// APPROXIMATION: FF7 pools the party's items; accounts here have separate inventories, so the drops
/// are dealt out like cards.
pub fn assign_drops(rewards: &mut [BattleReward], roster: &[(AccountId, bool)], drops: &[InventoryEntry], max_stack: u16) {
    let alive: Vec<usize> = roster
        .iter()
        .enumerate()
        .filter(|(i, (_, is_alive))| *is_alive && *i < rewards.len())
        .map(|(i, _)| i)
        .collect();
    if alive.is_empty() {
        return;
    }
    let mut unit = 0usize;
    for drop in drops {
        for _ in 0..drop.count {
            let member = alive[unit % alive.len()];
            unit += 1;
            let items = &mut rewards[member].items;
            match items.iter_mut().find(|e| e.item == drop.item) {
                Some(entry) => entry.count = entry.count.saturating_add(1).min(max_stack),
                None => items.push(InventoryEntry::new(drop.item, 1.min(max_stack))),
            }
        }
    }
    for &member in &alive {
        rewards[member].items.retain(|e| e.count > 0);
        rewards[member].items.sort_by_key(|e| e.item);
    }
}

/// Roll the drops of enemies whose drop tables are `tables` (each an enemy record's four rate bytes and
/// four item ids), in that order, from `seed` on the drop stream. An enemy drops at most one item: its
/// slots are tried in order, a slot with a rate under `0x80` (rates from `0x80` are STEAL chances, not drops)
/// and an item succeeds when `below(64) < rate`, and the first success ends the enemy's rolls. An item
/// index outside the item table (a weapon, armor or accessory) is not kept. The result is ascending by item
/// with each count capped at `max_stack`. One `below(64)` is drawn for every slot that is tried, so the
/// stream a seed gives is a function of the tables alone.
fn roll_enemy_drops(seed: u64, tables: &[([u8; 4], [u16; 4])], max_stack: u16) -> Vec<InventoryEntry> {
    let mut rng = Rng::with_stream(seed, DROP_STREAM);
    let mut drops: Vec<InventoryEntry> = Vec::new();
    for (rates, items) in tables {
        for (rate, item) in rates.iter().zip(items) {
            if *item == 0xFFFF || *rate >= 0x80 {
                continue;
            }
            if rng.below(DROP_DIE) < u32::from(*rate) {
                if usize::from(*item) < ITEM_COUNT {
                    match drops.iter_mut().find(|e| e.item == *item) {
                        Some(entry) => entry.count = entry.count.saturating_add(1).min(max_stack),
                        None => drops.push(InventoryEntry::new(*item, 1.min(max_stack))),
                    }
                }
                break;
            }
        }
    }
    drops.retain(|e| e.count > 0);
    drops.sort_by_key(|e| e.item);
    drops
}

// ---------------------------------------------------------------------------------------------
// FF7: scene.bin + kernel.bin
// ---------------------------------------------------------------------------------------------

/// Why the battle data files cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum Ff7BattleDataError {
    #[error("cannot read {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{what} is {len} bytes, over the {max} byte limit")]
    TooLarge { what: &'static str, len: u64, max: u64 },
    #[error("scene.bin: {0}")]
    Scene(#[from] SceneError),
    #[error("kernel.bin: {0}")]
    Kernel(#[from] KernelError),
}

/// `scene.bin` and `kernel.bin` of the owner's own FF7 copy, decoded read-only. Nothing in here is
/// ever serialized or logged: only [`Ff7BattleData::fingerprint`] leaves the process.
pub struct Ff7BattleData {
    scenes: SceneBin,
    kernel: Kernel,
    /// The items the engine can use in battle, from the kernel (shared by every battle built here).
    items: Arc<ItemTable>,
    fingerprint: Fingerprint,
}

impl fmt::Debug for Ff7BattleData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ff7BattleData")
            .field("scenes", &self.scenes.scenes().len())
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Domain-separated blake3 of both files, each length-prefixed.
fn battle_data_fingerprint(scene: &[u8], kernel: &[u8]) -> Fingerprint {
    let mut h = blake3::Hasher::new();
    h.update(b"fmv-ff7-battle-data/1");
    h.update(&(scene.len() as u64).to_le_bytes());
    h.update(scene);
    h.update(&(kernel.len() as u64).to_le_bytes());
    h.update(kernel);
    Fingerprint(*h.finalize().as_bytes())
}

/// Read a whole file under a size cap (read-only).
fn read_capped(path: &Path, what: &'static str, max: u64) -> Result<Vec<u8>, Ff7BattleDataError> {
    let io = |source| Ff7BattleDataError::Io { what, source };
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len > max {
        return Err(Ff7BattleDataError::TooLarge { what, len, max });
    }
    std::fs::read(path).map_err(io)
}

impl Ff7BattleData {
    /// Decode `scene.bin` and `kernel.bin` from their bytes.
    pub fn from_bytes(scene: &[u8], kernel: &[u8]) -> Result<Self, Ff7BattleDataError> {
        // scene.bin first: a caller that passes two bad files hears about the first one.
        let scenes = SceneBin::parse(scene)?;
        let kernel_data = Kernel::parse(kernel)?;
        Ok(Self {
            scenes,
            items: Arc::new(ItemTable::from_kernel(&kernel_data)),
            kernel: kernel_data,
            fingerprint: battle_data_fingerprint(scene, kernel),
        })
    }

    /// Read and decode the two files (opened read-only, size-capped).
    pub fn open(scene: &Path, kernel: &Path) -> Result<Self, Ff7BattleDataError> {
        let scene = read_capped(scene, "scene.bin", MAX_SCENE_BIN_BYTES)?;
        let kernel = read_capped(kernel, "kernel.bin", MAX_KERNEL_BYTES)?;
        Self::from_bytes(&scene, &kernel)
    }

    /// The decoded `scene.bin`.
    pub fn scenes(&self) -> &SceneBin {
        &self.scenes
    }

    /// The decoded `kernel.bin`.
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// Fingerprint of the two files (also [`BattleRules::data_fingerprint`]).
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    fn party(&self, setup: &BattleSetup) -> Result<Vec<Combatant>, BattleBuildError> {
        if setup.party.is_empty() {
            return Err(BattleBuildError::EmptyParty);
        }
        if setup.party.len() > MAX_BATTLE_PARTY {
            return Err(BattleBuildError::PartyTooLarge(setup.party.len()));
        }
        setup
            .party
            .iter()
            .map(|m| {
                let who = m.character_id().ok_or(BattleBuildError::UnknownCharacter(m.character))?;
                if m.limit > LIMIT_MAX {
                    return Err(BattleBuildError::InvalidLimit { character: m.character, limit: m.limit, max: LIMIT_MAX });
                }
                let mut c = Combatant::from_character(&self.kernel, who, m.level, m.weapon, m.armor)
                    .ok_or(BattleBuildError::NoCharacterData(m.character))?;
                // A player's character has no limit break (protocol v9): the combatant of a player on Cloud's
                // curve must not give Braver beside the canon Cloud. The engine drops a gauge that has no break.
                if m.is_player() && !PLAYER_LIMIT_BREAKS {
                    c.limit_break = None;
                }
                c.limit = m.limit;
                c.status = start_status(m)?;
                Ok(c)
            })
            .collect()
    }
}

impl BattleRules for Ff7BattleData {
    fn build(&self, setup: &BattleSetup) -> Result<BattleState, BattleBuildError> {
        let formation = self
            .scenes
            .formation(usize::from(setup.formation_id))
            .ok_or(BattleBuildError::UnknownFormation(setup.formation_id))?;
        let enemies = enemies_for_formation(formation.scene, formation.slot);
        if enemies.is_empty() {
            return Err(BattleBuildError::NoEnemies(setup.formation_id));
        }
        let party = self.party(setup)?;
        // How hard the fight is to run from is the scene's own: its escape counter and flags, and the
        // arrival that makes every Run Check succeed (a preemptive strike, a side attack).
        let scene_setup = formation.setup();
        let escape = scene_setup
            .run_difficulty()
            .map(|difficulty| EscapeRule { difficulty, sure: scene_setup.layout.makes_running_sure() });
        Ok(BattleState::with_config(setup.seed, party, enemies, setup.config())
            .with_items(Arc::clone(&self.items))
            .with_escape(escape))
    }

    fn data_fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    fn formation(&self, id: u16) -> Option<FormationInfo> {
        let formation = self.scenes.formation(usize::from(id))?;
        let enemies = formation.enemies().len();
        if enemies == 0 {
            return None;
        }
        let layout = formation.setup().layout;
        Some(FormationInfo {
            preemptive: layout == BattleLayout::Preemptive,
            back_attack: layout == BattleLayout::BackAttack,
            enemies,
        })
    }

    /// The level in the kernel's initial data. APPROXIMATION: a character that has not joined the
    /// story yet carries the placeholder level 1 there (checked on the owner's Steam copy: only
    /// Cloud, Cait Sith and Vincent start above 1), and joins at the leader's level, so it
    /// fights at Cloud's level instead.
    fn starting_level(&self, character: u8) -> u8 {
        let level_of = |id: u8| self.kernel.init_character_by_id(id).map(|c| c.level.clamp(1, 99));
        match level_of(character) {
            Some(own) if own > 1 => own,
            own => level_of(0).or(own).unwrap_or(DEFAULT_LEVEL),
        }
    }

    /// The canon characters' own names (`CharacterId::name`: Cloud, Barret, Tifa, Aerith, Red XIII, Yuffie, Cait Sith,
    /// Vincent, Cid).
    fn character_name(&self, character: u8) -> Option<String> {
        CharacterId::from_index(usize::from(character)).map(|who| who.name().to_string())
    }

    /// The weakest formation: the one whose enemies have the least HP in total (the lowest id on a
    /// tie). Data-derived, so it does not depend on one edition's numbering, and the likeliest to be
    /// winnable by a starting party. A server that wants a particular fight names it
    /// (`--debug-formation`); on a field with an encounter table the table is used first.
    fn default_formation(&self) -> Option<u16> {
        (0..self.scenes.formation_count())
            .filter_map(|id| u16::try_from(id).ok())
            .filter_map(|id| {
                let enemies = self.scenes.formation(usize::from(id))?.enemies();
                let total_hp: u64 = enemies.iter().map(|e| u64::from(e.record.hp)).sum();
                (!enemies.is_empty() && total_hp > 0).then_some((total_hp, id))
            })
            .min()
            .map(|(_, id)| id)
    }

    /// The kernel's EXP curve of the character ([`fmv_ff7_battle::kernel::GrowthData::exp_for_level`]).
    fn exp_for_level(&self, character: u8, level: u8) -> Option<u32> {
        self.kernel.growth.exp_for_level(CharacterId::from_index(usize::from(character))?, level)
    }

    /// The kernel's own starting record when it belongs to `level` on this character's curve (Cloud's
    /// does: level 6 with 610 EXP, six short of level 7), else the least `level` takes; see
    /// [`fmv_ff7_battle::kernel::Kernel::starting_exp`]. The other characters start at the leader's
    /// level (see [`BattleRules::starting_level`]), which their own records do not describe.
    fn starting_exp(&self, character: u8, level: u8) -> u32 {
        CharacterId::from_index(usize::from(character)).and_then(|who| self.kernel.starting_exp(who, level)).unwrap_or(0)
    }

    /// The kernel's starting item stock ([`Kernel::starting_inventory`]): Potion x3, Ether x1 and Phoenix Down
    /// x1 on the Steam copy.
    fn starting_inventory(&self) -> Vec<InventoryEntry> {
        self.kernel.starting_inventory().into_iter().map(|s| InventoryEntry::new(s.item, s.count)).collect()
    }

    /// The drops of the formation's enemies: each enemy record's drop rates and items, in placement
    /// order (see `roll_enemy_drops` for the rule).
    fn roll_drops(&self, formation_id: u16, seed: u64) -> Vec<InventoryEntry> {
        let Some(formation) = self.scenes.formation(usize::from(formation_id)) else {
            return Vec::new();
        };
        let tables: Vec<([u8; 4], [u16; 4])> =
            formation.enemies().iter().map(|e| (e.record.item_rates, e.record.item_ids)).collect();
        roll_enemy_drops(seed, &tables, self.max_item_stack())
    }

    /// The formation's enemy placements and idle camera from `scene.bin` (see
    /// [`FormationScene::of`]); `None` for a formation that does not exist or has no enemies.
    fn formation_scene(&self, id: u16) -> Option<FormationScene> {
        let scene = FormationScene::of(&self.scenes.formation(usize::from(id))?);
        (!scene.enemies.is_empty()).then_some(scene)
    }
}

/// The level used when the data has no starting level for a character.
pub const DEFAULT_LEVEL: u8 = 5;

// ---------------------------------------------------------------------------------------------
// Synthetic rules (test scaffolding)
// ---------------------------------------------------------------------------------------------

/// Hand-made rules for tests of the server and the client: no game files, a few fixed formations
/// and simple characters. **Test scaffolding only.** The binaries never use it, and it is not
/// FF7 support: nothing here resembles the real game's numbers.
///
/// Formations: `0` one weak enemy, `1` two weak enemies, `2` one overwhelming enemy (the party
/// loses), `3` like `0` but with a preemptive layout, `4` one brute that kills a hero per hit (the
/// party usually wins, with losses). Any `character` below 9 is a fighter whose strength grows
/// with its level, who knows Cure (spell 0, 5 MP), Life (spell 7, 34 MP) and Fire (spell 27, 4 MP), and
/// whose battles accept Potion (item 0), Ether (item 3), Phoenix Down (item 7) and Grenade (item 20) (and see
/// below for the whole-side items). A new account starts
/// with 3 Potions, 1 Ether and 1 Phoenix Down; a won battle drops [`SyntheticRules::drop_item`], one per
/// enemy, if it is set. The enemies only ever attack.
///
/// **Whole sides (v10).** Cure, Life and Fire are the kind a spell can be aimed at one target or a whole side
/// with (the real records' `0x0D` and `0x0F`), and every fighter has one use of the All materia per battle
/// ([`SyntheticRules::ALL_USES`], what the real kernel's level 1 gives), so each can cast one of them on a
/// whole side. The item table also holds Megalixir (6: the whole party, everything restored) and Shrapnel (21:
/// every enemy), which a test hands an account itself.
///
/// **Limits and escape (v7).** Every companion (and, with [`SyntheticRules::player_limits`], every player's fighter:
/// the real rules give players none, v9) has a limit that fills quickly (HP divisor 40): id 128, a
/// strike of Power 48 on one enemy, except character 3, whose limit (id 142) heals one ally for half of its
/// max HP, like Aerith's. Formations `0` and `1` can be run from at difficulty 1, formation `3` (the
/// preemptive one) surely at difficulty 1, formation `4` at difficulty 2 and formation `2`, the
/// overwhelming enemy, cannot be run from at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticRules {
    /// Hit points of each weak enemy.
    pub enemy_hp: u32,
    /// The item every enemy of a won battle drops (one each, for certain); `None` drops nothing.
    pub drop_item: Option<u16>,
    /// Give the PLAYERS' fighters a limit break too (protocol v9: the real rules give them none, see
    /// [`PLAYER_LIMIT_BREAKS`], and so does the default here). The companions always have one. It exists so that the
    /// machinery a player's limit gauge goes through (the gauge a member starts with, the command, the saved end
    /// gauge, the menu's row) stays under test while no real player has one.
    pub player_limits: bool,
}

impl Default for SyntheticRules {
    fn default() -> Self {
        Self { enemy_hp: 200, drop_item: None, player_limits: false }
    }
}

impl SyntheticRules {
    /// The fingerprint every synthetic rule set reports.
    pub const FINGERPRINT: Fingerprint = Fingerprint([0x5a; 32]);

    /// How many times a synthetic fighter may cast a spell on a whole side in a battle: the one use of an All
    /// materia at level 1, like the real kernel's.
    pub const ALL_USES: u8 = 1;

    /// The items of a synthetic battle: Potion (0), Ether (3), Megalixir (6, the whole party), Phoenix Down
    /// (7), Grenade (20, hurts an enemy for about `512 * 10 / 32` = 160) and Shrapnel (21, the same for every
    /// enemy at once).
    pub fn item_table() -> Arc<ItemTable> {
        Arc::new(ItemTable::new([
            (0, ItemData::new(ActionSpec::flat_restore_item(5, false))),
            (3, ItemData::new(ActionSpec::flat_restore_item(5, true))),
            (6, ItemData::new(ActionSpec::full_restore_item().with_aim(Aim::Side))),
            (7, ItemData::new(ActionSpec::revive_spell(8, 0).with_aim(Aim::One))),
            (20, ItemData::new(ActionSpec::damage_item(10, element::SHOOT))),
            (21, ItemData::new(ActionSpec::damage_item(10, element::SHOOT).with_aim(Aim::Side))),
        ]))
    }

    /// The spells of a synthetic fighter: Cure (0), Life (7) and Fire (27).
    pub fn spells() -> Vec<Spell> {
        vec![
            Spell::new(0, ActionSpec::cure_spell(5, 5)),
            Spell::new(7, ActionSpec::revive_spell(8, 34)),
            Spell::new(27, ActionSpec::damage_spell(8, 100, element::FIRE, 4)),
        ]
    }

    /// How many enemies formation `id` places (0 for one that does not exist).
    fn enemy_count(id: u16) -> u16 {
        match id {
            0 | 2 | 3 | 4 => 1,
            1 => 2,
            _ => 0,
        }
    }

    /// How hard formation `id` is to run from (see the type's docs).
    fn escape_rule(id: u16) -> Option<EscapeRule> {
        match id {
            0 | 1 => Some(EscapeRule { difficulty: 1, sure: false }),
            3 => Some(EscapeRule { difficulty: 1, sure: true }),
            4 => Some(EscapeRule { difficulty: 2, sure: false }),
            _ => None,
        }
    }

    fn enemy(&self, name: &str, hp: u32, attack: u32) -> Combatant {
        let mut e = Combatant::new(name, 3, hp);
        e.attack = attack;
        e.defense = 0;
        e.dex = 40;
        e.accuracy = 100;
        e.reward = Reward { exp: 12, gil: 20, ap: 2 };
        e
    }
}

impl BattleRules for SyntheticRules {
    fn build(&self, setup: &BattleSetup) -> Result<BattleState, BattleBuildError> {
        let enemies = match setup.formation_id {
            0 | 3 => vec![self.enemy("Dummy A", self.enemy_hp, 10)],
            1 => vec![self.enemy("Dummy A", self.enemy_hp, 10), self.enemy("Dummy B", self.enemy_hp, 10)],
            2 => vec![self.enemy("Colossus", 5_000, 200)],
            4 => vec![self.enemy("Brute", 600, 250)],
            other => return Err(BattleBuildError::UnknownFormation(other)),
        };
        if setup.party.is_empty() {
            return Err(BattleBuildError::EmptyParty);
        }
        if setup.party.len() > MAX_BATTLE_PARTY.min(MAX_PER_SIDE) {
            return Err(BattleBuildError::PartyTooLarge(setup.party.len()));
        }
        let party = setup
            .party
            .iter()
            .map(|m| {
                let who = m.character_id().ok_or(BattleBuildError::UnknownCharacter(m.character))?;
                if m.limit > LIMIT_MAX {
                    return Err(BattleBuildError::InvalidLimit { character: m.character, limit: m.limit, max: LIMIT_MAX });
                }
                let level = u32::from(m.level.clamp(1, 99));
                let mut c = Combatant::new(who.name(), m.level.clamp(1, 99), 120 + 20 * level);
                c.attack = 30 + 4 * level;
                c.defense = 5;
                c.magic = 10 + 2 * level;
                c.magic_defense = 5;
                c.mp = 60;
                c.max_mp = 60;
                c.dex = 40;
                c.accuracy = 100;
                c.spells = Self::spells();
                c.all_left = Self::ALL_USES;
                c.limit = m.limit;
                c.status = start_status(m)?;
                c.limit_break = (m.is_companion() || self.player_limits).then(|| match who {
                    CharacterId::Aeris => LimitBreak::new(142, ActionSpec::limit_heal(16), 40),
                    _ => LimitBreak::new(128, ActionSpec::limit_strike(48), 40),
                });
                Ok(c)
            })
            .collect::<Result<Vec<_>, BattleBuildError>>()?;
        Ok(BattleState::with_config(setup.seed, party, enemies, setup.config())
            .with_items(Self::item_table())
            .with_escape(Self::escape_rule(setup.formation_id)))
    }

    fn data_fingerprint(&self) -> Fingerprint {
        Self::FINGERPRINT
    }

    fn formation(&self, id: u16) -> Option<FormationInfo> {
        match id {
            0 => Some(FormationInfo { preemptive: false, back_attack: false, enemies: 1 }),
            1 => Some(FormationInfo { preemptive: false, back_attack: false, enemies: 2 }),
            2 => Some(FormationInfo { preemptive: false, back_attack: false, enemies: 1 }),
            3 => Some(FormationInfo { preemptive: true, back_attack: false, enemies: 1 }),
            4 => Some(FormationInfo { preemptive: false, back_attack: false, enemies: 1 }),
            _ => None,
        }
    }

    fn starting_level(&self, _character: u8) -> u8 {
        5
    }

    fn default_formation(&self) -> Option<u16> {
        Some(0)
    }

    /// The nine canon names, like the real rules.
    fn character_name(&self, character: u8) -> Option<String> {
        CharacterId::from_index(usize::from(character)).map(|who| who.name().to_string())
    }

    /// `2 * level * (level - 1)`: level 5 starts at 40, level 6 needs 60, level 7 needs 84 (a step of
    /// `4 * level`), so a won two-dummy formation (24 experience for one player) levels a starting
    /// character up. A made-up curve, like everything here.
    fn exp_for_level(&self, character: u8, level: u8) -> Option<u32> {
        (character < 9).then(|| {
            let level = u32::from(level.clamp(1, 99));
            2 * level * (level - 1)
        })
    }

    /// 3 Potions, 1 Ether and 1 Phoenix Down (what the real kernel starts a game with).
    fn starting_inventory(&self) -> Vec<InventoryEntry> {
        vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)]
    }

    /// One of `drop_item` per enemy of the formation, for certain, when it is set.
    fn roll_drops(&self, formation_id: u16, _seed: u64) -> Vec<InventoryEntry> {
        match (self.drop_item, Self::enemy_count(formation_id)) {
            (Some(item), count) if count > 0 => vec![InventoryEntry::new(item, count)],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7_battle::rules::{BattleState, Command};

    fn member(i: u64, character: u8) -> BattleMember {
        BattleMember::player(EntityId(10 + i), AccountId(20 + i), format!("Player {i}"), character, 6, 0)
    }

    pub(crate) fn setup(formation_id: u16, members: usize) -> BattleSetup {
        BattleSetup {
            formation_id,
            seed: 0x00BA_771E,
            party: (0..members as u64).map(|i| member(i, i as u8)).collect(),
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
        }
    }

    /// Drive a battle the way a server does: step a few ticks, let every ready party member attack
    /// the first living enemy. Returns every event and the applied operations in order.
    fn play(state: &mut BattleState) -> Vec<BattleEvent> {
        let mut log = Vec::new();
        for _ in 0..10_000 {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step_ticks(5));
            for id in state.ready_actors() {
                let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else {
                    break;
                };
                if let Ok(events) = state.submit_command(id, Command::Attack { target }) {
                    log.extend(events);
                }
            }
        }
        log
    }

    #[test]
    fn the_same_setup_builds_the_same_battle_and_plays_the_same_way() {
        let rules = SyntheticRules::default();
        let setup = setup(1, 2);
        let (mut a, mut b) = (rules.build(&setup).unwrap(), rules.build(&setup).unwrap());
        assert_eq!(a, b, "two peers building from one setup must agree before anything happens");
        assert_eq!((a.party_ids().len(), a.enemy_ids().len()), (2, 2));
        let (log_a, log_b) = (play(&mut a), play(&mut b));
        assert_eq!(log_a, log_b);
        assert_eq!(a.outcome(), Some(Outcome::Victory));
        assert!(log_a.contains(&BattleEvent::Victory));
        // A different seed is a different battle (initial gauges are random).
        let other = rules.build(&BattleSetup { seed: 7, ..setup.clone() }).unwrap();
        assert_ne!(other, rules.build(&setup).unwrap());
    }

    #[test]
    fn the_flags_choose_the_start_and_both_flags_mean_normal() {
        let mut s = setup(0, 1);
        assert_eq!(s.start_mode(), StartMode::Normal);
        s.preemptive = true;
        assert_eq!(s.start_mode(), StartMode::Preemptive);
        s.back_attack = true;
        assert_eq!(s.start_mode(), StartMode::Normal, "contradictory flags are a normal start");
        s.preemptive = false;
        assert_eq!(s.start_mode(), StartMode::BackAttack);
        assert_eq!(s.config().battle_speed, 0);
        assert_eq!(s.config().ticks_per_second, 1000);

        // Preemptive: the party's gauges start full, the enemies' empty.
        let rules = SyntheticRules::default();
        let pre = rules.build(&BattleSetup { preemptive: true, ..setup(0, 2) }).unwrap();
        assert_eq!(pre.ready_actors().len(), 2);
        let back = rules.build(&BattleSetup { back_attack: true, ..setup(0, 2) }).unwrap();
        assert!(back.ready_actors().is_empty());
    }

    #[test]
    fn bad_setups_are_errors_not_panics() {
        let rules = SyntheticRules::default();
        assert_eq!(rules.build(&setup(99, 1)).unwrap_err(), BattleBuildError::UnknownFormation(99));
        assert_eq!(rules.build(&setup(0, 0)).unwrap_err(), BattleBuildError::EmptyParty);
        assert_eq!(rules.build(&setup(0, 9)).unwrap_err(), BattleBuildError::PartyTooLarge(9));
        let mut s = setup(0, 1);
        s.party[0].character = 77;
        assert_eq!(rules.build(&s).unwrap_err(), BattleBuildError::UnknownCharacter(77));
        // A starting limit gauge above the fullest one is refused, not clamped: a server that sends one is
        // broken or hostile, and every peer says so instead of replaying something else.
        let mut s = setup(0, 2);
        s.party[1].limit = 256;
        let err = rules.build(&s).unwrap_err();
        assert_eq!(err, BattleBuildError::InvalidLimit { character: 1, limit: 256, max: 255 });
        assert!(err.to_string().contains("256"), "{err}");
        s.party[1].limit = 255;
        assert!(rules.build(&s).is_ok(), "a full gauge is the most there is");
        for e in [
            BattleBuildError::NoEnemies(3),
            BattleBuildError::NoCharacterData(2),
            BattleBuildError::PartyTooLarge(9),
            BattleBuildError::InvalidLimit { character: 0, limit: 999, max: 255 },
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn rewards_go_to_the_living_in_roster_order_and_ignore_connections() {
        let total = Reward { exp: 100, gil: 50, ap: 7 };
        let roster = [(AccountId(1), true), (AccountId(2), false), (AccountId(3), true)];
        assert_eq!(
            split_rewards(total, &roster),
            vec![
                BattleReward::new(AccountId(1), 50, 25, 7),
                BattleReward::new(AccountId(2), 0, 0, 0),
                BattleReward::new(AccountId(3), 50, 25, 7),
            ]
        );
        // The remainder is dropped, never invented.
        let odd = split_rewards(Reward { exp: 10, gil: 10, ap: 1 }, &[(AccountId(1), true), (AccountId(2), true), (AccountId(3), true)]);
        assert!(odd.iter().all(|r| r.exp == 3 && r.gil == 3 && r.ap == 1));
        // Nobody alive (not a state a victory reaches): nobody is paid, and nothing divides by zero.
        assert!(split_rewards(total, &[(AccountId(1), false)]).iter().all(|r| r.exp == 0 && r.gil == 0 && r.ap == 0));
        assert!(split_rewards(total, &[]).is_empty());
    }

    // ---- controllers (v9): canon companions first, then the players ----

    fn companion(name: &str, character: u8) -> BattleMember {
        BattleMember::companion(name, character, 6, 0)
    }

    /// Cloud and Barret as companions, then a player on Cloud's curve and one on Tifa's.
    fn canon_then_players() -> Vec<BattleMember> {
        vec![companion("Cloud", 0), companion("Barret", 1), member(0, 0), member(1, 2)]
    }

    #[test]
    fn a_member_is_a_player_with_an_account_and_an_entity_or_a_companion_with_neither() {
        let player = member(3, 2);
        assert_eq!((player.is_player(), player.is_companion()), (true, false));
        assert_eq!((player.account(), player.entity()), (Some(AccountId(23)), Some(EntityId(13))));
        assert_eq!(player.controller, Controller::Player { entity: EntityId(13), account: AccountId(23) });
        let cloud = companion("Cloud", 0);
        assert_eq!((cloud.is_player(), cloud.is_companion()), (false, true));
        assert_eq!((cloud.account(), cloud.entity()), (None, None));
        assert_eq!(cloud.controller, Controller::Companion);
        assert_eq!((cloud.character_id(), cloud.name.as_str()), (Some(CharacterId::Cloud), "Cloud"));
    }

    #[test]
    fn players_have_no_limit_break_and_the_companions_keep_theirs() {
        let rules = SyntheticRules::default();
        // Everybody starts with a full gauge on the wire and, being preemptive, with a full ATB gauge; only the engine
        // decides whom a limit gauge means something for.
        let mut setup = BattleSetup { party: canon_then_players(), preemptive: true, ..setup(0, 1) };
        for m in &mut setup.party {
            m.limit = 255;
        }
        let state = rules.build(&setup).unwrap();
        assert_eq!(state.party_ids().len(), 4);
        let enemy = ActorId(4);
        for companion in [ActorId(0), ActorId(1)] {
            assert!(state.limit_for(companion).is_some(), "{companion:?}: a canon companion has its limit break");
            assert_eq!(state.limit_gauge(companion), 255);
            assert!(state.limit_ready(companion));
            assert_eq!(state.check_command(companion, &Command::Limit { target: Target::One(enemy) }), Ok(()));
        }
        for player in [ActorId(2), ActorId(3)] {
            assert!(state.ready_actors().contains(&player), "the player's turn is here");
            assert!(state.limit_for(player).is_none(), "{player:?}: a player has no limit break");
            assert_eq!(state.limit_gauge(player), 0, "a gauge a player was sent with is no gauge");
            assert!(!state.limit_ready(player));
            assert_eq!(state.check_command(player, &Command::Limit { target: Target::One(enemy) }), Err(fmv_ff7_battle::rules::CommandError::NoLimit(player)));
        }
        // The synthetic switch gives the players one too, and the companions are the same either way.
        let with = SyntheticRules { player_limits: true, ..rules }.build(&setup).unwrap();
        assert!((0..4).all(|i| with.limit_for(ActorId(i)).is_some() && with.limit_gauge(ActorId(i)) == 255));
        // A player's stored gauge is a number the wire carries and the engine ignores: it still has to be one the
        // engine could hold, like any member's.
        let mut over = setup.clone();
        over.party[2].limit = 256;
        assert_eq!(rules.build(&over).unwrap_err(), BattleBuildError::InvalidLimit { character: 0, limit: 256, max: 255 });
    }

    #[test]
    fn the_rewards_roster_is_the_players_in_party_order_and_never_a_companion() {
        let rules = SyntheticRules::default();
        let party = canon_then_players();
        let setup = BattleSetup { party: party.clone(), ..setup(0, 1) };
        let mut state = rules.build(&setup).unwrap();
        // Everybody is alive at the start; the roster has the two players (accounts 20 and 21), in order.
        assert_eq!(reward_roster(&party, &state), vec![(AccountId(20), true), (AccountId(21), true)]);
        assert_eq!(BattleReplay::start(&rules, &setup).unwrap().roster(), reward_roster(&party, &state));
        // A battle played to the end: the exp is split among the living PLAYERS, the companions take no share.
        let log = serve(&mut state, 5);
        assert!(!log.is_empty());
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        let total = state.rewards();
        let rewards = split_rewards(total, &reward_roster(&party, &state));
        assert_eq!(rewards.iter().map(|r| r.account).collect::<Vec<_>>(), [AccountId(20), AccountId(21)]);
        let alive = reward_roster(&party, &state).iter().filter(|(_, alive)| *alive).count() as u32;
        assert!(alive >= 1);
        assert!(rewards.iter().all(|r| r.exp == 0 || r.exp == total.exp / alive), "{rewards:?}");
        // Drops are dealt to the same roster, never to a companion.
        let mut with_drops = rewards.clone();
        assign_drops(&mut with_drops, &reward_roster(&party, &state), &[InventoryEntry::new(0, 3)], 99);
        assert_eq!(with_drops.len(), 2);
        assert_eq!(with_drops.iter().flat_map(|r| &r.items).map(|e| u32::from(e.count)).sum::<u32>(), if alive == 0 { 0 } else { 3 });
    }

    /// The roster is by ACTOR index: a fallen companion in front of the players does not shift anyone's life or death
    /// onto a neighbour.
    #[test]
    fn the_rewards_roster_follows_each_players_own_actor_even_when_a_companion_has_fallen() {
        let party = canon_then_players();
        let mut fighters: Vec<Combatant> = (0..4).map(|i| Combatant::new(format!("F{i}"), 6, 100)).collect();
        fighters[0].hp = 0; // Cloud, a companion, is down
        fighters[2].hp = 0; // the first player is down
        let foe = Combatant::new("Foe", 3, 50);
        let state = BattleState::with_config(1, fighters, vec![foe], BattleConfig::default());
        assert_eq!(reward_roster(&party, &state), vec![(AccountId(20), false), (AccountId(21), true)]);
        // Nobody paid for a fight that a player did not live to see; the living player takes the whole share.
        let total = Reward { exp: 60, gil: 40, ap: 3 };
        let rewards = split_rewards(total, &reward_roster(&party, &state));
        assert_eq!(rewards, vec![BattleReward::new(AccountId(20), 0, 0, 0), BattleReward::new(AccountId(21), 60, 40, 3)]);
    }

    /// The server commands the companions with ordinary commands, applied and announced like a player's: a battle
    /// of two canon companions and two players, played by one driver that stands in for the server (the companions)
    /// and for the players' clients, replays on a peer's engine event for event and ends the way the server says.
    #[test]
    fn a_battle_of_companions_and_players_replays_exactly_whoever_gave_the_commands() {
        let rules = SyntheticRules::default();
        for seed in 0..40u64 {
            let setup = BattleSetup { seed, party: canon_then_players(), ..setup([0u16, 1, 3, 4][seed as usize % 4], 1) };
            let mut server = rules.build(&setup).unwrap();
            let log = serve(&mut server, [3u64, 11, 40][seed as usize % 3]);
            let outcome = server.outcome().unwrap_or_else(|| panic!("seed {seed} never ended"));
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert_eq!(replay.state(), &server, "seed {seed}: the replayed engine IS the server's engine");
            // Companions took part: their actors (0 and 1) gave commands, and so did the players' (2 and 3) in some fights.
            let actors: Vec<u8> = log.iter().filter_map(|(_, _, c)| c.map(|c| c.actor.0)).collect();
            assert!(actors.contains(&0) || actors.contains(&1), "seed {seed}: the companions fought");
            let rewards = replay.expected_rewards();
            replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            if outcome == Outcome::Victory {
                assert!(rewards.iter().all(|r| r.account == AccountId(20) || r.account == AccountId(21)), "seed {seed}: only players are paid: {rewards:?}");
                assert_eq!(rewards.len(), 2, "seed {seed}: one payment per player");
            } else {
                assert!(rewards.is_empty());
            }
        }
    }

    // ---- the input log: a server simulation and its replay ----

    /// One message of the input log, as a server produces it.
    type Message = (u64, Vec<BattleEvent>, Option<AppliedCommand>);

    /// What a server does with a battle: advance it in slices (sending only non-empty batches) and
    /// apply a command whenever a party member is ready.
    fn serve(state: &mut BattleState, slice: u64) -> Vec<Message> {
        let mut log = Vec::new();
        for _ in 0..100_000 {
            if state.outcome().is_some() {
                break;
            }
            let events = state.step_ticks(slice);
            if !events.is_empty() {
                log.push((state.tick(), events, None));
            }
            for id in state.ready_actors() {
                let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else {
                    break;
                };
                let command = Command::Attack { target };
                if let Ok(events) = state.submit_command(id, command) {
                    log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                }
            }
        }
        log
    }

    fn replay_all(rules: &SyntheticRules, setup: &BattleSetup, log: &[Message]) -> Result<BattleReplay, Divergence> {
        let mut replay = BattleReplay::start(rules, setup).expect("builds");
        for (tick, events, command) in log {
            replay.apply(*tick, events, command.as_ref())?;
        }
        Ok(replay)
    }

    #[test]
    fn a_replay_of_the_servers_log_reproduces_it_for_any_slicing_and_any_party() {
        let rules = SyntheticRules::default();
        for (formation, members, slice) in [(0, 1, 1), (1, 2, 5), (1, 3, 17), (3, 2, 3), (0, 2, 100)] {
            let mut setup = setup(formation, members);
            setup.preemptive = formation == 3;
            let mut server = rules.build(&setup).unwrap();
            let log = serve(&mut server, slice);
            assert_eq!(server.outcome(), Some(Outcome::Victory), "formation {formation}, {members} members, slice {slice}");
            assert!(log.iter().any(|(_, _, c)| c.is_some()) && log.iter().any(|(_, _, c)| c.is_none()));
            // The message that carries the victory is the last one.
            assert_eq!(log.last().unwrap().1.last(), Some(&BattleEvent::Victory));
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("{d}"));
            assert_eq!(replay.state(), &server, "the replayed engine IS the server's engine");
            let rewards = replay.expected_rewards();
            assert_eq!(rewards.len(), members);
            replay.check_end(Outcome::Victory, &rewards).expect("the end agrees");
            // A server that announced other rewards, or another outcome, is caught.
            let mut greedy = rewards.clone();
            greedy[0].gil += 1;
            assert!(matches!(replay.check_end(Outcome::Victory, &greedy), Err(Divergence::RewardsDiffer { .. })));
            assert!(matches!(replay.check_end(Outcome::Defeat, &[]), Err(Divergence::OutcomeDiffers { .. })));
        }
    }

    /// A member that starts the battle with `statuses`.
    fn sick(i: u64, character: u8, statuses: &[Status]) -> BattleMember {
        BattleMember { status: statuses.iter().copied().collect(), ..member(i, character) }
    }

    /// The statuses a member starts with are an input like its level: every peer builds the same battle from them, the engine
    /// announces them with its first step and says what they do after that, and a replay of the server's log agrees.
    #[test]
    fn a_battle_that_starts_with_statuses_announces_them_and_replays_exactly() {
        let rules = SyntheticRules::default();
        let mut ticks = 0;
        for seed in 0..24u64 {
            let setup = BattleSetup {
                seed,
                party: vec![
                    sick(0, 0, &[Status::Poison, Status::Haste, Status::Silence]),
                    sick(1, 1, &[Status::Regen, Status::Sadness]),
                    member(2, 2),
                ],
                ..setup([0u16, 1, 4][seed as usize % 3], 1)
            };
            let mut server = rules.build(&setup).unwrap();
            let log = serve(&mut server, [1u64, 7, 40][seed as usize % 3]);
            // Whatever the fight came to, the first message begins with the statuses, in actor and then bit order.
            let first = &log.first().expect("something happened").1;
            let (a, b) = (ActorId(0), ActorId(1));
            assert_eq!(
                first[..5],
                [
                    BattleEvent::StatusInflicted { target: a, status: Status::Poison },
                    BattleEvent::StatusInflicted { target: a, status: Status::Silence },
                    BattleEvent::StatusInflicted { target: a, status: Status::Haste },
                    BattleEvent::StatusInflicted { target: b, status: Status::Sadness },
                    BattleEvent::StatusInflicted { target: b, status: Status::Regen },
                ],
                "seed {seed}"
            );
            ticks += log.iter().flat_map(|(_, events, _)| events).filter(|e| matches!(e, BattleEvent::StatusTick { .. })).count();
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert_eq!(replay.state(), &server, "seed {seed}: the replayed engine IS the server's engine");
            // Nothing else about them was ever sent: a peer that dropped the one thing that carries them replays a
            // different battle, which is caught at the first message.
            let mut forged = BattleReplay::start(&rules, &BattleSetup { party: vec![member(0, 0), member(1, 1), member(2, 2)], ..setup.clone() }).unwrap();
            let (tick, events, command) = &log[0];
            assert!(forged.apply(*tick, events, command.as_ref()).is_err(), "seed {seed}: a peer without the statuses diverges");
        }
        assert!(ticks > 0, "the Poison and the Regen ran in at least one of the fights");
    }

    /// A starting set the engine does not play, or one that cannot be reached (Haste with Slow, Sadness with Fury), is
    /// refused when the battle is built, by every rule set: a hostile server cannot make a peer start from a state the
    /// engine never produces.
    #[test]
    fn a_starting_set_the_battle_does_not_play_is_refused() {
        let rules = SyntheticRules::default();
        let build = |statuses: &[Status]| {
            let setup = BattleSetup { party: vec![sick(0, 3, statuses)], ..setup(0, 1) };
            rules.build(&setup)
        };
        for good in [&[][..], &[Status::Poison], &[Status::Haste, Status::Regen, Status::Barrier, Status::MBarrier], &[Status::Sleep, Status::Stop, Status::Paralysis, Status::Darkness]] {
            let state = build(good).unwrap_or_else(|e| panic!("{good:?}: {e}"));
            assert_eq!(state.actor(ActorId(0)).unwrap().status.set(), good.iter().copied().collect::<StatusSet>());
        }
        for bad in [
            &[Status::Haste, Status::Slow][..],
            &[Status::Sadness, Status::Fury],
            &[Status::Confusion],
            &[Status::Poison, Status::Frog],
            &[Status::Death],
            &[Status::NearDeath],
        ] {
            let bits: u32 = bad.iter().fold(0, |m, s| m | s.bit());
            assert_eq!(build(bad).unwrap_err(), BattleBuildError::InvalidStatus { character: 3, bits }, "{bad:?}");
        }
        // Every bit at once, and one that is no status of the 32.
        let mut member = member(0, 3);
        member.status = StatusSet::from_bits(u32::MAX);
        assert!(matches!(rules.build(&BattleSetup { party: vec![member], ..setup(0, 1) }), Err(BattleBuildError::InvalidStatus { bits: u32::MAX, .. })));
        let message = BattleBuildError::InvalidStatus { character: 3, bits: 0x600 }.to_string();
        assert!(message.contains("0x600") && message.contains('3'), "{message}");
    }

    /// Like `serve`, but every ready member gives a different kind of command in turn: attack, Fire, Cure
    /// on itself, Defend, Potion, Ether, Phoenix Down or Life on a dead ally.
    fn serve_mixed(state: &mut BattleState, slice: u64) -> Vec<Message> {
        let mut log = Vec::new();
        let mut turn = 0usize;
        for _ in 0..100_000 {
            if state.outcome().is_some() {
                break;
            }
            let events = state.step_ticks(slice);
            if !events.is_empty() {
                log.push((state.tick(), events, None));
            }
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                turn += 1;
                let enemy = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive()));
                let dead = state.party_ids().into_iter().find(|a| state.actor(*a).is_some_and(|c| !c.is_alive()));
                let candidates = [
                    enemy.map(|target| Command::Attack { target }),
                    enemy.map(|target| Command::Magic { spell: 27, target: Target::One(target) }),
                    Some(Command::Magic { spell: 0, target: Target::One(id) }),
                    Some(Command::Defend),
                    Some(Command::Item { item: 0, target: Target::One(id) }),
                    Some(Command::Item { item: 3, target: Target::One(id) }),
                    dead.map(|target| Command::Item { item: 7, target: Target::One(target) }),
                    dead.map(|target| Command::Magic { spell: 7, target: Target::One(target) }),
                ];
                let attack = enemy.map(|target| Command::Attack { target });
                let Some(command) = candidates[turn % candidates.len()].filter(|c| state.check_command(id, c).is_ok()).or(attack) else {
                    break;
                };
                if let Ok(events) = state.submit_command(id, command) {
                    log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                }
            }
        }
        log
    }

    #[test]
    fn a_replay_of_a_battle_with_every_kind_of_command_reproduces_it() {
        let rules = SyntheticRules::default();
        let mut seen = std::collections::HashSet::new();
        for (seed, slice) in (0..60u64).zip([1u64, 5, 17, 100].into_iter().cycle()) {
            // The brute kills a hero per hit, so Life and Phoenix Down have somebody to bring back.
            let setup = BattleSetup { seed, ..setup(4, 3) };
            let mut server = rules.build(&setup).unwrap();
            let log = serve_mixed(&mut server, slice);
            assert!(server.outcome().is_some(), "seed {seed}");
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert_eq!(replay.state(), &server, "seed {seed}: the replayed engine IS the server's engine");
            let outcome = server.outcome().unwrap();
            replay.check_end(outcome, &replay.expected_rewards()).expect("the end agrees");
            for (_, events, _) in &log {
                for e in events {
                    seen.insert(std::mem::discriminant(e));
                }
            }
        }
        // Over the seeds: every kind of event but none of the engine's later ones went unplayed.
        let every_kind = [
            BattleEvent::Action { actor: ActorId(0), action: ActionRef::Defend },
            BattleEvent::Heal { actor: ActorId(0), target: ActorId(0), amount: 1 },
            BattleEvent::MpSpent { actor: ActorId(0), amount: 1 },
            BattleEvent::MpRestored { actor: ActorId(0), target: ActorId(0), amount: 1 },
            BattleEvent::Revive { actor: ActorId(0), target: ActorId(0), hp: 1 },
            BattleEvent::Death { actor: ActorId(0) },
            BattleEvent::Victory,
        ];
        for e in every_kind {
            assert!(seen.contains(&std::mem::discriminant(&e)), "no {e:?} in 60 battles");
        }
    }

    /// Like `serve`, but every ready member gives a command that hits a whole side when it can: Fire on every
    /// enemy, Cure or Megalixir on the whole party, Life on every fallen ally, Shrapnel on every enemy, and
    /// otherwise one of the single-target commands.
    fn serve_sides(state: &mut BattleState, slice: u64) -> Vec<Message> {
        let mut log = Vec::new();
        let mut turn = 0usize;
        for _ in 0..100_000 {
            if state.outcome().is_some() {
                break;
            }
            let events = state.step_ticks(slice);
            if !events.is_empty() {
                log.push((state.tick(), events, None));
            }
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                turn += 1;
                let enemy = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive()));
                let candidates = [
                    Some(Command::Magic { spell: 27, target: Target::AllEnemies }),
                    Some(Command::Magic { spell: 0, target: Target::AllAllies }),
                    Some(Command::Item { item: 6, target: Target::AllAllies }),
                    Some(Command::Magic { spell: 7, target: Target::AllAllies }),
                    Some(Command::Item { item: 21, target: Target::AllEnemies }),
                    enemy.map(|target| Command::Magic { spell: 27, target: Target::One(target) }),
                ];
                let attack = enemy.map(|target| Command::Attack { target });
                let Some(command) = candidates[turn % candidates.len()].filter(|c| state.check_command(id, c).is_ok()).or(attack) else {
                    break;
                };
                if let Ok(events) = state.submit_command(id, command) {
                    log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                }
            }
        }
        log
    }

    /// A replay of a battle in which whole sides are hit (protocol v10): the same engine from the same setup
    /// reproduces every message, the All use that a whole-side spell spends included, and a forged log that
    /// asks for one more than a character has is refused.
    #[test]
    fn a_replay_of_a_battle_with_whole_side_commands_reproduces_it() {
        let rules = SyntheticRules::default();
        let (mut sides, mut spells_on_sides) = (0, 0);
        for (seed, slice) in (0..40u64).zip([1u64, 5, 17, 100].into_iter().cycle()) {
            let setup = BattleSetup { seed, ..setup(1, 3) };
            let mut server = rules.build(&setup).unwrap();
            let log = serve_sides(&mut server, slice);
            assert!(server.outcome().is_some(), "seed {seed}");
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert_eq!(replay.state(), &server, "seed {seed}: the replayed engine IS the server's engine");
            replay.check_end(server.outcome().unwrap(), &replay.expected_rewards()).expect("the end agrees");
            for (_, _, command) in &log {
                if let Some(AppliedCommand { command: Command::Magic { target, .. } | Command::Item { target, .. }, .. }) = command {
                    if target.is_side() {
                        sides += 1;
                    }
                }
                if matches!(command, Some(AppliedCommand { command: Command::Magic { target, .. }, .. }) if target.is_side()) {
                    spells_on_sides += 1;
                }
            }
        }
        assert!(sides > 20 && spells_on_sides > 10, "{sides} whole-side commands, {spells_on_sides} of them spells");

        // The forged log: a second whole-side spell from a character that had one use. The replay says the
        // server applied something its own engine refuses.
        let setup = setup(1, 1);
        let mut server = rules.build(&setup).unwrap();
        let mut log = Vec::new();
        for _ in 0..2 {
            for _ in 0..2000 {
                if server.ready_actors().contains(&ActorId(0)) {
                    break;
                }
                let events = server.step_ticks(10);
                if !events.is_empty() {
                    log.push((server.tick(), events, None));
                }
            }
            let fire = Command::Magic { spell: 27, target: Target::AllEnemies };
            if let Ok(events) = server.submit_command(ActorId(0), fire) {
                log.push((server.tick(), events, Some(AppliedCommand { actor: ActorId(0), command: fire })));
            }
        }
        // The server refused the second one, so the log has one; a forger adds the second to a replay.
        let applied = log.iter().filter(|(_, _, c)| c.is_some()).count();
        assert_eq!(applied, 1, "one use, one whole-side spell");
        let mut forged = BattleReplay::start(&rules, &setup).unwrap();
        for (tick, events, command) in &log {
            forged.apply(*tick, events, command.as_ref()).expect("the honest part replays");
        }
        let again = AppliedCommand { actor: ActorId(0), command: Command::Magic { spell: 27, target: Target::AllEnemies } };
        let mut ahead = forged.state().clone();
        for _ in 0..2000 {
            if ahead.ready_actors().contains(&ActorId(0)) {
                break;
            }
            ahead.step_ticks(10);
        }
        let tick = ahead.tick();
        let refused = forged.apply(tick, &[], Some(&again));
        assert!(matches!(refused, Err(Divergence::CommandRefused(_)) | Err(Divergence::SilentTicksNotSilent { .. })), "{refused:?}");
    }

    /// Like `serve`, but a ready member uses its limit when the engine says it is ready (a healing one on
    /// the whole party, a damaging one on the first living enemy), tries to escape on every
    /// `escape_every`-th turn and attacks otherwise.
    fn serve_limits(state: &mut BattleState, slice: u64, escape_every: usize) -> Vec<Message> {
        let mut log = Vec::new();
        let mut turn = 0usize;
        for _ in 0..100_000 {
            if state.outcome().is_some() {
                break;
            }
            let events = state.step_ticks(slice);
            if !events.is_empty() {
                log.push((state.tick(), events, None));
            }
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                turn += 1;
                let Some(enemy) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())) else { break };
                let limit = state.limit_for(id).filter(|_| state.limit_ready(id)).map(|l| if l.targets_allies { Target::AllAllies } else { Target::One(enemy) });
                let command = if turn.is_multiple_of(escape_every) {
                    Command::Escape
                } else {
                    limit.map_or(Command::Attack { target: enemy }, |target| Command::Limit { target })
                };
                let command = if state.check_command(id, &command).is_ok() { command } else { Command::Attack { target: enemy } };
                if let Ok(events) = state.submit_command(id, command) {
                    log.push((state.tick(), events, Some(AppliedCommand { actor: id, command })));
                }
            }
        }
        log
    }

    #[test]
    fn a_replay_of_a_battle_with_limits_and_escapes_reproduces_it_and_ends_the_way_the_server_says() {
        // The players' fighters have limit breaks here (the real rules give them none, see `PLAYER_LIMIT_BREAKS`):
        // the machinery of a limit gauge is what this replays.
        let rules = SyntheticRules { player_limits: true, ..SyntheticRules::default() };
        let (mut escaped, mut won, mut gauges, mut failed, mut limits_used, mut heals) = (0, 0, 0, 0, 0, 0);
        for seed in 0..120u64 {
            let formation = [0u16, 1, 3, 4][seed as usize % 4];
            let mut setup = BattleSetup { seed, ..setup(formation, 3) };
            // Everybody starts with a full gauge (the third member is the Aerith-like healer); formation 3 is the preemptive one.
            for m in &mut setup.party {
                m.limit = 255;
            }
            setup.party[2].character = 3;
            setup.preemptive = formation == 3;
            let mut server = rules.build(&setup).unwrap();
            let log = serve_limits(&mut server, [3u64, 11, 40][seed as usize % 3], 4 + (seed as usize % 5));
            let outcome = server.outcome().unwrap_or_else(|| panic!("seed {seed} never ended"));
            let replay = replay_all(&rules, &setup, &log).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert_eq!(replay.state(), &server, "seed {seed}: the replayed engine IS the server's engine");
            let rewards = replay.expected_rewards();
            replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            let last = log.last().expect("something happened");
            match outcome {
                Outcome::Escaped => {
                    escaped += 1;
                    assert!(rewards.is_empty(), "an escape pays nothing");
                    // The message of the escape carries the command and no terminal event; BattleEnd follows it.
                    assert_eq!(last.2.map(|c| c.command), Some(Command::Escape), "seed {seed}");
                    assert!(!last.1.iter().any(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat | BattleEvent::EscapeFailed { .. })));
                    // A server that announced another ending, or a payment for the escape, is caught.
                    assert!(matches!(replay.check_end(Outcome::Victory, &[]), Err(Divergence::OutcomeDiffers { .. })));
                    let paid = vec![BattleReward::new(AccountId(20), 1, 0, 0)];
                    assert!(matches!(replay.check_end(Outcome::Escaped, &paid), Err(Divergence::RewardsDiffer { .. })));
                }
                Outcome::Victory => {
                    won += 1;
                    assert_eq!(last.1.last(), Some(&BattleEvent::Victory), "seed {seed}: the victory is the last event");
                }
                Outcome::Defeat => assert_eq!(last.1.last(), Some(&BattleEvent::Defeat), "seed {seed}"),
            }
            for (_, events, _) in &log {
                for e in events {
                    match e {
                        BattleEvent::LimitGauge { value, .. } => {
                            assert!(*value <= 255);
                            gauges += 1;
                        }
                        BattleEvent::EscapeFailed { .. } => failed += 1,
                        BattleEvent::Action { action: ActionRef::Limit(_), .. } => limits_used += 1,
                        // Nobody casts or drinks here: the only healing is the third member's (Aerith's) limit.
                        BattleEvent::Heal { actor, .. } if actor.0 == 2 => heals += 1,
                        _ => {}
                    }
                }
            }
        }
        assert!(escaped > 10 && won > 10, "{escaped} escapes and {won} victories in 120 battles");
        assert!(gauges > 100 && failed > 5 && limits_used > 100 && heals > 5, "{gauges} gauge events, {failed} failed escapes, {limits_used} limits, {heals} heals");
    }

    #[test]
    fn a_limit_or_an_escape_the_engine_would_refuse_is_caught_in_a_replay() {
        let rules = SyntheticRules { player_limits: true, ..SyntheticRules::default() };
        // Formation 2 cannot be run from; a member without a full gauge has no limit to give.
        let mut cannot = rules.build(&BattleSetup { preemptive: false, ..setup(2, 1) }).unwrap();
        assert!(!cannot.can_escape());
        assert_eq!(cannot.check_command(ActorId(0), &Command::Escape), Err(fmv_ff7_battle::rules::CommandError::GaugeNotFull(ActorId(0))));
        let mut forged = BattleReplay::start(&rules, &setup(2, 1)).unwrap();
        // The server "applied" an escape at tick 0, when nobody can act: the engine refuses it.
        let refused = forged.apply(0, &[], Some(&AppliedCommand { actor: ActorId(0), command: Command::Escape }));
        assert!(matches!(refused, Err(Divergence::CommandRefused(_))), "{refused:?}");
        let refused = forged.apply(0, &[], Some(&AppliedCommand { actor: ActorId(0), command: Command::Limit { target: Target::One(ActorId(1)) } }));
        assert!(matches!(refused, Err(Divergence::CommandRefused(_))), "{refused:?}");
        // Make the actor ready, then run: the formation forbids it.
        while cannot.ready_actors().is_empty() {
            cannot.step_ticks(10);
        }
        assert_eq!(cannot.check_command(ActorId(0), &Command::Escape), Err(fmv_ff7_battle::rules::CommandError::CannotEscape));
        assert_eq!(cannot.check_command(ActorId(0), &Command::Limit { target: Target::One(ActorId(1)) }), Err(fmv_ff7_battle::rules::CommandError::LimitNotReady(ActorId(0))));
    }

    #[test]
    fn a_tampered_spell_or_item_is_caught_like_any_other_event() {
        let rules = SyntheticRules::default();
        let setup = setup(1, 2);
        let mut server = rules.build(&setup).unwrap();
        let log = serve_mixed(&mut server, 5);
        // A heal that heals one more point than the engine would.
        let heal = log.iter().position(|(_, e, _)| e.iter().any(|ev| matches!(ev, BattleEvent::Heal { amount, .. } if *amount > 0)));
        let heal = heal.expect("some healing");
        let mut forged = log.clone();
        for ev in &mut forged[heal].1 {
            if let BattleEvent::Heal { amount, .. } = ev {
                *amount += 1;
                break;
            }
        }
        assert!(matches!(replay_all(&rules, &setup, &forged), Err(Divergence::EventsDiffer { .. })));
        // A command for a spell the character does not know is refused by the replay's engine.
        let magic = log.iter().position(|(_, _, c)| matches!(c, Some(AppliedCommand { command: Command::Magic { .. }, .. }))).expect("a spell");
        let mut unknown = log.clone();
        if let Some(applied) = unknown[magic].2.as_mut() {
            applied.command = Command::Magic { spell: 99, target: Target::One(ActorId(2)) };
        }
        assert!(matches!(replay_all(&rules, &setup, &unknown), Err(Divergence::CommandRefused(_))));
    }

    #[test]
    fn the_synthetic_fighters_know_cure_life_and_fire_and_the_battle_has_potions() {
        let state = SyntheticRules::default().build(&setup(0, 2)).unwrap();
        let spells = state.spells_for(ActorId(0));
        assert_eq!(spells.iter().map(|s| (s.spell, s.mp_cost, s.targets_allies)).collect::<Vec<_>>(), [(0, 5, true), (7, 34, true), (27, 4, false)]);
        assert_eq!(state.actor(ActorId(1)).unwrap().max_mp, 60);
        assert!(state.spells_for(ActorId(2)).is_empty());
        for item in [0u16, 3, 7, 20] {
            assert!(state.item_info(item).is_some(), "item {item}");
        }
        assert!(state.item_info(1).is_none());
        let rules = SyntheticRules::default();
        assert_eq!(rules.starting_inventory(), [InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)]);
        assert_eq!(rules.max_item_stack(), 99);
        // Drops: none unless asked; one per enemy of the formation, for certain, when asked.
        assert!(rules.roll_drops(1, 5).is_empty());
        let dropping = SyntheticRules { drop_item: Some(3), ..rules };
        assert_eq!(dropping.roll_drops(0, 5), [InventoryEntry::new(3, 1)]);
        assert_eq!(dropping.roll_drops(1, 5), [InventoryEntry::new(3, 2)]);
        assert!(dropping.roll_drops(77, 5).is_empty(), "no such formation, no drops");
    }

    #[test]
    fn the_end_check_ignores_the_items_a_server_rolled_but_not_the_numbers() {
        let rules = SyntheticRules::default();
        let setup = setup(1, 2);
        let mut server = rules.build(&setup).unwrap();
        let log = serve(&mut server, 7);
        let replay = replay_all(&rules, &setup, &log).unwrap();
        let mut rewards = replay.expected_rewards();
        assert_eq!(rewards.len(), 2);
        // A server that dropped items (which no replay can reproduce) is believed about them...
        rewards[0].items = vec![InventoryEntry::new(0, 2), InventoryEntry::new(7, 1)];
        rewards[1].items = vec![InventoryEntry::new(3, 1)];
        replay.check_end(Outcome::Victory, &rewards).expect("items are not replayed, so they are not compared");
        // ... but not about anything else.
        let mut greedy = rewards.clone();
        greedy[1].exp += 1;
        assert!(matches!(replay.check_end(Outcome::Victory, &greedy), Err(Divergence::RewardsDiffer { .. })));
        let mut lost = rewards.clone();
        lost.pop();
        assert!(matches!(replay.check_end(Outcome::Victory, &lost), Err(Divergence::RewardsDiffer { .. })));
    }

    #[test]
    fn drops_are_dealt_one_unit_at_a_time_to_the_living_in_roster_order() {
        let roster = [(AccountId(1), true), (AccountId(2), false), (AccountId(3), true)];
        let total = Reward { exp: 100, gil: 50, ap: 7 };
        let drops = [InventoryEntry::new(0, 3), InventoryEntry::new(7, 1)];
        let mut rewards = split_rewards(total, &roster);
        assign_drops(&mut rewards, &roster, &drops, 99);
        // Units: Potion, Potion, Potion, Phoenix Down -> living #1, #2, #1, #2. The dead one gets nothing.
        assert_eq!(rewards[0].items, [InventoryEntry::new(0, 2)]);
        assert!(rewards[1].items.is_empty());
        assert_eq!(rewards[2].items, [InventoryEntry::new(0, 1), InventoryEntry::new(7, 1)]);
        // The numbers of the split are untouched.
        assert_eq!(rewards[0].without_items(), BattleReward::new(AccountId(1), 50, 25, 7));
        // A stack never passes the cap; a zero cap keeps nothing.
        let mut capped = split_rewards(total, &roster[..1]);
        assign_drops(&mut capped, &roster[..1], &[InventoryEntry::new(0, 5), InventoryEntry::new(1, 1)], 3);
        assert_eq!(capped[0].items, [InventoryEntry::new(0, 3), InventoryEntry::new(1, 1)]);
        let mut none = split_rewards(total, &roster[..1]);
        assign_drops(&mut none, &roster[..1], &[InventoryEntry::new(0, 5)], 0);
        assert!(none[0].items.is_empty());
        // Items come out in ascending order whatever the order of the drops.
        let mut sorted = split_rewards(total, &roster[..1]);
        assign_drops(&mut sorted, &roster[..1], &[InventoryEntry::new(9, 1), InventoryEntry::new(2, 1)], 99);
        assert_eq!(sorted[0].items, [InventoryEntry::new(2, 1), InventoryEntry::new(9, 1)]);
        // Nobody alive (not a state a victory reaches), no drops, or a roster longer than the rewards: nothing, no panic.
        let dead = [(AccountId(1), false)];
        let mut nobody = split_rewards(total, &dead);
        assign_drops(&mut nobody, &dead, &drops, 99);
        assert!(nobody[0].items.is_empty());
        let mut empty = split_rewards(total, &roster);
        assign_drops(&mut empty, &roster, &[], 99);
        assert!(empty.iter().all(|r| r.items.is_empty()));
        let mut short = split_rewards(total, &roster[..1]);
        assign_drops(&mut short, &roster, &drops, 99);
        assert_eq!(short.len(), 1);
    }

    #[test]
    fn an_enemy_drops_at_most_one_item_by_its_own_rates_and_never_steals() {
        // MP of the first Midgar fights: a drop (Potion, 8 in 64) and a steal (rate 0x88 is a steal chance).
        let mp = ([8u8, 0x88, 0xFF, 0xFF], [0u16, 20, 0xFFFF, 0xFFFF]);
        let outcomes: Vec<Vec<InventoryEntry>> = (0..4000u64).map(|seed| roll_enemy_drops(seed, &[mp], 99)).collect();
        // Never the stolen item (20), only ever one Potion, and about 8 in 64 of the seeds drop it.
        assert!(outcomes.iter().all(|d| d.is_empty() || d == &[InventoryEntry::new(0, 1)]));
        let dropped = outcomes.iter().filter(|d| !d.is_empty()).count();
        assert!((350..650).contains(&dropped), "{dropped} of 4000 (expected about 500)");
        // The same seed always rolls the same.
        assert_eq!(roll_enemy_drops(77, &[mp, mp], 99), roll_enemy_drops(77, &[mp, mp], 99));
        // Rate 0 never drops, but its slot still costs a roll; the next slot is tried when it fails.
        let second_slot = ([0u8, 63, 0, 0], [5u16, 6, 7, 8]);
        let rolls: Vec<_> = (0..500u64).map(|seed| roll_enemy_drops(seed, &[second_slot], 99)).collect();
        assert!(rolls.iter().all(|d| d.is_empty() || d == &[InventoryEntry::new(6, 1)]), "the first slot has no chance");
        assert!(rolls.iter().filter(|d| !d.is_empty()).count() > 450, "63 in 64");
        // Two enemies with the same drop add up (one each), the result is ascending, and the cap holds.
        let certain = ([63u8, 0xFF, 0xFF, 0xFF], [3u16, 0xFFFF, 0xFFFF, 0xFFFF]);
        let other = ([63u8, 0xFF, 0xFF, 0xFF], [1u16, 0xFFFF, 0xFFFF, 0xFFFF]);
        let many = roll_enemy_drops(1, &[certain, other, certain, certain, certain, certain, certain, certain], 99);
        let total: u32 = many.iter().map(|e| u32::from(e.count)).sum();
        assert!(many.windows(2).all(|w| w[0].item < w[1].item) && (4..=8).contains(&total), "{many:?}");
        let capped = roll_enemy_drops(1, &[certain; 32], 2);
        assert!(capped.iter().all(|e| e.count <= 2), "{capped:?}");
        // An item outside the item table (a weapon, 128 and up) is not kept, and still ends the enemy's rolls.
        let weapon = ([63u8, 63, 0xFF, 0xFF], [200u16, 4, 0xFFFF, 0xFFFF]);
        let first_slot_succeeds = |seed: u64| Rng::with_stream(seed, DROP_STREAM).below(DROP_DIE) < 63;
        let wins: Vec<u64> = (0..300u64).filter(|s| first_slot_succeeds(*s)).collect();
        assert!(wins.len() > 250);
        for seed in wins {
            assert!(roll_enemy_drops(seed, &[weapon], 99).is_empty(), "seed {seed}: the weapon is lost and the enemy is done");
        }
        // Empty slots, no enemies and a zero cap drop nothing.
        assert!(roll_enemy_drops(1, &[([63; 4], [0xFFFF; 4])], 99).is_empty());
        assert!(roll_enemy_drops(1, &[], 99).is_empty());
        assert!(roll_enemy_drops(1, &[certain], 0).is_empty());
    }

    #[test]
    fn a_lost_battle_replays_and_pays_nothing() {
        let rules = SyntheticRules::default();
        let setup = setup(2, 2);
        let mut server = rules.build(&setup).unwrap();
        let log = serve(&mut server, 7);
        assert_eq!(server.outcome(), Some(Outcome::Defeat));
        let replay = replay_all(&rules, &setup, &log).unwrap();
        assert_eq!(replay.state(), &server);
        assert!(replay.expected_rewards().is_empty());
        replay.check_end(Outcome::Defeat, &[]).unwrap();
    }

    #[test]
    fn a_dead_member_is_paid_nothing_whoever_is_connected() {
        let rules = SyntheticRules::default();
        // Find (deterministically) a won battle in which the brute killed someone.
        let found = (0..400u64).find_map(|seed| {
            let setup = BattleSetup { seed, ..setup(4, 3) };
            let mut server = rules.build(&setup).unwrap();
            let log = serve(&mut server, 5);
            let replay = replay_all(&rules, &setup, &log).ok()?;
            let roster = replay.roster();
            (server.outcome() == Some(Outcome::Victory) && roster.iter().any(|r| !r.1)).then_some((replay, roster))
        });
        let (replay, roster) = found.expect("some seed wins with a loss");
        let rewards = replay.expected_rewards();
        // One entry per roster member, in roster order, whoever is connected; the dead earn nothing
        // and the living share the experience.
        assert_eq!(rewards.iter().map(|r| r.account).collect::<Vec<_>>(), roster.iter().map(|r| r.0).collect::<Vec<_>>());
        let alive = roster.iter().filter(|r| r.1).count() as u32;
        assert!((1..3).contains(&alive));
        for (reward, (_, is_alive)) in rewards.iter().zip(&roster) {
            if *is_alive {
                assert_eq!((reward.exp, reward.gil, reward.ap), (12 / alive, 20 / alive, 2));
            } else {
                assert_eq!((reward.exp, reward.gil, reward.ap), (0, 0, 0));
            }
        }
        replay.check_end(Outcome::Victory, &rewards).unwrap();
    }

    #[test]
    fn tampering_with_the_log_is_caught_where_it_happens() {
        let rules = SyntheticRules::default();
        let setup = setup(1, 2);
        let mut server = rules.build(&setup).unwrap();
        let log = serve(&mut server, 5);
        let damage = log
            .iter()
            .position(|(_, e, _)| e.iter().any(|ev| matches!(ev, BattleEvent::Damage { .. })))
            .expect("some damage");

        // One damage number is different.
        let mut forged = log.clone();
        for ev in &mut forged[damage].1 {
            if let BattleEvent::Damage { amount, .. } = ev {
                *amount += 1;
                break;
            }
        }
        match replay_all(&rules, &setup, &forged) {
            Err(Divergence::EventsDiffer { index, local: Some(_), server: Some(_), .. }) => assert!(index < forged[damage].1.len()),
            other => panic!("{other:?}"),
        }

        // A message is missing: the next one is for a tick that does not follow.
        let mut lost = log.clone();
        lost.remove(0);
        assert!(replay_all(&rules, &setup, &lost).is_err());

        // A message arrives twice: time would run backwards.
        let mut twice = log.clone();
        twice.insert(1, log[0].clone());
        assert!(matches!(replay_all(&rules, &setup, &twice), Err(Divergence::TickBackwards { .. } | Divergence::EventsDiffer { .. })));

        // A command the engine would refuse (the actor is not ready at that tick).
        let mut early = log.clone();
        let first_command = early.iter().position(|(_, _, c)| c.is_some()).unwrap();
        early[first_command].0 = 0;
        assert!(replay_all(&rules, &setup, &early).is_err());

        // A tick far in the future is a tick mismatch, not a hang or a panic.
        let mut replay = BattleReplay::start(&rules, &setup).unwrap();
        let huge = replay.apply(u64::MAX, &[], None).unwrap_err();
        assert!(matches!(huge, Divergence::TickMismatch { .. } | Divergence::EventsDiffer { .. }), "{huge}");

        // A peer with other game data builds another battle and disagrees at once.
        let weaker = SyntheticRules { enemy_hp: 100, ..SyntheticRules::default() };
        let divergence = replay_all(&weaker, &setup, &log).unwrap_err();
        assert!(!divergence.to_string().is_empty());
    }

    #[test]
    fn battle_data_that_is_not_game_data_is_an_error() {
        assert!(matches!(Ff7BattleData::from_bytes(b"", b""), Err(Ff7BattleDataError::Scene(_))));
        assert!(matches!(Ff7BattleData::from_bytes(&[0u8; 100], &[0u8; 10]), Err(Ff7BattleDataError::Scene(_))));
        let missing = std::path::Path::new("/definitely/not/here/scene.bin");
        let err = Ff7BattleData::open(missing, missing).unwrap_err();
        assert!(matches!(err, Ff7BattleDataError::Io { what: "scene.bin", .. }), "{err}");
    }

    #[test]
    fn the_data_fingerprint_binds_both_files_and_their_boundary() {
        let a = battle_data_fingerprint(b"ab", b"c");
        assert_eq!(a, battle_data_fingerprint(b"ab", b"c"));
        assert_ne!(a, battle_data_fingerprint(b"a", b"bc"), "moving the boundary changes it");
        assert_ne!(a, battle_data_fingerprint(b"ab", b"d"));
        assert_ne!(a, battle_data_fingerprint(b"xb", b"c"));
        assert_ne!(a, SyntheticRules::FINGERPRINT);
    }

    #[test]
    fn rejections_say_what_to_do() {
        for r in [
            BattleRejection::Unsupported,
            BattleRejection::DebugDisabled,
            BattleRejection::AlreadyInBattle,
            BattleRejection::UnknownFormation,
            BattleRejection::NotInBattle,
            BattleRejection::NotYourActor,
            BattleRejection::GaugeNotFull,
            BattleRejection::ActorDead,
            BattleRejection::InvalidTarget,
            BattleRejection::TargetDead,
            BattleRejection::BattleOver,
            BattleRejection::TargetNotDead,
            BattleRejection::UnknownSpell,
            BattleRejection::NotEnoughMp,
            BattleRejection::UnknownItem,
            BattleRejection::OutOfItems,
            BattleRejection::NoLimit,
            BattleRejection::LimitNotReady,
            BattleRejection::CannotEscape,
        ] {
            assert!(!r.to_string().is_empty());
        }
        assert!(BattleRejection::CannotEscape.to_string().contains("escape"));
        assert!(BattleRejection::DebugDisabled.to_string().contains("--debug-encounters"));
        assert_eq!(BattleId(4).to_string(), "battle#4");
    }

    #[test]
    fn the_synthetic_rules_have_a_made_up_experience_curve_and_ff7s_limits() {
        let rules = SyntheticRules::default();
        assert_eq!(rules.exp_for_level(0, 1), Some(0));
        // A step of `4 * level`: 20 from level 5 to 6, 24 from 6 to 7.
        assert_eq!((5..=7).map(|l| rules.exp_for_level(0, l).unwrap()).collect::<Vec<_>>(), [40, 60, 84]);
        assert_eq!(rules.exp_for_level(0, 0), Some(0), "levels are clamped to 1..=99");
        assert_eq!(rules.exp_for_level(8, 200), rules.exp_for_level(8, 99));
        assert_eq!(rules.exp_for_level(9, 5), None, "there is no tenth character");
        assert_eq!(rules.starting_exp(0, 5), 40, "by default a character starts at the floor of its level");
        assert_eq!(rules.starting_level(0), 5);
        assert_eq!(
            rules.progress_limits(),
            ProgressLimits { max_level: 99, max_exp: 99_999_999, max_gil: 9_999_999, max_ap: 16_777_215, max_limit: 255 }
        );
    }

    /// The new trait methods have defaults, so a rule set written before progress existed still
    /// compiles and behaves: no curve (nobody levels), nothing to start with, FF7's caps.
    #[test]
    fn a_rule_set_without_experience_curves_never_levels_anyone() {
        struct Bare;
        impl BattleRules for Bare {
            fn build(&self, _setup: &BattleSetup) -> Result<BattleState, BattleBuildError> {
                Err(BattleBuildError::EmptyParty)
            }
            fn data_fingerprint(&self) -> Fingerprint {
                Fingerprint([1; 32])
            }
            fn formation(&self, _id: u16) -> Option<FormationInfo> {
                None
            }
            fn starting_level(&self, _character: u8) -> u8 {
                3
            }
            fn default_formation(&self) -> Option<u16> {
                None
            }
        }
        let bare = Bare;
        assert_eq!(bare.exp_for_level(0, 5), None);
        assert_eq!(bare.starting_exp(0, 5), 0);
        assert_eq!(bare.progress_limits(), ProgressLimits::FF7);
        assert_eq!(bare.formation_scene(0), None, "no placement data unless the rule set has some");
    }

    /// A decompressed scene (7808 bytes) laid out as the `scene.bin` module documents it, with
    /// made-up numbers: enemy ids 7 and 9, and in formation slot 1 two entries (a, b) and a camera.
    fn raw_scene() -> Vec<u8> {
        let mut raw = vec![0u8; 7808];
        let mut put = |at: usize, bytes: &[u8]| raw[at..at + bytes.len()].copy_from_slice(bytes);
        for (k, id) in [7u16, 9, 0xFFFF].iter().enumerate() {
            put(k * 2, &id.to_le_bytes());
        }
        // Battle setup of slot 1 (20 bytes at 0x08 + 20): location 5, no chained formation, layout 2.
        put(0x08 + 20, &5u16.to_le_bytes());
        put(0x08 + 20 + 2, &0xFFFFu16.to_le_bytes());
        put(0x08 + 20 + 0x12, &[2]);
        // Cameras of slot 1 (48 bytes at 0x58 + 48): the idle one stands at (1000, -200, 300) looking at (10, -20, 30).
        for (i, v) in [1000i16, -200, 300, 10, -20, 30, 7, 7, 7, 8, 8, 8].iter().enumerate() {
            put(0x58 + 48 + i * 2, &v.to_le_bytes());
        }
        // Formation slot 1 (96 bytes at 0x118 + 96): entry 0 is enemy 9 at (-40, 0, -500) row 2, entry 2 enemy 7.
        let entry = |k: usize| 0x118 + 96 + k * 16;
        put(entry(0), &9u16.to_le_bytes());
        for (i, v) in [-40i16, 0, -500].iter().enumerate() {
            put(entry(0) + 2 + i * 2, &v.to_le_bytes());
        }
        put(entry(0) + 8, &2u16.to_le_bytes());
        for k in [1usize, 3, 4, 5] {
            put(entry(k), &0xFFFFu16.to_le_bytes());
        }
        put(entry(2), &7u16.to_le_bytes());
        for (i, v) in [60i16, -300, -700].iter().enumerate() {
            put(entry(2) + 2 + i * 2, &v.to_le_bytes());
        }
        put(entry(2) + 8, &1u16.to_le_bytes());
        raw
    }

    #[test]
    fn a_formation_scene_lists_the_placements_in_the_engines_enemy_order_and_the_idle_camera() {
        use fmv_ff7_battle::scene::{BattleLayout, FormationRef, Scene};
        let scene = Scene::parse(&raw_scene(), 0).unwrap();
        let formation = FormationRef { scene: &scene, slot: 1, global_index: 1 };
        let drawn = FormationScene::of(&formation);
        assert_eq!((drawn.location, drawn.layout), (5, BattleLayout::BackAttack));
        assert_eq!(drawn.camera, CameraPose { position: [1000, -200, 300], target: [10, -20, 30] });
        // The engine builds its enemies from the same entries in the same order: the k-th enemy of a
        // built battle stands where the k-th placement says.
        assert_eq!(
            drawn.enemies,
            [
                EnemyPlacement { enemy_id: 9, position: [-40, 0, -500], row: 2 },
                EnemyPlacement { enemy_id: 7, position: [60, -300, -700], row: 1 },
            ]
        );
        let engine = fmv_ff7_battle::rules::enemies_for_formation(&scene, 1);
        assert_eq!(engine.len(), drawn.enemies.len(), "one placement per engine enemy");
    }
}
