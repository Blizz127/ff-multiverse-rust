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

fn complete_outcome(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    (match outcome.end {
        fmv_ff7::script::dialogue::End::Finished => outcome.jump.is_none(),
        fmv_ff7::script::dialogue::End::MapJump { field } => outcome.jump.is_some_and(|jump| jump.field == field),
        _ => false,
    }) && [&outcome.setup_stats, &outcome.stats].iter().all(|s| s.unknown.is_empty() && s.unresolved == 0 && s.dropped_requests == 0)
}

fn safe_outcome(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    // These ignored instructions read or change the player's party, resources, inventory,
    // clock or menu-owned state. Advancing past one without carrying it out can lose state or
    // select the wrong branch. They remain blocked until the server implements their effects.
    const UNSUPPORTED_STATE: &[&str] = &[
        "PREQ", "PRQSW", "PRQEW", "JOIN", "SPLIT", "SPTYE", "GTPYE", "SPECIAL", "STTIM",
        "GOLDu", "GOLDd", "CHGLD", "HMPMAX1", "HMPMAX2", "MHMMX", "HMPMAX3",
        "MPu", "MPd", "MENU", "HPu", "HPd", "STITM", "DLITM", "CKITM", "SMTRA", "DMTRA", "CMTRA",
        "PRTYP", "PRTYM", "PRTYE", "MMBud", "MMBLK", "MMBUK",
    ];
    complete_outcome(outcome)
        && [&outcome.setup_stats, &outcome.stats].iter().all(|s| !UNSUPPORTED_STATE.iter().any(|op| s.ignored.contains_key(op)))
        // Banks 1–4 may contain persistent game state (for example Tifa's affection in
        // bank 1). Local field scratch variables remain allowed, but untracked persistent
        // writes must not be silently discarded when the story is saved.
        && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank))
}

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

    fn finish_scene(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], preview: bool) -> Option<SceneResult> {
        let (def, story) = (self.def(scene)?, StoryState::decode(progress)?);
        let area = self.field(&def.field)?;
        let answers: Vec<usize> = answers.iter().map(|&a| usize::from(a)).collect();
        let outcome = run_to_end(area.field_file(), def, &story, seed, &answers).ok()?;
        if !(if preview { complete_outcome(&outcome) } else { safe_outcome(&outcome) }) { return None; }
        let mut after = story.clone();
        let changed = outcome.writes.apply_to(&mut after);
        let mut summary = Vec::new();
        if after.moment != story.moment { summary.push(format!("moment={}->{}", story.moment, after.moment)); }
        for ((bank, address), value) in after.vars() {
            if story.var(bank, address) != value { summary.push(format!("var[{bank}][{address}]={value}")); }
        }
        let travel = if let Some(jump) = outcome.jump {
            let target = self.adapter.field_key_for_id(jump.field)?;
            if !fmv_ff7::is_reviewed_field(&target) { return None; }
            Some(fmv_adapter_api::Exit {
                line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)), target,
                arrival: Pos::new(f32::from(jump.x), f32::from(jump.y), 0.0),
                arrival_facing: Some(field_direction_to_facing(jump.direction)),
                arrival_hint: u32::try_from(jump.triangle).ok(), kind: fmv_adapter_api::ExitKind::Gateway,
            })
        } else { None };
        Some(SceneResult { progress: after.encode(), changed, summary: summary.join(" "), travel })
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
        self.finish_scene(scene, progress, seed, answers, false)
    }

    fn finish_preview(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.finish_scene(scene, progress, seed, answers, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::{dialogue::End, scene::{Outcome, Stats, Writes}};

    #[test]
    fn incomplete_or_economic_scenes_cannot_advance_or_travel() {
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None, setup_stats: Stats::default(), stats: Stats::default() };
        assert!(safe_outcome(&outcome));
        outcome.stats.ignored.insert("GOLDu", 1);
        assert!(!safe_outcome(&outcome));
        outcome.stats.ignored.clear();
        for op in ["HMPMAX3", "PRTYM", "CHGLD", "HPd", "DLITM"] {
            outcome.stats.ignored.insert(op, 1);
            assert!(!safe_outcome(&outcome), "ignored stateful operation {op} must refuse the write");
            outcome.stats.ignored.clear();
        }
        outcome.writes.untracked.insert((1, 4), 5);
        assert!(!safe_outcome(&outcome), "untracked persistent affection cannot be discarded");
        outcome.writes.untracked.clear();
        outcome.writes.untracked.insert((5, 13), 1);
        assert!(safe_outcome(&outcome), "field-local scratch variables do not require persistence");
        outcome.writes.untracked.clear();
        outcome.stats.unknown.insert("GOLDd", 1);
        assert!(!safe_outcome(&outcome));
        outcome.stats.unknown.clear();
        outcome.stats.unresolved = 1;
        assert!(!safe_outcome(&outcome));
        outcome.stats.unresolved = 0;
        outcome.setup_stats.unknown.insert("unknown", 1);
        assert!(!complete_outcome(&outcome));
        outcome.setup_stats.unknown.clear();
        outcome.setup_stats.dropped_requests = 1;
        assert!(!complete_outcome(&outcome));
        outcome.setup_stats.dropped_requests = 0;
        outcome.setup_stats.unresolved = 1;
        assert!(!complete_outcome(&outcome));
        outcome.setup_stats.unresolved = 0;
        outcome.setup_stats.ignored.insert("GOLDd", 1);
        assert!(complete_outcome(&outcome), "preview may show a complete simulated run");
        assert!(!safe_outcome(&outcome), "setup must not discard economic effects");
        outcome.setup_stats.ignored.clear();
        outcome.end = End::Budget;
        assert!(!safe_outcome(&outcome));
    }
}
