//! Composite a PC field background into one RGBA image, and map between field (walkmesh)
//! coordinates and background pixels.
//!
//! This module turns the raw tile/texture/palette data that [`crate::field`] parses into
//! pixels. It is the "renderable background" API: call [`decode_background`] once per field and
//! upload the resulting [`BackgroundImage`] as one texture, then place the walkmesh and the
//! characters with [`Projection`].
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08** (other editions unverified):
//!
//! * `md1stin` composites to a 512x512 image with origin (256, 256); all 1 115 tiles (795 in
//!   layer 1, 320 in layer 2) are drawn and the picture is the Sector 1 station platform (train
//!   on the right, yellow safety line, lamp and structure in the distance). Projecting the
//!   walkmesh through camera record 0 outlines the platform floor on that picture; the same
//!   overlay lands the radial platforms of `kuro_4` (camera looking straight down) and the
//!   pedestal tops of `ancnt1` on their art. Of 244 626 walkmesh vertices in 702 fields, 216 875
//!   (88.7%) project inside the field's camera range (115 of the 117 vertex references of
//!   `md1stin`; the other two, one corner, land 19 units past the left edge); the misses are
//!   large unused or off-screen mesh areas (for example `nvmkin23`'s +-1500 unit mesh against a
//!   single 320x240 screen), not a systematic offset.
//! * Layer order: in `bwhlin` (the Gold Saucer gondola) the scenery of layer 3 shows through the
//!   window holes of layer 1 only when layer 3 is drawn first, so layer 3 is behind layer 1.
//! * Transparency: only palette index 0 can be transparent, and only when its page's flag
//!   in [`crate::field::Background::palette_transparency`] enables it; direct-colour pages decode correctly
//!   (the Gold Saucer map of `gldinfo`).
//! * Blending: all four `type_trans` values occur (additive is by far the most common, 93 000
//!   layer-2 tiles); the glow, glass and fog layers of `ancnt2`, `woa_1` and `cosmin2` look
//!   right with the modes below. The mapping of `type_trans` 0-3 to modes follows the
//!   PlayStation convention and was not checked independently.
//! * Across the 702 field entries every tile of 700 fields decodes; the 312 tiles of `lastmap`
//!   and `fr_e` that name a palette page past the palette section are skipped
//!   ([`LayerInfo::tiles_unrenderable`]).
//! * Depth (see [`depth`]): a layer-2 tile's depth in [`Projection`] units is `4 * id`. The PC-only
//!   `IDBig` of the tiles is the Direct3D z-buffer value of that depth with the engine's near plane 50
//!   and far plane 32 000 in 394 of 451 fitted fields, and the floor at the foot of 4 015 layer-2
//!   objects lies at 1.01 (median; 0.98 to 1.06 for the middle half) times `4 * id` from the camera.
//!   Painting [`decode_background_tiles_with_depth`]'s tiles in order reproduces
//!   [`decode_background`]'s picture bit for bit in all 702 fields.
//!
//! # Sources
//!
//! * FFRTT wiki "FF7/Field/Background" <https://wiki.ffrtt.ru/index.php/FF7/Field/Background>
//!   and "FF7/Field/Palette" <https://wiki.ffrtt.ru/index.php/FF7/Field/Palette> (the
//!   Fergusson write-ups: 52-byte tile records, 256x256 8-bit pages, 15-bit colours).
//! * Makou Reactor `src/core/field/BackgroundTilesIO.cpp`, `BackgroundTexturesIO.cpp` and
//!   `BackgroundTextures.cpp` <https://github.com/myst6re/makoureactor> (layer ids and draw
//!   order, tile size per layer, which source set a blended tile uses, page depth sizes, "PC
//!   palettes can only make colour 0 transparent"). Only layout facts were taken.
//! * Camera projection: the PlayStation GTE model (rotate, translate, divide by depth) that the
//!   PC field renderer inherited, confirmed against the real art as described above.
//!
//! # Coordinate systems
//!
//! * **Field (world) space**: walkmesh vertices, gateway exit lines and trigger corners, `i16`
//!   `(x, y, z)`. `x`/`y` form the ground plane and `z` is height (the camera's down axis is
//!   `-z`; `md1stin` spans 122 units in `z` against 565 by 2161 in `x`/`y`).
//! * **Camera space**: `cam = R * world + T`, `R`'s rows the three camera axis vectors (`i16`
//!   fixed point, divide by 4096), `T` the camera `position` triple, all from camera record 0
//!   (further records are copies in 214 of 215 multi-camera fields). `cam.z` is the distance in
//!   front of the camera, positive when visible.
//! * **Field screen space** (also the space of tile destinations): `sx = cam.x * zoom / cam.z`,
//!   `sy = cam.y * zoom / cam.z`, `zoom` being the camera record's last field. The origin is the
//!   centre of the logical 320x240 screen when the view is not scrolled; **x grows to the right
//!   and y grows downwards**. A tile's `dst_x`/`dst_y` is its *top-left* corner in this space,
//!   which is why a projected walkmesh lines up with the art.
//! * **Image pixels**: [`BackgroundImage::rgba`] covers the bounding box of every kept tile
//!   (whatever the options). Field-screen point `(sx, sy)` is pixel `(sx + origin_x, sy +
//!   origin_y)`; use [`BackgroundImage::screen_to_pixel`] and [`Projection::project`].
//! * **Scrolling**: the visible window is 320x240 field-screen units (the PC game doubles the
//!   size when presenting). It follows the player and is limited to the field's *camera range*
//!   ([`BackgroundImage::view_bounds`], from the triggers section). That range is not the image:
//!   layers 3-4 may extend past it (`bwhlin` is 576x416 against a 320x240 range) and a sparse
//!   layer 1 may stop short of it. Use [`BackgroundImage::clamp_view_centre`].
//!
//! # Layers, draw order, transparency, blending
//!
//! * Tile size is 16 for layers 1-2 and 32 for layers 3-4.
//! * Back-to-front order follows Makou Reactor's tile ids: layer 3 (id 4096), layer 1 (4095),
//!   layer 2 (its own `id`, larger = further away), layer 4 (id 0); ties keep file order.
//! * Palette index 0 is transparent when its page's background-header flag is set; all
//!   entries are opaque when that flag is clear. For 16-bit direct colour a raw
//!   `0x0000` is transparent.
//! * Blended tiles (`blending != 0`, layers 2-4 only; layer 1 ignores the field) combine with
//!   what is behind them using the PlayStation semi-transparency mode chosen by `type_trans`:
//!   0 average, 1 additive, 2 subtractive, 3 add a quarter. Nothing behind counts as black.
//! * Animation groups: layer 2-4 tiles with `param != 0` belong to a script-controlled group
//!   (`BGON`/`BGOFF`); their `state` is a mask of the frames that show them (usually one bit).
//!   [`BackgroundState`] applies resolved instructions and [`GroupFilter::State`] selects tiles by
//!   mask intersection. Which groups are on after initialization depends on script execution, so the default
//!   [`GroupFilter::Static`] draws only `param == 0` tiles (a coherent base scene). 19% of the
//!   owned tiles are in groups. [`LayerInfo::groups`] lists what is there; pick frames with
//!   [`GroupFilter::Only`].
//!
//! # Drawing characters in front of and behind the background
//!
//! One flattened image puts everything behind the characters. For the common split use
//! [`decode_background_layers`]: it returns the **back** half (layers 1 and 3, [`BACK_LAYERS`])
//! and the **front** half (layers 2 and 4, [`FRONT_LAYERS`]) as two images with the same size and
//! origin as [`decode_background`] (the canvas does not depend on which layers are drawn), and the
//! characters go between them:
//!
//! ```no_run
//! # use fmv_ff7::background::decode_background_layers;
//! # fn demo(field: &fmv_ff7::field::FieldFile) -> Result<(), fmv_ff7::field::FieldError> {
//! let layers = decode_background_layers(field)?;
//! // draw `layers.back`, then the characters, then `layers.front` (alpha-over)
//! # let _ = (layers.back, layers.front); Ok(()) }
//! ```
//!
//! Caveats: blended tiles in the front-only image blend against black (additive tiles are fine,
//! average and subtractive ones come out wrong); the front half is one flat image, whereas the game
//! depth-sorts layer-2 tiles against each character. In a field such as `md1stin`, whose layer 2 is
//! large, a character standing behind a layer-2 object is covered by it in this split exactly as in
//! the game, but a character the game would draw in front of a layer-2 tile is covered here too.
//!
//! **For the game's own layering use per-tile depth** ([`depth`]): a layer-2 tile's depth is `4 *
//! id` in the camera-space units of [`Projection::project`]'s `depth` (verified on all 702 owned
//! fields, see that module), and a character is hidden by the tiles it overlaps that are nearer
//! than it. [`decode_background_tiles_with_depth`] returns every tile as a quad with its depth,
//! blend mode, pixels and rectangle, back to front, for a renderer that draws the tiles in the
//! same depth-tested pass as the characters (layer 1 and 3 at their far depths, layer 4 at 0);
//! [`depth_of_point`] gives a world point's depth in the same units and [`depth::pack_atlas`] puts
//! the tiles' pixels in one texture.
//!
//! **Never show the back half on its own**: it is not a finished picture. Under the layer-2
//! objects of `md1stin` the back half holds 6 277 opaque pure-green `(0, 255, 0)` pixels (a key
//! colour, the same one several palettes use at index 0), all of which the front half covers: drawing
//! the front over the back reproduces [`decode_background`]'s picture exactly there (no blended
//! tiles) and has none of that green. Checked on the owned data for all 702 fields: both halves have
//! the canvas of [`decode_background`] (`depth_layers_of_real_fields` in `tests/real_data.rs`), and
//! 642 of them have both back and front layers.
//!
//! # Safety against untrusted input
//!
//! Tile destinations are limited to `|dst| < MAX_TILE_DST` by the field parser, so the canvas is
//! at most about 2080x2080 pixels (17 MB); [`MAX_CANVAS_SIDE`] enforces it again here. All pixel
//! access is bounds-checked and no input value is used as an unchecked index.

