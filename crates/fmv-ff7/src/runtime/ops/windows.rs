//! Window variants and their contents: `WCLS` (0x2E), `WSIZW`, `WSPCL`, `WNUMB`, `STTIM`, `MPARA`, `MPRA2`, `WMOVE`, `WREST`, `WROW`, `GWCOL`, `SWCOL`.
//!
//! Behaviour follows the field-script documentation (FFRTT/Qhimm wiki, Makou Reactor) and a reading of the PSX
//! decompilation for behaviour only; no code is copied. Each instruction's operand bytes are addressed as the
//! original does (see [`FieldRuntime::p8`]).
//!
//! What lives here besides the instructions:
//!
//! * the per-window contents the host needs to draw a window (`MPARA`/`MPRA2` substitutions, the `WNUMB` number,
//!   the `WSPCL` display mode);
//! * the savemap's two clocks, the play time and the countdown timer (`STTIM`), which the original advances every
//!   frame and mirrors into bytes 16-23 of bank 1 for scripts to read;
//! * the menu window colours (`GWCOL`/`SWCOL`), which are save data the host owns.

use super::super::memory::{base_of, is_word_selector};
use super::super::{Event, FieldRuntime, Flow, Window, WINDOWS};

/// Replacement parameters per window (`MPARA` / `MPRA2` slots).
pub const PARAMS_PER_WINDOW: usize = 8;

/// A window's default geometry after a reset (field entry and `WREST`).
const DEFAULT_X: i16 = 8;
const DEFAULT_Y: i16 = 149;
const DEFAULT_Y_WINDOW_1: i16 = 8;
const DEFAULT_WIDTH: u16 = 304;
const DEFAULT_HEIGHT: u16 = 73;
/// Digits a `WNUMB` number shows until the script says otherwise.
const DEFAULT_NUMBER_LENGTH: u8 = 6;

/// The first `MENU` colour set the game starts with: four corners (top-left, top-right, bottom-left,
/// bottom-right) of red, green, blue.
pub const DEFAULT_MENU_COLORS: [u8; 12] = [0, 88, 176, 0, 0, 128, 0, 0, 80, 0, 0, 32];

/// Savemap clock step per 60 Hz vblank, in 1/65536 of a second; the field runs at 30 frames a second, so two
/// steps per frame.
const CLOCK_STEP: u32 = 1092;
const CLOCKS_PER_FRAME: u32 = 2;

/// Bank 1 addresses the field engine keeps current: the play time (hours, minutes, seconds, a sub-second counter)
/// and the countdown timer (the same four).
const PLAY_TIME_BYTES: usize = 16;
const COUNTDOWN_BYTES: usize = 20;
/// Bank 1 byte 95, bit 1 (0x02): the countdown counts up instead of down.
const COUNT_UP_BYTE: usize = 95;
const COUNT_UP_BIT: u8 = 0x02;

/// One `MPARA`/`MPRA2` substitution: where the value is read from when the text is drawn. The original stores the
/// place, not the value, so the text always shows the variable's value at the moment it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaceParam {
    /// The bank selector (0 = the address is the value itself; see [`crate::runtime::memory`]).
    pub bank: u8,
    pub addr: u16,
}

/// What `WSPCL` asks a window to show beside its text.
pub mod number_kind {
    pub const OFF: u8 = 0;
    /// A clock (the countdown timer).
    pub const CLOCK: u8 = 1;
    /// A number (`WNUMB`'s value).
    pub const NUMERICAL: u8 = 2;
}

/// A window's number/clock display (`WSPCL`, `WNUMB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberDisplay {
    /// A [`number_kind`] (other values are kept as the script gave them).
    pub kind: u8,
    /// Position inside the window, in pixels.
    pub x: u8,
    pub y: u8,
    /// The number `WNUMB` set (clocks ignore it).
    pub value: i32,
    /// Digits to show.
    pub length: u8,
}

impl Default for NumberDisplay {
    fn default() -> Self {
        NumberDisplay { kind: number_kind::OFF, x: 0, y: 0, value: 0, length: DEFAULT_NUMBER_LENGTH }
    }
}

/// What a window shows besides its dialog text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowContents {
    pub params: [ReplaceParam; PARAMS_PER_WINDOW],
    pub number: NumberDisplay,
}

