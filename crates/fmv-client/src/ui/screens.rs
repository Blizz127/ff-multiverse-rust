//! Drawing the pre-join screens: a [`Menu`] and a window size go in, a [`DrawList`] comes out.
//!
//! Layout is done on a "canvas" of whole canvas units, each [`ui_scale`] x [`ui_scale`] window
//! pixels, so text is always an integer multiple of the 8x8 font and every edge sits on a pixel
//! boundary. The layout is designed for a canvas of at least [`DESIGN_W`] x [`DESIGN_H`] (a 1100 x
//! 680 window is a 550 x 340 canvas at scale 2); smaller windows get a compact layout that drops
//! the optional lines instead of overlapping them.
//!
//! The look (panels, buttons, colours) is in [`super::widgets`].

use super::draw::{DrawList, Rgba};
use super::font::Special;
use super::menu::{
    list_window, look_label, Confirm, ConfirmChoice, CreateForm, Field, Menu, Notice, NoticeKind, Screen, SelectState, Target,
};
use super::palette;
use super::text::{cell_count, ellipsize, unsupported, wrap};
use super::widgets::{
    Canvas, ACCENT, BG_BOTTOM, BG_TOP, BORDER_DIM, BUTTON_H, DIM, DIMMER, ERROR, HILITE, INFO, SOFT, TEXT, TRACK, WARN,
};
use fmv_core::GameId;
use fmv_protocol::Secret;

/// The canvas the layout is designed for (canvas units).
pub const DESIGN_W: u32 = 400;
pub const DESIGN_H: u32 = 270;
/// Largest integer scale used (a 4K window gets 8).
pub const MAX_UI_SCALE: u32 = 8;

/// The scene the menus sit on: nothing (the renderer clears its background, the menu paints over it).
pub fn menu_scene() -> crate::render::Scene<'static> {
    use fmv_adapter_api::Bounds;
    use fmv_core::Pos;
    crate::render::Scene {
        bounds: Bounds { min: Pos::new(-1.0, -1.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) },
        triangles: &[],
        players: &[],
        fixture_banner: false,
        backdrop: None,
    }
}

/// How far the content of a screen moves down in a canvas taller than the design: a third of the
/// spare height, so it sits a little above the middle instead of hugging the top.
fn spare_height(ch: i32) -> i32 {
    (ch - DESIGN_H as i32).max(0) / 3
}

/// The integer scale of a `w` x `h` window: the largest one that still leaves the design canvas.
pub fn ui_scale(w: u32, h: u32) -> u32 {
    (w / DESIGN_W).min(h / DESIGN_H).clamp(1, MAX_UI_SCALE)
}

fn game_line(game: GameId) -> (String, Rgba) {
    match game {
        GameId::Fixture => ("Game: FIXTURE (synthetic, not FF7)".to_string(), WARN),
        GameId::Ff7 => ("Game: FF7 (reading your own copy, read-only)".to_string(), DIM),
        other => (format!("Game: {}", other.label()), DIM),
    }
}

/// How many cells a hint "Key  Label" takes.
fn hint_cells(item: &(&str, &str)) -> usize {
    cell_count(item.0) + 1 + cell_count(item.1)
}

/// The hint items broken into rows that fit `width` canvas units (each row as many as fit; an item is never torn apart).
fn hint_rows<'a>(width: i32, items: &[(&'a str, &'a str)]) -> Vec<Vec<(&'a str, &'a str)>> {
    let max_cells = (width / 8).max(8) as usize;
    let mut rows: Vec<Vec<(&str, &str)>> = vec![Vec::new()];
    let mut used = 0_usize;
    for item in items {
        let own = hint_cells(item);
        let gap = if used == 0 { 0 } else { 3 };
        if used > 0 && used + gap + own > max_cells {
            rows.push(Vec::new());
            used = own;
        } else {
            used += gap + own;
        }
        rows.last_mut().expect("a row").push(*item);
    }
    rows
}

/// A "Key  Label" hint row, centred; wraps to a second row when it is too wide. Returns the y
/// after the last row.
fn hints(c: &mut Canvas<'_>, left: i32, width: i32, y: i32, items: &[(&str, &str)]) -> i32 {
    let cells = hint_cells;
    let mut y = y;
    for row in hint_rows(width, items) {
        let total: usize = row.iter().map(cells).sum::<usize>() + 3 * row.len().saturating_sub(1);
        let mut x = left + (width - total as i32 * 8) / 2;
        for (i, (key, label)) in row.iter().enumerate() {
            if i > 0 {
                x += 24;
            }
            x += c.shadowed(x, y, ACCENT, key) + 8;
            x += c.shadowed(x, y, TEXT, label);
        }
        y += 11;
    }
    y
}

/// The lines of a notice (message, then hint) wrapped for a block `width` canvas units wide, and
/// how many of them belong to the message.
fn notice_lines(notice: &Notice, width: i32) -> (Vec<String>, usize) {
    let cells = ((width - 8) / 8).max(4) as usize;
    let mut lines = wrap(&notice.text, cells);
    let message = lines.len();
    if let Some(hint) = &notice.hint {
        lines.extend(wrap(hint, cells));
    }
    (lines, message)
}

/// The message of the menu as wrapped lines in `[left, left + width)`, a coloured bar at its left,
/// at most `max_lines` lines (the last one cut with `...`). Returns the y after it.
fn draw_notice(c: &mut Canvas<'_>, left: i32, width: i32, y: i32, max_lines: usize, notice: &Notice) -> i32 {
    let color = match notice.kind {
        NoticeKind::Error => ERROR,
        NoticeKind::Warning => WARN,
        NoticeKind::Info => INFO,
    };
    let cells = ((width - 8) / 8).max(4) as usize;
    let (mut lines, hint_start) = notice_lines(notice, width);
    if lines.len() > max_lines {
        lines.truncate(max_lines.max(1));
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(&format!("{last} ..."), cells);
        }
    }
    let h = lines.len() as i32 * 10;
    c.fill(left, y, 2, h - 2, color);
    for (i, line) in lines.iter().enumerate() {
        let tone = if i >= hint_start { TEXT } else { color };
        c.shadowed(left + 8, y + i as i32 * 10, tone, line);
    }
    y + h
}

