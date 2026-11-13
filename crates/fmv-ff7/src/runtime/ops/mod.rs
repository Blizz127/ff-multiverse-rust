//! The instructions. Operand bytes are addressed as the original addresses them: byte 0 is the opcode, byte `i`
//! the `i`-th byte after it; a banked operand names the nibble that selects its bank (see [`FieldRuntime::nib`]).

mod anim;
mod motion;
mod party;
mod stage;
mod windows;

pub use anim::AnimState;
pub use motion::MotionState;
pub use party::{CharacterVitals, PartySaveData, PartyState};
pub use stage::StageState;
pub use windows::WindowsState;
pub use windows::{number_kind, NumberDisplay, ReplaceParam, WindowContents, DEFAULT_MENU_COLORS};

use super::{Ask, Command, CommandState, Event, FieldRuntime, Flow, Line, MoveMode, Model, OpenWindow, Sync, MAIN_PRIORITY, NO_CHARACTER};
use crate::script::decode_at;
use crate::script::mnemonic;
use crate::script::scene::presentation::shake::RANDOM;

/// Menu command numbers (`MENU`'s second operand).
pub mod menu {
    pub const CHAR_NAME_ENTRY: u8 = 6;
    pub const PARTY_SELECT: u8 = 7;
    pub const SHOP: u8 = 8;
    pub const PARTY_MENU: u8 = 9;
    pub const SAVE_SCREEN: u8 = 14;
}

/// Whether the runtime implements `op` (every other opcode is skipped and counted as unsupported).
pub fn is_implemented(op: u8) -> bool {
    motion::implemented(op) || anim::implemented(op) || stage::implemented(op) || windows::implemented(op) || party::implemented(op) || core_implemented(op)
}

fn core_implemented(op: u8) -> bool {
    matches!(op, 0x00 | 0x01..=0x03 | 0x04..=0x06 | 0x07 | 0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x16..=0x19 | 0x24 | 0x5F | 0x30..=0x32 | 0x80 | 0x81 | 0x82 | 0x83 | 0x84 | 0x85 | 0x86 | 0x87 | 0x88 | 0x89 | 0x8A | 0x8B | 0x8C | 0x8D | 0x8E | 0x8F | 0x90 | 0x91 | 0x92 | 0x93 | 0x94 | 0x76 | 0x77 | 0x78 | 0x79 | 0x95 | 0x96 | 0x97 | 0x98 | 0x7A | 0x7B | 0x7C | 0x7D | 0x99 | 0x7F | 0x9A | 0x9B | 0x9C | 0x40 | 0x48 | 0x50 | 0x52 | 0x54 | 0x43 | 0xA1 | 0xA0 | 0xBF | 0xA4 | 0x7E | 0xC7 | 0xA5 | 0xA6 | 0xA7 | 0xB3 | 0xC5 | 0xC6 | 0xD6 | 0xD7 | 0xB2 | 0xD0 | 0xD1 | 0xD3 | 0x33 | 0x4A | 0x71 | 0x72 | 0x4B | 0xD2 | 0xF8 | 0x60 | 0x70 | 0x20 | 0x49 | 0xF9 | 0xFF | 0x0E | 0xC8 | 0xC9 | 0xCA | 0x0A | 0x0B | 0x74 | 0xCB | 0x39 | 0x3A | 0xF0..=0xF7 | 0xFC..=0xFE | 0xDA)
}

