//! One frame of avatars: pose, place and skin every visible player into camera space, and work out
//! the marks under them and where their name labels go.
//!
//! Everything here is pure CPU work on already loaded characters, so the placement (standing on the
//! walkmesh at the entity's position, upright, facing its heading, scaled for the field) is checked
//! by unit tests without a GPU. The GPU side ([`super::gpu`]) only uploads what [`Avatars::frame`]
//! built and draws it.
//!
//! # From a model vertex to the screen
//!
//! ```text
//! bone-local vertex --pose[bone]--> native model space --model_to_field--> field space
//!                   --camera (R, T)--> camera space  --(GPU) zoom / depth, window fit--> pixels
//! ```
//!
//! The first three steps are `fmv_ff7`'s documented conventions ([`fmv_ff7::model_to_field`] puts the
//! character's feet at the entity position, heading along the entity's facing, one model unit being
//! [`Stage::model_scale`] walkmesh units); the camera is the field camera the picture and the
//! walkmesh are projected with.

use super::anim::{Animator, Who};
use super::gpu::Tint;
use super::ground::{self, GroundShape, GroundVertex};
use super::identity::Identity;
use super::library::{is_placeholder_body, AvatarAsset, AvatarLibrary};
use super::scenery::Scenery;
use super::stage::Stage;
use crate::backdrop::FieldView;
use crate::render::Camera;
use fmv_core::{AnimState, Pos};
use fmv_ff7::model::model_to_field;
use std::collections::HashMap;
use std::sync::Arc;

/// Models drawn per frame at most (the nearest ones; the rest are drawn as markers). A field has a
/// handful of players; the cap only bounds the per-frame cost of a crowded one.
pub const MAX_MODELS: usize = 64;

/// How far the face overlays of a model are pulled towards the camera in the depth test, in model
/// units (see [`super::mesh::MergedMesh::overlay`]).
pub const OVERLAY_BIAS_MODEL_UNITS: f32 = 0.12;

/// Radius of the mark under a player, in model units (a character is about 10 across).
pub const RING_RADIUS_UNITS: f32 = 5.5;

const WHITE: [u8; 3] = [255, 255, 255];
const GOLD: [u8; 3] = crate::render::RING_RGB;

/// The least of each colour channel a placeholder body kept under its player's colour in the OLD look: a channel of
/// the identity colour at 0 scaled the model's own colour by this, at 255 left it as it was (see [`body_tint`]). The
/// look is now [`super::identity`]'s; this stays as the reference the tests compare it with.
#[cfg(test)]
pub const BODY_TINT_FLOOR: f32 = 0.45;

/// The colour factor a placeholder body was drawn with in the OLD look: its player's identity colour `c` gave each
/// channel `BODY_TINT_FLOOR + (1 - BODY_TINT_FLOOR) * c / 255`, so a white identity left the model as it was and a
/// saturated one pulled it towards that colour (the model's own colour multiplied by it, never replaced; which is why
/// it hardly showed on dark or saturated cloth). Opaque. Kept for the tests that draw the old look beside the new.
#[cfg(test)]
pub fn body_tint(identity: [u8; 3]) -> Tint {
    let channel = |c: u8| BODY_TINT_FLOOR + (1.0 - BODY_TINT_FLOOR) * f32::from(c) / 255.0;
    Tint { rgb: identity.map(channel), alpha: 1.0 }
}

/// What colours the model of `player`: a placeholder body takes its player's identity colour
/// ([`super::identity`]); everything else keeps its own colours and is marked on the ground only (a canon model,
/// which a legacy binding names, the default character that stands in for an unknown look, and a non-player
/// character).
fn model_identity(player: &AvatarPlayer<'_>, asset: &AvatarAsset) -> Identity {
    if player.npc.is_none() && is_placeholder_body(&asset.appearance) {
        Identity::of(player.tint)
    } else {
        Identity::NONE
    }
}

/// The model of a non-player character, as an area's data names it: untrusted text from the
/// server's snapshot, only ever looked up as `char.lgp` entry stems
/// ([`super::library::AvatarLibrary::get_npc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcRef<'a> {
    /// The skeleton's stem (`<model>.hrc`); empty when the data names none (always a marker).
    pub model: &'a str,
    /// The stem of the clip that shows it standing (`<idle>.a`).
    pub idle: Option<&'a str>,
}

/// A player to draw: where it stands, which way it faces and how it moves, and who it is.
#[derive(Debug, Clone, Copy)]
pub struct AvatarPlayer<'a> {
    /// Keys the animation phase: the entity id.
    pub who: Who,
    pub pos: Pos,
    pub facing: f32,
    pub anim: AnimState,
    /// The look the avatar names (untrusted text; only looked up in the table).
    pub appearance: Option<&'a str>,
    /// `Some` for a non-player character of the area instead of a player: the model its data
    /// names. It is drawn with that model standing (shadow only under it, no rings), and as a
    /// marker (`tint` its colour) when there is no such model here; `appearance` is not used.
    pub npc: Option<NpcRef<'a>>,
    pub tint: [u8; 3],
    /// The local player.
    pub own: bool,
    /// A member of the local player's party.
    pub party: bool,
}

/// A skinned model ready to upload: positions in camera space, one per merged-mesh vertex.
pub struct SkinnedModel {
    pub asset: Arc<AvatarAsset>,
    pub positions: Vec<[f32; 3]>,
}