/// Where the text of the name field starts so that the cursor stays inside a box of `cells`
/// characters: the whole text when it fits, else a window that slides along with the cursor.
pub fn entry_window(len: usize, cursor: usize, cells: usize) -> usize {
    if cells == 0 || len < cells {
        return 0;
    }
    // One extra cell for the cursor after the last character.
    cursor.saturating_sub(cells - 1).min(len + 1 - cells)
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Draw the whole menu for a `w` x `h` window (the renderer has already cleared the target; the
/// menu paints its own background).
pub fn draw_menu(menu: &Menu, w: u32, h: u32, out: &mut DrawList) {
    out.vertical_gradient(0, 0, w as i32, h as i32, [BG_TOP, BG_BOTTOM], 64);
    let s = ui_scale(w, h) as i32;
    let (cw, ch) = (w as i32 / s, h as i32 / s);
    let mut c = Canvas { out, s };
    if let Some(notice) = menu.notice().filter(|n| n.secret.is_some()) {
        draw_alert(&mut c, cw, ch, notice);
        return;
    }
    match menu.screen() {
        Screen::Select(state) => draw_select(&mut c, menu, state, cw, ch),
        Screen::Create(form) => draw_create(&mut c, menu, form, cw, ch),
        Screen::Connecting(connecting) => {
            let elapsed = (menu.clock() - connecting.since).max(0.0);
            draw_connecting(&mut c, menu, &connecting.target, elapsed, cw, ch);
        }
        Screen::Loading(what) => draw_loading(&mut c, menu, what, cw, ch),
    }
}

/// The footer every screen has: which game's content this is.
fn draw_game_line(c: &mut Canvas<'_>, menu: &Menu, cw: i32, ch: i32) {
    let (text, color) = game_line(menu.config().game);
    let text = ellipsize(&text, (cw / 8 - 2).max(4) as usize);
    c.centered(0, cw, ch - 12, color, &text);
}

fn server_line(menu: &Menu, cells: usize) -> String {
    ellipsize(&format!("Server: {}", menu.config().server), cells)
}

fn title(c: &mut Canvas<'_>, cw: i32, y: i32, text: &str) {
    let w = cell_count(text) as i32 * 16;
    c.big((cw - w) / 2, y, ACCENT, text);
}

// ---------------------------------------------------------------------------------------------
// Select
// ---------------------------------------------------------------------------------------------

const ROW_H: i32 = 20;

fn draw_select(c: &mut Canvas<'_>, menu: &Menu, state: &SelectState, cw: i32, ch: i32) {
    let pw = (cw - 16).min(380);
    let px = (cw - pw) / 2;
    let compact = ch < DESIGN_H as i32;
    // On a window taller than the design, a third of the spare room goes above the content.
    let lift = spare_height(ch);
    let mut y = if compact { 6 } else { 10 } + lift;
    title(c, cw, y, "FF MULTIVERSE");
    y += 20;
    if !compact {
        c.centered(0, cw, y, DIM, "Choose your character");
        y += 12;
    }
    c.centered(0, cw, y, DIM, &server_line(menu, (pw / 8) as usize));
    y += 14;

    // The room below the list: hints (up to two rows), the message, the game line.
    let message_lines = menu.notice().map_or(0, |n| notice_lines(n, pw).0.len().min(4));
    let below = 24 + message_lines as i32 * 10 + 8 + 14;
    let rows = (((ch - y - below - 8) / ROW_H).clamp(1, 8)) as usize;
    let count = menu.accounts().len();
    let top = list_window(state.cursor, count, rows);
    let shown = rows.min(count);
    let ph = shown as i32 * ROW_H + 8;
    c.panel(px, y, pw, ph);

    for (slot, index) in (top..count).take(rows).enumerate() {
        let account = &menu.accounts()[index];
        let ry = y + 4 + slot as i32 * ROW_H;
        if index == state.cursor {
            c.fill(px + 3, ry, pw - 6, ROW_H, HILITE);
            c.pointer(px + 8, ry + 6);
        }
        match &account.look {
            Some(look) => c.swatch(px + 22, ry + 4, 12, look.tint),
            None => {
                c.swatch(px + 22, ry + 4, 12, [74, 80, 118]);
                c.text(px + 24, ry + 6, DIM, "?");
            }
        }
        let name_x = px + 42;
        // The right edge of the text; a warning mark takes the last cell when the login's file
        // is readable by others.
        let right = px + pw - 8 - if account.loose_permissions { 12 } else { 0 };
        let avail = ((right - name_x) / 8).max(4) as usize;
        let look_text = match &account.look {
            Some(look) => look_label(look),
            None => "look unknown".to_string(),
        };
        let name_cells = cell_count(&account.name).min(avail);
        let look_cells = avail.saturating_sub(name_cells + 1).min(cell_count(&look_text));
        let look_shown = if look_cells >= 6 { ellipsize(&look_text, look_cells) } else { String::new() };
        let name_room = if look_shown.is_empty() { avail } else { avail - cell_count(&look_shown) - 1 };
        let name_tone = if index == state.cursor { TEXT } else { SOFT };
        c.shadowed(name_x, ry + 6, name_tone, &ellipsize(&account.name, name_room));
        if !look_shown.is_empty() {
            let lx = right - cell_count(&look_shown) as i32 * 8;
            c.shadowed(lx, ry + 6, DIM, &look_shown);
        }
        if account.loose_permissions {
            c.shadowed(px + pw - 16, ry + 6, WARN, "!");
        }
    }
    if top > 0 {
        c.special(px + pw - 14, y + 3, ACCENT, Special::ArrowUp);
    }
    if top + rows < count {
        c.special(px + pw - 14, y + ph - 11, ACCENT, Special::ArrowDown);
    }
    y += ph + 6;

    y = hints(c, 8, cw - 16, y, &[("Enter", "Play"), ("N", "New character"), ("Del", "Delete"), ("Esc", "Quit")]);
    y += 2;
    let room = ((ch - 16 - y) / 10).max(1) as usize;
    if let Some(notice) = menu.notice() {
        y = draw_notice(c, px, pw, y, room, notice);
    } else if let Some(account) = menu.accounts().get(state.cursor).filter(|a| a.loose_permissions) {
        let n = Notice::warning(format!("The saved login of {} can be read by other users on this computer.", account.name))
            .with_hint("Fix it with: chmod 600 on its file in the accounts folder.");
        y = draw_notice(c, px, pw, y, room, &n);
    }
    let _ = y;
    draw_game_line(c, menu, cw, ch);

    if let Some(confirm) = state.confirm {
        draw_confirm(c, menu, confirm, cw, ch);
    }
}

/// What the question says deleting does, in plain words: the character and its progress go from the server and its name is
/// free again, and the server keeps a copy that only its operator can bring back.
pub const CONFIRM_DELETE_TEXT: &str =
    "The character and its progress are deleted from the server, and its name is free again. The server keeps a recoverable copy that only its operator can restore.";
/// What the question says forgetting does: only this computer's saved login goes, the character stays where it is.
pub const CONFIRM_FORGET_TEXT: &str =
    "Forget only removes the saved login from this computer: the character stays on the server and its name stays taken.";
/// The keys of the question, in the order of its buttons: delete, forget on this computer only, keep.
const CONFIRM_KEYS: [(&str, &str); 3] = [("Y", "delete"), ("F", "forget on this computer only"), ("N or Esc", "keep")];

fn draw_confirm(c: &mut Canvas<'_>, menu: &Menu, confirm: Confirm, cw: i32, ch: i32) {
    c.fill(0, 0, cw, ch, DIMMER);
    let w = (cw - 24).min(300);
    let x = (cw - w) / 2;
    let cells = ((w - 16) / 8) as usize;
    let name = menu.accounts().get(confirm.index).map_or("?", |a| a.name.as_str());
    let delete_text = wrap(CONFIRM_DELETE_TEXT, cells);
    let forget_text = wrap(CONFIRM_FORGET_TEXT, cells);
    let key_rows = hint_rows(w - 16, &CONFIRM_KEYS).len() as i32;
    // The rows of the panel, top to bottom: the question and the name, what each answer does, the buttons, the keys.
    let text_h = (delete_text.len() + forget_text.len()) as i32 * 10 + 4;
    let h = 36 + text_h + 4 + BUTTON_H + 8 + key_rows * 11 + 6;
    let y = ((ch - h) / 2).max(4);
    c.panel(x, y, w, h);
    c.centered(x, w, y + 8, ACCENT, "Delete this character?");
    c.centered(x, w, y + 20, TEXT, &ellipsize(name, cells));
    let mut ty = y + 36;
    for line in &delete_text {
        c.shadowed(x + 8, ty, DIM, line);
        ty += 10;
    }
    ty += 4;
    for line in &forget_text {
        c.shadowed(x + 8, ty, DIM, line);
        ty += 10;
    }
    // The buttons, each as wide as its label needs (with room to spare where the panel has it), side by side.
    let buttons = [("Keep", ConfirmChoice::Keep), ("Delete", ConfirmChoice::Delete), ("Forget here", ConfirmChoice::Forget)];
    let gap = 6;
    let widths = |pad: i32| buttons.map(|(label, _)| cell_count(label) as i32 * 8 + pad);
    let pad = if widths(16).iter().sum::<i32>() + 2 * gap <= w - 16 { 16 } else { 8 };
    let widths = widths(pad);
    let by = ty + 4;
    let mut bx = x + (w - (widths.iter().sum::<i32>() + 2 * gap)) / 2;
    for ((label, choice), bw) in buttons.iter().zip(widths) {
        c.button(bx, by, bw, label, confirm.focus == *choice);
        bx += bw + gap;
    }
    hints(c, x + 8, w - 16, by + BUTTON_H + 8, &CONFIRM_KEYS);
}

// ---------------------------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------------------------

/// The width of the label column, in canvas units: wide enough for "Fighting style", the longest label.
const LABEL_W: i32 = 112;

/// The widest the create form gets, in canvas units: wide enough for the help line of the Body row on one
/// line (at the design width of 400 it is the width of the canvas less a margin).
const CREATE_W: i32 = 440;
/// What the Body row means, in one short line.
pub const BODY_HELP: &str = "how you look on the field (a placeholder model)";
/// What the Fighting style row means, in one short line (wrapped where the form is narrower than it is long).
pub const SLOT_HELP: &str = "how you fight: its stats, spells and level curve (fixed once created)";
/// Height of one line of help text, in canvas units, and the space left under a block of them.
const HELP_LINE_H: i32 = 10;
const HELP_PAD: i32 = 2;

/// The height of a block of `lines` help lines.
fn help_block_h(lines: usize) -> i32 {
    lines as i32 * HELP_LINE_H + HELP_PAD
}

/// A help text wrapped to `cells`: on one line when it fits; otherwise the part before its parenthesis and the
/// parenthesis, each on lines of their own, so that what is in brackets ("(fixed once created)") is not torn apart
/// when it fits a line by itself. Read back with a space between the lines, it is the text.
fn wrap_help(text: &str, cells: usize) -> Vec<String> {
    if cell_count(text) <= cells {
        return vec![text.to_string()];
    }
    match text.split_once(" (") {
        Some((head, tail)) => {
            let mut lines = wrap(head, cells);
            lines.extend(wrap(&format!("({tail}"), cells));
            lines
        }
        None => wrap(text, cells),
    }
}

/// The help text of a row, when it has one.
pub fn field_help(field: Field) -> Option<&'static str> {
    match field {
        Field::Body => Some(BODY_HELP),
        Field::Slot => Some(SLOT_HELP),
        _ => None,
    }
}

/// Where the rows of the create form sit.
struct FormRows {
    px: i32,
    pw: i32,
    label_x: i32,
}

impl FormRows {
    /// A row's label; the focused row also gets the pointer and a highlight band.
    fn draw(&self, c: &mut Canvas<'_>, focused: bool, ry: i32, h: i32, label: &str) {
        if focused {
            c.fill(self.px + 3, ry, self.pw - 6, h, HILITE);
            c.pointer(self.px + 5, ry + (h - 8) / 2);
        }
        c.shadowed(self.label_x, ry + (h - 8) / 2, if focused { ACCENT } else { TEXT }, label);
    }

    /// The help lines under a row (the band of a focused row goes on under them), `lines` of them, `ry` is
    /// where they start. Returns the y after them.
    fn help(&self, c: &mut Canvas<'_>, focused: bool, ry: i32, lines: &[String]) -> i32 {
        let h = help_block_h(lines.len());
        if focused {
            c.fill(self.px + 3, ry, self.pw - 6, h, HILITE);
        }
        for (i, line) in lines.iter().enumerate() {
            c.shadowed(self.label_x, ry + 1 + i as i32 * HELP_LINE_H, DIM, line);
        }
        ry + h
    }
}

/// A row that picks one of a few: an arrow each side of the choice, which is centred between them
/// (19 cells wide: the longest full name, Aerith Gainsborough). The arrows are yellow on the focused row.
fn picker(c: &mut Canvas<'_>, x: i32, ty: i32, focused: bool, label: &str) {
    let arrow = if focused { ACCENT } else { BORDER_DIM };
    c.special(x, ty, arrow, Special::ArrowLeft);
    c.centered(x + 12, 19 * 8, ty, TEXT, label);
    c.special(x + 12 + 19 * 8 + 4, ty, arrow, Special::ArrowRight);
}

fn draw_create(c: &mut Canvas<'_>, menu: &Menu, form: &CreateForm, cw: i32, ch: i32) {
    let pw = (cw - 8).min(CREATE_W);
    let px = (cw - pw) / 2;
    let compact = ch < DESIGN_H as i32;
    let row_h = if compact { 14 } else { 18 };
    let slider_h = if compact { 10 } else { 12 };
    let grid_cell = if compact { 11 } else { 12 };
    let grid_h = 2 * (grid_cell + 3) + 2;
    let label_x = px + 14;
    let ctl_x = label_x + LABEL_W + 4;
    let (bodies, slots) = (form.has_bodies(), form.has_slots());

    // What each row means, wrapped to the room under its label. Where the window has the room for all of it
    // the lines sit under their rows; where it has not, the focused row's help takes the place of the
    // key hints under the buttons, so the meaning of a row is never lost to a small window.
    let help_cells = ((px + pw - 8 - label_x) / 8).max(8) as usize;
    let body_help = if bodies { wrap_help(BODY_HELP, help_cells) } else { Vec::new() };
    let slot_help = if slots { wrap_help(SLOT_HELP, help_cells) } else { Vec::new() };
    let rows_h = 8
        + row_h
        + if bodies { row_h } else { 0 }
        + grid_h
        + 3 * slider_h
        + if slots { row_h } else { 0 }
        + if compact { 0 } else { row_h }
        + 4;
    let helps_h = if bodies { help_block_h(body_help.len()) } else { 0 } + if slots { help_block_h(slot_help.len()) } else { 0 };
    let top = if compact { 4 } else { 8 };
    // Title, server line, the panel, buttons, key hints, a message if there is one (a few lines) and the game
    // line: what a window must hold for the help lines to sit under their rows.
    let notice_h = menu.notice().map_or(0, |n| notice_lines(n, pw).0.len().min(3) as i32 * 10);
    let chrome = top + 20 + if compact { 0 } else { 14 } + 6 + 20 + 12 + notice_h + 14;
    let helps = !compact && ch >= chrome + rows_h + helps_h;
    let panel_h = rows_h + if helps { helps_h } else { 0 };
    // A third of the spare height goes above the content (never more than there is).
    let lift = spare_height(ch).min((ch - chrome - panel_h).max(0) / 3);
    let mut y = top + lift;
    title(c, cw, y, "NEW CHARACTER");
    y += 20;
    if !compact {
        c.centered(0, cw, y, DIM, &server_line(menu, (pw / 8) as usize));
        y += 14;
    }

    c.panel(px, y, pw, panel_h);
    let mut ry = y + 4;

    let rows = FormRows { px, pw, label_x };
    let row = |c: &mut Canvas<'_>, field: Field, ry: i32, h: i32, label: &str| {
        rows.draw(c, form.focus == field, ry, h, label);
    };

    // Name.
    row(c, Field::Name, ry, row_h, "Name");
    let max_cells = (((px + pw - 8 - 48) - ctl_x - 8) / 8).clamp(4, 24);
    let box_w = max_cells * 8 + 8;
    let by = ry + (row_h - 12) / 2;
    c.fill(ctl_x, by, box_w, 12, if form.focus == Field::Name { ACCENT } else { BORDER_DIM });
    c.fill(ctl_x + 1, by + 1, box_w - 2, 10, TRACK);
    let text = form.name.text();
    let start = entry_window(form.name.len(), form.name.cursor(), max_cells as usize);
    let visible: String = text.chars().skip(start).take(max_cells as usize).collect();
    c.text(ctl_x + 4, by + 2, TEXT, &visible);
    if form.focus == Field::Name && ((menu.clock() * 2.0) as i32) % 2 == 0 {
        let cx = ctl_x + 4 + (form.name.cursor() - start) as i32 * 8;
        c.fill(cx, by + 1, 1, 10, ACCENT);
    }
    let count = format!("{}/{}", form.name.len(), form.name.max());
    c.text(ctl_x + box_w + 6, ry + (row_h - 8) / 2, DIM, &count);
    ry += row_h;

    // Body: how the character looks on the field.
    if bodies {
        row(c, Field::Body, ry, row_h, "Body");
        let options = menu.bodies();
        let label = options.get(form.body).map_or("?", |o| o.label);
        picker(c, ctl_x, ry + (row_h - 8) / 2, form.focus == Field::Body, label);
        ry += row_h;
        if helps {
            ry = rows.help(c, form.focus == Field::Body, ry, &body_help);
        }
    }

    // Colour: the palette grid.
    row(c, Field::Palette, ry, grid_h, "Color");
    for index in 0..palette::PALETTE.len() {
        let (col, grow) = palette::cell(index);
        let gx = ctl_x + col as i32 * (grid_cell + 3);
        let gy = ry + 2 + grow as i32 * (grid_cell + 3);
        c.swatch(gx, gy, grid_cell, palette::PALETTE[index]);
        if index == form.palette_cursor && form.focus == Field::Palette {
            c.frame(gx - 2, gy - 2, grid_cell + 4, grid_cell + 4, [255, 255, 255, 255]);
        } else if index == form.palette_cursor && palette::index_of(form.tint) == Some(index) {
            c.frame(gx - 2, gy - 2, grid_cell + 4, grid_cell + 4, BORDER_DIM);
        }
    }
    ry += grid_h;

    // Colour: the sliders.
    let bar_w = 128;
    let channels = [
        (Field::Red, "Red", 0_usize, [232, 72, 72, 255]),
        (Field::Green, "Green", 1, [84, 212, 92, 255]),
        (Field::Blue, "Blue", 2, [88, 124, 244, 255]),
    ];
    for (field, label, channel, tone) in channels {
        row(c, field, ry, slider_h, label);
        let value = i32::from(form.tint[channel]);
        let bar_y = ry + (slider_h - 6) / 2;
        c.fill(ctl_x - 1, bar_y - 1, bar_w + 2, 8, BORDER_DIM);
        c.fill(ctl_x, bar_y, bar_w, 6, TRACK);
        c.fill(ctl_x, bar_y, value * bar_w / 255, 6, tone);
        let thumb = ctl_x + value * (bar_w - 2) / 255;
        c.fill(thumb, bar_y - 2, 2, 10, if form.focus == field { ACCENT } else { TEXT });
        c.text(ctl_x + bar_w + 8, ry + (slider_h - 8) / 2, if form.focus == field { TEXT } else { DIM }, &format!("{value:>3}"));
        ry += slider_h;
    }

    // Fighting style: how the character fights (the stat curve of one of the game's characters; never who it is).
    if slots {
        row(c, Field::Slot, ry, row_h, "Fighting style");
        let options = menu.slots();
        let label = options.get(form.slot).map_or("?", |o| o.label);
        picker(c, ctl_x, ry + (row_h - 8) / 2, form.focus == Field::Slot, label);
        ry += row_h;
        if helps {
            ry = rows.help(c, form.focus == Field::Slot, ry, &slot_help);
        }
    }

    // Preview: how the others will see the character.
    if !compact {
        c.fill(px + 8, ry + 1, pw - 16, 1, BORDER_DIM);
        c.shadowed(label_x, ry + (row_h - 8) / 2 + 1, DIM, "Preview");
        c.swatch(ctl_x, ry + (row_h - 12) / 2 + 1, 12, form.tint);
        let shown = form.name.name();
        let (name_text, tone) = match &shown {
            Some(n) => (n.clone(), TEXT),
            None => ("(your name)".to_string(), DIM),
        };
        let cells = (((px + pw - 8 - 56) - (ctl_x + 20)) / 8).max(4) as usize;
        c.shadowed(ctl_x + 20, ry + (row_h - 8) / 2 + 1, tone, &ellipsize(&name_text, cells));
        let hex = format!("#{}", palette::hex(form.tint));
        c.text(px + pw - 8 - 56, ry + (row_h - 8) / 2 + 1, DIM, &hex);
    }
    y += panel_h + if compact { 4 } else { 6 };

    // Buttons.
    let bw = 88;
    let quit_label = if menu.accounts().is_empty() { "Quit" } else { "Back" };
    c.button(cw / 2 - bw - 6, y, bw, "Create", form.focus == Field::Create);
    c.button(cw / 2 + 6, y, bw, quit_label, form.focus == Field::Back);
    y += 20;
    // Under the buttons: the meaning of the focused row when the window had no room to say it under the rows,
    // else the key hints (which the compact layout leaves out).
    match field_help(form.focus).filter(|_| !helps && (bodies || slots)) {
        Some(help) => {
            for line in wrap_help(help, (cw / 8 - 2).max(8) as usize) {
                c.centered(0, cw, y, DIM, &line);
                y += HELP_LINE_H;
            }
        }
        None if !compact => {
            c.centered(0, cw, y, DIM, "Tab: next   Arrows: change   Enter: create");
            y += 12;
        }
        None => {}
    }
    if unsupported(&form.name.text()) > 0 && !compact {
        c.centered(0, cw, y, DIM, "Characters the font lacks show as boxes.");
        y += 10;
    }
    if let Some(notice) = menu.notice() {
        let room = ((ch - 14 - y) / 10).max(1) as usize;
        draw_notice(c, px, pw, y, room, notice);
    }
    draw_game_line(c, menu, cw, ch);
}

