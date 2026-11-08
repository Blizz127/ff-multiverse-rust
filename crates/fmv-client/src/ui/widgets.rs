//! Simple widgets over a [`DrawList`]: the colours of the UI, a [`Canvas`] that lays things out in
//! whole "canvas units" (each `s` x `s` window pixels, so every edge stays on the pixel grid and text
//! is always an integer multiple of the 8x8 font), FF7-style panels, buttons that show whether
//! they have the keyboard focus, colour swatches and the focus pointer.
//!
//! Nothing here remembers anything: the menu state machine ([`super::menu`]) decides what has the
//! focus, and the screens hand that to the widget they draw (`button(.., focused)`, a form row's
//! highlight and pointer).

use super::draw::{DrawList, Rgba};
use super::font::Special;
use super::text::{cell_count, ellipsize};

// ---- style ---------------------------------------------------------------------------------
// The look is the FF7 menu window: blue gradient panels with a light border, white text, a
// yellow pointer.

pub const TEXT: Rgba = [244, 246, 255, 255];
pub const DIM: Rgba = [150, 160, 208, 255];
pub const ACCENT: Rgba = [255, 214, 80, 255];
pub const ERROR: Rgba = [255, 122, 122, 255];
pub const WARN: Rgba = [255, 198, 92, 255];
pub const INFO: Rgba = [150, 216, 255, 255];
pub const SHADOW: Rgba = [0, 0, 0, 190];
pub const BG_TOP: Rgba = [8, 10, 36, 255];
pub const BG_BOTTOM: Rgba = [22, 30, 84, 255];
pub const PANEL_TOP: Rgba = [34, 54, 164, 255];
pub const PANEL_BOTTOM: Rgba = [6, 10, 74, 255];
pub const BORDER: Rgba = [204, 210, 236, 255];
pub const BORDER_DIM: Rgba = [110, 120, 170, 255];
pub const HILITE: Rgba = [255, 255, 255, 40];
pub const TRACK: Rgba = [14, 18, 60, 255];
pub const OUTLINE: Rgba = [8, 8, 16, 255];
pub const DIMMER: Rgba = [0, 0, 0, 150];
/// The text of an unfocused button and of an unselected list row.
pub const SOFT: Rgba = [200, 208, 240, 255];

/// Height of a button in canvas units.
pub const BUTTON_H: i32 = 16;

/// A drawing surface in canvas units. Everything is scaled by `s` on the way to the list.
pub struct Canvas<'a> {
    pub out: &'a mut DrawList,
    pub s: i32,
}

