//! The field view of the window: how an area's world coordinates become pixels, and the
//! background picture behind the walkmesh.
//!
//! The renderer ([`crate::render`]) knows nothing about any game. It draws in *view space*
//! (+x right, +y up, any units): a list of triangles, player positions and, optionally, one
//! picture ([`Backdrop`]) covering a rectangle of that space. This module builds that input:
//!
//! * [`FieldView::top_down`]: the area seen from above (view space = the area's x/y). This is
//!   what FIXTURE areas, and any area without more render data, use.
//! * [`FieldView::ff7`]: an FF7 field seen through its own camera. The camera record projects the
//!   walkmesh (and the players) into the field's screen space, where the decoded background
//!   pictures live, so the walkmesh lines up with the scenery exactly as in the game.
//!
//! This is the one place that touches FF7 render data (`fmv_ff7::background`). The pictures are
//! decoded from this machine's own copy of the game and only ever go to this machine's GPU:
//! nothing here is sent anywhere.

use crate::avatar::scenery::Scenery;
use crate::avatar::stage::{Stage, StageCamera};
use fmv_adapter_api::{AreaRuntime, Bounds};
use fmv_core::Pos;
use fmv_ff7::background::{decode_background, BackgroundImage, Projection};
use fmv_ff7::{default_model_scale, Ff7Adapter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Largest backdrop side accepted (pixels). A field background is a few hundred pixels wide; the
/// cap only keeps a corrupt image from asking the GPU for an absurd texture.
pub const MAX_BACKDROP_SIDE: u32 = 8192;

static NEXT_BACKDROP_ID: AtomicU64 = AtomicU64::new(1);

/// One picture drawn behind the walkmesh.
#[derive(Clone)]
pub struct Backdrop {
    pub width: u32,
    pub height: u32,
    /// Opaque RGBA8 pixels, row-major, top row first (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Corner of the covered rectangle in view space with the smallest x and y (bottom left).
    pub min: [f32; 2],
    /// Corner of the covered rectangle in view space with the largest x and y (top right).
    pub max: [f32; 2],
    /// Distinguishes pictures, so the GPU copy is only replaced when the picture changed.
    pub id: u64,
}

impl std::fmt::Debug for Backdrop {
    /// Geometry only, never pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backdrop")
            .field("size", &(self.width, self.height))
            .field("min", &self.min)
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl Backdrop {
    /// A picture covering `min..max` of the view space. `None` if the pixel data does not match
    /// the size, the size is empty or too large, or the rectangle is not a finite, non-empty one.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, min: [f32; 2], max: [f32; 2]) -> Option<Self> {
        let expected = u64::from(width) * u64::from(height) * 4;
        let sound = width > 0
            && height > 0
            && width <= MAX_BACKDROP_SIDE
            && height <= MAX_BACKDROP_SIDE
            && rgba.len() as u64 == expected
            && min.iter().chain(max.iter()).all(|v| v.is_finite())
            && max[0] > min[0]
            && max[1] > min[1];
        sound.then(|| Self { width, height, rgba, min, max, id: NEXT_BACKDROP_ID.fetch_add(1, Ordering::Relaxed) })
    }

    /// The decoded FF7 background as a backdrop. The image is in field-screen space (x right, y
    /// DOWN, `origin` = the pixel of screen position 0,0); view space has y up, so y is negated.
    /// Pixels no tile touched are shown over black, as the game does.
    pub fn from_ff7(image: &BackgroundImage) -> Option<Self> {
        let b = image.bounds();
        Self::new(
            image.width,
            image.height,
            image.flatten_on_black(),
            [b.min_x as f32, -(b.max_y as f32)],
            [b.max_x as f32, -(b.min_y as f32)],
        )
    }
}

/// World -> view mapping. `None` means "cannot be drawn" (behind the camera, not finite).
type ViewMap = Box<dyn Fn(Pos) -> Option<Pos> + Send + Sync>;

/// Everything the window needs to draw an area: walkmesh and bounds in view space, the mapping
/// for moving things, and the picture behind them if there is one.
pub struct FieldView {
    map: ViewMap,
    /// Walkable triangles in view space (`x`, `y`; `z` is 0). Triangles that cannot be projected
    /// are left out.
    pub triangles: Vec<[Pos; 3]>,
    /// What the camera fits into the window: the picture's rectangle, else the walkmesh's extent.
    pub bounds: Bounds,
    pub backdrop: Option<Backdrop>,
    /// One line for the log: how the view was built.
    pub description: String,
    /// FF7 fields only: what drawing the players as the game's character models, and pacing them,
    /// needs (the camera, the model scale, the speeds, the depth-tested scenery). `None` for
    /// everything else, which keeps the plain markers.
    pub stage: Option<Stage>,
}

impl std::fmt::Debug for FieldView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldView")
            .field("triangles", &self.triangles.len())
            .field("bounds", &self.bounds)
            .field("backdrop", &self.backdrop)
            .field("description", &self.description)
            .field("stage", &self.stage.is_some())
            .finish_non_exhaustive()
    }
}

