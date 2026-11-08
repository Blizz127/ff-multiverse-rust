//! The battle camera: a look-at pose in battle space and the pinhole view of one window.
//!
//! # Frames
//!
//! * **Battle space** is the game's: right-handed, `x`/`z` the ground, **`y` points down** with the
//!   ground at `y = 0` (so up is negative and a flying enemy has a negative `y`). In a normal
//!   battle the enemies stand at negative `z` and the party at positive `z`.
//! * **Camera space** is the field camera's ([`crate::avatar::stage`]): `x` right, `y` DOWN, `z`
//!   forward (the distance in front of the camera). Right-handed too, so `right = down x forward`.
//!   Models are skinned straight into it on the CPU and the GPU only applies the pinhole divide
//!   ([`crate::avatar::gpu`]).
//!
//! # From a pose to a window
//!
//! A [`BattleCamera`] is a position and the point it looks at (what the formation's camera data
//! holds; there is no roll). A [`View`] adds what a window needs: the focal length in pixels
//! (`zoom`) and a lens shift that moves the picture up a little to leave the bottom of the window
//! to the menus. [`fit`] chooses both so that the whole cast is in the window whatever its aspect
//! ratio, but never zooms in beyond [`NOMINAL_TAN_HALF_V`].

/// A point or vector, `f64` because the camera stands thousands of units away.
pub type V3 = [f64; 3];

/// `tan(vertical field of view / 2)` of the nominal lens: the PlayStation battle screen is 240 lines
/// high and its projection distance is taken to be 512 (an ASSUMPTION about the original, not read
/// from the game files: the formation data holds a position and a target but no lens), giving a
/// vertical field of view of about 26 degrees. [`fit`] only ever widens it.
pub const NOMINAL_TAN_HALF_V: f64 = 120.0 / 512.0;
/// The widest `tan(vertical field of view / 2)` [`fit`] will use (a vertical field of view of
/// about 60 degrees): a cast that does not fit even then is cut off rather than drawn through a
/// fish-eye.
pub const MAX_TAN_HALF_V: f64 = 0.58;
/// Nothing nearer than this in front of the camera is drawn by the stage, and a camera that has
/// part of the cast nearer than [`MIN_CAST_DEPTH`] is not used.
pub const STAGE_NEAR: f64 = 1200.0;
/// A cast point must be at least this far in front of the camera for a pose to be usable.
pub const MIN_CAST_DEPTH: f64 = 1500.0;

/// Margins of the area the cast is fitted into, as fractions of the window's width and height.
pub const MARGIN_SIDE: f64 = 0.07;
pub const MARGIN_TOP: f64 = 0.10;
/// The bottom of the window is left to the command menu and the party's panel.
pub const MARGIN_BOTTOM: f64 = 0.24;

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalized(a: V3) -> Option<V3> {
    let len = dot(a, a).sqrt();
    (len.is_finite() && len > 1e-9).then(|| [a[0] / len, a[1] / len, a[2] / len])
}

/// A camera in battle space: where it stands and which way it looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BattleCamera {
    position: V3,
    /// The camera's axes in battle space (unit vectors): the rows of the world -> camera rotation.
    right: V3,
    down: V3,
    forward: V3,
}

impl BattleCamera {
    /// A camera standing at `position` and looking at `target`, upright (the screen's up is the
    /// battle's up). `None` when either is not finite or they are less than a unit apart. Looking
    /// straight down or up (no horizon to keep level) puts battle `-z` at the top of the screen.
    pub fn look_at(position: V3, target: V3) -> Option<Self> {
        if !position.iter().chain(target.iter()).all(|c| c.is_finite()) {
            return None;
        }
        let forward = normalized(sub(target, position))?;
        // The screen's "down" is the battle's down (+y) made perpendicular to the view direction.
        let world_down = [0.0, 1.0, 0.0];
        let mut down = {
            let along = dot(world_down, forward);
            [world_down[0] - along * forward[0], world_down[1] - along * forward[1], world_down[2] - along * forward[2]]
        };
        let degenerate = normalized(down).is_none() || dot(world_down, forward).abs() > 0.999_999;
        if degenerate {
            // Straight down/up: any ground direction will do; +z is the bottom of the screen.
            let reference = [0.0, 0.0, 1.0];
            let along = dot(reference, forward);
            down = [reference[0] - along * forward[0], reference[1] - along * forward[1], reference[2] - along * forward[2]];
        }
        let down = normalized(down)?;
        let right = normalized(cross(down, forward))?;
        Some(Self { position, right, down, forward })
    }

