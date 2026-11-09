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
//! | 8 | **the Defend command**: a one-shot guard that returns to the stance | 5 alone (10 to 12 frames) |
//! | 9 | cover: step in front of an ally and hold the guard | 8, a held guard (1 frame) |
//! | 12 | victory | 2 (25 to 55 frames, ends in a held pose) |
//! | 13 | run (the party running away) | 7 (6 frames, a loop, in place) |
//! | 17 | cast a spell | 3, then 12 (begin), 13 (hold), the effect (`EC`), 14 (end), then 4 |
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
//! | 5 | guard: raises the guard and returns to the stance (10 to 12 frames) | script 8, the Defend command: Cloud brings his sword across, Tifa covers her face, Barret crosses his arms. **Played for Defend.** |
//! | 8 | cover pose (1 or 2 frames, a held pose) | script 9 (cover, the support materia: a move to the ally, this pose, a wait, the return): Cloud crouches behind the sword, Tifa boxes. Not played: Defend is no cover |
//! | 9 | a gesture with an arm raised (12 to 16 frames) | script 32: the item thrown up to an ally |
//! | 10 | the item thrown forward to an enemy (12 to 17 frames) | script 33: **not the attack** |
//! | 12, 13, 14 | cast: begin (28 to 33 frames), hold (4 to 8), end (4 to 11) | script 17: Cloud stows the sword and holds out a hand; Tifa raises an arm |
//! | 15 | hurt: flinch and straighten up (8 to 10 frames) | script 5: Cloud arches back, Barret doubles over |
//! | 18 | death: thrown back, lands, ends lying (8 to 14 frames) | script 7; ends in slot 6's pose for every character |
//!
//! **The Defend command is script 8's guard.** The first version held slot 8 for a second and a half and snapped to
//! the stance; slot 8 is script 9's COVER pose (a move to an ally, this pose, a wait, the return: the support
//! materia, which this client does not play). Script 8, which sits among the reactions (5 hurt, 7 death) and the
//! cover (9), plays slot 5 alone for all nine, a guard that goes up and comes back to the stance in 10 to 12 frames:
//! looked at on the pictures of every character (`slots-<name>-0.png`: Cloud brings the Buster Sword across his
//! body, Tifa crouches with her fists at her face) and played once at its own speed. Every character's weapon has a
//! real track for slot 5 (held by `party_clips_agree_across_the_characters`). In the scene: `defend-close-<n>.png`
//! of `sheets::fight_defend_sheets`, before and after.
//!
//! **The hops around a cast are not played.** Script 17 hops forward (3: 8 to 14 frames, the root carries the actor
//! hundreds of units), gathers (12, 13), runs the effect (`EC`), ends (14) and hops back (4). The plan can only cover
//! [`super::plan::MAX_SPEEDUP`] x [`super::cue::MAX_IMPACT_DELAY`] of natural time before the impact (27 ticks), and the
//! gathering alone (36 ticks for Cloud, never fewer than 32) is longer: [`super::plan::fit`] drops the START of a plan that is
//! too long, so a hop forward put before the gathering would be the part cut, every time; and a hop back with no hop
//! forward is a jump in place. The arithmetic is held to the owner's files by `party_clips_agree_across_the_characters`;
//! the cast stays the gathering clips (begin to the impact, hold, end).
//!
//! The root's x and z translation is removed from every clip played here (the figure plays IN PLACE, see
//! [`super::model`]): the game's clips carry travel of 500 to 1 400 units that the engine's own movement, which
//! this client does not have, composes with, and the choreography moves the actor itself (the lunge).
//!
//! # Enemies: the numbers of `scene.bin` are scripts
//!
//! **An enemy model plays what its own scripts say, like a character.** Attack `i` of an enemy record has the id
//! `attack_ids[i]` and `animation_indices[i]` is the index of a SCRIPT of the enemy's `ab` entry, not a slot of its pack
//! ([`super::model::EnemyBook`]): the MP's Machine Gun (index 3) is its script 3, which plays slots 2, 3, 4 and 5 in turn, and
//! its Tonfa (index 4) is script 4: slots 6, 7, 8 and 9. The first version played slot 3 and slot 4 for them (slot 4,
//! one frame of the machine gun's aim, for the baton) and never the swing (slot 8), and guessed a run-up and a way back
//! from the root's travel; the Guard Hound's bite ran with a spell's first clip. Every enemy model has 32 scripts and the
//! first ones are the same for all:
//!
//! | script | what it is | |
//! |---|---|---|
//! | 0 | idle | `A9 C9 00 C1`: slot 0, over and over |
//! | 1 | hurt | slot 1 in 326 of the 329 enemies that have a name (two of them play another clip after it), then `F6` (the engine's death effect, if it died); the other three play nothing of their own |
//! | 2 | death | a clip in 45 of the 329 (slot 10 in 23 of them, slot 14 in 12, the Guard Hound's among them: a fall), none in the rest (`HOME IDLE`: the engine takes them apart) |
//! | 3 and up | the attacks `scene.bin` names | below |
//!
//! So the hurt clip is script 1's ([`Kit::hurt`]; an enemy whose script plays none flinches procedurally) and a death clip, when
//! there is one, script 2's ([`Kit::death`]): an enemy that has one plays it from the start of its death and fades when the fall
//! has been seen; the others flinch and fade as before ([`super::motion::DEATH_FALL_MAX`]). Held by
//! `sheets::enemy_attack_chains_are_the_scripts_clips`.
//!
//! ## An attack
//!
//! An attack is read by [`Chain::from_script`] the way a character's blow is: the clips before the **mark** are the approach, the
//! clips from it on the strike up to the return (`FA`), the clips after the return the way back. The mark is the damage (`F7`, or
//! `C2` for the number alone, a number of ticks after the op: the same 15 ticks to the second as a character's) or the effect (`EC`:
//! a spell, an enemy's special; it lands at once, at the first clip after it). Of the 819 attacks of the named enemies that the
//! engine can use (it models the damaging ones) 795 have such a script and play it; the other 24 (13 never mark where they land, 11
//! play nothing after the mark: bosses' and the like) are the procedural motion, and **never borrow another attack's clips**. The tally and the chains of the
//! MP, the Guard Hound and the Mono Drive are held to the owner's files by `sheets::enemy_attack_chains_are_the_scripts_clips`.
//!
//! | shape | the 795 attacks that play | example |
//! |---|---|---|
//! | `approach / strike @n / way back`: a damage mark, a return | 362 | the MP's Tonfa: 6, 7 / 8 @4 / 9; the Guard Hound's Bite: 2, 3 / 4 @15 / 5; its Tentacle: 6, 7 / 8 @5 / 9 |
//! | `approach / strike @n`: a damage mark, no return, the last clip carries the actor home | 79 | the MP's Machine Gun: 2 / 3, 4, 5 @8: the run-up (slot 2 carries it 970 units), two poses and a long clip that keeps the aim for ten frames and runs home |
//! | `approach / strike @0`: an effect | 354 | the Mono Drive's Fire: 6, 7, 8 / 9 @0: it gathers, the effect, the end clip (the tentacles open into a star and close) |
//!
//! The script does not move the actor on its own, but it says how: a **move to the target** before the mark (`D1`, `D0`,
//! `CF`: `FB` only places the actor, and does not count) sends the actor all the way to its target; a script without one
//! only carries it as far as its clips do ([`Chain::carries`]: the forward travel of the root of the approach clips, 970 units for
//! the MP's Machine Gun, which is fired from where the run-up stopped; none for a spell, which is cast where it stands). The actor
//! travels with the first clip that carries it forward or the one after the move, whichever comes first ([`Chain::travel_from`]: the
//! MP's leap, whose root jumps forward 581 units, then the move that puts it next to the target). A script with no return whose last
//! clip carries the actor home holds its pose (the MP's aim, for the first ten frames of the clip) and goes
//! when the root does: [`Chain::home_run`] is the frame ([`ClipSet::lead`]). Every clip plays once; the idle clip (slot 0: the engine lets the
//! idle loop finish before an action and settles after it) is not played. **Nothing here changes what the presentation rules say**
//! ([`super::plan`]): the impact is at `arrival` + 0.55 s whatever the clips, the follow-through and the way back are bounded.
//!
//! Pictures: `enemy-chain-<name>.png` (the clips of each in order, labelled), `enemy-attack-<name>-wide.png` and
//! `-close-0.png` (what the scene plays), `enemy-death-<name>-close-0.png`, written by `sheets` (the ignored data-gated tests), before
//! and after.

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
    /// How many frames of each clip go by before its root has covered a tenth of its travel (0 for a clip that does
    /// not travel, or one that sets off at once): where a clip that carries the actor home begins to.
    pub lead: Vec<u16>,
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

    /// How far the root of `slot` travels along `z` (0 for a slot the pack does not have): negative is forward.
    pub fn travel_of(&self, slot: Slot) -> f32 {
        self.travel.get(usize::from(slot)).copied().unwrap_or(0.0)
    }

    /// The frames of `slot` before its root has covered a tenth of its travel (0 for a slot the pack does not have).
    pub fn lead_of(&self, slot: Slot) -> u16 {
        self.lead.get(usize::from(slot)).copied().unwrap_or(0)
    }
}

