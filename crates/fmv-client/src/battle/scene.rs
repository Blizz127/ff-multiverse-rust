//! The battle as a 3D scene: a camera, a location, the enemies where the formation puts them, the
//! party facing them, everyone breathing, and what happens in the fight played as short reactions.
//!
//! It replaces the flat tokens of [`super::view`] as the window's battle picture. It draws no text
//! and no windows: the battle overlay ([`super::draw_battle_overlay`]) draws over it through the same
//! overlay pass, told where the fighters' heads are ([`BattleScene::head_anchors`]) for its pointer and
//! its floating numbers. The flat view remains the fallback ([`scene_for`] says when).
//!
//! # The pieces
//!
//! * [`camera`]: the formation's own camera (a position and a target in the game's battle units),
//!   or a default view of the same kind when it has none or it does not suit the cast, and the
//!   lens that fits everyone in the window at any aspect ratio.
//! * [`layout`]: who stands where: the enemies from the formation, the party opposite them.
//! * [`model`]: where each actor's model comes from ([`BattleActorModel`]: a field model, a real
//!   battle model, or a box) and who decides ([`BattleModelSource`]): the real models of the
//!   machine's own `battle.lgp` when the client found one ([`model::ArchiveModels`]), stand-ins otherwise.
//! * [`motion`]: what the actors do with time: lunges, flinches, dodges, deaths.
//! * [`stage`]: the stand-in location: sky, haze, a round tiled floor. A real location model, when
//!   the [`BattleModelSource`] has one, replaces its floor (and is cut to the camera first, see
//!   `clip`).
//!
//! The models go through the avatar renderer ([`crate::avatar`]): skinned on the CPU into camera
//! space with the same pose and skinning code the field's players use, drawn depth-tested by
//! `AvatarGpu::draw_models`; the stage is flat-coloured triangles under them. See
//! [`crate::render::Gpu::draw_battle`] for the frame.
//!
//! # Data flow
//!
//! ```text
//! BattleClient::scene_start()  --> BattleScene::new   (formation, party, enemies, models)
//! BattleClient::take_events()  --> BattleScene::update (events become reactions; the engine's
//! BattleClient::engine()                                state keeps who is alive and ready true)
//! per frame:   view(w, h) -> frame(view, srgb) -> BattleFrame  (GPU)
//!                          \-> head_anchors(view) -> Anchors     (the battle overlay)
//! ```
//!
//! Actors are keyed by the engine's [`ActorId`] (party members first, then the enemies, in the
//! order of `BattleState::party_ids` and `enemy_ids`), never by a position in a list of cards.
//!
//! Nothing here reads game files itself: the formation's placement comes through
//! `BattleRules::formation_scene` from this machine's own `scene.bin`, and the models through
//! the [`ModelKit`]'s loader from this machine's own archives. Nothing is sent anywhere.

pub mod camera;
mod clip;
pub mod layout;
pub mod model;
pub mod motion;
#[cfg(test)]
mod shots;
pub mod stage;

pub use model::ModelKit;

use super::view::{Anchors, BattleView, Status};
use crate::avatar::frame::{skin, AvatarFrame, SkinnedModel, OVERLAY_BIAS_MODEL_UNITS};
use crate::avatar::ground::{circle, GroundShape};
use crate::avatar::library::AvatarAsset;
use crate::render::{Camera, Vertex};
use crate::state::ClientState;
use camera::{fit, BattleCamera, View, V3, NOMINAL_TAN_HALF_V};
use fmv_core::Pos;
use fmv_ff7::model::{mat4_mul, mat4_translate, transform_point};
use fmv_ff7_battle::rules::{ActorId, BattleEvent, BattleState, Combatant, Side};
use fmv_protocol::battle::FormationScene;
use fmv_protocol::{BattleId, BattleMember};
use layout::Spot;
use model::{ActorRig, BoxSpec, EnemyLook, PartyLook};
use motion::{Anchor, Director};
use stage::Arena;
use std::cell::OnceCell;
use std::sync::{Arc, OnceLock};

