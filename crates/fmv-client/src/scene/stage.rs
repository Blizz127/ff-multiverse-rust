//! The stage of a scene: where each member of the cast stands, which way it faces and whether it is seen, moment by
//! moment, and what the scene told the camera.
//!
//! Pure data and arithmetic: no GPU, no game data. The scene player ([`super::ScenePlayer`]) feeds it the stage events
//! of the interpreter (`fmv_ff7::script::scene::Stage`) when their time has come and moves it on with the scene's clock;
//! the drawing reads the actors it holds.
//!
//! * `Place` puts an actor somewhere at once, and cancels a walk it was on.
//! * `Move` walks it there over the number of frames the event says (a straight line from where it stood, heading
//!   toward the target the whole way). A second move of the same actor first finishes the first.
//! * `Face` turns it (the short way round), at once or over the frames it says.
//! * `Show` hides or shows it.
//! * `Anim` is **not played**: the cast are the playable models, which have a standing, a walking and a running clip and
//!   none of the scripts' clips. The event is counted ([`Counts::anims_skipped`]), never guessed at.
//! * `Sound` is routed separately by the scene player to its cancellable audio channel.
//! * `Camera` is **recorded** ([`CameraState`]) and not shown: the field's own camera stays where it is.
//!
//! Time is in field frames (30 a second), the interpreter's clock. A walk or a turn only moves while the scene runs: a
//! window pauses the whole stage, as it pauses the interpreter.

use fmv_ff7::script::scene::{CameraOp, MoveHow, Stage};
use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};

/// What the scene told the camera last (the camera itself is the field's and never moves here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraState {
    /// Nothing asked of it, or given back (`SCRCC`).
    #[default]
    Default,
    /// A scroll to a place in the field's own screen units, over this many frames.
    Scroll { x: i16, y: i16, frames: u32 },
    /// Follow the cast member playing this script entity.
    Follow { entity: u8 },
}

/// What the stage could not show, and said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// `Anim` events of cast members: the clips are not played.
    pub anims_skipped: u32,
    /// Camera events recorded and not shown.
    pub camera_ops: u32,
    /// Events about an entity that is not in the cast.
    pub off_cast: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Walk {
    from: [f32; 3],
    to: [f32; 3],
    frames: f32,
    elapsed: f32,
    how: MoveHow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Turn {
    from: f32,
    to: f32,
    frames: f32,
    elapsed: f32,
}

/// How far through a motion of `frames` frames it is after `elapsed` of them (0 to 1; no frames is done).
fn progress(elapsed: f32, frames: f32) -> f32 {
    if frames.partial_cmp(&0.0).is_none_or(|o| o.is_le()) || elapsed.is_nan() {
        1.0
    } else {
        (elapsed / frames).clamp(0.0, 1.0)
    }
}

/// An angle in (-pi, pi].
fn wrap(angle: f32) -> f32 {
    if !angle.is_finite() {
        return 0.0;
    }
    let a = angle.rem_euclid(TAU);
    if a > PI {
        a - TAU
    } else {
        a
    }
}

/// `from` turned toward `to` by `t` of the short way round.
fn turn_toward(from: f32, to: f32, t: f32) -> f32 {
    let delta = wrap(to - from);
    wrap(from + delta * t)
}

/// One member of the cast as the stage has it.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    entity: u8,
    /// `x`, `y`, `z` in walkmesh units; `None` until something placed it.
    pos: Option<[f32; 3]>,
    triangle: Option<u16>,
    heading: f32,
    visible: bool,
    walk: Option<Walk>,
    turn: Option<Turn>,
}

impl Actor {
    fn new(entity: u8) -> Self {
        Self { entity, pos: None, triangle: None, heading: 0.0, visible: true, walk: None, turn: None }
    }

    /// The script entity this actor plays.
    pub fn entity(&self) -> u8 {
        self.entity
    }

