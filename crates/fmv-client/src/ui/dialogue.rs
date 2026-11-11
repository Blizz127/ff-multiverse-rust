//! Talking to the characters of a field: the dialogue window, and the keys that open and drive it.
//! The text is the game's own, read from this machine's copy of it, and shown exactly as written.
//!
//! * [`Talk`] is what the window (`crate::window`) holds. A fresh `Enter` or `Space` (a
//!   controller's A is `Enter`) while no battle is on starts a conversation with the character the
//!   player stands near and faces ([`pick_target`]); while a dialogue window is open the same keys
//!   page through it. The player is frozen while it is open ([`Talk::is_open`]).
//! * [`DialogueWindow`] is the window's own state: a source of messages ([`MessageSource`]), the
//!   message on show, its page, and the cursor of a question. A pure state machine: keys in
//!   ([`DialogueKey`]), pictures out ([`DialogueWindow::draw`]).
//! * [`layout`] turns a message into pages and rows that fit the window (the font has the
//!   printable ASCII only, so the game's typographic marks and accents are folded).
//! * [`ff7`] is where the conversations come from in the game: the character's talk script, run
//!   by `fmv_ff7::script::dialogue` over this machine's own field data. Anything else that can give
//!   [`Message`]s can stand in for it ([`Conversations`]): the window never asks where a line came
//!   from, except to draw an added line (not the game's) with a different border.
//!
//! # Keys
//!
//! | Key | Closed | Open |
//! |---|---|---|
//! | `Enter`, `Space`, `NumpadEnter` (pad A) | talk to the character in front of us | next page, next window, or answer the question |
//! | `Backspace` (pad B), `Esc` | (not ours) | close the whole conversation |
//! | `Up`/`W`, `Down`/`S` (stick, D-pad) | (not ours) | move the cursor of a question (wraps) |
//!
//! Only a fresh press of `Enter`, `Space`, `Backspace` and `Esc` counts (a held key must not page
//! through a whole conversation). The arrows repeat, but only a key pressed while the window is
//! open: the player who walked up holding a direction and pressed `Enter` has that key's repeats
//! sent on to the walk, and no question's pointer spins through its answers. Releases are never
//! ours: a movement key let go of during a dialogue still reaches the walk. A battle owns `Enter` while it is on and while its
//! panel lingers, so no conversation starts then, and one that is open when a battle begins is
//! closed.
//!
//! # Who can be talked to
//!
//! [`pick_target`]: someone whose talk radius (the script's `TALKR`, scaled by the field scale)
//! reaches the player, nearest first, and who is within 70 degrees of where the player faces, or
//! so near (under 40% of the radius) that facing does not matter. A character with nothing to say
//! at the story's point (hidden, silenced, a bare `RET`) is not a target's conversation: nothing
//! happens, as in the game.

mod layout;
mod echo_live;
pub mod live;

pub mod ff7;

#[cfg(test)]
mod shots;
#[cfg(test)]
mod tests;

pub use layout::{lay_out, Laid, Page, POINTER_CELLS};

use crate::mods::{ModSink, VoiceCue};
use crate::state::{ClientState, NpcView};
use crate::ui::draw::{DrawList, Rgba};
use crate::ui::font::Special;
use crate::ui::hud::hud_scale;
use crate::ui::widgets::{Canvas, ACCENT, BORDER, DIM, PANEL_BOTTOM, PANEL_TOP, TEXT};
use fmv_core::Pos;
use fmv_ff7::script::dialogue::{Beat, Message, MessageOrigin, MessageSource, TextColour};
use crate::live::{LiveClient, LiveWindowView};
use fmv_protocol::LiveAsk;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use winit::keyboard::KeyCode;

// ---- geometry ----------------------------------------------------------------------------------

/// Canvas units from the edge of the screen to a window.
const MARGIN: i32 = 4;
/// Canvas units between a window's border and its text.
const PAD: i32 = 6;
/// Canvas units from one row of text to the next.
const ROW_H: i32 = 12;
/// The canvas the layout is made for, as the battle's windows have it: one canvas unit is
/// `min(width / 480, height / 280)` pixels.
const FULL_W: i32 = 480;
const FULL_H: i32 = 280;
const MAX_SCALE: i32 = 8;
/// The fewest cells inside a window (a window of one short line is still a window).
const MIN_CELLS: usize = 16;
/// Pixels, at HUD scale 1, kept clear under the window for the HUD's lines at the bottom.
const HUD_CLEARANCE_PX: i32 = 36;
/// How much of the screen's height a window may take (percent).
const MAX_HEIGHT_PERCENT: i32 = 45;

