//! Pictures and measured acceptance of the PROOF SCENE (Cloud and Tifa at the bar of 7th Heaven) over the owner's REAL
//! field `mds7pb_1`, drawn through the window's own code: `field_draw` (the cast, the hidden trigger, the witnesses), the
//! HUD with its label anchors, and the scene's window near whoever speaks.
//!
//! They need the owner's data (`FMV_FF7_LIBRARY`, `FMV_FF7_DATA` or `FMV_FF7_FLEVEL`, read-only) and a GPU or a software
//! Vulkan/GL driver (lavapipe), so they only run on request:
//!
//! ```text
//! FMV_FF7_LIBRARY=<state dir> FMV_SHOT_DIR=<dir> cargo test --offline -p fmv-client --bin fmv-client \
//!     -- --ignored --exact scene::shots::offscreen_real_ff7_the_proof_scene --test-threads=1 --nocapture
//! FMV_FF7_LIBRARY=<state dir> FMV_SHOT_DIR=<dir> cargo test --offline -p fmv-client --bin fmv-client \
//!     -- --ignored --exact scene::shots::offscreen_real_ff7_the_witness_spots_and_a_recommendation --test-threads=1 --nocapture
//! FMV_FF7_LIBRARY=<state dir> FMV_SHOT_DIR=<dir> FMV_EXPLORATORY_MASK=3 cargo test --offline -p fmv-client --bin fmv-client \
//!     -- --ignored --exact scene::shots::offscreen_survey_flower_witness --test-threads=1 --nocapture
//! ```
//! The exploratory early-bar and flower surveys accept `FMV_EXPLORATORY_SPOT` (one spot) and
//! `FMV_EXPLORATORY_MASK` (choice ordinals). They save sample pictures and check walkmesh
//! distance, but do not validate all-frame screen occlusion, live progression or audio.
//! The registered flower scene has a separate all-branch visual regression,
//! [`offscreen_real_ff7_flower_choices_and_camera`], using its actual witnesses and owner-resource snapshot.
//!
//! The two post-escape scenes ask nothing, so their surveys (`offscreen_survey_post_escape_witness`, field `md8_4`, and
//! `offscreen_survey_station_witness`, field `md8_1`; also `FMV_EXPLORATORY_SPOT`, plus `FMV_SCENE_WITNESS_BODY` for the other
//! placeholder bodies) play the scene to its end and save a 960 x 720 picture every second step and one of each window as it
//! opens (`<prefix>-<step>.png`, the window drawn as the player sees it). They print, for the spot: its distance to every place
//! and walk the interpreter's stage events give the cast and to the nearest boundary edge (held to `2 * 5.5 * model_scale + 16`
//! and `1.5 * 5.5 * model_scale`, the rule `fmv-ff7`'s `tests/scene_real.rs` holds the proof scene's spots to); how much of
//! each placeholder body shows in front of the picture's foreground tiles; and, at every step, how much of the witness's picture
//! a nearer model's covers and how much of a farther model's the witness's covers (rectangle against rectangle, as `covered`;
//! `boxes_of` cannot tell the cast apart, every field model being `"npc"`, so [`Rig::pics_of`] tells models apart by their
//! feet), with the steps over [`MAX_COVERED`]. Those overlaps are printed, not asserted.
//!
//! The measurements read whatever witness spots and arrival `fmv_ff7::script::scene::proof_scene()` has, so they judge the
//! game's data as it is. Two environment variables try data that is not in `fmv_ff7` yet, without editing it:
//! `FMV_SCENE_SPOTS="x,y,triangle,direction;x,y,triangle,direction;..."` (the witness spots, the owner's first) and
//! `FMV_SCENE_ARRIVAL="x,y,triangle,direction"`; `FMV_SCENE_WITNESS_BODY=<archetype>` makes the local player wear another
//! placeholder body than the default (to MEASURE it: the default is unchanged).
//!
//! * `offscreen_real_ff7_the_proof_scene` plays the whole scene through `state.apply(SceneStart)` with the real
//!   [`Ff7Scenes`], the owner (the local player, the default placeholder body `ff7/body/lean`) and two party members that
//!   watch, each on its own witness spot, and writes `scene-*.png`: the field before, the first frame, a window over Tifa,
//!   one over Cloud, the question with its pointer, and the field after the scene. It MEASURES, on every window and every
//!   quarter second of the scene, and asserts (all of it reported at the end, after every picture is made):
//!   * every witness is drawn through the same pipeline and the same model scale as the cast, and its height (the skinned
//!     model's extent along the world's up axis) lies within `[0.9 x the shortest cast member, 1.1 x the tallest]` in the
//!     same frame (the ratios against Cloud and Tifa are printed);
//!   * every witness and every cast actor stand at least `2 * 5.5 * model_scale` walkmesh units apart (the rings under their
//!     feet do not touch);
//!   * on the SCREEN, no more than 10% of a witness's picture is covered by Cloud's, Tifa's or another witness's (the
//!     rectangle their skinned vertices fill; every fraction is printed);
//!   * every witness spot is inside the walkmesh triangle it names;
//!   * the models are DRAWN where the stage says (the lowest vertices of a skinned model are at its feet);
//!   * the OK that starts the scene is found next to Tifa with the reach the scene's own data gives, and not far from her.
//! * `offscreen_real_ff7_the_witness_spots_and_a_recommendation` measures the spots on the SCREEN, the heights of every
//!   placeholder body against Cloud's and Tifa's, ranks the floor of the customers' side for a spot one witness would not
//!   overlap anybody on, says where each spot could stand instead given the others, and finds the three spots TOGETHER that
//!   move least from the data's and overlap nobody (the cast at every frame of the scene, any of the three bodies on any
//!   spot, two players wearing the same one too): printed as `FMV_SCENE_SPOTS` and as `Spot` literals, then played through
//!   with the pictures and held to the acceptance numbers above.
//!
//! Nothing here prints a word of the game: ids, numbers and entity names only. (The pictures show the game's own windows.)

use super::draw::{cast_who, label_hidden};
use super::panel::Rect;
use super::{route_key, speaker_spot, Built, Ff7Scenes, SceneBuilder, SceneRoute};
use crate::avatar::frame::{Built as Drawn, RING_RADIUS_UNITS};
use crate::avatar::library::AvatarLibrary;
use crate::avatar::source::LgpLoader;
use crate::avatar::{AvatarPlayer, Avatars, Who};
use crate::backdrop::FieldView;
use crate::ff7::{prewarm, resolve_char_lgp, resolve_flevel, Ff7Labels, Ff7Source};
use crate::npc::npc_who;
use crate::render::{view_to_window, Scene};
use crate::session::SessionInfo;
use crate::state::ClientState;
use crate::ui::draw::DrawList;
use crate::ui::hud::{self, Subject};
use crate::ui::testkit::{save, Offscreen};
use crate::window::{field_draw, FieldDraw};
use fmv_adapter_api::AreaRuntime;
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos};
use fmv_ff7::playable::PLACEHOLDER_BODIES;
use fmv_ff7::script::scene::{proof_scene, trigger_npc, MoveHow, SceneDef, ScenePlay, Spot, Stage, Then, PROOF_FIELD};
use fmv_ff7::script::triangle_contains_xy;
use fmv_ff7::story::StoryState;
use fmv_ff7::{field_direction_to_facing, Ff7Adapter};
use fmv_protocol::{NpcSnapshot, SceneOutcome, SceneParticipant, SceneRole, SceneRunId, ServerMsg, NPC_NAME_CHARS, NPC_REF_CHARS, SCENE_NPC_ID_BASE};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use winit::keyboard::KeyCode;

mod continuation;
mod door_offsets;
mod effects;
mod rootmap;
mod station_guards;
mod train;
mod train_ready;

/// The most of a witness's picture another's may cover on the screen.
const MAX_COVERED: f32 = 0.10;

/// The owner's FF7 data, opened read-only: the field adapter and the character archive.
struct Real {
    adapter: Arc<Ff7Adapter>,
    char_lgp: PathBuf,
}

/// `FMV_FF7_LIBRARY`, else `FMV_FF7_DATA`, else `FMV_FF7_FLEVEL`; `None` (with a note) when none is set.
fn open() -> Option<Real> {
    let direct = |path: PathBuf| Ff7Source::Flevel { path, labels: Ff7Labels::default() };
    let source = if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        Ff7Source::Library { state_dir: Some(dir.into()), set: None }
    } else if let Some(dir) = std::env::var_os("FMV_FF7_DATA") {
        direct(PathBuf::from(dir).join("field").join("flevel.lgp"))
    } else if let Some(path) = std::env::var_os("FMV_FF7_FLEVEL") {
        direct(path.into())
    } else {
        eprintln!("skipped: none of FMV_FF7_LIBRARY, FMV_FF7_DATA, FMV_FF7_FLEVEL is set");
        return None;
    };
    let resolved = resolve_flevel(&source).expect("the field archive is found");
    let adapter = Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance).expect("the field archive opens");
    prewarm(&adapter);
    let char_lgp = resolve_char_lgp(&source).expect("the character archive is looked for").expect("a char.lgp next to the field archive");
    Some(Real { adapter: Arc::new(adapter), char_lgp })
}

/// A spot from `x,y,triangle,direction`.
fn parse_spot(text: &str) -> Option<Spot> {
    let n: Vec<i64> = text.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    match n.as_slice() {
        [x, y, triangle, direction] => Some(Spot { x: i16::try_from(*x).ok()?, y: i16::try_from(*y).ok()?, triangle: u16::try_from(*triangle).ok()?, direction: u8::try_from(*direction).ok()? }),
        _ => None,
    }
}

/// The proof scene as the game has it, with its witness spots and arrival replaced by `FMV_SCENE_SPOTS` and
/// `FMV_SCENE_ARRIVAL` when they are set (see the module docs). Whether anything was replaced is `overridden`.
fn proof_def() -> (SceneDef, bool) {
    let mut def = proof_scene();
    let mut overridden = false;
    if let Ok(text) = std::env::var("FMV_SCENE_SPOTS") {
        let spots: Vec<Spot> = text.split(';').filter_map(parse_spot).collect();
        if !spots.is_empty() {
            def.witness = spots;
            overridden = true;
        }
    }
    if let Some(arrival) = std::env::var("FMV_SCENE_ARRIVAL").ok().and_then(|t| parse_spot(&t)) {
        def.arrival = arrival;
        overridden = true;
    }
    (def, overridden)
}

/// The look the local player wears in these pictures: the default placeholder body (`ff7/body/lean`), unless
/// `FMV_SCENE_WITNESS_BODY` names another archetype of the table (to MEASURE it: the default is not changed by it).
fn witness() -> &'static str {
    let wanted = std::env::var("FMV_SCENE_WITNESS_BODY").unwrap_or_default();
    PLACEHOLDER_BODIES.iter().find(|b| b.archetype == wanted).unwrap_or(&PLACEHOLDER_BODIES[0]).model.appearance_ref
}

/// The looks of the three people who stand in the scene: the owner's, then two party members' (the other two bodies).
fn looks() -> [&'static str; 3] {
    let mine = witness();
    let mut others = PLACEHOLDER_BODIES.iter().map(|b| b.model.appearance_ref).filter(|r| *r != mine);
    [mine, others.next().expect("the table has three bodies"), others.next().expect("the table has three bodies")]
}

/// A model's drawn size and where its feet are, in the field's own units (walkmesh x, y and height).
#[derive(Debug, Clone, Copy)]
struct Body {
    /// The skinned model's extent along the world's up axis (hair, hat and what is carried included).
    height: f32,
    /// How far above the ground the frame puts the top of the head (what a name label goes over): `height_units` of the
    /// asset at the field's model scale.
    head: f32,
    feet: (f32, f32),
}

/// One drawn model's picture on the screen, told apart from the others' by where its feet stand (every cast member that is a
/// field model has the same `appearance_ref`, `"npc"`, so [`Rig::boxes_of`] cannot tell them apart).
#[derive(Debug, Clone, Copy)]
struct Pic {
    /// Where the model's feet are, in field x and y.
    feet: (f32, f32),
    /// The camera-space depth at its feet: the smaller, the nearer the camera (the nearer model is drawn over the other).
    depth: f32,
    /// The rectangle its skinned vertices fill, in window pixels.
    rect: Rect,
}

/// A camera-space point in field space: the camera is `cam = R * world + T` (the matrix is column-major, `R[i][j] =
/// m[4 * j + i]`, and a rotation), so `world = R^T * (cam - T)`.
fn to_world(m: &[f64; 16], cam: [f32; 3]) -> [f32; 3] {
    let d = [f64::from(cam[0]) - m[12], f64::from(cam[1]) - m[13], f64::from(cam[2]) - m[14]];
    [0, 1, 2].map(|j| (0..3).map(|i| m[4 * j + i] * d[i]).sum::<f64>() as f32)
}

/// How much of `a` (as a fraction of its area) `b` covers.
fn covered(a: Rect, b: Rect) -> f32 {
    let (dx, dy) = (a.2.min(b.2) - a.0.max(b.0), a.3.min(b.3) - a.1.max(b.1));
    let area = (a.2 - a.0) * (a.3 - a.1);
    if dx > 0.0 && dy > 0.0 && area > 0.0 {
        dx * dy / area
    } else {
        0.0
    }
}

/// What the run measured, to print and to hold the asserts to.
#[derive(Debug, Default)]
struct Report {
    frames: u32,
    /// The least distance between a witness and a cast actor or another witness, in walkmesh units.
    min_separation: f32,
    /// For each witness (by spot): the least and greatest height over Cloud's, and over Tifa's.
    ratio_cloud: [Option<(f32, f32)>; 3],
    ratio_tifa: [Option<(f32, f32)>; 3],
    /// The greatest distance between where a model's feet are drawn and where the stage stands it, in walkmesh units.
    max_feet_error: f32,
    /// How many frames had Cloud, and Tifa, drawn.
    seen: [u32; 2],
    /// The most of a witness's picture that another's covers on the screen, over the frames, by `"witnessN by <who>"`.
    covers: BTreeMap<String, f32>,
    /// What did not hold, the first few of each kind (the test fails with them AFTER every picture is made).
    problems: Vec<String>,
    /// Where Cloud and Tifa were, and where their pictures were, at each measured frame.
    cast: Vec<CastFrame>,
}

/// Cloud's and Tifa's pictures on the screen and the places they stood on, at one measured frame.
#[derive(Debug, Clone, Copy)]
struct CastFrame {
    cloud: Option<Rect>,
    tifa: Option<Rect>,
    at: [Option<Pos>; 2],
}

impl Report {
    fn note_ratio(slot: &mut Option<(f32, f32)>, ratio: f32) {
        *slot = Some(slot.map_or((ratio, ratio), |(lo, hi)| (lo.min(ratio), hi.max(ratio))));
    }