impl FieldRuntime {
    /// Execute one instruction of the current entity.
    pub(super) fn exec(&mut self, op: u8) -> Flow {
        let name = mnemonic(op).unwrap_or("?");
        let family = if let Some(f) = self.exec_motion(op) {
            Some(f)
        } else if let Some(f) = self.exec_anim(op) {
            Some(f)
        } else if let Some(f) = self.exec_stage(op) {
            Some(f)
        } else if let Some(f) = self.exec_windows(op) {
            Some(f)
        } else {
            self.exec_party(op)
        };
        if let Some(flow) = family {
            if flow != Flow::Broken {
                *self.coverage.executed.entry(name).or_default() += 1;
            }
            return flow;
        }
        let flow = match op {
            // ---- script control ----
            0x00 => self.ret(),
            0x01..=0x03 => {
                let target = self.p8(1);
                let (prio, script) = (self.p8(2) >> 5, self.p8(2) & 0x1F);
                self.req(op, target, prio, script)
            }
            0x04..=0x06 => {
                let slot = usize::from(self.p8(1)).min(2);
                let ch = self.party()[slot];
                let target = if ch == NO_CHARACTER { 0xFF } else { self.char_entity.get(usize::from(ch)).copied().flatten().map_or(0xFF, |e| e as u8) };
                let (prio, script) = (self.p8(2) >> 5, self.p8(2) & 0x1F);
                self.req(op - 3, target, prio, script)
            }
            0x07 => self.retto(),
            0x10 => self.jump_forward(usize::from(self.p8(1)) + 1),
            0x11 => self.jump_forward(usize::from(self.p16(1)) + 1),
            0x12 => self.jump_back(usize::from(self.p8(1))),
            0x13 => self.jump_back(usize::from(self.p16(1))),
            0x14 | 0x15 => {
                let a = self.rd8(1, 2);
                let b = self.rd8(2, 3);
                let truth = compare(self.p8(4), i32::from(a), i32::from(b), 8);
                if op == 0x14 { self.branch(truth, 6, usize::from(self.p8(5)) + 5) } else { self.branch(truth, 7, (self.p16(5) as i16 as isize + 5) as usize) }
            }
            0x16..=0x19 => {
                let (a, b) = (self.rd16(1, 2), self.rd16(2, 4));
                let signed = op <= 0x17;
                let (a, b) = if signed { (i32::from(a), i32::from(b)) } else { (i32::from(a as u16), i32::from(b as u16)) };
                let truth = compare(self.p8(6), a, b, 16);
                if op == 0x16 || op == 0x18 { self.branch(truth, 8, usize::from(self.p8(7)) + 7) } else { self.branch(truth, 9, (self.p16(7) as i16 as isize + 7) as usize) }
            }
            0x24 => self.wait(),
            0x5F => {
                self.advance(1);
                Flow::Yield
            }
            // ---- keys ----
            0x30..=0x32 => {
                let p = self.player.unwrap_or_default();
                let keys = match op {
                    0x30 => p.keys_held,
                    0x31 => p.keys_pressed,
                    _ => self.previous_player.map(|q| q.keys_held & !p.keys_held).unwrap_or(0),
                };
                let truth = keys & self.p16(1) != 0;
                self.branch(truth, 4, usize::from(self.p8(3)) + 3)
            }
            // ---- arithmetic ----
            0x80 => self.set8(|_, b| b),
            0x81 => self.set16(|_, b| b),
            0x82 => self.set8(|a, b| a | 1u8.wrapping_shl(u32::from(b))),
            0x83 => self.set8(|a, b| a & !1u8.wrapping_shl(u32::from(b))),
            0x84 => self.set8(|a, b| a ^ 1u8.wrapping_shl(u32::from(b))),
            0x85 => self.set8(|a, b| a.wrapping_add(b)),
            0x86 => self.set16(|a, b| a.wrapping_add(b)),
            0x87 => self.set8(|a, b| a.wrapping_sub(b)),
            0x88 => self.set16(|a, b| a.wrapping_sub(b)),
            0x89 => self.set8(|a, b| a.wrapping_mul(b)),
            0x8A => self.set16(|a, b| a.wrapping_mul(b)),
            0x8B => self.div8(|a, b| a / b),
            0x8C => self.div16(|a, b| a.wrapping_div(b)),
            0x8D => self.div8(|a, b| a % b),
            0x8E => self.div16(|a, b| a.wrapping_rem(b)),
            0x8F => self.set8(|a, b| a & b),
            0x90 => self.set16(|a, b| a & b),
            0x91 => self.set8(|a, b| a | b),
            0x92 => self.set16(|a, b| a | b),
            0x93 => self.set8(|a, b| a ^ b),
            0x94 => self.set16(|a, b| a ^ b),
            0x76 => self.set8(|a, b| a.saturating_add(b)),
            0x77 => self.set16(|a, b| (i32::from(a) + i32::from(b)).min(32767) as i16),
            0x78 => self.set8(|a, b| a.saturating_sub(b)),
            // MINUS2! and DEC2! do not occur in the owned data; the documented saturation at -32768 is used.
            0x79 => self.set16(|a, b| (i32::from(a) - i32::from(b)).max(-32768) as i16),
            0x95 => self.unary8(|a| a.wrapping_add(1)),
            0x96 => self.unary16(|a| a.wrapping_add(1)),
            0x97 => self.unary8(|a| a.wrapping_sub(1)),
            0x98 => self.unary16(|a| a.wrapping_sub(1)),
            0x7A => self.unary8(|a| a.saturating_add(1)),
            0x7B => self.unary16(|a| (i32::from(a) + 1).min(32767) as i16),
            0x7C => self.unary8(|a| a.saturating_sub(1)),
            0x7D => self.unary16(|a| (i32::from(a) - 1).max(-32768) as i16),
            0x99 => {
                self.rand_index = self.rand_index.wrapping_add(self.rand_step);
                let v = RANDOM[usize::from(self.rand_index)];
                self.wr8(2, 2, v);
                self.advance(3);
                Flow::Continue
            }
            0x7F => {
                self.rand_step = (self.rd8(2, 2) << 4).wrapping_add(1);
                self.advance(3);
                Flow::Continue
            }
            0x9A => {
                let v = self.rd8(2, 3);
                self.wr8(1, 2, v);
                self.advance(4);
                Flow::Continue
            }
            0x9B => {
                let v = (self.rd16(2, 3) >> 8) as u8;
                self.wr8(1, 2, v);
                self.advance(5);
                Flow::Continue
            }
            0x9C => {
                let v = i16::from(self.rd8(2, 4)) | (i16::from(self.rd8(4, 5)) << 8);
                self.wr16(1, 3, v);
                self.advance(6);
                Flow::Continue
            }
            // ---- windows ----
            0x40 => self.message(),
            0x48 => self.ask(),
            0x50 => {
                let w = usize::from(self.p8(1)) % super::WINDOWS;
                let (x, y, wd, h) = (self.p16(2) as i16, self.p16(4) as i16, self.p16(6), self.p16(8));
                let win = &mut self.windows[w];
                win.x = x;
                win.y = y;
                win.width = wd;
                win.height = h;
                self.advance(10);
                Flow::Continue
            }
            0x52 => {
                let w = usize::from(self.p8(1)) % super::WINDOWS;
                self.windows[w].mode = self.p8(2);
                // (bit 0: the player cannot close it; the script closes it itself)
                self.windows[w].prevent_close = self.p8(3) & 1 != 0;
                self.advance(4);
                Flow::Continue
            }
            0x54 => {
                let w = usize::from(self.p8(1));
                // WCLSE only requests closing. Opening must finish first; the owner performs the shrink polls.
                if !self.request_window_close(w) { return Flow::Yield; }
                self.advance(2);
                Flow::Continue
            }
            0x43 => {
                self.state.map_name = Some(self.p8(1));
                self.advance(2);
                Flow::Continue
            }
            // ---- models ----
            0xA1 => {
                let idx = self.models.len();
                let e = self.current;
                let char_id = self.p8(1);
                let scale = u32::from(self.scale);
                self.models.push(Model {
                    entity: e,
                    char_id,
                    x: 0,
                    y: 0,
                    z: 0,
                    triangle: 0,
                    dir: 0,
                    visible: true,
                    solid_off: false,
                    talk_off: false,
                    talk_range: (80 * scale / 512) as u16,
                    solid_range: (30 * scale / 512) as u16,
                    move_speed: (1024 * scale / 512) as u16,
                    move_mode: MoveMode::None,
                    move_to: None,
                    anim: 0,
                    anim_speed: 16,
                    anim_status: Default::default(),
                    request_talk: false,
                    request_push: false,
                });
                self.ents[e].model = Some(idx);
                self.advance(2);
                Flow::Continue
            }
            0xA0 => {
                let ch = self.p8(1);
                let e = self.current;
                if let Some(slot) = self.char_entity.get_mut(usize::from(ch)) {
                    *slot = Some(e);
                }
                let party = self.party();
                let model = self.ents[e].model;
                match party.iter().position(|c| *c == ch) {
                    Some(0) => self.state.pc_model = model,
                    _ => {
                        if let Some(m) = self.own_model() {
                            m.visible = false;
                            m.solid_off = true;
                            m.talk_off = true;
                        }
                    }
                }
                self.advance(2);
                Flow::Continue
            }
            0xBF => {
                let target = usize::from(self.p8(1));
                if let Some(m) = self.model_of(target) {
                    self.state.pc_model = Some(m);
                }
                self.advance(2);
                Flow::Continue
            }
            0xA4 => self.model_flag(|m, v| m.visible = v != 0),
            0x7E => self.model_flag(|m, v| m.talk_off = v != 0),
            0xC7 => self.model_flag(|m, v| m.solid_off = v != 0),
            0xA5 => {
                let (x, y, z, t) = (self.rd16(1, 3), self.rd16(2, 5), self.rd16(3, 7), self.rd16(4, 9));
                if let Some(m) = self.own_model() {
                    (m.x, m.y, m.z, m.triangle) = (i32::from(x), i32::from(y), i32::from(z), t as u16);
                }
                self.advance(11);
                Flow::Yield
            }
            0xA6 => {
                let (x, y, t) = (self.rd16(1, 3), self.rd16(2, 5), self.rd16(3, 7));
                if let Some(m) = self.own_model() {
                    (m.x, m.y, m.triangle) = (i32::from(x), i32::from(y), t as u16);
                }
                self.advance(9);
                Flow::Yield
            }
            0xA7 => {
                let (x, y, z) = (self.rd16(1, 3), self.rd16(2, 5), self.rd16(3, 7));
                if let Some(m) = self.own_model() {
                    (m.x, m.y, m.z) = (i32::from(x), i32::from(y), i32::from(z));
                }
                self.advance(9);
                Flow::Yield
            }
            0xB3 => {
                let d = self.rd8(2, 2);
                if let Some(m) = self.own_model() {
                    m.dir = d;
                }
                self.advance(3);
                Flow::Continue
            }
            0xC5 => {
                let r = self.rd8(2, 2);
                if let Some(m) = self.own_model() {
                    m.talk_range = u16::from(r);
                }
                self.advance(3);
                Flow::Continue
            }
            0xC6 => {
                let r = self.rd8(2, 2);
                if let Some(m) = self.own_model() {
                    m.solid_range = u16::from(r);
                }
                self.advance(3);
                Flow::Continue
            }
            0xD6 => {
                let r = self.rd16(2, 2);
                if let Some(m) = self.own_model() {
                    m.talk_range = r as u16;
                }
                self.advance(4);
                Flow::Continue
            }
            0xD7 => {
                let r = self.rd16(2, 2);
                if let Some(m) = self.own_model() {
                    m.solid_range = r as u16;
                }
                self.advance(4);
                Flow::Continue
            }
            0xB2 => {
                let s = self.rd16(2, 2);
                let scale = i32::from(self.scale);
                if let Some(m) = self.own_model() {
                    m.move_speed = (i32::from(s) * scale / 512) as u16;
                }
                self.advance(4);
                Flow::Continue
            }
            // ---- lines ----
            0xD0 => {
                if self.lines.len() < 32 {
                    let e = self.current;
                    let rd = |s: &Self, i: usize| s.p16(i) as i16;
                    let line = Line {
                        entity: e,
                        a: (rd(self, 1), rd(self, 3), rd(self, 5)),
                        b: (rd(self, 7), rd(self, 9), rd(self, 11)),
                        active: true,
                        touching: false,
                        request_talk: false,
                        request_push: false,
                        across: false,
                        touch: false,
                        touch_on: false,
                        touch_off: false,
                    };
                    self.ents[e].line = Some(self.lines.len());
                    self.lines.push(line);
                }
                self.advance(13);
                Flow::Continue
            }
            0xD1 => {
                let on = self.p8(1) != 0;
                if let Some(l) = self.ents[self.current].line.and_then(|l| self.lines.get_mut(l)) {
                    l.active = on;
                    if !on {
                        l.touch = false;
                    }
                }
                self.advance(2);
                Flow::Continue
            }
            0xD3 => {
                let a = (self.rd16(1, 4), self.rd16(2, 6), self.rd16(3, 8));
                let b = (self.rd16(4, 10), self.rd16(5, 12), self.rd16(6, 14));
                if let Some(l) = self.ents[self.current].line.and_then(|l| self.lines.get_mut(l)) {
                    l.a = a;
                    l.b = b;
                }
                self.advance(16);
                Flow::Continue
            }
            // ---- field state ----
            0x33 => {
                let locked = self.p8(1) != 0;
                if self.state.character_lock != locked {
                    self.events.push(Event::Control { locked });
                }
                self.state.character_lock = locked;
                self.advance(2);
                Flow::Continue
            }
            0x4A => {
                self.state.menu_disabled = self.p8(1) != 0;
                self.advance(2);
                Flow::Continue
            }
            0x71 => {
                self.state.battles_disabled = self.p8(1) != 0;
                self.advance(2);
                Flow::Continue
            }
            0x72 => {
                self.state.battle_mode = self.p16(1);
                self.advance(3);
                Flow::Continue
            }
            0x4B => {
                self.state.encounter_table = self.p8(1);
                self.advance(2);
                Flow::Continue
            }
            0xD2 => {
                self.state.map_jumps_disabled = self.p8(1) != 0;
                self.advance(2);
                Flow::Continue
            }
            0xF8 => {
                self.state.movie = Some(u16::from(self.p8(1)));
                self.advance(2);
                Flow::Yield
            }
            // ---- global commands ----
            0x60 => {
                let c = Command::MapJump { field: self.p16(1), x: self.p16(3) as i16, y: self.p16(5) as i16, triangle: self.p16(7) as i16, dir: self.p8(9) };
                self.command_op(c, 10)
            }
            0x70 => {
                let formation = self.rd16(2, 2) as u16;
                self.command_op(Command::Battle { formation }, 4)
            }
            0x20 => {
                let c = Command::Minigame {
                    field: self.p16(1),
                    x: self.p16(3) as i16,
                    y: self.p16(5) as i16,
                    triangle: self.p16(7) as i16,
                    dir: self.p8(9),
                    game: self.p8(10),
                    param: self.p8(11),
                };
                self.command_op(c, 11)
            }
            0x49 => {
                let kind = self.p8(2);
                let param = self.rd8(2, 3);
                self.command_op(Command::Menu { kind, param }, 4)
            }
            0xF9 => {
                let movie = self.state.movie.unwrap_or(0);
                self.command_op(Command::Movie { movie }, 1)
            }
            0xFF => {
                if !matches!(self.command, CommandState::Pending(Command::GameOver)) {
                    self.command = CommandState::Pending(Command::GameOver);
                    self.events.push(Event::Command(Command::GameOver));
                }
                Flow::Yield
            }
            0x0E => {
                let disc = self.p8(1);
                self.command_op(Command::DiscChange { disc }, 2)
            }
            // ---- party ----
            // PRTYP / PRTYM / PRTYE: the original's PartyAdd / PartyRemove / PartyReplace (`script::party`), so PRTYE's
            // 254 stores an empty 255 and 255 keeps the member already there. An operand the original would reject
            // leaves the party as it is.
            0xC8 | 0xC9 | 0xCA => {
                let len = if op == 0xCA { 4 } else { 2 };
                let change = match op {
                    0xC8 => crate::story::PartyOp::Add(self.p8(1)),
                    0xC9 => crate::story::PartyOp::Remove(self.p8(1)),
                    _ => crate::story::PartyOp::Set([self.p8(1), self.p8(2), self.p8(3)]),
                };
                let party = self.party();
                if let Ok(next) = crate::script::party::native_transition(party, party, change) {
                    self.set_party(next.bank);
                }
                self.advance(len);
                Flow::Continue
            }
            0x0A => {
                let party = [self.rd8(1, 3), self.rd8(2, 4), self.rd8(3, 5)];
                self.set_party(party);
                self.advance(6);
                Flow::Continue
            }
            0x0B => {
                let party = self.party();
                self.wr8(1, 3, party[0]);
                self.wr8(2, 4, party[1]);
                self.wr8(3, 5, party[2]);
                self.advance(6);
                Flow::Continue
            }
            0x74 => {
                let slot = usize::from(self.p8(2));
                if slot < 3 {
                    let ch = self.party()[slot];
                    self.wr8(2, 3, ch);
                }
                self.advance(4);
                Flow::Continue
            }
            0xCB => {
                let truth = self.party().contains(&self.p8(1));
                self.branch(truth, 3, usize::from(self.p8(2)) + 2)
            }
            // ---- gil and items ----
            0x39 | 0x3A => {
                let lo = self.rd16(1, 2) as u16;
                let hi = self.rd16(2, 4) as u16;
                let amount = u32::from(lo) | (u32::from(hi) << 16);
                let delta = if op == 0x39 { i64::from(amount) } else { -i64::from(amount) };
                if let Some(g) = self.gil.as_mut() {
                    *g = if op == 0x39 { g.saturating_add(amount) } else { g.saturating_sub(amount) };
                }
                self.events.push(Event::Gil { delta });
                self.advance(6);
                Flow::Continue
            }
            // ---- presentation the host may show; the field logic does not depend on it ----
            0xF0..=0xF7 | 0xFC..=0xFE | 0xDA => {
                let len = self.len_of();
                let bytes = self.code.get(self.pc()..self.pc() + len).map(<[u8]>::to_vec).unwrap_or_default();
                self.events.push(Event::Presentation { entity: self.current, mnemonic: name, bytes });
                self.advance(len);
                Flow::Continue
            }
            _ => {
                let len = self.len_of();
                if len == 0 {
                    self.coverage.undecodable += 1;
                    return Flow::Broken;
                }
                *self.coverage.unsupported.entry(name).or_default() += 1;
                self.advance(len);
                return Flow::Continue;
            }
        };
        *self.coverage.executed.entry(name).or_default() += 1;
        flow
    }

