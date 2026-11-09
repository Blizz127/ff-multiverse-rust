//! Which clip of a battle model plays what: the knowledge about the owner's `battle.lgp` animation
//! slots that the choreography ([`super::plan`], [`super::motion`]) reads.
//!
//! A real battle model has a pack of clips, the **slots**, numbered as the game numbers them
//! (`fmv_ff7::battle_model::BattleModel::animations`); a slot that does not decode or does not
//! exist (`slot_ok` false, or past the pack) is never played: whoever asks for it gets `None` and
//! falls back to the procedural motion (lunge, flinch, collapse) of [`super::motion`]. A model that
//! is not a real battle model (a field stand-in, a box) has [`Kit::none`]: everything procedural.
//!
//! # The playable characters: the model's own scripts say what plays
//!
//! What the game plays for an action is not one clip but a **script** of the model's own (its `ab` entry,
//! `fmv_ff7::battle_model::script`): a string of clips and movements ("run up, strike, recover, run back")
//! that the engine runs for the action. The nine character models (Cloud, Barret, Tifa, Aerith, Red XIII,
//! Yuffie, Cait Sith, Vincent, Cid) answer to the same script INDICES (the table below, [`party::script`]) and
//! share the slots of their common clips, but not the clips of their blows: Aerith's and Cid's are numbered
//! one lower than the others', Yuffie's and Vincent's hop where Cloud's run. So a character's blow is read
//! from ITS OWN scripts ([`Chain::from_script`], when its model is loaded), never from a table of slots.
//!
//! **The first version of this table took slot 10 for the attack of every character, and slot 10 is the clip
//! a character plays to throw an item at an ENEMY** (script 33: hop forward, slot 10, hop back): a lunge with
//! an arm swung forward, which the owner saw as "an uppercut instead of the weapon". It came from drawing only
//! a few slots of each pack. The pictures of EVERY slot, labelled with the scripts that play them, show it:
//! `slots-<name>-<n>.png` of the ignored test `sheets::party_every_slot_sheets`.
//!
//! | script | what it is | its clips (the same slots in all nine packs unless it says so) |
//! |---|---|---|
//! | 0, 1 | idle, idle when badly hurt | 0, 1 |
//! | 3, 4 | hop forward, hop back | 3, 4: the root travels about 510 forward / 515 back |
//! | 5, 6 | hurt, hurt hard | 15; 16 and 17 |
//! | 7 | death | 18, which ends in the pose of slot 6 |
//! | 9 | cover: step in front of an ally and hold the guard | 8, a held guard (1 frame) |
//! | 12 | victory | 2 (25 to 55 frames, ends in a held pose) |
//! | 13 | run (the party running away) | 7 (6 frames, a loop, in place) |
//! | 17 | cast a spell | 3, then 12 (begin), 13 (hold), 14 (end), then 4; the spell's effect starts after 13 |
//! | 20 | **a blow with a short-range weapon**: run up, strike, run back | per character, below |
//! | 21 | **a blow with a long-range weapon**: hop forward, shoot or throw, hop back | per character, below |
//! | 32, 33 | use an item on an ally, on an enemy | 3, **9**, 4; 3, **10**, 4 |
//!
//! The other scripts (the double and quadruple strikes, the materia's slashes and throws, the limit breaks, the
//! summons) are not used: a limit break is played like a spell, the performer gathers its power with the cast clips.
//!
//! ## A blow, per character
//!
//! Script 20 is played for a weapon the engine says is short range (the scene reads it from the weapon's target
//! flags: `look_in`; of the starting weapons only Barret's and Yuffie's are long range, held by the real
//! kernel in `party_attack_range_by_starting_weapon`) and script 21 for any other. In the table the `approach`
//! is what plays while the actor closes in, the `strike` what plays from the clip the damage lands in (the damage
//! marker of the script, `F7` or `C2`, this many ticks after the strike's first clip starts, 15 ticks to the second)
//! and the `back` what brings the actor back. **Held to the owner's own files by the real-data test
//! `sheets::party_blows_are_the_scripts_clips`.** Each clip of a blow plays once, whatever its length.
//!
//! | character | short range, script 20 | long range, script 21 |
//! |---|---|---|
//! | Cloud | 26, 27 / 30, 28 at 1 / 29: a crouch, a leap and the chop of the Buster Sword | 3 / 31 at 16 / 4 |
//! | Barret | 26, 27 / 30, 28 at 3 / 29: a run and a swing of the gun arm | 3 / 34 at 14 / 4: he aims the gun arm at the target and fires (31 frames) |
//! | Tifa | 26, 27 / 30, 28 at 8 / 29: a dash, then a combination of punches and kicks (the first hit at 2) | 3 / 34 at 18 / 4 |
//! | Aerith | 25, 26 / 29, 27 at 2 / 28: she raises the staff (12 frames), leaps and brings it down | 3 / 34 at 25 / 4 |
//! | Red XIII | 26, 27 / 30, 28 at 6 / 29 | 3 / 33 at 16 / 4 |
//! | Yuffie | 3 / 30 at 6 / 4: a hop and a lunge with the shuriken | 3 / 34 at 14 / 4: she throws the shuriken (32 frames) |
//! | Cait Sith | 26, 27 / 30, 28 at 4 / 29 | 3 / 31 at 23 / 4 |
//! | Vincent | 3 / 30, 31, 32 at 6 / 4: a hop, he aims, fires and lowers the gun | the same |
//! | Cid | 25, 26 / 29, 27 at 1 / 28 | 3 / 33 at 20 / 4 |
//!
//! The actor travels from the approach clip that follows the script's move to the target: a runner crouches or
//! winds up in place with the first clip (26, or 25 for Aerith and Cid) and travels with the second; a hop has
//! only the clips' own travel ([`party::HOP`]). Pictures: `attack-chain-<name>-<short|long>.png` (the clips of each, in order),
//! `attack-strip-<name>.png` and `attack-close-<name>.png` (the blow as the scene plays it, the whole fight and
//! a close-up), and `party-roles-overview-<n>.png` (every role of all nine), written by `sheets` (the ignored data-gated tests).
//!
//! ## The shared slots
//!
//! Found by drawing the slots of every character and looking at them, and held to the scripts above (the
//! real-data test `party_clips_agree_across_the_characters`):
//!
//! | slot | what it is | evidence |
//! |---|---|---|
//! | 0 | idle loop (14 to 20 frames) | script 0; the root stays at home |
//! | 1 | idle, badly hurt (kneeling, 15 to 20 frames) | script 1: Tifa kneels, Cloud crouches |
//! | 2 | victory (25 to 55 frames; ends in a held pose) | script 12: Cloud's sword flourish, Tifa's fist pump, Barret's gun arm |
//! | 3, 4 | hop forward, hop back (8 to 14 frames) | scripts 3 and 4 |
//! | 6 | dead, lying down (one frame) | the last frame of slot 18 for every character |
//! | 7 | run (6 frames, a loop) | script 13; in place: the root does not travel |
//! | 8 | guard (1 or 2 frames, a held pose) | script 9 (cover): Cloud raises an arm, Tifa boxes. **Played for Defend since the first version.** |
//! | 9 | a gesture with an arm raised (12 to 16 frames) | script 32: the item thrown up to an ally |
//! | 10 | the item thrown forward to an enemy (12 to 17 frames) | script 33: **not the attack** |
//! | 12, 13, 14 | cast: begin (28 to 33 frames), hold (4 to 8), end (4 to 11) | script 17: Cloud stows the sword and holds out a hand; Tifa raises an arm |
//! | 15 | hurt: flinch and straighten up (8 to 10 frames) | script 5: Cloud arches back, Barret doubles over |
//! | 18 | death: thrown back, lands, ends lying (8 to 14 frames) | script 7; ends in slot 6's pose for every character |
//!
//! **Not settled: the Defend command.** Script 8 plays slot 5 alone, a one-shot guard of 10 to 12 frames that
//! returns to the stance (Cloud holds his sword across, Tifa covers her face, Barret crosses his arms), and the
//! scripts around it are the reactions (hurt, death) and the cover: it may be what Defend plays. Slot 8, which
//! Defend holds here, is script 9's cover pose; for Tifa it is her boxing stance. Looked at, not changed.
//!
//! The root's x and z translation is removed from every clip played here (the figure plays IN PLACE, see
//! [`super::model`]): the game's clips carry travel of 500 to 1 400 units that the engine's own movement, which
//! this client does not have, composes with, and the choreography moves the actor itself (the lunge).
//!
//! # Enemies
//!
//! Slot 0 is the idle loop and slot 1 the hurt clip in every enemy model of the archive that has a
//! hurt clip at all (8 or 9 frames in the models looked at: the MP, Guard Hound, Sweeper, Kalm Fang,
//! Prowler, Mono Drive...). The attacks are the ones `scene.bin` names: attack `i` of an enemy record
//! has the id `attack_ids[i]` and plays slot `animation_indices[i]` ([`super::model::EnemyMotions`]);
//! `scene.bin`'s other slots are the enemy's own scripts, which are not decoded. **An enemy has no
//! death clip in the table**, so a beaten enemy plays its hurt clip and fades out (the engine of
//! the original fades it too).
//!
//! **Open finding, not fixed here: the numbers of `scene.bin`'s `animation_indices` are SCRIPT indices (of the
//! enemy's `ab` entry), not slots.** The MP's Machine Gun (index 3) is its script 3, which plays slots 2, 3, 4 and 5
//! in turn (the run-up, the two aiming poses held where it fires, the way back: the chain [`find_run`] and
//! [`find_back`] guess from the root's travel) and its Tonfa (index 4) is script 4: slots 6, 7, 8 and 9. What is
//! played for those attacks today is slot 3 and slot 4 alone, between that run-up and way back; the Tonfa's
//! swing (slot 8) is never played. The scripts are decoded now (`BattleModel::scripts`), so the same
//! [`Chain::from_script`] can give the enemies' blows the clips the game plays.
//!
//! The clips of an enemy's attack are mostly poses held where the attack happens (the MP's Machine
//! Gun is 2 frames, its Tonfa 1); the run-up and the way back are clips of their own that no table
//! names (the MP's slot 2 carries it 1 140 units forward, slot 5 brings it back, which is what
//! [`find_run`] and [`find_back`] look for: a slot whose root travels far one way, a slot whose root
//! travels the same distance back).

