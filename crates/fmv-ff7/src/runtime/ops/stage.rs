//! Background layers, palettes, camera, fades, shake and background movies: `BGON`, `BGOFF`, `BGROL`, `BGROL2`, `BGCLR`, `BGSCR`, `BGPDH`, `STPAL`, `LDPAL`, `CPPAL`, `RTPAL`, `ADPAL`, `MPPAL`, `STPLS`, `LDPLS`, `CPPAL2`, `RTPAL2`, `ADPAL2`, `MPPAL2`, `SCRLO`, `SCRLC`, `SCRLA`, `SCR2D`, `SCRCC`, `SCR2DC`, `SCRLW`, `SCR2DL`, `SCRLP`, `VWOFT`, `FADE`, `FADEW`, `NFADE`, `SHAKE`, `MVIEF`, `MVCAM`, `BGMOVIE`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).
//!
//! # What is reused
//!
//! The scene interpreter's owned-data-verified pieces do the arithmetic: [`BackgroundState`] holds the 64 group
//! masks, [`FadeState`] the one global fade, [`ShakeState`] the two shake axes and [`PaletteOp`] describes the
//! palette work it already knows. This file only reads operands, keeps the camera and view-offset machines (the
//! scene interpreter has no frame-accurate camera) and decides what blocks.
//!
//! # Naming
//!
//! The opcode table names `0xDF` `MPPAL` and `0xEA` `MPPAL2`; the instruction with the extra `start` operand is
//! `0xDF` (11 bytes). Everything here is keyed by opcode byte, so only the doc comments are affected.
//!
//! # What the host reads
//!
//! * [`FieldRuntime::stage_background`]: group masks (which background tiles show);
//! * [`FieldRuntime::drain_palette_commands`]: palette work in script order (see [`PaletteCommand`]);
//! * [`FieldRuntime::stage_fade`] / [`FieldRuntime::stage_fade_color`]: the global fade;
//! * [`FieldRuntime::stage_camera`]: the camera scroll, with [`FieldRuntime::set_camera_follow_target`] as the
//!   host's side of entity-following scrolls;
//! * [`FieldRuntime::stage_shake_offset`], [`FieldRuntime::stage_view_offset`], [`FieldRuntime::stage_layer`] and
//!   the movie flags;
//! * [`FieldRuntime::set_movie_frame`]: the host's side of `MVIEF`.

use super::super::{FieldRuntime, Flow, NO_CHARACTER};
use crate::background::{BackgroundOp, BackgroundState, PaletteOp};
use crate::script::scene::presentation::shake::ease;
use crate::script::scene::presentation::ShakeState;
use crate::script::scene::{FadeColor, FadeOp, FadeState};

/// Colours in the field's palette scratch buffer (64 rows of 16).
const SCRATCH_COLOURS: usize = 1024;
/// Palette commands kept for the host before new ones are dropped (a host that never drains cannot grow it).
const PALETTE_LOG_LIMIT: usize = 8192;

/// Camera scroll modes (`SCRLC`'s mode byte, and what the other scroll instructions set).
pub mod scroll_mode {
    pub const OFF: u8 = 0;
    pub const ENTITY_INSTANT: u8 = 1;
    pub const ENTITY_LINEAR: u8 = 2;
    pub const ENTITY_SMOOTH: u8 = 3;
    pub const COORDS_INSTANT: u8 = 4;
    pub const COORDS_LINEAR: u8 = 5;
    pub const COORDS_SMOOTH: u8 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ScrollPhase {
    /// A scroll instruction ran; the field's next background update starts it. A new field starts here.
    #[default]
    Init,
    Active,
    Done,
}

/// How the view offset (`VWOFT`) moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OffsetMode {
    #[default]
    Instant,
    Linear,
    Smooth,
    Done,
    Other,
}

/// Palette work a script asked for, in script order.
///
/// Instructions the scene interpreter's [`PaletteOp`] already describes (positions are 16-colour rows of the
/// field's scratch buffer) become [`PaletteCommand::Op`]. The others, whose spans start at an arbitrary colour or
/// which add or scale by amounts the old op cannot hold, are resolved to **flat scratch indices** (colour numbers
/// into the 1024-colour buffer, `row * 16 + colour`) and need host support: [`PaletteCommand::Store`] and
/// [`PaletteCommand::Load`] with a start colour, [`PaletteCommand::Copy`], [`PaletteCommand::Scale`] and
/// [`PaletteCommand::Add`]. Copies and scales run forwards element by element without a snapshot. `Scale` skips
/// colour 0 and `Add` does not; both turn a nonzero colour that would come out as 0 into `0x8000`, and both keep
/// bit 15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCommand {
    Op(PaletteOp),
    /// Live palette page `palette`, colours `start..start+count`, into scratch `scratch..`.
    Store { palette: u8, start: u8, scratch: u16, count: u16 },
    /// Scratch `scratch..` into live palette page `palette`, colours `start..start+count`.
    Load { scratch: u16, palette: u8, start: u8, count: u16 },
    Copy { from: u16, to: u16, count: u16 },
    /// Per-channel multiply (64 is neutral), as [`PaletteOp::Multiply`] but at arbitrary flat indices.
    Scale { from: u16, to: u16, count: u16, red: u8, green: u8, blue: u8 },
    /// Per-channel signed add, each channel clamped to 0..=31.
    Add { from: u16, to: u16, count: u16, red: i8, green: i8, blue: i8 },
}

/// The camera as the host should draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraView {
    /// The scroll position in field screen units (what the original keeps as the camera scroll).
    pub x: i32,
    pub y: i32,
    /// One of [`scroll_mode`]'s values (other bytes are kept as the script gave them).
    pub mode: u8,
    /// A scroll instruction has taken the camera over from the default follow-the-player camera.
    pub enabled: bool,
    /// The model an entity-following scroll tracks (the host supplies its screen position through
    /// [`FieldRuntime::set_camera_follow_target`]).
    pub follow_model: Option<usize>,
    /// The scroll has finished (what `SCRLW` waits for).
    pub done: bool,
}

/// One scrolling background layer (2 or 3): `BGSCR`'s speed, the accumulated phase, `BGPDH`'s depth.
#[derive(Debug, Clone, Copy, Default)]
struct Layer {
    speed: (i16, i16),
    phase: (i64, i64),
    /// Width and height times 16 of the layer's animation, the phase's wrap (0: none).
    period: (i64, i64),
    depth: i16,
}

