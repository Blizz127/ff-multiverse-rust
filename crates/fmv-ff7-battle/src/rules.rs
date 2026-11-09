//! Deterministic, server-authoritative FF7-style battle rules (milestone M2-c, extended in M2-e with
//! magic, items, Defend and the enemies' own attacks).
//!
//! The engine is a pure state machine: [`BattleState`] holds the combatants, the ATB gauges
//! and a seeded [`Rng`]; it advances only through [`BattleState::step`] /
//! [`BattleState::step_ticks`] and [`BattleState::submit_command`]. It never reads a clock, a
//! thread-local, the OS RNG or floating point, so a server and any client that replays the
//! same `(seed, party, enemies, config)` plus the same ordered inputs produce identical
//! events. The server stays authoritative: clients should treat server events as truth and
//! use local replay only for prediction.
//!
//! # Scope
//!
//! Two sides: a *party* (one or more player characters) against *enemies*. A party member whose gauge
//! is full gives one [`Command`]:
//!
//! * [`Command::Attack`]: the physical weapon attack on one living enemy;
//! * [`Command::Magic`]: a spell the character knows ([`Combatant::spells`], see
//!   [`BattleState::spells_for`]), which costs MP, on one ally (a restoring spell) or one enemy (a
//!   damaging one), or on a whole side (see "Whole-side targeting" below);
//! * [`Command::Item`]: one item of the engine's [`ItemTable`] on one ally (a potion, an Ether, a
//!   Phoenix Down on a dead one) or one enemy (a damage item), or on a whole side (Megalixir, Shrapnel).
//!   **The engine knows nothing of an inventory**: whether the account owns the item is the server's
//!   business (it checks the count, submits, and takes the item only when the engine accepts), so a
//!   client replaying the battle never needs to know a count;
//! * [`Command::Defend`]: physical damage taken is halved until the actor's gauge is full again;
//! * [`Command::Limit`]: the character's limit break (see "Limit breaks" below), once its limit gauge
//!   is full, on one enemy or the whole party (Healing Wind);
//! * [`Command::Escape`]: the actor spends its turn running (see "Escape" below).
//!
//! An enemy that has an AI script the engine can follow ([`crate::ai`]) does what the script says: it picks
//! its attacks and targets, or lets the turn pass (see "Enemy turns"). Any other enemy (no script, one the
//! interpreter does not follow, an answer the rules cannot carry out) uses the plain behaviour below.
//!
//! An enemy whose gauge fills picks one random living party member and one of its own `scene.bin`
//! attacks ([`Combatant::actions`]: the damaging ones the rules model), or a plain attack when it has
//! none. Every action, party or enemy, is announced by a [`BattleEvent::Action`] before its effects
//! ([`BattleEvent::MpSpent`], [`BattleEvent::Damage`], [`BattleEvent::Heal`], ...). Not modelled:
//! the statuses [`crate::status`] lists as left out (Confusion, Berserk, Frog, ...), summons, back-attack
//! damage bonus, covering, enemy rows, AI scripts other than Initialize, Main and the counters, AP/EXP growth,
//! party elemental resistances, limit levels above 1 (see [`crate::limit`]).
//!
//! # Statuses
//!
//! [`crate::status`] is the model (the statuses, what a record's status fields mean, the clocks and the
//! durations, with their sources); this is where they act. A combatant has [`Combatant::status`] and
//! [`Combatant::statuses`] says everything it has, Death and Near-death included. Every tick, before the gauges
//! fill, each living combatant's statuses run: a Poison or Regen turn is a [`BattleEvent::StatusTick`], a timed
//! status that runs out a [`BattleEvent::StatusEnded`] (in actor order), and a combatant that Poison knocks out
//! gets the usual [`BattleEvent::Death`] (and the battle its outcome). An action's status effect arrives as
//! [`BattleEvent::StatusInflicted`] (or `StatusEnded` for a cure, or for Sleep woken by a physical blow), after
//! the effects of the action on that target.
//!
//! * **Who acts.** Sleep, Stop and Paralysis hold a turn: the gauge of a sleeper keeps filling, the one of a
//!   stopped or paralysed combatant is frozen (Haste doubles the gauge and Slow halves it, Stop and Paralysis
//!   freeze it: [`crate::status::Pace`]), and [`BattleState::ready_actors`] leaves out whoever cannot take its turn.
//!   An enemy that cannot act waits with its full gauge and acts the tick it can, and a blow that leaves it asleep,
//!   stopped or paralysed gets no counter (it still notes who hit it; a physical blow wakes a sleeper first, so that
//!   one answers: an engine choice, not checked against the original); a party member's command is
//!   refused with [`CommandError::CannotAct`]. Silence refuses the Magic command with [`CommandError::Silenced`].
//! * **Damage and hit rate.** After the formulas: Barrier halves a physical blow and MBarrier a magical one,
//!   Sadness takes 30% off either ([`crate::status::damage_after_statuses`]); Fury takes 30% off the hit rate of the
//!   one who has it and Darkness halves the hit rate of a party member's Attack command
//!   ([`crate::status::hit_chance_after_statuses`]); Sadness halves the limit gauge a blow fills, Fury doubles it and
//!   Paralysis stops it. A physical blow that does damage wakes a sleeper; a magical one does not. A knockout
//!   removes every status.
//!
//! # Enemy turns
//!
//! An enemy with scripts it can follow ([`crate::ai::choose`]) plays what its Main script queued:
//!
//! * **Queued attacks** are played one after the other in the order queued (the original's action queue is
//!   first in, first out, see [`crate::ai::vm`]), each announced by its own [`BattleEvent::Action`], paid for
//!   with its own MP ([`BattleEvent::MpSpent`]) and played with its own rolls. Together they are one turn:
//!   the enemy's gauge empties once. Their MP must be affordable in all, or the turn falls back.
//! * **Whom** an attack reaches is the script's target mask, as a [`Target`] named from the enemy's own side
//!   ([`Target::One`]: a party member; [`Target::AllEnemies`]: the whole party; [`Target::AllAllies`] is not
//!   played for a damaging attack and the turn falls back). A whole-party attack is one announcement and one
//!   payment, then `resolve_action` plays it on every living member in ascending order with its
//!   own rolls each, and the 2/3 split of "Whole-side targeting" below. This engine treats the script's mask
//!   as authoritative (an engine choice, NOT checked against the original): the attack record's own
//!   targeting ([`Aim`], which is read as the rule of the player's menu and which `aim_at` checks a command
//!   against) is not consulted, so a one-target attack aimed at the whole party and a whole-side one aimed
//!   at one member are played as the script says (refusing them would make five more of the owner's enemies
//!   fall back to the plain behaviour). An attack whose
//!   record aims at a random target ([`Aim::Random`], which `aim_at` refuses) is played on ONE party member:
//!   the single member the script named, or, for a whole-party mask, one of the living members drawn with
//!   `below(n)`.
//! * **One target that has fallen** by the time its attack comes up (the script chose it earlier, perhaps
//!   turns ago, and kept the mask in its memory) makes the attack **fizzle**: nothing is announced, spent or
//!   drawn, and the script's memory (stored before the first attack) is kept, so that its cycle goes on.
//! * An attack of the scene that the rules cannot play (no damage, healing, a status) is announced and does
//!   nothing, whomever it is aimed at.
//! * **Counters.** When a party member's action ([`Command::Attack`], [`Command::Magic`], [`Command::Item`] or
//!   [`Command::Limit`]) has been played on its targets, every enemy among them that has scripts notes who
//!   hit it and answers with its counter scripts ([`crate::ai`], "Counters": the Death counter if the blow
//!   killed it, else the General counter and the Physical or Magical one by the [`ActionKind`] of the
//!   action). What a counter queues is played like an enemy turn (without touching the enemy's gauge), as
//!   events right after the action's own, in the same batch ([`BattleState::submit_command`] returns them),
//!   so a client that replays the command gets them from its own engine. Nothing answers a healing
//!   action, and nothing once the battle has an outcome.
//!
//! # Whole-side targeting
//!
//! A command that aims at more than one target gives a [`Target`]: [`Target::One`] (the one actor, as
//! before), [`Target::AllEnemies`] or [`Target::AllAllies`]. The two sides are named **relative to the
//! actor** (an enemy's "allies" are the other enemies), so the same words serve a party member's
//! command and an enemy's action; for a party member the enemies are the enemy side and the allies the
//! party. A whole-side target means every combatant of that side the action can hit: the living ones,
//! or the fallen for a revival.
//!
//! *Which way an action may be aimed* is its [`Aim`], decoded from the record's targeting bits
//! (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data>): [`Aim::One`] allows [`Target::One`]
//! only, [`Aim::Side`] ("All Tar (NS)": Ultima, Megalixir, Healing Wind) a whole side only, and
//! [`Aim::OneOrSide`] ("All Tar/1 Tar": Cure, Fire) either. The side must be the one the action aims at:
//! a restoring action at the allies, a damaging one at the enemies, as for one target. A way the
//! action does not allow is [`CommandError::InvalidReach`].
//!
//! *The All materia.* In FF7 a **spell** of the [`Aim::OneOrSide`] kind needs an All materia paired with
//! its magic materia to be cast on a whole side, and an All materia of level `n` allows that `n` times per
//! battle (Terence Fergusson's *FF7 Party Mechanics Guide*, "All": "Use paired Magic against all once per
//! battle" at level 1 up to five times at level 5; the abilities that need no All materia are the ones
//! that are not toggles, and every item and limit break). Here [`Combatant::all_left`] counts what the
//! caster has left: [`BattleState`] takes one off when a [`Target::AllEnemies`] or [`Target::AllAllies`]
//! spell of that kind is accepted (also when only one target was left to hit: the choice spent it), a
//! caster with none left gets [`CommandError::NoAllLeft`], and the number a battle starts with is an
//! input of the combatant ([`Combatant::from_character`] reads it from the kernel's All materia, see
//! [`STARTER_ALL_MATERIA`]). It is battle state only: it never travels and nothing keeps it.
//!
//! *Damage.* Every target is resolved like a one-target action would be (its own rolls, elements,
//! affinities, row and Defend), in ascending [`ActorId`] order, and an action that
//! [splits](ActionSpec::splits) deals 2/3 of what it would to each of several targets (Fergusson's
//! *Battle Mechanics* FAQ; see [`crate::action`], "Hitting a whole side"). The announcement, and the MP,
//! come once; then each target has its own [`BattleEvent::Damage`], [`BattleEvent::Miss`],
//! [`BattleEvent::Heal`], [`BattleEvent::Revive`] (and the [`BattleEvent::Death`] and
//! [`BattleEvent::LimitGauge`] that follow its blow). A blow that ends the battle is followed by
//! [`BattleEvent::Victory`] or [`BattleEvent::Defeat`] at once, and no target is resolved after it.
//!
//! # Formula sources and provenance
//!
//! * **ATB gauge fill rate.** From the FF7 Project blog's "Stats & Battle Timers"
//!   (<https://finalfantasy7project.wordpress.com/2011/11/24/stats/>), which credits Terence
//!   Fergusson's *FF7 Battle Mechanics* FAQ
//!   (<https://gamefaqs.gamespot.com/ps/197341-final-fantasy-vii/faqs/22395>; the FAQ itself
//!   could not be fetched, so this crate relies on the blog's quotation of it):
//!   `speed_value = 32768 / (120 + battle_speed * 15 / 8)`, `v_timer_increase = 2 * speed_value`,
//!   `normal_speed = round(average party Dex) + 50`; party members add
//!   `(Dex + 50) * v_timer_increase / normal_speed` to their turn timer every tick, enemies
//!   `Dex * v_timer_increase / normal_speed`; a combatant can act when the timer reaches 65535.
//!   A pastebin note, "FF7 Battle Timer" (<https://pastebin.com/nVBvNNfP>), restates the
//!   timer definitions (tick, global timer, V-timer, turn timer normalised by the party's
//!   average *base* Dexterity, C-timer). Secondary sources, therefore treat the ATB rates as
//!   *documented but not independently verified*; this crate uses base Dexterity only
//!   (no Speed Plus / Choco Feather style modifiers).
//! * **Physical damage ("X1" formula), hit chance and critical chance.** From the
//!   disassembly-derived pseudocode at <https://wiki.ffrtt.ru/index.php/FF7/DamageFormula>
//!   and the formula table at <https://wiki.ffrtt.ru/index.php/FF7/Battle/Damage_Calculation>
//!   (English 1.02 build), cross-checked against the commonly quoted form
//!   `Base = Att + ((Att + Lvl) / 32) * ((Att * Lvl) / 32)`,
//!   `Damage = Power * (512 - Def) * Base / (16 * 512)`:
//!   critical x2, short-range row penalty /2, then random variance `[3841..=4096] / 4096`
//!   with a minimum of 1.
//! * **Magic, healing, items.** The other formulas of the damage-calculation byte (`(Power/16) *
//!   ((Lvl+Mag) * 6)` for spells, `22 * Power + 6 * (Lvl+Mag)` for Cure, `20 * Power` for potions, a
//!   fraction of the target's max or current HP for Life, Full Cure, X-Potion and Demi, ...) are decoded
//!   in [`crate::action`], which also says which records are not modelled. Magic uses the Magic stat and
//!   the magic defense; it cannot be a critical hit; the random variance follows the formulas that have
//!   one. Defend, elements and the enemies' attack choice are explained below.
//! * **Row rule**: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data> (bit `0x20`,
//!   "short range"). Only the standard physical formula is row-sensitive.
//!
//! # RNG draw order (part of the determinism contract)
//!
//! * Construction, [`StartMode::Normal`] only: one `range_inclusive(0, 0x7FFF)` per combatant
//!   in [`ActorId`] order (party first, then enemies) for the initial gauge.
//! * Construction, after those gauge draws: the Initialize scripts of the enemies that have one
//!   ([`crate::ai`]), in actor order, each with exactly the draws of its script (one `next_u16()` per
//!   `RANDOM`, one `below(n)` per `RANDOM_BIT` of a mask of `n >= 2` bits: about a third of the owner's
//!   enemies draw something here, to start a counter at a random value). An Initialize script that cannot be
//!   followed draws nothing, and its enemy gives its scripts up for good.
//! * Enemy turn with an AI script that can be followed ([`crate::ai`]): exactly the draws of the script (one
//!   `next_u16()` per `RANDOM`, one `below(n)` per `RANDOM_BIT` of a mask of `n >= 2` bits), then, for each
//!   attack it queued in order, first (only for an attack whose record aims at a random target, a whole-party
//!   mask, and `n >= 2` living members) `below(n)` to pick its one member, then the action's own draws below
//!   (on each living party member in ascending order for a whole-party attack; none for an attack that
//!   fizzles or that the rules cannot play). A turn the script cannot be followed on
//!   ([`crate::ai::choose`] gives `None`) draws nothing for it and takes the next bullet.
//! * [`Command::Attack`], [`Command::Magic`], [`Command::Item`] and [`Command::Limit`] on an enemy that has
//!   scripts: after the action's own draws below, the draws of its counters, enemy by enemy in ascending
//!   position when the action has several targets: the Death counter alone if the blow killed it, else the
//!   General counter and then the Physical or the Magical one, each with exactly the draws of its script and
//!   then of the attacks it queued, as an enemy turn draws them. A counter that cannot be followed or carried
//!   out draws nothing. Nothing is drawn for an enemy with no scripts, for a healing action, or once the
//!   battle has an outcome.
//! * Enemy turn: first `below(living_party_count)` to pick the target (living party members
//!   in id order); then, **only if the enemy has two or more usable attacks** (those it has the MP
//!   for), `below(usable_count)` to pick one (so an enemy with one attack, or with none, draws exactly
//!   what it drew before attacks existed); then the action's own draws below.
//! * A physical action that rolls to hit, in this order: `luck_roll = range_inclusive(0, 99)`;
//!   `miss_roll = next_u16() * 99 / 65535 + 1`; **only if it hits**, and only if it can be a critical
//!   hit, `crit_roll = next_u16() * 99 / 65535 + 1`; then, if its formula has variance (the standard
//!   one does), `variance = range_inclusive(0, 255)`. A physical action that never misses
//!   (damage-calculation byte `0x0X` / `0x3X`) draws nothing for the roll.
//! * A magical action that rolls to hit: `miss_roll = next_u16() * 99 / 65535 + 1` (no luck roll);
//!   **only if it hits**, `variance = range_inclusive(0, 255)` when its formula has variance.
//! * A restoring action (Cure, Life, a potion) never misses: it draws `variance` only if its formula
//!   has one (Cure's does; potions' and Life's do not).
//! * Draws never depend on the target's elemental affinity (a nullified or absorbed hit draws what a
//!   normal hit draws), on defense, on rows or on statuses.
//! * An action on **several targets** draws, for each target in ascending [`ActorId`] order, what the same
//!   action on that one target would draw (the rolls above, one set per target); the split of its damage
//!   and the spending of an All use are arithmetic and draw nothing, so an action on one target draws
//!   exactly what it did before whole-side targets existed.
//! * An action with a **status effect** ([`ActionSpec::status`]) draws, after its effects on a target that still
//!   stands, one `below(63)` for that target and inflicts (or cures, or toggles) when the draw is below the
//!   effect's chance ([`crate::status::StatusEffect::chance_among`]: out of 63, cut to 2/3 when a splitting action
//!   hits several targets); a target that was missed or knocked out draws nothing for it. A **status-only** action
//!   ([`Formula::None`]) first rolls to hit like a spell on each target it aims at the other side with (a magical
//!   one: `miss_roll`; a physical one: `luck_roll` then `miss_roll`, as above), and never against an ally or with an
//!   item. The draw is made before anything else is asked of the target, so an immune target (or one that
//!   already has the status) costs it too; a record whose statuses the rules do not play has no status effect at
//!   all ([`ActionSpec::status`] is `None`) and draws nothing. Poison, Regen and the timers draw nothing: the
//!   draws of every action without a status effect are exactly what they were before statuses.
//! * [`Command::Defend`] and every validation failure draw nothing.
//! * [`Command::Limit`] draws what the limit's action draws: a damaging limit never misses and cannot
//!   be a critical hit, so only its `variance = range_inclusive(0, 255)` (the standard formula has
//!   one, the spell formula too; Healing Wind has none and draws nothing).
//! * [`Command::Escape`] makes [`RUN_CHECKS_PER_TURN`] Run Checks in a row, stopping once the party is
//!   away. A Run Check of a battle whose escape is [`EscapeRule::sure`] draws nothing; any other draws
//!   `below(`[`RUN_CHECK_ODDS`]`)` and succeeds on 0. A battle with no escape rule and every refusal draw
//!   nothing. The gauge the limit fills is arithmetic on the damage and draws nothing.
//!
//! # Defend, elements, MP
//!
//! * **Defend** sets [`Combatant::defending`]; physical damage ([`ActionKind::Physical`]) taken while
//!   it is set is halved (after the row penalty, before the elemental multiplier and the variance).
//!   It ends when the actor's gauge fills again (its next turn), or when it dies.
//! * **Elements.** An action's element mask is matched against the target's
//!   [`Combatant::affinities`] (enemies only, from the record): weakness doubles the damage, a
//!   resistance halves it (both before the variance), nullify makes it 0 (reported as a
//!   [`BattleEvent::Damage`] of 0), absorb turns it into a [`BattleEvent::Heal`] of that amount, full
//!   heal restores the target completely and Death kills it. These never miss.
//! * **MP.** A spell costs the MP of its record, announced by [`BattleEvent::MpSpent`] right after the
//!   action. An enemy pays for its attacks the same way (and cannot use one it lacks the MP for).
//!   Heals are reported as the amount actually restored (never more than was missing).
//!
//! # Limit breaks
//!
//! Every character that has a limit ([`Combatant::limit_break`]; [`crate::limit`] says where its id,
//! divisor and effect come from) has a **limit gauge** `0..=`[`LIMIT_MAX`] ([`Combatant::limit`]). A hit
//! of an enemy on a living party member adds [`crate::limit::limit_gain`] of the damage dealt and is
//! announced by a [`BattleEvent::LimitGauge`] right after the [`BattleEvent::Damage`] (and after the
//! [`BattleEvent::Death`], when the hit killed: a knocked-out character loses its whole gauge, which
//! is announced with value 0). A character whose gauge is full and whose ATB gauge is full may give
//! [`Command::Limit`]: the engine announces [`BattleEvent::Action`] with [`ActionRef::Limit`], then
//! [`BattleEvent::LimitGauge`] with 0 (the gauge empties as the limit begins, before its effects, so
//! that a battle that the limit ends still ends on its [`BattleEvent::Victory`]), then the effects of
//! one action on one target. The **starting gauge is an input** like the level: the server keeps each
//! account's gauge and sends it with the party, and the gauge at the end of a battle is what the server
//! saves ([`BattleState::limit_gauge`]).
//!
//! # Escape
//!
//! FF7's escape does **not** depend on levels. Each formation has a *Run Difficulty* (`scene.bin`'s
//! escape counter: 1 for most fights, 2 or 3 for a back attack and some bosses; the scene's flags say
//! when the party may not run at all, which is the case of every counter of 9 or more in the owner's
//! file), and holding L1+R1 makes a *Run Check* every quarter of a turn or so (Terence Fergusson's
//! *Battle Mechanics* FAQ, as summarised by the Final Fantasy Wiki's "Escape (Final Fantasy VII)" and
//! GameFAQs answers; the FAQ's text itself could not be fetched here): a check lowers the difficulty by
//! one when the party had a preemptive attack or surrounds the enemy (a side attack), and otherwise
//! with a chance of 25%; the party gets away when the difficulty reaches 0.
//!
//! Here [`Command::Escape`] is the actor spending its turn running: it makes [`RUN_CHECKS_PER_TURN`] (4)
//! Run Checks, the difficulty that is left is kept for the rest of the battle (the party that tried
//! once is halfway out the next time), and when it reaches 0 the battle ends with [`Outcome::Escaped`]
//! (nothing is paid: no experience, gil, AP or drops). A failed attempt is announced by
//! [`BattleEvent::EscapeFailed`] and ends the turn like any command. [`EscapeRule`] is a battle's
//! definition like its items ([`BattleState::with_escape`]): the peers build it from their own
//! `scene.bin`. Without one a battle can be run from with difficulty 1 (a preemptive start makes it
//! sure). APPROXIMATION: the four checks per command stand in for the real timing (a turn is about
//! four quarter-turns at the default speed), so one attempt at difficulty 1 succeeds 68% of the time
//! (`1 - 0.75^4`), at difficulty 2 about 26%; holding the buttons is a menu entry here (the window can
//! only press keys).
//!
//! # APPROXIMATION summary
//!
//! * **Tick duration**: the documents give per-tick increments but not how long a tick is.
//!   [`DEFAULT_TICKS_PER_SECOND`] = 60 (a default-speed, average-Dex combatant then needs a
//!   little over six seconds to fill). Configurable through [`BattleConfig`].
//! * **Initial gauges**: not documented; [`StartMode::Normal`] draws a random fill in
//!   `0..=0x7FFF` (at most half full). Preemptive starts with a full party gauge and an
//!   empty enemy gauge, back attack the reverse (APPROXIMATION from the layout names).
//!   Layouts other than preemptive / back attack are treated as normal.
//! * **Critical test**: `chance >= roll` as in the FAQ (an exact `chance`% rate); the
//!   disassembly pseudocode uses a strict `>`, a difference of one roll value.
//! * **Magic hit chance**: the documents give none; a spell hits when its accuracy minus the target's
//!   magic evade percent (a party member's armor; enemies have none) is at least the roll. Nothing about
//!   level or luck.
//! * **Damage cap** [`MAX_DAMAGE`] = 9999 (community-documented, not in the cited pages).
//!   Arithmetic is exact 64-bit; the original's 32-bit overflow glitch is not reproduced.
//! * **Enemy attacks**: the original picks them with AI scripts, which [`crate::ai`] runs when it can
//!   (the plain behaviour here is its fallback). Here an enemy
//!   picks uniformly among its damaging scene attacks (see [`crate::action::EnemyAction`]) it can pay
//!   for, hitting one random living party member whatever the attack's own targeting says; a plain
//!   physical attack of the enemy's first such attack ([`Combatant::from_enemy`]) is the fallback. The
//!   Enemy Attack command is always row-sensitive (`short_range = true`), so physical enemy attacks are
//!   halved against the back row.
//! * **Party attack and defense** = Strength + weapon attack, Vitality + armor defense; magic attack
//!   is the Magic stat, magic defense Spirit + armor magic defense; the weapon's hit/crit rates and
//!   the armor's evade percentages come from the kernel. Cait Sith and Vincent have no starting
//!   equipment record, so they fight unarmed unless equipment ids are passed explicitly.
//! * **All materia** ([`Combatant::from_character`]): one All materia at level 1 (so once per battle) is
//!   counted for the whole grimoire, whichever magic materia a spell comes from, because the engine has no
//!   materia loadout to pair them in (see [`STARTER_ALL_MATERIA`]).
//! * **Spells** ([`Combatant::from_character`]): the spells of the magic materia equipped in the
//!   character's kernel starting record at the AP it starts with, plus a documented STARTER grimoire
//!   every character is given: the level-1 spells of Restore, Fire, Ice and Lightning
//!   ([`STARTER_MATERIA`]), because on the owner's Steam copy only Cloud (Lightning, Ice) and Red XIII
//!   (Fire) start with magic materia equipped.
//! * Enemy rows from formations are ignored; party rows are honoured.
//!
//! # Events, not state, go on the wire
//!
//! [`BattleEvent`], [`Command`], [`ActionRef`], [`ActorId`] and [`Outcome`] derive serde so they can
//! travel in protocol messages. [`Combatant`] and [`BattleState`] deliberately do not: they carry
//! numbers derived from the owner's game files, which are never sent over the network.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use crate::action::{
    affinity_response, element, ActionKind, ActionSpec, Aim, ElementAffinity, Elemental, EnemyAction,
    Formula, ItemData, ItemInfo, ItemKind, ItemTable, Spell, SpellInfo,
};
use crate::kernel::{CharacterId, Kernel, MateriaRecord};
pub use crate::limit::{limit_gain, LimitBreak, LimitInfo, LIMIT_MAX};
use crate::rng::Rng;
use crate::scene::{AffinityRate, AttackRecord, BattleLayout, EnemyRecord, Scene};
pub use crate::status::{Status, StatusSet, StatusState};
use crate::status::{self, Change, CHANCE_OUT_OF, POISON_DIVISOR, REGEN_DIVISOR};