/// Where a window of a screen size goes, in canvas units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Window pixels per canvas unit.
    pub s: i32,
    /// The canvas, in units.
    pub cw: i32,
    pub ch: i32,
    /// The most cells inside a window and the most rows it shows.
    pub max_cells: usize,
    pub max_rows: usize,
    /// The row of the canvas that the window's bottom edge may reach (above the HUD's lines).
    pub floor: i32,
}

/// The geometry for a `vw` x `vh` screen.
pub fn geometry(vw: u32, vh: u32) -> Geometry {
    let (w, h) = (i32::try_from(vw.max(1)).unwrap_or(i32::MAX), i32::try_from(vh.max(1)).unwrap_or(i32::MAX));
    let s = (w / FULL_W).min(h / FULL_H).clamp(1, MAX_SCALE);
    let (cw, ch) = ((w / s).max(1), (h / s).max(1));
    let clearance = (HUD_CLEARANCE_PX * i32::try_from(hud_scale(vw, vh)).unwrap_or(1) + s - 1) / s;
    let max_cells = usize::try_from(((cw - 2 * MARGIN - 2 * PAD) / 8).max(0)).unwrap_or(0).max(POINTER_CELLS + 4);
    let rows = (ch * MAX_HEIGHT_PERCENT / 100 - 2 * PAD + (ROW_H - 8)) / ROW_H;
    let max_rows = usize::try_from(rows.clamp(2, 12)).unwrap_or(2);
    Geometry { s, cw, ch, max_cells, max_rows, floor: (ch - clearance).max(MARGIN + 2 * PAD + ROW_H) }
}

// ---- keys --------------------------------------------------------------------------------------

/// What a key means to a dialogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogueKey {
    Confirm,
    Back,
    Up,
    Down,
}

/// The dialogue key a keyboard event stands for (see the module docs for the table). Only a fresh
/// press of the confirming and closing keys counts; the arrows repeat; a release is nothing.
pub fn dialogue_key(code: KeyCode, pressed: bool, repeat: bool) -> Option<DialogueKey> {
    if !pressed {
        return None;
    }
    match code {
        KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter if !repeat => Some(DialogueKey::Confirm),
        KeyCode::Backspace | KeyCode::Escape if !repeat => Some(DialogueKey::Back),
        KeyCode::ArrowUp | KeyCode::KeyW => Some(DialogueKey::Up),
        KeyCode::ArrowDown | KeyCode::KeyS => Some(DialogueKey::Down),
        _ => None,
    }
}

// ---- the window --------------------------------------------------------------------------------

/// The message on show.
#[derive(Debug, Clone)]
struct Shown {
    message: Message,
    page: usize,
    /// Which of the page's choices the pointer is on (0 = the first).
    cursor: usize,
    /// First row of the page that is on show (a long list scrolls).
    scroll: usize,
}

impl Shown {
    fn new(message: Message) -> Self {
        // The cursor starts on the line the source says, if that is one of the choices.
        let cursor = message.choice.map_or(0, |c| c.cursor.saturating_sub(c.first).min(c.last.saturating_sub(c.first)));
        Self { message, page: 0, cursor, scroll: 0 }
    }
}

/// How many blank messages in a row a window skips before it gives up (a script of nothing but
/// codes, or a damaged one, must not make it spin).
const MAX_BLANKS: usize = 8;

/// What a window tells the voice layer as the player moves through a message (a conversation that is voiced gives its windows one):
/// the voice of the game's next page when the player reaches it, and of a question's line when the pointer reaches it. The voice of the
/// first page is the source's, when the window opens.
struct WindowVoice {
    field: String,
    sink: Arc<dyn ModSink>,
}

impl WindowVoice {
    /// The player reached the game's page `page` of `message`.
    fn page(&self, message: &Message, page: usize) {
        if let MessageOrigin::Canon { dialog, .. } = &message.origin {
            self.sink.cue(VoiceCue::Say { field: self.field.clone(), window: message.window, dialog: *dialog, page });
        }
    }

