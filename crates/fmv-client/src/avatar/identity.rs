//! The player's identity colour on a model.
//!
//! A player's colour used to reach a model as a per-draw colour factor ([`super::gpu::Tint`]: each channel of the
//! model's own colour scaled by `0.45 + 0.55 * c / 255`). That multiplies, so it cannot add a hue the cloth does not
//! have: the dark blue cloak of a body worn by an orange player came out a darker, greyer blue, and a skin tone was
//! pulled to brown. This is the replacement: the colour is carried per draw as an [`Identity`] (a second
//! instance-rate vec4 next to the tint; a model is never copied per player, the GPU cache holds 32), and the
//! fragment shader repaints the cloth in it.
//!
//! # The look
//!
//! Everything is computed on the authored colours, the sRGB bytes of the files as floats in 0..1, on either kind of
//! target (an sRGB target gets the lit colour re-encoded first and the result decoded again, so the two kinds show
//! the same picture). `c` is the lit colour of the fragment, vertex colour times texel; `id` the player's colour.
//!
//! * **Cloth** is repainted in the identity's hue and saturation at the cloth's own value: `hsv(hue(id),
//!   mix(s, s(id), 0.88), (0.2 + 0.8 * v^0.6) * mix(1, v(id), 0.5))`. The value is the cloth's (so the shading of the
//!   model, its folds, belts, shoes and shirt stay told apart) lifted by a curve that keeps the darkest cloth from
//!   being black: a dark cloak or a black suit turns plainly into the identity colour (a lightness floor of 0.2 and a
//!   power of 0.6, which maps 0.1 to 0.40 and 0.3 to 0.59), where the old floor left it nearly as dark as it was.
//!   Bright cloth keeps paler (its saturation is cut by up to 35%), so a white shirt becomes a pastel of the colour,
//!   not a saturated block. A dark identity colour darkens the cloth with it. A tenth of the cloth's own colour is
//!   kept, so a gold buckle is not entirely lost.
//! * **Skin** is not repainted. A smooth mask over hue (peach, tan: -20 to 56 degrees), saturation (0.1 to 0.88) and
//!   value (above the very dark, so a dark brown skin is kept too) picks skin-coloured texels, which take only a weak
//!   multiplicative tint (`c * (0.95 + 0.05 * id)`: at most 5% of a channel). The face decals (eyes, brows, mouth: the
//!   `overlay` flag of the merged mesh, which in the owned models is the textured opaque parts and nothing else,
//!   under 1% of any model) take the same weak tint whatever their colour. **Hair keeps its colour too.** The mask cannot
//!   tell dark brown hair from dark brown shoes (both are very saturated and very dark, outside its gates: a blue player's
//!   `lean` had blue hair), so the mesh flags the hair itself: the vertices of the HEAD bone whose colour is a hair colour
//!   ([`head_colour_is_own`]: the skin and hair hues, black and dark grey; not the white of a cap or the blue of a hood)
//!   are marked `own`, and the shader gives them the same weak tint as a decal. The shoes, belts and suits stay cloth.
//! * **Grey identities** (white, grey, black: the colour sliders allow them) have no hue to paint with: the cloth
//!   falls back to the old multiplicative look, over a smooth gate on the identity's chroma (a white identity leaves
//!   the model as it is, as it always did).
//!
//! `strength` 0 skips all of it: the shader then returns `rgb * texel.rgb * tint.rgb`, bit for bit what it always
//! returned. The constants below exist twice, in the shader text and in the CPU mirror the tests use as the oracle
//! (`look`); a test holds the two together.

/// The identity colour of one draw and how strongly it applies. A model that has none (the default) is drawn as it
/// is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Identity {
    /// The player's colour: sRGB bytes as floats in 0..1 (the authored space, whatever the target).
    pub rgb: [f32; 3],
    /// 0 draws the model exactly as it is; 1 is the full look.
    pub strength: f32,
}

impl Identity {
    /// No identity colour.
    pub const NONE: Identity = Identity { rgb: [1.0; 3], strength: 0.0 };

    /// A player's identity colour (sRGB bytes) at full strength.
    pub fn of(rgb: [u8; 3]) -> Self {
        Self { rgb: rgb.map(|c| f32::from(c) / 255.0), strength: 1.0 }
    }

    /// The four floats the shader reads.
    pub fn to_array(self) -> [f32; 4] {
        [self.rgb[0], self.rgb[1], self.rgb[2], self.strength]
    }
}

impl Default for Identity {
    fn default() -> Self {
        Self::NONE
    }
}

/// (hue in degrees from -180 to 180 with 0 = red, saturation, value) of an sRGB colour given as floats in 0..1: the one
/// arithmetic the shader's `id_hsv` and the CPU mirror use.
pub(crate) fn hsv(c: [f32; 3]) -> [f32; 3] {
    let mx = c[0].max(c[1].max(c[2]));
    let mn = c[0].min(c[1].min(c[2]));
    let d = mx - mn;
    let mut h = 0.0;
    if d > 0.00001 {
        h = if mx == c[0] {
            (c[1] - c[2]) / d
        } else if mx == c[1] {
            2.0 + (c[2] - c[0]) / d
        } else {
            4.0 + (c[0] - c[1]) / d
        };
        h *= 60.0;
        if h > 180.0 {
            h -= 360.0;
        }
        if h <= -180.0 {
            h += 360.0;
        }
    }
    [h, if mx > 0.00001 { d / mx } else { 0.0 }, mx]
}

