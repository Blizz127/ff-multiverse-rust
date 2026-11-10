//! Playing a scene: the interpreter ([`super::vm`]) over one field's own data, with the cast, the ground and the
//! story it is played against.
//!
//! [`ScenePlay`] is what a client (and the server, headless) holds for one run of a scene: it reads the field's
//! script, runs the stage scripts quietly to find where everyone stands, then hands out [`Beat`]s. What it adds to
//! the VM:
//!
//! * the **cast**: stage events about entities that are not in the cast are dropped (counted);
//! * the **ground**: every place gets its height from the walkmesh when the script gives none;
//! * the **speakers**: a window's first line names who speaks, which the scene's speaker table maps to a cast entity;
//! * the **story writes** ([`Writes`]): what the scene's script wrote to the moment and to the variables the story
//!   model carries, ready to be applied once.
//!
//! [`run_to_end`] is the headless run the server makes to learn those writes from the answers a player gave.

use super::data::{CastMember, SceneDef, Spot};
use super::vm::{Beat, Limits, SceneVars, Stage, Stats, Then, Timed, Vm, Where};
use crate::field::{FieldFile, ModelLoader, Vertex, Walkmesh};
use crate::field_direction_to_facing;
use crate::script::dialogue::End;
use crate::script::{parse_model_list, triangle_contains_xy, triangle_height_at, FieldScript};
use crate::story::{tracked_var, StoryState, MOMENT_ADDRESS, MOMENT_BANK};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Why a scene cannot be played.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SceneError {
    #[error("the field's script cannot be read: {0}")]
    Script(String),
    #[error("the field has no walkmesh: {0}")]
    Walkmesh(String),
    #[error("the scene's trigger names an entity the field does not have")]
    NoSuchEntity,
    #[error("the replacement script does not have unique matching scene entities")]
    UnmatchedEntities,
}

/// What a scene's script wrote that the story can carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Writes {
    /// The moment (`Var[2][0]`) the scene left it at, when it wrote one that differs from the story's.
    pub moment: Option<u16>,
    /// The variables the story model carries ([`crate::story::TRACKED_VARS`]) that the scene changed, as the new byte.
    pub tracked: Vec<((u8, u8), u8)>,
    /// Every other variable the scene changed (the field's own flags): not part of the story model, so not carried.
    pub untracked: BTreeMap<(u8, u8), u8>,
}

impl Writes {
    /// Whether the scene wrote anything the story carries.
    pub fn is_story_neutral(&self) -> bool {
        self.moment.is_none() && self.tracked.is_empty()
    }

    /// Apply the writes to a story: the moment only goes forward and a tracked bit only goes on (the story model's own
    /// rule: nothing a late or repeated write does can take a story back). Returns whether the story changed.
    pub fn apply_to(&self, state: &mut StoryState) -> bool {
        let mut changed = false;
        if let Some(moment) = self.moment.filter(|m| *m > state.moment) {
            state.moment = moment;
            changed = true;
        }
        for &((bank, address), value) in &self.tracked {
            let Some(tracked) = tracked_var(bank, address) else { continue };
            for bit in 0..8u8 {
                let mask = 1u8 << bit;
                if tracked.bits & mask != 0 && value & mask != 0 {
                    changed |= state.set_bit(bank, address, bit, true);
                }
            }
        }
        changed
    }
}

/// What a headless run of a scene came to ([`run_to_end`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub end: End,
    pub windows: usize,
    pub writes: Writes,
    pub jump: Option<super::vm::SceneJump>,
    pub stats: Stats,
}

/// The character whose talk starts a scene, as the field stands him at the scene's story: what a server needs to
/// announce him as an NPC.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerNpc {
    pub entity: u8,
    pub name: String,
    /// The `char.lgp` skeleton stem of his model (`aagb`).
    pub model: String,
    /// The stem of the clip that shows him standing, if the loader names one.
    pub idle: Option<String>,
    pub x: i16,
    pub y: i16,
    /// The ground's height at his place.
    pub z: f32,
    /// Heading in radians (counter-clockwise from the field's +x axis).
    pub heading: f32,
    /// How near a player has to stand to talk to him, in walkmesh units (the script's `TALKR` at the field's scale).
    pub talk_radius: f32,
}