    /// Note a failed check: at most three of each kind (the `kind`) are kept.
    fn problem(&mut self, kind: &str, text: String) {
        if self.problems.iter().filter(|p| p.starts_with(kind)).count() < 3 {
            self.problems.push(format!("{kind}: {text}"));
        }
    }
}

/// Builds the scene of a def that is not the game's own (the witness spot moved), from the same field.
struct Moved {
    adapter: Arc<Ff7Adapter>,
    def: SceneDef,
    /// For the trigger's reach, which does not depend on the spots.
    reach: Ff7Scenes,
}

impl SceneBuilder for Moved {
    fn build(&mut self, _scene: &str, story: &StoryState, seed: u32, resources: fmv_ff7::script::scene::SceneResources) -> Result<Built, String> {
        let area = self.adapter.load_field(&self.def.field).map_err(|e| e.to_string())?;
        let play = ScenePlay::with_resources(area.field_file(), &self.def, story, seed, resources).map_err(|e| e.to_string())?;
        Ok(Built { def: self.def.clone(), source: Box::new(play) })
    }

    fn build_with_origin(&mut self, scene: &str, story: &StoryState, seed: u32, resources: fmv_ff7::script::scene::SceneResources,
        origin: Option<&fmv_protocol::SceneOrigin>) -> Result<Built, String> {
        let Some(origin) = origin else { return self.build(scene, story, seed, resources) };
        let area = self.adapter.load_field(&self.def.field).map_err(|e| e.to_string())?;
        let def = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &self.def, origin.pos, origin.facing, origin.triangle)
            .map_err(|e| e.to_string())?;
        let play = ScenePlay::with_resources(area.field_file(), &def, story, seed, resources).map_err(|e| e.to_string())?;
        Ok(Built { def, source: Box::new(play) })
    }

    fn reach(&mut self, index: usize, area: &str) -> Option<f32> {
        self.reach.reach(index, area)
    }
}

struct Rig {
    view: FieldView,
    state: ClientState,
    avatars: Avatars,
    npc_epoch: u64,
    own_tint: [u8; 3],
    own_real: Pos,
    looks: [&'static str; 3],
    def: SceneDef,
    story: StoryState,
    gil: u32,
    /// The interpreter's own copy of the scene, for the ground's facts.
    play: ScenePlay,
    scale: f32,
    report: Report,
}

const SEED: u32 = 41;
const SIZE: (u32, u32) = (1100, 680);
/// What the witnesses are called by their spots, in the pictures' measurements.
const SPOT_NAMES: [&str; 3] = ["witness0", "witness1", "witness2"];

/// The rig for `def`: the game's proof scene (made by [`Ff7Scenes`]) or, when `moved`, one made from `def` itself (the witness
/// spots changed). The local player is the owner; two party members stand in the area too, to watch.
fn make_rig(real: &Real, def: SceneDef, moved: bool) -> Rig {
    let story = if def.requires_presentation_origin() { StoryState::fresh_first_control() }
        else { StoryState::at(*def.trigger.moments.start()) };
    make_rig_with_snapshot(real, def, moved, story, 0)
}

/// Build both the reference interpreter and the actual scene with the same server snapshot.
fn make_rig_with_snapshot(real: &Real, def: SceneDef, moved: bool, story: StoryState, gil: u32) -> Rig {
    let key = def.field.as_str();
    let area = real.adapter.load_field(key).expect("the proof field loads");
    let view = FieldView::ff7_with(&real.adapter, key, true).expect("field view");
    let scale = view.stage.as_ref().expect("the field has a stage").model_scale;
    let mut play = ScenePlay::with_resources(area.field_file(), &def, &story, SEED, fmv_ff7::script::scene::SceneResources { gil: Some(gil) }).expect("the scene plays here");
    // The area's own characters, announced as the server announces them, and then the scene's trigger as the server adds it
    // while the scene is on offer.
    let clip = |text: &str, max: usize| -> String { text.chars().filter(|c| !c.is_control()).take(max).collect() };
    let mut npcs: Vec<NpcSnapshot> = area
        .npcs()
        .iter()
        .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
        .enumerate()
        .map(|(i, n)| NpcSnapshot {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            name: clip(&n.entity_name, NPC_NAME_CHARS),
            model: clip(&n.appearance.model, NPC_REF_CHARS),
            idle_animation: n.appearance.idle_animation.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
            pos: n.pos,
            facing: n.facing,
        })
        .collect();
    let trigger = trigger_npc(area.field_file(), &def, &story);
    if def.id == fmv_ff7::script::scene::PROOF_SCENE_ID { assert!(trigger.is_some(), "the proof trigger stands in the field"); }
    if let Some(trigger) = trigger { npcs.push(NpcSnapshot {
        id: SCENE_NPC_ID_BASE,
        name: clip(&trigger.name, NPC_NAME_CHARS),
        model: clip(&trigger.model, NPC_REF_CHARS),
        idle_animation: trigger.idle.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
        pos: Pos::new(f32::from(trigger.x), f32::from(trigger.y), trigger.z),
        facing: trigger.heading,
    }); }
    // The local player stands where a preview puts it: at the scene's arrival spot, facing Tifa across the counter, in the
    // default placeholder body and a colour of its own. Two party members stand near it.
    let arrival = def.arrival;
    let z = play.ground().height(arrival.x, arrival.y, Some(arrival.triangle), None).expect("the arrival spot is on the floor");
    let own_real = Pos::new(f32::from(arrival.x), f32::from(arrival.y), z);
    let facing = field_direction_to_facing(arrival.direction);
    if def.requires_presentation_origin() {
        let bound = fmv_ff7::script::scene::with_trigger_position(area.field_file(), &def, own_real, facing,
            Some(u32::from(arrival.triangle))).expect("the test participant stands on the accepted source floor");
        play = ScenePlay::with_resources(area.field_file(), &bound, &story, SEED,
            fmv_ff7::script::scene::SceneResources { gil: Some(gil) }).expect("the reference uses the same actual origin");
    }
    let own_tint = [90, 125, 245];
    let looks = looks();
    let area_id = AreaId { game: GameId::Ff7, key: key.to_string() };
    let mut state = ClientState::new(EntityId(1), AccountId(1), own_real, area_id.clone());
    state.change_area(area_id.clone(), own_real, facing, false);
    eprintln!("the local player wears {}, the watchers {} and {}", looks[0], looks[1], looks[2]);
    let person = |id: u64, name: &str, tint: [u8; 3], look: &str, at: Pos| EntitySnapshot {
        id: EntityId(id),
        pos: at,
        facing,
        anim: AnimState::Idle,
        avatar: AvatarAppearance::new(name, tint, Some(look.to_string())),
    };
    let beside = |dx: f32, dy: f32| area.place_arrival(Pos::new(own_real.x + dx, own_real.y + dy, own_real.z), None);
    let mut entities = vec![
        person(1, "Wedge", own_tint, looks[0], own_real),
        person(2, "PlayerB", [240, 120, 40], looks[1], beside(60.0, 0.0)),
        person(3, "PlayerC", [70, 200, 90], looks[2], beside(0.0, 60.0)),
    ];
    // (the surveys start the scene for the owner alone: no watcher bodies stand about in the pictures)
    let owner_alone = [
        fmv_ff7::script::scene::EARLY_BAR_SCENE_ID,
        fmv_ff7::script::scene::FLOWER_STATION_SCENE_ID,
        fmv_ff7::script::scene::POST_ESCAPE_SCENE_ID,
        fmv_ff7::script::scene::STATION_SCENE_ID,
    ];
    if owner_alone.contains(&def.id.as_str()) && def.witness.len() <= 1 { entities.truncate(1); }
    assert!(state.apply(0.0, ServerMsg::AreaSnapshot { area: area_id, tick: 1, entities, npcs }).is_none());
    if moved {
        state.scene.set_builder(Box::new(Moved { adapter: Arc::clone(&real.adapter), def: def.clone(), reach: Ff7Scenes::new(Some(Arc::clone(&real.adapter))) }));
    } else {
        state.scene.set_builder(Box::new(Ff7Scenes::new(Some(Arc::clone(&real.adapter)))));
    }
    let avatars = Avatars::new(AvatarLibrary::new(Box::new(LgpLoader::new(&real.char_lgp))));
    Rig { view, state, avatars, npc_epoch: 0, own_tint, own_real, looks, def, story, gil, play, scale, report: Report { min_separation: f32::MAX, ..Report::default() } }
}

/// Every place the cast takes in a played scene, as the interpreter's stage events say it: the initial placements and every
/// walk (including `WalkKeepAnimation`, `Jump` or `Ladder`), sampled each ten units, hidden actors too. The same rule as `cast_path` in
/// `fmv-ff7`'s `tests/scene_real.rs`.
fn cast_path(stage: &[Stage]) -> Vec<(u8, f32, f32)> {
    let mut at: BTreeMap<u8, (f32, f32)> = BTreeMap::new();
    let mut out = Vec::new();
    for s in stage {
        match *s {
            Stage::Place { entity, x, y, .. } => {
                at.insert(entity, (f32::from(x), f32::from(y)));
                out.push((entity, f32::from(x), f32::from(y)));
            }
            Stage::Move { entity, from, x, y, how, .. } => {
                let start = from.map(|(fx, fy)| (f32::from(fx), f32::from(fy))).or_else(|| at.get(&entity).copied());
                let end = (f32::from(x), f32::from(y));
                if let (Some(a), MoveHow::Walk | MoveHow::WalkKeepAnimation | MoveHow::Jump | MoveHow::Ladder) = (start, how) {
                    let n = ((end.0 - a.0).hypot(end.1 - a.1) / 10.0).ceil().max(1.0) as u32;
                    for k in 0..=n {
                        let t = k as f32 / n as f32;
                        out.push((entity, a.0 + (end.0 - a.0) * t, a.1 + (end.1 - a.1) * t));
                    }
                }
                at.insert(entity, end);
                out.push((entity, end.0, end.1));
            }
            _ => {}
        }
    }
    out
}

/// Every place the cast of `def` takes over its whole scene (a scene that asks nothing), from the interpreter alone.
fn scene_path(real: &Real, def: &SceneDef) -> Vec<(u8, f32, f32)> {
    let area = real.adapter.load_field(&def.field).expect("the scene's field loads");
    let story = StoryState::at(*def.trigger.moments.start());
    let mut play = ScenePlay::with_resources(area.field_file(), def, &story, SEED, fmv_ff7::script::scene::SceneResources { gil: Some(0) }).expect("the scene plays here");
    let mut stage: Vec<Stage> = play.initial_stage_events().to_vec();
    for _ in 0..200 {
        let beat = play.next(None);
        stage.extend(beat.stage.iter().map(|t| t.what.clone()));
        if matches!(beat.then, Then::End { .. }) {
            return cast_path(&stage);
        }
    }
    panic!("the scene never ends");
}

/// The distance in x and y from `(x, y)` to the nearest boundary edge of the walkmesh (an edge with no neighbour: a wall, or the
/// edge of a hole). The same measure as `scenery_gap` in `fmv-ff7`'s `tests/scene_real.rs`.
fn boundary_gap(mesh: &fmv_ff7::field::Walkmesh, x: f32, y: f32) -> f32 {
    let mut best = f32::INFINITY;
    for (i, tri) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            if mesh.neighbor(i, e).is_some() {
                continue;
            }
            let (a, b) = (tri[e], tri[(e + 1) % 3]);
            best = best.min(segment_distance((x, y), (f32::from(a.x), f32::from(a.y)), (f32::from(b.x), f32::from(b.y))));
        }
    }
    best
}

/// The distance from the point `p` to the segment `a`-`b`.
fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p.0 - (a.0 + dx * t)).hypot(p.1 - (a.1 + dy * t))
}

/// What the other models' pictures did to the witness's over a quiet survey, at every step, on the screen the pictures are made for
/// (rectangle against rectangle: the rectangles the skinned vertices fill, the measure of [`covered`]). Which of two overlapping
/// pictures is drawn over the other is the one NEARER the camera.
#[derive(Debug, Clone)]
struct Overlap {
    /// The most of the witness's picture a nearer model's covers, at which step, and which cast entity it was.
    hidden: (f32, usize, Option<u8>),
    /// The most of a farther model's picture the witness's covers (the witness is in its way), at which step, and which entity.
    blocks: (f32, usize, Option<u8>),
    /// The least distance from the witness's picture to the screen's edge, in pixels (negative: part of it is off the screen).
    margin: f32,
    /// Steps measured, and the steps the witness's own model was found in the frame.
    steps: usize,
    found: usize,
    /// The steps at which a model covered more than [`MAX_COVERED`] of the witness's picture (`true`) or the witness covered more
    /// than that of a farther model's (`false`): step, the part, the cast entity.
    over: Vec<(usize, f32, bool, Option<u8>)>,
}

impl Overlap {
    fn new() -> Self {
        Self { hidden: (0.0, 0, None), blocks: (0.0, 0, None), margin: f32::MAX, steps: 0, found: 0, over: Vec::new() }
    }

    /// Note one step: every model of the frame as a [`Pic`], the witness's place and where each cast member stands.
    fn note(&mut self, step: usize, pics: &[Pic], witness: Pos, cast: &[(u8, Pos)], scale: f32, (w, h): (u32, u32)) {
        self.steps += 1;
        let near = |p: &Pic, at: Pos| (p.feet.0 - at.x).hypot(p.feet.1 - at.y);
        let Some(mine) = pics.iter().filter(|p| near(p, witness) <= 4.0 * scale).min_by(|a, b| near(a, witness).total_cmp(&near(b, witness))) else { return };
        self.found += 1;
        self.margin = self.margin.min(mine.rect.0).min(mine.rect.1).min(w as f32 - mine.rect.2).min(h as f32 - mine.rect.3);
        for other in pics.iter().filter(|p| !std::ptr::eq(*p, mine)) {
            let who = cast.iter().filter(|c| near(other, c.1) <= 4.0 * scale).map(|c| c.0).next();
            if other.depth < mine.depth {
                let c = covered(mine.rect, other.rect);
                if c > self.hidden.0 {
                    self.hidden = (c, step, who);
                }
                if c > MAX_COVERED {
                    self.over.push((step, c, true, who));
                }
            } else {
                let c = covered(other.rect, mine.rect);
                if c > self.blocks.0 {
                    self.blocks = (c, step, who);
                }
                if c > MAX_COVERED {
                    self.over.push((step, c, false, who));
                }
            }
        }
    }
}

/// How many pixels differ between two renders.
// (`as_chunks` would satisfy the lint but needs Rust 1.88; the workspace keeps `chunks_exact`, see `fmv_ff7::lgp`.)
#[allow(clippy::chunks_exact_to_as_chunks)]
fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(p, q)| (0..3).map(|i| u32::from(p[i].abs_diff(q[i]))).sum::<u32>() > 30).count()
}

