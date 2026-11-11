//! Model movement, turning, position queries, jumps, ladders, party split/join and walkmesh locks: `MOVE`, `CMOVE`, `FMOVE`, `MOVA`, `PMOVA`, `TURA`, `PTURA`, `TURNGEN`, `TURN`, `TURNW`, `DIRA`, `PDIRA`, `GETDIR`, `PGTDR`, `GETAI`, `GETAXY`, `AXYZI`, `PXYZI`, `JUMP`, `LADER`, `OFST`, `OFSTW`, `SPLIT`, `JOIN`, `IDLCK`, `SLIP`, `FCFIX`, `SIN`, `COS`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).
//!
//! # How the original structures it (and so this file)
//!
//! A blocking instruction (`MOVE`, `TURA`, `JUMP`, ...) does not wait by itself. On its first visit it *arms* a
//! motion on its model (a mode, an action state of 0) and yields without advancing the PC. The per-frame model
//! update, which runs after every entity has had its turn, performs the motion one step at a time and raises the
//! action state to 1 ("under way") and finally 2 ("arrived"). The instruction, visited again every frame, yields
//! while the state is 1 and, on seeing 2, disarms the motion and moves the PC on. Turns work the same way with a
//! turn kind (1 linear, 2 eased, 3 finished). [`update_motion`] is that per-frame update, in the original's order:
//! turns, offsets, scripted walks, jumps, vertical then horizontal ladders.
//!
//! Positions are kept in Q12 here (the `Model`'s whole-unit position is `q12 >> 12`). If the host or another
//! instruction moves a model, the Q12 value is re-seeded from the whole-unit position at the next visit.
//!
//! # Simplifications (deliberate)
//!
//! * A scripted walk heads straight for its target at the model's speed. The original slides along the walkmesh
//!   and is pushed off other solid models; here the walkmesh only supplies the triangle and height under the model
//!   after each step (a point query), and nothing ever blocks a scripted walk (a script waiting on a `MOVE` that
//!   can never arrive would stall its entity).
//! * Ladders climb on their own unless the player model has control, when the keys drive them (the host must then
//!   let the model's position stand: while the player has control it normally overwrites it every frame).
//! * An instruction whose arithmetic would divide by zero in the original (a zero-step turn, offset or ladder, a
//!   zero split speed) is counted as an anomaly and completes at once instead.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::super::{FieldRuntime, Flow, Model, MoveMode, AnimStatus, NO_CHARACTER};
use crate::field::Walkmesh;
use crate::script::scene::{landing_height, JumpMotion};
use crate::walk::Surface;

// ---- opcodes ---------------------------------------------------------------------------------------------------

const JOIN: u8 = 0x08;
const SPLIT: u8 = 0x09;
const PMOVA: u8 = 0x2A;
const SLIP: u8 = 0x2B;
const PDIRA: u8 = 0x34;
const PTURA: u8 = 0x35;
const IDLCK: u8 = 0x6D;
const PGTDR: u8 = 0x73;
const PXYZI: u8 = 0x75;
const MOVE: u8 = 0xA8;
const CMOVE: u8 = 0xA9;
const MOVA: u8 = 0xAA;
const TURA: u8 = 0xAB;
const FMOVE: u8 = 0xAD;
const TURNGEN: u8 = 0xB4;
const TURN: u8 = 0xB5;
const DIRA: u8 = 0xB6;
const GETDIR: u8 = 0xB7;
const GETAXY: u8 = 0xB8;
const GETAI: u8 = 0xB9;
const JUMP: u8 = 0xC0;
const AXYZI: u8 = 0xC1;
const LADER: u8 = 0xC2;
const OFST: u8 = 0xC3;
const OFSTW: u8 = 0xC4;
const SIN: u8 = 0xD4;
const COS: u8 = 0xD5;
const FCFIX: u8 = 0xDB;
const TURNW: u8 = 0xDE;

/// Pad bits (original layout) the ladders read.
const PAD_UP: u16 = 0x1000;
const PAD_RIGHT: u16 = 0x2000;
const PAD_DOWN: u16 = 0x4000;
const PAD_LEFT: u16 = 0x8000;

/// A turn's kinds.
const TURN_NONE: u8 = 0;
const TURN_LINEAR: u8 = 1;
const TURN_EASED: u8 = 2;
const TURN_DONE: u8 = 3;

/// A walk shorter than this many units squared is finished at once (the original's arrival test).
const ARRIVAL_SQ: i64 = 4;

// ---- per-model state -------------------------------------------------------------------------------------------

/// What a model's scripted motion is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Walk,
    Jump,
    LadderV,
    LadderH,
}

