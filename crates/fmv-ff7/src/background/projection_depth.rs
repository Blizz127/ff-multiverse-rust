//! The PC field's perspective depth mapping, calibrated from its baked tile depths.
//!
//! PC models use a homogeneous projection, not the PlayStation GTE's saturated perspective
//! divider. The PC reconstruction's `C_0067CCDE` calls `C_0067C3AE`, whose matrix gives
//! `z / w = far / (far - near) - far * near / ((far - near) * camera_z)`. The functions are in
//! `dx_mat.obj` of the pinned reconstruction's `FF7LIB.lib`:
//! <https://github.com/ergonomy-joe/ff7-coaster/blob/825f4baf194d0a8fbed372be68eb56acf32a967c/FF7LIB.lib>.
//! The named COFF function and relocation records, inspected without executing the library,
//! establish the formula; this is a reconstruction, not an observation of the original executable.
//!
//! Independently, FFNx forwards the original field's near/far values to that projection function:
//! <https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/background.cpp#L604>.
//! Its field loader's reduction of near planes greater than 50 to 50 is an FFNx modification,
//! not the original field rule. Makou Reactor's reversing notes name exceptions, including
//! map 138 (`cargoin`) at 16 and map 139 (`tin_1`) at 30:
//! <https://github.com/myst6re/makoureactor/blob/2452025714c033d698d1754a776bdcd5713427bd/src/core/field/BackgroundTiles.cpp#L36>.
//!
//! [`calibrate`] fits `IDBig / 10_000_000 = A - B / (4 * id)` to a field's static layer-2 tiles.
//! `4 * id` approximates their camera depth (see [`super::depth`]); fixed-point reconstruction
//! creates small differences between tiles with the same id. Consequently these coefficients
//! are a validated calibration, not exact recovered native constants. Distinct depths have equal
//! weight, independent ids validate the fit, and every supplied usable tile is checked afterward.
//! Insufficient or inconsistent evidence retains the existing 50/32_000 fallback.
//!
//! A model vertex uses **unclamped** clip-space `z = A * camera_z - B`, `w = camera_z`, so the GPU
//! clips triangles at the near and far planes. Background foreground-marker tiles are different:
//! their depth remains clamped to the front of the buffer; they must not be geometrically culled.

use super::depth::{ENGINE_FAR_PLANE, ENGINE_NEAR_PLANE, TILE_DEPTH_PER_ID};
use std::collections::BTreeMap;

/// A perspective mapping `buffer_depth = a - b / camera_depth`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepthMapping {
    pub a: f32,
    pub b: f32,
    /// The camera-space clipping distances implied by the coefficients.
    pub near: f32,
    pub far: f32,
    /// True only when independent tile depths and every usable tile passed validation.
    pub calibrated: bool,
}

impl Default for DepthMapping {
    fn default() -> Self {
        let near = ENGINE_NEAR_PLANE;
        let far = ENGINE_FAR_PLANE;
        let (n, f) = (f64::from(near), f64::from(far));
        Self { a: (f / (f - n)) as f32, b: (f * n / (f - n)) as f32, near, far, calibrated: false }
    }
}

// A layer's count is u16. Bound even callers other than the field parser before collecting data.
const MAX_SAMPLES: usize = u16::MAX as usize;
const MIN_IDS: usize = 6;
// Very near tile depths amplify the fixed-point camera reconstruction error. The existing
// owned-data depth survey fits ids >= 14; lower ids still have to pass the all-tile check below.
const MIN_FIT_ID: u16 = 14;
const ID_SCALE: f64 = 10_000_000.0;
// Each fitting/validation half must cover at least 10% of its nearest inverse depth.
const MIN_RELATIVE_SPAN: f64 = 0.1;
const MAX_ID_RMS: f64 = 1e-4;
const MAX_ID_ERROR: f64 = 5e-4;
const MAX_TILE_ERROR: f64 = 1e-3;

/// Calibrate from `(id, id_big)` of **static layer-2** tiles (`layer == 2 && param == 0`).
/// Callers filter the layer/group; no archive data, names or global state are needed here.
///
/// Foreground markers and reserved far ids are excluded. Repeated ids are represented by their
/// median, so repeating a tile cannot outweigh independent depths. Ids below 14 amplify the
/// fixed-point camera error and are reserved for the final tile check (absolute error <= 1e-3).
/// Alternating remaining sorted ids form fitting and validation sets; only after both pass do
/// those ids contribute to the final fit (RMS <= 1e-4, each median's absolute error <= 5e-4).
/// Any unusable fit returns [`DepthMapping::default`], never partial coefficients.
pub fn calibrate(samples: impl IntoIterator<Item = (u16, u32)>) -> DepthMapping {
    try_calibrate(samples).unwrap_or_default()
}

