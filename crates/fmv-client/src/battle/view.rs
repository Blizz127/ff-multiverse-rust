//! The battle view: a full-window overlay of flat coloured rectangles.
//!
//! Enemies on the top half, the party on the bottom half, one card per fighter: a border, an HP
//! bar and an ATB gauge. A party member whose gauge is full has a yellow border and a yellow
//! gauge (it can be commanded now); our own character has a white marker. A status band across
//! the top is green while the replay agrees with the server, RED after a divergence and grey
//! when this machine has no battle data to replay with.
//!
//! The layout is pure: [`battle_vertices`] turns a [`BattleView`] into the renderer's flat
//! vertices for a window size, and [`battle_labels`] says where the text of each card belongs.
//!
//! TODO(text): sw-charui is building text rendering. When it lands, draw [`battle_labels`] (names
//! and HP numbers); until then the window title carries the words (see
//! `BattleClient::title_suffix`) and nothing draws the labels.

use crate::render::{color, Vertex, MAX_VERTICES};
use fmv_ff7_battle::rules::GAUGE_MAX;
use fmv_protocol::BattleId;

// Palette (sRGB bytes).
pub const BACKDROP_RGB: [u8; 3] = [10, 12, 22];
pub const BAND_VERIFIED_RGB: [u8; 3] = [40, 150, 80];
pub const BAND_DIVERGED_RGB: [u8; 3] = [220, 30, 30];
pub const BAND_UNVERIFIED_RGB: [u8; 3] = [120, 120, 130];
pub const CARD_RGB: [u8; 3] = [26, 30, 46];
pub const BORDER_PARTY_RGB: [u8; 3] = [90, 110, 160];
pub const BORDER_ENEMY_RGB: [u8; 3] = [150, 60, 60];
pub const BORDER_READY_RGB: [u8; 3] = [255, 225, 70];
pub const BORDER_DEAD_RGB: [u8; 3] = [70, 70, 78];
pub const MARKER_RGB: [u8; 3] = [255, 255, 255];
pub const BAR_BACK_RGB: [u8; 3] = [8, 9, 14];
pub const HP_GOOD_RGB: [u8; 3] = [80, 200, 100];
pub const HP_WARN_RGB: [u8; 3] = [230, 180, 60];
pub const HP_LOW_RGB: [u8; 3] = [225, 70, 60];
pub const HP_DEAD_RGB: [u8; 3] = [90, 90, 100];
pub const ATB_RGB: [u8; 3] = [90, 150, 255];
pub const ATB_READY_RGB: [u8; 3] = [255, 225, 70];

// Sizes in pixels.
const BAND_H: f32 = 14.0;
const MARGIN: f32 = 20.0;
const GAP: f32 = 16.0;
const CARD_W_MAX: f32 = 240.0;
const CARD_H: f32 = 84.0;
const BORDER: f32 = 4.0;
const PAD: f32 = 8.0;
const HP_H: f32 = 22.0;
const ATB_H: f32 = 14.0;
const MARKER: f32 = 12.0;

/// How the replay stands against the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The replay agrees with every message so far.
    Verified,
    /// The replay disagreed with the server at least once.
    Diverged,
    /// This machine could not replay the battle (no battle data): only the server knows.
    Unverified,
}

/// One fighter as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    /// The fighter's name from this machine's own data (for the text labels).
    pub name: String,
    pub hp: u32,
    pub max_hp: u32,
    /// ATB gauge, `0..=GAUGE_MAX` (extrapolated between server messages for display).
    pub gauge: u32,
    /// The engine says this fighter can act now (a party member waits for a command).
    pub ready: bool,
    pub alive: bool,
    /// Our own character.
    pub mine: bool,
}

/// Everything the battle view draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleView {
    pub id: BattleId,
    pub party: Vec<Bar>,
    pub enemies: Vec<Bar>,
    pub status: Status,
    /// A command of ours is waiting for the server's answer.
    pub waiting_for_answer: bool,
}

