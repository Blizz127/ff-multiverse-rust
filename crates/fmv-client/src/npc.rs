//! How a non-player character is drawn.
//!
//! The server places NPCs in an area's snapshot (`NpcSnapshot`: a name, a model reference, an idle
//! clip, a position and a heading) and they never move. Until the character model renderer can
//! draw them, an NPC is a marker with its name over it. [`npc_appearance`] is the ONE place that
//! decides what an NPC looks like, so that the renderer hooks in here:
//!
//! * today it answers [`NpcLook::Marker`], a plain marker in a colour derived from the model
//!   reference (the same model always has the same colour, so the guards of a platform read as one
//!   kind of thing);
//! * a model renderer returns a model look for the references it can resolve from this machine's
//!   own game files (`NpcView::model` is the `char.lgp` skeleton stem for FF7, `idle_animation`
//!   the clip that shows it standing) and falls back to the marker for the rest.
//!
//! Nothing here reads game files or the network.

use crate::state::NpcView;

/// Marker colour range: dull and cool, far from the saturated colours of the players' markers.
const MARKER_BASE: u8 = 96;
const MARKER_SPAN: u8 = 96;

/// What one NPC looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpcLook {
    /// A flat marker (the player markers' shape) in this colour.
    Marker { tint: [u8; 3] },
}

impl NpcLook {
    /// The colour of the marker an NPC without a model is drawn as. (Every look has one: the
    /// marker is also what a model look falls back to, and what its label stands over.)
    pub fn marker_tint(&self) -> [u8; 3] {
        match self {
            NpcLook::Marker { tint } => *tint,
        }
    }
}

/// FNV-1a over the bytes: a stable hash that needs no dependency (the colour must not change
/// between runs or machines).
fn hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// The look of `npc`. THE hook for the model renderer (see the module docs).
pub fn npc_appearance(npc: &NpcView) -> NpcLook {
    let h = hash(&npc.model);
    let channel = |shift: u32| MARKER_BASE + u8::try_from((h >> shift) % u32::from(MARKER_SPAN)).unwrap_or(0);
    NpcLook::Marker { tint: [channel(0), channel(8), channel(16)] }
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
        let a = npc_appearance(&npc("gu0", "aaaa")).marker_tint();
        assert_eq!(a, npc_appearance(&npc("gu1", "aaaa")).marker_tint(), "the model decides, not the name");
        assert_ne!(a, npc_appearance(&npc("gu0", "aagb")).marker_tint());
        for model in ["", "aaaa", "aagb", "acgd", "\u{10FFFF}\u{10FFFF}", &"x".repeat(100)] {
            let [r, g, b] = npc_appearance(&npc("n", model)).marker_tint();
            for c in [r, g, b] {
                assert!((MARKER_BASE..MARKER_BASE + MARKER_SPAN).contains(&c), "{model:?}: {c}");
            }
        }
    }
}
