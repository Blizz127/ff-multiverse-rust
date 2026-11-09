//! The window of a scene: one message window of the scene's script on show, its page and its question's cursor, and how
//! it is drawn.
//!
//! It is the talk window's own layout and look ([`crate::ui::dialogue::lay_out`]: the game's text folded to what the
//! font draws, wrapped in colour runs, cut into pages, a question's choices kept together; the window is drawn with the
//! same [`Canvas`] panel, pointer and arrows), with two differences a scene needs:
//!
//! * it does not pull the next message itself: the scene decides what comes after a window ([`Outcome::Pass`] says the
//!   player is done with it and what it answered), and
//! * it stands NEAR whoever speaks: above the speaker's head when there is room, else below the feet, else at the bottom
//!   of the screen as the talk window does, and always inside the screen ([`place_window`]), and where it can it does not
//!   hide the other people of the scene (it is moved aside, or below the speaker, to cover the least of them). A window
//!   with no speaker (a narrator's note) or one whose speaker is not on screen is at the bottom.
//!
//! Its text is never wider than [`MAX_CELLS`] cells so that it stays beside a character instead of across the screen; the
//! layout (and so the pages a key turns) is the same whether the window is near its speaker or at the bottom.
//!
//! The constants of the geometry are the talk window's (`ui::dialogue`'s are private to it): a canvas unit is
//! `min(width / 480, height / 280)` pixels, a window is `PAD` units inside its border and a row is `ROW_H` units.

use crate::ui::dialogue::{geometry, lay_out, Geometry, Laid, Page, POINTER_CELLS};
use crate::ui::draw::{DrawList, Rgba};
use crate::ui::font::Special;
use crate::ui::widgets::{Canvas, ACCENT, BORDER, DIM, PANEL_BOTTOM, PANEL_TOP, TEXT};
use fmv_ff7::script::dialogue::{Message, Piece, TextColour};
use fmv_ff7::script::scene::Window;

/// Canvas units from the edge of the screen to a window.
const MARGIN: i32 = 4;
/// Canvas units between a window's border and its text.
const PAD: i32 = 6;
/// Canvas units from one row of text to the next.
const ROW_H: i32 = 12;
/// The fewest cells inside a window (a window of one short line is still a window).
const MIN_CELLS: usize = 16;
/// The most cells inside a scene's window.
pub const MAX_CELLS: usize = 36;
/// Canvas units between a window and the head (or feet) of its speaker.
const GAP: i32 = 6;

/// What a key did to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The window is still on show (a page turned, the pointer moved, or nothing happened).
    Stay,
    /// The player is done with it, and answered this line if it asked.
    Pass(Option<usize>),
}

/// What a window's keys are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    Confirm,
    Up,
    Down,
}

/// A rectangle on the screen in window pixels from the top left: left, top, right, bottom.
pub type Rect = (f32, f32, f32, f32);

/// Where the speaker stands on the screen, in window pixels from the top left: the top of its head and its feet, and where
/// the other people of the scene are (what the window should not hide).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerSpot {
    pub head: (f32, f32),
    pub feet: (f32, f32),
    pub others: Vec<Rect>,
}

impl SpeakerSpot {
    fn is_finite(&self) -> bool {
        [self.head.0, self.head.1, self.feet.0, self.feet.1].iter().all(|v| v.is_finite())
    }
}

/// One window on show.
#[derive(Clone)]
pub struct Panel {
    window: Window,
    page: usize,
    /// Which of the page's choices the pointer is on (0 = the first).
    cursor: usize,
    /// First row of the page that is on show (a long list scrolls).
    scroll: usize,
}

/// Counts only: the text of a window is the game's and never goes to a log.
impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel").field("page", &self.page).field("cursor", &self.cursor).field("asks", &self.asks()).finish_non_exhaustive()
    }
}

/// Whether a message shows no character at all and asks nothing (a window of nothing but codes).
fn is_blank(message: &Message) -> bool {
    message.choice.is_none()
        && message.pieces.iter().all(|p| match p {
            Piece::Text(t) => t.chars().all(|c| c.is_whitespace() || c.is_control()),
            _ => true,
        })
}