    /// The pointer of the question `message` reached its absolute line `line`.
    fn choice(&self, message: &Message, line: usize) {
        if let MessageOrigin::Canon { dialog, .. } = &message.origin {
            self.sink.cue(VoiceCue::Choice { field: self.field.clone(), dialog: *dialog, line });
        }
    }
}

/// A dialogue window: a source of messages and the one on show.
pub struct DialogueWindow {
    source: Box<dyn MessageSource>,
    shown: Shown,
    /// Where the window says what a voice follows (none: no voices).
    voice: Option<WindowVoice>,
}

impl std::fmt::Debug for DialogueWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogueWindow").field("shown", &self.shown).finish_non_exhaustive()
    }
}

impl DialogueWindow {
    /// A window on `source`, showing its first message; `None` when it has nothing to say.
    pub fn open(source: Box<dyn MessageSource>) -> Option<Self> {
        let mut source = source;
        let message = pull(&mut *source, None)?;
        Some(Self { source, shown: Shown::new(message), voice: None })
    }

    /// The same window, telling `sink` which voice follows as the player pages and moves the pointer (`field` is the area the
    /// conversation is in, which names the voices).
    pub fn with_voice(mut self, field: &str, sink: Arc<dyn ModSink>) -> Self {
        self.voice = Some(WindowVoice { field: field.to_string(), sink });
        self
    }

    /// The message on show.
    #[cfg(test)]
    pub fn message(&self) -> &Message {
        &self.shown.message
    }

    /// The page on show (0 is the first) and the cursor's choice (0 is the first).
    #[cfg(test)]
    pub fn position(&self) -> (usize, usize) {
        (self.shown.page, self.shown.cursor)
    }

    /// Whether the page on show (for a screen of `size`) holds the choices of a question, that is,
    /// whether the pointer is on screen and the arrows move it.
    #[cfg(test)]
    pub fn on_choices(&self, size: (u32, u32)) -> bool {
        let (laid, _) = self.laid(size);
        laid.pages.get(self.shown.page.min(laid.pages.len().saturating_sub(1))).is_some_and(|p| p.choices.is_some())
    }

    fn laid(&self, size: (u32, u32)) -> (Laid, Geometry) {
        let g = geometry(size.0, size.1);
        (lay_out(&self.shown.message, g.max_cells, g.max_rows), g)
    }

    /// The first row on show of `page`, so that the pointer's row is on screen (and a page with
    /// fewer rows than the window has no scroll).
    fn scroll_of(&self, laid: &Laid, page: &Page) -> usize {
        let max_scroll = page.rows.len().saturating_sub(laid.rows);
        let mut scroll = self.shown.scroll.min(max_scroll);
        if let Some(choices) = &page.choices {
            let focus = choices.start + self.shown.cursor.min(choices.len().saturating_sub(1));
            if focus < scroll {
                scroll = focus;
            } else if focus >= scroll + laid.rows {
                scroll = focus + 1 - laid.rows;
            }
        }
        scroll.min(max_scroll)
    }

    /// Give the window a key on a screen of `size`. Returns whether the window is still open.
    pub fn key(&mut self, key: DialogueKey, size: (u32, u32)) -> bool {
        let (laid, _) = self.laid(size);
        let last_page = laid.pages.len().saturating_sub(1);
        self.shown.page = self.shown.page.min(last_page);
        let Some(page) = laid.pages.get(self.shown.page) else { return false };
        match key {
            DialogueKey::Back => return false,
            DialogueKey::Up | DialogueKey::Down => {
                if let Some(choices) = &page.choices {
                    let n = choices.len().max(1);
                    let cursor = self.shown.cursor.min(n - 1);
                    self.shown.cursor = if key == DialogueKey::Up { (cursor + n - 1) % n } else { (cursor + 1) % n };
                    self.shown.scroll = self.scroll_of(&laid, page);
                    // The pointer is on another line: the voice of the line it reached (an absolute line number, as a script counts).
                    if let (Some(voice), true) = (&self.voice, self.shown.cursor != cursor) {
                        voice.choice(&self.shown.message, self.shown.message.choice.map_or(0, |c| c.first) + self.shown.cursor);
                    }
                }
            }
            DialogueKey::Confirm => {
                if let Some(choices) = &page.choices {
                    let choice = self.shown.message.choice;
                    let line = choice.map_or(0, |c| c.first) + self.shown.cursor.min(choices.len().saturating_sub(1));
                    return self.advance(Some(line));
                }
                if self.shown.page < last_page {
                    self.shown.page += 1;
                    // The player reached the next page of the game's message (not just the next of the window's cuts of one): its voice.
                    if let Some(voice) = &self.voice {
                        let at = |page: usize| laid.game_pages.get(page).copied().unwrap_or(0);
                        if at(self.shown.page) != at(self.shown.page - 1) {
                            voice.page(&self.shown.message, at(self.shown.page));
                        }
                    }
                    self.shown.scroll = 0;
                    if let Some(next) = laid.pages.get(self.shown.page) {
                        self.shown.scroll = self.scroll_of(&laid, next);
                    }
                } else {
                    return self.advance(None);
                }
            }
        }
        true
    }

