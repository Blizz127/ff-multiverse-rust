//! Validate a durable transcript against the original field script. The client reports window
//! acknowledgments only; battle records reach this adapter from acknowledged server settlements.

use super::{persistable_field_gil, safe_segment, Ff7SceneHost};
use crate::world::{SceneBoundary, SceneBoundaryKind, SceneHost};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    BattleResolution, BattleResume, Outcome, SceneBoundary as ScriptBoundary, ScenePlay,
    SceneResources, SceneSnapshot, WindowAck,
};
use fmv_ff7::story::StoryState;
use fmv_protocol::{SceneOrigin, SceneReplay, SceneReplayEvent, MAX_SCENE_STEPS};

impl Ff7SceneHost {
    pub(super) fn continuation_boundary(&self, scene: &str, original_progress: &[u8], seed: u32,
        original_gil: u32, replay: Option<&SceneReplay>, initial_origin: Option<SceneOrigin>) -> Option<SceneBoundary> {
        if !self.continuation(scene) || !self.offered(scene, original_progress)
            || !persistable_field_gil(original_gil, original_gil) { return None; }
        let def = self.def(scene)?;
        let original = def.prepare_story(&StoryState::decode(original_progress)?).ok()?;
        let area = self.field(&def.field)?;
        let origin = replay.map_or(initial_origin, |replay| replay.origin);
        let def = if def.id == fmv_ff7::script::scene::BRIDGE_SCENE_ID {
            let origin = origin?;
            origin.validate().ok()?;
            fmv_ff7::script::scene::with_trigger_position(area.field_file(), def, origin.pos, origin.facing, origin.triangle).ok()?
        } else { def.clone() };
        let mut play = ScenePlay::with_resources(area.field_file(), &def, &original, seed,
            SceneResources { gil: Some(original_gil) }).ok()?;
        let source = play.replay_identity().fingerprint;
        let events = match replay {
            Some(replay) if replay.source == source && replay.validate().is_ok() => &replay.events[..],
            Some(_) => return None,
            None => &[],
        };
        let mut segment_story = original;
        let mut segment_gil = original_gil;
        // Retain the original encoding for the first CAS; promotion itself is not a saved write.
        let mut segment_progress = original_progress.to_vec();
        play.next_strict(None).ok()?;
        for event in events {
            let snapshot = play.snapshot()?;
            let mark = snapshot.boundary;
            let (_, after, _) = checked_segment(snapshot, &segment_story, segment_gil)?;
            match (mark, event) {
                (ScriptBoundary::Window(mark), SceneReplayEvent::Window { step, identity, answer }) => {
                    if mark.ordinal.checked_sub(1)? != *step || *step >= MAX_SCENE_STEPS
                        || mark.identity_bytes() != *identity { return None; }
                    play.next_strict(Some(WindowAck { checkpoint: mark, answer: *answer })).ok()?;
                }
                (ScriptBoundary::Battle(mark), SceneReplayEvent::Battle { ordinal, identity, formation, progress, gil }) => {
                    if mark.ordinal != *ordinal || mark.formation != *formation || mark.flags != Some(8)
                        || mark.identity_bytes() != *identity || !persistable_field_gil(*gil, *gil) { return None; }
                    let stored = StoryState::decode(progress)?;
                    // Combat changes resources, not the field script's persistent selectors.
                    // A forged or mismatched story afterimage must not choose a later branch.
                    if stored != after || stored.encode() != *progress { return None; }
                    play.resume_battle_checked(BattleResume { checkpoint: mark,
                        resolution: BattleResolution::Victory, story: stored.clone(),
                        resources: SceneResources { gil: Some(*gil) } }).ok()?;
                    segment_story = stored;
                    segment_progress = progress.clone();
                    segment_gil = *gil;
                    play.next_strict(None).ok()?;
                }
                _ => return None,
            }
        }
        let snapshot = play.snapshot()?;
        let mark = snapshot.boundary;
        let (outcome, after, gil) = checked_segment(snapshot, &segment_story, segment_gil)?;
        let at = match mark {
            ScriptBoundary::Window(mark) => {
                let step = mark.ordinal.checked_sub(1)?;
                if step >= MAX_SCENE_STEPS { return None; }
                SceneBoundaryKind::Window { step, identity: mark.identity_bytes(), choices: mark.choices }
            }
            ScriptBoundary::Battle(mark) => {
                if mark.flags != Some(8) { return None; }
                SceneBoundaryKind::Battle { ordinal: mark.ordinal, identity: mark.identity_bytes(),
                    formation: mark.formation, flags: mark.flags }
            }
            ScriptBoundary::End(_) => SceneBoundaryKind::Complete(
                self.scene_result(&segment_progress, &segment_story, &outcome, false, Some(segment_gil))?),
        };
        Some(SceneBoundary { source, progress: after.encode(), gil, at })
    }
}