/// The slots the nine playable characters' packs share, and the indices of the animation scripts they all
/// answer to (see the module docs).
pub mod party {
    use super::Slot;

    pub const VICTORY: Slot = 2;
    pub const DEAD: Slot = 6;
    pub const RUN: Slot = 7;
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
        /// The Defend command: a one-shot guard that returns to the stance.
        pub const DEFEND: usize = 8;
        /// A blow with a short-range weapon: run up, strike, run back.
        pub const ATTACK: usize = 20;
        /// A blow with a long-range weapon: hop forward, shoot or throw, hop back.
        pub const SHOOT: usize = 21;
    }
}

/// The indices of the animation scripts every enemy model has (its `ab` entry, `scene.bin`'s `animation_indices` name the
/// others: an attack's script).
pub mod enemy_script {
    /// What the enemy plays when it is hit: its hurt clip.
    pub const HURT: usize = 1;
    /// What it plays when it dies, if it plays anything (an enemy without a death clip is taken apart by the engine's own
    /// effect, which this client draws as a fade).
    pub const DEATH: usize = 2;
}

/// A blow, a spell, any attack as the game's own animation script plays it, split where the choreography has to know:
/// what the actor plays while it closes in, what it strikes with and when the blow lands, how it comes back. Made from the
/// script of a character or of an enemy ([`Chain::from_script`]), so Cloud's leap and chop, Tifa's dash and combo, Aerith's
/// wind-up, the MP's baton swing and the Guard Hound's bite are each the model's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// The clips before the blow: a leap, a dash, a hop forward, a wind-up, a spell's gathering, each played once, in order.
    pub approach: Vec<Slot>,
    /// The clips the blow is struck with. The blow lands [`Chain::impact`] ticks after the first starts.
    pub strike: Vec<Slot>,
    /// Ticks (a clip of `n` frames lasts `n` ticks, 15 to the second) from the start of `strike[0]` to the damage
    /// marker of the script (an effect, `EC`, lands at once: 0): the blow lands then.
    pub impact: u16,
    /// The clips that bring the actor back: a run back, a hop back.
    pub back: Vec<Slot>,
    /// Does the script send the actor to its target (a move to the target comes before the blow)? One that does
    /// not only has the clips' own travel ([`Chain::carries`]; for a character that hops, [`party::HOP`]).
    pub closes_in: bool,
    /// The index of the clip of `approach` the actor starts to travel with: the first that carries it forward or the one
    /// after the script's move to the target, whichever comes first (a runner crouches or winds up in place first), 0 for a
    /// script that does not move.
    pub travel_from: usize,
    /// How far the clips of `approach` carry the actor forward by themselves, in battle units (the root's travel of those
    /// that go forward): all that a script that does not move it to the target covers.
    pub carries: u32,
    /// The last clip of `strike` carries the actor home (nothing in the script sends it back): from this frame of it on, where
    /// its root has begun to travel. `None` for a script that sends the actor back or whose clips do not.
    pub home_run: Option<u16>,
}

