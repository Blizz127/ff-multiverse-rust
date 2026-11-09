//! The battle as the scripts see it: actor slots, the masks, and the fields of each actor.
//!
//! # Actor slots
//!
//! A script addresses actors by slot: the player characters take slots 0..=2, slot 3 is the formation's
//! own actor (not modelled here) and the enemies take slots 4..=9
//! (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes/Battle_AI_Addresses>). The original has three
//! player characters; a battle of this engine (protocol v9: the canon companions, then every player's own
//! character) has up to [`MAX_PARTY`] of them. **The first three party members take slots 0..=2 as in the
//! game, the others take the unused slots from [`FIRST_EXTRA_PARTY_SLOT`] (10, 11, ...)**, which are free bits
//! of the 16-bit masks, so that a script's "enemies", "players" and "everyone" masks, and a random pick
//! among them, see the whole party. (The original never has more than three, so no script names slots 10 and
//! up on purpose; a script that spells out the slots 0..=2 by number does not reach the extra ones.) A battle
//! of the engine maps onto the slots only when it fits: at most [`MAX_PARTY`] party members and
//! [`MAX_ENEMIES`] enemies ([`Slots::new`]); a bigger battle has no AI, its enemies keep the plain behaviour.
//!
//! # Masks (APPROXIMATIONs to verify against the original engine)
//!
//! Only **living** actors are in the "allies", "enemies" and "active" masks, so a script never aims at the
//! dead; "allies" include the actor itself; "all" (`0x20E0`) lists every actor, dead or not.
//!
//! # Fields
//!
//! A field is read from a byte image of the actor (address `0x4000 + 8 * byte`), built from its
//! [`Combatant`]. Only what the engine models is known; anything else makes the run fail, not guess:
//!
//! | address | field | note |
//! |---|---|---|
//! | `0x4000` (dword) | statuses | only Death (bit 0) can be set: the engine has no statuses |
//! | `0x4025`, `0x4026`, `0x402D` (bits) | defending, back row, dead | |
//! | `0x4040` | actor index | the slot |
//! | `0x4048` | level | |
//! | `0x4060` | character id + `0x10` for a party member (by its name), the instance for an enemy | a party member that is not one of the nine characters has none |
//! | `0x40A0`, `0x40A8` | Dexterity, Luck | |
//! | `0x4100`, `0x4110` (word) | physical and magical defense | a script can write them ([`super::vm::Effect`]) |
//! | `0x4140`, `0x4150` (word), `0x4160`, `0x4180` (dword) | MP, max MP, HP, max HP | |

use crate::kernel::CharacterId;
use crate::rules::{BattleState, Combatant, Row};

use super::vm::{Image, Masks, World, FIRST_FIELD};
use super::AiAttack;

/// Most party members a battle can have for its enemies to run scripts: the protocol's largest party
/// (`MAX_BATTLE_PARTY`, 8). The three the original has take slots 0..=2, the rest [`FIRST_EXTRA_PARTY_SLOT`]
/// and up (bits 10 to 14 of a mask).
pub const MAX_PARTY: usize = 8;
/// Party members that have the slots the original gives its player characters (0..=2).
pub const CLASSIC_PARTY: usize = 3;
/// Most enemies a battle can have for them to run scripts.
pub const MAX_ENEMIES: usize = 6;
/// Slot of the first enemy.
pub const FIRST_ENEMY_SLOT: u8 = 4;
/// Slot of the fourth party member (the first the original has no slot for; the slots after it follow).
pub const FIRST_EXTRA_PARTY_SLOT: u8 = 10;

/// How the actors of a battle map to script slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    party: usize,
    enemies: usize,
}

impl Slots {
    /// The mapping for a battle of `party` party members and `enemies` enemies, if it fits the slots.
    pub fn new(party: usize, enemies: usize) -> Option<Self> {
        (party <= MAX_PARTY && enemies <= MAX_ENEMIES).then_some(Self { party, enemies })
    }

    /// The slot of the actor at position `idx` of the battle (party first, then enemies): party members
    /// 0..=2 are slots 0..=2, further ones are slots 10, 11, ..., and enemy `j` is slot `4 + j`.
    pub fn slot_of(&self, idx: usize) -> Option<u8> {
        if idx < self.party {
            if idx < CLASSIC_PARTY {
                u8::try_from(idx).ok()
            } else {
                u8::try_from(idx - CLASSIC_PARTY).ok().map(|j| FIRST_EXTRA_PARTY_SLOT + j)
            }
        } else if idx - self.party < self.enemies {
            u8::try_from(idx - self.party).ok().map(|j| FIRST_ENEMY_SLOT + j)
        } else {
            None
        }
    }

