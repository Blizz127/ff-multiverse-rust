//! The sounds of a battle, from the game's own data, and WHEN each one plays.
//!
//! # What is played, and where it comes from
//!
//! | performance | sound | source |
//! |---|---|---|
//! | a party member's attack that hits | its weapon's hit sound | the weapon record's sound bytes ([`WeaponSounds`]) |
//! | ... that is a critical hit | its weapon's critical-hit sound | the same |
//! | ... that misses | its weapon's miss sound | the same |
//! | an enemy's attack that lands | the attack's impact sound | `scene.bin`'s attack record, `u16` at `0x06` |
//! | a spell that lands | the spell's impact sound, when it has one | the kernel's attack record (none of the spells has) |
//! | an item, a limit break, a guard, a failed escape | nothing | no sound field in an item record, no attack record for a limit |
//! | a successful escape | "Escaping from a Battle" | a hand-made list of the sound bank (**by ear**, see below) |
//! | an enemy that is beaten | "Enemy Death" | the same (**by ear**) |
//!
//! The ids are the game's sound ids, 1-based, 0 and 0xFFFF "no sound", entry `id - 1` of `audio.fmt`
//! (`fmv_ff7::sound`, "Battle sound ids", has the evidence: 27 of 27 ids name the clips the community calls by those
//! roles, FFNx has one id space for every mode, and the lengths fit). What the data does NOT hold is not played from it:
//! every one of the kernel's 128 attack records for the spells has 0xFFFF (their effects make their own sounds in code
//! that is not in any file the library holds), the limit breaks (ids 128 and up) have no attack record, an item has no
//! sound field. So a battle that has no sourced sound for something is silent for it, never a guess.
//!
//! **By ear.** The two sounds named by a hand-made list of the clips ("Enemy Death" and "Escaping from a Battle", ids 22
//! and 26) are identifications by listening, not data, and nothing says WHEN the game plays them (a boss, for one, has its
//! own death clip, and nothing parsed marks an enemy as a boss). They are [`Why::by_ear`] and the audio plays them only when
//! asked (`FMV_BATTLE_SOUNDS=all`); by default only what the data names is heard.
//!
//! # When
//!
//! Each sound plays at the instant its performance LANDS: the very moment the number floats up over the target
//! ([`cue::Cue::delay_of`]: the performance's own start, one after the other, plus its impact delay), not when the batch
//! arrives. [`Sounds::on_events`] is called with the batch, like the numbers and the banner are, and keeps what to play
//! and when; the audio asks [`Sounds::due`] every frame for the sounds that came due since it last looked. A sound that is
//! due after the battle has ended still plays (the killing blow of a battle is heard after its `BattleEnd`).
//!
//! One sound per performance: a blow on three targets at once is one impact, not three.

use super::scene::cue;
use fmv_ff7_battle::kernel::{Kernel, WeaponSounds};
use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent};
use fmv_ff7_battle::scene::SceneBin;
use fmv_protocol::BattleMember;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// The bank has this many sounds (`audio.fmt` has 750 records); an id past it names nothing.
pub const MAX_SOUND_ID: u16 = 750;
/// "Escaping from a Battle": entry 25 of the bank, by ear (see the module docs).
pub const ESCAPE_SOUND: u16 = 26;
/// "Enemy Death": entry 21 of the bank, by ear (see the module docs).
pub const ENEMY_DEATH_SOUND: u16 = 22;
/// How long a sound that came due is kept, in seconds: the audio looks every frame, so this only has to outlive a slow one.
const KEEP_S: f64 = 6.0;
/// Sounds kept at most; beyond this the oldest are dropped.
const MAX_SCHEDULED: usize = 64;
/// The most kernel attack records and weapons read (the real tables have 128 of each).
const MAX_RECORDS: usize = 256;
/// The fighters of the kernel's initialization data (Cloud 0 to Cid 8).
const CHARACTERS: usize = 9;

/// An id that names a sound: 1 to [`MAX_SOUND_ID`] (0 and 0xFFFF are "no sound", and nothing past the bank is one).
fn valid(id: u16) -> Option<u16> {
    (1..=MAX_SOUND_ID).contains(&id).then_some(id)
}

/// Why a sound plays (what the log says, and what the audio decides on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Why {
    /// A weapon's blow hit.
    Hit,
    /// A weapon's blow was a critical hit.
    Critical,
    /// A weapon's blow missed.
    Miss,
    /// An attack record's impact sound.
    Impact,
    /// The party got away (by ear).
    Escape,
    /// An enemy was beaten (by ear).
    Death,
}

impl Why {
    pub fn name(self) -> &'static str {
        match self {
            Why::Hit => "hit",
            Why::Critical => "critical",
            Why::Miss => "miss",
            Why::Impact => "impact",
            Why::Escape => "escape",
            Why::Death => "death",
        }
    }

    /// Named by a hand-made list of the sound bank and not by the data (see the module docs): played only on request.
    pub fn by_ear(self) -> bool {
        matches!(self, Why::Escape | Why::Death)
    }
}

