//! The draw list: everything the UI wants on screen as textured rectangles, in window pixels.
//!
//! Plain rectangles and text glyphs are the same kind of thing, a rectangle with a piece of the
//! font atlas on it ([`crate::ui::font`]): a glyph samples its cell, a plain rectangle samples a
//! point inside the solid block. So the whole UI is one pipeline, one texture and one draw call.
//!
//! * Coordinates are window pixels, origin at the TOP LEFT, +y down, integers. Glyphs are only
//!   ever placed at integer positions and scaled by integer factors, which with nearest-neighbour
//!   sampling keeps every font pixel an exact square.
//! * Colours are sRGB bytes (like the rest of the client's palette); [`DrawList::vertices`]
//!   converts them for the target format once, at upload.
//! * A [`DrawList`] is pure data: building one needs no GPU, so the screens can be unit-tested by
//!   asking which strings they drew and where ([`DrawList::texts`], tests only).
//! * Drawing TEXT onto a list (`draw_text(x, y, scale, rgba, text)` and friends) is in
//!   [`super::text`], next to the layout helpers.

use super::font::solid_uv;
use bytemuck::{Pod, Zeroable};

/// An sRGB colour with alpha.
pub type Rgba = [u8; 4];

/// Largest text scale accepted (a bigger one would only overflow the coordinates).
pub const MAX_SCALE: u32 = 16;

/// One textured rectangle in window pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// `[u0, v0, u1, v1]` into the atlas.
    pub uv: [f32; 4],
    pub color: Rgba,
}