    /// The position in the battle of the actor in `slot`.
    pub fn actor_of(&self, slot: u8) -> Option<usize> {
        let slot = usize::from(slot);
        let first = usize::from(FIRST_ENEMY_SLOT);
        let extra = usize::from(FIRST_EXTRA_PARTY_SLOT);
        if slot < self.party.min(CLASSIC_PARTY) {
            Some(slot)
        } else if (first..first + self.enemies).contains(&slot) {
            Some(self.party + slot - first)
        } else if self.party > CLASSIC_PARTY && (extra..extra + self.party - CLASSIC_PARTY).contains(&slot) {
            Some(CLASSIC_PARTY + slot - extra)
        } else {
            None
        }
    }

    pub fn party(&self) -> usize {
        self.party
    }

    pub fn enemies(&self) -> usize {
        self.enemies
    }
}

/// The id a script sees for a party member: its character id + `0x10`, by name.
fn character_id(name: &str) -> Option<u8> {
    CharacterId::ALL
        .iter()
        .find(|c| c.name() == name)
        .and_then(|c| u8::try_from(c.index()).ok())
        .map(|i| i + 0x10)
}

/// The values behind an actor's field image.
#[derive(Debug, Clone, Copy, Default)]
struct Fields {
    dead: bool,
    defending: bool,
    back_row: bool,
    slot: u8,
    level: u8,
    /// `None`: the actor has no id a script could read.
    id: Option<u8>,
    dex: u8,
    luck: u8,
    defense: u16,
    magic_defense: u16,
    mp: u16,
    max_mp: u16,
    hp: u32,
    max_hp: u32,
}

fn clamp16(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

fn clamp8(v: u32) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

impl Fields {
    fn of(c: &Combatant, slot: u8, id: Option<u8>) -> Self {
        Self {
            dead: !c.is_alive(),
            defending: c.defending,
            back_row: c.row == Row::Back,
            slot,
            level: c.level,
            id,
            dex: clamp8(c.dex),
            luck: clamp8(c.luck),
            defense: clamp16(c.defense),
            magic_defense: clamp16(c.magic_defense),
            mp: clamp16(c.mp),
            max_mp: clamp16(c.max_mp),
            hp: c.hp,
            max_hp: c.max_hp,
        }
    }

    /// The image over the addresses from `0x4000` (the same shape whatever the values: what is known
    /// does not depend on them, except the id of an actor that has none).
    fn image(&self) -> Image {
        let mut img = Image::new(0x40);
        img.put_le(0x00, 4, u32::from(self.dead));
        img.put_bits(0x04, 1 << 5, u8::from(self.defending) << 5);
        img.put_bits(0x04, 1 << 6, u8::from(self.back_row) << 6);
        img.put_bits(0x05, 1 << 5, u8::from(self.dead) << 5);
        img.put_le(0x08, 1, u32::from(self.slot));
        img.put_le(0x09, 1, u32::from(self.level));
        if let Some(id) = self.id {
            img.put_le(0x0C, 1, u32::from(id));
        }
        img.put_le(0x14, 1, u32::from(self.dex));
        img.put_le(0x15, 1, u32::from(self.luck));
        img.put_le(0x20, 2, u32::from(self.defense));
        img.put_le(0x22, 2, u32::from(self.magic_defense));
        img.put_le(0x28, 2, u32::from(self.mp));
        img.put_le(0x2A, 2, u32::from(self.max_mp));
        img.put_le(0x2C, 4, self.hp);
        img.put_le(0x30, 4, self.max_hp);
        img
    }
}

/// Whether the battle models the actor field of `size` at `addr` (for any actor that has an id).
pub fn field_known(addr: u16, size: u8) -> bool {
    let Some(rel) = addr.checked_sub(FIRST_FIELD) else {
        return false;
    };
    Fields {
        id: Some(0),
        ..Fields::default()
    }
    .image()
    .read(usize::from(rel), size)
    .is_some()
}

/// A running battle seen by the scripts of one of its enemies.
pub struct BattleWorld<'a> {
    state: &'a BattleState,
    slots: Slots,
    me_idx: usize,
    me: u8,
    attacks: &'a [AiAttack],
}