    pub fn position(&self) -> V3 {
        self.position
    }

    /// The direction the camera looks in.
    pub fn forward(&self) -> V3 {
        self.forward
    }

    /// A point of battle space in camera space.
    pub fn to_camera(self, p: V3) -> V3 {
        let rel = sub(p, self.position);
        [dot(self.right, rel), dot(self.down, rel), dot(self.forward, rel)]
    }

    /// The world -> camera transform as a column-major 4x4 (what [`crate::avatar::frame::skin`] takes).
    pub fn matrix(&self) -> [f64; 16] {
        let (r, d, f) = (self.right, self.down, self.forward);
        let t = [-dot(r, self.position), -dot(d, self.position), -dot(f, self.position)];
        [r[0], d[0], f[0], 0.0, r[1], d[1], f[1], 0.0, r[2], d[2], f[2], 0.0, t[0], t[1], t[2], 1.0]
    }

    /// Where the horizon is, as the camera-space direction of "straight ahead on the ground"
    /// (`None` when looking straight down or up).
    fn horizon_direction(&self) -> Option<V3> {
        let ahead = normalized([self.forward[0], 0.0, self.forward[2]])?;
        Some([dot(self.right, ahead), dot(self.down, ahead), dot(self.forward, ahead)])
    }
}

/// A camera as one window sees it: the lens and the lens shift. Everything drawn in a battle
/// (stage, models, marks on the ground) goes through the one `View` of the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub camera: BattleCamera,
    pub width: u32,
    pub height: u32,
    /// Focal length in pixels: a point at camera-space `(x, y, z)` lands `zoom * x / z` pixels to the
    /// right of and `zoom * y / z` pixels below the lens axis.
    pub zoom: f64,
    /// Where the lens axis lands in the window, in pixels right of and ABOVE the window's centre.
    pub shift: [f64; 2],
}

impl View {
    /// The pinhole coordinates of a camera-space point: pixels right of and above the lens axis
    /// (the "view space" of the marks on the ground, which the GPU's camera shifts into place).
    /// `None` at or behind `min_z`.
    pub fn pinhole(&self, c: V3, min_z: f64) -> Option<[f32; 2]> {
        (c[2] >= min_z).then(|| [(self.zoom * c[0] / c[2]) as f32, (-self.zoom * c[1] / c[2]) as f32])
    }

    /// Normalised device coordinates (`+y` up) of a camera-space point in front of `min_z`.
    pub fn ndc(&self, c: V3, min_z: f64) -> Option<[f32; 2]> {
        let [u, v] = self.pinhole(c, min_z)?;
        let (hw, hh) = (f64::from(self.width.max(1)) * 0.5, f64::from(self.height.max(1)) * 0.5);
        Some([((f64::from(u) + self.shift[0]) / hw) as f32, ((f64::from(v) + self.shift[1]) / hh) as f32])
    }

    /// Window pixels from the top left of a point of battle space; `None` when it is not in front
    /// of the camera.
    pub fn to_pixels(self, p: V3) -> Option<[f32; 2]> {
        let c = self.camera.to_camera(p);
        let [u, v] = self.pinhole(c, 1.0)?;
        let (hw, hh) = (f64::from(self.width.max(1)) * 0.5, f64::from(self.height.max(1)) * 0.5);
        Some([(hw + f64::from(u) + self.shift[0]) as f32, (hh - f64::from(v) - self.shift[1]) as f32])
    }

    /// The same, as normalised device coordinates (tests).
    #[cfg(test)]
    pub fn to_ndc(self, p: V3) -> Option<[f32; 2]> {
        self.ndc(self.camera.to_camera(p), 1.0)
    }

