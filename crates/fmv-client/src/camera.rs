//! The field camera: which part of a field the window shows.
//!
//! FF7 never shows a whole field at once. Its screen is a 320x240 window into the background (field-screen units; the
//! PC game doubles them when presenting), centred on the player and kept inside the field's **camera range** (the
//! triggers section, [`fmv_ff7::background::BackgroundImage::view_bounds`]): a field whose range is no bigger than the
//! screen does not scroll at all. Story scenes move it: `SCR2D` puts it at a place at once, `SCR2DC` and `SCR2DL` scroll
//! it there over some frames, `SCRLA` follows an actor and `SCRCC` gives it back to the party's leader. This module
//! reproduces that, in the renderer's view space (+y up, so a field-screen y is negated):
//!
//! * **Out of scenes** the view is centred on the player's own character, as it is drawn this frame.
//! * **In a scene** it follows what the scene says: a scroll to field-screen `(x, y)` centres the view on `(-x, -y)`
//!   (measured on the owner's data: `mds7pb_1`'s `SCR2D 64,-83` centres it on Cloud, who stands at `(-64, 84)`; `md1_1`'s
//!   `SCR2DC -96,60` brings the team at `(111..178, -32..-81)` into view), run over the frames on the stage's own clock
//!   ([`crate::scene`]'s camera clock: a window that pauses the scene pauses the scroll); a follow tracks that cast
//!   member. When the scene says nothing, or gives the camera back, the view follows the scene's **lead**, the canon
//!   Cloud when he is in its cast (the party's leader, whom FF7's own camera follows: in the reactor's finale the team
//!   runs off with him), else the player.
//! * Changing whom it follows (a scene starting or ending, a follow, the scene giving the camera back) eases from where
//!   the view was over [`BLEND_SECONDS`]; a new area, or the first frame, puts it in place at once.
//!
//! The window is wider than FF7's 4:3 (1100x680 by default). The view keeps FF7's [`VIEW_HEIGHT`] of 240 units from the
//! top of the window to its bottom and shows more of the field to the sides where the camera range has it, never past
//! the range: what lies outside it was never meant to be seen. (The renderer fits what it is given inside a margin; the
//! rectangle handed to it is the view less that margin, so the margin shows the view's own edge and the whole window is
//! the view.) The centre is rounded to whole field pixels, so the nearest-filtered art does not shimmer while the view
//! moves.
//!
//! **F2** switches to the whole field at once (the overview the window showed before this camera) and back. Areas
//! without a camera range (the top-down FIXTURE rooms) are always shown whole.

use fmv_adapter_api::Bounds;
use fmv_core::Pos;

/// The height of the view in field-screen units: FF7's screen.
pub const VIEW_HEIGHT: f32 = 240.0;

/// The narrowest view, FF7's own 320 units (a window narrower than 4:3 shows black above and below instead).
pub const MIN_VIEW_WIDTH: f32 = 320.0;

/// How long the view takes to go from one subject to another.
pub const BLEND_SECONDS: f32 = 0.5;

/// What the window shows of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraMode {
    /// FF7's camera (see the module docs).
    #[default]
    Follow,
    /// The whole field at once (F2).
    Whole,
}

/// What a story scene asks of the camera this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneShot {
    /// The scene's run: a new run is a new account of the camera.
    pub run: u64,
    /// How many camera events the scene has had: one more is a new instruction, even one that repeats the last.
    pub events: u32,
    pub aim: SceneAim,
}

/// Where a scene points the camera. Positions are in view space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SceneAim {
    /// Nothing asked, or the camera was given back: follow the scene's lead when it is drawn, else the player.
    Lead { lead: Option<[f32; 2]> },
    /// Follow a cast member; `at` is `None` when it is not drawn (then as [`SceneAim::Lead`]).
    Follow { at: Option<[f32; 2]>, lead: Option<[f32; 2]> },
    /// Scroll to centre the view on `centre` over `frames` of scene time, of which `elapsed` went by (0: at once).
    Scroll { centre: [f32; 2], frames: u32, elapsed: f32 },
}