// ---------------------------------------------------------------------------------------------
// Connecting, loading, alert
// ---------------------------------------------------------------------------------------------

fn spinner(clock: f32) -> &'static str {
    ["|", "/", "-", "\\"][((clock * 8.0) as usize) % 4]
}

fn draw_connecting(c: &mut Canvas<'_>, menu: &Menu, target: &Target, elapsed: f32, cw: i32, ch: i32) {
    let w = (cw - 16).min(340);
    let x = (cw - w) / 2;
    let h = 70;
    let y = (ch - h) / 2;
    title(c, cw, y.saturating_sub(34).max(4), "FF MULTIVERSE");
    c.panel(x, y, w, h);
    let cells = ((w - 16) / 8) as usize;
    let (head, line, leave) = match target {
        Target::Login { name } => ("Logging in".to_string(), format!("as {name}"), "Esc: cancel"),
        Target::Register { name } => ("Creating your character".to_string(), name.clone(), "Esc: cancel"),
        // The request is already with the server: Esc only ends the wait.
        Target::Delete { name } => ("Deleting this character".to_string(), name.clone(), "Esc: stop waiting"),
    };
    c.centered(x, w, y + 8, ACCENT, &format!("{} {}", spinner(menu.clock()), head));
    c.centered(x, w, y + 22, TEXT, &ellipsize(&line, cells));
    c.centered(x, w, y + 36, DIM, &ellipsize(&format!("on {}   {} s", menu.config().server, elapsed as u32), cells));
    c.centered(x, w, y + 54, DIM, leave);
    draw_game_line(c, menu, cw, ch);
}

