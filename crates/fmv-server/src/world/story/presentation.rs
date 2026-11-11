//! A point whose original script reads the player needs a real accepted crossing pose.
//! Only those explicitly opted-in presentations constrain movement; ordinary points keep
//! their chord sweep and legacy battle behavior.

use fmv_adapter_api::{StoryStep, StoryTrigger};
use fmv_core::{EntityId, Pos};
use fmv_protocol::SceneOrigin;

use super::super::{dist_xy, walk_chord, Chord, World};

impl World {
    pub(in crate::world) fn point_requires_presentation_origin(&self, entity: EntityId, step: &StoryStep) -> bool {
        let Some(player) = self.players.get(&entity) else { return false; };
        let Some(host) = self.scenes.host() else { return false; };
        host.point_scene(&player.area, &player.progression, &step.progress)
            .is_some_and(|scene| host.requires_presentation_origin(&scene))
    }

    /// Keep a valid endpoint, or stop an overshooting accepted chord inside the source
    /// region it actually traversed. The original interpreter must accept the rounded
    /// pose too: touching an edge is not permission to seed a different triangle.
    /// Refusal leaves the original position/budget intact and cannot start the battle.
    pub(in crate::world) fn presentation_move(&self, entity: EntityId, from: Pos, to: Pos, facing: f32) -> Result<Pos, ()> {
        let Some(player) = self.players.get(&entity) else { return Ok(to); };
        if player.preview.is_some() || player.placement.as_ref().is_some_and(|p| !p.story_advance_allowed)
            || self.account_in_battle(player.account) { return Ok(to); }
        let Some(host) = self.scenes.host() else { return Ok(to); };
        let Some(area) = self.areas.get(&player.area) else { return Ok(to); };
        if !self.scenes.requires_presentation_origin_in(&player.area) {
            return Ok(to);
        }
        let mut crossing: Option<(&fmv_adapter_api::StoryPoint, String, [f64; 2])> = None;
        for point in area.story_points() {
            if !point.is_hit_by(from, to) || (area.runtime().story_battle_for(&player.progression, point.id).is_some()
                && point.is_hit_by(from, from)) { continue; }
            let Some(step) = area.runtime().story_advance(&player.progression, point.id) else { continue; };
            let Some(scene) = host.point_scene(&player.area, &player.progression, &step.progress)
                .filter(|scene| host.requires_presentation_origin(scene)) else { continue; };
            if !self.scenes.contains(&scene, &player.area) { return Err(()); }
            let StoryTrigger::Region(corners) = &point.trigger else { return Err(()); };
            let span = region_span(corners, from, to).ok_or(())?;
            if crossing.as_ref().is_none_or(|(_, _, old)| span[0] < old[0]) {
                crossing = Some((point, scene, span));
            }
        }
        let Some((point, scene, [enter, leave])) = crossing else { return Ok(to); };
        // Existing authored line continuations retain their priority over ordinary points.
        if host.line_scene(&player.area, &player.progression, from, to)
            .is_some_and(|scene| host.continuation_for(&scene, &player.progression) && host.offered(&scene, &player.progression)) {
            return Ok(to);
        }
        let valid = |pos| point.is_hit_by(pos, pos) && host.presentation_origin_valid(&scene, &player.area,
            &player.progression, &SceneOrigin { pos, facing, triangle: None });
        if valid(to) { return Ok(to); }

        // Start at the actual intersection. At an integer-script boundary it may round
        // into a neighbouring triangle; then try one unit along this same traversed
        // interval, and its midpoint. None is a map centroid or an off-chord placement.
        // A grazing segment with no representable source pose fails closed.
        let length = dist_xy(from, to);
        let inset = if length > 0.0 { (enter + 1.0 / length).min((enter + leave) * 0.5) } else { enter };
        for t in [enter, inset, (enter + leave) * 0.5] {
            let at = Pos::new(
                (f64::from(from.x) + (f64::from(to.x) - f64::from(from.x)) * t) as f32,
                (f64::from(from.y) + (f64::from(to.y) - f64::from(from.y)) * t) as f32,
                from.z,
            );
            let Chord::Clear(accepted) = walk_chord(area.runtime(), from, at, dist_xy(from, at), area.rules()) else { continue; };
            // Validation may supply the floor height, but may not relocate x/y off the
            // accepted segment in order to manufacture a usable presentation origin.
            if accepted.x == at.x && accepted.y == at.y && valid(accepted) { return Ok(accepted); }
        }
        Err(())
    }
}

