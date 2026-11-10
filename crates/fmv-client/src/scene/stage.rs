//! The stage of a scene: where each member of the cast stands, which way it faces and whether it is seen, moment by
//! moment, and what the scene told the camera.
//!
//! Pure data and arithmetic: no GPU, no game data. The scene player ([`super::ScenePlayer`]) feeds it the stage events
//! of the interpreter (`fmv_ff7::script::scene::Stage`) when their time has come and moves it on with the scene's clock;
//! the drawing reads the actors it holds.
//!
//! * `Place` puts an actor somewhere at once, and cancels a walk it was on.
//! * `Offset` replaces the model's world-space render offset without moving its logical position.
//!   Placement, movement and turning preserve the offset; a new scene starts with zero unless its setup supplies one.
//!   `OffsetMove` samples the original type-1 integer steps on scene time. Durable scenes reject
//!   a dialogue/end boundary while it is running: native offsets continue during dialogue,
//!   which needs the shared field timeline before such a boundary can be reproduced.
//! * `Move` walks it there over the number of frames the event says (a straight line from where it stood, heading
//!   toward the target the whole way). A second move of the same actor first finishes the first. A resolved ordinary-MOVE
//!   clip follows only that move's elapsed time, and yields to the standing default when it arrives.
//!   FMOVE and JUMP can instead keep an already resolved normal full loop advancing through movement and arrival.
//!   JUMP samples the original Q12 trajectory over its authored steps and two completion frames.
//! * `Face` turns it (the short way round), at once or over the frames it says.
//! * `Show` hides or shows it.
//! * The player resolves supported `Anim` loops, full Holds and explicit forward Holds against the actor's exact field model.
//!   Loops and Holds support ASPED 16 and 32 (normal and double speed), including standing rate changes without
//!   restarting their phase. Loops and Holds follow scene time; a Hold stops at its last frame until replaced. A Once can
//!   restart a separately remembered, different default slot after its range or actual loaded clip length.
//!   A normal full Once or a normal `[0, 0]` Once can also return to the exact same default clip at frame zero;
//!   other same-slot ranges and doubled same-slot returns remain unsupported.
//!   A full doubled Once can return to normal speed at an authored ASPED, retaining its pose and completion time.
//!   Other animations clear that loop and are counted ([`Counts::anims_skipped`]), never guessed at.
//! * `Sound` is routed separately by the scene player to its cancellable audio channel.
//! * `Camera` is **recorded** ([`CameraState`]) with the stage's own clock of it ([`CameraClock`]); the window's field camera
//!   (`crate::camera`) shows it.
//!
//! Time is in field frames (30 a second), the interpreter's clock. A walk or a turn only moves while the scene runs: a
//! window pauses actors and script time. Screen fades and shake follow the visible field clock, even during dialogue.

use fmv_ff7::script::scene::{AnimHow, CameraOp, FadeColor, FadeState, JumpMotion, MoveHow, Stage};
use super::shake::ShakeState;
use fmv_ff7::background::BackgroundOp;
use crate::avatar::anim::ClipFrame;
use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};

/// What the scene told the camera last (the window's field camera, `crate::camera`, shows it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraState {
    /// Nothing asked of it, or given back (`SCRCC`).
    #[default]
    Default,
    /// A scroll to a place in the field's own screen units, over this many frames.
    Scroll { x: i16, y: i16, frames: u32 },
    /// Follow the cast member playing this script entity.
    Follow { entity: u8 },
}

/// What the stage could not show, and said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Unsupported or unresolved `Anim` events of cast members.
    pub anims_skipped: u32,
    /// Jumps without a valid authored duration, position or landing height.
    pub moves_skipped: u32,
    /// Camera events (the window's field camera shows them; counted here as the scene's camera clock).
    pub camera_ops: u32,
    /// Events about an entity that is not in the cast.
    pub off_cast: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Walk {
    from: [f32; 3],
    to: [f32; 3],
    frames: f32,
    elapsed: f32,
    how: MoveHow,
    keep_animation: bool,
    jump: Option<JumpMotion>,
    landing_triangle: Option<u16>,
}

