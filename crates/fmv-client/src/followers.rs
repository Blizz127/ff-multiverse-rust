//! The story's own party walking behind its leader on the field.
//!
//! The owner's direction (2026-11-11): each player plays their own new character, and the canon heroes (Cloud, Barret, ...)
//! are computer-run. In battle the server commands them; on the field they follow the party's leader, walking the path the
//! leader walked, so they stay on its walkable ground and climb what it climbed.
//!
//! * **Who follows**: the story's party at the local player's story moment (`fmv_ff7::story::canon_party_of`): Cloud alone
//!   at the start, Cloud and Barret after the reactor door. A party member's client is told only its own story (the server
//!   tells each player its own), so a member whose story differs from its leader's sees its own story's party: a known limit.
//! * **Whom they follow**: the local player when it is solo or the party's leader; else the leader, while it stands in this
//!   instance (nobody, when it does not).
//! * **Where**: on the leader's trail, the first [`SPACING`] behind it and each next one [`SPACING`] further, facing the way
//!   it walks, walking (or running, as the leader does) while it moves and standing when it does not. When the leader enters
//!   a new field, or is put somewhere else by a link, the trail starts again where it stands.
//! * **Not shown**: while a story scene runs (its cast plays the heroes), for a hero the area itself shows (a field character
//!   with the hero's own model: never two Barrets), and when switched off (`FMV_FOLLOWERS=off`; FF7 itself shows only its
//!   leader on the field).
//! * **Presentation only**: they are drawn like the scene cast (the heroes' own field models, the cast's grey ring); nothing
//!   collides with them, talks to them, labels them or saves them, and the server knows nothing of them.

use std::collections::VecDeque;
use std::sync::OnceLock;

use fmv_core::{AnimState, Pos};

use crate::avatar::{AvatarPlayer, Who};
use crate::state::{ClientState, NpcView};

/// Field units between the leader and the first follower, and between one follower and the next.
pub const SPACING: f32 = 88.0;
/// How far the leader walks before the trail gets a new point (field units).
const STEP: f32 = 4.0;
/// A jump this long between two frames is a relocation (a link, a ladder), not a walk: the trail starts again.
const JUMP: f32 = 8.0 * SPACING;
/// Followers' animation keys: their own range, apart from the entities', the NPCs' and the scene cast's.
pub const FOLLOWER_WHO_BASE: u64 = (1 << 62) + (1 << 32);
/// The scene cast's tint (its grey ring): the heroes look the same following as acting in a scene.
const FOLLOWER_TINT: [u8; 3] = [150, 150, 150];

/// The field look of a canon party member (FF7 character id: 0 Cloud, 1 Barret, 2 Tifa, ...).
pub fn hero_appearance(character: u8) -> Option<&'static str> {
    Some(match character {
        0 => "ff7/cloud",
        1 => "ff7/barret",
        2 => "ff7/tifa",
        3 => "ff7/aerith",
        4 => "ff7/red_xiii",
        5 => "ff7/yuffie",
        7 => "ff7/vincent",
        8 => "ff7/cid",
        _ => return None,
    })
}

/// Are followers shown? `FMV_FOLLOWERS=off` (or `0`, `false`, `no`) switches them off for this run.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("FMV_FOLLOWERS").as_deref().map(str::to_ascii_lowercase).as_deref(), Ok("off" | "0" | "false" | "no")))
}

/// One follower's place in a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowerPose {
    pub pos: Pos,
    pub facing: f32,
    pub anim: AnimState,
}

/// The trail of one leader in one field, and where each follower stood last.
#[derive(Debug, Default)]
pub struct Followers {
    /// Newest first: where the leader has walked, a point every [`STEP`].
    trail: VecDeque<Pos>,
    /// The field (the client's count of area changes) and the leader the trail belongs to.
    key: Option<(u64, u64)>,
    last: Vec<FollowerPose>,
    /// Switched off for this state (a picture about something else); `FMV_FOLLOWERS=off` switches off the whole run.
    off: bool,
    /// The first follower stays here until its leader has walked [`SPACING`] away: where a scene left it (a live
    /// field's lead is drawn from the scene's cast while the scene runs, and must not jump when it ends).
    hold: Option<FollowerPose>,
}