/// The witness alone on the field, without the marks on the floor under it (the widest part of its picture, and a ledge in
/// front of the feet would hide them, which says nothing about the body), in a colour of its own, drawn with the picture's
/// foreground tiles or, when `flat`, over the flat picture with nothing in front of it.
fn render_alone(rig: &mut Rig, gpu: &mut Offscreen, look: &'static str, tint: [u8; 3], spot: &Spot, size: (u32, u32), flat: bool) -> Vec<u8> {
    let stage = rig.view.stage.clone().expect("stage");
    let players = [player(tint, Who(1), look, (spot_pos(rig, spot), field_direction_to_facing(spot.direction)), false)];
    let mut built = rig.avatars.frame(&rig.view, &stage, &players, 0.0);
    built.frame.ground.clear();
    if flat {
        built.frame.scenery = None;
    }
    let scene = Scene { bounds: rig.view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: rig.view.backdrop.as_ref() };
    gpu.render_field(size.0, size.1, &scene, Some(&built.frame), &DrawList::new())
}

/// The part of a body (`look`) standing on `spot` that shows in front of the picture's foreground (a field's picture is tiles
/// with a depth of their own, and a body behind a near tile is hidden by it, whatever the walkmesh says): the pixels that two
/// colours of the same body differ in (a placeholder body takes its player's colour, so they differ exactly where the body is
/// drawn) with the foreground tiles, over the same with the flat picture.
fn visible_fraction(rig: &mut Rig, gpu: &mut Offscreen, look: &'static str, spot: &Spot, size: (u32, u32)) -> f32 {
    let (a, b) = ([240, 60, 60], [60, 240, 60]);
    let (on_a, on_b) = (render_alone(rig, gpu, look, a, spot, size, false), render_alone(rig, gpu, look, b, spot, size, false));
    let (off_a, off_b) = (render_alone(rig, gpu, look, a, spot, size, true), render_alone(rig, gpu, look, b, spot, size, true));
    changed(&on_a, &on_b) as f32 / changed(&off_a, &off_b).max(1) as f32
}

/// What a quiet survey measures at every step besides the cast's sampled distance (see [`Survey::Quiet`]).
struct Quiet {
    size: (u32, u32),
    overlap: Overlap,
    /// Where each cast member was one step before, so that the walk between two steps counts as places the cast takes.
    last: BTreeMap<u8, Pos>,
    /// The least distance from the witness to a cast member or to the straight walk it made since the step before.
    between: f32,
}

impl Quiet {
    fn step(&mut self, rig: &mut Rig, step: usize) {
        let draw = rig.draw();
        let pics = rig.pics_of(draw.built.as_ref().expect("the models were built"), self.size);
        let Some(scene) = rig.state.scene.player() else { return };
        let Some(w) = scene.witnesses().first().copied() else { return };
        let cast: Vec<(u8, Pos)> = scene.cast_draws().iter().map(|c| (c.entity, c.pos)).collect();
        for (entity, at) in &cast {
            let from = self.last.insert(*entity, *at).unwrap_or(*at);
            self.between = self.between.min(segment_distance((w.pos.x, w.pos.y), (from.x, from.y), (at.x, at.y)));
        }
        self.overlap.note(step, &pics, w.pos, &cast, rig.scale, self.size);
    }
}

/// How a survey takes its pictures and what it asks of the scene.
#[derive(Debug, Clone, Copy)]
enum Survey {
    /// The scene asks a question (the early bar, the flower seller): a picture every 20 of the first 100 steps at [`SIZE`], and the
    /// run must take a choice and the path `FMV_EXPLORATORY_MASK` asks for.
    Asks,
    /// The scene asks nothing (the two post-escape scenes): a picture every `every` steps through the WHOLE scene at `size`, with
    /// the scene's window on it while one is open (as the player sees it), the witness's picture measured against the cast's at
    /// every step, and the spot held to the walkmesh rule `tests/scene_real.rs` holds the proof scene's spots to.
    Quiet { every: usize, size: (u32, u32) },
}

/// A survey of the witness on a scene that asks a question (see [`Survey::Asks`]).
fn survey_witness(def: SceneDef, default: Spot, prefix: &str) {
    survey(def, default, prefix, Survey::Asks);
}

/// A survey of the witness on a scene that asks nothing (see [`Survey::Quiet`]): `FMV_EXPLORATORY_SPOT` replaces `default`,
/// `FMV_SHOT_DIR` is where the pictures go (one every two steps and one of each window as it opens, `<prefix>-<step>.png`,
/// 960 x 720).
fn survey_quiet_witness(def: SceneDef, default: Spot, prefix: &str) {
    survey(def, default, prefix, Survey::Quiet { every: 2, size: (960, 720) });
}

/// A survey, not visual acceptance: sample one witness's walkmesh distance to visible cast
/// while saving frames. Background occlusion, other bodies and the frames between pictures must
/// still be inspected. The registered flower scene has its own stronger all-branch test below.
fn survey(mut def: SceneDef, default: Spot, prefix: &str, mode: Survey) {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    def.witness = vec![std::env::var("FMV_EXPLORATORY_SPOT").ok().and_then(|s| parse_spot(&s)).unwrap_or(default)];
    let spot = def.witness[0];
    let path = matches!(mode, Survey::Quiet { .. }).then(|| scene_path(&real, &def));
    let mut rig = make_rig(&real, def, true);
    let msg = ServerMsg::SceneStart { presentation_origin: None,
        run: SceneRunId(1), scene: rig.def.id.clone(), seed: SEED, progress: rig.story.encode(), gil: 0, replay: None, owner: EntityId(1),
        participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 }], you: SceneRole::Owner,
    };
    assert!(rig.state.apply(0.0, msg).is_none());
    assert!(rig.state.scene.is_active());
    let mask = std::env::var("FMV_EXPLORATORY_MASK").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let (mut samples, mut actor_samples, mut nearest, mut choice_epoch, mut choice_index) = (0usize, 0usize, f32::MAX, None, 0usize);
    let mut quiet = match mode {
        Survey::Quiet { size, .. } => Some(Quiet { size, overlap: Overlap::new(), last: BTreeMap::new(), between: f32::MAX }),
        Survey::Asks => None,
    };
    // (a scene that asks a question is looked at for 400 steps at most; one that asks nothing is played to its end)
    let (cap, started, mut steps, mut pictures) = (if quiet.is_some() { 6000 } else { 400 }, std::time::Instant::now(), 0usize, 0usize);
    let mut last_window: Option<u32> = None;
    for step in 0..cap {
        if !rig.state.scene.is_active() { break; }
        steps = step + 1;
        if let Some(scene) = rig.state.scene.player() {
            if let Some(w) = scene.witnesses().first() {
                for actor in scene.cast_draws() { nearest = nearest.min(w.pos.dist_xy(actor.pos)); actor_samples += 1; }
                samples += 1;
            }
        }
        match (&mut quiet, mode) {
            (Some(quiet), Survey::Quiet { every, size }) => {
                quiet.step(&mut rig, step);
                // a picture every `every` steps, and one of each window as it opens (the window changes what the picture shows)
                let window = rig.state.scene.player().filter(|p| p.has_window()).map(|p| p.epoch());
                let opened = window.is_some() && window != last_window;
                last_window = window.or(last_window);
                if step % every == 0 || opened {
                    rig.picture(&mut gpu, &format!("{prefix}-{step:03}"), size, true);
                    pictures += 1;
                }
            }
            _ => {
                if step % 20 == 0 && step < 100 { rig.picture(&mut gpu, &format!("{prefix}-{}", step / 20), SIZE, false); }
            }
        }
        if rig.state.scene.player().is_some_and(|p| p.has_window()) {
            let selection = rig.state.scene.player().and_then(|p| p.panel().filter(|panel| panel.window().message.choice.is_some() && panel.on_choices(SIZE)).map(|_| p.epoch()));
            if let Some(epoch) = selection.filter(|e| Some(*e) != choice_epoch) {
                if mask.checked_shr(u32::try_from(choice_index).unwrap_or(u32::MAX)).unwrap_or(0) & 1 != 0 { rig.state.scene.key(KeyCode::ArrowDown, true, false); }
                choice_epoch = Some(epoch);
                choice_index += 1;
            }
            rig.state.scene.key(KeyCode::Enter, true, false);
        } else { rig.state.scene.advance(0.25); }
    }
    let commands = rig.state.scene.take_commands();
    let threshold = 2.0 * RING_RADIUS_UNITS * rig.scale;
    eprintln!("{prefix}: samples={samples} actor_samples={actor_samples} nearest={nearest:.1} threshold={threshold:.1} commands={commands:?}");
    // The numbers of a quiet survey are printed before anything is asserted, so that a spot that fails shows why. How much of each
    // placeholder body shows in front of the picture's foreground does not change over the scene (the camera and the spot are fixed).
    if let Survey::Quiet { size, .. } = mode {
        let shown: Vec<String> = PLACEHOLDER_BODIES.iter().map(|b| format!("{} {:.0}%", b.archetype, 100.0 * visible_fraction(&mut rig, &mut gpu, b.model.appearance_ref, &spot, size))).collect();
        eprintln!("{prefix}: the body shows in front of the picture's foreground: {}", shown.join(", "));
    }
    let walkmesh = quiet.as_ref().zip(path.as_ref()).map(|(quiet, path)| {
        let mesh = rig.play.ground().mesh();
        let (x, y) = (f32::from(spot.x), f32::from(spot.y));
        let on_path = path.iter().map(|&(_, px, py)| (px - x).hypot(py - y)).fold(f32::MAX, f32::min);
        let gap = boundary_gap(mesh, x, y);
        let inside = mesh.triangles.get(usize::from(spot.triangle)).is_some_and(|t| triangle_contains_xy(t, i32::from(spot.x), i32::from(spot.y)));
        let ring = RING_RADIUS_UNITS * rig.scale;
        let o = &quiet.overlap;
        let who = |e: Option<u8>| e.map_or_else(|| "?".to_string(), |e| e.to_string());
        eprintln!(
            "{prefix}: spot ({}, {}) triangle {} direction {} (inside its triangle: {inside}); {steps} steps, {pictures} pictures in {:.1} s",
            spot.x, spot.y, spot.triangle, spot.direction, started.elapsed().as_secs_f32()
        );
        eprintln!(
            "{prefix}: nearest cast over every place and walk of the scene {on_path:.1} (needs {:.1}); between samples {:.1}; boundary gap {gap:.1} (needs {:.1}); ring {ring:.1}",
            threshold + 16.0, quiet.between, 1.5 * ring
        );
        eprintln!(
            "{prefix}: witness picture found in {} of {} steps; a nearer model covers up to {:.0}% of it (step {}, entity {}); it covers up to {:.0}% of a farther model's (step {}, entity {}); {:.0} px from the screen's edge",
            o.found, o.steps, o.hidden.0 * 100.0, o.hidden.1, who(o.hidden.2), o.blocks.0 * 100.0, o.blocks.1, who(o.blocks.2), o.margin
        );
        // every step at which one of the two is more than the project's bound (10%), as step:percent:H(idden)/B(locks):entity
        let steps_over: std::collections::BTreeSet<usize> = o.over.iter().map(|e| e.0).collect();
        let list: Vec<String> = o.over.iter().map(|(s, c, hidden, e)| format!("{s}:{:.0}%:{}:{}", c * 100.0, if *hidden { "H" } else { "B" }, who(*e))).collect();
        eprintln!("{prefix}: {} of {} steps have a picture covered more than {:.0}%: {}", steps_over.len(), o.steps, MAX_COVERED * 100.0, list.join(" "));
        (on_path, gap, inside)
    });
    assert!(commands.iter().any(|c| matches!(c, super::SceneCmd::End { skipped: false, .. })), "scene must finish without skipping");
    assert!(samples > 0 && actor_samples > 0 && nearest >= threshold, "sampled cast must stay clear of the witness");
    if let (Some(quiet), Some((on_path, gap, inside))) = (&quiet, walkmesh) {
        let ring = RING_RADIUS_UNITS * rig.scale;
        assert_eq!(choice_index, 0, "the scene asks nothing");
        assert!(commands.iter().all(|c| !matches!(c, super::SceneCmd::Step { answer: Some(_), .. })), "and nothing is answered");
        assert!(inside, "the spot is inside the triangle it names");
        assert!(on_path >= threshold + 16.0, "the cast comes within {on_path:.1} of the witness, over every place and walk of the scene (needs {:.1})", threshold + 16.0);
        assert!(gap >= 1.5 * ring, "the witness stands {gap:.1} from the nearest boundary edge (needs {:.1})", 1.5 * ring);
        assert!(quiet.overlap.steps > 0 && quiet.overlap.found == quiet.overlap.steps, "the witness's picture was found at every step");
        return;
    }
    assert!(choice_index > 0, "the survey must exercise a choice");
    let answers: Vec<usize> = commands.iter().filter_map(|c| match c { super::SceneCmd::Step { answer: Some(n), .. } => Some(usize::from(*n)), _ => None }).collect();
    let expected = if prefix == "early-bar" { vec![1 + (mask & 1)] }
        else if mask & 1 == 0 { vec![2] }
        else { vec![3, 2 + ((mask >> 1) & 1), (mask >> 2) & 1] };
    assert_eq!(answers, expected, "the survey must take the requested path");
}

#[test]
#[ignore = "requires owned FF7 data and an offscreen GPU; exploratory early-bar staging"]
fn offscreen_survey_early_bar_witness() {
    survey_witness(fmv_ff7::script::scene::early_bar_scene(), Spot { x: 218, y: 292, triangle: 37, direction: 192 }, "early-bar");
}

#[test]
#[ignore = "requires owned FF7 data and an offscreen GPU; exploratory flower-scene staging"]
fn offscreen_survey_flower_witness() {
    survey_witness(fmv_ff7::script::scene::flower_station_scene(), Spot { x: -3785, y: 20901, triangle: 31, direction: 192 }, "flower");
}

