//! Procedural battle effects: what a performance LOOKS like in the 3D scene, made of a handful of shared glow meshes.
//!
//! Nothing here is read from the game and nothing is the game's: the pictures are our own, drawn by our renderer from
//! seven tiny meshes ([`Prim`]: a soft disc, a star, a ring, a streak, a lightning bolt, a crescent blade and a soft
//! ribbon) that the avatar renderer draws ADDITIVELY like any other model (they are [`PartBlend::Additive`] parts of a
//! one-bone model), each with a per-draw tint ([`crate::avatar::gpu::Tint`]) that colours it and fades it. So they cost
//! the renderer nothing new, and the meshes are made once per [`EffectKit`] and shared by every effect of every battle
//! (the GPU keeps a bounded number of models: an asset per effect would evict the actors every frame, which a test of
//! the scene holds against, and so does one of the number of meshes the kit has).
//!
//! # What chooses the effect
//!
//! The ENGINE's own data, as the replayed battle describes it ([`fx::show`] is the one table, unit-tested):
//!
//! | what lands on a target | effect | chosen by |
//! |---|---|---|
//! | a blow's damage | slash, thump or punch flash, a critical hit bigger and gold | the attack record's element bits (`CUT`, `HIT`, `PUNCH`) |
//! | a shot's damage | a bolt flies from the shooter, a small impact | the `SHOOT` bit (a gun, a laser, a thrown rock) |
//! | a spell's damage | a blast in the element's colours and shapes | the element mask: fire, ice, bolt, earth, poison, gravity, water, wind, holy, none |
//! | a miss | a small whiff beside the target | the `Miss` event |
//! | HP, MP, a revival | green sparkles and a rising ring, blue ones, a pillar of light | the `Heal`, `MpRestored`, `Revive` events |
//! | a status inflicted | a sign of the status (Zs for Sleep, bubbles for Poison, a shell for Barrier, speed lines for Haste, ...) | the status; a buff is [`Status::is_beneficial`] |
//! | a status cured or run out | a wipe and the ailment's colours leaving | the `StatusEnded` event |
//! | an item | the same, thrown to the target first (a glint on an arc) | the item's own record ([`Style::Item`]) |
//! | a limit break | the same, bigger, with a flash over the whole picture | the limit's own record ([`Style::Limit`]) |
//!
//! Whole-side actions have an event per target, so every target shows its effect. A guard shimmers at the guard and
//! a spell that found nobody to land on fizzles at its caster.
//!
//! # When
//!
//! An [`fx::Fx`] has a start, a landing and a life after it, all on the cue timeline ([`super::cue`]): it begins with its
//! performance (the glow a caster gathers, the bolt leaving the muzzle, the item in the air), peaks at the landing (the
//! instant the numbers are born) and fades out. [`fx::layers`] is a pure function of the clock, so every curve is
//! unit-tested without a window; a counter's effects never draw before the counter begins.
//!
//! # Cost
//!
//! A piece is one draw of a few dozen vertices; an effect has at most [`fx::MAX_LAYERS_PER_FX`] of them and the scene at
//! most [`MAX_LAYERS`] a frame (the newest effects win), so a whole-side spell plus a limit break is a couple of hundred
//! small draws. Particles are not state: each is a pure function of the effect's age and its seed ([`noise`]), so nothing
//! is stored or allocated per particle.

mod fx;
mod looks;
#[cfg(test)]
mod tests;

pub use fx::{layers, show, Fx, Geo, Kind, Phase, MAX_LAYERS_PER_FX};

use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use fmv_ff7::model::ModelBone;
use fmv_ff7::playable::Character;
use fmv_ff7::{FieldModel, MeshPart, PartBlend};
use fmv_ff7_battle::rules::element;
use fmv_ff7_battle::status::Status;
use std::f32::consts::TAU;
use std::sync::Arc;

/// The most pieces the scene draws for effects in one frame; past it the oldest effects give way.
pub const MAX_LAYERS: usize = 240;

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

/// The colour of a status's sign, each channel in `0..=1` (the panels' own colours, brightened for a glow).
pub fn status_rgb(status: Status) -> [f32; 3] {
    match status {
        Status::Poison => [0.72, 0.42, 0.92],
        Status::Sleep => [0.52, 0.64, 1.0],
        Status::Stop => [0.74, 0.78, 0.9],
        Status::Paralysis => [1.0, 0.86, 0.26],
        Status::Silence => [0.72, 0.72, 0.86],
        Status::Sadness => [0.42, 0.56, 0.95],
        Status::Fury => [1.0, 0.36, 0.28],
        Status::Darkness => [0.52, 0.52, 0.74],
        Status::Slow => [0.72, 0.52, 0.96],
        Status::Haste => [0.4, 0.92, 1.0],
        Status::Regen => [0.46, 1.0, 0.56],
        Status::Barrier => [0.46, 0.8, 1.0],
        Status::MBarrier => [1.0, 0.56, 0.9],
        _ => [0.9, 0.9, 1.0],
    }
}