use fmv_ff7::battle_model::script::{code, Op, Script};

/// A slot of a model's pack of clips.
pub type Slot = u16;

/// How the scene sees a model's pack of clips: only what choosing and timing a clip needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipSet {
    /// Frames of each slot (at least 1).
    pub frames: Vec<u16>,
    /// Whether each slot holds the slot's own clip (`false`: a stand-in the loader made).
    pub ok: Vec<bool>,
    /// How far the root travels along the clip's `z`, last frame minus first, in battle units
    /// (negative is forward: models face `-z`).
    pub travel: Vec<f32>,
    /// Frames per second the clips play at.
    pub fps: f32,
}

impl ClipSet {
    /// The slot `slot` if the pack has it and it is the slot's own clip.
    pub fn usable(&self, slot: usize) -> Option<Slot> {
        (self.ok.get(slot).copied().unwrap_or(false) && self.frames.get(slot).is_some_and(|&n| n > 0)).then_some(slot as Slot)
    }

    /// The frames of `slot` (0 for a slot the pack does not have).
    pub fn frames_of(&self, slot: Slot) -> u16 {
        self.frames.get(usize::from(slot)).copied().unwrap_or(0)
    }
}

/// The slots the nine playable characters' packs share, and the indices of the animation scripts they all
/// answer to (see the module docs).
pub mod party {
    use super::Slot;

