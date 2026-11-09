//! FF7 battle data decoding and a deterministic battle rules engine (milestone M2-c).
//!
//! This crate turns the owner's own FF7 PC data files into battle inputs and runs a
//! server-authoritative battle on them. It never ships, copies or transmits game data:
//! every process (server and each client) reads its own local files, and only compact
//! events and commands ([`rules::BattleEvent`], [`rules::Command`]) are meant for the wire.
//!
//! | module | purpose |
//! |---|---|
//! | [`scene`] | `scene.bin`: 256 scenes with formations, enemy stat records, attacks |
//! | [`kernel`] | `kernel.bin` (commands, attacks, growth curves, starting data and stock, items, materia, weapons, armor) and `kernel2.bin` text |
//! | [`action`] | what an action is: the damage-calculation byte, formulas, spells, enemy attacks, elements, the item table |
//! | [`limit`] | the limit gauge and each character's first limit break (Braver, Big Shot, ...) |
//! | [`rules`] | the deterministic battle engine: ATB gauges, the commands (Attack, Magic, Item, Defend, Limit, Escape), enemy turns |
//! | [`ai`] | the enemies' real `scene.bin` AI scripts: decoder and interpreter, with the plain behaviour as the fallback |
//! | [`rng`] | the seeded PCG32 generator behind every roll |
//! | [`text`] | FF7 text encoding |
//!
//! # Safety and bounds
//!
//! All parsers take untrusted bytes and never panic: field reads are total, every gzip
//! stream and the LZSS decoder are read through hard output caps, and counts (scenes,
//! kernel sections) are bounded. Corrupt input yields an `Err`.
//!
//! # Determinism
//!
//! Given the same files, seed, party, enemies, configuration and the same ordered inputs
//! (`step` / `submit_command` calls), [`rules::BattleState`] produces identical events on every
//! platform. Combatants are pure functions of the file data and a character level
//! ([`rules::Combatant::from_enemy_in_scene`], [`rules::Combatant::from_character`]: stats, spells,
//! the enemies' own attacks and affinities), and the item table of the kernel
//! ([`action::ItemTable::from_kernel`]), so server and clients that own the same edition derive the same
//! numbers. The wire only needs the formation id, the seed, and (character, level, equipment) per party
//! member. **An inventory is not engine state**: the engine accepts any item of its table, and whether
//! the account owns one is for the server to check.
//!
//! # Example: a battle with hand-made combatants
//!
//! ```
//! use fmv_ff7_battle::rules::*;
//!
//! let mut hero = Combatant::new("Hero", 10, 300);
//! hero.attack = 50;
//! hero.defense = 20;
//! hero.dex = 40;
//! let mut foe = Combatant::new("Foe", 8, 120);
//! foe.attack = 25;
//! foe.defense = 10;
//! foe.dex = 45;
//!
//! let config = BattleConfig { start: StartMode::Zero, ..BattleConfig::default() };
//! let mut battle = BattleState::with_config(7, vec![hero], vec![foe], config);
//! let mut log = Vec::new();
//! while battle.outcome().is_none() {
//!     log.extend(battle.step(100));
//!     for id in battle.ready_actors() {
//!         let target = battle.enemy_ids()[0];
//!         if let Ok(events) = battle.submit_command(id, Command::Attack { target }) {
//!             log.extend(events);
//!         }
//!     }
//! }
//! assert_eq!(battle.outcome(), Some(Outcome::Victory));
//! assert!(log.contains(&BattleEvent::Victory));
//! ```
//!
//! # Example: building a shared battle from the owner's files
//!
//! ```no_run
//! use fmv_ff7_battle::kernel::{CharacterId, Kernel};
//! use fmv_ff7_battle::rules::*;
//! use fmv_ff7_battle::scene::SceneBin;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let scenes = SceneBin::open("data/battle/scene.bin")?;
//! let kernel = Kernel::open("data/kernel/kernel.bin")?;
//!
//! // Formation id (scene * 4 + slot) from the field's encounter table, and the players'
//! // levels from their own progression; both are placeholders here.
//! let (formation_id, level) = (0usize, 5u8);
//! let formation = scenes.formation(formation_id).ok_or("no such formation")?;
//! let enemies = enemies_for_formation(formation.scene, formation.slot);
//! let party = vec![
//!     Combatant::from_character(&kernel, CharacterId::Cloud, level, None, None).ok_or("no Cloud")?,
//!     Combatant::from_character(&kernel, CharacterId::Barret, level, None, None).ok_or("no Barret")?,
//! ];
//! let config = BattleConfig {
//!     start: StartMode::from_layout(formation.setup().layout),
//!     ..BattleConfig::default()
//! };
//! let mut battle = BattleState::with_config(0xC0FFEE, party, enemies, config);
//! let events = battle.step(33);
//! # let _ = events;
//! # Ok(())
//! # }
//! ```
//!
//! # Sources
//!
//! Each module lists the public format documentation it follows (Qhimm / Final Fantasy
//! Inside wiki pages and the FF7 Project's battle-timer notes) next to the code that uses it,
//! and states which values were checked against real files and which are APPROXIMATIONs.

#![forbid(unsafe_code)]

pub mod action;
pub mod ai;
mod bytes;
pub mod kernel;
pub mod limit;
pub mod rng;
pub mod rules;
pub mod scene;
pub mod text;

#[cfg(test)]
mod limit_tests;
#[cfg(test)]
mod testutil;