/// The walkmesh's height at a place.
pub struct Ground {
    mesh: Walkmesh,
}

impl Ground {
    pub fn new(mesh: Walkmesh) -> Self {
        Self { mesh }
    }

    pub fn mesh(&self) -> &Walkmesh {
        &self.mesh
    }

    /// The height of the surface at `(x, y)`: the triangle `hint` names when it holds the point, else the surface
    /// nearest `z_hint` where several overlap, else `None` (off the mesh).
    pub fn height(&self, x: i16, y: i16, hint: Option<u16>, z_hint: Option<i16>) -> Option<f32> {
        let (xi, yi) = (i32::from(x), i32::from(y));
        let at = |tri: &[Vertex; 3]| triangle_contains_xy(tri, xi, yi).then(|| triangle_height_at(tri, xi, yi)).flatten();
        if let Some(h) = hint.and_then(|t| self.mesh.triangles.get(usize::from(t))).and_then(at) {
            return Some(h as f32);
        }
        let reference = z_hint.map_or(0.0, f64::from);
        self.mesh
            .triangles
            .iter()
            .filter_map(at)
            .min_by(|a, b| (a - reference).abs().total_cmp(&(b - reference).abs()))
            .map(|h| h as f32)
    }

    /// The index of the triangle that holds `(x, y)` (the first, if several do).
    pub fn triangle_at(&self, x: i16, y: i16) -> Option<u16> {
        let (xi, yi) = (i32::from(x), i32::from(y));
        self.mesh.triangles.iter().position(|t| triangle_contains_xy(t, xi, yi)).and_then(|i| u16::try_from(i).ok())
    }
}

/// One run of a scene.
pub struct ScenePlay {
    def: SceneDef,
    section: Vec<u8>,
    ground: Ground,
    vm: Vm,
    setup: Stats,
    initial: Vec<Stage>,
    cast: BTreeSet<u8>,
    off_cast: u32,
    story: StoryState,
    ended: Option<End>,
}

impl std::fmt::Debug for ScenePlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScenePlay").field("scene", &self.def.id).field("ended", &self.ended).finish_non_exhaustive()
    }
}

fn spot_where(s: &Spot) -> Where {
    Where { x: s.x, y: s.y, z: None, triangle: Some(s.triangle) }
}

impl ScenePlay {
    /// Play `def` in `field` for a player whose story is `story`. `seed` feeds the script's `RANDOM` (the same seed gives
    /// the same run, which is how every client and the server agree).
    pub fn new(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32) -> Result<Self, SceneError> {
        Self::with_limits(field, def, story, seed, Limits::default())
    }

    pub fn with_limits(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, limits: Limits) -> Result<Self, SceneError> {
        let script = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        Self::with_script(field, def, story, seed, limits, &script)
    }