    pub const VICTORY: Slot = 2;
    pub const DEAD: Slot = 6;
    pub const RUN: Slot = 7;
    pub const DEFEND: Slot = 8;
    /// An item used on an ally: the gesture with the arm raised.
    pub const ITEM: Slot = 9;
    /// An item thrown at an enemy: the throw forward. (This is slot 10, which was taken for the attack.)
    pub const ITEM_AT_ENEMY: Slot = 10;
    pub const CAST: [Slot; 3] = [12, 13, 14];
    pub const HURT: Slot = 15;
    pub const DEATH: Slot = 18;
    pub const HOP_BACK: Slot = 4;
    /// How far the hop forward (slot 3) carries the actor, in battle units: the root of its last frame in all nine
    /// characters (-510 to -690, Cloud's -510): what a character that hops instead of running covers.
    pub const HOP: f32 = 515.0;

    /// The indices of the animation scripts of the nine models (`ab` entries) that the choreography reads.
    pub mod script {
        /// A blow with a short-range weapon: run up, strike, run back.
        pub const ATTACK: usize = 20;
        /// A blow with a long-range weapon: hop forward, shoot or throw, hop back.
        pub const SHOOT: usize = 21;
    }
}

/// A blow as the game's own animation script plays it, split where the choreography has to know: what the actor
/// plays while it closes in, what it strikes with and when the blow lands, how it comes back. Made from a
/// character's script ([`Chain::from_script`]), so Cloud's leap and chop, Tifa's dash and combo, Aerith's wind-up
/// and Vincent's hop and three-clip shot are each the character's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// The clips before the blow: a leap, a dash, a hop forward, a wind-up, each played once, in order.
    pub approach: Vec<Slot>,
    /// The clips the blow is struck with. The blow lands [`Chain::impact`] ticks after the first starts.
    pub strike: Vec<Slot>,
    /// Ticks (a clip of `n` frames lasts `n` ticks, 15 to the second) from the start of `strike[0]` to the damage
    /// marker of the script: the blow lands then.
    pub impact: u16,
    /// The clips that bring the actor back: a run back, a hop back.
    pub back: Vec<Slot>,
    /// Does the script send the actor to its target (a move to the target comes between its clips)? A hop only
    /// has the clips' own travel ([`party::HOP`]).
    pub closes_in: bool,
    /// The index of the clip of `approach` the actor starts to travel with: the one after the script's move to the
    /// target (a runner crouches or winds up in place first), 0 for a script that does not move.
    pub travel_from: usize,
}