impl BattleView {
    /// A view with nothing to show but the fact that a battle runs and cannot be replayed here.
    pub fn unverified(id: BattleId) -> Self {
        Self { id, party: Vec::new(), enemies: Vec::new(), status: Status::Unverified, waiting_for_answer: false }
    }
}

/// An axis-aligned rectangle in pixels from the top-left corner of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn inset(self, d: f32) -> Rect {
        Rect { x0: self.x0 + d, y0: self.y0 + d, x1: (self.x1 - d).max(self.x0 + d), y1: (self.y1 - d).max(self.y0 + d) }
    }

    fn width(self) -> f32 {
        self.x1 - self.x0
    }
}

/// One fighter's place in the window.
struct Card<'a> {
    rect: Rect,
    bar: &'a Bar,
    enemy: bool,
}

/// Lay one row of cards out in `region`, centred in it.
fn row<'a>(bars: &'a [Bar], enemy: bool, region: Rect, out: &mut Vec<Card<'a>>) {
    let n = bars.len();
    if n == 0 {
        return;
    }
    let slot = region.width() / n as f32;
    let card_w = (slot - GAP).min(CARD_W_MAX).max(8.0);
    let card_h = CARD_H.min((region.y1 - region.y0).max(8.0));
    let y0 = region.y0 + ((region.y1 - region.y0) - card_h).max(0.0) * 0.5;
    for (i, bar) in bars.iter().enumerate() {
        let x0 = region.x0 + slot * i as f32 + (slot - card_w) * 0.5;
        out.push(Card { rect: Rect { x0, y0, x1: x0 + card_w, y1: y0 + card_h }, bar, enemy });
    }
}

/// Every card of `view` for a `w` x `h` window: enemies above the middle, the party below it.
fn cards(view: &BattleView, w: f32, h: f32) -> Vec<Card<'_>> {
    let mut out = Vec::with_capacity(view.party.len() + view.enemies.len());
    let top = BAND_H + MARGIN;
    let mid = (h * 0.5).max(top);
    row(&view.enemies, true, Rect { x0: MARGIN, y0: top, x1: (w - MARGIN).max(MARGIN + 1.0), y1: (mid - MARGIN * 0.5).max(top + 1.0) }, &mut out);
    row(&view.party, false, Rect { x0: MARGIN, y0: mid + MARGIN * 0.5, x1: (w - MARGIN).max(MARGIN + 1.0), y1: (h - MARGIN).max(mid + MARGIN * 0.5 + 1.0) }, &mut out);
    out
}

/// Where the text of a card belongs (see the module docs: nothing draws it yet).
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// Top-left corner in pixels from the top-left of the window.
    pub x: f32,
    pub y: f32,
    pub text: String,
}

/// The labels of `view` for a `vw` x `vh` window: each fighter's name above its card and its HP
/// below it. TODO(text): draw these with the text renderer.
pub fn battle_labels(view: &BattleView, vw: u32, vh: u32) -> Vec<Label> {
    let (w, h) = (vw.max(1) as f32, vh.max(1) as f32);
    let mut labels = Vec::new();
    for card in cards(view, w, h) {
        labels.push(Label { x: card.rect.x0, y: card.rect.y0 - 16.0, text: card.bar.name.clone() });
        labels.push(Label { x: card.rect.x0, y: card.rect.y1 + 2.0, text: format!("HP {}/{}", card.bar.hp, card.bar.max_hp) });
    }
    labels
}

/// Accumulates rectangles given in pixels and emits NDC triangles.
struct Rects {
    out: Vec<Vertex>,
    w: f32,
    h: f32,
    srgb: bool,
}