fn finite_xy(p: Pos) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

impl FieldView {
    /// World position -> view position (`z` is 0), or `None` if it cannot be drawn.
    pub fn to_view(&self, p: Pos) -> Option<Pos> {
        (self.map)(p).filter(|q| finite_xy(*q))
    }

    /// The direction in the area's own x/y axes (a unit vector) that appears as `screen`
    /// (right, up) on screen at world position `at`, so that "up" keys walk up the screen
    /// whatever the camera looks like. Found by inverting the mapping's local slope (the 2x2
    /// matrix of view-space displacement per unit of world x and world y). `None` if the mapping
    /// cannot be evaluated there or is degenerate; callers then use `screen` as it is.
    pub fn screen_to_world_dir(&self, at: Pos, screen: (f32, f32)) -> Option<(f32, f32)> {
        const H: f32 = 8.0; // world units; small against any walkmesh, large against f32 noise
        let origin = self.to_view(at)?;
        let along_x = self.to_view(Pos::new(at.x + H, at.y, at.z))?;
        let along_y = self.to_view(Pos::new(at.x, at.y + H, at.z))?;
        let (a, c) = ((along_x.x - origin.x) / H, (along_x.y - origin.y) / H);
        let (b, d) = ((along_y.x - origin.x) / H, (along_y.y - origin.y) / H);
        let det = a * d - b * c;
        if !det.is_finite() || det.abs() < 1e-9 {
            return None;
        }
        let wx = (d * screen.0 - b * screen.1) / det;
        let wy = (-c * screen.0 + a * screen.1) / det;
        let len = wx.hypot(wy);
        (len.is_finite() && len > 1e-9).then(|| (wx / len, wy / len))
    }

    /// The area seen from above: view space is the area's own x/y, no picture.
    pub fn top_down(area: &dyn AreaRuntime) -> Self {
        Self {
            map: Box::new(|p| Some(Pos::new(p.x, p.y, 0.0))),
            triangles: area.walk_triangles(),
            bounds: area.bounds(),
            backdrop: None,
            description: "top-down walkmesh view".to_string(),
            stage: None,
        }
    }

    /// Build a view from a world->view `map`: the world triangles are projected (those with a
    /// vertex that cannot be, are dropped) and `backdrop` goes behind them. The fit rectangle is
    /// the picture's, or else the extent of what could be projected (`fallback` if nothing).
    pub fn projected(
        world_triangles: &[[Pos; 3]],
        fallback: Bounds,
        map: impl Fn(Pos) -> Option<Pos> + Send + Sync + 'static,
        backdrop: Option<Backdrop>,
        description: impl Into<String>,
    ) -> Self {
        let project = |p: &Pos| map(*p).filter(|q| finite_xy(*q)).map(|q| Pos::new(q.x, q.y, 0.0));
        let triangles: Vec<[Pos; 3]> = world_triangles
            .iter()
            .filter_map(|t| Some([project(&t[0])?, project(&t[1])?, project(&t[2])?]))
            .collect();
        let bounds = match &backdrop {
            Some(b) => Bounds { min: Pos::new(b.min[0], b.min[1], 0.0), max: Pos::new(b.max[0], b.max[1], 0.0) },
            None => extent(&triangles).unwrap_or(fallback),
        };
        Self { map: Box::new(map), triangles, bounds, backdrop, description: description.into(), stage: None }
    }

    /// An FF7 field seen through its own camera, with its decoded background behind it.
    ///
    /// Uses the field's first camera (the one active at the start; fields with several cameras
    /// switch between them as the player walks, which this free-roam view does not do). Needs
    /// this machine's own copy of the game: `adapter` decodes the field again from it.
    pub fn ff7(adapter: &Ff7Adapter, key: &str) -> Result<Self, String> {
        // The depth-tested scenery is only worth preparing when this machine has the character
        // archive to draw models from.
        Self::ff7_with(adapter, key, crate::avatar::source::remembered().is_some())
    }