/// This family's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsState {
    contents: [WindowContents; WINDOWS],
    menu_colors: [u8; 12],
    /// Play time in seconds and its sub-second accumulator.
    play_seconds: u32,
    play_fraction: u32,
    play_seeded: bool,
    /// Countdown timer in seconds and its accumulator.
    countdown_seconds: u32,
    countdown_fraction: u32,
    countdown_seeded: bool,
}

impl Default for WindowsState {
    fn default() -> Self {
        WindowsState {
            contents: [WindowContents::default(); WINDOWS],
            menu_colors: DEFAULT_MENU_COLORS,
            play_seconds: 0,
            play_fraction: 0,
            play_seeded: false,
            countdown_seconds: 0,
            countdown_fraction: 0,
            countdown_seeded: false,
        }
    }
}

/// Whether this family implements `op`.
pub(crate) fn implemented(op: u8) -> bool {
    matches!(op, 0x2E | 0x2F | 0x36 | 0x37 | 0x38 | 0x41 | 0x42 | 0x51 | 0x53 | 0x55 | 0x56 | 0x57)
}

/// The size a window gets from a size request: kept on screen (the 320x240 display minus an 8 pixel margin), in
/// the original's order, so a window too wide is moved left but never re-clamped on the left.
pub(crate) fn clamp_window(x: i16, y: i16, width: i16, height: i16) -> (i16, i16) {
    let (mut x, mut y) = (i32::from(x), i32::from(y));
    let (w, h) = (i32::from(width), i32::from(height));
    if x < 8 {
        x = 8;
    }
    if x + w > 312 {
        x = 312 - w;
    }
    if y < 8 {
        y = 8;
    }
    if y + h > 224 {
        y = 224 - h;
    }
    (x as i16, y as i16)
}

impl Window {
    /// The window as a field entry or `WREST` leaves it. Ownership (`open`) is not touched.
    fn reset_geometry(&mut self, index: usize) {
        self.x = DEFAULT_X;
        self.y = if index == 1 { DEFAULT_Y_WINDOW_1 } else { DEFAULT_Y };
        self.width = DEFAULT_WIDTH;
        self.height = DEFAULT_HEIGHT;
        self.mode = 0;
        self.prevent_close = false;
    }
}