/// Every authored branch of the flower encounter, through the real client stage, models, camera and dialogue UI.
/// The reference interpreter uses the same v3 story and explicit gil snapshot as SceneStart. Save every window for
/// visual review; assert its identity/choices, cast models, camera framing and witness clearance across the full paths.
#[test]
#[ignore = "needs the owner's FF7 data and an offscreen GPU; writes flower-path-*.png"]
fn offscreen_real_ff7_flower_choices_and_camera() {
    use crate::camera::FieldCamera;
    use crate::window::camera_input;
    use fmv_ff7::script::dialogue::End;
    use fmv_ff7::script::scene::{flower_station_scene, SceneResources};
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let area = real.adapter.load_field("md8_2").expect("the flower field");
    let story = StoryState::at(36).into_extended().expect("known-bit story");
    let gil = 1;
    let resources = SceneResources { gil: Some(gil) };
    let mut def = flower_station_scene();
    let moved = std::env::var_os("FMV_SCENE_SPOTS").is_some();
    if let Ok(spots) = std::env::var("FMV_SCENE_SPOTS") {
        def.witness = spots.split(';').filter_map(parse_spot).collect();
        assert!(!def.witness.is_empty());
    }
    eprintln!("flower witnesses: {:?}; automatic candidates: {:?}", def.witness,
        fmv_ff7::script::scene::auto_witnesses_with_resources(area.field_file(), &def, &story, SEED, resources, 3));
    let paths: &[(&str, &[usize], usize)] = &[
        ("leave", &[2], 3), ("buy", &[3, 2, 0], 7), ("ask-buy", &[3, 3, 0], 8),
        ("decline", &[3, 2, 1], 6), ("ask-decline", &[3, 3, 1], 7),
    ];
    let mut problems = Vec::new();
    for &(name, answers, windows) in paths {
        let mut reference = ScenePlay::with_resources(area.field_file(), &def, &story, SEED, resources).unwrap();
        let mut expected = Vec::new();
        let mut events = reference.initial_stage_events().to_vec();
        let (mut next, mut choice) = (None, 0usize);
        for _ in 0..64 {
            let beat = reference.next(next.take());
            events.extend(beat.stage.into_iter().map(|t| t.what));
            match beat.then {
                Then::End { how, .. } => { assert_eq!(how, End::Finished); break; }
                Then::Window(w) => {
                    if w.message.choice.is_some() { next = Some(answers[choice]); choice += 1; }
                    expected.push(w);
                }
            }
        }
        assert_eq!(expected.len(), windows, "{name}: authored window count");
        assert_eq!(choice, answers.len());
        assert_eq!(reference.off_cast(), 0, "{name}: every staged actor is declared");
        let path = cast_path(&events);
        if name == "buy" { eprintln!("flower authored animations: {:?}", events.iter().filter(|e| matches!(e, Stage::Anim { .. } | Stage::AnimSpeed { .. })).collect::<Vec<_>>()); }
        let mut rig = make_rig_with_snapshot(&real, def.clone(), moved, story.clone(), gil);
        for spot in &def.witness {
            let triangle = &rig.play.ground().mesh().triangles[usize::from(spot.triangle)];
            assert!(triangle_contains_xy(triangle, i32::from(spot.x), i32::from(spot.y)));
            let clearance = path.iter().map(|&(_, x, y)| (x - f32::from(spot.x)).hypot(y - f32::from(spot.y))).fold(f32::MAX, f32::min);
            let edge = boundary_gap(rig.play.ground().mesh(), f32::from(spot.x), f32::from(spot.y));
            assert!(edge >= 1.5 * RING_RADIUS_UNITS * rig.scale, "{name}: witness too close to floor edge ({edge:.1})");
            eprintln!("{name}: witness {spot:?} cast clearance {clearance:.1}, needs {:.1}", 2.0 * RING_RADIUS_UNITS * rig.scale + 16.0);
            if clearance < 2.0 * RING_RADIUS_UNITS * rig.scale + 16.0 { problems.push(format!("{name}: witness crosses a cast path ({clearance:.1})")); }
        }
        rig.start();
        let mut camera = FieldCamera::default();
        let (mut last_window, mut shown, mut answered) = (None, 0usize, 0usize);
        let (mut samples, mut models, mut seen) = (0usize, BTreeSet::new(), BTreeSet::new());
        let mut overlaps: Vec<_> = def.witness.iter().map(|_| Overlap::new()).collect();
        let mut dialogue_overlaps: Vec<_> = def.witness.iter().map(|_| Overlap::new()).collect();
        let mut foreground_checked = false;
        let mut last_counts = super::stage::Counts::default();
        for step in 0..2400usize {
            if !rig.state.scene.is_active() { break; }
            rig.draw(); // load each newly shown model before checking its anchor
            let draw = rig.draw();
            rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 0.25));
            if name == "buy" && matches!(step, 39 | 47) {
                rig.picture_clipped(&mut gpu, &format!("flower-entry-{step}"), SIZE, false, camera.clip_range(rig.view.range));
            }
            let player = rig.state.scene.player().unwrap();
            last_counts = player.counts();
            let casts = player.cast_draws();
            let built = draw.built.as_ref().expect("field models");
            let cast: Vec<_> = casts.iter().map(|c| (c.entity, c.pos)).collect();
            for c in &casts {
                seen.insert(c.entity);
                if built.anchors.contains_key(&cast_who(c.entity)) { models.insert(c.entity); }
                else { problems.push(format!("{name} step {step}: entity {} has no model", c.entity)); }
            }
            // Only overlap inside the viewport can hide a visible body. The opening authored
            // camera pan intentionally starts above the cast and witnesses; offscreen rectangles
            // crossing later do not cover any pixels of the actual picture.
            let pics: Vec<_> = rig.pics_of(built, SIZE).into_iter().filter_map(|mut p| {
                p.rect = (p.rect.0.max(0.0), p.rect.1.max(0.0), p.rect.2.min(SIZE.0 as f32), p.rect.3.min(SIZE.1 as f32));
                (p.rect.2 > p.rect.0 && p.rect.3 > p.rect.1).then_some(p)
            }).collect();
            for (index, w) in player.witnesses().iter().enumerate() {
                if let Some(overlap) = overlaps.get_mut(index) { overlap.note(step, &pics, w.pos, &cast, rig.scale, SIZE); }
                if player.has_window() {
                    if let Some(overlap) = dialogue_overlaps.get_mut(index) { overlap.note(step, &pics, w.pos, &cast, rig.scale, SIZE); }
                }
            }
            let window = player.panel().map(|p| (player.epoch(), p.window().clone(), p.on_choices(SIZE), p.position().1));
            if let Some((epoch, window, choices, cursor)) = window {
                if last_window != Some(epoch) {
                    let wanted = &expected[shown];
                    assert_eq!((&window.message.origin, window.message.choice, window.speaker), (&wanted.message.origin, wanted.message.choice, wanted.speaker), "{name} window {shown}");
                    let speaker = player.speaker();
                    eprintln!("{name} window {shown}: origin={:?} speaker={speaker:?}, cast={:?}, counts={last_counts:?}", window.message.origin, cast.iter().map(|c| c.0).collect::<Vec<_>>());
                    // The first line calls from offscreen before Aerith is shown. Every later
                    // spoken line has a visible actor; dialog 6 is the unlabelled purchase choice.
                    if speaker.is_some_and(|speaker| cast.iter().any(|c| c.0 == speaker)) && speaker_spot(player, Some(built), &rig.view, SIZE.0, SIZE.1).is_none() {
                        problems.push(format!("{name} window {shown}: speaker is not framed"));
                    }
                    if shown == 0 { assert_eq!((speaker, cast.iter().any(|c| c.0 == 2)), (Some(2), false)); }
                    if !foreground_checked && shown == 1 {
                        for (index, spot) in def.witness.iter().enumerate() {
                            let fraction = visible_fraction(&mut rig, &mut gpu, looks()[index], spot, SIZE);
                            eprintln!("{name}: witness {index} visible in foreground {:.1}%", fraction * 100.0);
                            if fraction < 0.9 { problems.push(format!("{name}: witness {index} is hidden by foreground ({fraction:.3})")); }
                        }
                        foreground_checked = true;
                    }
                    rig.picture_clipped(&mut gpu, &format!("flower-path-{name}-{shown:02}"), SIZE, true, camera.clip_range(rig.view.range));
                    shown += 1;
                    last_window = Some(epoch);
                }
                if choices {
                    let range = window.message.choice.unwrap();
                    let target = answers[answered] - range.first;
                    for _ in 0..cursor { rig.state.scene.key(KeyCode::ArrowUp, true, false); }
                    for _ in 0..target { rig.state.scene.key(KeyCode::ArrowDown, true, false); }
                    answered += 1;
                }
                rig.state.scene.key(KeyCode::Enter, true, false);
            } else {
                rig.state.scene.advance(0.25);
            }
            samples += 1;
        }
        let commands = rig.state.scene.take_commands();
        let sent: Vec<_> = commands.iter().filter_map(|c| match c { super::SceneCmd::Step { answer: Some(a), .. } => Some(usize::from(*a)), _ => None }).collect();
        assert_eq!(sent, answers, "{name}: actual UI choices");
        assert_eq!(shown, windows, "{name}: every authored window was displayed");
        assert!(commands.iter().any(|c| matches!(c, super::SceneCmd::End { skipped: false, .. })), "{name}: finishes normally");
        assert_eq!(seen, models, "{name}: every visible actor loads");
        assert_eq!(models, BTreeSet::from([1, 2]), "{name}: Cloud and the flower seller are the shown cast");
        assert_eq!(last_counts.anims_skipped, 0, "{name}: every authored animation is presented");
        for (index, overlap) in overlaps.iter().enumerate() {
            eprintln!("{name}: witness {index} visible {} of {samples} frames; hidden {:.1}%, blocks {:.1}%, worst step {}/{}", overlap.found, overlap.hidden.0 * 100.0, overlap.blocks.0 * 100.0, overlap.hidden.1, overlap.blocks.1);
            // Cloud's authored entrance crosses in front of the two watchers, which is natural
            // depth occlusion (reviewed flower-entry-39/47.png). They never obscure her; every
            // witness is clear during dialogue, and the owner's view is clear throughout.
            let speaking = &dialogue_overlaps[index];
            if overlap.blocks.0 > MAX_COVERED || (index == 0 && overlap.hidden.0 > MAX_COVERED)
                || speaking.hidden.0 > MAX_COVERED || speaking.blocks.0 > MAX_COVERED {
                problems.push(format!("{name}: witness {index} obstructs presentation (all {:.3}/{:.3}, dialogue {:.3}/{:.3})", overlap.hidden.0, overlap.blocks.0, speaking.hidden.0, speaking.blocks.0));
            }
        }
        eprintln!("{name}: {samples} samples, {shown} windows, actors {models:?}, last counts {last_counts:?}");
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The tunnel mouth `md8_4`. NO spot of its floor clears every bar (surveyed over the whole floor: 621 places): the only floor
/// that shows in front of the picture's foreground is the narrow ledge the cast stands and walks on, and the camera looks along
/// it, so a witness anywhere on it is drawn among the cast. This default is the least bad place that keeps the walkmesh rule
/// (60 from every place the cast takes, 33 from a boundary edge): the far left end of the ledge, which shows nearly all of the
/// body but has the nearest cast member's picture over most of it, and the witness's over a part of the next one's, in
/// nearly every step. The data's own spot, (-133, 144, 0), keeps the rule but is behind the foreground (about 10% of the body shows).
#[test]
#[ignore = "requires owned FF7 data and an offscreen GPU; exploratory post-escape (md8_4) staging"]
fn offscreen_survey_post_escape_witness() {
    survey_quiet_witness(fmv_ff7::script::scene::post_escape_scene(), Spot { x: -114, y: 260, triangle: 2, direction: 102 }, "post-escape");
}

/// The street `md8_1`. The left end of the walkable strip, left of where the cast talks (about (1347, -5270)) and facing it:
/// the whole body shows in front of the foreground, no picture is covered while a window is open, and four of the 105 steps have
/// a picture covered more than 10% (the cast's Wedge, entity 5, runs by in front of it).
#[test]
#[ignore = "requires owned FF7 data and an offscreen GPU; exploratory post-escape station (md8_1) staging"]
fn offscreen_survey_station_witness() {
    survey_quiet_witness(fmv_ff7::script::scene::station_scene(), Spot { x: 1074, y: -5183, triangle: 28, direction: 52 }, "station");
}

/// The window's field camera ([`crate::camera`]) over the owner's `md1_1`, through the window's own path
/// (`field_draw`, `camera_input`, `FieldCamera::update`, then the frame): FF7's 240-unit screen, as wide as the 1100 x 680
/// window allows (388 units), following the player where the team-talk preview stands it and at the walkable place inside
/// the range nearest to its top right, then the team talk's own opening scroll (`SCR2DC -96,60` over 32 frames),
/// which brings the team into view. Writes `camera-player.png`, `camera-corner.png` and `camera-team-talk.png` and checks
/// where each view is; look at the pictures (the models' size against the background, nothing cut at the range's edge).
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_the_field_camera() {
    use crate::camera::{clamp_centre, scroll_centre, view_size, FieldCamera};
    use crate::window::camera_input;
    use fmv_adapter_api::Bounds;
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let mut rig = make_rig(&real, fmv_ff7::script::scene::team_talk_scene(), true);
    let range = rig.view.range.expect("md1_1 has a camera range");
    let size_units = view_size(&range, SIZE.0 as f32 / SIZE.1 as f32);
    assert_eq!(size_units, [388.0, 240.0], "FF7's 240 units from the window's top to its bottom, as wide as 1100 x 680");
    let mut camera = FieldCamera::default();
    let mut shoot = |rig: &mut Rig, camera: &mut FieldCamera, name: &str| -> Bounds {
        rig.draw();
        let draw = rig.draw();
        rig.view.bounds = camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 0.0));
        let shown = rig.view.bounds;
        rig.picture_clipped(&mut gpu, name, SIZE, true, camera.clip_range(rig.view.range));
        eprintln!("{name}: shows {:.0}..{:.0} x {:.0}..{:.0} of the range {:.0}..{:.0} x {:.0}..{:.0}", shown.min.x, shown.max.x, shown.min.y, shown.max.y, range.min.x, range.max.x, range.min.y, range.max.y);
        shown
    };
    let centre = |b: &Bounds| [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0];
    // What the whole window shows (the renderer's fit of the given rectangle, margins and all) is inside the range.
    let inside = |b: &Bounds| {
        let fit = crate::render::Camera::fit(b, SIZE.0 as f32, SIZE.1 as f32, crate::render::MARGIN_PX);
        let [cx, cy] = centre(b);
        let (hw, hh) = (SIZE.0 as f32 / 2.0 / fit.pixels_per_unit(), SIZE.1 as f32 / 2.0 / fit.pixels_per_unit());
        cx - hw >= range.min.x - 0.5 && cx + hw <= range.max.x + 0.5 && cy - hh >= range.min.y - 0.5 && cy + hh <= range.max.y + 0.5
    };
    let expect = |p: Pos| { let c = clamp_centre([p.x, p.y], &range, size_units); [c[0].round(), c[1].round()] };
    let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01;
    // 1. Where the preview stands the player.
    let own = rig.view.to_view(rig.state.own.pos()).expect("the player is in front of the camera");
    eprintln!("the player stands at view ({:.0}, {:.0})", own.x, own.y);
    let shown = shoot(&mut rig, &mut camera, "camera-player");
    assert!(near(centre(&shown), expect(own)), "{:?} {:?}", centre(&shown), expect(own));
    assert!(inside(&shown), "never past the camera range");
    // 2. The walkable place nearest to the range's top right: the view stops at the range's edges.
    let area = real.adapter.load_field("md1_1").expect("md1_1");
    let corner = area.walk_triangles().iter()
        .map(|t| Pos::new((t[0].x + t[1].x + t[2].x) / 3.0, (t[0].y + t[1].y + t[2].y) / 3.0, (t[0].z + t[1].z + t[2].z) / 3.0))
        .filter_map(|p| rig.view.to_view(p).map(|v| (p, v)))
        // (the walkmesh runs on past the range into the way out, where FF7's screen leaves the player behind its edge)
        .filter(|(_, v)| v.x < range.max.x && v.y < range.max.y && v.x > range.min.x && v.y > range.min.y)
        .max_by(|a, b| (a.1.x + a.1.y).total_cmp(&(b.1.x + b.1.y)))
        .expect("a walkable place");
    let area_id = AreaId { game: GameId::Ff7, key: "md1_1".to_string() };
    rig.state.change_area(area_id, corner.0, 0.0, false);
    camera.snap();
    let own = rig.view.to_view(rig.state.own.pos()).expect("the player is in front of the camera");
    eprintln!("the corner place is at view ({:.0}, {:.0}) (world {:?})", own.x, own.y, corner.0);
    let shown = shoot(&mut rig, &mut camera, "camera-corner");
    assert!(near(centre(&shown), expect(own)), "{:?} {:?}", centre(&shown), expect(own));
    assert!(inside(&shown), "never past the camera range");
    // 3. The team talk: its opening scroll, once its 32 frames have run on the scene's clock.
    rig.start();
    for _ in 0..400 {
        let clock = rig.state.scene.player().map(|p| p.camera_clock());
        if clock.is_some_and(|c| c.events > 0 && c.frames >= 32.0) { break; }
        if rig.state.scene.player().is_some_and(|p| p.has_window()) { rig.state.scene.key(KeyCode::Enter, true, false); } else { rig.state.scene.advance(0.05); }
        let draw = rig.draw();
        camera.update(&camera_input(&rig.view, &rig.state, &draw, SIZE, 0.05));
    }
    let clock = rig.state.scene.player().expect("the scene still runs").camera_clock();
    assert!(clock.events > 0 && clock.frames >= 32.0, "the scroll ran: {clock:?}");
    let shown = shoot(&mut rig, &mut camera, "camera-team-talk");
    let target = clamp_centre(scroll_centre(-96, 60), &range, size_units);
    assert!(near(centre(&shown), [target[0].round(), target[1].round()]), "the scroll's place, kept inside the range: {:?}", centre(&shown));
    assert!(inside(&shown));
}