/// This family's state.
#[derive(Debug, Clone)]
pub struct StageState {
    background: BackgroundState,
    palette_log: Vec<PaletteCommand>,
    palette_dropped: u64,
    fade: FadeState,
    shake: ShakeState,
    // camera scroll
    scroll_mode: u8,
    scroll_phase: ScrollPhase,
    scroll_enabled: bool,
    scroll_target: (i16, i16),
    scroll_steps: i16,
    scroll_step: i16,
    scroll_start: (i32, i32),
    scroll_pos: (i32, i32),
    scroll_model: Option<usize>,
    follow_target: Option<(i32, i32)>,
    // view offset
    view_offset: i16,
    view_mode: OffsetMode,
    view_start: i16,
    view_target: i16,
    view_steps: i16,
    view_step: i16,
    // layers 2 and 3: scroll speeds, accumulated phases, depths, wrap periods (width*16, height*16; 0 = none)
    layers: [Layer; 2],
    // flags and movie
    background_movie: bool,
    movie_camera_disabled: bool,
    scrlo: u8,
    movie_frame: i16,
}

impl Default for StageState {
    fn default() -> Self {
        Self {
            background: BackgroundState::default(),
            palette_log: Vec::new(),
            palette_dropped: 0,
            fade: FadeState::default(),
            shake: ShakeState::default(),
            scroll_mode: scroll_mode::OFF,
            scroll_phase: ScrollPhase::Init,
            scroll_enabled: false,
            scroll_target: (0, 0),
            scroll_steps: 0,
            scroll_step: 0,
            scroll_start: (0, 0),
            scroll_pos: (0, 0),
            scroll_model: None,
            follow_target: None,
            view_offset: 0,
            view_mode: OffsetMode::Instant,
            view_start: 0,
            view_target: 0,
            view_steps: 0,
            view_step: 0,
            layers: [Layer::default(); 2],
            background_movie: false,
            movie_camera_disabled: false,
            scrlo: 0,
            movie_frame: 0,
        }
    }
}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    matches!(
        op,
        0x25 | 0x27 | 0x2C | 0x2D | 0x5E | 0x61..=0x68 | 0x6A | 0x6B | 0x6C | 0x6F | 0xDF | 0xE0..=0xEF | 0xFA | 0xFB
    )
}

fn span_ok(start: usize, count: usize) -> bool {
    count >= 1 && start.checked_add(count).is_some_and(|end| end <= SCRATCH_COLOURS)
}

fn aligned(a: usize, b: usize, count: usize) -> bool {
    a.is_multiple_of(16) && b.is_multiple_of(16) && (1..=256).contains(&count)
}

fn copy_command(from: usize, to: usize, count: usize) -> PaletteCommand {
    if aligned(from, to, count) {
        PaletteCommand::Op(PaletteOp::Copy { source: (from / 16) as u8, destination: (to / 16) as u8, count: count as u16 })
    } else {
        PaletteCommand::Copy { from: from as u16, to: to as u16, count: count as u16 }
    }
}

fn scale_command(from: usize, to: usize, count: usize, rgb: [u8; 3]) -> PaletteCommand {
    let [red, green, blue] = rgb;
    if aligned(from, to, count) {
        PaletteCommand::Op(PaletteOp::Multiply { source: (from / 16) as u8, destination: (to / 16) as u8, red, green, blue, count: count as u16 })
    } else {
        PaletteCommand::Scale { from: from as u16, to: to as u16, count: count as u16, red, green, blue }
    }
}

fn store_command(palette: u8, start: u8, scratch: usize, count: usize) -> PaletteCommand {
    if start == 0 && scratch.is_multiple_of(16) && count <= 256 {
        PaletteCommand::Op(PaletteOp::Store { palette, position: (scratch / 16) as u8, count: count as u16 })
    } else {
        PaletteCommand::Store { palette, start, scratch: scratch as u16, count: count as u16 }
    }
}

fn load_command(scratch: usize, palette: u8, start: u8, count: usize) -> PaletteCommand {
    if start == 0 && scratch.is_multiple_of(16) && count <= 256 {
        PaletteCommand::Op(PaletteOp::Load { position: (scratch / 16) as u8, palette, count: count as u16 })
    } else {
        PaletteCommand::Load { scratch: scratch as u16, palette, start, count: count as u16 }
    }
}

/// The original's linear interpolation: `start + delta * step / duration`.
fn linear(start: i32, target: i32, duration: i16, step: i16) -> i32 {
    let delta = i64::from(target) - i64::from(start);
    (i64::from(start) + delta * i64::from(step) / i64::from(duration.max(1))) as i32
}

/// The original's eased interpolation (the scene interpreter's shake easing).
fn eased(start: i32, target: i32, duration: i16, step: i16) -> i32 {
    ease(start as i16, target as i16, duration.max(1), step)
}

impl FieldRuntime {
    // ---- host interface ------------------------------------------------------------------------------------

    /// Which background tiles the script shows (group masks).
    /// Show or hide one background frame (the field's own door triggers do; see `FieldRuntime::set_bg_triggers`).
    /// Returns whether it changed.
    pub(in crate::runtime) fn stage_background_set(&mut self, group: u8, frame: u8, on: bool) -> bool {
        self.stage.background.apply(if on { BackgroundOp::On { group, frame } } else { BackgroundOp::Off { group, frame } })
    }

    pub fn stage_background(&self) -> &BackgroundState {
        &self.stage.background
    }

    /// Palette work since the last call, in script order.
    pub fn drain_palette_commands(&mut self) -> Vec<PaletteCommand> {
        std::mem::take(&mut self.stage.palette_log)
    }

    /// Palette commands dropped because the host did not drain the log fast enough.
    pub fn palette_commands_dropped(&self) -> u64 {
        self.stage.palette_dropped
    }

    pub fn stage_fade(&self) -> &FadeState {
        &self.stage.fade
    }

    /// The fade overlay to draw now (`None` when there is none).
    pub fn stage_fade_color(&self) -> Option<FadeColor> {
        self.stage.fade.color()
    }

    pub fn stage_camera(&self) -> CameraView {
        let s = &self.stage;
        CameraView {
            x: s.scroll_pos.0,
            y: s.scroll_pos.1,
            mode: s.scroll_mode,
            enabled: s.scroll_enabled,
            follow_model: s.scroll_model.filter(|_| matches!(s.scroll_mode, 1..=3)),
            done: s.scroll_phase == ScrollPhase::Done,
        }
    }