    /// Ask the source for what comes next. `false` when there is nothing more.
    fn advance(&mut self, answer: Option<usize>) -> bool {
        match pull(&mut *self.source, answer) {
            Some(message) => {
                self.shown = Shown::new(message);
                true
            }
            None => false,
        }
    }

    /// Draw the window for a `vw` x `vh` screen at time `t` (seconds; the arrow blinks and the
    /// flashing colours flash).
    pub fn draw(&self, vw: u32, vh: u32, t: f64, out: &mut DrawList) {
        let (laid, g) = self.laid((vw, vh));
        let page_index = self.shown.page.min(laid.pages.len().saturating_sub(1));
        let Some(page) = laid.pages.get(page_index) else { return };
        let scroll = self.scroll_of(&laid, page);
        let rows = laid.rows;
        // (Not `clamp`: on a screen too narrow for the minimum, the screen wins.)
        let cells = laid.cells.max(MIN_CELLS).min(g.max_cells);
        let cells_i = i32::try_from(cells).unwrap_or(i32::MAX / 16);
        let (w, h) = (cells_i * 8 + 2 * PAD, i32::try_from(rows).unwrap_or(1) * ROW_H - (ROW_H - 8) + 2 * PAD);
        let (x, y) = ((g.cw - w) / 2, (g.floor - h).max(MARGIN));
        let mut c = Canvas { out, s: g.s };

        // The game's windows have the usual light border; a line this project added is bordered in
        // the accent colour, so that it never passes for the game's.
        let border = if self.shown.message.origin.is_canon() { BORDER } else { ACCENT };
        c.fill(x, y, w, h, border);
        c.gradient(x + 1, y + 1, w - 2, h - 2, PANEL_TOP, PANEL_BOTTOM);

        let blink = ((t * 2.5).floor() as i64).rem_euclid(2) == 0;
        for (i, row) in page.rows.iter().skip(scroll).take(rows).enumerate() {
            let index = scroll + i;
            let ry = y + PAD + i32::try_from(i).unwrap_or(0) * ROW_H;
            let is_choice = page.choices.as_ref().is_some_and(|r| r.contains(&index));
            let mut tx = x + PAD;
            if is_choice {
                let focus = page.choices.as_ref().map_or(0, |r| r.start + self.shown.cursor.min(r.len().saturating_sub(1)));
                if index == focus {
                    c.pointer(tx, ry);
                }
                tx += i32::try_from(POINTER_CELLS).unwrap_or(2) * 8;
            }
            for run in &row.runs {
                tx += c.shadowed(tx, ry, colour(run.colour, t), &run.text);
            }
        }
        // More to scroll, and more to read.
        let right = x + w - PAD - 8;
        if scroll > 0 {
            c.special(right, y + 1, ACCENT, Special::ArrowUp);
        }
        if scroll + rows < page.rows.len() {
            c.special(right, y + h - 9, ACCENT, Special::ArrowDown);
        } else if page_index + 1 < laid.pages.len() && blink {
            c.special(right, y + h - 6, ACCENT, Special::ArrowDown);
        }
    }
}

/// The next non-blank message of `source` (blank ones are skipped), `None` at the end.
fn pull(source: &mut dyn MessageSource, answer: Option<usize>) -> Option<Message> {
    let mut answer = answer;
    for _ in 0..MAX_BLANKS {
        match source.next(answer.take()) {
            Beat::Message(m) if layout::is_blank(&m) => {}
            Beat::Message(mut m) => {
                crate::adventurer_lines::adopt(&mut m);
                return Some(m);
            }
            Beat::End(_) => return None,
        }
    }
    None
}