/// Is `op` a move to the target (or to a place the script names) that the actor makes while it closes in?
fn is_move_to_target(op: u8) -> bool {
    matches!(op, code::MOVE_TARGET_X | code::MOVE_TARGET_XZ | code::MOVE_TARGET_XYZ | code::MOVE_TO | code::MOVE_TO_TARGET | code::PLACE_AT_TARGET)
}

impl Chain {
    /// The strike clip the blow lands in and the frame of it, for `clips` (Cloud's chop lands in its frame 1). (tests)
    #[cfg(test)]
    pub fn landing(&self, clips: &ClipSet) -> (Slot, u16) {
        let mut left = self.impact;
        for &slot in &self.strike {
            let n = clips.frames_of(slot);
            if left < n {
                return (slot, left);
            }
            left -= n;
        }
        let last = self.strike.last().copied().unwrap_or(0);
        (last, clips.frames_of(last).saturating_sub(1))
    }

    /// The blow `script` plays, or `None` when it is not one (it never marks the damage, plays nothing after the
    /// mark) or plays a clip `clips` cannot give (the slot is past the pack or not the slot's own clip): the
    /// choreography is then procedural, it never borrows another clip.
    ///
    /// The script's clips fall in three runs: up to the damage marker (`F7` or `C2`, whichever comes first) is the
    /// approach, from it up to the return (`FA`) is the strike, after it the way back. A script of the hop kind brings
    /// the actor back with the hop-back clip BEFORE its return, which is then the way back, not part of the strike.
    pub fn from_script(script: &Script, clips: &ClipSet) -> Option<Self> {
        let (mut approach, mut strike, mut back) = (Vec::new(), Vec::new(), Vec::new());
        let (mut impact, mut moved_at, mut returned) = (None, None, false);
        for op in &script.ops {
            match op {
                Op::Play(n) => {
                    let slot = Slot::from(*n);
                    if returned {
                        back.push(slot);
                    } else if impact.is_some() {
                        strike.push(slot);
                    } else {
                        approach.push(slot);
                    }
                }
                Op::Code { code, args } => match *code {
                    code::DAMAGE | code::DAMAGE_QUIET if impact.is_none() => impact = args.first().map(|&ticks| u16::from(ticks)),
                    code::RETURN => returned = true,
                    c if is_move_to_target(c) && impact.is_none() && moved_at.is_none() => moved_at = Some(approach.len()),
                    _ => {}
                },
            }
        }
        if back.is_empty() && strike.len() > 1 && strike.last() == Some(&party::HOP_BACK) {
            back.extend(strike.pop());
        }
        let playable = |slots: &[Slot]| slots.iter().all(|&s| clips.usable(usize::from(s)).is_some());
        if strike.is_empty() || !(playable(&approach) && playable(&strike) && playable(&back)) {
            return None;
        }
        let strike_ticks: u16 = strike.iter().map(|&s| clips.frames_of(s)).fold(0u16, u16::saturating_add);
        Some(Self { approach, strike, impact: impact?.min(strike_ticks), back, closes_in: moved_at.is_some(), travel_from: moved_at.unwrap_or(0) })
    }
}

/// Which clips play what for one actor. Every field is a slot that EXISTS and is the slot's own
/// clip, or `None`: the choreography uses what is here and is procedural for the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Kit {
    pub clips: ClipSet,
    /// Flinch.
    pub hurt: Option<Slot>,
    /// Death clip; ends in `dead`'s pose (party) or fades out (an enemy has none: it fades after its hurt clip).
    pub death: Option<Slot>,
    /// The pose a fallen party member stays in.
    pub dead: Option<Slot>,
    pub victory: Option<Slot>,
    pub defend: Option<Slot>,
    /// A party member's blow with a short-range weapon (the character's script 20): it runs up to its target.
    pub attack: Option<Chain>,
    /// A party member's blow with a long-range weapon (script 21): it hops forward, shoots or throws and hops back.
    pub shoot: Option<Chain>,
    /// An item used on an ally, and thrown at an enemy.
    pub item: Option<Slot>,
    pub item_at_enemy: Option<Slot>,
    /// Cast: begin, hold, end (each may be missing).
    pub cast: [Option<Slot>; 3],
    /// What the actor plays while it runs up to its target, and on the way back.
    pub run: Option<Slot>,
    pub back: Option<Slot>,
    /// An enemy's attacks: `scene.bin` attack id and its slot.
    pub attacks: Vec<(u16, Slot)>,
    /// The attacks `scene.bin` names whose slot cannot be played (it is past the model's pack, or not the
    /// slot's own clip): they move procedurally, they do NOT borrow another attack's clip.
    pub unplayable: Vec<u16>,
    /// The slot of an enemy's first attack: what a plain attack plays.
    pub plain: Option<Slot>,
}

