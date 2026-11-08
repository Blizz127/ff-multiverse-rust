//! The battle view: what the screen shows of a battle, as plain data, and the flat stand-ins.
//!
//! [`BattleView`] is everything the 2D overlay ([`super::panels`], [`super::text`]) draws: the
//! fighters as [`Bar`]s, the status of the replay, and the live parts of the battle UI (the command
//! menu, the floating numbers, the action banner and the victory or defeat panel), all of them
//! plain data that [`super::BattleClient::view`] puts together.
//!
//! Without a 3D scene the fighters are drawn as flat tokens, one coloured box each with a thin HP
//! bar, in the strip of the window above the bottom windows: [`battle_vertices`] turns a view into
//! the renderer's flat vertices (the backdrop, the status band, the tokens) and [`flat_anchors`]
//! says where each token's head is. A 3D scene gives its own [`Anchors`] (where the models'
//! heads are on screen) to [`super::text::draw_battle_overlay`] instead, and draws no tokens.
//!
//! The layout is pure: no clock, no GPU.

use super::menu::MenuView;
use super::panels::{layout, Banner, Finale};
use super::popups::Popup;
use crate::render::{color, Vertex, MAX_VERTICES};
use fmv_ff7_battle::rules::{ActorId, GAUGE_MAX, LIMIT_MAX};
use fmv_protocol::BattleId;

// Palette (sRGB bytes).
pub const BACKDROP_RGB: [u8; 3] = [10, 12, 22];
pub const BAND_VERIFIED_RGB: [u8; 3] = [40, 150, 80];
pub const BAND_DIVERGED_RGB: [u8; 3] = [220, 30, 30];
pub const BAND_UNVERIFIED_RGB: [u8; 3] = [120, 120, 130];
pub const CARD_RGB: [u8; 3] = [26, 30, 46];
pub const BORDER_PARTY_RGB: [u8; 3] = [90, 110, 160];
pub const BORDER_ENEMY_RGB: [u8; 3] = [150, 60, 60];
pub const BORDER_READY_RGB: [u8; 3] = [255, 200, 40];
pub const BORDER_DEAD_RGB: [u8; 3] = [70, 70, 78];
pub const MARKER_RGB: [u8; 3] = [255, 255, 255];
pub const BAR_BACK_RGB: [u8; 3] = [8, 9, 14];
pub const HP_GOOD_RGB: [u8; 3] = [80, 200, 100];
pub const HP_WARN_RGB: [u8; 3] = [230, 180, 60];
pub const HP_LOW_RGB: [u8; 3] = [225, 70, 60];
pub const HP_DEAD_RGB: [u8; 3] = [90, 90, 100];
pub const ATB_RGB: [u8; 3] = [90, 150, 255];
pub const ATB_READY_RGB: [u8; 3] = [255, 238, 120];
/// The limit gauge filling, and full (FF7 draws it in an orange that turns bright when the limit is ready).
pub const LIMIT_RGB: [u8; 3] = [225, 120, 40];
pub const LIMIT_FULL_RGB: [u8; 3] = [255, 190, 60];

/// Height of the status band across the top of the window, in pixels.
pub const BAND_H: f32 = 14.0;

// Token sizes in canvas units (see [`super::panels::Layout`]; a unit is `scale` pixels).
const TOKEN_W: f32 = 26.0;
const TOKEN_H: f32 = 34.0;
const TOKEN_GAP: f32 = 6.0;
const BORDER: f32 = 1.0;
const MARKER: f32 = 4.0;
const HP_H: f32 = 3.0;

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

impl Status {
    /// The colour of the status band.
    pub fn band_rgb(self) -> [u8; 3] {
        match self {
            Status::Verified => BAND_VERIFIED_RGB,
            Status::Diverged => BAND_DIVERGED_RGB,
            Status::Unverified => BAND_UNVERIFIED_RGB,
        }
    }
}

