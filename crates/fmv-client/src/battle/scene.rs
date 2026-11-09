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
//! * [`cue`]: a batch of events as performances, and WHEN each begins and lands (the one table the overlay's
//!   numbers and the scene share, so that the numbers appear at the impact frame without the two
//!   talking; the first performance of a batch lands at most [`cue::MAX_IMPACT_DELAY`] after the events
//!   arrived, and a counter or any later one begins when the one before it is over).
//! * [`clips`]: which slot of a real battle model plays what (attack, cast, hurt, death, victory...),
//!   verified on contact sheets; a slot that is missing or not the slot's own clip is never played.
//! * [`plan`]: time-warps a performance's clips so that the impact frame lands on schedule: faster
//!   when they are long, the start skipped when even that is not enough, so nothing lags.
//! * [`body`]: what a fighter's statuses do to its body: a colour factor (Poison's purple pulse, a sleeper's dim blue,
//!   the grey of a stop), the droop of a sleeper and the held idle of Stop and Paralysis.
//! * [`motion`]: what the actors do with time: the clips that play, lunges, flinches, dodges, deaths
//!   (a party member's clip, an enemy's fade), the victory pose; the procedural motion for an actor
//!   without clips.
//! * [`effects`]: simple additive effects (a spell's burst coloured by its element, an item's
//!   sparkle, the sparks of a blow, the glow a caster gathers) made of four shared meshes.
//! * [`stage`]: the stand-in location: sky, haze, a round tiled floor. A real location model, when
//!   the [`BattleModelSource`] has one, replaces its floor (and is cut to the camera first, see
//!   `clip`).
//!
//! The models go through the avatar renderer ([`crate::avatar`]): skinned on the CPU into camera
//! space with the same pose and skinning code the field's players use, drawn depth-tested by
//! `AvatarGpu::draw_models_tinted` (a per-model tint: a beaten enemy's opacity, an effect's glow); the
//! stage is flat-coloured triangles under them. See [`crate::render::Gpu::draw_battle`] for the frame.
//!
//! # Presentation only
//!
//! The engine's state (the bars, the menu, who is alive) moves when a message arrives; the scene is
//! presentation and **nothing waits on it**. A performance's impact lands at most
//! [`cue::MAX_IMPACT_DELAY`] after the performance begins (the first one of a batch begins when the events
//! arrive, the next ones one after the other, see [`cue`]), however long its clips are, and an actor that
//! is given a new performance takes it over from its start.
//!
//! # Data flow
//!
//! ```text
//! BattleClient::scene_start()  --> BattleScene::new   (formation, party, enemies, models, their clips)
//! BattleClient::take_events()  --> BattleScene::update (events become performances; the engine's
//! BattleClient::engine()                                state says who is alive and ready, and what
//!                                                       an action looks like: melee, its element)
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

pub mod body;
pub mod camera;
mod clip;
pub mod clips;
pub mod cue;
pub mod effects;
pub mod layout;
pub mod model;
pub mod motion;
pub mod plan;
#[cfg(test)]
mod sheets;
#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod shots;
#[cfg(test)]
mod fx_shots;
pub mod stage;

pub use model::ModelKit;

use super::view::{Anchors, BattleView, Status};
use crate::avatar::frame::{skin, AvatarFrame, SkinnedModel, OVERLAY_BIAS_MODEL_UNITS};
use crate::avatar::gpu::Tint;
use crate::avatar::ground::{circle, GroundShape};
use crate::avatar::identity::Identity;
use crate::avatar::library::AvatarAsset;
use crate::render::{Camera, Vertex};
use crate::state::ClientState;
use camera::{fit, BattleCamera, View, V3, NOMINAL_TAN_HALF_V};
use fmv_core::{EntityId, Pos};
use fmv_ff7::model::{mat4_mul, mat4_translate, transform_point};
use clips::Kit;
use effects::{EffectKit, Element, Look, Strike, Style};
use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent, BattleState, Combatant, Side};
use fmv_ff7_battle::status::StatusSet;
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
    /// The party, `party[i]` fighting as `ActorId(i)`: the canon companions first, then the players.
    pub party: Vec<BattleMember>,
    /// How the game draws each PLAYER of `party` on the field (its binding, from the area's snapshots), in the same order:
    /// `None` for a companion and for a player whose look this client does not know. Missing entries (a list that is
    /// shorter, or empty) read as `None`; a player without a known look is drawn as the default placeholder body.
    pub bindings: Vec<Option<String>>,
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
    /// The identity colour of the player behind a party member, when this client knows it (see
    /// [`BattleScene::show_player_colours`]): its body (not its weapon) is drawn in it, as a player's body is in
    /// the field. `None`: the model keeps its own colours.
    colour: Option<[u8; 3]>,
}