/// Turn-timer value at which a combatant can act.
pub const GAUGE_MAX: u32 = 65_535;
/// Default battle speed setting (0 fastest .. 255 slowest).
pub const DEFAULT_BATTLE_SPEED: u8 = 128;
/// APPROXIMATION: ticks per second (see the module docs).
pub const DEFAULT_TICKS_PER_SECOND: u32 = 60;
/// Largest `dt_ms` a single [`BattleState::step`] call will consume; the rest is dropped.
pub const MAX_STEP_MS: u32 = 60_000;
/// Largest tick count a single [`BattleState::step_ticks`] call will process.
pub const MAX_STEP_TICKS: u64 = 60_000;
/// Combatants kept per side by [`BattleState::new`] (extras are dropped).
pub const MAX_PER_SIDE: usize = 32;
/// Power of the Attack command.
pub const ATTACK_COMMAND_POWER: u32 = 16;
/// Largest damage one hit can deal.
pub const MAX_DAMAGE: u32 = 9_999;
/// APPROXIMATION: upper bound of the random initial gauge of a normal start.
pub const INITIAL_GAUGE_MAX: u32 = 0x7FFF;
/// Run Checks one [`Command::Escape`] makes (see the module docs, "Escape").
pub const RUN_CHECKS_PER_TURN: u32 = 4;
/// A Run Check of a battle that is not [`EscapeRule::sure`] succeeds with a chance of 1 in this many.
pub const RUN_CHECK_ODDS: u32 = 4;
/// The materia every character's grimoire is built from on top of its own starting equipment (their ids
/// in the kernel's materia table): Restore (Cure), Fire, Ice and Lightning (Bolt), at level 1.
pub const STARTER_MATERIA: [u8; 4] = [53, 49, 50, 52];
/// The id of the All materia in the kernel's materia table (checked on the owner's Steam copy by its
/// real-data test): the support materia that lets the magic it is paired with hit a whole side, once per
/// battle at level 1 (see the module docs, "Whole-side targeting"). Every character is given one at level 1
/// for its whole grimoire, like the [`STARTER_MATERIA`], so [`Combatant::from_character`] starts with
/// [`MateriaRecord::all_uses`] of its level 1 in [`Combatant::all_left`].
pub const STARTER_ALL_MATERIA: u8 = 23;

/// Identifier of a combatant inside one battle: party members take ids `0..party`, enemies
/// follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ActorId(pub u8);

/// Whom an action is aimed at: one actor, or a whole side. The sides are named relative to the actor that
/// acts, so the same words serve a party member and an enemy (see the module docs, "Whole-side
/// targeting"). It travels in `Command::Magic`, `Command::Item` and `Command::Limit` (protocol v10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    /// This one actor.
    One(ActorId),
    /// Everyone on the other side from the actor (for a party member: the enemies).
    AllEnemies,
    /// Everyone on the actor's own side (for a party member: the party), itself included.
    AllAllies,
}

impl Target {
    /// The actor, when this is [`Target::One`].
    pub fn one(self) -> Option<ActorId> {
        match self {
            Target::One(id) => Some(id),
            Target::AllEnemies | Target::AllAllies => None,
        }
    }

    /// Whether this is a whole side.
    pub fn is_side(self) -> bool {
        !matches!(self, Target::One(_))
    }
}

/// Which side a combatant fights on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Party,
    Enemy,
}

/// Battle row. Back-row combatants deal and take half damage from short-range attacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Row {
    #[default]
    Front,
    Back,
}

/// Rewards granted for defeating an enemy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reward {
    pub exp: u32,
    pub gil: u32,
    pub ap: u32,
}

/// One fighter. All stats use the units of FF7's battle formulas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combatant {
    pub name: String,
    pub level: u8,
    pub hp: u32,
    pub max_hp: u32,
    pub mp: u32,
    pub max_mp: u32,
    /// "Att" of the damage formula (party: Strength + weapon attack; enemy: Strength).
    pub attack: u32,
    /// "Def" of the damage formula (party: Vitality + armor defense; enemy: Defense).
    pub defense: u32,
    /// "Mag" of the magic formulas (the Magic stat).
    pub magic: u32,
    /// Magic defense (party: Spirit + armor magic defense; enemy: its magic defense).
    pub magic_defense: u32,
    /// Dexterity (an enemy's speed byte): drives the ATB gauge and hit chance.
    pub dex: u32,
    pub luck: u32,
    /// Physical evade percentage (party: armor "Defense %"; enemy: evade stat).
    pub evade: u32,
    /// Magic evade percentage (party: armor "Magic Defense %"; enemies have none).
    pub magic_evade: u32,
    /// Hit rate (percent) of the Attack action (weapon accuracy or the enemy attack's rate).
    pub accuracy: u32,
    /// Power of the Attack action (16 for the party's Attack command).
    pub attack_power: u32,
    /// Critical rate bonus in percent (party weapons only).
    pub crit_bonus: u32,
    /// Short-range attacks are halved when either fighter is in the back row.
    pub short_range: bool,
    pub row: Row,
    /// Rewards for defeating this combatant (enemies).
    pub reward: Reward,
    /// The spells a party member can cast, by [`Spell::index`] (see [`BattleState::spells_for`]).
    pub spells: Vec<Spell>,
    /// The attacks an enemy picks from when its gauge fills (empty: a plain attack).
    pub actions: Vec<EnemyAction>,
    /// How the combatant answers elements (enemies only, from their record).
    pub affinities: Vec<ElementAffinity>,
    /// Assigned by [`BattleState`]; any value passed in is overwritten.
    pub side: Side,
    /// Turn timer, `0..=`[`GAUGE_MAX`]. Runtime state owned by [`BattleState`].
    pub gauge: u32,
    /// True once the gauge is full and the combatant has not yet acted.
    pub ready: bool,
    /// Defending: physical damage is halved. Runtime state owned by [`BattleState`].
    pub defending: bool,
    /// The limit gauge, `0..=`[`LIMIT_MAX`] (party members only: the starting value is an input, the
    /// battle changes it, see the module docs). Always 0 without a [`Combatant::limit_break`].
    pub limit: u16,
    /// The limit break this combatant can give once its limit gauge is full (`None`: it has none, and
    /// no gauge).
    pub limit_break: Option<LimitBreak>,
    /// How many more times in this battle the combatant may cast a spell of the [`Aim::OneOrSide`] kind on a
    /// whole side: the uses of its All materia (the starting value is an input like the level, then
    /// [`BattleState`] takes one off for each such cast, see the module docs). 0 for everyone who has no All
    /// materia and for every enemy. Battle state only: it is never sent or kept.
    pub all_left: u8,
    /// The statuses the combatant has, with their timers (see the module docs, "Statuses"). Runtime state owned by
    /// [`BattleState`]; what is passed in are the STARTING statuses, an input like the level and the limit gauge
    /// ([`BattleState::with_config`] keeps the modelled ones, announces them and starts their timers).
    pub status: StatusState,
    /// The statuses the combatant cannot be given: an enemy's own immunities (its record's status mask says which
    /// statuses affect it; see [`crate::status`]). Empty for a party member.
    pub status_immune: StatusSet,
    /// The enemy's own AI scripts and the memory they keep ([`Combatant::from_enemy_in_scene`] gives it
    /// the scene's); `None`: the enemy uses the plain behaviour.
    pub ai: Option<crate::ai::EnemyAi>,
    /// The id of the `scene.bin` enemy record this enemy was made from ([`Combatant::from_enemy_in_scene`]);
    /// what an AI script reads as "Index of actor" ([`crate::ai::world`]). `None`: not made from a scene.
    pub enemy_id: Option<u16>,
}

impl Combatant {
    /// A combatant with neutral defaults: no attack/defense/dex, 100% accuracy, Power 16,
    /// front row, short range, no spells, attacks or affinities. Set the public fields to taste.
    pub fn new(name: impl Into<String>, level: u8, max_hp: u32) -> Self {
        Self {
            name: name.into(),
            level,
            hp: max_hp,
            max_hp,
            mp: 0,
            max_mp: 0,
            attack: 0,
            defense: 0,
            magic: 0,
            magic_defense: 0,
            dex: 0,
            luck: 0,
            evade: 0,
            magic_evade: 0,
            accuracy: 100,
            attack_power: ATTACK_COMMAND_POWER,
            crit_bonus: 0,
            short_range: true,
            row: Row::Front,
            reward: Reward::default(),
            spells: Vec::new(),
            actions: Vec::new(),
            affinities: Vec::new(),
            side: Side::Party,
            gauge: 0,
            ready: false,
            defending: false,
            limit: 0,
            limit_break: None,
            all_left: 0,
            status: StatusState::none(),
            status_immune: StatusSet::EMPTY,
            ai: None,
            enemy_id: None,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Everything the combatant has: Death when it is out of HP, Near-death when it stands with a quarter of its
    /// HP or less, and the statuses it carries ([`StatusState::set`]). The one place to ask "what is wrong with
    /// this combatant", for a panel or an AI script.
    pub fn statuses(&self) -> StatusSet {
        if !self.is_alive() {
            return StatusSet::EMPTY.with(Status::Death);
        }
        let mut set = self.status.set();
        if u64::from(self.hp) * 4 <= u64::from(self.max_hp) {
            set.insert(Status::NearDeath);
        }
        set
    }

    /// Whether the combatant has `status` ([`Combatant::statuses`]).
    pub fn has_status(&self, status: Status) -> bool {
        self.statuses().contains(status)
    }

    /// Whether the combatant can take a turn: alive, and not asleep, stopped or paralysed.
    pub fn can_act(&self) -> bool {
        self.is_alive() && !self.status.blocks_turns()
    }

    /// The Attack command as an action: a physical attack of this combatant's Power and accuracy.
    pub fn attack_spec(&self) -> ActionSpec {
        ActionSpec::basic_attack(self.attack_power, self.accuracy, self.short_range)
    }

    /// How this combatant answers an action with element mask `mask` (see [`affinity_response`]).
    pub fn affinity_for(&self, mask: u16) -> Option<AffinityRate> {
        affinity_response(&self.affinities, mask)
    }

    /// Build an enemy from its scene record. `basic_attack` (see [`Scene::basic_attack`])
    /// supplies the Attack action's Power and hit rate; without it the enemy uses Power 16 at
    /// 100%. The enemy has no attack list ([`Combatant::actions`]) and so only ever makes that plain
    /// attack; [`Combatant::from_enemy_in_scene`] gives it its own attacks.
    pub fn from_enemy(record: &EnemyRecord, basic_attack: Option<&AttackRecord>) -> Self {
        let mut c = Combatant::new(record.name.clone(), record.level, record.hp);
        c.side = Side::Enemy;
        c.mp = u32::from(record.mp);
        c.max_mp = u32::from(record.mp);
        c.attack = u32::from(record.strength);
        c.defense = u32::from(record.defense);
        c.magic = u32::from(record.magic);
        c.magic_defense = u32::from(record.magic_defense);
        c.dex = u32::from(record.speed);
        c.luck = u32::from(record.luck);
        c.evade = u32::from(record.evade);
        if let Some(a) = basic_attack {
            c.accuracy = u32::from(a.accuracy);
            c.attack_power = u32::from(a.power);
        }
        c.short_range = true; // the Enemy Attack command is always row-sensitive
        c.reward = Reward {
            exp: record.exp,
            gil: record.gil,
            ap: u32::from(record.ap),
        };
        // The record's status mask says which statuses AFFECT the enemy (a set bit), so a clear bit is an immunity.
        c.status_immune = StatusSet::from_bits(!record.status_immunities);
        // Element codes `0..=15` are elements; the codes from `0x20` are statuses (not modelled).
        c.affinities = record
            .affinities()
            .into_iter()
            .filter(|a| a.code < 16)
            .map(|a| ElementAffinity {
                element: a.code,
                rate: a.rate,
            })
            .collect();
        c
    }

    /// Build the enemy in slot `enemy_slot` of `scene` ([`Combatant::from_enemy`]) and give it the
    /// scene's attacks it can use: its damaging attacks in its own order, each once.
    pub fn from_enemy_in_scene(scene: &Scene, enemy_slot: usize, record: &EnemyRecord) -> Self {
        let basic = scene.basic_attack(enemy_slot).map(|a| &a.record);
        let mut c = Self::from_enemy(record, basic);
        c.enemy_id = scene.enemy_ids.get(enemy_slot).copied().flatten();
        for attack in scene.enemy_attacks(enemy_slot) {
            if let Some(action) = EnemyAction::from_scene(attack) {
                if !c.actions.iter().any(|known| known.id == action.id) {
                    c.actions.push(action);
                }
            }
        }
        if let Some(scripts) = scene.ai.enemy(enemy_slot) {
            let known: Vec<crate::ai::AiAttack> = scene
                .attacks
                .iter()
                .map(|a| crate::ai::AiAttack {
                    id: a.id,
                    mp_cost: a.record.mp_cost,
                })
                .collect();
            c.ai = Some(crate::ai::EnemyAi::new(Arc::clone(scripts), Arc::from(known)));
        }
        c
    }

    /// Build a party member from kernel data: a deterministic stat block for `level`
    /// ([`crate::kernel::GrowthData::stat_block`]) plus equipment and spells.
    ///
    /// `weapon` / `armor` override the equipment ids; `None` uses the character's starting
    /// equipment from the kernel initialization data, and a character with no initialization
    /// record (Cait Sith, Vincent) is unarmed and unarmored. Returns `None` if the kernel
    /// has no growth curves for the character.
    ///
    /// The spells are [`party_spells`]: the magic materia of the starting record plus the starter
    /// grimoire.
    pub fn from_character(
        kernel: &Kernel,
        who: CharacterId,
        level: u8,
        weapon: Option<u8>,
        armor: Option<u8>,
    ) -> Option<Self> {
        let stats = kernel.growth.stat_block(who, level)?;
        let init = kernel.init_character_by_id(who.index() as u8);
        let weapon = weapon
            .or(init.map(|i| i.weapon))
            .and_then(|id| kernel.weapon(id));
        let armor = armor
            .or(init.map(|i| i.armor))
            .and_then(|id| kernel.armor(id));

        let mut c = Combatant::new(who.name(), stats.level, u32::from(stats.max_hp));
        c.mp = u32::from(stats.max_mp);
        c.max_mp = u32::from(stats.max_mp);
        c.attack = u32::from(stats.strength) + weapon.map_or(0, |w| u32::from(w.attack));
        c.defense = u32::from(stats.vitality) + armor.map_or(0, |a| u32::from(a.defense));
        c.magic = u32::from(stats.magic);
        c.magic_defense = u32::from(stats.spirit) + armor.map_or(0, |a| u32::from(a.magic_defense));
        c.dex = u32::from(stats.dexterity);
        c.luck = u32::from(stats.luck);
        c.evade = armor.map_or(0, |a| u32::from(a.evade_percent));
        c.magic_evade = armor.map_or(0, |a| u32::from(a.magic_evade_percent));
        if let Some(w) = weapon {
            c.accuracy = u32::from(w.accuracy);
            c.crit_bonus = u32::from(w.crit_rate);
            c.short_range = w.is_short_range();
        }
        c.row = if init.is_some_and(|i| i.back_row) {
            Row::Back
        } else {
            Row::Front
        };
        c.spells = party_spells(kernel, who);
        c.limit_break = LimitBreak::for_character(kernel, who);
        c.all_left = kernel.materia(STARTER_ALL_MATERIA).map_or(0, |m: MateriaRecord| m.all_uses(m.level_for_ap(0)));
        Some(c)
    }
}

/// The spells `who` can cast, ascending by index: the spells that the magic materia of its kernel
/// starting record teach at the AP they start with ([`Kernel::equipped_spells`]), plus the level-1 spells
/// of the [`STARTER_MATERIA`] (Restore, Fire, Ice, Lightning: Cure, Fire, Ice, Bolt on the Steam copy).
/// Only the records the rules can cast are kept ([`Spell::from_attack`]).
pub fn party_spells(kernel: &Kernel, who: CharacterId) -> Vec<Spell> {
    let mut indexes = kernel.equipped_spells(who);
    for id in STARTER_MATERIA {
        if let Some(materia) = kernel.materia(id) {
            indexes.extend(materia.unlocked_spells(0));
        }
    }
    indexes.sort_unstable();
    indexes.dedup();
    indexes
        .into_iter()
        .filter_map(|index| Spell::from_attack(index, &kernel.attack(usize::from(index))?))
        .collect()
}

/// The enemies of formation `formation_slot` (0..4) of `scene`, in placement order. Empty if
/// the slot is out of range or the formation has no resolvable enemies.
pub fn enemies_for_formation(scene: &Scene, formation_slot: usize) -> Vec<Combatant> {
    scene
        .formation_enemies(formation_slot)
        .into_iter()
        .map(|r| Combatant::from_enemy_in_scene(scene, r.enemy_slot, r.record))
        .collect()
}

/// How the gauges are initialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartMode {
    /// Every gauge starts at a random fill in `0..=`[`INITIAL_GAUGE_MAX`] (APPROXIMATION).
    #[default]
    Normal,
    /// Party gauges full, enemy gauges empty.
    Preemptive,
    /// Enemy gauges full, party gauges empty.
    BackAttack,
    /// All gauges start empty (deterministic pace; used by tests).
    Zero,
}

impl StartMode {
    /// Map a scene battle layout to a start mode (see the module docs).
    pub fn from_layout(layout: BattleLayout) -> Self {
        match layout {
            BattleLayout::Preemptive => StartMode::Preemptive,
            BattleLayout::BackAttack => StartMode::BackAttack,
            _ => StartMode::Normal,
        }
    }
}

/// Tunable parameters of a battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BattleConfig {
    /// FF7 "Battle Speed" configuration value: 0 is fastest, 255 slowest.
    pub battle_speed: u8,
    /// Ticks per second of simulated time (clamped to `1..=1000`).
    pub ticks_per_second: u32,
    pub start: StartMode,
}

impl Default for BattleConfig {
    fn default() -> Self {
        Self {
            battle_speed: DEFAULT_BATTLE_SPEED,
            ticks_per_second: DEFAULT_TICKS_PER_SECOND,
            start: StartMode::Normal,
        }
    }
}

/// A command a party member can issue when its gauge is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Physical weapon attack on one living enemy.
    Attack { target: ActorId },
    /// Cast a spell the actor knows. `spell` is the kernel's attack index (Cure 0, Fire 27, Ice 30,
    /// Bolt 33, ...); `target` is an ally for a restoring spell (a dead one for Life) and a living enemy
    /// for a damaging one, or a whole side of them (`AllAllies` for a restoring spell, `AllEnemies` for a
    /// damaging one) when the spell can hit one (see the module docs, "Whole-side targeting"). Costs the
    /// spell's MP, once, however many it hits.
    Magic { spell: u16, target: Target },
    /// Use an item of the engine's item table. `item` is the kernel item index (Potion 0, Hi-Potion 1,
    /// Ether 3, Megalixir 6, Phoenix Down 7, ...); `target` is an ally (a dead one for Phoenix Down) for a
    /// restoring item and a living enemy for a damage item, or the whole side for an item that hits one
    /// (Megalixir restores the whole party). Whether the account owns one is not the engine's business
    /// (see the module docs); one item is used however many it hits.
    Item { item: u16, target: Target },
    /// Guard: physical damage taken is halved until the actor's gauge is full again.
    Defend,
    /// The actor's limit break (protocol v7), on `target`: a living enemy for a damaging limit, a living
    /// ally for a healing one, or the whole party for the one that heals it all (Healing Wind)
    /// ([`BattleState::limit_for`] says which). Only when the actor's limit gauge is full; it empties the
    /// gauge.
    Limit { target: Target },
    /// Try to run (protocol v7): the actor spends its turn on [`RUN_CHECKS_PER_TURN`] Run Checks. When the
    /// party gets away the battle ends with [`Outcome::Escaped`] for everyone, and when not the turn is
    /// over. Refused when the formation cannot be escaped.
    Escape,
}

/// What an [`BattleEvent::Action`] announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionRef {
    /// A plain physical attack (a party member's Attack command, or an enemy without attacks of its own).
    Attack,
    /// A spell, by kernel attack index.
    Magic(u16),
    /// An item, by kernel item index.
    Item(u16),
    Defend,
    /// An enemy's own attack, by its `scene.bin` attack id (the name is in the scene's attack table).
    EnemyAttack(u16),
    /// A limit break (protocol v7), by its id in the kernel's character record (also its index in
    /// "Names - magic": Braver 128, ...).
    Limit(u16),
    /// The actor tries to run (protocol v7).
    Escape,
}

/// How the battle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Victory,
    Defeat,
    /// The party got away (protocol v7): nothing is paid.
    Escaped,
}

/// Something that happened in the battle, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattleEvent {
    /// `actor`'s gauge filled (party members now wait for a command; enemies act at once).
    GaugeReady { actor: ActorId },
    /// A hit. `amount` is the damage rolled; the target's HP drops by at most that much.
    Damage {
        actor: ActorId,
        target: ActorId,
        amount: u32,
        critical: bool,
    },
    /// The attack missed. (Addition to the minimal event set: hit chance is modelled.)
    Miss { actor: ActorId, target: ActorId },
    /// `actor` was reduced to 0 HP.
    Death { actor: ActorId },
    /// All enemies are dead.
    Victory,
    /// All party members are dead.
    Defeat,
    /// `actor` begins `action`: announced before the effects of every action, party and enemy.
    Action { actor: ActorId, action: ActionRef },
    /// `amount` HP were restored to `target` (a restoring spell or item, or an element it absorbed):
    /// what was actually restored, never more than the target was missing.
    Heal {
        actor: ActorId,
        target: ActorId,
        amount: u32,
    },
    /// `actor` paid `amount` MP for its action.
    MpSpent { actor: ActorId, amount: u32 },
    /// `amount` MP were restored to `target` (Ether), never more than it was missing.
    MpRestored {
        actor: ActorId,
        target: ActorId,
        amount: u32,
    },
    /// `target` was brought back from the dead with `hp` HP (Life, Phoenix Down); its gauge starts
    /// empty.
    Revive {
        actor: ActorId,
        target: ActorId,
        hp: u32,
    },
    /// `actor`'s limit gauge is now `value` (`0..=`[`LIMIT_MAX`]): after a hit that raised it, when a
    /// knockout emptied it and when a limit break used it up.
    LimitGauge { actor: ActorId, value: u16 },
    /// `actor` tried to run and the party did not get away (the turn is over). A successful escape has no
    /// event of its own: the [`BattleEvent::Action`] with [`ActionRef::Escape`] is followed by the end of
    /// the battle with [`Outcome::Escaped`].
    EscapeFailed { actor: ActorId },
    /// `target` now has `status` (an action inflicted it, or the battle started with it): see the module docs,
    /// "Statuses". Only a status the rules model is ever announced.
    StatusInflicted { target: ActorId, status: Status },
    /// `target` no longer has `status`: an action cured it, its timer ran out, or (Sleep) a physical blow woke it.
    StatusEnded { target: ActorId, status: Status },
    /// A turn of a status that hurts or heals: `Poison` took `amount` HP from `target` (what was rolled; its HP
    /// drops by at most that, and a knockout follows as [`BattleEvent::Death`]), `Regen` gave it `amount` HP
    /// (what it gained, never more than it was missing; a turn that would give nothing is not announced).
    StatusTick { target: ActorId, status: Status, amount: u32 },
}

/// Why a command was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("the battle is over")]
    BattleOver,
    #[error("unknown actor {0:?}")]
    UnknownActor(ActorId),
    #[error("actor {0:?} is not party-controlled")]
    NotControllable(ActorId),
    #[error("actor {0:?} is dead")]
    ActorDead(ActorId),
    #[error("actor {0:?} has no full gauge")]
    GaugeNotFull(ActorId),
    #[error("unknown target {0:?}")]
    UnknownTarget(ActorId),
    #[error("target {0:?} is not a valid target (damage goes to an enemy, restoring to an ally)")]
    InvalidTarget(ActorId),
    #[error("target {0:?} is already dead")]
    TargetDead(ActorId),
    #[error("target {0:?} is alive (only a dead ally can be revived)")]
    TargetNotDead(ActorId),
    #[error("the actor does not know spell {0}")]
    UnknownSpell(u16),
    #[error("actor {0:?} does not have the MP for that spell")]
    NotEnoughMp(ActorId),
    #[error("item {0} cannot be used in battle")]
    UnknownItem(u16),
    /// The action cannot be aimed that way: a whole side where it hits one target, one target where it
    /// hits a whole side, or a whole side that is not the one it aims at (a damaging action on the
    /// allies, a restoring one on the enemies). (Protocol v10.)
    #[error("{0:?} is not a way this action can be aimed (one target or a whole side, damage to the enemies, restoring to the allies)")]
    InvalidReach(Target),
    /// Nobody on that side can be hit (a revival of a whole side with nobody fallen). (Protocol v10.)
    #[error("there is nobody on that side {0:?} the action can hit")]
    NoTargets(Target),
    /// The spell can hit a whole side only with an All materia use, and the actor has none left this
    /// battle. (Protocol v10.)
    #[error("actor {0:?} has no All left this battle")]
    NoAllLeft(ActorId),
    /// The actor is asleep, stopped or paralysed: it takes no turn until that is over (see the module docs,
    /// "Statuses").
    #[error("actor {0:?} cannot act: it is asleep, stopped or paralysed")]
    CannotAct(ActorId),
    /// The actor is silenced: it cannot cast a spell.
    #[error("actor {0:?} is silenced")]
    Silenced(ActorId),
    #[error("actor {0:?} has no limit break")]
    NoLimit(ActorId),
    #[error("actor {0:?} has no full limit gauge")]
    LimitNotReady(ActorId),
    #[error("this battle cannot be escaped")]
    CannotEscape,
}

/// How an action's damage is dealt, for [`action_damage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rolls {
    /// The hit is a critical hit (only a physical action that can crit rolls for it).
    pub critical: bool,
    /// The random variance roll `0..=255` (used by the formulas that have variance).
    pub variance: u32,
}

/// Inputs of the physical damage formula (all roll results are passed in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageInput {
    pub attack: u32,
    pub level: u32,
    pub power: u32,
    pub defense: u32,
    pub critical: bool,
    /// Short-range attack with a back-row attacker or target: damage is halved.
    pub halve_for_row: bool,
    /// The target is defending: damage is halved (after the row penalty).
    pub defending: bool,
    /// The element's effect on the damage: doubled for a weakness, halved for a resistance.
    pub elemental: Elemental,
    /// Random variance roll `0..=255` (damage is multiplied by `(3841 + roll) / 4096`).
    pub variance_roll: u32,
}