    /// The window's vertical position of the horizon in NDC (`+y` up), `None` when there is none
    /// (looking straight down).
    pub fn horizon_ndc_y(&self) -> Option<f32> {
        let c = self.camera.horizon_direction()?;
        if c[2] <= 1e-9 {
            return None;
        }
        let hh = f64::from(self.height.max(1)) * 0.5;
        Some(((-self.zoom * c[1] / c[2] + self.shift[1]) / hh) as f32)
    }

    /// `tan(vertical field of view / 2)` of this lens (tests).
    #[cfg(test)]
    pub fn tan_half_v(&self) -> f64 {
        f64::from(self.height.max(1)) * 0.5 / self.zoom
    }

    /// The camera the GPU draws the models and the marks with: view space is pinhole pixels and the
    /// lens shift moves them into place ([`crate::render::Camera::pixels`]).
    pub fn gpu_camera(&self) -> crate::render::Camera {
        crate::render::Camera::pixels(self.shift[0] as f32, self.shift[1] as f32)
    }
}

/// The lens for `camera` in a `width` x `height` window that puts every one of `cast` (battle-space
/// points: the feet, heads and sides of everyone) inside the window's margins: the nominal focal
/// length ([`NOMINAL_TAN_HALF_V`]) unless the cast does not fit, then the longest that makes it fit
/// (down to [`MAX_TAN_HALF_V`]); and the least lens shift that moves the cast into the margins.
/// `None` when part of the cast is closer than [`MIN_CAST_DEPTH`] to the camera's plane (the pose
/// does not suit this cast: the caller uses another).
pub fn fit(camera: BattleCamera, width: u32, height: u32, cast: &[V3]) -> Option<View> {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let mut pinholes: Vec<[f64; 2]> = Vec::with_capacity(cast.len());
    for &p in cast {
        let c = camera.to_camera(p);
        if !c.iter().all(|v| v.is_finite()) || c[2] < MIN_CAST_DEPTH {
            return None;
        }
        pinholes.push([c[0] / c[2], -c[1] / c[2]]);
    }
    let nominal = h * 0.5 / NOMINAL_TAN_HALF_V;
    let widest = h * 0.5 / MAX_TAN_HALF_V;
    // The area the cast must land in, in pixels right of / above the window's centre.
    let (left, right) = (-w * 0.5 + w * MARGIN_SIDE, w * 0.5 - w * MARGIN_SIDE);
    let (bottom, top) = (-h * 0.5 + h * MARGIN_BOTTOM, h * 0.5 - h * MARGIN_TOP);
    let mut zoom = nominal;
    if let Some(((u0, u1), (v0, v1))) = extent(&pinholes) {
        // The zoom at which the cast's extent exactly fills the area, if it is wider than the area.
        let fit_u = (right - left) / (u1 - u0).max(1e-9);
        let fit_v = (top - bottom) / (v1 - v0).max(1e-9);
        zoom = nominal.min(fit_u).min(fit_v).max(widest);
        // The least shift that brings it inside (none when it already is).
        let shift_for = |lo: f64, hi: f64, min: f64, max: f64| {
            if lo + 0.0 < min {
                min - lo
            } else if hi > max {
                max - hi
            } else {
                0.0
            }
        };
        let shift = [shift_for(u0 * zoom, u1 * zoom, left, right), shift_for(v0 * zoom, v1 * zoom, bottom, top)];
        return Some(View { camera, width, height, zoom, shift });
    }
    Some(View { camera, width, height, zoom, shift: [0.0, 0.0] })
}

