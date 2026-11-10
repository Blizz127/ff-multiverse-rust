//! What a frame of the field shows while a scene runs.
//!
//! The window draws the field through `crate::window::field_draw_with`, which asks this module four things:
//!
//! * [`visible_npcs`]: the area's characters minus the scene's trigger (an NPC whose id is from
//!   [`fmv_protocol::SCENE_NPC_ID_BASE`]) while a scene is on show: the cast member that plays the trigger's part stands in
//!   its place (Tifa behind the counter is the cast's Tifa), so the NPC is not drawn twice and not labelled;
//! * [`cast_players`]: every cast member that is placed and not hidden, as an avatar of the cast's look (a playable
//!   canon model: it stands, and walks in its walk clip while a move is on), under a key from a range that cannot meet an
//!   entity's or an NPC's ([`CAST_WHO_BASE`]);
//! * [`place_witnesses`]: every participant of the run, the local player and the others, drawn on its WITNESS spot (the
//!   scene's name for where somebody stands) instead of where it really is, with its own look and the field's model scale
//!   like every other avatar (it is a player body: the same pipeline as the cast). The local player's real place is never
//!   touched: it is where it was drawn before the scene and where it is drawn after;
//! * [`speaker_spot`]: where the speaker of the window on show stands on the screen, for [`super::panel::Panel`] (none
//!   when the field camera has the speaker out of the window: the window then takes its usual place);
//! * [`camera_shot`]: what the scene asks of the window's field camera ([`crate::camera`]).
//!
//! Players that are not participants keep being drawn at their real places.

use super::panel::{Rect, SpeakerSpot};
use super::player::ScenePlayer;
use super::stage::CameraState;
use crate::camera::{scroll_centre, SceneAim, SceneShot};
use crate::avatar::frame::{Built, NpcRef};
use crate::avatar::{AvatarPlayer, Who};
use crate::backdrop::FieldView;
use crate::render::view_to_window;
use crate::state::NpcView;
use crate::ui::hud::Subject;
use fmv_core::{AnimState, EntityId, Pos};
use fmv_ff7::script::scene::Look;
use fmv_protocol::SCENE_NPC_ID_BASE;
use std::borrow::Cow;

/// The animation keys of cast members start here: above every entity id (they count up from 1) and below the keys of the
/// area's characters (`1 << 63`), so a cast member never shares an animation track or a label anchor with either.
pub const CAST_WHO_BASE: u64 = 1 << 62;

/// The ring under a cast member's feet: a neutral grey, so that nothing marks it as a player (players' rings are their own
/// colours; the cast are the story's, not anybody's).
const CAST_TINT: [u8; 3] = [150, 150, 150];

/// The key the avatar pipeline knows the cast member playing script entity `entity` by.
pub fn cast_who(entity: u8) -> Who {
    Who(CAST_WHO_BASE + u64::from(entity))
}

/// Whether the character with this `id` is the trigger of a scene (the server's own ids: `SCENE_NPC_ID_BASE` and up).
pub fn is_trigger(id: u32) -> bool {
    id >= SCENE_NPC_ID_BASE
}

/// The area's characters as a frame draws them: all of them, except, while a scene is on show, the scenes' triggers and the
/// characters the scene's cast plays (an area character whose name is a cast member's: the cast stands for it, where the scene
/// puts it, so it is not drawn twice).
pub fn visible_npcs<'a>(npcs: &'a [NpcView], scene: Option<&ScenePlayer>) -> Cow<'a, [NpcView]> {
    let Some(scene) = scene else { return Cow::Borrowed(npcs) };
    if npcs.iter().any(|n| hides_npc(scene, n)) {
        Cow::Owned(npcs.iter().filter(|n| !hides_npc(scene, n)).cloned().collect())
    } else {
        Cow::Borrowed(npcs)
    }
}

fn hides_npc(scene: &ScenePlayer, npc: &NpcView) -> bool {
    is_trigger(npc.id) || (!npc.name.is_empty() && scene.def().cast.iter().any(|m| m.name.eq_ignore_ascii_case(&npc.name)))
}

/// A scene replaces the trigger and cast NPCs, including their old labels. Otherwise a hidden
/// pedestrian's label would remain at its field position after the scene moves or hides its body.
pub fn label_hidden(scene: Option<&ScenePlayer>, subject: Subject, npcs: &[NpcView]) -> bool {
    let (Some(scene), Subject::Npc(id)) = (scene, subject) else { return false };
    is_trigger(id) || npcs.iter().find(|n| n.id == id).is_some_and(|n| hides_npc(scene, n))
}

/// A held participant can be absent from the picture during an authored entrance. Its name
/// must be absent too: the ordinary HUD fallback would otherwise label its real world position.
pub fn participant_label_hidden(scene: Option<&ScenePlayer>, own: EntityId, subject: Subject) -> bool {
    let entity = match subject {
        Subject::Own => own,
        Subject::Remote(entity) => entity,
        Subject::Npc(_) => return false,
    };
    scene.is_some_and(|scene| scene.hides_participant(entity))
}