/// How a physical blow strikes, from the attack record's element bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strike {
    /// Crossing slashes (a blade, a claw: the `CUT` bit, and the Attack command's default).
    Slash,
    /// A round impact with a shock ring (the `HIT` bit: a tail, a fist of the body, a tonfa; the `SHOUT` bit).
    Thump,
    /// A burst of rays (the `PUNCH` bit: a claw of the hand, a punch).
    Punch,
}

impl Strike {
    /// The strike of an attack record's element mask: `PUNCH`, then `HIT` or `SHOUT`, then `CUT`; a record that says
    /// none of them (and the party's Attack command) slashes.
    pub fn of_mask(mask: u16) -> Self {
        if mask & element::PUNCH != 0 {
            Self::Punch
        } else if mask & (element::HIT | element::SHOUT) != 0 && mask & element::CUT == 0 {
            Self::Thump
        } else {
            Self::Slash
        }
    }
}

/// How an action is staged, which is what decides the effects around it (see the table in the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// A weapon blow or an enemy's body attack: it strikes the target.
    Blow(Strike),
    /// Fired from where the shooter stands (the record's `SHOOT` bit): a bolt flies from it to each target.
    Shot,
    /// A spell, and what is cast like one (an enemy's magical attack): it breaks over each target by its element.
    Spell,
    /// An item: thrown to the target, takes effect there.
    Item,
    /// A limit break: a physical one (Braver, Beat Rush) strikes like a blow, a magical or a healing one breaks like a
    /// spell; either way it is bigger and flashes.
    Limit { physical: bool },
}

/// What an action looks like, as the engine says it: whether its performer runs up to its target, the colour of its
/// effect and how it is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub melee: bool,
    pub element: Element,
    pub style: Style,
}

impl Look {
    /// A plain physical blow: it runs up, its sparks are white, it slashes.
    pub const PLAIN: Look = Look { melee: true, element: Element::Plain, style: Style::Blow(Strike::Slash) };
}

/// The meshes every effect is made of, in a unit square facing the camera (`x` right, `y` down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prim {
    /// A round glow, white at the middle and black (nothing, added) at the rim, radius 1.
    Disc,
    /// A star of eight spikes, radius 1.
    Star,
    /// A thin soft ring, outer radius 1.
    Ring,
    /// A tall thin streak from `y = -1` to `y = 1`, pointed at both ends.
    Streak,
    /// A jagged lightning bolt from `y = -1` to `y = 1`, bright along its spine.
    Bolt,
    /// A curved blade: the far rim of the unit circle from -57 to +57 degrees about `+x`, pointed at both ends.
    Crescent,
    /// A ribbon from `y = -1` to `y = 1`, flat across the middle, soft at the sides and at both ends.
    Beam,
}

/// Every [`Prim`], in the order of the kit's assets.
pub const PRIMS: [Prim; 7] = [Prim::Disc, Prim::Star, Prim::Ring, Prim::Streak, Prim::Bolt, Prim::Crescent, Prim::Beam];

impl Prim {
    fn index(self) -> usize {
        PRIMS.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// Where a piece of an effect is, in the frame of the effect: offsets are battle units from the effect's anchor (the
/// middle of a fighter's chest) along the camera's axes: right, down, towards the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    /// Facing the camera: the middle of the piece, its half-size across (`x`) and along (`y`) before it is turned, and
    /// its turn about the view axis in radians (0: `y` points down the screen).
    Facing { at: [f32; 3], half: [f32; 2], spin: f32 },
    /// A ribbon from one point to another, `half` wide, turned to face the camera as far as it can (a bolt, a beam).
    Between { from: [f32; 3], to: [f32; 3], half: f32 },
    /// Over the whole picture, in front of everyone (a flash): the scene sizes it to the window.
    Screen,
}

/// One drawn piece of an effect at one instant: a primitive, where, in which colour and how strongly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    pub prim: Prim,
    pub place: Place,
    pub rgb: [f32; 3],
    /// The intensity, `0..=1` (it multiplies the glow).
    pub alpha: f32,
}