/// The view used when the formation has no camera, or its camera does not suit the cast: the shape
/// of the game's usual battle camera (raised, off to the side, looking at the middle of the field),
/// in round numbers of our own.
const DEFAULT_POSE: (V3, V3) = ([12_000.0, -3_600.0, 7_000.0], [0.0, -300.0, -200.0]);
/// Where the camera of last resort stands relative to the middle of the cast.
const AUTO_OFFSET: V3 = [12_000.0, -3_600.0, 4_500.0];
/// Seconds of the idle loop between one actor and the next (a real model plays its own loop; two
/// of one kind would otherwise move as one).
const IDLE_PHASE_STEP: f32 = 0.37;
/// How much bigger than its model the shadow under an actor is.
const SHADOW_RATIO: f32 = 1.15;
/// The rings under a party member, as multiples of its model's radius (inner, outer). The ground is
/// seen at a slant, so a ring has to be wide to be thick on the screen: ours is white, the gold one
/// of a member waiting for a command lies outside it (or, for the others, where ours would be).
const RING_MINE: (f32, f32) = (1.18, 1.42);
const RING_READY_OUTSIDE_MINE: (f32, f32) = (1.46, 1.90);
const RING_READY: (f32, f32) = (1.18, 1.62);

/// What the scene needs to start drawing the battle the client is in (see
/// [`super::BattleClient::scene_start`]).
#[derive(Debug, Clone)]
pub struct SceneStart {
    pub id: BattleId,
    /// The party, `party[i]` fighting as `ActorId(i)`.
    pub party: Vec<BattleMember>,
    /// Our own character.
    pub my_actor: Option<ActorId>,
    /// Where the enemies stand and the camera, from this machine's own rules (`None` for rules
    /// without placement data: a default layout and view are used).
    pub formation: Option<FormationScene>,
}

/// The nearest a location's triangles are drawn to the camera, in battle units: nothing nearer is
/// kept (see [`clip`]).
const LOCATION_NEAR: f64 = 300.0;

/// The location's model, and for each camera the scene may use, the model cut to that camera's near
/// plane and skinned into its camera space (made when that camera is first drawn with).
struct Location {
    rig: Arc<ActorRig>,
    cuts: Vec<OnceCell<Option<LocationCut>>>,
}

/// A location as one camera sees it: nothing of it behind the camera, in camera space already.
struct LocationCut {
    asset: Arc<AvatarAsset>,
    positions: Vec<[f32; 3]>,
}

impl LocationCut {
    /// `rig` cut to `camera`'s near plane. A model that is not one bone (the contract says a
    /// location is one identity bone) is drawn as it is, uncut.
    fn make(rig: &ActorRig, camera: &BattleCamera) -> Option<Self> {
        let camera_matrix = camera.matrix();
        let identity = mat4_translate([0.0; 3]);
        let pose = rig.idle_pose(0.0)?;
        let model = &rig.asset.character.model;
        let cut_asset = (model.bones.len() == 1)
            .then(|| {
                // The one bone's pose is baked into the vertices, which are then cut in battle space.
                let bone = pose.first()?;
                let parts: Vec<_> = model
                    .parts
                    .iter()
                    .filter_map(|part| {
                        let mut baked = part.clone();
                        baked.positions = part.positions.iter().map(|&p| transform_point(bone, p)).collect();
                        baked.bone_index = 0;
                        clip::clip_part(&baked, camera.position(), camera.forward(), LOCATION_NEAR)
                    })
                    .collect();
                let still = model::still_clip(1);
                let character = fmv_ff7::playable::Character {
                    appearance: crate::avatar::source::NON_PLAYABLE,
                    model: fmv_ff7::FieldModel { parts, ..model.clone() },
                    idle: still.clone(),
                    walk: still.clone(),
                    run: still,
                };
                AvatarAsset::new(character).ok().map(Arc::new)
            })
            .flatten();
        let (asset, pose) = match cut_asset {
            Some(asset) => {
                let pose = asset.character.pose(fmv_core::AnimState::Idle, 0)?;
                (asset, pose)
            }
            None => (Arc::clone(&rig.asset), pose),
        };
        let mut positions = Vec::new();
        skin(&asset, &pose, &identity, &camera_matrix, &mut positions).then_some(Self { asset, positions })
    }
}

