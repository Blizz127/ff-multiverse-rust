//! The 2D windows of the battle screen, in the style of FF7's blue gradient windows: the enemy list
//! (bottom left), our command menu and its Magic and Item lists (bottom middle, or over the enemy
//! list in a small window), the party panel (bottom right: name, HP, MP and the ATB bar of
//! everyone, our character highlighted), the action banner at the top and the victory or defeat
//! panel. The floating damage numbers and the target pointer are drawn here too, over the places a
//! scene gives ([`super::view::Anchors`]).
//!
//! Everything is laid out on a canvas of whole canvas units ([`Layout::s`] window pixels each, like
//! the character menu's), so every edge is on the pixel grid and text is always a whole multiple of
//! the 8x8 font. The layout is [`layout`]: it chooses the scale for the window, puts the three
//! bottom windows side by side when the canvas is wide enough and lets the command menu cover the
//! enemy list when it is not, and says where the strip above the windows is (where fighters stand,
//! see [`Layout::stage`]). Drawing is pure: a view and a layout in, rectangles and text out.

use super::menu::{AimView, Level, ListView, MenuView};
use super::popups::{Popup, PopupKind};
use super::view::{gauge_fraction, limit_fraction, limit_full, Anchors, Bar, BattleView, ATB_READY_RGB, ATB_RGB, BAND_H, LIMIT_FULL_RGB, LIMIT_RGB};
use crate::ui::draw::{DrawList, Rgba};
use crate::ui::font::Special;
use crate::ui::text::{cell_count, ellipsize, text_height, text_width};
use crate::ui::widgets::{Canvas, ACCENT, BORDER, BORDER_DIM, DIM, ERROR, HILITE, INFO, OUTLINE, PANEL_BOTTOM, PANEL_TOP, SHADOW, SOFT, TEXT, TRACK, WARN};
use fmv_ff7_battle::rules::{ActorId, Outcome};

/// Canvas units from the window's edge to a window.
const MARGIN: i32 = 4;
/// Canvas units between a window's border and its content.
const PAD: i32 = 6;
/// Canvas units from one row of text to the next.
pub const ROW_H: i32 = 12;
/// Width of the pointer column in front of a row.
const POINTER_W: i32 = 8;
/// The smallest canvas of the three-windows layout (canvas units).
const FULL_W: i32 = 480;
const FULL_H: i32 = 280;
const MAX_SCALE: i32 = 8;
/// Height of the banner window.
const BANNER_H: i32 = 16;
/// Most fighters per side the layout makes room for.
const MAX_ROWS: usize = 8;

/// Milliseconds after the end of a battle at which the victory or defeat panel appears (the last
/// blow's number is on screen first).
pub const PANEL_DELAY_MS: u32 = 900;

/// How long the banner stays, in seconds.
pub const BANNER_S: f64 = 1.6;

/// A rectangle in canvas units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    fn right(self) -> i32 {
        self.x + self.w
    }

    /// The content area: inside the border and the padding.
    fn inner(self) -> Area {
        Area { x: self.x + PAD, y: self.y + PAD, w: (self.w - 2 * PAD).max(0), h: (self.h - 2 * PAD).max(0) }
    }
}

/// Where everything goes for a window size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Window pixels per canvas unit.
    pub s: i32,
    /// The canvas in units.
    pub cw: i32,
    pub ch: i32,
    /// The window is too narrow for three windows side by side.
    pub compact: bool,
    /// The row of the status line and the top of the banner, in units.
    pub status_y: i32,
    pub banner_y: i32,
    pub enemy: Area,
    pub command: Area,
    pub party: Area,
    /// The enemy list and the command window together: where the Magic and Item lists open.
    pub left: Area,
}

/// The layout for a `vw` x `vh` window with `party_len` party members (the party window grows past
/// four).
pub fn layout(vw: u32, vh: u32, party_len: usize) -> Layout {
    let (w, h) = (i32::try_from(vw.max(1)).unwrap_or(i32::MAX), i32::try_from(vh.max(1)).unwrap_or(i32::MAX));
    let s = (w / FULL_W).min(h / FULL_H).clamp(1, MAX_SCALE);
    let (cw, ch) = ((w / s).max(1), (h / s).max(1));
    let compact = cw < FULL_W;
    // The header row and a row per member (at least four).
    let rows = i32::try_from(party_len.clamp(4, MAX_ROWS)).unwrap_or(4) + 1;
    let win_h = (rows * ROW_H + 2 * PAD).min((ch / 2).max(2 * PAD + ROW_H));
    let y = (ch - MARGIN - win_h).max(0);
    let band = (BAND_H as i32 + s - 1) / s;
    let status_y = band + 2;
    let banner_y = status_y + 8 + 4;
    let (enemy, command, party, left);
    if compact {
        let left_w = (cw * 38 / 100).clamp(72, 150).min((cw - 2 * MARGIN).max(0));
        left = Area { x: MARGIN, y, w: left_w, h: win_h };
        enemy = left;
        command = left;
        let party_x = left.right() + MARGIN;
        party = Area { x: party_x, y, w: (cw - MARGIN - party_x).max(0), h: win_h };
    } else {
        let enemy_w = (cw * 26 / 100).clamp(112, 170);
        enemy = Area { x: MARGIN, y, w: enemy_w, h: win_h };
        command = Area { x: enemy.right() + MARGIN, y, w: 72, h: win_h };
        let party_x = command.right() + MARGIN;
        party = Area { x: party_x, y, w: (cw - MARGIN - party_x).max(0), h: win_h };
        left = Area { x: MARGIN, y, w: command.right() - MARGIN, h: win_h };
    }
    Layout { s, cw, ch, compact, status_y, banner_y, enemy, command, party, left }
}

impl Layout {
    /// The strip above the bottom windows where fighters stand, in window pixels
    /// (`x0, y0, x1, y1`): below the banner, above the windows and the names under the fighters.
    pub fn stage(&self) -> (i32, i32, i32, i32) {
        let s = self.s;
        let top = (self.banner_y + BANNER_H + 4) * s;
        let bottom = ((self.enemy.y - 14) * s).max(top + 1);
        (MARGIN * s, top, (self.cw - MARGIN) * s, bottom)
    }
}

// ---------------------------------------------------------------------------------------------
// What the panels show besides the fighters
// ---------------------------------------------------------------------------------------------

/// The name of the action being performed, on screen for [`BANNER_S`] seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    /// Milliseconds since it appeared.
    pub age_ms: u32,
}

/// An item the battle paid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardItem {
    pub name: String,
    pub count: u16,
}

/// What a won battle paid our character.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RewardLine {
    pub exp: u32,
    pub gil: u32,
    pub ap: u32,
    pub items: Vec<RewardItem>,
}

/// The end of the battle: the victory or defeat panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finale {
    pub outcome: Outcome,
    /// What our character earned; `None` when we were only watching.
    pub reward: Option<RewardLine>,
    /// Milliseconds since the battle ended.
    pub age_ms: u32,
    /// A key press closes the panel now (it is held back for a moment so that a key still
    /// pressed from the battle does not skip it).
    pub dismissible: bool,
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

/// The colour of text that cannot be chosen: darker than [`DIM`], so that a greyed-out row is
/// clearly not an ordinary one.
pub const DISABLED: Rgba = [132, 138, 178, 255];
/// The colour of an aimed-at row.
const AIMED: Rgba = [150, 180, 255, 110];
/// The colour of HP that is low (a third or less).
const HP_LOW: Rgba = WARN;
const HEAL: Rgba = [112, 236, 132, 255];
const MP_POP: Rgba = [130, 210, 255, 255];
const MISS: Rgba = [214, 218, 236, 255];

/// An FF7-style window: a light border with rounded corners, a blue gradient inside, a dark outline
/// that separates it from whatever is behind.
fn window(c: &mut Canvas<'_>, a: Area) {
    if a.w < 6 || a.h < 6 {
        return;
    }
    let Area { x, y, w, h } = a;
    // Outline: the border's shape, one unit wider.
    c.fill(x - 1, y + 1, w + 2, h - 2, OUTLINE);
    c.fill(x + 1, y - 1, w - 2, h + 2, OUTLINE);
    // The border, without its corner units.
    c.fill(x + 1, y, w - 2, h, BORDER);
    c.fill(x, y + 1, w, h - 2, BORDER);
    // Inside: the gradient with its own corners cut, in the border's dimmer tone.
    c.fill(x + 2, y + 1, w - 4, 1, PANEL_TOP);
    c.fill(x + 2, y + h - 2, w - 4, 1, PANEL_BOTTOM);
    c.gradient(x + 1, y + 2, w - 2, h - 4, PANEL_TOP, PANEL_BOTTOM);
    for (cx, cy) in [(x + 1, y + 1), (x + w - 2, y + 1), (x + 1, y + h - 2), (x + w - 2, y + h - 2)] {
        c.fill(cx, cy, 1, 1, BORDER_DIM);
    }
}