    /// The host's side of entity-following scrolls (`SCRLA`, `SCRLC`, `SCRLP`): each frame, before
    /// [`FieldRuntime::tick`], give the followed model's scroll position (its screen position, negated and clamped to
    /// the camera range, as the original computes it). Without one, a following scroll keeps its position; its
    /// timing (what `SCRLW` waits for) does not depend on the host.
    pub fn set_camera_follow_target(&mut self, target: Option<(i32, i32)>) {
        self.stage.follow_target = target;
    }

    /// Screen shake in whole field-screen pixels (+x right, +y down), to add after the camera.
    pub fn stage_shake_offset(&self) -> [f32; 2] {
        self.stage.shake.offset()
    }

    /// `VWOFT`'s current vertical view offset (added to the followed model's height).
    pub fn stage_view_offset(&self) -> i16 {
        self.stage.view_offset
    }

    /// Background layer 2 or 3's `(scroll phase x, y, depth)`: the accumulated `BGSCR` scroll (already wrapped when
    /// [`FieldRuntime::set_layer_period`] was given the layer's animation size) and the `BGPDH` depth.
    pub fn stage_layer(&self, layer: u8) -> Option<(i64, i64, i16)> {
        let i = usize::from(layer.checked_sub(2)?);
        let l = self.stage.layers.get(i)?;
        Some((l.phase.0, l.phase.1, l.depth))
    }

    /// Layer 2 or 3's animation size in 1/16 pixels (width and height times 16), so its scroll phase wraps as the
    /// original's does. 0 leaves an axis unwrapped.
    pub fn set_layer_period(&mut self, layer: u8, width16: u32, height16: u32) {
        if let Some(p) = usize::from(layer).checked_sub(2).and_then(|i| self.stage.layers.get_mut(i)) {
            p.period = (i64::from(width16), i64::from(height16));
        }
    }

    pub fn stage_background_movie_enabled(&self) -> bool {
        self.stage.background_movie
    }

    pub fn stage_movie_camera_disabled(&self) -> bool {
        self.stage.movie_camera_disabled
    }

    /// The host's side of `MVIEF`: the frame the playing movie is on (0 when none).
    pub fn set_movie_frame(&mut self, frame: i16) {
        self.stage.movie_frame = frame;
    }

    // ---- instructions --------------------------------------------------------------------------------------

    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_stage(&mut self, op: u8) -> Option<Flow> {
        if !implemented(op) {
            return None;
        }
        Some(match op {
            0xE0..=0xE4 => self.stage_background_op(op),
            0xE5..=0xEF | 0xDF => self.stage_palette_op(op),
            0x25 | 0x6B | 0x6C => self.stage_fade_op(op),
            0x61..=0x68 | 0x6F => self.stage_scroll_op(op),
            0x5E => self.stage_shake_op(),
            0x6A => self.stage_view_offset_op(),
            0x2C | 0x2D => self.stage_layer_op(op),
            0x27 => {
                self.stage.background_movie = self.p8(1) != 0;
                self.advance(2);
                Flow::Continue
            }
            0xFA => {
                let frame = self.stage.movie_frame;
                self.wr16(2, 2, frame);
                self.advance(3);
                Flow::Continue
            }
            0xFB => {
                self.stage.movie_camera_disabled = self.p8(1) != 0;
                self.advance(2);
                Flow::Continue
            }
            _ => Flow::Continue,
        })
    }

    fn stage_background_op(&mut self, op: u8) -> Flow {
        let (group, frame, len) = match op {
            0xE0 | 0xE1 => (self.rd8(1, 2), self.rd8(2, 3), 4),
            _ => (self.rd8(2, 2), 0, 3),
        };
        let change = match op {
            0xE0 => BackgroundOp::On { group, frame },
            0xE1 => BackgroundOp::Off { group, frame },
            0xE2 => BackgroundOp::ShiftLeft { group },
            0xE3 => BackgroundOp::ShiftRight { group },
            _ => BackgroundOp::Clear { group },
        };
        // A group outside the 64 masks or a frame outside the byte reads or writes memory the original does not
        // define: leave the masks alone.
        if group >= 64 || (matches!(op, 0xE0 | 0xE1) && frame >= 8) {
            self.coverage.anomalies += 1;
        } else {
            self.stage.background.apply(change);
        }
        self.advance(len);
        Flow::Continue
    }

    fn push_palette(&mut self, command: PaletteCommand) {
        if self.stage.palette_log.len() >= PALETTE_LOG_LIMIT {
            self.stage.palette_dropped += 1;
        } else {
            self.stage.palette_log.push(command);
        }
    }

    /// Log `commands` if every span lies inside the scratch buffer (else count an anomaly: the original would index
    /// past its buffer).
    fn log_palette(&mut self, spans: &[(usize, usize)], commands: impl FnOnce() -> Vec<PaletteCommand>) {
        if spans.iter().all(|&(start, count)| span_ok(start, count)) {
            for c in commands() {
                self.push_palette(c);
            }
        } else {
            self.coverage.anomalies += 1;
        }
    }