impl Panel {
    pub fn new(window: Window) -> Self {
        // The cursor starts on the line the script says, if that is one of the choices.
        let cursor = window.message.choice.map_or(0, |c| c.cursor.saturating_sub(c.first).min(c.last.saturating_sub(c.first)));
        Self { window, page: 0, cursor, scroll: 0 }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Whether the window has nothing to show (a scene's owner moves past it at once).
    pub fn is_blank(&self) -> bool {
        is_blank(&self.window.message)
    }

    pub fn asks(&self) -> bool {
        self.window.message.choice.is_some()
    }

    /// The line a question's answer would be if it were given now: the pointer's, as the game's script stores it.
    fn cursor_line(&self) -> Option<usize> {
        let c = self.window.message.choice?;
        Some(c.first.saturating_add(self.cursor).clamp(c.first, c.last.max(c.first)))
    }

    /// The answer to put on a window that is moved past with `answer` (`None`: the pointer's line): the line, clamped into
    /// the choices as the script clamps it, for a window that asks; nothing for one that does not.
    pub fn resolve(&self, answer: Option<usize>) -> Option<usize> {
        let c = self.window.message.choice?;
        Some(answer.map_or_else(|| self.cursor_line().unwrap_or(c.first), |a| a.clamp(c.first, c.last.max(c.first))))
    }

    fn laid(&self, size: (u32, u32)) -> (Laid, Geometry) {
        let g = geometry(size.0, size.1);
        (lay_out(&self.window.message, g.max_cells.min(MAX_CELLS), g.max_rows), g)
    }

    /// Whether the page on show (for a screen of `size`) holds the choices of a question, that is, whether the pointer is on
    /// screen and the arrows move it.
    #[cfg(test)]
    pub fn on_choices(&self, size: (u32, u32)) -> bool {
        let (laid, _) = self.laid(size);
        laid.pages.get(self.page.min(laid.pages.len().saturating_sub(1))).is_some_and(|p| p.choices.is_some())
    }

    /// The page on show (0 is the first) and the pointer's choice (0 is the first).
    #[cfg(test)]
    pub fn position(&self) -> (usize, usize) {
        (self.page, self.cursor)
    }

    /// How many pages the window has on a screen of `size`.
    #[cfg(test)]
    pub fn pages(&self, size: (u32, u32)) -> usize {
        self.laid(size).0.pages.len()
    }

    /// The first row on show of `page`, so that the pointer's row is on screen (and a page with fewer rows than the window
    /// has no scroll).
    fn scroll_of(&self, laid: &Laid, page: &Page) -> usize {
        let max_scroll = page.rows.len().saturating_sub(laid.rows);
        let mut scroll = self.scroll.min(max_scroll);
        if let Some(choices) = &page.choices {
            let focus = choices.start + self.cursor.min(choices.len().saturating_sub(1));
            if focus < scroll {
                scroll = focus;
            } else if focus >= scroll + laid.rows {
                scroll = focus + 1 - laid.rows;
            }
        }
        scroll.min(max_scroll)
    }

    /// Give the window a key on a screen of `size`. `answers` is whether this player's keys answer (the owner's do; a
    /// watcher only turns the pages: it never moves past a window, the owner does).
    pub fn key(&mut self, key: PanelKey, size: (u32, u32), answers: bool) -> Outcome {
        let (laid, _) = self.laid(size);
        let last_page = laid.pages.len().saturating_sub(1);
        self.page = self.page.min(last_page);
        let Some(page) = laid.pages.get(self.page) else { return Outcome::Pass(None) };
        match key {
            PanelKey::Up | PanelKey::Down => {
                if let (Some(choices), true) = (&page.choices, answers) {
                    let n = choices.len().max(1);
                    let cursor = self.cursor.min(n - 1);
                    self.cursor = if key == PanelKey::Up { (cursor + n - 1) % n } else { (cursor + 1) % n };
                    self.scroll = self.scroll_of(&laid, page);
                }
                Outcome::Stay
            }
            PanelKey::Confirm => {
                if page.choices.is_some() {
                    return if answers { Outcome::Pass(self.cursor_line()) } else { Outcome::Stay };
                }
                if self.page < last_page {
                    self.page += 1;
                    self.scroll = laid.pages.get(self.page).map_or(0, |next| self.scroll_of(&laid, next));
                    Outcome::Stay
                } else if answers {
                    Outcome::Pass(None)
                } else {
                    Outcome::Stay
                }
            }
        }
    }

    /// Draw the window for a `vw` x `vh` screen at time `t` (seconds: the arrow blinks and the flashing colours flash), near
    /// `spot` when there is one.
    pub fn draw(&self, vw: u32, vh: u32, t: f64, spot: Option<&SpeakerSpot>, out: &mut DrawList) {
        if self.is_blank() {
            return;
        }
        let (laid, g) = self.laid((vw, vh));
        let page_index = self.page.min(laid.pages.len().saturating_sub(1));
        let Some(page) = laid.pages.get(page_index) else { return };
        let scroll = self.scroll_of(&laid, page);
        let rows = laid.rows;
        // (Not `clamp`: on a screen too narrow for the minimum, the screen wins.)
        let cells = laid.cells.max(MIN_CELLS).min(g.max_cells);
        let cells_i = i32::try_from(cells).unwrap_or(i32::MAX / 16);
        let (w, h) = (cells_i * 8 + 2 * PAD, i32::try_from(rows).unwrap_or(1) * ROW_H - (ROW_H - 8) + 2 * PAD);
        let (x, y) = place_window(&g, w, h, spot);
        let mut c = Canvas { out, s: g.s };

        // The game's windows have the usual light border; a line this project added is bordered in the accent colour, so
        // that it never passes for the game's.
        let border = if self.window.message.origin.is_canon() { BORDER } else { ACCENT };
        c.fill(x, y, w, h, border);
        c.gradient(x + 1, y + 1, w - 2, h - 2, PANEL_TOP, PANEL_BOTTOM);

        let blink = ((t * 2.5).floor() as i64).rem_euclid(2) == 0;
        for (i, row) in page.rows.iter().skip(scroll).take(rows).enumerate() {
            let index = scroll + i;
            let ry = y + PAD + i32::try_from(i).unwrap_or(0) * ROW_H;
            let is_choice = page.choices.as_ref().is_some_and(|r| r.contains(&index));
            let mut tx = x + PAD;
            if is_choice {
                let focus = page.choices.as_ref().map_or(0, |r| r.start + self.cursor.min(r.len().saturating_sub(1)));
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

/// Where a window `w` x `h` canvas units goes on a screen of geometry `g`: near its speaker, in the first of these places that
/// hides the least of the other people of the scene (the first of equals wins): above the head, centred on it; above it and
/// shifted to either side; below the feet, centred and shifted; and, last, at the bottom of the screen like the talk window.
/// Always inside the screen (and above the HUD's lines at the bottom) where the window is small enough to be. With no speaker
/// (or one that is not a place on the screen) it is at the bottom, centred.
pub fn place_window(g: &Geometry, w: i32, h: i32, spot: Option<&SpeakerSpot>) -> (i32, i32) {
    let bottom = ((g.cw - w) / 2, (g.floor - h).max(MARGIN));
    let Some(spot) = spot.filter(|s| s.is_finite()) else { return bottom };
    let s = g.s.max(1) as f32;
    let units = |v: f32| (v / s).round().clamp(-1.0e6, 1.0e6) as i32;
    let (head_x, head_y, feet_y) = (units(spot.head.0), units(spot.head.1), units(spot.feet.1));
    let x_max = (g.cw - w - MARGIN).max(MARGIN);
    let xs = [head_x - w / 2, head_x - w + w / 4, head_x - w / 4].map(|x| x.clamp(MARGIN, x_max));
    let above = head_y - GAP - h;
    let below = feet_y + GAP;
    let mut candidates: Vec<(i32, i32)> = Vec::new();
    if above >= MARGIN {
        candidates.extend(xs.iter().map(|&x| (x, above.min((g.floor - h).max(MARGIN)))));
    }
    if below + h <= g.floor {
        candidates.extend(xs.iter().map(|&x| (x, below)));
    }
    candidates.push(bottom);
    // The people the window must not hide, in canvas units (rounded outwards).
    let outwards = |v: f32, up: bool| -> i32 {
        let u = v / s;
        (if up { u.ceil() } else { u.floor() }).clamp(-1.0e6, 1.0e6) as i32
    };
    let people: Vec<(i32, i32, i32, i32)> = spot
        .others
        .iter()
        .filter(|r| [r.0, r.1, r.2, r.3].iter().all(|v| v.is_finite()))
        .map(|r| (outwards(r.0, false), outwards(r.1, false), outwards(r.2, true), outwards(r.3, true)))
        .collect();
    let hidden = |(x, y): (i32, i32)| -> i64 {
        people
            .iter()
            .map(|&(x0, y0, x1, y1)| {
                let (dx, dy) = ((x + w).min(x1) - x.max(x0), (y + h).min(y1) - y.max(y0));
                if dx > 0 && dy > 0 { i64::from(dx) * i64::from(dy) } else { 0 }
            })
            .sum()
    };
    candidates.into_iter().min_by_key(|&c| hidden(c)).unwrap_or(bottom)
}

/// What a text colour looks like on the window's blue (the talk window's own table).
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

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::dialogue::{Choice, MessageOrigin};

    fn window(pieces: Vec<Piece>, choice: Option<Choice>) -> Window {
        let mut message = Message::new(MessageOrigin::Added { layer: "test".into() }, pieces);
        message.choice = choice;
        Window { at: 0, opener: 1, speaker: None, id: 0, geometry: None, message }
    }

    fn text(t: &str) -> Piece {
        Piece::Text(t.into())
    }

    /// A message of `n` short lines (one page at the sizes of these tests when `n` is small).
    fn lines(n: usize) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for i in 0..n {
            if i > 0 {
                pieces.push(Piece::NewLine);
            }
            pieces.push(text(&format!("line {i}")));
        }
        pieces
    }

    fn question() -> Window {
        window(
            vec![text("Q?"), Piece::NewLine, Piece::Choice, text("Yes"), Piece::NewLine, Piece::Choice, text("No"), Piece::NewLine, Piece::Choice, text("Maybe")],
            Some(Choice { first: 1, last: 3, cursor: 2 }),
        )
    }

    const SIZE: (u32, u32) = (1100, 680);

    #[test]
    fn a_plain_window_turns_its_pages_and_then_is_passed_with_no_answer() {
        // 30 lines is more than a window's rows: several pages.
        let mut panel = Panel::new(window(lines(30), None));
        let pages = panel.pages(SIZE);
        assert!(pages >= 2, "{pages}");
        for page in 1..pages {
            assert_eq!(panel.key(PanelKey::Confirm, SIZE, true), Outcome::Stay);
            assert_eq!(panel.position().0, page);
        }
        assert_eq!(panel.key(PanelKey::Confirm, SIZE, true), Outcome::Pass(None), "the last page's confirm moves past the window");
        // Up and down mean nothing on a window that asks nothing.
        let mut panel = Panel::new(window(lines(2), None));
        assert_eq!(panel.key(PanelKey::Down, SIZE, true), Outcome::Stay);
        assert_eq!(panel.position(), (0, 0));
    }

    #[test]
    fn a_question_starts_on_the_scripts_line_wraps_and_answers_with_the_absolute_line() {
        let mut panel = Panel::new(question());
        assert!(panel.on_choices(SIZE));
        assert_eq!(panel.position().1, 1, "the cursor starts on the line the script says (line 2 is the second choice)");
        assert_eq!(panel.key(PanelKey::Down, SIZE, true), Outcome::Stay);
        assert_eq!(panel.position().1, 2);
        assert_eq!(panel.key(PanelKey::Down, SIZE, true), Outcome::Stay);
        assert_eq!(panel.position().1, 0, "it wraps round");
        assert_eq!(panel.key(PanelKey::Up, SIZE, true), Outcome::Stay);
        assert_eq!(panel.position().1, 2, "and back");
        assert_eq!(panel.key(PanelKey::Confirm, SIZE, true), Outcome::Pass(Some(3)), "the third choice is line 3 of the message");
        // The first choice is line 1.
        let mut panel = Panel::new(question());
        panel.key(PanelKey::Up, SIZE, true);
        assert_eq!(panel.key(PanelKey::Confirm, SIZE, true), Outcome::Pass(Some(1)));
    }

    #[test]
    fn a_watcher_turns_pages_and_never_answers_or_moves_past() {
        let mut panel = Panel::new(question());
        assert_eq!(panel.key(PanelKey::Down, SIZE, false), Outcome::Stay);
        assert_eq!(panel.position().1, 1, "a watcher's arrows do not move the pointer");
        assert_eq!(panel.key(PanelKey::Confirm, SIZE, false), Outcome::Stay, "nor does its confirm answer");
        let mut panel = Panel::new(window(lines(30), None));
        let pages = panel.pages(SIZE);
        for _ in 1..pages {
            assert_eq!(panel.key(PanelKey::Confirm, SIZE, false), Outcome::Stay);
        }
        assert_eq!(panel.position().0, pages - 1, "it read to the last page");
        assert_eq!(panel.key(PanelKey::Confirm, SIZE, false), Outcome::Stay, "and waits there for the owner");
    }

    #[test]
    fn an_answer_is_clamped_into_the_choices_as_the_script_clamps_it() {
        let panel = Panel::new(question());
        assert_eq!(panel.resolve(None), Some(2), "the pointer's line");
        assert_eq!(panel.resolve(Some(3)), Some(3));
        assert_eq!(panel.resolve(Some(0)), Some(1), "before the first choice is the first");
        assert_eq!(panel.resolve(Some(200)), Some(3), "past the last is the last");
        assert_eq!(Panel::new(window(lines(2), None)).resolve(Some(2)), None, "a window that asks nothing has no answer");
    }

    #[test]
    fn a_window_of_nothing_is_blank_and_draws_nothing() {
        let blank = window(vec![Piece::Colour(TextColour::Red), text("  "), Piece::NewLine, text("\u{a0}")], None);
        assert!(Panel::new(blank.clone()).is_blank());
        let mut out = DrawList::new();
        Panel::new(blank).draw(1100, 680, 0.0, None, &mut out);
        assert!(out.is_empty());
        assert!(!Panel::new(window(lines(1), None)).is_blank());
        assert!(!Panel::new(question()).is_blank(), "a question is never blank");
    }

    #[test]
    fn nothing_the_game_wrote_reaches_a_debug_print() {
        let panel = Panel::new(window(vec![text("secret words")], None));
        assert!(!format!("{panel:?}").contains("secret"));
    }

    /// The pixels the window covers, from its border quad (the first thing it draws).
    fn rect(out: &DrawList) -> (i32, i32, i32, i32) {
        let q = out.quads()[0];
        (q.x, q.y, q.x + q.w, q.y + q.h)
    }

    #[test]
    fn a_window_stands_above_its_speakers_head_centred_on_it() {
        let g = geometry(1100, 680);
        let (w, h) = (200, 40);
        // The speaker's head is at the middle of the screen, in the lower half.
        let spot = SpeakerSpot { head: (550.0, 400.0), feet: (550.0, 480.0), others: vec![] };
        let (x, y) = place_window(&g, w, h, Some(&spot));
        let s = g.s as f32;
        assert_eq!(x, (550.0 / s).round() as i32 - w / 2, "centred on the speaker");
        assert_eq!(y, (400.0 / s).round() as i32 - GAP - h, "its bottom edge a gap above the head");
        // Drawn: the window's rectangle sits above the head's pixel row and holds the head's column.
        let mut out = DrawList::new();
        let panel = Panel::new(window(lines(2), None));
        panel.draw(1100, 680, 0.0, Some(&spot), &mut out);
        let (x0, y0, x1, y1) = rect(&out);
        assert!(y1 <= 400 - GAP * g.s + 1, "above the head: bottom {y1}");
        assert!(x0 <= 550 && 550 <= x1, "over the speaker's column: {x0}..{x1}");
        assert!(y0 >= 0 && x0 >= 0 && x1 <= 1100, "inside the screen");
    }

    #[test]
    fn a_window_that_does_not_fit_above_goes_below_the_feet_and_else_to_the_bottom() {
        let g = geometry(1100, 680);
        let (w, h) = (200, 40);
        let s = g.s as f32;
        // A head near the top of the screen: no room above, room below the feet.
        let high = SpeakerSpot { head: (550.0, 20.0), feet: (550.0, 120.0), others: vec![] };
        let (_, y) = place_window(&g, w, h, Some(&high));
        assert_eq!(y, (120.0 / s).round() as i32 + GAP, "below the feet");
        // No room above and none below: the bottom, like the talk window.
        let tall = SpeakerSpot { head: (550.0, 20.0), feet: (550.0, 700.0), others: vec![] };
        assert_eq!(place_window(&g, w, h, Some(&tall)), place_window(&g, w, h, None));
        // Nobody speaks (or the spot is not a place): the bottom, centred.
        let (bx, by) = place_window(&g, w, h, None);
        assert_eq!((bx, by), ((g.cw - w) / 2, g.floor - h));
        let nowhere = SpeakerSpot { head: (f32::NAN, 1.0), feet: (1.0, 1.0), others: vec![] };
        assert_eq!(place_window(&g, w, h, Some(&nowhere)), (bx, by));
    }

    /// How many canvas units of area of `people` (window pixels) the window of `size` at `at` covers.
    fn hidden_area(g: &Geometry, at: (i32, i32), size: (i32, i32), people: &[Rect]) -> f32 {
        let (x0, y0, x1, y1) = (at.0, at.1, at.0 + size.0, at.1 + size.1);
        let s = g.s as f32;
        people
            .iter()
            .map(|r| {
                let (dx, dy) = ((x1 as f32).min(r.2 / s) - (x0 as f32).max(r.0 / s), (y1 as f32).min(r.3 / s) - (y0 as f32).max(r.1 / s));
                if dx > 0.0 && dy > 0.0 { dx * dy } else { 0.0 }
            })
            .sum()
    }

    #[test]
    fn a_window_does_not_hide_the_other_people_of_the_scene_when_there_is_a_place_that_does_not() {
        let g = geometry(1100, 680);
        let (w, h) = (140, 36);
        // The speaker's head is at (550, 440); somebody stands up and to the left of it (so the window above, centred, covers
        // them) and somebody up and to the right (so the window shifted that way would).
        let left = (330.0, 330.0, 500.0, 470.0);
        let right = (600.0, 330.0, 770.0, 470.0);
        let spot = |others: Vec<Rect>| SpeakerSpot { head: (550.0, 440.0), feet: (550.0, 540.0), others };
        // Nobody else: above the head, centred, exactly as before.
        let alone = place_window(&g, w, h, Some(&spot(vec![])));
        assert_eq!(alone.1, (440.0 / g.s as f32).round() as i32 - GAP - h);
        // With a person in the way of the centred window, it is moved aside or below, and covers less of them than the centred one.
        let people = vec![left];
        let moved = place_window(&g, w, h, Some(&spot(people.clone())));
        assert!(hidden_area(&g, moved, (w, h), &people) < hidden_area(&g, alone, (w, h), &people), "{moved:?} vs {alone:?}");
        assert_eq!(hidden_area(&g, moved, (w, h), &people), 0.0, "there is a place that hides nobody");
        // With a person on each side, the window goes where it hides nobody: below the speaker's feet.
        let both = vec![left, right];
        let below = place_window(&g, w, h, Some(&spot(both.clone())));
        assert_eq!(hidden_area(&g, below, (w, h), &both), 0.0, "{below:?}");
        assert!(below.1 > (540.0 / g.s as f32).round() as i32, "below the feet: {below:?}");
        // With nowhere clear, it covers the least it can (never the speaker's own head).
        let everywhere = vec![(0.0, 0.0, 1100.0, 600.0)];
        let least = place_window(&g, w, h, Some(&spot(everywhere.clone())));
        let candidates = [alone, moved, below, place_window(&g, w, h, None)];
        assert!(candidates.iter().all(|c| hidden_area(&g, least, (w, h), &everywhere) <= hidden_area(&g, *c, (w, h), &everywhere)), "{least:?}");
        // Everything stays on the screen.
        for at in [alone, moved, below, least] {
            assert!(at.0 >= MARGIN && at.0 + w <= g.cw - MARGIN && at.1 >= MARGIN && at.1 + h <= g.ch, "{at:?}");
        }
    }

    #[test]
    fn a_speaker_at_the_edge_of_the_screen_keeps_the_window_on_screen() {
        for size in [(320u32, 240u32), (640, 360), (1100, 680), (1920, 1080)] {
            let g = geometry(size.0, size.1);
            for head_x in [-500.0, 0.0, 5.0, size.0 as f32 / 2.0, size.0 as f32 - 5.0, size.0 as f32 + 900.0] {
                for head_y in [-50.0, 0.0, 120.0, size.1 as f32 / 2.0, size.1 as f32 * 0.9, size.1 as f32 * 2.0] {
                    let spot = SpeakerSpot { head: (head_x, head_y), feet: (head_x, head_y + 80.0), others: vec![] };
                    let (w, h) = (150, 36);
                    let (x, y) = place_window(&g, w, h, Some(&spot));
                    assert!(x >= MARGIN && x + w <= g.cw - MARGIN, "{size:?} head ({head_x},{head_y}): x {x} w {w} of {}", g.cw);
                    assert!(y >= MARGIN && y + h <= g.ch, "{size:?} head ({head_x},{head_y}): y {y} h {h} of {}", g.ch);
                }
            }
        }
    }

    #[test]
    fn a_question_draws_its_pointer_on_the_row_the_cursor_is_on() {
        let mut panel = Panel::new(question());
        let draw = |panel: &Panel| {
            let mut out = DrawList::new();
            panel.draw(1100, 680, 0.0, None, &mut out);
            out
        };
        // The pointer is drawn in the accent colour next to the cursor's row; moving the cursor moves it.
        let pointer_y = |out: &DrawList| out.quads().iter().filter(|q| q.color == ACCENT && q.w <= 16 && q.h <= 16).map(|q| q.y).max();
        let before = draw(&panel);
        panel.key(PanelKey::Down, SIZE, true);
        let after = draw(&panel);
        let (a, b) = (pointer_y(&before), pointer_y(&after));
        assert!(a.is_some() && b.is_some(), "a pointer is drawn");
        assert!(b > a, "the pointer moved down with the cursor: {a:?} -> {b:?}");
    }

    #[test]
    fn a_window_of_added_text_is_bordered_in_the_accent_colour_and_canon_in_the_light_border() {
        let mut out = DrawList::new();
        Panel::new(window(lines(1), None)).draw(1100, 680, 0.0, None, &mut out);
        assert_eq!(out.quads()[0].color, ACCENT);
        let mut canon = window(lines(1), None);
        canon.message.origin = MessageOrigin::Canon { entity: 1, dialog: 2 };
        let mut out = DrawList::new();
        Panel::new(canon).draw(1100, 680, 0.0, None, &mut out);
        assert_eq!(out.quads()[0].color, BORDER);
    }
}