impl<'a> BattleWorld<'a> {
    /// The view for the enemy at position `me_idx`; `None` if the battle does not fit the script slots.
    pub fn new(state: &'a BattleState, me_idx: usize, attacks: &'a [AiAttack]) -> Option<Self> {
        let party = state.party_ids().len();
        let enemies = state.actors().len().checked_sub(party)?;
        let slots = Slots::new(party, enemies)?;
        let me = slots.slot_of(me_idx)?;
        Some(Self {
            state,
            slots,
            me_idx,
            me,
            attacks,
        })
    }

    pub fn slots(&self) -> &Slots {
        &self.slots
    }

    pub fn me_idx(&self) -> usize {
        self.me_idx
    }
}

impl World for BattleWorld<'_> {
    fn me(&self) -> u8 {
        self.me
    }

    fn masks(&self) -> Masks {
        let party = self.slots.party();
        let mine_is_party = self.me_idx < party;
        let (mut all, mut alive, mut players, mut allies, mut enemies) = (0u16, 0u16, 0u16, 0u16, 0u16);
        for (idx, c) in self.state.actors().iter().enumerate() {
            let Some(slot) = self.slots.slot_of(idx) else {
                continue;
            };
            let bit = 1u16 << slot;
            all |= bit;
            if !c.is_alive() {
                continue;
            }
            alive |= bit;
            let is_party = idx < party;
            if is_party {
                players |= bit;
            }
            if is_party == mine_is_party {
                allies |= bit;
            } else {
                enemies |= bit;
            }
        }
        Masks {
            active: alive,
            me: 1u16 << self.me,
            allies,
            active_allies: allies,
            enemies,
            active_enemies: enemies,
            players,
            all,
        }
    }

    fn actor_field(&self, slot: u8, addr: u16, size: u8) -> Option<u32> {
        let idx = self.slots.actor_of(slot)?;
        let c = self.state.actors().get(idx)?;
        let id = if idx < self.slots.party() {
            character_id(&c.name)
        } else {
            u8::try_from(idx - self.slots.party()).ok()
        };
        let rel = usize::from(addr.checked_sub(FIRST_FIELD)?);
        Fields::of(c, slot, id).image().read(rel, size)
    }

    fn attack_mp_cost(&self, attack: u16) -> Option<u16> {
        self.attacks
            .iter()
            .find(|a| a.id == attack)
            .map(|a| a.mp_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{BattleConfig, StartMode};

    #[test]
    fn slots_map_party_first_then_enemies_from_slot_four() {
        let s = Slots::new(2, 3).unwrap();
        let got: Vec<Option<u8>> = (0..6).map(|i| s.slot_of(i)).collect();
        assert_eq!(got, vec![Some(0), Some(1), Some(4), Some(5), Some(6), None]);
        for (idx, slot) in [(0usize, 0u8), (1, 1), (2, 4), (3, 5), (4, 6)] {
            assert_eq!(s.actor_of(slot), Some(idx));
        }
        for gap in [2u8, 3, 7, 9, 10, 200] {
            assert_eq!(s.actor_of(gap), None, "slot {gap}");
        }
        assert!(Slots::new(3, 6).is_some());
        assert!(Slots::new(1, 7).is_none());
        // slot 3 (the formation's actor) is never a party member or an enemy
        assert_eq!(Slots::new(3, 6).unwrap().actor_of(3), None);
    }

    #[test]
    fn party_members_after_the_third_take_the_slots_from_ten_up() {
        // A v9 battle: two canon companions and three players, against two enemies.
        let s = Slots::new(5, 2).unwrap();
        let got: Vec<Option<u8>> = (0..8).map(|i| s.slot_of(i)).collect();
        assert_eq!(got, vec![Some(0), Some(1), Some(2), Some(10), Some(11), Some(4), Some(5), None]);
        for (idx, slot) in [(0usize, 0u8), (1, 1), (2, 2), (3, 10), (4, 11), (5, 4), (6, 5)] {
            assert_eq!(s.actor_of(slot), Some(idx), "slot {slot}");
        }
        for gap in [3u8, 6, 7, 9, 12, 13, 14, 15, 200] {
            assert_eq!(s.actor_of(gap), None, "slot {gap}");
        }
        // The largest party the protocol allows still fits (slots 0..=2 and 10..=14), with the most enemies.
        let big = Slots::new(MAX_PARTY, MAX_ENEMIES).unwrap();
        assert_eq!(big.slot_of(MAX_PARTY - 1), Some(14));
        assert_eq!(big.actor_of(14), Some(MAX_PARTY - 1));
        assert_eq!(big.actor_of(15), None);
        assert!(Slots::new(MAX_PARTY + 1, 1).is_none());
        // Slot and position are each other's inverse for every actor of every shape of battle.
        for party in 0..=MAX_PARTY {
            for enemies in 0..=MAX_ENEMIES {
                let s = Slots::new(party, enemies).unwrap();
                for idx in 0..party + enemies {
                    let slot = s.slot_of(idx).expect("every actor has a slot");
                    assert_eq!(s.actor_of(slot), Some(idx), "party {party}, enemies {enemies}, actor {idx}");
                    assert!(slot < 16, "a slot is a bit of a 16-bit mask");
                }
            }
        }
    }

    #[test]
    fn characters_are_known_by_name() {
        assert_eq!(character_id("Cloud"), Some(0x10));
        assert_eq!(character_id("Tifa"), Some(0x12));
        assert_eq!(character_id("Aerith"), Some(0x13));
        assert_eq!(character_id("Cid"), Some(0x18));
        assert_eq!(character_id("Hero"), None);
    }

    #[test]
    fn the_fields_the_engine_models_are_known_and_the_rest_is_not() {
        for (addr, size) in [
            (0x4000u16, 3u8),
            (0x4000, 0),
            (0x401A, 0),
            (0x4025, 0),
            (0x4026, 0),
            (0x402D, 0),
            (0x4040, 1),
            (0x4048, 1),
            (0x4060, 1),
            (0x40A0, 1),
            (0x40A8, 1),
            (0x4100, 2),
            (0x4110, 2),
            (0x4140, 2),
            (0x4150, 2),
            (0x4160, 3),
            (0x4180, 3),
        ] {
            assert!(field_known(addr, size), "{addr:#06x}/{size}");
        }
        for (addr, size) in [
            (0x4021u16, 0u8), // "ally of current actor": not modelled
            (0x4020, 1),      // a whole flag byte: some of its bits are unknown
            (0x4068, 1),
            (0x4999, 2),
            (0x2060, 2), // not an actor field
        ] {
            assert!(!field_known(addr, size), "{addr:#06x}/{size}");
        }
    }

    fn battle() -> BattleState {
        let mut cloud = Combatant::new("Cloud", 7, 300);
        cloud.defense = 20;
        cloud.dex = 33;
        let mut hero = Combatant::new("Hero", 7, 120);
        hero.row = Row::Back;
        let mut foe = Combatant::new("Foe", 5, 80);
        foe.mp = 12;
        foe.max_mp = 20;
        foe.magic_defense = 44;
        let mut hound = Combatant::new("Hound", 5, 60);
        hound.hp = 0;
        BattleState::with_config(
            1,
            vec![cloud, hero],
            vec![foe, hound],
            BattleConfig {
                start: StartMode::Zero,
                ..BattleConfig::default()
            },
        )
    }

    #[test]
    fn masks_list_living_actors_from_the_point_of_view_of_the_owner() {
        let state = battle();
        let attacks = [AiAttack { id: 5, mp_cost: 3 }];
        // Foe (position 2, slot 4)
        let w = BattleWorld::new(&state, 2, &attacks).unwrap();
        assert_eq!(w.me(), 4);
        let m = w.masks();
        assert_eq!(m.me, 0b01_0000);
        assert_eq!(m.allies, 0b01_0000, "the dead Hound is not an ally");
        assert_eq!(m.enemies, 0b11);
        assert_eq!(m.players, 0b11);
        assert_eq!(m.active, 0b01_0011);
        assert_eq!(m.all, 0b11_0011, "every actor, the dead included");
        assert_eq!(w.attack_mp_cost(5), Some(3));
        assert_eq!(w.attack_mp_cost(6), None);
        // From a party member's side the sides swap.
        let w = BattleWorld::new(&state, 0, &attacks).unwrap();
        let m = w.masks();
        assert_eq!(m.allies, 0b11);
        assert_eq!(m.enemies, 0b01_0000);
        // A position outside the battle has no view.
        assert!(BattleWorld::new(&state, 9, &attacks).is_none());
    }

    #[test]
    fn the_masks_of_a_five_fighter_party_hold_the_extra_members_at_bits_ten_and_eleven() {
        let party: Vec<Combatant> = (0..5).map(|i| Combatant::new(format!("P{i}"), 5, 100 + i as u32)).collect();
        let mut dead = Combatant::new("Gone", 5, 50);
        dead.hp = 0;
        let state = BattleState::with_config(
            1,
            party,
            vec![Combatant::new("Foe", 5, 80), dead],
            BattleConfig {
                start: StartMode::Zero,
                ..BattleConfig::default()
            },
        );
        // The foe is position 5, slot 4.
        let w = BattleWorld::new(&state, 5, &[]).unwrap();
        assert_eq!(w.me(), 4);
        let m = w.masks();
        assert_eq!(m.enemies, 0b1100_0000_0111, "party members 0..=2 and 3, 4 at bits 10, 11");
        assert_eq!(m.players, m.enemies);
        assert_eq!(m.allies, 1 << 4, "the dead one is nobody's ally");
        assert_eq!(m.all, m.enemies | 0b11_0000, "every actor, the dead included");
        assert_eq!(m.active, m.enemies | 1 << 4);
        // Their fields are read through the same slots.
        assert_eq!(w.actor_field(10, 0x4160, 3), Some(103));
        assert_eq!(w.actor_field(11, 0x4160, 3), Some(104));
        assert_eq!(w.actor_field(10, 0x4040, 1), Some(10), "the actor index is the slot");
        assert_eq!(w.actor_field(3, 0x4160, 3), None, "slot 3 is the formation's actor");
        assert_eq!(w.actor_field(12, 0x4160, 3), None, "no sixth party member");
        // From a party member's point of view the sides swap, extra members are allies.
        let w = BattleWorld::new(&state, 4, &[]).unwrap();
        assert_eq!(w.me(), 11);
        assert_eq!(w.masks().allies, 0b1100_0000_0111);
        assert_eq!(w.masks().enemies, 1 << 4);
    }

    #[test]
    fn fields_come_from_the_combatants() {
        let state = battle();
        let w = BattleWorld::new(&state, 2, &[]).unwrap();
        assert_eq!(w.actor_field(0, 0x4160, 3), Some(300));
        assert_eq!(w.actor_field(0, 0x4180, 3), Some(300));
        assert_eq!(w.actor_field(4, 0x4140, 2), Some(12));
        assert_eq!(w.actor_field(4, 0x4150, 2), Some(20));
        assert_eq!(w.actor_field(0, 0x4100, 2), Some(20));
        assert_eq!(w.actor_field(4, 0x4110, 2), Some(44));
        assert_eq!(w.actor_field(0, 0x40A0, 1), Some(33));
        assert_eq!(w.actor_field(0, 0x4048, 1), Some(7));
        assert_eq!(w.actor_field(1, 0x4026, 0), Some(1), "Hero is in the back row");
        assert_eq!(w.actor_field(0, 0x4026, 0), Some(0));
        assert_eq!(w.actor_field(5, 0x4000, 0), Some(1), "the Hound is dead");
        assert_eq!(w.actor_field(5, 0x402D, 0), Some(1));
        assert_eq!(w.actor_field(4, 0x4000, 0), Some(0));
        assert_eq!(w.actor_field(0, 0x4040, 1), Some(0));
        assert_eq!(w.actor_field(5, 0x4040, 1), Some(5));
        // A party member's id is by its name; an unknown name has none; an enemy has its instance.
        assert_eq!(w.actor_field(0, 0x4060, 1), Some(0x10));
        assert_eq!(w.actor_field(1, 0x4060, 1), None);
        assert_eq!(w.actor_field(4, 0x4060, 1), Some(0));
        assert_eq!(w.actor_field(5, 0x4060, 1), Some(1));
        // No such actor, an address that is not a field, one that is not modelled.
        assert_eq!(w.actor_field(2, 0x4160, 3), None);
        assert_eq!(w.actor_field(0, 0x2060, 2), None);
        assert_eq!(w.actor_field(0, 0x4999, 2), None);
    }

    #[test]
    fn a_battle_too_big_for_the_slots_has_no_view() {
        let party: Vec<Combatant> = (0..=MAX_PARTY).map(|i| Combatant::new(format!("P{i}"), 1, 10)).collect();
        let foes = vec![Combatant::new("F", 1, 10)];
        let state = BattleState::new(1, party, foes);
        assert!(BattleWorld::new(&state, MAX_PARTY + 1, &[]).is_none());
        let party = vec![Combatant::new("P", 1, 10)];
        let foes: Vec<Combatant> = (0..7).map(|i| Combatant::new(format!("F{i}"), 1, 10)).collect();
        let state = BattleState::new(1, party, foes);
        assert!(BattleWorld::new(&state, 1, &[]).is_none());
    }
}
