//! Simple procedural effects: the glow a caster gathers, a spell's burst coloured by its element,
//! an item's sparkle, the sparks of a blow.
//!
//! Nothing here is read from the game: the effects are made of four tiny meshes ([`Prim`]: a soft
//! disc, a star, a ring, a streak) that the avatar renderer draws ADDITIVELY like any other model
//! (they are [`PartBlend::Additive`] parts of a one-bone model), facing the camera, each with a
//! per-draw tint ([`crate::avatar::gpu::Tint`]) that colours it and fades it. So they cost the
//! renderer nothing new, and the four meshes are made once per [`EffectKit`] and shared by every
//! effect of every battle (the GPU keeps a bounded number of models: an asset per effect would
//! evict the actors every frame, which a test of the scene holds against).
//!
//! The shapes are meant to read at a glance, not to be the game's: fire is an orange bloom with embers
//! rising, ice a pale blue flash with a spinning star of shards, bolt a yellow flash with streaks falling,
//! cure green sparkles rising, anything else a soft white-violet bloom; a blow throws a white star
//! (gold for a critical hit). [`burst_layers`] and [`charge_layers`] are pure functions of the age of
//! the effect, so every curve is unit-tested without a window.

use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use fmv_ff7::model::ModelBone;
use fmv_ff7::playable::Character;
use fmv_ff7::{FieldModel, MeshPart, PartBlend};
use fmv_ff7_battle::rules::element;
use std::f32::consts::TAU;
use std::sync::Arc;

/// How long a burst lasts, in seconds.
pub const BURST_LIFE: f64 = 0.7;

/// Which colour an effect is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    Plain,
    Fire,
    Ice,
    Bolt,
    Cure,
    Earth,
    Wind,
    Water,
    Holy,
    Poison,
    Gravity,
}

impl Element {
    /// The element of an attack's element mask ([`element`]); `heals` (a restoring action) is Cure
    /// whatever else the mask says. The first of Fire, Ice, Bolt, Water, Wind, Earth, Poison, Gravity,
    /// Holy that the mask has; none of them is [`Element::Plain`].
    pub fn from_mask(mask: u16, heals: bool) -> Self {
        if heals || mask & element::RESTORATIVE != 0 {
            return Self::Cure;
        }
        [
            (element::FIRE, Self::Fire),
            (element::ICE, Self::Ice),
            (element::BOLT, Self::Bolt),
            (element::WATER, Self::Water),
            (element::WIND, Self::Wind),
            (element::EARTH, Self::Earth),
            (element::POISON, Self::Poison),
            (element::GRAVITY, Self::Gravity),
            (element::HOLY, Self::Holy),
        ]
        .into_iter()
        .find(|&(bit, _)| mask & bit != 0)
        .map_or(Self::Plain, |(_, e)| e)
    }

    /// The colour of its glow, each channel in `0..=1`.
    pub fn rgb(self) -> [f32; 3] {
        match self {
            Self::Plain => [0.92, 0.9, 1.0],
            Self::Fire => [1.0, 0.45, 0.12],
            Self::Ice => [0.55, 0.85, 1.0],
            Self::Bolt => [1.0, 0.95, 0.4],
            Self::Cure => [0.45, 1.0, 0.6],
            Self::Earth => [0.85, 0.6, 0.3],
            Self::Wind => [0.7, 1.0, 0.8],
            Self::Water => [0.3, 0.55, 1.0],
            Self::Holy => [1.0, 1.0, 0.8],
            Self::Poison => [0.7, 0.4, 0.9],
            Self::Gravity => [0.6, 0.35, 1.0],
        }
    }
}

/// What an action looks like, as the engine says it: whether its performer runs up to its target and
/// the colour of its effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub melee: bool,
    pub element: Element,
}

impl Look {
    /// A plain physical blow: it runs up, its sparks are white.
    pub const PLAIN: Look = Look { melee: true, element: Element::Plain };
}

/// What kind of burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstKind {
    /// A spell breaking over its target.
    Spell,
    /// An item taking effect.
    Item,
    /// The sparks of a blow.
    Hit { critical: bool },
}

/// An effect that flashes at actor `target` from `at` for [`BURST_LIFE`] seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burst {
    pub at: f64,
    pub target: usize,
    pub kind: BurstKind,
    pub element: Element,
}

/// A spell that caster `caster` gathers from `start` to `end` (the impact).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Charge {
    pub caster: usize,
    pub start: f64,
    pub end: f64,
    pub element: Element,
}

