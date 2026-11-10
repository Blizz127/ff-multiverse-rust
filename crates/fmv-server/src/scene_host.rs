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
use fmv_ff7::script::scene::{run_to_end_with_resources, run_to_end_with_resources_strict, scenes, trigger_npc, SceneDef, SceneResources};
use fmv_ff7::story::StoryState;
use fmv_ff7::{field_direction_to_facing, Ff7Adapter, Ff7FieldArea};

use crate::world::{SceneHost, SceneInfo, SceneResult, SceneTransaction, TriggerInfo};

mod continuation;
mod points;

fn complete_outcome(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    (match outcome.end {
        fmv_ff7::script::dialogue::End::Finished => outcome.jump.is_none(),
        fmv_ff7::script::dialogue::End::MapJump { field } => outcome.jump.is_some_and(|jump| jump.field == field),
        _ => false,
    }) && !outcome.resources.unknown_gil && [&outcome.setup_stats, &outcome.stats].iter().all(|s| !s.setup_incomplete && s.unknown.is_empty() && s.unresolved == 0 && s.dropped_requests == 0)
}

fn safe_interpretation(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    complete_outcome(outcome) && safe_segment(outcome, true)
}

/// A continuation can stop at a window or battle rather than the end of the script. Its state
/// still needs the same complete accounting before it can become a durable checkpoint.
fn safe_segment(outcome: &fmv_ff7::script::scene::Outcome, full_restore_is_redundant: bool) -> bool {
    // These ignored instructions read or change the player's resources, inventory, clock or menu-owned state, or split
    // and join the party in ways nothing here models. Advancing past one without carrying it out can lose state or select
    // the wrong branch. They remain blocked until the server implements their effects.
    //
    // Two families are NOT in this list, each for a reason that is checked elsewhere:
    //
    // * `PRTYP`, `PRTYM`, `PRTYE` are not ignored any more: the interpreter records them (`Outcome::party`) and
    //   `finish_scene` accepts the scene only when the story's own party table has the party they leave
    //   (`Outcome::party_is_the_storys`), which is how this server carries a party.
    // * `HMPMAX1`, `HMPMAX2`, `HMPMAX3` and `MHMMX` restore the party's HP and MP. Ordinary battles still start full,
    //   so those legacy scenes may treat a full restore as redundant. Consecutive continuation battles carry combat
    //   resources instead: their caller passes false and refuses an unimplemented restore. Changes by an amount
    //   (`HPu`, `HPd`, `MPu`, `MPd`) remain refused in either path.
    use fmv_ff7::script::scene::{FULL_RESTORE_OPS, SKIPPED_STATE_OPS};
    let refused = |op: &&'static str| SKIPPED_STATE_OPS.contains(op)
        && !(full_restore_is_redundant && FULL_RESTORE_OPS.contains(op));
    outcome.is_complete()
        && outcome.unknown_persistent_reads.is_empty()
        && [&outcome.setup_stats, &outcome.stats].iter().all(|s| !s.ignored.keys().any(refused))
        // Every persistent selector must be accounted for, including aliases and the higher banks.
        && !outcome.writes.untracked.keys().any(|(bank, _)| fmv_ff7::story::canonical_persistent_bank(*bank).is_some())
}

fn safe_outcome(outcome: &fmv_ff7::script::scene::Outcome) -> bool {
    safe_interpretation(outcome)
        // Legacy saves cannot carry exact assignments or economic effects.
        && outcome.writes.persistent.is_empty()
        && outcome.resources.gil_ops.is_empty()
}

