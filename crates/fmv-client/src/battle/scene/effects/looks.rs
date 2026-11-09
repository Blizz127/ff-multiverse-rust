//! The shapes of the effects: for each [`Kind`] and instant, the handful of pieces ([`Layer`]) that make it. Everything here is
//! a pure function of the effect's age (`0..1` of its life after the impact, or `0..1` of the way to the impact before it),
//! its strength and its seed: no state, no allocation but the pieces appended to the caller's list.
//!
//! Sizes are fractions of the anchor's height `h` (a boss gets a bigger effect than a hound) times the effect's strength
//! `s`. Offsets are along the camera's axes from the middle of the anchor's chest: `x` right, `y` DOWN (the feet are at
//! about `+0.5 h`, the head at `-0.5 h`), `z` towards the camera.

// The drawing helpers take positional numbers (where, how big, which way, what colour, how bright): a list of them is how an
// effect reads, and a struct for each would only hide it.
#![allow(clippy::too_many_arguments)]

use super::{noise, status_rgb, Element, Fx, Geo, Kind, Layer, Phase, Prim, Strike};
use fmv_ff7_battle::status::Status;
use std::f32::consts::{PI, TAU};

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const GOLD: [f32; 3] = [1.0, 0.82, 0.3];
const YELLOW: [f32; 3] = [1.0, 0.9, 0.4];
/// How much bigger than its target a spell's blast is drawn: the party is a few dozen pixels tall in a window, and a spell fills
/// more of the screen than the body it lands on.
const BLAST_SIZE: f32 = 1.3;
/// The widest half-width of the streak mesh at an `x` scale of 1 (see [`Prim::Streak`]).
const STREAK_WIDTH: f32 = 0.14;

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

/// What is left of an effect `age` (`0..1`) into its fade: `(1 - age) ^ power`.
fn fade(age: f32, power: f32) -> f32 {
    (1.0 - age.clamp(0.0, 1.0)).powf(power)
}

/// Out-and-slow growth from `a` to `b`.
fn grow(age: f32, a: f32, b: f32) -> f32 {
    let x = age.clamp(0.0, 1.0);
    a + (b - a) * (1.0 - (1.0 - x) * (1.0 - x))
}

/// `0` before `a`, `1` after `b`, smooth in between.
fn ramp(x: f32, a: f32, b: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The turn that points a piece's long axis along `(dx, dy)` on the screen (`x` right, `y` down).
fn spin_along(dx: f32, dy: f32) -> f32 {
    (-dx).atan2(dy)
}

/// How an effect is drawn into a list of pieces.
struct Ctx<'a> {
    out: &'a mut Vec<Layer>,
    /// The anchor's height.
    h: f32,
    /// The effect's strength.
    s: f32,
    seed: u32,
    /// The element's colour.
    col: [f32; 3],
    /// Every piece is a little towards the camera, so that the body it surrounds does not hide it.
    lift: f32,
}

impl Ctx<'_> {
    fn n(&self, k: u32) -> f32 {
        noise(self.seed, k)
    }

    fn at(&self, x: f32, y: f32) -> [f32; 3] {
        [x, y, self.lift]
    }

    fn disc(&mut self, x: f32, y: f32, r: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::round(Prim::Disc, at, r, 0.0, rgb, a));
    }

    fn glow(&mut self, x: f32, y: f32, hx: f32, hy: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::oval(Prim::Disc, at, [hx, hy], 0.0, rgb, a));
    }

    fn star(&mut self, x: f32, y: f32, r: f32, spin: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::round(Prim::Star, at, r, spin, rgb, a));
    }

    fn ring(&mut self, x: f32, y: f32, rx: f32, ry: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::oval(Prim::Ring, at, [rx, ry], 0.0, rgb, a));
    }

    /// A streak (a spindle) whose widest half-width is `hx` and whose half-length is `hy` before it is turned by `spin`.
    fn streak(&mut self, x: f32, y: f32, hx: f32, hy: f32, spin: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::oval(Prim::Streak, at, [hx / STREAK_WIDTH, hy], spin, rgb, a));
    }

    /// A curved blade whose arc bulges towards `spin`.
    fn blade(&mut self, x: f32, y: f32, r: f32, spin: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::round(Prim::Crescent, at, r, spin, rgb, a));
    }

    /// A lightning bolt of half-width `hx` and half-length `hy`.
    fn zig(&mut self, x: f32, y: f32, hx: f32, hy: f32, spin: f32, rgb: [f32; 3], a: f32) {
        let at = self.at(x, y);
        self.out.push(Layer::oval(Prim::Bolt, at, [hx, hy], spin, rgb, a));
    }

    /// Specks of `rgb` that fly out of the middle and fall: `count` of them, `reach` of `h` far, over the age `a`.
    fn sparks(&mut self, count: u32, first: u32, reach: f32, size: f32, rgb: [f32; 3], a: f32, life_power: f32) {
        for k in 0..count {
            let th = self.n(first + k) * TAU;
            let v = (0.45 + 0.9 * self.n(first + 40 + k)) * reach * self.h * self.s;
            let t = a * (1.0 - 0.35 * a);
            let (x, y) = (th.cos() * v * t, th.sin() * v * t + 0.45 * self.h * a * a);
            let r = (0.6 + 0.8 * self.n(first + 80 + k)) * size * self.h * self.s;
            self.star(x, y, r, a * 5.0 + k as f32, rgb, fade(a, life_power));
        }
    }
}

