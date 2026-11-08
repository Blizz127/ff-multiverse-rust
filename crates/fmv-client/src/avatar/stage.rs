//! What drawing characters in an FF7 field needs to know about the field: its camera, how big a
//! model is in it, how fast a character moves in it, and its scenery as depth-tested tiles (what
//! hides a character and what does not).
//!
//! [`crate::backdrop::FieldView::ff7`] builds the [`Stage`] next to the picture and the walkmesh
//! view; it is the only place that reads these field facts. Nothing here is sent anywhere.

use super::scenery::Scenery;
use fmv_ff7::field::Camera;
use std::sync::Arc;

/// The field camera as a world -> camera-space transform in `f64` (the same camera record
/// [`fmv_ff7::background::Projection`] projects the walkmesh and the picture with, so models and
/// scenery agree to the pixel).
///
/// ```text
/// cam = R * world + T          sx = cam.x * zoom / cam.z      sy = cam.y * zoom / cam.z
/// ```
///
/// `cam.z` is the distance in front of the camera. Models are skinned straight into camera space
/// on the CPU (so the large translation of the camera never meets an `f32` vertex position) and
/// the GPU only applies the perspective divide and the window fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageCamera {
    rows: [[f64; 3]; 3],
    translation: [f64; 3],
    pub zoom: f64,
}

impl StageCamera {
    pub fn from_camera(camera: &Camera) -> Self {
        Self {
            rows: camera.axes_f32().map(|row| row.map(f64::from)),
            translation: camera.position.map(f64::from),
            zoom: f64::from(camera.zoom),
        }
    }

    /// A point of field space in camera space.
    pub fn to_camera(self, p: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0; 3];
        for ((slot, row), t) in out.iter_mut().zip(self.rows.iter()).zip(self.translation.iter()) {
            *slot = row[0] * p[0] + row[1] * p[1] + row[2] * p[2] + t;
        }
        out
    }

    /// Field-screen position (x right, y DOWN) and depth of a field-space point; `None` at or
    /// behind the camera plane, like `Projection::project`.
    pub fn project(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let [x, y, z] = self.to_camera(p);
        (z >= 1.0).then(|| [x * self.zoom / z, y * self.zoom / z, z])
    }

    /// The world -> camera transform as a column-major 4x4.
    pub fn matrix(&self) -> [f64; 16] {
        let r = &self.rows;
        let t = &self.translation;
        [
            r[0][0], r[1][0], r[2][0], 0.0, //
            r[0][1], r[1][1], r[2][1], 0.0, //
            r[0][2], r[1][2], r[2][2], 0.0, //
            t[0], t[1], t[2], 1.0,
        ]
    }
}

/// Facts about the field the characters are drawn in.
#[derive(Debug, Clone)]
pub struct Stage {
    pub camera: StageCamera,
    /// Walkmesh units per model unit ([`fmv_ff7::default_model_scale`] of the field's own scale).
    pub model_scale: f32,
    /// Walking and running speed in walkmesh units per second (`Ff7FieldArea::walk_speed` and
    /// `run_speed`: hypotheses used for pacing and prediction only; the server's cap is the
    /// area's `max_speed`).
    pub walk_speed: f32,
    pub run_speed: f32,
    /// The field's picture as depth-tested tiles ([`Scenery`]), drawn instead of the flat picture
    /// when the characters are models: a tile nearer to the camera than a character hides it and a
    /// farther one does not, as in the game. `None` when the characters are to be drawn over the
    /// whole flat picture: nothing in front of them (no tile of layer 2 or 4), or tiles that do not
    /// fit an atlas.
    pub scenery: Option<Arc<Scenery>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::background::Projection;

    fn camera() -> Camera {
        Camera { axes: [[4096, 0, 0], [0, -3000, 2800], [0, 2800, 3000]], position: [-120, 4200, 9500], zoom: 380 }
    }

    #[test]
    fn the_stage_camera_projects_exactly_as_the_background_projection_does() {
        let cam = camera();
        let (stage, reference) = (StageCamera::from_camera(&cam), Projection::from_camera(&cam));
        for p in [[0.0f32, 0.0, 0.0], [1500.0, -300.0, 40.0], [-2200.0, 900.0, 122.0], [30.0, 7000.0, -10.0]] {
            let want = reference.project(p);
            let got = stage.project(p.map(f64::from));
            match (want, got) {
                (Some(w), Some(g)) => {
                    assert!((f64::from(w.x) - g[0]).abs() < 1e-3, "{p:?}: {w:?} {g:?}");
                    assert!((f64::from(w.y) - g[1]).abs() < 1e-3, "{p:?}: {w:?} {g:?}");
                    assert!((f64::from(w.depth) - g[2]).abs() < 1e-2, "{p:?}: {w:?} {g:?}");
                }
                (None, None) => {}
                other => panic!("{p:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_matrix_form_agrees_with_the_direct_one_and_points_behind_the_camera_are_not_projected() {
        let stage = StageCamera::from_camera(&camera());
        let m = stage.matrix();
        for p in [[10.0, 20.0, 30.0], [-5000.0, 100.0, 7.0]] {
            let direct = stage.to_camera(p);
            let via = [
                m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
                m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
                m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
            ];
            for k in 0..3 {
                assert!((direct[k] - via[k]).abs() < 1e-9);
            }
        }
        assert_eq!(m[3], 0.0);
        assert_eq!(m[15], 1.0);
        // A camera looking away from the point: nothing to draw.
        let away = Camera { axes: [[4096, 0, 0], [0, 4096, 0], [0, 0, -4096]], position: [0, 0, 0], zoom: 300 };
        assert_eq!(StageCamera::from_camera(&away).project([0.0, 0.0, 100.0]), None);
    }
}
