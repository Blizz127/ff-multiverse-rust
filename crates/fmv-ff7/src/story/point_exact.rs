//! Full persistent assignments for the existing field points. Legacy discovery selects an
//! authored entry and its trigger; it never defines the exact write set.

use super::*;
use crate::script::dialogue::{ExactBattle, ExactError, ExactRun, ExactScript};
use fmv_adapter_api::{StoryBattleAction, StoryBattlePolicy};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoryPointError {
    #[error("ordinary exact story points require a supported extended predecessor")]
    Story,
    #[error("the native main party at BATTLE is not fully known")]
    UnknownBattleParty,
    #[error("the script does not match the source from which these points were discovered")]
    Source,
    #[error("the point has no unambiguous authored entry or authoritative triangle input")]
    Trigger,
    #[error(transparent)]
    Execution(#[from] ExactError),
    #[error(transparent)]
    Write(#[from] StoryWriteError),
    #[error("the resulting story failed exact replacement validation")]
    Replacement,
    #[error("the battle uses unimplemented script mode flags {0:#06x}")]
    BattleMode(u16),
    #[error("the authored party does not match the resulting story's authoritative party")]
    Party,
    #[error("the authored unlocked-member effects do not match the reviewed early-route state")]
    PartyVisibility,
}

struct EvaluatedPoint {
    step: StoryStep,
    battle: Option<ExactBattle>,
    battle_progress: Option<Vec<u8>>,
}

impl AreaStory {
    /// Evaluate an ordinary exact point without discarding unsupported state effects.
    /// The detailed error is available to route audits; the ordinary adapter API withholds
    /// failed points. Nothing is written here. The caller must commit the exact predecessor
    /// and the returned replacement atomically before publishing the step or its countdown.
    pub fn advance_exact_with(&self, script: &FieldScript<'_>, blob: &[u8], id: u32) -> Result<Option<StoryStep>, StoryPointError> {
        Ok(self.evaluate_exact_with(script, blob, id)?.map(|evaluated| evaluated.step))
    }

    /// Effective policy captured at the source's BATTLE, rather than any later BTLMD.
    /// Unknown modes refuse the point as well as the battle: a caller cannot save a story
    /// result whose required battle rules it cannot enforce.
    pub fn battle_policy(&self, blob: &[u8], id: u32) -> Option<StoryBattlePolicy> {
        let state = StoryState::decode(blob)?;
        if !state.supports_extended() {
            return self.battle_for(blob, id).map(|formation| StoryBattlePolicy {
                formation, escape_forbidden: false, victory_fanfare: true, game_over_on_defeat: false,
            });
        }
        Some(self.battle_action(blob, id)?.policy)
    }

    /// Capture the battle's roster state and the final victory result in one execution.
    /// The intermediate native snapshot selects combatants only; the complete victory
    /// afterimage still commits against the original bytes in the battle transaction.
    pub fn battle_action(&self, blob: &[u8], id: u32) -> Option<StoryBattleAction> {
        let state = StoryState::decode(blob)?;
        if !state.supports_extended() {
            let victory = match FieldScript::parse(&self.source.0) {
                Ok(script) => self.advance_with(&script, blob, id)?,
                Err(_) => self.advance(blob, id)?,
            };
            return Some(StoryBattleAction { policy: self.battle_policy(blob, id)?,
                battle_progress: None, victory });
        }
        let script = FieldScript::parse(&self.source.0).ok()?;
        let EvaluatedPoint { step: victory, battle, battle_progress } = self.evaluate_exact_with(&script, blob, id).ok()??;
        let battle = battle?;
        let policy = StoryBattlePolicy {
            formation: battle.formation, escape_forbidden: battle.flags & 0x08 != 0,
            victory_fanfare: battle.flags & 0x20 == 0,
            // Accepted modes have high byte zero: original field return sends defeat
            // to GAME_OVER, never back to the pending script or ordinary field control.
            game_over_on_defeat: true,
        };
        Some(StoryBattleAction { policy, battle_progress, victory })
    }

    fn evaluate_exact_with(&self, script: &FieldScript<'_>, blob: &[u8], id: u32)
        -> Result<Option<EvaluatedPoint>, StoryPointError> {
        let state = StoryState::decode(blob).filter(StoryState::supports_extended).ok_or(StoryPointError::Story)?;
        if self.source.0 != script.bytes() { return Err(StoryPointError::Source); }
        let Some(point) = usize::try_from(id).ok().and_then(|id| self.points.get(id)) else { return Ok(None) };
        let run = match &point.trigger {
            Trigger::Talk { entity, .. } => match dialogue::run_talk_exact(script, *entity, &state, 1, &[]) {
                Err(ExactError::NotTalkable) => return Ok(None),
                result => result?,
            },
            Trigger::Panel { entity, triangle, .. } => {
                let source = point.source.as_ref().ok_or(StoryPointError::Trigger)?;
                if source.triangle_vars.len() != 1 { return Err(StoryPointError::Trigger); }
                dialogue::run_panel_exact(script, *entity, &state, 1,
                    u16::try_from(*triangle).map_err(|_| StoryPointError::Trigger)?, &source.triangle_vars)?
            }
            _ => {
                // The existing reviewed point must apply; running an entire main loop is not
                // permission to activate a different point or bypass its trigger/guards.
                if point.apply(&state).is_none() { return Ok(None); }
                let source = point.source.as_ref().ok_or(StoryPointError::Trigger)?;
                let mut entry = ExactScript {
                    entity: source.entity, slot: source.slot, start_offset: source.start_offset,
                    setup: source.setup, after_battle: point.battle_first.or(point.battle), countdown: point.countdown,
                    triangle_sources: source.triangle_sources.clone(),
                    ..ExactScript::default()
                };
                if let Trigger::Triangle { triangle, wants_ok, .. } = point.trigger {
                    if source.triangle_vars.len() != 1 { return Err(StoryPointError::Trigger); }
                    let [lo, hi] = u16::try_from(triangle).map_err(|_| StoryPointError::Trigger)?.to_le_bytes();
                    for &(bank, address) in &source.triangle_vars {
                        let high = address.checked_add(1).ok_or(StoryPointError::Trigger)?;
                        entry.seeds.extend([((bank, address), lo), ((bank, high), hi)]);
                    }
                    if wants_ok { entry.keys = dialogue::KEY_OK; }
                }
                dialogue::run_script_exact(script, &state, 1, &entry, &[])?
            }
        };
        let battle = run.battle;
        if let Some(battle) = battle {
            // 0x08 forbids escape, 0x20 suppresses the victory fanfare. The countdown and
            // defeat-return modes need additional runtime support and cannot disappear.
            if battle.flags & !0x28 != 0 { return Err(StoryPointError::BattleMode(battle.flags)); }
        }
        let battle_progress = if state.supports_native_party() && battle.is_some() {
            let at_battle = run.battle_story.as_ref().filter(|story| story.supports_native_party())
                .ok_or(StoryPointError::UnknownBattleParty)?;
            let progress = at_battle.encode();
            checked_canon_party_of(&progress).ok_or(StoryPointError::UnknownBattleParty)?;
            Some(progress)
        } else { None };
        let next = apply(&state, &run)?;
        if !state.supports_native_party() {
            let party = party_after(&canon_party(&state), &run.party);
            if party != canon_party(&next) { return Err(StoryPointError::Party); }
            if !party_visibility_matches(&state, &next, &run.party) { return Err(StoryPointError::PartyVisibility); }
        }
        if next == state { return Ok(None); }
        if matches!(point.trigger, Trigger::Panel { .. }) && !is_return_ride(&state, &next) { return Ok(None); }
        let progress = next.encode();
        compare_exchange_progression(blob, blob, &progress).ok_or(StoryPointError::Replacement)?;
        Ok(Some(EvaluatedPoint { step: StoryStep { summary: summary(&state, &next), progress, countdown: point.countdown }, battle, battle_progress }))
    }
}

fn apply(state: &StoryState, run: &ExactRun) -> Result<StoryState, StoryPointError> {
    let mut next = state.clone();
    if state.supports_native_party() {
        let journal = run.native.as_ref().ok_or(StoryPointError::Replacement)?;
        journal.apply_to(&mut next, state).map_err(ExactError::from)?;
    } else {
        for write in &run.writes { next.write_masked(write.bank, write.address, write.value, write.mask)?; }
    }
    Ok(next)
}

/// Preserve the old panel sibling's trigger role: it is the return ride. Its complete exact
/// delta is kept, including other authored writes, but it cannot also take the forward ride.
fn is_return_ride(before: &StoryState, after: &StoryState) -> bool {
    if after.moment > before.moment { return false; }
    let mut cleared = false;
    for tracked in TRACKED_VARS {
        for bit in 0..8 {
            if tracked.bits & (1 << bit) == 0 { continue; }
            match (before.known_bit(tracked.bank, tracked.address, bit), after.known_bit(tracked.bank, tracked.address, bit)) {
                (Some(true), Some(false)) => cleared = true,
                (Some(false), Some(true)) => return false,
                _ => {}
            }
        }
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::synth::SynthScript;

    fn word(bank: u8, address: u8, value: u16) -> Vec<u8> {
        [vec![0x81, bank << 4, address], value.to_le_bytes().to_vec()].concat()
    }

    fn at_six(body: Vec<u8>) -> Vec<u8> {
        at_moment(6, body)
    }

    fn at_moment(moment: u8, body: Vec<u8>) -> Vec<u8> {
        [vec![0x16, 0x20, 0, 0, moment, 0, 0, u8::try_from(body.len() + 1).unwrap()], body, vec![0]].concat()
    }

    fn model(bytes: &[u8]) -> AreaStory {
        AreaStory::scan(&FieldScript::parse(bytes).unwrap(), &Walkmesh { triangles: vec![], access: vec![] })
    }

    #[test]
    fn exact_points_keep_nontracked_assignments_clears_and_unrelated_unknown_bits() {
        let body = [word(2, 0, 7), vec![0x80, 0x30, 70, 13], vec![0x83, 0x30, 71, 2], vec![0x82, 0x30, 72, 6]].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, at_six(body))]).build();
        let area = model(&bytes);
        assert_eq!(area.point_count(), 1, "the extra writes do not change existing point IDs");
        let mut before = StoryState::at(6).into_extended().unwrap();
        before.write_masked(3, 70, 128, 128).unwrap();
        before.write_byte(3, 71, 255).unwrap();
        before.write_masked(7, 80, 1, 5).unwrap();
        let expected = before.encode();
        let step = area.advance(&expected, 0).expect("exact source evaluation");
        let after = StoryState::decode(&step.progress).unwrap();
        assert_eq!(after.moment, 7);
        assert_eq!(after.known_byte(3, 70), Some(13));
        assert_eq!(after.known_byte(4, 71), Some(251));
        assert_eq!((after.var(3, 72), after.known_mask(3, 72)), (64, 64));
        assert_eq!((after.var(7, 80), after.known_mask(7, 80)), (1, 5));
        assert_eq!(after.known_mask(7, 81), 0, "unrelated unknowns stay unknown");
        assert_eq!(compare_exchange_progression(&expected, &expected, &step.progress), Some(step.progress.clone()));
        assert!(compare_exchange_progression(&step.progress, &expected, &step.progress).is_none(), "stale bases refuse");
        assert!(area.advance(&step.progress, 0).is_none(), "the existing moment guard prevents replay");
        let legacy = area.advance(&StoryState::at(6).encode(), 0).unwrap();
        assert_eq!(legacy.progress, StoryState::at(7).encode(), "legacy interpretation is unchanged");
    }

    #[test]
    fn unknown_input_or_a_different_script_cannot_publish_a_curated_partial_step() {
        let body = [vec![0x80, 0x53, 10, 70], word(2, 0, 7)].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, at_six(body))]).build();
        let area = model(&bytes);
        let before = StoryState::at(6).into_extended().unwrap().encode();
        let script = FieldScript::parse(&bytes).unwrap();
        assert!(matches!(area.advance_exact_with(&script, &before, 0),
            Err(StoryPointError::Execution(ExactError::UnknownRead { bank: 3, address: 70, .. }))));
        assert!(area.advance(&before, 0).is_none(), "no fallback to the discovered moment write");
        let other = SynthScript::default().entity("other", vec![(0, at_six(word(2, 0, 7)))]).build();
        assert_eq!(area.advance_exact_with(&FieldScript::parse(&other).unwrap(), &before, 0), Err(StoryPointError::Source));
    }

    #[test]
    fn main_points_must_not_rerun_persistent_initialization_at_interaction_time() {
        let code = [word(3, 70, 1), vec![0], at_six(word(2, 0, 7))].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, code)]).build();
        let area = model(&bytes);
        let script = FieldScript::parse(&bytes).unwrap();
        let mut state = StoryState::at(6).into_extended().unwrap();
        state.write_word(3, 70, 0).unwrap();
        assert!(matches!(area.advance_exact_with(&script, &state.encode(), 0),
            Err(StoryPointError::Execution(ExactError::SetupWrite { bank: 3, address: 70 }))));
        state.write_word(3, 70, 1).unwrap();
        assert_eq!(StoryState::decode(&area.advance_exact_with(&script, &state.encode(), 0).unwrap().unwrap().progress).unwrap().moment, 7);
    }

    #[test]
    fn scene_admission_returns_only_the_guarded_authored_root() {
        let code = [vec![0], at_six(word(2, 0, 7))].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, code)]).build();
        let area = model(&bytes);
        let before = StoryState::at(6).into_extended().unwrap().encode();
        assert_eq!(area.point_source(&before, 0), Some(StoryPointSource { entity: 0, slot: 0, main: true }));
        assert_eq!(area.point_source(&StoryState::at(6).encode(), 0), None, "legacy scenes keep their existing admission path");
        assert_eq!(area.point_source(&StoryState::at(7).into_extended().unwrap().encode(), 0), None, "the original guard no longer holds");
        assert_eq!(area.point_source(&before, 99), None);
    }

    #[test]
    fn battle_policy_uses_the_mode_at_battle_and_unknown_modes_refuse_the_entire_step() {
        let code = |flags: u16| {
            let mode = [vec![0x72], flags.to_le_bytes().to_vec(), vec![0]].concat();
            let body = [word(2, 0, 7), vec![0x70, 0, 44, 1], vec![0x72, 34, 0]].concat();
            SynthScript::default().entity("director", vec![(0, [mode, at_six(body)].concat())]).build()
        };
        let before = StoryState::at(6).into_extended().unwrap().encode();
        for (flags, escape_forbidden, victory_fanfare) in [(0, false, true), (32, false, false), (40, true, false)] {
            let bytes = code(flags);
            let area = model(&bytes);
            assert!(area.advance(&before, 0).is_some(), "supported mode {flags}");
            assert_eq!(area.battle_policy(&before, 0), Some(StoryBattlePolicy { formation: 300, escape_forbidden, victory_fanfare, game_over_on_defeat: true }));
        }
        let bytes = code(2);
        let area = model(&bytes);
        assert!(matches!(area.advance_exact_with(&FieldScript::parse(&bytes).unwrap(), &before, 0), Err(StoryPointError::BattleMode(2))));
        assert!(area.advance(&before, 0).is_none() && area.battle_policy(&before, 0).is_none(), "no partial story save after an unsupported countdown battle");
    }

    #[test]
    fn legacy_battle_action_keeps_the_discovered_step_when_source_cannot_be_parsed() {
        let body = [vec![0x70, 0, 44, 1], word(2, 0, 7)].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, at_six(body))]).build();
        let mut area = model(&bytes);
        let before = StoryState::at(6).encode();
        let expected = area.battle_action(&before, 0).expect("legacy battle point");
        assert!(expected.battle_progress.is_none());
        assert_eq!(expected.policy.formation, 300);
        area.source.0.clear();
        assert_eq!(area.battle_action(&before, 0), Some(expected), "legacy discovery remains its fallback");
        let exact = StoryState::at(6).into_extended().unwrap().encode();
        assert!(area.battle_action(&exact, 0).is_none(), "exact execution never falls back to discovery");
    }

    #[test]
    fn native_party_battles_require_known_main_slots_without_substituting_the_legacy_profile() {
        let mode = vec![0x72, 8, 0, 0];
        let body = [word(2, 0, 7), vec![0x70, 0, 44, 1]].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, [mode, at_six(body)].concat())]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let area = model(&bytes);
        assert_eq!(area.point_count(), 1);
        let v3 = StoryState::at(6).into_extended().unwrap();
        let v3_blob = v3.encode();
        assert_eq!(StoryState::decode(&area.advance_exact_with(&script, &v3_blob, 0).unwrap().unwrap().progress).unwrap().moment, 7);
        assert!(area.point_source(&v3_blob, 0).is_some());
        assert_eq!(area.battle_policy(&v3_blob, 0), Some(StoryBattlePolicy {
            formation: 300, escape_forbidden: true, victory_fanfare: true, game_over_on_defeat: true,
        }), "the supported v3 point and battle are unchanged");
        assert!(area.battle_action(&v3_blob, 0).unwrap().battle_progress.is_none(),
            "v3 keeps the online leader's canon roster");

        let cases = [
            NativePartyState::unknown(),
            NativePartyState::from_parts([2, 255, 0], 7, (0x8004, 0xffff), (0x4000, 0xffff)).unwrap(),
            NativePartyState::from_parts([0, 255, 255], 7, (0x8000, 0x8001), (0, 0x4000)).unwrap(),
            NativePartyState::from_parts([255, 255, 255], 7, (0, 0xffff), (0xffff, 0xffff)).unwrap(),
        ];
        for resources in cases {
            let mut native = v3.clone().into_native_party().unwrap();
            native.replace_native_party(&NativePartyState::unknown(), resources).unwrap();
            // The raw field tuple is deliberately independent of every main tuple above.
            for (address, value) in [(9, 1), (10, 0), (11, 255)] { native.write_byte(3, address, value).unwrap(); }
            let before = native.encode();
            assert_eq!(StoryState::decode(&before).unwrap(), native);
            if resources.known_main().is_none() {
                assert_eq!(area.advance_exact_with(&script, &before, 0), Err(StoryPointError::UnknownBattleParty));
                assert!(area.advance(&before, 0).is_none());
                assert!(area.advance_with(&script, &before, 0).is_none());
                assert!(area.battle_policy(&before, 0).is_none() && area.battle_action(&before, 0).is_none(),
                    "no battle or final story can bypass an unknown battle roster");
            } else {
                let action = area.battle_action(&before, 0).expect("known native battle roster");
                let at_battle = StoryState::decode(action.battle_progress.as_ref().unwrap()).unwrap();
                assert_eq!(at_battle.native_party(), Some(&resources));
                assert_eq!(at_battle.known_byte(3, 9), Some(1), "raw slots remain independent of main");
                assert_eq!(area.advance_exact_with(&script, &before, 0).unwrap(), Some(action.victory.clone()));
                let after = StoryState::decode(&action.victory.progress).unwrap();
                assert_eq!(after.moment, 7);
                assert_eq!(after.native_party(), Some(&resources));
            }
            assert!(area.point_source(&before, 0).is_none(), "scene source admission remains gated separately");
            assert_eq!(native.encode(), before, "evaluation changes no source bytes or resource knowledge");
        }
        assert_eq!(v3.encode(), v3_blob);
    }

    #[test]
    fn native_battle_action_keeps_its_prefix_roster_and_commits_the_joint_final_afterimage() {
        let body = [
            word(2, 0, 7), vec![0x80, 0x30, 70, 13],
            vec![0xCA, CLOUD, 2, 254], vec![0x70, 0, 44, 1],
            vec![0xCA, CLOUD, 254, 254], vec![0x80, 0x30, 10, BARRET],
            vec![0xCE, 2], vec![0x80, 0x30, 70, 22],
        ].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, [vec![0x72, 8, 0, 0], at_six(body)].concat())]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let area = model(&bytes);
        assert_eq!(area.point_count(), 1, "the existing guarded moment transition discovers the action");
        let mut before = StoryState::at(6).into_native_party().unwrap();
        *before.native_party_mut().unwrap() = NativePartyState::from_parts(
            [CLOUD, 255, 255], 7, (0x8001, 0xffff), (0x4000, 0x4000),
        ).unwrap();
        for (address, value) in [(9, CLOUD), (10, BARRET), (11, 255)] { before.write_byte(3, address, value).unwrap(); }
        before.write_masked(7, 80, 1, 5).unwrap();
        let original = before.encode();
        let action = area.battle_action(&original, 0).expect("complete native action");
        assert_eq!(action.policy, StoryBattlePolicy {
            formation: 300, escape_forbidden: true, victory_fanfare: true, game_over_on_defeat: true,
        });
        let at_battle = StoryState::decode(action.battle_progress.as_ref().unwrap()).unwrap();
        assert_eq!(checked_canon_party_of(action.battle_progress.as_ref().unwrap()), Some(vec![CLOUD, 2]));
        assert_eq!(at_battle.native_party().unwrap().known_main(), Some([CLOUD, 2, 255]));
        assert_eq!(at_battle.known_byte(3, 70), Some(13));
        assert_eq!(at_battle.native_party().unwrap().locking_bits(), (0x4000, 0x4000));

        let after = StoryState::decode(&action.victory.progress).unwrap();
        assert_eq!(after.moment, 7);
        assert_eq!(after.native_party().unwrap().known_main(), Some([CLOUD, 255, 255]));
        assert_eq!([9, 10, 11].map(|address| after.known_byte(3, address)), [Some(CLOUD), Some(BARRET), Some(255)],
            "the final raw tuple is restored without overwriting the independently synchronized main tuple");
        assert_eq!(after.native_party().unwrap().availability_bits(), (0x8005, 0xffff),
            "temporary party membership still grants its authored availability");
        assert_eq!(after.native_party().unwrap().locking_bits(), (0x4004, 0x4004));
        assert_eq!(after.known_byte(3, 70), Some(22));
        assert_eq!((after.var(7, 80), after.known_mask(7, 80)), (1, 5));
        assert_eq!(area.advance_exact_with(&script, &original, 0).unwrap(), Some(action.victory.clone()));
        assert_eq!(compare_exchange_progression(&original, &original, &action.victory.progress), Some(action.victory.progress.clone()));
        let mut stale = before.clone();
        stale.native_party_mut().unwrap().write_locks_masked(4, 4);
        assert!(compare_exchange_progression(&stale.encode(), &original, &action.victory.progress).is_none(),
            "party-resource-only changes invalidate the complete predecessor");
        assert!(area.battle_action(&action.victory.progress, 0).is_none(), "the original guard prevents repetition");
        assert_eq!(before.encode(), original, "the battle prefix is never installed into the caller's story");
    }

    #[test]
    fn the_reactor_party_join_requires_the_authored_assignment_and_matching_final_story() {
        let before = StoryState::at(10).into_extended().unwrap().encode();
        assert_eq!(canon_party_of(&before), vec![CLOUD]);
        let make = |body| SynthScript::default().entity("door", vec![(0, at_moment(10, body))]).build();
        let without_party = make(word(2, 0, 11));
        let area = model(&without_party);
        assert_eq!(area.advance_exact_with(&FieldScript::parse(&without_party).unwrap(), &before, 0), Err(StoryPointError::Party));
        let with_party = make([vec![0xCA, CLOUD, BARRET, 254], word(2, 0, 11)].concat());
        let area = model(&with_party);
        let step = area.advance(&before, 0).expect("both the party assignment and its resulting moment are authored");
        assert_eq!(canon_party_of(&step.progress), vec![CLOUD, BARRET]);
        let wrong_party = make([vec![0xCA, CLOUD, 2, 254], word(2, 0, 11)].concat());
        assert_eq!(model(&wrong_party).advance_exact_with(&FieldScript::parse(&wrong_party).unwrap(), &before, 0), Err(StoryPointError::Party));
    }

    #[test]
    fn exact_points_refuse_unrepresented_unlocks_even_when_the_active_party_returns_to_normal() {
        // Ordinary exact points support full PRTYE assignments; PRTYM belongs to the
        // full scene interpreter. Replace the temporary party explicitly so this reaches
        // the visibility check instead of stopping at an unsupported opcode first.
        let body = [vec![0xCA, CLOUD, 2, 254, 0xCA, CLOUD, 254, 254], word(2, 0, 7)].concat();
        let bytes = SynthScript::default().entity("director", vec![(0, at_six(body))]).build();
        let before = StoryState::at(6).into_extended().unwrap().encode();
        let area = model(&bytes);
        assert_eq!(area.advance_exact_with(&FieldScript::parse(&bytes).unwrap(), &before, 0), Err(StoryPointError::PartyVisibility));
        assert!(area.advance(&before, 0).is_none(), "a temporary Tifa addition still unlocks her");

        let bytes = SynthScript::default().entity("later", vec![(0, at_moment(63, word(2, 0, 64)))]).build();
        let before = StoryState::at(63).into_extended().unwrap().encode();
        assert_eq!(model(&bytes).advance_exact_with(&FieldScript::parse(&bytes).unwrap(), &before, 0), Err(StoryPointError::PartyVisibility));
    }
}