/// One combatant of the scene.
struct Actor {
    side: Side,
    rig: Arc<ActorRig>,
    /// Where it stands and which way it faces when it is doing nothing.
    home: Spot,
}

/// Where an actor is on the screen, in pixels from the top left (tests).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorScreen {
    pub actor: ActorId,
    pub side: Side,
    /// Under its feet.
    pub feet: Option<[f32; 2]>,
    /// Over its head.
    pub head: Option<[f32; 2]>,
    /// The model is drawn this frame (not collapsed away, not in the dark half of a blink).
    pub drawn: bool,
}

/// One frame of the battle for the GPU ([`crate::render::Gpu::draw_battle`]).
pub struct BattleFrame {
    /// The window's background colour before the stage is drawn.
    pub clear_rgb: [u8; 3],
    /// The stage, flat-coloured triangles in NDC, painter's order.
    pub stage: Vec<Vertex>,
    /// The location's model in camera space, when there is one: drawn over the stage and under
    /// everyone else (in a pass of its own, so the marks under the actors are not covered by its floor).
    pub location: Option<AvatarFrame>,
    /// The models in camera space and the marks on the ground, for the avatar renderer.
    pub models: AvatarFrame,
    /// The window camera the models and marks are placed with ([`View::gpu_camera`]).
    pub camera: Camera,
}

/// The battle as a 3D scene (see the module docs).
pub struct BattleScene {
    id: BattleId,
    actors: Vec<Actor>,
    /// The cameras to try, in order: the formation's own, the default, the one of last resort.
    cameras: Vec<BattleCamera>,
    /// Points that must all be in the window: feet, heads and sides of everyone.
    cast: Vec<V3>,
    arena: Arena,
    /// The location's model, drawn behind everyone in place of the stand-in floor (`None`: the stand-in).
    location: Option<Location>,
    director: Director,
    my_actor: Option<ActorId>,
    /// The party members waiting for a command.
    ready: Vec<bool>,
    /// The scene clock: the time of the last [`BattleScene::update`], seconds.
    now: f64,
}