    /// [`FieldView::ff7`] with the choice of whether to prepare the depth-tested scenery
    /// (`scenery`) made by the caller.
    pub fn ff7_with(adapter: &Ff7Adapter, key: &str, scenery: bool) -> Result<Self, String> {
        let area = adapter.load_field(key).map_err(|e| format!("cannot load field {key} for rendering: {e}"))?;
        let cameras = area.cameras().map_err(|e| format!("field {key} has no readable camera: {e}"))?;
        let camera = cameras.first().ok_or_else(|| format!("field {key} has no camera"))?;
        let projection = Projection::from_camera(camera);

        // Field-screen space has y DOWN; the renderer's view space has y up.
        let map = move |p: Pos| {
            projection.project([p.x, p.y, p.z]).map(|s| Pos::new(s.x, -s.y, 0.0))
        };
        let (backdrop, canvas, picture) = match decode_background(area.field_file()) {
            Ok(image) => match Backdrop::from_ff7(&image) {
                Some(b) => (Some(b), Some(image.bounds()), format!("{}x{} background", image.width, image.height)),
                None => (None, None, "background image unusable".to_string()),
            },
            Err(e) => (None, None, format!("background not decoded ({e})")),
        };
        let world = area.walk_triangles();
        let mut view = Self::projected(
            &world,
            area.bounds(),
            map,
            backdrop,
            format!("FF7 field {key} through its camera, {picture}"),
        );
        if view.triangles.is_empty() {
            return Err(format!("no walkmesh triangle of field {key} is in front of its camera"));
        }
        // The picture as depth-tested tiles, on the rectangle of the flat picture.
        let (scenery, note) = match (scenery, canvas) {
            (true, Some(canvas)) => {
                let (scenery, note) = Scenery::from_field(area.field_file(), canvas);
                (scenery.map(Arc::new), note)
            }
            (true, None) => (None, "no picture, so nothing goes in front of the characters".to_string()),
            (false, _) => (None, "characters are drawn as markers".to_string()),
        };
        view.description.push_str(&format!("; {note}"));
        view.stage = Some(Stage {
            camera: StageCamera::from_camera(camera),
            model_scale: default_model_scale(area.field_scale()),
            walk_speed: area.walk_speed(),
            run_speed: area.run_speed(),
            scenery,
        });
        Ok(view)
    }
}