    fn stage_palette_op(&mut self, op: u8) -> Flow {
        let len = match op {
            0xE5 => {
                // STPAL [banks][palette][position][count]
                let (palette, position, count) = (self.rd8(1, 2), usize::from(self.rd8(2, 3)) * 16, usize::from(self.p8(4)) + 1);
                self.log_palette(&[(position, count)], || vec![store_command(palette, 0, position, count)]);
                5
            }
            0xE6 => {
                // LDPAL [banks][position][palette][count]
                let (position, palette, count) = (usize::from(self.rd8(1, 2)) * 16, self.rd8(2, 3), usize::from(self.p8(4)) + 1);
                self.log_palette(&[(position, count)], || vec![load_command(position, palette, 0, count)]);
                5
            }
            0xE7 => {
                // CPPAL [banks][source][destination][count]
                let (from, to, count) = (usize::from(self.rd8(1, 2)) * 16, usize::from(self.rd8(2, 3)) * 16, usize::from(self.p8(4)) + 1);
                self.log_palette(&[(from, count), (to, count)], || vec![copy_command(from, to, count)]);
                5
            }
            0xE8 => {
                // RTPAL [banks][source][destination][rotation][end]: the span rotates, in two forward copies
                let (from, to) = (usize::from(self.rd8(1, 3)) * 16, usize::from(self.rd8(2, 4)) * 16);
                let (rotation, end) = (usize::from(self.rd8(4, 5)), usize::from(self.p8(6)) + 1);
                self.rotate_palette(from, to, 0, rotation, end);
                7
            }
            0xEE => {
                // RTPAL2 [banks][start/rotation banks][source][destination][start][rotation][end]
                let (from, to) = (usize::from(self.p8(3)) * 16, usize::from(self.p8(4)) * 16);
                let (start, rotation) = (usize::from(self.rd8(1, 5)), usize::from(self.rd8(2, 6)));
                let end = usize::from(self.rd8(4, 7)) + 1 + start;
                self.rotate_palette(from, to, start, rotation, end);
                8
            }
            0xE9 | 0xEA | 0xDF | 0xEF => return self.stage_palette_colour_op(op),
            0xEB => {
                // STPLS [palette][position][start][count]
                let (palette, position, start, count) = (self.p8(1), usize::from(self.p8(2)) * 16, self.p8(3), usize::from(self.p8(4)) + 1);
                let at = position + usize::from(start);
                self.log_palette(&[(at, count)], || vec![store_command(palette, start, at, count)]);
                5
            }
            0xEC => {
                // LDPLS [position][palette][start][count]
                let (position, palette, start, count) = (usize::from(self.p8(1)) * 16, self.p8(2), self.p8(3), usize::from(self.p8(4)) + 1);
                let at = position + usize::from(start);
                self.log_palette(&[(at, count)], || vec![load_command(at, palette, start, count)]);
                5
            }
            _ => {
                // 0xED CPPAL2 [banks][banks][source][destination][source start][destination start][count]
                let (src, dst) = (usize::from(self.p8(3)) * 16, usize::from(self.p8(4)) * 16);
                let (from, to) = (src + usize::from(self.rd8(1, 5)), dst + usize::from(self.rd8(2, 6)));
                let count = usize::from(self.rd8(4, 7)) + 1;
                self.log_palette(&[(from, count), (to, count)], || vec![copy_command(from, to, count)]);
                8
            }
        };
        self.advance(len);
        Flow::Continue
    }

    /// `RTPAL` / `RTPAL2`: copy the source from `start` up to the end to the destination from `rotation`, then the
    /// last `rotation + 1` colours up to the end to the destination from `start`.
    fn rotate_palette(&mut self, from: usize, to: usize, start: usize, rotation: usize, end: usize) {
        // A rotation past the end would read before the source (undefined in the original).
        let Some(tail) = end.checked_sub(rotation) else {
            self.coverage.anomalies += 1;
            return;
        };
        let (first_count, tail_count) = (tail + 1, rotation + 1);
        let spans = [(from + start, first_count), (to + rotation, first_count), (from + tail, tail_count), (to + start, tail_count)];
        self.log_palette(&spans, || {
            vec![copy_command(from + start, to + rotation, first_count), copy_command(from + tail, to + start, tail_count)]
        });
    }

    /// `ADPAL`, `MPPAL2` (0xEA), `MPPAL` (0xDF) and `ADPAL2`: per-channel add or scale of a span.
    fn stage_palette_colour_op(&mut self, op: u8) -> Flow {
        // (source row, destination row, start colour, count, [b, g, r] operands, instruction length)
        let (src, dst, start, count, blue, green, red, len) = match op {
            0xE9 | 0xEA => (
                usize::from(self.rd8(1, 4)) * 16,
                usize::from(self.rd8(2, 5)) * 16,
                0,
                usize::from(self.p8(9)) + 1,
                self.rd8(3, 6),
                self.rd8(4, 7),
                self.rd8(5, 8),
                10,
            ),
            _ => (
                usize::from(self.p8(4)) * 16,
                usize::from(self.p8(5)) * 16,
                usize::from(self.rd8(1, 6)),
                usize::from(self.rd8(6, 10)) + 1,
                self.rd8(2, 7),
                self.rd8(3, 8),
                self.rd8(4, 9),
                11,
            ),
        };
        let (from, to) = (src + start, dst + start);
        self.log_palette(&[(from, count), (to, count)], || {
            if matches!(op, 0xE9 | 0xEF) {
                vec![PaletteCommand::Add { from: from as u16, to: to as u16, count: count as u16, red: red as i8, green: green as i8, blue: blue as i8 }]
            } else {
                vec![scale_command(from, to, count, [red, green, blue])]
            }
        });
        self.advance(len);
        Flow::Continue
    }

    fn stage_fade_op(&mut self, op: u8) -> Flow {
        match op {
            0x6B => {
                // FADE [banks][banks][r][g][b][speed][type][adjust]: r, g, b by bank nibbles 1, 2, 4
                let rgb = [self.rd8(1, 3), self.rd8(2, 4), self.rd8(4, 5)];
                let fade = FadeOp::Fade { kind: self.p8(7), rgb, speed: self.p8(6), adjust: self.p8(8) };
                // A system transition (types 3 and 13+) is the engine's own and leaves the script's fade intact.
                self.stage.fade.apply(fade);
                self.advance(9);
                Flow::Continue
            }
            0x25 => {
                let rgb = [self.rd8(1, 4), self.rd8(2, 5), self.rd8(3, 6)];
                let fade = FadeOp::Nfade { kind: self.p8(3), rgb, frames: self.rd16(4, 7) };
                if !self.stage.fade.apply(fade) && matches!(self.p8(3), 11 | 12) {
                    // The original interpolator divides by this duration.
                    self.coverage.anomalies += 1;
                }
                self.advance(9);
                Flow::Continue
            }
            _ => {
                // FADEW waits for the fade to finish; the fade advances once a frame after every script has run.
                if self.stage.fade.complete() {
                    self.advance(1);
                    Flow::Continue
                } else {
                    Flow::Yield
                }
            }
        }
    }