/// Everything the renderer needs to draw the avatars of one frame.
pub struct AvatarFrame {
    pub models: Vec<SkinnedModel>,
    /// The colour factor and opacity of each of `models`, in the same order (a model past the end of this list, or
    /// every model when it is empty, is drawn as it is). The field leaves it empty: its players' colours are
    /// [`AvatarFrame::identities`]; the battle's (a fading enemy, an effect's glow) are `BattleFrame::tints`.
    pub tints: Vec<Tint>,
    /// The identity colour of each of `models`, in the same order (a model past the end of this list, or every model
    /// when it is empty, has none): a placeholder body in its player's colour, nothing for the rest.
    pub identities: Vec<Identity>,
    /// The camera's focal length ([`super::stage::StageCamera::zoom`]).
    pub zoom: f32,
    /// Depth bias of the face overlays, in camera-space units.
    pub overlay_bias: f32,
    /// Marks on the ground, under the models (each at the depth of its feet).
    pub ground: Vec<GroundShape>,
    /// The field's picture as depth-tested tiles, if it has something in front of the characters:
    /// drawn instead of the flat picture, around the models (see [`super::scenery`]).
    pub scenery: Option<Arc<Scenery>>,
}

impl std::fmt::Debug for AvatarFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarFrame")
            .field("models", &self.models.len())
            .field("ground", &self.ground.len())
            .field("scenery", &self.scenery.is_some())
            .finish()
    }
}

impl AvatarFrame {
    /// The ground marks as flat-colour vertices (with their depth) for a `vw` x `vh` window fitted
    /// by `cam`.
    pub fn ground_vertices(&self, cam: &Camera, vw: u32, vh: u32, srgb: bool) -> Vec<GroundVertex> {
        ground::vertices(&self.ground, cam, vw, vh, srgb)
    }

    /// Nothing to draw at all (no models, no scenery).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.scenery.is_none()
    }
}

/// What [`Avatars::frame`] decided about the players it was given.
#[derive(Debug)]
pub struct Built {
    pub frame: AvatarFrame,
    /// Indices (into the given players) of those that could not be drawn as models (their
    /// character failed to load, or there were more than [`MAX_MODELS`]): draw their markers.
    pub markers: Vec<usize>,
    /// Where the name label of each model-drawn player goes: a view-space point just above its head.
    pub anchors: HashMap<Who, Pos>,
}

// ---------------------------------------------------------------------------------------------
// Matrices (column-major, f64: the camera's translation is large)
// ---------------------------------------------------------------------------------------------

type Mat = [f64; 16];

fn mul(a: &Mat, b: &Mat) -> Mat {
    std::array::from_fn(|index| {
        let (column, row) = (index / 4, index % 4);
        (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum()
    })
}

fn promote(m: &[f32; 16]) -> Mat {
    m.map(f64::from)
}

/// Skin `asset` in `pose` (per-bone matrices from the clip) placed by `place` (model space to
/// field space), into camera space: `out` gets one position per merged-mesh vertex. `false` (and
/// `out` unusable) if the pose lacks a bone or anything comes out non-finite.
pub fn skin(asset: &AvatarAsset, pose: &[[f32; 16]], place: &[f32; 16], camera: &Mat, out: &mut Vec<[f32; 3]>) -> bool {
    out.clear();
    let mesh = &asset.mesh;
    out.reserve(mesh.positions.len());
    let to_camera = mul(camera, &promote(place));
    for run in &mesh.bone_runs {
        let Some(bone) = pose.get(run.bone) else { return false };
        let Some(vertices) = mesh.positions.get(run.start..run.start + run.len) else { return false };
        let m = mul(&to_camera, &promote(bone));
        for p in vertices {
            let p = p.map(f64::from);
            out.push([
                (m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12]) as f32,
                (m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13]) as f32,
                (m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14]) as f32,
            ]);
        }
    }
    out.len() == mesh.positions.len() && out.iter().flatten().all(|c| c.is_finite())
}

/// The ground marks of one player (`depth`: the camera depth of its feet): a soft shadow, the tint
/// ring, and for the local player a white outline and for a party member a gold ring around it all.
fn ground_marks(view: &FieldView, player: &AvatarPlayer<'_>, stand_in: bool, scale: f32, depth: f32) -> Vec<GroundShape> {
    let to_view = |p: Pos| view.to_view(p);
    let circle = |k: f32| ground::circle(&to_view, player.pos, RING_RADIUS_UNITS * scale * k);
    let mut marks = Vec::new();
    if let Some(shadow) = circle(0.95) {
        marks.push(GroundShape::disc(shadow, [0, 0, 0], 0.30, depth));
    }
    // A non-player character is scenery: a shadow, and none of the rings that tell players apart.
    if player.npc.is_some() {
        return marks;
    }
    // A stand-in (the player's own look is unknown here) leans on the tint more: a stronger ring.
    let tint_alpha = if stand_in { 1.0 } else { 0.85 };
    if let (Some(outer), Some(inner)) = (circle(1.0), circle(0.78)) {
        marks.push(GroundShape::band(outer, inner, player.tint, tint_alpha, depth));
    }
    if player.own {
        if let (Some(outer), Some(inner)) = (circle(1.15), circle(1.0)) {
            marks.push(GroundShape::band(outer, inner, WHITE, 1.0, depth));
        }
    }
    if player.party {
        if let (Some(outer), Some(inner)) = (circle(1.42), circle(1.26)) {
            marks.push(GroundShape::band(outer, inner, GOLD, 1.0, depth));
        }
    }
    marks
}

/// The avatars of a window: the characters loaded so far and every player's animation phase.
pub struct Avatars {
    library: AvatarLibrary,
    animator: Animator,
}

impl Avatars {
    pub fn new(library: AvatarLibrary) -> Self {
        Self { library, animator: Animator::new() }
    }