    /// Every family's per-frame update, after all entities had their turn.
    pub(super) fn update_families(&mut self) {
        self.update_motion();
        self.update_anim();
        self.update_stage();
        self.update_windows();
        self.update_party();
    }

    /// The length of the current instruction (0 if it cannot be decoded).
    fn len_of(&self) -> usize {
        decode_at(&self.code, self.pc(), 0).map(|i| i.len).unwrap_or(0)
    }

    fn branch(&mut self, truth: bool, taken: usize, skip: usize) -> Flow {
        self.advance(if truth { taken } else { skip });
        Flow::Continue
    }

    fn jump_forward(&mut self, by: usize) -> Flow {
        self.advance(by);
        Flow::Continue
    }

    fn jump_back(&mut self, by: usize) -> Flow {
        let e = self.current;
        self.ents[e].pc = self.ents[e].pc.saturating_sub(by);
        Flow::Yield
    }

    fn wait(&mut self) -> Flow {
        let e = self.current;
        if self.ents[e].wait == 0 {
            let n = self.p16(1);
            self.ents[e].wait = n;
            if n == 0 {
                self.advance(3);
            }
            return Flow::Yield;
        }
        if self.ents[e].wait == 1 {
            self.ents[e].wait = 0;
            self.advance(3);
            return Flow::Continue;
        }
        self.ents[e].wait -= 1;
        Flow::Yield
    }

