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
}

impl FieldRuntime {
    /// The visible models other than the one the player controls, with what the client needs to draw them.
    pub fn cast(&self, loader: Option<&ModelLoader>) -> Vec<CastMember> {
        let pc = self.state.pc_model;
        self.models
            .iter()
            .enumerate()
            .filter(|(i, m)| m.visible && Some(*i) != pc)
            .filter_map(|(i, m)| {
                let record = loader.and_then(|l| l.models.get(usize::from(m.char_id)));
                let skeleton = record.map(|r| crate::util::entry_stem(&r.hrc)).filter(|s| !s.is_empty())?;
                let animation = record.and_then(|r| r.animations.get(usize::from(m.anim)).or_else(|| r.animations.first())).map(|a| a.stem()).filter(|s| !s.is_empty());
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
            let mask = !crate::story::engine_owned_bits(selector, address);
            if (now ^ was) & mask == 0 {
                continue;
            }
            let value = (was & !mask) | (now & mask);
            changed |= next.write_masked(selector, address, value, 0xFF)?;
        }
        Ok(changed.then_some(next))
    }
}