/// What a text colour looks like on the window's blue.
fn colour(c: TextColour, t: f64) -> Rgba {
    let phase = (t * 3.0).floor() as i64;
    match c {
        TextColour::White => TEXT,
        TextColour::Gray => [150, 156, 190, 255],
        TextColour::Blue => [120, 150, 255, 255],
        TextColour::Red => [255, 110, 110, 255],
        TextColour::Purple => [214, 140, 255, 255],
        TextColour::Green => [130, 235, 140, 255],
        TextColour::Cyan => [120, 230, 255, 255],
        TextColour::Yellow => [255, 226, 96, 255],
        TextColour::Flash => {
            if phase.rem_euclid(2) == 0 {
                TEXT
            } else {
                DIM
            }
        }
        TextColour::Rainbow => {
            [[255, 110, 110, 255], [255, 226, 96, 255], [130, 235, 140, 255], [120, 230, 255, 255], [214, 140, 255, 255]]
                [usize::try_from(phase.rem_euclid(5)).unwrap_or(0)]
        }
    }
}

// ---- who is in front of us ---------------------------------------------------------------------

/// Where a character stands and how near one has to be to talk to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub pos: Pos,
    pub radius: f32,
}

/// How far (in cosine of the angle) the player's heading may be from the character: 70 degrees.
const FACING_COSINE: f32 = 0.342;
/// Within this part of its radius a character is talked to whichever way the player faces.
const CLOSE_FRACTION: f32 = 0.4;

/// The index of the spot the player at `own`, heading `facing` (radians, counter-clockwise from the
/// area's +x), talks to: the nearest whose radius reaches the player and that is in front of them
/// (or very near). Heights are ignored; a spot that is not finite is never chosen.
pub fn pick_target(own: Pos, facing: f32, spots: &[Spot]) -> Option<usize> {
    if !(own.x.is_finite() && own.y.is_finite() && facing.is_finite()) {
        return None;
    }
    let (fx, fy) = (facing.cos(), facing.sin());
    spots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let (dx, dy) = (s.pos.x - own.x, s.pos.y - own.y);
            let d = dx.hypot(dy);
            if !(d.is_finite() && s.radius.is_finite()) || d > s.radius {
                return None;
            }
            let in_front = d < 1e-3 || (dx * fx + dy * fy) / d >= FACING_COSINE;
            (in_front || d <= s.radius * CLOSE_FRACTION).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

// ---- the controller ----------------------------------------------------------------------------

/// Where conversations come from: the game's own scripts in the game, canned ones in tests.
pub trait Conversations {
    /// How near one has to stand to talk to `npc` of `area` (in the area's units; 0 if it cannot
    /// be talked to at all).
    fn talk_radius(&mut self, area: &str, npc: &NpcView) -> f32;

    /// The conversation with `npc`, or `None` when it has nothing to say.
    fn start(&mut self, area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>>;

    /// The text of dialog `dialog` of `area`'s original field script, as the window `window` of a live field shows it
    /// (`ask`: the lines of its question), or `None` when this machine cannot read it (no such field or dialog).
    fn live_message(&mut self, _area: &str, _window: u8, _dialog: usize, _ask: Option<LiveAsk>) -> Option<Message> {
        None
    }

    /// The window `view` of a live field as this machine shows it: [`Conversations::live_message`] by default; with a text layer
    /// that has its own numbering, the layer's line (and the voice that goes with it) found by the instruction that opened it.
    fn live_window(&mut self, area: &str, view: &LiveWindowView) -> Option<live::LiveText> {
        self.live_message(area, view.window, usize::from(view.dialog), view.ask).map(|message| live::LiveText { message, voice: None })
    }

    /// Where the story comes from (the hook for story progress), for sources whose branches depend
    /// on it; the others ignore it.
    fn set_story(&mut self, _story: ff7::StoryFactory) {}

    /// Where the windows of the conversation that [`Conversations::start`] just returned send the voices of its later pages and of a
    /// question's lines, when it is a voiced one (the voice of each message's first page is the source's own, as the window opens).
    fn voice(&mut self) -> Option<Arc<dyn ModSink>> {
        None
    }
}

/// What the player's surroundings are, for a key press.
#[derive(Debug, Clone, Copy)]
pub struct Around<'a> {
    /// The area's key (what the conversations are looked up by).
    pub area: &'a str,
    pub own: Pos,
    pub facing: f32,
    pub npcs: &'a [NpcView],
    /// A battle is on, or its panel still lingers: `Enter` belongs to the battle.
    pub blocked: bool,
}

/// The talk controller the window holds: the conversations, and the dialogue window when one is open.
pub struct Talk {
    conversations: Box<dyn Conversations>,
    window: Option<DialogueWindow>,
    /// The screen the window was last drawn for (keys lay the text out for the same one).
    size: Cell<(u32, u32)>,
    /// The arrow keys pressed (fresh) while the window has been open, and not yet let go of. Only
    /// they move the pointer, and only they may repeat: a direction held while walking up to the
    /// character goes on repeating when the window opens, and those repeats are the walk's, not
    /// a pointer that spins through the answers.
    armed: Vec<KeyCode>,
    /// The OK press the last [`Talk::key`] call was, if it was one: a fresh `Enter` or `Space` in the field (no
    /// dialogue open, no battle) is a press whether or not anybody was there to hear it. `Some(Some(id))` when the
    /// player faced a character in reach (the [`NpcView::id`] of the one it picked, whether or not it had anything to
    /// say), `Some(None)` when it found nobody. Taken by [`Talk::take_press`].
    press: Option<Option<u32>>,
    /// The area is a live field: the server's scripts own talking, so no conversation starts here (see [`Talk::set_live_area`]).
    live_area: bool,
    /// The window of a live field's script on show, and the one the server's windows were last built from.
    live: Option<LiveShown>,
    /// The window (number and opening) that could not be built, so that it is not tried again every frame.
    live_failed: Option<(u8, u64)>,
}

/// A window a live field's script opened, on show.
struct LiveShown {
    window: u8,
    serial: u64,
    answerable: bool,
    /// The first line of its question (the answer goes to the server counted from it).
    first: usize,
    view: DialogueWindow,
    outcome: Rc<Cell<live::Outcome>>,
}

impl std::fmt::Debug for Talk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Talk").field("window", &self.window).finish_non_exhaustive()
    }
}

