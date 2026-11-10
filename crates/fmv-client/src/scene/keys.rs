//! Where a key goes while a story scene may be on show: the one decision the window makes before the field's own key
//! handling ([`route_key`]), kept out of the window so it can be tested without one.
//!
//! * A suspended scene gives its battle and result panel the keys. Otherwise a scene on show takes
//!   every key ([`SceneRoute::Scene`]): its own keys (the talk window's) do what they do and the rest
//!   do nothing, so nothing reaches the field: no walking, no talking, no battle key, no `Interact`. (A key being let go
//!   of still ends the walk: the window passes releases to the walk, which must never miss one.)
//! * Otherwise, with no dialogue open, F6 asks the server for a preview of the game's first scene (a debug aid: the server
//!   refuses unless it runs with `--debug-scene-preview`), not in a battle ([`SceneRoute::Preview`]).
//! * Otherwise a fresh OK with a scene's trigger in reach and in front starts the scene ([`SceneRoute::Press`]: the window
//!   sends `Interact` naming it, and the field's talk never sees the key).
//! * Anything else is the field's ([`SceneRoute::Field`]), exactly as before scenes.

use super::trigger::trigger_press;
use crate::state::ClientState;
use fmv_ff7::script::scene::scenes;
use winit::keyboard::KeyCode;

/// Where a key goes (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneRoute {
    /// A scene is on show and took the key.
    Scene,
    /// F6: a preview of the first scene was queued for the server.
    Preview,
    /// OK next to a scene's trigger: send `Interact` naming this NPC id.
    Press(u32),
    /// Not a scene's: the field handles it.
    Field,
}

/// Route one key event. `talk_open` is whether a dialogue window is open (it owns the keys then).
pub fn route_key(state: &mut ClientState, area: &str, now: f64, talk_open: bool, code: KeyCode, pressed: bool, repeat: bool) -> SceneRoute {
    // A suspended story still holds field movement, but combat and its result panel own input.
    if state.scene.battle_input() && (state.battle.in_battle() || state.battle.lingering(now)) {
        return SceneRoute::Field;
    }
    if state.scene.key(code, pressed, repeat) {
        return SceneRoute::Scene;
    }
    if talk_open {
        return SceneRoute::Field;
    }
    if code == KeyCode::F6 && pressed && !repeat && !state.battle.in_battle() && !state.battle.lingering(now) {
        if let Some(first) = scenes().first() {
            state.scene.request_preview(&first.id);
            return SceneRoute::Preview;
        }
    }
    match trigger_press(state, area, now, code, pressed, repeat) {
        Some(npc) => SceneRoute::Press(npc),
        None => SceneRoute::Field,
    }
}