fn try_calibrate(samples: impl IntoIterator<Item = (u16, u32)>) -> Option<DepthMapping> {
    let mut by_id: BTreeMap<u16, Vec<u32>> = BTreeMap::new();
    for (count, (id, big)) in samples.into_iter().enumerate() {
        if count >= MAX_SAMPLES {
            return None;
        }
        // Zero is foreground; the high ids are reserved/background sentinels, not calibration.
        if id > 0 && id < 4090 {
            by_id.entry(id).or_default().push(big);
        }
    }
    // The native foreground marker is 10000 * id (up to two units of stored rounding).
    // Do not let any other tile of that same foreground id participate in a perspective fit.
    by_id.retain(|&id, values| !values.iter().any(|&v| v.abs_diff(10_000 * u32::from(id)) <= 2));
    if by_id.len() < MIN_IDS || by_id.values().flatten().any(|&v| v == 0 || f64::from(v) > ID_SCALE) {
        return None;
    }
    let mut ids = Vec::with_capacity(by_id.len());
    for (&id, values) in by_id.iter_mut().filter(|(id, _)| **id >= MIN_FIT_ID) {
        values.sort_unstable();
        ids.push((inverse_depth(id), f64::from(*values.get(values.len() / 2)?) / ID_SCALE));
    }
    if ids.len() < MIN_IDS {
        return None;
    }
    let fit_ids: Vec<_> = ids.iter().step_by(2).copied().collect();
    let held_ids: Vec<_> = ids.iter().skip(1).step_by(2).copied().collect();
    if !spans_depths(&fit_ids) || !spans_depths(&held_ids) {
        return None;
    }
    let first = fit(&fit_ids)?;
    mapping(first)?;
    if !valid_ids(first, &fit_ids) || !valid_ids(first, &held_ids) {
        return None;
    }
    let all = fit(&ids)?;
    let result = mapping(all)?;
    if !valid_ids(all, &ids) {
        return None;
    }
    // A median can hide one damaged tile. Validate every tile, including duplicates, separately.
    for (&id, values) in &by_id {
        let predicted = all.0 - all.1 * inverse_depth(id);
        if values.iter().any(|&v| (f64::from(v) / ID_SCALE - predicted).abs() > MAX_TILE_ERROR) {
            return None;
        }
    }
    Some(result)
}

fn inverse_depth(id: u16) -> f64 {
    1.0 / (f64::from(TILE_DEPTH_PER_ID) * f64::from(id))
}

fn spans_depths(points: &[(f64, f64)]) -> bool {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return false };
    points.len() >= MIN_IDS / 2 && first.0 - last.0 >= MIN_RELATIVE_SPAN * first.0
}

// Mean-centering avoids subtracting two large, almost equal sums for distant tile depths.
fn fit(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let count = points.len() as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / count;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / count;
    let variance = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum::<f64>();
    if !variance.is_finite() || variance <= 0.0 {
        return None;
    }
    let covariance = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum::<f64>();
    let b = -covariance / variance;
    let a = mean_y + b * mean_x;
    (a.is_finite() && b.is_finite()).then_some((a, b))
}

fn valid_ids((a, b): (f64, f64), points: &[(f64, f64)]) -> bool {
    let mut squared = 0.0;
    for &(x, y) in points {
        let error = y - (a - b * x);
        if !error.is_finite() || error.abs() > MAX_ID_ERROR {
            return false;
        }
        squared += error * error;
    }
    !points.is_empty() && squared / points.len() as f64 <= MAX_ID_RMS * MAX_ID_RMS
}

