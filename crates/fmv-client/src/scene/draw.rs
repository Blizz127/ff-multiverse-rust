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
//! * [`speaker_spot`]: where the speaker of the window on show stands on the screen, for [`super::panel::Panel`].
//!
//! Players that are not participants keep being drawn at their real places.

use super::panel::SpeakerSpot;
use super::player::ScenePlayer;
use crate::avatar::frame::{Built, NpcRef};
use crate::avatar::{AvatarPlayer, Who};
use crate::backdrop::FieldView;
use crate::render::view_to_window;
use crate::state::NpcView;
use crate::ui::hud::Subject;
use fmv_core::{AnimState, Pos};
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

/// The area's characters as a frame draws them: all of them, except the scenes' triggers while a scene is on show.
pub fn visible_npcs<'a>(npcs: &'a [NpcView], scene: Option<&ScenePlayer>) -> Cow<'a, [NpcView]> {
    if scene.is_some() && npcs.iter().any(|n| is_trigger(n.id)) {
        Cow::Owned(npcs.iter().filter(|n| !is_trigger(n.id)).cloned().collect())
    } else {
        Cow::Borrowed(npcs)
    }
}

/// Whether a name label is not drawn while `scene` is on show: the label of a scene's trigger.
pub fn label_hidden(scene: Option<&ScenePlayer>, subject: Subject) -> bool {
    scene.is_some() && matches!(subject, Subject::Npc(id) if is_trigger(id))
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
                pos: c.pos,
                facing: c.facing,
                anim: if c.moving { AnimState::Walk } else { AnimState::Idle },
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
/// frame computed for it) and its feet. `None` when the window has no speaker, or the speaker is not drawn as a model.
pub fn speaker_spot(scene: &ScenePlayer, built: Option<&Built>, view: &FieldView, vw: u32, vh: u32) -> Option<SpeakerSpot> {
    let entity = scene.speaker()?;
    let head = *built?.anchors.get(&cast_who(entity))?;
    let feet = view.to_view(scene.cast_draws().into_iter().find(|c| c.entity == entity)?.pos)?;
    let pixels = |p: Pos| view_to_window(&view.bounds, vw, vh, p.x, p.y);
    let (head, feet) = (pixels(head), pixels(feet));
    Some(SpeakerSpot { head: (head[0], head[1]), feet: (feet[0], feet[1]) })
}