impl BattleScene {
    /// The scene of the battle `start` describes and `engine` (the replayed engine) has built.
    /// `looks[i]` is the look the player of party member `i` has chosen, if known. `None` without
    /// an engine (nothing is known of who fights): the flat view is used then.
    pub fn new(start: &SceneStart, engine: Option<&BattleState>, looks: &[Option<String>], kit: &mut ModelKit) -> Option<Self> {
        let engine = engine?;
        let (party_ids, enemy_ids) = (engine.party_ids(), engine.enemy_ids());
        let spots = layout::arrange(start.formation.as_ref(), enemy_ids.len(), party_ids.len());

        let mut actors: Vec<Actor> = Vec::with_capacity(party_ids.len() + enemy_ids.len());
        for (i, &id) in party_ids.iter().enumerate() {
            let who = PartyLook {
                actor: id,
                character: start.party.get(i).map_or(0, |m| m.character),
                look: looks.get(i).cloned().flatten(),
                mine: Some(id) == start.my_actor,
            };
            let model = kit.source.party_member(&who);
            let rig = kit.rigs.resolve(&model, BoxSpec::for_party(i));
            actors.push(Actor { side: Side::Party, rig, home: spots.party[i] });
        }
        for (j, &id) in enemy_ids.iter().enumerate() {
            let combatant = engine.actor(id)?;
            let who = EnemyLook {
                actor: id,
                enemy_id: start.formation.as_ref().and_then(|f| f.enemies.get(j)).map(|e| e.enemy_id),
                name: combatant.name.clone(),
                max_hp: combatant.max_hp,
                level: combatant.level,
            };
            let model = kit.source.enemy(&who);
            let rig = kit.rigs.resolve(&model, BoxSpec::for_enemy(0, combatant.max_hp));
            actors.push(Actor { side: Side::Enemy, rig, home: spots.enemies[j] });
        }

        let mut cast: Vec<V3> = Vec::with_capacity(actors.len() * 8);
        for a in &actors {
            let (x, y, z) = (f64::from(a.home.pos[0]), f64::from(a.home.pos[1]), f64::from(a.home.pos[2]));
            let (r, h) = (f64::from(a.rig.radius), f64::from(a.rig.height));
            for (dx, dz) in [(-r, -r), (r, -r), (r, r), (-r, r)] {
                cast.push([x + dx, y, z + dz]);
                cast.push([x + dx, y - h, z + dz]);
            }
        }
        let arena = Arena::around(actors.iter().map(|a| a.home.ground()));
        let centroid = if cast.is_empty() {
            [0.0; 3]
        } else {
            let n = cast.len() as f64;
            [0, 1, 2].map(|k| cast.iter().map(|p| p[k]).sum::<f64>() / n)
        };
        let from_formation = start.formation.as_ref().and_then(|f| {
            let (p, t) = (f.camera.position.map(f64::from), f.camera.target.map(f64::from));
            BattleCamera::look_at(p, t)
        });
        let auto = BattleCamera::look_at(
            [centroid[0] + AUTO_OFFSET[0], centroid[1] + AUTO_OFFSET[1], centroid[2] + AUTO_OFFSET[2]],
            [centroid[0], centroid[1] - 300.0, centroid[2]],
        )?;
        let cameras: Vec<BattleCamera> = from_formation
            .into_iter()
            .chain(BattleCamera::look_at(DEFAULT_POSE.0, DEFAULT_POSE.1))
            .chain(std::iter::once(auto))
            .collect();

        let location = start
            .formation
            .as_ref()
            .and_then(|f| kit.source.location(f.location))
            .and_then(|model| kit.rigs.resolve_optional(&model))
            .map(|rig| Location { rig, cuts: cameras.iter().map(|_| OnceCell::new()).collect() });
        let sides: Vec<Side> = actors.iter().map(|a| a.side).collect();
        let anchors: Vec<Anchor> = actors.iter().map(|a| Anchor { ground: a.home.ground(), radius: a.rig.radius }).collect();
        let n = actors.len();
        Some(Self {
            id: start.id,
            actors,
            cameras,
            cast,
            arena,
            location,
            director: Director::new(sides, anchors),
            my_actor: start.my_actor,
            ready: vec![false; n],
            now: 0.0,
        })
    }

    pub fn id(&self) -> BattleId {
        self.id
    }

    /// Actors in the scene (party first, then enemies).
    #[cfg(test)]
    pub fn actor_count(&self) -> usize {
        self.actors.len()
    }

    /// Where everyone is on the screen of a window with `view` (tests).
    #[cfg(test)]
    pub fn screen_anchors(&self, view: &View) -> Vec<ActorScreen> {
        (0..self.actors.len())
            .filter_map(|i| {
                let (pos, _, motion, _) = self.pose_of(i)?;
                let up = f64::from(self.actors[i].rig.height * motion.squash);
                Some(ActorScreen {
                    actor: ActorId(i as u8),
                    side: self.actors[i].side,
                    feet: view.to_pixels(pos),
                    head: view.to_pixels([pos[0], pos[1] - up, pos[2]]),
                    drawn: !motion.gone && motion.visible,
                })
            })
            .collect()
    }

    /// Advance the scene to `now` (seconds, the session clock): `events` are what the server's
    /// messages did since the last call (they start reactions), `engine` keeps who is alive and who
    /// is waiting for a command true (`None` after the battle has ended: what is playing plays out).
    pub fn update(&mut self, now: f64, events: &[BattleEvent], engine: Option<&BattleState>) {
        if now.is_finite() {
            self.now = now;
        }
        for event in events {
            if let Some(event) = motion::scene_event(event) {
                self.director.on_event(self.now, event);
            }
        }
        match engine {
            Some(engine) => {
                let actors = engine.actors();
                self.director.sync(self.now, |i| actors.get(i).map(Combatant::is_alive));
                for (i, ready) in self.ready.iter_mut().enumerate() {
                    *ready = self.actors[i].side == Side::Party && actors.get(i).is_some_and(|c| c.is_alive() && c.ready);
                }
            }
            None => self.ready.iter_mut().for_each(|r| *r = false),
        }
        self.director.prune(self.now);
    }