/// Every registered non-battle scene of the route over the owner's data, played through the window's own path with the field camera
/// (`field_draw`, `camera_input`, `FieldCamera::update`) at four steps a second, each at the story it plays at: at every step
/// the window shows the scene's lead (the canon Cloud when he is drawn, else the player) and any actor a `SCRLA` follows.
/// The half second the camera eases in after the scene starts or its camera changes is left out, and so is a lead outside
/// the field's camera range (FF7's screen does not show that place either). Prints each scene's count and writes one
/// picture of its middle step (`camera-scene-<field>-<name>.png`); fails if any step has the lead out of the window.
/// The bridge and cargo door need an accepted crossing, covered by their dedicated tests;
/// this general scene walk does not invent an origin or battle acknowledgements.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_the_field_camera_keeps_each_route_scenes_lead_in_view() {
    use crate::camera::{FieldCamera, SceneAim};
    use crate::render::{Camera, MARGIN_PX};
    use crate::window::camera_input;
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("software or physical GPU");
    let story_for = |id: &str| -> StoryState {
        if id == "ff7/md1stin/station-guards" { return StoryState::fresh_first_control(); }
        let (moment, bit): (u16, Option<(u8, u8, u8)>) = match id {
            "ff7/md8_4/post-escape" | "ff7/md8_1/post-escape-station" => (33, None),
            "ff7/md1_1/team-talk" => (6, None),
            "ff7/nmkin_1/reactor-door" => (10, None),
            "ff7/nmkin_5/guard-scorpion" | "ff7/nmkin_5/guard-scorpion-victory" => (26, None),
            "ff7/nmkin_3/jessie-rescue" => (27, Some((1, 225, 0))),
            "ff7/nmkin_3/jessie-ladder" => (13, None),
            "ff7/md1_2/run-ahead" => (7, None),
            "ff7/nmkin_2/jessie-descent" => (12, None),
            "ff7/nrthmk/reactor-approach" => (8, None),
            "ff7/nrthmk/reactor-finale" => (27, Some((1, 225, 5))),
            "ff7/md8_2/flower" => (36, None),
            fmv_ff7::script::scene::CARGO_TRAIN_SCENE_ID => (39, None),
            fmv_ff7::script::scene::TRAIN_SCENE_ID => (48, None),
            _ => (0, None),
        };
        let mut story = StoryState::at(moment);
        if let Some((bank, address, bit)) = bit { story.set_bit(bank, address, bit, true); }
        story
    };
    let mut failures = Vec::new();
    for def in fmv_ff7::script::scene::scenes().into_iter().filter(|d|
        ![fmv_ff7::script::scene::PROOF_SCENE_ID, fmv_ff7::script::scene::BRIDGE_SCENE_ID,
            fmv_ff7::script::scene::CARGO_DOOR_SCENE_ID].contains(&d.id.as_str())) {
        let id = def.id.clone();
        let mut rig = make_rig(&real, def, true);
        rig.story = story_for(&id);
        let Some(range) = rig.view.range else { failures.push(format!("{id}: no camera range")); continue };
        rig.start();
        let mut camera = FieldCamera::default();
        let (mut steps, mut checked, mut out, mut since_change, mut last) = (0usize, 0usize, Vec::new(), 0usize, None);
        let mut middle: Option<usize> = None;
        for step in 0..2400usize {
            if !rig.state.scene.is_active() { break; }
            let draw = rig.draw();
            let input = camera_input(&rig.view, &rig.state, &draw, SIZE, 0.25);
            let shown = camera.update(&input);
            let key = input.scene.map(|s| (s.events, std::mem::discriminant(&s.aim)));
            if key != last { since_change = 0; last = key; } else { since_change += 1; }
            let subject = input.scene.and_then(|s| match s.aim {
                SceneAim::Lead { lead } => lead,
                SceneAim::Follow { at, lead } => at.or(lead),
                SceneAim::Scroll { .. } => None,
            });
            if let (Some(p), true) = (subject, since_change >= 2) {
                let in_range = p[0] >= range.min.x && p[0] <= range.max.x && p[1] >= range.min.y && p[1] <= range.max.y;
                if in_range {
                    checked += 1;
                    let fit = Camera::fit(&shown, SIZE.0 as f32, SIZE.1 as f32, MARGIN_PX);
                    let (hw, hh) = (SIZE.0 as f32 / 2.0 / fit.pixels_per_unit(), SIZE.1 as f32 / 2.0 / fit.pixels_per_unit());
                    let (cx, cy) = ((shown.min.x + shown.max.x) / 2.0, (shown.min.y + shown.max.y) / 2.0);
                    if (p[0] - cx).abs() > hw + 1.0 || (p[1] - cy).abs() > hh + 1.0 { out.push(step); }
                }
            }
            steps += 1;
            if middle.is_none() && step >= 8 && subject.is_some() {
                middle = Some(step);
                rig.view.bounds = shown;
                let name = id.replace("ff7/", "").replace('/', "-");
                rig.picture_clipped(&mut gpu, &format!("camera-scene-{name}"), SIZE, true, camera.clip_range(rig.view.range));
            }
            if rig.state.scene.player().is_some_and(|p| p.has_window()) { rig.state.scene.key(KeyCode::Enter, true, false); } else { rig.state.scene.advance(0.25); }
        }
        eprintln!("{id}: {steps} steps, the lead checked at {checked}, out of the window at {} {:?}", out.len(), &out[..out.len().min(12)]);
        if !out.is_empty() { failures.push(format!("{id}: lead out of the window at {} steps", out.len())); }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

impl Rig {
    /// The server starts the scene for the owner and two watchers: entities 1, 2 and 3 on witness spots 0, 1 and 2.
    fn start(&mut self) {
        let participants = (0..3u8).map(|spot| SceneParticipant { entity: EntityId(u64::from(spot) + 1), spot }).collect();
        let presentation_origin = self.def.requires_presentation_origin().then(|| fmv_protocol::SceneOrigin {
            pos: self.state.own.pos(), facing: self.state.own.facing(), triangle: Some(u32::from(self.def.arrival.triangle)),
        });
        let msg = ServerMsg::SceneStart {
            presentation_origin,
            run: SceneRunId(1),
            scene: self.def.id.clone(),
            seed: SEED,
            progress: self.story.encode(),
            gil: self.gil,
            replay: None,
            owner: EntityId(1),
            participants,
            you: SceneRole::Owner,
        };
        assert!(self.state.apply(0.0, msg).is_none());
        assert!(self.state.scene.is_active(), "the scene was built from the owner's own field");
    }

    /// One frame of the field as the window builds it (the first call loads the models the field names).
    fn draw(&mut self) -> FieldDraw {
        self.view.apply_background_events(self.state.scene.drain_background());
        field_draw(Some(&mut self.avatars), &mut self.npc_epoch, &self.view, &self.state, self.own_tint, 1.0, 0.0)
    }

    /// The drawn models by look: how tall they are and where their feet are.
    fn bodies_of(&self, built: &Drawn) -> BTreeMap<String, Body> {
        let camera = self.view.stage.as_ref().expect("stage").camera.matrix();
        built
            .frame
            .models
            .iter()
            .map(|m| {
                let mut used = vec![false; m.positions.len()];
                for &i in &m.asset.mesh.indices {
                    used[i as usize] = true;
                }
                let world: Vec<[f32; 3]> = m.positions.iter().zip(&used).filter(|(_, u)| **u).map(|(p, _)| to_world(&camera, *p)).collect();
                let (lo, hi) = world.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[2]), hi.max(p[2])));
                let feet: Vec<&[f32; 3]> = world.iter().filter(|p| p[2] <= lo + 1.5 * self.scale).collect();
                let n = feet.len().max(1) as f32;
                let at = (feet.iter().map(|p| p[0]).sum::<f32>() / n, feet.iter().map(|p| p[1]).sum::<f32>() / n);
                (m.asset.appearance.appearance_ref.to_string(), Body { height: hi - lo, head: m.asset.height_units * self.scale, feet: at })
            })
            .collect()
    }

    fn bodies(&self, draw: &FieldDraw) -> BTreeMap<String, Body> {
        self.bodies_of(draw.built.as_ref().expect("the models were built"))
    }

    /// Where each drawn model's picture is on a `w` x `h` screen, as the rectangle its skinned vertices fill, in window pixels.
    fn boxes_of(&self, built: &Drawn, (w, h): (u32, u32)) -> BTreeMap<String, Rect> {
        let camera = self.view.stage.as_ref().expect("stage").camera.matrix();
        built
            .frame
            .models
            .iter()
            .map(|m| {
                let mut used = vec![false; m.positions.len()];
                for &i in &m.asset.mesh.indices {
                    used[i as usize] = true;
                }
                let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for (p, _) in m.positions.iter().zip(&used).filter(|(_, u)| **u) {
                    let world = to_world(&camera, *p);
                    if let Some(v) = self.view.to_view(Pos::new(world[0], world[1], world[2])) {
                        let px = view_to_window(&self.view.bounds, w, h, v.x, v.y);
                        b = (b.0.min(px[0]), b.1.min(px[1]), b.2.max(px[0]), b.3.max(px[1]));
                    }
                }
                (m.asset.appearance.appearance_ref.to_string(), b)
            })
            .collect()
    }

    /// Every model of a frame as a [`Pic`] on a `w` x `h` screen, in the order the frame holds them.
    fn pics_of(&self, built: &Drawn, (w, h): (u32, u32)) -> Vec<Pic> {
        let camera = self.view.stage.as_ref().expect("stage").camera.matrix();
        built
            .frame
            .models
            .iter()
            .filter_map(|m| {
                let mut used = vec![false; m.positions.len()];
                for &i in &m.asset.mesh.indices {
                    used[i as usize] = true;
                }
                // (each drawn vertex in camera space and in the field)
                let drawn: Vec<([f32; 3], [f32; 3])> = m.positions.iter().zip(&used).filter(|(_, u)| **u).map(|(p, _)| (*p, to_world(&camera, *p))).collect();
                let lowest = drawn.iter().map(|(_, field)| field[2]).fold(f32::MAX, f32::min);
                let feet: Vec<&([f32; 3], [f32; 3])> = drawn.iter().filter(|(_, field)| field[2] <= lowest + 1.5 * self.scale).collect();
                let n = feet.len() as f32;
                if feet.is_empty() {
                    return None;
                }
                let at = (feet.iter().map(|(_, f)| f[0]).sum::<f32>() / n, feet.iter().map(|(_, f)| f[1]).sum::<f32>() / n);
                let depth = feet.iter().map(|(c, _)| c[2]).sum::<f32>() / n;
                let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for (_, field) in &drawn {
                    if let Some(v) = self.view.to_view(Pos::new(field[0], field[1], field[2])) {
                        let px = view_to_window(&self.view.bounds, w, h, v.x, v.y);
                        b = (b.0.min(px[0]), b.1.min(px[1]), b.2.max(px[0]), b.3.max(px[1]));
                    }
                }
                (b.0 <= b.2 && b.1 <= b.3).then_some(Pic { feet: at, depth, rect: b })
            })
            .collect()
    }

    /// Measure one frame of the scene and hold it to the acceptance asserts; `label` says which frame in a failure.
    fn sample(&mut self, label: &str) {
        let draw = self.draw();
        let built = draw.built.as_ref().expect("the models were built");
        let bodies = self.bodies_of(built);
        let boxes = self.boxes_of(built, SIZE);
        let looks = self.looks;
        // (the field, not a method: the report below is written while the scene is still read)
        let scene = self.state.scene.player().expect("a scene is on show");
        let witnesses = scene.witnesses();
        let cast = scene.cast_draws();
        let min_apart = 2.0 * RING_RADIUS_UNITS * self.scale;
        let mut problems: Vec<(&str, String)> = Vec::new();
        let mut least = f32::MAX;
        // (1) the people of the scene stand clear of each other: every witness of every cast actor and of every other witness
        for (i, w) in witnesses.iter().enumerate() {
            for c in &cast {
                let apart = c.pos.dist_xy(w.pos);
                least = least.min(apart);
                if apart < min_apart {
                    problems.push(("separation", format!("{label}: witness {i} is {apart:.1} from the actor of entity {}: less than {min_apart:.1}", c.entity)));
                }
            }
            for (j, other) in witnesses.iter().enumerate().skip(i + 1) {
                let apart = other.pos.dist_xy(w.pos);
                least = least.min(apart);
                if apart < min_apart {
                    problems.push(("separation", format!("{label}: witness {i} is {apart:.1} from witness {j}: less than {min_apart:.1}")));
                }
            }
        }
        // (2) every witness is drawn through the same pipeline and scale as the cast, and is neither a dwarf nor a giant next to them
        let cast_heights: Vec<f32> = ["ff7/cloud", "ff7/tifa"].iter().filter_map(|k| bodies.get(*k)).map(|b| b.height).collect();
        let (shortest, tallest) = (cast_heights.iter().copied().fold(f32::MAX, f32::min), cast_heights.iter().copied().fold(f32::MIN, f32::max));
        for (slot, key) in ["ff7/cloud", "ff7/tifa"].iter().enumerate() {
            if bodies.contains_key(*key) {
                self.report.seen[slot] += 1;
            }
        }
        for (i, look) in looks.iter().enumerate() {
            let Some(body) = bodies.get(*look) else {
                self.report.problem("witness", format!("{label}: the body of witness {i} ({look}) was not drawn"));
                continue;
            };
            if !cast_heights.is_empty() && !(0.9 * shortest..=1.1 * tallest).contains(&body.height) {
                problems.push(("height", format!("{label}: witness {i} ({look}) is {:.1} tall, the cast {shortest:.1} to {tallest:.1}: outside 0.9 x the shortest to 1.1 x the tallest", body.height)));
            }
            for (key, slot) in [("ff7/cloud", 0usize), ("ff7/tifa", 1)] {
                if let Some(other) = bodies.get(key) {
                    let ratio = body.height / other.height;
                    Report::note_ratio(if slot == 0 { &mut self.report.ratio_cloud[i] } else { &mut self.report.ratio_tifa[i] }, ratio);
                }
            }
        }
        // (3) drawn where the stage stands them: the feet of each model are at its place
        let mut worst = 0.0f32;
        for (i, w) in witnesses.iter().enumerate() {
            if let Some(body) = looks.get(i).and_then(|look| bodies.get(*look)) {
                worst = worst.max((body.feet.0 - w.pos.x).hypot(body.feet.1 - w.pos.y));
            }
        }
        for c in &cast {
            let key = match c.entity {
                18 => "ff7/cloud",
                20 => "ff7/tifa",
                _ => continue,
            };
            if let Some(body) = bodies.get(key) {
                worst = worst.max((body.feet.0 - c.pos.x).hypot(body.feet.1 - c.pos.y));
            }
        }
        if worst > 4.0 * self.scale {
            problems.push(("feet", format!("{label}: a model's feet are {worst:.1} units from where the stage stands it")));
        }
        // (4) on the screen: how much of each witness's picture is covered by the cast's and by the other witnesses'
        let mut named: Vec<(String, Option<Rect>)> = vec![("cloud".into(), boxes.get("ff7/cloud").copied()), ("tifa".into(), boxes.get("ff7/tifa").copied())];
        named.extend(looks.iter().enumerate().map(|(j, look)| (SPOT_NAMES[j].to_string(), boxes.get(*look).copied())));
        for (i, look) in looks.iter().enumerate() {
            let Some(mine) = boxes.get(*look).copied() else { continue };
            for (name, other) in &named {
                let Some(other) = other else { continue };
                if *name == SPOT_NAMES[i] {
                    continue;
                }
                let c = covered(mine, *other);
                let key = format!("{} by {name}", SPOT_NAMES[i]);
                let max = self.report.covers.entry(key).or_insert(0.0);
                *max = max.max(c);
                if c > MAX_COVERED {
                    problems.push(("covered", format!("{label}: {:.0}% of witness {i}'s picture is covered by {name}'s: more than {:.0}%", c * 100.0, MAX_COVERED * 100.0)));
                }
            }
        }
        let stands = |entity: u8| cast.iter().find(|c| c.entity == entity).map(|c| c.pos);
        self.report.cast.push(CastFrame { cloud: boxes.get("ff7/cloud").copied(), tifa: boxes.get("ff7/tifa").copied(), at: [stands(18), stands(20)] });
        for (kind, text) in problems {
            self.report.problem(kind, text);
        }
        self.report.max_feet_error = self.report.max_feet_error.max(worst);
        self.report.min_separation = self.report.min_separation.min(least);
        self.report.frames += 1;
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            name: "Wedge".into(),
            tint: self.own_tint,
            area_id: AreaId { game: GameId::Ff7, key: self.def.field.clone() },
            fixture: false,
            card: fmv_protocol::CharacterCard::plain("Wedge", self.own_tint),
        }
    }

    /// The field as the window draws it, with the scene's window over it when `window`, saved as `<name>.png`.
    fn picture(&mut self, gpu: &mut Offscreen, name: &str, (w, h): (u32, u32), window: bool) -> (Vec<u8>, FieldDraw) {
        self.picture_clipped(gpu, name, (w, h), window, None)
    }

    /// The follow camera also clips actors outside the authored range. Overview surveys pass
    /// no range, matching F2; their pictures keep the full field and its actors.
    fn picture_clipped(&mut self, gpu: &mut Offscreen, name: &str, (w, h): (u32, u32), window: bool,
        range: Option<fmv_adapter_api::Bounds>) -> (Vec<u8>, FieldDraw) {
        let fade = self.state.scene.fade();
        self.picture_with_fade(gpu, name, (w, h), window, range, fade)
    }

    /// The same owned frame with a selected fade, used to compare the field and unchanged HUD.
    fn picture_with_fade(&mut self, gpu: &mut Offscreen, name: &str, (w, h): (u32, u32), window: bool,
        range: Option<fmv_adapter_api::Bounds>, fade: Option<fmv_ff7::script::scene::FadeColor>) -> (Vec<u8>, FieldDraw) {
        // The first frame loads the models the field names; the second is the steady state.
        self.draw();
        let draw = self.draw();
        let built = draw.built.as_ref().expect("the models were built");
        let scene = Scene { bounds: self.view.bounds, triangles: &[], players: &draw.markers, fixture_banner: false, backdrop: self.view.backdrop.as_ref() };
        let story = self.state.scene.player();
        let anchor = |subject: Subject, at: Pos| if label_hidden(story, subject, self.state.npc_views())
            || super::draw::participant_label_hidden(story, self.state.self_entity(), subject) { None }
            else { draw.label_anchor(&self.view, subject, at) };
        let mut hud_input = hud::gather_anchored(&self.state, &self.info(), &self.view, 1.0, &anchor);
        if let Some(story) = story {
            let mut labels = super::draw::cast_labels(story, draw.built.as_ref(), &self.view);
            labels.append(&mut hud_input.labels);
            hud_input.labels = labels;
        }
        let mut overlay = DrawList::new();
        hud::draw_hud(&hud_input, w, h, &mut overlay);
        if let (true, Some(story)) = (window, story) {
            let spot = speaker_spot(story, draw.built.as_ref(), &self.view, w, h);
            story.draw(w, h, 0.0, spot.as_ref(), &mut overlay);
        }
        let img = gpu.render_field_effects(w, h, &scene, Some(&built.frame), &overlay, range, fade);
        save(name, w, h, &img);
        (img, draw)
    }

    /// Where the head and the feet of the people of the picture are on a `w` x `h` screen, in window pixels, by name: the
    /// people of the scene while it runs, else the owner's own body and the trigger.
    fn pixels(&self, draw: &FieldDraw, (w, h): (u32, u32)) -> BTreeMap<&'static str, [(f32, f32); 2]> {
        let built = draw.built.as_ref().expect("built");
        let mut out = BTreeMap::new();
        let mut put = |name: &'static str, who: Who, feet: Pos| {
            if let (Some(head), Some(foot)) = (built.anchors.get(&who), self.view.to_view(feet)) {
                let (a, b) = (view_to_window(&self.view.bounds, w, h, head.x, head.y), view_to_window(&self.view.bounds, w, h, foot.x, foot.y));
                out.insert(name, [(a[0], a[1]), (b[0], b[1])]);
            }
        };
        match self.state.scene.player() {
            Some(scene) => {
                for (i, witness) in scene.witnesses().iter().enumerate().take(3) {
                    put(SPOT_NAMES[i], Who(witness.entity.0), witness.pos);
                }
                for c in scene.cast_draws() {
                    match c.entity {
                        18 => put("cloud", cast_who(18), c.pos),
                        20 => put("tifa", cast_who(20), c.pos),
                        _ => {}
                    }
                }
            }
            None => {
                put("own", Who(1), self.own_real);
                if let Some(tifa) = self.state.npc_views().iter().find(|n| n.id == SCENE_NPC_ID_BASE) {
                    put("tifa", npc_who(SCENE_NPC_ID_BASE), tifa.pos);
                }
            }
        }
        out
    }

    /// The rectangle (x0, y0, x1, y1) of the scene's window on a `w` x `h` screen, and what it covers of the people.
    fn window_cover(&self, draw: &FieldDraw, size: (u32, u32)) -> ((i32, i32, i32, i32), Vec<&'static str>) {
        let scene = self.state.scene.player().expect("a scene is on show");
        let spot = speaker_spot(scene, draw.built.as_ref(), &self.view, size.0, size.1);
        let mut list = DrawList::new();
        scene.draw(size.0, size.1, 0.0, spot.as_ref(), &mut list);
        let q = list.quads()[0];
        let rect = (q.x, q.y, q.x + q.w, q.y + q.h);
        let inside = |p: (f32, f32)| p.0 >= rect.0 as f32 && p.0 <= rect.2 as f32 && p.1 >= rect.1 as f32 && p.1 <= rect.3 as f32;
        let people = self.pixels(draw, size).into_iter().filter(|(_, pts)| pts.iter().any(|p| inside(*p))).map(|(name, _)| name).collect();
        (rect, people)
    }
}

