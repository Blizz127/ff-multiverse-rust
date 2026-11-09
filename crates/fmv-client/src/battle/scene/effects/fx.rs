//! One effect on the cue timeline: what it is ([`Kind`]), between which actors, when it begins, when it lands and how long it
//! lives after; the table that says which effect an event makes ([`show`]); and the pure function that turns an effect and
//! the clock into pieces to draw ([`layers`], the shapes are in `looks`).
//!
//! # The timeline
//!
//! ```text
//!  start ---- Phase::Lead(0..1) ----> land ---- Phase::Age(0..1) ----> land + life
//!  the performance begins              the impact: the numbers are born         gone
//!  (a glow gathers, a bolt leaves,     (the peak)                                (nothing is drawn, the scene forgets it)
//!   an item is in the air)
//! ```
//!
//! `start` is when the effect first draws: the start of its performance for what the performer does (a counter's effects begin
//! when the counter does, [`super::super::cue::Cue::start`], never with the batch), the instant of the landing for what only
//! happens at the target (a blow's flash has no lead). `land` is the performance's impact, the very instant the overlay's
//! number for it is born, so the picture and the number agree. After it the effect fades for [`Fx::life`].

use super::{looks, Element, Layer, Look, Strike, Style};
use fmv_ff7_battle::rules::BattleEvent;
use fmv_ff7_battle::status::Status;

/// The most pieces one effect makes at any instant.
pub const MAX_LAYERS_PER_FX: usize = 28;

/// What an effect is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A blow lands: slashes, a round impact or a burst of rays; a critical hit is bigger and gold.
    Strike { how: Strike, critical: bool },
    /// A shot lands: a small flash and sparks.
    Impact { critical: bool },
    /// A blow found nobody there: a whiff beside the target.
    Whiff,
    /// A spell breaks over the target in the colours and shapes of its element ([`Fx::element`]).
    Blast,
    /// HP comes back: green sparkles and a ring that rises up the body.
    Mend,
    /// A fallen fighter is brought back: a pillar of light and feathers.
    Raise,
    /// MP comes back: blue sparkles.
    Mana,
    /// A status that hurts or holds is inflicted: its sign (Zs for Sleep, bubbles for Poison, ...).
    Afflict(Status),
    /// A status that helps is inflicted: a shell for Barrier, speed lines for Haste, ...
    Boost(Status),
    /// A status is cured or runs out: a wipe and the ailment's colours leaving.
    Cleanse(Status),
    /// A caster gathers its spell, or a performer its limit break: a glow that grows and motes that close in.
    Charge,
    /// A shot flies from `from` to the target (`hit: false`: past it), leaving a line that fades.
    Bolt { hit: bool },
    /// An item flies from `from` to the target on an arc.
    Toss,
    /// A guard shimmers around the guard.
    Guard,
    /// A spell or an item had nobody to land on: it fizzles at its performer.
    Fizzle,
    /// A limit break breaks: a bloom over the target and a wash over the whole picture.
    Flash,
}

/// One effect of the scene. `target` is the actor it plays at (the performer for a [`Kind::Charge`], a [`Kind::Guard`], a
/// [`Kind::Fizzle`]); `from` the actor a flying effect comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fx {
    pub kind: Kind,
    pub element: Element,
    pub target: usize,
    pub from: Option<usize>,
    /// When it first draws, seconds on the scene clock.
    pub start: f64,
    /// The instant of its impact (the peak).
    pub land: f64,
    /// How strong it is: 1 is usual, more for a limit break and a critical hit, less for an item and a status running out.
    pub scale: f32,
    /// What makes its particles its own ([`super::noise`]).
    pub seed: u32,
}

/// Where an effect is in its life (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Before the impact: how far to it, `0..1`.
    Lead(f32),
    /// After it: how far through the fade, `0..1`.
    Age(f32),
}

impl Fx {
    /// How long the effect lives after its landing, in seconds.
    pub fn life(&self) -> f64 {
        match self.kind {
            Kind::Strike { critical: false, .. } => 0.45,
            Kind::Strike { critical: true, .. } => 0.62,
            Kind::Impact { .. } => 0.32,
            Kind::Whiff => 0.38,
            Kind::Blast => 0.95,
            Kind::Mend => 1.0,
            Kind::Raise => 1.3,
            Kind::Mana => 0.95,
            Kind::Afflict(_) | Kind::Boost(_) => 1.1,
            Kind::Cleanse(_) => 0.85,
            Kind::Charge => 0.24,
            Kind::Bolt { .. } => 0.34,
            Kind::Toss => 0.06,
            Kind::Guard => 0.6,
            Kind::Fizzle => 0.55,
            Kind::Flash => 0.34,
        }
    }

    /// When the effect is over.
    pub fn end(&self) -> f64 {
        self.land + self.life()
    }