impl Talk {
    pub fn new(conversations: Box<dyn Conversations>) -> Self {
        Self { conversations, window: None, size: Cell::new((960, 540)), armed: Vec::new(), press: None, live_area: false, live: None, live_failed: None }
    }

    /// Whether the area is a live field (the client has received a `LiveCast` since it entered): the server's own scripts
    /// run its talks, so an OK press only goes to the server (as `Interact`) and never opens a conversation here.
    pub fn set_live_area(&mut self, live: bool) {
        self.live_area = live;
    }

    /// Whether a live field's window is on show.
    #[cfg(test)]
    pub fn live_is_open(&self) -> bool {
        self.live.is_some()
    }

    /// Show what the server's scripts show: the lowest-numbered window `live` has open, built from this machine's own field data
    /// when it first appears (`story` is asked for the story the text reads, only then). Closes the window the server closed. A window this
    /// machine cannot show (no such dialog here, nothing written in it) is dismissed at once if this player is the one who answers, so
    /// that the script is never left waiting on it.
    pub fn sync_live(&mut self, live: &mut LiveClient, area: &str, story: impl FnOnce() -> ff7::StoryFactory) {
        let Some(view) = live.windows().next().copied() else {
            self.live = None;
            self.live_failed = None;
            return;
        };
        if self.live.as_ref().is_some_and(|s| (s.window, s.serial) == (view.window, view.serial)) || self.live_failed == Some((view.window, view.serial)) {
            return;
        }
        self.live = None;
        self.live_failed = None;
        self.armed.clear();
        self.conversations.set_story(story());
        let text = self.conversations.live_window(area, &view);
        let outcome = Rc::new(Cell::new(live::Outcome::Open));
        // The answer goes back counted from the first line of the question as shown (the server counts from the original's).
        let first = text.as_ref().and_then(|t| t.message.choice).map_or(0, |c| c.first);
        let voice = text.as_ref().and_then(|t| t.voice.clone());
        let built = text.and_then(|t| {
            let source = live::LiveSource::new(t.message, Rc::clone(&outcome)).with_voice(t.voice);
            DialogueWindow::open(Box::new(source))
        });
        // Later pages and the lines of a question cue their voices as the window's player reaches them.
        let built = built.map(|w| match voice {
            Some(v) => w.with_voice(&v.field, v.sink),
            None => w,
        });
        match built {
            Some(window) => {
                self.live = Some(LiveShown { window: view.window, serial: view.serial, answerable: view.answerable, first, view: window, outcome });
            }
            None => {
                self.live_failed = Some((view.window, view.serial));
                eprintln!("live: window {} dialog {} has nothing to show here{}", view.window, view.dialog, if view.answerable { "; dismissing it" } else { "" });
                live.answer(view.window, None);
            }
        }
    }