use crate::field::{Background, Camera, FieldError, FieldFile, Palette, TileSprite, MAX_TILE_DST};

pub mod depth;
pub mod state;
pub mod palette;
mod entry;
#[cfg(test)]
mod palette_real_tests;
mod projection_depth;

pub use depth::{
    decode_background_tiles_with_depth, decode_background_tiles_with_depth_options, decode_background_tiles_with_depth_palette, depth_of_id, depth_of_point,
    tile_depth, tile_hides_point, DepthTile, TILE_DEPTH_PER_ID,
};
pub use state::{BackgroundOp, BackgroundState};
pub use palette::{PaletteOp, PaletteState};
pub use entry::entry_background_ops;
pub use projection_depth::{calibrate as calibrate_depth_mapping, DepthMapping};

/// Logical width of the field screen in field-screen units.
pub const SCREEN_WIDTH: i32 = 320;
/// Logical height of the field screen in field-screen units.
pub const SCREEN_HEIGHT: i32 = 240;
/// Largest canvas edge this module will allocate: the widest span the parser can produce
/// (`2 * MAX_TILE_DST` plus one 32-pixel tile).
pub const MAX_CANVAS_SIDE: i32 = 2 * MAX_TILE_DST as i32 + 32;

/// PlayStation semi-transparency mode selected by a blended tile's `type_trans`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// `(back + front) / 2`.
    Average,
    /// `back + front`.
    Additive,
    /// `back - front`.
    Subtractive,
    /// `back + front / 4`.
    AddQuarter,
}

impl BlendMode {
    /// Mode for a raw `type_trans` byte (only the low two bits are meaningful).
    pub fn from_type_trans(type_trans: u8) -> Self {
        match type_trans & 3 {
            0 => Self::Average,
            1 => Self::Additive,
            2 => Self::Subtractive,
            _ => Self::AddQuarter,
        }
    }

    /// Combine one 8-bit channel.
    pub fn apply(self, back: u8, front: u8) -> u8 {
        let (b, f) = (u16::from(back), u16::from(front));
        let v = match self {
            Self::Average => (b + f) / 2,
            Self::Additive => (b + f).min(255),
            Self::Subtractive => b.saturating_sub(f),
            Self::AddQuarter => (b + f / 4).min(255),
        };
        u8::try_from(v).unwrap_or(u8::MAX)
    }
}

/// Which animation groups (`param`/`state` of layers 2-4) are drawn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GroupFilter {
    /// Only tiles with `param == 0` (always visible). This is the default.
    #[default]
    Static,
    /// Every tile, whatever its group (draws all animation frames on top of each other; for
    /// debugging).
    All,
    /// `param == 0` tiles plus tiles whose `(param, state)` is listed.
    Only(Vec<(u8, u8)>),
    /// Tiles whose state intersects their group's enabled bits, plus unconditional tiles.
    State(BackgroundState),
}

impl GroupFilter {
    fn allows(&self, tile: &TileSprite) -> bool {
        if tile.layer == 1 || tile.param == 0 {
            return true; // layer 1 ignores param/state (Makou Reactor forces them to 0)
        }
        match self {
            Self::Static => false,
            Self::All => true,
            Self::Only(list) => list.contains(&(tile.param, tile.state)),
            Self::State(state) => state.visible(tile.layer, tile.param, tile.state),
        }
    }
}

/// What [`decode_background_with`] draws and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundOptions {
    /// Draw layers 1-4 (index 0 is layer 1).
    pub layers: [bool; 4],
    /// Animation-group filter for layers 2-4.
    pub groups: GroupFilter,
    /// Apply [`BlendMode`]s to blended tiles; if false they are drawn opaque.
    pub blending: bool,
}

impl Default for BackgroundOptions {
    fn default() -> Self {
        Self { layers: [true; 4], groups: GroupFilter::Static, blending: true }
    }
}

/// Axis-aligned rectangle in field-screen units; `max_*` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl ScreenRect {
    pub fn width(&self) -> i32 {
        self.max_x - self.min_x
    }
    pub fn height(&self) -> i32 {
        self.max_y - self.min_y
    }
    fn union(self, o: Self) -> Self {
        Self {
            min_x: self.min_x.min(o.min_x),
            min_y: self.min_y.min(o.min_y),
            max_x: self.max_x.max(o.max_x),
            max_y: self.max_y.max(o.max_y),
        }
    }
    fn of_tile(tile: &TileSprite) -> Self {
        let size = i32::from(tile.tile_size);
        Self {
            min_x: i32::from(tile.dst_x),
            min_y: i32::from(tile.dst_y),
            max_x: i32::from(tile.dst_x) + size,
            max_y: i32::from(tile.dst_y) + size,
        }
    }
}

/// One animation group seen in a layer: tiles sharing `param`, with the union of their `state`
/// bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupInfo {
    pub param: u8,
    /// OR of the `state` bytes of the group's tiles (one bit per animation frame).
    pub state_mask: u8,
    pub tiles: usize,
}