/// One fighter as the view draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    /// Who it is in the battle (what events and commands call it).
    pub actor: ActorId,
    /// The fighter's name from this machine's own data.
    pub name: String,
    pub hp: u32,
    pub max_hp: u32,
    pub mp: u32,
    pub max_mp: u32,
    /// ATB gauge, `0..=GAUGE_MAX` (extrapolated between server messages for display).
    pub gauge: u32,
    /// The engine says this fighter can act now (a party member waits for a command).
    pub ready: bool,
    pub alive: bool,
    /// Our own character.
    pub mine: bool,
    /// The fighter's limit gauge, `0..=`[`LIMIT_MAX`] (protocol v7; always 0 for an enemy and for a
    /// character that has no limit). Unlike the ATB gauge it is not extrapolated: it only moves with
    /// the server's messages.
    pub limit: u16,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            actor: ActorId(0),
            name: String::new(),
            hp: 0,
            max_hp: 0,
            mp: 0,
            max_mp: 0,
            gauge: 0,
            ready: false,
            alive: false,
            mine: false,
            limit: 0,
        }
    }
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
    /// Our character's command menu (only on its turn).
    pub menu: Option<MenuView>,
    /// Damage, healing and misses floating over the fighters.
    pub popups: Vec<Popup>,
    /// The name of the action being performed.
    pub banner: Option<Banner>,
    /// The battle is over: the victory or defeat panel (the view lingers after the end).
    pub finale: Option<Finale>,
}

impl BattleView {
    /// A view with nothing to show but the fact that a battle runs and cannot be replayed here.
    pub fn unverified(id: BattleId) -> Self {
        Self {
            id,
            party: Vec::new(),
            enemies: Vec::new(),
            status: Status::Unverified,
            waiting_for_answer: false,
            menu: None,
            popups: Vec::new(),
            banner: None,
            finale: None,
        }
    }
}

impl Default for BattleView {
    fn default() -> Self {
        Self::unverified(BattleId(0))
    }
}

/// Where the fighters are on screen: for each one the point just over its head, in window pixels
/// from the top-left corner (the pointer and the floating numbers go there), in the order of
/// [`BattleView::party`] and [`BattleView::enemies`]. `None` for one that is not on screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Anchors {
    pub party: Vec<Option<(f32, f32)>>,
    pub enemies: Vec<Option<(f32, f32)>>,
}

impl Anchors {
    /// The anchor of `actor` in `view`, if it has one on screen.
    pub fn of(&self, view: &BattleView, actor: ActorId) -> Option<(f32, f32)> {
        let found = match view.party.iter().position(|b| b.actor == actor) {
            Some(i) => self.party.get(i),
            None => self.enemies.get(view.enemies.iter().position(|b| b.actor == actor)?),
        };
        found.copied().flatten().filter(|(x, y)| x.is_finite() && y.is_finite())
    }
}

/// An axis-aligned rectangle in pixels from the top-left corner of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    fn inset(self, d: f32) -> Rect {
        Rect { x0: self.x0 + d, y0: self.y0 + d, x1: (self.x1 - d).max(self.x0 + d), y1: (self.y1 - d).max(self.y0 + d) }
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }
}

/// One fighter's token.
pub(super) struct Card<'a> {
    pub rect: Rect,
    pub bar: &'a Bar,
    pub enemy: bool,
    /// The width of the slot the token stands in (its name may use all of it).
    pub slot: f32,
}

/// Lay one row of tokens out in `region`, centred in it.
fn row<'a>(bars: &'a [Bar], enemy: bool, region: Rect, s: f32, out: &mut Vec<Card<'a>>) {
    let n = bars.len();
    if n == 0 {
        return;
    }
    let slot = region.width() / n as f32;
    let card_w = (slot - TOKEN_GAP * s).clamp(8.0, TOKEN_W * s);
    let card_h = (TOKEN_H * s).min(region.height().max(8.0));
    let y0 = region.y0 + (region.height() - card_h).max(0.0) * 0.5;
    for (i, bar) in bars.iter().enumerate() {
        let x0 = region.x0 + slot * i as f32 + (slot - card_w) * 0.5;
        out.push(Card { rect: Rect { x0, y0, x1: x0 + card_w, y1: y0 + card_h }, bar, enemy, slot });
    }
}