    fn stage_scroll_op(&mut self, op: u8) -> Flow {
        let len = match op {
            0x61 => {
                self.stage.scrlo = self.p8(1);
                2
            }
            0x62 => {
                // SCRLC [banks][steps u16][mode]: scroll to the player's model
                self.stage.scroll_mode = self.p8(4);
                self.stage.scroll_model = self.state.pc_model;
                self.stage.scroll_steps = self.rd16(2, 2);
                self.stage.scroll_phase = ScrollPhase::Init;
                5
            }
            0x63 | 0x6F => {
                // SCRLA [banks][steps u16][entity][mode] / SCRLP [banks][steps u16][party slot][mode]
                let entity = if op == 0x63 {
                    Some(usize::from(self.p8(4)))
                } else {
                    let character = self.party().get(usize::from(self.p8(4))).copied().unwrap_or(NO_CHARACTER);
                    if character == NO_CHARACTER { None } else { self.char_entity.get(usize::from(character)).copied().flatten() }
                };
                if let Some(model) = entity.and_then(|e| self.model_of(e)) {
                    self.stage.scroll_mode = self.p8(5);
                    self.stage.scroll_model = Some(model);
                    self.stage.scroll_steps = self.rd16(2, 2);
                    self.stage.scroll_phase = ScrollPhase::Init;
                }
                6
            }
            0x64 => {
                self.stage.scroll_mode = scroll_mode::COORDS_INSTANT;
                self.stage.scroll_target = (self.rd16(1, 2), self.rd16(2, 4));
                self.stage.scroll_phase = ScrollPhase::Init;
                6
            }
            0x65 => {
                self.stage.scroll_mode = scroll_mode::OFF;
                self.stage.scroll_model = self.state.pc_model;
                self.stage.scroll_phase = ScrollPhase::Init;
                1
            }
            0x66 | 0x68 => {
                self.stage.scroll_mode = if op == 0x66 { scroll_mode::COORDS_SMOOTH } else { scroll_mode::COORDS_LINEAR };
                self.stage.scroll_target = (self.rd16(1, 3), self.rd16(2, 5));
                self.stage.scroll_steps = self.rd16(4, 7);
                self.stage.scroll_phase = ScrollPhase::Init;
                9
            }
            _ => {
                // SCRLW: wait for the scroll to finish, then make a following scroll instant (it keeps following)
                // and a scroll to coordinates instant (it stays where it arrived).
                if self.stage.scroll_phase != ScrollPhase::Done {
                    return Flow::Yield;
                }
                match self.stage.scroll_mode {
                    scroll_mode::ENTITY_INSTANT..=scroll_mode::ENTITY_SMOOTH => self.stage.scroll_mode = scroll_mode::ENTITY_INSTANT,
                    scroll_mode::COORDS_LINEAR | scroll_mode::COORDS_SMOOTH => self.stage.scroll_mode = scroll_mode::COORDS_INSTANT,
                    _ => {}
                }
                self.stage.scroll_phase = ScrollPhase::Init;
                1
            }
        };
        self.advance(len);
        Flow::Continue
    }

    fn stage_shake_op(&mut self) -> Flow {
        let mask = self.p8(3);
        // Only an enabled axis reads its operands.
        let (xa, xs) = if mask & 1 != 0 { (self.rd8(1, 4), self.rd8(2, 5)) } else { (0, 0) };
        let (ya, ys) = if mask & 2 != 0 { (self.rd8(3, 6), self.rd8(4, 7)) } else { (0, 0) };
        if !self.stage.shake.apply(mask & 3, xa, xs, ya, ys) {
            // The easing divides by the segment length.
            self.coverage.anomalies += 1;
        }
        self.advance(8);
        Flow::Continue
    }

    fn stage_view_offset_op(&mut self) -> Flow {
        let (target, steps, mode) = (self.rd16(1, 2), self.rd16(2, 4), self.p8(6));
        let s = &mut self.stage;
        if mode == 0 {
            s.view_offset = target;
            s.view_steps = 0;
            s.view_step = 0;
            s.view_mode = OffsetMode::Instant;
            s.view_start = 0;
            s.view_target = 0;
        } else {
            s.view_start = s.view_offset;
            s.view_target = target;
            s.view_steps = steps;
            s.view_mode = match mode {
                1 => OffsetMode::Linear,
                2 => OffsetMode::Smooth,
                3 => OffsetMode::Done,
                _ => OffsetMode::Other,
            };
            s.view_step = 0;
        }
        self.advance(7);
        Flow::Continue
    }

    fn stage_layer_op(&mut self, op: u8) -> Flow {
        let layer = self.p8(2);
        let (x, y) = (self.rd16(1, 3), if op == 0x2D { self.rd16(2, 5) } else { 0 });
        // The original ignores layers other than 2 and 3.
        if let Some(l) = layer.checked_sub(2).and_then(|i| self.stage.layers.get_mut(usize::from(i))) {
            if op == 0x2D {
                l.speed = (x, y);
            } else {
                l.depth = x;
            }
        }
        self.advance(if op == 0x2D { 7 } else { 5 });
        Flow::Continue
    }

    // ---- per frame -----------------------------------------------------------------------------------------

    /// Per-frame update after every entity's turn: the fade, the camera scroll, the shake, then the view offset and
    /// the layer scrolls (the field loop's own order).
    pub(in crate::runtime) fn update_stage(&mut self) {
        self.stage.fade.advance(1.0);
        self.scroll_init();
        self.scroll_update();
        self.stage.shake.advance(1.0);
        self.view_offset_update();
        self.layer_update();
    }

    fn scroll_init(&mut self) {
        let s = &mut self.stage;
        if s.scroll_phase != ScrollPhase::Init {
            return;
        }
        let begin = |s: &mut StageState, phase: ScrollPhase| {
            s.scroll_enabled = true;
            s.scroll_step = 0;
            s.scroll_start = s.scroll_pos;
            s.scroll_phase = phase;
        };
        match s.scroll_mode {
            scroll_mode::OFF => {
                s.scroll_enabled = false;
                s.scroll_pos = (0, 0);
                s.scroll_phase = ScrollPhase::Done;
            }
            scroll_mode::ENTITY_INSTANT..=scroll_mode::ENTITY_SMOOTH | scroll_mode::COORDS_LINEAR | scroll_mode::COORDS_SMOOTH => {
                begin(s, ScrollPhase::Active);
            }
            scroll_mode::COORDS_INSTANT => {
                s.scroll_enabled = true;
                s.scroll_pos = (i32::from(s.scroll_target.0), i32::from(s.scroll_target.1));
                s.scroll_phase = ScrollPhase::Done;
            }
            // Modes the original's init does not know leave the scroll waiting.
            _ => {}
        }
    }