/// Hues of skin and of every hair a person has (brown, red, blond), in degrees: the skin mask's own range.
const HEAD_OWN_HUE: std::ops::RangeInclusive<f32> = -20.0..=56.0;
/// A colour needs this much saturation to count as a hue (below it the head's colour is a grey).
const HEAD_OWN_SATURATION: f32 = 0.1;
/// A grey is hair (black, dark grey) or a visor, not cloth, only when it is this dark and this grey.
const HEAD_DARK_VALUE: f32 = 0.4;
const HEAD_DARK_SATURATION: f32 = 0.25;

/// Does a vertex of the HEAD keep its own colour under a player's identity colour although it is no face decal?
///
/// Hair is the case. It is not cloth, but the skin mask of the shader cannot say so: a dark brown is both very saturated and
/// very dark, which the mask's gates exclude (a blue player's `lean` had BLUE HAIR, and a brown bob went orange). Only the
/// vertices of the head bone ask (`mesh::MergedMesh::build`), so the shoes, the belts and the suits that are the same dark
/// brown or black elsewhere in the model stay cloth. On the head these keep their colour: every colour of the skin and hair
/// hues (brown, red, blond, skin itself) and the dark greys (black hair, a visor). A cap or a hood keeps being cloth: white and
/// light grey (a cap) and the cool hues (blue, green, violet: a hood, a helmet).
pub(crate) fn head_colour_is_own(rgb: [u8; 3]) -> bool {
    let [h, s, v] = hsv(rgb.map(|c| f32::from(c) / 255.0));
    (s >= HEAD_OWN_SATURATION && HEAD_OWN_HUE.contains(&h)) || (s < HEAD_DARK_SATURATION && v < HEAD_DARK_VALUE)
}

/// The identity look in WGSL (`identity_look`), for the model shader. A macro so that it is spelled once, in a
/// literal `concat!` can take.
macro_rules! identity_wgsl {
    () => {
        r#"
// ---- the player's identity colour (see avatar/identity.rs) --------------------------------------------------------
const ID_GREY_FLOOR: f32 = 0.45;
const ID_GATE_LO: f32 = 0.05;
const ID_GATE_HI: f32 = 0.2;
const ID_V_FLOOR: f32 = 0.2;
const ID_V_GAMMA: f32 = 0.6;
const ID_V_OF_ID: f32 = 0.5;
const ID_S_MIX: f32 = 0.88;
const ID_PASTEL: f32 = 0.35;
const ID_PASTEL_V0: f32 = 0.7;
const ID_CLOTH_MIX: f32 = 0.9;
const ID_KEEP_FLOOR: f32 = 0.95;
const ID_HUE_0: f32 = -20.0;
const ID_HUE_1: f32 = -5.0;
const ID_HUE_2: f32 = 42.0;
const ID_HUE_3: f32 = 56.0;
const ID_SAT_0: f32 = 0.1;
const ID_SAT_1: f32 = 0.22;
const ID_SAT_2: f32 = 0.72;
const ID_SAT_3: f32 = 0.88;
const ID_VAL_0: f32 = 0.12;
const ID_VAL_1: f32 = 0.26;

// (hue in degrees from -180 to 180, saturation, value)
fn id_hsv(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    var h = 0.0;
    if (d > 0.00001) {
        if (mx == c.r) {
            h = (c.g - c.b) / d;
        } else if (mx == c.g) {
            h = 2.0 + (c.b - c.r) / d;
        } else {
            h = 4.0 + (c.r - c.g) / d;
        }
        h = h * 60.0;
        if (h > 180.0) {
            h = h - 360.0;
        }
        if (h <= -180.0) {
            h = h + 360.0;
        }
    }
    var s = 0.0;
    if (mx > 0.00001) {
        s = d / mx;
    }
    return vec3<f32>(h, s, mx);
}

fn id_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let h6 = (h - 360.0 * floor(h / 360.0)) / 60.0;
    let x = vec3<f32>(5.0, 3.0, 1.0) + vec3<f32>(h6);
    let xm = x - vec3<f32>(6.0) * floor(x / vec3<f32>(6.0));
    return vec3<f32>(v) - vec3<f32>(v * s) * clamp(min(xm, vec3<f32>(4.0) - xm), vec3<f32>(0.0), vec3<f32>(1.0));
}

// `c`: the lit colour of the fragment (vertex colour times texel) and `id`: the player's colour, both authored
// (sRGB-encoded) colours; `face` is 1 on the face decals.
fn identity_look(c: vec3<f32>, id: vec3<f32>, strength: f32, face: f32) -> vec3<f32> {
    let hc = id_hsv(c);
    let hi = id_hsv(id);
    // Skin: peach and tan, moderate saturation, anything above the very dark.
    let skin = smoothstep(ID_HUE_0, ID_HUE_1, hc.x) * (1.0 - smoothstep(ID_HUE_2, ID_HUE_3, hc.x))
        * smoothstep(ID_SAT_0, ID_SAT_1, hc.y) * (1.0 - smoothstep(ID_SAT_2, ID_SAT_3, hc.y))
        * smoothstep(ID_VAL_0, ID_VAL_1, hc.z);
    // Cloth: the identity's hue and saturation at the cloth's own (lifted) value.
    let s_id = hi.y * (1.0 - ID_PASTEL * smoothstep(ID_PASTEL_V0, 1.0, hc.z));
    let s2 = mix(hc.y, s_id, ID_S_MIX);
    let v2 = (ID_V_FLOOR + (1.0 - ID_V_FLOOR) * pow(hc.z, ID_V_GAMMA)) * mix(1.0, hi.z, ID_V_OF_ID);
    let dyed = id_rgb(hi.x, s2, min(v2, 1.0));
    // A grey identity has no hue to paint with: the old multiplicative look.
    let chroma = max(id.r, max(id.g, id.b)) - min(id.r, min(id.g, id.b));
    let old = c * (vec3<f32>(ID_GREY_FLOOR) + vec3<f32>(1.0 - ID_GREY_FLOOR) * id);
    let cloth = mix(c, mix(old, dyed, smoothstep(ID_GATE_LO, ID_GATE_HI, chroma)), ID_CLOTH_MIX);
    // Skin and the face decals keep their own colours, under a weak multiplicative tint.
    let kept = c * (vec3<f32>(ID_KEEP_FLOOR) + vec3<f32>(1.0 - ID_KEEP_FLOOR) * id);
    let out = mix(cloth, kept, max(skin, face));
    return mix(c, out, strength);
}
"#
    };
}
pub(super) use identity_wgsl;