/// What an enemy plays for an attack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttackClip {
    /// The clip `scene.bin` names for it.
    Slot(Slot),
    /// `scene.bin` names it but the clip cannot be played: the procedural motion.
    Procedural,
}

impl Kit {
    /// Nothing of its own: the actor moves procedurally (a stand-in, a box).
    pub fn none() -> Self {
        Self::default()
    }

    /// The kit of a playable character's battle model: the slots the nine share, and the character's own blows read
    /// from `scripts` (its animation scripts, indexed as the game's table: none, and the blows are procedural).
    pub fn party(clips: ClipSet, scripts: &[Script]) -> Self {
        let slot = |s: Slot| clips.usable(usize::from(s));
        let chain = |index: usize| scripts.get(index).and_then(|script| Chain::from_script(script, &clips));
        Self {
            hurt: slot(party::HURT),
            death: slot(party::DEATH),
            dead: slot(party::DEAD),
            victory: slot(party::VICTORY),
            defend: slot(party::DEFEND),
            attack: chain(party::script::ATTACK),
            shoot: chain(party::script::SHOOT),
            item: slot(party::ITEM),
            item_at_enemy: slot(party::ITEM_AT_ENEMY),
            cast: [slot(party::CAST[0]), slot(party::CAST[1]), slot(party::CAST[2])],
            run: slot(party::RUN),
            back: slot(party::HOP_BACK),
            clips,
            ..Self::default()
        }
    }

    /// The blow a party member makes: the one of its short-range script when `melee`, of its long-range script
    /// otherwise, and the other one if the character has only that one (never a clip that is not a blow).
    pub fn blow(&self, melee: bool) -> Option<&Chain> {
        if melee {
            self.attack.as_ref().or(self.shoot.as_ref())
        } else {
            self.shoot.as_ref().or(self.attack.as_ref())
        }
    }

    /// The kit of an enemy's battle model, with the attacks `scene.bin` names for it (`attacks`: attack
    /// id and slot; one that points at a slot the model cannot play is left out).
    pub fn enemy(clips: ClipSet, attacks: &[(u16, u8)]) -> Self {
        let unplayable: Vec<u16> = attacks.iter().filter(|&&(_, slot)| clips.usable(usize::from(slot)).is_none()).map(|&(id, _)| id).collect();
        let attacks: Vec<(u16, Slot)> = attacks.iter().filter_map(|&(id, slot)| clips.usable(usize::from(slot)).map(|s| (id, s))).collect();
        let run = find_run(&clips, &attacks);
        Self {
            hurt: clips.usable(1),
            back: run.and_then(|r| find_back(&clips, r, &attacks)),
            run,
            plain: attacks.first().map(|&(_, s)| s),
            attacks,
            unplayable,
            clips,
            ..Self::default()
        }
    }

    /// What the enemy attack `id` plays: its own clip, or `Procedural` when `scene.bin` names the attack
    /// with a slot that cannot be played. `None` for an id `scene.bin` does not name for this enemy at all
    /// (a plain attack's clip is [`Kit::plain`]).
    pub fn attack_clip(&self, id: u16) -> Option<AttackClip> {
        if self.unplayable.contains(&id) {
            return Some(AttackClip::Procedural);
        }
        self.attacks.iter().find(|&&(a, _)| a == id).map(|&(_, s)| AttackClip::Slot(s))
    }

    /// Does this kit play any real clip at all? (tests)
    #[cfg(test)]
    pub fn is_real(&self) -> bool {
        self.clips.fps > 0.0 && !self.clips.frames.is_empty()
    }
}

/// How far a slot's root has to travel to count as a run-up or a way back, in battle units.
pub const RUN_TRAVEL: f32 = 500.0;
/// Fewest frames of a run-up or way back (the 1 and 2 frame clips are held poses).
const MIN_RUN_FRAMES: u16 = 4;

/// The slot an enemy runs up to its target with: the one (from slot 2 on, so neither the idle nor
/// the hurt clip, and none of the attacks) whose root travels the farthest forward, at least
/// [`RUN_TRAVEL`]. The MP's slot 2 (-1 139 units). `None` for an enemy that has no such clip: it
/// slides to its target in its idle pose.
pub fn find_run(set: &ClipSet, attacks: &[(u16, Slot)]) -> Option<Slot> {
    (2..set.frames.len())
        .filter(|&s| set.usable(s).is_some() && set.frames[s] >= MIN_RUN_FRAMES && !attacks.iter().any(|&(_, a)| usize::from(a) == s))
        .filter(|&s| set.travel.get(s).is_some_and(|&t| t <= -RUN_TRAVEL))
        .min_by(|&a, &b| set.travel[a].total_cmp(&set.travel[b]))
        .map(|s| s as Slot)
}

