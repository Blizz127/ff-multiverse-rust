//! Bind existing point triggers to their full authored scene, without constructing the
//! legacy point's partial afterimage. The strict scene replay owns writes and MAPJUMP.

use super::Ff7SceneHost;
use fmv_adapter_api::AreaRuntime;
use fmv_ff7::script::scene::{SceneDef, Step, POST_ESCAPE_SCENE_ID, REACTOR_FINALE_SCENE_ID, STATION_SCENE_ID, TEAM_TALK_SCENE_ID};
use fmv_ff7::story::{StoryPointSource, StoryState};

pub(super) fn is_point_continuation(scene: &str) -> bool {
    [TEAM_TALK_SCENE_ID, REACTOR_FINALE_SCENE_ID].contains(&scene)
}

pub(super) fn is_early_continuation(scene: &str) -> bool {
    is_point_continuation(scene) || is_noncombat_restore_scene(scene)
}

pub(super) fn is_noncombat_restore_scene(scene: &str) -> bool {
    [POST_ESCAPE_SCENE_ID, STATION_SCENE_ID].contains(&scene)
}

fn matches_source(def: &SceneDef, source: StoryPointSource) -> bool {
    if !is_point_continuation(&def.id) || !def.is_point_cutscene() || def.resume_after_battle.is_some()
        || def.trigger.entity != source.entity || def.trigger.slot != source.slot {
        return false;
    }
    // These two reviewed points enter the main after its initial RET. The finale has other
    // actor mains as well; exactly one root must be the scanned trigger's own main.
    source.main && source.slot == 0
        && def.root.iter().filter(|step| matches!(step, Step::StartMain(entity) if *entity == source.entity)).count() == 1
}

impl Ff7SceneHost {
    pub(super) fn exact_point_scene(&self, field: &str, before: &[u8], point: u32) -> Option<&SceneDef> {
        if !self.extended_save || !StoryState::decode(before)?.supports_extended() { return None; }
        let area = self.field(field)?;
        let source = area.story_point_source(before, point)?;
        let story = StoryState::decode(before)?;
        let mut matches = self.defs.iter().filter(|def| def.field.eq_ignore_ascii_case(field)
            && def.applies_at(story.moment) && matches_source(def, source));
        let def = matches.next()?;
        if matches.next().is_some() { return None; }
        Some(def)
    }