/// A sound a battle makes: the game's id of it and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BattleSound {
    pub id: u16,
    pub why: Why,
}

/// A sound a battle may need, to have it read before it is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Planned {
    pub id: u16,
    /// Named by ear only (see [`Why::by_ear`]).
    pub by_ear: bool,
}

/// What this machine's own battle data says a battle sounds like: the weapons' sounds, who starts with which weapon, and
/// the impact sounds of the kernel's and the scenes' attack records. Read once from the same files as the rules, shared
/// (it travels in the [`super::names::NameBook`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoundBook {
    /// The three sounds of each weapon, by weapon id.
    weapons: Vec<WeaponSounds>,
    /// The weapon each fighter of the initialization data starts with, by character (`None`: no record, so no weapon: the
    /// engine fights such a character unarmed).
    starting_weapon: [Option<u8>; CHARACTERS],
    /// The impact sound of each kernel attack record, by attack index (a spell's index).
    kernel_attacks: Vec<Option<u16>>,
    /// The impact sound of the scenes' attack records, by `(scene index, attack id)`.
    scene_attacks: HashMap<(u16, u16), u16>,
}

impl SoundBook {
    /// The book of this machine's own battle data. Ids that name nothing (0, 0xFFFF, past the bank) are dropped here, so
    /// a record without a sound has none.
    pub fn from_data(kernel: &Kernel, scenes: &SceneBin) -> Self {
        let weapons = (0..MAX_RECORDS).map_while(|id| u8::try_from(id).ok().and_then(|id| kernel.weapon_sounds(id))).collect();
        let starting_weapon = std::array::from_fn(|who| u8::try_from(who).ok().and_then(|who| kernel.init_character_by_id(who)).map(|c| c.weapon));
        let kernel_attacks = (0..MAX_RECORDS).map_while(|i| kernel.attack(i)).map(|a| valid(a.impact_sound)).collect();
        let mut scene_attacks = HashMap::new();
        for scene in scenes.scenes() {
            let Ok(index) = u16::try_from(scene.index) else { continue };
            for attack in &scene.attacks {
                if let Some(sound) = valid(attack.record.impact_sound) {
                    scene_attacks.insert((index, attack.id), sound);
                }
            }
        }
        Self { weapons, starting_weapon, kernel_attacks, scene_attacks }
    }

    /// The sounds of the weapon a party member fights with: the roster's override, else the weapon the character's
    /// starting record holds (the same choice as the engine's `Combatant::from_character`). `None` for a fighter the engine
    /// has no weapon for (Cait Sith and Vincent have no starting record on the Steam copy, so they fight unarmed and
    /// there is nothing to hear: no weapon is invented for them) and for a weapon id past the table.
    pub fn weapon(&self, character: u8, equipped: Option<u8>) -> Option<WeaponSounds> {
        let id = equipped.or_else(|| self.starting_weapon.get(usize::from(character)).copied().flatten())?;
        self.weapons.get(usize::from(id)).copied()
    }

    /// The impact sound of kernel attack record `index` (a spell), if it has one.
    pub fn spell(&self, index: u16) -> Option<u16> {
        self.kernel_attacks.get(usize::from(index)).copied().flatten()
    }

    /// The impact sound of attack `id` of scene `scene`, if its record has one.
    pub fn enemy_attack(&self, scene: u16, id: u16) -> Option<u16> {
        self.scene_attacks.get(&(scene, id)).copied()
    }

    /// The impact sounds of every attack of scene `scene` (the sounds a fight in it can need), once each.
    pub fn scene_impacts(&self, scene: u16) -> BTreeSet<u16> {
        self.scene_attacks.iter().filter(|((s, _), _)| *s == scene).map(|(_, sound)| *sound).collect()
    }

    /// A book of invented records (tests): see the `with_` methods.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn with_weapon(mut self, id: u8, hit: u16, critical: u16, miss: u16) -> Self {
        let at = usize::from(id);
        if self.weapons.len() <= at {
            self.weapons.resize(at + 1, WeaponSounds { hit: 0, critical: 0, miss: 0 });
        }
        self.weapons[at] = WeaponSounds { hit, critical, miss };
        self
    }

    #[cfg(test)]
    pub fn with_starting_weapon(mut self, character: u8, weapon: u8) -> Self {
        self.starting_weapon[usize::from(character)] = Some(weapon);
        self
    }

    #[cfg(test)]
    pub fn with_spell(mut self, index: u16, sound: u16) -> Self {
        let at = usize::from(index);
        if self.kernel_attacks.len() <= at {
            self.kernel_attacks.resize(at + 1, None);
        }
        self.kernel_attacks[at] = valid(sound);
        self
    }

    #[cfg(test)]
    pub fn with_scene_attack(mut self, scene: u16, attack: u16, sound: u16) -> Self {
        if let Some(sound) = valid(sound) {
            self.scene_attacks.insert((scene, attack), sound);
        }
        self
    }
}