/// Summary of one background layer after decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    /// 1..=4.
    pub layer: u8,
    /// 16 (layers 1-2) or 32 (layers 3-4).
    pub tile_size: u8,
    /// `width`/`height` stored in the layer header (informational; layer 1's is the camera range
    /// plus 16 in most fields).
    pub header_width: u16,
    pub header_height: u16,
    /// Tiles of this layer that passed the parser's destination rule.
    pub tiles_total: usize,
    /// Tiles actually composited.
    pub tiles_drawn: usize,
    /// Of the drawn tiles, how many were blended.
    pub tiles_blended: usize,
    /// Tiles left out by [`BackgroundOptions::groups`] or a disabled layer.
    pub tiles_hidden: usize,
    /// Tiles left out because their texture page, palette page or source rectangle is missing
    /// or out of range.
    pub tiles_unrenderable: usize,
    /// Destination extent of the layer's kept tiles, if it has any.
    pub bounds: Option<ScreenRect>,
    /// Animation groups present (layers 2-4 only), ordered by `param`.
    pub groups: Vec<GroupInfo>,
}

/// The composited field background.
///
/// `rgba` is `width * height * 4` bytes, row-major, top row first, straight (non-premultiplied)
/// alpha. Pixels no drawn tile touched have alpha 0 (draw over black, which is what the game
/// shows there). Field-screen point `(sx, sy)` is pixel `(sx + origin_x, sy + origin_y)`.
#[derive(Clone, PartialEq, Eq)]
pub struct BackgroundImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Pixel column of field-screen x = 0.
    pub origin_x: i32,
    /// Pixel row of field-screen y = 0.
    pub origin_y: i32,
    /// The area the 320x240 view may scroll over, in field-screen units: the field's camera
    /// range when the triggers section parses, else the layer-1 tile bounds, else the image
    /// bounds. Not the same as the image bounds (see the module docs).
    pub view_bounds: ScreenRect,
    /// Layers 1-4 that exist in the field (absent layers are not listed).
    pub layers: Vec<LayerInfo>,
}

/// `Debug` prints the geometry and layer summaries only, never pixels.
impl std::fmt::Debug for BackgroundImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("origin", &(self.origin_x, self.origin_y))
            .field("view_bounds", &self.view_bounds)
            .field("layers", &self.layers)
            .finish_non_exhaustive()
    }
}

impl BackgroundImage {
    /// Extent of the image in field-screen units.
    pub fn bounds(&self) -> ScreenRect {
        ScreenRect {
            min_x: -self.origin_x,
            min_y: -self.origin_y,
            max_x: i32::try_from(self.width).unwrap_or(i32::MAX).saturating_sub(self.origin_x),
            max_y: i32::try_from(self.height).unwrap_or(i32::MAX).saturating_sub(self.origin_y),
        }
    }

    /// Image pixel (fractional) for a field-screen point.
    pub fn screen_to_pixel(&self, sx: f32, sy: f32) -> (f32, f32) {
        (sx + self.origin_x as f32, sy + self.origin_y as f32)
    }

    /// Field-screen point for an image pixel.
    pub fn pixel_to_screen(&self, px: f32, py: f32) -> (f32, f32) {
        (px - self.origin_x as f32, py - self.origin_y as f32)
    }

    /// RGBA of pixel `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(y).ok()?.checked_mul(usize::try_from(self.width).ok()?)?.checked_add(usize::try_from(x).ok()?)?;
        let at = index.checked_mul(4)?;
        self.rgba.get(at..at.checked_add(4)?)?.try_into().ok()
    }

    /// Clamp the centre of the 320x240 view window (field-screen units) so the window stays
    /// inside [`BackgroundImage::view_bounds`]; on an axis where the bounds are no larger than the
    /// window, the window is centred on the bounds. This is how the game limits scrolling to the
    /// field's camera range (not run against the game; the 176 owned fields whose range is exactly
    /// 320x240 do not scroll, which this reproduces).
    pub fn clamp_view_centre(&self, cx: f32, cy: f32) -> (f32, f32) {
        let b = self.view_bounds;
        let clamp_axis = |c: f32, lo: i32, hi: i32, view: i32| -> f32 {
            if hi - lo <= view {
                (lo + hi) as f32 / 2.0
            } else {
                c.clamp((lo + view / 2) as f32, (hi - view / 2) as f32)
            }
        };
        (clamp_axis(cx, b.min_x, b.max_x, SCREEN_WIDTH), clamp_axis(cy, b.min_y, b.max_y, SCREEN_HEIGHT))
    }

    /// Copy of the pixels composited over opaque black (what the game displays), RGBA with alpha
    /// 255 everywhere. Handy for viewers that cannot show transparency.
    pub fn flatten_on_black(&self) -> Vec<u8> {
        let mut out = self.rgba.clone();
        // (`as_chunks_mut` would satisfy the lint but needs Rust 1.88; see `lgp` for why the MSRV stays put)
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for px in out.chunks_exact_mut(4) {
            if let [r, g, b, a] = px {
                let k = u16::from(*a);
                *r = u8::try_from(u16::from(*r) * k / 255).unwrap_or(u8::MAX);
                *g = u8::try_from(u16::from(*g) * k / 255).unwrap_or(u8::MAX);
                *b = u8::try_from(u16::from(*b) * k / 255).unwrap_or(u8::MAX);
                *a = 255;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// Camera projection
// ---------------------------------------------------------------------------------------

/// A field-screen position with its camera-space depth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoint {
    /// Field-screen x (right is positive); add [`BackgroundImage::origin_x`] for the pixel.
    pub x: f32,
    /// Field-screen y (down is positive).
    pub y: f32,
    /// Distance in front of the camera (`cam.z`); only positive points are visible.
    pub depth: f32,
}

/// The field camera as a projection from field (walkmesh) coordinates to field-screen space.
///
/// ```text
/// cam = R * world + T            R rows = camera.axes / 4096, T = camera.position
/// sx  = cam.x * zoom / cam.z     sy = cam.y * zoom / cam.z
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    rows: [[f64; 3]; 3],
    translation: [f64; 3],
    zoom: f64,
}

impl Projection {
    /// Build the projection of one camera record (use record 0 of the field).
    pub fn from_camera(camera: &Camera) -> Self {
        let scale = f64::from(crate::field::CAMERA_FIXED_POINT);
        Self {
            rows: camera.axes.map(|row| row.map(|c| f64::from(c) / scale)),
            translation: camera.position.map(f64::from),
            zoom: f64::from(camera.zoom),
        }
    }

    fn camera_space(&self, world: [f32; 3]) -> [f64; 3] {
        let w = world.map(f64::from);
        let mut cam = [0.0f64; 3];
        for ((slot, row), t) in cam.iter_mut().zip(self.rows.iter()).zip(self.translation.iter()) {
            *slot = row.iter().zip(w.iter()).map(|(a, b)| a * b).sum::<f64>() + t;
        }
        cam
    }

    /// World point in camera space.
    pub fn to_camera(&self, world: [f32; 3]) -> [f32; 3] {
        self.camera_space(world).map(|c| c as f32)
    }

    /// Project a world point to field-screen space. `None` if it is at or behind the camera
    /// plane (depth below 1 unit), where the perspective divide is meaningless.
    pub fn project(&self, world: [f32; 3]) -> Option<ScreenPoint> {
        let [cx, cy, cz] = self.camera_space(world);
        if cz < 1.0 {
            return None;
        }
        Some(ScreenPoint { x: (cx * self.zoom / cz) as f32, y: (cy * self.zoom / cz) as f32, depth: cz as f32 })
    }

    /// Project an `i16` walkmesh/gateway vertex.
    pub fn project_i16(&self, v: [i16; 3]) -> Option<ScreenPoint> {
        self.project(v.map(f32::from))
    }
}

// ---------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------

/// Decode the field's background with the default options (all layers, static groups, blending
/// on). See the module docs for the coordinate mapping and for how to split the result into a
/// part behind and a part in front of the characters.
pub fn decode_background(field: &FieldFile) -> Result<BackgroundImage, FieldError> {
    decode_background_with(field, &BackgroundOptions::default())
}

/// Decode the field's background with explicit options.
pub fn decode_background_with(field: &FieldFile, options: &BackgroundOptions) -> Result<BackgroundImage, FieldError> {
    let palette = field.palette()?;
    decode_background_with_palette(field, &palette, options)
}

/// Decode with the field's current script-controlled palette. Texture indices, transparency,
/// blending, tile selection and camera bounds follow the ordinary decoder.
pub fn decode_background_with_palette(field: &FieldFile, palette: &Palette, options: &BackgroundOptions) -> Result<BackgroundImage, FieldError> {
    let background = field.background()?;
    compose(&background, palette, options, camera_range_view(field))
}

/// The camera range of the field's triggers section as a [`ScreenRect`], if that section parses.
fn camera_range_view(field: &FieldFile) -> Option<ScreenRect> {
    field.triggers().ok().map(|t| {
        let (min_x, min_y, max_x, max_y) = t.camera_range.bounds();
        ScreenRect { min_x, min_y, max_x, max_y }
    })
}

/// The layers drawn **behind** the characters (layers 1 and 3, indices 0 and 2 of
/// [`BackgroundOptions::layers`]).
pub const BACK_LAYERS: [bool; 4] = [true, false, true, false];

/// The layers drawn **in front of** the characters (layers 2 and 4).
pub const FRONT_LAYERS: [bool; 4] = [false, true, false, true];

/// The background split for drawing characters between its two halves: draw
/// [`DepthLayers::back`], then the characters, then [`DepthLayers::front`] (alpha-over; pixels no
/// drawn tile touched have alpha 0). Both images have the same `width`, `height`, `origin_x`,
/// `origin_y` and `view_bounds` as [`decode_background`] returns for the same field, so a point
/// projects to the same pixel in all three ([`Projection::project`],
/// [`BackgroundImage::screen_to_pixel`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthLayers {
    /// Layers 1 and 3 ([`BACK_LAYERS`]).
    pub back: BackgroundImage,
    /// Layers 2 and 4 ([`FRONT_LAYERS`]).
    pub front: BackgroundImage,
}