/// Cells of text that fit in `width` units.
fn cells_in(width: i32) -> usize {
    usize::try_from((width / 8).max(0)).unwrap_or(0)
}

/// A row highlight behind the text of a row of the window content `inner`.
fn row_band(c: &mut Canvas<'_>, inner: Area, y: i32, color: Rgba) {
    c.fill(inner.x - 3, y - 2, inner.w + 6, ROW_H - 1, color);
}

/// Draw `text` at (`x`, `y`) cut to fit `width` units.
fn fitted(c: &mut Canvas<'_>, x: i32, y: i32, width: i32, color: Rgba, text: &str) {
    let text = ellipsize(text, cells_in(width));
    c.shadowed(x, y, color, &text);
}

/// The enemy list: the living enemies' names, the one aimed at highlighted.
fn draw_enemies(c: &mut Canvas<'_>, l: &Layout, view: &BattleView, aim: Option<ActorId>) {
    let living: Vec<&Bar> = view.enemies.iter().filter(|b| b.alive).collect();
    if living.is_empty() {
        return;
    }
    window(c, l.enemy);
    let inner = l.enemy.inner();
    let capacity = usize::try_from((inner.h / ROW_H).max(1)).unwrap_or(1);
    let aimed = aim.and_then(|a| living.iter().position(|b| b.actor == a));
    // Scroll so the aimed-at name is on screen.
    let first = aimed.map_or(0, |i| (i + 1).saturating_sub(capacity));
    for (row, bar) in living.iter().enumerate().skip(first).take(capacity) {
        let y = inner.y + ROW_H * i32::try_from(row - first).unwrap_or(0);
        let hit = aim == Some(bar.actor);
        if hit {
            row_band(c, inner, y, AIMED);
            c.pointer(inner.x, y);
        }
        fitted(c, inner.x + POINTER_W, y, inner.w - POINTER_W, if hit { ACCENT } else { TEXT }, &bar.name);
    }
    if first > 0 {
        c.special(inner.right() - 8, inner.y, DIM, Special::ArrowUp);
    }
    if first + capacity < living.len() {
        c.special(inner.right() - 8, inner.y + ROW_H * (i32::try_from(capacity).unwrap_or(1) - 1), DIM, Special::ArrowDown);
    }
}

/// Which columns of the party panel fit in a content area `inner_w` wide, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    name_x: i32,
    name_cells: usize,
    hp_x: Option<i32>,
    mp_x: Option<i32>,
    /// The ATB bar: x and width.
    bar: Option<(i32, i32)>,
}

const NAME_CELLS: usize = 9;
const HP_W: i32 = 72;
const MP_W: i32 = 56;
const COL_GAP: i32 = 6;
const BAR_MIN: i32 = 24;
const BAR_MAX: i32 = 140;

/// Name first, then HP, then MP with the bar, or the bar alone when MP does not fit too.
fn columns(inner_x: i32, inner_w: i32) -> Columns {
    let end = inner_x + inner_w;
    let name_x = inner_x + POINTER_W;
    let name_cells = cells_in(end - name_x).clamp(1, NAME_CELLS);
    let mut x = name_x + i32::try_from(name_cells).unwrap_or(1) * 8;
    let mut cols = Columns { name_x, name_cells, hp_x: None, mp_x: None, bar: None };
    if end - (x + COL_GAP) >= HP_W {
        cols.hp_x = Some(x + COL_GAP);
        x += COL_GAP + HP_W;
        let rest = end - x;
        if rest >= COL_GAP + BAR_MIN + COL_GAP + MP_W {
            cols.mp_x = Some(x + COL_GAP);
            let bx = x + COL_GAP + MP_W + COL_GAP;
            cols.bar = Some((bx, (end - bx).min(BAR_MAX)));
        } else if rest >= COL_GAP + BAR_MIN {
            let bx = x + COL_GAP;
            cols.bar = Some((bx, (end - bx).min(BAR_MAX)));
        }
    }
    cols
}

/// A number for a column: right-aligned current value, a slash and the maximum, padded so the
/// columns of every row line up (the font has one width for every character).
fn fraction_text(now: u32, max: u32, cap: u32, width: usize) -> String {
    format!("{:>w$}/{:<w$}", now.min(cap), max.min(cap), w = width)
}

/// The party panel.
fn draw_party(c: &mut Canvas<'_>, l: &Layout, view: &BattleView, aim: Option<ActorId>) {
    window(c, l.party);
    let inner = l.party.inner();
    let cols = columns(inner.x, inner.w);
    let rows = usize::try_from((inner.h / ROW_H - 1).max(0)).unwrap_or(0);
    if let Some(x) = cols.hp_x {
        c.shadowed(x + 16, inner.y, DIM, "HP");
    }
    if let Some(x) = cols.mp_x {
        c.shadowed(x + 8, inner.y, DIM, "MP");
    }
    for (i, bar) in view.party.iter().enumerate().take(rows) {
        let y = inner.y + ROW_H * (1 + i32::try_from(i).unwrap_or(0));
        if aim == Some(bar.actor) {
            row_band(c, inner, y, AIMED);
            c.pointer(inner.x, y);
        } else if bar.mine {
            row_band(c, inner, y, HILITE);
        }
        let commanding = bar.mine && bar.ready && bar.alive;
        if commanding && aim.is_none() {
            c.pointer(inner.x, y);
        }
        let name_color = if aim == Some(bar.actor) || (bar.mine && bar.alive) {
            ACCENT
        } else if !bar.alive {
            DISABLED
        } else {
            TEXT
        };
        let name = ellipsize(&bar.name, cols.name_cells);
        c.shadowed(cols.name_x, y, name_color, &name);
        if let Some(x) = cols.hp_x {
            let color = if bar.hp == 0 { ERROR } else if bar.hp.saturating_mul(3) < bar.max_hp { HP_LOW } else { TEXT };
            c.shadowed(x, y, color, &fraction_text(bar.hp, bar.max_hp, 9999, 4));
        }
        if let Some(x) = cols.mp_x {
            let color = if bar.max_mp == 0 { DISABLED } else { INFO };
            c.shadowed(x, y, color, &fraction_text(bar.mp, bar.max_mp, 999, 3));
        }
        if let Some((x, w)) = cols.bar {
            atb_bar(c, x, y + 2, w, bar);
            // A fighter with a limit has a thin limit gauge under its ATB bar (FF7's orange one).
            if bar.limit > 0 {
                limit_bar(c, x, y + 8, w, bar);
            }
        }
    }
}

/// The ATB gauge: a dark track, the fill, and a bright edge when it is full.
fn atb_bar(c: &mut Canvas<'_>, x: i32, y: i32, w: i32, bar: &Bar) {
    let h = 4;
    c.fill(x - 1, y - 1, w + 2, h + 2, OUTLINE);
    c.fill(x, y, w, h, TRACK);
    let filled = (f64::from(w) * f64::from(gauge_fraction(bar))).round() as i32;
    if filled > 0 {
        let rgb = if bar.ready && bar.alive { ATB_READY_RGB } else { ATB_RGB };
        c.fill(x, y, filled.min(w), h, [rgb[0], rgb[1], rgb[2], 255]);
        c.fill(x, y, filled.min(w), 1, [255, 255, 255, 90]);
    }
}

/// The limit gauge (protocol v7): a thin track under the ATB bar of a party member, orange while it
/// fills and bright with a lit edge when it is full (the limit can be given). Drawn only for a member whose
/// gauge is not empty, so a character that has no limit shows nothing.
fn limit_bar(c: &mut Canvas<'_>, x: i32, y: i32, w: i32, bar: &Bar) {
    let h = 2;
    c.fill(x - 1, y - 1, w + 2, h + 2, OUTLINE);
    c.fill(x, y, w, h, TRACK);
    let filled = (f64::from(w) * f64::from(limit_fraction(bar))).round() as i32;
    if filled > 0 {
        let rgb = if limit_full(bar) { LIMIT_FULL_RGB } else { LIMIT_RGB };
        c.fill(x, y, filled.min(w), h, [rgb[0], rgb[1], rgb[2], 255]);
        if limit_full(bar) {
            c.fill(x, y, filled.min(w), 1, [255, 255, 255, 140]);
        }
    }
}

