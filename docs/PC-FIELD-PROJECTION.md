# PC field projection and near clipping

Status: calibration, renderer and affected GPU checks passed on 2026-11-10.
This note records the evidence and limits separately from claims of
original-runtime parity.

## Evidence

The PC reconstruction's `C_0067CCDE` forwards its projection arguments to
`C_0067C3AE`. The named functions in `dx_mat.obj`, inspected with their COFF
relocations, construct a homogeneous perspective matrix whose divided depth is
`A - B / camera_z`, with `A = far / (far - near)` and
`B = near * far / (far - near)`. Its common matrix scale cancels in the divide.
This supports ordinary homogeneous clipping; it does not support importing the
PlayStation GTE's saturated perspective division into the PC renderer.
[Pinned reconstruction library](https://github.com/ergonomy-joe/ff7-coaster/blob/825f4baf194d0a8fbed372be68eb56acf32a967c/FF7LIB.lib).

This is reconstruction evidence, not an observation of the owner's original PC
executable. The library was inspected without executing or linking it. A raw
decompiler view with unresolved relocations was unsuitable; the named COFF
symbols, assembly and relocation records supplied the matrix derivation.

FFNx independently forwards the field's original near/far values to the same
projection function. Its later reduction of near planes greater than 50 to 50
is an FFNx modification, so it cannot establish a universal vanilla near plane.
[Projection call](https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/background.cpp#L604),
[field-loader modification](https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/field.cpp#L246).

Makou Reactor's original-engine reversing notes identify field 138 at near 16,
139 at near 30, 253 at near 56, and 466 at near 1. The owned archive's map list
identifies these as `cargoin`, `tin_1`, `blin66_4`, and `mtcrl_8`. Its separate
note about early fields using 512 is explicitly uncertain. These annotations
corroborate field-specific planes; they are not runtime measurements by this
project. [Pinned reversing notes](https://github.com/myst6re/makoureactor/blob/2452025714c033d698d1754a776bdcd5713427bd/src/core/field/BackgroundTiles.cpp#L36).

## Calibration and survey

`background::calibrate_depth_mapping` uses the PC tile record's stored `IDBig`
at offset 38 and its existing approximate camera depth `4 * id`. It fits
`IDBig / 10_000_000 = A - B / (4 * id)` from static layer-2 tiles. This is a
validated estimate from baked data, not recovery of exact native constants:
fixed-point camera reconstruction introduces differences between tiles with
the same id, and the far plane is particularly sensitive to small errors in A.

At least six distinct fitting ids are required. Their medians have equal
weight; alternating ids form independent fitting and validation sets, each
spanning at least 10% of its nearest inverse depth. Each set must have RMS
error at most `1e-4` and individual error at most `5e-4`. After refitting, every
usable raw tile must agree within `1e-3`, so a median cannot hide a damaged
duplicate. Quantization-sensitive ids below 14 are excluded from fitting but
remain in that raw validation. Foreground markers and reserved far ids are
excluded from the fit. Bounds, finite values, allocation limits and usable
GPU `f32` coefficients are checked before accepting a mapping.

A read-only survey of all 702 fields in the owner's FF7 Steam data produced:

| Outcome | Fields |
| --- | ---: |
| Validated calibration | 306 |
| Insufficient distinct ids | 307 |
| Insufficient depth span | 51 |
| Fitting or held-out residual too large | 35 |
| Raw tile outlier | 2 |
| Nonphysical fitted planes | 1 |

Representative estimates are cargo near **15.9858**, far **36172.1**; train
near **29.9918**, far **32326.6**; and `md1_1` near **50.0028**. Cargo and train
each supply 33 distinct fitting ids. `md1stin`, `blin66_4`, and `mtcrl_8` lack
enough usable evidence and retain fallback behavior. A separate check of every
layer-2 group in all 306 accepted fields found 52,511 recognized foreground
marker tiles; none has `4 * id` beyond the calibrated near plane.

The survey uses an independent read-only archive/parser script implementing
the same fit and thresholds. Its local audit files are under
`/tmp/fmv-pc-near-reconstruction/`: `survey_calibration.py`,
`calibration-survey.json`, `calibration-survey.log`, and
`foreground-marker-audit.log`. These files contain measurements rather than
redistributed game assets.

## Render behavior and limits

For calibrated fields, model vertices use `clip.z = A * camera_z - B` and
`clip.w = camera_z`, allowing the GPU to clip triangles before perspective
division. Models, depth-tested tiles and ground marks share the same A/B.
Screen-space foreground tiles retain their clamped front depth. Model decals
clip at their geometric depth, then apply their depth-only offset to surviving
fragments; biasing only inside vertices would incorrectly collapse some
near-crossing decals. Window projection, model scale and native HUD coordinates
are unchanged.

Uncalibrated fields retain both the previous near-50/far-32000 depth mapping
and its previous clamped model behavior. The fallback is not treated as a
known clipping plane: doing so would newly hide legitimate geometry in a field
such as `mtcrl_8`, whose documented near plane is 1. Battle and preview frames
also retain their existing projection policy.

This correction does **not** promise to remove the large cargo-door close-up.
The authored jump's frame-21 model vertices range from depth approximately
15.895 to 64.965, so cargo's inferred near plane clips only a sliver. The train
employee's inspected pose ranges from approximately 152.37 to 197.80; its size
is not a near-plane issue. Model scale remains unchanged, and complete native
model-loader scale parity has not been established. The existing CPU frame
builder also rejects a whole model when its anchor passes behind the camera;
triangle clipping does not resolve that separate visibility limitation.

## Verification

The eight `background::projection_depth::tests` passed in the coordinated FF7
library suite (706 passed, zero failed, eight ignored). They cover known
planes, repeated and reordered tiles, foreground markers, held-out corruption,
raw outliers, low-id quantization, insufficient data and invalid bounds.

Client checks passed on the coordinated rendering binary, using llvmpipe with
`LP_NUM_THREADS=2`:

- `field_calibration_changes_shared_depth_and_clip_policy_without_changing_screen_projection`:
  shared coefficients, unchanged framing and exact fallback uniforms.
- `offscreen_field_near_and_far_crossings_match_camera_space_clipping_before_division`:
  independent camera-space polygon clipping versus GPU pixels, including
  camera-crossing geometry and biased decals, native/2×/FXAA, unchanged HUD,
  and the old clamped path as a negative control.
- Existing depth, supersampling and authored cargo/train shots:
  **16 depth tests, four supersampling tests, one camera-range clipping test,
  and both cargo/train scene tests passed**. The two tests listed above also
  passed (one CPU invariant and one GPU clipping regression).

The cargo frame 14 and train dialogue frame 2 are pixel-identical to the
archived pre-change renders. Cargo frame 21 changes only 112 of 748,000 pixels;
its large close-up remains, as the depth measurements predict. The new
camera-crossing and biased-decal synthetic images, cargo frame 21 and train
dialogue frame 2 were visually inspected. This verifies the scoped clipping
change, not native scale or original-executable parity.

Rendering binary SHA-256:
`a6312ece0fb977656616c423eabd5b7c1438b5ad8caca88d702a270f40d4ec2d`.
Logs: `/tmp/fmv-pc-near-{gpu,depth-gpu,supersample-gpu,bars-gpu}.log`;
images: `/tmp/fmv-pc-near-shots/` and `/tmp/fmv-ofst-acceptance/`;
image hashes and comparison counts: `/tmp/fmv-pc-near-artifacts.txt`.
These checks cover the frozen rendering changes in that binary; later audio,
party and route changes require their own integration verification.