/// A 3x close-up of the part of `img` (`w` wide) that holds the people of the picture, saved as `<name>.png`.
fn close_up(rig: &Rig, draw: &FieldDraw, img: &[u8], size: (u32, u32), name: &str) {
    let (w, h) = size;
    let pts: Vec<(f32, f32)> = rig.pixels(draw, size).values().flatten().copied().collect();
    if pts.is_empty() {
        return;
    }
    let pad = 90.0 * (w as f32 / 1100.0).max(1.0);
    let x0 = (pts.iter().map(|p| p.0).fold(f32::MAX, f32::min) - pad).max(0.0) as u32;
    let y0 = (pts.iter().map(|p| p.1).fold(f32::MAX, f32::min) - pad * 2.0).max(0.0) as u32;
    let x1 = ((pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) + pad) as u32).min(w - 1);
    let y1 = ((pts.iter().map(|p| p.1).fold(f32::MIN, f32::max) + pad) as u32).min(h - 1);
    let (cw, ch) = ((x1 - x0).max(8), (y1 - y0).max(8));
    let zoom = (1600 / cw).clamp(1, 3);
    let mut out = Vec::with_capacity((cw * ch * zoom * zoom * 4) as usize);
    for y in 0..ch * zoom {
        for x in 0..cw * zoom {
            let at = (((y0 + y / zoom).min(h - 1) * w + (x0 + x / zoom).min(w - 1)) * 4) as usize;
            out.extend_from_slice(&img[at..at + 4]);
        }
    }
    save(name, cw * zoom, ch * zoom, &out);
}

/// How many pixels of `img` are the window's light border or bright text white (a picture that came out empty is caught).
fn bright(img: &[u8]) -> usize {
    img.chunks(4).filter(|p| p[0] > 230 && p[1] > 230 && p[2] > 240).count()
}

/// Play the whole scene as its owner on `rig` (a picture of the first of each kind of window, with `prefix` in the names, a
/// measurement of every window and of every quarter second between them). Returns what the windows were: (opener, speaker,
/// asks).
fn play(rig: &mut Rig, gpu: &mut Offscreen, prefix: &str) -> Vec<(u8, Option<u8>, bool)> {
    let (mut tifa, mut cloud, mut asked, mut first_window) = (false, false, false, false);
    let mut spoke: Vec<(u8, Option<u8>, bool)> = Vec::new();
    let mut guard = 0;
    while rig.state.scene.is_active() {
        guard += 1;
        assert!(guard < 6000, "the scene must end");
        let Some(player) = rig.state.scene.player() else { break };
        if !player.has_window() {
            rig.state.scene.advance(0.25);
            if rig.state.scene.is_active() {
                rig.sample("between windows");
            }
            continue;
        }
        // The keys lay the window out for the screen it was last drawn for: this one.
        player.draw(SIZE.0, SIZE.1, 0.0, None, &mut DrawList::new());
        let (speaker, asks, epoch) = (player.speaker(), player.panel().is_some_and(|p| p.asks()), player.epoch());
        spoke.push((player.panel().map_or(0, |p| p.window().opener), speaker, asks));
        rig.sample("a window");
        // A question is shown on the page that holds its choices (the pages before it are the question's own text).
        if asks {
            let mut turned = 0;
            while rig.state.scene.player().and_then(|p| p.panel()).is_some_and(|p| !p.on_choices(SIZE)) && turned < 20 {
                rig.state.scene.key(KeyCode::Enter, true, false);
                turned += 1;
            }
        }
        let wants = if !first_window {
            Some("01-first-window")
        } else if speaker == Some(20) && !tifa && !asks {
            tifa = true;
            Some("02-window-over-tifa")
        } else if speaker == Some(18) && !cloud && !asks {
            cloud = true;
            Some("03-window-over-cloud")
        } else if asks && !asked {
            asked = true;
            Some("04-question")
        } else {
            None
        };
        first_window = true;
        if let Some(name) = wants {
            let file = format!("scene-{prefix}{name}");
            let (img, draw) = rig.picture(gpu, &file, SIZE, true);
            let (rect, people) = rig.window_cover(&draw, SIZE);
            eprintln!("{file}: speaker {speaker:?}, asks {asks}; the window covers {rect:?}, over the heads or feet of: {people:?}");
            assert!(bright(&img) > 0, "{file}: the window put light text on the picture");
            close_up(rig, &draw, &img, SIZE, &format!("{file}-closeup"));
            if name == "02-window-over-tifa" {
                let big = (1920, 1080);
                let (img, draw) = rig.picture(gpu, &format!("{file}-1920x1080"), big, true);
                let (rect, people) = rig.window_cover(&draw, big);
                eprintln!("{file} at 1920x1080: the window covers {rect:?}, over: {people:?}");
                close_up(rig, &draw, &img, big, &format!("{file}-1920x1080-closeup"));
            }
            if name == "04-question" {
                rig.state.scene.key(KeyCode::ArrowDown, true, false);
                rig.picture(gpu, &format!("{file}-pointer-moved"), SIZE, true);
            }
        }
        let mut presses = 0;
        while rig.state.scene.player().is_some_and(|p| p.epoch() == epoch) {
            presses += 1;
            assert!(presses < 60, "a window must be moved past");
            rig.state.scene.key(KeyCode::Enter, true, false);
        }
    }
    assert!(!rig.state.scene.is_active(), "the owner played the scene to its end");
    assert!(tifa && cloud && asked, "a window over Tifa, one over Cloud and a question were all seen");
    spoke
}