impl Phase {
    /// The `Model::move_mode` that goes with this phase.
    fn mode(self) -> MoveMode {
        match self {
            Phase::Idle => MoveMode::None,
            Phase::Walk => MoveMode::Walk,
            Phase::Jump => MoveMode::Jump,
            Phase::LadderV | Phase::LadderH => MoveMode::Ladder,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Turn {
    kind: u8,
    start: i32,
    end: i32,
    steps: i32,
    step: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Offset {
    /// 0 instant, 1 linear, 2 eased, 3 finished.
    kind: u8,
    step: i32,
    steps: i32,
    start: [i32; 3],
    end: [i32; 3],
    cur: [i32; 3],
}

/// One model's motion state.
#[derive(Debug, Clone, Default, PartialEq)]
struct ModelMotion {
    /// Position in Q12 and the whole-unit position last written to the `Model` (they differ when something else
    /// moved the model, which re-seeds the Q12 value).
    pos: [i32; 3],
    synced: [i32; 3],
    seeded: bool,
    phase: Phase,
    /// 0 armed, 1 under way, 2 arrived.
    state: u8,
    /// A walk's arrival radius (`MOVA`: the target's solid range), or a ladder's direction.
    arg: i32,
    dir_lock: bool,
    start: [i32; 3],
    end: [i32; 3],
    end_tri: u16,
    steps: i32,
    step: i32,
    move_dir: u8,
    jump: Option<JumpMotion>,
    /// A walk speed that overrides the model's (`SPLIT`/`JOIN` time their walks).
    speed_override: Option<i32>,
    /// The model's animation speed when the first ladder armed on it (`LADER` divides the base, not the last result).
    base_anim_speed: Option<u16>,
    turn: Turn,
    offset: Offset,
}

impl ModelMotion {
    /// Bring this state in line with the model: re-seed the Q12 position if the model was moved from outside,
    /// and drop a motion the model no longer has (a higher-priority script preempted it).
    fn sync(&mut self, model: &Model) {
        let whole = [model.x, model.y, model.z];
        if !self.seeded || whole != self.synced {
            self.pos = whole.map(|v| v.wrapping_shl(12));
            self.synced = whole;
            self.seeded = true;
        }
        if self.phase.mode() != model.move_mode {
            self.phase = Phase::Idle;
            self.state = 0;
            self.speed_override = None;
        }
    }

    /// Publish the Q12 position to the model's whole units.
    fn store(&mut self, model: &mut Model) {
        let whole = self.pos.map(|v| v >> 12);
        (model.x, model.y, model.z) = (whole[0], whole[1], whole[2]);
        self.synced = whole;
    }

    fn arm(&mut self, model: &mut Model, phase: Phase) {
        self.phase = phase;
        self.state = 0;
        model.move_mode = phase.mode();
        model.move_to = (phase == Phase::Walk).then_some((self.end[0] >> 12, self.end[1] >> 12));
    }

    fn disarm(&mut self, model: &mut Model) {
        self.phase = Phase::Idle;
        self.state = 0;
        self.speed_override = None;
        model.move_mode = MoveMode::None;
        model.move_to = None;
    }
}

/// A walkmesh's lookup structure, built once per walkmesh.
#[derive(Debug, Clone, Default)]
struct SurfaceCache {
    key: usize,
    surface: Option<Arc<Surface>>,
}

impl PartialEq for SurfaceCache {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl SurfaceCache {
    fn get(&mut self, mesh: Option<&Arc<Walkmesh>>) -> Option<Arc<Surface>> {
        let mesh = mesh?;
        let key = Arc::as_ptr(mesh) as usize;
        if self.key != key || self.surface.is_none() {
            self.surface = Some(Arc::new(Surface::new(mesh)));
            self.key = key;
        }
        self.surface.clone()
    }
}

/// This family's state (per model where needed, indexed by model id).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MotionState {
    models: Vec<ModelMotion>,
    /// Walkmesh triangles `IDLCK` has locked.
    locked: BTreeSet<u16>,
    /// Entities whose line `SLIP` switched slipping off.
    slip_off: BTreeSet<usize>,
    /// `SPLIT`/`JOIN`'s per-entity progress (0 idle, 1 walking, 2 turning, 3 done).
    split_join: BTreeMap<usize, u8>,
    /// The player's control lock as it was before a `SPLIT`/`JOIN` took control.
    saved_lock: Option<bool>,
    surface: SurfaceCache,
}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    matches!(
        op,
        JOIN | SPLIT
            | PMOVA
            | SLIP
            | PDIRA
            | PTURA
            | IDLCK
            | PGTDR
            | PXYZI
            | MOVE
            | CMOVE
            | MOVA
            | TURA
            | FMOVE
            | TURNGEN
            | TURN
            | DIRA
            | GETDIR
            | GETAXY
            | GETAI
            | JUMP
            | AXYZI
            | LADER
            | OFST
            | OFSTW
            | SIN
            | COS
            | FCFIX
            | TURNW
    )
}

// ---- arithmetic the original's helpers perform -----------------------------------------------------------------

/// `sin` of a field direction (256 to the turn) in Q12.
fn sin_q12(dir: u8) -> i32 {
    (4096.0 * (f64::from(dir) / 256.0 * std::f64::consts::TAU).sin()).trunc() as i32
}

/// `cos` of a field direction in Q12.
fn cos_q12(dir: u8) -> i32 {
    (4096.0 * (f64::from(dir) / 256.0 * std::f64::consts::TAU).cos()).trunc() as i32
}

/// The field direction (0-255) of the way from `from` to `to`: 64 faces +x, 128 faces +y, 0 faces -y, 192 faces
/// -x (the convention of [`crate::area_content::field_direction_to_facing`]).
fn dir_by_vec(from: (i32, i32), to: (i32, i32)) -> u8 {
    let (dx, dy) = (f64::from(to.0.wrapping_sub(from.0)), f64::from(to.1.wrapping_sub(from.1)));
    let units = (64.0 + dy.atan2(dx) * 256.0 / std::f64::consts::TAU).round() as i64;
    units.rem_euclid(256) as u8
}

/// The distance in whole units.
fn distance(from: (i32, i32), to: (i32, i32)) -> i32 {
    let (dx, dy) = (i64::from(to.0) - i64::from(from.0), i64::from(to.1) - i64::from(from.1));
    ((dx * dx + dy * dy) as f64).sqrt() as i32
}

/// `f` applied to the three axes of two vectors.
fn zip3(a: [i32; 3], b: [i32; 3], f: impl Fn(i32, i32) -> i32) -> [i32; 3] {
    let ([a0, a1, a2], [b0, b1, b2]) = (a, b);
    [f(a0, b0), f(a1, b1), f(a2, b2)]
}

/// Linear interpolation, with the original's rounding (a product that could overflow is divided first).
fn linear_step(start: i32, target: i32, duration: i32, step: i32) -> i32 {
    if duration == 0 {
        return target;
    }
    let delta = target.wrapping_sub(start);
    if delta > -524_288 && delta < 524_288 {
        start.wrapping_add(delta.wrapping_mul(step).wrapping_div(duration))
    } else {
        start.wrapping_add(delta.wrapping_div(duration).wrapping_mul(step))
    }
}

/// Ease-in-out interpolation along half a cosine.
fn ease_step(start: i32, target: i32, duration: i32, step: i32) -> i32 {
    if duration == 0 {
        return target;
    }
    let angle = (step.wrapping_shl(12).wrapping_div(duration) / 32).wrapping_add(128) as u8;
    let delta = target.wrapping_sub(start);
    start.wrapping_add((cos_q12(angle) + 4096).wrapping_mul(delta) / 8192)
}

/// The shortest-way wrap a turn's end direction gets when it is more than half a turn from the start.
fn shortest_way(start: i32, end: i32) -> i32 {
    if (end - start).abs() >= 129 {
        if end > start {
            end - 256
        } else {
            end + 256
        }
    } else {
        end
    }
}

/// Arm a turn from `model`'s direction towards `end` (already wrapped).
fn begin_turn(turn: &mut Turn, dir: u8, kind: u8, steps: i32, end: i32) {
    turn.start = i32::from(dir);
    turn.kind = kind;
    turn.steps = steps;
    turn.end = end;
}

/// One frame of a model's turn.
fn update_turn(model: &mut Model, turn: &mut Turn, anomalies: &mut u64) {
    let eased = match turn.kind {
        TURN_LINEAR => false,
        TURN_EASED => true,
        _ => return,
    };
    if turn.steps <= 0 {
        // Zero steps divides by zero in the original.
        *anomalies += 1;
        model.dir = turn.end as u8;
        turn.kind = TURN_DONE;
        return;
    }
    let value = if eased { ease_step(turn.start, turn.end, turn.steps, turn.step) } else { linear_step(turn.start, turn.end, turn.steps, turn.step) };
    model.dir = value as u8;
    if turn.step == turn.steps {
        turn.kind = TURN_DONE;
    } else {
        turn.step += 1;
    }
}

/// One frame of a model's render offset.
fn update_offset(offset: &mut Offset, anomalies: &mut u64) {
    let eased = match offset.kind {
        1 => false,
        2 => true,
        _ => return,
    };
    if offset.steps <= 0 {
        *anomalies += 1;
        offset.cur = offset.end;
        offset.kind = 3;
        return;
    }
    let (steps, step) = (offset.steps, offset.step);
    offset.cur = zip3(offset.start, offset.end, |from, to| if eased { ease_step(from, to, steps, step) } else { linear_step(from, to, steps, step) });
    if offset.step == offset.steps {
        offset.kind = 3;
    } else {
        offset.step += 1;
    }
}

/// Which animation a walk of this speed plays: the run clip above three times the field scale, else the walk clip.
fn walk_anim(speed: i32, scale: i32) -> u8 {
    if speed > scale.wrapping_mul(3) {
        2
    } else {
        1
    }
}

/// One frame of a scripted walk.
fn update_walk(model: &mut Model, mm: &mut ModelMotion, surface: Option<&Surface>) {
    let (cx, cy) = (mm.pos[0] >> 12, mm.pos[1] >> 12);
    let (tx, ty) = (mm.end[0] >> 12, mm.end[1] >> 12);
    let (dx, dy) = (i64::from(tx) - i64::from(cx), i64::from(ty) - i64::from(cy));
    let dist_sq = dx * dx + dy * dy;
    let reach = i64::from(model.solid_range) + i64::from(mm.arg);
    let speed = i64::from(mm.speed_override.unwrap_or_else(|| i32::from(model.move_speed)));
    // Arrival: inside the target's radius, or within one step of the target (which is then taken exactly).
    if mm.arg != 0 && dist_sq <= reach * reach + 4096 {
        mm.state = 2;
        return;
    }
    if dist_sq < (speed * speed) >> 16 || dist_sq < ARRIVAL_SQ {
        mm.pos[0] = mm.end[0];
        mm.pos[1] = mm.end[1];
        mm.state = 2;
        mm.store(model);
        return;
    }
    mm.move_dir = dir_by_vec((cx, cy), (tx, ty));
    mm.state = 1;
    let (vx, vy) = (i64::from(sin_q12(mm.move_dir)), -i64::from(cos_q12(mm.move_dir)));
    mm.pos[0] = mm.pos[0].wrapping_add((speed * vx / 256) as i32);
    mm.pos[1] = mm.pos[1].wrapping_add((speed * vy / 256) as i32);
    if !mm.dir_lock {
        model.dir = mm.move_dir;
    }
    settle_on_mesh(model, mm, surface);
    mm.store(model);
}

/// Put the model's triangle and height on the walkmesh under it (a point query; nothing blocks the move).
fn settle_on_mesh(model: &mut Model, mm: &mut ModelMotion, surface: Option<&Surface>) {
    let Some(surface) = surface else { return };
    let (x, y) = (f64::from(mm.pos[0]) / 4096.0, f64::from(mm.pos[1]) / 4096.0);
    if let Some((tri, z)) = surface.locate(x, y, Some(f64::from(mm.pos[2]) / 4096.0)) {
        model.triangle = u16::try_from(tri).unwrap_or(model.triangle);
        mm.pos[2] = (z.round() as i32).wrapping_shl(12);
    }
}

/// One frame of a jump.
fn update_jump(model: &mut Model, mm: &mut ModelMotion, mesh: Option<&Walkmesh>, anomalies: &mut u64) {
    if mm.state == 0 {
        mm.start = mm.pos;
        let landing = mesh
            .and_then(|w| w.triangles.get(usize::from(mm.end_tri)))
            .and_then(|tri| landing_height(tri, (mm.end[0] / 4096) as i16, (mm.end[1] / 4096) as i16))
            .map_or(mm.pos[2], |z| i32::from(z).wrapping_shl(12));
        mm.end[2] = landing;
        mm.step = 0;
        mm.jump = i16::try_from(mm.steps).ok().and_then(|steps| JumpMotion::from_fixed(mm.pos, [mm.end[0], mm.end[1], landing], steps));
        if mm.jump.is_none() {
            // A jump of no steps divides by zero in the original; land at once.
            *anomalies += 1;
            mm.pos = mm.end;
            model.triangle = mm.end_tri;
            mm.state = 2;
        } else {
            mm.state = 1;
        }
    } else if mm.step >= mm.steps {
        model.triangle = mm.end_tri;
        mm.state = 2;
    } else {
        mm.step += 1;
        if let Some(jump) = mm.jump {
            mm.pos = jump.fixed_at(u32::try_from(mm.step).unwrap_or(0));
        }
    }
    mm.store(model);
}

/// One frame of a ladder climb. `keys` is the player's held pad when the player's own model is climbing.
fn update_ladder(model: &mut Model, mm: &mut ModelMotion, horizontal: bool, keys: Option<u16>, anomalies: &mut u64) {
    if mm.state == 0 {
        mm.start = mm.pos;
        let [dx, dy, dz] = zip3(mm.end, mm.start, |to, from| to.wrapping_sub(from) / 4096).map(i64::from);
        mm.steps = (((dx * dx + dy * dy + dz * dz) as f64).sqrt() as i32) / 4;
        mm.step = 0;
        mm.state = 1;
        return;
    }
    // `arg` 0 climbs towards the end with the "forward" key (up for a vertical ladder, right for a horizontal one),
    // 1 with the other.
    let (back, forward) = if horizontal { (PAD_LEFT, PAD_RIGHT) } else { (PAD_UP, PAD_DOWN) };
    let (back, forward) = if mm.arg == 0 { (back, forward) } else { (forward, back) };
    let go_back = |mm: &mut ModelMotion| {
        if mm.step == 0 {
            mm.state = 2;
        } else {
            mm.step -= 1;
        }
    };
    let go_forward = |mm: &mut ModelMotion, model: &mut Model| {
        if mm.step >= mm.steps {
            model.triangle = mm.end_tri;
            mm.state = 2;
        } else {
            mm.step += 1;
        }
    };
    match keys {
        Some(held) => {
            if held & back != 0 {
                go_back(mm);
            }
            if held & forward != 0 {
                go_forward(mm, model);
            }
        }
        None => go_forward(mm, model),
    }
    if mm.steps <= 0 {
        *anomalies += 1;
        mm.pos = mm.end;
    } else {
        let (steps, step) = (mm.steps, mm.step);
        mm.pos = zip3(mm.start, mm.end, |from, to| linear_step(from, to, steps, step));
    }
    mm.store(model);
}

// ---- the instructions ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveKind {
    /// `MOVE`: plays the walk/run clip.
    Move,
    /// `FMOVE`: the same without touching the animation.
    Fmove,
    /// `CMOVE`: keeps the model's facing.
    Cmove,
}

/// Where `SPLIT` sends a member and which way it ends up facing.
#[derive(Debug, Clone, Copy)]
struct SplitTarget {
    x: i32,
    y: i32,
    dir: u8,
}

impl FieldRuntime {
    /// The current entity's model, if it has one.
    fn cur_model(&self) -> Option<usize> {
        self.ents.get(self.current).and_then(|e| e.model).filter(|m| *m < self.models.len())
    }

    /// The model of entity `e`, if any.
    fn entity_model(&self, e: usize) -> Option<usize> {
        self.model_of(e).filter(|m| *m < self.models.len())
    }

    /// The entity of the character in party slot `slot`.
    fn party_entity(&self, slot: usize) -> Option<usize> {
        let ch = *self.party().get(slot)?;
        if ch == NO_CHARACTER {
            return None;
        }
        self.char_entity.get(usize::from(ch)).copied().flatten()
    }

    /// A model and its motion state, brought in line with each other.
    fn model_pair(&mut self, m: usize) -> Option<(&mut Model, &mut ModelMotion)> {
        if m >= self.models.len() {
            return None;
        }
        if self.motion.models.len() <= m {
            self.motion.models.resize_with(m + 1, ModelMotion::default);
        }
        let model = self.models.get_mut(m)?;
        let mm = self.motion.models.get_mut(m)?;
        mm.sync(model);
        Some((model, mm))
    }

    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_motion(&mut self, op: u8) -> Option<Flow> {
        if !implemented(op) {
            return None;
        }
        Some(match op {
            MOVE => self.op_move(MoveKind::Move),
            FMOVE => self.op_move(MoveKind::Fmove),
            CMOVE => self.op_move(MoveKind::Cmove),
            MOVA => {
                let target = usize::from(self.p8(1));
                self.op_mova(Some(target))
            }
            PMOVA => {
                let target = self.party_entity(usize::from(self.p8(1)));
                self.op_mova(target)
            }
            TURA => {
                let target = usize::from(self.p8(1));
                self.op_tura(Some(target))
            }
            PTURA => {
                let target = self.party_entity(usize::from(self.p8(1)));
                self.op_tura(target)
            }
            DIRA => {
                let target = usize::from(self.p8(1));
                self.op_dira(Some(target))
            }
            PDIRA => {
                let target = self.party_entity(usize::from(self.p8(1)));
                self.op_dira(target)
            }
            TURNGEN => self.op_turngen(),
            TURN => self.op_turn(),
            TURNW => self.op_turnw(),
            JUMP => self.op_jump(),
            LADER => self.op_lader(),
            OFST => self.op_ofst(),
            OFSTW => self.op_ofstw(),
            JOIN => self.op_split_join(false),
            SPLIT => self.op_split_join(true),
            IDLCK => {
                let id = self.p16(1);
                if self.p8(3) != 0 {
                    self.motion.locked.insert(id);
                } else {
                    self.motion.locked.remove(&id);
                }
                self.advance(4);
                Flow::Continue
            }
            SLIP => {
                let entity = self.current;
                if self.p8(1) != 0 {
                    self.motion.slip_off.insert(entity);
                } else {
                    self.motion.slip_off.remove(&entity);
                }
                self.advance(2);
                Flow::Continue
            }
            FCFIX => {
                let lock = self.p8(1) != 0;
                if let Some((_, mm)) = self.cur_model().and_then(|m| self.model_pair(m)) {
                    mm.dir_lock = lock;
                }
                self.advance(2);
                Flow::Continue
            }
            SIN | COS => {
                let angle = self.rd16(1, 3);
                let (mul, add) = (i32::from(self.rd16(2, 5)), i32::from(self.rd16(3, 7)));
                // The angle is 4096 to the turn (the console library's table, in Q12).
                let turn = f64::from(angle & 0xFFF) / 4096.0 * std::f64::consts::TAU;
                let unit = if op == SIN { turn.sin() } else { turn.cos() };
                let value = ((4096.0 * unit).round() as i32).wrapping_mul(mul).wrapping_add(add) >> 12;
                self.wr16(4, 9, value as i16);
                self.advance(10);
                Flow::Continue
            }
            GETDIR => {
                let model = self.entity_model(usize::from(self.p8(2)));
                if let Some(dir) = model.and_then(|m| self.models.get(m)).map(|m| m.dir) {
                    self.wr8(2, 3, dir);
                }
                self.advance(4);
                Flow::Continue
            }
            PGTDR => {
                let slot = usize::from(self.p8(2));
                let model = if slot < 3 { self.party_entity(slot).and_then(|e| self.entity_model(e)) } else { None };
                if let Some(dir) = model.and_then(|m| self.models.get(m)).map(|m| m.dir) {
                    self.wr8(2, 3, dir);
                }
                self.advance(4);
                Flow::Continue
            }
            GETAI => {
                let model = self.entity_model(usize::from(self.p8(2)));
                if let Some(tri) = model.and_then(|m| self.models.get(m)).map(|m| m.triangle) {
                    self.wr16(2, 3, tri as i16);
                }
                self.advance(4);
                Flow::Continue
            }
            GETAXY => {
                let model = self.entity_model(usize::from(self.p8(2)));
                if let Some((x, y)) = model.and_then(|m| self.models.get(m)).map(|m| (m.x, m.y)) {
                    self.wr16(1, 3, x as i16);
                    self.wr16(2, 4, y as i16);
                }
                self.advance(5);
                Flow::Continue
            }
            AXYZI => {
                let model = self.entity_model(usize::from(self.p8(3)));
                self.write_position(model, 4);
                self.advance(8);
                Flow::Continue
            }
            PXYZI => {
                let slot = usize::from(self.p8(3));
                let model = if slot < 3 { self.party_entity(slot).and_then(|e| self.entity_model(e)) } else { None };
                self.write_position(model, 4);
                self.advance(8);
                Flow::Continue
            }
            _ => return None,
        })
    }

    /// Write a model's x, y, z and triangle to the four variables of `AXYZI`/`PXYZI` (selectors 1-4, addresses
    /// from byte `first`).
    fn write_position(&mut self, model: Option<usize>, first: usize) {
        let Some(m) = model.and_then(|m| self.models.get(m)) else { return };
        let (x, y, z, tri) = (m.x, m.y, m.z, m.triangle);
        self.wr16(1, first, x as i16);
        self.wr16(2, first + 1, y as i16);
        self.wr16(3, first + 2, z as i16);
        self.wr16(4, first + 3, tri as i16);
    }

    /// `MOVE`, `FMOVE`, `CMOVE`.
    fn op_move(&mut self, kind: MoveKind) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(6);
            return Flow::Continue;
        };
        let target = (i32::from(self.rd16(1, 2)), i32::from(self.rd16(2, 4)));
        let scale = i32::from(self.scale);
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(6);
            return Flow::Continue;
        };
        mm.arg = 0;
        mm.dir_lock = kind == MoveKind::Cmove;
        mm.end[0] = target.0.wrapping_shl(12);
        mm.end[1] = target.1.wrapping_shl(12);
        if kind == MoveKind::Move {
            model.anim = walk_anim(i32::from(model.move_speed), scale);
            model.anim_speed = 16;
            model.anim_status = AnimStatus::ScriptedLoop;
        }
        if mm.phase == Phase::Walk {
            match mm.state {
                1 => return Flow::Yield,
                2 => {
                    mm.disarm(model);
                    mm.dir_lock = false;
                    if kind == MoveKind::Move {
                        model.anim = 0;
                        model.anim_status = AnimStatus::DefaultLoop;
                    }
                    self.advance(6);
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        mm.arm(model, Phase::Walk);
        Flow::Yield
    }

    /// `MOVA` and `PMOVA`: walk to another entity's model, stopping at its solid range.
    fn op_mova(&mut self, target: Option<usize>) -> Flow {
        let scale = i32::from(self.scale);
        let (Some(mi), Some(ti)) = (self.cur_model(), target.and_then(|t| self.entity_model(t))) else {
            self.advance(2);
            return Flow::Continue;
        };
        let Some(t) = self.models.get(ti) else {
            self.advance(2);
            return Flow::Continue;
        };
        let (tx, ty, radius) = (t.x, t.y, i32::from(t.solid_range));
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(2);
            return Flow::Continue;
        };
        mm.arg = radius;
        mm.dir_lock = false;
        mm.end[0] = tx.wrapping_shl(12);
        mm.end[1] = ty.wrapping_shl(12);
        if mm.phase == Phase::Walk {
            match mm.state {
                1 => {
                    model.anim = walk_anim(i32::from(model.move_speed), scale);
                    model.anim_speed = 16;
                    model.anim_status = AnimStatus::ScriptedLoop;
                    return Flow::Yield;
                }
                2 => {
                    mm.disarm(model);
                    model.anim = 0;
                    model.anim_status = AnimStatus::DefaultLoop;
                    self.advance(2);
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        mm.arm(model, Phase::Walk);
        Flow::Yield
    }

    /// The facing from the current model to entity `target`'s model (one unit aside if they share x and y).
    fn direction_to(&self, mi: usize, ti: usize) -> Option<u8> {
        let (a, b) = (self.models.get(mi)?, self.models.get(ti)?);
        let from = if (a.x, a.y) == (b.x, b.y) { (a.x.wrapping_add(1), a.y) } else { (a.x, a.y) };
        Some(dir_by_vec(from, (b.x, b.y)))
    }

    /// `DIRA` and `PDIRA`: face an entity at once (the instruction both advances and ends the entity's turn).
    fn op_dira(&mut self, target: Option<usize>) -> Flow {
        let (Some(mi), Some(ti)) = (self.cur_model(), target.and_then(|t| self.entity_model(t))) else {
            self.advance(2);
            return Flow::Continue;
        };
        let Some(dir) = self.direction_to(mi, ti) else {
            self.advance(2);
            return Flow::Continue;
        };
        if let Some((model, mm)) = self.model_pair(mi) {
            model.dir = dir;
            mm.turn.kind = TURN_NONE;
            mm.turn.step = 0;
        }
        self.advance(2);
        Flow::Yield
    }

    /// `TURA` and `PTURA`: turn to face an entity over a number of frames, waiting for the turn to finish.
    fn op_tura(&mut self, target: Option<usize>) -> Flow {
        let (Some(mi), Some(ti)) = (self.cur_model(), target.and_then(|t| self.entity_model(t))) else {
            self.advance(4);
            return Flow::Continue;
        };
        let (steps, way) = (i32::from(self.p8(2)), self.p8(3));
        let end = self.direction_to(mi, ti).map_or(0, i32::from);
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(4);
            return Flow::Continue;
        };
        if mm.turn.kind == TURN_DONE {
            mm.turn = Turn::default();
            self.advance(4);
            return Flow::Continue;
        }
        if mm.turn.step == 0 || mm.turn.kind != TURN_EASED || mm.turn.steps != steps {
            let start = i32::from(model.dir);
            let end = match way {
                2 => shortest_way(start, end),
                1 if start < end => end - 256,
                0 if end < start => end + 256,
                _ => end,
            };
            begin_turn(&mut mm.turn, model.dir, TURN_EASED, steps, end);
        }
        Flow::Yield
    }

    /// `TURNGEN`: turn to a direction over a number of frames in a chosen way, waiting for it to finish.
    fn op_turngen(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(6);
            return Flow::Continue;
        };
        let (way, steps, kind) = (self.p8(3), i32::from(self.p8(4)), self.p8(5));
        let end = i32::from(self.rd8(2, 2));
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(6);
            return Flow::Continue;
        };
        if mm.turn.kind == TURN_DONE {
            mm.turn = Turn::default();
            self.advance(6);
            return Flow::Continue;
        }
        if mm.turn.step == 0 || mm.turn.kind != kind || mm.turn.steps != steps {
            if kind != TURN_LINEAR && kind != TURN_EASED {
                // A turn of no known kind never finishes in the original.
                self.coverage.anomalies += 1;
                self.advance(6);
                return Flow::Continue;
            }
            let start = i32::from(model.dir);
            let end = match way {
                2 => shortest_way(start, end),
                1 if start < end => end - 256,
                0 if end < start => end + 256,
                _ => end,
            };
            begin_turn(&mut mm.turn, model.dir, kind, steps, end);
        }
        Flow::Yield
    }

    /// `TURN`: like `TURNGEN` without the choice of way; the end direction is a 16-bit operand.
    fn op_turn(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(6);
            return Flow::Continue;
        };
        let (steps, kind) = (i32::from(self.p8(4)), self.p8(5));
        let end = i32::from(self.rd16(2, 2));
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(6);
            return Flow::Continue;
        };
        if mm.turn.kind == TURN_DONE {
            mm.turn = Turn::default();
            self.advance(6);
            return Flow::Continue;
        }
        if mm.turn.step != 0 && mm.turn.end == end && mm.turn.kind == kind && mm.turn.steps == steps {
            return Flow::Yield;
        }
        if kind != TURN_LINEAR && kind != TURN_EASED {
            self.coverage.anomalies += 1;
            self.advance(6);
            return Flow::Continue;
        }
        begin_turn(&mut mm.turn, model.dir, kind, steps, end);
        Flow::Yield
    }

    /// `TURNW`: wait for the model's turn to finish.
    fn op_turnw(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(1);
            return Flow::Continue;
        };
        let Some((_, mm)) = self.model_pair(mi) else {
            self.advance(1);
            return Flow::Continue;
        };
        match mm.turn.kind {
            TURN_NONE => {
                self.advance(1);
                Flow::Continue
            }
            TURN_DONE => {
                mm.turn = Turn::default();
                self.advance(1);
                Flow::Continue
            }
            _ => Flow::Yield,
        }
    }

    /// `JUMP`: an arc to a point on a triangle over a number of steps (reuses the scene interpreter's verified
    /// trajectory); the instruction waits until the model has landed.
    fn op_jump(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(11);
            return Flow::Continue;
        };
        let (x, y, tri, steps) = (i32::from(self.rd16(1, 3)), i32::from(self.rd16(2, 5)), self.rd16(3, 7) as u16, i32::from(self.rd16(4, 9)));
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(11);
            return Flow::Continue;
        };
        if mm.phase == Phase::Jump {
            match mm.state {
                1 => return Flow::Yield,
                2 => {
                    mm.disarm(model);
                    self.advance(11);
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        mm.end[0] = x.wrapping_shl(12);
        mm.end[1] = y.wrapping_shl(12);
        mm.end_tri = tri;
        mm.steps = steps;
        mm.arm(model, Phase::Jump);
        Flow::Yield
    }

    /// `LADER`: climb to a point over a distance-derived number of steps, playing a clip.
    fn op_lader(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(15);
            return Flow::Continue;
        };
        let end = [i32::from(self.rd16(1, 3)), i32::from(self.rd16(2, 5)), i32::from(self.rd16(3, 7))];
        let (tri, way, anim, dir, speed) = (self.rd16(4, 9) as u16, self.p8(11), self.p8(12), self.p8(13), i32::from(self.p8(14)));
        let Some((model, mm)) = self.model_pair(mi) else {
            self.advance(15);
            return Flow::Continue;
        };
        if matches!(mm.phase, Phase::LadderV | Phase::LadderH) {
            match mm.state {
                1 => return Flow::Yield,
                2 => {
                    mm.disarm(model);
                    self.advance(15);
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        let (phase, arg) = match way {
            0 => (Phase::LadderV, 0),
            1 => (Phase::LadderV, 1),
            2 => (Phase::LadderH, 0),
            3 => (Phase::LadderH, 1),
            // Another way keeps the previous mode in the original; a first-time ladder falls back to vertical.
            _ => (if mm.phase == Phase::LadderH { Phase::LadderH } else { Phase::LadderV }, mm.arg),
        };
        mm.arg = arg;
        mm.end = end.map(|v| v.wrapping_shl(12));
        mm.end_tri = tri;
        model.anim = anim;
        let divisor = if speed == 0 { None } else { Some(speed) };
        let base = *mm.base_anim_speed.get_or_insert(model.anim_speed);
        model.anim_speed = divisor.map_or(base, |d| (i32::from(base) / d) as u16);
        model.anim_status = AnimStatus::DefaultLoop;
        model.dir = dir;
        mm.arm(model, phase);
        if divisor.is_none() {
            self.coverage.anomalies += 1;
        }
        Flow::Yield
    }

    /// `OFST`: move a model's render offset (instantly or over a number of steps); does not wait.
    fn op_ofst(&mut self) -> Flow {
        if let Some(mi) = self.cur_model() {
            let to = [i32::from(self.rd16(1, 4)), i32::from(self.rd16(2, 6)), i32::from(self.rd16(3, 8))];
            let (kind, steps) = (self.p8(3), i32::from(self.rd16(4, 10)));
            if let Some((_, mm)) = self.model_pair(mi) {
                let ofs = &mut mm.offset;
                ofs.step = 0;
                ofs.steps = steps;
                ofs.end = to;
                ofs.kind = kind;
                if kind != 0 {
                    ofs.start = ofs.cur;
                } else {
                    ofs.cur = to;
                }
            }
        }
        self.advance(12);
        Flow::Continue
    }

    /// `OFSTW`: wait for the model's offset motion to finish.
    fn op_ofstw(&mut self) -> Flow {
        let Some(mi) = self.cur_model() else {
            self.advance(1);
            return Flow::Continue;
        };
        let Some((_, mm)) = self.model_pair(mi) else {
            self.advance(1);
            return Flow::Continue;
        };
        if mm.offset.kind == 0 || mm.offset.kind == 3 {
            mm.offset.kind = 0;
            mm.offset.step = 0;
            mm.offset.steps = 0;
            self.advance(1);
            return Flow::Continue;
        }
        Flow::Yield
    }

    // ---- SPLIT / JOIN ----------------------------------------------------------------------------------------

    /// `SPLIT` (the two followers walk from the leader to given spots and turn) and `JOIN` (they turn to the leader,
    /// walk to it and disappear into it). The leader cannot be moved meanwhile.
    fn op_split_join(&mut self, split: bool) -> Flow {
        let len = if split { 15 } else { 2 };
        let speed = i32::from(self.p8(if split { 14 } else { 1 }));
        let targets = if split {
            [
                SplitTarget { x: i32::from(self.rd16(1, 4)), y: i32::from(self.rd16(2, 6)), dir: self.rd8(3, 8) },
                SplitTarget { x: i32::from(self.rd16(4, 9)), y: i32::from(self.rd16(5, 11)), dir: self.rd8(6, 13) },
            ]
        } else {
            [SplitTarget { x: 0, y: 0, dir: 0 }; 2]
        };
        // Both followers are stepped every visit (no short circuit), as in the original.
        let mut done = true;
        for (i, target) in targets.into_iter().enumerate() {
            let finished = match self.party_entity(i + 1) {
                None => true,
                Some(e) => self.split_join_step(e, split, target, speed),
            };
            done &= finished;
        }
        let leader = self.party_entity(0).and_then(|e| self.entity_model(e));
        if done {
            for slot in 0..3 {
                if let Some(e) = self.party_entity(slot) {
                    self.motion.split_join.insert(e, 0);
                }
            }
            if let Some(m) = leader.and_then(|m| self.models.get_mut(m)) {
                m.solid_off = false;
            }
            if let Some(lock) = self.motion.saved_lock.take() {
                self.set_character_lock(lock);
            }
            self.advance(len);
            return Flow::Continue;
        }
        if self.motion.saved_lock.is_none() {
            self.motion.saved_lock = Some(self.state.character_lock);
        }
        self.set_character_lock(true);
        if let Some((model, mm)) = leader.and_then(|m| self.model_pair(m)) {
            mm.disarm(model);
            model.solid_off = true;
        }
        Flow::Yield
    }

    fn set_character_lock(&mut self, locked: bool) {
        if self.state.character_lock != locked {
            self.state.character_lock = locked;
            self.emit(super::super::Event::Control { locked });
        }
    }

    /// Advance follower `entity` one visit through its split or join; returns whether it is finished.
    fn split_join_step(&mut self, entity: usize, split: bool, target: SplitTarget, speed: i32) -> bool {
        let (Some(mi), Some(leader)) = (self.entity_model(entity), self.party_entity(0).and_then(|e| self.entity_model(e))) else {
            return true;
        };
        let state = self.motion.split_join.get(&entity).copied().unwrap_or(0);
        let next = if split {
            match state {
                0 => {
                    if !self.split_join_begin_walk(mi, leader, (target.x, target.y), speed, true) {
                        return true;
                    }
                    1
                }
                1 => {
                    if !self.split_join_end_walk(mi) {
                        1
                    } else {
                        if let Some(m) = self.models.get_mut(mi) {
                            m.solid_off = false;
                            m.talk_off = false;
                        }
                        self.split_join_begin_turn(mi, target.dir);
                        2
                    }
                }
                2 => {
                    if self.split_join_end_turn(mi) {
                        3
                    } else {
                        2
                    }
                }
                _ => 3,
            }
        } else {
            match state {
                0 => {
                    let dir = self.direction_to(mi, leader).unwrap_or(0);
                    self.split_join_begin_turn(mi, dir);
                    2
                }
                2 => {
                    if self.split_join_end_turn(mi) {
                        let at = self.models.get(leader).map_or((0, 0), |l| (l.x, l.y));
                        if !self.split_join_begin_walk(mi, leader, at, speed, false) {
                            return true;
                        }
                        1
                    } else {
                        2
                    }
                }
                1 => {
                    if self.split_join_end_walk(mi) {
                        if let Some(m) = self.models.get_mut(mi) {
                            m.solid_off = true;
                            m.talk_off = true;
                            m.visible = false;
                        }
                        3
                    } else {
                        1
                    }
                }
                _ => 3,
            }
        };
        self.motion.split_join.insert(entity, next);
        next == 3
    }

    /// Start a follower walking to `to`, timed to take `speed` frames. Returns false when it cannot be started.
    fn split_join_begin_walk(&mut self, mi: usize, leader: usize, to: (i32, i32), speed: i32, copy_leader: bool) -> bool {
        let Some(l) = self.models.get(leader) else { return false };
        let from = (l.x, l.y, l.z, l.triangle);
        let mut speed = speed;
        if speed == 0 {
            // A zero-frame walk divides by zero in the original.
            self.coverage.anomalies += 1;
            speed = 1;
        }
        let Some((model, mm)) = self.model_pair(mi) else { return false };
        model.visible = true;
        model.solid_off = true;
        model.talk_off = true;
        if copy_leader {
            (model.x, model.y, model.z, model.triangle) = from;
            mm.sync(model);
        }
        mm.arg = 0;
        mm.dir_lock = false;
        mm.end[0] = to.0.wrapping_shl(12);
        mm.end[1] = to.1.wrapping_shl(12);
        let timed = distance((model.x, model.y), to).wrapping_shl(8) / speed;
        mm.speed_override = Some(timed);
        model.anim = walk_anim(timed, 512);
        model.anim_speed = 16;
        model.anim_status = AnimStatus::ScriptedLoop;
        mm.arm(model, Phase::Walk);
        true
    }

    fn split_join_end_walk(&mut self, mi: usize) -> bool {
        let Some((model, mm)) = self.model_pair(mi) else { return true };
        // A walk the model no longer has (a higher-priority script preempted it) counts as over: waiting for it
        // would hold the split, and the player's control, for good.
        if mm.phase == Phase::Walk && mm.state != 2 {
            return false;
        }
        mm.disarm(model);
        model.anim_status = AnimStatus::DefaultLoop;
        true
    }

    fn split_join_begin_turn(&mut self, mi: usize, end_dir: u8) {
        if let Some((model, mm)) = self.model_pair(mi) {
            let start = i32::from(model.dir);
            let end = shortest_way(start, i32::from(end_dir));
            mm.turn.step = 0;
            begin_turn(&mut mm.turn, model.dir, TURN_EASED, 16, end);
        }
    }

    fn split_join_end_turn(&mut self, mi: usize) -> bool {
        let Some((_, mm)) = self.model_pair(mi) else { return true };
        if mm.turn.kind == TURN_DONE {
            mm.turn = Turn::default();
            return true;
        }
        false
    }

    // ---- read API ---------------------------------------------------------------------------------------------

    /// The walkmesh triangles `IDLCK` has locked.
    pub fn locked_triangles(&self) -> &BTreeSet<u16> {
        &self.motion.locked
    }

    /// Whether triangle `triangle` is locked.
    pub fn triangle_locked(&self, triangle: u16) -> bool {
        self.motion.locked.contains(&triangle)
    }

    /// Model `model`'s interpolated render offset (`OFST`), in field units, as x, y, z.
    pub fn model_offset(&self, model: usize) -> Option<[i32; 3]> {
        self.motion.models.get(model).map(|m| m.offset.cur)
    }

    /// Whether `SLIP` switched slipping off for `entity`'s line.
    pub fn slip_disabled(&self, entity: usize) -> bool {
        self.motion.slip_off.contains(&entity)
    }

    /// Whether model `model` is under a scripted walk, jump or climb (the host should let the runtime place it).
    pub fn motion_active(&self, model: usize) -> bool {
        self.motion.models.get(model).is_some_and(|m| m.phase != Phase::Idle)
    }

    /// Per-frame update after every entity's turn (movement, animation, effects advancing).
    pub(in crate::runtime) fn update_motion(&mut self) {
        let count = self.models.len();
        if count == 0 {
            return;
        }
        let mesh = self.walkmesh.clone();
        let surface = self.motion.surface.get(mesh.as_ref());
        let pc = self.state.pc_model;
        // Ladders climb by themselves unless the host hands the pad over (`ladder_keys`): a multiplayer host sends
        // positions, not direction keys, so a key-driven ladder would never move.
        let keys = if self.state.character_lock || !self.ladder_keys { None } else { self.player.map(|p| p.keys_held) };
        let mut anomalies = 0u64;
        // Bring every model's state in line first (a preempted motion is dropped here).
        for m in 0..count {
            self.model_pair(m);
        }
        for m in 0..count {
            if let Some((model, mm)) = self.model_pair(m) {
                update_turn(model, &mut mm.turn, &mut anomalies);
            }
        }
        for m in 0..count {
            if let Some((_, mm)) = self.model_pair(m) {
                update_offset(&mut mm.offset, &mut anomalies);
            }
        }
        for m in 0..count {
            if let Some((model, mm)) = self.model_pair(m) {
                if mm.phase == Phase::Walk {
                    update_walk(model, mm, surface.as_deref());
                }
            }
        }
        for m in 0..count {
            if let Some((model, mm)) = self.model_pair(m) {
                if mm.phase == Phase::Jump {
                    update_jump(model, mm, mesh.as_deref(), &mut anomalies);
                }
            }
        }
        for horizontal in [false, true] {
            let wanted = if horizontal { Phase::LadderH } else { Phase::LadderV };
            for m in 0..count {
                let player_keys = if Some(m) == pc { keys } else { None };
                if let Some((model, mm)) = self.model_pair(m) {
                    if mm.phase == wanted {
                        update_ladder(model, mm, horizontal, player_keys, &mut anomalies);
                    }
                }
            }
        }
        self.coverage.anomalies += anomalies;
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::{Banks, FieldRuntime, MoveMode};
    use crate::script::synth::SynthScript;

    use super::*;

    /// `[init..., RET, main...]` as script 0.
    fn script0(init: &[u8], main: &[u8]) -> (usize, Vec<u8>) {
        let mut v = init.to_vec();
        v.push(0x00);
        v.extend_from_slice(main);
        (0, v)
    }

    const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

    fn runtime(s: SynthScript) -> FieldRuntime {
        let mut s = s;
        s.scale = 512;
        let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
        rt.init();
        rt
    }

    fn set_byte(addr: u8, value: u8) -> Vec<u8> {
        vec![0x80, 1 << 4, addr, value]
    }

    /// `CHAR 0` then `XYZI` to the given place (literals).
    fn spawn(x: i16, y: i16, z: i16, tri: i16) -> Vec<u8> {
        let mut v = vec![0xA1, 0x00, 0xA5, 0, 0];
        for n in [x, y, z, tri] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v
    }

    fn le(n: i16) -> [u8; 2] {
        n.to_le_bytes()
    }

    fn model(rt: &FieldRuntime, i: usize) -> crate::runtime::Model {
        rt.models()[i].clone()
    }

    /// Frames until byte `addr` becomes non-zero (at most `limit`).
    fn frames_until(rt: &mut FieldRuntime, addr: usize, limit: u32) -> Option<u32> {
        for f in 1..=limit {
            rt.tick(None);
            if rt.mem.banks.get(addr) != 0 {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn move_walks_straight_to_its_target_then_continues() {
        let mut move_op = vec![MOVE, 0x00];
        move_op.extend_from_slice(&le(40));
        move_op.extend_from_slice(&le(0));
        let main = [&move_op[..], &set_byte(0x30, 1), IDLE].concat();
        let s = SynthScript::default().entity("npc", vec![script0(&spawn(0, 0, 0, 0), &main)]);
        let mut rt = runtime(s);
        let frames = frames_until(&mut rt, 0x30, 60).expect("MOVE finishes");
        let m = model(&rt, 0);
        assert_eq!((m.x, m.y), (40, 0));
        assert_eq!(m.move_mode, MoveMode::None);
        // 4 units a frame at the default walk speed: ten steps, plus the arming and completion visits.
        assert!((10..=14).contains(&frames), "{frames}");
        assert_eq!(m.dir, 64, "facing +x");
        assert!(rt.coverage().unsupported.is_empty(), "{:?}", rt.coverage().unsupported);
    }

    #[test]
    fn move_is_resumed_after_a_higher_priority_script_preempts_it() {
        // Entity 0 walks far; entity 1 requests (REQ) entity 0's script 1 at priority 1, which returns at once.
        let mut move_op = vec![MOVE, 0x00];
        move_op.extend_from_slice(&le(200));
        move_op.extend_from_slice(&le(0));
        let main = [&move_op[..], &set_byte(0x31, 1), IDLE].concat();
        let walker = vec![script0(&spawn(0, 0, 0, 0), &main), (1, vec![0x00])];
        let mut kick = vec![0x24, 5, 0]; // WAIT 5
        kick.extend_from_slice(&[0x01, 0, 1 << 5 | 1]); // REQ entity 0 script 1 at priority 1
        kick.extend_from_slice(IDLE);
        let s = SynthScript::default().entity("walker", walker).entity("kicker", vec![script0(&[], &kick)]);
        let mut rt = runtime(s);
        let frames = frames_until(&mut rt, 0x31, 200).expect("the interrupted MOVE still arrives");
        assert_eq!(model(&rt, 0).x, 200);
        assert!(frames > 40, "the walk was not cut short ({frames})");
    }

    #[test]
    fn tura_waits_for_its_steps_and_faces_the_target() {
        // Entity 0 (model at the origin) turns to entity 1 (model at +y): facing +y is direction 128.
        let target = script0(&spawn(0, 100, 0, 0), IDLE);
        // TURA entity 1, 8 steps, shortest way
        let turn = [&[TURA, 1, 8, 2][..], &set_byte(0x32, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &turn)]).entity("b", vec![target]);
        let mut rt = runtime(s);
        let frames = frames_until(&mut rt, 0x32, 40).expect("TURA completes");
        assert_eq!(model(&rt, 0).dir, 128);
        assert!((8..=12).contains(&frames), "N steps hold N+1 frames, plus the visit after ({frames})");
    }

    #[test]
    fn turngen_turns_the_chosen_way_and_waits() {
        // From direction 0 to 192 in 4 steps, linear; way 0 increases, so it sweeps 0 -> 192 directly.
        let turn = [&[TURNGEN, 0x00, 192, 0, 4, 1][..], &set_byte(0x33, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &turn)]);
        let mut rt = runtime(s);
        let mut seen = vec![];
        for _ in 0..12 {
            rt.tick(None);
            seen.push(model(&rt, 0).dir);
            if rt.mem.banks.get(0x33) != 0 {
                break;
            }
        }
        assert_eq!(rt.mem.banks.get(0x33), 1);
        assert_eq!(model(&rt, 0).dir, 192);
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotone: {seen:?}");
        // Way 1 goes the other way round: 0 -> 192 decreasing passes through 224 and 255.
        let turn = [&[TURNGEN, 0x00, 192, 1, 4, 1][..], IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &turn)]);
        let mut rt = runtime(s);
        rt.tick(None);
        rt.tick(None);
        assert!(model(&rt, 0).dir > 192, "went the long way round");
    }

    #[test]
    fn dira_faces_at_once_and_still_yields() {
        let dira = [&[DIRA, 1][..], &set_byte(0x34, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &dira)]).entity("b", vec![script0(&spawn(-50, 0, 0, 0), IDLE)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(model(&rt, 0).dir, 192, "facing -x");
        assert_eq!(rt.mem.banks.get(0x34), 0, "DIRA ends the entity's turn");
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x34), 1);
    }

    #[test]
    fn idlck_locks_and_unlocks_triangles() {
        let main = [&[IDLCK, 5, 0, 1][..], &[IDLCK, 0x2C, 0x01, 1], &[IDLCK, 5, 0, 0], IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert!(rt.locked_triangles().contains(&300));
        assert!(!rt.triangle_locked(5), "locked and unlocked again");
        assert_eq!(rt.locked_triangles().len(), 1);
    }

    #[test]
    fn jump_blocks_for_n_plus_two_frames_and_lands_on_target() {
        let mut jump = vec![JUMP, 0x00, 0x00];
        for n in [30i16, 10, 0, 12] {
            jump.extend_from_slice(&le(n));
        }
        let main = [&jump[..], &set_byte(0x35, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &main)]);
        let mut rt = runtime(s);
        let frames = frames_until(&mut rt, 0x35, 40).expect("JUMP completes");
        assert_eq!((model(&rt, 0).x, model(&rt, 0).y), (30, 10));
        assert!((13..=15).contains(&frames), "N+2 blocked frames ({frames})");
        // Mid-air the model rose (the arc is above both ends).
        let mut rt = runtime(SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &[&jump[..], IDLE].concat())]));
        let mut peak = 0;
        for _ in 0..14 {
            rt.tick(None);
            peak = peak.max(model(&rt, 0).z);
        }
        assert!(peak > 50, "{peak}");
    }

    #[test]
    fn get_ops_read_models_into_variables() {
        // GETAXY entity 1 -> vars 0x40, 0x42; AXYZI entity 1 -> 0x44, 0x46, 0x48, 0x4A; GETAI, GETDIR.
        let mut main = vec![GETAXY, 0x22, 1, 0x40, 0x42];
        main.extend_from_slice(&[AXYZI, 0x22, 0x22, 1, 0x44, 0x46, 0x48, 0x4A]);
        main.extend_from_slice(&[GETAI, 0x02, 1, 0x4C]);
        main.extend_from_slice(&[GETDIR, 0x01, 1, 0x4E]);
        main.extend_from_slice(&[0x80, 0x10, 0x50, 1]);
        main.extend_from_slice(IDLE);
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]).entity("b", vec![script0(&spawn(70, -20, 9, 3), IDLE)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x50), 1, "ran to the end");
        let w = |a: usize| i16::from_le_bytes([rt.mem.banks.get(a), rt.mem.banks.get(a + 1)]);
        assert_eq!((w(0x40), w(0x42)), (70, -20));
        assert_eq!((w(0x44), w(0x46), w(0x48), w(0x4A)), (70, -20, 9, 3));
        assert_eq!(w(0x4C), 3);
        assert_eq!(rt.mem.banks.get(0x4E), 0);
    }

    #[test]
    fn ofst_interpolates_and_ofstw_waits() {
        // OFST linear (type 1) to (8, -4, 2) over 4 steps, then OFSTW, then a flag.
        let mut ofst = vec![OFST, 0, 0, 1];
        for n in [8i16, -4, 2, 4] {
            ofst.extend_from_slice(&le(n));
        }
        let main = [&ofst[..], &[OFSTW], &set_byte(0x36, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x36), 0, "OFSTW holds");
        assert_eq!(rt.model_offset(0), Some([0, 0, 0]), "step zero is the start");
        let frames = frames_until(&mut rt, 0x36, 20).expect("OFSTW releases");
        assert!(frames >= 4);
        assert_eq!(rt.model_offset(0), Some([8, -4, 2]));
    }

    #[test]
    fn sin_and_cos_use_a_4096_turn_and_scale_by_their_operands() {
        // SIN: dest bank 1 / literals; angle 1024 (quarter turn), multiplier 100, addend 3 << 12 -> 103.
        let mut sin = vec![SIN, 0x00, 0x02];
        for n in [1024i16, 100, 3 << 12] {
            sin.extend_from_slice(&le(n));
        }
        sin.push(0x60);
        let mut cos = vec![COS, 0x00, 0x02];
        for n in [0i16, 100, 0] {
            cos.extend_from_slice(&le(n));
        }
        cos.push(0x62);
        let main = [&sin[..], &cos[..], IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        let w = |a: usize| i16::from_le_bytes([rt.mem.banks.get(a), rt.mem.banks.get(a + 1)]);
        assert_eq!(w(0x60), 103);
        assert_eq!(w(0x62), 100);
    }

    #[test]
    fn lader_climbs_on_its_own_and_waits() {
        // LADER to (0, 0, 80) triangle 4, way 0, clip 2, dir 128, anim divisor 2.
        let mut lader = vec![LADER, 0, 0];
        for n in [0i16, 0, 80, 4] {
            lader.extend_from_slice(&le(n));
        }
        lader.extend_from_slice(&[0, 2, 128, 2]);
        let main = [&lader[..], &set_byte(0x37, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &main)]);
        let mut rt = runtime(s);
        let frames = frames_until(&mut rt, 0x37, 60).expect("the climb ends");
        let m = model(&rt, 0);
        // (the clip is the animation family's after the climb: a finished ladder returns to the default loop)
        assert_eq!((m.z, m.triangle, m.dir), (80, 4, 128));
        assert!((20..=24).contains(&frames), "80 units / 4 = 20 steps ({frames})");
    }

    #[test]
    fn split_and_join_finish_at_once_without_party_members() {
        let mut split = vec![SPLIT, 0, 0, 0];
        for n in [10i16, 10] {
            split.extend_from_slice(&le(n));
        }
        split.push(0);
        for n in [-10i16, 10] {
            split.extend_from_slice(&le(n));
        }
        split.push(0);
        split.push(8);
        let main = [&split[..], &[JOIN, 8], &set_byte(0x38, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x38), 1);
    }

    #[test]
    fn fmove_leaves_the_animation_and_cmove_keeps_the_facing() {
        for (op, anim_after, dir_after) in [(FMOVE, 0u8, 64u8), (CMOVE, 0, 128), (MOVE, 0, 64)] {
            let mut mv = vec![op, 0x00];
            mv.extend_from_slice(&le(20));
            mv.extend_from_slice(&le(0));
            // DIR 128 first, so CMOVE has a facing to keep.
            let main = [&[0xB3, 0x00, 128][..], &mv[..], IDLE].concat();
            let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &main)]);
            let mut rt = runtime(s);
            rt.tick(None);
            rt.tick(None);
            let m = model(&rt, 0);
            assert_eq!(m.anim, if op == MOVE { 1 } else { 0 }, "only MOVE plays the walk clip");
            for _ in 0..20 {
                rt.tick(None);
            }
            let m = model(&rt, 0);
            assert_eq!((m.x, m.anim), (20, anim_after));
            assert_eq!(m.dir, dir_after, "op {op:#04x}");
        }
    }

    #[test]
    fn mova_stops_at_the_targets_solid_range() {
        // b stands at (100, 0) with solid range 30 (the default at scale 512); a walks towards it and stops short.
        let mova = [&[MOVA, 1][..], &set_byte(0x39, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &mova)]).entity("b", vec![script0(&spawn(100, 0, 0, 0), IDLE)]);
        let mut rt = runtime(s);
        frames_until(&mut rt, 0x39, 80).expect("MOVA finishes");
        let x = model(&rt, 0).x;
        assert!((10..60).contains(&x), "stopped within the radius, well short of the target ({x})");
    }

    #[test]
    fn a_walkmesh_supplies_the_triangle_and_height_under_a_walking_model() {
        use crate::field::{Vertex, Walkmesh};
        let v = |x, y, z| Vertex { x, y, z };
        // Two triangles side by side along x: the left flat at z=0, the right raised to z=20.
        let mesh = Walkmesh {
            triangles: vec![[v(-10, -50, 0), v(50, -50, 0), v(-10, 50, 0)], [v(50, -50, 20), v(150, -50, 20), v(50, 50, 20)]],
            access: vec![[0xFFFF; 3], [0xFFFF; 3]],
        };
        let mut mv = vec![MOVE, 0x00];
        mv.extend_from_slice(&le(100));
        mv.extend_from_slice(&le(-20));
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, -20, 0, 0), &[&mv[..], &set_byte(0x3A, 1), IDLE].concat())]);
        let mut rt = runtime(s);
        rt.set_walkmesh(std::sync::Arc::new(mesh));
        frames_until(&mut rt, 0x3A, 60).expect("MOVE finishes");
        let m = model(&rt, 0);
        assert_eq!((m.x, m.y, m.triangle, m.z), (100, -20, 1, 20));
    }

    #[test]
    fn turnw_waits_for_a_turn_and_passes_when_none_runs() {
        // TURNGEN has already finished by the time TURNW runs, so TURNW passes straight away.
        let main = [&[TURNW][..], &set_byte(0x3B, 1), IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&spawn(0, 0, 0, 0), &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x3B), 1);
    }

    #[test]
    fn every_motion_opcode_goes_on_past_itself_without_a_model() {
        for op in 0u8..=255 {
            if !implemented(op) {
                continue;
            }
            let len = crate::script::decode_at(&[op, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, 0).map(|i| i.len).expect("decodes");
            let mut code = vec![op];
            code.resize(len, 0);
            let s = SynthScript::default().entity("x", vec![script0(&[], &[&code[..], &set_byte(0x70, 1), IDLE].concat())]);
            let mut rt = runtime(s);
            // Two frames: `DIRA` advances and yields in one visit.
            rt.tick(None);
            rt.tick(None);
            assert_eq!(rt.mem.banks.get(0x70), 1, "op {op:#04x} (length {len}) did not let the script go on");
            assert!(rt.coverage().unsupported.is_empty());
            assert_eq!(rt.coverage().undecodable, 0);
        }
    }
}
