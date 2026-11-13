//! What a host shows of a running field: its visible models, with their skeletons and animations named the way the
//! client loads them, and the story image a host saves.

use super::{Banks, FieldRuntime, PERSISTENT_BYTES};
use crate::field::ModelLoader;
use crate::runtime::memory::PERSISTENT_SELECTORS;
use crate::story::{StoryState, StoryWriteError};

/// One visible model of a running field.
#[derive(Debug, Clone, PartialEq)]
pub struct CastMember {
    /// The model's index in the running field (stable while the field runs).
    pub model: usize,
    pub entity: usize,
    /// The script entity's name.
    pub name: String,
    /// The skeleton's archive stem (`aaaa`).
    pub skeleton: String,
    /// The animation it plays, as an archive stem, when the field's model loader names it.
    pub animation: Option<String>,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Field direction (0-255).
    pub dir: u8,
    /// This is the player's own character (the model the scripts move in scenes and the player moves otherwise).
    pub lead: bool,
    /// The animation and frame it shows now when a script plays it once or holds it (`animation` is then what it
    /// stands in afterwards); `None` while its animation simply loops.
    pub pose: Option<(String, u16)>,
}

impl FieldRuntime {
    /// The visible models, with what the client needs to draw them. The player's own character is among them
    /// ([`CastMember::lead`]): a host shows it where the scripts put it while they hold the player.
    pub fn cast(&self, loader: Option<&ModelLoader>) -> Vec<CastMember> {
        use super::AnimStatus;
        let pc = self.state.player_model.or(self.state.pc_model);
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.visible)
            .filter_map(|(i, m)| {
                let record = loader.and_then(|l| l.models.get(usize::from(m.char_id)));
                let skeleton = record.map(|r| crate::util::entry_stem(&r.hrc)).filter(|s| !s.is_empty())?;
                let stem = |anim: u8| record.and_then(|r| r.animations.get(usize::from(anim))).map(|a| a.stem()).filter(|s| !s.is_empty());
                let view = self.model_animation(i);
                let looping = view.is_none_or(|v| matches!(v.status, AnimStatus::DefaultLoop | AnimStatus::ScriptedLoop));
                // A clip played once or held is shown frame by frame; what the model stands in is its default clip.
                let pose = if looping { None } else { view.and_then(|v| Some((stem(v.animation)?, v.frame.max(0.0) as u16))) };
                let standing = if pose.is_some() { self.model_default_animation(i) } else { m.anim };
                let animation = stem(standing).or_else(|| record.and_then(|r| r.animations.first()).map(|a| a.stem()).filter(|s| !s.is_empty()));
                Some(CastMember {
                    model: i,
                    entity: m.entity,
                    name: self.entity_name(m.entity).unwrap_or_default().to_string(),
                    skeleton,
                    animation,
                    x: m.x,
                    y: m.y,
                    z: m.z,
                    dir: m.dir,
                    lead: Some(i) == pc,
                    pose,
                })
            })
            .collect()
    }

    /// `saved` with every persistent byte this field changed written into it, leaving the bytes the engine owns (clocks,
    /// counters, menu locks: [`crate::story::engine_owned_bits`]) as they were. `None` when nothing changed.
    pub fn story_after(&self, saved: &StoryState) -> Result<Option<StoryState>, StoryWriteError> {
        let Some(before) = Banks::from_story(saved) else { return Ok(None) };
        let mut next = saved.clone();
        let mut changed = false;
        for index in 0..PERSISTENT_BYTES {
            let (now, was) = (self.mem.banks.get(index), before.get(index));
            if now == was {
                continue;
            }
            let selector = PERSISTENT_SELECTORS[index / 256];
            let address = (index % 256) as u8;
            // (and never the engine's "the countdown ran out" signal, bank 1 address 225 bit 7: a field sets it on the
            // way to the game-over screen. Saved, it would end the game again on every return.)
            let game_over = if (selector, address) == (1, 225) { 0x80 } else { 0 };
            // The field party (bank 3, addresses 9-11) is engine-owned in the reset profile because its first value is
            // the engine's, but afterwards only the field scripts (PRTYP / PRTYM / PRTYE / SPTYE) and the party menu
            // change it, and the original keeps it in the savemap from field to field. Left out, every field reloaded
            // Cloud alone: JOIN/SPLIT found no followers and battles lost Barret and Tifa.
            let owned = if selector == 3 && (9..=11).contains(&address) { 0 } else { crate::story::engine_owned_bits(selector, address) };
            let mask = !(owned | game_over);
            if (now ^ was) & mask == 0 {
                continue;
            }
            let value = (was & !mask) | (now & mask);
            changed |= next.write_masked(selector, address, value, 0xFF)?;
        }
        Ok(changed.then_some(next))
    }
}