fn persistable_field_gil(before: u32, after: u32) -> bool {
    before <= fmv_ff7_battle::kernel::MAX_GIL && after <= fmv_ff7_battle::kernel::MAX_GIL
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
    /// ([`saves_only_what_was_always_saved`]). Off unless the server is started with `--story-extended-save`.
    /// Scenes needing an exact transaction and already extended stories are not offered while it is off.
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

    fn finish_scene(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], preview: bool, gil: Option<u32>) -> Option<SceneResult> {
        let def = self.def(scene)?;
        let story = def.prepare_story(&StoryState::decode(progress)?).ok()?;
        if !preview && story.supports_extended() && (!self.extended_save || gil.is_none()) { return None; }
        let area = self.field(&def.field)?;
        let answers: Vec<usize> = answers.iter().map(|&a| usize::from(a)).collect();
        let run = if !preview && story.supports_extended() { run_to_end_with_resources_strict } else { run_to_end_with_resources };
        let outcome = run(area.field_file(), def, &story, seed, SceneResources { gil }, &answers).ok()?;
        self.scene_result(progress, &story, &outcome, preview, gil)
    }

    /// Build the final transition against the latest durable segment's input. A resumed scene
    /// uses its post-battle resource snapshot here, so prefix writes cannot replace earned gil.
    fn scene_result(&self, progress: &[u8], story: &StoryState, outcome: &fmv_ff7::script::scene::Outcome, preview: bool, gil: Option<u32>) -> Option<SceneResult> {
        if !(if preview { complete_outcome(outcome) } else if story.supports_extended() { safe_interpretation(outcome) } else { safe_outcome(outcome) }) { return None; }
        let mut after = story.clone();
        let changed = if story.supports_extended() {
            outcome.writes.apply_extended_to(&mut after, story)?
        } else { outcome.writes.apply_to(&mut after) };
        if !preview {
            // The party the scripts leave must be the one the story has from now on (the party table carries it), and the
            // story may hold nothing new to save unless this server was told it may.
            if !outcome.party_is_the_storys(story, &after) { return None; }
            if !self.extended_save && !saves_only_what_was_always_saved(story, &after) { return None; }
        }
        let transaction = if !preview && story.supports_extended() {
            let expected_gil = gil?;
            if outcome.resources.gil_before != Some(expected_gil) { return None; }
            let next_gil = outcome.resources.gil_after?;
            // Login and ordinary resource saves still apply the battle cap. Until those paths
            // preserve field balances, refuse an out-of-range scene instead of storing a value
            // a later non-gil operation would silently reduce. The VM retains original u32 math.
            if !persistable_field_gil(expected_gil, next_gil) { return None; }
            // The known-bit encoding and the exact original predecessor must survive validation.
            fmv_ff7::story::compare_exchange_progression(progress, progress, &after.encode())?;
            Some(SceneTransaction { expected_gil, next_gil })
        } else { None };
        let mut summary = Vec::new();
        if after.moment != story.moment { summary.push(format!("moment={}->{}", story.moment, after.moment)); }
        for ((bank, address), value) in after.vars() {
            if story.var(bank, address) != value { summary.push(format!("var[{bank}][{address}]={value}")); }
        }
        let travel = if let Some(jump) = outcome.jump {
            let target = self.adapter.field_key_for_id(jump.field)?;
            // A scene sends its players only where the story goes on: a reviewed field (its own points and exits are
            // offered) or the field of another scene.
            if !fmv_ff7::is_reviewed_field(&target) && !self.defs.iter().any(|def| def.field.eq_ignore_ascii_case(&target)) { return None; }
            // The bridge's zero MAPJUMP is an entry into a required arrival scene, not a
            // grounded control position. Its director places hidden Cloud later. Keep the
            // player held through that arrival and use its reviewed control handoff as the
            // stored landing, including the real walkmesh triangle, for reconnect as well.
            let landing = if jump.x == 0 && jump.y == 0 && jump.triangle == 0 {
                self.defs.iter().find(|def| def.id == fmv_ff7::script::scene::CARGO_TRAIN_SCENE_ID
                    && def.field == target && def.starts_on_arrival() && def.offered_to(&after)).map(|def| def.arrival)
            } else { None };
            let (x, y, direction, hint) = match landing {
                Some(spot) => (spot.x, spot.y, spot.direction, Some(u32::from(spot.triangle))),
                None => (jump.x, jump.y, jump.direction, u32::try_from(jump.triangle).ok()),
            };
            Some(fmv_adapter_api::Exit {
                line: (Pos::new(0.0, 0.0, 0.0), Pos::new(0.0, 0.0, 0.0)), target,
                arrival: Pos::new(f32::from(x), f32::from(y), 0.0),
                arrival_facing: Some(field_direction_to_facing(direction)),
                arrival_hint: hint, kind: fmv_adapter_api::ExitKind::Gateway,
            })
        } else { None };
        Some(SceneResult { progress: after.encode(), changed, summary: summary.join(" "), travel, transaction })
    }
}