impl Rects {
    fn rect(&mut self, r: Rect, rgb: [u8; 3]) {
        let ok = [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite());
        if !ok || r.x1 <= r.x0 || r.y1 <= r.y0 || self.out.len() + 6 > MAX_VERTICES {
            return;
        }
        let ndc = |x: f32, y: f32| [x / self.w * 2.0 - 1.0, 1.0 - y / self.h * 2.0];
        let c = color(rgb, self.srgb);
        let (a, b, d, e) = (ndc(r.x0, r.y0), ndc(r.x1, r.y0), ndc(r.x1, r.y1), ndc(r.x0, r.y1));
        for p in [a, b, d, a, d, e] {
            self.out.push(Vertex { pos: p, color: c });
        }
    }
}

/// `part / whole` as a fraction in `0..=1` (an empty whole is empty).
fn fraction(part: u32, whole: u32) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f32 / whole as f32).clamp(0.0, 1.0)
    }
}

/// The colour of an HP bar by how full it is.
fn hp_rgb(bar: &Bar) -> [u8; 3] {
    let f = fraction(bar.hp, bar.max_hp);
    if !bar.alive {
        HP_DEAD_RGB
    } else if f > 0.5 {
        HP_GOOD_RGB
    } else if f > 0.25 {
        HP_WARN_RGB
    } else {
        HP_LOW_RGB
    }
}

