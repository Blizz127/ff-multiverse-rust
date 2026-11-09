//! The countdown's HUD: the server's bomb timer, drawn as `M:SS` at the top centre of the window.
//!
//! The SERVER owns the clock (`fmv-server`'s `world::countdown`) and tells the player what to show
//! (`ServerMsg::Countdown`): a running countdown with the time left as it wrote the message, or that it is over, and
//! why. This module only draws what it was told. [`CountdownMirror`] (kept in the client's state) counts down on the
//! client's own clock from the moment a `Running` message is read, so the numbers move between messages and a later
//! `Running` replaces them; [`CountdownMirror::view`] says what to draw now and [`draw`] draws it.
//!
//! * **Running.** The time left, `M:SS` (rounded up: it reads `0:01` until it is over), in the calm tone, and in the warning
//!   tone for the last minute ([`URGENT_SECS`]). A countdown that reaches zero before the server says so stays at `0:00`.
//! * **Over, time up.** `0:00` and a line saying so, in the warning tone, for [`TIME_UP_SHOW_S`] seconds, then nothing.
//!   **That is all it means**: nothing else happens to the player or its story (the server does not move anybody, or touch
//!   what it saved).
//! * **Over, cleared.** The story moved past it: nothing is drawn.
//!
//! The headless client says the same in `countdown:` lines on stderr ([`CountdownMirror::drain_notes`]).

use std::collections::VecDeque;

use fmv_protocol::{CountdownEnd, CountdownState};

use super::draw::{DrawList, Rgba};
use super::hud::hud_scale;
use super::text::{text_height, text_width};
use super::widgets::{DIM, SHADOW, TEXT};
use crate::render::BANNER_PX;

/// The last this many seconds are drawn in the warning tone.
pub const URGENT_SECS: f64 = 60.0;
/// How long "time is up" stays on the screen (seconds).
pub const TIME_UP_SHOW_S: f64 = 8.0;
/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 16;
/// The plate behind the clock.
const PLATE: Rgba = [6, 8, 24, 175];
/// The warning tone (the last minute, and time up).
const WARNING: Rgba = [255, 96, 96, 255];
/// The line under the clock when time is up (the player's own words: nothing of the game's).
const TIME_UP_LINE: &str = "TIME IS UP";

/// What the server last said, in the client's own clock.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shown {
    /// Counting down: zero at `deadline` (seconds of the session clock), `total` seconds long at the start.
    Running { deadline: f64, total: f64 },
    /// Over for the reason given, told at `since`.
    Over { reason: CountdownEnd, since: f64 },
}

/// What the server last said about the countdown that governs our story (see the module docs). Empty until told: nothing is
/// drawn and nothing is said.
#[derive(Debug, Default)]
pub struct CountdownMirror {
    shown: Option<Shown>,
    notes: VecDeque<String>,
    told: u32,
}

/// The colour a clock is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Calm,
    Urgent,
}

/// What to draw now.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// `M:SS`.
    pub clock: String,
    pub tone: Tone,
    /// The fraction of the countdown that is left (1 at the start, 0 when it is over).
    pub fraction: f32,
    /// Time is up (the line that says so is drawn too).
    pub time_up: bool,
}

/// `seconds` left as `M:SS`, rounded up (so a countdown reads `0:01` until it is over). Negative, NaN and infinite
/// values read `0:00`, and an absurd length is cut at 99:59.
pub fn clock_text(seconds: f64) -> String {
    let whole = if seconds.is_finite() && seconds > 0.0 { seconds.ceil().min(99.0 * 60.0 + 59.0) as u32 } else { 0 };
    format!("{}:{:02}", whole / 60, whole % 60)
}

