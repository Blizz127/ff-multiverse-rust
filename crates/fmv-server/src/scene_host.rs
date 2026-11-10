//! FF7's story scenes for the server: the world's [`SceneHost`] over the owner's own field archive.
//!
//! The scenes themselves are `fmv_ff7::script::scene`'s (data and interpreter); this only turns them into what the
//! game-neutral world asks for: which scenes there are, whether one is on offer for a story, where its trigger stands, the
//! story a debug preview plays on, and what a finished scene wrote. Everything is read from the owner's files through the
//! adapter, read-only; the fields it decodes are kept (a field is decoded once, not once per press).
//!
//! The progression the world hands in is FF7's story blob ([`fmv_ff7::story::StoryState`]); one that cannot be read is no story
//! for a scene: nothing is offered and nothing is written.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use fmv_core::Pos;
use fmv_ff7::script::scene::{run_to_end, scenes, trigger_npc, SceneDef};
use fmv_ff7::story::StoryState;
use fmv_ff7::{field_direction_to_facing, Ff7Adapter, Ff7FieldArea};

use crate::world::{SceneHost, SceneInfo, SceneResult, TriggerInfo};

/// The scenes of FF7 over an opened adapter.
pub struct Ff7SceneHost {
    adapter: Arc<Ff7Adapter>,
    defs: Vec<SceneDef>,
    fields: Mutex<BTreeMap<String, Option<Arc<Ff7FieldArea>>>>,
}

impl Ff7SceneHost {
    pub fn new(adapter: Arc<Ff7Adapter>) -> Self {
        Self { adapter, defs: scenes(), fields: Mutex::new(BTreeMap::new()) }
    }

    fn def(&self, id: &str) -> Option<&SceneDef> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// The decoded field `name`, loaded the first time it is asked for (a field that cannot be loaded is remembered as such).
    fn field(&self, name: &str) -> Option<Arc<Ff7FieldArea>> {
        let mut fields = self.fields.lock().unwrap_or_else(PoisonError::into_inner);
        fields.entry(name.to_string()).or_insert_with(|| self.adapter.load_field(name).ok().map(Arc::new)).clone()
    }
}

impl SceneHost for Ff7SceneHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        self.defs
            .iter()
            .map(|d| SceneInfo {
                id: d.id.clone(),
                field: d.field.clone(),
                arrival: Pos::new(f32::from(d.arrival.x), f32::from(d.arrival.y), 0.0),
                arrival_facing: field_direction_to_facing(d.arrival.direction),
                arrival_hint: Some(u32::from(d.arrival.triangle)),
            })
            .collect()
    }

    fn offered(&self, scene: &str, progress: &[u8]) -> bool {
        let (Some(def), Some(story)) = (self.def(scene), StoryState::decode(progress)) else { return false };
        def.offered_to(&story)
    }

    fn trigger(&self, scene: &str, progress: &[u8]) -> Option<TriggerInfo> {
        let (def, story) = (self.def(scene)?, StoryState::decode(progress)?);
        let area = self.field(&def.field)?;
        let npc = trigger_npc(area.field_file(), def, &story)?;
        Some(TriggerInfo {
            name: npc.name,
            model: npc.model,
            idle: npc.idle,
            pos: Pos::new(f32::from(npc.x), f32::from(npc.y), npc.z),
            facing: npc.heading,
            talk_radius: npc.talk_radius,
        })
    }

    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>> {
        let def = self.def(scene)?;
        Some(StoryState::at(*def.trigger.moments.start()).encode())
    }

    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        let (def, story) = (self.def(scene)?, StoryState::decode(progress)?);
        let area = self.field(&def.field)?;
        let answers: Vec<usize> = answers.iter().map(|&a| usize::from(a)).collect();
        let outcome = run_to_end(area.field_file(), def, &story, seed, &answers).ok()?;
        let mut after = story.clone();
        let changed = outcome.writes.apply_to(&mut after);
        let mut summary = Vec::new();
        if after.moment != story.moment {
            summary.push(format!("moment={}->{}", story.moment, after.moment));
        }
        for ((bank, address), value) in after.vars() {
            if story.var(bank, address) != value {
                summary.push(format!("var[{bank}][{address}]={value}"));
            }
        }
        let travel = outcome.jump.and_then(|jump| {
            let target = self.adapter.field_key_for_id(jump.field)?;
            // Never send a real player to a field whose story has not been reviewed.
            if !fmv_ff7::is_reviewed_field(&target) { return None; }
            Some(fmv_adapter_api::Exit {
                line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)), target,
                arrival: Pos::new(f32::from(jump.x), f32::from(jump.y), 0.0),
                arrival_facing: Some(field_direction_to_facing(jump.direction)),
                arrival_hint: u32::try_from(jump.triangle).ok(),
                kind: fmv_adapter_api::ExitKind::Gateway,
            })
        });
        Some(SceneResult { progress: after.encode(), changed, summary: summary.join(" "), travel })
    }
}
