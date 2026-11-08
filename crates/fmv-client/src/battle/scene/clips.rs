//! Which clip of a battle model plays what: the knowledge about the owner's `battle.lgp` animation
//! slots that the choreography ([`super::plan`], [`super::motion`]) reads.
//!
//! A real battle model has a pack of clips, the **slots**, numbered as the game numbers them
//! (`fmv_ff7::battle_model::BattleModel::animations`); a slot that does not decode or does not
//! exist (`slot_ok` false, or past the pack) is never played: whoever asks for it gets `None` and
//! falls back to the procedural motion (lunge, flinch, collapse) of [`super::motion`]. A model that
//! is not a real battle model (a field stand-in, a box) has [`Kit::none`]: everything procedural.
//!
//! # The playable characters
//!
//! All nine character models (Cloud, Barret, Tifa, Aerith, Red XIII, Yuffie, Cait Sith, Vincent,
//! Cid) share one table of the first slots. **Found by drawing the slots of every character and
//! looking at them** (the ignored test `sheets::party_slot_evidence_sheets` writes the pictures; the
//! statistics of every slot agree for the three characters it was also checked on by numbers):
//!
//! | slot | what it is | evidence |
//! |---|---|---|
//! | 0 | idle loop (14 to 20 frames) | the pose the game shows; the root stays at home |
//! | 1 | idle, badly hurt (kneeling, 15 to 20 frames) | Tifa kneels, Cloud crouches |
//! | 2 | victory (25 to 55 frames; ends in a held pose) | Cloud's sword flourish, Tifa's fist pump, Barret's gun arm |
//! | 3, 4 | hop forward, hop back (8 to 14 frames) | the root travels about -515 / +515 along z in every character |
//! | 6 | dead, lying down (one frame) | the last frame of slot 18 for every character |
//! | 7 | run (6 frames, a loop) | in place: the root does not travel |
//! | 8 | defend (1 or 2 frames, a held guard) | Cloud and Tifa cross their arms |
//! | 9 | a self-contained gesture with an arm raised (12 to 16 frames) | used for Item; the ROLE IS UNVERIFIED |
//! | 10 | the attack: wind up, step in, strike, recover (12 to 17 frames) | Cloud's slash, Tifa's punches, Barret's punch, Aerith's staff swing, Red XIII's lunge, Yuffie's throw-kick, Cid's spear jab |
//! | 12, 13, 14 | cast: begin (28 to 33 frames), hold (4 to 8), end (4 to 11) | Cloud stows the sword and holds out a hand; Tifa raises an arm |
//! | 15 | hurt: flinch and straighten up (8 to 10 frames) | Cloud arches back, Barret doubles over |
//! | 18 | death: thrown back, lands, ends lying (8 to 14 frames) | ends in slot 6's pose for every character |
//!
//! Slots 19 and up are the weapon-specific attacks and limit breaks; they are not used. The root's
//! x and z translation is removed from every clip played here (the figure plays IN PLACE, see
//! [`super::model`]): the game's clips carry travel of 500 to 1 400 units that the engine's own
//! movement, which this client does not have, composes with, and the choreography moves the actor
//! itself (the lunge).
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
//! The clips of an enemy's attack are mostly poses held where the attack happens (the MP's Machine
//! Gun is 2 frames, its Tonfa 1); the run-up and the way back are clips of their own that no table
//! names (the MP's slot 2 carries it 1 140 units forward, slot 5 brings it back, which is what
//! [`find_run`] and [`find_back`] look for: a slot whose root travels far one way, a slot whose root
//! travels the same distance back).

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

/// The slots of the playable characters' models (see the module docs).
pub mod party {
    use super::Slot;

    pub const VICTORY: Slot = 2;
    pub const DEAD: Slot = 6;
    pub const RUN: Slot = 7;
    pub const DEFEND: Slot = 8;
    pub const ITEM: Slot = 9;
    pub const ATTACK: Slot = 10;
    pub const CAST: [Slot; 3] = [12, 13, 14];
    pub const HURT: Slot = 15;
    pub const DEATH: Slot = 18;
    pub const HOP_BACK: Slot = 4;
    /// The frame of the attack clip at which the blow lands, as a fraction of its length: the swing's
    /// apex in the pictures of every character (Cloud 9 of 15, Tifa 7 of 12, Barret 7 of 16, Aerith 9 of 17).
    pub const ATTACK_IMPACT: f32 = 0.55;
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
    /// A party member's attack.
    pub attack: Option<Slot>,
    pub item: Option<Slot>,
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

    /// The kit of a playable character's battle model.
    pub fn party(clips: ClipSet) -> Self {
        let slot = |s: Slot| clips.usable(usize::from(s));
        Self {
            hurt: slot(party::HURT),
            death: slot(party::DEATH),
            dead: slot(party::DEAD),
            victory: slot(party::VICTORY),
            defend: slot(party::DEFEND),
            attack: slot(party::ATTACK),
            item: slot(party::ITEM),
            cast: [slot(party::CAST[0]), slot(party::CAST[1]), slot(party::CAST[2])],
            run: slot(party::RUN),
            back: slot(party::HOP_BACK),
            clips,
            ..Self::default()
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

#[cfg(test)]
mod tests {
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
    fn a_party_member_has_the_shared_table_and_loses_what_its_pack_cannot_play() {
        let kit = Kit::party(pack(52, 10));
        assert_eq!((kit.hurt, kit.death, kit.dead, kit.victory, kit.defend), (Some(15), Some(18), Some(6), Some(2), Some(8)));
        assert_eq!((kit.attack, kit.item, kit.run, kit.back), (Some(10), Some(9), Some(7), Some(4)));
        assert_eq!(kit.cast, [Some(12), Some(13), Some(14)]);
        assert!(kit.is_real() && kit.attacks.is_empty());
        // A pack that ends at slot 12, whose slot 12 is a stand-in: what is missing or not real is None.
        let mut set = pack(13, 10);
        set.ok[12] = false;
        let kit = Kit::party(set);
        assert_eq!(kit.cast, [None, None, None], "slot 12 is not the slot's own clip, 13 and 14 are not there");
        assert_eq!(kit.hurt, None, "slot 15 is beyond the pack");
        assert_eq!(kit.attack, Some(10));
        // A model with nothing is procedural throughout.
        assert!(!Kit::none().is_real());
        assert_eq!(Kit::none().hurt, None);
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