impl Layer {
    /// A round piece of `radius` at `at`, turned by `spin`.
    pub fn round(prim: Prim, at: [f32; 3], radius: f32, spin: f32, rgb: [f32; 3], alpha: f32) -> Self {
        Self { prim, place: Place::Facing { at, half: [radius, radius], spin }, rgb, alpha }
    }

    /// A piece of half-size `half` (across, along) at `at`, turned by `spin`.
    pub fn oval(prim: Prim, at: [f32; 3], half: [f32; 2], spin: f32, rgb: [f32; 3], alpha: f32) -> Self {
        Self { prim, place: Place::Facing { at, half, spin }, rgb, alpha }
    }

    /// A ribbon from `from` to `to`, `half` wide.
    pub fn between(prim: Prim, from: [f32; 3], to: [f32; 3], half: f32, rgb: [f32; 3], alpha: f32) -> Self {
        Self { prim, place: Place::Between { from, to, half }, rgb, alpha }
    }

    /// A piece over the whole picture.
    pub fn screen(rgb: [f32; 3], alpha: f32) -> Self {
        Self { prim: Prim::Disc, place: Place::Screen, rgb, alpha }
    }

    /// Whether every number of the piece is a number and its intensity is a fraction (what [`layers`] guarantees).
    pub fn is_sound(&self) -> bool {
        let finite = |v: &[f32]| v.iter().all(|c| c.is_finite());
        let place = match self.place {
            Place::Facing { at, half, spin } => finite(&at) && finite(&half) && spin.is_finite() && half[0] > 0.0 && half[1] > 0.0,
            Place::Between { from, to, half } => finite(&from) && finite(&to) && half.is_finite() && half > 0.0,
            Place::Screen => true,
        };
        place && finite(&self.rgb) && self.rgb.iter().all(|c| (0.0..=1.0).contains(c)) && (0.0..=1.0).contains(&self.alpha)
    }
}

/// A number in `0.0..1.0` that depends only on `seed` and `k` (a hash): the same effect has the same particles every
/// frame, and two effects (two seeds) do not look alike.
pub fn noise(seed: u32, k: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B1) ^ k.wrapping_mul(0x85EB_CA6B).wrapping_add(0x1B87_3593);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x >> 8) as f32 / (1u32 << 24) as f32
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