    /// The camera and lens for a `width` x `height` window: the formation's own camera when it
    /// shows the whole cast, else the default one, else one that stands off to a side of the cast.
    pub fn view(&self, width: u32, height: u32) -> View {
        for camera in &self.cameras {
            if let Some(view) = fit(*camera, width, height, &self.cast) {
                return view;
            }
        }
        // Even the camera of last resort has part of the cast too close: draw through it anyway.
        let camera = *self.cameras.last().expect("a camera of last resort");
        View { camera, width, height, zoom: f64::from(height.max(1)) * 0.5 / NOMINAL_TAN_HALF_V, shift: [0.0, 0.0] }
    }

    /// Where actor `i` stands now and its motion and idle sway at the scene clock.
    fn pose_of(&self, i: usize) -> Option<(V3, [f32; 16], motion::Motion, motion::Idle)> {
        let actor = self.actors.get(i)?;
        let motion = self.director.motion(i, self.now);
        let idle = if actor.rig.procedural_idle { motion::idle(i, self.now) } else { motion::Idle { breath: 1.0, sway: 0.0, bob: 0.0 } };
        let lift = actor.rig.radius * 0.45 * motion.fall;
        let pos = [
            actor.home.pos[0] + motion.offset[0],
            actor.home.pos[1] + motion.offset[1] - idle.bob - lift,
            actor.home.pos[2] + motion.offset[2],
        ];
        let height = actor.rig.scale * motion.squash * idle.breath;
        let place = placement(pos, actor.home.yaw + idle.sway, [actor.rig.scale, height, actor.rig.scale], motion.fall);
        Some((pos.map(f64::from), place, motion, idle))
    }