/// What one battle sounds like: the book, the scene of its formation and the weapons of its party. Made when the battle
/// starts ([`Palette::new`]); an empty palette (no book on this machine) is silent.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    book: Option<Arc<SoundBook>>,
    /// The scene of the formation (`formation / 4`): whose attack table names the enemies' attacks.
    scene: u16,
    /// The weapon sounds of each party member, by actor (`None`: unarmed). The party is `0..weapons.len()`.
    weapons: Vec<Option<WeaponSounds>>,
}

impl Palette {
    /// The palette of a battle of formation `formation` with `party` (the roster, in actor order).
    pub fn new(book: Option<Arc<SoundBook>>, formation: u16, party: &[BattleMember]) -> Self {
        let weapons = party.iter().map(|m| book.as_deref().and_then(|b| b.weapon(m.character, m.weapon))).collect();
        Self { book, scene: formation / 4, weapons }
    }

    /// Is `actor` one of the party?
    fn is_party(&self, actor: ActorId) -> bool {
        usize::from(actor.0) < self.weapons.len()
    }

    fn weapon(&self, actor: ActorId) -> Option<WeaponSounds> {
        self.weapons.get(usize::from(actor.0)).copied().flatten()
    }

    /// Every sound this battle can need, once each: the party's weapons, the impact sounds of the scene's attacks and of
    /// the spells in reach (`spells`: kernel attack indexes), then the two by-ear ones. Empty without a book.
    pub fn plan(&self, spells: &[u16]) -> Vec<Planned> {
        let Some(book) = self.book.as_deref() else { return Vec::new() };
        let mut ids: BTreeSet<u16> = BTreeSet::new();
        for weapon in self.weapons.iter().flatten() {
            ids.extend([weapon.hit, weapon.critical, weapon.miss].into_iter().filter_map(valid));
        }
        ids.extend(book.scene_impacts(self.scene));
        ids.extend(spells.iter().filter_map(|spell| book.spell(*spell)));
        let mut plan: Vec<Planned> = ids.iter().map(|&id| Planned { id, by_ear: false }).collect();
        plan.extend([ESCAPE_SOUND, ENEMY_DEATH_SOUND].into_iter().filter(|id| !ids.contains(id)).map(|id| Planned { id, by_ear: true }));
        plan
    }

    /// The sound a performance of `action` by `actor` makes, if it makes one, and how long after the performance began (its
    /// impact: the moment the number floats up).
    fn sound_of(&self, actor: ActorId, action: ActionRef, cue: &cue::Cue<'_>) -> Option<(f64, BattleSound)> {
        let book = self.book.as_deref()?;
        let events = cue.events;
        let damaged = events.iter().any(|e| matches!(e, BattleEvent::Damage { .. }));
        let sound = match action {
            // A party member's blow: its weapon's sound for what came of it. An enemy's plain attack has no weapon, and
            // neither has a party member the engine fights unarmed.
            ActionRef::Attack => {
                let weapon = self.weapon(actor).filter(|_| self.is_party(actor))?;
                let critical = events.iter().any(|e| matches!(e, BattleEvent::Damage { critical: true, .. }));
                let missed = events.iter().any(|e| matches!(e, BattleEvent::Miss { .. }));
                let (id, why) = match (critical, damaged, missed) {
                    (true, _, _) => (weapon.critical, Why::Critical),
                    (false, true, _) => (weapon.hit, Why::Hit),
                    (false, false, true) => (weapon.miss, Why::Miss),
                    (false, false, false) => return None,
                };
                BattleSound { id: valid(id)?, why }
            }
            // An enemy's attack: its record's impact sound, when it connects (a blow that misses has none).
            ActionRef::EnemyAttack(id) if damaged => BattleSound { id: book.enemy_attack(self.scene, id)?, why: Why::Impact },
            // A spell: its record's impact sound if it has one (none of the spells does), when it lands on somebody.
            ActionRef::Magic(index) if !cue.targets().is_empty() => BattleSound { id: book.spell(index)?, why: Why::Impact },
            ActionRef::EnemyAttack(_) | ActionRef::Magic(_) => return None,
            // No sound field in an item record, no attack record for a limit break (its id is a name's, 128 and up: it is
            // never looked up in the kernel's table), a guard has no results.
            ActionRef::Item(_) | ActionRef::Limit(_) | ActionRef::Defend => return None,
            // The party got away: a failed attempt only stumbles.
            ActionRef::Escape if events.iter().any(|e| matches!(e, BattleEvent::EscapeFailed { .. })) => return None,
            ActionRef::Escape => BattleSound { id: ESCAPE_SOUND, why: Why::Escape },
        };
        Some((cue.impact(), sound))
    }
}

/// A sound waiting for its moment.
#[derive(Debug, Clone, Copy)]
struct Scheduled {
    /// When it plays, seconds on the client's clock (possibly still ahead).
    at: f64,
    sound: BattleSound,
}

/// The sounds of one battle, scheduled as its events arrive.
#[derive(Debug, Clone, Default)]
pub struct Sounds {
    /// By time, oldest first.
    live: Vec<Scheduled>,
}