    fn scroll_update(&mut self) {
        let s = &mut self.stage;
        if s.scroll_phase != ScrollPhase::Active {
            return;
        }
        let mode = s.scroll_mode;
        let coords = (i32::from(s.scroll_target.0), i32::from(s.scroll_target.1));
        let follow = s.follow_target.unwrap_or(s.scroll_pos);
        let target = match mode {
            scroll_mode::ENTITY_INSTANT => {
                if let Some(t) = s.follow_target {
                    s.scroll_pos = t;
                }
                return;
            }
            scroll_mode::ENTITY_LINEAR | scroll_mode::ENTITY_SMOOTH => follow,
            scroll_mode::COORDS_LINEAR | scroll_mode::COORDS_SMOOTH => coords,
            _ => return,
        };
        let smooth = matches!(mode, scroll_mode::ENTITY_SMOOTH | scroll_mode::COORDS_SMOOTH);
        if s.scroll_steps <= 0 {
            // The original divides by the step count here; it arrives at once and is done after this update.
            s.scroll_pos = target;
            s.scroll_phase = ScrollPhase::Done;
            self.coverage.anomalies += 1;
            return;
        }
        let at = |a: i32, b: i32, steps: i16, step: i16| if smooth { eased(a, b, steps, step) } else { linear(a, b, steps, step) };
        // With nothing to follow a following scroll keeps its position but still takes its time.
        let (x, y) = (at(s.scroll_start.0, target.0, s.scroll_steps, s.scroll_step), at(s.scroll_start.1, target.1, s.scroll_steps, s.scroll_step));
        s.scroll_pos = (x, y);
        if s.scroll_step == s.scroll_steps {
            s.scroll_phase = ScrollPhase::Done;
        } else {
            s.scroll_step += 1;
        }
    }

    fn view_offset_update(&mut self) {
        let s = &mut self.stage;
        // The view offset moves only while the default camera follows the player.
        if s.scroll_enabled {
            return;
        }
        let smooth = match s.view_mode {
            OffsetMode::Linear => false,
            OffsetMode::Smooth => true,
            _ => return,
        };
        if s.view_steps <= 0 {
            s.view_offset = s.view_target;
            s.view_mode = OffsetMode::Done;
            self.coverage.anomalies += 1;
            return;
        }
        let (a, b) = (i32::from(s.view_start), i32::from(s.view_target));
        // The original stores the interpolated value through an unsigned byte.
        let v = if smooth { eased(a, b, s.view_steps, s.view_step) } else { linear(a, b, s.view_steps, s.view_step) };
        s.view_offset = i16::from(v as u8);
        if s.view_step == s.view_steps {
            s.view_mode = OffsetMode::Done;
        } else {
            s.view_step += 1;
        }
    }

