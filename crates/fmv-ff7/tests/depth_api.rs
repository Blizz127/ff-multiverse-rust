//! Compile-time pin of the per-tile depth API a depth-tested renderer consumes (see
//! `fmv_ff7::background::depth`). If one of these signatures or public fields changes, this file
//! stops compiling instead of the client.
//!
//! No game data is involved; nothing here runs a decoder except on a tiny hand-made value.

use fmv_ff7::background::depth::{
    decode_background_tiles_with_depth_options, depth_tiles, pack_atlas, AtlasRect, TileAtlas, ENGINE_FAR_PLANE, ENGINE_NEAR_PLANE,
    LAYER1_DEPTH, LAYER3_DEPTH, LAYER4_DEPTH, MAX_ATLAS_HEIGHT, MAX_ATLAS_WIDTH, MAX_DEPTH_TILE_BYTES,
};
use fmv_ff7::background::{
    decode_background_tiles_with_depth, depth_of_id, depth_of_point, tile_depth, tile_hides_point, BackgroundOptions, BlendMode,
    DepthTile, Projection, ScreenRect, TILE_DEPTH_PER_ID,
};
use fmv_ff7::field::{Background, Camera, FieldError, FieldFile, Palette};

#[test]
fn depth_signatures_are_stable() {
    let _: fn(&FieldFile) -> Result<Vec<DepthTile>, FieldError> = decode_background_tiles_with_depth;
    let _: fn(&FieldFile, &BackgroundOptions) -> Result<Vec<DepthTile>, FieldError> = decode_background_tiles_with_depth_options;
    let _: for<'a> fn(&Background<'a>, &Palette, &BackgroundOptions) -> Result<Vec<DepthTile>, FieldError> = depth_tiles;
    let _: fn(&Projection, [f32; 3]) -> Option<f32> = depth_of_point;
    let _: fn(u16) -> f32 = depth_of_id;
    let _: fn(u8, u16) -> f32 = tile_depth;
    let _: fn(f32, f32) -> bool = tile_hides_point;
    let _: fn(&[DepthTile], u32) -> Option<TileAtlas> = pack_atlas;
    let _: fn(&DepthTile) -> u32 = DepthTile::size;
    let _: fn(&DepthTile, u32, u32) -> Option<[u8; 4]> = DepthTile::pixel;
    let _: fn(&DepthTile, f32) -> bool = DepthTile::hides_depth;
}

#[test]
fn depth_tile_fields_and_constants_are_stable() {
    // the fields a renderer reads
    let read = |t: &DepthTile| {
        let rect: ScreenRect = t.rect;
        let blend: Option<BlendMode> = t.blend;
        (t.layer, t.index, t.id, rect.min_x, rect.min_y, rect.max_x, rect.max_y, t.depth, blend, t.param, t.state, t.rgba.len())
    };
    let _ = read;
    let read_atlas = |a: &TileAtlas| -> (u32, u32, usize, Vec<AtlasRect>) { (a.width, a.height, a.rgba.len(), a.rects.clone()) };
    let _ = read_atlas;
    let read_rect = |r: AtlasRect| (r.x, r.y, r.size);
    let _ = read_rect;
    // the rule and the fixed depths
    assert_eq!(TILE_DEPTH_PER_ID, 4.0);
    assert_eq!(depth_of_id(152), 608.0);
    assert_eq!(tile_depth(2, 544), 2176.0);
    assert_eq!((LAYER1_DEPTH, LAYER3_DEPTH, LAYER4_DEPTH), (16_380.0, 16_384.0, 0.0));
    assert_eq!((ENGINE_NEAR_PLANE, ENGINE_FAR_PLANE), (50.0, 32_000.0));
    assert_eq!((MAX_ATLAS_WIDTH, MAX_ATLAS_HEIGHT, MAX_DEPTH_TILE_BYTES), (8192, 8192, 64 * 1024 * 1024));
    // hiding is a strict comparison of the two depths
    assert!(tile_hides_point(608.0, 609.0) && !tile_hides_point(608.0, 608.0) && !tile_hides_point(608.0, 607.0));
    // a point's depth is the projection's
    let camera = Camera { axes: [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]], position: [0, 0, 1000], zoom: 500 };
    let projection = Projection::from_camera(&camera);
    assert_eq!(depth_of_point(&projection, [0.0, 1000.0, 0.0]), projection.project([0.0, 1000.0, 0.0]).map(|p| p.depth));
}
