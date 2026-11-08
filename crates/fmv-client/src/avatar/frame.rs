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
use super::ground::{self, GroundShape};
use super::library::{AvatarAsset, AvatarLibrary};
use super::stage::Stage;
use crate::backdrop::{Backdrop, FieldView};
use crate::render::{Camera, Vertex};
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
    /// The camera's focal length ([`super::stage::StageCamera::zoom`]).
    pub zoom: f32,
    /// Camera-space depth range of every skinned vertex, padded: the depth buffer covers exactly
    /// this, which keeps its resolution fine whatever the field's size.
    pub near: f32,
    pub far: f32,
    /// Depth bias of the face overlays, in camera-space units.
    pub overlay_bias: f32,
    /// Marks on the ground, under the models.
    pub ground: Vec<GroundShape>,
    /// Scenery drawn over the models (layers 2 and 4), if the field has some.
    pub front: Option<Arc<Backdrop>>,
}

impl std::fmt::Debug for AvatarFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarFrame")
            .field("models", &self.models.len())
            .field("depth", &(self.near, self.far))
            .field("ground", &self.ground.len())
            .field("front", &self.front.is_some())
            .finish()
    }
}

impl AvatarFrame {
    /// The ground marks as flat-colour vertices for a `vw` x `vh` window fitted by `cam`.
    pub fn ground_vertices(&self, cam: &Camera, vw: u32, vh: u32, srgb: bool) -> Vec<Vertex> {
        ground::vertices(&self.ground, cam, vw, vh, srgb)
    }