impl FieldRuntime {
    /// Execute `op` if it belongs to this family.
    pub(in crate::runtime) fn exec_windows(&mut self, op: u8) -> Option<Flow> {
        let flow = match op {
            // WCLS: close the window and wait until it has gone.
            0x2E => {
                let w = self.window_slot(self.p8(1));
                if self.open_owner(w).is_none() {
                    self.advance(2);
                    Flow::Continue
                } else {
                    self.close_by_script(w);
                    Flow::Yield
                }
            }
            // WSIZW: wait until nobody has the window open, then size it.
            0x2F => {
                let w = self.window_slot(self.p8(1));
                match self.open_owner(w) {
                    None => {
                        self.size_window(w, self.p16(2) as i16, self.p16(4) as i16, self.p16(6) as i16, self.p16(8) as i16);
                        self.advance(10);
                        Flow::Continue
                    }
                    Some(owner) => {
                        if owner == self.current {
                            self.close_by_script(w);
                        }
                        Flow::Yield
                    }
                }
            }
            // WSPCL: show a clock or a number inside the window.
            0x36 => {
                let w = self.window_slot(self.p8(1));
                let (kind, x, y) = (self.p8(2), self.p8(3), self.p8(4));
                if let Some(c) = self.windows_ext.contents.get_mut(w) {
                    (c.number.kind, c.number.x, c.number.y) = (kind, x, y);
                }
                self.advance(5);
                Flow::Continue
            }
            // WNUMB: the number a numeric window shows. The original ORs the sign-extended low half with the
            // high half shifted up, so a negative low half poisons the high bits; kept as it is.
            0x37 => {
                let w = self.window_slot(self.p8(2));
                let low = i32::from(self.rd16(1, 3));
                let high = i32::from(self.rd16(2, 5)) << 16;
                let length = self.p8(7);
                if let Some(c) = self.windows_ext.contents.get_mut(w) {
                    c.number.value = low | high;
                    c.number.length = length;
                }
                self.advance(8);
                Flow::Continue
            }
            // STTIM: set the countdown timer.
            0x38 => {
                let hours = u32::from(self.rd8(1, 3));
                let minutes = u32::from(self.rd8(2, 4));
                let seconds = u32::from(self.rd8(4, 5));
                self.windows_ext.countdown_seconds = hours * 3600 + minutes * 60 + seconds;
                self.windows_ext.countdown_seeded = true;
                self.advance(6);
                Flow::Continue
            }
            // MPARA / MPRA2: remember where a window's text parameter comes from.
            0x41 | 0x42 => {
                let w = self.window_slot(self.p8(2));
                let slot = usize::from(self.rd8(1, 3));
                let bank = self.nib(2);
                let addr = if op == 0x41 { u16::from(self.p8(4)) } else { self.p16(4) };
                match self.windows_ext.contents.get_mut(w).and_then(|c| c.params.get_mut(slot)) {
                    Some(p) => *p = ReplaceParam { bank, addr },
                    None => self.coverage.anomalies += 1,
                }
                self.advance(if op == 0x41 { 5 } else { 6 });
                Flow::Continue
            }
            // WMOVE: shift a window.
            0x51 => {
                let w = self.window_slot(self.p8(1));
                let (dx, dy) = (self.p16(2) as i16, self.p16(4) as i16);
                if let Some(win) = self.windows.get_mut(w) {
                    win.x = win.x.wrapping_add(dx);
                    win.y = win.y.wrapping_add(dy);
                }
                self.advance(6);
                Flow::Continue
            }
            // WREST: back to the field-entry state.
            0x53 => {
                let w = self.window_slot(self.p8(1));
                self.reset_window(w);
                self.advance(2);
                Flow::Continue
            }
            // WROW: the window's height in text rows.
            0x55 => {
                let w = self.window_slot(self.p8(1));
                let height = (u16::from(self.p8(2)) << 4) | 9;
                if let Some(win) = self.windows.get_mut(w) {
                    win.height = height;
                }
                self.advance(3);
                Flow::Continue
            }
            // GWCOL: read one corner colour of the menu windows into three variables.
            0x56 => {
                let at = usize::from(self.rd8(1, 3)) * 3;
                match self.windows_ext.menu_colors.get(at..at + 3).and_then(|c| match c {
                    [r, g, b] => Some([*r, *g, *b]),
                    _ => None,
                }) {
                    Some([r, g, b]) => {
                        self.wr8(2, 4, r);
                        self.wr8(3, 5, g);
                        self.wr8(4, 6, b);
                    }
                    None => self.coverage.anomalies += 1,
                }
                self.advance(7);
                Flow::Continue
            }
            // SWCOL: set one corner colour of the menu windows.
            0x57 => {
                let at = usize::from(self.rd8(1, 3)) * 3;
                let rgb = [self.rd8(2, 4), self.rd8(3, 5), self.rd8(4, 6)];
                match self.windows_ext.menu_colors.get_mut(at..at + 3) {
                    Some(c) => c.copy_from_slice(&rgb),
                    None => self.coverage.anomalies += 1,
                }
                self.advance(7);
                Flow::Continue
            }
            _ => return None,
        };
        Some(flow)
    }

    /// Per-frame update after every entity's turn: the savemap clocks.
    pub(in crate::runtime) fn update_windows(&mut self) {
        self.seed_clocks();
        let ws = &mut self.windows_ext;
        let count_up = self.mem.banks.get(COUNT_UP_BYTE) & COUNT_UP_BIT != 0;
        for _ in 0..CLOCKS_PER_FRAME {
            ws.play_fraction += CLOCK_STEP;
            if ws.play_fraction >> 16 != 0 {
                ws.play_seconds = ws.play_seconds.saturating_add(1);
                ws.play_fraction &= 0xFFFF;
            }
            ws.countdown_fraction += CLOCK_STEP;
            if ws.countdown_fraction >> 16 != 0 {
                if count_up {
                    ws.countdown_seconds = ws.countdown_seconds.saturating_add(1);
                } else {
                    ws.countdown_seconds = ws.countdown_seconds.saturating_sub(1);
                }
                ws.countdown_fraction &= 0xFFFF;
            }
        }
        self.publish_clocks();
    }

    // ---- window helpers ------------------------------------------------------------------------------------

    /// The window slot a window operand names, the way the core maps it (an out-of-range slot is an anomaly and
    /// wraps).
    fn window_slot(&mut self, operand: u8) -> usize {
        if usize::from(operand) >= WINDOWS {
            self.coverage.anomalies += 1;
        }
        usize::from(operand) % WINDOWS
    }

    /// The entity that has window `w` open, if any.
    fn open_owner(&self, w: usize) -> Option<usize> {
        self.windows.get(w).and_then(|win| win.open.as_ref()).map(|o| o.entity)
    }

