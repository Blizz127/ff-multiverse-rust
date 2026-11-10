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

use super::data::{CastMember, Look, SceneDef, Spot};
use super::vm::{AnimHow, Beat, Limits, MoveHow, SceneVars, SceneResources, ResourceOutcome, Stage, Stats, Then, Timed, Vm, Where};
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
    #[error("the scene cannot reconstruct its expected battle boundary")]
    BattleBoundary,
    #[error("the scene cannot prepare its story snapshot: {0}")]
    Story(#[from] crate::story::StoryWriteError),
    #[error("the scene is missing the answer to question {question}")]
    MissingAnswer { question: usize },
    #[error("answer {answer} to question {question} is outside its choices {first}..={last}")]
    InvalidAnswer { question: usize, answer: usize, first: usize, last: usize },
    #[error("the scene ended with {remaining} unused answers")]
    TrailingAnswers { remaining: usize },
    #[error("the replay window acknowledgement does not match the pending window or its choices")]
    WindowBoundary,
    #[error("strict replay cannot follow unchecked preview advancement")]
    UncheckedReplay,
    #[error("the replay source or interpreter version differs")]
    ReplayIdentity,
    #[error("the transcript does not end at the expected boundary")]
    ReplayBoundary,
    #[error("the battle resume lacks an authoritative extended story or resource snapshot")]
    BattleResources,
    #[error("the authored battle mode is not supported for durable continuation")]
    BattleMode,
    #[error("the script defaulted a setup or malformed question instead of receiving an accepted answer")]
    ImplicitAnswer,
    #[error("a required scene window cannot be decoded")]
    WindowData,
    #[error("the accepted scene trigger position cannot be represented on this field's floor")]
    SceneOrigin,
}

/// Replay semantics version. Change this when the interpreter's durable replay contract changes.
pub const REPLAY_VERSION: u32 = 1;

/// The exact authored instruction at which this run stopped for combat. Ordinals distinguish
/// repeated executions of the same instruction, including repeated formations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BattleCheckpoint {
    pub ordinal: u32,
    pub entity: u8,
    pub instruction_offset: u32,
    pub formation: u16,
    /// The last authored BTLMD flags; absent means no mode was established by this scene.
    pub flags: Option<u16>,
}

impl BattleCheckpoint {
    /// Canonical opaque key shared by host and client. No decoder is needed: compare with the
    /// boundary reconstructed from trusted script data. Includes a kind and encoding version.
    pub fn identity_bytes(&self) -> Vec<u8> {
        let mut out = vec![1, 2];
        out.extend(self.ordinal.to_le_bytes()); out.push(self.entity);
        out.extend(self.instruction_offset.to_le_bytes()); out.extend(self.formation.to_le_bytes());
        out.push(u8::from(self.flags.is_some()));
        if let Some(flags) = self.flags { out.extend(flags.to_le_bytes()); }
        out
    }
}

/// Every presented window has an identity, including non-question windows and interpreter notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowCheckpoint {
    pub ordinal: u32,
    pub entity: u8,
    pub instruction_offset: Option<u32>,
    pub window: u8,
    pub dialog: Option<u16>,
    pub choices: Option<[u8; 2]>,
}