    fn set8(&mut self, f: impl Fn(u8, u8) -> u8) -> Flow {
        let v = f(self.rd8(1, 2), self.rd8(2, 3));
        self.wr8(1, 2, v);
        self.advance(4);
        Flow::Continue
    }

    fn set16(&mut self, f: impl Fn(i16, i16) -> i16) -> Flow {
        let v = f(self.rd16(1, 2), self.rd16(2, 3));
        self.wr16(1, 2, v);
        self.advance(5);
        Flow::Continue
    }

    fn div8(&mut self, f: impl Fn(u8, u8) -> u8) -> Flow {
        let (a, b) = (self.rd8(1, 2), self.rd8(2, 3));
        if b == 0 {
            self.coverage.anomalies += 1;
        } else {
            self.wr8(1, 2, f(a, b));
        }
        self.advance(4);
        Flow::Continue
    }

    fn div16(&mut self, f: impl Fn(i16, i16) -> i16) -> Flow {
        let (a, b) = (self.rd16(1, 2), self.rd16(2, 3));
        if b == 0 {
            self.coverage.anomalies += 1;
        } else {
            self.wr16(1, 2, f(a, b));
        }
        self.advance(5);
        Flow::Continue
    }

    fn unary8(&mut self, f: impl Fn(u8) -> u8) -> Flow {
        let v = f(self.rd8(2, 2));
        self.wr8(2, 2, v);
        self.advance(3);
        Flow::Continue
    }