/// The physical ("X1") damage formula, in the order of the disassembly:
/// `Base = Att + ((Att + Lvl) / 32) * ((Att * Lvl) / 32)`,
/// `damage = Power * (512 - Def) * Base / (16 * 512)`, critical x2, row penalty /2, Defend /2, the
/// element (x2 or /2), then variance `(3841 + roll) / 4096` with a minimum of 1, capped at
/// [`MAX_DAMAGE`].
pub fn physical_damage(input: &DamageInput) -> u32 {
    finish_damage(
        physical_base(input.attack, input.level, input.power, input.defense),
        input.critical,
        input.halve_for_row,
        input.defending,
        false,
        input.elemental,
        Some(input.variance_roll),
    )
}

/// `Power * (512 - Def) * Base / (16 * 512)` of the physical formula, before any modifier.
fn physical_base(attack: u32, level: u32, power: u32, defense: u32) -> u64 {
    // Real stats are tiny (attack <= ~510, level <= 255, power <= 255); the clamps only keep
    // absurd hand-made inputs from overflowing the 64-bit arithmetic.
    let att = u64::from(attack.min(0xFFFF));
    let lvl = u64::from(level.min(255));
    let power = u64::from(power.min(0xFFFF));
    let x = (lvl * att) / 32;
    let y = (lvl + att) / 32;
    let base = att + x * y;
    let defense = u64::from(defense).min(511);
    (base * (512 - defense) * power) >> 13
}

/// The common tail of every damage formula: critical x2, row penalty /2, Defend /2, the split of an
/// action that hits several targets x2/3, the element, the variance (when the formula has one), a minimum
/// of 1 and the cap.
fn finish_damage(
    mut dmg: u64,
    critical: bool,
    halve_for_row: bool,
    defending: bool,
    split: bool,
    elemental: Elemental,
    variance_roll: Option<u32>,
) -> u32 {
    if critical {
        dmg *= 2;
    }
    if halve_for_row {
        dmg /= 2;
    }
    if defending {
        dmg /= 2;
    }
    if split {
        dmg = dmg * 2 / 3;
    }
    match elemental {
        Elemental::Normal => {}
        Elemental::Double => dmg *= 2,
        Elemental::Half => dmg /= 2,
    }
    if let Some(roll) = variance_roll {
        dmg = (dmg * (3841 + u64::from(roll.min(255)))) >> 12;
    }
    dmg.max(1).min(u64::from(MAX_DAMAGE)) as u32
}

/// The damage `spec` deals to `target` when `actor` uses it, from the formulas of
/// [`crate::action`], for the given roll results and elemental multiplier. Physical actions use
/// the actor's Strength and the target's Defense (halved for a short-range attack across the back
/// row, and by Defend), magical ones Magic and magic defense. At least 1, at most [`MAX_DAMAGE`].
/// A restoring spec is not damage: see [`restore_amount`].
pub fn action_damage(
    spec: &ActionSpec,
    actor: &Combatant,
    target: &Combatant,
    elemental: Elemental,
    rolls: Rolls,
) -> u32 {
    action_damage_among(spec, actor, target, elemental, rolls, 1)
}

/// [`action_damage`] for an action that hits `targets` combatants at once: when that is more than one and
/// the action [splits](ActionSpec::splits), the damage to each of them is 2/3 of what it would be to one
/// (after the row penalty and Defend, before the element and the variance: Fergusson's order, see the
/// module docs).
pub fn action_damage_among(
    spec: &ActionSpec,
    actor: &Combatant,
    target: &Combatant,
    elemental: Elemental,
    rolls: Rolls,
    targets: usize,
) -> u32 {
    let split = targets > 1 && spec.splits();
    let (stat, defense) = match spec.kind {
        ActionKind::Physical => (actor.attack, target.defense),
        ActionKind::Magical => (actor.magic, target.magic_defense),
    };
    let defense = if spec.ignores_defense { 0 } else { defense };
    let physical = spec.kind == ActionKind::Physical;
    let defending = physical && target.defending;
    // Only the standard formula has the row penalty (the others never look at rows).
    let halve_for_row = physical
        && spec.formula == Formula::Standard
        && spec.short_range
        && (target.row == Row::Back || actor.row == Row::Back);
    let variance = spec.formula.has_variance().then_some(rolls.variance);
    let level = u64::from(actor.level);
    let stat64 = u64::from(stat.min(0xFFFF));
    let power = u64::from(spec.power.min(0xFFFF));
    let defense64 = u64::from(defense).min(511);
    let finish = |dmg: u64| finish_damage(dmg, false, false, defending, split, elemental, variance);
    match spec.formula {
        Formula::Standard => finish_damage(
            physical_base(stat, u32::from(actor.level), spec.power, defense),
            rolls.critical && spec.can_crit,
            halve_for_row,
            defending,
            split,
            elemental,
            Some(rolls.variance),
        ),
        Formula::Spell => finish((((level + stat64) * 6) * (512 - defense64) * power) >> 13),
        Formula::CurrentHp => finish(u64::from(target.hp) * power / 32),
        Formula::MaxHp => finish(u64::from(target.max_hp) * power / 32),
        Formula::Restore => finish(22 * power + 6 * (level + stat64)),
        Formula::Flat => finish(20 * power),
        Formula::Fixed => finish(((512 - defense64) * power) / 32),
        // Only a restoring action has this formula (see `ActionSpec`); as damage it is the target's HP.
        Formula::Full => finish(u64::from(target.hp)),
        // A status-only action deals nothing (the rules never ask for its damage).
        Formula::None => 0,
    }
}

/// How much a restoring action ([`ActionSpec::heals`]) gives `target` before it is limited to what the
/// target is missing: a fraction of its max HP (or MP, for an action that hits MP) for formula 4,
/// Cure's formula with the variance for 5, a flat amount for 6. Formula 8 (Elixir) is "everything" and
/// returns the target's whole max HP (or MP).
pub fn restore_amount(
    spec: &ActionSpec,
    actor: &Combatant,
    target: &Combatant,
    variance: u32,
) -> u32 {
    restore_amount_among(spec, actor, target, variance, 1)
}

/// [`restore_amount`] for an action that restores `targets` combatants at once: Cure's formula, when that
/// is more than one, is split like damage is (2/3 of the base, before the variance); the formulas of a
/// fraction of max HP, a flat amount and "everything" never are.
pub fn restore_amount_among(
    spec: &ActionSpec,
    actor: &Combatant,
    target: &Combatant,
    variance: u32,
    targets: usize,
) -> u32 {
    let split = targets > 1 && spec.splits();
    let pool = if spec.hits_mp {
        target.max_mp
    } else {
        target.max_hp
    };
    let power = u64::from(spec.power.min(0xFFFF));
    let amount: u64 = match spec.formula {
        Formula::MaxHp => u64::from(pool) * power / 32,
        Formula::Restore => {
            let base =
                22 * power + 6 * (u64::from(actor.level) + u64::from(actor.magic.min(0xFFFF)));
            let base = if split { base * 2 / 3 } else { base };
            (base * (3841 + u64::from(variance.min(255)))) >> 12
        }
        Formula::Flat => 20 * power,
        Formula::Full => u64::from(pool),
        // Not restoring formulas: a restoring spec never has them.
        Formula::Standard | Formula::Spell | Formula::CurrentHp | Formula::Fixed | Formula::None => 0,
    };
    amount.min(u64::from(u32::MAX)) as u32
}

/// Base hit chance (before luck) of a physical attack, as an unclamped percentage:
/// `(Dex/4 + accuracy) + attacker_evade - target_evade`, where a party member's evade is
/// `Dex/4 + armor evade%` and an enemy's is its evade stat. A result of exactly 0 becomes 1.
pub fn hit_chance(actor: &Combatant, target: &Combatant) -> i32 {
    hit_chance_with(actor, target, actor.accuracy)
}

/// [`hit_chance`] for an action whose own hit rate is `accuracy` (an enemy's attack has its own).
pub fn hit_chance_with(actor: &Combatant, target: &Combatant, accuracy: u32) -> i32 {
    let evade_of = |c: &Combatant| -> i64 {
        match c.side {
            Side::Enemy => i64::from(c.evade),
            Side::Party => i64::from(c.dex >> 2) + i64::from(c.evade),
        }
    };
    let physical = i64::from(actor.dex >> 2) + i64::from(accuracy);
    let chance = physical + evade_of(actor) - evade_of(target);
    let chance = chance.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    if chance == 0 {
        1
    } else {
        chance
    }
}

/// Hit chance of a magical action with hit rate `accuracy` against `target`, as an unclamped
/// percentage: the rate minus the target's magic evade (APPROXIMATION: the documents give none).
pub fn magic_hit_chance(accuracy: u32, target: &Combatant) -> i64 {
    i64::from(accuracy) - i64::from(target.magic_evade)
}

/// Decide a physical hit from the chance and the three rolls.
///
/// `luck_roll` is `0..=99`, `miss_roll` is `1..=100`. A "lucky hit" (attacker `Luck / 4 >
/// luck_roll`) always connects; a party attacker against an enemy can instead hit a "lucky
/// dodge" (target `Luck / 4 > luck_roll`) and always misses. Otherwise the attack hits when
/// `chance >= miss_roll`.
pub fn resolve_hit(
    actor: &Combatant,
    target: &Combatant,
    chance: i32,
    luck_roll: u32,
    miss_roll: u32,
) -> bool {
    let mut chance = i64::from(chance);
    if actor.luck / 4 > luck_roll {
        chance = 255;
    } else if actor.side == Side::Party && target.side == Side::Enemy && target.luck / 4 > luck_roll
    {
        chance = 0;
    }
    chance >= i64::from(miss_roll)
}

/// Critical chance in percent: `((Luck + Lvl) - target Lvl) >> 2`, plus the weapon crit
/// bonus for party attackers; never negative.
pub fn critical_chance(actor: &Combatant, target: &Combatant) -> u32 {
    let base = (i64::from(actor.luck) + i64::from(actor.level) - i64::from(target.level)) >> 2;
    let bonus = match actor.side {
        Side::Party => i64::from(actor.crit_bonus),
        Side::Enemy => 0,
    };
    (base + bonus).clamp(0, 255) as u32
}

/// Map a raw 16-bit roll to `1..=100` like FF7's `rand(0..=65535) * 99 / 65535 + 1`.
fn percent_roll(raw: u16) -> u32 {
    u32::from(raw) * 99 / 65_535 + 1
}

/// Gauge increase per tick for a combatant (see the module docs for the formula).
pub fn turn_timer_increase(side: Side, dex: u32, v_timer_increase: u32, normal_speed: u32) -> u32 {
    // Real values are tiny (Dex <= ~255, increase <= 546); the clamps only keep absurd
    // hand-made inputs from overflowing.
    let dex = u64::from(dex.min(0xFFFF));
    let v_timer_increase = u64::from(v_timer_increase.min(0xFFFF));
    let numerator = match side {
        Side::Party => dex + 50,
        Side::Enemy => dex,
    } * v_timer_increase;
    let inc = numerator / u64::from(normal_speed.max(1));
    inc.min(u64::from(GAUGE_MAX)) as u32
}

/// `v_timer_increase = 2 * (32768 / (120 + battle_speed * 15 / 8))`.
pub fn v_timer_increase(battle_speed: u8) -> u32 {
    2 * (32_768 / (120 + u32::from(battle_speed) * 15 / 8))
}

/// How hard a battle is to run from (see the module docs, "Escape"): part of the battle's definition,
/// built by the peers from their own `scene.bin` ([`BattleState::with_escape`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeRule {
    /// The Run Difficulty: how many successful Run Checks the party needs in all (at least 1; the scene's
    /// escape counter, 1 for most fights).
    pub difficulty: u8,
    /// Every Run Check succeeds (the party had a preemptive attack or surrounds the enemy) instead of one
    /// in [`RUN_CHECK_ODDS`].
    pub sure: bool,
}

/// A validated command, ready to resolve (indexes are positions in the actor list; a `targets` list is
/// ascending and never empty).
enum Plan {
    Attack {
        target: usize,
    },
    Magic {
        spell: Spell,
        targets: Vec<usize>,
        /// A whole side with a spell that needs an All use: one is spent.
        spends_all: bool,
    },
    Item {
        item: u16,
        data: ItemData,
        targets: Vec<usize>,
    },
    Defend,
    Limit {
        limit: LimitBreak,
        targets: Vec<usize>,
    },
    Escape(EscapeRule),
}

/// One attack an enemy's script queued, ready to be played ([`BattleState::play_choices`]).
#[derive(Debug, Clone, Copy)]
struct Planned {
    /// The attack id the scene gives it (what [`ActionRef::EnemyAttack`] announces).
    id: u16,
    /// What of it the rules can play and whom the script aimed it at (the whom is resolved when it comes
    /// up, see [`BattleState::enemy_targets`]); `None`: an attack of the scene the rules cannot play, which
    /// is announced and does nothing.
    play: Option<(EnemyAction, Target)>,
}

/// A running battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleState {
    config: BattleConfig,
    rng: Rng,
    actors: Vec<Combatant>,
    party_len: usize,
    tick: u64,
    /// Milliseconds-times-ticks-per-second not yet converted to whole ticks.
    clock_remainder: u64,
    v_timer_increase: u32,
    normal_speed: u32,
    outcome: Option<Outcome>,
    /// Events produced at construction, delivered with the next step or command.
    pending: Vec<BattleEvent>,
    /// The items a party member can use ([`Command::Item`]); empty unless [`BattleState::with_items`].
    items: Arc<ItemTable>,
    /// How hard the battle is to run from (`None`: it cannot be escaped), see [`BattleState::with_escape`].
    escape: Option<EscapeRule>,
    /// The Run Difficulty still to bring down before the party is away (starts at the rule's).
    escape_left: u8,
}

impl BattleState {
    /// Start a battle with the default [`BattleConfig`].
    ///
    /// Party members become actors `0..party.len()` and enemies follow. Each side is
    /// truncated to [`MAX_PER_SIDE`]. If a side starts with nobody alive, the outcome is set
    /// immediately (enemies are checked first, so an empty enemy list is a victory) and the
    /// matching event is delivered with the first step.
    pub fn new(seed: u64, party: Vec<Combatant>, enemies: Vec<Combatant>) -> Self {
        Self::with_config(seed, party, enemies, BattleConfig::default())
    }

    /// Start a battle with an explicit configuration.
    pub fn with_config(
        seed: u64,
        mut party: Vec<Combatant>,
        mut enemies: Vec<Combatant>,
        mut config: BattleConfig,
    ) -> Self {
        config.ticks_per_second = config.ticks_per_second.clamp(1, 1000);
        party.truncate(MAX_PER_SIDE);
        enemies.truncate(MAX_PER_SIDE);
        let party_len = party.len();

        let mut actors: Vec<Combatant> = Vec::with_capacity(party_len + enemies.len());
        for mut c in party {
            c.side = Side::Party;
            actors.push(c);
        }
        for mut c in enemies {
            c.side = Side::Enemy;
            actors.push(c);
        }

        // Normal speed: rounded average Dex of the party, plus 50.
        let dex_sum: u64 = actors
            .iter()
            .take(party_len)
            .map(|c| u64::from(c.dex))
            .sum();
        let n = party_len.max(1) as u64;
        let normal_speed = (((dex_sum + n / 2) / n) as u32).saturating_add(50);

        let mut rng = Rng::new(seed);
        for c in &mut actors {
            c.hp = c.hp.min(c.max_hp);
            c.mp = c.mp.min(c.max_mp);
            c.defending = false;
            // A limit gauge is a living party member's, and one that has a limit.
            c.limit = if c.side == Side::Party && c.limit_break.is_some() && c.is_alive() { c.limit.min(LIMIT_MAX) } else { 0 };
            c.gauge = match (config.start, c.side) {
                (StartMode::Normal, _) => rng.range_inclusive(0, INITIAL_GAUGE_MAX),
                (StartMode::Preemptive, Side::Party) | (StartMode::BackAttack, Side::Enemy) => {
                    GAUGE_MAX
                }
                _ => 0,
            };
            c.ready = false;
            // The statuses passed in are the starting ones: the modelled ones of a living combatant, timers full.
            c.status = if c.is_alive() { StatusState::starting(c.status.set()) } else { StatusState::none() };
        }

        let mut state = Self {
            config,
            rng,
            actors,
            party_len,
            tick: 0,
            clock_remainder: 0,
            v_timer_increase: v_timer_increase(config.battle_speed),
            normal_speed,
            outcome: None,
            pending: Vec::new(),
            items: Arc::new(ItemTable::default()),
            escape: None,
            escape_left: 0,
        }
        .with_escape(Some(EscapeRule { difficulty: 1, sure: config.start == StartMode::Preemptive }));
        state.start_enemy_scripts();
        for i in 0..state.actors.len() {
            let c = &mut state.actors[i];
            if c.is_alive() && c.gauge >= GAUGE_MAX {
                c.gauge = GAUGE_MAX;
                c.ready = true;
                state.pending.push(BattleEvent::GaugeReady {
                    actor: ActorId(i as u8),
                });
            }
        }
        // The statuses a combatant starts with are announced like any other, in actor and bit order.
        for i in 0..state.actors.len() {
            for status in state.actors[i].status.set().iter() {
                state.pending.push(BattleEvent::StatusInflicted { target: ActorId(i as u8), status });
            }
        }
        let mut initial = Vec::new();
        state.check_outcome(&mut initial);
        state.pending.extend(initial);
        state
    }

    /// The same battle with `items` as the items a party member can use. Part of the battle's
    /// definition like the combatants: server and clients must give it the same table (they build it
    /// from the same kernel, see [`ItemTable::from_kernel`]).
    pub fn with_items(mut self, items: Arc<ItemTable>) -> Self {
        self.items = items;
        self
    }

    /// The same battle with `rule` as how hard it is to run from (`None`: it cannot be escaped, the
    /// boss fights). Part of the battle's definition like the combatants: server and clients must give
    /// it the same rule (they build it from the same `scene.bin`). A difficulty of 0 counts as 1.
    pub fn with_escape(mut self, rule: Option<EscapeRule>) -> Self {
        self.escape = rule.map(|r| EscapeRule { difficulty: r.difficulty.max(1), ..r });
        self.escape_left = self.escape.map_or(0, |r| r.difficulty);
        self
    }

    // --- accessors ---

    pub fn config(&self) -> &BattleConfig {
        &self.config
    }

    /// All combatants; index with [`ActorId`]`.0`.
    pub fn actors(&self) -> &[Combatant] {
        &self.actors
    }

    pub fn actor(&self, id: ActorId) -> Option<&Combatant> {
        self.actors.get(usize::from(id.0))
    }

    pub fn party_ids(&self) -> Vec<ActorId> {
        (0..self.party_len).map(|i| ActorId(i as u8)).collect()
    }

    pub fn enemy_ids(&self) -> Vec<ActorId> {
        (self.party_len..self.actors.len())
            .map(|i| ActorId(i as u8))
            .collect()
    }

    /// Party members that are alive, waiting for a command and able to give one (a sleeper, a stopped or a
    /// paralysed member holds its turn: see the module docs, "Statuses").
    pub fn ready_actors(&self) -> Vec<ActorId> {
        (0..self.party_len)
            .filter(|&i| self.actors[i].can_act() && self.actors[i].ready)
            .map(|i| ActorId(i as u8))
            .collect()
    }

    /// The spells `actor` can cast, in the order of its spell list (ascending by index for a character
    /// built from the kernel), with their MP cost and which side they aim at. Empty for an enemy or an
    /// unknown actor. Whether the actor has the MP right now is [`BattleState::check_command`]'s
    /// business, so that a menu can show an unaffordable spell greyed out.
    pub fn spells_for(&self, actor: ActorId) -> Vec<SpellInfo> {
        match self.actor(actor) {
            Some(c) if usize::from(actor.0) < self.party_len => {
                c.spells.iter().map(Spell::info).collect()
            }
            _ => Vec::new(),
        }
    }

    /// What item `item` does, if the engine can use it in battle (`None`: not an item here).
    pub fn item_info(&self, item: u16) -> Option<ItemInfo> {
        self.items.info(item)
    }

    /// The items the engine can use in battle.
    pub fn items(&self) -> &ItemTable {
        &self.items
    }

    /// Whether `id` is defending (halving physical damage until its next turn).
    pub fn is_defending(&self, id: ActorId) -> bool {
        self.actor(id).is_some_and(|c| c.defending)
    }

    /// `id`'s limit gauge, `0..=`[`LIMIT_MAX`] (0 for an enemy, a character with no limit, a dead one or
    /// an unknown actor).
    pub fn limit_gauge(&self, id: ActorId) -> u16 {
        self.actor(id).map_or(0, |c| c.limit)
    }

    /// How many more times `id` may hit a whole side with a spell that needs an All materia use
    /// ([`Combatant::all_left`]); 0 for an enemy, an unknown actor and a party member with no All.
    pub fn all_left(&self, id: ActorId) -> u8 {
        if usize::from(id.0) < self.party_len {
            self.actor(id).map_or(0, |c| c.all_left)
        } else {
            0
        }
    }

    /// Whether `id` is alive, has a limit break and a full limit gauge (it may give [`Command::Limit`]
    /// when its ATB gauge is full too).
    pub fn limit_ready(&self, id: ActorId) -> bool {
        self.actor(id).is_some_and(|c| c.is_alive() && c.limit_break.is_some() && c.limit >= LIMIT_MAX)
    }

    /// What `id`'s limit break is, for a menu: its id and whom it aims at (`None`: `id` has none, or is not
    /// a party member).
    pub fn limit_for(&self, id: ActorId) -> Option<LimitInfo> {
        if usize::from(id.0) >= self.party_len {
            return None;
        }
        self.actor(id)?.limit_break.map(|l| l.info())
    }

    /// Whether the battle can be run from at all (see [`Command::Escape`]).
    pub fn can_escape(&self) -> bool {
        self.escape.is_some()
    }

    /// The Run Difficulty the party still has to bring down (`None` when the battle cannot be escaped).
    pub fn escape_left(&self) -> Option<u8> {
        self.escape.map(|_| self.escape_left)
    }

    /// Whole ticks simulated so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// Gauge increase per tick of `id` (for pacing displays and tests), at the pace its statuses give it: doubled by
    /// Haste, halved by Slow, 0 while Stopped or Paralysed.
    pub fn gauge_increase(&self, id: ActorId) -> Option<u32> {
        let c = self.actor(id)?;
        let increase = turn_timer_increase(c.side, c.dex, self.v_timer_increase, self.normal_speed);
        Some(c.status.pace().scale(increase))
    }

    /// Everything `id` has, Death and Near-death included ([`Combatant::statuses`]); empty for an unknown actor.
    pub fn statuses(&self, id: ActorId) -> StatusSet {
        self.actor(id).map_or(StatusSet::EMPTY, Combatant::statuses)
    }

    /// Whether `id` has `status` ([`BattleState::statuses`]).
    pub fn has_status(&self, id: ActorId, status: Status) -> bool {
        self.statuses(id).contains(status)
    }

    /// Whether `id` can take a turn: alive, and not asleep, stopped or paralysed (`false` for an unknown actor).
    pub fn can_act(&self, id: ActorId) -> bool {
        self.actor(id).is_some_and(Combatant::can_act)
    }

    /// Whether `id` is silenced: a party member cannot give the Magic command.
    pub fn is_silenced(&self, id: ActorId) -> bool {
        self.actor(id).is_some_and(|c| c.status.silenced())
    }

    /// The party's "normal speed" (rounded average Dex + 50) used to scale every gauge.
    pub fn normal_speed(&self) -> u32 {
        self.normal_speed
    }

    /// Sum of the rewards of all dead enemies (meaningful once the outcome is `Victory`).
    pub fn rewards(&self) -> Reward {
        let mut total = Reward::default();
        for c in self
            .actors
            .iter()
            .skip(self.party_len)
            .filter(|c| !c.is_alive())
        {
            total.exp = total.exp.saturating_add(c.reward.exp);
            total.gil = total.gil.saturating_add(c.reward.gil);
            total.ap = total.ap.saturating_add(c.reward.ap);
        }
        total
    }

    // --- stepping ---

    /// Advance simulated time by `dt_ms` milliseconds (at most [`MAX_STEP_MS`]; the rest is
    /// dropped) and return what happened. Time is converted to whole ticks with an exact
    /// integer accumulator, so many small steps equal one big step.
    pub fn step(&mut self, dt_ms: u32) -> Vec<BattleEvent> {
        let dt = u64::from(dt_ms.min(MAX_STEP_MS));
        let tps = u64::from(self.config.ticks_per_second);
        self.clock_remainder += dt * tps;
        let ticks = self.clock_remainder / 1000;
        self.clock_remainder %= 1000;
        self.step_ticks(ticks)
    }

    /// Advance by whole ticks (at most [`MAX_STEP_TICKS`]) and return what happened.
    /// Stops early once the battle has an outcome.
    pub fn step_ticks(&mut self, ticks: u64) -> Vec<BattleEvent> {
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        for _ in 0..ticks.min(MAX_STEP_TICKS) {
            if self.outcome.is_some() {
                break;
            }
            self.tick_once(&mut events);
        }
        events
    }

    /// Whether `actor` could give `command` right now, without giving it: the same checks
    /// [`BattleState::submit_command`] makes, in the same order, and no change to the battle. A menu uses
    /// it to grey out what the engine would refuse (a spell the actor lacks the MP for, a target on the
    /// wrong side). It knows nothing of an inventory.
    pub fn check_command(&self, actor: ActorId, command: &Command) -> Result<(), CommandError> {
        self.plan_command(actor, command).map(|_| ())
    }