/// How far forward a clip's root has to travel to count as carrying the actor (a run, a leap, a hop), and how far back for
/// the clip that brings it home, in battle units.
pub const CARRY_TRAVEL: f32 = 250.0;

/// Whose script a [`Chain`] is read from: the two are read by different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A playable character's: the clip that brings it back from a hop is slot 4 ([`party::HOP_BACK`]), and it travels from the
    /// clip that follows the script's move to the target (the table of the module docs, held by a real-data test).
    Character,
    /// An enemy's: its clips are its own (slot 4 is nothing special), it travels with the first clip that carries it forward
    /// or the one after its move to the target, whichever comes first, and its last clip may carry it home.
    Enemy,
}

/// Is `op` a move to the target (or to a place the script names) that the actor makes while it closes in? (`FB` places
/// the actor next to the target or far from it without a run: it is not a way to close in.)
fn is_move_to_target(op: u8) -> bool {
    matches!(op, code::MOVE_TARGET_X | code::MOVE_TARGET_XZ | code::MOVE_TARGET_XYZ | code::MOVE_TO | code::MOVE_TO_TARGET)
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

    /// The attack `script` plays, or `None` when it is not one (it never marks where the damage lands, plays nothing after
    /// the mark) or plays a clip `clips` cannot give (the slot is past the pack or not the slot's own clip): the
    /// choreography is then procedural, it never borrows another clip.
    ///
    /// The script's clips fall in three runs: up to the mark is the approach, from it up to the return (`FA`) is the
    /// strike, after the return the way back. The mark is the damage (`F7` or `C2`, a number of ticks after it) or the
    /// effect (`EC`: a spell or an effect of the enemy's, which lands at once), whichever comes first. The idle clip (slot 0)
    /// is not played: it is the engine letting the idle loop finish before the action, or settling after it.
    ///
    /// A character's script hops back with slot 4 BEFORE its return: that clip is the way back, not part of the strike (an
    /// enemy's clips are its own: see [`Role`]).
    pub fn from_script(script: &Script, clips: &ClipSet, role: Role) -> Option<Self> {
        let hop_back = (role == Role::Character).then_some(party::HOP_BACK);
        let (mut approach, mut strike, mut back) = (Vec::new(), Vec::new(), Vec::new());
        let (mut impact, mut moved_at, mut returned) = (None, None, false);
        for op in &script.ops {
            match op {
                Op::Play(0) => {}
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
                    code::EFFECT if impact.is_none() => impact = Some(0),
                    code::RETURN => returned = true,
                    c if is_move_to_target(c) && impact.is_none() && moved_at.is_none() => moved_at = Some(approach.len()),
                    _ => {}
                },
            }
        }
        if hop_back.is_some() && back.is_empty() && strike.len() > 1 && strike.last() == hop_back.as_ref() {
            back.extend(strike.pop());
        }
        let playable = |slots: &[Slot]| slots.iter().all(|&s| clips.usable(usize::from(s)).is_some());
        if strike.is_empty() || !(playable(&approach) && playable(&strike) && playable(&back)) {
            return None;
        }
        let strike_ticks: u16 = strike.iter().map(|&s| clips.frames_of(s)).fold(0u16, u16::saturating_add);
        let carries = approach.iter().map(|&s| (-clips.travel_of(s)).max(0.0)).sum::<f32>().round() as u32;
        let (travel_from, home_run) = match role {
            Role::Character => (moved_at.unwrap_or(0), None),
            Role::Enemy => {
                // Where the actor travels from: the first clip that carries it forward, or the one after the script's move.
                let first_forward = approach.iter().position(|&s| -clips.travel_of(s) >= CARRY_TRAVEL);
                let travel_from = match (moved_at, first_forward) {
                    (Some(moved), Some(forward)) => moved.min(forward),
                    (moved, forward) => moved.or(forward).unwrap_or(0),
                };
                // A script that never sends the actor back, whose last clip does: that clip is the way home.
                let home_run = (!returned && back.is_empty()).then(|| strike.last().copied()).flatten().filter(|&s| clips.travel_of(s) >= CARRY_TRAVEL).map(|s| clips.lead_of(s));
                (travel_from, home_run)
            }
        };
        Some(Self { approach, strike, impact: impact?.min(strike_ticks), back, closes_in: moved_at.is_some(), travel_from, carries, home_run })
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
    /// What a party member plays while it runs up to its target without a script to say, and on the way back.
    pub run: Option<Slot>,
    pub back: Option<Slot>,
    /// An enemy's attacks: `scene.bin`'s attack id and the chain of the script it names (its `animation_indices`).
    pub attacks: Vec<(u16, Chain)>,
    /// The attacks `scene.bin` names whose script cannot be played (it is not there, never marks where it lands, or plays
    /// a slot that is past the model's pack or not the slot's own clip): they move procedurally, they do NOT borrow
    /// another attack's clips.
    pub unplayable: Vec<u16>,
}