impl Actor {
    fn identity(&self) -> Identity {
        self.colour.map_or(Identity::NONE, Identity::of)
    }
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
    /// The tint of each of `models.models` (a model past the end of this list is drawn as it is): the
    /// opacity of an enemy that is fading away, the glow of an effect.
    pub tints: Vec<Tint>,
    /// The identity colour of each of `models.models` (a model past the end of this list has none): the body of a
    /// party member whose player's colour is known, in that colour. Not its weapon, the enemies, the effects.
    pub identities: Vec<Identity>,
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
    /// The meshes the effects are drawn with, shared with every other scene of the kit.
    effects: Arc<EffectKit>,
    my_actor: Option<ActorId>,
    /// The field entity of each party member that is a PLAYER, in actor order: whose colour is whose
    /// ([`BattleScene::show_player_colours`]); `None` for a canon companion, which has no player, so no colour and
    /// nothing to look up.
    entities: Vec<Option<EntityId>>,
    /// The party members waiting for a command.
    ready: Vec<bool>,
    /// The statuses each actor has: what [`body`] draws. [`BattleScene::update`] copies the replayed engine's;
    /// [`BattleScene::show_statuses`] replaces them with the panels' (which wait for the performance that changes them).
    statuses: Vec<StatusSet>,
    /// The scene clock: the time of the last [`BattleScene::update`], seconds.
    now: f64,
}

/// The look of an action that does nothing the picture shows of its performer (a guard, running away).
const STILL: Look = Look { melee: false, element: Element::Plain, style: Style::Blow(Strike::Slash) };

/// What an action looks like, from the engine's own data: whether its performer runs up to its target (a party member's
/// weapon is short-range, an enemy's physical attack is melee unless it is a shot), the colour of its effect (a spell's
/// element, a restoring action's cure green) and how it is staged ([`Style`]): a record with the `SHOOT` bit (a gun, a
/// laser, a thrown rock) is a shot, a magical one a spell, a physical one a blow that strikes as its `CUT`, `HIT` and `PUNCH`
/// bits say; an item and a limit break are staged from their own records. Without the engine, a plain blow.
fn look_in(engine: Option<&BattleState>, actor: ActorId, action: ActionRef) -> Look {
    use fmv_ff7_battle::rules::{element, ActionKind, ActionSpec};
    let Some(who) = engine.and_then(|e| e.actor(actor)) else { return Look::PLAIN };
    let of = |spec: &ActionSpec| {
        let shot = spec.element_mask & element::SHOOT != 0;
        let style = if shot {
            Style::Shot
        } else if spec.kind == ActionKind::Magical {
            Style::Spell
        } else {
            Style::Blow(Strike::of_mask(spec.element_mask))
        };
        Look { melee: spec.kind == ActionKind::Physical && !shot, element: Element::from_mask(spec.element_mask, spec.aims_at_allies()), style }
    };
    match action {
        // The party's weapon: a blade slashes, a gun shoots (a short-range weapon runs up). An enemy's plain attack thumps.
        ActionRef::Attack if who.side == Side::Enemy => Look { melee: true, element: Element::Plain, style: Style::Blow(Strike::Thump) },
        ActionRef::Attack if who.short_range => Look::PLAIN,
        ActionRef::Attack => Look { melee: false, element: Element::Plain, style: Style::Shot },
        // The scene's attacks that deal damage, and the ones that only have a status (a curse, a buff).
        ActionRef::EnemyAttack(id) => who.actions.iter().chain(&who.status_actions).find(|a| a.id == id).map_or(Look::PLAIN, |a| of(&a.spec)),
        ActionRef::Magic(id) => {
            who.spells.iter().find(|s| s.index == id).map_or(Look { style: Style::Spell, ..STILL }, |s| Look { melee: false, style: Style::Spell, ..of(&s.spec) })
        }
        ActionRef::Item(id) => {
            let spec = engine.and_then(|e| e.items().get(id)).map(|item| item.spec);
            Look { melee: false, element: spec.map_or(Element::Plain, |spec| Element::from_mask(spec.element_mask, spec.aims_at_allies())), style: Style::Item }
        }
        // A limit break does not run up to its target (the performer gathers its power, see `motion`): its effect is
        // cure green when it heals (Healing Wind) and the element of its blow otherwise; a physical one (Braver) strikes.
        ActionRef::Limit(_) => match who.limit_break {
            Some(limit) => {
                let spec = &limit.spec;
                Look {
                    melee: false,
                    element: Element::from_mask(spec.element_mask, spec.aims_at_allies()),
                    style: Style::Limit { physical: spec.kind == ActionKind::Physical && !spec.heals() },
                }
            }
            None => Look { style: Style::Limit { physical: true }, ..STILL },
        },
        // A guard and running away are no blows: nothing to colour.
        ActionRef::Defend | ActionRef::Escape => STILL,
    }
}