/// Validate and apply just this segment. Earlier committed resource operations have already
/// been removed by checked resume; cumulative interpreter failures remain grounds for refusal.
fn checked_segment(snapshot: SceneSnapshot, before: &StoryState, gil: u32) -> Option<(Outcome, StoryState, u32)> {
    let outcome = Outcome {
        end: match snapshot.boundary { ScriptBoundary::End(end) => end, _ => End::Battle },
        windows: snapshot.stats.windows, questions: 0, writes: snapshot.writes,
        jump: snapshot.jump, setup_stats: snapshot.setup_stats,
        unknown_persistent_reads: snapshot.unknown_persistent_reads, party: snapshot.party,
        resources: snapshot.resources, stats: snapshot.stats,
    };
    if !safe_segment(&outcome, false) || outcome.resources.gil_before != Some(gil) { return None; }
    let next_gil = outcome.resources.gil_after?;
    if !persistable_field_gil(gil, next_gil) { return None; }
    let mut after = before.clone();
    outcome.writes.apply_extended_to(&mut after, before)?;
    if !outcome.party_is_the_storys(before, &after) { return None; }
    Some((outcome, after, next_gil))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; reads owned scripts without writing live state"]
    fn owned_bridge_replay_validates_every_boundary_and_preserves_all_battle_rewards() {
        use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
        use fmv_ff7::script::scene::{bridge_scene, cargo_train_scene, BRIDGE_SCENE_ID};
        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
        let mut host = Ff7SceneHost::with_extended_save(opened.adapter, true);
        // Activation is tested separately; the adapter can be proved before registering the route.
        host.defs.retain(|def| def.id != BRIDGE_SCENE_ID && def.field != "cargoin");
        host.defs.extend([bridge_scene(), cargo_train_scene()]);
        assert!(!host.automatic(BRIDGE_SCENE_ID));
        let legacy = StoryState::at(39).encode();
        let extended = StoryState::at(39).into_extended().unwrap().encode();
        for original in [&legacy, &extended] {
            let at = |x, y| fmv_core::Pos::new(x, y, 518.0);
            assert_eq!(host.line_scene("md8brdg", original, at(-426.0, 840.0), at(-426.0, 830.0)), Some(BRIDGE_SCENE_ID.into()));
            assert!(host.line_scene("md8brdg", original, at(-700.0, 840.0), at(-700.0, 830.0)).is_none());
            assert!(host.line_scene("md8brdg", &StoryState::at(38).encode(), at(-426.0, 840.0), at(-426.0, 830.0)).is_none());
            for initial_gil in [0, 600] {
                for branches in 0..8u8 {
                    let answers = [if branches & 1 != 0 { 2 } else { 3 }, if branches & 2 != 0 { 0 } else { 1 }, if branches & 4 != 0 { 0 } else { 1 }];
                    let origin = SceneOrigin { pos: at(-426.0, 830.0), facing: 0.0, triangle: None };
                    let first = host.initial_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(origin)).expect("first boundary");
                    assert!(host.initial_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, None).is_none(),
                        "the survey midpoint cannot replace an authoritative crossing");
                    let mut replay = SceneReplay { source: first.source, origin: Some(origin), events: Vec::new() };
                    let mut question = 0usize;
                    let mut fought = Vec::new();
                    let mut completed = false;
                    for _ in 0..32 {
                        let boundary = host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&replay)).expect("valid prefix");
                        assert_eq!(boundary.gil, initial_gil + 17 * fought.len() as u32, "earlier rewards survive replay");
                        let mut wrong_source = replay.clone();
                        wrong_source.source[0] ^= 1;
                        assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&wrong_source)).is_none());
                        let mut wrong_origin = replay.clone();
                        wrong_origin.origin.as_mut().unwrap().facing += 1.0;
                        assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&wrong_origin)).is_none(),
                            "saved staging is bound to the same source identity");
                        let event = match boundary.at {
                            SceneBoundaryKind::Window { step, identity, choices } => {
                                let answer = choices.map(|_| { let answer = answers[question]; question += 1; answer });
                                let invalid = SceneReplayEvent::Window { step, identity: identity.clone(), answer: Some(255) };
                                let mut forged = replay.clone();
                                forged.events.push(invalid);
                                assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&forged)).is_none(), "bad answer cannot advance any window");
                                SceneReplayEvent::Window { step, identity, answer }
                            }
                            SceneBoundaryKind::Battle { ordinal, identity, formation, flags } => {
                                assert_eq!(flags, Some(8));
                                assert_eq!(ordinal as usize, fought.len() + 1);
                                fought.push(formation);
                                let progress = boundary.progress;
                                let gil = boundary.gil + 17;
                                let mut forged = replay.clone();
                                forged.events.push(SceneReplayEvent::Battle { ordinal, identity: identity.clone(), formation,
                                    progress: StoryState::at(48).into_extended().unwrap().encode(), gil });
                                assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&forged)).is_none(), "combat cannot rewrite field story");
                                SceneReplayEvent::Battle { ordinal, identity, formation, progress, gil }
                            }
                            SceneBoundaryKind::Complete(result) => {
                                assert_eq!(question, 3);
                                let expected: Vec<u16> = (0..3).filter(|bit| branches & (1 << bit) != 0).map(|bit| 328 + bit).collect();
                                assert_eq!(fought, expected);
                                assert_eq!(StoryState::decode(&result.progress).unwrap().moment, 39);
                                let travel = result.travel.unwrap();
                                assert_eq!(travel.target, "cargoin");
                                assert_eq!(travel.arrival, fmv_core::Pos::new(-24.0, 16.0, 0.0));
                                assert_eq!(travel.arrival_hint, Some(20), "the script's zero hint is not its grounded handoff");
                                let transaction = result.transaction.unwrap();
                                assert_eq!(transaction.expected_gil, boundary.gil);
                                assert_eq!(transaction.next_gil, boundary.gil);
                                let mut trailing = replay.clone();
                                trailing.events.push(replay.events.last().unwrap().clone());
                                assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&trailing)).is_none());
                                completed = true;
                                break;
                            }
                        };
                        let mut wrong_identity = event.clone();
                        match &mut wrong_identity {
                            SceneReplayEvent::Window { identity, .. } | SceneReplayEvent::Battle { identity, .. } => identity[0] ^= 1,
                        }
                        let mut forged = replay.clone();
                        forged.events.push(wrong_identity);
                        assert!(host.evaluate_continuation(BRIDGE_SCENE_ID, original, 1, initial_gil, Some(&forged)).is_none());
                        replay.events.push(event);
                    }
                    assert!(completed, "all eight authored branches reach the train");
                }
            }
        }

        // The linked arrival consumes the landing only after all 26 original windows and the
        // durable moment 48 handoff; no premature window acknowledgment can manufacture it.
        let cargo = fmv_ff7::script::scene::CARGO_TRAIN_SCENE_ID;
        let first = host.evaluate_continuation(cargo, &extended, 1, 651, None).expect("cargo arrival");
        let mut replay = SceneReplay { source: first.source, origin: None, events: Vec::new() };
        for step in 0..26 {
            let boundary = host.evaluate_continuation(cargo, &extended, 1, 651, Some(&replay)).expect("cargo window");
            let SceneBoundaryKind::Window { step: actual, identity, choices } = boundary.at else { panic!("arrival ended before window {step}") };
            assert_eq!(actual, step);
            assert!(choices.is_none());
            replay.events.push(SceneReplayEvent::Window { step, identity, answer: None });
        }
        let end = host.evaluate_continuation(cargo, &extended, 1, 651, Some(&replay)).expect("cargo finish");
        let SceneBoundaryKind::Complete(result) = end.at else { panic!("arrival did not finish after 26 windows") };
        assert_eq!(StoryState::decode(&result.progress).unwrap().moment, 48);
        assert_eq!(end.gil, 651);
        let entry = host.entry(&result.progress).expect("durable cargo entry");
        assert_eq!(entry.target, "cargoin");
        assert_eq!(entry.arrival, fmv_core::Pos::new(-24.0, 16.0, 0.0));
        assert_eq!(entry.arrival_hint, Some(20));
    }
}