/// Play the whole scene as its owner on `rig` the quick way: no pictures, only the measurements of every window and of every
/// quarter second between them (and so where the cast was, all through it). The question is answered with its choice number
/// `answer` (0 is the first); what comes back is how many choices the question had (0 if the scene asked nothing).
fn walk(rig: &mut Rig, answer: usize) -> usize {
    let mut guard = 0;
    let mut choices = 0;
    while rig.state.scene.is_active() {
        guard += 1;
        assert!(guard < 6000, "the scene must end");
        let Some(player) = rig.state.scene.player() else { break };
        if !player.has_window() {
            rig.state.scene.advance(0.25);
            if rig.state.scene.is_active() {
                rig.sample("between windows");
            }
            continue;
        }
        player.draw(SIZE.0, SIZE.1, 0.0, None, &mut DrawList::new());
        let (epoch, asks) = (player.epoch(), player.panel().is_some_and(|p| p.asks()));
        if let Some(c) = player.panel().and_then(|p| p.window().message.choice) {
            choices = choices.max(c.last.saturating_sub(c.first) + 1);
        }
        rig.sample("a window");
        if asks {
            let mut turned = 0;
            while rig.state.scene.player().and_then(|p| p.panel()).is_some_and(|p| !p.on_choices(SIZE)) && turned < 20 {
                rig.state.scene.key(KeyCode::Enter, true, false);
                turned += 1;
            }
            for _ in 0..answer {
                rig.state.scene.key(KeyCode::ArrowDown, true, false);
            }
        }
        let mut presses = 0;
        while rig.state.scene.player().is_some_and(|p| p.epoch() == epoch) {
            presses += 1;
            assert!(presses < 60, "a window must be moved past");
            rig.state.scene.key(KeyCode::Enter, true, false);
        }
    }
    choices
}

/// The scene on a fresh rig made from `def`, played through to its end with the choice number `answer` for its question and no
/// pictures; the rig (with its measurements) and how many choices the question had.
fn branch(real: &Real, def: &SceneDef, moved: bool, answer: usize) -> (Rig, usize) {
    let mut rig = make_rig(real, def.clone(), moved);
    rig.start();
    let choices = walk(&mut rig, answer);
    (rig, choices)
}

fn range(r: Option<(f32, f32)>) -> String {
    r.map_or_else(|| "-".to_string(), |(lo, hi)| format!("{lo:.3}..{hi:.3}"))
}

fn print_report(rig: &Rig) {
    let r = &rig.report;
    eprintln!(
        "measured over {} frames (model scale {:.2}; Cloud in {} of them, Tifa in {}): least separation {:.1} (needs {:.1}); worst feet error {:.2}",
        r.frames,
        rig.scale,
        r.seen[0],
        r.seen[1],
        r.min_separation,
        2.0 * RING_RADIUS_UNITS * rig.scale,
        r.max_feet_error
    );
    for (i, look) in rig.looks.iter().enumerate() {
        eprintln!("  witness {i} ({look}): height over Cloud's {}, over Tifa's {}", range(r.ratio_cloud[i]), range(r.ratio_tifa[i]));
    }
    for (what, fraction) in &r.covers {
        eprintln!("  {what}: up to {:.0}% of its picture covered{}", fraction * 100.0, if *fraction > MAX_COVERED { "  <-- MORE THAN THE BOUND" } else { "" });
    }
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_the_proof_scene() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("these pictures are the acceptance of the scene's drawing: a GPU or a software Vulkan/GL driver is needed");
    let (def, overridden) = proof_def();
    eprintln!("witness spots: {:?}; arrival {:?}{}", def.witness, def.arrival, if overridden { " (from FMV_SCENE_SPOTS / FMV_SCENE_ARRIVAL)" } else { " (the game's own data)" });
    let mut rig = make_rig(&real, def, overridden);

    // The OK that starts the scene: with the reach the scene's own data gives, read with the story the field is at, found next to
    // Tifa (where a preview puts the player) and not far from her; and the radius does not depend on the player's own story.
    let radius = rig.state.scene.reach(0, PROOF_FIELD).expect("the trigger's reach is known on this machine");
    let area = real.adapter.load_field(PROOF_FIELD).expect("field");
    let at_start = trigger_npc(area.field_file(), &rig.def, &StoryState::new_game()).map(|t| t.talk_radius);
    let at_scene = trigger_npc(area.field_file(), &rig.def, &rig.story).map(|t| t.talk_radius);
    eprintln!("reach: {radius:.1} (at the scene's moment {at_scene:?}, at a new game's {at_start:?})");
    assert_eq!(Some(radius), at_scene);
    assert_eq!(route_key(&mut rig.state, PROOF_FIELD, 1.0, false, KeyCode::Enter, true, false), SceneRoute::Press(SCENE_NPC_ID_BASE), "OK at the arrival spot starts the scene");
    let far = Pos::new(rig.own_real.x + radius + 80.0, rig.own_real.y, rig.own_real.z);
    rig.state.own.set_motion(far, 0.0, AnimState::Idle);
    assert_eq!(route_key(&mut rig.state, PROOF_FIELD, 1.0, false, KeyCode::Enter, true, false), SceneRoute::Field, "and not from beyond the reach");
    rig.state.own.set_motion(rig.own_real, field_direction_to_facing(rig.def.arrival.direction), AnimState::Idle);
    assert_eq!(rig.state.own.pos(), rig.own_real);

    // Every witness spot stands inside the triangle it names, on the floor.
    let mesh = rig.play.ground().mesh();
    for (i, spot) in rig.def.witness.iter().enumerate() {
        let triangle = mesh.triangles.get(usize::from(spot.triangle)).expect("the named triangle exists");
        assert!(triangle_contains_xy(triangle, i32::from(spot.x), i32::from(spot.y)), "spot {i} is not inside triangle {}", spot.triangle);
        assert!(rig.play.ground().triangle_at(spot.x, spot.y).is_some(), "spot {i} is on the floor");
    }

    // Before the scene: the field as it is, Tifa at her counter as the trigger, nobody else of the scene.
    let (before, draw) = rig.picture(&mut gpu, "scene-00-before", SIZE, false);
    assert!(bright(&before) > 0);
    assert!(draw.built.as_ref().expect("built").anchors.contains_key(&npc_who(SCENE_NPC_ID_BASE)), "before the scene Tifa stands at her counter as the area's character");
    assert!(!draw.built.as_ref().expect("built").anchors.contains_key(&cast_who(20)), "and no cast is drawn");

    // The first frame: Tifa and Cloud where the stage scripts leave them, the three witnesses on their spots. (No window in this
    // one, so the placements are all that is on the picture.)
    rig.start();
    {
        let draw = rig.draw();
        for (look, body) in rig.bodies(&draw) {
            if look.starts_with("ff7/") {
                eprintln!("first frame: {look}: extent {:.1}, head {:.1}, feet at ({:.0}, {:.0})", body.height, body.head, body.feet.0, body.feet.1);
            }
        }
    }
    rig.sample("the first frame");
    let (first, draw) = rig.picture(&mut gpu, "scene-00-first-frame", SIZE, false);
    close_up(&rig, &draw, &first, SIZE, "scene-00-first-frame-closeup");
    assert!(!draw.built.as_ref().expect("built").anchors.contains_key(&npc_who(SCENE_NPC_ID_BASE)), "the trigger's body is not drawn while its scene runs: the cast's Tifa stands in for it");
    for who in [Who(1), Who(2), Who(3), cast_who(18), cast_who(20)] {
        assert!(draw.built.as_ref().expect("built").anchors.contains_key(&who), "{who:?} is drawn in the first frame");
    }

    // Play the whole scene as its owner.
    let spoke = play(&mut rig, &mut gpu, "");

    // What the owner reported: its steps in order and its end once.
    let commands = rig.state.scene.take_commands();
    let steps: Vec<u32> = commands.iter().filter_map(|c| if let super::player::SceneCmd::Step { step, .. } = c { Some(*step) } else { None }).collect();
    assert_eq!(steps, (0..u32::try_from(steps.len()).unwrap()).collect::<Vec<_>>(), "steps 0, 1, 2 ... in order");
    assert_eq!(commands.iter().filter(|c| matches!(c, super::player::SceneCmd::End { skipped: false, .. })).count(), 1, "the end, once");
    assert_eq!(steps.len(), spoke.len());
    eprintln!("the scene: {} window(s) (opener, speaker, asks): {spoke:?}", spoke.len());

    // After the scene: the server says it is over; Tifa is back as the trigger, no cast actor is drawn, and the owner's own
    // body stands where it really is.
    assert!(rig.state.apply(1.0, ServerMsg::SceneEnded { run: SceneRunId(1), outcome: SceneOutcome::Finished }).is_none());
    assert!(!rig.state.scene.is_active());
    let (after, draw) = rig.picture(&mut gpu, "scene-05-after", SIZE, false);
    close_up(&rig, &draw, &after, SIZE, "scene-05-after-closeup");
    let built = draw.built.as_ref().expect("built");
    assert!(built.anchors.contains_key(&npc_who(SCENE_NPC_ID_BASE)), "Tifa is back as the area's character");
    assert!(!built.anchors.contains_key(&cast_who(18)) && !built.anchors.contains_key(&cast_who(20)), "no cast actor is drawn");
    let bodies = rig.bodies(&draw);
    assert!(!bodies.contains_key("ff7/cloud") && !bodies.contains_key("ff7/tifa"), "no canon model of the cast is on the stage");
    let own = bodies.get(rig.looks[0]).expect("the owner's own body is drawn");
    let off = (own.feet.0 - rig.own_real.x).hypot(own.feet.1 - rig.own_real.y);
    assert!(off <= 4.0 * rig.scale, "the owner stands where it really is again: its feet are {off:.1} units from there");
    assert_eq!(rig.state.own.pos(), rig.own_real, "and the scene never moved it");

    print_report(&rig);
    let r = &rig.report;
    assert!(r.frames > 20 && r.seen[0] > 0 && r.seen[1] > 0, "the measurements covered the scene, with both Cloud and Tifa in frame");

    // Every answer of the question: the cast moves differently after each, so the same measurements are made on each (no pictures).
    let mut problems = r.problems.clone();
    let mut answer = 0;
    loop {
        let (other, choices) = branch(&real, &rig.def, overridden, answer);
        let worst = other.report.covers.values().copied().fold(0.0, f32::max);
        eprintln!(
            "answer {answer} (of {choices}): {} frames, least separation {:.1}, the most of a picture another's covers {:.0}%, {} problem(s)",
            other.report.frames,
            other.report.min_separation,
            worst * 100.0,
            other.report.problems.len()
        );
        assert!(other.report.frames > 10, "the measurements covered this answer's scene");
        problems.extend(other.report.problems.iter().map(|p| format!("(answer {answer}) {p}")));
        answer += 1;
        if answer >= choices {
            break;
        }
    }
    assert!(problems.is_empty(), "the scene does not hold to its acceptance numbers:\n{}", problems.join("\n"));
}

/// The direction byte (256 to a turn) that faces from `from` toward `to`.
fn direction_toward(from: (i16, i16), to: (i16, i16)) -> u8 {
    let want = f32::from(to.1 - from.1).atan2(f32::from(to.0 - from.0));
    let off = |d: u8| {
        let a = (field_direction_to_facing(d) - want).rem_euclid(std::f32::consts::TAU);
        a.min(std::f32::consts::TAU - a)
    };
    (0..=255u8).min_by(|a, b| off(*a).total_cmp(&off(*b))).unwrap_or(0)
}

/// What a candidate spot is on the screen.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    spot: Spot,
    /// The part of the witness's picture Cloud's, and Tifa's, covers, and the most of it (or of theirs) that the other witnesses'
    /// pictures cover (0 when there are none).
    over_cloud: f32,
    over_tifa: f32,
    over_others: f32,
    /// Walkmesh distance to Cloud's stool, to Tifa, to the nearest other witness, and to the spot of the data it would replace.
    to_cloud: f32,
    to_tifa: f32,
    to_others: f32,
    to_original: f32,
}

fn player(tint: [u8; 3], who: Who, look: &'static str, (pos, facing): (Pos, f32), own: bool) -> AvatarPlayer<'static> {
    AvatarPlayer { who, pos, facing, anim: AnimState::Idle, scene_frame: None, scene_clip: None, scene_return: None, appearance: Some(look), npc: None, tint, own, party: false }
}

/// The spot's place on the floor, at the ground's height there.
fn spot_pos(rig: &Rig, spot: &Spot) -> Pos {
    let z = rig.play.ground().height(spot.x, spot.y, Some(spot.triangle), None).unwrap_or(rig.own_real.z);
    Pos::new(f32::from(spot.x), f32::from(spot.y), z)
}

/// What a witness (wearing `look`) on `spot` is on the screen next to Cloud and Tifa where they stand (`cloud`, `tifa`: place and
/// heading) and the other witnesses (`others`: look and spot), measured against the data's spot number `replaces`.
fn measure(rig: &mut Rig, look: &'static str, spot: Spot, cloud: (Pos, f32), tifa: (Pos, f32), others: &[(&'static str, Spot)], replaces: usize) -> Candidate {
    let tint = rig.own_tint;
    let pos = spot_pos(rig, &spot);
    let mut players = vec![
        player(tint, Who(1), look, (pos, field_direction_to_facing(spot.direction)), true),
        player(tint, Who(2), "ff7/cloud", cloud, false),
        player(tint, Who(3), "ff7/tifa", tifa, false),
    ];
    for (k, (other_look, other)) in others.iter().enumerate() {
        players.push(player(tint, Who(10 + k as u64), other_look, (spot_pos(rig, other), field_direction_to_facing(other.direction)), false));
    }
    let stage = rig.view.stage.clone().expect("stage");
    let built = rig.avatars.frame(&rig.view, &stage, &players, 0.0);
    let boxes = rig.boxes_of(&built, SIZE);
    let (mine, c, t) = (boxes[look], boxes["ff7/cloud"], boxes["ff7/tifa"]);
    let over_others = others.iter().filter_map(|(other_look, _)| boxes.get(*other_look)).map(|b| covered(mine, *b).max(covered(*b, mine))).fold(0.0, f32::max);
    let to_others = others.iter().map(|(_, other)| pos.dist_xy(spot_pos(rig, other))).fold(f32::MAX, f32::min);
    let original = rig.def.witness[replaces];
    Candidate {
        spot,
        over_cloud: covered(mine, c),
        over_tifa: covered(mine, t),
        over_others,
        to_cloud: pos.dist_xy(cloud.0),
        to_tifa: pos.dist_xy(tifa.0),
        to_others,
        to_original: pos.dist_xy(Pos::new(f32::from(original.x), f32::from(original.y), 0.0)),
    }
}

/// A place on the floor, and where the picture of each of the three bodies would be on the screen standing there.
#[derive(Debug, Clone, Copy)]
struct Place {
    spot: Spot,
    boxes: [Rect; 3],
}