/// The command window: the four commands, the one under the cursor marked, the ones that cannot be
/// chosen greyed out.
fn draw_commands(c: &mut Canvas<'_>, l: &Layout, menu: &MenuView) {
    window(c, l.command);
    let inner = l.command.inner();
    for (i, row) in menu.commands.iter().enumerate() {
        let y = inner.y + ROW_H * i32::try_from(i).unwrap_or(0);
        let selected = i == menu.command_cursor;
        if selected {
            row_band(c, inner, y, HILITE);
            c.pointer(inner.x, y);
        }
        let color = if !row.enabled { DISABLED } else if selected { TEXT } else { SOFT };
        fitted(c, inner.x + POINTER_W, y, inner.w - POINTER_W, color, row.label);
    }
}

/// The Magic or Item list: a title row, then the visible rows with the cost or count on the right.
fn draw_list(c: &mut Canvas<'_>, l: &Layout, list: &ListView) {
    window(c, l.left);
    let inner = l.left.inner();
    c.shadowed(inner.x + POINTER_W, inner.y, ACCENT, list.title);
    let right_w = list.rows.iter().map(|r| i32::try_from(cell_count(&r.right)).unwrap_or(0) * 8).max().unwrap_or(0);
    for (i, row) in list.rows.iter().enumerate() {
        let y = inner.y + ROW_H * (1 + i32::try_from(i).unwrap_or(0));
        if y + 8 > inner.y + inner.h {
            break;
        }
        let selected = i == list.cursor;
        if selected {
            row_band(c, inner, y, HILITE);
            c.pointer(inner.x, y);
        }
        let color = if !row.enabled { DISABLED } else if selected { TEXT } else { SOFT };
        let name_w = (inner.w - POINTER_W - right_w - 22).max(8);
        fitted(c, inner.x + POINTER_W, y, name_w, color, &row.label);
        let rx = inner.right() - 14 - i32::try_from(cell_count(&row.right)).unwrap_or(0) * 8;
        c.shadowed(rx, y, color, &row.right);
    }
    let arrow_x = inner.right() - 8;
    if list.scroll > 0 {
        c.special(arrow_x, inner.y, ACCENT, Special::ArrowUp);
    }
    if list.scroll + list.rows.len() < list.total {
        c.special(arrow_x, inner.y + ROW_H * i32::try_from(super::menu::LIST_ROWS).unwrap_or(4), ACCENT, Special::ArrowDown);
    }
}

/// The bottom windows: the enemy list and the party panel always, our menu on top of them on our
/// turn. Nothing before there is anything to show.
pub fn draw_windows(c: &mut Canvas<'_>, l: &Layout, view: &BattleView) {
    if view.party.is_empty() && view.enemies.is_empty() {
        return;
    }
    let aim: Option<AimView> = view.menu.as_ref().and_then(|m| m.aim);
    draw_enemies(c, l, view, aim.filter(|a| a.enemy).map(|a| a.target));
    draw_party(c, l, view, aim.filter(|a| !a.enemy).map(|a| a.target));
    if let Some(menu) = &view.menu {
        match (menu.level, &menu.list) {
            (Level::Commands, _) => draw_commands(c, l, menu),
            (Level::Magic | Level::Items, Some(list)) => draw_list(c, l, list),
            _ => {}
        }
    }
}

/// The action banner: a small window with the action's name, under the status line.
pub fn draw_banner(c: &mut Canvas<'_>, l: &Layout, banner: &Banner) {
    let cells = i32::try_from(cell_count(&banner.text)).unwrap_or(0);
    let w = (cells * 8 + 2 * PAD).clamp(48, (l.cw - 2 * MARGIN).max(48));
    let a = Area { x: (l.cw - w) / 2, y: l.banner_y, w, h: BANNER_H };
    window(c, a);
    c.centered(a.x, a.w, a.y + (BANNER_H - 8) / 2, TEXT, &banner.text);
}

/// The target pointer: a yellow arrow over the head of whoever the menu aims at.
pub fn draw_pointer(out: &mut DrawList, l: &Layout, view: &BattleView, anchors: &Anchors, vw: u32) {
    let Some(aim) = view.menu.as_ref().and_then(|m| m.aim) else { return };
    let Some((ax, ay)) = anchors.of(view, aim.target) else { return };
    let scale = u32::try_from(l.s + 2).unwrap_or(3);
    let cell = i32::try_from(8 * scale).unwrap_or(24);
    // The arrow is drawn in the lower rows of its cell: its tip is 7/8 of the way down.
    let shadow = l.s.max(1);
    let x = (ax as i32 - cell / 2).clamp(0, (i32::try_from(vw).unwrap_or(i32::MAX) - cell - shadow).max(0));
    let y = (ay as i32 - cell * 7 / 8).max(0);
    out.draw_special(x + shadow, y + shadow, scale, SHADOW, Special::ArrowDown);
    out.draw_special(x, y, scale, ACCENT, Special::ArrowDown);
}

/// The colour and the text of a popup at `age_ms` of its life: it rises quickly and slows, then
/// fades out over the last third of its life.
fn popup_look(p: &Popup) -> (Rgba, String) {
    let color = match p.kind {
        PopupKind::Damage { critical: true, .. } => ACCENT,
        PopupKind::Damage { .. } => [255, 255, 255, 255],
        PopupKind::Heal { .. } => HEAL,
        PopupKind::Mp { .. } => MP_POP,
        PopupKind::Miss => MISS,
    };
    (color, p.kind.text())
}

/// Pixels a popup has risen after `age_ms`, in units of `s`: 20 in the first third of a second
/// (easing out), then a slow drift.
pub fn popup_rise(age_ms: u32, s: i32) -> i32 {
    let t = f64::from(age_ms) / 1000.0;
    let quick = (t / 0.35).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - quick) * (1.0 - quick);
    let drift = (t - 0.35).max(0.0) * 8.0;
    ((eased * 20.0 + drift) * f64::from(s)).round() as i32
}

/// The alpha of a popup at `age_ms`: opaque for two thirds of its life, then fading to nothing.
pub fn popup_alpha(age_ms: u32) -> u8 {
    let life = super::popups::LIFETIME_S * 1000.0;
    let fade_from = life * 0.65;
    let t = f64::from(age_ms);
    if t <= fade_from {
        255
    } else {
        (255.0 * (1.0 - (t - fade_from) / (life - fade_from)).clamp(0.0, 1.0)).round() as u8
    }
}

/// The floating numbers over the fighters they belong to (those the scene has no place for are
/// skipped). A number rises from just over its fighter's head; when another lands on the same
/// fighter before it has gone, the older ones are moved up out of the newer one's way, so no two
/// numbers over a fighter are printed over each other.
pub fn draw_popups(out: &mut DrawList, l: &Layout, view: &BattleView, anchors: &Anchors, vw: u32, vh: u32) {
    let scale = u32::try_from(l.s + 1).unwrap_or(2);
    let (vw, vh) = (i32::try_from(vw).unwrap_or(i32::MAX), i32::try_from(vh).unwrap_or(i32::MAX));
    let h = text_height(scale);
    // Per fighter: the lowest top edge the next, older popup may have (just above the newer one).
    let mut ceiling: Vec<(ActorId, i32)> = Vec::new();
    // Popups are oldest first: go through them newest first.
    for p in view.popups.iter().rev() {
        let Some((ax, ay)) = anchors.of(view, p.target) else { continue };
        let (color, text) = popup_look(p);
        let alpha = popup_alpha(p.age_ms);
        let w = text_width(&text, scale);
        // The margin holds the shadow too (it is drawn `scale` pixels down and to the right).
        let pad = i32::try_from(scale).unwrap_or(2) + 2;
        let x = (ax as i32 - w / 2).clamp(pad, (vw - w - pad).max(pad));
        let natural = ay as i32 - popup_rise(p.age_ms, l.s) - h;
        let above = ceiling.iter_mut().find(|(t, _)| *t == p.target);
        let top = above.as_ref().map_or(natural, |(_, c)| natural.min(*c));
        match above {
            Some(entry) => entry.1 = top - h - l.s,
            None => ceiling.push((p.target, top - h - l.s)),
        }
        let y = top.clamp(pad, (vh - h - pad).max(pad));
        let fade = |c: Rgba| [c[0], c[1], c[2], (u16::from(c[3]) * u16::from(alpha) / 255) as u8];
        out.draw_text_shadowed(x, y, scale, fade(color), fade(SHADOW), &text);
    }
}

/// How far (canvas units) a fighter's body reaches to either side of the point over its head, for the end-of-battle
/// panel to keep off it.
const FINALE_REACH: i32 = 16;