/// The slot an enemy comes back with after a run-up `run`: the first one after it whose root travels
/// back, at least [`RUN_TRAVEL`] and within a fifth of what the run-up covered. The MP's slot 5.
pub fn find_back(set: &ClipSet, run: Slot, attacks: &[(u16, Slot)]) -> Option<Slot> {
    let out = -set.travel.get(usize::from(run)).copied()?;
    if out < RUN_TRAVEL {
        return None;
    }
    ((usize::from(run) + 1)..set.frames.len())
        .filter(|&s| set.usable(s).is_some() && set.frames[s] >= MIN_RUN_FRAMES && !attacks.iter().any(|&(_, a)| usize::from(a) == s))
        .find(|&s| set.travel.get(s).is_some_and(|&t| t >= RUN_TRAVEL && (t - out).abs() <= out * 0.2))
        .map(|s| s as Slot)
}

/// Invented packs and scripts shaped like Cloud's, for the tests of the choreography: the slot numbers and clip
/// lengths of his blow are the structure the docs above state, and everything else (the move distance, the damage
/// ticks, which cosmetic opcodes there are) is made up. No bytes of the owner's files.
#[cfg(test)]
pub mod testing {
    use super::{ClipSet, Script};
    use fmv_ff7::battle_model::script::decode;

    /// A 52-slot pack whose clips have the lengths of Cloud's chain (26: 2 frames, 27: 5, 30: 6, 28: 3, 29: 4, the hops
    /// 12 to 14, the long-range swing 31) and 15 frames everywhere else, at 15 frames a second.
    pub fn cloud_like_pack() -> ClipSet {
        let mut frames = vec![15u16; 52];
        for (slot, n) in [(26, 2), (27, 5), (30, 6), (28, 3), (29, 4), (3, 14), (4, 12), (31, 31)] {
            frames[slot] = n;
        }
        ClipSet { frames, ok: vec![true; 52], travel: vec![0.0; 52], fps: 15.0 }
    }

    /// The decoded script of invented `bytes`.
    pub fn script(bytes: &[u8]) -> Script {
        decode(bytes, 0).expect("the script decodes")
    }

