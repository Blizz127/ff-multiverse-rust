use super::{compose_tile_pixels, RasterBackground, TilePixels, MAX_RASTER_BYTES, MAX_RASTER_SIDE};
use crate::background::depth::{pack_atlas, pack_tile_pixels, DepthTile, MAX_ATLAS_WIDTH};
use crate::background::{BlendMode, ScreenRect, MAX_CANVAS_SIDE};

fn rect(x: i32, y: i32, side: i32) -> ScreenRect {
    ScreenRect { min_x: x, min_y: y, max_x: x + side, max_y: y + side }
}

fn solid(side: u32, color: [u8; 4]) -> Vec<u8> {
    color.repeat((side * side) as usize)
}

fn tile(bounds: ScreenRect, blend: Option<BlendMode>) -> DepthTile {
    DepthTile {
        layer: 2, index: 7, id: 123, rect: bounds, depth: 492.0, blend, param: 3, state: 4,
        rgba: solid(bounds.width() as u32, [9, 19, 29, 255]),
    }
}

fn pixel(image: &RasterBackground, x: u32, y: u32) -> [u8; 4] {
    assert!(x < image.width && y < image.height);
    let start = ((y * image.width + x) * 4) as usize;
    image.rgba[start..start + 4].try_into().unwrap()
}

#[test]
fn extra_raster_detail_keeps_the_logical_origin_geometry_and_native_tile_unchanged() {
    let canvas = rect(-20, 12, 32);
    let tiles = [tile(rect(-12, 20, 16), None)];
    let before = tiles.clone();
    for scale in [1, 2, 4] {
        let side = 16 * scale;
        let rgba: Vec<_> = (0..side).flat_map(|y| (0..side).flat_map(move |x| [x as u8, y as u8, (x ^ y) as u8, 255])).collect();
        let pixels = [TilePixels { side, rgba: &rgba }];
        let image = compose_tile_pixels(&tiles, &pixels, canvas, scale).unwrap();
        assert_eq!(image.logical_bounds, canvas);
        assert_eq!((image.width, image.height), (32 * scale, 32 * scale));
        // An eight-unit border remains eight units at every raster density. Adjacent
        // high-resolution samples retain their individual colours, not enlarged native pixels.
        let offset = 8 * scale;
        for (x, y) in [(0, 0), (1, 2), (side - 1, side - 1)] {
            assert_eq!(pixel(&image, offset + x, offset + y), [x as u8, y as u8, (x ^ y) as u8, 255]);
        }
        assert_eq!(pixel(&image, offset - 1, offset), [0, 0, 0, 255]);
        assert_eq!(pixel(&image, offset + side, offset), [0, 0, 0, 255]);
        assert!(image.rgba.iter().skip(3).step_by(4).all(|alpha| *alpha == 255), "the flat fallback is opaque on black");
    }
    assert_eq!(tiles, before, "raster detail does not rewrite geometry, depth, groups, or native texels");
}

#[test]
fn flat_blending_and_atlas_cutouts_agree_at_the_alpha_threshold() {
    let canvas = rect(-8, -8, 16);
    let back = solid(64, [100, 120, 140, 255]);
    let mut front = solid(64, [80, 160, 240, 255]);
    for (x, alpha) in [0, 127, 128, 255].into_iter().enumerate() {
        front[x * 4 + 3] = alpha;
    }
    let pixels = [TilePixels { side: 64, rgba: &back }, TilePixels { side: 64, rgba: &front }];
    let atlas = pack_tile_pixels(&pixels, 128).unwrap();
    let at = atlas.rects[1];
    for (x, expected_alpha) in [0, 0, 255, 255].into_iter().enumerate() {
        let start = ((at.y as usize * atlas.width as usize) + at.x as usize + x) * 4;
        assert_eq!(&atlas.rgba[start..start + 4], &[80, 160, 240, expected_alpha]);
    }
    for (mode, expected) in [
        (None, [80, 160, 240, 255]),
        (Some(BlendMode::Average), [90, 140, 190, 255]),
        (Some(BlendMode::Additive), [180, 255, 255, 255]),
        (Some(BlendMode::Subtractive), [20, 0, 0, 255]),
        (Some(BlendMode::AddQuarter), [120, 160, 200, 255]),
    ] {
        let tiles = [tile(canvas, None), tile(canvas, mode)];
        let image = compose_tile_pixels(&tiles, &pixels, canvas, 4).unwrap();
        assert_eq!(pixel(&image, 0, 0), [100, 120, 140, 255], "{mode:?}: alpha zero");
        assert_eq!(pixel(&image, 1, 0), [100, 120, 140, 255], "{mode:?}: alpha 127");
        assert_eq!(pixel(&image, 2, 0), expected, "{mode:?}: alpha 128");
        assert_eq!(pixel(&image, 3, 0), expected, "{mode:?}: alpha 255");
    }
}