    fn size_window(&mut self, w: usize, x: i16, y: i16, width: i16, height: i16) {
        let (x, y) = clamp_window(x, y, width, height);
        if let Some(win) = self.windows.get_mut(w) {
            (win.x, win.y, win.width, win.height) = (x, y, width as u16, height as u16);
        }
    }

    /// Put window `w` and everything its text shows back to the state a field starts in.
    pub(crate) fn reset_window(&mut self, w: usize) {
        if let Some(win) = self.windows.get_mut(w) {
            win.reset_geometry(w);
        }
        if let Some(c) = self.windows_ext.contents.get_mut(w) {
            *c = WindowContents::default();
        }
    }

    /// Every window as a field entry leaves them (called when the runtime is created).
    pub(in crate::runtime) fn reset_all_windows(&mut self) {
        for w in 0..WINDOWS {
            self.reset_window(w);
        }
    }

    /// Whether `owner` is sitting on the `MESSAGE`/`ASK` that opened window `w` with `dialog`, so that it will see
    /// the window closed and finish the instruction.
    fn owner_will_finish(&self, owner: usize, w: usize, dialog: u8) -> bool {
        let Some(ent) = self.ents.get(owner) else { return false };
        if ent.broken {
            return false;
        }
        let at = |i: usize| self.code.get(ent.pc + i).copied().unwrap_or(0);
        match at(0) {
            0x40 => usize::from(at(1)) % WINDOWS == w && at(2) == dialog,
            0x48 => usize::from(at(2)) % WINDOWS == w && at(3) == dialog,
            _ => false,
        }
    }

    /// A script closes window `w` (`WCLS`, or `WSIZW` on its own window). When another entity is waiting on the
    /// window's `MESSAGE`/`ASK`, the window is marked closed and that entity finishes its instruction on its next
    /// turn, as in the original; otherwise the window is simply taken down.
    fn close_by_script(&mut self, w: usize) {
        let Some(open) = self.windows.get(w).and_then(|win| win.open.as_ref()) else { return };
        if open.closed {
            return;
        }
        let (owner, dialog) = (open.entity, open.dialog);
        if owner != self.current && self.owner_will_finish(owner, w, dialog) {
            if let Some(open) = self.windows.get_mut(w).and_then(|win| win.open.as_mut()) {
                open.closed = true;
            }
        } else {
            if let Some(win) = self.windows.get_mut(w) {
                win.open = None;
            }
            self.events.push(Event::WindowClosedByScript { window: w as u8 });
        }
    }

    // ---- clocks --------------------------------------------------------------------------------------------

    /// Start the clocks from the persisted bank bytes unless something already set them (a script's `STTIM`, or the
    /// host from the real save), so a countdown that spans field changes keeps running and is never reset by the
    /// first frame's write-back.
    fn seed_clocks(&mut self) {
        let from_bytes = |banks: &super::super::memory::Banks, at: usize| {
            u32::from(banks.get(at)) * 3600 + u32::from(banks.get(at + 1)) * 60 + u32::from(banks.get(at + 2))
        };
        if !self.windows_ext.play_seeded {
            self.windows_ext.play_seconds = from_bytes(&self.mem.banks, PLAY_TIME_BYTES);
            // Byte 3 counts the frames since the second began: restore that much of the second too.
            let frames = u32::from(self.mem.banks.get(PLAY_TIME_BYTES + 3));
            self.windows_ext.play_fraction = (frames * CLOCK_STEP * CLOCKS_PER_FRAME).min(0xFFFF);
            self.windows_ext.play_seeded = true;
        }
        if !self.windows_ext.countdown_seeded {
            self.windows_ext.countdown_seconds = from_bytes(&self.mem.banks, COUNTDOWN_BYTES);
            // Byte 3 starts at 30 when the second changes and counts down by frame.
            let frames = 30u32.saturating_sub(u32::from(self.mem.banks.get(COUNTDOWN_BYTES + 3)));
            self.windows_ext.countdown_fraction = (frames * CLOCK_STEP * CLOCKS_PER_FRAME).min(0xFFFF);
            self.windows_ext.countdown_seeded = true;
        }
    }