/// Every token of `view` for a `w` x `h` window: the enemies on the left, the party on the right,
/// in the strip above the bottom windows.
pub(super) fn cards(view: &BattleView, vw: u32, vh: u32) -> Vec<Card<'_>> {
    let l = layout(vw, vh, view.party.len());
    let s = l.s as f32;
    let (x0, y0, x1, y1) = l.stage();
    let (x0, y0, x1, y1) = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
    let split = x0 + (x1 - x0) * 0.56;
    let gap = TOKEN_GAP * s * 0.5;
    let mut out = Vec::with_capacity(view.party.len() + view.enemies.len());
    row(&view.enemies, true, Rect { x0, y0, x1: (split - gap).max(x0 + 1.0), y1: y1.max(y0 + 1.0) }, s, &mut out);
    row(&view.party, false, Rect { x0: (split + gap).min(x1), y0, x1: x1.max(split + gap + 1.0), y1: y1.max(y0 + 1.0) }, s, &mut out);
    out
}

/// Where the flat tokens' heads are (the point over the middle of each token's top edge).
pub fn flat_anchors(view: &BattleView, vw: u32, vh: u32) -> Anchors {
    let s = layout(vw, vh, view.party.len()).s as f32;
    let mut anchors = Anchors { party: vec![None; view.party.len()], enemies: vec![None; view.enemies.len()] };
    let (mut enemy, mut party) = (0, 0);
    for card in cards(view, vw, vh) {
        let at = Some(((card.rect.x0 + card.rect.x1) * 0.5, card.rect.y0 - 2.0 * s));
        let (list, i) = if card.enemy { (&mut anchors.enemies, &mut enemy) } else { (&mut anchors.party, &mut party) };
        list[*i] = at;
        *i += 1;
    }
    anchors
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
pub(super) fn fraction(part: u32, whole: u32) -> f32 {
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

/// The frame's flat vertices for a `vw` x `vh` window, painter's order: the backdrop, the status
/// band, then a token for each fighter. Non-finite or degenerate rectangles are skipped, never
/// forwarded to the GPU. (The overlay draws the band again, in the same colour, so a 3D scene
/// that skips this function still has one.)
pub fn battle_vertices(view: &BattleView, vw: u32, vh: u32, srgb: bool) -> Vec<Vertex> {
    let (w, h) = (vw.max(1) as f32, vh.max(1) as f32);
    let s = layout(vw, vh, view.party.len()).s as f32;
    let mut r = Rects { out: Vec::with_capacity(256), w, h, srgb };
    r.rect(Rect { x0: 0.0, y0: 0.0, x1: w, y1: h }, BACKDROP_RGB);
    r.rect(Rect { x0: 0.0, y0: 0.0, x1: w, y1: BAND_H }, view.status.band_rgb());

    for card in cards(view, vw, vh) {
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
        let inner = card.rect.inset(BORDER * s);
        r.rect(inner, CARD_RGB);

        // A thin HP bar along the bottom of the token.
        let pad = 2.0 * s;
        let hp_back = Rect { x0: inner.x0 + pad, y0: (inner.y1 - pad - HP_H * s).max(inner.y0), x1: (inner.x1 - pad).max(inner.x0 + pad), y1: (inner.y1 - pad).max(inner.y0) };
        r.rect(hp_back, BAR_BACK_RGB);
        let hp_fill = if bar.alive { fraction(bar.hp, bar.max_hp) } else { 0.0 };
        r.rect(Rect { x1: hp_back.x0 + hp_back.width() * hp_fill, ..hp_back }, hp_rgb(bar));

        if bar.mine {
            // A white marker in the token's top-left corner: this one is you.
            let m = MARKER * s;
            r.rect(Rect { x0: card.rect.x0, y0: card.rect.y0, x1: card.rect.x0 + m, y1: card.rect.y0 + m }, MARKER_RGB);
        }
    }
    r.out
}

/// The gauge of `bar` as a fraction of a full one (the ATB bars use it).
pub(super) fn gauge_fraction(bar: &Bar) -> f32 {
    if bar.ready && bar.alive {
        1.0
    } else {
        fraction(bar.gauge, GAUGE_MAX)
    }
}

/// The limit gauge of `bar` as a fraction of a full one (the limit bars use it): a fallen character's is
/// empty whatever it says.
pub(super) fn limit_fraction(bar: &Bar) -> f32 {
    if bar.alive {
        fraction(u32::from(bar.limit), u32::from(LIMIT_MAX))
    } else {
        0.0
    }
}

/// Whether `bar`'s limit gauge is full (and its owner stands): the limit can be given.
pub(super) fn limit_full(bar: &Bar) -> bool {
    bar.alive && bar.limit >= LIMIT_MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(hp: u32, max: u32, ready: bool, alive: bool, mine: bool) -> Bar {
        Bar { name: "x".into(), hp, max_hp: max, ready, alive, mine, ..Bar::default() }
    }

    fn view(party: Vec<Bar>, enemies: Vec<Bar>, status: Status) -> BattleView {
        BattleView { id: BattleId(1), party, enemies, status, ..BattleView::default() }
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
        Some((xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max), ys.clone().fold(f32::MAX, f32::min), ys.fold(f32::MIN, f32::max)))
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
        assert!(x0.abs() < 0.01 && y0.abs() < 0.01 && (x1 - 800.0).abs() < 0.01 && (y1 - BAND_H).abs() < 0.01);
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
            assert_eq!(status.band_rgb(), rgb);
        }
        assert_eq!(BattleView::unverified(BattleId(3)).status, Status::Unverified);
        assert_eq!(BattleView::default().id, BattleId(0));
    }

    #[test]
    fn the_hp_bar_fills_in_proportion_and_changes_colour_as_it_empties() {
        let (w, h) = (1000u32, 700u32);
        let at = |hp: u32| battle_vertices(&view(vec![bar(hp, 100, false, true, false)], vec![], Status::Verified), w, h, false);
        let full = at(100);
        let (fx0, fx1, ..) = extent(&full, HP_GOOD_RGB, w as f32, h as f32).unwrap();
        let (hx0, hx1, ..) = extent(&at(75), HP_GOOD_RGB, w as f32, h as f32).unwrap();
        assert!(((hx1 - hx0) / (fx1 - fx0) - 0.75).abs() < 0.01, "75% fills three quarters: {} of {}", hx1 - hx0, fx1 - fx0);
        // More than half: green; half or less: amber; a quarter or less: red; dead: grey and empty.
        assert!(count(&at(51), HP_GOOD_RGB) > 0 && count(&at(50), HP_GOOD_RGB) == 0);
        assert!(count(&at(50), HP_WARN_RGB) > 0 && count(&at(26), HP_WARN_RGB) > 0 && count(&at(25), HP_WARN_RGB) == 0);
        assert!(count(&at(25), HP_LOW_RGB) > 0 && count(&at(1), HP_LOW_RGB) > 0);
        assert_eq!(count(&at(0), HP_GOOD_RGB) + count(&at(0), HP_WARN_RGB), 0, "no HP at all: nothing is filled");
        let dead = battle_vertices(&view(vec![bar(0, 100, false, false, false)], vec![], Status::Verified), w, h, false);
        assert_eq!(count(&dead, HP_GOOD_RGB) + count(&dead, HP_WARN_RGB) + count(&dead, HP_LOW_RGB), 0);
        assert!(count(&dead, BORDER_DEAD_RGB) > 0);
    }

    #[test]
    fn a_ready_member_is_highlighted_and_a_dead_one_never_is() {
        let (w, h) = (1000u32, 700u32);
        let waiting = battle_vertices(&view(vec![bar(10, 10, false, true, true)], vec![], Status::Verified), w, h, false);
        let ready = battle_vertices(&view(vec![bar(10, 10, true, true, true)], vec![], Status::Verified), w, h, false);
        assert_eq!(count(&waiting, BORDER_READY_RGB), 0);
        assert!(count(&ready, BORDER_READY_RGB) > 0 && count(&ready, BORDER_PARTY_RGB) == 0);
        assert!(count(&waiting, BORDER_PARTY_RGB) > 0);
        let ghost = battle_vertices(&view(vec![bar(0, 10, true, false, false)], vec![], Status::Verified), w, h, false);
        assert_eq!(count(&ghost, BORDER_READY_RGB), 0);
        // An enemy that is "ready" is still an enemy.
        let foe = battle_vertices(&view(vec![], vec![bar(10, 10, true, true, false)], Status::Verified), w, h, false);
        assert!(count(&foe, BORDER_ENEMY_RGB) > 0 && count(&foe, BORDER_READY_RGB) == 0);
    }

    #[test]
    fn our_own_character_is_marked_and_the_others_are_not() {
        let (w, h) = (900u32, 600u32);
        let party = vec![bar(5, 10, false, true, false), bar(5, 10, false, true, true), bar(5, 10, false, true, false)];
        let v = battle_vertices(&view(party, vec![], Status::Verified), w, h, false);
        assert_eq!(count(&v, MARKER_RGB), 6, "exactly one marker (one rectangle)");
        let (x0, x1, y0, y1) = extent(&v, MARKER_RGB, w as f32, h as f32).unwrap();
        assert!((x1 - x0 - y1 + y0).abs() < 0.01, "a square");
    }

    #[test]
    fn enemies_are_on_the_left_the_party_on_the_right_and_tokens_stay_above_the_windows() {
        for (w, h) in [(1100u32, 680u32), (1920, 1080), (800, 600), (640, 480), (320, 240)] {
            let party: Vec<Bar> = (0..3).map(|i| Bar { actor: ActorId(i), ..bar(5, 10, false, true, false) }).collect();
            let enemies: Vec<Bar> = (0..4).map(|i| Bar { actor: ActorId(3 + i), ..bar(5, 10, false, true, false) }).collect();
            let v = view(party, enemies, Status::Verified);
            let cs = cards(&v, w, h);
            assert_eq!(cs.len(), 7);
            let (enemy_cards, party_cards): (Vec<_>, Vec<_>) = cs.iter().partition(|c| c.enemy);
            assert_eq!((enemy_cards.len(), party_cards.len()), (4, 3));
            let l = layout(w, h, 3);
            let (_, _, _, stage_bottom) = l.stage();
            let enemy_right = enemy_cards.iter().map(|c| c.rect.x1).fold(0.0, f32::max);
            let party_left = party_cards.iter().map(|c| c.rect.x0).fold(f32::MAX, f32::min);
            assert!(enemy_right <= party_left, "{w}x{h}: the enemies' side ends before the party's begins ({enemy_right} vs {party_left})");
            for group in [&enemy_cards, &party_cards] {
                for pair in group.windows(2) {
                    assert!(pair[0].rect.x1 <= pair[1].rect.x0, "{w}x{h}: tokens in a row do not overlap");
                }
            }
            let windows_top = (l.enemy.y * l.s) as f32;
            for c in &cs {
                assert!(c.rect.x0 >= 0.0 && c.rect.x1 <= w as f32 && c.rect.y0 >= BAND_H, "{w}x{h}: {:?}", c.rect);
                assert!(c.rect.y1 <= stage_bottom as f32 + 0.5 && c.rect.y1 <= windows_top, "{w}x{h}: a token reaches into the bottom windows: {:?} vs {windows_top}", c.rect);
            }
        }
    }

    #[test]
    fn anchors_sit_over_the_tokens_in_view_order_and_are_found_by_actor() {
        let party = vec![Bar { actor: ActorId(0), ..bar(5, 10, false, true, true) }, Bar { actor: ActorId(1), ..bar(5, 10, false, true, false) }];
        let enemies = vec![Bar { actor: ActorId(2), ..bar(5, 10, false, true, false) }];
        let v = view(party, enemies, Status::Verified);
        let a = flat_anchors(&v, 1100, 680);
        assert_eq!((a.party.len(), a.enemies.len()), (2, 1));
        let cs = cards(&v, 1100, 680);
        let enemy = cs.iter().find(|c| c.enemy).unwrap();
        let (ex, ey) = a.of(&v, ActorId(2)).unwrap();
        assert!((ex - (enemy.rect.x0 + enemy.rect.x1) * 0.5).abs() < 0.01 && ey < enemy.rect.y0);
        let (x0, _) = a.of(&v, ActorId(0)).unwrap();
        let (x1, _) = a.of(&v, ActorId(1)).unwrap();
        assert!(x0 < x1, "the party's order is kept");
        assert!(x0 > ex, "the party is on the right of the enemies");
        assert_eq!(a.of(&v, ActorId(9)), None, "an actor the view does not have");
        // A 3D scene's anchors with a hole or a non-finite point give nothing.
        let holey = Anchors { party: vec![None, Some((f32::NAN, 1.0))], enemies: vec![] };
        assert_eq!((holey.of(&v, ActorId(0)), holey.of(&v, ActorId(1)), holey.of(&v, ActorId(2))), (None, None, None));
    }

    #[test]
    fn any_window_size_and_any_numbers_give_finite_vertices_in_the_viewport() {
        let party: Vec<Bar> = (0..8).map(|i| Bar { actor: ActorId(i as u8), ..bar(i * 3, 20, i % 2 == 0, i != 5, i == 2) }).collect();
        let enemies: Vec<Bar> = (0..6).map(|_| bar(u32::MAX, 0, true, true, false)).collect();
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
            let _ = flat_anchors(&v, w, h);
        }
        // An empty max_hp or an absurd gauge neither divides by zero nor overflows the bar.
        assert_eq!(fraction(5, 0), 0.0);
        assert_eq!(fraction(u32::MAX, 1), 1.0);
        assert_eq!(gauge_fraction(&Bar { ready: true, alive: true, gauge: 0, ..Bar::default() }), 1.0);
        assert_eq!(gauge_fraction(&Bar { ready: true, alive: false, gauge: 0, ..Bar::default() }), 0.0);
        assert_eq!(gauge_fraction(&Bar { gauge: u32::MAX, ..Bar::default() }), 1.0);
    }

    #[test]
    fn the_limit_gauge_is_a_fraction_of_255_that_a_fallen_character_does_not_show() {
        let limit = |limit: u16, alive: bool| Bar { limit, alive, ..Bar::default() };
        assert_eq!(limit_fraction(&limit(0, true)), 0.0);
        assert_eq!(limit_fraction(&limit(255, true)), 1.0);
        assert!((limit_fraction(&limit(51, true)) - 0.2).abs() < 1e-6);
        assert_eq!(limit_fraction(&limit(u16::MAX, true)), 1.0, "an absurd number does not overflow the bar");
        assert_eq!(limit_fraction(&limit(255, false)), 0.0, "the fallen show an empty gauge");
        assert!(limit_full(&limit(255, true)) && !limit_full(&limit(254, true)) && !limit_full(&limit(255, false)));
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
