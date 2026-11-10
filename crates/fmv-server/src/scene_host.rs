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
    }) && [&outcome.setup_stats, &outcome.stats].iter().all(|s| !s.setup_incomplete && s.unknown.is_empty() && s.unresolved == 0 && s.dropped_requests == 0)
}

fn safe_outcome(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    // These ignored instructions read or change the player's resources, inventory, clock or menu-owned state, or split
    // and join the party in ways nothing here models. Advancing past one without carrying it out can lose state or select
    // the wrong branch. They remain blocked until the server implements their effects.
    //
    // Two families are NOT in this list, each for a reason that is checked elsewhere:
    //
    // * `PRTYP`, `PRTYM`, `PRTYE` are not ignored any more: the interpreter records them (`Outcome::party`) and
    //   `finish_scene` accepts the scene only when the story's own party table has the party they leave
    //   (`Outcome::party_is_the_storys`), which is how this server carries a party.
    // * `HMPMAX1`, `HMPMAX2`, `HMPMAX3` and `MHMMX` restore the party's HP and MP. This server keeps no HP or MP between
    //   battles: a fighter is built from its level alone and starts every battle full (`fmv_protocol::BattleMember` has
    //   no HP or MP input, and the stored progress is level, experience, AP, gil and the limit gauge). So a full
    //   restore is what already holds, and skipping it loses nothing. (`HPu`, `HPd`, `MPu`, `MPd` change them by an
    //   amount, which is not that, and stay refused.)
    use fmv_ff7::script::scene::{FULL_RESTORE_OPS, SKIPPED_STATE_OPS};
    let refused = |op: &&'static str| SKIPPED_STATE_OPS.contains(op) && !FULL_RESTORE_OPS.contains(op);
    complete_outcome(outcome)
        && outcome.unknown_persistent_reads.is_empty()
        && [&outcome.setup_stats, &outcome.stats].iter().all(|s| !s.ignored.keys().any(refused))
        // Banks 1–4 may contain persistent game state (for example Tifa's affection in
        // bank 1). Local field scratch variables remain allowed, but untracked persistent
        // writes must not be silently discarded when the story is saved.
        && !outcome.writes.untracked.keys().any(|(bank, _)| (1..=4).contains(bank))
}

/// The variables a scene may change in a story that is SAVED while the extended save is off (`Ff7SceneHost::new`): the
/// moment, and the one variable the reactor mission's own story points already save (`REACTOR_STATE`). Every build since the
/// reactor walk writes exactly these, so a server that only lets scenes write these stores nothing an older build did not.
fn saves_only_what_was_always_saved(before: &StoryState, after: &StoryState) -> bool {
    let always = fmv_ff7::story::REACTOR_STATE;
    let differs = |(key, value): ((u8, u8), u8)| key != (always.bank, always.address) && before.var(key.0, key.1) != value;
    !after.vars().any(differs) && !before.vars().any(|(key, _)| key != (always.bank, always.address) && after.var(key.0, key.1) != before.var(key.0, key.1))
}

/// The scenes of FF7 over an opened adapter.
pub struct Ff7SceneHost {
    adapter: Arc<Ff7Adapter>,
    defs: Vec<SceneDef>,
    fields: Mutex<BTreeMap<String, Option<Arc<Ff7FieldArea>>>>,
    /// Whether a scene may save story variables beyond the ones every build has always saved
    /// ([`saves_only_what_was_always_saved`]). Off unless the server is started with `--story-extended-save`: with it off, a
    /// scene whose story would carry such a variable is not applied at all (it can be played, it changes nothing).
    extended_save: bool,
}

impl Ff7SceneHost {
    pub fn new(adapter: Arc<Ff7Adapter>) -> Self {
        Self::with_extended_save(adapter, false)
    }