    /// The avatars of this machine's own `char.lgp`, if it has one (see [`super::source`]).
    pub fn from_local_content() -> Option<Self> {
        let path = super::source::remembered()?;
        Some(Self::new(AvatarLibrary::new(Box::new(super::source::LgpLoader::new(path)))))
    }

    /// Load the character an avatar names now, so the first frame that shows it does not stall.
    pub fn preload(&mut self, appearance: Option<&str>) {
        self.library.get(appearance);
    }

    /// The area's non-player characters changed: these are the models they name. Loads them now
    /// (so the first frame that shows them does not stall) and drops what the area left behind.
    pub fn set_npcs(&mut self, npcs: &[NpcRef<'_>]) {
        self.library.keep_npcs(npcs.iter().map(|n| (n.model, n.idle)));
    }

    /// Characters held by the cache.
    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.library.cached()
    }

    /// Non-player models held by the cache.
    #[cfg(test)]
    pub fn cached_npcs(&self) -> usize {
        self.library.cached_npcs()
    }

    /// Build the frame for `players` standing in `view`'s field. A player that cannot be drawn at
    /// all (not finite, behind the camera) is simply left out, as it always was; one whose
    /// character is unavailable is listed in [`Built::markers`]. `dt` is the time since the last
    /// frame, in seconds (it moves the walk and run loops on).
    pub fn frame(&mut self, view: &FieldView, stage: &Stage, players: &[AvatarPlayer<'_>], dt: f32) -> Built {
        self.animator.begin_frame();
        let camera = stage.camera.matrix();
        let scale = stage.model_scale;

        // Which players can be drawn, nearest first (the cap keeps the nearest). The players come
        // before the area's non-player characters, so that a crowd of those can never take the
        // place of a player: the cap is spent on players first.
        let mut order: Vec<(usize, f64)> = players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.pos.is_finite() && p.facing.is_finite() && view.to_view(p.pos).is_some())
            .filter_map(|(i, p)| {
                let depth = stage.camera.project([f64::from(p.pos.x), f64::from(p.pos.y), f64::from(p.pos.z)])?[2];
                Some((i, depth))
            })
            .collect();
        let is_npc = |i: usize| players.get(i).is_some_and(|p| p.npc.is_some());
        order.sort_by(|a, b| is_npc(a.0).cmp(&is_npc(b.0)).then(a.1.total_cmp(&b.1)).then(a.0.cmp(&b.0)));

        let mut models: Vec<SkinnedModel> = Vec::new();
        let mut identities: Vec<Identity> = Vec::new();
        let mut ground_marks_by_player: Vec<(usize, Vec<GroundShape>)> = Vec::new();
        let mut markers: Vec<usize> = Vec::new();
        let mut anchors = HashMap::new();
        for (rank, &(i, feet_depth)) in order.iter().enumerate() {
            let Some(p) = players.get(i) else { continue };
            if rank >= MAX_MODELS {
                markers.push(i);
                continue;
            }
            // A player is its table character (the default, flagged, when it names none that
            // exists); a non-player character is the model its data names, or a marker.
            let (asset, stand_in) = match p.npc {
                Some(npc) => (self.library.get_npc(npc.model, npc.idle), false),
                None => {
                    let look = self.library.get(p.appearance);
                    (look.asset, look.stand_in)
                }
            };
            let Some(asset) = asset else {
                markers.push(i);
                continue;
            };
            let clip = asset.character.animation(p.anim);
            let playback = if p.npc.is_some() && p.anim == AnimState::Idle {
                self.animator.advance_npc(p.who, dt, clip.frame_count())
            } else {
                self.animator.advance(p.who, p.anim, dt, clip.frame_count())
            };
            let Some(pose) = clip.pose_at(&asset.character.model.bones, playback.frame) else {
                markers.push(i);
                continue;
            };
            let place = model_to_field([p.pos.x, p.pos.y, p.pos.z], p.facing, scale);
            let mut positions = Vec::new();
            if !skin(&asset, &pose, &place, &camera, &mut positions) {
                markers.push(i);
                continue;
            }
            let head = Pos::new(p.pos.x, p.pos.y, p.pos.z + asset.height_units * scale);
            if let Some(anchor) = view.to_view(head).or_else(|| view.to_view(p.pos)) {
                anchors.insert(p.who, anchor);
            }
            ground_marks_by_player.push((i, ground_marks(view, p, stand_in, scale, feet_depth as f32)));
            identities.push(model_identity(p, &asset));
            models.push(SkinnedModel { asset, positions });
        }

        // Draw the marks of the far players first, so a near player's ring is on top of theirs.
        ground_marks_by_player.sort_by_key(|(i, _)| std::cmp::Reverse(order.iter().position(|(j, _)| j == i)));
        let ground: Vec<GroundShape> = ground_marks_by_player.into_iter().flat_map(|(_, marks)| marks).collect();

        markers.sort_unstable();
        Built {
            frame: AvatarFrame {
                models,
                tints: Vec::new(),
                identities,
                zoom: stage.camera.zoom as f32,
                overlay_bias: OVERLAY_BIAS_MODEL_UNITS * scale,
                ground,
                scenery: stage.scenery.clone(),
            },
            markers,
            anchors,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::avatar::library::tests::Counting;
    use crate::avatar::stage::StageCamera;
    use fmv_adapter_api::Bounds;
    use fmv_ff7::field::Camera as FieldCamera;
    use fmv_ff7::model::{mat4_identity, transform_point};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A camera 1200 units from the origin looking along world +y, pitched 30 degrees down: x right,
    /// z up on screen, like an FF7 field camera.
    pub(crate) fn camera() -> FieldCamera {
        let (s, c) = (30.0f32.to_radians().sin(), 30.0f32.to_radians().cos());
        let q = |v: f32| (v * 4096.0).round() as i16;
        FieldCamera { axes: [[q(1.0), 0, 0], [0, q(-s), q(-c)], [0, q(c), q(-s)]], position: [0, 0, 1200], zoom: 400 }
    }

    pub(crate) fn stage() -> Stage {
        Stage {
            camera: StageCamera::from_camera(&camera()),
            model_scale: 4.0,
            walk_speed: 60.0,
            run_speed: 240.0,
            scenery: None,
        }
    }

    /// A view through [`camera`] with the usual y-up flip of field-screen space.
    pub(crate) fn view() -> FieldView {
        let stage = stage();
        let camera = stage.camera;
        let bounds = Bounds { min: Pos::new(-300.0, -300.0, 0.0), max: Pos::new(300.0, 300.0, 0.0) };
        FieldView::projected(
            &[],
            bounds,
            move |p: Pos| {
                camera.project([f64::from(p.x), f64::from(p.y), f64::from(p.z)]).map(|s| Pos::new(s[0] as f32, -s[1] as f32, 0.0))
            },
            None,
            "test view",
        )
    }

    fn player(id: u64, pos: Pos, facing: f32, anim: AnimState) -> AvatarPlayer<'static> {
        AvatarPlayer { who: Who(id), pos, facing, anim, appearance: Some("ff7/cloud"), npc: None, tint: [10, 200, 30], own: false, party: false }
    }

    fn avatars(fail: Vec<&'static str>) -> (Avatars, Rc<RefCell<Vec<&'static str>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let loader = Counting::new(Rc::clone(&asked), fail);
        (Avatars::new(AvatarLibrary::new(Box::new(loader))), asked)
    }

    fn cloud_asset() -> Arc<AvatarAsset> {
        let (mut a, _) = avatars(vec![]);
        a.library.get(Some("ff7/cloud")).asset.unwrap()
    }

    /// The colour factor: each channel of the identity colour `c` gives `0.45 + 0.55 * c / 255`.
    #[test]
    fn the_body_tint_is_the_players_colour_over_a_floor_that_keeps_the_texture_readable() {
        assert_eq!(BODY_TINT_FLOOR, 0.45);
        assert_eq!(body_tint([255, 255, 255]), Tint::NONE, "a white identity leaves the model as it is");
        assert_eq!(body_tint([0, 0, 0]), Tint { rgb: [0.45; 3], alpha: 1.0 }, "black does not black the model out");
        let red = body_tint([255, 0, 0]);
        assert_eq!(red.rgb, [1.0, 0.45, 0.45]);
        assert_eq!(red.alpha, 1.0, "a tint only colours: the model stays opaque");
        let mid = body_tint([0, 128, 255]);
        assert!((mid.rgb[1] - (0.45 + 0.55 * 128.0 / 255.0)).abs() < 1e-6 && mid.rgb[2] == 1.0, "{mid:?}");
        // Monotone in each channel, and always between the floor and 1: a colour never darkens below the floor nor brightens.
        for c in 0..=255u8 {
            let t = body_tint([c, c, c]).rgb;
            assert!(t[0] == t[1] && t[1] == t[2] && (BODY_TINT_FLOOR..=1.0).contains(&t[0]), "{c}: {t:?}");
            if c > 0 {
                assert!(t[0] >= body_tint([c - 1, c - 1, c - 1]).rgb[0], "{c}");
            }
        }
        // Different colours are plainly different tints (the whole point of the tint).
        let (a, b) = (body_tint([70, 130, 245]).rgb, body_tint([240, 120, 40]).rgb);
        assert!((a[2] - b[2]).abs() > 0.2 && (a[0] - b[0]).abs() > 0.2, "{a:?} {b:?}");
    }

    /// What carries an identity colour: a placeholder body takes its own player's colour; a canon model (a legacy
    /// binding), the default character standing in for an unknown look and a non-player character keep their own
    /// colours. The frame's tints stay empty (they are for opacity and glow), and so does the old body tint.
    #[test]
    fn a_placeholder_body_carries_its_players_identity_colour_and_nothing_else_does() {
        use fmv_ff7::playable::PLACEHOLDER_BODIES;
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let body_a = PLACEHOLDER_BODIES[0].model.appearance_ref;
        let body_b = PLACEHOLDER_BODIES[PLACEHOLDER_BODIES.len() - 1].model.appearance_ref;
        let make = |id: u64, x: f32, appearance: Option<&'static str>, tint: [u8; 3]| {
            let mut p = player(id, Pos::new(x, 0.0, 0.0), 0.0, AnimState::Idle);
            p.appearance = appearance;
            p.tint = tint;
            p
        };
        let npc = {
            let mut p = make(6, 120.0, Some(body_a), [90, 90, 250]);
            p.npc = Some(NpcRef { model: "agyb", idle: Some("acfe") });
            p
        };
        let players = [
            make(1, -120.0, Some(body_a), [200, 40, 40]),
            make(2, -60.0, Some(body_b), [40, 40, 200]),
            make(3, 0.0, Some("ff7/barret"), [10, 250, 10]),
            make(4, 60.0, None, [250, 250, 10]),
            npc,
        ];
        let built = avatars.frame(&view, &stage, &players, 0.0).frame;
        assert_eq!(built.models.len(), players.len());
        assert_eq!(built.identities.len(), built.models.len(), "one identity per model, in the models' order");
        assert!(built.tints.is_empty(), "the field's colours are identities: nothing is scaled or faded");
        // Which model is whose: the frame lists the players nearest first, so find each by what it is.
        let by_asset = |name: &str| built.models.iter().position(|m| m.asset.appearance.appearance_ref == name);
        let all: Vec<(&str, Identity)> = built.models.iter().zip(&built.identities).map(|(m, i)| (m.asset.appearance.appearance_ref, *i)).collect();
        // The canon model, the stand-in (Cloud) and the non-player character are untouched.
        for plain in ["ff7/barret", "ff7/cloud", "npc"] {
            let i = by_asset(plain).unwrap_or_else(|| panic!("{plain} was drawn: {all:?}"));
            assert_eq!(built.identities[i], Identity::NONE, "{plain} keeps its own colours");
        }
        // The two bodies carry their own players' colours at full strength: the colour a body player has, not
        // another's.
        let coloured: Vec<Identity> = built.identities.iter().copied().filter(|i| *i != Identity::NONE).collect();
        assert_eq!(coloured.len(), 2, "{all:?}");
        assert!(coloured.contains(&Identity::of([200, 40, 40])) && coloured.contains(&Identity::of([40, 40, 200])), "{all:?}");
        assert!(coloured.iter().all(|i| i.strength == 1.0));
        // The same body worn by two players carries the colour of each of them (one asset, two draws).
        let twins = [make(1, -60.0, Some(body_a), [200, 40, 40]), make(2, 60.0, Some(body_a), [40, 40, 200])];
        let built = avatars.frame(&view, &stage, &twins, 0.0).frame;
        assert_eq!(built.models.len(), 2);
        assert_eq!(built.models[0].asset.id, built.models[1].asset.id, "one asset on the GPU");
        assert_ne!(built.identities[0], built.identities[1], "two colours for it");
        // A player that is only a marker has no model and so no identity.
        let (mut broken, _) = avatars_failing(body_a);
        let marker = broken.frame(&view, &stage, &twins[..1], 0.0).frame;
        assert!(marker.identities.is_empty() && marker.models.is_empty());
    }