/// Where the end-of-battle panel goes: **hung from the top of the stage**, just under the banner (on a window too
/// small for it, as high as the room allows), and across the window in the first of three spots (centred, at the
/// left, at the right) that has the fewest of `heads` under it: the points over the heads (window pixels) of the
/// fighters that must stay in view, which are the party's after a win, standing in their victory poses. (It used
/// to be centred in the middle of the stage, where it covered the leftmost member's pose; the party stands on the
/// right of most stages, so the usual place for it after a win is over the left, where the beaten enemies were.)
/// Only the horizontal places count, so the panel does not hop when a fighter breathes under its edge.
fn finale_area(l: &Layout, w: i32, h: i32, heads: &[(f32, f32)]) -> Area {
    let (top, bottom) = (l.banner_y + BANNER_H + 4, l.enemy.y - 4);
    let spots = [(l.cw - w) / 2, MARGIN, l.cw - MARGIN - w];
    let scale = l.s.max(1) as f32;
    let covered = |x: i32| heads.iter().filter(|&&(hx, _)| hx.is_finite() && (x - FINALE_REACH) as f32 <= hx / scale && hx / scale <= (x + w + FINALE_REACH) as f32).count();
    // `min_by_key` keeps the first of the best: the centre unless another spot is clearer.
    let x = spots.iter().copied().min_by_key(|&x| covered(x)).unwrap_or(spots[0]);
    Area { x, y: top.min(bottom - h).max(0), w, h }
}