/// A vertex as the UI pipeline reads it: NDC position, atlas coordinate, straight-alpha colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct UiVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// A piece of text that was drawn (tests only: lets a test ask what a screen says).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct DrawnText {
    pub x: i32,
    pub y: i32,
    pub scale: u32,
    pub color: Rgba,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub(super) quads: Vec<Quad>,
    #[cfg(test)]
    pub(super) texts: Vec<DrawnText>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.quads.clear();
        self.texts.clear();
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }

    /// Number of rectangles (glyphs included).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.quads.len()
    }

    #[cfg(test)]
    pub fn quads(&self) -> &[Quad] {
        &self.quads
    }

    /// Every string drawn, in drawing order (shadows included: they are drawn as text too).
    #[cfg(test)]
    pub fn texts(&self) -> &[DrawnText] {
        &self.texts
    }

    /// Is `needle` part of any drawn string?
    #[cfg(test)]
    pub fn says(&self, needle: &str) -> bool {
        self.texts.iter().any(|t| t.text.contains(needle))
    }

    /// A filled rectangle. Empty or fully transparent rectangles are ignored.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        if w <= 0 || h <= 0 || color[3] == 0 {
            return;
        }
        self.quads.push(Quad { x, y, w, h, uv: solid_uv(), color });
    }

    /// The outline of a rectangle, `thickness` pixels wide, drawn inside it.
    pub fn frame_rect(&mut self, x: i32, y: i32, w: i32, h: i32, thickness: i32, color: Rgba) {
        let t = thickness.min(w / 2).min(h / 2);
        if t <= 0 {
            return;
        }
        self.fill_rect(x, y, w, t, color);
        self.fill_rect(x, y + h - t, w, t, color);
        self.fill_rect(x, y + t, t, h - 2 * t, color);
        self.fill_rect(x + w - t, y + t, t, h - 2 * t, color);
    }

    /// A vertical gradient from `ends[0]` (top) to `ends[1]` (bottom) in `bands` horizontal
    /// strips (at least 1; each strip is at least one pixel tall).
    pub fn vertical_gradient(&mut self, x: i32, y: i32, w: i32, h: i32, ends: [Rgba; 2], bands: i32) {
        let [top, bottom] = ends;
        if w <= 0 || h <= 0 {
            return;
        }
        let bands = bands.clamp(1, h);
        for i in 0..bands {
            let (y0, y1) = (y + h * i / bands, y + h * (i + 1) / bands);
            let t = if bands == 1 { 0.0 } else { i as f32 / (bands - 1) as f32 };
            let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
            let color = [mix(top[0], bottom[0]), mix(top[1], bottom[1]), mix(top[2], bottom[2]), mix(top[3], bottom[3])];
            self.fill_rect(x, y0, w, y1 - y0, color);
        }
    }

    /// The vertices for a `vw` x `vh` pixel target: two triangles per rectangle, converted to NDC.
    /// `srgb` says whether the target converts shader output from linear (see
    /// [`crate::render::color`]). Rectangles completely outside the target are dropped.
    pub fn vertices(&self, vw: u32, vh: u32, srgb: bool) -> Vec<UiVertex> {
        let (fw, fh) = (vw.max(1) as f32, vh.max(1) as f32);
        let mut out = Vec::with_capacity(self.quads.len() * 6);
        for q in &self.quads {
            if q.x >= vw as i32 || q.y >= vh as i32 || q.x + q.w <= 0 || q.y + q.h <= 0 {
                continue;
            }
            let rgb = crate::render::color([q.color[0], q.color[1], q.color[2]], srgb);
            let color = [rgb[0], rgb[1], rgb[2], f32::from(q.color[3]) / 255.0];
            let ndc = |px: i32, py: i32| [px as f32 / fw * 2.0 - 1.0, 1.0 - py as f32 / fh * 2.0];
            let [u0, v0, u1, v1] = q.uv;
            let (x1, y1) = (q.x + q.w, q.y + q.h);
            let tl = UiVertex { pos: ndc(q.x, q.y), uv: [u0, v0], color };
            let tr = UiVertex { pos: ndc(x1, q.y), uv: [u1, v0], color };
            let bl = UiVertex { pos: ndc(q.x, y1), uv: [u0, v1], color };
            let br = UiVertex { pos: ndc(x1, y1), uv: [u1, v1], color };
            out.extend_from_slice(&[tl, bl, br, tl, br, tr]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::font::{cell_of, cell_uv, solid_uv};

    const WHITE: Rgba = [255, 255, 255, 255];

    #[test]
    fn text_becomes_one_glyph_quad_per_visible_character_on_an_integer_grid() {
        let mut d = DrawList::new();
        let w = d.draw_text(10, 20, 3, WHITE, "Hi A");
        assert_eq!(w, 4 * 8 * 3, "the width includes the space");
        // The space draws nothing: three quads.
        assert_eq!(d.len(), 3);
        let q = d.quads();
        assert_eq!((q[0].x, q[0].y, q[0].w, q[0].h), (10, 20, 24, 24));
        assert_eq!((q[1].x, q[1].y), (34, 20));
        assert_eq!((q[2].x, q[2].y), (10 + 3 * 24, 20), "after the space");
        assert_eq!(q[0].uv, cell_uv(cell_of('H')));
        assert_eq!(q[1].uv, cell_uv(cell_of('i')));
        assert_eq!(d.texts().len(), 1);
        assert_eq!(d.texts()[0].text, "Hi A");
        assert!(d.says("i A") && !d.says("xyz"));
    }

    #[test]
    fn unknown_characters_are_boxes_and_controls_are_skipped() {
        let mut d = DrawList::new();
        let w = d.draw_text(0, 0, 1, WHITE, "a\u{e9}\n\u{1b}b");
        assert_eq!(w, 24, "three cells: a, the box, b");
        assert_eq!(d.len(), 3);
        assert_eq!(d.quads()[1].uv, cell_uv(cell_of('\u{e9}')));
        assert_ne!(d.quads()[1].uv, d.quads()[0].uv);
        assert_eq!(d.texts()[0].text, "a\u{e9}b", "the recorded text has no control characters");
    }

    #[test]
    fn the_scale_is_clamped_and_transparent_text_draws_nothing() {
        let mut d = DrawList::new();
        d.draw_text(0, 0, 0, WHITE, "a");
        assert_eq!((d.quads()[0].w, d.quads()[0].h), (8, 8), "0 counts as 1");
        d.draw_text(0, 0, 1000, WHITE, "a");
        assert_eq!(d.quads()[1].w, (8 * MAX_SCALE) as i32);
        let n = d.len();
        d.draw_text(0, 0, 2, [255, 255, 255, 0], "abc");
        assert_eq!(d.len(), n, "alpha 0 is invisible");
    }

    #[test]
    fn rectangles_use_the_solid_texel_and_degenerate_ones_vanish() {
        let mut d = DrawList::new();
        d.fill_rect(1, 2, 30, 40, [1, 2, 3, 255]);
        d.fill_rect(0, 0, 0, 10, WHITE);
        d.fill_rect(0, 0, 10, -1, WHITE);
        d.fill_rect(0, 0, 10, 10, [9, 9, 9, 0]);
        assert_eq!(d.len(), 1);
        assert_eq!(d.quads()[0].uv, solid_uv());
        d.clear();
        assert!(d.is_empty() && d.texts().is_empty());
    }

    #[test]
    fn a_frame_is_four_bars_inside_the_rectangle_and_a_gradient_covers_it_exactly() {
        let mut d = DrawList::new();
        d.frame_rect(10, 10, 100, 50, 2, WHITE);
        assert_eq!(d.len(), 4);
        let covered: i32 = d.quads().iter().map(|q| q.w * q.h).sum();
        assert_eq!(covered, 100 * 50 - 96 * 46, "the border area, nothing twice");
        assert!(d.quads().iter().all(|q| q.x >= 10 && q.y >= 10 && q.x + q.w <= 110 && q.y + q.h <= 60));
        // A frame thicker than the rectangle is clamped, a zero one is nothing.
        let mut d = DrawList::new();
        d.frame_rect(0, 0, 10, 10, 50, WHITE);
        assert!(d.quads().iter().all(|q| q.x + q.w <= 10 && q.y + q.h <= 10));
        let mut none = DrawList::new();
        none.frame_rect(0, 0, 10, 10, 0, WHITE);
        assert!(none.is_empty());

        let mut g = DrawList::new();
        g.vertical_gradient(5, 7, 40, 33, [[0, 0, 0, 255], [200, 100, 50, 255]], 8);
        let area: i32 = g.quads().iter().map(|q| q.w * q.h).sum();
        assert_eq!(area, 40 * 33);
        assert_eq!(g.quads().first().unwrap().color, [0, 0, 0, 255]);
        assert_eq!(g.quads().last().unwrap().color, [200, 100, 50, 255]);
        // Strips are contiguous top to bottom.
        let mut y = 7;
        for q in g.quads() {
            assert_eq!(q.y, y);
            y += q.h;
        }
        assert_eq!(y, 7 + 33);
        // More bands than rows is clamped to one pixel per band.
        let mut tiny = DrawList::new();
        tiny.vertical_gradient(0, 0, 4, 3, [[10; 4], [20; 4]], 100);
        assert_eq!(tiny.len(), 3);
    }

    #[test]
    fn a_shadow_is_drawn_first_one_font_pixel_down_and_right() {
        let mut d = DrawList::new();
        d.draw_text_shadowed(10, 10, 2, WHITE, [0, 0, 0, 255], "A");
        assert_eq!(d.len(), 2);
        assert_eq!((d.quads()[0].x, d.quads()[0].y, d.quads()[0].color), (12, 12, [0, 0, 0, 255]));
        assert_eq!((d.quads()[1].x, d.quads()[1].y, d.quads()[1].color), (10, 10, WHITE));
    }

    #[test]
    fn vertices_are_two_triangles_per_quad_in_ndc_and_offscreen_quads_are_dropped() {
        let mut d = DrawList::new();
        d.fill_rect(0, 0, 100, 50, [255, 0, 0, 128]);
        d.fill_rect(500, 0, 10, 10, WHITE); // right of a 100 x 50 target
        d.fill_rect(-20, 10, 10, 10, WHITE); // left of it
        d.fill_rect(95, 45, 100, 100, WHITE); // partly visible
        let v = d.vertices(100, 50, false);
        assert_eq!(v.len(), 12, "two quads of six vertices");
        // The first quad covers the whole target: corners at -1/+1 NDC, y up.
        let xs: Vec<f32> = v[..6].iter().map(|v| v.pos[0]).collect();
        let ys: Vec<f32> = v[..6].iter().map(|v| v.pos[1]).collect();
        assert!(xs.iter().all(|x| *x == -1.0 || *x == 1.0) && ys.iter().all(|y| *y == -1.0 || *y == 1.0));
        assert_eq!(v[0].pos, [-1.0, 1.0], "top left first");
        assert_eq!(v[1].pos, [-1.0, -1.0], "then bottom left");
        // Colour: straight alpha, sRGB bytes as given for a non-sRGB target.
        assert_eq!(v[0].color, [1.0, 0.0, 0.0, 128.0 / 255.0]);
        // For an sRGB target the colour is linearised, alpha is not.
        let lin = d.vertices(100, 50, true);
        assert_eq!(lin[0].color[0], 1.0);
        let mut mid = DrawList::new();
        mid.fill_rect(0, 0, 4, 4, [128, 128, 128, 255]);
        let m = mid.vertices(10, 10, true);
        assert!(m[0].color[0] < 0.25 && m[0].color[0] > 0.2, "{:?}", m[0].color);
        assert_eq!(m[0].color[3], 1.0);
        // uv of a quad: the two triangles cover the same four corners.
        let uvs: std::collections::HashSet<[u32; 2]> = v[..6].iter().map(|v| [v.uv[0].to_bits(), v.uv[1].to_bits()]).collect();
        assert_eq!(uvs.len(), 1, "a plain rectangle samples one texel");
        // A degenerate target does not divide by zero.
        assert!(d.vertices(0, 0, false).iter().all(|v| v.pos.iter().all(|c| c.is_finite())));
        assert_eq!(std::mem::size_of::<UiVertex>(), 32);
    }

    #[test]
    fn glyph_quads_map_their_corners_to_their_cell() {
        let mut d = DrawList::new();
        d.draw_text(0, 0, 2, WHITE, "A");
        let v = d.vertices(64, 64, false);
        let [u0, v0, u1, v1] = cell_uv(cell_of('A'));
        assert_eq!(v[0].uv, [u0, v0]);
        assert_eq!(v[1].uv, [u0, v1]);
        assert_eq!(v[2].uv, [u1, v1]);
        assert_eq!(v[5].uv, [u1, v0]);
    }
}