    fn layer_update(&mut self) {
        let s = &mut self.stage;
        for l in &mut s.layers {
            let (mut nx, mut ny) = (l.phase.0 + i64::from(l.speed.0), l.phase.1 + i64::from(l.speed.1));
            if l.period.0 > 0 {
                nx %= l.period.0;
            }
            if l.period.1 > 0 {
                ny %= l.period.1;
            }
            l.phase = (nx, ny);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Banks;
    use crate::script::synth::SynthScript;
    use crate::script::decode_at;

    const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

    fn runtime(main: &[u8]) -> FieldRuntime {
        let mut main = main.to_vec();
        main.extend_from_slice(IDLE);
        let mut code = vec![0x00];
        code.extend_from_slice(&main);
        let mut s = SynthScript::default().entity("stage", vec![(0, code)]);
        s.scale = 512;
        let mut rt = FieldRuntime::new(&s.build(), Banks::default()).expect("runtime");
        rt.init();
        rt
    }

    fn flag(bank: u8, addr: u8) -> Vec<u8> {
        vec![0x80, bank << 4, addr, 1]
    }

    fn flag_set(rt: &FieldRuntime, addr: u8) -> bool {
        rt.mem.banks.get(usize::from(addr)) == 1
    }

    #[test]
    fn every_instruction_advances_by_its_decoded_length() {
        for op in 0u8..=255 {
            if !implemented(op) || op == 0x67 {
                continue; // SCRLW waits on a fresh field's first update
            }
            let probe = [op, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let len = decode_at(&probe, 0, 0).map(|i| i.len).expect("decodes");
            let mut code = vec![op];
            code.resize(len, 0);
            let mut rt = runtime(&code);
            let pc0 = rt.entity_state(0).unwrap().1;
            rt.tick(None);
            assert_eq!(rt.entity_state(0).unwrap().1, pc0 + len + 1, "op {op:#04x} (length {len}) then the NOP");
            assert!(rt.coverage().unsupported.is_empty(), "op {op:#04x} skipped");
        }
    }

    #[test]
    fn background_masks_follow_bgon_bgoff_bgclr_and_the_shifts() {
        // BGON group 5 frame 2, BGON 5 frame 3, BGOFF 5 2, BGROL 5, then BGON 6 0, BGCLR 6
        let code = [
            0xE0, 0, 5, 2, 0xE0, 0, 5, 3, 0xE1, 0, 5, 2, 0xE2, 0, 5, 0xE0, 0, 6, 0, 0xE4, 0, 6,
        ];
        let mut rt = runtime(&code);
        rt.tick(None);
        let bg = rt.stage_background();
        // 5 had bits {2,3}, lost 2 -> {3}, shifted left -> {4}
        assert!(bg.visible(2, 5, 1 << 4));
        assert!(!bg.visible(2, 5, 1 << 3));
        assert!(!bg.visible(2, 6, 255), "group 6 was cleared");
    }

    #[test]
    fn banked_group_and_frame_operands_are_read_from_memory() {
        // SETBYTE Var[1][0x10] = 9; Var[1][0x11] = 4; BGON group=Var[1][0x10] frame=Var[1][0x11]
        let mut code = vec![0x80, 0x10, 0x10, 9, 0x80, 0x10, 0x11, 4];
        code.extend_from_slice(&[0xE0, 0x11, 0x10, 0x11]);
        let mut rt = runtime(&code);
        rt.tick(None);
        assert!(rt.stage_background().visible(2, 9, 1 << 4));
    }

    #[test]
    fn a_group_outside_the_masks_is_counted_and_changes_nothing() {
        let mut rt = runtime(&[0xE0, 0, 200, 1, 0xE0, 0, 3, 9]);
        rt.tick(None);
        assert_eq!(rt.coverage().anomalies, 2);
        assert_eq!(rt.stage_background(), &BackgroundState::default());
    }

    #[test]
    fn fadew_waits_for_the_fade_and_the_fade_advances_after_the_scripts() {
        // FADE type 2 (field -> colour), speed 100, adjust 0 reaches 255 on the third update; FADEW; flag
        let mut code = vec![0x6B, 0, 0, 10, 20, 30, 100, 2, 0, 0x6C];
        code.extend_from_slice(&flag(1, 0x40));
        let mut rt = runtime(&code);
        for _ in 0..3 {
            rt.tick(None);
            assert!(!flag_set(&rt, 0x40));
        }
        assert!(rt.stage_fade_color().is_some());
        rt.tick(None);
        assert!(flag_set(&rt, 0x40), "released on the visit after the fade finished");
    }

    #[test]
    fn nfade_with_a_duration_interpolates_and_a_zero_duration_is_an_anomaly() {
        // NFADE type 11 to (200,100,50) in 4 frames
        let mut rt = runtime(&[0x25, 0, 0, 11, 200, 100, 50, 4, 0]);
        for _ in 0..4 {
            rt.tick(None);
        }
        assert!(rt.stage_fade().complete());
        assert_eq!(rt.stage_fade_color().map(|c| c.rgb), Some([200, 100, 50]));
        let mut rt = runtime(&[0x25, 0, 0, 11, 1, 1, 1, 0, 0]);
        rt.tick(None);
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn scr2d_arrives_during_the_next_update_and_scrlw_then_goes_on() {
        // SCR2D to (-30, 40); SCRLW; flag
        let mut code = vec![0x64, 0, 0xE2, 0xFF, 40, 0, 0x67];
        code.extend_from_slice(&flag(1, 0x41));
        let mut rt = runtime(&code);
        rt.tick(None);
        assert!(!flag_set(&rt, 0x41), "the scroll is only started by the update after the script");
        let cam = rt.stage_camera();
        assert_eq!((cam.x, cam.y, cam.done, cam.enabled), (-30, 40, true, true));
        rt.tick(None);
        assert!(flag_set(&rt, 0x41));
    }

    #[test]
    fn a_bare_scrlw_on_a_fresh_field_waits_one_update_and_goes_on() {
        let mut code = vec![0x67];
        code.extend_from_slice(&flag(1, 0x42));
        let mut rt = runtime(&code);
        rt.tick(None);
        assert!(!flag_set(&rt, 0x42));
        rt.tick(None);
        assert!(flag_set(&rt, 0x42));
    }

    #[test]
    fn a_linear_scroll_takes_its_steps_and_scrlw_waits_for_all_of_them() {
        // SCR2DL to (100, 0) in 4 steps; SCRLW; flag
        let mut code = vec![0x68, 0, 0, 100, 0, 0, 0, 4, 0, 0x67];
        code.extend_from_slice(&flag(1, 0x43));
        let mut rt = runtime(&code);
        let mut xs = Vec::new();
        let mut released = None;
        for frame in 1..=10 {
            rt.tick(None);
            xs.push(rt.stage_camera().x);
            if flag_set(&rt, 0x43) && released.is_none() {
                released = Some(frame);
            }
        }
        assert_eq!(&xs[..5], &[0, 25, 50, 75, 100]);
        assert_eq!(released, Some(6));
        assert_eq!(rt.stage_camera().mode, scroll_mode::COORDS_INSTANT, "SCRLW makes a finished scroll instant");
    }

    #[test]
    fn a_smooth_scroll_eases_and_ends_on_target() {
        let mut rt = runtime(&[0x66, 0, 0, 0xC8, 0, 0, 0, 8, 0]);
        let mut xs = Vec::new();
        for _ in 0..10 {
            rt.tick(None);
            xs.push(rt.stage_camera().x);
        }
        assert_eq!(xs[0], 0);
        assert_eq!(xs[8], 200);
        assert!(xs[1] < 200 / 8, "eases in");
        assert!(rt.stage_camera().done);
    }

    #[test]
    fn a_scroll_of_zero_steps_arrives_and_is_counted() {
        let mut rt = runtime(&[0x68, 0, 0, 100, 0, 0, 0, 0, 0]);
        rt.tick(None);
        let cam = rt.stage_camera();
        assert_eq!((cam.x, cam.done), (100, true));
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn scrlc_follows_the_player_model_and_finishes_on_its_step_counter_without_a_host_position() {
        // SCRLC steps 3 mode 2 (entity linear); SCRLW; flag
        let mut code = vec![0x62, 0, 3, 0, 2, 0x67];
        code.extend_from_slice(&flag(1, 0x44));
        let mut rt = runtime(&code);
        for _ in 0..8 {
            rt.tick(None);
        }
        assert!(flag_set(&rt, 0x44), "timing does not depend on the host");
        let mut rt2 = runtime(&[0x62, 0, 2, 0, 1]);
        rt2.set_camera_follow_target(Some((7, 9)));
        rt2.tick(None);
        assert_eq!((rt2.stage_camera().x, rt2.stage_camera().y), (7, 9));
        assert!(!rt2.stage_camera().done, "an instant follow never finishes");
    }

    #[test]
    fn scrcc_gives_the_camera_back() {
        let mut rt = runtime(&[0x64, 0, 5, 0, 6, 0, 0x65]);
        rt.tick(None);
        let cam = rt.stage_camera();
        assert_eq!((cam.enabled, cam.x, cam.y, cam.mode), (false, 0, 0, scroll_mode::OFF));
    }

    #[test]
    fn shake_runs_and_settles_and_a_zero_segment_is_counted() {
        // SHAKE x axis only, amplitude 200, 4 steps per segment
        let mut rt = runtime(&[0x5E, 0, 0, 1, 200, 4, 0, 0]);
        let mut moved = false;
        for _ in 0..40 {
            rt.tick(None);
            moved |= rt.stage_shake_offset()[0] != 0.0;
            assert_eq!(rt.stage_shake_offset()[1], 0.0);
        }
        assert!(moved);
        let mut rt = runtime(&[0x5E, 0, 0, 1, 200, 0, 0, 0]);
        rt.tick(None);
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn mvief_writes_the_hosts_movie_frame() {
        // MVIEF [banks: destination bank in nibble 2 = 2 (word view)][address 0x30]
        let mut rt = runtime(&[0xFA, 0x02, 0x30]);
        rt.set_movie_frame(321);
        rt.tick(None);
        assert_eq!(rt.mem.read16(2, 0x30), 321);
    }

    #[test]
    fn movie_flags_and_scrlo_are_kept() {
        let mut rt = runtime(&[0x27, 1, 0xFB, 1, 0x61, 3]);
        rt.tick(None);
        assert!(rt.stage_background_movie_enabled());
        assert!(rt.stage_movie_camera_disabled());
    }

    #[test]
    fn vwoft_moves_the_view_offset_only_while_the_default_camera_follows() {
        // VWOFT target 40 over 4 steps, linear (mode 1); the value goes through a byte, as the original's
        let mut rt = runtime(&[0x6A, 0, 40, 0, 4, 0, 1]);
        let mut seen = Vec::new();
        for _ in 0..6 {
            rt.tick(None);
            seen.push(rt.stage_view_offset());
        }
        assert_eq!(&seen[..5], &[0, 10, 20, 30, 40]);
        // an instant VWOFT sets it at once
        let mut rt = runtime(&[0x6A, 0, 7, 0, 0, 0, 0]);
        rt.tick(None);
        assert_eq!(rt.stage_view_offset(), 7);
        // a scroll takes the camera over: the offset stands still
        let mut rt = runtime(&[0x64, 0, 0, 0, 0, 0, 0x6A, 0, 40, 0, 4, 0, 1]);
        for _ in 0..6 {
            rt.tick(None);
        }
        assert_eq!(rt.stage_view_offset(), 0);
    }

    #[test]
    fn layers_two_and_three_scroll_and_have_a_depth() {
        // BGSCR layer 2 speed (3, -1); BGPDH layer 3 depth 77; BGSCR layer 9 is ignored
        let mut rt = runtime(&[0x2D, 0, 2, 3, 0, 0xFF, 0xFF, 0x2C, 0, 3, 77, 0, 0x2D, 0, 9, 1, 0, 1, 0]);
        for _ in 0..4 {
            rt.tick(None);
        }
        assert_eq!(rt.stage_layer(2), Some((12, -4, 0)));
        assert_eq!(rt.stage_layer(3), Some((0, 0, 77)));
        assert_eq!(rt.stage_layer(4), None);
        rt.set_layer_period(2, 16, 0);
        rt.tick(None);
        assert_eq!(rt.stage_layer(2).map(|l| l.0), Some(15));
    }

    #[test]
    fn palette_instructions_become_commands_in_script_order() {
        let code = [
            0xE5, 0, 2, 3, 15, // STPAL palette 2 -> row 3, 16 colours
            0xE6, 0, 3, 2, 15, // LDPAL row 3 -> palette 2
            0xE7, 0, 1, 2, 7, // CPPAL row 1 -> row 2, 8 colours
            0xEA, 0, 0, 0, 1, 2, 64, 64, 64, 15, // MPPAL2 row 1 -> 2, neutral, 16 colours
        ];
        let mut rt = runtime(&code);
        rt.tick(None);
        let log = rt.drain_palette_commands();
        assert_eq!(
            log,
            vec![
                PaletteCommand::Op(PaletteOp::Store { palette: 2, position: 3, count: 16 }),
                PaletteCommand::Op(PaletteOp::Load { position: 3, palette: 2, count: 16 }),
                PaletteCommand::Op(PaletteOp::Copy { source: 1, destination: 2, count: 8 }),
                PaletteCommand::Op(PaletteOp::Multiply { source: 1, destination: 2, red: 64, green: 64, blue: 64, count: 16 }),
            ]
        );
        assert!(rt.drain_palette_commands().is_empty());
    }

    #[test]
    fn colour_adds_sign_extend_and_spans_with_a_start_colour_use_flat_indices() {
        // ADPAL row 1 -> row 2, blue +5, green -3 (0xFD), red 0, 16 colours
        // ADPAL2 row 1 -> row 2 from colour 4, blue 0, green 0, red -1 (0xFF), 8 colours
        // CPPAL2 row 1 colour 2 -> row 2 colour 3, 4 colours
        // STPLS palette 1 into row 2 from colour 5, 3 colours; LDPLS back
        let code = [
            0xE9, 0, 0, 0, 1, 2, 5, 0xFD, 0, 15, //
            0xEF, 0, 0, 0, 1, 2, 4, 0, 0, 0xFF, 7, //
            0xED, 0, 0, 1, 2, 2, 3, 3, //
            0xEB, 1, 2, 5, 2, //
            0xEC, 2, 1, 5, 2,
        ];
        let mut rt = runtime(&code);
        rt.tick(None);
        let log = rt.drain_palette_commands();
        assert_eq!(
            log,
            vec![
                PaletteCommand::Add { from: 16, to: 32, count: 16, red: 0, green: -3, blue: 5 },
                PaletteCommand::Add { from: 20, to: 36, count: 8, red: -1, green: 0, blue: 0 },
                PaletteCommand::Copy { from: 18, to: 35, count: 4 },
                PaletteCommand::Store { palette: 1, start: 5, scratch: 37, count: 3 },
                PaletteCommand::Load { scratch: 37, palette: 1, start: 5, count: 3 },
            ]
        );
    }

    #[test]
    fn a_rotation_is_two_forward_copies_and_a_span_past_the_buffer_is_counted() {
        // RTPAL row 1 -> row 2, rotate by 2, end operand 7 (end = 8): copies 7 colours then 3
        let mut rt = runtime(&[0xE8, 0, 0, 1, 2, 2, 7]);
        rt.tick(None);
        assert_eq!(
            rt.drain_palette_commands(),
            vec![PaletteCommand::Copy { from: 16, to: 34, count: 7 }, PaletteCommand::Copy { from: 22, to: 32, count: 3 }]
        );
        // CPPAL from row 63 with 17 colours runs off the 1024-colour buffer
        let mut rt = runtime(&[0xE7, 0, 63, 0, 16]);
        rt.tick(None);
        assert!(rt.drain_palette_commands().is_empty());
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn the_palette_log_is_bounded() {
        let mut rt = runtime(&[0xE7, 0, 0, 1, 0]);
        for _ in 0..PALETTE_LOG_LIMIT + 10 {
            rt.push_palette(PaletteCommand::Copy { from: 0, to: 1, count: 1 });
        }
        assert_eq!(rt.stage.palette_log.len(), PALETTE_LOG_LIMIT);
        assert_eq!(rt.palette_commands_dropped(), 10);
    }
}