    fn unary16(&mut self, f: impl Fn(i16) -> i16) -> Flow {
        let v = f(self.rd16(2, 2));
        self.wr16(2, 2, v);
        self.advance(3);
        Flow::Continue
    }

    fn model_flag(&mut self, f: impl Fn(&mut Model, u8)) -> Flow {
        let v = self.p8(1);
        if let Some(m) = self.own_model() {
            f(m, v);
        }
        self.advance(2);
        Flow::Continue
    }

    /// Issue `command` (first visit), wait for it, and continue past the instruction once the host completed it.
    fn command_op(&mut self, command: Command, len: usize) -> Flow {
        match &self.command {
            CommandState::Idle => {
                self.events.push(Event::Command(command.clone()));
                self.command = CommandState::Pending(command);
                Flow::Yield
            }
            CommandState::Done(done) if *done == command => {
                self.command = CommandState::Idle;
                self.advance(len);
                Flow::Continue
            }
            _ => Flow::Yield,
        }
    }

    fn message(&mut self) -> Flow {
        let w = usize::from(self.p8(1)) % super::WINDOWS;
        let dialog = self.p8(2);
        let e = self.current;
        match &self.windows[w].open {
            None => {
                let offset = self.pc();
                self.windows[w].open = Some(OpenWindow::new(e, dialog, None, offset, self.windows[w].width, self.windows[w].height));
                Flow::Yield
            }
            Some(open) if open.entity == e && open.dialog == dialog => {
                if !self.poll_window(w) { return Flow::Yield; }
                self.windows[w].open = None;
                self.events.push(Event::WindowDone { window: w as u8 });
                self.advance(3);
                Flow::Continue
            }
            Some(_) => Flow::Yield,
        }
    }