impl WindowCheckpoint {
    pub fn identity_bytes(&self) -> Vec<u8> {
        let mut out = vec![1, 1];
        out.extend(self.ordinal.to_le_bytes()); out.push(self.entity);
        out.push(u8::from(self.instruction_offset.is_some()));
        if let Some(offset) = self.instruction_offset { out.extend(offset.to_le_bytes()); }
        out.push(self.window); out.push(u8::from(self.dialog.is_some()));
        if let Some(dialog) = self.dialog { out.extend(dialog.to_le_bytes()); }
        out.push(u8::from(self.choices.is_some()));
        if let Some(choices) = self.choices { out.extend(choices); }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowAck {
    pub checkpoint: WindowCheckpoint,
    /// Required for an ASK, forbidden for every other window. Absolute authored choice line.
    pub answer: Option<u8>,
}

/// Only victorious continuation is currently supported. The host must establish the real result;
/// an escaped or defeated battle cannot manufacture a successful continuation with this API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleResolution { Victory }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleResume {
    pub checkpoint: BattleCheckpoint,
    pub resolution: BattleResolution,
    /// Authoritative afterimage including the committed segment and the battle's rewards.
    pub story: StoryState,
    pub resources: SceneResources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneInput { Window(WindowAck), Battle(BattleResume) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneBoundary { Window(WindowCheckpoint), Battle(BattleCheckpoint), End(End) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayIdentity {
    pub version: u32,
    pub fingerprint: [u8; 32],
    pub seed: u32,
}

/// A boundary's current segment effects. Stats and unknown reads remain cumulative so a later
/// battle cannot erase an unsafe prefix. Persistent and economic writes rebase after each battle.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneSnapshot {
    pub boundary: SceneBoundary,
    pub writes: Writes,
    pub resources: ResourceOutcome,
    pub party: Vec<PartyOp>,
    pub jump: Option<super::vm::SceneJump>,
    pub setup_stats: Stats,
    pub stats: Stats,
    pub unknown_persistent_reads: BTreeSet<(u8, u8)>,
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
    /// Version 3 persistent assignments: canonical byte key to (value, assigned bits).
    /// Includes known zeroes and clears; only [`Writes::apply_extended_to`] applies these.
    pub persistent: BTreeMap<(u8, u8), (u8, u8)>,
}

impl Writes {
    /// Whether the scene wrote anything the story carries.
    pub fn is_story_neutral(&self) -> bool {
        self.moment.is_none() && self.tracked.is_empty() && self.persistent.is_empty()
    }

    /// Apply the writes to a story: the moment only goes forward and a tracked bit only goes on (the story model's own
    /// rule: nothing a late or repeated write does can take a story back). Returns whether the story changed.
    pub fn apply_to(&self, state: &mut StoryState) -> bool {
        // Extended states require ordered, exact-base application, never the legacy union.
        if state.supports_extended() { return false; }
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

    /// Apply an extended outcome only to the snapshot it was interpreted against. Callers must also
    /// compare that snapshot with durable storage atomically before publishing any resulting state.
    /// Validation is atomic: an invalid assignment changes nothing.
    pub fn apply_extended_to(&self, state: &mut StoryState, expected: &StoryState) -> Option<bool> {
        if state != expected || !state.supports_extended() { return None; }
        let mut next = state.clone();
        if let Some(moment) = self.moment { next.moment = moment; }
        for (&(bank, address), &(value, mask)) in &self.persistent {
            next.write_masked(bank, address, value, mask).ok()?;
        }
        let changed = next != *state;
        *state = next;
        Some(changed)
    }
}

/// **The instructions the interpreter skips that read or change the PLAYER's state** rather than what a scene shows: script
/// requests to party members, the party's split and join, the clock, gil, HP and MP, the menus, items, materia and who
/// belongs to the team. Skipping one is not harmless the way skipping a fade is: a caller that SAVES what a scene wrote must
/// look for them ([`Stats::ignored`] of both the stage scripts and the scene) and refuse what it does not carry out, and two
/// editions of a scene tell the same story only if they skip the same ones ([`Outcome::same_story_as`]). `PRTYP`, `PRTYM`
/// and `PRTYE` are not here: the interpreter records those ([`Outcome::party`]). The gil operations are skipped only
/// without a resource snapshot; with one, [`Outcome::resources`] records their ordered, evaluated effects instead.
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
    /// Ordered economic operations and their authoritative before/after values.
    pub resources: ResourceOutcome,
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
        !self.resources.unknown_gil && [&self.setup_stats, &self.stats].iter().all(|s| !s.setup_incomplete && s.unknown.is_empty() && s.unresolved == 0 && s.dropped_requests == 0)
    }

    /// Whether this run and `other` (the same scene from another edition of the field's script) come to the same story: both
    /// interpreted completely, the same resource inputs and ordered effects, the same end and destination, the same writes to the moment, to the variables the story
    /// carries and to every other persistent bank, the same party changes, and the same skipped instructions
    /// whose effect a window could announce ([`ANNOUNCED_STATE_OPS`]: so neither tells of something given or taken that the
    /// other does not). The windows may differ: how a scene is worded and how many windows it takes is the edition's own.
    pub fn same_story_as(&self, other: &Self) -> bool {
        let persistent = |w: &Writes| w.untracked.iter().filter(|((bank, _), _)| crate::story::canonical_persistent_bank(*bank).is_some()).map(|(k, v)| (*k, *v)).collect::<BTreeMap<_, _>>();
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
            && self.writes.persistent == other.writes.persistent
            && persistent(&self.writes) == persistent(&other.writes)
            && self.resources == other.resources
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
    /// Loader clips only for cast models whose skeleton matches the script's actual binding.
    animation_clips: BTreeMap<u8, Vec<String>>,
    /// Supported standing defaults at the first visible boundary, after any battle prelude.
    initial_default_animations: BTreeMap<u8, u8>,
    /// Last authored/resolved height of each actor, carried across beats to select overlapping floors.
    heights: BTreeMap<u8, i16>,
    cast: BTreeSet<u8>,
    off_cast: u32,
    story: StoryState,
    ended: Option<End>,
    seed: u32,
    unchecked: bool,
    strict: bool,
    transcript: Vec<SceneInput>,
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
        Self::with_script(field, def, story, seed, limits, &script, SceneResources::default())
    }

    /// Play with the account's authoritative resource snapshot (or an explicitly synthetic preview).
    pub fn with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources) -> Result<Self, SceneError> {
        let script = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        Self::with_script(field, def, story, seed, Limits::default(), &script, resources)
    }

    /// Build a scene from a replacement script section and the original field's walkmesh.
    /// The caller must establish compatible choices, pacing, and multiplayer synchronization
    /// before using this for a live scene; mapping entity names does not establish those properties.
    pub fn with_script_section(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8]) -> Result<Self, SceneError> {
        Self::with_script_section_and_resources(field, def, story, seed, section, SceneResources::default())
    }

