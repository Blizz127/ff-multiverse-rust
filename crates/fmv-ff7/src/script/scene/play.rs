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
use crate::story::{canon_party, party_after, tracked_var, PartyOp, StoryState, MOMENT_ADDRESS, MOMENT_BANK};
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
    /// Every other variable or untracked bit the scene changed: not part of the story model, so not carried.
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

/// **The instructions the interpreter skips that read or change the PLAYER's state** rather than what a scene shows: script
/// requests to party members, the party's split and join, the clock, gil, HP and MP, the menus, items, materia and who
/// belongs to the team. Skipping one is not harmless the way skipping a fade is: a caller that SAVES what a scene wrote must
/// look for them ([`Stats::ignored`] of both the stage scripts and the scene) and refuse what it does not carry out, and two
/// editions of a scene tell the same story only if they skip the same ones ([`Outcome::same_story_as`]). `PRTYP`, `PRTYM`
/// and `PRTYE` are not here: the interpreter records those ([`Outcome::party`]).
pub const SKIPPED_STATE_OPS: &[&str] = &[
    "PREQ", "PRQSW", "PRQEW", "JOIN", "SPLIT", "SPTYE", "GTPYE", "SPECIAL", "STTIM", "GOLDu", "GOLDd", "CHGLD", "HMPMAX1", "HMPMAX2", "MHMMX",
    "HMPMAX3", "MPu", "MPd", "MENU", "HPu", "HPd", "STITM", "DLITM", "CKITM", "SMTRA", "DMTRA", "CMTRA", "MMBud", "MMBLK", "MMBUK",
];

/// The ones of [`SKIPPED_STATE_OPS`] that restore the whole party's HP and MP to full and do nothing else. A server that
/// keeps no HP or MP between battles (every fighter starts a battle full) loses nothing by skipping them.
pub const FULL_RESTORE_OPS: &[&str] = &["HMPMAX1", "HMPMAX2", "MHMMX", "HMPMAX3"];

/// The ones of [`SKIPPED_STATE_OPS`] that a window of the scene could announce or that change who takes part: gil, items,
/// materia, HP and MP by an amount, the materia menu's locks, and the party's requests, splits and joins. Two editions of a
/// scene tell the same story only if they skip the same of THESE ([`Outcome::same_story_as`]). The rest are skipped by
/// both editions alike and announce nothing a player is told was given: a menu the game opens (a name entry, a shop, a
/// save), the `SPECIAL` family's settings, a timer, a full restore, and an item count a branch reads (whose outcome the
/// writes and the end already compare). An edition may use more or fewer of those (Echo-S's `md1_1` has one name entry and
/// two `SPECIAL`s where the original has two name entries).
pub const ANNOUNCED_STATE_OPS: &[&str] = &[
    "PREQ", "PRQSW", "PRQEW", "JOIN", "SPLIT", "SPTYE", "GTPYE", "GOLDu", "GOLDd", "CHGLD", "MPu", "MPd", "HPu", "HPd", "STITM", "DLITM",
    "SMTRA", "DMTRA", "CMTRA", "MMBud", "MMBLK", "MMBUK",
];

/// What a headless run of a scene came to ([`run_to_end`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub end: End,
    pub windows: usize,
    /// How many of the windows asked something.
    pub questions: usize,
    pub writes: Writes,
    pub jump: Option<super::vm::SceneJump>,
    /// Interpreter observations from the stage setup, before the main scene starts.
    pub setup_stats: Stats,
    /// Persistent bytes consulted without a complete authoritative value.
    pub unknown_persistent_reads: BTreeSet<(u8, u8)>,
    /// What the scripts did to the story's party, in order, the stage scripts' part first ([`super::Vm::party_ops`]).
    /// Nothing here carries it out: a caller that saves the scene's story must hold it against the party that story
    /// has ([`Outcome::party_is_the_storys`]).
    pub party: Vec<PartyOp>,
    pub stats: Stats,
}

impl Outcome {
    /// Whether the party the scene's scripts leave behind is the one the story has once the scene's writes are
    /// applied: the story's party at `before`, with the scripts' own joins and leaves, equals the story's party at
    /// `after` ([`crate::story::canon_party`]). A scene that touches no party always passes; one that does passes only
    /// where the party table has the row for it, which is how a party change is carried (never guessed, never dropped).
    pub fn party_is_the_storys(&self, before: &StoryState, after: &StoryState) -> bool {
        self.party.is_empty() || party_after(&canon_party(before), &self.party) == canon_party(after)
    }

    /// Whether both runs were interpreted completely: the stage scripts and the scene met no instruction the interpreter
    /// does not know, read back no place it does not know and dropped no request.
    pub fn is_complete(&self) -> bool {
        [&self.setup_stats, &self.stats].iter().all(|s| !s.setup_incomplete && s.unknown.is_empty() && s.unresolved == 0 && s.dropped_requests == 0)
    }