    /// Nothing to draw at all (no models, no front layer).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.front.is_none()
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

/// The ground marks of one player: a soft shadow, the tint ring, and for the local player a white
/// outline and for a party member a gold ring around it all.
fn ground_marks(view: &FieldView, player: &AvatarPlayer<'_>, stand_in: bool, scale: f32) -> Vec<GroundShape> {
    let to_view = |p: Pos| view.to_view(p);
    let circle = |k: f32| ground::circle(&to_view, player.pos, RING_RADIUS_UNITS * scale * k);
    let mut marks = Vec::new();
    if let Some(shadow) = circle(0.95) {
        marks.push(GroundShape::disc(shadow, [0, 0, 0], 0.30));
    }
    // A stand-in (the player's own look is unknown here) leans on the tint more: a stronger ring.
    let tint_alpha = if stand_in { 1.0 } else { 0.85 };
    if let (Some(outer), Some(inner)) = (circle(1.0), circle(0.78)) {
        marks.push(GroundShape::band(outer, inner, player.tint, tint_alpha));
    }
    if player.own {
        if let (Some(outer), Some(inner)) = (circle(1.15), circle(1.0)) {
            marks.push(GroundShape::band(outer, inner, WHITE, 1.0));
        }
    }
    if player.party {
        if let (Some(outer), Some(inner)) = (circle(1.42), circle(1.26)) {
            marks.push(GroundShape::band(outer, inner, GOLD, 1.0));
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

    /// Characters held by the cache.
    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.library.cached()
    }

    /// Build the frame for `players` standing in `view`'s field. A player that cannot be drawn at
    /// all (not finite, behind the camera) is simply left out, as it always was; one whose
    /// character is unavailable is listed in [`Built::markers`]. `dt` is the time since the last
    /// frame, in seconds (it moves the walk and run loops on).
    pub fn frame(&mut self, view: &FieldView, stage: &Stage, players: &[AvatarPlayer<'_>], dt: f32) -> Built {
        self.animator.begin_frame();
        let camera = stage.camera.matrix();
        let scale = stage.model_scale;

        // Which players can be drawn, nearest first (the cap keeps the nearest).
        let mut order: Vec<(usize, f64)> = players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.pos.is_finite() && p.facing.is_finite() && view.to_view(p.pos).is_some())
            .filter_map(|(i, p)| {
                let depth = stage.camera.project([f64::from(p.pos.x), f64::from(p.pos.y), f64::from(p.pos.z)])?[2];
                Some((i, depth))
            })
            .collect();
        order.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));

        let mut models: Vec<SkinnedModel> = Vec::new();
        let mut ground_marks_by_player: Vec<(usize, Vec<GroundShape>)> = Vec::new();
        let mut markers: Vec<usize> = Vec::new();
        let mut anchors = HashMap::new();
        let (mut near, mut far) = (f32::INFINITY, f32::NEG_INFINITY);
        for (rank, &(i, _)) in order.iter().enumerate() {
            let Some(p) = players.get(i) else { continue };
            if rank >= MAX_MODELS {
                markers.push(i);
                continue;
            }
            let look = self.library.get(p.appearance);
            let Some(asset) = look.asset else {
                markers.push(i);
                continue;
            };
            let clip = asset.character.animation(p.anim);
            let playback = self.animator.advance(p.who, p.anim, dt, clip.frame_count());
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
            for v in &positions {
                near = near.min(v[2]);
                far = far.max(v[2]);
            }
            let head = Pos::new(p.pos.x, p.pos.y, p.pos.z + asset.height_units * scale);
            if let Some(anchor) = view.to_view(head).or_else(|| view.to_view(p.pos)) {
                anchors.insert(p.who, anchor);
            }
            ground_marks_by_player.push((i, ground_marks(view, p, look.stand_in, scale)));
            models.push(SkinnedModel { asset, positions });
        }

        // Draw the marks of the far players first, so a near player's ring is on top of theirs.
        ground_marks_by_player.sort_by_key(|(i, _)| std::cmp::Reverse(order.iter().position(|(j, _)| j == i)));
        let ground: Vec<GroundShape> = ground_marks_by_player.into_iter().flat_map(|(_, marks)| marks).collect();

        let (near, far) = if near.is_finite() && far.is_finite() {
            let pad = ((far - near) * 0.1).max(1.0);
            ((near - pad).max(0.5), far + pad)
        } else {
            (1.0, 2.0)
        };
        markers.sort_unstable();
        Built {
            frame: AvatarFrame {
                models,
                zoom: stage.camera.zoom as f32,
                near,
                far,
                overlay_bias: OVERLAY_BIAS_MODEL_UNITS * scale,
                ground,
                front: stage.front.clone(),
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
            front: None,
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
        AvatarPlayer { who: Who(id), pos, facing, anim, appearance: Some("ff7/cloud"), tint: [10, 200, 30], own: false, party: false }
    }

    fn avatars(fail: Vec<&'static str>) -> (Avatars, Rc<RefCell<Vec<&'static str>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let loader = Counting { asked: Rc::clone(&asked), fail };
        (Avatars::new(AvatarLibrary::new(Box::new(loader))), asked)
    }

    fn cloud_asset() -> Arc<AvatarAsset> {
        let (mut a, _) = avatars(vec![]);
        a.library.get(Some("ff7/cloud")).asset.unwrap()
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
        // The whole figure is in front of the camera plane, inside the depth range of the frame.
        assert!(m.positions.iter().all(|v| v[2] > 1.0));
        assert!(built.frame.near < built.frame.far && built.frame.near >= 0.5);
        assert!(m.positions.iter().all(|v| v[2] >= built.frame.near && v[2] <= built.frame.far));
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

    #[test]
    fn an_empty_frame_has_a_sane_depth_range() {
        let (mut avatars, _) = avatars(vec![]);
        let built = avatars.frame(&view(), &stage(), &[], 0.0);
        assert!(built.frame.is_empty() && built.markers.is_empty() && built.anchors.is_empty());
        assert!(built.frame.near < built.frame.far);
        assert!(format!("{:?}", built.frame).contains("models"));
        let _ = built.frame.ground_vertices(&Camera::fit(&Bounds { min: Pos::default(), max: Pos::new(1.0, 1.0, 0.0) }, 10.0, 10.0, 0.0), 10, 10, false);
    }
}