    /// Invented scripts of Cloud's shape: 20 runs up (26, a move to the target of a made-up distance, 27), strikes (a
    /// damage marker 2 ticks into 30, then 28) and runs back (a return, then 29); 21 hops forward (3), shoots (a marker
    /// 12 ticks into 31) and hops back (4).
    pub fn cloud_like_scripts() -> Vec<Script> {
        let mut scripts = vec![Script::default(); 22];
        scripts[20] = script(&[26, 0xD1, 0x58, 0x02, 0x00, 0x00, 0x03, 27, 0xF7, 2, 30, 28, 0xFA, 29, 0xEE]);
        scripts[21] = script(&[3, 0xF7, 12, 31, 4, 0xFA, 0xEE]);
        scripts
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{cloud_like_pack, cloud_like_scripts, script};
    use super::*;

    /// A pack of `n` slots with `frames` frames each, all of them real, none travelling.
    fn pack(n: usize, frames: u16) -> ClipSet {
        ClipSet { frames: vec![frames; n], ok: vec![true; n], travel: vec![0.0; n], fps: 15.0 }
    }

    #[test]
    fn a_slot_is_usable_only_if_it_exists_and_is_the_slots_own_clip() {
        let mut set = pack(4, 6);
        set.ok[2] = false;
        assert_eq!(set.usable(1), Some(1));
        assert_eq!((set.usable(2), set.usable(4), set.usable(99)), (None, None, None));
        assert_eq!(set.frames_of(3), 6);
        assert_eq!(set.frames_of(40), 0);
        // An empty clip is not one.
        set.frames[0] = 0;
        assert_eq!(set.usable(0), None);
    }

    #[test]
    fn a_party_member_has_the_shared_slots_and_its_blows_from_its_scripts_and_loses_what_its_pack_cannot_play() {
        let scripts = cloud_like_scripts();
        let kit = Kit::party(cloud_like_pack(), &scripts);
        assert_eq!((kit.hurt, kit.death, kit.dead, kit.victory, kit.defend), (Some(15), Some(18), Some(6), Some(2), Some(8)));
        assert_eq!((kit.item, kit.item_at_enemy, kit.run, kit.back), (Some(9), Some(10), Some(7), Some(4)));
        assert_eq!(kit.cast, [Some(12), Some(13), Some(14)]);
        assert!(kit.is_real() && kit.attacks.is_empty());
        // The blow of a short-range weapon: a crouch (26), the move to the target, the run (27), the chop that lands two
        // ticks in (30), the recovery (28), the way back (29).
        let attack = kit.attack.as_ref().expect("a blow");
        assert_eq!((attack.approach.as_slice(), attack.strike.as_slice(), attack.impact, attack.back.as_slice()), (&[26, 27][..], &[30, 28][..], 2, &[29][..]));
        assert!(attack.closes_in && attack.travel_from == 1, "the actor travels from the clip after the move to the target");
        // The long-range one: hop forward, shoot (the shot lands 12 ticks in), hop back; the hop is the clips' own travel.
        let shoot = kit.shoot.as_ref().expect("a shot");
        assert_eq!((shoot.approach.as_slice(), shoot.strike.as_slice(), shoot.impact, shoot.back.as_slice()), (&[3][..], &[31][..], 12, &[4][..]));
        assert!(!shoot.closes_in && shoot.travel_from == 0);
        // `melee` picks the short-range script, anything else the long-range one.
        assert_eq!((kit.blow(true), kit.blow(false)), (kit.attack.as_ref(), kit.shoot.as_ref()));
        // A character that has only one of them uses it for both.
        let only_short = Kit { shoot: None, ..kit.clone() };
        assert_eq!(only_short.blow(false), only_short.attack.as_ref());
        // No scripts, no blows: the choreography is procedural, and slot 10 (an item thrown at an enemy) is never one.
        let bare = Kit::party(cloud_like_pack(), &[]);
        assert!(bare.attack.is_none() && bare.shoot.is_none() && bare.blow(true).is_none());
        // A pack that ends at slot 12, whose slot 12 is a stand-in: what is missing or not real is None, and a script
        // that plays clips past the pack is no blow.
        let mut set = pack(13, 10);
        set.ok[12] = false;
        let kit = Kit::party(set, &scripts);
        assert_eq!(kit.cast, [None, None, None], "slot 12 is not the slot's own clip, 13 and 14 are not there");
        assert_eq!(kit.hurt, None, "slot 15 is beyond the pack");
        assert!(kit.attack.is_none() && kit.shoot.is_none(), "26 and 31 are beyond the pack");
        // A model with nothing is procedural throughout.
        assert!(!Kit::none().is_real());
        assert_eq!(Kit::none().hurt, None);
    }

    /// Scripts of the shapes the characters' real ones have (a dash and a run, a wind-up and a leap, a hop and a
    /// three-clip shot), made up: the clip numbers are the structure, the moves, ticks and opcodes around them are invented.
    #[test]
    fn a_chain_is_read_from_the_script_whatever_its_shape() {
        let set = cloud_like_pack();
        // A dash (26), a run cycle (27) after the move, two hits scheduled (`BE`) and the damage readout 7 ticks on (`C2`:
        // the number), the punches (30), the recovery (28), the way back (29): the readout is the impact.
        let dash = script(&[26, 0xD1, 0x20, 0x01, 0x00, 0x00, 0x04, 27, 0xBE, 3, 0xBE, 7, 0xC2, 7, 30, 28, 0xFA, 29, 0xEE]);
        let chain = Chain::from_script(&dash, &set).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.impact, chain.back.clone()), (vec![26, 27], vec![30, 28], 7, vec![29]), "the readout is 7 ticks in: inside the second strike clip (30 has 6 frames, 28 has 3)");
        // A wind-up in place (25), the move comes after it, then the leap (26): the actor travels from 26.
        let leap = script(&[25, 0xD0, 0x20, 0x01, 0x05, 26, 0xF7, 3, 29, 27, 0xFA, 28, 0xEE]);
        let chain = Chain::from_script(&leap, &set).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.back.clone()), (vec![25, 26], vec![29, 27], vec![28]));
        assert!(chain.closes_in && chain.travel_from == 1, "it winds up in place first");
        // A hop, a three-clip shot that lands 5 ticks in, and the hop back before the return op: the hop back is the way back,
        // not part of the strike. No move: the clips' own travel.
        let shot = script(&[3, 0xF7, 5, 30, 31, 32, 4, 0xFA, 0xEE]);
        let chain = Chain::from_script(&shot, &set).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.impact, chain.back.clone()), (vec![3], vec![30, 31, 32], 5, vec![4]));
        assert!(!chain.closes_in && chain.travel_from == 0);
        // The marker can sit past what the strike clips hold: it lands on their last frame.
        let late = script(&[3, 0xF7, 200, 31, 4, 0xFA, 0xEE]);
        assert_eq!(Chain::from_script(&late, &set).expect("a blow").impact, 31);
        // Not blows: no damage marker anywhere, nothing played after the marker, and a clip the pack cannot give.
        assert_eq!(Chain::from_script(&script(&[3, 4, 0xFA, 0xEE]), &set), None, "nothing says when it lands");
        assert_eq!(Chain::from_script(&script(&[3, 0xF7, 4, 0xFA, 0xEE]), &set), None, "nothing is struck with");
        assert_eq!(Chain::from_script(&Script::default(), &set), None);
        let mut broken = cloud_like_pack();
        broken.ok[31] = false;
        assert_eq!(Chain::from_script(&script(&[3, 0xF7, 4, 31, 4, 0xFA, 0xEE]), &broken), None, "slot 31 is not the slot's own clip");
        assert_eq!(Chain::from_script(&script(&[3, 0xF7, 4, 60, 4, 0xFA, 0xEE]), &set), None, "slot 60 is past the pack");
    }

    /// The MP: idle, hurt, a run-up of 1 139 units (slot 2), two attack poses held there (3 and 4), a way
    /// back (5), and clips of its own tonfa sequence that no attack names.
    fn the_mp() -> ClipSet {
        let frames = [15u16, 9, 12, 2, 1, 21, 12, 4, 12, 2];
        let mut travel = vec![0.0; frames.len()];
        travel[2] = -1139.0;
        travel[5] = 1145.0;
        travel[8] = -300.0;
        ClipSet { frames: frames.to_vec(), ok: vec![true; frames.len()], travel, fps: 15.0 }
    }

    #[test]
    fn the_mps_chain_is_found_run_up_attacks_and_way_back() {
        // Attack 272 (Machine Gun) plays slot 3, 273 (Tonfa) slot 4: scene.bin's words.
        let kit = Kit::enemy(the_mp(), &[(272, 3), (273, 4)]);
        assert_eq!(kit.hurt, Some(1));
        assert_eq!((kit.run, kit.back), (Some(2), Some(5)), "the run-up carries it forward, the way back returns it");
        assert_eq!((kit.attack_clip(272), kit.attack_clip(273), kit.attack_clip(999)), (Some(AttackClip::Slot(3)), Some(AttackClip::Slot(4)), None));
        assert_eq!(kit.plain, Some(3), "a plain attack plays the first of its attacks");
        assert_eq!((kit.death, kit.victory, kit.attack), (None, None, None), "an enemy has no death clip: it fades");
    }

    #[test]
    fn an_attack_that_points_past_the_pack_or_at_a_stand_in_is_left_out() {
        let mut set = the_mp();
        set.ok[4] = false;
        let kit = Kit::enemy(set, &[(272, 3), (273, 4), (274, 40), (275, 255)]);
        assert_eq!(kit.attacks, [(272, 3)], "slot 4 is not the slot's own clip, 40 and 255 are not in the pack");
        // What the record names but cannot be played is kept apart: procedural, never another attack's clip.
        assert_eq!(kit.unplayable, [273, 274, 275]);
        assert_eq!((kit.attack_clip(273), kit.attack_clip(275), kit.attack_clip(272), kit.attack_clip(9999)), (Some(AttackClip::Procedural), Some(AttackClip::Procedural), Some(AttackClip::Slot(3)), None));
    }

    #[test]
    fn no_travelling_clip_means_no_run_up_and_the_run_up_is_never_an_attack() {
        // A hound that leaps in place: no slot travels, so it slides in its idle pose.
        let kit = Kit::enemy(pack(8, 8), &[(274, 3), (275, 4)]);
        assert_eq!((kit.run, kit.back), (None, None));
        // The run-up must not be the idle or the hurt clip, and must not be an attack's own slot.
        let mut set = pack(8, 8);
        set.travel[0] = -900.0;
        set.travel[1] = -900.0;
        set.travel[3] = -900.0;
        assert_eq!(find_run(&set, &[(1, 3)]), None);
        set.travel[5] = -700.0;
        assert_eq!(find_run(&set, &[(1, 3)]), Some(5));
        // Short clips are held poses, not runs.
        set.frames[5] = 2;
        assert_eq!(find_run(&set, &[(1, 3)]), None);
    }

    #[test]
    fn the_way_back_is_the_slot_after_the_run_up_that_covers_the_same_ground_the_other_way() {
        let mut set = pack(9, 8);
        set.travel[2] = -1000.0;
        set.travel[4] = 600.0; // too short a way back
        set.travel[6] = 1050.0;
        set.travel[7] = 1000.0;
        assert_eq!(find_back(&set, 2, &[]), Some(6));
        assert_eq!(find_back(&set, 2, &[(1, 6)]), Some(7), "an attack's own slot is not a way back");
        assert_eq!(find_back(&set, 6, &[]), None, "nothing after slot 6 returns what slot 6 covered (it covered none)");
    }
}