#[test]
fn native_atlas_layout_and_texels_are_unchanged_and_larger_rasters_keep_input_order() {
    let mut tiles = [tile(rect(0, 0, 16), None), tile(rect(16, 0, 32), None), tile(rect(48, 0, 16), None)];
    tiles[0].rgba[3] = 0;
    tiles[1].rgba[4..8].copy_from_slice(&[5, 15, 25, 255]);
    let pixels: Vec<_> = tiles.iter().map(|t| TilePixels { side: t.size(), rgba: &t.rgba }).collect();
    assert_eq!(pack_tile_pixels(&pixels, 96), pack_atlas(&tiles, 96));
    // Native packing remains a byte-preserving API even for an independently supplied
    // fractional alpha; only the replacement API clips it to the GPU threshold.
    tiles[0].rgba[3] = 127;
    let native = pack_atlas(&tiles, 96).unwrap();
    let at = native.rects[0];
    assert_eq!(native.rgba[((at.y * native.width + at.x) * 4 + 3) as usize], 127);
    let buffers = [(16, [1, 2, 3, 255]), (128, [4, 5, 6, 255]), (64, [7, 8, 9, 255]), (32, [10, 11, 12, 255])]
        .map(|(side, color)| (side, solid(side, color), color));
    let pixels: Vec<_> = buffers.iter().map(|(side, rgba, _)| TilePixels { side: *side, rgba }).collect();
    let atlas = pack_tile_pixels(&pixels, 256).unwrap();
    for ((side, _, color), at) in buffers.iter().zip(&atlas.rects) {
        assert_eq!(at.size, *side);
        for (x, y) in [(0, 0), (side - 1, side - 1)] {
            let start = (((at.y + y) * atlas.width + at.x + x) * 4) as usize;
            assert_eq!(&atlas.rgba[start..start + 4], color, "atlas entries still correspond to input tiles");
        }
    }
}

#[test]
fn malformed_raster_scale_length_and_tile_pairings_are_refused() {
    let canvas = rect(0, 0, 32);
    let tiles = [tile(rect(0, 0, 16), None)];
    let rgba = solid(16, [1, 2, 3, 255]);
    let pixels = [TilePixels { side: 16, rgba: &rgba }];
    for scale in [0, 3, 8, u32::MAX] {
        assert!(compose_tile_pixels(&tiles, &pixels, canvas, scale).is_err(), "scale {scale}");
    }
    assert!(compose_tile_pixels(&tiles, &[], canvas, 1).is_err());
    assert!(compose_tile_pixels(&[], &pixels, canvas, 1).is_err());
    assert!(compose_tile_pixels(&tiles, &pixels, canvas, 4).is_err(), "a native raster cannot masquerade as four times larger");
    for length in [0, rgba.len() - 1, rgba.len() + 1] {
        let bad = vec![255; length];
        let pixels = [TilePixels { side: 16, rgba: &bad }];
        assert!(compose_tile_pixels(&tiles, &pixels, canvas, 1).is_err(), "length {length}");
        assert!(pack_tile_pixels(&pixels, 128).is_none(), "length {length}");
    }
    for side in [0, 8, 17, 256, u32::MAX] {
        let pixels = [TilePixels { side, rgba: &rgba }];
        assert!(pack_tile_pixels(&pixels, 128).is_none(), "side {side}");
    }
    assert!(pack_tile_pixels(&pixels, 0).is_none());
    assert!(pack_tile_pixels(&pixels, MAX_ATLAS_WIDTH + 1).is_none());
    let larger = solid(128, [1, 2, 3, 255]);
    assert!(pack_tile_pixels(&[TilePixels { side: 128, rgba: &larger }], 64).is_none(), "one tile must fit the atlas width");
}