    pub(super) fn exact_point_offered(&self, def: &SceneDef, before: &[u8]) -> bool {
        if !self.extended_save { return false; }
        let Some(area) = self.field(&def.field) else { return false };
        area.story_points().iter().any(|point|
            self.exact_point_scene(&def.field, before, point.id).is_some_and(|found| found.id == def.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::SceneHost;
    use fmv_ff7::script::scene::{reactor_finale_scene, team_talk_scene};

    #[test]
    fn point_scene_identity_requires_the_registered_trigger_main() {
        for (mut def, entity) in [(team_talk_scene(), 0), (reactor_finale_scene(), 7)] {
            let source = StoryPointSource { entity, slot: 0, main: true };
            assert!(matches_source(&def, source));
            assert!(!matches_source(&def, StoryPointSource { entity: entity + 1, ..source }));
            assert!(!matches_source(&def, StoryPointSource { slot: 1, ..source }));
            assert!(!matches_source(&def, StoryPointSource { main: false, ..source }));
            def.root.push(Step::StartMain(entity));
            assert!(!matches_source(&def, source), "ambiguous duplicated trigger root refuses");
            def.root.pop();
            def.id.push_str("-unreviewed");
            assert!(!matches_source(&def, source), "another scene does not inherit authority");
        }
    }

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; reads scripts without writing live state"]
    fn owned_point_admission_checks_source_guards_save_version_and_flag() {
        use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
        let mut host = Ff7SceneHost::with_extended_save(opened.adapter, true);
        for (field, point, moment, scene) in [("md1_1", 0, 6, TEAM_TALK_SCENE_ID), ("nrthmk", 1, 27, REACTOR_FINALE_SCENE_ID)] {
            let mut story = StoryState::fresh_first_control();
            story.moment = moment;
            let before = story.encode();
            assert_eq!(host.point_continuation(field, &before, point).as_deref(), Some(scene));
            assert!(host.offered(scene, &before) && host.continuation(scene));
            assert!(host.continuation_for(scene, &before));
            assert!(!host.continuation_for(scene, &StoryState::at(moment).encode()));
            assert!(!host.automatic(scene));
            assert!(host.trigger(scene, &before).is_none(), "no manufactured Interact target");
            assert!(host.point_continuation(field, &before, point + 1).is_none());
            assert!(host.point_continuation(field, &StoryState::at(moment).encode(), point).is_none());
            story.moment = if moment == 6 { 7 } else { 33 };
            assert!(host.point_continuation(field, &story.encode(), point).is_none(), "stale point refuses");
            host.extended_save = false;
            assert!(host.point_continuation(field, &before, point).is_none());
            assert!(!host.offered(scene, &before) && !host.continuation(scene));
            host.extended_save = true;
        }
        let mut expired = StoryState::fresh_first_control();
        expired.moment = 27;
        expired.write_masked(1, 225, 128, 128).unwrap();
        assert!(host.point_continuation("nrthmk", &expired.encode(), 1).is_none(), "the authored timeout guard still applies");
    }

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; reads scripts without writing live state"]
    fn owned_point_and_post_escape_continuations_retain_every_authored_transfer() {
        use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
        use crate::world::{SceneBoundaryKind, SceneResult};
        use fmv_protocol::{SceneOrigin, SceneReplay, SceneReplayEvent};

        fn finish(host: &Ff7SceneHost, scene: &str, before: &[u8], origin: Option<SceneOrigin>) -> (SceneResult, SceneReplay, usize) {
            let initial = host.initial_continuation(scene, before, 7, 617, origin)
                .unwrap_or_else(|| panic!("{scene}: strict initial boundary, origin={origin:?}"));
            let mut replay = SceneReplay { source: initial.source, origin, events: vec![] };
            for _ in 0..128 {
                let boundary = host.evaluate_continuation(scene, before, 7, 617, Some(&replay))
                    .unwrap_or_else(|| panic!("{scene}: strict replay after {} accepted events; last={:?}", replay.events.len(), replay.events.last()));
                assert_eq!(boundary.gil, 617, "this route has no authored gil operations");
                match boundary.at {
                    SceneBoundaryKind::Window { step, identity, choices: None } =>
                        replay.events.push(SceneReplayEvent::Window { step, identity, answer: None }),
                    SceneBoundaryKind::Complete(result) => {
                        let windows = replay.events.len();
                        return (result, replay, windows);
                    }
                    other => panic!("unexpected route boundary: {other:?}"),
                }
            }
            panic!("bounded original scene did not finish");
        }

        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
        let host = Ff7SceneHost::with_extended_save(opened.adapter, true);
        let mut story = StoryState::fresh_first_control();
        story.moment = 6;
        let origin = SceneOrigin { pos: fmv_core::Pos::new(1120.0, 1468.0, 1298.0),
            facing: fmv_ff7::field_direction_to_facing(64), triangle: Some(62) };
        assert!(host.requires_origin(TEAM_TALK_SCENE_ID));
        assert!(host.initial_continuation(TEAM_TALK_SCENE_ID, &story.encode(), 7, 617, None).is_none(),
            "production never substitutes the offline survey seed for an accepted trigger position");
        let (team, replay, windows) = finish(&host, TEAM_TALK_SCENE_ID, &story.encode(), Some(origin));
        assert_eq!(windows, 12);
        let mut moved = replay;
        let changed_facing = fmv_ff7::field_direction_to_facing(128);
        assert_ne!(origin.facing, changed_facing, "the negative probe must change the accepted heading");
        moved.origin.as_mut().unwrap().facing = changed_facing;
        assert!(host.evaluate_continuation(TEAM_TALK_SCENE_ID, &story.encode(), 7, 617, Some(&moved)).is_none(),
            "changing the accepted pose invalidates the bound source identity");
        story = StoryState::decode(&team.progress).unwrap();
        assert_eq!(story.moment, 7);
        let landing = team.travel.unwrap();
        assert_eq!((landing.target.as_str(), landing.arrival.x, landing.arrival.y, landing.arrival_hint),
            ("md1_2", 3549.0, 30574.0, Some(60)));
        assert_eq!(landing.arrival_facing, Some(fmv_ff7::field_direction_to_facing(128)));

        // Continue with the team's exact afterimage. The real reactor-door script then
        // establishes the party mirrors for the larger party that the escape scripts read.
        story.moment = 10;
        let door = host.field("nmkin_1").unwrap().story_advance(&story.encode(), 0).expect("authored reactor-door party setup");
        story = StoryState::decode(&door.progress).unwrap();
        story.moment = 27;
        story.set_bit(1, 225, 5, true);
        assert!(!host.requires_origin(REACTOR_FINALE_SCENE_ID), "the finale places Cloud with authored XYZI before moving him");
        let (finale, _, windows) = finish(&host, REACTOR_FINALE_SCENE_ID, &story.encode(), None);
        assert_eq!(windows, 0);
        assert_eq!(StoryState::decode(&finale.progress).unwrap().moment, 33);
        let landing = finale.travel.unwrap();
        assert_eq!((landing.target.as_str(), landing.arrival.x, landing.arrival.y, landing.arrival_hint),
            ("md8_4", -21.0, 156.0, Some(2)));
        assert_eq!(landing.arrival_facing, Some(fmv_ff7::field_direction_to_facing(0)));

        let (tunnel, _, tunnel_windows) = finish(&host, POST_ESCAPE_SCENE_ID, &finale.progress, None);
        assert!(tunnel_windows >= 3);
        assert_eq!(tunnel.progress, finale.progress, "the tunnel has no persistent writes");
        let landing = tunnel.travel.unwrap();
        assert_eq!((landing.target.as_str(), landing.arrival.x, landing.arrival.y, landing.arrival_hint),
            ("md8_1", 1629.0, -5357.0, Some(15)));
        assert_eq!(landing.arrival_facing, Some(fmv_ff7::field_direction_to_facing(192)));

        let (station, replay, station_windows) = finish(&host, STATION_SCENE_ID, &tunnel.progress, None);
        assert!(station_windows >= 4 && station.travel.is_none());
        assert_eq!(StoryState::decode(&station.progress).unwrap().moment, 36);
        assert_eq!(fmv_ff7::story::canon_party_of(&station.progress), vec![0]);
        let mut forged = replay;
        forged.events.push(SceneReplayEvent::Battle { ordinal: 1, identity: vec![1], formation: 300,
            progress: tunnel.progress.clone(), gil: 617 });
        assert!(host.evaluate_continuation(STATION_SCENE_ID, &tunnel.progress, 7, 617, Some(&forged)).is_none(),
            "a full-restore exception never admits a combat history");
        for scene in [POST_ESCAPE_SCENE_ID, STATION_SCENE_ID] {
            assert!(host.continuation_for(scene, &tunnel.progress));
            assert!(!host.continuation_for(scene, &StoryState::at(33).encode()), "legacy route retains its existing path");
        }
        assert!(!host.requires_landing_for(POST_ESCAPE_SCENE_ID, &tunnel.progress), "the escape chain can start in its first field");
        assert!(host.requires_landing_for(STATION_SCENE_ID, &tunnel.progress), "the street continuation needs the tunnel's saved landing");
        assert!(!host.requires_landing_for(STATION_SCENE_ID, &StoryState::at(33).encode()), "legacy standalone arrival is unchanged");
    }
}