    /// Where it stands now (`x`, `y`, `z`), `None` while nothing has placed it.
    pub fn pos(&self) -> Option<[f32; 3]> {
        self.pos
    }

    /// The walkmesh triangle it was last placed on, if the scene said.
    #[cfg(test)]
    pub fn triangle(&self) -> Option<u16> {
        self.triangle
    }

    /// Which way it faces, radians counter-clockwise from the field's +x axis.
    pub fn heading(&self) -> f32 {
        self.heading
    }

    /// Whether it is walking now (what the avatar's clip shows: a walk while this holds, standing otherwise).
    pub fn moving(&self) -> bool {
        self.walk.is_some_and(|w| w.how == MoveHow::Walk)
    }

    /// Whether it is seen at all: it has a place and is not hidden.
    pub fn shown(&self) -> bool {
        self.visible && self.pos.is_some()
    }

    /// Whether anything is in motion.
    fn busy(&self) -> bool {
        self.walk.is_some() || self.turn.is_some()
    }

    fn advance(&mut self, frames: f32) {
        if let Some(walk) = &mut self.walk {
            walk.elapsed += frames;
            let t = progress(walk.elapsed, walk.frames);
            let at = [0, 1, 2].map(|i| walk.from[i] + (walk.to[i] - walk.from[i]) * t);
            self.pos = Some(at);
            if t >= 1.0 {
                self.walk = None;
            }
        }
        if let Some(turn) = &mut self.turn {
            turn.elapsed += frames;
            let t = progress(turn.elapsed, turn.frames);
            self.heading = turn_toward(turn.from, turn.to, t);
            if t >= 1.0 {
                self.turn = None;
            }
        }
    }

    /// Finish whatever it is doing: it stands where its walk was going, facing where its turn was going.
    fn settle(&mut self) {
        self.advance(f32::INFINITY);
    }
}

/// The cast of one scene and the camera.
#[derive(Debug, Clone, Default)]
pub struct Cast {
    actors: BTreeMap<u8, Actor>,
    camera: CameraState,
    counts: Counts,
}

/// The ground's height at a place of the field (`x`, `y`, and the triangle the scene says holds it, if it does).
pub type Height<'a> = &'a dyn Fn(i16, i16, Option<u16>) -> Option<f32>;

impl Cast {
    /// A stage for the cast playing these script entities (every one starts unplaced).
    pub fn new(entities: impl IntoIterator<Item = u8>) -> Self {
        Self { actors: entities.into_iter().map(|e| (e, Actor::new(e))).collect(), camera: CameraState::Default, counts: Counts::default() }
    }

    #[cfg(test)]
    pub fn actor(&self, entity: u8) -> Option<&Actor> {
        self.actors.get(&entity)
    }

    /// Every actor, by script entity.
    pub fn actors(&self) -> impl Iterator<Item = &Actor> {
        self.actors.values()
    }

    #[cfg(test)]
    pub fn camera(&self) -> CameraState {
        self.camera
    }