#[cfg(test)]
pub(crate) mod look {
    //! The CPU mirror of the look (the oracle of the unit tests and of the picture tests): the same arithmetic as the
    //! shader's `identity_look` and of the colour path of `fs_model`.
    use super::Identity;

    pub const ID_GREY_FLOOR: f32 = 0.45;
    pub const ID_GATE_LO: f32 = 0.05;
    pub const ID_GATE_HI: f32 = 0.2;
    pub const ID_V_FLOOR: f32 = 0.2;
    pub const ID_V_GAMMA: f32 = 0.6;
    pub const ID_V_OF_ID: f32 = 0.5;
    pub const ID_S_MIX: f32 = 0.88;
    pub const ID_PASTEL: f32 = 0.35;
    pub const ID_PASTEL_V0: f32 = 0.7;
    pub const ID_CLOTH_MIX: f32 = 0.9;
    pub const ID_KEEP_FLOOR: f32 = 0.95;
    pub const ID_HUE_0: f32 = -20.0;
    pub const ID_HUE_1: f32 = -5.0;
    pub const ID_HUE_2: f32 = 42.0;
    pub const ID_HUE_3: f32 = 56.0;
    pub const ID_SAT_0: f32 = 0.1;
    pub const ID_SAT_1: f32 = 0.22;
    pub const ID_SAT_2: f32 = 0.72;
    pub const ID_SAT_3: f32 = 0.88;
    pub const ID_VAL_0: f32 = 0.12;
    pub const ID_VAL_1: f32 = 0.26;

    /// Every constant, by the name the shader gives it.
    pub const CONSTANTS: [(&str, f32); 21] = [
        ("ID_GREY_FLOOR", ID_GREY_FLOOR),
        ("ID_GATE_LO", ID_GATE_LO),
        ("ID_GATE_HI", ID_GATE_HI),
        ("ID_V_FLOOR", ID_V_FLOOR),
        ("ID_V_GAMMA", ID_V_GAMMA),
        ("ID_V_OF_ID", ID_V_OF_ID),
        ("ID_S_MIX", ID_S_MIX),
        ("ID_PASTEL", ID_PASTEL),
        ("ID_PASTEL_V0", ID_PASTEL_V0),
        ("ID_CLOTH_MIX", ID_CLOTH_MIX),
        ("ID_KEEP_FLOOR", ID_KEEP_FLOOR),
        ("ID_HUE_0", ID_HUE_0),
        ("ID_HUE_1", ID_HUE_1),
        ("ID_HUE_2", ID_HUE_2),
        ("ID_HUE_3", ID_HUE_3),
        ("ID_SAT_0", ID_SAT_0),
        ("ID_SAT_1", ID_SAT_1),
        ("ID_SAT_2", ID_SAT_2),
        ("ID_SAT_3", ID_SAT_3),
        ("ID_VAL_0", ID_VAL_0),
        ("ID_VAL_1", ID_VAL_1),
    ];

    pub fn mix(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    pub fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
        [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
    }

    pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn modulo(x: f32, m: f32) -> f32 {
        x - m * (x / m).floor()
    }

    /// (hue in degrees from -180 to 180, 0 = red; saturation; value): the arithmetic the shader's `id_hsv` has, shared with
    /// the mesh's head-colour rule.
    pub(crate) use super::hsv;

    pub fn from_hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
        let h6 = modulo(h, 360.0) / 60.0;
        [5.0f32, 3.0, 1.0].map(|n| {
            let xm = modulo(n + h6, 6.0);
            v - v * s * xm.min(4.0 - xm).clamp(0.0, 1.0)
        })
    }

    /// How much of `c` is skin: peach and tan, moderate saturation, anything above the very dark.
    pub fn skin_mask(c: [f32; 3]) -> f32 {
        let [h, s, v] = hsv(c);
        smoothstep(ID_HUE_0, ID_HUE_1, h)
            * (1.0 - smoothstep(ID_HUE_2, ID_HUE_3, h))
            * smoothstep(ID_SAT_0, ID_SAT_1, s)
            * (1.0 - smoothstep(ID_SAT_2, ID_SAT_3, s))
            * smoothstep(ID_VAL_0, ID_VAL_1, v)
    }

    /// A multiplicative tint with `floor`: each channel of `c` scaled by `floor + (1 - floor) * id`.
    pub fn weak(c: [f32; 3], id: [f32; 3], floor: f32) -> [f32; 3] {
        [0, 1, 2].map(|k| c[k] * (floor + (1.0 - floor) * id[k]))
    }

    /// The OLD look of a placeholder body (`frame::body_tint`'s factor applied to `c`).
    pub fn old(c: [f32; 3], id: [f32; 3]) -> [f32; 3] {
        weak(c, id, ID_GREY_FLOOR)
    }