    /// Submit a command for a party member whose gauge is full. On success the command is
    /// resolved immediately and the resulting events (preceded by any undelivered start
    /// events) are returned; the actor's gauge resets to empty.
    pub fn submit_command(
        &mut self,
        actor: ActorId,
        command: Command,
    ) -> Result<Vec<BattleEvent>, CommandError> {
        let (idx, plan) = self.plan_command(actor, &command)?;
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        match plan {
            Plan::Attack { target } => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Attack,
                });
                let spec = self.actors.get(idx).map(Combatant::attack_spec);
                if let Some(spec) = spec {
                    self.resolve_action(idx, &[target], &spec, &mut events);
                    self.run_counters(idx, &[target], &spec, &mut events);
                }
            }
            Plan::Magic { spell, targets, spends_all } => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Magic(spell.index),
                });
                self.spend_mp(idx, spell.spec.mp_cost, &mut events);
                if spends_all {
                    if let Some(c) = self.actors.get_mut(idx) {
                        c.all_left = c.all_left.saturating_sub(1);
                    }
                }
                self.resolve_action(idx, &targets, &spell.spec, &mut events);
                self.run_counters(idx, &targets, &spell.spec, &mut events);
            }
            Plan::Item { item, data, targets } => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Item(item),
                });
                self.resolve_action(idx, &targets, &data.spec, &mut events);
                self.run_counters(idx, &targets, &data.spec, &mut events);
            }
            Plan::Defend => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Defend,
                });
                if let Some(c) = self.actors.get_mut(idx) {
                    c.defending = true;
                }
            }
            Plan::Limit { limit, targets } => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Limit(limit.attack),
                });
                // The gauge empties as the limit begins: before its effects, so that a battle the limit
                // ends still ends on its Victory event.
                if let Some(c) = self.actors.get_mut(idx) {
                    c.limit = 0;
                }
                events.push(BattleEvent::LimitGauge { actor, value: 0 });
                self.resolve_action(idx, &targets, &limit.spec, &mut events);
                self.run_counters(idx, &targets, &limit.spec, &mut events);
            }
            Plan::Escape(rule) => {
                events.push(BattleEvent::Action {
                    actor,
                    action: ActionRef::Escape,
                });
                self.run_checks(rule);
                if self.escape_left == 0 {
                    self.outcome = Some(Outcome::Escaped);
                } else {
                    events.push(BattleEvent::EscapeFailed { actor });
                }
            }
        }
        self.finish_turn(idx);
        Ok(events)
    }

    /// The Run Checks of one escape attempt (see the module docs): up to [`RUN_CHECKS_PER_TURN`], each
    /// lowering the difficulty that is left by one when it succeeds, and none once it is 0.
    fn run_checks(&mut self, rule: EscapeRule) {
        for _ in 0..RUN_CHECKS_PER_TURN {
            if self.escape_left == 0 {
                break;
            }
            if rule.sure || self.rng.below(RUN_CHECK_ODDS) == 0 {
                self.escape_left -= 1;
            }
        }
    }

    // --- internals ---

    fn flush_pending(&mut self, out: &mut Vec<BattleEvent>) {
        out.append(&mut self.pending);
    }

    /// Validate `command` (see [`BattleState::check_command`]) and say what it means.
    fn plan_command(
        &self,
        actor: ActorId,
        command: &Command,
    ) -> Result<(usize, Plan), CommandError> {
        if self.outcome.is_some() {
            return Err(CommandError::BattleOver);
        }
        let idx = usize::from(actor.0);
        let me = self
            .actors
            .get(idx)
            .ok_or(CommandError::UnknownActor(actor))?;
        if idx >= self.party_len {
            return Err(CommandError::NotControllable(actor));
        }
        if !me.is_alive() {
            return Err(CommandError::ActorDead(actor));
        }
        if !me.ready {
            return Err(CommandError::GaugeNotFull(actor));
        }
        if me.status.blocks_turns() {
            return Err(CommandError::CannotAct(actor));
        }
        let plan = match *command {
            Command::Attack { target } => Plan::Attack {
                target: self.check_target(target, false, false)?,
            },
            Command::Magic { spell, target } => {
                if me.status.silenced() {
                    return Err(CommandError::Silenced(actor));
                }
                let known = me
                    .spells
                    .iter()
                    .find(|s| s.index == spell)
                    .copied()
                    .ok_or(CommandError::UnknownSpell(spell))?;
                if me.mp < u32::from(known.spec.mp_cost) {
                    return Err(CommandError::NotEnoughMp(actor));
                }
                let targets = self.aim_at(idx, &known.spec, target)?;
                // A spell that can be toggled to a whole side needs an All use for it (a forced whole side,
                // Ultima, needs none).
                let spends_all = known.spec.aim == Aim::OneOrSide && target.is_side();
                if spends_all && me.all_left == 0 {
                    return Err(CommandError::NoAllLeft(actor));
                }
                Plan::Magic {
                    spell: known,
                    targets,
                    spends_all,
                }
            }
            Command::Item { item, target } => {
                let data = self
                    .items
                    .get(item)
                    .copied()
                    .ok_or(CommandError::UnknownItem(item))?;
                let targets = self.aim_at(idx, &data.spec, target)?;
                Plan::Item { item, data, targets }
            }
            Command::Defend => Plan::Defend,
            Command::Limit { target } => {
                let limit = me.limit_break.ok_or(CommandError::NoLimit(actor))?;
                if me.limit < LIMIT_MAX {
                    return Err(CommandError::LimitNotReady(actor));
                }
                let targets = self.aim_at(idx, &limit.spec, target)?;
                Plan::Limit { limit, targets }
            }
            Command::Escape => Plan::Escape(self.escape.ok_or(CommandError::CannotEscape)?),
        };
        Ok((idx, plan))
    }

    /// The positions `target` stands for when the actor at `actor` (a party member for a command, an enemy for
    /// an action of its AI: the sides of a [`Target`] are the actor's own) aims `spec` at it, checked against
    /// the action's [`Aim`], the side it aims at (a restoring action at the actor's allies, a damaging one at its
    /// enemies) and who can be hit (the living, or the fallen for a revival). One target is
    /// [`BattleState::check_target`]'s answer; a whole side is every combatant of it that can be hit, in
    /// ascending order, and [`CommandError::NoTargets`] when that is nobody.
    pub(crate) fn aim_at(&self, actor: usize, spec: &ActionSpec, target: Target) -> Result<Vec<usize>, CommandError> {
        let (ally, dead) = (spec.aims_at_allies(), spec.hits_dead);
        // Whether the action aims at the party's side of the battle: its own side for a restoring action when the
        // actor is a party member (and the other one for an enemy), the opposite for a damaging one.
        let in_party = ally == (actor < self.party_len);
        match target {
            Target::One(id) => {
                if !spec.aim.allows_one() {
                    return Err(CommandError::InvalidReach(target));
                }
                self.check_target(id, in_party, dead).map(|t| vec![t])
            }
            Target::AllAllies | Target::AllEnemies => {
                if !spec.aim.allows_side() || (target == Target::AllAllies) != ally {
                    return Err(CommandError::InvalidReach(target));
                }
                let side = if in_party { 0..self.party_len } else { self.party_len..self.actors.len() };
                let hit: Vec<usize> =
                    side.filter(|&i| self.actors.get(i).is_some_and(|c| c.is_alive() != dead)).collect();
                if hit.is_empty() {
                    Err(CommandError::NoTargets(target))
                } else {
                    Ok(hit)
                }
            }
        }
    }

    /// The position of `target` if it is a valid target: on the party's side when `in_party`, else on the
    /// enemies'; dead when `dead` (a revival), else alive.
    fn check_target(&self, target: ActorId, in_party: bool, dead: bool) -> Result<usize, CommandError> {
        let t_idx = usize::from(target.0);
        let victim = self
            .actors
            .get(t_idx)
            .ok_or(CommandError::UnknownTarget(target))?;
        if (t_idx < self.party_len) != in_party {
            return Err(CommandError::InvalidTarget(target));
        }
        if dead {
            if victim.is_alive() {
                return Err(CommandError::TargetNotDead(target));
            }
        } else if !victim.is_alive() {
            return Err(CommandError::TargetDead(target));
        }
        Ok(t_idx)
    }

    /// One tick: fill gauges, then let ready enemies act (in id order).
    fn tick_once(&mut self, events: &mut Vec<BattleEvent>) {
        self.tick += 1;
        self.run_status_clocks(events);
        for i in 0..self.actors.len() {
            let c = &self.actors[i];
            if !c.is_alive() || c.ready {
                continue;
            }
            let inc = turn_timer_increase(c.side, c.dex, self.v_timer_increase, self.normal_speed);
            // Haste doubles the gauge, Slow halves it, Stop and Paralysis freeze it.
            let inc = c.status.pace().scale(inc);
            let c = &mut self.actors[i];
            c.gauge = (c.gauge + inc).min(GAUGE_MAX);
            if c.gauge >= GAUGE_MAX {
                c.ready = true;
                // The next turn has come: a guard from the last one is over.
                c.defending = false;
                events.push(BattleEvent::GaugeReady {
                    actor: ActorId(i as u8),
                });
            }
        }
        for i in self.party_len..self.actors.len() {
            if self.outcome.is_some() {
                break;
            }
            // An enemy that is asleep, stopped or paralysed waits with its full gauge.
            if self.actors[i].can_act() && self.actors[i].ready {
                self.enemy_act(i, events);
            }
        }
    }

    /// One tick of every living combatant's statuses (see the module docs, "Statuses"): the turns of Poison and
    /// Regen, then the statuses whose timers ran out, in actor order. Poison can knock a combatant out, and the
    /// battle then has its outcome like after any blow; no clock runs once it has one.
    fn run_status_clocks(&mut self, events: &mut Vec<BattleEvent>) {
        for i in 0..self.actors.len() {
            if self.outcome.is_some() {
                break;
            }
            if !self.actors[i].is_alive() || self.actors[i].status.is_empty() {
                continue;
            }
            let elapsed = self.actors[i].status.advance(self.v_timer_increase);
            let target = ActorId(i as u8);
            if elapsed.poison {
                let amount = (self.actors[i].max_hp / POISON_DIVISOR).max(1);
                let c = &mut self.actors[i];
                c.hp = c.hp.saturating_sub(amount);
                events.push(BattleEvent::StatusTick { target, status: Status::Poison, amount });
                if c.hp == 0 {
                    c.gauge = 0;
                    c.ready = false;
                    c.defending = false;
                    c.status.clear();
                    let emptied = c.limit != 0;
                    c.limit = 0;
                    events.push(BattleEvent::Death { actor: target });
                    if emptied {
                        events.push(BattleEvent::LimitGauge { actor: target, value: 0 });
                    }
                    self.check_outcome(events);
                    continue;
                }
            }
            if elapsed.regen {
                let c = &mut self.actors[i];
                let gained = (c.max_hp / REGEN_DIVISOR).max(1).min(c.max_hp.saturating_sub(c.hp));
                if gained > 0 {
                    c.hp += gained;
                    events.push(BattleEvent::StatusTick { target, status: Status::Regen, amount: gained });
                }
            }
            for status in elapsed.ended.iter() {
                events.push(BattleEvent::StatusEnded { target, status });
            }
        }
    }

    /// Enemy AI: attack a uniformly random living party member with one of the attacks it can pay for
    /// (a uniform pick among them), or with a plain attack when it has none (see the module docs for the
    /// draw order). An enemy whose AI script can be followed does what the script says instead
    /// ([`BattleState::enemy_act_by_script`]).
    fn enemy_act(&mut self, idx: usize, events: &mut Vec<BattleEvent>) {
        if self.enemy_act_by_script(idx, events) {
            self.finish_turn(idx);
            return;
        }
        let actor = ActorId(idx as u8);
        let living: Vec<usize> = (0..self.party_len)
            .filter(|&p| self.actors[p].is_alive())
            .collect();
        if !living.is_empty() {
            let pick = self.rng.below(living.len() as u32) as usize;
            let target = living.get(pick).copied().unwrap_or(0);
            let usable: Vec<EnemyAction> = self.actors[idx]
                .actions
                .iter()
                .filter(|a| u32::from(a.spec.mp_cost) <= self.actors[idx].mp)
                .copied()
                .collect();
            let chosen = match usable.len() {
                0 => None,
                1 => usable.first().copied(),
                n => usable.get(self.rng.below(n as u32) as usize).copied(),
            };
            match chosen {
                Some(action) => {
                    events.push(BattleEvent::Action {
                        actor,
                        action: ActionRef::EnemyAttack(action.id),
                    });
                    self.spend_mp(idx, action.spec.mp_cost, events);
                    self.resolve_action(idx, &[target], &action.spec, events);
                }
                None => {
                    events.push(BattleEvent::Action {
                        actor,
                        action: ActionRef::Attack,
                    });
                    let spec = self.actors[idx].attack_spec();
                    self.resolve_action(idx, &[target], &spec, events);
                }
            }
        }
        self.finish_turn(idx);
    }

    /// Run the Initialize scripts of the enemies that have AI ([`crate::ai::initialize`]); an enemy whose
    /// script cannot be followed keeps the plain behaviour for good.
    fn start_enemy_scripts(&mut self) {
        for i in self.party_len..self.actors.len() {
            if self.actors[i].ai.is_none() {
                continue;
            }
            let mut rng = self.rng.clone();
            if let Some(started) = crate::ai::initialize(self, i, &mut rng) {
                self.rng = rng;
                self.apply_ai(i, started.memory, &started.changes);
            } else if let Some(ai) = self.actors[i].ai.as_mut() {
                ai.disable();
            }
        }
    }

    /// Keep the memory a script left in enemy `idx` and make the changes it made to the battle.
    fn apply_ai(&mut self, idx: usize, memory: crate::ai::Memory, changes: &[crate::ai::Change]) {
        if let Some(ai) = self.actors.get_mut(idx).and_then(|c| c.ai.as_mut()) {
            ai.set_memory(memory);
        }
        for change in changes {
            match *change {
                crate::ai::Change::Defense { actor, value } => {
                    if let Some(c) = self.actors.get_mut(usize::from(actor.0)) {
                        c.defense = value;
                    }
                }
                crate::ai::Change::MagicDefense { actor, value } => {
                    if let Some(c) = self.actors.get_mut(usize::from(actor.0)) {
                        c.magic_defense = value;
                    }
                }
                crate::ai::Change::Evade { actor, value } => {
                    if let Some(c) = self.actors.get_mut(usize::from(actor.0)) {
                        c.evade = value;
                    }
                }
                crate::ai::Change::Attack { actor, value } => {
                    if let Some(c) = self.actors.get_mut(usize::from(actor.0)) {
                        c.attack = value;
                    }
                }
                crate::ai::Change::MagicAttack { actor, value } => {
                    if let Some(c) = self.actors.get_mut(usize::from(actor.0)) {
                        c.magic = value;
                    }
                }
            }
        }
    }

    /// The enemy at `idx` acts as its AI script says ([`crate::ai::choose`]): the attacks it queued, one
    /// after the other in that order (see [`BattleState::play_choices`]), or nothing (the script lets the
    /// turn pass). `false` when it has no script or the turn cannot be followed (nothing was drawn or
    /// changed: the caller uses the plain behaviour).
    fn enemy_act_by_script(&mut self, idx: usize, events: &mut Vec<BattleEvent>) -> bool {
        let scripted = self.actors[idx].ai.as_ref().is_some_and(|ai| ai.memory().phase == crate::ai::Phase::Ready);
        let mut rng = self.rng.clone();
        let planned = crate::ai::choose(self, idx, &mut rng)
            .and_then(|turn| Some((self.plan_choices(idx, &turn.actions)?, turn)));
        let Some((planned, turn)) = planned else {
            if let (true, Some(ai)) = (scripted, self.actors[idx].ai.as_mut()) {
                ai.note_fallback();
            }
            return false;
        };
        self.rng = rng;
        self.apply_ai(idx, turn.memory, &turn.changes);
        if let Some(ai) = self.actors[idx].ai.as_mut() {
            ai.note_turn();
        }
        self.play_choices(idx, &planned, events);
        true
    }

    /// What the rules can carry out of the attacks an enemy's script queued, checked before anything is
    /// drawn or changed: `None` when one of them cannot be carried out (the whole answer is then not followed).
    ///
    /// * An attack the rules play ([`Combatant::actions`]) must be one the enemy can pay for, in all, and be
    ///   aimed at a party member or the whole party ([`BattleState::can_aim`]; a target that has fallen or an
    ///   empty side is no reason to refuse, the attack then fizzles when it comes up).
    /// * An attack of the scene the rules cannot play (no damage, healing, a status) is announced and has no
    ///   effect, whomever it is aimed at (a Death counter's own animation has its dead owner as target).
    fn plan_choices(&self, idx: usize, choices: &[crate::ai::EnemyChoice]) -> Option<Vec<Planned>> {
        let me = self.actors.get(idx)?;
        let mut mp_left = me.mp;
        let mut planned = Vec::with_capacity(choices.len());
        for choice in choices {
            let play = match me.actions.iter().find(|a| a.id == choice.attack).copied() {
                None => None,
                Some(action) => {
                    let cost = u32::from(action.spec.mp_cost);
                    if cost > mp_left || !self.can_aim(choice.target) {
                        return None;
                    }
                    mp_left -= cost;
                    Some((action, choice.target))
                }
            };
            planned.push(Planned { id: choice.attack, play });
        }
        Some(planned)
    }

    /// Whether the rules can aim an attack of an enemy as its script says: at a party member or at the whole
    /// party. (A damaging attack on the enemy's own side, [`Target::AllAllies`], is not played.) The attack's
    /// [`Aim`] is NOT consulted (an engine choice, not checked against the original): it is read as the rule
    /// of the player's target menu ([`BattleState::aim_at`] checks a command against it), while an enemy's
    /// script names its target mask itself, so a script that aims a one-target attack at the whole party, or a
    /// whole-side one at a single member, is followed (measured on the owner's data: refusing what a command
    /// could not do makes five more enemies fall back to the plain behaviour). A fallen target or an empty
    /// side does not count against it either: that attack fizzles when it comes up.
    fn can_aim(&self, target: Target) -> bool {
        match target {
            Target::One(who) => usize::from(who.0) < self.party_len,
            Target::AllEnemies => true,
            Target::AllAllies => false,
        }
    }

    /// The party members an attack of an enemy plays on right now, in ascending order: the one member the
    /// script named if it is alive, or every living member for the whole party; none if it fizzles. The
    /// whole-side rules take it from there ([`BattleState::resolve_action`]: the 2/3 split when the action
    /// splits and several are hit, each target with its own rolls). An attack whose record aims at a random
    /// target ([`Aim::Random`], which `aim_at` refuses for one target and for a side alike) hits ONE member:
    /// the single one the script named, or one of the living members drawn with `below(n)` (no draw for one
    /// candidate) when the script named the whole party.
    fn enemy_targets(&mut self, spec: &ActionSpec, target: Target) -> Vec<usize> {
        let living: Vec<usize> = match target {
            Target::One(who) => {
                let who = usize::from(who.0);
                if who < self.party_len && self.actors[who].is_alive() { vec![who] } else { Vec::new() }
            }
            Target::AllEnemies => (0..self.party_len).filter(|&i| self.actors[i].is_alive()).collect(),
            Target::AllAllies => Vec::new(),
        };
        match (spec.aim, living.len()) {
            (Aim::Random, n) if n > 1 => vec![living[self.rng.below(n as u32) as usize]],
            _ => living,
        }
    }

    /// Carry out the attacks of `planned` for the enemy at `idx`, one after the other in the order its script
    /// queued them (the original's action queue is first in, first out, see [`crate::ai::vm`]). Each is announced
    /// ([`BattleEvent::Action`]), paid for and played on the party members it reaches *at that moment*
    /// ([`BattleState::enemy_targets`], then [`BattleState::resolve_action`]: every living member of a whole
    /// party in ascending order, the 2/3 split of "Whole-side targeting" included): a one-target attack whose
    /// target has fallen since the script chose it (the script may keep a mask for turns, as Guard Scorpion
    /// keeps its scope target) **fizzles**: nothing is announced and nothing is spent, while the script's own
    /// memory (already stored) goes on. Nothing is played once the battle has an outcome.
    fn play_choices(&mut self, idx: usize, planned: &[Planned], events: &mut Vec<BattleEvent>) {
        for p in planned {
            if self.outcome.is_some() {
                break;
            }
            let Some((action, target)) = p.play else {
                events.push(BattleEvent::Action {
                    actor: ActorId(idx as u8),
                    action: ActionRef::EnemyAttack(p.id),
                });
                continue;
            };
            let targets = self.enemy_targets(&action.spec, target);
            if targets.is_empty() {
                continue;
            }
            events.push(BattleEvent::Action {
                actor: ActorId(idx as u8),
                action: ActionRef::EnemyAttack(p.id),
            });
            self.spend_mp(idx, action.spec.mp_cost, events);
            self.resolve_action(idx, &targets, &action.spec, events);
        }
    }

    /// The party member `attacker` has played `spec` on `targets` (ascending positions): every enemy among
    /// them that has scripts takes note of who acted on it, then runs its counter scripts in the order of
    /// the original's ([`crate::ai`], "Counters"): the Death counter if the blow killed it (and no other),
    /// else the General counter and then the Physical or the Magical one by the [kind](ActionKind) of the
    /// action. It is called once the WHOLE action has been played, so that an action on several targets has hit
    /// every one of them before any of them answers; a healing action starts no counter, and none is run
    /// once the battle has an outcome. A counter plays like an enemy's turn but is not one (the enemy's gauge
    /// is untouched): its attacks come as events right after the action, in the same batch.
    fn run_counters(&mut self, attacker: usize, targets: &[usize], spec: &ActionSpec, events: &mut Vec<BattleEvent>) {
        use crate::ai::script::slot;
        if attacker >= self.party_len || spec.heals() {
            return;
        }
        let Some(hit_by) = crate::ai::world::slot_mask(self, attacker) else {
            return;
        };
        let physical = spec.kind == ActionKind::Physical;
        for &target in targets {
            if target < self.party_len {
                continue;
            }
            let Some(ai) = self.actors.get_mut(target).and_then(|c| c.ai.as_mut()) else {
                continue;
            };
            if ai.memory().phase != crate::ai::Phase::Ready {
                continue;
            }
            // Whoever it is that hit the enemy, it remembers it, whether or not it has a counter to answer with.
            ai.note_hit(hit_by, physical);
            let scripts: &[usize] = if !self.actors[target].is_alive() {
                &[slot::DEATH_COUNTER]
            } else if !self.actors[target].can_act() {
                // Asleep, stopped or paralysed (a physical blow has already woken a sleeper): it notes the hit and
                // does not answer it (see "Statuses"; an engine choice, not checked against the original).
                &[]
            } else if physical {
                &[slot::GENERAL_COUNTER, slot::PHYSICAL_COUNTER]
            } else {
                &[slot::GENERAL_COUNTER, slot::MAGICAL_COUNTER]
            };
            for &script in scripts {
                if self.outcome.is_some() {
                    break;
                }
                self.run_counter(target, script, events);
            }
        }
    }

    /// Run the counter script `script` of the enemy at `idx` and play what it queued, as a Main turn is played:
    /// on a copy of the random stream, kept only if the counter can be followed and carried out (otherwise
    /// nothing at all is drawn or changed: no counter).
    fn run_counter(&mut self, idx: usize, script: usize, events: &mut Vec<BattleEvent>) {
        let has_script = self.actors[idx].ai.as_ref().is_some_and(|ai| ai.scripts().entry(script).is_some());
        if !has_script {
            return;
        }
        let mut rng = self.rng.clone();
        let planned = crate::ai::react(self, idx, script, &mut rng)
            .and_then(|turn| Some((self.plan_choices(idx, &turn.actions)?, turn)));
        let Some((planned, turn)) = planned else {
            if let Some(ai) = self.actors[idx].ai.as_mut() {
                ai.note_counter_skipped();
            }
            return;
        };
        self.rng = rng;
        self.apply_ai(idx, turn.memory, &turn.changes);
        if let Some(ai) = self.actors[idx].ai.as_mut() {
            ai.note_counter();
        }
        self.play_choices(idx, &planned, events);
    }

    fn finish_turn(&mut self, idx: usize) {
        if let Some(c) = self.actors.get_mut(idx) {
            c.gauge = 0;
            c.ready = false;
        }
    }

    /// Pay `cost` MP (announced; nothing for a cost of 0).
    fn spend_mp(&mut self, idx: usize, cost: u16, events: &mut Vec<BattleEvent>) {
        if cost == 0 {
            return;
        }
        if let Some(c) = self.actors.get_mut(idx) {
            c.mp = c.mp.saturating_sub(u32::from(cost));
            events.push(BattleEvent::MpSpent {
                actor: ActorId(idx as u8),
                amount: u32::from(cost),
            });
        }
    }

    /// Play one action on its targets, one after the other in the order given: restoring (allies) or
    /// offensive (enemies). Each target is resolved as a one-target action would be, and an action that
    /// [splits](ActionSpec::splits) deals 2/3 of that to each of several. Nothing is resolved after the
    /// battle has an outcome.
    pub(crate) fn resolve_action(
        &mut self,
        actor: usize,
        targets: &[usize],
        spec: &ActionSpec,
        events: &mut Vec<BattleEvent>,
    ) {
        for &target in targets {
            if self.outcome.is_some() {
                break;
            }
            if spec.formula == Formula::None {
                self.resolve_status_only(actor, target, spec, targets.len(), events);
            } else if spec.heals() {
                self.resolve_restore(actor, target, spec, targets.len(), events);
                // A recovery with a status of its own (Angel Whisper heals and cures) tries it on the one it restored.
                self.apply_status_effect(target, spec, targets.len(), events);
            } else {
                self.resolve_offense(actor, target, spec, targets.len(), events);
            }
        }
    }

    /// Resolve one offensive action with a fixed RNG draw order (see the module docs).
    fn resolve_offense(
        &mut self,
        attacker: usize,
        target: usize,
        spec: &ActionSpec,
        among: usize,
        events: &mut Vec<BattleEvent>,
    ) {
        let actor_id = ActorId(attacker as u8);
        let target_id = ActorId(target as u8);
        // The rolls to hit come first, in a fixed order, whether or not the target's affinity makes
        // them moot.
        let (luck_roll, miss_roll) = match (spec.rolls_accuracy, spec.kind) {
            (false, _) => (0, 0),
            (true, ActionKind::Physical) => {
                let luck = self.rng.range_inclusive(0, 99);
                (luck, percent_roll(self.rng.next_u16()))
            }
            (true, ActionKind::Magical) => (0, percent_roll(self.rng.next_u16())),
        };

        let (hit, critical_chance_pct, response) = {
            let (Some(a), Some(t)) = (self.actors.get(attacker), self.actors.get(target)) else {
                return;
            };
            let response = t.affinity_for(spec.element_mask);
            // What is nullified, absorbed or fully healed by an element is never missed.
            let sure = matches!(
                response,
                Some(AffinityRate::Nullify | AffinityRate::Absorb | AffinityRate::FullHeal)
            );
            let party_member = a.side == Side::Party;
            let hit = !spec.rolls_accuracy
                || sure
                || match spec.kind {
                    ActionKind::Physical => {
                        // Fury takes 30% off the hit rate, Darkness halves the Attack command's (party only).
                        let chance = status::hit_chance_after_statuses(
                            i64::from(hit_chance_with(a, t, spec.accuracy)),
                            &a.status,
                            spec.weapon_attack,
                            party_member,
                        );
                        let chance = chance.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                        resolve_hit(a, t, chance, luck_roll, miss_roll)
                    }
                    ActionKind::Magical => {
                        let chance = status::hit_chance_after_statuses(magic_hit_chance(spec.accuracy, t), &a.status, false, party_member);
                        chance >= i64::from(miss_roll)
                    }
                };
            (hit, critical_chance(a, t), response)
        };

        if !hit {
            events.push(BattleEvent::Miss {
                actor: actor_id,
                target: target_id,
            });
            return;
        }

        let critical = spec.can_crit && critical_chance_pct >= percent_roll(self.rng.next_u16());
        let variance = if spec.formula.has_variance() {
            self.rng.range_inclusive(0, 255)
        } else {
            0
        };

        let Some(victim) = self.actors.get(target) else {
            return;
        };
        let damage_of = |elemental: Elemental, critical: bool| {
            self.actors.get(attacker).map_or(0, |a| {
                action_damage_among(spec, a, victim, elemental, Rolls { critical, variance }, among)
            })
        };
        let outcome = match response {
            Some(AffinityRate::Nullify) => Offense::Damage {
                amount: 0,
                critical: false,
            },
            Some(AffinityRate::Death) => Offense::Damage {
                amount: victim.hp.max(1),
                critical: false,
            },
            Some(AffinityRate::FullHeal) => Offense::Heal(victim.max_hp),
            Some(AffinityRate::Absorb) => Offense::Heal(damage_of(Elemental::Normal, false)),
            _ => Offense::Damage {
                // Barrier, MBarrier and Sadness take their share off what the formulas gave.
                amount: status::damage_after_statuses(
                    damage_of(Elemental::of(response), critical),
                    spec.kind == ActionKind::Physical,
                    &victim.status,
                ),
                critical,
            },
        };
        match outcome {
            Offense::Damage { amount, critical } => {
                let mut died = false;
                let mut woke = false;
                // Only an enemy hurting a party member fills a limit gauge (see `crate::limit`).
                let hurts_party = attacker >= self.party_len && target < self.party_len;
                let mut gauge: Option<u16> = None;
                if let Some(t) = self.actors.get_mut(target) {
                    t.hp = t.hp.saturating_sub(amount);
                    if t.hp == 0 {
                        died = true;
                        t.gauge = 0;
                        t.ready = false;
                        t.defending = false;
                        // A knockout removes every status.
                        t.status.clear();
                        // A knocked-out character loses its whole limit gauge.
                        if t.limit != 0 {
                            t.limit = 0;
                            gauge = Some(0);
                        }
                    } else {
                        if hurts_party && amount > 0 {
                            if let Some(limit) = t.limit_break {
                                // Sadness halves the gauge a blow fills, Fury doubles it, Paralysis stops it.
                                let gain = status::limit_gain_after_statuses(limit_gain(amount, t.max_hp, limit.divisor), &t.status);
                                let value = t.limit.saturating_add(gain).min(LIMIT_MAX);
                                if value != t.limit {
                                    t.limit = value;
                                    gauge = Some(value);
                                }
                            }
                        }
                        // A physical blow that does damage wakes a sleeper; a magical one does not.
                        if spec.kind == ActionKind::Physical && amount > 0 {
                            woke = t.status.wake();
                        }
                    }
                }
                events.push(BattleEvent::Damage {
                    actor: actor_id,
                    target: target_id,
                    amount,
                    critical,
                });
                if died {
                    events.push(BattleEvent::Death { actor: target_id });
                }
                if let Some(value) = gauge {
                    events.push(BattleEvent::LimitGauge { actor: target_id, value });
                }
                if woke {
                    events.push(BattleEvent::StatusEnded { target: target_id, status: Status::Sleep });
                }
                self.check_outcome(events);
                // The blow's own status effect, on a target that still stands (see the module docs).
                if self.outcome.is_none() {
                    self.apply_status_effect(target, spec, among, events);
                }
            }
            Offense::Heal(amount) => {
                let gained = match self.actors.get_mut(target) {
                    Some(t) => {
                        let gained = amount.min(t.max_hp.saturating_sub(t.hp));
                        t.hp += gained;
                        gained
                    }
                    None => 0,
                };
                events.push(BattleEvent::Heal {
                    actor: actor_id,
                    target: target_id,
                    amount: gained,
                });
            }
        }
    }

    /// Try the status effect of `spec` on `target` (when it has one and the target stands): one draw
    /// `below(63)` and, when it is below the chance, the effect (see the module docs, "RNG draw order"). What
    /// changed is announced in bit order; a status the target is immune to, has already, or that the rules do not
    /// model says nothing.
    fn apply_status_effect(&mut self, target: usize, spec: &ActionSpec, among: usize, events: &mut Vec<BattleEvent>) {
        let Some(effect) = spec.status else { return };
        let Some(t) = self.actors.get(target) else { return };
        if !t.is_alive() {
            return;
        }
        let roll = self.rng.below(CHANCE_OUT_OF);
        if roll >= effect.chance_among(among > 1 && spec.splits()) {
            return;
        }
        let immune = t.status_immune;
        let target_id = ActorId(target as u8);
        if let Some(t) = self.actors.get_mut(target) {
            for change in t.status.apply(&effect, immune) {
                events.push(match change {
                    Change::Inflicted(status) => BattleEvent::StatusInflicted { target: target_id, status },
                    Change::Ended(status) => BattleEvent::StatusEnded { target: target_id, status },
                });
            }
        }
    }

    /// Resolve one status-only action ([`Formula::None`]) on a target: a curse rolls to hit first, like a spell
    /// of its kind does (an ally, or an item, is never missed), then its status effect.
    fn resolve_status_only(&mut self, user: usize, target: usize, spec: &ActionSpec, among: usize, events: &mut Vec<BattleEvent>) {
        if !spec.on_allies && spec.rolls_accuracy {
            let (luck_roll, miss_roll) = match spec.kind {
                ActionKind::Physical => {
                    let luck = self.rng.range_inclusive(0, 99);
                    (luck, percent_roll(self.rng.next_u16()))
                }
                ActionKind::Magical => (0, percent_roll(self.rng.next_u16())),
            };
            let hit = {
                let (Some(a), Some(t)) = (self.actors.get(user), self.actors.get(target)) else {
                    return;
                };
                let party_member = a.side == Side::Party;
                match spec.kind {
                    ActionKind::Physical => {
                        let chance = status::hit_chance_after_statuses(i64::from(hit_chance_with(a, t, spec.accuracy)), &a.status, false, party_member);
                        let chance = chance.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                        resolve_hit(a, t, chance, luck_roll, miss_roll)
                    }
                    ActionKind::Magical => {
                        status::hit_chance_after_statuses(magic_hit_chance(spec.accuracy, t), &a.status, false, party_member) >= i64::from(miss_roll)
                    }
                }
            };
            if !hit {
                events.push(BattleEvent::Miss { actor: ActorId(user as u8), target: ActorId(target as u8) });
                return;
            }
        }
        self.apply_status_effect(target, spec, among, events);
    }

    /// Resolve one restoring action on an ally: HP, MP, both (Elixir) or a revival.
    fn resolve_restore(
        &mut self,
        user: usize,
        target: usize,
        spec: &ActionSpec,
        among: usize,
        events: &mut Vec<BattleEvent>,
    ) {
        let actor_id = ActorId(user as u8);
        let target_id = ActorId(target as u8);
        let variance = if spec.formula.has_variance() {
            self.rng.range_inclusive(0, 255)
        } else {
            0
        };
        let amount = {
            let (Some(u), Some(t)) = (self.actors.get(user), self.actors.get(target)) else {
                return;
            };
            restore_amount_among(spec, u, t, variance, among)
        };
        let Some(t) = self.actors.get_mut(target) else {
            return;
        };
        if spec.formula == Formula::Full {
            let hp = t.max_hp.saturating_sub(t.hp);
            let mp = t.max_mp.saturating_sub(t.mp);
            t.hp += hp;
            t.mp += mp;
            events.push(BattleEvent::Heal {
                actor: actor_id,
                target: target_id,
                amount: hp,
            });
            events.push(BattleEvent::MpRestored {
                actor: actor_id,
                target: target_id,
                amount: mp,
            });
        } else if spec.hits_dead {
            let hp = amount.clamp(1, t.max_hp.max(1));
            t.hp = hp;
            t.gauge = 0;
            t.ready = false;
            t.defending = false;
            events.push(BattleEvent::Revive {
                actor: actor_id,
                target: target_id,
                hp,
            });
        } else if spec.hits_mp {
            let gained = amount.min(t.max_mp.saturating_sub(t.mp));
            t.mp += gained;
            events.push(BattleEvent::MpRestored {
                actor: actor_id,
                target: target_id,
                amount: gained,
            });
        } else {
            let gained = amount.min(t.max_hp.saturating_sub(t.hp));
            t.hp += gained;
            events.push(BattleEvent::Heal {
                actor: actor_id,
                target: target_id,
                amount: gained,
            });
        }
    }

    /// Set the outcome if a side has been wiped out (enemies are checked first).
    fn check_outcome(&mut self, events: &mut Vec<BattleEvent>) {
        if self.outcome.is_some() {
            return;
        }
        let enemies_alive = self
            .actors
            .iter()
            .skip(self.party_len)
            .any(Combatant::is_alive);
        let party_alive = self
            .actors
            .iter()
            .take(self.party_len)
            .any(Combatant::is_alive);
        if !enemies_alive {
            self.outcome = Some(Outcome::Victory);
            events.push(BattleEvent::Victory);
        } else if !party_alive {
            self.outcome = Some(Outcome::Defeat);
            events.push(BattleEvent::Defeat);
        }
    }
}