    /// Where the effect is at `now`, `None` before it starts, after it ends and for a clock that is not a time.
    pub fn phase(&self, now: f64) -> Option<Phase> {
        if !now.is_finite() || !self.start.is_finite() || !self.land.is_finite() || now < self.start || now >= self.end() {
            return None;
        }
        if now < self.land {
            let span = (self.land - self.start).max(1e-6);
            Some(Phase::Lead(((now - self.start) / span).clamp(0.0, 1.0) as f32))
        } else {
            Some(Phase::Age(((now - self.land) / self.life()).clamp(0.0, 1.0) as f32))
        }
    }
}

/// What an effect needs to know of the places it is drawn at, from the scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geo {
    /// How tall the anchor fighter is, in battle units.
    pub height: f32,
    /// Where the fighter a flying effect comes from has its chest, relative to the anchor's, along the camera's axes (right,
    /// down, towards the camera); `None` for an effect that does not fly.
    pub from: Option<[f32; 3]>,
}

/// An effect a landing event makes at its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Show {
    pub kind: Kind,
    pub element: Element,
    pub scale: f32,
}

/// The strength of an effect of an item (a lesser one than a spell's) and of a limit break (a greater one).
const ITEM_SCALE: f32 = 0.8;
const LIMIT_SCALE: f32 = 1.5;
/// A status that runs out on the clock ends quietly.
const EXPIRY_SCALE: f32 = 0.7;

/// The effect `event` makes at its target, staged as `look` says; `timed`: the event belongs to no action (a status that ran
/// out, or one the battle began with). `None` for an event that lands on nobody (gauges, a death, the end).
pub fn show(event: &BattleEvent, look: &Look, timed: bool) -> Option<Show> {
    let big = matches!(look.style, Style::Limit { .. });
    let weight = match look.style {
        Style::Item => ITEM_SCALE,
        Style::Limit { .. } => LIMIT_SCALE,
        _ => 1.0,
    };
    let plain = |kind, scale| Some(Show { kind, element: look.element, scale });
    match *event {
        BattleEvent::Damage { critical, .. } => match look.style {
            Style::Blow(how) => plain(Kind::Strike { how, critical }, 1.0),
            Style::Shot => plain(Kind::Impact { critical }, 1.0),
            Style::Spell => plain(Kind::Blast, 1.0),
            Style::Item => plain(Kind::Blast, ITEM_SCALE),
            // A physical limit strikes like a blow, as hard as a critical hit; a magical one breaks like a spell.
            Style::Limit { physical: true } => plain(Kind::Strike { how: Strike::Slash, critical: true }, LIMIT_SCALE),
            Style::Limit { physical: false } => plain(Kind::Blast, LIMIT_SCALE),
        },
        BattleEvent::Miss { .. } => plain(Kind::Whiff, 1.0),
        BattleEvent::Heal { .. } => Some(Show { kind: Kind::Mend, element: Element::Cure, scale: weight }),
        BattleEvent::Revive { .. } => Some(Show { kind: Kind::Raise, element: Element::Holy, scale: weight }),
        BattleEvent::MpRestored { .. } => Some(Show { kind: Kind::Mana, element: Element::Water, scale: weight }),
        BattleEvent::StatusInflicted { status, .. } => {
            let kind = if status.is_beneficial() { Kind::Boost(status) } else { Kind::Afflict(status) };
            Some(Show { kind, element: Element::Plain, scale: if big { LIMIT_SCALE } else { 1.0 } })
        }
        BattleEvent::StatusEnded { status, .. } => Some(Show { kind: Kind::Cleanse(status), element: Element::Plain, scale: if timed { EXPIRY_SCALE } else { weight } }),
        // A turn of Poison or Regen, a gauge, a death, an announcement, the end: nothing lands at a target.
        BattleEvent::StatusTick { .. }
        | BattleEvent::Death { .. }
        | BattleEvent::GaugeReady { .. }
        | BattleEvent::Victory
        | BattleEvent::Defeat
        | BattleEvent::Action { .. }
        | BattleEvent::LimitGauge { .. }
        | BattleEvent::EscapeFailed { .. }
        | BattleEvent::MpSpent { .. } => None,
    }
}

/// The pieces of `fx` at `now` appended to `out`: nothing before it starts or after it ends, at most
/// [`MAX_LAYERS_PER_FX`], every one of them sound ([`Layer::is_sound`]) and visible. Pure: the same effect at the same instant
/// is the same pieces.
pub fn layers(fx: &Fx, now: f64, geo: &Geo, out: &mut Vec<Layer>) {
    let Some(phase) = fx.phase(now) else { return };
    let base = out.len();
    looks::draw(fx, phase, geo, out);
    out.truncate(base + MAX_LAYERS_PER_FX);
    // Whatever the curves do, a piece is a fraction of a glow with numbers that are numbers, and one nobody could see is
    // not drawn at all.
    let mut keep = base;
    for i in base..out.len() {
        let mut layer = out[i];
        layer.alpha = layer.alpha.clamp(0.0, 1.0);
        layer.rgb = layer.rgb.map(|c| c.clamp(0.0, 1.0));
        if layer.alpha > 0.004 && layer.is_sound() {
            out[keep] = layer;
            keep += 1;
        }
    }
    out.truncate(keep);
}