    /// Replacement-script playback with exactly the same resource snapshot as the original.
    pub fn with_script_section_and_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8], resources: SceneResources) -> Result<Self, SceneError> {
        let original = FieldScript::from_field(field).map_err(|e| SceneError::Script(e.to_string()))?;
        let replacement = FieldScript::parse(section).map_err(|e| SceneError::Script(e.to_string()))?;
        let rebound = def.rebind_entities(&original, &replacement).ok_or(SceneError::UnmatchedEntities)?;
        Self::with_script(field, &rebound, story, seed, Limits::default(), &replacement, resources)
    }

    fn with_script(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, limits: Limits, script: &FieldScript<'_>, resources: SceneResources) -> Result<Self, SceneError> {
        let story = def.prepare_story(story)?;
        if usize::from(def.trigger.entity) >= script.entity_count() {
            return Err(SceneError::NoSuchEntity);
        }
        let mesh = field.walkmesh().map_err(|e| SceneError::Walkmesh(e.to_string()))?;
        let section = script.bytes().to_vec();
        let mut vm = Vm::with_resources(SceneVars::new(story.clone(), &def.given), seed, limits, resources);
        vm.setup(script, &def.setup);
        let setup = vm.take_stats();
        // A cast member the stage scripts did not place stands where the cast says, and the scene knows it.
        for m in &def.cast {
            if let Some(s) = &m.start {
                vm.seed(m.entity, spot_where(s), Some(field_direction_to_facing(s.direction)));
            }
        }
        vm.begin(def.root.clone());
        if let Some(formation) = def.resume_after_battle {
            if !vm.seek_after_battle(script, formation) {
                return Err(SceneError::BattleBoundary);
            }
        }
        let ground = Ground::new(mesh);
        let cast: BTreeSet<u8> = def.cast.iter().map(|m| m.entity).collect();
        let initial = Self::initial_stage(&vm, &ground, &def.cast);
        let initial_default_animations = def.cast.iter().filter_map(|member| {
            match vm.standing_default_animation_of(member.entity) {
                Some(Stage::Anim { animation, how: AnimHow::Loop, first: None, last: None, speed: 16, divisor: 1, .. }) => Some((member.entity, animation)),
                _ => None,
            }
        }).collect();
        let animation_clips = Self::animation_clips(field, script, &def.cast);
        let heights = initial.iter().filter_map(|event| match *event {
            Stage::Place { entity, z: Some(z), .. } => Some((entity, z)), _ => None,
        }).collect();
        Ok(Self { def: def.clone(), section, ground, vm, setup, initial, animation_clips, initial_default_animations, heights, cast, off_cast: 0, story: story.clone(), ended: None, seed, unchecked: false, strict: false, transcript: Vec::new() })
    }

    fn animation_clips(field: &FieldFile, script: &FieldScript<'_>, cast: &[CastMember]) -> BTreeMap<u8, Vec<String>> {
        let mut clips = BTreeMap::new();
        let Ok(loader) = field.model_loader() else { return clips };
        // CHAR's operand is a character id, not a loader index. Records are assigned in entity
        // order. An uncertain declaration can therefore make all following bindings uncertain.
        for init in script.entity_inits(None) {
            if init.decode_error.is_some() || init.char_id.as_ref().is_some_and(|c| {
                c.phase != crate::script::Phase::Init || c.conditional || c.may_be_overridden
            }) {
                break;
            }
            let Ok(entity) = u8::try_from(init.entity) else { continue };
            let Some(member) = cast.iter().find(|m| m.entity == entity) else { continue };
            let hrc = match &member.look {
                Look::Model { hrc, .. } => hrc.as_str(),
                // A canonical look may keep its own asset when it is the very same field rig.
                // Unknown ids and generic bodies must not borrow a fallback character's clips.
                Look::Appearance(id) => match crate::playable::resolve_appearance(id) {
                    Some(model) => model.hrc,
                    None => continue,
                },
            };
            let Some(record) = init.model_index.and_then(|index| loader.models.get(index)) else { continue };
            if !hrc.eq_ignore_ascii_case(&record.hrc_stem()) {
                continue;
            }
            clips.insert(entity, record.animations.iter().take(usize::from(u8::MAX) + 1).map(|a| a.stem()).collect());
        }
        clips
    }

    /// The cast's starting presentation, including default loops left by quiet setup.
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
            if let Some(animation) = vm.default_animation_of(member.entity) {
                out.push(animation.clone());
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

    /// The cast's placement, visibility and default loops when the scene begins.
    pub fn initial_stage_events(&self) -> &[Stage] {
        &self.initial
    }

    /// A model-loader slot for this exact cast skeleton, resolved from the script being played.
    /// Appearance substitutions, missing metadata and uncertain model bindings have no mapping.
    pub fn animation_clip(&self, entity: u8, slot: u8) -> Option<&str> {
        self.animation_clips.get(&entity)?.get(usize::from(slot)).map(String::as_str)
    }

    /// The standing default captured before visible playback, only at normal speed and on the
    /// script's exact field-model binding. This does not change the initial displayed pose.
    pub fn initial_default_animation_clip(&self, entity: u8) -> Option<(u8, &str)> {
        let slot = *self.initial_default_animations.get(&entity)?;
        Some((slot, self.animation_clip(entity, slot)?))
    }

    /// What the stage scripts did (their counters; the scene's own are [`ScenePlay::stats`]).
    pub fn setup_stats(&self) -> &Stats {
        &self.setup
    }

    pub fn resources(&self) -> &ResourceOutcome { self.vm.resources() }

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
        self.unchecked = true;
        self.advance(answer)
    }

    fn advance(&mut self, answer: Option<usize>) -> Beat {
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

    /// Advance only after acknowledging the exact window previously handed out. At the start
    /// or immediately after a checked battle resume, pass None. Validation never mutates the VM.
    pub fn next_strict(&mut self, ack: Option<WindowAck>) -> Result<Beat, SceneError> {
        if self.vm.has_implicit_choice() { return Err(SceneError::ImplicitAnswer); }
        if self.vm.has_invalid_message() { return Err(SceneError::WindowData); }
        if self.unchecked || self.def.resume_after_battle.is_some() { return Err(SceneError::UncheckedReplay); }
        match (self.boundary(), ack) {
            (Some(SceneBoundary::Window(expected)), Some(ack)) if expected == ack.checkpoint => {
                match (expected.choices, ack.answer) {
                    (Some([first, last]), Some(answer)) if (first..=last).contains(&answer) => {},
                    (None, None) => {},
                    _ => return Err(SceneError::WindowBoundary),
                }
                self.vm.accept_window(ack.answer);
                self.transcript.push(SceneInput::Window(ack));
            }
            (None, None) => {},
            _ => return Err(SceneError::WindowBoundary),
        }
        self.strict = true;
        let beat = self.advance(None);
        if self.vm.has_implicit_choice() { return Err(SceneError::ImplicitAnswer); }
        if self.vm.has_invalid_message() { return Err(SceneError::WindowData); }
        Ok(beat)
    }

    pub fn boundary(&self) -> Option<SceneBoundary> { self.vm.boundary() }

    pub fn replay_identity(&self) -> ReplayIdentity {
        ReplayIdentity { version: REPLAY_VERSION, fingerprint: super::replay_identity::fingerprint(&self.section, &self.def), seed: self.seed }
    }

    pub fn transcript(&self) -> &[SceneInput] { &self.transcript }

    pub fn snapshot(&self) -> Option<SceneSnapshot> {
        Some(SceneSnapshot {
            boundary: self.boundary()?, writes: self.writes(), resources: self.resources().clone(),
            party: self.vm.party_ops().to_vec(), jump: self.vm.destination(), setup_stats: self.setup.clone(),
            stats: self.vm.stats().clone(), unknown_persistent_reads: self.vm.vars().unknown_persistent_reads(),
        })
    }

    /// Resume once at the exact pending instruction. No scheduler, scratch, or RNG state is reset.
    /// Only authored mode 8 (no escape, ordinary defeat is game over) is currently supported.
    /// This policy follows the [original PSX battle setup] and [field return handler]; the owned
    /// PC bridge's mode 8 is measured, but original PC battle-return byte parity is not claimed.
    ///
    /// [original PSX battle setup]: https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/battle/batini.c#L1083
    /// [field return handler]: https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/main/110B8.c#L365
    ///
    /// The caller must first commit the segment and verify an actual victorious battle, then supply
    /// its authoritative afterimage. This method itself performs no persistence or combat.
    pub fn resume_battle_checked(&mut self, resume: BattleResume) -> Result<(), SceneError> {
        if self.unchecked || !self.strict { return Err(SceneError::UncheckedReplay); }
        if self.boundary() != Some(SceneBoundary::Battle(resume.checkpoint)) { return Err(SceneError::BattleBoundary); }
        if resume.checkpoint.flags != Some(8) { return Err(SceneError::BattleMode); }
        if !self.story.supports_extended() || !resume.story.supports_extended() || resume.resources.gil.is_none() {
            return Err(SceneError::BattleResources);
        }
        self.vm.resume_battle(&resume.story, resume.resources);
        self.story = resume.story.clone();
        self.ended = None;
        self.transcript.push(SceneInput::Battle(resume));
        Ok(())
    }

    /// Keep what concerns the cast, give every place its height, name the speaker of a window.
    fn refine(&mut self, beat: &mut Beat) {
        let before = beat.stage.len();
        beat.stage.retain(|t| self.cast.contains(&stage_entity(&t.what).unwrap_or(u8::MAX)) || stage_entity(&t.what).is_none());
        self.off_cast = self.off_cast.saturating_add(u32::try_from(before - beat.stage.len()).unwrap_or(u32::MAX));
        for Timed { what, .. } in &mut beat.stage {
            match what {
                Stage::Place { entity, x, y, z, triangle } => {
                    if z.is_none() {
                        *z = self.ground.height(*x, *y, *triangle, self.heights.get(entity).copied()).map(|h| h.round() as i16);
                    }
                    if let Some(z) = *z { self.heights.insert(*entity, z); }
                }
                Stage::Move { entity, x, y, z, triangle, how, .. } => {
                    // The authored landing triangle selects a jump's floor. Walking has no
                    // triangle: prefer its preceding surface where several floors overlap.
                    if z.is_none() {
                        let hint = matches!(*how, MoveHow::Walk | MoveHow::WalkKeepAnimation)
                            .then(|| self.heights.get(entity).copied()).flatten();
                        *z = self.ground.height(*x, *y, *triangle, hint).map(|h| h.round() as i16);
                    }
                    if let Some(z) = *z { self.heights.insert(*entity, z); }
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
        let extended = self.story.supports_extended();
        let byte = |bank: u8, address: u8| written.get(&base.key(bank, address)).copied().unwrap_or_else(|| {
            use crate::script::dialogue::StoryVars;
            if extended { base.baseline_byte(bank, address) } else { base.byte(bank, address) }
        });
        let mut out = Writes::default();
        let lo = byte(MOMENT_BANK, MOMENT_ADDRESS);
        let hi = byte(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1));
        let moment = u16::from_le_bytes([lo, hi]);
        if (written.contains_key(&base.key(MOMENT_BANK, MOMENT_ADDRESS)) || written.contains_key(&base.key(MOMENT_BANK, MOMENT_ADDRESS.wrapping_add(1)))) && moment != self.story.moment {
            out.moment = Some(moment);
        }
        for (&(bank, address), &value) in written {
            use crate::script::dialogue::StoryVars;
            if base.key(bank, address).0 == base.key(MOMENT_BANK, MOMENT_ADDRESS).0 && address <= MOMENT_ADDRESS.wrapping_add(1) {
                continue;
            }
            if extended && crate::story::canonical_persistent_bank(bank).is_some() {
                let mask = self.vm.written_mask(bank, address);
                // Explicitly writing zero establishes knowledge just as writing a nonzero value does.
                if mask & !self.story.known_mask(bank, address) != 0 || (value ^ self.story.byte(bank, address)) & mask != 0 {
                    out.persistent.insert((bank, address), (value & mask, mask));
                }
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

/// Bind the bridge's initially unplaced Cloud to the accepted crossing position. Both the
/// host and client call this before constructing the VM and its source fingerprint. This does
/// not verify that a crossing occurred: the authoritative world owns that movement test.
/// An explicit triangle hint must be valid; without one the nearest containing floor is used.
/// Other scenes do not accept a trigger-origin override.
pub fn with_trigger_position(field: &FieldFile, def: &SceneDef, pos: fmv_core::Pos, facing: f32, triangle: Option<u32>) -> Result<SceneDef, SceneError> {
    if def.id != super::data::BRIDGE_SCENE_ID || !pos.is_finite() || !facing.is_finite() { return Err(SceneError::SceneOrigin); }
    let [x, y] = [pos.x.round(), pos.y.round()];
    if [x, y].into_iter().any(|n| n < f32::from(i16::MIN) || n > f32::from(i16::MAX)) { return Err(SceneError::SceneOrigin); }
    let (x, y) = (x as i16, y as i16);
    let mesh = field.walkmesh().map_err(|e| SceneError::Walkmesh(e.to_string()))?;
    let height = |tri: &[Vertex; 3]| triangle_contains_xy(tri, i32::from(x), i32::from(y))
        .then(|| triangle_height_at(tri, i32::from(x), i32::from(y))).flatten();
    let triangle = match triangle {
        Some(index) => {
            let index = u16::try_from(index).map_err(|_| SceneError::SceneOrigin)?;
            mesh.triangles.get(usize::from(index)).and_then(height).ok_or(SceneError::SceneOrigin)?;
            index
        }
        None => mesh.triangles.iter().enumerate().filter_map(|(index, tri)| {
            Some((u16::try_from(index).ok()?, (height(tri)? - f64::from(pos.z)).abs()))
        }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(index, _)| index).ok_or(SceneError::SceneOrigin)?,
    };
    let mut bound = def.clone();
    let cloud = bound.cast.iter_mut().find(|member| member.entity == 1).ok_or(SceneError::SceneOrigin)?;
    // Bound the angle before the older converter's float-to-integer step, including huge but
    // finite protocol headings. Server and client use the same quantized turn.
    let facing = facing.rem_euclid(std::f32::consts::TAU);
    cloud.start = Some(Spot { x, y, triangle, direction: crate::facing_to_field_direction(facing) });
    Ok(bound)
}

/// Reconstruct to the first unacknowledged window, combat instruction, or terminal boundary.
/// Every supplied record must consume the exact next boundary. No choices are defaulted and no
/// battle is skipped by formation alone. Callers bind initial story/resources and seed durably.
pub fn replay_scene(field: &FieldFile, def: &SceneDef, story: &StoryState, resources: SceneResources, identity: ReplayIdentity, transcript: &[SceneInput]) -> Result<ScenePlay, SceneError> {
    let mut play = ScenePlay::with_resources(field, def, story, identity.seed, resources)?;
    if play.replay_identity() != identity { return Err(SceneError::ReplayIdentity); }
    play.next_strict(None)?;
    for input in transcript {
        match input {
            SceneInput::Window(ack) => { play.next_strict(Some(*ack))?; }
            SceneInput::Battle(resume) => {
                play.resume_battle_checked(resume.clone())?;
                play.next_strict(None)?;
            }
        }
    }
    Ok(play)
}

/// As [`replay_scene`], additionally requiring the exact recorded endpoint. A truncated transcript
/// cannot silently reconstruct an earlier valid window when a later boundary was committed.
pub fn replay_scene_to(field: &FieldFile, def: &SceneDef, story: &StoryState, resources: SceneResources, identity: ReplayIdentity, transcript: &[SceneInput], target: SceneBoundary) -> Result<ScenePlay, SceneError> {
    let play = replay_scene(field, def, story, resources, identity, transcript)?;
    if play.boundary() != Some(target) { return Err(SceneError::ReplayBoundary); }
    Ok(play)
}

fn stage_entity(stage: &Stage) -> Option<u8> {
    match stage {
        Stage::Place { entity, .. } | Stage::Move { entity, .. } | Stage::Face { entity, .. } | Stage::Anim { entity, .. } | Stage::AnimSpeed { entity, .. } | Stage::Show { entity, .. } => Some(*entity),
        Stage::Sound { .. } | Stage::Camera(_) => None,
    }
}

/// Run `def` in `field` to its end without a person: each window that asks takes the next of `answers` (the lines a
/// player chose, in order; when they run out, the cursor's starting line). What the server does to learn what the scene
/// wrote, from what the owner answered.
pub fn run_to_end(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, answers: &[usize]) -> Result<Outcome, SceneError> {
    outcome_of(ScenePlay::new(field, def, story, seed)?, answers, false)
}

/// [`run_to_end`] over a replacement script section (another edition of the field's script, with `def`'s entities found in
/// it by name: [`ScenePlay::with_script_section`]) on the original field's floor. What a client runs beside the original to
/// learn whether the replacement comes to the same story ([`Outcome::same_story_as`]) before it shows it.
pub fn run_section_to_end(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8], answers: &[usize]) -> Result<Outcome, SceneError> {
    outcome_of(ScenePlay::with_script_section(field, def, story, seed, section)?, answers, false)
}

/// Headless evaluation against the same resource snapshot the client received at scene start.
pub fn run_to_end_with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources, answers: &[usize]) -> Result<Outcome, SceneError> {
    outcome_of(ScenePlay::with_resources(field, def, story, seed, resources)?, answers, false)
}

/// Evaluate a durable scene against its exact resource snapshot and the player's complete answer
/// transcript. Every question on the selected path must have one in-range answer and no answers
/// may remain at the end. Unlike previews and surveys, this never substitutes a cursor default or
/// clamps an invalid choice before calculating persistent or economic effects.
pub fn run_to_end_with_resources_strict(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources, answers: &[usize]) -> Result<Outcome, SceneError> {
    outcome_of(ScenePlay::with_resources(field, def, story, seed, resources)?, answers, true)
}

/// Replacement-script equivalent of [`run_to_end_with_resources`].
pub fn run_section_to_end_with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, section: &[u8], resources: SceneResources, answers: &[usize]) -> Result<Outcome, SceneError> {
    outcome_of(ScenePlay::with_script_section_and_resources(field, def, story, seed, section, resources)?, answers, false)
}

fn outcome_of(mut play: ScenePlay, answers: &[usize], strict: bool) -> Result<Outcome, SceneError> {
    let mut answers = answers.iter().copied();
    let mut given: Option<usize> = None;
    let (mut windows, mut questions) = (0usize, 0usize);
    let end = loop {
        let beat = play.next(given.take());
        if strict && play.vm.has_implicit_choice() { return Err(SceneError::ImplicitAnswer); }
        if strict && play.vm.has_invalid_message() { return Err(SceneError::WindowData); }
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                windows += 1;
                if let Some(choice) = w.message.choice {
                    questions += 1;
                    given = answers.next();
                    if strict {
                        let answer = given.ok_or(SceneError::MissingAnswer { question: questions })?;
                        if !(choice.first..=choice.last).contains(&answer) {
                            return Err(SceneError::InvalidAnswer { question: questions, answer, first: choice.first, last: choice.last });
                        }
                    }
                }
            }
        }
    };
    if strict && answers.len() != 0 {
        return Err(SceneError::TrailingAnswers { remaining: answers.len() });
    }
    let writes = play.writes();
    Ok(Outcome {
        end,
        windows,
        questions,
        writes,
        jump: play.vm.destination(),
        setup_stats: play.setup_stats().clone(),
        unknown_persistent_reads: play.vm.vars().unknown_persistent_reads(),
        party: play.vm.party_ops().to_vec(),
        resources: play.resources().clone(),
        stats: play.stats().clone(),
    })
}

/// Every place the cast stands or walks through in one run of `def` (played to its end with the cursor's answers), as
/// `(x, y)`: the initial places, every place and turn's spot, and each walk sampled every 10 units. What a witness spot must keep
/// clear of ([`auto_witnesses`]).
pub fn cast_places(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32) -> Result<Vec<(f32, f32)>, SceneError> {
    cast_places_with_resources(field, def, story, seed, SceneResources::default())
}

pub fn cast_places_with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources) -> Result<Vec<(f32, f32)>, SceneError> {
    let mut play = ScenePlay::with_resources(field, def, story, seed, resources)?;
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
    auto_witnesses_with_resources(field, def, story, seed, SceneResources::default(), count)
}

pub fn auto_witnesses_with_resources(field: &FieldFile, def: &SceneDef, story: &StoryState, seed: u32, resources: SceneResources, count: usize) -> Vec<Spot> {
    let (Ok(script), Ok(mesh)) = (FieldScript::from_field(field), field.walkmesh()) else { return Vec::new() };
    let Ok(cast) = cast_places_with_resources(field, def, story, seed, resources) else { return Vec::new() };
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
    let story = def.prepare_story(story).ok()?;
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