    /// The look on authored colours: `c` the lit colour, `id` the identity colour, `face` 1 on a face decal.
    pub fn look(c: [f32; 3], id: [f32; 3], strength: f32, face: f32) -> [f32; 3] {
        let [_, s, v] = hsv(c);
        let [hi, si, vi] = hsv(id);
        let skin = skin_mask(c);
        let s_id = si * (1.0 - ID_PASTEL * smoothstep(ID_PASTEL_V0, 1.0, v));
        let s2 = mix(s, s_id, ID_S_MIX);
        let v2 = (ID_V_FLOOR + (1.0 - ID_V_FLOOR) * v.powf(ID_V_GAMMA)) * mix(1.0, vi, ID_V_OF_ID);
        let dyed = from_hsv(hi, s2, v2.min(1.0));
        let chroma = id[0].max(id[1].max(id[2])) - id[0].min(id[1].min(id[2]));
        let cloth = mix3(c, mix3(old(c, id), dyed, smoothstep(ID_GATE_LO, ID_GATE_HI, chroma)), ID_CLOTH_MIX);
        let out = mix3(cloth, weak(c, id, ID_KEEP_FLOOR), skin.max(face));
        mix3(c, out, strength)
    }

    pub fn to_linear(c: [f32; 3]) -> [f32; 3] {
        c.map(|c| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) })
    }

    pub fn to_srgb(c: [f32; 3]) -> [f32; 3] {
        c.map(|c| if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 })
    }

    /// What `fs_model` writes for the colour of a fragment: vertex colour `vertex` and `texel` (authored bytes as
    /// floats, as the shader's inputs are before the sRGB handling), the draw's `identity`, `face` (the overlay
    /// flag) and tint factor, for a target that is (`srgb`) or is not sRGB.
    pub fn fragment(vertex: [f32; 3], texel: [f32; 3], identity: Identity, face: f32, tint: [f32; 3], srgb: bool) -> [f32; 3] {
        // (an sRGB texture is decoded by the sampler)
        let (v, t) = if srgb { (to_linear(vertex), to_linear(texel)) } else { (vertex, texel) };
        let mut lit = [0, 1, 2].map(|k| v[k] * t[k]);
        if identity.strength > 0.0 {
            let c = if srgb { to_srgb(lit) } else { lit };
            let out = look(c, identity.rgb, identity.strength, face);
            lit = if srgb { to_linear(out) } else { out };
        }
        [0, 1, 2].map(|k| lit[k] * tint[k])
    }
}

#[cfg(test)]
mod tests {
    use super::look::*;
    use super::*;
    use crate::avatar::frame::body_tint;
    use crate::avatar::gpu::MODEL_SHADER_WGSL;

    /// The four player colours of the pictures: blue, orange, green, magenta.
    const PLAYERS: [[u8; 3]; 4] = [[70, 130, 245], [240, 120, 40], [60, 200, 90], [210, 60, 200]];

    fn f(c: [u8; 3]) -> [f32; 3] {
        c.map(|c| f32::from(c) / 255.0)
    }

    fn bytes(c: [f32; 3]) -> [u8; 3] {
        c.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
    }