/// `((min u, max u), (min v, max v))` of some pinhole points (`None` for none).
fn extent(points: &[[f64; 2]]) -> Option<((f64, f64), (f64, f64))> {
    let first = points.first()?;
    let mut e = ((first[0], first[0]), (first[1], first[1]));
    for p in points {
        e.0 = (e.0 .0.min(p[0]), e.0 .1.max(p[0]));
        e.1 = (e.1 .0.min(p[1]), e.1 .1.max(p[1]));
    }
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// A camera on the +x side, raised, looking at a point between the sides: the shape of the
    /// game's usual battle camera, with round numbers of our own.
    fn side_camera() -> BattleCamera {
        BattleCamera::look_at([12_000.0, -3_600.0, 3_000.0], [0.0, -300.0, 0.0]).unwrap()
    }

    fn view(w: u32, h: u32) -> View {
        let zoom = f64::from(h) * 0.5 / NOMINAL_TAN_HALF_V;
        View { camera: side_camera(), width: w, height: h, zoom, shift: [0.0, 0.0] }
    }

    #[test]
    fn the_axes_are_unit_perpendicular_and_right_handed() {
        for cam in [side_camera(), BattleCamera::look_at([0.0, -5000.0, 9000.0], [0.0, 0.0, -300.0]).unwrap()] {
            let (r, d, f) = (cam.right, cam.down, cam.forward);
            for v in [r, d, f] {
                assert!(near(dot(v, v), 1.0, 1e-9));
            }
            assert!(dot(r, d).abs() < 1e-9 && dot(r, f).abs() < 1e-9 && dot(d, f).abs() < 1e-9);
            // right x down = forward (a right-handed x, y, z).
            let rd = cross(r, d);
            for k in 0..3 {
                assert!(near(rd[k], f[k], 1e-9), "{rd:?} vs {f:?}");
            }
            // The camera is upright: its "down" has a downward (positive y) battle component.
            assert!(d[1] > 0.0);
        }
    }

    #[test]
    fn the_target_is_on_the_lens_axis_and_the_camera_is_at_the_origin_of_camera_space() {
        let cam = side_camera();
        let c = cam.to_camera([0.0, -300.0, 0.0]);
        assert!(near(c[0], 0.0, 1e-6) && near(c[1], 0.0, 1e-6), "{c:?}");
        let dist = (12_000f64.powi(2) + 3_300f64.powi(2) + 3_000f64.powi(2)).sqrt();
        assert!(near(c[2], dist, 1e-6), "{c:?}");
        assert_eq!(cam.to_camera(cam.position()), [0.0, 0.0, 0.0]);
        // The matrix form agrees with the direct one.
        let m = cam.matrix();
        for p in [[10.0, -20.0, 30.0], [-5000.0, 0.0, 2500.0]] {
            let direct = cam.to_camera(p);
            let via = [
                m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
                m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
                m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
            ];
            for k in 0..3 {
                assert!(near(direct[k], via[k], 1e-6));
            }
        }
    }

    /// The side of the screen the two sides of a battle land on, seen from the +x side of the
    /// field: the party at positive z is right of centre, the enemies at negative z left of it.
    #[test]
    fn from_the_positive_x_side_positive_z_is_on_the_right_and_negative_z_on_the_left() {
        let v = view(1100, 680);
        let party = v.to_pixels([0.0, 0.0, 2000.0]).unwrap();
        let enemy = v.to_pixels([0.0, 0.0, -2000.0]).unwrap();
        assert!(party[0] > 550.0 + 200.0, "{party:?}");
        assert!(enemy[0] < 550.0 - 200.0, "{enemy:?}");
    }

    #[test]
    fn up_is_negative_y_and_nearer_is_lower_on_the_screen() {
        let v = view(1100, 680);
        let ground = v.to_pixels([0.0, 0.0, 0.0]).unwrap();
        let flying = v.to_pixels([0.0, -1500.0, 0.0]).unwrap();
        assert!(flying[1] < ground[1], "a point above the ground is higher up: {flying:?} {ground:?}");
        // A camera looking down: of two ground points the one nearer the camera (larger x here) is lower.
        let near_point = v.to_pixels([4000.0, 0.0, 0.0]).unwrap();
        let far_point = v.to_pixels([-4000.0, 0.0, 0.0]).unwrap();
        assert!(near_point[1] > far_point[1], "{near_point:?} {far_point:?}");
        // The lens axis hits the window's centre without a shift.
        let target = v.to_pixels([0.0, -300.0, 0.0]).unwrap();
        assert!(near(f64::from(target[0]), 550.0, 0.01) && near(f64::from(target[1]), 340.0, 0.01), "{target:?}");
    }

    #[test]
    fn pixels_and_ndc_agree_and_the_shift_moves_everything_up() {
        let mut v = view(800, 600);
        let p = [500.0, -200.0, 700.0];
        let (px, ndc) = (v.to_pixels(p).unwrap(), v.to_ndc(p).unwrap());
        assert!(near(f64::from(px[0]), (f64::from(ndc[0]) + 1.0) * 400.0, 1e-3));
        assert!(near(f64::from(px[1]), (1.0 - f64::from(ndc[1])) * 300.0, 1e-3));
        v.shift = [0.0, 60.0];
        let up = v.to_pixels(p).unwrap();
        assert!(near(f64::from(px[1] - up[1]), 60.0, 1e-3), "shifted up by 60 px: {px:?} -> {up:?}");
        assert!(near(f64::from(px[0]), f64::from(up[0]), 1e-3));
        // Behind the camera there is nothing to draw.
        assert!(v.to_pixels(v.camera.position()).is_none());
        let behind = [24_000.0, -3_600.0, 6_000.0];
        assert!(v.to_pixels(behind).is_none());
    }

    #[test]
    fn the_gpu_camera_lands_a_pinhole_point_where_to_pixels_does() {
        // The avatar shader's arithmetic on the CPU, as in `avatar::gpu::projection_terms`.
        let mut v = view(1100, 680);
        v.shift = [12.0, 40.0];
        let cam = v.gpu_camera();
        let p = [1500.0, -400.0, -1200.0];
        let c = v.camera.to_camera(p);
        let pin = v.pinhole(c, 1.0).unwrap();
        let view_space = cam.project(pin[0], pin[1]);
        let (hw, hh) = (550.0f32, 340.0f32);
        let via_gpu = [hw + view_space[0], hh - view_space[1]];
        let direct = v.to_pixels(p).unwrap();
        assert!((via_gpu[0] - direct[0]).abs() < 1e-3 && (via_gpu[1] - direct[1]).abs() < 1e-3, "{via_gpu:?} vs {direct:?}");
    }

    #[test]
    fn degenerate_poses_are_refused_or_made_upright() {
        assert!(BattleCamera::look_at([1.0, 2.0, 3.0], [1.0, 2.0, 3.0]).is_none(), "no direction");
        assert!(BattleCamera::look_at([f64::NAN, 0.0, 0.0], [0.0; 3]).is_none());
        assert!(BattleCamera::look_at([0.0; 3], [0.0, f64::INFINITY, 0.0]).is_none());
        // Straight down: still a proper frame, with a ground point ahead of the lens axis at the top.
        let down = BattleCamera::look_at([0.0, -9000.0, 0.0], [0.0, 0.0, 0.0]).unwrap();
        let (r, d, f) = (down.right, down.down, down.forward);
        assert!(near(dot(r, d), 0.0, 1e-9) && near(dot(d, f), 0.0, 1e-9));
        let c = down.to_camera([0.0, 0.0, -1000.0]);
        assert!(c[1] < 0.0, "battle -z is the top of the screen when looking straight down: {c:?}");
        // Straight up, too.
        assert!(BattleCamera::look_at([0.0, 0.0, 0.0], [0.0, -5000.0, 0.0]).is_some());
        // No horizon when looking straight down.
        let v = View { camera: down, width: 800, height: 600, zoom: 600.0, shift: [0.0, 0.0] };
        assert!(v.horizon_ndc_y().is_none());
    }

    #[test]
    fn the_horizon_is_above_the_lens_axis_when_looking_down() {
        let v = view(1100, 680);
        let y = v.horizon_ndc_y().unwrap();
        assert!(y > 0.0, "looking down puts the horizon above the middle: {y}");
        // Looking level, it is at the middle.
        let level = View { camera: BattleCamera::look_at([5000.0, -300.0, 0.0], [0.0, -300.0, 0.0]).unwrap(), ..v };
        assert!(level.horizon_ndc_y().unwrap().abs() < 1e-5);
        // And moves up with the lens shift.
        let shifted = View { shift: [0.0, 68.0], ..level };
        assert!(near(f64::from(shifted.horizon_ndc_y().unwrap()), 0.2, 1e-5));
    }

    /// Feet and heads of two sides, in the shape of a normal formation.
    fn cast() -> Vec<V3> {
        let mut points = Vec::new();
        for (x, z) in [(-500.0, -1500.0), (500.0, -1700.0), (-1000.0, 1800.0), (0.0, 2000.0), (1000.0, 2200.0)] {
            for y in [0.0, -1200.0] {
                for dz in [-300.0, 300.0] {
                    points.push([x, y, z + dz]);
                }
            }
        }
        points
    }

    fn inside(v: &View, points: &[V3]) -> bool {
        points.iter().all(|&p| {
            let px = v.to_pixels(p).unwrap();
            px[0] >= 0.0 && px[0] <= v.width as f32 && px[1] >= 0.0 && px[1] <= v.height as f32
        })
    }

    #[test]
    fn a_cast_that_fits_keeps_the_nominal_lens_and_one_that_does_not_is_widened() {
        let near_camera = BattleCamera::look_at([4500.0, -1200.0, 600.0], [0.0, -300.0, 100.0]).unwrap();
        let far_camera = BattleCamera::look_at([16_000.0, -4000.0, 6000.0], [0.0, -300.0, 100.0]).unwrap();
        let points = cast();
        let far = fit(far_camera, 1100, 680, &points).unwrap();
        let nominal = 680.0 * 0.5 / NOMINAL_TAN_HALF_V;
        assert!(near(far.zoom, nominal, 1e-6), "a distant camera keeps the nominal lens: {}", far.zoom);
        assert!(inside(&far, &points));
        let close = fit(near_camera, 1100, 680, &points).unwrap();
        assert!(close.zoom < nominal, "a close camera is widened: {} vs {nominal}", close.zoom);
        assert!(close.tan_half_v() <= MAX_TAN_HALF_V + 1e-9);
        assert!(inside(&close, &points), "and the whole cast is in the window");
    }

    #[test]
    fn the_cast_stays_out_of_the_margins_at_any_window_shape_and_the_bottom_is_left_free() {
        let camera = BattleCamera::look_at([9000.0, -2400.0, 1800.0], [0.0, -350.0, 200.0]).unwrap();
        let points = cast();
        for (w, h) in [(1100u32, 680u32), (640, 480), (1920, 1080), (800, 800), (1600, 600)] {
            let Some(v) = fit(camera, w, h, &points) else { panic!("{w}x{h}: no fit") };
            let low = points.iter().map(|&p| v.to_pixels(p).unwrap()[1]).fold(f32::MIN, f32::max);
            assert!(f64::from(low) <= f64::from(h) * (1.0 - MARGIN_BOTTOM) + 0.5, "{w}x{h}: lowest point at {low}");
            let (left, right) = points.iter().map(|&p| v.to_pixels(p).unwrap()[0]).fold((f32::MAX, f32::MIN), |a, x| (a.0.min(x), a.1.max(x)));
            assert!(f64::from(left) >= f64::from(w) * MARGIN_SIDE - 0.5 && f64::from(right) <= f64::from(w) * (1.0 - MARGIN_SIDE) + 0.5, "{w}x{h}: {left}..{right}");
            let high = points.iter().map(|&p| v.to_pixels(p).unwrap()[1]).fold(f32::MAX, f32::min);
            assert!(f64::from(high) >= f64::from(h) * MARGIN_TOP - 0.5, "{w}x{h}: highest point at {high}");
        }
    }

    #[test]
    fn a_pose_with_the_cast_behind_or_next_to_it_is_refused() {
        let points = cast();
        // Looking away from the cast.
        let away = BattleCamera::look_at([4000.0, -1000.0, 0.0], [20_000.0, -1000.0, 0.0]).unwrap();
        assert!(fit(away, 800, 600, &points).is_none());
        // Standing inside the cast.
        let inside_cast = BattleCamera::look_at([0.0, -500.0, 0.0], [0.0, -500.0, -5000.0]).unwrap();
        assert!(fit(inside_cast, 800, 600, &points).is_none());
        // An empty cast is fitted to the nominal lens, unshifted.
        let v = fit(side_camera(), 800, 600, &[]).unwrap();
        assert_eq!(v.shift, [0.0, 0.0]);
        // A zero-sized window does not divide by zero.
        let z = fit(side_camera(), 0, 0, &points);
        assert!(z.is_none_or(|v| v.zoom.is_finite() && v.shift.iter().all(|s| s.is_finite())));
    }
}