    /// Build a scene from a replacement script section and the original field's walkmesh.
    /// The caller must establish compatible choices, pacing, and multiplayer synchronization
    /// before using this for a live scene; mapping entity names does not establish those properties.
    pub fn with_script_section(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8]) -> Result<Self, SceneError> {
        let original = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        let replacement = FieldScript::parse(section).map_err(|e| SceneError::Script(e.to_string()))?;
        let rebound = def.rebind_entities(&original, &replacement).ok_or(SceneError::UnmatchedEntities)?;
        Self::with_script(field, &rebound, story, seed, Limits::default(), &replacement)
    }

    fn with_script(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, limits: Limits, script: &FieldScript<'_>) -> Result<Self, SceneError> {
        if usize::from(def.trigger.entity) >= script.entity_count() {
            return Err(SceneError::NoSuchEntity);
        }
        let mesh = field.walkmesh().map_err(|e| SceneError::Walkmesh(e.to_string()))?;
        let section = script.bytes().to_vec();
        let mut vm = Vm::new(SceneVars::new(story.clone(), &def.given), seed, limits);
        vm.setup(script, &def.setup);
        let setup = vm.take_stats();
        // A cast member the stage scripts did not place stands where the cast says, and the scene knows it.
        for m in &def.cast {
            if let Some(s) = &m.start {
                vm.seed(m.entity, spot_where(s), Some(field_direction_to_facing(s.direction)));
            }
        }
        vm.begin(def.root.clone());
        let ground = Ground::new(mesh);
        let cast: BTreeSet<u8> = def.cast.iter().map(|m| m.entity).collect();
        let initial = Self::initial_stage(&vm, &ground, &def.cast);
        Ok(Self { def: def.clone(), section, ground, vm, setup, initial, cast, off_cast: 0, story: story.clone(), ended: None })
    }

    /// Where everyone of the cast stands when the scene begins: what the stage scripts left, else the cast's own start.
    fn initial_stage(vm: &Vm, ground: &Ground, cast: &[CastMember]) -> Vec<Stage> {
        let mut out = Vec::new();
        for member in cast {
            let place = vm.place_of(member.entity).or_else(|| member.start.as_ref().map(spot_where));
            if let Some(w) = place {
                let z = w.z.or_else(|| ground.height(w.x, w.y, w.triangle, None).map(|h| h.round() as i16));
                out.push(Stage::Place { entity: member.entity, x: w.x, y: w.y, z, triangle: w.triangle });
            }
            let heading = vm.heading_of(member.entity).or_else(|| member.start.as_ref().map(|s| field_direction_to_facing(s.direction)));
            if let Some(heading) = heading {
                out.push(Stage::Face { entity: member.entity, heading, frames: 0 });
            }
            if let Some(visible) = vm.visible_of(member.entity) {
                out.push(Stage::Show { entity: member.entity, visible });
            }
        }
        out
    }

    pub fn def(&self) -> &SceneDef {
        &self.def
    }

    pub fn ground(&self) -> &Ground {
        &self.ground
    }

    /// Where the cast stands when the scene begins.
    pub fn initial_stage_events(&self) -> &[Stage] {
        &self.initial
    }

    /// What the stage scripts did (their counters; the scene's own are [`ScenePlay::stats`]).
    pub fn setup_stats(&self) -> &Stats {
        &self.setup
    }

    pub fn stats(&self) -> &Stats {
        self.vm.stats()
    }

    /// Stage events dropped because they were about an entity outside the cast.
    pub fn off_cast(&self) -> u32 {
        self.off_cast
    }

    /// How the scene ended, once it has.
    pub fn ended(&self) -> Option<End> {
        self.ended
    }

    /// Run on to the next window or the end. `answer` is the answer to the window handed out last, when it asked: the
    /// absolute line number of the choice (the beat's `message.choice` has the range); anything else is clamped into the
    /// choices and `None` takes the cursor's starting line.
    pub fn next(&mut self, answer: Option<usize>) -> Beat {
        let Ok(script) = FieldScript::parse(&self.section) else {
            self.ended = Some(End::Broken);
            return Beat { stage: Vec::new(), then: Then::End { at: 0, how: End::Broken } };
        };
        let mut beat = self.vm.resume(&script, answer);
        drop(script);
        self.refine(&mut beat);
        if let Then::End { how, .. } = &beat.then {
            self.ended = Some(*how);
        }
        beat
    }

    /// Keep what concerns the cast, give every place its height, name the speaker of a window.
    fn refine(&mut self, beat: &mut Beat) {
        let before = beat.stage.len();
        beat.stage.retain(|t| self.cast.contains(&stage_entity(&t.what).unwrap_or(u8::MAX)) || stage_entity(&t.what).is_none());
        self.off_cast = self.off_cast.saturating_add(u32::try_from(before - beat.stage.len()).unwrap_or(u32::MAX));
        for Timed { what, .. } in &mut beat.stage {
            match what {
                Stage::Place { x, y, z, triangle, .. } if z.is_none() => {
                    *z = self.ground.height(*x, *y, *triangle, None).map(|h| h.round() as i16);
                }
                Stage::Move { x, y, z, .. } if z.is_none() => {
                    *z = self.ground.height(*x, *y, None, None).map(|h| h.round() as i16);
                }
                _ => {}
            }
        }
        if let Then::Window(w) = &mut beat.then {
            let label = w.message.page_lines().first().and_then(|page| (page.len() > 1).then(|| page.first().cloned())).flatten();
            w.speaker = label.and_then(|l| self.def.speaker_entity(&l));
        }
    }

    /// What the scene has written so far that the story carries ([`Writes`]).
    pub fn writes(&self) -> Writes {
        let base = self.vm.vars();
        let written = self.vm.writes();
        let byte = |bank: u8, address: u8| written.get(&(bank, address)).copied().unwrap_or_else(|| {
            use crate::script::dialogue::StoryVars;
            base.byte(bank, address)
        });
        let mut out = Writes::default();
        let lo = byte(MOMENT_BANK, MOMENT_ADDRESS);
        let hi = byte(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1));
        let moment = u16::from_le_bytes([lo, hi]);
        if (written.contains_key(&(MOMENT_BANK, MOMENT_ADDRESS)) || written.contains_key(&(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1)))) && moment != self.story.moment {
            out.moment = Some(moment);
        }
        for (&(bank, address), &value) in written {
            use crate::script::dialogue::StoryVars;
            if bank == MOMENT_BANK && address <= MOMENT_ADDRESS.wrapping_add(1) {
                continue;
            }
            if value == base.byte(bank, address) {
                continue;
            }
            match tracked_var(bank, address) {
                Some(t) if value & t.bits != self.story.var(bank, address) & t.bits => out.tracked.push(((bank, address), value)),
                Some(_) => {}
                None => {
                    out.untracked.insert((bank, address), value);
                }
            }
        }
        out
    }
}