/// The four meshes every effect is made of, in a unit square facing the camera (`x` right, `y` down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prim {
    /// A round glow, white at the middle and black (nothing, added) at the rim, radius 1.
    Disc,
    /// A star of eight spikes, radius 1.
    Star,
    /// A thin soft ring, outer radius 1.
    Ring,
    /// A tall thin streak from `y = -1` to `y = 1`.
    Streak,
}

/// One drawn piece of an effect at one instant: a primitive, where (offsets from the effect's centre
/// in the camera's frame: right, down, towards the camera, in battle units), how big, how it is turned
/// about the view axis, in which colour and how strongly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    pub prim: Prim,
    pub offset: [f32; 3],
    pub radius: f32,
    pub spin: f32,
    pub rgb: [f32; 3],
    /// The intensity, `0..=1` (it multiplies the glow).
    pub alpha: f32,
}

fn mixed(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const GOLD: [f32; 3] = [1.0, 0.82, 0.3];

fn fade(age: f32, power: f32) -> f32 {
    (1.0 - age.clamp(0.0, 1.0)).powf(power)
}

/// Out-and-slow growth from `a` to `b`.
fn grow(age: f32, a: f32, b: f32) -> f32 {
    let x = age.clamp(0.0, 1.0);
    a + (b - a) * (1.0 - (1.0 - x) * (1.0 - x))
}

/// The pieces of a burst at `age` (`0..=1` of [`BURST_LIFE`]), its target `height` tall: nothing once
/// the age is 1 or more. Offsets are in units of the target's height where they scale (so a boss gets
/// a bigger effect than a hound).
pub fn burst_layers(kind: BurstKind, element: Element, age: f32, height: f32) -> Vec<Layer> {
    if !(0.0..1.0).contains(&age) {
        return Vec::new();
    }
    let h = height.clamp(300.0, 3000.0);
    let col = element.rgb();
    let mut out = Vec::new();
    let mut push = |prim, offset: [f32; 3], radius: f32, spin: f32, rgb: [f32; 3], alpha: f32| {
        out.push(Layer { prim, offset, radius, spin, rgb, alpha: alpha.clamp(0.0, 1.0) });
    };
    match kind {
        BurstKind::Hit { critical } => {
            let tint = if critical { GOLD } else { WHITE };
            let (small, big) = if critical { (0.55, 1.05) } else { (0.38, 0.72) };
            push(Prim::Star, [0.0; 3], grow(age, small, big) * h, age * 0.6, tint, fade(age, 1.6));
            push(Prim::Disc, [0.0; 3], grow(age, 0.25, 0.5) * h, 0.0, mixed(tint, col, 0.3), fade(age, 3.0));
        }
        BurstKind::Item => {
            push(Prim::Disc, [0.0; 3], grow(age, 0.35, 0.8) * h, 0.0, mixed(col, WHITE, 0.4), fade(age, 1.5) * 0.9);
            for k in 0..3 {
                let phase = k as f32 / 3.0;
                let rise = (age + phase * 0.35).min(1.0);
                push(Prim::Star, [(k as f32 - 1.0) * 0.18 * h, -rise * 0.7 * h, 0.0], 0.14 * h, age * 2.0 + k as f32, mixed(col, WHITE, 0.6), fade(age, 1.2) * 0.9);
            }
        }
        BurstKind::Spell => {
            // The shared start: a ring that spreads and a bloom in the element's colour.
            push(Prim::Ring, [0.0, 0.25 * h, 0.0], grow(age, 0.3, 1.1) * h, 0.0, col, fade(age, 1.0) * 0.9);
            push(Prim::Disc, [0.0; 3], grow(age, 0.45, 0.95) * h, 0.0, col, fade(age, 1.4));
            match element {
                Element::Fire => {
                    for k in 0..4 {
                        let phase = k as f32 * 0.17;
                        let rise = (age * 1.2 + phase).min(1.2);
                        push(Prim::Disc, [(k as f32 - 1.5) * 0.2 * h, 0.15 * h - rise * 0.9 * h, 0.0], 0.16 * h * (1.0 - age * 0.5), 0.0, mixed(col, [1.0, 0.9, 0.4], 0.5), fade(age, 1.0));
                    }
                }
                Element::Ice => {
                    push(Prim::Star, [0.0; 3], grow(age, 0.4, 1.0) * h, age * 1.4, mixed(col, WHITE, 0.5), fade(age, 1.2));
                    push(Prim::Star, [0.0; 3], grow(age, 0.3, 0.75) * h, -age * 1.9 + 0.4, WHITE, fade(age, 1.5) * 0.8);
                }
                Element::Bolt => {
                    for k in 0..3 {
                        let x = (k as f32 - 1.0) * 0.28 * h;
                        let flicker = if ((age * 14.0) as u32 + k).is_multiple_of(2) { 1.0 } else { 0.55 };
                        push(Prim::Streak, [x, -0.5 * h, 0.0], 0.95 * h, (k as f32 - 1.0) * 0.12, mixed(col, WHITE, 0.5), fade(age, 0.8) * flicker);
                    }
                }
                Element::Cure => {
                    for k in 0..5 {
                        let phase = k as f32 / 5.0;
                        let rise = (age * 0.9 + phase * 0.3).min(1.0);
                        push(Prim::Star, [(k as f32 - 2.0) * 0.17 * h, 0.35 * h - rise * 1.0 * h, 0.0], 0.15 * h, age * 2.5 + k as f32, mixed(col, WHITE, 0.5), fade(age, 1.0) * 0.95);
                    }
                }
                _ => {
                    push(Prim::Star, [0.0; 3], grow(age, 0.3, 0.7) * h, age, mixed(col, WHITE, 0.5), fade(age, 1.6) * 0.8);
                }
            }
        }
    }
    out
}

/// The glow a caster gathers at `progress` (`0..=1` from the start of the cast to the impact), the
/// caster `height` tall: a bloom that grows at its hands and a ring that closes in on it.
pub fn charge_layers(element: Element, progress: f32, height: f32) -> Vec<Layer> {
    if !(0.0..=1.0).contains(&progress) {
        return Vec::new();
    }
    let h = height.clamp(300.0, 3000.0);
    let col = element.rgb();
    vec![
        Layer { prim: Prim::Disc, offset: [0.0, 0.0, 0.0], radius: (0.12 + 0.3 * progress) * h, spin: 0.0, rgb: mixed(col, WHITE, 0.25), alpha: 0.3 + 0.6 * progress },
        Layer { prim: Prim::Ring, offset: [0.0, 0.3 * h, 0.0], radius: (1.0 - 0.7 * progress) * 0.9 * h, spin: 0.0, rgb: col, alpha: 0.2 + 0.6 * progress },
        Layer { prim: Prim::Star, offset: [0.0; 3], radius: (0.1 + 0.2 * progress) * h, spin: progress * TAU, rgb: mixed(col, WHITE, 0.6), alpha: 0.4 + 0.5 * progress },
    ]
}

/// The tint a layer is drawn with: its colour (white where the mesh already carries the glow) scaled
/// by its intensity, and its opacity for the fade.
pub fn layer_tint(layer: &Layer) -> crate::avatar::gpu::Tint {
    crate::avatar::gpu::Tint { rgb: layer.rgb, alpha: layer.alpha }
}

// ---------------------------------------------------------------------------------------------
// The meshes
// ---------------------------------------------------------------------------------------------

/// A vertex of a primitive: where, and how bright (the colour is a grey: the tint colours it).
type Vert = ([f32; 2], u8);

fn mesh_of(verts: &[Vert], triangles: &[[u32; 3]]) -> MeshPart {
    let n = verts.len();
    MeshPart {
        bone_index: 0,
        positions: verts.iter().map(|(p, _)| [p[0], p[1], 0.0]).collect(),
        normals: vec![[0.0, 0.0, -1.0]; n],
        uvs: vec![[0.0; 2]; n],
        colors: verts.iter().map(|&(_, g)| [g, g, g, 255]).collect(),
        indices: triangles.iter().flatten().copied().collect(),
        texture: None,
        texture_name: None,
        // Added to the picture: black adds nothing, so a grey that falls to black is a soft glow.
        blend: PartBlend::Additive,
        alpha: 1.0,
    }
}

fn polar(r: f32, a: f32) -> [f32; 2] {
    [r * a.cos(), r * a.sin()]
}

/// The mesh of `prim`.
fn prim_part(prim: Prim) -> MeshPart {
    match prim {
        Prim::Disc => {
            const N: u32 = 20;
            // Centre, a ring at 0.4 of the radius (a bright shoulder), the rim.
            let mut verts: Vec<Vert> = vec![([0.0, 0.0], 255)];
            verts.extend((0..N).map(|k| (polar(0.4, k as f32 / N as f32 * TAU), 150)));
            verts.extend((0..N).map(|k| (polar(1.0, k as f32 / N as f32 * TAU), 0)));
            let mut tris = Vec::new();
            for k in 0..N {
                let (a, b) = (1 + k, 1 + (k + 1) % N);
                let (c, d) = (1 + N + k, 1 + N + (k + 1) % N);
                tris.push([0, a, b]);
                tris.push([a, c, d]);
                tris.push([a, d, b]);
            }
            mesh_of(&verts, &tris)
        }
        Prim::Star => {
            const SPIKES: u32 = 8;
            let mut verts: Vec<Vert> = vec![([0.0, 0.0], 255)];
            // Valleys between the spikes, then the tips (alternately long and short).
            for k in 0..SPIKES {
                verts.push((polar(0.16, (k as f32 + 0.5) / SPIKES as f32 * TAU), 200));
            }
            for k in 0..SPIKES {
                let long = if k % 2 == 0 { 1.0 } else { 0.62 };
                verts.push((polar(long, k as f32 / SPIKES as f32 * TAU), 0));
            }
            let mut tris = Vec::new();
            for k in 0..SPIKES {
                let (valley, next_valley) = (1 + k, 1 + (k + SPIKES - 1) % SPIKES);
                let tip = 1 + SPIKES + k;
                tris.push([0, next_valley, tip]);
                tris.push([0, tip, valley]);
            }
            mesh_of(&verts, &tris)
        }
        Prim::Ring => {
            const N: u32 = 28;
            let mut verts: Vec<Vert> = Vec::new();
            // Inner edge, the bright middle, the outer edge.
            for (r, g) in [(0.76, 0u8), (0.88, 255), (1.0, 0)] {
                verts.extend((0..N).map(|k| (polar(r, k as f32 / N as f32 * TAU), g)));
            }
            let mut tris = Vec::new();
            for band in 0..2u32 {
                for k in 0..N {
                    let (a, b) = (band * N + k, band * N + (k + 1) % N);
                    let (c, d) = ((band + 1) * N + k, (band + 1) * N + (k + 1) % N);
                    tris.push([a, c, d]);
                    tris.push([a, d, b]);
                }
            }
            mesh_of(&verts, &tris)
        }
        Prim::Streak => {
            let w = 0.1;
            let verts: Vec<Vert> = vec![([0.0, -1.0], 0), ([-w, -0.45], 200), ([w, -0.45], 200), ([-w * 1.4, 0.45], 255), ([w * 1.4, 0.45], 255), ([0.0, 1.0], 0)];
            mesh_of(&verts, &[[0, 1, 2], [1, 3, 2], [2, 3, 4], [3, 5, 4]])
        }
    }
}

/// A primitive as a one-bone model the avatar renderer draws.
fn prim_asset(prim: Prim) -> Arc<AvatarAsset> {
    let model = FieldModel {
        hrc_name: "effect".into(),
        skeleton_name: "effect".into(),
        bones: vec![ModelBone { name: "root".into(), parent: None, length: 1.0 }],
        parts: vec![prim_part(prim)],
        dropped_triangles: 0,
        missing_textures: Vec::new(),
    };
    let clip = super::model::still_clip(1);
    let character = Character { appearance: NON_PLAYABLE, model, idle: clip.clone(), walk: clip.clone(), run: clip };
    Arc::new(AvatarAsset::new(character).expect("a primitive has triangles"))
}

/// The four meshes, made once and shared (see the module docs).
pub struct EffectKit {
    assets: [Arc<AvatarAsset>; 4],
}

impl EffectKit {
    pub fn new() -> Self {
        Self { assets: [prim_asset(Prim::Disc), prim_asset(Prim::Star), prim_asset(Prim::Ring), prim_asset(Prim::Streak)] }
    }

    /// The asset of `prim`.
    pub fn asset(&self, prim: Prim) -> &Arc<AvatarAsset> {
        &self.assets[match prim {
            Prim::Disc => 0,
            Prim::Star => 1,
            Prim::Ring => 2,
            Prim::Streak => 3,
        }]
    }
}

impl Default for EffectKit {
    fn default() -> Self {
        Self::new()
    }
}

/// The matrix that puts a primitive of radius `radius` centred at `centre` (battle space), turned
/// by `spin` about the view axis, facing a camera whose axes in battle space are `right`, `down` and
/// `forward`: local `x` goes along `right`, local `y` along `down`.
pub fn billboard(centre: [f32; 3], radius: f32, spin: f32, axes: ([f32; 3], [f32; 3], [f32; 3])) -> [f32; 16] {
    let (right, down, forward) = axes;
    let (s, c) = spin.sin_cos();
    // Spin in the screen plane: x' = c x - s y, y' = s x + c y.
    let x = [right[0] * c + down[0] * s, right[1] * c + down[1] * s, right[2] * c + down[2] * s];
    let y = [down[0] * c - right[0] * s, down[1] * c - right[1] * s, down[2] * c - right[2] * s];
    [
        x[0] * radius,
        x[1] * radius,
        x[2] * radius,
        0.0,
        y[0] * radius,
        y[1] * radius,
        y[2] * radius,
        0.0,
        forward[0] * radius,
        forward[1] * radius,
        forward[2] * radius,
        0.0,
        centre[0],
        centre[1],
        centre[2],
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn an_element_is_read_from_the_attacks_mask_and_a_heal_is_cure() {
        assert_eq!(Element::from_mask(element::FIRE, false), Element::Fire);
        assert_eq!(Element::from_mask(element::ICE, false), Element::Ice);
        assert_eq!(Element::from_mask(element::BOLT, false), Element::Bolt);
        assert_eq!(Element::from_mask(element::RESTORATIVE, false), Element::Cure);
        assert_eq!(Element::from_mask(element::FIRE, true), Element::Cure, "a restoring action is cure whatever its mask");
        assert_eq!(Element::from_mask(0, false), Element::Plain);
        assert_eq!(Element::from_mask(element::CUT | element::HIT, false), Element::Plain, "a weapon's element is not a colour");
        // The first of a mask's elements in the order fire, ice, bolt wins.
        assert_eq!(Element::from_mask(element::BOLT | element::ICE, false), Element::Ice);
        // Every element has a colour of its own.
        let all = [Element::Plain, Element::Fire, Element::Ice, Element::Bolt, Element::Cure, Element::Earth, Element::Wind, Element::Water, Element::Holy, Element::Poison, Element::Gravity];
        for (i, a) in all.iter().enumerate() {
            assert!(a.rgb().iter().all(|c| (0.0..=1.0).contains(c)));
            for b in &all[i + 1..] {
                assert_ne!(a.rgb(), b.rgb(), "{a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn the_four_elements_of_the_lead_look_different_from_each_other() {
        // Fire, ice, bolt and cure differ in their pieces, not only in colour: embers, a star of shards,
        // streaks falling, sparkles rising.
        let shapes = |e: Element| -> Vec<Prim> { burst_layers(BurstKind::Spell, e, 0.3, 900.0).iter().map(|l| l.prim).collect() };
        let (fire, ice, bolt, cure) = (shapes(Element::Fire), shapes(Element::Ice), shapes(Element::Bolt), shapes(Element::Cure));
        assert!(fire.iter().filter(|p| **p == Prim::Disc).count() > 3, "embers: {fire:?}");
        assert!(ice.iter().filter(|p| **p == Prim::Star).count() >= 2, "{ice:?}");
        assert_eq!(bolt.iter().filter(|p| **p == Prim::Streak).count(), 3, "{bolt:?}");
        assert!(cure.iter().filter(|p| **p == Prim::Star).count() >= 5, "{cure:?}");
        let colours: Vec<[f32; 3]> = [Element::Fire, Element::Ice, Element::Bolt, Element::Cure].iter().map(|e| burst_layers(BurstKind::Spell, *e, 0.3, 900.0)[1].rgb).collect();
        for i in 0..4 {
            for j in i + 1..4 {
                assert_ne!(colours[i], colours[j]);
            }
        }
    }

    #[test]
    fn every_effect_is_whole_in_its_life_fades_and_is_gone_at_the_end() {
        for kind in [BurstKind::Spell, BurstKind::Item, BurstKind::Hit { critical: false }, BurstKind::Hit { critical: true }] {
            for element in [Element::Plain, Element::Fire, Element::Ice, Element::Bolt, Element::Cure, Element::Earth] {
                let mut last = f32::MAX;
                for step in 0..20 {
                    let age = step as f32 / 20.0;
                    let layers = burst_layers(kind, element, age, 900.0);
                    assert!(!layers.is_empty(), "{kind:?} {element:?} at {age}");
                    for l in &layers {
                        assert!((0.0..=1.0).contains(&l.alpha) && l.radius > 0.0 && l.radius.is_finite(), "{l:?}");
                        assert!(l.offset.iter().all(|c| c.is_finite()) && l.spin.is_finite() && l.rgb.iter().all(|c| (0.0..=1.0).contains(c)));
                    }
                    // The brightest piece never grows brighter as the burst ages (it fades out).
                    let peak = layers.iter().map(|l| l.alpha).fold(0.0, f32::max);
                    assert!(peak <= last + 1e-4, "{kind:?} {element:?} at {age}: {peak} after {last}");
                    last = peak;
                }
                assert!(burst_layers(kind, element, 1.0, 900.0).is_empty() && burst_layers(kind, element, -0.1, 900.0).is_empty());
                assert!(burst_layers(kind, element, f32::NAN, 900.0).is_empty());
            }
        }
        // A critical blow is bigger and gold; a boss's burst is bigger than a hound's.
        let small = burst_layers(BurstKind::Hit { critical: false }, Element::Plain, 0.3, 800.0)[0];
        let crit = burst_layers(BurstKind::Hit { critical: true }, Element::Plain, 0.3, 800.0)[0];
        assert!(crit.radius > small.radius && crit.rgb != small.rgb);
        let boss = burst_layers(BurstKind::Hit { critical: false }, Element::Plain, 0.3, 2400.0)[0];
        assert!(boss.radius > small.radius * 2.0);
    }

    #[test]
    fn a_caster_gathers_a_glow_that_grows_and_a_ring_that_closes_in() {
        let (a, b) = (charge_layers(Element::Fire, 0.0, 850.0), charge_layers(Element::Fire, 1.0, 850.0));
        assert_eq!(a.len(), 3);
        assert!(b[0].radius > a[0].radius && b[0].alpha > a[0].alpha, "the bloom grows and brightens");
        assert!(b[1].radius < a[1].radius, "the ring closes in");
        assert!(charge_layers(Element::Fire, 1.1, 850.0).is_empty() && charge_layers(Element::Fire, f32::NAN, 850.0).is_empty());
    }

    #[test]
    fn the_primitives_are_additive_meshes_with_triangles_that_fall_to_black() {
        for prim in [Prim::Disc, Prim::Star, Prim::Ring, Prim::Streak] {
            let part = prim_part(prim);
            assert_eq!(part.blend, PartBlend::Additive, "{prim:?}");
            assert_eq!(part.positions.len(), part.colors.len());
            assert!(part.indices.len() >= 12 && part.indices.len().is_multiple_of(3) && part.indices.iter().all(|&i| (i as usize) < part.positions.len()));
            // Brightest at the middle, nothing at the rim (the colour of an additive part is its light).
            assert!(part.colors.iter().any(|c| c[0] == 255) && part.colors.iter().any(|c| c[0] == 0), "{prim:?}");
            let radius = part.positions.iter().map(|p| p[0].hypot(p[1])).fold(0.0, f32::max);
            assert!((0.99..=1.5).contains(&radius), "{prim:?}: {radius}");
        }
        // They merge into meshes the renderer can draw, with one additive batch each.
        let kit = EffectKit::new();
        for prim in [Prim::Disc, Prim::Star, Prim::Ring, Prim::Streak] {
            let mesh = &kit.asset(prim).mesh;
            assert_eq!(mesh.batches.len(), 1);
            assert_eq!(mesh.batches[0].kind, crate::avatar::mesh::BatchKind::Additive);
        }
        // Distinct assets: four meshes on the GPU however many effects are drawn.
        let ids: std::collections::BTreeSet<u64> = [Prim::Disc, Prim::Star, Prim::Ring, Prim::Streak].iter().map(|p| kit.asset(*p).id).collect();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn a_billboard_faces_the_camera_and_turns_in_the_screen_plane() {
        // A camera looking along +x with right = +z, down = +y (the battle camera's own frame).
        let axes = ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]);
        let m = billboard([10.0, 20.0, 30.0], 5.0, 0.0, axes);
        let apply = |v: [f32; 3]| [m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12], m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13], m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14]];
        // Local x lands to the camera's right (+z), local y down (+y), scaled by the radius, about the centre.
        assert_eq!(apply([0.0; 3]), [10.0, 20.0, 30.0]);
        assert_eq!(apply([1.0, 0.0, 0.0]), [10.0, 20.0, 35.0]);
        assert_eq!(apply([0.0, 1.0, 0.0]), [10.0, 25.0, 30.0]);
        // A quarter turn about the view axis takes local x to where local y was.
        let q = billboard([0.0; 3], 1.0, PI / 2.0, axes);
        let moved = [q[0], q[1], q[2]];
        assert!((moved[1] - 1.0).abs() < 1e-5 && moved[2].abs() < 1e-5, "{moved:?}");
    }
}