    /// A key event for the live window on show. Returns whether it took the key. Only this player's own window (the leader's)
    /// answers; a window of another leader is read (paged) and never sends anything. Like a conversation's window, only an arrow
    /// key pressed while the window is open moves the pointer and repeats (a direction held while walking in does not spin it).
    pub fn live_key(&mut self, live: &mut LiveClient, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        if !pressed {
            self.armed.retain(|k| *k != code);
            return false;
        }
        let Some(shown) = self.live.as_mut() else { return false };
        // Leaving is the player's menu's business (Esc, Backspace): a script's window cannot be closed from here.
        let Some(key) = dialogue_key(code, pressed, repeat).filter(|k| *k != DialogueKey::Back) else { return false };
        if matches!(key, DialogueKey::Up | DialogueKey::Down) {
            match (repeat, self.armed.contains(&code)) {
                (false, false) => self.armed.push(code),
                (true, false) => return false,
                _ => {}
            }
        }
        // The window the server has now may be another one under the same number (closed and reopened since the last
        // frame): this one is stale and answers nothing.
        let current = live.windows().find(|w| w.window == shown.window && w.serial == shown.serial);
        if current.is_none_or(|w| w.answered) {
            return true;
        }
        // The window stays shown whatever it answers with: the server closes it.
        let open = shown.view.key(key, self.size.get());
        if open || !shown.answerable {
            return true;
        }
        match shown.outcome.get() {
            live::Outcome::Answered(line) => live.answer(shown.window, Some(u8::try_from(line.saturating_sub(shown.first)).unwrap_or(u8::MAX))),
            live::Outcome::Dismissed => live.answer(shown.window, None),
            live::Outcome::Open => {}
        }
        true
    }

    /// The OK press the last [`Talk::key`] call was (see the `press` field), once: the window sends it to the server as
    /// `ClientMsg::Interact` right after the key was handled, so that the conversation the key opened has already been
    /// built from the story as it was before whatever the press does.
    pub fn take_press(&mut self) -> Option<Option<u32>> {
        self.press.take()
    }

    /// Where the story comes from: THE hook for story progress (`fmv_ff7::script::dialogue::StoryVars`
    /// through a factory called for every conversation). Until something is plugged in here every
    /// branch of every talk script is read as at the start of a new game.
    pub fn set_story(&mut self, story: ff7::StoryFactory) {
        self.conversations.set_story(story);
    }

    /// Open, replace or close the window; no key is armed across it.
    fn set_window(&mut self, window: Option<DialogueWindow>) {
        self.window = window;
        self.armed.clear();
    }

    /// A talk controller over this machine's own FF7 field data (`None`: nobody has anything to say).
    pub fn ff7(adapter: Option<std::sync::Arc<fmv_ff7::Ff7Adapter>>) -> Self {
        Self::new(Box::new(ff7::Ff7Conversations::new(adapter)))
    }

    /// Whether a dialogue window is open (the player stands still then).
    pub fn is_open(&self) -> bool {
        self.window.is_some()
    }

    /// Close the dialogue (a battle began, the area changed).
    pub fn close(&mut self) {
        self.set_window(None);
    }

    /// The message on show.
    #[cfg(test)]
    pub fn message(&self) -> Option<&Message> {
        self.window.as_ref().map(DialogueWindow::message)
    }