impl CountdownMirror {
    /// The server told us about the countdown (`state`) at `now` (seconds of the session clock).
    pub fn on_message(&mut self, now: f64, state: CountdownState) {
        self.told = self.told.saturating_add(1);
        match state {
            CountdownState::Running { remaining_ms, total_ms } => {
                let (left, total) = (f64::from(remaining_ms) / 1000.0, (f64::from(total_ms) / 1000.0).max(f64::from(remaining_ms) / 1000.0));
                self.shown = Some(Shown::Running { deadline: now + left, total });
                self.note(format!("running: {left:.1} s left of {total:.1} s"));
            }
            CountdownState::Over { reason: CountdownEnd::TimeUp } => {
                self.shown = Some(Shown::Over { reason: CountdownEnd::TimeUp, since: now });
                self.note("over: time is up (nothing else happens: the story and what was saved are as they were)".to_string());
            }
            CountdownState::Over { reason: CountdownEnd::Cleared } => {
                self.shown = Some(Shown::Over { reason: CountdownEnd::Cleared, since: now });
                self.note("over: cleared (the story moved past it)".to_string());
            }
        }
    }

    /// Whether a countdown is running as far as we were told (and has not been told over).
    pub fn is_running(&self) -> bool {
        matches!(self.shown, Some(Shown::Running { .. }))
    }

    /// How many countdown messages the server has sent.
    pub fn told(&self) -> u32 {
        self.told
    }

    /// What to draw at `now`: `None` when nothing is running and no time-up notice is on show.
    pub fn view(&self, now: f64) -> Option<View> {
        match self.shown? {
            Shown::Running { deadline, total } => {
                let left = (deadline - now).max(0.0);
                Some(View {
                    clock: clock_text(left),
                    tone: if left <= URGENT_SECS { Tone::Urgent } else { Tone::Calm },
                    fraction: if total > 0.0 { (left / total).clamp(0.0, 1.0) as f32 } else { 0.0 },
                    time_up: false,
                })
            }
            Shown::Over { reason: CountdownEnd::TimeUp, since } if now - since < TIME_UP_SHOW_S => {
                Some(View { clock: clock_text(0.0), tone: Tone::Urgent, fraction: 0.0, time_up: true })
            }
            Shown::Over { .. } => None,
        }
    }

    /// The notes since the last call, oldest first (`countdown: ...` on stderr in a headless run).
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }
}

/// Draw the countdown (nothing when there is nothing to show) at the top centre of a `w` x `h` window, under the banner a
/// fixture area wears (`fixture`) and clear of the label it has centred below it. `now` is the session clock the mirror was
/// told with.
pub fn draw(mirror: &CountdownMirror, w: u32, h: u32, fixture: bool, now: f64, out: &mut DrawList) {
    let Some(view) = mirror.view(now) else { return };
    draw_view(&view, w, h, fixture, out);
}

/// [`draw`] of a view (tests draw views of their own).
pub fn draw_view(view: &View, w: u32, h: u32, fixture: bool, out: &mut DrawList) {
    let s = hud_scale(w, h);
    let si = i32::try_from(s).unwrap_or(1);
    let big = s.saturating_mul(2);
    let pad = 3 * si;
    let wi = i32::try_from(w).unwrap_or(i32::MAX);
    // under the banner, and under the FIXTURE label that is centred below it
    let top = if fixture { BANNER_PX as i32 + text_height(s) + 10 * si } else { 0 } + 6 * si;
    let tone = if view.tone == Tone::Urgent { WARNING } else { TEXT };
    let clock_w = text_width(&view.clock, big);
    let line_w = if view.time_up { text_width(TIME_UP_LINE, s) } else { 0 };
    let plate_w = clock_w.max(line_w) + 2 * pad;
    let plate_h = text_height(big) + if view.time_up { text_height(s) + pad } else { 0 } + 2 * pad;
    let x = ((wi - plate_w) / 2).max(2);
    out.fill_rect(x, top, plate_w, plate_h, PLATE);
    // a thin bar under the clock: what is left of it
    let bar_w = (f64::from(plate_w - 2 * pad) * f64::from(view.fraction.clamp(0.0, 1.0))).round() as i32;
    out.draw_text_shadowed(x + (plate_w - clock_w) / 2, top + pad, big, tone, SHADOW, &view.clock);
    if bar_w > 0 {
        out.fill_rect(x + pad, top + plate_h - si.max(1), bar_w, si.max(1), tone);
    }
    if view.time_up {
        out.draw_text_shadowed(x + (plate_w - line_w) / 2, top + pad + text_height(big) + pad, s, DIM, SHADOW, TIME_UP_LINE);
    }
}

#[cfg(test)]
mod tests;
