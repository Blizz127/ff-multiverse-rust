//! Where the player's own character stands while a live field's scripts hold the scene.
//!
//! The owner's direction (2026-11-12): the player steers their own character and the story's lead (FF7: Cloud) is the
//! computer's. On a live field the scripts know one "player character", and it is the lead's:
//!
//! * **Free**: the player walks; the server tells the scripts the lead stands where the player does; the lead is drawn
//!   as a follower, walking behind (see `crate::followers`).
//! * **A scene** (the scripts hold the player): the lead is drawn where the scripts put it, playing what they make it
//!   play (its jump off the train, its turns and gestures). The player's character must not stand inside it: it walks
//!   to a free place beside the lead, a little behind for choice, and stands there facing the way the lead faces. When
//!   the lead walks on, it follows.
//! * **After the scene** nobody jumps: the lead stays where the scene left it until the player has walked away from
//!   it, and the player's character is drawn where it stood, the difference to its real place (the lead's) shrinking
//!   over the first strides it walks.
//!
//! Presentation only: the server, the scripts and every rule of the field see the player where the lead is. Nothing
//! here reads game files or the network.

use fmv_core::{AnimState, Pos};

/// How far from the lead the player's character stands in a scene (field units).
const RING: f32 = 64.0;
/// A place stays good while it is this near the lead, and no nearer than this.
const KEEP_FAR: f32 = 140.0;
const KEEP_NEAR: f32 = 38.0;
/// How far a place keeps from everyone else in the scene.
const CLEAR: f32 = 46.0;
/// Walking speed to the place (field units a second).
const SPEED: f32 = 240.0;
/// Further than this from the lead is no walk: the character is put beside it.
const SNAP: f32 = 640.0;
/// The distance the player walks after a scene over which the drawn place comes back to the real one.
const SETTLE: f32 = 96.0;

/// Where and how the player's character is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    pub pos: Pos,
    pub facing: f32,
    pub anim: AnimState,
}

#[derive(Debug, Default)]
pub struct Companion {
    /// The field the state belongs to (the client's count of area changes).
    area: u64,
    /// Where the character was drawn last.
    drawn: Option<Pos>,
    facing: f32,
    /// The place it walks to or stands on in the scene.
    spot: Option<Pos>,
    in_scene: bool,
    /// After a scene: the drawn place minus the real one, the part of it still left (1 to 0), and the real place last
    /// frame.
    settle: Option<(f32, f32, f32, Pos)>,
}

fn dist(a: Pos, b: Pos) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

impl Companion {
    /// One frame of a scene. The lead stands at `lead` facing `lead_facing`; `own` is where the character really is
    /// (the lead's place, by the server's reckoning); `others` are the places of everyone else drawn; `walkable` says
    /// whether a place is ground.
    #[allow(clippy::too_many_arguments)]
    pub fn scene(&mut self, area: u64, own: Pos, lead: Pos, lead_facing: f32, others: &[Pos], walkable: &dyn Fn(Pos) -> bool, dt: f32) -> Drawn {
        if self.area != area {
            *self = Companion { area, ..Companion::default() };
        }
        self.in_scene = true;
        self.settle = None;
        let known = self.drawn.filter(|p| p.is_finite());
        let mut at = known.unwrap_or(own);
        if !lead.is_finite() {
            return Drawn { pos: at, facing: self.facing, anim: AnimState::Idle };
        }
        // Coming on with the lead (it was off the stage, or is far off): the character is put on its place, it does
        // not walk out of the lead or across the field.
        let arrives = known.is_none() || dist(at, lead) > SNAP;
        if arrives {
            at = lead;
            self.spot = None;
        }
        let free = |p: Pos| walkable(p) && others.iter().all(|o| dist(*o, p) >= CLEAR);
        let good = |p: Pos| (KEEP_NEAR..=KEEP_FAR).contains(&dist(p, lead)) && free(p);
        if !self.spot.is_some_and(good) {
            self.spot = Self::choose(at, lead, lead_facing, &free, walkable);
        }
        if let (true, Some(spot)) = (arrives, self.spot) {
            at = spot;
        }
        // Nowhere free beside the lead (a ledge, a crowd): stay put if that is not inside it, else just behind it.
        let spot = self.spot.unwrap_or_else(|| {
            if dist(at, lead) >= KEEP_NEAR {
                at
            } else {
                Pos::new(lead.x - lead_facing.cos() * KEEP_NEAR, lead.y - lead_facing.sin() * KEEP_NEAR, lead.z)
            }
        });
        let left = dist(at, spot);
        let step = SPEED * dt.clamp(0.0, 0.1);
        let (pos, anim) = if left <= step.max(0.5) {
            self.facing = lead_facing;
            (Pos::new(spot.x, spot.y, lead.z), AnimState::Idle)
        } else {
            self.facing = (spot.y - at.y).atan2(spot.x - at.x);
            let t = step / left;
            (Pos::new(at.x + (spot.x - at.x) * t, at.y + (spot.y - at.y) * t, lead.z), AnimState::Walk)
        };
        self.drawn = Some(pos);
        Drawn { pos, facing: self.facing, anim }
    }