fn mapping((a, b): (f64, f64)) -> Option<DepthMapping> {
    if !(a.is_finite() && b.is_finite() && a > 1.0 && b > 0.0) {
        return None;
    }
    let near = b / a;
    let far = b / (a - 1.0);
    // The known native near1 must survive small fixed-point/fit error. Keep the lower margin.
    // A far plane must contain the background's reserved 4096 * 4 depth. Broad upper bounds are
    // rejection guards, not guesses at another field's native settings.
    if !(near.is_finite() && far.is_finite() && (0.5..=1024.0).contains(&near) && (16_384.0..=10_000_000.0).contains(&far)) {
        return None;
    }
    let (a, b) = (a as f32, b as f32);
    // Do not lose the finite far plane when the GPU's f32 rounds a very small (A - 1) to zero.
    if a <= 1.0 || !a.is_finite() || !b.is_finite() {
        return None;
    }
    Some(DepthMapping { a, b, near: near as f32, far: far as f32, calibrated: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(near: f64, far: f64) -> Vec<(u16, u32)> {
        let (a, b) = (far / (far - near), far * near / (far - near));
        [150, 190, 250, 350, 500, 750, 1000, 1500, 2500, 3500].into_iter()
            .map(|id| (id, ((a - b * inverse_depth(id)) * ID_SCALE).round() as u32)).collect()
    }

    #[test]
    fn the_pc_depth_mapping_recovers_native_near_values_including_cargo_and_train() {
        for near in [1.0, 16.0, 30.0, 50.0, 512.0] {
            let got = calibrate(samples(near, 32_000.0));
            assert!(got.calibrated, "near {near}: {got:?}");
            assert!((f64::from(got.near) - near).abs() < 0.001, "{got:?}");
            assert!((got.far / 32_000.0 - 1.0).abs() < 0.01, "{got:?}");
            assert!((got.a * got.near - got.b).abs() < 0.0001);
            assert!((got.a - got.b / got.far - 1.0).abs() < 1e-6);
            assert!(got.a * (got.near * 0.5) - got.b < 0.0, "a model before near is clipped");
        }
    }

    #[test]
    fn repeated_tiles_do_not_outweigh_independent_depths_and_order_does_not_matter() {
        let data = samples(30.0, 32_000.0);
        let expected = calibrate(data.clone());
        let mut repeated = data.clone();
        repeated.extend(std::iter::repeat_n(data[0], 1000));
        repeated.reverse();
        assert_eq!(calibrate(repeated), expected);
    }

    #[test]
    fn foreground_markers_and_reserved_far_ids_do_not_distort_the_fit() {
        let mut data = samples(16.0, 32_000.0);
        let expected = calibrate(data.clone());
        data.extend([(0, 999), (1, 10_000), (5, 49_999), (10, 100_002), (4095, 9_998_999), (u16::MAX, u32::MAX)]);
        assert_eq!(calibrate(data), expected);
    }

    #[test]
    fn independent_held_out_ids_must_agree_before_the_full_fit() {
        let mut data = samples(30.0, 32_000.0);
        // The fitting half is perfect; only ids that were not used for it are inconsistent.
        for (_, big) in data.iter_mut().skip(1).step_by(2) {
            *big += 6_000;
        }
        assert!(data.iter().all(|(_, big)| *big < ID_SCALE as u32), "valid stored depths, inconsistent held-out ids");
        assert_eq!(calibrate(data), DepthMapping::default());
    }

    #[test]
    fn a_single_bad_tile_cannot_hide_behind_its_ids_median() {
        let mut data = samples(16.0, 32_000.0);
        data.extend(std::iter::repeat_n(data[0], 20));
        data.push((data[0].0, data[0].1 + 50_000));
        assert_eq!(calibrate(data), DepthMapping::default());
    }

    #[test]
    fn rounding_sensitive_near_tiles_are_validated_without_biasing_the_fit() {
        let mut data = samples(16.0, 32_000.0);
        let expected = calibrate(data.clone());
        let a = 32_000.0 / (32_000.0 - 16.0);
        let b = 16.0 * a;
        let near_tile = ((a - b * inverse_depth(7)) * ID_SCALE).round() as u32;
        data.push((7, near_tile + 8_000));
        assert_eq!(calibrate(data.clone()), expected, "a close tile's fixed-point error is bounded");
        data.push((7, near_tile + 20_000));
        assert_eq!(calibrate(data), DepthMapping::default(), "near tiles are still checked");
    }

    #[test]
    fn insufficient_degenerate_invalid_and_excessive_samples_keep_the_previous_fallback() {
        let fallback = DepthMapping::default();
        assert!(!fallback.calibrated);
        assert_eq!((fallback.near, fallback.far), (ENGINE_NEAR_PLANE, ENGINE_FAR_PLANE));
        assert_eq!(calibrate([]), fallback);
        assert_eq!(calibrate(samples(30.0, 32_000.0).into_iter().take(5)), fallback);
        assert_eq!(calibrate(std::iter::repeat_n((150, 9_000_000), 100)), fallback);
        assert_eq!(calibrate((4000..4010).map(|id| (id, 9_000_000))), fallback);
        assert_eq!(calibrate((100..110).map(|id| (id, 9_000_000))), fallback);
        let mut invalid = samples(30.0, 32_000.0);
        invalid[0].1 = u32::MAX;
        assert_eq!(calibrate(invalid), fallback);
        assert_eq!(calibrate(std::iter::repeat_n((0, 999), MAX_SAMPLES + 1)), fallback);
    }

    #[test]
    fn nonphysical_planes_and_nonfinite_internal_fits_are_refused() {
        assert!(mapping((f64::NAN, 1.0)).is_none());
        assert!(mapping((1.01, f64::INFINITY)).is_none());
        assert!(mapping((1.0, 30.0)).is_none());
        assert!(mapping((1.01, -30.0)).is_none());
        assert!(mapping((2.0, 30.0)).is_none(), "far must enclose the background");
        assert!(mapping((1.0 + 5.8e-8, 0.50000006)).is_none(), "f32 must retain the finite far plane");
        assert!(fit(&[]).is_none());
    }
}