fn stage_entity(stage: &Stage) -> Option<u8> {
    match stage {
        Stage::Place { entity, .. } | Stage::Move { entity, .. } | Stage::Face { entity, .. } | Stage::Anim { entity, .. } | Stage::Show { entity, .. } => Some(*entity),
        Stage::Camera(_) => None,
    }
}

/// Run `def` in `field` to its end without a person: each window that asks takes the next of `answers` (the lines a
/// player chose, in order; when they run out, the cursor's starting line). What the server does to learn what the scene
/// wrote, from what the owner answered.
pub fn run_to_end(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, answers: &[usize]) -> Result<Outcome, SceneError> {
    let mut play = ScenePlay::new(field, def, story, seed)?;
    let mut answers = answers.iter().copied();
    let mut given: Option<usize> = None;
    let mut windows = 0usize;
    let end = loop {
        let beat = play.next(given.take());
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                windows += 1;
                if w.message.choice.is_some() {
                    given = answers.next();
                }
            }
        }
    };
    Ok(Outcome { end, windows, writes: play.writes(), jump: play.vm.destination(), stats: play.stats().clone() })
}

/// The character that starts `def`, as the field stands him at the scene's story (see [`TriggerNpc`]).
pub fn trigger_npc(field: &FieldFile, def: &SceneDef, story: &StoryState) -> Option<TriggerNpc> {
    let script = FieldScript::from_field(field).ok()?;
    let loader: ModelLoader = field.model_loader().ok()?;
    let models = parse_model_list(field.section(crate::field::section::MODEL_LOADER).ok()?).ok()?;
    let entity = usize::from(def.trigger.entity);
    let init = script.entity_inits(Some(&models)).into_iter().find(|i| i.entity == entity)?;
    let record = loader.models.get(init.model_index?)?;
    let mesh = field.walkmesh().ok()?;
    let ground = Ground::new(mesh);
    let mut vm = Vm::new(SceneVars::new(story.clone(), &def.given), 1, Limits::default());
    vm.setup(&script, &def.setup);
    let place = vm.place_of(def.trigger.entity)?;
    let z = place.z.map(f32::from).or_else(|| ground.height(place.x, place.y, place.triangle, None))?;
    let scale = f32::from(script.header().scale.max(1)) / 512.0;
    Some(TriggerNpc {
        entity: def.trigger.entity,
        name: def.trigger.name.clone(),
        model: record.hrc_stem(),
        idle: record.animations.first().map(|a| a.stem()),
        x: place.x,
        y: place.y,
        z,
        heading: vm.heading_of(def.trigger.entity).unwrap_or(0.0),
        talk_radius: f32::from(def.trigger.talk_radius) * scale,
    })
}