fn is_walk(how: MoveHow) -> bool {
    matches!(how, MoveHow::Walk | MoveHow::WalkKeepAnimation)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Turn {
    from: f32,
    to: f32,
    frames: f32,
    elapsed: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct SceneAnimation {
    clip: String,
    frame: ClipFrame,
    /// Clip frames per scene frame; supported loops, Holds and full Once clips run at normal or doubled rates.
    rate: f32,
    once: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct DefaultAnimation {
    slot: u8,
    clip: String,
}

/// How far through a motion of `frames` frames it is after `elapsed` of them (0 to 1; no frames is done).
fn progress(elapsed: f32, frames: f32) -> f32 {
    if frames.partial_cmp(&0.0).is_none_or(|o| o.is_le()) || elapsed.is_nan() {
        1.0
    } else {
        (elapsed / frames).clamp(0.0, 1.0)
    }
}

/// An angle in (-pi, pi].
fn wrap(angle: f32) -> f32 {
    if !angle.is_finite() {
        return 0.0;
    }
    let a = angle.rem_euclid(TAU);
    if a > PI {
        a - TAU
    } else {
        a
    }
}

/// `from` turned toward `to` by `t` of the short way round.
fn turn_toward(from: f32, to: f32, t: f32) -> f32 {
    let delta = wrap(to - from);
    wrap(from + delta * t)
}

/// One member of the cast as the stage has it.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    entity: u8,
    /// `x`, `y`, `z` in walkmesh units; `None` until something placed it.
    pos: Option<[f32; 3]>,
    /// OFST's world-space translation of the picture only; never part of walkmesh movement.
    render_offset: [i16; 3],
    offset_motion: Option<(fmv_ff7::script::scene::OffsetMotion, f32)>,
    triangle: Option<u16>,
    heading: f32,
    visible: bool,
    walk: Option<Walk>,
    turn: Option<Turn>,
    /// Scene frames actually spent walking, retained across consecutive movement segments.
    walked_frames: f32,
    animation: Option<SceneAnimation>,
    movement_animation: Option<SceneAnimation>,
    default_animation: Option<DefaultAnimation>,
}

impl Actor {
    fn new(entity: u8) -> Self {
        Self { entity, pos: None, render_offset: [0; 3], offset_motion: None, triangle: None, heading: 0.0, visible: true, walk: None, turn: None, walked_frames: 0.0, animation: None, movement_animation: None, default_animation: None }
    }

    /// The script entity this actor plays.
    pub fn entity(&self) -> u8 {
        self.entity
    }

    /// Where it stands now (`x`, `y`, `z`), `None` while nothing has placed it.
    pub fn pos(&self) -> Option<[f32; 3]> {
        self.pos
    }

    /// Where the model is drawn: the logical position plus OFST in field-world units.
    /// Facing and model scale transform the mesh independently; neither transforms this offset
    /// (ff7-decomp d4db3de3, field_camera.c, HandleKawaiDataInModel).
    pub fn render_pos(&self) -> Option<[f32; 3]> {
        self.pos.map(|p| std::array::from_fn(|axis| p[axis] + f32::from(self.render_offset[axis])))
    }

    /// The walkmesh triangle it was last placed on, if the scene said.
    #[cfg(test)]
    pub fn triangle(&self) -> Option<u16> {
        self.triangle
    }

    /// Which way it faces, radians counter-clockwise from the field's +x axis.
    pub fn heading(&self) -> f32 {
        self.heading
    }

    /// Whether it is walking now (what the avatar's clip shows: a walk while this holds, standing otherwise).
    pub fn moving(&self) -> bool {
        self.walk.is_some_and(|w| is_walk(w.how))
    }

    pub fn walked_frames(&self) -> f32 { self.walked_frames }

    pub fn animation(&self) -> Option<(&str, ClipFrame)> {
        if let Some(walk) = self.walk {
            if walk.keep_animation && self.has_normal_loop() {
                return self.animation.as_ref().map(|a| (a.clip.as_str(), a.frame));
            }
            return is_walk(walk.how).then_some(self.movement_animation.as_ref()).flatten()
                .map(|a| (a.clip.as_str(), a.frame));
        }
        self.animation.as_ref().map(|a| (a.clip.as_str(), a.frame))
    }

    fn has_normal_loop(&self) -> bool {
        self.animation.as_ref().is_some_and(|animation|
            matches!(animation.frame, ClipFrame::Loop(_)) && animation.rate == 1.0 && !animation.once)
    }

    fn replace_kept_animation(&mut self) {
        if let Some(walk) = &mut self.walk { walk.keep_animation = false; }
    }

    /// A full Once's verified return clip. The avatar cache resolves the actual length without
    /// mutating the stage, so hidden actors and late first draws use the same scene clock.
    pub fn full_once_return(&self) -> Option<&str> {
        if self.walk.is_some() { return None; }
        match self.animation.as_ref()?.frame {
            ClipFrame::OnceFull { .. } | ClipFrame::OnceFullDoubled { .. } | ClipFrame::OnceFullSlowed { .. } => self.default_animation.as_ref().map(|default| default.clip.as_str()),
            _ => None,
        }
    }

    /// Whether it is seen at all: it has a place and is not hidden.
    pub fn shown(&self) -> bool {
        self.visible && self.pos.is_some()
    }

    /// Whether anything is in motion.
    fn busy(&self) -> bool {
        self.walk.is_some() || self.turn.is_some() || self.offset_motion.is_some()
    }

    fn advance(&mut self, frames: f32) {
        self.advance_offset(frames);
        let moving_frames = self.walk.map_or(0.0, |w| frames.min((w.frames - w.elapsed).max(0.0)));
        let keep_animation = self.walk.is_some_and(|walk| walk.keep_animation) && self.has_normal_loop();
        self.advance_walk(frames);
        self.advance_turn(frames);
        if frames.is_finite() {
            if let Some(animation) = &mut self.animation {
                let advancing = match &mut animation.frame {
                    ClipFrame::Loop(frame) => Some(frame),
                    ClipFrame::HoldRange { elapsed, .. } | ClipFrame::HoldFull { elapsed } | ClipFrame::OnceFull { elapsed }
                    | ClipFrame::OnceFullDoubled { elapsed } | ClipFrame::OnceFullSlowed { elapsed, .. } => Some(elapsed),
                    ClipFrame::Hold(_) => None,
                };
                let animation_frames = if keep_animation { frames } else { (frames - moving_frames).max(0.0) };
                if let Some(frame) = advancing { *frame += animation_frames * animation.rate; }
            }
            self.finish_once();
        }
    }

    fn advance_offset(&mut self, frames: f32) {
        if let Some((motion, elapsed)) = &mut self.offset_motion {
            *elapsed += frames;
            self.render_offset = motion.at(*elapsed as u32);
            if *elapsed >= f32::from(motion.steps()) { self.offset_motion = None; }
        }
    }

    fn restore_default(&mut self, overshoot: f32) {
        let frame = if overshoot.is_finite() { overshoot.max(0.0) } else { 0.0 };
        self.animation = self.default_animation.as_ref().map(|default| SceneAnimation {
            clip: default.clip.clone(), frame: ClipFrame::Loop(frame), rate: 1.0, once: false,
        });
    }

    fn finish_once(&mut self) {
        if let Some(SceneAnimation { once: true, frame: ClipFrame::HoldRange { first, last, elapsed }, .. }) = &self.animation {
            let duration = f32::from(last - first) + 1.0;
            if *elapsed >= duration { self.restore_default(*elapsed - duration); }
        }
    }

    fn advance_walk(&mut self, frames: f32) {
        if let Some(walk) = &mut self.walk {
            if is_walk(walk.how) {
                let spent = frames.min((walk.frames - walk.elapsed).max(0.0));
                self.walked_frames += spent;
                if let Some(SceneAnimation { frame: ClipFrame::Loop(frame), .. }) = &mut self.movement_animation {
                    *frame += spent;
                }
            }
            walk.elapsed += frames;
            let t = progress(walk.elapsed, walk.frames);
            let at = walk.jump.map_or_else(
                || [0, 1, 2].map(|i| walk.from[i] + (walk.to[i] - walk.from[i]) * t),
                |jump| jump.at(walk.elapsed));
            self.pos = Some(at);
            // Original JUMP changes the triangle on the completion update, one frame after
            // its last motion step and one before the script consumes completion.
            if walk.jump.is_some() && walk.elapsed >= walk.frames - 1.0 {
                self.triangle = walk.landing_triangle;
            }
            if t >= 1.0 {
                self.walk = None;
                self.movement_animation = None;
            }
        }
    }

    fn advance_turn(&mut self, frames: f32) {
        if let Some(turn) = &mut self.turn {
            turn.elapsed += frames;
            let t = progress(turn.elapsed, turn.frames);
            self.heading = turn_toward(turn.from, turn.to, t);
            if t >= 1.0 {
                self.turn = None;
            }
        }
    }

    /// Finish whatever it is doing: it stands where its walk was going, facing where its turn was going.
    fn settle(&mut self) {
        self.advance(f32::INFINITY);
    }
}

/// What the scene told the camera last, with the stage's own clock of it: how many camera events the scene has had
/// (two identical scrolls are two events) and how many frames of scene time went by since the last one. The window's
/// field camera (`crate::camera`) runs a scroll over its frames on this clock, so a window that pauses the stage pauses
/// the scroll too, and the scroll ends on the frame the interpreter's `SCRLW` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CameraClock {
    pub state: CameraState,
    /// Camera events so far in this scene.
    pub events: u32,
    /// Frames of scene time since the last camera event (since the start when there was none).
    pub frames: f32,
}

/// The cast of one scene and the camera.
#[derive(Debug, Clone, Default)]
pub struct Cast {
    actors: BTreeMap<u8, Actor>,
    camera: CameraState,
    /// Frames of scene time since the last camera event ([`CameraClock::frames`]).
    camera_frames: f32,
    /// Authored field mutations survive cast teardown until the client captures their suffix.
    background_ops: Vec<BackgroundOp>,
    fade: FadeState,
    shake: ShakeState,
    counts: Counts,
}

/// The ground's height at a place of the field (`x`, `y`, and the triangle the scene says holds it, if it does).
pub type Height<'a> = &'a dyn Fn(i16, i16, Option<u16>) -> Option<f32>;

impl Cast {
    /// A stage for the cast playing these script entities (every one starts unplaced).
    pub fn new(entities: impl IntoIterator<Item = u8>) -> Self {
        Self { actors: entities.into_iter().map(|e| (e, Actor::new(e))).collect(), camera: CameraState::Default, camera_frames: 0.0, background_ops: Vec::new(), fade: FadeState::default(), shake: ShakeState::default(), counts: Counts::default() }
    }

    /// What the scene told the camera and the stage's clock of it ([`CameraClock`]).
    pub fn camera_clock(&self) -> CameraClock {
        CameraClock { state: self.camera, events: self.counts.camera_ops, frames: self.camera_frames }
    }

    pub fn background_ops(&self) -> &[BackgroundOp] { &self.background_ops }

    pub fn fade(&self) -> Option<FadeColor> { self.fade.color() }

    pub fn shake(&self) -> [f32; 2] { self.shake.offset() }

    pub fn shake_rng_indices(&self) -> [u8; 2] { self.shake.rng_indices() }

    pub fn seed_shake_rng(&mut self, indices: [u8; 2]) { self.shake = ShakeState::with_rng_indices(indices); }

    /// The original field updater keeps screen effects moving while dialogue holds script time.
    pub fn advance_effects(&mut self, frames: f32) {
        self.fade.advance(frames);
        self.shake.advance(frames);
    }

    #[cfg(test)]
    pub fn actor(&self, entity: u8) -> Option<&Actor> {
        self.actors.get(&entity)
    }

    /// Every actor, by script entity.
    pub fn actors(&self) -> impl Iterator<Item = &Actor> {
        self.actors.values()
    }

    #[cfg(test)]
    pub fn camera(&self) -> CameraState {
        self.camera
    }

    #[cfg(test)]
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Whether any actor is walking or turning.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.actors.values().any(Actor::busy)
    }

    /// Everybody vanishes and the camera is the field's again (the end of the scene).
    pub fn clear(&mut self) {
        self.actors.clear();
        self.camera = CameraState::Default;
        self.camera_frames = 0.0;
        self.fade = FadeState::default();
        self.shake = ShakeState::with_rng_indices(self.shake.rng_indices());
    }

    /// Move every walk and turn on by `frames` of scene time (no time, or time that is not a number, moves nothing).
    pub fn advance(&mut self, frames: f32) {
        if frames.partial_cmp(&0.0).is_none_or(|o| o.is_le()) {
            return;
        }
        self.advance_effects(frames);
        self.camera_frames += frames;
        for actor in self.actors.values_mut() {
            actor.advance(frames);
        }
    }

    /// Finish every walk and turn at once (a bot that does not wait for the picture).
    pub fn settle(&mut self) {
        for actor in self.actors.values_mut().filter(|a| a.busy()) {
            actor.settle();
        }
    }

    /// Remember verified setup metadata without changing the currently displayed pose.
    pub fn remember_default(&mut self, entity: u8, slot: u8, clip: &str) {
        if let Some(actor) = self.actors.get_mut(&entity) {
            actor.default_animation = Some(DefaultAnimation { slot, clip: clip.to_string() });
        }
    }

    /// Set a resolved full-range loop at normal or doubled speed without changing the actor's model or movement.
    pub fn apply_loop(&mut self, entity: u8, slot: u8, clip: &str, overshoot: f32, doubled: bool) {
        let Some(actor) = self.actors.get_mut(&entity) else { return self.off_cast() };
        actor.replace_kept_animation();
        let rate = if doubled { 2.0 } else { 1.0 };
        let frame = if overshoot.is_finite() { overshoot.max(0.0) * rate } else { 0.0 };
        actor.default_animation = Some(DefaultAnimation { slot, clip: clip.to_string() });
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::Loop(frame), rate, once: false });
    }

    /// Use the resolved ordinary MOVE clip only during this movement. The scene player supplies
    /// a phase offset only for a verified uninterrupted same-slot MOVE; otherwise it starts at zero.
    pub fn apply_move_animation(&mut self, entity: u8, clip: &str, phase_offset: f32) {
        let Some(actor) = self.actors.get_mut(&entity) else { return };
        if let Some(walk) = actor.walk.filter(|walk| walk.how == MoveHow::Walk) {
            let phase = if phase_offset.is_finite() { phase_offset.max(0.0) } else { 0.0 };
            actor.movement_animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::Loop(phase + walk.elapsed), rate: 1.0, once: false });
        }
    }

    /// Hold one exact authored frame. The avatar pipeline validates its bounds in the loaded clip.
    pub fn apply_hold(&mut self, entity: u8, clip: &str, frame: u8) {
        let Some(actor) = self.actors.get_mut(&entity) else { return self.off_cast() };
        actor.replace_kept_animation();
        actor.movement_animation = None;
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::Hold(frame), rate: 1.0, once: false });
    }

    /// Play one normal-speed forward range, holding its final frame. Loaded clip bounds are
    /// checked by the avatar pipeline before it samples any pose.
    pub fn apply_hold_range(&mut self, entity: u8, clip: &str, first: u8, last: u8, overshoot: f32) {
        self.hold_range_at_rate(entity, clip, first, last, overshoot, 1.0);
    }

    /// A supported doubled forward Hold uses the same authored endpoints and holds its last
    /// pose; only clip time advances twice as quickly as the stage clock.
    pub fn apply_hold_range_doubled(&mut self, entity: u8, clip: &str, first: u8, last: u8, overshoot: f32) {
        self.hold_range_at_rate(entity, clip, first, last, overshoot, 2.0);
    }

    fn hold_range_at_rate(&mut self, entity: u8, clip: &str, first: u8, last: u8, overshoot: f32, rate: f32) {
        let Some(actor) = self.actors.get_mut(&entity) else { return self.off_cast() };
        actor.replace_kept_animation();
        let elapsed = if overshoot.is_finite() { overshoot.max(0.0) * rate } else { 0.0 };
        actor.movement_animation = None;
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::HoldRange { first, last, elapsed }, rate, once: false });
    }

    /// Play from frame zero at the normal or supported doubled rate, holding the loaded clip's
    /// actual last frame. Sampling resolves the endpoint; this does not change the interpreter's waits.
    pub fn apply_hold_full(&mut self, entity: u8, clip: &str, overshoot: f32, doubled: bool) {
        let Some(actor) = self.actors.get_mut(&entity) else { return self.off_cast() };
        actor.replace_kept_animation();
        let rate = if doubled { 2.0 } else { 1.0 };
        let elapsed = if overshoot.is_finite() { overshoot.max(0.0) * rate } else { 0.0 };
        actor.movement_animation = None;
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::HoldFull { elapsed }, rate, once: false });
    }

    /// A bounded Once can return to a verified, different default slot. The same slot is
    /// supported only for `[0, 0]` on the exact default clip: its return begins at frame zero.
    /// Other same-slot phase retention and gestures started during movement remain unsupported.
    pub fn apply_once(&mut self, entity: u8, slot: u8, clip: &str, first: u8, last: u8, overshoot: f32) -> bool {
        let Some(actor) = self.actors.get_mut(&entity) else { return false };
        let known_return = actor.default_animation.as_ref().is_some_and(|default|
            default.slot != slot || (default.clip == clip && first == 0 && last == 0));
        if first > last || actor.walk.is_some() || !known_return { return false; }
        let elapsed = if overshoot.is_nan() { 0.0 } else { overshoot.max(0.0) };
        actor.movement_animation = None;
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame: ClipFrame::HoldRange { first, last, elapsed }, rate: 1.0, once: true });
        actor.finish_once();
        true
    }

    /// A full Once carries its elapsed time and verified default to the avatar cache. At the
    /// normal rate the same exact clip can return to itself: reaching its end restarts frame zero.
    /// Only the cache knows the actual loaded clip length; no guessed deadline ends this pose.
    pub fn apply_once_full(&mut self, entity: u8, slot: u8, clip: &str, overshoot: f32) -> bool {
        self.once_full_at_rate(entity, slot, clip, overshoot, false)
    }

    /// A full Once at doubled speed, returning to the same verified normal standing default.
    /// The avatar cache converts the gesture's excess clip time back to scene time on return.
    pub fn apply_once_full_doubled(&mut self, entity: u8, slot: u8, clip: &str, overshoot: f32) -> bool {
        self.once_full_at_rate(entity, slot, clip, overshoot, true)
    }

    fn once_full_at_rate(&mut self, entity: u8, slot: u8, clip: &str, overshoot: f32, doubled: bool) -> bool {
        let Some(actor) = self.actors.get_mut(&entity) else { return false };
        let known_return = actor.default_animation.as_ref().is_some_and(|default|
            default.slot != slot || (!doubled && default.clip == clip));
        if actor.walk.is_some() || !known_return { return false; }
        let rate = if doubled { 2.0 } else { 1.0 };
        let elapsed = if overshoot.is_finite() { overshoot.max(0.0) * rate } else { 0.0 };
        let frame = if doubled { ClipFrame::OnceFullDoubled { elapsed } } else { ClipFrame::OnceFull { elapsed } };
        actor.movement_animation = None;
        actor.animation = Some(SceneAnimation { clip: clip.to_string(), frame, rate, once: true });
        true
    }

    /// Show one event of the interpreter. `overshoot` is how many frames ago it should have happened (the clock ran past
    /// its time since the last call): the motion it starts is that far along already. `height` is the ground's height, for
    /// the events that do not carry one.
    pub fn apply(&mut self, event: &Stage, overshoot: f32, height: Height<'_>) {
        let overshoot = if overshoot.is_nan() { 0.0 } else { overshoot.max(0.0) };
        match event {
            Stage::Sound { .. } => {},
            Stage::Background(op) => self.background_ops.push(*op),
            Stage::Fade(op) => {
                if self.fade.apply(*op) { self.fade.advance(overshoot); }
            }
            Stage::Shake { mask, x_amplitude, x_steps, y_amplitude, y_steps } => {
                if self.shake.apply(*mask, *x_amplitude, *x_steps, *y_amplitude, *y_steps) {
                    self.shake.advance(overshoot);
                }
            }
            Stage::Camera(op) => {
                self.counts.camera_ops = self.counts.camera_ops.saturating_add(1);
                // A late event is that far into its scroll already, as a late walk is into its walk.
                self.camera_frames = overshoot;
                self.camera = match *op {
                    CameraOp::Scroll { x, y, frames } => CameraState::Scroll { x, y, frames },
                    CameraOp::Follow { entity } => CameraState::Follow { entity },
                    CameraOp::Reset => CameraState::Default,
                };
            }
            Stage::Place { entity, x, y, z, triangle } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.walk = None;
                actor.movement_animation = None;
                let z = z.map(f32::from).or_else(|| height(*x, *y, *triangle)).or_else(|| actor.pos.map(|p| p[2])).unwrap_or(0.0);
                actor.pos = Some([f32::from(*x), f32::from(*y), z]);
                actor.triangle = *triangle;
            }
            Stage::Offset { entity, x, y, z } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.offset_motion = None;
                actor.render_offset = [*x, *y, *z];
            }
            Stage::OffsetMove { entity, motion } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.render_offset = motion.start();
                actor.offset_motion = Some((*motion, 0.0));
                actor.advance_offset(overshoot);
            }
            Stage::Move { entity, from, x, y, z, triangle, frames, how, .. } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                if *how == MoveHow::Jump && (actor.pos.is_none() || z.is_none()) {
                    self.counts.moves_skipped = self.counts.moves_skipped.saturating_add(1);
                    return;
                }
                actor.movement_animation = None;
                // FMOVE and JUMP leave an explicitly resolved normal loop running. Restoring a
                // Once's default below must not promote an unsupported movement/gesture combination.
                let keep_animation = matches!(how, MoveHow::WalkKeepAnimation | MoveHow::Jump) && actor.has_normal_loop()
                    && actor.walk.is_none_or(|walk| walk.keep_animation);
                if actor.animation.as_ref().is_some_and(|animation| animation.once) { actor.restore_default(0.0); }
                // Other movement uses its own clip rather than this standing default. Restart
                // the default on arrival; any unused part of a tick advances it.
                match &mut actor.animation {
                    Some(SceneAnimation { frame: ClipFrame::Loop(frame), .. }) if !keep_animation => *frame = 0.0,
                    Some(SceneAnimation { frame: ClipFrame::Loop(_), .. }) => {},
                    _ => actor.animation = None, // A new movement replaces a held pose or range.
                }
                // A second move of the same actor starts where the first one was going.
                if let Some(walk) = actor.walk.take() {
                    actor.pos = Some(walk.jump.map_or(walk.to, |jump| jump.at(f32::INFINITY)));
                    if walk.jump.is_some() { actor.triangle = walk.landing_triangle; }
                }
                let target_xy = (f32::from(*x), f32::from(*y));
                let start = match (from, actor.pos) {
                    // Preserve the actual Q12 landing remainder across consecutive jumps.
                    (_, Some(here)) if *how == MoveHow::Jump => here,
                    (Some((fx, fy)), here) => {
                        let z = here.map(|p| p[2]).or_else(|| height(*fx, *fy, None)).unwrap_or(0.0);
                        [f32::from(*fx), f32::from(*fy), z]
                    }
                    (None, Some(here)) => here,
                    (None, None) => [target_xy.0, target_xy.1, 0.0],
                };
                // ScenePlay resolves a jump against the authored triangle's infinite plane.
                // The renderer's generic ground callback is a walkability query and must not
                // silently substitute another surface for an unresolved jump.
                let z = z.map(f32::from).or_else(|| (*how != MoveHow::Jump).then(|| height(*x, *y, *triangle)).flatten());
                let to = [target_xy.0, target_xy.1, z.unwrap_or(start[2])];
                let jump = if *how == MoveHow::Jump {
                    let Some(jump) = z.and_then(|_| JumpMotion::from_stage_frames(start, to, *frames)) else {
                        self.counts.moves_skipped = self.counts.moves_skipped.saturating_add(1);
                        return;
                    };
                    Some(jump)
                } else { None };
                let (dx, dy) = (to[0] - start[0], to[1] - start[1]);
                if is_walk(*how) && dx.hypot(dy) > 0.5 {
                    actor.heading = dy.atan2(dx);
                    actor.turn = None;
                }
                actor.pos = Some(start);
                actor.walk = Some(Walk { from: start, to, frames: *frames as f32, elapsed: 0.0, how: *how, keep_animation,
                    jump, landing_triangle: *triangle });
                // (a walk of no frames is a jump to the place: `advance` finishes it at once)
                actor.advance_walk(overshoot);
            }
            Stage::Face { entity, heading, frames } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                let heading = wrap(*heading);
                if *frames == 0 {
                    actor.heading = heading;
                    actor.turn = None;
                } else {
                    actor.turn = Some(Turn { from: actor.heading, to: heading, frames: *frames as f32, elapsed: 0.0 });
                    actor.advance_turn(overshoot);
                }
            }
            Stage::Show { entity, visible } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                actor.visible = *visible;
            }
            Stage::Anim { entity, how, .. } => {
                if let Some(actor) = self.actors.get_mut(entity) {
                    actor.replace_kept_animation();
                    if *how == AnimHow::Loop { actor.default_animation = None; }
                    actor.animation = None;
                    actor.movement_animation = None;
                    self.counts.anims_skipped = self.counts.anims_skipped.saturating_add(1);
                } else {
                    self.off_cast();
                }
            }
            Stage::AnimSpeed { entity, speed } => {
                let Some(actor) = self.actors.get_mut(entity) else { return self.off_cast() };
                // ASPED 16 is one clip frame per scene frame, 32 is two. The player advances
                // precisely to each event: changing a standing loop's rate keeps its current phase.
                // A standing Hold preserves its phase and known default. The bridge also returns
                // a doubled full Once to normal speed; retain that one rate boundary so the avatar
                // can resolve completion against the loaded length without guessing it here.
                if actor.walk.is_none() && matches!(speed, 16 | 32) {
                    // Setup can name a verified standing default before any scene clip plays.
                    // Cargo's first ASPED precedes its first Hold: no active phase needs changing,
                    // and discarding that metadata would make the later Once unable to return.
                    if actor.animation.is_none() && actor.movement_animation.is_none() { return; }
                    if let Some(animation) = &mut actor.animation {
                        if !animation.once {
                            animation.rate = if *speed == 32 { 2.0 } else { 1.0 };
                            return;
                        }
                        match animation.frame {
                            ClipFrame::OnceFullDoubled { elapsed } if *speed == 16 => {
                                animation.frame = ClipFrame::OnceFullSlowed { elapsed, doubled_until: elapsed };
                                animation.rate = 1.0;
                                return;
                            }
                            ClipFrame::OnceFullDoubled { .. } if *speed == 32 => return,
                            ClipFrame::OnceFullSlowed { .. } if *speed == 16 => return,
                            _ => {},
                        }
                    }
                }
                actor.replace_kept_animation();
                // A fresh DFANM can establish another supported loop after this fallback.
                actor.default_animation = None;
                let standing = actor.animation.take().is_some();
                let moving = actor.movement_animation.take().is_some();
                if standing || moving {
                    self.counts.anims_skipped = self.counts.anims_skipped.saturating_add(1);
                }
            }
        }
    }

    fn off_cast(&mut self) {
        self.counts.off_cast = self.counts.off_cast.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::scene::AnimHow;

    fn flat(_x: i16, _y: i16, _t: Option<u16>) -> Option<f32> {
        Some(7.0)
    }

    fn place(entity: u8, x: i16, y: i16) -> Stage {
        Stage::Place { entity, x, y, z: None, triangle: Some(3) }
    }

    fn walk(entity: u8, x: i16, y: i16, frames: u32) -> Stage {
        Stage::Move { entity, from: None, x, y, z: None, triangle: None, frames, how: MoveHow::Walk, animation: None }
    }

    #[test]
    fn standing_loops_yield_to_movement_and_restart_after_arrival() {
        for how in [MoveHow::Walk, MoveHow::Ladder] {
            let mut cast = Cast::new([1]);
            cast.apply(&place(1, 0, 0), 0.0, &flat);
            cast.apply_loop(1, 0, "idle", 0.0, false);
            cast.advance(7.0);
            assert_eq!(cast.actor(1).unwrap().animation(), Some(("idle", ClipFrame::Loop(7.0))));
            cast.apply(&Stage::Move { entity: 1, from: None, x: 100, y: 0, z: None, triangle: None, frames: 10, how, animation: None }, 0.0, &flat);
            assert_eq!(cast.actor(1).unwrap().animation(), None);
            cast.advance(4.0);
            assert_eq!(cast.actor(1).unwrap().animation(), None, "standing default must not replace a moving pose");
            cast.advance(9.0);
            assert_eq!(cast.actor(1).unwrap().animation(), Some(("idle", ClipFrame::Loop(3.0))), "only time after arrival advances the restarted idle");
            assert_eq!(cast.actor(1).unwrap().pos(), Some([100.0, 0.0, 7.0]));
        }
    }

    #[test]
    fn a_jump_keeps_the_explicit_normal_loop_and_its_phase_through_arrival() {
        for chunks in [vec![23.0], vec![4.0, 5.0, 14.0]] {
            let mut cast = Cast::new([11]);
            cast.apply(&place(11, 0, 0), 0.0, &flat);
            cast.apply_loop(11, 13, "btce", 0.0, false);
            cast.advance(15.0); // cargoin: DFANM 13, WAIT 15, JUMP, VISI 0.
            cast.apply(&Stage::Move { entity: 11, from: None, x: 100, y: 0, z: Some(7), triangle: Some(0),
                frames: 20, how: MoveHow::Jump, animation: None }, 0.0, &flat);
            assert_eq!(cast.actor(11).unwrap().animation(), Some(("btce", ClipFrame::Loop(15.0))));
            let mut elapsed = 0.0;
            for frames in chunks {
                cast.advance(frames);
                elapsed += frames;
                let actor = cast.actor(11).unwrap();
                assert_eq!(actor.animation(), Some(("btce", ClipFrame::Loop(15.0 + elapsed))));
                let motion = JumpMotion::from_stage_frames([0.0, 0.0, 7.0], [100.0, 0.0, 7.0], 20).unwrap();
                assert_eq!(actor.pos(), Some(motion.at(elapsed)));
                assert!(!actor.moving(), "JUMP never substitutes a walking clip");
                assert_eq!(actor.walked_frames(), 0.0);
            }
            assert_eq!(cast.actor(11).unwrap().pos(), Some([100.0, 0.0, 7.0]));
            cast.apply(&Stage::Show { entity: 11, visible: false }, 0.0, &flat);
            assert!(!cast.actor(11).unwrap().shown());
            assert_eq!(cast.actor(11).unwrap().animation(), Some(("btce", ClipFrame::Loop(38.0))));
            cast.apply(&walk(11, 120, 0, 4), 0.0, &flat);
            assert_eq!(cast.actor(11).unwrap().animation(), None, "ordinary movement still replaces the jump clip");
            cast.advance(5.0);
            assert_eq!(cast.actor(11).unwrap().animation(), Some(("btce", ClipFrame::Loop(1.0))));
        }
    }

    #[test]
    fn a_jump_does_not_invent_a_clip_or_promote_a_doubled_loop_hold_or_once() {
        for form in 0..4 {
            let mut cast = Cast::new([11]);
            cast.apply(&place(11, 0, 0), 0.0, &flat);
            cast.remember_default(11, 13, "btce");
            match form {
                0 => {}, // Metadata alone does not select a clip to show.
                1 => cast.apply_loop(11, 13, "btce", 0.0, true),
                2 => cast.apply_hold(11, "btce", 2),
                _ => { assert!(cast.apply_once_full(11, 13, "btce", 0.0)); },
            }
            cast.advance(15.0);
            cast.apply(&Stage::Move { entity: 11, from: None, x: 100, y: 0, z: Some(7), triangle: None,
                frames: 20, how: MoveHow::Jump, animation: None }, 0.0, &flat);
            cast.advance(5.0);
            assert_eq!(cast.actor(11).unwrap().animation(), None, "unsupported jump combination {form}");
        }
    }

    #[test]
    fn same_default_zero_frame_and_full_once_restart_the_verified_normal_clip() {
        let mut cast = Cast::new([35]);
        cast.apply(&place(35, 0, 0), 0.0, &flat);
        cast.apply_loop(35, 4, "cdie", 0.0, false);
        assert!(cast.apply_once(35, 4, "cdie", 0, 0, 0.0));
        cast.advance(0.5);
        assert_eq!(cast.actor(35).unwrap().animation().unwrap().1.in_clip(8), Some(0.0));
        cast.advance(1.0);
        assert_eq!(cast.actor(35).unwrap().animation(), Some(("cdie", ClipFrame::Loop(0.5))), "only time after the authored one-frame range advances the loop");
        cast.advance(59.5);
        assert_eq!(cast.actor(35).unwrap().animation(), Some(("cdie", ClipFrame::Loop(60.0))));
        assert!(cast.apply_once_full(35, 4, "cdie", 0.0), "tin_1 restarts the same slot 61 frames after the bounded Once");
        assert_eq!(cast.actor(35).unwrap().animation(), Some(("cdie", ClipFrame::OnceFull { elapsed: 0.0 })));
        cast.advance(7.5);
        let actor = cast.actor(35).unwrap();
        assert_eq!(actor.animation().unwrap().1.in_clip(8), Some(7.0));
        assert_eq!(actor.animation().unwrap().1.return_elapsed(8), None);
        assert_eq!(actor.full_once_return(), Some("cdie"));
        cast.advance(0.5);
        assert_eq!(cast.actor(35).unwrap().animation().unwrap().1.return_elapsed(8), Some(0.0));
        cast.advance(2.5);
        assert_eq!(cast.actor(35).unwrap().animation().unwrap().1.return_elapsed(8), Some(2.5));
        cast.apply_hold(35, "cdie", 3);
        cast.advance(100.0);
        assert_eq!(cast.actor(35).unwrap().animation(), Some(("cdie", ClipFrame::Hold(3))));
        assert_eq!(cast.actor(35).unwrap().full_once_return(), None, "replacement cancels the pending same-slot return");
    }

    #[test]
    fn same_default_once_still_rejects_other_ranges_rates_clips_and_active_movement() {
        let mut cast = Cast::new([35]);
        cast.apply(&place(35, 0, 0), 0.0, &flat);
        assert!(!cast.apply_once(35, 4, "cdie", 0, 0, 0.0));
        assert!(!cast.apply_once_full(35, 4, "cdie", 0.0), "no guessed default");
        cast.apply_loop(35, 4, "cdie", 0.0, false);
        for (first, last) in [(0, 1), (1, 1), (2, 5), (5, 2)] {
            assert!(!cast.apply_once(35, 4, "cdie", first, last, 0.0), "unsupported same-slot range {first}..{last}");
        }
        assert!(!cast.apply_once(35, 4, "different", 0, 0, 0.0));
        assert!(!cast.apply_once_full(35, 4, "different", 0.0), "slot identity alone cannot authorize another clip");
        assert!(!cast.apply_once_full_doubled(35, 4, "cdie", 0.0), "same-slot doubled return would retain a different rate");
        cast.apply(&walk(35, 100, 0, 20), 0.0, &flat);
        assert!(!cast.apply_once(35, 4, "cdie", 0, 0, 0.0));
        assert!(!cast.apply_once_full(35, 4, "cdie", 0.0), "gestures during movement remain unsupported");
    }

    #[test]
    fn doubled_loops_catch_up_and_rate_changes_preserve_their_phase() {
        let mut cast = Cast::new([1]);
        cast.apply_loop(1, 3, "three", 3.0, true);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("three", ClipFrame::Loop(6.0))));
        cast.advance(4.0);
        cast.apply(&Stage::AnimSpeed { entity: 1, speed: 16 }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("three", ClipFrame::Loop(14.0))), "resetting the rate does not restart the clip");
        cast.advance(3.0);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("three", ClipFrame::Loop(17.0))));
        cast.apply(&Stage::AnimSpeed { entity: 1, speed: 32 }, 0.0, &flat);
        cast.advance(2.0);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("three", ClipFrame::Loop(21.0))));
        cast.apply_loop(1, 4, "four", 0.0, true);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("four", ClipFrame::Loop(0.0))), "a fresh loop starts its own phase");
        assert_eq!(cast.counts().anims_skipped, 0);
        for late in [f32::NAN, f32::INFINITY, -4.0] {
            cast.apply_loop(1, 3, "three", late, true);
            assert_eq!(cast.actor(1).unwrap().animation(), Some(("three", ClipFrame::Loop(0.0))));
        }
    }

    #[test]
    fn doubled_ranges_hold_the_authored_endpoint_and_replacements_reset_the_rate() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply_loop(1, 0, "idle", 0.0, false);
        cast.apply_hold_range_doubled(1, "gesture", 15, 29, 1.25);
        let frame = cast.actor(1).unwrap().animation().unwrap().1;
        assert_eq!(frame.in_clip(30), Some(17.5), "late delivery is scaled once");
        cast.advance(5.75);
        assert_eq!(cast.actor(1).unwrap().animation().unwrap().1.in_clip(30), Some(29.0));
        cast.advance(100.0);
        assert_eq!(cast.actor(1).unwrap().animation().unwrap().1.in_clip(30), Some(29.0), "holding never wraps");
        cast.apply_hold_range(1, "normal", 1, 7, 1.0);
        cast.advance(1.0);
        assert_eq!(cast.actor(1).unwrap().animation().unwrap().1.in_clip(8), Some(3.0), "replacement restores normal rate");
        cast.apply_hold_range_doubled(1, "gesture", 0, 14, 0.0);
        cast.apply_loop(1, 0, "idle", 0.0, false);
        cast.advance(4.0);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("idle", ClipFrame::Loop(4.0))));
        cast.apply_hold_range_doubled(1, "gesture", 0, 14, 0.0);
        cast.apply(&walk(1, 20, 0, 3), 0.0, &flat);
        cast.advance(30.0);
        assert_eq!(cast.actor(1).unwrap().animation(), None, "movement permanently replaces a Hold");
    }

    #[test]
    fn doubled_full_once_keeps_its_return_and_cancels_cleanly_for_replacement_and_movement() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        assert!(!cast.apply_once_full_doubled(1, 3, "gesture", 0.0), "no guessed default");
        cast.apply_loop(1, 0, "idle", 0.0, false);
        assert!(!cast.apply_once_full_doubled(1, 0, "idle", 0.0), "same-slot phase remains unsupported");
        assert!(cast.apply_once_full_doubled(1, 3, "gesture", 1.25));
        cast.advance(0.5);
        let actor = cast.actor(1).unwrap();
        assert_eq!(actor.animation(), Some(("gesture", ClipFrame::OnceFullDoubled { elapsed: 3.5 })));
        assert_eq!(actor.full_once_return(), Some("idle"));
        assert_eq!(actor.animation().unwrap().1.in_clip(4), Some(3.0));
        cast.advance(0.75);
        assert_eq!(cast.actor(1).unwrap().animation().unwrap().1.return_elapsed(4), Some(0.5));
        assert!(cast.apply_once_full_doubled(1, 3, "gesture", 0.0));
        assert_eq!(cast.actor(1).unwrap().animation().unwrap().1, ClipFrame::OnceFullDoubled { elapsed: 0.0 }, "repeat starts afresh");
        cast.apply_hold(1, "pose", 2);
        cast.advance(50.0);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("pose", ClipFrame::Hold(2))));
        assert_eq!(cast.actor(1).unwrap().full_once_return(), None, "no delayed return after replacement");
        assert!(cast.apply_once_full_doubled(1, 3, "gesture", 0.0));
        cast.apply(&walk(1, 20, 0, 3), 0.0, &flat);
        cast.advance(5.0);
        assert_eq!(cast.actor(1).unwrap().animation(), Some(("idle", ClipFrame::Loop(2.0))), "arrival resumes default at normal speed");
        assert_eq!(cast.actor(1).unwrap().full_once_return(), None);
        for late in [f32::NAN, f32::INFINITY, -4.0] {
            assert!(cast.apply_once_full_doubled(1, 3, "gesture", late));
            assert_eq!(cast.actor(1).unwrap().animation().unwrap().1, ClipFrame::OnceFullDoubled { elapsed: 0.0 });
        }
    }

    #[test]
    fn bridge_hold_then_fast_once_then_normal_rate_preserves_the_verified_default() {
        let mut cast = Cast::new([1]);
        cast.apply_loop(1, 0, "idle", 0.0, false);
        cast.apply_hold_full(1, "waiting", 0.0, false);
        cast.advance(25.0);
        cast.apply(&Stage::AnimSpeed { entity: 1, speed: 32 }, 0.0, &flat);
        assert!(cast.apply_once_full_doubled(1, 4, "gesture", 0.0), "supported Hold rate change keeps the known standing slot");
        cast.advance(7.0);
        cast.apply(&Stage::AnimSpeed { entity: 1, speed: 16 }, 0.0, &flat);
        let frame = cast.actor(1).unwrap().animation().unwrap().1;
        assert_eq!(frame, ClipFrame::OnceFullSlowed { elapsed: 14.0, doubled_until: 14.0 });
        assert_eq!(frame.in_clip(15), Some(14.0), "the owned gesture has fifteen frames");
        cast.advance(0.5);
        let frame = cast.actor(1).unwrap().animation().unwrap().1;
        assert_eq!(frame.in_clip(15), Some(14.0));
        assert_eq!(frame.return_elapsed(15), None);
        cast.advance(1.0);
        let actor = cast.actor(1).unwrap();
        assert_eq!(actor.animation().unwrap().1.return_elapsed(15), Some(0.5));
        assert_eq!(actor.full_once_return(), Some("idle"));
        assert_eq!(cast.counts().anims_skipped, 0);
    }

    #[test]
    fn cargo_speed_before_the_first_clip_preserves_setup_default_for_its_later_once() {
        let mut cast = Cast::new([12]);
        cast.remember_default(12, 0, "adcb");
        cast.apply(&place(12, 96, 22), 0.0, &flat);
        // The first ASPED appears before any visible clip, then bounded doubled Holds and
        // a full doubled Hold run before the script restores normal speed.
        cast.apply(&Stage::AnimSpeed { entity: 12, speed: 32 }, 0.0, &flat);
        assert_eq!(cast.actor(12).unwrap().animation(), None, "remembering a default does not start or retime an idle");
        cast.apply_hold_range_doubled(12, "cdaf", 0, 14, 0.0);
        cast.advance(8.0);
        cast.apply_hold_range_doubled(12, "cdbb", 15, 29, 0.0);
        cast.advance(8.0);
        cast.apply(&Stage::AnimSpeed { entity: 12, speed: 32 }, 0.0, &flat);
        cast.apply_hold_full(12, "cdbc", 0.0, true);
        cast.advance(18.0);
        cast.apply(&Stage::AnimSpeed { entity: 12, speed: 16 }, 0.0, &flat);
        cast.apply_hold_range(12, "cdbb", 15, 29, 0.0);
        cast.advance(58.0);
        assert!(cast.apply_once_full(12, 8, "cdbd", 0.0), "the distinct setup default survives until the authored Once");
        assert_eq!(cast.actor(12).unwrap().full_once_return(), Some("adcb"));
        assert_eq!(cast.counts().anims_skipped, 0);
        // Unsupported speed still invalidates an unplayed default; it cannot be used to
        // manufacture a later supported restoration target.
        let mut unsupported = Cast::new([12]);
        unsupported.remember_default(12, 0, "adcb");
        unsupported.apply(&Stage::AnimSpeed { entity: 12, speed: 48 }, 0.0, &flat);
        assert!(!unsupported.apply_once_full(12, 8, "cdbd", 0.0));
    }

    #[test]
    fn unsupported_rates_and_unhandled_once_or_moving_speed_changes_keep_the_existing_fallback() {
        for speed in [0, 8, 24, 48, u16::MAX] {
            let mut cast = Cast::new([1]);
            cast.apply_loop(1, 3, "three", 2.0, true);
            cast.apply(&Stage::AnimSpeed { entity: 1, speed }, 0.0, &flat);
            assert_eq!(cast.actor(1).unwrap().animation(), None, "speed {speed}");
            assert!(!cast.apply_once(1, 4, "four", 0, 2, 0.0), "unsupported rate also invalidates the remembered default");
            assert_eq!(cast.counts().anims_skipped, 1);
        }
        for speed in [16, 32] {
            for mode in 1..4 {
                let mut cast = Cast::new([1]);
                cast.apply(&place(1, 0, 0), 0.0, &flat);
                cast.apply_loop(1, 3, "three", 0.0, false);
                match mode {
                    1 => { assert!(cast.apply_once(1, 4, "four", 0, 9, 0.0)); },
                    2 => { assert!(cast.apply_once_full(1, 4, "four", 0.0)); },
                    _ => cast.apply(&walk(1, 100, 0, 10), 0.0, &flat),
                }
                cast.apply(&Stage::AnimSpeed { entity: 1, speed }, 0.0, &flat);
                cast.advance(20.0);
                assert_eq!(cast.actor(1).unwrap().animation(), None, "speed {speed}, mode {mode}");
                assert_eq!(cast.counts().anims_skipped, 1);
            }
        }
    }

    #[test]
    fn a_place_stands_the_actor_there_at_the_grounds_height_unless_the_script_gives_one() {
        let mut cast = Cast::new([1, 2]);
        assert!(!cast.actor(1).unwrap().shown(), "nothing is seen before it is placed");
        cast.apply(&place(1, 10, -20), 0.0, &flat);
        let a = cast.actor(1).unwrap();
        assert_eq!((a.pos(), a.triangle(), a.shown()), (Some([10.0, -20.0, 7.0]), Some(3), true));
        cast.apply(&Stage::Place { entity: 2, x: 1, y: 2, z: Some(-9), triangle: None }, 0.0, &flat);
        assert_eq!(cast.actor(2).unwrap().pos(), Some([1.0, 2.0, -9.0]));
        // Off the mesh and no height of its own: it keeps the height it had.
        cast.apply(&Stage::Place { entity: 2, x: 5, y: 5, z: None, triangle: None }, 0.0, &|_, _, _| None);
        assert_eq!(cast.actor(2).unwrap().pos(), Some([5.0, 5.0, -9.0]));
    }

    #[test]
    fn a_walk_is_a_straight_line_over_its_frames_heading_toward_the_target() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 50), 0.0, &flat);
        let a = cast.actor(1).unwrap();
        assert!(a.moving() && cast.busy());
        assert_eq!(a.heading(), 0.0, "+x");
        cast.advance(25.0);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some([50.0, 0.0, 7.0]), "half way after half the frames");
        cast.advance(1000.0);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some([100.0, 0.0, 7.0]));
        assert!(!a.moving() && !cast.busy(), "it arrived and stands");
        // Walking up the +y axis faces it that way.
        cast.apply(&walk(1, 100, 80, 10), 0.0, &flat);
        assert!((cast.actor(1).unwrap().heading() - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn a_move_starts_from_the_events_own_from_when_it_has_one_and_a_second_move_finishes_the_first() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 40, 0, 100), 0.0, &flat);
        cast.advance(10.0);
        // The script's next walk was planned from where the first one ends.
        cast.apply(&Stage::Move { entity: 1, from: Some((40, 0)), x: 40, y: 60, z: None, triangle: None, frames: 20, how: MoveHow::Walk, animation: None }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([40.0, 0.0, 7.0]));
        cast.advance(10.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([40.0, 30.0, 7.0]));
    }

    #[test]
    fn catching_up_one_motion_does_not_advance_the_other_twice() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 100), 0.0, &flat);
        cast.advance(20.0);
        cast.apply(&Stage::Face { entity: 1, heading: 1.0, frames: 80 }, 10.0, &flat);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some([20.0, 0.0, 7.0]), "late turn leaves the already advanced walk alone");
        assert_eq!(a.walked_frames(), 20.0);
        assert!((a.heading() - 0.125).abs() < 1e-5);
        cast.advance(10.0);
        let heading = cast.actor(1).unwrap().heading();
        // A jump does not replace the turn. Catching up that jump must not tick the turn again.
        let jump = Stage::Move { entity: 1, from: None, x: 120, y: 0, z: Some(7), triangle: None, frames: 20, how: MoveHow::Jump, animation: None };
        cast.apply(&jump, 5.0, &flat);
        let a = cast.actor(1).unwrap();
        assert_eq!(a.pos(), Some(JumpMotion::from_stage_frames([100.0, 0.0, 7.0], [120.0, 0.0, 7.0], 20).unwrap().at(5.0)));
        assert_eq!(a.heading(), heading, "late jump leaves the already advanced turn alone");
        assert_eq!(a.walked_frames(), 30.0, "jump time is not walking animation time");
    }

    #[test]
    fn an_event_that_is_late_starts_its_motion_that_far_along() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 100), 30.0, &flat);
        let [x, y, z] = cast.actor(1).unwrap().pos().unwrap();
        assert!((x - 30.0).abs() < 1e-4 && y == 0.0 && z == 7.0, "{x} {y} {z}");
        // A negative or nonsense lateness is none.
        for late in [f32::NAN, -5.0] {
            let mut cast = Cast::new([1]);
            cast.apply(&place(1, 0, 0), 0.0, &flat);
            cast.apply(&walk(1, 100, 0, 100), late, &flat);
            assert_eq!(cast.actor(1).unwrap().pos(), Some([0.0, 0.0, 7.0]), "{late}");
        }
    }

    #[test]
    fn a_move_of_no_frames_is_a_jump_and_a_place_cancels_a_walk() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 12, 34, 0), 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([12.0, 34.0, 7.0]));
        assert!(!cast.actor(1).unwrap().moving());
        cast.apply(&walk(1, 100, 0, 100), 0.0, &flat);
        cast.apply(&place(1, -5, -5), 0.0, &flat);
        cast.advance(50.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([-5.0, -5.0, 7.0]), "the walk was cancelled");
        // A jump and a ladder are not walks: the model does not walk for them.
        cast.apply(&Stage::Move { entity: 1, from: None, x: 0, y: 0, z: Some(50), triangle: None, frames: 30, how: MoveHow::Ladder, animation: None }, 0.0, &flat);
        assert!(!cast.actor(1).unwrap().moving());
    }

    #[test]
    fn a_jump_has_an_arc_and_changes_triangle_on_the_original_completion_update() {
        let mut cast = Cast::new([1]);
        cast.apply(&Stage::Place { entity: 1, x: 0, y: 0, z: Some(0), triangle: Some(0) }, 0.0, &flat);
        let jump = Stage::Move { entity: 1, from: Some((0, 0)), x: 40, y: 20, z: Some(96), triangle: Some(1), frames: 14, how: MoveHow::Jump, animation: None };
        cast.apply(&jump, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([0.0; 3]), "step zero initializes without moving");
        cast.advance(6.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([20.0, 10.0, 48.0 + 70.3125]));
        assert!(!cast.actor(1).unwrap().moving(), "a jump does not use the walking animation");
        cast.advance(6.0);
        assert_eq!(cast.actor(1).unwrap().pos(), Some([40.0, 20.0, 96.0]));
        assert_eq!(cast.actor(1).unwrap().triangle(), Some(0));
        assert!(cast.busy(), "the last movement step does not yet release the script");
        cast.advance(1.0);
        assert_eq!(cast.actor(1).unwrap().triangle(), Some(1));
        assert!(cast.busy(), "the original completion update precedes the script's next pass");
        cast.advance(1.0);
        assert!(!cast.busy());
    }

    #[test]
    fn malformed_or_unresolved_jumps_are_not_replaced_with_linear_motion() {
        for (frames, z) in [(0, Some(0)), (2, Some(0)), (32770, Some(0)), (14, None)] {
            let mut cast = Cast::new([1]);
            cast.apply(&place(1, 0, 0), 0.0, &flat);
            cast.apply(&Stage::Move { entity: 1, from: None, x: 40, y: 20, z, triangle: Some(0), frames,
                how: MoveHow::Jump, animation: None }, 0.0, &flat);
            cast.advance(100.0);
            assert_eq!(cast.actor(1).unwrap().pos(), Some([0.0, 0.0, 7.0]));
            assert_eq!(cast.counts().moves_skipped, 1);
            assert!(!cast.busy());
        }
    }

    #[test]
    fn a_late_jump_and_small_ticks_share_the_same_arc_and_keep_the_landing_remainder() {
        let jump = Stage::Move { entity: 1, from: Some((9, -105)), x: 9, y: -247, z: Some(0), triangle: Some(1),
            frames: 14, how: MoveHow::Jump, animation: None };
        for elapsed in [0.0, 5.5, 12.0, 13.0, 14.0, 30.0] {
            let mut live = Cast::new([1]);
            live.apply(&Stage::Place { entity: 1, x: 9, y: -105, z: Some(0), triangle: Some(0) }, 0.0, &flat);
            let mut late = live.clone();
            live.apply(&jump, 0.0, &flat);
            for _ in 0..(elapsed * 2.0) as usize { live.advance(0.5); }
            late.apply(&jump, elapsed, &flat);
            assert_eq!(live.actor(1).unwrap().pos(), late.actor(1).unwrap().pos());
            assert_eq!(live.actor(1).unwrap().triangle(), late.actor(1).unwrap().triangle());
            if elapsed >= 12.0 {
                assert_eq!(late.actor(1).unwrap().pos(), Some([9.0, -246.99902, 0.0]), "no endpoint resnap");
            }
        }
    }

    #[test]
    fn an_authored_ladder_face_cancels_an_old_turn_and_survives_arrival() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 10, -20), 0.0, &flat);
        cast.apply_loop(1, 0, "idle", 0.0, false);
        cast.apply(&Stage::Face { entity: 1, heading: 1.0, frames: 30 }, 0.0, &flat);
        cast.advance(6.0);
        assert!((cast.actor(1).unwrap().heading() - 0.2).abs() < 1e-5);
        cast.apply(&Stage::Face { entity: 1, heading: 1.5, frames: 0 }, 0.0, &flat);
        cast.apply(&Stage::Move { entity: 1, from: None, x: 30, y: 20, z: Some(87), triangle: Some(11),
            frames: 20, how: MoveHow::Ladder, animation: None }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().heading(), 1.5);
        cast.advance(10.0);
        let actor = cast.actor(1).unwrap();
        assert_eq!((actor.pos(), actor.heading()), (Some([20.0, 0.0, 47.0]), 1.5));
        assert!(cast.busy() && !actor.moving(), "ladder travel keeps its existing non-walking fallback");
        assert_eq!((actor.animation(), actor.walked_frames()), (None, 0.0));
        cast.advance(10.0);
        let actor = cast.actor(1).unwrap();
        assert_eq!((actor.pos(), actor.heading()), (Some([30.0, 20.0, 87.0]), 1.5));
        assert!(!cast.busy(), "the old turn was cancelled, and the ladder still lasts twenty frames");
        assert_eq!(actor.animation(), Some(("idle", ClipFrame::Loop(0.0))));
        cast.advance(40.0);
        assert_eq!(cast.actor(1).unwrap().heading(), 1.5, "the former turn cannot resume after arrival");
    }

    #[test]
    fn a_turn_goes_the_short_way_round_at_once_or_over_its_frames() {
        let mut cast = Cast::new([1]);
        cast.apply(&Stage::Face { entity: 1, heading: 3.0, frames: 0 }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().heading(), 3.0);
        // From 3.0 to -3.0 is a short turn across pi (0.28 radians), not 6 the other way.
        cast.apply(&Stage::Face { entity: 1, heading: -3.0, frames: 10 }, 0.0, &flat);
        cast.advance(5.0);
        let h = cast.actor(1).unwrap().heading();
        assert!(h.abs() > 3.0, "half way is at the far side of pi: {h}");
        cast.advance(5.0);
        assert!((cast.actor(1).unwrap().heading() + 3.0).abs() < 1e-5);
        assert!(!cast.busy());
        // A heading that is not finite is the field's +x.
        cast.apply(&Stage::Face { entity: 1, heading: f32::NAN, frames: 0 }, 0.0, &flat);
        assert_eq!(cast.actor(1).unwrap().heading(), 0.0);
    }

    #[test]
    fn show_hides_and_shows_and_the_camera_is_recorded_not_shown() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&Stage::Show { entity: 1, visible: false }, 0.0, &flat);
        assert!(!cast.actor(1).unwrap().shown());
        cast.apply(&Stage::Show { entity: 1, visible: true }, 0.0, &flat);
        assert!(cast.actor(1).unwrap().shown());
        assert_eq!(cast.camera(), CameraState::Default);
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 4, y: -5, frames: 12 }), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Scroll { x: 4, y: -5, frames: 12 });
        cast.apply(&Stage::Camera(CameraOp::Follow { entity: 20 }), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Follow { entity: 20 });
        cast.apply(&Stage::Camera(CameraOp::Reset), 0.0, &flat);
        assert_eq!(cast.camera(), CameraState::Default);
        assert_eq!(cast.counts().camera_ops, 3);
    }

    #[test]
    fn the_camera_clock_counts_every_instruction_and_runs_on_scene_time_only() {
        let mut cast = Cast::new([1]);
        assert_eq!(cast.camera_clock(), CameraClock { state: CameraState::Default, events: 0, frames: 0.0 });
        cast.advance(10.0);
        // A scroll that should have started two frames ago is two frames into its run.
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 4, y: -5, frames: 12 }), 2.0, &flat);
        assert_eq!(cast.camera_clock(), CameraClock { state: CameraState::Scroll { x: 4, y: -5, frames: 12 }, events: 1, frames: 2.0 });
        cast.advance(5.0);
        assert_eq!(cast.camera_clock().frames, 7.0);
        // The same scroll again is another instruction: its clock starts again.
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 4, y: -5, frames: 12 }), 0.0, &flat);
        assert_eq!((cast.camera_clock().events, cast.camera_clock().frames), (2, 0.0));
        // Time that is not time moves nothing; the end of the scene gives the camera back.
        cast.advance(f32::NAN);
        cast.advance(-3.0);
        assert_eq!(cast.camera_clock().frames, 0.0);
        cast.advance(4.0);
        cast.clear();
        assert_eq!((cast.camera_clock().state, cast.camera_clock().frames), (CameraState::Default, 0.0));
    }

    #[test]
    fn fade_and_shake_advance_during_dialogue_without_advancing_actors_or_camera() {
        use fmv_ff7::script::scene::{FadeBlend, FadeOp};
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 0, 10), 0.0, &flat);
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 20, y: -10, frames: 12 }), 0.0, &flat);
        cast.apply(&Stage::Fade(FadeOp::Nfade { kind: 11, rgb: [120, 60, 30], frames: 6 }), 0.0, &flat);
        cast.apply(&Stage::Shake { mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2 }, 0.0, &flat);
        let actor = cast.actor(1).unwrap().clone();
        let camera = cast.camera_clock();
        // Two visible field frames pass while a dialogue holds script time.
        for _ in 0..8 { cast.advance_effects(0.25); }
        assert_eq!(cast.fade(), Some(FadeColor { blend: FadeBlend::Add, rgb: [40, 20, 10] }));
        assert_eq!(cast.shake(), [0.0, 1.0]);
        assert_eq!(cast.actor(1), Some(&actor));
        assert_eq!(cast.camera_clock(), camera);
        // A resumed scene frame advances the effects once, as well as its script clocks.
        cast.advance(1.0);
        assert_eq!(cast.fade(), Some(FadeColor { blend: FadeBlend::Add, rgb: [60, 30, 15] }));
        assert_eq!(cast.shake(), [0.0, 3.0], "the third shake frame, not a double tick");
        assert_eq!(cast.actor(1).unwrap().pos(), Some([10.0, 0.0, 7.0]));
        assert_eq!(cast.camera_clock().frames, 1.0);
    }

    #[test]
    fn a_late_fade_catches_up_only_its_own_effect() {
        use fmv_ff7::script::scene::{FadeBlend, FadeOp};
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&Stage::Shake { mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2 }, 0.0, &flat);
        cast.advance_effects(3.0);
        assert_eq!(cast.shake(), [0.0, 3.0]);
        let actor = cast.actor(1).unwrap().clone();
        let camera = cast.camera_clock();
        cast.apply(&Stage::Fade(FadeOp::Nfade { kind: 11, rgb: [120, 60, 30], frames: 6 }), 2.0, &flat);
        assert_eq!(cast.fade(), Some(FadeColor { blend: FadeBlend::Add, rgb: [40, 20, 10] }));
        assert_eq!(cast.shake(), [0.0, 3.0], "the existing shake already reached the visible frame");
        assert_eq!(cast.actor(1), Some(&actor));
        assert_eq!(cast.camera_clock(), camera);
        cast.advance_effects(1.0);
        assert_eq!(cast.fade().unwrap().rgb, [60, 30, 15]);
        assert_eq!(cast.shake(), [0.0, 1.0]);
    }

    #[test]
    fn a_late_shake_catches_up_only_its_own_effect() {
        use fmv_ff7::script::scene::{FadeBlend, FadeOp};
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&Stage::Fade(FadeOp::Nfade { kind: 11, rgb: [120, 60, 30], frames: 6 }), 0.0, &flat);
        cast.advance_effects(2.0);
        let actor = cast.actor(1).unwrap().clone();
        let camera = cast.camera_clock();
        cast.apply(&Stage::Shake { mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2 }, 3.0, &flat);
        assert_eq!(cast.shake(), [0.0, 3.0]);
        assert_eq!(cast.fade(), Some(FadeColor { blend: FadeBlend::Add, rgb: [40, 20, 10] }), "the existing fade must not consume the new shake's overshoot");
        assert_eq!(cast.actor(1), Some(&actor));
        assert_eq!(cast.camera_clock(), camera);
        cast.advance_effects(1.0);
        assert_eq!(cast.fade().unwrap().rgb, [60, 30, 15]);
        assert_eq!(cast.shake(), [0.0, 1.0]);
    }

    #[test]
    fn clearing_the_cast_resets_screen_effects_and_keeps_authored_background_changes() {
        use fmv_ff7::script::scene::FadeOp;
        let mut cast = Cast::new([1]);
        let background = [BackgroundOp::On { group: 8, frame: 2 }, BackgroundOp::Off { group: 8, frame: 1 }];
        for op in background { cast.apply(&Stage::Background(op), 0.0, &flat); }
        cast.apply(&Stage::Camera(CameraOp::Scroll { x: 20, y: -10, frames: 12 }), 0.0, &flat);
        cast.apply(&Stage::Fade(FadeOp::Nfade { kind: 11, rgb: [120, 60, 30], frames: 6 }), 0.0, &flat);
        cast.apply(&Stage::Shake { mask: 2, x_amplitude: 0, x_steps: 0, y_amplitude: 5, y_steps: 2 }, 0.0, &flat);
        cast.advance(3.0);
        assert!(cast.fade().is_some());
        assert_eq!(cast.shake(), [0.0, 3.0]);
        cast.clear();
        assert_eq!(cast.actors().count(), 0);
        assert_eq!((cast.camera_clock().state, cast.camera_clock().frames), (CameraState::Default, 0.0));
        assert_eq!(cast.fade(), None);
        assert_eq!(cast.shake(), [0.0, 0.0]);
        assert_eq!(cast.background_ops(), &background);
        cast.advance_effects(30.0);
        assert_eq!(cast.fade(), None);
        assert_eq!(cast.shake(), [0.0, 0.0], "cleared shake cannot restart its next segment");
        assert_eq!(cast.background_ops(), &background, "field mutations survive scene teardown");
    }

    #[test]
    fn the_clips_are_counted_not_played_and_strangers_are_counted_too() {
        let mut cast = Cast::new([1]);
        let anim = |entity| Stage::Anim { entity, animation: 3, how: AnimHow::Once, first: None, last: None, speed: 16, divisor: 1 };
        cast.apply(&anim(1), 0.0, &flat);
        cast.apply(&anim(1), 0.0, &flat);
        cast.apply(&anim(9), 0.0, &flat);
        cast.apply(&place(9, 0, 0), 0.0, &flat);
        cast.apply(&walk(9, 1, 1, 5), 0.0, &flat);
        cast.apply(&Stage::Face { entity: 9, heading: 1.0, frames: 0 }, 0.0, &flat);
        cast.apply(&Stage::Show { entity: 9, visible: false }, 0.0, &flat);
        assert_eq!(cast.counts(), Counts { anims_skipped: 2, moves_skipped: 0, camera_ops: 0, off_cast: 5 });
        assert!(cast.actor(9).is_none(), "an entity that is not in the cast never gets an actor");
    }

    #[test]
    fn settling_finishes_every_motion_and_clearing_takes_the_cast_away() {
        let mut cast = Cast::new([1, 2]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 90, 90, 600), 0.0, &flat);
        cast.apply(&Stage::Face { entity: 2, heading: 1.0, frames: 300 }, 0.0, &flat);
        cast.settle();
        assert_eq!(cast.actor(1).unwrap().pos(), Some([90.0, 90.0, 7.0]));
        assert!((cast.actor(2).unwrap().heading() - 1.0).abs() < 1e-6);
        assert!(!cast.busy());
        cast.apply(&Stage::Camera(CameraOp::Follow { entity: 1 }), 0.0, &flat);
        cast.clear();
        assert_eq!((cast.actors().count(), cast.camera()), (0, CameraState::Default));
    }

    #[test]
    fn nonsense_numbers_never_leave_the_stage_without_a_finite_place() {
        let mut cast = Cast::new([1]);
        cast.apply(&place(1, 0, 0), 0.0, &flat);
        cast.apply(&walk(1, 100, 100, 10), 0.0, &flat);
        for frames in [f32::NAN, f32::INFINITY, -4.0, 0.0, 1e30] {
            cast.advance(frames);
            let p = cast.actor(1).unwrap().pos().unwrap();
            assert!(p.iter().all(|c| c.is_finite()), "{frames}: {p:?}");
        }
    }
}
