//! How a non-player character is drawn.
//!
//! The server places NPCs in an area's snapshot (`NpcSnapshot`: a name, a model reference, an idle
//! clip, a position and a heading) and they never move. [`npc_appearance`] is the ONE place that
//! decides what an NPC looks like:
//!
//! * the character model its data names, drawn by the same pipeline as the players' models
//!   (`crate::avatar`): standing in its idle clip, at its position, facing its heading, a shadow
//!   under it and its name over its head. `NpcView::model` is the `char.lgp` skeleton stem for FF7
//!   and `idle_animation` the clip that shows it standing; both are looked up in **this machine's
//!   own** `char.lgp` and never come from the network (only the two names do);
//! * a marker (the players' marker shape, in a dull colour derived from the model reference, so
//!   that the guards of a platform read as one kind of thing) for every NPC whose model cannot be
//!   had here: no `char.lgp`, a model that does not load, a fixture area, or an area with more
//!   models than the cache holds. The marker is also what the NPC's name stands over then.
//!
//! The window builds every NPC as an [`AvatarPlayer`] ([`npc_players`]) and hands them to
//! `Avatars::frame` together with the players; the frame says which of them became models and which
//! stay markers. Nothing here reads game files or the network.

use crate::avatar::frame::NpcRef;
use crate::avatar::{AvatarPlayer, Who};
use crate::state::NpcView;
use fmv_core::AnimState;

/// Marker colour range: dull and cool, far from the saturated colours of the players' markers.
const MARKER_BASE: u8 = 96;
const MARKER_SPAN: u8 = 96;

/// The animation keys of non-player characters start here, far above any entity id the server
/// hands out (they count up from 1), so an NPC and a player never share an animation track or a
/// label anchor.
const NPC_WHO_BASE: u64 = 1 << 63;

/// What one NPC looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcLook<'a> {
    /// The model to draw, as the area's data names it (an empty `model` can never be had: a
    /// marker). Whether this machine has it is the renderer's to find out.
    pub model: NpcRef<'a>,
    /// The colour of the marker an NPC without a model here is drawn as.
    pub marker_tint: [u8; 3],
}

/// FNV-1a over the bytes: a stable hash that needs no dependency (the colour must not change
/// between runs or machines).
fn hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// The look of `npc`. THE hook for the model renderer (see the module docs).
pub fn npc_appearance(npc: &NpcView) -> NpcLook<'_> {
    let h = hash(&npc.model);
    let channel = |shift: u32| MARKER_BASE + u8::try_from((h >> shift) % u32::from(MARKER_SPAN)).unwrap_or(0);
    NpcLook {
        model: NpcRef { model: &npc.model, idle: npc.idle_animation.as_deref() },
        marker_tint: [channel(0), channel(8), channel(16)],
    }
}

/// The key the avatar pipeline knows NPC number `id` of the area by.
pub fn npc_who(id: u32) -> Who {
    Who(NPC_WHO_BASE + u64::from(id))
}

/// Every NPC of the area as something to draw: standing still (idle) where the server put it, facing
/// the way it says, a marker if its model is not to be had. The window and the offscreen shots
/// both build their NPCs here.
pub fn npc_players(npcs: &[NpcView]) -> Vec<AvatarPlayer<'_>> {
    npcs.iter()
        .map(|n| {
            let look = npc_appearance(n);
            AvatarPlayer {
                who: npc_who(n.id),
                pos: n.pos,
                facing: n.facing,
                anim: AnimState::Idle,
                appearance: None,
                npc: Some(look.model),
                tint: look.marker_tint,
                own: false,
                party: false,
            }
        })
        .collect()
}

/// The models the NPCs name, for `Avatars::set_npcs`.
pub fn npc_models(npcs: &[NpcView]) -> Vec<NpcRef<'_>> {
    npcs.iter().map(|n| npc_appearance(n).model).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::Pos;

    fn npc(name: &str, model: &str) -> NpcView {
        NpcView { id: 0, name: name.into(), model: model.into(), idle_animation: None, pos: Pos::default(), facing: 0.0 }
    }

    #[test]
    fn the_same_model_always_gets_the_same_dull_marker() {
        let a = npc_appearance(&npc("gu0", "aaaa")).marker_tint;
        assert_eq!(a, npc_appearance(&npc("gu1", "aaaa")).marker_tint, "the model decides, not the name");
        assert_ne!(a, npc_appearance(&npc("gu0", "aagb")).marker_tint);
        for model in ["", "aaaa", "aagb", "acgd", "\u{10FFFF}\u{10FFFF}", &"x".repeat(100)] {
            let [r, g, b] = npc_appearance(&npc("n", model)).marker_tint;
            for c in [r, g, b] {
                assert!((MARKER_BASE..MARKER_BASE + MARKER_SPAN).contains(&c), "{model:?}: {c}");
            }
        }
    }

    #[test]
    fn the_look_names_the_model_and_the_standing_clip_the_data_gives() {
        let mut guard = npc("gu0", "agyb");
        guard.idle_animation = Some("acfe".into());
        let look = npc_appearance(&guard);
        assert_eq!(look.model, NpcRef { model: "agyb", idle: Some("acfe") });
        // Nothing named is nothing to look up: the renderer draws a marker for it.
        let bare = npc("x", "");
        assert_eq!(npc_appearance(&bare).model, NpcRef { model: "", idle: None });
    }

    #[test]
    fn every_npc_is_something_to_draw_standing_still_where_the_server_put_it() {
        let mut guard = npc("gu0", "agyb");
        guard.id = 3;
        guard.pos = Pos::new(10.0, 20.0, 30.0);
        guard.facing = 1.25;
        guard.idle_animation = Some("acfe".into());
        let mut other = npc("gu1", "aagb");
        other.id = 4;
        let npcs = [guard, other];
        let drawn = npc_players(&npcs);
        assert_eq!(drawn.len(), 2);
        let first = &drawn[0];
        assert_eq!((first.pos, first.facing, first.anim), (Pos::new(10.0, 20.0, 30.0), 1.25, AnimState::Idle));
        assert_eq!(first.npc, Some(NpcRef { model: "agyb", idle: Some("acfe") }));
        assert_eq!((first.appearance, first.own, first.party), (None, false, false), "never a player");
        assert_eq!(first.tint, npc_appearance(&npcs[0]).marker_tint);
        assert_eq!(drawn[1].npc, Some(NpcRef { model: "aagb", idle: None }));
        assert_eq!(npc_models(&npcs), [NpcRef { model: "agyb", idle: Some("acfe") }, NpcRef { model: "aagb", idle: None }]);
    }

    #[test]
    fn an_npc_never_shares_a_key_with_a_player() {
        // Entity ids count up from 1; an NPC's key is far above any of them, and its own index
        // makes the keys of one area differ.
        assert_ne!(npc_who(0), npc_who(1));
        for entity in [0u64, 1, 2, 1000, u64::from(u32::MAX), 1 << 40] {
            assert_ne!(npc_who(0), Who(entity));
            assert_ne!(npc_who(u32::MAX), Who(entity));
        }
    }
}