impl BattleScene {
    /// The scene of the battle `start` describes and `engine` (the replayed engine) has built. A canon companion's
    /// model is the canon character's own battle model; a PLAYER is drawn as its own body, the field look its binding
    /// names ([`model::player_appearance`]: never a canon battle model). `None` without an engine (nothing is
    /// known of who fights): the flat view is used then.
    pub fn new(start: &SceneStart, engine: Option<&BattleState>, kit: &mut ModelKit) -> Option<Self> {
        let engine = engine?;
        let (party_ids, enemy_ids) = (engine.party_ids(), engine.enemy_ids());
        let spots = layout::arrange(start.formation.as_ref(), enemy_ids.len(), party_ids.len());

        let mut actors: Vec<Actor> = Vec::with_capacity(party_ids.len() + enemy_ids.len());
        for (i, &id) in party_ids.iter().enumerate() {
            let member = start.party.get(i);
            let (character, mine) = (member.map_or(0, |m| m.character), Some(id) == start.my_actor);
            // A member the roster does not describe (the engine has more than the roster, which the server never sends)
            // is a player without a known look: the default body.
            let who = if member.is_some_and(BattleMember::is_companion) {
                PartyLook::companion(id, character, mine)
            } else {
                PartyLook::player(id, character, mine, start.bindings.get(i).cloned().flatten())
            };
            let model = kit.source.party_member(&who);
            let rig = kit.rigs.resolve(&model, BoxSpec::for_party(i));
            actors.push(Actor { side: Side::Party, rig, home: spots.party[i], colour: None });
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
            actors.push(Actor { side: Side::Enemy, rig, home: spots.enemies[j], colour: None });
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
        let kits: Vec<Kit> = actors.iter().map(|a| a.rig.kit.clone()).collect();
        let n = actors.len();
        Some(Self {
            id: start.id,
            actors,
            cameras,
            cast,
            arena,
            location,
            director: Director::with_kits(sides, anchors, kits),
            effects: Arc::clone(&kit.effects),
            my_actor: start.my_actor,
            entities: start.party.iter().map(BattleMember::entity).collect(),
            ready: vec![false; n],
            statuses: vec![StatusSet::EMPTY; n],
            now: 0.0,
        })
    }

    /// Show each party member's player's colour on the member's body: `tint_of` answers with the identity colour
    /// of the player whose field entity a member is (`None`: not known here, the member keeps its own colours, as
    /// it did before there was any). The window asks every frame, from what its snapshots say, so a colour that
    /// arrives late applies and one that changes follows; a colour once known stays (a player that drops out in
    /// the middle of the fight does not change colour with it). Nothing else of the scene changes.
    pub fn show_player_colours(&mut self, tint_of: impl Fn(EntityId) -> Option<[u8; 3]>) {
        for (actor, entity) in self.actors.iter_mut().zip(&self.entities) {
            // A canon companion has no entity, so nothing to be coloured by: it keeps its own colours.
            actor.colour = entity.and_then(&tint_of).or(actor.colour);
        }
    }

    /// Show the statuses the panels show: `view`'s bars say which statuses each fighter has, and a status changes there when the
    /// performance that inflicts or cures it lands (see `lag`), where [`BattleScene::update`] copies the engine's, which has
    /// the whole batch at once. The window's scene is given this after every update (see [`scene_for`]), so that a body's
    /// colour, droop and held pose change at the very instant its status tag does.
    pub fn show_statuses(&mut self, view: &BattleView) {
        for bar in view.party.iter().chain(&view.enemies) {
            if let Some(slot) = self.statuses.get_mut(usize::from(bar.actor.0)) {
                *slot = bar.status;
            }
        }
    }

    /// Every model in its own colours again (tests: the same scene, drawn without the colours it was given).
    #[cfg(test)]
    pub fn clear_player_colours(&mut self) {
        self.actors.iter_mut().for_each(|a| a.colour = None);
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
        // The batch that arrived is a set of performances, each landing at its own instant after `now`
        // (the instants the overlay's numbers are born at: see `cue`).
        if !events.is_empty() {
            self.director.on_batch(self.now, events, &|actor, action| look_in(engine, actor, action));
        }
        match engine {
            Some(engine) => {
                let actors = engine.actors();
                self.director.sync(self.now, |i| actors.get(i).map(Combatant::is_alive));
                for (i, ready) in self.ready.iter_mut().enumerate() {
                    // A sleeper, a stopped or a paralysed member holds a full gauge but is not waiting for a command.
                    *ready = self.actors[i].side == Side::Party && actors.get(i).is_some_and(|c| c.ready && c.can_act());
                }
                for (i, set) in self.statuses.iter_mut().enumerate() {
                    *set = actors.get(i).map_or(StatusSet::EMPTY, |c| c.status.set());
                }
            }
            None => {
                self.ready.iter_mut().for_each(|r| *r = false);
                self.statuses.iter_mut().for_each(|s| *s = StatusSet::EMPTY);
            }
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
        // A sleeper droops.
        let droop = body::of(self.statuses.get(i).copied().unwrap_or_default(), self.now, i).squash;
        let height = actor.rig.scale * motion.squash * idle.breath * droop;
        let place = placement(pos, actor.home.yaw + idle.sway + motion.turn, [actor.rig.scale, height, actor.rig.scale], motion.fall);
        Some((pos.map(f64::from), place, motion, idle))
    }

    /// One frame for a window with `view`, for a target that is (`srgb`) or is not sRGB.
    pub fn frame(&self, view: &View, srgb: bool) -> BattleFrame {
        let camera_matrix = view.camera.matrix();
        let mut models: Vec<SkinnedModel> = Vec::new();
        let mut tints: Vec<Tint> = Vec::new();
        // In step with `models` up to the effects, which have none (a model past the end of the list has none).
        let mut identities: Vec<Identity> = Vec::new();
        let mut marks: Vec<(f64, Vec<GroundShape>)> = Vec::new();
        let to_view = |p: Pos| {
            // `Pos` carries a ground position `(x, z)` in its first two fields; see `ground::circle`.
            let c = view.camera.to_camera([f64::from(p.x), 0.0, f64::from(p.y)]);
            view.pinhole(c, camera::STAGE_NEAR * 0.5).map(|[u, v]| Pos::new(u, v, 0.0))
        };
        for (i, actor) in self.actors.iter().enumerate() {
            let Some((pos, place, motion, _)) = self.pose_of(i) else { continue };
            let body = body::of(self.statuses.get(i).copied().unwrap_or_default(), self.now, i);
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
                // The shadow of an enemy that is fading goes with it.
                shapes.push(GroundShape::disc(outline, [0, 0, 0], 0.40 * (1.0 - lifted.clamp(0.0, 0.8)) * motion.alpha, depth as f32));
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
            // (A stopped or paralysed body holds its idle on one frame.)
            let phase = if body.frozen { i as f32 * IDLE_PHASE_STEP } else { self.now as f32 + i as f32 * IDLE_PHASE_STEP };
            // The clip the director says it plays now (in place), else its idle loop; a weapon moves with
            // its owner: the same placement, the same frame.
            let (pose, weapons) = match motion.clip.and_then(|c| actor.rig.clip_pose(c.slot, c.frame).map(|(pose, root)| (pose, c, root))) {
                Some((pose, c, root)) => (pose, actor.rig.attachment_clip_poses(c.slot, c.frame, root)),
                None => {
                    let Some(pose) = actor.rig.idle_pose(phase) else { continue };
                    (pose, actor.rig.attachment_poses(phase))
                }
            };
            let tint = Tint { rgb: body.rgb, alpha: motion.alpha };
            let mut positions = Vec::new();
            if skin(&actor.rig.asset, &pose, &place, &camera_matrix, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(&actor.rig.asset), positions });
                tints.push(tint);
                identities.push(actor.identity());
                for (asset, pose) in weapons {
                    let mut positions = Vec::new();
                    if skin(asset, &pose, &place, &camera_matrix, &mut positions) {
                        models.push(SkinnedModel { asset: Arc::clone(asset), positions });
                        tints.push(tint);
                        identities.push(Identity::NONE);
                    }
                }
            }
        }
        self.add_effects(view, &camera_matrix, &mut models, &mut tints);
        // The far marks first, so that a near one is drawn over theirs.
        marks.sort_by(|a, b| b.0.total_cmp(&a.0));
        // The location, as it is (its vertices are in battle coordinates already), cut to this
        // camera's near plane and skinned once: the camera does not move.
        let location = self.location.as_ref().and_then(|location| {
            let k = self.cameras.iter().position(|c| *c == view.camera)?;
            let cut = location.cuts.get(k)?.get_or_init(|| LocationCut::make(&location.rig, &view.camera)).as_ref()?;
            Some(AvatarFrame {
                models: vec![SkinnedModel { asset: Arc::clone(&cut.asset), positions: cut.positions.clone() }],
                // The battle's tints and identity colours are `BattleFrame::tints` and `identities` (an actor's
                // opacity, an effect's glow, a player's colour): the frames themselves carry none.
                tints: Vec::new(),
                identities: Vec::new(),
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
                tints: Vec::new(),
                identities: Vec::new(),
                zoom: view.zoom as f32,
                overlay_bias: OVERLAY_BIAS_MODEL_UNITS * model::FIELD_TO_BATTLE_SCALE * 2.0,
                ground: marks.into_iter().flat_map(|(_, shapes)| shapes).collect(),
                scenery: None,
            },
            tints,
            identities,
            camera: view.gpu_camera(),
        }
    }

    /// Where the middle of actor `i` is now (its chest) and its height, for an effect that centres on it:
    /// a little towards the camera so that the effect is not hidden inside the model.
    fn chest_of(&self, i: usize, view: &View) -> Option<(V3, f32)> {
        let (pos, _, _, _) = self.pose_of(i)?;
        let actor = self.actors.get(i)?;
        let (_, _, forward) = view.camera.axes();
        let (h, toward) = (f64::from(actor.rig.height), f64::from(actor.rig.radius) * 0.6);
        Some(([pos[0] - forward[0] * toward, pos[1] - h * 0.55 - forward[1] * toward, pos[2] - forward[2] * toward], actor.rig.height))
    }

    /// The effects of the moment ([`effects`]): each one that is on at the scene clock, its pieces placed round the actor it plays
    /// at as models facing the camera (a bolt as a ribbon between two actors, a flash over the whole window), with a tint each.
    /// The newest effects win when there are more pieces than [`effects::MAX_LAYERS`].
    fn add_effects(&self, view: &View, camera_matrix: &[f64; 16], models: &mut Vec<SkinnedModel>, tints: &mut Vec<Tint>) {
        use effects::Place;
        let (right, down, forward) = view.camera.axes();
        let to_f32 = |v: V3| v.map(|c| c as f32);
        let axes = (to_f32(right), to_f32(down), to_f32(forward));
        let identity = [fmv_ff7::model::mat4_identity()];
        // The camera-frame offset `o` (right, down, towards the camera) from `centre`, in battle space.
        let world = |centre: V3, o: [f32; 3]| -> V3 {
            let o = o.map(f64::from);
            [0, 1, 2].map(|k| centre[k] + right[k] * o[0] + down[k] * o[1] - forward[k] * o[2])
        };
        let mut pieces: Vec<(V3, effects::Layer)> = Vec::new();
        let mut scratch: Vec<effects::Layer> = Vec::with_capacity(effects::MAX_LAYERS_PER_FX);
        for fx in self.director.fx().iter().rev() {
            if pieces.len() >= effects::MAX_LAYERS || fx.phase(self.now).is_none() {
                continue;
            }
            let Some((centre, height)) = self.chest_of(fx.target, view) else { continue };
            // Where the actor a flying effect comes from is, as seen from the actor it flies to.
            let from = fx.from.and_then(|f| self.chest_of(f, view)).map(|(p, _)| {
                let d = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
                let dot = |a: V3| (d[0] * a[0] + d[1] * a[1] + d[2] * a[2]) as f32;
                [dot(right), dot(down), -dot(forward)]
            });
            scratch.clear();
            effects::layers(fx, self.now, &effects::Geo { height, from }, &mut scratch);
            pieces.extend(scratch.drain(..).map(|l| (centre, l)));
        }
        pieces.truncate(effects::MAX_LAYERS);
        for (centre, layer) in pieces {
            let place = match layer.place {
                Place::Facing { at, half, spin } => Some(effects::billboard(world(centre, at).map(|c| c as f32), half, spin, axes)),
                Place::Between { from, to, half } => effects::ribbon(world(centre, from).map(|c| c as f32), world(centre, to).map(|c| c as f32), half, axes.0, axes.2),
                // A plane in front of everyone that covers the window, with the lens axis at its middle.
                Place::Screen => {
                    const DEPTH: f64 = 1700.0;
                    let zoom = view.zoom.max(1.0);
                    let (hw, hh) = (f64::from(view.width) * 0.5 + view.shift[0].abs(), f64::from(view.height) * 0.5 + view.shift[1].abs());
                    let reach = (hw.hypot(hh) * DEPTH / zoom * 2.2) as f32;
                    let at = [-view.shift[0] * DEPTH / zoom, view.shift[1] * DEPTH / zoom, DEPTH];
                    let eye = view.camera.position();
                    let middle = [0, 1, 2].map(|k| (eye[k] + right[k] * at[0] + down[k] * at[1] + forward[k] * at[2]) as f32);
                    Some(effects::billboard(middle, [reach, reach], 0.0, axes))
                }
            };
            let Some(place) = place else { continue };
            let asset = self.effects.asset(layer.prim);
            let mut positions = Vec::new();
            if skin(asset, &identity, &place, camera_matrix, &mut positions) {
                models.push(SkinnedModel { asset: Arc::clone(asset), positions });
                tints.push(effects::layer_tint(&layer));
            }
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
        let mut start = state.battle.scene_start().filter(|s| s.id == view.id)?;
        // A player is drawn as its own body: how the area's snapshots say the game draws it on the field.
        start.bindings = start.party.iter().map(|m| m.entity().and_then(|entity| state.appearance_of(entity)).map(str::to_string)).collect();
        let kit = kit.get_or_insert_with(ModelKit::from_local_content);
        *slot = BattleScene::new(&start, state.battle.engine(), kit);
    }
    let scene = slot.as_mut()?;
    // Each party member's body is drawn in its player's colour, as the field draws the player (what the area's
    // snapshots say, asked every frame so that one that arrives late applies).
    scene.show_player_colours(|entity| state.tint_of(entity));
    scene.update(now, events, state.battle.engine());
    // The statuses of the bodies are the panels': they change when the performance that changes them lands (only while the
    // battle runs: after it, nothing is left on anybody).
    if state.battle.engine().is_some() {
        scene.show_statuses(view);
    }
    Some(scene)
}

#[cfg(test)]
mod tests;