    /// The free place nearest `at` on a ring round the lead: behind it to either side for choice, then beside it,
    /// then straight behind, last in front of it; a wider ring when the near one has nothing.
    fn choose(at: Pos, lead: Pos, lead_facing: f32, free: &dyn Fn(Pos) -> bool, walkable: &dyn Fn(Pos) -> bool) -> Option<Pos> {
        use std::f32::consts::PI;
        let mut best: Option<(f32, Pos)> = None;
        for (ring, far) in [(RING, 0.0), (RING * 1.5, 18.0)] {
            for (turn, bias) in [(0.75 * PI, 0.0), (-0.75 * PI, 0.0), (0.5 * PI, 12.0), (-0.5 * PI, 12.0), (PI, 24.0), (0.25 * PI, 70.0), (-0.25 * PI, 70.0)] {
                let a = lead_facing + turn;
                let p = Pos::new(lead.x + a.cos() * ring, lead.y + a.sin() * ring, lead.z);
                let half = Pos::new((lead.x + p.x) / 2.0, (lead.y + p.y) / 2.0, lead.z);
                if !free(p) || !walkable(half) {
                    continue;
                }
                let cost = dist(at, p) + bias + far;
                if best.is_none_or(|(c, _)| cost < c) {
                    best = Some((cost, p));
                }
            }
        }
        best.map(|(_, p)| p)
    }

    /// One frame of a scene whose lead the scripts do not show: the player's character is off the stage with it.
    pub fn offstage(&mut self, area: u64) {
        *self = Companion { area, in_scene: true, ..Companion::default() };
    }