    fn avatars_failing(appearance: &'static str) -> (Avatars, Rc<RefCell<Vec<&'static str>>>) {
        avatars(vec![appearance])
    }

    #[test]
    fn skinning_applies_pose_placement_and_camera_in_that_order() {
        let asset = cloud_asset();
        let stage = stage();
        let pose = asset.character.pose(AnimState::Idle, 0).unwrap();
        let place = model_to_field([100.0, -40.0, 12.0], 0.7, 4.0);
        let mut out = Vec::new();
        assert!(skin(&asset, &pose, &place, &stage.camera.matrix(), &mut out));
        assert_eq!(out.len(), asset.mesh.positions.len());
        // Every vertex equals: bone matrix, then placement, then the camera.
        for run in &asset.mesh.bone_runs {
            let to_field = fmv_ff7::model::mat4_mul(&place, &pose[run.bone]);
            let range = run.start..run.start + run.len;
            for (offset, (local, skinned)) in asset.mesh.positions[range.clone()].iter().zip(&out[range]).enumerate() {
                let world = transform_point(&to_field, *local);
                let want = stage.camera.to_camera(world.map(f64::from));
                for (c, (got, want)) in skinned.iter().zip(want).enumerate() {
                    assert!((f64::from(*got) - want).abs() < 0.05, "vertex {} axis {c}: {got} vs {want}", run.start + offset);
                }
            }
        }
    }