    fn ask(&mut self) -> Flow {
        let w = usize::from(self.p8(2)) % super::WINDOWS;
        let (dialog, first, last) = (self.p8(3), self.p8(4), self.p8(5));
        let var = (self.nib(2), self.p8(6));
        let e = self.current;
        match &self.windows[w].open {
            None => {
                let ask = Ask { first, last, var, answer: None };
                let offset = self.pc();
                self.windows[w].open = Some(OpenWindow::new(e, dialog, Some(ask), offset, self.windows[w].width, self.windows[w].height));
                Flow::Yield
            }
            Some(open) if open.entity == e && open.dialog == dialog => {
                if !self.poll_window(w) { return Flow::Yield; }
                // The original stores the selected LINE of the message (`OpcodeFuncAsk`'s selectedLine; Braver writes the
                // same), so the first choice is line `first`, not 0: 369 of the Disc 1 questions whose choices start
                // below line 0 compare against those line numbers (mrkt3's fatman1 tests 1 and 2 for lines 1 and 2).
                let answer = self.windows[w].open.as_ref().and_then(|open| open.ask).and_then(|ask| ask.answer).unwrap_or(0);
                self.windows[w].open = None;
                self.wr8(2, 6, first.saturating_add(answer));
                self.events.push(Event::WindowDone { window: w as u8 });
                self.advance(7);
                Flow::Continue
            }
            Some(_) => Flow::Yield,
        }
    }