/// Decode the background as [`DepthLayers`] with the default options (static animation groups,
/// blending on). See the module docs ("Drawing characters in front of and behind the
/// background") for what the split does and does not model.
pub fn decode_background_layers(field: &FieldFile) -> Result<DepthLayers, FieldError> {
    decode_background_layers_with(field, &BackgroundOptions::default())
}

/// [`decode_background_layers`] with explicit options. `options.layers` is ignored (the two
/// halves are [`BACK_LAYERS`] and [`FRONT_LAYERS`]); `groups` and `blending` apply to both. The
/// field's background and palette are parsed once.
pub fn decode_background_layers_with(field: &FieldFile, options: &BackgroundOptions) -> Result<DepthLayers, FieldError> {
    let background = field.background()?;
    let palette = field.palette()?;
    let view = camera_range_view(field);
    let back = compose(&background, &palette, &BackgroundOptions { layers: BACK_LAYERS, ..options.clone() }, view)?;
    let front = compose(&background, &palette, &BackgroundOptions { layers: FRONT_LAYERS, ..options.clone() }, view)?;
    Ok(DepthLayers { back, front })
}

/// Same as [`decode_background_with`] for an already parsed background and palette. `view` is
/// the camera range to report as [`BackgroundImage::view_bounds`] (`None`: the layer-1 tile
/// bounds, else the image bounds).
pub fn compose(
    background: &Background<'_>,
    palette: &Palette,
    options: &BackgroundOptions,
    view: Option<ScreenRect>,
) -> Result<BackgroundImage, FieldError> {
    if let Some(err) = &background.texture_error {
        return Err(err.clone());
    }

    // Canvas = bounding box of every kept tile of every layer (independent of the options, so
    // the origin stays the same whatever is switched on).
    let mut bounds: Option<ScreenRect> = None;
    let mut layer1_bounds: Option<ScreenRect> = None;
    for tile in &background.tiles {
        let r = ScreenRect::of_tile(tile);
        bounds = Some(bounds.map_or(r, |b| b.union(r)));
        if tile.layer == 1 {
            layer1_bounds = Some(layer1_bounds.map_or(r, |b| b.union(r)));
        }
    }
    let bounds = bounds.unwrap_or(ScreenRect { min_x: 0, min_y: 0, max_x: 0, max_y: 0 });
    if bounds.width() > MAX_CANVAS_SIDE || bounds.height() > MAX_CANVAS_SIDE {
        return Err(FieldError::Invalid {
            what: "background",
            why: format!("canvas {}x{} exceeds the limit of {MAX_CANVAS_SIDE}", bounds.width(), bounds.height()),
        });
    }
    let width = usize::try_from(bounds.width()).unwrap_or(0);
    let height = usize::try_from(bounds.height()).unwrap_or(0);
    let mut canvas = Canvas {
        width,
        height,
        origin_x: -bounds.min_x,
        origin_y: -bounds.min_y,
        rgba: vec![0u8; width * height * 4], // <= 2078 * 2078 * 4 bytes
    };

    let mut infos: Vec<LayerInfo> = Vec::new();
    for (i, header) in background.layers.iter().enumerate() {
        let (Some(header), Ok(layer)) = (header, u8::try_from(i + 1)) else { continue };
        infos.push(LayerInfo {
            layer,
            tile_size: if layer > 2 { 32 } else { 16 },
            header_width: header.width,
            header_height: header.height,
            tiles_total: 0,
            tiles_drawn: 0,
            tiles_blended: 0,
            tiles_hidden: 0,
            tiles_unrenderable: 0,
            bounds: None,
            groups: Vec::new(),
        });
    }

    // Back to front: larger draw depth first; the sort is stable so ties keep file order.
    let mut order: Vec<&TileSprite> = background.tiles.iter().collect();
    order.sort_by_key(|t| std::cmp::Reverse(draw_depth(t)));

    for tile in order {
        let Some(info) = infos.iter_mut().find(|l| l.layer == tile.layer) else { continue };
        info.tiles_total += 1;
        let rect = ScreenRect::of_tile(tile);
        info.bounds = Some(info.bounds.map_or(rect, |b| b.union(rect)));
        if tile.layer > 1 && tile.param != 0 {
            match info.groups.iter_mut().find(|g| g.param == tile.param) {
                Some(g) => {
                    g.state_mask |= tile.state;
                    g.tiles += 1;
                }
                None => info.groups.push(GroupInfo { param: tile.param, state_mask: tile.state, tiles: 1 }),
            }
        }
        let layer_on =
            usize::from(tile.layer).checked_sub(1).and_then(|i| options.layers.get(i)).copied().unwrap_or(false);
        if !layer_on || !options.groups.allows(tile) {
            info.tiles_hidden += 1;
            continue;
        }
        let blended = options.blending && tile.layer > 1 && tile.blending != 0;
        match draw_tile(&mut canvas, background, palette, tile, blended) {
            Some(()) => {
                info.tiles_drawn += 1;
                if blended {
                    info.tiles_blended += 1;
                }
            }
            None => info.tiles_unrenderable += 1,
        }
    }
    for info in &mut infos {
        info.groups.sort_by_key(|g| g.param);
    }

    let image_bounds = ScreenRect { min_x: bounds.min_x, min_y: bounds.min_y, max_x: bounds.max_x, max_y: bounds.max_y };
    Ok(BackgroundImage {
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
        rgba: canvas.rgba,
        origin_x: canvas.origin_x,
        origin_y: canvas.origin_y,
        view_bounds: view.or(layer1_bounds).unwrap_or(image_bounds),
        layers: infos,
    })
}

