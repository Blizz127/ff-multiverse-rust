//! The OK that starts a scene.
//!
//! The server adds the trigger of every scene on offer to the area's characters (an NPC from
//! [`fmv_protocol::SCENE_NPC_ID_BASE`]; the `n`-th scene of the game's list is `SCENE_NPC_ID_BASE + n`). A fresh OK press
//! (Enter, Space, a pad's A) in the field, with such a character in reach and in front, starts that scene: the press goes to
//! the server as `Interact { npc: Some(id) }` and the key is consumed, so the character's ordinary talk does not open as well.
//!
//! **Reach** is the same rule as talking ([`crate::ui::dialogue::pick_target`]: the character's talk radius, and within 70
//! degrees of where the player faces unless it is very near), with the radius the scene's own data gives, read from this
//! machine's field ([`super::SceneClient::reach`], cached per field). The server checks it again; this check keeps a press
//! that cannot start anything from being spent on it, and from taking the key away from the ordinary talk.
//!
//! A client that cannot tell a trigger's reach (no game data) does nothing here: the press falls through to the field's own
//! keys, as it did before scenes.
//!
//! A press is never a scene's while a scene is on show (every key is the scene's then), while a dialogue is open (the caller
//! checks) and in a battle or while its panel lingers.

use super::player::SceneKey;
use super::draw::is_trigger;
use crate::state::ClientState;
use crate::ui::dialogue::{pick_target, Spot};
use fmv_protocol::SCENE_NPC_ID_BASE;
use winit::keyboard::KeyCode;

/// The id of the scene trigger a key event starts the scene of, if it is a fresh OK press with one in reach and in front.
/// `area` is the key of the field the player stands in and `now` the session's clock.
pub fn trigger_press(state: &mut ClientState, area: &str, now: f64, code: KeyCode, pressed: bool, repeat: bool) -> Option<u32> {
    if SceneKey::from_event(code, pressed, repeat) != Some(SceneKey::Confirm) || state.scene.is_active() {
        return None;
    }
    if state.battle.in_battle() || state.battle.lingering(now) {
        return None;
    }
    let triggers: Vec<(u32, fmv_core::Pos)> = state.npc_views().iter().filter(|n| is_trigger(n.id)).map(|n| (n.id, n.pos)).collect();
    if triggers.is_empty() {
        return None;
    }
    let mut ids = Vec::new();
    let mut spots = Vec::new();
    for (id, pos) in triggers {
        let Some(index) = id.checked_sub(SCENE_NPC_ID_BASE).and_then(|i| usize::try_from(i).ok()) else { continue };
        if let Some(radius) = state.scene.reach(index, area) {
            ids.push(id);
            spots.push(Spot { pos, radius });
        }
    }
    let picked = pick_target(state.own.pos(), state.own.facing(), &spots)?;
    ids.get(picked).copied()
}