/// The victory, defeat or escaped panel, once its moment has come. `heads` are the points over the heads (window
/// pixels) of the fighters it must not cover (see [`finale_area`]): the party's after a win, none otherwise.
pub fn draw_finale(c: &mut Canvas<'_>, l: &Layout, finale: &Finale, heads: &[(f32, f32)]) {
    if finale.age_ms < PANEL_DELAY_MS {
        return;
    }
    let victory = finale.outcome == Outcome::Victory;
    let escaped = finale.outcome == Outcome::Escaped;
    let (title, title_color) = match finale.outcome {
        Outcome::Victory => ("VICTORY", ACCENT),
        Outcome::Defeat => ("DEFEAT", ERROR),
        Outcome::Escaped => ("ESCAPED", INFO),
    };
    // The body: label and value rows, or a sentence.
    let mut rows: Vec<(String, String, Rgba)> = Vec::new();
    let mut note = None;
    match (&finale.reward, victory) {
        // Nothing is paid for getting away, whoever you were.
        _ if escaped => note = Some("Got away safely."),
        (Some(r), true) => {
            rows.push(("EXP".into(), format!("+{}", r.exp), TEXT));
            rows.push(("Gil".into(), format!("+{}", r.gil), TEXT));
            rows.push(("AP".into(), format!("+{}", r.ap), TEXT));
            for item in r.items.iter().take(3) {
                rows.push((item.name.clone(), format!("x{}", item.count), HEAL));
            }
            if r.items.len() > 3 {
                rows.push((format!("and {} more", r.items.len() - 3), String::new(), DIM));
            }
        }
        (None, true) => note = Some("You watched this battle."),
        (_, false) => note = Some("Your party has fallen."),
    }
    let body_rows = i32::try_from(rows.len().max(usize::from(note.is_some()))).unwrap_or(1);
    let w = 216.min((l.cw - 2 * MARGIN).max(40));
    let h = PAD + 16 + 6 + body_rows * ROW_H + 4 + if finale.dismissible { 12 } else { 0 } + PAD;
    let a = finale_area(l, w, h, heads);
    window(c, a);
    let inner = a.inner();
    let title_w = i32::try_from(cell_count(title)).unwrap_or(0) * 16;
    c.big(inner.x + (inner.w - title_w) / 2, inner.y, title_color, title);
    let mut y = inner.y + 16 + 6;
    for (label, value, color) in &rows {
        fitted(c, inner.x, y, inner.w - 56, *color, label);
        let vw = i32::try_from(cell_count(value)).unwrap_or(0) * 8;
        c.shadowed(inner.right() - vw, y, *color, value);
        y += ROW_H;
    }
    if let Some(note) = note {
        c.centered(inner.x, inner.w, y, SOFT, note);
        y += ROW_H;
    }
    if finale.dismissible {
        c.centered(inner.x, inner.w, y + 4, DIM, "Press Enter");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::menu::{BattleMenu, BattleMenuModel, Ctx, Fighter, MenuItem, MenuKey, MenuSpell, TargetRule};
    use crate::battle::popups::LIFETIME_S;
    use crate::ui::widgets::SHADOW as SHADOW_COLOR;
    use fmv_protocol::BattleId;

    fn bar(actor: u8, name: &str, hp: u32, max_hp: u32, mp: u32, max_mp: u32, mine: bool) -> Bar {
        Bar { actor: ActorId(actor), name: name.into(), hp, max_hp, mp, max_mp, gauge: 30_000, ready: false, alive: hp > 0, mine, limit: 0 }
    }

    /// Three heroes (the second is ours) against three enemies.
    fn battle() -> BattleView {
        BattleView {
            id: BattleId(1),
            party: vec![bar(0, "Cloud", 427, 427, 59, 59, false), bar(1, "Barret", 120, 480, 8, 40, true), bar(2, "Tifa", 0, 390, 20, 50, false)],
            enemies: vec![bar(3, "Guard Hound", 55, 55, 0, 0, false), bar(4, "Mono Drive", 0, 40, 0, 0, false), bar(5, "Soldier", 80, 80, 0, 0, false)],
            status: crate::battle::view::Status::Verified,
            ..BattleView::default()
        }
    }

    fn fighters(bars: &[Bar]) -> Vec<Fighter> {
        bars.iter().map(|b| Fighter { id: b.actor, alive: b.alive }).collect()
    }

    fn menu_after(view: &BattleView, keys: &[MenuKey]) -> MenuView {
        let model = BattleMenuModel {
            spells: vec![MenuSpell::new(1, "Fire", 4, TargetRule::Enemies), MenuSpell::new(2, "Cure", 5, TargetRule::from_allies(true)), MenuSpell::new(3, "Meteor", 40, TargetRule::Enemies)],
            items: vec![MenuItem::new(10, "Potion", 3, TargetRule::Allies), MenuItem::new(11, "Phoenix Down", 2, TargetRule::DeadAllies)],
            defend: true,
            ..BattleMenuModel::default()
        };
        let (allies, enemies) = (fighters(&view.party), fighters(&view.enemies));
        let ctx = Ctx { model: &model, allies: &allies, enemies: &enemies, mp: 8 };
        let mut menu = BattleMenu::default();
        for k in keys {
            menu.press(*k, &ctx);
        }
        menu.view(&ctx)
    }

    fn draw(view: &BattleView, vw: u32, vh: u32) -> DrawList {
        let l = layout(vw, vh, view.party.len());
        let mut list = DrawList::new();
        let mut c = Canvas { out: &mut list, s: l.s };
        draw_windows(&mut c, &l, view);
        list
    }

    fn drawn(list: &DrawList) -> Vec<String> {
        list.texts().iter().filter(|t| t.color != SHADOW_COLOR && t.color[3] != 0).map(|t| t.text.clone()).collect()
    }

    #[test]
    fn the_scale_follows_the_window_and_small_windows_get_the_compact_layout() {
        assert_eq!(layout(1100, 680, 3).s, 2);
        assert_eq!(layout(1920, 1080, 3).s, 3);
        assert_eq!(layout(1280, 720, 3).s, 2);
        assert_eq!(layout(3840, 2160, 3).s, 7);
        assert_eq!(layout(320, 240, 3).s, 1);
        assert!(!layout(1100, 680, 3).compact && !layout(1920, 1080, 3).compact);
        assert!(layout(320, 240, 3).compact && layout(400, 300, 3).compact);
        // A window of nothing is still a layout.
        let l = layout(0, 0, 0);
        assert!(l.cw >= 1 && l.ch >= 1 && l.s >= 1);
    }

    #[test]
    fn the_three_bottom_windows_sit_side_by_side_without_overlap_inside_the_window() {
        for (w, h, party) in [(1100u32, 680u32, 3usize), (1920, 1080, 4), (1280, 720, 8), (800, 600, 2)] {
            let l = layout(w, h, party);
            assert!(!l.compact, "{w}x{h}");
            assert!(l.enemy.right() < l.command.x && l.command.right() < l.party.x, "{w}x{h}: enemy | command | party in that order");
            assert!(l.party.right() <= l.cw - MARGIN && l.enemy.x >= MARGIN, "{w}x{h}");
            for a in [l.enemy, l.command, l.party, l.left] {
                assert!(a.y >= 0 && a.y + a.h <= l.ch, "{w}x{h}: {a:?} is inside the canvas");
                assert_eq!(a.y, l.enemy.y, "{w}x{h}: bottom-aligned, one height");
            }
            assert_eq!((l.left.x, l.left.right()), (l.enemy.x, l.command.right()), "the lists open over the enemy list and the command window");
            let (x0, y0, x1, y1) = l.stage();
            assert!(x0 < x1 && y0 < y1 && y1 <= l.enemy.y * l.s, "{w}x{h}: the stage is above the windows");
            assert!(y0 > (l.banner_y + BANNER_H) * l.s - 1, "{w}x{h}: and below the banner");
        }
        let c = layout(320, 240, 3);
        assert_eq!((c.enemy, c.command), (c.left, c.left), "compact: the command menu covers the enemy list");
        assert!(c.left.right() < c.party.x);
    }

    #[test]
    fn the_party_panel_shows_name_hp_mp_and_the_ones_that_are_down() {
        let list = draw(&battle(), 1100, 680);
        let said = drawn(&list);
        for needle in ["HP", "MP", "Cloud", " 427/427 ", "Barret", " 120/480 ", "  8/40 ", "Tifa", "   0/390 "] {
            assert!(said.iter().any(|t| t.contains(needle)), "the party panel says {needle:?}: {said:?}");
        }
        // Our character is in the accent colour, the others plain, the one that is down dim.
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW_COLOR).map(|t| t.color);
        assert_eq!((colour_of("Cloud"), colour_of("Barret"), colour_of("Tifa")), (Some(TEXT), Some(ACCENT), Some(DISABLED)));
        // HP: plain while healthy, yellow below a third, red at zero.
        let hp = |s: &str| list.texts().iter().find(|t| t.text.contains(s) && t.color != SHADOW_COLOR).map(|t| t.color);
        assert_eq!((hp("427/427"), hp("120/480"), hp("0/390")), (Some(TEXT), Some(HP_LOW), Some(ERROR)));
    }

    #[test]
    fn the_enemy_list_names_the_living_and_leaves_out_the_dead() {
        let said = drawn(&draw(&battle(), 1100, 680));
        assert!(said.contains(&"Guard Hound".to_string()) && said.contains(&"Soldier".to_string()));
        assert!(!said.iter().any(|t| t.contains("Mono Drive")), "the dead one is gone from the list");
    }

    #[test]
    fn the_command_menu_marks_the_cursor_and_greys_out_what_cannot_be_chosen() {
        let mut v = battle();
        v.party[1].ready = true;
        v.menu = Some(menu_after(&v, &[]));
        let list = draw(&v, 1100, 680);
        let said = drawn(&list);
        for needle in ["Attack", "Magic", "Item", "Defend"] {
            assert!(said.contains(&needle.to_string()), "{needle}: {said:?}");
        }
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW_COLOR).map(|t| t.color);
        assert_eq!(colour_of("Attack"), Some(TEXT), "the cursor is on Attack");
        assert_eq!(colour_of("Magic"), Some(SOFT));
        // With no data the other commands are greyed out.
        let mut empty = battle();
        empty.party[1].ready = true;
        let ctx_model = BattleMenuModel::default();
        let (a, e) = (fighters(&empty.party), fighters(&empty.enemies));
        empty.menu = Some(BattleMenu::default().view(&Ctx { model: &ctx_model, allies: &a, enemies: &e, mp: 8 }));
        let list = draw(&empty, 1100, 680);
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW_COLOR).map(|t| t.color);
        assert_eq!((colour_of("Magic"), colour_of("Item"), colour_of("Defend")), (Some(DISABLED), Some(DISABLED), Some(DISABLED)));
        assert_eq!(colour_of("Attack"), Some(TEXT));
    }

    #[test]
    fn the_magic_list_shows_costs_and_greys_out_what_mp_cannot_pay() {
        let mut v = battle();
        v.party[1].ready = true;
        v.menu = Some(menu_after(&v, &[MenuKey::Down, MenuKey::Confirm]));
        let list = draw(&v, 1100, 680);
        let said = drawn(&list);
        for needle in ["Magic", "Fire", "MP 4", "Cure", "MP 5", "Meteor", "MP 40"] {
            assert!(said.contains(&needle.to_string()), "{needle}: {said:?}");
        }
        assert!(!said.contains(&"Attack".to_string()), "the list replaces the command window");
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW_COLOR).map(|t| t.color);
        assert_eq!((colour_of("Fire"), colour_of("Cure"), colour_of("Meteor")), (Some(TEXT), Some(SOFT), Some(DISABLED)), "40 MP is more than the 8 we have");
        // The party panel is still there, with our MP.
        assert!(said.iter().any(|t| t.contains("8/40")));
    }

    #[test]
    fn target_selection_highlights_the_name_of_the_aimed_enemy_or_ally() {
        let mut v = battle();
        v.party[1].ready = true;
        v.menu = Some(menu_after(&v, &[MenuKey::Confirm, MenuKey::Down]));
        let list = draw(&v, 1100, 680);
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW_COLOR).map(|t| t.color);
        // The enemies: Guard Hound is first, Mono Drive is dead, so one step down is the Soldier.
        assert_eq!((colour_of("Guard Hound"), colour_of("Soldier")), (Some(TEXT), Some(ACCENT)));
        assert!(!drawn(&list).contains(&"Attack".to_string()), "no command window while aiming");
        // Aiming at an ally highlights the party panel's row instead.
        v.menu = Some(menu_after(&v, &[MenuKey::Down, MenuKey::Down, MenuKey::Confirm, MenuKey::Down, MenuKey::Confirm]));
        let aim = v.menu.as_ref().and_then(|m| m.aim).expect("aiming");
        assert!(!aim.enemy, "Phoenix Down aims at the party");
        assert_eq!(aim.target, ActorId(2));
    }

    #[test]
    fn nothing_is_drawn_for_a_battle_without_fighters() {
        let list = draw(&BattleView::default(), 1100, 680);
        assert!(list.is_empty());
    }

    #[test]
    fn long_names_are_cut_and_nothing_is_drawn_outside_the_window() {
        let long = "An Extraordinarily Long Enemy Name That Cannot Fit";
        let mut v = battle();
        v.party = (0..8).map(|i| bar(i, long, 9999, 9999, 999, 999, i == 3)).collect();
        v.enemies = (8..14).map(|i| bar(i, long, 100, 100, 0, 0, false)).collect();
        v.party[3].ready = true;
        v.menu = Some(menu_after(&v, &[MenuKey::Confirm]));
        for (w, h) in [(1100u32, 680u32), (1920, 1080), (800, 600), (640, 480), (320, 240)] {
            let list = draw(&v, w, h);
            assert!(!list.is_empty());
            assert!(!list.says(long), "{w}x{h}: the full name does not fit in a window");
            for t in list.texts() {
                let width = text_width(&t.text, t.scale);
                assert!(t.x >= 0 && t.y >= 0, "{w}x{h}: {t:?}");
                assert!(t.x + width <= w as i32 && t.y + text_height(t.scale) <= h as i32, "{w}x{h}: {t:?} is cut off by the window edge");
            }
        }
    }

    #[test]
    fn a_long_list_shows_a_scroll_arrow_and_the_window_follows_the_cursor() {
        let mut v = battle();
        v.party[1].ready = true;
        let model = BattleMenuModel {
            spells: (0..10).map(|i| MenuSpell::new(i, format!("Spell{i}"), 1, TargetRule::Enemies)).collect(),
            items: vec![],
            defend: false,
            ..BattleMenuModel::default()
        };
        let (a, e) = (fighters(&v.party), fighters(&v.enemies));
        let ctx = Ctx { model: &model, allies: &a, enemies: &e, mp: 8 };
        let mut menu = BattleMenu::default();
        menu.press(MenuKey::Down, &ctx);
        menu.press(MenuKey::Confirm, &ctx);
        v.menu = Some(menu.view(&ctx));
        let arrows = |list: &DrawList, glyph: Special| {
            let uv = crate::ui::font::cell_uv(crate::ui::font::special_cell(glyph));
            list.quads().iter().filter(|q| q.uv == uv).count()
        };
        let list = draw(&v, 1100, 680);
        assert_eq!((arrows(&list, Special::ArrowDown), arrows(&list, Special::ArrowUp)), (1, 0), "more below, nothing above");
        for _ in 0..6 {
            menu.press(MenuKey::Down, &ctx);
        }
        v.menu = Some(menu.view(&ctx));
        let list = draw(&v, 1100, 680);
        assert_eq!((arrows(&list, Special::ArrowDown), arrows(&list, Special::ArrowUp)), (1, 1), "more on both sides");
        assert!(list.says("Spell6") && !list.says("Spell0"));
    }

    #[test]
    fn popups_rise_ease_out_and_fade_at_the_end_of_their_life() {
        assert_eq!(popup_rise(0, 2), 0);
        let (a, b, c) = (popup_rise(100, 2), popup_rise(350, 2), popup_rise(1000, 2));
        assert!(0 < a && a < b && b < c, "{a} {b} {c}");
        assert_eq!(b, 40, "twenty units, two pixels each, after a third of a second");
        assert!(popup_rise(175, 2) > popup_rise(350, 2) / 2, "fast at first");
        assert_eq!(popup_alpha(0), 255);
        assert_eq!(popup_alpha(700), 255);
        assert!(popup_alpha(900) < 255 && popup_alpha(900) > 0);
        assert_eq!(popup_alpha((LIFETIME_S * 1000.0) as u32), 0);
        assert_eq!(popup_alpha(u32::MAX), 0);
    }

    #[test]
    fn popups_are_drawn_over_their_fighter_in_their_colour_and_skipped_without_one() {
        let mut v = battle();
        v.popups = vec![
            Popup { target: ActorId(3), kind: PopupKind::Damage { amount: 1234, critical: false }, age_ms: 100 },
            Popup { target: ActorId(5), kind: PopupKind::Damage { amount: 77, critical: true }, age_ms: 100 },
            Popup { target: ActorId(0), kind: PopupKind::Heal { amount: 300 }, age_ms: 100 },
            Popup { target: ActorId(1), kind: PopupKind::Mp { amount: 12 }, age_ms: 100 },
            Popup { target: ActorId(4), kind: PopupKind::Miss, age_ms: 100 },
            Popup { target: ActorId(99), kind: PopupKind::Miss, age_ms: 100 },
        ];
        let anchors = crate::battle::view::flat_anchors(&v, 1100, 680);
        let l = layout(1100, 680, 3);
        let mut list = DrawList::new();
        draw_popups(&mut list, &l, &v, &anchors, 1100, 680);
        let colour_of = |text: &str| list.texts().iter().find(|t| t.text == text && t.color[..3] != SHADOW_COLOR[..3]).map(|t| t.color);
        assert_eq!(colour_of("1234"), Some([255, 255, 255, 255]), "damage is white");
        assert_eq!(colour_of("77"), Some(ACCENT), "a critical hit is gold");
        assert_eq!(colour_of("300"), Some(HEAL));
        assert_eq!(colour_of("12"), Some(MP_POP));
        assert_eq!(colour_of("Miss"), Some(MISS));
        assert_eq!(list.texts().iter().filter(|t| t.text == "Miss" && t.color[..3] != SHADOW_COLOR[..3]).count(), 1, "the popup over a fighter that is not there is skipped");
        // The number is centred over the fighter's anchor and above it.
        let (ax, ay) = anchors.of(&v, ActorId(3)).unwrap();
        let t = list.texts().iter().find(|t| t.text == "1234" && t.color[..3] != SHADOW_COLOR[..3]).unwrap();
        let (w, h) = (text_width("1234", t.scale), text_height(t.scale));
        assert!(((t.x + w / 2) as f32 - ax).abs() <= 1.0 && (t.y + h) as f32 <= ay, "{t:?} vs ({ax}, {ay})");
        // A fading popup keeps its colour and loses its alpha, the shadow too.
        v.popups = vec![Popup { target: ActorId(3), kind: PopupKind::Damage { amount: 5, critical: false }, age_ms: 1000 }];
        let mut faded = DrawList::new();
        draw_popups(&mut faded, &l, &v, &anchors, 1100, 680);
        assert!(faded.texts().iter().all(|t| t.color[3] < 255 && t.color[3] > 0), "{:?}", faded.texts());
    }

    #[test]
    fn numbers_over_one_fighter_never_overprint_and_a_popup_stays_inside_the_window() {
        let mut v = battle();
        // Three hits on the first enemy, oldest first, and one on another fighter at the same moment.
        v.popups = vec![
            Popup { target: ActorId(3), kind: PopupKind::Damage { amount: 12, critical: false }, age_ms: 300 },
            Popup { target: ActorId(3), kind: PopupKind::Damage { amount: 11, critical: false }, age_ms: 120 },
            Popup { target: ActorId(4), kind: PopupKind::Damage { amount: 99, critical: false }, age_ms: 120 },
            Popup { target: ActorId(3), kind: PopupKind::Damage { amount: 10, critical: false }, age_ms: 20 },
        ];
        let anchors = crate::battle::view::flat_anchors(&v, 1100, 680);
        let l = layout(1100, 680, 3);
        let mut list = DrawList::new();
        draw_popups(&mut list, &l, &v, &anchors, 1100, 680);
        let y_of = |s: &str| list.texts().iter().find(|t| t.text == s && t.color[..3] != SHADOW_COLOR[..3]).unwrap().y;
        let h = text_height(u32::try_from(l.s + 1).unwrap());
        assert!(y_of("10") > y_of("11") && y_of("11") > y_of("12"), "the newest is lowest, the older ones are above it");
        assert!(y_of("10") - y_of("11") >= h && y_of("11") - y_of("12") >= h, "a whole line apart at least: {} {} {}", y_of("12"), y_of("11"), y_of("10"));
        // A lone popup over another fighter is not moved by them.
        let mut alone = DrawList::new();
        let mut one = v.clone();
        one.popups = vec![v.popups[2]];
        draw_popups(&mut alone, &l, &one, &anchors, 1100, 680);
        assert_eq!(alone.texts().iter().find(|t| t.text == "99" && t.color[..3] != SHADOW_COLOR[..3]).unwrap().y, y_of("99"));
        // An anchor at the very edge, or above the window, is pulled inside it.
        let edge = Anchors { party: vec![Some((-50.0, -50.0)); 3], enemies: vec![Some((5000.0, 5000.0)); 3] };
        let mut list = DrawList::new();
        draw_popups(&mut list, &l, &v, &edge, 1100, 680);
        for t in list.texts() {
            assert!(t.x >= 0 && t.y >= 0 && t.x + text_width(&t.text, t.scale) <= 1100 && t.y + text_height(t.scale) <= 680, "{t:?}");
        }
    }

    #[test]
    fn the_target_pointer_is_a_yellow_arrow_over_the_aimed_fighter() {
        let mut v = battle();
        v.party[1].ready = true;
        v.menu = Some(menu_after(&v, &[MenuKey::Confirm]));
        let anchors = crate::battle::view::flat_anchors(&v, 1100, 680);
        let l = layout(1100, 680, 3);
        let mut list = DrawList::new();
        draw_pointer(&mut list, &l, &v, &anchors, 1100);
        let (ax, ay) = anchors.of(&v, ActorId(3)).unwrap();
        let q = list.quads().iter().find(|q| q.color == ACCENT).expect("a yellow arrow");
        assert!(((q.x + q.w / 2) as f32 - ax).abs() <= 1.0 && (q.y + q.h) as f32 >= ay - 1.0 && (q.y as f32) < ay);
        assert_eq!(list.len(), 2, "the arrow and its shadow");
        // No menu, or no aim: no pointer.
        v.menu = None;
        let mut none = DrawList::new();
        draw_pointer(&mut none, &l, &v, &anchors, 1100);
        assert!(none.is_empty());
        v.menu = Some(menu_after(&v, &[]));
        draw_pointer(&mut none, &l, &v, &anchors, 1100);
        assert!(none.is_empty());
    }

    #[test]
    fn the_banner_names_the_action_in_a_window_at_the_top() {
        let l = layout(1100, 680, 3);
        let mut list = DrawList::new();
        draw_banner(&mut Canvas { out: &mut list, s: l.s }, &l, &Banner { text: "Fire".into(), age_ms: 0 });
        assert!(list.says("Fire"));
        let t = list.texts().iter().find(|t| t.text == "Fire" && t.color == TEXT).unwrap();
        assert!(t.y < l.stage().1, "above the fighters");
        assert!((t.x + text_width("Fire", t.scale) / 2 - 550).abs() <= 2 * l.s, "centred: {t:?}");
        // A name longer than the window is cut.
        let mut cut = DrawList::new();
        draw_banner(&mut Canvas { out: &mut cut, s: l.s }, &l, &Banner { text: "x".repeat(300), age_ms: 0 });
        assert!(cut.texts().iter().all(|t| t.x >= 0 && t.x + text_width(&t.text, t.scale) <= 1100));
    }

    fn finale(outcome: Outcome, reward: Option<RewardLine>, age_ms: u32, dismissible: bool) -> Finale {
        Finale { outcome, reward, age_ms, dismissible }
    }

    #[test]
    fn the_victory_panel_lists_exp_gil_ap_and_items_after_its_delay() {
        let l = layout(1100, 680, 3);
        let reward = RewardLine { exp: 626, gil: 1_200, ap: 18, items: vec![RewardItem { name: "Potion".into(), count: 2 }] };
        let panel = |f: &Finale| {
            let mut list = DrawList::new();
            draw_finale(&mut Canvas { out: &mut list, s: l.s }, &l, f, &[]);
            list
        };
        assert!(panel(&finale(Outcome::Victory, Some(reward.clone()), PANEL_DELAY_MS - 1, true)).is_empty(), "the killing blow gets its moment first");
        let list = panel(&finale(Outcome::Victory, Some(reward), PANEL_DELAY_MS, true));
        for needle in ["VICTORY", "EXP", "+626", "Gil", "+1200", "AP", "+18", "Potion", "x2", "Press Enter"] {
            assert!(list.says(needle), "the victory panel says {needle:?}");
        }
        assert!(!panel(&finale(Outcome::Victory, Some(RewardLine::default()), 5000, false)).says("Press Enter"), "held back while it cannot be closed");
        // Watching a battle you were not in pays nothing and says so.
        assert!(panel(&finale(Outcome::Victory, None, 5000, true)).says("You watched this battle."));
        // More items than room: three, then how many more.
        let many = RewardLine { items: (0..6).map(|i| RewardItem { name: format!("Thing{i}"), count: 1 }).collect(), ..RewardLine::default() };
        let list = panel(&finale(Outcome::Victory, Some(many), 5000, true));
        assert!(list.says("Thing2") && !list.says("Thing3") && list.says("and 3 more"));
    }

    #[test]
    fn the_escaped_panel_says_so_pays_nothing_and_is_neither_a_victory_nor_a_defeat() {
        let l = layout(1100, 680, 3);
        let panel = |f: &Finale| {
            let mut list = DrawList::new();
            draw_finale(&mut Canvas { out: &mut list, s: l.s }, &l, f, &[]);
            list
        };
        assert!(panel(&finale(Outcome::Escaped, None, PANEL_DELAY_MS - 1, true)).is_empty(), "it waits like the others");
        // Whoever was watching and whatever a (hostile) server said it paid: nothing is shown as earned.
        for reward in [None, Some(RewardLine { exp: 5, gil: 5, ap: 5, items: vec![RewardItem { name: "Potion".into(), count: 1 }] })] {
            let list = panel(&finale(Outcome::Escaped, reward, 5000, true));
            assert!(list.says("ESCAPED") && list.says("Got away safely."), "{:?}", list.texts());
            assert!(!list.says("VICTORY") && !list.says("DEFEAT") && !list.says("EXP") && !list.says("Potion"), "{:?}", list.texts());
            let title = list.texts().iter().find(|t| t.text == "ESCAPED" && t.color != SHADOW_COLOR).expect("the title");
            assert_eq!(title.color, INFO);
        }
    }

    /// A quad of exactly `rgb` (opaque), anywhere: what a test of a bar looks for.
    fn has_fill(list: &DrawList, rgb: [u8; 3]) -> bool {
        list.quads().iter().any(|q| q.color == [rgb[0], rgb[1], rgb[2], 255])
    }

    #[test]
    fn a_member_with_a_limit_gauge_gets_a_thin_bar_under_its_atb_bar_that_lights_up_when_full() {
        let mut v = battle();
        let none = draw(&v, 1100, 680);
        assert!(!has_fill(&none, LIMIT_RGB) && !has_fill(&none, LIMIT_FULL_RGB), "nobody has a gauge yet: no bar");
        // Cloud's gauge is part full, Barret's (ours) full, Tifa is down with a stale one.
        v.party[0].limit = 100;
        v.party[1].limit = 255;
        v.party[2].limit = 200;
        let list = draw(&v, 1100, 680);
        assert!(has_fill(&list, LIMIT_RGB), "the filling one is orange");
        assert!(has_fill(&list, LIMIT_FULL_RGB), "the full one is bright");
        // The bar sits under the ATB bar of its own row and is as wide as it, so the fill widths compare
        // like the gauges: 100/255 of the track for Cloud, all of it for Barret.
        let quads = list.quads();
        let part = quads.iter().find(|q| q.color == [LIMIT_RGB[0], LIMIT_RGB[1], LIMIT_RGB[2], 255]).expect("Cloud's fill");
        let full = quads.iter().find(|q| q.color == [LIMIT_FULL_RGB[0], LIMIT_FULL_RGB[1], LIMIT_FULL_RGB[2], 255]).expect("Barret's fill");
        assert!(part.w > 0 && part.w < full.w, "{} against {}", part.w, full.w);
        assert!((f64::from(part.w) - f64::from(full.w) * 100.0 / 255.0).abs() <= f64::from(l_scale()), "{} of {}", part.w, full.w);
        assert!(part.y < full.y, "Cloud's row is above Barret's");
        // A fallen member shows an empty gauge whatever it says: two fills only (Cloud's and Barret's).
        let fills = quads.iter().filter(|q| q.color[..3] == LIMIT_RGB || q.color[..3] == LIMIT_FULL_RGB).count();
        assert_eq!(fills, 2, "Tifa is down: her stale gauge shows nothing");
        // Narrow windows have no room for any bar, and nothing breaks.
        for (w, h) in [(320u32, 240u32), (1, 1)] {
            let _ = draw(&v, w, h);
        }
    }

    fn l_scale() -> i32 {
        layout(1100, 680, 3).s
    }

    #[test]
    fn the_defeat_panel_says_so_in_red() {
        let l = layout(1100, 680, 3);
        let mut list = DrawList::new();
        draw_finale(&mut Canvas { out: &mut list, s: l.s }, &l, &finale(Outcome::Defeat, None, 5000, true), &[]);
        assert!(list.says("DEFEAT") && list.says("fallen") && !list.says("VICTORY"));
        let title = list.texts().iter().find(|t| t.text == "DEFEAT" && t.color != SHADOW_COLOR).unwrap();
        assert_eq!(title.color, ERROR);
    }

    #[test]
    fn the_finale_panel_sits_between_the_banner_and_the_bottom_windows_at_any_size() {
        let reward = RewardLine { exp: 99_999_999, gil: 9_999_999, ap: 99_999, items: (0..5).map(|i| RewardItem { name: format!("Item number {i} with a long name"), count: 99 }).collect() };
        for (w, h) in [(1100u32, 680u32), (1920, 1080), (800, 600), (640, 480), (320, 240)] {
            let l = layout(w, h, 3);
            let mut list = DrawList::new();
            draw_finale(&mut Canvas { out: &mut list, s: l.s }, &l, &finale(Outcome::Victory, Some(reward.clone()), 9999, true), &[]);
            assert!(!list.is_empty(), "{w}x{h}");
            for q in list.quads() {
                assert!(q.x >= 0 && q.x + q.w <= w as i32, "{w}x{h}: {q:?} is outside the window horizontally");
            }
            for t in list.texts() {
                assert!(t.x >= 0 && t.x + text_width(&t.text, t.scale) <= w as i32 && t.y >= 0 && t.y + text_height(t.scale) <= h as i32, "{w}x{h}: {t:?}");
            }
        }
    }

    #[test]
    fn the_finale_panel_hangs_from_the_top_of_the_stage_so_the_victory_poses_below_it_show() {
        for (w, h) in [(1100u32, 680u32), (1920, 1080), (1280, 720), (800, 600)] {
            let l = layout(w, h, 3);
            let (x0, top, x1, bottom) = l.stage();
            for panel_h in [60, 100, 130] {
                let a = finale_area(&l, 216, panel_h, &[]);
                // With nobody to keep clear of: centred across the window, directly under the banner (the stage
                // begins 4 units below it)...
                assert_eq!(a.x, (l.cw - 216) / 2, "{w}x{h}");
                assert_eq!(a.y, l.banner_y + BANNER_H + 4, "{w}x{h}: under the banner");
                // ... and well clear of the bottom of the stage, where the party stands: the panel ends in the
                // upper part of the stage on every window that has room for the three windows.
                let panel_bottom = (a.y + a.h) * l.s;
                assert!(panel_bottom <= top + (bottom - top) * 3 / 4, "{w}x{h} panel of {panel_h}: it ends at {panel_bottom}, the stage is {top}..{bottom}");
                assert!(x0 <= a.x * l.s && (a.x + a.w) * l.s <= x1 + 2 * l.s, "{w}x{h}");
            }
        }
        // A window with no room for it: as high as it can go, never above the window.
        let tiny = layout(320, 240, 3);
        let a = finale_area(&tiny, 216, 400, &[]);
        assert!(a.y >= 0 && a.y <= tiny.banner_y + BANNER_H + 4);
    }

    #[test]
    fn the_finale_panel_keeps_off_the_heads_of_the_victors_by_moving_to_the_side_that_is_clear() {
        let l = layout(1100, 680, 3);
        let at = |units: i32| ((units * l.s) as f32, 200.0f32);
        let (w, h) = (216, 100);
        let (centre, left, right) = ((l.cw - w) / 2, MARGIN, l.cw - MARGIN - w);
        assert!(left < centre && centre < right, "three different places");
        // Nobody under the centre: it stays there, whoever stands at the sides.
        assert_eq!(finale_area(&l, w, h, &[at(left + 10), at(right + w - 10)]).x, centre);
        // The party on the right (the usual stage): the centre's right half is theirs, so it goes left.
        let on_the_right = [at(centre + w - 20), at(centre + w + 40), at(centre + w + 90)];
        assert_eq!(finale_area(&l, w, h, &on_the_right).x, left);
        // The party on the left: it goes right (the centre is covered too; the left is worse).
        let on_the_left = [at(left + 30), at(left + 80), at(centre + 20)];
        assert_eq!(finale_area(&l, w, h, &on_the_left).x, right);
        // Somebody under every place: the one with the fewest, the centre when they tie.
        let everywhere = [at(left + 10), at(centre + 100), at(right + 100)];
        assert_eq!(finale_area(&l, w, h, &everywhere).x, centre);
        // A body reaches a little way to the side: a head just past the edge still counts.
        assert_eq!(finale_area(&l, w, h, &[at(centre + w + FINALE_REACH - 1)]).x, left);
        assert_eq!(finale_area(&l, w, h, &[at(centre + w + FINALE_REACH + 40)]).x, centre);
        // Heads that are not numbers are nobody's, and the height of a head does not matter (no hopping).
        assert_eq!(finale_area(&l, w, h, &[(f32::NAN, 0.0), (f32::INFINITY, 1.0)]).x, centre);
        let (a, b) = (finale_area(&l, w, h, &[(at(centre + 50).0, 150.0)]), finale_area(&l, w, h, &[(at(centre + 50).0, 600.0)]));
        assert_eq!((a.x, a.y), (b.x, b.y));
        // Whatever the places, the panel stays inside the window.
        for (ww, hh) in [(1100u32, 680u32), (800, 600), (320, 240)] {
            let l = layout(ww, hh, 3);
            let w = 216.min((l.cw - 2 * MARGIN).max(40));
            for x in [0, l.cw / 4, l.cw / 2, l.cw - 1] {
                let a = finale_area(&l, w, 90, &[((x * l.s) as f32, 100.0)]);
                assert!(a.x >= 0 && a.x + a.w <= l.cw.max(a.w), "{ww}x{hh}: {a:?} in a canvas of {}", l.cw);
            }
        }
    }

    #[test]
    fn the_party_columns_drop_the_least_important_first() {
        // Plenty of room: everything.
        let c = columns(0, 300);
        assert!(c.hp_x.is_some() && c.mp_x.is_some() && c.bar.is_some());
        let (bx, bw) = c.bar.unwrap();
        assert!((BAR_MIN..=BAR_MAX).contains(&bw) && bx + bw <= 300);
        assert!(c.mp_x.unwrap() > c.hp_x.unwrap() && bx > c.mp_x.unwrap(), "name, HP, MP, bar from left to right");
        // Less: MP goes, the bar stays. Less still: the bar goes. Less: HP goes too.
        let c = columns(0, 200);
        assert!(c.hp_x.is_some() && c.mp_x.is_none() && c.bar.is_some());
        let c = columns(0, 170);
        assert!(c.hp_x.is_some() && c.mp_x.is_none() && c.bar.is_none());
        let c = columns(0, 100);
        assert!(c.hp_x.is_none() && c.name_cells >= 1);
        // However little room, nothing is negative or past the end.
        for w in 0..400 {
            let c = columns(10, w);
            for x in [c.hp_x, c.mp_x].into_iter().flatten() {
                assert!(x >= 10);
            }
            if let Some((x, bw)) = c.bar {
                assert!(x + bw <= 10 + w.max(0) && bw >= BAR_MIN);
            }
        }
    }

    #[test]
    fn fractions_are_padded_to_line_up_and_capped_to_the_columns() {
        assert_eq!(fraction_text(427, 427, 9999, 4), " 427/427 ");
        assert_eq!(fraction_text(8, 40, 999, 3), "  8/40 ");
        assert_eq!(fraction_text(u32::MAX, u32::MAX, 9999, 4), "9999/9999");
        assert_eq!(cell_count(&fraction_text(0, 0, 9999, 4)), 9);
        assert_eq!(cell_count(&fraction_text(0, 0, 999, 3)), 7);
    }

    #[test]
    fn any_size_and_any_state_draws_without_panicking() {
        use crate::ui::testkit::Rng;
        let mut rng = Rng::new(7);
        for _ in 0..300 {
            let n = rng.below(9);
            let party: Vec<Bar> = (0..n).map(|i| bar(i as u8, &rng.string(30), rng.below(12_000) as u32, rng.below(12_000) as u32, rng.below(2_000) as u32, rng.below(2_000) as u32, rng.chance(3))).collect();
            let m = rng.below(8);
            let enemies: Vec<Bar> = (0..m).map(|i| bar(20 + i as u8, &rng.string(30), rng.below(100) as u32, 100, 0, 0, false)).collect();
            let mut v = BattleView { party, enemies, ..BattleView::default() };
            if rng.chance(2) && !v.party.is_empty() {
                let keys: Vec<MenuKey> = (0..rng.below(6)).map(|_| [MenuKey::Up, MenuKey::Down, MenuKey::Confirm, MenuKey::Left, MenuKey::Right][rng.below(5)]).collect();
                v.menu = Some(menu_after(&v, &keys));
            }
            if rng.chance(2) {
                v.popups = vec![Popup { target: ActorId(rng.below(30) as u8), kind: PopupKind::Miss, age_ms: rng.below(2000) as u32 }];
            }
            if rng.chance(3) {
                v.banner = Some(Banner { text: rng.string(60), age_ms: 0 });
            }
            if rng.chance(3) {
                v.finale = Some(Finale { outcome: Outcome::Victory, reward: rng.chance(2).then(RewardLine::default), age_ms: rng.below(3000) as u32, dismissible: rng.chance(2) });
            }
            let (w, h) = (rng.below(2500) as u32, rng.below(1500) as u32);
            let l = layout(w, h, v.party.len());
            let mut list = DrawList::new();
            let anchors = crate::battle::view::flat_anchors(&v, w, h);
            let mut c = Canvas { out: &mut list, s: l.s };
            draw_windows(&mut c, &l, &v);
            if let Some(b) = &v.banner {
                draw_banner(&mut c, &l, b);
            }
            if let Some(f) = &v.finale {
                let heads: Vec<(f32, f32)> = anchors.party.iter().flatten().copied().collect();
                draw_finale(&mut c, &l, f, &heads);
            }
            draw_pointer(&mut list, &l, &v, &anchors, w);
            draw_popups(&mut list, &l, &v, &anchors, w, h);
            let _ = list.vertices(w, h, false);
        }
    }

    #[test]
    fn a_window_is_drawn_with_rounded_corners() {
        let mut list = DrawList::new();
        window(&mut Canvas { out: &mut list, s: 1 }, Area { x: 10, y: 10, w: 40, h: 30 });
        // The corner unit of the border is not painted: nothing covers (10, 10).
        let covers = |px: i32, py: i32| list.quads().iter().any(|q| q.x <= px && px < q.x + q.w && q.y <= py && py < q.y + q.h && q.color != OUTLINE);
        assert!(!covers(10, 10) && !covers(49, 10) && !covers(10, 39) && !covers(49, 39), "the four corners are cut");
        assert!(covers(11, 10) && covers(10, 11) && covers(30, 25), "the edges and the inside are painted");
        // Too small to be a window: nothing.
        let mut tiny = DrawList::new();
        window(&mut Canvas { out: &mut tiny, s: 1 }, Area { x: 0, y: 0, w: 5, h: 5 });
        assert!(tiny.is_empty());
    }
}