/// Appends to `out` the pieces of `fx` in `phase`.
pub(super) fn draw(fx: &Fx, phase: Phase, geo: &Geo, out: &mut Vec<Layer>) {
    let h = geo.height.clamp(300.0, 3000.0);
    let col = fx.element.rgb();
    let mut c = Ctx { out, h, s: fx.scale.clamp(0.3, 2.0), seed: fx.seed, col, lift: 0.1 * h };
    match (fx.kind, phase) {
        (Kind::Strike { how, critical }, Phase::Age(a)) => strike(&mut c, how, critical, a),
        (Kind::Impact { critical }, Phase::Age(a)) => impact(&mut c, critical, a),
        (Kind::Whiff, Phase::Age(a)) => whiff(&mut c, a),
        (Kind::Blast, Phase::Lead(p)) => gather(&mut c, p, col, 1.0),
        (Kind::Blast, Phase::Age(a)) => blast(&mut c, fx.element, a),
        (Kind::Mend, Phase::Lead(p)) => gather(&mut c, p, Element::Cure.rgb(), 0.8),
        (Kind::Mend, Phase::Age(a)) => mend(&mut c, Element::Cure.rgb(), a, true),
        (Kind::Mana, Phase::Lead(p)) => gather(&mut c, p, Element::Water.rgb(), 0.8),
        (Kind::Mana, Phase::Age(a)) => mend(&mut c, mix(Element::Water.rgb(), WHITE, 0.15), a, false),
        (Kind::Raise, Phase::Lead(p)) => gather(&mut c, p, GOLD, 0.9),
        (Kind::Raise, Phase::Age(a)) => raise(&mut c, a),
        (Kind::Afflict(st), Phase::Lead(p)) => gather(&mut c, p, status_rgb(st), 0.7),
        (Kind::Afflict(st), Phase::Age(a)) => afflict(&mut c, st, a),
        (Kind::Boost(st), Phase::Lead(p)) => gather(&mut c, p, status_rgb(st), 0.7),
        (Kind::Boost(st), Phase::Age(a)) => boost(&mut c, st, a),
        (Kind::Cleanse(st), Phase::Age(a)) => cleanse(&mut c, st, a),
        (Kind::Charge, phase) => charge(&mut c, phase),
        (Kind::Bolt { hit }, phase) => bolt(&mut c, phase, geo.from, hit),
        (Kind::Toss, Phase::Lead(p)) => toss(&mut c, p, geo.from),
        (Kind::Guard, Phase::Age(a)) => guard(&mut c, a),
        (Kind::Fizzle, Phase::Age(a)) => fizzle(&mut c, a),
        (Kind::Flash, Phase::Age(a)) => flash(&mut c, a),
        // The rest of the table: an effect that only happens at the impact has no lead, and a flying one no age.
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Blows and shots
// ---------------------------------------------------------------------------------------------

/// A blow lands. Slashes cross the target, a thump rings, a punch bursts; a critical hit is bigger, gold and has a ring.
fn strike(c: &mut Ctx, how: Strike, critical: bool, a: f32) {
    let h = c.h;
    c.s *= if critical { 1.3 } else { 1.0 };
    let s = c.s;
    let tint = if critical { GOLD } else { mix(WHITE, c.col, 0.55) };
    match how {
        Strike::Slash => {
            let slashes = if critical { 3 } else { 2 };
            for k in 0..slashes {
                let ang = (k as f32 - (slashes - 1) as f32 * 0.5) * 0.95 + (c.n(k) - 0.5) * 0.5 - 0.35;
                let (dx, dy) = (ang.cos(), ang.sin());
                let r = 0.95 * h * s * grow(a, 0.82, 1.0);
                // The arc is the far rim of its circle: the circle's middle is behind the target so that the arc crosses it.
                c.blade(-dx * 0.74 * r, -dy * 0.74 * r, r, ang, tint, fade(a, 1.7));
            }
            // The cut itself, a streak that is there for an instant.
            c.streak(0.0, 0.0, 0.014 * h * s, 0.85 * h * s * grow(a, 0.6, 1.0), spin_along(0.55, 1.0), mix(tint, WHITE, 0.4), fade(a, 3.5));
        }
        Strike::Thump => {
            c.ring(0.0, 0.0, grow(a, 0.18, 0.9) * h * s, grow(a, 0.18, 0.9) * h * s, tint, fade(a, 1.5) * 0.9);
            for k in 0..6 {
                let th = (k as f32 + c.n(k)) / 6.0 * TAU;
                let d = grow(a, 0.08, 0.6 + 0.2 * c.n(9 + k)) * h * s;
                c.disc(th.cos() * d, th.sin() * d * 0.8, 0.07 * h * s * (1.0 - a * 0.5), mix(tint, c.col, 0.3), fade(a, 1.6) * 0.55);
            }
        }
        Strike::Punch => {
            c.star(0.0, 0.0, grow(a, 0.45, 0.85) * h * s, a * 0.9, tint, fade(a, 1.4));
            c.ring(0.0, 0.0, grow(a, 0.15, 0.55) * h * s, grow(a, 0.15, 0.55) * h * s, tint, fade(a, 1.8) * 0.8);
        }
    }
    // The flash of the blow and the sparks that fly off it.
    c.disc(0.0, 0.0, grow(a, 0.22, 0.46) * h * s, mix(tint, c.col, 0.2), fade(a, 2.6));
    c.star(0.0, 0.0, grow(a, 0.3, 0.62) * h * s, a * 0.6, tint, fade(a, 1.8));
    c.sparks(if critical { 9 } else { 6 }, 10, 0.95, 0.035, mix(tint, WHITE, 0.5), a, 1.1);
    if critical {
        c.ring(0.0, 0.0, grow(a, 0.25, 1.25) * h * s, grow(a, 0.25, 1.25) * h * s, GOLD, fade(a, 1.4) * 0.9);
    }
}

/// A shot lands: a small flash and a few sparks.
fn impact(c: &mut Ctx, critical: bool, a: f32) {
    let h = c.h;
    c.s *= if critical { 1.4 } else { 1.0 };
    let s = c.s;
    let tint = if critical { GOLD } else { mix(WHITE, c.col, 0.3) };
    c.disc(0.0, 0.0, grow(a, 0.12, 0.26) * h * s, mix(tint, WHITE, 0.4), fade(a, 2.2));
    c.star(0.0, 0.0, grow(a, 0.2, 0.46) * h * s, a * 1.3, tint, fade(a, 1.7));
    c.ring(0.0, 0.0, grow(a, 0.1, 0.5) * h * s, grow(a, 0.1, 0.5) * h * s, tint, fade(a, 1.6) * 0.7);
    c.sparks(if critical { 6 } else { 4 }, 20, 0.7, 0.03, tint, a, 1.1);
}

/// A blow found nobody there: a faint swoosh beside the target and a little dust.
fn whiff(c: &mut Ctx, a: f32) {
    let (h, s) = (c.h, c.s);
    let side = if c.n(0) < 0.5 { -1.0 } else { 1.0 };
    let pale = [0.82, 0.88, 1.0];
    c.blade(side * 0.3 * h, -0.04 * h, 0.5 * h * s * grow(a, 0.7, 1.0), if side > 0.0 { 0.0 } else { PI }, pale, fade(a, 1.3) * 0.8);
    for k in 0..3 {
        let x = side * (0.3 + 0.2 * c.n(k)) * h;
        c.disc(x, (c.n(5 + k) - 0.5) * 0.4 * h - a * 0.12 * h, 0.05 * h, pale, fade(a, 1.2) * 0.5);
    }
}

// ---------------------------------------------------------------------------------------------
// Spells
// ---------------------------------------------------------------------------------------------

/// Before a spell breaks: faint motes in the spell's colour close in on the target.
fn gather(c: &mut Ctx, p: f32, rgb: [f32; 3], strength: f32) {
    let h = c.h;
    let close = ramp(p, 0.0, 1.0);
    let ground = lerp(1.0, 0.4, close) * h;
    c.ring(0.0, 0.5 * h, ground, ground * 0.38, rgb, (0.1 + 0.4 * p) * strength);
    for k in 0..6u32 {
        let ang = (k as f32 / 6.0 + c.n(k) * 0.15) * TAU + p * 1.6;
        let r = lerp(0.95, 0.12, close) * h;
        c.star(ang.cos() * r * 0.8, ang.sin() * r * 0.55, 0.05 * h, ang, mix(rgb, WHITE, 0.3), (0.2 + 0.5 * p) * strength);
    }
}

/// A spell of `element` breaks over the target. Every one starts with a ring that spreads on the ground and a bloom in its
/// colour, and goes on in shapes of its own.
fn blast(c: &mut Ctx, element: Element, a: f32) {
    c.s *= BLAST_SIZE;
    let (h, s, col) = (c.h, c.s, c.col);
    let spread = grow(a, 0.3, 1.15) * h * s;
    c.ring(0.0, 0.5 * h, spread, spread * 0.38, col, fade(a, 1.0) * 0.9);
    c.disc(0.0, 0.0, grow(a, 0.45, 0.95) * h * s, col, fade(a, 1.4));
    match element {
        Element::Fire => {
            c.disc(0.0, 0.0, grow(a, 0.3, 0.6) * h * s, mix(col, YELLOW, 0.45), fade(a, 2.0) * 0.9);
            c.disc(0.0, -0.05 * h, grow(a, 0.6, 0.9) * h * s, col, fade(a, 1.6) * 0.8);
            // Tongues of flame rise from the feet and gutter out.
            for k in 0..4u32 {
                let x = (k as f32 - 1.5) * 0.2 * h * s + (c.n(k) - 0.5) * 0.06 * h;
                let flick = 0.85 + 0.15 * (a * 24.0 + k as f32 * 2.1).sin();
                let tall = (0.55 + 0.35 * c.n(4 + k)) * h * s * flick * (1.0 - a * 0.45);
                // A bulb at the foot, a body that rises from it and a hot core.
                c.glow(x, 0.38 * h, 0.15 * h * s, 0.16 * h * s, col, fade(a, 1.1) * 0.7);
                c.glow(x, 0.45 * h - tall * 0.5, 0.09 * h * s, tall * 0.55, col, fade(a, 1.1) * 0.8);
                c.streak(x, 0.45 * h - tall * 0.5, 0.05 * h * s, tall * 0.5, 0.0, mix(col, YELLOW, 0.7), fade(a, 1.3) * 0.8);
            }
            // Embers rise.
            for k in 0..6u32 {
                let rise = (a * 1.2 + c.n(10 + k) * 0.2).min(1.2);
                let x = (c.n(20 + k) - 0.5) * 0.8 * h * s + (a * 9.0 + k as f32).sin() * 0.03 * h;
                c.disc(x, 0.2 * h - rise * 0.9 * h, 0.05 * h * (1.0 - a * 0.5), mix(col, YELLOW, 0.55), fade(a, 1.0));
            }
        }
        Element::Ice => {
            c.star(0.0, 0.0, grow(a, 0.4, 1.0) * h * s, a * 1.4, mix(col, WHITE, 0.5), fade(a, 1.2));
            c.star(0.0, 0.0, grow(a, 0.3, 0.75) * h * s, -a * 1.9 + 0.4, WHITE, fade(a, 1.5) * 0.8);
            // Shards of ice burst out, point first.
            for k in 0..6u32 {
                let ang = (k as f32 + c.n(k) * 0.4) / 6.0 * TAU;
                let (dx, dy) = (ang.cos(), ang.sin() * 0.85);
                let d = grow(a, 0.12, 0.8) * h * s;
                c.streak(dx * d, dy * d, 0.045 * h * s, 0.17 * h * s, spin_along(dx, dy), mix(col, WHITE, 0.55), fade(a, 1.3) * 0.95);
            }
            for k in 0..5u32 {
                let tw = 0.5 + 0.5 * (a * 22.0 + k as f32 * 2.0).sin();
                c.star((c.n(30 + k) - 0.5) * 1.1 * h, (c.n(40 + k) - 0.5) * 1.0 * h, 0.05 * h, a * 3.0, WHITE, fade(a, 1.0) * tw);
            }
        }
        Element::Bolt => {
            c.disc(0.0, 0.0, grow(a, 0.35, 0.7) * h * s, mix(col, WHITE, 0.5), fade(a, 2.5));
            // Three bolts strike down from above and flicker.
            for k in 0..3u32 {
                let x = (k as f32 - 1.0) * 0.3 * h * s + (c.n(k) - 0.5) * 0.08 * h;
                let on = if ((a * 18.0) as u32 + k).is_multiple_of(2) { 1.0 } else { 0.55 };
                c.zig(x, -0.5 * h, 0.32 * h * s, 0.98 * h * s, (c.n(8 + k) - 0.5) * 0.14, mix(col, WHITE, 0.55), fade(a, 0.8) * on);
            }
            c.sparks(5, 50, 0.8, 0.04, mix(col, WHITE, 0.4), a, 1.0);
        }
        Element::Earth => {
            c.disc(0.0, 0.0, grow(a, 0.4, 0.6) * h * s, mix(col, WHITE, 0.2), fade(a, 3.0) * 0.6);
            // Rocks burst up from the feet and fall back.
            for k in 0..7u32 {
                let vx = (c.n(k) - 0.5) * 1.5 * h * s;
                let vy = -(0.9 + 0.9 * c.n(10 + k)) * h * s;
                let (x, y) = (vx * a, 0.45 * h + vy * a + 1.7 * h * a * a);
                c.streak(x, y, 0.08 * h * s, 0.16 * h * s, spin_along(vx, vy + 3.4 * h * a) + a * 2.0, mix(col, YELLOW, 0.15), fade(a, 0.9) * 0.95);
            }
            for k in 0..4u32 {
                let x = (c.n(20 + k) - 0.5) * 1.1 * h;
                c.disc(x, 0.45 * h - grow(a, 0.0, 0.3) * h, 0.14 * h * (0.7 + a), mix(col, WHITE, 0.25), fade(a, 1.4) * 0.35);
            }
        }
        Element::Wind => {
            for k in 0..4u32 {
                let phi = k as f32 / 4.0 * TAU + a * 5.5;
                let (x, y) = (phi.cos() * 0.4 * h * s, phi.sin() * 0.2 * h * s + (k as f32 - 1.5) * 0.14 * h);
                c.blade(x, y, 0.3 * h * s, phi + PI * 0.5, mix(col, WHITE, 0.3), fade(a, 0.9) * (0.55 + 0.45 * phi.sin()));
            }
            for k in 0..6u32 {
                let phi = k as f32 / 6.0 * TAU + a * 7.0;
                let rise = (a * 0.9 + k as f32 * 0.08).min(1.0);
                c.disc(phi.cos() * 0.35 * h, 0.35 * h - rise * 0.85 * h, 0.035 * h, mix(col, WHITE, 0.5), fade(a, 1.0) * 0.9);
            }
        }
        Element::Water => {
            c.glow(0.0, -0.05 * h, 0.2 * h * s, 0.85 * h * s * grow(a, 0.5, 1.0), mix(col, WHITE, 0.3), fade(a, 1.8) * 0.5);
            let wave = grow((a * 1.4 - 0.2).clamp(0.0, 1.0), 0.3, 1.2) * h * s;
            c.ring(0.0, 0.5 * h, wave, wave * 0.38, mix(col, WHITE, 0.3), fade(a, 1.0) * 0.7);
            // Droplets leap and fall.
            for k in 0..9u32 {
                let vx = (c.n(k) - 0.5) * 1.3 * h * s;
                let vy = -(0.8 + 0.8 * c.n(10 + k)) * h * s;
                let (x, y) = (vx * a, 0.3 * h + vy * a + 1.6 * h * a * a);
                c.disc(x, y, (0.03 + 0.025 * c.n(20 + k)) * h, mix(col, WHITE, 0.45), fade(a, 0.8) * 0.95);
            }
        }
        Element::Poison => {
            c.disc(0.0, 0.1 * h, grow(a, 0.35, 0.8) * h * s, mix(col, [0.5, 0.9, 0.3], 0.25), fade(a, 1.2) * 0.55);
            // Bubbles rise and burst.
            for k in 0..7u32 {
                let rise = (a * 1.1 + c.n(k) * 0.25).min(1.2);
                let x = (c.n(10 + k) - 0.5) * 0.7 * h * s + (a * 8.0 + k as f32).sin() * 0.03 * h;
                let r = (0.06 + 0.07 * c.n(20 + k)) * h * s;
                let rgb = mix(col, [0.5, 0.9, 0.3], 0.35);
                c.ring(x, 0.4 * h - rise * 0.9 * h, r, r, rgb, fade(a, 0.9));
                c.disc(x, 0.4 * h - rise * 0.9 * h, r * 0.7, rgb, fade(a, 1.1) * 0.5);
            }
        }
        Element::Gravity => {
            // Rings close in on the target and crush it.
            for k in 0..3u32 {
                let t = ramp(a * 1.3 - k as f32 * 0.12, 0.0, 1.0);
                let r = lerp(1.0, 0.12, t) * h * s;
                c.ring(0.0, 0.0, r, r * 0.8, mix(col, WHITE, 0.1), fade(a, 1.1) * 0.6 * (1.0 - 0.5 * t));
            }
            c.disc(0.0, 0.0, (0.25 + 0.12 * (a * 18.0).sin().abs()) * h * s, mix(col, WHITE, 0.5), fade(a, 1.4));
            for k in 0..6u32 {
                let ang = (k as f32 + c.n(k)) / 6.0 * TAU;
                let r = lerp(0.9, 0.05, ramp(a, 0.0, 0.7)) * h;
                c.star(ang.cos() * r, ang.sin() * r * 0.8, 0.04 * h, ang, mix(col, WHITE, 0.4), fade(a, 1.0));
            }
        }
        Element::Holy => {
            let top = -0.45 * h;
            c.streak(0.0, top, 0.2 * h * s * (1.0 - a * 0.4), 1.5 * h * s, 0.0, mix(GOLD, WHITE, 0.35), fade(a, 1.1) * 0.7);
            c.streak(0.0, top, 0.07 * h * s, 1.5 * h * s, 0.0, WHITE, fade(a, 1.5) * 0.85);
            c.streak(0.0, 0.0, 0.025 * h * s, 0.95 * h * s * grow(a, 0.6, 1.0), spin_along(1.0, 0.0), mix(col, WHITE, 0.4), fade(a, 1.6));
            c.star(0.0, 0.0, grow(a, 0.3, 0.8) * h * s, a * 0.8, WHITE, fade(a, 1.4) * 0.9);
            for k in 0..6u32 {
                let rise = (a * 0.9 + c.n(k) * 0.2).min(1.0);
                c.star((c.n(10 + k) - 0.5) * 0.7 * h, 0.4 * h - rise * 1.0 * h, 0.06 * h, a * 2.0 + k as f32, mix(col, WHITE, 0.5), fade(a, 1.0) * 0.95);
            }
        }
        // The rest (Plain, and the restoring colour, which a heal draws as a heal): an arcane burst.
        Element::Plain | Element::Cure => {
            c.star(0.0, 0.0, grow(a, 0.3, 0.7) * h * s, a, mix(col, WHITE, 0.5), fade(a, 1.6) * 0.9);
            c.star(0.0, 0.0, grow(a, 0.2, 0.5) * h * s, -a * 1.4, WHITE, fade(a, 1.8) * 0.8);
            c.sparks(6, 60, 0.9, 0.04, mix(col, WHITE, 0.4), a, 1.1);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Restoring
// ---------------------------------------------------------------------------------------------

/// HP (or MP) comes back: a bloom, a ring that rises up the body, sparkles that float up and, for HP, a cross above the head.
fn mend(c: &mut Ctx, rgb: [f32; 3], a: f32, cross: bool) {
    let (h, s) = (c.h, c.s);
    c.disc(0.0, 0.0, grow(a, 0.35, 0.75) * h * s, rgb, fade(a, 1.5) * 0.7);
    // A ring sweeps from the feet to the head.
    let sweep = ramp(a, 0.0, 0.7);
    let y = lerp(0.5, -0.55, sweep) * h;
    c.ring(0.0, y, 0.46 * h * s, 0.16 * h * s, mix(rgb, WHITE, 0.25), (1.0 - sweep * 0.6) * fade(a, 1.2) * 0.9);
    let sparkles = if s > 1.2 { 9 } else { 6 };
    for k in 0..sparkles {
        let rise = (a * 0.9 + c.n(k) * 0.3).min(1.0);
        let x = (k as f32 - (sparkles - 1) as f32 * 0.5) * 0.9 / sparkles as f32 * h + (a * 7.0 + k as f32 * 1.7).sin() * 0.025 * h;
        c.star(x, 0.4 * h - rise * 1.0 * h, (0.06 + 0.05 * c.n(10 + k)) * h * s.min(1.3), a * 2.5 + k as f32, mix(rgb, WHITE, 0.3), fade(a, 1.0) * 0.95);
    }
    if cross {
        let (cy, size) = (-0.68 * h - a * 0.25 * h, 0.1 * h * s.min(1.3));
        let rgb = mix(rgb, WHITE, 0.55);
        c.streak(0.0, cy, size * 0.28, size, 0.0, rgb, fade(a, 1.2) * ramp(a, 0.0, 0.1));
        c.streak(0.0, cy, size * 0.28, size, spin_along(1.0, 0.0), rgb, fade(a, 1.2) * ramp(a, 0.0, 0.1));
    }
}

/// A fallen fighter comes back: a pillar of light comes down, rings spread, feathers drift.
fn raise(c: &mut Ctx, a: f32) {
    let (h, s) = (c.h, c.s);
    let light = mix(GOLD, WHITE, 0.45);
    let pillar = ramp(a, 0.0, 0.12);
    c.streak(0.0, -0.6 * h, 0.2 * h * s * (1.0 - a * 0.5), 1.7 * h * s, 0.0, light, fade(a, 0.9) * 0.75 * pillar);
    c.streak(0.0, -0.6 * h, 0.07 * h * s, 1.7 * h * s, 0.0, WHITE, fade(a, 1.3) * 0.9 * pillar);
    c.disc(0.0, 0.1 * h, grow(a, 0.4, 0.9) * h * s, GOLD, fade(a, 1.3) * 0.8);
    for k in 0..2u32 {
        let t = ramp(a * 1.4 - k as f32 * 0.2, 0.0, 1.0);
        c.ring(0.0, 0.5 * h, lerp(0.3, 1.2, t) * h * s, lerp(0.3, 1.2, t) * h * s * 0.38, light, (1.0 - t) * fade(a, 0.8) * 0.85);
    }
    // Feathers drift down, then sparkles go up.
    for k in 0..8u32 {
        let fall = (a * (0.8 + 0.4 * c.n(k)) + c.n(10 + k) * 0.2).min(1.0);
        let x = (c.n(20 + k) - 0.5) * 0.7 * h + (a * 6.0 + k as f32 * 2.0).sin() * 0.05 * h;
        c.star(x, -0.9 * h + fall * 1.3 * h, 0.05 * h, a * 2.0 + k as f32, light, fade(a, 0.9) * ramp(a, 0.0, 0.15) * 0.9);
    }
}

// ---------------------------------------------------------------------------------------------
// Statuses
// ---------------------------------------------------------------------------------------------

/// A bubble of `rgb` (a ring and a faint fill).
fn bubble(c: &mut Ctx, x: f32, y: f32, r: f32, rgb: [f32; 3], a: f32) {
    c.ring(x, y, r, r, rgb, a);
    c.disc(x, y, r * 0.75, rgb, a * 0.4);
}

/// A "Z" of half-size `size` made of a bar, a slash and a bar.
fn letter_z(c: &mut Ctx, x: f32, y: f32, size: f32, rgb: [f32; 3], a: f32) {
    let bar = size * 0.16;
    c.streak(x, y - size, bar, size, spin_along(1.0, 0.0), rgb, a);
    c.streak(x, y + size, bar, size, spin_along(1.0, 0.0), rgb, a);
    c.streak(x, y, bar, size * 1.42, spin_along(-1.0, 1.0), rgb, a);
}

/// A status that hurts or holds is inflicted: its sign over the fighter.
fn afflict(c: &mut Ctx, st: Status, a: f32) {
    let (h, s) = (c.h, c.s);
    let rgb = status_rgb(st);
    let pop = ramp(a, 0.0, 0.1);
    match st {
        Status::Sleep => {
            // Zs drift up and to the side, each bigger than the last.
            c.disc(0.0, -0.1 * h, grow(a, 0.3, 0.55) * h, rgb, fade(a, 1.6) * 0.4);
            for k in 0..3u32 {
                let t = ramp(a * 1.25 - k as f32 * 0.1, 0.0, 1.0);
                let size = (0.1 + 0.05 * k as f32) * h * s;
                letter_z(c, 0.12 * h + k as f32 * 0.12 * h + t * 0.05 * h, -0.45 * h - t * 0.5 * h, size, mix(rgb, WHITE, 0.3), (1.0 - t) * ramp(t, 0.0, 0.15) * 0.95);
            }
        }
        Status::Poison => {
            c.disc(0.0, 0.1 * h, grow(a, 0.35, 0.7) * h * s, rgb, fade(a, 1.3) * 0.5);
            for k in 0..6u32 {
                let rise = (a * 1.0 + c.n(k) * 0.25).min(1.2);
                let x = (c.n(10 + k) - 0.5) * 0.6 * h + (a * 7.0 + k as f32).sin() * 0.03 * h;
                bubble(c, x, 0.4 * h - rise * 0.85 * h, (0.06 + 0.06 * c.n(20 + k)) * h * s, mix(rgb, [0.5, 0.9, 0.3], 0.3), fade(a, 0.9));
            }
        }
        Status::Silence => {
            // A "no" sign over the head: a ring struck through.
            let y = -0.68 * h - a * 0.08 * h;
            c.ring(0.0, y, 0.2 * h * s, 0.2 * h * s, rgb, fade(a, 1.0) * pop);
            c.streak(0.0, y, 0.02 * h * s, 0.2 * h * s, spin_along(-1.0, 1.0), rgb, fade(a, 1.0) * pop);
            c.disc(0.0, y, 0.18 * h * s, rgb, fade(a, 1.4) * 0.3);
        }
        Status::Darkness => {
            for k in 0..5u32 {
                let phi = k as f32 / 5.0 * TAU + a * 4.0;
                let r = lerp(0.35, 0.12, ramp(a, 0.0, 0.8)) * h;
                c.disc(phi.cos() * r, -0.4 * h + phi.sin() * r * 0.45, 0.06 * h, rgb, fade(a, 1.0) * 0.7);
            }
            c.ring(0.0, -0.4 * h, lerp(0.45, 0.2, ramp(a, 0.0, 0.8)) * h, lerp(0.45, 0.2, ramp(a, 0.0, 0.8)) * h * 0.5, rgb, fade(a, 1.1) * 0.8);
        }
        Status::Slow => {
            // A clock face with a hand that crawls, and heavy drops that sink.
            c.ring(0.0, -0.1 * h, 0.3 * h * s, 0.3 * h * s, rgb, fade(a, 1.0) * pop);
            c.streak(0.0, -0.1 * h, 0.014 * h, 0.22 * h, a * 2.2, mix(rgb, WHITE, 0.4), fade(a, 1.0) * pop);
            for k in 0..3u32 {
                c.disc((k as f32 - 1.0) * 0.18 * h, -0.5 * h + (a * 0.9 + k as f32 * 0.1) * 0.9 * h, 0.05 * h, rgb, fade(a, 1.0) * 0.8);
            }
        }
        Status::Stop => {
            // A ring clamps shut and four locks flash at its corners.
            let r = lerp(0.9, 0.4, ramp(a, 0.0, 0.35)) * h * s;
            c.ring(0.0, 0.0, r, r * 1.1, rgb, fade(a, 1.0) * 0.9);
            c.disc(0.0, 0.0, 0.6 * h * s, rgb, fade(a, 2.0) * 0.45);
            for (dx, dy) in [(1.0f32, 1.0f32), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                c.star(dx * 0.32 * h, dy * 0.4 * h, 0.09 * h, a * 2.0, mix(rgb, WHITE, 0.5), fade(a, 1.2) * pop);
            }
        }
        Status::Paralysis => {
            // Current crawls over the body.
            c.disc(0.0, 0.0, 0.5 * h * s, rgb, fade(a, 1.6) * 0.45);
            for k in 0..4u32 {
                let flicker = if ((a * 20.0) as u32 + k).is_multiple_of(2) { 1.0 } else { 0.4 };
                let x = (k as f32 - 1.5) * 0.24 * h;
                c.zig(x, (c.n(k) - 0.5) * 0.3 * h, 0.12 * h, 0.4 * h, (c.n(5 + k) - 0.5) * 0.9, mix(rgb, WHITE, 0.5), fade(a, 0.9) * flicker * pop);
            }
        }
        Status::Sadness => {
            // Tears fall from the head.
            c.disc(0.0, -0.45 * h, 0.25 * h * s, rgb, fade(a, 1.6) * 0.35);
            for k in 0..5u32 {
                let t = ramp(a * 1.2 - k as f32 * 0.08, 0.0, 1.0);
                let x = (k as f32 - 2.0) * 0.12 * h;
                c.streak(x, -0.55 * h + t * 1.0 * h, 0.02 * h, 0.07 * h, 0.0, mix(rgb, WHITE, 0.3), (1.0 - t) * 0.9 * pop);
            }
        }
        Status::Fury => {
            // A red ring pulses, flares burst out and steam rises.
            let pulse = 0.5 + 0.5 * (a * 22.0).sin();
            c.ring(0.0, 0.0, lerp(0.5, 1.0, ramp(a, 0.0, 0.5)) * h * s, lerp(0.5, 1.0, ramp(a, 0.0, 0.5)) * h * s, rgb, fade(a, 1.0) * (0.55 + 0.4 * pulse));
            c.star(0.0, 0.0, grow(a, 0.35, 0.85) * h * s, a * 3.0, mix(rgb, YELLOW, 0.3), fade(a, 1.3));
            for k in 0..5u32 {
                let rise = (a * 1.3 + c.n(k) * 0.2).min(1.0);
                c.disc((c.n(10 + k) - 0.5) * 0.5 * h, 0.2 * h - rise * 0.9 * h, 0.05 * h, rgb, fade(a, 1.0));
            }
        }
        // A status the rules do not play (never inflicted): a plain sign.
        _ => {
            c.disc(0.0, 0.0, grow(a, 0.3, 0.7) * h * s, rgb, fade(a, 1.4) * 0.7);
            c.star(0.0, 0.0, grow(a, 0.3, 0.6) * h * s, a, mix(rgb, WHITE, 0.4), fade(a, 1.4));
        }
    }
}

/// A status that helps is inflicted: a shell, speed lines, a spiral of leaves.
fn boost(c: &mut Ctx, st: Status, a: f32) {
    let (h, s) = (c.h, c.s);
    let rgb = status_rgb(st);
    match st {
        Status::Haste => {
            // Streaks of speed rush across the body, and a ring pulses.
            for k in 0..5u32 {
                let t = (a * 1.8 + c.n(k) * 0.35) % 1.0;
                let y = (-0.38 + 0.19 * k as f32) * h;
                let x = lerp(-0.85, 0.85, t) * h;
                c.streak(x, y, 0.014 * h * s, 0.3 * h * s, spin_along(1.0, 0.0), mix(rgb, WHITE, 0.35), fade(a, 0.9) * ramp(t, 0.0, 0.2) * (1.0 - ramp(t, 0.8, 1.0)));
            }
            c.ring(0.0, 0.0, grow(a, 0.4, 1.0) * h * s, grow(a, 0.4, 1.0) * h * s, rgb, fade(a, 1.4) * 0.8);
            c.disc(0.0, 0.0, 0.45 * h * s, rgb, fade(a, 2.2) * 0.5);
        }
        Status::Regen => {
            // Leaves of light spiral up the body.
            c.ring(0.0, 0.5 * h, grow(a, 0.3, 0.9) * h * s, grow(a, 0.3, 0.9) * h * s * 0.38, rgb, fade(a, 1.2) * 0.75);
            for k in 0..7u32 {
                let t = (a * 1.0 + k as f32 / 7.0 * 0.6).min(1.0);
                let phi = k as f32 * 1.1 + a * 5.0;
                c.star(phi.cos() * 0.32 * h, 0.45 * h - t * 1.0 * h, 0.06 * h, phi, mix(rgb, WHITE, 0.4), fade(a, 1.0) * (0.55 + 0.45 * phi.sin()));
            }
            c.disc(0.0, 0.0, 0.5 * h * s, rgb, fade(a, 1.8) * 0.4);
        }
        Status::Barrier | Status::MBarrier => {
            // A shell of light forms around the body, shivers and fades.
            let form = ramp(a, 0.0, 0.2);
            let (rx, ry) = (lerp(0.45, 0.62, form) * h * s, lerp(0.7, 0.95, form) * h * s);
            let alpha = fade(a, 1.3);
            c.glow(0.0, 0.0, rx * 0.97, ry * 0.97, rgb, 0.22 * alpha);
            c.ring(0.0, 0.0, rx, ry, rgb, 0.95 * alpha);
            if st == Status::MBarrier {
                let (ix, iy) = (rx * (0.8 + 0.06 * (a * 14.0).sin()), ry * (0.8 + 0.06 * (a * 14.0).sin()));
                c.ring(0.0, 0.0, ix, iy, mix(rgb, WHITE, 0.4), 0.6 * alpha);
            }
            for k in 0..6u32 {
                let phi = k as f32 / 6.0 * TAU + a * if st == Status::MBarrier { 5.0 } else { 1.5 };
                c.star(phi.cos() * rx * 0.92, phi.sin() * ry * 0.92, 0.05 * h, a * 3.0 + k as f32, mix(rgb, WHITE, 0.55), alpha * (0.45 + 0.55 * (a * 16.0 + k as f32 * 1.3).sin().abs()));
            }
        }
        _ => {
            c.ring(0.0, 0.0, grow(a, 0.4, 1.0) * h * s, grow(a, 0.4, 1.0) * h * s, rgb, fade(a, 1.3) * 0.8);
            c.disc(0.0, 0.0, 0.5 * h * s, rgb, fade(a, 1.8) * 0.5);
            c.star(0.0, 0.0, grow(a, 0.3, 0.7) * h * s, a * 2.0, mix(rgb, WHITE, 0.5), fade(a, 1.4));
        }
    }
}

/// A status is cured or runs out: a ring wipes the fighter from head to feet and the ailment's colours drift away.
fn cleanse(c: &mut Ctx, st: Status, a: f32) {
    let (h, s) = (c.h, c.s);
    let rgb = status_rgb(st);
    let sweep = ramp(a, 0.0, 0.6);
    c.ring(0.0, lerp(-0.55, 0.5, sweep) * h, 0.45 * h * s, 0.16 * h * s, mix(WHITE, rgb, 0.3), (1.0 - sweep * 0.5) * fade(a, 1.1) * 0.9);
    c.disc(0.0, 0.0, grow(a, 0.35, 0.7) * h * s, mix(rgb, WHITE, 0.4), fade(a, 1.6) * 0.7);
    for k in 0..8u32 {
        let th = c.n(k) * TAU;
        let d = grow(a, 0.05, 0.5 + 0.4 * c.n(10 + k)) * h * s;
        c.disc(th.cos() * d, th.sin() * d * 0.8 - a * 0.35 * h, 0.075 * h * (1.0 - 0.5 * a), rgb, fade(a, 1.0) * 0.9);
    }
    for k in 0..4u32 {
        let tw = 0.5 + 0.5 * (a * 20.0 + k as f32 * 1.7).sin();
        c.star((c.n(30 + k) - 0.5) * 0.9 * h, (c.n(40 + k) - 0.5) * 1.0 * h, 0.05 * h, a * 2.0, WHITE, fade(a, 1.0) * tw);
    }
}

// ---------------------------------------------------------------------------------------------
// Staging: what the performer does and what flies
// ---------------------------------------------------------------------------------------------

/// A caster gathers its spell (or a performer its limit break) up to the impact: a glow that grows, a ring that closes in,
/// a spinning star and motes that fall in; after the impact the glow is let go.
fn charge(c: &mut Ctx, phase: Phase) {
    let (h, s, col) = (c.h, c.s, c.col);
    let big = s > 1.2;
    match phase {
        Phase::Lead(p) => {
            c.disc(0.0, 0.0, (0.12 + 0.3 * p) * h * s.min(1.6), mix(col, WHITE, 0.25), 0.3 + 0.6 * p);
            c.ring(0.0, 0.3 * h, (1.0 - 0.7 * p) * 0.9 * h * s.min(1.6), (1.0 - 0.7 * p) * 0.9 * h * s.min(1.6), col, 0.2 + 0.6 * p);
            c.star(0.0, 0.0, (0.1 + 0.2 * p) * h * s.min(1.6), p * TAU, mix(col, WHITE, 0.6), 0.4 + 0.5 * p);
            let motes = if big { 9 } else { 5 };
            for k in 0..motes {
                let ang = (k as f32 / motes as f32 + c.n(k) * 0.1) * TAU + p * 2.0;
                let r = lerp(1.0, 0.1, ramp(p, 0.0, 1.0)) * h;
                c.star(ang.cos() * r * 0.8, ang.sin() * r * 0.6, 0.04 * h, ang, mix(col, WHITE, 0.4), 0.3 + 0.55 * p);
            }
            if big {
                c.ring(0.0, 0.0, (1.3 - 0.9 * p) * h, (1.3 - 0.9 * p) * h * 0.8, mix(col, WHITE, 0.3), 0.15 + 0.5 * p);
            }
        }
        Phase::Age(a) => {
            c.disc(0.0, 0.0, (0.42 + 0.35 * a) * h * s.min(1.6), mix(col, WHITE, 0.25), fade(a, 1.5) * 0.8);
        }
    }
}

/// A shot flies from the shooter to the target (or past it) as a bolt with a soft glow, leaving a line that fades.
fn bolt(c: &mut Ctx, phase: Phase, from: Option<[f32; 3]>, hit: bool) {
    let Some(from) = from else { return };
    let h = c.h;
    let side = if c.n(0) < 0.5 { -1.0 } else { 1.0 };
    let to = if hit { [0.0, 0.0, c.lift] } else { [side * 0.55 * h, -0.05 * h, c.lift] };
    let from = [from[0], from[1], from[2] + c.lift];
    let rgb = mix(c.col, WHITE, 0.5);
    match phase {
        Phase::Lead(p) => {
            let head = lerp3(from, to, p);
            let tail = lerp3(from, to, (p - 0.5).max(0.0));
            c.out.push(Layer::between(Prim::Beam, tail, head, 0.11 * h, c.col, 0.4));
            c.out.push(Layer::between(Prim::Beam, tail, head, 0.045 * h, rgb, 1.0));
            c.out.push(Layer::round(Prim::Disc, head, 0.1 * h, 0.0, rgb, 0.9));
            // The flash at the muzzle as it leaves.
            let flash = 1.0 - ramp(p, 0.0, 0.5);
            c.out.push(Layer::round(Prim::Star, from, 0.2 * h * flash.max(0.2), p * 3.0, rgb, flash));
            c.out.push(Layer::round(Prim::Disc, from, 0.14 * h * flash.max(0.2), 0.0, c.col, flash * 0.8));
        }
        Phase::Age(a) => {
            c.out.push(Layer::between(Prim::Beam, from, to, 0.03 * h * (1.0 - a), rgb, fade(a, 1.5) * 0.7));
            c.out.push(Layer::between(Prim::Beam, from, to, 0.08 * h * (1.0 - a * 0.5), c.col, fade(a, 2.0) * 0.25));
        }
    }
}

/// An item flies from its user to the target on an arc, with a trail (a user that is its own target tosses it up).
fn toss(c: &mut Ctx, p: f32, from: Option<[f32; 3]>) {
    let h = c.h;
    let from = from.unwrap_or([0.0; 3]);
    let to = [0.0, 0.0, 0.0];
    let reach = from[0].hypot(from[1]);
    let arc = 0.3 * reach + 0.35 * h;
    let lift = c.lift;
    let at = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        let p = lerp3(from, to, t);
        [p[0], p[1] - (PI * t).sin() * arc, p[2] + lift]
    };
    let rgb = [1.0, 0.97, 0.8];
    for k in 1..5u32 {
        let t = p - k as f32 * 0.06;
        if t >= 0.0 {
            let q = at(t);
            c.out.push(Layer::round(Prim::Disc, q, 0.1 * h * (1.0 - k as f32 * 0.15), 0.0, rgb, 0.6 * (1.0 - k as f32 * 0.2)));
        }
    }
    let q = at(p);
    c.out.push(Layer::round(Prim::Star, q, 0.2 * h, p * TAU * 2.0, rgb, 1.0));
    c.out.push(Layer::round(Prim::Disc, q, 0.22 * h, 0.0, rgb, 0.6));
}

/// A guard shimmers: a shell of pale light swells around the guard and sparkles at its rim.
fn guard(c: &mut Ctx, a: f32) {
    let h = c.h;
    let rgb = [0.7, 0.86, 1.0];
    let (rx, ry) = (grow(a, 0.4, 0.62) * h, grow(a, 0.65, 0.95) * h);
    let alpha = fade(a, 1.2);
    c.glow(0.0, 0.0, rx * 0.97, ry * 0.97, rgb, 0.2 * alpha);
    c.ring(0.0, 0.0, rx, ry, rgb, 0.9 * alpha);
    for k in 0..4u32 {
        let phi = k as f32 / 4.0 * TAU + 0.6 + a * 2.0;
        c.star(phi.cos() * rx * 0.92, phi.sin() * ry * 0.92, 0.05 * h, a * 3.0, WHITE, alpha * 0.9);
    }
}

/// A spell or an item that found nobody to land on fizzles: a puff, a few motes, a faint ring.
fn fizzle(c: &mut Ctx, a: f32) {
    let (h, col) = (c.h, c.col);
    c.disc(0.0, 0.0, (0.3 - 0.12 * a) * h, mix(col, WHITE, 0.2), fade(a, 1.5) * 0.55);
    c.ring(0.0, 0.0, grow(a, 0.25, 0.55) * h, grow(a, 0.25, 0.55) * h, col, fade(a, 1.4) * 0.4);
    for k in 0..4u32 {
        c.star((c.n(k) - 0.5) * 0.4 * h, -a * (0.3 + 0.3 * c.n(5 + k)) * h, 0.04 * h, a * 3.0, mix(col, WHITE, 0.3), fade(a, 1.0) * 0.8);
    }
}

/// A limit break breaks: a big bloom over its target and a wash over the whole picture.
fn flash(c: &mut Ctx, a: f32) {
    let (h, s, col) = (c.h, c.s, c.col);
    let tint = mix(mix(col, GOLD, 0.35), WHITE, 0.5);
    c.out.push(Layer::screen(tint, 0.4 * fade(a, 2.4)));
    c.disc(0.0, 0.0, grow(a, 1.0, 2.4) * h * s * 0.8, tint, fade(a, 2.0) * 0.9);
    c.star(0.0, 0.0, grow(a, 0.8, 1.9) * h * s * 0.8, a * 0.7, WHITE, fade(a, 2.2) * 0.9);
}