impl SceneHost for Ff7SceneHost {
    fn fresh_story(&self) -> Option<Vec<u8>> {
        self.extended_save.then(|| StoryState::fresh_first_control().encode())
    }

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
        if points::is_point_continuation(scene) {
            return self.exact_point_offered(def, progress);
        }
        (!(def.requires_exact_transaction() || story.supports_extended()) || self.extended_save)
            && def.prepare_story(&story).is_ok()
            && def.offered_to(&story)
    }

    fn automatic(&self, scene: &str) -> bool {
        self.def(scene).is_some_and(SceneDef::starts_on_arrival)
    }

    fn continuation(&self, scene: &str) -> bool {
        self.extended_save && self.def(scene).is_some_and(|def|
            def.requires_continuation() || points::is_early_continuation(&def.id))
    }

    fn continuation_for(&self, scene: &str, progress: &[u8]) -> bool {
        self.continuation(scene) && self.def(scene).is_some_and(|def|
            def.requires_continuation() || StoryState::decode(progress).is_some_and(|story| story.supports_extended()))
    }

    fn noncombat_continuation(&self, scene: &str) -> bool {
        points::is_noncombat_restore_scene(scene)
    }

    fn requires_origin(&self, scene: &str) -> bool {
        self.def(scene).is_some_and(|def| def.origin_actor().is_some())
    }

    fn requires_landing(&self, scene: &str) -> bool {
        self.def(scene).is_some_and(SceneDef::requires_landing)
    }

    fn requires_landing_for(&self, scene: &str, progress: &[u8]) -> bool {
        self.requires_landing(scene) || (scene == fmv_ff7::script::scene::STATION_SCENE_ID
            && self.continuation_for(scene, progress))
    }

    fn evaluate_continuation(&self, scene: &str, original_progress: &[u8], seed: u32, original_gil: u32,
        replay: Option<&fmv_protocol::SceneReplay>) -> Option<crate::world::SceneBoundary> {
        self.continuation_boundary(scene, original_progress, seed, original_gil, replay, None)
    }

    fn initial_continuation(&self, scene: &str, progress: &[u8], seed: u32, gil: u32,
        origin: Option<fmv_protocol::SceneOrigin>) -> Option<crate::world::SceneBoundary> {
        self.continuation_boundary(scene, progress, seed, gil, None, origin)
    }

    fn line_scene(&self, field: &str, progress: &[u8], from: Pos, to: Pos) -> Option<String> {
        self.defs.iter().find(|def| {
            if !def.field.eq_ignore_ascii_case(field) || !self.offered(&def.id, progress) || !self.continuation(&def.id) {
                return false;
            }
            let Some(line) = def.line_trigger() else { return false };
            let pos = |p: [i16; 3]| Pos::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
            fmv_adapter_api::StoryPoint { id: 0, label: String::new(),
                trigger: fmv_adapter_api::StoryTrigger::Line(pos(line.a), pos(line.b)) }.is_hit_by(from, to)
        }).map(|def| def.id.clone())
    }

    fn point_scene(&self, field: &str, before: &[u8], after: &[u8]) -> Option<String> {
        let (before, after) = (StoryState::decode(before)?, StoryState::decode(after)?);
        self.defs.iter().find(|d| d.presents_step(field, &before, &after)).map(|d| d.id.clone())
    }

    fn point_continuation(&self, field: &str, before: &[u8], point: u32) -> Option<String> {
        self.exact_point_scene(field, before, point).map(|def| def.id.clone())
    }

    fn victory_scene(&self, field: &str, before: &[u8], after: &[u8], formation: u16) -> Option<String> {
        let (before, after) = (StoryState::decode(before)?, StoryState::decode(after)?);
        self.defs.iter().find(|d| d.follows_battle_step(field, &before, &after, formation)).map(|d| d.id.clone())
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
        // Point scenes are admitted only by the area's authoritative trigger. Making their
        // director into an Interact target would bypass that trigger, even with radius zero.
        if def.is_point_cutscene() { return None; }
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
        Some(def.prepare_story(&StoryState::at(*def.trigger.moments.start())).ok()?.encode())
    }

    fn finish(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.finish_scene(scene, progress, seed, answers, false, None)
    }

    fn finish_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], gil: u32) -> Option<SceneResult> {
        self.finish_scene(scene, progress, seed, answers, false, Some(gil))
    }

    fn finish_preview_with_resources(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8], gil: u32) -> Option<SceneResult> {
        self.finish_scene(scene, progress, seed, answers, true, Some(gil))
    }

    fn finish_preview(&self, scene: &str, progress: &[u8], seed: u32, answers: &[u8]) -> Option<SceneResult> {
        self.finish_scene(scene, progress, seed, answers, true, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::{dialogue::End, scene::{Outcome, Stats, Writes}};

    #[test]
    fn incomplete_or_economic_scenes_cannot_advance_or_travel() {
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None, setup_stats: Stats::default(), questions: 0, unknown_persistent_reads: Default::default(), party: Vec::new(), resources: Default::default(), stats: Stats::default() };
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
        outcome.writes.persistent.insert((3, 66), (1, 1));
        assert!(!safe_outcome(&outcome), "extended assignments require the acknowledged transaction path");
        assert!(complete_outcome(&outcome), "extended preview still interprets normally");
        outcome.writes.persistent.clear();
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

    #[test]
    fn extended_commit_checks_all_persistent_banks_and_never_drops_a_resource_operation() {
        use fmv_ff7::script::scene::{GilOp, ResourceOutcome};
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None,
            setup_stats: Stats::default(), questions: 0, unknown_persistent_reads: Default::default(),
            party: Vec::new(), resources: ResourceOutcome { gil_before: Some(1), gil_after: Some(0),
                gil_ops: vec![GilOp::Subtract(1)], unknown_gil: false }, stats: Stats::default() };
        outcome.writes.persistent.insert((3, 111), (1, 1));
        assert!(safe_interpretation(&outcome));
        assert!(!safe_outcome(&outcome), "legacy saver cannot carry economic effects");
        for bank in [1, 2, 3, 4, 7, 11, 12, 13, 14, 15] {
            outcome.writes.untracked.insert((bank, 66), 1);
            assert!(!safe_interpretation(&outcome), "persistent selector {bank} cannot be dropped");
            outcome.writes.untracked.clear();
        }
        outcome.resources.unknown_gil = true;
        assert!(!complete_outcome(&outcome), "unknown gil cannot choose even a preview branch");
    }

    #[test]
    fn battle_segments_require_known_state_and_cannot_discard_a_restore() {
        let mut outcome = Outcome { end: End::Battle, windows: 2, writes: Writes::default(), jump: None,
            setup_stats: Stats::default(), questions: 1, unknown_persistent_reads: Default::default(),
            party: Vec::new(), resources: Default::default(), stats: Stats::default() };
        assert!(!complete_outcome(&outcome), "a battle is not a completed scene");
        assert!(safe_segment(&outcome, false), "a completely evaluated battle can be checkpointed");
        outcome.unknown_persistent_reads.insert((3, 66));
        assert!(!safe_segment(&outcome, false), "a guessed branch cannot start a durable fight");
        outcome.unknown_persistent_reads.clear();
        for op in ["HMPMAX1", "HPu", "STITM", "GOLDd", "MENU"] {
            outcome.stats.ignored.insert(op, 1);
            assert!(!safe_segment(&outcome, false), "continuation cannot discard {op}");
            if op == "MENU" {
                assert!(!safe_segment(&outcome, true), "a noncombat restore exception does not authorize an unhandled menu");
            }
            outcome.stats.ignored.clear();
        }
        outcome.setup_stats.setup_incomplete = true;
        assert!(!safe_segment(&outcome, false), "all stage setup must also finish");
    }

    #[test]
    fn scene_commits_refuse_balances_other_save_paths_would_silently_reduce() {
        let cap = fmv_ff7_battle::kernel::MAX_GIL;
        assert!(persistable_field_gil(0, cap) && persistable_field_gil(cap, 0));
        assert!(!persistable_field_gil(cap + 1, cap));
        assert!(!persistable_field_gil(cap, cap + 1));
        assert!(!persistable_field_gil(u32::MAX, u32::MAX));
    }

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; reads owned fields and uses throwaway state only"]
    fn owned_flower_choices_produce_exact_durable_resource_results() {
        use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
        use crate::saver::{persist_scene, SceneSaveJob};
        use fmv_core::GameId;
        use fmv_persist::{CharacterProgress, Progression, Store};
        use fmv_ff7::script::scene::FLOWER_STATION_SCENE_ID;
        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
        let mut host = Ff7SceneHost::with_extended_save(opened.adapter, true);
        assert!(host.def(FLOWER_STATION_SCENE_ID).is_some());
        for before in [StoryState::at(36).encode(), StoryState::at(36).into_extended().unwrap().encode()] {
            for gil in [0u32, 1, 600] {
                for answers in [&[][..], &[3][..], &[3, 2][..], &[3, 2, 9][..], &[0][..], &[2, 0][..], &[3, 2, 0, 0][..]] {
                    assert!(host.finish_with_resources(FLOWER_STATION_SCENE_ID, &before, 1, answers, gil).is_none(),
                        "incomplete, out-of-range or trailing answers cannot produce a durable outcome: {answers:?}");
                    assert!(host.finish_preview_with_resources(FLOWER_STATION_SCENE_ID, &before, 1, answers, gil).is_some(),
                        "previews retain cursor defaults and clamping: {answers:?}");
                }
                for (answers, bought) in [(&[2u8][..], false), (&[3, 2, 0][..], true), (&[3, 3, 0][..], true), (&[3, 2, 1][..], false), (&[3, 3, 1][..], false)] {
                    let result = host.finish_with_resources(FLOWER_STATION_SCENE_ID, &before, 1, answers, gil).expect("complete original scene");
                    let next_gil = if bought { gil.saturating_sub(1) } else { gil };
                    assert_eq!(result.transaction, Some(SceneTransaction { expected_gil: gil, next_gil }));
                    assert!(result.changed && result.travel.is_none());
                    let dir = tempfile::tempdir().unwrap();
                    let store = Store::open(dir.path()).unwrap();
                    let (account, _token) = store.create_account("Flower test", 1).unwrap();
                    store.put_progression(&Progression::new(account, GameId::Ff7, before.clone(), 1)).unwrap();
                    let character = CharacterProgress { account_id: account, game: GameId::Ff7, gil, ..CharacterProgress::new(account, GameId::Ff7, 0, 6, 1) };
                    store.put_character_progress(&character).unwrap();
                    let job = SceneSaveJob { account, scene_key: FLOWER_STATION_SCENE_ID.into(), expected_story: before.clone(),
                        next_story: result.progress.clone(), expected_gil: gil, next_gil };
                    let saved = persist_scene(&store, GameId::Ff7, &job, 2).unwrap();
                    assert_eq!(saved.pair.character.as_ref().unwrap().gil, next_gil);
                    assert_eq!(saved.pair.story.blob, result.progress);
                    drop(store);
                    let store = Store::open(dir.path()).unwrap();
                    let repeated = persist_scene(&store, GameId::Ff7, &job, 3).unwrap();
                    assert_eq!(repeated.commit, fmv_persist::SceneCommit::AlreadyApplied);
                    assert_eq!(repeated.pair, saved.pair, "reconnect never charges twice");
                    let preview = host.finish_preview_with_resources(FLOWER_STATION_SCENE_ID, &before, 1, answers, gil).unwrap();
                    assert!(preview.transaction.is_none());
                }
            }
            assert!(host.finish(FLOWER_STATION_SCENE_ID, &before, 1, &[3, 2, 0]).is_none(), "no gil snapshot");
        }
        let before = StoryState::at(36).encode();
        host.extended_save = false;
        assert!(!host.offered(FLOWER_STATION_SCENE_ID, &before), "disabled unless the extended save is enabled");
        assert!(host.finish_with_resources(FLOWER_STATION_SCENE_ID, &before, 1, &[2], 1).is_none(), "explicit v3 save gate");
        let legacy = StoryState::at(33);
        let scene = fmv_ff7::script::scene::POST_ESCAPE_SCENE_ID;
        assert!(host.offered(scene, &legacy.encode()), "legacy reactor route stays available with the flag off");
        assert!(!host.offered(scene, &legacy.into_extended().unwrap().encode()), "a scene that cannot commit its existing v3 snapshot must not be offered");
    }

    /// A scene's party change is accepted only where the story's own party table has the party the scripts leave: the
    /// table is how the change is carried, so a change it has no row for is refused, not dropped.
    #[test]
    fn a_party_change_is_carried_by_the_storys_party_table_or_refused() {
        use fmv_ff7::story::{PartyOp, BARRET, CLOUD};
        let mut outcome = Outcome { end: End::Finished, windows: 0, writes: Writes::default(), jump: None, setup_stats: Stats::default(), questions: 0, unknown_persistent_reads: Default::default(), party: Vec::new(), resources: Default::default(), stats: Stats::default() };
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