/// A grid of `rows` x `cols` vertices (row by row) as quads of two triangles each.
fn grid(rows: u32, cols: u32) -> Vec<[u32; 3]> {
    let mut tris = Vec::new();
    for r in 0..rows - 1 {
        for c in 0..cols - 1 {
            let (a, b) = (r * cols + c, r * cols + c + 1);
            let (d, e) = ((r + 1) * cols + c, (r + 1) * cols + c + 1);
            tris.push([a, d, e]);
            tris.push([a, e, b]);
        }
    }
    tris
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
        Prim::Bolt => {
            // A jagged strip from the top to the bottom: a bright spine with a soft edge on each side, thinner at the ends.
            const KINKS: [f32; 8] = [0.0, 0.34, -0.30, 0.36, -0.32, 0.30, -0.26, 0.0];
            let n = KINKS.len() as u32;
            let mut verts: Vec<Vert> = Vec::new();
            for (k, &x) in KINKS.iter().enumerate() {
                let t = k as f32 / (n - 1) as f32;
                let w = 0.12 * (1.0 - (2.0 * t - 1.0).powi(2) * 0.8);
                let y = -1.0 + 2.0 * t;
                verts.push(([x - w, y], 0));
                verts.push(([x, y], 255));
                verts.push(([x + w, y], 0));
            }
            let mut tris = Vec::new();
            for k in 0..n - 1 {
                let (a, b) = (k * 3, (k + 1) * 3);
                tris.push([a, a + 1, b + 1]);
                tris.push([a, b + 1, b]);
                tris.push([a + 1, a + 2, b + 2]);
                tris.push([a + 1, b + 2, b + 1]);
            }
            mesh_of(&verts, &tris)
        }
        Prim::Crescent => {
            // The far rim of the unit circle between -A and +A about +x, thick in the middle and pointed at both ends, bright
            // along its spine.
            const SEG: u32 = 12;
            const A: f32 = 1.0;
            let mut verts: Vec<Vert> = Vec::new();
            for (row, grey) in [(0usize, 0u8), (1, 255), (2, 0)] {
                for k in 0..=SEG {
                    let t = k as f32 / SEG as f32;
                    let thick = 0.28 * (std::f32::consts::PI * t).sin();
                    let r = match row {
                        0 => 1.0 + thick * 0.5,
                        1 => 1.0,
                        _ => 1.0 - thick * 0.5,
                    };
                    verts.push((polar(r, -A + 2.0 * A * t), grey));
                }
            }
            mesh_of(&verts, &grid(3, SEG + 1))
        }
        Prim::Beam => {
            // Rows from the head to the tail (the ends fade out), columns across (the edges fade out).
            let across = [(-1.0f32, 0.0f32), (-0.35, 1.0), (0.35, 1.0), (1.0, 0.0)];
            let along = [(-1.0f32, 0.0f32), (-0.82, 1.0), (0.82, 1.0), (1.0, 0.0)];
            let mut verts: Vec<Vert> = Vec::new();
            for (y, f) in along {
                for (x, g) in across {
                    verts.push(([x, y], (255.0 * f * g) as u8));
                }
            }
            mesh_of(&verts, &grid(4, 4))
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

/// The meshes, made once and shared (see the module docs).
pub struct EffectKit {
    assets: [Arc<AvatarAsset>; PRIMS.len()],
}

impl EffectKit {
    pub fn new() -> Self {
        Self { assets: PRIMS.map(prim_asset) }
    }

    /// The asset of `prim`.
    pub fn asset(&self, prim: Prim) -> &Arc<AvatarAsset> {
        &self.assets[prim.index()]
    }

    /// How many meshes the kit has: the GPU holds a bounded number of models, and these are among them (tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

impl Default for EffectKit {
    fn default() -> Self {
        Self::new()
    }
}

/// The matrix that puts a primitive of half-size `half` (across, along) centred at `centre` (battle space), turned by
/// `spin` about the view axis, facing a camera whose axes in battle space are `right`, `down` and `forward`: local `x`
/// goes along `right`, local `y` along `down`.
pub fn billboard(centre: [f32; 3], half: [f32; 2], spin: f32, axes: ([f32; 3], [f32; 3], [f32; 3])) -> [f32; 16] {
    let (right, down, forward) = axes;
    let (s, c) = spin.sin_cos();
    // Spin in the screen plane: x' = c x - s y, y' = s x + c y.
    let x = [right[0] * c + down[0] * s, right[1] * c + down[1] * s, right[2] * c + down[2] * s];
    let y = [down[0] * c - right[0] * s, down[1] * c - right[1] * s, down[2] * c - right[2] * s];
    let depth = half[0].max(half[1]);
    [
        x[0] * half[0],
        x[1] * half[0],
        x[2] * half[0],
        0.0,
        y[0] * half[1],
        y[1] * half[1],
        y[2] * half[1],
        0.0,
        forward[0] * depth,
        forward[1] * depth,
        forward[2] * depth,
        0.0,
        centre[0],
        centre[1],
        centre[2],
        1.0,
    ]
}

/// The matrix that stretches a primitive (`y` from -1 to 1) from `from` to `to` (battle space), `half` wide, turned about
/// the segment to face a camera looking along `forward`. `None` for a segment too short to have a direction.
pub fn ribbon(from: [f32; 3], to: [f32; 3], half: f32, right: [f32; 3], forward: [f32; 3]) -> Option<[f32; 16]> {
    let along = [(to[0] - from[0]) * 0.5, (to[1] - from[1]) * 0.5, (to[2] - from[2]) * 0.5];
    let length = (along[0] * along[0] + along[1] * along[1] + along[2] * along[2]).sqrt();
    if !length.is_finite() || length < 1e-3 {
        return None;
    }
    let middle = [(from[0] + to[0]) * 0.5, (from[1] + to[1]) * 0.5, (from[2] + to[2]) * 0.5];
    // Across the segment and across the view: the ribbon's width faces the camera.
    let mut across = [along[1] * forward[2] - along[2] * forward[1], along[2] * forward[0] - along[0] * forward[2], along[0] * forward[1] - along[1] * forward[0]];
    let mut size = (across[0] * across[0] + across[1] * across[1] + across[2] * across[2]).sqrt();
    if size < length * 1e-3 {
        // Pointing straight at (or away from) the camera: any way across will do.
        across = right;
        size = 1.0;
    }
    let across = across.map(|c| c / size * half);
    Some([across[0], across[1], across[2], 0.0, along[0], along[1], along[2], 0.0, forward[0] * half, forward[1] * half, forward[2] * half, 0.0, middle[0], middle[1], middle[2], 1.0])
}