/// Everything the camera needs to know about a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraInput {
    /// The area the view may scroll over (view space), `None` when the area has none (then it is shown whole).
    pub range: Option<Bounds>,
    /// The rectangle of the whole area (what [`CameraMode::Whole`] shows).
    pub whole: Bounds,
    /// The window's size in pixels.
    pub window: [f32; 2],
    /// The margin (pixels) the renderer keeps round the rectangle it is given ([`crate::render::MARGIN_PX`]).
    pub margin: f32,
    /// Where the player's own character is drawn this frame (view space).
    pub own: Option<[f32; 2]>,
    /// What a story scene asks, while one runs.
    pub scene: Option<SceneShot>,
    /// Authored field-screen displacement, applied after clamping, without moving the base camera.
    pub shake: [f32; 2],
    /// Seconds since the last frame.
    pub dt: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Blend {
    from: [f32; 2],
    t: f32,
}

/// The window's field camera (see the module docs). One per window; [`FieldCamera::snap`] when the area changes.
#[derive(Debug, Clone, Default)]
pub struct FieldCamera {
    mode: CameraMode,
    /// The centre shown last (view space, before rounding), `None` before the first frame of an area.
    shown: Option<[f32; 2]>,
    /// The scene, camera event and kind of aim the view last followed (`None` out of scenes).
    scene: Option<(u64, u32, u8)>,
    /// Where the scroll in progress started.
    scroll_from: Option<[f32; 2]>,
    blend: Option<Blend>,
}

impl FieldCamera {
    /// Follow mode keeps actors and markers inside the authored camera range. The whole-field
    /// overview keeps its original extent; invalid ranges never turn into an accidental mask.
    pub fn clip_range(&self, range: Option<Bounds>) -> Option<Bounds> {
        range.filter(|range| self.mode == CameraMode::Follow && sound(range))
    }

    /// F2: the other mode. The view is put in place at once.
    pub fn toggle(&mut self) -> CameraMode {
        self.mode = match self.mode {
            CameraMode::Follow => CameraMode::Whole,
            CameraMode::Whole => CameraMode::Follow,
        };
        self.snap();
        self.mode
    }

    /// Forget where the view was (a new area): the next frame puts it in place at once.
    pub fn snap(&mut self) {
        self.shown = None;
        self.scene = None;
        self.scroll_from = None;
        self.blend = None;
    }

    /// The rectangle (view space) to show this frame.
    pub fn update(&mut self, input: &CameraInput) -> Bounds {
        let range = match (self.mode, input.range) {
            (CameraMode::Follow, Some(range)) if sound(&range) => range,
            _ => {
                self.snap();
                return shaken(input.whole, input.shake);
            }
        };
        let [vw, vh] = input.window;
        let aspect = if vw.is_finite() && vh.is_finite() && vh > 0.0 { vw / vh } else { f32::NAN };
        let size = view_size(&range, aspect);
        let clamp = |c: [f32; 2]| clamp_centre(c, &range, size);
        let middle = [(range.min.x + range.max.x) / 2.0, (range.min.y + range.max.y) / 2.0];
        let own = input.own.filter(|p| finite(*p));
        let previous = self.shown;

        // A new instruction of the scene (or a scene starting or ending) changes whom the view follows. So does a change of
        // the kind of aim without a new instruction: the end of a scene clears its stage (the camera is the field's again)
        // while its player may still be on show.
        let key = input.scene.map(|s| (s.run, s.events, aim_kind(&s.aim)));
        if key != self.scene {
            self.scene = key;
            self.scroll_from = None;
            self.blend = None;
            match input.scene.map(|s| s.aim) {
                Some(SceneAim::Scroll { .. }) => self.scroll_from = previous,
                _ => self.blend = previous.map(|from| Blend { from, t: 0.0 }),
            }
        }
        let target = match input.scene.map(|s| s.aim) {
            None => own,
            Some(SceneAim::Lead { lead }) => lead.filter(|p| finite(*p)).or(own),
            Some(SceneAim::Follow { at, lead }) => at.filter(|p| finite(*p)).or(lead.filter(|p| finite(*p))).or(own),
            Some(SceneAim::Scroll { centre, frames, elapsed }) if finite(centre) => {
                let to = clamp(centre);
                let from = self.scroll_from.unwrap_or(to);
                let p = if frames == 0 { 1.0 } else { (elapsed / frames as f32).clamp(0.0, 1.0) };
                let p = if p.is_finite() { p } else { 1.0 };
                Some(lerp(from, to, smooth(p)))
            }
            Some(SceneAim::Scroll { .. }) => own,
        };
        let target = clamp(target.or(previous).unwrap_or(middle));
        let centre = match self.blend.as_mut() {
            Some(blend) => {
                let dt = if input.dt.is_finite() { input.dt.max(0.0) } else { 0.0 };
                blend.t += dt;
                let k = smooth((blend.t / BLEND_SECONDS).min(1.0));
                let c = lerp(blend.from, target, k);
                if blend.t >= BLEND_SECONDS {
                    self.blend = None;
                }
                c
            }
            None => target,
        };
        self.shown = Some(centre);
        let [cx, cy] = [centre[0].round(), centre[1].round()];
        // The renderer fits the rectangle inside its margin and shows the margin round it: hand it the view less the
        // margin's share of each axis, so that the whole window is the view (`render::Camera::fit` keeps the shape).
        let inset = |pixels: f32| {
            let m = if input.margin.is_finite() { input.margin.max(0.0) } else { 0.0 };
            if pixels.is_finite() && pixels > 2.0 * m { 1.0 - 2.0 * m / pixels } else { 1.0 }
        };
        let (hw, hh) = (size[0] / 2.0 * inset(vw), size[1] / 2.0 * inset(vh));
        shaken(Bounds { min: Pos::new(cx - hw, cy - hh, 0.0), max: Pos::new(cx + hw, cy + hh, 0.0) }, input.shake)
    }
}