    /// Whether this run and `other` (the same scene from another edition of the field's script) come to the same story: both
    /// interpreted completely, the same end and destination, the same writes to the moment, to the variables the story
    /// carries and to every other saved variable (banks 1 to 4), the same party changes, and the same skipped instructions
    /// whose effect a window could announce ([`ANNOUNCED_STATE_OPS`]: so neither tells of something given or taken that the
    /// other does not). The windows may differ: how a scene is worded and how many windows it takes is the edition's own.
    pub fn same_story_as(&self, other: &Self) -> bool {
        let persistent = |w: &Writes| w.untracked.iter().filter(|((bank, _), _)| (1..=4).contains(bank)).map(|(k, v)| (*k, *v)).collect::<BTreeMap<_, _>>();
        let state_ops = |o: &Self| {
            let mut ops: BTreeMap<&'static str, u32> = BTreeMap::new();
            for stats in [&o.setup_stats, &o.stats] {
                for (op, n) in stats.ignored.iter().filter(|(op, _)| ANNOUNCED_STATE_OPS.contains(op)) {
                    *ops.entry(op).or_insert(0) += n;
                }
            }
            ops
        };
        self.is_complete()
            && other.is_complete()
            && self.end == other.end
            && self.jump == other.jump
            && self.writes.moment == other.writes.moment
            && self.writes.tracked == other.writes.tracked
            && persistent(&self.writes) == persistent(&other.writes)
            && self.party == other.party
            && state_ops(self) == state_ops(other)
    }
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
                Some(t) => {
                    if value & t.bits != self.story.var(bank, address) & t.bits {
                        out.tracked.push(((bank, address), value));
                    }
                    if (value ^ base.byte(bank, address)) & !t.bits != 0 {
                        out.untracked.insert((bank, address), value);
                    }
                }
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
    Ok(outcome_of(ScenePlay::new(field, def, story, seed)?, answers))
}

/// [`run_to_end`] over a replacement script section (another edition of the field's script, with `def`'s entities found in
/// it by name: [`ScenePlay::with_script_section`]) on the original field's floor. What a client runs beside the original to
/// learn whether the replacement comes to the same story ([`Outcome::same_story_as`]) before it shows it.
pub fn run_section_to_end(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8], answers: &[usize]) -> Result<Outcome, SceneError> {
    Ok(outcome_of(ScenePlay::with_script_section(field, def, story, seed, section)?, answers))
}

fn outcome_of(mut play: ScenePlay, answers: &[usize]) -> Outcome {
    let mut answers = answers.iter().copied();
    let mut given: Option<usize> = None;
    let (mut windows, mut questions) = (0usize, 0usize);
    let end = loop {
        let beat = play.next(given.take());
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                windows += 1;
                if w.message.choice.is_some() {
                    questions += 1;
                    given = answers.next();
                }
            }
        }
    };
    let writes = play.writes();
    Outcome {
        end,
        windows,
        questions,
        writes,
        jump: play.vm.destination(),
        setup_stats: play.setup_stats().clone(),
        unknown_persistent_reads: play.vm.vars().unknown_persistent_reads(),
        party: play.vm.party_ops().to_vec(),
        stats: play.stats().clone(),
    }
}