/// What an offensive action did to its target.
enum Offense {
    /// Hurt it by this much (0 for a nullified element).
    Damage { amount: u32, critical: bool },
    /// Healed it (an absorbed element, a full heal).
    Heal(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Kernel;
    use crate::scene::SceneBin;
    use crate::testutil::{
        attack_record, demo_scene, gzip, pack_scene_bin, synthetic_kernel_bytes,
    };

    fn fighter(name: &str, level: u8, hp: u32, attack: u32, defense: u32, dex: u32) -> Combatant {
        let mut c = Combatant::new(name, level, hp);
        c.attack = attack;
        c.defense = defense;
        c.dex = dex;
        c
    }

    fn zero_start() -> BattleConfig {
        BattleConfig {
            start: StartMode::Zero,
            ..BattleConfig::default()
        }
    }

    /// One line per event, for golden logs: `ready 0 | act 0 attack | dmg 0->2 90 | ...`.
    fn render(log: &[BattleEvent]) -> String {
        log.iter().map(render_event).collect::<Vec<_>>().join(" | ")
    }

    fn render_event(e: &BattleEvent) -> String {
        match e {
            BattleEvent::GaugeReady { actor } => format!("ready {}", actor.0),
            BattleEvent::Damage {
                actor,
                target,
                amount,
                critical,
            } => {
                format!(
                    "dmg {}->{} {}{}",
                    actor.0,
                    target.0,
                    amount,
                    if *critical { " crit" } else { "" }
                )
            }
            BattleEvent::Miss { actor, target } => format!("miss {}->{}", actor.0, target.0),
            BattleEvent::Death { actor } => format!("death {}", actor.0),
            BattleEvent::Victory => "victory".to_string(),
            BattleEvent::Defeat => "defeat".to_string(),
            BattleEvent::Action { actor, action } => {
                let what = match action {
                    ActionRef::Attack => "attack".to_string(),
                    ActionRef::Magic(s) => format!("magic {s}"),
                    ActionRef::Item(i) => format!("item {i}"),
                    ActionRef::Defend => "defend".to_string(),
                    ActionRef::EnemyAttack(a) => format!("enemy-attack {a}"),
                    ActionRef::Limit(l) => format!("limit {l}"),
                    ActionRef::Escape => "escape".to_string(),
                };
                format!("act {} {what}", actor.0)
            }
            BattleEvent::Heal {
                actor,
                target,
                amount,
            } => format!("heal {}->{} {amount}", actor.0, target.0),
            BattleEvent::MpSpent { actor, amount } => format!("mp-spent {} {amount}", actor.0),
            BattleEvent::MpRestored {
                actor,
                target,
                amount,
            } => format!("mp-restored {}->{} {amount}", actor.0, target.0),
            BattleEvent::Revive { actor, target, hp } => {
                format!("revive {}->{} {hp}", actor.0, target.0)
            }
            BattleEvent::LimitGauge { actor, value } => format!("limit-gauge {} {value}", actor.0),
            BattleEvent::EscapeFailed { actor } => format!("escape-failed {}", actor.0),
            BattleEvent::StatusInflicted { target, status } => format!("status+ {} {status}", target.0),
            BattleEvent::StatusEnded { target, status } => format!("status- {} {status}", target.0),
            BattleEvent::StatusTick { target, status, amount } => format!("status-tick {} {status} {amount}", target.0),
        }
    }

    fn simple_battle(seed: u64, config: BattleConfig) -> BattleState {
        let party = vec![
            fighter("Hero A", 10, 300, 60, 20, 40),
            fighter("Hero B", 10, 280, 55, 18, 30),
        ];
        let mut e1 = fighter("Enemy 1", 8, 120, 30, 10, 50);
        e1.reward = Reward {
            exp: 20,
            gil: 30,
            ap: 2,
        };
        let mut e2 = fighter("Enemy 2", 8, 90, 25, 8, 45);
        e2.reward = Reward {
            exp: 12,
            gil: 18,
            ap: 1,
        };
        BattleState::with_config(seed, party, vec![e1, e2], config)
    }

    /// Drive a battle: step in 100 ms slices; ready party members attack the first living
    /// enemy. Returns every event in order.
    fn play_out(state: &mut BattleState, max_slices: usize) -> Vec<BattleEvent> {
        let mut log = Vec::new();
        for _ in 0..max_slices {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(100));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                let Some(target) = state
                    .enemy_ids()
                    .into_iter()
                    .find(|e| state.actor(*e).is_some_and(Combatant::is_alive))
                else {
                    break;
                };
                if let Ok(ev) = state.submit_command(id, Command::Attack { target }) {
                    log.extend(ev);
                }
            }
        }
        log
    }

    // --- formulas ---

    #[test]
    fn damage_formula_hand_computed() {
        // Att 22, Lvl 6: (28/32)=0 so Base = 22. Power 16, Def 2:
        // 16 * 510 * 22 = 179520; >> 13 = 21.
        let base = DamageInput {
            attack: 22,
            level: 6,
            power: 16,
            defense: 2,
            critical: false,
            halve_for_row: false,
            defending: false,
            elemental: Elemental::Normal,
            variance_roll: 255,
        };
        assert_eq!(physical_damage(&base), 21);
        // Variance floor: 21 * 3841 >> 12 = 19.
        assert_eq!(
            physical_damage(&DamageInput {
                variance_roll: 0,
                ..base
            }),
            19
        );
        // Critical doubles before variance; the row penalty halves.
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                ..base
            }),
            42
        );
        assert_eq!(
            physical_damage(&DamageInput {
                halve_for_row: true,
                ..base
            }),
            10
        );
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                halve_for_row: true,
                ..base
            }),
            21
        );

        // Att 120, Lvl 40: x = 4800/32 = 150, y = 160/32 = 5, Base = 120 + 750 = 870.
        // Power 16, Def 50: 870 * 462 * 16 >> 13 = 785.
        let big = DamageInput {
            attack: 120,
            level: 40,
            power: 16,
            defense: 50,
            critical: false,
            halve_for_row: false,
            defending: false,
            elemental: Elemental::Normal,
            variance_roll: 255,
        };
        assert_eq!(physical_damage(&big), 785);
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                ..big
            }),
            1570
        );
        // Power scales linearly; the attack command's Power 16 is the reference.
        assert_eq!(physical_damage(&DamageInput { power: 32, ..big }), 1570);

        // Minimum of 1 on any hit, and the 9999 cap.
        assert_eq!(physical_damage(&DamageInput { attack: 0, ..big }), 1);
        assert_eq!(
            physical_damage(&DamageInput {
                defense: 600,
                ..big
            }),
            physical_damage(&DamageInput {
                defense: 511,
                ..big
            })
        );
        let huge = DamageInput {
            attack: 255,
            level: 99,
            power: 255,
            defense: 0,
            ..big
        };
        assert_eq!(physical_damage(&huge), MAX_DAMAGE);
    }

    #[test]
    fn hit_and_critical_formulas() {
        let mut party = fighter("P", 10, 100, 0, 0, 20);
        party.accuracy = 96;
        party.evade = 4;
        party.luck = 12;
        party.side = Side::Party;
        let mut enemy = fighter("E", 6, 100, 0, 0, 40);
        enemy.accuracy = 100;
        enemy.evade = 6;
        enemy.luck = 8;
        enemy.side = Side::Enemy;
        // Party attacker: (20>>2) + 96 = 101; own evade (20>>2)+4 = 9; enemy evade 6 => 104.
        assert_eq!(hit_chance(&party, &enemy), 104);
        // Enemy attacker: (40>>2) + 100 = 110; own evade 6 (stat only); party target evade 9.
        assert_eq!(hit_chance(&enemy, &party), 107);
        // Exactly zero is bumped to 1; negative stays negative.
        let mut weak = fighter("W", 1, 1, 0, 0, 0);
        weak.accuracy = 9;
        weak.evade = 0;
        weak.side = Side::Enemy;
        let mut sturdy = fighter("S", 1, 1, 0, 0, 0);
        sturdy.evade = 9;
        sturdy.side = Side::Enemy;
        assert_eq!(hit_chance(&weak, &sturdy), 1);
        sturdy.evade = 20;
        assert_eq!(hit_chance(&weak, &sturdy), -11);

        // Lucky hit: attacker Luck/4 = 3 > roll 2 => always hits even at chance -11.
        weak.luck = 12;
        assert!(resolve_hit(&weak, &sturdy, -11, 2, 100));
        assert!(
            !resolve_hit(&weak, &sturdy, -11, 3, 1),
            "no lucky hit at roll 3"
        );
        // Ordinary threshold: hit when chance >= miss roll.
        assert!(resolve_hit(&enemy, &party, 50, 99, 50));
        assert!(!resolve_hit(&enemy, &party, 50, 99, 51));
        // Lucky dodge: a party attacker (Luck 0 here, so no lucky hit of its own) against an
        // enemy with Luck/4 = 2 > roll 1 always misses, even at chance 255.
        let mut careful = party.clone();
        careful.luck = 0;
        assert!(
            !resolve_hit(&careful, &enemy, 255, 1, 1),
            "lucky dodge forces a miss"
        );
        // ... but not when the roll is too high for the dodge.
        assert!(resolve_hit(&careful, &enemy, 255, 2, 1));
        // The dodge does not apply to enemies attacking the party.
        let mut dodgy_party = party.clone();
        dodgy_party.luck = 99;
        let mut attacker = enemy.clone();
        attacker.luck = 0;
        assert!(resolve_hit(&attacker, &dodgy_party, 255, 1, 1));

        // Critical: ((luck + lvl) - target lvl) >> 2, plus weapon bonus for the party.
        // (12 + 10 - 6) >> 2 = 4; with weapon bonus 3 => 7.
        assert_eq!(critical_chance(&party, &enemy), 4);
        party.crit_bonus = 3;
        assert_eq!(critical_chance(&party, &enemy), 7);
        // Enemies never get the weapon bonus: (8 + 6 - 10) >> 2 = 1 regardless of it.
        enemy.crit_bonus = 50;
        assert_eq!(critical_chance(&enemy, &party), 1);
        // Negative base chances clamp to zero: (0 + 1 - 10) >> 2 = -3.
        let mut low = fighter("L", 1, 1, 0, 0, 0);
        low.side = Side::Enemy;
        assert_eq!(critical_chance(&low, &party), 0);
        // A huge bonus (an always-critical weapon) saturates.
        party.crit_bonus = 255;
        assert!(critical_chance(&party, &enemy) >= 100);
        // Roll mapping: 0 -> 1, 65535 -> 100.
        assert_eq!(percent_roll(0), 1);
        assert_eq!(percent_roll(65_535), 100);
    }

    #[test]
    fn atb_rates_follow_the_documented_formula() {
        // Battle speed 128: 32768 / (120 + 240) = 91, doubled = 182.
        assert_eq!(v_timer_increase(128), 182);
        assert_eq!(v_timer_increase(0), 2 * (32_768 / 120));
        assert_eq!(v_timer_increase(255), 2 * (32_768 / (120 + 255 * 15 / 8)));

        // Party Dex 30 and 40 -> average 35 -> normal speed 85.
        let state = simple_battle(1, zero_start());
        assert_eq!(state.normal_speed(), 85);
        // Party member: (Dex + 50) * 182 / 85.
        assert_eq!(state.gauge_increase(ActorId(0)), Some((40 + 50) * 182 / 85));
        assert_eq!(state.gauge_increase(ActorId(1)), Some((30 + 50) * 182 / 85));
        // Enemy: Dex * 182 / 85 (no +50).
        assert_eq!(state.gauge_increase(ActorId(2)), Some(50 * 182 / 85));
        assert_eq!(state.gauge_increase(ActorId(9)), None);

        // Rounded average: Dex 30 and 41 -> 35.5 -> 36 -> 86.
        let party = vec![fighter("A", 1, 10, 1, 1, 30), fighter("B", 1, 10, 1, 1, 41)];
        let s =
            BattleState::with_config(1, party, vec![fighter("E", 1, 10, 1, 1, 10)], zero_start());
        assert_eq!(s.normal_speed(), 86);
        // An empty party falls back to a normal speed of 50 without dividing by zero.
        let s =
            BattleState::with_config(1, vec![], vec![fighter("E", 1, 10, 1, 1, 10)], zero_start());
        assert_eq!(s.normal_speed(), 50);
    }

    #[test]
    fn ticks_to_fill_for_a_known_dex() {
        // One party member at the average Dex: increase = 182 per tick, so
        // ceil(65535 / 182) = 361 ticks, i.e. just over 6 s at 60 ticks per second.
        let party = vec![fighter("A", 5, 100, 10, 5, 25)];
        let foe = fighter("Slow Foe", 5, 1000, 1, 1, 0); // Dex 0: never fills
        let mut state = BattleState::with_config(7, party, vec![foe], zero_start());
        assert_eq!(state.gauge_increase(ActorId(0)), Some(182));
        assert_eq!(state.gauge_increase(ActorId(1)), Some(0));

        let events = state.step_ticks(360);
        assert!(events.is_empty());
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, 360 * 182);
        assert!(state.ready_actors().is_empty());
        let events = state.step_ticks(1);
        assert_eq!(events, vec![BattleEvent::GaugeReady { actor: ActorId(0) }]);
        assert_eq!(state.tick(), 361);
        assert_eq!(state.ready_actors(), vec![ActorId(0)]);
        // A full gauge stays full and does not repeat the event.
        assert!(state.step_ticks(50).is_empty());
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, GAUGE_MAX);

        // In wall-clock terms: 6000 ms is 360 ticks (not ready); 6017 ms reaches 361.
        let party = vec![fighter("A", 5, 100, 10, 5, 25)];
        let foe = fighter("Slow Foe", 5, 1000, 1, 1, 0);
        let mut timed = BattleState::with_config(7, party, vec![foe], zero_start());
        assert!(timed.step(6000).is_empty());
        assert_eq!(timed.tick(), 360);
        let ev = timed.step(17);
        assert_eq!(ev, vec![BattleEvent::GaugeReady { actor: ActorId(0) }]);
    }

    #[test]
    fn step_chunking_does_not_change_the_outcome() {
        let cfg = BattleConfig {
            start: StartMode::Normal,
            ..BattleConfig::default()
        };
        let mut whole = simple_battle(99, cfg);
        let mut chunked = simple_battle(99, cfg);
        let mut chunked_events = Vec::new();
        let whole_events = whole.step(8000);
        for _ in 0..8000 {
            chunked_events.extend(chunked.step(1));
        }
        assert_eq!(whole_events, chunked_events);
        assert_eq!(whole, chunked);
        // Uneven chunks agree too.
        let mut uneven = simple_battle(99, cfg);
        let mut uneven_events = Vec::new();
        for dt in [1u32, 7, 333, 2, 1500, 41, 6116] {
            uneven_events.extend(uneven.step(dt));
        }
        assert_eq!(whole_events, uneven_events);
        assert_eq!(whole, uneven);
        assert!(!whole_events.is_empty(), "something happens in 8 seconds");
    }

    // --- determinism ---

    #[test]
    fn same_seed_same_battle_different_seed_diverges() {
        let cfg = BattleConfig::default();
        let mut a = simple_battle(2024, cfg);
        let mut b = simple_battle(2024, cfg);
        let la = play_out(&mut a, 600);
        let lb = play_out(&mut b, 600);
        assert_eq!(la, lb);
        assert_eq!(a, b);
        assert!(a.outcome().is_some());

        let mut c = simple_battle(2025, cfg);
        let lc = play_out(&mut c, 600);
        assert_ne!(la, lc);
    }

    #[test]
    fn golden_event_log_for_a_fixed_seed() {
        // Regression guard for the RNG draw order and formulas: this exact event sequence
        // must never change silently, because server and clients rely on it.
        let mut state = simple_battle(1234, zero_start());
        let log = play_out(&mut state, 600);
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        // The numbers are exactly the ones this battle had before magic, items and enemy attacks
        // existed: hand-made fighters have no spells and no attack list, so they draw what they drew
        // then. Only the "act" lines are new (every action is announced).
        assert_eq!(
            render(&log),
            "ready 0 | act 0 attack | dmg 0->2 90 | ready 1 | act 1 attack | dmg 1->2 81 | death 2 | \
             ready 3 | act 3 attack | dmg 3->1 28 | ready 0 | act 0 attack | dmg 0->3 92 | death 3 | victory"
        );
    }

    // --- command validation ---

    #[test]
    fn command_validation_covers_every_error() {
        let mut state = simple_battle(5, zero_start());
        let attack = |t: u8| Command::Attack { target: ActorId(t) };

        // Gauge not full yet.
        assert_eq!(
            state.submit_command(ActorId(0), attack(2)),
            Err(CommandError::GaugeNotFull(ActorId(0)))
        );
        // Unknown actor, and enemies cannot be commanded.
        assert_eq!(
            state.submit_command(ActorId(77), attack(2)),
            Err(CommandError::UnknownActor(ActorId(77)))
        );
        assert_eq!(
            state.submit_command(ActorId(2), attack(0)),
            Err(CommandError::NotControllable(ActorId(2)))
        );

        // Fill hero A's gauge (the enemies may act in the meantime; keep everyone alive).
        let mut guard = 0;
        while !state.actor(ActorId(0)).unwrap().ready {
            state.step(100);
            guard += 1;
            assert!(guard < 1000, "gauge fills eventually");
        }
        assert_eq!(
            state.submit_command(ActorId(0), attack(77)),
            Err(CommandError::UnknownTarget(ActorId(77)))
        );
        assert_eq!(
            state.submit_command(ActorId(0), attack(1)),
            Err(CommandError::InvalidTarget(ActorId(1))),
            "allies are not valid targets"
        );
        assert_eq!(
            state.submit_command(ActorId(0), attack(0)),
            Err(CommandError::InvalidTarget(ActorId(0))),
            "self is not a valid target"
        );
        // A legal command works and empties the gauge; a second one is refused.
        let events = state.submit_command(ActorId(0), attack(2)).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. })));
        let actor = state.actor(ActorId(0)).unwrap();
        assert_eq!((actor.gauge, actor.ready), (0, false));
        assert_eq!(
            state.submit_command(ActorId(0), attack(2)),
            Err(CommandError::GaugeNotFull(ActorId(0)))
        );
    }

    #[test]
    fn dead_targets_are_rejected_and_never_act() {
        // The hero cannot miss (hit chance 130) and one-shots the frail enemy (1 HP).
        let hero = fighter("Hero", 20, 500, 200, 50, 60);
        let frail = fighter("Frail", 1, 1, 1, 0, 1);
        let tough = fighter("Tough", 1, 5000, 1, 255, 1);
        let mut state = BattleState::with_config(3, vec![hero], vec![frail, tough], zero_start());
        let mut guard = 0;
        while state.ready_actors().is_empty() {
            state.step(100);
            guard += 1;
            assert!(guard < 1000);
        }
        let ev = state
            .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
            .unwrap();
        assert!(
            ev.contains(&BattleEvent::Death { actor: ActorId(1) }),
            "{ev:?}"
        );
        assert!(state.outcome().is_none(), "the tough enemy is still alive");

        // The dead enemy is not a valid target and never fills its gauge or acts.
        guard = 0;
        while state.ready_actors().is_empty() {
            state.step(100);
            guard += 1;
            assert!(guard < 2000);
        }
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(1) }),
            Err(CommandError::TargetDead(ActorId(1)))
        );
        let dead = state.actor(ActorId(1)).unwrap();
        assert_eq!((dead.gauge, dead.ready), (0, false));
    }

    #[test]
    fn dead_party_members_cannot_command_and_battle_over_blocks_commands() {
        // A one-member party with 1 HP against a fast, strong enemy dies quickly.
        let hero = fighter("Hero", 1, 1, 1, 0, 1);
        let mut brute = fighter("Brute", 30, 9000, 150, 0, 200);
        brute.luck = 100;
        let mut state = BattleState::with_config(11, vec![hero], vec![brute], zero_start());
        let mut log = Vec::new();
        for _ in 0..2000 {
            log.extend(state.step(100));
            if state.outcome().is_some() {
                break;
            }
        }
        assert_eq!(state.outcome(), Some(Outcome::Defeat));
        assert!(log.contains(&BattleEvent::Death { actor: ActorId(0) }));
        assert_eq!(log.last(), Some(&BattleEvent::Defeat));
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(1) }),
            Err(CommandError::BattleOver)
        );
        // No further events once the outcome is set.
        assert!(state.step(10_000).is_empty());
        assert!(state.step_ticks(10_000).is_empty());
    }

    #[test]
    fn dead_party_members_are_refused_while_the_battle_continues() {
        // Weak (1 HP) and Sturdy fight a fast brute; once the brute happens to pick Weak,
        // Weak is dead but the battle goes on because Sturdy survives.
        let weak = fighter("Weak", 1, 1, 1, 0, 1);
        let sturdy = fighter("Sturdy", 20, 1_000_000, 10, 255, 1);
        let brute = fighter("Brute", 30, 9000, 150, 0, 200);
        let mut state = BattleState::with_config(21, vec![weak, sturdy], vec![brute], zero_start());
        let mut guard = 0;
        while state.actor(ActorId(0)).is_some_and(Combatant::is_alive) {
            state.step(100);
            guard += 1;
            assert!(guard < 5000, "the brute eventually picks the weak hero");
        }
        assert!(state.outcome().is_none());
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(2) }),
            Err(CommandError::ActorDead(ActorId(0)))
        );
        // The dead hero is not listed as ready and its gauge is cleared.
        assert!(!state.ready_actors().contains(&ActorId(0)));
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, 0);
    }

    // --- whole battles ---

    #[test]
    fn victory_run_emits_victory_once_and_totals_rewards() {
        let mut state = simple_battle(31337, zero_start());
        let log = play_out(&mut state, 3000);
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        assert_eq!(
            log.iter().filter(|e| **e == BattleEvent::Victory).count(),
            1
        );
        assert_eq!(log.last(), Some(&BattleEvent::Victory));
        assert!(!log.contains(&BattleEvent::Defeat));
        // Both enemies died exactly once.
        for id in [2u8, 3] {
            assert_eq!(
                log.iter()
                    .filter(|e| **e == BattleEvent::Death { actor: ActorId(id) })
                    .count(),
                1
            );
        }
        assert_eq!(
            state.rewards(),
            Reward {
                exp: 32,
                gil: 48,
                ap: 3
            }
        );
        // Nothing happens afterwards.
        assert!(state.step(5000).is_empty());
    }

    #[test]
    fn defeat_when_the_party_never_acts() {
        let mut state = simple_battle(8, zero_start());
        let mut log = Vec::new();
        for _ in 0..20_000 {
            log.extend(state.step(100));
            if state.outcome().is_some() {
                break;
            }
        }
        assert_eq!(state.outcome(), Some(Outcome::Defeat));
        assert_eq!(log.iter().filter(|e| **e == BattleEvent::Defeat).count(), 1);
        assert_eq!(log.last(), Some(&BattleEvent::Defeat));
        assert!(!log.contains(&BattleEvent::Victory));
        assert!(
            state.ready_actors().is_empty(),
            "dead heroes are not 'ready'"
        );
        assert_eq!(state.rewards(), Reward::default());
    }

    #[test]
    fn hits_misses_and_crits_all_occur_across_seeds() {
        let mut hits = 0;
        let mut misses = 0;
        let mut crits = 0;
        for seed in 0..300u64 {
            let mut hero = fighter("Hero", 15, 100_000, 40, 10, 30);
            hero.accuracy = 60; // a real miss chance
            hero.luck = 60;
            hero.crit_bonus = 20;
            let mut foe = fighter("Foe", 15, 1_000_000, 10, 10, 1);
            foe.evade = 20;
            let mut state = BattleState::with_config(seed, vec![hero], vec![foe], zero_start());
            // Wait until ready, then attack once.
            while state.ready_actors().is_empty() {
                state.step(100);
            }
            for e in state
                .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
                .unwrap()
            {
                match e {
                    BattleEvent::Damage { critical, .. } => {
                        hits += 1;
                        if critical {
                            crits += 1;
                        }
                    }
                    BattleEvent::Miss { .. } => misses += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(hits + misses, 300);
        assert!(hits > 100 && misses > 10, "hits {hits}, misses {misses}");
        assert!(crits > 10 && crits < hits, "crits {crits} of {hits}");
    }

    #[test]
    fn back_row_halves_short_range_damage_only() {
        // Same seed and same draw order in every run, so the only difference between the
        // runs is the row rule. Hit chance is 114 (cannot miss).
        let fixed = |row: Row, short_range: bool, target_row: Row| {
            let mut hero = fighter("Hero", 20, 100, 50, 0, 30);
            hero.row = row;
            hero.short_range = short_range;
            hero.luck = 100;
            let mut foe = fighter("Foe", 20, 1_000_000, 10, 10, 1);
            foe.row = target_row;
            let mut state = BattleState::with_config(4, vec![hero], vec![foe], zero_start());
            while state.ready_actors().is_empty() {
                state.step(100);
            }
            let ev = state
                .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
                .unwrap();
            ev.iter()
                .find_map(|e| match e {
                    BattleEvent::Damage { amount, .. } => Some(*amount),
                    _ => None,
                })
                .expect("a hit")
        };
        let front = fixed(Row::Front, true, Row::Front);
        let back_short = fixed(Row::Back, true, Row::Front);
        let target_back_short = fixed(Row::Front, true, Row::Back);
        let back_long = fixed(Row::Back, false, Row::Back);
        assert!(
            back_short < front,
            "short range from the back row is halved"
        );
        assert_eq!(
            back_short, target_back_short,
            "a back-row target halves it too"
        );
        assert_eq!(back_long, front, "long range ignores rows");
    }

    // --- setup edge cases ---

    #[test]
    fn empty_sides_resolve_immediately_and_deliver_the_event_once() {
        let hero = fighter("Hero", 5, 50, 10, 5, 20);
        let foe = fighter("Foe", 5, 50, 10, 5, 20);
        let mut won = BattleState::new(1, vec![hero.clone()], vec![]);
        assert_eq!(won.outcome(), Some(Outcome::Victory));
        assert_eq!(won.step(1000), vec![BattleEvent::Victory]);
        assert!(won.step(1000).is_empty());

        let mut lost = BattleState::new(1, vec![], vec![foe.clone()]);
        assert_eq!(lost.outcome(), Some(Outcome::Defeat));
        assert_eq!(lost.step_ticks(1), vec![BattleEvent::Defeat]);

        // An enemy that starts with zero HP is dead from the start and never acts.
        let mut corpse = foe;
        corpse.hp = 0;
        let state = BattleState::new(1, vec![hero], vec![corpse]);
        assert_eq!(state.outcome(), Some(Outcome::Victory));
    }

    #[test]
    fn start_modes_set_initial_gauges() {
        let mk = |start: StartMode| {
            let party = vec![
                fighter("A", 5, 50, 10, 5, 30),
                fighter("B", 5, 50, 10, 5, 30),
            ];
            let enemies = vec![fighter("E", 5, 50, 10, 5, 30)];
            BattleState::with_config(
                77,
                party,
                enemies,
                BattleConfig {
                    start,
                    ..BattleConfig::default()
                },
            )
        };
        // Zero: everything empty.
        let s = mk(StartMode::Zero);
        assert!(s.actors().iter().all(|c| c.gauge == 0 && !c.ready));
        // Normal: partial fills only, never full.
        let s = mk(StartMode::Normal);
        assert!(s
            .actors()
            .iter()
            .all(|c| c.gauge <= INITIAL_GAUGE_MAX && !c.ready));
        assert!(s.actors().iter().any(|c| c.gauge > 0));

        // Preemptive: party full and waiting, enemies empty; events delivered with the first
        // step (or command).
        let mut s = mk(StartMode::Preemptive);
        assert_eq!(s.ready_actors(), vec![ActorId(0), ActorId(1)]);
        assert_eq!(s.actor(ActorId(2)).unwrap().gauge, 0);
        let first = s.step(0);
        assert_eq!(
            first,
            vec![
                BattleEvent::GaugeReady { actor: ActorId(0) },
                BattleEvent::GaugeReady { actor: ActorId(1) }
            ]
        );
        assert!(s.step(0).is_empty());

        // Back attack: the enemy is full, so it strikes on the very first tick.
        let mut s = mk(StartMode::BackAttack);
        assert!(s.ready_actors().is_empty());
        let ev = s.step_ticks(1);
        assert_eq!(ev[0], BattleEvent::GaugeReady { actor: ActorId(2) });
        assert!(ev.iter().any(|e| matches!(
            e,
            BattleEvent::Damage {
                actor: ActorId(2),
                ..
            } | BattleEvent::Miss {
                actor: ActorId(2),
                ..
            }
        )));

        // Pending start events are also delivered by an early command.
        let mut s = mk(StartMode::Preemptive);
        let ev = s
            .submit_command(ActorId(0), Command::Attack { target: ActorId(2) })
            .unwrap();
        assert_eq!(ev[0], BattleEvent::GaugeReady { actor: ActorId(0) });

        assert_eq!(
            StartMode::from_layout(BattleLayout::Preemptive),
            StartMode::Preemptive
        );
        assert_eq!(
            StartMode::from_layout(BattleLayout::BackAttack),
            StartMode::BackAttack
        );
        assert_eq!(
            StartMode::from_layout(BattleLayout::Pincer),
            StartMode::Normal
        );
    }

    #[test]
    fn construction_sanitises_inputs() {
        // Oversized sides are truncated; hp above max is clamped; sides are reassigned.
        let party: Vec<Combatant> = (0..40)
            .map(|i| fighter(&format!("P{i}"), 5, 50, 5, 5, 5))
            .collect();
        let mut odd = fighter("Odd", 5, 50, 5, 5, 5);
        odd.hp = 9999;
        odd.side = Side::Enemy;
        let state = BattleState::new(1, party, vec![odd]);
        assert_eq!(state.party_ids().len(), MAX_PER_SIDE);
        assert_eq!(state.enemy_ids().len(), 1);
        let enemy = state.actor(state.enemy_ids()[0]).unwrap();
        assert_eq!((enemy.hp, enemy.side), (50, Side::Enemy));
        assert!(state
            .actors()
            .iter()
            .take(MAX_PER_SIDE)
            .all(|c| c.side == Side::Party));

        // Config clamps: tick rate in 1..=1000.
        let cfg = BattleConfig {
            ticks_per_second: 0,
            ..BattleConfig::default()
        };
        let s = BattleState::with_config(1, vec![], vec![], cfg);
        assert_eq!(s.config().ticks_per_second, 1);
        let cfg = BattleConfig {
            ticks_per_second: 100_000,
            ..BattleConfig::default()
        };
        let s = BattleState::with_config(1, vec![], vec![], cfg);
        assert_eq!(s.config().ticks_per_second, 1000);

        // Absurd step sizes are bounded: one call consumes at most MAX_STEP_MS of time
        // (60 s at the default 60 ticks per second = 3600 ticks) or MAX_STEP_TICKS ticks.
        let mut s = simple_battle(1, zero_start());
        let _ = s.step(u32::MAX);
        assert!(s.tick() <= 3600);
        let before = s.tick();
        let _ = s.step_ticks(u64::MAX);
        assert!(s.tick() - before <= MAX_STEP_TICKS);
    }

    #[test]
    fn absurd_stat_values_do_not_overflow_or_panic() {
        let max = DamageInput {
            attack: u32::MAX,
            level: u32::MAX,
            power: u32::MAX,
            defense: u32::MAX,
            critical: true,
            halve_for_row: false,
            defending: true,
            elemental: Elemental::Double,
            variance_roll: u32::MAX,
        };
        assert!((1..=MAX_DAMAGE).contains(&physical_damage(&max)));

        let mut a = Combatant::new("A", 255, u32::MAX);
        let mut b = Combatant::new("B", 255, u32::MAX);
        for c in [&mut a, &mut b] {
            c.attack = u32::MAX;
            c.defense = u32::MAX;
            c.dex = u32::MAX;
            c.luck = u32::MAX;
            c.evade = u32::MAX;
            c.accuracy = u32::MAX;
            c.attack_power = u32::MAX;
            c.crit_bonus = u32::MAX;
            c.max_mp = u32::MAX;
        }
        let mut state = BattleState::new(1, vec![a.clone()], vec![b.clone()]);
        assert_eq!(state.normal_speed(), u32::MAX); // saturated, no overflow
        let _ = state.step(60_000);
        let _ = hit_chance(&a, &b);
        let _ = critical_chance(&a, &b);
        let _ = turn_timer_increase(Side::Party, u32::MAX, u32::MAX, 0);
        // The engine stays usable.
        for id in state.ready_actors() {
            let target = state.enemy_ids()[0];
            let _ = state.submit_command(id, Command::Attack { target });
        }
    }

    // --- data-driven constructors ---

    #[test]
    fn combatants_from_scene_data() {
        let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
        let scene = bin.scene(0).unwrap();
        let enemies = enemies_for_formation(scene, 0);
        assert_eq!(enemies.len(), 2);
        let grunt = &enemies[0];
        assert_eq!(grunt.name, "Test Grunt");
        assert_eq!(grunt.side, Side::Enemy);
        assert_eq!((grunt.level, grunt.hp, grunt.max_hp), (7, 316, 316));
        assert_eq!(
            (
                grunt.attack,
                grunt.defense,
                grunt.dex,
                grunt.luck,
                grunt.evade
            ),
            (21, 11, 60, 5, 3)
        );
        assert_eq!((grunt.mp, grunt.max_mp), (12, 12));
        // The grunt's basic attack is "Test Bite" (accuracy 95, power 20), not the magic one.
        assert_eq!((grunt.accuracy, grunt.attack_power), (95, 20));
        assert!(grunt.short_range);
        assert_eq!(
            grunt.reward,
            Reward {
                exp: 77,
                gil: 55,
                ap: 4
            }
        );
        let hound = &enemies[1];
        assert_eq!((hound.accuracy, hound.attack_power), (90, 16));
        // Formation 1 has two grunts; empty and out-of-range formations give nothing.
        assert_eq!(enemies_for_formation(scene, 1).len(), 2);
        assert!(enemies_for_formation(scene, 2).is_empty());
        assert!(enemies_for_formation(scene, 9).is_empty());

        // Without a basic attack the enemy falls back to Power 16 at 100%.
        let rec = scene.enemies[0].as_ref().unwrap();
        let fallback = Combatant::from_enemy(rec, None);
        assert_eq!(
            (fallback.accuracy, fallback.attack_power),
            (100, ATTACK_COMMAND_POWER)
        );
    }

    #[test]
    fn combatants_from_kernel_data() {
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // Cloud (init id 0): level 6; primary stats 16 each; starting weapon 0 (attack 10,
        // accuracy 90, short range) and armor 0 (defense 5, evade 0).
        let cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
        assert_eq!(cloud.name, "Cloud");
        assert_eq!(cloud.side, Side::Party);
        assert_eq!(cloud.level, 6);
        assert_eq!((cloud.hp, cloud.max_hp), (280, 280));
        assert_eq!((cloud.mp, cloud.max_mp), (50, 50));
        assert_eq!(cloud.attack, 16 + 10);
        assert_eq!(cloud.defense, 16 + 5);
        assert_eq!((cloud.dex, cloud.luck, cloud.evade), (16, 16, 0));
        assert_eq!(
            (cloud.accuracy, cloud.crit_bonus, cloud.attack_power),
            (90, 0, 16)
        );
        assert!(cloud.short_range);
        assert_eq!(cloud.row, Row::Front);

        // Explicit equipment overrides the starting kit: weapon 1 is long range with crit 1.
        let gunner =
            Combatant::from_character(&kernel, CharacterId::Cloud, 6, Some(1), Some(2)).unwrap();
        // Armor 2: defense 5 + 3*2 = 11, evade 2*2 = 4 percent.
        assert_eq!(gunner.attack, 16 + 11);
        assert_eq!(gunner.defense, 16 + 11);
        assert_eq!(gunner.evade, 4);
        assert_eq!((gunner.accuracy, gunner.crit_bonus), (91, 1));
        assert!(!gunner.short_range);

        // Aerith starts in the back row (init record id 3).
        let aerith =
            Combatant::from_character(&kernel, CharacterId::Aeris, 10, None, None).unwrap();
        assert_eq!(aerith.row, Row::Back);
        // Cait Sith has no initialization record: unarmed, unarmored, 100% accuracy.
        let cait =
            Combatant::from_character(&kernel, CharacterId::CaitSith, 10, None, None).unwrap();
        let stats = kernel.growth.stat_block(CharacterId::CaitSith, 10).unwrap();
        assert_eq!(cait.attack, u32::from(stats.strength));
        assert_eq!(cait.defense, u32::from(stats.vitality));
        assert_eq!((cait.accuracy, cait.evade, cait.crit_bonus), (100, 0, 0));
        assert!(cait.short_range);

        // Levels are clamped by the stat block.
        let capped =
            Combatant::from_character(&kernel, CharacterId::Cloud, 255, None, None).unwrap();
        assert_eq!(capped.level, 99);
    }

    #[test]
    fn shared_battle_party_of_two_players_vs_a_formation() {
        // End-to-end with synthetic data: two player characters against a scene formation.
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, 12, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Barret, 12, None, None).unwrap(),
        ];
        let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
        let mut state = BattleState::new(0xFEED_FACE, party, enemies);
        let log = play_out(&mut state, 5000);
        assert!(state.outcome().is_some());
        assert!(log.iter().any(|e| matches!(e, BattleEvent::Damage { .. })));
        // Replaying with the same seed reproduces the identical log.
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, 12, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Barret, 12, None, None).unwrap(),
        ];
        let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
        let mut replay = BattleState::new(0xFEED_FACE, party, enemies);
        assert_eq!(play_out(&mut replay, 5000), log);
    }

    // --- magic, items, Defend, elements and the enemies' own attacks ---
    //
    // These fixtures use hand-made combatants whose numbers are easy to follow by hand. Enemies that
    // must not act have Dex 0 (their gauge never fills); heroes start from empty gauges
    // (`zero_start`) and `wait_ready` runs the clock until they can act, so the RNG is untouched and
    // the draws of one command can be replayed in the test and compared with `state.rng`.

    #[allow(clippy::too_many_arguments)] // a table row of an attack record, spelled out
    fn spell(
        index: u16,
        calc: u8,
        power: u8,
        accuracy: u8,
        targets: u8,
        element: u16,
        flags: u16,
        mp: u16,
    ) -> Spell {
        Spell::from_attack(
            index,
            &attack_record(calc, power, accuracy, targets, element, flags, mp),
        )
        .expect("a castable spell")
    }

    fn fire() -> Spell {
        spell(27, 0x22, 8, 100, 0x0F, element::FIRE, 0xFDFF, 4)
    }

    fn cure() -> Spell {
        spell(0, 0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF, 5)
    }

    fn life() -> Spell {
        spell(7, 0x24, 8, 255, 0x0D, element::RESTORATIVE, 0xFCFD, 34)
    }

    /// Level 10, 500 HP, 50 MP, Magic 20, Strength 30, Cure, Fire and Life.
    fn mage() -> Combatant {
        let mut c = Combatant::new("Mage", 10, 500);
        c.mp = 50;
        c.max_mp = 50;
        c.magic = 20;
        c.attack = 30;
        c.defense = 10;
        c.dex = 40;
        c.spells = vec![cure(), fire(), life()];
        c
    }

    /// A dead party member (400 max HP).
    fn corpse() -> Combatant {
        let mut c = Combatant::new("Corpse", 10, 400);
        c.hp = 0;
        c.mp = 20;
        c.max_mp = 20;
        c.dex = 40;
        c
    }

    /// An enemy that stands there: 1000 HP, no defense, magic defense 8, never acts.
    fn dummy() -> Combatant {
        let mut c = Combatant::new("Dummy", 5, 1000);
        c.magic_defense = 8;
        c
    }

    fn item_table() -> Arc<ItemTable> {
        Arc::new(ItemTable::from_kernel(
            &Kernel::parse(&synthetic_kernel_bytes()).unwrap(),
        ))
    }

    fn battle(party: Vec<Combatant>, enemies: Vec<Combatant>, seed: u64) -> BattleState {
        BattleState::with_config(seed, party, enemies, zero_start()).with_items(item_table())
    }

    /// Run the clock until `who` can act.
    fn wait_ready(state: &mut BattleState, who: ActorId) {
        for _ in 0..5000 {
            if state.actor(who).is_some_and(|c| c.ready) {
                return;
            }
            state.step(100);
        }
        panic!("{who:?} never became ready");
    }

    /// The generator after the draws a command is documented to make.
    fn rng_after(seed: u64, draws: impl FnOnce(&mut Rng)) -> Rng {
        let mut rng = Rng::new(seed);
        draws(&mut rng);
        rng
    }

    fn hero() -> ActorId {
        ActorId(0)
    }

    fn foe() -> ActorId {
        ActorId(1)
    }

    #[test]
    fn the_formulas_are_hand_computed() {
        let (me, target) = (mage(), dummy());
        let dmg = |spec: &ActionSpec, who: &Combatant, victim: &Combatant, elemental, variance| {
            action_damage(
                spec,
                who,
                victim,
                elemental,
                Rolls {
                    critical: false,
                    variance,
                },
            )
        };
        // Fire (spell formula): (Lvl 10 + Mag 20) * 6 = 180; 180 * (512 - MDef 8) * Power 8 >> 13 = 88; the
        // variance scales 3841..=4096 of 4096 (82 at the bottom).
        assert_eq!(dmg(&fire().spec, &me, &target, Elemental::Normal, 255), 88);
        assert_eq!(dmg(&fire().spec, &me, &target, Elemental::Normal, 0), 82);
        // Weak: doubled before the variance; resistant: halved.
        assert_eq!(dmg(&fire().spec, &me, &target, Elemental::Double, 255), 176);
        assert_eq!(dmg(&fire().spec, &me, &target, Elemental::Half, 255), 44);
        // The standard formula with magic: Stat 20 at level 10: x = 200 / 32 = 6, y = 30 / 32 = 0, base 20;
        // 20 * 504 * 16 >> 13 = 19.
        let standard_magic = spell(1, 0x21, 16, 100, 0x0F, 0, 0xFFFF, 0);
        assert_eq!(
            dmg(&standard_magic.spec, &me, &target, Elemental::Normal, 255),
            19
        );
        // Demi: a quarter of the target's CURRENT HP (8 / 32), no variance.
        let demi = spell(42, 0x23, 8, 75, 0x0F, element::GRAVITY, 0xFFFF, 14);
        assert_eq!(dmg(&demi.spec, &me, &target, Elemental::Normal, 0), 250);
        let mut hurt = dummy();
        hurt.hp = 400;
        assert_eq!(dmg(&demi.spec, &me, &hurt, Elemental::Normal, 0), 100);
        // The max-HP fraction as damage (not restorative), the flat one, Cure's formula as damage and the
        // fixed one that defense reduces (magic defense 8 for magic, defense 0 for a physical one).
        let gravity_max = spell(2, 0x24, 16, 100, 0x0F, element::GRAVITY, 0xFFFF, 0);
        assert_eq!(
            dmg(&gravity_max.spec, &me, &target, Elemental::Normal, 0),
            500
        );
        let flat = spell(3, 0x26, 12, 100, 0x0F, element::GRAVITY, 0xFFFF, 0);
        assert_eq!(dmg(&flat.spec, &me, &target, Elemental::Normal, 0), 240);
        let restore_formula = spell(4, 0x25, 10, 100, 0x0F, element::GRAVITY, 0xFFFF, 0);
        assert_eq!(
            dmg(&restore_formula.spec, &me, &target, Elemental::Normal, 255),
            400,
            "22 * 10 + 6 * 30"
        );
        let fixed_magic = spell(5, 0x27, 16, 100, 0x0F, 0, 0xFFFF, 0);
        assert_eq!(
            dmg(&fixed_magic.spec, &me, &target, Elemental::Normal, 255),
            252
        );
        let grenade = ItemTable::from_kernel(&Kernel::parse(&synthetic_kernel_bytes()).unwrap())
            .get(20)
            .unwrap()
            .spec;
        assert_eq!(
            dmg(&grenade, &me, &target, Elemental::Normal, 255),
            160,
            "512 * 10 / 32"
        );
        // The attack command: Str 30, level 10: base 30 + 9 * 1 = 39; 39 * 512 * 16 >> 13 = 39; a critical
        // hit doubles it, Defend halves it (physical damage only).
        let attack = me.attack_spec();
        assert_eq!(dmg(&attack, &me, &target, Elemental::Normal, 255), 39);
        assert_eq!(
            action_damage(
                &attack,
                &me,
                &target,
                Elemental::Normal,
                Rolls {
                    critical: true,
                    variance: 255
                }
            ),
            78
        );
        let mut guard = dummy();
        guard.defending = true;
        assert_eq!(dmg(&attack, &me, &guard, Elemental::Normal, 255), 19);
        assert_eq!(
            dmg(&fire().spec, &me, &guard, Elemental::Normal, 255),
            88,
            "a guard does not stop magic"
        );
        // Pierce: a cleared 0x0400 flag leaves defense out (magic defense 8 -> 0: 180 * 512 * 8 >> 13 = 90).
        let piercing = spell(6, 0x22, 8, 100, 0x0F, element::FIRE, 0xFBFF, 0);
        assert_eq!(
            dmg(&piercing.spec, &me, &target, Elemental::Normal, 255),
            90
        );
        // The damage never drops under 1 or passes the cap, whatever the stats.
        let weak = Combatant::new("Weak", 1, 10);
        assert_eq!(dmg(&fire().spec, &weak, &target, Elemental::Half, 0), 1);
        let mut god = mage();
        god.magic = u32::MAX;
        god.level = 255;
        assert_eq!(
            dmg(
                &spell(8, 0x22, 255, 100, 0x0F, 0, 0xFFFF, 0).spec,
                &god,
                &Combatant::new("T", 1, 1),
                Elemental::Double,
                255
            ),
            MAX_DAMAGE
        );

        // Restoring: Cure (22 * 5 + 6 * (10 + 20) = 290, with the variance), Life and Phoenix Down (max HP * 8 / 32),
        // a potion's 20 * Power, an Ether on MP, a Turbo Ether (all the MP) and an Elixir (everything).
        let ally = mage();
        assert_eq!(restore_amount(&cure().spec, &me, &ally, 255), 290);
        assert_eq!(restore_amount(&cure().spec, &me, &ally, 0), 271);
        let table = item_table();
        let spec = |item: u16| table.get(item).unwrap().spec;
        assert_eq!(restore_amount(&life().spec, &me, &ally, 0), 125);
        assert_eq!(restore_amount(&spec(7), &me, &ally, 0), 125);
        assert_eq!(restore_amount(&spec(0), &me, &ally, 0), 100);
        assert_eq!(restore_amount(&spec(1), &me, &ally, 0), 500);
        assert_eq!(
            restore_amount(&spec(2), &me, &ally, 0),
            500,
            "X-Potion: all the max HP"
        );
        assert_eq!(
            restore_amount(&spec(3), &me, &ally, 0),
            100,
            "Ether: 20 * 5 MP"
        );
        assert_eq!(
            restore_amount(&spec(4), &me, &ally, 0),
            50,
            "Turbo Ether: all the max MP"
        );
        assert_eq!(restore_amount(&spec(5), &me, &ally, 0), 500);
    }

    #[test]
    fn a_damaging_spell_costs_mp_rolls_to_hit_and_follows_the_spell_formula() {
        let seed = 31;
        let mut state = battle(vec![mage()], vec![dummy()], seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 27,
                    target: Target::One(foe()),
                },
            )
            .unwrap();
        // The draws, replayed: the roll to hit (a magical action has no luck roll), then the variance.
        let mut replay = Rng::new(seed);
        let miss_roll = percent_roll(replay.next_u16());
        let variance = replay.range_inclusive(0, 255);
        assert!(miss_roll <= 100, "Fire's 100% cannot miss");
        let amount = action_damage(
            &fire().spec,
            &mage(),
            &dummy(),
            Elemental::Normal,
            Rolls {
                critical: false,
                variance,
            },
        );
        assert!((82..=88).contains(&amount));
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Magic(27)
                },
                BattleEvent::MpSpent {
                    actor: hero(),
                    amount: 4
                },
                BattleEvent::Damage {
                    actor: hero(),
                    target: foe(),
                    amount,
                    critical: false
                },
            ]
        );
        assert_eq!(state.rng, replay, "exactly those two draws, in that order");
        let me = state.actor(hero()).unwrap();
        assert_eq!(
            (me.mp, me.gauge, me.ready),
            (46, 0, false),
            "4 MP paid, the turn is spent"
        );
        assert_eq!(state.actor(foe()).unwrap().hp, 1000 - amount);
    }

    #[test]
    fn a_spell_that_misses_says_so_and_still_costs_the_mp() {
        // Demi's 75% misses for some rolls: find a seed where the roll is above 75.
        let demi = spell(42, 0x23, 8, 75, 0x0F, element::GRAVITY, 0xFFFF, 14);
        let mut caster = mage();
        caster.spells.push(demi);
        let miss_seed = (0..200u64)
            .find(|&seed| percent_roll(Rng::new(seed).next_u16()) > 75)
            .expect("a seed with a high roll");
        let mut state = battle(vec![caster.clone()], vec![dummy()], miss_seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 42,
                    target: Target::One(foe()),
                },
            )
            .unwrap();
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Magic(42)
                },
                BattleEvent::MpSpent {
                    actor: hero(),
                    amount: 14
                },
                BattleEvent::Miss {
                    actor: hero(),
                    target: foe()
                },
            ]
        );
        assert_eq!(
            state.rng,
            rng_after(miss_seed, |r| _ = r.next_u16()),
            "a miss draws only the roll to hit"
        );
        assert_eq!(state.actor(hero()).unwrap().mp, 36);
        assert_eq!(state.actor(foe()).unwrap().hp, 1000);
        // And a seed with a low roll hits for a quarter of the target's current HP, with no variance draw.
        let hit_seed = (0..200u64)
            .find(|&seed| percent_roll(Rng::new(seed).next_u16()) <= 75)
            .unwrap();
        let mut state = battle(vec![caster], vec![dummy()], hit_seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 42,
                    target: Target::One(foe()),
                },
            )
            .unwrap();
        assert_eq!(
            events[2],
            BattleEvent::Damage {
                actor: hero(),
                target: foe(),
                amount: 250,
                critical: false
            }
        );
        assert_eq!(
            state.rng,
            rng_after(hit_seed, |r| _ = r.next_u16()),
            "Demi has no variance: one draw"
        );
    }

    #[test]
    fn a_magic_hit_can_win_the_battle() {
        let mut weak = dummy();
        weak.hp = 10;
        weak.max_hp = 10;
        weak.reward = Reward {
            exp: 5,
            gil: 6,
            ap: 1,
        };
        let mut state = battle(vec![mage()], vec![weak], 3);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 27,
                    target: Target::One(foe()),
                },
            )
            .unwrap();
        assert!(matches!(events[2], BattleEvent::Damage { .. }));
        assert_eq!(
            events[3..],
            [BattleEvent::Death { actor: foe() }, BattleEvent::Victory]
        );
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        assert_eq!(
            state.rewards(),
            Reward {
                exp: 5,
                gil: 6,
                ap: 1
            }
        );
        assert_eq!(
            state.submit_command(hero(), Command::Defend),
            Err(CommandError::BattleOver)
        );
    }

    #[test]
    fn cure_heals_with_a_variance_roll_and_never_more_than_is_missing() {
        let seed = 77;
        let mut hurt = mage();
        hurt.hp = 100;
        let mut state = battle(vec![hurt.clone()], vec![dummy()], seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 0,
                    target: Target::One(hero()),
                },
            )
            .unwrap();
        // Restoring never rolls to hit: the one draw is the variance.
        let mut replay = Rng::new(seed);
        let variance = replay.range_inclusive(0, 255);
        let amount = restore_amount(&cure().spec, &hurt, &hurt, variance);
        assert!((271..=290).contains(&amount));
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Magic(0)
                },
                BattleEvent::MpSpent {
                    actor: hero(),
                    amount: 5
                },
                BattleEvent::Heal {
                    actor: hero(),
                    target: hero(),
                    amount
                },
            ]
        );
        assert_eq!(state.rng, replay);
        assert_eq!(state.actor(hero()).unwrap().hp, 100 + amount);
        assert_eq!(state.actor(hero()).unwrap().mp, 45);

        // One HP short of full: one HP is healed, and that is what the event says.
        let mut nearly = mage();
        nearly.hp = 499;
        let mut state = battle(vec![nearly], vec![dummy()], seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 0,
                    target: Target::One(hero()),
                },
            )
            .unwrap();
        assert_eq!(
            events[2],
            BattleEvent::Heal {
                actor: hero(),
                target: hero(),
                amount: 1
            }
        );
        assert_eq!(state.actor(hero()).unwrap().hp, 500);
        // At full health it is a wasted spell, reported as a heal of 0.
        let events = {
            wait_ready(&mut state, hero());
            state
                .submit_command(
                    hero(),
                    Command::Magic {
                        spell: 0,
                        target: Target::One(hero()),
                    },
                )
                .unwrap()
        };
        assert_eq!(
            events[2],
            BattleEvent::Heal {
                actor: hero(),
                target: hero(),
                amount: 0
            }
        );
    }

    #[test]
    fn spell_commands_are_validated_in_a_fixed_order_and_check_command_agrees() {
        let mut party = vec![mage(), corpse()];
        party[0].mp = 10;
        let mut dead_enemy = dummy();
        dead_enemy.hp = 0;
        let mut state = battle(party, vec![dummy(), dead_enemy], 9);
        let magic = |spell, target: u8| Command::Magic {
            spell,
            target: Target::One(ActorId(target)),
        };
        // The gauge comes first, then everything about the command itself.
        assert_eq!(
            state.check_command(hero(), &magic(27, 2)),
            Err(CommandError::GaugeNotFull(hero()))
        );
        wait_ready(&mut state, hero());
        let cases: Vec<(Command, Result<(), CommandError>)> = vec![
            (magic(99, 2), Err(CommandError::UnknownSpell(99))),
            (magic(27, 9), Err(CommandError::UnknownTarget(ActorId(9)))),
            (magic(27, 0), Err(CommandError::InvalidTarget(ActorId(0)))), // Fire on an ally
            (magic(0, 2), Err(CommandError::InvalidTarget(ActorId(2)))),  // Cure on an enemy
            (magic(27, 3), Err(CommandError::TargetDead(ActorId(3)))),
            (magic(0, 1), Err(CommandError::TargetDead(ActorId(1)))), // Cure on a dead ally
            // The MP is checked before the target: Life costs 34 and the mage has 10.
            (magic(7, 1), Err(CommandError::NotEnoughMp(hero()))),
            (magic(7, 0), Err(CommandError::NotEnoughMp(hero()))),
            (magic(27, 2), Ok(())),
            (magic(0, 0), Ok(())),
            (Command::Defend, Ok(())),
        ];
        let before = state.clone();
        for (command, expected) in &cases {
            assert_eq!(
                state.check_command(hero(), command),
                *expected,
                "{command:?}"
            );
            assert_eq!(state, before, "checking changes nothing ({command:?})");
        }
        // Every refusal changes nothing either (no MP paid, no draw, no event, the gauge stays full).
        for (command, expected) in &cases {
            if let Err(e) = expected {
                assert_eq!(
                    state.submit_command(hero(), *command),
                    Err(*e),
                    "{command:?}"
                );
                assert_eq!(
                    state, before,
                    "a refused command changes nothing ({command:?})"
                );
            }
        }
        // The same checks for an enemy, a dead hero and an unknown actor.
        assert_eq!(
            state.check_command(ActorId(2), &Command::Defend),
            Err(CommandError::NotControllable(ActorId(2)))
        );
        assert_eq!(
            state.check_command(ActorId(1), &Command::Defend),
            Err(CommandError::ActorDead(ActorId(1)))
        );
        assert_eq!(
            state.check_command(ActorId(9), &Command::Defend),
            Err(CommandError::UnknownActor(ActorId(9)))
        );
        // With enough MP, Life brings the dead back, and only the dead.
        let mut rich = battle(vec![mage(), corpse()], vec![dummy()], 9);
        wait_ready(&mut rich, hero());
        assert_eq!(rich.check_command(hero(), &magic(7, 1)), Ok(()));
        assert_eq!(
            rich.check_command(hero(), &magic(7, 0)),
            Err(CommandError::TargetNotDead(hero()))
        );
        assert_eq!(
            rich.check_command(hero(), &magic(7, 2)),
            Err(CommandError::InvalidTarget(ActorId(2)))
        );
    }

    #[test]
    fn life_and_phoenix_down_revive_with_a_fraction_of_max_hp_and_an_empty_gauge() {
        let mut state = battle(vec![mage(), corpse(), corpse()], vec![dummy()], 12);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 7,
                    target: Target::One(ActorId(1)),
                },
            )
            .unwrap();
        // Life: 400 * 8 / 32 = 100 HP, 34 MP, no draws (not a formula with variance, it never misses).
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Magic(7)
                },
                BattleEvent::MpSpent {
                    actor: hero(),
                    amount: 34
                },
                BattleEvent::Revive {
                    actor: hero(),
                    target: ActorId(1),
                    hp: 100
                },
            ]
        );
        assert_eq!(state.rng, Rng::new(12));
        let revived = state.actor(ActorId(1)).unwrap();
        assert_eq!((revived.hp, revived.gauge, revived.ready), (100, 0, false));
        assert!(revived.is_alive());
        // A revived ally is no longer a target for another revival, and fights on (its gauge fills again).
        wait_ready(&mut state, hero());
        assert_eq!(
            state.check_command(
                hero(),
                &Command::Item {
                    item: 7,
                    target: Target::One(ActorId(1))
                }
            ),
            Err(CommandError::TargetNotDead(ActorId(1)))
        );
        wait_ready(&mut state, ActorId(1));
        // Phoenix Down (item 7) on the other one: the same fraction, no MP, still no draw.
        let events = state
            .submit_command(
                hero(),
                Command::Item {
                    item: 7,
                    target: Target::One(ActorId(2)),
                },
            )
            .unwrap();
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Item(7)
                },
                BattleEvent::Revive {
                    actor: hero(),
                    target: ActorId(2),
                    hp: 100
                },
            ]
        );
        assert_eq!(state.actor(hero()).unwrap().mp, 16, "the item cost no MP");
    }

    #[test]
    fn items_restore_hp_and_mp_by_their_own_numbers_and_never_roll() {
        let seed = 5;
        let mut me = mage();
        me.hp = 100;
        me.max_hp = 2000;
        me.mp = 10;
        me.max_mp = 150;
        let mut state = battle(vec![me], vec![dummy()], seed);
        let item = |item: u16| Command::Item {
            item,
            target: Target::One(hero()),
        };
        let cases: [(u16, Vec<BattleEvent>); 5] = [
            // Potion: 20 * 5 = 100.
            (
                0,
                vec![BattleEvent::Heal {
                    actor: hero(),
                    target: hero(),
                    amount: 100,
                }],
            ),
            // Hi-Potion: 20 * 25 = 500.
            (
                1,
                vec![BattleEvent::Heal {
                    actor: hero(),
                    target: hero(),
                    amount: 500,
                }],
            ),
            // X-Potion: all the max HP (2000), limited to what is missing (2000 - 700 = 1300).
            (
                2,
                vec![BattleEvent::Heal {
                    actor: hero(),
                    target: hero(),
                    amount: 1300,
                }],
            ),
            // Ether: 100 MP of the 140 missing.
            (
                3,
                vec![BattleEvent::MpRestored {
                    actor: hero(),
                    target: hero(),
                    amount: 100,
                }],
            ),
            // Elixir: HP (nothing is missing) and the MP that is (40).
            (
                5,
                vec![
                    BattleEvent::Heal {
                        actor: hero(),
                        target: hero(),
                        amount: 0,
                    },
                    BattleEvent::MpRestored {
                        actor: hero(),
                        target: hero(),
                        amount: 40,
                    },
                ],
            ),
        ];
        for (id, effects) in cases {
            wait_ready(&mut state, hero());
            let events = state.submit_command(hero(), item(id)).unwrap();
            let mut expected = vec![BattleEvent::Action {
                actor: hero(),
                action: ActionRef::Item(id),
            }];
            expected.extend(effects);
            assert_eq!(events, expected, "item {id}");
            assert_eq!(
                state.rng,
                Rng::new(seed),
                "item {id}: a potion draws nothing"
            );
        }
        let me = state.actor(hero()).unwrap();
        assert_eq!((me.hp, me.mp), (2000, 150));
        // The same item can be used again and again: the engine has no inventory.
        for _ in 0..3 {
            wait_ready(&mut state, hero());
            assert!(state.submit_command(hero(), item(0)).is_ok());
        }
    }

    #[test]
    fn a_damage_item_hurts_an_enemy_and_rolls_only_the_variance() {
        let seed = 14;
        let mut state = battle(vec![mage()], vec![dummy()], seed);
        wait_ready(&mut state, hero());
        let events = state
            .submit_command(
                hero(),
                Command::Item {
                    item: 20,
                    target: Target::One(foe()),
                },
            )
            .unwrap();
        let mut replay = Rng::new(seed);
        let variance = replay.range_inclusive(0, 255);
        // Grenade never misses and cannot be a critical hit; its damage is (512 - Def) * 10 / 32 = 160 with variance.
        let amount = 160 * (3841 + u64::from(variance)) / 4096;
        assert_eq!(
            events,
            vec![
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Item(20)
                },
                BattleEvent::Damage {
                    actor: hero(),
                    target: foe(),
                    amount: amount as u32,
                    critical: false
                },
            ]
        );
        assert_eq!(state.rng, replay);
    }

    #[test]
    fn item_commands_are_validated_by_the_item_table_and_the_side_of_the_target() {
        let mut state = battle(vec![mage(), corpse()], vec![dummy()], 4);
        wait_ready(&mut state, hero());
        let item = |item, target: u8| Command::Item {
            item,
            target: Target::One(ActorId(target)),
        };
        let side = |item, target| Command::Item { item, target };
        let cases: Vec<(Command, Result<(), CommandError>)> = vec![
            // Not in the table: Antidote is a status, 100 forbids battle use.
            (item(8, 0), Err(CommandError::UnknownItem(8))),
            (item(100, 0), Err(CommandError::UnknownItem(100))),
            (item(127, 0), Err(CommandError::UnknownItem(127))),
            (item(5000, 0), Err(CommandError::UnknownItem(5000))),
            (item(0, 9), Err(CommandError::UnknownTarget(ActorId(9)))),
            (item(0, 2), Err(CommandError::InvalidTarget(ActorId(2)))), // Potion on an enemy
            (item(20, 0), Err(CommandError::InvalidTarget(hero()))),    // Grenade on an ally
            (item(0, 1), Err(CommandError::TargetDead(ActorId(1)))),    // Potion on a dead ally
            (item(7, 0), Err(CommandError::TargetNotDead(hero()))), // Phoenix Down on a living one
            (item(7, 2), Err(CommandError::InvalidTarget(ActorId(2)))), // ... or an enemy
            (item(0, 0), Ok(())),
            (item(3, 0), Ok(())),
            (item(7, 1), Ok(())),
            (item(20, 2), Ok(())),
            // Megalixir hits the whole party and nothing else: no one target, not the enemies.
            (item(6, 0), Err(CommandError::InvalidReach(Target::One(hero())))),
            (side(6, Target::AllEnemies), Err(CommandError::InvalidReach(Target::AllEnemies))),
            (side(6, Target::AllAllies), Ok(())),
            // A potion, a revival and a grenade hit one target: a whole side is no way to aim them.
            (side(0, Target::AllAllies), Err(CommandError::InvalidReach(Target::AllAllies))),
            (side(7, Target::AllAllies), Err(CommandError::InvalidReach(Target::AllAllies))),
            (side(20, Target::AllEnemies), Err(CommandError::InvalidReach(Target::AllEnemies))),
            (side(20, Target::AllAllies), Err(CommandError::InvalidReach(Target::AllAllies))),
        ];
        let before = state.clone();
        for (command, expected) in &cases {
            assert_eq!(
                state.check_command(hero(), command),
                *expected,
                "{command:?}"
            );
            if let Err(e) = expected {
                assert_eq!(
                    state.submit_command(hero(), *command),
                    Err(*e),
                    "{command:?}"
                );
                assert_eq!(state, before, "{command:?}");
            }
        }
        // What a menu asks of the engine about an item.
        assert_eq!(
            state
                .item_info(0)
                .map(|i| (i.kind, i.targets_allies, i.revives)),
            Some((ItemKind::RestoreHp, true, false))
        );
        assert_eq!(
            state
                .item_info(7)
                .map(|i| (i.kind, i.targets_allies, i.revives)),
            Some((ItemKind::Revive, true, true))
        );
        assert_eq!(
            state.item_info(20).map(|i| (i.kind, i.targets_allies)),
            Some((ItemKind::Damage, false))
        );
        assert_eq!(state.item_info(6).map(|i| (i.kind, i.aim)), Some((ItemKind::RestoreAll, Aim::Side)));
        assert!(state.item_info(8).is_none() && state.items().len() == 9);
        // A battle without an item table refuses every item.
        let mut bare = BattleState::with_config(4, vec![mage()], vec![dummy()], zero_start());
        wait_ready(&mut bare, hero());
        assert_eq!(
            bare.check_command(hero(), &item(0, 0)),
            Err(CommandError::UnknownItem(0))
        );
        assert!(bare.items().is_empty());
    }

    #[test]
    fn defend_halves_physical_damage_until_the_next_turn_and_draws_nothing() {
        // The brute is faster than the hero's second gauge fill (it hits at tick ~541; the hero is ready
        // at 361 and again at 722), so its first blow lands while the hero guards.
        let brute = || {
            let mut c = Combatant::new("Brute", 10, 5000);
            c.attack = 60;
            c.dex = 60;
            c.accuracy = 255;
            c
        };
        let tough = || {
            let mut c = mage();
            c.hp = 9000;
            c.max_hp = 9000;
            c
        };
        let seed = 21;
        let first_blow = |guard: bool| {
            let mut state = battle(vec![tough()], vec![brute()], seed);
            wait_ready(&mut state, hero());
            if guard {
                let events = state.submit_command(hero(), Command::Defend).unwrap();
                assert_eq!(
                    events,
                    vec![BattleEvent::Action {
                        actor: hero(),
                        action: ActionRef::Defend
                    }]
                );
                assert!(state.is_defending(hero()));
                assert_eq!(state.rng, Rng::new(seed), "Defend draws nothing");
                let me = state.actor(hero()).unwrap();
                assert_eq!((me.gauge, me.ready), (0, false), "the turn is spent");
            } else {
                assert!(!state.is_defending(hero()));
            }
            for _ in 0..400 {
                for event in state.step(10) {
                    if let BattleEvent::Damage {
                        actor,
                        target,
                        amount,
                        critical,
                    } = event
                    {
                        assert_eq!((actor, target), (foe(), hero()));
                        return (amount, critical, state);
                    }
                }
            }
            panic!("the brute never hit");
        };
        let (guarded, guarded_crit, state) = first_blow(true);
        let (open, open_crit, _) = first_blow(false);
        assert_eq!(guarded_crit, open_crit, "the same rolls, the same blow");
        assert!(
            open >= 4 && guarded < open,
            "guarded {guarded}, open {open}"
        );
        assert!(
            2 * guarded <= open && open <= 2 * guarded + 3,
            "about half: guarded {guarded}, open {open}"
        );
        // Still guarding at the blow; the guard ends when the hero's gauge is full again (its next turn).
        assert!(state.is_defending(hero()));
        let mut state = state;
        wait_ready(&mut state, hero());
        assert!(!state.is_defending(hero()), "a new turn ends the guard");
        assert!(!state.is_defending(foe()) && !state.is_defending(ActorId(9)));
    }

    #[test]
    fn elements_double_halve_nullify_absorb_heal_or_kill() {
        use crate::scene::AffinityRate as Rate;
        let seed = 62;
        let aff = |element: u16, rate| ElementAffinity {
            element: element.trailing_zeros() as u8,
            rate,
        };
        let elemental_dummy = |hp: u32| {
            let mut d = dummy();
            d.hp = hp;
            d.affinities = vec![
                aff(element::FIRE, Rate::Double),
                aff(element::ICE, Rate::Half),
                aff(element::BOLT, Rate::Nullify),
                aff(element::EARTH, Rate::Absorb),
                aff(element::POISON, Rate::Death),
                aff(element::GRAVITY, Rate::FullHeal),
            ];
            d
        };
        // One spell per element (and one with an element the dummy has no entry for), 0 MP so that the
        // hero can cast any of them.
        let element_spell =
            |index: u16, mask: u16| spell(index, 0x22, 8, 100, 0x0F, mask, 0xFFFF, 0);
        let book = [
            element_spell(10, element::FIRE),
            element_spell(11, element::ICE),
            element_spell(12, element::BOLT),
            element_spell(13, element::EARTH),
            element_spell(14, element::POISON),
            element_spell(15, element::GRAVITY),
            element_spell(16, element::WATER),
            element_spell(17, 0),
        ];
        let mut caster = mage();
        caster.spells = book.to_vec();
        // The draws are the same whatever the target's affinity: the roll to hit, then the variance.
        let mut replay = Rng::new(seed);
        replay.next_u16();
        let variance = replay.range_inclusive(0, 255);
        let normal = action_damage(
            &book[6].spec,
            &caster,
            &elemental_dummy(1000),
            Elemental::Normal,
            Rolls {
                critical: false,
                variance,
            },
        );
        let cast = |spell: u16, hp: u32| {
            let mut state = battle(vec![caster.clone()], vec![elemental_dummy(hp)], seed);
            wait_ready(&mut state, hero());
            let events = state
                .submit_command(
                    hero(),
                    Command::Magic {
                        spell,
                        target: Target::One(foe()),
                    },
                )
                .unwrap();
            assert_eq!(state.rng, replay, "spell {spell}: the same two draws");
            assert_eq!(
                events[0],
                BattleEvent::Action {
                    actor: hero(),
                    action: ActionRef::Magic(spell)
                }
            );
            (
                events[1..].to_vec(),
                state.actor(foe()).unwrap().hp,
                state.outcome(),
            )
        };
        let damage = |amount| BattleEvent::Damage {
            actor: hero(),
            target: foe(),
            amount,
            critical: false,
        };
        // No entry, and no element: the plain number.
        assert_eq!(cast(16, 1000), (vec![damage(normal)], 1000 - normal, None));
        assert_eq!(cast(17, 1000), (vec![damage(normal)], 1000 - normal, None));
        // Fire: weak, so doubled before the variance; Ice: halved.
        let double = action_damage(
            &book[0].spec,
            &caster,
            &elemental_dummy(1000),
            Elemental::Double,
            Rolls {
                critical: false,
                variance,
            },
        );
        let half = action_damage(
            &book[1].spec,
            &caster,
            &elemental_dummy(1000),
            Elemental::Half,
            Rolls {
                critical: false,
                variance,
            },
        );
        assert!(
            double >= 2 * normal - 1
                && double <= 2 * normal + 1
                && half * 2 <= normal + 1
                && half * 2 + 2 >= normal,
            "{normal} {double} {half}"
        );
        assert_eq!(cast(10, 1000), (vec![damage(double)], 1000 - double, None));
        assert_eq!(cast(11, 1000), (vec![damage(half)], 1000 - half, None));
        // Bolt: nullified, a hit of 0 that changes nothing.
        assert_eq!(cast(12, 1000), (vec![damage(0)], 1000, None));
        // Earth: absorbed, so the damage heals (limited to what is missing); at full HP it heals 0.
        let heal = |amount| BattleEvent::Heal {
            actor: hero(),
            target: foe(),
            amount,
        };
        assert_eq!(cast(13, 600), (vec![heal(normal)], 600 + normal, None));
        assert_eq!(cast(13, 1000), (vec![heal(0)], 1000, None));
        // Gravity: full heal.
        assert_eq!(cast(15, 600), (vec![heal(400)], 1000, None));
        // Poison: death, whatever the HP; it ends this battle.
        assert_eq!(
            cast(14, 777),
            (
                vec![
                    damage(777),
                    BattleEvent::Death { actor: foe() },
                    BattleEvent::Victory
                ],
                0,
                Some(Outcome::Victory)
            )
        );
    }

    #[test]
    fn an_enemy_picks_among_the_attacks_it_can_pay_for_and_announces_each() {
        let bite = EnemyAction {
            id: 0x101,
            spec: ActionSpec::basic_attack(16, 255, true),
        };
        let blaze = EnemyAction {
            id: 0x102,
            spec: Spell::from_attack(
                0,
                &attack_record(0x22, 10, 255, 0x0F, element::FIRE, 0xFDFF, 6),
            )
            .unwrap()
            .spec,
        };
        let sorcerer = || {
            let mut c = Combatant::new("Sorcerer", 10, 5000);
            c.attack = 40;
            c.magic = 40;
            c.dex = 60;
            c.mp = 12;
            c.max_mp = 12;
            c.accuracy = 255;
            c.actions = vec![bite, blaze];
            c
        };
        let target = || {
            let mut c = mage();
            c.hp = 1_000_000;
            c.max_hp = 1_000_000;
            c
        };
        for seed in 0..12u64 {
            let mut state = battle(vec![target()], vec![sorcerer()], seed);
            let mut log = Vec::new();
            for _ in 0..40_000 {
                log.extend(state.step(100));
                if log
                    .iter()
                    .filter(|e| matches!(e, BattleEvent::Action { actor, .. } if *actor == foe()))
                    .count()
                    >= 80
                {
                    break;
                }
            }
            let actions: Vec<ActionRef> = log
                .iter()
                .filter_map(|e| match e {
                    BattleEvent::Action { actor, action } if *actor == foe() => Some(*action),
                    _ => None,
                })
                .collect();
            assert_eq!(actions.len(), 80, "seed {seed}");
            assert!(
                actions
                    .iter()
                    .all(|a| matches!(a, ActionRef::EnemyAttack(0x101 | 0x102))),
                "{actions:?}"
            );
            // 12 MP pay for two Blazes and no more: afterwards only the Bite is usable.
            let casts = actions
                .iter()
                .filter(|a| **a == ActionRef::EnemyAttack(0x102))
                .count();
            assert_eq!(casts, 2, "seed {seed}");
            assert_eq!(state.actor(foe()).unwrap().mp, 0);
            let spent: Vec<&BattleEvent> = log
                .iter()
                .filter(|e| matches!(e, BattleEvent::MpSpent { .. }))
                .collect();
            assert_eq!(
                spent,
                vec![
                    &BattleEvent::MpSpent {
                        actor: foe(),
                        amount: 6
                    };
                    2
                ]
            );
            // Each cast is announced, then paid, then lands as magic (a hit or a miss) on the hero.
            for (i, event) in log.iter().enumerate() {
                if *event
                    == (BattleEvent::Action {
                        actor: foe(),
                        action: ActionRef::EnemyAttack(0x102),
                    })
                {
                    assert_eq!(
                        log[i + 1],
                        BattleEvent::MpSpent {
                            actor: foe(),
                            amount: 6
                        }
                    );
                    assert!(
                        matches!(log[i + 2], BattleEvent::Damage { actor, target, critical: false, .. } if actor == foe() && target == hero())
                    );
                }
            }
        }
    }

    #[test]
    fn an_enemy_with_one_usable_attack_draws_exactly_what_a_plain_attacker_draws() {
        // The same numbers as a plain attack of Power 16 / 255%: with a single usable attack there is no
        // pick, so the whole log is the plain one apart from the name of the announced action.
        let build = |actions: Vec<EnemyAction>| {
            let mut c = Combatant::new("Foe", 10, 100_000);
            c.attack = 40;
            c.dex = 60;
            c.accuracy = 255;
            c.attack_power = 16;
            c.actions = actions;
            let mut hero = mage();
            hero.hp = 100_000;
            hero.max_hp = 100_000;
            BattleState::with_config(40, vec![hero], vec![c], zero_start())
        };
        let one = EnemyAction {
            id: 0x201,
            spec: ActionSpec::basic_attack(16, 255, true),
        };
        let run = |actions: Vec<EnemyAction>| {
            let mut state = build(actions);
            let mut log = Vec::new();
            for _ in 0..600 {
                log.extend(state.step(100));
            }
            log
        };
        let with_attack = run(vec![one]);
        let plain = run(vec![]);
        let renamed: Vec<BattleEvent> = with_attack
            .iter()
            .map(|e| match e {
                BattleEvent::Action {
                    actor,
                    action: ActionRef::EnemyAttack(0x201),
                } => BattleEvent::Action {
                    actor: *actor,
                    action: ActionRef::Attack,
                },
                other => *other,
            })
            .collect();
        assert!(
            renamed.len() > 10
                && renamed
                    .iter()
                    .any(|e| matches!(e, BattleEvent::Damage { .. }))
        );
        assert_eq!(renamed, plain);
        assert!(with_attack.iter().any(|e| matches!(
            e,
            BattleEvent::Action {
                action: ActionRef::EnemyAttack(0x201),
                ..
            }
        )));
        // An attack the enemy cannot pay for leaves it with the plain attack too.
        let pricey = EnemyAction {
            id: 0x202,
            spec: ActionSpec {
                mp_cost: 5,
                ..one.spec
            },
        };
        assert_eq!(run(vec![pricey]), plain);
    }

    #[test]
    fn enemies_know_their_scene_attacks_and_their_affinities() {
        let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
        let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
        let (grunt, hound) = (&enemies[0], &enemies[1]);
        // The grunt lists Test Fire (a magical attack, Power 30) and Test Bite (physical, Power 20), in
        // that order; the hound has one claw. Magic ignores the row, a physical attack respects it.
        assert_eq!(
            grunt.actions.iter().map(|a| a.id).collect::<Vec<_>>(),
            [0x0302, 0x0301]
        );
        assert_eq!(
            (grunt.actions[0].spec.kind, grunt.actions[0].spec.power),
            (ActionKind::Magical, 30)
        );
        assert_eq!(
            (grunt.actions[1].spec.kind, grunt.actions[1].spec.power),
            (ActionKind::Physical, 20)
        );
        assert!(grunt.actions[1].spec.short_range && !grunt.actions[0].spec.short_range);
        assert_eq!(
            hound.actions.iter().map(|a| a.id).collect::<Vec<_>>(),
            [0x0303]
        );
        // The plain attack still comes from the first plain physical attack (Test Bite), as before.
        assert_eq!((grunt.accuracy, grunt.attack_power), (95, 20));
        // Stats the magic formulas use, and the affinities (Fire halved, Holy absorbed) from the record.
        assert_eq!((grunt.magic, grunt.magic_defense), (4, 9));
        assert_eq!(
            grunt.affinities,
            vec![
                ElementAffinity {
                    element: 0,
                    rate: AffinityRate::Half
                },
                ElementAffinity {
                    element: 8,
                    rate: AffinityRate::Absorb
                },
            ]
        );
        assert_eq!(grunt.affinity_for(element::FIRE), Some(AffinityRate::Half));
        assert_eq!(
            grunt.affinity_for(element::HOLY),
            Some(AffinityRate::Absorb)
        );
        assert_eq!(grunt.affinity_for(element::ICE), None);
        assert!(hound.affinities.is_empty() && hound.spells.is_empty());
        // The enemy built the old way (no scene) has no attack list: a plain attacker.
        let rec = bin.scene(0).unwrap().enemies[0].as_ref().unwrap();
        assert!(Combatant::from_enemy(rec, None).actions.is_empty());
    }

    #[test]
    fn characters_know_the_spells_of_their_materia_plus_the_starter_grimoire() {
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let ids = |who| {
            party_spells(&kernel, who)
                .iter()
                .map(|s| s.index)
                .collect::<Vec<_>>()
        };
        // The starter grimoire (Restore, Fire, Ice, Lightning at level 1): Cure, Fire, Ice, Bolt; nobody
        // is without. Cloud's own Lightning and Ice and Red XIII's Fire add nothing to it.
        for who in [
            CharacterId::Barret,
            CharacterId::Tifa,
            CharacterId::Aeris,
            CharacterId::Cloud,
            CharacterId::RedXiii,
            CharacterId::CaitSith,
            CharacterId::Vincent,
        ] {
            assert_eq!(ids(who), [0, 27, 30, 33], "{who:?}");
        }
        // A Restore materia at 2500 AP is level 2 (Cure2 too); Master Magic teaches everything the rules
        // can cast: the Cure family, Life, the Fire, Ice and Bolt spells that exist, Demi and Ultima (it hits
        // a whole side, which a command can ask for now). Not the record that is not magic, not what has no
        // record.
        assert_eq!(ids(CharacterId::Yuffie), [0, 1, 27, 30, 33]);
        assert_eq!(
            ids(CharacterId::Cid),
            [0, 1, 2, 7, 27, 28, 29, 30, 31, 32, 33, 34, 42, 52]
        );
        // What a menu is told: the cost, and the side.
        let cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
        let info: Vec<SpellInfo> = cloud.spells.iter().map(Spell::info).collect();
        assert_eq!(
            info,
            vec![
                SpellInfo {
                    spell: 0,
                    mp_cost: 5,
                    targets_allies: true,
                    aim: Aim::OneOrSide
                },
                SpellInfo {
                    spell: 27,
                    mp_cost: 4,
                    targets_allies: false,
                    aim: Aim::OneOrSide
                },
                SpellInfo {
                    spell: 30,
                    mp_cost: 4,
                    targets_allies: false,
                    aim: Aim::OneOrSide
                },
                SpellInfo {
                    spell: 33,
                    mp_cost: 4,
                    targets_allies: false,
                    aim: Aim::OneOrSide
                },
            ]
        );
        // Magic stat, magic defense (Spirit 16 + the armor's 2) and magic evade come from the kernel.
        assert_eq!(
            (cloud.magic, cloud.magic_defense, cloud.magic_evade),
            (16, 18, 0)
        );
        // A kernel without the starter materia's records gives only what the character equips.
        let mut sections = crate::testutil::synthetic_kernel_sections();
        sections[8].1 = vec![0xFF; MATERIA_COUNT_FOR_TESTS * 20];
        let bare = Kernel::parse(&crate::testutil::pack_kernel(&sections, &[])).unwrap();
        assert!(party_spells(&bare, CharacterId::Barret).is_empty());
    }

    const MATERIA_COUNT_FOR_TESTS: usize = crate::kernel::MATERIA_COUNT;

    #[test]
    fn spells_for_lists_what_the_actor_can_cast_for_a_menu() {
        let mut state = battle(vec![mage(), corpse()], vec![dummy()], 1);
        assert_eq!(
            state.spells_for(hero()),
            vec![
                SpellInfo {
                    spell: 0,
                    mp_cost: 5,
                    targets_allies: true,
                    aim: Aim::OneOrSide
                },
                SpellInfo {
                    spell: 27,
                    mp_cost: 4,
                    targets_allies: false,
                    aim: Aim::OneOrSide
                },
                SpellInfo {
                    spell: 7,
                    mp_cost: 34,
                    targets_allies: true,
                    aim: Aim::OneOrSide
                },
            ]
        );
        assert!(
            state.spells_for(ActorId(1)).is_empty(),
            "this party member knows none"
        );
        assert!(
            state.spells_for(ActorId(2)).is_empty(),
            "enemies cast nothing from a menu"
        );
        assert!(state.spells_for(ActorId(77)).is_empty());
        // The list does not depend on the MP at the moment (the menu greys an unaffordable spell out).
        wait_ready(&mut state, hero());
        let before = state.spells_for(hero());
        state
            .submit_command(
                hero(),
                Command::Magic {
                    spell: 7,
                    target: Target::One(ActorId(1)),
                },
            )
            .unwrap();
        assert_eq!(state.actor(hero()).unwrap().mp, 16);
        assert_eq!(state.spells_for(hero()), before);
    }

    #[test]
    fn construction_keeps_mp_inside_the_maximum() {
        let mut c = fighter("Over", 5, 50, 5, 5, 5);
        c.mp = 99;
        c.max_mp = 10;
        let state = BattleState::new(1, vec![c], vec![fighter("E", 5, 50, 5, 5, 5)]);
        assert_eq!(state.actor(hero()).unwrap().mp, 10);
    }

    /// A deterministic mixed policy: whatever the actor can do, chosen by the tick and the actor.
    fn mixed_policy(state: &BattleState, actor: ActorId) -> Command {
        let enemy = state
            .enemy_ids()
            .into_iter()
            .find(|e| state.actor(*e).is_some_and(Combatant::is_alive));
        let dead = state
            .party_ids()
            .into_iter()
            .find(|a| state.actor(*a).is_some_and(|c| !c.is_alive()));
        let attack = enemy.map(|target| Command::Attack { target });
        let candidates = [
            attack,
            enemy.map(|target| Command::Magic { spell: 27, target: Target::One(target) }),
            Some(Command::Magic {
                spell: 0,
                target: Target::One(actor),
            }),
            Some(Command::Defend),
            Some(Command::Item {
                item: 0,
                target: Target::One(actor),
            }),
            dead.map(|target| Command::Item { item: 7, target: Target::One(target) }),
            dead.map(|target| Command::Magic { spell: 7, target: Target::One(target) }),
            Some(Command::Item {
                item: 3,
                target: Target::One(actor),
            }),
        ];
        let pick = (state.tick() / 13 + u64::from(actor.0)) as usize % candidates.len();
        candidates[pick]
            .filter(|c| state.check_command(actor, c).is_ok())
            .or(attack)
            .unwrap_or(Command::Defend)
    }

    fn assert_sane(state: &BattleState) {
        for (i, c) in state.actors().iter().enumerate() {
            assert!(c.hp <= c.max_hp, "actor {i}: hp {} over {}", c.hp, c.max_hp);
            assert!(c.mp <= c.max_mp, "actor {i}: mp {} over {}", c.mp, c.max_mp);
            assert!(c.gauge <= GAUGE_MAX);
            assert!(!c.ready || c.is_alive(), "actor {i} is ready but dead");
            assert!(
                !c.defending || c.is_alive() || c.side == Side::Enemy,
                "actor {i} guards while dead"
            );
        }
    }

    fn mixed_battle(seed: u64) -> BattleState {
        let knight = || {
            let mut c = Combatant::new("Knight", 12, 900);
            c.attack = 60;
            c.defense = 20;
            c.magic = 12;
            c.magic_defense = 10;
            c.dex = 50;
            c.mp = 30;
            c.max_mp = 30;
            c.spells = vec![cure(), fire(), life()];
            c
        };
        let goblin = |name: &str| {
            let mut c = Combatant::new(name, 8, 700);
            c.attack = 45;
            c.magic = 30;
            c.defense = 12;
            c.magic_defense = 10;
            c.dex = 45;
            c.accuracy = 120;
            c.evade = 40; // the heroes miss now and then
            c.mp = 24;
            c.max_mp = 24;
            c.reward = Reward {
                exp: 20,
                gil: 30,
                ap: 2,
            };
            c.actions = vec![
                EnemyAction {
                    id: 1,
                    spec: ActionSpec::basic_attack(16, 120, true),
                },
                EnemyAction {
                    id: 2,
                    spec: Spell::from_attack(
                        0,
                        &attack_record(0x22, 12, 100, 0x0F, element::BOLT, 0xFDFF, 8),
                    )
                    .unwrap()
                    .spec,
                },
            ];
            c
        };
        // Casters that have an All materia, so that the fuzz below also reaches the whole-side paths.
        let mut mage = mage();
        mage.all_left = 3;
        let mut knight = knight();
        knight.all_left = 1;
        BattleState::with_config(
            seed,
            vec![mage, knight, corpse()],
            vec![goblin("Goblin A"), goblin("Goblin B")],
            zero_start(),
        )
        .with_items(item_table())
    }

    /// Play a mixed battle: 100 ms slices, every ready hero follows the policy.
    fn play_mixed(state: &mut BattleState, slice_ms: u32) -> Vec<BattleEvent> {
        let mut log = Vec::new();
        for _ in 0..200_000 {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(slice_ms));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                let command = mixed_policy(state, id);
                match state.submit_command(id, command) {
                    Ok(events) => log.extend(events),
                    Err(e) => {
                        panic!("the policy chose a command the engine refuses: {command:?}: {e}")
                    }
                }
                assert_sane(state);
            }
        }
        log
    }

    #[test]
    fn a_battle_of_mixed_commands_is_deterministic_ends_once_and_stays_sane() {
        let mut outcomes = [0u32; 2];
        let mut kinds = std::collections::HashSet::new();
        for seed in 0..24u64 {
            let (mut a, mut b) = (mixed_battle(seed), mixed_battle(seed));
            let (log_a, log_b) = (play_mixed(&mut a, 100), play_mixed(&mut b, 100));
            assert_eq!(log_a, log_b, "seed {seed}");
            assert_eq!(a, b);
            let outcome = a
                .outcome()
                .unwrap_or_else(|| panic!("seed {seed} never ended"));
            outcomes[usize::from(outcome == Outcome::Defeat)] += 1;
            let ends: Vec<&BattleEvent> = log_a
                .iter()
                .filter(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat))
                .collect();
            assert_eq!(ends.len(), 1, "seed {seed}");
            assert_eq!(log_a.last(), ends.first().copied());
            for e in &log_a {
                kinds.insert(std::mem::discriminant(e));
            }
            // Every command shows up as an announced action followed by its effects.
            let announced = log_a
                .iter()
                .filter(|e| matches!(e, BattleEvent::Action { .. }))
                .count();
            assert!(announced > 5, "seed {seed}");
        }
        // Over the seeds every kind of event but a defeat happened (this party is strong), and every
        // battle ended one way or the other.
        let (a, t) = (ActorId(0), ActorId(1));
        let every_kind = [
            BattleEvent::GaugeReady { actor: a },
            BattleEvent::Damage {
                actor: a,
                target: t,
                amount: 1,
                critical: false,
            },
            BattleEvent::Miss {
                actor: a,
                target: t,
            },
            BattleEvent::Death { actor: a },
            BattleEvent::Victory,
            BattleEvent::Action {
                actor: a,
                action: ActionRef::Defend,
            },
            BattleEvent::Heal {
                actor: a,
                target: t,
                amount: 1,
            },
            BattleEvent::MpSpent {
                actor: a,
                amount: 1,
            },
            BattleEvent::MpRestored {
                actor: a,
                target: t,
                amount: 1,
            },
            BattleEvent::Revive {
                actor: a,
                target: t,
                hp: 1,
            },
        ];
        for e in every_kind {
            assert!(
                kinds.contains(&std::mem::discriminant(&e)),
                "no {e:?} in 24 battles"
            );
        }
        assert_eq!(outcomes.iter().sum::<u32>(), 24);
        assert!(outcomes[0] > 0, "the party wins some");
    }

    #[test]
    fn the_slicing_of_the_clock_does_not_change_a_mixed_battle() {
        // The policy looks at the tick, so the commands only agree when they land on the same ticks:
        // slices of whole ticks (60 ticks per second: 1000 ms is 60 ticks) are the same battle as long
        // as the commands are given at the same ticks. Using the step that never overshoots a tick
        // boundary keeps that true: 1000 ms and 100 ms slices both end on tick boundaries every 1000 ms.
        let mut coarse = mixed_battle(5);
        let mut fine = mixed_battle(5);
        let mut log_coarse = Vec::new();
        let mut log_fine = Vec::new();
        for _ in 0..4000 {
            if coarse.outcome().is_some() {
                break;
            }
            log_coarse.extend(coarse.step(1000));
            for _ in 0..10 {
                log_fine.extend(fine.step(100));
            }
            assert_eq!(coarse.tick(), fine.tick());
            // Both are ready at the same time only if no command is pending: give both the same commands
            // at the end of the second.
            for id in coarse.ready_actors() {
                let command = mixed_policy(&coarse, id);
                log_coarse.extend(coarse.submit_command(id, command).unwrap());
                log_fine.extend(fine.submit_command(id, command).unwrap());
            }
        }
        assert_eq!(coarse, fine);
        assert_eq!(log_coarse, log_fine);
    }

    /// A small deterministic generator for the fuzz tests.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % bound
        }
    }

    #[test]
    fn random_commands_never_panic_and_check_command_always_agrees_with_submit() {
        let mut rnd = Xorshift(0x2545_F491_4F6C_DD1D);
        let (mut accepted, mut whole_sides) = (0, 0);
        for seed in 0..40u64 {
            let mut state = mixed_battle(seed);
            for _ in 0..600 {
                if state.outcome().is_some() {
                    break;
                }
                let _ = state.step(rnd.next(900) as u32);
                for _ in 0..4 {
                    let actor = ActorId(rnd.next(8) as u8);
                    let target = ActorId(rnd.next(9) as u8);
                    // One actor, or either whole side (a third of the time: the paths a whole side takes
                    // must agree between check and submit like the others).
                    let aim = match rnd.next(6) {
                        0 | 1 => Target::AllEnemies,
                        2 => Target::AllAllies,
                        _ => Target::One(target),
                    };
                    // Mostly spells and items the heroes have (Cure, Life, Fire; Potion, Ether, Megalixir, Phoenix
                    // Down, Grenade), now and then anything at all.
                    let known_spell = [0u16, 7, 27][rnd.next(3) as usize];
                    let known_item = [0u16, 3, 6, 7, 20][rnd.next(5) as usize];
                    let command = match rnd.next(4) {
                        0 => Command::Attack { target },
                        1 => Command::Magic {
                            spell: if rnd.next(4) == 0 { rnd.next(60) as u16 } else { known_spell },
                            target: aim,
                        },
                        2 => Command::Item {
                            item: if rnd.next(4) == 0 { rnd.next(140) as u16 } else { known_item },
                            target: aim,
                        },
                        _ => Command::Defend,
                    };
                    let checked = state.check_command(actor, &command);
                    let before = state.clone();
                    let submitted = state.submit_command(actor, command);
                    assert_eq!(
                        checked,
                        submitted.as_ref().map(|_| ()).map_err(|e| *e),
                        "{command:?} by {actor:?}"
                    );
                    if submitted.is_err() {
                        assert_eq!(
                            state, before,
                            "a refused command changes nothing: {command:?}"
                        );
                    } else {
                        accepted += 1;
                        if matches!(command, Command::Magic { target, .. } | Command::Item { target, .. } if target.is_side()) {
                            whole_sides += 1;
                        }
                    }
                    assert_sane(&state);
                }
            }
        }
        assert!(accepted > 200, "only {accepted} random commands were valid");
        assert!(whole_sides > 20, "only {whole_sides} whole-side commands were valid");
    }

    // --- serde ---

    #[test]
    fn events_and_commands_roundtrip_through_json() {
        let events = vec![
            BattleEvent::GaugeReady { actor: ActorId(1) },
            BattleEvent::Damage {
                actor: ActorId(0),
                target: ActorId(3),
                amount: 42,
                critical: true,
            },
            BattleEvent::Miss {
                actor: ActorId(2),
                target: ActorId(0),
            },
            BattleEvent::Death { actor: ActorId(3) },
            BattleEvent::Victory,
            BattleEvent::Defeat,
            BattleEvent::Action {
                actor: ActorId(0),
                action: ActionRef::Attack,
            },
            BattleEvent::Action {
                actor: ActorId(0),
                action: ActionRef::Magic(27),
            },
            BattleEvent::Action {
                actor: ActorId(0),
                action: ActionRef::Item(7),
            },
            BattleEvent::Action {
                actor: ActorId(0),
                action: ActionRef::Defend,
            },
            BattleEvent::Action {
                actor: ActorId(3),
                action: ActionRef::EnemyAttack(0x0302),
            },
            BattleEvent::Heal {
                actor: ActorId(0),
                target: ActorId(1),
                amount: 271,
            },
            BattleEvent::MpSpent {
                actor: ActorId(0),
                amount: 5,
            },
            BattleEvent::MpRestored {
                actor: ActorId(0),
                target: ActorId(0),
                amount: 100,
            },
            BattleEvent::Revive {
                actor: ActorId(0),
                target: ActorId(2),
                hp: 100,
            },
        ];
        let json = serde_json::to_string(&events).unwrap();
        let back: Vec<BattleEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, events);

        for cmd in [
            Command::Attack { target: ActorId(4) },
            Command::Magic {
                spell: 33,
                target: Target::One(ActorId(2)),
            },
            Command::Item {
                item: 3,
                target: Target::One(ActorId(1)),
            },
            Command::Magic { spell: 27, target: Target::AllEnemies },
            Command::Item { item: 6, target: Target::AllAllies },
            Command::Limit { target: Target::AllAllies },
            Command::Defend,
        ] {
            let back: Command =
                serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
            assert_eq!(back, cmd);
        }
        let out: Outcome =
            serde_json::from_str(&serde_json::to_string(&Outcome::Victory).unwrap()).unwrap();
        assert_eq!(out, Outcome::Victory);
    }
}

#[cfg(test)]
mod side_tests;

#[cfg(test)]
mod status_tests;