    /// Mirror the clocks into bank 1 for the scripts. Byte 3 of each group counts frames within the current second:
    /// the play time's counts up and restarts when the second changes; the countdown's starts at 30 and counts down.
    fn publish_clocks(&mut self) {
        let banks = &mut self.mem.banks;
        let split = |seconds: u32| {
            let hours = (seconds / 3600).min(255) as u8;
            let rest = seconds % 3600;
            (hours, (rest / 60) as u8, (rest % 60) as u8)
        };
        let (h, m, s) = split(self.windows_ext.play_seconds);
        banks.set(PLAY_TIME_BYTES, h);
        banks.set(PLAY_TIME_BYTES + 1, m);
        if banks.get(PLAY_TIME_BYTES + 2) != s {
            banks.set(PLAY_TIME_BYTES + 2, s);
            banks.set(PLAY_TIME_BYTES + 3, 0);
        } else {
            banks.set(PLAY_TIME_BYTES + 3, banks.get(PLAY_TIME_BYTES + 3).wrapping_add(1));
        }
        let (h, m, s) = split(self.windows_ext.countdown_seconds);
        banks.set(COUNTDOWN_BYTES, h);
        banks.set(COUNTDOWN_BYTES + 1, m);
        if banks.get(COUNTDOWN_BYTES + 2) != s {
            banks.set(COUNTDOWN_BYTES + 2, s);
            banks.set(COUNTDOWN_BYTES + 3, 30);
        } else if banks.get(COUNTDOWN_BYTES + 3) != 0 {
            banks.set(COUNTDOWN_BYTES + 3, banks.get(COUNTDOWN_BYTES + 3) - 1);
        }
    }

    // ---- host interface ------------------------------------------------------------------------------------

    /// What window `window` shows besides its dialog text.
    pub fn window_contents(&self, window: usize) -> Option<&WindowContents> {
        self.windows_ext.contents.get(window)
    }

    /// The value of text parameter `index` of window `window` as it reads right now (the original looks the
    /// variable up when it draws the text).
    pub fn window_param_value(&self, window: usize, index: usize) -> u16 {
        let Some(p) = self.windows_ext.contents.get(window).and_then(|c| c.params.get(index)) else {
            return 0;
        };
        self.read_param(*p)
    }

    fn read_param(&self, p: ReplaceParam) -> u16 {
        let addr = usize::from(p.addr);
        match p.bank {
            0 => p.addr,
            5 | 6 => {
                let byte = |i: usize| self.mem.scratch.get(i).copied().unwrap_or(0);
                if p.bank == 6 { u16::from(byte(addr)) | u16::from(byte(addr + 1)) << 8 } else { u16::from(byte(addr)) }
            }
            sel => match base_of(sel) {
                Some(base) => {
                    let lo = u16::from(self.mem.banks.get(base + addr));
                    if is_word_selector(sel) { lo | u16::from(self.mem.banks.get(base + addr + 1)) << 8 } else { lo }
                }
                None => 0,
            },
        }
    }

    /// The countdown timer in seconds.
    pub fn countdown_seconds(&self) -> u32 {
        self.windows_ext.countdown_seconds
    }

    /// The host knows the real countdown (from the save): use it instead of the persisted bank bytes.
    pub fn set_countdown_seconds(&mut self, seconds: u32) {
        self.windows_ext.countdown_seconds = seconds;
        self.windows_ext.countdown_seeded = true;
    }

    /// The play time in seconds.
    pub fn play_time_seconds(&self) -> u32 {
        self.windows_ext.play_seconds
    }

    pub fn set_play_time_seconds(&mut self, seconds: u32) {
        self.windows_ext.play_seconds = seconds;
        self.windows_ext.play_seeded = true;
    }

    /// The menu window colours: four corners of red, green, blue.
    pub fn menu_colors(&self) -> [u8; 12] {
        self.windows_ext.menu_colors
    }

    pub fn set_menu_colors(&mut self, colors: [u8; 12]) {
        self.windows_ext.menu_colors = colors;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Banks, MAIN_PRIORITY};
    use crate::script::synth::{dialog, SynthScript};

    const IDLE: &[u8] = &[0x5F, 0x12, 0x01];

    fn script0(init: &[u8], main: &[u8]) -> (usize, Vec<u8>) {
        let mut v = init.to_vec();
        v.push(0x00);
        v.extend_from_slice(main);
        (0, v)
    }

    fn runtime_with(s: SynthScript, banks: Banks) -> FieldRuntime {
        let mut s = s;
        s.scale = 512;
        let mut rt = FieldRuntime::new(&s.build(), banks).expect("runtime");
        rt.init();
        rt
    }