/// Every place the cast stands or walks through in one run of `def` (played to its end with the cursor's answers), as
/// `(x, y)`: the initial places, every place and turn's spot, and each walk sampled every 10 units. What a witness spot must keep
/// clear of ([`auto_witnesses`]).
pub fn cast_places(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32) -> Result<Vec<(f32, f32)>, SceneError> {
    let mut play = ScenePlay::new(field, def, story, seed)?;
    let mut stage: Vec<Stage> = play.initial_stage_events().to_vec();
    for _ in 0..=super::vm::Limits::default().windows.max(64) {
        let beat = play.next(None);
        stage.extend(beat.stage.into_iter().map(|t| t.what));
        if matches!(beat.then, Then::End { .. }) {
            break;
        }
    }
    let mut at: BTreeMap<u8, (f32, f32)> = BTreeMap::new();
    let mut out = Vec::new();
    for s in &stage {
        match *s {
            Stage::Place { entity, x, y, .. } => {
                at.insert(entity, (f32::from(x), f32::from(y)));
                out.push((f32::from(x), f32::from(y)));
            }
            Stage::Move { entity, from, x, y, .. } => {
                let start = from.map(|(fx, fy)| (f32::from(fx), f32::from(fy))).or_else(|| at.get(&entity).copied());
                let end = (f32::from(x), f32::from(y));
                if let Some(a) = start {
                    let n = ((end.0 - a.0).hypot(end.1 - a.1) / 10.0).ceil().clamp(1.0, 4096.0) as u32;
                    for k in 0..=n {
                        let t = k as f32 / n as f32;
                        out.push((a.0 + (end.0 - a.0) * t, a.1 + (end.1 - a.1) * t));
                    }
                }
                at.insert(entity, end);
                out.push(end);
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The part of the floor each triangle of `mesh` is on: triangles joined by an edge a walk can cross (either way) are on one
/// part, numbered from 0 in the order of their lowest triangle.
fn floor_parts(mesh: &Walkmesh) -> Vec<usize> {
    let n = mesh.triangles.len();
    let mut part = vec![usize::MAX; n];
    let mut next = 0usize;
    for start in 0..n {
        if part.get(start) != Some(&usize::MAX) {
            continue;
        }
        if let Some(slot) = part.get_mut(start) {
            *slot = next;
        }
        let mut stack = vec![start];
        while let Some(t) = stack.pop() {
            for (other, slot) in part.iter_mut().enumerate() {
                if *slot == usize::MAX && (0..3).any(|e| mesh.neighbor(t, e) == Some(other) || mesh.neighbor(other, e) == Some(t)) {
                    *slot = next;
                    stack.push(other);
                }
            }
        }
        next += 1;
    }
    part
}

/// **Witness spots for a scene that names none**: up to `count` places on the field's floor, each clear of every place the
/// cast takes in the scene ([`cast_places`]) by two body rings and a margin, clear of the floor's edge (walls, stools, holes) by
/// one and a half rings, on the part of the floor the scene's arrival is on, apart from each other, and as near the arrival as
/// that allows; each faces the middle of where the cast stands. The same data gives the same spots on every machine (the
/// scene's own arrival, never a player's place), so every client of a run puts its people in the same places. Empty when the
/// scene cannot be played here or no place of the floor qualifies (the participants then keep their own places).
///
/// This keeps a witness off the cast on the FLOOR; what the camera shows (a foreground layer in front, another person in the
/// line of sight) is not judged here: a scene whose spots are measured on the picture names them in its data instead.
pub fn auto_witnesses(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, count: usize) -> Vec<Spot> {
    let (Ok(script), Ok(mesh)) = (FieldScript::from_field(field), field.walkmesh()) else { return Vec::new() };
    let Ok(cast) = cast_places(field, def, story, seed) else { return Vec::new() };
    let ring = 5.5 * crate::default_model_scale(script.header().scale);
    let (clear_of_cast, clear_of_edge) = (2.0 * ring + 16.0, 1.5 * ring);
    let parts = floor_parts(&mesh);
    let home = parts.get(usize::from(def.arrival.triangle)).copied();
    let edge_gap = |x: f32, y: f32| -> f32 {
        let mut best = f32::INFINITY;
        for (i, tri) in mesh.triangles.iter().enumerate() {
            for e in 0..3 {
                if mesh.neighbor(i, e).is_some() {
                    continue;
                }
                let (Some(a), Some(b)) = (tri.get(e), tri.get((e + 1) % 3)) else { continue };
                let (ax, ay, bx, by) = (f32::from(a.x), f32::from(a.y), f32::from(b.x), f32::from(b.y));
                let (dx, dy) = (bx - ax, by - ay);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 { (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                best = best.min((x - (ax + dx * t)).hypot(y - (ay + dy * t)));
            }
        }
        best
    };
    // candidates: each triangle's centre and the points halfway from it to each corner, on the arrival's part of the floor
    let mut candidates: Vec<(f32, i16, i16, u16)> = Vec::new();
    for (i, tri) in mesh.triangles.iter().enumerate() {
        if home.is_some_and(|h| parts.get(i) != Some(&h)) {
            continue;
        }
        let Ok(index) = u16::try_from(i) else { continue };
        let (cx, cy) = (tri.iter().map(|v| f32::from(v.x)).sum::<f32>() / 3.0, tri.iter().map(|v| f32::from(v.y)).sum::<f32>() / 3.0);
        let mut points = vec![(cx, cy)];
        points.extend(tri.iter().map(|v| ((cx + f32::from(v.x)) / 2.0, (cy + f32::from(v.y)) / 2.0)));
        for (x, y) in points {
            let (xi, yi) = (x.round() as i16, y.round() as i16);
            if !triangle_contains_xy(tri, i32::from(xi), i32::from(yi)) {
                continue;
            }
            let (fx, fy) = (f32::from(xi), f32::from(yi));
            if cast.iter().any(|&(px, py)| (px - fx).hypot(py - fy) < clear_of_cast) || edge_gap(fx, fy) < clear_of_edge {
                continue;
            }
            let d = (fx - f32::from(def.arrival.x)).hypot(fy - f32::from(def.arrival.y));
            candidates.push((d, xi, yi, index));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2, a.3).cmp(&(b.1, b.2, b.3))));
    // face the middle of the cast's places
    let (sx, sy, m) = cast.iter().fold((0.0f32, 0.0f32, 0usize), |(sx, sy, m), &(x, y)| (sx + x, sy + y, m + 1));
    let middle = (m > 0).then(|| (sx / m as f32, sy / m as f32));
    let mut out: Vec<Spot> = Vec::new();
    for (_, x, y, triangle) in candidates {
        if out.len() >= count {
            break;
        }
        if out.iter().any(|o| (f32::from(o.x) - f32::from(x)).hypot(f32::from(o.y) - f32::from(y)) < clear_of_cast) {
            continue;
        }
        let direction = middle.map_or(0, |(mx, my)| crate::facing_to_field_direction((my - f32::from(y)).atan2(mx - f32::from(x))));
        out.push(Spot { x, y, triangle, direction });
    }
    out
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