    // ---- requests -----------------------------------------------------------------------------------------

    /// `REQ` (kind 1), `REQSW` (2), `REQEW` (3).
    fn req(&mut self, opcode: u8, target: u8, priority: u8, script: u8) -> Flow {
        let kind = opcode;
        let me = self.current;
        let t = usize::from(target);
        if target == 0xFF || t >= self.ents.len() {
            self.advance(3);
            return Flow::Continue;
        }
        let p = usize::from(priority);
        if kind == 3 && usize::from(self.ents[t].sync_waiter[p]) == me {
            match self.ents[t].sync[p] {
                Sync::Waiting => return Flow::Yield,
                Sync::Done => {
                    self.ents[t].sync[p] = Sync::None;
                    self.ents[t].sync_waiter[p] = u8::MAX;
                    self.advance(3);
                    return Flow::Continue;
                }
                Sync::None => {}
            }
        }
        let target_priority = self.ents[t].priority;
        if target_priority == priority {
            if kind == 1 {
                self.advance(3);
                return Flow::Continue;
            }
            return Flow::Yield;
        }
        let entry = usize::from(self.ents[t].entries[usize::from(script)]);
        if target_priority < priority {
            // Busy with something more important: queue the request in its priority's slot, if that is free.
            if self.ents[t].saved_pc[p] != 0 {
                if kind == 1 {
                    self.advance(3);
                    return Flow::Continue;
                }
                return Flow::Yield;
            }
            self.ents[t].saved_pc[p] = entry;
            return self.after_request(kind, t, p, me);
        }
        if self.ents[t].sync[p] == Sync::None {
            let target_ent = &mut self.ents[t];
            let active = usize::from(target_ent.priority);
            target_ent.saved_pc[active] = target_ent.pc;
            target_ent.pc = entry;
            target_ent.priority = priority;
            target_ent.wait = 0;
            if let Some(m) = target_ent.model.and_then(|m| self.models.get_mut(m)) {
                m.move_mode = MoveMode::None;
                m.move_to = None;
            }
            return self.after_request(kind, t, p, me);
        }
        Flow::Yield
    }