/// Back-to-front key (larger = further back), following Makou Reactor's tile ids.
fn draw_depth(tile: &TileSprite) -> u32 {
    match tile.layer {
        1 => 4095,
        3 => 4096,
        4 => 0,
        _ => u32::from(tile.id),
    }
}

struct Canvas {
    width: usize,
    height: usize,
    origin_x: i32,
    origin_y: i32,
    rgba: Vec<u8>,
}

/// Draw one tile; `None` if it cannot be rendered (missing page/palette, source outside the
/// page, or destination outside the canvas). A tile is drawn completely or not at all.
fn draw_tile(
    canvas: &mut Canvas,
    background: &Background<'_>,
    palette: &Palette,
    tile: &TileSprite,
    blended: bool,
) -> Option<()> {
    let texels = background.tile_rgba(tile, palette)?;
    let size = usize::from(tile.tile_size);
    let dst_x = usize::try_from(i32::from(tile.dst_x).checked_add(canvas.origin_x)?).ok()?;
    let dst_y = usize::try_from(i32::from(tile.dst_y).checked_add(canvas.origin_y)?).ok()?;
    if dst_x.checked_add(size)? > canvas.width || dst_y.checked_add(size)? > canvas.height {
        return None;
    }
    let mode = BlendMode::from_type_trans(tile.type_trans);
    for (i, texel) in texels.iter().enumerate() {
        let [fr, fg, fb, fa] = *texel;
        if fa == 0 {
            continue;
        }
        let (x, y) = (i % size, i / size);
        let at = ((dst_y + y) * canvas.width + dst_x + x) * 4;
        let px = canvas.rgba.get_mut(at..at + 4)?;
        let [br, bg, bb] = match px {
            [r, g, b, a] if *a != 0 => [*r, *g, *b],
            _ => [0, 0, 0],
        };
        let out = if blended {
            [mode.apply(br, fr), mode.apply(bg, fg), mode.apply(bb, fb)]
        } else {
            [fr, fg, fb]
        };
        px.copy_from_slice(&[out[0], out[1], out[2], 255]);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{rgb555_to_rgba, FieldFile};
    use crate::testutil::{FieldSpec, LayerSpec, TileSpec};

    fn parse(spec: &FieldSpec) -> FieldFile {
        FieldFile::parse(spec.build()).unwrap()
    }

    /// Page 0 of the synthetic palette maps index `i` to raw colour `i`, page 1 to `0x7FFF - i`.
    /// Slot 0 is an 8-bit page whose pixel at page offset `o = y * 256 + x` is `o % 251`.
    fn index_at(x: usize, y: usize) -> u16 {
        ((y * 256 + x) % 251) as u16
    }

    fn colour(page: u8, index: u16) -> [u8; 3] {
        let raw = if page == 0 { index } else { 0x7FFF - index };
        let [r, g, b, _] = rgb555_to_rgba(raw);
        [r, g, b]
    }

    /// A spec with no layer-2/4 tiles and a single layer-1 tile at the origin.
    fn single(t: TileSpec) -> FieldSpec {
        FieldSpec { tiles: vec![t], layer2: None, layer4: None, texture_slots: vec![(0, 1), (3, 2)], ..FieldSpec::default() }
    }

    fn at(img: &BackgroundImage, sx: i32, sy: i32) -> [u8; 4] {
        img.pixel((sx + img.origin_x) as u32, (sy + img.origin_y) as u32).unwrap()
    }

    #[test]
    fn single_tile_lands_at_its_destination_with_index_zero_transparent() {
        // src (0, 0) of slot 0: page offset 0 holds index 0 (transparent); (1, 0) holds index 1.
        let spec = single(TileSpec { dst_x: -32, dst_y: -16, src_x: 0, src_y: 0, palette_id: 1, ..TileSpec::default() });
        let img = decode_background(&parse(&spec)).unwrap();
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!((img.origin_x, img.origin_y), (32, 16));
        assert_eq!(img.bounds(), ScreenRect { min_x: -32, min_y: -16, max_x: -16, max_y: 0 });
        assert_eq!(img.pixel(0, 0), Some([0, 0, 0, 0]), "palette index 0 is transparent");
        let [r, g, b] = colour(1, index_at(1, 0));
        assert_eq!(img.pixel(1, 0), Some([r, g, b, 255]));
        assert_eq!(at(&img, -31, -16), [r, g, b, 255], "field-screen coordinates map through the origin");
        let [r, g, b] = colour(1, index_at(15, 15));
        assert_eq!(img.pixel(15, 15), Some([r, g, b, 255]));
        assert_eq!(img.pixel(16, 0), None);
        assert_eq!(img.pixel(0, 16), None);
    }

    #[test]
    fn opaque_entry_zero_reaches_both_flat_and_depth_backgrounds() {
        let field = parse(&FieldSpec {
            palette_transparency: [false; 20],
            ..single(TileSpec { dst_x: 0, dst_y: 0, src_x: 0, src_y: 0, palette_id: 1, ..TileSpec::default() })
        });
        let image = decode_background(&field).unwrap();
        assert_eq!(image.pixel(0, 0), Some([255, 255, 255, 255]));
        let tiles = decode_background_tiles_with_depth(&field).unwrap();
        assert_eq!(tiles[0].pixel(0, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn index_zero_is_transparent_on_every_layer_even_when_its_colour_is_not_black() {
        // Page 1's index 0 is 0x7FFF (white), page 0's is 0x0000: both transparent. Layers 2, 3
        // and 4 each draw a tile whose pixel (0, 0) is index 0 and whose pixel (1, 0) is index 1,
        // over an opaque layer-1 backdrop of known colour.
        let backdrop = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 0, ..TileSpec::default() };
        let over = TileSpec { dst_x: 0, dst_y: 0, src_x: 0, src_y: 0, palette_id: 1, id: 10, ..TileSpec::default() };
        let layer = || Some(LayerSpec { width: 640, height: 480, tiles: vec![over.clone()] });
        let spec = FieldSpec {
            tiles: vec![backdrop],
            layer2: layer(),
            layer3: layer(),
            layer4: layer(),
            texture_slots: vec![(0, 1), (3, 2)],
            ..FieldSpec::default()
        };
        let f = parse(&spec);
        let img = decode_background(&f).unwrap();
        // pixel (0, 0) of every overlay is index 0 -> the backdrop's pixel shows through
        let [r, g, b] = colour(0, index_at(16, 32));
        assert_eq!(at(&img, 0, 0), [r, g, b, 255]);
        // pixel (1, 0) of the overlays is opaque index 1 of page 1
        let [r, g, b] = colour(1, index_at(1, 0));
        assert_eq!(at(&img, 1, 0), [r, g, b, 255]);
        // with only layer 3 drawn (nothing behind it) the index-0 pixel is a hole
        let only3 = decode_background_with(&f, &BackgroundOptions { layers: [false, false, true, false], ..BackgroundOptions::default() }).unwrap();
        assert_eq!(at(&only3, 0, 0)[3], 0);
    }

    #[test]
    fn draw_order_is_layer3_then_layer1_then_layer2_by_descending_id_then_layer4() {
        // Five tiles on top of each other, each with a different palette page / source so the
        // winner is identifiable at a pixel where all of them are opaque.
        let t = |src_x: u8, page: u8, id: u16| TileSpec { dst_x: 0, dst_y: 0, src_x, src_y: 32, palette_id: page, id, ..TileSpec::default() };
        let layer = |tiles: Vec<TileSpec>| Some(LayerSpec { width: 640, height: 480, tiles });
        // Pixel (0, 0) of tile (src_x, 32) has page offset 32*256 + src_x.
        // layer 1: src_x 16, page 0; layer 3: src_x 32, page 0; layer 2: ids 50 (src 48) and 5
        // (src 64) on page 1; layer 4: src 80 page 1.
        let spec = FieldSpec {
            tiles: vec![t(16, 0, 0)],
            layer2: layer(vec![t(64, 1, 5), t(48, 1, 50)]),
            layer3: layer(vec![t(32, 0, 0)]),
            layer4: layer(vec![t(80, 1, 0)]),
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        };
        let f = parse(&spec);
        let px = |opts: &BackgroundOptions| {
            let img = decode_background_with(&f, opts).unwrap();
            at(&img, 0, 0)
        };
        let want = |src_x: usize, page: u8| {
            let [r, g, b] = colour(page, index_at(src_x, 32));
            [r, g, b, 255]
        };
        let only = |layers: [bool; 4]| BackgroundOptions { layers, ..BackgroundOptions::default() };
        // everything on: layer 4 is on top
        assert_eq!(px(&BackgroundOptions::default()), want(80, 1));
        // without layer 4: layer 2's NEAREST tile (smallest id = 5) wins, drawn after id 50
        assert_eq!(px(&only([true, true, true, false])), want(64, 1));
        // layer 2 alone with ids 50 then 5: tile id 5 is nearer and drawn last
        assert_eq!(px(&only([false, true, false, false])), want(64, 1));
        // layers 1 and 3 only: layer 1 is in front of layer 3
        assert_eq!(px(&only([true, false, true, false])), want(16, 0));
        // layer 3 alone
        assert_eq!(px(&only([false, false, true, false])), want(32, 0));
    }

    #[test]
    fn blend_modes_combine_with_the_backdrop() {
        let backdrop = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 0, ..TileSpec::default() };
        let (back, front) = (colour(0, index_at(16, 32)), colour(1, index_at(16, 32)));
        for (type_trans, mode) in [(0u8, BlendMode::Average), (1, BlendMode::Additive), (2, BlendMode::Subtractive), (3, BlendMode::AddQuarter)] {
            let over = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 1, blending: 1, type_trans, id: 9, ..TileSpec::default() };
            let spec = FieldSpec {
                tiles: vec![backdrop.clone()],
                layer2: Some(LayerSpec { width: 640, height: 480, tiles: vec![over] }),
                layer4: None,
                texture_slots: vec![(0, 1)],
                ..FieldSpec::default()
            };
            // blended tiles read their source from the *2 fields; mirror the primary ones
            let mut spec = spec;
            if let Some(l2) = spec.layer2.as_mut() {
                for t in &mut l2.tiles {
                    t.src_x2 = t.src_x;
                    t.src_y2 = t.src_y;
                    t.texture_id2 = t.texture_id;
                }
            }
            let f = parse(&spec);
            let img = decode_background(&f).unwrap();
            let expect = [mode.apply(back[0], front[0]), mode.apply(back[1], front[1]), mode.apply(back[2], front[2]), 255];
            assert_eq!(at(&img, 0, 0), expect, "{mode:?}");
            assert_eq!(img.layers[1].tiles_blended, 1);
            // with blending disabled the tile is opaque
            let flat = decode_background_with(&f, &BackgroundOptions { blending: false, ..BackgroundOptions::default() }).unwrap();
            assert_eq!(at(&flat, 0, 0), [front[0], front[1], front[2], 255]);
        }
        // the arithmetic itself
        assert_eq!(BlendMode::Average.apply(200, 100), 150);
        assert_eq!(BlendMode::Additive.apply(200, 100), 255);
        assert_eq!(BlendMode::Subtractive.apply(100, 200), 0);
        assert_eq!(BlendMode::Subtractive.apply(200, 100), 100);
        assert_eq!(BlendMode::AddQuarter.apply(10, 100), 35);
        assert_eq!(BlendMode::from_type_trans(0x0D), BlendMode::Additive); // only the low 2 bits
    }

    #[test]
    fn blended_tile_over_nothing_blends_against_black() {
        let over = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, src_x2: 16, src_y2: 32, palette_id: 1, blending: 1, type_trans: 0, ..TileSpec::default() };
        let spec = FieldSpec {
            tiles: vec![],
            layer2: Some(LayerSpec { width: 640, height: 480, tiles: vec![over] }),
            layer4: None,
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        };
        let img = decode_background(&parse(&spec)).unwrap();
        let front = colour(1, index_at(16, 32));
        assert_eq!(at(&img, 0, 0), [front[0] / 2, front[1] / 2, front[2] / 2, 255]);
    }

    #[test]
    fn group_filter_and_layer_toggles() {
        // default spec: layer-2 tile has param 5 / state 6 (a group), layer 4 tile is static
        let f = parse(&FieldSpec::default());
        let default = decode_background(&f).unwrap();
        assert_eq!(default.layers.len(), 3);
        let l2 = &default.layers[1];
        assert_eq!((l2.layer, l2.tiles_total, l2.tiles_drawn, l2.tiles_hidden), (2, 1, 0, 1));
        assert_eq!(l2.groups, vec![GroupInfo { param: 5, state_mask: 6, tiles: 1 }]);
        let all = decode_background_with(&f, &BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() }).unwrap();
        assert_eq!(all.layers[1].tiles_drawn, 1);
        let wrong_frame = BackgroundOptions { groups: GroupFilter::Only(vec![(5, 1)]), ..BackgroundOptions::default() };
        assert_eq!(decode_background_with(&f, &wrong_frame).unwrap().layers[1].tiles_drawn, 0);
        let right_frame = BackgroundOptions { groups: GroupFilter::Only(vec![(5, 6)]), ..BackgroundOptions::default() };
        assert_eq!(decode_background_with(&f, &right_frame).unwrap().layers[1].tiles_drawn, 1);
        // layer 1 ignores param/state even if the record carries them
        let spec = single(TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 0, param: 9, state: 9, ..TileSpec::default() });
        assert_eq!(decode_background(&parse(&spec)).unwrap().layers[0].tiles_drawn, 1);
        // layer switches
        let no_l1 = decode_background_with(&f, &BackgroundOptions { layers: [false, true, true, true], ..BackgroundOptions::default() }).unwrap();
        assert_eq!((no_l1.layers[0].tiles_drawn, no_l1.layers[0].tiles_hidden), (0, 2));
        // the canvas does not depend on the options
        assert_eq!((no_l1.width, no_l1.height, no_l1.origin_x), (default.width, default.height, default.origin_x));
    }

    #[test]
    fn scripted_group_masks_select_the_same_tiles_in_flat_and_depth_decodes() {
        // The synthetic layer-2 tile belongs to group 5 and accepts either bit 1 or bit 2 (state 6).
        let f = parse(&FieldSpec::default());
        let mut state = BackgroundState::default();
        let visible = |state: &BackgroundState| {
            let options = BackgroundOptions { groups: GroupFilter::State(state.clone()), ..BackgroundOptions::default() };
            let image = decode_background_with(&f, &options).unwrap();
            let tiles = decode_background_tiles_with_depth_options(&f, &options).unwrap();
            (image.layers[1].tiles_drawn, tiles.iter().filter(|t| t.layer == 2).count())
        };
        assert_eq!(visible(&state), (0, 0));
        state.apply(BackgroundOp::On { group: 5, frame: 1 });
        assert_eq!(visible(&state), (1, 1), "state 6 intersects enabled bit 2");
        let exact = BackgroundOptions { groups: GroupFilter::Only(vec![(5, 2)]), ..BackgroundOptions::default() };
        assert_eq!(decode_background_with(&f, &exact).unwrap().layers[1].tiles_drawn, 0, "Only still selects exact pairs");
        state.apply(BackgroundOp::Off { group: 5, frame: 1 });
        assert_eq!(visible(&state), (0, 0));
        state.apply(BackgroundOp::On { group: 5, frame: 2 });
        assert_eq!(visible(&state), (1, 1), "the other accepted bit also shows the tile");
        state.apply(BackgroundOp::Clear { group: 5 });
        assert_eq!(visible(&state), (0, 0));
    }

    #[test]
    fn unrenderable_tiles_are_counted_not_fatal() {
        let bad_palette = TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 32, palette_id: 9, ..TileSpec::default() };
        let missing_slot = TileSpec { dst_x: 16, dst_y: 0, texture_id: 9, ..TileSpec::default() };
        let off_page = TileSpec { dst_x: 32, dst_y: 0, src_x: 250, ..TileSpec::default() };
        let ok = TileSpec { dst_x: 48, dst_y: 0, src_x: 16, src_y: 32, ..TileSpec::default() };
        let spec = single(TileSpec::default());
        let spec = FieldSpec { tiles: vec![bad_palette, missing_slot, off_page, ok], ..spec };
        let img = decode_background(&parse(&spec)).unwrap();
        assert_eq!((img.layers[0].tiles_total, img.layers[0].tiles_drawn, img.layers[0].tiles_unrenderable), (4, 1, 3));
        // the canvas still covers every kept tile
        assert_eq!((img.width, img.height), (64, 16));
    }

    #[test]
    fn texture_errors_are_reported_and_empty_backgrounds_are_fine() {
        let broken = FieldSpec { corrupt_texture_tag: true, ..FieldSpec::default() };
        assert!(matches!(decode_background(&parse(&broken)), Err(FieldError::Invalid { .. })));
        let empty = FieldSpec { tiles: vec![], layer2: None, layer4: None, texture_slots: vec![], ..FieldSpec::default() };
        let img = decode_background(&parse(&empty)).unwrap();
        assert_eq!((img.width, img.height, img.rgba.len()), (0, 0, 0));
        assert_eq!(img.pixel(0, 0), None);
        assert_eq!(img.clamp_view_centre(5.0, 5.0), (0.0, 0.0)); // camera range 320x240 is centred on 0
    }

    #[test]
    fn view_bounds_come_from_the_camera_range_and_clamp_the_view() {
        // synthetic camera range is (-160, -120, 160, 120): a non-scrolling screen
        let f = parse(&FieldSpec::default());
        let img = decode_background(&f).unwrap();
        assert_eq!(img.view_bounds, ScreenRect { min_x: -160, min_y: -120, max_x: 160, max_y: 120 });
        assert_eq!(img.clamp_view_centre(100.0, -50.0), (0.0, 0.0));
        // a bigger range scrolls: clamp keeps the 320x240 window inside it
        let mut big = img.clone();
        big.view_bounds = ScreenRect { min_x: -256, min_y: -256, max_x: 256, max_y: 256 };
        assert_eq!(big.clamp_view_centre(0.0, 0.0), (0.0, 0.0));
        assert_eq!(big.clamp_view_centre(1000.0, -1000.0), (96.0, -136.0));
        assert_eq!(big.clamp_view_centre(-1000.0, 1000.0), (-96.0, 136.0));
        // an axis smaller than the window is centred on the bounds
        big.view_bounds = ScreenRect { min_x: 0, min_y: -300, max_x: 100, max_y: 300 };
        assert_eq!(big.clamp_view_centre(-50.0, 0.0), (50.0, 0.0));
        // fall back to layer-1 bounds when no range is supplied
        let background = f.background().unwrap();
        let palette = f.palette().unwrap();
        let no_range = compose(&background, &palette, &BackgroundOptions::default(), None).unwrap();
        assert_eq!(no_range.view_bounds, ScreenRect { min_x: -160, min_y: -120, max_x: -128, max_y: -104 });
    }

    #[test]
    fn projection_with_a_simple_camera() {
        // looking along +y with +z up: camera x = world x, camera y (down) = -world z, camera z =
        // world y; 4096 = 1.0
        let cam = Camera { axes: [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]], position: [0, 0, 1000], zoom: 500 };
        let p = Projection::from_camera(&cam);
        let s = p.project([200.0, 1000.0, 0.0]).unwrap();
        assert!((s.x - 50.0).abs() < 1e-3, "{s:?}"); // 200 * 500 / 2000
        assert!(s.y.abs() < 1e-3);
        assert!((s.depth - 2000.0).abs() < 1e-3);
        // height moves the point up the screen (negative y)
        assert!(p.project([0.0, 1000.0, 100.0]).unwrap().y < 0.0);
        // at or behind the camera plane
        assert!(p.project([0.0, -1000.0, 0.0]).is_none());
        assert!(p.project([0.0, -1500.0, 0.0]).is_none());
        let c = p.to_camera([1.0, 2.0, 3.0]);
        assert_eq!(c, [1.0, -3.0, 1002.0]);
        assert_eq!(p.project_i16([200, 1000, 0]), p.project([200.0, 1000.0, 0.0]));
        // pixel mapping round-trip
        let img = decode_background(&parse(&FieldSpec::default())).unwrap();
        let (px, py) = img.screen_to_pixel(s.x, s.y);
        let (sx, sy) = img.pixel_to_screen(px, py);
        assert!((sx - s.x).abs() < 1e-4 && (sy - s.y).abs() < 1e-4);
        // an all-zero camera never divides by zero
        let zero = Projection::from_camera(&Camera { axes: [[0; 3]; 3], position: [0; 3], zoom: 0 });
        assert!(zero.project([1.0, 2.0, 3.0]).is_none());
    }

    #[test]
    fn flatten_on_black_makes_everything_opaque() {
        let spec = single(TileSpec { dst_x: 0, dst_y: 0, src_x: 0, src_y: 0, palette_id: 1, ..TileSpec::default() });
        let img = decode_background(&parse(&spec)).unwrap();
        let flat = img.flatten_on_black();
        assert_eq!(flat.len(), img.rgba.len());
        assert!(flat.chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(&flat[..4], &[0, 0, 0, 255]); // the transparent pixel became black
        assert!(format!("{img:?}").contains("origin"));
    }

    #[test]
    fn depth_zero_pages_decode_with_low_nibble_first() {
        // slot 1 is a 4-bit page whose byte `i` is `i % 253`; page pixel `p` is the low nibble of
        // byte `p / 2` when `p` is even and the high nibble when it is odd. The tile reads pixels
        // 16..32 of row 0, i.e. bytes 8..16 (values below 16 have a zero high nibble).
        let spec = FieldSpec {
            tiles: vec![TileSpec { dst_x: 0, dst_y: 0, src_x: 16, src_y: 0, texture_id: 1, palette_id: 0, ..TileSpec::default() }],
            layer2: None,
            layer4: None,
            texture_slots: vec![(1, 0)],
            ..FieldSpec::default()
        };
        let img = decode_background(&parse(&spec)).unwrap();
        // x = 0 -> byte 8 low nibble = 8; x = 1 -> byte 8 high nibble = 0 (transparent);
        // x = 2 -> byte 9 low nibble = 9; x = 3 -> transparent; x = 4 -> byte 10 low nibble = 10
        for (x, index) in [(0u32, 8u16), (1, 0), (2, 9), (3, 0), (4, 10)] {
            let px = img.pixel(x, 0).unwrap();
            if index == 0 {
                assert_eq!(px[3], 0, "pixel {x}");
            } else {
                let [r, g, b] = colour(0, index);
                assert_eq!(px, [r, g, b, 255], "pixel {x}");
            }
        }
    }

    #[test]
    fn composing_damaged_fields_never_panics() {
        // one 64 KiB page keeps the clone-per-case loops cheap
        let good = FieldSpec { texture_slots: vec![(0, 1)], ..FieldSpec::default() }.build();
        let exercise = |bytes: Vec<u8>| {
            if let Ok(f) = FieldFile::parse(bytes) {
                let _ = decode_background(&f);
                let _ = decode_background_with(&f, &BackgroundOptions { groups: GroupFilter::All, layers: [true; 4], blending: true });
            }
        };
        let structured = good.len().min(2600);
        let mut lens: Vec<usize> = (0..structured).collect();
        lens.extend((structured..good.len()).step_by(4099));
        lens.push(good.len() - 1);
        for n in lens {
            exercise(good[..n].to_vec());
        }
        for i in 0..structured {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                exercise(b);
            }
        }
    }

    /// A spec with all four layers, no blending and no animation groups: layer 1 has two tiles,
    /// layer 2 two tiles (the first over layer 1's second tile), layer 3 and 4 one tile each.
    fn four_layer_spec() -> FieldSpec {
        let t = |dst_x: i16, dst_y: i16, src_x: u8, page: u8, id: u16| TileSpec {
            dst_x,
            dst_y,
            src_x,
            src_y: 32,
            palette_id: page,
            id,
            ..TileSpec::default()
        };
        let layer = |tiles: Vec<TileSpec>| Some(LayerSpec { width: 640, height: 480, tiles });
        FieldSpec {
            tiles: vec![t(0, 0, 16, 0, 0), t(16, 0, 16, 0, 0)],
            layer2: layer(vec![t(16, 0, 64, 1, 5), t(48, 16, 48, 1, 6)]),
            layer3: layer(vec![t(-32, -32, 32, 0, 0)]),
            layer4: layer(vec![t(48, 48, 80, 1, 0)]),
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        }
    }

    #[test]
    fn depth_layers_have_the_canvas_of_the_full_decode_and_split_the_layers() {
        let f = parse(&four_layer_spec());
        let full = decode_background(&f).unwrap();
        let split = decode_background_layers(&f).unwrap();
        assert_eq!(BACK_LAYERS, [true, false, true, false]);
        assert_eq!(FRONT_LAYERS, [false, true, false, true]);
        for half in [&split.back, &split.front] {
            assert_eq!(
                (half.width, half.height, half.origin_x, half.origin_y, half.view_bounds),
                (full.width, full.height, full.origin_x, full.origin_y, full.view_bounds)
            );
            assert_eq!(half.rgba.len(), full.rgba.len());
        }
        // layers 1 and 3 behind, 2 and 4 in front; the other half's tiles are only "hidden"
        let drawn = |img: &BackgroundImage| img.layers.iter().map(|l| (l.layer, l.tiles_drawn, l.tiles_hidden)).collect::<Vec<_>>();
        assert_eq!(drawn(&split.back), [(1, 2, 0), (2, 0, 2), (3, 1, 0), (4, 0, 1)]);
        assert_eq!(drawn(&split.front), [(1, 0, 2), (2, 2, 0), (3, 0, 1), (4, 1, 0)]);
        // alpha-over of the front onto the back reproduces the full composite exactly when
        // nothing is blended (the draw order inside each half is the full draw order)
        let mut over = split.back.rgba.clone();
        for (dst, src) in over.chunks_exact_mut(4).zip(split.front.rgba.chunks_exact(4)) {
            if src[3] == 255 {
                dst.copy_from_slice(src);
            } else {
                assert_eq!(src[3], 0, "tiles are opaque or transparent here");
            }
        }
        assert_eq!(over, full.rgba);
        // the halves really differ: layer 2's tile over layer 1 shows only in the front image
        let (px, py) = (u32::try_from(20 + full.origin_x).unwrap(), u32::try_from(4 + full.origin_y).unwrap());
        assert_eq!(split.front.pixel(px, py).map(|p| p[3]), Some(255));
        assert_ne!(split.front.pixel(px, py), split.back.pixel(px, py));
        // a pixel only layer 1 draws is transparent in the front image
        let (qx, qy) = (u32::try_from(4 + full.origin_x).unwrap(), u32::try_from(4 + full.origin_y).unwrap());
        assert_eq!(split.front.pixel(qx, qy).map(|p| p[3]), Some(0));
        assert_eq!(split.back.pixel(qx, qy).map(|p| p[3]), Some(255));
    }

    #[test]
    fn depth_layers_with_options_ignore_the_layer_switches_but_keep_groups_and_blending() {
        // the default spec has a layer-2 tile in animation group 5 and a blended tile
        let f = parse(&FieldSpec::default());
        let plain = decode_background_layers(&f).unwrap();
        assert_eq!(plain.front.layers[1].tiles_drawn, 0, "group tiles are hidden by default");
        let everything = BackgroundOptions { layers: [false; 4], groups: GroupFilter::All, blending: true };
        let split = decode_background_layers_with(&f, &everything).unwrap();
        assert_eq!(split.front.layers[1].tiles_drawn, 1, "groups apply to the split");
        assert_eq!(split.front.layers[1].tiles_blended, 1);
        assert_eq!(split.back.layers[0].tiles_drawn, 2, "the layers field is ignored: layer 1 is in the back half");
        let opaque = BackgroundOptions { blending: false, groups: GroupFilter::All, ..BackgroundOptions::default() };
        assert_eq!(decode_background_layers_with(&f, &opaque).unwrap().front.layers[1].tiles_blended, 0);
        // the same errors as the plain decode
        let broken = FieldSpec { corrupt_texture_tag: true, ..FieldSpec::default() };
        assert!(matches!(decode_background_layers(&parse(&broken)), Err(FieldError::Invalid { .. })));
    }

    #[test]
    fn widest_legal_span_fits_the_canvas_limit() {
        // The parser keeps |dst| < 1024, so two tiles at -1023 and 1023 give the widest canvas a
        // field can produce; it must stay within MAX_CANVAS_SIDE.
        let wide = FieldSpec {
            tiles: vec![
                TileSpec { dst_x: -1023, dst_y: 0, src_x: 16, src_y: 32, ..TileSpec::default() },
                TileSpec { dst_x: 1023, dst_y: 0, src_x: 16, src_y: 32, ..TileSpec::default() },
            ],
            layer2: None,
            layer4: None,
            texture_slots: vec![(0, 1)],
            ..FieldSpec::default()
        };
        let img = decode_background(&parse(&wide)).unwrap();
        assert_eq!((img.width, img.height), (2046 + 16, 16));
        assert!(i32::try_from(img.width).unwrap() <= MAX_CANVAS_SIDE);
        assert_eq!(MAX_CANVAS_SIDE, 2080);
    }
}