    /// One frame for a window with `view`, for a target that is (`srgb`) or is not sRGB.
    pub fn frame(&self, view: &View, srgb: bool) -> BattleFrame {
        let camera_matrix = view.camera.matrix();
        let mut models: Vec<SkinnedModel> = Vec::new();
        let mut marks: Vec<(f64, Vec<GroundShape>)> = Vec::new();
        let to_view = |p: Pos| {
            // `Pos` carries a ground position `(x, z)` in its first two fields; see `ground::circle`.
            let c = view.camera.to_camera([f64::from(p.x), 0.0, f64::from(p.y)]);
            view.pinhole(c, camera::STAGE_NEAR * 0.5).map(|[u, v]| Pos::new(u, v, 0.0))
        };
        for (i, actor) in self.actors.iter().enumerate() {
            let Some((pos, place, motion, _)) = self.pose_of(i) else { continue };
            let id = ActorId(i as u8);
            if motion.gone {
                continue;
            }
            // The marks under it: its shadow (fainter the higher it is) and, for a party member
            // waiting for a command, a pulsing ring; ours has a white outline.
            let depth = view.camera.to_camera([pos[0], 0.0, pos[2]])[2];
            let centre = Pos::new(pos[0] as f32, pos[2] as f32, 0.0);
            let lifted = (-actor.home.pos[1] + (-motion.offset[1]).max(0.0)) / 2500.0;
            let mut shapes = Vec::new();
            if let Some(outline) = circle(&to_view, centre, actor.rig.radius * SHADOW_RATIO) {
                shapes.push(GroundShape::disc(outline, [0, 0, 0], 0.40 * (1.0 - lifted.clamp(0.0, 0.8)), depth as f32));
            }
            if actor.side == Side::Party && motion.fall < 0.5 {
                let mine = Some(id) == self.my_actor;
                let band = |(inner, outer): (f32, f32), rgb: [u8; 3], alpha: f32| {
                    let (outer, inner) = (circle(&to_view, centre, actor.rig.radius * outer)?, circle(&to_view, centre, actor.rig.radius * inner)?);
                    Some(GroundShape::band(outer, inner, rgb, alpha, depth as f32))
                };
                if mine {
                    shapes.extend(band(RING_MINE, [255, 255, 255], 0.90));
                }
                if self.ready.get(i).copied().unwrap_or(false) {
                    let pulse = 0.70 + 0.25 * (self.now * 6.0).sin() as f32;
                    shapes.extend(band(if mine { RING_READY_OUTSIDE_MINE } else { RING_READY }, [255, 214, 64], pulse));
                }
            }
            marks.push((depth, shapes));
            // The dark half of a blink: the shadow stays, the model is not drawn.
            if !motion.visible {
                continue;
            }
            // Each actor is at its own point of the idle loop, so that two of one kind (two MPs) do not
            // breathe in step.
            let phase = self.now as f32 + i as f32 * IDLE_PHASE_STEP;
            let Some(pose) = actor.rig.idle_pose(phase) else { continue };
            let mut positions = Vec::new();
            if skin(&actor.rig.asset, &pose, &place, &camera_matrix, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(&actor.rig.asset), positions });
                // A weapon moves with its owner: the same placement, the same frame.
                for (asset, pose) in actor.rig.attachment_poses(phase) {
                    let mut positions = Vec::new();
                    if skin(asset, &pose, &place, &camera_matrix, &mut positions) {
                        models.push(SkinnedModel { asset: Arc::clone(asset), positions });
                    }
                }
            }
        }
        // The far marks first, so that a near one is drawn over theirs.
        marks.sort_by(|a, b| b.0.total_cmp(&a.0));
        // The location, as it is (its vertices are in battle coordinates already), cut to this
        // camera's near plane and skinned once: the camera does not move.
        let location = self.location.as_ref().and_then(|location| {
            let k = self.cameras.iter().position(|c| *c == view.camera)?;
            let cut = location.cuts.get(k)?.get_or_init(|| LocationCut::make(&location.rig, &view.camera)).as_ref()?;
            Some(AvatarFrame {
                models: vec![SkinnedModel { asset: Arc::clone(&cut.asset), positions: cut.positions.clone() }],
                zoom: view.zoom as f32,
                overlay_bias: 0.0,
                ground: Vec::new(),
                scenery: None,
            })
        });
        BattleFrame {
            clear_rgb: stage::HORIZON,
            stage: stage::vertices(view, location.is_none().then_some(&self.arena), srgb),
            location,
            models: AvatarFrame {
                models,
                zoom: view.zoom as f32,
                overlay_bias: OVERLAY_BIAS_MODEL_UNITS * model::FIELD_TO_BATTLE_SCALE * 2.0,
                ground: marks.into_iter().flat_map(|(_, shapes)| shapes).collect(),
                scenery: None,
            },
            camera: view.gpu_camera(),
        }
    }

    /// The point over each fighter's head in window pixels from the top left (a little above the top
    /// of its model wherever the model is now: lunging, lying down, collapsing), for the battle
    /// overlay's pointer and numbers ([`super::draw_battle_overlay`]): the party's, then the
    /// enemies', each in the engine's order, which is the order of the battle view's bars. `None`:
    /// not on the screen (an enemy that has collapsed away, or one behind the camera).
    pub fn head_anchors(&self, view: &View) -> Anchors {
        let mut anchors = Anchors::default();
        for (i, actor) in self.actors.iter().enumerate() {
            let anchor = self.head_of(i, view);
            match actor.side {
                Side::Party => anchors.party.push(anchor),
                Side::Enemy => anchors.enemies.push(anchor),
            }
        }
        anchors
    }

    fn head_of(&self, i: usize, view: &View) -> Option<(f32, f32)> {
        let actor = self.actors.get(i)?;
        let (_, place, motion, _) = self.pose_of(i)?;
        if motion.gone {
            return None;
        }
        // The top of the model is `height / scale` above its feet in its own space, and its placement
        // takes it wherever the model is now (standing, lunging, lying down).
        let top = transform_point(&place, [0.0, -actor.rig.height / actor.rig.scale, 0.0]);
        let above = f64::from(actor.rig.height) * 0.10;
        let [x, y] = view.to_pixels([f64::from(top[0]), f64::from(top[1]) - above, f64::from(top[2])])?;
        Some((x, y))
    }

}