/// The cast as avatars to draw.
pub fn cast_players(scene: &ScenePlayer) -> Vec<AvatarPlayer<'_>> {
    scene
        .cast_draws()
        .into_iter()
        .map(|c| {
            let (appearance, npc) = match c.look {
                Look::Appearance(id) => (Some(id.as_str()), None),
                Look::Model { hrc, idle } => (None, Some(NpcRef { model: hrc.as_str(), idle: Some(idle.as_str()) })),
            };
            AvatarPlayer {
                who: cast_who(c.entity),
                pos: c.render_pos,
                facing: c.facing,
                anim: if c.moving { AnimState::Walk } else { AnimState::Idle },
                scene_frame: Some(c.frame),
                scene_clip: c.clip,
                scene_return: c.return_clip,
                appearance,
                npc,
                tint: CAST_TINT,
                own: false,
                party: false,
            }
        })
        .collect()
}

/// Put every participant of the run on its witness spot: the avatar of the player (the local one or another) whose key is
/// the participant's entity is drawn there, standing, facing the way the spot faces. Where the ground's height at the spot
/// is not known the avatar keeps the height it had.
pub fn place_witnesses(players: &mut [AvatarPlayer<'_>], scene: &ScenePlayer) {
    for w in scene.witnesses() {
        let Some(p) = players.iter_mut().find(|p| p.who == Who(w.entity.0) && p.npc.is_none()) else { continue };
        let z = if w.height_known { w.pos.z } else { p.pos.z };
        p.pos = Pos::new(w.pos.x, w.pos.y, z);
        p.facing = w.facing;
        p.anim = AnimState::Idle;
    }
}

/// Where the speaker of the window on show stands on a `vw` x `vh` screen: the top of its head (the anchor the avatar
/// frame computed for it) and its feet, with the rectangles of the other people of the scene (every participant and every
/// other cast member that is drawn as a model: what the window should not hide). `None` when the window has no speaker, or
/// the speaker is not drawn as a model.
pub fn speaker_spot(scene: &ScenePlayer, built: Option<&Built>, view: &FieldView, vw: u32, vh: u32) -> Option<SpeakerSpot> {
    let entity = scene.speaker()?;
    let built = built?;
    // The original dialogue windows stay still while the field shakes. Undo only that offset
    // for automatic speaker placement; body anchors and name labels still follow the field.
    let [x, y] = scene.shake();
    let mut bounds = view.bounds;
    bounds.min.x += x; bounds.max.x += x;
    bounds.min.y -= y; bounds.max.y -= y;
    let pixels = |p: Pos| view_to_window(&bounds, vw, vh, p.x, p.y);
    let casts = scene.cast_draws();
    let person = |who: Who, stands: Pos| -> Option<((f32, f32), (f32, f32))> {
        let head = pixels(*built.anchors.get(&who)?);
        let feet = pixels(view.to_view(stands)?);
        Some(((head[0], head[1]), (feet[0], feet[1])))
    };
    let (head, feet) = person(cast_who(entity), casts.iter().find(|c| c.entity == entity)?.render_pos)?;
    // A speaker the field camera has out of the window is nobody to point at.
    let inside = |p: (f32, f32)| (0.0..=vw as f32).contains(&p.0) && (0.0..=vh as f32).contains(&p.1);
    if !inside(head) && !inside(feet) {
        return None;
    }
    // A person's rectangle: the height between the head and the feet, about half as wide, and a few pixels more each way.
    let rect = |(head, feet): ((f32, f32), (f32, f32))| -> Rect {
        let tall = (feet.1 - head.1).abs().max(8.0);
        let half = 0.3 * tall;
        let (cx, pad) = ((head.0 + feet.0) / 2.0, 4.0);
        (cx - half - pad, head.1.min(feet.1) - pad, cx + half + pad, head.1.max(feet.1) + pad)
    };
    let mut others: Vec<Rect> = casts.iter().filter(|c| c.entity != entity).filter_map(|c| person(cast_who(c.entity), c.render_pos)).map(rect).collect();
    others.extend(scene.witnesses().iter().filter_map(|w| person(Who(w.entity.0), w.pos)).map(rect));
    Some(SpeakerSpot { head, feet, others })
}

/// What the scene asks of the window's field camera this frame ([`crate::camera`]): its last camera instruction on the
/// stage's own clock and the followed actor's logical position. OFST moves the model but not this native camera target.
/// The default lead is the canon Cloud when he is in the cast and on show.
pub fn camera_shot(scene: &ScenePlayer, view: &FieldView) -> SceneShot {
    let clock = scene.camera_clock();
    let casts = scene.cast_draws();
    let drawn = |c: &super::player::CastDraw<'_>| view.to_view(c.pos).map(|p| [p.x, p.y]);
    let lead = casts.iter().find(|c| is_lead(c.look)).and_then(drawn);
    let aim = match clock.state {
        CameraState::Default => SceneAim::Lead { lead },
        CameraState::Follow { entity } => SceneAim::Follow { at: casts.iter().find(|c| c.entity == entity).and_then(drawn), lead },
        CameraState::Scroll { x, y, frames } => SceneAim::Scroll { centre: scroll_centre(x, y), frames, elapsed: clock.frames },
    };
    SceneShot { run: scene.run_id().0, events: clock.events, aim }
}

/// Whether a cast member is the canon Cloud: the playable table's `ff7/cloud`, or a model of his field skeleton `aaaa`.
fn is_lead(look: &Look) -> bool {
    match look {
        Look::Appearance(id) => id == "ff7/cloud",
        Look::Model { hrc, .. } => hrc.eq_ignore_ascii_case("aaaa"),
    }
}