/// Where the picture of a body (`look`) standing on `spot` is on the screen, alone on the floor.
fn lone_box(rig: &mut Rig, look: &'static str, spot: &Spot) -> Rect {
    let players = [player(rig.own_tint, Who(1), look, (spot_pos(rig, spot), field_direction_to_facing(spot.direction)), false)];
    let stage = rig.view.stage.clone().expect("stage");
    let built = rig.avatars.frame(&rig.view, &stage, &players, 0.0);
    rig.boxes_of(&built, SIZE)[look]
}

fn flat(spot: &Spot) -> Pos {
    Pos::new(f32::from(spot.x), f32::from(spot.y), 0.0)
}

/// The most of a picture at `rect` that Cloud's or Tifa's covers, over the measured frames.
fn against_cast(frames: &[CastFrame], rect: Rect) -> f32 {
    frames.iter().flat_map(|f| [f.cloud, f.tifa]).flatten().map(|b| covered(rect, b)).fold(0.0, f32::max)
}

/// How near to Cloud or Tifa, at any measured frame, `spot` is.
fn nearest_cast(frames: &[CastFrame], spot: &Spot) -> f32 {
    frames.iter().flat_map(|f| f.at).flatten().map(|p| p.dist_xy(flat(spot))).fold(f32::MAX, f32::min)
}

/// The three places (one for each spot of the data, in order) that move least from the data's spots in all, and whose pictures
/// stay clear of the cast's at every measured frame and of each other's (any of the three bodies on any of the places, two
/// players may wear the same one) by `tolerance` of a picture, `apart` or more from the cast and from each other. With the total
/// move.
fn best_triple(places: &[Place], frames: &[CastFrame], data: &[Spot], tolerance: f32, apart: f32) -> Option<([usize; 3], f32)> {
    let clear: Vec<usize> = (0..places.len()).filter(|&k| places[k].boxes.iter().all(|b| against_cast(frames, *b) <= tolerance) && nearest_cast(frames, &places[k].spot) >= apart).collect();
    let spaced = |a: usize, b: usize| flat(&places[a].spot).dist_xy(flat(&places[b].spot)) >= apart;
    let not_over = |a: usize, b: usize| places[a].boxes.iter().all(|x| places[b].boxes.iter().all(|y| covered(*x, *y) <= tolerance && covered(*y, *x) <= tolerance));
    let moved = |slot: usize, k: usize| flat(&places[k].spot).dist_xy(flat(&data[slot]));
    let mut best: Option<([usize; 3], f32)> = None;
    for &a in &clear {
        let cost_a = moved(0, a);
        if best.is_some_and(|(_, total)| cost_a >= total) {
            continue;
        }
        for &b in &clear {
            if b == a || !spaced(a, b) || !not_over(a, b) {
                continue;
            }
            let cost_b = cost_a + moved(1, b);
            if best.is_some_and(|(_, total)| cost_b >= total) {
                continue;
            }
            for &c in &clear {
                if c == a || c == b || !spaced(a, c) || !spaced(b, c) || !not_over(a, c) || !not_over(b, c) {
                    continue;
                }
                let total = cost_b + moved(2, c);
                if !best.is_some_and(|(_, least)| total >= least) {
                    best = Some(([a, b, c], total));
                }
            }
        }
    }
    best
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_the_witness_spots_and_a_recommendation() {
    let Some(real) = open() else { return };
    let mut gpu = Offscreen::new().expect("these pictures need a GPU or a software Vulkan/GL driver");
    let (def, overridden) = proof_def();
    let mut rig = make_rig(&real, def, overridden);
    rig.start();
    rig.draw();

    // The cast where the stage scripts leave it (the first frame): positions and headings.
    let (cloud, tifa) = {
        let scene = rig.state.scene.player().expect("a scene is on show");
        let at = |entity: u8| scene.cast_draws().into_iter().find(|c| c.entity == entity).map(|c| (c.pos, c.facing)).expect("placed in the first frame");
        (at(18), at(20))
    };
    let tint = rig.own_tint;

    // 1. The heights of every placeholder body against Cloud's and Tifa's, idle, side by side on the floor of the customers' side.
    let bodies: Vec<&'static str> = PLACEHOLDER_BODIES.iter().map(|b| b.model.appearance_ref).collect();
    let mut line = vec![
        player(tint, Who(100), "ff7/cloud", (Pos::new(-100.0, -40.0, cloud.0.z), 0.0), false),
        player(tint, Who(101), "ff7/tifa", (Pos::new(-100.0, -80.0, cloud.0.z), 0.0), false),
    ];
    for (i, look) in bodies.iter().copied().enumerate() {
        line.push(player(tint, Who(110 + i as u64), look, (Pos::new(-100.0, -120.0 - 40.0 * i as f32, cloud.0.z), 0.0), false));
    }
    let stage = rig.view.stage.clone().expect("stage");
    let built = rig.avatars.frame(&rig.view, &stage, &line, 0.0);
    let table = rig.bodies_of(&built);
    let (cloud_height, tifa_height) = (table["ff7/cloud"].height, table["ff7/tifa"].height);
    for (look, body) in &table {
        if look.starts_with("ff7/") {
            eprintln!("height table: {look}: extent {:.1} ({:.0}% of Cloud's, {:.0}% of Tifa's), head {:.1}", body.height, 100.0 * body.height / cloud_height, 100.0 * body.height / tifa_height, body.head);
        }
    }

    // 2. The witness spots of the data on the screen, against Cloud's and Tifa's pictures.
    let originals: Vec<Spot> = rig.def.witness.iter().copied().take(3).collect();
    let looks = rig.looks;
    for (i, spot) in originals.iter().enumerate() {
        let others: Vec<(&'static str, Spot)> = (0..originals.len()).filter(|j| *j != i).map(|j| (looks[j], originals[j])).collect();
        let m = measure(&mut rig, looks[i], *spot, cloud, tifa, &others, i);
        eprintln!(
            "spot {i} ({}, {}) triangle {} direction {}: Cloud's picture covers {:.0}% of the witness's, Tifa's {:.0}%, the other witnesses' up to {:.0}%; {:.0} from Cloud, {:.0} from Tifa, {:.0} from the nearest other witness",
            spot.x,
            spot.y,
            spot.triangle,
            spot.direction,
            m.over_cloud * 100.0,
            m.over_tifa * 100.0,
            m.over_others * 100.0,
            m.to_cloud,
            m.to_tifa,
            m.to_others
        );
    }

    // 3. The floor of the customers' side (walkable from where a preview puts the player, right of the counter), ranked: nobody's
    // picture overlaps the witness's, clear of Cloud and of Tifa by the scene's own distance, then nearest the first spot.
    let mut component: BTreeSet<u16> = BTreeSet::new();
    {
        let mesh = rig.play.ground().mesh();
        let mut queue: VecDeque<u16> = VecDeque::from([rig.def.arrival.triangle]);
        while let Some(t) = queue.pop_front() {
            if !component.insert(t) {
                continue;
            }
            for &n in mesh.access.get(usize::from(t)).into_iter().flatten() {
                if usize::from(n) < mesh.triangles.len() {
                    queue.push_back(n);
                }
            }
        }
    }
    let cloud_xy = (cloud.0.x as i16, cloud.0.y as i16);
    let mut all: Vec<Candidate> = Vec::new();
    for x in (-190i16..=60).step_by(10) {
        for y in (-330i16..=60).step_by(10) {
            let Some(triangle) = rig.play.ground().triangle_at(x, y) else { continue };
            if !component.contains(&triangle) {
                continue;
            }
            let spot = Spot { x, y, triangle, direction: direction_toward((x, y), cloud_xy) };
            all.push(measure(&mut rig, looks[0], spot, cloud, tifa, &[], 0));
        }
    }
    // (the strictest overlap that leaves a place: none at all, else a tenth of the witness's picture, else a quarter)
    let mut found: Vec<Candidate> = Vec::new();
    let mut tolerance = 0.0;
    for tol in [0.01f32, 0.10, 0.25] {
        found = all.iter().filter(|m| m.over_cloud <= tol && m.over_tifa <= tol && m.to_cloud >= 70.0 && m.to_tifa >= 70.0).copied().collect();
        tolerance = tol;
        if !found.is_empty() {
            break;
        }
    }
    found.sort_by(|a, b| a.to_original.total_cmp(&b.to_original));
    eprintln!(
        "{} of {} floor places on the customers' side overlap at most {:.0}% of the witness's picture on the screen with Cloud's or Tifa's, 70 or more from Cloud and Tifa; the nearest to the first spot:",
        found.len(),
        all.len(),
        tolerance * 100.0
    );
    for m in found.iter().take(8) {
        eprintln!("  ({}, {}) triangle {} direction {}: {:.0} from Cloud, {:.0} from Tifa, {:.0} from the first spot", m.spot.x, m.spot.y, m.spot.triangle, m.spot.direction, m.to_cloud, m.to_tifa, m.to_original);
    }
    let best = found.first().expect("the customers' side has a place that overlaps nobody").spot;

    // 3b. For each spot of the data: where it could stand instead, given the OTHER spots as the data has them, so that nobody's
    // picture overlaps its own (the same floor, the same tiers, 70 or more from Cloud, Tifa and the other witnesses).
    for i in 0..originals.len() {
        let others: Vec<(&'static str, Spot)> = (0..originals.len()).filter(|j| *j != i).map(|j| (looks[j], originals[j])).collect();
        let mut candidates: Vec<Candidate> = Vec::new();
        for x in (-190i16..=60).step_by(10) {
            for y in (-330i16..=60).step_by(10) {
                let Some(triangle) = rig.play.ground().triangle_at(x, y) else { continue };
                if !component.contains(&triangle) {
                    continue;
                }
                let spot = Spot { x, y, triangle, direction: direction_toward((x, y), cloud_xy) };
                candidates.push(measure(&mut rig, looks[i], spot, cloud, tifa, &others, i));
            }
        }
        let mut tolerance = 0.0;
        let mut instead: Vec<Candidate> = Vec::new();
        for tol in [0.01f32, 0.10] {
            instead = candidates
                .iter()
                .filter(|m| m.over_cloud <= tol && m.over_tifa <= tol && m.over_others <= tol && m.to_cloud >= 70.0 && m.to_tifa >= 70.0 && m.to_others >= 70.0)
                .copied()
                .collect();
            tolerance = tol;
            if !instead.is_empty() {
                break;
            }
        }
        instead.sort_by(|a, b| a.to_original.total_cmp(&b.to_original));
        eprintln!("spot {i} could stand instead (at most {:.0}% overlap with anybody, 70 or more from everybody; {} places), nearest the data's:", tolerance * 100.0, instead.len());
        for m in instead.iter().take(5) {
            eprintln!("  ({}, {}) triangle {} direction {}: {:.0} from Cloud, {:.0} from Tifa, {:.0} from the nearest other witness, {:.0} from the data's spot", m.spot.x, m.spot.y, m.spot.triangle, m.spot.direction, m.to_cloud, m.to_tifa, m.to_others, m.to_original);
        }
    }
    assert!(triangle_contains_xy(&rig.play.ground().mesh().triangles[usize::from(best.triangle)], i32::from(best.x), i32::from(best.y)), "the recommended spot is inside its triangle");

    // 3c. The three spots TOGETHER. Where Cloud and Tifa are on the screen is measured at every frame of the scene (the spots decide
    // nothing about them); every place of the floor gets the picture each of the three bodies would have on it; then the triple of
    // places that moves least from the data's spots and overlaps nobody, whichever body stands on whichever place.
    let mut frames: Vec<CastFrame> = Vec::new();
    let mut answer = 0;
    loop {
        let (mut other, choices) = branch(&real, &rig.def, overridden, answer);
        frames.append(&mut other.report.cast);
        answer += 1;
        if answer >= choices {
            break;
        }
    }
    eprintln!("the cast was measured at {} frames, over {answer} answer(s) of the scene", frames.len());
    let mut places: Vec<Place> = Vec::new();
    for x in (-190i16..=60).step_by(10) {
        for y in (-330i16..=60).step_by(10) {
            let Some(triangle) = rig.play.ground().triangle_at(x, y) else { continue };
            if !component.contains(&triangle) {
                continue;
            }
            let spot = Spot { x, y, triangle, direction: direction_toward((x, y), cloud_xy) };
            let boxes = looks.map(|look| lone_box(&mut rig, look, &spot));
            places.push(Place { spot, boxes });
        }
    }
    let mut found_triple = None;
    for tol in [0.01f32, 0.05, 0.10] {
        if let Some((picked, total)) = best_triple(&places, &frames, &originals, tol, 60.0) {
            found_triple = Some((picked, total, tol));
            break;
        }
    }
    let (picked, total, tolerance) = found_triple.expect("the customers' side has room for three witnesses who overlap nobody");
    let spots: Vec<Spot> = picked.iter().map(|k| places[*k].spot).collect();
    eprintln!(
        "the three spots together (from {} places): at most {:.0}% of any picture covered by another's, whichever bodies stand on them, at every measured frame; 60 or more from the cast and from each other; {total:.0} units of move in all:",
        places.len(),
        tolerance * 100.0
    );
    for (i, spot) in spots.iter().enumerate() {
        eprintln!(
            "  spot {i}: Spot {{ x: {}, y: {}, triangle: {}, direction: {} }}  ({:.0} from the data's ({}, {}))",
            spot.x,
            spot.y,
            spot.triangle,
            spot.direction,
            flat(spot).dist_xy(flat(&originals[i])),
            originals[i].x,
            originals[i].y
        );
    }
    eprintln!("  as FMV_SCENE_SPOTS: {}", spots.iter().map(|s| format!("{},{},{},{}", s.x, s.y, s.triangle, s.direction)).collect::<Vec<_>>().join(";"));
    for spot in &spots {
        assert!(triangle_contains_xy(&rig.play.ground().mesh().triangles[usize::from(spot.triangle)], i32::from(spot.x), i32::from(spot.y)), "a recommended spot is inside its triangle");
    }

    // 4. The scene with those spots: the pictures and the same measurements, held to the acceptance numbers.
    let mut def = rig.def.clone();
    def.witness = spots.clone();
    let mut moved = make_rig(&real, def, true);
    moved.start();
    moved.sample("the first frame with the recommended spots");
    let (first, draw) = moved.picture(&mut gpu, "scene-06-recommended-first-frame", SIZE, false);
    close_up(&moved, &draw, &first, SIZE, "scene-06-recommended-first-frame-closeup");
    let spoke = play(&mut moved, &mut gpu, "06-recommended-");
    eprintln!("with the recommended spots: {} window(s)", spoke.len());
    print_report(&moved);
    let r = &moved.report;
    assert!(r.problems.is_empty(), "the recommended spots do not hold to the acceptance numbers:\n{}", r.problems.join("\n"));
}