/// The matrix from a model's native space to battle space: scaled by `scale` (per axis), turned
/// over backwards by `fall` (0 upright, 1 lying on its back), facing `yaw` (see
/// [`layout::Spot::yaw`]) and standing at `pos`.
///
/// Native model space and battle space are both right-handed with `y` down, and a model faces its
/// native `-z` with its left at `+x`, so no axes need swapping (unlike the field's
/// `model_to_field`): facing direction `(sin yaw, 0, cos yaw)`, left `(-cos yaw, 0, sin yaw)`.
fn placement(pos: [f32; 3], yaw: f32, scale: [f32; 3], fall: f32) -> [f32; 16] {
    let (s, c) = yaw.sin_cos();
    let basis: [f32; 16] = [-c, 0.0, s, 0.0, 0.0, 1.0, 0.0, 0.0, -s, 0.0, -c, 0.0, 0.0, 0.0, 0.0, 1.0];
    // Falling backwards is a turn about the model's left-right axis by -90 degrees.
    let (fs, fc) = (-std::f32::consts::FRAC_PI_2 * fall.clamp(0.0, 1.0)).sin_cos();
    let tip: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, fc, fs, 0.0, 0.0, -fs, fc, 0.0, 0.0, 0.0, 0.0, 1.0];
    let size: [f32; 16] = [scale[0], 0.0, 0.0, 0.0, 0.0, scale[1], 0.0, 0.0, 0.0, 0.0, scale[2], 0.0, 0.0, 0.0, 0.0, 1.0];
    mat4_mul(&mat4_translate(pos), &mat4_mul(&basis, &mat4_mul(&tip, &size)))
}

/// The looks of the players of `party` as this client knows them (ours from our own snapshot, the
/// others' from the area's remote players), aligned with `party`.
fn party_looks(state: &ClientState, party: &[BattleMember]) -> Vec<Option<String>> {
    let remotes = state.remote_views(0.0);
    party
        .iter()
        .map(|m| {
            if m.entity == state.self_entity() {
                state.own_appearance().map(str::to_string)
            } else {
                remotes.iter().find(|r| r.id == m.entity).and_then(|r| r.appearance.clone())
            }
        })
        .collect()
}

/// Whether the flat view is forced (`FMV_BATTLE_FLAT` set): an escape hatch for a machine whose
/// GPU does not like the scene.
fn flat_forced() -> bool {
    static FLAT: OnceLock<bool> = OnceLock::new();
    *FLAT.get_or_init(|| std::env::var_os("FMV_BATTLE_FLAT").is_some_and(|v| !v.is_empty() && v != "0"))
}

/// The scene to draw for `view` this frame, brought up to `now` (`None`: draw the flat view).
///
/// A scene is made when a battle is first seen (and again when another battle replaces it) from
/// what the client knows of it; it stays while the window has a view of that battle, a lingering
/// view after the battle's end included (the last events are still taken and played). Without
/// the battle data to replay the battle ([`Status::Unverified`]) nothing is known of who fights,
/// so there is no scene; nor when the scene cannot be built.
pub fn scene_for<'a>(
    slot: &'a mut Option<BattleScene>,
    kit: &mut Option<ModelKit>,
    state: &ClientState,
    view: &BattleView,
    now: f64,
    events: &[BattleEvent],
) -> Option<&'a mut BattleScene> {
    if flat_forced() || view.status == Status::Unverified {
        *slot = None;
        return None;
    }
    if slot.as_ref().map(BattleScene::id) != Some(view.id) {
        *slot = None;
        let start = state.battle.scene_start().filter(|s| s.id == view.id)?;
        let looks = party_looks(state, &start.party);
        let kit = kit.get_or_insert_with(ModelKit::from_local_content);
        *slot = BattleScene::new(&start, state.battle.engine(), &looks, kit);
    }
    let scene = slot.as_mut()?;
    scene.update(now, events, state.battle.engine());
    Some(scene)
}

#[cfg(test)]
mod tests;