fn dist(a: Pos, b: Pos) -> f32 {
    let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn lerp(a: Pos, b: Pos, t: f32) -> Pos {
    Pos::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t)
}

impl Followers {
    /// One frame: the leader (`leader_id`) stands at `leader` in field `area` (any value that changes with the field),
    /// facing `facing`, moving as `anim`; `count` followers are placed on its trail. Returns their poses, the first first.
    pub fn frame(&mut self, area: u64, leader_id: u64, leader: Pos, facing: f32, anim: AnimState, count: usize) -> Vec<FollowerPose> {
        let key = (area, leader_id);
        let jumped = self.trail.front().is_some_and(|&newest| dist(newest, leader) > JUMP);
        if self.key.as_ref() != Some(&key) || jumped || !leader.is_finite() {
            self.trail.clear();
            self.last.clear();
            self.hold = None;
            self.key = Some(key);
        }
        if !leader.is_finite() {
            return Vec::new();
        }
        if self.trail.front().is_none_or(|&newest| dist(newest, leader) >= STEP) {
            self.trail.push_front(leader);
        }
        // Keep what the last follower needs, and a little more.
        let need = SPACING * (count as f32 + 1.0) + STEP;
        let mut walked = 0.0;
        let mut keep = self.trail.len();
        for i in 1..self.trail.len() {
            walked += dist(self.trail[i - 1], self.trail[i]);
            if walked > need {
                keep = i + 1;
                break;
            }
        }
        self.trail.truncate(keep);

        let mut poses = Vec::with_capacity(count);
        for slot in 0..count {
            let target = self.at(leader, SPACING * (slot as f32 + 1.0));
            let before = self.last.get(slot).copied();
            let step = before.map_or(0.0, |b| dist(b.pos, target));
            let facing = match before {
                Some(b) if step > 0.01 => (target.y - b.pos.y).atan2(target.x - b.pos.x),
                Some(b) => b.facing,
                None => facing,
            };
            let moving = step > 0.05;
            let anim = if !moving { AnimState::Idle } else if anim == AnimState::Run { AnimState::Run } else { AnimState::Walk };
            poses.push(FollowerPose { pos: target, facing, anim });
        }
        if let Some(hold) = self.hold {
            if count == 0 || dist(leader, hold.pos) >= SPACING || dist(leader, hold.pos) > JUMP {
                // The leader has walked off: fall in behind it, from where it stood.
                self.hold = None;
                self.trail.clear();
                self.trail.push_front(hold.pos);
                self.trail.push_front(leader);
            } else if let Some(first) = poses.first_mut() {
                *first = hold;
            }
        }
        self.last = poses.clone();
        poses
    }

    /// The first follower stands at `pos` facing `facing` until its leader has walked away from there.
    pub fn hold_first(&mut self, pos: Pos, facing: f32) {
        if pos.is_finite() {
            self.hold = Some(FollowerPose { pos, facing, anim: AnimState::Idle });
        }
    }

    /// The point `behind` field units back along the trail from `leader` (the trail's oldest point when it is shorter).
    fn at(&self, leader: Pos, behind: f32) -> Pos {
        let mut from = leader;
        let mut left = behind;
        for &point in &self.trail {
            let d = dist(from, point);
            if d >= left && d > 0.0 {
                return lerp(from, point, left / d);
            }
            left -= d;
            from = point;
        }
        from
    }

    /// No followers for this state from now on.
    #[cfg(test)]
    pub fn switch_off(&mut self) {
        self.off = true;
        self.clear();
    }

    /// Forget the trail (a field change the caller knows of, a scene): followers appear at the leader again.
    pub fn clear(&mut self) {
        self.trail.clear();
        self.last.clear();
        self.hold = None;
        self.key = None;
    }
}