    #[cfg(test)]
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Whether any actor is walking or turning.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.actors.values().any(Actor::busy)
    }

    /// Everybody vanishes and the camera is the field's again (the end of the scene).
    pub fn clear(&mut self) {
        self.actors.clear();
        self.camera = CameraState::Default;
    }

    /// Move every walk and turn on by `frames` of scene time (no time, or time that is not a number, moves nothing).
    pub fn advance(&mut self, frames: f32) {
        if frames.partial_cmp(&0.0).is_none_or(|o| o.is_le()) {
            return;
        }
        for actor in self.actors.values_mut() {
            actor.advance(frames);
        }
    }

    /// Finish every walk and turn at once (a bot that does not wait for the picture).
    pub fn settle(&mut self) {
        for actor in self.actors.values_mut().filter(|a| a.busy()) {
            actor.settle();
        }
    }

    /// Show one event of the interpreter. `overshoot` is how many frames ago it should have happened (the clock ran past
    /// its time since the last call): the motion it starts is that far along already. `height` is the ground's height, for
    /// the events that do not carry one.
    pub fn apply(&mut self, event: &Stage, overshoot: f32, height: Height<'_>) {
        let overshoot = if overshoot.is_nan() { 0.0 } else { overshoot.max(0.0) };
        match event {
            Stage::Sound { .. } => {},
            Stage::Camera(op) => {
                self.counts.camera_ops = self.counts.camera_ops.saturating_add(1);
                self.camera = match *op {
                    CameraOp::Scroll { x, y, frames } => CameraState::Scroll { x, y, frames },
                    CameraOp::Follow { entity } => CameraState::Follow { entity },
                    CameraOp::Reset => CameraState::Default,
                };
            }
            Stage::Place { entity, x, y, z, triangle } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.walk = None;
                let z = z.map(f32::from).or_else(|| height(*x, *y, *triangle)).or_else(|| actor.pos.map(|p| p[2])).unwrap_or(0.0);
                actor.pos = Some([f32::from(*x), f32::from(*y), z]);
                actor.triangle = *triangle;
            }
            Stage::Move { entity, from, x, y, z, frames, how } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                // A second move of the same actor starts where the first one was going.
                if let Some(walk) = actor.walk.take() {
                    actor.pos = Some(walk.to);
                }
                let target_xy = (f32::from(*x), f32::from(*y));
                let start = match (from, actor.pos) {
                    (Some((fx, fy)), here) => {
                        let z = here.map(|p| p[2]).or_else(|| height(*fx, *fy, None)).unwrap_or(0.0);
                        [f32::from(*fx), f32::from(*fy), z]
                    }
                    (None, Some(here)) => here,
                    (None, None) => [target_xy.0, target_xy.1, 0.0],
                };
                let z = z.map(f32::from).or_else(|| height(*x, *y, None)).unwrap_or(start[2]);
                let to = [target_xy.0, target_xy.1, z];
                let (dx, dy) = (to[0] - start[0], to[1] - start[1]);
                if *how == MoveHow::Walk && dx.hypot(dy) > 0.5 {
                    actor.heading = dy.atan2(dx);
                    actor.turn = None;
                }
                actor.pos = Some(start);
                actor.walk = Some(Walk { from: start, to, frames: *frames as f32, elapsed: 0.0, how: *how });
                // (a walk of no frames is a jump to the place: `advance` finishes it at once)
                actor.advance(overshoot);
            }
            Stage::Face { entity, heading, frames } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                let heading = wrap(*heading);
                if *frames == 0 {
                    actor.heading = heading;
                    actor.turn = None;
                } else {
                    actor.turn = Some(Turn { from: actor.heading, to: heading, frames: *frames as f32, elapsed: 0.0 });
                    actor.advance(overshoot);
                }
            }
            Stage::Show { entity, visible } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.visible = *visible;
            }
            Stage::Anim { entity, .. } => {
                if self.actors.contains_key(entity) {
                    self.counts.anims_skipped = self.counts.anims_skipped.saturating_add(1);
                } else {
                    self.off_cast();
                }
            }
        }
    }

    fn off_cast(&mut self) {
        self.counts.off_cast = self.counts.off_cast.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::scene::AnimHow;

    fn flat(_x: i16, _y: i16, _t: Option<u16>) -> Option<f32> {
        Some(7.0)
    }

    fn place(entity: u8, x: i16, y: i16) -> Stage {
        Stage::Place { entity, x, y, z: None, triangle: Some(3) }
    }

    fn walk(entity: u8, x: i16, y: i16, frames: u32) -> Stage {
        Stage::Move { entity, from: None, x, y, z: None, frames, how: MoveHow::Walk }
    }

    #[test]
    fn a_place_stands_the_actor_there_at_the_grounds_height_unless_the_script_gives_one() {
        let mut cast = Cast::new([1, 2]);
        assert!(!cast.actor(1).unwrap().shown(), "nothing is seen before it is placed");
        cast.apply(&place(1, 10, -20), 0.0, &flat);
        let a = cast.actor(1).unwrap();
        assert_eq!((a.pos(), a.triangle(), a.shown()), (Some([10.0, -20.0, 7.0]), Some(3), true));
        cast.apply(&Stage::Place { entity: 2, x: 1, y: 2, z: Some(-9), triangle: None }, 0.0, &flat);
        assert_eq!(cast.actor(2).unwrap().pos(), Some([1.0, 2.0, -9.0]));
        // Off the mesh and no height of its own: it keeps the height it had.
        cast.apply(&Stage::Place { entity: 2, x: 5, y: 5, z: None, triangle: None }, 0.0, &|_, _, _| None);
        assert_eq!(cast.actor(2).unwrap().pos(), Some([5.0, 5.0, -9.0]));
    }

    #[test]
    fn a_walk_is_a_straight_line_over_its_frames_heading_toward_the_target() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 50), 0.0, &flat);
        let a = cast.actor(1).unwrap();
        assert!(a.moving() && cast.busy());
        assert_eq!(a.heading(), 0.0, "+x");
        cast.advance(25.0);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some([50.0, 0.0, 7.0]), "half way after half the frames");
        cast.advance(1000.0);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some([100.0, 0.0, 7.0]));
        assert!(!a.moving() && !cast.busy(), "it arrived and stands");
        // Walking up the +y axis faces it that way.
        cast.apply(&walk(1, 100, 80, 10), 0.0, &flat);
        assert!((cast.actor(1).unwrap().heading() - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn a_move_starts_from_the_events_own_from_when_it_has_one_and_a_second_move_finishes_the_first() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 40, 0, 100), 0.0, &flat);
        cast.advance(10.0);
        // The script's next walk was planned from where the first one ends.
        cast.apply(&Stage::Move { entity: 1, from: Some((40, 0)), x: 40, y: 60, z: None, frames: 20, how: MoveHow::Walk }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([40.0, 0.0, 7.0]));
        cast.advance(10.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([40.0, 30.0, 7.0]));
    }

    #[test]
    fn an_event_that_is_late_starts_its_motion_that_far_along() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 100), 30.0, &flat);
        let [x, y, z] = cast.actor(1).unwrap().pos().unwrap();
        assert!((x - 30.0).abs() < 1e-4 && y == 0.0 && z == 7.0, "{x} {y} {z}");
        // A negative or nonsense lateness is none.
        for late in [f32::NAN, -5.0] {
            let mut cast = Cast::new([1]);
            cast.apply(&place(1, 0, 0), 0.0, &flat);
            cast.apply(&walk(1, 100, 0, 100), late, &flat);
            assert_eq!(cast.actor(1).unwrap().pos(), Some([0.0, 0.0, 7.0]), "{late}");
        }
    }

    #[test]
    fn a_move_of_no_frames_is_a_jump_and_a_place_cancels_a_walk() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 12, 34, 0), 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([12.0, 34.0, 7.0]));
        assert!(!cast.actor(1).unwrap().moving());
        cast.apply(&walk(1, 100, 0, 100), 0.0, &flat);
        cast.apply(&place(1, -5, -5), 0.0, &flat);
        cast.advance(50.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([-5.0, -5.0, 7.0]), "the walk was cancelled");
        // A jump and a ladder are not walks: the model does not walk for them.
        cast.apply(&Stage::Move { entity: 1, from: None, x: 0, y: 0, z: Some(50), frames: 30, how: MoveHow::Ladder }, 0.0, &flat);
        assert!(!cast.actor(1).unwrap().moving());
    }

    #[test]
    fn a_turn_goes_the_short_way_round_at_once_or_over_its_frames() {
        let mut cast = Cast::new([1]);
        cast.apply(&Stage::Face { entity: 1, heading: 3.0, frames: 0 }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().heading(), 3.0);
        // From 3.0 to -3.0 is a short turn across pi (0.28 radians), not 6 the other way.
        cast.apply(&Stage::Face { entity: 1, heading: -3.0, frames: 10 }, 0.0, &flat);
        cast.advance(5.0);
        let h = cast.actor(1).unwrap().heading();
        assert!(h.abs() > 3.0, "half way is at the far side of pi: {h}");
        cast.advance(5.0);
        assert!((cast.actor(1).unwrap().heading() + 3.0).abs() < 1e-5);
        assert!(!cast.busy());
        // A heading that is not finite is the field's +x.
        cast.apply(&Stage::Face { entity: 1, heading: f32::NAN, frames: 0 }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().heading(), 0.0);
    }

    #[test]
    fn show_hides_and_shows_and_the_camera_is_recorded_not_shown() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&Stage::Show { entity: 1, visible: false }, 0.0, &flat);
        assert!(!cast.actor(1).unwrap().shown());
        cast.apply(&Stage::Show { entity: 1, visible: true }, 0.0, &flat);
        assert!(cast.actor(1).unwrap().shown());
        assert_eq!(cast.camera(), CameraState::Default);
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 4, y: -5, frames: 12 }), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Scroll { x: 4, y: -5, frames: 12 });
        cast.apply(&Stage::Camera(CameraOp::Follow { entity: 20 }), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Follow { entity: 20 });
        cast.apply(&Stage::Camera(CameraOp::Reset), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Default);
        assert_eq!(cast.counts().camera_ops, 3);
    }

    #[test]
    fn the_clips_are_counted_not_played_and_strangers_are_counted_too() {
        let mut cast = Cast::new([1]);
        let anim = |entity| Stage::Anim { entity, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16 };
        cast.apply(&anim(1), 0.0, &flat);
        cast.apply(&anim(1), 0.0, &flat);
        cast.apply(&anim(9), 0.0, &flat);
        cast.apply(&place(9, 0, 0), 0.0, &flat);
        cast.apply(&walk(9, 1, 1, 5), 0.0, &flat);
        cast.apply(&Stage::Face { entity: 9, heading: 1.0, frames: 0 }, 0.0, &flat);
        cast.apply(&Stage::Show { entity: 9, visible: false }, 0.0, &flat);
        assert_eq!(cast.counts(), Counts { anims_skipped: 2, camera_ops: 0, off_cast: 5 });
        assert!(cast.actor(9).is_none(), "an entity that is not in the cast never gets an actor");
    }

    #[test]
    fn settling_finishes_every_motion_and_clearing_takes_the_cast_away() {
        let mut cast = Cast::new([1, 2]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 90, 90, 600), 0.0, &flat);
        cast.apply(&Stage::Face { entity: 2, heading: 1.0, frames: 300 }, 0.0, &flat);
        cast.settle();
        assert_eq!(cast.actor(1).unwrap().pos(), Some([90.0, 90.0, 7.0]));
        assert!((cast.actor(2).unwrap().heading() - 1.0).abs() < 1e-6);
        assert!(!cast.busy());
        cast.apply(&Stage::Camera(CameraOp::Follow { entity: 1 }), 0.0, &flat);
        cast.clear();
        assert_eq!((cast.actors().count(), cast.camera()), (0, CameraState::Default));
    }

    #[test]
    fn nonsense_numbers_never_leave_the_stage_without_a_finite_place() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 100, 10), 0.0, &flat);
        for frames in [f32::NAN, f32::INFINITY, -4.0, 0.0, 1e30] {
            cast.advance(frames);
            let p = cast.actor(1).unwrap().pos().unwrap();
            assert!(p.iter().all(|c| c.is_finite()), "{frames}: {p:?}");
        }
    }
}