/// The portion of a finite segment inside a convex source region, in segment parameters.
/// Clipping half-planes includes the boundary and works for either polygon winding.
fn region_span(corners: &[Pos], from: Pos, to: Pos) -> Option<[f64; 2]> {
    if corners.len() < 3 || !from.is_finite() || !to.is_finite() || corners.iter().any(|p| !p.is_finite()) { return None; }
    let edges = || corners.iter().zip(corners.iter().cycle().skip(1)).take(corners.len());
    let area: f64 = edges().map(|(a, b)| f64::from(a.x) * f64::from(b.y) - f64::from(b.x) * f64::from(a.y)).sum();
    if area == 0.0 || !area.is_finite() { return None; }
    let sign = area.signum();
    let (mut enter, mut leave) = (0.0f64, 1.0f64);
    for (a, b) in edges() {
        let side = |p: Pos| sign * ((f64::from(b.x) - f64::from(a.x)) * (f64::from(p.y) - f64::from(a.y))
            - (f64::from(b.y) - f64::from(a.y)) * (f64::from(p.x) - f64::from(a.x)));
        let (start, end) = (side(from), side(to));
        if start < 0.0 && end < 0.0 { return None; }
        if start < 0.0 { enter = enter.max(start / (start - end)); }
        if end < 0.0 { leave = leave.min(start / (start - end)); }
        if enter > leave { return None; }
    }
    Some([enter, leave])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_is_the_actual_segment_interval_for_both_windings_and_grazing_edges() {
        let mut triangle = vec![Pos::new(0.0, 0.0, 0.0), Pos::new(10.0, 0.0, 0.0), Pos::new(0.0, 10.0, 0.0)];
        for _ in 0..2 {
            assert_eq!(region_span(&triangle, Pos::new(-5.0, 2.0, 0.0), Pos::new(15.0, 2.0, 0.0)), Some([0.25, 0.65]));
            assert_eq!(region_span(&triangle, Pos::new(-5.0, 10.0, 0.0), Pos::new(5.0, 10.0, 0.0)), Some([0.5, 0.5]));
            assert_eq!(region_span(&triangle, Pos::new(-5.0, 11.0, 0.0), Pos::new(5.0, 11.0, 0.0)), None);
            triangle.reverse();
        }
        assert_eq!(region_span(&[Pos::default(); 3], Pos::default(), Pos::new(1.0, 0.0, 0.0)), None);
    }

    #[test]
    #[ignore = "needs owner's FMV_FF7_DATA; original station geometry and script, no save writes"]
    fn owned_station_overshoot_stops_on_its_actual_chord_before_the_guard_presentation() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use fmv_adapter_api::GameAdapter;
        use fmv_core::{AccountId, AnimState, AvatarAppearance};
        use fmv_ff7::story::StoryState;
        use fmv_protocol::ServerMsg;
        use tokio::sync::{mpsc, watch};
        use crate::ff7::{open_ff7, Ff7Labels, Ff7Source};
        use crate::scene_host::Ff7SceneHost;
        use crate::world::{MoveOutcome, NewPlayer, SceneHost, SceneSettings};

        let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned field data"));
        let opened = open_ff7(&Ff7Source::Flevel { path: data.join("field/flevel.lgp"), labels: Ff7Labels::default() }).unwrap();
        let host = Arc::new(Ff7SceneHost::with_extended_save(Arc::clone(&opened.adapter), true));
        let mut world = World::new(opened.adapter.load_area("md1stin").unwrap()).unwrap();
        world.configure_scenes(Some(SceneSettings { host: host.clone(), debug_preview: false, debug_new_story: None }));
        let before = StoryState::fresh_first_control().encode();
        let area = world.areas.get("md1stin").unwrap();
        let point = area.story_points().iter().find(|p| area.runtime().story_battle_for(&before, p.id) == Some(300)).unwrap();
        let StoryTrigger::Region(corners) = &point.trigger else { panic!("source triangle region"); };
        // Derive a walkable outside-to-outside segment from the original source polygon.
        // Its midpoint only chooses test directions; production clipping never uses it.
        let middle = point.center();
        let mut chosen = None;
        'directions: for (dx, dy) in [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)] {
            for offset in [-20.25, -3.49, 0.0, 3.51, 20.25] {
                let a = Pos::new(middle.x - dx * 1000.0 - dy * offset, middle.y - dy * 1000.0 + dx * offset, middle.z);
                let b = Pos::new(middle.x + dx * 1000.0 - dy * offset, middle.y + dy * 1000.0 + dx * offset, middle.z);
                let Some([enter, leave]) = region_span(corners, a, b) else { continue; };
                let epsilon = 1.0 / dist_xy(a, b);
                let at = |t: f64| Pos::new((f64::from(a.x) + f64::from(b.x - a.x) * t) as f32,
                    (f64::from(a.y) + f64::from(b.y - a.y) * t) as f32, middle.z);
                let from = at(enter - epsilon);
                let to = at(leave + epsilon);
                let placed = area.runtime().place_arrival(from, None);
                if placed.x != from.x || placed.y != from.y || point.is_hit_by(placed, placed) || point.is_hit_by(to, to) { continue; }
                if let Chord::Clear(accepted) = walk_chord(area.runtime(), placed, to, dist_xy(placed, to), area.rules()) {
                    chosen = Some((placed, accepted));
                    break 'directions;
                }
            }
        }
        let (from, to) = chosen.expect("an original walkable chord passes through triangle12 and ends beyond it");
        let cap = area.rules().cap;
        let now = Instant::now();
        let (session, entity) = world.allocate_ids();
        let (tx, mut rx) = mpsc::channel(256);
        world.join(NewPlayer { session, entity, account: AccountId(entity.0),
            avatar: AvatarAppearance::new("Crossing", [1, 2, 3], None), progression: before.clone(),
            progress: Default::default(), scene_state: None, scene_group: vec![], tx, close: Arc::new(watch::channel(false).0) }, now).unwrap();
        while rx.try_recv().is_ok() {}
        let player = world.players.get_mut(&entity).unwrap();
        player.snapshot.pos = from;
        player.budget.tokens = cap;
        assert_eq!(world.apply_move(entity, to, 0.75, AnimState::Run, now + Duration::from_secs(1)), MoveOutcome::Applied);
        let messages: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let start_index = messages.iter().position(|m| matches!(m.as_ref(), ServerMsg::SceneStart { .. })).expect("the prelude cannot be skipped");
        let ServerMsg::SceneStart { presentation_origin: Some(origin), scene, .. } = messages[start_index].as_ref() else { panic!("bound ordinary start"); };
        assert_eq!(scene, "ff7/md1stin/station-guards");
        assert_ne!(origin.pos, to);
        assert!(host.presentation_origin_valid(scene, "md1stin", &before, origin), "the original GETAI must see triangle12 after rounding");
        let cross = f64::from(to.x - from.x) * f64::from(origin.pos.y - from.y)
            - f64::from(to.y - from.y) * f64::from(origin.pos.x - from.x);
        assert!(cross.abs() < 1.0, "the clipped origin stays on the accepted chord: {cross}");
        assert!(messages[..start_index].iter().any(|m| matches!(m.as_ref(), ServerMsg::EntityUpdate { entities, .. }
            if entities.iter().any(|p| p.id == entity && p.pos == origin.pos && p.anim == AnimState::Idle))));
        assert_eq!(world.players.get(&entity).unwrap().progression, before, "presentation changes no save state");
        assert!(!world.in_battle(entity), "battle300 waits for the authored prelude");
    }
}