    /// A key event. Returns whether the dialogue took it (the window then does nothing else with it).
    pub fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool, around: &Around<'_>) -> bool {
        self.press = None;
        if around.blocked || self.live_area {
            self.set_window(None);
        }
        if !pressed {
            // A release is never ours (the walk must see it), but it ends an arrow key's claim.
            self.armed.retain(|k| *k != code);
            return false;
        }
        let Some(key) = dialogue_key(code, pressed, repeat) else { return false };
        if matches!(key, DialogueKey::Up | DialogueKey::Down) {
            if self.window.is_none() {
                return false;
            }
            // Only a key pressed while the window is open moves the pointer, and only that one repeats.
            match (repeat, self.armed.contains(&code)) {
                (false, false) => self.armed.push(code),
                (true, false) => return false,
                _ => {}
            }
        }
        if let Some(window) = self.window.as_mut() {
            if !window.key(key, self.size.get()) {
                self.set_window(None);
            }
            return true;
        }
        if key != DialogueKey::Confirm || around.blocked {
            return false;
        }
        if self.live_area {
            // The server's scripts own talking on a live field: the press is theirs (it picks who is in front of the player).
            self.press = Some(None);
            return false;
        }
        let spots: Vec<Spot> =
            around.npcs.iter().map(|n| Spot { pos: n.pos, radius: self.conversations.talk_radius(around.area, n) }).collect();
        let target = pick_target(around.own, around.facing, &spots).and_then(|i| around.npcs.get(i));
        // The conversation is built now, from the story as the caller's hook tells it NOW (see `field_key`): whatever
        // this press does to the story is the server's, comes back later, and never reaches a window that is open.
        let opened = target.and_then(|npc| {
            let source = self.conversations.start(around.area, npc)?;
            let window = DialogueWindow::open(source)?;
            // A voiced conversation's window says when its player reaches a later page or moves the pointer.
            let window = match self.conversations.voice() {
                Some(sink) => window.with_voice(around.area, sink),
                None => window,
            };
            Some((npc.id, window))
        });
        // A fresh OK in the field is a press, and the server decides what it does (a character's talk can move the
        // story, a ladder or a lift panel under the player's feet is used). It names the character only when the
        // press became a conversation with it: a character this machine has nothing to say for (hidden, silenced, a
        // stand-in that stands on a ladder) must not take the press away from the ladder.
        self.press = Some(opened.as_ref().map(|(id, _)| *id));
        match opened {
            Some((_, window)) => {
                self.set_window(Some(window));
                true
            }
            None => false,
        }
    }

    /// Draw the window, when one is open.
    pub fn draw(&self, vw: u32, vh: u32, t: f64, out: &mut DrawList) {
        self.size.set((vw, vh));
        if let Some(window) = &self.window {
            window.draw(vw, vh, t, out);
        }
        if let Some(shown) = &self.live {
            shown.view.draw(vw, vh, t, out);
        }
    }
}

/// What a key event did in the field (see [`field_key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldKey {
    /// The dialogue took the key (the window does nothing else with it).
    pub consumed: bool,
    /// The key was an OK press in the field, and what it addressed: send `ClientMsg::Interact { npc }` with it
    /// (`Some(None)`: the press found nobody to talk to, and is where the player stands, for a ladder or a panel).
    pub interact: Option<Option<u32>>,
}

/// A key event in the field, as the window handles it: the part that is not a window.
///
/// **Order.** A fresh OK press that opens a conversation builds it HERE, at the moment of the key, from the story the
/// session has mirrored by then (the hook is refreshed from the mirror just before the key is given to the dialogue,
/// so a conversation reads a copy: [`ff7::story_of`]). The press that the caller sends to the server afterwards
/// (`interact`) is what may move that story, and the `StoryUpdate` that comes back moves the mirror only: a
/// conversation that is open goes on reading the story it started with, and the next one reads the later story.
/// (The first talk of a story step shows the lines of the story as it was before the step, like the game.)
pub fn field_key(talk: &mut Talk, state: &ClientState, area: &str, now: f64, code: KeyCode, pressed: bool, repeat: bool) -> FieldKey {
    let blocked = state.battle.in_battle() || state.battle.lingering(now);
    if dialogue_key(code, pressed, repeat) == Some(DialogueKey::Confirm) && !talk.is_open() && !blocked {
        talk.set_story(ff7::story_of(state.story.state()));
    }
    talk.set_live_area(state.live.live_area);
    let around = Around { area, own: state.own.pos(), facing: state.own.facing(), npcs: state.npc_views(), blocked };
    let consumed = talk.key(code, pressed, repeat, &around);
    FieldKey { consumed, interact: talk.take_press() }
}