impl Sounds {
    pub fn clear(&mut self) {
        self.live.clear();
    }

    fn push(&mut self, at: f64, sound: BattleSound) {
        if self.live.len() >= MAX_SCHEDULED {
            self.live.remove(0);
        }
        let place = self.live.iter().rposition(|s| s.at <= at).map_or(0, |i| i + 1);
        self.live.insert(place, Scheduled { at, sound });
    }

    /// The sounds the events of a batch that arrived at `now` make, each at the instant its performance lands (see the
    /// module docs): the same instants the numbers are born at.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent], palette: &Palette) {
        // What is long over is forgotten; what is ahead (a counter waiting for the blow before it) is not.
        self.live.retain(|s| now - s.at < KEEP_S);
        for cue in cue::cues(events) {
            if let Some((actor, action)) = cue.action {
                if let Some((after, sound)) = palette.sound_of(actor, action, &cue) {
                    self.push(now + cue.start + after, sound);
                }
            }
            // A beaten enemy: its sound when its collapse begins (the party's falling has none).
            if palette.book.is_some() {
                for event in cue.events {
                    if let BattleEvent::Death { actor } = event {
                        if !palette.is_party(*actor) {
                            self.push(now + cue.delay_of(event), BattleSound { id: ENEMY_DEATH_SOUND, why: Why::Death });
                        }
                    }
                }
            }
        }
    }

    /// The sounds due in `from <= at < to` (the window between two looks of the audio), with the time each was due, oldest
    /// first. Looking at consecutive windows gives each sound once.
    pub fn due(&self, from: f64, to: f64) -> Vec<(f64, BattleSound)> {
        self.live.iter().filter(|s| s.at >= from && s.at < to).map(|s| (s.at, s.sound)).collect()
    }

    /// Everything scheduled and not yet forgotten, by time (tests).
    #[cfg(test)]
    pub fn scheduled(&self) -> Vec<(f64, BattleSound)> {
        self.live.iter().map(|s| (s.at, s.sound)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AccountId, EntityId};
    use fmv_ff7_battle::rules::ActionRef as A;

    fn hit(actor: u8, target: u8, critical: bool) -> BattleEvent {
        BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount: 9, critical }
    }

    fn miss(actor: u8, target: u8) -> BattleEvent {
        BattleEvent::Miss { actor: ActorId(actor), target: ActorId(target) }
    }

    fn act(actor: u8, action: A) -> BattleEvent {
        BattleEvent::Action { actor: ActorId(actor), action }
    }

    /// Two players (character 0 and 3: invented weapons 0 and 5) against enemies of scene 75.
    fn palette() -> Palette {
        let book = SoundBook::for_tests()
            .with_weapon(0, 18, 27, 5)
            .with_weapon(5, 0x100 + 36, 0x100 + 94, 5)
            .with_starting_weapon(0, 0)
            .with_starting_weapon(3, 5)
            .with_scene_attack(75, 272, 15)
            .with_scene_attack(75, 273, 35)
            .with_scene_attack(75, 286, 0xFFFF)
            .with_scene_attack(76, 272, 99)
            .with_spell(0, 0xFFFF)
            .with_spell(9, 61);
        let party = [
            BattleMember::player(EntityId(1), AccountId(1), "A", 0, 6, 0),
            BattleMember::player(EntityId(2), AccountId(2), "B", 3, 6, 0),
        ];
        Palette::new(Some(Arc::new(book)), 301, &party)
    }

    fn sounds_of(events: &[BattleEvent]) -> Vec<(f64, BattleSound)> {
        let mut s = Sounds::default();
        s.on_events(10.0, events, &palette());
        s.scheduled()
    }

    fn one(id: u16, why: Why) -> BattleSound {
        BattleSound { id, why }
    }

    #[test]
    fn a_party_members_attack_makes_its_weapons_hit_critical_or_miss_sound() {
        // The roster's character decides the weapon (its starting one), and a weapon's byte plus 0x100 is a high id.
        let blow = |event: BattleEvent, who: u8| sounds_of(&[act(who, A::Attack), event]);
        assert_eq!(blow(hit(0, 2, false), 0).iter().map(|s| s.1).collect::<Vec<_>>(), [one(18, Why::Hit)]);
        assert_eq!(blow(hit(0, 2, true), 0).iter().map(|s| s.1).collect::<Vec<_>>(), [one(27, Why::Critical)]);
        assert_eq!(blow(miss(0, 2), 0).iter().map(|s| s.1).collect::<Vec<_>>(), [one(5, Why::Miss)]);
        assert_eq!(blow(hit(1, 2, false), 1).iter().map(|s| s.1).collect::<Vec<_>>(), [one(292, Why::Hit)]);
        assert_eq!(blow(hit(1, 2, true), 1).iter().map(|s| s.1).collect::<Vec<_>>(), [one(350, Why::Critical)]);
        // An attack with nothing that lands (nobody to hit) has nothing to hear.
        assert!(sounds_of(&[act(0, A::Attack)]).is_empty());
        // A blow on two targets is one impact; a critical one is the critical sound, once.
        let both = sounds_of(&[act(0, A::Attack), hit(0, 2, false), hit(0, 3, true)]);
        assert_eq!(both.iter().map(|s| s.1).collect::<Vec<_>>(), [one(27, Why::Critical)]);
    }

    #[test]
    fn the_roster_can_override_the_weapon_and_a_member_the_engine_fights_unarmed_is_silent() {
        // Weapon 5 on character 0, by the roster; character 6 has no starting weapon and no override.
        let book = SoundBook::for_tests().with_weapon(0, 18, 27, 5).with_weapon(5, 40, 41, 42).with_starting_weapon(0, 0);
        let mut armed = BattleMember::player(EntityId(1), AccountId(1), "A", 0, 6, 0);
        armed.weapon = Some(5);
        let unarmed = BattleMember::player(EntityId(2), AccountId(2), "B", 6, 6, 0);
        let palette = Palette::new(Some(Arc::new(book)), 301, &[armed, unarmed]);
        let mut s = Sounds::default();
        s.on_events(1.0, &[act(0, A::Attack), hit(0, 2, false), act(1, A::Attack), hit(1, 2, false)], &palette);
        assert_eq!(s.scheduled().iter().map(|x| x.1).collect::<Vec<_>>(), [one(40, Why::Hit)], "the override's, and nothing for the unarmed one");
    }

    #[test]
    fn an_enemy_attack_makes_its_records_impact_sound_when_it_lands_and_a_record_without_one_is_silent() {
        // Scene 75 (formation 301): attack 272 is sound 15, 273 is 35, 286 has none; scene 76 has its own table.
        let landed = |attack: u16| sounds_of(&[act(2, A::EnemyAttack(attack)), hit(2, 0, false)]).iter().map(|s| s.1).collect::<Vec<_>>();
        assert_eq!(landed(272), [one(15, Why::Impact)]);
        assert_eq!(landed(273), [one(35, Why::Impact)]);
        assert!(landed(286).is_empty(), "0xFFFF is no sound");
        assert!(landed(999).is_empty(), "an attack the scene does not have");
        // A blow on the whole party is one impact; a blow that missed everybody has none.
        let all = sounds_of(&[act(2, A::EnemyAttack(272)), hit(2, 0, false), hit(2, 1, false)]);
        assert_eq!(all.len(), 1);
        assert!(sounds_of(&[act(2, A::EnemyAttack(272)), miss(2, 0)]).is_empty());
        // The scene of the formation says which table: formation 304 is scene 76, whose attack 272 is another sound.
        let book = palette().book.clone();
        let other = Palette::new(book, 304, &[]);
        let mut s = Sounds::default();
        s.on_events(1.0, &[act(2, A::EnemyAttack(272)), hit(2, 0, false)], &other);
        assert_eq!(s.scheduled().iter().map(|x| x.1).collect::<Vec<_>>(), [one(99, Why::Impact)]);
    }

    #[test]
    fn spells_items_limits_and_guards_have_no_sourced_sound_and_a_spell_record_that_has_one_plays_it() {
        // Spell 0 has 0xFFFF (like every spell on the Steam copy): silent. Spell 9 has a sound (an invented one).
        let landed = |action: A| sounds_of(&[act(0, action), hit(0, 2, false)]).iter().map(|s| s.1).collect::<Vec<_>>();
        assert!(landed(A::Magic(0)).is_empty());
        assert_eq!(landed(A::Magic(9)), [one(61, Why::Impact)]);
        assert!(landed(A::Magic(77)).is_empty(), "a spell past the table");
        // Items and limits are never looked up: a limit's id (128 and up) is a name's, not a record's, and no id may
        // reach record 98 or 113 by being near it.
        assert!(landed(A::Item(0)).is_empty());
        for limit in [128, 135, 142, 149, 98, 113] {
            assert!(landed(A::Limit(limit)).is_empty(), "limit {limit}");
        }
        assert!(sounds_of(&[act(0, A::Defend)]).is_empty());
    }

    #[test]
    fn an_escape_is_heard_only_when_it_succeeds_and_a_beaten_enemy_has_its_death_sound_but_the_party_has_none() {
        let (escape, death) = (one(ESCAPE_SOUND, Why::Escape), one(ENEMY_DEATH_SOUND, Why::Death));
        assert_eq!(sounds_of(&[act(0, A::Escape)]).iter().map(|s| s.1).collect::<Vec<_>>(), [escape]);
        assert!(sounds_of(&[act(0, A::Escape), BattleEvent::EscapeFailed { actor: ActorId(0) }]).is_empty());
        // Actors 0 and 1 are the party, 2 and up are enemies.
        let fall = sounds_of(&[act(0, A::Attack), hit(0, 2, false), BattleEvent::Death { actor: ActorId(2) }, BattleEvent::Death { actor: ActorId(1) }]);
        assert_eq!(fall.iter().map(|s| s.1).collect::<Vec<_>>(), [one(18, Why::Hit), death], "the enemy's death only");
        // Both are by ear; what the data names is not.
        assert!(escape.why.by_ear() && death.why.by_ear());
        assert!([Why::Hit, Why::Critical, Why::Miss, Why::Impact].iter().all(|w| !w.by_ear()));
        // No book, no sound at all (nothing is known of the battle).
        let mut s = Sounds::default();
        s.on_events(1.0, &[act(0, A::Escape), BattleEvent::Death { actor: ActorId(2) }], &Palette::default());
        assert!(s.scheduled().is_empty());
    }

    #[test]
    fn a_sound_plays_when_its_performance_lands_and_a_counter_waits_for_the_blow_it_answers() {
        // A blow and the counter to it in ONE batch: the blow lands at its impact, the counter after the blow's
        // performance is over, never at once; the death that follows the blow comes a beat after it.
        let batch = [
            act(0, A::Attack),
            hit(0, 2, false),
            BattleEvent::Death { actor: ActorId(2) },
            act(2, A::EnemyAttack(272)),
            hit(2, 0, false),
        ];
        let heard = sounds_of(&batch);
        let blow = 10.0 + cue::impact_delay(Some(A::Attack));
        let death = 10.0 + cue::impact_delay(Some(A::Attack)) + cue::DEATH_BEAT;
        let counter = 10.0 + cue::length_of(Some(A::Attack)) + cue::impact_delay(Some(A::EnemyAttack(272)));
        let want = [(blow, one(18, Why::Hit)), (death, one(ENEMY_DEATH_SOUND, Why::Death)), (counter, one(15, Why::Impact))];
        assert_eq!(heard.len(), 3, "{heard:?}");
        for ((at, sound), (want_at, want_sound)) in heard.iter().zip(want) {
            assert!((at - want_at).abs() < 1e-9 && *sound == want_sound, "{heard:?} against {want:?}");
        }
        // Nothing at the arrival of the batch: the first blow has still to land.
        let mut s = Sounds::default();
        s.on_events(10.0, &batch, &palette());
        assert!(s.due(f64::NEG_INFINITY, 10.0).is_empty() && s.due(10.0, blow - 0.001).is_empty());
        assert_eq!(s.due(10.0, blow + 0.001).len(), 1);
        // The number of the same blow is born at the very same instant (the overlay and the audio are one timeline).
        let mut popups = super::super::popups::Popups::default();
        popups.on_events(10.0, &batch);
        let births: Vec<f64> = popups.births().iter().map(|(_, at)| *at).collect();
        assert!((births[0] - blow).abs() < 1e-9 && (births[1] - counter).abs() < 1e-9, "{births:?}");
    }

    #[test]
    fn looking_at_consecutive_windows_gives_each_sound_once_and_a_late_batch_is_still_heard() {
        let mut s = Sounds::default();
        s.on_events(10.0, &[act(0, A::Attack), hit(0, 2, false)], &palette());
        s.on_events(10.2, &[act(1, A::Attack), hit(1, 2, true)], &palette());
        let (mut heard, mut from) = (Vec::new(), 10.0);
        for to in [10.2, 10.4, 10.6, 10.8, 11.0] {
            heard.extend(s.due(from, to).into_iter().map(|(_, sound)| sound.id));
            from = to;
        }
        assert_eq!(heard, [18, 350], "each once, in the order they come due");
        // What is long over is forgotten by the next batch; what is ahead is kept.
        s.on_events(20.0, &[act(0, A::Attack), hit(0, 2, false)], &palette());
        assert_eq!(s.scheduled().len(), 1);
        // The list is bounded.
        for i in 0..200 {
            s.on_events(30.0 + f64::from(i) * 0.001, &[act(0, A::Attack), hit(0, 2, false)], &palette());
        }
        assert!(s.scheduled().len() <= MAX_SCHEDULED);
        s.clear();
        assert!(s.scheduled().is_empty());
    }

    #[test]
    fn ids_that_name_nothing_are_no_sound() {
        assert_eq!([valid(0), valid(1), valid(750), valid(751), valid(0xFFFF)], [None, Some(1), Some(750), None, None]);
        // A weapon record with such an id is silent for that blow (and the others still play).
        let book = SoundBook::for_tests().with_weapon(0, 0, 0xFFFF, 5).with_starting_weapon(0, 0);
        let party = [BattleMember::player(EntityId(1), AccountId(1), "A", 0, 6, 0)];
        let palette = Palette::new(Some(Arc::new(book)), 4, &party);
        let mut s = Sounds::default();
        s.on_events(1.0, &[act(0, A::Attack), hit(0, 1, false)], &palette);
        assert!(s.scheduled().is_empty(), "a hit whose sound is id 0");
        s.on_events(1.0, &[act(0, A::Attack), miss(0, 1)], &palette);
        assert_eq!(s.scheduled().iter().map(|x| x.1).collect::<Vec<_>>(), [one(5, Why::Miss)]);
    }

    #[test]
    fn the_plan_is_what_the_battle_can_need_once_each() {
        // Weapons 0 and 5 (5 ids: their miss is one), the scene's impacts (15, 35; 0xFFFF is none; scene 76's 99 is not this scene's), spell 9's
        // sound (61), and last the two by ear.
        let plan = palette().plan(&[0, 9]);
        let data: Vec<u16> = plan.iter().filter(|p| !p.by_ear).map(|p| p.id).collect();
        assert_eq!(data, [5, 15, 18, 27, 35, 61, 292, 350]);
        let ear: Vec<u16> = plan.iter().filter(|p| p.by_ear).map(|p| p.id).collect();
        assert_eq!(ear, [ESCAPE_SOUND, ENEMY_DEATH_SOUND]);
        assert!(!data.contains(&99), "another scene's attack is not needed");
        // Without the spells in reach nothing of theirs is asked for; without a book the plan is empty.
        assert!(!palette().plan(&[]).iter().any(|p| p.id == 61));
        assert!(Palette::default().plan(&[0, 9]).is_empty());
        // A by-ear id that the data names as well is asked for once, as data.
        let book = SoundBook::for_tests().with_scene_attack(1, 1, ESCAPE_SOUND);
        let plan = Palette::new(Some(Arc::new(book)), 4, &[]).plan(&[]);
        assert_eq!(plan.iter().filter(|p| p.id == ESCAPE_SOUND).count(), 1);
        assert!(plan.iter().any(|p| p.id == ESCAPE_SOUND && !p.by_ear));
    }

    /// The evidence table of `fmv_ff7::sound` ("Battle sound ids"), checked on the owner's `audio.fmt`: the 27 ids that the data names for
    /// something the community's list of the sound bank also names, and the number that list gives the clip. The list numbers the
    /// 724 clips from 0 and skips the 26 empty slots, so its number is the entry minus the empty slots before it. Only ids, counts
    /// and numbers are used (`FMV_FF7_AUDIO_DATA` is the `data/` directory that holds `sound/audio.fmt` and `sound/audio.dat`).
    #[test]
    #[ignore = "needs the owner's real FF7 sound files (FMV_FF7_AUDIO_DATA)"]
    fn the_real_sound_bank_numbers_the_clips_of_the_data_the_way_the_communitys_list_does() {
        use fmv_ff7::sound::SfxBank;
        let Some(audio) = std::env::var_os("FMV_FF7_AUDIO_DATA") else {
            eprintln!("skipped: FMV_FF7_AUDIO_DATA is not set");
            return;
        };
        let audio = std::path::PathBuf::from(audio).join("sound");
        let bank = SfxBank::open(&audio.join("audio.fmt"), &audio.join("audio.dat")).expect("the sound bank opens");
        assert_eq!((bank.len(), bank.clip_count()), (750, 724), "750 records, 26 of them empty slots");
        // (the id the data names, the list's number): weapons, then the attacks of the enemies.
        let named: [(u16, usize); 27] = [
            (5, 4), (18, 17), (27, 26), (16, 15), (46, 45), (47, 46), (50, 49), (23, 22), (109, 108),
            (0x100 + 36, 289), (0x100 + 94, 347), (0x100 + 89, 342), (0x100 + 18, 271), (123, 122),
            (15, 14), (25, 24), (35, 34), (20, 19), (118, 117), (144, 143), (28, 27), (221, 218), (226, 223), (227, 224), (228, 225), (250, 247), (348, 345),
        ];
        for (id, number) in named {
            let entry = usize::from(id) - 1;
            assert!(bank.info(entry).is_some(), "sound id {id} is an empty slot");
            let before = (0..entry).filter(|e| bank.info(*e).is_some()).count();
            assert_eq!(before, number, "sound id {id}: entry {entry} is clip number {before} of the clips, the list says {number}");
        }
        // The slots that are empty before entry 300 are the two that make the shift of 2.
        let empty: Vec<usize> = (0..300).filter(|e| bank.info(*e).is_none()).collect();
        assert_eq!(empty, [198, 209]);
    }

    /// Every sound id a battle of the md1 fields' formations, of the Guard Scorpion's formation 324 and of the starting party can
    /// need names a clip in the owner's `audio.fmt`, read from the owner's own files (nothing is written; only ids, counts and
    /// lengths are printed). `FMV_FF7_LIBRARY` is the state directory the battle data is read from (the way the names test
    /// reads it), `FMV_FF7_AUDIO_DATA` the `data/` directory that holds `sound/audio.fmt` and `sound/audio.dat`.
    #[test]
    #[ignore = "needs the owner's real FF7 library (FMV_FF7_LIBRARY) and sound files (FMV_FF7_AUDIO_DATA)"]
    fn the_real_data_names_a_clip_for_every_sound_the_md1_fights_and_the_boss_can_need() {
        use crate::ff7::Ff7Source;
        use fmv_ff7::sound::SfxBank;
        use fmv_ff7_battle::rules::party_spells;
        use fmv_protocol::Ff7BattleData;
        let (Some(library), Some(audio)) = (std::env::var_os("FMV_FF7_LIBRARY"), std::env::var_os("FMV_FF7_AUDIO_DATA")) else {
            eprintln!("skipped: FMV_FF7_LIBRARY and FMV_FF7_AUDIO_DATA are not both set");
            return;
        };
        let source = Ff7Source::Library { state_dir: Some(library.into()), set: None };
        let files = fmv_library::ff7::resolve_battle_files(&source).expect("the battle files");
        let data = Ff7BattleData::open(&files.scene, &files.kernel).expect("the battle data opens");
        let book = Arc::new(SoundBook::from_data(data.kernel(), data.scenes()));
        let audio = std::path::PathBuf::from(audio).join("sound");
        let bank = SfxBank::open(&audio.join("audio.fmt"), &audio.join("audio.dat")).expect("the sound bank opens");
        // The formations: every one the eight reviewed fields' encounter tables can roll, and the Guard Scorpion's.
        let mut formations: BTreeSet<u16> = BTreeSet::from([324]);
        // The adapter alone, not `open_ff7`: that one also sets process-wide globals (the models' source) the picture tests own.
        let resolved = crate::ff7::resolve_flevel(&source).expect("the field archive");
        let adapter = fmv_ff7::Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance).expect("the field archive opens");
        for field in fmv_ff7::REVIEWED_FIELDS {
            use fmv_adapter_api::AreaRuntime;
            let area = adapter.load_field(field).unwrap_or_else(|e| panic!("field {field}: {e}"));
            if let Some(table) = area.encounters() {
                formations.extend(table.entries.iter().map(|e| e.formation));
            }
        }
        // The starting party: the canon pair, as the story fights with them (Cloud and Barret).
        let party: Vec<BattleMember> = [0u8, 1].iter().map(|c| BattleMember::companion("x", *c, 6, 0)).collect();
        let kernel = data.kernel();
        let spells: Vec<u16> = [fmv_ff7_battle::kernel::CharacterId::Cloud, fmv_ff7_battle::kernel::CharacterId::Barret]
            .into_iter()
            .flat_map(|who| party_spells(kernel, who))
            .map(|s| s.index)
            .collect();
        let mut needed: BTreeSet<u16> = BTreeSet::new();
        let mut counted = 0usize;
        for formation in &formations {
            let plan = Palette::new(Some(Arc::clone(&book)), *formation, &party).plan(&spells);
            assert!(plan.iter().any(|p| !p.by_ear), "formation {formation} needs some sound");
            counted += plan.len();
            needed.extend(plan.iter().map(|p| p.id));
        }
        let mut lengths_ms = Vec::new();
        for id in &needed {
            let info = bank.info_for_sound_id(*id).unwrap_or_else(|| panic!("sound id {id} names no clip in audio.fmt"));
            assert!(info.frames > 0, "sound id {id} is an empty clip");
            let clip = bank.decode_sound_id(*id).unwrap_or_else(|e| panic!("sound id {id} does not decode: {e}"));
            assert!(!clip.samples.is_empty() && clip.loop_region.is_none(), "sound id {id}: a one-shot with samples");
            lengths_ms.push(info.frames * 1000 / u64::from(info.rate));
        }
        eprintln!("{} formations, {counted} planned sounds, {} distinct ids, every one a non-empty one-shot clip ({}..{} ms)", formations.len(), needed.len(), lengths_ms.iter().min().unwrap(), lengths_ms.iter().max().unwrap());
        assert!(formations.contains(&301) && formations.contains(&324) && formations.len() >= 4);
        // The starting party's weapons, as ids: Cloud's Buster Sword and Barret's Gatling Gun (the canon hit, critical, miss).
        let sounds = |c: u8| book.weapon(c, None).map(|w| (w.hit, w.critical, w.miss));
        assert_eq!((sounds(0), sounds(1)), (Some((18, 27, 5)), Some((16, 46, 47))));
        // Who starts with a weapon, by character slot: the engine fights a character with no starting record unarmed, and so
        // does the sound (no weapon is invented for it).
        let armed: Vec<bool> = (0..9u8).map(|c| sounds(c).is_some()).collect();
        eprintln!("characters with a starting weapon (Cloud 0 to Cid 8): {armed:?}");
        assert!(!armed[6] && !armed[7], "Cait Sith and Vincent have no starting record, so no weapon: {armed:?}");
        assert!(armed[0] && armed[1], "Cloud and Barret do");
        // Nothing is sourced for what the party casts: every starting spell's record has no impact sound, an item has no
        // sound field, and a limit break has no record. Stated so the gap is visible.
        assert!(!spells.is_empty());
        for spell in &spells {
            assert_eq!(book.spell(*spell), None, "spell {spell}");
        }
        // The Guard Scorpion's own attacks: its rifle and tail strike have impact sounds; its Search Scope and Tail Laser
        // have none (effects).
        assert_eq!([282, 283, 285, 284, 286].map(|a| book.enemy_attack(81, a)), [Some(35), Some(35), Some(15), None, None]);
    }
}