    /// The host of a server that may (or may not) save story variables beyond the long-saved ones (see
    /// [`Ff7SceneHost::extended_save`]).
    pub fn with_extended_save(adapter: Arc<Ff7Adapter>, extended_save: bool) -> Self {
        Self { adapter, defs: scenes(), fields: Mutex::new(BTreeMap::new()), extended_save }
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
        if !preview {
            // The party the scripts leave must be the one the story has from now on (the party table carries it), and the
            // story may hold nothing new to save unless this server was told it may.
            if !outcome.party_is_the_storys(&story, &after) { return None; }
            if !self.extended_save && !saves_only_what_was_always_saved(&story, &after) { return None; }
        }
        let mut summary = Vec::new();
        if after.moment != story.moment { summary.push(format!("moment={}->{}", story.moment, after.moment)); }
        for ((bank, address), value) in after.vars() {
            if story.var(bank, address) != value { summary.push(format!("var[{bank}][{address}]={value}")); }
        }
        let travel = if let Some(jump) = outcome.jump {
            let target = self.adapter.field_key_for_id(jump.field)?;
            // A scene sends its players only where the story goes on: a reviewed field (its own points and exits are
            // offered) or the field of another scene.
            if !fmv_ff7::is_reviewed_field(&target) && !fmv_ff7::script::scene::is_scene_field(&target) { return None; }
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

    fn automatic(&self, scene: &str) -> bool {
        self.def(scene).is_some_and(SceneDef::starts_on_arrival)
    }

    fn point_scene(&self, field: &str, before: &[u8], after: &[u8]) -> Option<String> {
        let (before, after) = (StoryState::decode(before)?, StoryState::decode(after)?);
        self.defs.iter().find(|d| d.presents_step(field, &before, &after)).map(|d| d.id.clone())
    }

    fn entry(&self, progress: &[u8]) -> Option<fmv_adapter_api::Exit> {
        // (a progression that cannot be read is nobody's story: it joins like everyone, as it always has)
        let entry = fmv_ff7::script::scene::story_entry(StoryState::decode(progress)?.moment)?;
        Some(fmv_adapter_api::Exit {
            line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)),
            target: entry.field.to_string(),
            arrival: Pos::new(f32::from(entry.spot.x), f32::from(entry.spot.y), 0.0),
            arrival_facing: Some(field_direction_to_facing(entry.spot.direction)),
            arrival_hint: Some(u32::from(entry.spot.triangle)),
            kind: fmv_adapter_api::ExitKind::Gateway,
        })
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
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None, setup_stats: Stats::default(), questions: 0, unknown_persistent_reads: Default::default(), party: Vec::new(), stats: Stats::default() };
        assert!(safe_outcome(&outcome));
        outcome.unknown_persistent_reads.insert((3, 211));
        assert!(!safe_outcome(&outcome), "assumed persistent inputs cannot drive real progress");
        assert!(complete_outcome(&outcome), "preview remains available");
        outcome.unknown_persistent_reads.clear();
        outcome.stats.ignored.insert("GOLDu", 1);
        assert!(!safe_outcome(&outcome));
        outcome.stats.ignored.clear();
        for op in ["CHGLD", "HPd", "MPu", "DLITM", "MENU", "MMBud", "SPECIAL"] {
            outcome.stats.ignored.insert(op, 1);
            assert!(!safe_outcome(&outcome), "ignored stateful operation {op} must refuse the write");
            outcome.stats.ignored.clear();
        }
        // A full restore is what this server's battles already start with: nothing is lost by not carrying it out.
        for op in ["HMPMAX1", "HMPMAX2", "HMPMAX3", "MHMMX"] {
            outcome.stats.ignored.insert(op, 1);
            assert!(safe_outcome(&outcome), "{op} restores what no battle here ever starts without");
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
        outcome.setup_stats.setup_incomplete = true;
        assert!(!complete_outcome(&outcome), "budget-exhausted setup cannot be accepted");
        outcome.setup_stats.setup_incomplete = false;
        outcome.setup_stats.ignored.insert("GOLDd", 1);
        assert!(complete_outcome(&outcome), "preview may show a complete simulated run");
        assert!(!safe_outcome(&outcome), "setup must not discard economic effects");
        outcome.setup_stats.ignored.clear();
        outcome.end = End::Budget;
        assert!(!safe_outcome(&outcome));
    }

    /// A scene's party change is accepted only where the story's own party table has the party the scripts leave: the
    /// table is how the change is carried, so a change it has no row for is refused, not dropped.
    #[test]
    fn a_party_change_is_carried_by_the_storys_party_table_or_refused() {
        use fmv_ff7::story::{PartyOp, BARRET, CLOUD};
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None, setup_stats: Stats::default(), questions: 0, unknown_persistent_reads: Default::default(), party: Vec::new(), stats: Stats::default() };
        let (escaped, split_up) = (StoryState::at(33), StoryState::at(36));
        assert!(outcome.party_is_the_storys(&escaped, &escaped), "a scene that touches no party");
        outcome.party = vec![PartyOp::Remove(BARRET)];
        assert!(outcome.party_is_the_storys(&escaped, &split_up), "Barret leaves and the story moves to where Cloud is alone");
        assert!(!outcome.party_is_the_storys(&escaped, &escaped), "Barret leaves but the story stays where he is in the party");
        assert!(!outcome.party_is_the_storys(&escaped, &StoryState::at(35)));
        outcome.party = vec![PartyOp::Add(2)];
        assert!(!outcome.party_is_the_storys(&escaped, &split_up), "a join the table has no row for");
        outcome.party = vec![PartyOp::Set([CLOUD, 254, 254])];
        assert!(outcome.party_is_the_storys(&escaped, &split_up), "a set that comes to the same party");
        outcome.party = vec![PartyOp::Remove(BARRET), PartyOp::Add(BARRET)];
        assert!(outcome.party_is_the_storys(&escaped, &escaped), "left and back again");
    }

    /// With the extended save off, a scene may move the moment and the reactor's long-saved byte and nothing else.
    #[test]
    fn without_the_extended_save_a_scene_stores_nothing_an_older_build_did_not() {
        use fmv_ff7::story::{BAR_CONVERSATION, BAR_CONVERSATION_BIT, LIFT_BIT, REACTOR_STATE};
        let before = StoryState::at(33);
        let mut after = StoryState::at(36);
        assert!(saves_only_what_was_always_saved(&before, &after), "the moment alone");
        assert!(after.set_bit(REACTOR_STATE.bank, REACTOR_STATE.address, LIFT_BIT, true));
        assert!(saves_only_what_was_always_saved(&before, &after), "the reactor's byte is what the mission's points already save");
        assert!(after.set_bit(BAR_CONVERSATION.bank, BAR_CONVERSATION.address, BAR_CONVERSATION_BIT, true));
        assert!(!saves_only_what_was_always_saved(&before, &after), "a variable no older build saved");
        // ... and a story that already holds one may keep it, but not change it.
        assert!(saves_only_what_was_always_saved(&after, &after));
        let mut cleared = after.clone();
        assert!(cleared.set_bit(BAR_CONVERSATION.bank, BAR_CONVERSATION.address, BAR_CONVERSATION_BIT, false));
        assert!(!saves_only_what_was_always_saved(&after, &cleared));
    }
}