    fn runtime(s: SynthScript) -> FieldRuntime {
        runtime_with(s, Banks::default())
    }

    fn set_byte(bank: u8, addr: u8, value: u8) -> Vec<u8> {
        vec![0x80, bank << 4, addr, value]
    }

    /// An entity whose main script is `main` followed by a flag write and an idle loop.
    fn flag_after(main: &[u8], flag: u8) -> (usize, Vec<u8>) {
        script0(&[], &[main, &set_byte(1, flag, 1), IDLE].concat())
    }

    #[test]
    fn every_implemented_op_advances_by_the_decoded_length() {
        for op in (0u8..=255).filter(|op| implemented(*op)) {
            // Operands of zeros: window 0, nothing open, bank selectors literal.
            let probe = [op, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let len = crate::script::decode_at(&probe, 0, 0).expect("decodes").len;
            let mut code = vec![op];
            code.resize(len, 0);
            code.extend_from_slice(IDLE);
            let s = SynthScript::default().entity("x", vec![script0(&[], &code)]);
            let mut rt = runtime(s);
            let start = rt.entity_state(0).expect("entity").1;
            rt.tick(None);
            let end = rt.entity_state(0).expect("entity").1;
            // The first instruction ran, then the idle loop's WAIT1 yielded.
            assert_eq!(end, start + len + 1, "op {op:#04x} (length {len})");
            assert!(rt.coverage().unsupported.is_empty());
        }
    }

    #[test]
    fn wcls_on_an_unowned_window_just_continues() {
        let s = SynthScript::default().entity("a", vec![flag_after(&[0x2E, 2], 0x40)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x40), 1);
    }

    #[test]
    fn wcls_lets_the_owners_message_finish_and_then_goes_on() {
        // "talker" holds window 0 open with a message nothing can dismiss; "closer" waits two frames, then WCLS 0.
        let talker = flag_after(&[0x40, 0, 0], 0x50);
        let closer = flag_after(&[&[0x24, 2, 0][..], &[0x2E, 0]].concat(), 0x51);
        let mut s = SynthScript::default().entity("talker", vec![talker]).entity("closer", vec![closer]);
        s.dialogs.push(dialog("Locked"));
        let mut rt = runtime(s);
        rt.tick(None);
        assert!(rt.window_open());
        let mut owner_done_first = false;
        for _ in 0..8 {
            rt.tick(None);
            let (talker, closer) = (rt.mem.banks.get(0x50), rt.mem.banks.get(0x51));
            assert!(closer == 0 || talker == 1, "WCLS goes on only after the owner's message finished");
            owner_done_first |= talker == 1;
        }
        assert!(owner_done_first);
        assert_eq!(rt.mem.banks.get(0x50), 1, "the owner's MESSAGE completed instead of opening again");
        assert_eq!(rt.mem.banks.get(0x51), 1);
        assert!(!rt.window_open());
        let events = rt.drain_events();
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Window { .. })).count(), 1, "{events:?}");
        assert!(events.iter().any(|e| matches!(e, Event::WindowDone { window: 0 })), "{events:?}");
    }

    #[test]
    fn wcls_takes_down_a_window_nobody_is_waiting_on() {
        // The owner's script moved on (a priority switch left a stale window): the closer must not hang.
        let s = SynthScript::default().entity("closer", vec![flag_after(&[0x2E, 1], 0x52)]);
        let mut rt = runtime(s);
        rt.windows[1].open = Some(crate::runtime::OpenWindow { entity: 0, dialog: 0, ask: None, closed: false });
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x52), 0);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x52), 1);
        assert!(rt.windows()[1].open.is_none());
    }

    #[test]
    fn wsizw_waits_for_a_foreign_window_and_then_clamps() {
        // WSIZW 0: x=0 (too far left), y=200, width 100, height 60 (too low): clamped to x=8, y=164.
        let size = [0x2F, 0, 0, 0, 200, 0, 100, 0, 60, 0];
        let talker = flag_after(&[0x40, 0, 0], 0x50);
        let sizer = flag_after(&size, 0x53);
        let mut s = SynthScript::default().entity("talker", vec![talker]).entity("sizer", vec![sizer]);
        s.dialogs.push(dialog("Hi"));
        let mut rt = runtime(s);
        for _ in 0..4 {
            rt.tick(None);
        }
        assert_eq!(rt.mem.banks.get(0x53), 0, "waits while another entity's window is open");
        rt.close_window(0);
        rt.tick(None);
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(0x53), 1);
        let w = &rt.windows()[0];
        assert_eq!((w.x, w.y, w.width, w.height), (8, 164, 100, 60));
    }

    #[test]
    fn size_clamps_follow_the_originals_order() {
        assert_eq!(clamp_window(0, 0, 100, 50), (8, 8));
        assert_eq!(clamp_window(300, 200, 100, 50), (212, 174));
        // Wider than the screen: moved left past the margin, and not re-clamped.
        assert_eq!(clamp_window(10, 10, 400, 20), (-88, 10));
    }

    #[test]
    fn windows_start_in_their_reset_geometry_and_wrest_restores_it() {
        // WINDOW (0x50) 0 to (50,60) 70x80; WMODE 0 style 2 prevent-close; WROW 0 rows=3; WMOVE 0 (+5,-6); WREST 0.
        let mut code = vec![0x50, 0, 50, 0, 60, 0, 70, 0, 80, 0];
        code.extend_from_slice(&[0x52, 0, 2, 1]);
        code.extend_from_slice(&[0x55, 0, 3]);
        code.extend_from_slice(&[0x51, 0, 5, 0, 0xFA, 0xFF]);
        let s = SynthScript::default().entity("a", vec![script0(&[], &[&code[..], IDLE].concat())]);
        let mut rt = runtime(s);
        assert_eq!((rt.windows()[0].x, rt.windows()[0].y, rt.windows()[0].width, rt.windows()[0].height), (8, 149, 304, 73));
        assert_eq!(rt.windows()[1].y, 8);
        rt.tick(None);
        let w = &rt.windows()[0];
        assert_eq!((w.x, w.y, w.width, w.height, w.mode, w.prevent_close), (55, 54, 70, 57, 2, true));
        // Now reset it from a second runtime step: WREST 0 then idle.
        let s = SynthScript::default().entity("a", vec![script0(&[], &[&code[..], &[0x53, 0], IDLE].concat())]);
        let mut rt = runtime(s);
        rt.tick(None);
        let w = &rt.windows()[0];
        assert_eq!((w.x, w.y, w.width, w.height, w.mode, w.prevent_close), (8, 149, 304, 73, 0, false));
    }

    #[test]
    fn mpara_stores_the_place_so_text_reads_the_variable_when_drawn() {
        // MPARA [banks: 0 | 1][window 2][slot 3][addr 0x20], and MPRA2 [banks 0|2][window 2][slot 4][addr 0x0021 u16]
        let main = [&[0x41, 0x01, 2, 3, 0x20][..], &[0x42, 0x02, 2, 4, 0x21, 0x00], &[0x42, 0x00, 2, 5, 0x34, 0x12], IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]);
        let mut rt = runtime(s);
        rt.mem.banks.set(0x20, 7);
        rt.tick(None);
        assert_eq!(rt.window_param_value(2, 3), 7);
        rt.mem.banks.set(0x20, 99);
        rt.mem.banks.set(0x21, 0x34);
        rt.mem.banks.set(0x22, 0x12);
        assert_eq!(rt.window_param_value(2, 3), 99, "read at draw time, not at MPARA");
        assert_eq!(rt.window_param_value(2, 4), 0x1234, "a word selector reads two bytes");
        assert_eq!(rt.window_param_value(2, 5), 0x1234, "bank 0 is the literal");
        assert_eq!(rt.window_param_value(0, 3), 0);
        assert_eq!(rt.window_param_value(9, 9), 0);
        assert_eq!(rt.coverage().anomalies, 0);
    }

    #[test]
    fn mpara_out_of_range_slot_is_counted_not_fatal() {
        let main = [&[0x41, 0x01, 0, 9, 0x20][..], IDLE].concat();
        let s = SynthScript::default().entity("a", vec![script0(&[], &main)]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.coverage().anomalies, 1);
    }

    #[test]
    fn wnumb_and_wspcl_set_the_number_display() {
        // WSPCL window 1 numerical at (4, 6); WNUMB window 1 value 0x0001_0002 (two literal words), 5 digits.
        let code = [0x36, 1, 2, 4, 6, 0x37, 0x00, 1, 2, 0, 1, 0, 5];
        let s = SynthScript::default().entity("a", vec![script0(&[], &[&code[..], IDLE].concat())]);
        let mut rt = runtime(s);
        rt.tick(None);
        let n = rt.window_contents(1).expect("window").number;
        assert_eq!(n, NumberDisplay { kind: number_kind::NUMERICAL, x: 4, y: 6, value: 0x0001_0002, length: 5 });
        assert_eq!(rt.window_contents(0).expect("window").number, NumberDisplay::default());
    }

    #[test]
    fn sttim_counts_down_and_mirrors_into_bank_one() {
        // STTIM 0:1:30 -> 90 seconds.
        let code = [0x38, 0x00, 0x00, 0, 1, 30];
        let s = SynthScript::default().entity("a", vec![script0(&[], &[&code[..], IDLE].concat())]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(rt.countdown_seconds(), 90);
        assert_eq!((rt.mem.banks.get(20), rt.mem.banks.get(21), rt.mem.banks.get(22)), (0, 1, 30));
        // The first whole second takes 30 frames (60 vblanks).
        let mut frames = 1;
        while rt.countdown_seconds() == 90 && frames < 100 {
            rt.tick(None);
            frames += 1;
        }
        assert!((29..=31).contains(&frames), "a second is about 30 frames, took {frames}");
        assert_eq!(rt.countdown_seconds(), 89);
        assert_eq!((rt.mem.banks.get(21), rt.mem.banks.get(22)), (1, 29));
        assert_eq!(rt.mem.banks.get(23), 30, "the sub-second counter restarts at 30");
        rt.tick(None);
        assert_eq!(rt.mem.banks.get(23), 29);
        // It stops at zero and never wraps.
        rt.set_countdown_seconds(0);
        for _ in 0..90 {
            rt.tick(None);
        }
        assert_eq!(rt.countdown_seconds(), 0);
    }

    #[test]
    fn the_countdown_counts_up_when_the_script_flag_says_so() {
        let s = SynthScript::default().entity("a", vec![script0(&[], IDLE)]);
        let mut rt = runtime(s);
        rt.mem.banks.set(95, 2);
        rt.set_countdown_seconds(5);
        for _ in 0..61 {
            rt.tick(None);
        }
        assert!(rt.countdown_seconds() >= 7, "{}", rt.countdown_seconds());
    }

    #[test]
    fn a_new_runtime_keeps_the_countdown_and_play_time_the_persisted_bytes_hold() {
        let mut banks = Banks::default();
        for (i, v) in [1u8, 2, 3, 0, 0, 4, 5, 20].into_iter().enumerate() {
            banks.set(16 + i, v);
        }
        let s = SynthScript::default().entity("a", vec![script0(&[], IDLE)]);
        let mut rt = runtime_with(s, banks);
        rt.tick(None);
        assert_eq!(rt.play_time_seconds(), 3600 + 120 + 3);
        assert_eq!(rt.countdown_seconds(), 4 * 60 + 5, "seeded from the bytes, not reset to zero");
        assert_eq!((rt.mem.banks.get(20), rt.mem.banks.get(21), rt.mem.banks.get(22)), (0, 4, 5));
        assert_eq!(rt.mem.banks.get(23), 19, "the saved sub-second counter went on counting down");
    }


    #[test]
    fn menu_colours_round_trip_through_the_scripts() {
        // SWCOL corner 2 = (10, 20, 30) from literals; GWCOL corner 2 into bank 1 0x60..0x62; GWCOL corner 7 is out of
        // range (an anomaly, nothing written).
        let code = [0x57, 0x00, 0x00, 2, 10, 20, 30, 0x56, 0x01, 0x11, 2, 0x60, 0x61, 0x62, 0x56, 0x01, 0x11, 7, 0x70, 0x71, 0x72];
        let s = SynthScript::default().entity("a", vec![script0(&[], &[&code[..], IDLE].concat())]);
        let mut rt = runtime(s);
        rt.tick(None);
        assert_eq!(&rt.menu_colors()[6..9], &[10, 20, 30]);
        assert_eq!((rt.mem.banks.get(0x60), rt.mem.banks.get(0x61), rt.mem.banks.get(0x62)), (10, 20, 30));
        assert_eq!(rt.mem.banks.get(0x70), 0);
        assert_eq!(rt.coverage().anomalies, 1);
        assert_eq!(rt.menu_colors()[0..3], DEFAULT_MENU_COLORS[0..3]);
        assert_eq!(rt.entity_state(0).map(|(p, _)| p), Some(MAIN_PRIORITY));
    }
}