#[test]
fn out_of_canvas_and_invalid_logical_tiles_are_refused_instead_of_cropped() {
    let canvas = rect(0, 0, 16);
    let rgba = solid(16, [1, 2, 3, 255]);
    let pixels = [TilePixels { side: 16, rgba: &rgba }];
    for bounds in [rect(-1, 0, 16), rect(1, 0, 16), rect(0, -1, 16), rect(0, 1, 16)] {
        assert!(compose_tile_pixels(&[tile(bounds, None)], &pixels, canvas, 1).is_err(), "{bounds:?}");
    }
    let mut invalid = tile(canvas, None);
    for bounds in [rect(0, 0, 8), ScreenRect { min_x: 0, min_y: 0, max_x: 16, max_y: 15 }, rect(0, 0, 0),
        ScreenRect { min_x: i32::MIN, min_y: 0, max_x: i32::MAX, max_y: 16 }] {
        invalid.rect = bounds;
        assert!(compose_tile_pixels(std::slice::from_ref(&invalid), &pixels, canvas, 1).is_err(), "{bounds:?}");
    }
}

#[test]
fn canvas_limits_and_coordinate_overflow_fail_before_allocating_a_picture() {
    let bad_canvases = [
        (rect(0, 0, 0), 1),
        (ScreenRect { min_x: 10, min_y: 0, max_x: 0, max_y: 16 }, 1),
        (ScreenRect { min_x: i32::MIN, min_y: 0, max_x: i32::MAX, max_y: 16 }, 1),
        (rect(0, 0, MAX_CANVAS_SIDE + 1), 1),
        (ScreenRect { min_x: 0, min_y: 0, max_x: 16, max_y: (MAX_RASTER_SIDE / 4 + 1) as i32 }, 4),
        (rect(0, 0, 2048), 4), // legal side lengths, but 256 MiB of pixels exceeds the byte budget
    ];
    for (canvas, scale) in bad_canvases {
        assert!(compose_tile_pixels(&[], &[], canvas, scale).is_err(), "{canvas:?} at {scale}x");
    }
}

#[test]
fn atlas_budget_counts_both_borrowed_input_pixels_and_packing_padding() {
    let large = solid(128, [1, 2, 3, 255]);
    let small = solid(16, [4, 5, 6, 255]);
    let count = MAX_RASTER_BYTES / large.len();
    let too_many: Vec<_> = (0..=count).map(|_| TilePixels { side: 128, rgba: &large }).collect();
    assert!(pack_tile_pixels(&too_many, MAX_ATLAS_WIDTH).is_none(), "shared borrows still account for every tile's pixels");
    let canvas = rect(0, 0, 32);
    let tiles = vec![tile(canvas, None); too_many.len()];
    assert!(compose_tile_pixels(&tiles, &too_many, canvas, 4).is_err(), "even overlapping tiles count toward the input byte budget");
    // 1,023 large tiles leave a gap in the final 8,192-wide row; one small tile
    // starts another row. Input bytes fit, while the padded atlas exceeds 64 MiB.
    let mut padded: Vec<_> = (0..count - 1).map(|_| TilePixels { side: 128, rgba: &large }).collect();
    padded.push(TilePixels { side: 16, rgba: &small });
    assert!((count - 1) * large.len() + small.len() < MAX_RASTER_BYTES);
    assert!(pack_tile_pixels(&padded, MAX_ATLAS_WIDTH).is_none(), "atlas allocation includes unoccupied row pixels");
}