    fn after_request(&mut self, kind: u8, target: usize, priority: usize, me: usize) -> Flow {
        if kind == 3 {
            self.ents[target].sync_waiter[priority] = me as u8;
            self.ents[target].sync[priority] = Sync::Waiting;
            return Flow::Yield;
        }
        self.advance(3);
        Flow::Continue
    }

    fn ret(&mut self) -> Flow {
        let e = self.current;
        let ent = &mut self.ents[e];
        if ent.priority >= MAIN_PRIORITY {
            return Flow::Yield;
        }
        let p = usize::from(ent.priority);
        if ent.sync[p] == Sync::Waiting {
            ent.sync[p] = Sync::Done;
        }
        ent.priority += 1;
        ent.pc = ent.saved_pc[usize::from(ent.priority)];
        while ent.pc == 0 && ent.priority < MAIN_PRIORITY {
            ent.priority += 1;
            ent.pc = ent.saved_pc[usize::from(ent.priority)];
        }
        let p = usize::from(ent.priority);
        ent.saved_pc[p] = 0;
        if ent.pc == 0 {
            // Nothing to resume, not even a main script (one that was never entered): stay quiescent.
            ent.broken = true;
            return Flow::Broken;
        }
        Flow::Continue
    }

    fn retto(&mut self) -> Flow {
        let e = self.current;
        let priority = self.p8(1) >> 5;
        let script = usize::from(self.p8(1) & 0x1F);
        let ent = &mut self.ents[e];
        while ent.priority < priority.saturating_sub(1) && ent.priority < MAIN_PRIORITY {
            let p = usize::from(ent.priority);
            if ent.sync[p] == Sync::Waiting {
                ent.sync[p] = Sync::Done;
            }
            ent.priority += 1;
            let p = usize::from(ent.priority);
            ent.saved_pc[p] = 0;
        }
        ent.pc = usize::from(ent.entries[script]);
        ent.priority = priority;
        Flow::Continue
    }
}

/// The comparison operators of the `IF` family, as the existing owned-data-verified interpreter evaluates them:
/// the bitwise operators are true when the full-width result is non-zero, and a bit number outside the operand's
/// width (8 or 16) is never set.
fn compare(op: u8, a: i32, b: i32, bits: u32) -> bool {
    let bit = || u32::try_from(b).ok().filter(|&n| n < bits).is_some_and(|n| (a >> n) & 1 == 1);
    match op {
        0 => a == b,
        1 => a != b,
        2 => a > b,
        3 => a < b,
        4 => a >= b,
        5 => a <= b,
        6 => a & b != 0,
        7 => a ^ b != 0,
        8 => a | b != 0,
        9 => bit(),
        10 => !bit(),
        _ => false,
    }
}