fn shaken(mut bounds: Bounds, [x, y]: [f32; 2]) -> Bounds {
    if finite([x, y]) {
        // The engine adds screen offsets after clamping. View Y points upward, so moving
        // the picture right/down translates its camera centre left/up. Do not reclamp.
        bounds.min.x -= x;
        bounds.max.x -= x;
        bounds.min.y += y;
        bounds.max.y += y;
    }
    bounds
}

fn aim_kind(aim: &SceneAim) -> u8 {
    match aim {
        SceneAim::Lead { .. } => 0,
        SceneAim::Follow { .. } => 1,
        SceneAim::Scroll { .. } => 2,
    }
}

fn finite(p: [f32; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

fn sound(b: &Bounds) -> bool {
    [b.min.x, b.min.y, b.max.x, b.max.y].iter().all(|v| v.is_finite()) && b.max.x > b.min.x && b.max.y > b.min.y
}

/// The size of the view (view units): FF7's height, as wide as the window's shape asks, at least FF7's own width and
/// no wider than the range (when the range is at least that wide). The width is an even number of units, so the
/// centre's whole-pixel steps keep the edges on the pixel grid.
pub fn view_size(range: &Bounds, aspect: f32) -> [f32; 2] {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { MIN_VIEW_WIDTH / VIEW_HEIGHT };
    let wide = (VIEW_HEIGHT * aspect).max(MIN_VIEW_WIDTH);
    let range_w = range.max.x - range.min.x;
    let w = wide.min(range_w.max(MIN_VIEW_WIDTH));
    [2.0 * (w / 2.0).round(), VIEW_HEIGHT]
}

/// Keep a view of `size` centred at `c` inside `range`: along an axis the range is no longer than the view, the view
/// is centred on the range (FF7 does not scroll that way).
pub fn clamp_centre(c: [f32; 2], range: &Bounds, size: [f32; 2]) -> [f32; 2] {
    let axis = |c: f32, lo: f32, hi: f32, view: f32| -> f32 {
        if hi - lo <= view {
            (lo + hi) / 2.0
        } else {
            c.clamp(lo + view / 2.0, hi - view / 2.0)
        }
    };
    [axis(c[0], range.min.x, range.max.x, size[0]), axis(c[1], range.min.y, range.max.y, size[1])]
}

/// A field-screen rectangle (y down) as a view-space one (y up).
pub fn screen_rect_to_view(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> Bounds {
    Bounds { min: Pos::new(min_x as f32, -(max_y as f32), 0.0), max: Pos::new(max_x as f32, -(min_y as f32), 0.0) }
}

/// Where a scene's scroll to field-screen `(x, y)` centres the view, in view space: the field-screen centre is
/// `(-x, -y)`, and view space negates y.
pub fn scroll_centre(x: i16, y: i16) -> [f32; 2] {
    [-f32::from(x), f32::from(y)]
}

fn lerp(a: [f32; 2], b: [f32; 2], k: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k]
}

/// Ease in and out over 0..1.
fn smooth(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Bounds {
        Bounds { min: Pos::new(min_x, min_y, 0.0), max: Pos::new(max_x, max_y, 0.0) }
    }

    /// md1stin's range (512x512, centred) in view space.
    fn big() -> Bounds {
        screen_rect_to_view(-256, -256, 256, 256)
    }

    fn input(range: Option<Bounds>, own: Option<[f32; 2]>, scene: Option<SceneShot>) -> CameraInput {
        CameraInput { range, whole: rect(-300.0, -300.0, 300.0, 300.0), window: [640.0, 480.0], margin: 0.0, own, scene, shake: [0.0; 2], dt: 1.0 / 60.0 }
    }

    #[test]
    fn authored_shake_moves_the_picture_after_clamping_without_changing_the_base_camera() {
        let mut cam = FieldCamera::default();
        let mut i = input(Some(big()), Some([400.0, 400.0]), None);
        let base = cam.update(&i);
        i.shake = [3.0, -5.0];
        let moved = cam.update(&i);
        assert_eq!([moved.min.x - base.min.x, moved.min.y - base.min.y], [-3.0, -5.0]);
        assert_eq!([moved.max.x - base.max.x, moved.max.y - base.max.y], [-3.0, -5.0]);
        i.shake = [0.0; 2];
        assert_eq!(cam.update(&i), base, "shake never feeds the follow easing or clamps");
        i.shake = [f32::NAN, 9.0];
        assert_eq!(cam.update(&i), base, "invalid offsets cannot poison projection");
        cam.toggle();
        i.shake = [3.0, -5.0];
        assert_eq!(cam.update(&i), shaken(i.whole, i.shake), "overview uses the same authored displacement");
    }

    fn centre(b: &Bounds) -> [f32; 2] {
        [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0]
    }

    fn size(b: &Bounds) -> [f32; 2] {
        [b.max.x - b.min.x, b.max.y - b.min.y]
    }

    #[test]
    fn out_of_scenes_the_view_is_ff7s_screen_centred_on_the_player_and_kept_inside_the_range() {
        let mut cam = FieldCamera::default();
        let shown = cam.update(&input(Some(big()), Some([10.4, -20.6]), None));
        assert_eq!(size(&shown), [320.0, 240.0], "4:3 shows FF7's own 320x240");
        assert_eq!(centre(&shown), [10.0, -21.0], "on the player, in whole field pixels");
        // At a corner of the range the view stops at its edges: x within +-96, y within +-136.
        let corner = cam.update(&input(Some(big()), Some([400.0, -400.0]), None));
        assert_eq!(centre(&corner), [96.0, -136.0]);
        assert_eq!((corner.max.x, corner.min.y), (256.0, -256.0), "the view ends at the range's edge, never past it");
    }

    #[test]
    fn an_asymmetric_range_keeps_its_orientation_through_the_y_flip() {
        // Field-screen y grows downwards: a range from y -40 (top) to 400 (bottom) is view y -400..40.
        let range = screen_rect_to_view(-160, -40, 480, 400);
        assert_eq!((range.min.y, range.max.y), (-400.0, 40.0));
        let mut cam = FieldCamera::default();
        // A player at the top of the field (view y +30) sees the top of the range: the view is pushed down to y -80.
        let top = cam.update(&input(Some(range), Some([0.0, 30.0]), None));
        assert_eq!(centre(&top), [0.0, -80.0]);
        assert_eq!(top.max.y, 40.0);
        // One at the bottom right sees that corner.
        let low = cam.update(&input(Some(range), Some([1000.0, -1000.0]), None));
        assert_eq!(centre(&low), [320.0, -280.0]);
    }

    #[test]
    fn a_range_no_bigger_than_the_screen_does_not_scroll() {
        let small = screen_rect_to_view(-160, -120, 160, 120);
        let mut cam = FieldCamera::default();
        for own in [[0.0, 0.0], [150.0, 100.0], [-150.0, -100.0]] {
            let shown = cam.update(&input(Some(small), Some(own), None));
            assert_eq!((centre(&shown), size(&shown)), ([0.0, 0.0], [320.0, 240.0]));
        }
        // md8_1's 384x240 range scrolls only sideways.
        let wide = screen_rect_to_view(-192, -120, 192, 120);
        let shown = cam.update(&input(Some(wide), Some([500.0, 500.0]), None));
        assert_eq!(centre(&shown), [32.0, 0.0]);
    }

    #[test]
    fn the_rectangle_given_to_the_renderer_leaves_room_for_its_margin_so_the_whole_window_is_the_view() {
        use crate::render::Camera;
        let mut cam = FieldCamera::default();
        let window = CameraInput { window: [1100.0, 680.0], margin: 24.0, ..input(Some(big()), Some([400.0, 400.0]), None) };
        let given = cam.update(&window);
        // The view is 240 units from the window's top to its bottom and as wide as the window's shape: 388 of md1stin's 512.
        let fit = Camera::fit(&given, 1100.0, 680.0, 24.0);
        let units = |px: f32| px / fit.pixels_per_unit();
        assert!((units(680.0) - 240.0).abs() < 0.01, "{}", units(680.0));
        assert!((units(1100.0) - 388.0).abs() < 0.5, "{}", units(1100.0));
        // At the range's top right corner the window ends exactly at the range's edges: no black beyond them.
        let [cx, cy] = centre(&given);
        assert!((cx + units(1100.0) / 2.0 - 256.0).abs() < 0.5 && (cy + units(680.0) / 2.0 - 256.0).abs() < 0.5, "{given:?}");
    }

    #[test]
    fn a_wide_window_shows_more_of_the_field_sideways_but_never_past_the_range() {
        assert_eq!(view_size(&big(), 1100.0 / 680.0), [388.0, 240.0]);
        assert_eq!(view_size(&big(), 3.0), [512.0, 240.0], "as wide as the range at most");
        assert_eq!(view_size(&screen_rect_to_view(-160, -120, 160, 120), 2.0), [320.0, 240.0]);
        assert_eq!(view_size(&big(), 0.5), [320.0, 240.0], "a tall window keeps FF7's width");
        assert_eq!(view_size(&big(), f32::NAN), [320.0, 240.0]);
        assert_eq!(view_size(&screen_rect_to_view(-100, -100, 100, 100), 2.0), [320.0, 240.0], "a range narrower than the screen is shown with black at the sides, as in FF7");
    }

    #[test]
    fn a_scene_scroll_lands_where_the_owners_data_puts_it_and_runs_on_the_scene_clock() {
        // mds7pb_1: SCR2D 64,-83 centres the view on Cloud at field-screen (-64, 84): view (-64, -84) give or take one.
        assert_eq!(scroll_centre(64, -83), [-64.0, -83.0]);
        let shot = |events: u32, frames: u32, elapsed: f32| Some(SceneShot { run: 1, events, aim: SceneAim::Scroll { centre: scroll_centre(64, -83), frames, elapsed } });
        let mut cam = FieldCamera::default();
        cam.update(&input(Some(big()), Some([0.0, 0.0]), None));
        // Over 60 frames: nothing at the start, halfway at frame 30, there at 60 and after.
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), shot(1, 60, 0.0)))), [0.0, 0.0]);
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), shot(1, 60, 30.0)))), [-32.0, -42.0]);
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), shot(1, 60, 60.0)))), [-64.0, -83.0]);
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), shot(1, 60, 500.0)))), [-64.0, -83.0]);
        // The same scroll again is a new instruction: it starts from where the view is (there already: it stays).
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), shot(2, 60, 0.0)))), [-64.0, -83.0]);
        // An instant scroll is there at once, and the target is kept inside the range.
        let far = Some(SceneShot { run: 1, events: 3, aim: SceneAim::Scroll { centre: scroll_centre(-400, 400), frames: 0, elapsed: 0.0 } });
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([0.0, 0.0]), far))), [96.0, 136.0]);
    }

    #[test]
    fn a_scene_follows_its_lead_or_the_actor_it_names_and_gives_the_view_back_gently() {
        let mut cam = FieldCamera::default();
        let own = Some([-50.0, 0.0]);
        cam.update(&input(Some(big()), own, None));
        // The scene starts with Cloud at (50, 0): the view eases over to him.
        let lead = |events: u32| Some(SceneShot { run: 7, events, aim: SceneAim::Lead { lead: Some([50.0, 0.0]) } });
        let mut last = [0.0, 0.0];
        for _ in 0..10 {
            last = centre(&cam.update(&input(Some(big()), own, lead(0))));
        }
        assert!(last[0] > -50.0 && last[0] < 0.0, "under way after a sixth of a second, not there at once: {last:?}");
        for _ in 0..40 {
            last = centre(&cam.update(&input(Some(big()), own, lead(0))));
        }
        assert_eq!(last, [50.0, 0.0], "on the lead after the blend");
        // A follow of another actor, then of one that is not drawn (back to the lead), then a scene without Cloud.
        let follow = |at: Option<[f32; 2]>| Some(SceneShot { run: 7, events: 1, aim: SceneAim::Follow { at, lead: Some([50.0, 0.0]) } });
        let mut seen = [0.0, 0.0];
        for _ in 0..40 {
            seen = centre(&cam.update(&input(Some(big()), own, follow(Some([20.0, 60.0])))));
        }
        assert_eq!(seen, [20.0, 60.0]);
        let nobody = Some(SceneShot { run: 8, events: 0, aim: SceneAim::Lead { lead: None } });
        for _ in 0..40 {
            seen = centre(&cam.update(&input(Some(big()), own, nobody)));
        }
        assert_eq!(seen, [-50.0, 0.0], "a scene without its lead drawn follows the player");
        // The scene ends: back on the player (eased, then there).
        for _ in 0..40 {
            seen = centre(&cam.update(&input(Some(big()), Some([-70.0, 10.0]), None)));
        }
        assert_eq!(seen, [-70.0, 10.0]);
    }

    #[test]
    fn a_scene_whose_stage_is_cleared_without_a_new_instruction_still_eases_back() {
        let mut cam = FieldCamera::default();
        let own = Some([-60.0, 0.0]);
        cam.update(&input(Some(big()), own, None));
        let scroll = Some(SceneShot { run: 3, events: 1, aim: SceneAim::Scroll { centre: [60.0, 0.0], frames: 0, elapsed: 0.0 } });
        assert_eq!(centre(&cam.update(&input(Some(big()), own, scroll))), [60.0, 0.0]);
        // The stage is cleared (the camera is the field's again) with the same count of instructions: not a cut.
        let cleared = Some(SceneShot { run: 3, events: 1, aim: SceneAim::Lead { lead: None } });
        let next = centre(&cam.update(&input(Some(big()), own, cleared)));
        assert!(next[0] > 55.0, "eases from the scroll's place: {next:?}");
        let mut seen = next;
        for _ in 0..40 {
            seen = centre(&cam.update(&input(Some(big()), own, cleared)));
        }
        assert_eq!(seen, [-60.0, 0.0]);
    }

    #[test]
    fn the_whole_field_is_shown_on_request_or_without_a_range_and_bad_input_poisons_nothing() {
        let mut cam = FieldCamera::default();
        let whole = input(None, Some([0.0, 0.0]), None).whole;
        assert_eq!(cam.update(&input(None, Some([0.0, 0.0]), None)), whole, "a FIXTURE room has no range");
        assert_eq!(cam.toggle(), CameraMode::Whole);
        assert_eq!(cam.update(&input(Some(big()), Some([0.0, 0.0]), None)), whole);
        assert_eq!(cam.toggle(), CameraMode::Follow);
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([5.0, 5.0]), None))), [5.0, 5.0]);
        // No player and no scene: the view stays where it was; on a fresh area it is the range's middle.
        assert_eq!(centre(&cam.update(&input(Some(big()), None, None))), [5.0, 5.0]);
        cam.snap();
        assert_eq!(centre(&cam.update(&input(Some(big()), Some([f32::NAN, 1.0]), None))), [0.0, 0.0]);
        let broken = Some(rect(0.0, 0.0, f32::INFINITY, 1.0));
        assert_eq!(cam.update(&input(broken, Some([0.0, 0.0]), None)), whole);
        let bad = CameraInput { window: [f32::NAN, 0.0], margin: f32::NAN, dt: f32::NAN, ..input(Some(big()), Some([1.0, 1.0]), Some(SceneShot { run: 1, events: 1, aim: SceneAim::Scroll { centre: [f32::NAN, 0.0], frames: 10, elapsed: f32::NAN } })) };
        let shown = cam.update(&bad);
        assert!([shown.min.x, shown.min.y, shown.max.x, shown.max.y].iter().all(|v| v.is_finite()), "{shown:?}");
    }
}