/// The frame's vertices for a `vw` x `vh` window (flat colours, painter's order: backdrop,
/// status band, then each card). Non-finite or degenerate rectangles are skipped, never forwarded
/// to the GPU.
pub fn battle_vertices(view: &BattleView, vw: u32, vh: u32, srgb: bool) -> Vec<Vertex> {
    let (w, h) = (vw.max(1) as f32, vh.max(1) as f32);
    let mut r = Rects { out: Vec::with_capacity(256), w, h, srgb };
    r.rect(Rect { x0: 0.0, y0: 0.0, x1: w, y1: h }, BACKDROP_RGB);
    let band = match view.status {
        Status::Verified => BAND_VERIFIED_RGB,
        Status::Diverged => BAND_DIVERGED_RGB,
        Status::Unverified => BAND_UNVERIFIED_RGB,
    };
    r.rect(Rect { x0: 0.0, y0: 0.0, x1: w, y1: BAND_H }, band);

    for card in cards(view, w, h) {
        let bar = card.bar;
        let ready = bar.ready && bar.alive;
        let border = if !bar.alive {
            BORDER_DEAD_RGB
        } else if ready && !card.enemy {
            BORDER_READY_RGB
        } else if card.enemy {
            BORDER_ENEMY_RGB
        } else {
            BORDER_PARTY_RGB
        };
        r.rect(card.rect, border);
        let inner = card.rect.inset(BORDER);
        r.rect(inner, CARD_RGB);

        let bars = inner.inset(PAD - BORDER);
        let hp_back = Rect { x0: bars.x0, y0: bars.y0, x1: bars.x1, y1: (bars.y0 + HP_H).min(bars.y1) };
        r.rect(hp_back, BAR_BACK_RGB);
        let hp_fill = if bar.alive { fraction(bar.hp, bar.max_hp) } else { 0.0 };
        r.rect(Rect { x1: hp_back.x0 + hp_back.width() * hp_fill, ..hp_back }, hp_rgb(bar));

        let atb_top = hp_back.y1 + PAD * 0.5;
        let atb_back = Rect { x0: bars.x0, y0: atb_top, x1: bars.x1, y1: (atb_top + ATB_H).min(bars.y1.max(atb_top)) };
        r.rect(atb_back, BAR_BACK_RGB);
        if bar.alive {
            let (fill, rgb) = if ready { (1.0, ATB_READY_RGB) } else { (fraction(bar.gauge, GAUGE_MAX), ATB) };
            r.rect(Rect { x1: atb_back.x0 + atb_back.width() * fill, ..atb_back }, rgb);
        }

        if bar.mine {
            // A white marker in the card's top-left corner: this one is you.
            r.rect(Rect { x0: card.rect.x0, y0: card.rect.y0, x1: card.rect.x0 + MARKER, y1: card.rect.y0 + MARKER }, MARKER_RGB);
        }
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(hp: u32, max: u32, gauge: u32, ready: bool, alive: bool, mine: bool) -> Bar {
        Bar { name: "x".into(), hp, max_hp: max, gauge, ready, alive, mine }
    }

    fn view(party: Vec<Bar>, enemies: Vec<Bar>, status: Status) -> BattleView {
        BattleView { id: BattleId(1), party, enemies, status, waiting_for_answer: false }
    }

    /// Pixel extent `(min_x, max_x, min_y, max_y)` of the vertices of one colour, in a `w` x `h` window.
    fn extent(v: &[Vertex], rgb: [u8; 3], w: f32, h: f32) -> Option<(f32, f32, f32, f32)> {
        let c = color(rgb, false);
        let mine: Vec<&Vertex> = v.iter().filter(|p| p.color == c).collect();
        if mine.is_empty() {
            return None;
        }
        let xs = mine.iter().map(|p| (p.pos[0] + 1.0) * 0.5 * w);
        let ys = mine.iter().map(|p| (1.0 - p.pos[1]) * 0.5 * h);
        Some((
            xs.clone().fold(f32::MAX, f32::min),
            xs.fold(f32::MIN, f32::max),
            ys.clone().fold(f32::MAX, f32::min),
            ys.fold(f32::MIN, f32::max),
        ))
    }

    fn count(v: &[Vertex], rgb: [u8; 3]) -> usize {
        let c = color(rgb, false);
        v.iter().filter(|p| p.color == c).count()
    }

    #[test]
    fn an_empty_battle_is_a_backdrop_and_a_band() {
        let v = battle_vertices(&view(vec![], vec![], Status::Verified), 800, 600, false);
        assert_eq!(v.len(), 12, "two rectangles");
        assert_eq!(count(&v, BACKDROP_RGB), 6);
        assert_eq!(count(&v, BAND_VERIFIED_RGB), 6);
        // The band is a strip across the top, the backdrop the whole window.
        let (x0, x1, y0, y1) = extent(&v, BAND_VERIFIED_RGB, 800.0, 600.0).unwrap();
        assert!((x0, x1, y0, y1) == (0.0, 800.0, 0.0, BAND_H) || ((x0 - 0.0).abs() < 0.01 && (x1 - 800.0).abs() < 0.01 && (y1 - BAND_H).abs() < 0.01));
        let (bx0, bx1, by0, by1) = extent(&v, BACKDROP_RGB, 800.0, 600.0).unwrap();
        assert!(bx0.abs() < 0.01 && by0.abs() < 0.01 && (bx1 - 800.0).abs() < 0.01 && (by1 - 600.0).abs() < 0.01);
    }

    #[test]
    fn the_status_band_is_green_red_or_grey() {
        for (status, rgb, others) in [
            (Status::Verified, BAND_VERIFIED_RGB, [BAND_DIVERGED_RGB, BAND_UNVERIFIED_RGB]),
            (Status::Diverged, BAND_DIVERGED_RGB, [BAND_VERIFIED_RGB, BAND_UNVERIFIED_RGB]),
            (Status::Unverified, BAND_UNVERIFIED_RGB, [BAND_VERIFIED_RGB, BAND_DIVERGED_RGB]),
        ] {
            let v = battle_vertices(&view(vec![], vec![], status), 400, 300, false);
            assert_eq!(count(&v, rgb), 6, "{status:?}");
            assert!(others.iter().all(|o| count(&v, *o) == 0), "{status:?}");
        }
        assert_eq!(BattleView::unverified(BattleId(3)).status, Status::Unverified);
    }

    #[test]
    fn the_hp_bar_fills_in_proportion_and_changes_colour_as_it_empties() {
        let (w, h) = (1000u32, 700u32);
        let at = |hp: u32| battle_vertices(&view(vec![bar(hp, 100, 0, false, true, false)], vec![], Status::Verified), w, h, false);
        let full = at(100);
        let (fx0, fx1, ..) = extent(&full, HP_GOOD_RGB, w as f32, h as f32).unwrap();
        let (hx0, hx1, ..) = extent(&at(75), HP_GOOD_RGB, w as f32, h as f32).unwrap();
        assert!(((hx1 - hx0) / (fx1 - fx0) - 0.75).abs() < 0.01, "75% fills three quarters: {} of {}", hx1 - hx0, fx1 - fx0);
        // More than half: green; half or less: amber; a quarter or less: red; dead: grey and empty.
        assert!(count(&at(51), HP_GOOD_RGB) > 0 && count(&at(50), HP_GOOD_RGB) == 0);
        assert!(count(&at(50), HP_WARN_RGB) > 0 && count(&at(26), HP_WARN_RGB) > 0 && count(&at(25), HP_WARN_RGB) == 0);
        assert!(count(&at(25), HP_LOW_RGB) > 0 && count(&at(1), HP_LOW_RGB) > 0);
        // No HP at all: nothing is filled.
        assert_eq!(count(&at(0), HP_GOOD_RGB) + count(&at(0), HP_WARN_RGB), 0);
        let dead = battle_vertices(&view(vec![bar(0, 100, 0, false, false, false)], vec![], Status::Verified), w, h, false);
        assert_eq!(count(&dead, HP_GOOD_RGB) + count(&dead, HP_WARN_RGB) + count(&dead, HP_LOW_RGB), 0);
        assert!(count(&dead, BORDER_DEAD_RGB) > 0);
    }

    #[test]
    fn the_atb_gauge_fills_and_a_ready_member_is_highlighted() {
        let (w, h) = (1000u32, 700u32);
        let half = battle_vertices(&view(vec![bar(10, 10, GAUGE_MAX / 2, false, true, true)], vec![], Status::Verified), w, h, false);
        let (a0, a1, ..) = extent(&half, ATB_RGB, w as f32, h as f32).unwrap();
        let full = battle_vertices(&view(vec![bar(10, 10, GAUGE_MAX, true, true, true)], vec![], Status::Verified), w, h, false);
        let (r0, r1, ..) = extent(&full, ATB_READY_RGB, w as f32, h as f32).unwrap();
        assert!(((a1 - a0) / (r1 - r0) - 0.5).abs() < 0.01, "half a gauge: {} of {}", a1 - a0, r1 - r0);
        // Waiting: blue gauge and a plain border; ready: yellow gauge AND a yellow border.
        assert_eq!(count(&half, ATB_READY_RGB) + count(&half, BORDER_READY_RGB), 0);
        assert!(count(&full, BORDER_READY_RGB) > 0 && count(&full, ATB_RGB) == 0);
        assert!(count(&half, BORDER_PARTY_RGB) > 0);
        // A dead member is never "ready", whatever the flag says.
        let ghost = battle_vertices(&view(vec![bar(0, 10, GAUGE_MAX, true, false, false)], vec![], Status::Verified), w, h, false);
        assert_eq!(count(&ghost, BORDER_READY_RGB) + count(&ghost, ATB_READY_RGB), 0);
    }

    #[test]
    fn our_own_character_is_marked_and_the_others_are_not() {
        let (w, h) = (900u32, 600u32);
        let party = vec![bar(5, 10, 0, false, true, false), bar(5, 10, 0, false, true, true), bar(5, 10, 0, false, true, false)];
        let v = battle_vertices(&view(party, vec![], Status::Verified), w, h, false);
        assert_eq!(count(&v, MARKER_RGB), 6, "exactly one marker (one rectangle)");
        let (x0, x1, y0, y1) = extent(&v, MARKER_RGB, w as f32, h as f32).unwrap();
        assert!((x1 - x0 - MARKER).abs() < 0.01 && (y1 - y0 - MARKER).abs() < 0.01);
        // It sits on the middle of three cards.
        assert!(x0 > w as f32 / 3.0 && x1 < w as f32 * 2.0 / 3.0, "{x0}..{x1}");
    }

    #[test]
    fn enemies_are_above_and_the_party_below_and_cards_never_overlap() {
        let (w, h) = (1100u32, 680u32);
        let party: Vec<Bar> = (0..3).map(|_| bar(5, 10, 0, false, true, false)).collect();
        let enemies: Vec<Bar> = (0..4).map(|_| bar(5, 10, 0, false, true, false)).collect();
        let v = view(party, enemies, Status::Verified);
        let cs = cards(&v, w as f32, h as f32);
        assert_eq!(cs.len(), 7);
        let (enemy_cards, party_cards): (Vec<_>, Vec<_>) = cs.iter().partition(|c| c.enemy);
        assert_eq!((enemy_cards.len(), party_cards.len()), (4, 3));
        assert!(enemy_cards.iter().all(|c| c.rect.y1 <= h as f32 / 2.0));
        assert!(party_cards.iter().all(|c| c.rect.y0 >= h as f32 / 2.0));
        for group in [&enemy_cards, &party_cards] {
            for pair in group.windows(2) {
                assert!(pair[0].rect.x1 <= pair[1].rect.x0, "cards in a row do not overlap");
            }
        }
        for c in &cs {
            assert!(c.rect.x0 >= 0.0 && c.rect.x1 <= w as f32 && c.rect.y0 >= BAND_H && c.rect.y1 <= h as f32, "{:?}", c.rect);
        }
    }

    #[test]
    fn labels_name_every_fighter_and_follow_the_cards() {
        let mut p = bar(7, 10, 0, false, true, true);
        p.name = "Cloud".into();
        let mut e = bar(9, 90, 0, false, true, false);
        e.name = "Guard".into();
        let v = view(vec![p], vec![e], Status::Verified);
        let labels = battle_labels(&v, 800, 600);
        let texts: Vec<&str> = labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["Guard", "HP 9/90", "Cloud", "HP 7/10"]);
        assert!(labels.iter().all(|l| l.x.is_finite() && l.y.is_finite()));
        // The enemy's label is above the party's.
        assert!(labels[0].y < labels[2].y);
    }

    #[test]
    fn any_window_size_and_any_numbers_give_finite_vertices_in_the_viewport() {
        let party: Vec<Bar> = (0..8).map(|i| bar(i * 3, 20, i * 9_000, i % 2 == 0, i != 5, i == 2)).collect();
        let enemies: Vec<Bar> = (0..6).map(|i| bar(u32::MAX, 0, u32::MAX, true, true, false).clone_with(i)).collect();
        let v = view(party, enemies, Status::Diverged);
        for (w, h) in [(1, 1), (0, 0), (10, 10), (100, 50), (320, 240), (1100, 680), (4000, 3000)] {
            let out = battle_vertices(&v, w, h, false);
            assert!(!out.is_empty());
            assert_eq!(out.len() % 3, 0);
            assert!(out.len() <= MAX_VERTICES);
            for p in &out {
                assert!(p.pos[0].is_finite() && p.pos[1].is_finite(), "{w}x{h}: {:?}", p.pos);
                assert!(p.color.iter().all(|c| (0.0..=1.0).contains(c)));
            }
            let _ = battle_labels(&v, w, h);
        }
        // An empty max_hp or an absurd gauge neither divides by zero nor overflows the bar.
        assert_eq!(fraction(5, 0), 0.0);
        assert_eq!(fraction(u32::MAX, 1), 1.0);
    }

    trait CloneWith {
        fn clone_with(self, i: u32) -> Self;
    }

    impl CloneWith for Bar {
        fn clone_with(mut self, i: u32) -> Self {
            self.name = format!("e{i}");
            self
        }
    }

    #[test]
    fn colour_conversion_matches_the_renderers() {
        // The overlay uses the renderer's own conversion, so it looks the same on either target.
        let srgb = battle_vertices(&view(vec![], vec![], Status::Verified), 100, 100, true);
        let plain = battle_vertices(&view(vec![], vec![], Status::Verified), 100, 100, false);
        assert_ne!(srgb[0].color, plain[0].color);
        assert_eq!(srgb[0].color, color(BACKDROP_RGB, true));
    }
}