fn draw_loading(c: &mut Canvas<'_>, menu: &Menu, what: &str, cw: i32, ch: i32) {
    let w = (cw - 16).min(340);
    let x = (cw - w) / 2;
    let h = 44;
    let y = (ch - h) / 2;
    title(c, cw, y.saturating_sub(34).max(4), "FF MULTIVERSE");
    c.panel(x, y, w, h);
    c.centered(x, w, y + 9, ACCENT, "Joined!");
    let cells = ((w - 16) / 8) as usize;
    c.centered(x, w, y + 24, TEXT, &ellipsize(&format!("Loading {what} {}", spinner(menu.clock())), cells));
    draw_game_line(c, menu, cw, ch);
}

/// A notice that carries a secret (a token that could not be saved) takes the whole screen: it
/// must be read and copied, and nothing may cover it.
fn draw_alert(c: &mut Canvas<'_>, cw: i32, ch: i32, notice: &Notice) {
    let w = (cw - 16).min(380);
    let x = (cw - w) / 2;
    let cells = ((w - 16) / 8) as usize;
    let mut lines = wrap(&notice.text, cells);
    if let Some(hint) = &notice.hint {
        lines.extend(wrap(hint, cells));
    }
    let secret = notice.secret.as_ref().map_or("", Secret::expose);
    let secret_lines = wrap(secret, cells);
    let footer = wrap("It was also printed on the terminal. Press Enter to continue.", cells);
    let h = 8 + 12 + lines.len() as i32 * 10 + 8 + 10 + secret_lines.len() as i32 * 10 + 8 + footer.len() as i32 * 10 + 6;
    let y = ((ch - h) / 2).max(2);
    c.panel(x, y, w, h);
    let mut ty = y + 8;
    c.centered(x, w, ty, ERROR, "! SAVE THIS TOKEN NOW !");
    ty += 12;
    for line in &lines {
        c.shadowed(x + 8, ty, TEXT, line);
        ty += 10;
    }
    ty += 8;
    c.shadowed(x + 8, ty, ACCENT, "Token (shown only once):");
    ty += 10;
    for line in &secret_lines {
        c.fill(x + 6, ty - 1, w - 12, 10, [0, 0, 0, 160]);
        c.text(x + 8, ty, [255, 255, 255, 255], line);
        ty += 10;
    }
    ty += 8;
    for line in &footer {
        c.centered(x, w, ty, DIM, line);
        ty += 10;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::draw::DrawnText;
    use crate::ui::widgets::SHADOW;
    use crate::ui::menu::{CharacterEntry, KeyPress, Look, MenuAction, MenuConfig, NavKey};

    const SIZES: [(u32, u32); 9] =
        [(320, 240), (400, 270), (480, 270), (640, 360), (800, 600), (1100, 680), (1280, 720), (1920, 1080), (3840, 2160)];

    fn cfg(game: GameId) -> MenuConfig {
        MenuConfig::new("127.0.0.1:47770", game)
    }

    fn entry(name: &str, tint: Option<[u8; 3]>, look: Option<&str>) -> CharacterEntry {
        CharacterEntry {
            name: name.into(),
            look: tint.map(|t| Look::plain(t, look)),
            loose_permissions: false,
        }
    }

    fn sample_accounts() -> Vec<CharacterEntry> {
        vec![
            entry("Cloud", Some([90, 125, 245]), Some("ff7/cloud")),
            entry("Tifa", Some([235, 80, 80]), Some("ff7/tifa")),
            entry("Red XIII", None, None),
        ]
    }

    fn draw(menu: &Menu, w: u32, h: u32) -> DrawList {
        let mut list = DrawList::new();
        draw_menu(menu, w, h, &mut list);
        list
    }

    fn rect_of(t: &DrawnText) -> (i32, i32, i32, i32) {
        let w = cell_count(&t.text) as i32 * 8 * t.scale as i32;
        (t.x, t.y, t.x + w, t.y + 8 * t.scale as i32)
    }

    /// What the screen says, as one string: the drawn texts in drawing order (drop shadows left out, they are
    /// the same words again), so that a sentence the layout wrapped over several lines reads as one.
    fn said(list: &DrawList) -> String {
        list.texts().iter().filter(|t| t.color != SHADOW).map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// No two strings of the screen share a pixel (a drop shadow is the same words again and is left out).
    fn assert_no_text_overlap(list: &DrawList, what: &str) {
        let texts: Vec<&DrawnText> = list.texts().iter().filter(|t| t.color != SHADOW).collect();
        for (i, a) in texts.iter().enumerate() {
            for b in &texts[i + 1..] {
                let (ra, rb) = (rect_of(a), rect_of(b));
                let apart = ra.2 <= rb.0 || rb.2 <= ra.0 || ra.3 <= rb.1 || rb.3 <= ra.1;
                assert!(apart, "{what}: {:?} {ra:?} overlaps {:?} {rb:?}", a.text, b.text);
            }
        }
    }

    /// No drawn string sticks out of the window, whatever the window size.
    fn assert_text_inside(list: &DrawList, w: u32, h: u32, what: &str) {
        for t in list.texts() {
            let (x0, y0, x1, y1) = rect_of(t);
            assert!(
                x0 >= 0 && y0 >= 0 && x1 <= w as i32 && y1 <= h as i32,
                "{what} at {w}x{h}: {:?} at ({x0},{y0})-({x1},{y1}) leaves the window",
                t.text
            );
        }
    }

    #[test]
    fn the_scale_is_the_largest_integer_that_keeps_the_design_canvas() {
        assert_eq!(ui_scale(320, 240), 1);
        assert_eq!(ui_scale(400, 270), 1);
        assert_eq!(ui_scale(640, 360), 1);
        assert_eq!(ui_scale(800, 600), 2);
        assert_eq!(ui_scale(1100, 680), 2);
        assert_eq!(ui_scale(1280, 720), 2);
        assert_eq!(ui_scale(1920, 1080), 4);
        assert_eq!(ui_scale(3840, 2160), 8);
        assert_eq!(ui_scale(10_000, 10_000), MAX_UI_SCALE);
        assert_eq!(ui_scale(0, 0), 1);
        // The canvas never gets smaller than the design while the window is big enough for it.
        for (w, h) in SIZES {
            let s = ui_scale(w, h);
            if w >= DESIGN_W && h >= DESIGN_H {
                assert!(w / s >= DESIGN_W && h / s >= DESIGN_H, "{w}x{h} at scale {s}");
            }
        }
    }

    #[test]
    fn the_entry_window_slides_with_the_cursor() {
        assert_eq!(entry_window(5, 5, 24), 0, "it fits");
        assert_eq!(entry_window(23, 23, 24), 0);
        assert_eq!(entry_window(24, 24, 24), 1, "full box with the cursor after the last character");
        assert_eq!(entry_window(24, 0, 10), 0);
        assert_eq!(entry_window(24, 24, 10), 15);
        for cursor in 0..=24 {
            let start = entry_window(24, cursor, 10);
            assert!(cursor >= start && cursor - start < 10, "cursor {cursor} visible (start {start})");
            assert!(start + 10 <= 25);
        }
        assert_eq!(entry_window(5, 2, 0), 0);
    }

    #[test]
    fn the_select_screen_shows_every_saved_character_with_its_look() {
        let menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        let list = draw(&menu, 1100, 680);
        for needle in ["FF MULTIVERSE", "Choose your character", "127.0.0.1:47770", "Cloud", "Tifa", "Cloud Strife", "Tifa Lockhart"] {
            assert!(list.says(needle), "missing {needle:?} in {:?}", list.texts().iter().map(|t| &t.text).collect::<Vec<_>>());
        }
        // A character whose look this machine has not seen says so.
        assert!(list.says("look unknown"));
        // The key hints are all there: Del asks to delete (it is not "forget" any more).
        for key in ["Enter", "Play", "New character", "Del", "Delete", "Esc", "Quit"] {
            assert!(list.says(key), "{key}");
        }
        assert!(!list.says("Forget"), "the list no longer offers to forget: Del asks to delete");
        // The FF7 footer is a plain one: no FIXTURE label for real content.
        assert!(list.says("Game: FF7") && !list.says("FIXTURE"));
        assert_text_inside(&list, 1100, 680, "select");
    }

    #[test]
    fn a_fixture_menu_says_so_on_every_screen() {
        let mut menu = Menu::new(cfg(GameId::Fixture), sample_accounts());
        let needle = "FIXTURE (synthetic, not FF7)";
        assert!(draw(&menu, 1100, 680).says(needle), "list");
        menu.text("n");
        assert!(draw(&menu, 1100, 680).says(needle), "create");
        let mut menu = Menu::new(cfg(GameId::Fixture), sample_accounts());
        menu.key(KeyPress::new(NavKey::Enter));
        assert!(draw(&menu, 1100, 680).says(needle), "connecting");
        menu.loading("area");
        assert!(draw(&menu, 1100, 680).says(needle), "loading");
    }

    #[test]
    fn the_highlighted_row_has_the_pointer_and_moves_with_the_cursor() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        let pointer_rows = |menu: &Menu| -> Vec<i32> {
            let list = draw(menu, 1100, 680);
            let arrow = crate::ui::font::cell_uv(crate::ui::font::special_cell(Special::ArrowRight));
            list.quads().iter().filter(|q| q.uv == arrow).map(|q| q.y).collect()
        };
        let first = pointer_rows(&menu);
        assert_eq!(first.len(), 1);
        menu.key(KeyPress::new(NavKey::Down));
        let second = pointer_rows(&menu);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0] - first[0], 20 * 2, "one row of 20 canvas units, at scale 2, further down");
    }

    #[test]
    fn a_very_long_name_is_shown_in_full_and_the_look_label_gives_way() {
        let name = "Barret With A Very Long Display Nam";
        let accounts = vec![entry(name, Some([60, 200, 90]), Some("ff7/aerith"))];
        let list = draw(&Menu::new(cfg(GameId::Ff7), accounts.clone()), 1100, 680);
        assert!(list.texts().iter().any(|t| t.text == name), "the whole name is there");
        assert!(!list.says("Aerith Gainsborough"), "the look label made room");
        // In a window with room for both, both are shown.
        let list = draw(&Menu::new(cfg(GameId::Ff7), vec![entry("Barret", Some([60, 200, 90]), Some("ff7/aerith"))]), 1100, 680);
        assert!(list.says("Barret") && list.says("Aerith Gainsborough"));
        // And the name is cut, never the window overrun, in the smallest window.
        let list = draw(&Menu::new(cfg(GameId::Ff7), accounts), 320, 240);
        assert_text_inside(&list, 320, 240, "long name");
    }

    #[test]
    fn a_long_list_scrolls_and_says_there_is_more() {
        let many: Vec<CharacterEntry> = (0..30).map(|i| entry(&format!("Hero {i:02}"), Some([200, 100, 50]), None)).collect();
        let mut menu = Menu::new(cfg(GameId::Ff7), many);
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Hero 00") && !list.says("Hero 29"));
        let down = crate::ui::font::cell_uv(crate::ui::font::special_cell(Special::ArrowDown));
        let up = crate::ui::font::cell_uv(crate::ui::font::special_cell(Special::ArrowUp));
        assert!(list.quads().iter().any(|q| q.uv == down), "more below");
        assert!(!list.quads().iter().any(|q| q.uv == up), "nothing above yet");
        menu.key(KeyPress::new(NavKey::End));
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Hero 29") && !list.says("Hero 00"));
        assert!(list.quads().iter().any(|q| q.uv == up) && !list.quads().iter().any(|q| q.uv == down));
        assert_text_inside(&list, 1100, 680, "long list");
    }

    #[test]
    fn the_confirmation_names_the_character_and_says_what_deleting_and_forgetting_mean() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.key(KeyPress::new(NavKey::Down));
        menu.key(KeyPress::new(NavKey::Delete));
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Delete this character?"));
        assert!(!list.says("Forget this saved login?"), "the old question is gone");
        assert!(list.texts().iter().any(|t| t.text == "Tifa"), "it names the character");
        let said = said(&list);
        // What deleting does, plainly: the character and its progress go from the server, the name is free again, and the
        // server keeps a copy that only its operator can bring back.
        for needle in ["deleted from the server", "its name is free again", "recoverable copy that only its operator can restore"] {
            assert!(said.contains(needle), "{needle:?} is not on the question: {said}");
        }
        // What forgetting does: the saved login of this computer only, the character stays on the server.
        for needle in ["Forget only removes the saved login from this computer", "stays on the server", "its name stays taken"] {
            assert!(said.contains(needle), "{needle:?} is not on the question: {said}");
        }
        // The three answers and the keys that give them.
        for label in ["Keep", "Delete", "Forget here"] {
            assert!(list.texts().iter().any(|t| t.text == label), "the {label} button");
        }
        for keys in ["Y delete", "F forget on this computer only", "N or Esc keep"] {
            assert!(said.contains(keys), "{keys:?} is not on the question: {said}");
        }
        assert!(!said.contains("Y forget") && !said.contains("This deletes the saved login"), "the old strings are gone: {said}");
        assert_text_inside(&list, 1100, 680, "confirm");
    }

    /// The focused answer is the bright one (the others are dimmer), whichever it is; it starts on Keep.
    #[test]
    fn the_focused_answer_is_the_bright_one() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.key(KeyPress::new(NavKey::Delete));
        // (The word Delete is also on the list's key hints, in bright text: that one is always there.)
        let dim = |menu: &Menu, label: &str| draw(menu, 1100, 680).texts().iter().filter(|t| t.text == label && t.color == SOFT).count();
        for (moves, focused) in [(0, "Keep"), (1, "Delete"), (2, "Forget here"), (3, "Keep")] {
            if moves > 0 {
                menu.key(KeyPress::new(NavKey::Right));
            }
            for label in ["Keep", "Delete", "Forget here"] {
                assert_eq!(dim(&menu, label), usize::from(label != focused), "{label} after {moves} moves (focus: {focused})");
            }
        }
    }

    /// Nothing on the question overlaps anything else on it, in any window: the three buttons, the two texts and the
    /// keys (which break over rows where the panel is narrow) each have their own place.
    #[test]
    fn nothing_on_the_confirmation_overlaps_and_it_fits_every_window() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.key(KeyPress::new(NavKey::Delete));
        for (w, h) in SIZES {
            let list = draw(&menu, w, h);
            // The question is drawn last: everything from its title on is its own.
            let panel: Vec<&DrawnText> = list.texts().iter().skip_while(|t| t.text != "Delete this character?").filter(|t| t.color != SHADOW).collect();
            assert!(panel.len() >= 12, "{w}x{h}: only {} texts", panel.len());
            for (i, a) in panel.iter().enumerate() {
                for b in &panel[i + 1..] {
                    let (ra, rb) = (rect_of(a), rect_of(b));
                    let apart = ra.2 <= rb.0 || rb.2 <= ra.0 || ra.3 <= rb.1 || rb.3 <= ra.1;
                    assert!(apart, "{w}x{h}: {:?} {ra:?} overlaps {:?} {rb:?}", a.text, b.text);
                }
            }
            assert_text_inside(&list, w, h, "confirm");
        }
    }

    #[test]
    fn the_deleting_screen_says_what_is_happening_and_that_escape_only_stops_the_wait() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.key(KeyPress::new(NavKey::Down));
        menu.key(KeyPress::new(NavKey::Delete));
        assert_eq!(menu.text("y"), Some(MenuAction::Delete { index: 1 }));
        // (One tick counts for at most a second, so two seconds and a bit are three ticks.)
        for dt in [1.0, 1.0, 0.5] {
            menu.tick(dt);
        }
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Deleting this character") && list.texts().iter().any(|t| t.text == "Tifa"), "{:?}", list.texts().iter().map(|t| &t.text).collect::<Vec<_>>());
        assert!(list.says("127.0.0.1:47770") && list.says("2 s"));
        assert!(list.says("Esc: stop waiting") && !list.says("Esc: cancel"), "Esc only ends the wait: the server may still delete it");
        assert!(!list.says("Logging in") && !list.says("Creating your character"));
        assert_text_inside(&list, 1100, 680, "deleting");
    }

    #[test]
    fn the_create_screen_shows_the_form_the_body_and_the_fighting_style() {
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text("Cloud");
        let list = draw(&menu, 1100, 680);
        let default_body = crate::ui::menu::body_options(GameId::Ff7)[0].label;
        for needle in ["NEW CHARACTER", "Name", "Body", default_body, "Color", "Red", "Green", "Blue", "Fighting style", "Gunner", "Preview", "Create", "Quit", "5/24"] {
            assert!(list.says(needle), "missing {needle:?}");
        }
        assert!(!list.says("Look"), "the old row is gone");
        assert!(!list.texts().iter().any(|t| t.text == "Slot"), "the Slot row is the Fighting style row now");
        // (The name typed above is Cloud's: the rest of the screen names no canon character.)
        for canon in fmv_ff7_battle::kernel::CharacterId::ALL.into_iter().skip(1) {
            assert!(!list.says(canon.name()), "the create screen never names {}", canon.name());
        }
        assert!(list.texts().iter().any(|t| t.text == "Cloud"), "the typed name is in the field");
        assert!(list.says("#"), "the preview shows the colour as hex");
        assert_text_inside(&list, 1100, 680, "create");
        // With a list to go back to the button says Back.
        let menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        let mut menu = menu;
        menu.text("n");
        assert!(draw(&menu, 1100, 680).says("Back"));
    }

    #[test]
    fn the_fixture_create_form_has_neither_a_body_nor_a_slot_row() {
        let menu = Menu::new(cfg(GameId::Fixture), vec![]);
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Name") && list.says("Color"));
        assert!(!list.texts().iter().any(|t| t.text == "Body" || t.text == "Slot" || t.text == "Fighting style" || t.text == "Look"));
        assert!(!list.says("Barret") && !list.says("Gunner") && !list.says("how you look") && !list.says("how you fight"));
        assert_text_inside(&list, 1100, 680, "fixture create");
        assert_no_text_overlap(&list, "fixture create");
    }

    /// The create screen says what each choice means, in one short line under its row (wrapped where the
    /// window is narrow): the body is how you look on the field, the fighting style is how you fight.
    #[test]
    fn each_row_says_what_it_means_under_it_where_the_window_has_room() {
        let menu = Menu::new(cfg(GameId::Ff7), vec![]);
        let list = draw(&menu, 1100, 680);
        let said = said(&list);
        assert!(said.contains("how you look on the field (a placeholder model)"), "{said}");
        assert!(said.contains("how you fight: its stats, spells and level curve (fixed once created)"), "{said}");
        // The help is DIM text under the row it is about: the body's between the Body and Color rows, the style's
        // between the Fighting style row and the Preview line.
        let y_of = |text: &str| list.texts().iter().find(|t| t.text == text && t.color != SHADOW).map(|t| t.y).unwrap_or_else(|| panic!("{text:?} is drawn"));
        let (body_row, color_row, slot_row, preview) = (y_of("Body"), y_of("Color"), y_of("Fighting style"), y_of("Preview"));
        let body_help = list.texts().iter().find(|t| t.text.starts_with("how you look") && t.color == DIM).expect("the body's help").y;
        let slot_help = list.texts().iter().find(|t| t.text.starts_with("how you fight") && t.color == DIM).expect("the style's help").y;
        assert!(body_row < body_help && body_help < color_row, "{body_row} < {body_help} < {color_row}");
        assert!(slot_row < slot_help && slot_help < preview, "{slot_row} < {slot_help} < {preview}");
        // The Body row's help is one line here, the Fighting style row's wraps over two (it is longer than the window is wide).
        let helps: Vec<&DrawnText> = list.texts().iter().filter(|t| t.color == DIM && (t.text.starts_with("how you look") || t.text.starts_with("how you fight") || t.text.starts_with("(fixed"))).collect();
        assert!(helps.len() >= 3, "{helps:?}");
    }

    /// In every window size the meaning of the focused choice is on screen: under its row where there is room
    /// for all the help lines, else in the place of the key hints under the buttons.
    #[test]
    fn the_focused_choice_is_explained_at_every_window_size_and_nothing_overlaps() {
        for (w, h) in SIZES {
            for (presses, want) in [(1, "how you look on the field (a placeholder model)"), (6, "how you fight: its stats, spells and level curve (fixed once created)")] {
                let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
                for _ in 0..presses {
                    menu.key(KeyPress::new(NavKey::Tab));
                }
                let list = draw(&menu, w, h);
                assert!(said(&list).contains(want), "{w}x{h}: {want:?} is not on screen: {}", said(&list));
                assert_text_inside(&list, w, h, "create");
                assert_no_text_overlap(&list, &format!("create at {w}x{h}"));
            }
            // With a name that is long, a refusal on screen and a list to go back to: still no overlap.
            let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
            menu.text("n");
            menu.text(&"W".repeat(24));
            menu.set_notice(Notice::error("The server refused: that display name is already taken.").with_hint("Change the name and press Enter again."));
            let list = draw(&menu, w, h);
            assert_text_inside(&list, w, h, "create with a notice");
            assert_no_text_overlap(&list, &format!("create with a notice at {w}x{h}"));
        }
    }

    /// The list line of a character says its body and its fighting style where the row has the room.
    #[test]
    fn the_select_screen_says_each_characters_body_and_fighting_style() {
        use crate::ui::menu::Look;
        let body = fmv_ff7::playable::PLACEHOLDER_BODIES[0];
        let created = Look { tint: [200, 40, 40], appearance: Some(body.model.appearance_ref.into()), body: Some(body.archetype.into()), slot: Some(1) };
        let migrated = Look { tint: [40, 40, 200], appearance: Some("ff7/tifa".into()), body: None, slot: Some(2) };
        let accounts = vec![
            CharacterEntry { name: "Wedge".into(), look: Some(created), loose_permissions: false },
            CharacterEntry { name: "Old".into(), look: Some(migrated), loose_permissions: false },
        ];
        let list = draw(&Menu::new(cfg(GameId::Ff7), accounts), 1100, 680);
        let said = said(&list);
        assert!(said.contains(&format!("{}  style: Gunner", body.label)), "{said}");
        assert!(said.contains("Tifa Lockhart  style: Martial artist"), "{said}");
        assert_text_inside(&list, 1100, 680, "select with slots");
    }

    #[test]
    fn the_focused_row_is_marked_and_the_mark_follows_tab() {
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        let pointer = crate::ui::font::cell_uv(crate::ui::font::special_cell(Special::ArrowRight));
        // The pointer is in the form's left margin; the body row's right arrow is further right.
        let ys = |menu: &Menu| -> Vec<i32> {
            draw(menu, 1100, 680).quads().iter().filter(|q| q.uv == pointer && q.x < 550).map(|q| q.y).collect()
        };
        let mut last = ys(&menu);
        assert_eq!(last.len(), 1, "exactly one row has the pointer");
        for _ in 0..3 {
            menu.key(KeyPress::new(NavKey::Tab));
            let now = ys(&menu);
            assert_eq!(now.len(), 1);
            assert!(now[0] > last[0], "the pointer moves down the form: {last:?} -> {now:?}");
            last = now;
        }
        // The focused field's label is yellow.
        let list = draw(&menu, 1100, 680);
        let colours: Vec<(&str, Rgba)> = list.texts().iter().filter(|t| ["Name", "Body", "Color", "Red"].contains(&t.text.as_str())).map(|t| (t.text.as_str(), t.color)).collect();
        assert!(colours.iter().any(|(n, c)| *n == "Red" && *c == ACCENT), "{colours:?}");
        assert!(colours.iter().any(|(n, c)| *n == "Name" && *c == TEXT), "{colours:?}");
    }

    #[test]
    fn a_server_refusal_is_shown_in_full_on_the_form_it_came_back_to() {
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text("Cloud");
        menu.key(KeyPress::new(NavKey::Enter));
        menu.connect_failed(
            Notice::error("The server refused: that display name is already taken.").with_hint("Change the name and press Enter again."),
        );
        let list = draw(&menu, 1100, 680);
        let said: String = list.texts().iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(said.contains("that display name is already taken"), "{said}");
        assert!(said.contains("Change the name"), "{said}");
        assert!(list.texts().iter().any(|t| t.text.contains("refused") && t.color == ERROR));
        assert_text_inside(&list, 1100, 680, "refusal");
    }

    #[test]
    fn long_messages_wrap_inside_the_window_at_every_size() {
        let long = "x".repeat(60) + " that display name is already taken " + &"y".repeat(60);
        for (w, h) in SIZES {
            let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
            menu.set_notice(Notice::error(long.clone()).with_hint("Press Enter to try again, or Esc to go back and think about it."));
            assert_text_inside(&draw(&menu, w, h), w, h, "select + notice");
            menu.text("n");
            menu.set_notice(Notice::error(long.clone()).with_hint("Press Enter to try again."));
            assert_text_inside(&draw(&menu, w, h), w, h, "create + notice");
        }
    }

    #[test]
    fn the_connecting_screen_says_what_is_happening_and_how_to_leave() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.key(KeyPress::new(NavKey::Down));
        menu.key(KeyPress::new(NavKey::Enter));
        // (One tick counts for at most a second, so three seconds and a bit are four ticks.)
        for dt in [1.0, 1.0, 1.0, 0.2] {
            menu.tick(dt);
        }
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Logging in") && list.says("as Tifa") && list.says("127.0.0.1:47770") && list.says("3 s"));
        assert!(list.says("Esc: cancel"));
        assert_text_inside(&list, 1100, 680, "connecting");
        // Registering says so too.
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text("Aerith");
        menu.key(KeyPress::new(NavKey::Enter));
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Creating your character") && list.texts().iter().any(|t| t.text == "Aerith"));
    }

    #[test]
    fn the_loading_screen_names_the_area() {
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.loading("FF7 md1stin");
        let list = draw(&menu, 1100, 680);
        assert!(list.says("Joined!") && list.says("FF7 md1stin"));
        assert_text_inside(&list, 1100, 680, "loading");
    }

    #[test]
    fn a_token_that_could_not_be_saved_takes_the_screen_and_is_shown_whole() {
        let token = "fmvt_0123456789abcdefghijklmnopqrstuvwxyz0123456789ABCDEF";
        let mut menu = Menu::new(cfg(GameId::Ff7), sample_accounts());
        menu.set_notice(
            Notice::error("The character was created on the server, but its login could not be saved on this computer (disk full).")
                .with_hint("Write the token below into a file by hand.")
                .with_secret(token),
        );
        for (w, h) in SIZES {
            let list = draw(&menu, w, h);
            let rejoined: String = list.texts().iter().filter(|t| t.color == [255, 255, 255, 255]).map(|t| t.text.as_str()).collect();
            assert_eq!(rejoined, token, "the whole token is on screen at {w}x{h}");
            assert!(list.says("SAVE THIS TOKEN"));
            assert!(!list.says("Choose your character"), "nothing else competes with it");
            assert_text_inside(&list, w, h, "token alert");
        }
    }

    #[test]
    fn every_screen_fits_every_window_size() {
        // The normal sizes (at least the design canvas) must fit completely; the smallest window
        // gets the compact layout, which must fit too.
        let states: Vec<(&str, Menu)> = {
            let mut v = Vec::new();
            v.push(("select", Menu::new(cfg(GameId::Ff7), sample_accounts())));
            v.push(("select one", Menu::new(cfg(GameId::Ff7), vec![entry("Cloud", Some([1, 2, 3]), Some("ff7/cloud"))])));
            let mut ask = Menu::new(cfg(GameId::Ff7), sample_accounts());
            ask.key(KeyPress::new(NavKey::Delete));
            v.push(("delete question", ask));
            let mut deleting = Menu::new(cfg(GameId::Ff7), sample_accounts());
            deleting.key(KeyPress::new(NavKey::Delete));
            deleting.text("y");
            v.push(("deleting", deleting));
            let mut create = Menu::new(cfg(GameId::Ff7), vec![]);
            create.text(&"W".repeat(24));
            v.push(("create", create));
            v.push(("create fixture", Menu::new(cfg(GameId::Fixture), vec![])));
            let mut connecting = Menu::new(cfg(GameId::Ff7), sample_accounts());
            connecting.key(KeyPress::new(NavKey::Enter));
            v.push(("connecting", connecting));
            let mut loading = Menu::new(cfg(GameId::Ff7), sample_accounts());
            loading.loading("FF7 a_really_quite_long_field_name_here");
            v.push(("loading", loading));
            v
        };
        for (what, menu) in &states {
            for (w, h) in SIZES {
                let list = draw(menu, w, h);
                assert!(!list.is_empty());
                assert_text_inside(&list, w, h, what);
            }
        }
    }

    #[test]
    fn random_menu_states_always_draw_inside_the_window() {
        for seed in 1..=12 {
            crate::ui::testkit::monkey(seed, 150, |menu, step| {
                // Every few steps, at a spread of window sizes (drawing every step would be a lot).
                if step % 7 == 0 {
                    for (w, h) in [(320, 240), (640, 360), (1100, 680), (1920, 1080)] {
                        let list = draw(menu, w, h);
                        assert!(!list.is_empty());
                        assert_text_inside(&list, w, h, &format!("seed {seed} step {step} {:?}", menu.screen()));
                    }
                }
            });
        }
    }

    #[test]
    fn the_name_field_slides_when_the_box_is_narrower_than_the_name() {
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text(&"abcdefghij".repeat(3));
        // 24 characters fit the name; at the smallest window the field box is narrower.
        let list = draw(&menu, 320, 240);
        let field = list.texts().iter().find(|t| t.text.starts_with("abc") || t.text.contains("hij")).expect("the name field text");
        assert!(cell_count(&field.text) <= 24);
        assert_text_inside(&list, 320, 240, "narrow name field");
    }

    #[test]
    fn unsupported_characters_in_the_name_get_a_note() {
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text("Jos\u{e9}");
        assert!(draw(&menu, 1100, 680).says("boxes"));
        let mut menu = Menu::new(cfg(GameId::Ff7), vec![]);
        menu.text("Jose");
        assert!(!draw(&menu, 1100, 680).says("boxes"));
    }

    #[test]
    fn loose_token_permissions_are_flagged_on_the_list() {
        let mut accounts = sample_accounts();
        accounts[0].loose_permissions = true;
        let menu = Menu::new(cfg(GameId::Ff7), accounts);
        let list = draw(&menu, 1100, 680);
        // The message wraps over several lines: read it as one.
        let said: String = list.texts().iter().filter(|t| t.color == WARN || t.color == TEXT).map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(said.contains("can be read by") && said.contains("other users"), "{said}");
        assert!(said.contains("chmod 600"), "{said}");
        assert!(list.texts().iter().any(|t| t.text == "!" && t.color == WARN));
    }
}