impl Canvas<'_> {
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        let s = self.s;
        self.out.fill_rect(x * s, y * s, w * s, h * s, color);
    }

    /// A one-unit outline drawn inside the rectangle.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        let s = self.s;
        self.out.frame_rect(x * s, y * s, w * s, h * s, s, color);
    }

    pub fn gradient(&mut self, x: i32, y: i32, w: i32, h: i32, top: Rgba, bottom: Rgba) {
        let s = self.s;
        self.out.vertical_gradient(x * s, y * s, w * s, h * s, [top, bottom], (h / 2).max(1));
    }

    /// Text at the canvas scale (glyphs 8 canvas units); returns its width in canvas units.
    pub fn text(&mut self, x: i32, y: i32, color: Rgba, text: &str) -> i32 {
        let s = self.s;
        self.out.draw_text(x * s, y * s, s as u32, color, text) / s
    }

    /// Text with a drop shadow.
    pub fn shadowed(&mut self, x: i32, y: i32, color: Rgba, text: &str) -> i32 {
        let s = self.s;
        self.out.draw_text_shadowed(x * s, y * s, s as u32, color, SHADOW, text) / s
    }

    /// Double-size text (16 canvas units tall) with a shadow.
    pub fn big(&mut self, x: i32, y: i32, color: Rgba, text: &str) -> i32 {
        let s = self.s;
        self.out.draw_text_shadowed(x * s, y * s, 2 * s as u32, color, SHADOW, text) / s
    }

    pub fn special(&mut self, x: i32, y: i32, color: Rgba, glyph: Special) {
        let s = self.s;
        self.out.draw_special(x * s, y * s, s as u32, color, glyph);
    }

    /// Text centred in `[left, left + width)`; text wider than that (less one cell of margin, so
    /// the drop shadow stays inside too) is cut with `...` rather than left to hang out of its box.
    pub fn centered(&mut self, left: i32, width: i32, y: i32, color: Rgba, text: &str) {
        let fitted = ellipsize(text, (width / 8 - 1).max(1) as usize);
        let w = cell_count(&fitted) as i32 * 8;
        self.shadowed(left + (width - w) / 2, y, color, &fitted);
    }

    /// An FF7-style window: light border, blue gradient inside.
    pub fn panel(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.fill(x, y, w, h, BORDER);
        self.gradient(x + 1, y + 1, w - 2, h - 2, PANEL_TOP, PANEL_BOTTOM);
    }

    /// A button with its label centred. The one with the keyboard focus has a yellow border and a
    /// brighter face, so the focus is visible without colour alone being the signal (the label
    /// is brighter too).
    pub fn button(&mut self, x: i32, y: i32, w: i32, label: &str, focused: bool) {
        if focused {
            self.fill(x, y, w, BUTTON_H, ACCENT);
            self.gradient(x + 2, y + 2, w - 4, BUTTON_H - 4, [70, 100, 214, 255], [26, 44, 150, 255]);
        } else {
            self.fill(x, y, w, BUTTON_H, BORDER_DIM);
            self.gradient(x + 1, y + 1, w - 2, BUTTON_H - 2, PANEL_TOP, PANEL_BOTTOM);
        }
        self.centered(x, w, y + 4, if focused { TEXT } else { SOFT }, label);
    }

    /// A colour swatch with a dark outline.
    pub fn swatch(&mut self, x: i32, y: i32, size: i32, tint: [u8; 3]) {
        self.fill(x - 1, y - 1, size + 2, size + 2, OUTLINE);
        self.fill(x, y, size, size, [tint[0], tint[1], tint[2], 255]);
    }

    /// The yellow pointer that marks the focused row.
    pub fn pointer(&mut self, x: i32, y: i32) {
        self.special(x, y, ACCENT, Special::ArrowRight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(list: &mut DrawList, s: i32) -> Canvas<'_> {
        Canvas { out: list, s }
    }

    #[test]
    fn everything_is_scaled_to_the_pixel_grid() {
        let mut list = DrawList::new();
        let mut c = canvas(&mut list, 3);
        c.fill(2, 4, 10, 6, TEXT);
        let w = c.text(1, 1, TEXT, "ab");
        assert_eq!(w, 16, "widths come back in canvas units");
        assert_eq!((list.quads()[0].x, list.quads()[0].y, list.quads()[0].w, list.quads()[0].h), (6, 12, 30, 18));
        // The glyphs are 8 canvas units, 24 pixels, at scale 3.
        assert_eq!((list.quads()[1].w, list.quads()[1].h, list.quads()[1].x), (24, 24, 3));
    }

    #[test]
    fn centred_text_that_does_not_fit_is_cut_not_overhanging() {
        let mut list = DrawList::new();
        let mut c = canvas(&mut list, 1);
        c.centered(10, 40, 0, TEXT, "abcdefghijkl");
        let drawn = list.texts().iter().find(|t| t.color == TEXT).expect("the text");
        assert_eq!(drawn.text, "a...", "four cells fit in forty units, leaving a cell of margin");
        assert!(drawn.x >= 10 && drawn.x + 32 <= 10 + 40);
        // Text that fits is centred as it is.
        let mut list = DrawList::new();
        let mut c = canvas(&mut list, 2);
        c.centered(0, 100, 0, TEXT, "abcd");
        let drawn = list.texts().iter().find(|t| t.color == TEXT).unwrap();
        assert_eq!(drawn.text, "abcd");
        assert_eq!(drawn.x, (100 - 32) / 2 * 2);
    }

    #[test]
    fn a_focused_button_looks_different_from_an_unfocused_one() {
        let draw = |focused: bool| {
            let mut list = DrawList::new();
            canvas(&mut list, 1).button(0, 0, 80, "Create", focused);
            list
        };
        let (on, off) = (draw(true), draw(false));
        assert!(on.quads().iter().any(|q| q.color == ACCENT), "the focused one has the yellow border");
        assert!(!off.quads().iter().any(|q| q.color == ACCENT));
        // The label is brighter when focused, and both say the same thing.
        let label = |l: &DrawList| l.texts().iter().rev().find(|t| t.text == "Create").map(|t| t.color).unwrap();
        assert_eq!(label(&on), TEXT);
        assert_eq!(label(&off), SOFT);
        // Same footprint either way: moving the focus never moves the layout.
        let extent = |l: &DrawList| (l.quads()[0].x, l.quads()[0].y, l.quads()[0].w, l.quads()[0].h);
        assert_eq!(extent(&on), extent(&off));
    }

    #[test]
    fn a_panel_has_a_border_and_a_gradient_inside_it() {
        let mut list = DrawList::new();
        canvas(&mut list, 2).panel(5, 5, 100, 40);
        let q = list.quads();
        assert_eq!((q[0].color, q[0].x, q[0].w), (BORDER, 10, 200));
        // The gradient covers exactly the inside.
        let inside: i32 = q[1..].iter().map(|r| r.w * r.h).sum();
        assert_eq!(inside, 196 * 76);
        assert_eq!(q[1].color, PANEL_TOP);
        assert_eq!(q.last().unwrap().color, PANEL_BOTTOM);
    }

    #[test]
    fn a_swatch_is_its_colour_inside_a_dark_outline() {
        let mut list = DrawList::new();
        canvas(&mut list, 1).swatch(10, 10, 12, [200, 100, 50]);
        let q = list.quads();
        assert_eq!((q[0].color, q[0].x, q[0].w), (OUTLINE, 9, 14));
        assert_eq!((q[1].color, q[1].x, q[1].w), ([200, 100, 50, 255], 10, 12));
    }
}