/// Smallest rectangle (view space) around every vertex, if there is any.
fn extent(triangles: &[[Pos; 3]]) -> Option<Bounds> {
    let mut it = triangles.iter().flatten();
    let first = it.next()?;
    let (mut lo, mut hi) = ([first.x, first.y], [first.x, first.y]);
    for p in it {
        lo = [lo[0].min(p.x), lo[1].min(p.y)];
        hi = [hi[0].max(p.x), hi[1].max(p.y)];
    }
    Some(Bounds { min: Pos::new(lo[0], lo[1], 0.0), max: Pos::new(hi[0], hi[1], 0.0) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};

    fn tri(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> [Pos; 3] {
        [Pos::new(a.0, a.1, 5.0), Pos::new(b.0, b.1, 5.0), Pos::new(c.0, c.1, 5.0)]
    }

    #[test]
    fn a_backdrop_must_be_sound() {
        let px = |w: u32, h: u32| vec![0u8; (w * h * 4) as usize];
        let ok = Backdrop::new(2, 3, px(2, 3), [-1.0, -2.0], [1.0, 2.0]).expect("sound");
        assert_eq!((ok.width, ok.height, ok.rgba.len()), (2, 3, 24));
        // Wrong pixel count, empty, too large, empty or inverted or non-finite rectangle.
        assert!(Backdrop::new(2, 3, px(2, 2), [-1.0, -2.0], [1.0, 2.0]).is_none());
        assert!(Backdrop::new(0, 3, vec![], [-1.0, -2.0], [1.0, 2.0]).is_none());
        assert!(Backdrop::new(MAX_BACKDROP_SIDE + 1, 1, vec![0; (MAX_BACKDROP_SIDE as usize + 1) * 4], [0.0; 2], [1.0; 2]).is_none());
        assert!(Backdrop::new(2, 3, px(2, 3), [1.0, 2.0], [1.0, 3.0]).is_none());
        assert!(Backdrop::new(2, 3, px(2, 3), [2.0, 0.0], [1.0, 3.0]).is_none());
        assert!(Backdrop::new(2, 3, px(2, 3), [f32::NAN, 0.0], [1.0, 3.0]).is_none());
        // Every picture has its own identity, so a changed picture is re-uploaded.
        let other = Backdrop::new(2, 3, px(2, 3), [-1.0, -2.0], [1.0, 2.0]).unwrap();
        assert_ne!(ok.id, other.id);
        assert!(!format!("{ok:?}").contains("rgba"), "Debug must not print pixels");
    }

    #[test]
    fn top_down_is_the_identity_on_the_ground_plane() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let view = FieldView::top_down(area.as_ref());
        assert_eq!(view.triangles.len(), area.walk_triangles().len());
        assert_eq!(view.bounds, area.bounds());
        assert!(view.backdrop.is_none());
        assert_eq!(view.to_view(Pos::new(12.0, -7.0, 99.0)), Some(Pos::new(12.0, -7.0, 0.0)));
        assert_eq!(view.to_view(Pos::new(f32::NAN, 0.0, 0.0)), None, "non-finite input is never drawn");
    }

    #[test]
    fn screen_directions_become_world_directions_through_the_mapping() {
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        let at = Pos::new(100.0, 200.0, 5.0);
        let fallback = Bounds { min: Pos::default(), max: Pos::new(1.0, 1.0, 0.0) };

        // Identity: screen right/up are world +x/+y; a diagonal is normalised.
        let id = FieldView::projected(&[], fallback, |p| Some(Pos::new(p.x, p.y, 0.0)), None, "id");
        assert!(near(id.screen_to_world_dir(at, (1.0, 0.0)).unwrap(), (1.0, 0.0)));
        assert!(near(id.screen_to_world_dir(at, (0.0, 1.0)).unwrap(), (0.0, 1.0)));
        let d = std::f32::consts::FRAC_1_SQRT_2;
        assert!(near(id.screen_to_world_dir(at, (1.0, 1.0)).unwrap(), (d, d)));

        // A view rotated a quarter turn (world +y is screen LEFT, world +x is screen UP) and
        // stretched: pressing up must walk world +x, pressing right must walk world -y.
        let rot = FieldView::projected(&[], fallback, |p| Some(Pos::new(-3.0 * p.y, 0.5 * p.x, 0.0)), None, "rot");
        assert!(near(rot.screen_to_world_dir(at, (0.0, 1.0)).unwrap(), (1.0, 0.0)));
        assert!(near(rot.screen_to_world_dir(at, (1.0, 0.0)).unwrap(), (0.0, -1.0)));

        // A mirror (world +y is screen DOWN, as in field-screen space) flips the vertical key.
        let mirror = FieldView::projected(&[], fallback, |p| Some(Pos::new(p.x, -p.y, 0.0)), None, "mirror");
        assert!(near(mirror.screen_to_world_dir(at, (0.0, 1.0)).unwrap(), (0.0, -1.0)));

        // A degenerate mapping (everything on one line) or one that cannot be drawn here: None.
        let flat = FieldView::projected(&[], fallback, |p| Some(Pos::new(p.x + p.y, p.x + p.y, 0.0)), None, "flat");
        assert_eq!(flat.screen_to_world_dir(at, (1.0, 0.0)), None);
        let behind = FieldView::projected(&[], fallback, |_| None, None, "behind");
        assert_eq!(behind.screen_to_world_dir(at, (1.0, 0.0)), None);
    }

    #[test]
    fn projected_views_drop_what_cannot_be_drawn_and_fit_what_remains() {
        // A mapping that cannot draw anything with x < 0 (like a point behind the camera).
        let map = |p: Pos| (p.x >= 0.0).then(|| Pos::new(p.x * 2.0, -p.y, 0.0));
        let world = [tri((0.0, 0.0), (10.0, 0.0), (0.0, 10.0)), tri((-5.0, 0.0), (10.0, 0.0), (0.0, 10.0))];
        let fallback = Bounds { min: Pos::new(-1.0, -1.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) };
        let view = FieldView::projected(&world, fallback, map, None, "test");
        assert_eq!(view.triangles.len(), 1, "the triangle with an undrawable vertex is dropped");
        assert_eq!(view.triangles[0][1], Pos::new(20.0, 0.0, 0.0), "the height does not survive into view space");
        // Without a picture the fit rectangle is the extent of the drawn triangles (y mirrored).
        assert_eq!((view.bounds.min.x, view.bounds.max.x), (0.0, 20.0));
        assert_eq!((view.bounds.min.y, view.bounds.max.y), (-10.0, 0.0));
        assert_eq!(view.to_view(Pos::new(3.0, 4.0, 9.0)), Some(Pos::new(6.0, -4.0, 0.0)));
        assert_eq!(view.to_view(Pos::new(-1.0, 4.0, 9.0)), None);

        // Nothing drawable: the fallback bounds keep the camera well defined.
        let none = FieldView::projected(&world, fallback, |_| None, None, "test");
        assert!(none.triangles.is_empty());
        assert_eq!(none.bounds, fallback);

        // With a picture, the picture's rectangle is what is fitted.
        let pic = Backdrop::new(1, 1, vec![9, 9, 9, 255], [-100.0, -50.0], [100.0, 50.0]).unwrap();
        let with = FieldView::projected(&world, fallback, map, Some(pic), "test");
        assert_eq!((with.bounds.min.x, with.bounds.min.y, with.bounds.max.x, with.bounds.max.y), (-100.0, -50.0, 100.0, 50.0));
    }
}