impl Kit {
    /// Nothing of its own: the actor moves procedurally (a stand-in, a box).
    pub fn none() -> Self {
        Self::default()
    }

    /// The kit of a playable character's battle model: the slots the nine share, and the character's own blows and guard
    /// read from `scripts` (its animation scripts, indexed as the game's table: none, and the blows are procedural and
    /// nothing guards).
    pub fn party(clips: ClipSet, scripts: &[Script]) -> Self {
        let slot = |s: Slot| clips.usable(usize::from(s));
        let chain = |index: usize| scripts.get(index).and_then(|script| Chain::from_script(script, &clips, Role::Character));
        Self {
            hurt: slot(party::HURT),
            death: slot(party::DEATH),
            dead: slot(party::DEAD),
            victory: slot(party::VICTORY),
            defend: first_clip(scripts.get(party::script::DEFEND), &clips),
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

    /// The kit of an enemy's battle model, read from its own animation `scripts` (its `ab` entry; none, and everything is
    /// procedural): the clip its hurt script (1) plays and the one its death script (2) plays, if it plays one, and the
    /// chains of the scripts `scene.bin` names for its attacks (`attacks`: the attack id and the SCRIPT index, not a slot).
    /// An attack whose script cannot be played is `unplayable`.
    pub fn enemy(clips: ClipSet, scripts: &[Script], attacks: &[(u16, u8)]) -> Self {
        let mut chains: Vec<(u16, Chain)> = Vec::new();
        let mut unplayable: Vec<u16> = Vec::new();
        for &(id, index) in attacks {
            match scripts.get(usize::from(index)).and_then(|script| Chain::from_script(script, &clips, Role::Enemy)) {
                Some(chain) => chains.push((id, chain)),
                None => unplayable.push(id),
            }
        }
        Self {
            hurt: first_clip(scripts.get(enemy_script::HURT), &clips),
            death: first_clip(scripts.get(enemy_script::DEATH), &clips),
            attacks: chains,
            unplayable,
            clips,
            ..Self::default()
        }
    }

    /// The chain an enemy's attack `id` plays: its own script's; the first attack's for a plain attack (`None`) or an
    /// attack `scene.bin` does not name for this enemy at all; `None` (procedural) for an attack it names whose script
    /// cannot be played, which does NOT borrow another attack's clips.
    pub fn enemy_chain(&self, id: Option<u16>) -> Option<&Chain> {
        match id {
            Some(id) if self.unplayable.contains(&id) => None,
            Some(id) => self.attacks.iter().find(|(a, _)| *a == id).or(self.attacks.first()).map(|(_, chain)| chain),
            None => self.attacks.first().map(|(_, chain)| chain),
        }
    }

    /// Does this kit play any real clip at all? (tests)
    #[cfg(test)]
    pub fn is_real(&self) -> bool {
        self.clips.fps > 0.0 && !self.clips.frames.is_empty()
    }
}

/// The first clip `script` plays (the idle clip does not count: see [`Chain::from_script`]), if the pack can give it: what a
/// one-clip script (a hurt reaction, a death, a guard) plays.
fn first_clip(script: Option<&Script>, clips: &ClipSet) -> Option<Slot> {
    let slot = script?.ops.iter().find_map(|op| match op {
        Op::Play(n) if *n != 0 => Some(usize::from(*n)),
        _ => None,
    })?;
    clips.usable(slot)
}

/// Invented packs and scripts shaped like Cloud's, for the tests of the choreography: the slot numbers and clip
/// lengths of his blow are the structure the docs above state, and everything else (the move distance, the damage
/// ticks, which cosmetic opcodes there are) is made up. No bytes of the owner's files.
#[cfg(test)]
pub mod testing {
    use super::{ClipSet, Script};
    use fmv_ff7::battle_model::script::decode;

    /// A 52-slot pack whose clips have the lengths of Cloud's chain (26: 2 frames, 27: 5, 30: 6, 28: 3, 29: 4, the hops
    /// 12 to 14, the long-range swing 31, the guard 10) and 15 frames everywhere else, at 15 frames a second.
    pub fn cloud_like_pack() -> ClipSet {
        let mut frames = vec![15u16; 52];
        for (slot, n) in [(26, 2), (27, 5), (30, 6), (28, 3), (29, 4), (3, 14), (4, 12), (31, 31), (5, 10)] {
            frames[slot] = n;
        }
        ClipSet { frames, ok: vec![true; 52], travel: vec![0.0; 52], lead: vec![0; 52], fps: 15.0 }
    }

    /// The decoded script of invented `bytes`.
    pub fn script(bytes: &[u8]) -> Script {
        decode(bytes, 0).expect("the script decodes")
    }

    /// Invented scripts of Cloud's shape: 8 guards (5); 20 runs up (26, a move to the target of a made-up distance, 27),
    /// strikes (a damage marker 2 ticks into 30, then 28) and runs back (a return, then 29); 21 hops forward (3), shoots (a
    /// marker 12 ticks into 31) and hops back (4).
    pub fn cloud_like_scripts() -> Vec<Script> {
        let mut scripts = vec![Script::default(); 22];
        scripts[8] = script(&[5, 0xE5, 0xEE]);
        scripts[20] = script(&[26, 0xD1, 0x58, 0x02, 0x00, 0x00, 0x03, 27, 0xF7, 2, 30, 28, 0xFA, 29, 0xEE]);
        scripts[21] = script(&[3, 0xF7, 12, 31, 4, 0xFA, 0xEE]);
        scripts
    }

    /// An invented 16-slot pack shaped like a small enemy's: the idle loop (0), the hurt clip (1), a run-up that carries the
    /// root far forward (2), two short poses (3, 4), a long clip that holds its pose and then carries the root all the way
    /// back (5), a leap that carries it forward in its last frame (6), a wind-up (7), a swing (8) and a recovery (9), a
    /// death (10), the gathering of a spell (11: long; 12 and 13 short) and its end (14), and a clip that is a stand-in
    /// (15). The travels, the lengths and the leads are made up; only the shape is a real enemy's.
    pub fn enemy_like_pack() -> ClipSet {
        let frames = vec![14u16, 8, 11, 2, 1, 20, 12, 4, 11, 2, 16, 13, 3, 2, 10, 1];
        let mut travel = vec![0.0f32; 16];
        let mut lead = vec![0u16; 16];
        for (slot, z) in [(2, -900.0), (5, 1000.0), (6, -500.0), (8, -600.0)] {
            travel[slot] = z;
        }
        lead[5] = 9;
        let mut ok = vec![true; 16];
        ok[15] = false;
        ClipSet { frames, ok, travel, lead, fps: 15.0 }
    }

    /// Invented scripts of an enemy's shape for [`enemy_like_pack`]: 1 is hurt (the clip, the death effect, home), 2 dies
    /// (the clip); 3 runs up, fires (a damage marker 7 ticks after the run-up, no return) with two short poses and the long
    /// clip that holds and runs back; 4 leaps (6), moves to the target, winds up (7), swings (a marker 3 ticks into 8),
    /// returns and recovers (9); 5 casts (the idle clip, 11, 12, 13, the effect, 14); 6 points at the stand-in (15); 7
    /// plays a clip and marks nothing.
    pub fn enemy_like_scripts() -> Vec<Script> {
        let mut scripts = vec![Script::default(); 8];
        scripts[1] = script(&[1, 0xF6, 0xE5, 0xEE]);
        scripts[2] = script(&[10, 0xE5, 0xEE]);
        scripts[3] = script(&[0xFC, 0xEA, 2, 0xF7, 7, 3, 4, 5, 0xE5, 0xEE]);
        scripts[4] = script(&[0xFC, 6, 0xD1, 0x5E, 0x01, 0x00, 0x00, 0x06, 7, 0xF7, 3, 8, 0xFA, 9, 0xE5, 0xEE]);
        scripts[5] = script(&[0xE8, 0, 0xFC, 0xEA, 11, 0xC9, 12, 0xCA, 13, 0xEC, 14, 0xE5, 0xEE]);
        scripts[6] = script(&[0xF7, 3, 15, 0xE5, 0xEE]);
        scripts[7] = script(&[3, 4, 0xE5, 0xEE]);
        scripts
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{cloud_like_pack, cloud_like_scripts, enemy_like_pack, enemy_like_scripts, script};
    use super::*;

    /// A pack of `n` slots with `frames` frames each, all of them real, none travelling.
    fn pack(n: usize, frames: u16) -> ClipSet {
        ClipSet { frames: vec![frames; n], ok: vec![true; n], travel: vec![0.0; n], lead: vec![0; n], fps: 15.0 }
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
        // What a slot the pack does not have travels and leads: nothing (and a pack without the numbers does not panic).
        set.travel[1] = -700.0;
        set.lead[1] = 3;
        assert_eq!((set.travel_of(1), set.lead_of(1), set.travel_of(77), set.lead_of(77)), (-700.0, 3, 0.0, 0));
        assert_eq!((ClipSet::default().travel_of(0), ClipSet::default().lead_of(0)), (0.0, 0));
    }

    #[test]
    fn a_party_member_has_the_shared_slots_and_its_blows_from_its_scripts_and_loses_what_its_pack_cannot_play() {
        let scripts = cloud_like_scripts();
        let kit = Kit::party(cloud_like_pack(), &scripts);
        // The guard is script 8's clip (5), not the held pose of the cover script (8).
        assert_eq!((kit.hurt, kit.death, kit.dead, kit.victory, kit.defend), (Some(15), Some(18), Some(6), Some(2), Some(5)));
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
        // No scripts, no blows and no guard: the choreography is procedural, and slot 10 (an item thrown at an enemy) is
        // never a blow, nor slot 8 (the cover pose) a guard.
        let bare = Kit::party(cloud_like_pack(), &[]);
        assert!(bare.attack.is_none() && bare.shoot.is_none() && bare.blow(true).is_none() && bare.defend.is_none());
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
        let party = |bytes: &[u8]| Chain::from_script(&script(bytes), &set, Role::Character);
        let enemy = |bytes: &[u8]| Chain::from_script(&script(bytes), &set, Role::Enemy);
        // A dash (26), a run cycle (27) after the move, two hits scheduled (`BE`) and the damage readout 7 ticks on (`C2`:
        // the number), the punches (30), the recovery (28), the way back (29): the readout is the impact.
        let chain = party(&[26, 0xD1, 0x20, 0x01, 0x00, 0x00, 0x04, 27, 0xBE, 3, 0xBE, 7, 0xC2, 7, 30, 28, 0xFA, 29, 0xEE]).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.impact, chain.back.clone()), (vec![26, 27], vec![30, 28], 7, vec![29]), "the readout is 7 ticks in: inside the second strike clip (30 has 6 frames, 28 has 3)");
        // A wind-up in place (25), the move comes after it, then the leap (26): the actor travels from 26.
        let chain = party(&[25, 0xD0, 0x20, 0x01, 0x05, 26, 0xF7, 3, 29, 27, 0xFA, 28, 0xEE]).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.back.clone()), (vec![25, 26], vec![29, 27], vec![28]));
        assert!(chain.closes_in && chain.travel_from == 1, "it winds up in place first");
        // A hop, a three-clip shot that lands 5 ticks in, and the hop back before the return op: for a character the hop back
        // is the way back, not part of the strike. No move: the clips' own travel.
        let shot = [3, 0xF7, 5, 30, 31, 32, 4, 0xFA, 0xEE];
        let chain = party(&shot).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.impact, chain.back.clone()), (vec![3], vec![30, 31, 32], 5, vec![4]));
        assert!(!chain.closes_in && chain.travel_from == 0);
        // An enemy's clip 4 is an ordinary clip: the same bytes are a strike of four clips and no way back but the return.
        let chain = enemy(&shot).expect("an attack");
        assert_eq!((chain.strike.clone(), chain.back.clone(), chain.impact), (vec![30, 31, 32, 4], vec![], 5), "slot 4 is nobody's hop back but a character's");
        // The marker can sit past what the strike clips hold: it lands on their last frame.
        assert_eq!(party(&[3, 0xF7, 200, 31, 4, 0xFA, 0xEE]).expect("a blow").impact, 31);
        // Not blows: no damage marker anywhere, nothing played after the marker, and a clip the pack cannot give.
        assert_eq!(party(&[3, 4, 0xFA, 0xEE]), None, "nothing says when it lands");
        assert_eq!(party(&[3, 0xF7, 4, 0xFA, 0xEE]), None, "nothing is struck with");
        assert_eq!(Chain::from_script(&Script::default(), &set, Role::Enemy), None);
        let mut broken = cloud_like_pack();
        broken.ok[31] = false;
        assert_eq!(Chain::from_script(&script(&[3, 0xF7, 4, 31, 4, 0xFA, 0xEE]), &broken, Role::Character), None, "slot 31 is not the slot's own clip");
        assert_eq!(party(&[3, 0xF7, 4, 60, 4, 0xFA, 0xEE]), None, "slot 60 is past the pack");
    }

    /// An effect (`EC`: a spell, an enemy's special) lands at once; the idle clip is never part of a chain; a placement next
    /// to the target (`FB`) is not a run; whichever mark comes first is the one.
    #[test]
    fn an_effect_is_the_mark_of_a_spell_and_the_idle_clip_is_no_part_of_a_chain() {
        let set = cloud_like_pack();
        let enemy = |bytes: &[u8]| Chain::from_script(&script(bytes), &set, Role::Enemy);
        // The engine's idle clip (0) first, then the gathering (26, 27), the effect, the end (28): the effect lands at the
        // first tick of the end clip.
        let cast = enemy(&[0xE8, 0, 0xFC, 26, 0xC9, 27, 0xCA, 0xEC, 28, 0xE5, 0xEE]).expect("a spell");
        assert_eq!((cast.approach.clone(), cast.strike.clone(), cast.impact, cast.back.clone()), (vec![26, 27], vec![28], 0, vec![]));
        assert!(!cast.closes_in && cast.carries == 0 && cast.home_run.is_none(), "a spell cast in place stays in place");
        // The first mark wins: a damage mark after the effect is not the impact; an effect after the damage is not either.
        assert_eq!(enemy(&[26, 0xEC, 27, 0xF7, 3, 28, 0xEE]).expect("a spell").impact, 0);
        assert_eq!(enemy(&[26, 0xF7, 5, 27, 0xEC, 28, 0xEE]).expect("a blow").impact, 5);
        // Idle clips between and after the clips of the blow are not played, and do not count in its ticks.
        let chain = enemy(&[0, 26, 0xF7, 4, 27, 0, 28, 0, 0xEE]).expect("a blow");
        assert_eq!((chain.approach.clone(), chain.strike.clone(), chain.impact), (vec![26], vec![27, 28], 4));
        // A script that only plays the idle clip marks nothing and strikes with nothing.
        assert_eq!(enemy(&[0, 0xEC, 0, 0xEE]), None);
        // A placement (`FB`) is not a run; a move to the target (`D1`) is, and so is `D0`.
        let placed = enemy(&[26, 0xFB, 0xB8, 0x0B, 0x00, 0x00, 0xF7, 2, 27, 0xFA, 0xEE]).expect("a blow");
        assert!(!placed.closes_in, "FB places the actor, it does not close in");
        let moved = enemy(&[26, 0xD0, 0x20, 0x01, 0x05, 0xF7, 2, 27, 0xFA, 0xEE]).expect("a blow");
        assert!(moved.closes_in && moved.travel_from == 1);
        // A move that comes after the mark is not how the actor got there.
        assert!(!enemy(&[26, 0xF7, 2, 0xD1, 0x58, 0x02, 0x00, 0x00, 0x04, 27, 0xFA, 0xEE]).expect("a blow").closes_in);
    }

    /// The clips' own travel (the root's `z` from the first frame to the last, negative is forward) decides where an actor
    /// travels from and how far it gets by itself; a last clip that travels back is the way home when nothing sends the actor back.
    #[test]
    fn the_clips_own_travel_says_where_the_actor_travels_from_how_far_it_gets_and_what_brings_it_home() {
        let mut set = cloud_like_pack();
        set.travel[26] = -600.0; // a leap
        set.travel[27] = 90.0; // a wind-up that drifts back a little
        set.travel[28] = 700.0; // a recovery that carries the actor home
        set.lead[28] = 2;
        let enemy = |set: &ClipSet, bytes: &[u8]| Chain::from_script(&script(bytes), set, Role::Enemy).expect("a chain");
        // A leap that travels before the move to the target: the actor travels with the leap, from the first clip.
        let leap = [26, 0xD1, 0x58, 0x02, 0x00, 0x00, 0x04, 27, 0xF7, 2, 30, 0xFA, 29, 0xEE];
        let chain = enemy(&set, &leap);
        assert!(chain.closes_in && chain.travel_from == 0 && chain.carries == 600, "{chain:?}");
        // A character's script is read by the move alone: it travels from the clip after the move, whatever its clips carry
        // (the table of the characters' blows is held to the owner's files), and no clip of its brings it home.
        let chain = Chain::from_script(&script(&leap), &set, Role::Character).expect("a chain");
        assert!(chain.closes_in && chain.travel_from == 1 && chain.home_run.is_none(), "{chain:?}");
        let no_return = [27, 26, 0xF7, 2, 30, 28, 0xEE];
        assert!(Chain::from_script(&script(&no_return), &set, Role::Character).expect("a chain").home_run.is_none());
        // No move at all: it only gets as far as its clips carry it, and it starts with them.
        let chain = enemy(&set, &[27, 26, 0xF7, 2, 30, 28, 0xEE]);
        assert!(!chain.closes_in && chain.travel_from == 1 && chain.carries == 600, "{chain:?}");
        // Nothing sends the actor back and the last clip travels back: that clip is the way home, from its frame 2 on.
        assert_eq!((chain.strike.clone(), chain.back.clone(), chain.home_run), (vec![30, 28], vec![], Some(2)));
        // A return op sends it back: no home run. A last clip that does not travel back is no way home either.
        assert_eq!(enemy(&set, &[26, 0xF7, 2, 30, 28, 0xFA, 0xEE]).home_run, None);
        assert_eq!(enemy(&set, &[26, 0xF7, 2, 28, 30, 0xEE]).home_run, None);
        // A clip that travels a little (a drift) is no carrying.
        set.travel[26] = -100.0;
        let chain = enemy(&set, &[26, 0xF7, 2, 30, 0xEE]);
        assert!(chain.carries == 100 && chain.travel_from == 0 && !chain.closes_in);
        // A pack that does not know its travel (no numbers) never panics: nothing travels.
        let bare = ClipSet { travel: Vec::new(), lead: Vec::new(), ..cloud_like_pack() };
        let chain = enemy(&bare, &[26, 0xF7, 2, 28, 0xEE]);
        assert!(chain.carries == 0 && chain.home_run.is_none());
    }

    #[test]
    fn an_enemys_kit_is_read_from_its_scripts_hurt_death_and_the_chain_of_each_attack() {
        let kit = Kit::enemy(enemy_like_pack(), &enemy_like_scripts(), &[(272, 3), (273, 4), (274, 5)]);
        // Script 1 plays the hurt clip (1), script 2 the death clip (10).
        assert_eq!((kit.hurt, kit.death), (Some(1), Some(10)));
        assert!(kit.unplayable.is_empty() && kit.attacks.len() == 3);
        // Attack 272 plays script 3: it runs up with clip 2, holds its poses (3, 4) and the long clip (5) that carries it home;
        // nothing moves it to the target, so it only gets as far as the run-up carried it; it lands 7 ticks after the run-up
        // ends: 4 ticks into the long clip. The long clip is the way home from its frame 9 on.
        let gun = kit.enemy_chain(Some(272)).expect("a chain");
        assert_eq!((gun.approach.as_slice(), gun.strike.as_slice(), gun.impact, gun.back.as_slice()), (&[2][..], &[3, 4, 5][..], 7, &[][..]));
        assert!(!gun.closes_in && gun.travel_from == 0 && gun.carries == 900 && gun.home_run == Some(9), "{gun:?}");
        // 273 plays script 4: a leap (6, it travels), the move to the target, a wind-up (7), the swing (8) that lands 3 ticks in,
        // the return and the recovery (9).
        let tonfa = kit.enemy_chain(Some(273)).expect("a chain");
        assert_eq!((tonfa.approach.as_slice(), tonfa.strike.as_slice(), tonfa.impact, tonfa.back.as_slice()), (&[6, 7][..], &[8][..], 3, &[9][..]));
        assert!(tonfa.closes_in && tonfa.travel_from == 0 && tonfa.home_run.is_none(), "the leap carries the actor, the move comes after it: {tonfa:?}");
        // 274 plays script 5: the gathering (11, 12, 13) up to the effect, its end (14); it stays where it is.
        let cast = kit.enemy_chain(Some(274)).expect("a chain");
        assert_eq!((cast.approach.as_slice(), cast.strike.as_slice(), cast.impact), (&[11, 12, 13][..], &[14][..], 0));
        assert!(!cast.closes_in && cast.carries == 0);
        // A plain attack, or an attack the scene does not name for this enemy, plays its first attack's.
        assert_eq!((kit.enemy_chain(None), kit.enemy_chain(Some(999))), (Some(gun), Some(gun)));
        // No clip of an enemy's is a party member's: it has no blows, no guard and no slots of the party's table.
        assert_eq!((kit.attack.as_ref(), kit.defend, kit.victory, kit.run, kit.back), (None, None, None, None, None));
    }

    #[test]
    fn an_attack_whose_script_cannot_be_played_is_procedural_and_never_borrows_anothers_clips() {
        let scripts = enemy_like_scripts();
        // 275 names script 6, which strikes with a stand-in clip; 276 names script 7, which marks nothing; 277 and 278 name
        // scripts that are not there; 273 plays.
        let kit = Kit::enemy(enemy_like_pack(), &scripts, &[(273, 4), (275, 6), (276, 7), (277, 40), (278, 255)]);
        assert_eq!(kit.attacks.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [273]);
        assert_eq!(kit.unplayable, [275, 276, 277, 278]);
        for bad in [275u16, 276, 277, 278] {
            assert_eq!(kit.enemy_chain(Some(bad)), None, "attack {bad} is procedural: it does not play the tonfa's clips");
        }
        assert!(kit.enemy_chain(Some(273)).is_some());
        // An enemy whose pack ends early loses what is past it.
        let mut short = enemy_like_pack();
        short.ok[8] = false;
        let kit = Kit::enemy(short, &scripts, &[(273, 4)]);
        assert_eq!(kit.unplayable, [273], "slot 8 is not the slot's own clip");
        // A model without scripts (or a hurt script that plays a stand-in) is procedural throughout.
        let bare = Kit::enemy(enemy_like_pack(), &[], &[(273, 4)]);
        assert_eq!((bare.hurt, bare.death, bare.unplayable.clone(), bare.enemy_chain(None)), (None, None, vec![273], None));
        let mut stand_in = enemy_like_scripts();
        stand_in[1] = script(&[15, 0xE5, 0xEE]);
        assert_eq!(Kit::enemy(enemy_like_pack(), &stand_in, &[]).hurt, None, "slot 15 is not the slot's own clip");
        // The idle clip is not a hurt reaction: a script that begins with it plays the clip after.
        stand_in[1] = script(&[0, 1, 0xEE]);
        assert_eq!(Kit::enemy(enemy_like_pack(), &stand_in, &[]).hurt, Some(1));
    }
}