    fn hue_distance(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        255.0 * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn mean_levels(a: [f32; 3], b: [f32; 3]) -> f32 {
        255.0 * (0..3).map(|k| (a[k] - b[k]).abs()).sum::<f32>() / 3.0
    }

    /// Strength 0 is no identity colour: the colour path is `vertex * texel * tint`, bit for bit, whatever the
    /// colour, the face flag, the tint and the kind of target.
    #[test]
    fn a_strength_of_zero_is_exactly_the_old_output() {
        let levels = [0.0f32, 0.07, 0.2, 0.5, 0.83, 1.0];
        let texels = [[1.0f32; 3], [0.3, 0.6, 0.9], [0.0; 3]];
        let tints = [[1.0f32; 3], [0.5, 1.0, 2.0]];
        let mut checked = 0;
        for &r in &levels {
            for &g in &levels {
                for &b in &levels {
                    for texel in texels {
                        for tint in tints {
                            for srgb in [false, true] {
                                for face in [0.0, 1.0] {
                                    let vertex = [r, g, b];
                                    let id = Identity { rgb: [0.3, 0.9, 0.1], strength: 0.0 };
                                    let got = fragment(vertex, texel, id, face, tint, srgb);
                                    let (v, t) = if srgb { (to_linear(vertex), to_linear(texel)) } else { (vertex, texel) };
                                    let want = [0, 1, 2].map(|k| v[k] * t[k] * tint[k]);
                                    assert!(got.iter().zip(&want).all(|(a, b)| a.to_bits() == b.to_bits()), "{vertex:?} {texel:?} {tint:?}: {got:?} vs {want:?}");
                                    assert_eq!(fragment(vertex, texel, Identity::NONE, face, tint, srgb), got);
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 6 * 6 * 6 * 3 * 2 * 2 * 2);
        assert_eq!(Identity::NONE.strength, 0.0);
        assert_eq!(Identity::default(), Identity::NONE);
        assert_eq!(Identity::of([255, 0, 128]).to_array(), [1.0, 0.0, 128.0 / 255.0, 1.0]);
    }

    /// The point of it: near-black cloth, dark grey and the dark blue cloak the owner measured turn plainly into each
    /// player's colour (the hue is the identity's, the colour is a colour and not a grey, and it is lighter than the
    /// cloth it came from), where the old tint left the cloak a dark grey of its own hue. (Pure black is the one
    /// exception to "plainly": see `pure_black_takes_the_hue_but_stays_a_very_dark_shade`.)
    #[test]
    fn dark_cloth_turns_plainly_into_the_players_colour() {
        let cloak = f([57, 66, 109]);
        for rgb in PLAYERS {
            let id = f(rgb);
            let hue = hsv(id)[0];
            for (name, cloth) in [("near black", f([13, 13, 13])), ("dark suit grey", f([48, 48, 52])), ("dark cloak", cloak), ("dark navy", f([20, 20, 70]))] {
                let new = look(cloth, id, 1.0, 0.0);
                let [h, s, v] = hsv(new);
                assert!(hue_distance(h, hue) < 15.0, "{name} in {rgb:?}: hue {h} against {hue}");
                assert!(s > 0.45, "{name} in {rgb:?}: a colour, not a grey ({s})");
                assert!(v > hsv(cloth)[2] + 0.1, "{name} in {rgb:?}: lifted from {} to {v}", hsv(cloth)[2]);
            }
            // The cloak in particular: the old tint is a dark mix of the cloak's blue and the colour, the new one the colour.
            let (old, new) = (old(cloak, id), look(cloak, id, 1.0, 0.0));
            assert!(hue_distance(hsv(new)[0], hue) < hue_distance(hsv(old)[0], hue) || hue_distance(hsv(old)[0], hue) < 15.0, "{rgb:?}: old {old:?} new {new:?}");
            assert!(hsv(new)[1] >= hsv(old)[1] - 0.05, "{rgb:?}: the new colour is not greyer than the old");
        }
        // The cloak the owner measured: [57,66,109] became [55,46,58] under the orange player's old tint; now orange.
        let orange = bytes(look(cloak, f(PLAYERS[1]), 1.0, 0.0));
        assert!(orange[0] > orange[1] + 40 && orange[1] > orange[2], "an orange cloak: {orange:?}");
        assert_eq!(bytes(old(cloak, f(PLAYERS[1]))), [55, 47, 58], "(and the old tint, for reference: the owner measured [55,46,58])");
    }

    /// Pure black (all three channels 0) has no value to lift from: it takes the identity's hue and saturation but
    /// only the floor of the lift (0.2 of a full channel, less the tenth of its own black that is kept), so it is a
    /// very dark shade of the colour and not a plain one. The black of these models is near-black (a vertex colour
    /// of 10 to 30), which is lifted plainly: the test above.
    #[test]
    fn pure_black_takes_the_hue_but_stays_a_very_dark_shade() {
        for rgb in PLAYERS {
            let id = f(rgb);
            let new = look([0.0; 3], id, 1.0, 0.0);
            let [h, s, v] = hsv(new);
            assert!(hue_distance(h, hsv(id)[0]) < 10.0 && s > 0.5, "{rgb:?}: a dark shade of the colour: {new:?}");
            assert!((0.12..0.30).contains(&v), "{rgb:?}: value {v}");
            eprintln!("pure black under {rgb:?}: {:?} (value {v:.2})", bytes(new));
        }
    }

    /// Skin and the face decals stay readable: skin-hued colours (peach, tan, dark brown) change by under 7 levels on
    /// average, a face decal of any colour by under 7 as well, whoever the player is (the weak tint moves a channel
    /// by at most 5%).
    #[test]
    fn skin_and_the_face_decals_change_by_little() {
        let skins = [[230u8, 180, 150], [255, 140, 100], [200, 150, 100], [90, 55, 40], [250, 215, 190]];
        let decals = [[40u8, 30, 30], [240, 240, 240], [200, 60, 60], [60, 90, 160], [230, 180, 150]];
        let mut worst = (0.0f32, 0.0f32);
        for rgb in PLAYERS {
            let id = f(rgb);
            for skin in skins {
                let c = f(skin);
                assert!(skin_mask(c) > 0.99, "{skin:?} is skin");
                let change = mean_levels(c, look(c, id, 1.0, 0.0));
                worst.0 = worst.0.max(change);
                assert!(change < 7.0, "skin {skin:?} under {rgb:?} changes by {change:.1} levels");
            }
            for decal in decals {
                let c = f(decal);
                let change = mean_levels(c, look(c, id, 1.0, 1.0));
                worst.1 = worst.1.max(change);
                assert!(change < 7.0, "face decal {decal:?} under {rgb:?} changes by {change:.1} levels");
                // (and the flag is what keeps it: the same colour as cloth is repainted)
                if skin_mask(c) < 0.5 && decal[0] > 100 {
                    assert!(mean_levels(c, look(c, id, 1.0, 0.0)) > 3.0 * change.max(1.0), "{decal:?}");
                }
            }
        }
        eprintln!("worst mean change: skin {:.1}, face decal {:.1} levels (the old tint: up to {:.0})", worst.0, worst.1, 255.0 * 0.55 * 0.4);
        // The old tint, for contrast, pulled the same skin to brown.
        assert!(mean_levels(f([230, 180, 150]), old(f([230, 180, 150]), f(PLAYERS[0]))) > 25.0);
    }

    /// White and bright cloth take the colour as a pastel of it: tinted, not a saturated block, not blown out.
    #[test]
    fn white_cloth_takes_the_colour_without_blowing_out() {
        for rgb in PLAYERS {
            let id = f(rgb);
            for white in [[235u8, 235, 235], [255, 255, 255], [200, 200, 205]] {
                let new = look(f(white), id, 1.0, 0.0);
                assert!(new.iter().all(|c| (0.0..=1.0).contains(c)), "{white:?} in {rgb:?}: {new:?}");
                let [h, s, v] = hsv(new);
                assert!(hue_distance(h, hsv(id)[0]) < 12.0, "{white:?} in {rgb:?}: hue {h}");
                assert!(s > 0.2 && s < hsv(id)[1], "{white:?} in {rgb:?}: a pastel of the colour ({s} against {})", hsv(id)[1]);
                assert!(v > 0.78 && new.iter().any(|c| *c < 0.97), "{white:?} in {rgb:?}: bright but not white ({new:?})");
            }
        }
    }

    /// Order: a darker cloth is a darker result (the shading of a model survives), and the four players' colours
    /// are plainly four colours for dark cloth.
    #[test]
    fn the_look_keeps_the_models_shading_and_tells_players_apart() {
        for rgb in PLAYERS {
            let id = f(rgb);
            let mut last = -1.0f32;
            for g in 0..=100 {
                let grey = g as f32 / 100.0;
                let v = hsv(look([grey; 3], id, 1.0, 0.0))[2];
                assert!(v >= last - 1e-5, "{rgb:?}: value {v} after {last} at grey {grey}");
                last = v;
            }
            // Distinct shades of cloth stay distinct (a shoe against a suit against a shirt).
            let shades: Vec<f32> = [0.05f32, 0.2, 0.4, 0.7, 0.95].iter().map(|g| hsv(look([*g; 3], id, 1.0, 0.0))[2]).collect();
            assert!(shades.windows(2).all(|w| w[1] > w[0] + 0.04), "{rgb:?}: {shades:?}");
        }
        // (Black stays a very dark shade, so its four colours are the closest of all; the rest are plainly four colours.)
        for (cloth, least) in [(f([57, 66, 109]), 75.0f32), (f([48, 48, 52]), 60.0), (f([30, 70, 190]), 75.0), (f([13, 13, 13]), 40.0), ([0.0; 3], 25.0)] {
            let mut closest = f32::MAX;
            for (i, a) in PLAYERS.iter().enumerate() {
                for b in &PLAYERS[i + 1..] {
                    let d = distance(look(cloth, f(*a), 1.0, 0.0), look(cloth, f(*b), 1.0, 0.0));
                    closest = closest.min(d);
                    assert!(d > least, "{cloth:?}: {a:?} and {b:?} are {d:.0} apart");
                }
            }
            eprintln!("cloth {cloth:?}: the closest two of the four players are {closest:.0} levels apart (of 441)");
        }
        // Strength is a blend between the model's own colour and the full look, in order.
        let (c, id) = (f([57, 66, 109]), f(PLAYERS[1]));
        let steps: Vec<f32> = (0..=4).map(|k| distance(c, look(c, id, k as f32 / 4.0, 0.0))).collect();
        assert!(steps[0] < 1e-3 && steps.windows(2).all(|w| w[1] > w[0]), "{steps:?}");
    }

    /// A white, grey or black identity colour has no hue to paint with (the sliders allow them): white leaves the model
    /// as it is, grey and black darken it without colouring it, as the old tint did, and the paint comes in smoothly as
    /// the colour gets a hue.
    #[test]
    fn grey_identities_keep_the_old_look() {
        let cloths = [f([57, 66, 109]), f([230, 180, 150]), f([48, 48, 52]), f([235, 235, 235]), f([200, 30, 30])];
        for cloth in cloths {
            let white = look(cloth, [1.0; 3], 1.0, 0.0);
            assert!((0..3).all(|k| (white[k] - cloth[k]).abs() < 1e-5), "white leaves {cloth:?} as it is: {white:?}");
            for (grey, darkening) in [(0.5f32, 0.725f32), (0.0, 0.45)] {
                let out = look(cloth, [grey; 3], 1.0, 0.0);
                // The old look's factor, mixed with the model's own colour as the cloth mix says; no hue is added.
                let skin = skin_mask(cloth);
                for k in 0..3 {
                    let cloth_factor = 1.0 + (darkening - 1.0) * ID_CLOTH_MIX;
                    let kept_factor = ID_KEEP_FLOOR + (1.0 - ID_KEEP_FLOOR) * grey;
                    let factor = cloth_factor + (kept_factor - cloth_factor) * skin;
                    assert!((out[k] - cloth[k] * factor).abs() < 1e-4, "grey {grey}: {cloth:?} -> {out:?}");
                }
            }
        }
        // The paint comes in smoothly with the identity's chroma: a nearly grey colour paints only a little.
        let cloth = [0.19f32; 3];
        let mut last = -1.0f32;
        for step in 0..=12 {
            let id = [0.5 + step as f32 * 0.025, 0.5, 0.5];
            let s = hsv(look(cloth, id, 1.0, 0.0))[1];
            assert!(s >= last - 1e-5, "step {step}: saturation {s} after {last}");
            last = s;
        }
        assert!(last > 0.3, "{last}");
    }

    /// An sRGB target shows the picture a plain one does: the look is worked out on the authored colours either way.
    #[test]
    fn an_srgb_target_shows_the_same_picture_as_a_plain_one() {
        let id = Identity::of(PLAYERS[1]);
        let mut worst = 0.0f32;
        for r in [0.0f32, 0.1, 0.4, 0.8, 1.0] {
            for g in [0.0f32, 0.2, 0.5, 0.9] {
                for b in [0.0f32, 0.3, 0.6, 1.0] {
                    for face in [0.0, 1.0] {
                        // Untextured (a white texel): the lit colour is the vertex colour on both.
                        let plain = fragment([r, g, b], [1.0; 3], id, face, [1.0; 3], false);
                        let srgb = to_srgb(fragment([r, g, b], [1.0; 3], id, face, [1.0; 3], true));
                        worst = worst.max(mean_levels(plain, srgb));
                    }
                }
            }
        }
        assert!(worst < 0.05, "{worst} levels apart");
        // Textured: the product of two colours is taken in linear space on an sRGB target (as it always was), so
        // the pictures agree to within a few levels, not exactly.
        let (plain, srgb) = (fragment([0.9, 0.8, 0.7], [0.5, 0.7, 0.9], id, 0.0, [1.0; 3], false), to_srgb(fragment([0.9, 0.8, 0.7], [0.5, 0.7, 0.9], id, 0.0, [1.0; 3], true)));
        assert!(mean_levels(plain, srgb) < 12.0, "{plain:?} {srgb:?}");
        // The conversions are inverses.
        for c in [0.0f32, 0.002, 0.03, 0.2, 0.5, 0.9, 1.0] {
            assert!((to_srgb(to_linear([c; 3]))[0] - c).abs() < 1e-5, "{c}");
        }
    }

    /// The constants exist twice, in the shader and in the mirror: they are the same numbers, and the shader holds no
    /// other.
    #[test]
    fn the_shader_spells_the_constants_the_mirror_uses() {
        for (name, value) in CONSTANTS {
            let line = format!("const {name}: f32 = {value:?};");
            assert!(MODEL_SHADER_WGSL.contains(&line), "the shader lacks `{line}`");
        }
        assert_eq!(MODEL_SHADER_WGSL.matches("const ID_").count(), CONSTANTS.len(), "a constant of the shader the mirror does not know");
        for entry in ["fn id_hsv", "fn id_rgb", "fn identity_look", "fn to_srgb", "identity_look(to_srgb(lit)"] {
            assert!(MODEL_SHADER_WGSL.contains(entry), "{entry}");
        }
        // The old multiplicative look (the floor of `frame::body_tint`) is the fallback of grey identities.
        assert_eq!(ID_GREY_FLOOR, crate::avatar::frame::BODY_TINT_FLOOR);
    }

    /// The reference the pictures draw the old look with is the old function, not a copy of it.
    #[test]
    fn the_mirrors_old_look_is_body_tints_factor() {
        for rgb in [[70u8, 130, 245], [240, 120, 40], [0, 0, 0], [255, 255, 255], [128, 128, 128]] {
            let tint = body_tint(rgb);
            let c = [0.3f32, 0.6, 0.9];
            let got = old(c, f(rgb));
            for k in 0..3 {
                assert!((got[k] - c[k] * tint.rgb[k]).abs() < 1e-6, "{rgb:?}");
            }
            assert_eq!(tint.alpha, 1.0);
        }
    }

    /// Hair keeps its colour and a cap or a hood does not: the colours of the owned bodies' heads (to the unit; the
    /// numbers are the vertex colours measured by `real_looks_survey`), and the ones a person's head can have.
    #[test]
    fn hair_keeps_its_colour_on_the_head_and_a_cap_or_a_hood_does_not() {
        for (name, rgb) in [
            ("lean's dark brown hair (very saturated, very dark: outside the skin mask)", [46u8, 23, 0]),
            ("slight's brown bob", [104, 41, 20]),
            ("black hair", [10, 10, 10]),
            ("dark grey hair or a visor", [48, 48, 52]),
            ("blond hair", [255, 200, 60]),
            ("red hair", [180, 40, 20]),
            ("skin", [255, 131, 80]),
            ("peach skin", [255, 204, 153]),
            ("a red headband", [255, 0, 0]),
        ] {
            assert!(head_colour_is_own(rgb), "{name} {rgb:?} keeps its colour");
        }
        for (name, rgb) in [
            ("slight's cap", [205u8, 211, 211]),
            ("a white cap", [255, 255, 255]),
            ("a light grey cap", [180, 180, 180]),
            ("a mid grey hood", [128, 128, 128]),
            ("sturdy's blue hood", [27, 62, 159]),
            ("a dark navy hood", [20, 20, 70]),
            ("a green hood", [20, 160, 60]),
            ("a violet helmet", [140, 60, 200]),
        ] {
            assert!(!head_colour_is_own(rgb), "{name} {rgb:?} is cloth: the player's colour goes on it");
        }
        // What the rule keeps is kept by the shader's weak tint (a decal's), whoever the player is: it moves a channel by
        // at most 5%.
        for player in PLAYERS {
            for rgb in [[46u8, 23, 0], [104, 41, 20], [10, 10, 10], [255, 200, 60]] {
                let (c, id) = (f(rgb), f(player));
                assert!(mean_levels(c, look(c, id, 1.0, 1.0)) < 7.0, "{rgb:?} under {player:?}");
            }
        }
    }

    /// The shared HSV is the arithmetic the CPU mirror always had (a few colours worked out by hand).
    #[test]
    fn the_shared_hsv_is_the_mirrors() {
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-4);
        assert!(close(hsv([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0]));
        assert!(close(hsv([0.0, 1.0, 0.0]), [120.0, 1.0, 1.0]));
        assert!(close(hsv([0.0, 0.0, 1.0]), [-120.0, 1.0, 1.0]));
        assert!(close(hsv([0.5, 0.5, 0.5]), [0.0, 0.0, 0.5]));
        assert!(close(hsv([0.0; 3]), [0.0, 0.0, 0.0]));
        assert!(close(hsv([1.0, 0.5, 0.0]), [30.0, 1.0, 1.0]));
    }
}

#[cfg(test)]
pub(crate) mod pics {
    //! Small picture helpers of the tests that look at the look.

    /// An RGBA image.
    pub struct Img {
        pub w: u32,
        pub h: u32,
        pub px: Vec<u8>,
    }

    const GAP: [u8; 4] = [24, 24, 24, 255];

    pub fn hstack(images: &[Img]) -> Img {
        let h = images.iter().map(|i| i.h).max().unwrap_or(0);
        let w: u32 = images.iter().map(|i| i.w).sum();
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&GAP);
        }
        let mut x0 = 0;
        for img in images {
            for y in 0..img.h {
                let from = (y * img.w * 4) as usize;
                let to = ((y * w + x0) * 4) as usize;
                px[to..to + (img.w * 4) as usize].copy_from_slice(&img.px[from..from + (img.w * 4) as usize]);
            }
            x0 += img.w;
        }
        Img { w, h, px }
    }

    pub fn vstack(images: &[Img]) -> Img {
        let w = images.iter().map(|i| i.w).max().unwrap_or(0);
        let h: u32 = images.iter().map(|i| i.h).sum();
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&GAP);
        }
        let mut y0 = 0;
        for img in images {
            for y in 0..img.h {
                let from = (y * img.w * 4) as usize;
                let to = ((y0 + y) * w * 4) as usize;
                px[to..to + (img.w * 4) as usize].copy_from_slice(&img.px[from..from + (img.w * 4) as usize]);
            }
            y0 += img.h;
        }
        Img { w, h, px }
    }

    /// The `cw` x `ch` rectangle of `img` (`w` wide) at (`x0`, `y0`), scaled up `zoom` times (nearest neighbour).
    pub fn crop(img: &[u8], w: u32, (x0, y0): (u32, u32), (cw, ch): (u32, u32), zoom: u32) -> Img {
        let mut px = Vec::with_capacity((cw * ch * zoom * zoom * 4) as usize);
        for y in 0..ch * zoom {
            for x in 0..cw * zoom {
                let at = (((y0 + y / zoom) * w + x0 + x / zoom) * 4) as usize;
                px.extend_from_slice(&img[at..at + 4]);
            }
        }
        Img { w: cw * zoom, h: ch * zoom, px }
    }

    /// A square crop of a `w` x `h` image around the pixel box `(x0, y0, x1, y1)`, `pad` pixels beyond it, scaled up
    /// `zoom` times.
    pub fn around(img: &[u8], w: u32, h: u32, (x0, y0, x1, y1): (f32, f32, f32, f32), pad: f32, zoom: u32) -> Img {
        let (cw, ch) = (((x1 - x0) + 2.0 * pad).max(48.0) as u32, ((y1 - y0) + 2.0 * pad).max(48.0) as u32);
        let side = cw.max(ch).min(w).min(h);
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let ox = (cx as i64 - i64::from(side) / 2).clamp(0, i64::from(w - side)) as u32;
        let oy = (cy as i64 - i64::from(side) / 2).clamp(0, i64::from(h - side)) as u32;
        crop(img, w, (ox, oy), (side, side), zoom)
    }
}

#[cfg(test)]
pub(crate) mod measure {
    //! Aggregates of a mesh's colours (no pixel of the game ever leaves a test): how much of it is flagged an
    //! overlay, textured or skin-coloured.
    use super::look;
    use crate::avatar::mesh::MergedMesh;

    #[derive(Debug, Default, Clone, Copy)]
    pub struct Shares {
        pub triangles: usize,
        pub overlay: f32,
        /// The share of the head's hair and skin: triangles whose first vertex is flagged `own` (see `MergedMesh::own`).
        pub own: f32,
        pub textured: f32,
        pub skin: f32,
        /// Colours whose brightest channel is under 8 (of 255): the black the look cannot lift plainly.
        pub black: f32,
    }

    /// Area-weighted shares (a triangle's area does not change under the rigid bone transforms).
    pub fn shares(mesh: &MergedMesh) -> Shares {
        let mut out = Shares::default();
        let mut total = 0.0f32;
        for batch in &mesh.batches {
            let lo = batch.first_index as usize;
            let hi = lo + batch.index_count as usize;
            for tri in mesh.indices[lo..hi].chunks_exact(3) {
                let p: Vec<[f32; 3]> = tri.iter().map(|&i| mesh.positions[i as usize]).collect();
                let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
                let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
                let cross = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let area = 0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
                // The colour: the mean vertex colour times the texel at the mean uv.
                let vertex = |k: usize| tri.iter().map(|&i| f32::from(mesh.colors[i as usize][k]) / 255.0).sum::<f32>() / 3.0;
                let mut texel = [1.0f32; 3];
                let mut alpha = 1.0;
                if let Some(img) = &batch.texture {
                    let uv: [f32; 2] = [0, 1].map(|k| tri.iter().map(|&i| mesh.uvs[i as usize][k]).sum::<f32>() / 3.0);
                    let x = ((uv[0] * img.width as f32).floor().max(0.0) as u32).min(img.width.saturating_sub(1));
                    let y = ((uv[1] * img.height as f32).floor().max(0.0) as u32).min(img.height.saturating_sub(1));
                    let at = ((y * img.width + x) * 4) as usize;
                    if let Some(px) = img.rgba.get(at..at + 4) {
                        texel = [f32::from(px[0]) / 255.0, f32::from(px[1]) / 255.0, f32::from(px[2]) / 255.0];
                        alpha = f32::from(px[3]) / 255.0;
                    }
                }
                if alpha < 0.5 || !area.is_finite() {
                    continue;
                }
                let c = [vertex(0) * texel[0], vertex(1) * texel[1], vertex(2) * texel[2]];
                out.triangles += 1;
                total += area;
                out.overlay += area * mesh.overlay[tri[0] as usize];
                out.own += area * mesh.own[tri[0] as usize];
                out.textured += area * f32::from(u8::from(batch.texture.is_some()));
                out.skin += area * look::skin_mask(c);
                out.black += area * f32::from(u8::from(c[0].max(c[1]).max(c[2]) < 8.0 / 255.0));
            }
        }
        if total > 0.0 {
            out.overlay /= total;
            out.own /= total;
            out.textured /= total;
            out.skin /= total;
            out.black /= total;
        }
        out
    }
}