    #[test]
    fn a_pose_that_lacks_a_bone_or_is_not_finite_is_refused() {
        let asset = cloud_asset();
        let stage = stage();
        let pose = asset.character.pose(AnimState::Idle, 0).unwrap();
        let place = mat4_identity();
        let mut out = Vec::new();
        assert!(!skin(&asset, &pose[..0], &place, &stage.camera.matrix(), &mut out), "a bone is missing");
        let mut nan = pose.clone();
        nan[0][12] = f32::NAN;
        assert!(!skin(&asset, &nan, &place, &stage.camera.matrix(), &mut out));
    }

    /// Where a camera-space point lands, in field-screen units (x right, y DOWN).
    fn screen(v: [f32; 3], zoom: f64) -> (f64, f64) {
        (f64::from(v[0]) * zoom / f64::from(v[2]), f64::from(v[1]) * zoom / f64::from(v[2]))
    }

    #[test]
    fn a_character_stands_on_its_position_upright_with_its_head_above_its_feet() {
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let at = Pos::new(0.0, 0.0, 0.0);
        let built = avatars.frame(&view, &stage, &[player(1, at, 0.0, AnimState::Idle)], 0.0);
        assert_eq!(built.frame.models.len(), 1);
        let m = &built.frame.models[0];
        // The model's lowest and highest vertices on the screen (y grows downwards there).
        let ys: Vec<f64> = m.positions.iter().map(|&v| screen(v, 400.0).1).collect();
        let (top, bottom) = (ys.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MIN, f64::max));
        // The feet sit at the entity's projected position (the origin projects to screen y 0)...
        assert!(bottom.abs() < 3.0, "feet at {bottom}, the position projects to 0");
        // ...and the head is 30 model units = 120 field units up: about 36 screen units here.
        assert!(top < -30.0 && top > -45.0, "head at {top}");
        // The whole figure is in front of the camera plane, between the depth mapping's planes (so
        // the buffer tells its parts apart).
        assert!(m.positions.iter().all(|v| v[2] > 1.0));
        assert!(m.positions.iter().all(|v| v[2] > crate::avatar::scenery::DEPTH_NEAR && v[2] < crate::avatar::scenery::DEPTH_FAR));
        // Moving the entity moves the figure with it, whole.
        let moved = avatars.frame(&view, &stage, &[player(1, Pos::new(200.0, 0.0, 0.0), 0.0, AnimState::Idle)], 0.0);
        let xs = |m: &SkinnedModel| m.positions.iter().map(|&v| screen(v, 400.0).0).sum::<f64>() / m.positions.len() as f64;
        let shift = xs(&moved.frame.models[0]) - xs(m);
        assert!((shift - 200.0 * 400.0 / 1200.0).abs() < 5.0, "200 units right is about 67 screen units: {shift}");
    }

    #[test]
    fn a_character_faces_the_direction_of_its_facing() {
        let asset = cloud_asset();
        let pose = asset.character.pose(AnimState::Idle, 0).unwrap();
        // The stick figure's nose is its vertices 3..6, three native units in front (-z) of it.
        let nose_in_field = |facing: f32| {
            let place = model_to_field([0.0, 0.0, 0.0], facing, 4.0);
            let m = fmv_ff7::model::mat4_mul(&place, &pose[0]);
            let sum = (3..6).map(|k| transform_point(&m, asset.mesh.positions[k])).fold([0.0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
            sum.map(|c| c / 3.0)
        };
        let near = |got: [f32; 3], x: f32, y: f32| (got[0] - x).abs() < 1e-3 && (got[1] - y).abs() < 1e-3;
        // Heading 0 is +x, a quarter turn is +y: the nose is 3 model units = 12 field units ahead.
        assert!(near(nose_in_field(0.0), 12.0, 0.0), "{:?}", nose_in_field(0.0));
        assert!(near(nose_in_field(std::f32::consts::FRAC_PI_2), 0.0, 12.0), "{:?}", nose_in_field(std::f32::consts::FRAC_PI_2));
        assert!(near(nose_in_field(std::f32::consts::PI), -12.0, 0.0), "{:?}", nose_in_field(std::f32::consts::PI));
        assert!(near(nose_in_field(-std::f32::consts::FRAC_PI_2), 0.0, -12.0), "{:?}", nose_in_field(-std::f32::consts::FRAC_PI_2));
        // And at head height (27 model units above the ground: the nose is at local y = -22 on a
        // root 5 units up), 108 field units at this scale.
        assert!((nose_in_field(0.3)[2] - 108.0).abs() < 1e-3);

        // Through the whole frame: a character facing the camera (heading -y... the camera looks
        // along +y, so facing it means heading -y) has its nose nearer to the camera than the
        // top of its head's centre line; facing away it is further.
        let (view, stage) = (view(), stage());
        let (mut avatars, _) = avatars(vec![]);
        let depth_of_nose = |avatars: &mut Avatars, facing: f32| {
            let built = avatars.frame(&view, &stage, &[player(1, Pos::new(0.0, 0.0, 0.0), facing, AnimState::Idle)], 0.0);
            let m = &built.frame.models[0];
            (3..6).map(|k| m.positions[k][2]).sum::<f32>() / 3.0
        };
        let toward = depth_of_nose(&mut avatars, -std::f32::consts::FRAC_PI_2);
        let away = depth_of_nose(&mut avatars, std::f32::consts::FRAC_PI_2);
        assert!(toward < away - 10.0, "facing the camera puts the nose nearer: {toward} vs {away}");
    }

    #[test]
    fn the_frame_lists_markers_for_players_without_a_character_and_skips_the_undrawable() {
        let (mut avatars, asked) = avatars(vec!["ff7/barret"]);
        let (view, stage) = (view(), stage());
        let mut barret = player(2, Pos::new(40.0, 10.0, 0.0), 1.0, AnimState::Walk);
        barret.appearance = Some("ff7/barret");
        let players = [
            player(1, Pos::new(0.0, 0.0, 0.0), 0.0, AnimState::Idle),
            barret,
            player(3, Pos::new(f32::NAN, 0.0, 0.0), 0.0, AnimState::Idle),
            player(4, Pos::new(0.0, -20_000.0, 0.0), 0.0, AnimState::Idle), // far behind the camera
            player(5, Pos::new(-40.0, 0.0, 0.0), f32::INFINITY, AnimState::Idle),
        ];
        let built = avatars.frame(&view, &stage, &players, 0.016);
        assert_eq!(built.frame.models.len(), 1, "only the first can be a model");
        assert_eq!(built.markers, [1], "Barret's character failed to load: a marker, not nothing");
        assert!(built.anchors.contains_key(&Who(1)) && !built.anchors.contains_key(&Who(2)));
        assert_eq!(*asked.borrow(), ["ff7/cloud", "ff7/barret"]);
        // Asking again does not load again.
        avatars.frame(&view, &stage, &players, 0.016);
        assert_eq!(asked.borrow().len(), 2);
    }

    #[test]
    fn the_name_label_anchor_is_above_the_head_not_at_the_feet() {
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let at = Pos::new(0.0, 0.0, 0.0);
        let built = avatars.frame(&view, &stage, &[player(9, at, 0.0, AnimState::Idle)], 0.0);
        let anchor = built.anchors[&Who(9)];
        let foot = view.to_view(at).unwrap();
        // View space has y up: the anchor is above the feet by about the model's height in pixels
        // of the field screen (a model unit is 4 field units).
        let height = built.frame.models[0].asset.height_units * stage.model_scale;
        let expected_top = view.to_view(Pos::new(0.0, 0.0, height)).unwrap();
        assert!(anchor.y > foot.y + 10.0, "anchor {anchor:?} vs feet {foot:?}");
        assert!((anchor.y - expected_top.y).abs() < 1e-3 && (anchor.x - expected_top.x).abs() < 1e-3);
    }

    #[test]
    fn animation_follows_the_state_and_time() {
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let at = Pos::new(0.0, 0.0, 0.0);
        let mut walk = player(1, at, 0.0, AnimState::Walk);
        let a = avatars.frame(&view, &stage, &[walk], 0.0);
        let b = avatars.frame(&view, &stage, &[walk], 0.1);
        // (The synthetic clips have all-zero angles, so poses do not differ; what is checked here
        // is that the same player keeps its animation track across frames and that a state change
        // does not lose or duplicate the player.)
        assert_eq!((a.frame.models.len(), b.frame.models.len()), (1, 1));
        walk.anim = AnimState::Run;
        assert_eq!(avatars.frame(&view, &stage, &[walk], 0.1).frame.models.len(), 1);
        walk.anim = AnimState::Idle;
        assert_eq!(avatars.frame(&view, &stage, &[walk], 0.1).frame.models.len(), 1);
    }

    #[test]
    fn ground_marks_show_the_tint_the_own_outline_and_the_party_ring() {
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let at = Pos::new(0.0, 0.0, 0.0);
        let mut remote = player(1, at, 0.0, AnimState::Idle);
        let plain = avatars.frame(&view, &stage, &[remote], 0.0).frame.ground;
        // Shadow + tint ring.
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1].rgb, remote.tint);
        // Every mark carries the camera depth of the feet it lies around (the depth test's value:
        // a tile nearer than the feet hides the marks, as it hides the character).
        let feet = stage.camera.project([0.0, 0.0, 0.0]).unwrap()[2] as f32;
        assert!(feet > 1000.0 && plain.iter().all(|m| (m.depth - feet).abs() < 1e-3), "{feet} {:?}", plain.iter().map(|m| m.depth).collect::<Vec<_>>());
        // (and a nearer player's marks are nearer)
        let nearer = avatars.frame(&view, &stage, &[player(2, Pos::new(0.0, -200.0, 0.0), 0.0, AnimState::Idle)], 0.0).frame.ground;
        assert!(nearer.iter().all(|m| m.depth < feet - 100.0));
        remote.party = true;
        let party = avatars.frame(&view, &stage, &[remote], 0.0).frame.ground;
        assert_eq!(party.len(), 3);
        assert_eq!(party[2].rgb, GOLD);
        remote.own = true;
        let own = avatars.frame(&view, &stage, &[remote], 0.0).frame.ground;
        assert_eq!(own.len(), 4);
        assert_eq!(own[2].rgb, WHITE, "the own outline hugs the tint ring, the party ring is outside it");
        // A stand-in (unknown appearance) shows its tint at full strength.
        let mut unknown = player(2, at, 0.0, AnimState::Idle);
        unknown.appearance = Some("ff7/nobody");
        let stand_in = avatars.frame(&view, &stage, &[unknown], 0.0).frame.ground;
        assert!(stand_in[1].alpha > plain[1].alpha);
        // The marks lie around the entity's projected position.
        let centre = view.to_view(at).unwrap();
        let (cx, cy) = stand_in[1].outer.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
        let n = stand_in[1].outer.len() as f32;
        assert!(((cx / n) - centre.x).abs() < 3.0 && ((cy / n) - centre.y).abs() < 3.0);
    }

    #[test]
    fn a_crowd_is_capped_at_the_nearest_models_and_the_rest_get_markers() {
        let (mut avatars, _) = avatars(vec![]);
        let (view, stage) = (view(), stage());
        let players: Vec<AvatarPlayer<'static>> =
            (0..MAX_MODELS + 6).map(|i| player(i as u64, Pos::new(i as f32 * 3.0 - 90.0, i as f32 * 10.0, 0.0), 0.0, AnimState::Idle)).collect();
        let built = avatars.frame(&view, &stage, &players, 0.0);
        assert_eq!(built.frame.models.len(), MAX_MODELS);
        assert_eq!(built.markers.len(), 6);
        // The nearest to the camera (smallest y: the camera looks along +y) are the models.
        assert!(built.markers.iter().all(|&i| i >= MAX_MODELS), "{:?}", built.markers);
        assert_eq!(avatars.cached(), 1);
    }

    /// A non-player character standing at `pos`, with the model `agyb` and the clip `acfe`.
    fn npc(id: u64, pos: Pos, facing: f32) -> AvatarPlayer<'static> {
        AvatarPlayer {
            who: Who(id),
            pos,
            facing,
            anim: AnimState::Idle,
            appearance: None,
            npc: Some(NpcRef { model: "agyb", idle: Some("acfe") }),
            tint: [120, 130, 140],
            own: false,
            party: false,
        }
    }

    /// Avatars whose non-player loads are counted (as `model/clip`) and fail for the skeletons
    /// `fail_npc`.
    fn npc_avatars(fail_npc: Vec<&'static str>) -> (Avatars, Rc<RefCell<Vec<String>>>) {
        let mut loader = Counting::new(Rc::default(), vec![]);
        loader.fail_npc = fail_npc;
        let asked = Rc::clone(&loader.asked_npc);
        (Avatars::new(AvatarLibrary::new(Box::new(loader))), asked)
    }

    #[test]
    fn a_non_player_character_stands_where_its_data_puts_it_facing_its_way_with_only_a_shadow() {
        let (mut avatars, asked) = npc_avatars(vec![]);
        let (view, stage) = (view(), stage());
        let at = Pos::new(0.0, 0.0, 0.0);
        let id = 1 << 63;
        let built = avatars.frame(&view, &stage, &[npc(id, at, 0.0)], 0.0);
        assert_eq!((built.frame.models.len(), built.markers.len()), (1, 0), "a model, not a marker");
        assert_eq!(*asked.borrow(), ["agyb/acfe"]);
        assert_eq!(avatars.cached(), 0, "no playable character was needed: no stand-in Cloud");
        // Feet at its position, head above, like a player.
        let ys: Vec<f64> = built.frame.models[0].positions.iter().map(|&v| screen(v, 400.0).1).collect();
        let (top, bottom) = (ys.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MIN, f64::max));
        assert!(bottom.abs() < 3.0 && top < -30.0, "feet {bottom}, head {top}");
        // A shadow, and no ring of any kind (a non-player character is scenery).
        assert_eq!(built.frame.ground.len(), 1);
        assert_eq!(built.frame.ground[0].rgb, [0, 0, 0]);
        // Its label goes over its head.
        let anchor = built.anchors[&Who(id)];
        assert!(anchor.y > view.to_view(at).unwrap().y + 10.0, "{anchor:?}");
        // It faces its heading: toward the camera puts the nose (vertices 3..6 of the stick
        // figure) nearer than away from it.
        let mut nose = |facing: f32| {
            let m = &avatars.frame(&view, &stage, &[npc(id, at, facing)], 0.0).frame.models[0];
            (3..6).map(|k| m.positions[k][2]).sum::<f32>() / 3.0
        };
        let half_turn = std::f32::consts::FRAC_PI_2;
        assert!(nose(-half_turn) < nose(half_turn) - 10.0, "facing the camera puts the nose nearer");
        // Standing still: asking again loads nothing.
        assert_eq!(asked.borrow().len(), 1);
    }

    #[test]
    fn a_non_player_character_without_a_model_here_is_a_marker_and_never_a_stand_in() {
        let (mut avatars, asked) = npc_avatars(vec!["failing"]);
        let (view, stage) = (view(), stage());
        let mut failing = npc(11, Pos::new(0.0, 0.0, 0.0), 0.0);
        failing.npc = Some(NpcRef { model: "failing", idle: Some("acfe") });
        let mut unnamed = npc(12, Pos::new(30.0, 0.0, 0.0), 0.0);
        unnamed.npc = Some(NpcRef { model: "", idle: None });
        let mut standing_nowhere = npc(13, Pos::new(60.0, 0.0, 0.0), 0.0);
        standing_nowhere.npc = Some(NpcRef { model: "agyb", idle: None });
        let mut hostile = npc(14, Pos::new(90.0, 0.0, 0.0), 0.0);
        hostile.npc = Some(NpcRef { model: "../../char", idle: Some("acfe") });
        let built = avatars.frame(&view, &stage, &[failing, unnamed, standing_nowhere, hostile], 0.0);
        assert!(built.frame.models.is_empty(), "nothing is drawn as a model");
        assert_eq!(built.markers, [0, 1, 2, 3], "every one of them is a marker instead");
        assert!(built.frame.ground.is_empty() && built.anchors.is_empty());
        assert_eq!(avatars.cached(), 0, "the default character does not stand in for a non-player one");
        assert_eq!(*asked.borrow(), ["failing/acfe"], "only the one that could name an entry was asked for, once");
        avatars.frame(&view, &stage, &[failing], 0.0);
        assert_eq!(asked.borrow().len(), 1, "a failure is not retried");
    }

    #[test]
    fn a_crowd_of_non_player_characters_never_takes_a_players_model() {
        let (mut avatars, _) = npc_avatars(vec![]);
        let (view, stage) = (view(), stage());
        // The non-player characters are all NEARER to the camera than the three players.
        let mut things: Vec<AvatarPlayer<'static>> =
            (0..MAX_MODELS + 10).map(|i| npc((1 << 63) + i as u64, Pos::new(i as f32 * 2.0 - 60.0, -300.0 + i as f32, 0.0), 0.0)).collect();
        for i in 0..3 {
            let mut p = player(i, Pos::new(i as f32 * 20.0, 500.0, 0.0), 0.0, AnimState::Idle);
            p.appearance = Some("ff7/cloud");
            things.push(p);
        }
        let n = things.len();
        let built = avatars.frame(&view, &stage, &things, 0.0);
        assert_eq!(built.frame.models.len(), MAX_MODELS);
        // The players are the last three in the list and all have models; the markers are the
        // non-player characters that did not fit.
        assert!((n - 3..n).all(|i| !built.markers.contains(&i)), "{:?}", built.markers);
        assert_eq!(built.markers.len(), 13);
        assert!(built.markers.iter().all(|&i| i < MAX_MODELS + 10));
        assert!((0..3).all(|i| built.anchors.contains_key(&Who(i))));
    }

    #[test]
    fn setting_the_npcs_loads_their_models_ahead_of_the_first_frame() {
        let (mut avatars, asked) = npc_avatars(vec![]);
        avatars.set_npcs(&[NpcRef { model: "agyb", idle: Some("acfe") }, NpcRef { model: "agyb", idle: Some("acfe") }, NpcRef { model: "", idle: None }]);
        assert_eq!(*asked.borrow(), ["agyb/acfe"]);
        let built = avatars.frame(&view(), &stage(), &[npc(1 << 63, Pos::new(0.0, 0.0, 0.0), 0.0)], 0.0);
        assert_eq!(built.frame.models.len(), 1);
        assert_eq!(asked.borrow().len(), 1, "the frame found it ready");
        avatars.set_npcs(&[]);
        assert_eq!(avatars.cached_npcs(), 0, "an area without characters holds none");
    }

    #[test]
    fn an_empty_frame_draws_nothing() {
        let (mut avatars, _) = avatars(vec![]);
        let built = avatars.frame(&view(), &stage(), &[], 0.0);
        assert!(built.frame.is_empty() && built.markers.is_empty() && built.anchors.is_empty());
        assert!(format!("{:?}", built.frame).contains("models"));
        let _ = built.frame.ground_vertices(&Camera::fit(&Bounds { min: Pos::default(), max: Pos::new(1.0, 1.0, 0.0) }, 10.0, 10.0, 0.0), 10, 10, false);
    }
}