/// The followers to draw in this frame of the field (see the module docs): an empty list while a scene runs, when switched
/// off, or when the party's leader is not in this instance.
///
/// `staged`: a live field's scripts hold the scene. They own the stage then (a hero they do not show is not there), so
/// nobody is drawn, but the trail goes on: when the scene ends the followers stand behind where it ended.
pub fn players(state: &ClientState, npcs: &[NpcView], scene_running: bool, staged: bool, render_t: f64) -> Vec<AvatarPlayer<'static>> {
    let mut followers = state.followers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !enabled() || followers.off || scene_running {
        followers.clear();
        return Vec::new();
    }
    // The leader: the local player (solo, or the leader), else the party's leader when this client sees it.
    let party = &state.party;
    let leader = if party.party_size() == 0 || party.is_leader() {
        let own = &state.own;
        Some((state.self_entity().0, own.pos(), own.facing(), own.anim()))
    } else {
        party.leader_entity().and_then(|leader| {
            state.remote_views(render_t).into_iter().find(|v| v.id == leader).map(|v| (leader.0, v.pose.pos, v.pose.facing, v.pose.anim))
        })
    };
    let Some((leader_id, pos, facing, anim)) = leader else {
        followers.clear();
        return Vec::new();
    };
    // The story's party, without a hero the area itself shows (its own model on a field character).
    let shown = |appearance: &str| {
        fmv_ff7::playable::resolve_appearance(appearance).is_some_and(|m| npcs.iter().any(|n| n.model.eq_ignore_ascii_case(m.hrc)))
    };
    let heroes: Vec<(u8, &'static str)> = fmv_ff7::story::canon_party_of(state.story.progress())
        .into_iter()
        .filter_map(|c| hero_appearance(c).map(|a| (c, a)))
        .collect();
    let poses = followers.frame(state.area_changes(), leader_id, pos, facing, anim, heroes.len());
    if staged {
        return Vec::new();
    }
    heroes
        .into_iter()
        .zip(poses)
        .filter(|((_, appearance), _)| !shown(appearance))
        .map(|((character, appearance), pose)| AvatarPlayer {
            who: Who(FOLLOWER_WHO_BASE + u64::from(character)),
            pos: pose.pos,
            facing: pose.facing,
            anim: pose.anim,
            scene_frame: None,
            scene_clip: None,
            scene_return: None,
            appearance: Some(appearance),
            npc: None,
            tint: FOLLOWER_TINT,
            own: false,
            party: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(f: &mut Followers, from: (f32, f32), to: (f32, f32), steps: usize, count: usize) -> Vec<FollowerPose> {
        let mut last = Vec::new();
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let p = Pos::new(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, 0.0);
            last = f.frame(0, 1, p, 0.0, AnimState::Walk, count);
        }
        last
    }

    #[test]
    fn followers_string_out_behind_the_leader_on_its_own_path() {
        let mut f = Followers::default();
        // They start where the leader stands, then fall in line as it walks.
        let first = f.frame(0, 1, Pos::new(0.0, 0.0, 0.0), 0.0, AnimState::Idle, 2);
        assert!(first.iter().all(|p| p.pos == Pos::new(0.0, 0.0, 0.0) && p.anim == AnimState::Idle));
        let poses = walk(&mut f, (0.0, 0.0), (400.0, 0.0), 100, 2);
        assert!((poses[0].pos.x - (400.0 - SPACING)).abs() < 1.0, "{poses:?}");
        assert!((poses[1].pos.x - (400.0 - 2.0 * SPACING)).abs() < 1.0, "{poses:?}");
        assert!(poses.iter().all(|p| p.pos.y.abs() < 1e-3 && p.anim == AnimState::Walk && p.facing.abs() < 1e-3));
        // Round a corner: they keep to the path the leader walked, not a straight line to it.
        let poses = walk(&mut f, (400.0, 0.0), (400.0, 30.0), 10, 2);
        assert!(poses[1].pos.y.abs() < 1e-3 && poses[1].pos.x < 400.0, "the second is still on the first leg: {poses:?}");
        // The leader stops: they stop and stand.
        let still = f.frame(0, 1, Pos::new(400.0, 30.0, 0.0), 0.0, AnimState::Idle, 2);
        let again = f.frame(0, 1, Pos::new(400.0, 30.0, 0.0), 0.0, AnimState::Idle, 2);
        assert_eq!(still.iter().map(|p| p.pos).collect::<Vec<_>>(), again.iter().map(|p| p.pos).collect::<Vec<_>>());
        assert!(again.iter().all(|p| p.anim == AnimState::Idle));
    }

    #[test]
    fn a_held_first_follower_stays_until_the_leader_has_walked_away_and_then_falls_in_without_a_jump() {
        let mut f = Followers::default();
        let stood = Pos::new(100.0, 0.0, 0.0);
        f.frame(0, 1, stood, 0.0, AnimState::Idle, 1);
        f.hold_first(stood, 1.5);
        let near = walk(&mut f, (100.0, 0.0), (100.0 + SPACING - 2.0, 0.0), 40, 1);
        assert_eq!((near[0].pos, near[0].facing, near[0].anim), (stood, 1.5, AnimState::Idle));
        let off = f.frame(0, 1, Pos::new(100.0 + SPACING + 1.0, 0.0, 0.0), 0.0, AnimState::Walk, 1);
        assert!(dist(off[0].pos, stood) < 2.0, "it starts from where it stood: {off:?}");
        let on = walk(&mut f, (100.0 + SPACING + 1.0, 0.0), (400.0, 0.0), 80, 1);
        assert!((on[0].pos.x - (400.0 - SPACING)).abs() < 1.0, "{on:?}");
    }

    #[test]
    fn a_new_field_a_new_leader_or_a_relocation_starts_the_trail_again() {
        let mut f = Followers::default();
        walk(&mut f, (0.0, 0.0), (400.0, 0.0), 100, 1);
        let moved = f.frame(1, 1, Pos::new(-50.0, 7.0, 3.0), 1.0, AnimState::Idle, 1);
        assert_eq!(moved[0].pos, Pos::new(-50.0, 7.0, 3.0), "a new field: it appears with the leader");
        assert_eq!(moved[0].facing, 1.0);
        walk(&mut f, (-50.0, 7.0), (200.0, 7.0), 50, 1);
        let jumped = f.frame(1, 1, Pos::new(200.0, 2000.0, 0.0), 0.0, AnimState::Idle, 1);
        assert_eq!(jumped[0].pos, Pos::new(200.0, 2000.0, 0.0), "a link's relocation is no walk");
        let other = f.frame(1, 2, Pos::new(10.0, 10.0, 0.0), 0.0, AnimState::Idle, 1);
        assert_eq!(other[0].pos, Pos::new(10.0, 10.0, 0.0), "another leader has its own trail");
    }

    #[test]
    fn the_trail_stays_bounded_and_a_running_leader_makes_them_run() {
        let mut f = Followers::default();
        let mut last = Vec::new();
        for i in 0..5000 {
            last = f.frame(0, 1, Pos::new(i as f32 * 3.0, 0.0, 0.0), 0.0, AnimState::Run, 3);
        }
        assert!(f.trail.len() < 100, "only what the last follower needs is kept: {}", f.trail.len());
        assert!(last.iter().all(|p| p.anim == AnimState::Run));
        assert!(!Followers::default().frame(0, 1, Pos::new(f32::NAN, 0.0, 0.0), 0.0, AnimState::Idle, 2).iter().any(|p| !p.pos.is_finite()));
    }

    #[test]
    fn every_canon_hero_with_a_field_model_has_a_look_the_avatar_table_knows() {
        for character in [0u8, 1, 2, 3, 4, 5, 7, 8] {
            let look = hero_appearance(character).unwrap();
            assert!(fmv_ff7::playable::resolve_appearance(look).is_some(), "{look}");
        }
        assert_eq!(hero_appearance(6), None, "Cait Sith has no playable field model in the table");
    }
}