    /// One frame of free walking: the character is drawn at `own`, except for what is left of a scene's place.
    /// Returns the pose and, on the frame a scene ended, `true`.
    pub fn free(&mut self, area: u64, own: Pos, facing: f32, anim: AnimState) -> (Drawn, bool) {
        let ended = self.in_scene && self.area == area;
        if self.area != area {
            *self = Companion { area, ..Companion::default() };
        }
        self.in_scene = false;
        self.spot = None;
        if ended {
            self.settle = self.drawn.filter(|d| d.is_finite() && dist(*d, own) <= KEEP_FAR * 1.5).map(|d| (d.x - own.x, d.y - own.y, 1.0, own));
        }
        let pos = match self.settle {
            Some((dx, dy, left, last)) if own.is_finite() => {
                let left = left - dist(own, last) / SETTLE;
                if left <= 0.0 || dist(own, last) > SNAP {
                    self.settle = None;
                    own
                } else {
                    self.settle = Some((dx, dy, left, own));
                    Pos::new(own.x + dx * left, own.y + dy * left, own.z)
                }
            }
            _ => {
                self.settle = None;
                own
            }
        };
        self.drawn = Some(pos);
        // (standing after the scene it keeps the way it faced in it)
        let facing = if self.settle.is_some() && anim == AnimState::Idle { self.facing } else { facing };
        self.facing = facing;
        (Drawn { pos, facing, anim }, ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(c: &mut Companion, own: Pos, lead: Pos, facing: f32, others: &[Pos], walkable: &dyn Fn(Pos) -> bool, frames: usize) -> Drawn {
        let mut last = c.scene(0, own, lead, facing, others, walkable, 1.0 / 60.0);
        for _ in 1..frames {
            last = c.scene(0, own, lead, facing, others, walkable, 1.0 / 60.0);
        }
        last
    }

    #[test]
    fn in_a_scene_it_walks_out_of_the_lead_to_a_free_place_behind_it_and_faces_its_way() {
        let mut c = Companion::default();
        let lead = Pos::new(100.0, 100.0, 5.0);
        // The player walked here; the scene begins with both on one spot, the lead facing +x.
        c.free(0, lead, 0.0, AnimState::Walk);
        let first = c.scene(0, lead, lead, 0.0, &[], &|_| true, 1.0 / 60.0);
        assert_eq!(first.anim, AnimState::Walk, "it starts to walk at once");
        let stood = run(&mut c, lead, lead, 0.0, &[], &|_| true, 120);
        assert_eq!(stood.anim, AnimState::Idle);
        assert!((dist(stood.pos, lead) - RING).abs() < 1.0, "{stood:?}");
        assert!(stood.pos.x < lead.x, "behind a lead that faces +x: {stood:?}");
        assert_eq!((stood.facing, stood.pos.z), (0.0, 5.0));
        // It stays there: no wandering between equally good places.
        assert_eq!(run(&mut c, lead, lead, 0.0, &[], &|_| true, 30).pos, stood.pos);
    }

    #[test]
    fn it_keeps_off_everyone_else_and_off_what_is_not_ground_and_follows_a_lead_that_walks_on() {
        let lead = Pos::new(0.0, 0.0, 0.0);
        let behind_left = Pos::new(-45.0, 45.0, 0.0);
        let mut c = Companion::default();
        let stood = run(&mut c, lead, lead, 0.0, &[behind_left], &|p| p.y <= 20.0, 200);
        assert!(dist(stood.pos, behind_left) >= CLEAR && stood.pos.y <= 20.0, "{stood:?}");
        assert!(dist(stood.pos, lead) >= KEEP_NEAR);
        // The lead walks far on: the place is no good any more, it follows.
        let far = Pos::new(400.0, 0.0, 0.0);
        let moving = c.scene(0, far, far, 0.0, &[], &|_| true, 1.0 / 60.0);
        assert_eq!(moving.anim, AnimState::Walk);
        let after = run(&mut c, far, far, 0.0, &[], &|_| true, 240);
        assert!(dist(after.pos, far) <= KEEP_FAR && after.anim == AnimState::Idle, "{after:?}");
        // Nowhere to stand at all: never inside the lead.
        let mut c = Companion::default();
        let stuck = run(&mut c, lead, lead, 0.0, &[], &|_| false, 120);
        assert!(dist(stuck.pos, lead) >= KEEP_NEAR - 1.0, "{stuck:?}");
    }

    #[test]
    fn coming_on_with_a_lead_that_was_off_the_stage_it_stands_on_its_place_at_once() {
        let mut c = Companion::default();
        c.free(0, Pos::new(5000.0, 0.0, 0.0), 0.0, AnimState::Idle);
        c.offstage(0);
        let lead = Pos::new(0.0, 0.0, 0.0);
        let on = c.scene(0, lead, lead, 0.0, &[], &|_| true, 1.0 / 60.0);
        assert_eq!(on.anim, AnimState::Idle);
        assert!((dist(on.pos, lead) - RING).abs() < 1.0, "{on:?}");
        // The scene ending right after the lead went off again leaves nothing to walk off.
        c.offstage(0);
        let (free, ended) = c.free(0, lead, 0.3, AnimState::Idle);
        assert_eq!((free.pos, free.facing, ended), (lead, 0.3, true));
    }

    #[test]
    fn after_the_scene_nothing_jumps_and_the_difference_is_walked_off() {
        let mut c = Companion::default();
        let lead = Pos::new(0.0, 0.0, 0.0);
        let stood = run(&mut c, lead, lead, 0.0, &[], &|_| true, 120);
        let (free, ended) = c.free(0, lead, 1.0, AnimState::Idle);
        assert!(ended);
        assert_eq!(free.pos, stood.pos, "the scene ends: it is drawn where it stood");
        assert_eq!(free.facing, 0.0, "and faces as it did");
        // Walking: the real place moves, the drawn one comes to it.
        let mut own = lead;
        let mut last = free;
        for _ in 0..60 {
            own = Pos::new(own.x + 2.0, own.y, 0.0);
            let (d, ended) = c.free(0, own, 0.0, AnimState::Walk);
            assert!(!ended);
            assert!(dist(d.pos, last.pos) < 6.0, "no jump: {d:?} after {last:?}");
            last = d;
        }
        assert_eq!(last.pos, own, "after {SETTLE} units it is where it really is");
        // A new field forgets everything.
        let (d, ended) = c.free(1, Pos::new(9.0, 9.0, 0.0), 0.5, AnimState::Idle);
        assert_eq!((d.pos, d.facing, ended), (Pos::new(9.0, 9.0, 0.0), 0.5, false));
    }
}
